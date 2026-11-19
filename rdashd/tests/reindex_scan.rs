//! A reindex's scan of the blk files (CH5-3b-2) against dashd v24.0.0: each case writes the same
//! crafted `blkNNNNN.dat` files on one rdashd and one dashd (the nodes' own records, split or
//! reordered) and compares the scan's lines and the end state. Pauses between two files are a
//! FIFO's (`common::fifo`). They cannot be skipped: with `SKIP_DASHD_TESTS` set they fail
//! (`common::require_dashd`). Linux-only, as `common::fifo` is.
#![cfg(target_os = "linux")]

mod common;

use std::process::Output;
use std::time::Instant;

use common::fifo::*;
use common::mine::*;
use common::node::*;
use common::peer::Peer;
use common::*;

const REINDEX: &str = "-reindex";
const DEBUG: &str = "-debug=reindex";
const FINISHED: &str = "Reindexing finished";
const EXIT: &str = "Shutdown requested. Exit ThreadImport";
const OUT_OF_ORDER: &str = "LoadExternalBlockFile: Out of order block";
const CHILD: &str = "LoadExternalBlockFile: Processing out of order child";
const REFUSED: &str = "rdashd is in initial sync and waiting for blocks...";
/// rpc_generate.py's regtest address of the generator point's key.
const ADDRESS: &str = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";

/// The two nodes, rdashd first, started with `flags`.
fn pair(home: &RetainedHome, flags: &[&str]) -> [Node; 2] {
    let dashd = require_dashd();
    let mut nodes = [
        Node::rdashd(home.path(), flags, Storage::Live),
        Node::new("dashd", &dashd, home.path(), flags),
    ];
    for node in &mut nodes {
        node.start(0);
    }
    nodes
}

/// Both nodes on F1..F`n`, submitted alike, then stopped: each `blk00000.dat` holds genesis..F`n`.
fn mined(home: &RetainedHome, n: usize) -> ([Node; 2], Fork) {
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
fn records(nodes: &[Node; 2]) -> Vec<Vec<u8>> {
    let records = blk_records(&nodes[0], 0);
    assert_eq!(records, blk_records(&nodes[1], 0), "both nodes' records");
    records
}

/// `block`'s record: `magic ‖ size ‖ block`, `magic` as `records`' first holds it.
fn record(records: &[Vec<u8>], block: &Mined) -> Vec<u8> {
    let bytes = parse_hex(&block.hex);
    let mut out = records[0][..4].to_vec();
    out.extend(u32::try_from(bytes.len()).unwrap().to_le_bytes());
    out.extend(bytes);
    out
}

/// Writes `files` as `blk00000.dat`, `blk00001.dat`, … on both nodes.
fn write_files(nodes: &[Node; 2], files: &[Vec<Vec<u8>>]) {
    for node in nodes {
        for (n, records) in files.iter().enumerate() {
            write_blk(node, u32::try_from(n).unwrap(), records);
        }
    }
}

/// [`Node::start`] with `flags` for this launch only.
fn start_with(node: &mut Node, flags: &[&str]) {
    let kept = node.flags.len();
    node.flags.extend(flags.iter().map(|f| (*f).to_owned()));
    node.start(0);
    node.flags.truncate(kept);
}

/// `node` started with `flags`, run until it exits by itself within [`BOUND`]: its output and
/// this launch's log. No wait for init: the exit may come first.
fn run_to_exit(node: &mut Node, flags: &[&str]) -> (Output, String) {
    assert!(node.run.is_none(), "{} runs", node.name);
    node.offset = node.log_len();
    let home = node.datadir.parent().unwrap().to_owned();
    let rpcport = format!("-rpcport={}", free_port());
    let mut args = vec!["-printtoconsole=0", &rpcport, "-dbcrashratio=0"];
    args.extend(node.flags.iter().map(String::as_str));
    args.extend(flags);
    let cmd = regtest_on(&node.binary, &home, &node.datadir, &args);
    let mut running = Running::spawn(cmd, node.log());
    let end = Instant::now() + BOUND;
    while !running.exited() {
        assert!(Instant::now() < end, "{} exits within {BOUND:?}", node.name);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    (running.wait(), node.new_log())
}

/// The scan's lines in `log`, in log order, without their time or the "Loaded" line's
/// duration: each file's line, its records' `-debug=reindex` lines, the "Loaded" line, the check
/// after the file, and the clear's.
fn scan_lines(log: &str) -> Vec<String> {
    texts(log).into_iter().filter_map(scan_line).collect()
}

/// `line` (a line of [`texts`]) as [`scan_lines`] keeps it, if it is one of the scan's.
fn scan_line(line: &str) -> Option<String> {
    let l = body(line);
    let kept = l.starts_with("Reindexing block file ")
        || is_loaded(l)
        || l == FINISHED
        || l == EXIT
        || l.starts_with("LoadExternalBlockFile: ")
        || l.starts_with("Block Import: ")
        || l.starts_with("ReadBlockFromDisk: ");
    kept.then(|| {
        if is_loaded(line) {
            loaded_count(line)
        } else {
            line
        }
        .to_owned()
    })
}

/// Waits for `node`'s tip to be `want` within [`BOUND`].
fn tip_is(node: &Node, want: &[u8; 32]) {
    within(&format!("{} at its tip", node.name), || node.best() == shown(want));
}

/// A node's end state: the tip, `getchaintips` and `gettxoutsetinfo`.
type EndState = (String, Vec<Tip>, Vec<(String, String)>);

/// What `node` ends with.
fn end_state(node: &Node) -> EndState {
    (node.best(), node.tips(), node.utxo_set())
}

/// `feature_reindex.py`'s `txindex=0` cycles: `-reindex`, then `-reindex-chainstate`, over
/// F1..F12. Both nodes end on the same tip and coins, and log the same scan: file 0's line, its
/// 13 blocks, then the clear, for `-reindex`; nothing for `-reindex-chainstate`.
// Core: node/blockstorage.cpp:900-928, validation.cpp:5624 at v24.0.0
#[test]
fn feature_reindex_cycles_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-cycles-");
    let (mut nodes, fork) = mined(&home, 12);
    for flag in [REINDEX, "-reindex-chainstate"] {
        let mut logs = Vec::new();
        let mut ends = Vec::new();
        for node in &mut nodes {
            start_with(node, &[flag]);
            tip_is(node, &fork.f[11].hash);
            ends.push(end_state(node));
            node.stop();
            logs.push(scan_lines(&node.new_log()));
        }
        assert_eq!(logs[0], logs[1], "{flag}: rdashd's lines and dashd's");
        assert_eq!(ends[0], ends[1], "{flag}: the end state");
        if flag == REINDEX {
            let want = [
                "Reindexing block file blk00000.dat...",
                "Loaded 13 blocks from external file",
                FINISHED,
            ];
            assert_eq!(logs[0], want);
        } else {
            assert_eq!(logs[0], Vec::<String>::new());
        }
    }
}

/// `feature_reindex.py:67`'s swap of the first two blocks after genesis: F2's record before F1's.
/// F2 is mapped as an out-of-order block and imported once F1 is, with the same
/// `-debug=reindex` lines on both nodes.
// Core: validation.cpp:5508-5515, :5576-5606 at v24.0.0
#[test]
fn out_of_order_records_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-order-");
    let (mut nodes, fork) = mined(&home, 12);
    let mut records = records(&nodes);
    records.swap(1, 2);
    write_files(&nodes, &[records]);
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX, DEBUG]);
        tip_is(node, &fork.f[11].hash);
        node.stop();
        logs.push(scan_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert!(logs[0].iter().any(|l| body(l).starts_with(OUT_OF_ORDER)), "{:?}", logs[0]);
    assert!(logs[0].iter().any(|l| body(l).starts_with(CHILD)), "{:?}", logs[0]);
}

