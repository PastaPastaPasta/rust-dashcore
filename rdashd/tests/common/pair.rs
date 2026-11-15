use std::io::{ErrorKind, Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

use super::recorded::RecordedRun;
use super::{
    PATIENCE, RDASHD, Running, debug_log, free_port, read_retrying, regtest_on, require_dashd,
    spawn_until_loaded,
};
use dash_args::univalue::SettingsValue as UniValue;

/// A POST of `body` to `path` with `headers` (each ending in CRLF).
pub fn post_with(path: &str, version: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut request = format!(
        "POST {path} {version}\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

pub fn post(body: &str) -> Vec<u8> {
    post_with("/", "HTTP/1.1", AUTH, body.as_bytes())
}

/// The reply bytes to `request`: the header block with its `Date` header's value masked, the body
/// read through `Content-Length` (libevent sends a body to `HEAD` too), or to the close when no
/// length is sent, then `<closed>` if the server closes within the watch. A reply that ends
/// early, a read error, or more bytes after the body fail the test; no bytes at all, then a close,
/// is `<closed>`.
pub fn exchange(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    stream.write_all(request).unwrap();
    let name = String::from_utf8_lossy(&request[..request.len().min(40)]).into_owned();
    let read = |stream: &mut TcpStream, buf: &mut [u8]| {
        read_retrying(stream, buf)
            .unwrap_or_else(|e| panic!("{name:?}: reading the reply: {e} ({:?})", e.kind()))
    };
    let mut buf = [0; 64 * 1024];
    let mut out = Vec::new();
    let header_end = loop {
        if let Some(i) = out.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match read(&mut stream, &mut buf) {
            0 if out.is_empty() => return b"<closed>".to_vec(),
            0 => {
                panic!("{name:?}: closed inside the headers: {:?}", out.escape_ascii().to_string())
            }
            n => out.extend_from_slice(&buf[..n]),
        }
    };
    let mut body = out.split_off(header_end);
    let headers = mask_date(&out);
    let length = std::str::from_utf8(&headers).unwrap().split("\r\n").find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().unwrap())
    });
    let announces_close = std::str::from_utf8(&headers).unwrap().split("\r\n").any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.eq_ignore_ascii_case("connection") && v.trim().eq_ignore_ascii_case("close")
        })
    });
    let mut closed = false;
    match length {
        Some(length) => {
            while body.len() < length {
                match read(&mut stream, &mut buf) {
                    0 => panic!("{name:?}: closed {} bytes into a {length}-byte body", body.len()),
                    n => body.extend_from_slice(&buf[..n]),
                }
            }
            assert_eq!(body.len(), length, "{name:?}: bytes after the body");
        }
        None => loop {
            match read(&mut stream, &mut buf) {
                0 => break closed = true,
                n => body.extend_from_slice(&buf[..n]),
            }
        },
    }
    if !closed {
        // A deadline, not a per-read timeout: a signal's retry must not stretch the watch.
        let deadline = Instant::now()
            + if announces_close {
                ANNOUNCED_CLOSE_WATCH
            } else {
                CLOSE_WATCH
            };
        closed = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break false;
            }
            stream.set_read_timeout(Some(left)).unwrap();
            match stream.read(&mut buf) {
                Ok(0) => break true,
                Ok(n) => panic!("{name:?}: {n} bytes after the reply"),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    break false;
                }
                Err(e) => panic!("{name:?}: watching for the close: {e} ({:?})", e.kind()),
            }
        };
    }
    let mut reply = headers;
    reply.extend(body);
    if closed {
        reply.extend_from_slice(b"<closed>");
    }
    reply
}

