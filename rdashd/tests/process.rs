//! rdashd as a process: init commands, the refusals before anything is written, and a clean stop
//! by signal.

mod common;

use common::*;

#[test]
fn version_and_help_print_and_exit() {
    let home = tempfile::tempdir().unwrap();
    // As dashd v24.0.0: `-version` works without a default data directory, which it creates
    // with no empty dash.conf (dashpay/dash#8029). The default data directory is rdashd's own
    // (DEC-201 Q5).
    let out = run(home.path(), &["-version"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with(&format!("rdashd version v{}\n", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains("This is experimental software."));
    assert!(out.stderr.is_empty());
    assert!(home.path().join(".rdashcore").is_dir());
    assert!(!home.path().join(".rdashcore").join("dash.conf").exists());
    assert!(!home.path().join(".dashcore").exists());
    // Nor once the directory exists.
    assert!(run(home.path(), &["-version"]).status.success());
    assert!(!home.path().join(".rdashcore").join("dash.conf").exists());

    let out = run(home.path(), &["-help"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("\nUsage:  rdashd [options]"));
    assert!(text.contains("  -startupnotify=<cmd>"));
}

#[test]
fn command_line_errors() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["-printcrashinfo=x"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "Error: -printcrashinfo is not supported by rdashd yet (unplanned).\n"
    );
    // Core answers -printcrashinfo before reading any config: nothing is created.
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);

    std::fs::create_dir(home.path().join(".rdashcore")).unwrap();
    let out = run(home.path(), &["-regtest", "stray"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "Error: Command line contains unexpected token 'stray', see rdashd -h for a list of \
         options.\n"
    );
}

/// A `datadir=` in the default config file that names a Dash Core data directory: refused, and
/// the Core tree is untouched (ruling at C6).
#[test]
fn a_config_file_datadir_naming_core_s_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let core = home.path().join("core");
    let net = core_shaped(&core);
    let conf_dir = home.path().join(".rdashcore");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(conf_dir.join("dash.conf"), format!("datadir={}\n", core.display())).unwrap();
    let before = tree(&core);

    let out = run(home.path(), &["-regtest"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), core_datadir_refusal(&net));
    assert_eq!(tree(&core), before);
}

/// A Dash Core regtest data directory a `datadir=` in the default config file names, with the
/// child's HOME a temporary home: imported as one given on the command line (CH5-4b).
#[cfg(unix)]
#[test]
fn a_config_file_datadir_naming_a_core_regtest_directory_imports_it() {
    use common::core_cache::{build_cache, copy_tree, imported, members};
    let home = RetainedHome::new("ch54b-conf-");
    let (cache, _) = build_cache(&home);
    let core = home.path().join("core");
    copy_tree(&cache, &core);
    let regtest = net(&core);
    let core_members = members(&regtest);
    let conf_dir = home.path().join(".rdashcore");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(conf_dir.join("dash.conf"), format!("datadir={}\n", core.display())).unwrap();

    let args = ["-regtest", "-listen=0", "-server=0", "-printtoconsole=0"];
    let rdashd = std::path::Path::new(RDASHD);
    spawn_until_loaded(command(rdashd, home.path(), &args), regtest.join("debug.log"))
        .stop_ok(libc::SIGTERM);
    let backup = imported(&regtest, &core_members, &[]);
    let log = std::fs::read_to_string(regtest.join("debug.log")).unwrap();
    assert!(log.contains(&format!("aside to {}; reindexing its block files", backup.display())));
}

#[test]
fn a_core_data_directory_given_on_the_command_line_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let core = home.path().join("core");
    let net = core_shaped(&core);
    let before = tree(&core);
    let out = run_regtest(home.path(), &core, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), core_datadir_refusal(&net));
    assert_eq!(tree(&core), before);
}

/// dashd's lock on `net`: an `fcntl` write lock on `.lock`, held until the file drops.
#[cfg(unix)]
fn hold_lock(net: &std::path::Path) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let lock = std::fs::File::create(net.join(".lock")).unwrap();
    // SAFETY: `fl` is a zeroed `flock` (the whole file); `lock` is open for writing.
    let held = unsafe {
        let mut fl: libc::flock = std::mem::zeroed();
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        libc::fcntl(lock.as_raw_fd(), libc::F_SETLK, &fl)
    };
    assert_ne!(held, -1, "{}", std::io::Error::last_os_error());
    lock
}

