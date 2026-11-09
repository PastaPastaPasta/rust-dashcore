//! The connector seam (CH3 §8.1 a2): `BlockChecked` per failure kind, the abort path's one
//! report (D52), V's context lent into an attempt, and G2's predicate over V's context.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::{Block, BlockId};
use dash_evo::Effect;
use dash_num::Hash256;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_store::ErrorKind;

use super::abc::Breaks;
use super::blocks::devnet;
use super::failure::{at_close, flag_at_close, pause};
use super::support::{Ev, Mined, Rig, RigConfig, batch, branch, child, regtest};
use crate::chain::actor::{Deferred, V, VExit};
use crate::chain::reply::{ActorReply, WaitError, reply};
use crate::chain::seams::HeldChainLocks;
use crate::chain::{
    AbortCause, AbortPath, AbortSink as _, AttemptOrigin, BlockConnector, BlockResult, BlockState,
    BlockStatusWriter, ChainCommand, ChainLockSig, ConnectCx, ConnectOutcome, DisconnectPool,
    DisconnectResult, Fatal, FlushBell, FlushHost, FlushSite, FlushStateMode, GuardScope,
    LoadFailure, NullConnector, ProposalChecks, RecordingAbort, ScratchCoins, ShutdownFlag as _,
    SiteDecision, StartError, VContext, ViewTarget, genesis_block,
};
use crate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use crate::connect::{
    AttemptId, ChainHelper as _, EvoAttempt as _, EvoHooks as _, INTERIM_REFUSAL, IbdHelper, NoEvo,
    NodeHelper, is_governance_height,
};
use crate::store::held::{HeldSink, fail_on_w, until};
use crate::store::sites::{W_PANIC_SITE, WRITE_SITE};
use crate::store::{Failure, GlobalFailStop, GuardedEngine, ProbeEngine, RecordingSink};

fn checked(rig: &Rig) -> Vec<Ev> {
    rig.rec.take().into_iter().filter(|e| matches!(e, Ev::Checked(..))).collect()
}

/// `ConnectTip` signals `BlockChecked` after `ConnectBlock` returns (`validation.cpp:3431-3432`),
/// so a system error (`state.Error`, the connector having called `AbortNode`) is checked; a
/// storage fault is not, as Core's `CCoinsViewErrorCatcher` aborts inside `ConnectBlock`
/// (`coins.cpp:356-380`, CH3 §2.1).
#[test]
fn a_storage_fault_emits_no_block_checked_but_a_system_error_does() {
    for abort in [false, true] {
        let p = regtest();
        let genesis = Mined {
            block: genesis_block(&p),
            height: 0,
        };
        let a1 = child(&p, &genesis, 0);
        let mut rig = Rig::with(RigConfig {
            connector: Some(Box::new(Breaks {
                at: a1.hash(),
                abort,
            })),
            ..RigConfig::default()
        });
        let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
        assert_eq!(r, Err(WaitError::Disconnected), "abort {abort}");
        let want = if abort {
            vec![]
        } else {
            vec![Ev::Checked(a1.hash(), BlockState::Error("disk on fire".into()))]
        };
        assert_eq!(checked(&rig), want, "abort {abort}");
        let exit = rig.stop();
        assert!(exit.fatal.is_some_and(|f| f.0.contains("disk on fire")), "abort {abort}");
    }
}

/// D52: each `abort_node` trips the latch if it has not tripped and logs, whoever tripped it.
#[test]
fn the_abort_path_trips_the_latch_and_logs_once_per_call() {
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    let path = AbortPath::new(sink.clone(), latch.clone());
    path.abort_node("Failed to write undo data", None);
    let first = latch.tripped().map(|f| (f.site, f.message.clone()));
    assert_eq!(first, Some(("chainstate validation", "Failed to write undo data".to_owned())));
    assert_eq!(sink.messages(), ["Failed to write undo data"]);

    path.abort_node("Failed to write block", Some("disk"));
    let kept = latch.tripped().map(|f| f.message.clone());
    assert_eq!(kept.as_deref(), Some("Failed to write undo data"), "the first failure is kept");
    assert_eq!(sink.messages(), ["Failed to write undo data", "Failed to write block"]);

    // A store that tripped the latch first left the report to the path.
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    assert!(latch.trip(Failure {
        kind: ErrorKind::Other,
        site: "blocks",
        message: "low space".into(),
    }));
    AbortPath::new(sink.clone(), latch.clone()).abort_node("Disk space is too low!", None);
    assert_eq!(latch.tripped().map(|f| f.site), Some("blocks"));
    assert_eq!(sink.messages(), ["Disk space is too low!"]);

    AbortPath::new(sink.clone(), latch).read_failed("coins");
    assert_eq!(sink.messages()[1], "Error reading from database: coins");
}

/// BL-683 D132: the recorder's `abort_fatal` trips the latch and reports as V's stop would. Its
/// own trip winning, a `GetForBlock` fault is reported; after another failure latched first, the
/// same fault is not, and an `AbortNode` of V's own still is.
#[test]
fn abort_fatal_reports_as_vs_stop_would() {
    const READ: &str = "GetForBlock: failed-getehfforblock-read";
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    let path = AbortPath::new(sink.clone(), latch.clone());
    path.abort_fatal(&Fatal::new(READ));
    let first = latch.tripped().map(|f| (f.site, f.message.clone()));
    assert_eq!(first, Some(("chainstate validation", READ.to_owned())));
    assert_eq!(sink.messages(), [READ]);

    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    assert!(latch.trip(Failure {
        kind: ErrorKind::Other,
        site: "blocks",
        message: "low space".into(),
    }));
    let path = AbortPath::new(sink.clone(), latch.clone());
    path.abort_fatal(&Fatal::new(READ));
    assert_eq!(latch.tripped().map(|f| f.site), Some("blocks"), "the first failure is kept");
    assert!(sink.messages().is_empty(), "{:?}", sink.messages());
    path.abort_fatal(&Fatal::abort_node("Failed to write block"));
    assert_eq!(sink.messages(), ["Failed to write block"]);
}

/// A connector that reports its system error through the abort path, as Core's `ConnectBlock`
/// calls `AbortNode` before it returns `state.Error`.
struct Reports {
    at: BlockHash,
    path: AbortPath,
}

impl BlockConnector for Reports {
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
        if block.hash() == self.at {
            self.path.abort_node("Failed to write undo data", None);
            return Ok(ConnectOutcome::SystemError("Failed to write undo data".into()));
        }
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
}

/// A flush side over the node's latch: its `abort` reports whether it was already tripped (L-3)
/// and, on a panic or an assert, trips it, as W's will (D62, D62a).
pub(super) struct Latched(pub(super) GlobalFailStop);

impl FlushSite for Latched {
    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn abort(&mut self, cause: AbortCause, _: &mut FlushHost<'_>) -> bool {
        match cause {
            AbortCause::Panic => !self.0.trip(Failure {
                kind: ErrorKind::Other,
                site: "validation",
                message: "V stopped".into(),
            }),
            AbortCause::AbortNode => self.0.tripped().is_some(),
        }
    }
}

/// What [`FailsAtExit`]'s stop returns.
const EXIT_FAILED: &str = "Failed to flush at V's exit";

/// [`Latched`] whose stop at V's exit fails with an `AbortNode`, and nothing else, so no bell
/// rings before V takes a `Shutdown`.
struct FailsAtExit(Latched);

impl FlushSite for FailsAtExit {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        self.0.at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.service(h)
    }

    fn stop(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Err(Fatal::new(EXIT_FAILED))
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        self.0.abort(cause, h)
    }
}

