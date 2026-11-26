//! `-loadblock` (CH5-3c) against dashd v24: `ThreadImport`'s loop over the paths after the
//! reindex scan, each file imported with no positions and no map, then startup's ABC. Each case
//! writes the same files from a source pair's records, outside the datadirs, and compares the
//! import's lines and the end state. Pauses are a FIFO's (`common::fifo`). They cannot be
//! skipped: with `SKIP_DASHD_TESTS` set they fail (`common::require_dashd`). Linux-only, as
//! `common::fifo` is.
#![cfg(target_os = "linux")]

mod common;

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};

use common::fifo::*;
use common::import::*;
use common::node::*;
use common::*;

const DEBUG: &str = "-debug=reindex";
const FINISHED: &str = "Reindexing finished";
const EXIT: &str = "Shutdown requested. Exit ThreadImport";
const STOPPING: &str = "Stopping after block import";
const OUT_OF_ORDER: &str = "LoadExternalBlockFile: Out of order block";
const CHILD: &str = "LoadExternalBlockFile: Processing out of order child";
const REFUSED: &str = "rdashd is in initial sync and waiting for blocks...";
const NO_ACTIVE_CHAIN: &str =
    "CheckBlockIndex: no active chain, yet more than the genesis entry at entry 1";
/// rpc_generate.py's regtest address of the generator point's key.
const ADDRESS: &str = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";

/// `path`'s "Importing blocks file" line.
fn importing(path: &Path) -> String {
    format!("Importing blocks file {}...", path.display())
}

/// `path`'s line when it does not open.
fn could_not_open(path: &Path) -> String {
    format!("Warning: Could not open blocks file {}", path.display())
}

/// `-loadblock=<path>`.
fn loadblock(path: &Path) -> String {
    format!("-loadblock={}", path.display())
}

/// The import's lines in `log`, in log order, without their time or the "Loaded" line's
/// duration: each file's line, the warning, its records' `-debug=reindex` lines, the "Loaded"
/// line, the check after the file, the reindex's lines and `-stopafterblockimport`'s.
fn import_lines(log: &str) -> Vec<String> {
    texts(log)
        .into_iter()
        .filter_map(|line| {
            let l = body(line);
            let kept = l.starts_with("Importing blocks file ")
                || l.starts_with("Warning: Could not open blocks file ")
                || l.starts_with("Reindexing block file ")
                || is_loaded(l)
                || l == FINISHED
                || l == EXIT
                || l == STOPPING
                || l.starts_with("LoadExternalBlockFile: ")
                || l.starts_with("Block Import: ");
            kept.then(|| {
                if is_loaded(line) {
                    loaded_count(line)
                } else {
                    line
                }
                .to_owned()
            })
        })
        .collect()
}

/// A source pair's `blk00000.dat` records, genesis..F`n`, and the blocks.
fn source(prefix: &str, n: usize) -> (Vec<Vec<u8>>, Fork) {
    let src = RetainedHome::new(prefix);
    let (nodes, fork) = mined(&src, n);
    (records(&nodes), fork)
}

/// The two nodes at a genesis tip: started once and stopped.
fn at_genesis(home: &RetainedHome) -> [Node; 2] {
    let mut nodes = pair(home, &[]);
    for node in &mut nodes {
        node.stop();
    }
    nodes
}

/// Writes `records` as the file at `path`.
fn write_records(path: &Path, records: &[Vec<u8>]) {
    std::fs::write(path, records.concat()).unwrap();
}

/// [`start_with`] with `paths` as `-loadblock` arguments, in order, after `flags`.
fn start_loading(node: &mut Node, flags: &[&str], paths: &[&Path]) {
    let args: Vec<String> = paths.iter().map(|p| loadblock(p)).collect();
    let all: Vec<&str> = flags.iter().copied().chain(args.iter().map(String::as_str)).collect();
    start_with(node, &all);
}

