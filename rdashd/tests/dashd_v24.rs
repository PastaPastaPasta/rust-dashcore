//! rdashd against dashd v24.0.0 as the oracle: the init errors, the data directory lock,
//! and the refusal of a data directory dashd made.
//!
//! Gated like the other dashd tests: skipped when `SKIP_DASHD_TESTS` is set. The dashd is
//! `DASHD_V24_PATH`, else the copy `DASHVERSION=24.0.0 python3 contrib/setup-dashd.py`
//! caches (see `common::find_dashd`). Every dashd runs on a private data directory, home and
//! ports.
#![cfg(unix)]

mod common;

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::mine::{coinbase, next_with};
use common::*;

/// Argument sets on which both binaries fail init, most before binding any port. `$TMP` is the
/// run's own directory. A case that binds one names it `$PORT1` or `$PORT2`: a free port picked
/// for the case, the same for both binaries, since a fixed port another run holds changes the
/// error (BL-562). Stderr and the exit code must be equal once Core's `PACKAGE_NAME` ("Dash
/// Core") reads rdashd's; no message here is written by hand.
///
/// Left out: `-mocktime=-1`, where Core fails an assertion and rdashd reports an init error (an
/// accepted deviation), and option values Core accepts without error: since v24.0.0 those
/// include `-bind` without an `=onion` bind ([`a_bind_without_an_onion_bind_starts`]) and the
/// sets in [`ACCEPTED_BY_DASHD_UNSUPPORTED_BY_RDASHD`].
const CASES: &str = "
    # Command line, config file and chain (InitConfig)
    -regtest -foo
    -regtest -testnet
    -regtest -signet
    -regtest -chain=main
    -chain=foo
    -regtest -conf=$TMP/missing.conf
    -regtest -includeconf=x
    -regtest -datadir=$TMP/missing
    -regtest -devnet=foo
    # Chain parameters (SelectParams)
    -regtest -vbparams=foo
    -regtest -vbparams=foo:1:2
    -regtest -testactivationheight=foo@1
    -regtest -testactivationheight=dip0003@x
    -regtest -budgetparams=1:2
    -regtest -dip3params=1
    -regtest -llmqinstantsend=llmq_test
    -regtest -llmqtestinstantsendparams=x
    -devnet=foo -port=29999 -rpcport=29998 -llmqchainlocks=foo
    # Step 2: parameter interactions
    -regtest -blocksdir=$TMP/missing
    -regtest -blockfilterindex=foo
    -regtest -peerblockfilters=1
    -regtest -prune=1000 -txindex=1
    -regtest -prune=1000 -txindex=0 -addressindex=1
    -regtest -prune=1000 -txindex=0 -spentindex=1
    -regtest -prune=1000 -txindex=0 -reindex-chainstate
    # The prune checks in their order before Core's later refusals and rdashd's support table
    -regtest -prune=1000 -reindex-chainstate -coinstatsindex -blockfilterindex
    -regtest -prune=1000 -txindex=0 -reindex-chainstate -coinstatsindex -blockfilterindex
    -regtest -prune=1000 -txindex=0 -addressindex=1 -reindex-chainstate
    -regtest -prune=1000 -txindex=0 -reindex-chainstate -disablegovernance=0
    -regtest -prune=1000 -txindex=0 -reindex-chainstate -loadblock=$TMP/missing
    -regtest -prune=1000 -txindex=0 -disablegovernance=0
    -devnet=foo
    -devnet=foo -port=29999
    -devnet=foo -devnet=bar -port=29999 -rpcport=29998
    -regtest -dnsseed=0 -forcednsseed=1
    -regtest -listen=0 -bind=127.0.0.1
    -regtest -listen=0 -whitebind=127.0.0.1
    -regtest -listen=0 -listenonion=1
    # Step 6: the ports and socket addresses (BL-327)
    -regtest -server=0 -port=0
    -regtest -server=0 -port=x
    -regtest -server=0 -port=65536
    -regtest -server=0 -rpcport=1 -noport
    -regtest -rpcport=0
    -regtest -bind=127.0.0.1:0
    -regtest -bind=127.0.0.1:x=onion
    -regtest -whitebind=1.2.3.4:0
    -regtest -rpcbind=1.2.3.4:
    -regtest -proxy=1.2.3.4:x
    -regtest -torcontrol=[::1]:65536
    -regtest -i2psam=1.2.3.4:0
    -regtest -onion=1.2.3.4:x
    -regtest -zmqpubrawtx=127.0.0.1:0
    -regtest -zmqpubhashblock=unix:/x -zmqpubsequence=x:65536
    # ...after the spork checks, before -uacomment's
    -regtest -sporkaddr=x -port=0
    -regtest -sporkkey=x -port=0
    -regtest -port=0 -uacomment=a/b
    # ...after -uacomment's: the reachable networks, the proxies and -externalip (P8a), names
    # not looked up (-dns=0)
    -regtest -dns=0 -onlynet=abc
    -regtest -dns=0 -onlynet=tor -onlynet=ipv5
    -regtest -dns=0 -onlynet=cjdns
    -regtest -dns=0 -onlynet=onion -dnsseed=1
    -regtest -dns=0 -proxy=abc..abc:23456
    -regtest -dns=0 -proxy=unix:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
    -regtest -dns=0 -onion=xyz..xyz:23456
    -regtest -dns=0 -onion=0.0.0.0
    -regtest -dns=0 -onlynet=onion -onion=0
    -regtest -dns=0 -onlynet=onion -noonion
    -regtest -dns=0 -onlynet=onion -listenonion=0
    -regtest -dns=0 -onlynet=onion -onion=1.2.3.4 -externalip=foo
    -regtest -dns=0 -externalip=[::]
    # ...a name that would resolve is not looked up; the numeric literals before a later
    # error are the positive controls
    -regtest -dns=0 -proxy=localhost
    -regtest -dns=0 -onion=localhost
    -regtest -dns=0 -externalip=localhost
    -regtest -dns=0 -proxy=127.0.0.1:23456 -externalip=localhost
    -regtest -dns=0 -onion=127.0.0.1:23456 -externalip=localhost
    -regtest -dns=0 -externalip=1.2.3.4 -onlynet=i2p
    # ...and at step 12, after the chainstate loads: -onlynet=i2p without -i2psam
    -regtest -dns=0 -onlynet=i2p
    -regtest -dns=0 -onlynet=ipv4 -onlynet=i2p -bind=foo
    # Step 12: the binds, the onion service checks and -whitelist, after the chainstate loads;
    # warnings come before a later error (BL-327)
    -regtest -bind=foo
    -regtest -bind=127.0.0.1=tor
    -regtest -bind=0.0.0.0=onion
    -regtest -bind=127.0.0.1:22 -bind=foo
    -regtest -whitebind=127.0.0.1
    -regtest -whitebind=foo@127.0.0.1:1
    -regtest -whitebind=foo:1
    -regtest -whitelist=foo@1.2.3.4
    # ...-whitelist's in and out (the accepted forms are the positive controls before a later
    # error), a direction without a permission, and -whitebind's out
    -regtest -whitelist=in,noban@1.2.3.4 -whitelist=x
    -regtest -whitelist=out,noban@1.2.3.4 -whitelist=x
    -regtest -whitelist=in,out,noban@1.2.3.4 -whitelist=x
    -regtest -whitelist=in@1.2.3.4
    -regtest -whitelist=in,out@1.2.3.4
    -regtest -whitebind=out,noban@127.0.0.1:1
    -regtest -whitebind=in@127.0.0.1:1
    -regtest -cjdnsreachable -whitelist=fc00::/8
    -regtest -server=0 -port=22 -whitelist=1.2.3.4/33
    -regtest -bind=127.0.0.1:29993=onion -bind=127.0.0.1:29992=onion -whitelist=x
    # -maxsendbuffer, -maxreceivebuffer and -bantime are taken unchecked; rdashd refused them at
    # step 7 before (BL-327 D15)
    -regtest -maxsendbuffer=x -maxreceivebuffer=-1 -bantime=y -bind=foo
    # CConnman::Start: a bind to an address the host doesn't have (TEST-NET-1): bind's errno in
    # NetworkErrorString's message-and-code form (BL-327 D23)
    -regtest -bind=192.0.2.1:29989 -listenonion=0
    # CConnman::Start: the onion bind finds the RPC server on its port
    -regtest -rpcport=$PORT1 -port=$PORT2 -bind=127.0.0.1:$PORT1=onion
    # Step 3: logging (P2P-24) and -debuglogfile
    -regtest -debug=abc
    -regtest -debugexclude=abc
    -regtest -debug=abc -debugexclude=xyz
    -regtest -loglevel=abc
    -regtest -loglevel=net:abc
    -regtest -loglevel=net:info:abc
    -regtest -loglevel=net:trace:x
    -regtest -debuglogfile=foo/foo.log
    -regtest -debuglogfile=$TMP/missing/foo.log
    # Step 3: pruning, timeouts, fees, indexes
    -regtest -prune=-1
    -regtest -prune=100
    -testnet -prune=100 -txindex=0 -disablegovernance=1
    -regtest -peertimeout=0
    -regtest -peertimeout=-1
    -regtest -blockmintxfee=x
    -regtest -llmq-qvvec-sync=foo
    -regtest -llmq-qvvec-sync=llmq_test:5
    -regtest -llmq-qvvec-sync=llmq_foo:0
    # Step 3: masternode refusals (P2P-35) and governance (P2P-36). rdashd's -blocksonly is 1
    # until P5 (DEC-322, P5a D23), which a masternode refuses first: the other cases set dashd's 0.
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -txindex=0
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -peerbloomfilters=0
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -maxconnections=10
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -disablegovernance=1
    -regtest -masternodeblsprivkey=00 -blocksonly=1
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -peerblockfilters=0
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -maxuploadtarget=100
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -dustrelayfee=x
    -regtest -masternodeblsprivkey=00 -blocksonly=0 -prune=1000 -txindex=1
    -testnet -masternodeblsprivkey=00 -blocksonly=0 -listen=0
    -testnet -masternodeblsprivkey=00 -blocksonly=0 -minrelaytxfee=0.001
    -testnet -masternodeblsprivkey=00 -blocksonly=0 -bytespersigop=21
    -testnet -masternodeblsprivkey=00 -blocksonly=0 -datacarrier=0 -permitbaremultisig=0
    # rdashd's default is -disablegovernance=1 until GV1b (DEC-510, SP1 D9), so its governance
    # check needs dashd's default set explicitly. Revert with BL-423.
    -testnet -txindex=0 -disablegovernance=0
    # AppInitMain before the chainstate: mempool options, chain work, sporks
    -regtest -maxmempool=1
    -regtest -minrelaytxfee=x
    -regtest -incrementalrelayfee=x
    -regtest -dustrelayfee=x
    -regtest -minimumchainwork=xyz
    -regtest -maxsigcachesize=262144
    -regtest -maxsigcachesize=9223372036854775807
    -regtest -sporkaddr=x
    -regtest -minsporkkeys=0
    # Step 7: LoadGenesisBlock rejects a devnet genesis block from the future (time-too-new)
    -devnet=nd1 -listen=0 -server=0 -mocktime=1000000000
