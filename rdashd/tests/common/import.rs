//! The block import's two-node helpers: the rdashd and dashd pair, their records and blk files,
//! and the launches a reindex or a `-loadblock` start runs.

use std::path::Path;
use std::process::Output;
use std::time::Instant;

use super::fifo::{BOUND, blk_records, within, write_blk};
use super::mine::{Mined, parse_hex};
use super::node::{Fork, Node, Storage, genesis, shown};
use super::{RetainedHome, Running, free_port, regtest_on, require_dashd, without_core_file};

/// The two nodes, rdashd first, on fresh datadirs in `home`, not started.
pub fn unstarted(home: &RetainedHome, flags: &[&str]) -> [Node; 2] {
    let dashd = require_dashd();
    [
        Node::rdashd(home.path(), flags, Storage::Live),
        Node::new("dashd", &dashd, home.path(), flags),
    ]
}

/// The two nodes, rdashd first, started with `flags`.
pub fn pair(home: &RetainedHome, flags: &[&str]) -> [Node; 2] {
    let mut nodes = unstarted(home, flags);
    for node in &mut nodes {
        node.start(0);
    }
    nodes
}

/// Both nodes on F1..F`n`, submitted alike, then stopped: each `blk00000.dat` holds genesis..F`n`.
pub fn mined(home: &RetainedHome, n: usize) -> ([Node; 2], Fork) {
    let mut nodes = pair(home, &[]);
    let fork = Fork::new(&genesis(&nodes[1]));
    for block in &fork.f[..n] {
        let want = nodes[1].submit(block);
        assert_eq!(nodes[0].submit(block), want);
    }
    assert_eq!(nodes[0].best(), shown(&fork.f[n - 1].hash));
    assert_eq!(nodes[1].best(), shown(&fork.f[n - 1].hash));
    for node in &mut nodes {
        node.stop();
    }
    (nodes, fork)
}

/// `blk00000.dat`'s records, the same on both nodes.
pub fn records(nodes: &[Node; 2]) -> Vec<Vec<u8>> {
    let records = blk_records(&nodes[0], 0);
    assert_eq!(records, blk_records(&nodes[1], 0), "both nodes' records");
    records
}

/// `block`'s record: `magic ‖ size ‖ block`, `magic` as `records`' first holds it.
pub fn record(records: &[Vec<u8>], block: &Mined) -> Vec<u8> {
    let bytes = parse_hex(&block.hex);
    let mut out = records[0][..4].to_vec();
    out.extend(u32::try_from(bytes.len()).unwrap().to_le_bytes());
    out.extend(bytes);
    out
}

/// Writes `files` as `blk00000.dat`, `blk00001.dat`, … on both nodes.
pub fn write_files(nodes: &[Node; 2], files: &[Vec<Vec<u8>>]) {
    for node in nodes {
        for (n, records) in files.iter().enumerate() {
            write_blk(node, u32::try_from(n).unwrap(), records);
        }
    }
}

/// Waits for `node`'s tip to be `want` within [`BOUND`].
pub fn tip_is(node: &Node, want: &[u8; 32]) {
    within(&format!("{} at its tip", node.name), || node.best() == shown(want));
}

/// Whether `path` names anything, a FIFO included.
pub fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// [`Node::start`] with `flags` for this launch only.
pub fn start_with(node: &mut Node, flags: &[&str]) {
    let kept = node.flags.len();
    node.flags.extend(flags.iter().map(|f| (*f).to_owned()));
    node.start(0);
    node.flags.truncate(kept);
}

/// `node` started with `flags`, run until it exits by itself within [`BOUND`]: its output and
/// this launch's log. No wait for init: the exit may come first, and an abort leaves no core
/// file.
pub fn run_to_exit(node: &mut Node, flags: &[&str]) -> (Output, String) {
    assert!(node.run.is_none(), "{} runs", node.name);
    node.offset = node.log_len();
    let home = node.datadir.parent().unwrap().to_owned();
    let rpcport = format!("-rpcport={}", free_port());
    let mut args = vec!["-printtoconsole=0", &rpcport, "-dbcrashratio=0"];
    args.extend(node.flags.iter().map(String::as_str));
    args.extend(flags);
    let mut cmd = regtest_on(&node.binary, &home, &node.datadir, &args);
    without_core_file(&mut cmd);
    let mut running = Running::spawn(cmd, node.log());
    let end = Instant::now() + BOUND;
    while !running.exited() {
        assert!(Instant::now() < end, "{} exits within {BOUND:?}", node.name);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    (running.wait(), node.new_log())
}

/// Sets each path to mode 0600 when dropped, on a panic too, so a retained home keeps no file its
/// owner cannot read.
pub struct ModeGuard(pub Vec<std::path::PathBuf>);

impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        for path in &self.0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}
