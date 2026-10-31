//! Core's DMN tip and the live list on a real V (CH7 D16, D17; §6.3 steps 3, 4 and 6): the tip
//! dispatchers move it under Core's guard before any listener runs, init's
//! `InitializeCurrentBlockTip` sets it after startup, `reorg_drain` moves it only with a mempool
//! and `add`, every view publishes the list at it with the tip's credit pool, and
//! `ChainQuery::evo_view` answers both lists in one V turn.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use dash_chain::{ChainError, ChainReader};
use dash_consensus::{Block, BlockId};
use dash_dml::creditpool::{CreditPool, PoolState};
use dash_dml::{MnList, StoreError};
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::Numeric as _;

use super::failure::restartable;
use super::locks::{intake, lock, record, settle};
use super::support::{Mined, Rig, RigConfig, branch, cb_script, coinbase, mine};
use crate::chain::dmn::reorg_drain;
use crate::chain::index::entry;
use crate::chain::reply::WaitError;
use crate::chain::{
    BlockConnector, BlockStatusWriter, ChainCommand, ChainViewHandle, ConnectCx, ConnectOutcome,
    DisconnectPool, DisconnectResult, EvoReadCx, EvoReads, Fatal, MempoolSink, NullConnector,
    NullMempool, ProposalChecks, RemovalReason, VContext, ValidationInterface, ValidationSignals,
    ViewTarget,
};

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

/// D72 (Opus r1 m-2): the reaper reads `ibd` at V's tip, not at the published view's. V's tip
/// here is a recent block one step past the view's stale tip, taken after the `AbortNode` as a
/// carried step is: Core's `IsInitialBlockDownload()` reads `m_chain.Tip()`, which is recent, so
/// `false`; the view's tip would give `true`.
// Core: validation.cpp:1976-1998, init.cpp:2460-2463
#[test]
fn the_reaper_reads_ibd_at_vs_tip_not_the_views() {
    let (tagged, (probe, _entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(TaggedConnector(tagged.clone()))),
        listener: Some(probe.clone()),
        ..RigConfig::default()
    });
    probe.attach(&rig);
    go.send(()).expect("the probe waits");
    rig.v.send(importing(true)).expect("V runs");
    // On the rig's clock, a day past genesis, a block a second after genesis is stale.
    let day = rig.genesis.time() + 24 * 60 * 60;
    let b1 =
        mine(&rig.p, rig.genesis.hash(), rig.genesis.time() + 1, vec![coinbase(&cb_script(1, 0))]);
    let b2 = mine(&rig.p, b1.hash(), day - 60, vec![coinbase(&cb_script(2, 0))]);
    let stale = Mined {
        block: b1,
        height: 1,
    };
    let recent = Mined {
        block: b2,
        height: 2,
    };
    rig.accept(&stale);
    rig.headers(std::slice::from_ref(&recent));
    let (b1, b2) = (rig.id(&stale), rig.id(&recent));
    let fail = ChainCommand::TestOnV(Box::new(move |v| {
        let f = v.abort_at_fault::<()>(Err(Fatal::new("disk on fire"))).expect_err("stops");
        v.chain.connect_tip(b2).expect("b2 extends the tip");
        Err(f)
    }));
    rig.v.send(fail).expect("sent");
    rig.v.send(importing(false)).expect("the lane");
    assert_eq!(rig.v.initialize_current_block_tip().recv_timeout(SOON), Some(Ok(())));
    let call = InitCall {
        tip: Some(b2),
        ibd: false,
        dmn: Some(b2),
        published: Some(b1),
        height: Some(2),
    };
    assert_eq!(probe.calls(), [call]);
    rig.stop_on("disk on fire");
}