";

/// An unsupported-option refusal: the option and its owner.
type Refusal = (&'static str, &'static str);

/// Argument sets dashd v24 starts with and rdashd refuses as not supported yet, each with the
/// refusals (option, owner) it may give: `-reindex-chainstate` with an index, which dashd v24.0.0
/// no longer refuses (bitcoin#25193; the indexes wait for the reindex).
/// - `-coinstatsindex` and `-blockfilterindex`: their own IX1 refusal, before and after CH5-2a.
/// - `-txindex`: today `-reindex-chainstate`'s CH5 refusal, which comes first in the support
///   table; once CH5-2a serves `-reindex-chainstate`, `-txindex`'s IX1 refusal. Dropping the CH5
///   alternative is CH5-2a's follow-up.
///
/// Serving an index with `-reindex-chainstate` needs Core's index deferral first (CE7-02,
/// init.cpp:2275-2276): CE7-02 lands before IX1 and `-reindex-chainstate` are both served.
const ACCEPTED_BY_DASHD_UNSUPPORTED_BY_RDASHD: &[(&[&str], &[Refusal])] = &[
    (&["-reindex-chainstate", "-coinstatsindex"], &[("-coinstatsindex", "IX1")]),
    (&["-reindex-chainstate", "-blockfilterindex"], &[("-blockfilterindex", "IX1")]),
    (&["-reindex-chainstate", "-txindex"], &[("-reindex-chainstate", "CH5"), ("-txindex", "IX1")]),
];

/// How a run ended: its exit code (or `None` when it outlived the bound) and its stderr with
/// the run's paths replaced by `$DATADIR` and `$TMP`.
type Outcome = (Option<i32>, String);

/// `case` with `$PORT1` and `$PORT2` replaced by free ports, each picked once.
fn with_ports(case: &[&str]) -> Vec<String> {
    let mut ports = [None; 2];
    case.iter()
        .map(|arg| {
            let mut arg = arg.to_string();
            for (i, port) in ports.iter_mut().enumerate() {
                let name = format!("$PORT{}", i + 1);
                if arg.contains(&name) {
                    arg = arg.replace(&name, &port.get_or_insert_with(free_port).to_string());
                }
            }
            // A mistyped name would reach both binaries and fail them alike.
            assert!(!arg.contains("$PORT"), "unknown port name in {arg}");
            arg
        })
        .collect()
}

fn run_case(binary: &Path, case: &[&str]) -> Outcome {
    run_case_on(binary, case, |_| {})
}

