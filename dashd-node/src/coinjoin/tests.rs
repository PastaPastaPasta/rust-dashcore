use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_chain::{BlockRef, SporkView};
use dash_clock::{Clock, MockClock};
use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{DstxChain, DstxScore, DstxValidation, PoolLimits, txid};
use dash_coinjoin::inouts::CoinView;
use dash_coinjoin::queue::{MixingCounters, TipMasternodes};
use dash_consensus::deployments::VERSIONBITS_TOP_BITS;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, Tx};
use dash_consensus::{Block, BlockId, BlsCtx};
use dash_dml::testkit::{dmn, dmn_with};
use dash_dml::{MnList, MnType};
use dash_num::CompactTarget;
use dash_p2p::conn::Permissions;
use dash_p2p::{CommandString, NetAddr, OutMsg, Service};
use dash_p2p_core::{DsQueue, DsTx, Inv, InvType, Inventory};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Hashable;
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::{
    MemBlockFiles, NoFlush, NoWaitGuard, NullConnector, RecordingAbort, genesis_block,
};
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{ChainView, DstxChainAdapter, Mempool, MnLists, PeerSink};
use dashd_coinjoin::server::{RandomSource, ServerClock};
use dashd_coinjoin::testing::FakeMnLists;
use dashd_masternode::active::state::ListError;
use dashd_masternode::active::worker::ListForBlock;
use dashd_masternode::sync::{MasternodeSync, NodeSyncNotifier};
use dashd_masternode::tip::{TipListRef, TipMnList};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

use super::adapters::*;
use super::mempool::GatedMempool;
use crate::chain::{Chain, tests as chain_tests};

fn block(n: u8) -> BlockHash {
    BlockHash::from([n; 32])
}

#[test]
fn has_chain_lock_takes_cores_three_branches() {
    let floor = BlockRef {
        id: BlockId(10),
        hash: block(10),
        height: 10,
    };
    let ancestors = BTreeMap::from([(5, block(5))]);
    let lookup = |id: BlockId, height: u32| {
        assert_eq!(id, floor.id);
        ancestors.get(&height).copied()
    };
    let check =
        |height: i32, hash: u8| has_chain_lock(true, Some(floor), height, &block(hash), lookup);
    // Above the best ChainLock.
    assert!(!check(11, 10));
    assert!(!check(11, 11));
    // At its height: the hash alone, with no ancestor read.
    assert!(check(10, 10));
    assert!(!check(10, 5));
    // Below it: the floor's ancestor.
    assert!(check(5, 5));
    assert!(!check(5, 10));
    assert!(!check(4, 4));
    assert!(!check(-1, 5));
    assert!(!has_chain_lock(false, Some(floor), 10, &block(10), lookup));
    assert!(!has_chain_lock(true, None, 10, &block(10), lookup));
}

struct Sporks(bool);

impl SporkView for Sporks {
    fn chainlocks_enabled(&self) -> bool {
        self.0
    }
}

struct Silent;

impl NodeSyncNotifier for Silent {
    fn sync_reset(&self) {}
    fn sync_finished(&self) {}
}

const REGTEST_BITS: u32 = 0x207f_ffff;
/// Regtest's V24 bit 13 (`chainparams.cpp:856-863` at v24.0.0).
const V24_VERSION: i32 = VERSIONBITS_TOP_BITS | (1 << 13);

