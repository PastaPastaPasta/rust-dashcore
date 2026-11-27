//! D60: [`MasternodeListEngine::progress`] over Core's regtest payloads. Every tracked call's
//! cross-check (`check_projection`) holds it to its projection across the whole suite; these pin
//! the exact moves per call.

use dash_evo::locks::LockReject;

use super::*;
use crate::{LockOutcome, QuorumState};

/// Core's ChainLock at `height`.
fn clsig_at(height: i32) -> ClSig {
    clsigs().into_iter().find(|(c, _)| c.height() == height).expect("captured").0
}

/// Progress after `call`, less before it.
fn moves(engine: &mut MasternodeListEngine, call: impl FnOnce(&mut MasternodeListEngine)) -> u64 {
    let before = engine.progress();
    call(engine);
    engine.progress() - before
}

/// A call that only asks moves nothing; failing what it asked moves once; a repeat of either
/// moves nothing.
#[test]
fn a_request_added_alone_moves_nothing_and_its_failure_moves_once() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&read_diff(&genesis_diff(281)), &chain, config()).expect("Core's diff");
    let cl = clsig_at(311);
    let unavailable = Err(LockOutcome::Rejected(LockReject::QuorumUnavailable));
    let asked = moves(&mut engine, |e| {
        assert_eq!(e.verify_chainlock(&cl, &chain, config()).map(|_| ()), unavailable);
    });
    assert_eq!(asked, 0);
    let requests = engine.requests();
    assert!(!requests.is_empty());
    let (islock, _) = isdlocks().into_iter().next().expect("captured");
    let again = moves(&mut engine, |e| {
        assert_eq!(e.verify_chainlock(&cl, &chain, config()).map(|_| ()), unavailable);
        let _ = e.verify_islock(&islock, &chain, config());
    });
    assert_eq!(again, 0);
    for request in requests {
        assert_eq!(moves(&mut engine, |e| e.request_failed(request.key())), 1);
        assert_eq!(moves(&mut engine, |e| e.request_failed(request.key())), 0, "gone");
    }
}

/// Steps 1–4 and step 5 on a step diff move once; a `qrinfo` moves once; `take_dirty` and a
/// `tip_changed` at the tip retention last ran on move nothing; a branch change moves once.
#[test]
fn payloads_and_a_branch_change_move_once_and_bookkeeping_never() {
    let chain = FixtureChain {
        fork: Some(300),
        ..FixtureChain::full()
    };
    let mut engine = applied(&chain, 24, 25..=chain.tip - 1);
    assert_eq!(
        moves(&mut engine, |e| {
            e.apply_diff(&read_diff(&step_diff(chain.tip)), &chain, config()).expect("Core's diff");
        }),
        1
    );
    assert_eq!(
        moves(&mut engine, |e| {
            e.apply_qrinfo(&read_qrinfo(&qrinfo(284, &[])), &chain, config())
                .expect("Core's qrinfo");
        }),
        1
    );
    assert_eq!(
        moves(&mut engine, |e| {
            assert!(!e.take_dirty().is_empty());
        }),
        0
    );
    assert_eq!(
        moves(&mut engine, |e| {
            e.tip_changed(&chain, config()).expect("retention");
        }),
        0
    );
    let forked = FixtureChain {
        fork: Some(300),
        fork_active: true,
        ..FixtureChain::full()
    };
    assert_eq!(
        moves(&mut engine, |e| {
            e.tip_changed(&forked, config()).expect("retention");
        }),
        1
    );
}

/// A ChainLock that moves `best_cl` moves once and its repeat nothing; an InstantSend lock's
/// verdict changes nothing a waiter reads: it moves nothing.
#[test]
fn a_new_best_chainlock_moves_once_and_an_islock_never() {
    let chain = FixtureChain::full();
    let mut engine = synced(&chain);
    let cl = clsig_at(311);
    assert_eq!(
        moves(&mut engine, |e| assert!(e.verify_chainlock(&cl, &chain, config()).is_ok())),
        1
    );
    assert_eq!(
        moves(&mut engine, |e| assert!(e.verify_chainlock(&cl, &chain, config()).is_ok())),
        0
    );
    let verified = isdlocks()
        .into_iter()
        .filter(|(_, ok)| *ok)
        .filter(|(islock, _)| engine.verify_islock(islock, &chain, config()).is_ok())
        .count();
    assert!(verified > 0);
    for (islock, _) in isdlocks() {
        assert_eq!(
            moves(&mut engine, |e| {
                let _ = e.verify_islock(&islock, &chain, config());
            }),
            0
        );
    }
}

