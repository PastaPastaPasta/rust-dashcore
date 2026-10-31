//! DIFF-P2P for SOCKS5 (P8a): dashd v24.0.0-rc.4 and rdashd dial the same targets through the
//! same local recording proxy, and every byte each sends the proxy must be equal. The cases are
//! feature_proxy.py's: `-proxy` with and without `-proxyrandomize`, `-onion`, `-cjdnsreachable`
//! and a `unix:` proxy, each dialling an IPv4, an IPv6, a Tor, a CJDNS and a named target with
//! `addnode … onetry`. The proxy answers every request "connection refused" (`REP` 5), so no
//! peer connects, and the nodes' logs must hold the same `Socks5()` failure lines.
//!
//! The proxies listen on loopback below the ephemeral range; nothing leaves the host. Unlike the
//! other dashd tests this one cannot be skipped (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;

// Core: test/functional/feature_proxy.py:158-218
const TORV3: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
const TARGETS: [&str; 4] = [
    "15.61.23.23:1234",
    "[1233:3432:2434:2343:3234:2345:6546:4534]:5443",
    "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333",
    "node.noumenon:8333",
];
const CJDNS_TARGET: &str = "[fc00:1:2:3:4:5:6:7]:8888";

/// Every connection's bytes from the client, in arrival order.
type Transcripts = Arc<Mutex<Vec<Vec<u8>>>>;

/// Reads exactly `n` bytes into `log`.
fn take(s: &mut impl Read, n: usize, log: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut buf = vec![0; n];
    s.read_exact(&mut buf).ok()?;
    log.extend_from_slice(&buf);
    Some(buf)
}

/// One SOCKS5 server conversation: user/password when offered, and `REP` 5 to the request.
/// Returns everything the client sent, recorded before the final reply goes out.
fn serve(mut s: impl Read + Write) -> Option<Vec<u8>> {
    let mut log = Vec::new();
    let head = take(&mut s, 2, &mut log)?;
    let methods = take(&mut s, usize::from(head[1]), &mut log)?;
    if methods.contains(&2) {
        s.write_all(&[5, 2]).ok()?;
        let v = take(&mut s, 2, &mut log)?;
        take(&mut s, usize::from(v[1]), &mut log)?;
        let n = take(&mut s, 1, &mut log)?;
        take(&mut s, usize::from(n[0]), &mut log)?;
        s.write_all(&[1, 0]).ok()?;
    } else {
        s.write_all(&[5, 0]).ok()?;
    }
    let req = take(&mut s, 4, &mut log)?;
    let len = match req[3] {
        3 => usize::from(take(&mut s, 1, &mut log)?[0]),
        1 => 4,
        _ => 16,
    };
    take(&mut s, len + 2, &mut log)?;
    Some(log)
}

/// Serves each accepted connection on its own thread, recording its transcript.
fn record<S: Read + Write + Send + 'static>(
    accept: impl Fn() -> Option<S> + Send + 'static,
) -> Transcripts {
    let transcripts = Transcripts::default();
    let recorded = Arc::clone(&transcripts);
    std::thread::spawn(move || {
        while let Some(mut s) = accept() {
            let recorded = Arc::clone(&recorded);
            std::thread::spawn(move || {
                if let Some(log) = serve(&mut s) {
                    recorded.lock().unwrap().push(log);
                    let _ = s.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
                }
            });
        }
    });
    transcripts
}

fn tcp_proxy() -> (Transcripts, String) {
    let listener = TcpListener::bind(("127.0.0.1", free_port())).unwrap();
    let at = listener.local_addr().unwrap().to_string();
    (record(move || listener.accept().ok().map(|(s, _)| s)), at)
}

fn unix_proxy(path: &Path) -> (Transcripts, String) {
    let listener = UnixListener::bind(path).unwrap();
    let at = format!("unix:{}", path.display());
    (record(move || listener.accept().ok().map(|(s, _)| s)), at)
}

