//! DEC-1028: V's versionbits read the connector's MNHF signals (`CMNHFManager::GetForBlock`,
//! [`EhfRows`]). An EHF deployment with no signal stays `DEFINED`; with one, it starts at the
//! first period's end at or past the signal's height (`versionbits.cpp:38`, `:69`, `:93-97`,
//! `:238-251` at v24.0.0), on each branch as its own signals say, across reorgs and with the
//! connector's CbTx rule; [`NodeEvo`]'s rows read a block's stored map without its data, answer
//! a missing one with Core's `EvoDbMissingStateError` and never rebuild it from the blocks' data
//! (dashpay/dash#8098), and a stored bit out of range is a store fault, reported once, that V's
//! versionbits answer as `GetForBlock`'s error (BL-274).
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

use super::deployments::{FAULT_TEXT, FaultingRows};
use super::node_evo::{node_evo, node_evo_over_blocks, node_evo_reporting};
use super::support::{
    Ev, Mined, Rig, RigConfig, SPACING, cb_script, coinbase, install_accept_log, levels_on,
    lines_on, mine_version,
};
use crate::chain::{
    BlockConnector, BlockState, BlockStatusWriter, ChainCommand, ConnectCx, ConnectOutcome,
    DisconnectPool, DisconnectResult, EhfRows, Fatal, HeldMempool, MempoolSink, NullConnector,
    NullMempool, ProposalChecks, VContext, ViewTarget,
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
/// `mnhftx.cpp:333-373`) with no block data. A block without one is `GetForBlock`'s
/// `EvoDbMissingStateError`, "MNHF signals unavailable for block <hash>" (`:286-290`).
///
/// [`NodeEvo`]: crate::evo::node::NodeEvo
#[test]
fn the_rows_read_a_stored_map_and_answer_a_missing_one_as_unavailable() {
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
    assert_eq!(past, Err(GetForBlockError::Missing(chain[188].hash())));
    // Below V20 a block's map is empty and read from nowhere.
    let below = rows.get_for_block(reader, rig.id(&chain[98]));
    assert_eq!(below.unwrap(), EhfSignals::new());
    rig.assert_no_abort();
}

/// The blocks' data, as V's block files serve it.
struct Blocks(HashMap<BlockHash, Arc<Block>>);

impl EvoBlocks for Blocks {
    fn read(&self, reader: &ChainReader, id: BlockId) -> Option<Arc<Block>> {
        self.0.get(&crate::chain::index::entry(reader, id).hash).cloned()
    }
}

/// A block on `parent` holding an MNHF signal for V24 after its coinbase. Its quorum signature is
/// EV1's step 4's, which nothing here runs.
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

/// [`NodeEvo`]'s rows never rebuild a block's map from the blocks' data: with no row stored and
/// every block readable through the node's wiring, `GetForBlock` answers
/// `EvoDbMissingStateError` and caches nothing in the node's one MNHF manager (D16), so the
/// connect path's read misses too; a stored row then answers both.
///
/// [`NodeEvo`]: crate::evo::node::NodeEvo
// Core: evo/mnhftx.cpp:286-290 (dashpay/dash#8098)
#[test]
fn the_rows_never_rebuild_a_missing_map_from_the_blocks_data() {
    let p = regtest_v20_at_100();
    let mut chain = branch(&p, &Mined::genesis(&p), 187, 0);
    chain.push(signalling(&p, &chain[186]));
    let tail = branch(&p, &chain[187], 2, 0);
    chain.extend(tail);
    let rig = rig(&p, 190, Box::new(NullConnector));
    rig.headers(&chain);
    let reader = rig.v.reader();
    let tip = rig.id(&chain[189]);

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
    let missing = GetForBlockError::Missing(chain[189].hash());
    let rows = evo.ehf_rows().expect("NodeEvo keeps the rows");
    assert_eq!(rows.get_for_block(reader, tip), Err(missing.clone()));
    assert_eq!(connect(evo.mnhf_manager()), Err(missing), "nothing cached");

    let want = EhfSignals::from_iter([(V24_BIT, 188)]);
    put_row(&store, &chain[189].hash(), &encode_mnhf(&want));
    assert_eq!(rows.get_for_block(reader, tip).unwrap(), want);
    assert_eq!(connect(evo.mnhf_manager()).unwrap(), want);
    rig.assert_no_abort();
}

/// BL-274: a stored bit at or past `VERSIONBITS_NUM_BITS` (29) is a corrupt row, as the in-block
/// signal's `bad-mnhf-nbit-out-of-bounds` domain says. V's versionbits, which read it through
/// `GetSignalsStage`, answer `GetForBlock`'s error rather than skip the signal; the engine
/// reports it once as it trips the latch, and the rows answer the store's error. The row is the
/// period end's own: the rows read no other block's (dashpay/dash#8098).
#[test]
fn a_stored_signal_bit_out_of_range_is_a_store_fault_reported_once() {
    let p = regtest_v20_at_100();
    let chain = branch(&p, &Mined::genesis(&p), PERIOD, 0);
    let sink = Arc::new(RecordingSink::default());
    let (evo, _layer, store) = node_evo_reporting(&p, BlsCtx::BASIC, sink.clone());
    // Core's `std::map<uint8_t, int>`: one entry, bit 29 at 188.
    let corrupt = [&[1, 29][..], &188_i32.to_le_bytes()].concat();
    let end_block = &chain[PERIOD as usize - 1];
    put_row(&store, &end_block.hash(), &corrupt);
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
    // The first period's end reads its map through its own row.
    let end = rig.id(end_block);
    let state = rig
        .v
        .query(move |q| q.deployments.try_versionbits(Some(end), V24).map(|a| a.map(|a| a.state)));
    let err = state.recv().expect("V answers").expect_err("the read's error");
    assert!(err.to_string().contains("bit 29 out of order or range"), "{err}");
    assert_eq!(sink.marks().len(), 1, "one report: {:?}", sink.marks());
    let tripped = store.db().latch().tripped().map(|f| (f.kind, f.site));
    assert_eq!(tripped, Some((ErrorKind::Corruption, "chainstate read")), "the latch trips");
    let read = rows.get_for_block(rig.v.reader(), end);
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
    rig.stop_on(&format!("GetForBlock: {FAULT_TEXT}"));
}

/// T8's chain: blocks 1..287, each setting bits 0 and 1.
fn t8_blocks(p: &Arc<ChainSetup>) -> Vec<Mined> {
    let version = VERSIONBITS_TOP_BITS | 0b11;
    let mut chain: Vec<Mined> = Vec::with_capacity(287);
    for height in 1..=287 {
        let prev = chain.last().cloned().unwrap_or_else(|| Mined::genesis(p));
        let cb = coinbase_at(height, 0);
        let block = mine_version(p, prev.hash(), prev.time() + SPACING, vec![cb], version);
        chain.push(Mined {
            block,
            height,
        });
    }
    chain
}

/// A branch 286', 287' from T8's 285, after it.
fn t8_branch(p: &Arc<ChainSetup>, chain: &[Mined]) -> Vec<Mined> {
    let version = VERSIONBITS_TOP_BITS | 0b11;
    let mut branch = vec![chain[284].clone()];
    for height in 286..=287 {
        let prev = branch.last().cloned().expect("a parent");
        let block = mine_version(
            p,
            prev.hash(),
            prev.time() + SPACING,
            vec![coinbase_at(height, 1)],
            version,
        );
        branch.push(Mined {
            block,
            height,
        });
    }
    branch
}

/// [`RowsOnly`], whose connect of `.1` answers `.2`.
struct Injected(RowsOnly, BlockHash, ConnectOutcome);

impl BlockConnector for Injected {
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
        if block.hash() == self.1 {
            return Ok(self.2.clone());
        }
        self.0.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        self.0.disconnect_block(cx, v, target, block, id)
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
        self.0.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        self.0.ehf_rows()
    }
}