/// A child in `blk00000.dat` whose parent is in `blk00001.dat`: the map of blocks with unknown
/// parents is carried across files, and the children are imported after the parent.
// Core: node/blockstorage.cpp:905, validation.cpp:5576-5606 at v24.0.0
#[test]
fn children_across_files_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-across-");
    let (mut nodes, fork) = mined(&home, 12);
    let mut file0 = records(&nodes);
    let parent = file0.remove(2);
    write_files(&nodes, &[file0, vec![parent]]);
    let mut logs = Vec::new();
    let mut ends = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX, DEBUG]);
        tip_is(node, &fork.f[11].hash);
        ends.push(end_state(node));
        node.stop();
        logs.push(scan_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(ends[0], ends[1]);
    let file1 = logs[0].iter().position(|l| *l == reindexing_line(1)).expect("file 1");
    assert!(logs[0][file1..].iter().any(|l| body(l).starts_with(CHILD)), "{:?}", logs[0]);
}

/// `blk00000.dat` ends with F7, its header intact and its body corrupted at the same length (a
/// transaction count over `MAX_SIZE`); `blk00001.dat` is F6. F7 is mapped by its header, and its
/// read fails once F6 is imported: the same read line on both, no child line for it, the tip F6,
/// and F7 unknown.
// Core: validation.cpp:5508-5515, :5590-5591; node/blockstorage.cpp:820-824 at v24.0.0
#[test]
fn a_child_whose_body_does_not_read_matches_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-unread-");
    let (mut nodes, fork) = mined(&home, 7);
    let records = records(&nodes);
    let mut file0 = records[..=5].to_vec();
    let mut f7 = records[7].clone();
    f7[88..93].copy_from_slice(&[0xfe, 0xff, 0xff, 0xff, 0xff]);
    file0.push(f7);
    write_files(&nodes, &[file0, vec![records[6].clone()]]);
    let mut logs = Vec::new();
    let mut ends = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX, DEBUG]);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, &fork.f[5].hash);
        let header = node.try_call("getblockheader", &format!("[{}]", shown(&fork.f[6].hash)));
        let header = header.expect("an answer");
        let (_, error) = header.error();
        assert!(error.contains("\"code\":-5"), "{}: {error}", node.name);
        ends.push(end_state(node));
        node.stop();
        logs.push(scan_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(ends[0], ends[1]);
    let read = logs[0].iter().filter(|l| body(l).starts_with("ReadBlockFromDisk: ")).count();
    assert_eq!(read, 1, "{:?}", logs[0]);
    let f7 = rpc_hex(&fork.f[6].hash);
    assert!(
        !logs[0].iter().any(|l| body(l).starts_with(CHILD) && l.contains(&f7)),
        "{:?}",
        logs[0]
    );
    assert!(!ends[0].1.iter().any(|t| t.hash == f7));
}

/// Known records: a duplicate in the file and a second genesis record at its end. "Loaded"
/// counts neither, and the genesis copy's `ActivateBestChain` runs.
// Core: validation.cpp:5517-5552 at v24.0.0
#[test]
fn known_records_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-known-");
    let (mut nodes, fork) = mined(&home, 12);
    let mut file0 = records(&nodes);
    let duplicate = file0[3].clone();
    file0.insert(7, duplicate);
    file0.push(file0[0].clone());
    write_files(&nodes, &[file0]);
    let mut logs = Vec::new();
    let mut ends = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX, DEBUG]);
        tip_is(node, &fork.f[11].hash);
        ends.push(end_state(node));
        node.stop();
        logs.push(scan_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(ends[0], ends[1]);
    assert!(logs[0].iter().any(|l| l == "Loaded 13 blocks from external file"), "{:?}", logs[0]);
}

