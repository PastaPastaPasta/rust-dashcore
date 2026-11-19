//! DIFF for Tor control (P8b): dashd v24.0.0 and rdashd talk to the same local mock Tor
//! control port, and every command each sends must be equal, with equal `tor` log lines, equal
//! onion key files and the same onion network (reachable, proxy) and local addresses in
//! `getnetworkinfo`. The cases are each authentication method (NULL, HASHEDPASSWORD with a quote
//! in the password, SAFECOOKIE, and none usable), `-onion` set (no `GETINFO`), `-onion=` empty
//! (`GETINFO` still sent), `-noonion`, `-onlynet=ipv4`, `-onlynet=onion` and the deprecated
//! `-onlynet=tor`, `-proxy` with `-listen=1` (Tor's SOCKS port replaces the proxy), a restart
//! with the cached key, and a control port that closes after `ADD_ONION` (the reconnection after
//! one second). Separate tests cover a refused and an unreachable control port.
//!
//! The mock listens on loopback below the ephemeral range and checks SAFECOOKIE's hashes as Tor
//! does; nothing leaves the host. Unlike the other dashd tests this one cannot be skipped
//! (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use bitcoin_hashes::{Hash, HashEngine, Hmac, HmacEngine, sha256};
use common::*;

const ONION_ID: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd";
/// `ED25519-V3:` and the base64 of the bytes 0 to 63: a key IM1b's codec reads.
const KEY: &str = "ED25519-V3:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw==";
const SERVER_NONCE: [u8; 32] = [0x22; 32];
/// How long a node runs after its session, for the last reply's log lines.
const SETTLE: Duration = Duration::from_millis(500);

/// What the mock offers.
#[derive(Clone, Default)]
struct Script {
    /// `PROTOCOLINFO`'s `METHODS`.
    methods: &'static str,
    /// The cookie file and its content, for SAFECOOKIE.
    cookie: Option<(String, Vec<u8>)>,
    /// Close the connection after answering `ADD_ONION`.
    close_after_onion: bool,
    /// Close the connection after answering `GETINFO net/listeners/socks`.
    close_after_socks: bool,
    /// Nothing listens: every connection is refused.
    refuse: bool,
}

/// Every connection's command lines; SAFECOOKIE's nonce and hash replaced once checked.
type Sessions = Arc<Mutex<Vec<Vec<String>>>>;