/// The latch's waiting read as `NodeFlush::admit` makes it (BL-678): its failure, once reported,
/// as `latched_fatal` converts it.
pub(super) fn admitted(latch: &GlobalFailStop) -> Result<(), Fatal> {
    latch.tripped().map_or(Ok(()), |f| Err(crate::flush::latched_fatal(f.clone())))
}

/// [`Latched`] that answers [`FlushSite::admit`] with the latch's waiting read, as `NodeFlush`
/// does (BL-678).
pub(super) struct Admitting(pub(super) Latched);

impl FlushSite for Admitting {
    fn at_site(&mut self, m: FlushStateMode, h: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        self.0.at_site(m, h)
    }

    fn after_step(&mut self, d: SiteDecision, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.after_step(d, h)
    }

    fn service(&mut self, h: &mut FlushHost<'_>) -> Result<(), Fatal> {
        self.0.service(h)
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        self.0.abort(cause, h)
    }

    fn admit(&self) -> Result<(), Fatal> {
        admitted(&self.0.0)
    }
}

/// [`Admitting`] over a [`WSide`]'s latch whose one armed admission (`armed`) is W's failure:
/// W's store failure, or with `panic` its panic, latches and W's report goes out (the sink's, as
/// [`w_fails`] sends it, joined), then with `ring` V's bell rings, and the admission answers
/// `Ok`. So W fails between that command's admission read and its first release point. The
/// bell's answer and `service` read the latch as an admission does.
pub(super) struct FailsAfterAdmitting {
    latch: GlobalFailStop,
    held: Arc<HeldSink>,
    armed: Arc<AtomicBool>,
    panic: bool,
    ring: bool,
    bell: Option<FlushBell>,
}

impl FailsAfterAdmitting {
    pub(super) fn new(
        w: &WSide,
        armed: &Arc<AtomicBool>,
        panic: bool,
        ring: bool,
    ) -> Box<FailsAfterAdmitting> {
        Box::new(FailsAfterAdmitting {
            latch: w.latch.clone(),
            held: w.held.clone(),
            armed: armed.clone(),
            panic,
            ring,
            bell: None,
        })
    }
}

impl FlushSite for FailsAfterAdmitting {
    fn attach(&mut self, bell: FlushBell) {
        self.bell = Some(bell);
    }

    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        admitted(&self.latch)
    }

    fn answer_due(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        admitted(&self.latch)
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        Latched(self.latch.clone()).abort(cause, h)
    }

    fn admit(&self) -> Result<(), Fatal> {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return admitted(&self.latch);
        }
        w_fails(&self.latch, &self.held, self.panic).join().expect("W reports");
        if self.ring
            && let Some(bell) = &self.bell
        {
            bell.ring();
        }
        Ok(())
    }
}

/// A flush site over a [`WSide`]'s latch that keeps V's bell for the test to ring. The bell's
/// answer, `service` and the admission read the latch as an admission does.
struct Belled {
    latch: GlobalFailStop,
    bell: Arc<Mutex<Option<FlushBell>>>,
}

impl FlushSite for Belled {
    fn attach(&mut self, bell: FlushBell) {
        *self.bell.lock().expect("the bell") = Some(bell);
    }

    fn at_site(&mut self, _: FlushStateMode, _: &mut FlushHost<'_>) -> Result<SiteDecision, Fatal> {
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        admitted(&self.latch)
    }

    fn abort(&mut self, cause: AbortCause, h: &mut FlushHost<'_>) -> bool {
        Latched(self.latch.clone()).abort(cause, h)
    }

    fn admit(&self) -> Result<(), Fatal> {
        admitted(&self.latch)
    }
}

/// The text W reports for [`WSide::store_fails`]' failure, which `latched_fatal` gives V too.
pub(super) const W_STORE_TEXT: &str = "System error while flushing: injected store failure";
/// W's panic, as its thread's end reports it.
pub(super) const W_PANIC_TEXT: &str = "the chainstate flush thread panicked";

/// W's side of a BL-678 light rig: the node's latch, the sink V and W report to, and W's report
/// held there while armed (CH3b D222).
pub(super) struct WSide {
    pub(super) latch: GlobalFailStop,
    pub(super) sink: Arc<RecordingAbort>,
    pub(super) held: Arc<HeldSink>,
}

impl WSide {
    pub(super) fn new() -> WSide {
        let sink = Arc::new(RecordingAbort::default());
        WSide {
            latch: GlobalFailStop::new(),
            held: Arc::new(HeldSink::new(sink.clone())),
            sink,
        }
    }

    /// `cfg` with an [`Admitting`] flush site over this latch, and this sink.
    pub(super) fn config(&self, cfg: RigConfig) -> RigConfig {
        RigConfig {
            flush: Some(Box::new(Admitting(Latched(self.latch.clone())))),
            abort: Some(self.sink.clone()),
            ..cfg
        }
    }

    /// W's store failure, latched on W's thread; its report goes to the held sink.
    pub(super) fn store_fails(&self) -> std::thread::JoinHandle<()> {
        w_fails(&self.latch, &self.held, false)
    }

    /// W's panic, latched as W's end latches it; its report goes to the held sink.
    pub(super) fn panics(&self) -> std::thread::JoinHandle<()> {
        w_fails(&self.latch, &self.held, true)
    }
}

/// W's store failure, or with `panic` its panic, latched on `latch` and reported to `held`.
pub(super) fn w_fails(
    latch: &GlobalFailStop,
    held: &Arc<HeldSink>,
    panic: bool,
) -> std::thread::JoinHandle<()> {
    let (site, message, report) = if panic {
        (W_PANIC_SITE, W_PANIC_TEXT, W_PANIC_TEXT)
    } else {
        (WRITE_SITE, "injected store failure", W_STORE_TEXT)
    };
    let failure = Failure {
        kind: if panic {
            ErrorKind::Other
        } else {
            ErrorKind::Io
        },
        site,
        message: message.into(),
    };
    fail_on_w(latch, held.clone(), failure, report.to_owned())
}

/// D52: a connector's `AbortNode` is the node's one report. V's stop finds the latch the
/// connector tripped and calls no sink.
#[test]
fn a_system_error_the_connector_reported_is_the_one_abort_node() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let latch = GlobalFailStop::new();
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(Reports {
            at: a1.hash(),
            path: AbortPath::new(sink.clone(), latch.clone()),
        })),
        flush: Some(Box::new(Latched(latch.clone()))),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("Failed to write undo data"));
    assert_eq!(sink.messages(), ["Failed to write undo data"]);
    assert_eq!(latch.tripped().map(|f| f.message.as_str()), Some("Failed to write undo data"));
}

/// A flush side whose `AcceptBlock` site (NONE, where only pruning happens) fails once, as a
/// prune's full flush can: like W, it reports through the node's sink and trips the latch
/// before V hears of it (L-3), and its `abort` is [`Latched`]'s. With `contract`, the failure
/// is instead an SB4 contract failure from the flush host's chain, which nothing reports first.
struct FailsAtAccept {
    latched: Latched,
    sink: Arc<RecordingAbort>,
    failed: bool,
    contract: bool,
    /// The failure trips the latch and is reported; else it is V's to report, the latch open.
    trips: bool,
    /// [`FlushSite::admit`] reads the latch, as `NodeFlush`'s does (BL-678).
    admitting: bool,
    /// Runs at the first PERIODIC site after the failure.
    at_periodic: Option<Box<dyn FnOnce() + Send>>,
}

