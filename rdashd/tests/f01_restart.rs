//! F01 at node level (CH3 §4.6, D28, D39; CH3b D223): a script-invalid block, accepted and made
//! durable before its connect, is rejected after a `kill -9` and a restart without reindex, as
//! dashd v24 rejects it when it is submitted.
//!
//! B is block 102 on the test's own 101-block regtest chain. It spends block 1's `OP_TRUE`
//! coinbase with the scriptSig `OP_RETURN`, so it passes `CheckBlock` and `AcceptBlock`
//! (`HAVE_DATA`, `VALID_TRANSACTIONS`) and fails `CheckInputScripts` in `ConnectBlock`. rdashd runs
//! with `RDASHD_TEST_HOOK=f01-park`: at the start of B's ABC tail V's hook makes B's index record
//! durable by V's own ALWAYS flush, writes what it read back from the database to `f01-ready`,
//! and parks. The test checks that record, kills the node, restarts it (B is `invalid`, with
//! Core's `ConnectBlock`, `ConnectTip` and `InvalidChainFound` lines), stops it and starts it
//! again: that start loads B's `FAILED_VALID`, so `ResetBlockFailureFlags` reconsiders B and ABC
//! fails it once more. dashd has no seam between `AcceptBlock` and ABC, so it connects B as B is
//! submitted; its lines, chain tips and restarts are the reference. Each
//! case runs serially (`-par=1`) and with a script-check thread (`-par=2`). It cannot be skipped:
//! with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use dash_chain::{BlockIndexRecord, Status};
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::primitives::Header;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};

use common::mine::*;
use common::*;

/// The lines §3.3 of the plan names: `ConnectBlock`'s failure, `InvalidChainFound`'s and
/// `ConnectTip`'s, and the reconsider line of a start that loaded a failed block, their times cut.
const FAILURE_PREFIXES: [&str; 5] = [
    "[error] ConnectBlock(",
    "ERROR: ConnectBlock",
    "InvalidChainFound:",
    "[error] ConnectTip:",
    "ResetBlockFailureFlags:",
];

/// The abort a startup ABC that returned false would log (`node/blockstorage.cpp:949-952`). ABC
/// returns true after an invalid block (`validation.cpp:3653-3660`), so no run prints it.
const ABC_FAILED: &str = "Failed to connect best block";

/// The barrier rdashd logs once startup's ABC has run, where Core's `-stopafterblockimport`
/// stops (`node/blockstorage.cpp:955-959`).
const IMPORT_DONE: &str = "test-hooks: block import done";

/// dashd's barrier for the same: its `loadblk` thread's exit line (`util/thread.cpp:22`), after
/// `ThreadImport`'s ABC. `Done loading` is not ordered after that thread's ABC.
const LOADBLK_EXIT: &str = "loadblk thread exit";

/// The line V's F01 hook logs once `f01-ready` is in place, before it parks.
const PARKING: &str = "test-hooks: F01 ready, parking V";

/// The chain: blocks 1..=101, then B.
struct Chain {
    blocks: Vec<Mined>,
    b: Mined,
    /// B's hash, in display hex.
    b_hash: String,
    /// P's (B's parent, block 101), in display hex.
    p_hash: String,
    /// B's spend's txid, in display hex.
    spend: String,
}

/// `hash` (internal byte order) in display hex, as the RPCs and the logs print it.
fn display(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn chain() -> Chain {
    let p = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap();
    let genesis = *Header::new(p.genesis.header).hash().as_bytes();
    let mut blocks: Vec<Mined> = Vec::new();
    for height in 1..=101 {
        blocks.push(next(blocks.last().map_or(&genesis, |b| &b.hash), height, 0));
    }
    // Block 1's coinbase output 0, mature at 102, spent by `OP_RETURN` to `OP_TRUE`.
    let mut spend = 1i32.to_le_bytes().to_vec();
    spend.push(1);
    spend.extend(txid(&coinbase(1, 0)));
    spend.extend(0u32.to_le_bytes());
    spend.extend([1, 0x6a]);
    spend.extend(u32::MAX.to_le_bytes());
    spend.push(1);
    spend.extend(0i64.to_le_bytes());
    spend.extend([1, 0x51]);
    spend.extend(0u32.to_le_bytes());
    let cb = coinbase(102, 0);
    let (merkle, _) = compute_merkle_root(vec![txid(&cb), txid(&spend)]);
    let p = blocks.last().unwrap().hash;
    let spend_txid = display(&txid(&spend));
    let b = mine(&p, GENESIS_TIME + 60 * 102, &[cb, spend], Some(merkle), true);
    Chain {
        blocks,
        b_hash: display(&b.hash),
        p_hash: display(&p),
        b,
        spend: spend_txid,
    }
}

/// A home and a datadir of mode 0700, as the storage refuses a group-writable one.
fn home() -> (tempfile::TempDir, std::path::PathBuf) {
    let (home, datadir) = home_and_datadir();
    std::fs::set_permissions(&datadir, std::fs::Permissions::from_mode(0o700)).unwrap();
    (home, datadir)
}

/// `line` with its time prefix cut.
fn untimed(line: &str) -> &str {
    line.split_once(' ').map_or(line, |(_, text)| text)
}

/// `log`'s failure lines (§3.3), in order.
fn failure_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(untimed)
        .filter(|l| FAILURE_PREFIXES.iter().any(|p| l.starts_with(p)))
        .map(str::to_owned)
        .collect()
}