fn hmac(key: &str, parts: &[&[u8]]) -> Vec<u8> {
    let mut e = HmacEngine::<sha256::HashEngine>::new(key.as_bytes());
    for p in parts {
        e.input(p);
    }
    let mac: Hmac<sha256::Hash> = e.finalize();
    mac.to_byte_array().to_vec()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// One control connection, as Tor answers each command.
fn serve(stream: std::net::TcpStream, script: &Script, session: &mut dyn FnMut(String)) {
    let mut out = stream.try_clone().unwrap();
    let mut client_hash = None;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        let line = line.trim_end_matches('\r').to_owned();
        let reply = if line == "PROTOCOLINFO 1" {
            session(line.clone());
            let file = match &script.cookie {
                Some((path, _)) => format!(" COOKIEFILE=\"{path}\""),
                None => String::new(),
            };
            format!(
                "250-PROTOCOLINFO 1\r\n250-AUTH METHODS={}{file}\r\n250-VERSION Tor=\"0.4.8.13\"\r\n250 OK\r\n",
                script.methods
            )
        } else if let Some(nonce) = line.strip_prefix("AUTHCHALLENGE SAFECOOKIE ") {
            session("AUTHCHALLENGE SAFECOOKIE <client nonce>".into());
            let cookie = &script.cookie.as_ref().unwrap().1;
            let nonce = unhex(nonce);
            let key = |k| hmac(k, &[cookie, &nonce, &SERVER_NONCE]);
            client_hash =
                Some(hex(&key("Tor safe cookie authentication controller-to-server hash")));
            let server = key("Tor safe cookie authentication server-to-controller hash");
            format!(
                "250 AUTHCHALLENGE SERVERHASH={} SERVERNONCE={}\r\n",
                hex(&server).to_uppercase(),
                hex(&SERVER_NONCE)
            )
        } else if let Some(auth) = line.strip_prefix("AUTHENTICATE ")
            && client_hash.is_some()
        {
            let ok = client_hash.as_deref() == Some(auth);
            session(format!(
                "AUTHENTICATE <client hash {}>",
                if ok {
                    "ok"
                } else {
                    "wrong"
                }
            ));
            if ok {
                "250 OK\r\n".into()
            } else {
                "515 Authentication failed\r\n".into()
            }
        } else if line.starts_with("ADD_ONION ") {
            session(line.clone());
            let key = if line.contains("NEW:") {
                format!("250-PrivateKey={KEY}\r\n")
            } else {
                String::new()
            };
            format!("250-ServiceID={ONION_ID}\r\n{key}250 OK\r\n")
        } else {
            session(line.clone());
            match line.as_str() {
                "GETINFO net/listeners/socks" => {
                    "250-net/listeners/socks=\"127.0.0.1:19050\"\r\n250 OK\r\n".into()
                }
                l if l.starts_with("AUTHENTICATE") => "250 OK\r\n".into(),
                _ => "510 Unrecognized command\r\n".into(),
            }
        };
        out.write_all(reply.as_bytes()).unwrap();
        if script.close_after_onion && line.starts_with("ADD_ONION ") {
            return;
        }
        if script.close_after_socks && line == "GETINFO net/listeners/socks" {
            return;
        }
    }
}

fn mock_tor(script: Script) -> (Sessions, u16) {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let sessions = Sessions::default();
    if script.refuse {
        return (sessions, port);
    }
    let recorded = Arc::clone(&sessions);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                return;
            };
            let n = {
                let mut s = recorded.lock().unwrap();
                s.push(Vec::new());
                s.len() - 1
            };
            let recorded = Arc::clone(&recorded);
            let script = script.clone();
            std::thread::spawn(move || {
                serve(stream, &script, &mut |l| recorded.lock().unwrap()[n].push(l));
            });
        }
    });
    (sessions, port)
}

/// The log's Tor lines (and `ParseNetwork`'s warning for the `tor` alias) without their
/// timestamps, with what differs by run: the data directory, the mock's port, and SAFECOOKIE's
/// server hash (from the random client nonce).
// Core: netbase.cpp:104-107
fn tor_lines(log: &str, datadir: &Path, port: u16) -> Vec<String> {
    let datadir = datadir.display().to_string();
    log.lines()
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest))
        .filter(|l| {
            l.starts_with("tor: ")
                || l.starts_with("[tor] ")
                || l.starts_with("Got tor service ID")
                || l.starts_with("Warning: net name 'tor' is deprecated")
        })
        .map(|l| {
            let l = l.replace(&datadir, "<datadir>").replace(&format!(":{port}"), ":<port>");
            match l.strip_prefix("[tor] AUTHCHALLENGE ServerHash ") {
                Some(rest) => format!("[tor] AUTHCHALLENGE ServerHash <hash>{}", &rest[64..]),
                None => l,
            }
        })
        .collect()
}

/// `getnetworkinfo`'s onion network object and local addresses.
fn onion_state(info: &str) -> (String, String) {
    let net = info.find(r#"{"name":"onion""#).map(|at| {
        let end = info[at..].find('}').unwrap();
        info[at..=at + end].to_owned()
    });
    let local = info.find(r#""localaddresses":"#).map(|at| {
        let end = info[at..].find(']').unwrap();
        info[at..=at + end].to_owned()
    });
    (net.unwrap_or_default(), local.unwrap_or_default())
}

struct Run {
    sessions: Vec<Vec<String>>,
    log: Vec<String>,
    info: (String, String),
    key: Option<Vec<u8>>,
}

