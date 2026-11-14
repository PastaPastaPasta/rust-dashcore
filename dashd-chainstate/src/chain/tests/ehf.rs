//! DEC-1028: V's versionbits read the connector's MNHF signals (`CMNHFManager::GetForBlock`,
//! [`EhfRows`]). An EHF deployment with no signal stays `DEFINED`; with one, it starts at the
//! first period's end at or past the signal's height (`versionbits.cpp:38`, `:69`, `:93-97`,
//! `:238-251` at v24.0.0), on each branch as its own signals say, across reorgs and with the
//! connector's CbTx rule; [`NodeEvo`]'s rows read a block's stored map without its data, rebuild a
//! missing one from the blocks' data into the node's one MNHF manager, and a stored bit out of
//! range is a store fault, reported once, that V's versionbits answer as `GetForBlock`'s error
//! (BL-274).
//!
//! [`NodeEvo`]: crate::evo::node::NodeEvo

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dash_chain::ChainReader;
use dash_consensus::ctx::{Deployment, DeploymentView as _};
use dash_consensus::deployments::{EhfSignals, ThresholdState, VERSIONBITS_TOP_BITS};
use dash_consensus::primitives::Transaction;
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{Block, BlockId, BlsCtx, Tx};
use dash_dml::mnhf::{GetForBlockError, MnhfManager, MnhfView};
use dash_dml::testkit::operator_sig;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, DeploymentPos, Network, Release};
use dash_primitives::{BlockHash, MnHardFork, QuorumHash, TxType};
use dash_types::codec::BaseCodec as _;
use dashd_store::{Commit, ErrorKind, KvBatch as _};

use super::deployments::FaultingRows;
use super::node_evo::{node_evo, node_evo_over_blocks, node_evo_reporting};
use super::support::{Mined, Rig, RigConfig, SPACING, cb_script, coinbase, mine_version};
use crate::chain::{
    BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome, DisconnectResult, EhfRows, Fatal,
    NullConnector, ProposalChecks, VContext, ViewTarget,
};
use crate::connect::EvoHooks as _;
use crate::evo::codec::{EvoCf, encode_mnhf};
use crate::evo::ev1::Ev1NotBuilt;
use crate::evo::node::{Ancestry, EvoBlocks};
use crate::evo::testkit::cbtx_bytes;
use crate::store::RecordingSink;

use ThresholdState::{Active, Defined, LockedIn, Started};

const V24: DeploymentPos = DeploymentPos::V24;
/// Regtest's V24 bit (`chainparams.cpp:856-863` at v24.0.0).
const V24_BIT: u8 = 13;
/// A version signalling V24, as every block mined here does.
const V24_VERSION: i32 = VERSIONBITS_TOP_BITS | (1 << V24_BIT);
/// Regtest's V24 window (`chainparams.cpp:859` at v24.0.0).
pub(super) const PERIOD: u32 = 250;

/// Regtest with DIP0003 at 2 and V20 at 100, as the recorded runs set them: an EHF deployment
/// reads signals only once V20 is active after the block (`mnhftx.cpp:63`).
fn regtest_v20_at_100() -> Arc<ChainSetup> {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .build()
        .expect("regtest with -testactivationheight");
    Arc::new(p)
}

/// The coinbase of a block at `height`: from DIP0003 (2) a CbTx, as `ContextualCheckBlock`
/// requires; the connectors here judge nothing in it.
fn coinbase_at(height: u32, salt: u32) -> Arc<Tx> {
    let cb = coinbase(&cb_script(height, salt));
    if height < 2 {
        return cb;
    }
    let mut cb = cb.to_mutable();
    cb = dash_primitives::Transaction::new(
        3,
        cb.tx_type(),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        cb.extra_payload().clone(),
    );
    cb = dash_primitives::Transaction::new(
        cb.version(),
        TxType::new(TRANSACTION_COINBASE),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        cb.extra_payload().clone(),
    );
    cb = dash_primitives::Transaction::new(
        cb.version(),
        cb.tx_type(),
        cb.inputs().clone(),
        cb.outputs().clone(),
        cb.lock_time(),
        cbtx_bytes(3, height, 0, 0),
    );
    Arc::new(Tx::new(cb))
}

/// `n` blocks on `parent` that signal V24, `SPACING` seconds apart.
fn branch(p: &ChainSetup, parent: &Mined, n: u32, salt: u32) -> Vec<Mined> {
    let mut out: Vec<Mined> = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let prev = out.last().unwrap_or(parent);
        let height = prev.height + 1;
        let cb = coinbase_at(height, salt);
        let block = mine_version(p, prev.hash(), prev.time() + SPACING, vec![cb], V24_VERSION);
        out.push(Mined {
            block,
            height,
        });
    }
    out
}

