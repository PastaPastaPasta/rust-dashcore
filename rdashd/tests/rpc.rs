//! Dash Core's RPC server functional tests on rdashd (RP0): `interface_rpc`, `interface_http`,
//! `rpc_users`, `rpc_whitelist`, `rpc_named_arguments`, `rpc_help`, `rpc_bind` (its loopback
//! cases), `rpc_external_queue`, the RPC parts of `feature_init` and `feature_shutdown`, and
//! `rdash-cli` against the node.
//!
//! The chain commands Core's tests call are RP1's. Where a case needs a command that succeeds,
//! it calls one RP0 serves instead, named in the case: `uptime` for `getblockcount` (written
//! before O11 served it), `getrpcinfo` for `getbestblockhash`, `echo` for `getblockhash 0`,
//! `logging ["nosuch"]` for `getblockhash 42` (a `-8` error), `help uptime` for the `quorum list`
//! sub-command.
#![cfg(unix)]

mod common;

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use common::*;

/// What `log` gained while `f` ran.
fn logged(log: &Path, f: impl FnOnce()) -> String {
    let before = read(log).len();
    f();
    // The debug log is written as lines arrive; give the last one a moment.
    std::thread::sleep(Duration::from_millis(200));
    read(log)[before..].to_owned()
}

/// A connection that sends requests one after another (`http.client.HTTPConnection`).
struct Conn(TcpStream, Vec<u8>);

impl Conn {
    fn open(port: u16) -> Conn {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        Conn(stream, Vec::new())
    }

    fn post(&mut self, auth: &str, extra_headers: &str, body: &str) -> Reply {
        let request = format!(
            "POST / HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {}\r\n{extra_headers}Content-Length: {}\r\n\r\n{body}",
            base64(auth.as_bytes()),
            body.len()
        );
        self.0.write_all(request.as_bytes()).unwrap();
        let mut buf = [0; 16 * 1024];
        loop {
            if let Some(end) = complete_reply(&self.1) {
                let reply = Reply::parse(&self.1[..end]);
                self.1.drain(..end);
                return reply;
            }
            let n = self.0.read(&mut buf).unwrap();
            assert!(n > 0, "closed before a whole reply");
            self.1.extend_from_slice(&buf[..n]);
        }
    }

    /// Whether the server has closed the connection (within `wait`).
    fn closed(&mut self, wait: Duration) -> bool {
        self.0.set_read_timeout(Some(wait)).unwrap();
        matches!(self.0.read(&mut [0; 1]), Ok(0))
    }
}

