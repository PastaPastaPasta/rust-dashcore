//! O15-2: the live shadow's start-up against fake JSON-RPC servers. The live loop starts at
//! once, so a peer that is still loading never holds back rdashd's tripwire, and a peer without
//! `coinstatsindex` stops the shadow.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TRIPWIRE: &str = "Warning: This node disagrees with the ChainLocked chain";

/// A JSON-RPC server on a free port that answers each request with `answer(method)`'s
/// `(result, error)` pair.
fn fake_rpc(answer: impl Fn(&str) -> (Value, Value) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                return;
            };
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let request: Value = serde_json::from_slice(&body).unwrap();
            let (result, error) = answer(request["method"].as_str().unwrap());
            let reply = json!({"result": result, "error": error, "id": request["id"]}).to_string();
            let status = if error.is_null() {
                "200 OK"
            } else {
                "500 Internal Server Error"
            };
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    url
}

fn error(code: i64) -> (Value, Value) {
    (Value::Null, json!({"code": code, "message": "no"}))
}

/// rdashd with the ChainLock tripwire set and no ChainLock.
fn tripped_node() -> String {
    fake_rpc(|method| match method {
        "getblockchaininfo" => (json!({"warnings": TRIPWIRE}), Value::Null),
        _ => error(-32603),
    })
}

/// The live shadow on empty digest logs in `dir`, its stderr in `dir/stderr`.
fn shadow(dir: &Path, peer: &str, node: &str) -> Child {
    let cookie = dir.join("cookie");
    fs::write(&cookie, "u:p").unwrap();
    for log in ["l", "r"] {
        fs::write(dir.join(log), "").unwrap();
    }
    let stderr = fs::File::create(dir.join("stderr")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_dash-conformance"))
        .arg("shadow")
        .args(["--left".as_ref(), dir.join("l").as_os_str()])
        .args(["--right".as_ref(), dir.join("r").as_os_str()])
        .args(["--out".as_ref(), dir.join("out").as_os_str()])
        .args(["--peer-rpc-url", peer, "--node-rpc-url", node, "--poll-secs", "1"])
        .args(["--peer-cookie".as_ref(), cookie.as_os_str()])
        .args(["--node-cookie".as_ref(), cookie.as_os_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .unwrap()
}

fn alarm_classes(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("out/alarms.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["class"].to_string())
        .collect()
}

fn wait_exit(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            panic!("the shadow did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn loading_peer_does_not_hold_back_d3_at_start_up() {
    let dir = tempfile::Builder::new().prefix("o15-2-startup-").tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    // The peer is still loading its block index for the whole test.
    let peer = fake_rpc(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        error(-28)
    });
    let mut child = shadow(dir.path(), &peer, &tripped_node());
    // Three peer attempts span the 2 s and 4 s backoffs.
    let deadline = Instant::now() + Duration::from_secs(60);
    while calls.load(Ordering::SeqCst) < 3 || alarm_classes(dir.path()).is_empty() {
        assert!(child.try_wait().unwrap().is_none(), "the shadow exited");
        if Instant::now() >= deadline {
            child.kill().ok();
            panic!(
                "no alarm journal after {} peer calls; stderr:\n{}",
                calls.load(Ordering::SeqCst),
                fs::read_to_string(dir.path().join("stderr")).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let classes = alarm_classes(dir.path());
    assert_eq!(classes, [r#"{"Crit":"D3"}"#, r#"{"Warn":"PeerRpc"}"#], "{classes:?}");
    // SIGTERM stops it cleanly.
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: kill(2) takes no pointers.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert!(wait_exit(&mut child, Duration::from_secs(30)).success());
}

#[test]
fn peer_without_coinstatsindex_stops_the_shadow() {
    let dir = tempfile::Builder::new().prefix("o15-2-startup-").tempdir().unwrap();
    let peer = fake_rpc(|_| (json!({"txindex": {"synced": true}}), Value::Null));
    let mut child = shadow(dir.path(), &peer, &tripped_node());
    let status = wait_exit(&mut child, Duration::from_secs(30));
    assert!(!status.success(), "{status}");
    let stderr = fs::read_to_string(dir.path().join("stderr")).unwrap();
    assert!(stderr.contains("-coinstatsindex"), "{stderr}");
}
