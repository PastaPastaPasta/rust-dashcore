//! Core's DMN tip and the live list on a real V (CH7 D16, D17; §6.3 steps 3, 4 and 6): the tip
//! dispatchers move it under Core's guard before any listener runs, init's
//! `InitializeCurrentBlockTip` sets it after startup, `reorg_drain` moves it only with a mempool
//! and `add`, every view publishes the list at it with the tip's credit pool, and
//! `ChainQuery::evo_view` answers both lists in one V turn. D21's startup check.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use dash_chain::ChainReader;
use dash_consensus::{Block, BlockId};
use dash_dml::creditpool::{CreditPool, PoolState};
use dash_dml::{MnList, StoreError};
use dash_params::BuriedDeployment;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::Numeric as _;
use dashd_store::{Commit, KvBatch as _, MemoryEngine};

use super::failure::restartable;
use super::support::{Mined, Rig, RigConfig, branch, cb_script, coinbase, mine, regtest};
use crate::chain::dmn::reorg_drain;
use crate::chain::index::entry;
use crate::chain::reply::WaitError;
use crate::chain::{
    BlockConnector, BlockStatusWriter, ChainCommand, ChainViewHandle, ConnectCx, ConnectOutcome,
    DisconnectPool, DisconnectResult, EvoReadCx, EvoReads, Fatal, LoadFailure, MempoolSink,
    NullConnector, NullMempool, ProposalChecks, RemovalReason, VContext, ValidationInterface,
    ValidationSignals, ViewTarget, check_evo_below_dip3,
};
use crate::store::CHAINSTATE_SCHEMA;

/// Lists tagged with their block: `list_at(b)` is Core's empty list with `b`'s hash, so a list
/// names the block it was read at. The pool records the block it is asked after; one quorum is
/// mined at `mined`, seen from tips that descend from it.
#[derive(Default)]
struct Tagged {
    pools: Mutex<Vec<BlockId>>,
    mined: OnceLock<BlockId>,
    fail: AtomicBool,
}

/// The LLMQ type of [`Tagged`]'s quorum.
const LLMQ: LlmqType = LlmqType::Llmq50_60;

/// The quorum [`Tagged`] has mined.
fn mined_quorum() -> QuorumHash {
    QuorumHash::from([7; 32])
}

impl EvoReads for Tagged {
    fn list_at(&self, cx: EvoReadCx<'_>, at: BlockId) -> Result<Arc<MnList>, StoreError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::read("planted".to_owned()));
        }
        Ok(Arc::new(MnList::default_pre_dip3().with_block_hash(entry(cx.reader, at).hash)))
    }

    fn credit_pool_after(
        &self,
        _: EvoReadCx<'_>,
        at: BlockId,
    ) -> Result<Option<Arc<PoolState>>, StoreError> {
        self.pools.lock().expect("pools").push(at);
        Ok(Some(Arc::new(Ok(CreditPool::default()))))
    }

    fn has_mined_commitment(
        &self,
        cx: EvoReadCx<'_>,
        _: LlmqType,
        quorum_hash: &QuorumHash,
        tip: BlockId,
    ) -> Result<bool, StoreError> {
        let Some(&mined) = self.mined.get() else {
            return Ok(false);
        };
        let height = entry(cx.reader, mined).height;
        Ok(*quorum_hash == mined_quorum() && cx.reader.ancestor(tip, height) == Some(mined))
    }
}

/// [`NullConnector`] whose evo reads are [`Tagged`]'s.
struct TaggedConnector(Arc<Tagged>);

impl BlockConnector for TaggedConnector {
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
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        &*self.0
    }
}

/// A tip signal as a listener saw it: its block, and the DMN tip V held and had published then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    init: bool,
    tip: Option<BlockId>,
    dmn: Option<BlockId>,
    published: Option<BlockId>,
}

/// Records each synchronous tip signal with the DMN tip at the call.
#[derive(Default)]
struct TipLog {
    signals: OnceLock<ValidationSignals>,
    view: OnceLock<ChainViewHandle>,
    seen: Mutex<Vec<Seen>>,
    /// Each queued `UpdatedBlockTip`'s new tip.
    updated: Mutex<Vec<BlockId>>,
    /// Each `InitializeCurrentBlockTip`'s `ibd`.
    init_ibd: Mutex<Vec<bool>>,
}

