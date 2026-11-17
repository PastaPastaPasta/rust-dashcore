//! O2b-4 4f: the X-check against real dashds ([`node_l1::chain::xcheck`]): stock v24.0.0 from
//! `O2B_XCHECK_STOCK` and the published hooked build from `O2B_XCHECK_HOOKED`. Either unset
//! fails every test: a missing binary is never a skip. Datadirs go under `O2B_XCHECK_ROOT` (else
//! the temporary directory), one 0700 directory per test, removed after; each test runs one dashd
//! at a time, so `--test-threads=4` keeps to the X-check's four.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use dash_oracle_sys::Pin;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::{Encoding, Program};
use node_l1::chain::run::{Mode, Runner};
use node_l1::chain::xcheck::compare::refused;
use node_l1::chain::xcheck::sample::{CONTEXTS, Executed, Minimums, project};
use node_l1::chain::xcheck::{
    Agreement, Binary, DISABLE_GOVERNANCE, Dashd, KEEP_DEBUG_LOG, Kind, Replay, SessionRun,
    build_base, compare_binaries, execute, governance_disabled, private_dir, replay,
    session_sample, spork,
};

/// One binary from its variable; unset or unreadable fails the test.
fn binary(var: &str, kind: Kind) -> Binary {
    let path = std::env::var_os(var).unwrap_or_else(|| {
        panic!(
            "{var} is unset: the X-check tests need both dashds (a missing one fails, never skips)"
        )
    });
    Binary::new(Path::new(&path), kind).unwrap_or_else(|e| panic!("{var}: {e}"))
}

fn binaries() -> [Binary; 2] {
    [binary("O2B_XCHECK_STOCK", Kind::Stock), binary("O2B_XCHECK_HOOKED", Kind::Hooked)]
}

fn work_dir() -> tempfile::TempDir {
    let root = std::env::var_os("O2B_XCHECK_ROOT").map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("l1_chain_xcheck")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(root)
        .unwrap()
}

fn runner(schedule: Schedule) -> Runner {
    let mut runner = Runner::with_mode(Pin::V24_0, schedule, Mode::Diff).unwrap();
    runner.encoding = Encoding::V2;
    runner.trace(true);
    runner
}

/// The session's run of `program`, which must map whole and run untainted.
fn session(runner: &mut Runner, program: &Program) -> SessionRun {
    match execute(runner, program).unwrap() {
        Executed::Ran(run, _) => run,
        Executed::Excluded(why) => panic!("an excluded op: {why}"),
        Executed::Tainted => panic!("tie-tainted"),
    }
}

/// Each binary's base for `schedule` under `dir`, and its replay of `run`.
fn on_both(dir: &Path, schedule: Schedule, label: &str, run: &SessionRun) -> [Replay; 2] {
    binaries().map(|b| {
        let base = dir.join(format!("base-{}", b.kind.name()));
        if !base.exists() {
            let (_, arguments) = build_base(&b, &base, schedule).unwrap();
            for flag in [DISABLE_GOVERNANCE, KEEP_DEBUG_LOG] {
                assert!(arguments.iter().any(|a| a == flag), "{flag}: {arguments:?}");
            }
        }
        let work = dir.join(format!("p-{}", b.kind.name()));
        replay(&b, &base, &work, &dir.join("log"), schedule, label, run).unwrap()
    })
}

/// Neither binary differs from the session, nor from each other; the agreement both gave.
fn agreed(schedule: Schedule, label: &str, replays: &[Replay; 2]) -> Agreement {
    let [stock, hooked] = replays;
    for r in replays {
        assert_eq!(r.disagreements, [], "{label}");
    }
    let mut agreement = Agreement::new();
    let between =
        compare_binaries(schedule, label, &stock.observed, &hooked.observed, &mut agreement);
    assert_eq!(between, [], "{label}: stock against hooked");
    for r in replays {
        for (k, (eq, ne)) in &r.agreement {
            let e = agreement.entry(*k).or_default();
            e.0 += eq;
            e.1 += ne;
        }
    }
    agreement
}