/// A V out of IBD over `rows` with `chain`'s 1..286 connected, none of which reads 250's row;
/// `inject`'s block connects to its outcome.
fn t8_rig(
    p: &Arc<ChainSetup>,
    chain: &[Mined],
    rows: &Arc<FaultingRows>,
    inject: Option<(BlockHash, ConnectOutcome)>,
    mempool: Box<dyn MempoolSink>,
) -> Rig {
    let only = RowsOnly(rows.clone());
    let connector: Box<dyn BlockConnector> = match inject {
        Some((at, outcome)) => Box::new(Injected(only, at, outcome)),
        None => Box::new(only),
    };
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        after_genesis: Some(i64::from(SPACING * 298)),
        no_check_block_index: true,
        connector: Some(connector),
        mempool: Some(mempool),
        ..RigConfig::default()
    });
    for m in &chain[..286] {
        assert!(rig.submit(m).accepted, "{}", m.height);
    }
    assert!(rows.take_reads().iter().all(|&h| h != 250), "no read of 250 below 287");
    rig
}

/// dashpay/dash#8098: `GetForBlock`'s `EvoDbMissingStateError` inside `UpdateTip`'s warning walk,
/// connecting 287 with 250's row missing. The throw leaves `UpdateTip` with the tip moved, and
/// before its line, then `ProcessNewBlock` with neither its "failed" nor its ACCEPTED line. No
/// report, the block is connected and valid, V keeps running, and a resubmission is a duplicate.
// Core: validation.cpp:3232-3247, :3490, :4911-4918 at v24.0.3; evo/mnhftx.cpp:286-290
#[test]
fn a_missing_row_in_update_tips_warning_walk_fails_process_new_block() {
    install_accept_log();
    let p = regtest_v20_at_100();
    let chain = t8_blocks(&p);
    let hash_250 = chain[249].hash();
    let rows = FaultingRows::missing(250, hash_250);
    let mut rig = t8_rig(&p, &chain, &rows, None, Box::new(NullMempool));
    let v = rig.v_thread();
    let top = &chain[286];
    let accepted = levels_on(v, "ProcessNewBlock : ACCEPTED").len();
    let want = format!("MNHF signals unavailable for block {hash_250}");

    let r = rig.submit(top);
    assert_eq!(rows.take_reads().last(), Some(&250));
    assert_eq!((r.accepted, r.new_block), (false, true), "{r:?}");
    assert_eq!(r.thrown.as_deref(), Some(want.as_str()));
    assert_eq!(r.state, BlockState::Valid);
    let failed = format!("ProcessNewBlock: ActivateBestChain failed ({want})");
    assert!(levels_on(v, &failed).is_empty(), "no failed line");
    assert_eq!(levels_on(v, "ProcessNewBlock : ACCEPTED").len(), accepted);
    let update_tip = format!("UpdateTip: new best={} ", top.hash());
    assert!(!lines_on(v).iter().any(|l| l.starts_with(&update_tip)), "no UpdateTip line");
    let id = rig.id(top);
    let state = rig.v.query(move |q| {
        let e = q.chain.reader().entry(id).expect("indexed");
        (q.chain.private_tip().map(|(t, _)| t.hash), e.status)
    });
    let (tip, status) = state.recv().expect("V serves");
    assert_eq!(tip, Some(top.hash()), "the tip moved");
    assert!(status.is_valid(dash_chain::Status::VALID_SCRIPTS), "{status:?}");

    let r = rig.submit(top);
    assert_eq!((r.accepted, r.new_block, r.thrown), (true, false, None), "a duplicate");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}