/// Malformed records: bytes between records, a size field out of range, a block whose body does
/// not decode, and a truncated record at the file's end. The scan skips each as Core does, with
/// the same "unexpected data" lines.
// Core: validation.cpp:5465-5490, :5611-5616 at v24.0.0
#[test]
fn malformed_records_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-malformed-");
    let (mut nodes, fork) = mined(&home, 12);
    let records = records(&nodes);
    let magic = records[0][..4].to_vec();
    let mut bad_size = magic.clone();
    bad_size.extend(0x7fff_ffffu32.to_le_bytes());
    let mut undecodable = magic.clone();
    let mut body = parse_hex(&fork.f[3].hex)[..80].to_vec();
    body.extend([0xfe, 0xff, 0xff, 0xff, 0xff, 0, 0, 0]);
    undecodable.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
    undecodable.extend(body);
    let truncated = records[12][..records[12].len() / 2].to_vec();
    let mut file0 = vec![records[0].clone(), records[1].clone(), b"junk junk".to_vec()];
    file0.extend([records[2].clone(), bad_size, records[3].clone(), undecodable]);
    file0.extend(records[4..12].iter().cloned());
    file0.push(truncated);
    write_files(&nodes, &[file0]);
    let mut logs = Vec::new();
    let mut ends = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX, DEBUG]);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, &fork.f[10].hash);
        ends.push(end_state(node));
        node.stop();
        logs.push(scan_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(ends[0], ends[1]);
    let unexpected = logs[0].iter().filter(|l| l.contains(": unexpected data at file offset 0x"));
    assert!(unexpected.count() >= 1, "{:?}", logs[0]);
}

/// `blk00000.dat` and `blk00002.dat` present, `blk00001.dat` absent: the scan ends at the
/// missing file, so file 2 is never read and the tip is file 0's.
// Core: node/blockstorage.cpp:908-910 at v24.0.0
#[test]
fn a_missing_file_ends_the_scan_as_on_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-missing-");
    let (mut nodes, fork) = mined(&home, 12);
    let records = records(&nodes);
    for node in &nodes {
        write_blk(node, 0, &records[..=6]);
        write_blk(node, 2, &records[7..]);
    }
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX]);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, &fork.f[5].hash);
        node.stop();
        logs.push(scan_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert!(!logs[0].contains(&reindexing_line(2)), "{:?}", logs[0]);
}

/// `blk00001.dat` present but unreadable (mode 0): each node logs that it cannot open the file
/// and the scan ends there, so file 2 is never read and the tip is file 0's.
// Core: node/blockstorage.cpp:911-914, flatfile.cpp:41-46 at v24.0.0
#[test]
fn an_unopenable_file_ends_the_scan_as_on_dashd_v24() {
    use std::os::unix::fs::PermissionsExt as _;
    assert_ne!(unsafe { libc::geteuid() }, 0, "an unreadable file needs a non-root euid");
    let home = RetainedHome::new("ch5-3b2-unopenable-");
    let (mut nodes, fork) = mined(&home, 12);
    let records = records(&nodes);
    for node in &nodes {
        write_blk(node, 0, &records[..=6]);
        write_blk(node, 1, &records[7..]);
        write_blk(node, 2, &records[7..]);
        let closed = std::fs::Permissions::from_mode(0o000);
        std::fs::set_permissions(blk_path(node, 1), closed).unwrap();
    }
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX]);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, &fork.f[5].hash);
        node.stop();
        let log = node.new_log();
        let unable = format!("Unable to open file {}", blk_path(node, 1).display());
        assert!(texts(&log).contains(&unable.as_str()), "{}: {log}", node.name);
        logs.push(scan_lines(&log));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let file0 = reindexing_line(0);
    let want = [file0.as_str(), "Loaded 7 blocks from external file", FINISHED];
    assert_eq!(logs[0], want);
}

/// The genesis copy in `blk00001.dat` connects to F5 under `-stopatheight=5`, which requests the
/// shutdown: "Loaded 0 blocks" for file 1, then the check's line; `blk00002.dat` is never read,
/// no clear, and the tip is F5 on both. A restart then resumes the scan ([`stopped_at_five`]).
// Core: validation.cpp:3841-3852, :5538-5552; node/blockstorage.cpp:917-920 at v24.0.0
#[test]
fn a_shutdown_between_files_matches_dashd_v24() {
    stopped_at_five("ch5-3b2-between-", |records, genesis| {
        vec![records[..=10].to_vec(), vec![genesis], records[11..].to_vec()]
    });
}

/// As [`a_shutdown_between_files_matches_dashd_v24`], with F11..F12 after the genesis copy in
/// `blk00001.dat`: the record loop returns at the shutdown, so file 1 logs no "Loaded" line.
// Core: validation.cpp:5466 at v24.0.0
#[test]
fn a_shutdown_mid_file_matches_dashd_v24() {
    stopped_at_five("ch5-3b2-midfile-", |records, genesis| {
        vec![records[..=10].to_vec(), [vec![genesis], records[11..].to_vec()].concat()]
    });
}