fn seed(name: &str) -> Program {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds").join(name);
    Program::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Equal comparisons of `plane` on `pair`, none differing.
fn equal(agreement: &Agreement, pair: &str, plane: &str) -> u64 {
    let (eq, ne) = agreement.get(&(pair, plane)).copied().unwrap_or_default();
    assert_eq!(ne, 0, "{pair} {plane}");
    eq
}

/// 4f test 8: the twelve spork assertions, each named: the session and `NodeSut` on S0's image,
/// and stock dashd on S0's base, each trial fresh.
#[test]
fn the_spork_experiment_holds_on_all_three_systems() {
    let dir = work_dir();
    let [stock, _] = binaries();
    let mut rows = spork::session_rows(&dir.path().join("session")).unwrap();
    let base = dir.path().join("base");
    build_base(&stock, &base, Schedule::S0).unwrap();
    rows.extend(spork::dashd_rows(&stock, &base, dir.path()).unwrap());
    for system in spork::SYSTEMS {
        for trial in spork::TRIALS {
            for check in spork::CHECKS {
                let row = rows
                    .iter()
                    .find(|r| {
                        r.system == system
                            && r.trial == spork::trial_name(trial)
                            && r.check == check
                    })
                    .unwrap_or_else(|| panic!("{system}/{trial:?}/{check}: missing"));
                assert!(row.passed(), "{row:?}");
            }
        }
    }
    assert_eq!(spork::verdict(&rows), Ok(()));
}

/// 4f test 9: on each schedule the image is the same on the session and both dashds: the restore
/// step's planes (tip, best header, tips, sporks) and, at the end, `gettxoutsetinfo`'s fields
/// with MuHash against `NodeSut`'s coins.
#[test]
fn the_image_prefix_is_the_same_on_the_session_and_both_dashds() {
    for schedule in [Schedule::S0, Schedule::S1, Schedule::S2] {
        let dir = work_dir();
        let run = session(&mut runner(schedule), &Program::parse("").unwrap());
        assert!(run.trace.is_empty() && run.start_muhash.is_some());
        let agreement = agreed(schedule, "image", &on_both(dir.path(), schedule, "image", &run));
        for pair in ["session-stock", "session-hooked"] {
            for plane in ["tip", "best_header", "tips", "sporks", "ibd"] {
                assert_eq!(equal(&agreement, pair, plane), 1, "{} {pair} {plane}", schedule.name());
            }
            for plane in ["utxo.txouts", "utxo.total_amount", "utxo.bestblock", "utxo.muhash"] {
                assert_eq!(equal(&agreement, pair, plane), 1, "{} {pair} {plane}", schedule.name());
            }
        }
    }
}

/// 4f test 10: `basic.prog`, projected to the ops dashd runs, replays the same everywhere: every
/// plane at every step.
#[test]
fn a_seed_replays_the_same_on_the_session_and_both_dashds() {
    let dir = work_dir();
    let program = project(&seed("basic.prog"));
    let run = session(&mut runner(Schedule::S0), &program);
    let agreement =
        agreed(Schedule::S0, "basic", &on_both(dir.path(), Schedule::S0, "basic", &run));
    let steps = run.trace.len() as u64;
    for pair in ["session-stock", "session-hooked"] {
        assert_eq!(equal(&agreement, pair, "reply"), steps);
        assert_eq!(equal(&agreement, pair, "tip"), steps + 1);
        assert!(equal(&agreement, pair, "utxo.muhash") >= 2, "a flush and the end");
    }
    assert_eq!(
        equal(&agreement, "stock-hooked", "reply"),
        steps + 2,
        "the restore, the ops, the end"
    );
}

/// 4f test 11: `refused_load.prog`'s refused load is dashd's reindex question (a `failure`
/// status), with Core's message as the session's restart reply has it; the program ends there
/// on every side. No test drives a real dashd's `InitError` refusal: no seed reaches one.
#[test]
fn a_refused_load_is_the_reindex_question_with_cores_message() {
    let dir = work_dir();
    let run = session(&mut runner(Schedule::S0), &seed("refused_load.prog"));
    let at = run
        .trace
        .iter()
        .position(|t| refused(&t.step.reply.outcome.clone().map_err(|e| (e.code, e.message))));
    let at = at.expect("the session refuses the load");
    let replays = on_both(dir.path(), Schedule::S0, "refused_load", &run);
    agreed(Schedule::S0, "refused_load", &replays);
    for r in &replays {
        assert_eq!(r.observed.len(), at + 2, "the restore and the ops through the refusal");
        let answer = r.observed.last().unwrap().answer.clone().unwrap().unwrap();
        assert_eq!(answer["status"], "failure");
        let session = run.trace[at].step.reply.outcome.clone().unwrap();
        assert_eq!(answer["message"], session["message"]);
        assert!(!answer["message"].as_str().unwrap().is_empty());
    }
}

/// 4f test 12: the killed and the clean restarts of `kill_unflushed.prog` and
/// `invalidate_restart.prog` match on every plane.
#[test]
fn kill_and_clean_restarts_match() {
    for name in ["kill_unflushed.prog", "invalidate_restart.prog"] {
        let dir = work_dir();
        let run = session(&mut runner(Schedule::S0), &seed(name));
        let restarts = run
            .trace
            .iter()
            .filter(|t| matches!(t.op, dash_oracle_sys::ChainOp::Restart(_)))
            .count();
        assert!(restarts > 0, "{name}");
        let agreement = agreed(Schedule::S0, name, &on_both(dir.path(), Schedule::S0, name, &run));
        assert_eq!(equal(&agreement, "session-stock", "reply"), run.trace.len() as u64, "{name}");
    }
}

/// 4f test 13: stock and hooked give the same outcomes on shared programs; one side's recorded
/// reply perturbed (test-only) surfaces as an undisposed stock-hooked disagreement, never
/// resolved by taking either binary.
#[test]
fn stock_and_hooked_outcomes_agree_on_shared_programs() {
    let dir = work_dir();
    let mut runner = runner(Schedule::S0);
    for name in ["proposals.prog", "reorg.prog"] {
        let program = project(&seed(name));
        let run = session(&mut runner, &program);
        let sub = dir.path().join(name);
        node_l1::chain::xcheck::private_dir(&sub).unwrap();
        let replays = on_both(&sub, Schedule::S0, name, &run);
        agreed(Schedule::S0, name, &replays);
        let [stock, mut hooked] = replays;
        let step = hooked.observed.iter().position(|o| o.answer.is_some()).expect("an op");
        hooked.observed[step].answer = Some(Ok(serde_json::json!("perturbed")));
        let mut agreement = Agreement::new();
        let found =
            compare_binaries(Schedule::S0, name, &stock.observed, &hooked.observed, &mut agreement);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            (found[0].pair, found[0].plane, found[0].disposition),
            ("stock-hooked", "reply", None)
        );
        assert_eq!(agreement[&("stock-hooked", "reply")].1, 1);
    }
}

