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

/// How a call's reference reply is compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// The reply body byte for byte.
    Body,
    /// `protx listdiff`: `updatedMNs` as a set ([`sorted_updates`]), the rest byte for byte.
    ListDiff,
    /// A failed `CHECK_NONFATAL` ([`Observed::InternalBug`]).
    InternalBug,
}

impl Policy {
    /// [`Policy::ListDiff`] for `protx listdiff`, by its own name or through the `protx` family,
    /// else [`Policy::Body`].
    pub fn of(method: &str, params: &str) -> Policy {
        if method == "protx listdiff" || (method == "protx" && params.starts_with(r#"["listdiff""#))
        {
            Policy::ListDiff
        } else {
            Policy::Body
        }
    }
}

/// One node's reply to a call, in the form its [`Policy`] compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observed {
    Body(String),
    ListDiff(String),
    /// The error's code, its message's first line after "Internal bug detected: ", and the
    /// function its second line names; the rest names each node's own package, source and report
    /// URL, so it is never compared (RP1a-notes RP0-D5).
    InternalBug {
        code: i64,
        check: String,
        function: String,
    },
}

impl Observed {
    /// `body`, a JSON-RPC reply object's text, under `policy`. An internal-bug call answered
    /// otherwise is its [`Observed::Body`], so it differs.
    pub fn read(policy: Policy, body: &str) -> Observed {
        match policy {
            Policy::Body => Observed::Body(body.to_owned()),
            Policy::ListDiff => Observed::ListDiff(body.to_owned()),
            Policy::InternalBug => {
                internal_bug(body).unwrap_or_else(|| Observed::Body(body.to_owned()))
            }
        }
    }

    /// Whether `ours` matches this reference reply under its policy.
    pub fn matches(&self, ours: &Observed) -> bool {
        match (self, ours) {
            (Observed::ListDiff(a), Observed::ListDiff(b)) => {
                a == b || sorted_updates(a) == sorted_updates(b)
            }
            _ => self == ours,
        }
    }

    pub fn policy(&self) -> Policy {
        match self {
            Observed::Body(_) => Policy::Body,
            Observed::ListDiff(_) => Policy::ListDiff,
            Observed::InternalBug {
                ..
            } => Policy::InternalBug,
        }
    }

    /// Whether this is the reference failing `check` (any check when `None`) with Core's
    /// `RPC_MISC_ERROR`; a reference that did not fail it leaves its case testing nothing.
    pub fn failed(&self, check: Option<&str>) -> bool {
        matches!(self, Observed::InternalBug { code: -1, check: c, .. } if check.is_none_or(|x| x == c))
    }
}

/// `body`'s internal-bug error, if it is one.
fn internal_bug(body: &str) -> Option<Observed> {
    let UniValue::Obj(fields) = UniValue::read(body.as_bytes())? else {
        return None;
    };
    let (_, UniValue::Obj(error)) = fields.iter().find(|(k, _)| k == "error")? else {
        return None;
    };
    let get = |k: &str| error.iter().find(|(key, _)| key == k).map(|(_, v)| v);
    let code = get("code")?.get_int64().ok()?;
    let mut lines = get("message")?.get_str().ok()?.split('\n');
    let check = lines.next()?.strip_prefix("Internal bug detected: ")?.to_owned();
    let function = lines.next()?.rsplit_once(" (")?.1.strip_suffix(')')?.to_owned();
    Some(Observed::InternalBug {
        code,
        check,
        function,
    })
}

/// A `protx listdiff` reply object's text with its result's `updatedMNs` sorted: Core v24.0.0
/// keeps them in an `unordered_map` under `StaticSaltedHasher`, whose salt each process draws at
/// start (saltedhasher.cpp:8-10), so dashd's order changes from run to run; `addedMNs` and
/// `removedMNs` keep their order. Other text is returned as it is.
// Core: evo/deterministicmns.h:635, rpc/evo.cpp:1984-1998
pub fn sorted_updates(body: &str) -> String {
    let Some(UniValue::Obj(mut fields)) = UniValue::read(body.as_bytes()) else {
        return body.to_owned();
    };
    for (key, value) in &mut fields {
        let (true, UniValue::Obj(result)) = (key == "result", value) else {
            continue;
        };
        for (key, value) in result {
            if let (true, UniValue::Arr(updated)) = (key == "updatedMNs", value) {
                updated.sort_by_cached_key(UniValue::write);
            }
        }
    }
    UniValue::Obj(fields).write()
}

/// A call's reference reply, as [`Pair`] keeps it and the goldens store it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub method: String,
    pub params: String,
    /// The batch the call went in (its ordinal on the pair) and its index (its id) there; `None`
    /// for a single call.
    pub batch: Option<(usize, usize)>,
    pub reply: Observed,
}

