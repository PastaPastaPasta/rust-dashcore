//! BL-223 (O2b-4 4a): one Diff chain target's process run `count` executions long, through
//! [`fuzz_one_diff`] as its fuzz binary calls it (sessions, replays, census folds), over the
//! target's `chain-v2` public seeds and then a fixed generated set (splitmix64 from `seed`, 16 to
//! 400 bytes each). Every 1,000 executions it prints the process's resident set and CPU time, and
//! at the end the RSS plateau (the growth over the second half of the samples) and a throughput
//! estimate: executions per CPU second of this process, the percentiles of an execution's CPU
//! seconds, and the outcomes. Before the soak it times the session's start and an empty input
//! (the restores' floor). A divergent input is printed with its program, for triage.
//!
//! Built with node-fuzz's release profile (debug assertions on, which `sut`'s test seams need):
//! `cargo run --release --config profile.release.debug-assertions=true --config
//! profile.release.overflow-checks=true --config profile.release.debug=1 -p node-l1 --features sut
//! --example chain_diff_soak -- S0 20000 1` (the census goes to `NODE_L1_CHAIN_STATS`, else the
//! temporary directory; `CHAIN_DIFF_SOAK_LAST=FILE` keeps the number of the execution about to
//! run and the seed in FILE, for a crash's triage: the input is reproduced from the two, the
//! seed's stream run up to that execution, so no input's bytes are written to a file).

use std::collections::BTreeMap;

use dash_oracle_sys::Pin;
use node_l1::Outcome;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::Program;
use node_l1::chain::run::{diff_target, fuzz_one_diff, start_diff};

/// This process's user + system CPU seconds (`/proc/self/stat`, 100 ticks a second). A failed
/// read is fatal: a zero would read as a free execution.
fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    let (_, rest) = stat.rsplit_once(')').expect("/proc/self/stat's comm field");
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| -> f64 {
        fields.get(i).and_then(|f| f.parse().ok()).expect("/proc/self/stat's utime and stime")
    };
    (tick(11) + tick(12)) / 100.0
}

/// This process's resident set in KiB (`VmRSS`). A failed read is fatal: a zero series would
/// read as a flat plateau.
fn rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("/proc/self/status's VmRSS")
}

/// Empty inputs timed after the start, before the soak.
const EMPTY: usize = 20;

const USAGE: &str = "usage: chain_diff_soak S0|S1|S2|S0p [count, default 20000] [seed, default 1]";