impl FailsAtAccept {
    fn new(sink: &Arc<RecordingAbort>, contract: bool) -> Box<FailsAtAccept> {
        Box::new(FailsAtAccept {
            latched: Latched(GlobalFailStop::new()),
            sink: sink.clone(),
            failed: false,
            contract,
            trips: true,
            admitting: false,
            at_periodic: None,
        })
    }

    /// BL-678: admitting, its NONE failure tripping the latch (BL-535's shape) only if `trips`.
    fn admitting(sink: &Arc<RecordingAbort>, trips: bool) -> Box<FailsAtAccept> {
        Box::new(FailsAtAccept {
            trips,
            admitting: true,
            ..*FailsAtAccept::new(sink, false)
        })
    }
}

/// The contract failure [`FailsAtAccept`] returns with `contract`.
fn flush_contract_failure() -> Fatal {
    dash_chain::ChainError::Internal("a flush's chain read").into()
}

impl FlushSite for FailsAtAccept {
    fn at_site(
        &mut self,
        mode: FlushStateMode,
        _: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        if mode == FlushStateMode::Periodic
            && self.failed
            && let Some(f) = self.at_periodic.take()
        {
            f();
        }
        if mode != FlushStateMode::None || std::mem::replace(&mut self.failed, true) {
            return Ok(SiteDecision::nothing());
        }
        if self.contract {
            return Err(flush_contract_failure());
        }
        let msg = "Disk space is too low!";
        if self.trips {
            self.sink.abort_node(msg, None);
            self.latched.0.trip(Failure {
                kind: ErrorKind::NoSpace,
                site: "flush",
                message: msg.into(),
            });
        }
        Err(Fatal::new(msg))
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn abort(&mut self, cause: AbortCause, host: &mut FlushHost<'_>) -> bool {
        self.latched.abort(cause, host)
    }

    fn admit(&self) -> Result<(), Fatal> {
        if self.admitting {
            return admitted(&self.latched.0);
        }
        Ok(())
    }
}

/// BL-279: Core ignores `AcceptBlock`'s NONE flush. One that fails there has reported its
/// `AbortNode` and requested shutdown, but the block stays accepted: its ABC connects it and
/// stops at the shutdown check, `BlockChecked` sees it valid (`submitblock`'s null), the reply
/// is the accepted block's, and V's loop then ends on the abort, reported once.
// Core: validation.cpp:4860-4862, :3847, :4904-4912
#[test]
fn a_failed_flush_in_accept_block_still_connects_the_block() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(FailsAtAccept::new(&sink, false)),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv().expect("a reply");
    assert!(r.accepted && r.new_block && r.state.is_valid(), "{r:?}");
    assert!(rig.rec.take().contains(&Ev::Checked(a1.hash(), BlockState::Valid)));
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("Disk space is too low!"));
    let tip = exit.chain.private_tip().expect("a tip").0.id;
    assert_eq!(exit.chain.reader().entry(tip).expect("the tip").hash, a1.hash());
    assert_eq!(sink.messages(), ["Disk space is too low!"]);
}

/// Serves a `ProcessBlock` for `block` at the release point of a class C operation that only
/// releases, once V is inside it: its reply.
fn process_at_a_release_point(rig: &Rig, block: &Arc<Block>) -> ActorReply<BlockResult> {
    process_at_a_release_point_then(rig, block, |_| Ok(()))
}

/// BL-535: a `ProcessBlock` served at a release point whose NONE flush fails is carried past
/// (BL-279), and its ABC tail, deferred after V's stop cleared `deferred`, still runs once the
/// interrupted operation has ended, as Core's thread runs its ABC once the first one breaks:
/// one step, then the shutdown check. The block connects, `BlockChecked` sees it valid, the
/// reply is the accepted block's (`submitblock`'s null), and V's loop ends on the abort.
// Core: validation.cpp:3744, :3847, :4860-4862, :4905-4911; rpc/mining.cpp:534-535
#[test]
fn a_carried_accept_at_a_release_point_still_runs_its_abc_tail() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(FailsAtAccept::new(&sink, false)),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let r = process_at_a_release_point(&rig, &a1.block).recv().expect("a1's reply");
    assert!(r.accepted && r.new_block && r.state.is_valid(), "{r:?}");
    assert!(rig.rec.take().contains(&Ev::Checked(a1.hash(), BlockState::Valid)));
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("Disk space is too low!"));
    let tip = exit.chain.private_tip().expect("a tip").0.id;
    assert_eq!(exit.chain.reader().entry(tip).expect("the tip").hash, a1.hash());
    assert_eq!(sink.messages(), ["Disk space is too low!"]);
}

/// BL-535: a carried accept's ABC tail that fails on its own is the operation failing (D213):
/// V's loop ends on the tail's failure, not on the abort it carried, and the reply disconnects.
#[test]
fn a_carried_accept_s_failing_tail_is_the_stop() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(FailsAtAccept::new(&sink, false)),
        connector: Some(Box::new(Breaks {
            at: a1.hash(),
            abort: true,
        })),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let r = process_at_a_release_point(&rig, &a1.block).recv();
    assert!(matches!(r, Err(WaitError::Disconnected)), "{r:?}");
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("disk on fire"));
    let tip = exit.chain.private_tip().expect("a tip").0.id;
    assert_eq!(exit.chain.reader().entry(tip).expect("the tip").hash, genesis.block.hash());
}

/// BL-279: `AcceptBlock`'s NONE flush is carried past only as an `AbortNode`. An SB4 contract
/// failure there is V's own (D213), never carried past: V unwinds the block's command, its
/// reply disconnects, the block does not connect, and V's exit is that failure.
#[test]
fn a_contract_failure_in_accept_block_s_flush_unwinds_v() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(FailsAtAccept::new(&sink, true)),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert!(matches!(r, Err(WaitError::Disconnected)), "{r:?}");
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0), Some(flush_contract_failure().0));
    let tip = exit.chain.private_tip().expect("a tip").0.id;
    assert_eq!(exit.chain.reader().entry(tip).expect("the tip").hash, genesis.block.hash());
}

/// D213: of two `AbortNode`s V carries past in one operation, it keeps the first, whose report
/// came first, and its loop ends on that one.
#[test]
fn v_keeps_the_first_abort_it_carries_past() {
    let mut rig = Rig::new();
    rig.on_v(|v| {
        assert_eq!(v.carry_past(Err(Fatal::new("first"))), Ok(()));
        assert_eq!(v.carry_past(Err(Fatal::new("second"))), Ok(()));
        Ok(())
    });
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("first"));
}