/// A mempool whose reorgs queue the disconnected blocks' transactions in Core's pool.
struct Queuing;

impl MempoolSink for Queuing {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(crate::reorg::DisconnectedBlockTransactions::default())
    }

    fn begin_hold(&mut self) -> Option<Box<dyn HeldMempool>> {
        Some(Box::new(NullMempool))
    }
}

/// dashpay/dash#8098: the same missing row met by a reorg's connect, after the step disconnected
/// a block, with a mempool: Core's throw unwinds through the step's disconnect pool, which still
/// holds the disconnected block's transactions, and its destructor's assert ends the process. V
/// stops on that assert with one report. 286 is disconnected for a branch 286', 287' from 285,
/// and the walk at 287' reads 250's row.
// Core: validation.cpp:3320, :3480-3482, :3625, :3490; kernel/disconnected_transactions.h:74-76
// at v24.0.3
#[test]
fn a_missing_row_in_a_reorgs_warning_walk_is_cores_disconnect_pool_assert() {
    let p = regtest_v20_at_100();
    let chain = t8_blocks(&p);
    let hash_250 = chain[249].hash();
    let rows = FaultingRows::missing(250, hash_250);
    let mut rig = t8_rig(&p, &chain, &rows, None, Box::new(Queuing));
    let branch = t8_branch(&p, &chain);
    let r = rig.submit(&branch[1]);
    assert!(r.accepted, "286' is stored: {r:?}");
    rig.submit_stopping(&branch[2], "the reorg's walk");
    let want = format!(
        "~DisconnectedBlockTransactions: assert(queuedTx.empty()) failed, unwinding from \
         ConnectTip: MNHF signals unavailable for block {hash_250}"
    );
    rig.stop_on(&want);
}

