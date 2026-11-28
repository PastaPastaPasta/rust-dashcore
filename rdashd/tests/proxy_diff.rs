//! DIFF-P2P for SOCKS5 (P8a): dashd v24.0.3 and rdashd dial the same targets through the
//! same local recording proxy, and every byte each sends the proxy must be equal. The cases are
//! feature_proxy.py's: `-proxy` with and without `-proxyrandomize`, `-onion`, `-cjdnsreachable`
//! and a `unix:` proxy, each dialling an IPv4, an IPv6, a Tor, a CJDNS and a named target with
//! `addnode … onetry`. The proxy answers every request "connection refused" (`REP` 5), so no
//! peer connects, and the nodes' logs must hold the same `Socks5()` failure lines.
//!
//! The same for I2P's SAM 3.1 proxy (P8c-1): with `-i2psam` and `-i2pacceptincoming=0`, both
//! daemons dial I2P targets through transient sessions on one scripted local SAM mock, which
//! records every request line; the peer is never reached (`CANT_REACH_PEER`), and the logs must
//! hold the same `[i2p]` lines.
//!
//! The proxies listen on loopback below the ephemeral range; nothing leaves the host. Unlike the
//! other dashd tests this one cannot be skipped (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
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
    let mut extra = vec!["-listen=0", "-disablewallet"];
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

// Core: test/functional/p2p_i2p_ports.py:23, :28
const I2P_PEER: &str = "zsxwyo6qcn3chqzwxnseusqgsnuw3maqnztkiypyfxtya4snkoka.b32.i2p";
const I2P_OTHER: &str = "h3r6bkn46qxftwja53pxiykntegfyfjqtnzbm6iv6r5mungmqgmq.b32.i2p";

/// Every SAM connection's request lines, in accept order.
type SamLines = Arc<Mutex<Vec<Vec<String>>>>;

/// A transient private key for `SESSION CREATE`'s reply, in I2P's Base64: a 387-byte
/// destination with no certificate, then key bytes.
fn sam_destination() -> String {
    let key: Vec<u8> = (0..387u32).map(|i| (i * 13 % 251) as u8).chain([0; 64]).collect();
    let mut key = key;
    key[385..387].copy_from_slice(&[0, 0]);
    base64(&key).replace('+', "-").replace('/', "~")
}

/// A SAM 3.1 server for one conversation: greets, creates any session with
/// [`sam_destination`], resolves any name, and answers every `STREAM CONNECT` with
/// `CANT_REACH_PEER`. Each request line is recorded before its reply goes out.
fn serve_sam(s: TcpStream, lines: SamLines, index: usize) {
    let mut reader = BufReader::new(s.try_clone().unwrap());
    let mut s = s;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        lines.lock().unwrap()[index].push(line.clone());
        let reply = match line.split(' ').take(2).collect::<Vec<_>>()[..] {
            ["HELLO", ..] => "HELLO REPLY RESULT=OK VERSION=3.1".to_owned(),
            ["SESSION", "CREATE"] => {
                format!("SESSION STATUS RESULT=OK DESTINATION={}", sam_destination())
            }
            ["NAMING", "LOOKUP"] => "NAMING REPLY RESULT=OK VALUE=peerdest~-AAAA".to_owned(),
            ["STREAM", "CONNECT"] => "STREAM STATUS RESULT=CANT_REACH_PEER".to_owned(),
            _ => return,
        };
        if s.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return;
        }
    }
}

fn sam_proxy() -> (SamLines, String) {
    let listener = TcpListener::bind(("127.0.0.1", free_port())).unwrap();
    let at = listener.local_addr().unwrap().to_string();
    let lines = SamLines::default();
    let recorded = Arc::clone(&lines);
    std::thread::spawn(move || {
        while let Ok((s, _)) = listener.accept() {
            let index = {
                let mut lines = recorded.lock().unwrap();
                lines.push(Vec::new());
                lines.len() - 1
            };
            let recorded = Arc::clone(&recorded);
            std::thread::spawn(move || serve_sam(s, recorded, index));
        }
    });
    (lines, at)
}

/// `text` with each ten-hex-digit session id after `prefix` replaced by `<id>`.
fn mask_session_ids(text: &str, prefix: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        let (head, tail) = rest.split_at(at + prefix.len());
        out.push_str(head);
        let id = tail.get(..10).filter(|id| id.bytes().all(|b| b.is_ascii_hexdigit()));
        rest = if id.is_some() {
            out.push_str("<id>");
            &tail[10..]
        } else {
            tail
        };
    }
    out.push_str(rest);
    out
}

/// The log's `[i2p` lines and Core's `connect() to` lines, from their prefix on, with the session
/// ids masked.
fn i2p_lines(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|l| l.find("[i2p").or_else(|| l.find("connect() to ")).map(|at| &l[at..]))
        .map(|l| mask_session_ids(&mask_session_ids(l, "SAM session "), "ID="))
        .collect()
}