struct Fixture {
    chain: Chain,
    abort: Arc<RecordingAbort>,
    clock: Arc<dyn Clock>,
    params: Arc<ChainSetup>,
    blocks: Vec<Arc<Block>>,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture::on(chain_tests::regtest())
    }

    /// `-vbparams=v24:0:…:0:10:8:0:0:0`: 10-block periods, 8 of 10, no EHF. With every block
    /// signalling, V24 is `STARTED` from 10, `LOCKED_IN` from 20 and `ACTIVE` from 30.
    fn short_v24() -> Fixture {
        Fixture::on(Arc::new(
            ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
                .arg(ChainOption::VbParams, format!("v24:0:{}:0:10:8:0:0:0", i64::MAX))
                .build()
                .expect("regtest with -vbparams"),
        ))
    }

    fn on(params: Arc<ChainSetup>) -> Fixture {
        let genesis = genesis_block(&params);
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().time()) + 60));
        let abort = Arc::new(RecordingAbort::default());
        let files = Box::new(MemBlockFiles::default());
        let mut chain = Chain::start(chain_tests::fresh(&params, &clock, &abort, files)).unwrap();
        chain_tests::startup_over(&mut chain, Box::new(NullConnector), Box::new(NoFlush));
        chain_tests::imported(&mut chain);
        Fixture {
            chain,
            abort,
            clock,
            params,
            blocks: vec![genesis],
        }
    }

    fn view(&self) -> NodeChainView {
        let sync = Arc::new(MasternodeSync::new(Arc::clone(&self.clock), Box::new(Silent)));
        NodeChainView::new(self.chain.handle().view_handle(), sync, Arc::new(Sporks(true)))
    }

    fn genesis(&self) -> BlockHash {
        self.blocks[0].hash()
    }

    /// Connects one block of `version` on the tip, mined under regtest's limit.
    fn mine(&mut self, version: i32) -> u32 {
        let height = u32::try_from(self.blocks.len()).unwrap();
        let block = self.process(self.blocks.last().unwrap(), height, version, 150);
        self.blocks.push(block);
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.chain.handle().view().snap.tip.tip_height != height {
            assert!(Instant::now() < deadline, "block {height} is the published tip");
            std::thread::sleep(Duration::from_millis(5));
        }
        height
    }

    /// Stores a block at `height` on `prev`, 151 s after it so it differs from the tip branch's.
    fn fork(&self, prev: &Block, height: u32) -> Arc<Block> {
        self.process(prev, height, 0x2000_0000, 151)
    }

    fn process(&self, prev: &Block, height: u32, version: i32, spacing: u32) -> Arc<Block> {
        let prev = prev.header();
        let txs = vec![coinbase(height)];
        let base = BlockHeader::default();
        let mut raw = BlockHeader::new(
            version,
            prev.hash(),
            base.merkle_root(),
            prev.raw().time() + spacing,
            REGTEST_BITS,
            0,
        );
        raw.set_merkle_root(Block::new(Header::new(raw), txs.clone()).merkle().0);
        let pow = *HeaderParams::from_chain_params(&self.params).unwrap().pow();
        let header = (0..)
            .map(|nonce| {
                Header::new(BlockHeader::new(
                    raw.version(),
                    raw.prev_hash(),
                    raw.merkle_root(),
                    raw.time(),
                    raw.bits(),
                    nonce,
                ))
            })
            .find(|h| {
                check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow)
            })
            .unwrap();
        let block = Arc::new(Block::new(header, txs));
        let result = self.chain.handle().process_block(Arc::clone(&block), true, None, true);
        assert!(result.recv().is_ok_and(|r| r.accepted), "block {height} accepted");
        block
    }

    fn stop(mut self) {
        assert!(self.chain.stop().is_some());
        assert!(self.abort.messages().is_empty());
    }
}

/// Core `CScript() << height << OP_0` (heights 1–127), paying one `OP_TRUE` output nothing.
fn coinbase(height: u32) -> Arc<Tx> {
    let h = u8::try_from(height).unwrap();
    let mut push = if h <= 16 {
        vec![0x50 + h]
    } else {
        vec![1, h]
    };
    push.push(0);
    let tx = Transaction::new(
        1,
        TxType::Spend,
        vec![TxIn::new(OutPoint::new(TxHash::default(), u32::MAX), push.into(), u32::MAX)],
        vec![TxOut::from_duffs(0, vec![0x51].into())],
        0,
        Vec::new(),
    );
    Arc::new(Tx::new(tx))
}

#[test]
fn node_chain_view_reads_published_state_and_issues_no_v_query_under_no_wait() {
    let fx = Fixture::new();
    let sync = Arc::new(MasternodeSync::new(Arc::clone(&fx.clock), Box::new(Silent)));
    let view = NodeChainView::new(
        fx.chain.handle().view_handle(),
        Arc::clone(&sync),
        Arc::new(Sporks(true)),
    );
    {
        // A V query on a marked thread is refused at once (LO-9), so each read below would fail.
        let _marked = NoWaitGuard::set();
        assert_eq!(view.tip(), Some((0, fx.genesis())));
        assert!(!view.has_chain_lock(0, &fx.genesis()));
        assert!(!view.blockchain_synced());
        sync.switch_to_next_asset();
        assert!(view.blockchain_synced());
    }
    fx.stop();
}