/// One JSON-RPC batch of `(name, method, params)` calls sent to dashd (`ports.0`) and rdashd
/// (`ports.1`), the replies compared byte for byte with the `Date` header masked; a differing
/// batch is reported per call where the replies parse. Returns dashd's results, in order.
pub fn diff_batch(
    ports: (u16, u16),
    calls: &[(String, &str, String)],
    failures: &mut Vec<String>,
) -> Vec<UniValue> {
    batch(ports, calls, failures).0
}

/// [`diff_batch`], each call compared under its [`Policy`], and dashd's reply to each. The
/// envelope is the whole reply byte for byte, or with a `protx listdiff` in the batch (whose
/// `updatedMNs` order is the process's) its head.
fn batch(
    ports: (u16, u16),
    calls: &[(String, &str, String)],
    failures: &mut Vec<String>,
) -> (Vec<UniValue>, Vec<Observed>) {
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
    let split = |bytes: &[u8]| -> (String, Vec<UniValue>) {
        let text = std::str::from_utf8(bytes).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));
        match UniValue::read(body.as_bytes()) {
            Some(UniValue::Arr(items)) => (head.to_owned(), items),
            _ => (head.to_owned(), Vec::new()),
        }
    };
    let ((core_head, core_replies), (our_head, our_replies)) = (split(&theirs), split(&mine));
    assert_eq!(core_replies.len(), calls.len(), "{}: dashd's batch {}", calls[0].0, show(&theirs));
    let mut named = false;
    let mut listdiff = false;
    let mut replies = Vec::new();
    for (i, ((name, method, params), core)) in calls.iter().zip(&core_replies).enumerate() {
        let policy = Policy::of(method, params);
        listdiff |= policy == Policy::ListDiff;
        let theirs = Observed::read(policy, &core.write());
        let mine = our_replies.get(i).map(|v| Observed::read(policy, &v.write()));
        if !mine.as_ref().is_some_and(|mine| theirs.matches(mine)) {
            failures.push(format!("{name}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
            named = true;
        }
        replies.push(theirs);
    }
    if listdiff && core_head != our_head {
        failures.push(format!("{} (head):\n  dashd  {core_head}\n  rdashd {our_head}", calls[0].0));
    }
    if !listdiff && theirs != mine && !named {
        failures.push(format!(
            "{} (whole batch):\n  dashd  {}\n  rdashd {}",
            calls[0].0,
            show(&theirs),
            show(&mine)
        ));
    }
    let results = core_replies.into_iter().map(|reply| field(&reply, "result")).collect();
    (results, replies)
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

/// [`diff_batch`] in chunks of up to 25 consecutive calls of one [`Policy`] (so a batch without
/// `protx listdiff` keeps its whole-reply byte comparison), counted on `pair`, dashd's reply to
/// each call kept in its store by name and index.
pub fn diff_chunks(pair: &mut Pair, calls: &[(String, &str, String)]) -> Vec<UniValue> {
    let ports = (pair.ports[0], pair.ports[1]);
    let mut results = Vec::new();
    let policy = |(_, method, params): &(String, &str, String)| Policy::of(method, params);
    let runs = calls.chunk_by(|a, b| policy(a) == policy(b));
    for chunk in runs.flat_map(|run| run.chunks(25)) {
        pair.count += chunk.len();
        let (chunk_results, replies) = batch(ports, chunk, &mut pair.failures);
        let id = pair.batches;
        pair.batches += 1;
        for (i, ((name, method, params), reply)) in chunk.iter().zip(replies).enumerate() {
            pair.records.push(Record {
                name: name.clone(),
                method: (*method).to_owned(),
                params: params.clone(),
                batch: Some((id, i)),
                reply,
            });
        }
        results.extend(chunk_results);
    }
    results
}

/// [`both_internal_bug_of`] any check.
pub fn both_internal_bug(pair: &mut Pair, name: &str, method: &str, params: &str) {
    both_internal_bug_of(pair, None, name, method, params);
}

/// One call whose reply on both nodes must be an internal-bug error, compared as
/// [`Observed::InternalBug`]. dashd must fail `check` (any check when `None`) with code -1, or the
/// case tests nothing: "reference did not fail".
pub fn both_internal_bug_of(
    pair: &mut Pair,
    check: Option<&str>,
    name: &str,
    method: &str,
    params: &str,
) {
    pair.count += 1;
    let request = call(method, params);
    let [theirs, mine] = pair.ports.map(|port| {
        let reply = String::from_utf8(exchange(port, &request)).unwrap();
        Observed::read(Policy::InternalBug, reply.split_once("\r\n\r\n").unwrap().1)
    });
    if !theirs.failed(check) {
        pair.failures
            .push(format!("{name}: reference did not fail {check:?}:\n  dashd  {theirs:?}"));
    } else if theirs != mine {
        pair.failures.push(format!("{name}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
    }
    pair.records.push(Record {
        name: name.to_owned(),
        method: method.to_owned(),
        params: params.to_owned(),
        batch: None,
        reply: theirs,
    });
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

/// A finished pair's reference replies with what it ran on, kept by [`recording`].
#[derive(Clone, Debug)]
pub struct Section {
    /// The recorded run its chain came from ([`recorded::last_run`]), if any.
    pub run: Option<String>,
    /// Both nodes' extra arguments.
    pub args: Vec<String>,
    pub records: Vec<Record>,
}

thread_local! {
    static SECTIONS: std::cell::RefCell<Option<Vec<Section>>> =
        const { std::cell::RefCell::new(None) };
}

/// `f`, with every pair it finishes on this thread kept as a [`Section`], in finishing order.
pub fn recording<R>(f: impl FnOnce() -> R) -> (R, Vec<Section>) {
    SECTIONS.set(Some(Vec::new()));
    let out = f();
    (out, SECTIONS.take().unwrap())
}

/// rdashd as [`Pair`] starts it, on `home/ours` with `extra`: the node and its RPC port.
pub fn solo(home: &Path, extra: &[&str]) -> (Running, u16) {
    start_node(Path::new(RDASHD), home, "ours", &[], extra)
}

/// `binary` on `home/name` with `own` and `extra` arguments and a free RPC port, until loaded.
fn start_node(
    binary: &Path,
    home: &Path,
    name: &str,
    own: &[&str],
    extra: &[&str],
) -> (Running, u16) {
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
}

/// The arguments [`Pair`] gives both nodes ahead of its extra ones (the RPC port is a free one),
/// then dashd's own: for a golden's provenance.
pub const NODE_ARGS: [&str; 6] = [
    "-regtest",
    "-printtoconsole=0",
    "-listen=0",
    "-rpcport=<free>",
    "-rpcuser=u",
    "-rpcpassword=p",
];
pub const DASHD_ARGS: [&str; 1] = ["-disablewallet"];

/// A pair of fresh regtest nodes, dashd (with `-disablewallet`) and rdashd, each with `extra`.
/// [`Pair::both`] sends one call to both and records a differing reply; [`Pair::finish`] stops
/// them and fails on any. dashd's reply to every call is kept as a [`Record`].
pub struct Pair {
    pub ports: [u16; 2],
    nodes: Vec<Running>,
    home: tempfile::TempDir,
    extra: Vec<String>,
    run: Option<String>,
    pub failures: Vec<String>,
    pub count: usize,
    batches: usize,
    records: Vec<Record>,
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
            run: super::recorded::last_run(),
            failures: Vec::new(),
            count: 0,
            batches: 0,
            records: Vec::new(),
        }
    }

    /// dashd, then rdashd, on their datadirs under `home` with `extra`, each on a free RPC port.
    fn spawn(home: &Path, extra: &[&str]) -> (Vec<Running>, [u16; 2]) {
        let dashd = require_dashd();
        let (core, core_port) = start_node(&dashd, home, "core", &DASHD_ARGS, extra);
        let (ours, our_port) = solo(home, extra);
        (vec![core, ours], [core_port, our_port])
    }

    /// Stops both nodes cleanly and starts them again on the same datadirs and arguments.
    pub fn restart(&mut self) {
        assert!(
            SECTIONS.with_borrow(Option::is_none),
            "a golden replay cannot restart its node: record no restarting pair"
        );
        for node in self.nodes.drain(..) {
            node.stop_ok(libc::SIGTERM);
        }
        let extra: Vec<&str> = self.extra.iter().map(String::as_str).collect();
        (self.nodes, self.ports) = Pair::spawn(self.home.path(), &extra);
    }

    /// One call to both nodes, warnings emptied, compared byte for byte (a `protx listdiff`
    /// under [`Policy::ListDiff`], its head byte for byte); dashd's body is kept for
    /// [`Pair::result`] and [`Pair::finish`].
    pub fn both(&mut self, name: &str, method: &str, params: &str) {
        self.count += 1;
        let request = call(method, params);
        let [theirs, mine] = self.ports.map(|port| empty_warnings(&exchange(port, &request)));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        let policy = Policy::of(method, params);
        let [(core_head, core_body), (our_head, our_body)] = [&theirs, &mine].map(|bytes| {
            let text = String::from_utf8_lossy(bytes).into_owned();
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            (head.to_owned(), body.to_owned())
        });
        let reply = Observed::read(policy, &core_body);
        let same = match policy {
            Policy::ListDiff => {
                core_head == our_head && reply.matches(&Observed::read(policy, &our_body))
            }
            _ => theirs == mine,
        };
        if !same {
            self.failures.push(format!(
                "{name}:\n  dashd  {}\n  rdashd {}",
                show(&theirs),
                show(&mine)
            ));
        }
        self.records.push(Record {
            name: name.to_owned(),
            method: method.to_owned(),
            params: params.to_owned(),
            batch: None,
            reply,
        });
    }

    /// Sets both nodes' mock time to now; now.
    pub fn mock_now(&mut self) -> std::time::Duration {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        self.mock_at(now.as_secs());
        now
    }

    /// Sets both nodes' mock time to `secs`.
    pub fn mock_at(&mut self, secs: u64) {
        self.both("setmocktime", "setmocktime", &format!("[{secs}]"));
    }

    /// dashd's replies, in call order.
    pub fn dashd_replies(&self) -> &[Record] {
        &self.records
    }

    /// dashd's body of the last single call ([`Pair::both`]) `name` compared by body.
    pub fn body(&self, name: &str) -> &str {
        self.records
            .iter()
            .rev()
            .find_map(|r| match (&r.reply, r.name == name && r.batch.is_none()) {
                (Observed::Body(body) | Observed::ListDiff(body), true) => Some(body.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no body for {name}"))
    }

    /// dashd's `result` of the call `name`.
    pub fn result(&self, name: &str) -> UniValue {
        let body = self.body(name);
        let UniValue::Obj(fields) = UniValue::read(body.as_bytes()).unwrap() else {
            panic!("{body}")
        };
        fields.into_iter().find(|(k, _)| k == "result").unwrap().1
    }

    /// Stops both nodes, keeps the pair's [`Section`] when [`recording`], then fails on any
    /// differing reply. Returns dashd's bodies of the single calls by name ([`Pair::both`]).
    pub fn finish(self) -> std::collections::HashMap<String, String> {
        for node in self.nodes {
            node.stop_ok(libc::SIGTERM);
        }
        SECTIONS.with_borrow_mut(|sections| {
            if let Some(sections) = sections {
                sections.push(Section {
                    run: self.run.clone(),
                    args: self.extra.clone(),
                    records: self.records.clone(),
                });
            }
        });
        assert!(
            self.failures.is_empty(),
            "{} of {} differ:\n{}",
            self.failures.len(),
            self.count,
            self.failures.join("\n")
        );
        let singles = self.records.into_iter().filter(|r| r.batch.is_none());
        singles
            .filter_map(|r| match r.reply {
                Observed::Body(body) | Observed::ListDiff(body) => Some((r.name, body)),
                Observed::InternalBug {
                    ..
                } => None,
            })
            .collect()
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