/// The argument at `i` parsed, `default` when absent; one that does not parse ends the run.
fn arg<T: std::str::FromStr>(args: &[String], i: usize, default: T) -> T {
    match args.get(i) {
        None => default,
        Some(a) => a.parse().unwrap_or_else(|_| {
            eprintln!("chain_diff_soak: {a:?} does not parse\n{USAGE}");
            std::process::exit(2)
        }),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let schedule = match args.get(1).map(String::as_str) {
        Some("S0") => Schedule::S0,
        Some("S1") => Schedule::S1,
        Some("S2") => Schedule::S2,
        Some("S0p") => Schedule::S0p,
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2)
        }
    };
    if args.len() > 4 {
        eprintln!("{USAGE}");
        std::process::exit(2)
    }
    let count: usize = arg(&args, 2, 20_000);
    let seed: u64 = arg(&args, 3, 1);
    let mut x = seed;
    let mut next = move || {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let target = diff_target(schedule);
    let dir = format!("{}/../node-fuzz/public-seeds/{}", env!("CARGO_MANIFEST_DIR"), target.name);
    let mut seeds: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .expect("the public seeds")
        .map(|e| {
            let e = e.expect("entry");
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).expect("read"))
        })
        .collect();
    seeds.sort();
    let mut input = |n: usize| -> Vec<u8> {
        match seeds.get(n) {
            Some((_, bytes)) => bytes.clone(),
            None => {
                let len = 16 + (next() % 385) as usize;
                (0..len).map(|_| next() as u8).collect()
            }
        }
    };

    let (cpu0, wall0) = (cpu_seconds(), std::time::Instant::now());
    start_diff(Pin::V24_0, schedule);
    let started = (cpu_seconds() - cpu0, rss_kib());
    println!(
        "{}: started in {:.1} CPU-s, rss {} KiB; {} public seeds, then generated inputs",
        target.name,
        started.0,
        started.1,
        seeds.len()
    );
    // An empty program is both sides' restores: the floor of every execution.
    let cpu_empty = cpu_seconds();
    for _ in 0..EMPTY {
        fuzz_one_diff(&[], Pin::V24_0, schedule).expect("an empty input");
    }
    println!(
        "{}: an empty input costs {:.3} CPU-s (the mean of {EMPTY})",
        target.name,
        (cpu_seconds() - cpu_empty) / EMPTY as f64
    );
    let (cpu1, wall1) = (cpu_seconds(), std::time::Instant::now());
    let mut outcomes: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples: Vec<u64> = Vec::new();
    let mut per_input: Vec<f64> = Vec::with_capacity(count);
    let last = std::env::var_os("CHAIN_DIFF_SOAK_LAST");
    for n in 0..count {
        let bytes = input(n);
        if let Some(path) = &last {
            std::fs::write(path, format!("execution {}\nseed {seed}\n", n + 1))
                .expect("CHAIN_DIFF_SOAK_LAST");
        }
        let before = cpu_seconds();
        let compared = fuzz_one_diff(&bytes, Pin::V24_0, schedule);
        per_input.push(cpu_seconds() - before);
        let outcome = match compared {
            Ok(Outcome::BothAccept) => "both-accept".to_owned(),
            Ok(Outcome::Discarded) => "discarded".to_owned(),
            Ok(Outcome::OutOfDomain) => "out-of-domain".to_owned(),
            Ok(other) => format!("{other:?}"),
            Err(d) => {
                // For triage: the input and its program (synthetic bytes, `chain-v2`).
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "{}: execution {} diverged at {}: {hex}\n{}",
                    target.name,
                    n + 1,
                    d.field,
                    Program::decode_v2(&bytes)
                );
                format!("divergence:{}", d.field)
            }
        };
        *outcomes.entry(outcome).or_default() += 1;
        if (n + 1) % 1000 == 0 {
            let rss = rss_kib();
            samples.push(rss);
            println!(
                "{}: {} executions, {:.1} CPU-s, {:.1} s, rss {rss} KiB",
                target.name,
                n + 1,
                cpu_seconds() - cpu1,
                wall1.elapsed().as_secs_f64()
            );
        }
    }
    let cpu = cpu_seconds() - cpu1;
    per_input.sort_by(f64::total_cmp);
    let at = |q: f64| {
        per_input
            .get(((per_input.len() as f64 * q) as usize).min(per_input.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0.0)
    };
    println!(
        "{}: CPU-s per execution: p50 {:.2}, p90 {:.2}, p99 {:.2}, max {:.2}",
        target.name,
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0)
    );
    let half = &samples[samples.len() / 2..];
    let growth = match (half.first(), half.last()) {
        (Some(a), Some(b)) => i128::from(*b) - i128::from(*a),
        _ => 0,
    };
    println!(
        "{}: {count} executions in {cpu:.1} CPU-s ({:.1} s wall, {:.1} s with the start): {:.2} \
         executions per CPU-s; rss first {} KiB, max {} KiB, last {} KiB; second-half growth {growth} \
         KiB over {} samples; outcomes {outcomes:?}",
        target.name,
        wall1.elapsed().as_secs_f64(),
        wall0.elapsed().as_secs_f64(),
        count as f64 / cpu,
        samples.first().copied().unwrap_or(0),
        samples.iter().max().copied().unwrap_or(0),
        samples.last().copied().unwrap_or(0),
        half.len()
    );
}