/// Two files on datadirs at a genesis tip: A = [genesis, F1..F5] and B = [F6..F10], imported in
/// argument order (B's records need A's). Both nodes end at F10 with the same lines and the same
/// `blk00000.dat`.
// Core: init.cpp:2443-2446; node/blockstorage.cpp:931-935; validation.cpp:5624 at v24.0.0
#[test]
fn loadblock_files_import_in_argument_order_as_on_dashd_v24() {
    let (records, fork) = source("ch5-3c-order-src-", 10);
    let home = RetainedHome::new("ch5-3c-order-");
    let (a, b) = (home.path().join("a.dat"), home.path().join("b.dat"));
    write_records(&a, &records[..=5]);
    write_records(&b, &records[6..=10]);
    let mut nodes = at_genesis(&home);
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_loading(node, &[], &[&a, &b]);
        tip_is(node, &fork.f[9].hash);
        node.stop();
        logs.push(import_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let want = [
        importing(&a),
        "Loaded 5 blocks from external file".to_owned(),
        importing(&b),
        "Loaded 5 blocks from external file".to_owned(),
    ];
    assert_eq!(logs[0], want);
    assert_eq!(blk_records(&nodes[0], 0), blk_records(&nodes[1], 0), "both nodes' blk00000.dat");
}

/// A path that does not open logs Core's warning and the loop goes on with no check: a missing
/// file, an unreadable one (mode 0, skipped as root) and a directory, which opens and loads no
/// block, between two good files. The tip is the last file's.
// Core: node/blockstorage.cpp:932-942; streams.h:609-619; validation.cpp:5475-5488 at v24.0.0
#[test]
fn a_loadblock_path_that_does_not_open_is_skipped_as_on_dashd_v24() {
    use std::os::unix::fs::PermissionsExt as _;
    let (records, fork) = source("ch5-3c-skip-src-", 6);
    let home = RetainedHome::new("ch5-3c-skip-");
    let path = |name: &str| home.path().join(name);
    let (a, m, u, d, b) =
        (path("a.dat"), path("missing.dat"), path("unreadable.dat"), path("dir"), path("b.dat"));
    write_records(&a, &records[..=3]);
    write_records(&b, &records[4..=6]);
    std::fs::create_dir(&d).unwrap();
    // SAFETY: geteuid has no preconditions.
    let root = unsafe { libc::geteuid() } == 0;
    let _closed = ModeGuard(vec![u.clone()]);
    let mut paths: Vec<&Path> = vec![&a, &m];
    if root {
        println!("euid 0: the unreadable path is skipped (root opens a mode-0 file)");
    } else {
        write_records(&u, &records[4..=6]);
        std::fs::set_permissions(&u, std::fs::Permissions::from_mode(0o000)).unwrap();
        paths.push(&u);
    }
    paths.extend([d.as_path(), b.as_path()]);
    let mut nodes = at_genesis(&home);
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_loading(node, &[], &paths);
        tip_is(node, &fork.f[5].hash);
        node.stop();
        logs.push(import_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let mut want =
        vec![importing(&a), "Loaded 3 blocks from external file".to_owned(), could_not_open(&m)];
    if !root {
        want.push(could_not_open(&u));
    }
    want.extend([
        importing(&d),
        "Loaded 0 blocks from external file".to_owned(),
        importing(&b),
        "Loaded 3 blocks from external file".to_owned(),
    ]);
    assert_eq!(logs[0], want);
}

/// `node` started in `dir`, its working directory, with `flags` for this launch only, waited for
/// until init is done.
fn start_in(node: &mut Node, dir: &Path, flags: &[&str]) {
    assert!(node.run.is_none(), "{} runs", node.name);
    node.offset = node.log_len();
    let (binary, datadir) = (node.binary.clone(), node.datadir.clone());
    let home = datadir.parent().unwrap().to_owned();
    let kept: Vec<&str> =
        node.flags.iter().map(String::as_str).chain(flags.iter().copied()).collect();
    let started = try_start_on_free_port(&datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        let args = [&["-printtoconsole=0", &rpcport, "-dbcrashratio=0"][..], &kept].concat();
        let mut cmd = regtest_on(&binary, &home, &datadir, &args);
        cmd.current_dir(dir);
        cmd
    });
    let (running, port) = started.unwrap_or_else(|(_, out)| {
        panic!("{} exited during init: {}\n{}", node.name, stderr(&out), node.new_log())
    });
    node.run = Some((running, port));
}

/// A relative `-loadblock` path is used as given, never made absolute: it resolves against the
/// process's working directory, and the line prints it as given.
// Core: init.cpp:2445 (unlike `-pid`'s AbsPathForConfigVal at :212) at v24.0.0
#[test]
fn relative_loadblock_paths_resolve_from_the_working_directory_as_on_dashd_v24() {
    let (records, fork) = source("ch5-3c-relative-src-", 5);
    let home = RetainedHome::new("ch5-3c-relative-");
    let dir = home.path().join("cwd");
    std::fs::create_dir(&dir).unwrap();
    write_records(&dir.join("boot.dat"), &records);
    let mut nodes = at_genesis(&home);
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_in(node, &dir, &["-loadblock=boot.dat"]);
        tip_is(node, &fork.f[4].hash);
        node.stop();
        logs.push(import_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let want = [importing(Path::new("boot.dat")), "Loaded 5 blocks from external file".to_owned()];
    assert_eq!(logs[0], want);
}

/// `-reindex -loadblock=X`: the reindex's scan of `blk00000.dat` (genesis..F5), its clear, then
/// the `-loadblock` file X (F6..F8), then startup's ABC to F8.
// Core: node/blockstorage.cpp:900-943 at v24.0.0
#[test]
fn reindex_then_loadblock_in_cores_order_as_on_dashd_v24() {
    let home = RetainedHome::new("ch5-3c-reindex-");
    let (mut nodes, fork) = mined(&home, 8);
    let records = records(&nodes);
    write_files(&nodes, &[records[..=5].to_vec()]);
    let x = home.path().join("x.dat");
    write_records(&x, &records[6..=8]);
    let mut logs = Vec::new();
    for node in &mut nodes {
        start_loading(node, &["-reindex"], &[&x]);
        tip_is(node, &fork.f[7].hash);
        node.stop();
        logs.push(import_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let want = [
        reindexing_line(0),
        "Loaded 6 blocks from external file".to_owned(),
        FINISHED.to_owned(),
        importing(&x),
        "Loaded 3 blocks from external file".to_owned(),
    ];
    assert_eq!(logs[0], want);
}

/// Whether `log` (a launch's) has `path`'s "Importing" line followed by a "Loaded" line.
fn file_loaded(log: &str, path: &Path) -> bool {
    let lines = texts(log);
    let line = importing(path);
    lines.iter().position(|l| *l == line).is_some_and(|at| lines[at..].iter().any(|l| is_loaded(l)))
}

/// A shutdown while the loop is paused between files: A (F1..F3) is loaded, B is a FIFO the
/// open waits on, C is never read. The `stop` RPC returns first, then B is released (D75): B is
/// imported and returns at its first record's check with no "Loaded" line, the check after the
/// file logs its line, and no ABC runs, so a restart without `-loadblock` loads the genesis tip.
// Core: node/blockstorage.cpp:936-939; validation.cpp:5466 at v24.0.0
#[test]
fn a_shutdown_between_loadblock_files_matches_dashd_v24() {
    let (records, _) = source("ch5-3c-shutdown-src-", 6);
    let home = RetainedHome::new("ch5-3c-shutdown-");
    let (a, b, c) =
        (home.path().join("a.dat"), home.path().join("b.fifo"), home.path().join("c.dat"));
    write_records(&a, &records[1..=3]);
    write_records(&c, &records[4..=6]);
    let mut nodes = at_genesis(&home);
    let mut logs = Vec::new();
    let mut bests = Vec::new();
    for node in &mut nodes {
        let mut guard = fifo_at(b.clone());
        start_loading(node, &[], &[&a, &b, &c]);
        guard.watch(node);
        within(&format!("{} paused at B", node.name), || file_loaded(&node.new_log(), &a));
        let log = node.new_log();
        assert!(!texts(&log).contains(&importing(&b).as_str()), "{}: {log}", node.name);
        node.call("stop", "[]");
        release_fifo(&guard);
        let (running, _) = node.run.take().unwrap();
        let out = running.wait();
        assert_eq!(out.status.code(), Some(0), "{}: {}", node.name, stderr(&out));
        guard.unlink();
        let log = node.new_log();
        let lines = texts(&log);
        let paused = lines.iter().position(|l| is_loaded(l)).unwrap();
        assert!(
            !lines[paused..].iter().any(|l| l.starts_with("UpdateTip: ")),
            "{}: {log}",
            node.name
        );
        logs.push(import_lines(&log));
        node.start(0);
        let best = texts(&node.new_log())
            .into_iter()
            .find(|l| l.starts_with("Loaded best chain: "))
            .map(|l| l.split(" date=").next().unwrap().to_owned());
        bests.push(best);
        node.stop();
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let want = [
        importing(&a),
        "Loaded 3 blocks from external file".to_owned(),
        importing(&b),
        EXIT.to_owned(),
    ];
    assert_eq!(logs[0], want);
    assert!(!logs[0].contains(&importing(&c)) && !logs[0].iter().any(|l| l == STOPPING));
    assert_eq!(bests[0], bests[1], "the restart's loaded tip");
    let best = bests[0].as_deref().unwrap_or_default();
    assert!(best.ends_with(" height=0"), "{best}");
}

/// `-loadblock=A -stopafterblockimport`: the import, then startup's ABC, then the stop; both
/// exit 0, and a restart has A's tip.
// Core: node/blockstorage.cpp:945-962 at v24.0.0
#[test]
fn loadblock_with_stopafterblockimport_matches_dashd_v24() {
    let (records, fork) = source("ch5-3c-stopafter-src-", 3);
    let home = RetainedHome::new("ch5-3c-stopafter-");
    let a = home.path().join("a.dat");
    write_records(&a, &records[1..=3]);
    let mut nodes = at_genesis(&home);
    let mut logs = Vec::new();
    for node in &mut nodes {
        let flag = loadblock(&a);
        let (out, log) = run_to_exit(node, &[&flag, "-stopafterblockimport"]);
        assert_eq!(out.status.code(), Some(0), "{}: {}", node.name, stderr(&out));
        logs.push(import_lines(&log));
        node.start(0);
        tip_is(node, &fork.f[2].hash);
        node.stop();
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    let want =
        [importing(&a), "Loaded 3 blocks from external file".to_owned(), STOPPING.to_owned()];
    assert_eq!(logs[0], want);
}

/// A file starting at genesis on fresh datadirs under `-checkblockindex=1`: the stored genesis
/// record's ABC activates genesis, so F1..F10 are accepted with an active chain and every
/// `CheckBlockIndex` walks the index. Both end at F10 with the same tips, lines and blk file.
// Core: validation.cpp:4789, :4869, :5538-5552 at v24.0.0
#[test]
fn a_loadblock_file_imports_under_checkblockindex_as_on_dashd_v24() {
    let (records, fork) = source("ch5-3c-checks-src-", 10);
    let home = RetainedHome::new("ch5-3c-checks-");
    let boot = home.path().join("boot.dat");
    write_records(&boot, &records);
    let mut nodes = unstarted(&home, &["-checkblockindex=1", DEBUG]);
    let mut logs = Vec::new();
    let mut tips = Vec::new();
    for node in &mut nodes {
        start_loading(node, &[], &[&boot]);
        tip_is(node, &fork.f[9].hash);
        tips.push(node.tips());
        node.stop();
        logs.push(import_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(logs[0], [importing(&boot), "Loaded 10 blocks from external file".to_owned()]);
    assert_eq!(tips[0], tips[1], "getchaintips");
    assert_eq!(blk_records(&nodes[0], 0), blk_records(&nodes[1], 0), "both nodes' blk00000.dat");
}

/// BL-29's file: F2, F1, F3, F4 from a source pair.
fn bl29(prefix: &str) -> (Vec<Vec<u8>>, Fork) {
    let (records, fork) = source(prefix, 4);
    let file = vec![records[2].clone(), records[1].clone(), records[3].clone(), records[4].clone()];
    (file, fork)
}

/// BL-29 with checks on: on a fresh datadir the active chain is empty until the import's first
/// ABC, which follows the last file. F2 is dropped as out of order, and F1, whose parent is
/// genesis, is accepted before any ABC: `CheckBlockIndex`'s no-active-chain rule stops both
/// nodes. Their lines match through the out-of-order line; dashd dies on its assert, and rdashd
/// reports V's assert, logs the per-file check's line (V set the shutdown flag before its
/// replies disconnected, D37) and exits 1.
// Core: validation.cpp:4789, :5638-5641 at v24.0.0
#[test]
fn bl29_a_first_record_not_genesis_stops_both_nodes_as_on_dashd_v24() {
    let (file, fork) = bl29("ch5-3c-bl29-src-");
    let home = RetainedHome::new("ch5-3c-bl29-");
    let path = home.path().join("bl29.dat");
    write_records(&path, &file);
    let flag = loadblock(&path);
    let mut nodes = unstarted(&home, &[DEBUG]);
    let mut runs = Vec::new();
    for node in &mut nodes {
        runs.push(run_to_exit(node, &[&flag]));
    }
    let lines: Vec<Vec<String>> = runs.iter().map(|(_, log)| import_lines(log)).collect();
    let f2 = rpc_hex(&fork.f[1].hash);
    let through = |lines: &[String]| {
        let at = lines.iter().position(|l| body(l).starts_with(OUT_OF_ORDER) && l.contains(&f2));
        lines[..=at.unwrap_or_else(|| panic!("the out-of-order line: {lines:?}"))].to_vec()
    };
    assert_eq!(through(&lines[0]), through(&lines[1]), "rdashd's lines and dashd's");
    assert_eq!(through(&lines[0])[0], importing(&path));
    for (node, (_, log)) in nodes.iter().zip(&runs) {
        assert!(!texts(log).contains(&STOPPING), "{}: {log}", node.name);
    }

    let (out, log) = &runs[1];
    assert_eq!((out.status.signal(), out.status.code()), (Some(libc::SIGABRT), None), "dashd");
    let assertion = "Assertion `m_blockman.m_block_index.size() <= 1' failed.";
    assert!(stderr(out).contains(assertion), "dashd: {}", stderr(out));
    let after = through(&lines[1]).len();
    assert!(!lines[1][after..].iter().any(|l| is_loaded(l)), "dashd: {log}");

    let (out, log) = &runs[0];
    assert_eq!(out.status.code(), Some(1), "rdashd: {}", stderr(out));
    let tail: Vec<&str> = texts(log)
        .into_iter()
        .skip_while(|l| !(body(l).starts_with(OUT_OF_ORDER) && l.contains(&f2)))
        .collect();
    let reports = tail.iter().filter(|l| l.contains(NO_ACTIVE_CHAIN)).count();
    let exits: Vec<usize> =
        tail.iter().enumerate().filter(|(_, l)| **l == EXIT).map(|(i, _)| i).collect();
    let report = tail.iter().position(|l| l.contains(NO_ACTIVE_CHAIN));
    assert_eq!((reports, exits.len()), (1, 1), "rdashd: {log}");
    assert!(report.is_some_and(|r| r < exits[0]), "V's report, then the check's line: {log}");
    assert!(!tail.iter().any(|l| is_loaded(l)), "rdashd: {log}");
    assert_eq!(stderr(out), "Error: A fatal internal error occurred, see debug.log for details\n");
}

/// BL-29's file with `-checkblockindex=0`: both import it, F2 is dropped as out of order (no
/// map), F1 is accepted, F3 and F4 are dropped as F2 was never indexed, and startup's ABC
/// connects F1.
// Core: validation.cpp:5508-5515 at v24.0.0
#[test]
fn bl29_without_checkblockindex_both_import_and_drop_out_of_order_records_as_on_dashd_v24() {
    let (file, fork) = bl29("ch5-3c-bl29off-src-");
    let home = RetainedHome::new("ch5-3c-bl29off-");
    let path = home.path().join("bl29.dat");
    write_records(&path, &file);
    let mut nodes = unstarted(&home, &["-checkblockindex=0", DEBUG]);
    let mut logs = Vec::new();
    let mut tips = Vec::new();
    for node in &mut nodes {
        start_loading(node, &[], &[&path]);
        tip_is(node, &fork.f[0].hash);
        tips.push(node.tips());
        node.stop();
        logs.push(import_lines(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1], "rdashd's lines and dashd's");
    assert_eq!(tips[0], tips[1], "getchaintips");
    let lines = &logs[0];
    assert_eq!(lines.first(), Some(&importing(&path)));
    assert_eq!(lines.iter().filter(|l| body(l).starts_with(OUT_OF_ORDER)).count(), 3, "{lines:?}");
    assert_eq!(lines.last().map(String::as_str), Some("Loaded 1 blocks from external file"));
    assert!(!lines.iter().any(|l| body(l).starts_with(CHILD)), "{lines:?}");
}

/// Under `-reindex -loadblock`: during the scan, block-writing RPCs refuse while blocks are
/// imported (DEC-1642); the `-loadblock` loop that follows: D71. The test writes no block while a
/// FIFO is linked (D75). Paused before `blk00001.dat`, the scan half calls the four RPCs; once
/// the scan is over and the loop is parked in a `-loadblock` FIFO's first read (a held writer,
/// the FIFO then unlinked), `submitblock` and the three generate RPCs write, and the file ends
/// once the writer closes.
// Core: rpc/mining.cpp:746-752, :1007; node/blockstorage.cpp:923-943 at v24.0.0
#[test]
fn block_writing_rpcs_are_served_in_the_loadblock_loop_after_the_reindex_scan() {
    let home = RetainedHome::new("ch5-3c-served-");
    let mut node = Node::rdashd(home.path(), &[], Storage::Live);
    node.start(0);
    let genesis = genesis(&node);
    let fork = Fork::new(&genesis);
    for block in &fork.f[..10] {
        assert_eq!(node.submit(block), "null");
    }
    node.stop();
    let a = home.path().join("loadblock-A");
    std::fs::write(&a, b"").unwrap();
    let l: PathBuf = home.path().join("loadblock-L");
    let mut blk = make_fifo(&node, 1);
    let mut held = fifo_at(l.clone());
    start_loading(&mut node, &["-reindex"], &[&a, &l]);
    blk.watch(&node);
    held.watch(&node);

    wait_paused(&node, 1);
    let calls = [
        ("submitblock", format!(r#"["{}"]"#, fork.f[10].hex)),
        ("generatetoaddress", format!(r#"[1, "{ADDRESS}"]"#)),
        ("generatetodescriptor", r#"[1, "raw(51)"]"#.to_owned()),
        ("generateblock", format!(r#"["{ADDRESS}", []]"#)),
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

    release_fifo(&blk);
    blk.unlink();
    within("A's end in the loop", || {
        let log = node.new_log();
        texts(&log).contains(&FINISHED) && file_loaded(&log, &a)
    });
    held.hold_writer(BOUND).unwrap();
    within("L's open", || texts(&node.new_log()).contains(&importing(&l).as_str()));
    held.unlink();

    assert_eq!(node.best(), shown(&genesis), "only the scan's genesis ABC has run");
    assert_eq!(node.submit(&fork.f[10]), "null");
    assert_eq!(node.submit(&fork.f[11]), "null");
    for (method, params) in &calls[1..] {
        let mined = node.call(method, params);
        assert!(mined.contains('"'), "{method}: {mined}");
    }
    assert_eq!(node.call("getblockcount", "[]"), "15");

    held.release_held();
    within("L's end", || file_loaded(&node.new_log(), &l));
    assert_eq!(node.call("getblockcount", "[]"), "15");
    node.stop();
    assert!(!exists(&blk_path(&node, 1)), "no block went to file 1");
    assert!(!exists(&l));
}
