//! `Pow_X11_*` of B1's `microbench` (`node-bench/run run microbench --impl rust --rust-bench-bin`),
//! for the X11 backend this build selects.
//!
//! Prints nanobench-style JSON under bench_dash's names, like
//! `bench_dash -filter=… -min-time=… -output-json=…` does, so `node-bench` lines the rows up:
//!
//! - `Pow_X11_0080b`: X11 of an 80-byte header, one hash per iteration (`batch` 80, so the
//!   harness's ns/byte is Core's);
//! - `Pow_X11_1M`: X11 of one million bytes (`batch` 1,000,000). Only with the default backend:
//!   `rs-x11-hash` cannot hash that length, so under `x11-c` the row is left out.
//!
//! The hash is `dashcore_hashes::hash_x11::Hash::hash`, the call behind `dashcore::BlockHash`,
//! engine buffer included. Each result also carries the `hash_x11::backend()` name, which
//! `node-bench` ignores and a reader of the JSON needs. Build one binary per backend and compare:
//!
//! ```text
//! cargo build --release -p dashcore_hashes --features x11 --bench x11
//! cargo build --release -p dashcore_hashes --features x11-c --bench x11
//! ```
//!
//! Arguments: `-filter=<name or ^(a|b|…)$>`, `-min-time=<ms>` (default 100) per benchmark and
//! `-output-json=<path>` (default: stdout). Anything else is ignored, so `cargo bench` works.

use std::hint::black_box;
use std::time::Instant;

use dashcore_hashes::hash_x11;
use dashcore_hashes::Hash;

/// Epochs measured per benchmark; the reported time is their median, as nanobench's.
const EPOCHS: usize = 11;

/// (bench_dash name, input length).
const BENCHES: &[(&str, usize)] = &[
    ("Pow_X11_0080b", 80),
    // `rs-x11-hash` cannot hash this length; under `x11-c` the row would measure `dash-x11`.
    #[cfg(not(feature = "x11-c"))]
    ("Pow_X11_1M", 1_000_000),
];

/// Whether `name` is selected by a bench_dash `-filter` value: a name, or `^(a|b|…)$`.
fn selected(filter: &str, name: &str) -> bool {
    let alternatives =
        filter.strip_prefix("^(").and_then(|f| f.strip_suffix(")$")).unwrap_or(filter);
    alternatives.split('|').any(|a| a == name)
}

/// Seconds per iteration: the median over `EPOCHS` epochs of about `min_time` / `EPOCHS` each.
fn measure(len: usize, min_time: f64) -> f64 {
    let input = vec![0u8; len]; // Core hashes zeros
    let run = |iterations: u64| {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(hash_x11::Hash::hash(black_box(&input)));
        }
        start.elapsed().as_secs_f64()
    };
    // Grow the epoch until it takes the share of `min_time` it is given.
    let share = min_time / EPOCHS as f64;
    let mut iterations = 1;
    while run(iterations) < share {
        iterations *= 2;
    }
    let mut times: Vec<f64> = (0..EPOCHS).map(|_| run(iterations) / iterations as f64).collect();
    times.sort_by(f64::total_cmp);
    times[EPOCHS / 2]
}

fn main() {
    let (mut filter, mut min_time, mut output) = (None, 0.1, None);
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("-filter=") {
            filter = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("-min-time=") {
            min_time = v.parse::<f64>().expect("-min-time is in milliseconds") / 1000.0;
        } else if let Some(v) = arg.strip_prefix("-output-json=") {
            output = Some(v.to_string());
        }
    }
    let backend = hash_x11::backend();
    eprintln!("x11 backend: {backend}");
    let results: Vec<String> = BENCHES
        .iter()
        .filter(|(name, _)| filter.as_deref().is_none_or(|f| selected(f, name)))
        .map(|&(name, len)| {
            let seconds = measure(len, min_time);
            eprintln!("{name}: {:.1} ns/byte", seconds * 1e9 / len as f64);
            format!(
                r#"{{"name": "{name}", "unit": "byte", "batch": {len}, "median(elapsed)": {seconds:e}, "backend": {backend:?}}}"#
            )
        })
        .collect();
    if results.is_empty() {
        eprintln!("no benchmark matches the filter (only names and ^(a|b|...)$ are understood)");
    }
    let json = format!(r#"{{"results": [{}]}}"#, results.join(", "));
    match output {
        Some(path) => std::fs::write(&path, json + "\n").unwrap_or_else(|e| panic!("{path}: {e}")),
        None => println!("{json}"),
    }
}