/// F1..F12 laid out by `files` from the records and the genesis record, then a `-reindex` start
/// under `-stopatheight=5`, run to its own exit on both nodes. Then a restart without `-reindex`
/// on both, which resumes from the stored flag at F5: the same resumed lines and end state, and
/// a rescan that stores only F11..F12 and ends at F12.
fn stopped_at_five(prefix: &str, files: impl Fn(&[Vec<u8>], Vec<u8>) -> Vec<Vec<Vec<u8>>>) {
    let home = RetainedHome::new(prefix);
    let (mut nodes, fork) = mined(&home, 12);
    let records = records(&nodes);
    let files = files(&records, records[0].clone());
    let mid_file = files[1].len() > 1;
    write_files(&nodes, &files);
    let mut logs = Vec::new();
    let mut tips = Vec::new();
    for node in &mut nodes {
        let (out, log) = run_to_exit(node, &[REINDEX, "-stopatheight=5"]);
        assert_eq!(out.status.code(), Some(0), "{}: {}", node.name, stderr(&out));
        let last = texts(&log).into_iter().rev().find(|l| l.starts_with("UpdateTip: ")).unwrap();
        tips.push(last.split(" cache=").next().unwrap().to_owned());
        logs.push(scan_lines(&log));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(tips[0], tips[1], "the last tip");
    let f5 = rpc_hex(&fork.f[4].hash);
    assert!(tips[0].contains(&f5) && tips[0].contains(" height=5 "), "{}", tips[0]);
    let lines = &logs[0];
    let file1 = lines.iter().position(|l| *l == reindexing_line(1)).expect("file 1");
    let after: Vec<&str> = lines[file1 + 1..].iter().map(String::as_str).collect();
    if mid_file {
        assert_eq!(after, [EXIT]);
    } else {
        assert_eq!(after, ["Loaded 0 blocks from external file", EXIT]);
    }
    assert!(!lines.contains(&reindexing_line(2)) && !lines.iter().any(|l| l == FINISHED));
    let mut logs = Vec::new();
    let mut ends = Vec::new();
    for node in &mut nodes {
        node.start(0);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, &fork.f[11].hash);
        ends.push(end_state(node));
        node.stop();
        logs.push(node.new_log());
    }
    assert_eq!(ends[0], ends[1]);
    let resumed: Vec<_> = logs.iter().map(|l| resumed_lines(l)).collect();
    assert_eq!(resumed[0], resumed[1], "rdashd's lines and dashd's");
    let best = format!("Loaded best chain: hashBestChain={f5} height=5");
    assert!(resumed[0].contains(&best), "{:?}", resumed[0]);
    let mut want = vec![reindexing_line(0), "Loaded 0 blocks from external file".to_owned()];
    if !mid_file {
        want.extend([reindexing_line(1), "Loaded 0 blocks from external file".to_owned()]);
    }
    let last = files.len() - 1;
    want.extend([
        reindexing_line(u32::try_from(last).unwrap()),
        "Loaded 2 blocks from external file".to_owned(),
        FINISHED.to_owned(),
    ]);
    assert_eq!(scan_lines(&logs[0]), want);
}

/// A fresh reindex over F1..F12 loads no chain tip and verifies nothing, on both nodes.
// Core: node/chainstate.cpp:214-216 at v24.0.0
#[test]
fn a_fresh_reindex_logs_no_loaded_best_chain_matches_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-fresh-");
    let (mut nodes, fork) = mined(&home, 12);
    for node in &mut nodes {
        start_with(node, &[REINDEX]);
        tip_is(node, &fork.f[11].hash);
        node.stop();
        let log = node.new_log();
        let lines = texts(&log);
        assert!(lines.contains(&FINISHED), "{}: {log}", node.name);
        let loaded = lines.iter().any(|l| l.starts_with("Loaded best chain"));
        let verified = lines.iter().any(|l| l.starts_with("Verifying last"));
        assert!(!loaded && !verified, "{}: {log}", node.name);
    }
}

/// The lines a resumed scan compares, in log order: the chain tip's load, the BLS scheme,
/// VerifyDB's walk and the scan's ([`scan_line`]).
fn resumed_lines(log: &str) -> Vec<String> {
    texts(log)
        .into_iter()
        .filter_map(|l| {
            let start = l.starts_with("Loaded best chain: ")
                || l == STARTUP_SCHEME
                || l.starts_with("Verifying last ")
                || l.starts_with("Verification: ");
            if start {
                Some(l.split(" date=").next().unwrap().to_owned())
            } else {
                scan_line(l)
            }
        })
        .collect()
}

/// A reindex paused at `paused` after `files` (`files[paused]` is the FIFO's place), the coins
/// flushed there by `gettxoutsetinfo`, both nodes killed, then `then` written in the FIFO's
/// place and a restart without `-reindex`, which resumes from the stored flag. Returns each
/// node's resumed log and end state, and the tip at the kill.
fn resumed_after_a_kill(
    nodes: &mut [Node; 2],
    files: &[Vec<Vec<u8>>],
    flushed: bool,
    then: Option<&[Vec<u8>]>,
    want: &[u8; 32],
) -> (Vec<String>, Vec<EndState>, Vec<String>) {
    let paused = u32::try_from(files.len()).unwrap();
    write_files(nodes, files);
    let mut guards = Vec::new();
    for node in nodes.iter_mut() {
        let mut guard = make_fifo(node, paused);
        start_with(node, &[REINDEX]);
        guard.watch(node);
        guards.push(guard);
    }
    let mut at_kill = Vec::new();
    for (node, guard) in nodes.iter_mut().zip(&mut guards) {
        wait_paused(node, paused);
        if flushed {
            node.utxo_set();
        }
        at_kill.push(node.best());
        node.kill();
        guard.unlink();
        if let Some(records) = then {
            write_blk(node, paused, records);
        }
    }
    let mut logs = Vec::new();
    let mut ends = Vec::new();
    for node in nodes.iter_mut() {
        node.start(0);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, want);
        ends.push(end_state(node));
        node.stop();
        logs.push(node.new_log());
    }
    (logs, ends, at_kill)
}