/// A ChainLock floor on a branch other than the tip's: the hashes below it are its ancestors'.
// Core: chainlock/chainlock.cpp:62-71
#[test]
fn the_chain_lock_lookup_walks_the_floors_branch_from_the_block_index() {
    let mut fx = Fixture::new();
    for _ in 1..=3 {
        fx.mine(0x2000_0000);
    }
    let one = fx.fork(&fx.blocks[0], 1);
    let two = fx.fork(&one, 2);
    let h = fx.chain.handle().view_handle();
    assert_eq!(h.view().snap.tip.tip_height, 3);
    let floor = |block: &Block, height| BlockRef {
        id: chain_tests::id(fx.chain.handle(), block),
        hash: block.hash(),
        height,
    };
    let check = |floor, hash: BlockHash| {
        has_chain_lock(true, Some(floor), 1, &hash, floor_ancestor_hash(&h))
    };
    let (side, main) = (floor(&two, 2), floor(&fx.blocks[3], 3));
    assert!(check(side, one.hash()));
    assert!(!check(side, fx.blocks[1].hash()));
    assert!(!check(side, two.hash()));
    assert!(check(main, fx.blocks[1].hash()));
    assert!(!check(main, one.hash()));
    fx.stop();
}

#[test]
#[should_panic(expected = "a V24 read on a no-wait thread")]
fn the_at_tip_v24_read_under_a_no_wait_guard_asserts() {
    let fx = Fixture::new();
    let view = fx.view();
    let _marked = NoWaitGuard::set();
    view.v24_active_at_tip();
}

#[test]
#[should_panic(expected = "a V24 read on a no-wait thread")]
fn the_after_tip_v24_read_under_a_no_wait_guard_asserts() {
    let fx = Fixture::new();
    let view = fx.view();
    let _marked = NoWaitGuard::set();
    view.v24_active_after_tip();
}

/// 19 inputs to 10 outputs: unbalanced, so valid only under the post-V24 rules.
fn unbalanced_dstx() -> DsTx {
    let inputs = (0..19)
        .map(|i| TxIn::new(OutPoint::new(TxHash::from([1; 32]), i), Vec::new().into(), u32::MAX))
        .collect();
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend([1; 20]);
    script.extend([0x88, 0xac]);
    let outputs =
        (0..10).map(|_| TxOut::from_duffs(SMALLEST_DENOMINATION, script.clone().into())).collect();
    let tx = Transaction::new(2, TxType::Spend, inputs, outputs, 0, Vec::new());
    DsTx::new(tx, TxHash::from([1; 32]), Vec::new(), 1_760_000_000)
}

fn dstx_score(view: &NodeChainView) -> DstxValidation {
    let limits = PoolLimits {
        min_participants: 3,
        max_participants: 20,
    };
    let dstx = unbalanced_dstx();
    let chain = DstxChainAdapter {
        chain: view,
        mns: &FakeMnLists::default(),
    };
    let mut meta = MixingCounters::default();
    DstxRuntime::new(limits).check(&dstx, &txid(dstx.tx()), &mut meta, &chain)
}

/// Core's V24 reads at a versionbits period's last block, before and at activation, and the
/// DSTX score they decide there: an unbalanced DSTX is `INVALID` (+10) one block further from
/// V24, `PREMATURE` (+1) on its last `LOCKED_IN` block, and structurally valid once it is active.
// Core: coinjoin/coinjoin.cpp:144, :255-262; net_processing.cpp:3660-3671
#[test]
fn v24_reads_and_the_premature_score_cross_activation_at_a_periods_last_block() {
    let mut fx = Fixture::short_v24();
    let view = fx.view();
    assert_eq!((view.v24_active_at_tip(), view.v24_active_after_tip()), (false, false));
    for _ in 1..=28 {
        fx.mine(V24_VERSION);
    }
    let at = |view: &NodeChainView| (view.v24_active_at_tip(), view.v24_active_after_tip());
    assert_eq!(at(&view), (false, false), "tip 28");
    assert_eq!(dstx_score(&view), DstxValidation::Stop(DstxScore::Invalid));
    assert_eq!(fx.mine(V24_VERSION), 29);
    assert_eq!(at(&view), (false, true), "tip 29, the last LOCKED_IN block");
    assert_eq!(dstx_score(&view), DstxValidation::Stop(DstxScore::Premature));
    assert_eq!(fx.mine(V24_VERSION), 30);
    assert_eq!(at(&view), (true, true), "tip 30, ACTIVE");
    assert_eq!(dstx_score(&view), DstxValidation::Stop(DstxScore::UnknownMasternode));
    fx.stop();
}

