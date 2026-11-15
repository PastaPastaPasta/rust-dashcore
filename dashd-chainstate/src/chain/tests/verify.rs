//! `VerifyLoadedChainstate` on V (CH1-node-LR, D64): the load-time
//! `ResetBlockFailureFlags(nullptr)` after the VerifyDB point, its gates, and the statuses
//! Core's init reads. `node-l1/tests/load_reset_core.rs` runs the same restarts on Core.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_chain::Status;
use dash_consensus::BlockId;
use dash_params::ChainSetup;

use super::blocks::devnet;
use super::failure::{three_forks, tip};
use super::support::{
    Mined, Rig, RigConfig, SharedFiles, branch, child, install_accept_log, levels_on, lines_on,
    regtest,
};
use crate::chain::actor::{V, VERIFY_LOADED_MISUSE, VExit};
use crate::chain::verify::{
    CORRUPTED_BLOCK_DB, EVODB_WITHOUT_BLOCKS, FUTURE_TIP, INSUFFICIENT_DBCACHE, VERIFY_INTERRUPTED,
};
use crate::chain::{
    ChainCommand, ChainHandle, Fatal, L3Skip, ScratchParent, VerifyDbArgs, VerifyDbResult,
    VerifyLoadError, VerifyLoadOptions,
};

const SOON: Duration = Duration::from_secs(120);

/// Runs on V before startup's `VerifyLoaded`.
type OnVerify = Box<dyn FnOnce(&mut V) + Send>;

fn failed(rig: &Rig, m: &Mined) -> bool {
    rig.status(m).has(Status::FAILED_VALID)
}

/// A rig whose block files outlive it, with `main` (`n` blocks) accepted.
fn chain(n: u32) -> (Rig, SharedFiles, Vec<Mined>) {
    chain_on(None, n)
}