/// D72 (Opus r1 n-2): a V stopped on an SB4 contract failure no longer trusts its state
/// (BL-271), so the reaper drops init's command: the reply disconnects, the DMN tip stays and no
/// listener runs.
#[test]
fn the_reaper_drops_init_after_a_contract_failure() {
    let (tagged, (probe, _entered, go)) = (Arc::new(Tagged::default()), InitProbe::new());
    let (mut rig, _) = importing_rig(&tagged, &probe);
    go.send(()).expect("the probe waits");
    let broken = ChainCommand::TestOnV(Box::new(|_| {
        Err(Fatal::from(ChainError::Internal("broken at init")))
    }));
    rig.v.send(broken).expect("sent");
    rig.v.send(importing(false)).expect("the lane");
    let moves = rig.signals.dmn_moves();
    assert_eq!(
        rig.v.initialize_current_block_tip().recv_timeout(SOON),
        Some(Err(WaitError::Disconnected))
    );
    assert_eq!(rig.signals.dmn_moves(), moves);
    assert_eq!(probe.calls(), []);
    rig.stop_on("chain: internal: broken at init");
}

/// A listener holding a real `ChainViewHandle`, which holds V's lanes open, and noting its drop.
struct HoldsHandle {
    view: OnceLock<ChainViewHandle>,
    dropped: Arc<AtomicBool>,
}

impl ValidationInterface for HoldsHandle {}

impl Drop for HoldsHandle {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

/// D72 (Sol r1, Opus r1 n-1): the reaper holds V's signals weakly. A listener holding a
/// `ChainViewHandle` keeps V's lanes, and so the reaper, open; once V has stopped, the reaper has
/// answered init, and the signals thread and every other owner are gone, the listener is
/// dropped, so the lanes close and the reaper ends.
#[test]
fn the_reaper_lets_go_of_a_listener_holding_a_handle() {
    let dropped = Arc::new(AtomicBool::new(false));
    let holder = Arc::new(HoldsHandle {
        view: OnceLock::new(),
        dropped: dropped.clone(),
    });
    let mut rig = Rig::with(RigConfig {
        listener: Some(holder.clone()),
        ..RigConfig::default()
    });
    assert!(holder.view.set(rig.v.view_handle()).is_ok());
    drop(holder);
    rig.v.send(ChainCommand::TestOnV(Box::new(|_| Err(Fatal::new("disk on fire"))))).expect("sent");
    assert_eq!(rig.v.initialize_current_block_tip().recv_timeout(SOON), Some(Ok(())));
    rig.stop_on("disk on fire");
    drop(rig);
    let deadline = Instant::now() + SOON;
    while !dropped.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the listener is still held");
        std::thread::sleep(Duration::from_millis(10));
    }
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

/// The views a fault withheld cost no `view_seq`: once the reads answer again, the next accept's
/// views take the stamps they would have taken had no view been withheld, its last pairs its
/// tip's chain snapshot with the list at the DMN tip then, and only its tip is signalled.
#[test]
fn the_view_after_a_withheld_one_takes_the_next_stamp() {
    let (tagged, log) = (Arc::new(Tagged::default()), Arc::new(TipLog::default()));
    let rig = Rig::with(tagged_cfg(&tagged, &log));
    let h = rig.v.view_handle();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    rig.sync();
    let start = h.view().view_seq;
    rig.accept(&main[0]);
    rig.sync();
    log.updated.lock().expect("updated").clear();
    let before = h.view();
    // The stamps one accept's views take.
    let per_accept = before.view_seq - start;
    assert!(per_accept > 0);
    tagged.fail.store(true, Ordering::SeqCst);
    rig.accept(&main[1]);
    rig.sync();
    assert!(Arc::ptr_eq(&before, &h.view()), "a new view was stored");
    tagged.fail.store(false, Ordering::SeqCst);
    rig.accept(&main[2]);
    rig.sync();
    let c = rig.id(&main[2]);
    let view = h.view();
    assert_eq!(view.snap.tip.tip, c);
    assert_eq!(published(&h), (Some(c), main[2].hash()));
    assert_eq!(view.view_seq, before.view_seq + per_accept);
    assert_eq!(*log.updated.lock().expect("updated"), [c]);
}

/// What a test sink saw at each drain: `add`, the DMN tip then, and its `tip_list`'s block
/// (`None` for a drain given no list).
type Drain = (bool, Option<BlockId>, Option<BlockHash>);

/// A mempool, or none, that records each drain, and the block of each `removeForBlock` list.
struct Drains {
    has: bool,
    signals: ValidationSignals,
    seen: Arc<Mutex<Vec<Drain>>>,
    removed: Arc<Mutex<Vec<BlockHash>>>,
}

impl MempoolSink for Drains {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }

    fn remove_for_block_at(&mut self, _: &[dash_consensus::TxRef], _: u32, list: &Arc<MnList>) {
        self.removed.lock().expect("removed").push(*list.block_hash());
    }

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
        tip_list: Option<&Arc<MnList>>,
    ) {
        let drain = (tip_list.is_some(), self.signals.dmn_tip(), tip_list.map(|l| *l.block_hash()));
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
            removed: Arc::default(),
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

/// D60 at V's sites, with a mempool: `ConnectTip`'s `removeForBlock` gets the list at the DMN tip,
/// which inside a multi-block step is still the old tip's (D16); ABC's drain after a reorg step
/// moves the DMN tip to the new tip and hands the sink that list, and `InvalidateBlock`'s does
/// the same at the parent. Init's genesis connect reads Core's empty list, before any DMN tip.
/// (`Tagged`'s lists name their block; the sink's own signals are not V's, so its DMN-tip column
/// stays empty.)
// Core: validation.cpp:420-433, :3466, :3622, :3971
#[test]
fn v_drains_move_the_dmn_tip_and_connects_read_its_list() {
    let tagged = Arc::new(Tagged::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let removed = Arc::new(Mutex::new(Vec::new()));
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(TaggedConnector(tagged))),
        mempool: Some(Box::new(Drains {
            has: true,
            signals: ValidationSignals::default(),
            seen: seen.clone(),
            removed: removed.clone(),
        })),
        ..RigConfig::default()
    });
    fn take<T>(log: &Mutex<Vec<T>>) -> Vec<T> {
        std::mem::take(&mut *log.lock().expect("log"))
    }
    // Init's genesis connect comes before any DMN tip: Core's empty list.
    assert_eq!(take(&removed), [BlockHash::ZERO]);
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in &main {
        rig.accept(m);
    }
    assert_eq!(take(&removed), [rig.genesis.hash(), main[0].hash()]);
    assert_eq!(take(&seen), []);
    // The side chain overtakes in one step: its connects read a2's list, the drain b3's.
    rig.headers(&side);
    for m in &side {
        rig.submit(m);
    }
    rig.sync();
    assert_eq!(rig.v.view_handle().view().snap.tip.tip, rig.id(&side[2]));
    assert_eq!(take(&removed), [main[1].hash(); 3]);
    assert_eq!(take(&seen), [(true, None, Some(side[2].hash()))]);
    assert_eq!(rig.v.invalidate_block(side[2].hash()).recv(), Ok(Ok(())));
    let drains = take(&seen);
    assert_eq!(drains.first(), Some(&(true, None, Some(side[1].hash()))), "{drains:?}");
    rig.assert_no_abort();
}

/// D60 at `MarkConflictingBlock` (`v:4125`): the mark's adding drain moves the DMN tip to the tip
/// its disconnects left, so the sink reads that block's list, not the old tip's.
#[test]
fn a_chainlock_marks_drain_moves_the_dmn_tip() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rig = Rig::with(RigConfig {
        chainlocks: true,
        connector: Some(Box::new(TaggedConnector(Arc::new(Tagged::default())))),
        mempool: Some(Box::new(Drains {
            has: true,
            signals: ValidationSignals::default(),
            seen: seen.clone(),
            removed: Arc::new(Mutex::new(Vec::new())),
        })),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 2, 0);
    let side = branch(&rig.p, &rig.genesis, 1, 1);
    for m in main.iter().chain(&side) {
        rig.accept(m);
    }
    assert_eq!(rig.v.view_handle().view().snap.tip.tip, rig.id(&main[1]));
    intake(&rig);
    seen.lock().expect("seen").clear();
    // The lock on b1 marks a1, which disconnects a2 and a1: the drain reads genesis's list.
    assert!(record(&rig, &lock(&side[0])));
    settle(&rig);
    assert_eq!(rig.v.view_handle().view().snap.tip.tip, rig.id(&side[0]));
    let drains = std::mem::take(&mut *seen.lock().expect("seen"));
    assert_eq!(drains.first(), Some(&(true, None, Some(rig.genesis.hash()))), "{drains:?}");
    rig.assert_no_abort();
}