impl TipLog {
    fn attach(&self, rig: &Rig) {
        assert!(self.signals.set(rig.signals.clone()).is_ok());
        assert!(self.view.set(rig.v.view_handle()).is_ok());
        self.take();
    }

    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().expect("seen"))
    }

    fn push(&self, init: bool, tip: Option<BlockId>) {
        let seen = Seen {
            init,
            tip,
            dmn: self.signals.get().and_then(ValidationSignals::dmn_tip),
            published: self.view.get().and_then(|h| h.view().dmn_tip),
        };
        self.seen.lock().expect("seen").push(seen);
    }
}

impl ValidationInterface for TipLog {
    fn synchronous_updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        self.push(false, Some(new));
    }

    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        self.init_ibd.lock().expect("ibd").push(ibd);
        self.push(true, tip);
    }

    fn updated_block_tip(&self, new: BlockId, _: Option<BlockId>, _: bool) {
        self.updated.lock().expect("updated").push(new);
    }
}

/// A rig over [`Tagged`] with a [`TipLog`], on `files` if given.
fn tagged_cfg(tagged: &Arc<Tagged>, log: &Arc<TipLog>) -> RigConfig {
    RigConfig {
        connector: Some(Box::new(TaggedConnector(tagged.clone()))),
        listener: Some(log.clone()),
        ..RigConfig::default()
    }
}

/// The published view's DMN tip and the block its list names.
fn published(h: &ChainViewHandle) -> (Option<BlockId>, BlockHash) {
    let v = h.view();
    (v.dmn_tip, *v.mn_list.block_hash())
}

/// D16: the DMN tip moves on `SynchronousUpdatedBlockTip` before any listener runs, init's
/// first ABC included (it connects genesis with a null fork), and on `InvalidateBlock`'s signal;
/// the published list is the one at it.
#[test]
fn the_dmn_tip_moves_on_the_tip_signal_before_its_listeners() {
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig = Rig::with(tagged_cfg(&tagged, &log));
    log.attach(&rig);
    let h = rig.v.view_handle();
    let g = rig.id(&rig.genesis);
    assert_eq!(published(&h), (Some(g), rig.genesis.hash()));

    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let ids = rig.ids(&main);
    assert_eq!(published(&h), (Some(ids[2]), main[2].hash()));
    let seen = log.take();
    assert!(!seen.is_empty());
    for s in seen {
        assert!(!s.init && s.dmn == s.tip, "{s:?}");
    }

    assert_eq!(rig.v.invalidate_block(main[2].hash()).recv(), Ok(Ok(())));
    assert_eq!(published(&h), (Some(ids[1]), main[1].hash()));
    assert!(log.take().iter().all(|s| s.dmn == s.tip));
    rig.assert_no_abort();
}

/// The dispatchers' guard (`dsnotificationinterface.cpp:31-35`, `:47-53`): a signal whose new tip
/// is its fork (blocks only disconnected) leaves the DMN tip, any other moves it, and the init
/// signal moves it only when there is a tip.
#[test]
fn the_dispatchers_move_the_dmn_tip_under_cores_guard() {
    let signals = ValidationSignals::default();
    let rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    rig.headers(&main);
    let [a, b] = rig.ids_n::<2>(&main);
    assert_eq!(signals.dmn_tip(), None);
    signals.initialize_dmn_tip(None);
    assert_eq!(signals.dmn_tip(), None);
    signals.sync_updated_block_tip(a, Some(a), false);
    assert_eq!(signals.dmn_tip(), None);
    signals.sync_updated_block_tip(a, None, false);
    assert_eq!(signals.dmn_tip(), Some(a));
    signals.sync_updated_block_tip(b, Some(a), false);
    assert_eq!(signals.dmn_tip(), Some(b));
    signals.sync_updated_block_tip(a, Some(a), false);
    assert_eq!(signals.dmn_tip(), Some(b));
    signals.initialize_dmn_tip(Some(a));
    assert_eq!(signals.dmn_tip(), Some(a));
}

