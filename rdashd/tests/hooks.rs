//! Faults injected through the `test-hooks` feature (`src/test_hooks.rs`): SIGTERM during init
//! (`feature_shutdown`'s init case), an `AbortNode` and a failed shutdown flush, and CH2's
//! coins-read abort; and `feature_shutdown`'s RPC case, a `waitfornewblock` that `stop` ends;
//! and RPC warmup, held open by the hook; and the storage profile the launcher starts (CH5-5);
//! and an injected V panic against dashd v24's SIGABRT (BL-586). Run by
//! `cargo test -p rdashd --features test-hooks --test hooks`.
#![cfg(unix)]

mod common;

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::Command;

use common::mine::*;
use common::*;

const FLUSH: &str = "test-hooks: flushing the chainstate";
const STORE: &str = "test-hooks: storing the sporks";
/// Regtest's genesis block hash.
const GENESIS: &str = "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e";

/// No core file for `cmd`'s expected abort.
fn without_core_file(cmd: &mut Command) {
    // SAFETY: only `setrlimit` between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            let none = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &none);
            Ok(())
        });
    }
}

fn with_hook(home: &Path, datadir: &Path, hook: &str) -> Command {
    let mut cmd = regtest(home, datadir, &["-printtoconsole=0"]);
    cmd.env("RDASHD_TEST_HOOK", hook);
    cmd
}

/// The exit of `cmd`, which init must refuse. A node that starts instead is stopped and fails
/// the test at once, rather than leaving the test waiting for its exit.
fn refused(cmd: Command, log: std::path::PathBuf) -> std::process::Output {
    match try_spawn_until_loaded(cmd, log) {
        Err(out) => out,
        Ok(running) => {
            running.stop_ok(libc::SIGTERM);
            panic!("the node started; init should have refused it");
        }
    }
}

/// CH5-5 §6.7: the shared launcher's default start-up runs regtest's live-copy storage profile
/// and says so; `-storageprofile=production` (BL-634) gives the production profile, and says
/// that, in the hooks line and in the option's own.
#[test]
fn the_launcher_starts_the_live_copy_profile() {
    const LIVE: &str = "test-hooks: live-copy storage profile on regtest\n";
    const PRODUCTION: &str = "test-hooks: production storage profile on regtest\n";
    let (home, datadir) = home_and_datadir();
    let node = start(home.path(), &datadir, &[]);
    let log = node.log.clone();
    node.stop_ok(libc::SIGTERM);
    let text = read(&log);
    assert!(text.contains(LIVE) && !text.contains(PRODUCTION), "{text}");
    assert!(!text.contains("Chainstate storage profile"), "{text}");

    let (home, datadir) = home_and_datadir();
    let node = start(home.path(), &datadir, &["-storageprofile=production"]);
    node.stop_ok(libc::SIGTERM);
    let text = read(&debug_log(&datadir));
    assert!(text.contains(PRODUCTION) && !text.contains(LIVE), "{text}");
    assert!(text.contains(PRODUCTION_PROFILE_LINE), "{text}");
}

/// BL-634 §2.6 (3): the live-copy hooks on the production profile fail init, with no RPC served,
/// so no tripwire can report on a node whose copies carry no promise.
#[test]
fn live_copy_hooks_refuse_the_production_profile() {
    let (home, datadir) = home_and_datadir();
    let hooks = tempfile::tempdir().unwrap();
    let mut cmd =
        regtest(home.path(), &datadir, &["-printtoconsole=0", "-storageprofile=production"]);
    cmd.env("RDASHD_TEST_HOOK", "live-copy").env("RDASHD_TEST_HOOK_DIR", hooks.path());
    let out = refused(cmd, debug_log(&datadir));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "Error: test-hooks: the live-copy hooks need the livecopy storage profile \
             (-storageprofile=production is set)\n"
        ),
        "{}",
        stderr(&out)
    );
    assert!(!read(&debug_log(&datadir)).contains("init message: Done loading"));
}

/// BL-634 §6.3: the retired seam variable fails a test-hooks start, so a leftover script cannot
/// silently get the live profile.
#[test]
fn the_retired_profile_seam_is_refused() {
    let (home, datadir) = home_and_datadir();
    let mut cmd = regtest(home.path(), &datadir, &["-printtoconsole=0"]);
    cmd.env("RDASHD_TEST_HOOK_PROFILE", "production");
    let out = refused(cmd, debug_log(&datadir));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out)
            .contains("Error: RDASHD_TEST_HOOK_PROFILE was replaced by -storageprofile (BL-634)\n"),
        "{}",
        stderr(&out)
    );
}

