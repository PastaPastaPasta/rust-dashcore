//! Exercise watcher startup and exit status without a running node.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn watch(nodes: &Path, out: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mixnet-watch"))
        .arg("--nodes")
        .arg(nodes)
        .arg("--out")
        .arg(out)
        .args(args)
        .output()
        .unwrap()
}

fn diagnostic(output: Output, code: i32) -> String {
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(code), "{stderr}");
    assert!(stderr.contains("mixnet-watch:"), "{stderr}");
    stderr
}

fn nodes_json() -> Value {
    json!({
        "v":1,"run_id":"cli","run":{
            "run_kind":"rehearsal","context":"regtest","preset":"modeb5","caps":[],
            "host":{"nproc":1,"mem":1,"disk":1},"clock":{"source":"monotonic","offset_ms":0},
            "binaries":{"dashd":{"sha256":"01".repeat(32),"version":"test"},"rdashd":null}
        },
        "nodes":[{"slot":"node0","runs":"dashd","rpc":"127.0.0.1:1","cookie":"unused",
            "chainlocks":false,"chaintips":false}],
        "llmq_params":[],"coinbase":{}
    })
}

#[test]
fn bad_arguments_exit_two_with_usage() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = dir.path().join("nodes.json");
    let out = dir.path().join("obs.jsonl");
    for args in [
        &["--unknown", "value"][..],
        &["positional"],
        &["--events"],
        &["--events", "--out"],
        &["--events", "-file"],
        &["--out", "again"],
        &["--poll-ms", "abc"],
        &["--poll-ms", "0"],
        &["--page-cmd"],
        &["--page-cmd", "-"],
        &["--page-cmd", "-cmd"],
        &["--page-cmd", "--out"],
        &["--page-cmd", "a", "--page-cmd", "b"],
    ] {
        assert!(
            diagnostic(watch(&nodes, &out, args), 2).contains("usage: mixnet-watch"),
            "{args:?}"
        );
    }
    assert!(diagnostic(watch(&nodes, Path::new("-"), &[]), 2).contains("usage: mixnet-watch"));
}

#[test]
fn stdin_events_are_accepted_and_bad_nodes_exit_two() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = dir.path().join("nodes.json");
    let out = dir.path().join("obs.jsonl");
    let mut cfg = nodes_json();
    cfg["nodes"] = json!([]);
    fs::write(&nodes, cfg.to_string()).unwrap();
    let err = diagnostic(watch(&nodes, &out, &["--events", "-"]), 2);
    assert!(err.contains("first node must be node0"), "{err}");
    assert!(!out.exists());
}

#[test]
fn run_mismatch_and_malformed_resume_log_exit_two() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = dir.path().join("nodes.json");
    let out = dir.path().join("obs.jsonl");
    let cfg = nodes_json();
    fs::write(&nodes, cfg.to_string()).unwrap();
    let mut run = cfg["run"].clone();
    for (key, value) in json!({"v":1,"run":"other","seq":1,"mono_ns":1,
        "src":"watch","kind":"Run"})
    .as_object()
    .unwrap()
    {
        run[key] = value.clone();
    }
    for (line, expected) in
        [(format!("{run}\n"), "run id differs"), ("broken\n".into(), "expected value")]
    {
        fs::write(&out, &line).unwrap();
        let err = diagnostic(watch(&nodes, &out, &["--events", "missing-events"]), 2);
        assert!(err.contains(expected), "{err}");
        assert_eq!(fs::read_to_string(&out).unwrap(), line);
    }
}

#[test]
fn a_page_file_name_as_out_exits_two_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = dir.path().join("nodes.json");
    let calls = dir.path().join("calls.txt");
    let script = dir.path().join("page.sh");
    fs::write(&script, format!("#!/bin/sh\necho \"$1\" >> '{}'\n", calls.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&nodes, nodes_json().to_string()).unwrap();
    for name in [
        "PAGE-4-digest-mismatch.json",
        ".PAGE-4-digest-mismatch.json.tmp",
        "page-4-digest-mismatch.json",
        ".Page-4-digest-mismatch.json.tmp",
    ] {
        let out = dir.path().join(name);
        fs::write(&out, "kept\n").unwrap();
        let err = diagnostic(
            watch(&nodes, &out, &["--events", "-", "--page-cmd", script.to_str().unwrap()]),
            2,
        );
        assert!(err.contains("page file"), "{err}");
        assert_eq!(fs::read_to_string(&out).unwrap(), "kept\n");
    }
    assert!(!calls.exists());
}

#[test]
fn log_write_failure_exits_one() {
    let full = Path::new("/dev/full");
    if !full.exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let nodes = dir.path().join("nodes.json");
    fs::write(&nodes, nodes_json().to_string()).unwrap();
    diagnostic(watch(&nodes, full, &["--events", "-"]), 1);
}