struct Lists {
    by_block: BTreeMap<BlockId, Arc<MnList>>,
    asked: Arc<Mutex<Vec<BlockId>>>,
}

impl ListForBlock for Lists {
    fn list_for_block(&self, id: BlockId) -> Result<Arc<MnList>, ListError> {
        self.asked.lock().unwrap().push(id);
        Ok(Arc::clone(&self.by_block[&id]))
    }
}

fn pro(n: u8) -> TxHash {
    TxHash::from(*dmn(n, MnType::Regular).pro_tx_hash.as_bytes())
}

fn tip_list() -> MnList {
    let mut list = MnList::new(BlockHash::from([1; 32]), 1, 0);
    list.insert_for_tests(dmn(1, MnType::Regular));
    list.insert_for_tests(dmn_with(2, MnType::Regular, |d, _| d.hot.ban_if_not_banned(1)));
    list
}

type Asked = Arc<Mutex<Vec<BlockId>>>;

fn lists(fx: &Fixture, by_block: BTreeMap<BlockId, Arc<MnList>>) -> (NodeMnLists<Lists>, Asked) {
    let tip = TipMnList::fixed(TipListRef {
        list: Arc::new(tip_list()),
        bls: BlsCtx::BASIC,
    });
    let asked = Asked::default();
    let lists = Lists {
        by_block,
        asked: Arc::clone(&asked),
    };
    (NodeMnLists::new(tip, fx.chain.handle().view_handle(), lists), asked)
}

// Core: evo/deterministicmns.cpp:941-947, coinjoin/server.cpp:82-83,101,149-151
#[test]
fn node_mn_lists_read_the_list_at_the_chain_tip() {
    let fx = Fixture::new();
    let (mns, _) = lists(&fx, BTreeMap::new());
    let one = dmn(1, MnType::Regular);
    let valid = mns.valid_mn(&pro(1)).unwrap();
    assert_eq!(valid.pro_tx_hash, pro(1));
    assert_eq!(valid.collateral, one.collateral);
    assert_eq!(valid.operator_key, one.cold.pub_key_operator.get());
    assert!(mns.valid_mn(&pro(2)).is_none());
    assert!(mns.valid_mn(&pro(3)).is_none());
    assert_eq!(mns.enabled_count(), 1);
    assert_eq!(mns.valid_by_collateral(&one.collateral).unwrap().pro_tx_hash, pro(1));
    assert!(mns.valid_by_collateral(&dmn(2, MnType::Regular).collateral).is_none());
    fx.stop();
}

// Core: net_processing.cpp:3675-3682; evo/deterministicmns.h:796-799
#[test]
fn mn_at_depth_reads_the_ancestor_list_including_invalid_masternodes() {
    let mut fx = Fixture::new();
    fx.mine(0x2000_0000);
    let id = |fx: &Fixture, n: usize| chain_tests::id(fx.chain.handle(), &fx.blocks[n]);
    let (genesis, one) = (id(&fx, 0), id(&fx, 1));
    let by_block = BTreeMap::from([
        (genesis, Arc::new(tip_list())),
        (one, Arc::new(MnList::new(BlockHash::from([2; 32]), 2, 0))),
    ]);
    let (mns, asked) = lists(&fx, by_block);
    assert!(mns.mn_at_depth(0, &pro(2)).is_none());
    assert_eq!(mns.mn_at_depth(1, &pro(2)).unwrap().pro_tx_hash, pro(2));
    assert!(mns.mn_at_depth(1, &pro(3)).is_none());
    assert!(mns.mn_at_depth(2, &pro(1)).is_none());
    assert_eq!(*asked.lock().unwrap(), [one, genesis, genesis]);
    fx.stop();
}

#[test]
#[should_panic(expected = "a masternode list read on a no-wait thread")]
fn mn_at_depth_under_a_no_wait_guard_asserts() {
    let fx = Fixture::new();
    let genesis = fx.chain.handle().view().snap.tip.tip;
    let (mns, _) = lists(&fx, BTreeMap::from([(genesis, Arc::new(tip_list()))]));
    let _marked = NoWaitGuard::set();
    let _ = mns.mn_at_depth(0, &pro(1));
}

