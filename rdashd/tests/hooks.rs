//! Faults injected through the `test-hooks` feature (`src/test_hooks.rs`): SIGTERM during init
//! (`feature_shutdown`'s init case), an `AbortNode` and a failed shutdown flush, and CH2's
//! coins-read abort; and `feature_shutdown`'s RPC case, with the hooks build's
//! `waitfornewblock` stand-in; and RPC warmup, held open by the hook. Run by
//! `cargo test -p rdashd --features test-hooks --test hooks`.
#![cfg(unix)]

mod common;

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::Command;

use common::*;

const FLUSH: &str = "test-hooks: flushing the chainstate";
const STORE: &str = "test-hooks: storing the sporks";

fn with_hook(home: &Path, datadir: &Path, hook: &str) -> Command {
    let mut cmd = regtest(home, datadir, &["-printtoconsole=0"]);
    cmd.env("RDASHD_TEST_HOOK", hook);
    cmd
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
    // SAFETY: only `setrlimit` between fork and exec. No core file for the expected abort.
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
/// (with the `getrpcinfo` asking), still gets its answer, and the node exits 0. Until RP1a,
/// `waitfornewblock` is the hooks build's stand-in, which returns once the server is stopping.
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
    assert_eq!(reply.body, "{\"result\":{\"height\":0},\"error\":null,\"id\":1}\n");
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
