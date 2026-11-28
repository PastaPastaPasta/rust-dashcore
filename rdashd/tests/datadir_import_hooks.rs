//! CH6a's `-importcoredatadir` killed, held or aborted at its process hooks
//! (`RDASHD_TEST_HOOK=datadir-import`, and the reindex's `abort-after-scan`), from a copy of a
//! checked-in Dash Core v24.0.0 regtest data directory. Each start that stops before P5 leaves
//! the import to the next start without the flag, which reaches the source's best block and
//! reports it. No dashd runs.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;

const FIXTURE: &str = "../dash-coreimport/tests/data/leveldb/a-wal";
const REPORTED: &str = "Core datadir import: complete, report ";

/// The source base data directory in `home` and its best block (display hex).
fn source(home: &Path) -> (PathBuf, String) {
    let base = home.join("core");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    for part in ["blocks", "chainstate"] {
        copy_tree(&fixture.join(part), &base.join("regtest").join(part));
    }
    std::fs::write(base.join("regtest/.lock"), b"").unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(&read(&fixture.join("expected.json"))).unwrap();
    (base, expected["best_block"].as_str().unwrap().to_owned())
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// A start with the import and `env`, until it exits or logs `until`; then it is sent SIGTERM.
/// Its exit status and its log.
fn hooked(
    home: &Path,
    dest: &Path,
    base: &Path,
    env: &[(&str, &str)],
    until: Option<&str>,
) -> (Option<i32>, String) {
    let import = format!("-importcoredatadir={}", base.display());
    let mut cmd = regtest(home, dest, &["-printtoconsole=0", &import]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let log = debug_log(dest);
    let mut running = Running::spawn(cmd, log.clone());
    let end = Instant::now() + PATIENCE;
    loop {
        if running.exited() {
            return (running.wait().status.code(), read(&log));
        }
        if until.is_some_and(|until| read(&log).contains(until)) {
            let out = running.stop(libc::SIGTERM);
            return (out.status.code(), read(&log));
        }
        assert!(Instant::now() < end, "the hooked start ends: {}", read(&log));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A start without the flag: it reaches `best` and reports the import. The node, still running.
fn resumed(home: &Path, dest: &Path, best: &str) -> RpcNode {
    let before = std::fs::metadata(debug_log(dest)).map_or(0, |m| m.len() as usize);
    let node = RpcNode::start(home, dest, &[]);
    wait_until("the resumed node reaches the source's tip", || {
        node.call("getbestblockhash", "[]").result().trim_matches('"') == best
    });
    wait_until("the import is reported", || {
        read(&debug_log(dest)).get(before..).is_some_and(|l| l.contains(REPORTED))
    });
    node
}

fn kill_then_resume(case: &str, point: &str, until: Option<&str>) {
    let home = RetainedHome::new(&format!("ch6a-{case}-"));
    let (base, best) = source(home.path());
    let point = point.replace("<best>", &best);
    let dest = home.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let env = [("RDASHD_TEST_HOOK", "datadir-import"), ("RDASHD_TEST_HOOK_POINT", point.as_str())];
    let (status, log) = hooked(home.path(), &dest, &base, &env, until);
    let expected = if until.is_some() {
        0
    } else {
        3
    };
    assert_eq!(status, Some(expected), "{case}: {log}");
    assert!(!log.contains(REPORTED), "{case}: {log}");
    match case {
        "reported" => {
            let report = dest.join("regtest/import-report.json");
            assert!(report.exists(), "{case}: the report is durable before the kill");
        }
        "hold" => {
            let line = "Core datadir import: incomplete (shutdown requested)";
            assert!(log.contains(line), "{case}: {log}");
        }
        _ => {}
    }
    resumed(home.path(), &dest, &best).stop();
}

#[test]
fn a_kill_in_the_reindex_scan_resumes() {
    kill_then_resume("scan", "kill-in-scan:0", None);
}

#[test]
fn a_kill_as_the_source_tip_connects_resumes() {
    kill_then_resume("block", "kill-at-block:<best>", None);
}

#[test]
fn a_kill_before_the_reported_record_resumes() {
    kill_then_resume("reported", "kill-before-reported", None);
}

#[test]
fn a_shutdown_held_at_the_source_tip_is_incomplete_and_resumes() {
    kill_then_resume(
        "hold",
        "hold-at-block:<best>",
        Some("test-hooks: datadir import held at block"),
    );
}

/// `AbortNode` before the reindex's clear (the chainstate engine's next durable write fails):
/// the node exits before P5, and the next start resumes the stored reindex and reports. The
/// restart's index matches a plain import (DEC-1620).
#[test]
fn an_abort_before_the_reindex_clear_resumes_to_a_plain_import() {
    let home = RetainedHome::new("ch6a-abort-");
    let (base, best) = source(home.path());
    let dest = home.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let env = [("RDASHD_TEST_HOOK", "abort-after-scan")];
    let (status, log) = hooked(home.path(), &dest, &base, &env, None);
    assert!(status.is_some(), "{log}");
    assert!(!log.contains(REPORTED), "{log}");
    let node = resumed(home.path(), &dest, &best);
    let tips = node.call("getchaintips", "[]").result().to_owned();
    assert!(!tips.contains("invalid") && !tips.contains("conflicting"), "{tips}");
    assert_eq!(tips.matches("\"status\"").count(), 1, "{tips}");
    node.stop();
}

/// `RDASHD_TEST_NO_REFLINK=1`: every FICLONE fails and the block files are copied.
#[test]
fn without_reflink_the_block_files_are_copied() {
    let home = RetainedHome::new("ch6a-copy-");
    let (base, _) = source(home.path());
    let dest = home.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let env = [("RDASHD_TEST_NO_REFLINK", "1")];
    let (status, log) = hooked(home.path(), &dest, &base, &env, Some(REPORTED));
    assert_eq!(status, Some(0), "{log}");
    let line = log.lines().find(|l| l.contains(REPORTED)).unwrap_or_else(|| panic!("{log}"));
    let report = line.split(REPORTED).nth(1).unwrap().trim();
    let report: serde_json::Value = serde_json::from_str(&read(Path::new(report))).unwrap();
    let methods = report["by_method"].as_object().unwrap();
    assert_eq!(methods.keys().collect::<Vec<_>>(), ["copy"], "{report}");
}
