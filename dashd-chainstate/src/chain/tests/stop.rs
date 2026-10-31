//! CH3b D213: V stops where Core's `AbortNode` reports (`shutdown.cpp:27-37`). An `AbortNode`
//! leaving ABC or `InvalidateBlock` has stopped V before their callers log their `false`. One a
//! command run at a release point raises stops V there, as Core's other thread requests
//! shutdown, and the class C operation it interrupted ends as Core's does.

use std::time::Duration;

use dash_chain::{ChainError, Status};
use dash_consensus::{Block, BlockId};
use dash_primitives::BlockHash;

use super::abc::Breaks;
use super::failure::{flag_at_close, pause, tip};
use super::flush::{ModelW, misused_attach};
use super::support::{
    Mined, Rig, RigConfig, assert_logged_before, branch, child, install_accept_log, lines_on,
    regtest,
};
use crate::chain::actor::COINS_ATTACH_MISUSE;
use crate::chain::reply::{WaitError, reply};
use crate::chain::{
    BlockConnector, BlockStatusWriter, ChainCommand, ConnectCx, ConnectOutcome, DisconnectResult,
    Fatal, NullConnector, ProposalChecks, VContext, ViewTarget,
};

const SOON: Duration = Duration::from_secs(120);

/// Fails one block's disconnect with a storage fault (CH3 §2.1).
pub(super) struct DisconnectFails(pub(super) BlockHash);

impl BlockConnector for DisconnectFails {
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
        b: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        if b.hash() == self.0 {
            return Err(Fatal::new("disk on fire"));
        }
        NullConnector.disconnect_block(cx, v, target, b, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        b: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, b, prev, pow, merkle)
    }
}

/// (a): an `AbortNode` leaving ABC or `InvalidateBlock` has stopped V when the operation returns
/// to its caller: the sink has reported it, and V's stop has set V's latch. The stop in `run`
/// then reports nothing more, so the node sees one `AbortNode`, and the flush side as before
/// (D62).
#[test]
fn an_abort_leaving_abc_or_invalidate_block_has_stopped_v() {
    for site in ["abc", "invalidate"] {
        let p = regtest();
        let genesis = Mined::genesis(&p);
        let (a1, b1) = (child(&p, &genesis, 0), child(&p, &genesis, 1));
        let connector: Box<dyn BlockConnector> = if site == "abc" {
            Box::new(Breaks {
                at: b1.hash(),
                abort: true,
            })
        } else {
            Box::new(DisconnectFails(a1.hash()))
        };
        let w = ModelW::default();
        let mut rig = Rig::with(RigConfig {
            connector: Some(connector),
            flush: Some(w.site()),
            ..RigConfig::default()
        });
        rig.accept(&a1);
        rig.accept(&b1);
        let (id, abort, handle) = (rig.id(&a1), rig.abort.clone(), rig.v.clone());
        let (at_return, seen) = reply();
        let on_v = ChainCommand::TestUnderV(Box::new(move |v| {
            let r = if site == "abc" {
                // b1 ties a1: with a1 invalid, ABC connects b1, whose connect fails.
                assert!(v.invalidate_block(id)?, "a1 is disconnected");
                v.activate_best_chain(None)
            } else {
                v.invalidate_block(id).map(|_| ())
            };
            at_return.send((abort.messages(), handle.stopping()));
            r
        }));
        rig.v.send(on_v).expect("sent");
        // Before `stop`, whose `shutdown` sets V's latch too.
        let seen = seen.recv_timeout(SOON).expect("ran").expect("sent");
        assert_eq!(seen, (vec!["disk on fire".to_owned()], true), "{site}: V had stopped");
        assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some("disk on fire"), "{site}");
        assert_eq!(rig.abort.messages(), ["disk on fire"], "{site}: one report");
        w.assert_stopped_on_an_abort_node();
    }
}

/// (a), as Core's `debug.log` shows it: `ProcessNewBlock`'s failure line follows the
/// `AbortNode`'s report (`v:4901`, after `shutdown.cpp:30`).
#[test]
fn the_process_new_block_failure_line_follows_the_report() {
    install_accept_log();
    let p = regtest();
    let a1 = child(&p, &Mined::genesis(&p), 0);
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(Breaks {
            at: a1.hash(),
            abort: true,
        })),
        ..RigConfig::default()
    });
    let thread = rig.v_thread();
    let r = rig.v.process_block(a1.block.clone(), true, None, true).recv();
    assert_eq!(r, Err(WaitError::Disconnected));
    assert!(rig.stop().fatal.is_some());
    assert_logged_before(
        &lines_on(thread),
        "*** disk on fire",
        "ProcessNewBlock: ActivateBestChain failed (disk on fire)",
    );
}