/// Before any tip or ancestor check, on either path.
#[test]
#[should_panic(expected = "a masternode list read on a no-wait thread")]
fn a_masternode_list_read_with_no_tip_under_a_no_wait_guard_asserts() {
    let fx = Fixture::new();
    let (mns, _) = lists(&fx, BTreeMap::new());
    let _marked = NoWaitGuard::set();
    let _ = mns.mn_at(None, 0, &pro(1));
}

#[test]
#[should_panic(expected = "a masternode list read on a no-wait thread")]
fn a_pinned_walk_below_genesis_under_a_no_wait_guard_asserts() {
    let fx = Fixture::new();
    let (mns, _) = lists(&fx, BTreeMap::new());
    let view = fx.view();
    let chain = NodeDstxChain::pin(&view, &mns);
    let _marked = NoWaitGuard::set();
    let _ = chain.mn_at_depth(1, &pro(1));
}

/// Publishes one more block from inside its first lookup, as V may mid-walk.
struct Publishing {
    fx: Arc<Mutex<Fixture>>,
    mined: AtomicBool,
    asked: Asked,
}

impl ListForBlock for Publishing {
    fn list_for_block(&self, id: BlockId) -> Result<Arc<MnList>, ListError> {
        self.asked.lock().unwrap().push(id);
        if !self.mined.swap(true, Ordering::SeqCst) {
            self.fx.lock().unwrap().mine(0x2000_0000);
        }
        Ok(Arc::new(MnList::new(BlockHash::from([2; 32]), 2, 0)))
    }
}

// Core: net_processing.cpp:3652-3656, :3673-3681
#[test]
fn the_dstx_walk_reads_the_24_ancestors_of_the_tip_it_pinned() {
    let mut fx = Fixture::new();
    for _ in 1..=24 {
        fx.mine(0x2000_0000);
    }
    let handle = fx.chain.handle().view_handle();
    let view = fx.view();
    let ancestors: Vec<_> =
        (1..=24).rev().map(|n| chain_tests::id(fx.chain.handle(), &fx.blocks[n])).collect();
    let fx = Arc::new(Mutex::new(fx));
    let asked = Asked::default();
    let tip = TipMnList::fixed(TipListRef {
        list: Arc::new(tip_list()),
        bls: BlsCtx::BASIC,
    });
    let source = Publishing {
        fx: Arc::clone(&fx),
        mined: AtomicBool::new(false),
        asked: Arc::clone(&asked),
    };
    let mns = NodeMnLists::new(tip, handle, source);
    let chain = NodeDstxChain::pin(&view, &mns);
    assert_eq!(chain.tip_height(), Some(24));
    assert!((0..24).all(|depth| chain.mn_at_depth(depth, &pro(1)).is_none()));
    assert_eq!(fx.lock().unwrap().chain.handle().view().snap.tip.tip_height, 25);
    assert_eq!(chain.tip_height(), Some(24));
    assert_eq!(*asked.lock().unwrap(), ancestors);
    drop(mns);
    Arc::into_inner(fx).unwrap().into_inner().unwrap().stop();
}

fn sim(pool: Option<&SimTxPool>) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        txpool: pool.map_or_else(|| Seams::default().txpool, |p| Arc::new(p.clone())),
        ..Seams::default()
    };
    let sim =
        Sim::with(params, identity, ProcessorOptions::default(), Registry::new(), seams, |_| {});
    sim.chain.set_ibd(false);
    sim
}

