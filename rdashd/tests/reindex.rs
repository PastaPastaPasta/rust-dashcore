//! `-reindex` on the node (CH5-3b-1) against dashd v24.0.0: a datadir with no block files, where
//! `ThreadImport` has nothing to scan, writes genesis through its retry and reaches height 0;
//! "Initializing databases..." at `LoadBlockIndex`'s `needs_init`; and, under test-hooks, an
//! `AbortNode` between the scan and the clear (rdashd only). Each two-node case runs one rdashd
//! and one dashd on the same flags (`common::node`). They cannot be skipped: with
//! `SKIP_DASHD_TESTS` set they fail (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::node::*;
use common::*;

const REINDEX: &str = "-reindex";
const FINISHED: &str = "Reindexing finished";
const INITIALIZING: &str = "Initializing databases...";

/// The lines a reindex with no block files changes that both nodes log alike: `LoadBlockIndex`'s,
/// the import's, the genesis tip's, and the load and VerifyDB lines a fresh reindex skips.
const COMPARED: [&str; 7] = [
    "Checking all blk files are present",
    INITIALIZING,
    FINISHED,
    "UpdateTip:",
    "Loaded best chain",
    "Verifying last",
    "AppInitMain:",
];

/// `log`'s [`COMPARED`] lines without their time, an `UpdateTip` without its cache sizes.
fn compared(log: &str) -> Vec<String> {
    lines_of(log, &COMPARED)
        .into_iter()
        .map(|l| l.split(" cache=").next().unwrap().to_owned())
        .collect()
}

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

/// Removes `node`'s block and undo files, leaving its databases.
fn remove_block_files(node: &Node) {
    let blocks = node.datadir.join("regtest").join("blocks");
    for entry in std::fs::read_dir(&blocks).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if (name.starts_with("blk") || name.starts_with("rev")) && name.ends_with(".dat") {
            std::fs::remove_file(&path).unwrap();
        }
    }
}

/// Each manager summary in `lines` holds nothing.
fn stored_empty(lines: &[String]) {
    for summary in SUMMARIES {
        let found: Vec<_> = lines.iter().filter(|l| l.starts_with(summary)).collect();
        assert!(!found.is_empty(), "{summary:?} in {lines:?}");
        for line in found {
            let counts: Vec<_> =
                line.split(|c: char| !c.is_ascii_digit()).filter(|n| !n.is_empty()).collect();
            assert!(!counts.is_empty() && counts.iter().all(|n| *n == "0"), "stored empty: {line}");
        }
    }
}