/// `headers` (a reply's header block) with the `Date` header's value replaced by `X`: its name's
/// case and its line ending stay as sent, so they are compared too.
pub fn mask_date(headers: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in headers.split_inclusive(|b| *b == b'\n') {
        if line.get(..6).is_some_and(|p| p.eq_ignore_ascii_case(b"date: ")) {
            let end =
                line.len() - line.iter().rev().take_while(|b| matches!(b, b'\r' | b'\n')).count();
            out.extend_from_slice(&line[..6]);
            out.push(b'X');
            out.extend_from_slice(&line[end..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// A node from `cmd` with `u:p` credentials, until init is done.
pub fn node(cmd: std::process::Command, datadir: &Path) -> Running {
    spawn_until_loaded(cmd, debug_log(datadir))
}

/// A JSON-RPC call of `method` with `params` (JSON text).
pub fn call(method: &str, params: &str) -> Vec<u8> {
    post(&format!(r#"{{"method":"{method}","params":{params},"id":1}}"#))
}

/// `reply` with its JSON-RPC body's `"warnings":"…"` emptied and `Content-Length` to match: dashd
/// rc.4 warns of its pre-release build, where rdashd, which reports its pin as a release
/// (BL-175), has nothing to say ([`IDENTITY`]). The comparison then holds rdashd's to "".
pub fn empty_warnings(reply: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(reply);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return reply.to_vec();
    };
    let Some((before, value)) = body.split_once(r#""warnings":""#) else {
        return reply.to_vec();
    };
    // The string's closing quote: the first one no backslash escapes.
    let bytes = value.as_bytes();
    let mut end = 0;
    while bytes[end] != b'"' {
        end += if bytes[end] == b'\\' {
            2
        } else {
            1
        };
    }
    let body = format!(r#"{before}"warnings":""{}"#, &value[end + 1..]);
    let head: Vec<_> = head
        .split("\r\n")
        .map(|l| match l.split_once(": ") {
            Some((name, _)) if name.eq_ignore_ascii_case("content-length") => {
                format!("{name}: {}", body.len())
            }
            _ => l.to_owned(),
        })
        .collect();
    format!("{}\r\n\r\n{body}", head.join("\r\n")).into_bytes()
}

/// One JSON-RPC batch of `(name, method, params)` calls sent to dashd (`ports.0`) and rdashd
/// (`ports.1`), the replies compared byte for byte with the `Date` header masked; a differing
/// batch is reported per call where the replies parse. Returns dashd's results, in order.
pub fn diff_batch(
    ports: (u16, u16),
    calls: &[(String, &str, String)],
    failures: &mut Vec<String>,
) -> Vec<UniValue> {
    let body: Vec<String> = calls
        .iter()
        .enumerate()
        .map(|(i, (_, method, params))| {
            format!(r#"{{"method":"{method}","params":{params},"id":{i}}}"#)
        })
        .collect();
    let request = post(&format!("[{}]", body.join(",")));
    let [theirs, mine] = [ports.0, ports.1].map(|port| exchange(port, &request));
    let show = |b: &[u8]| b.escape_ascii().to_string();
    assert!(theirs.starts_with(b"HTTP/"), "{}: dashd answered {}", calls[0].0, show(&theirs));
    let replies = |bytes: &[u8]| -> Vec<UniValue> {
        let text = std::str::from_utf8(bytes).unwrap();
        let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        match UniValue::read(body.as_bytes()) {
            Some(UniValue::Arr(items)) => items,
            _ => Vec::new(),
        }
    };
    let (core_replies, our_replies) = (replies(&theirs), replies(&mine));
    if theirs != mine {
        let mut named = false;
        for (i, (name, _, _)) in calls.iter().enumerate() {
            let [a, b] = [&core_replies, &our_replies].map(|r| r.get(i).map(|v| v.write()));
            if a != b {
                failures.push(format!("{name}:\n  dashd  {a:?}\n  rdashd {b:?}"));
                named = true;
            }
        }
        if !named {
            failures.push(format!(
                "{} (whole batch):\n  dashd  {}\n  rdashd {}",
                calls[0].0,
                show(&theirs),
                show(&mine)
            ));
        }
    }
    core_replies
        .into_iter()
        .map(|reply| match reply {
            UniValue::Obj(fields) => {
                fields.into_iter().find(|(k, _)| k == "result").map(|(_, v)| v).unwrap()
            }
            other => panic!("{}", other.write()),
        })
        .collect()
}

/// Submits the run's blocks at `heights` to both nodes in batches; dashd must connect each.
pub fn replay(pair: &mut Pair, run: &RecordedRun, heights: std::ops::RangeInclusive<usize>) {
    let calls: Vec<_> = heights
        .map(|h| {
            (format!("submitblock_{h}"), "submitblock", format!(r#"["{}"]"#, run.blocks[h - 1]))
        })
        .collect();
    let results = diff_chunks(pair, &calls);
    assert!(results.iter().all(|r| *r == UniValue::Null), "dashd connected {}", calls[0].0);
}

/// Both nodes' tip is the run's block at `height`.
pub fn assert_tips(pair: &Pair, run: &RecordedRun, height: usize) {
    for port in pair.ports {
        let best = field(&reply_on(port, "getbestblockhash", "[]"), "result");
        assert_eq!(best.get_str().unwrap(), run.hash(height), "port {port}");
        let count = field(&reply_on(port, "getblockcount", "[]"), "result");
        assert_eq!(count.write(), height.to_string(), "port {port}");
    }
}

/// [`diff_batch`] in chunks of 25 calls, counted on `pair`.
pub fn diff_chunks(pair: &mut Pair, calls: &[(String, &str, String)]) -> Vec<UniValue> {
    let ports = (pair.ports[0], pair.ports[1]);
    let mut results = Vec::new();
    for chunk in calls.chunks(25) {
        pair.count += chunk.len();
        results.extend(diff_batch(ports, chunk, &mut pair.failures));
    }
    results
}

/// One call whose reply on both nodes must be an internal-bug error: the code, the message's
/// first line and the function its second line names compared, as the rest names each node's
/// own package and source (RP1a-notes RP0-D5).
pub fn both_internal_bug(pair: &mut Pair, name: &str, method: &str, params: &str) {
    pair.count += 1;
    let request = call(method, params);
    let [theirs, mine] = pair.ports.map(|port| {
        let reply = exchange(port, &request);
        let text = String::from_utf8(reply).unwrap();
        let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
        let UniValue::Obj(fields) = UniValue::read(body.as_bytes()).unwrap() else {
            panic!("{body}")
        };
        let error = fields.into_iter().find(|(k, _)| k == "error").unwrap().1;
        let UniValue::Obj(error) = error else {
            // Not an error: the whole body, so a missing check shows as a difference.
            return (String::new(), body);
        };
        let get = |k: &str| error.iter().find(|(key, _)| key == k).unwrap().1.write();
        let message = get("message");
        let mut lines = message.split("\\n");
        let check = lines.next().unwrap().to_owned();
        let func = lines.next().and_then(|l| l.rsplit_once(" (")).map(|(_, f)| f.to_owned());
        (get("code"), format!("{check} {}", func.unwrap_or_default()))
    });
    // dashd must fail the check too, or the case tests nothing.
    if theirs != mine || !theirs.1.starts_with("\"Internal bug detected") {
        pair.failures.push(format!("{name}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
    }
}

/// `v`'s member `key`.
pub fn field(v: &UniValue, key: &str) -> UniValue {
    let UniValue::Obj(fields) = v else {
        panic!("{}", v.write())
    };
    fields
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("{key} in {}", v.write()))
        .1
        .clone()
}

/// `method(params)` on one node: its reply body as JSON.
pub fn reply_on(port: u16, method: &str, params: &str) -> UniValue {
    let reply = String::from_utf8(exchange(port, &call(method, params))).unwrap();
    let body = reply.split_once("\r\n\r\n").unwrap().1;
    UniValue::read(body.as_bytes()).unwrap_or_else(|| panic!("{reply}"))
}

/// The run's block at `height` as a one-hash parameter list.
pub fn hash_param(run: &RecordedRun, height: usize) -> String {
    format!(r#"["{}"]"#, run.hash(height))
}

/// A pair of fresh regtest nodes, dashd (with `-disablewallet`) and rdashd, each with `extra`.
/// [`Pair::both`] sends one call to both and records a differing reply; [`Pair::finish`] stops
/// them and fails on any.
pub struct Pair {
    pub ports: [u16; 2],
    nodes: Vec<Running>,
    home: tempfile::TempDir,
    extra: Vec<String>,
    pub failures: Vec<String>,
    pub count: usize,
    pub replies: std::collections::HashMap<String, String>,
}

impl Pair {
    pub fn start(extra: &[&str]) -> Pair {
        let home = tempfile::tempdir().unwrap();
        let (nodes, ports) = Pair::spawn(home.path(), extra);
        Pair {
            ports,
            nodes,
            home,
            extra: extra.iter().map(|a| (*a).to_owned()).collect(),
            failures: Vec::new(),
            count: 0,
            replies: Default::default(),
        }
    }

    /// dashd, then rdashd, on their datadirs under `home` with `extra`, each on a free RPC port.
    fn spawn(home: &Path, extra: &[&str]) -> (Vec<Running>, [u16; 2]) {
        let dashd = require_dashd();
        let start = |binary: &Path, name: &str, own: &[&str]| {
            let datadir = home.join(name);
            std::fs::create_dir_all(&datadir).unwrap();
            let port = free_port();
            let rpcport = format!("-rpcport={port}");
            let args = [
                &["-printtoconsole=0", "-listen=0", &rpcport, "-rpcuser=u", "-rpcpassword=p"][..],
                own,
                extra,
            ]
            .concat();
            (node(regtest_on(binary, home, &datadir, &args), &datadir), port)
        };
        let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
        let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
        (vec![core, ours], [core_port, our_port])
    }

    /// Stops both nodes cleanly and starts them again on the same datadirs and arguments.
    pub fn restart(&mut self) {
        for node in self.nodes.drain(..) {
            node.stop_ok(libc::SIGTERM);
        }
        let extra: Vec<&str> = self.extra.iter().map(String::as_str).collect();
        (self.nodes, self.ports) = Pair::spawn(self.home.path(), &extra);
    }

    /// One call to both nodes, warnings emptied; dashd's body is kept for [`Pair::result`] and
    /// [`Pair::finish`].
    pub fn both(&mut self, name: &str, method: &str, params: &str) {
        self.count += 1;
        let request = call(method, params);
        let [theirs, mine] = self.ports.map(|port| empty_warnings(&exchange(port, &request)));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            self.failures.push(format!(
                "{name}:\n  dashd  {}\n  rdashd {}",
                show(&theirs),
                show(&mine)
            ));
        }
        let text = String::from_utf8(theirs).unwrap();
        let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
        self.replies.insert(name.to_owned(), body);
    }

    /// Sets both nodes' mock time to now; now.
    pub fn mock_now(&mut self) -> std::time::Duration {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        self.both("setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
        now
    }

    pub fn dashd_replies(&self) -> &std::collections::HashMap<String, String> {
        &self.replies
    }

    /// dashd's `result` of the call `name`.
    pub fn result(&self, name: &str) -> UniValue {
        let UniValue::Obj(fields) = UniValue::read(self.replies[name].as_bytes()).unwrap() else {
            panic!("{}", self.replies[name])
        };
        fields.into_iter().find(|(k, _)| k == "result").unwrap().1
    }

    /// Stops both nodes, then fails on any differing reply.
    pub fn finish(self) -> std::collections::HashMap<String, String> {
        for node in self.nodes {
            node.stop_ok(libc::SIGTERM);
        }
        assert!(
            self.failures.is_empty(),
            "{} of {} differ:\n{}",
            self.failures.len(),
            self.count,
            self.failures.join("\n")
        );
        self.replies
    }
}

/// `u:p`.
pub const AUTH: &str = "Authorization: Basic dTpw\r\n";

/// How long a reply's connection is watched after its last byte for the server's close, which
/// the comparison includes (`<closed>`): a kept-alive connection stays quiet. A reply that sends
/// `Connection: close` is watched for [`ANNOUNCED_CLOSE_WATCH`] instead.
pub const CLOSE_WATCH: Duration = Duration::from_secs(1);

/// The close watch after a `Connection: close` reply: long enough for a close that a loaded box
/// delays (BL-478), and below both servers' 30 s `-rpcservertimeout` default, so an idle-timeout
/// close is never taken for the announced one (no node in this file sets `-rpcservertimeout`).
pub const ANNOUNCED_CLOSE_WATCH: Duration = Duration::from_secs(10);