/// The genesis variant of a resume after a kill: `blk00000.dat` is the genesis record alone, a
/// FIFO pauses before `blk00001.dat`, `gettxoutsetinfo` flushes the coins at genesis, then the
/// kill. The restart loads the genesis tip, runs no VerifyDB walk, reads no net cache, rescans
/// `blk00000.dat` (0 blocks) and imports F1..F10 from the new `blk00001.dat`.
// Core: node/chainstate.cpp:214-216; validation.cpp:5051-5053; node/blockstorage.cpp:900-928
// at v24.0.0
#[test]
fn a_resumed_scan_over_a_genesis_coins_tip_matches_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-resume0-");
    let (mut nodes, fork) = mined(&home, 10);
    let records = records(&nodes);
    // F1's previous-block hash: the nodes are stopped.
    let genesis = shown(&parse_hex(&fork.f[0].hex)[4..36].try_into().unwrap());
    let (logs, ends, at_kill) = resumed_after_a_kill(
        &mut nodes,
        &[vec![records[0].clone()]],
        true,
        Some(&records[1..]),
        &fork.f[9].hash,
    );
    assert_eq!(at_kill, [genesis.clone(), genesis]);
    assert_eq!(ends[0], ends[1]);
    let resumed: Vec<_> = logs.iter().map(|l| resumed_lines(l)).collect();
    assert_eq!(resumed[0], resumed[1], "rdashd's lines and dashd's");
    let caches: Vec<_> = logs.iter().map(|l| cache_lines(l)).collect();
    assert_eq!(caches[0], caches[1], "rdashd's cache lines and dashd's");
    assert!(!caches[0].iter().any(|l| l.starts_with("Reading info from")), "{:?}", caches[0]);
    let lines = &resumed[0];
    assert!(lines.iter().any(|l| l.starts_with("Loaded best chain: ") && l.ends_with(" height=0")));
    assert!(lines.iter().any(|l| l == STARTUP_SCHEME), "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("Verifying last")), "{lines:?}");
    let scan = scan_lines(&logs[0]);
    let want = [
        reindexing_line(0),
        "Loaded 0 blocks from external file".to_owned(),
        reindexing_line(1),
        "Loaded 10 blocks from external file".to_owned(),
        FINISHED.to_owned(),
    ];
    assert_eq!(scan, want);
}

/// The deeper variant: `blk00000.dat` = genesis..F20, `blk00001.dat` = a genesis record, whose
/// `ActivateBestChain` connects F1..F20 mid-scan, and a FIFO before `blk00002.dat`.
/// `gettxoutsetinfo` flushes the coins at F20, then the kill. The restart loads F20, verifies the
/// last 6 blocks, reads no net cache, and rescans: 0 blocks in files 0 and 1, F21..F30 in the
/// new file 2.
// Core: validation.cpp:5538-5552, :5051-5180; node/chainstate.cpp:214-216, :423-428 at v24.0.0
#[test]
fn a_resumed_scan_over_a_coins_tip_above_genesis_matches_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-resume20-");
    let (mut nodes, fork) = mined(&home, 30);
    let records = records(&nodes);
    let files = [records[..=20].to_vec(), vec![records[0].clone()]];
    let f20 = shown(&fork.f[19].hash);
    let (logs, ends, at_kill) =
        resumed_after_a_kill(&mut nodes, &files, true, Some(&records[21..]), &fork.f[29].hash);
    assert_eq!(at_kill, [f20.clone(), f20]);
    assert_eq!(ends[0], ends[1]);
    let resumed: Vec<_> = logs.iter().map(|l| resumed_lines(l)).collect();
    assert_eq!(resumed[0], resumed[1], "rdashd's lines and dashd's");
    let caches: Vec<_> = logs.iter().map(|l| cache_lines(l)).collect();
    assert_eq!(caches[0], caches[1], "rdashd's cache lines and dashd's");
    assert!(!caches[0].iter().any(|l| l.starts_with("Reading info from")), "{:?}", caches[0]);
    let lines = &resumed[0];
    let best = format!("Loaded best chain: hashBestChain={} height=20", rpc_hex(&fork.f[19].hash));
    assert!(lines.contains(&best), "{lines:?}");
    assert!(lines.iter().any(|l| l == STARTUP_SCHEME), "{lines:?}");
    assert!(lines.iter().any(|l| l == "Verifying last 6 blocks at level 3"), "{lines:?}");
    let verified = "Verification: No coin database inconsistencies in last 6 blocks (";
    assert!(lines.iter().any(|l| l.starts_with(verified)), "{lines:?}");
    let scan = scan_lines(&logs[0]);
    let want = [
        reindexing_line(0),
        "Loaded 0 blocks from external file".to_owned(),
        reindexing_line(1),
        "Loaded 0 blocks from external file".to_owned(),
        reindexing_line(2),
        "Loaded 10 blocks from external file".to_owned(),
        FINISHED.to_owned(),
    ];
    assert_eq!(scan, want);
}