/// Starts `binary` on `datadir` against a mock with `script`, waits until `done` holds for its
/// sessions, then reads `getnetworkinfo` and stops it.
fn run(
    binary: &Path,
    home: &Path,
    datadir: &Path,
    script: Script,
    args: &[String],
    done: impl Fn(&[Vec<String>]) -> bool,
    settle: Duration,
) -> Run {
    std::fs::create_dir_all(datadir).unwrap();
    let (sessions, port) = mock_tor(script);
    let control = format!("-torcontrol=127.0.0.1:{port}");
    let mut extra = vec!["-dnsseed=0", "-connect=0", "-debug=tor", control.as_str()];
    extra.extend(args.iter().map(String::as_str));
    if binary != Path::new(RDASHD) {
        extra.extend(["-disablewallet", "-blocksonly=1"]);
    }
    let node = RpcNode::start_on(binary, home, datadir, &extra);
    wait_until("the mock's sessions", || done(&sessions.lock().unwrap()));
    // The last reply's log lines follow the session.
    std::thread::sleep(settle);
    let info = onion_state(node.call("getnetworkinfo", "[]").result());
    node.node.stop_ok(libc::SIGTERM);
    let sessions = sessions.lock().unwrap().clone();
    Run {
        sessions,
        log: tor_lines(&read(&debug_log(datadir)), datadir, port),
        info,
        key: std::fs::read(net(datadir).join("onion_v3_private_key")).ok(),
    }
}

fn add_onion_count(s: &[Vec<String>]) -> usize {
    s.iter().flatten().filter(|l| l.starts_with("ADD_ONION")).count()
}

fn commands(s: &[Vec<String>]) -> usize {
    s.iter().map(Vec::len).sum()
}