/// [`run_case`] on a data directory `setup` prepared.
fn run_case_on(binary: &Path, case: &[&str], setup: impl Fn(&Path)) -> Outcome {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let datadir = tmp.path().join("dd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&datadir).unwrap();
    setup(&datadir);
    let tmp_text = tmp.path().display().to_string();
    let mut args = vec![datadir_arg(&datadir)];
    args.extend(case.iter().map(|a| a.replace("$TMP", &tmp_text)));
    // The run's own ports in case init gets that far, except where the case is about them. Each
    // on its own: a case that sets only `-port` still starts the RPC server, on the network's
    // fixed default port unless given one (BL-562).
    let devnet = case.iter().any(|a| a.contains("devnet"));
    if !devnet && !case.iter().any(|a| a.starts_with("-port") || a.starts_with("-noport")) {
        args.push(format!("-port={}", free_port()));
    }
    if !devnet && !case.iter().any(|a| a.contains("rpcport")) {
        args.push(format!("-rpcport={}", free_port()));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut child =
        command(binary, &home, &args).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    // Not `finish`: a run that outlives the bound is a result here (the case did not fail
    // init), not a test failure of its own.
    let deadline = Instant::now() + PATIENCE;
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = child.wait_with_output().unwrap();
    let text =
        stderr(&out).replace(&datadir.display().to_string(), "$DATADIR").replace(&tmp_text, "$TMP");
    (code, text)
}

#[test]
fn init_errors_match_dashd_v24() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let cases: Vec<Vec<&str>> = CASES
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect())
        .collect();
    // A few at a time: each case is two short-lived processes.
    let results: Vec<(String, Outcome, Outcome)> = std::thread::scope(|s| {
        let handles: Vec<_> = cases
            .chunks(cases.len().div_ceil(8))
            .map(|chunk| {
                let dashd = &dashd;
                s.spawn(move || {
                    chunk
                        .iter()
                        .map(|case| {
                            let args = with_ports(case);
                            let args: Vec<&str> = args.iter().map(String::as_str).collect();
                            let (code, text) = run_case(dashd, &args);
                            let core = (code, text.replace("Dash Core", "rdashd"));
                            (case.join(" "), core, run_case(Path::new(RDASHD), &args))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    // `-dns=0` cases whose name would resolve: Core's errors, pinned so a lookup in both
    // binaries cannot pass by agreeing.
    // Core: netbase.cpp:44-74 (AI_NUMERICHOST), init.cpp:1900-1916, :1947-1953, :1975-1980
    let externalip = "Cannot resolve -externalip address: 'localhost'";
    // The -whitelist and -whitebind direction cases: Core's errors, pinned so an older dashd,
    // which reads `in` and `out` as unknown permissions, cannot pass by agreeing.
    // Core: net_permissions.cpp:60, :78, :136
    let netmask = "Invalid netmask specified in -whitelist: 'x'";
    let only_direction = "Only direction was set, no permissions:";
    for (case, expected) in [
        ("-regtest -whitelist=in,noban@1.2.3.4 -whitelist=x", netmask),
        ("-regtest -whitelist=out,noban@1.2.3.4 -whitelist=x", netmask),
        ("-regtest -whitelist=in,out,noban@1.2.3.4 -whitelist=x", netmask),
        ("-regtest -whitelist=in@1.2.3.4", &format!("{only_direction} 'in@1.2.3.4'")),
        ("-regtest -whitelist=in,out@1.2.3.4", &format!("{only_direction} 'in,out@1.2.3.4'")),
        (
            "-regtest -whitebind=out,noban@127.0.0.1:1",
            "whitebind may only be used for incoming connections (\"out\" was passed)",
        ),
        ("-regtest -whitebind=in@127.0.0.1:1", &format!("{only_direction} 'in@127.0.0.1:1'")),
        ("-regtest -dns=0 -proxy=localhost", "Invalid -proxy address or hostname: 'localhost'"),
        ("-regtest -dns=0 -onion=localhost", "Invalid -onion address or hostname: 'localhost'"),
        ("-regtest -dns=0 -externalip=localhost", externalip),
        ("-regtest -dns=0 -proxy=127.0.0.1:23456 -externalip=localhost", externalip),
        ("-regtest -dns=0 -onion=127.0.0.1:23456 -externalip=localhost", externalip),
        (
            "-regtest -dns=0 -externalip=1.2.3.4 -onlynet=i2p",
            "Outbound connections restricted to i2p (-onlynet=i2p) but -i2psam is not provided",
        ),
    ] {
        let (_, core, ours) = results.iter().find(|r| r.0 == case).expect(case);
        assert!(core.1.contains(expected), "{case}: dashd {core:?}");
        assert!(ours.1.contains(expected), "{case}: rdashd {ours:?}");
    }
    let mut failures = Vec::new();
    for (case, core, ours) in &results {
        if core.0 != Some(1) {
            failures.push(format!("{case}: dashd did not fail init: {core:?}"));
        } else if core != ours {
            failures.push(format!("{case}:\n  dashd  {core:?}\n  rdashd {ours:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} differ:\n{}",
        failures.len(),
        results.len(),
        failures.join("\n")
    );
}

/// BL-722: an init error whose stderr is a pipe nobody reads still shuts down and exits 1, as
/// in dashd. Both ignore SIGPIPE by then, so each write fails with EPIPE; Core's `InitError`
/// writes to `std::cerr`, which only sets the stream's badbit. One case fails at step 2, before
/// the log file opens, and one at step 12, whose log ends with the shutdown's last line.
// Core: noui.cpp:22-49 (:45 std::cerr); init.cpp:1177-1178 (SIGPIPE ignored);
// bitcoind.cpp:182-186, :284-285
#[test]
fn an_init_error_into_a_closed_stderr_exits_1_as_dashd_does() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let ended = |binary: &Path, case: &[&str]| {
        let tmp = tempfile::tempdir().unwrap();
        let (home, datadir) = (tmp.path().join("home"), tmp.path().join("dd"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&datadir).unwrap();
        let mut args = vec![
            datadir_arg(&datadir),
            format!("-port={}", free_port()),
            format!("-rpcport={}", free_port()),
        ];
        args.extend(case.iter().map(|a| a.to_string()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let child = command(binary, &home, &args).stdout(Stdio::null()).stderr(writer).spawn();
        let status = finish(child.unwrap()).status;
        let log = std::fs::read_to_string(datadir.join("regtest/debug.log")).unwrap_or_default();
        // Its last line without the timestamp.
        let last = log.lines().last().map(|l| l.split_once(' ').map_or(l, |(_, m)| m).to_owned());
        (status.code(), last)
    };
    for (case, last) in [
        (&["-regtest", "-peerblockfilters=1"][..], None),
        (&["-regtest", "-bind=foo"], Some("Shutdown: done".to_owned())),
    ] {
        let core = ended(&dashd, case);
        assert_eq!(core, (Some(1), last), "{case:?}: dashd");
        assert_eq!(ended(Path::new(RDASHD), case), core, "{case:?}: rdashd");
    }
}

/// `-bind` without an `=onion` bind, with `-listenonion` on by default, starts on both binaries:
/// the default onion bind is added and is optional (dashpay/dash#8003).
// Core: init.cpp:2681-2685, net.cpp:3986-3994
#[test]
fn a_bind_without_an_onion_bind_starts() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    for binary in [dashd.as_path(), Path::new(RDASHD)] {
        let bind = format!("-bind=127.0.0.1:{}", free_port());
        start_and_stop(binary, &[&bind])
            .unwrap_or_else(|out| panic!("{}: init failed: {}", binary.display(), stderr(&out)));
    }
}

/// Starts `binary` on regtest with `args` and stops it once loaded; the run's output if init
/// fails.
fn start_and_stop(binary: &Path, args: &[&str]) -> Result<(), std::process::Output> {
    let (home, datadir) = home_and_datadir();
    let cmd = regtest_on(binary, home.path(), &datadir, &[args, &["-printtoconsole=0"]].concat());
    try_spawn_until_loaded(cmd, debug_log(&datadir)).map(|node| {
        node.stop_ok(libc::SIGTERM);
    })
}

/// [`ACCEPTED_BY_DASHD_UNSUPPORTED_BY_RDASHD`]: dashd v24 starts with each set; rdashd refuses it
/// with one of the set's refusals, and each index alone with its IX1 refusal.
// Core: init.cpp:2275-2276
#[test]
fn reindex_chainstate_with_an_index_is_dashd_s_start_and_rdashd_s_unsupported_option() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let unsupported = |option: &str, owner: &str| {
        format!("Error: {option} is not supported by rdashd yet ({owner}).\n")
    };
    let rdashd_refusal = |args: &[&str]| {
        let (home, datadir) = home_and_datadir();
        let out = output(regtest(home.path(), &datadir, args));
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        stderr(&out)
    };
    for &(args, refusals) in ACCEPTED_BY_DASHD_UNSUPPORTED_BY_RDASHD {
        let index = args[1];
        assert_eq!(rdashd_refusal(&[index]), unsupported(index, "IX1"));
        let ours = rdashd_refusal(args);
        let accepted: Vec<String> = refusals.iter().map(|&(o, w)| unsupported(o, w)).collect();
        assert!(
            accepted.contains(&ours),
            "{args:?}: rdashd {ours:?}, expected one of {accepted:?}"
        );
        start_and_stop(&dashd, args)
            .unwrap_or_else(|out| panic!("{args:?}: dashd failed init: {}", stderr(&out)));
    }
}

/// BL-726: before init step 1, a write to a closed pipe meets the SIGPIPE disposition the node
/// inherited, as in dashd. Under a parent with the default disposition the signal kills it;
/// under one that ignores SIGPIPE (systemd's default) the write fails and the node exits as it
/// would anyway. Core ignores SIGPIPE only in `AppInitBasicSetup`; Rust's runtime ignores it
/// before `main`, which rdashd undoes. The cases: a parameter error into a closed stderr,
/// `-version` into a closed stdout, and the recursive `-includeconf` warning into a closed stderr,
/// which the args sink writes (a panic there exited 101) before a stray token ends init.
// Core: init.cpp:1177-1178; bitcoind.cpp:111-133 (ParseArgs), :135-159 (ProcessInitCommands);
// util/system.cpp:989-999 (the warning, to std::cerr)
#[test]
fn a_closed_pipe_before_step_1_meets_the_inherited_sigpipe_disposition_as_in_dashd() {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};

    let Some(dashd) = find_dashd() else {
        return;
    };
    let recursive_include =
        [("dash.conf", "includeconf=a.conf\n"), ("a.conf", "includeconf=b.conf\n")];
    // `binary` with `args` and `conf` files in a new data directory (without one v24 fails
    // reading its configuration before `-version`). The temporary directory lives as long as
    // the command needs it.
    let node = |binary: &Path, args: &[&str], conf: &[(&str, &str)]| {
        let home = tempfile::tempdir().unwrap();
        let datadir = home.path().join("dd");
        std::fs::create_dir(&datadir).unwrap();
        for (name, text) in conf {
            std::fs::write(datadir.join(name), text).unwrap();
        }
        let datadir = datadir_arg(&datadir);
        let args = [&[datadir.as_str()][..], args].concat();
        let cmd = command(binary, home.path(), &args);
        (home, cmd)
    };
    let ended =
        |binary: &Path, args: &[&str], conf: &[(&str, &str)], closed: Closed, ignore: bool| {
            let (_home, mut cmd) = node(binary, args, conf);
            let (reader, writer) = std::io::pipe().unwrap();
            drop(reader);
            match closed {
                Closed::Stdout => cmd.stdout(writer).stderr(Stdio::null()),
                Closed::Stderr => cmd.stdout(Stdio::null()).stderr(writer),
            };
            if ignore {
                // SAFETY: `signal` is async-signal-safe, so it may run between fork and exec.
                unsafe {
                    cmd.pre_exec(|| {
                        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
                        Ok(())
                    });
                }
            }
            let status = finish(cmd.spawn().unwrap()).status;
            (status.code(), status.signal())
        };

    // The warning is the case's first write to stderr, in both.
    for binary in [dashd.as_path(), Path::new(RDASHD)] {
        let (_home, mut cmd) = node(binary, &["-regtest", "stray"], &recursive_include);
        let out = finish(cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap());
        assert_eq!(
            stderr(&out).lines().next(),
            Some(
                "warning: -includeconf cannot be used from included files; ignoring -includeconf=b.conf"
            ),
            "{}",
            binary.display()
        );
    }

    let killed = (None, Some(libc::SIGPIPE));
    for (args, conf, closed, ignored) in [
        (&["-regtest", "-nosuchopt=1"][..], &[][..], Closed::Stderr, (Some(1), None)),
        (&["-version"], &[], Closed::Stdout, (Some(0), None)),
        (&["-regtest", "stray"], &recursive_include, Closed::Stderr, (Some(1), None)),
    ] {
        for (ignore, want) in [(false, killed), (true, ignored)] {
            let core = ended(&dashd, args, conf, closed, ignore);
            assert_eq!(core, want, "{args:?}, SIGPIPE ignored by the parent: {ignore}: dashd");
            let ours = ended(Path::new(RDASHD), args, conf, closed, ignore);
            assert_eq!(ours, core, "{args:?}, SIGPIPE ignored by the parent: {ignore}: rdashd");
        }
    }
}

/// Which of the node's output pipes has no reader.
#[derive(Clone, Copy)]
enum Closed {
    Stdout,
    Stderr,
}

/// Core's `LoadChainstate` loads the block index and runs `LoadGenesisBlock` before it opens the
/// coins, so with a chainstate that cannot open dashd's error is a devnet genesis block from the
/// future's. Here the chainstate store holds the block index too and opens first, so its error is
/// the one shown (ND1 2b-D18's first ordering, recorded); the reindex question is dashd's.
// Core: node/chainstate.cpp:210-211, :225-230
#[test]
fn an_unopenable_chainstate_fails_before_the_block_index() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let unopenable = |datadir: &Path| {
        std::fs::create_dir(datadir.join("devnet-nd1")).unwrap();
        std::fs::write(datadir.join("devnet-nd1").join("chainstate"), "").unwrap();
    };
    let devnet = ["-devnet=nd1", "-listen=0", "-server=0"];
    let opening = ": Error opening block database.\n";
    for (case, error) in [
        (&[&devnet[..], &["-mocktime=1000000000"]].concat(), "Error initializing block database"),
        (&devnet.to_vec(), "Error opening block database"),
    ] {
        let core = run_case_on(&dashd, case, unopenable);
        assert_eq!(core.0, Some(1), "{case:?}: {core:?}");
        assert!(core.1.starts_with(&format!(": {error}.\n")), "{case:?}: {core:?}");
        let ours = run_case_on(Path::new(RDASHD), case, unopenable);
        assert_eq!(ours.0, Some(1), "{case:?}: {ours:?}");
        assert!(ours.1.starts_with(opening), "{case:?}: {ours:?}");
        assert_eq!(ours.1[opening.len()..], core.1[error.len() + 4..], "{case:?}: the question");
    }
}

/// BL-636 row 10 on dashd v24 (the expectations `dashd-node`'s NodeSut tests assert): coins
/// whose best block the block index lacks. The fixture is dashd's own: a regtest chain of three
/// blocks, whose `chainstate` and `evodb` directories replace those of a second one-block chain.
/// The start then fails init with "Error initializing block database" and the reindex question
/// on stderr, exit status 1, and both shutdown flushes each log the evodb timer pair (the coins'
/// best block is not null), with no `AbortNode`. Under `-debug=bench -debug=coindb` each flush
/// also logs its BENCH timer pairs and COINDB commit lines, in this order, the coins pair with
/// the empty cache's figures (0 coins, 262kB).
// Core: node/chainstate.cpp:256-257, :272-276; validation.cpp:3065-3110, :3089, :3107;
// txdb.cpp:170-172; init.cpp:378-386, :449-456, :2115-2133
#[test]
fn a_coins_tip_the_index_lacks_flushes_twice_on_dashd() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    // rpc_generate.py's regtest address of the generator point's key.
    let generate = |datadir: &Path, home: &Path, n: u32| {
        let node = RpcNode::start_on(&dashd, home, datadir, &["-disablewallet"]);
        let params = format!(r#"[{n}, "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6"]"#);
        let mined = node.call("generatetoaddress", &params);
        assert_eq!(mined.result().matches(',').count() + 1, n as usize, "{}", mined.body);
        assert_eq!(node.call("stop", "[]").result(), r#""Dash Core stopping""#);
        let out = node.node.wait();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    };
    let (long_home, long) = home_and_datadir();
    generate(&long, long_home.path(), 3);
    let (home, datadir) = home_and_datadir();
    generate(&datadir, home.path(), 1);
    for dir in ["chainstate", "evodb"] {
        let to = net(&datadir).join(dir);
        std::fs::remove_dir_all(&to).unwrap();
        copy_dir(&net(&long).join(dir), &to);
    }
    let before = read(&debug_log(&datadir)).len();
    let args =
        ["-disablewallet", "-printtoconsole=0", "-server=0", "-debug=bench", "-debug=coindb"];
    let out = output(regtest_on(&dashd, home.path(), &datadir, &args));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out),
        ": Error initializing block database.\nPlease restart with -reindex or \
         -reindex-chainstate to recover.\n"
    );
    let log = read(&debug_log(&datadir))[before..].to_owned();
    let started = "FlushStateToDisk: write evodb cache to disk started";
    let completed = "FlushStateToDisk: write evodb cache to disk completed (";
    assert_eq!(log.lines().filter(|l| l.ends_with(started)).count(), 2, "{log}");
    assert_eq!(log.lines().filter(|l| l.contains(completed)).count(), 2, "{log}");
    assert!(!log.contains("***"), "{log}");
    assert!(!log.contains("failed to flush state"), "{log}");
    // Each flush's lines, a timer's completed line up to its duration.
    let keys = ["[bench] ", "[coindb] ", "FlushStateToDisk: "];
    let flushed: Vec<&str> = log
        .lines()
        .filter_map(|l| keys.iter().filter_map(|k| l.find(k)).min().map(|at| &l[at..]))
        .map(|l| l.find(" completed (").map_or(l, |i| &l[..i + " completed (".len()]))
        .collect();
    let bench = "[bench] FlushStateToDisk: write";
    let flush = [
        format!("{bench} block and undo data to disk started"),
        format!("{bench} block and undo data to disk completed ("),
        format!("{bench} block index to disk started"),
        format!("{bench} block index to disk completed ("),
        format!("{bench} coins cache to disk (0 coins, 262kB) started"),
        "[coindb] Writing final batch of 0.00 MiB".to_owned(),
        "[coindb] Committed 0 changed transaction outputs (out of 0) to coin database..."
            .to_owned(),
        format!("{bench} coins cache to disk (0 coins, 262kB) completed ("),
        started.to_owned(),
        completed.to_owned(),
    ];
    assert_eq!(flushed, [&flush[..], &flush[..]].concat(), "{log}");
}

/// A copy of the directory `from` at `to`, its files only (a stopped dashd's database).
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file(), "{}", entry.path().display());
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

/// dashd v24 on regtest in `datadir`, logging to its file only, with no listening socket other
/// than its private RPC port.
fn dashd_regtest(dashd: &Path, home: &Path, datadir: &Path) -> std::process::Command {
    let rpcport = format!("-rpcport={}", free_port());
    let args = [&datadir_arg(datadir), "-regtest", "-listen=0", "-printtoconsole=0", &rpcport];
    command(dashd, home, &args)
}

/// [`dashd_regtest`] in rdashd's default mode until P5, `-blocksonly=1` (DEC-322, P5a D23),
/// whose `-maxmempool=5` soft-set sizes the unused mempool space in the UTXO cache line.
fn dashd_blocksonly_regtest(dashd: &Path, home: &Path, datadir: &Path) -> std::process::Command {
    let mut cmd = dashd_regtest(dashd, home, datadir);
    cmd.arg("-blocksonly=1");
    cmd
}

/// `-stopatheight=-1` stops at genesis (height 0 ≥ −1), but Core races here: ThreadImport wakes
/// init's genesis wait before it requests the shutdown, and init's check after that wait may or
/// may not see it. dashd v24 exits 1 with "Shutdown requested. Exiting." in most runs, and 0 after
/// a full start in the rest. rdashd has the same race (V's tip watch, then its `StartShutdown`).
/// On both binaries, either branch must be whole.
// Core: validation.cpp:3823-3830, init.cpp:2569-2580
#[test]
fn stopatheight_minus_one_stops_at_genesis() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    for core in [true, false] {
        let (home, datadir) = home_and_datadir();
        let mut cmd = if core {
            dashd_blocksonly_regtest(&dashd, home.path(), &datadir)
        } else {
            regtest(home.path(), &datadir, &["-printtoconsole=0"])
        };
        cmd.arg("-stopatheight=-1");
        let out = output(cmd);
        let log = read(&debug_log(&datadir));
        let name = if core {
            "dashd"
        } else {
            "rdashd"
        };
        eprintln!("{name}: exit {:?}", out.status.code());
        assert!(out.stderr.is_empty(), "{name}: {}", stderr(&out));
        let (done, exiting) = (
            log.contains("init message: Done loading"),
            log.contains("Shutdown requested. Exiting."),
        );
        match out.status.code() {
            Some(1) => assert!(exiting && !done, "{name}: {log}"),
            Some(0) => assert!(done && !exiting, "{name}: {log}"),
            code => panic!("{name}: exit {code:?}: {log}"),
        }
        // Genesis connected first, either way.
        let genesis = "UpdateTip: new best=000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e height=0 ";
        assert!(log.contains(genesis), "{name}: {log}");
        assert_shut_down_in_order(&log);
        assert!(!log.contains("***"), "{name}: {log}");
        assert!(!net(&datadir).join("dashd.pid").exists(), "{name}");
    }
}

/// `-checkblockindex` (BL-525) is a boolean dashd v24 takes in any form, a non-number (off, as
/// `InterpretBool` reads it) and a negative (on) included: both binaries start to "Done loading"
/// on each value, say nothing on stderr and stop cleanly. rdashd refused the option before.
// Core: init.cpp:801, node/chainstatemanager_args.cpp:22, util/system.cpp:103-108
#[test]
fn checkblockindex_values_start_as_on_dashd() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let values = [
        "-checkblockindex",
        "-checkblockindex=1",
        "-checkblockindex=0",
        "-nocheckblockindex",
        "-checkblockindex=foo",
        "-checkblockindex=-1",
    ];
    for value in values {
        for core in [true, false] {
            let (home, datadir) = home_and_datadir();
            let mut cmd = if core {
                dashd_blocksonly_regtest(&dashd, home.path(), &datadir)
            } else {
                regtest(home.path(), &datadir, &["-printtoconsole=0"])
            };
            cmd.arg(value);
            let out = spawn_until_loaded(cmd, debug_log(&datadir)).stop_ok(libc::SIGTERM);
            let name = if core {
                "dashd"
            } else {
                "rdashd"
            };
            assert!(out.stderr.is_empty(), "{name} {value}: {}", stderr(&out));
            let log = read(&debug_log(&datadir));
            assert_shut_down_in_order(&log);
            assert!(!log.contains("***"), "{name} {value}: {log}");
        }
    }
}

/// The lock is the same lock: dashd v24 cannot start on a data directory rdashd holds, and says
/// so as it does for another dashd.
#[test]
fn dashd_respects_rdashd_s_lock() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let (home, datadir) = home_and_datadir();
    let node = start(home.path(), &datadir, &[]);
    let out = output(dashd_regtest(&dashd, home.path(), &datadir));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!(
            "Error: Cannot obtain a lock on data directory {}. Dash Core is probably already \
             running.\n",
            net(&datadir).display()
        )
    );
    node.stop_ok(libc::SIGTERM);
}

/// A regtest data directory dashd v24 made and stopped: rdashd refuses it and changes nothing in
/// it (no lock, log or pid file).
#[test]
fn a_data_directory_dashd_made_is_refused_untouched() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let (home, datadir) = home_and_datadir();
    spawn_until_loaded(dashd_regtest(&dashd, home.path(), &datadir), debug_log(&datadir))
        .stop_ok(libc::SIGTERM);
    let before = tree(&datadir);

    let out = run_regtest(home.path(), &datadir, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), core_datadir_refusal(&net(&datadir)));
    assert_eq!(tree(&datadir), before);
}

/// The help of the options BL-273 implements, and of the cache options they share a budget with,
/// is dashd's: `-assumevalid` with each network's `defaultAssumeValid`, `-maxsigcachesize` (a
/// debug option), `-dbcache`/`-maxmempool`, and `-dbbatchsize` (a debug option).
// Core: init.cpp:598, :613, :614, :818
#[test]
fn the_coins_setup_options_help_is_dashd_s() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let help = |binary: &Path| {
        let (home, datadir) = home_and_datadir();
        let args = [datadir_arg(&datadir), "-help".to_owned(), "-help-debug".to_owned()];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = output(command(binary, home.path(), &args));
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
    };
    // An option's lines: its name, then its indented text up to the blank line.
    let block = |text: &str, option: &str| {
        let start = text.find(&format!("\n  {option}=")).expect(option);
        let rest = &text[start + 1..];
        rest[..rest.find("\n\n").expect("the block ends")].to_owned()
    };
    let (core, ours) = (help(&dashd), help(Path::new(RDASHD)));
    for option in ["-assumevalid", "-dbcache", "-maxmempool", "-maxsigcachesize", "-par"] {
        assert_eq!(block(&ours, option), block(&core, option));
    }
    // `-dbbatchsize` (ST3b S0) takes no `=<n>` in its help.
    let batch = |text: &str| {
        let start = text.find("\n  -dbbatchsize\n").expect("-dbbatchsize");
        let rest = &text[start + 1..];
        rest[..rest.find("\n\n").expect("the block ends")].to_owned()
    };
    assert_eq!(batch(&ours), batch(&core));
}