/// A full reindex over two blocks on F5 that fail: one whose coinbase names the wrong height,
/// which is stored, logged and marked invalid, and one of version 3, whose header is refused and
/// stays unknown. One `-reindex` start; both nodes end at F5 with the same tips.
// Core: validation.cpp:3707-3717, :3740-3745 at v24.0.0
#[test]
fn m2_fixtures_under_a_full_reindex_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-fixtures-");
    let (mut nodes, fork) = mined(&home, 5);
    let records = records(&nodes);
    let f5 = &fork.f[4].hash;
    let time = GENESIS_TIME + 60 * 6;
    let bad_height = mine_txs(f5, time, &[coinbase_paying(7, b"x", &[0])]);
    let bad_version = mine_version(f5, time, &[coinbase_paying(6, b"y", &[0])], None, true, 3);
    let mut file0 = records.clone();
    file0.extend([record(&records, &bad_height), record(&records, &bad_version)]);
    write_files(&nodes, &[file0]);
    let mut ends = Vec::new();
    for node in &mut nodes {
        start_with(node, &[REINDEX]);
        within(&format!("{}'s scan", node.name), || node.new_log().contains(FINISHED));
        tip_is(node, f5);
        let header = node.try_call("getblockheader", &format!("[{}]", shown(&bad_version.hash)));
        let header = header.expect("an answer");
        let (_, error) = header.error();
        assert!(error.contains("\"code\":-5"), "{}: {error}", node.name);
        ends.push(end_state(node));
        node.stop();
        let log = node.new_log();
        assert!(log.contains("bad-cb-height"), "{}: {log}", node.name);
    }
    assert_eq!(ends[0], ends[1]);
    let tips = &ends[0].1;
    assert_eq!(status_of(tips, &bad_height.hash), Some("invalid"), "{tips:?}");
    assert_eq!(status_of(tips, &bad_version.hash), None, "{tips:?}");
}