/// [`chain`] on `params` (default regtest).
fn chain_on(params: Option<Arc<ChainSetup>>, n: u32) -> (Rig, SharedFiles, Vec<Mined>) {
    let files = SharedFiles::default();
    let rig = Rig::with(RigConfig {
        params,
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
                v.send(ChainCommand::TestUnderV(Box::new(move |v| {
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

/// CE5-05: init's pruned-datadir warning (`init.cpp:2092-2095` at v24.0.0) is at the network's
/// retention, Core's `MinBlocksToKeep()`: on a pruned datadir `-checkblocks` at the retention is
/// quiet and one more warns, on regtest (288) and on a devnet (2304), where 289 stays quiet. Coins
/// and evodb are empty, so VerifyDB does not run.
#[test]
fn the_pruned_datadir_warning_is_at_the_networks_retention() {
    install_accept_log();
    let options = |check_blocks| VerifyLoadOptions {
        check_blocks,
        coins_empty: true,
        evodb_empty: true,
        ..VerifyLoadOptions::default()
    };
    for (p, keep) in [(regtest(), 288), (devnet(), 2304)] {
        let warning = format!(
            "pruned datadir may not have more than {keep} blocks; only checking available blocks"
        );
        let mut cases = vec![(keep, false), (keep + 1, true)];
        if keep != 288 {
            // Above regtest's retention, within the devnet's.
            cases.push((289, false));
        }
        for (check_blocks, warns) in cases {
            let files = SharedFiles::default();
            let cfg = || RigConfig {
                params: Some(p.clone()),
                files: Some(Box::new(files.clone())),
                no_check_block_index: true,
                ..RigConfig::default()
            };
            let mut rig = Rig::with(cfg());
            let tip = rig.v.view().snap.tip.tip;
            let pruned: OnVerify = Box::new(|v: &mut V| v.have_pruned = true);
            let (rig, r) = restart_exit(
                rig.stop(),
                files.clone(),
                tip,
                options(check_blocks),
                cfg(),
                Some(pruned),
            );
            assert_eq!(r, Ok(()));
            let logged = levels_on(rig.v_thread(), &warning);
            let want: &[tracing::Level] = if warns {
                &[tracing::Level::WARN]
            } else {
                &[]
            };
            assert_eq!(logged, want, "{:?} -checkblocks={check_blocks}", p.network);
            rig.assert_no_abort();
        }
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
/// clears it and its failed ancestors (`validation.cpp:4205-4227` at v24.0.0), and the whole branch
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

/// Init's `AppInitMain: bls_legacy_scheme=0`: logged once V19 is active after the loaded tip,
/// after the pruned-datadir warning and before VerifyDB's first line; at a genesis tip with V19
/// at 1, with no VerifyDB line after it. Not logged on an empty coins view, a tip from the
/// future, or a tip whose next block is below V19 (BL-801).
// Core: node/chainstate.cpp:415-434, init.cpp:2091-2098 (v24.0.0)
#[test]
fn the_startup_scheme_line_is_logged_once_v19_is_active_after_the_tip() {
    use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};

    install_accept_log();
    const LINE: &str = "AppInitMain: bls_legacy_scheme=0";
    let v19_at = |height: u32| {
        let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .arg(ChainOption::TestActivationHeight, format!("v19@{height}"))
            .build()
            .expect("regtest with -testactivationheight");
        Arc::new(p)
    };
    // V's lines from a pruned restart at `n` blocks on `p` with `options`; `ahead` puts the
    // clock that many seconds behind the tip's time.
    let started = |p: Arc<ChainSetup>, n: u32, options: VerifyLoadOptions, ahead: Option<i64>| {
        let (mut rig, files, main) = chain_on(Some(Arc::clone(&p)), n);
        // Genesis when `n` is 0.
        let (tip, tip_time) = match main.last() {
            Some(m) => (rig.id(m), i64::from(m.time()) - i64::from(rig.genesis.time())),
            None => (rig.v.view().snap.tip.tip, 0),
        };
        let cfg = RigConfig {
            params: Some(p),
            files: Some(Box::new(files.clone())),
            after_genesis: ahead.map(|ahead| tip_time - ahead),
            ..RigConfig::default()
        };
        let pruned: OnVerify = Box::new(|v: &mut V| v.have_pruned = true);
        let (mut rig, r) = restart_exit(rig.stop(), files, tip, options, cfg, Some(pruned));
        let v = rig.v_thread();
        assert!(levels_on(v, LINE).iter().all(|l| *l == tracing::Level::INFO));
        let lines = lines_on(v);
        rig.stop();
        (r, lines)
    };
    let count = |lines: &[String]| lines.iter().filter(|l| *l == LINE).count();
    let at = |lines: &[String], prefix: &str| lines.iter().position(|l| l.starts_with(prefix));
    let defaults = VerifyLoadOptions::default();

    // 1. The genesis tip, V19 at 1: the line, and VerifyDB returns before its first line.
    let (r, lines) = started(regtest(), 0, defaults, None);
    assert_eq!(r, Ok(()));
    assert_eq!(count(&lines), 1, "{lines:#?}");
    assert_eq!(at(&lines, "Verifying last"), None, "{lines:#?}");

    // 2. Tip 2, V19 at 3: active after the tip, not at it. Between the pruned warning and
    // VerifyDB's first line.
    let pruned = VerifyLoadOptions {
        check_blocks: 289,
        ..defaults
    };
    let (r, lines) = started(v19_at(3), 2, pruned, None);
    assert_eq!(r, Ok(()));
    assert_eq!(count(&lines), 1, "{lines:#?}");
    let warning = at(&lines, "pruned datadir may not have more than 288 blocks");
    let line = at(&lines, LINE);
    let verifying = at(&lines, "Verifying last 2 blocks at level 3");
    assert!(warning.is_some() && verifying.is_some(), "{lines:#?}");
    assert!(warning < line && line < verifying, "{lines:#?}");

    // 3. Tip 2, V19 at 4: not active after the tip; VerifyDB still runs.
    let (r, lines) = started(v19_at(4), 2, defaults, None);
    assert_eq!(r, Ok(()));
    assert_eq!(count(&lines), 0, "{lines:#?}");
    assert!(at(&lines, "Verifying last 2 blocks at level 3").is_some(), "{lines:#?}");

    // 4. An empty coins view (`-reindex`, `-reindex-chainstate`, no best block).
    let empty = VerifyLoadOptions {
        coins_empty: true,
        ..defaults
    };
    let (r, lines) = started(regtest(), 2, empty, None);
    assert_eq!(r, Ok(()));
    assert_eq!(count(&lines), 0, "{lines:#?}");

    // 5. A tip from the future fails the load before the line.
    let (r, lines) = started(regtest(), 2, defaults, Some(7_201));
    assert_eq!(r, Err(VerifyLoadError::Failure(FUTURE_TIP)));
    assert_eq!(count(&lines), 0, "{lines:#?}");
}

/// VerifyDB's results as init reads them (`node/chainstate.cpp:441-454`): a failing result
/// returns before the reset; skipped level-3 checks fail only when `-checkblocks` or
/// `-checklevel` was set, with Core's text for a memory skip and the readiness refusal for an
/// evo skip (DEC-993); otherwise the reset runs after either.
#[test]
fn verifydb_results_map_to_cores_statuses() {
    use VerifyDbResult::*;
    let cases = [
        (Success, false, Ok(())),
        (SkippedMissingBlocks, true, Ok(())),
        (Interrupted, false, Err(VerifyLoadError::Interrupted(VERIFY_INTERRUPTED))),
        (CorruptedBlockDb, false, Err(VerifyLoadError::Failure(CORRUPTED_BLOCK_DB))),
        (SkippedL3Checks(L3Skip::Dbcache), false, Ok(())),
        (
            SkippedL3Checks(L3Skip::Dbcache),
            true,
            Err(VerifyLoadError::InsufficientDbcache(INSUFFICIENT_DBCACHE)),
        ),
        (SkippedL3Checks(L3Skip::NotReady("refused".into())), false, Ok(())),
        // The readiness refusal is the load's answer, not a reindex question.
        (
            SkippedL3Checks(L3Skip::NotReady("refused".into())),
            true,
            Err(VerifyLoadError::NotReady("refused".into())),
        ),
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
            Some(Box::new({
                let result = result.clone();
                move |v| v.verify_db_result = Some(result)
            })),
        );
        assert_eq!(r, want, "{result:?}, required {require_full_verification}");
        assert_eq!(failed(&rig, &main[1]), want.is_err(), "{result:?}: the reset ran on success");
        rig.stop();
    }
}

/// `verifychain`'s `VerifyDb` runs the load path's VerifyDB point after startup, as often as it is
/// sent, and answers its result (RP1a D46).
#[test]
fn verify_db_answers_the_verifydb_point() {
    use VerifyDbResult::*;

    let (mut rig, _files, _main) = chain(2);
    let args = VerifyDbArgs {
        level: 4,
        depth: 0,
    };
    assert_eq!(rig.v.verify_db(args).recv(), Ok(Success));
    let skips = [L3Skip::Dbcache, L3Skip::NotReady("refused".into())].map(SkippedL3Checks);
    for result in
        [CorruptedBlockDb, Interrupted, SkippedMissingBlocks, Success].into_iter().chain(skips)
    {
        let set = result.clone();
        rig.on_v(move |v| {
            v.verify_db_result = Some(set);
            Ok(())
        });
        assert_eq!(rig.v.verify_db(args).recv(), Ok(result));
    }
    rig.stop();
}

/// Block files that fail to read one block, or serve it garbled: its header with no
/// transactions, which `CheckBlock` refuses (`bad-blk-length`).
#[derive(Clone, Default)]
struct Faulty {
    files: SharedFiles,
    unreadable: Arc<Mutex<Option<dash_primitives::BlockHash>>>,
    garbled: Arc<Mutex<Option<dash_primitives::BlockHash>>>,
    /// The blocks read, in order.
    reads: Arc<Mutex<Vec<dash_primitives::BlockHash>>>,
}

impl crate::chain::BlockFiles for Faulty {
    fn save_block(
        &mut self,
        block: &Arc<dash_consensus::Block>,
        height: u32,
        known: Option<dash_chain::FlatFilePos>,
    ) -> Result<Option<dash_chain::FlatFilePos>, String> {
        self.files.save_block(block, height, known)
    }

    fn read_block(
        &self,
        pos: dash_chain::FlatFilePos,
        hash: &dash_primitives::BlockHash,
    ) -> Result<Arc<dash_consensus::Block>, String> {
        self.reads.lock().unwrap().push(*hash);
        if *self.unreadable.lock().unwrap() == Some(*hash) {
            return Err(format!("no block at {pos:?}"));
        }
        let block = self.files.read_block(pos, hash)?;
        if *self.garbled.lock().unwrap() == Some(*hash) {
            return Ok(Arc::new(dash_consensus::Block::new(*block.header(), Vec::new())));
        }
        Ok(block)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.files.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.files.current_usage()
    }

    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String> {
        self.files.scan_already_pruned_files(have_pruned)
    }
}

/// VerifyDB's levels 0 and 1 over the last `depth` blocks (`validation.cpp:5079-5108` at v24.0.0):
/// an unreadable block fails level 0 and a block `CheckBlock` refuses fails level 1, inside the
/// depth only; levels 3 and 4 check nothing more (BL-524).
#[test]
fn verify_db_reads_and_checks_the_last_depth_blocks() {
    use VerifyDbResult::*;

    let faulty = Faulty::default();
    let mut rig = Rig::with(RigConfig {
        files: Some(Box::new(faulty.clone())),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    for m in &main {
        rig.accept(m);
    }
    let verify = |level, depth| {
        rig.v
            .verify_db(VerifyDbArgs {
                level,
                depth,
            })
            .recv()
            .unwrap()
    };
    for level in 0..=4 {
        assert_eq!(verify(level, 0), Success, "healthy at level {level}");
    }
    // The block at height 2: inside a depth of 3 or more (or 0, all), outside 2.
    let bad = main[1].hash();
    *faulty.unreadable.lock().unwrap() = Some(bad);
    for (level, depth, want) in
        [(0, 0, CorruptedBlockDb), (0, 3, CorruptedBlockDb), (0, 2, Success)]
    {
        assert_eq!(verify(level, depth), want, "unreadable, level {level}, depth {depth}");
    }
    *faulty.unreadable.lock().unwrap() = None;
    *faulty.garbled.lock().unwrap() = Some(bad);
    for (level, depth, want) in [
        (0, 0, Success),
        (1, 0, CorruptedBlockDb),
        (4, 3, CorruptedBlockDb),
        (4, 2, Success),
        (-7, 0, Success),
    ] {
        assert_eq!(verify(level, depth), want, "garbled, level {level}, depth {depth}");
    }
    rig.stop();
}

/// A shutdown requested during the walk stops it after the block it has just checked: the tip
/// is read, and VerifyDB answers `INTERRUPTED` (`validation.cpp:5140` at v24.0.0; r1 m-1).
#[test]
fn verify_db_stops_after_a_block_once_shutdown_is_requested() {
    let faulty = Faulty::default();
    let rig = Rig::with(RigConfig {
        files: Some(Box::new(faulty.clone())),
        ..RigConfig::default()
    });
    let main = branch(&rig.p, &rig.genesis, 4, 0);
    for m in &main {
        rig.accept(m);
    }
    faulty.reads.lock().unwrap().clear();
    let ran = Arc::new(Mutex::new(None));
    let r = ran.clone();
    rig.on_v(move |v| {
        v.request_shutdown();
        *r.lock().unwrap() = Some(v.verify_db(
            VerifyDbArgs {
                level: 0,
                depth: 0,
            },
            ScratchParent::Tip,
        )?);
        Ok(())
    });
    assert_eq!(*ran.lock().unwrap(), Some(VerifyDbResult::Interrupted));
    assert_eq!(*faulty.reads.lock().unwrap(), [main[3].hash()], "the tip alone");
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
/// DEC-992 (b) is pinned here: d3 is reset at level 3 only, and b2 keeps its mark at both. The
/// process tests cannot make the conflict through Core's path: a regtest ChainLock needs a
/// quorum's signature, and their nodes run no masternodes.
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
        let d3_failed = rig.status(&d3).has(Status::FAILED_MASK);
        assert_eq!(d3_failed, check_level < 3, "level {check_level}: the reset reached d3");
        let kept = rig.status(&side[1]).has(Status::CONFLICT_CHAINLOCK);
        assert!(kept, "level {check_level}: b2 keeps its mark");
        assert!(rig.stop().fatal.is_none(), "level {check_level}");
    }
}

/// D64's latch, as D51's for the coins: a second `VerifyLoaded`, even before init's ABC, is V's
/// `AbortNode`; the first is served and the second's reply disconnects. Init's ABC, sent once V
/// has stopped, is answered `Ok` (D241).
// Core: node/blockstorage.cpp:950-962; validation.cpp:3789-3791, :3823 at v24.0.0
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
        stops_at_init: true,
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