/// dashpay/dash#8098: the same throw with no mempool: the step's disconnect pool queued nothing,
/// so the throw leaves `ProcessNewBlock` with no assert and V keeps running.
// Core: validation.cpp:3320, :3480-3482; kernel/disconnected_transactions.h:74-76 at v24.0.3
#[test]
fn a_missing_row_in_a_reorgs_warning_walk_without_a_mempool_throws() {
    let p = regtest_v20_at_100();
    let chain = t8_blocks(&p);
    let hash_250 = chain[249].hash();
    let rows = FaultingRows::missing(250, hash_250);
    let mut rig = t8_rig(&p, &chain, &rows, None, Box::new(NullMempool));
    let branch = t8_branch(&p, &chain);
    let r = rig.submit(&branch[1]);
    assert!(r.accepted, "286' is stored: {r:?}");
    let r = rig.submit(&branch[2]);
    let want = format!("MNHF signals unavailable for block {hash_250}");
    assert_eq!((r.accepted, r.thrown.as_deref()), (false, Some(want.as_str())), "{r:?}");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(rig.abort.messages().is_empty(), "no report");
}

/// Rows each of which is missing: `GetForBlock`'s `EvoDbMissingStateError` for `.0` at any read.
struct AllMissing(BlockHash);

impl EhfRows for AllMissing {
    fn get_for_block(&self, _: &ChainReader, _: BlockId) -> Result<EhfSignals, GetForBlockError> {
        Err(GetForBlockError::Missing(self.0))
    }
}

/// A child of `prev` at `height` with T8's version, its coinbase salted by `salt`.
fn t8_child(p: &Arc<ChainSetup>, prev: &Mined, height: u32, salt: u32) -> Mined {
    let version = VERSIONBITS_TOP_BITS | 0b11;
    let cb = coinbase_at(height, salt);
    Mined {
        block: mine_version(p, prev.hash(), prev.time() + SPACING, vec![cb], version),
        height,
    }
}

/// T8's 1..288 over `mempool` with every row present, a branch 288' from 287 stored, then rows
/// each missing with both versionbits caches emptied: 289' reorgs, and the walk of the
/// disconnect's `UpdateTip` at 287 reads a missing row before any connect. Returns 289' and
/// Core's text.
fn t8_disconnect_walk(mempool: Box<dyn MempoolSink>) -> (Rig, Mined, String) {
    let p = regtest_v20_at_100();
    let chain = t8_blocks(&p);
    let rig = t8_rig(&p, &chain, &FaultingRows::new(u32::MAX), None, mempool);
    let top = &chain[286];
    let main = t8_child(&p, top, 288, 0);
    let side = t8_child(&p, top, 288, 1);
    let next = t8_child(&p, &side, 289, 1);
    assert!(rig.submit(top).accepted, "287 connects");
    assert!(rig.submit(&main).accepted, "288 connects");
    let r = rig.submit(&side);
    assert!(r.accepted, "288' is stored: {r:?}");
    assert_eq!(rig.v.view().snap.tip.tip_height, 288);
    let missing = chain[0].hash();
    let swap = ChainCommand::TestUnderV(Box::new(move |v| {
        v.deployments.set_ehf_rows(Some(Arc::new(AllMissing(missing))));
        Ok(())
    }));
    rig.v.send(swap).expect("V runs");
    (rig, next, format!("MNHF signals unavailable for block {missing}"))
}

/// dashpay/dash#8098: `UpdateTip`'s throw out of a reorg's `DisconnectTip` with no mempool: the
/// step's disconnect pool queued nothing, so the throw leaves `ProcessNewBlock` with no assert,
/// the tip moved back, and V keeps running.
// Core: validation.cpp:3320, :3334, :3625 at v24.0.3; kernel/disconnected_transactions.h:74-76
#[test]
fn a_missing_row_in_a_disconnects_warning_walk_without_a_mempool_throws() {
    let (mut rig, top, want) = t8_disconnect_walk(Box::new(NullMempool));
    let r = rig.submit(&top);
    assert_eq!((r.accepted, r.thrown.as_deref()), (false, Some(want.as_str())), "{r:?}");
    assert_eq!(rig.v.view().snap.tip.tip_height, 287, "288 is disconnected");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(rig.abort.messages().is_empty(), "no report");
}

/// dashpay/dash#8098: the same throw with a mempool: the disconnect queued 288's transactions,
/// and the pool's destructor asserts; V stops on it with one report.
// Core: validation.cpp:3320, :3334, :3625 at v24.0.3; kernel/disconnected_transactions.h:74-76
#[test]
fn a_missing_row_in_a_disconnects_warning_walk_is_cores_disconnect_pool_assert() {
    let (mut rig, top, want) = t8_disconnect_walk(Box::new(Queuing));
    rig.submit_stopping(&top, "the disconnect's walk");
    rig.stop_on(&format!(
        "~DisconnectedBlockTransactions: assert(queuedTx.empty()) failed, unwinding from \
         DisconnectTip: {want}"
    ));
}