/// Starts `binary` with `-i2psam=<sam>` and `-i2pacceptincoming=0`, dials `targets` one by one
/// with `addnode … onetry`, waiting for each one's `Error connecting to` line, and returns the
/// log as it stood then (before the shutdown destroys the pooled session).
fn dial_i2p(binary: &Path, home: &Path, name: &str, sam: &str, targets: &[&str]) -> String {
    let datadir = home.join(name);
    std::fs::create_dir_all(&datadir).unwrap();
    let i2psam = format!("-i2psam={sam}");
    let mut args =
        vec!["-listen=0", "-dnsseed=0", "-debug=i2p", "-i2pacceptincoming=0", i2psam.as_str()];
    if binary != Path::new(RDASHD) {
        args.push("-disablewallet");
    }
    let node = RpcNode::start_on(binary, home, &datadir, &args);
    let log = || read(&debug_log(&datadir));
    for (i, target) in targets.iter().enumerate() {
        let reply = node.call("addnode", &format!(r#"["{target}","onetry",false]"#));
        assert_eq!(reply.result(), "null", "{name} {target}: {}", reply.body);
        wait_until(&format!("{name}: {target} fails"), || {
            log().matches("Error connecting to ").count() == i + 1
        });
    }
    let seen = log();
    node.node.stop_ok(libc::SIGTERM);
    seen
}

/// DIFF-P2P for SAM 3.1 transient sessions: dashd v24.0.2 and rdashd send the same request
/// bytes for p2p_i2p_sessions.py's `-i2pacceptincoming=0` node, and log the same `[i2p]` lines.
/// The first target (port 0) creates a session (a control connection with `HELLO` and
/// `SESSION CREATE`, then a stream connection with `HELLO`, `NAMING LOOKUP` and
/// `STREAM CONNECT`); the peer cannot be reached, so the session goes back to the pool. The
/// second (p2p_i2p_ports.py's port 8333) is refused before any I/O; the third takes the pooled
/// session, so it creates none.
// Core: i2p.cpp:219-276, :326-337, :405-456; net.cpp:437-466
#[test]
fn sam_bytes_match_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let arbitrary = format!("{I2P_PEER}:8333");
    let targets = [I2P_PEER, arbitrary.as_str(), I2P_OTHER];
    let mut transcripts = Vec::new();
    let mut logs = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let (sam, at) = sam_proxy();
        let log = dial_i2p(binary, home.path(), &format!("sam-{who}"), &at, &targets);
        let lines = sam.lock().unwrap().clone();
        transcripts.push(
            lines
                .iter()
                .map(|c| c.iter().map(|l| mask_session_ids(l, "ID=")).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
        );
        logs.push(i2p_lines(&log).into_iter().map(|l| l.replace(&at, "<sam>")).collect::<Vec<_>>());
        // One session: its id is the same on every connection.
        let ids: BTreeSet<_> = lines
            .iter()
            .flatten()
            .filter_map(|l| l.split(' ').find_map(|w| w.strip_prefix("ID=")))
            .collect();
        assert_eq!(ids.len(), 1, "{who}: {lines:?}");
    }
    let hello = "HELLO VERSION MIN=3.1 MAX=3.1\n";
    let stream = |peer: &str| {
        vec![
            hello.to_owned(),
            format!("NAMING LOOKUP NAME={peer}\n"),
            "STREAM CONNECT ID=<id> DESTINATION=peerdest~-AAAA SILENT=false\n".to_owned(),
        ]
    };
    let control = vec![
        hello.to_owned(),
        "SESSION CREATE STYLE=STREAM ID=<id> DESTINATION=TRANSIENT SIGNATURE_TYPE=7 \
         i2cp.leaseSetEncType=4,0 inbound.quantity=1 outbound.quantity=1\n"
            .to_owned(),
    ];
    assert_eq!(transcripts[0], [control, stream(I2P_PEER), stream(I2P_OTHER)]);
    assert_eq!(transcripts[0], transcripts[1], "dashd, then rdashd");
    for expected in [
        "[i2p] Creating transient SAM session <id> with <sam>".to_owned(),
        format!(
            "[i2p] Error connecting to {arbitrary}, connection refused due to arbitrary port 8333"
        ),
        format!(
            "[i2p] Error connecting to {I2P_OTHER}:0: \"STREAM STATUS RESULT=CANT_REACH_PEER\""
        ),
    ] {
        assert!(logs[0].contains(&expected), "dashd: {expected:?} in {:?}", logs[0]);
    }
    assert!(logs[1].iter().any(|l| l.contains("Creating transient SAM session")));
    assert_eq!(logs[0], logs[1], "dashd, then rdashd");
}

/// p2p_i2p_ports.py's second case: nothing listens on the SAM port, so the session's greeting
/// fails with Core's `Cannot connect to <proxy>`, after the proxy connect's own log line. The
/// test holds the port, bound without `SO_REUSEADDR` and not listening, so nothing else can
/// take it while either binary dials it.
// Core: i2p.cpp:326-332; netbase.cpp:564-572, :652-656
#[test]
fn an_unreachable_sam_proxy_logs_as_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let target = format!("{I2P_OTHER}:0");
    let held = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    held.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into()).unwrap();
    let sam = held.local_addr().unwrap().as_socket().unwrap().to_string();
    let mut logs = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let log = dial_i2p(binary, home.path(), &format!("nosam-{who}"), &sam, &[&target]);
        let expected = format!("Error connecting to {target}: Cannot connect to {sam}");
        assert!(log.contains(&expected), "{who}: {expected:?} in {log}");
        logs.push(
            i2p_lines(&log).into_iter().map(|l| l.replace(&sam, "<sam>")).collect::<Vec<_>>(),
        );
    }
    assert_eq!(logs[0], logs[1], "dashd, then rdashd");
    drop(held);
}