/// BL-279 at the devnet step (D48): V carries past a failed NONE flush there too, but init hears
/// the step fail, with V's exit the abort, not a step that went well on a V that has stopped.
/// ND1's devnet step has no flush site that can fail, so only a rig gets here.
#[test]
fn a_failed_flush_in_the_devnet_step_fails_init() {
    let sink = Arc::new(RecordingAbort::default());
    let started = Rig::try_with(RigConfig {
        params: Some(devnet()),
        flush: Some(FailsAtAccept::new(&sink, false)),
        no_check_block_index: true,
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    assert!(
        matches!(
            started,
            Err(StartError::Load(LoadFailure::Failure {
                cause: None
            }))
        ),
        "init's error"
    );
    assert_eq!(sink.messages(), ["Disk space is too low!"]);
}

/// EV addendum §2.6: an attempt borrows V's context; `NoEvo` has no applier, so an attempt that
/// recorded an effect cannot finish.
#[test]
fn no_evo_lends_vs_context_and_refuses_an_effect() {
    let rig = Rig::new();
    let result = Arc::new(Mutex::new(None));
    let out = result.clone();
    rig.on_v(move |v| {
        let mut evo = NoEvo;
        let first = AttemptId::next();
        super::support::with_tip_cx(v, |vcx, cx| {
            let tip = AttemptOrigin::Tip;
            let att = evo.begin_attempt(&(), vcx, cx, first, tip, &ViewTarget::Active)?;
            assert_eq!(att.id(), first);
            evo.finish_attempt(att)?;

            let next = AttemptId::next();
            let mut att = evo.begin_attempt(&(), vcx, cx, next, tip, &ViewTarget::Active)?;
            att.sink().emit(Effect::RemoveConflictingIsLock {
                lock_hash: Hash256::from([7; 32]),
            });
            *out.lock().expect("out") = Some(evo.finish_attempt(att).is_err());
            Ok(())
        })
    });
    assert_eq!(*result.lock().expect("result"), Some(true));
    rig.assert_no_abort();
}

/// The first governance height at or above `from`, and the first non-governance one.
fn heights(p: &ChainSetup, from: u32) -> (u32, u32) {
    let gov = (from..from + 100_000).find(|&h| is_governance_height(p, h)).expect("a gov height");
    let plain = (from..).find(|&h| !is_governance_height(p, h)).expect("a plain height");
    (gov, plain)
}

/// G2 (CH3 §2.17): over V's real context, the interim refusal holds only on a live network,
/// out of IBD, at a governance height that no ChainLock covers.
#[test]
fn the_interim_refusal_is_g2s_predicate_over_vs_context() {
    let rig = Rig::new();
    let p = rig.p.clone();
    let (gov, plain) = heights(&p, 1);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    rig.on_v(move |v| {
        let deployments = v.deployments.view();
        let chain = RefCell::new(&mut v.chain);
        let best_header = chain.borrow().best_header();
        let cases = [
            (GuardScope::Live, false, -1, gov),
            (GuardScope::Live, false, -1, plain),
            (GuardScope::Declared, false, -1, gov),
            (GuardScope::Live, true, -1, gov),
            (GuardScope::Live, false, i32::try_from(gov).expect("height"), gov),
            (GuardScope::Live, false, i32::try_from(gov).expect("height") - 1, gov),
        ];
        for (guard, is_ibd, best_cl, h) in cases {
            // A best lock at `best_cl`; −1 is none.
            let best = (best_cl >= 0).then(|| {
                Arc::new(ChainLockSig {
                    height: best_cl,
                    ..ChainLockSig::null()
                })
            });
            let chainlocks = HeldChainLocks::new(&chain, best);
            let cx = ConnectCx {
                params: &v.params,
                reader: &v.reader,
                deployments: &deployments,
                best_header,
                min_chain_work: v.reader.min_chain_work(),
                is_ibd,
                chainlocks: &chainlocks,
                guard,
                origin: AttemptOrigin::Tip,
            };
            out.lock().expect("out").push(NodeHelper.interim_refusal(&cx, h));
        }
        Ok(())
    });
    let r = Some(INTERIM_REFUSAL);
    assert_eq!(*seen.lock().expect("seen"), [r, None, None, None, None, r]);
    rig.assert_no_abort();
}

/// CH7b-3 C2: `NodeHelper::cl_best` is V's best lock as the call's view holds it, at the view's
/// best height; the L2 replay helper answers Core's null sig.
#[test]
fn the_node_helper_answers_vs_best_chainlock() {
    let rig = Rig::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let out = seen.clone();
    let lock = Arc::new(ChainLockSig {
        height: 7,
        block_hash: BlockHash::from([1; 32]),
        sig: [2; 96],
    });
    let held = lock.clone();
    rig.on_v(move |v| {
        let deployments = v.deployments.view();
        let chain = RefCell::new(&mut v.chain);
        let best_header = chain.borrow().best_header();
        for best in [None, Some(held)] {
            let chainlocks = HeldChainLocks::new(&chain, best);
            let cx = ConnectCx {
                params: &v.params,
                reader: &v.reader,
                deployments: &deployments,
                best_header,
                min_chain_work: v.reader.min_chain_work(),
                is_ibd: false,
                chainlocks: &chainlocks,
                guard: GuardScope::Live,
                origin: AttemptOrigin::Tip,
            };
            let node = NodeHelper.cl_best(&cx).expect("no fault");
            let ibd = IbdHelper.cl_best(&cx).expect("no fault");
            out.lock().expect("out").push((node, cx.chainlocks.best_height(), ibd));
        }
        Ok(())
    });
    assert_eq!(*seen.lock().expect("seen"), [(None, -1, None), (Some(lock), 7, None)]);
    rig.assert_no_abort();
}

/// The guard is live on mainnet and testnet, declared on the local networks (CH3 §2.17).
#[test]
fn the_guard_scope_follows_the_network() {
    use dash_params::{ChainParamsBuilder, Network, Release};
    for (kind, want) in [
        (Network::Main, GuardScope::Live),
        (Network::Testnet3, GuardScope::Live),
        (Network::Regtest, GuardScope::Declared),
    ] {
        let p = ChainParamsBuilder::new(kind, Release::V24_0).build().expect("Core's parameters");
        assert_eq!(GuardScope::of(&p), want, "{kind:?}");
    }
}

/// No mempool, no pool: `take` and `DynamicMemoryUsage` of the empty pool.
#[test]
fn the_unit_disconnect_pool_is_empty() {
    let mut pool: Box<dyn DisconnectPool> = Box::new(());
    assert!(pool.take().is_empty());
    assert_eq!(pool.dynamic_memory_usage(), 0);
}

/// What V handed one connector call: the context's tags, the memo and the target.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Connect {
        hash: BlockHash,
        origin: AttemptOrigin,
        guard: GuardScope,
        is_ibd: bool,
        checked: bool,
        active: bool,
    },
    Disconnect {
        hash: BlockHash,
        origin: AttemptOrigin,
        guard: GuardScope,
        active: bool,
    },
}

/// Records each call, then connects as [`NullConnector`].
struct Records(Arc<Mutex<Vec<Call>>>);

impl BlockConnector for Records {
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
        self.0.lock().expect("calls").push(Call::Connect {
            hash: block.hash(),
            origin: cx.origin,
            guard: cx.guard,
            is_ibd: cx.is_ibd,
            checked,
            active: matches!(target, ViewTarget::Active),
        });
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
        self.0.lock().expect("calls").push(Call::Disconnect {
            hash: block.hash(),
            origin: cx.origin,
            guard: cx.guard,
            active: matches!(target, ViewTarget::Active),
        });
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
}

/// The tip path's calls (CH3 D3, D36, D42): a reorg disconnects A1 and connects B1, read from
/// disk, then B2, `ProcessNewBlock`'s own object. Each is a tip attempt into the active view,
/// with the rig's injected guard and V's IBD; only B2 carries the `fChecked` memo.
#[test]
fn the_tip_path_hands_the_connector_its_context_memo_and_target() {
    // A day past genesis is Core's IBD on regtest (a 6 h tip age); ten minutes is not.
    for (after_genesis, ibd) in [(None, true), (Some(600), false)] {
        let p = regtest();
        let genesis = Mined {
            block: genesis_block(&p),
            height: 0,
        };
        let a1 = child(&p, &genesis, 0);
        let side = branch(&p, &genesis, 2, 1);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut rig = Rig::with(RigConfig {
            connector: Some(Box::new(Records(calls.clone()))),
            guard: Some(GuardScope::Live),
            after_genesis,
            ..RigConfig::default()
        });
        rig.accept(&a1);
        rig.accept(&side[0]);
        calls.lock().expect("calls").clear();
        rig.accept(&side[1]);

        let (origin, guard, active) = (AttemptOrigin::Tip, GuardScope::Live, true);
        let connect = |m: &Mined, checked| Call::Connect {
            hash: m.hash(),
            origin,
            guard,
            is_ibd: ibd,
            checked,
            active,
        };
        let want = [
            Call::Disconnect {
                hash: a1.hash(),
                origin,
                guard,
                active,
            },
            connect(&side[0], false),
            connect(&side[1], true),
        ];
        assert_eq!(*calls.lock().expect("calls"), want, "after genesis {after_genesis:?}");
        rig.assert_no_abort();
        assert!(rig.stop().fatal.is_none());
    }
}