/// A Dash Core network directory off regtest is refused by its `evodb/` before its LevelDB
/// chainstate is looked at, so while a dashd holds the directory's lock the answer is still the
/// refusal, and nothing is written. LevelDB alone is decided under the lock (D21): looked at
/// first, it would give the lock error here. On regtest every sign is decided under the lock,
/// since a Core directory there may be imported: the answer is the lock error.
#[cfg(unix)]
#[test]
fn a_core_directory_s_evodb_is_refused_before_its_chainstate() {
    let home = tempfile::tempdir().unwrap();
    let core = home.path().join("core");
    let regtest = core_shaped(&core);
    let net = core.join("testnet3");
    std::fs::rename(&regtest, &net).unwrap();
    std::fs::create_dir(net.join("evodb")).unwrap();
    let _lock = hold_lock(&net);
    let before = tree(&core);
    let out = run(home.path(), &["-testnet", &datadir_arg(&core)]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), core_datadir_refusal(&net));
    assert_eq!(tree(&core), before);

    let home = tempfile::tempdir().unwrap();
    let core = home.path().join("core");
    let net = core_shaped(&core);
    std::fs::create_dir(net.join("evodb")).unwrap();
    let _lock = hold_lock(&net);
    let before = tree(&core);
    let out = run_regtest(home.path(), &core, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!(
            "Error: Cannot obtain a lock on data directory {}. rdashd is probably already \
             running.\n",
            net.display()
        )
    );
    assert_eq!(tree(&core), before);
}

/// A devnet genesis block `LoadGenesisBlock` cannot accept (from the future under `-mocktime`)
/// is Core's init error and reindex question, not `AbortNode`: exit 1 after the shutdown, with
/// the pid file gone (dashd v24's text: `dashd_v24`).
// Core: node/chainstate.cpp:210-211, init.cpp:2109-2126, noui.cpp:47-50
#[test]
fn a_devnet_genesis_block_init_cannot_accept_fails_init() {
    let (home, datadir) = home_and_datadir();
    let out = output({
        let mut cmd =
            rdashd(home.path(), &["-devnet=nd1", "-listen=0", "-server=0", "-mocktime=1000000000"]);
        cmd.arg(datadir_arg(&datadir));
        cmd
    });
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        ": Error initializing block database.\nPlease restart with -reindex or \
         -reindex-chainstate to recover.\n"
    );
    let net = datadir.join("devnet-nd1");
    let log = read(&net.join("debug.log"));
    let question = log.find("Please restart with -reindex").expect("the question");
    let aborted = log.find("Aborted block database rebuild. Exiting.").expect("declined");
    assert!(question < aborted);
    assert_shut_down_in_order(&log[aborted..]);
    assert!(!log.contains("***"), "no AbortNode: {log}");
    assert!(!net.join("dashd.pid").exists());
    // The chainstate store holds the block index, so it opens before the index loads, as Core's
    // `blocks/index` (ND1 2b-D18); V's devnet step still refuses before the coins recover.
    assert!(net.join("chainstate").exists());
}

