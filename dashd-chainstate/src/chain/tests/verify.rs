//! `VerifyLoadedChainstate` on V (CH1-node-LR, D64): the load-time
//! `ResetBlockFailureFlags(nullptr)` after the VerifyDB point, its gates, and the statuses
//! Core's init reads. `node-l1/tests/load_reset_core.rs` runs the same restarts on Core.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_chain::Status;
use dash_consensus::BlockId;

use super::failure::{three_forks, tip};
use super::support::{Mined, Rig, RigConfig, SharedFiles, branch, child};
use crate::chain::actor::{V, VERIFY_LOADED_MISUSE, VExit};
use crate::chain::verify::{
    CORRUPTED_BLOCK_DB, EVODB_WITHOUT_BLOCKS, FUTURE_TIP, INSUFFICIENT_DBCACHE, VERIFY_INTERRUPTED,
};
use crate::chain::{
    ChainCommand, ChainHandle, Fatal, VerifyDbArgs, VerifyDbResult, VerifyLoadError,
    VerifyLoadOptions,
};

const SOON: Duration = Duration::from_secs(120);

/// Runs on V before startup's `VerifyLoaded`.
type OnVerify = Box<dyn FnOnce(&mut V) + Send>;

fn failed(rig: &Rig, m: &Mined) -> bool {
    rig.status(m).has(Status::FAILED_VALID)
}

/// A rig whose block files outlive it, with `main` (`n` blocks) accepted.
fn chain(n: u32) -> (Rig, SharedFiles, Vec<Mined>) {
    let files = SharedFiles::default();
    let rig = Rig::with(RigConfig {
        files: Some(Box::new(files.clone())),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, n, 0);
    for m in &main {
        rig.accept(m);
    }
    (rig, files, main)
}

/// V stopped and restarted with the coins at `coins_tip`; before init's ABC, `before` runs on V
/// and then startup's `VerifyLoaded` is sent with `options`. Its reply, and the rig after init's
/// ABC.
fn restart_verified(
    mut rig: Rig,
    files: SharedFiles,
    coins_tip: &Mined,
    options: VerifyLoadOptions,
    cfg: RigConfig,
    before: Option<OnVerify>,
) -> (Rig, Result<(), VerifyLoadError>) {
    let coins_tip = rig.id(coins_tip);
    restart_exit(rig.stop(), files, coins_tip, options, cfg, before)
}

/// [`restart_verified`] from V's exit, which a test may edit first.
fn restart_exit(
    exit: VExit,
    files: SharedFiles,
    coins_tip: BlockId,
    options: VerifyLoadOptions,
    cfg: RigConfig,
    before: Option<OnVerify>,
) -> (Rig, Result<(), VerifyLoadError>) {
    let got = Arc::new(Mutex::new(None));
    let sent = got.clone();
    let cfg = RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            if let Some(before) = before {
                v.send(ChainCommand::TestOnV(Box::new(move |v| {
                    before(v);
                    Ok::<(), Fatal>(())
                })))
                .expect("V runs");
            }
            let r =
                v.verify_loaded(options).recv_timeout(SOON).expect("V serves it before the ABC");
            *sent.lock().expect("reply") = Some(r.expect("V replies"));
        })),
        ..cfg
    };
    let rig = Rig::restart_with(exit, Box::new(files), coins_tip, cfg).expect("V starts");
    let r = got.lock().expect("reply").take().expect("sent before the ABC");
    (rig, r)
}

fn restart(
    rig: Rig,
    files: SharedFiles,
    coins_tip: &Mined,
    options: VerifyLoadOptions,
) -> (Rig, Result<(), VerifyLoadError>) {
    restart_verified(rig, files, coins_tip, options, RigConfig::default(), None)
}

fn level(check_level: i64) -> VerifyLoadOptions {
    VerifyLoadOptions {
        check_level,
        ..VerifyLoadOptions::default()
    }
}

