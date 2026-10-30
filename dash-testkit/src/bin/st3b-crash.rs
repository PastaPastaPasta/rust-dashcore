//! `st3b-crash`: the ST3b power-loss harness (see `docs/node-program/designs/ST3b-notes.md`).
//!
//! ```text
//! st3b-crash selftest [--fs ext4|xfs] [--seed N] [--rand N] [--torn N] [--split N] [--fault panic|pause]
//! st3b-crash sweep (--stale | <run>)
//! st3b-crash leaks [<run>]
//! st3b-crash baseline <fixture dir> --rdashd PATH --dashd PATH [--every N] [-- <rdashd args>]
//! st3b-crash c6 <fixture dir> <image> --rdashd PATH --dashd PATH --point I --floor ack:N
//! ```
//!
//! Every run ends with a leak check of its own devices and mounts, and fails loudly if any remain.

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
    use dash_testkit::crash::devices::{Fs, Priv, ROOT, Run, leaks, private_dir, sweep_stale};
    use dash_testkit::crash::fixture::Fixture;
    use dash_testkit::crash::inspect::Inspection;
    use dash_testkit::crash::logwrites::Draw;
    use dash_testkit::crash::selftest::{self, Fault};

    fn usage() -> ExitCode {
        eprintln!(
            "usage: st3b-crash selftest [--fs ext4|xfs] [--seed N] [--rand N] [--torn N] [--split N] [--fault panic|pause]\n\
             \x20      st3b-crash sweep (--stale | <run>)\n\
             \x20      st3b-crash leaks [<run>]\n\
         \x20      st3b-crash baseline <fixture dir> --rdashd PATH --dashd PATH [--every N] \
         [--min-partial N] [-- <rdashd args>]\n\
         \x20      st3b-crash inspect <datadir>\n\
         \x20      st3b-crash check <fixture dir> <datadir> --rdashd PATH [--dashd PATH] \
         [--floor start|ack:N|stopped:N] [--seed N] [-- <rdashd args>]\n\
         \x20      st3b-crash campaign <fixture dir> --rdashd PATH --dashd PATH [--fs ext4|xfs] \
         [--seed N] [--rand N] [--torn N] [--split N] [--every N] [--c5-every N] [--c6-every N] [--shard I/N] \
         [--points A-B] [--split-chainstate] [--plant BUG] [--torn-only] [--c0-only] [--dry-run] [-- <rdashd args>]\n\
         \x20      st3b-crash c6 <fixture dir> <image> --rdashd PATH --dashd PATH --point I \
         --floor start|ack:N|stopped:N [--fs ext4|xfs] [--seed N] [-- <rdashd args>]"
        );
        ExitCode::from(2)
    }

    /// Reports `run`'s leftovers; true when there are none.
    fn leak_check(p: &Priv, run: &str) -> bool {
        match leaks(p, Some(run)) {
            Ok(left) if left.is_empty() => {
                println!("leak check: clean");
                true
            }
            Ok(left) => {
                eprintln!("LEAK: run {run} left {} device(s) or mount(s):", left.len());
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
        let run = match Run::start(p) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("st3b: cannot start a run: {e}");
                return ExitCode::FAILURE;
            }
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

    /// Prints a campaign's report; success when it has no failures and checked a point.
    fn campaign_report(result: std::io::Result<campaign::Report>, what: &str) -> bool {
        match result {
            Ok(report) => {
                println!(
                    "log entries {}, points {} {:?}, phases {:?}, C5 runs {}, C6 runs {} ({} points)",
                    report.entries,
                    report.points,
                    report.by_kind,
                    report.phases,
                    report.c5_runs,
                    report.c6_runs,
                    report.c6_points
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
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime");
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
        let (ours, node_extra) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[][..]),
        };
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
        let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86400);
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
                _ => return usage(),
            }
        }
        let (Some(rdashd), Some(dashd)) = (rdashd, dashd) else {
            return usage();
        };
        let (fs, draw, every, c5_every, c6_every, shard, points) = cfg;
        let config = campaign::Config {
            fs,
            draw,
            rdashd,
            dashd,
            extra: node_extra.to_vec(),
            every,
            c5_every,
            c6_every,
            shard,
            points,
            split_chainstate,
            plant,
            torn_only,
            c0_only,
        };
        let fixture = match Fixture::load(Path::new(dir)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("st3b campaign: loading {dir}: {e}");
                return ExitCode::FAILURE;
            }
        };
        if dry {
            return dry_run(dir, &fixture, &config);
        }
        let run = match Run::start(p) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("st3b: cannot start a run: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "run {} campaign {dir} fs {} seed {} rand {} torn {} split {} every {every} c5 \
             1/{c5_every} extra {:?} plant {:?}",
            run.id(),
            fs.name(),
            draw.seed,
            draw.rand,
            draw.torn,
            draw.split,
            config.extra,
            config.plant
        );
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime");
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

    /// Syncs a fixture on plain disk (no devices, no sudo), C2 at every acknowledgement.
    fn baseline(args: &[String]) -> ExitCode {
        let (ours, node_extra) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[][..]),
        };
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
        let fixture = match Fixture::load(Path::new(dir)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("st3b baseline: loading {dir}: {e}");
                return ExitCode::FAILURE;
            }
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
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime");
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
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let work = Path::new(ROOT).join(kind).join(format!("{secs}-{}", std::process::id()));
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
        let (ours, node_extra) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[][..]),
        };
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
        let fixture = match Fixture::load(Path::new(dir)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("st3b check: loading {dir}: {e}");
                return ExitCode::FAILURE;
            }
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
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime");
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
        let (ours, node_extra) = match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[][..]),
        };
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
            shard: (0, 1),
            points: i..=i,
            split_chainstate: false,
            plant: None,
            torn_only: false,
            c0_only: false,
        };
        let fixture = match Fixture::load(Path::new(dir)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("st3b c6: loading {dir}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let run = match Run::start(p) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("st3b: cannot start a run: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "run {} c6 {img} point {i} floor {floor:?} fs {} seed {seed} extra {:?}",
            run.id(),
            fs.name(),
            config.extra
        );
        let runtime =
            tokio::runtime::Builder::new_current_thread().enable_time().build().expect("a runtime");
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
            Some("sweep") if args.len() == 2 => {
                let r = if args[1] == "--stale" {
                    // Every run is tried; any that did not sweep fails the verb.
                    sweep_stale(&p).and_then(|stale| {
                        if stale.failed.is_empty() {
                            Ok(stale.swept)
                        } else {
                            for s in &stale.swept {
                                println!("{s}");
                            }
                            Err(std::io::Error::other(stale.failed.join("; ")))
                        }
                    })
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
            Some("leaks") if args.len() <= 2 => match leaks(&p, args.get(1).map(String::as_str)) {
                Ok(left) => {
                    for l in &left {
                        println!("{l}");
                    }
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
            },
            _ => usage(),
        }
    }
}
