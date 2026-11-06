//! BL-502 test 8: the process RNG's debug.log lines, rdashd against dashd v24 on the same host,
//! both started with `-debug` and `rand` not excluded:
//! - (i) at startup, ReportHardwareRand's lines in dashd's order between the last parameter
//!   interaction line and "Default data directory", and no STARTUP "environment data" line in
//!   either log;
//! - (ii) one "dynamic environment data" line per periodic run, four `mockscheduler 61` apart.
//!
//! (ii) also prints each binary's byte counts and asserts every rdashd count lies in a ±1 % band
//! around dashd's median, the allowed difference in RP1a-notes (the count includes live `/proc`
//! text, so dashd's own count moves call to call).
#![cfg(all(target_os = "linux", target_env = "gnu"))]

mod common;

use common::net::*;
use common::*;

/// After the common flags, which exclude `rand`: the exclusions cleared, then the noisy ones
/// again.
const EXTRA: &[&str] = &["-nodebugexclude", "-debugexclude=libevent", "-debugexclude=leveldb"];
const MOCKTIME: u64 = 1_700_000_000;
const PERIODIC_RUNS: usize = 4;

/// A debug.log line without its timestamp and mock time.
fn message(line: &str) -> &str {
    let line = match line.split_once(' ') {
        Some((stamp, rest)) if stamp.ends_with('Z') => rest,
        _ => line,
    };
    match line.strip_prefix("(mocktime: ").and_then(|l| l.split_once(") ")) {
        Some((_, rest)) => rest,
        None => line,
    }
}

/// The startup sequence up to "Default data directory": the last parameter interaction line
/// (as `<parameter interaction>`), then every RNG line, then the anchor.
fn startup_sequence(log: &str) -> Vec<String> {
    let mut seq = Vec::new();
    for line in log.lines().map(message) {
        if line.contains("InitParameterInteraction: parameter interaction:") {
            seq.clear();
            seq.push("<parameter interaction>".to_owned());
        } else if line.contains("entropy source") || line.contains("environment data into RNG") {
            seq.push(line.to_owned());
        } else if line.starts_with("Default data directory ") {
            seq.push("Default data directory".to_owned());
            return seq;
        }
    }
    panic!("no \"Default data directory\" line:\n{log}");
}

/// The byte counts of the "dynamic environment data" lines.
fn periodic_counts(log: &str) -> Vec<u64> {
    log.lines()
        .map(message)
        .filter_map(|l| {
            l.split_once("Feeding ")?
                .1
                .strip_suffix(" bytes of dynamic environment data into RNG")?
                .parse()
                .ok()
        })
        .collect()
}

/// Starts one node of `binary`, runs the periodic task four times, stops it: its debug.log, and
/// the number of periodic lines already in the log once the node is up (a run the task's own
/// minute made on a slow start).
/// Each run is awaited in the log before the next `mockscheduler`: rdashd's `mockscheduler`
/// syncs V's queue, not the scheduler the periodic task runs on, so a forward that lands before
/// the last run has queued the next one misses it, and that run comes a real minute later.
fn run(binary: &Binary) -> (String, usize) {
    let mut net = Net::start(binary, 1, MOCKTIME, EXTRA);
    let node = &net.nodes[0];
    let log = debug_log(&node.datadir);
    let before = periodic_counts(&read(&log)).len();
    for run in 1..=PERIODIC_RUNS {
        assert!(node.call("mockscheduler", "[61]").is_null());
        node.call("getblockcount", "[]");
        wait_for(&format!("periodic run {run}"), 90, || {
            periodic_counts(&read(&log)).len() >= before + run
        });
    }
    net.stop_all();
    (read(&log), before)
}

#[test]
fn rng_log_lines_match_dashd() {
    let (core, core_before) = run(&Binary::dashd());
    let (ours, our_before) = run(&Binary::rdashd());

    // (i)
    let (core_seq, our_seq) = (startup_sequence(&core), startup_sequence(&ours));
    eprintln!("BL-502 test 8(i) dashd:  {core_seq:?}");
    eprintln!("BL-502 test 8(i) rdashd: {our_seq:?}");
    assert!(
        core_seq.iter().any(|l| l.contains("entropy source")),
        "dashd logged no hardware entropy line on this host, so the order check cannot fail"
    );
    assert!(core_seq.iter().all(|l| !l.contains("environment data")), "{core_seq:?}");
    assert_eq!(our_seq, core_seq);

    // (ii)
    let core_counts = periodic_counts(&core).split_off(core_before);
    let our_counts = periodic_counts(&ours).split_off(our_before);
    assert_eq!(core_counts.len(), PERIODIC_RUNS, "dashd: {core_counts:?} after {core_before}");
    assert_eq!(our_counts.len(), PERIODIC_RUNS, "rdashd: {our_counts:?} after {our_before}");
    let mut sorted = core_counts.clone();
    sorted.sort_unstable();
    let median = (sorted[1] + sorted[2]) as f64 / 2.0;
    let (lo, hi) = (median * 0.99, median * 1.01);
    let inside = our_counts.iter().filter(|&&c| (lo..=hi).contains(&(c as f64))).count();
    eprintln!(
        "BL-502 test 8(ii) dashd {core_counts:?} (spread {}, after {core_before}), rdashd \
         {our_counts:?} (after {our_before}); band [{lo:.0}, {hi:.0}] around dashd's median \
         {median}: {inside}/{PERIODIC_RUNS} inside",
        sorted[3] - sorted[0]
    );
    assert_eq!(inside, PERIODIC_RUNS, "rdashd {our_counts:?} outside [{lo:.0}, {hi:.0}]");
}
