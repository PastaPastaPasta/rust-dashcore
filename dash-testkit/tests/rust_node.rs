//! `RustNode` against today's rdashd, the process shell (ND1): start, init log, mock time, pid
//! file and datadir lock, refusals, clean shutdown. Needs `RDASHD_PATH` (build it with
//! `cargo build -p rdashd`).
#![cfg(unix)]

use std::io::Write;
use std::time::{Duration, Instant};

use dash_testkit::{MockTime, RustNode, RustNodeConfig, StartError, TestDir};

fn config(dir: &TestDir) -> RustNodeConfig {
    RustNodeConfig::new(dir.subdir("node").unwrap())
}

#[tokio::test]
async fn starts_logs_its_init_and_stops_cleanly() {
    let dir = TestDir::new("rust-node").unwrap();
    let node = RustNode::start(config(&dir)).await.unwrap();

    let log = node.log();
    assert!(log.contains("init message: Done loading"), "{log}");
    let pid_file = node.datadir().join("regtest").join("dashd.pid");
    assert_eq!(std::fs::read_to_string(&pid_file).unwrap().trim(), node.pid().to_string());

    let stopped = node.stop().await.unwrap();
    assert!(stopped.is_clean(), "{stopped:?}");
    let prepare = stopped.log.find("PrepareShutdown: In progress...").expect("PrepareShutdown");
    assert!(prepare < stopped.log.find("Shutdown: done").unwrap(), "{}", stopped.log);
    assert!(!pid_file.exists(), "Core removes its pid file on shutdown");
}

#[tokio::test]
async fn restarts_on_its_data_directory() {
    let dir = TestDir::new("rust-node").unwrap();
    let first = RustNode::start(config(&dir)).await.unwrap();
    assert!(first.stop().await.unwrap().is_clean());

    // The second run appends to the same debug.log; start waits for its own "Done loading".
    let second = RustNode::start(config(&dir)).await.unwrap();
    let log_path = second.debug_log_path();
    let stopped = second.stop().await.unwrap();
    assert!(stopped.is_clean());
    // `Stopped::log` is the second run's part; the file holds both runs.
    assert_eq!(stopped.log.matches("Shutdown: done").count(), 1);
    let log = std::fs::read_to_string(log_path).unwrap();
    assert_eq!(log.matches("init message: Done loading").count(), 2);
    assert_eq!(log.matches("Shutdown: done").count(), 2);
}

#[tokio::test]
async fn the_stopped_process_is_the_spawned_one() {
    let dir = TestDir::new("rust-node").unwrap();
    let node = RustNode::start(config(&dir)).await.unwrap();
    let spawned = node.pid();
    // The process rdashd runs as is the one spawned: it did not fork.
    assert_eq!(node.pid_file(), Some(spawned));
    let stopped = node.stop().await.unwrap();
    assert_eq!(stopped.pid, spawned);
    assert!(stopped.is_clean(), "{stopped:?}");
    // SAFETY: signal 0 only checks for the process; it was reaped, so none should exist.
    assert_eq!(unsafe { libc::kill(spawned as libc::pid_t, 0) }, -1, "pid {spawned} still exists");
}

#[tokio::test]
async fn a_daemon_argument_is_refused_before_anything_starts() {
    let dir = TestDir::new("rust-node").unwrap();
    let datadir = dir.path().join("node");
    let mut config = RustNodeConfig::with_binary("rdashd", &datadir);
    // Past `with_args`, which would panic: the field is public.
    config.extra_args.push("-daemon".to_string());
    match RustNode::start(config).await {
        Err(StartError::NotAllowed {
            arg,
            ..
        }) => assert_eq!(arg, "-daemon"),
        Err(other) => panic!("expected NotAllowed, got {other}"),
        Ok(_) => panic!("a daemonizing node started"),
    }
    assert!(!datadir.exists(), "nothing was created");
}

#[test]
#[should_panic(expected = "`-daemonwait=1` is not allowed")]
fn with_args_panics_on_a_daemon_argument() {
    let _ = RustNodeConfig::with_binary("rdashd", "/nonexistent").with_args(["-daemonwait=1"]);
}

