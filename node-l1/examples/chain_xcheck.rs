//! O2b-4 4f: the full X-check (O2b-4 PLAN §5, [`node_l1::chain::xcheck`]). The bases, the spork
//! experiment, then each schedule's sample on the session and on both dashds; the evidence goes
//! to `<root>/<run>/evidence/` and its markdown also to `--out`. Exits 1 on a failed minimum, a
//! plane never compared, an undisposed disagreement or a failed spork assertion, and on any run
//! narrower than S0, S1 and S2 (it writes evidence, but is not the M3 gate); 2 on bad arguments.
//!
//! Built as `chain_diff_soak` is, with debug assertions on (`sut`'s test seams need them):
//! `cargo run --release --config profile.release.debug-assertions=true --config
//! profile.release.overflow-checks=true -p node-l1 --features l3 --example chain_xcheck --
//! --root DIR --stock PATH --hooked PATH --schedules S0,S1,S2 --out FILE`.

use std::path::PathBuf;

use node_l1::chain::core::Schedule;
use node_l1::chain::xcheck::sample::M3;
use node_l1::chain::xcheck::{self, Binary, Config, Kind, MAX_CONCURRENCY, evidence};

const USAGE: &str = "usage: chain_xcheck --root DIR --stock PATH --hooked PATH \
                     [--schedules S0,S1,S2] [--concurrency N<=4] [--max-draws N] --out FILE";

fn fail(message: &str, code: i32) -> ! {
    eprintln!("chain_xcheck: {message}");
    std::process::exit(code)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| {
        args.iter().position(|a| a == flag).map(|i| {
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| fail(&format!("{flag} needs a value\n{USAGE}"), 2))
        })
    };
    let need = |v: Option<String>, flag: &str| {
        v.unwrap_or_else(|| fail(&format!("{flag} is required\n{USAGE}"), 2))
    };
    let root = PathBuf::from(need(get("--root"), "--root"));
    let stock = PathBuf::from(need(get("--stock"), "--stock"));
    let hooked = PathBuf::from(need(get("--hooked"), "--hooked"));
    let out = PathBuf::from(need(get("--out"), "--out"));
    let schedules = get("--schedules").unwrap_or_else(|| "S0,S1,S2".into());
    let schedules: Vec<Schedule> = schedules
        .split(',')
        .map(|s| match s {
            "S0" => Schedule::S0,
            "S1" => Schedule::S1,
            "S2" => Schedule::S2,
            other => fail(&format!("unknown schedule {other:?} (S0p is excluded)\n{USAGE}"), 2),
        })
        .collect();
    let number = |v: Option<String>, default: u64| {
        v.map_or(default, |v| {
            v.parse().unwrap_or_else(|_| fail(&format!("{v:?} is not a number"), 2))
        })
    };
    let concurrency = number(get("--concurrency"), MAX_CONCURRENCY as u64) as usize;
    if concurrency == 0 || concurrency > MAX_CONCURRENCY {
        fail(&format!("--concurrency must be 1 to {MAX_CONCURRENCY}"), 2);
    }
    let max_draws = number(get("--max-draws"), 20_000);
    let binary = |path: &PathBuf, kind| Binary::new(path, kind).unwrap_or_else(|e| fail(&e, 2));
    let config = Config {
        root,
        stock: binary(&stock, Kind::Stock),
        hooked: binary(&hooked, Kind::Hooked),
        schedules,
        concurrency,
        minimums: M3,
        max_draws,
    };
    let started = std::time::Instant::now();
    let (evidence, dir) = xcheck::run(&config).unwrap_or_else(|e| fail(&e, 1));
    let written = evidence::write(&evidence, &dir.join("evidence"));
    let md = dir.join("evidence").join(format!("{}.md", evidence::NAME));
    if let Err(e) = std::fs::copy(&md, &out) {
        fail(&format!("{}: {e}", out.display()), 1);
    }
    println!(
        "chain_xcheck: {} in {:.0} s; evidence {}",
        dir.display(),
        started.elapsed().as_secs_f64(),
        md.display()
    );
    if let Err(e) = written {
        fail(&e, 1);
    }
}