/// §6.3 step 6 after a restart with no block to connect: no DMN tip, so every reader gets the
/// empty list while `evo_view`'s tip list is the tip's; init's command sets it, publishes it,
/// then runs the listeners, which see it published.
#[test]
fn init_sets_the_dmn_tip_after_a_restart_with_nothing_to_connect() {
    let (mut rig, files) = restartable();
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    for m in &main {
        rig.accept(m);
    }
    let tip = rig.id(&main[1]);
    let exit = rig.stop();
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig =
        Rig::restart_with(exit, Box::new(files), tip, tagged_cfg(&tagged, &log)).expect("starts");
    log.attach(&rig);
    let h = rig.v.view_handle();
    assert_eq!(published(&h), (None, BlockHash::ZERO));
    let lists = h.query(|q| {
        let e = q.evo_view();
        let tip_list = *e.tip_list().expect("reads").block_hash();
        (e.tip, e.dmn_tip, tip_list, *e.dmn_list().expect("reads").block_hash())
    });
    assert_eq!(lists.recv(), Ok((Some(tip), None, main[1].hash(), BlockHash::ZERO)));

    let moves = rig.signals.dmn_moves();
    assert_eq!(rig.v.initialize_current_block_tip().recv(), Ok(()));
    assert_eq!(rig.signals.dmn_moves(), moves + 1, "V's handler moves the DMN tip once");
    assert_eq!(published(&h), (Some(tip), main[1].hash()));
    let init = Seen {
        init: true,
        tip: Some(tip),
        dmn: Some(tip),
        published: Some(tip),
    };
    assert_eq!(log.take(), [init]);
    rig.assert_no_abort();
}

/// D62: init's command reads `ibd` as V serves it, after loadblk clears `fImporting`. On a chain
/// whose tip is recent only once the import's block connects, the command served while V still
/// loads hears `true` (Core's `fImporting` check, the IBD latch still open) and the one served
/// after `SetLoading(false)` hears `false`.
// Core: validation.cpp:1976-1998, node/blockstorage.cpp:960, init.cpp:2460-2473
#[test]
fn init_reads_ibd_when_it_is_served() {
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig = Rig::with(tagged_cfg(&tagged, &log));
    log.attach(&rig);
    let importing = |on| ChainCommand::SetLoading {
        importing: Some(on),
        reindex: None,
    };
    rig.v.send(importing(true)).expect("V runs");
    // The rig's clock is a day past genesis: a block a minute old is a recent tip.
    let recent = Mined {
        block: mine(
            &rig.p,
            rig.genesis.hash(),
            rig.genesis.time() + 24 * 60 * 60 - 60,
            vec![coinbase(&cb_script(1, 0))],
        ),
        height: 1,
    };
    rig.accept(&recent);
    assert_eq!(rig.v.initialize_current_block_tip().recv(), Ok(()));
    rig.v.send(importing(false)).expect("V runs");
    assert_eq!(rig.v.initialize_current_block_tip().recv(), Ok(()));
    assert_eq!(*log.init_ibd.lock().expect("ibd"), [true, false]);
    rig.assert_no_abort();
}

/// D16 at a restart with blocks above the coins' tip: init's ABC connects them and its signal
/// sets the DMN tip before the command, which then sets it again at the same tip.
#[test]
fn init_abc_sets_the_dmn_tip_before_the_command() {
    let (mut rig, files) = restartable();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let [a1, _, a3] = rig.ids_n::<3>(&main);
    let exit = rig.stop();
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig =
        Rig::restart_with(exit, Box::new(files), a1, tagged_cfg(&tagged, &log)).expect("starts");
    log.attach(&rig);
    let h = rig.v.view_handle();
    assert_eq!(published(&h), (Some(a3), main[2].hash()));
    assert_eq!(rig.v.initialize_current_block_tip().recv(), Ok(()));
    assert_eq!(published(&h), (Some(a3), main[2].hash()));
    rig.assert_no_abort();
}