/// A rig whose clock lets `n` blocks of [`branch`] in.
fn rig(p: &Arc<ChainSetup>, n: u32, connector: Box<dyn BlockConnector>) -> Rig {
    Rig::with(RigConfig {
        params: Some(p.clone()),
        after_genesis: Some(i64::from(SPACING * (n + 10))),
        no_check_block_index: true,
        connector: Some(connector),
        ..RigConfig::default()
    })
}

/// Core's map at a block (`GetForBlock`) on chains whose `mined` blocks each mined a V24 signal:
/// the signal of each such block at or below it, at that block's height.
struct Signals {
    mined: HashMap<BlockHash, i32>,
}

impl Signals {
    fn at(blocks: &[&Mined]) -> Arc<Signals> {
        let mined = blocks.iter().map(|m| (m.hash(), i32::try_from(m.height).unwrap())).collect();
        Arc::new(Signals {
            mined,
        })
    }
}

impl EhfRows for Signals {
    fn get_for_block(
        &self,
        reader: &ChainReader,
        block: BlockId,
    ) -> Result<EhfSignals, GetForBlockError> {
        let mut signals = EhfSignals::new();
        let walk =
            std::iter::successors(reader.entry(block), |e| e.prev.and_then(|p| reader.entry(p)));
        for e in walk {
            if let Some(&height) = self.mined.get(&e.hash) {
                signals.insert(V24_BIT, height);
            }
        }
        Ok(signals)
    }
}

/// What the connector's context answered for one connected block: its hash and
/// `SpecialTxRules::v24`, V24's `DeploymentActiveAfter` of its parent (`validation.cpp:6817-6824`
/// at v24.0.0), with which `CheckCbTx` requires a version 4 CbTx (`evo/cbtx.cpp:45-51`).
type Answer = (BlockHash, bool);

/// [`NullConnector`] whose rows are `rows`, asking its context about V24 at each connect.
struct Signalled {
    rows: Arc<dyn EhfRows>,
    asks: Arc<Mutex<Vec<Answer>>>,
}

impl BlockConnector for Signalled {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let prev = cx.reader.entry(id).expect("indexed").prev;
        let v24 = cx.deployments.active_after(prev, Deployment::Versionbits(V24));
        self.asks.lock().expect("answers").push((block.hash(), v24));
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        Some(Arc::clone(&self.rows))
    }
}

/// V24's state for the block after each of `blocks` (`GetStateFor(pindexPrev)`), from V's own
/// deployments.
fn v24_after(rig: &Rig, blocks: &[Mined]) -> Vec<ThresholdState> {
    let ids = rig.ids(blocks);
    let states = rig.v.query(move |q| {
        let answer = |id| q.deployments.versionbits(Some(id), V24).expect("V24 on regtest");
        ids.into_iter().map(|id| answer(id).state).collect()
    });
    states.recv().expect("V serves")
}

/// V24's state for the block after height `h` on a chain whose every block signals, with its
/// signal at `signal`: `DEFINED` until the first period end at or past the signal (Core compares
/// that end's height with the signal's, `versionbits.cpp:69`, `:95` at v24.0.0), then one period
/// each `STARTED` and `LOCKED_IN`.
pub(super) fn core_state(signal: Option<u32>, h: u32) -> ThresholdState {
    let Some(signal) = signal else {
        return Defined;
    };
    // The block after the first period end (a height `PERIOD k - 1`) at or past the signal.
    let start = (signal + 1).div_ceil(PERIOD) * PERIOD;
    match (h + 1).checked_sub(start) {
        None => Defined,
        Some(n) if n < PERIOD => Started,
        Some(n) if n < 2 * PERIOD => LockedIn,
        Some(_) => Active,
    }
}