/// A scratch view's coins give the connector an overlay over its own engine only (D36).
#[test]
fn scratch_coins_give_an_overlay_only_for_their_engine() {
    let db = Arc::new(GuardedEngine::probe(Arc::new(RecordingSink::default())));
    let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
    let mut layer: CoinsCache<ProbeEngine> = CoinsCache::new(db, budget, Salt::fixed(7), None);
    assert!(ScratchCoins::new(&mut layer).overlay::<ProbeEngine>().is_some());
    assert!(ScratchCoins::new(&mut layer).overlay::<dash_kv::MemoryEngine>().is_none());
}

/// How long a BL-678 test waits for what must happen at once.
const PROMPT: std::time::Duration = std::time::Duration::from_secs(10);
/// A generous bound on a wait a test needs to end.
const SOON: std::time::Duration = std::time::Duration::from_secs(120);

/// V's tip at its exit.
pub(super) fn exit_tip(exit: &crate::chain::actor::VExit) -> Option<BlockHash> {
    exit.chain.private_tip().map(|(t, _)| t.hash)
}

/// BL-678, path 2: deferred work V restarts after W's failure is admitted first. A
/// `reconsiderblock`'s scope clears a1's failure and defers its ABC tail (`Deferred::RpcTail`),
/// then W's store failure latches with its report held: V waits for that report before the tail,
/// then drops it. No ABC runs (a1 stays unconnected), the tail's reply disconnects, and V stops
/// on W's failure, reported once.
// Core: validation.cpp:3128; rpc/blockchain.cpp:2059-2063
#[test]
fn deferred_work_after_ws_failure_runs_nothing() {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig::default()));
    let a1 = child(&rig.p, &rig.genesis, 0);
    rig.accept(&a1);
    assert_eq!(rig.v.invalidate_block(a1.hash()).recv(), Ok(Ok(())));
    let (id, v) = (rig.id(&a1), rig.v_thread());
    let release = w.held.arm();
    let (to_tail, tail) = reply();
    let (latch, held) = (w.latch.clone(), w.held.clone());
    let scope = ChainCommand::TestUnderMain(Box::new(move |vv| {
        vv.reconsider_block(id, false)?;
        vv.defer(Deferred::RpcTail {
            reply: to_tail,
        });
        drop(w_fails(&latch, &held, false));
        Ok(())
    }));
    rig.v.send(scope).expect("V runs");
    let parked = w.held.parked(&w.latch, v);
    let early = (tail.try_recv(), rig.shutdown.shutdown_requested());
    release.release();
    assert!(parked.is_some(), "V waits for W's report before the deferred tail");
    assert_eq!(early, (None, false), "nothing before W's report");
    assert_eq!(tail.recv(), Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert_eq!(exit_tip(&exit), Some(rig.genesis.hash()), "no ABC ran");
    assert_eq!(w.sink.messages(), [W_STORE_TEXT]);
}