/// `invalidateblock` of the tip, then a restart at the default level: the best known invalid
/// block descends from the loaded tip, so the reset brings it back and init's ABC reconnects it
/// (O2b D32: Core's restart undoes the invalidation).
#[test]
fn a_restart_reconsiders_an_invalidated_tip() {
    let (rig, files, main) = chain(3);
    assert_eq!(rig.v.invalidate_block(main[2].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&main[1]));
    let (mut rig, r) = restart(rig, files, &main[1], VerifyLoadOptions::default());
    assert_eq!(r, Ok(()));
    assert_eq!(tip(&rig), rig.id(&main[2]));
    assert!(!failed(&rig, &main[2]));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), None);
    rig.stop();
}

/// An invalidated ancestor of the tip: the best known invalid block is the old tip, the reset
/// clears it and its failed ancestors (`validation.cpp:4194-4216`), and the whole branch
/// reconnects.
#[test]
fn a_restart_reconsiders_an_invalidated_branch() {
    let (rig, files, main) = chain(3);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&main[0]));
    let (mut rig, r) = restart(rig, files, &main[0], VerifyLoadOptions::default());
    assert_eq!(r, Ok(()));
    assert_eq!(tip(&rig), rig.id(&main[2]));
    assert!(!failed(&rig, &main[1]) && !failed(&rig, &main[2]));
    rig.stop();
}

/// The reset runs at a `-checklevel` of 3 or more as `GetIntArg` read it, before VerifyDB's
/// narrowing to `int` (`node/chainstate.cpp:463`): 2^32 + 2 resets though VerifyDB would run at
/// level 2, and -(2^32) + 3 does not though VerifyDB would run at 3.
#[test]
fn the_reset_reads_the_level_before_its_narrowing() {
    let resets = |check_level: i64| {
        let (rig, files, main) = chain(2);
        assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
        let (mut rig, r) = restart(rig, files, &main[0], level(check_level));
        assert_eq!(r, Ok(()));
        let reset = tip(&rig) == rig.id(&main[1]);
        assert_eq!(reset, !failed(&rig, &main[1]), "level {check_level}");
        rig.stop();
        reset
    };
    for (check_level, want) in [
        (i64::MIN, false),
        (-(1 << 32) + 3, false),
        (-1, false),
        (0, false),
        (2, false),
        (3, true),
        (4, true),
        (5, true),
        ((1 << 32) + 2, true),
        (i64::MAX, true),
    ] {
        assert_eq!(resets(check_level), want, "level {check_level}");
    }
    assert_eq!(VerifyDbArgs::new(&level((1 << 32) + 2)).level(), 2);
    assert_eq!(VerifyDbArgs::new(&level(-(1 << 32) + 3)).level(), 3);
}

/// An empty coins view skips VerifyDB and the reset (`node/chainstate.cpp:422`, `:467-475`); with
/// a non-empty evodb it is Core's failure.
#[test]
fn an_empty_coins_view_skips_the_reset() {
    let (rig, files, main) = chain(2);
    assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
    let empty = VerifyLoadOptions {
        coins_empty: true,
        ..VerifyLoadOptions::default()
    };
    let (rig, r) = restart(rig, files.clone(), &main[0], empty);
    assert_eq!(r, Ok(()));
    assert_eq!(tip(&rig), rig.id(&main[0]));
    assert!(failed(&rig, &main[1]));
    let with_evo = VerifyLoadOptions {
        evodb_empty: false,
        ..empty
    };
    let (mut rig, r) = restart(rig, files, &main[0], with_evo);
    assert_eq!(r, Err(VerifyLoadError::Failure(EVODB_WITHOUT_BLOCKS)));
    assert!(failed(&rig, &main[1]));
    rig.stop();
}