#[tokio::test]
async fn its_data_directory_is_owner_only_even_if_it_existed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TestDir::new("rust-node").unwrap();
    let datadir = dir.path().join("node");
    for path in [datadir.clone(), datadir.join("home")] {
        std::fs::create_dir_all(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let node = RustNode::start(RustNodeConfig::new(&datadir)).await.unwrap();
    for path in [dir.path().to_path_buf(), datadir.clone(), datadir.join("home")] {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", path.display());
    }
    assert!(node.stop().await.unwrap().is_clean());
}

/// A shell stand-in for rdashd: it finds `-datadir`, logs rdashd's version line, then runs
/// `body` (with `$d` the data directory and `$log` its debug.log).
fn stub(dir: &TestDir, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.path().join(name);
    let prelude = "#!/bin/sh\n\
         for a; do case $a in -datadir=*) d=${a#-datadir=};; esac; done\n\
         mkdir -p \"$d/regtest\"\n\
         log=\"$d/regtest/debug.log\"\n\
         echo 'rdashd version stub' >> \"$log\"\n";
    std::fs::write(&script, format!("{prelude}{body}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}

/// Starts `script` as the node in `dir/node`.
async fn start_stub(dir: &TestDir, script: &std::path::Path) -> Result<RustNode, StartError> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let config = RustNodeConfig::with_binary(script, dir.path().join("node"));
        match RustNode::start(config).await {
            // Another test's fork can hold the new script open until it execs (ETXTBSY).
            Err(StartError::Io(e))
                if e.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            result => return result,
        }
    }
}

fn expect_exit(result: Result<RustNode, StartError>, code: i32) {
    match result {
        Err(StartError::Exited {
            status,
            ..
        }) => assert_eq!(status.code(), Some(code)),
        Err(other) => panic!("expected the exit, got {other}"),
        Ok(_) => panic!("a node that exited was reported started"),
    }
}

/// A stand-in that writes its pid file, logs `Done loading` and exits 1: start must reap the exit
/// rather than accept the log. (If a poll lands after its writes and before its exit, the
/// stand-in was running when checked, and this test fails. That window is short, but under load
/// it can be a scheduler quantum, so this may rarely flake.)
#[tokio::test]
async fn a_node_that_logs_ready_and_exits_did_not_start() {
    let dir = TestDir::new("rust-node").unwrap();
    let script = stub(
        &dir,
        "ready-then-exit",
        "echo $$ > \"$d/regtest/dashd.pid\"\n\
         echo 'init message: Done loading' >> \"$log\"\n\
         exit 1",
    );
    expect_exit(start_stub(&dir, &script).await, 1);
}

/// `Done loading` beside a pid file naming another process (another rdashd on the same data
/// directory) is not this node's readiness.
#[tokio::test]
async fn a_pid_file_naming_another_process_is_not_ready() {
    let dir = TestDir::new("rust-node").unwrap();
    let script = stub(
        &dir,
        "foreign-pid",
        "echo $PPID > \"$d/regtest/dashd.pid\"\n\
         echo 'init message: Done loading' >> \"$log\"\n\
         sleep 2\n\
         exit 3",
    );
    expect_exit(start_stub(&dir, &script).await, 3);
}

/// A node ignoring SIGTERM, with its `stop()` abandoned by a timeout: the node must still be
/// killed when the abandoned stop is dropped.
#[tokio::test]
async fn an_abandoned_stop_still_kills_the_node() {
    let dir = TestDir::new("rust-node").unwrap();
    // `exec` keeps the pid, and the ignored SIGTERM survives it.
    let script = stub(
        &dir,
        "ignores-sigterm",
        "trap '' TERM\n\
         echo $$ > \"$d/regtest/dashd.pid\"\n\
         echo 'init message: Done loading' >> \"$log\"\n\
         exec sleep 600",
    );
    let node = start_stub(&dir, &script).await.unwrap();
    let pid = node.pid();
    let stop = tokio::time::timeout(Duration::from_secs(2), node.stop()).await;
    assert!(stop.is_err(), "the node exited on SIGTERM: {stop:?}");
    // SAFETY: signal 0 only checks for the process; it was killed and reaped on drop.
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    assert!(!alive, "pid {pid} outlived its abandoned stop");
}

/// Dash Core where rdashd should be: it starts, and the start fails because its log is not
/// rdashd's.
#[tokio::test]
async fn dash_core_as_rdashd_is_refused() {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        return;
    }
    let dashd =
        dash_testkit::NodeImpl::Dashd.binary().unwrap_or_else(|missing| panic!("{missing}"));
    let dir = TestDir::new("rust-node").unwrap();
    let config = RustNodeConfig::with_binary(&dashd, dir.path().join("node"));
    match RustNode::start(config).await {
        Err(StartError::NotRdashd {
            version,
            ..
        }) => {
            let version = version.expect("Core logs a version line");
            assert!(version.contains("Dash Core version"), "{version}");
        }
        Err(other) => panic!("expected NotRdashd, got {other}"),
        Ok(_) => panic!("Dash Core was accepted as rdashd"),
    }
}

/// A restart after debug.log passed 11 MB: Core's init shrinks such a log, which would move this
/// run's start in it, so the harness turns the shrink off.
#[tokio::test]
async fn restarts_after_debug_log_passes_11_mb() {
    let dir = TestDir::new("rust-node").unwrap();
    let first = RustNode::start(config(&dir)).await.unwrap();
    let log_path = first.debug_log_path();
    assert!(first.stop().await.unwrap().is_clean());
    let line = "padding line of a long-running node's debug.log\n";
    let padding = line.repeat((12 << 20) / line.len() + 1);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .unwrap()
        .write_all(padding.as_bytes())
        .unwrap();
    let padded = std::fs::metadata(&log_path).unwrap().len();
    assert!(padded > 11 << 20);

    // A shrunk log would leave this run's offset past the end: start times out with the tail.
    let second = RustNode::start(config(&dir)).await.unwrap();
    assert!(std::fs::metadata(&log_path).unwrap().len() > padded, "debug.log was shrunk");
    let stopped = second.stop().await.unwrap();
    assert!(stopped.is_clean(), "{}", stopped.log);
    assert!(!stopped.log.contains("padding line"), "Stopped::log is this run's part only");
}

/// On Linux a node dies with the thread that started it: a harness that dies without unwinding
/// leaves no node running.
#[cfg(target_os = "linux")]
#[test]
fn a_node_dies_with_the_thread_that_started_it() {
    let dir = TestDir::new("rust-node").unwrap();
    let datadir = dir.path().join("node");
    let pid = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let node = runtime.block_on(RustNode::start(RustNodeConfig::new(&datadir))).unwrap();
        let pid = node.pid();
        // As if the harness died: no Drop, so nothing kills the node but the kernel.
        std::mem::forget(node);
        pid
    })
    .join()
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut status = 0;
    loop {
        // SAFETY: reaping our own child, which the forgotten node never reaped.
        let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        if reaped == pid as libc::pid_t {
            break;
        }
        if Instant::now() > deadline {
            // SAFETY: SIGKILL to the node this test leaked.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            panic!("pid {pid} outlived the thread that started it");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL,
        "status {status:#x}"
    );
}

