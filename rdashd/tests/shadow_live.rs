//! O15-2's regtest end-to-end: a StateDigest dashd (`O15_PEER_DASHD`) mines, rdashd follows it
//! over P2P with `-statedigestlog`, and the live `dash-conformance shadow` follows both logs and
//! both RPC endpoints. `STATEDIGEST_CONFORMANCE` selects the conformance binary when it is not
//! beside rdashd.
#![cfg(unix)]

mod common;

use std::env;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::node::{Node, Storage};
use common::*;
use serde_json::Value;

fn conformance() -> PathBuf {
    let path = env::var_os("STATEDIGEST_CONFORMANCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(RDASHD).parent().unwrap().join("dash-conformance"));
    assert!(
        path.exists(),
        "{} is missing: `cargo build -p dash-conformance` or set STATEDIGEST_CONFORMANCE",
        path.display()
    );
    path
}

/// The shadow process, killed if the test fails before it stops it.
struct ShadowProcess(Option<Child>);

impl ShadowProcess {
    fn stop(mut self) -> ExitStatus {
        let mut child = self.0.take().unwrap();
        signal(child.id(), libc::SIGTERM);
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("the shadow did not exit on SIGTERM");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ShadowProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn state(out: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(out.join("state.json")).ok()?).ok()
}

fn checkpoint(out: &Path) -> Option<i64> {
    state(out)?["checkpoint"]["h"].as_i64()
}

fn jsonl(path: &Path) -> Vec<Value> {
    read(path).lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn mine(node: &Node, count: u32, script: &str) {
    node.call("generatetodescriptor", &format!(r#"[{count},"raw({script})"]"#));
}

fn height(node: &Node) -> i64 {
    node.call("getblockcount", "[]").parse().unwrap()
}

fn synced(peer: &Node, ours: &Node) {
    wait_until("rdashd to follow the peer's tip", || ours.best() == peer.best());
}

fn wait_for(
    what: &str,
    limit: Duration,
    shadow: &mut ShadowProcess,
    mut done: impl FnMut() -> bool,
) {
    let deadline = Instant::now() + limit;
    while !done() {
        let child = shadow.0.as_mut().unwrap();
        assert!(child.try_wait().unwrap().is_none(), "the shadow exited waiting for {what}");
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn shadow_is_clean_over_200_blocks_with_a_reorg() {
    let Some(dashd) = env::var_os("O15_PEER_DASHD").map(PathBuf::from) else {
        eprintln!("skipped: set O15_PEER_DASHD to a StateDigest dashd to run the O15 end-to-end");
        return;
    };
    let conformance = conformance();
    let home = RetainedHome::new("o15-2-shadow");
    let p2p = free_port();
    let bind = format!("-bind=127.0.0.1:{p2p}");
    let mut peer = Node::new(
        "peer",
        &dashd,
        home.path(),
        &[
            &bind,
            "-listenonion=0",
            "-dnsseed=0",
            "-fixedseeds=0",
            "-coinstatsindex=1",
            "-statedigestlog=digest.jsonl",
        ],
    );
    let connect = format!("-connect=127.0.0.1:{p2p}");
    let mut ours = Node::rdashd(
        home.path(),
        &[&connect, "-listen=0", "-statedigestlog=digest.jsonl"],
        Storage::Live,
    );
    peer.start(0);
    ours.start(0);
    let peer_port = peer.run.as_ref().unwrap().1;
    let our_port = ours.run.as_ref().unwrap().1;

    mine(&peer, 100, "51");
    synced(&peer, &ours);

    let out = home.path().join("shadow");
    let hook_marker = home.path().join("on-crit.marker");
    let node_password = home.path().join("node-password");
    fs::write(&node_password, "p\n").unwrap();
    fs::set_permissions(&node_password, fs::Permissions::from_mode(0o600)).unwrap();
    let mut shadow = ShadowProcess(Some(
        Command::new(&conformance)
            .arg("shadow")
            .arg("--left")
            .arg(peer.datadir.join("regtest/digest.jsonl"))
            .arg("--right")
            .arg(ours.datadir.join("regtest/digest.jsonl"))
            .arg("--out")
            .arg(&out)
            .args(["--peer-rpc-url", &format!("http://127.0.0.1:{peer_port}")])
            .args(["--peer-user", "u", "--peer-password", "p"])
            .args(["--node-rpc-url", &format!("http://127.0.0.1:{our_port}")])
            .args(["--node-user", "u", "--poll-secs", "1"])
            .arg("--node-password-file")
            .arg(&node_password)
            .arg("--on-crit")
            .arg(format!("touch {}", sh_quote(&hook_marker)))
            .stdin(Stdio::null())
            .stdout(File::create(home.path().join("shadow.stdout")).unwrap())
            .stderr(File::create(home.path().join("shadow.stderr")).unwrap())
            .spawn()
            .unwrap(),
    ));
    let limit = Duration::from_secs(300);
    wait_for("the shadow to settle height 94", limit, &mut shadow, || checkpoint(&out) == Some(94));

    // A reorg above the settled height: the peer's top three blocks are replaced by a longer
    // branch with another coinbase script, which rdashd follows.
    let fork = peer.call("getblockhash", "[98]");
    assert_eq!(peer.call("invalidateblock", &format!("[{fork}]")), "null");
    mine(&peer, 4, "52");
    assert_eq!(height(&peer), 101);
    synced(&peer, &ours);
    assert_ne!(peer.call("getblockhash", "[98]"), fork);

    mine(&peer, 105, "51");
    assert_eq!(height(&peer), 206);
    synced(&peer, &ours);
    wait_for("the shadow to settle height 200", limit, &mut shadow, || {
        checkpoint(&out) == Some(200)
    });

    let settled = peer.call("getblockhash", "[200]");
    let status = shadow.stop();
    assert_eq!(status.code(), Some(0), "{}", read(&home.path().join("shadow.stderr")));
    peer.stop();
    ours.stop();

    let state = state(&out).unwrap();
    assert_eq!(state["checkpoint"]["h"].as_i64(), Some(200));
    assert_eq!(state["checkpoint"]["hash"].as_str(), Some(settled.trim_matches('"')));
    let alarms = jsonl(&out.join("alarms.jsonl"));
    let crits: Vec<_> = alarms.iter().filter(|a| a["class"].get("Crit").is_some()).collect();
    assert!(crits.is_empty(), "CRIT alarms: {crits:?}");
    assert!(!hook_marker.exists(), "--on-crit ran without a CRIT");
    // Both sides saw the reorg: three rewinds each, heights 100, 99 and 98 popped.
    let events = jsonl(&out.join("events.jsonl"));
    for side in 0..2 {
        let rewinds: Vec<_> = events
            .iter()
            .filter(|e| e["event"] == "rewind" && e["side"] == side)
            .map(|e| e["from"].as_i64().unwrap())
            .collect();
        assert_eq!(rewinds, [100, 99, 98], "side {side} rewinds: {events:?}");
    }
    assert!(events.iter().any(|e| e["event"] == "settle" && e["mode"] == "depth"));
    let line = read(&out.join("STATUS.line"));
    assert!(line.starts_with("O15 shadow "), "{line}");
    assert!(line.contains(" settled=200 "), "{line}");
    assert!(line.contains(" alarms=0/"), "{line}");
    eprintln!("{}", line.trim_end());
    let mut warns = std::collections::BTreeMap::<String, usize>::new();
    for alarm in &alarms {
        *warns.entry(alarm["class"]["Warn"].to_string()).or_default() += 1;
    }
    eprintln!("WARN alarms by class: {warns:?}");
}