/// No signal: V24 stays `DEFINED` through four periods. A signal at 188 or 249 starts it from
/// 250, the first period's end at 249 being at or past the signal, so it locks in from 500 and
/// is active from 750 (the recorded `feature_asset_locks` run's 750); one at 250 starts it a
/// period later. Each case is a branch from genesis on one V, with one versionbits cache.
#[test]
fn v24_starts_at_the_first_period_end_at_or_past_its_signal() {
    let p = regtest_v20_at_100();
    let n = 4 * PERIOD + 2;
    let genesis = Mined::genesis(&p);
    let cases = [None, Some(188), Some(249), Some(250)];
    let branches: Vec<Vec<Mined>> =
        (0..).zip(cases).map(|(salt, _)| branch(&p, &genesis, n, salt)).collect();
    let signalled: Vec<&Mined> = cases
        .iter()
        .zip(&branches)
        .filter_map(|(signal, b)| signal.map(|s| &b[s as usize - 1]))
        .collect();
    let asks = Arc::new(Mutex::new(Vec::new()));
    let rows = Signals::at(&signalled);
    let rig = rig(
        &p,
        n,
        Box::new(Signalled {
            rows,
            asks,
        }),
    );
    for b in &branches {
        rig.headers(b);
    }
    for (signal, b) in cases.into_iter().zip(&branches) {
        let want: Vec<ThresholdState> = b.iter().map(|m| core_state(signal, m.height)).collect();
        assert_eq!(v24_after(&rig, b), want, "signal at {signal:?}");
    }
    let starts = cases.map(|s| (1..n).find(|&h| core_state(s, h) == Started).map(|h| h + 1));
    assert_eq!(starts, [None, Some(250), Some(250), Some(500)]);
    rig.assert_no_abort();
}

/// Branch A mines the signal at 188 and B, from 187, none. Connecting A, then B (one block
/// longer), then A again (two blocks more), the connector's context answers V24 active after 749
/// on A only: block 750's CbTx must be version 4 on A and below it on B (`evo/cbtx.cpp:45-51`).
/// V's one cache, keyed by block as Core's, stays right across both reorgs, and V24's state at
/// each tip follows the branch's signal.
#[test]
fn v24_and_the_cbtx_rule_follow_each_branches_signal_across_reorgs() {
    let p = regtest_v20_at_100();
    let genesis = Mined::genesis(&p);
    let a = branch(&p, &genesis, 3 * PERIOD, 0);
    let b = branch(&p, &a[186], 3 * PERIOD - 187 + 1, 1);
    let a_more = branch(&p, &a[3 * PERIOD as usize - 1], 2, 0);
    let asks = Arc::new(Mutex::new(Vec::new()));
    let rows = Signals::at(&[&a[187]]);
    assert_eq!(a[187].height, 188);
    let rig = rig(
        &p,
        3 * PERIOD + 2,
        Box::new(Signalled {
            rows,
            asks: asks.clone(),
        }),
    );
    // Genesis, connected at the start.
    asks.lock().expect("answers").clear();
    let connect = |m: &Mined| {
        let r = rig.v.process_block(m.block.clone(), true, None, true).recv().expect("V replies");
        assert!(r.accepted && r.state.is_valid(), "{}: {r:?}", m.height);
    };
    let tip_v24 = |rig: &Rig| {
        let tip = rig.v.view().snap.tip.tip;
        let state = rig.v.query(move |q| q.deployments.versionbits(Some(tip), V24).unwrap().state);
        state.recv().expect("V serves")
    };
    for m in &a {
        connect(m);
    }
    assert_eq!(tip_v24(&rig), Active);
    for m in &b {
        connect(m);
    }
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(b.last().unwrap()));
    assert_eq!(tip_v24(&rig), Defined);
    for m in &a_more {
        connect(m);
    }
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&a_more[1]));
    assert_eq!(tip_v24(&rig), Active);

    let answers = |blocks: &[Mined], from: Option<u32>| -> Vec<Answer> {
        blocks.iter().map(|m| (m.hash(), from.is_some_and(|f| m.height >= f))).collect()
    };
    let a_back: Vec<Mined> = a[187..].iter().chain(&a_more).cloned().collect();
    let want: Vec<Answer> =
        [answers(&a, Some(750)), answers(&b, None), answers(&a_back, Some(750))]
            .into_iter()
            .flatten()
            .collect();
    let got = asks.lock().expect("answers").clone();
    assert_eq!(got.len(), want.len());
    let wrong: Vec<_> = got.iter().zip(&want).filter(|(g, w)| g != w).collect();
    assert!(wrong.is_empty(), "{wrong:?}");
    rig.assert_no_abort();
}

/// Writes `bytes` as `block`'s MNHF row, as the evo flush would.
fn put_row(store: &super::node_evo::Store, block: &BlockHash, bytes: &[u8]) {
    let mut b = store.db().batch();
    b.put(EvoCf::Mnhf.cf(), EvoCf::Mnhf.key(block).as_bytes(), bytes);
    store.db().write(b, Commit::Durable).unwrap();
}

