//! `st3b-crash`: the ST3b power-loss harness (see `docs/node-program/designs/ST3b-notes.md`).
//!
//! ```text
//! st3b-crash selftest [--fs ext4|xfs] [--seed N] [--rand N] [--torn N] [--split N] [--fault panic|pause]
//! st3b-crash sweep (--stale | --stale-vm | <run>)
//! st3b-crash leaks [--vm] [<run>]
//! st3b-crash baseline <fixture dir> --rdashd PATH --dashd PATH [--every N] [-- <rdashd args>]
//! st3b-crash c6 <fixture dir> <image> --rdashd PATH --dashd PATH --point I --floor ack:N
//! st3b-crash fault <fixture dir> --case FL-IBD|FL-FLUSH|FL-WAL|DF-PRE|DF-MID --rdashd PATH
//!     --dashd PATH [--fs ext4|xfs] [--seed N] [--every N] [-- <rdashd args>]
//! st3b-crash vm <fixture dir> --scenario V-IBD|V-REORG|V-FLUSH|V-RECOVERY|V-REINDEX --fs ext4|xfs
//!     --kills N --seed S --dashd PATH --guest DIR [--every N] [-- <rdashd args>]
//! st3b-crash kill-smoke <fixture dir> --rdashd PATH --dashd PATH [--kills N] [--seed S]
//!     [--every N] [-- <rdashd args>]
//! ```
//!
//! `vm`'s seed fixes the sequence of kill plans (random times, trigger skips and jitter), not the
//! kill points: how many plans a run draws, and where each kill lands after its line, follow the
//! box's timing.
//!
//! Every run ends with a leak check of its own devices and mounts (the VM level: its containers),
//! and fails loudly if any remain. `sweep --stale` and `leaks` cover the containers labelled
//! `st3b.run` too.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("st3b-crash: Linux only (device-mapper and loop devices)");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::{SystemTime, UNIX_EPOCH};

    use dash_testkit::crash::baseline::Report;
    use dash_testkit::crash::campaign;
    use dash_testkit::crash::checks::{self, Floor};
    use dash_testkit::crash::devices::{
        Fs, Priv, ROOT, Run, Stale, leaks, private_dir, sweep_stale,
    };
    use dash_testkit::crash::faults::{self, Case};
    use dash_testkit::crash::fixture::Fixture;
    use dash_testkit::crash::inspect::Inspection;
    use dash_testkit::crash::kill_smoke;
    use dash_testkit::crash::logwrites::Draw;
    use dash_testkit::crash::selftest::{self, Fault};
    use dash_testkit::crash::vm;
    use dash_testkit::crash::vm::scenario::{self, Scenario};

    fn usage() -> ExitCode {
        eprintln!(
            "usage: st3b-crash selftest [--fs ext4|xfs] [--seed N] [--rand N] [--torn N] [--split N] [--fault panic|pause]\n\
             \x20      st3b-crash sweep (--stale | --stale-vm | <run>)\n\
             \x20      st3b-crash leaks [--vm] [<run>]\n\
         \x20      st3b-crash baseline <fixture dir> --rdashd PATH --dashd PATH [--every N] \
         [--min-partial N] [-- <rdashd args>]\n\
         \x20      st3b-crash inspect <datadir>\n\
         \x20      st3b-crash check <fixture dir> <datadir> --rdashd PATH [--dashd PATH] \
         [--floor start|ack:N|stopped:N] [--seed N] [-- <rdashd args>]\n\
         \x20      st3b-crash campaign <fixture dir> --rdashd PATH --dashd PATH [--fs ext4|xfs] \
         [--seed N] [--rand N] [--torn N] [--split N] [--every N] [--c5-every N] [--c6-every N] [--shard I/N] \
         [--points A-B] [--split-chainstate] [--plant BUG] [--torn-only] [--c0-only] [--sample N] \
         [--c6-mid-every N] [--c6-max N] [--dry-run] [-- <rdashd args>]\n\
         \x20      st3b-crash c6 <fixture dir> <image> --rdashd PATH --dashd PATH --point I \
         --floor start|ack:N|stopped:N [--fs ext4|xfs] [--seed N] [-- <rdashd args>]\n\
         \x20      st3b-crash fault <fixture dir> --case FL-IBD|FL-FLUSH|FL-WAL|DF-PRE|DF-MID \
         --rdashd PATH --dashd PATH [--fs ext4|xfs] [--seed N] [--every N] [-- <rdashd args>]\n\
         \x20      st3b-crash vm <fixture dir> --scenario V-IBD|V-REORG|V-FLUSH|V-RECOVERY|V-REINDEX \
         --fs ext4|xfs --kills N --seed S --dashd PATH --guest DIR [--every N] [-- <rdashd args>]\n\
         \x20        (vm's --seed fixes the sequence of kill plans, not the kill points)\n\
         \x20      st3b-crash kill-smoke <fixture dir> --rdashd PATH --dashd PATH [--kills N] [--seed S] \
         [--every N] [-- <rdashd args>]"
        );
        ExitCode::from(2)
    }

    /// A verb's own arguments, and the arguments for rdashd after `--`.
    fn split_node_args(args: &[String]) -> (&[String], &[String]) {
        match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[][..]),
        }
    }

    /// Seconds since the Unix epoch.
    fn epoch_secs() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    fn current_thread_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime")
    }

    /// The fixture in `dir`; `None` after the message for verb `what` (the verb fails).
    fn load_fixture(what: &str, dir: &str) -> Option<Fixture> {
        Fixture::load(Path::new(dir))
            .inspect_err(|e| eprintln!("st3b {what}: loading {dir}: {e}"))
            .ok()
    }

    /// The run `started`; `None` after the message (the verb fails).
    fn started_run(started: std::io::Result<Run>) -> Option<Run> {
        started.inspect_err(|e| eprintln!("st3b: cannot start a run: {e}")).ok()
    }

    /// The end of a verb that ran in `run`: a failed one keeps the run directory for the repro, a
    /// passing one ends the run ([`Run::finish`]).
    fn finish_run(run: Run, ok: bool) -> ExitCode {
        if !ok {
            eprintln!("kept {} for the repro", run.dir().display());
            return ExitCode::FAILURE;
        }
        match run.finish() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("st3b: cleaning up the run: {e}");
                ExitCode::FAILURE
            }
        }
    }

    /// A kill's cycle time for a report line.
    fn cycle_text(cycle: Option<std::time::Duration>) -> String {
        cycle.map_or("-".to_owned(), |c| format!("{:.1} s", c.as_secs_f64()))
    }

    /// Reports `run`'s leftovers; true when there are none.
    fn leak_check(p: &Priv, run: &str) -> bool {
        report_leaks(run, leaks(p, Some(run)), "device(s) or mount(s)")
    }

    /// Reports the leftovers `left` of run `run`, `what` naming their kind; true when there are none.
    fn report_leaks(run: &str, left: std::io::Result<Vec<String>>, what: &str) -> bool {
        match left {
            Ok(left) if left.is_empty() => {
                println!("leak check: clean");
                true
            }
            Ok(left) => {
                eprintln!("LEAK: run {run} left {} {what}:", left.len());
                for l in left {
                    eprintln!("  {l}");
                }
                false
            }
            Err(e) => {
                eprintln!("LEAK CHECK FAILED: {e}");
                false
            }
        }
    }

    fn selftest(p: &Priv, args: &[String]) -> ExitCode {
        let mut fs = Fs::Ext4;
        let mut fault = Fault::None;
        let mut draw = Draw {
            rand: 16,
            torn: 16,
            seed: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64),
            split: 16,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let Some(v) = it.next() else {
                return usage();
            };
            match (a.as_str(), v.as_str()) {
                ("--fs", v) if Fs::parse(v).is_some() => fs = Fs::parse(v).expect("checked"),
                ("--seed", v) if v.parse::<u64>().is_ok() => {
                    draw.seed = v.parse().expect("checked")
                }
                ("--rand", v) if v.parse::<usize>().is_ok() => {
                    draw.rand = v.parse().expect("checked")
                }
                ("--torn", v) if v.parse::<usize>().is_ok() => {
                    draw.torn = v.parse().expect("checked")
                }
                ("--split", v) if v.parse::<usize>().is_ok() => {
                    draw.split = v.parse().expect("checked")
                }
                ("--fault", "panic") => fault = Fault::Panic,
                ("--fault", "pause") => fault = Fault::Pause,
                _ => return usage(),
            }
        }
        let Some(run) = started_run(Run::start(p)) else {
            return ExitCode::FAILURE;
        };
        println!(
            "run {} fs {} seed {} rand {} torn {} split {}",
            run.id(),
            fs.name(),
            draw.seed,
            draw.rand,
            draw.torn,
            draw.split
        );
        let result = catch_unwind(AssertUnwindSafe(|| selftest::run(p, &run, fs, fault, draw)));
        let mut ok = match result {
            Ok(Ok(report)) => {
                println!(
                    "log entries {}, points {} {:?}",
                    report.entries, report.points, report.by_kind
                );
                for f in &report.failures {
                    eprintln!("FAIL {f}");
                }
                println!(
                    "{} of {} points passed",
                    report.points.saturating_sub(report.failures.len()),
                    report.points
                );
                report.failures.is_empty()
            }
            Ok(Err(e)) => {
                eprintln!("st3b selftest: {e}");
                false
            }
            Err(_) => {
                eprintln!("st3b selftest: panicked");
                false
            }
        };
        ok &= leak_check(p, run.id());
        finish_run(run, ok)
    }

    /// Prints a campaign's report; success when it has no failures and checked a point.
    fn campaign_report(result: std::io::Result<campaign::Report>, what: &str) -> bool {
        match result {
            Ok(report) => {
                println!(
                    "log entries {}, points {} {:?}, phases {:?}, C1-C4 on {}, C5 runs {}, C6 runs {} \
                     ({} points), C6 skipped {}, C0 {:.2} CPU-s a point",
                    report.entries,
                    report.points,
                    report.by_kind,
                    report.phases,
                    report.full,
                    report.c5_runs,
                    report.c6_runs,
                    report.c6_points,
                    report.c6_skipped,
                    report.c0_cpu.as_secs_f64() / report.points.max(1) as f64
                );
                for f in &report.failures {
                    eprintln!("FAIL {f}");
                }
                report.failures.is_empty() && report.points > 0
            }
            Err(e) => {
                eprintln!("st3b {what}: {e}");
                false
            }
        }
    }

    /// `campaign --dry-run`: no devices, no sudo (`campaign::dry_run`).
    fn dry_run(dir: &str, fixture: &Fixture, config: &campaign::Config) -> ExitCode {
        let work = match work_dir("dryrun") {
            Ok(w) => w,
            Err(e) => {
                eprintln!("st3b dry run: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "dry run {dir} in {} every {} extra {:?}",
            work.display(),
            config.every,
            config.extra
        );
        let runtime = current_thread_runtime();
        let result = campaign::dry_run(&work, &runtime, fixture, config);
        if campaign_report(result, "dry run") {
            ExitCode::SUCCESS
        } else {
            eprintln!("kept {}", work.display());
            ExitCode::FAILURE
        }
    }

    /// Records a fixture's schedule over log-writes and checks its points (C0-C6).
    fn campaign(p: &Priv, args: &[String]) -> ExitCode {
        let (ours, node_extra) = split_node_args(args);
        let Some((dir, rest)) = ours.split_first() else {
            return usage();
        };
        let flag = |f: &str| rest.iter().any(|a| a == f);
        let (dry, split_chainstate, torn_only, c0_only) =
            (flag("--dry-run"), flag("--split-chainstate"), flag("--torn-only"), flag("--c0-only"));
        let rest: Vec<&String> = rest
            .iter()
            .filter(|a| {
                !["--dry-run", "--split-chainstate", "--torn-only", "--c0-only"]
                    .contains(&a.as_str())
            })
            .collect();
        let days = epoch_secs() / 86400;
        let (mut rdashd, mut dashd, mut plant) = (None, None, None);
        let mut cfg = (
            Fs::Ext4,
            Draw {
                rand: 16,
                torn: 16,
                seed: days,
                split: 16,
            },
            100,
            10,
            10,
            (0, 1),
            0..=usize::MAX,
            1,
            1,
            usize::MAX,
        );
        let mut it = rest.into_iter();
        while let Some(a) = it.next() {
            let Some(v) = it.next() else {
                return usage();
            };
            let n = v.parse::<u64>().ok();
            match (a.as_str(), n) {
                ("--rdashd", _) => rdashd = Some(PathBuf::from(v)),
                ("--dashd", _) => dashd = Some(PathBuf::from(v)),
                ("--plant", _) => plant = Some(v.clone()),
                ("--fs", _) if Fs::parse(v).is_some() => cfg.0 = Fs::parse(v).expect("checked"),
                ("--seed", Some(n)) => cfg.1.seed = n,
                ("--rand", Some(n)) => cfg.1.rand = n as usize,
                ("--torn", Some(n)) => cfg.1.torn = n as usize,
                ("--split", Some(n)) => cfg.1.split = n as usize,
                ("--every", Some(n)) if n > 0 => cfg.2 = n as usize,
                ("--c5-every", Some(n)) if n > 0 => cfg.3 = n as usize,
                ("--c6-every", Some(n)) if n > 0 => cfg.4 = n as usize,
                ("--shard", _) => match v.split_once('/').map(|(a, b)| (a.parse(), b.parse())) {
                    Some((Ok(i), Ok(n))) if i < n => cfg.5 = (i, n),
                    _ => return usage(),
                },
                ("--points", _) => match v.split_once('-').map(|(a, b)| (a.parse(), b.parse())) {
                    Some((Ok(a), Ok(b))) if a <= b => cfg.6 = a..=b,
                    _ => return usage(),
                },
                ("--sample", Some(n)) if n > 0 => cfg.7 = n as usize,
                ("--c6-mid-every", Some(n)) if n > 0 => cfg.8 = n as usize,
                ("--c6-max", Some(n)) => cfg.9 = n as usize,
                _ => return usage(),
            }
        }
        let (Some(rdashd), Some(dashd)) = (rdashd, dashd) else {
            return usage();
        };
        let (fs, draw, every, c5_every, c6_every, shard, points, sample, c6_mid_every, c6_max) =
            cfg;
        let config = campaign::Config {
            fs,
            draw,
            rdashd,
            dashd,
            extra: node_extra.to_vec(),
            every,
            c5_every,
            c6_every,
            c6_mid_every,
            c6_max,
            shard,
            points,
            split_chainstate,
            plant,
            torn_only,
            c0_only,
            sample,
        };
        let Some(fixture) = load_fixture("campaign", dir) else {
            return ExitCode::FAILURE;
        };
        if dry {
            return dry_run(dir, &fixture, &config);
        }
        let Some(run) = started_run(Run::start(p)) else {
            return ExitCode::FAILURE;
        };
        println!(
            "run {} campaign {dir} fs {} seed {} rand {} torn {} split {} every {every} c5 \
             1/{c5_every} sample 1/{sample} c6 marker 1/{c6_mid_every} c6 max {} extra {:?} plant {:?}",
            run.id(),
            fs.name(),
            draw.seed,
            draw.rand,
            draw.torn,
            draw.split,
            if c6_max == usize::MAX {
                "none".to_owned()
            } else {
                c6_max.to_string()
            },
            config.extra,
            config.plant
        );
        let runtime = current_thread_runtime();
        let result =
            catch_unwind(AssertUnwindSafe(|| campaign::run(p, &run, &runtime, &fixture, &config)));
        let mut ok = match result {
            Ok(r) => campaign_report(r, "campaign"),
            Err(_) => {
                eprintln!("st3b campaign: panicked");
                false
            }
        };
        ok &= leak_check(p, run.id());
        finish_run(run, ok)
    }

    /// Syncs a fixture on plain disk (no devices, no sudo), C2 at every acknowledgement.
    fn baseline(args: &[String]) -> ExitCode {
        let (ours, node_extra) = split_node_args(args);
        let Some((dir, rest)) = ours.split_first() else {
            return usage();
        };
        let (mut rdashd, mut dashd, mut every, mut min_partial) = (None, None, 100, 0);
        let mut it = rest.iter();
        while let Some(a) = it.next() {
            let Some(v) = it.next() else {
                return usage();
            };
            match a.as_str() {
                "--rdashd" => rdashd = Some(PathBuf::from(v)),
                "--dashd" => dashd = Some(PathBuf::from(v)),
                "--every" if v.parse::<usize>().is_ok_and(|n| n > 0) => {
                    every = v.parse().expect("checked")
                }
                "--min-partial" if v.parse::<usize>().is_ok() => {
                    min_partial = v.parse().expect("checked")
                }
                _ => return usage(),
            }
        }
        let (Some(rdashd), Some(dashd)) = (rdashd, dashd) else {
            return usage();
        };
        let Some(fixture) = load_fixture("baseline", dir) else {
            return ExitCode::FAILURE;
        };
        let work = match work_dir("baseline") {
            Ok(w) => w,
            Err(e) => {
                eprintln!("st3b baseline: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "baseline {} in {}: {} rows, P2P to {}, then {} submitted, every {every}",
            dir,
            work.display(),
            fixture.rows.len(),
            fixture.p2p_tip,
            fixture.submit.len()
        );
        let runtime = current_thread_runtime();
        let started = std::time::Instant::now();
        let result = runtime.block_on(dash_testkit::crash::baseline::run(
            &rdashd,
            &dashd,
            &fixture,
            &work,
            &work.join("rdashd"),
            0,
            node_extra,
            None,
            every,
            &mut |_| Ok(()),
        ));
        // A run meant to split its flushes (D-IBD-S) asks for at least one partial batch.
        let result = result.map(|mut r| {
            if r.partial_batches < min_partial {
                r.failures.push(format!(
                    "{} partial coins batches, fewer than --min-partial {min_partial}",
                    r.partial_batches
                ));
            }
            r
        });
        report_run("baseline", result, &work, started)
    }

    /// Prints a schedule run's report; success when it has no failures.
    fn report_run(
        what: &str,
        result: std::io::Result<Report>,
        work: &Path,
        started: std::time::Instant,
    ) -> ExitCode {
        match result {
            Ok(report) => {
                for f in &report.failures {
                    eprintln!("FAIL {f}");
                }
                println!(
                    "{what}: {} acknowledgements, tip {}, {} partial batches, {} failures, {:.0} s",
                    report.acks.len(),
                    report.tip,
                    report.partial_batches,
                    report.failures.len(),
                    started.elapsed().as_secs_f64()
                );
                if report.failures.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("st3b {what}: {e} (logs in {})", work.display());
                ExitCode::FAILURE
            }
        }
    }

    /// A new private directory `ROOT/<kind>/<secs>-<pid>`.
    fn work_dir(kind: &str) -> Result<PathBuf, String> {
        let work =
            Path::new(ROOT).join(kind).join(format!("{}-{}", epoch_secs(), std::process::id()));
        private_dir(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        Ok(work)
    }

    /// A reflinked copy of the directory `from` at `to`.
    fn reflink_dir(from: &Path, to: &Path) -> Result<(), String> {
        let copied = Command::new("cp").args(["-a", "--reflink=always"]).arg(from).arg(to).status();
        if copied.is_ok_and(|s| s.success()) {
            Ok(())
        } else {
            Err(format!("copying {} failed", from.display()))
        }
    }

    /// C0 on a reflinked copy of `datadir` (opening a chainstate replays its WAL, which writes),
    /// with I1 against `fixture`'s blocks if one is given; its failures are printed.
    fn c0(datadir: &Path, copy: &Path, fixture: Option<&Fixture>) -> Result<Inspection, String> {
        reflink_dir(datadir, copy)?;
        let i = dash_testkit::crash::inspect::inspect(copy, fixture)?;
        for f in &i.failures {
            eprintln!("FAIL C0 {f}");
        }
        println!(
            "C0 {:?}: {} records, {} with data, {} failures",
            i.keys,
            i.records,
            i.with_data.len(),
            i.failures.len()
        );
        Ok(i)
    }

    fn inspect(datadir: &str) -> ExitCode {
        let result = work_dir("inspect").and_then(|work| {
            println!("copy in {}", work.display());
            c0(Path::new(datadir), &work.join("datadir"), None)
        });
        match result {
            Ok(i) if i.failures.is_empty() => ExitCode::SUCCESS,
            Ok(_) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("st3b inspect: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn parse_floor(v: &str) -> Option<Floor> {
        match v.split_once(':') {
            None if v == "start" => Some(Floor::Start),
            Some(("ack", n)) => n.parse().ok().map(Floor::Ack),
            Some(("stopped", n)) => n.parse().ok().map(Floor::Stopped),
            _ => None,
        }
    }

    /// C0–C4 on reflinked copies of a node's `datadir` (a crash image, or a killed or stopped node
    /// on plain disk), and C5 from the recovered tip with `--dashd`.
    fn check(args: &[String]) -> ExitCode {
        let (ours, node_extra) = split_node_args(args);
        let [dir, datadir, rest @ ..] = ours else {
            return usage();
        };
        let (mut rdashd, mut dashd, mut floor, mut seed) = (None, None, Floor::Start, 0);
        let mut it = rest.iter();
        while let Some(a) = it.next() {
            let Some(v) = it.next() else {
                return usage();
            };
            match (a.as_str(), parse_floor(v), v.parse::<u64>()) {
                ("--rdashd", ..) => rdashd = Some(PathBuf::from(v)),
                ("--dashd", ..) => dashd = Some(PathBuf::from(v)),
                ("--floor", Some(f), _) => floor = f,
                ("--seed", _, Ok(n)) => seed = n,
                _ => return usage(),
            }
        }
        let Some(rdashd) = rdashd else {
            return usage();
        };
        let Some(fixture) = load_fixture("check", dir) else {
            return ExitCode::FAILURE;
        };
        let work = match work_dir("check") {
            Ok(w) => w,
            Err(e) => {
                eprintln!("st3b check: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!("check {datadir} against {dir} in {}, floor {floor:?}", work.display());
        let stored = match c0(Path::new(datadir), &work.join("c0"), Some(&fixture)) {
            Ok(i) if i.failures.is_empty() => i.with_data,
            Ok(_) => return ExitCode::FAILURE,
            Err(e) => {
                eprintln!("st3b check: C0: {e}");
                return ExitCode::FAILURE;
            }
        };
        let node_dir = work.join("node");
        if let Err(e) = reflink_dir(Path::new(datadir), &node_dir) {
            eprintln!("st3b check: {e}");
            return ExitCode::FAILURE;
        }
        let runtime = current_thread_runtime();
        let point = runtime.block_on(checks::check_point(
            &rdashd, &fixture, &node_dir, node_extra, floor, &stored, seed,
        ));
        for f in &point.failures {
            eprintln!("FAIL {f}");
        }
        println!("C1-C4: tip {} at {:?}, {} failures", point.tip, point.pos, point.failures.len());
        let (Some(dashd), Some(pos), true) = (dashd, point.pos, point.failures.is_empty()) else {
            return if point.failures.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        };
        let started = std::time::Instant::now();
        let result = runtime.block_on(dash_testkit::crash::baseline::run(
            &rdashd,
            &dashd,
            &fixture,
            &work,
            &node_dir,
            pos,
            node_extra,
            None,
            100,
            &mut |_| Ok(()),
        ));
        report_run("C5", result, &work, started)
    }

    /// C6 alone on a kept first-crash image (`campaign::double_crash_image`): a C6 failure rerun
    /// on a new binary, with the campaign's floor and seed for the point.
    fn c6(p: &Priv, args: &[String]) -> ExitCode {
        let (ours, node_extra) = split_node_args(args);
        let [dir, img, rest @ ..] = ours else {
            return usage();
        };
        let (mut rdashd, mut dashd, mut point, mut floor) = (None, None, None, None);
        let (mut fs, mut seed) = (Fs::Ext4, 0);
        let mut it = rest.iter();
        while let Some(a) = it.next() {
            let Some(v) = it.next() else {
                return usage();
            };
            match (a.as_str(), v.parse::<u64>()) {
                ("--rdashd", _) => rdashd = Some(PathBuf::from(v)),
                ("--dashd", _) => dashd = Some(PathBuf::from(v)),
                ("--point", Ok(n)) => point = Some(n as usize),
                ("--floor", _) if parse_floor(v).is_some() => floor = parse_floor(v),
                ("--fs", _) if Fs::parse(v).is_some() => fs = Fs::parse(v).expect("checked"),
                ("--seed", Ok(n)) => seed = n,
                _ => return usage(),
            }
        }
        let (Some(rdashd), Some(dashd), Some(i), Some(floor)) = (rdashd, dashd, point, floor)
        else {
            return usage();
        };
        let config = campaign::Config {
            fs,
            draw: Draw {
                seed,
                ..Draw::default()
            },
            rdashd,
            dashd,
            extra: node_extra.to_vec(),
            every: 100,
            c5_every: usize::MAX,
            c6_every: 1,
            c6_mid_every: 1,
            c6_max: usize::MAX,
            shard: (0, 1),
            points: i..=i,
            split_chainstate: false,
            plant: None,
            torn_only: false,
            c0_only: false,
            sample: 1,
        };
        let Some(fixture) = load_fixture("c6", dir) else {
            return ExitCode::FAILURE;
        };
        let Some(run) = started_run(Run::start(p)) else {
            return ExitCode::FAILURE;
        };
        println!(
            "run {} c6 {img} point {i} floor {floor:?} fs {} seed {seed} extra {:?}",
            run.id(),
            fs.name(),
            config.extra
        );
        let runtime = current_thread_runtime();
        let result = catch_unwind(AssertUnwindSafe(|| {
            campaign::double_crash_image(
                p,
                &run,
                &runtime,
                &fixture,
                &config,
                Path::new(img),
                i,
                floor,
            )
        }));
        let mut ok = match result {
            Ok(r) => campaign_report(r, "c6"),
            Err(_) => {
                eprintln!("st3b c6: panicked");
                false
            }
        };
        ok &= leak_check(p, run.id());
        finish_run(run, ok)
    }

    /// Coverage attempts per case (S6 plan §3b): seeds `seed`, `seed + 1`, ...
    const ATTEMPTS: u64 = 3;

    /// One fault case (S6): attempts with successive seeds until one meets the case's condition,
    /// each in its own run. A behaviour failure ends it at once (never retried away), and so does a
    /// leak; three misses fail coverage.
    fn fault(p: &Priv, args: &[String]) -> ExitCode {
        let (ours, node_extra) = split_node_args(args);
        let Some((dir, rest)) = ours.split_first() else {
            return usage();
        };
        let days = epoch_secs() / 86400;
        let (mut case, mut rdashd, mut dashd) = (None, None, None);
        let (mut fs, mut seed, mut every) = (Fs::Ext4, days, 100);
        for pair in rest.chunks(2) {
            let [a, v] = pair else {
                return usage();
            };
            let n = v.parse::<u64>().ok();
            match (a.as_str(), n) {
                ("--case", _) if Case::parse(v).is_some() => case = Case::parse(v),
                ("--rdashd", _) => rdashd = Some(PathBuf::from(v)),
                ("--dashd", _) => dashd = Some(PathBuf::from(v)),
                ("--fs", _) if Fs::parse(v).is_some() => fs = Fs::parse(v).expect("checked"),
                ("--seed", Some(n)) => seed = n,
                ("--every", Some(n)) if n > 0 => every = n as usize,
                _ => return usage(),
            }
        }
        let (Some(case), Some(rdashd), Some(dashd)) = (case, rdashd, dashd) else {
            return usage();
        };
        let Some(fixture) = load_fixture("fault", dir) else {
            return ExitCode::FAILURE;
        };
        let runtime = current_thread_runtime();
        for attempt in 0..ATTEMPTS {
            let config = faults::Config {
                case,
                fs,
                rdashd: rdashd.clone(),
                dashd: dashd.clone(),
                extra: node_extra.to_vec(),
                every,
                seed: seed + attempt,
            };
            let Some(run) = started_run(Run::start(p)) else {
                return ExitCode::FAILURE;
            };
            println!(
                "run {} fault {case} fs {} seed {} every {every} extra {:?} (attempt {} of \
                 {ATTEMPTS})",
                run.id(),
                fs.name(),
                config.seed,
                config.extra,
                attempt + 1
            );
            let result = catch_unwind(AssertUnwindSafe(|| {
                faults::run(p, &run, &runtime, &fixture, &config)
            }));
            let (mut ok, miss) = match result {
                Ok(Ok(report)) => {
                    println!(
                        "trigger: {}\nfirst fatal failure: {}\nabort: {:?} (+{} repeats)\n\
                         floor: {:?}",
                        report.trigger,
                        report.first_failure.as_deref().unwrap_or("none"),
                        report.abort,
                        report.repeats,
                        report.floor
                    );
                    for f in &report.set_aside {
                        eprintln!("SET ASIDE {f}");
                    }
                    for f in &report.failures {
                        eprintln!("FAIL {f}");
                    }
                    (report.failures.is_empty(), report.miss)
                }
                Ok(Err(e)) => {
                    eprintln!("st3b fault: {e}");
                    (false, None)
                }
                Err(_) => {
                    eprintln!("st3b fault: panicked");
                    (false, None)
                }
            };
            ok &= leak_check(p, run.id());
            if !ok {
                eprintln!("kept {} for the repro", run.dir().display());
                eprintln!("{case} {}: FAILED", fs.name());
                return ExitCode::FAILURE;
            }
            if let Err(e) = run.finish() {
                eprintln!("st3b: cleaning up the run: {e}");
                return ExitCode::FAILURE;
            }
            match miss {
                None => {
                    println!("{case} {}: PASS (attempt {})", fs.name(), attempt + 1);
                    return ExitCode::SUCCESS;
                }
                Some(m) => println!("coverage: {m}"),
            }
        }
        eprintln!("{case} {}: FAILED coverage: {ATTEMPTS} attempts missed", fs.name());
        ExitCode::FAILURE
    }

    /// A stale sweep's outcome: what it swept, or, when anything was not swept, the error naming
    /// each (what it did sweep printed first).
    fn stale_result(stale: Stale) -> std::io::Result<Vec<String>> {
        if stale.failed.is_empty() {
            return Ok(stale.swept);
        }
        stale.swept.iter().for_each(|s| println!("{s}"));
        Err(std::io::Error::other(stale.failed.join("; ")))
    }

    /// Our devices and mounts (through the wrapper) and our containers: all runs', or one's.
    fn all_leaks(p: &Priv, run: Option<&str>) -> std::io::Result<Vec<String>> {
        let mut left = leaks(p, run)?;
        left.extend(vm::container_leaks(run)?);
        Ok(left)
    }

    /// Prints the leak lines of `st3b-crash leaks`; success when there are none.
    fn print_leaks(left: std::io::Result<Vec<String>>) -> ExitCode {
        match left {
            Ok(left) => {
                left.iter().for_each(|l| println!("{l}"));
                if left.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("st3b leaks: {e}");
                ExitCode::FAILURE
            }
        }
    }

    /// One VM scenario (S7): kill cycles of a KVM guest on one disk, C1-C4 after each. No devices
    /// and no sudo; the guest comes from `mkguest.sh`.
    fn vm_scenario(args: &[String]) -> ExitCode {
        let (ours, node_extra) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[][..]),
        };
        let Some((dir, rest)) = ours.split_first() else {
            return usage();
        };
        let (mut scenario, mut fs, mut kills, mut seed) = (None, None, None, None);
        let (mut dashd, mut guest, mut every) = (None, None, 100);
        for pair in rest.chunks(2) {
            let [a, v] = pair else {
                return usage();
            };
            let n = v.parse::<u64>().ok();
            match (a.as_str(), n) {
                ("--scenario", _) if Scenario::parse(v).is_some() => scenario = Scenario::parse(v),
                ("--fs", _) if Fs::parse(v).is_some() => fs = Fs::parse(v),
                ("--kills", Some(n)) => kills = Some(n as usize),
                ("--seed", Some(n)) => seed = Some(n),
                ("--dashd", _) => dashd = Some(PathBuf::from(v)),
                ("--guest", _) => guest = Some(PathBuf::from(v)),
                ("--every", Some(n)) if n > 0 => every = n as usize,
                _ => return usage(),
            }
        }
        let (Some(scenario), Some(fs)) = (scenario, fs) else {
            return usage();
        };
        if scenario == Scenario::Reindex {
            println!("{} {}: SKIPPED (CH5)", scenario.name(), fs.name());
            return ExitCode::SUCCESS;
        }
        let (Some(kills), Some(seed), Some(dashd), Some(guest)) = (kills, seed, dashd, guest)
        else {
            return usage();
        };
        let config = scenario::Config {
            scenario,
            fs,
            kills,
            seed,
            dashd,
            guest,
            extra: node_extra.to_vec(),
            every,
        };
        let fixture = match Fixture::load(Path::new(dir)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("st3b vm: loading {dir}: {e}");
                return ExitCode::FAILURE;
            }
        };
        match vm::sweep_stale_containers() {
            Ok(stale) => {
                stale.swept.iter().for_each(|s| eprintln!("st3b: stale {s}"));
                stale.failed.iter().for_each(|f| eprintln!("st3b: not swept: {f}"));
            }
            Err(e) => eprintln!("st3b: sweeping stale containers: {e}"),
        }
        let run = match Run::create() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("st3b: cannot start a run: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "run {} vm {dir} scenario {} fs {} kills {kills} seed {seed} every {every} extra {:?}",
            run.id(),
            scenario.name(),
            fs.name(),
            config.extra
        );
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime");
        let started = std::time::Instant::now();
        let result =
            catch_unwind(AssertUnwindSafe(|| scenario::run(&runtime, &run, &fixture, &config)));
        let mut ok = match result {
            Ok(Ok(report)) => vm_report(&report, kills, started),
            Ok(Err(e)) => {
                eprintln!("st3b vm: {e}");
                false
            }
            Err(_) => {
                eprintln!("st3b vm: panicked");
                false
            }
        };
        ok &= report_leaks(run.id(), vm::container_leaks(Some(run.id())), "container(s)");
        let status = if ok {
            "PASS"
        } else {
            "FAILED"
        };
        println!("{} {}: {status}", scenario.name(), fs.name());
        if !ok {
            eprintln!("kept {} for the repro", run.dir().display());
            return ExitCode::FAILURE;
        }
        match run.finish() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("st3b: cleaning up the run: {e}");
                ExitCode::FAILURE
            }
        }
    }

    /// `kill-smoke`: SIGKILL rdashd on plain disk during W-small's IBD (S8; no devices, no sudo).
    fn kill_smoke_cmd(args: &[String]) -> ExitCode {
        let (ours, node_extra) = split_node_args(args);
        let Some((dir, rest)) = ours.split_first() else {
            return usage();
        };
        let (mut rdashd, mut dashd, mut kills, mut seed, mut every) = (None, None, 8, 0, 100);
        for pair in rest.chunks(2) {
            let [a, v] = pair else {
                return usage();
            };
            let n = v.parse::<u64>().ok();
            match (a.as_str(), n) {
                ("--rdashd", _) => rdashd = Some(PathBuf::from(v)),
                ("--dashd", _) => dashd = Some(PathBuf::from(v)),
                ("--kills", Some(n)) if n > 0 => kills = n as usize,
                ("--seed", Some(n)) => seed = n,
                ("--every", Some(n)) if n > 0 => every = n as usize,
                _ => return usage(),
            }
        }
        let (Some(rdashd), Some(dashd)) = (rdashd, dashd) else {
            return usage();
        };
        let config = kill_smoke::Config {
            rdashd,
            dashd,
            kills,
            seed,
            extra: node_extra.to_vec(),
            every,
        };
        let Some(fixture) = load_fixture("kill-smoke", dir) else {
            return ExitCode::FAILURE;
        };
        let Some(run) = started_run(Run::create()) else {
            return ExitCode::FAILURE;
        };
        println!(
            "run {} kill-smoke {dir} kills {kills} seed {seed} every {every} extra {:?}",
            run.id(),
            config.extra
        );
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        let started = std::time::Instant::now();
        let result =
            catch_unwind(AssertUnwindSafe(|| kill_smoke::run(&runtime, &run, &fixture, &config)));
        let ok = match result {
            Ok(Ok(report)) => kill_smoke_report(&report, kills, started),
            Ok(Err(e)) => {
                eprintln!("st3b kill-smoke: {e}");
                false
            }
            Err(_) => {
                eprintln!("st3b kill-smoke: panicked");
                false
            }
        };
        println!(
            "kill-smoke: {}",
            if ok {
                "PASS"
            } else {
                "FAILED"
            }
        );
        finish_run(run, ok)
    }

    /// Prints a kill smoke's kills and totals; success when it has no failures and made its kills.
    /// The `points N {"smoke": N}` line is what the nightly's ledger reads.
    fn kill_smoke_report(
        report: &kill_smoke::Report,
        kills: usize,
        started: std::time::Instant,
    ) -> bool {
        let secs = |d: std::time::Duration| d.as_secs_f64();
        for (i, k) in report.kills.iter().enumerate() {
            println!(
                "kill {i}: lap {} boot {} after {:.2} s, floor {:?}, recovered at {:?}, cycle {}",
                k.lap,
                k.boot,
                secs(k.after),
                k.floor,
                k.recovered,
                cycle_text(k.cycle)
            );
        }
        let checked = report.kills.iter().filter(|k| k.recovered.is_some()).count();
        let starts: Vec<f64> = report.start_times.iter().map(|d| secs(*d)).collect();
        let mean_start = starts.iter().sum::<f64>() / starts.len().max(1) as f64;
        let wall = secs(started.elapsed());
        println!(
            "kills {} of {kills} ({checked} checked), laps {}, tips {}, starts {} (mean {:.1} s to \
             RPC), wall {:.0} s ({:.1} s per kill)",
            report.kills.len(),
            report.laps,
            report.tips,
            report.boots,
            mean_start,
            wall,
            wall / report.kills.len().max(1) as f64
        );
        println!("points {checked} {{\"smoke\": {checked}}}");
        for f in &report.failures {
            eprintln!("FAIL {f}");
        }
        report.failures.is_empty() && report.kills.len() >= kills && checked == report.kills.len()
    }

    /// The VM summary's legend (S7 r1 M1 and its rulings, r2 N3, N4, N6).
    const VM_LEGEND: &str = "legend: a kill counts when it is the scenario's own kind and in its \
        window. flush, partial, recovery: in window when the next boot replays blocks (the flush, \
        or the replay, never committed). reorg: the kills are split evenly over the fixture's \
        reorgs as quotas (the remainder to the last), each boot aims at one reorg still ahead \
        with quota left, drawn from the seed (none left: the lap kills no more), and the run \
        passes only with every quota met; in window when the submitblock that sets that reorg off had not returned \
        when the kill was armed (\"submit pending\", the host's bound) and the killed boot's \
        console shows the reorg cut mid-way (a height drop from the boot's starting height or a \
        logged tip, no UpdateTip at or above the old top yet); nothing is flushed inside a reorg \
        at -dbcache=4, so the node recovers to the last flushed state, and C1-C4 check it has no \
        half-applied reorg. \"strictly inside a reorg (C3)\" is the stronger case, a durable \
        state inside the reorg, reported and not required (durable mid-reorg states are the \
        device level's, D-REORG-S); C3 fails it unless the killed boot's console shows a coins \
        flush inside that reorg. random: always in window. Every kill, counted or not, passes \
        C1-C4 at its floor.";

    /// Prints a VM run's kills and totals; success when it has no failures and made its kills.
    fn vm_report(report: &scenario::Report, kills: usize, started: std::time::Instant) -> bool {
        let secs = |d: std::time::Duration| d.as_secs_f64();
        for (i, k) in report.kills.iter().enumerate() {
            let window = match (k.in_window, k.counted) {
                (_, true) => "in window, counted",
                (Some(true), false) => "in window, not counted",
                (Some(false), _) => "OUT OF WINDOW, not counted",
                (None, _) => "unchecked, not counted",
            };
            let reorg = match (k.mid_reorg, k.inside) {
                (None, _) => String::new(),
                (Some(mid), inside) => format!(
                    " [reorg {}; submit {}; {}; C3 {}]",
                    k.reorg.map_or("-".to_owned(), |w| (w + 1).to_string()),
                    if k.pending == Some(true) {
                        "pending"
                    } else {
                        "RETURNED"
                    },
                    if mid {
                        "cut mid-reorg"
                    } else {
                        "reorg done"
                    },
                    match inside {
                        Some(true) => "strictly inside",
                        Some(false) => "outside",
                        None => "unchecked",
                    }
                ),
            };
            println!(
                "kill {i}: lap {} boot {} {}{reorg} ({window}) after {:.2} s, floor {:?}, \
                 recovered at {:?}, cycle {}, line {:?}",
                k.lap,
                k.boot,
                k.kind,
                secs(k.kill.after),
                k.floor,
                k.recovered,
                k.cycle.map_or("-".to_owned(), |c| format!("{:.1} s", secs(c))),
                k.kill.line.as_deref().unwrap_or("-")
            );
        }
        let mean = |v: &[f64]| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        let mut boots: Vec<f64> = report.boot_times.iter().map(|d| secs(*d)).collect();
        boots.sort_by(f64::total_cmp);
        // Nearest rank.
        let pct = |p: usize| {
            boots.get((boots.len() * p).div_ceil(100).saturating_sub(1)).copied().unwrap_or(0.0)
        };
        let cycles: Vec<f64> = report.kills.iter().filter_map(|k| k.cycle).map(secs).collect();
        let wall = secs(started.elapsed());
        let own = report.own();
        println!(
            "kills {} ({} counted of {kills}), {} kills {own}: {} in window ({:.0}%), laps {}, \
             boots {}, boot to RPC p50 {:.1} s p95 {:.1} s max {:.1} s, cycle per kill mean \
             {:.1} s, wall {:.0} s ({:.1} s per counted kill)",
            report.kills.len(),
            report.counted,
            report.kind,
            report.counted,
            100.0 * report.counted as f64 / own.max(1) as f64,
            report.laps,
            report.boots,
            pct(50),
            pct(95),
            boots.last().copied().unwrap_or(0.0),
            mean(&cycles),
            wall,
            wall / report.counted.max(1) as f64
        );
        if report.kind == "reorg" {
            // Per reorg (S7 r2 N6), by each kill's target.
            for (w, (top, end)) in report.windows.iter().enumerate() {
                let of: Vec<_> = report.kills.iter().filter(|k| k.reorg == Some(w)).collect();
                let n = |f: fn(&scenario::KillRecord) -> bool| of.iter().filter(|k| f(k)).count();
                println!(
                    "reorg {} (positions {top}..{end}): quota {}, {} kills, {} counted, submit \
                     pending {}, cut mid-reorg {}, strictly inside (C3) {}",
                    w + 1,
                    report.quotas.get(w).copied().unwrap_or(0),
                    of.len(),
                    n(|k| k.counted),
                    n(|k| k.pending == Some(true)),
                    n(|k| k.mid_reorg == Some(true)),
                    n(|k| k.inside == Some(true)),
                );
            }
            println!("strictly inside a reorg (C3): {} of {own} reorg kills", report.inside());
        }
        println!("{VM_LEGEND}");
        for f in &report.failures {
            eprintln!("FAIL {f}");
        }
        report.failures.is_empty() && report.counted >= kills
    }

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let p = Priv::locate();
        match args.first().map(String::as_str) {
            Some("selftest") => selftest(&p, &args[1..]),
            Some("baseline") => baseline(&args[1..]),
            Some("inspect") if args.len() == 2 => inspect(&args[1]),
            Some("check") => check(&args[1..]),
            Some("campaign") => campaign(&p, &args[1..]),
            Some("c6") => c6(&p, &args[1..]),
            Some("fault") => fault(&p, &args[1..]),
            Some("vm") => vm_scenario(&args[1..]),
            Some("kill-smoke") => kill_smoke_cmd(&args[1..]),
            Some("leaks") if (2..=3).contains(&args.len()) && args[1] == "--vm" => {
                print_leaks(vm::container_leaks(args.get(2).map(String::as_str)))
            }
            Some("sweep") if args.len() == 2 => {
                let r = if args[1] == "--stale" {
                    // Every run is tried; any that did not sweep fails the verb. The containers of
                    // runs whose lock nobody holds go too.
                    sweep_stale(&p).and_then(|mut stale| {
                        let containers = vm::sweep_stale_containers()?;
                        stale.swept.extend(containers.swept);
                        stale.failed.extend(containers.failed);
                        stale_result(stale)
                    })
                } else if args[1] == "--stale-vm" {
                    // The VM level's own, without the wrapper (no sudo): stale containers only.
                    vm::sweep_stale_containers().and_then(stale_result)
                } else {
                    // A live run's lock makes the wrapper refuse: that is a failure here.
                    p.sweep(&args[1]).and_then(|swept| {
                        swept.map(|s| vec![s]).ok_or_else(|| {
                            std::io::Error::other(format!("run {} is live", args[1]))
                        })
                    })
                };
                match r {
                    Ok(swept) => {
                        for s in swept {
                            println!("{s}");
                        }
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("st3b sweep: {e}");
                        ExitCode::FAILURE
                    }
                }
            }
            // With a run, only that run's (the nightly checks only the runs it started).
            Some("leaks") if args.len() <= 2 => {
                print_leaks(all_leaks(&p, args.get(1).map(String::as_str)))
            }
            _ => usage(),
        }
    }
}
