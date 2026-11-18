//! Dash Core's functional tests for the process shell, mirrored on rdashd: `feature_filelock`,
//! `feature_logging`, `feature_shutdown` (signal form) and `feature_startupnotify`, plus
//! `-shutdownnotify` (`feature_notifications`) and `-daemon`/`-daemonwait`. Their RPC parts
//! (`.cookie`, `getrpcinfo`) are RP0's, and `getblockcount` O11's. The log lines of
//! `feature_config_args` and `feature_init` that rdashd emits are checked here too: these tests
//! are the proofs the asserted-log list cites (DEC-204).
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;

/// `feature_filelock`: a second instance on the same data directory is refused, and leaves the
/// first one's pid file in place.
#[test]
fn a_second_instance_on_the_same_data_directory_is_refused() {
    let (home, datadir) = home_and_datadir();
    let first = start(home.path(), &datadir, &[]);
    let out = run_regtest(home.path(), &datadir, &["-noserver"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!(
            "Error: Cannot obtain a lock on data directory {}. rdashd is probably already \
             running.\n",
            net(&datadir).display()
        )
    );
    assert_eq!(read(&net(&datadir).join("dashd.pid")).trim(), first.id().to_string());
    first.stop_ok(libc::SIGTERM);
}

/// Two nodes started together on one fresh data directory: exactly one runs, and the other
/// fails with the lock error, never the Core data directory refusal. The loser can see the
/// winner's databases half-made: a RocksDB has `CURRENT` and a `MANIFEST` before its `OPTIONS`,
/// but its `IDENTITY` before either, so that is no sign of Core's (BL-147); a LevelDB-shaped one
/// would be confirmed under the lock, as Core opens its databases only once it holds the
/// directory (D21). Each round starts the second node a little later, to sweep the race.
///
/// The settings file is written before the lock, in Core as here, through one fixed
/// `settings.json.tmp`: when both nodes write it at once, the later rename fails, with Core's
/// text. That is the loser's only other answer, and with `-nosettings` (odd rounds) it has none.
// Core: init.cpp:1470-1505 (LockDataDirectory), util/system.cpp:486-504 (WriteSettingsFile)
#[test]
fn two_nodes_starting_together_meet_the_lock() {
    const ROUNDS: u64 = 40;
    for round in 0..ROUNDS {
        let (home, datadir) = home_and_datadir();
        let settings = if round % 2 == 0 {
            "-settings=settings.json"
        } else {
            "-nosettings"
        };
        let node = || {
            let cmd = regtest(home.path(), &datadir, &["-printtoconsole=0", settings]);
            Running::spawn(cmd, debug_log(&datadir))
        };
        let mut a = node();
        std::thread::sleep(Duration::from_millis(round / 2 * 5));
        let mut b = node();
        wait_until("one node to exit", || a.exited() || b.exited());
        let (lost, mut won) = if a.exited() {
            (a, b)
        } else {
            (b, a)
        };
        let out = lost.wait();
        assert_eq!(out.status.code(), Some(1), "round {round}: {}", stderr(&out));
        let net = net(&datadir);
        let lock = format!(
            "Error: Cannot obtain a lock on data directory {}. rdashd is probably already \
             running.\n",
            net.display()
        );
        let rename = format!(
            "Error: Settings file could not be written:\n- Failed renaming settings file {} to \
             {}\n\n",
            net.join("settings.json.tmp").display(),
            net.join("settings.json").display()
        );
        let err = stderr(&out);
        assert!(err == lock || (round % 2 == 0 && err == rename), "round {round}: {err}");
        let log = won.log.clone();
        wait_until("the other node to load", || {
            assert!(!won.exited(), "round {round}: both nodes exited");
            read(&log).contains("init message: Done loading")
        });
        won.stop_ok(libc::SIGTERM);
    }
}

/// A first start killed before RocksDB writes its `OPTIONS` file leaves `CURRENT` and a
/// `MANIFEST` without one, as LevelDB has. RocksDB's `IDENTITY` is there by then, so the next
/// start opens the database, as Core reopens its own (BL-147 (b)). A node stopped after loading
/// leaves the same files once its `OPTIONS` are deleted, and its next start writes them again.
// Core: leveldb/db/db_impl.cc:290-317
#[test]
fn a_chainstate_without_its_options_file_is_rdashd_s() {
    let (home, datadir) = home_and_datadir();
    start(home.path(), &datadir, &[]).stop_ok(libc::SIGTERM);
    let chainstate = net(&datadir).join("chainstate");
    let options = || -> Vec<PathBuf> {
        let paths = std::fs::read_dir(&chainstate).unwrap().map(|e| e.unwrap().path());
        paths.filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-")).collect()
    };
    let written = options();
    assert!(!written.is_empty() && chainstate.join("IDENTITY").is_file());
    for path in written {
        std::fs::remove_file(path).unwrap();
    }
    start(home.path(), &datadir, &[]).stop_ok(libc::SIGTERM);
    assert!(!options().is_empty());
}

/// Runs rdashd on regtest in `datadir` until init is done, then stops it (see
/// [`stop_once_started`]).
fn start_and_stop(home: &Path, datadir: &Path, extra: &[&str]) {
    let cmd = regtest(home, datadir, &[&["-printtoconsole=0"], extra].concat());
    stop_once_started(home, cmd, debug_log(datadir));
}

/// Runs `cmd` with `-startupnotify` touching a file, so init is known to be done without reading
/// a debug log there may not be. Stops it with SIGTERM.
fn stop_once_started(home: &Path, mut cmd: Command, log: PathBuf) {
    let started = home.join("started");
    let _ = std::fs::remove_file(&started);
    cmd.arg(format!("-startupnotify=touch {}", sh_quote(&started)));
    spawn_until(cmd, log, || started.exists()).stop_ok(libc::SIGTERM);
}

/// `feature_logging`'s `-debuglogfile` cases.
#[test]
fn debuglogfile_names_the_log() {
    let (home, datadir) = home_and_datadir();
    let net = net(&datadir);
    let default_log = debug_log(&datadir);
    start_and_stop(home.path(), &datadir, &[]);
    assert!(default_log.is_file());

    // Relative names are in the network directory.
    start_and_stop(home.path(), &datadir, &["-debuglogfile=foo.log"]);
    assert!(net.join("foo.log").is_file());

    // Absolute names anywhere.
    let outside = home.path().join("foo.log");
    start_and_stop(home.path(), &datadir, &[&format!("-debuglogfile={}", outside.display())]);
    assert!(outside.is_file());

    // A missing directory is an init error, relative or absolute, and nothing is created; once
    // the directory exists, the node starts.
    let absolute = home.path().join("foo");
    let cases: [(String, PathBuf); 2] = [
        ("foo/foo.log".into(), net.join("foo")),
        (absolute.join("foo.log").display().to_string(), absolute),
    ];
    for (name, dir) in cases {
        let arg = format!("-debuglogfile={name}");
        let out = run_regtest(home.path(), &datadir, &[&arg]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            stderr(&out),
            format!("Error: Could not open debug log file {}\n", dir.join("foo.log").display())
        );
        assert!(!dir.exists());
        std::fs::create_dir(&dir).unwrap();
        start_and_stop(home.path(), &datadir, &[&arg]);
        assert!(dir.join("foo.log").is_file());
    }

    // -nodebuglogfile writes no log.
    std::fs::remove_file(&default_log).unwrap();
    start_and_stop(home.path(), &datadir, &["-nodebuglogfile"]);
    assert!(!default_log.exists());

    // Just a sanity check that nothing breaks.
    start_and_stop(home.path(), &datadir, &["-debuglogfile=/dev/null"]);
}

/// `feature_logging`'s invalid `-debug`, `-debugexclude` and `-loglevel` values (P2P-24). The
/// first three are matched in full, the last two by prefix, as the functional test does; the
/// dashd v24 differential (`dashd_v24.rs`) compares all five byte for byte.
#[test]
fn invalid_logging_values_are_init_errors() {
    let (home, datadir) = home_and_datadir();
    let cases = [
        ("-debug=abc", "Error: Unsupported logging category -debug=abc.\n", true),
        ("-debugexclude=abc", "Error: Unsupported logging category -debugexclude=abc.\n", true),
        (
            "-loglevel=abc",
            "Error: Unsupported global logging level -loglevel=abc. Valid values: info, debug, \
             trace.\n",
            true,
        ),
        (
            "-loglevel=net:abc",
            "Error: Unsupported category-specific logging level -loglevel=net:abc.",
            false,
        ),
        (
            "-loglevel=net:info:abc",
            "Error: Unsupported category-specific logging level -loglevel=net:info:abc.",
            false,
        ),
    ];
    for (arg, expected, full) in cases {
        let out = run_regtest(home.path(), &datadir, &[arg]);
        assert_eq!(out.status.code(), Some(1), "{arg}");
        let text = stderr(&out);
        if full {
            assert_eq!(text, expected);
        } else {
            assert!(text.starts_with(expected), "{arg}: {text}");
        }
    }
}

/// `feature_startupnotify`: the command runs once per start, after init is done, and not when
/// init fails. `-shutdownnotify` runs once per stop, failed init or not (Core runs `Interrupt`).
#[test]
fn notify_commands_run_once_per_start_and_stop() {
    let (home, datadir) = home_and_datadir();
    let started = home.path().join("started.txt");
    let stopped = home.path().join("stopped.txt");
    // The startup command counts the "Done loading" lines already in the log (the bracket keeps
    // the logged command line from matching).
    let startup = format!(
        "-startupnotify=grep -c 'init message: Done loadin[g]' {} >> {}",
        sh_quote(&debug_log(&datadir)),
        sh_quote(&started)
    );
    let shutdown = format!("-shutdownnotify=echo stopped >> {}", sh_quote(&stopped));
    for n in 1..=2 {
        let node = start(home.path(), &datadir, &[&startup, &shutdown]);
        wait_until("startupnotify", || read(&started).lines().count() == n);
        node.stop_ok(libc::SIGTERM);
        let counts: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        assert_eq!(read(&started).lines().collect::<Vec<_>>(), counts);
        assert_eq!(read(&stopped).lines().count(), n);
    }

    // A failed init (the pid file cannot be written).
    let pid = format!("-pid={}", home.path().join("missing").join("x.pid").display());
    let out = run_regtest(home.path(), &datadir, &[&startup, &shutdown, &pid]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(read(&stopped).lines().count(), 3);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(read(&started).lines().count(), 2);
}

/// A detached node this test started: SIGKILLed if the test fails before it exits.
struct Daemon(u32);

impl Drop for Daemon {
    fn drop(&mut self) {
        if !gone(self.0) {
            // SAFETY: killing the daemon this test started.
            unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// `-daemon`: the parent says it is starting and exits 0 at once; the child runs on until a
/// SIGTERM stops it. `-daemonwait`: the parent exits once the child's init is done.
#[test]
fn the_daemon_parent_exits_and_the_child_runs() {
    for (flag, waits) in [("-daemon", false), ("-daemonwait", true)] {
        let (home, datadir) = home_and_datadir();
        let log = debug_log(&datadir);
        let pid_file = net(&datadir).join("dashd.pid");
        let mut cmd = regtest(home.path(), &datadir, &[flag]);
        let child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let parent = child.id();
        let out = finish(child);
        // The guard first: no assertion below may leak the daemon.
        wait_until("the daemon's pid file", || read(&pid_file).ends_with('\n'));
        let pid: u32 = read(&pid_file).trim().parse().unwrap();
        let daemon = Daemon(pid);
        assert_ne!(pid, parent);

        assert_eq!(out.status.code(), Some(0), "{flag}: {}", stderr(&out));
        assert_eq!(stdout(&out), "rdashd starting\n");
        assert!(out.stderr.is_empty());
        if waits {
            assert!(read(&log).contains("init message: Done loading"));
        }
        wait_until("the daemon's init", || read(&log).contains("init message: Done loading"));

        signal(pid, libc::SIGTERM);
        wait_until("the daemon's exit", || gone(pid));
        assert!(!pid_file.exists());
        assert_shut_down_in_order(&read(&log));
        drop(daemon);
    }
}

/// With `-daemonwait` a failed init (the pid file cannot be written) fails the parent with Core's
/// line.
#[test]
fn a_failed_daemon_init_fails_the_waiting_parent() {
    let (home, datadir) = home_and_datadir();
    let pid = format!("-pid={}", home.path().join("missing").join("x.pid").display());
    let out = run_regtest(home.path(), &datadir, &[&pid, "-daemonwait"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "rdashd starting\n");
    assert_eq!(stderr(&out), "Error during initialization - check debug.log for details\n");
}

/// What `feature_config_args`'s `test_args_log` expects in the log.
const ARGS_LOG_EXPECTED: [&str; 7] = [
    "Command-line arg: addnode=\"some.node\"",
    "Command-line arg: rpcauth=****",
    "Command-line arg: rpcpassword=****",
    "Command-line arg: rpcuser=****",
    "Command-line arg: torpassword=****",
    "Config file arg: regtest=\"1\"",
    "Config file arg: [regtest] server=\"1\"",
];

/// What it must not find: the sensitive values, and the non-sensitive ones masked.
const ARGS_LOG_UNEXPECTED: [&str; 5] = [
    "alice:f7efda5c189b999524f151318c0c86$d5b51b3beffbc0",
    "secret-rpcuser",
    "secret-torpassword",
    "Command-line arg: rpcbind=****",
    "Command-line arg: rpcallowip=****",
];

/// `Err` names the first expected line `log` lacks, or the first unexpected text it holds.
fn args_log_check(log: &str) -> Result<(), String> {
    if let Some(line) = ARGS_LOG_EXPECTED.iter().find(|l| !log.contains(*l)) {
        return Err(format!("{line:?} is not logged"));
    }
    match ARGS_LOG_UNEXPECTED.iter().find(|u| log.contains(*u)) {
        Some(text) => Err(format!("{text:?} is logged")),
        None => Ok(()),
    }
}

/// `feature_config_args`'s `test_args_log`, with Core's arguments and configuration: the
/// arguments are logged with the sensitive values masked, and the whole log, from the process's
/// start to its shutdown, holds none of the five texts Core checks. rdashd refuses `-addnode`
/// until P2a-net lands, so that run ends at the refusal, after the arguments are logged, and the
/// node shuts down as Core's does. Without it the RPC options and `-torpassword` (P8b; unused,
/// as `-listen=0` turns `-listenonion` off)
/// reach the RPC server, which binds, reads `-rpcauth` and writes its cookie, and the node runs
/// until it is stopped. Each refusal is pinned: whoever lifts one revisits this proof, since the
/// options it lets through then reach code these runs never did.
#[test]
fn the_args_log_masks_sensitive_values() {
    let (home, datadir) = home_and_datadir();
    std::fs::write(datadir.join("dash.conf"), "regtest=1\n[regtest]\nserver=1\n").unwrap();
    let args = [
        "-addnode=some.node",
        "-rpcauth=alice:f7efda5c189b999524f151318c0c86$d5b51b3beffbc0",
        "-rpcbind=127.1.1.1",
        "-rpcbind=127.0.0.1",
        "-rpcallowip=127.0.0.1",
        "-rpcpassword=",
        "-rpcuser=secret-rpcuser",
        "-torpassword=secret-torpassword",
    ];
    let rpcport = format!("-rpcport={}", free_port());
    // `-listen=0`: tests run in parallel, and regtest's default P2P port is fixed (BL-365).
    let node = |args: &[&str]| {
        let datadir = datadir_arg(&datadir);
        let mut cmd = rdashd(home.path(), &[&datadir, "-printtoconsole=0", "-listen=0", &rpcport]);
        cmd.args(args);
        cmd
    };
    // Core's arguments, then without the refused option.
    let out = output(node(&args));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), "Error: -addnode is not supported by rdashd yet (P2a-net).\n");
    spawn_until_loaded(node(&args[1..]), debug_log(&datadir)).stop_ok(libc::SIGTERM);
    let log = read(&debug_log(&datadir));
    assert!(log.contains("Using rpcauth authentication."));
    assert!(log.contains("Binding RPC on address 127.1.1.1 port"));
    assert_shut_down_in_order(&log);
    args_log_check(&log).unwrap();
    // The check itself fails on a log without any one expected line, or with any one text.
    for line in ARGS_LOG_EXPECTED {
        assert!(args_log_check(&log.replace(line, "")).is_err(), "{line}");
    }
    for text in ARGS_LOG_UNEXPECTED {
        assert!(args_log_check(&format!("{log}{text}\n")).is_err(), "{text}");
    }
}

/// `feature_config_args`'s config-file cases. An unknown value in an included file is ignored
/// with a warning; `reindex=1` in it warns (rdashd then refuses `-reindex` until CH5);
/// `-noconnect=0` warns of the double negative; and
/// the configuration file read is named, here one in the default data directory that names the
/// data directory.
#[test]
fn config_file_cases_are_logged() {
    let (home, datadir) = home_and_datadir();
    let include = datadir.join("include.conf");
    let conf = format!("includeconf={}\n", include.display());
    std::fs::write(datadir.join("dash.conf"), conf).unwrap();
    let log = debug_log(&datadir);
    // What `run` adds to the log.
    let logged = |run: &dyn Fn()| {
        let before = read(&log).len();
        run();
        read(&log)[before..].to_owned()
    };

    std::fs::write(&include, "dash_conf=1\n").unwrap();
    let new = logged(&|| start_and_stop(home.path(), &datadir, &[]));
    assert!(new.contains("Ignoring unknown configuration value dash_conf\n"), "{new}");

    std::fs::write(&include, "reindex=1\n").unwrap();
    let new = logged(&|| {
        let out = run_regtest(home.path(), &datadir, &["-printtoconsole=0"]);
        assert_eq!(stderr(&out), "Error: -reindex is not supported by rdashd yet (CH5).\n");
    });
    let warning = "Warning: reindex=1 is set in the configuration file, which will significantly \
                   slow down startup. Consider removing or commenting out this option for better \
                   performance, unless there is currently a condition which makes rebuilding the \
                   indexes necessary\n";
    assert!(new.contains(warning), "{new}");

    std::fs::write(&include, "").unwrap();
    // `-connect=1` dials the name `1`, which never resolves.
    let new = logged(&|| start_and_stop(home.path(), &datadir, &["-noconnect=0"]));
    assert!(new.contains("Warning: parsed potentially confusing double-negative -connect=0\n"));

    let default_conf = home.path().join(".rdashcore").join("dash.conf");
    std::fs::create_dir_all(default_conf.parent().unwrap()).unwrap();
    std::fs::write(&default_conf, format!("datadir={}\nregtest=1\n", datadir.display())).unwrap();
    let new = logged(&|| {
        // No `-datadir` or `-regtest` here, so not [`regtest`], but the same fixed-port care (BL-365).
        let rpcport = format!("-rpcport={}", free_port());
        let args = ["-allowignoredconf", "-printtoconsole=0", "-listen=0", &rpcport];
        let cmd = rdashd(home.path(), &args);
        stop_once_started(home.path(), cmd, log.clone());
    });
    assert!(new.contains(&format!("Config file: {}\n", default_conf.display())), "{new}");
}

/// `feature_init`'s start lines that rdashd emits: the scheduler thread's start, init's
/// block-index steps and regtest's `AssumedValidBlock()` (null: every block's scripts). Core
/// also stops the node at each line and checks it starts clean after; these rows track only
/// that the lines appear. The others are pending with their owners in the
/// asserted-log list.
#[test]
fn init_logs_its_start_lines() {
    let (home, datadir) = home_and_datadir();
    start_and_stop(home.path(), &datadir, &[]);
    let log = read(&debug_log(&datadir));
    for line in [
        "scheduler thread start",
        "init message: Loading block index…",
        "Validating signatures for all blocks",
        "init message: Verifying blocks…",
    ] {
        assert!(log.contains(line), "{line}: {log}");
    }
}