/// `ChainView.credit_pool` is `GetCreditPool(Tip())`: each view asks the reads after its tip,
/// and `evo_view` answers the pool and `HasMinedCommitment` on the active chain in its turn.
#[test]
fn the_view_and_evo_view_read_the_tip() {
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig = Rig::with(tagged_cfg(&tagged, &log));
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &main[0], 3, 1);
    rig.accept(&main[0]);
    rig.accept(&main[1]);
    let a2 = rig.id(&main[1]);
    assert!(tagged.mined.set(a2).is_ok());
    assert!(rig.v.view_handle().view().credit_pool.is_some());
    assert_eq!(tagged.pools.lock().expect("pools").last(), Some(&a2));

    let h = rig.v.view_handle();
    let answers = || {
        h.query(|q| {
            let e = q.evo_view();
            let pool = e.credit_pool().expect("reads").is_some();
            let mined = |q| e.has_mined_commitment(LLMQ, &q).expect("reads");
            (e.tip, pool, mined(mined_quorum()), mined(QuorumHash::from([8; 32])))
        })
        .recv()
    };
    assert_eq!(answers(), Ok((Some(a2), true, true, false)));
    // The side chain overtakes: a2 is off the active chain, and so is its commitment.
    for m in &side {
        rig.accept(m);
    }
    let b3 = rig.id(&side[2]);
    assert_eq!(answers(), Ok((Some(b3), true, false, false)));
    assert_eq!(tagged.pools.lock().expect("pools").last(), Some(&b3));
    rig.assert_no_abort();
}

/// A store fault reading the evo fields is `AbortNode`: the whole last view stays published,
/// its chain snapshot and `view_seq` included, and neither the release nor init's command emits
/// a signal for the tip no view shows.
#[test]
fn a_fault_reading_the_list_aborts_and_keeps_the_whole_last_view() {
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig = Rig::with(tagged_cfg(&tagged, &log));
    let g = rig.id(&rig.genesis);
    let h = rig.v.view_handle();
    rig.sync();
    log.updated.lock().expect("updated").clear();
    let before = h.view();
    tagged.fail.store(true, Ordering::SeqCst);
    rig.accept(&branch(&rig.p, &rig.genesis, 1, 0)[0]);
    assert_eq!(rig.v.initialize_current_block_tip().recv(), Ok(()));
    rig.sync();
    assert!(Arc::ptr_eq(&before, &h.view()), "a new view was stored");
    assert_eq!(published(&h), (Some(g), rig.genesis.hash()));
    assert_eq!(*log.updated.lock().expect("updated"), []);
    assert_eq!(*log.init_ibd.lock().expect("ibd"), [], "init's command dispatched nothing");
    let messages = rig.abort.messages();
    assert!(messages.iter().any(|m| m.starts_with("GetListAtChainTip: ")), "{messages:?}");
}

/// A bound on a wait that should end at once: a regression fails instead of hanging.
const SOON: Duration = Duration::from_secs(120);

/// What [`InitProbe`] saw in an `InitializeCurrentBlockTip` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InitCall {
    tip: Option<BlockId>,
    ibd: bool,
    /// The DMN tip at the call.
    dmn: Option<BlockId>,
    /// The published view's DMN tip at the call.
    published: Option<BlockId>,
    /// The tip's height, read from the `ChainReader` inside the call.
    height: Option<u32>,
}

/// A listener that records each `InitializeCurrentBlockTip`, reading the `ChainReader` and the
/// published view inside the call, then tells the test it has entered and waits to be let go:
/// the reply must wait for it (D72).
struct InitProbe {
    signals: OnceLock<ValidationSignals>,
    view: OnceLock<ChainViewHandle>,
    reader: OnceLock<ChainReader>,
    calls: Mutex<Vec<InitCall>>,
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl InitProbe {
    /// The probe, the receiver its calls enter on, and the sender that lets each one go.
    fn new() -> (Arc<InitProbe>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, on_enter) = mpsc::channel();
        let (go, release) = mpsc::channel();
        let probe = InitProbe {
            signals: OnceLock::new(),
            view: OnceLock::new(),
            reader: OnceLock::new(),
            calls: Mutex::default(),
            entered: Mutex::new(entered),
            release: Mutex::new(release),
        };
        (Arc::new(probe), on_enter, go)
    }

    fn attach(&self, rig: &Rig) {
        assert!(self.signals.set(rig.signals.clone()).is_ok());
        assert!(self.view.set(rig.v.view_handle()).is_ok());
        assert!(self.reader.set(rig.v.reader().clone()).is_ok());
    }