/// The log lines of the coins setup are dashd's for the same arguments: the validation caches
/// `-maxsigcachesize` sizes, the cache configuration with the mempool headroom, and the
/// `AssumedValidBlock()` line, with `-assumevalid` read by `uint256S` (spaces and `0x` skipped,
/// the digits up to the first other character, only the last 64 kept), and neither refuses the
/// options: both accept `-dbcache` and `-dbbatchsize`, with nothing on stderr.
// Core: init.cpp:1546-1552, :2023; node/chainstate.cpp:307-311; node/caches.cpp:17;
// uint256.cpp:20-44
#[test]
fn the_coins_setup_log_lines_are_dashd_s() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let markers = [
        " requested for signature cache",
        " requested for script execution cache",
        "* Using ",
        "Assuming ancestors of block ",
        "Validating signatures for all blocks.",
    ];
    let lines = |core: bool, case: &[&str]| {
        let (home, datadir) = home_and_datadir();
        let mut cmd = if core {
            dashd_blocksonly_regtest(&dashd, home.path(), &datadir)
        } else {
            regtest(home.path(), &datadir, &["-printtoconsole=0"])
        };
        // Stops at genesis (see `stopatheight_minus_one_stops_at_genesis`), either way.
        cmd.arg("-stopatheight=-1").args(case);
        let out = output(cmd);
        assert!(matches!(out.status.code(), Some(0 | 1)), "{case:?}: {}", stderr(&out));
        // Without the timestamp.
        let lines = read(&debug_log(&datadir))
            .lines()
            .filter(|l| markers.iter().any(|m| l.contains(m)))
            .map(|l| l.split_once(' ').map_or(l, |(_, text)| text).to_owned())
            .collect::<Vec<_>>();
        // rdashd logs these lines before it refuses an option it lacks or fails its init, so
        // stderr counts too: empty for both, either way the stop at genesis goes (see
        // `stopatheight_minus_one_stops_at_genesis`). Not the exit status, which is 0 or 1 by
        // timing, in dashd too.
        (stderr(&out), lines)
    };
    // `-dbcache` (ST3b S0) in MiB, clamped to 4..16,384 after the shift into bytes, which wraps:
    // 2^44 MiB is 0 bytes, so 4 MiB.
    let cases: [&[&str]; 12] = [
        &[],
        &["-assumevalid=0", "-maxsigcachesize=0"],
        &["-assumevalid=0x2a", "-maxsigcachesize=5"],
        &["-assumevalid= \t0X0000002aZZ12", "-maxsigcachesize=1"],
        &["-assumevalid=ab7d1de5eaf9b156d53208f033b5aa8122d2d2355d5e12292b121156cfdb4a529c"],
        &["-assumevalid=x12"],
        &["-dbcache=4"],
        &["-dbcache=0"],
        &["-dbcache=-1"],
        &["-dbcache=100000"],
        &["-dbcache=17592186044416"],
        &["-dbbatchsize=200000"],
    ];
    for case in cases {
        let (core, ours) = (lines(true, case), lines(false, case));
        assert!(core.1.iter().any(|l| l.contains("signatures")), "{case:?}: {core:?}");
        assert_eq!(ours, core, "{case:?}");
    }
}