/// The best known invalid block on another branch does not descend from the tip: no reset.
#[test]
fn an_invalid_side_branch_stays_invalid() {
    let (rig, files, main) = chain(2);
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in &side {
        rig.accept(m);
    }
    assert_eq!(tip(&rig), rig.id(&side[2]));
    assert_eq!(rig.v.invalidate_block(side[0].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&main[1]));
    let (mut rig, r) = restart(rig, files, &main[1], VerifyLoadOptions::default());
    assert_eq!(r, Ok(()));
    assert_eq!(rig.idx(|ix| ix.best_invalid()), Some(rig.id(&side[2])));
    assert_eq!(tip(&rig), rig.id(&main[1]));
    assert!(side.iter().all(|m| failed(&rig, m)));
    rig.stop();
}

/// A loaded tip more than two hours ahead of the clock is Core's failure, before VerifyDB and
/// the reset (`node/chainstate.cpp:423-428`); at two hours it loads.
#[test]
fn a_tip_from_the_future_fails_the_load() {
    for (ahead, refused) in [(7_200, false), (7_201, true)] {
        let (rig, files, main) = chain(2);
        assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
        let tip_time = i64::from(main[0].time()) - i64::from(rig.genesis.time());
        let cfg = RigConfig {
            after_genesis: Some(tip_time - ahead),
            ..RigConfig::default()
        };
        let (mut rig, r) =
            restart_verified(rig, files, &main[0], VerifyLoadOptions::default(), cfg, None);
        let want = if refused {
            Err(VerifyLoadError::Failure(FUTURE_TIP))
        } else {
            Ok(())
        };
        assert_eq!(r, want, "{ahead} s ahead");
        assert_eq!(failed(&rig, &main[1]), refused, "the reset did not run");
        rig.stop();
    }
}

/// VerifyDB's results as init reads them (`node/chainstate.cpp:441-454`): a failing result
/// returns before the reset; skipped level-3 checks fail only when `-checkblocks` or
/// `-checklevel` was set.
#[test]
fn verifydb_results_map_to_cores_statuses() {
    use VerifyDbResult::*;
    let cases = [
        (Success, false, Ok(())),
        (SkippedMissingBlocks, true, Ok(())),
        (Interrupted, false, Err(VerifyLoadError::Interrupted(VERIFY_INTERRUPTED))),
        (CorruptedBlockDb, false, Err(VerifyLoadError::Failure(CORRUPTED_BLOCK_DB))),
        (SkippedL3Checks, false, Ok(())),
        (SkippedL3Checks, true, Err(VerifyLoadError::InsufficientDbcache(INSUFFICIENT_DBCACHE))),
    ];
    for (result, require_full_verification, want) in cases {
        let (rig, files, main) = chain(2);
        assert_eq!(rig.v.invalidate_block(main[1].hash()).recv(), Ok(Ok(())));
        let options = VerifyLoadOptions {
            require_full_verification,
            ..VerifyLoadOptions::default()
        };
        let (mut rig, r) = restart_verified(
            rig,
            files,
            &main[0],
            options,
            RigConfig::default(),
            Some(Box::new(move |v| v.verify_db_result = Some(result))),
        );
        assert_eq!(r, want, "{result:?}, required {require_full_verification}");
        assert_eq!(failed(&rig, &main[1]), want.is_err(), "{result:?}: the reset ran on success");
        rig.stop();
    }
}

/// VerifyDB's depth and level as it computes them (`validation.cpp:5050-5053`).
#[test]
fn verifydb_args_follow_cores_bounds() {
    let args = |check_blocks, check_level| {
        VerifyDbArgs::new(&VerifyLoadOptions {
            check_blocks,
            check_level,
            ..VerifyLoadOptions::default()
        })
    };
    assert_eq!(args(6, 3).depth(100), 6);
    assert_eq!(args(0, 3).depth(100), 100, "0 = all");
    assert_eq!(args(-5, 3).depth(100), 100);
    assert_eq!(args(101, 3).depth(100), 100);
    assert_eq!(args(1 << 32, 3).depth(100), 100, "narrowed to 0");
    assert_eq!(args((1 << 32) + 7, 3).depth(100), 7);
    assert_eq!(args(6, -1).level(), 0);
    assert_eq!(args(6, 9).level(), 4);
}