/// `-reindex` on a datadir whose block files are gone: both nodes wipe the block tree and the
/// chainstate, skip `LoadBlockIndexDB`, find no `blk00000.dat` to scan, log "Reindexing
/// finished", write genesis through the retry and connect it, height 0. The net caches written
/// by the earlier run are verified and stored empty (BL-549), and a plain restart loads height 0.
// Core: node/blockstorage.cpp:900-928, validation.cpp:5337-5394, init.cpp:2239-2257 at v24.0.0
#[test]
fn reindex_without_block_files_matches_dashd_v24() {
    let home = RetainedHome::new("ch5-3b1-reindex-");
    let mut nodes = pair(&home, &[]);
    let fork = Fork::new(&genesis(&nodes[1]));
    for block in &fork.f {
        let want = nodes[1].submit(block);
        assert_eq!(nodes[0].submit(block), want);
    }
    assert_eq!(nodes[0].best(), nodes[1].best());
    let mut logs = Vec::new();
    let mut caches = Vec::new();
    for node in &mut nodes {
        node.stop();
        remove_block_files(node);
        node.flags.push(REINDEX.to_owned());
        node.start(0);
        node.flags.pop();
        wait_until(&format!("{} at genesis", node.name), || node.best() == shown(&genesis(node)));
        node.stop();
        let log = node.new_log();
        caches.push(cache_lines(&log));
        logs.push(compared(&log));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(caches[0], caches[1], "rdashd's cache lines and dashd's");
    let lines = &logs[0];
    assert_eq!(lines.iter().filter(|l| *l == FINISHED).count(), 1, "{lines:?}");
    assert!(lines.iter().any(|l| l == INITIALIZING), "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("Checking all blk files")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("Loaded best chain")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("UpdateTip:") && l.contains(" height=0 ")));
    assert!(!caches[0].iter().any(|l| l.starts_with("Reading info from")), "{:?}", caches[0]);
    stored_empty(&caches[0]);
    for node in &mut nodes {
        node.start(0);
        assert_eq!(node.best(), shown(&genesis(node)), "{}: the plain restart", node.name);
        assert!(!node.new_log().contains(FINISHED), "{}", node.name);
        node.stop();
    }
}

/// "Initializing databases..." is `LoadBlockIndex`'s line for an index still empty after
/// `LoadBlockIndexDB`: a fresh datadir logs it once, after the blk-file check, and a restart on
/// the index it wrote does not.
// Core: validation.cpp:5337-5394 at v24.0.0
#[test]
fn initializing_databases_on_a_fresh_datadir_matches_dashd_v24() {
    let home = RetainedHome::new("ch5-3b1-init-");
    let mut nodes = pair(&home, &[]);
    let mut logs = Vec::new();
    for node in &mut nodes {
        logs.push(compared(&node.new_log()));
        node.stop();
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let checking = logs[0].iter().position(|l| l.starts_with("Checking all blk files"));
    let initializing = logs[0].iter().position(|l| l == INITIALIZING);
    assert!(checking.is_some() && checking < initializing, "{:?}", logs[0]);
    assert_eq!(logs[0].iter().filter(|l| *l == INITIALIZING).count(), 1);
    for node in &mut nodes {
        node.start(0);
        assert!(!node.new_log().contains(INITIALIZING), "{}: the restart", node.name);
        node.stop();
    }
}

/// The reindex's clear failing in its own write (`RDASHD_TEST_HOOK=abort-after-scan` fails the
/// clear's durable write), AD-CH5-3 (DEC-1648): Core's `WriteReindexing(false)` throws there and
/// its process ends. rdashd reports the flush's `AbortNode` ("*** System error while flushing:"
/// and the engine's text) and its import ends: no "Reindexing finished", no genesis retry, no
/// startup's ABC and no "Stopping after block import". The shutdown writes nothing: the
/// chainstate and the block index stay as the wipe left them, with the stored flag set, and with
/// no block or undo byte written the clear's files barrier was skipped, so no block file exists
/// (OD-CH5-78). The next start, without `-reindex`, reads the flag from disk (`LoadBlockIndexDB`
/// runs: "Checking all blk files are present"), resumes the reindex, finishes it and stores the
/// net caches empty unread; genesis creates `blk00000.dat` and `rev00000.dat` through its own
/// write, as on a fresh datadir. A further start serves genesis from that file, and the block and
/// undo files are dashd's after its `-reindex` over no block files, which scans none either.
// Core: node/blockstorage.cpp:908-928, txdb.cpp:188-193, dbwrapper.cpp:200-208, :261-268,
// init.cpp:2239-2257 at v24.0.0
#[cfg(feature = "test-hooks")]
#[test]
fn a_failed_reindex_clear_ends_the_import_and_the_next_start_resumes() {
    let home = RetainedHome::new("ch5-3b1-abort-");
    let mut node = Node::rdashd(home.path(), &[], Storage::Live);
    node.start(0);
    node.stop();
    remove_block_files(&node);
    let offset = node.log_len();
    let flags: Vec<&str> = node.flags.iter().map(String::as_str).collect();
    assert!(!flags.contains(&REINDEX), "{flags:?}");
    let args = [&["-printtoconsole=0", REINDEX, "-stopafterblockimport"][..], &flags].concat();
    let mut cmd = regtest_on(&node.binary, home.path(), &node.datadir, &args);
    cmd.env("RDASHD_TEST_HOOK", "abort-after-scan");
    let out = output(cmd);
    let log = read_since(&node.log(), Some(offset));
    let funnel = "*** System error while flushing: ";
    let report = log.lines().find(|l| l.contains(funnel)).unwrap_or_else(|| panic!("{log}"));
    assert!(report.contains("injected durable write failure"), "{report}");
    for absent in [FINISHED, "Stopping after block import", "UpdateTip:", "block import done"] {
        assert!(!log.contains(absent), "{absent:?} in {log}");
    }
    assert_ne!(out.status.code(), Some(0), "{}", stderr(&out));
    let left = block_files_of(&node);
    assert!(left.is_empty(), "no block file: the clear's barrier was skipped: {left:?}");
    let rows = chainstate_rows(&node);
    let kept: Vec<_> = rows.iter().filter(|(cf, ..)| *cf != "meta").collect();
    assert!(kept.is_empty(), "the wipe's empty chainstate and index: {kept:?}");
    let meta = |key: &[u8]| rows.iter().find(|(cf, k, _)| *cf == "meta" && k == key);
    assert!(meta(b"reindexing").is_some_and(|(.., v)| v == &[1]), "the flag stays set");
    let erased = ["tip", "evo_tip", "head_blocks", "last_file", "pruned_files"];
    for erased in erased.into_iter().chain(["wiping", "wiping_tree"]) {
        assert!(meta(erased.as_bytes()).is_none(), "{erased}");
    }

    node.start(0);
    wait_until("the resumed reindex at genesis", || node.best() == shown(&genesis(&node)));
    node.stop();
    let log = node.new_log();
    assert_eq!(log.matches(FINISHED).count(), 1, "{log}");
    let checking = log.find("Checking all blk files").unwrap_or_else(|| panic!("{log}"));
    assert!(checking < log.find(INITIALIZING).unwrap_or_else(|| panic!("{log}")), "{log}");
    let caches = cache_lines(&log);
    assert!(!caches.iter().any(|l| l.starts_with("Reading info from")), "{caches:?}");
    stored_empty(&caches);
    assert!(!log.contains(SCANNED) && !log.contains(LOADED_NONE), "{log}");

    node.start(0);
    assert!(!node.new_log().contains(FINISHED), "nothing left to resume");
    let hash = genesis(&node);
    let served = node.call("getblock", &format!("[{}, 0]", shown(&hash)));
    node.stop();
    let files = block_files_of(&node);
    let blk = &files.iter().find(|(name, _)| name == "blk00000.dat").expect("genesis's file").1;
    let len = u32::from_le_bytes(blk[4..8].try_into().unwrap()) as usize;
    let stored: String = blk[8..8 + len].iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(served.trim_matches('"'), stored, "genesis read back from blk00000.dat");

    let dashd = require_dashd();
    let mut core = Node::new("dashd", &dashd, home.path(), &[]);
    core.start(0);
    core.stop();
    remove_block_files(&core);
    core.flags.push(REINDEX.to_owned());
    core.start(0);
    wait_until("dashd's reindex at genesis", || core.best() == shown(&genesis(&core)));
    core.stop();
    let core_log = core.new_log();
    assert!(!core_log.contains(SCANNED) && !core_log.contains(LOADED_NONE), "{core_log}");
    assert_eq!(block_files_of(&node), block_files_of(&core), "rdashd's block files and dashd's");
}

/// Core's lines for the scan of an empty `blk00000.dat` (CH5-3b-2's), which neither node logs
/// over no block files.
// Core: node/blockstorage.cpp:915, validation.cpp:5624 at v24.0.0
#[cfg(feature = "test-hooks")]
const SCANNED: &str = "Reindexing block file blk00000.dat...";
#[cfg(feature = "test-hooks")]
const LOADED_NONE: &str = "Loaded 0 blocks from external file in ";

/// Every record of `node`'s stopped chainstate, by column family.
#[cfg(feature = "test-hooks")]
fn chainstate_rows(node: &Node) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    use dashd_chainstate::store::CHAINSTATE_SCHEMA;
    use dashd_store::{KvRead as _, RocksConfig, RocksEngine};
    let path = node.datadir.join("regtest").join("chainstate");
    let (db, _) = RocksEngine::open(&path, &CHAINSTATE_SCHEMA, &RocksConfig::default()).unwrap();
    let mut rows = Vec::new();
    for spec in CHAINSTATE_SCHEMA.cfs {
        let cf = CHAINSTATE_SCHEMA.cf(spec.name).unwrap();
        for row in db.iter_prefix(cf, &[]) {
            let (k, v) = row.unwrap();
            rows.push((spec.name, k, v));
        }
    }
    rows
}

/// `node`'s block and undo files by name, sorted, with their bytes.
#[cfg(feature = "test-hooks")]
fn block_files_of(node: &Node) -> Vec<(String, Vec<u8>)> {
    let blocks = node.datadir.join("regtest").join("blocks");
    let mut files: Vec<_> = std::fs::read_dir(&blocks)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let block =
                (name.starts_with("blk") || name.starts_with("rev")) && name.ends_with(".dat");
            block.then(|| (name, std::fs::read(&path).unwrap()))
        })
        .collect();
    files.sort();
    files
}