/// `-dbbatchsize` (ST3b S0) reaches the node's own chainstate, RocksDB: block 1's coinbase pays
/// 300 `OP_TRUE` outputs, and the coins flush of them is written in partial batches at 1,000
/// bytes and in one at the default. The 300 coins (about 13 KB) are far above 1,000 bytes and far
/// below the default's sub-batch (at `-dbcache=4`, `coins/8`: about 196 KiB). rdashd-only: the
/// sub-batch is capped where Core's is not (ST3b-D1), so the counts are not dashd's.
// Core: txdb.cpp:117, :152-154
#[test]
fn dbbatchsize_splits_the_node_s_coins_flush() {
    // `coinbase(1, 0)` with its one output (value, `01 51`: 10 bytes before the 4-byte lock
    // time) 300 times, and the output count `01` as `fd 2c 01`.
    let one = coinbase(1, 0);
    let (inputs, output) = (&one[..one.len() - 15], &one[one.len() - 14..one.len() - 4]);
    let mut cb = inputs.to_vec();
    cb.extend([0xfd, 0x2c, 0x01]);
    (0..300).for_each(|_| cb.extend(output));
    cb.extend(0u32.to_le_bytes());
    let partial = |extra: &[&str]| {
        let (home, datadir) = home_and_datadir();
        let args = [&["-debug=coindb", "-dbcache=4"][..], extra].concat();
        let node = RpcNode::start(home.path(), &datadir, &args);
        let genesis = node.call("getblockhash", "[0]").result().trim_matches('"').to_owned();
        let mut prev = [0; 32];
        for (i, b) in prev.iter_mut().rev().enumerate() {
            *b = u8::from_str_radix(&genesis[2 * i..2 * i + 2], 16).unwrap();
        }
        let block = next_with(&prev, 1, cb.clone());
        assert_eq!(node.call("submitblock", &format!(r#"["{}"]"#, block.hex)).result(), "null");
        let stats = node.call("gettxoutsetinfo", "[]");
        assert!(stats.result().contains(r#""txouts":300,"#), "{}", stats.body);
        node.stop();
        read(&debug_log(&datadir)).matches("Writing partial batch").count()
    };
    assert_eq!(partial(&[]), 0);
    let split = partial(&["-dbbatchsize=1000"]);
    // About 13 KB in sub-batches of just over 1,000 bytes: about a dozen partial batches (12
    // seen, the final batch apart). Asked: at least 2, which no unsplit flush writes.
    assert!(split >= 2, "{split} partial batches");
}

/// The script-check threads (CH3a a9, DEC-323) are dashd's for each `-par`: its "Script
/// verification uses N additional threads" line, and once init is done N threads named
/// `d-scriptch.0` to `d-scriptch.<N-1>`. N counts the online processors (`GetNumCores()`), whatever
/// the affinity mask: one case pins both binaries to one CPU, the first this process may use, and
/// the last shows them four of the host's processors online (`fake_online`), where both start 3
/// threads; on a host that configures more than four, a count of the configured ones would not.
/// The cases cover the default and 0 (auto), serial and one thread, the cap at 15, `-par=-n`
/// leaving `n` cores free (down to 0 threads), and values `GetIntArg` saturates to `i64::MAX` or
/// that wrap when Core reads them into an `int`. The BLS check threads (BL-647) are dashd's too:
/// its "BLS verification uses M additional threads" line after the script line and the load's
/// first line ("Validating signatures for all blocks.") and before the block index load's
/// "Checking all blk files are present...", and M more threads with the same names
/// (`m_bls_queue` names its workers as the script queue does), for `-parbls` 1 (the default
/// here), 2 (the functional framework's), 0 (auto, on the host and on four online), negative
/// values, the clamp at 33 and the `i64` ends, alone and beside `-par`.
// Core: init.cpp:1554-1570, :2061-2071, checkqueue.h:139-154,
// util/threadnames.cpp:59-63, util/system.cpp:1211-1214, llmq/blockprocessor.cpp:89-90,
// node/chainstate.cpp:157-161, :183, node/blockstorage.cpp:381
#[cfg(target_os = "linux")]
#[test]
fn the_script_check_threads_are_dashd_s() {
    use std::os::unix::process::CommandExt as _;
    let Some(dashd) = find_dashd() else {
        return;
    };
    // The first CPU this process may use, the one `Cpus::One` pins both binaries to.
    // SAFETY: a zeroed `cpu_set_t` is empty, and `sched_getaffinity` fills it.
    let cpu = unsafe {
        let mut allowed: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut allowed), 0);
        (0..libc::CPU_SETSIZE as usize).find(|&cpu| libc::CPU_ISSET(cpu, &allowed)).unwrap()
    };
    let fake = tempfile::tempdir().unwrap();
    let online = fake.path().join("online");
    std::fs::write(&online, "0-3\n").unwrap();
    // The four-online case needs unprivileged user namespaces; it is skipped where they are
    // refused.
    let mut probe = std::process::Command::new("true");
    // SAFETY: `fake_online`'s hook makes only async-signal-safe calls.
    unsafe { probe.pre_exec(fake_online(&online)) };
    let namespaces = probe.status().is_ok_and(|s| s.success());
    if !namespaces {
        eprintln!("skipped the four-online case: no unprivileged user and mount namespace");
    }
    // The count a "… verification uses N additional threads" line gives, and a thread's index.
    let count = |line: &str| line.split(' ').nth(3).and_then(|n| n.parse::<usize>().ok());
    let index = |name: &String| name.rsplit('.').next().and_then(|n| n.parse::<u32>().ok());
    let census = |binary: &Path, case: &[&str], cpus: Cpus| {
        let (home, datadir) = home_and_datadir();
        let node = start_on_free_port(&datadir, free_port, |port| {
            let rpcport = format!("-rpcport={port}");
            let parbls: &[&str] = match case.iter().any(|a| a.contains("parbls")) {
                true => &[],
                false => &["-parbls=1"],
            };
            let args = [&["-printtoconsole=0", rpcport.as_str()], parbls, case].concat();
            let mut cmd = regtest_on(binary, home.path(), &datadir, &args);
            match cpus {
                Cpus::Host => {}
                // SAFETY: only async-signal-safe calls between fork and exec.
                Cpus::One => unsafe {
                    cmd.pre_exec(move || {
                        let mut set: libc::cpu_set_t = std::mem::zeroed();
                        libc::CPU_SET(cpu, &mut set);
                        match libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) {
                            0 => Ok(()),
                            _ => Err(std::io::Error::last_os_error()),
                        }
                    });
                },
                // SAFETY: as `namespaces`'s probe.
                Cpus::FourOnline => unsafe {
                    cmd.pre_exec(fake_online(&online));
                },
            }
            cmd
        })
        .0;
        let lines: Vec<String> = read(&node.log)
            .split_inclusive('\n')
            .filter(|l| {
                l.contains("Script verification uses ")
                    || l.contains("Validating signatures for all blocks.")
                    || l.contains("BLS verification uses ")
                    || l.contains("Checking all blk files are present...")
            })
            .map(|l| l.split_once(' ').map_or(l, |(_, text)| text).to_owned())
            .collect();
        let logged = (lines.iter().filter(|l| l.contains("verification uses ")))
            .map(|l| count(l))
            .sum::<Option<usize>>();
        // A worker takes its name when it first runs (std's spawn, as `ThreadRename` in Core's
        // worker), so under load the census waits, at most 10 s, for the logged count.
        let deadline = Instant::now() + Duration::from_secs(10);
        let threads = loop {
            let mut threads: Vec<String> = std::fs::read_dir(format!("/proc/{}/task", node.id()))
                .unwrap()
                .map(|task| read(&task.unwrap().path().join("comm")).trim_end().to_owned())
                .filter(|name| name.contains("scriptch"))
                .collect();
            if logged == Some(threads.len()) || Instant::now() > deadline {
                threads.sort_by_key(index);
                break threads;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        node.stop_ok(libc::SIGTERM);
        (lines, threads)
    };
    // Each case, its CPUs, and the script and BLS threads whatever the host.
    type Case = (&'static [&'static str], Cpus, Option<usize>, Option<usize>);
    let cases: [Case; 23] = [
        (&[], Cpus::Host, None, Some(0)),
        (&["-par=0"], Cpus::Host, None, Some(0)),
        (&["-par=1"], Cpus::Host, Some(0), Some(0)),
        (&["-par=2"], Cpus::Host, Some(1), Some(0)),
        (&["-par=16"], Cpus::Host, Some(15), Some(0)),
        (&["-par=-1"], Cpus::Host, None, Some(0)),
        (&["-par=-40"], Cpus::Host, None, Some(0)),
        (&["-par=-1000"], Cpus::Host, Some(0), Some(0)),
        // 2^32 + 2 is -par=2 in an int.
        (&["-par=4294967298"], Cpus::Host, Some(1), Some(0)),
        // Past i64::MAX: GetIntArg saturates, and the int reads -1.
        (&["-par=99999999999999999999"], Cpus::Host, None, Some(0)),
        (&[], Cpus::One, None, Some(0)),
        // Four online of the host's configured CPUs: the count is the online ones'.
        (&[], Cpus::FourOnline, Some(3), Some(0)),
        // `-parbls`, with the script threads serial unless set.
        (&["-par=1", "-parbls=2"], Cpus::Host, Some(0), Some(1)),
        (&["-par=1", "-parbls=0"], Cpus::Host, Some(0), None),
        (&["-par=1", "-noparbls"], Cpus::Host, Some(0), None),
        (&["-par=1", "-parbls=-2"], Cpus::Host, Some(0), None),
        (&["-par=1", "-parbls=-1000"], Cpus::Host, Some(0), Some(0)),
        // The clamp: at most 33 threads.
        (&["-par=1", "-parbls=40"], Cpus::Host, Some(0), Some(33)),
        (&["-par=1", "-parbls=9223372036854775807"], Cpus::Host, Some(0), Some(33)),
        (&["-par=1", "-parbls=-9223372036854775808"], Cpus::Host, Some(0), Some(0)),
        (&["-par=3", "-parbls=4"], Cpus::Host, Some(2), Some(3)),
        (&["-parbls=0"], Cpus::One, None, None),
        (&["-par=0", "-parbls=0"], Cpus::FourOnline, Some(3), Some(3)),
    ];
    for (case, cpus, script, bls) in cases {
        if cpus == Cpus::FourOnline && !namespaces {
            continue;
        }
        let core = census(&dashd, case, cpus);
        let [s, b] = [0, 2].map(|i| core.0.get(i).and_then(|l| count(l)).unwrap_or(usize::MAX));
        assert!(script.is_none_or(|t| t == s), "{case:?}: dashd runs {s} script threads");
        assert!(bls.is_none_or(|t| t == b), "{case:?}: dashd runs {b} BLS threads");
        assert_eq!(
            core.0,
            [
                format!("Script verification uses {s} additional threads\n"),
                "Validating signatures for all blocks.\n".to_owned(),
                format!("BLS verification uses {b} additional threads\n"),
                "Checking all blk files are present...\n".to_owned(),
            ],
            "{case:?}"
        );
        let mut named: Vec<String> =
            (0..s).chain(0..b).map(|i| format!("d-scriptch.{i}")).collect();
        named.sort_by_key(index);
        assert_eq!(core.1, named, "{case:?}: dashd's threads");
        let ours = census(Path::new(RDASHD), case, cpus);
        assert_eq!(ours, core, "{case:?} on {cpus:?}");
    }
}

/// Where the census runs a node: on the host's CPUs, pinned to one of them, or seeing four of them
/// online (`fake_online`).
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Cpus {
    Host,
    One,
    FourOnline,
}

/// A `pre_exec` hook that moves the child into a new user and mount namespace, its uid and gid
/// mapped to root there (as `unshare -rm` does), and bind-mounts `fake` over
/// `/sys/devices/system/cpu/online`. glibc's `get_nprocs()` then reads `fake`, while its
/// `get_nprocs_conf()` (`_SC_NPROCESSORS_CONF`) stays the host's.
#[cfg(target_os = "linux")]
fn fake_online(fake: &Path) -> impl FnMut() -> std::io::Result<()> + Send + Sync + 'static {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: neither call can fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let maps = [
        (c"/proc/self/setgroups", b"deny".to_vec()),
        (c"/proc/self/uid_map", format!("0 {uid} 1").into_bytes()),
        (c"/proc/self/gid_map", format!("0 {gid} 1").into_bytes()),
    ];
    let source = std::ffi::CString::new(fake.as_os_str().as_bytes()).unwrap();
    move || {
        let check = |r: libc::c_int| match r {
            0.. => Ok(r),
            _ => Err(std::io::Error::last_os_error()),
        };
        let null = std::ptr::null::<libc::c_char>();
        // SAFETY: only async-signal-safe calls, on buffers made before the fork.
        unsafe {
            check(libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS))?;
            for (path, bytes) in &maps {
                let fd = check(libc::open(path.as_ptr(), libc::O_WRONLY))?;
                let n = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                if n < 0 {
                    return Err(err);
                }
            }
            let private = libc::MS_REC | libc::MS_PRIVATE;
            check(libc::mount(null, c"/".as_ptr(), null, private, std::ptr::null()))?;
            let online = c"/sys/devices/system/cpu/online";
            check(libc::mount(
                source.as_ptr(),
                online.as_ptr(),
                null,
                libc::MS_BIND,
                std::ptr::null(),
            ))?;
        }
        Ok(())
    }
}
