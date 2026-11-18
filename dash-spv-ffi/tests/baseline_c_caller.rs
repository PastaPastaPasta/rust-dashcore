//! A C caller compiled against the baseline headers in `ffi-c-tests/baseline` and linked to
//! today's library syncs from regtest dashd, follows a reorg and shuts down.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use dash_spv::test_utils::{retain_test_dir, DashdTestContext, TestChain};
use tempfile::TempDir;

/// How long the caller may run before the test kills it. A normal run takes about a minute, and
/// the caller's own two sync waits give up after 300 s each, so only a hang reaches it.
const CALLER_DEADLINE: Duration = Duration::from_secs(900);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Builds the cdylib with `cargo build`, as a C caller links it, and returns its directory.
fn build_library() -> PathBuf {
    let root = workspace_root();
    let cargo = env!("CARGO");
    let status = Command::new(cargo)
        .args(["build", "-p", "dash-spv-ffi"])
        .current_dir(&root)
        .status()
        .expect("run cargo build");
    assert!(status.success(), "cargo build -p dash-spv-ffi failed");
    let metadata = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(&root)
        .output()
        .expect("run cargo metadata");
    assert!(metadata.status.success(), "cargo metadata failed");
    let metadata = String::from_utf8(metadata.stdout).unwrap();
    let key = "\"target_directory\":\"";
    let start = metadata.find(key).expect("target_directory in cargo metadata") + key.len();
    let end = start + metadata[start..].find('"').unwrap();
    PathBuf::from(&metadata[start..end]).join("debug")
}

fn compile(source: &Path, include: &Path, lib_dir: &Path, out: &Path) {
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let output = Command::new(&cc)
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(include)
        .arg(source)
        .arg("-L")
        .arg(lib_dir)
        .arg("-ldash_spv_ffi")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(out)
        .output()
        .expect("run the C compiler");
    assert!(
        output.status.success(),
        "{cc} failed on {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn count(lines: &[String], name: &str) -> u32 {
    let prefix = format!("COUNT {name} ");
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {name} count in {lines:?}"))
        .parse()
        .unwrap()
}

/// Returns the child's exit status, or `None` if it is still running at `deadline`.
fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn baseline_c_caller() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let Some(dashd) = rt.block_on(DashdTestContext::new(TestChain::Minimal)) else {
        return;
    };
    assert!(dashd.supports_mining, "the reorg needs dashd's RPC miner");

    let lib_dir = build_library();
    let work = TempDir::new().unwrap();
    let caller = work.path().join("caller");
    let tests_dir = workspace_root().join("ffi-c-tests");
    compile(
        &tests_dir.join("baseline-caller/caller.c"),
        &tests_dir.join("baseline"),
        &lib_dir,
        &caller,
    );

    let data_dir = work.path().join("data");
    fs::create_dir(&data_dir).unwrap();
    let initial_height = dashd.initial_height;
    let mut child = Command::new(&caller)
        .arg(&data_dir)
        .arg(dashd.addr.to_string())
        .arg(initial_height.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the caller");
    let deadline = Instant::now() + CALLER_DEADLINE;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut lines = Vec::new();
    let status = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                if let Some(tip) = line.strip_prefix("SYNCED ") {
                    let tip: u32 = tip.parse().unwrap();
                    assert!(tip >= initial_height, "synced below dashd: {line}");
                    let node = &dashd.node;
                    let miner = node.get_new_address();
                    node.invalidate_block(&node.get_best_block_hash());
                    node.generate_blocks(3, &miner);
                    writeln!(stdin, "REORG {}", node.get_block_count()).unwrap();
                    stdin.flush().unwrap();
                }
                lines.push(line);
            }
            Err(RecvTimeoutError::Disconnected) => break wait_until(&mut child, deadline),
            Err(RecvTimeoutError::Timeout) => break None,
        }
    };
    let Some(status) = status else {
        child.kill().unwrap();
        child.wait().unwrap();
        retain_test_dir(work.path(), "caller");
        panic!("the caller ran past {CALLER_DEADLINE:?} and was killed: {lines:?}");
    };
    if !status.success() {
        retain_test_dir(work.path(), "caller");
    }
    assert!(status.success(), "the caller failed ({status}): {lines:?}");
    assert!(lines.iter().any(|l| l == "DONE"), "the caller never stopped: {lines:?}");

    let reorg_height = initial_height + 2;
    assert!(count(&lines, "sync_start") > 0);
    assert!(count(&lines, "headers_stored") > 0);
    assert!(count(&lines, "header_tip") >= reorg_height);
    assert!(count(&lines, "filter_headers_stored") > 0);
    assert!(count(&lines, "filter_header_tip") >= reorg_height);
    assert!(count(&lines, "sync_complete") > 0);
    assert!(count(&lines, "sync_complete_tip") >= initial_height);
    assert!(count(&lines, "peer_connected") > 0);
    assert!(count(&lines, "peers_updated") > 0);
    assert!(count(&lines, "best_height") >= initial_height);
    assert!(count(&lines, "progress") > 0);
    assert_eq!(count(&lines, "client_error"), 0);
    assert_eq!(count(&lines, "bad_value"), 0);
}
