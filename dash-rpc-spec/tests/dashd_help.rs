//! Compares the table with a live regtest dashd v24.0.0-rc.4: `help` and `help <cmd>` for every
//! command, with the wallet enabled and under `-disablewallet`, and the server's argument map
//! (`help dump_all_command_conversions`) against the declared arguments and `CONVERT_PARAMS`.
//!
//! Gated like the dash-spv dashd tests: skipped when `SKIP_DASHD_TESTS` is set. The dashd is
//! `DASHD_V24_PATH`, else `DASHD_PATH` if that is v24.0.0-rc.4, else the copy
//! `DASHVERSION=24.0.0-rc.4 python3 contrib/setup-dashd.py` caches next to `DASHD_PATH`'s
//! (or in `~/.rust-dashcore-test`). `DASH_RPC_SPEC_RPCPORT` picks the RPC port (dashd runs
//! with `-listen=0`, so it binds no other), and must be free; otherwise a free one is picked,
//! and another if dashd cannot bind it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dash_rpc_spec::{ArgType, COMMANDS, CONVERT_PARAMS, CORE_VERSION, Gating, help};
use node_test_ports::free_port;
use serde_json::{Value, json};

/// `CORE_VERSION` as setup-dashd.py's DASHVERSION spells it.
fn setup_version() -> &'static str {
    CORE_VERSION.strip_prefix('v').unwrap()
}

/// The first line of `-version`. dashd rewrites `settings.json` in its data directory even for
/// `-version`, so concurrent probes of the shared default directory race (BL-438): use a private
/// one.
fn dashd_version(path: &Path) -> Option<String> {
    let datadir = tempfile::tempdir().ok()?;
    let out = Command::new(path)
        .arg("-version")
        .arg(format!("-datadir={}", datadir.path().display()))
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.lines().next().map(str::to_owned)
}

fn is_pinned(path: &Path) -> bool {
    dashd_version(path).is_some_and(|v| v.ends_with(&format!("version {CORE_VERSION}")))
}

/// The v24 dashd, or `None` when dashd tests are skipped.
fn find_dashd() -> Option<PathBuf> {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("Skipping dashd integration test (SKIP_DASHD_TESTS is set)");
        return None;
    }
    if let Some(p) = std::env::var_os("DASHD_V24_PATH") {
        let p = PathBuf::from(p);
        assert!(is_pinned(&p), "DASHD_V24_PATH ({}) is not dashd {CORE_VERSION}", p.display());
        return Some(p);
    }
    let version = setup_version();
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("DASHD_PATH") {
        let p = PathBuf::from(p);
        // <cache>/dashcore-<version>/bin/dashd
        let cache = p.ancestors().nth(3).map(|c| c.join(format!("dashcore-{version}/bin/dashd")));
        candidates.push(p);
        candidates.extend(cache);
    }
    // setup-dashd.py's default cache directory.
    for home in ["HOME", "USERPROFILE"].into_iter().filter_map(std::env::var_os) {
        candidates.push(
            PathBuf::from(home).join(format!(".rust-dashcore-test/dashcore-{version}/bin/dashd")),
        );
    }
    let Some(found) = candidates.into_iter().find(|p| is_pinned(p)) else {
        panic!(
            "dashd {CORE_VERSION} not found. Run `DASHVERSION={version} python3 \
             contrib/setup-dashd.py`, or set DASHD_V24_PATH, or set SKIP_DASHD_TESTS=1 to skip."
        )
    };
    Some(found)
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize])
            } else {
                '='
            });
        }
    }
    out
}

/// A regtest dashd on its own datadir and RPC port. It authenticates with the cookie dashd
/// writes into that datadir, so no other daemon accepts its requests, and it counts as owned
/// only once the child is still running and `getrpcinfo` reports a log inside the datadir. Only
/// an owned endpoint is sent `stop`; otherwise the child is killed by PID.
struct Dashd {
    child: Child,
    rpcport: u16,
    datadir: tempfile::TempDir,
    auth: Option<String>,
    owned: bool,
}

