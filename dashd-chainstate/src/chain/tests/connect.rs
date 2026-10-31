//! The connector seam (CH3 §8.1 a2): `BlockChecked` per failure kind, the abort path's one
//! report (D52), V's context lent into an attempt, and G2's predicate over V's context.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use dash_consensus::{Block, BlockId};
use dash_evo::Effect;
use dash_num::Hash256;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_store::ErrorKind;

use super::abc::Breaks;
use super::blocks::devnet;
use super::support::{Ev, Mined, Rig, RigConfig, branch, child, regtest};
use crate::chain::reply::{ActorReply, WaitError};
use crate::chain::seams::HeldChainLocks;
use crate::chain::{
    AbortCause, AbortPath, AbortSink as _, AttemptOrigin, BlockConnector, BlockResult, BlockState,
    BlockStatusWriter, ChainCommand, ConnectCx, ConnectOutcome, DisconnectPool, DisconnectResult,
    Fatal, FlushHost, FlushSite, FlushStateMode, GuardScope, LoadFailure, NullConnector,
    ProposalChecks, RecordingAbort, ScratchCoins, SiteDecision, StartError, VContext, ViewTarget,
    genesis_block,
};
use crate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use crate::connect::{
    AttemptId, ChainHelper as _, EvoAttempt as _, EvoHooks as _, INTERIM_REFUSAL, NoEvo,
    NodeHelper, is_governance_height,
};
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
}

impl FailsAtAccept {
    fn new(sink: &Arc<RecordingAbort>, contract: bool) -> Box<FailsAtAccept> {
        Box::new(FailsAtAccept {
            latched: Latched(GlobalFailStop::new()),
            sink: sink.clone(),
            failed: false,
            contract,
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
        if mode != FlushStateMode::None || std::mem::replace(&mut self.failed, true) {
            return Ok(SiteDecision::nothing());
        }
        if self.contract {
            return Err(flush_contract_failure());
        }
        let msg = "Disk space is too low!";
        self.sink.abort_node(msg, None);
        self.latched.0.trip(Failure {
            kind: ErrorKind::NoSpace,
            site: "flush",
            message: msg.into(),
        });
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
    let (inside, entered) = std::sync::mpsc::channel();
    let (go, queued) = std::sync::mpsc::channel();
    let operation = ChainCommand::TestOnV(Box::new(move |v| {
        inside.send(()).expect("the test waits");
        queued.recv().expect("the test queues the block");
        v.release_point()
    }));
    rig.v.send(operation).expect("V runs");
    entered.recv().expect("V is inside the operation");
    let reply = rig.v.process_block(block.clone(), true, None, true);
    go.send(()).expect("V waits");
    reply
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
            let chainlocks = HeldChainLocks::new(&chain, best_cl);
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