/// [`NodeEvo`]'s rows read a block's stored map (`GetFromCache`'s `m_evoDb.Read`,
/// `mnhftx.cpp:323-362`) with no block data. A block without one takes Core's rebuild
/// (`:285-321`), whose block read fails here, with no block data: `failed-getehfforblock-read`
/// (`:302-304`).
///
/// [`NodeEvo`]: crate::evo::node::NodeEvo
#[test]
fn the_rows_read_a_stored_map_and_fail_a_rebuild_without_block_data() {
    let p = regtest_v20_at_100();
    let chain = branch(&p, &Mined::genesis(&p), 190, 0);
    let rig = rig(&p, 190, Box::new(NullConnector));
    rig.headers(&chain);
    let (evo, _layer, store) = node_evo(&p, BlsCtx::BASIC);
    let stored = EhfSignals::from_iter([(V24_BIT, 188)]);
    put_row(&store, &chain[187].hash(), &encode_mnhf(&stored));
    let rows = evo.ehf_rows().expect("NodeEvo keeps the rows");
    let reader = rig.v.reader();
    assert_eq!(rows.get_for_block(reader, rig.id(&chain[187])).unwrap(), stored);
    let past = rows.get_for_block(reader, rig.id(&chain[188]));
    assert!(matches!(past, Err(GetForBlockError::Read)), "{past:?}");
    // Below V20 a block's map is empty and read from nowhere.
    let below = rows.get_for_block(reader, rig.id(&chain[98]));
    assert_eq!(below.unwrap(), EhfSignals::new());
    rig.assert_no_abort();
}

/// The blocks' data, as V's block files serve it to the rows' rebuild.
struct Blocks(HashMap<BlockHash, Arc<Block>>);

impl EvoBlocks for Blocks {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>> {
        self.0.get(&crate::chain::index::entry(reader, id).hash).cloned()
    }
}

/// A block on `parent` holding an MNHF signal for V24 after its coinbase. The rows' rebuild
/// extracts the signal (`extractSignals`, `mnhftx.cpp:190-208`); its quorum signature is EV1's
/// step 4's, which no rebuild runs.
fn signalling(p: &ChainSetup, parent: &Mined) -> Mined {
    let height = parent.height + 1;
    let payload = MnHardFork::new(1, V24_BIT, QuorumHash::from([0; 32]), operator_sig(0, &[0; 32]));
    let mut extra_payload = Vec::new();
    payload.encode(&mut extra_payload);
    let signal =
        Tx::new(Transaction::new(3, TxType::MnhfSignal, Vec::new(), Vec::new(), 0, extra_payload));
    let txs = vec![coinbase_at(height, 0), Arc::new(signal)];
    Mined {
        block: mine_version(p, parent.hash(), parent.time() + SPACING, txs, V24_VERSION),
        height,
    }
}

/// [`NodeEvo`]'s rows rebuild a block's map from the blocks' data where no row is stored, through
/// the node's wiring: its block reads, and its one MNHF manager (D16). Core walks back to the
/// first block whose map it holds, here 99, below V20, and processes each block after it from
/// disk, oldest first (`GetForBlock`, `mnhftx.cpp:285-321`). The map rebuilt at 190 is what a
/// stored row answers: the signal mined at 188. The connect path's `GetForBlock`, which never
/// rebuilds (EV3 D17), misses before the rebuild and then finds the map in that manager's cache.
///
/// [`NodeEvo`]: crate::evo::node::NodeEvo
#[test]
fn the_rows_rebuild_a_missing_map_from_the_blocks_data() {
    let p = regtest_v20_at_100();
    let mut chain = branch(&p, &Mined::genesis(&p), 187, 0);
    chain.push(signalling(&p, &chain[186]));
    let tail = branch(&p, &chain[187], 2, 0);
    chain.extend(tail);
    let rig = rig(&p, 190, Box::new(NullConnector));
    rig.headers(&chain);
    let reader = rig.v.reader();
    let tip = rig.id(&chain[189]);

    // The stored row's answer, with no block data.
    let (evo, _layer, store) = node_evo(&p, BlsCtx::BASIC);
    let want = EhfSignals::from_iter([(V24_BIT, 188)]);
    put_row(&store, &chain[189].hash(), &encode_mnhf(&want));
    let rows = evo.ehf_rows().expect("NodeEvo keeps the rows");
    let stored = rows.get_for_block(reader, tip).unwrap();
    assert_eq!(stored, want);

    // No row, the blocks' data readable.
    let blocks = Blocks(chain.iter().map(|m| (m.hash(), Arc::clone(&m.block))).collect());
    let (evo, layer, store) =
        node_evo_over_blocks(&p, BlsCtx::BASIC, Ev1NotBuilt, Box::new(blocks), None);
    let connect = |manager: &MnhfManager| {
        let ancestry = Ancestry(reader);
        let view = store.view(&layer.lock(), &ancestry);
        let mnhf = MnhfView {
            manager,
            params: &p,
            chain: &ancestry,
            store: &view,
        };
        mnhf.get_for_block(tip)
    };
    let before = connect(evo.mnhf_manager());
    assert!(before.is_err(), "no row and nothing cached: {before:?}");
    let rows = evo.ehf_rows().expect("NodeEvo keeps the rows");
    assert_eq!(rows.get_for_block(reader, tip).unwrap(), stored, "rebuilt");
    assert_eq!(connect(evo.mnhf_manager()).unwrap(), stored, "in the node's manager");
    rig.assert_no_abort();
}