#[test]
fn tor_control_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let cookie_path = home.path().join("control_auth_cookie");
    std::fs::write(&cookie_path, [0xc0; 32]).unwrap();
    let cookie = Some((cookie_path.display().to_string(), vec![0xc0; 32]));
    let null = Script {
        methods: "NULL",
        ..Script::default()
    };
    type Done = fn(&[Vec<String>]) -> bool;
    let onion_added: Done = |s| add_onion_count(s) == 1;
    let protocolinfo_answered: Done = |s| commands(s) >= 1;
    let cases: [(&str, Script, Vec<String>, Done); 12] = [
        ("null", null.clone(), vec![], onion_added),
        (
            "password",
            Script {
                methods: "COOKIE,SAFECOOKIE,HASHEDPASSWORD",
                ..Script::default()
            },
            vec![r#"-torpassword=pa"ss word"#.into()],
            onion_added,
        ),
        (
            "safecookie",
            Script {
                methods: "COOKIE,SAFECOOKIE",
                cookie: cookie.clone(),
                ..Script::default()
            },
            vec![],
            onion_added,
        ),
        ("onion", null.clone(), vec!["-onion=127.0.0.1:19051".into()], onion_added),
        (
            "nomethod",
            Script {
                methods: "COOKIE",
                ..Script::default()
            },
            vec![],
            protocolinfo_answered,
        ),
        (
            "reconnect",
            Script {
                methods: "NULL",
                close_after_onion: true,
                ..Script::default()
            },
            vec![],
            |s| add_onion_count(s) == 2,
        ),
        // `-onlynet` without onion: Tor's SOCKS port is still the onion proxy, but onion is not
        // reachable and the service is not a local address.
        ("onlynet-ipv4", null.clone(), vec!["-onlynet=ipv4".into()], onion_added),
        ("onlynet-onion", null.clone(), vec!["-onlynet=onion".into()], onion_added),
        // The deprecated alias: onion, with `ParseNetwork`'s warning again at `GETINFO`'s reply.
        ("onlynet-tor", null.clone(), vec!["-onlynet=tor".into()], onion_added),
        // `-proxy` turns `-listen` off unless it is given; Tor's SOCKS port replaces the proxy.
        (
            "proxy",
            null.clone(),
            vec!["-proxy=127.0.0.1:19052".into(), "-listen=1".into()],
            onion_added,
        ),
        ("noonion", null.clone(), vec!["-noonion".into()], onion_added),
        // An empty `-onion` is set but still asks Tor for its SOCKS port (`GetArg == ""`).
        ("onion-empty", null.clone(), vec!["-onion=".into()], onion_added),
    ];
    let mut failures = Vec::new();
    for (name, script, args, done) in cases {
        let binds = [
            format!("-bind=127.0.0.1:{}", free_port()),
            format!("-bind=127.0.0.1:{}=onion", free_port()),
        ];
        let args = [&binds[..], &args].concat();
        let mut runs = Vec::new();
        for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
            let datadir = home.path().join(format!("{name}-{who}"));
            let mut run1 = run(binary, home.path(), &datadir, script.clone(), &args, done, SETTLE);
            if name == "null" {
                // Restarted, the node sends the key it cached.
                let again = run(binary, home.path(), &datadir, script.clone(), &args, done, SETTLE);
                run1.sessions.extend(again.sessions);
                run1.log.extend(again.log);
            }
            runs.push(run1);
        }
        let (d, r) = (&runs[0], &runs[1]);
        for (what, same) in [
            ("sessions", d.sessions == r.sessions),
            ("logs", d.log == r.log),
            ("getnetworkinfo", d.info == r.info),
            ("key files", d.key == r.key),
        ] {
            if !same {
                failures.push(format!(
                    "{name} {what}:\n  dashd  {:?}\n  rdashd {:?}",
                    (&d.sessions, &d.log, &d.info),
                    (&r.sessions, &r.log, &r.info)
                ));
            }
        }
        // The case reached what it meant to.
        let expected_key = !matches!(name, "nomethod");
        assert_eq!(
            d.key.as_deref() == Some(KEY.as_bytes()),
            expected_key,
            "{name}: {:?}",
            d.sessions
        );
        assert_eq!(
            d.info.0.contains(r#""reachable":true"#),
            !matches!(name, "nomethod" | "onlynet-ipv4" | "noonion"),
            "{name}: {:?}",
            d.info
        );
        if matches!(name, "proxy" | "onlynet-ipv4") {
            assert!(d.info.0.contains(r#""proxy":"127.0.0.1:19050""#), "{name}: {:?}", d.info);
        }
        if name == "onlynet-tor" {
            // Twice at init (init.cpp:1076, :1849), and again after `GETINFO`'s reply re-reads
            // `-onlynet` (torcontrol.cpp:402).
            let warned = |l: &String| l.starts_with("Warning: net name 'tor'");
            assert_eq!(d.log.iter().filter(|l| warned(l)).count(), 3, "{name}: {:?}", d.log);
            let proxy = d.log.iter().position(|l| l.starts_with("[tor] Configuring onion proxy"));
            assert!(d.log[proxy.unwrap()..].iter().any(warned), "{name}: {:?}", d.log);
        }
        if name == "safecookie" {
            assert!(d.sessions[0].contains(&"AUTHENTICATE <client hash ok>".to_owned()), "{name}");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A control port that refuses connections: the error event, then reconnections after 1 s and
/// 1.5 s more, each logged alike. The first two attempts are compared (a third depends on how
/// long the node runs).
// Core: torcontrol.cpp:122-137, :619-646
#[test]
fn a_refused_control_port_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let script = Script {
        refuse: true,
        ..Script::default()
    };
    let binds = [
        format!("-bind=127.0.0.1:{}", free_port()),
        format!("-bind=127.0.0.1:{}=onion", free_port()),
    ];
    let mut logs = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let datadir = home.path().join(who);
        let run = run(
            binary,
            home.path(),
            &datadir,
            script.clone(),
            &binds,
            |_| true,
            Duration::from_millis(1800),
        );
        assert!(run.sessions.is_empty());
        logs.push(run.log);
    }
    let attempt = [
        "[tor] Error connecting to Tor control socket",
        "[tor] Not connected to Tor control port 127.0.0.1:<port>, trying to reconnect",
    ];
    assert_eq!(logs[0][..4], [attempt, attempt].concat(), "{:?}", logs[0]);
    assert_eq!(logs[0][..4], logs[1][..4], "{:?}", logs[1]);
    assert_eq!(logs[0].last(), logs[1].last());
}

/// A control port whose connect fails at once (the broadcast address is unreachable): Core's
/// `Connect` logs and returns false, no error event follows and no reconnection is scheduled,
/// yet the loop waits for the interrupt. The mock's port is overridden by the later
/// `-torcontrol`.
// Core: torcontrol.cpp:168-172, :325-327
#[test]
fn an_unreachable_control_port_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let args = [
        format!("-bind=127.0.0.1:{}", free_port()),
        format!("-bind=127.0.0.1:{}=onion", free_port()),
        "-torcontrol=255.255.255.255:9051".to_owned(),
    ];
    let mut logs = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let datadir = home.path().join(who);
        // Long enough for two reconnections, were any scheduled.
        let run = run(
            binary,
            home.path(),
            &datadir,
            Script::default(),
            &args,
            |_| true,
            Duration::from_millis(2800),
        );
        assert!(run.sessions.is_empty());
        logs.push(run.log);
    }
    assert_eq!(
        logs[0],
        [
            "tor: Error connecting to address 255.255.255.255:9051",
            "tor: Initiating connection to Tor control port 255.255.255.255:9051 failed",
            "tor: Thread interrupt",
        ],
    );
    assert_eq!(logs[0], logs[1]);
}

/// The regtest onion service target, which the default onion bind uses.
// Core: chainparamsbase.cpp:48
const REGTEST_ONION_TARGET: &str = "127.0.0.1:19896";

/// Keeps [`REGTEST_ONION_TARGET`] taken until dropped, so that a node's bind of it fails. Any
/// regtest node on the host with a default onion bind may hold it already, which serves as well
/// and may last longer than the test; the thread binds the target within about a millisecond of
/// such a holder letting it go, so only a node binding inside that gap would get it.
struct OnionTargetTaken {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for OnionTargetTaken {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

fn bind_regtest_onion_target() -> Option<TcpListener> {
    match TcpListener::bind(REGTEST_ONION_TARGET) {
        Ok(listener) => Some(listener),
        Err(e) if e.kind() == ErrorKind::AddrInUse => None,
        Err(e) => panic!("binding {REGTEST_ONION_TARGET}: {e}"),
    }
}

fn hold_regtest_onion_target() -> OnionTargetTaken {
    let mut held = bind_regtest_onion_target();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            while !stop.load(Ordering::Relaxed) {
                if held.is_none() {
                    held = bind_regtest_onion_target();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });
    OnionTargetTaken {
        stop,
        thread: Some(thread),
    }
}

/// A plain `-bind` with `-listenonion` on by default and the default onion target taken
/// (dashpay/dash#8003): the optional default onion bind fails with Core's warning, the node
/// starts, and Tor control runs without an onion service. Each session authenticates and asks
/// for the SOCKS port, no `ADD_ONION` is sent, and "Not creating an onion service" is logged
/// once across a reconnection, on both binaries alike. With `-listenonion=1` the bind is
/// required and init fails alike.
// Core: init.cpp:2681-2686, :2761-2767, net.cpp:3986-3994, torcontrol.cpp:460-466
#[test]
fn a_taken_default_onion_target_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let _held = hold_regtest_onion_target();
    let script = Script {
        methods: "NULL",
        close_after_socks: true,
        ..Script::default()
    };
    let args = [format!("-bind=127.0.0.1:{}", free_port())];
    let reconnected =
        |s: &[Vec<String>]| s.len() >= 2 && s[1].iter().any(|l| l == "GETINFO net/listeners/socks");
    // The bind lines, without their timestamps and with the program's name as rdashd's.
    let bind_lines = |datadir: &Path| -> Vec<String> {
        read(&debug_log(datadir))
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest))
            .filter(|l| l.contains(REGTEST_ONION_TARGET))
            .map(|l| l.replace("Dash Core", "rdashd"))
            .collect()
    };
    let mut runs = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let datadir = home.path().join(who);
        let run = run(binary, home.path(), &datadir, script.clone(), &args, reconnected, SETTLE);
        runs.push((run, bind_lines(&datadir)));
    }
    let ((d, d_binds), (r, r_binds)) = (&runs[0], &runs[1]);
    assert_eq!(
        *d_binds,
        [
            format!(
                "[net:error] Unable to bind to {REGTEST_ONION_TARGET} on this computer. rdashd is probably already running."
            ),
            format!(
                "[warning] Unable to bind the default onion service target {REGTEST_ONION_TARGET}, continuing without it"
            ),
        ]
    );
    assert_eq!(r_binds, d_binds);
    assert_eq!(r.sessions, d.sessions);
    assert_eq!(r.log, d.log);
    assert_eq!(add_onion_count(&d.sessions), 0, "{:?}", d.sessions);
    let not_creating = "tor: Not creating an onion service because there is no dedicated onion bind. Use -bind=127.0.0.1:<port>=onion to create one.";
    assert_eq!(d.log.iter().filter(|l| *l == not_creating).count(), 1, "{:?}", d.log);
    assert_eq!(r.info, d.info);

    let mut failed = Vec::new();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let datadir = home.path().join(format!("{who}-explicit"));
        std::fs::create_dir_all(&datadir).unwrap();
        let explicit = [args[0].as_str(), "-listenonion=1", "-dnsseed=0", "-connect=0"];
        let out = output(regtest_on(binary, home.path(), &datadir, &explicit));
        failed.push((out.status.code(), stderr(&out).replace("Dash Core", "rdashd")));
    }
    assert_eq!(
        failed[0],
        (
            Some(1),
            format!(
                "Error: Unable to bind to {REGTEST_ONION_TARGET} on this computer. rdashd is probably already running.\nError: Failed to listen on any port. Use -listen=0 if you want this.\n"
            )
        )
    );
    assert_eq!(failed[1], failed[0]);
}