fn peer(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.accept_from(n, NetAddr::ipv4([10, 0, 0, n]), Permissions::NO_BAN).unwrap();
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

fn service(n: u8) -> Service {
    Service {
        addr: NetAddr::ipv4([10, 0, 0, n]),
        port: 40000 + u16::from(n),
    }
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn ping(nonce: u64) -> OutMsg {
    OutMsg {
        command: CommandString::PING,
        payload: nonce.to_le_bytes().to_vec(),
    }
}

#[test]
fn node_peer_sink_pushes_by_id_and_address_and_scores() {
    let mut sim = sim(None);
    let p = peer(&mut sim, 1);
    let mut sink = NodePeerSink::new(Arc::clone(&sim.shared));
    assert!(sink.is_connected(&service(1)));
    assert!(!sink.is_connected(&service(2)));
    sink.push_to(p.handle.id(), ping(7));
    assert!(sink.push_to_addr(&service(1), ping(8)));
    assert!(!sink.push_to_addr(&service(2), ping(9)));
    // A peer before its handshake is not fully connected (net.h:1369-1373).
    let half = sim.accept_from(3, NetAddr::ipv4([10, 0, 0, 3]), Permissions::NO_BAN).unwrap();
    assert!(!sink.is_connected(&service(3)));
    assert!(!sink.push_to_addr(&service(3), ping(10)));
    assert!(sim.sent(&half).is_empty());
    assert_eq!(
        sim.sent_at_least(&p, 2),
        [
            (CommandString::PING, 7u64.to_le_bytes().to_vec()),
            (CommandString::PING, 8u64.to_le_bytes().to_vec()),
        ]
    );
    sink.misbehaving(p.handle.id(), 10, "test");
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 10);
}

fn tx_inv(tag: u8) -> Inventory {
    Inventory::new(InvType::Tx, TxHash::from([tag; 32]).into())
}

// Core: net_processing.cpp:6930-6935, 6974-6977
#[test]
fn node_peer_sink_erases_one_peers_request_and_forgets_every_peers() {
    let pool = SimTxPool::default();
    let mut sim = sim(Some(&pool));
    let (a, b) = (peer(&mut sim, 1), peer(&mut sim, 2));
    for p in [&a, &b] {
        sim.admitted(p, "inv", encode(&Inv::new(vec![tx_inv(1)]))).unwrap();
        sim.pass();
    }
    let requests = sim.lp.logic().requested_objects();
    assert_eq!((requests.count(a.handle.id()), requests.count(b.handle.id())), (1, 1));
    let mut sink = NodePeerSink::new(Arc::clone(&sim.shared));
    sink.erase_object_request(b.handle.id(), tx_inv(1));
    assert_eq!((requests.count(a.handle.id()), requests.count(b.handle.id())), (1, 1));
    sink.erase_object_request(a.handle.id(), tx_inv(1));
    assert_eq!((requests.count(a.handle.id()), requests.count(b.handle.id())), (0, 0));
    for p in [&a, &b] {
        sim.admitted(p, "inv", encode(&Inv::new(vec![tx_inv(2)]))).unwrap();
        sim.pass();
    }
    sink.forget_object_request(tx_inv(2));
    assert_eq!((requests.count(a.handle.id()), requests.count(b.handle.id())), (0, 0));
}

fn invs(sim: &Sim, p: &SimPeer) -> Vec<Inventory> {
    sim.sent(p)
        .into_iter()
        .filter(|(cmd, _)| *cmd == CommandString::INV)
        .flat_map(|(_, bytes)| Inv::decode(&mut &bytes[..]).unwrap().inventory().clone())
        .collect()
}

#[test]
fn node_peer_sink_relays_queues_after_the_server_call_returns() {
    let mut sim = sim(None);
    let p = peer(&mut sim, 1);
    sim.admitted(&p, "senddsq", vec![1]).unwrap();
    sim.pass();
    let mut sink = NodePeerSink::new(Arc::clone(&sim.shared));
    let queue = |n: u8| DsQueue::new(16, TxHash::from([n; 32]), SIM_START, true, vec![n; 96]);
    let inv = |n: u8| Inventory::new(InvType::Dsq, queue(n).hash());
    NodePeerSink::outside_server(|| {
        sink.relay_dsq(&queue(1));
        NodePeerSink::outside_server(|| sink.relay_dsq(&queue(2)));
        sim.pass();
        assert!(invs(&sim, &p).is_empty());
    });
    sim.pass();
    assert_eq!(invs(&sim, &p), [inv(1), inv(2)]);
    sink.relay_dsq(&queue(3));
    sink.relay_inv(inv(4));
    sim.pass();
    assert_eq!(invs(&sim, &p), [inv(3), inv(4)]);
}

#[test]
fn node_server_clock_is_mocktime_aware() {
    let sim = sim(None);
    let clock = NodeServerClock::new(Arc::clone(&sim.core));
    sim.clock.set_mock_time(SIM_START + 77);
    assert_eq!(clock.now(), SIM_START + 77);
    assert_eq!(clock.adjusted_time(), SIM_START + 77 + sim.core.time_offset());
}

#[test]
fn node_random_draws_below_the_bound_from_fresh_contexts() {
    let mut random = NodeRandom;
    assert!((0..1000).all(|_| random.below(100) < 100));
    assert_eq!(random.below(1), 0);
    let draws: Vec<_> = (0..4).map(|_| random.below(u32::MAX)).collect();
    assert!(draws.windows(2).any(|w| w[0] != w[1]), "{draws:?}");
}

// D17: the server's admission is refused until W6; InstantSend is off until LK1b.
#[test]
fn gated_mempool_refuses_admission_and_sees_no_lock() {
    let tx = Transaction::new(2, TxType::Spend, Vec::new(), Vec::new(), 0, Vec::new());
    let mut mempool = GatedMempool;
    assert_eq!(mempool.test_accept(&tx), None);
    assert!(!mempool.submit(&tx));
    mempool.prioritise(&TxHash::from([1; 32]), 10);
    assert!(mempool.coin(&OutPoint::new(TxHash::from([1; 32]), 0)).is_none());
    assert!(!mempool.is_instantsend_locked(&TxHash::from([1; 32])));
}

/// A non-masternode network attached over `fx`'s V with the masternode services, not started:
/// its listeners are on V's signals.
fn attached(
    fx: &Fixture,
    dir: &std::path::Path,
) -> (crate::p2p::Net, crate::mnsync::SyncParts, Arc<DstxRuntime>) {
    use dashd_masternode::meta::MasternodeMetaMan;
    use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
    let args = crate::init::test_args(&[]);
    let role = crate::p2p::MasternodeRole::from_args(&args);
    let setup = crate::init::checks::NetSetup {
        basic_filter_index: false,
        v2_transport: false,
        compact_filters: false,
        allow_private_net: false,
        user_max_connections: 125,
        max_connections: 125,
        fd: 1024,
    };
    let clock = Arc::clone(&fx.clock);
    let mut net =
        crate::p2p::Net::new(&args, &fx.params, &setup, role, Arc::clone(&clock), dir).unwrap();
    net.check_options(&args, &crate::logging::Logger::new()).unwrap();
    let fulfilled = Arc::new(NetFulfilledRequestManager::new(Arc::clone(&clock), 300));
    let core = Arc::clone(net.core());
    let sync = crate::mnsync::SyncParts::new(
        &args,
        &fx.params,
        core,
        Arc::clone(&clock),
        fulfilled,
        false,
    )
    .unwrap();
    let sporks = crate::sporks::spork_manager(&args, &fx.params, Arc::clone(&clock)).unwrap();
    let meta = Arc::new(MasternodeMetaMan::new());
    let cj =
        crate::coinjoin::role::CoinJoin::new(PoolLimits::of(&fx.params), Arc::clone(&meta), None);
    let dstx = Arc::clone(cj.dstx());
    let tip = crate::p2p::tip_mn_list::new(fx.chain.handle().view_handle());
    net.attach(&fx.chain, None, clock, &sporks, &sync, tip, None, &meta, None, cj, None).unwrap();
    (net, sync, dstx)
}

/// `events` on V's signals, then a barrier: every listener has had them.
fn signal(fx: &Fixture, events: Vec<dashd_chainstate::chain::Queued>) {
    let signals = fx.chain.signals();
    for event in events {
        signals.enqueue(event);
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    signals.enqueue(dashd_chainstate::chain::Queued::Barrier(tx));
    rx.recv_timeout(Duration::from_secs(60)).expect("the signals thread reaches the barrier");
}

/// The DSTX listener runs right after `mn_sync`'s on V's signals. With the sync at the best
/// header, a tip below it resets the sync first, so the DSTX expiry sees an unsynced chain and
/// keeps a DSTX mined 25 blocks below; a ChainLock on a synced chain then expires it. The listener
/// forwards connected and disconnected blocks, and shutdown takes it off V's signals.
// Core: init.cpp:2191-2194; dsnotificationinterface.cpp:55-104; masternode/sync.cpp:121-158;
// coinjoin/coinjoin.cpp:584-650
#[test]
fn the_dstx_listener_follows_the_sync_listener_and_leaves_at_shutdown() {
    use dashd_chainstate::chain::{ChainLockSig, Queued};
    let mut fx = Fixture::new();
    for _ in 0..27 {
        fx.mine(0x2000_0000);
    }
    let view = fx.chain.handle().view_handle();
    let tip = view.view().snap.tip.tip;
    let id = |height| view.ancestor(tip, height).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mut net, sync, dstx) = attached(&fx, dir.path());
    let parts = net.parts();
    assert!(parts.sync_registered && parts.dstx_registered, "{parts:?}");
    let updated = |height| Queued::UpdatedBlockTip {
        new: id(height),
        fork: Some(id(height - 1)),
        ibd: false,
    };
    let connected = |block: &Arc<Block>| Queued::BlockConnected {
        block: Arc::clone(block),
        id: id(1),
    };

    sync.sync.switch_to_next_asset();
    signal(&fx, vec![updated(27)]);
    assert!(sync.sync.is_blockchain_synced() && sync.sync.is_reached_best_header());

    let tx = fx.blocks[1].txs()[0].raw().clone();
    let hash = txid(&tx);
    let mixed = DsTx::new(tx, TxHash::from([9; 32]), vec![0; 96], 0);
    dstx.on_accepted(mixed.clone());
    let confirmed = || dstx.get(&hash).and_then(|stored| stored.confirmed_height);
    signal(&fx, vec![connected(&fx.blocks[1])]);
    assert_eq!(confirmed(), Some(1));
    let disconnected = Queued::BlockDisconnected {
        block: Arc::clone(&fx.blocks[1]),
        id: id(1),
    };
    signal(&fx, vec![disconnected, connected(&fx.blocks[1])]);
    assert_eq!(confirmed(), Some(1));

    signal(&fx, vec![updated(26)]);
    assert!(!sync.sync.is_blockchain_synced(), "the tip below the best header reset the sync");
    assert!(dstx.contains(&hash), "the DSTX check ran after the sync's reset");

    sync.sync.switch_to_next_asset();
    let sig = Arc::new(ChainLockSig::null());
    signal(
        &fx,
        vec![Queued::NotifyChainLock {
            id: id(26),
            sig,
        }],
    );
    assert!(!dstx.contains(&hash), "expired 25 blocks after it was mined");

    net.stop();
    assert!(!net.parts().dstx_registered);
    dstx.on_accepted(mixed);
    signal(&fx, vec![connected(&fx.blocks[1])]);
    assert_eq!(confirmed(), None, "no listener after shutdown");
    drop(net);
    fx.stop();
}

/// Step 10a puts the role's ticks on the scheduler every second, after the two minute tasks, in
/// [`Ticks::around`](super::role::Ticks::around)'s order: the server's first, the wallet's last.
// Core: init.cpp:2363-2366, :2372-2373
#[test]
fn step_10a_schedules_the_coinjoin_ticks_around_sync_manager() {
    let fx = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let (mut net, sync, _) = attached(&fx, dir.path());
    let ran = Arc::new(Mutex::new(Vec::new()));
    let record = |name: &'static str| -> super::role::Tick {
        let ran = Arc::clone(&ran);
        Arc::new(move |down| ran.lock().unwrap().push((name, down)))
    };
    let ticks = super::role::Ticks {
        server: Some(record("server")),
        wallet: Some(record("wallet")),
    };
    let (scheduler, shutdown) =
        (crate::scheduler::Scheduler::new(), Arc::new(crate::shutdown::ShutdownState::new()));
    let maintenance = crate::mnsync::MnMaintenance::new(
        Arc::clone(net.core()),
        Arc::clone(&sync.sync),
        Arc::clone(net.quorum_nodes()),
        Arc::clone(&shutdown),
        Arc::new(crate::logging::Logger::new()),
    );
    sync.schedule(&scheduler, &shutdown, maintenance, &ticks);
    assert_eq!(scheduler.queue_info().0, 5, "two minute tasks and three 1 s tasks");
    // A requested shutdown keeps SyncManager's tick a no-op; each tick is handed the flag.
    shutdown.start_shutdown();
    scheduler.mock_forward(Duration::from_secs(1));
    let stop = scheduler.clone();
    scheduler.schedule_from_now(move || stop.stop(), Duration::from_millis(1));
    scheduler.service_queue();
    assert_eq!(*ran.lock().unwrap(), [("server", true), ("wallet", true)]);
    net.stop();
    drop(net);
    fx.stop();
}