/// 4f test 14: the hooked build's C/R/D and A records, read from its O8 logs, compare strictly
/// with the session's at every step.
#[test]
fn the_hooked_records_compare_in_stream_mode() {
    let dir = work_dir();
    let run = session(&mut runner(Schedule::S0), &project(&seed("basic.prog")));
    let agreement =
        agreed(Schedule::S0, "basic", &on_both(dir.path(), Schedule::S0, "basic", &run));
    for plane in ["records", "attempts", "sparse records"] {
        assert_eq!(
            equal(&agreement, "session-hooked", plane),
            run.trace.len() as u64 + 1,
            "{plane}"
        );
        assert!(!agreement.contains_key(&("session-stock", plane)), "stock has no O8 logs");
    }
}

/// 4f test 15: every proposal context is executed on dashd and compared: sampled S0 programs
/// covering the seven, each replayed on both binaries with every plane equal.
#[test]
fn every_proposal_context_reaches_dashd() {
    let min = Minimums {
        programs: 1,
        per_context: 1,
    };
    let (sampled, _) = session_sample(Schedule::S0, min, 5_000).unwrap();
    let mut covered = BTreeMap::new();
    let mut chosen = Vec::new();
    for s in &sampled {
        if s.contexts.iter().any(|c| !covered.contains_key(c)) {
            for c in &s.contexts {
                *covered.entry(*c).or_insert(0) += 1;
            }
            chosen.push(s);
        }
    }
    assert_eq!(covered.len(), CONTEXTS.len(), "{covered:?}");
    let dir = work_dir();
    for s in chosen {
        let agreement =
            agreed(Schedule::S0, &s.label, &on_both(dir.path(), Schedule::S0, &s.label, &s.run));
        assert_eq!(
            equal(&agreement, "session-stock", "reply"),
            s.run.trace.len() as u64,
            "{}",
            s.label
        );
    }
}

/// The governance start guard (OD-L3-113) on a real stock dashd: a start without
/// `-disablegovernance=1` fails it, naming the binary; one with it passes, read from that start's
/// lines only.
#[test]
fn the_governance_guard_needs_the_init_warning() {
    use node_l1::chain::core::T_RECENT;
    use node_l1::l3::dashd::{Process, Started};
    let dir = work_dir();
    let [stock, _] = binaries();
    let log = dir.path().join("regtest/debug.log");
    let mut args: Vec<String> = Schedule::S0.args().into_iter().map(str::to_owned).collect();
    let start = |args: &[String]| match Process::start_once(&stock.path, dir.path(), args, T_RECENT)
        .unwrap()
    {
        Started::Up(process) => process,
        Started::Refused(r) => panic!("{r:?}"),
    };
    start(&args).stop().unwrap();
    let err = governance_disabled(Kind::Stock, &log, 0).unwrap_err();
    assert!(err.starts_with("stock dashd did not warn"), "{err}");
    let from = std::fs::metadata(&log).unwrap().len();
    args.push(DISABLE_GOVERNANCE.to_owned());
    start(&args).stop().unwrap();
    governance_disabled(Kind::Stock, &log, from).unwrap();
}

/// The guard's wiring (OD-L3-113 (3)): a `Dashd` whose arguments lack `-disablegovernance=1`
/// fails `start()` with the guard's error and is left down.
#[test]
fn a_dashd_without_the_flag_fails_its_start() {
    let dir = work_dir();
    let [stock, _] = binaries();
    let datadir = dir.path().join("node");
    private_dir(&datadir).unwrap();
    let mut node = Dashd::new(&stock, &datadir, Schedule::S0, node_l1::chain::core::T_RECENT)
        .without_arg(DISABLE_GOVERNANCE);
    let err = node.start().unwrap_err();
    assert!(err.starts_with("stock dashd did not warn"), "{err}");
    assert!(!node.up());
}