/// BL-274: a stored bit at or past `VERSIONBITS_NUM_BITS` (29) is a corrupt row, as the in-block
/// signal's `bad-mnhf-nbit-out-of-bounds` domain says. V's versionbits, which read it through
/// `GetSignalsStage`, answer `GetForBlock`'s error rather than skip the signal; the engine
/// reports it once as it trips the latch, and the rows answer the store's error.
#[test]
fn a_stored_signal_bit_out_of_range_is_a_store_fault_reported_once() {
    let p = regtest_v20_at_100();
    let chain = branch(&p, &Mined::genesis(&p), PERIOD, 0);
    let sink = Arc::new(RecordingSink::default());
    let (evo, _layer, store) = node_evo_reporting(&p, BlsCtx::BASIC, sink.clone());
    // Core's `std::map<uint8_t, int>`: one entry, bit 29 at 188.
    let corrupt = [&[1, 29][..], &188_i32.to_le_bytes()].concat();
    put_row(&store, &chain[187].hash(), &corrupt);
    let asks = Arc::new(Mutex::new(Vec::new()));
    let rows = evo.ehf_rows().expect("NodeEvo keeps the rows");
    let rig = rig(
        &p,
        PERIOD,
        Box::new(Signalled {
            rows: rows.clone(),
            asks,
        }),
    );
    rig.headers(&chain);
    // The first period's end, 249, reads its map through 188's row.
    let end = rig.id(&chain[PERIOD as usize - 1]);
    let state = rig
        .v
        .query(move |q| q.deployments.try_versionbits(Some(end), V24).map(|a| a.map(|a| a.state)));
    let err = state.recv().expect("V answers").expect_err("the read's error");
    assert!(err.to_string().contains("bit 29 out of order or range"), "{err}");
    assert_eq!(sink.marks().len(), 1, "one report: {:?}", sink.marks());
    let tripped = store.db().latch().tripped().map(|f| (f.kind, f.site));
    assert_eq!(tripped, Some((ErrorKind::Corruption, "chainstate read")), "the latch trips");
    let read = rows.get_for_block(rig.v.reader(), rig.id(&chain[187]));
    let Err(GetForBlockError::Store(e)) = read else {
        panic!("a store fault: {read:?}");
    };
    assert!(e.to_string().contains("bit 29 out of order or range"), "{e}");
}

/// [`NullConnector`] whose rows are `rows`; it asks its context nothing.
struct RowsOnly(Arc<dyn EhfRows>);

impl BlockConnector for RowsOnly {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        Some(Arc::clone(&self.0))
    }
}

/// BL-683 D131: `GetForBlock`'s throw inside `UpdateTip`'s warning walk stops V. Blocks 1..287 set
/// bits 0 and 1, which no deployment uses (T8's chain); connecting 287 out of IBD walks bit 0's
/// window back to the failing read of 250, and V stops on it with one report.
#[test]
fn a_fault_in_update_tips_warning_walk_stops_v() {
    const FAULT: u32 = 250;
    let p = regtest_v20_at_100();
    let version = VERSIONBITS_TOP_BITS | 0b11;
    let mut chain: Vec<Mined> = Vec::with_capacity(287);
    for height in 1..=287 {
        let prev = chain.last().cloned().unwrap_or_else(|| Mined::genesis(&p));
        let cb = coinbase_at(height, 0);
        let block = mine_version(&p, prev.hash(), prev.time() + SPACING, vec![cb], version);
        chain.push(Mined {
            block,
            height,
        });
    }
    let rows = FaultingRows::new(FAULT);
    let mut rig = rig(&p, 287, Box::new(RowsOnly(rows.clone())));
    for m in &chain[..286] {
        assert!(rig.submit(m).accepted, "{}", m.height);
    }
    assert!(rows.take_reads().iter().all(|&h| h != FAULT), "no read of the fault below 287");
    rig.submit_stopping(&chain[286], "the walk's fault");
    assert_eq!(rows.take_reads().last(), Some(&FAULT));
    rig.stop_on("GetForBlock: failed-getehfforblock-read");
}