#[tokio::test]
async fn of_two_nodes_started_together_on_one_data_directory_one_runs() {
    let dir = TestDir::new("rust-node").unwrap();
    let (a, b) = tokio::join!(RustNode::start(config(&dir)), RustNode::start(config(&dir)));
    let (node, refused) = match (a, b) {
        (Ok(node), Err(e)) | (Err(e), Ok(node)) => (node, e),
        (Ok(_), Ok(_)) => panic!("both nodes started on one data directory"),
        (Err(a), Err(b)) => panic!("neither node started: {a}; {b}"),
    };
    // The loser exits during init. Its reason depends on how far the winner got: the lock;
    // the settings.json rename, which Core also does before locking; or, while the winner is
    // creating `regtest/`, rdashd's Core-data-directory check, which also runs before the lock.
    match refused {
        StartError::Exited {
            status,
            stderr,
        } => assert!(!status.success() && stderr.starts_with("Error: "), "{status}: {stderr}"),
        other => panic!("expected the loser to exit during init, got {other}"),
    }
    assert_eq!(node.pid_file(), Some(node.pid()));
    assert!(node.stop().await.unwrap().is_clean());
}

#[tokio::test]
async fn a_second_node_on_the_same_data_directory_is_refused_by_the_lock() {
    let dir = TestDir::new("rust-node").unwrap();
    let first = RustNode::start(config(&dir)).await.unwrap();

    match RustNode::start(config(&dir)).await {
        Err(StartError::Exited {
            status,
            stderr,
        }) => {
            assert_eq!(status.code(), Some(1));
            assert!(stderr.contains("Cannot obtain a lock on data directory"), "{stderr}");
        }
        Err(other) => panic!("expected the lock refusal, got {other}"),
        Ok(_) => panic!("a second node started on a locked data directory"),
    }
    // The first one is untouched.
    assert!(first.log().contains("init message: Done loading"));
    assert!(first.stop().await.unwrap().is_clean());
}