/// BL-678, paths 3h and 3l: at a release point after W's failure, V admits neither a held
/// command nor a lane one. The refusal is V's own: `release_point` returns W's failure itself,
/// with nothing carried past (`interrupted` stays empty) and V's `stop` not set by a carry; the
/// command's reply disconnects and no header tip moves.
fn a_release_point_after_ws_failure_serves_nothing(lane: bool) {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig::default()));
    let a1 = child(&rig.p, &rig.genesis, 0);
    let v = rig.v_thread();
    let release = w.held.arm();
    let (to_held, held) = reply();
    let staged = (!lane).then(|| ChainCommand::ProcessHeaders {
        batch: batch(std::slice::from_ref(&a1)),
        from: None,
        reply: to_held,
    });
    let (inside, entered) = std::sync::mpsc::channel();
    let (go, queued) = std::sync::mpsc::channel();
    let (seen, at_release) = std::sync::mpsc::channel();
    let operation = ChainCommand::TestUnderV(Box::new(move |vv| {
        if let Some(cmd) = staged {
            vv.stage_held(cmd);
        }
        inside.send(()).expect("the test waits");
        queued.recv().expect("the test trips W");
        let r = vv.release_point();
        let uncarried = vv.interrupted.is_none();
        seen.send((r.clone().map_err(|f| f.0), uncarried, vv.stop)).expect("the test waits");
        r
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv().expect("V is inside the operation");
    let queued = lane.then(|| rig.v.process_headers(batch(std::slice::from_ref(&a1)), None));
    drop(w.store_fails());
    go.send(()).expect("V waits");
    let parked = w.held.parked(&w.latch, v);
    let early = rig.shutdown.shutdown_requested();
    release.release();
    assert!(parked.is_some(), "lane {lane}: V waits for W's report at the admission");
    assert!(!early, "lane {lane}: the node's flag before W's report");
    let (r, uncarried, stopped) = at_release.recv_timeout(PROMPT).expect("the release point");
    assert_eq!(r, Err(W_STORE_TEXT.to_owned()), "lane {lane}");
    assert!(uncarried, "lane {lane}: nothing carried past");
    assert!(!stopped, "lane {lane}: no carry stopped V at the release point");
    match queued {
        Some(r) => assert_eq!(r.recv().map(|_| ()), Err(WaitError::Disconnected)),
        None => assert_eq!(held.recv().map(|_| ()), Err(WaitError::Disconnected)),
    }
    let exit = rig.stop();
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some(W_STORE_TEXT), "lane {lane}");
    assert!(rig.v.reader().lookup(&a1.hash()).is_none(), "lane {lane}: no header accepted");
    assert!(!rig.rec.take().iter().any(|e| matches!(e, Ev::HeaderTip(..))), "lane {lane}");
    assert_eq!(w.sink.messages(), [W_STORE_TEXT], "lane {lane}");
}

#[test]
fn a_held_command_at_a_release_point_after_ws_failure_is_refused() {
    a_release_point_after_ws_failure_serves_nothing(false);
}

#[test]
fn a_lane_command_at_a_release_point_after_ws_failure_is_refused() {
    a_release_point_after_ws_failure_serves_nothing(true);
}

/// [`process_at_a_release_point`], then `then` on V once the release point returns.
fn process_at_a_release_point_then(
    rig: &Rig,
    block: &Arc<Block>,
    then: impl FnOnce(&mut V) -> Result<(), Fatal> + Send + 'static,
) -> ActorReply<BlockResult> {
    let (inside, entered) = std::sync::mpsc::channel();
    let (go, queued) = std::sync::mpsc::channel();
    let operation = ChainCommand::TestUnderV(Box::new(move |v| {
        inside.send(()).expect("the test waits");
        queued.recv().expect("the test queues the block");
        v.release_point()?;
        then(v)
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv().expect("V is inside the operation");
    let reply = rig.v.process_block(block.clone(), true, None, true);
    go.send(()).expect("V waits");
    reply
}

/// BL-678, path 5 (C3): a carried accept's ABC tail is admitted when the latch was still open
/// at the carried stop. The accept's NONE flush fails with the latch open (V reports it, BL-279
/// carries it past), then W's store failure latches at the interrupted operation's closing
/// PERIODIC site, its report held. V waits for that report before the tail, which then never
/// begins: a1 stays unconnected, its reply disconnects, and V stops on W's failure, after the
/// carried abort's report.
// Core: validation.cpp:3749, :3852, :4911-4914; shutdown.cpp:27-38
#[test]
fn a_carried_tail_after_a_later_w_failure_is_refused() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let held = Arc::new(HeldSink::new(sink.clone()));
    let mut site = FailsAtAccept::admitting(&sink, false);
    let (latch, w) = (site.latched.0.clone(), held.clone());
    site.at_periodic = Some(Box::new(move || drop(w_fails(&latch, &w, false))));
    let latch = site.latched.0.clone();
    let mut rig = Rig::with(RigConfig {
        flush: Some(site),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let v = rig.v_thread();
    let release = held.arm();
    let reply = process_at_a_release_point_then(&rig, &a1.block, |vv| {
        vv.flush_now(FlushStateMode::Periodic)
    });
    let parked = held.parked(&latch, v);
    let early = rig.rec.take();
    release.release();
    assert!(parked.is_some(), "V waits for W's report before the tail");
    assert!(!early.iter().any(|e| matches!(e, Ev::Checked(..))), "no tail began: {early:?}");
    assert_eq!(reply.recv().map(|_| ()), Err(WaitError::Disconnected));
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT));
    assert_eq!(exit_tip(&exit), Some(genesis.block.hash()), "a1 is not connected");
    assert!(!rig.rec.take().iter().any(|e| matches!(e, Ev::Checked(..))));
    assert_eq!(sink.messages(), ["Disk space is too low!", W_STORE_TEXT]);
}

/// BL-678, C3's control: when the carried abort tripped the latch itself (BL-535's shape), the
/// tail runs unadmitted, as BL-535 designs: a1 connects, `BlockChecked` sees it valid, the reply
/// is the accepted block's, and the node has one report.
// Core: validation.cpp:3749, :3852, :4911-4916
#[test]
fn a_carried_tail_after_its_own_trip_still_runs() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let sink = Arc::new(RecordingAbort::default());
    let mut rig = Rig::with(RigConfig {
        flush: Some(FailsAtAccept::admitting(&sink, true)),
        abort: Some(sink.clone()),
        ..RigConfig::default()
    });
    let r = process_at_a_release_point(&rig, &a1.block).recv().expect("a1's reply");
    assert!(r.accepted && r.new_block && r.state.is_valid(), "{r:?}");
    assert!(rig.rec.take().contains(&Ev::Checked(a1.hash(), BlockState::Valid)));
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some("Disk space is too low!"));
    assert_eq!(exit_tip(&exit), Some(a1.hash()));
    assert_eq!(sink.messages(), ["Disk space is too low!"]);
}

/// BL-678 (C4): the command an admission refuses goes to V's reaper as if V had never taken it.
/// (a) A refused `SetLoading` is applied once V has stopped (CH3b D220). (b) A refused
/// `InitializeCurrentBlockTip` is served after an `AbortNode`-class stop (CH7-notes D72), and
/// (c) dropped after an assert. (d) A refused `ActivateBestChain` with no work is answered `Ok`
/// (D240), and (e) so is one queued behind a refused `SetLoading`, which the reaper applies
/// first: W's report, then the node's flag, then the reply. Each is the refused command: V waits
/// for W's report at its admission, and before that report the command has not run.
// Core: validation.cpp:3789-3791, :3823; node/blockstorage.cpp:886-890, :952-953
fn the_reaper_takes_the_refused_command(run: char) {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig::default()));
    let loading = |importing| ChainCommand::SetLoading {
        importing: Some(importing),
        reindex: None,
    };
    rig.v.send(loading(true)).expect("V runs");
    if run != 'a' {
        rig.v.send(loading(false)).expect("V runs");
    }
    rig.v.query(|_| ()).recv().expect("V served them");
    let importing = rig.v.loading().importing();
    assert_eq!(importing, run == 'a', "run {run}");
    let v = rig.v_thread();
    let release = w.held.arm();
    drop(if run == 'c' {
        w.panics()
    } else {
        w.store_fails()
    });
    if matches!(run, 'd' | 'e') {
        if run == 'e' {
            rig.v.send(loading(true)).expect("V runs");
        }
        let abc = rig.v.activate_best_chain();
        let parked = w.held.parked(&w.latch, v);
        let early = (rig.v.loading().importing(), abc.try_recv().is_none());
        let abc = at_close(abc, &rig.shutdown, rig.v.loading(), &w.sink);
        release.release();
        assert!(parked.is_some(), "run {run}: V waits for W's report at the admission");
        assert_eq!(early, (importing, true), "run {run}: the command ran before W's report");
        assert_eq!(abc(), (Ok(Ok(())), Some((true, run == 'e', 1))), "run {run}");
        assert_eq!(w.sink.messages(), [W_STORE_TEXT], "run {run}");
        let exit = rig.stop();
        assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(W_STORE_TEXT), "run {run}");
        assert_eq!(exit_tip(&exit), Some(rig.genesis.block.hash()), "run {run}");
        return;
    }
    let init = if run == 'a' {
        rig.v.send(loading(false)).expect("V runs");
        None
    } else {
        Some(rig.v.initialize_current_block_tip())
    };
    let parked = w.held.parked(&w.latch, v);
    let early = (rig.v.loading().importing(), init.as_ref().and_then(|i| i.try_recv()));
    release.release();
    assert!(parked.is_some(), "run {run}: V waits for W's report at the admission");
    assert_eq!(early, (importing, None), "run {run}: the command ran before W's report");
    match init {
        None => assert!(until(|| !rig.v.loading().any()), "run {run}: the reaper applied it"),
        Some(init) if run == 'b' => assert_eq!(init.recv(), Ok(()), "run {run}"),
        Some(init) => assert_eq!(init.recv(), Err(WaitError::Disconnected), "run {run}"),
    }
    let exit = rig.stop();
    let fatal = exit.fatal.expect("V stopped on W's failure");
    assert_eq!(fatal.is_abort_node(), run != 'c', "run {run}");
}

#[test]
fn the_reaper_applies_a_refused_set_loading() {
    the_reaper_takes_the_refused_command('a');
}

#[test]
fn the_reaper_serves_a_refused_initialize_current_block_tip_after_an_abort_node() {
    the_reaper_takes_the_refused_command('b');
}

#[test]
fn the_reaper_drops_a_refused_initialize_current_block_tip_after_an_assert() {
    the_reaper_takes_the_refused_command('c');
}

#[test]
fn the_reaper_answers_a_refused_abc_ok() {
    the_reaper_takes_the_refused_command('d');
}

#[test]
fn the_reaper_answers_an_abc_queued_behind_a_refused_set_loading_ok() {
    the_reaper_takes_the_refused_command('e');
}

/// BL-678 (C5): `Shutdown` is not admitted. After W's store failure, its report held, V takes a
/// `Shutdown` without waiting for that report and stops as on any shutdown (no `Fatal`).
#[test]
fn a_shutdown_after_ws_failure_is_not_held_for_its_report() {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig::default()));
    let release = w.held.arm();
    drop(w.store_fails());
    let join = rig.join.take().expect("running");
    rig.v.shutdown();
    let (tx, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let exit = join.join().expect("an unmarked joiner").expect("V does not panic");
        let _ = tx.send(exit.fatal.map(|f| f.0));
    });
    let stopped = stopped.recv_timeout(PROMPT);
    release.release();
    assert_eq!(stopped, Ok(None), "V stopped on the Shutdown before W's report");
}