/// CH5-5 §2.2, §6.2's seams on a running node: `gettxoutsetinfo` carries the copy point's
/// counters; the `flush` hook's job ends with the release, then the post-Bk step; `quiesce`
/// answers; `verdicts` lists the copy point's barrier verdict, one line a column family; and
/// `livecopycounters` reads the counters live with the hooks' events.
#[test]
fn the_live_copy_hooks_answer() {
    let (home, datadir) = home_and_datadir();
    let hooks = tempfile::tempdir().unwrap();
    let (node, port) = start_on_free_port(&datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        let mut cmd = regtest(home.path(), &datadir, &["-printtoconsole=0", rpcport.as_str()]);
        cmd.env("RDASHD_TEST_HOOK", "live-copy").env("RDASHD_TEST_HOOK_DIR", hooks.path());
        cmd
    });
    let node = RpcNode {
        node,
        port,
        datadir: datadir.clone(),
    };
    let info = node.call("gettxoutsetinfo", "[]");
    let info = info.result();
    assert!(info.contains(r#""livecopy_counters":{"activations":"#), "{info}");
    assert!(info.contains(r#""super_versions":{"#), "{info}");

    let flush = ask(hooks.path(), "flush", "");
    let steps = flush.lines().next().unwrap().strip_prefix("steps=").expect(&flush);
    assert!(steps.ends_with("Release,AfterBk"), "{flush}");
    assert_eq!(ask(hooks.path(), "quiesce", ""), "ok\n");
    let verdicts = ask(hooks.path(), "verdicts", "");
    assert_eq!(verdicts.lines().count(), 11, "one barrier, eleven column families: {verdicts}");
    assert!(verdicts.lines().all(|l| l.starts_with("0 ")), "{verdicts}");

    let live = node.call("livecopycounters", "[]");
    let live = live.result();
    assert!(live.contains(r#""events":{"cl_turned_on_reset":"#), "{live}");
    assert!(live.contains(r#""verdicts":1"#), "{live}");
    node.stop();
}

/// OD-CH5-27 (b): with the live-copy seam off, a hooks build answers as the production build:
/// `gettxoutsetinfo` has no `livecopy_counters` and `livecopycounters` is not a command.
#[test]
fn without_the_live_copy_seam_the_counters_are_absent() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let info = node.call("gettxoutsetinfo", "[]");
    let info = info.result();
    assert!(info.contains(r#""txouts":"#), "{info}");
    assert!(!info.contains("livecopy_counters"), "{info}");
    let live = node.call("livecopycounters", "[]");
    assert_eq!(live.error(), (404, r#"{"code":-32601,"message":"Method not found"}"#));
    node.stop();
}

/// The hooks build logs each shutdown flush; a clean stop has Core's two.
#[test]
fn a_clean_stop_flushes() {
    let (home, datadir) = home_and_datadir();
    let node = start(home.path(), &datadir, &[]);
    let log = node.log.clone();
    node.stop_ok(libc::SIGTERM);
    let text = read(&log);
    // Core's two flushes (init.cpp:385, :455), both inside the shutdown (ND1-D9).
    let flushes: Vec<_> = text.match_indices(FLUSH).map(|(at, _)| at).collect();
    assert_eq!(flushes.len(), 2, "{text}");
    assert!(flushes[0] > text.find("PrepareShutdown: In progress...").unwrap());
    assert!(flushes[1] < text.find("Shutdown: done").unwrap());
    // The spork store between them (init.cpp:415).
    let store = text.find(STORE).expect("the sporks are stored");
    assert!(flushes[0] < store && store < flushes[1], "{text}");
    assert!(!text.contains("failed to flush state"), "{text}");
}

/// `feature_shutdown`, during init: as Core, "Shutdown requested. Exiting.", exit 1 and nothing
/// on stderr; the shutdown still runs and removes the pid file.
#[test]
fn a_sigterm_during_init_exits_1() {
    let (home, datadir) = home_and_datadir();
    let out = output(with_hook(home.path(), &datadir, "term-during-init"));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    let net = net(&datadir);
    let log = read(&debug_log(&datadir));
    assert!(log.contains("Shutdown requested. Exiting.\n"));
    assert!(!log.contains("Loading block index"));
    assert!(!log.contains("Done loading"));
    assert_shut_down_in_order(&log);
    assert!(!net.join("dashd.pid").exists());
}

/// An `AbortNode` after init: Core's stderr line, exit 1, the log line, and a complete shutdown
/// with both flushes. Core's `AbortNode` only sets exit 1 and requests the shutdown, which flushes
/// whenever the coins loaded (`CanFlushToDisk`; DEC-241).
// Core: shutdown.cpp:27-37, init.cpp:379-385, :449-455
#[test]
fn an_abort_flushes_twice_and_exits_1() {
    let (home, datadir) = home_and_datadir();
    let out = output(with_hook(home.path(), &datadir, "abort-after-init"));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), "Error: A fatal internal error occurred, see debug.log for details\n");
    let net = net(&datadir);
    let log = read(&debug_log(&datadir));
    assert!(log.contains("*** test-hooks: injected abort\n"));
    assert_eq!(log.matches(FLUSH).count(), 2, "{log}");
    assert!(!log.contains("failed to flush state"), "{log}");
    assert_shut_down_in_order(&log);
    assert!(!net.join("dashd.pid").exists());
}

/// Shutdown's first flush fails (its WAL sync): as Core's `FlushStateToDisk`, `HandleError`'s two
/// lines, an `AbortNode` (Core's stderr line, exit 1) and the flush's failure line. The second
/// flush still runs; the store's latch refuses it, as LevelDB fails every write after a failed log
/// sync, and it reports the same failure the same way (ND1-D16, DEC-253).
// Core: validation.cpp:3116-3117, :3122-3126, init.cpp:379-385, :449-455, dbwrapper.cpp:265-267,
// leveldb/db/db_impl.cc:1229-1244, :1328-1331
#[test]
fn a_failed_shutdown_flush_exits_1() {
    let (home, datadir) = home_and_datadir();
    let cmd = with_hook(home.path(), &datadir, "wal-sync-fails-after-init");
    let node = spawn_until_loaded(cmd, debug_log(&datadir));
    let out = node.stop(libc::SIGTERM);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let error = "Error: A fatal internal error occurred, see debug.log for details\n";
    assert_eq!(stderr(&out), error.repeat(2));
    let log = read(&debug_log(&datadir));
    let flushes: Vec<_> = log.match_indices(FLUSH).map(|(at, _)| at).collect();
    assert_eq!(flushes.len(), 2, "{log}");
    let e = "rocksdb error (Io): injected WAL sync failure";
    let system = format!("System error while flushing: {e}");
    let report = [
        format!("Z {e}\n"),
        "Z You can use -debug=leveldb to get more complete diagnostic messages\n".to_owned(),
        format!("Z *** {system}\n"),
        format!("Z {error}"),
        format!("Z ForceFlushStateToDisk: failed to flush state ({system})\n"),
    ];
    // Each flush reports in Core's order before the next one starts (`Z` ends the timestamp).
    for (n, &start) in flushes.iter().enumerate() {
        let end = flushes.get(n + 1).copied().unwrap_or(log.len());
        let mut at = start;
        for line in &report {
            at += log[at..end].find(line.as_str()).unwrap_or_else(|| panic!("{line:?}: {log}"));
        }
    }
    assert_eq!(log.matches("*** ").count(), 2, "{log}");
    assert_shut_down_in_order(&log);
}

/// CH2's coins-read failure aborts the process at once: two lines, SIGABRT, no shutdown.
#[test]
fn a_read_failure_aborts_the_process() {
    let (home, datadir) = home_and_datadir();
    let mut cmd = with_hook(home.path(), &datadir, "read-failed-after-init");
    without_core_file(&mut cmd);
    let out = output(cmd);
    assert_eq!(out.status.signal(), Some(libc::SIGABRT), "{:?}", out.status);
    assert_eq!(stderr(&out), "Error: Error reading from database, shutting down.\n");
    let net = net(&datadir);
    let log = read(&debug_log(&datadir));
    let user = log.find("Error: Error reading from database, shutting down.\n").unwrap();
    assert!(user < log.find("Error reading from database: test-hooks: injected read\n").unwrap());
    assert!(!log.contains("PrepareShutdown"));
    assert!(net.join("dashd.pid").exists());
}

/// `feature_shutdown`: `stop` while another call runs. The running call shows in `getrpcinfo`
/// (with the `getrpcinfo` asking), still gets its answer, and the node exits 0. The call is
/// `waitfornewblock`, which no block ends here: `StopRPC`'s `OnRPCStopped` wakes it and it answers
/// the tip, genesis.
// Core: test/functional/feature_shutdown.py:9-35, init.cpp:563-568
#[test]
fn stop_waits_for_a_running_call() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let port = node.port;
    let auth = cookie(&datadir);
    assert_eq!(rpc(port, &auth, "uptime", "[]").status, 200);
    let long = {
        let auth = auth.clone();
        std::thread::spawn(move || rpc(port, &auth, "waitfornewblock", "[]"))
    };
    wait_until("waitfornewblock is running", || {
        let info = rpc(port, &auth, "getrpcinfo", "[]").body;
        info.contains(r#""method":"waitfornewblock""#) && info.contains(r#""method":"getrpcinfo""#)
    });
    node.stop();
    let reply = long.join().unwrap();
    let want = format!(r#"{{"result":{{"hash":"{GENESIS}","height":0}},"error":null,"id":1}}"#);
    assert_eq!(reply.body, want + "\n");
}

/// Requests during init are answered with the init message (`RPC_IN_WARMUP`): the hook holds
/// init before it ends the warmup, so a warmup reply is certain, and releasing it ends it.
#[test]
fn requests_during_init_are_in_warmup() {
    let (home, datadir) = home_and_datadir();
    let port = free_port();
    let rpcport = format!("-rpcport={port}");
    let mut cmd = regtest(home.path(), &datadir, &["-printtoconsole=0", &rpcport]);
    cmd.env("RDASHD_TEST_HOOK", "hold-warmup");
    let cookie_path = net(&datadir).join(".cookie");
    let node = spawn_until(cmd, debug_log(&datadir), || cookie_path.exists());
    let reply = rpc(port, &read(&cookie_path), "uptime", "[]");
    assert_eq!(reply.status, 500, "{}", reply.body);
    assert!(
        reply.body.starts_with(r#"{"result":null,"error":{"code":-28,"message":""#),
        "{}",
        reply.body
    );
    std::fs::write(net(&datadir).join("release-warmup"), b"").unwrap();
    wait_until("the warmup to end", || {
        rpc(port, &read(&cookie_path), "uptime", "[]").status == 200
    });
    node.stop_ok(libc::SIGTERM);
}

/// The regtest chain both sides of [`an_injected_v_panic_runs_no_shutdown_flush_as_dashds_sigabrt_does`]
/// get by `submitblock`: five coinbase-only blocks on genesis, a minute apart.
fn five_blocks() -> Vec<Mined> {
    let mut prev: [u8; 32] = parse_hex(GENESIS).try_into().unwrap();
    prev.reverse();
    (1..=5)
        .map(|height| {
            let block = next(&prev, height, 0);
            prev = block.hash;
            block
        })
        .collect()
}

/// `hash` (internal byte order) as RPC and the log show it.
fn display(hash: &[u8; 32]) -> String {
    let mut hash = *hash;
    hash.reverse();
    hex(&hash)
}

/// The log's length once it has stopped growing for a second: the cut, after which only the
/// failure's output and what follows it are read.
fn quiet_len(log: &Path) -> u64 {
    let len = || std::fs::metadata(log).map_or(0, |m| m.len());
    let (mut last, mut still) = (len(), 0);
    wait_until("a quiet debug log", || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let now = len();
        still = if now == last {
            still + 1
        } else {
            0
        };
        last = now;
        still >= 5
    });
    last
}

/// Whether `line` is one of Core's crash-info stack frames (`   12#: (0x…) file - symbol`).
fn is_frame(line: &str) -> bool {
    let line = line.trim_start();
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with("#: ")
}

/// A line of the chainstate's shutdown-flush output: the flush's `[bench]` timers and evodb pair,
/// its `[coindb]` commit lines, `ChainStateFlushed`, and a failed flush's lines.
fn is_flush_output(line: &str) -> bool {
    [
        "FlushStateToDisk:",
        "[coindb] ",
        "ChainStateFlushed",
        "ForceFlushStateToDisk",
        "System error while flushing",
        "Fatal LevelDB error",
        "-debug=leveldb",
    ]
    .iter()
    .any(|k| line.contains(k))
}

/// A file's bytes, inode and modification time.
type FileStamp = (Vec<u8>, u64, std::time::SystemTime);

/// `mncache.dat` under `datadir` as it stands, if it exists. Any store changes it: rdashd's
/// renames a new file over it (a new inode), Core's rewrites it in place (a new mtime).
fn mncache(datadir: &Path) -> Option<FileStamp> {
    use std::os::unix::fs::MetadataExt as _;
    let path = net(datadir).join("mncache.dat");
    let meta = std::fs::metadata(&path).ok()?;
    Some((std::fs::read(&path).ok()?, meta.ino(), meta.modified().ok()?))
}

/// What one side of the vehicle did, from the cut on.
struct Crashed {
    status: std::process::ExitStatus,
    stderr: String,
    /// The debug log after the cut, timestamps kept.
    after: String,
    /// The restart's first `Loaded best chain:` line: its hash and height.
    loaded: (String, String),
    /// [`mncache`] at the cut and once the process has ended, before the restart.
    mncache: [Option<FileStamp>; 2],
}

/// The vehicle's scenario on `binary` (dashd when `core`): blocks 1-3, the forced flush
/// (`gettxoutsetinfo "none"`), block 4 and the validation queue drained, the cut; then dashd gets
/// SIGABRT, rdashd block 5, whose ABC tail V's hook panics at. One restart with no hook reads the
/// chainstate left on disk and stops cleanly.
fn crash(binary: &Path, core: bool, blocks: &[Mined]) -> Crashed {
    let (home, datadir) = home_and_datadir();
    let mut extra = vec!["-debug=bench", "-debug=coindb", "-debug=validation"];
    if core {
        extra.push("-disablewallet");
    }
    let (node, port) = start_on_free_port(&datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        let args = [&["-printtoconsole=0", rpcport.as_str()], &extra[..]].concat();
        let mut cmd = regtest_on(binary, home.path(), &datadir, &args);
        if core {
            without_core_file(&mut cmd);
        } else {
            cmd.env("RDASHD_TEST_HOOK", "v-panic-at-block")
                .env("RDASHD_TEST_HOOK_BLOCK", display(&blocks[4].hash));
        }
        cmd
    });
    let node = RpcNode {
        node,
        port,
        datadir: datadir.clone(),
    };
    let submit = |block: &Mined| {
        let reply = node.call("submitblock", &format!(r#"["{}"]"#, block.hex));
        assert_eq!(reply.result(), "null", "{}", reply.body);
    };
    blocks[..3].iter().for_each(submit);
    let info = node.call("gettxoutsetinfo", r#"["none"]"#);
    assert!(info.result().contains(r#""height":3"#), "{}", info.body);
    submit(&blocks[3]);
    assert_eq!(node.call("syncwithvalidationinterfacequeue", "[]").result(), "null");
    let best = node.call("getbestblockhash", "[]");
    assert_eq!(best.result(), format!(r#""{}""#, display(&blocks[3].hash)));
    let log = debug_log(&datadir);
    let cut = quiet_len(&log);
    let at_cut = mncache(&datadir);
    let out = if core {
        node.node.stop(libc::SIGABRT)
    } else {
        let (auth, hex) = (cookie(&datadir), blocks[4].hex.clone());
        // Its reply is not compared: the node stops serving once V's report requests the shutdown.
        let submitted = std::thread::spawn(move || {
            try_rpc(port, &auth, "submitblock", &format!(r#"["{hex}"]"#))
        });
        let out = node.node.wait();
        let _ = submitted.join();
        out
    };
    let after = read_since(&log, Some(cut));
    let mncache = [at_cut, mncache(&datadir)];

    let before = std::fs::metadata(&log).map(|m| m.len()).ok();
    let restarted = RpcNode::start_on(binary, home.path(), &datadir, &extra);
    let restart = read_since(&log, before);
    restarted.node.stop_ok(libc::SIGTERM);
    let line = restart
        .lines()
        .find_map(|l| l.split_once("Loaded best chain: ").map(|(_, rest)| rest.to_owned()))
        .unwrap_or_else(|| panic!("no Loaded best chain line: {restart}"));
    let field = |name: &str| {
        let at = line.find(name).unwrap_or_else(|| panic!("{name}: {line}")) + name.len();
        line[at..].split(' ').next().unwrap().to_owned()
    };
    Crashed {
        status: out.status,
        stderr: stderr(&out),
        after,
        loaded: (field("hashBestChain="), field("height=")),
        mncache,
    }
}

/// BL-586: an injected V panic (one of Core's asserts on the thread holding `cs_main`) against
/// dashd v24.0.0-rc.5 sent SIGABRT after the same four blocks, the termination `abort()` gives
/// every failed assert. Each side on its own home and data directory, with a forced flush at
/// block 3.
///
/// **Compared (must match):**
/// - O1: no chainstate shutdown-flush output after the cut ([`is_flush_output`]): empty on both.
///   `-debug=validation` makes Core's `ChainStateFlushed` visible; rdashd's flush lines stand for
///   it.
/// - O2: the chainstate left at its last flush: the restart's `Loaded best chain` is block 3, at
///   height 3, on both.
/// - O6: `mncache.dat` unchanged since the cut (bytes, inode, modification time), on both: dashd
///   is killed, and rdashd skips its store after the assert (DEC-1156). Both sides start on a
///   fresh data directory, where `LoadCache(false)` stores the file at startup, before the cut,
///   so it exists there on both and the check is "unchanged", not "absent".
///
/// **Each side on its own (AD-CH3-1 may-differ):**
/// - O3, termination: dashd dies of SIGABRT with no exit code; rdashd exits 1.
/// - O4, the failure's output: dashd's debug.log after the cut is `Posix Signal: Aborted` then
///   only frame lines, its stderr starts with `Posix Signal: Aborted`, and there is no `***`,
///   `Error:` or `Shutdown:` line; rdashd's debug.log has exactly one `*** V panicked: test-hooks:
///   injected V panic`, its stderr exactly one `Error: A fatal internal error occurred, see
///   debug.log for details`.
/// - O5, the shutdown (rdashd only): `Shutdown: In progress...`, the hooks build's two
///   `test-hooks: flushing the chainstate` lines (not product output) and `Shutdown: done`, in
///   order.
///
/// **Never compared:** stack frames, their addresses and count; Rust's panic hook line, its file
/// and line; the libc assertion line, which no assert produces here (the SIGABRT comes from
/// `kill`, and reproducing the line needs a patched build); the core-dump bit (the box's
/// `core_pattern`); timestamps; block 5's RPC reply; the restart's tip after its own ABC.
// Core: util/check.h:49-51; stacktraces.cpp:771-790; init.cpp:383, :453; rpc/blockchain.cpp:1238;
// init.cpp:2249, :2259; masternode/meta.cpp:31-36
#[test]
fn an_injected_v_panic_runs_no_shutdown_flush_as_dashds_sigabrt_does() {
    let dashd = require_dashd();
    let version = {
        let dir = tempfile::tempdir().unwrap();
        let out = output(command(&dashd, dir.path(), &["-version", &datadir_arg(dir.path())]));
        stdout(&out).lines().next().unwrap_or_default().to_owned()
    };
    println!(
        "dashd: {version}, {}, release build, crash hooks off (contrib/guix/libexec/build.sh:214)",
        dashd.display()
    );
    let blocks = five_blocks();
    let core = crash(&dashd, true, &blocks);
    let ours = crash(Path::new(RDASHD), false, &blocks);

    let flush_output = |c: &Crashed| -> Vec<String> {
        c.after.lines().filter(|l| is_flush_output(l)).map(str::to_owned).collect()
    };
    assert_eq!(flush_output(&ours), flush_output(&core), "O1");
    assert_eq!(flush_output(&core), Vec::<String>::new(), "O1: {}", core.after);
    assert_eq!(ours.loaded, core.loaded, "O2");
    assert_eq!(core.loaded, (display(&blocks[2].hash), "3".to_owned()), "O2");
    for (side, c) in [("dashd", &core), ("rdashd", &ours)] {
        assert!(c.mncache[0].is_some(), "O6: {side} stored mncache.dat before the cut");
        let stamps = c.mncache.each_ref().map(|m| m.as_ref().map(|(_, ino, at)| (ino, at)));
        assert!(
            c.mncache[0] == c.mncache[1],
            "O6: {side} wrote mncache.dat after the cut: {stamps:?}"
        );
    }

    assert_eq!(core.status.signal(), Some(libc::SIGABRT), "O3: {:?}", core.status);
    assert_eq!(core.status.code(), None, "O3");
    assert_eq!(ours.status.code(), Some(1), "O3: {}", ours.stderr);

    let lines: Vec<&str> = core.after.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        lines.first().is_some_and(|l| l.ends_with("Z Posix Signal: Aborted")),
        "O4: {}",
        core.after
    );
    assert!(lines[1..].iter().all(|l| is_frame(l)), "O4: {}", core.after);
    assert!(core.stderr.starts_with("Posix Signal: Aborted\n"), "O4: {}", core.stderr);
    let failure_line =
        |l: &&str| l.contains("*** ") || l.contains("Error:") || l.contains("Shutdown:");
    assert!(!core.after.lines().any(|l| failure_line(&l)), "O4: {}", core.after);
    let reports: Vec<&str> =
        (ours.after.lines()).filter_map(|l| l.find("*** ").map(|at| &l[at..])).collect();
    assert_eq!(reports, ["*** V panicked: test-hooks: injected V panic"], "O4: {}", ours.after);
    let error = "Error: A fatal internal error occurred, see debug.log for details";
    let errors: Vec<&str> = ours.stderr.lines().filter(|l| l.starts_with("Error:")).collect();
    assert_eq!(errors, [error], "O4: {}", ours.stderr);

    assert_shut_down_in_order(&ours.after);
    let flushes: Vec<_> = ours.after.match_indices(FLUSH).map(|(at, _)| at).collect();
    assert_eq!(flushes.len(), 2, "O5: {}", ours.after);
    assert!(ours.after.find("Shutdown: In progress...").unwrap() < flushes[0], "O5");
    assert!(flushes[1] < ours.after.find("Shutdown: done").unwrap(), "O5");
}

/// BL-502 test 7: the process RNG's state does not exist yet just before the daemon step, and
/// the serving process holds its one chunk of the locked pool, so `getmemoryinfo` is dashd's
/// idle answer (240 bytes used in one arena of 256 KiB, or less where `RLIMIT_MEMLOCK` caps the
/// first arena lower): without `-daemon`, and with `-daemonwait`, where the child serves.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
#[test]
fn the_rng_state_is_made_after_the_daemon_step() {
    const BEFORE: &str = "test-hooks: process RNG state before the daemon step: false\n";
    let idle = |port: u16, datadir: &Path| {
        let reply = rpc(port, &cookie(datadir), "getmemoryinfo", "[]").result().to_owned();
        let stats: serde_json::Value = serde_json::from_str(&reply).unwrap();
        let n = |key: &str| stats["locked"][key].as_u64().unwrap_or_else(|| panic!("{reply}"));
        assert_eq!((n("used"), n("chunks_used"), n("chunks_free")), (240, 1, 1), "{reply}");
        assert_eq!(n("used") + n("free"), n("total"), "{reply}");
        assert!((1..=256 * 1024).contains(&n("total")), "{reply}");
        assert!(n("locked") <= n("total"), "{reply}");
    };
    let one_line = |log: &str| {
        assert_eq!(log.matches("test-hooks: process RNG state").count(), 1, "{log}");
        assert!(log.contains(BEFORE), "{log}");
    };

    let (home, datadir) = home_and_datadir();
    let (node, port) = start_on_free_port(&datadir, free_port, |port| {
        let rpcport = format!("-rpcport={port}");
        let mut cmd = regtest(home.path(), &datadir, &["-printtoconsole=0", &rpcport]);
        cmd.env("RDASHD_TEST_HOOK", "rng-order");
        cmd
    });
    idle(port, &datadir);
    node.stop_ok(libc::SIGTERM);
    one_line(&read(&debug_log(&datadir)));

    let (home, datadir) = home_and_datadir();
    let port = free_port();
    let rpcport = format!("-rpcport={port}");
    let mut cmd = regtest(home.path(), &datadir, &["-printtoconsole=0", &rpcport, "-daemonwait"]);
    cmd.env("RDASHD_TEST_HOOK", "rng-order");
    let out = output(cmd);
    let pid_file = net(&datadir).join("dashd.pid");
    let pid: u32 = read(&pid_file).trim().parse().expect("the daemon's pid");
    struct Daemon(u32);
    impl Drop for Daemon {
        fn drop(&mut self) {
            if !gone(self.0) {
                // SAFETY: killing the daemon this test started.
                unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
    let daemon = Daemon(pid);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    idle(port, &datadir);
    signal(pid, libc::SIGTERM);
    wait_until("the daemon's exit", || gone(pid));
    drop(daemon);
    one_line(&read(&debug_log(&datadir)));
}