#[tokio::test]
async fn the_start_mocktime_stamps_its_log() {
    let dir = TestDir::new("rust-node").unwrap();
    let time = MockTime::regtest_genesis();
    let node = RustNode::start(config(&dir).with_mocktime(Some(time))).await.unwrap();
    let stopped = node.stop().await.unwrap();
    assert!(stopped.is_clean());
    // Core marks each line with the mock time once it is set (init step 3), through shutdown.
    let mark = format!("(mocktime: {})", time.time());
    let line = stopped.log.lines().find(|l| l.contains("Shutdown: done")).unwrap();
    assert!(line.contains(&mark), "{line} lacks {mark}");
}

#[tokio::test]
async fn an_unsupported_option_is_refused_with_its_owner() {
    // `-printcrashinfo` has no planned owner, so this refusal stays while others are lifted.
    let dir = TestDir::new("rust-node").unwrap();
    let err = RustNode::start(config(&dir).with_args(["-printcrashinfo=x"])).await.err().unwrap();
    match err {
        StartError::Refused {
            option,
            owner,
            ..
        } => assert_eq!((option.as_str(), owner.as_str()), ("-printcrashinfo", "unplanned")),
        other => panic!("expected a refusal, got {other}"),
    }
}

#[tokio::test]
#[ignore = "needs RP0: rdashd has no RPC server yet (-server/-rpcport are refused)"]
async fn serves_rpc_and_follows_setmocktime() {
    use dash_testkit::{NodeRpc, find_available_port};
    let dir = TestDir::new("rust-node").unwrap();
    let time = MockTime::regtest_genesis();
    let node =
        RustNode::start(config(&dir).with_mocktime(Some(time)).with_rpc(find_available_port()))
            .await
            .unwrap();
    let rpc = node.rpc().unwrap();
    assert_eq!(rpc.get_block_count().unwrap(), 0);
    rpc.set_mocktime(time.time() + 600).unwrap();
    assert!(node.stop().await.unwrap().is_clean());
}

/// The pids of live processes whose command line holds `-datadir=<datadir>` (Linux: `/proc`;
/// elsewhere none are found, so the checks on it pass vacuously).
fn processes_on(datadir: &std::path::Path) -> Vec<u32> {
    let wanted = format!("-datadir={}", datadir.display());
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|cmdline| cmdline.split(|&b| b == 0).any(|arg| arg == wanted.as_bytes()))
        })
        .collect()
}

/// An existing data directory whose `dash.conf` daemonizes: the harness's `-daemon=0
/// -daemonwait=0` override it, so the spawned process is the node and nothing outlives it.
async fn a_daemonizing_conf_is_overridden(conf: &str) {
    let dir = TestDir::new("rust-node").unwrap();
    let datadir = dir.path().join("node");
    std::fs::create_dir_all(&datadir).unwrap();
    std::fs::write(datadir.join("dash.conf"), conf).unwrap();

    let started = RustNode::start(RustNodeConfig::new(&datadir)).await;
    // Kill a forked node before any assertion, so a failing run leaves nothing behind.
    let spawned = started.as_ref().ok().map(RustNode::pid);
    let strays = kill_except(&datadir, spawned);
    let node = started.unwrap_or_else(|e| panic!("conf {conf:?}: {e}; forked {strays:?}"));
    assert_eq!(strays, Vec::<u32>::new(), "conf {conf:?}: other processes on the data directory");
    assert_eq!(node.pid_file(), spawned, "conf {conf:?}");
    assert!(node.stop().await.unwrap().is_clean(), "conf {conf:?}");
    assert_eq!(kill_except(&datadir, None), Vec::<u32>::new(), "conf {conf:?}: outlived stop");
}

/// Kills the processes on `datadir` other than `keep`, returning them.
fn kill_except(datadir: &std::path::Path, keep: Option<u32>) -> Vec<u32> {
    let strays: Vec<u32> =
        processes_on(datadir).into_iter().filter(|&pid| Some(pid) != keep).collect();
    for &pid in &strays {
        // SAFETY: SIGKILL to a node process this test started (directly or by its fork).
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    strays
}

#[tokio::test]
async fn a_conf_with_daemon_does_not_daemonize() {
    a_daemonizing_conf_is_overridden("daemon=1\n").await;
}

#[tokio::test]
async fn a_conf_with_daemonwait_does_not_daemonize() {
    a_daemonizing_conf_is_overridden("daemonwait=1\n").await;
}

#[tokio::test]
async fn a_conf_with_daemon_and_daemonwait_does_not_daemonize() {
    a_daemonizing_conf_is_overridden("daemon=1\ndaemonwait=1\n").await;
}