/// A lock held above the tip moves once; reading it ready moves nothing, nor does clearing a
/// lock not held; clearing it moves once.
#[test]
fn a_held_chainlock_moves_when_held_and_cleared_only() {
    let full = FixtureChain::full();
    let short = FixtureChain::anchored(0, 290);
    let mut engine = synced(&full);
    let cl = clsig_at(311);
    assert_eq!(
        moves(&mut engine, |e| {
            assert_eq!(e.verify_chainlock(&cl, &short, config()), Err(LockOutcome::Pending));
        }),
        1
    );
    let progress = engine.progress();
    assert_eq!(engine.pending_chainlock_ready(&full), Ok(Some(cl.clone())));
    assert_eq!(engine.pending_chainlock_ready(&full), Ok(Some(cl.clone())));
    assert_eq!(engine.progress(), progress);
    assert_eq!(moves(&mut engine, |e| assert!(!e.clear_pending_chainlock(&clsig_at(308)))), 0);
    assert_eq!(moves(&mut engine, |e| assert!(e.clear_pending_chainlock(&cl))), 1);
    assert_eq!(moves(&mut engine, |e| assert!(!e.clear_pending_chainlock(&cl))), 0);
}

/// Ending deferral runs step 5: one move. While deferred, the refused payloads move nothing.
#[test]
fn ending_deferral_moves_once() {
    let chain = FixtureChain::full();
    let mut engine = MasternodeListEngine::new(Mode::Client).with_deferred_verification();
    apply_to(&mut engine, &chain, 24, 25..=chain.tip);
    assert_eq!(
        moves(&mut engine, |e| {
            let _ = e.apply_qrinfo(&read_qrinfo(&qrinfo(284, &[])), &chain, config());
            let _ = e.verify_chainlock(&clsig_at(311), &chain, config());
        }),
        0
    );
    assert_eq!(
        moves(&mut engine, |e| {
            e.end_deferral(&chain, config()).expect("step 5");
        }),
        1
    );
    assert!(matches!(
        state(&engine, CHAIN_LOCKS, core_quorums(CHAIN_LOCKS)[0].hash),
        QuorumState::Verified(_)
    ));
}

/// A step-5 request that outlives its need: the list at 240 asks for its commitments' work
/// blocks at 136 and 160, then a reorg below it drops that list and its commitments, and the
/// fork's tip list needs neither block. Core's late reply to 136 answers the request, and the same
/// call's retention drops the reply's list, below the horizon, with the commitments it added: the
/// request's removal is the call's only change, one move.
#[test]
fn a_late_reply_to_a_request_that_outlived_its_need_moves_once() {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let lower = FixtureChain {
        tip: 240,
        ..FixtureChain::full()
    };
    engine.apply_diff(&read_diff(&genesis_diff(240)), &lower, config()).expect("Core's diff");
    assert_eq!(asked_heights(&engine), [136, 160], "step 5's work blocks");
    let fork = FixtureChain {
        fork: Some(200),
        fork_active: true,
        ..FixtureChain::full()
    };
    engine.tip_changed(&fork, config()).expect("the reorg");
    assert!(engine.held_heights().is_empty() && engine.held_tos().is_empty(), "nothing needs them");
    engine
        .apply_diff(&read_diff(&fork.on_fork(genesis_diff(fork.tip))), &fork, config())
        .expect("Core's diff");
    assert_eq!(asked_heights(&engine), [136, 160], "still outstanding");
    let (lists, records) = (engine.held_heights(), engine.held_tos());
    assert_eq!(
        moves(&mut engine, |e| {
            e.apply_diff(&read_diff(&genesis_diff(136)), &fork, config())
                .expect("Core's late reply");
        }),
        1
    );
    assert_eq!(asked_heights(&engine), [160], "answered");
    assert_eq!((engine.held_heights(), engine.held_tos()), (lists, records), "its list went");
}