/// DEC-196 (2) holds V's commands until startup's ABC (D38); `VerifyLoaded` is served under the
/// hold, as startup's step before that ABC, while a query stays held. Core's order then resets
/// the loaded failed tip, the best known invalid block and its own ancestor at the tip's
/// height, so main's failure flags clear before the ABC; side's tied block still outranks it,
/// so the ABC moves there, as Core's would (D64).
#[test]
fn verify_loaded_is_served_under_the_startup_hold() {
    let (mut rig, files, _) = chain(0);
    let [main, side, _] = three_forks(&rig, 2);
    let a2 = rig.id(&main[1]);
    assert_eq!(rig.v.invalidate_block(main[0].hash()).recv(), Ok(Ok(())));
    let exit = rig.stop();
    let (sent, replies) = std::sync::mpsc::channel();
    let cfg = RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            let held = v.query(|_| ());
            let verified = v.verify_loaded(VerifyLoadOptions::default()).recv_timeout(SOON);
            sent.send((verified, held.try_recv().is_none())).expect("the test waits");
        })),
        ..RigConfig::default()
    };
    let mut rig = Rig::restart_with(exit, Box::new(files), a2, cfg).expect("V starts");
    let (verified, held) = replies.recv().expect("sent before init's ABC");
    assert_eq!(verified, Some(Ok(Ok(()))), "served under the hold");
    assert!(held, "the query waits for the ABC");
    assert!(!failed(&rig, &main[0]) && !failed(&rig, &main[1]));
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert!(rig.stop().fatal.is_none());
}