/// An argument that is not UTF-8 is refused rather than read lossily as another path, and the
/// refusal never shows its value (Sol r1 S1): neither the canary's bytes nor their lossy form
/// reach stderr or stdout.
#[cfg(unix)]
#[test]
fn a_non_utf8_argument_is_refused_without_its_value() {
    use std::os::unix::ffi::OsStrExt as _;
    let home = tempfile::tempdir().unwrap();
    let canary = b"canary-\xffsecret";
    let out = output({
        let mut cmd = rdashd(home.path(), &["-regtest"]);
        cmd.arg(std::ffi::OsStr::from_bytes(&[&b"-rpcpassword="[..], canary].concat()));
        cmd
    });
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "Error: Error parsing command line arguments: argument 2 (-rpcpassword) is not valid \
         UTF-8\n"
    );
    let lossy = String::from_utf8_lossy(canary).into_owned();
    for stream in [&out.stderr, &out.stdout] {
        assert!(!stream.windows(canary.len()).any(|w| w == canary));
        let text = String::from_utf8_lossy(stream);
        for needle in [lossy.as_str(), "canary", "secret"] {
            assert!(!text.contains(needle), "{needle:?} in {text:?}");
        }
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

/// SIGTERM and SIGINT stop the node in Core's order: exit 0, nothing on stderr or stdout, the
/// pid file gone. A SIGHUP before them reopens the log: the shutdown lines go to a new file
/// once the old one was moved away.
#[cfg(unix)]
#[test]
fn a_signal_stops_the_node_cleanly() {
    for sig in [libc::SIGTERM, libc::SIGINT] {
        let (home, datadir) = home_and_datadir();
        let node = start(home.path(), &datadir, &[]);
        let pid = net(&datadir).join("dashd.pid");
        assert_eq!(read(&pid).trim(), node.id().to_string());

        let moved = net(&datadir).join("debug.log.1");
        std::fs::rename(&node.log, &moved).unwrap();
        // One pipe and one thread: the reopen is requested before the shutdown.
        signal(node.id(), libc::SIGHUP);
        let log = node.log.clone();
        let out = node.stop_ok(sig);
        assert!(out.stderr.is_empty(), "{}", stderr(&out));
        assert!(out.stdout.is_empty());
        assert!(!pid.exists());
        let old = read(&moved);
        assert!(old.contains("init message: Done loading") && !old.contains("PrepareShutdown"));
        // `LoadBlockIndexDB`'s line on the empty index, between the two init messages.
        let line = old.find("Checking all blk files are present...").expect("the blk-file line");
        assert!(old.find("init message: Loading block index").unwrap() < line);
        assert!(line < old.find("init message: Done loading").unwrap());
        assert_shut_down_in_order(&read(&log));
    }
}

/// A `-stopatheight` above the tip stops nothing: the node runs on after genesis, until a
/// signal stops it cleanly.
// Core: validation.cpp:3830
#[cfg(unix)]
#[test]
fn a_stop_height_above_the_tip_leaves_the_node_running() {
    let (home, datadir) = home_and_datadir();
    let node = start(home.path(), &datadir, &["-stopatheight=1"]);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!gone(node.id()), "the node stopped by itself");
    node.stop_ok(libc::SIGTERM);
}

/// `-stopatheight=-1` stops the node once genesis connects (height 0 ≥ −1), by itself and
/// cleanly; whether init finished first is Core's race (`dashd_v24`).
// Core: validation.cpp:3830
#[test]
fn a_negative_stop_height_stops_at_genesis() {
    let (home, datadir) = home_and_datadir();
    let out = run_regtest(home.path(), &datadir, &["-printtoconsole=0", "-stopatheight=-1"]);
    assert!(matches!(out.status.code(), Some(0 | 1)), "{out:?}");
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    assert_shut_down_in_order(&read(&debug_log(&datadir)));
}

/// As Core's `SetupEnvironment` (umask 077): whatever the inherited umask, everything the node
/// creates is private to its user.
#[cfg(unix)]
#[test]
fn created_files_are_private() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    let (home, datadir) = home_and_datadir();
    let mut cmd = regtest(home.path(), &datadir, &["-printtoconsole=0"]);
    // SAFETY: only `umask` between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::umask(0o022);
            Ok(())
        });
    }
    spawn_until_loaded(cmd, debug_log(&datadir)).stop_ok(libc::SIGTERM);
    let created = tree(&datadir);
    assert!(created.contains_key(&debug_log(&datadir)));
    for path in created.keys() {
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{} is {mode:o}", path.display());
    }
}

/// `-testquorumshares` is refused as an unknown option if and only if the binary does not list it
/// in `-help-debug`.
#[test]
fn testquorumshares_matches_help_debug() {
    const UNKNOWN: &str =
        "Error: Error parsing command line arguments: Invalid parameter -testquorumshares=f\n";
    let home = tempfile::tempdir().unwrap();
    let help = run(home.path(), &["-help-debug"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let listed = stdout(&help).contains("-testquorumshares");
    let out = run(home.path(), &["-regtest", "-server=0", "-listen=0", "-testquorumshares=f"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    if listed {
        assert_ne!(stderr(&out), UNKNOWN);
        assert!(!stderr(&out).contains("Invalid parameter -testquorumshares"), "{}", stderr(&out));
    } else {
        assert_eq!(stderr(&out), UNKNOWN);
    }
}