/// `log`'s `Loaded best chain` line (`v:5018-5022`), its clock-dependent progress masked.
fn loaded_best_chain(log: &str) -> String {
    let line = log.lines().map(untimed).find(|l| l.starts_with("Loaded best chain:"));
    let line = line.unwrap_or_else(|| panic!("no Loaded best chain line in {log}"));
    let (head, _) = line.split_once(" progress=").expect("a progress");
    format!("{head} progress=<masked>")
}

/// The debug log's length now, where the next run's lines start.
fn log_len(datadir: &Path) -> usize {
    std::fs::metadata(debug_log(datadir)).map_or(0, |m| m.len() as usize)
}

/// What the debug log gained since it was `from` bytes long.
fn log_since(datadir: &Path, from: usize) -> String {
    read(&debug_log(datadir)).get(from..).unwrap_or_default().to_owned()
}

fn submit(node: &RpcNode, block: &Mined) -> String {
    node.call("submitblock", &format!(r#"["{}"]"#, block.hex)).result().to_owned()
}

/// `submitblock` of `block`, sent without waiting for the reply: the node parks before it answers.
/// The connection stays open until the stream drops.
fn submit_detached(node: &RpcNode, block: &Mined) -> TcpStream {
    let body = format!(r#"{{"method":"submitblock","params":["{}"],"id":1}}"#, block.hex);
    let auth = base64(cookie(&node.datadir).as_bytes());
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic {auth}\r\nContent-Type: \
         application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", node.port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream
}

/// A node's chain tips and best block after a restart, and the restart's lines.
#[derive(Debug, PartialEq)]
struct Restarted {
    tips: String,
    best: String,
    loaded: String,
    /// The restart's failure lines (§3.3).
    lines: Vec<String>,
}

impl Restarted {
    fn read(node: &RpcNode, log: &str) -> Restarted {
        Restarted {
            tips: node.call("getchaintips", "[]").result().to_owned(),
            best: node.call("getbestblockhash", "[]").result().to_owned(),
            loaded: loaded_best_chain(log),
            lines: failure_lines(log),
        }
    }
}

/// What dashd did.
#[derive(Debug)]
struct Core {
    /// B's `submitblock` result.
    rejected: String,
    /// The lines its submit logged.
    lines: Vec<String>,
    /// The failure lines of its restart under `-stopafterblockimport`.
    import: Vec<String>,
    /// Its normal restart (its `lines` are those of that start).
    restarted: Restarted,
}

/// dashd's `stop`, then its clean exit.
fn stop_dashd(node: RpcNode) {
    assert_eq!(node.call("stop", "[]").result(), r#""Dash Core stopping""#);
    let out = node.node.wait();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

/// dashd v24 on the same blocks, without the park: B is connected and rejected as it is
/// submitted. Then a restart that stops after its block import, and a normal one.
fn on_dashd(chain: &Chain, par: &str) -> Core {
    let dashd = require_dashd();
    let (home, datadir) = home();
    let args = ["-disablewallet", par];
    let node = RpcNode::start_on(&dashd, home.path(), &datadir, &args);
    for block in &chain.blocks {
        assert_eq!(submit(&node, block), "null");
    }
    let rejected = submit(&node, &chain.b);
    stop_dashd(node);
    let log = read(&debug_log(&datadir));
    assert!(!log.contains(ABC_FAILED), "{log}");
    let lines = failure_lines(&log);

    let from = log_len(&datadir);
    // No RPC server, so no RPC port to lose a race for (BL-221): this run makes no call.
    let stop_after =
        [&args[..], &["-printtoconsole=0", "-server=0", "-stopafterblockimport"]].concat();
    let out = output(regtest_on(&dashd, home.path(), &datadir, &stop_after));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let log = log_since(&datadir, from);
    assert!(log.contains("Stopping after block import"), "{log}");
    assert!(!log.contains(ABC_FAILED), "{log}");
    let import = failure_lines(&log);

    // The normal restart is read once its startup ABC has run, not at `Done loading`.
    let from = log_len(&datadir);
    let node = RpcNode::start_on(&dashd, home.path(), &datadir, &args);
    wait_until("dashd's block import", || log_since(&datadir, from).contains(LOADBLK_EXIT));
    let log = log_since(&datadir, from);
    assert!(!log.contains(ABC_FAILED), "{log}");
    let restarted = Restarted::read(&node, &log);
    stop_dashd(node);
    Core {
        rejected,
        lines,
        import,
        restarted,
    }
}

/// `f01-ready`'s `key=value` lines.
fn ready_fields(path: &Path) -> BTreeMap<String, String> {
    read(path)
        .lines()
        .map(|l| {
            let (k, v) = l.split_once('=').unwrap_or_else(|| panic!("{l:?}"));
            (k.to_owned(), v.to_owned())
        })
        .collect()
}

/// Waits until `done` while `node` runs, failing at once with its stderr if it exits first.
fn wait_while_running(node: RpcNode, what: &str, mut done: impl FnMut() -> bool) -> RpcNode {
    let mut node = node;
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        if node.node.exited() {
            let out = node.node.wait();
            panic!("rdashd exited ({}) waiting for {what}: {}", out.status, stderr(&out));
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
    node
}

/// rdashd on `datadir` with `args`, waited for until its startup ABC has run (the barrier line in
/// this start's part of the log); that part's text with it.
fn start_rdashd(home: &Path, datadir: &Path, args: &[&str]) -> (RpcNode, String) {
    let from = log_len(datadir);
    let node = RpcNode::start(home, datadir, args);
    wait_until("the block import", || log_since(datadir, from).contains(IMPORT_DONE));
    let log = log_since(datadir, from);
    assert!(!log.contains(ABC_FAILED), "{log}");
    assert!(!log.contains("Reindexing"), "{log}");
    (node, log)
}

/// What rdashd did: its two restarts.
#[derive(Debug)]
struct Ours {
    restart_1: Restarted,
    restart_2: Restarted,
}

/// rdashd: the blocks, B parked at its ABC tail after the hook's flush, the durable record checked,
/// `kill -9`, a restart without reindex, a clean `stop` and a second restart.
fn on_rdashd(chain: &Chain, par: &str) -> Ours {
    let (home, datadir) = home();
    let args = [par];
    let (b, p) = (&chain.b_hash, &chain.p_hash);
    let (running, port) = start_on_free_port(&datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        let args = [&["-printtoconsole=0", rpcport.as_str()], &args[..]].concat();
        let mut cmd = regtest(home.path(), &datadir, &args);
        cmd.env("RDASHD_TEST_HOOK", "f01-park").env("RDASHD_TEST_HOOK_BLOCK", b);
        cmd
    });
    let node = RpcNode {
        node: running,
        port,
        datadir: datadir.clone(),
    };
    for block in &chain.blocks {
        assert_eq!(submit(&node, block), "null");
    }
    let _pending = submit_detached(&node, &chain.b);
    let ready = net(&datadir).join("f01-ready");
    let node = wait_while_running(node, "f01-ready", || ready.exists());

    // §3.1: before the kill, B's record is durable, from the hook's own flush, and B is neither
    // connected nor failed; the coins are at P with no transition marker.
    let f = ready_fields(&ready);
    let field = |k: &str| f.get(k).unwrap_or_else(|| panic!("{k} in {f:?}")).as_str();
    assert_eq!((field("tip"), field("coins_tip"), field("head_blocks")), (&**p, &**p, "none"));
    assert_eq!(field("before"), "none", "the hook's flush made the record durable");
    assert_eq!(field("released_epoch"), field("snapshot_epoch"));
    let record = BlockIndexRecord::decode(&parse_hex(field("record"))).unwrap();
    assert_eq!(record.height, 102);
    assert_eq!(*Header::new(record.header).hash().as_bytes(), chain.b.hash);
    assert_eq!(record.n_tx, 2);
    let status = record.status;
    assert!(status.has(Status::HAVE_DATA) && !status.has(Status::HAVE_UNDO), "{status:?}");
    assert_eq!(status.bits() & Status::VALID_MASK, Status::VALID_TRANSACTIONS, "{status:?}");
    assert!(!status.has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK), "{status:?}");
    assert_eq!((record.file, record.data_pos.is_some()), (Some(0), true));
    // V parked before its ABC: no connect has failed yet. The hook logs its park after the
    // rename, so the line is waited for.
    let node =
        wait_while_running(node, "the park line", || read(&debug_log(&datadir)).contains(PARKING));
    let log = read(&debug_log(&datadir));
    assert!(!log.contains(ABC_FAILED), "{log}");
    assert_eq!(failure_lines(&log), Vec::<String>::new());

    let out = node.node.stop(libc::SIGKILL);
    assert_eq!(out.status.signal(), Some(libc::SIGKILL), "{}", stderr(&out));

    // Restart 1, without reindex: startup's ABC tries B and fails it.
    let (node, log) = start_rdashd(home.path(), &datadir, &args);
    assert_eq!(node.call("getblockcount", "[]").result(), "101");
    let restart_1 = Restarted::read(&node, &log);
    // Shutdown's flushes write B's `FAILED_VALID`.
    node.stop();

    // Restart 2: B's `FAILED_VALID` was loaded, so the load sets it as the best invalid block
    // (`validation.cpp:5362-5363`), `ResetBlockFailureFlags(nullptr)` reconsiders it
    // (`node/chainstate.cpp:463-465`, `validation.cpp:4161-4169`) and ABC fails it again. Restart
    // 1 has no such line: B's mark was not durable before the kill.
    let (node, log) = start_rdashd(home.path(), &datadir, &args);
    let restart_2 = Restarted::read(&node, &log);
    node.stop();
    Ours {
        restart_1,
        restart_2,
    }
}

/// Both nodes on `par`: rdashd's restarts held to dashd's submit and restarts, then both to the
/// values they printed, so a change both nodes share cannot pass. `first` is `ConnectBlock`'s
/// failure line and `reason` the block's state.
fn f01(par: &str, first: impl Fn(&Chain) -> String, reason: &str) {
    let chain = chain();
    let core = on_dashd(&chain, par);
    let ours = on_rdashd(&chain, par);

    // dashd connects B at its submit, rdashd at restart 1: the same lines, no reconsider line.
    assert_eq!(ours.restart_1.lines, core.lines);
    // dashd's two restarts and rdashd's restart 2 reconsider B, then fail it again.
    assert_eq!(core.import, core.restarted.lines);
    assert_eq!(ours.restart_2.lines, core.restarted.lines);
    for ours in [&ours.restart_1, &ours.restart_2] {
        assert_eq!((&ours.tips, &ours.best), (&core.restarted.tips, &core.restarted.best));
        assert_eq!(ours.loaded, core.restarted.loaded);
    }

    let (b, p) = (&chain.b_hash, &chain.p_hash);
    // Regtest's `nBits` is two hashes of work a block: 2·103 at B, 2·102 at P.
    let (work_b, work_p) = (format!("{:064x}", 206), format!("{:064x}", 204));
    assert_eq!(core.rejected, format!("\"{reason}\""));
    let invalid = format!(
        "InvalidChainFound: invalid block={b}  height=102  log2_work=7.686501  \
         date=2014-12-04T18:57:37Z"
    );
    let best = format!(
        "InvalidChainFound:  current best={p}  height=101  log2_work=7.672425  \
         date=2014-12-04T18:56:37Z"
    );
    let connect_tip = format!("[error] ConnectTip: ConnectBlock {b} failed, {reason}");
    let lines = [first(&chain), invalid.clone(), best.clone(), connect_tip, invalid, best];
    assert_eq!(ours.restart_1.lines, lines);
    let reconsider = format!(
        "ResetBlockFailureFlags: the best known invalid block ({b}) is ahead of our tip, \
         reconsidering"
    );
    assert_eq!(
        ours.restart_2,
        Restarted {
            tips: format!(
                r#"[{{"height":102,"hash":"{b}","difficulty":4.656542373906925e-10,"chainwork":"{work_b}","branchlen":1,"forkpoint":"{p}","status":"invalid"}},{{"height":101,"hash":"{p}","difficulty":4.656542373906925e-10,"chainwork":"{work_p}","branchlen":0,"forkpoint":"{p}","status":"active"}}]"#
            ),
            best: format!("\"{p}\""),
            loaded: format!(
                "Loaded best chain: hashBestChain={p} height=101 date=2014-12-04T18:56:37Z \
                 progress=<masked>"
            ),
            lines: [&[reconsider][..], &lines].concat(),
        }
    );
}

#[test]
fn an_accepted_script_invalid_block_persisted_before_its_connect_is_rejected_after_a_kill_as_dashd_v24_rejects_it()
 {
    // Core: validation.cpp:2758-2764, the reason :2229
    let reason = "mandatory-script-verify-flag-failed (OP_RETURN was encountered)";
    let first = |chain: &Chain| {
        format!("[error] ConnectBlock(): CheckInputScripts on {} failed with {reason}", chain.spend)
    };
    f01("-par=1", first, reason);
}

#[test]
fn the_same_with_parallel_script_checks() {
    // Core: validation.cpp:2784-2787
    let first = |_: &Chain| "ERROR: ConnectBlock: CheckQueue failed".to_owned();
    f01("-par=2", first, "block-validation-failed");
}