/// A startup ABC's reply and the node's flag as it closed ([`flag_at_close`]).
type AbcAtClose = (Result<Result<(), String>, WaitError>, Option<bool>);

/// A `TestUnderV` that fails with `stop`, an `ActivateBestChain` queued behind it in the lane,
/// the latch open and nothing refused: the ABC's reply and the node's flag as it closed, V's exit
/// and its W side.
fn queued_abc_after(stop: Fatal) -> (AbcAtClose, VExit, WSide) {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig::default()));
    let (inside, entered) = std::sync::mpsc::channel();
    let (go, queued) = std::sync::mpsc::channel::<()>();
    let operation = ChainCommand::TestUnderV(Box::new(move |_| {
        inside.send(()).expect("the test waits");
        queued.recv().expect("the test queues the ABC");
        Err(stop)
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv_timeout(SOON).expect("V is inside the operation");
    let closed = flag_at_close(rig.v.activate_best_chain(), &rig.shutdown);
    go.send(()).expect("V waits");
    let abc = closed();
    (abc, rig.stop(), w)
}

/// D241: V's stop on its own `AbortNode`, with the latch open and nothing refused, answers an
/// `ActivateBestChain` queued behind it `Ok`, after the node's flag (D55): it never ran, as
/// Core's `ThreadImport` ABC after another thread's `AbortNode`, with no work, returns `true`.
// Core: node/blockstorage.cpp:950-962; validation.cpp:3789-3791, :3823
#[test]
fn a_stop_on_vs_own_abort_node_answers_a_queued_abc_ok() {
    let (abc, exit, w) = queued_abc_after(Fatal::reported("V's own failure"));
    assert_eq!(abc, (Ok(Ok(())), Some(true)));
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), Some("V's own failure"));
    assert!(w.latch.tripped_now().is_none(), "the latch stayed open");
}

/// D241's assert control: after V's stop on one of Core's asserts, an `ActivateBestChain` queued
/// behind it is dropped (`AfterStop::Drop`): Core's process has ended.
// Core: util/check.h:49-51; node/blockstorage.cpp:950-962
#[test]
fn a_stop_on_vs_assert_drops_a_queued_abc() {
    let (abc, exit, _) = queued_abc_after(Fatal::assert("V's assert"));
    assert_eq!(abc, (Err(WaitError::Disconnected), Some(true)));
    let fatal = exit.fatal.expect("V stopped on its assert");
    assert_eq!((fatal.0.as_str(), fatal.is_abort_node()), ("V's assert", false));
}

/// D241's contract control: after an SB4 contract failure, whose state V no longer trusts
/// (BL-271), an `ActivateBestChain` queued behind it is dropped (`AfterStop::Drop`).
// Core: node/blockstorage.cpp:950-962; validation.cpp:3789-3791, :3823
#[test]
fn a_stop_on_a_contract_failure_drops_a_queued_abc() {
    let (abc, exit, _) = queued_abc_after(flush_contract_failure());
    assert_eq!(abc, (Err(WaitError::Disconnected), Some(true)));
    let fatal = exit.fatal.expect("V stopped on the contract failure");
    assert!(fatal.is_contract(), "{fatal:?}");
}

/// D241 (D237's bell race, the bell first): W's failure that V takes at its idle bell, not at an
/// admission, stops V with nothing refused; an `ActivateBestChain` queued meanwhile is answered
/// `Ok` as after a refusal, inside `fImporting`, after W's one report and the node's flag. V,
/// importing, takes the only ready operation, the bell, and waits there for W's held report;
/// only then is the ABC sent. With `panic`, W's failure is its panic, an assert: the reply is
/// dropped.
// Core: node/blockstorage.cpp:950-962; validation.cpp:3789-3791, :3823
fn a_queued_abc_after_a_stop_at_ws_bell(panic: bool) {
    let w = WSide::new();
    let bell = Arc::new(Mutex::new(None));
    let mut rig = Rig::with(RigConfig {
        flush: Some(Box::new(Belled {
            latch: w.latch.clone(),
            bell: bell.clone(),
        })),
        abort: Some(w.sink.clone()),
        ..RigConfig::default()
    });
    let loading = ChainCommand::SetLoading {
        importing: Some(true),
        reindex: None,
    };
    rig.v.send(loading).expect("V runs");
    let served = rig.v.query(|_| ()).recv_timeout(SOON);
    assert_eq!(served, Some(Ok(())), "panic {panic}: V served the SetLoading");
    assert!(rig.v.loading().importing(), "panic {panic}");
    let v = rig.v_thread();
    let release = w.held.arm();
    drop(if panic {
        w.panics()
    } else {
        w.store_fails()
    });
    bell.lock().expect("the bell").as_ref().expect("V attached its bell").ring();
    let parked = w.held.parked(&w.latch, v);
    let abc = at_close(rig.v.activate_best_chain(), &rig.shutdown, rig.v.loading(), &w.sink);
    release.release();
    assert!(parked.is_some(), "panic {panic}: V waits for W's report at its bell");
    let (want, text) = if panic {
        (Err(WaitError::Disconnected), W_PANIC_TEXT)
    } else {
        (Ok(Ok(())), W_STORE_TEXT)
    };
    assert_eq!(abc(), (want, Some((true, true, 1))), "panic {panic}");
    assert_eq!(w.sink.messages(), [text], "panic {panic}: W's report only");
    let exit = rig.stop();
    let fatal = exit.fatal.clone().expect("V stopped on W's failure");
    assert_eq!(fatal.0, text, "panic {panic}");
    assert_eq!(fatal.is_abort_node(), !panic, "panic {panic}");
    assert!(!fatal.is_own(), "panic {panic}: V stopped at its bell, with nothing refused");
    assert_eq!(exit_tip(&exit), Some(rig.genesis.block.hash()), "panic {panic}");
}

#[test]
fn a_stop_at_ws_bell_answers_a_queued_abc_ok() {
    a_queued_abc_after_a_stop_at_ws_bell(false);
}

#[test]
fn a_stop_at_ws_bell_after_ws_panic_drops_a_queued_abc() {
    a_queued_abc_after_a_stop_at_ws_bell(true);
}