/// Block-writing RPCs refuse while blocks are imported (DEC-1642), on rdashd: paused before
/// `blk00001.dat`, `submitblock`, `generatetoaddress`, `generatetodescriptor` and `generateblock`
/// answer -10 and write nothing, an argument error still wins, and `generateblock … false` is
/// served. After the scan each writes, and the blocks go to `blk00000.dat`.
#[test]
fn block_writing_rpcs_refuse_while_blocks_are_imported() {
    let home = RetainedHome::new("ch5-3b2-refuse-");
    let mut node = Node::rdashd(home.path(), &[], Storage::Live);
    node.start(0);
    let genesis = genesis(&node);
    let fork = Fork::new(&genesis);
    for block in &fork.f[..10] {
        assert_eq!(node.submit(block), "null");
    }
    node.stop();
    let mut guard = make_fifo(&node, 1);
    start_with(&mut node, &[REINDEX]);
    guard.watch(&node);
    wait_paused(&node, 1);
    assert_eq!(blk_records(&node, 0).len(), 11);
    let f11 = &fork.f[10];
    let calls = [
        ("submitblock", format!(r#"["{}"]"#, f11.hex)),
        ("generatetoaddress", format!(r#"[1, "{ADDRESS}"]"#)),
        ("generatetodescriptor", r#"[1, "raw(51)"]"#.to_owned()),
        ("generateblock", format!(r#"["{ADDRESS}", []]"#)),
        ("generateblock", format!(r#"["{ADDRESS}", [], true]"#)),
    ];
    for (method, params) in &calls {
        let reply = node.try_call(method, params).unwrap();
        let (_, error) = reply.error();
        assert_eq!(error, format!(r#"{{"code":-10,"message":"{REFUSED}"}}"#), "{method}");
    }
    assert_eq!(node.best(), shown(&genesis), "nothing written");
    let decode = node.try_call("submitblock", r#"["zz"]"#).unwrap();
    let (_, error) = decode.error();
    assert_eq!(error, r#"{"code":-22,"message":"Block decode failed"}"#);
    let unsubmitted = node.call("generateblock", &format!(r#"["{ADDRESS}", [], false]"#));
    assert!(unsubmitted.contains("\"hex\""), "{unsubmitted}");
    release_fifo(&guard);
    within("the scan's end", || node.new_log().contains(FINISHED));
    within("startup's ABC", || node.call("getblockcount", "[]") == "10");
    guard.unlink();
    assert_eq!(node.submit(f11), "null");
    for (method, params) in &calls[1..4] {
        let mined = node.call(method, params);
        assert!(mined.contains('"'), "{method}: {mined}");
    }
    assert_eq!(node.call("getblockcount", "[]"), "14");
    node.stop();
    assert!(!exists(&blk_path(&node, 1)), "no block went to file 1");
    assert_eq!(blk_records(&node, 0).len(), 15, "the four blocks in file 0");
}

/// Whether `path` names anything, a FIFO included.
fn exists(path: &std::path::Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// A kill mid-scan with nothing flushed since the wipe: the restart, without `-reindex`, finds
/// an empty index ("Initializing databases..."), loads no chain tip, and rescans from
/// `blk00000.dat`, all 13 blocks.
// Core: validation.cpp:5337-5394; node/blockstorage.cpp:900-928 at v24.0.0
#[test]
fn a_kill_mid_scan_with_no_flush_rescans_from_blk00000_as_on_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-noflush-");
    let (mut nodes, fork) = mined(&home, 12);
    let records = records(&nodes);
    let (logs, ends, _) =
        resumed_after_a_kill(&mut nodes, &[records], false, None, &fork.f[11].hash);
    assert_eq!(ends[0], ends[1]);
    let scans: Vec<_> = logs.iter().map(|l| scan_lines(l)).collect();
    assert_eq!(scans[0], scans[1], "rdashd's lines and dashd's");
    let file0 = reindexing_line(0);
    let want = [file0.as_str(), "Loaded 13 blocks from external file", FINISHED];
    assert_eq!(scans[0], want);
    for log in &logs {
        let lines = texts(log);
        assert!(lines.contains(&"Initializing databases..."), "{log}");
        assert!(!lines.iter().any(|l| l.starts_with("Loaded best chain")), "{log}");
    }
}

/// A kill between two files after `gettxoutsetinfo`'s flush (file 0's index records, the coins
/// at genesis): the restart loads the genesis tip, finds file 0's records stored, and imports
/// F13..F20 from the new `blk00001.dat`.
// Core: rpc/blockchain.cpp:1238; node/blockstorage.cpp:900-928 at v24.0.0
#[test]
fn a_kill_mid_scan_after_a_flush_keeps_its_records_as_on_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-midblk-");
    let (mut nodes, fork) = mined(&home, 20);
    let records = records(&nodes);
    let (logs, ends, _) = resumed_after_a_kill(
        &mut nodes,
        &[records[..=12].to_vec()],
        true,
        Some(&records[13..]),
        &fork.f[19].hash,
    );
    assert_eq!(ends[0], ends[1]);
    let resumed: Vec<_> = logs.iter().map(|l| resumed_lines(l)).collect();
    assert_eq!(resumed[0], resumed[1], "rdashd's lines and dashd's");
    let lines = &resumed[0];
    assert!(lines.iter().any(|l| l.starts_with("Loaded best chain: ") && l.ends_with(" height=0")));
    let scan = scan_lines(&logs[0]);
    let want = [
        reindexing_line(0),
        "Loaded 0 blocks from external file".to_owned(),
        reindexing_line(1),
        "Loaded 8 blocks from external file".to_owned(),
        FINISHED.to_owned(),
    ];
    assert_eq!(scan, want);
    for log in &logs {
        assert!(!texts(log).contains(&"Initializing databases..."), "{log}");
    }
}

/// A kill at the last file, before the clear, after `gettxoutsetinfo`'s flush: nothing is put in
/// the FIFO's place, so the restart finds every record stored ("Loaded 0 blocks"), then clears.
// Core: node/blockstorage.cpp:900-928 at v24.0.0
#[test]
fn a_kill_after_the_last_file_before_the_clear_as_on_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-last-");
    let (mut nodes, fork) = mined(&home, 12);
    let records = records(&nodes);
    let (logs, ends, _) =
        resumed_after_a_kill(&mut nodes, &[records], true, None, &fork.f[11].hash);
    assert_eq!(ends[0], ends[1]);
    let scans: Vec<_> = logs.iter().map(|l| scan_lines(l)).collect();
    assert_eq!(scans[0], scans[1], "rdashd's lines and dashd's");
    let file0 = reindexing_line(0);
    let want = [file0.as_str(), "Loaded 0 blocks from external file", FINISHED];
    assert_eq!(scans[0], want);
}

/// The read-only calls of [`a_scan_with_rpc_traffic_and_a_peer`]'s traffic, each of which must
/// answer without an error.
fn read_only(genesis: &[u8; 32], f1_coinbase: &str) -> Vec<(&'static str, String)> {
    let genesis = shown(genesis);
    vec![
        ("getblockcount", "[]".to_owned()),
        ("getbestblockhash", "[]".to_owned()),
        ("getblockhash", "[0]".to_owned()),
        ("getblockheader", format!("[{genesis}]")),
        ("getblock", format!("[{genesis}, 2]")),
        ("getchaintips", "[]".to_owned()),
        ("getblockchaininfo", "[]".to_owned()),
        ("getdifficulty", "[]".to_owned()),
        ("gettxout", format!(r#"["{f1_coinbase}", 0]"#)),
        ("getmininginfo", "[]".to_owned()),
        ("getnetworkinfo", "[]".to_owned()),
        ("getpeerinfo", "[]".to_owned()),
        ("uptime", "[]".to_owned()),
        ("help", "[]".to_owned()),
    ]
}

/// RPC traffic and a dashd peer during rdashd's scan (debug build, `-checkblockindex=1`), paused
/// before `blk00002.dat`: the read-only calls loop while the four block-writing ones answer -10;
/// then the release, and the read-only calls for 2 s after "Reindexing finished". No abort, one
/// clear, and after the import rdashd syncs the peer's 5 more blocks and ends on its state. The
/// gate on blocks and headers while loading is
/// [`unsolicited_headers_and_blocks_while_loading_match_dashd_v24`]'s.
// Core: node/blockstorage.cpp:900-928 at v24.0.0
#[test]
fn a_scan_with_rpc_traffic_and_a_peer() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let home = RetainedHome::new("ch5-3b2-traffic-");
    let dashd = require_dashd();
    let p2p = free_port();
    let bind = format!("-bind=127.0.0.1:{p2p}");
    let mut peer =
        Node::new("dashd", &dashd, home.path(), &["-listen=1", bind.as_str(), "-listenonion=0"]);
    let mut node = Node::rdashd(home.path(), &["-checkblockindex=1"], Storage::Live);
    peer.start(0);
    node.start(0);
    let genesis = genesis(&peer);
    let fork = Fork::new(&genesis);
    for block in &fork.f[..15] {
        assert_eq!(peer.submit(block), "null");
    }
    for block in &fork.f[..10] {
        assert_eq!(node.submit(block), "null");
    }
    node.stop();
    let records = blk_records(&node, 0);
    write_blk(&node, 0, &records[..=5]);
    write_blk(&node, 1, &records[6..]);
    let mut guard = make_fifo(&node, 2);
    let connect = format!("-connect=127.0.0.1:{p2p}");
    start_with(&mut node, &[REINDEX, connect.as_str(), "-debug=net"]);
    guard.watch(&node);
    wait_paused(&node, 2);
    let f1_coinbase: String = rpc_hex(&txid(&coinbase_paying(1, b"f", &[0])));
    let reads = read_only(&genesis, &f1_coinbase);
    let writes = [
        ("submitblock", format!(r#"["{}"]"#, fork.f[10].hex)),
        ("generatetoaddress", format!(r#"[1, "{ADDRESS}"]"#)),
        ("generatetodescriptor", r#"[1, "raw(51)"]"#.to_owned()),
        ("generateblock", format!(r#"["{ADDRESS}", []]"#)),
    ];
    let refused = format!(r#"{{"code":-10,"message":"{REFUSED}"}}"#);
    let call = |method: &str, params: &str| -> Result<String, String> {
        let reply = node.try_call(method, params)?;
        if reply.body.contains(r#""error":null"#) {
            Ok(reply.result().to_owned())
        } else {
            Err(format!("{method} {params}: {}", reply.body))
        }
    };
    let writing = AtomicBool::new(true);
    let reading = AtomicBool::new(true);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut n = 0u64;
            while reading.load(Ordering::SeqCst) {
                for (method, params) in &reads {
                    call(method, params).unwrap();
                }
                n += 1;
            }
            n
        });
        let writer = scope.spawn(|| {
            let mut n = 0u64;
            while writing.load(Ordering::SeqCst) {
                for (method, params) in &writes {
                    let reply = node.try_call(method, params).unwrap();
                    assert_eq!(reply.error().1, refused, "{method}");
                }
                n += 1;
            }
            n
        });
        std::thread::sleep(Duration::from_secs(2));
        writing.store(false, Ordering::SeqCst);
        assert!(writer.join().unwrap() > 0, "the block-writing calls ran");
        release_fifo(&guard);
        within("the scan's end", || node.new_log().contains(FINISHED));
        std::thread::sleep(Duration::from_secs(2));
        reading.store(false, Ordering::SeqCst);
        assert!(reader.join().unwrap() > 0, "the read-only calls ran");
    });
    guard.unlink();
    tip_is(&node, &fork.f[14].hash);
    let peer_state = end_state(&peer);
    assert_eq!(end_state(&node), peer_state, "rdashd ends on the peer's state");
    node.stop();
    peer.stop();
    let log = node.new_log();
    let lines = texts(&log);
    assert_eq!(lines.iter().filter(|l| **l == FINISHED).count(), 1, "{log}");
    assert!(!lines.iter().any(|l| l.contains("panicked") || l.starts_with("*** ")), "{log}");
}

/// The "Unexpected … message" lines of `log`, without their time or category.
fn unexpected_lines(log: &str) -> Vec<String> {
    texts(log)
        .into_iter()
        .map(body)
        .filter(|l| l.starts_with("Unexpected "))
        .map(str::to_owned)
        .collect()
}

/// Whether a scripted peer's received messages hold the node's first header request.
fn asked_headers(peer: &Peer) -> bool {
    peer.received.iter().any(|c| c == "getheaders" || c == "getheaders2")
}

/// A scripted peer's unsolicited `headers` and `block` of F11 while a `-reindex` imports
/// genesis..F10, paused before `blk00002.dat`: each node drops both with an "Unexpected headers
/// message" and an "Unexpected block message" line. The import's end, as the peer sees it, is the
/// node's first header request: a node starts a peer's headers sync only once no blocks are
/// loading. The same two messages after it log nothing unexpected and connect F11. Both nodes
/// log the same lines.
// Core: net_processing.cpp:5326-5331, :5366-5371 (LoadingBlocks); node/blockstorage.cpp:893-966
// at v24.0.0
#[test]
fn unsolicited_headers_and_blocks_while_loading_match_dashd_v24() {
    let home = RetainedHome::new("ch5-3b2-unsolicited-");
    let (mut nodes, fork) = mined(&home, 11);
    let records = records(&nodes);
    write_files(&nodes, &[records[..=5].to_vec(), records[6..=10].to_vec()]);
    let f11 = parse_hex(&fork.f[10].hex);
    let headers = [&[1][..], &f11[..80], &[0]].concat();
    let mut loading = Vec::new();
    for node in &mut nodes {
        let port = free_port();
        let bind = format!("-bind=127.0.0.1:{port}");
        let mut guard = make_fifo(node, 2);
        start_with(node, &[REINDEX, "-listen=1", bind.as_str(), "-debug=net"]);
        guard.watch(node);
        wait_paused(node, 2);
        let mut peer = Peer::dial(port);
        peer.send("headers", headers.clone());
        peer.send("block", f11.clone());
        peer.sync_with_ping();
        assert!(!asked_headers(&peer), "{}: no headers sync while loading", node.name);
        within(&format!("{}'s unexpected lines", node.name), || {
            unexpected_lines(&node.new_log()).len() >= 2
        });
        release_fifo(&guard);
        peer.answer_until("the node's first header request", 60, asked_headers);
        let ended = node.log_len();
        guard.unlink();
        assert!(node.new_log().contains(FINISHED), "{}: the scan ended first", node.name);
        peer.send("headers", headers.clone());
        peer.send("block", f11.clone());
        peer.sync_with_ping();
        tip_is(node, &fork.f[10].hash);
        node.stop();
        let after = read_since(&node.log(), Some(ended));
        assert_eq!(unexpected_lines(&after), Vec::<String>::new(), "{}: {after}", node.name);
        loading.push(unexpected_lines(&node.new_log()));
    }
    assert_eq!(loading[0], loading[1], "rdashd's lines and dashd's");
    let want = ["headers", "block"].map(|m| format!("Unexpected {m} message received from peer 0"));
    assert_eq!(loading[0], want);
}