/// A shutdown requested between the scan and the clear (`RDASHD_TEST_HOOK=shutdown-after-scan`):
/// init sees it and goes to `Shutdown()`, which joins the import before it stops V, as Core's
/// joins `m_load_block`. So the import goes on: the clear is written ("Reindexing finished"),
/// the genesis retry writes genesis, startup's ABC is answered and `-stopafterblockimport`
/// stops. The next start finds no reindex to resume.
// Core: init.cpp:355-356, :2570-2573; node/blockstorage.cpp:923-962 at v24.0.0
#[cfg(feature = "test-hooks")]
#[test]
fn a_shutdown_after_the_scan_still_finishes_the_reindex() {
    const STOP: &str = "Stopping after block import";
    let home = RetainedHome::new("ch5-3b1-shutdown-");
    let mut node = Node::rdashd(home.path(), &[], Storage::Live);
    node.start(0);
    node.stop();
    remove_block_files(&node);
    let offset = node.log_len();
    let flags: Vec<&str> = node.flags.iter().map(String::as_str).collect();
    let args = [&["-printtoconsole=0", REINDEX, "-stopafterblockimport"][..], &flags].concat();
    let mut cmd = regtest_on(&node.binary, home.path(), &node.datadir, &args);
    cmd.env("RDASHD_TEST_HOOK", "shutdown-after-scan");
    let out = output(cmd);
    let log = read_since(&node.log(), Some(offset));
    let exiting = log.find("Shutdown requested. Exiting.").unwrap_or_else(|| panic!("{log}"));
    let finished = log.find(FINISHED).unwrap_or_else(|| panic!("{log}"));
    let stop = log.find(STOP).unwrap_or_else(|| panic!("{log}"));
    assert!(exiting < finished && finished < stop, "{log}");
    let tip = log.find("UpdateTip:").unwrap_or_else(|| panic!("{log}"));
    assert!(log[tip..].contains(" height=0 ") && tip < stop, "{log}");
    let blk = block_files_of(&node);
    assert!(blk.iter().any(|(name, b)| name == "blk00000.dat" && !b.is_empty()), "genesis");
    assert_ne!(out.status.code(), None, "{}", stderr(&out));

    node.start(0);
    assert_eq!(node.best(), shown(&genesis(&node)), "the next start");
    node.stop();
    let log = node.new_log();
    assert!(!log.contains(FINISHED) && !log.contains(INITIALIZING), "{log}");
}