    fn calls(&self) -> Vec<InitCall> {
        self.calls.lock().expect("calls").clone()
    }
}

impl ValidationInterface for InitProbe {
    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        let reader = self.reader.get().expect("attached");
        let call = InitCall {
            tip,
            ibd,
            dmn: self.signals.get().and_then(ValidationSignals::dmn_tip),
            published: self.view.get().and_then(|h| h.view().dmn_tip),
            height: tip.and_then(|id| reader.entry(id)).map(|e| e.height),
        };
        self.calls.lock().expect("calls").push(call);
        let _ = self.entered.lock().expect("entered").send(());
        let _ = self.release.lock().expect("release").recv_timeout(SOON);
    }
}

/// A rig over [`Tagged`] with an [`InitProbe`], `fImporting` set, and two blocks whose times
/// make each a recent tip on the rig's clock (a day past genesis): out of IBD once the flag
/// clears, in it while the import runs.
fn importing_rig(tagged: &Arc<Tagged>, probe: &Arc<InitProbe>) -> (Rig, [BlockId; 2]) {
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(TaggedConnector(tagged.clone()))),
        listener: Some(probe.clone()),
        ..RigConfig::default()
    });
    probe.attach(&rig);
    rig.v.send(importing(true)).expect("V runs");
    let day = rig.genesis.time() + 24 * 60 * 60;
    let b1 = mine(&rig.p, rig.genesis.hash(), day - 120, vec![coinbase(&cb_script(1, 0))]);
    let b2 = mine(&rig.p, b1.hash(), day - 60, vec![coinbase(&cb_script(2, 0))]);
    let blocks = [
        Mined {
            block: b1,
            height: 1,
        },
        Mined {
            block: b2,
            height: 2,
        },
    ];
    for m in &blocks {
        rig.accept(m);
    }
    let ids = rig.ids_n::<2>(&blocks);
    (rig, ids)
}

/// `SetLoading { importing }`.
fn importing(on: bool) -> ChainCommand {
    ChainCommand::SetLoading {
        importing: Some(on),
        reindex: None,
    }
}