/// dashpay/dash#8098: `ProcessBlock`'s `state.Error` for a missing row met by a reorg's connect,
/// after the step disconnected a block: ABCStep's system-error branch empties the disconnect pool
/// without re-adding and returns `false`, with no `AbortNode`. 286 is disconnected for a branch
/// 286', 287' from 285 and 286' fails: the tip stays at 285, `ProcessNewBlock` returns `false`
/// with its "failed" line and no throw, and V keeps running. The outcome is injected: on a real
/// connect `GetSpecialTxRules` reads the parent's row before `ProcessBlock` does.
// Core: validation.cpp:3625-3637, :3671-3675, :3798-3800, :4913-4916 at v24.0.3;
// evo/mnhftx.cpp:253-257
#[test]
fn a_missing_row_error_after_a_reorgs_disconnect_fails_the_step() {
    install_accept_log();
    let p = regtest_v20_at_100();
    let chain = t8_blocks(&p);
    let branch = t8_branch(&p, &chain);
    let want = "MNHF signals unavailable for block 00ab".to_owned();
    let inject = (branch[1].hash(), ConnectOutcome::Unavailable(want.clone()));
    let mut rig =
        t8_rig(&p, &chain, &FaultingRows::new(u32::MAX), Some(inject), Box::new(NullMempool));
    let v = rig.v_thread();
    let r = rig.submit(&branch[1]);
    assert!(r.accepted, "286' is stored: {r:?}");

    let r = rig.submit(&branch[2]);
    assert_eq!((r.accepted, r.new_block, r.thrown.as_deref()), (false, true, None), "{r:?}");
    let failed = format!("ProcessNewBlock: ActivateBestChain failed ({want})");
    assert_eq!(levels_on(v, &failed), [tracing::Level::ERROR]);
    let tip = rig.v.query(|q| q.chain.private_tip().map(|(t, _)| t.hash));
    assert_eq!(tip.recv().expect("V serves"), Some(chain[284].hash()), "286 is disconnected");
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}

/// dashpay/dash#8098: `GetSpecialTxRules`' `EvoDbMissingStateError` thrown out of the tip's
/// `ConnectBlock`, with no disconnect in the step: no `BlockChecked`, no `ConnectTip` line, and
/// `ProcessNewBlock` left by the throw with neither its "failed" nor its ACCEPTED line. The tip
/// stays at 286 and V keeps running; a resubmission meets the same throw, not a duplicate.
// Core: validation.cpp:2670, :3443-3448, :4911-4918 at v24.0.3; evo/mnhftx.cpp:286-290
#[test]
fn a_missing_row_thrown_out_of_the_tips_connect_leaves_process_new_block() {
    install_accept_log();
    let p = regtest_v20_at_100();
    let chain = t8_blocks(&p);
    let top = &chain[286];
    let want = "MNHF signals unavailable for block 00ab".to_owned();
    let inject = (top.hash(), ConnectOutcome::Thrown(want.clone()));
    let mut rig =
        t8_rig(&p, &chain, &FaultingRows::new(u32::MAX), Some(inject), Box::new(NullMempool));
    let v = rig.v_thread();
    let accepted = levels_on(v, "ProcessNewBlock : ACCEPTED").len();
    rig.rec.take();
    for new_block in [true, false] {
        let r = rig.submit(top);
        assert_eq!((r.accepted, r.new_block), (false, new_block), "{r:?}");
        assert_eq!(r.thrown.as_deref(), Some(want.as_str()));
        assert!(
            !rig.rec.take().iter().any(|e| matches!(e, Ev::Checked(h, _) if *h == top.hash())),
            "no BlockChecked"
        );
    }
    let line = format!("ConnectTip: ConnectBlock {} failed, {want}", top.hash());
    assert!(levels_on(v, &line).is_empty(), "no ConnectTip line");
    let failed = format!("ProcessNewBlock: ActivateBestChain failed ({want})");
    assert!(levels_on(v, &failed).is_empty(), "no failed line");
    assert_eq!(levels_on(v, "ProcessNewBlock : ACCEPTED").len(), accepted);
    let tip = rig.v.query(|q| q.chain.private_tip().map(|(t, _)| t.hash));
    assert_eq!(tip.recv().expect("V serves"), Some(chain[285].hash()));
    let exit = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}