/// D241's shutdown controls: V's loop ends on a plain `Shutdown` with an `ActivateBestChain`
/// queued behind it, and the reaper drops it (`aborted` is `false`), where Core's ABC would
/// return `true`; no production route queues startup's ABC behind a `Shutdown`. With
/// `exit_fails`, the flush side's stop at V's exit then fails with an `AbortNode`: V reports it
/// and sets the node's flag, and the ABC is still dropped, as `aborted` was read from the loop's
/// end, before that stop.
// Core: node/blockstorage.cpp:950-962; validation.cpp:3789-3791, :3823
fn a_queued_abc_after_a_plain_shutdown(exit_fails: bool) {
    let w = WSide::new();
    let latched = Latched(w.latch.clone());
    let flush: Box<dyn FlushSite> = if exit_fails {
        Box::new(FailsAtExit(latched))
    } else {
        Box::new(latched)
    };
    let mut rig = Rig::with(RigConfig {
        flush: Some(flush),
        abort: Some(w.sink.clone()),
        ..RigConfig::default()
    });
    let (inside, entered) = std::sync::mpsc::channel();
    let (go, queued) = std::sync::mpsc::channel::<()>();
    let operation = ChainCommand::TestUnderV(Box::new(move |_| {
        inside.send(()).expect("the test waits");
        queued.recv().expect("the test queues the Shutdown and the ABC");
        Ok(())
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv_timeout(SOON).expect("V is inside the operation");
    rig.v.shutdown();
    let abc = flag_at_close(rig.v.activate_best_chain(), &rig.shutdown);
    go.send(()).expect("V waits");
    let what = format!("exit fails {exit_fails}");
    assert_eq!(abc(), (Err(WaitError::Disconnected), Some(exit_fails)), "{what}");
    let exit = rig.stop();
    let reported = exit_fails.then_some(EXIT_FAILED);
    assert_eq!(exit.fatal.map(|f| f.0).as_deref(), reported, "{what}");
    assert_eq!(w.sink.messages(), reported.as_slice(), "{what}");
}

#[test]
fn a_plain_shutdown_drops_a_queued_abc() {
    a_queued_abc_after_a_plain_shutdown(false);
}

#[test]
fn a_plain_shutdown_whose_exit_flush_fails_drops_a_queued_abc() {
    a_queued_abc_after_a_plain_shutdown(true);
}

/// D240 (E4-B): an admitted `ActivateBestChain` that meets W's failure at its first release
/// point, before any search or step, answers `Ok`, after W's report and the node's flag (D222,
/// D55). [`FailsAfterAdmitting`] latches W's failure at the ABC's admission, with its report
/// joined; then the ABC's first release point meets it at the bell, or (`lane`) at the admission
/// of a query queued behind the ABC, which it refuses. With `work`, a1 is invalidated first, so
/// b1, which ties it, is due; the ABC takes no step towards it, and nothing resumes. The staging
/// runs in one paused operation: the invalidation, then the arming, then the test queues the ABC
/// (and the query), so no release point of the invalidation takes them. The default clock keeps
/// the rig in IBD, so the invalidation defers no ChainLock task that would take the armed
/// admission. With `panic`, W's failure is its panic, an assert: the reply is dropped.
// Core: validation.cpp:3771, :3789-3791, :3823; node/blockstorage.cpp:952-953
fn an_abc_meeting_ws_failure_at_its_first_release_point(lane: bool, work: bool, panic: bool) {
    let what = format!("lane {lane}, work {work}, panic {panic}");
    let w = WSide::new();
    let armed = Arc::new(AtomicBool::new(false));
    let mut rig = Rig::with(RigConfig {
        flush: Some(FailsAfterAdmitting::new(&w, &armed, panic, !lane)),
        abort: Some(w.sink.clone()),
        ..RigConfig::default()
    });
    let genesis = rig.genesis.clone();
    let (a1, b1) = (child(&rig.p, &genesis, 0), child(&rig.p, &genesis, 1));
    let ids = work.then(|| {
        rig.accept(&a1);
        rig.accept(&b1);
        (rig.id(&a1), rig.id(&b1))
    });
    let a1_id = ids.map(|(a1, _)| a1);
    rig.rec.take();
    let (staged, at_staged) = std::sync::mpsc::channel();
    let (go, queued) = std::sync::mpsc::channel::<()>();
    let stage = ChainCommand::TestUnderV(Box::new(move |v| {
        if let Some(id) = a1_id {
            assert!(v.invalidate_block(id, &mut false)?, "a1 is disconnected");
        }
        assert_eq!(v.deferred_len(), 0, "nothing deferred takes the armed admission");
        armed.store(true, Ordering::SeqCst);
        staged.send(()).expect("the test waits");
        queued.recv().expect("the test queues the ABC");
        Ok(())
    }));
    rig.v.send(stage).expect("V runs");
    at_staged.recv().expect("V staged");
    let abc = at_close(rig.v.activate_best_chain(), &rig.shutdown, rig.v.loading(), &w.sink);
    let query = lane.then(|| rig.v.query(|_| ()));
    let open = query.as_ref().map(|q| q.try_recv().is_none());
    go.send(()).expect("V waits");
    let want = if panic {
        Err(WaitError::Disconnected)
    } else {
        Ok(Ok(()))
    };
    assert_eq!(abc(), (want, Some((true, false, 1))), "{what}");
    if let Some(query) = query {
        assert_eq!(open, Some(true), "{what}: the query is unanswered before V resumes");
        assert_eq!(query.recv(), Err(WaitError::Disconnected), "{what}: refused");
    }
    let text = if panic {
        W_PANIC_TEXT
    } else {
        W_STORE_TEXT
    };
    assert_eq!(w.sink.messages(), [text], "{what}: W's report only");
    let exit = rig.stop();
    let fatal = exit.fatal.clone().expect("V stopped on W's failure");
    assert_eq!(fatal.0, text, "{what}");
    assert_eq!((fatal.is_abort_node(), fatal.is_contract()), (!panic, false), "{what}");
    assert_eq!(exit_tip(&exit), Some(genesis.block.hash()), "{what}: no step");
    if let Some((_, b1_id)) = ids {
        assert!(exit.idx.is_candidate(b1_id), "{what}: b1 was due");
    }
    let b1_checked =
        rig.rec.take().iter().any(|e| matches!(e, Ev::Checked(h, _) if *h == b1.hash()));
    assert!(!b1_checked, "{what}: no work resumed");
}

#[test]
fn an_abc_meeting_ws_failure_at_its_first_release_point_answers_ok_bell_no_work() {
    an_abc_meeting_ws_failure_at_its_first_release_point(false, false, false);
}

#[test]
fn an_abc_meeting_ws_failure_at_its_first_release_point_answers_ok_bell_work_due() {
    an_abc_meeting_ws_failure_at_its_first_release_point(false, true, false);
}

#[test]
fn an_abc_meeting_ws_failure_at_its_first_release_point_answers_ok_lane_no_work() {
    an_abc_meeting_ws_failure_at_its_first_release_point(true, false, false);
}

#[test]
fn an_abc_meeting_ws_failure_at_its_first_release_point_answers_ok_lane_work_due() {
    an_abc_meeting_ws_failure_at_its_first_release_point(true, true, false);
}

/// D240's assert control: W's panic at an ABC's first release point is one of Core's asserts, so
/// the reply is dropped as before, after the node's flag, with W's one report.
// Core: util/check.h:49-51
#[test]
fn an_abc_meeting_ws_panic_at_its_first_release_point_drops_its_reply() {
    an_abc_meeting_ws_failure_at_its_first_release_point(false, false, true);
}

/// D240's contract control: an SB4 contract failure at an ABC's first release point (a class M
/// command dispatched there) is not W's failure, so the ABC still answers `Err` with its text, and
/// V stops on it.
// Core: validation.cpp:3771, :3795-3797
#[test]
fn an_abc_meeting_a_contract_failure_at_its_first_release_point_answers_err() {
    let w = WSide::new();
    let mut rig = Rig::with(w.config(RigConfig::default()));
    let resume = pause(&rig.v);
    let abc = rig.v.activate_best_chain();
    let main = ChainCommand::TestUnderMain(Box::new(|_| Err(flush_contract_failure())));
    rig.v.send(main).expect("V runs");
    resume.send(()).expect("V paused");
    let text = flush_contract_failure().0;
    assert_eq!(abc.recv(), Ok(Err(text.clone())));
    let exit = rig.stop();
    let fatal = exit.fatal.expect("V stopped on the contract failure");
    assert_eq!(fatal.0, text);
    assert!(fatal.is_contract(), "{fatal:?}");
    assert_eq!(w.sink.messages(), [text], "V's one report");
}