const START_ATTEMPTS: usize = 5;

impl Dashd {
    /// With `rpcport`, a port that dashd cannot bind is an error. Without, a free port is
    /// picked, and picked again when dashd exits during startup (another process took it).
    fn start(path: &Path, rpcport: Option<u16>, extra: &[&str]) -> Dashd {
        let mut attempt = 1;
        loop {
            match Self::try_start(path, rpcport.unwrap_or_else(free_port), extra) {
                Ok(node) => return node,
                Err(e) if rpcport.is_none() && attempt < START_ATTEMPTS => {
                    eprintln!("retrying on another RPC port: {e}");
                    attempt += 1;
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// `Err` (with the end of debug.log) when the child exits during startup.
    fn try_start(path: &Path, rpcport: u16, extra: &[&str]) -> Result<Dashd, String> {
        let datadir = tempfile::tempdir().unwrap();
        let child = Command::new(path)
            .args([
                "-regtest",
                &format!("-datadir={}", datadir.path().display()),
                &format!("-rpcport={rpcport}"),
                "-rpcbind=127.0.0.1",
                "-rpcallowip=127.0.0.1",
                "-listen=0",
                "-dnsseed=0",
                "-fixedseeds=0",
                "-server",
                "-printtoconsole=0",
            ])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut node = Dashd {
            child,
            rpcport,
            datadir,
            auth: None,
            owned: false,
        };
        let cookie = node.datadir.path().join("regtest").join(".cookie");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if node.auth.is_none() {
                node.auth = std::fs::read(&cookie).ok().map(|c| format!("Basic {}", base64(&c)));
            }
            if node.auth.is_some() {
                match node.try_call("getblockcount", json!([])) {
                    Ok(Ok(_)) if node.child.try_wait().unwrap().is_none() => break,
                    Ok(Err(e)) if e["code"] != -28 => panic!("getblockcount: {e}"),
                    _ => {}
                }
            }
            if node.child.try_wait().unwrap().is_some() {
                return Err(format!(
                    "dashd on RPC port {rpcport} exited during startup; debug.log ends:\n{}",
                    node.debug_log_tail()
                ));
            }
            if Instant::now() >= deadline {
                panic!(
                    "dashd did not answer RPC within 120 s; debug.log ends:\n{}",
                    node.debug_log_tail()
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let logpath =
            PathBuf::from(node.call("getrpcinfo", json!([]))["logpath"].as_str().unwrap());
        let datadir = node.datadir.path().canonicalize().unwrap();
        assert!(
            logpath.canonicalize().is_ok_and(|l| l.starts_with(&datadir)),
            "RPC port {rpcport} answers for another dashd (log {})",
            logpath.display()
        );
        node.owned = true;
        Ok(node)
    }

    fn debug_log_tail(&self) -> String {
        let log = std::fs::read_to_string(self.datadir.path().join("regtest").join("debug.log"))
            .unwrap_or_else(|e| format!("(no debug.log: {e})"));
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    }

    /// `Err` for transport failures (a refused cookie included), `Ok(Err(error))` for an RPC
    /// error.
    fn try_call(&self, method: &str, params: Value) -> std::io::Result<Result<Value, Value>> {
        let auth = self.auth.as_deref().ok_or_else(|| std::io::Error::other("no cookie yet"))?;
        let body =
            json!({"jsonrpc": "1.0", "id": 0, "method": method, "params": params}).to_string();
        let mut s = TcpStream::connect(("127.0.0.1", self.rpcport))?;
        s.set_read_timeout(Some(Duration::from_secs(60)))?;
        write!(
            s,
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {auth}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
        let mut reply = Vec::new();
        s.read_to_end(&mut reply)?;
        let reply = String::from_utf8(reply).unwrap();
        let (_, body) =
            reply.split_once("\r\n\r\n").ok_or_else(|| std::io::Error::other("no HTTP body"))?;
        let v: Value = serde_json::from_str(body)
            .map_err(|e| std::io::Error::other(format!("{e}: {body}")))?;
        Ok(if v["error"].is_null() {
            Ok(v["result"].clone())
        } else {
            Err(v["error"].clone())
        })
    }

    fn call(&self, method: &str, params: Value) -> Value {
        self.try_call(method, params).unwrap().unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    fn help(&self, command: &str) -> String {
        let params: Vec<&str> = if command.is_empty() {
            vec![]
        } else {
            command.splitn(2, ' ').collect()
        };
        self.call("help", json!(params)).as_str().unwrap().to_owned()
    }
}

impl Drop for Dashd {
    fn drop(&mut self) {
        if self.owned && self.child.try_wait().ok().flatten().is_none() {
            let _ = self.try_call("stop", json!([]));
            let deadline = Instant::now() + Duration::from_secs(60);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn base64_encodes() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"__cookie__:ab"), "X19jb29raWVfXzphYg==");
}

/// `help dump_all_command_conversions` as the table predicts it (`RPCHelpMan::GetArgMap` over
/// the registered commands, sub-commands left out, in name order).
fn expected_arg_map(wallet_enabled: bool) -> Value {
    let mut rows = Vec::new();
    for c in
        COMMANDS.iter().filter(|c| c.gating.registered(wallet_enabled) && c.subcommand().is_none())
    {
        for (i, a) in c.args.iter().enumerate() {
            for name in a.aliases() {
                rows.push(json!([c.name, i, name, matches!(a.ty, ArgType::Str | ArgType::StrHex)]));
            }
        }
    }
    Value::Array(rows)
}

fn check_config(dashd: &Path, rpcport: Option<u16>, wallet_enabled: bool) -> usize {
    let (extra, mode): (&[&str], _) = if wallet_enabled {
        (&[], "wallet")
    } else {
        (&["-disablewallet"], "-disablewallet")
    };
    let node = Dashd::start(dashd, rpcport, extra);
    if wallet_enabled {
        node.call("createwallet", json!(["rpcspec"]));
    }

    assert_eq!(node.help(""), help("", wallet_enabled), "help overview ({mode})");
    assert_eq!(node.help("foo"), help("foo", wallet_enabled));
    for c in COMMANDS {
        // An unregistered command must answer "unknown command" on both sides too.
        assert_eq!(node.help(c.name), help(c.name, wallet_enabled), "help {} ({mode})", c.name);
    }

    let live_map = node.call("help", json!(["dump_all_command_conversions"]));
    assert_eq!(live_map, expected_arg_map(wallet_enabled), "argument map ({mode})");
    if wallet_enabled {
        // rpc_help.py's check: client.cpp's table equals the server's non-string arguments,
        // leaving out echojson (client only) and sub-commands (not in the server's map).
        let mut server: Vec<(&str, u64, &str)> = live_map
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r[3] == false)
            .map(|r| (r[0].as_str().unwrap(), r[1].as_u64().unwrap(), r[2].as_str().unwrap()))
            .collect();
        let mut client: Vec<(&str, u64, &str)> = CONVERT_PARAMS
            .iter()
            .filter(|p| p.method != "echojson" && !p.method.contains(' '))
            .map(|p| (p.method, p.index as u64, p.name))
            .collect();
        server.sort();
        client.sort();
        assert_eq!(client, server, "CONVERT_PARAMS vs the server's conversion map");
    }
    // The overview, "help foo" and every command.
    COMMANDS.len() + 2
}

#[test]
fn help_matches_dashd() {
    let Some(dashd) = find_dashd() else {
        return;
    };
    let rpcport = std::env::var("DASH_RPC_SPEC_RPCPORT").ok().map(|p| p.parse().unwrap());
    let with = check_config(&dashd, rpcport, true);
    let without = check_config(&dashd, rpcport, false);
    let wallet_only = COMMANDS.iter().filter(|c| c.gating == Gating::Wallet).count();
    eprintln!(
        "dash-rpc-spec: {with} help outputs equal with the wallet, {without} under -disablewallet \
         ({} commands, {wallet_only} wallet-only)",
        COMMANDS.len()
    );
}