/// D64: the loaded failed tip b2 outranks every block that ties it, so DEC-196 (2) kept them all
/// below it (D36), a set Core's `LoadChainTip` never leaves (it asserts, `v:3592`). Once the
/// reset brings b2 back it stays the tip, and Core's prune drops the tied blocks: the next
/// `invalidateblock`'s `CheckBlockIndex` (`v:5755`) passes.
#[test]
fn a_reset_tip_that_outranks_its_tied_blocks_prunes_them() {
    let (rig, files, _) = chain(0);
    let [_, side, other] = three_forks(&rig, 2);
    // b2 the tip, so its invalidation marks it failed as it disconnects.
    assert_eq!(rig.v.precious_block(side[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert_eq!(rig.v.invalidate_block(side[0].hash()).recv(), Ok(Ok(())));
    assert!(failed(&rig, &side[1]));
    let (mut rig, verified) = restart(rig, files, &side[1], VerifyLoadOptions::default());
    assert_eq!(verified, Ok(()));
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert!(!failed(&rig, &side[0]) && !failed(&rig, &side[1]));
    assert_eq!(rig.v.invalidate_block(other[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(tip(&rig), rig.id(&side[1]));
    assert!(rig.stop().fatal.is_none());
}

/// D64: the prune runs only after a reset that ran. Here the best known invalid block is c3,
/// on another branch with more work, so the reset does not reach the loaded failed tip b2,
/// which stays failed; DEC-196 (2)'s tied blocks below it (D36) stay for startup's ABC, which
/// moves to one of them.
#[test]
fn a_failed_tip_the_reset_does_not_reach_keeps_its_tied_fallback() {
    let (rig, files, _) = chain(0);
    let [main, side, other] = three_forks(&rig, 2);
    let c3 = child(&rig.p, &other[1], 2);
    rig.accept(&c3);
    assert_eq!(rig.v.invalidate_block(c3.hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.v.precious_block(side[1].hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.v.invalidate_block(side[0].hash()).recv(), Ok(Ok(())));
    assert!(failed(&rig, &side[1]));
    let (mut rig, verified) = restart(rig, files, &side[1], VerifyLoadOptions::default());
    assert_eq!(verified, Ok(()));
    assert!(failed(&rig, &side[1]) && failed(&rig, &c3));
    assert!([rig.id(&main[1]), rig.id(&other[1])].contains(&tip(&rig)));
    assert!(rig.stop().fatal.is_none());
}

/// Opus LR r1 F1 (DEC-272 amended): the reset reaches a ChainLock-conflicting loaded tip b2
/// through its failed header child d3, but keeps b2's mark (it does not ignore ChainLocks), so
/// b2 stays out of the candidate set and nothing is pruned: DEC-196 (2)'s tied blocks below it
/// (D36) stay, and startup's ABC moves to a2 at level 3 as at level 2, where no reset runs.
#[test]
fn a_conflicting_tip_the_reset_reaches_keeps_its_tied_fallback() {
    for check_level in [2, 3] {
        let (rig, files, _) = chain(0);
        let [main, side, _] = three_forks(&rig, 2);
        assert_eq!(rig.v.precious_block(side[1].hash()).recv(), Ok(Ok(())));
        let d3 = child(&rig.p, &side[1], 2);
        rig.headers(std::slice::from_ref(&d3));
        assert_eq!(rig.v.invalidate_block(d3.hash()).recv(), Ok(Ok(())));
        let (b1, b2) = (rig.id(&side[0]), rig.id(&side[1]));
        let mut rig = rig;
        let mut exit = rig.stop();
        // DEC-196's index written after a ChainLock reorg off b2, with the coins still at b2.
        let conflicting = |g: &mut dash_chain::MutableGroup| {
            g.status = Status(g.status.bits() | Status::CONFLICT_CHAINLOCK);
        };
        exit.chain.node_index().expect("a node chain").modify(b2, conflicting);
        exit.chain.init_active_tip(b1).expect("b1 resident");
        let (mut rig, verified) =
            restart_exit(exit, files, b2, level(check_level), RigConfig::default(), None);
        assert_eq!(verified, Ok(()), "level {check_level}");
        assert_eq!(tip(&rig), rig.id(&main[1]), "level {check_level}: a2");
        assert!(rig.stop().fatal.is_none(), "level {check_level}");
    }
}

/// D64's latch, as D51's for the coins: a second `VerifyLoaded`, even before init's ABC, is V's
/// `AbortNode`; the first is served, the second's reply disconnects, and init fails.
#[test]
fn a_second_verify_loaded_aborts_the_node() {
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sent = replies.clone();
    let empty = VerifyLoadOptions {
        coins_empty: true,
        ..VerifyLoadOptions::default()
    };
    let mut rig = Rig::with(RigConfig {
        before_init: Some(Box::new(move |v: &ChainHandle| {
            for _ in 0..2 {
                let r = v.verify_loaded(empty).recv_timeout(SOON).expect("V answers or stops");
                sent.lock().expect("replies").push(r.is_ok());
            }
        })),
        aborts_before_init: true,
        ..RigConfig::default()
    });
    assert_eq!(*replies.lock().expect("replies"), [true, false]);
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(VERIFY_LOADED_MISUSE));
    assert_eq!(rig.abort.messages(), [VERIFY_LOADED_MISUSE]);
}

/// D64's latch, as D51's for the coins: a `VerifyLoaded` outside Core's init, after startup's
/// ABC, is V's `AbortNode`; its reply disconnects.
#[test]
fn a_verify_loaded_after_startup_aborts_the_node() {
    let mut rig = Rig::new();
    let verified = rig.v.verify_loaded(VerifyLoadOptions::default()).recv_timeout(SOON);
    assert!(matches!(verified, Some(Err(_))), "the reply disconnects: {verified:?}");
    assert_eq!(rig.stop().fatal.map(|f| f.0).as_deref(), Some(VERIFY_LOADED_MISUSE));
    assert_eq!(rig.abort.messages(), [VERIFY_LOADED_MISUSE]);
}