/// A bad `-whitelist` fails init at step 12 before Tor control starts: neither binary connects
/// to the control port.
// Core: init.cpp:2708-2719, :2761-2767
#[test]
fn a_bad_whitelist_stops_init_before_tor_control() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    for (binary, who) in [(dashd.as_path(), "dashd"), (Path::new(RDASHD), "rdashd")] {
        let (sessions, port) = mock_tor(Script {
            methods: "NULL",
            ..Script::default()
        });
        let datadir = home.path().join(who);
        std::fs::create_dir_all(&datadir).unwrap();
        let args = [
            format!("-bind=127.0.0.1:{}", free_port()),
            format!("-bind=127.0.0.1:{}=onion", free_port()),
            format!("-torcontrol=127.0.0.1:{port}"),
            "-whitelist=x".to_owned(),
        ];
        let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
        args.extend(["-dnsseed=0", "-connect=0", "-debug=tor"]);
        let out = output(regtest_on(binary, home.path(), &datadir, &args));
        assert_eq!(out.status.code(), Some(1), "{who}: {}", stderr(&out));
        assert_eq!(stderr(&out), "Error: Invalid netmask specified in -whitelist: 'x'\n", "{who}");
        // A connection the node made would have been accepted by now.
        std::thread::sleep(SETTLE);
        assert!(sessions.lock().unwrap().is_empty(), "{who}: {:?}", sessions.lock().unwrap());
    }
}