/// Starts `binary` with `args`, dials `targets` one by one with `addnode … onetry` (V1, as
/// feature_proxy does), waiting for each to reach a proxy, then stops it. Returns its log.
fn dial_all(
    binary: &Path,
    home: &Path,
    name: &str,
    args: &[String],
    targets: &[&str],
    proxies: &[&Transcripts],
) -> String {
    let datadir = home.join(name);
    std::fs::create_dir_all(&datadir).unwrap();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut extra = vec!["-listen=0", "-disablewallet", "-blocksonly=1"];
    extra.retain(|a| binary != Path::new(RDASHD) || *a == "-listen=0");
    extra.extend(args);
    let node = RpcNode::start_on(binary, home, &datadir, &extra);
    let seen = || proxies.iter().map(|p| p.lock().unwrap().len()).sum::<usize>();
    for (i, target) in targets.iter().enumerate() {
        let reply = node.call("addnode", &format!(r#"["{target}","onetry",false]"#));
        assert_eq!(reply.result(), "null", "{name} {target}: {}", reply.body);
        wait_until(&format!("{name}: {target} reaches a proxy"), || seen() == i + 1);
    }
    // The last refusal's log line follows the transcript; with no targets, the DNS seed's
    // connection has time to start.
    std::thread::sleep(Duration::from_millis(if targets.is_empty() {
        3000
    } else {
        500
    }));
    node.node.stop_ok(libc::SIGTERM);
    read(&debug_log(&datadir))
}

/// The log's `Socks5()` lines, without their timestamps.
fn socks5_lines(log: &str) -> Vec<String> {
    log.lines().filter_map(|l| l.find("Socks5() ").map(|at| l[at..].to_owned())).collect()
}

#[test]
fn socks5_bytes_match_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    // `-dnsseed=0`: with a name proxy, regtest's DNS seed would be an ADDR_FETCH through it
    // too, racing the targets ("seed" checks that one alone).
    let cases: [(&str, &[&str], bool); 5] = [
        ("proxy", &["-dnsseed=0", "-proxyrandomize=0"], false),
        ("proxyrandomize", &["-dnsseed=0"], false),
        ("onion", &["-dnsseed=0", "-proxyrandomize=0", "-onion=$ONION"], false),
        ("cjdns", &["-dnsseed=0", "-cjdnsreachable"], true),
        ("unix", &["-dnsseed=0"], false),
    ];
    let mut failures = Vec::new();
    for (name, extra, cjdns) in cases {
        let mut transcripts = Vec::new();
        let mut logs = Vec::new();
        for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
            let (main, main_at) = if name == "unix" {
                unix_proxy(&home.path().join(format!("{who}.sock")))
            } else {
                tcp_proxy()
            };
            let (onion, onion_at) = tcp_proxy();
            let mut args = vec![format!("-proxy={main_at}")];
            args.extend(extra.iter().map(|a| a.replace("$ONION", &onion_at)));
            let mut targets = TARGETS.to_vec();
            if cjdns {
                targets.insert(3, CJDNS_TARGET);
            }
            let log = dial_all(
                binary,
                home.path(),
                &format!("{name}-{who}"),
                &args,
                &targets,
                &[&main, &onion],
            );
            let main = main.lock().unwrap().clone();
            let onion = onion.lock().unwrap().clone();
            transcripts.push((main, onion));
            logs.push(socks5_lines(&log));
        }
        if transcripts[0] != transcripts[1] {
            failures.push(format!(
                "{name} transcripts:\n  dashd  {:?}\n  rdashd {:?}",
                transcripts[0], transcripts[1]
            ));
        }
        if logs[0] != logs[1] {
            failures.push(format!("{name} logs:\n  dashd  {:?}\n  rdashd {:?}", logs[0], logs[1]));
        }
        // Each target reached a proxy, and Core logged each refusal.
        let (main, onion) = &transcripts[0];
        let expected = TARGETS.len() + usize::from(cjdns);
        assert_eq!(main.len() + onion.len(), expected, "{name}");
        assert_eq!(logs[0].len(), expected, "{name}: {:?}", logs[0]);
        if name == "onion" {
            assert_eq!(onion.len(), 1, "{name}");
            assert!(onion[0].ends_with(&[TORV3.as_bytes(), &8333u16.to_be_bytes()].concat()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// With a name proxy, the DNS seed is not looked up: it becomes an ADDR_FETCH destination, which
/// the proxy is asked to connect to by name on the chain's port (regtest's `dummySeed.invalid.`).
// Core: net.cpp:2758-2762
#[test]
fn the_dns_seed_goes_through_the_name_proxy() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let mut transcripts = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let (proxy, at) = tcp_proxy();
        let args = [format!("-proxy={at}"), "-proxyrandomize=0".into()];
        dial_all(binary, home.path(), &format!("seed-{who}"), &args, &[], &[&proxy]);
        wait_until(&format!("{who}: the seed reaches the proxy"), || {
            !proxy.lock().unwrap().is_empty()
        });
        transcripts.push(proxy.lock().unwrap().clone());
    }
    let seed = b"dummySeed.invalid.";
    let port = 19899u16.to_be_bytes();
    let request = [&[5, 1, 0, 5, 1, 0, 3, seed.len() as u8][..], seed, &port].concat();
    assert_eq!(transcripts[0], [request]);
    assert_eq!(transcripts[0], transcripts[1]);
}

/// feature_config_args.py's `-dnsseed` with `-connect` and `-proxy`: an explicit `-dnsseed=1` is
/// reported ignored, and a `-dnsseed` that `-connect` soft-disabled is not. dashd and rdashd
/// must agree; `-connect`'s name goes to the local mock as the name proxy.
// Core: init.cpp:2756-2758; test/functional/feature_config_args.py:302-311
#[test]
fn dnsseed_ignored_with_connect_and_proxy() {
    const IGNORED: &str = "-dnsseed is ignored when -connect is used and -proxy is specified\n";
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    for (case, dnsseed, logged) in [("explicit", Some("-dnsseed=1"), true), ("soft", None, false)] {
        for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
            let (proxy, at) = tcp_proxy();
            let mut args = vec!["-connect=fakeaddress1".to_owned(), format!("-proxy={at}")];
            args.extend(dnsseed.map(str::to_owned));
            let log =
                dial_all(binary, home.path(), &format!("{case}-{who}"), &args, &[], &[&proxy]);
            assert_eq!(log.contains(IGNORED), logged, "{case} {who}");
        }
    }
}