/// `interface_rpc`: `getrpcinfo`, batches, the HTTP status of errors, and a full work queue.
#[test]
fn interface_rpc() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let info = node.call("getrpcinfo", "[]").result().to_owned();
    let log_path = debug_log(&datadir).canonicalize().unwrap();
    let prefix = r#"{"active_commands":[{"method":"getrpcinfo","duration":"#;
    assert!(info.starts_with(prefix), "{info}");
    assert!(info.ends_with(&format!(r#"}}],"logpath":"{}"}}"#, log_path.display())), "{info}");

    let batch = r#"[{"method":"uptime","id":1},{"method":"invalidmethod","id":2},{"method":"echo","id":3,"params":[0]}]"#;
    let reply = post(node.port, "/", Some(&cookie(&datadir)), batch);
    assert_eq!(reply.status, 200);
    let (first, rest) = reply.body.split_once("},").unwrap();
    assert!(first.starts_with(r#"[{"result":"#) && first.ends_with(r#","error":null,"id":1"#));
    assert_eq!(
        rest,
        "{\"result\":null,\"error\":{\"code\":-32601,\"message\":\"Method not found\"},\"id\":2},{\"result\":[0],\"error\":null,\"id\":3}]\n"
    );

    assert_eq!(node.call("invalidmethod", "[]").error().0, 404);
    assert_eq!(
        node.call("logging", r#"[["nosuch"]]"#).error(),
        (500, r#"{"code":-8,"message":"unknown logging category nosuch"}"#)
    );
    node.stop();

    // Core's test runs dash-cli in three threads; plain requests show the same refusal.
    let node = RpcNode::start(home.path(), &datadir, &["-rpcworkqueue=1", "-rpcthreads=1"]);
    let exceeded = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + PATIENCE;
    let threads: Vec<_> = (0..3)
        .map(|_| {
            let (exceeded, auth, port) = (Arc::clone(&exceeded), cookie(&datadir), node.port);
            std::thread::spawn(move || {
                while !exceeded.load(Ordering::SeqCst) && Instant::now() < deadline {
                    let reply = rpc(port, &auth, "getrpcinfo", "[]");
                    if reply.status != 200 {
                        assert_eq!(
                            (reply.status, reply.body.as_str()),
                            (503, "Work queue depth exceeded")
                        );
                        exceeded.store(true, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(exceeded.load(Ordering::SeqCst), "no request was refused");
    node.stop();
}

/// `rdash-cli` next to `rdashd`, when it is built: a `cargo test --workspace` (the gate) builds
/// it, a `cargo test -p rdashd` (a CI group) does not (RP0-D10).
fn rdash_cli_path() -> Option<std::path::PathBuf> {
    let path = Path::new(RDASHD).with_file_name("rdash-cli");
    if path.exists() {
        return Some(path);
    }
    eprintln!("Skipping: {} is not built (`cargo build -p rdash-cli`)", path.display());
    None
}

/// `rdash-cli` against rdashd: the cookie, help text, named arguments, errors and `stop`.
#[test]
fn rdash_cli_drives_rdashd() {
    let Some(cli_path) = rdash_cli_path() else {
        return;
    };
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let cli = |args: &[&str]| {
        let port = format!("-rpcport={}", node.port);
        Command::new(&cli_path)
            .arg(datadir_arg(&datadir))
            .arg("-regtest")
            .arg(port)
            .args(args)
            .output()
            .unwrap()
    };
    let out = cli(&["help", "stop"]);
    assert!(out.status.success());
    // A string result is printed raw, with a newline.
    let help = unescape_json_string(node.call("help", r#"["stop"]"#).result());
    assert_eq!(stdout(&out), format!("{help}\n"));
    let out = cli(&["-named", "echo", "arg1=x", "arg3=4"]);
    assert_eq!(stdout(&out), "[\n  null,\n  \"x\",\n  null,\n  \"4\"\n]\n");
    let out = cli(&["echojson", "1.50", "{\"a\":[]}"]);
    assert_eq!(stdout(&out), "[\n  1.50,\n  {\n    \"a\": [\n    ]\n  }\n]\n");
    // `getmempoolinfo` is MP1a's: a missing method until then.
    let out = cli(&["getmempoolinfo"]);
    assert_eq!(out.status.code(), Some(89));
    assert_eq!(stderr(&out), "error code: -32601\nerror message:\nMethod not found\n");
    let out = cli(&["stop"]);
    assert_eq!(stdout(&out), "rdashd stopping\n");
    let out = node.node.wait();
    assert_eq!(out.status.code(), Some(0));
    assert!(!net(&datadir).join(".cookie").exists());
}

/// A JSON string's text (the escapes Core's writer uses).
fn unescape_json_string(json: &str) -> String {
    let inner = json.strip_prefix('"').and_then(|j| j.strip_suffix('"')).expect(json);
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().unwrap() {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
            }
            other => out.push(other),
        }
    }
    out
}

/// `interface_http`: HTTP/1.1 keeps the connection, `Connection: close` ends it, an idle
/// connection is closed after `-rpcservertimeout`, and the request line is limited.
#[test]
fn interface_http() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let auth = cookie(&datadir);
    let call = r#"{"method":"uptime"}"#;
    for extra in ["", "Connection: keep-alive\r\n"] {
        let mut conn = Conn::open(node.port);
        assert!(conn.post(&auth, extra, call).body.contains(r#""error":null"#));
        assert!(
            conn.post(&auth, extra, r#"{"method":"getrpcinfo"}"#).body.contains(r#""error":null"#)
        );
        assert!(!conn.closed(Duration::from_millis(100)));
    }
    let mut conn = Conn::open(node.port);
    let reply = conn.post(&auth, "Connection: close\r\n", call);
    assert!(reply.body.contains(r#""error":null"#));
    assert_eq!(reply.header("Connection"), Some("close"));
    assert!(conn.closed(PATIENCE));

    let get = |len: usize| {
        let request = format!("GET /{} HTTP/1.1\r\nHost: x\r\n\r\n", "x".repeat(len));
        Reply::parse(&http_raw(node.port, request.as_bytes())).status
    };
    assert_eq!(get(1000), 404);
    assert_eq!(get(10_000), 400);
    node.stop();

    // -rpcservertimeout=1, on its own node so the checks above never race it: an idle
    // connection is closed.
    let node = RpcNode::start(home.path(), &datadir, &["-rpcservertimeout=1"]);
    let mut conn = Conn::open(node.port);
    assert!(conn.post(&cookie(&datadir), "", call).body.contains(r#""error":null"#));
    assert!(conn.closed(Duration::from_secs(30)));
    node.stop();
}

/// Core's `rpc_users` vectors.
const RT_RPCAUTH: &str = "rt:93648e835a54c573682c2eb19f882535$7681e9c5b74bdd85e78166031d2058e1069b3ed7ed967c93fc63abba06f31144";
const RT_PASSWORD: &str = "cA773lm788buwYe4g4WT+05pKyNruVKjQ25x3n0DQcM=";

/// `rpc_users`' `test_auth`: right, wrong password, wrong user, both wrong.
fn test_auth(port: u16, user: &str, password: &str) {
    let status = |u: &str, p: &str| rpc(port, &format!("{u}:{p}"), "uptime", "[]").status;
    assert_eq!(status(user, password), 200);
    assert_eq!(status(user, &format!("{password}wrong")), 401);
    assert_eq!(status(&format!("{user}wrong"), password), 401);
    assert_eq!(status(&format!("{user}wrong"), &format!("{password}wrong")), 401);
}

/// `rpc_users`: `-rpcauth` next to the cookie, `-rpcuser`/`-rpcpassword` (non-ASCII), and the
/// init failures of a malformed `-rpcauth` and of an unwritable cookie.
#[test]
fn rpc_users() {
    let (home, datadir) = home_and_datadir();
    std::fs::write(datadir.join("dash.conf"), format!("rpcauth={RT_RPCAUTH}\n")).unwrap();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let cookie_text = cookie(&datadir);
    let (user, password) = cookie_text.split_once(':').unwrap();
    test_auth(node.port, user, password);
    test_auth(node.port, "rt", RT_PASSWORD);
    // A rejected attempt is logged and answered after a delay.
    let log = debug_log(&datadir);
    let started = Instant::now();
    let text = logged(&log, || assert_eq!(rpc(node.port, "rt:x", "uptime", "[]").status, 401));
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(text.contains("ThreadRPCServer incorrect password attempt from 127.0.0.1:"), "{text}");
    node.stop();

    let (home1, datadir1) = home_and_datadir();
    std::fs::write(datadir1.join("dash.conf"), "rpcuser=rpcuser💻\nrpcpassword=rpcpassword🔑\n")
        .unwrap();
    let node1 = RpcNode::start(home1.path(), &datadir1, &[]);
    test_auth(node1.port, "rpcuser💻", "rpcpassword🔑");
    assert!(!net(&datadir1).join(".cookie").exists());
    let reply = rpc(node1.port, "rpcuser💻:rpcpassword🔑", "stop", "[]");
    assert_eq!(reply.result(), "\"rdashd stopping\"");
    assert_eq!(node1.node.wait().status.code(), Some(0));

    // Empty -rpcauth= values are ignored.
    RpcNode::start(home.path(), &datadir, &["-rpcauth="]).stop();
    for bad in ["foo", "foo:bar", "foo:bar:baz", "foo$bar:baz", "foo$bar$baz"] {
        let auth = format!("-rpcauth={bad}");
        let out = run_regtest(home.path(), &datadir, &["-printtoconsole=0", &auth]);
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert_eq!(stderr(&out), HTTP_START_FAILED, "{bad}");
    }
    // A cookie that cannot be written: init fails, and nothing is left behind.
    std::fs::create_dir(net(&datadir).join(".cookie.tmp")).unwrap();
    let out = run_regtest(home.path(), &datadir, &["-printtoconsole=0"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out), HTTP_START_FAILED);
    assert!(!net(&datadir).join(".cookie").exists());
    let log = read(&debug_log(&datadir));
    assert!(log.contains("Unable to open cookie authentication file"), "{log}");
    assert_shut_down_in_order(&log);
}

/// `rpc_whitelist`'s users: name, `-rpcauth` hash, whitelist, password.
const WHITELIST_USERS: [[&str; 4]; 4] = [
    [
        "user1",
        "50358aa884c841648e0700b073c32b2e$b73e95fff0748cc0b517859d2ca47d9bac1aa78231f3e48fa9222b612bd2083e",
        "getrpcinfo,uptime,",
        "12345",
    ],
    [
        "user2",
        "8650ba41296f62092377a38547f361de$4620db7ba063ef4e2f7249853e9f3c5c3592a9619a759e3e6f1c63f2e22f1d21",
        "uptime",
        "54321",
    ],
    [
        "platform-user",
        "8650ba41296f62092377a38547f361de$4620db7ba063ef4e2f7249853e9f3c5c3592a9619a759e3e6f1c63f2e22f1d21",
        "uptime,helpuptime",
        "54321",
    ],
    // ", " separated list yields an empty whitelist entry
    [
        "user3",
        "8650ba41296f62092377a38547f361de$4620db7ba063ef4e2f7249853e9f3c5c3592a9619a759e3e6f1c63f2e22f1d21",
        "uptime, getrpcinfo",
        "54321",
    ],
];
const STRANGE_USERS: [[&str; 4]; 6] = [
    [
        "strangedude",
        "62d67dffec03836edd698314f1b2be62$c2fb4be29bb0e3646298661123cf2d8629640979cabc268ef05ea613ab54068d",
        ":",
        "s7R4nG3R7H1nGZ",
    ],
    [
        "strangedude2",
        "575c012c7fe4b1e83b9d809412da3ef7$09f448d0acfc19924dd62ecb96004d3c2d4b91f471030dfe43c6ea64a8f658c1",
        "",
        "s7R4nG3R7H1nGZ",
    ],
    [
        "strangedude3",
        "23189c561b5975a56f4cf94030495d61$3a2f6aac26351e2257428550a553c4c1979594e36675bbd3db692442387728c0",
        ":uptime,",
        "s7R4nG3R7H1nGZ",
    ],
    [
        "strangedude4",
        "990c895760a70df83949e8278665e19a$8f0906f20431ff24cb9e7f5b5041e4943bdf2a5c02a19ef4960dcf45e72cde1c",
        ":uptime, getrpcinfo",
        "s7R4nG3R7H1nGZ",
    ],
    [
        "strangedude4",
        "990c895760a70df83949e8278665e19a$8f0906f20431ff24cb9e7f5b5041e4943bdf2a5c02a19ef4960dcf45e72cde1c",
        ":uptime",
        "s7R4nG3R7H1nGZ",
    ],
    [
        "strangedude5",
        "d12c6e962d47a454f962eb41225e6ec8$2dd39635b155536d3c1a2e95d05feff87d5ba55f2d5ff975e6e997a836b717c9",
        ":uptime,uptime",
        "s7R4nG3R7H1nGZ",
    ],
];
/// Allowed to nobody.
const NEVER_ALLOWED: [&str; 2] = ["getnetworkinfo", "quorum sign"];

/// `rpcquery`: a sub-command written without its space (`helpuptime`, `quorumsign`) is sent
/// as the method and its first argument.
fn rpcquery(method: &str) -> String {
    for parent in ["help", "quorum"] {
        if let Some(sub) = method.strip_prefix(parent).filter(|s| !s.is_empty()) {
            let sub = sub.trim_start();
            return format!(r#"{{"method":"{parent}","params":["{sub}"]}}"#);
        }
    }
    format!(r#"{{"method":"{method}","params":[]}}"#)
}

fn whitelist_call(port: u16, user: &[&str; 4], methods: &[&str], batch: bool) -> u16 {
    let auth = format!("{}:{}", user[0], user[3]);
    let calls: Vec<String> = methods.iter().map(|m| rpcquery(m)).collect();
    let body = if batch {
        format!("[{}]", calls.join(","))
    } else {
        calls[0].clone()
    };
    post(port, "/", Some(&auth), &body).status
}

/// `rpc_whitelist`: permitted calls answer, others are refused with 403, alone or in a batch
/// (DEC-82: v24 checks every call of a batch).
#[test]
fn rpc_whitelist() {
    let (home, datadir) = home_and_datadir();
    let mut conf = String::from("rpcwhitelistdefault=0\n");
    for user in WHITELIST_USERS {
        conf += &format!("rpcauth={}:{}\nrpcwhitelist={}:{}\n", user[0], user[1], user[0], user[2]);
    }
    for user in STRANGE_USERS {
        conf += &format!("rpcauth={}:{}\nrpcwhitelist={}{}\n", user[0], user[1], user[0], user[2]);
    }
    std::fs::write(datadir.join("dash.conf"), conf).unwrap();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let port = node.port;
    for user in &WHITELIST_USERS {
        let permissions: Vec<String> = user[2]
            .replace(' ', "")
            .split(',')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        let permissions: Vec<&str> = permissions.iter().map(String::as_str).collect();
        for permission in &permissions {
            assert_eq!(
                whitelist_call(port, user, &[permission], false),
                200,
                "{} {permission}",
                user[0]
            );
        }
        for permission in NEVER_ALLOWED {
            assert_eq!(
                whitelist_call(port, user, &[permission], false),
                403,
                "{} {permission}",
                user[0]
            );
        }
        assert_eq!(whitelist_call(port, user, &permissions, true), 200, "{}", user[0]);
        for permission in NEVER_ALLOWED {
            assert_eq!(whitelist_call(port, user, &[permission], true), 403);
            let with = [permissions.as_slice(), &[permission]].concat();
            assert_eq!(whitelist_call(port, user, &with, true), 403);
        }
    }
    for permission in NEVER_ALLOWED {
        assert_eq!(whitelist_call(port, &STRANGE_USERS[0], &[permission], false), 403);
        assert_eq!(whitelist_call(port, &STRANGE_USERS[1], &[permission], false), 403);
    }
    assert_eq!(whitelist_call(port, &STRANGE_USERS[2], &["uptime"], false), 200);
    assert_eq!(whitelist_call(port, &STRANGE_USERS[3], &["getrpcinfo"], false), 403);
    assert_eq!(whitelist_call(port, &STRANGE_USERS[4], &["uptime"], false), 200);
    // A batch element that is not an object is refused before any call runs.
    let auth = format!("{}:{}", WHITELIST_USERS[0][0], WHITELIST_USERS[0][3]);
    let reply = post(port, "/", Some(&auth), r#"[{"method":"uptime"},5]"#);
    assert_eq!(
        (reply.status, reply.body.as_str()),
        (
            400,
            "{\"result\":null,\"error\":{\"code\":-32600,\"message\":\"Invalid Request object\"},\"id\":null}\n"
        )
    );
    drop(node.node.stop_ok(libc::SIGTERM));

    // With -rpcwhitelistdefault, a user without a whitelist may call nothing.
    let (home, datadir) = home_and_datadir();
    let conf = format!("rpcauth={RT_RPCAUTH}\nrpcwhitelist=user1:uptime\n");
    std::fs::write(datadir.join("dash.conf"), conf).unwrap();
    let node = RpcNode::start(home.path(), &datadir, &["-debug=http"]);
    let reply = rpc(node.port, &format!("rt:{RT_PASSWORD}"), "uptime", "[]");
    assert_eq!((reply.status, reply.body.as_str()), (403, ""));
    drop(node.node.stop_ok(libc::SIGTERM));
}

/// `rpc_named_arguments`, with `echo`'s cases (the `getblockhash`/`getblock` calls are RP1a's).
#[test]
fn rpc_named_arguments() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let h = node.call("help", r#"{"command":"getblockchaininfo"}"#).result().to_owned();
    assert!(h.starts_with(r#""getblockchaininfo\n"#), "{h}");
    assert_eq!(
        node.call("help", r#"{"random":"getblockchaininfo"}"#).error(),
        (500, r#"{"code":-8,"message":"Unknown named parameter random"}"#)
    );
    let echo = |params: &str| node.call("echo", params).result().to_owned();
    assert_eq!(echo("{}"), "[]");
    assert_eq!(echo(r#"{"arg0":0,"arg9":9}"#), "[0,null,null,null,null,null,null,null,null,9]");
    assert_eq!(echo(r#"{"arg1":1}"#), "[null,1]");
    assert_eq!(echo(r#"{"arg9":null}"#), "[null,null,null,null,null,null,null,null,null,null]");
    assert_eq!(
        echo(r#"{"arg0":0,"arg3":3,"arg9":9}"#),
        "[0,null,null,3,null,null,null,null,null,9]"
    );
    assert_eq!(echo(r#"{"args":[0,1],"arg3":3,"arg5":5}"#), "[0,1,null,3,null,5]");
    for params in [r#"{"args":[0,1],"arg1":1}"#, r#"{"args":[0,null,2],"arg1":1}"#] {
        assert_eq!(
            node.call("echo", params).error(),
            (
                500,
                r#"{"code":-8,"message":"Parameter arg1 specified twice both as positional and named argument"}"#
            )
        );
    }
    node.stop();
}

/// `rpc_help`: wrong argument count and type, an unknown command, the categories, every
/// command's help, and the conversion table's consistency.
#[test]
fn rpc_help() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let reply = node.call("help", r#"["foo","bar","foobar"]"#);
    let (status, err) = reply.error();
    assert_eq!(status, 500);
    assert!(err.starts_with(r#"{"code":-1,"message":"help "#), "{err}");
    let reply = node.call("help", "[0]");
    let (_, err) = reply.error();
    assert!(err.starts_with(r#"{"code":-3,"#), "{err}");
    assert!(err.contains("JSON value of type number is not of expected type string"), "{err}");
    assert_eq!(node.call("help", r#"["foo"]"#).result(), r#""help: unknown command: foo""#);

    let overview = unescape_json_string(node.call("help", "[]").result());
    let titles: Vec<&str> = overview
        .lines()
        .filter_map(|l| l.strip_prefix("== ").and_then(|l| l.strip_suffix(" ==")))
        .collect();
    // What dashd v24's release build lists with -disablewallet (rdashd has no wallet yet).
    let components = [
        "Addressindex",
        "Blockchain",
        "Control",
        "Dash",
        "Evo",
        "Mining",
        "Network",
        "Rawtransactions",
        "Signer",
        "Util",
        "Zmq",
    ];
    assert_eq!(titles, components);
    for call in overview.lines().filter(|l| !l.is_empty() && !l.starts_with("==")) {
        let name = call.split(' ').next().unwrap();
        let text = unescape_json_string(node.call("help", &format!("[\"{name}\"]")).result());
        assert!(text.starts_with(name), "{name}: {text}");
    }

    // Arguments of the same name convert the same way (`dummy` excepted).
    let map = node.call("help", r#"["dump_all_command_conversions"]"#).result().to_owned();
    let mut by_name = std::collections::BTreeMap::<String, Vec<bool>>::new();
    for row in map.trim_start_matches("[[").trim_end_matches("]]").split("],[") {
        let cells: Vec<&str> = row.split(',').collect();
        let name = cells[2].trim_matches('"').to_owned();
        by_name.entry(name).or_default().push(cells[3] == "true");
    }
    for (name, converts) in by_name {
        let all = converts.iter().all(|c| *c);
        let any = converts.iter().any(|c| *c);
        assert!(all == any || name == "dummy", "conversion mismatch for {name}");
    }
    node.stop();
}

/// `rpc_bind`'s loopback cases: what is bound for each `-rpcbind`/`-rpcallowip` combination,
/// seen in the log and by connecting. The non-loopback cases (a public interface, and a client
/// outside `-rpcallowip`) are D-rows (RP0-D9); the allow list's matching is unit-tested.
#[test]
fn rpc_bind_loopback() {
    let (home, datadir) = home_and_datadir();
    // -rpcallowip values, -rpcbind values, the addresses bound (on the node's -rpcport).
    let cases: [(&[&str], &[&str], &[&str]); 5] = [
        (&[], &[], &["127.0.0.1", "::1"]),
        (&["127.0.0.1"], &[], &["127.0.0.1", "::1"]),
        (&[], &["127.0.0.1"], &["127.0.0.1", "::1"]),
        (&["[::1]"], &["[::1]"], &["::1"]),
        (&["127.0.0.1"], &["127.0.0.1", "[::1]"], &["127.0.0.1", "::1"]),
    ];
    for (allow, bind, expected) in cases {
        let mut args: Vec<String> = allow.iter().map(|a| format!("-rpcallowip={a}")).collect();
        args.extend(bind.iter().map(|b| format!("-rpcbind={b}")));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let log = debug_log(&datadir);
        let mut node = None;
        let text = logged(&log, || node = Some(RpcNode::start(home.path(), &datadir, &args)));
        let node = node.unwrap();
        let mut bound: Vec<&str> = text
            .lines()
            .filter_map(|l| l.split_once("Binding RPC on address ").map(|(_, r)| r))
            .map(|r| r.split(" port ").next().unwrap())
            .collect();
        bound.sort_unstable();
        let mut want = expected.to_vec();
        want.sort_unstable();
        assert_eq!(bound, want, "{args:?}");
        for ip in expected {
            let addr: std::net::IpAddr = ip.parse().unwrap();
            assert!(TcpStream::connect((addr, node.port)).is_ok(), "{ip} {args:?}");
        }
        if expected == ["::1"] {
            assert!(TcpStream::connect(("127.0.0.1", node.port)).is_err());
        }
        if !bind.is_empty() && allow.is_empty() {
            assert!(text.contains("WARNING: option -rpcbind was ignored because -rpcallowip was not specified, refusing to allow everyone to connect"));
        }
        if !allow.is_empty() && bind.is_empty() {
            assert!(text.contains("WARNING: option -rpcallowip was specified without -rpcbind; this doesn't usually make sense"));
        }
        // RPC `stop` would need 127.0.0.1, which the `[::1]` case does not bind.
        node.node.stop_ok(libc::SIGTERM);
    }
    // A port given with -rpcbind.
    let port = free_port();
    let bind = format!("-rpcbind=127.0.0.1:{port}");
    let node = RpcNode::start(home.path(), &datadir, &["-rpcallowip=127.0.0.1", &bind]);
    assert_eq!(rpc(port, &cookie(&datadir), "uptime", "[]").status, 200);
    // Nothing listens on -rpcport now: stop through the bound port.
    assert_eq!(rpc(port, &cookie(&datadir), "stop", "[]").result(), r#""rdashd stopping""#);
    assert_eq!(node.node.wait().status.code(), Some(0));
    // A subnet that does not parse: the message box, then the init error.
    let out = run_regtest(home.path(), &datadir, &["-printtoconsole=0", "-rpcallowip=1.2.3.4/33"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!(
            "Error: Invalid -rpcallowip subnet specification: 1.2.3.4/33. Valid are a single IP (e.g. 1.2.3.4), a network/netmask (e.g. 1.2.3.4/255.255.255.0) or a network/CIDR (e.g. 1.2.3.4/24).\n{HTTP_START_FAILED}"
        )
    );
}

/// `rpc_external_queue`: requests of `-rpcexternaluser` users run from the second queue, which
/// logs "HTTP: Calling handler for external user"; other users' requests never do. Core's test
/// also sets `-masternodeblsprivkey`, which only makes its node a masternode.
#[test]
fn rpc_external_queue() {
    let (home, datadir) = home_and_datadir();
    let conf = "rpcauth=platform-user:dd88fd676186f48553775d6fb5a2d344$bc1f7898698ead19c6ec7ff47055622dd7101478f1ff6444103d3dc03cd77c13\n\
                rpcauth=operator:e9b45dd0b61a7be72155535435365a3a$8fb7470bc6f74d8ceaf9a23f49b06127723bd563b3ed5d9cea776ef01803d191\n";
    std::fs::write(datadir.join("dash.conf"), conf).unwrap();
    let (platform, operator) = ("platform-user:password123", "operator:otherpassword");
    let external = "HTTP: Calling handler for external user";
    let always = "ThreadRPCServer method=";
    let log = debug_log(&datadir);
    let call = |node: &RpcNode, auth: &str| {
        logged(&log, || assert_eq!(rpc(node.port, auth, "getrpcinfo", "[]").status, 200))
    };

    let node = RpcNode::start(home.path(), &datadir, &["-debug=rpc"]);
    assert_eq!(rpc(node.port, "platform-user:rpcpasswordwrong", "getrpcinfo", "[]").status, 401);
    for auth in [platform, operator] {
        let text = call(&node, auth);
        assert!(text.contains(always) && !text.contains(external), "{text}");
    }
    node.stop();

    let node =
        RpcNode::start(home.path(), &datadir, &["-debug=rpc", "-rpcexternaluser=platform-user"]);
    let text = call(&node, platform);
    assert!(text.contains(always) && text.contains(external), "{text}");
    let text = call(&node, operator);
    assert!(text.contains(always) && !text.contains(external), "{text}");
    node.stop();

    let node = RpcNode::start(
        home.path(),
        &datadir,
        &["-debug=rpc", "-rpcexternaluser=platform-user,operator"],
    );
    for auth in [platform, operator] {
        let text = call(&node, auth);
        assert!(text.contains(always) && text.contains(external), "{text}");
    }
    node.stop();
}

/// `feature_init`'s RPC line, the cookie's life, warmup, and `-server=0`.
#[test]
fn the_server_starts_with_init_and_stops_with_shutdown() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let log = read(&debug_log(&datadir));
    // feature_init waits for this line before it interrupts init.
    assert!(log.contains("Starting HTTP server with 4 worker threads"), "{log}");
    let cookie_text = cookie(&datadir);
    assert!(cookie_text.starts_with("__cookie__:") && cookie_text.len() == 11 + 64);
    // Everything about the order is Core's: servers in step 4a, warmup over before "Done loading".
    let at = |needle: &str| log.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("Binding RPC on address") < at("Starting HTTP server"));
    assert!(at("Starting HTTP server") < at("init message: Loading block index"));
    let out = node.stop();
    drop(out);
    assert!(!net(&datadir).join(".cookie").exists());
    let log = read(&debug_log(&datadir));
    assert_shut_down_in_order(&log);

    // -server=0: no server, no cookie.
    let node = start(home.path(), &datadir, &["-server=0"]);
    let log = read(&debug_log(&datadir));
    assert_eq!(log.matches("Starting HTTP server").count(), 1);
    assert!(!net(&datadir).join(".cookie").exists());
    node.stop_ok(libc::SIGTERM);
}

/// The JSON-RPC path's own answers: methods, credentials, bodies, `/wallet/<name>`.
#[test]
fn json_rpc_answers() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[]);
    let auth = cookie(&datadir);
    let raw = |request: String| Reply::parse(&http_raw(node.port, request.as_bytes()));
    let reply =
        raw(format!("GET / HTTP/1.1\r\nAuthorization: Basic {}\r\n\r\n", base64(auth.as_bytes())));
    assert_eq!(
        (reply.status, reply.body.as_str()),
        (405, "JSONRPC server handles only POST requests")
    );
    let reply = post(node.port, "/", None, "{}");
    assert_eq!(
        (reply.status, reply.header("WWW-Authenticate")),
        (401, Some("Basic realm=\"jsonrpc\""))
    );
    for (body, status, message) in [
        ("", 500, r#"{"code":-32700,"message":"Parse error"}"#),
        ("5", 500, r#"{"code":-32700,"message":"Top-level object parse error"}"#),
        (r#"{"id":5}"#, 400, r#"{"code":-32600,"message":"Missing method"}"#),
    ] {
        let reply = post(node.port, "/", Some(&auth), body);
        assert_eq!(reply.status, status, "{body}");
        assert!(reply.body.contains(message), "{body}: {}", reply.body);
    }
    let reply =
        post(node.port, "/wallet/w1", Some(&auth), r#"{"method":"echo","params":["w"],"id":1}"#);
    assert_eq!(reply.result(), r#"["w"]"#);
    assert_eq!(post(node.port, "/wallet", Some(&auth), "{}").status, 404);
    node.stop();
}

/// Batches and the reply's id, with a barrier-free check that requests run in parallel workers.
#[test]
fn requests_run_on_parallel_workers() {
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &["-rpcthreads=4"]);
    let auth = cookie(&datadir);
    let start = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let (start, auth, port) = (Arc::clone(&start), auth.clone(), node.port);
            std::thread::spawn(move || {
                start.wait();
                for _ in 0..20 {
                    let body = format!(r#"{{"method":"echo","params":[{i}],"id":{i}}}"#);
                    let reply = post(port, "/", Some(&auth), &body);
                    assert_eq!(
                        reply.body,
                        format!("{{\"result\":[{i}],\"error\":null,\"id\":{i}}}\n")
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    node.stop();
}

/// `uptime` subtracts the real start time from the mockable clock, as Core's static
/// `nStartupTime` is taken before `-mocktime` applies.
#[test]
fn uptime_counts_from_the_real_start_under_mocktime() {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        as i64;
    let mocktime = format!("-mocktime={}", now - 1000);
    let (home, datadir) = home_and_datadir();
    let node = RpcNode::start(home.path(), &datadir, &[&mocktime]);
    let uptime: i64 = node.call("uptime", "[]").result().parse().unwrap();
    // The node started at or after `now`, and the test took less than PATIENCE.
    assert!((-1000 - PATIENCE.as_secs() as i64..=-1000).contains(&uptime), "{uptime}");
    node.stop();
}

/// The names of process `pid`'s threads.
#[cfg(target_os = "linux")]
fn thread_names(pid: u32) -> Vec<String> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .map(|name| name.trim_end().to_owned())
        .collect()
}

/// A start that loses its port to another process (BL-221) runs again on the next port, and the
/// debug log keeps only the start that succeeded: on a new datadir, and on one an earlier run
/// left a log in.
#[test]
fn a_start_that_loses_its_port_retries_on_another() {
    let (home, datadir) = home_and_datadir();
    for run in 0..2 {
        let before = std::fs::read_to_string(debug_log(&datadir)).unwrap_or_default();
        let taken = free_port();
        let _v4 = TcpListener::bind(("127.0.0.1", taken)).unwrap();
        let _v6 = TcpListener::bind(("::1", taken));
        let mut ports = [taken, free_port(), free_port()].into_iter();
        let (node, port) = start_on_free_port(
            &datadir,
            || ports.next().unwrap(),
            |port| {
                let rpcport = format!("-rpcport={port}");
                regtest(home.path(), &datadir, &["-printtoconsole=0", &rpcport])
            },
        );
        assert_ne!(port, taken);
        let log = read(&debug_log(&datadir));
        let new = log.strip_prefix(before.as_str()).expect("the earlier run's lines kept");
        assert!(!new.contains(&format!("port {taken}")), "run {run}: {new}");
        assert_eq!(new.matches("init message: Done loading").count(), 1, "run {run}: {new}");
        assert_eq!(rpc(port, &cookie(&datadir), "stop", "[]").result(), r#""rdashd stopping""#);
        assert_eq!(node.wait().status.code(), Some(0));
    }
}

/// Idle and half-sent connections hold no thread: one event thread serves every connection, as
/// libevent's does in Core (RP0-D7). With no idle timeout, 500 idle connections and 100 stalled
/// mid-header leave the thread count where it was, and authenticated calls are still answered:
/// `getmempoolinfo`, which MP1a serves, as a missing method, and `uptime`.
#[cfg(target_os = "linux")]
#[test]
fn idle_connections_hold_no_threads() {
    const RPC_THREADS: usize = 4;
    let (home, datadir) = home_and_datadir();
    let threads_arg = format!("-rpcthreads={RPC_THREADS}");
    let node = RpcNode::start(home.path(), &datadir, &["-rpcservertimeout=0", &threads_arg]);
    let pid = node.node.id();
    assert_eq!(node.call("uptime", "[]").status, 200);
    // A thread takes its name once it runs, so a worker spawned moments ago may still carry the
    // process's (BL-188): count once every server thread has named itself.
    let server_threads = |names: &[String]| {
        names.iter().filter(|n| *n == "http" || n.starts_with("httpworker.")).count()
    };
    let deadline = Instant::now() + PATIENCE;
    let names = loop {
        let names = thread_names(pid);
        if server_threads(&names) == RPC_THREADS + 1 || Instant::now() >= deadline {
            break names;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(server_threads(&names), RPC_THREADS + 1, "{names:?}");
    let before = names.len();
    let held: Vec<TcpStream> = (0..600)
        .map(|i| {
            let mut stream = TcpStream::connect(("127.0.0.1", node.port)).unwrap();
            if i >= 500 {
                stream.write_all(b"POST / HTTP/1.1\r\nHost: x\r\nAuthoriz").unwrap();
            }
            stream
        })
        .collect();
    // Answered after every connection above was accepted.
    assert_eq!(
        node.call("getmempoolinfo", "[]").error(),
        (404, r#"{"code":-32601,"message":"Method not found"}"#)
    );
    assert_eq!(node.call("uptime", "[]").status, 200);
    let names = thread_names(pid);
    let server = server_threads(&names);
    eprintln!(
        "threads: {before} before, {} with {} connections held, {server} of them the server's \
         (-rpcthreads={RPC_THREADS})",
        names.len(),
        held.len()
    );
    // The server's threads are its workers and the event thread; the rest are the node's.
    assert_eq!(server, RPC_THREADS + 1, "{names:?}");
    assert!(names.len() <= before + 1, "{names:?} with {} connections held", held.len());
    // Closing them all, half-sent requests included, leaves the server answering.
    drop(held);
    assert_eq!(node.call("uptime", "[]").status, 200);
    node.stop();
}