/// D72: after an `AbortNode`, V's reaper answers init's `InitializeCurrentBlockTip` in Core's
/// order: the `SetLoading(false)` queued first is served, `ibd` is read with `fImporting`
/// cleared, the DMN tip moves once to V's tip as V's loop ended (here one step back, taken after
/// the `AbortNode` as a carried step is, so behind the published view's tip), the listeners run,
/// and only then the reply. A listener that reads the `ChainReader` in the call does not block,
/// and no view is published.
// Core: init.cpp:2455-2473, node/blockstorage.cpp:883-887, :950, dsnotificationinterface.cpp:31-35
#[test]
fn the_reaper_answers_init_after_an_abort_in_cores_order() {
    let (tagged, (probe, entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let (mut rig, [b1, b2]) = importing_rig(&tagged, &probe);
    let h = rig.v.view_handle();
    let before = h.view();
    assert_eq!(before.dmn_tip, Some(b2));
    let fail = ChainCommand::TestOnV(Box::new(|v| {
        let f = v.abort_at_fault::<()>(Err(Fatal::new("disk on fire"))).expect_err("stops");
        v.chain.disconnect_tip().expect("b2 is the tip");
        Err(f)
    }));
    rig.v.send(fail).expect("sent");
    rig.v.send(importing(false)).expect("the lane");
    let moves = rig.signals.dmn_moves();
    let init = rig.v.initialize_current_block_tip();
    assert_eq!(entered.recv_timeout(SOON), Ok(()), "the listener runs on the reaper");
    assert_eq!(init.try_recv(), None, "no reply before the listeners return");
    go.send(()).expect("the probe waits");
    assert_eq!(init.recv_timeout(SOON), Some(Ok(())));
    let call = InitCall {
        tip: Some(b1),
        ibd: false,
        dmn: Some(b1),
        published: Some(b2),
        height: Some(1),
    };
    assert_eq!(probe.calls(), [call]);
    assert_eq!(rig.signals.dmn_moves(), moves + 1, "the DMN tip moves once");
    assert_eq!(rig.signals.dmn_tip(), Some(b1));
    assert!(Arc::ptr_eq(&before, &h.view()), "no view is published");
    rig.stop_on("disk on fire");
}

/// D72 on a shutdown's stop: Core's ABC returns on `ShutdownRequested()` and `loadblk` still
/// calls `InitializeCurrentBlockTip`, which V's reaper answers as after an `AbortNode`.
// Core: init.cpp:2455-2473, node/blockstorage.cpp:944-961
#[test]
fn the_reaper_answers_init_after_a_shutdown() {
    let (tagged, (probe, _entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let (mut rig, [_, b2]) = importing_rig(&tagged, &probe);
    assert!(rig.stop().fatal.is_none());
    go.send(()).expect("the probe waits");
    rig.v.send(importing(false)).expect("the reaper's lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(rig.v.initialize_current_block_tip().recv_timeout(SOON), Some(Ok(())));
    let call = InitCall {
        tip: Some(b2),
        ibd: false,
        dmn: Some(b2),
        published: Some(b2),
        height: Some(2),
    };
    assert_eq!(probe.calls(), [call]);
    assert_eq!(rig.signals.dmn_moves(), moves + 1);
}

/// D72: a V that panicked (or failed one of Core's asserts, which end Core's process before
/// `loadblk` gets there) leaves init's command unanswered: its reply disconnects, the DMN tip
/// stays and no listener runs.
#[test]
fn the_reaper_drops_init_after_a_panic() {
    let (tagged, (probe, _entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let (mut rig, _) = importing_rig(&tagged, &probe);
    go.send(()).expect("the probe waits");
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| panic!("index corrupt")))).expect("sent");
    let joined = rig.join.take().expect("running").join().expect("an unmarked joiner");
    assert!(joined.is_err(), "the panic reaches V's joiner");
    rig.v.send(importing(false)).expect("the reaper's lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(
        rig.v.initialize_current_block_tip().recv_timeout(SOON),
        Some(Err(WaitError::Disconnected))
    );
    assert_eq!(rig.signals.dmn_moves(), moves);
    assert_eq!(probe.calls(), []);
}

/// D72 with D65: V stopped with its last publication withheld, so, as V's own handler, the
/// reaper moves the DMN tip and runs no listener, whatever the order of V's stop and the
/// command.
#[test]
fn the_reaper_runs_no_listener_after_a_withheld_view() {
    let (tagged, (probe, _entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let (mut rig, [b1, _]) = importing_rig(&tagged, &probe);
    go.send(()).expect("the probe waits");
    tagged.fail.store(true, Ordering::SeqCst);
    let fail = ChainCommand::TestOnV(Box::new(|v| {
        v.chain.disconnect_tip().expect("b2 is the tip");
        assert!(!v.publish_view(), "the evo reads fail");
        Err(Fatal::new("disk on fire"))
    }));
    rig.v.send(fail).expect("sent");
    rig.v.send(importing(false)).expect("the lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(rig.v.initialize_current_block_tip().recv_timeout(SOON), Some(Ok(())));
    assert_eq!(rig.signals.dmn_moves(), moves + 1);
    assert_eq!(rig.signals.dmn_tip(), Some(b1));
    assert_eq!(probe.calls(), []);
    assert!(rig.stop().fatal.is_some());
}

/// D72: an `AbortNode` V carried past, then one of Core's asserts, which ends Core's process
/// before `loadblk` calls `InitializeCurrentBlockTip`: the reaper, which took the lanes at the
/// `AbortNode`, still drops the command.
// Core: node/blockstorage.cpp:944-961, init.cpp:2455-2473
#[test]
fn the_reaper_drops_init_after_an_assert_past_an_abort() {
    let (tagged, (probe, _entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let (mut rig, _) = importing_rig(&tagged, &probe);
    go.send(()).expect("the probe waits");
    let fail = ChainCommand::TestOnV(Box::new(|v| {
        let _ = v.abort_at_fault::<()>(Err(Fatal::new("disk on fire"))).expect_err("stops");
        Err(Fatal::assert("CheckBlockIndex: planted"))
    }));
    rig.v.send(fail).expect("sent");
    rig.v.send(importing(false)).expect("the lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(
        rig.v.initialize_current_block_tip().recv_timeout(SOON),
        Some(Err(WaitError::Disconnected))
    );
    assert_eq!(rig.signals.dmn_moves(), moves);
    assert_eq!(probe.calls(), []);
    assert!(rig.stop().fatal.is_some());
}

/// A listener that panics in `InitializeCurrentBlockTip`.
struct PanickingInit;

impl ValidationInterface for PanickingInit {
    fn initialize_current_block_tip(&self, _: Option<BlockId>, _: bool) {
        panic!("planted");
    }
}

/// D72: a listener's panic on the reaper is `AbortNode`, as on the signals thread: the sink hears
/// it and the node's flag is set; the command's reply disconnects, and the reaper still drops
/// what comes after, so nothing waits on it.
#[test]
fn a_listener_panic_on_the_reaper_aborts_the_node() {
    let mut rig = Rig::with(RigConfig {
        listener: Some(Arc::new(PanickingInit)),
        ..RigConfig::default()
    });
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("disk on fire"))))).expect("sent");
    assert_eq!(
        rig.v.initialize_current_block_tip().recv_timeout(SOON),
        Some(Err(WaitError::Disconnected))
    );
    assert_eq!(rig.v.query(|_| ()).recv_timeout(SOON), Some(Err(WaitError::Disconnected)));
    assert!(rig.v.shutdown_requested());
    assert_eq!(
        rig.abort.messages(),
        ["disk on fire", "a validation interface listener panicked: planted"]
    );
    assert!(rig.stop().fatal.is_some());
}

/// What a test sink saw at each drain: `add`, the DMN tip then, and its `tip_list`'s block
/// (`None` for a drain given no list).
type Drain = (bool, Option<BlockId>, Option<BlockHash>);

/// A mempool, or none, that records each drain.
struct Drains {
    has: bool,
    signals: ValidationSignals,
    seen: Arc<Mutex<Vec<Drain>>>,
}

impl MempoolSink for Drains {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }

    fn maybe_update_for_reorg(&mut self, _: Box<dyn DisconnectPool>, add: bool) {
        self.seen.lock().expect("seen").push((add, self.signals.dmn_tip(), None));
    }

    fn remove_for_block(&mut self, _: &[dash_consensus::TxRef], _: u32) {}

    fn remove_expired_asset_unlock(&mut self, _: u32) {}

    fn remove_recursive(&mut self, _: &dash_consensus::TxRef, _: RemovalReason) {}

    fn add_transactions_updated(&mut self, _: u32) {}

    fn check(&mut self, _: u32) {}

    fn has_mempool(&self) -> bool {
        self.has
    }

    fn maybe_update_for_reorg_at(
        &mut self,
        _: Box<dyn DisconnectPool>,
        add: bool,
        tip_list: &Arc<MnList>,
    ) {
        let drain = (add, self.signals.dmn_tip(), Some(*tip_list.block_hash()));
        self.seen.lock().expect("seen").push(drain);
    }
}

/// D16's `reorg_drain` (`validation.cpp:420-433`): without a mempool nothing, not even the sink;
/// with one, an adding drain moves the DMN tip to the chain tip first and the sink gets the list
/// at the moved tip; a non-adding drain reads no list, so a store fault cannot stop it; a view
/// published after carries the move.
#[test]
fn reorg_drain_moves_the_dmn_tip_only_with_a_mempool_and_add() {
    let tagged = Arc::new(Tagged::default());
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(TaggedConnector(tagged.clone()))),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    rig.headers(&main);
    let [a1, a2] = rig.ids_n::<2>(&main);
    let (h1, h2) = (main[0].hash(), main[1].hash());
    let out = Arc::new(Mutex::new(Vec::new()));
    let put = out.clone();
    rig.on_v(move |v| {
        let deployments = v.deployments.view();
        let cx = EvoReadCx {
            reader: &v.reader,
            deployments: &deployments,
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let drains = |has| Drains {
            has,
            signals: v.signals.clone(),
            seen: seen.clone(),
        };
        let mut facts = Vec::new();
        for (has, add) in [(false, true), (false, false)] {
            v.signals.move_dmn_tip(a1);
            reorg_drain(&mut drains(has), &v.signals, &*tagged, cx, Some(a2), Box::new(()), add)?;
            facts.push((v.signals.dmn_tip(), seen.lock().expect("seen").len()));
        }
        v.signals.move_dmn_tip(a1);
        reorg_drain(&mut drains(true), &v.signals, &*tagged, cx, Some(a2), Box::new(()), false)?;
        facts.push((v.signals.dmn_tip(), seen.lock().expect("seen").len()));
        tagged.fail.store(true, Ordering::SeqCst);
        reorg_drain(&mut drains(true), &v.signals, &*tagged, cx, Some(a2), Box::new(()), false)?;
        tagged.fail.store(false, Ordering::SeqCst);
        facts.push((v.signals.dmn_tip(), seen.lock().expect("seen").len()));
        reorg_drain(&mut drains(true), &v.signals, &*tagged, cx, None, Box::new(()), true)?;
        facts.push((v.signals.dmn_tip(), seen.lock().expect("seen").len()));
        reorg_drain(&mut drains(true), &v.signals, &*tagged, cx, Some(a2), Box::new(()), true)?;
        facts.push((v.signals.dmn_tip(), seen.lock().expect("seen").len()));
        v.publish_view();
        let drained = seen.lock().expect("seen").clone();
        *put.lock().expect("out") = vec![(facts, drained)];
        Ok(())
    });
    let (facts, drained) = out.lock().expect("out").pop().expect("V ran it");
    let view = rig.v.view_handle().view().dmn_tip;
    let at = |n| (Some(a1), n);
    assert_eq!(facts, [at(0), at(0), at(1), at(2), at(3), (Some(a2), 4)]);
    let lagging = (false, Some(a1), None);
    assert_eq!(drained, [lagging, lagging, (true, Some(a1), Some(h1)), (true, Some(a2), Some(h2))]);
    assert_eq!(view, Some(a2));
    rig.assert_no_abort();
}

/// `NullMempool` is Core without a mempool (`if (!m_mempool) return;`).
#[test]
fn the_null_mempool_has_none() {
    assert!(!NullMempool.has_mempool());
}

/// D21 (§6.3 step 3): at a tip below DIP0003 every evo column family but the effects' must be
/// empty, else Core's `EvoDbInconsistencyMessage`; from DIP0003 on nothing is checked.
#[test]
fn the_startup_check_below_dip3() {
    let p = regtest();
    let dip3 = p.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    let mut rig = Rig::new();
    let main = branch(&p, &Mined::genesis(&p), dip3 + 1, 0);
    rig.headers(&main);
    let (below, at) = (rig.id(&main[dip3 as usize - 2]), rig.id(&main[dip3 as usize - 1]));
    let exit = rig.stop();
    let engine = || crate::evo::testkit::guarded(MemoryEngine::new(&CHAINSTATE_SCHEMA));
    let put = |db: &crate::store::GuardedEngine<MemoryEngine>, name: &str| {
        let mut b = db.batch();
        b.put(CHAINSTATE_SCHEMA.cf(name).expect("a column family"), &[1], &[2]);
        db.write(b, Commit::Durable).expect("written");
    };
    assert_eq!(check_evo_below_dip3(&p, &exit.chain, below, &engine()), Ok(()));
    for name in ["mn_diff", "mn_snapshot", "credit_pool", "mnhf", "llmq_mined", "llmq_mined_h"] {
        let db = engine();
        put(&db, name);
        let refused = check_evo_below_dip3(&p, &exit.chain, below, &db);
        let cause = format!("`{name}` holds records at a tip below DIP0003");
        assert_eq!(
            refused,
            Err(LoadFailure::EvoInconsistent {
                cause
            }),
            "{name}"
        );
        assert_eq!(
            refused.map_err(|e| e.to_string()),
            Err("Found EvoDB inconsistency, you must reindex to continue".to_owned())
        );
        assert_eq!(check_evo_below_dip3(&p, &exit.chain, at, &db), Ok(()), "{name} at DIP3");
    }
    let db = engine();
    put(&db, "llmq_snapshot");
    put(&db, "coins");
    assert_eq!(check_evo_below_dip3(&p, &exit.chain, below, &db), Ok(()));
}