/// (b): an `AbortNode` from a command at ABC's release point (`v:3760`, before its step) stops
/// V there, so the command behind it is not served and both replies disconnect after the node's
/// flag. ABC then takes its step and breaks at `ShutdownRequested()` (`v:3841`), and
/// `ProcessNewBlock` logs `ACCEPTED` and replies (`v:4905`), as Core's thread does after
/// another's `AbortNode`. V's loop then ends on the command's `AbortNode`, reported once.
#[test]
fn an_abort_at_abcs_release_point_lets_abc_take_its_step() {
    install_accept_log();
    let w = ModelW::default();
    let mut rig = Rig::with(RigConfig {
        flush: Some(w.site()),
        ..RigConfig::default()
    });
    let a1 = child(&rig.p, &rig.genesis, 0);
    let thread = rig.v_thread();
    let resume = pause(&rig.v);
    let processed =
        flag_at_close(rig.v.process_block(a1.block.clone(), true, None, true), &rig.shutdown);
    let attached = flag_at_close(misused_attach(&rig.v), &rig.shutdown);
    let behind = flag_at_close(rig.v.query(|_| ()), &rig.shutdown);
    resume.send(()).expect("V paused");
    let (accepted, flag) = processed();
    assert!(accepted.expect("a1's reply").accepted);
    assert_eq!(flag, Some(true), "V had stopped before ABC's step");
    assert_eq!(attached(), (Err(WaitError::Disconnected), Some(true)));
    assert_eq!(behind(), (Err(WaitError::Disconnected), Some(true)), "not served");
    assert_eq!(tip(&rig), rig.id(&a1), "the step connected a1");
    rig.stop_on(COINS_ATTACH_MISUSE);
    w.assert_stopped_on_an_abort_node();
    let lines = lines_on(thread);
    assert_logged_before(
        &lines,
        &format!("*** {COINS_ATTACH_MISUSE}"),
        "ProcessNewBlock : ACCEPTED",
    );
    assert!(!lines.iter().any(|l| l.contains("ActivateBestChain failed")), "{lines:#?}");
}

/// (b) leaves V's own failures to unwind: an SB4 contract failure (`ChainError`) from a command
/// at ABC's release point stops V there and unwinds ABC before its step, as SB4's contract has
/// the host stop (`dash-chain/src/chain.rs:1226-1231`). `ProcessNewBlock` logs its failure line
/// after the report.
#[test]
fn a_chain_failure_at_abcs_release_point_unwinds_it() {
    install_accept_log();
    let mut rig = Rig::new();
    let a1 = child(&rig.p, &rig.genesis, 0);
    let thread = rig.v_thread();
    let resume = pause(&rig.v);
    let processed =
        flag_at_close(rig.v.process_block(a1.block.clone(), true, None, true), &rig.shutdown);
    let broken = ChainCommand::TestUnderMain(Box::new(|_| {
        Err(Fatal::from(ChainError::Internal("broken at the release point")))
    }));
    rig.v.send(broken).expect("sent");
    resume.send(()).expect("V paused");
    assert_eq!(processed(), (Err(WaitError::Disconnected), Some(true)));
    let msg = "chain: internal: broken at the release point";
    rig.stop_on(msg);
    assert_eq!(tip(&rig), rig.id(&rig.genesis), "no step");
    assert_logged_before(
        &lines_on(thread),
        &format!("*** {msg}"),
        &format!("ProcessNewBlock: ActivateBestChain failed ({msg})"),
    );
}

/// (b) for `InvalidateBlock`, whose `ShutdownRequested()` check heads its loop (`v:3937`), before
/// the release (`v:3940`): the `AbortNode` at the first release point leaves it one disconnect,
/// and it breaks at the next check. The block it was asked to invalidate stays in the chain.
#[test]
fn an_abort_at_invalidate_blocks_release_point_leaves_it_one_disconnect() {
    let mut rig = Rig::new();
    let main = branch(&rig.p, &rig.genesis, 3, 0);
    for m in &main {
        rig.accept(m);
    }
    let resume = pause(&rig.v);
    let invalidated = rig.v.invalidate_block(main[1].hash());
    let attached = flag_at_close(misused_attach(&rig.v), &rig.shutdown);
    resume.send(()).expect("V paused");
    assert_eq!(invalidated.recv_timeout(SOON), Some(Ok(Ok(()))));
    assert_eq!(attached(), (Err(WaitError::Disconnected), Some(true)));
    assert_eq!(tip(&rig), rig.id(&main[1]), "one disconnect");
    assert!(rig.status(&main[2]).has(Status::FAILED_VALID));
    assert!(!rig.status(&main[1]).has(Status::FAILED_VALID));
    rig.stop_on(COINS_ATTACH_MISUSE);
}
