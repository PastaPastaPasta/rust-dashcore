//! DIFF-RPC (RP0): the RPC server's bytes against dashd v24.0.0-rc.5's. The same raw requests go
//! to both nodes, and each reply, headers and body, must be equal once the `Date` header is
//! masked: HTTP error pages, JSON-RPC errors and their statuses, batches, named arguments, the
//! number and string formatting of `echo`, and the help text.
//!
//! dashd runs with `-disablewallet`, the view rdashd's `help` renders (RP0-D1). Cases call only
//! commands rdashd serves; a reply that holds the node's state (`uptime`, `getrpcinfo`) is not
//! compared. The chain commands (DEC-279) start on both nodes' fresh regtest chain; the
//! `submitblock` cases then grow both chains with the same blocks, run in order, and
//! `getblockhash` and `getblockcount` (O11, DEC-359) read them. Unlike the other dashd tests these
//! cannot be skipped: with `SKIP_DASHD_TESTS` set they fail (`common::require_dashd`). Replies are
//! compared byte for byte, with only the HTTP `Date` header's value masked.
#![cfg(unix)]

mod common;

use std::io::{ErrorKind, Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};

use common::mine::*;
use common::*;
use dash_args::univalue::SettingsValue as UniValue;

/// `u:p`.
const AUTH: &str = "Authorization: Basic dTpw\r\n";

/// A POST of `body` to `path` with `headers` (each ending in CRLF).
fn post_with(path: &str, version: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut request = format!(
        "POST {path} {version}\r\nHost: x\r\n{headers}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

fn post(body: &str) -> Vec<u8> {
    post_with("/", "HTTP/1.1", AUTH, body.as_bytes())
}

/// The cases: a name, then the request's bytes.
fn cases() -> Vec<(&'static str, Vec<u8>)> {
    let raw = |s: &str| s.as_bytes().to_vec();
    let long =
        |n: usize| format!("GET /{} HTTP/1.1\r\nHost: x\r\n\r\n", "x".repeat(n)).into_bytes();
    vec![
        ("help_command", post(r#"{"method":"help","params":["uptime"],"id":1}"#)),
        ("help_overview", post(r#"{"method":"help","id":1}"#)),
        ("help_wrong_type", post(r#"{"method":"help","params":[0],"id":1}"#)),
        ("help_too_many", post(r#"{"method":"help","params":["a","b","c"],"id":1}"#)),
        ("help_unknown", post(r#"{"method":"help","params":["foo"],"id":1}"#)),
        (
            "help_conversions",
            post(r#"{"method":"help","params":["dump_all_command_conversions"],"id":1}"#),
        ),
        (
            "connection_close",
            post_with(
                "/",
                "HTTP/1.1",
                &format!("{AUTH}Connection: close\r\n"),
                br#"{"method":"echo","params":[1,"a",1.50,1e3,-0,true,null],"id":"x"}"#,
            ),
        ),
        ("get", raw(&format!("GET / HTTP/1.1\r\nHost: x\r\n{AUTH}\r\n"))),
        ("no_auth", post_with("/", "HTTP/1.1", "", b"{}")),
        ("bad_auth", post_with("/", "HTTP/1.1", "Authorization: Basic Zm9vOmJhcg==\r\n", b"{}")),
        ("not_basic", post_with("/", "HTTP/1.1", "Authorization: Bearer x\r\n", b"{}")),
        ("unknown_path", post_with("/foo", "HTTP/1.1", AUTH, br#"{"method":"help"}"#)),
        ("wallet_path", post_with("/wallet/w1", "HTTP/1.1", AUTH, br#"{"method":"echo","id":1}"#)),
        ("get_1000", long(1000)),
        ("get_10000", long(10_000)),
        ("put", raw("PUT / HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")),
        ("head", raw("HEAD / HTTP/1.1\r\nHost: x\r\n\r\n")),
        ("options", raw("OPTIONS / HTTP/1.1\r\nHost: x\r\n\r\n")),
        ("delete", raw("DELETE / HTTP/1.1\r\nHost: x\r\n\r\n")),
        ("garbage", raw("hello\r\n\r\n")),
        ("bad_json", post(r#"{"method":"#)),
        ("empty", post("")),
        ("scalar", post("1")),
        ("string", post(r#""x""#)),
        ("trailing", post(r#"{"method":"echo","id":1} x"#)),
        ("nul", post_with("/", "HTTP/1.1", AUTH, b"{\"method\":\"echo\",\"id\":1}\0")),
        ("depth", post(&format!("{}{}", "[".repeat(600), "]".repeat(600)))),
        ("no_method", post(r#"{"id":5}"#)),
        ("method_number", post(r#"{"method":5,"id":5}"#)),
        ("params_string", post(r#"{"method":"help","params":"x","id":5}"#)),
        ("not_found", post(r#"{"method":"nosuch","id":[1,{"a":2}]}"#)),
        ("jsonrpc_2", post(r#"{"jsonrpc":"2.0","method":"echo","id":1}"#)),
        (
            "batch",
            post(
                r#"[{"method":"echo","params":[1],"id":1},{"method":"nosuch","id":2},5,{"id":3}]"#,
            ),
        ),
        ("batch_empty", post("[]")),
        ("named_unknown", post(r#"{"method":"help","params":{"random":"x"},"id":1}"#)),
        ("named_duplicate", post(r#"{"method":"echo","params":{"arg0":1,"arg0":2},"id":1}"#)),
        ("named_twice", post(r#"{"method":"echo","params":{"args":[0,1],"arg1":1},"id":1}"#)),
        ("named_holes", post(r#"{"method":"echo","params":{"arg0":0,"arg3":3,"arg9":9},"id":1}"#)),
        ("named_help", post(r#"{"method":"help","params":{"command":"stop"},"id":1}"#)),
        ("echojson", post(r#"{"method":"echojson","params":[1.0000,"x",{"b":1,"a":[]}],"id":1}"#)),
        (
            "numbers",
            post(
                r#"{"method":"echo","params":[1E5,0.1e-2,-1.5,12345678901234567890,1.0],"id":1.50}"#,
            ),
        ),
        ("strings", post(r#"{"method":"echo","params":["é😀\n\t\"/\u0001\u007f","é"],"id":1}"#)),
        ("http_1_0", post_with("/", "HTTP/1.0", AUTH, br#"{"method":"echo","id":1}"#)),
        (
            "http_1_0_keep_alive",
            post_with(
                "/",
                "HTTP/1.0",
                &format!("{AUTH}Connection: keep-alive\r\n"),
                br#"{"method":"echo","id":1}"#,
            ),
        ),
        // `sscanf("%d")` reads a negative version component, which the status line repeats.
        ("http_minor_negative", post_with("/", "HTTP/1.-1", AUTH, br#"{"method":"echo","id":1}"#)),
        ("http_major_negative", post_with("/", "HTTP/-1.1", AUTH, br#"{"method":"echo","id":1}"#)),
        ("get_minor_negative", raw(&format!("GET / HTTP/1.-1\r\nHost: x\r\n{AUTH}\r\n"))),
        ("get_major_negative", raw(&format!("GET / HTTP/-1.1\r\nHost: x\r\n{AUTH}\r\n"))),
        ("logging_unknown", post(r#"{"method":"logging","params":[["nosuch"]],"id":1}"#)),
        ("stop_wrong_type", post(r#"{"method":"stop","params":["x"],"id":1}"#)),
        // DEC-279's `getblockhash` on both nodes' fresh regtest chain, genesis alone.
        ("getblockhash_genesis", post(r#"{"method":"getblockhash","params":[0],"id":1}"#)),
        ("getblockhash_named", post(r#"{"method":"getblockhash","params":{"height":0},"id":1}"#)),
        ("getblockhash_above_tip", post(r#"{"method":"getblockhash","params":[1],"id":1}"#)),
        ("getblockhash_negative", post(r#"{"method":"getblockhash","params":[-1],"id":1}"#)),
        ("getblockhash_minus_zero", post(r#"{"method":"getblockhash","params":[-0],"id":1}"#)),
        (
            "getblockhash_int_underflow",
            post(r#"{"method":"getblockhash","params":[-2147483649],"id":1}"#),
        ),
        ("getblockhash_int_max", post(r#"{"method":"getblockhash","params":[2147483647],"id":1}"#)),
        (
            "getblockhash_int_overflow",
            post(r#"{"method":"getblockhash","params":[2147483648],"id":1}"#),
        ),
        (
            "getblockhash_beyond_i64",
            post(r#"{"method":"getblockhash","params":[9223372036854775808],"id":1}"#),
        ),
        ("getblockhash_fraction", post(r#"{"method":"getblockhash","params":[1.5],"id":1}"#)),
        ("getblockhash_exponent", post(r#"{"method":"getblockhash","params":[0e0],"id":1}"#)),
        ("getblockhash_string", post(r#"{"method":"getblockhash","params":["0"],"id":1}"#)),
        ("getblockhash_null", post(r#"{"method":"getblockhash","params":[null],"id":1}"#)),
        ("getblockhash_none", post(r#"{"method":"getblockhash","params":[],"id":1}"#)),
        ("getblockhash_two", post(r#"{"method":"getblockhash","params":[0,1],"id":1}"#)),
        (
            "getblockhash_named_unknown",
            post(r#"{"method":"getblockhash","params":{"h":0},"id":1}"#),
        ),
        (
            "getblockhash_named_string",
            post(r#"{"method":"getblockhash","params":{"height":"0"},"id":1}"#),
        ),
        (
            "getblockhash_batch",
            post(
                r#"[{"method":"getblockhash","params":[0],"id":1},{"method":"getblockhash","params":[5],"id":2}]"#,
            ),
        ),
        ("getblockhash_help", post(r#"{"method":"help","params":["getblockhash"],"id":1}"#)),
        // O11's `getblockcount` (DEC-359) on the same fresh chain: height 0.
        ("getblockcount_genesis", post(r#"{"method":"getblockcount","id":1}"#)),
        ("getblockcount_empty_params", post(r#"{"method":"getblockcount","params":[],"id":1}"#)),
        ("getblockcount_named_none", post(r#"{"method":"getblockcount","params":{},"id":1}"#)),
        ("getblockcount_one", post(r#"{"method":"getblockcount","params":[0],"id":1}"#)),
        ("getblockcount_null", post(r#"{"method":"getblockcount","params":[null],"id":1}"#)),
        ("getblockcount_named", post(r#"{"method":"getblockcount","params":{"x":1},"id":1}"#)),
        (
            "getblockcount_batch",
            post(
                r#"[{"method":"getblockcount","id":1},{"method":"getblockcount","params":[1],"id":2}]"#,
            ),
        ),
        ("getblockcount_help", post(r#"{"method":"help","params":["getblockcount"],"id":1}"#)),
        // DEC-279's `getpeerinfo` on both nodes, neither with a peer; `getpeerinfo_matches_…`
        // compares a connected peer.
        ("getpeerinfo", post(r#"{"method":"getpeerinfo","id":1}"#)),
        ("getpeerinfo_empty_params", post(r#"{"method":"getpeerinfo","params":[],"id":1}"#)),
        ("getpeerinfo_named_none", post(r#"{"method":"getpeerinfo","params":{},"id":1}"#)),
        ("getpeerinfo_one", post(r#"{"method":"getpeerinfo","params":[1],"id":1}"#)),
        ("getpeerinfo_null", post(r#"{"method":"getpeerinfo","params":[null],"id":1}"#)),
        ("getpeerinfo_named", post(r#"{"method":"getpeerinfo","params":{"x":1},"id":1}"#)),
        (
            "getpeerinfo_batch",
            post(
                r#"[{"method":"getpeerinfo","id":1},{"method":"getpeerinfo","params":[0],"id":2}]"#,
            ),
        ),
        ("getpeerinfo_help", post(r#"{"method":"help","params":["getpeerinfo"],"id":1}"#)),
        (
            "help_getblockfrompeer",
            post(r#"{"method":"help","params":["getblockfrompeer"],"id":1}"#),
        ),
    ]
}

/// The `submitblock` cases on `genesis`, and the `getblockhash` and `getblockcount` reads of the
/// chain they build (`getblockcount` after each block that moves the tip, and after a side chain's
/// and a rejected block that do not), in
/// order: the argument checks; `DecodeHexBlk`'s and the coinbase's refusals; `CheckBlock`'s,
/// `AcceptBlockHeader`'s and `AcceptBlock`'s rejects; a new block, its duplicate, a side chain's
/// block (inconclusive, then a duplicate), and blocks `ConnectBlock` and `ContextualCheckBlock`
/// reject (then `duplicate-invalid`), with a child of the first. Then the side chain overtakes
/// the first chain at height 5, a reorg through the connector's disconnects (CH3a D67), and the
/// first chain overtakes it back at height 6, reconnecting blocks that already have their undo.
fn submitblock_cases(genesis: &Mined) -> Vec<(&'static str, Vec<u8>)> {
    let submit =
        |hex: &str| post(&format!(r#"{{"method":"submitblock","params":["{hex}"],"id":1}}"#));
    let at_60 = |prev: &[u8; 32], txs: &[Vec<u8>], merkle, meets| {
        mine(prev, GENESIS_TIME + 60, txs, merkle, meets)
    };
    let g = &genesis.hash;
    let b1 = next(g, 1, 0);
    let b2 = next(&b1.hash, 2, 0);
    // The genesis block's time: a side chain's block at height 1, as much work as `b1`.
    let side = mine(g, GENESIS_TIME + 1, &[coinbase(1, 0)], None, true);
    let overpaid = next(&b2.hash, 3, 100_000 * 100_000_000);
    let overpaid_child = next(&overpaid.hash, 4, 0);
    // BIP34's height 9 at height 3.
    let bad_cb_height = next_with(&b2.hash, 3, coinbase(9, 0));
    let b3 = next(&b2.hash, 3, 0);
    let b4 = next(&b3.hash, 4, 0);
    let s2 = next(&side.hash, 2, 0);
    let s3 = next(&s2.hash, 3, 0);
    let s4 = next(&s3.hash, 4, 0);
    let s5 = next(&s4.hash, 5, 0);
    let b5 = next(&b4.hash, 5, 0);
    let b6 = next(&b5.hash, 6, 0);
    let cb = [coinbase(1, 0)];
    let high_hash = at_60(g, &cb, None, false);
    let bad_merkle = at_60(g, &cb, Some([7; 32]), true);
    let orphan = at_60(&[9; 32], &cb, None, true);
    // A first transaction that spends an output.
    let mut spend = coinbase(1, 0);
    spend[5..37].fill(0x11);
    let not_coinbase = at_60(g, &[spend], None, true);
    let no_txs = format!("{}00", &b1.hex[..160]);
    let getblockhash =
        |h: i32| post(&format!(r#"{{"method":"getblockhash","params":[{h}],"id":1}}"#));
    let getblockcount = || post(r#"{"method":"getblockcount","id":1}"#);
    vec![
        ("submitblock_help", post(r#"{"method":"help","params":["submitblock"],"id":1}"#)),
        ("submitblock_none", post(r#"{"method":"submitblock","params":[],"id":1}"#)),
        ("submitblock_three", post(r#"{"method":"submitblock","params":["00","x","y"],"id":1}"#)),
        ("submitblock_number", post(r#"{"method":"submitblock","params":[1],"id":1}"#)),
        ("submitblock_null", post(r#"{"method":"submitblock","params":[null],"id":1}"#)),
        ("submitblock_dummy_number", post(r#"{"method":"submitblock","params":["00",1],"id":1}"#)),
        (
            "submitblock_named_unknown",
            post(r#"{"method":"submitblock","params":{"hex":"00"},"id":1}"#),
        ),
        ("submitblock_empty", submit("")),
        ("submitblock_odd", submit("0")),
        ("submitblock_not_hex", submit("zz")),
        ("submitblock_short", submit("00")),
        ("submitblock_truncated", submit(&b1.hex[..b1.hex.len() - 2])),
        ("submitblock_space", submit(&format!(" {}", b1.hex))),
        ("submitblock_no_txs", submit(&no_txs)),
        ("submitblock_not_coinbase", submit(&not_coinbase.hex)),
        ("submitblock_genesis", submit(&genesis.hex)),
        ("submitblock_high_hash", submit(&high_hash.hex)),
        ("submitblock_bad_merkle", submit(&bad_merkle.hex)),
        ("submitblock_bad_merkle_again", submit(&bad_merkle.hex)),
        ("submitblock_orphan", submit(&orphan.hex)),
        ("submitblock_b1", submit(&b1.hex)),
        ("getblockcount_1", getblockcount()),
        ("submitblock_b1_again", submit(&b1.hex)),
        ("submitblock_side", submit(&side.hex)),
        ("submitblock_side_again", submit(&side.hex)),
        ("getblockcount_side", getblockcount()),
        ("submitblock_b2_upper_trailing", submit(&format!("{}00FF", b2.hex.to_uppercase()))),
        ("submitblock_overpaid", submit(&overpaid.hex)),
        ("getblockcount_overpaid", getblockcount()),
        ("submitblock_overpaid_again", submit(&overpaid.hex)),
        ("submitblock_overpaid_child", submit(&overpaid_child.hex)),
        ("submitblock_bad_cb_height", submit(&bad_cb_height.hex)),
        ("submitblock_bad_cb_height_again", submit(&bad_cb_height.hex)),
        (
            "submitblock_b3_named",
            post(&format!(
                r#"{{"method":"submitblock","params":{{"hexdata":"{}"}},"id":1}}"#,
                b3.hex
            )),
        ),
        (
            "submitblock_b4_dummy",
            post(&format!(r#"{{"method":"submitblock","params":["{}","x"],"id":1}}"#, b4.hex)),
        ),
        ("getblockhash_1", getblockhash(1)),
        ("getblockhash_2", getblockhash(2)),
        ("getblockhash_3", getblockhash(3)),
        ("getblockhash_4", getblockhash(4)),
        ("getblockhash_5", getblockhash(5)),
        ("getblockcount_4", getblockcount()),
        ("submitblock_s2", submit(&s2.hex)),
        ("submitblock_s3", submit(&s3.hex)),
        ("submitblock_s4", submit(&s4.hex)),
        ("submitblock_s5_reorg", submit(&s5.hex)),
        ("getblockhash_1_reorged", getblockhash(1)),
        ("getblockhash_3_reorged", getblockhash(3)),
        ("getblockhash_5_reorged", getblockhash(5)),
        ("getblockhash_6_reorged", getblockhash(6)),
        ("submitblock_b5", submit(&b5.hex)),
        ("submitblock_b6_reorg_back", submit(&b6.hex)),
        ("getblockhash_1_back", getblockhash(1)),
        ("getblockhash_3_back", getblockhash(3)),
        ("getblockhash_6_back", getblockhash(6)),
        ("getblockhash_7_back", getblockhash(7)),
    ]
}

/// A string `result` of `body`'s call on `port`.
fn result_str(port: u16, body: &str) -> String {
    let reply = String::from_utf8(exchange(port, &post(body))).unwrap();
    let (_, rest) = reply.split_once(r#""result":""#).unwrap_or_else(|| panic!("{reply}"));
    rest.split('"').next().unwrap().to_owned()
}

/// The genesis block both nodes start with, read from dashd.
fn genesis(port: u16) -> Mined {
    let hash_hex = result_str(port, r#"{"method":"getblockhash","params":[0],"id":1}"#);
    let hex =
        result_str(port, &format!(r#"{{"method":"getblock","params":["{hash_hex}",0],"id":1}}"#));
    let mut hash = [0; 32];
    for (i, b) in hash.iter_mut().rev().enumerate() {
        *b = u8::from_str_radix(&hash_hex[2 * i..2 * i + 2], 16).unwrap();
    }
    Mined {
        hex,
        hash,
    }
}

/// How long a reply's connection is watched after its last byte for the server's close, which
/// the comparison includes (`<closed>`): a kept-alive connection stays quiet. A reply that sends
/// `Connection: close` is watched for [`ANNOUNCED_CLOSE_WATCH`] instead.
const CLOSE_WATCH: Duration = Duration::from_secs(1);

/// The close watch after a `Connection: close` reply: long enough for a close that a loaded box
/// delays (BL-478), and below both servers' 30 s `-rpcservertimeout` default, so an idle-timeout
/// close is never taken for the announced one (no node in this file sets `-rpcservertimeout`).
const ANNOUNCED_CLOSE_WATCH: Duration = Duration::from_secs(10);

/// The reply bytes to `request`: the header block with its `Date` header's value masked, the body
/// read through `Content-Length` (libevent sends a body to `HEAD` too), or to the close when no
/// length is sent, then `<closed>` if the server closes within the watch. A reply that ends
/// early, a read error, or more bytes after the body fail the test; no bytes at all, then a close,
/// is `<closed>`.
fn exchange(port: u16, request: &[u8]) -> Vec<u8> {
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

/// `stream.read(buf)`, retried when a signal interrupts it: std does not retry `EINTR`, so a signal
/// the test process takes during a read would otherwise fail the comparison.
fn read_retrying(stream: &mut TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

/// BL-478: a `Connection: close` reply whose close comes after [`CLOSE_WATCH`] (as on a loaded
/// box) is still `<closed>`; one after [`ANNOUNCED_CLOSE_WATCH`] (a server idle timeout, not the
/// announced close) is not, and neither is a kept-alive reply.
/// No dashd: a local listener plays the server.
#[test]
fn exchange_waits_out_an_announced_close_only() {
    // `close_after`: when the server closes after its reply; `None` holds the connection open until
    // the client closes it.
    let serve = |reply: &'static [u8], close_after: Option<Duration>| {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 1024];
            assert!(stream.read(&mut buf).unwrap() > 0);
            stream.write_all(reply).unwrap();
            match close_after {
                Some(after) => std::thread::sleep(after),
                None => while stream.read(&mut buf).is_ok_and(|n| n > 0) {},
            }
        });
        let reply = exchange(port, b"GET / HTTP/1.1\r\n\r\n");
        server.join().unwrap();
        reply
    };
    let announced = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok";
    let kept = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    let late = Some(CLOSE_WATCH + Duration::from_secs(3));
    assert_eq!(serve(announced, late), [&announced[..], b"<closed>"].concat());
    // The 5 s past the watch is the load tolerance: a client stall that long would see the close.
    assert_eq!(serve(announced, Some(ANNOUNCED_CLOSE_WATCH + Duration::from_secs(5))), announced);
    assert_eq!(serve(kept, None), kept);
}

/// `headers` (a reply's header block) with the `Date` header's value replaced by `X`: its name's
/// case and its line ending stay as sent, so they are compared too.
fn mask_date(headers: &[u8]) -> Vec<u8> {
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

#[test]
fn mask_date_keeps_the_name_and_line_ending() {
    let masked =
        mask_date(b"HTTP/1.1 200 OK\r\nDate: Thu, 22 Oct 2026\r\ndate: x\nX-Date: y\r\n\r\n");
    assert_eq!(masked, b"HTTP/1.1 200 OK\r\nDate: X\r\ndate: X\nX-Date: y\r\n\r\n");
    assert_ne!(mask_date(b"date: a\n"), mask_date(b"Date: b\r\n"));
}

/// A node from `cmd` with `u:p` credentials, until init is done.
fn node(cmd: std::process::Command, datadir: &Path) -> Running {
    spawn_until_loaded(cmd, debug_log(datadir))
}

#[test]
fn rpc_replies_match_dashd_v24() {
    let dashd = require_dashd();
    let (home, datadir) = home_and_datadir();
    let (core_port, our_port) = (free_port(), free_port());
    let core_datadir = home.path().join("core");
    std::fs::create_dir_all(&core_datadir).unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let core_rpcport = format!("-rpcport={core_port}");
    let core_args = [
        &datadir_arg(&core_datadir),
        "-regtest",
        "-listen=0",
        "-printtoconsole=0",
        "-disablewallet",
        &core_rpcport,
    ];
    let core =
        node(command(&dashd, home.path(), &[&core_args[..], &credentials].concat()), &core_datadir);
    let our_rpcport = format!("-rpcport={our_port}");
    let ours = node(
        regtest(
            home.path(),
            &datadir,
            &[&["-printtoconsole=0", &our_rpcport][..], &credentials].concat(),
        ),
        &datadir,
    );

    let mut failures = Vec::new();
    let mut replies = std::collections::HashMap::new();
    let mut cases = cases();
    cases.extend(submitblock_cases(&genesis(core_port)));
    for (name, request) in &cases {
        let (theirs, mine) = std::thread::scope(|s| {
            let theirs = s.spawn(|| exchange(core_port, request));
            let mine = s.spawn(|| exchange(our_port, request));
            // The exchange's own panic: `join().unwrap()` would end the test on an `Any { .. }`.
            let join = |h: std::thread::ScopedJoinHandle<'_, Vec<u8>>| {
                h.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            };
            (join(theirs), join(mine))
        });
        // Every case gets an answer: the comparison is never of two silences.
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            failures.push(format!("{name}:\n  dashd  {}\n  rdashd {}", show(&theirs), show(&mine)));
        }
        replies.insert(*name, theirs);
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(
        failures.is_empty(),
        "{} of {} differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    // The reorg cases are not vacuous (CH3a D67): dashd's block at height 1 moves to the side
    // chain and back, and its chain ends at 5, then 6.
    let height_1 = &replies["getblockhash_1"];
    assert_ne!(&replies["getblockhash_1_reorged"], height_1, "dashd did not reorg");
    assert_eq!(&replies["getblockhash_1_back"], height_1, "dashd did not reorg back");
    for (name, found) in [
        ("getblockhash_5_reorged", true),
        ("getblockhash_6_reorged", false),
        ("getblockhash_6_back", true),
        ("getblockhash_7_back", false),
    ] {
        let reply = String::from_utf8_lossy(&replies[name]);
        assert_eq!(reply.contains(r#""error":null"#), found, "{name}: {reply}");
    }
    // Both nodes stored the same blocks and undo records in the same files at the same offsets:
    // the reorg back reconnects b1..b4 over the undo they already have (`blockstorage.cpp:777`).
    for name in ["blk00000.dat", "rev00000.dat"] {
        let [theirs, mine] = [&core_datadir, &datadir]
            .map(|d| std::fs::read(net(d).join("blocks").join(name)).unwrap());
        assert!(
            theirs == mine,
            "{name}: dashd's {} bytes and rdashd's {} first differ at {:?}",
            theirs.len(),
            mine.len(),
            theirs.iter().zip(&mine).position(|(a, b)| a != b)
        );
    }
}

/// Blocks 1 to 100 on `genesis`, then three blocks 101 on block 100. The first's second
/// transaction spends block 1's coinbase, mature there, with `OP_RETURN` for its `scriptSig`: its
/// only script check fails. The second adds a third transaction, which spends an output no
/// transaction made: `CheckTxInputs` fails it after the first's script check is queued, or has
/// failed inline. The third is the first with a coinbase that pays too much, which only the
/// checks after `Wait` see (`bad-cb-amount`).
fn script_failure_chain(genesis: &Mined) -> Vec<Mined> {
    let mut blocks: Vec<Mined> = Vec::new();
    for height in 1..=100 {
        let prev = blocks.last().map_or(genesis.hash, |b| b.hash);
        blocks.push(next(&prev, height, 0));
    }
    let mut spend = 1i32.to_le_bytes().to_vec();
    spend.push(1);
    spend.extend(txid(&coinbase(1, 0)));
    spend.extend(0u32.to_le_bytes());
    spend.extend([1, 0x6a]);
    spend.extend(u32::MAX.to_le_bytes());
    spend.push(1);
    spend.extend(0i64.to_le_bytes());
    spend.extend([1, 0x51]);
    spend.extend(0u32.to_le_bytes());
    let mut missing = 1i32.to_le_bytes().to_vec();
    missing.push(1);
    missing.extend([0xee; 32]);
    missing.extend(0u32.to_le_bytes());
    missing.push(0);
    missing.extend(u32::MAX.to_le_bytes());
    missing.push(1);
    missing.extend(0i64.to_le_bytes());
    missing.extend([1, 0x51]);
    missing.extend(0u32.to_le_bytes());
    // 1,000 DASH: far past block 101's subsidy, and within `MAX_MONEY`.
    let overpaying = coinbase(101, 100_000_000_000);
    for txs in [
        &[coinbase(101, 0), spend.clone()][..],
        &[coinbase(101, 0), spend.clone(), missing],
        &[overpaying, spend],
    ] {
        blocks.push(mine_txs(&blocks[99].hash, GENESIS_TIME + 60 * 101, txs));
    }
    blocks
}

/// CH3a a9 (DEC-323, DEC-450): [`script_failure_chain`] submitted to dashd and rdashd at the same
/// `-par`, for each of `-par=1`, `2`, `16` and `-1000` (no threads, 1, 15 and none, on any host).
/// For each block 101, each binary must (a) reject it at every `-par`, with a reason (not
/// `submitblock`'s `null`, `duplicate`, `duplicate-invalid` or `inconclusive`,
/// `rpc/mining.cpp:1039-1063`) and its 100 blocks kept, and (b) give one reason at every `-par`
/// with threads and one at every `-par` without; (c) every reply of rdashd's equals dashd's at
/// the same `-par`, the blocks 101's byte for byte. dashd's two reasons differ for each, so (c)
/// sees whether the threads check the scripts. Without threads the failing input's reason comes
/// at once (`validation.cpp:2223`, `:2752-2758`). With them the first block's is
/// `block-validation-failed` (`:2778-2781`); the second's is its third transaction's
/// `bad-txns-inputs-missingorspent` (`:2698-2702`), which the loop returns before `Wait`; and the
/// third's is `block-validation-failed` again, since `Wait` precedes the coinbase's
/// `bad-cb-amount` (`:2846-2849`).
#[test]
fn submitblock_reasons_match_dashd_v24_at_every_par() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let pars = [("-par=1", false), ("-par=2", true), ("-par=16", true), ("-par=-1000", false)];
    let mut chain = None;
    // Per `-par`: dashd's and rdashd's replies to the blocks 101's `submitblock`, then
    // `getblockcount`.
    let mut runs = Vec::new();
    for (par, _) in pars {
        let [core_datadir, our_datadir] =
            ["core", "ours"].map(|name| home.path().join(format!("{name}{par}")));
        let (core_port, our_port) = (free_port(), free_port());
        let start = |binary: &Path, datadir: &Path, port: u16, extra: &[&str]| {
            std::fs::create_dir_all(datadir).unwrap();
            let rpcport = format!("-rpcport={port}");
            let args = [&["-printtoconsole=0", par, &rpcport][..], &credentials, extra].concat();
            node(regtest_on(binary, home.path(), datadir, &args), datadir)
        };
        let core = start(&dashd, &core_datadir, core_port, &["-disablewallet"]);
        let ours = start(Path::new(RDASHD), &our_datadir, our_port, &[]);
        let chain: &Vec<Mined> =
            chain.get_or_insert_with(|| script_failure_chain(&genesis(core_port)));
        for (height, block) in (1..).zip(&chain[..100]) {
            let params = format!(r#"["{}"]"#, block.hex);
            let [theirs, mine] =
                [core_port, our_port].map(|port| rpc(port, "u:p", "submitblock", &params).body);
            assert_eq!(mine, theirs, "(c) {par}, block {height}");
            assert!(
                theirs.contains(r#""result":null,"error":null"#),
                "{par}, block {height}: {theirs}"
            );
        }
        let submit = |block: &Mined| {
            post(&format!(r#"{{"method":"submitblock","params":["{}"],"id":1}}"#, block.hex))
        };
        let tip = post(r#"{"method":"getblockcount","id":1}"#);
        let requests = [submit(&chain[100]), submit(&chain[101]), submit(&chain[102]), tip];
        runs.push(
            [core_port, our_port]
                .map(|port| requests.each_ref().map(|request| exchange(port, request))),
        );
        core.stop_ok(libc::SIGTERM);
        ours.stop_ok(libc::SIGTERM);
    }
    let result = |reply: &[u8]| Reply::parse(reply).result().to_owned();
    // A reason, not `null` or `submitblock`'s other answers.
    let rejected = |r: &str| {
        !["null", "\"duplicate\"", "\"duplicate-invalid\"", "\"inconclusive\""].contains(&r)
    };
    let script = "mandatory-script-verify-flag-failed (OP_RETURN was encountered)";
    let dashd_s = [
        [script, "block-validation-failed"],
        [script, "bad-txns-inputs-missingorspent"],
        [script, "block-validation-failed"],
    ];
    for (i, binary) in ["dashd", "rdashd"].into_iter().enumerate() {
        for (block, dashd_s) in dashd_s.iter().enumerate() {
            let mut reasons = [None, None];
            for ((par, threaded), replies) in pars.iter().zip(&runs) {
                let [submitted, tip] = [block, 3].map(|r| result(&replies[i][r]));
                assert!(
                    rejected(&submitted) && tip == "100",
                    "(a) {binary} {par} 101#{block}: {submitted}, {tip}"
                );
                let reason =
                    reasons[usize::from(*threaded)].get_or_insert_with(|| submitted.clone());
                assert_eq!(&submitted, reason, "(b) {binary} {par} 101#{block}");
            }
            if binary == "dashd" {
                let want = dashd_s.map(|r| Some(format!("\"{r}\"")));
                assert_eq!(reasons, want, "dashd's reasons for 101#{block}");
            }
        }
    }
    for ((par, _), [theirs, mine]) in pars.iter().zip(&runs) {
        let show =
            |replies: &[Vec<u8>; 4]| replies.each_ref().map(|b| b.escape_ascii().to_string());
        assert_eq!(show(mine), show(theirs), "(c) {par}");
    }
}

/// The node's getpeerinfo, each peer's keys and values.
fn peer_objects(port: u16) -> Vec<Vec<(String, UniValue)>> {
    let reply = rpc(port, "u:p", "getpeerinfo", "[]");
    let Some(UniValue::Arr(peers)) = UniValue::read(reply.result().as_bytes()) else {
        panic!("{}", reply.body)
    };
    let object = |peer| match peer {
        UniValue::Obj(peer) => peer,
        _ => panic!("{}", reply.body),
    };
    peers.into_iter().map(object).collect()
}

/// `key`'s value in a getpeerinfo object.
fn peer_field<'a>(peer: &'a [(String, UniValue)], key: &str) -> &'a UniValue {
    peer.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("{key} in {peer:?}"))
}

/// The node's one peer once it has answered a ping, else `None`.
fn pinged_peer(port: u16) -> Option<Vec<(String, UniValue)>> {
    // Exactly one: a reconnection would renumber the peer.
    match <[_; 1]>::try_from(peer_objects(port)) {
        Ok([peer]) if peer.iter().any(|(k, _)| k == "minping") => Some(peer),
        _ => None,
    }
}

/// The keys whose values depend on the connection's timing, ports or keys: compared by type.
/// The message maps are too: which messages A sends a peer depends on timing (it syncs headers
/// from one peer at a time), and rdashd's v2 sends use the long message-type format where Core
/// uses short IDs, since dash-p2p's `record_version` never sets the transport's peer version as
/// `CNode::SetCommonVersion` does (2b-D38), so their counts differ. A key's presence is still
/// compared: `pingwait` could differ if the wait ended as A's next ping went out, two minutes
/// on, which the wait never comes near.
const VOLATILE: &[&str] = &[
    "addrbind",
    "addrlocal",
    "lastsend",
    "lastrecv",
    "bytessent",
    "bytesrecv",
    "conntime",
    "pingtime",
    "minping",
    "pingwait",
    "session_id",
    "bytessent_per_msg",
    "bytesrecv_per_msg",
];

/// DIFF-RPC's `getpeerinfo` with a peer (DEC-279): dashd B and rdashd each `-connect` to the
/// same dashd A, and each node's view of A must match: the same keys in the same order
/// (`banscore` included), the same JSON types, and the same values but for the [`VOLATILE`]
/// ones.
#[test]
fn getpeerinfo_matches_dashd_v24_on_a_shared_peer() {
    let dashd = require_dashd();
    let (home, datadir) = home_and_datadir();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let p2p_port = free_port();
    let core = |name: &str, args: &[&str]| {
        let dir = home.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let base =
            [&datadir_arg(&dir), "-regtest", "-printtoconsole=0", "-disablewallet", &rpcport];
        let cmd = command(&dashd, home.path(), &[&base[..], args, &credentials].concat());
        (node(cmd, &dir), port)
    };
    let (hub, _) = core(
        "a",
        &[&format!("-port={p2p_port}"), "-bind=127.0.0.1", "-listenonion=0", "-dnsseed=0"],
    );
    let connect = format!("-connect=127.0.0.1:{p2p_port}");
    // `banscore` shows the node passes `-deprecatedrpc` on to the command.
    let client = [connect.as_str(), "-listen=0", "-deprecatedrpc=banscore"];
    // rdashd's default until P5 is dashd's `-blocksonly=1` (DEC-322, P5a D23): no `senddsq`.
    let (core_b, b_port) = core("b", &[&client[..], &["-blocksonly=1"]].concat());
    let our_port = free_port();
    let our_rpcport = format!("-rpcport={our_port}");
    let ours = node(
        regtest(
            home.path(),
            &datadir,
            &[&["-printtoconsole=0", &our_rpcport][..], &client, &credentials].concat(),
        ),
        &datadir,
    );

    let mut views = None;
    wait_until("both nodes' peer answered a ping", || {
        views = pinged_peer(b_port).zip(pinged_peer(our_port));
        views.is_some()
    });
    let (theirs, mine) = views.unwrap();
    let mut failures = object_differences(&theirs, &mine, |key, value, our_value| {
        if VOLATILE.contains(&key) {
            value.value_type() != our_value.value_type()
        } else if key == "timeoffset" {
            // `nTime - GetTime()` in whole seconds: 0, or −1 when a second boundary falls
            // between A's `version` and its processing, on each side independently.
            let offset =
                |v: &UniValue| v.write().parse::<i64>().ok().filter(|o| (-1..=0).contains(o));
            offset(value).is_none() || offset(our_value).is_none()
        } else {
            value != our_value
        }
    });
    // The maps' shape: `std::map`'s order, zero counts left out.
    for (key, value) in mine.iter().filter(|(k, _)| k.ends_with("_per_msg")) {
        let UniValue::Obj(counts) = value else {
            continue;
        };
        let sorted = counts.windows(2).all(|w| w[0].0 < w[1].0);
        if !sorted || counts.iter().any(|(_, n)| !matches!(n.write().parse::<u64>(), Ok(1..))) {
            failures.push(format!("{key}: {}", value.write()));
        }
    }
    // BL-391: each node's network view holds its one outbound connection.
    let (theirs, mine) = (network_info(b_port), network_info(our_port));
    assert!(theirs.iter().any(|(k, v)| k == "connections_out" && v.write() == "1"), "{theirs:?}");
    failures.extend(
        network_info_differences(&theirs, &mine).into_iter().map(|d| format!("getnetworkinfo {d}")),
    );
    ours.stop_ok(libc::SIGTERM);
    core_b.stop_ok(libc::SIGTERM);
    hub.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `getnetworkinfo`'s object.
fn network_info(port: u16) -> Vec<(String, UniValue)> {
    let reply = rpc(port, "u:p", "getnetworkinfo", "[]");
    let Some(UniValue::Obj(info)) = UniValue::read(reply.result().as_bytes()) else {
        panic!("{}", reply.body)
    };
    info
}

/// How two objects differ: their keys and order, then each value `differs` rejects.
fn object_differences(
    theirs: &[(String, UniValue)],
    mine: &[(String, UniValue)],
    differs: impl Fn(&str, &UniValue, &UniValue) -> bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    let keys = |o: &[(String, UniValue)]| o.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    if keys(theirs) != keys(mine) {
        failures.push(format!("keys:\n  dashd  {:?}\n  rdashd {:?}", keys(theirs), keys(mine)));
    }
    for (key, value) in theirs {
        let Some((_, our_value)) = mine.iter().find(|(k, _)| k == key) else {
            continue;
        };
        if differs(key, value, our_value) {
            failures.push(format!("{key}: dashd {} rdashd {}", value.write(), our_value.write()));
        }
    }
    failures
}

/// The keys that name the node rather than its state (BL-391 D-rows), compared by type:
/// rdashd's own version, build and user agent (DEC-83, DEC-201), and `warnings`, where dashd
/// rc.3 warns of its pre-release build and rdashd, which reports its pin as a release
/// (`CLIENT_VERSION_IS_RELEASE`, BL-175), has nothing to say.
const IDENTITY: &[&str] = &["version", "buildversion", "subversion", "warnings"];

/// How two `getnetworkinfo` objects differ: [`object_differences`], the [`IDENTITY`] keys by
/// type, which are then checked against rdashd's own version.
fn network_info_differences(
    theirs: &[(String, UniValue)],
    mine: &[(String, UniValue)],
) -> Vec<String> {
    let mut failures = object_differences(theirs, mine, |key, value, our_value| {
        if IDENTITY.contains(&key) {
            value.value_type() != our_value.value_type()
        } else {
            value != our_value
        }
    });
    // Core's `CLIENT_VERSION` formula (clientversion.h:33-36), over rdashd's version.
    let part = |p: &str| p.parse::<i64>().unwrap();
    let version = 10_000 * part(env!("CARGO_PKG_VERSION_MAJOR"))
        + 100 * part(env!("CARGO_PKG_VERSION_MINOR"))
        + part(env!("CARGO_PKG_VERSION_PATCH"));
    let own = [
        ("version", version.to_string()),
        ("buildversion", format!(r#""v{}""#, env!("CARGO_PKG_VERSION"))),
        ("subversion", format!(r#""/rdashd:{}/""#, env!("CARGO_PKG_VERSION"))),
        ("warnings", r#""""#.to_owned()),
    ];
    for (key, expected) in own {
        let found = mine.iter().find(|(k, _)| k == key).map(|(_, v)| v.write());
        if found.as_deref() != Some(expected.as_str()) {
            failures.push(format!("{key}: rdashd {found:?}, expected {expected}"));
        }
    }
    failures
}

/// One of the node's `getnetworkinfo` values, as JSON text.
fn network_value(port: u16, key: &str) -> String {
    let info = network_info(port);
    info.iter().find(|(k, _)| k == key).map(|(_, v)| v.write()).unwrap()
}

/// A node of a differential test in `home/name`, rdashd or dashd (`-disablewallet`, and
/// rdashd's default `-blocksonly=1` (DEC-322) unless `extra` sets it), with `u:p` credentials:
/// the node, its RPC port, its datadir and its P2P port, which it listens on when `extra` binds.
fn start_node(
    home: &Path,
    binary: &Path,
    name: &str,
    extra: &[&str],
) -> (Running, u16, std::path::PathBuf, u16) {
    let dir = home.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let port = free_port();
    let rpcport = format!("-rpcport={port}");
    let p2p = free_port();
    let p2p_port = format!("-port={p2p}");
    let mut args = vec!["-printtoconsole=0", rpcport.as_str()];
    if binary != Path::new(RDASHD) {
        args.push("-disablewallet");
        if !extra.iter().any(|a| a.starts_with("-blocksonly")) {
            args.push("-blocksonly=1");
        }
    }
    if extra.iter().any(|a| a.starts_with("-bind=")) {
        args.push(&p2p_port);
    }
    args.extend(extra);
    args.extend(["-rpcuser=u", "-rpcpassword=p"]);
    (node(regtest_on(binary, home, &dir, &args), &dir), port, dir, p2p)
}

/// DIFF-RPC's `getnetworkinfo` (BL-391, DEC-451): dashd and rdashd started with the same options
/// must show the same network, but for the [`IDENTITY`] keys:
/// - listening on loopback (`-bind`, each node on its own `-port`), without `-discover` so the
///   host's routable addresses (each with its node's port) stay out of `localaddresses`;
/// - `-listen=0`;
/// - `-networkactive=0` with a `-connect` peer, a dashd hub: no connection until
///   `setnetworkactive true` opens one, and `setnetworkactive false` closes it again, both
///   nodes answering the calls with the same bytes (rc.3's `rpc_net.py` `test_getnetworkinfo`);
///   with `-debug=netconn`, rdashd logs Core's `NET_NETCONN` line for the dropped peer;
/// - serving one inbound peer, a dashd that `-connect`s to both, with `-blocksonly=0`: the
///   inbound counts and `localrelay` true (review r1 finding 1);
/// - each form rc.3's `feature_config_args.py` `test_networkactive` starts with:
///   `-networkactive`, `-networkactive=1`, `-nonetworkactive` and `-nonetworkactive=1`, with
///   dashd's `networkactive` checked too (review r1 finding 7).
///
/// rdashd's log must hold Core's unprefixed `SetNetworkActive` lines as rc.3's
/// `feature_config_args.py` and `rpc_net.py` assert them: init's, then each call's. dashd runs
/// with rdashd's default `-blocksonly=1` (DEC-322) unless the config sets it.
///
/// The P8a configs check `networks` (reachable, proxy, `proxy_randomize_credentials`) and
/// `localaddresses`: `-proxy` alone and with `-onion` and `-proxyrandomize=0`, with
/// `-cjdnsreachable` (and `=0`, which Core counts as set), a `unix:` proxy, `-onlynet=ipv4`,
/// `-onlynet=onion` with `-onion`, `-noonion`, and `-externalip`. No node dials (`-listen=0` and regtest has no seeds), so the
/// loopback proxy addresses are never contacted.
#[test]
fn getnetworkinfo_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let start =
        |binary: &Path, name: &str, extra: &[&str]| start_node(home.path(), binary, name, extra);
    let (hub, .., hub_port) = start(&dashd, "hub", &["-bind=127.0.0.1", "-listenonion=0"]);
    let connect = format!("-connect=127.0.0.1:{hub_port}");
    let listening = ["-bind=127.0.0.1", "-listenonion=0", "-discover=0"];
    let configs: [(&str, &[&str]); 17] = [
        ("listening", &listening),
        ("listen0", &["-listen=0"]),
        ("networkactive0", &[&connect, "-listen=0", "-networkactive=0", "-debug=netconn"]),
        ("served", &[&listening[..], &["-blocksonly=0"]].concat()),
        ("networkactive", &["-listen=0", "-networkactive"]),
        ("networkactive1", &["-listen=0", "-networkactive=1"]),
        ("nonetworkactive", &["-listen=0", "-nonetworkactive"]),
        ("nonetworkactive1", &["-listen=0", "-nonetworkactive=1"]),
        ("proxy", &["-listen=0", "-proxy=127.0.0.1:19050"]),
        (
            "proxyonion",
            &["-listen=0", "-proxy=127.0.0.1:19050", "-onion=127.0.0.1:19051", "-proxyrandomize=0"],
        ),
        ("cjdnsreachable", &["-listen=0", "-proxy=[::1]:19050", "-cjdnsreachable"]),
        // Core tests `IsArgSet`: CJDNS stays reachable.
        ("cjdnsreachable0", &["-listen=0", "-cjdnsreachable=0"]),
        ("unixproxy", &["-listen=0", "-proxy=unix:/nonexistent/p8.sock"]),
        ("onlynetipv4", &["-listen=0", "-onlynet=ipv4"]),
        ("onlynetonion", &["-listen=0", "-onlynet=onion", "-onion=127.0.0.1:19051"]),
        ("noonion", &["-listen=0", "-proxy=127.0.0.1:19050", "-noonion"]),
        ("externalip", &["-listen=0", "-externalip=1.2.3.4", "-externalip=[2001:db9::1]:5"]),
    ];
    let mut failures = Vec::new();
    for (name, extra) in configs {
        let (core, core_port, _, core_p2p) = start(&dashd, &format!("{name}-core"), extra);
        let (ours, our_port, our_dir, our_p2p) =
            start(Path::new(RDASHD), &format!("{name}-ours"), extra);
        let mut client = None;
        if name == "served" {
            let connects = [core_p2p, our_p2p].map(|p| format!("-connect=127.0.0.1:{p}"));
            let args = [&connects[0], &connects[1], "-listen=0"];
            let (served_client, client_port, ..) = start(&dashd, "served-client", &args);
            client = Some(served_client);
            // Waited for on the client's side, so a miscount on either node shows as a
            // difference: a peer's `subver` comes from its version message, which each node
            // sends once it has attached the inbound peer.
            wait_until("the client has each node's version", || {
                let peers = rpc(client_port, "u:p", "getpeerinfo", "[]").body;
                peers.matches(r#""subver":"/"#).count() == 2
            });
        }
        let compare = |step: &str| {
            let (theirs, mine) = (network_info(core_port), network_info(our_port));
            let differences = network_info_differences(&theirs, &mine);
            differences.into_iter().map(|d| format!("{name} {step}: {d}")).collect::<Vec<_>>()
        };
        failures.extend(compare("start"));
        let inactive = ["-networkactive=0", "-nonetworkactive", "-nonetworkactive=1"];
        let active = !extra.iter().any(|a| inactive.contains(a));
        assert_eq!(network_value(core_port, "networkactive"), active.to_string(), "{name} dashd");
        let mut logged = vec![if active {
            "SetNetworkActive: true\n"
        } else {
            "SetNetworkActive: false\n"
        }];
        if name == "networkactive0" {
            for (state, outbound) in [("true", "1"), ("false", "0")] {
                let params = format!("[{state}]");
                let [theirs, mine] = [core_port, our_port]
                    .map(|port| rpc(port, "u:p", "setnetworkactive", &params).body);
                assert!(theirs.contains(&format!(r#""result":{state}"#)), "{theirs}");
                if theirs != mine {
                    failures.push(format!(
                        "{name} setnetworkactive {state}:\n  dashd  {theirs}\n  rdashd {mine}"
                    ));
                }
                wait_until(&format!("{name}: {outbound} outbound after {state}"), || {
                    [core_port, our_port]
                        .iter()
                        .all(|&port| network_value(port, "connections_out") == outbound)
                });
                failures.extend(compare(&format!("after setnetworkactive {state}")));
                logged.push(if state == "true" {
                    "SetNetworkActive: true\n"
                } else {
                    "SetNetworkActive: false\n"
                });
            }
        }
        // The line follows the timestamp directly: no `[net:info] ` prefix.
        let log = read(&debug_log(&our_dir));
        let calls: Vec<&str> = log
            .split_inclusive('\n')
            .filter_map(|line| line.split_once(' ').map(|(_, rest)| rest))
            .filter(|rest| rest.contains("SetNetworkActive"))
            .collect();
        if calls != logged {
            failures.push(format!("{name}: rdashd logged {calls:?}, expected {logged:?}"));
        }
        // Core: net.cpp:2103, `LogPrint(BCLog::NET_NETCONN, …)`.
        let dropped = "[net|netconn] Network not active, dropping peer=";
        if name == "networkactive0"
            && !log
                .lines()
                .any(|l| l.split_once(' ').is_some_and(|(_, rest)| rest.starts_with(dropped)))
        {
            failures.push(format!("{name}: rdashd did not log {dropped:?}"));
        }
        if let Some(client) = client {
            client.stop_ok(libc::SIGTERM);
        }
        core.stop_ok(libc::SIGTERM);
        ours.stop_ok(libc::SIGTERM);
    }
    hub.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The node's peers, each as `<connection_type>/<transport_protocol_type>`.
fn peer_types(port: u16) -> Vec<String> {
    let text =
        |peer: &[(String, UniValue)], key| peer_field(peer, key).get_str().unwrap().to_owned();
    peer_objects(port)
        .iter()
        .map(|peer| {
            format!("{}/{}", text(peer, "connection_type"), text(peer, "transport_protocol_type"))
        })
        .collect()
}

/// The same calls to a dashd and an rdashd, and how their replies and peers differed.
struct Diff {
    ports: [u16; 2],
    failures: Vec<String>,
}

impl Diff {
    /// `method` with `params` on both nodes: dashd's status and body, and a failure unless
    /// rdashd's are the same.
    fn same(&mut self, step: &str, method: &str, params: &str) -> (u16, String) {
        let [theirs, mine] = self.ports.map(|port| {
            let reply = rpc(port, "u:p", method, params);
            (reply.status, reply.body)
        });
        if theirs != mine {
            self.failures.push(format!(
                "{step}: {method} {params}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"
            ));
        }
        theirs
    }

    /// Waits until each node's peers pass `done`. The failures recorded so far come first: a step
    /// one node refused would otherwise show only as this wait running out of patience, which
    /// names each node's last peers.
    fn wait_peers(&self, what: &str, done: impl Fn(&[String]) -> bool) {
        assert!(self.failures.is_empty(), "before {what}:\n{}", self.failures.join("\n"));
        let deadline = Instant::now() + PATIENCE;
        loop {
            let [theirs, mine] = self.ports.map(peer_types);
            if done(&theirs) && done(&mine) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// As [`Diff::same`], with each node's own first peer's address: `params` builds the call from
    /// it, and the replies are compared with it replaced by `<inbound>`. Returns dashd's.
    fn same_per_peer(
        &mut self,
        step: &str,
        method: &str,
        params: impl Fn(&str) -> String,
    ) -> (u16, String) {
        let [theirs, mine] = self.ports.map(|port| {
            let (_, peer) = first_peer(port);
            let reply = rpc(port, "u:p", method, &params(&peer));
            (reply.status, reply.body.replace(&peer, "<inbound>"))
        });
        if theirs != mine {
            self.failures
                .push(format!("{step}: {method}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
        }
        theirs
    }

    /// As [`Diff::same`], with each node's own first peer's id: `params` builds the call from it.
    /// The ids are not compared. Returns dashd's.
    fn same_per_id(
        &mut self,
        step: &str,
        method: &str,
        params: impl Fn(i64) -> String,
    ) -> (u16, String) {
        let [theirs, mine] = self.ports.map(|port| {
            let reply = rpc(port, "u:p", method, &params(first_peer(port).0));
            (reply.status, reply.body)
        });
        if theirs != mine {
            self.failures
                .push(format!("{step}: {method}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
        }
        theirs
    }

    /// Each node's peers must be `expected`.
    fn expect_peers(&mut self, step: &str, expected: &[&str]) {
        for (port, who) in self.ports.into_iter().zip(["dashd", "rdashd"]) {
            let found = peer_types(port);
            if found != expected {
                self.failures.push(format!("{step}: {who} has {found:?}, expected {expected:?}"));
            }
        }
    }
}

/// DIFF-RPC's `addnode` (BL-394): the same calls to dashd and rdashd, each node with its own
/// datadir and no other peer, must answer the same status and bytes, and leave the same
/// connections to a dashd hub. With v2 on:
/// - an unknown command (its help, `-1`), and the server's arity and type errors;
/// - `onetry` with the default transport: one manual connection, listed when it returns, v2
///   once the handshake is done; `onetry` to a second hub with `v2transport` false: v1;
/// - `add` (null), the same node again and its IPv4-mapped spelling (`-23`: `AddNode` compares
///   numeric addresses); `onetry` to the connected node and to a closed port (null, nothing
///   opened);
/// - `remove` by the mapped spelling while the node is listed (`-24`: `RemoveAddedNode`
///   compares names), by name (null; the connection stays), and again (`-24`);
/// - `add` of a third hub with `v2transport` false: the added-node thread opens it as v1.
///
/// With `-v2transport=0 -maxconnections=0`:
/// - `v2transport` asked for: `-8` for every command, `remove` included;
/// - `onetry`: a v1 manual connection, listed when it returns: it takes no outbound slot;
/// - `onetry` by name (`ExistsNode`) and by the mapped spelling (the address match): nothing;
/// - after `setnetworkactive false` has closed it, `onetry` opens nothing;
/// - after `setnetworkactive true`, `add`: the added-node thread opens a v1 manual connection,
///   on its own slots.
///
/// With `-v2transport=0`, a node string holding a NUL: Core's `OpenNetworkConnection` takes
/// `c_str()`, so `onetry` and the added-node thread connect to the name before the NUL, while
/// `AddNode` and `RemoveAddedNode` keep and compare the whole string:
/// - `onetry "<hub2>\0junk"` connects to the second hub; `onetry "<hub2>\0other"` opens nothing;
/// - `add "<hub3>\0junk"` connects to the third hub; the same string again is `-23`; `add
///   "<hub3>"` is null (the names differ, and a name with a NUL has no numeric address);
/// - `remove "<hub3>\0other"` is `-24`; the whole string, then `"<hub3>"`, are null.
#[test]
fn addnode_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let (node, port, ..) = start_node(home.path(), binary, name, extra);
        (node, port)
    };
    let start_hub = |name: &str| {
        let (node, .., port) =
            start_node(home.path(), &dashd, name, &["-bind=127.0.0.1", "-listenonion=0"]);
        (node, port)
    };
    let (hub, hub_port) = start_hub("hub");
    let [(hub2, hub2_port), (hub3, hub3_port)] = ["hub2", "hub3"].map(start_hub);
    let (hub_addr, hub2_addr, hub3_addr) = (
        format!("127.0.0.1:{hub_port}"),
        format!("127.0.0.1:{hub2_port}"),
        format!("127.0.0.1:{hub3_port}"),
    );
    let mapped = format!("[::ffff:127.0.0.1]:{hub_port}");
    let closed = format!("127.0.0.1:{}", free_port());

    let (core, core_port) = start(&dashd, "core", &["-listen=0"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &["-listen=0"]);
    let mut diff = Diff {
        ports: [core_port, our_port],
        failures: Vec::new(),
    };
    let addnode = |diff: &mut Diff, step: &str, params: String| diff.same(step, "addnode", &params);
    let (status, help) = addnode(&mut diff, "unknown command", format!(r#"["{hub_addr}","Add"]"#));
    assert!(status == 500 && help.contains(r#""code":-1"#), "{help}");
    addnode(&mut diff, "one argument", format!(r#"["{hub_addr}"]"#));
    addnode(&mut diff, "a number for the node", r#"[1,"add"]"#.into());
    addnode(&mut diff, "a string for v2transport", format!(r#"["{hub_addr}","add","yes"]"#));
    assert_eq!(addnode(&mut diff, "onetry", format!(r#"["{hub_addr}","onetry"]"#)).0, 200);
    for (port, who) in diff.ports.into_iter().zip(["dashd", "rdashd"]) {
        let found = peer_types(port);
        let listed = matches!(&found[..], [one] if one.starts_with("manual/"));
        assert!(listed, "{who} when onetry has returned: {found:?}");
    }
    diff.wait_peers("v2: the handshake (hub) on both", |found| found == ["manual/v2"]);
    addnode(
        &mut diff,
        "onetry with v2transport false",
        format!(r#"["{hub2_addr}","onetry",false]"#),
    );
    diff.expect_peers("when the v1 onetry has returned", &["manual/v2", "manual/v1"]);
    let mut errors = vec![];
    for (step, params) in [
        ("add", format!(r#"["{hub_addr}","add"]"#)),
        ("add again", format!(r#"["{hub_addr}","add"]"#)),
        ("add the mapped spelling", format!(r#"["{mapped}","add"]"#)),
        ("onetry the connected node", format!(r#"["{hub_addr}","onetry"]"#)),
        ("onetry a closed port", format!(r#"["{closed}","onetry"]"#)),
        ("remove by the mapped spelling", format!(r#"["{mapped}","remove"]"#)),
        ("remove", format!(r#"["{hub_addr}","remove"]"#)),
        ("remove again", format!(r#"["{hub_addr}","remove"]"#)),
    ] {
        let (_, body) = addnode(&mut diff, step, params);
        errors.push(["-23", "-24"].into_iter().find(|c| body.contains(&format!(r#""code":{c}"#))));
    }
    let codes = [None, Some("-23"), Some("-23"), None, None, Some("-24"), None, Some("-24")];
    assert_eq!(errors, codes, "dashd's replies");
    diff.expect_peers("after onetry, add and remove", &["manual/v2", "manual/v1"]);
    addnode(&mut diff, "add with v2transport false", format!(r#"["{hub3_addr}","add",false]"#));
    diff.wait_peers("v2: the v1 added node (hub3) on both", |found| {
        found.len() == 3 && !found.iter().any(|t| t.ends_with("/detecting"))
    });
    diff.expect_peers("after the v1 add", &["manual/v2", "manual/v1", "manual/v1"]);
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    let v1 = ["-listen=0", "-v2transport=0", "-maxconnections=0"];
    let (core, core_port) = start(&dashd, "core-v1", &v1);
    let (ours, our_port) = start(Path::new(RDASHD), "ours-v1", &v1);
    diff.ports = [core_port, our_port];
    for command in ["add", "remove", "onetry"] {
        let (_, refused) =
            addnode(&mut diff, "v1 v2 asked for", format!(r#"["{hub_addr}","{command}",true]"#));
        assert!(refused.contains(r#""code":-8"#), "{refused}");
    }
    addnode(&mut diff, "v1 onetry", format!(r#"["{hub_addr}","onetry"]"#));
    diff.expect_peers("v1 when onetry has returned", &["manual/v1"]);
    addnode(&mut diff, "v1 onetry by name", format!(r#"["{hub_addr}","onetry"]"#));
    addnode(&mut diff, "v1 onetry by the mapped spelling", format!(r#"["{mapped}","onetry"]"#));
    diff.expect_peers("v1 after the onetries", &["manual/v1"]);
    diff.same("v1 setnetworkactive false", "setnetworkactive", "[false]");
    diff.wait_peers("v1: the connection closed on both", <[String]>::is_empty);
    addnode(&mut diff, "v1 onetry while inactive", format!(r#"["{hub_addr}","onetry"]"#));
    diff.expect_peers("v1 onetry while inactive", &[]);
    diff.same("v1 setnetworkactive true", "setnetworkactive", "[true]");
    addnode(&mut diff, "v1 add", format!(r#"["{hub_addr}","add"]"#));
    diff.wait_peers("v1: the added node (hub) on both", |found| found == ["manual/v1"]);
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    let v1 = ["-listen=0", "-v2transport=0"];
    let (core, core_port) = start(&dashd, "core-nul", &v1);
    let (ours, our_port) = start(Path::new(RDASHD), "ours-nul", &v1);
    diff.ports = [core_port, our_port];
    addnode(&mut diff, "nul onetry", format!(r#"["{hub2_addr}\u0000junk","onetry"]"#));
    diff.expect_peers("nul when onetry has returned", &["manual/v1"]);
    addnode(&mut diff, "nul onetry again", format!(r#"["{hub2_addr}\u0000other","onetry"]"#));
    addnode(&mut diff, "nul add", format!(r#"["{hub3_addr}\u0000junk","add"]"#));
    diff.wait_peers("nul: the added node (hub3) on both", |found| {
        found == ["manual/v1", "manual/v1"]
    });
    let mut errors = vec![];
    for (step, params) in [
        ("nul add again", format!(r#"["{hub3_addr}\u0000junk","add"]"#)),
        ("nul add the name before it", format!(r#"["{hub3_addr}","add"]"#)),
        ("nul remove another string", format!(r#"["{hub3_addr}\u0000other","remove"]"#)),
        ("nul remove", format!(r#"["{hub3_addr}\u0000junk","remove"]"#)),
        ("nul remove the name before it", format!(r#"["{hub3_addr}","remove"]"#)),
    ] {
        let (_, body) = addnode(&mut diff, step, params);
        errors.push(["-23", "-24"].into_iter().find(|c| body.contains(&format!(r#""code":{c}"#))));
    }
    assert_eq!(errors, [Some("-23"), None, Some("-24"), None, None], "dashd's replies");
    diff.expect_peers("nul after the removes", &["manual/v1", "manual/v1"]);
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    for hub in [hub, hub2, hub3] {
        hub.stop_ok(libc::SIGTERM);
    }
    assert!(diff.failures.is_empty(), "{}", diff.failures.join("\n"));
}

/// The node's first peer's `id` and `addr`.
fn first_peer(port: u16) -> (i64, String) {
    let peers = peer_objects(port);
    let peer = peers.first().expect("a peer");
    let addr = peer_field(peer, "addr").get_str().unwrap().to_owned();
    (peer_field(peer, "id").get_int64().unwrap(), addr)
}

/// DIFF-RPC's `disconnectnode` and `getaddednodeinfo` (BL-427): the same calls to dashd and
/// rdashd, each node with its own datadir and dashd hubs, must answer the same status and
/// bytes. With `-v2transport=0 -listen=0`:
/// - `disconnectnode`'s forms: neither or both of address and id (`-32602`, the named forms
///   too), an address or id no peer has (`-29`, `""` included), a fractional id (`-1`), and
///   the server's type errors; `getaddednodeinfo` with nothing added (`[]`, `-24`);
/// - a hub added: listed connected and outbound, by its name only (the mapped spelling is
///   `-24`); removed, then `disconnectnode` by its address, then again at once (`-29`: the
///   marked peer is skipped) while getpeerinfo no longer lists it; reconnected by `onetry`,
///   `disconnectnode` by id, positional then named, drops it each time.
///
/// Listening, with a dashd that connects in: an added entry naming the inbound peer's address
/// is connected and inbound; an entry holding a NUL is never connected, though the added-node
/// thread connects to the name before the NUL, whose connection `disconnectnode` finds by that
/// name once the entry is removed; a closed port's entry is unconnected. The inbound peer's
/// port differs per node, so those replies are compared with it replaced.
#[test]
fn disconnectnode_and_getaddednodeinfo_match_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let hub_args = ["-bind=127.0.0.1", "-listenonion=0", "-v2transport=0"];
    let [(hub, _, _, hub_port), (hub2, hub2_rpc, _, _), (hub3, _, _, hub3_port)] =
        ["hub", "hub2", "hub3"].map(|name| start_node(home.path(), &dashd, name, &hub_args));
    let hub_addr = format!("127.0.0.1:{hub_port}");
    let hub3_addr = format!("127.0.0.1:{hub3_port}");
    let mapped = format!("[::ffff:127.0.0.1]:{hub_port}");
    let closed = format!("127.0.0.1:{}", free_port());

    let v1 = ["-listen=0", "-v2transport=0"];
    let (core, core_port, ..) = start_node(home.path(), &dashd, "core", &v1);
    let (ours, our_port, ..) = start_node(home.path(), Path::new(RDASHD), "ours", &v1);
    let mut diff = Diff {
        ports: [core_port, our_port],
        failures: Vec::new(),
    };
    for (step, params) in [
        ("disconnectnode without arguments", "[]".to_owned()),
        ("disconnectnode with two nulls", "[null,null]".to_owned()),
        ("disconnectnode with both", format!(r#"["{hub_addr}",0]"#)),
        ("disconnectnode named both", format!(r#"{{"address":"{hub_addr}","nodeid":0}}"#)),
        ("disconnectnode named neither", "{}".to_owned()),
        ("disconnectnode an unknown address", format!(r#"["{hub_addr}"]"#)),
        ("disconnectnode the empty address", r#"[""]"#.to_owned()),
        ("disconnectnode an unknown id", r#"["",99]"#.to_owned()),
        ("disconnectnode an unknown named id", r#"{"nodeid":99}"#.to_owned()),
        ("disconnectnode a negative id", r#"[null,-1]"#.to_owned()),
        ("disconnectnode a fractional id", r#"["",1.5]"#.to_owned()),
        ("disconnectnode a numeric address", "[1]".to_owned()),
        ("disconnectnode a string id", r#"["","1"]"#.to_owned()),
        ("disconnectnode three arguments", r#"["",1,2]"#.to_owned()),
        ("getaddednodeinfo with nothing added", "[]".to_owned()),
        ("getaddednodeinfo an unknown node", format!(r#"["{hub_addr}"]"#)),
        ("getaddednodeinfo a number", "[1]".to_owned()),
    ] {
        let method = step.split(' ').next().unwrap();
        diff.same(step, method, &params);
    }
    diff.same("add the hub", "addnode", &format!(r#"["{hub_addr}","add"]"#));
    diff.wait_peers("the added hub on both", |found| found == ["manual/v1"]);
    let (status, body) = diff.same("getaddednodeinfo the hub connected", "getaddednodeinfo", "[]");
    let connected_hub = format!(
        r#"[{{"addednode":"{hub_addr}","connected":true,"addresses":[{{"address":"{hub_addr}","connected":"outbound"}}]}}]"#
    );
    assert_eq!(
        (status, body.as_str()),
        (200, &*format!("{{\"result\":{connected_hub},\"error\":null,\"id\":1}}\n"))
    );
    for (step, params) in [
        ("getaddednodeinfo the hub by name", format!(r#"["{hub_addr}"]"#)),
        ("getaddednodeinfo the hub's mapped spelling", format!(r#"["{mapped}"]"#)),
    ] {
        diff.same(step, "getaddednodeinfo", &params);
    }
    // The added-node thread retries 60 s after its dial (net.cpp:3391-3393), which a slow run
    // could reach; once the hub is removed, only the `onetry`s below reconnect it.
    diff.same("remove the hub", "addnode", &format!(r#"["{hub_addr}","remove"]"#));
    diff.same("disconnectnode the hub", "disconnectnode", &format!(r#"["{hub_addr}"]"#));
    diff.same("disconnectnode the hub again", "disconnectnode", &format!(r#"["{hub_addr}"]"#));
    diff.expect_peers("when disconnectnode has returned", &[]);
    for form in ["positional", "named"] {
        diff.same(
            &format!("onetry before the {form} id"),
            "addnode",
            &format!(r#"["{hub_addr}","onetry"]"#),
        );
        diff.wait_peers(&format!("the hub reconnected before the {form} id"), |found| {
            found == ["manual/v1"]
        });
        for port in diff.ports {
            let (id, _) = first_peer(port);
            let params = if form == "named" {
                format!(r#"{{"nodeid":{id}}}"#)
            } else {
                format!(r#"["",{id}]"#)
            };
            let reply = rpc(port, "u:p", "disconnectnode", &params);
            assert_eq!(reply.result(), "null", "{form} id {id} on {port}");
            let again = rpc(port, "u:p", "disconnectnode", &params);
            assert_eq!(
                again.error().1,
                r#"{"code":-29,"message":"Node not found in connected nodes"}"#
            );
        }
        diff.expect_peers(&format!("after the {form} id"), &[]);
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    let listening = ["-bind=127.0.0.1", "-listenonion=0", "-v2transport=0"];
    let (core, core_port, _, core_p2p) = start_node(home.path(), &dashd, "core-in", &listening);
    let (ours, our_port, _, our_p2p) =
        start_node(home.path(), Path::new(RDASHD), "ours-in", &listening);
    diff.ports = [core_port, our_port];
    for p2p in [core_p2p, our_p2p] {
        let reply =
            rpc(hub2_rpc, "u:p", "addnode", &format!(r#"["127.0.0.1:{p2p}","onetry",false]"#));
        assert_eq!(reply.result(), "null");
    }
    diff.wait_peers("the hub's inbound connection on both", |found| found == ["inbound/v1"]);
    let nul = format!(r#"{hub3_addr}\u0000junk"#);
    diff.same_per_peer("add the inbound peer", "addnode", |inbound| {
        format!(r#"["{inbound}","add"]"#)
    });
    let (_, inbound_info) =
        diff.same_per_peer("the inbound peer added", "getaddednodeinfo", |_| "[]".into());
    assert!(
        inbound_info.contains(r#""address":"<inbound>","connected":"inbound""#),
        "{inbound_info}"
    );
    diff.same("add a name with a NUL", "addnode", &format!(r#"["{nul}","add"]"#));
    diff.wait_peers("the added node before the NUL on both", |found| {
        found == ["inbound/v1", "manual/v1"]
    });
    diff.same("add a closed port", "addnode", &format!(r#"["{closed}","add"]"#));
    let (_, all) = diff.same_per_peer("the three added nodes", "getaddednodeinfo", |_| "[]".into());
    let unconnected =
        |node: &str| format!(r#"{{"addednode":"{node}","connected":false,"addresses":[]}}"#);
    assert!(all.contains(&unconnected(&nul)) && all.contains(&unconnected(&closed)), "{all}");
    for (step, node) in [
        ("getaddednodeinfo the NUL entry", nul.as_str()),
        ("getaddednodeinfo the name before the NUL", hub3_addr.as_str()),
        ("getaddednodeinfo the closed port", closed.as_str()),
    ] {
        diff.same(step, "getaddednodeinfo", &format!(r#"["{node}"]"#));
    }
    diff.same("remove the NUL entry", "addnode", &format!(r#"["{nul}","remove"]"#));
    diff.same(
        "disconnectnode the name before the NUL",
        "disconnectnode",
        &format!(r#"["{hub3_addr}"]"#),
    );
    diff.same_per_peer("disconnectnode the inbound peer", "disconnectnode", |inbound| {
        format!(r#"["{inbound}"]"#)
    });
    diff.expect_peers("after both disconnects", &[]);
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    for hub in [hub, hub2, hub3] {
        hub.stop_ok(libc::SIGTERM);
    }
    assert!(diff.failures.is_empty(), "{}", diff.failures.join("\n"));
}

/// `getchaintips` on the node, in its order: each tip's height, hash, branch length and status.
fn chain_tips(port: u16) -> Vec<(i64, String, i64, String)> {
    let reply = rpc(port, "u:p", "getchaintips", "[]");
    let Some(UniValue::Arr(tips)) = UniValue::read(reply.result().as_bytes()) else {
        panic!("{}", reply.body)
    };
    let tip = |tip: &UniValue| {
        let UniValue::Obj(tip) = tip else {
            panic!("{}", reply.body)
        };
        let int = |key| peer_field(tip, key).get_int64().unwrap();
        let text = |key| peer_field(tip, key).get_str().unwrap().to_owned();
        (int("height"), text("hash"), int("branchlen"), text("status"))
    };
    tips.iter().map(tip).collect()
}

/// BL-503's `getblockfrompeer` with real P2P: a dashd and an rdashd, each with its own datadir,
/// `-v2transport=0` and listening, take chain A (A1-A4) by `submitblock`, and only the headers of
/// F3 on A2 and G2 on A1 by `submitheader`: every tip has its own height, so `getchaintips` has
/// one order. A dashd hub holds A1, A2 and F3, which is its tip. The same calls must answer the
/// same status and body, each node with its own peer's id:
/// - with no peer, "Peer does not exist";
/// - a raw inbound connection that sends nothing, its id 0 on both: "Peer not fully connected",
///   with G2 left in its `inflight`; then it closes;
/// - the hub by `addnode onetry`, id 1 on both once it answered a ping: the negative of its id
///   is no peer; F3 from it is `{}`, then stored on both as a fork block, not connected; then
///   "Block already downloaded" (its id, and -1); G2 from the next id is no peer, and from the
///   hub `{}`, a request the hub ignores, having no G2.
// Core: rpc/blockchain.cpp:483-534; net_processing.cpp:2057-2089, :2777-2779
#[test]
fn getblockfrompeer_matches_dashd_v24_with_a_serving_hub() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let listening = ["-bind=127.0.0.1", "-listenonion=0", "-v2transport=0"];
    let (hub, hub_rpc, _, hub_p2p) = start_node(home.path(), &dashd, "hub", &listening);
    let (core, core_rpc, _, core_p2p) = start_node(home.path(), &dashd, "core", &listening);
    let (ours, our_rpc, _, our_p2p) =
        start_node(home.path(), Path::new(RDASHD), "ours", &listening);
    let suts = [core_rpc, our_rpc];
    let all = [hub_rpc, core_rpc, our_rpc];

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let base = u32::try_from(now.as_secs()).unwrap() - 3600;
    let at = |prev: &Mined, height: u8, offset: u32, value: i64| {
        mine(&prev.hash, base + offset, &[coinbase(height, value)], None, true)
    };
    let genesis = genesis(core_rpc);
    let a1 = at(&genesis, 1, 60, 5_000);
    let a2 = at(&a1, 2, 120, 5_000);
    let a3 = at(&a2, 3, 180, 5_000);
    let a4 = at(&a3, 4, 240, 5_000);
    let f3 = at(&a2, 3, 181, 6_000);
    let g2 = at(&a1, 2, 121, 7_000);
    let null = |port: u16, method: &str, params: String| {
        assert_eq!(rpc(port, "u:p", method, &params).result(), "null", "{method} on {port}");
    };
    for port in suts {
        for m in [&a1, &a2, &a3, &a4] {
            null(port, "submitblock", format!(r#"["{}"]"#, m.hex));
        }
        for m in [&f3, &g2] {
            null(port, "submitheader", format!(r#"["{}"]"#, &m.hex[..160]));
        }
    }
    for m in [&a1, &a2, &f3] {
        null(hub_rpc, "submitblock", format!(r#"["{}"]"#, m.hex));
    }
    for port in all {
        let info = rpc(port, "u:p", "getblockchaininfo", "[]");
        assert!(info.result().contains(r#""initialblockdownload":false"#), "{}", info.body);
    }
    let getblock = |port: u16, m: &Mined| rpc(port, "u:p", "getblock", &hash_params(&m.hash, ",0"));
    for port in suts {
        for m in [&f3, &g2] {
            let reply = getblock(port, m);
            assert_eq!(reply.error().1, r#"{"code":-1,"message":"Block not found on disk"}"#);
        }
        let mut heights: Vec<_> = chain_tips(port).into_iter().map(|t| t.0).collect();
        heights.sort_unstable();
        assert_eq!(heights, [2, 3, 4], "the SUT's tips on {port}");
    }
    assert_eq!(getblock(hub_rpc, &f3).result(), format!(r#""{}""#, f3.hex));
    assert_eq!(getblock(hub_rpc, &g2).error().1, r#"{"code":-5,"message":"Block not found"}"#);
    let tip = |height, m: &Mined, branchlen, status: &str| {
        (height, shown(&m.hash), branchlen, status.to_owned())
    };
    assert_eq!(chain_tips(hub_rpc), [tip(3, &f3, 0, "active")], "the hub's one tip");

    let mut diff = Diff {
        ports: suts,
        failures: Vec::new(),
    };
    diff.same("0 the SUTs' tips", "getchaintips", "[]");
    let fetch = |m: &Mined, id: i64| hash_params(&m.hash, &format!(",{id}"));
    let says = |reply: (u16, String), what: &str| {
        assert!(reply.1.contains(what), "dashd: {reply:?}, expected {what}");
    };
    let fetched = |reply: (u16, String)| {
        assert_eq!(reply, (200, "{\"result\":{},\"error\":null,\"id\":1}\n".to_owned()));
    };
    says(diff.same("1 no peer", "getblockfrompeer", &fetch(&f3, 0)), "Peer does not exist");

    let raw = [core_p2p, our_p2p].map(|p| TcpStream::connect(("127.0.0.1", p)).unwrap());
    let opened = Instant::now();
    wait_until("the raw peer listed on both", || suts.iter().all(|&p| peer_objects(p).len() == 1));
    assert_eq!(suts.map(|p| first_peer(p).0), [0, 0], "the raw peer's id on dashd and rdashd");
    let reply = diff.same_per_id("2 not fully connected", "getblockfrompeer", |id| fetch(&g2, id));
    says(reply, "Peer not fully connected");
    let inflight = suts.map(|p| {
        let peers = peer_objects(p);
        let [peer] = &peers[..] else {
            panic!("the raw peer left {p} early: {peers:?}")
        };
        (peer_field(peer, "id").get_int64().unwrap(), peer_field(peer, "inflight").write())
    });
    assert!(
        opened.elapsed() < Duration::from_secs(30),
        "steps 2-2b took {:?}: {inflight:?}",
        opened.elapsed()
    );
    assert_eq!(inflight, [(0, "[2]".to_owned()), (0, "[2]".to_owned())], "dashd, rdashd");
    drop(raw);
    wait_until("the raw peer gone on both", || suts.iter().all(|&p| peer_objects(p).is_empty()));

    let onetry = format!(r#"["127.0.0.1:{hub_p2p}","onetry"]"#);
    assert_eq!(diff.same("3 addnode", "addnode", &onetry).0, 200);
    wait_until("the hub answered a ping on both", || {
        suts.iter().all(|&p| pinged_peer(p).is_some())
    });
    assert_eq!(suts.map(|p| first_peer(p).0), [1, 1], "the hub's id on dashd and rdashd");
    let reply = diff.same_per_id("3b the negative id", "getblockfrompeer", |id| fetch(&f3, -id));
    says(reply, "Peer does not exist");
    fetched(diff.same_per_id("4 F3 from the hub", "getblockfrompeer", |id| fetch(&f3, id)));
    wait_until("F3 stored on both", || suts.iter().all(|&p| getblock(p, &f3).status == 200));
    let (_, block) = diff.same("4 getblock F3", "getblock", &hash_params(&f3.hash, ",0"));
    assert!(block.contains(&f3.hex), "{block}");
    diff.same("4 getchaintips", "getchaintips", "[]");
    let tips = chain_tips(core_rpc);
    assert!(tips.contains(&tip(3, &f3, 1, "valid-headers")), "F3 stored, not connected: {tips:?}");
    let reply = diff.same_per_id("5 F3 again", "getblockfrompeer", |id| fetch(&f3, id));
    says(reply, "Block already downloaded");
    says(diff.same("5 F3, -1", "getblockfrompeer", &fetch(&f3, -1)), "Block already downloaded");
    let reply = diff.same_per_id("6 the next id", "getblockfrompeer", |id| fetch(&g2, id + 1));
    says(reply, "Peer does not exist");
    fetched(diff.same_per_id("7 G2 from the hub", "getblockfrompeer", |id| fetch(&g2, id)));
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    hub.stop_ok(libc::SIGTERM);
    assert!(diff.failures.is_empty(), "{}", diff.failures.join("\n"));
}

/// DIFF-RPC's `mnsync` (SP1, PLAN's `mnsync status`; Sol r1 Major 2): Core's six status keys, the
/// `next` and `reset` strings, `"failure"` for another mode, and the arity, type and help
/// errors, on dashd and rdashd started with the same `-mocktime`, so that the asset start times
/// agree. The clock stands still, so the tick's six-second gate stops every tick before it acts
/// (`node/sync_manager.cpp:157-160`). dashd runs with rdashd's `-disablegovernance=1` (DEC-510).
// Core: rpc/node.cpp:95-149
#[test]
fn mnsync_matches_dashd_v24() {
    let dashd = require_dashd();
    let (home, _) = home_and_datadir();
    let mocktime = "-mocktime=1700000000";
    let (core, core_port, ..) =
        start_node(home.path(), &dashd, "core", &["-listen=0", mocktime, "-disablegovernance=1"]);
    let (ours, our_port, ..) =
        start_node(home.path(), Path::new(RDASHD), "ours", &["-listen=0", mocktime]);
    let mut diff = Diff {
        ports: [core_port, our_port],
        failures: Vec::new(),
    };
    let status = |diff: &mut Diff, step: &str| diff.same(step, "mnsync", r#"["status"]"#).1;
    let start = status(&mut diff, "at start");
    assert!(start.contains(r#""AssetID":1,"#) && start.contains("1700000000"), "{start}");
    for (step, params) in [
        ("no mode", "[]"),
        ("two arguments", r#"["status","x"]"#),
        ("a number", "[1]"),
        ("null", "[null]"),
        ("an unknown mode", r#"["STATUS"]"#),
        ("named", r#"{"mode":"status"}"#),
        ("named unknown", r#"{"m":"status"}"#),
    ] {
        diff.same(step, "mnsync", params);
    }
    diff.same("help", "help", r#"["mnsync"]"#);
    let (_, next) = diff.same("next", "mnsync", r#"["next"]"#);
    assert!(next.contains("sync updated to MASTERNODE_SYNC_GOVERNANCE"), "{next}");
    status(&mut diff, "at GOVERNANCE");
    diff.same("next again", "mnsync", r#"["next"]"#);
    let finished = status(&mut diff, "at FINISHED");
    assert!(finished.contains(r#""IsSynced":true"#), "{finished}");
    diff.same("next when finished", "mnsync", r#"["next"]"#);
    let (_, reset) = diff.same("reset", "mnsync", r#"["reset"]"#);
    assert!(reset.contains(r#""result":"success""#), "{reset}");
    status(&mut diff, "after reset");
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(diff.failures.is_empty(), "{}", diff.failures.join("\n"));
}

/// A JSON-RPC call of `method` with `params` (JSON text).
fn call(method: &str, params: &str) -> Vec<u8> {
    post(&format!(r#"{{"method":"{method}","params":{params},"id":1}}"#))
}

/// `hash` as RPC shows it: its bytes reversed, in hex.
fn shown(hash: &[u8; 32]) -> String {
    let mut shown = *hash;
    shown.reverse();
    hex(&shown)
}

/// `[<hash>]`, as RPC shows it, then `rest`.
fn hash_params(hash: &[u8; 32], rest: &str) -> String {
    format!(r#"["{}"{rest}]"#, shown(hash))
}

/// RP1a-1a's chain commands (DEC-593, getblock at verbosity 0 and 1: DEC-606) on blocks 1 to 5 (`a`) on genesis, with forks: `d2` and
/// `b2` on `a1`, the second with only the headers of `b3` and `b4` after it (`submitheader`),
/// then `c5` on `a4`, equal in work to `a5`, and `c6` on it. In order: `getbestblockhash`,
/// `getblockheader` and `getchaintips` on the fresh chain with the argument checks; the chain
/// and its forks; `invalidateblock a4` (back to `a3`) and `reconsiderblock`; `preciousblock`
/// across `a5` and `c5`; `gettxoutsetinfo` in its three hash types at each tip; `setmocktime`
/// letting a header from `far_time` in (`time-too-new` before it); `mockscheduler` and
/// `syncwithvalidationinterfacequeue`. No `getchaintips` runs while two tips share a height:
/// Core orders those by `CBlockIndex` address (`CompareBlocksByHeight`, `rpc/blockchain.cpp`).
fn chain_command_cases(genesis: &Mined, far_time: u32) -> Vec<(&'static str, Vec<u8>)> {
    let paid = |prev: &Mined, height: u8, value: i64| next(&prev.hash, height, value);
    let mut a = vec![paid(genesis, 1, 5_000)];
    for height in 2..=5 {
        a.push(paid(&a[a.len() - 1], height, 5_000));
    }
    let d2 = paid(&a[0], 2, 7_000);
    let b2 = paid(&a[0], 2, 6_000);
    let b3 = paid(&b2, 3, 6_000);
    let b4 = paid(&b3, 4, 6_000);
    let c5 = paid(&a[3], 5, 8_000);
    let c6 = paid(&c5, 6, 8_000);
    let future = mine(&c6.hash, far_time, &[coinbase(7, 0)], None, true);
    let header = |m: &Mined| format!(r#"["{}"]"#, &m.hex[..160]);
    let submit = |m: &Mined| call("submitblock", &format!(r#"["{}"]"#, m.hex));
    let block = |m: &Mined, rest: &str| hash_params(&m.hash, rest);
    // Names built at run time; the test's lifetime is the process's.
    let named = |name: String| -> &'static str { Box::leak(name.into_boxed_str()) };
    let utxos = |tip: &str| {
        [("", "[]"), ("_muhash", r#"["muhash"]"#), ("_none", r#"["none"]"#)].map(
            |(kind, params)| {
                (named(format!("gettxoutsetinfo_{tip}{kind}")), call("gettxoutsetinfo", params))
            },
        )
    };
    let unknown = format!(r#"["{}"]"#, "11".repeat(32));
    let mut cases = Vec::new();
    for command in [
        "getbestblockhash",
        "getblock",
        "getblockheader",
        "getchaintips",
        "invalidateblock",
        "reconsiderblock",
        "preciousblock",
        "submitheader",
        "gettxoutsetinfo",
        "setmocktime",
        "mockscheduler",
        "syncwithvalidationinterfacequeue",
        "waitfornewblock",
        "waitforblock",
        "waitforblockheight",
    ] {
        cases.push((named(format!("help_{command}")), call("help", &format!(r#"["{command}"]"#))));
    }
    cases.extend([
        ("getbestblockhash_genesis", call("getbestblockhash", "[]")),
        ("getblockheader_genesis", call("getblockheader", &block(genesis, ""))),
        ("getblockheader_genesis_hex", call("getblockheader", &block(genesis, ",false"))),
        ("getblockheader_unknown", call("getblockheader", &unknown)),
        ("getblockheader_short", call("getblockheader", r#"["00"]"#)),
        ("getblockheader_not_hex", call("getblockheader", &format!(r#"["{}"]"#, "zz".repeat(32)))),
        ("getblockheader_verbose_number", call("getblockheader", &block(genesis, ",1"))),
        ("getchaintips_genesis", call("getchaintips", "[]")),
        ("gettxoutsetinfo_genesis", call("gettxoutsetinfo", "[]")),
        ("gettxoutsetinfo_bad_type", call("gettxoutsetinfo", r#"["foo"]"#)),
        ("gettxoutsetinfo_height", call("gettxoutsetinfo", r#"["muhash",1]"#)),
        ("gettxoutsetinfo_index_type", call("gettxoutsetinfo", r#"["none",null,"x"]"#)),
        // RP1a-2a: init's `RPCNotifyBlockChange(ActiveTip())` set genesis; the waits that it
        // satisfies return at once, the others at their timeout (1 ms) or a negative one.
        ("waitforblockheight_genesis", call("waitforblockheight", "[0]")),
        ("waitforblockheight_timeout", call("waitforblockheight", "[5, 1]")),
        ("waitforblock_genesis", call("waitforblock", &block(genesis, ""))),
        ("waitforblock_unknown_timeout", call("waitforblock", &block(&a[0], ", 1"))),
        ("waitfornewblock_timeout", call("waitfornewblock", "[1]")),
        ("waitfornewblock_negative", call("waitfornewblock", "[-5]")),
        ("waitforblock_short", call("waitforblock", r#"["00"]"#)),
        ("waitforblock_not_hex", call("waitforblock", &format!(r#"["{}"]"#, "zz".repeat(32)))),
        ("waitforblock_number", call("waitforblock", "[1]")),
        ("waitforblockheight_string", call("waitforblockheight", r#"["1"]"#)),
        ("waitforblockheight_fraction", call("waitforblockheight", "[1.5]")),
        ("waitforblockheight_none", call("waitforblockheight", "[]")),
        ("waitfornewblock_string", call("waitfornewblock", r#"["1"]"#)),
        ("waitfornewblock_extra", call("waitfornewblock", "[1, 2]")),
        ("waitforblockheight_timeout_string", call("waitforblockheight", r#"[0, "1"]"#)),
    ]);
    for (m, name) in a.iter().zip(["a1", "a2", "a3", "a4", "a5"]) {
        cases.push((name, submit(m)));
    }
    cases.extend([
        ("submitblock_d2", submit(&d2)),
        ("submitblock_b2", submit(&b2)),
        ("submitheader_b3", call("submitheader", &header(&b3))),
        ("submitheader_b3_again", call("submitheader", &header(&b3))),
        ("submitheader_b4", call("submitheader", &header(&b4))),
    ]);
    cases.extend(getblockfrompeer_cases(genesis, &a[4], &b2, &b3, &b4));
    cases.extend([
        ("submitheader_orphan", call("submitheader", &header(&c6))),
        ("submitheader_bad_hex", call("submitheader", r#"["00"]"#)),
        ("submitheader_number", call("submitheader", "[1]")),
        ("getbestblockhash_a5", call("getbestblockhash", "[]")),
        ("getblockheader_a3", call("getblockheader", &block(&a[2], ""))),
        ("getblockheader_a5", call("getblockheader", &block(&a[4], ""))),
        ("getblockheader_a5_hex", call("getblockheader", &block(&a[4], ",false"))),
        ("getblockheader_b2", call("getblockheader", &block(&b2, ""))),
        ("getblockheader_b4", call("getblockheader", &block(&b4, ""))),
        ("getblock_genesis", call("getblock", &block(genesis, ""))),
        ("getblock_a3", call("getblock", &block(&a[2], ""))),
        ("getblock_a3_hex", call("getblock", &block(&a[2], ",0"))),
        ("getblock_a3_negative", call("getblock", &block(&a[2], ",-1"))),
        ("getblock_a3_true", call("getblock", &block(&a[2], ",true"))),
        ("getblock_a3_false", call("getblock", &block(&a[2], ",false"))),
        ("getblock_a3_string", call("getblock", &block(&a[2], r#","1""#))),
        ("getblock_d2_side", call("getblock", &block(&d2, ""))),
        ("getblock_b4_headers_only", call("getblock", &block(&b4, ""))),
        ("getblock_unknown", call("getblock", &unknown)),
        ("getblock_short", call("getblock", r#"["00"]"#)),
        ("getchaintips_a5", call("getchaintips", "[]")),
        ("waitforblockheight_a5", call("waitforblockheight", "[5, 1]")),
        ("waitforblock_a3_timeout", call("waitforblock", &block(&a[2], ", 1"))),
        ("waitforblock_a5", call("waitforblock", &block(&a[4], ""))),
    ]);
    cases.extend(utxos("a5"));
    cases.extend([
        ("invalidateblock_a4", call("invalidateblock", &block(&a[3], ""))),
        ("getbestblockhash_a3", call("getbestblockhash", "[]")),
        ("getblockheader_a5_invalid", call("getblockheader", &block(&a[4], ""))),
        ("getblock_a5_invalid", call("getblock", &block(&a[4], ""))),
        ("getchaintips_a3", call("getchaintips", "[]")),
        // `InvalidateBlock`'s `NotifyBlockTip` moved `latestblock` back to a3.
        ("waitforblockheight_a3", call("waitforblockheight", "[4, 1]")),
        ("waitfornewblock_a3", call("waitfornewblock", "[-1]")),
    ]);
    cases.extend(utxos("a3"));
    cases.extend([
        ("invalidateblock_unknown", call("invalidateblock", &unknown)),
        ("reconsiderblock_unknown", call("reconsiderblock", &unknown)),
        ("preciousblock_unknown", call("preciousblock", &unknown)),
        ("invalidateblock_short", call("invalidateblock", r#"["00"]"#)),
        ("reconsiderblock_a4", call("reconsiderblock", &block(&a[3], ""))),
        ("getbestblockhash_a5_again", call("getbestblockhash", "[]")),
        ("getchaintips_a5_again", call("getchaintips", "[]")),
        ("submitblock_c5", submit(&c5)),
        ("getbestblockhash_tied", call("getbestblockhash", "[]")),
        ("preciousblock_c5", call("preciousblock", &block(&c5, ""))),
        ("getbestblockhash_c5", call("getbestblockhash", "[]")),
        ("waitfornewblock_c5", call("waitfornewblock", "[-1]")),
        ("preciousblock_a5", call("preciousblock", &block(&a[4], ""))),
        ("getbestblockhash_a5_precious", call("getbestblockhash", "[]")),
        ("preciousblock_c5_again", call("preciousblock", &block(&c5, ""))),
        ("preciousblock_b2", call("preciousblock", &block(&b2, ""))),
        ("getbestblockhash_c5_again", call("getbestblockhash", "[]")),
        ("submitblock_c6", submit(&c6)),
        ("getchaintips_c6", call("getchaintips", "[]")),
        ("getblockheader_a5_fork", call("getblockheader", &block(&a[4], ""))),
        ("waitforblockheight_c6", call("waitforblockheight", "[6, 1]")),
    ]);
    cases.extend(utxos("c6"));
    cases.extend([
        ("submitheader_too_new", call("submitheader", &header(&future))),
        ("setmocktime_negative", call("setmocktime", "[-1]")),
        ("setmocktime_too_large", call("setmocktime", "[9223372036854775807]")),
        ("setmocktime_string", call("setmocktime", r#"["x"]"#)),
        ("setmocktime_far", call("setmocktime", &format!("[{far_time}]"))),
        ("submitheader_mocked", call("submitheader", &header(&future))),
        ("setmocktime_off", call("setmocktime", "[0]")),
        ("getchaintips_future", call("getchaintips", "[]")),
        ("getblockheader_future", call("getblockheader", &block(&future, ""))),
        ("mockscheduler_zero", call("mockscheduler", "[0]")),
        ("mockscheduler_too_far", call("mockscheduler", "[3601]")),
        ("mockscheduler_string", call("mockscheduler", r#"["x"]"#)),
        ("mockscheduler_hour", call("mockscheduler", "[3600]")),
        ("syncwithvalidationinterfacequeue", call("syncwithvalidationinterfacequeue", "[]")),
    ]);
    cases
}

/// BL-503's `getblockfrompeer` on a node with no peer, none of which changes its state:
/// `genesis`, `a5` and `b2` have data, `b3` and `b4` are headers only. The server's arity and
/// type checks, `ParseHashV`'s, `getInt<int64_t>`'s (before the lookup: an unknown hash with a
/// fraction is out of range), the unknown header, the data check before `FetchBlock` (a bad peer
/// id too), then `FetchBlock`'s unknown peer for every id spelling, and the named forms.
// Core: rpc/blockchain.cpp:483-534; net_processing.cpp:2057-2063
fn getblockfrompeer_cases(
    genesis: &Mined,
    a5: &Mined,
    b2: &Mined,
    b3: &Mined,
    b4: &Mined,
) -> Vec<(&'static str, Vec<u8>)> {
    let fetch = |params: &str| call("getblockfrompeer", params);
    let block = |m: &Mined, rest: &str| fetch(&hash_params(&m.hash, rest));
    let zero = |rest: &str| fetch(&format!(r#"["{}"{rest}]"#, "00".repeat(32)));
    let b3_hash = shown(&b3.hash);
    vec![
        ("getblockfrompeer_none", fetch("[]")),
        ("getblockfrompeer_one", block(b3, "")),
        ("getblockfrompeer_three", block(b3, ",0,0")),
        ("getblockfrompeer_number_hash", fetch("[1234,0]")),
        ("getblockfrompeer_string_peer", block(b3, r#","0""#)),
        ("getblockfrompeer_both_types", fetch(r#"[1234,"0"]"#)),
        ("getblockfrompeer_null_hash", fetch("[null,0]")),
        ("getblockfrompeer_short", fetch(r#"["1234",0]"#)),
        ("getblockfrompeer_not_hex", fetch(&format!(r#"["{}",0]"#, "g".repeat(64)))),
        ("getblockfrompeer_short_fraction", fetch(r#"["1234",1.5]"#)),
        ("getblockfrompeer_fraction", block(b3, ",1.5")),
        ("getblockfrompeer_exponent", block(b3, ",1e3")),
        ("getblockfrompeer_point_zero", block(b3, ",1.0")),
        ("getblockfrompeer_too_large", block(b3, ",9223372036854775808")),
        ("getblockfrompeer_unknown_fraction", zero(",1.5")),
        ("getblockfrompeer_unknown", zero(",0")),
        ("getblockfrompeer_unknown_negative", zero(",-1")),
        ("getblockfrompeer_genesis", block(genesis, ",0")),
        ("getblockfrompeer_genesis_negative", block(genesis, ",-1")),
        ("getblockfrompeer_b2_side", block(b2, ",0")),
        ("getblockfrompeer_a5", block(a5, ",0")),
        ("getblockfrompeer_b3", block(b3, ",0")),
        ("getblockfrompeer_b3_negative", block(b3, ",-1")),
        ("getblockfrompeer_b3_negative_zero", block(b3, ",-0")),
        ("getblockfrompeer_b3_min", block(b3, ",-9223372036854775808")),
        ("getblockfrompeer_b4", block(b4, ",0")),
        ("getblockfrompeer_named", fetch(&format!(r#"{{"blockhash":"{b3_hash}","peer_id":0}}"#))),
        ("getblockfrompeer_named_no_peer", fetch(&format!(r#"{{"blockhash":"{b3_hash}"}}"#))),
        ("getblockfrompeer_named_no_hash", fetch(r#"{"peer_id":0}"#)),
    ]
}

/// `reply` with the value of `gettxoutsetinfo`'s `disk_size`, and so the `Content-Length`, masked:
/// the two nodes' coins databases differ (LevelDB, RocksDB; ST1).
fn mask_disk_size(reply: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(reply);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return reply.to_vec();
    };
    let Some((before, after)) = body.split_once(r#""disk_size":"#) else {
        return reply.to_vec();
    };
    let after = after.trim_start_matches(|c: char| c.is_ascii_digit());
    let head: Vec<_> = head
        .split("\r\n")
        .map(|l| {
            if l.to_ascii_lowercase().starts_with("content-length:") {
                "Content-Length: N"
            } else {
                l
            }
        })
        .collect();
    format!("{}\r\n\r\n{before}\"disk_size\":N{after}", head.join("\r\n")).into_bytes()
}

#[test]
fn mask_disk_size_masks_only_the_size_and_length() {
    let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 30\r\n\r\n{\"disk_size\":1234,\"txouts\":5}";
    assert_eq!(
        mask_disk_size(reply),
        b"HTTP/1.1 200 OK\r\nContent-Length: N\r\n\r\n{\"disk_size\":N,\"txouts\":5}"
    );
    assert_eq!(mask_disk_size(b"HTTP/1.1 200 OK\r\n\r\n{}"), b"HTTP/1.1 200 OK\r\n\r\n{}");
}

/// RP1a-1a (DEC-593): [`chain_command_cases`] sent to dashd and rdashd in order, each reply equal
/// byte for byte with the `Date` header masked, and `gettxoutsetinfo`'s `disk_size` too. Then the
/// cases are checked not to be vacuous: the reorgs happened on dashd, its `gettxoutsetinfo`
/// moved with the tip, and so did the waits' `latestblock` (RP1a-2a).
#[test]
fn chain_commands_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    // A month ahead: past `MAX_FUTURE_BLOCK_TIME` (two hours) on any clock this test runs on.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let far_time = u32::try_from(now.as_secs() + 30 * 24 * 60 * 60).unwrap();
    let cases = chain_command_cases(&genesis(core_port), far_time);
    let mut failures = Vec::new();
    let mut replies = std::collections::HashMap::new();
    for (name, request) in &cases {
        let [theirs, mine] =
            [core_port, our_port].map(|port| mask_disk_size(&exchange(port, request)));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            failures.push(format!("{name}:\n  dashd  {}\n  rdashd {}", show(&theirs), show(&mine)));
        }
        replies.insert(*name, String::from_utf8(theirs).unwrap());
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    // A restart loads c6 and connects no block, so only init's `RPCNotifyBlockChange` sets
    // `latestblock`: without it the wait times out at Core's zero static.
    // Core: init.cpp:2761-2769
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let restarted = call("waitforblockheight", "[1, 1]");
    let [theirs, mine] = [core_port, our_port].map(|port| exchange(port, &restarted));
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    let show = |b: &[u8]| b.escape_ascii().to_string();
    if theirs != mine {
        failures.push(format!(
            "waitforblockheight_restarted:\n  dashd  {}\n  rdashd {}",
            show(&theirs),
            show(&mine)
        ));
    }
    assert!(
        failures.is_empty(),
        "{} of {} differ:\n{}",
        failures.len(),
        cases.len() + 1,
        failures.join("\n")
    );
    let restarted = String::from_utf8_lossy(&theirs);
    assert!(restarted.contains(r#""height":6}"#), "restarted at c6: {restarted}");
    let ok = |name: &str| replies[name].contains(r#""error":null"#);
    for name in [
        "a5",
        "submitheader_b4",
        "invalidateblock_a4",
        "setmocktime_far",
        "submitheader_mocked",
        "getblock_a3",
        "getblock_a3_hex",
    ] {
        assert!(ok(name), "{name}: {}", replies[name]);
    }
    // The waits followed dashd's tip: back to a3 after the invalidation, then c5 and c6.
    for (name, height) in [
        ("waitforblockheight_genesis", 0),
        ("waitforblockheight_a5", 5),
        ("waitforblockheight_a3", 3),
        ("waitfornewblock_c5", 5),
        ("waitforblockheight_c6", 6),
    ] {
        let reply = &replies[name];
        assert!(ok(name) && reply.contains(&format!(r#""height":{height}}}"#)), "{name}: {reply}");
    }
    let body = |name: &str| replies[name].split_once("\r\n\r\n").map(|(_, b)| b.to_owned());
    assert_ne!(
        body("waitfornewblock_c5"),
        body("waitforblockheight_a5"),
        "c5 is not a5, though both are at height 5"
    );
    for name in [
        "submitheader_too_new",
        "getblockheader_unknown",
        "setmocktime_negative",
        "getblock_b4_headers_only",
    ] {
        assert!(!ok(name), "{name}: {}", replies[name]);
    }
    for (names, message) in [
        (
            &["getblockfrompeer_none", "getblockfrompeer_named_no_peer"][..],
            r#"getblockfrompeer \"blockhash\""#,
        ),
        (&["getblockfrompeer_both_types"], "Position 2 (peer_id)"),
        (&["getblockfrompeer_short_fraction"], "blockhash must be of length 64"),
        (&["getblockfrompeer_unknown_fraction"], "JSON integer out of range"),
        (&["getblockfrompeer_unknown"], "Block header missing"),
        (
            &["getblockfrompeer_genesis_negative", "getblockfrompeer_b2_side"],
            "Block already downloaded",
        ),
        (
            &["getblockfrompeer_b3_min", "getblockfrompeer_b4", "getblockfrompeer_named"],
            "Peer does not exist",
        ),
    ] {
        for name in names {
            assert!(replies[name].contains(message), "{name}: {}", replies[name]);
        }
    }
    let distinct = |names: &[&str]| {
        let bodies: std::collections::HashSet<_> =
            names.iter().map(|n| replies[n].split_once("\r\n\r\n").unwrap().1).collect();
        assert_eq!(bodies.len(), names.len(), "{names:?} repeat a reply");
    };
    distinct(&["getbestblockhash_a5", "getbestblockhash_a3", "getbestblockhash_c5"]);
    assert_eq!(replies["getbestblockhash_tied"], replies["getbestblockhash_a5"]);
    assert_eq!(replies["getbestblockhash_a5_precious"], replies["getbestblockhash_a5"]);
    distinct(&["gettxoutsetinfo_a5", "gettxoutsetinfo_a3", "gettxoutsetinfo_c6"]);
}

/// `rpc_generate.py`'s regtest address of the generator point's key, and that key.
const GENERATOR_ADDRESS: &str = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";
const GENERATOR_KEY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
/// The same key uncompressed.
const GENERATOR_KEY_FULL: &str = "0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";
/// `rpc_generate.py`'s tpub.
const TPUB: &str = "tpubD6NzVbkrYhZ4XgiXtGrdW5XDAPFCL9h7we1vwNCpn8tGbBcgfVYjXyhWo4E1xkh56hjod1RhGjxbaTLV3X4FyWuejifB9jusQ46QzG87VKp";

/// A version-1 transaction spending output 0 of `txid` (as RPC shows it) with an empty
/// scriptSig, paying `value` duffs to `OP_TRUE`.
fn op_true_spend(txid: &str, value: u64) -> String {
    let mut prev = [0u8; 32];
    for (i, b) in prev.iter_mut().rev().enumerate() {
        *b = u8::from_str_radix(&txid[2 * i..2 * i + 2], 16).unwrap();
    }
    format!(
        "0100000001{}0000000000ffffffff01{}0151{}",
        hex(&prev),
        hex(&value.to_le_bytes()),
        "00".repeat(4)
    )
}

/// RP1a-1b: `generatetoaddress`, `generatetodescriptor`, `generateblock` and `generate` sent to
/// dashd and rdashd in order on fresh regtest chains under one mock time, so both mine the same
/// blocks: each reply equal byte for byte with the `Date` header masked. The outputs cover an
/// address and every descriptor function (keys hex, BIP32, with origins and checksums), the
/// argument and descriptor errors, `maxtries` 0 and -1, and `generateblock` with a txid, bad
/// hex, a mature `OP_TRUE` coinbase's spend, the same spend again (`TestBlockValidity`'s
/// refusal: BIP30, the transaction's own output being unspent), and `submit` false then
/// `submitblock`.
// Core: rpc/mining.cpp:139-449; test/functional/rpc_generate.py
#[test]
fn generate_commands_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    // Each case to both nodes; dashd's body, which the next cases may build on.
    let mut both = |name: &str, method: &str, params: &str| -> String {
        count += 1;
        let request = call(method, params);
        let [theirs, mine] = [core_port, our_port].map(|port| exchange(port, &request));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            failures.push(format!("{name}:\n  dashd  {}\n  rdashd {}", show(&theirs), show(&mine)));
        }
        let text = String::from_utf8(theirs).unwrap();
        text.split_once("\r\n\r\n").unwrap().1.to_owned()
    };
    let result = |body: &str| -> UniValue {
        let reply = UniValue::read(body.as_bytes()).unwrap();
        let UniValue::Obj(fields) = reply else {
            panic!("{body}")
        };
        fields.into_iter().find(|(k, _)| k == "result").unwrap().1
    };
    let ok = |body: &str| body.contains(r#""error":null"#);

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    both("setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
    for command in ["generatetoaddress", "generatetodescriptor", "generateblock", "generate"] {
        both(&format!("help_{command}"), "help", &format!(r#"["{command}"]"#));
    }
    both("generate", "generate", "[]");
    both("generate_args", "generate", "[1]");

    let mined = both("toaddress_3", "generatetoaddress", &format!(r#"[3, "{GENERATOR_ADDRESS}"]"#));
    assert_eq!(result(&mined).get_values().unwrap().len(), 3, "{mined}");
    let g = GENERATOR_KEY;
    let full = GENERATOR_KEY_FULL;
    let checksummed = format!("addr({GENERATOR_ADDRESS})");
    let descriptors = [
        ("combo", format!("combo({g})")),
        ("combo_full", format!("combo({full})")),
        ("raw", "raw(51)".to_owned()),
        ("pk", format!("pk({full})")),
        ("pkh_origin", format!("pkh([deadbeef/1/2'/3h]{g})")),
        ("pkh_tpub", format!("pkh({TPUB}/0/1)")),
        ("multi", format!("multi(1,{g},{full})")),
        ("sortedmulti", format!("sortedmulti(1,{full},{g})")),
        ("sh_multi", format!("sh(multi(2,{g},{full}))")),
        ("sh_pkh", format!("sh(pkh({g}))")),
        ("addr", checksummed.clone()),
    ];
    for (name, descriptor) in &descriptors {
        let body = both(
            &format!("todescriptor_{name}"),
            "generatetodescriptor",
            &format!(r#"[1, "{descriptor}"]"#),
        );
        assert!(ok(&body), "{name}: {body}");
    }
    let refused = [
        ("ranged", format!("pkh({TPUB}/0/*)")),
        ("hardened", format!("pkh({TPUB}/0'/0)")),
        ("bad_checksum", "raw(51)#qqqqqqqq".to_owned()),
        ("short_checksum", "raw(51)#qqq".to_owned()),
        ("two_checksums", "raw(51)#a#b".to_owned()),
        ("bad_chars", "raw(51)\u{e9}".to_owned()),
        ("leftover", "raw(51))".to_owned()),
        ("sh_leftover", format!("sh(pk({g}),1)")),
        ("not_a_function", "1234".to_owned()),
        ("hybrid", format!("pk(06{})", &full[2..])),
        ("bad_key", "pk(02aa)".to_owned()),
        ("no_key", "pk()".to_owned()),
        ("bare_multi_4", format!("multi(1,{g},{g},{g},{g})")),
        ("multi_threshold", format!("multi(3,{g},{g})")),
        ("sh_combo", format!("sh(combo({g}))")),
        ("sh_raw", "sh(raw(51))".to_owned()),
        ("sh_key", format!("sh({g})")),
        ("raw_not_hex", "raw(5)".to_owned()),
        ("addr_bad", "addr(1234)".to_owned()),
        ("origin_no_bracket", format!("pkh(deadbeef]{g})")),
        ("origin_short", format!("pkh([dead]{g})")),
        ("path_overflow", format!("pkh({TPUB}/2147483648)")),
        ("mainnet_xpub", "pkh(xpub661MyMwAqRbcFW31YEwpkMuc5THy2PSt5bDMsktWQcFF8syAmRUapSCGu8ED9W6oDMSgv6Zz8idoc4a6mr8BDzTJY47LJhkJ8UB7WEGuduB)".to_owned()),
    ];
    for (name, descriptor) in &refused {
        let body = both(
            &format!("todescriptor_{name}"),
            "generatetodescriptor",
            &format!(r#"[1, "{descriptor}"]"#),
        );
        assert!(!ok(&body), "{name}: {body}");
    }
    for (name, params) in [
        ("toaddress_bad", r#"[1, "1234"]"#.to_owned()),
        ("toaddress_mainnet", r#"[1, "XqmXR1mFTWJdTsmA62WmmMzkGdVz6vmvQs"]"#.to_owned()),
        ("toaddress_hrp", r#"[1, "tdash1qqqq"]"#.to_owned()),
        ("toaddress_spaces", format!(r#"[1, " {GENERATOR_ADDRESS}\n"]"#)),
        ("toaddress_zero_tries", format!(r#"[2, "{GENERATOR_ADDRESS}", 0]"#)),
        ("toaddress_minus_one_tries", format!(r#"[1, "{GENERATOR_ADDRESS}", -1]"#)),
        ("toaddress_zero_blocks", format!(r#"[0, "{GENERATOR_ADDRESS}"]"#)),
        ("toaddress_negative_blocks", format!(r#"[-1, "{GENERATOR_ADDRESS}"]"#)),
        ("toaddress_string_blocks", format!(r#"["1", "{GENERATOR_ADDRESS}"]"#)),
        ("toaddress_fraction_tries", format!(r#"[1, "{GENERATOR_ADDRESS}", 1.5]"#)),
    ] {
        both(name, "generatetoaddress", &params);
    }
    for (name, params) in [
        ("todescriptor_minus_one_tries", r#"[1, "raw(51)", -1]"#),
        ("todescriptor_big_tries", r#"[1, "raw(51)", 18446744073709551615]"#),
        ("todescriptor_too_big_tries", r#"[1, "raw(51)", 18446744073709551616]"#),
    ] {
        both(name, "generatetodescriptor", params);
    }

    // A hundred blocks to mature the first OP_TRUE coinbase, then spend it.
    let matured = both("todescriptor_101", "generatetodescriptor", r#"[101, "raw(51)"]"#);
    let first = result(&matured).get_values().unwrap()[0].get_str().unwrap().to_owned();
    let block = both("getblock_first_op_true", "getblock", &format!(r#"["{first}"]"#));
    let txids = result(&block);
    let UniValue::Obj(fields) = &txids else {
        panic!("{block}")
    };
    let coinbase = fields.iter().find(|(k, _)| k == "tx").unwrap().1.get_values().unwrap()[0]
        .get_str()
        .unwrap()
        .to_owned();
    let spend = op_true_spend(&coinbase, 100_000_000);
    let address = GENERATOR_ADDRESS;
    let mut blocks = std::collections::HashMap::new();
    for (name, params) in [
        ("block_bad_output", r#"["1234", []]"#.to_owned()),
        ("block_txid", format!(r#"["{address}", ["{}"]]"#, "00".repeat(32))),
        ("block_bad_hex", format!(r#"["{address}", ["0000"]]"#)),
        ("block_ranged", format!(r#"["pkh({TPUB}/0/*)", []]"#)),
        ("block_hardened", format!(r#"["pkh({TPUB}/0'/0)", []]"#)),
        ("block_descriptor", format!(r#"["{checksummed}", []]"#)),
        ("block_empty", format!(r#"["{address}", []]"#)),
        ("block_spend", format!(r#"["{address}", ["{spend}"]]"#)),
        ("block_spend_again", format!(r#"["{address}", ["{spend}"]]"#)),
    ] {
        blocks.insert(name, both(name, "generateblock", &params));
    }
    let made = both("block_unsubmitted", "generateblock", &format!(r#"["{address}", [], false]"#));
    let made = result(&made);
    let UniValue::Obj(fields) = &made else {
        panic!("an object")
    };
    let hex_of = fields.iter().find(|(k, _)| k == "hex").unwrap().1.get_str().unwrap().to_owned();
    both("getblockcount_before_submit", "getblockcount", "[]");
    let submitted = both("submitblock_unsubmitted", "submitblock", &format!(r#"["{hex_of}"]"#));
    assert!(submitted.contains(r#""result":null"#), "{submitted}");
    both("getbestblockhash_end", "getbestblockhash", "[]");
    both("getblockcount_end", "getblockcount", "[]");

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    assert!(ok(&blocks["block_spend"]), "{}", blocks["block_spend"]);
    assert!(
        blocks["block_spend_again"].contains("TestBlockValidity failed: bad-txns-BIP30"),
        "{}",
        blocks["block_spend_again"]
    );
    assert!(blocks["block_txid"].contains("not in mempool."), "{}", blocks["block_txid"]);
}

/// A version-`version` transaction spending each `(txid, vout, scriptSig)` (txids as RPC shows
/// them, scripts as hex) with sequence `0xffffffff`, paying each `(value, scriptPubKey)`.
fn spend_tx(version: u32, inputs: &[(&str, u32, &str)], outputs: &[(i64, &str)]) -> String {
    let mut tx = version.to_le_bytes().to_vec();
    tx.push(u8::try_from(inputs.len()).unwrap());
    for (txid, vout, script_sig) in inputs {
        let mut prev = parse_hex(txid);
        prev.reverse();
        tx.extend(prev);
        tx.extend(vout.to_le_bytes());
        let script = parse_hex(script_sig);
        tx.push(u8::try_from(script.len()).unwrap());
        tx.extend(script);
        tx.extend(u32::MAX.to_le_bytes());
    }
    tx.push(u8::try_from(outputs.len()).unwrap());
    for (value, script) in outputs {
        tx.extend(value.to_le_bytes());
        let script = parse_hex(script);
        tx.push(u8::try_from(script.len()).unwrap());
        tx.extend(script);
    }
    tx.extend(0u32.to_le_bytes());
    hex(&tx)
}

/// `tx`'s txid as RPC shows it.
fn rpc_txid(tx: &str) -> String {
    let mut id = txid(&parse_hex(tx));
    id.reverse();
    hex(&id)
}

/// RP1a-2c: getblock at verbosity 1 to 4, `true` and `false` (`blockToJSON` with `TxToUniv` and
/// the block's undo data) sent to dashd and rdashd on fresh regtest chains under one mock time,
/// so both mine the same blocks, each reply equal byte for byte with the `Date` header masked.
/// Below G1 (D42): coinbases to every descriptor shape; a block of spends with fees, one input
/// whose scriptSig pushes a strictly encoded signature with a sighash type (`[ALL]`), a
/// two-input version-2 spend with `OP_RETURN` and P2SH outputs, and a spend of an output created
/// earlier in the same block; a block spending the previous block's outputs; genesis, which has
/// no undo data; the tip after `invalidateblock` (its undo data still on disk) and after
/// `reconsiderblock`; and a P2PKH spend signed by dashd's `signrawtransactionwithkey`, so a
/// `prevout` carries an address (r1 n-4).
///
/// Then a block both nodes store but never connect (r1 m-1, D42): `submitblock` of a block on
/// the tip whose coinbase carries a valid version-2 CbTx and whose other transactions are the
/// special transactions of dashd-rpc's txtouniv fixtures verbatim, the crafted ones included.
/// Below DIP0003 `CheckBlock` and `ContextualCheckBlock` accept any special type and payload, so
/// the block is written to disk; `ConnectBlock`'s CbTx check passes and step 4 refuses the rest
/// (`bad-tx-type-dip3-inactive`, pinned as dashd's answer). getblock 1, 2 and 3 print it with
/// every payload object and the block-level `cbTx`, without undo data (n-3), before and after
/// both nodes restart with `-deprecatedrpc=service` (CH7a-2b-2: rdashd judges a special-version
/// coinbase below DIP0003, CH7 D57, BL-516).
// Core: rpc/blockchain.cpp:168-207, 954-1072; core_write.cpp:180-362; validation.cpp:351-385
#[test]
fn getblock_transactions_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    let mut both = |name: &str, method: &str, params: &str| -> String {
        count += 1;
        let request = call(method, params);
        let [theirs, mine] = [core_port, our_port].map(|port| exchange(port, &request));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            failures.push(format!("{name}:\n  dashd  {}\n  rdashd {}", show(&theirs), show(&mine)));
        }
        let text = String::from_utf8(theirs).unwrap();
        text.split_once("\r\n\r\n").unwrap().1.to_owned()
    };
    let hashes = |body: &str| -> Vec<String> {
        let reply = UniValue::read(body.as_bytes()).unwrap();
        let UniValue::Obj(fields) = reply else {
            panic!("{body}")
        };
        let result = &fields.iter().find(|(k, _)| k == "result").unwrap().1;
        result.get_values().unwrap().iter().map(|v| v.get_str().unwrap().to_owned()).collect()
    };
    let coinbase_of = |block: &str, both: &mut dyn FnMut(&str, &str, &str) -> String| {
        let body = both(&format!("getblock_{block}"), "getblock", &format!(r#"["{block}"]"#));
        let (_, rest) = body.split_once(r#""tx":[""#).unwrap_or_else(|| panic!("{body}"));
        rest[..64].to_owned()
    };

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    both("setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
    let g = GENERATOR_KEY;
    let full = GENERATOR_KEY_FULL;
    let mut shaped = Vec::new();
    for (name, descriptor) in [
        ("pkh", format!("pkh({g})")),
        ("pk", format!("pk({full})")),
        ("multi", format!("multi(1,{g},{full})")),
        ("sh_multi", format!("sh(multi(2,{g},{full}))")),
        ("addr", format!("addr({GENERATOR_ADDRESS})")),
    ] {
        let body = both(
            &format!("mine_{name}"),
            "generatetodescriptor",
            &format!(r#"[1, "{descriptor}"]"#),
        );
        shaped.push(hashes(&body)[0].clone());
    }
    let op_true = hashes(&both("mine_op_true", "generatetodescriptor", r#"[4, "raw(51)"]"#));
    both("mine_maturity", "generatetodescriptor", r#"[100, "raw(51)"]"#);
    let c: Vec<String> = op_true.iter().map(|b| coinbase_of(b, &mut both)).collect();

    let sig = "304502207fa7a6d1e0ee81132a269ad84e68d695483745cde8b541e3bf630749894e342a022100c1f7ab20e13e22fb95281a870f3dcf38d782e53023ee313d741ad0cfbc0c509001";
    let signed = format!("{:02x}{sig}{:02x}{g}", sig.len() / 2, g.len() / 2);
    let p2pkh = format!("76a914{}88ac", "22".repeat(20));
    let p2sh = format!("a914{}87", "33".repeat(20));
    // At most what each regtest coinbase pays below G1, whatever share the budget keeps.
    let coin = 30_000_000_000;
    let tx1 = spend_tx(1, &[(&c[0], 0, &signed)], &[(100_000_000, "51"), (200_000_000, &p2pkh)]);
    let tx2 = spend_tx(
        2,
        &[(&c[1], 0, ""), (&c[2], 0, "0100")],
        &[(coin, "51"), (0, "6a04deadbeef"), (500_000, &p2sh)],
    );
    let tx3 = spend_tx(1, &[(&rpc_txid(&tx1), 0, "")], &[(50_000_000, "51")]);
    let spends = both(
        "block_spends",
        "generateblock",
        &format!(r#"["{GENERATOR_ADDRESS}", ["{tx1}", "{tx2}", "{tx3}"]]"#),
    );
    assert!(spends.contains(r#""error":null"#), "{spends}");
    let (_, rest) = spends.split_once(r#""hash":""#).unwrap();
    let spends_block = rest[..64].to_owned();
    let tx4 = spend_tx(1, &[(&c[3], 0, "")], &[(coin, "51")]);
    let tx5 = spend_tx(1, &[(&rpc_txid(&tx2), 0, "51")], &[(coin - 1_000, "51")]);
    // The `pkh` coinbase, signed by dashd with the generator's key (the private key 1).
    let pkh_coinbase = coinbase_of(&shaped[0], &mut both);
    let unsigned = spend_tx(1, &[(&pkh_coinbase, 0, "")], &[(coin, "51")]);
    let signed_reply = String::from_utf8(exchange(
        core_port,
        &call("signrawtransactionwithkey", &format!(r#"["{unsigned}", ["{GENERATOR_WIF}"]]"#)),
    ))
    .unwrap();
    assert!(signed_reply.contains(r#""complete":true"#), "{signed_reply}");
    let (_, rest) = signed_reply.split_once(r#""hex":""#).unwrap();
    let tx6 = rest.split('"').next().unwrap().to_owned();
    let next = both(
        "block_next",
        "generateblock",
        &format!(r#"["{GENERATOR_ADDRESS}", ["{tx4}", "{tx5}", "{tx6}"]]"#),
    );
    assert!(next.contains(r#""error":null"#), "{next}");
    let (_, rest) = next.split_once(r#""hash":""#).unwrap();
    let next_block = rest[..64].to_owned();

    let genesis = result_str(core_port, r#"{"method":"getblockhash","params":[0],"id":1}"#);
    let mut blocks = vec![("genesis", genesis), ("op_true", op_true[0].clone())];
    blocks.extend(["pkh", "pk", "multi", "sh_multi", "addr"].into_iter().zip(shaped));
    blocks.push(("spends", spends_block.clone()));
    blocks.push(("next", next_block.clone()));
    let mut replies = std::collections::HashMap::new();
    for (name, hash) in &blocks {
        for (v, rest) in [
            ("1", ",1"),
            ("2", ",2"),
            ("3", ",3"),
            ("4", ",4"),
            ("true", ",true"),
            ("false", ",false"),
        ] {
            let case = format!("getblock_{name}_{v}");
            let body = both(&case, "getblock", &format!(r#"["{hash}"{rest}]"#));
            replies.insert(case, body);
        }
    }
    both("invalidate_next", "invalidateblock", &format!(r#"["{next_block}"]"#));
    let disconnected =
        both("getblock_next_disconnected_3", "getblock", &format!(r#"["{next_block}",3]"#));
    both("reconsider_next", "reconsiderblock", &format!(r#"["{next_block}"]"#));
    both("getblock_next_reconnected_3", "getblock", &format!(r#"["{next_block}",3]"#));

    // A block stored but never connected (m-1), on the tip.
    let count_reply = String::from_utf8(exchange(core_port, &call("getblockcount", "[]"))).unwrap();
    let (_, rest) = count_reply.split_once(r#""result":"#).unwrap();
    let tip_height: u8 = rest.split(',').next().unwrap().parse().unwrap();
    let time = u32::try_from(now.as_secs() + 600).unwrap();
    let (stored, stored_hash) = unconnectable_block(&next_block, tip_height + 1, time);
    let rejected = both("submitblock_stored", "submitblock", &format!(r#"["{stored}"]"#));
    assert!(rejected.contains(r#""result":"bad-tx-type-dip3-inactive""#), "{rejected}");
    for v in ["1", "2", "3"] {
        let case = format!("getblock_stored_{v}");
        let body = both(&case, "getblock", &format!(r#"["{stored_hash}",{v}]"#));
        replies.insert(case, body);
    }
    both("getbestblockhash_end", "getbestblockhash", "[]");
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    // The same on two fresh nodes under `-deprecatedrpc=service`, past V19 (height 1) so that
    // Core prints the BLS fields as stored (D41). A restart would lose the block on rdashd, which
    // does not keep a stored invalid block across one (BL-517).
    let service = ["-deprecatedrpc=service"];
    let (core, core_port) =
        start(&dashd, "core_service", &[&["-disablewallet"][..], &service].concat());
    let (ours, our_port) = start(Path::new(RDASHD), "ours_service", &service);
    let mut cases = vec![
        ("setmocktime_service".to_owned(), "setmocktime", format!("[{}]", now.as_secs())),
        ("mine_service".to_owned(), "generatetodescriptor", r#"[1, "raw(51)"]"#.to_owned()),
    ];
    let mut run = |cases: Vec<(String, &str, String)>| {
        for (case, method, params) in cases {
            let request = call(method, &params);
            let [theirs, mine] = [core_port, our_port].map(|port| exchange(port, &request));
            count += 1;
            let show = |b: &[u8]| b.escape_ascii().to_string();
            if theirs != mine {
                failures.push(format!(
                    "{case}:\n  dashd  {}\n  rdashd {}",
                    show(&theirs),
                    show(&mine)
                ));
            }
            replies.insert(case, String::from_utf8(theirs).unwrap());
        }
    };
    run(cases);
    let one = result_str(core_port, r#"{"method":"getblockhash","params":[1],"id":1}"#);
    let (stored, stored_hash) = unconnectable_block(&one, 2, time);
    cases = vec![("submitblock_service".to_owned(), "submitblock", format!(r#"["{stored}"]"#))];
    for v in ["2", "3"] {
        let params = format!(r#"["{stored_hash}",{v}]"#);
        cases.push((format!("getblock_stored_service_{v}"), "getblock", params));
    }
    run(cases);
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    // The cases reach what they are for.
    let spends3 = &replies["getblock_spends_3"];
    for want in [
        "[ALL]",
        r#""prevout":{"generated":true"#,
        r#""prevout":{"generated":false"#,
        r#""fee":"#,
        "nulldata",
    ] {
        assert!(spends3.contains(want), "{want}: {spends3}");
    }
    assert!(!replies["getblock_spends_2"].contains("prevout"));
    assert!(replies["getblock_spends_2"].contains(r#""fee":"#));
    assert!(!replies["getblock_genesis_3"].contains("prevout"));
    assert!(disconnected.contains("prevout"), "{disconnected}");
    let next3 = &replies["getblock_next_3"];
    let p2pkh_prevout = format!(r#""address":"{GENERATOR_ADDRESS}","type":"pubkeyhash"}}}}"#);
    assert!(next3.contains(&p2pkh_prevout), "{next3}");
    let stored3 = &replies["getblock_stored_3"];
    for want in [
        r#""confirmations":-1"#,
        r#""proRegTx""#,
        r#""proUpServTx""#,
        r#""proUpRegTx""#,
        r#""proUpRevTx""#,
        r#""qcTx""#,
        r#""mnhfTx""#,
        r#""assetLockTx""#,
        r#""assetUnlockTx""#,
        r#""proDisTx""#,
        r#""proUpShareTx""#,
        r#""proUpSharedRegTx""#,
        "[invalid entry]",
        "0.07000000000000001",
    ] {
        assert!(stored3.contains(want), "{want}: {stored3}");
    }
    // No undo data: no `prevout`, and no transaction `fee` (the one `fee` is assetUnlockTx's).
    assert!(!stored3.contains("prevout"), "{stored3}");
    assert_eq!(stored3.matches(r#""fee":"#).count(), 1, "{stored3}");
    // The block-level `cbTx` (rpc/blockchain.cpp:201-205) beside the coinbase's own.
    let cbtx = r#""cbTx":{"version":2,"height":"#;
    assert!(replies["getblock_stored_1"].contains(cbtx), "{}", replies["getblock_stored_1"]);
    assert_eq!(stored3.matches(cbtx).count(), 2, "{stored3}");
    let service_submit = &replies["submitblock_service"];
    assert!(service_submit.contains(r#""result":"bad-tx-type-dip3-inactive""#), "{service_submit}");
    let service3 = &replies["getblock_stored_service_3"];
    assert!(service3.contains(r#""service":"#), "{service3}");
    assert!(service3.contains(r#""platformHTTPPort":0"#), "{service3}");
}

/// A block at `height` on `prev` (as RPC shows it) that `CheckBlock` accepts below DIP0003 and
/// `ConnectBlock` refuses: [`cbtx_coinbase`], then [`stored_block_payloads`]. Returns its hex and
/// its hash as RPC shows it.
fn unconnectable_block(prev: &str, height: u8, time: u32) -> (String, String) {
    let mut txs = vec![cbtx_coinbase(height)];
    txs.extend(stored_block_payloads());
    let mut prev = parse_hex(prev);
    prev.reverse();
    let block = mine_txs(&prev.try_into().unwrap(), time, &txs);
    let mut hash = block.hash;
    hash.reverse();
    (block.hex, hex(&hash))
}

/// [`coinbase`] at version 3 with type bits 5, carrying a version-2 CbTx for `height` with zero
/// roots (`evo/cbtx.h`): what `CheckCbTx` accepts below DIP0003, and steps 8–10 there too.
fn cbtx_coinbase(height: u8) -> Vec<u8> {
    let mut tx = coinbase(height, 1);
    tx[..4].copy_from_slice(&(3i32 | 5 << 16).to_le_bytes());
    let payload = [&2u16.to_le_bytes()[..], &u32::from(height).to_le_bytes(), &[0; 64]].concat();
    tx.push(u8::try_from(payload.len()).unwrap());
    tx.extend(payload);
    tx
}

/// The secret key 1 (`GENERATOR_KEY`'s), as a regtest WIF.
const GENERATOR_WIF: &str = "cMahea7zqjxrtgAbB7LSGbcQUr1uX1ojuat9jZodMN87JcbXMTcA";

/// The same key's uncompressed WIF (no 0x01 suffix): Core signs a P2PK output of the full key only
/// with a key flagged uncompressed.
const GENERATOR_WIF_UNCOMPRESSED: &str = "91avARGdfge8E4tZfYLoxeJ5sGBdNJQH4kvjJoQFacbgwmaKkrx";

/// The special transactions of dashd-rpc's txtouniv fixtures (PROVENANCE.md there) that a block
/// below DIP0003 can store: one regtest row per payload shape, crafted rows included. CbTxs are
/// coinbases, and a block has one; the stored block's own coinbase carries its CbTx.
fn stored_block_payloads() -> Vec<Vec<u8>> {
    use std::io::BufRead as _;
    const SHAPES: [&str; 22] = [
        "t1/pv1/mn0",
        "t1/pv2/mn0",
        "t1/pv2/mn1",
        "t1/pv3/mn1",
        "t1/pv3/mn0/ownreward",
        "t2/pv2/mn1",
        "t2/pv3/mn1/domain",
        "t3/pv3",
        "t4/pv3",
        "t6/pv1/qv4/full",
        "t7/pv1",
        "t8/pv2",
        "t9/pv2",
        "t10/pv1",
        "t11/pv1",
        "t12/pv1",
        "crafted/t2/pv3/mn1/invalid",
        "crafted/t11/pv1/noreward",
        "crafted/t1/pv2/mn0/opreward7",
        "crafted/t2/pv3/mn1/invalidfirst",
        "crafted/t11/pv1/nonstdreward",
        "crafted/t1/pv2/mn0/opreward57",
    ];
    let mut found = std::collections::HashMap::new();
    for name in ["decoderawtransaction-v24.0.0-rc.4", "decoderawtransaction-crafted-v24.0.0-rc.4"] {
        let path = format!(
            "{}/../dashd-rpc/tests/data/txtouniv/{name}.jsonl.gz",
            env!("CARGO_MANIFEST_DIR")
        );
        let file = std::fs::File::open(path).expect("the txtouniv fixture");
        for line in std::io::BufReader::new(flate2::read::GzDecoder::new(file)).lines() {
            let UniValue::Obj(row) = UniValue::read(line.unwrap().as_bytes()).unwrap() else {
                panic!("a fixture row")
            };
            let field = |k: &str| row.iter().find(|(name, _)| name == k).unwrap().1.clone();
            if field("net").get_str().unwrap() != "regtest" || field("service").get_bool().unwrap()
            {
                continue;
            }
            let bucket = field("bucket").get_str().unwrap().to_owned();
            found.entry(bucket).or_insert_with(|| parse_hex(field("hex").get_str().unwrap()));
        }
    }
    SHAPES.iter().map(|shape| found.remove(*shape).unwrap_or_else(|| panic!("{shape}"))).collect()
}

/// One JSON-RPC batch of `(name, method, params)` calls sent to dashd (`ports.0`) and rdashd
/// (`ports.1`), the replies compared byte for byte with the `Date` header masked; a differing
/// batch is reported per call where the replies parse. Returns dashd's results, in order.
fn diff_batch(
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

/// A `filterload` filter for 10 elements at a 10⁻⁶ false-positive rate holding `keys`, with
/// `flags`, serialized as hex.
fn bloom_hex(keys: &[Vec<u8>], flags: u8) -> String {
    use dash_types::codec::BaseCodec as _;
    let mut filter = dashd_p2p::processing::bloom::BloomFilter::new(10, 0.000_001, 0, flags);
    for key in keys {
        filter.insert(key);
    }
    let mut wire = Vec::new();
    filter.to_msg().encode(&mut wire);
    hex(&wire)
}

/// RP1a-2c-3: getmerkleblocks sent to dashd and rdashd on fresh regtest chains under one mock
/// time, as JSON-RPC batches, each reply equal byte for byte with the `Date` header masked.
/// - **Errors:** rpc_getmerkleblocks.py's size-precheck boundaries and every reading error of the
///   filter (end of data, a non-canonical or too-large count, 51 hash functions), lenient hex
///   (whitespace, upper case, odd or bad digits), the hash's and `count`'s errors in Core's
///   order, the help and wrong arities.
/// - **Matching:** coinbases to `pkh(G)` and `pk(G_full)`, matured, then one block spending both
///   with signatures from dashd's `signrawtransactionwithkey`. Filters on G's key hash and full
///   key under `BLOOM_UPDATE_NONE`, `_ALL` and `_P2PUBKEY_ONLY` show whether a match in one
///   block inserts the outpoint the next block's spend matches (one filter for the whole walk);
///   filters on the spend's txid and on a spent outpoint; all-ones, all-zeros and empty `vData`.
///   Walks start at genesis, mid-chain and at the tip, with counts from 1 to 2000.
/// - **Starts:** a block off the active chain (invalidated, so `Next` is null), a header without
///   its block ("Block not found on disk"), and a block stored but never connected carrying the
///   txtouniv special transactions, matched whole (quorum commitments and MnHF signals never
///   match) and by a ProUpServTx's `proTxHash`.
// Core: rpc/blockchain.cpp:827-924; merkleblock.cpp:30-72; common/bloom.cpp:29-305
#[test]
fn getmerkleblocks_match_dashd_v24() {
    use bitcoin_hashes::hash160;

    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    let mut batch = |calls: Vec<(String, &str, String)>| {
        count += calls.len();
        diff_batch((core_port, our_port), &calls, &mut failures)
    };
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);
    let get = |v: &UniValue, key: &str| -> UniValue {
        let UniValue::Obj(fields) = v else {
            panic!("{}", v.write())
        };
        fields.iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("{key}")).1.clone()
    };
    let text = |v: UniValue| v.get_str().unwrap().to_owned();

    // The chain: genesis, `pkh(G)` at 1, `pk(G_full)` at 2, 100 blocks to maturity, the spends at
    // 103, then two empty blocks.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let g = GENERATOR_KEY;
    let full = GENERATOR_KEY_FULL;
    let mined = batch(vec![
        one("setmocktime", "setmocktime", format!("[{}]", now.as_secs())),
        one("mine_pkh", "generatetodescriptor", format!(r#"[1, "pkh({g})"]"#)),
        one("mine_pk", "generatetodescriptor", format!(r#"[1, "pk({full})"]"#)),
        one("mine_maturity", "generatetodescriptor", r#"[100, "raw(51)"]"#.into()),
        one("genesis", "getblockhash", "[0]".into()),
    ]);
    let hash_at = |v: &UniValue| text(v.get_values().unwrap()[0].clone());
    let (pkh_block, pk_block, genesis) =
        (hash_at(&mined[1]), hash_at(&mined[2]), text(mined[4].clone()));
    let blocks = batch(vec![
        one("getblock_pkh", "getblock", format!(r#"["{pkh_block}"]"#)),
        one("getblock_pk", "getblock", format!(r#"["{pk_block}"]"#)),
    ]);
    let coinbase_txid = |b: &UniValue| text(get(b, "tx").get_values().unwrap()[0].clone());
    let (pkh_cb, pk_cb) = (coinbase_txid(&blocks[0]), coinbase_txid(&blocks[1]));
    // At most what each regtest coinbase pays below G1.
    let coin = 30_000_000_000;
    let unsigned = spend_tx(1, &[(&pkh_cb, 0, ""), (&pk_cb, 0, "")], &[(2 * coin, "51")]);
    let signed_reply = String::from_utf8(exchange(
        core_port,
        &call(
            "signrawtransactionwithkey",
            &format!(r#"["{unsigned}", ["{GENERATOR_WIF}", "{GENERATOR_WIF_UNCOMPRESSED}"]]"#),
        ),
    ))
    .unwrap();
    assert!(signed_reply.contains(r#""complete":true"#), "{signed_reply}");
    let (_, rest) = signed_reply.split_once(r#""hex":""#).unwrap();
    let spend = rest.split('"').next().unwrap().to_owned();
    let spent = batch(vec![
        one("block_spend", "generateblock", format!(r#"["raw(51)", ["{spend}"]]"#)),
        one("mine_after", "generatetodescriptor", r#"[2, "raw(51)"]"#.into()),
        one("tip", "getbestblockhash", "[]".into()),
    ]);
    let spend_block = text(get(&spent[0], "hash"));
    let tip = text(spent[2].clone());

    // The filters.
    let key = parse_hex(g);
    let key_full = parse_hex(full);
    let key_hash = hash160::Hash::hash(&key).to_byte_array().to_vec();
    let internal = |txid: &str| {
        let mut id = parse_hex(txid);
        id.reverse();
        id
    };
    let outpoint = [internal(&pkh_cb), 0u32.to_le_bytes().to_vec()].concat();
    let (none, all, p2pk_only) = (0, 1, 2);
    let filters = [
        ("pkh_none", bloom_hex(std::slice::from_ref(&key_hash), none)),
        ("pkh_all", bloom_hex(std::slice::from_ref(&key_hash), all)),
        ("pkh_p2pk", bloom_hex(std::slice::from_ref(&key_hash), p2pk_only)),
        ("pk_none", bloom_hex(std::slice::from_ref(&key_full), none)),
        ("pk_p2pk", bloom_hex(std::slice::from_ref(&key_full), p2pk_only)),
        ("txid", bloom_hex(&[internal(&rpc_txid(&spend))], none)),
        ("outpoint", bloom_hex(&[outpoint], none)),
        ("ones", format!("03ffffff{}0000000000", "05000000")),
        ("zeros", format!("03000000{}0000000000", "05000000")),
        ("empty", format!("00{}0000000000", "05000000")),
    ];
    let mut walks = Vec::new();
    for (name, filter) in &filters {
        for (from, hash, counts) in [
            ("genesis", &genesis, &["2000", "3", "104"][..]),
            ("pkh", &pkh_block, &["1", "102", "103"]),
            ("spend", &spend_block, &["1", "2000"]),
            ("tip", &tip, &["5"]),
        ] {
            for n in counts {
                let params = format!(r#"["{filter}", "{hash}", {n}]"#);
                walks.push(one(&format!("{name}_{from}_{n}"), "getmerkleblocks", params));
            }
        }
    }
    let matched = batch(walks.clone());

    // The errors, on the genesis block.
    let ok = "03000000010000000000000000";
    let compact = |n: u64| -> String {
        if n < 0xFD {
            format!("{n:02x}")
        } else if n <= 0xFFFF {
            format!("fd{}", hex(&(n as u16).to_le_bytes()))
        } else {
            format!("fe{}", hex(&(n as u32).to_le_bytes()))
        }
    };
    let over = 36_001;
    let zeros = |n: usize| "00".repeat(n);
    let mut errors = vec![];
    for (name, filter) in [
        ("oversize_complete", format!("{}{}{}", compact(over), zeros(over as usize), zeros(9))),
        ("oversize_truncated", compact(over)),
        (
            "oversize_trailer_short",
            format!("{}{}{}", compact(over), zeros(over as usize), zeros(8)),
        ),
        ("limit", format!("{}{}{}", compact(36_000), zeros(36_000), zeros(9))),
        ("trailer_short", "0300000000".into()),
        ("empty_string", String::new()),
        ("noncanonical", format!("fd0300{}", &ok[2..])),
        ("too_large", "fe00000003".into()),
        ("hash_funcs_51", "0300000033000000000000000000".into()),
        ("hash_funcs_50", "0300000032000000000000000000".into()),
        ("trailing", format!("{ok}abcd")),
        ("bad_hex", "zz".into()),
        ("odd_hex", format!("{ok}0")),
        ("spaced", " 03 000000 01000000 00000000 00 ".into()),
        // `IsSpace`'s other characters, as JSON escapes (r1 m-1).
        ("tab_newline", r"03\t000000\n0100000000000000000000".into()),
        ("vertical_tab", r"03\u000b000000010000000000000000".into()),
        ("form_feed", r"03\u000c000000010000000000000000".into()),
        ("carriage_return", r"0300\r0000010000000000000000\r".into()),
        ("upper", "03000000010000000000000000".to_uppercase()),
    ] {
        errors.push(one(
            &format!("filter_{name}"),
            "getmerkleblocks",
            format!(r#"["{filter}", "{genesis}", 1]"#),
        ));
    }
    let unknown = "11".repeat(32);
    for (name, params) in [
        ("filter_number", format!(r#"[5, "{genesis}", 1]"#)),
        ("filter_null", format!(r#"[null, "{genesis}", 1]"#)),
        ("hash_short", format!(r#"["{ok}", "abc", 1]"#)),
        ("hash_nonhex", format!(r#"["{ok}", "{}", 1]"#, "z".repeat(64))),
        ("hash_number", format!(r#"["{ok}", 5, 1]"#)),
        ("hash_unknown", format!(r#"["{ok}", "{unknown}", 1]"#)),
        ("count_0", format!(r#"["{ok}", "{genesis}", 0]"#)),
        ("count_negative", format!(r#"["{ok}", "{genesis}", -1]"#)),
        ("count_2001", format!(r#"["{ok}", "{genesis}", 2001]"#)),
        ("count_string", format!(r#"["{ok}", "{genesis}", "5"]"#)),
        ("count_fraction", format!(r#"["{ok}", "{genesis}", 1.5]"#)),
        ("count_huge", format!(r#"["{ok}", "{genesis}", 4294967297]"#)),
        ("count_null", format!(r#"["{ok}", "{genesis}", null]"#)),
        ("count_bool", format!(r#"["{ok}", "{genesis}", true]"#)),
        ("bad_filter_unknown_hash", format!(r#"["", "{unknown}", 0]"#)),
        ("bad_filter_bad_hash", r#"["", "abc", 0]"#.into()),
        ("unknown_hash_count_0", format!(r#"["{ok}", "{unknown}", 0]"#)),
        ("no_args", "[]".into()),
        ("one_arg", format!(r#"["{ok}"]"#)),
        ("four_args", format!(r#"["{ok}", "{genesis}", 1, 1]"#)),
        ("named", format!(r#"{{"filter":"{ok}","blockhash":"{genesis}"}}"#)),
    ] {
        errors.push(one(name, "getmerkleblocks", params));
    }
    errors.push(one("help", "help", r#"["getmerkleblocks"]"#.into()));
    batch(errors);

    // Off the active chain: the invalidated tip reads alone, though a branch from its parent now
    // has a block above it; then that branch is invalidated and the old tip is back.
    let ones = &filters[7].1;
    let parent = batch(vec![
        one("invalidate_tip", "invalidateblock", format!(r#"["{tip}"]"#)),
        one("parent", "getbestblockhash", "[]".into()),
        one("branch", "generatetodescriptor", r#"[2, "raw(52)"]"#.into()),
    ]);
    let branch = text(parent[2].get_values().unwrap()[0].clone());
    let parent = text(parent[1].clone());
    let back = batch(vec![
        one("off_chain_5", "getmerkleblocks", format!(r#"["{ones}", "{tip}", 5]"#)),
        one("new_tip_5", "getmerkleblocks", format!(r#"["{ones}", "{parent}", 5]"#)),
        one("invalidate_branch", "invalidateblock", format!(r#"["{branch}"]"#)),
        one("reconsider_tip", "reconsiderblock", format!(r#"["{tip}"]"#)),
        one("tip_again", "getbestblockhash", "[]".into()),
    ]);
    assert_eq!(text(back[4].clone()), tip);

    // A header without its block, and a block stored but never connected, on the tip.
    let tip_height = 105;
    let time = u32::try_from(now.as_secs() + 600).unwrap();
    let mut prev = parse_hex(&tip);
    prev.reverse();
    let headless =
        mine(&prev.try_into().unwrap(), time, &[coinbase(tip_height + 1, 1)], None, true);
    let mut headless_hash = headless.hash;
    headless_hash.reverse();
    let headless_hash = hex(&headless_hash);
    let (stored, stored_hash) = unconnectable_block(&tip, tip_height + 1, time + 1);
    let stored_reply = batch(vec![
        one("submitheader", "submitheader", format!(r#"["{}"]"#, &headless.hex[..160])),
        one("headless", "getmerkleblocks", format!(r#"["{ones}", "{headless_hash}", 1]"#)),
        one("submit_stored", "submitblock", format!(r#"["{stored}"]"#)),
        one("getblock_stored", "getblock", format!(r#"["{stored_hash}", 2]"#)),
    ]);
    let up_serv = get(&stored_reply[3], "tx")
        .get_values()
        .unwrap()
        .iter()
        .find_map(|tx| match tx {
            UniValue::Obj(fields) => fields
                .iter()
                .find(|(k, _)| k == "proUpServTx")
                .map(|(_, p)| text(get(p, "proTxHash"))),
            _ => None,
        })
        .unwrap();
    let pro_tx = bloom_hex(&[internal(&up_serv)], none);
    let stored_walks = batch(vec![
        one("stored_ones", "getmerkleblocks", format!(r#"["{ones}", "{stored_hash}", 1]"#)),
        one("stored_pro_tx", "getmerkleblocks", format!(r#"["{pro_tx}", "{stored_hash}", 1]"#)),
        one(
            "stored_pro_tx_all",
            "getmerkleblocks",
            format!(r#"["{}", "{stored_hash}", 1]"#, bloom_hex(&[internal(&up_serv)], all)),
        ),
    ]);

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    // The cases reach what they are for: a walk's length per filter (genesis, 2000).
    let blocks_matched = |name: &str| {
        let i = walks.iter().position(|(n, _, _)| n == name).unwrap();
        matched[i].get_values().unwrap().len()
    };
    // The key hash matches the pkh coinbase; with UPDATE_ALL its outpoint then matches the
    // spend; P2PUBKEY_ONLY inserts only the pk coinbase's.
    assert_eq!(blocks_matched("pkh_none_genesis_2000"), 1);
    assert_eq!(blocks_matched("pkh_all_genesis_2000"), 2);
    assert_eq!(blocks_matched("pkh_p2pk_genesis_2000"), 1);
    assert_eq!(blocks_matched("pk_none_genesis_2000"), 1);
    assert_eq!(blocks_matched("pk_p2pk_genesis_2000"), 2);
    assert_eq!(blocks_matched("txid_genesis_2000"), 1);
    assert_eq!(blocks_matched("outpoint_genesis_2000"), 1);
    assert_eq!(blocks_matched("ones_genesis_2000"), 106);
    assert_eq!(blocks_matched("zeros_genesis_2000"), 0);
    assert_eq!(blocks_matched("empty_genesis_2000"), 106);
    // Starting past the coinbase, the spend no longer matches by the inserted outpoint.
    assert_eq!(blocks_matched("pkh_all_spend_2000"), 0);
    for (i, want) in [(0, true), (1, true), (2, true)] {
        assert_eq!(!stored_walks[i].get_values().unwrap().is_empty(), want, "{i}");
    }
}

/// DIFF-RPC `verifychain` (RP1a-2c-4): dashd and rdashd answer the same at every level and depth
/// on a healthy chain, and the same errors.
/// - **Chains:** genesis alone (VerifyDB's early `SUCCESS`), then 110 blocks, then the same with
///   its tip invalidated (VerifyDB reads the active chain).
/// - **Arguments:** levels -1 to 5 (clamped to 0..=4) by depths -1, 0, 1, 6, the height and more;
///   the defaults; non-integers, out-of-range integers, extra and named arguments, and `help`.
/// - rdashd's VerifyDB point verifies nothing until CH5 (D46), so this pins the RPC surface, not
///   VerifyDB's checks.
// Core: rpc/blockchain.cpp:1413-1443; validation.cpp:5036-5060
#[test]
fn verifychain_matches_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    let mut batch = |calls: Vec<(String, &str, String)>| {
        count += calls.len();
        diff_batch((core_port, our_port), &calls, &mut failures)
    };
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);
    let levels_by_depths = |chain: &str, height: u32| {
        let mut calls = vec![
            one(&format!("{chain}_default"), "verifychain", "[]".into()),
            one(&format!("{chain}_nulls"), "verifychain", "[null, null]".into()),
        ];
        for level in -1..=5 {
            for depth in [-1, 0, 1, 6, i64::from(height), i64::from(height) + 1, 2_147_483_647] {
                let name = format!("{chain}_{level}_{depth}");
                calls.push(one(&name, "verifychain", format!("[{level}, {depth}]")));
            }
        }
        calls
    };

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let mut genesis = vec![one("setmocktime", "setmocktime", format!("[{}]", now.as_secs()))];
    genesis.extend(levels_by_depths("genesis", 0));
    let logs = ["core", "ours"].map(|name| debug_log(&home.path().join(name)));
    let from = logs.clone().map(|log| read(&log).len());
    let genesis = batch(genesis);
    // At a genesis tip VerifyDB answers before it logs (`validation.cpp:5045-5047`; r1 n-2).
    let [theirs, mine] = [0, 1].map(|i| verify_lines(&logs[i], from[i], 0));
    assert_eq!((theirs.as_slice(), mine.as_slice()), (&[][..], &[][..]), "genesis logs");
    let mut mined = vec![one("mine", "generatetodescriptor", r#"[110, "raw(51)"]"#.into())];
    mined.extend(levels_by_depths("mined", 110));
    let mined = batch(mined);
    let tip = mined[0].get_values().unwrap()[109].get_str().unwrap().to_owned();
    let mut invalid = vec![one("invalidate_tip", "invalidateblock", format!(r#"["{tip}"]"#))];
    invalid.extend(levels_by_depths("invalid", 109));
    invalid.push(one("reconsider_tip", "reconsiderblock", format!(r#"["{tip}"]"#)));
    let invalid = batch(invalid);
    // dashd verified each healthy chain: every call answered true.
    let verified = |results: &[UniValue]| results.iter().all(|r| *r == UniValue::Bool(true));
    assert!(verified(&genesis[1..]) && verified(&mined[1..]));
    assert!(verified(&invalid[1..invalid.len() - 1]));

    let mut errors = vec![];
    for (name, params) in [
        ("level_string", r#"["3"]"#),
        ("level_fraction", "[1.5]"),
        ("level_bool", "[true]"),
        ("level_huge", "[2147483648]"),
        ("level_negative_huge", "[-2147483649]"),
        ("depth_string", r#"[3, "6"]"#),
        ("depth_fraction", "[3, 0.5]"),
        ("depth_huge", "[3, 4294967296]"),
        ("depth_object", "[3, {}]"),
        ("three_args", "[3, 6, 1]"),
        ("named", r#"{"checklevel":4,"nblocks":0}"#),
        ("named_depth_only", r#"{"nblocks":1}"#),
        ("named_unknown", r#"{"level":4}"#),
    ] {
        errors.push(one(name, "verifychain", params.into()));
    }
    errors.push(one("help", "help", r#"["verifychain"]"#.into()));
    batch(errors);

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
}

/// `dir` copied to `to`, file by file.
fn copy_dir(dir: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

/// Where `block`'s bytes start in `file`.
fn find_bytes(file: &[u8], block: &[u8]) -> usize {
    file.windows(block.len()).position(|w| w == block).expect("the block in its blk file")
}

/// The offset of the checksum of each undo record in a rev file: records of a 4-byte magic, a
/// 4-byte size, the undo data and its 32-byte checksum, until the preallocated zeros.
fn undo_checksums(rev: &[u8]) -> Vec<usize> {
    let mut at = 0;
    let mut out = Vec::new();
    while at + 8 <= rev.len() && rev[at..at + 4] != [0; 4] {
        let size = u32::from_le_bytes(rev[at + 4..at + 8].try_into().unwrap()) as usize;
        out.push(at + 8 + size);
        at += 8 + size + 32;
    }
    out
}

/// The lines VerifyDB and the reads it calls log, from `log` after byte `from`, without their
/// timestamps; at levels 3 and 4 without the progress and summary lines, which count CH5's
/// checks (BL-524).
fn verify_lines(log: &Path, from: usize, level: i32) -> Vec<String> {
    log_messages(log, from)
        // Each node's own datadir, cut to the part under it.
        .map(|l| match l.find("/blocks/") {
            Some(at) if l.starts_with("Unable to open file ") => {
                format!("Unable to open file …{}", &l[at..])
            }
            _ => l,
        })
        .filter(|l| {
            [
                "Unable to open file ",
                "Verifying last ",
                "Verification progress: ",
                "Verification: No coin database",
                "VerifyDB(): ",
                "[error] Verification error: ",
                "[error] ReadBlockFromDisk",
                "[error] UndoReadFromDisk",
            ]
            .iter()
            .any(|p| l.starts_with(p))
        })
        .filter(|l| {
            level < 3
                || !(l.starts_with("Verification progress")
                    || l.starts_with("Verification: No coin database"))
        })
        .collect()
}

/// VerifyDB's progress lines in `log` after byte `from`, without their timestamps.
fn progress_lines(log: &Path, from: usize) -> Vec<String> {
    log_messages(log, from).filter(|l| l.starts_with("Verification progress: ")).collect()
}

/// The lines of `log` after byte `from`, without their timestamps and the mock time logged
/// after them (`(mocktime: …)`, `logging.cpp`'s `LogTimestampStr`).
fn log_messages(log: &Path, from: usize) -> impl Iterator<Item = String> {
    let text = read(log);
    let lines: Vec<String> = text
        .get(from..)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest))
        .map(|l| {
            l.strip_prefix("(mocktime: ")
                .and_then(|r| r.split_once(") "))
                .map_or(l, |(_, rest)| rest)
                .to_owned()
        })
        .collect();
    lines.into_iter()
}

/// DIFF-RPC `verifychain` over corrupted copies of the same 20-block chain (RP1a-2c-4, DEC-766):
/// - **garbled_tx:** a block's last byte flipped, so `CheckBlock` finds a bad merkle root;
/// - **garbled_count:** a block's transaction count made a huge CompactSize, so the read fails;
/// - **garbled_count_noncanonical:** the count spelled `fd 01 00`, which the read refuses too;
/// - **bad_undo:** the undo checksum of the block above it flipped;
/// - **rev_truncated**, **rev_removed:** the rev file cut before that checksum, or removed
///   (startup only: the default VerifyDB reads the tip's undo);
/// - **truncated:** the tip's block cut short (startup only).
///
/// With the damage at 10 (11), past startup's default depth, each copy starts and runs
/// `verifychain` at levels 0 to 4 over depths inside and beyond it; dashd and rdashd must answer
/// and log the same. With the damage at 17 (18) or the tip, inside the default depth, both must
/// refuse to start the same way.
// Core: validation.cpp:5036-5180; node/blockstorage.cpp:539-575, :805-848; init.cpp:2115-2133
#[test]
fn verifychain_on_corrupted_datadirs_matches_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let args_for = |port: u16, extra: &[&str]| -> Vec<String> {
        let mut args = vec!["-printtoconsole=0".to_owned(), "-listen=0".to_owned()];
        args.push(format!("-rpcport={port}"));
        args.extend(credentials.iter().chain(extra).map(|a| (*a).to_owned()));
        args
    };
    let start = |binary: &Path, datadir: &Path, extra: &[&str]| {
        std::fs::create_dir_all(datadir).unwrap();
        let port = free_port();
        let args = args_for(port, extra);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        (node(regtest_on(binary, home.path(), datadir, &args), datadir), port)
    };
    let nodes = [(dashd.clone(), "core", vec!["-disablewallet"]), (RDASHD.into(), "ours", vec![])];
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);
    let mut failures = Vec::new();

    // The chain, on each node; dashd's blocks by height.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let mut started = Vec::new();
    for (binary, name, extra) in &nodes {
        started.push(start(binary, &home.path().join(name), extra));
    }
    let ports = (started[0].1, started[1].1);
    let mut calls = vec![
        one("setmocktime", "setmocktime", format!("[{}]", now.as_secs())),
        one("mine", "generatetodescriptor", r#"[20, "raw(51)"]"#.into()),
    ];
    let mined = diff_batch(ports, &calls, &mut failures);
    let hashes: Vec<String> =
        mined[1].get_values().unwrap().iter().map(|h| h.get_str().unwrap().to_owned()).collect();
    calls = [10, 17, 20]
        .iter()
        .map(|&h| one(&format!("raw_{h}"), "getblock", format!(r#"["{}", 0]"#, hashes[h - 1])))
        .collect();
    let raw = diff_batch(ports, &calls, &mut failures);
    let raw: Vec<Vec<u8>> = raw.iter().map(|r| parse_hex(r.get_str().unwrap())).collect();
    for (running, _) in started {
        running.stop_ok(libc::SIGTERM);
    }
    assert!(failures.is_empty(), "the chain differs:\n{}", failures.join("\n"));

    // The damage, applied to each node's own files: to the block at `height` (10, 17 or the tip)
    // or the undo record of the block above it.
    let damage = |name: &str, height: usize, blocks: &Path| {
        let block = &raw[[10, 17, 20].iter().position(|&h| h == height).unwrap()];
        let blk = blocks.join("blk00000.dat");
        let rev = blocks.join("rev00000.dat");
        let mut bytes = std::fs::read(&blk).unwrap();
        match name {
            "garbled_tx" => {
                let at = find_bytes(&bytes, block) + block.len() - 1;
                bytes[at] ^= 0x01;
            }
            "garbled_count" => {
                let at = find_bytes(&bytes, block) + 80;
                bytes[at] = 0xff;
            }
            "garbled_count_noncanonical" => {
                // The count (1) spelled in three bytes, over the coinbase's version.
                let at = find_bytes(&bytes, block) + 80;
                bytes[at..at + 3].copy_from_slice(&[0xfd, 0x01, 0x00]);
            }
            "bad_undo" | "rev_truncated" => {
                let mut undo = std::fs::read(&rev).unwrap();
                let records = undo_checksums(&undo);
                assert_eq!(records.len(), 20, "one undo record per block after genesis");
                if name == "bad_undo" {
                    undo[records[height]] ^= 0x01;
                } else {
                    // Cut before the checksum of the record of the block above `height`.
                    undo.truncate(records[height]);
                }
                std::fs::write(&rev, undo).unwrap();
            }
            "rev_removed" => std::fs::remove_file(&rev).unwrap(),
            "truncated" => {
                bytes.truncate(find_bytes(&bytes, block) + 100);
            }
            _ => unreachable!(),
        }
        std::fs::write(&blk, bytes).unwrap();
    };
    let copy = |scenario: &str, height: usize, name: &str| {
        let to = home.path().join(format!("{scenario}-{height}-{name}"));
        copy_dir(&home.path().join(name), &to);
        damage(scenario, height, &net(&to).join("blocks"));
        to
    };

    // Levels by depths, on each copy; the damage is at 10 (11 for the undo) below a tip at 20,
    // past startup's default depth of 6. A depth of 11 reaches 10, 10 does not.
    for scenario in ["garbled_tx", "garbled_count", "garbled_count_noncanonical", "bad_undo"] {
        let mut running = Vec::new();
        for (binary, name, extra) in &nodes {
            let dir = copy(scenario, 10, name);
            running.push((start(binary, &dir, extra), dir));
        }
        let ports = (running[0].0.1, running[1].0.1);
        let count = diff_batch(ports, &[one("count", "getblockcount", "[]".into())], &mut failures);
        assert_eq!(count[0].write(), "20", "{scenario}: dashd loads the whole chain");
        // The level that catches the damage, and the depth that reaches it (0 is the whole chain).
        let (catching, reach) = match scenario {
            "garbled_count" | "garbled_count_noncanonical" => (0, 11),
            "garbled_tx" => (1, 11),
            _ => (2, 10),
        };
        for level in 0..=4 {
            for depth in [0, 6, 9, 10, 11, 21] {
                let at = |d: &Path| std::fs::metadata(debug_log(d)).unwrap().len() as usize;
                let from = [at(&running[0].1), at(&running[1].1)];
                let name = format!("{scenario}_{level}_{depth}");
                let call = [one(&name, "verifychain", format!("[{level}, {depth}]"))];
                let verdict = diff_batch(ports, &call, &mut failures);
                let caught = level >= catching && (depth == 0 || depth >= reach);
                assert_eq!(verdict[0].write(), (!caught).to_string(), "{name}: dashd's verdict");
                let [theirs, mine] =
                    [0, 1].map(|i| verify_lines(&debug_log(&running[i].1), from[i], level));
                if theirs != mine {
                    failures.push(format!("{name} log:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
                }
                // At levels 3 and 4 rdashd's progress is Core's first loop's, which at level 4
                // counts on a scale of 50 (`validation.cpp:5074-5078`; r1 n-3): a prefix of
                // dashd's lines, which go on through the reconnect loop.
                if level >= 3 {
                    let [theirs, mine] =
                        [0, 1].map(|i| progress_lines(&debug_log(&running[i].1), from[i]));
                    if mine.len() < 2 || !theirs.starts_with(&mine) {
                        failures.push(format!(
                            "{name} progress:\n  dashd  {theirs:?}\n  rdashd {mine:?}"
                        ));
                    }
                }
            }
        }
        for ((r, _), _) in running {
            r.stop_ok(libc::SIGTERM);
        }
    }

    // Startup with the default level (3) and depth (6) over damage they reach: 17 (18 for the
    // undo), and the tip's block cut short.
    for (scenario, height) in [
        ("garbled_tx", 17),
        ("garbled_count", 17),
        ("bad_undo", 17),
        ("rev_truncated", 17),
        ("rev_removed", 17),
        ("truncated", 20),
    ] {
        let (mut outcomes, mut tails) = (Vec::new(), Vec::new());
        for (binary, name, extra) in &nodes {
            let dir = copy(scenario, height, name);
            let _ = std::fs::remove_file(debug_log(&dir));
            let args = args_for(free_port(), extra);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            // A node that starts anyway is stopped and reported as started.
            let log = debug_log(&dir);
            let mut child =
                Running::spawn(regtest_on(binary, home.path(), &dir, &args), log.clone());
            let deadline = Instant::now() + Duration::from_secs(120);
            let (code, err) = loop {
                if child.exited() {
                    let out = child.wait();
                    break (out.status.code(), stderr(&out));
                }
                if read(&log).contains("init message: Done loading") || Instant::now() > deadline {
                    child.stop(libc::SIGTERM);
                    break (None, "started".to_owned());
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            outcomes.push((code, err, verify_lines(&log, 0, 3)));
            let text = read(&log);
            let lines: Vec<&str> = text.lines().collect();
            let end =
                lines.iter().position(|l| l.contains("Please restart")).unwrap_or(lines.len());
            tails.push(lines[end.saturating_sub(25)..end].join("\n    "));
        }
        if outcomes[0] != outcomes[1] {
            failures.push(format!(
                "{scenario} startup:\n  dashd  {:?}\n  rdashd {:?}\n  rdashd's log ends:\n    {}",
                outcomes[0], outcomes[1], tails[1]
            ));
        }
        assert!(outcomes[0].0.is_some_and(|c| c != 0), "{scenario}: dashd {:?}", outcomes[0]);
    }
    assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
}

/// RP1a-2c-2: getblockstats and pruneblockchain on Core's own `rpc_getblockstats.json` chain
/// (functional `rpc_getblockstats.py`'s 104 regtest blocks: spends with and without a
/// subtracted fee, a 0.003 fee rate, an `OP_RETURN`), submitted to dashd and rdashd under the
/// file's mock time. Every height by height and by hash, every statistic alone at 101 to 103,
/// the FT's errors, an invalidated block by hash (its undo data still on disk), pruneblockchain,
/// and heights 148 to 152 around regtest's first halving after 50 more mined blocks. The calls go
/// as JSON-RPC batches (each exchange waits out the close watch),
/// each batch's reply equal byte for byte with the `Date` header masked; dashd's replies at 101
/// to 103 equal the file's stats.
// Core: rpc/blockchain.cpp:2211-2449 getblockstats, :1077-1135 pruneblockchain;
// test/functional/rpc_getblockstats.py
#[test]
fn getblockstats_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    // One batch of `(name, method, params)` calls; dashd's results, in order.
    let mut batch = |calls: &[(String, &str, String)]| -> Vec<UniValue> {
        count += calls.len();
        let body: Vec<String> = calls
            .iter()
            .enumerate()
            .map(|(i, (_, method, params))| {
                format!(r#"{{"method":"{method}","params":{params},"id":{i}}}"#)
            })
            .collect();
        let request = post(&format!("[{}]", body.join(",")));
        let [theirs, mine] = [core_port, our_port].map(|port| exchange(port, &request));
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
        core_replies.into_iter().map(|reply| field(&reply, "result").clone()).collect()
    };
    // An object's fields, borrowed.
    fn fields(v: &UniValue) -> &[(String, UniValue)] {
        let UniValue::Obj(fields) = v else {
            panic!("{}", v.write())
        };
        fields
    }
    fn field<'a>(v: &'a UniValue, key: &str) -> &'a UniValue {
        &fields(v).iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("{key}")).1
    }
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);

    let data = UniValue::read(
        dash_core_vectors::read_core_text("test/functional/data/rpc_getblockstats.json").as_bytes(),
    )
    .unwrap();
    batch(&[one("setmocktime", "setmocktime", format!("[{}]", field(&data, "mocktime").write()))]);
    let blocks = field(&data, "blocks").get_values().unwrap();
    let submits: Vec<_> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            one(
                &format!("submitblock_{i}"),
                "submitblock",
                format!(r#"["{}"]"#, b.get_str().unwrap()),
            )
        })
        .collect();
    batch(&submits);
    let tip = blocks.len() - 1;

    let by_height: Vec<_> =
        (0..=tip).map(|h| one(&format!("stats_{h}"), "getblockstats", format!("[{h}]"))).collect();
    let stats = batch(&by_height);
    let hashes: Vec<String> =
        stats.iter().map(|s| field(s, "blockhash").get_str().unwrap().to_owned()).collect();
    let by_hash: Vec<_> = hashes
        .iter()
        .enumerate()
        .map(|(h, hash)| one(&format!("stats_hash_{h}"), "getblockstats", format!(r#"["{hash}"]"#)))
        .collect();
    batch(&by_hash);
    // dashd's own replies are the file's (the FT's check).
    let mut singles = Vec::new();
    for (i, want) in field(&data, "stats").get_values().unwrap().iter().enumerate() {
        let h = 101 + i;
        for (key, value) in fields(want) {
            assert_eq!(field(&stats[h], key).write(), value.write(), "dashd at {h}: {key}");
            singles.push(one(
                &format!("stat_{h}_{key}"),
                "getblockstats",
                format!(r#"[{h}, ["{key}"]]"#),
            ));
        }
    }
    batch(&singles);
    let errors: Vec<_> = [
        ("some", r#"[1, ["minfee", "maxfee"]]"#.to_owned()),
        ("dup", r#"[103, ["txs", "avgfee", "txs"]]"#.to_owned()),
        ("empty_list", "[103, []]".to_owned()),
        ("after_tip", format!("[{}]", tip + 1)),
        ("negative", "[-1]".to_owned()),
        ("inv_1", r#"[1, ["asdfghjkl"]]"#.to_owned()),
        ("inv_2", r#"[1, ["minfee", "asdfghjkl"]]"#.to_owned()),
        ("inv_3", r#"[1, ["asdfghjkl", "minfee"]]"#.to_owned()),
        ("inv_4", r#"[1, ["minfee", "asdfghjkl", "maxfee"]]"#.to_owned()),
        ("inv_aaa", r#"[1, ["minfee", "aaaasdfghjkl"]]"#.to_owned()),
        ("stat_not_str", "[1, [1]]".to_owned()),
        ("stats_not_arr", r#"[1, "minfee"]"#.to_owned()),
        (
            "unknown_hash",
            r#"["000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"]"#.to_owned(),
        ),
        ("short_hash", r#"["00"]"#.to_owned()),
        ("bool", "[true]".to_owned()),
        ("fraction", "[1.5]".to_owned()),
        ("too_many", r#"["00", 1, 2]"#.to_owned()),
        ("none", "[]".to_owned()),
    ]
    .into_iter()
    .map(|(name, params)| one(&format!("stats_{name}"), "getblockstats", params))
    .collect();
    batch(&errors);
    // An invalidated block keeps its undo data: by hash it reads as before; by height it is
    // past the tip.
    let top = &hashes[tip];
    batch(&[
        one("invalidate", "invalidateblock", format!(r#"["{top}"]"#)),
        one("stats_invalidated_hash", "getblockstats", format!(r#"["{top}"]"#)),
        one("stats_invalidated_height", "getblockstats", format!("[{tip}]")),
        one("reconsider", "reconsiderblock", format!(r#"["{top}"]"#)),
        one("stats_reconsidered", "getblockstats", format!("[{tip}]")),
    ]);
    let prunes: Vec<_> =
        [("one", "[1]"), ("time", "[2000000000]"), ("str", r#"["1"]"#), ("none", "[]")]
            .into_iter()
            .map(|(name, params)| {
                one(&format!("prune_{name}"), "pruneblockchain", params.to_owned())
            })
            .collect();
    batch(&prunes);
    // Past regtest's first halving (150): both mine the same blocks under the mock time, and
    // the subsidy halves at the block whose parent is at 149.
    batch(&[one("mine_past_halving", "generatetodescriptor", r#"[50, "raw(51)"]"#.to_owned())]);
    let halving: Vec<_> = (148..=152)
        .map(|h| one(&format!("stats_halving_{h}"), "getblockstats", format!("[{h}]")))
        .collect();
    let around = batch(&halving);
    let subsidy = |i: usize| field(&around[i], "subsidy").write();
    assert_ne!(subsidy(1), subsidy(3), "the subsidy halves between 149 and 151");

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    assert!(count > 300, "{count} calls");
}

/// `desc` with the checksum dashd's `getdescriptorinfo` reports for it.
fn descsum(port: u16, desc: &str) -> String {
    let body = call("getdescriptorinfo", &format!("[{}]", UniValue::from(desc.to_owned()).write()));
    let reply = String::from_utf8(exchange(port, &body)).unwrap();
    let (_, rest) = reply.split_once(r#""checksum":""#).unwrap_or_else(|| panic!("{reply}"));
    format!("{desc}#{}", &rest[..8])
}

/// A JSON string of `s`.
fn json(s: &str) -> String {
    UniValue::from(s.to_owned()).write()
}

/// The util commands' cases (RP1a-3a): each command's help, arity and type errors, then
/// `validateaddress` on L1 and DIP-18 Platform addresses and the decoders' errors,
/// `createmultisig` with every `HexToPubKey` and `AddAndGetMultisigDestination` refusal and a
/// hybrid key's `addr()` descriptor, `getdescriptorinfo` and `deriveaddresses` on
/// rpc_getdescriptorinfo.py's and rpc_deriveaddresses.py's descriptors (key origins with `'`
/// and `h`, WIF and xprv keys, every range error), and the message commands on
/// rpc_signmessagewithprivkey.py's key. The Platform addresses come from BIP-350's reference
/// encoder (hrp `tdash`, DIP-18 type bytes); dashd's replies are the verdicts.
fn util_command_cases(core_port: u16) -> Vec<(String, Vec<u8>)> {
    const G: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    const G_FULL: &str = "0479be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";
    const G_HYBRID: &str = "0679be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";
    const K2: &str = "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
    const TPRV: &str = "tprv8ZgxMBicQKsPd7Uf69XL1XwhmjHopUGep8GuEiJDZmbQz6o58LninorQAfcKZWARbtRtfnLcJ5MQ2AtHcQJCCRUcMRvmDUjyEmNUWwx8UbK";
    const TPUB: &str = "tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B";
    const WIF: &str = "cU4zhap7nPJAWeMFu4j6jLrfPmqakDAzy8zn8Fhb3oEevdm4e5Lc";
    const WIF_ADDRESS: &str = "yeMpGzMj3rhtnz48XsfpB8itPHhHtgxLc3";
    const MESSAGE: &str = "This is just a test message";
    const SIGNATURE: &str =
        "ICzMhjIUmmXcPWy2+9nw01zQMawo+s5FIy6F7VMkL+TmIeNq1j3AMEuw075os29kh5KYLbysKkDlDD+EAqERBd4=";
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    let mut add = |name: String, method: &str, params: String| {
        cases.push((name, call(method, &params)));
    };
    for (method, arity) in [
        ("validateaddress", 1),
        ("createmultisig", 2),
        ("getdescriptorinfo", 1),
        ("deriveaddresses", 1),
        ("verifymessage", 3),
        ("signmessagewithprivkey", 2),
    ] {
        add(format!("help_{method}"), "help", format!("[{}]", json(method)));
        add(format!("{method}_none"), method, "[]".into());
        let too_many = ["\"x\""; 4].join(",");
        add(format!("{method}_too_many"), method, format!("[{too_many}]"));
        let numbers = vec!["1"; arity].join(",");
        add(format!("{method}_numbers"), method, format!("[{numbers}]"));
    }
    for (name, address) in [
        ("p2pkh", "yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM"),
        ("p2sh", "93EpXofs6W7eNiuj4gu2LJh8L8opowW1jz"),
        ("mainnet", "XuXS24zs2xP1vPynvNt14kWLa64csH8Mur"),
        ("base58_checksum", "x"),
        ("not_base58", "0"),
        ("empty", ""),
        ("platform_p2pkh", "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl"),
        ("platform_p2sh", "tdash1sqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyhkzhyn"),
        ("platform_upper", "TDASH1KQG3ZYG3ZYG3ZYG3ZYG3ZYG3ZYG3ZYG3ZY6DV7VL"),
        ("platform_mixed", "tdash1Kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl"),
        ("platform_typo", "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vm"),
        ("platform_bech32", "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy03ujfa"),
        ("platform_type", "tdash1qyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyy0jrhl"),
        ("platform_short", "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3506th7"),
        ("platform_mainnet", "dash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy22lqa3"),
        ("platform_hrp_only", "tdash1"),
    ] {
        add(format!("validateaddress_{name}"), "validateaddress", format!("[{}]", json(address)));
    }
    let keys = |ks: &[&str]| ks.iter().map(|k| json(k)).collect::<Vec<_>>().join(",");
    for (name, params) in [
        ("2_of_2", format!("[2,[{}]]", keys(&[G, G_FULL]))),
        ("1_of_1", format!("[1,[{}]]", keys(&[K2]))),
        ("hybrid", format!("[1,[{}]]", keys(&[G, G_HYBRID]))),
        ("15", format!("[3,[{}]]", keys(&[G; 15]))),
        ("16", format!("[3,[{}]]", keys(&[G; 16]))),
        ("21", format!("[1,[{}]]", keys(&[G; 21]))),
        ("zero", format!("[0,[{}]]", keys(&[G]))),
        ("negative", format!("[-1,[{}]]", keys(&[G]))),
        ("not_enough", format!("[3,[{}]]", keys(&[G, K2]))),
        ("not_hex", r#"[1,["zz"]]"#.into()),
        ("short", r#"[1,["0279"]]"#.into()),
        ("invalid", format!("[1,[{}]]", json(&format!("05{}", &G[2..])))),
        ("number_key", "[1,[1]]".into()),
        ("keys_string", r#"[1,"x"]"#.into()),
        ("required_string", format!(r#"["1",[{}]]"#, keys(&[G]))),
        ("required_fraction", format!("[1.5,[{}]]", keys(&[G]))),
        ("empty", "[1,[]]".into()),
    ] {
        add(format!("createmultisig_{name}"), "createmultisig", params);
    }
    let infos = [
        ("pk", format!("pk({G})")),
        ("pkh", format!("pkh({K2})")),
        ("sh_pkh", format!("sh(pkh({K2}))")),
        ("combo", format!("combo({G})")),
        ("combo_uncompressed", format!("combo({G_FULL})")),
        ("multi", format!("multi(1,{G},{K2})")),
        ("sortedmulti", format!("sh(sortedmulti(1,{K2},{G}))")),
        ("xpub", format!("pk({TPUB})")),
        ("xpub_path", format!("pkh({TPUB}/1h/2)")),
        ("origin_h", format!("pkh([d34db33f/44h/0h/0h]{TPUB}/1/*)")),
        ("origin_apostrophe", format!("pkh([d34db33f/44'/0'/0']{TPUB}/1/*')")),
        ("origin_mixed", format!("pkh([d34db33f/44'/0h]{TPUB}/1'/*h)")),
        ("xprv", format!("pkh({TPRV}/0'/1/*)")),
        ("wif", format!("pkh({WIF})")),
        ("addr", "addr(yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM)".into()),
        ("raw", "raw(51)".into()),
        ("sh_raw", "sh(raw(51))".into()),
        ("sh_multi_xpubs", format!("sh(multi(1,{TPUB}/1/0/*,{TPUB}/0/0/*))")),
        ("empty", String::new()),
        ("bad_function", "foo(1)".into()),
        ("hybrid", format!("pk({G_HYBRID})")),
        ("two_hashes", format!("pk({G})#a#b")),
        ("short_checksum", format!("pk({G})#abc")),
        ("wrong_checksum", format!("pk({G})#aaaaaaaa")),
        ("bad_char", "pk(\u{1})".into()),
        ("bad_fingerprint", format!("pkh([d34db33/0]{TPUB})")),
        ("two_origins", format!("pkh([d34db33f]]{TPUB})")),
    ];
    for (name, desc) in &infos {
        add(format!("getdescriptorinfo_{name}"), "getdescriptorinfo", format!("[{}]", json(desc)));
    }
    add(
        "getdescriptorinfo_checksummed".into(),
        "getdescriptorinfo",
        format!("[{}]", json(&descsum(core_port, &format!("pkh([d34db33f/44'/0h]{TPUB}/1/*)")))),
    );
    let fixed = descsum(core_port, &format!("pkh({TPRV}/1/1/0)"));
    let ranged = descsum(core_port, &format!("pkh({TPRV}/1/1/*)"));
    for (name, params) in [
        ("missing_checksum", r#"["a"]"#.to_owned()),
        ("fixed", format!("[{}]", json(&fixed))),
        ("fixed_unchecked", format!("[{}]", json(&fixed[..fixed.len() - 9]))),
        ("fixed_pub", format!("[{}]", json(&descsum(core_port, &format!("pkh({TPUB}/1/1/0)"))))),
        ("ranged_pair", format!("[{},[1,2]]", json(&ranged))),
        ("ranged_end", format!("[{},2]", json(&ranged))),
        ("ranged_null", format!("[{},null]", json(&ranged))),
        ("fixed_range", format!("[{},[0,2]]", json(&fixed))),
        ("fixed_null", format!("[{},null]", json(&fixed))),
        ("ranged_none", format!("[{}]", json(&ranged))),
        ("too_high", format!("[{},10000000000]", json(&ranged))),
        ("too_large", format!("[{},[1000000000,2000000000]]", json(&ranged))),
        ("reversed", format!("[{},[2,0]]", json(&ranged))),
        ("negative", format!("[{},[-1,0]]", json(&ranged))),
        ("range_string", format!(r#"[{},"x"]"#, json(&ranged))),
        ("range_one", format!("[{},[1]]", json(&ranged))),
        ("range_fraction", format!("[{},1.5]", json(&ranged))),
        ("range_bad_first", r#"["a","x"]"#.to_owned()),
        ("max_index", format!("[{},[2147483647,2147483647]]", json(&ranged))),
        ("combo", format!("[{}]", json(&descsum(core_port, &format!("combo({TPRV}/1/1/0)"))))),
        (
            "hardened_pub",
            format!("[{}]", json(&descsum(core_port, &format!("pkh({TPUB}/1'/1/0)")))),
        ),
        (
            "bare_multi",
            format!(
                "[{}]",
                json(&descsum(core_port, &format!("multi(1,{TPUB}/1/1/0,{TPUB}/1/1/1)")))
            ),
        ),
        ("raw", format!("[{}]", json(&descsum(core_port, "raw(51)")))),
        (
            "addr",
            format!("[{}]", json(&descsum(core_port, "addr(93EpXofs6W7eNiuj4gu2LJh8L8opowW1jz)"))),
        ),
        (
            "sh_multi",
            format!(
                "[{},[0,1]]",
                json(&descsum(core_port, &format!("sh(multi(1,{TPUB}/1/0/*,{TPUB}/0/0/*))")))
            ),
        ),
        ("wif", format!("[{}]", json(&descsum(core_port, &format!("pkh({WIF})"))))),
    ] {
        add(format!("deriveaddresses_{name}"), "deriveaddresses", params);
    }
    let p2sh = "93EpXofs6W7eNiuj4gu2LJh8L8opowW1jz";
    for (name, address, signature, message) in [
        ("ok", WIF_ADDRESS, SIGNATURE, MESSAGE),
        ("other_message", WIF_ADDRESS, SIGNATURE, "other"),
        ("short_signature", WIF_ADDRESS, "AAAA", MESSAGE),
        ("not_base64", WIF_ADDRESS, "!", MESSAGE),
        ("bad_padding", WIF_ADDRESS, "Zh==", MESSAGE),
        ("invalid_address", "x", SIGNATURE, MESSAGE),
        ("p2sh", p2sh, SIGNATURE, MESSAGE),
        ("platform", "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl", SIGNATURE, MESSAGE),
    ] {
        add(
            format!("verifymessage_{name}"),
            "verifymessage",
            format!("[{},{},{}]", json(address), json(signature), json(message)),
        );
    }
    for (name, key, message) in [
        ("ok", WIF, MESSAGE),
        ("empty_message", WIF, ""),
        ("unicode", WIF, "\u{e9}\u{1f600}"),
        ("mainnet_key", "XJvEUEcFWCHCyruc8ZX5exPZaGe4UR7gC5FHrhwPnQGDs1uWCsT2", MESSAGE),
        ("invalid_key", "x", MESSAGE),
    ] {
        add(
            format!("signmessagewithprivkey_{name}"),
            "signmessagewithprivkey",
            format!("[{},{}]", json(key), json(message)),
        );
    }
    // Edge cases from the RP1a-3a review: padding, lengths and typos in addresses, key encodings,
    // origin and hardened-marker mixes, range shapes, and malformed signatures.
    {
        let addr = "yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM";
        let plat = "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl";
        let ones = "1".repeat(101);
        let long58 = "z".repeat(120);
        let long_plat = format!("tdash1{}", "q".repeat(90));
        let two_err = "tdash1kqg3zyg3zyg3zqg3zyg3zyg3zyg3zyg3zy6dv7vm";
        let va: Vec<(String, String)> = vec![
            ("sp".into(), format!(" {addr} ")),
            ("tabnl".into(), format!("\t{addr}\n")),
            ("trail_garbage".into(), format!("{addr} x")),
            ("ones101".into(), ones.clone()),
            ("ones30".into(), format!("{}{}", "1".repeat(30), "z")),
            ("long58".into(), long58.clone()),
            ("long_plat".into(), long_plat.clone()),
            ("two_err".into(), two_err.into()),
            // Bech32 and Bech32m locate as many errors; Core keeps Bech32's (r1 m-1).
            ("tie".into(), "tdash1q3g3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl".into()),
            ("bad32".into(), "tdash1bbbbbbbbb".into()),
            ("hrp_nosep".into(), "tdash".into()),
            ("tdas".into(), "tdas".into()),
            ("nul".into(), "\u{0}".into()),
            ("tdash_nul".into(), "tdash1\u{0}qqqqqq".into()),
            ("tdash_e".into(), "tdash1\u{e9}qqqqqq".into()),
            ("plat_sp".into(), format!(" {plat}")),
            (
                "plat_bech32_upper_hrp".into(),
                "TDASH1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl".into(),
            ),
            ("sep_end".into(), "tdash1qqqqq".into()),
            ("sep0".into(), "1qqqqqqqq".into()),
            ("tdashx".into(), "tdashxyz".into()),
            ("plat_lenpad".into(), "tdash1kqg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyqqqqqqqqqq".into()),
        ];
        for (n, a) in va {
            add(format!("edge_va_{n}"), "validateaddress", format!("[{}]", json(&a)));
        }
        for (n, params) in [
            ("cm_17", format!("[17,[{}]]", keys(&[G; 17]))),
            ("cm_big_req", format!("[2147483648,[{}]]", keys(&[G]))),
            ("cm_upper", format!("[1,[{}]]", keys(&[&G.to_uppercase()]))),
            ("cm_null_key", "[1,[null]]".into()),
            ("cm_req_over_bad", format!("[5,[{},\"zz\"]]", json(G))),
            ("cm_g_full_only", format!("[1,[{}]]", keys(&[G_FULL]))),
            ("cm_odd", r#"[1,["027"]]"#.into()),
            ("cm_65_02", format!("[1,[{}]]", json(&format!("02{}", &G_FULL[2..])))),
            ("cm_33_04", format!("[1,[{}]]", json(&format!("04{}", &G[2..])))),
            ("cm_obj_key", r#"[1,[{}]]"#.into()),
            ("cm_req_exp", format!("[1e0,[{}]]", keys(&[G]))),
        ] {
            add(format!("edge_{n}"), "createmultisig", params);
        }
        let gi = [
            ("origin_const", format!("pkh([d34db33f/0']{G})")),
            ("origin_key_h", format!("pkh([d34db33f/0']{TPUB}/1h)")),
            ("star_ap", format!("pkh({TPUB}/*')")),
            ("origin_h_star_ap", format!("pkh([d34db33f/0h]{TPUB}/*')")),
            ("origin_ap_star_h_path", format!("pkh([d34db33f/0']{TPUB}/1/*h)")),
            ("origin_ap_path_h_star_ap", format!("pkh([d34db33f/0h]{TPUB}/1h/*')")),
            ("plus_path", format!("pkh({TPUB}/+1)")),
            ("plus_thres", format!("multi(+1,{G})")),
            ("zero_thres", format!("multi(01,{G})")),
            ("raw_upper", "raw(AB)".into()),
            ("addr_space", format!("addr( {})", "yZTyMdEJjZWJi6CwY6g3WurLESH3UsWrrM")),
            ("sh_pk_full", format!("sh(pk({G_FULL}))")),
            ("xprv_bare", format!("pkh({TPRV})")),
            ("combo_wif", format!("combo({WIF})")),
            ("origin_empty_path", format!("pkh([d34db33f]{TPUB})")),
            ("origin_noslash_ap", format!("pkh([d34db33f/']{TPUB})")),
            ("origin_upper_fpr", format!("pkh([D34DB33F/1]{TPUB})")),
            ("sh_multi_wif", format!("sh(multi(1,{WIF},{G}))")),
            ("trailing", format!("pk({G})x")),
            ("sorted_xpub_range", format!("sh(sortedmulti(2,{TPUB}/*,{TPUB}/1/*'))")),
            ("key_path_big", format!("pkh({TPUB}/2147483648)")),
            ("raw_odd", "raw(5)".into()),
            ("raw_empty", "raw()".into()),
            // The checksum length is checked before the payload's characters (r1 n-1).
            ("bad_char_short_checksum", "pk(\u{1})#abc".into()),
        ];
        for (n, d) in &gi {
            add(format!("edge_gi_{n}"), "getdescriptorinfo", format!("[{}]", json(d)));
        }
        let fixed = descsum(core_port, &format!("pkh({TPRV}/1/1/0)"));
        let ranged = descsum(core_port, &format!("pkh({TPRV}/1/1/*)"));
        let p2pk = descsum(core_port, &format!("raw(21{G}ac)"));
        let p2pk_full = descsum(core_port, &format!("raw(41{G_FULL}ac)"));
        let p2pk_bad = descsum(core_port, &format!("raw(2102{}ac)", "00".repeat(32)));
        let combo_full = descsum(core_port, &format!("combo({G_FULL})"));
        let combo_g = descsum(core_port, &format!("combo({G})"));
        let hard_xprv = descsum(core_port, &format!("pkh({TPRV}/*')"));
        for (n, params) in [
            ("named_null_fixed", format!(r#"{{"descriptor":{},"range":null}}"#, json(&fixed))),
            ("named_null_ranged", format!(r#"{{"descriptor":{},"range":null}}"#, json(&ranged))),
            ("named_only", format!(r#"{{"descriptor":{}}}"#, json(&ranged))),
            ("named_range_only", r#"{"range":1}"#.to_string()),
            ("range_mixed", format!(r#"[{},[0,"1"]]"#, json(&ranged))),
            ("range_bool", format!("[{},true]", json(&ranged))),
            ("range_three", format!("[{},[0,1,2]]", json(&ranged))),
            ("range_obj", format!("[{},{{}}]", json(&ranged))),
            ("range_2p31", format!("[{},2147483648]", json(&ranged))),
            ("range_rev_neg", format!("[{},[-1,-2]]", json(&ranged))),
            ("range_neg", format!("[{},-1]", json(&ranged))),
            ("range_exp", format!("[{},1e1]", json(&ranged))),
            ("range_toolarge", format!("[{},[5,1000005]]", json(&ranged))),
            ("range_neg_zero", format!("[{},-0]", json(&ranged))),
            ("range_huge", format!("[{},99999999999999999999]", json(&ranged))),
            ("range_bad_desc", r#"["zz",[1,0]]"#.to_string()),
            ("p2pk", format!("[{}]", json(&p2pk))),
            ("p2pk_full", format!("[{}]", json(&p2pk_full))),
            ("p2pk_bad", format!("[{}]", json(&p2pk_bad))),
            ("combo_full", format!("[{}]", json(&combo_full))),
            ("combo_g", format!("[{}]", json(&combo_g))),
            ("hard_xprv", format!("[{},[0,2]]", json(&hard_xprv))),
            ("checksum_upper", format!("[{}]", json(&fixed.to_uppercase()))),
            ("bad_char_no_checksum", format!("[{}]", json("pk(\u{1})"))),
        ] {
            add(format!("edge_da_{n}"), "deriveaddresses", params);
        }
        for (n, a, s, m) in [
            ("addr_sp", format!(" {WIF_ADDRESS} "), SIGNATURE.to_string(), MESSAGE.to_string()),
            ("sig_empty", WIF_ADDRESS.to_string(), String::new(), MESSAGE.to_string()),
            ("sig_sp", WIF_ADDRESS.to_string(), format!(" {SIGNATURE}"), MESSAGE.to_string()),
            ("sig_eq3", WIF_ADDRESS.to_string(), "A===".into(), MESSAGE.to_string()),
            (
                "sig_hdr0",
                WIF_ADDRESS.to_string(),
                format!("A{}", &SIGNATURE[1..]),
                MESSAGE.to_string(),
            ),
            (
                "sig_hdr_ff",
                WIF_ADDRESS.to_string(),
                format!("/{}", &SIGNATURE[1..]),
                MESSAGE.to_string(),
            ),
            (
                "sig_hdr_flip",
                WIF_ADDRESS.to_string(),
                format!("H{}", &SIGNATURE[1..]),
                MESSAGE.to_string(),
            ),
            ("sig_mid_eq", WIF_ADDRESS.to_string(), "AA=A".into(), MESSAGE.to_string()),
            ("sig_stray_bits", WIF_ADDRESS.to_string(), "AB==".into(), MESSAGE.to_string()),
            (
                "plat_upper",
                "TDASH1KQG3ZYG3ZYG3ZYG3ZYG3ZYG3ZYG3ZYG3ZY6DV7VL".into(),
                SIGNATURE.to_string(),
                MESSAGE.to_string(),
            ),
        ] {
            add(
                format!("edge_vm_{n}"),
                "verifymessage",
                format!("[{},{},{}]", json(&a), json(&s), json(&m)),
            );
        }
        for (n, k) in [("wif_sp", format!(" {WIF}\n")), ("wif_upper", WIF.to_uppercase())] {
            add(
                format!("edge_sm_{n}"),
                "signmessagewithprivkey",
                format!("[{},{}]", json(&k), json(MESSAGE)),
            );
        }
    }
    add(
        "deriveaddresses_named".into(),
        "deriveaddresses",
        format!(r#"{{"descriptor":{},"range":[0,1]}}"#, json(&ranged)),
    );
    cases
}

#[test]
fn util_commands_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let cases = util_command_cases(core_port);
    let mut failures = Vec::new();
    let mut replies = std::collections::HashMap::new();
    for (name, request) in &cases {
        let [theirs, mine] = [core_port, our_port].map(|port| exchange(port, request));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            failures.push(format!("{name}:\n  dashd  {}\n  rdashd {}", show(&theirs), show(&mine)));
        }
        replies.insert(name.clone(), String::from_utf8(theirs).unwrap());
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(
        failures.is_empty(),
        "{} of {} differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    // The cases reach the paths they name: valid answers where valid, and the Platform ones.
    let ok = |name: &str| replies[name].contains(r#""error":null"#);
    for name in [
        "validateaddress_platform_p2pkh",
        "validateaddress_platform_p2sh",
        "createmultisig_2_of_2",
        "createmultisig_hybrid",
        "getdescriptorinfo_origin_mixed",
        "deriveaddresses_ranged_pair",
        "verifymessage_ok",
        "signmessagewithprivkey_ok",
    ] {
        assert!(ok(name), "{name}: {}", replies[name]);
    }
    assert!(replies["validateaddress_platform_p2pkh"].contains(r#""isplatform":true"#));
    assert!(replies["createmultisig_hybrid"].contains(r#""descriptor":"addr("#));
    assert!(replies["verifymessage_ok"].contains(r#""result":true"#));
}

/// `reply` with its JSON-RPC body's `"warnings":"…"` emptied and `Content-Length` to match: dashd
/// rc.4 warns of its pre-release build, where rdashd, which reports its pin as a release
/// (BL-175), has nothing to say ([`IDENTITY`]). The comparison then holds rdashd's to "".
fn empty_warnings(reply: &[u8]) -> Vec<u8> {
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

#[test]
fn empty_warnings_empties_only_the_warnings_and_fixes_the_length() {
    let reply =
        b"HTTP/1.1 200 OK\r\nContent-Length: 34\r\n\r\n{\"a\":1,\"warnings\":\"x\\\"y\",\"b\":2}";
    assert_eq!(
        empty_warnings(reply),
        b"HTTP/1.1 200 OK\r\nContent-Length: 27\r\n\r\n{\"a\":1,\"warnings\":\"\",\"b\":2}"
    );
    assert_eq!(empty_warnings(b"HTTP/1.1 200 OK\r\n\r\n{}"), b"HTTP/1.1 200 OK\r\n\r\n{}");
}

/// A script as a transaction output's: its length as a `CompactSize`, then its bytes.
fn script_field(script: &str) -> String {
    let len = script.len() / 2;
    match u8::try_from(len) {
        Ok(n) if n < 0xfd => format!("{n:02x}{script}"),
        _ => format!("fd{}{script}", hex(&u16::try_from(len).unwrap().to_le_bytes())),
    }
}

/// The scripts RP1a-2b's `gettxout` cases pay to, one output each in this order: every
/// `Solver` type and `ScriptToUniv` branch (`pk()` and `raw()` keys, the multisig key counts,
/// `PUSHDATA1` keys, a 17-key bare multisig), two unspendable `OP_RETURN`s, `OP_TRUE`, and a
/// truncated push (`[error]`).
// Core: core_write.cpp:159-178 ScriptToUniv; script/standard.cpp:145-195 Solver
fn txout_scripts() -> Vec<(&'static str, String)> {
    let g = GENERATOR_KEY;
    let full = GENERATOR_KEY_FULL;
    let hash20 = "00112233445566778899aabbccddeeff00112233";
    let keys17 = format!("21{g}").repeat(17);
    vec![
        ("p2pk", format!("21{g}ac")),
        ("p2pk_full", format!("41{full}ac")),
        ("p2pkh", format!("76a914{hash20}88ac")),
        ("p2sh", format!("a914{hash20}87")),
        ("multisig", format!("5121{g}41{full}52ae")),
        ("multisig_pushdata1", format!("514c21{g}51ae")),
        ("multisig_17", format!("51{keys17}0111ae")),
        ("p2pk_hybrid", format!("4106{}ac", &full[2..])),
        ("p2pk_off_curve", format!("2102{}ac", "11".repeat(32))),
        ("nulldata", "6a04deadbeef".to_owned()),
        ("op_return_bare", "6a".to_owned()),
        ("op_true", "51".to_owned()),
        ("truncated", "76a902".to_owned()),
    ]
}

/// RP1a-2b: getblockchaininfo, getdifficulty, getdeploymentinfo, getchaintxstats, gettxout,
/// getbestchainlock, getblockheaders and getblockhashes sent to dashd and rdashd in order on
/// fresh regtest chains under one mock time, so both mine the same blocks: each reply equal byte
/// for byte with the `Date` header masked, and getblockchaininfo's `warnings` emptied
/// ([`empty_warnings`]). At genesis, then after 101 blocks, a block spending the first coinbase to
/// [`txout_scripts`] and two more, then with the tip invalidated and a fork block on its parent,
/// and last on the same nodes restarted with `-deprecatedrpc=softforks`: the reloaded chain,
/// `testdummy` through its first two windows, then a header-only tip from `submitheader`.
// Core: rpc/blockchain.cpp:251-297, 462-480, 536-576, 680-784, 1328-1411, 1546-1739, 2074-2153
#[test]
fn chain_state_commands_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    let mut replies = std::collections::HashMap::new();
    // Each case to both nodes; dashd's body, which the next cases may build on.
    let mut both = |ports: [u16; 2], name: &str, method: &str, params: &str| -> String {
        count += 1;
        let request = call(method, params);
        let [theirs, mine] = ports.map(|port| empty_warnings(&exchange(port, &request)));
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with(b"HTTP/"), "{name}: dashd answered {}", show(&theirs));
        if theirs != mine {
            failures.push(format!("{name}:\n  dashd  {}\n  rdashd {}", show(&theirs), show(&mine)));
        }
        let text = String::from_utf8(theirs).unwrap();
        let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
        replies.insert(name.to_owned(), body.clone());
        body
    };
    let ports = [core_port, our_port];
    let result = |body: &str| -> UniValue {
        let reply = UniValue::read(body.as_bytes()).unwrap();
        let UniValue::Obj(fields) = reply else {
            panic!("{body}")
        };
        fields.into_iter().find(|(k, _)| k == "result").unwrap().1
    };
    let field = |v: &UniValue, key: &str| -> UniValue {
        let UniValue::Obj(fields) = v else {
            panic!("an object: {}", v.write())
        };
        fields.iter().find(|(k, _)| k == key).unwrap().1.clone()
    };
    let text = |v: UniValue| v.get_str().unwrap().to_owned();

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    both(ports, "setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
    let commands = [
        "getblockchaininfo",
        "getdifficulty",
        "getdeploymentinfo",
        "getchaintxstats",
        "gettxout",
        "getbestchainlock",
        "getblockheaders",
        "getblockhashes",
    ];
    for command in commands {
        both(ports, &format!("help_{command}"), "help", &format!(r#"["{command}"]"#));
    }

    // Genesis only.
    let genesis = text(result(&both(ports, "getblockhash_0", "getblockhash", "[0]")));
    for (name, method, params) in [
        ("info_genesis", "getblockchaininfo", "[]".to_owned()),
        ("info_args", "getblockchaininfo", "[1]".to_owned()),
        ("difficulty_genesis", "getdifficulty", "[]".to_owned()),
        ("deployments_genesis", "getdeploymentinfo", "[]".to_owned()),
        ("txstats_genesis", "getchaintxstats", "[]".to_owned()),
        ("txstats_genesis_zero", "getchaintxstats", "[0]".to_owned()),
        ("txstats_genesis_one", "getchaintxstats", "[1]".to_owned()),
        ("chainlock_none", "getbestchainlock", "[]".to_owned()),
        ("headers_genesis", "getblockheaders", format!(r#"["{genesis}"]"#)),
        ("headers_genesis_hex", "getblockheaders", format!(r#"["{genesis}", 1, false]"#)),
        ("blockhashes", "getblockhashes", "[2000000000, 0]".to_owned()),
        ("blockhashes_no_args", "getblockhashes", "[]".to_owned()),
        ("blockhashes_fraction", "getblockhashes", "[1.5, 0]".to_owned()),
    ] {
        both(ports, name, method, &params);
    }

    // 101 blocks to `OP_TRUE`, maturing block 1's coinbase; a block spending it to every script.
    let mined = both(ports, "mine_101", "generatetodescriptor", r#"[101, "raw(51)"]"#);
    let first = text(result(&mined).get_values().unwrap()[0].clone());
    let block1 = result(&both(ports, "getblock_1", "getblock", &format!(r#"["{first}"]"#)));
    let coinbase1 = text(field(&block1, "tx").get_values().unwrap()[0].clone());
    let scripts = txout_scripts();
    let mut prev = [0u8; 32];
    for (i, b) in prev.iter_mut().rev().enumerate() {
        *b = u8::from_str_radix(&coinbase1[2 * i..2 * i + 2], 16).unwrap();
    }
    let outputs: String = scripts
        .iter()
        .map(|(name, script)| {
            let value: u64 = if name.starts_with("nulldata") || *name == "op_return_bare" {
                0
            } else {
                100_000
            };
            format!("{}{}", hex(&value.to_le_bytes()), script_field(script))
        })
        .collect();
    let spend =
        format!("0100000001{}0000000000ffffffff{:02x}{outputs}00000000", hex(&prev), scripts.len());
    let made = both(
        ports,
        "block_spend",
        "generateblock",
        &format!(r#"["{GENERATOR_ADDRESS}", ["{spend}"]]"#),
    );
    let spend_block = text(field(&result(&made), "hash"));
    let block102 =
        result(&both(ports, "getblock_102", "getblock", &format!(r#"["{spend_block}"]"#)));
    let txs = field(&block102, "tx");
    let [coinbase102, txid] = [0, 1].map(|i| text(txs.get_values().unwrap()[i].clone()));
    let tail =
        both(ports, "mine_2", "generatetoaddress", &format!(r#"[2, "{GENERATOR_ADDRESS}"]"#));
    let tip = text(result(&tail).get_values().unwrap()[1].clone());
    let block50 = text(result(&both(ports, "getblockhash_50", "getblockhash", "[50]")));

    for (i, (name, _)) in scripts.iter().enumerate() {
        both(ports, &format!("txout_{name}"), "gettxout", &format!(r#"["{txid}", {i}]"#));
    }
    let unknown = "11".repeat(32);
    for (name, params) in [
        ("txout_past_the_end", format!(r#"["{txid}", {}]"#, scripts.len())),
        ("txout_no_mempool", format!(r#"["{txid}", 2, false]"#)),
        ("txout_with_mempool", format!(r#"["{txid}", 2, true]"#)),
        ("txout_named", format!(r#"{{"txid":"{txid}","n":3}}"#)),
        ("txout_spent_coinbase", format!(r#"["{coinbase1}", 0]"#)),
        ("txout_coinbase", format!(r#"["{coinbase102}", 0]"#)),
        ("txout_unknown", format!(r#"["{unknown}", 0]"#)),
        ("txout_max_n", format!(r#"["{txid}", 4294967295]"#)),
        ("txout_negative_n", format!(r#"["{txid}", -1]"#)),
        ("txout_big_n", format!(r#"["{txid}", 4294967296]"#)),
        ("txout_fraction_n", format!(r#"["{txid}", 1.5]"#)),
        ("txout_string_n", format!(r#"["{txid}", "1"]"#)),
        ("txout_short_txid", r#"["1234", 0]"#.to_owned()),
        ("txout_non_hex_txid", format!(r#"["{}", 0]"#, "zz".repeat(32))),
        ("txout_bool_mempool", format!(r#"["{txid}", 0, 1]"#)),
        ("txout_no_n", format!(r#"["{txid}"]"#)),
    ] {
        both(ports, name, "gettxout", &params);
    }

    let at = |hash: &str, rest: &str| format!(r#"["{hash}"{rest}]"#);
    let window = |nblocks: &str, hash: &str| format!(r#"[{nblocks}, "{hash}"]"#);
    for (name, method, params) in [
        ("info_mined", "getblockchaininfo", "[]".to_owned()),
        ("difficulty_mined", "getdifficulty", "[]".to_owned()),
        ("deployments_tip", "getdeploymentinfo", "[]".to_owned()),
        ("deployments_genesis_hash", "getdeploymentinfo", at(&genesis, "")),
        ("deployments_50", "getdeploymentinfo", at(&block50, "")),
        ("deployments_named", "getdeploymentinfo", format!(r#"{{"blockhash":"{block50}"}}"#)),
        ("deployments_unknown", "getdeploymentinfo", at(&unknown, "")),
        ("deployments_short", "getdeploymentinfo", r#"["00"]"#.to_owned()),
        ("txstats", "getchaintxstats", "[]".to_owned()),
        ("txstats_10", "getchaintxstats", "[10]".to_owned()),
        ("txstats_0", "getchaintxstats", "[0]".to_owned()),
        ("txstats_103", "getchaintxstats", "[103]".to_owned()),
        ("txstats_104", "getchaintxstats", "[104]".to_owned()),
        ("txstats_negative", "getchaintxstats", "[-1]".to_owned()),
        ("txstats_fraction", "getchaintxstats", "[1.5]".to_owned()),
        ("txstats_string", "getchaintxstats", r#"["1"]"#.to_owned()),
        ("txstats_50", "getchaintxstats", window("null", &block50)),
        ("txstats_50_7", "getchaintxstats", window("7", &block50)),
        ("txstats_50_49", "getchaintxstats", window("49", &block50)),
        ("txstats_50_50", "getchaintxstats", window("50", &block50)),
        ("txstats_genesis_hash", "getchaintxstats", window("null", &genesis)),
        ("txstats_unknown", "getchaintxstats", window("1000", &unknown)),
        ("txstats_short", "getchaintxstats", window("1", "00")),
        ("txstats_named", "getchaintxstats", format!(r#"{{"blockhash":"{block50}","nblocks":3}}"#)),
        ("headers_genesis_5", "getblockheaders", at(&genesis, ", 5")),
        ("headers_genesis_all", "getblockheaders", at(&genesis, "")),
        ("headers_genesis_all_hex", "getblockheaders", at(&genesis, ", 2000, false")),
        ("headers_50_3", "getblockheaders", at(&block50, ", 3, true")),
        ("headers_tip", "getblockheaders", at(&tip, "")),
        ("headers_tip_hex", "getblockheaders", at(&tip, ", 1, false")),
        ("headers_count_0", "getblockheaders", at(&block50, ", 0")),
        ("headers_count_2001", "getblockheaders", at(&block50, ", 2001")),
        ("headers_count_negative", "getblockheaders", at(&block50, ", -5")),
        ("headers_count_fraction", "getblockheaders", at(&block50, ", 1.5")),
        ("headers_count_string", "getblockheaders", at(&block50, r#", "1""#)),
        ("headers_verbose_number", "getblockheaders", at(&block50, ", 1, 1")),
        ("headers_unknown", "getblockheaders", at(&unknown, ", 0")),
        ("headers_short", "getblockheaders", r#"["00"]"#.to_owned()),
        ("headers_named", "getblockheaders", format!(r#"{{"blockhash":"{block50}","count":2}}"#)),
        ("chainlock_mined", "getbestchainlock", "[]".to_owned()),
    ] {
        both(ports, name, method, &params);
    }

    // The tip off the active chain: invalidated, and a fork block on its parent.
    both(ports, "invalidateblock_tip", "invalidateblock", &at(&tip, ""));
    both(ports, "mine_fork", "generatetodescriptor", r#"[1, "raw(52)"]"#);
    for (name, method, params) in [
        ("info_fork", "getblockchaininfo", "[]".to_owned()),
        ("deployments_off_chain", "getdeploymentinfo", at(&tip, "")),
        ("txstats_off_chain", "getchaintxstats", window("1", &tip)),
        ("txstats_off_chain_default", "getchaintxstats", window("null", &tip)),
        ("headers_off_chain", "getblockheaders", at(&tip, ", 10")),
        ("headers_102_to_fork", "getblockheaders", at(&spend_block, "")),
        ("txout_after_fork", "gettxout", format!(r#"["{txid}", 0]"#)),
        ("txstats_fork", "getchaintxstats", "[]".to_owned()),
    ] {
        both(ports, name, method, &params);
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    // The same nodes restarted under `-deprecatedrpc=softforks` (RP1a D36): the chain they
    // reloaded, then `softforks`.
    let (core, core_port) = start(&dashd, "core", &["-disablewallet", "-deprecatedrpc=softforks"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &["-deprecatedrpc=softforks"]);
    let ports = [core_port, our_port];
    both(ports, "softforks_setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
    for (name, method, params) in [
        ("info_softforks", "getblockchaininfo", "[]".to_owned()),
        ("deployments_restarted", "getdeploymentinfo", "[]".to_owned()),
        ("txstats_restarted", "getchaintxstats", "[]".to_owned()),
        ("txout_restarted", "gettxout", format!(r#"["{txid}", 0]"#)),
        ("txout_coinbase_restarted", "gettxout", format!(r#"["{coinbase102}", 0]"#)),
        ("headers_off_chain_restarted", "getblockheaders", at(&tip, ", 1")),
    ] {
        both(ports, name, method, &params);
    }
    // `testdummy` across its first two 144-block windows: DEFINED to STARTED at 144, then
    // LOCKED_IN at 288 as every block signals; below DIP0003's height 432.
    let mut height = 104;
    for (first, last) in [(143, 145), (287, 289)] {
        for h in first..=last {
            let blocks = h - height;
            both(
                ports,
                &format!("mine_to_{h}"),
                "generatetodescriptor",
                &format!(r#"[{blocks}, "raw(51)"]"#),
            );
            height = h;
            both(ports, &format!("deployments_{h}"), "getdeploymentinfo", "[]");
            both(ports, &format!("info_softforks_{h}"), "getblockchaininfo", "[]");
        }
    }
    // A header-only tip: a block made and not submitted, then its header submitted.
    let made = both(
        ports,
        "block_unsubmitted",
        "generateblock",
        &format!(r#"["{GENERATOR_ADDRESS}", [], false]"#),
    );
    let made = result(&made);
    let header_only = text(field(&made, "hash"));
    let header_hex = text(field(&made, "hex"))[..160].to_owned();
    both(ports, "submitheader", "submitheader", &format!(r#"["{header_hex}"]"#));
    for (name, method, params) in [
        ("info_header_only", "getblockchaininfo", "[]".to_owned()),
        ("deployments_header_only", "getdeploymentinfo", at(&header_only, "")),
        ("headers_header_only", "getblockheaders", at(&header_only, "")),
        ("txstats_header_only", "getchaintxstats", window("null", &header_only)),
        ("txstats_header_only_1", "getchaintxstats", window("1", &header_only)),
    ] {
        both(ports, name, method, &params);
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    // The cases reach the paths they name.
    let ok = |name: &str| replies[name].contains(r#""error":null"#);
    for name in ["block_spend", "mine_fork", "invalidateblock_tip", "headers_off_chain"] {
        assert!(ok(name), "{name}: {}", replies[name]);
    }
    for name in ["txout_nulldata", "txout_op_return_bare", "txout_spent_coinbase", "txout_unknown"]
    {
        assert!(replies[name].contains(r#""result":null"#), "{name}: {}", replies[name]);
    }
    for (name, script_type) in [
        ("txout_p2pk", "pubkey"),
        ("txout_p2pkh", "pubkeyhash"),
        ("txout_p2sh", "scripthash"),
        ("txout_multisig", "multisig"),
        ("txout_multisig_pushdata1", "multisig"),
        ("txout_multisig_17", "multisig"),
        ("txout_op_true", "nonstandard"),
    ] {
        let found = format!(r#""type":"{script_type}""#);
        assert!(replies[name].contains(&found), "{name}: {}", replies[name]);
    }
    assert!(replies["txout_truncated"].contains("[error]"), "{}", replies["txout_truncated"]);
    assert!(replies["txout_coinbase"].contains(r#""coinbase":true"#));
    assert!(replies["info_softforks"].contains(r#""softforks":{"#));
    assert!(
        replies["info_softforks"].contains(r#""blocks":104,"#),
        "{}",
        replies["info_softforks"]
    );
    assert!(
        replies["txout_restarted"].contains(r#""confirmations":3"#),
        "{}",
        replies["txout_restarted"]
    );
    assert!(!replies["info_mined"].contains("softforks"));
    assert!(replies["txstats_50_49"].contains(r#""txrate":"#), "{}", replies["txstats_50_49"]);
    assert!(replies["info_genesis"].contains(r#""initialblockdownload":true"#));
    assert!(replies["info_mined"].contains(r#""initialblockdownload":false"#));
    assert!(replies["txstats_off_chain"].contains("Block is not in main chain"));
    // Each window's triple sees `testdummy` change state.
    let testdummy_status = |name: &str| {
        let reply = &replies[name];
        let at = reply.find(r#""testdummy":"#).unwrap_or_else(|| panic!("{name}: {reply}"));
        let status = &reply[at..][reply[at..].find(r#""status":""#).unwrap() + 10..];
        status[..status.find('"').unwrap()].to_owned()
    };
    for (first, last) in [(143, 145), (287, 289)] {
        let [a, b] = [first, last].map(|h| testdummy_status(&format!("deployments_{h}")));
        assert_ne!(a, b, "testdummy at {first} and {last}");
    }
    assert!(ok("submitheader"), "{}", replies["submitheader"]);
    assert!(replies["info_header_only"].contains(r#""blocks":289,"headers":290"#));
    assert!(replies["headers_off_chain"].contains(r#""confirmations":-1"#));
    assert_eq!(
        result(&replies["headers_genesis_all"]).get_values().unwrap().len(),
        105,
        "genesis, 104 blocks"
    );
}

/// RP1b-1b: `dumptxoutset` sent to dashd and rdashd on fresh regtest chains under one mock time,
/// so both hold the same coins: each reply equal byte for byte with the `Date` header masked and
/// dashd's datadir read as rdashd's (the two names have one length, so `Content-Length` stays),
/// and **each file equal byte for byte** (US-NODE-16, DEC-862 R1). The snapshots: genesis's
/// empty set, 101 `OP_TRUE` coinbases, a block spending one into outputs of every compression
/// form (P2PKH, P2SH, compressed and uncompressed P2PK, an uncompressed key off the curve,
/// bare multisig, `OP_RETURN` (never a coin), empty, long, zero value), and that block
/// invalidated; a stale `.incomplete` file overwritten; relative, `..` and absolute paths. The
/// errors: an existing file, the empty name (the directory itself), a directory with a slash,
/// an unopenable temporary file, the argument checks and `help`. Both debug logs carry the
/// `LOG_TIME_SECONDS` lines.
// Core: rpc/blockchain.cpp:3091-3210; compressor.cpp:51-129
#[test]
fn dumptxoutset_matches_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet", "-logthreadnames=1"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &["-logthreadnames=1"]);
    let [core_net, our_net] = ["core", "ours"].map(|n| home.path().join(n).join("regtest"));
    let (core_text, our_text) = (core_net.display().to_string(), our_net.display().to_string());
    assert_eq!(core_text.len(), our_text.len(), "the mask keeps Content-Length");
    let mut failures = Vec::new();
    let mut count = 0;
    // Each case to both nodes, `params` naming each node's own net directory as `{net}`;
    // dashd's body, read as rdashd's.
    let mut both = |name: &str, method: &str, params: &str| -> String {
        count += 1;
        let [theirs, mine] = [(core_port, &core_text), (our_port, &our_text)]
            .map(|(port, net)| exchange(port, &call(method, &params.replace("{net}", net))));
        let theirs = String::from_utf8(theirs).unwrap().replace(&core_text, &our_text);
        let show = |b: &[u8]| b.escape_ascii().to_string();
        assert!(theirs.starts_with("HTTP/"), "{name}: dashd answered {theirs}");
        if theirs.as_bytes() != mine {
            failures.push(format!(
                "{name}:\n  dashd  {}\n  rdashd {}",
                show(theirs.as_bytes()),
                show(&mine)
            ));
        }
        theirs.split_once("\r\n\r\n").unwrap().1.to_owned()
    };
    let result = |body: &str| -> UniValue {
        let reply = UniValue::read(body.as_bytes()).unwrap();
        let UniValue::Obj(fields) = reply else {
            panic!("{body}")
        };
        fields.into_iter().find(|(k, _)| k == "result").unwrap().1
    };
    let ok = |body: &str| body.contains(r#""error":null"#);
    let mut files = Vec::new();
    let mut dump = |both: &mut dyn FnMut(&str, &str, &str) -> String, name: &str, file: &str| {
        let body = both(name, "dumptxoutset", &format!(r#"["{file}"]"#));
        assert!(ok(&body), "{name}: {body}");
        files.push((name.to_owned(), file.replace("{net}/", "")));
        body
    };

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    both("setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
    both("help", "help", r#"["dumptxoutset"]"#);
    both("no_path", "dumptxoutset", "[]");
    both("two_args", "dumptxoutset", r#"["a.dat", 1]"#);
    both("number_path", "dumptxoutset", "[5]");
    both("null_path", "dumptxoutset", "[null]");
    let genesis = dump(&mut both, "genesis", "genesis.dat");
    assert!(genesis.contains(r#""coins_written":0"#), "{genesis}");

    let matured = both("mine_101", "generatetodescriptor", r#"[101, "raw(51)"]"#);
    let first = result(&matured).get_values().unwrap()[0].get_str().unwrap().to_owned();
    dump(&mut both, "coinbases", "coinbases.dat");
    let block = both("getblock_first", "getblock", &format!(r#"["{first}"]"#));
    let UniValue::Obj(fields) = result(&block) else {
        panic!("{block}")
    };
    let coinbase = fields.iter().find(|(k, _)| k == "tx").unwrap().1.get_values().unwrap()[0]
        .get_str()
        .unwrap()
        .to_owned();
    let g = GENERATOR_KEY;
    let full = GENERATOR_KEY_FULL;
    let off_curve = format!("04{}", "11".repeat(64));
    let outputs = [
        (1_000, format!("76a914{}88ac", "ab".repeat(20))),
        (2_000, format!("a914{}87", "cd".repeat(20))),
        (3_000, format!("21{g}ac")),
        (4_000, format!("41{full}ac")),
        (5_000, format!("41{off_curve}ac")),
        (6_000, format!("5121{g}41{full}52ae")),
        (0, "6a04deadbeef".to_owned()),
        (7_000, String::new()),
        (8_000, "61".repeat(252)),
        (0, "51".to_owned()),
        (123_456_789, "52".to_owned()),
    ];
    let outputs: Vec<(i64, &str)> = outputs.iter().map(|(v, s)| (*v, s.as_str())).collect();
    let spend = spend_tx(2, &[(&coinbase, 0, "")], &outputs);
    let mined =
        both("spend_block", "generateblock", &format!(r#"["{GENERATOR_ADDRESS}", ["{spend}"]]"#));
    assert!(ok(&mined), "{mined}");
    let UniValue::Obj(fields) = result(&mined) else {
        panic!("{mined}")
    };
    let spend_block = fields[0].1.get_str().unwrap().to_owned();
    dump(&mut both, "spends", "blocks/../spends.dat");
    dump(&mut both, "absolute", "{net}/absolute.dat");
    for n in [&core_net, &our_net] {
        std::fs::write(n.join("stale.dat.incomplete"), b"left by an interrupted dump").unwrap();
    }
    dump(&mut both, "stale_incomplete", "stale.dat");
    both("invalidate", "invalidateblock", &format!(r#"["{spend_block}"]"#));
    let invalidated = dump(&mut both, "invalidated", "invalidated.dat");
    assert!(invalidated.contains(r#""base_height":101"#), "{invalidated}");
    both("reconsider", "reconsiderblock", &format!(r#"["{spend_block}"]"#));
    dump(&mut both, "reconsidered", "../outside.dat");
    // A NUL ends the name for the file system: both write `a` (libstdc++'s `c_str()`).
    let nul = both("nul_name", "dumptxoutset", r#"["a\u0000b"]"#);
    assert!(ok(&nul), "{nul}");
    files.push(("nul_name".into(), "a".into()));

    both("exists", "dumptxoutset", r#"["genesis.dat"]"#);
    both("exists_absolute", "dumptxoutset", r#"["{net}/absolute.dat"]"#);
    both("empty_name", "dumptxoutset", r#"[""]"#);
    both("directory_slash", "dumptxoutset", r#"["blocks/"]"#);
    both("directory", "dumptxoutset", r#"["blocks"]"#);
    both("unopenable", "dumptxoutset", r#"["invalid/path"]"#);
    both("unopenable_absolute", "dumptxoutset", r#"["{net}/invalid/path"]"#);
    both("named", "dumptxoutset", r#"{"path":"named.dat"}"#);
    files.push(("named".into(), "named.dat".into()));

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    for (name, file) in &files {
        let read =
            |net: &Path| std::fs::read(net.join(file)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (theirs, mine) = (read(&core_net), read(&our_net));
        if theirs != mine {
            failures.push(format!(
                "{name}: files differ: {} and {} bytes",
                theirs.len(),
                mine.len()
            ));
        }
        for net in [&core_net, &our_net] {
            let mut temp = net.join(file).into_os_string();
            temp.push(".incomplete");
            assert!(!Path::new(&temp).exists(), "{name}: renamed");
        }
    }
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    let spends = std::fs::read(our_net.join("spends.dat")).unwrap();
    let coinbases = std::fs::read(our_net.join("coinbases.dat")).unwrap();
    assert!(spends.len() > coinbases.len() + 252, "the long script is a coin");
    // Every `LOG_TIME_SECONDS` line, its timestamp cut and duration masked, dashd's datadir read
    // as rdashd's: the same lines in the same order.
    let lines = |net: &Path| -> Vec<String> {
        let log = std::fs::read_to_string(debug_log(net.parent().unwrap())).unwrap();
        log.lines()
            .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest))
            .filter(|l| l.contains("] CreateUTXOSnapshot: "))
            // The thread's name without the worker's index, which each node picks.
            .map(|l| {
                let (thread, rest) = l.split_once("] ").unwrap();
                format!("{}] {rest}", thread.trim_end_matches(|c: char| c.is_ascii_digit()))
            })
            .map(|l| match l.rsplit_once(" completed (") {
                Some((title, _)) => format!("{title} completed (…s)"),
                None => l.to_owned(),
            })
            .map(|l| l.replace(&core_text, &our_text))
            .collect()
    };
    let (theirs, mine) = (lines(&core_net), lines(&our_net));
    assert_eq!(theirs, mine, "the debug logs' snapshot lines");
    assert_eq!(mine.len(), 2 * files.len(), "a started and a completed line a dump");
    let UniValue::Obj(fields) = result(&genesis) else {
        panic!("{genesis}")
    };
    let base = fields.iter().find(|(k, _)| k == "base_hash").unwrap().1.get_str().unwrap();
    let genesis = format!(
        "CreateUTXOSnapshot: writing UTXO snapshot at height 0 ({}) to file {1}/genesis.dat (via \
         {1}/genesis.dat.incomplete) started",
        base,
        our_net.display()
    );
    // After the `(mocktime: …)` both print under `setmocktime`.
    assert!(mine[0].ends_with(&format!(") [httpworker.] {genesis}")), "{}", mine[0]);
}

/// RP1b-1c: getspecialtxes, gettxoutproof and verifytxoutproof sent to dashd and rdashd, both
/// with `-txindex=0`, on fresh regtest chains under one mock time, as JSON-RPC batches, each
/// reply equal byte for byte with the `Date` header masked.
/// - **gettxoutproof by coin:** spends of OP_TRUE coinbases leave a coinbase unspent, a
///   transaction whose first unspent output is its second, one fully spent ("not yet in
///   block"), and a later spend; a set whose first txid is spent, and one spanning two blocks
///   (whatever txid finds a block, the reply is the same).
/// - **gettxoutproof by block:** every transaction, a subset, the genesis coinbase, a txid not
///   in the block, an invalidated block, a header without its block, a block stored but never
///   connected; and the argument errors in Core's order (empty, duplicates as given, bad
///   hashes, wrong types, arities, named arguments).
/// - **verifytxoutproof:** dashd's proofs, then each altered (a hash, the count, cut short,
///   extended), bad hex, and proofs of blocks off the active chain.
/// - **getspecialtxes:** the stored block's 22 txtouniv special transactions by every type, with
///   `count` and `skip` windows (past `int` too) at each verbosity; connected blocks with none;
///   the errors in Core's order; the headless block.
// Core: rpc/blockchain.cpp:2455-2558; rpc/rawtransaction.cpp:75-167; rpc/txoutproof.cpp:23-175;
// merkleblock.cpp; coins.cpp:340-353
#[test]
fn specialtxes_and_txoutproofs_match_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p", "-txindex=0"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    let replies = std::cell::RefCell::new(std::collections::HashMap::new());
    let mut batch = |calls: Vec<(String, &str, String)>| {
        count += calls.len();
        let results = diff_batch((core_port, our_port), &calls, &mut failures);
        for ((name, _, _), result) in calls.iter().zip(&results) {
            replies.borrow_mut().insert(name.clone(), result.write());
        }
        results
    };
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);
    let get = |v: &UniValue, key: &str| -> UniValue {
        let UniValue::Obj(fields) = v else {
            panic!("{}", v.write())
        };
        fields.iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("{key}")).1.clone()
    };
    let text = |v: &UniValue| v.get_str().unwrap_or_else(|_| panic!("{}", v.write())).to_owned();
    let first = |v: &UniValue| text(v.get_values().unwrap()[0]);

    // The chain: three OP_TRUE coinbases at 1 to 3, 100 blocks to maturity, the spends at 104,
    // the next spend at 105, one more block.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let mined = batch(vec![
        one("setmocktime", "setmocktime", format!("[{}]", now.as_secs())),
        one("mine_coins", "generatetodescriptor", r#"[3, "raw(51)"]"#.into()),
        one("mine_maturity", "generatetodescriptor", r#"[100, "raw(51)"]"#.into()),
        one("genesis", "getblockhash", "[0]".into()),
    ]);
    let coins: Vec<String> = mined[1].get_values().unwrap().iter().map(|v| text(v)).collect();
    let genesis = text(&mined[3]);
    let mut reads: Vec<_> = coins
        .iter()
        .enumerate()
        .map(|(i, b)| one(&format!("getblock_coin_{i}"), "getblock", format!(r#"["{b}"]"#)))
        .collect();
    reads.push(one("getblock_genesis", "getblock", format!(r#"["{genesis}"]"#)));
    let read = batch(reads);
    let cb: Vec<String> = read.iter().map(|b| first(&get(b, "tx"))).collect();
    let coin = 30_000_000_000;
    let tx_a = spend_tx(1, &[(&cb[0], 0, "")], &[(100_000_000, "51"), (200_000_000, "51")]);
    let tx_b = spend_tx(1, &[(&cb[1], 0, "")], &[(coin, "51")]);
    let (a, b) = (rpc_txid(&tx_a), rpc_txid(&tx_b));
    let tx_c = spend_tx(1, &[(&a, 0, "")], &[(50_000_000, "51")]);
    let tx_d = spend_tx(1, &[(&b, 0, "")], &[(coin - 1_000, "51")]);
    let (c, d) = (rpc_txid(&tx_c), rpc_txid(&tx_d));
    let spent = batch(vec![
        one(
            "block_spend",
            "generateblock",
            format!(r#"["raw(51)", ["{tx_a}", "{tx_b}", "{tx_c}"]]"#),
        ),
        one("block_next", "generateblock", format!(r#"["raw(51)", ["{tx_d}"]]"#)),
        one("mine_tip", "generatetodescriptor", r#"[1, "raw(51)"]"#.into()),
    ]);
    let spend_block = text(&get(&spent[0], "hash"));
    let next_block = text(&get(&spent[1], "hash"));
    let tip = first(&spent[2]);
    let blocks = batch(vec![one("getblock_spend", "getblock", format!(r#"["{spend_block}"]"#))]);
    let spend_cb = first(&get(&blocks[0], "tx"));
    let genesis_cb = cb[3].clone();

    // gettxoutproof.
    let unknown = "11".repeat(32);
    let upper_a = a.to_uppercase();
    let mut proofs = vec![];
    for (name, params) in [
        ("cb_unspent", format!(r#"[["{}"]]"#, cb[2])),
        ("second_output", format!(r#"[["{a}"]]"#)),
        ("spent", format!(r#"[["{b}"]]"#)),
        ("spent_coinbase", format!(r#"[["{}"]]"#, cb[0])),
        ("spending", format!(r#"[["{c}"]]"#)),
        ("later", format!(r#"[["{d}"]]"#)),
        ("spent_then_found", format!(r#"[["{b}", "{a}"]]"#)),
        ("two_blocks", format!(r#"[["{a}", "{d}"]]"#)),
        ("two_blocks_rev", format!(r#"[["{d}", "{a}"]]"#)),
        ("all_by_block", format!(r#"[["{a}", "{b}", "{c}", "{spend_cb}"], "{spend_block}"]"#)),
        ("subset_by_block", format!(r#"[["{c}", "{a}"], "{spend_block}"]"#)),
        ("spent_by_block", format!(r#"[["{b}"], "{spend_block}"]"#)),
        ("wrong_block", format!(r#"[["{a}"], "{genesis}"]"#)),
        ("genesis_cb", format!(r#"[["{genesis_cb}"], "{genesis}"]"#)),
        ("unknown_txid", format!(r#"[["{unknown}"]]"#)),
        ("unknown_block", format!(r#"[["{a}"], "{unknown}"]"#)),
        ("bad_block", format!(r#"[["{a}"], "abc"]"#)),
        ("nonhex_block", format!(r#"[["{a}"], "{}"]"#, "z".repeat(64))),
        ("number_block", format!(r#"[["{a}"], 5]"#)),
        ("null_block", format!(r#"[["{a}"], null]"#)),
        ("empty", "[[]]".into()),
        ("dup", format!(r#"[["{a}", "{upper_a}"]]"#)),
        ("dup_before_bad", format!(r#"[["{a}", "{a}", "abc"]]"#)),
        ("bad_before_dup", format!(r#"[["abc", "{a}", "{a}"]]"#)),
        ("short_txid", r#"[["abc"]]"#.into()),
        ("number_txid", "[[5]]".into()),
        ("string_txids", format!(r#"["{a}"]"#)),
        ("null_txids", "[null]".into()),
        ("named", format!(r#"{{"txids":["{a}"],"blockhash":"{spend_block}"}}"#)),
        ("no_args", "[]".into()),
        ("three_args", format!(r#"[["{a}"], "{spend_block}", 1]"#)),
    ] {
        proofs.push(one(&format!("proof_{name}"), "gettxoutproof", params));
    }
    for method in ["gettxoutproof", "verifytxoutproof", "getspecialtxes"] {
        proofs.push(one(&format!("help_{method}"), "help", format!(r#"["{method}"]"#)));
    }
    batch(proofs);

    // verifytxoutproof of dashd's proofs, and of altered ones.
    let proof = |name: &str| -> String {
        replies.borrow()[&format!("proof_{name}")].trim_matches('"').to_owned()
    };
    let all = proof("all_by_block");
    let flip = |s: &str, at: usize| {
        let mut b = parse_hex(s);
        b[at] ^= 1;
        hex(&b)
    };
    let mut verifies = vec![];
    for name in
        ["cb_unspent", "second_output", "spending", "later", "spent_then_found", "all_by_block"]
    {
        verifies.push((format!("verify_{name}"), proof(name)));
    }
    verifies.extend([
        ("verify_subset".to_owned(), proof("subset_by_block")),
        ("verify_genesis".to_owned(), proof("genesis_cb")),
        ("verify_upper".to_owned(), all.to_uppercase()),
        ("verify_trailing".to_owned(), format!("{all}00ff")),
        ("verify_cut".to_owned(), all[..all.len() - 2].to_owned()),
        ("verify_cut_header".to_owned(), all[..100].to_owned()),
        ("verify_flipped_hash".to_owned(), flip(&all, 85)),
        ("verify_flipped_count".to_owned(), flip(&all, 80)),
        ("verify_flipped_header".to_owned(), flip(&all, 4)),
        ("verify_flipped_flags".to_owned(), flip(&all, all.len() / 2 - 1)),
        ("verify_odd".to_owned(), format!("{all}0")),
        ("verify_nonhex".to_owned(), "zz".to_owned()),
        ("verify_empty".to_owned(), String::new()),
        ("verify_spaced".to_owned(), format!(" {all}")),
        ("verify_huge_count".to_owned(), format!("{}fe00000003", &all[..168])),
    ]);
    let mut calls: Vec<_> = verifies
        .iter()
        .map(|(name, p)| one(name, "verifytxoutproof", format!(r#"["{p}"]"#)))
        .collect();
    calls.extend([
        one("verify_number", "verifytxoutproof", "[5]".into()),
        one("verify_no_args", "verifytxoutproof", "[]".into()),
        one("verify_two_args", "verifytxoutproof", format!(r#"["{all}", 1]"#)),
        one("verify_named", "verifytxoutproof", format!(r#"{{"proof":"{all}"}}"#)),
    ]);
    batch(calls);

    // The next block off the active chain, then back.
    let later = proof("later");
    batch(vec![
        one("invalidate_next", "invalidateblock", format!(r#"["{next_block}"]"#)),
        one("off_verify", "verifytxoutproof", format!(r#"["{later}"]"#)),
        one("off_by_block", "gettxoutproof", format!(r#"[["{d}"], "{next_block}"]"#)),
        one("off_by_coin", "gettxoutproof", format!(r#"[["{d}"]]"#)),
        one("off_specialtxes", "getspecialtxes", format!(r#"["{next_block}", -1, 10, 0, 2]"#)),
        one("reconsider_next", "reconsiderblock", format!(r#"["{next_block}"]"#)),
        one("back_verify", "verifytxoutproof", format!(r#"["{later}"]"#)),
        one("tip_again", "getbestblockhash", "[]".into()),
    ]);
    assert_eq!(replies.borrow()["tip_again"], format!(r#""{tip}""#));

    // A header without its block, and a block stored but never connected, on the tip.
    let tip_height = 106;
    let time = u32::try_from(now.as_secs() + 600).unwrap();
    let mut prev = parse_hex(&tip);
    prev.reverse();
    let headless =
        mine(&prev.try_into().unwrap(), time, &[coinbase(tip_height + 1, 1)], None, true);
    let mut headless_hash = headless.hash;
    headless_hash.reverse();
    let headless_hash = hex(&headless_hash);
    let (stored, stored_hash) = unconnectable_block(&tip, tip_height + 1, time + 1);
    let stored_reply = batch(vec![
        one("submitheader", "submitheader", format!(r#"["{}"]"#, &headless.hex[..160])),
        one("headless_proof", "gettxoutproof", format!(r#"[["{a}"], "{headless_hash}"]"#)),
        one("headless_special", "getspecialtxes", format!(r#"["{headless_hash}"]"#)),
        one("submit_stored", "submitblock", format!(r#"["{stored}"]"#)),
        one("stored_ids", "getspecialtxes", format!(r#"["{stored_hash}", -1, 100]"#)),
    ]);
    let special = stored_reply[4].get_values().unwrap().to_vec();
    let (s0, s5) = (text(special[0]), text(special[5]));
    let stored_proof = batch(vec![
        one("stored_proof", "gettxoutproof", format!(r#"[["{s5}", "{s0}"], "{stored_hash}"]"#)),
        one("stored_by_coin", "gettxoutproof", format!(r#"[["{s0}"]]"#)),
    ]);
    let stored_proof = text(&stored_proof[0]);

    // getspecialtxes.
    let mut grid = vec![one("stored_verify", "verifytxoutproof", format!(r#"["{stored_proof}"]"#))];
    for v in 0..=2 {
        grid.push(one(
            &format!("special_v{v}"),
            "getspecialtxes",
            format!(r#"["{stored_hash}", -1, 30, 0, {v}]"#),
        ));
    }
    for ty in
        ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "65537", "-2"]
    {
        grid.push(one(
            &format!("special_type_{ty}"),
            "getspecialtxes",
            format!(r#"["{stored_hash}", {ty}, 30, 0, 1]"#),
        ));
    }
    for (count, skip) in [
        ("0", "0"),
        ("1", "0"),
        ("3", "2"),
        ("10", "21"),
        ("10", "22"),
        ("10", "100"),
        ("2147483647", "1"),
        ("1", "2147483647"),
        ("2147483647", "2147483647"),
        ("2147483646", "1"),
    ] {
        grid.push(one(
            &format!("special_window_{count}_{skip}"),
            "getspecialtxes",
            format!(r#"["{stored_hash}", -1, {count}, {skip}]"#),
        ));
    }
    for (name, params) in [
        ("default", format!(r#"["{stored_hash}"]"#)),
        ("nulls", format!(r#"["{stored_hash}", null, null, null, null]"#)),
        ("type_2_v2", format!(r#"["{stored_hash}", 2, 10, 0, 2]"#)),
        ("connected", format!(r#"["{spend_block}", -1, 10, 0, 2]"#)),
        ("genesis", format!(r#"["{genesis}"]"#)),
        ("negative_count", format!(r#"["{stored_hash}", 1, -1, -1, 9]"#)),
        ("negative_skip", format!(r#"["{stored_hash}", 1, 1, -1, 9]"#)),
        ("verbosity_3", format!(r#"["{stored_hash}", 1, 1, 1, 3]"#)),
        ("verbosity_negative", format!(r#"["{stored_hash}", 1, 1, 1, -1]"#)),
        ("type_string", format!(r#"["{stored_hash}", "1"]"#)),
        ("type_fraction", format!(r#"["{stored_hash}", 1.5]"#)),
        ("type_huge", format!(r#"["{stored_hash}", 2147483648]"#)),
        ("count_huge", format!(r#"["{stored_hash}", 1, 2147483648]"#)),
        ("bad_hash", r#"["abc", 1, -1]"#.into()),
        ("unknown_hash", format!(r#"["{unknown}", 1, -1]"#)),
        ("unknown_hash_ok", format!(r#"["{unknown}"]"#)),
        ("no_args", "[]".into()),
        ("six_args", format!(r#"["{stored_hash}", 1, 1, 1, 1, 1]"#)),
        ("named", format!(r#"{{"blockhash":"{stored_hash}","skip":3,"verbosity":1}}"#)),
    ] {
        grid.push(one(&format!("special_{name}"), "getspecialtxes", params));
    }
    batch(grid);

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    // The cases reach what they are for.
    let is_proof = |name: &str| replies.borrow()[name].starts_with('"');
    for name in ["cb_unspent", "second_output", "spending", "later", "spent_then_found"] {
        assert!(
            is_proof(&format!("proof_{name}")),
            "{name}: {}",
            replies.borrow()[&format!("proof_{name}")]
        );
    }
    for name in ["all_by_block", "subset_by_block", "spent_by_block", "genesis_cb", "named"] {
        assert!(is_proof(&format!("proof_{name}")), "{name}");
    }
    assert!(is_proof("off_by_block") && is_proof("stored_proof"));
    assert_eq!(replies.borrow()["verify_all_by_block"].matches(',').count(), 3);
    assert_eq!(replies.borrow()["verify_flipped_hash"], "[]");
    // The stored block's coinbase is a type-5 CbTx since CH7a-2b-2 (3452ef350), so getspecialtxes
    // lists it ahead of the fixture payloads: exactly those txids, no other.
    let special_v0 = &replies.borrow()["special_v0"];
    let listed: Vec<Vec<u8>> =
        [cbtx_coinbase(tip_height + 1)].into_iter().chain(stored_block_payloads()).collect();
    for tx in &listed {
        let mut id = txid(tx);
        id.reverse();
        assert!(special_v0.contains(&hex(&id)), "{} missing: {special_v0}", hex(&id));
    }
    assert_eq!(special_v0.matches(',').count(), listed.len() - 1, "{special_v0}");
    assert!(replies.borrow()["special_v2"].contains(r#""height":-1,"confirmations":0"#));
    assert_eq!(replies.borrow()["special_connected"], "[]");
}

/// RP1b-1a: `scantxoutset` sent to dashd and rdashd on the same regtest chain, each reply equal
/// byte for byte: `success`, `txouts`, `height`, `bestblock`, each unspent with its inferred
/// `desc`, and `total_amount`.
///
/// - **The chain:** OP_TRUE coinbases matured, then one block whose spends pay P2PK, P2PKH and
///   P2SH-P2PKH of a compressed key, P2PK and P2PKH of an uncompressed one, a P2SH and a bare
///   1-of-2 multisig, `pkh(tpub/0/i)` at 0, 3, 5, 1000 and 1001, a script twice in one
///   transaction, an OP_RETURN, and an OP_TRUE output a second block spends; then an immature
///   coinbase to `pkh(key)`. The scripts come from dashd's own `deriveaddresses`,
///   `validateaddress` and `createmultisig`, compared too.
/// - **Scans:** each descriptor shape (`raw`, `addr`, `pk`, `pkh`, `combo`, `sh(multi)`,
///   `multi`, ranged xpubs with and without an origin, a hardened xprv), the ranges (default,
///   `end`, `[begin,end]`, past the default end), mixed and duplicate scan objects in both orders
///   (the first inference of a script stays), a needle with no coin, the spent and OP_RETURN
///   outputs, a checksum, an empty array.
/// - **The rest:** idle `status` (null) and `abort` (false), the errors in Core's order (an
///   unknown or mis-cased action, no or null or non-array scan objects, invalid descriptors, a
///   hardened step from a tpub, wrong-shaped objects, the range errors), named arguments, arity
///   and `help`. Mid-scan progress and an abort that lands are unit-proven only.
// Core: rpc/blockchain.cpp:2564-2836; rpc/util.cpp:1126-1215; script/descriptor.cpp:1131-1269
#[test]
fn scantxoutset_matches_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p", "-txindex=0"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let mut failures = Vec::new();
    let mut count = 0;
    let replies = std::cell::RefCell::new(std::collections::HashMap::new());
    let mut batch = |calls: Vec<(String, &str, String)>| {
        count += calls.len();
        let results = diff_batch((core_port, our_port), &calls, &mut failures);
        for ((name, _, _), result) in calls.iter().zip(&results) {
            replies.borrow_mut().insert(name.clone(), result.write());
        }
        results
    };
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);
    let get = |v: &UniValue, key: &str| -> UniValue {
        let UniValue::Obj(fields) = v else {
            panic!("{}", v.write())
        };
        fields.iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("{key}")).1.clone()
    };
    let text = |v: &UniValue| v.get_str().unwrap_or_else(|_| panic!("{}", v.write())).to_owned();
    let first = |v: &UniValue| text(v.get_values().unwrap()[0]);

    let k1 = GENERATOR_KEY;
    let k2 = GENERATOR_KEY_FULL;
    let k3 = "03a34b99f22c790c4e36b2b3c2c35a36db06226e41c692fc82b8b56ac1c540c5bd";
    let tpub = "tpubD6NzVbkrYhZ4WaWSyoBvQwbpLkojyoTZPRsgXELWz3Popb3qkjcJyJUGLnL4qHHoQvao8ESaAstxYSnhyswJ76uZPStJRJCTKvosUCJZL5B";
    let tprv = "tprv8ZgxMBicQKsPd7Uf69XL1XwhmjHopUGep8GuEiJDZmbQz6o58LninorQAfcKZWARbtRtfnLcJ5MQ2AtHcQJCCRUcMRvmDUjyEmNUWwx8UbK";
    let ranged = format!("pkh({tpub}/0/*)");
    let origin = format!("pkh([deadbeef/44h]{tpub}/0/*)");

    // The chain's coinbases, and the scripts, from dashd's own commands.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let mined = batch(vec![
        one("setmocktime", "setmocktime", format!("[{}]", now.as_secs())),
        one("mine_coins", "generatetodescriptor", r#"[2, "raw(51)"]"#.into()),
        one("mine_maturity", "generatetodescriptor", r#"[100, "raw(51)"]"#.into()),
        one("info_ranged", "getdescriptorinfo", format!(r#"["{ranged}"]"#)),
        one(
            "combo_k1",
            "deriveaddresses",
            format!(r#"["{}"]"#, descsum(core_port, &format!("combo({k1})"))),
        ),
        one(
            "combo_k2",
            "deriveaddresses",
            format!(r#"["{}"]"#, descsum(core_port, &format!("combo({k2})"))),
        ),
        one("multisig", "createmultisig", format!(r#"[1, ["{k1}", "{k3}"]]"#)),
    ]);
    let coins: Vec<String> = mined[1].get_values().unwrap().iter().map(|v| text(v)).collect();
    let ranged_sum = text(&get(&mined[3], "descriptor"));
    let combo_k1: Vec<String> = mined[4].get_values().unwrap().iter().map(|v| text(v)).collect();
    let combo_k2 = first(&mined[5]);
    let multisig_addr = text(&get(&mined[6], "address"));
    let mut derive: Vec<_> = [0, 3, 5, 1000, 1001]
        .iter()
        .map(|i| {
            one(&format!("xpub_{i}"), "deriveaddresses", format!(r#"["{ranged_sum}", [{i}, {i}]]"#))
        })
        .collect();
    derive.extend(
        coins
            .iter()
            .enumerate()
            .map(|(i, b)| one(&format!("getblock_coin_{i}"), "getblock", format!(r#"["{b}"]"#))),
    );
    let derived = batch(derive);
    let xpub_addrs: Vec<String> = derived[..5].iter().map(first).collect();
    let cb: Vec<String> = derived[5..].iter().map(|b| first(&get(b, "tx"))).collect();
    // combo(k1): P2PK and P2PKH share an address; the third is P2SH-P2PKH.
    let addrs = [&combo_k1[1], &combo_k1[2], &combo_k2, &multisig_addr]
        .into_iter()
        .chain(&xpub_addrs)
        .cloned()
        .collect::<Vec<_>>();
    let validated = batch(
        addrs
            .iter()
            .enumerate()
            .map(|(i, a)| one(&format!("validate_{i}"), "validateaddress", format!(r#"["{a}"]"#)))
            .collect(),
    );
    let spk: Vec<String> = validated.iter().map(|v| text(&get(v, "scriptPubKey"))).collect();
    let (p2pkh_k1, p2sh_p2pkh_k1, p2pkh_k2, p2sh_multi) = (&spk[0], &spk[1], &spk[2], &spk[3]);
    let xpub_spk = &spk[4..];
    let p2pk_k1 = format!("21{k1}ac");
    let p2pk_k2 = format!("41{k2}ac");
    let bare_multi = format!("5121{k1}21{k3}52ae");

    // The spends: every shape in one block, and an OP_TRUE output a second block spends.
    let v = 1_000_000;
    let mut outputs: Vec<(i64, &str)> = vec![
        (v, &p2pk_k1),
        (v + 1, p2pkh_k1),
        (v + 2, p2sh_p2pkh_k1),
        (v + 3, &p2pk_k2),
        (v + 4, p2pkh_k2),
        (v + 5, p2sh_multi),
        (v + 6, &bare_multi),
    ];
    for (i, s) in xpub_spk.iter().enumerate() {
        outputs.push((v + 10 + i as i64, s));
    }
    outputs.extend([(v + 20, "51"), (v + 21, p2pkh_k1.as_str()), (0, "6a01ff")]);
    let tx_a = spend_tx(1, &[(&cb[0], 0, "")], &outputs);
    let a = rpc_txid(&tx_a);
    let op_true = outputs.iter().position(|(_, s)| *s == "51").unwrap() as u32;
    let tx_b = spend_tx(1, &[(&a, op_true, "")], &[(v, &p2pk_k1)]);
    let tx_c = spend_tx(1, &[(&cb[1], 0, "")], &[(v + 30, p2pkh_k1)]);
    batch(vec![
        one("block_spend", "generateblock", format!(r#"["raw(51)", ["{tx_a}", "{tx_c}"]]"#)),
        one("block_next", "generateblock", format!(r#"["raw(51)", ["{tx_b}"]]"#)),
        one("mine_immature", "generatetodescriptor", format!(r#"[1, "pkh({k1})"]"#)),
    ]);

    // The scans.
    let addr_k1 = &combo_k1[1];
    let scan = |objects: String| format!(r#"["start", {objects}]"#);
    let mut scans = vec![];
    for (name, objects) in [
        ("raw_true", r#"["raw(51)"]"#.to_owned()),
        ("raw_p2pk", format!(r#"["raw({p2pk_k1})"]"#)),
        ("raw_op_return", r#"["raw(6a01ff)"]"#.to_owned()),
        ("raw_none", r#"["raw(00)"]"#.to_owned()),
        ("addr_p2pkh", format!(r#"["addr({addr_k1})"]"#)),
        ("addr_p2sh", format!(r#"["addr({multisig_addr})"]"#)),
        ("pk_k1", format!(r#"["pk({k1})"]"#)),
        ("pk_k2", format!(r#"["pk({k2})"]"#)),
        ("pkh_k1", format!(r#"["pkh({k1})"]"#)),
        ("pkh_k2", format!(r#"["pkh({k2})"]"#)),
        ("combo_k1", format!(r#"["combo({k1})"]"#)),
        ("combo_k2", format!(r#"["combo({k2})"]"#)),
        ("sh_multi", format!(r#"["sh(multi(1,{k1},{k3}))"]"#)),
        ("bare_multi", format!(r#"["multi(1,{k1},{k3})"]"#)),
        ("no_coin", format!(r#"["pk({k3})"]"#)),
        ("xpub_default", format!(r#"["{ranged}"]"#)),
        ("xpub_end", format!(r#"[{{"desc":"{ranged}","range":1001}}]"#)),
        ("xpub_window", format!(r#"[{{"desc":"{ranged}","range":[3,1000]}}]"#)),
        ("xpub_past", format!(r#"[{{"desc":"{ranged}","range":[1001,1001]}}]"#)),
        ("xpub_small", format!(r#"[{{"desc":"{ranged}","range":4}}]"#)),
        ("xpub_null_range", format!(r#"[{{"desc":"{ranged}","range":null}}]"#)),
        ("xpub_origin", format!(r#"[{{"desc":"{origin}","range":[5,5]}}]"#)),
        ("xpub_checksum", format!(r#"["{ranged_sum}"]"#)),
        ("xprv_hardened", format!(r#"[{{"desc":"pkh({tprv}/0h/*h)","range":2}}]"#)),
        ("unranged_range", format!(r#"[{{"desc":"pkh({k1})","range":[3,9]}}]"#)),
        ("addr_then_pkh", format!(r#"["addr({addr_k1})", "pkh({k1})"]"#)),
        ("pkh_then_addr", format!(r#"["pkh({k1})", {{"desc":"addr({addr_k1})"}}]"#)),
        ("pk_then_combo", format!(r#"["pk({k1})", "combo({k1})"]"#)),
        ("dup", format!(r#"["combo({k1})", "combo({k1})", "raw({p2pk_k1})"]"#)),
        (
            "everything",
            format!(
                r#"["combo({k1})", "combo({k2})", "sh(multi(1,{k1},{k3}))", "{ranged}", "raw(51)"]"#
            ),
        ),
        ("empty", "[]".to_owned()),
    ] {
        scans.push(one(&format!("scan_{name}"), "scantxoutset", scan(objects)));
    }
    batch(scans);

    // Idle status and abort, the errors, named arguments, arity and help.
    let mut rest = vec![];
    for (name, params) in [
        ("status", r#"["status"]"#.to_owned()),
        ("status_extra", r#"["status", ["raw(51)"]]"#.to_owned()),
        ("abort", r#"["abort"]"#.to_owned()),
        ("unknown_action", r#"["stop"]"#.to_owned()),
        ("cased_action", r#"["Start", []]"#.to_owned()),
        ("empty_action", r#"[""]"#.to_owned()),
        ("number_action", "[5]".to_owned()),
        ("no_objects", r#"["start"]"#.to_owned()),
        ("null_objects", r#"["start", null]"#.to_owned()),
        ("string_objects", r#"["start", "raw(51)"]"#.to_owned()),
        ("object_objects", r#"["start", {"desc":"raw(51)"}]"#.to_owned()),
        ("bad_desc", r#"["start", ["pkh(junk)"]]"#.to_owned()),
        ("bad_func", r#"["start", ["foo(51)"]]"#.to_owned()),
        ("bad_checksum", r#"["start", ["raw(51)#00000000"]]"#.to_owned()),
        ("underivable", format!(r#"["start", ["pkh({tpub}/*h)"]]"#)),
        ("number_object", r#"["start", [5]]"#.to_owned()),
        ("array_object", r#"["start", [["raw(51)"]]]"#.to_owned()),
        ("no_desc", r#"["start", [{"range":5}]]"#.to_owned()),
        ("number_desc", r#"["start", [{"desc":5}]]"#.to_owned()),
        ("range_reversed", format!(r#"["start", [{{"desc":"{ranged}","range":[3,2]}}]]"#)),
        ("range_string", format!(r#"["start", [{{"desc":"{ranged}","range":"5"}}]]"#)),
        ("range_negative", format!(r#"["start", [{{"desc":"{ranged}","range":-1}}]]"#)),
        ("range_negative_begin", format!(r#"["start", [{{"desc":"{ranged}","range":[-1,5]}}]]"#)),
        ("range_too_high", format!(r#"["start", [{{"desc":"{ranged}","range":2147483648}}]]"#)),
        ("range_too_large", format!(r#"["start", [{{"desc":"{ranged}","range":[5,1000005]}}]]"#)),
        ("range_fraction", format!(r#"["start", [{{"desc":"{ranged}","range":1.5}}]]"#)),
        ("good_then_bad", r#"["start", ["raw(51)", "pkh(junk)"]]"#.to_owned()),
        ("named_start", r#"{"action":"start","scanobjects":["raw(6a01ff)"]}"#.to_owned()),
        ("named_status", r#"{"action":"status"}"#.to_owned()),
        ("named_unknown", r#"{"action":"start","objects":[]}"#.to_owned()),
        ("no_args", "[]".to_owned()),
        ("three_args", r#"["status", [], 1]"#.to_owned()),
    ] {
        rest.push(one(&format!("rest_{name}"), "scantxoutset", params));
    }
    rest.push(one("help", "help", r#"["scantxoutset"]"#.into()));
    rest.push(one("status_after", "scantxoutset", r#"["status"]"#.into()));
    batch(rest);

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} of {count} differ:\n{}", failures.len(), failures.join("\n"));
    // The cases reach what they are for.
    let r = replies.borrow();
    let found = |name: &str| r[&format!("scan_{name}")].matches(r#""txid""#).count();
    assert_eq!(found("raw_op_return") + found("raw_none") + found("no_coin") + found("empty"), 0);
    assert_eq!(found("pkh_k1"), 4, "{}", r["scan_pkh_k1"]);
    assert_eq!(found("combo_k1"), 7, "{}", r["scan_combo_k1"]);
    assert_eq!((found("xpub_default"), found("xpub_end"), found("xpub_past")), (4, 5, 1));
    assert_eq!(found("xpub_origin"), 1);
    assert!(r["scan_xpub_origin"].contains(r#""desc":"pkh([deadbeef/44h/0/5]"#));
    assert!(r["scan_combo_k1"].contains(r#""coinbase":true"#));
    assert!(r["scan_addr_then_pkh"].contains(r#""desc":"addr("#));
    assert!(r["scan_pkh_then_addr"].contains(r#""desc":"pkh(["#));
    assert!(found("raw_true") > 100);
    assert_eq!(r["rest_status"], "null");
    assert_eq!(r["rest_abort"], "false");
}

/// A pair of fresh regtest nodes, dashd (with `-disablewallet`) and rdashd, each with `extra`.
/// [`Pair::both`] sends one call to both and records a differing reply; [`Pair::finish`] stops
/// them and fails on any.
struct Pair {
    ports: [u16; 2],
    nodes: Vec<Running>,
    home: tempfile::TempDir,
    extra: Vec<String>,
    failures: Vec<String>,
    count: usize,
    replies: std::collections::HashMap<String, String>,
}

impl Pair {
    fn start(extra: &[&str]) -> Pair {
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
    fn restart(&mut self) {
        for node in self.nodes.drain(..) {
            node.stop_ok(libc::SIGTERM);
        }
        let extra: Vec<&str> = self.extra.iter().map(String::as_str).collect();
        (self.nodes, self.ports) = Pair::spawn(self.home.path(), &extra);
    }

    /// One call to both nodes, warnings emptied; dashd's body is kept for [`Pair::result`] and
    /// [`Pair::finish`].
    fn both(&mut self, name: &str, method: &str, params: &str) {
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
    fn mock_now(&mut self) -> std::time::Duration {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        self.both("setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
        now
    }

    /// dashd's `result` of the call `name`.
    fn result(&self, name: &str) -> UniValue {
        let UniValue::Obj(fields) = UniValue::read(self.replies[name].as_bytes()).unwrap() else {
            panic!("{}", self.replies[name])
        };
        fields.into_iter().find(|(k, _)| k == "result").unwrap().1
    }

    /// Stops both nodes, then fails on any differing reply.
    fn finish(self) -> std::collections::HashMap<String, String> {
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

/// `getnetworkhashps` and `getmininginfo` (RP1a-4) on a 600-block chain mined under one mock
/// time, so many blocks share a time and a window can span none. DIP0003, V20 and MN_RR move
/// to 5000, above the chain (rdashd's G1 guard refuses their blocks); regtest's difficulty
/// interval is 576 blocks, so `nblocks` -1 changes window at 575, 576 and 577.
#[test]
fn mining_reads_match_dashd_v24() {
    let mut pair = Pair::start(&[
        "-dip3params=5000:5000",
        "-testactivationheight=v20@5000",
        "-testactivationheight=mn_rr@5000",
    ]);
    pair.mock_now();
    for command in ["getnetworkhashps", "getmininginfo"] {
        pair.both(&format!("help_{command}"), "help", &format!(r#"["{command}"]"#));
    }
    // Genesis only, and nothing assembled yet.
    for (name, method, params) in [
        ("info_genesis", "getmininginfo", "[]"),
        ("info_args", "getmininginfo", "[1]"),
        ("hashps_genesis", "getnetworkhashps", "[]"),
        ("hashps_genesis_minus_one", "getnetworkhashps", "[-1, 0]"),
        ("hashps_genesis_past", "getnetworkhashps", "[1, 1]"),
        ("hashps_zero", "getnetworkhashps", "[0]"),
        ("hashps_minus_two", "getnetworkhashps", "[-2]"),
        ("hashps_zero_past", "getnetworkhashps", "[0, 5]"),
        ("hashps_height_minus_two", "getnetworkhashps", "[1, -2]"),
        ("hashps_string", "getnetworkhashps", r#"["1"]"#),
        ("hashps_strings", "getnetworkhashps", r#"["1", "x"]"#),
        ("hashps_string_height", "getnetworkhashps", r#"[1, "0"]"#),
        ("hashps_fraction", "getnetworkhashps", "[1.5]"),
        ("hashps_big", "getnetworkhashps", "[2147483648]"),
        ("hashps_big_height", "getnetworkhashps", "[1, -2147483649]"),
        ("hashps_bool", "getnetworkhashps", "[true]"),
        ("hashps_nulls", "getnetworkhashps", "[null, null]"),
        ("hashps_three", "getnetworkhashps", "[1, 0, 0]"),
        ("hashps_named", "getnetworkhashps", r#"{"height":0}"#),
    ] {
        pair.both(name, method, params);
    }

    pair.both("mine", "generatetodescriptor", r#"[600, "raw(51)"]"#);
    pair.both("info_mined", "getmininginfo", "[]");
    pair.both("hashps_default", "getnetworkhashps", "[]");
    pair.both("hashps_named_mined", "getnetworkhashps", r#"{"nblocks":-1,"height":300}"#);
    let mut grid = Vec::new();
    for nblocks in [-1, 1, 2, 11, 120, 575, 576, 577, 1000, 2147483647] {
        for height in [-1, 0, 1, 2, 11, 12, 300, 575, 576, 577, 599, 600, 601] {
            grid.push((nblocks, height));
        }
    }
    for (nblocks, height) in &grid {
        pair.both(
            &format!("hashps_{nblocks}_{height}"),
            "getnetworkhashps",
            &format!("[{nblocks}, {height}]"),
        );
    }
    // The tip off the active chain: the height past the new tip.
    let tip = pair.result("mine").get_values().unwrap()[599].get_str().unwrap().to_owned();
    pair.both("invalidateblock_tip", "invalidateblock", &format!(r#"["{tip}"]"#));
    pair.both("hashps_invalidated_600", "getnetworkhashps", "[1, 600]");
    pair.both("hashps_invalidated", "getnetworkhashps", "[-1]");
    pair.both("info_invalidated", "getmininginfo", "[]");
    pair.both("reconsiderblock_tip", "reconsiderblock", &format!(r#"["{tip}"]"#));
    pair.both("info_reconsidered", "getmininginfo", "[]");
    let replies = pair.finish();

    // The cases reach the paths they name.
    assert!(!replies["info_genesis"].contains("currentblock"), "{}", replies["info_genesis"]);
    assert!(
        replies["info_mined"].contains(r#""currentblocksize":"#)
            && replies["info_mined"].contains(r#""currentblocktx":0"#),
        "{}",
        replies["info_mined"]
    );
    assert!(replies["info_mined"].contains(r#""blocks":600,"#));
    assert!(replies["hashps_zero"].contains("Invalid nblocks"));
    assert!(replies["hashps_genesis_past"].contains("Block does not exist at specified height"));
    assert!(replies["hashps_invalidated_600"].contains("Block does not exist"));
    assert!(replies["hashps_genesis"].contains(r#""result":0,"#));
    let rate = |name: &str| -> f64 {
        let reply = &replies[name];
        let at = reply.find(r#""result":"#).unwrap() + 9;
        reply[at..reply[at..].find(',').unwrap() + at].parse().unwrap_or_else(|_| panic!("{reply}"))
    };
    // Some windows span no time, others a rate.
    let rates: Vec<f64> = grid
        .iter()
        .filter(|(_, h)| (1..=600).contains(h))
        .map(|(n, h)| rate(&format!("hashps_{n}_{h}")))
        .collect();
    assert!(rates.contains(&0.0) && rates.iter().any(|r| *r > 0.0), "{rates:?}");
    // -1 at 576 reads one block, as 1 does; at 575 the whole chain, as 575 does.
    assert_eq!(rate("hashps_-1_576"), rate("hashps_1_576"));
    assert_eq!(rate("hashps_-1_575"), rate("hashps_575_575"));
    assert_eq!(rate("hashps_-1_577"), rate("hashps_2_577"));
    assert!(
        ["hashps_-1_575", "hashps_-1_576", "hashps_-1_577"].iter().any(|n| rate(n) > 0.0),
        "the interval cases span no time"
    );
}

/// `getnetworkhashps` over submitted blocks whose times go back (now + 10, + 20, + 15; each above
/// its parent's median time past): a window's span is its earliest and latest time, not its
/// ends' (Opus r1 n-2). Generated chains under one mock time never go back.
#[test]
fn network_hash_ps_over_unordered_times_matches_dashd_v24() {
    let mut pair = Pair::start(&[]);
    let now = pair.mock_now().as_secs() as u32;
    pair.both("getblockhash_0", "getblockhash", "[0]");
    let mut prev = parse_hex(pair.result("getblockhash_0").get_str().unwrap());
    prev.reverse();
    let mut prev: [u8; 32] = prev.try_into().unwrap();
    for (height, offset) in [(1u8, 10), (2, 20), (3, 15)] {
        let block = mine(&prev, now + offset, &[coinbase(height, 1)], None, true);
        prev = block.hash;
        pair.both(
            &format!("submitblock_{height}"),
            "submitblock",
            &format!(r#"["{}"]"#, block.hex),
        );
    }
    for (name, params) in [
        ("hashps_1", "[1]"),
        ("hashps_2", "[2]"),
        ("hashps_2_2", "[2, 2]"),
        ("hashps_3", "[3]"),
        ("hashps_default", "[]"),
        ("hashps_minus_one", "[-1]"),
    ] {
        pair.both(name, "getnetworkhashps", params);
    }
    pair.both("info", "getmininginfo", "[]");
    let replies = pair.finish();

    for height in 1..=3 {
        let name = format!("submitblock_{height}");
        assert!(replies[&name].contains(r#""result":null"#), "{name}: {}", replies[&name]);
    }
    // Blocks 3 back to 1 span 10 seconds (10 to 20), where their ends span 5; each block adds a
    // work of 2.
    let expected = UniValue::from_f64(4.0 / 10.0).unwrap().write();
    assert!(
        replies["hashps_2"].contains(&format!(r#""result":{expected},"#)),
        "{}",
        replies["hashps_2"]
    );
}

/// `getdeploymentinfo` on header-only blocks around V20's height (BL-270, RP1a D52): from V20
/// on, Core's `GetForBlock` must rebuild such a block's EHF signals from data it does not have,
/// and fails the read. V20, MN_RR and DIP0003 at 5, the chain's tip at 3, headers to 6.
#[test]
fn deployment_info_on_header_only_blocks_matches_dashd_v24() {
    let mut pair = Pair::start(&[
        "-dip3params=5:5",
        "-testactivationheight=v20@5",
        "-testactivationheight=mn_rr@5",
    ]);
    let now = pair.mock_now();
    pair.both("mine", "generatetodescriptor", r#"[3, "raw(51)"]"#);
    let tip = pair.result("mine").get_values().unwrap()[2].get_str().unwrap().to_owned();
    let mut prev = parse_hex(&tip);
    prev.reverse();
    let mut prev: [u8; 32] = prev.try_into().unwrap();
    let mut headers = Vec::new();
    for height in 4..=6u8 {
        let block = mine(
            &prev,
            now.as_secs() as u32 + u32::from(height),
            &[coinbase(height, 1)],
            None,
            true,
        );
        prev = block.hash;
        let mut hash = block.hash;
        hash.reverse();
        headers.push(hex(&hash));
        pair.both(
            &format!("submitheader_{height}"),
            "submitheader",
            &format!(r#"["{}"]"#, &block.hex[..160]),
        );
    }
    for (height, hash) in (4..).zip(&headers) {
        pair.both(&format!("deployments_{height}"), "getdeploymentinfo", &format!(r#"["{hash}"]"#));
    }
    pair.both("deployments_tip", "getdeploymentinfo", "[]");
    pair.both("info", "getblockchaininfo", "[]");
    let replies = pair.finish();

    for height in 4..=6 {
        let name = format!("submitheader_{height}");
        assert!(replies[&name].contains(r#""error":null"#), "{name}: {}", replies[&name]);
    }
    assert!(replies["deployments_4"].contains(r#""height":4,"#), "{}", replies["deployments_4"]);
    for name in ["deployments_5", "deployments_6"] {
        assert!(replies[name].contains("failed-getehfforblock-read"), "{name}: {}", replies[name]);
    }
    assert!(replies["info"].contains(r#""blocks":3,"headers":6"#), "{}", replies["info"]);
}

/// `getmemoryinfo "stats"`'s six numbers on the node at `port`: used, free, total, locked,
/// chunks_used, chunks_free.
fn memory_stats(port: u16) -> [u64; 6] {
    let reply = exchange(port, &post(r#"{"method":"getmemoryinfo","params":[],"id":0}"#));
    let text = String::from_utf8(reply).unwrap();
    let locked = text.split_once(r#""locked":{"#).expect("a locked object").1;
    let fields = locked.split_once('}').unwrap().0;
    let keys = ["used", "free", "total", "locked", "chunks_used", "chunks_free"];
    let mut out = [0; 6];
    for (i, field) in fields.split(',').enumerate() {
        let (key, value) = field.split_once(':').unwrap();
        assert_eq!(key, format!(r#""{}""#, keys[i]), "{text}");
        out[i] = value.parse().unwrap();
    }
    out
}

/// RP1a-3b: getmemoryinfo against dashd v24's (DEC-679 design A, D48–D50).
/// - **Stats.** dashd's pool holds one chunk for its whole life: `RNGState`'s 240 bytes
///   (random.cpp:497), in one arena. rdashd has no RNG state in the pool (BL-502), so at idle
///   it has no arena and answers zeros. Once either has handled a key, its stats must be
///   dashd's less exactly that chunk: same arena, same locked bytes, nothing left allocated.
///   A secret rdashd kept, or a chunk it never freed, shows as a difference.
/// - **Keys handled:** signmessagewithprivkey (WIF), getdescriptorinfo over a WIF and an xprv
///   with hardened steps, and deriveaddresses over an xprv's hardened range.
/// - **The rest:** the mode errors and `help`, byte for byte; mallocinfo's XML root (its
///   numbers are two heaps').
/// - **A 64 KiB `RLIMIT_MEMLOCK`:** a second pair started under it: both first arenas are capped
///   at 64 KiB and lock, and rdashd's stats after a signature are dashd's less the RNG chunk.
// Core: rpc/node.cpp:922-1004; support/lockedpool.cpp
#[test]
fn getmemoryinfo_matches_dashd_v24() {
    let dashd = require_dashd();
    let home = tempfile::tempdir().unwrap();
    let credentials = ["-rpcuser=u", "-rpcpassword=p"];
    let start = |binary: &Path, name: &str, extra: &[&str]| {
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        (node(regtest_on(binary, home.path(), &datadir, &args), &datadir), port)
    };
    let (core, core_port) = start(&dashd, "core", &["-disablewallet"]);
    let (ours, our_port) = start(Path::new(RDASHD), "ours", &[]);
    let ports = (core_port, our_port);
    let mut failures = Vec::new();
    let one = |name: &str, method: &'static str, params: String| (name.to_owned(), method, params);

    // dashd's one chunk: RNGState.
    const RNG_STATE: u64 = 240;
    let idle = memory_stats(core_port);
    assert_eq!((idle[0], idle[4], idle[5]), (RNG_STATE, 1, 1), "dashd at idle: {idle:?}");
    // One arena: 256 KiB, or less where the first is capped at a lower `RLIMIT_MEMLOCK`.
    assert!((1..=256 * 1024).contains(&idle[2]), "one arena: {idle:?}");
    assert_eq!(memory_stats(our_port), [0; 6], "rdashd has no arena at idle (BL-502)");
    // rdashd's stats are dashd's without the RNGState chunk.
    let less_rng = |c: [u64; 6]| [c[0] - RNG_STATE, c[1] + RNG_STATE, c[2], c[3], c[4] - 1, c[5]];
    let check = |after: &str, failures: &mut Vec<String>| {
        let [theirs, mine] = [core_port, our_port].map(memory_stats);
        if mine != less_rng(theirs) {
            failures.push(format!("stats after {after}:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
        }
    };

    const TPRV: &str = "tprv8ZgxMBicQKsPd7Uf69XL1XwhmjHopUGep8GuEiJDZmbQz6o58LninorQAfcKZWARbtRtfnLcJ5MQ2AtHcQJCCRUcMRvmDUjyEmNUWwx8UbK";
    const WIF: &str = "cU4zhap7nPJAWeMFu4j6jLrfPmqakDAzy8zn8Fhb3oEevdm4e5Lc";
    let sign = one("sign", "signmessagewithprivkey", format!(r#"["{WIF}", "memory"]"#));
    diff_batch(ports, &[sign], &mut failures);
    check("signmessagewithprivkey", &mut failures);
    let hardened = format!("pkh({TPRV}/0'/1h/*')");
    let info = [
        one("info_wif", "getdescriptorinfo", format!(r#"["pkh({WIF})"]"#)),
        one("info_xprv", "getdescriptorinfo", format!(r#"["{hardened}"]"#)),
    ];
    let info = diff_batch(ports, &info, &mut failures);
    check("getdescriptorinfo", &mut failures);
    let text = info[1].write();
    let checksum = &text.split(r#""checksum":""#).nth(1).expect("a checksum")[..8];
    let derive =
        one("derive_hardened", "deriveaddresses", format!(r#"["{hardened}#{checksum}", [0, 20]]"#));
    let derived = diff_batch(ports, &[derive], &mut failures);
    assert_eq!(derived[0].get_values().map(|v| v.len()).unwrap_or(0), 21, "{}", derived[0].write());
    check("deriveaddresses", &mut failures);

    let calls = [
        one("null", "getmemoryinfo", "[null]".into()),
        one("unknown", "getmemoryinfo", r#"["foo"]"#.into()),
        one("empty", "getmemoryinfo", r#"[""]"#.into()),
        one("case", "getmemoryinfo", r#"["STATS"]"#.into()),
        one("number", "getmemoryinfo", "[1]".into()),
        one("object", "getmemoryinfo", "[{}]".into()),
        one("extra", "getmemoryinfo", r#"["stats", 1]"#.into()),
        one("named", "getmemoryinfo", r#"{"mode":"nope"}"#.into()),
        one("help", "help", r#"["getmemoryinfo"]"#.into()),
    ];
    // The null mode is stats: its numbers are checked above, so it is compared on its own.
    diff_batch(ports, &calls[1..], &mut failures);
    let null = [core_port, our_port].map(|port| {
        let reply = exchange(port, &post(r#"{"method":"getmemoryinfo","params":[null],"id":0}"#));
        String::from_utf8(reply).unwrap()
    });
    assert!(null.iter().all(|r| r.contains(r#""locked":{"used":"#)), "{null:?}");
    check("the errors", &mut failures);

    let mallocinfo = [core_port, our_port].map(|port| {
        let reply =
            exchange(port, &post(r#"{"method":"getmemoryinfo","params":["mallocinfo"],"id":0}"#));
        String::from_utf8(reply).unwrap()
    });
    for (node, reply) in ["dashd", "rdashd"].iter().zip(&mallocinfo) {
        assert!(reply.contains(r#""result":"<malloc version=\"1\">"#), "{node}: {reply}");
        assert!(reply.contains(r#"</malloc>"#), "{node}: {reply}");
        assert!(reply.contains(r#""error":null"#), "{node}: {reply}");
    }

    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);

    // Under a 64 KiB `RLIMIT_MEMLOCK` (systemd's and docker's default) the first arena is capped
    // at the limit, so that it locks (`lockedpool.cpp:341-350`; r1 m-1).
    const MEMLOCK: u64 = 64 * 1024;
    let capped = |binary: &Path, name: &str, extra: &[&str]| {
        use std::os::unix::process::CommandExt as _;
        let datadir = home.path().join(name);
        std::fs::create_dir_all(&datadir).unwrap();
        let port = free_port();
        let rpcport = format!("-rpcport={port}");
        let args =
            [&["-printtoconsole=0", "-listen=0", &rpcport][..], &credentials, extra].concat();
        let mut cmd = regtest_on(binary, home.path(), &datadir, &args);
        // SAFETY: setrlimit and getrlimit are async-signal-safe; the closure touches only its
        // own stack between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                limit.rlim_cur = MEMLOCK.min(limit.rlim_max);
                if libc::setrlimit(libc::RLIMIT_MEMLOCK, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
        };
        (node(cmd, &datadir), port)
    };
    let (core, core_port) = capped(&dashd, "core-64k", &["-disablewallet"]);
    let (ours, our_port) = capped(Path::new(RDASHD), "ours-64k", &[]);
    let ports = (core_port, our_port);
    let arena = MEMLOCK;
    let idle = memory_stats(core_port);
    let want = [RNG_STATE, arena - RNG_STATE, arena, arena, 1, 1];
    assert_eq!(idle, want, "dashd at idle under a 64 KiB memlock limit");
    assert_eq!(memory_stats(our_port), [0; 6], "rdashd has no arena at idle (BL-502)");
    let sign = one("sign_64k", "signmessagewithprivkey", format!(r#"["{WIF}", "memory"]"#));
    diff_batch(ports, &[sign], &mut failures);
    let [theirs, mine] = [core_port, our_port].map(memory_stats);
    assert_eq!(theirs, want, "dashd after signing under a 64 KiB memlock limit");
    if mine != less_rng(theirs) {
        failures.push(format!("stats at 64 KiB:\n  dashd  {theirs:?}\n  rdashd {mine:?}"));
    }
    core.stop_ok(libc::SIGTERM);
    ours.stop_ok(libc::SIGTERM);
    assert!(failures.is_empty(), "{} differ:\n{}", failures.len(), failures.join("\n"));
}

/// One O8 recorder run's node0 (dashd v24, `test-vectors/statedigest/regtest-v24/<name>`): its
/// `chain_args`, and its blocks after genesis as `submitblock` hex with their hashes in RPC hex,
/// the first at height 1.
struct RecordedRun {
    args: Vec<String>,
    blocks: Vec<String>,
    hashes: Vec<String>,
}

fn recorded_run(name: &str) -> RecordedRun {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/statedigest/regtest-v24")
        .join(name)
        .join("node0");
    let extract = std::fs::read_to_string(dir.join("extract.json")).unwrap();
    let UniValue::Obj(fields) = UniValue::read(extract.as_bytes()).unwrap() else {
        panic!()
    };
    let UniValue::Arr(args) = &fields.iter().find(|(k, _)| k == "chain_args").unwrap().1 else {
        panic!("chain_args")
    };
    let args = args.iter().map(|a| a.get_str().unwrap().to_owned()).collect();
    let raw = zstd::decode_all(std::fs::File::open(dir.join("blocks.dat.zst")).unwrap()).unwrap();
    // blk records: the network's magic, the size, the block.
    let (mut rest, mut blocks, mut hashes) = (raw.as_slice(), Vec::new(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        let size = u32::from_le_bytes(record[4..].try_into().unwrap()) as usize;
        let (block, tail) = tail.split_at(size);
        let decoded = dash_consensus::primitives::decode_block(&mut &block[..]).expect("a block");
        hashes.push(decoded.into_value().hash().to_string());
        blocks.push(hex(block));
        rest = tail;
    }
    blocks.remove(0);
    hashes.remove(0);
    RecordedRun {
        args,
        blocks,
        hashes,
    }
}

impl RecordedRun {
    fn args(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }

    fn tip(&self) -> usize {
        self.blocks.len()
    }

    /// The block at `height`'s hash, in RPC hex.
    fn hash(&self, height: usize) -> &str {
        &self.hashes[height - 1]
    }

    /// The run's asset unlocks: (index, block height, transaction hex), in block order.
    fn asset_unlocks(&self) -> Vec<(u64, usize, String)> {
        let mut out = Vec::new();
        for (i, block) in self.blocks.iter().enumerate() {
            let bytes = parse_hex(block);
            let block = dash_consensus::primitives::decode_block(&mut bytes.as_slice()).unwrap();
            for tx in block.into_value().txs() {
                let Some(Ok(decoded)) = tx.payload() else {
                    continue;
                };
                if let dash_primitives::SpecialPayload::AssetUnlock(unlock) = &decoded.payload {
                    let raw = dash_consensus::CoreSerialize::core_serialize(tx.raw());
                    out.push((unlock.index, i + 1, hex(&raw)));
                }
            }
        }
        out
    }
}

/// Submits the run's blocks at `heights` to both nodes in batches; dashd must connect each.
fn replay(pair: &mut Pair, run: &RecordedRun, heights: std::ops::RangeInclusive<usize>) {
    let calls: Vec<_> = heights
        .map(|h| {
            (format!("submitblock_{h}"), "submitblock", format!(r#"["{}"]"#, run.blocks[h - 1]))
        })
        .collect();
    let results = diff_chunks(pair, &calls);
    assert!(results.iter().all(|r| *r == UniValue::Null), "dashd connected {}", calls[0].0);
}

/// Both nodes' tip is the run's block at `height`.
fn assert_tips(pair: &Pair, run: &RecordedRun, height: usize) {
    for port in pair.ports {
        let best = field(&reply_on(port, "getbestblockhash", "[]"), "result");
        assert_eq!(best.get_str().unwrap(), run.hash(height), "port {port}");
        let count = field(&reply_on(port, "getblockcount", "[]"), "result");
        assert_eq!(count.write(), height.to_string(), "port {port}");
    }
}

/// `getcreditpoolinfo` (RP1a-5, US-PLAT-04) at every height of the recorded `feature_asset_locks`
/// run, submitted to both nodes: V20 at 100 (the regtest window is 100 blocks, so the window
/// start is -1 below 100 and genesis at 100), asset locks, unlocks from 189 in and out of the
/// window, and V24 from 750 on its MNHF signal (DEC-1028), whose limit is relative to the window
/// start's balance. Then its parameter errors, and the tip moved back and forth.
#[test]
fn getcreditpoolinfo_matches_dashd_v24() {
    let run = recorded_run("feature_asset_locks");
    let mut pair = Pair::start(&run.args());
    pair.both("help", "help", r#"["getcreditpoolinfo"]"#);
    for (name, params) in [("genesis_default", "[]"), ("genesis_0", "[0]"), ("genesis_1", "[1]")] {
        pair.both(name, "getcreditpoolinfo", params);
    }
    // In batches: each `exchange` waits `CLOSE_WATCH` on each node, so one call per block would
    // take most of an hour.
    replay(&mut pair, &run, 1..=run.tip());
    let ports = (pair.ports[0], pair.ports[1]);
    let tip = run.tip();
    let calls: Vec<_> =
        (0..=tip).map(|h| (format!("pool_{h}"), "getcreditpoolinfo", format!("[{h}]"))).collect();
    pair.count += calls.len();
    let pools: Vec<String> =
        diff_batch(ports, &calls, &mut pair.failures).iter().map(UniValue::write).collect();
    assert_eq!(pools.len(), tip + 1);
    for (name, params) in [
        ("default", "[]"),
        ("null", "[null]"),
        ("past_tip", &*format!("[{}]", tip + 1)),
        ("minus_one", "[-1]"),
        ("max", "[2147483647]"),
        ("big", "[2147483648]"),
        ("big_negative", "[-2147483649]"),
        ("string", r#"["5"]"#),
        ("fraction", "[1.5]"),
        ("bool", "[true]"),
        ("two", "[5, 1]"),
        ("named", r#"{"height":5}"#),
    ] {
        pair.both(&format!("arg_{name}"), "getcreditpoolinfo", params);
    }
    let UniValue::Obj(fields) = UniValue::read(pools[tip].as_bytes()).unwrap() else {
        panic!("{}", pools[tip])
    };
    let tip_hash = fields.iter().find(|(k, _)| k == "blockhash").unwrap().1.get_str().unwrap();
    let tip_hash = tip_hash.to_owned();
    pair.both("invalidate", "invalidateblock", &format!(r#"["{tip_hash}"]"#));
    for (name, params) in [
        ("default", "[]"),
        ("old_tip", &*format!("[{tip}]")),
        ("new_tip", &*format!("[{}]", tip - 1)),
    ] {
        pair.both(&format!("invalidated_{name}"), "getcreditpoolinfo", params);
    }
    pair.both("reconsider", "reconsiderblock", &format!(r#"["{tip_hash}"]"#));
    pair.both("reconsidered", "getcreditpoolinfo", "[]");
    pair.finish();

    // What the run covers, read from dashd's replies: a funded pool, unlocks inside the window,
    // the window start's boundary, the pre-WITHDRAWALS windowed limit (0 at 500), WITHDRAWALS'
    // 2000 DASH cap at 749, and V24's limit at 750, which that cap would not give.
    let pool = |h: usize| &pools[h];
    assert!(pool(99).contains(r#""window":{"blocks":100,"height":-1,"#), "{}", pool(99));
    assert!(pool(100).contains(r#""window":{"blocks":100,"height":0,"#), "{}", pool(100));
    assert!(pool(189).contains(r#""unlocked":1.00000000"#), "{}", pool(189));
    assert!(pool(500).contains(r#""currentlimit":0.00000000,"#), "{}", pool(500));
    assert!(pool(500).contains(r#""unlocked":994.24772887"#), "{}", pool(500));
    assert!(pool(749).contains(r#""currentlimit":2000.00000000"#), "{}", pool(749));
    assert!(pool(750).contains(r#""currentlimit":2107.54994300"#), "{}", pool(750));
    assert!(pool(tip).contains(r#""balance":5264.45976389"#), "{}", pool(tip));
}

/// A version-3 transaction of `tx_type` with no inputs or outputs and `payload`, as hex.
fn special_tx(tx_type: u16, payload: &str) -> String {
    let version = (u32::from(tx_type) << 16) | 3;
    let len = payload.len() / 2;
    let size = if len < 0xfd {
        format!("{len:02x}")
    } else {
        format!("fd{}", hex(&u16::try_from(len).unwrap().to_le_bytes()))
    };
    format!("{}000000000000{size}{payload}", hex(&version.to_le_bytes()))
}

/// The four kinds of 96 bytes a BLS signature field can hold, as hex: zeros, the basic and the
/// legacy encoding of one signature, and bytes canonical in neither scheme.
fn bls_sig_kinds() -> [(&'static str, String); 4] {
    use dash_pkc::bls::{BlsCtx, BlsGlobalSig, BlsScIetf, BlsSchemeKind, BlsSecretKey};
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).expect("a key");
    let basic = key.sign_message(b"rp1c").to_bytes();
    let legacy =
        BlsGlobalSig::decode(&basic).unwrap().get(BlsCtx::BASIC).to_bytes(BlsSchemeKind::Legacy);
    assert!(BlsGlobalSig::decode(&[0xff; 96]).is_err(), "canonical in neither");
    [
        ("zeros", hex(&[0; 96])),
        ("basic", hex(&basic)),
        ("legacy", hex(&legacy)),
        ("neither", hex(&[0xff; 96])),
    ]
}

/// `decoderawtransaction` calls of one hand-built payload per BLS field and byte kind: a v3 CbTx's
/// `bestCLSignature`, an asset unlock's `quorumSig` and an MnHF signal's `sig`.
fn bls_field_calls(at: &str) -> Vec<(String, &'static str, String)> {
    let mut calls = Vec::new();
    for (kind, sig) in bls_sig_kinds() {
        let hash = "11".repeat(32);
        let cbtx = format!("0300{}{hash}{hash}05{sig}{}", "02000000", "00e1f50500000000");
        let unlock = format!("01{}{}{}{hash}{sig}", "07".repeat(8), "08".repeat(4), "09".repeat(4));
        let signal = format!("0105{hash}{sig}");
        for (field, tx) in [
            ("cbtx", special_tx(5, &cbtx)),
            ("unlock", special_tx(9, &unlock)),
            ("mnhf", special_tx(7, &signal)),
        ] {
            calls.push((
                format!("{at}_{field}_{kind}"),
                "decoderawtransaction",
                format!(r#"["{tx}"]"#),
            ));
        }
    }
    calls
}

/// The regtest rows of dashd-rpc's txtouniv fixtures (crafted ones included) whose
/// `-deprecatedrpc=service` setting is `service`: each bucket and transaction hex.
fn txtouniv_rows(service: bool) -> Vec<(String, String)> {
    use std::io::BufRead as _;
    let mut rows = Vec::new();
    for name in ["decoderawtransaction-v24.0.0-rc.4", "decoderawtransaction-crafted-v24.0.0-rc.4"] {
        let path = format!(
            "{}/../dashd-rpc/tests/data/txtouniv/{name}.jsonl.gz",
            env!("CARGO_MANIFEST_DIR")
        );
        let file = std::fs::File::open(path).expect("the txtouniv fixture");
        for line in std::io::BufReader::new(flate2::read::GzDecoder::new(file)).lines() {
            let UniValue::Obj(row) = UniValue::read(line.unwrap().as_bytes()).unwrap() else {
                panic!("a fixture row")
            };
            let field = |k: &str| row.iter().find(|(name, _)| name == k).unwrap().1.clone();
            if field("net").get_str().unwrap() == "regtest"
                && field("service").get_bool().unwrap() == service
            {
                let bucket = field("bucket").get_str().unwrap().to_owned();
                rows.push((bucket, field("hex").get_str().unwrap().to_owned()));
            }
        }
    }
    rows
}

/// An Evo ProRegTx and ProUpServTx of `rows` (version 3, extended network info) with that info
/// edited: only the core purpose, no `platform_https`, and an `ExtNetInfo` version Core does not
/// know. Each is named by its bucket and edit.
fn evo_net_info_edits(rows: &[(String, String)]) -> Vec<(String, String)> {
    use dash_primitives::{NIPurpose, NetInfo, ProRegTx, ProUpServTx, Transaction};
    use dash_types::codec::BaseCodec as _;
    type Edit = fn(&mut NetInfo);
    let edits: [(&str, Edit); 3] = [
        ("core_only", |ni| {
            if let NetInfo::Extended(v2) = ni {
                v2.entries.retain(|(p, _)| *p == NIPurpose::CoreP2p);
            }
        }),
        ("no_https", |ni| {
            if let NetInfo::Extended(v2) = ni {
                v2.entries.retain(|(p, _)| *p != NIPurpose::PlatformHttps);
            }
        }),
        ("unknown_version", |ni| {
            if let NetInfo::Extended(v2) = ni {
                v2.version = 2;
                v2.entries.clear();
            }
        }),
    ];
    let row = |prefix: &str| {
        let (bucket, tx) = rows.iter().find(|(b, _)| b.starts_with(prefix)).expect(prefix);
        (bucket.clone(), Transaction::decode(&mut parse_hex(tx).as_slice()).unwrap())
    };
    let (serv_bucket, serv_tx) = row("t2/pv3/mn1");
    let serv = ProUpServTx::decode(&mut serv_tx.extra_payload.as_slice()).unwrap();
    // The registration row announces no address, so it takes the service update's entries.
    let (reg_bucket, reg_tx) = row("t1/pv3/mn1");
    let mut reg = ProRegTx::decode(&mut reg_tx.extra_payload.as_slice()).unwrap();
    reg.net_info = serv.net_info.clone();
    let mut out = Vec::new();
    for (name, edit) in edits {
        let mut payloads = Vec::new();
        let mut reg = reg.clone();
        edit(&mut reg.net_info);
        let mut bytes = Vec::new();
        reg.encode(&mut bytes);
        payloads.push((&reg_bucket, &reg_tx, bytes));
        let mut serv = serv.clone();
        edit(&mut serv.net_info);
        let mut bytes = Vec::new();
        serv.encode(&mut bytes);
        payloads.push((&serv_bucket, &serv_tx, bytes));
        for (bucket, tx, payload) in payloads {
            let mut tx = tx.clone();
            tx.extra_payload = payload;
            let mut bytes = Vec::new();
            tx.encode(&mut bytes);
            out.push((format!("{bucket}/{name}"), hex(&bytes)));
        }
    }
    out
}

/// [`diff_batch`] in chunks of 25 calls, counted on `pair`.
fn diff_chunks(pair: &mut Pair, calls: &[(String, &str, String)]) -> Vec<UniValue> {
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
fn both_internal_bug(pair: &mut Pair, name: &str, method: &str, params: &str) {
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

/// RP1c's raw-transaction utilities: `decoderawtransaction`, `decodescript` and
/// `createrawtransaction` against dashd v24 on the same arguments (PLAN §6).
///
/// - **decoderawtransaction:** every transaction of the recorded `feature_asset_locks` run and
///   every regtest txtouniv fixture row under its `-deprecatedrpc=service` setting (all twelve
///   payload types); hand-built payloads: unknown provider versions (default objects, the
///   `netInfo` check, a byte left over), a version-5 CbTx, inner truncation, a trailing payload
///   byte, unknown and payload-less types; decode failures; and the three process-flag BLS
///   fields with each kind of bytes at the last tip before V19's flip, the first after it, and
///   with that block disconnected.
/// - **decodescript:** every distinct script of the run, each `Solver` type, bare multisig 1-of-1
///   to 3-of-3, empty and malformed scripts.
/// - **createrawtransaction:** every `ConstructTransaction` branch and each argument's JSON
///   types, and its transactions decoded back.
// Core: rpc/rawtransaction.cpp:905-1009; rpc/rawtransaction_util.cpp:24-143
#[test]
fn raw_transaction_utilities_match_dashd_v24() {
    let run = recorded_run("feature_asset_locks");
    let blocks = run.blocks.as_slice();
    let mut pair = Pair::start(&run.args());
    for method in ["decoderawtransaction", "decodescript", "createrawtransaction"] {
        pair.both(&format!("help_{method}"), "help", &format!(r#"["{method}"]"#));
        pair.both(&format!("{method}_no_args"), method, "[]");
        pair.both(&format!("{method}_extra_arg"), method, r#"["00", "00", 0, 1]"#);
    }
    pair.both("decoderaw_named", "decoderawtransaction", r#"{"hexstring":"00"}"#);
    pair.both("decodescript_named", "decodescript", r#"{"hexstring":"51"}"#);
    pair.both("create_named", "createrawtransaction", r#"{"inputs":[],"outputs":{},"locktime":7}"#);

    // A node started on an empty chain keeps Core's flag legacy until it connects a block past
    // genesis, even where V19 is active at height 1 (RP1a D68, which both nodes follow); the
    // run's first block puts both on the height rule.
    pair.both("submit_1", "submitblock", &format!(r#"["{}"]"#, blocks[0]));

    // The run's transactions and their distinct scripts.
    let mut txs = Vec::new();
    let mut scripts = std::collections::BTreeSet::new();
    for block in blocks {
        let bytes = parse_hex(block);
        let block = dash_consensus::primitives::decode_block(&mut bytes.as_slice()).unwrap();
        for tx in block.into_value().txs() {
            txs.push(hex(&dash_consensus::CoreSerialize::core_serialize(tx.raw())));
            for input in &tx.raw().inputs {
                scripts.insert(hex(input.script_sig.as_bytes()));
            }
            for output in &tx.raw().outputs {
                scripts.insert(hex(output.script_pubkey.as_bytes()));
            }
        }
    }
    let calls: Vec<_> = txs
        .iter()
        .enumerate()
        .map(|(i, tx)| (format!("run_tx_{i}"), "decoderawtransaction", format!(r#"["{tx}"]"#)))
        .collect();
    let decoded = diff_chunks(&mut pair, &calls);
    let types: std::collections::BTreeSet<i64> = decoded
        .iter()
        .map(|d| match d {
            UniValue::Obj(f) => {
                f.iter().find(|(k, _)| k == "type").unwrap().1.write().parse().unwrap()
            }
            _ => panic!(),
        })
        .collect();
    assert!(types.is_superset(&[0, 5, 6, 8, 9].into()), "the run's types: {types:?}");

    // Every regtest fixture row with the service fields off; the on rows run on the second pair.
    let rows = txtouniv_rows(false);
    let calls: Vec<_> = rows
        .iter()
        .map(|(bucket, tx)| {
            (format!("fixture_{bucket}"), "decoderawtransaction", format!(r#"["{tx}"]"#))
        })
        .collect();
    diff_chunks(&mut pair, &calls);
    let fixture_types: std::collections::BTreeSet<&str> =
        rows.iter().filter_map(|(b, _)| b.split('/').find(|p| p.starts_with('t'))).collect();
    assert_eq!(fixture_types.len(), 12, "every payload type: {fixture_types:?}");

    // The Evo network-info edits print whole without the deprecated ports.
    let net_edits = evo_net_info_edits(&txtouniv_rows(true));
    let calls: Vec<_> = net_edits
        .iter()
        .map(|(name, tx)| {
            (format!("netinfo_{name}"), "decoderawtransaction", format!(r#"["{tx}"]"#))
        })
        .collect();
    diff_chunks(&mut pair, &calls);

    // Hand-built payloads (§3.3).
    let hash = "11".repeat(32);
    let cbtx_v5 =
        format!("0500{}{hash}{hash}00{}{}{hash}", "02000000", "00".repeat(96), "00e1f50500000000");
    let rev_v1 = format!("0100{hash}0000{hash}{}", "00".repeat(96));
    let mut payload_cases = vec![
        ("rev_ffff", special_tx(4, "ffff")),
        ("rev_0000", special_tx(4, "0000")),
        ("rev_v4", special_tx(4, "0400")),
        ("reg_v4", special_tx(3, "0400")),
        ("reg_0000", special_tx(3, "0000")),
        ("cbtx_v5", special_tx(5, &cbtx_v5)),
        ("rev_v1", special_tx(4, &rev_v1)),
        ("rev_v1_truncated", special_tx(4, &rev_v1[..rev_v1.len() - 2])),
        ("rev_v1_trailing", special_tx(4, &format!("{rev_v1}00"))),
        ("cbtx_v5_trailing", special_tx(5, &format!("{cbtx_v5}00"))),
        ("unknown_type_13", special_tx(13, "0102")),
        ("unknown_type_100", special_tx(100, "")),
        ("rev_empty", special_tx(4, "")),
        ("cbtx_empty", special_tx(5, "")),
        ("qc_empty", special_tx(6, "")),
        ("qc_truncated", special_tx(6, "0100")),
    ];
    for (t, name) in [(1, "reg_ffff00"), (2, "serv_ffff00"), (3, "upreg_ffff00"), (4, "rev_ffff00")]
    {
        payload_cases.push((name, special_tx(t, "ffff00")));
    }
    let calls: Vec<_> = payload_cases
        .iter()
        .map(|(name, tx)| {
            (format!("payload_{name}"), "decoderawtransaction", format!(r#"["{tx}"]"#))
        })
        .collect();
    diff_chunks(&mut pair, &calls);
    for (t, payload) in [(1, "ffff"), (1, "0000"), (2, "ffff"), (2, "0400")] {
        let tx = special_tx(t, payload);
        both_internal_bug(
            &mut pair,
            &format!("netinfo_t{t}_{payload}"),
            "decoderawtransaction",
            &format!(r#"["{tx}"]"#),
        );
    }

    // Decode failures and unusual transactions (§3.1).
    let empty_v1 = "01000000000000000000";
    let negative_out = format!("010000000001{}00{}", "ffffffffffffffff", "00000000");
    let decode_cases = [
        ("empty_string", r#"[""]"#.to_owned()),
        ("odd", r#"["0"]"#.to_owned()),
        ("non_hex", r#"["zz"]"#.to_owned()),
        ("number", "[1]".to_owned()),
        ("null", "[null]".to_owned()),
        ("zero_in_out", format!(r#"["{empty_v1}"]"#)),
        ("negative_output", format!(r#"["{negative_out}"]"#)),
        ("trailing_byte", format!(r#"["{empty_v1}00"]"#)),
        ("truncated", format!(r#"["{}"]"#, &empty_v1[..16])),
        ("nonminimal_size", r#"["01000000fd000000000000"]"#.to_owned()),
        (
            "payload_short",
            format!(r#"["{}"]"#, &special_tx(4, "ffff")[..special_tx(4, "ffff").len() - 2]),
        ),
        ("uppercase", format!(r#"["{}"]"#, special_tx(4, "FFFF"))),
    ];
    let calls: Vec<_> = decode_cases
        .iter()
        .map(|(name, params)| (format!("decode_{name}"), "decoderawtransaction", params.clone()))
        .collect();
    diff_chunks(&mut pair, &calls);

    // decodescript (§4).
    let g = GENERATOR_KEY;
    let mut script_cases: Vec<(String, String)> =
        scripts.iter().enumerate().map(|(i, s)| (format!("run_script_{i}"), s.clone())).collect();
    script_cases.extend(txout_scripts().into_iter().map(|(n, s)| (n.to_owned(), s)));
    for (m, n) in [(1, 1), (1, 2), (2, 2), (1, 3), (2, 3), (3, 3)] {
        let keys = format!("21{g}").repeat(n);
        script_cases.push((
            format!("multisig_{m}_of_{n}"),
            format!("{:02x}{keys}{:02x}ae", 0x50 + m, 0x50 + n),
        ));
    }
    script_cases.extend(
        [
            ("empty", String::new()),
            ("push_only", "0102030405".to_owned()),
            ("bad_push", "4c05ff".to_owned()),
            ("p2sh_of_p2sh", format!("a914{}87", "22".repeat(20))),
            ("uppercase", "A914".to_owned() + &"22".repeat(20) + "87"),
        ]
        .map(|(n, s)| (n.to_owned(), s)),
    );
    let mut calls: Vec<_> = script_cases
        .iter()
        .map(|(name, s)| (format!("script_{name}"), "decodescript", format!(r#"["{s}"]"#)))
        .collect();
    for (name, params) in [
        ("odd", r#"["0"]"#),
        ("non_hex", r#"["0g"]"#),
        ("number", "[51]"),
        ("null", "[null]"),
        ("array", "[[]]"),
    ] {
        calls.push((format!("script_arg_{name}"), "decodescript", params.to_owned()));
    }
    diff_chunks(&mut pair, &calls);

    // createrawtransaction (§5).
    let txid = "01".repeat(32);
    let address = GENERATOR_ADDRESS;
    let other = "yNpezfFDfoikDuT1f4iK75AiLp2YLPsGAb";
    let input = |extra: &str| format!(r#"[{{"txid":"{txid}"{extra}}}]"#);
    let mut create_cases: Vec<(String, String)> = Vec::new();
    let mut case = |name: &str, params: String| create_cases.push((name.to_owned(), params));
    for (name, v) in [
        ("null", "null"),
        ("num", "1"),
        ("str", "\"x\""),
        ("bool", "true"),
        ("obj", "{}"),
        ("arr", "[]"),
    ] {
        case(&format!("inputs_{name}"), format!("[{v}, {{}}]"));
        case(&format!("outputs_{name}"), format!("[[], {v}]"));
        case(&format!("locktime_{name}"), format!("[[], {{}}, {v}]"));
        case(&format!("input_elem_{name}"), format!("[[{v}], {{}}]"));
        case(&format!("vout_{name}"), format!("[{}, {{}}]", input(&format!(r#","vout":{v}"#))));
        case(&format!("txid_{name}"), format!(r#"[[{{"txid":{v},"vout":0}}], {{}}]"#));
        for lock in [0, 1] {
            case(
                &format!("sequence_{name}_lock{lock}"),
                format!("[{}, {{}}, {lock}]", input(&format!(r#","vout":0,"sequence":{v}"#))),
            );
        }
        case(&format!("data_{name}"), format!(r#"[[], {{"data":{v}}}]"#));
        case(&format!("amount_{name}"), format!(r#"[[], {{"{address}":{v}}}]"#));
        case(&format!("pair_{name}"), format!("[[], [{v}]]"));
    }
    for lock in ["0", "1", "4294967295", "4294967296", "-1", "1.5", "1e0", "9223372036854775808"] {
        case(&format!("locktime_{lock}"), format!("[[], {{}}, {lock}]"));
    }
    for vout in ["0", "2147483647", "2147483648", "-1", "0.5", "1e0", "\"0\""] {
        case(&format!("vout_{vout}"), format!("[{}, {{}}]", input(&format!(r#","vout":{vout}"#))));
    }
    case("vout_missing", format!("[{}, {{}}]", input("")));
    for seq in [
        "-1",
        "0",
        "4294967294",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "9223372036854775808",
        "1.5",
        "1e0",
    ] {
        for lock in [0, 1] {
            case(
                &format!("sequence_{seq}_lock{lock}"),
                format!("[{}, {{}}, {lock}]", input(&format!(r#","vout":0,"sequence":{seq}"#))),
            );
        }
    }
    for amount in [
        "0",
        "1",
        "\"1\"",
        "0.00000001",
        "0.000000001",
        "1e-8",
        "\"1e-8\"",
        "-0",
        "-1",
        "21000000",
        "21000000.00000001",
        "1e11",
        "\"\"",
        "\"abc\"",
        "\"1.\"",
        "92233720368.54775808",
    ] {
        case(&format!("amount_{amount}"), format!(r#"[[], {{"{address}":{amount}}}]"#));
    }
    for data in [
        "\"00\"",
        "\"\"",
        "\"0\"",
        "\"0g\"",
        "12",
        "\"ff\"",
        &format!("\"{}\"", "ab".repeat(80)),
        &format!("\"{}\"", "cd".repeat(300)),
    ] {
        case(&format!("data_{data}"), format!(r#"[[], {{"data":{data}}}]"#));
    }
    case("ok_one", format!(r#"[{}, {{"{address}":0.5}}]"#, input(r#","vout":1"#)));
    case(
        "ok_order",
        format!(
            r#"[{}, [{{"data":"00"}}, {{"{other}":1}}, {{"{address}":2}}]]"#,
            input(r#","vout":1"#)
        ),
    );
    case("ok_obj_order", format!(r#"[[], {{"{other}":1, "data":"00", "{address}":2}}]"#));
    case(
        "ok_two_inputs",
        format!(
            r#"[[{{"txid":"{txid}","vout":0}},{{"txid":"{}","vout":9,"sequence":7}}], {{}}, 5]"#,
            "02".repeat(32)
        ),
    );
    case("empty_both", "[[], {}]".to_owned());
    case("empty_array_outputs", "[[], []]".to_owned());
    case("dup_data_obj", r#"[[], {"data":"00","data":"01"}]"#.to_owned());
    case("dup_data_arr", r#"[[], [{"data":"00"},{"data":"01"}]]"#.to_owned());
    case("dup_addr_obj", format!(r#"[[], {{"{address}":1,"{address}":2}}]"#));
    case("dup_addr_arr", format!(r#"[[], [{{"{address}":1}},{{"{address}":2}}]]"#));
    case("pair_two_keys", r#"[[], [{"data":"00","x":1}]]"#.to_owned());
    case("pair_empty", "[[], [{}]]".to_owned());
    case("bad_address", r#"[[], {"nope":1}]"#.to_owned());
    case("mainnet_address", r#"[[], {"XtqSw4GK4k8Y1fgCtVTYsELSH3hFBx2sNb":1}]"#.to_owned());
    case("precedence_locktime_then_inputs", "[[5], null, -1]".to_owned());
    case("precedence_inputs_then_outputs", "[[5], null]".to_owned());
    case(
        "precedence_bad_address_then_dup",
        r#"[[], {"nope":1, "data":"00", "data":"01"}]"#.to_owned(),
    );
    case("precedence_amount_then_dup", format!(r#"[[], {{"{address}":"x", "{address}":1}}]"#));
    case("txid_short", r#"[[{"txid":"00","vout":0}], {}]"#.to_owned());
    case("txid_non_hex", format!(r#"[[{{"txid":"{}","vout":0}}], {{}}]"#, "g".repeat(64)));
    let calls: Vec<_> = create_cases
        .iter()
        .map(|(name, params)| (format!("create_{name}"), "createrawtransaction", params.clone()))
        .collect();
    let created = diff_chunks(&mut pair, &calls);
    let round_trip: Vec<_> = created
        .iter()
        .zip(&create_cases)
        .filter_map(|(r, (name, _))| {
            r.get_str().ok().map(|hex| {
                (format!("roundtrip_{name}"), "decoderawtransaction", format!(r#"["{hex}"]"#))
            })
        })
        .collect();
    assert!(round_trip.len() >= 30, "{} successes", round_trip.len());
    diff_chunks(&mut pair, &round_trip);
    pair.finish();

    // The process flag across V19's activation, with the service fields on (§3.4): V19 at 3, so
    // tips 0 and 1 are legacy and tip 2 flips it.
    let mut pair = Pair::start(&["-testactivationheight=v19@3", "-deprecatedrpc=service"]);
    pair.mock_now();
    let rows = txtouniv_rows(true);
    let calls: Vec<_> = rows
        .iter()
        .map(|(bucket, tx)| {
            (format!("service_fixture_{bucket}"), "decoderawtransaction", format!(r#"["{tx}"]"#))
        })
        .collect();
    let at_genesis = diff_chunks(&mut pair, &calls);
    assert!(!at_genesis.is_empty());
    for (t, payload) in [(1, "ffff"), (2, "ffff")] {
        let tx = special_tx(t, payload);
        both_internal_bug(
            &mut pair,
            &format!("service_netinfo_t{t}"),
            "decoderawtransaction",
            &format!(r#"["{tx}"]"#),
        );
    }
    // `GetPlatformPort`'s check, P2P before HTTPS.
    for (name, tx) in evo_net_info_edits(&rows) {
        both_internal_bug(
            &mut pair,
            &format!("service_ports_{name}"),
            "decoderawtransaction",
            &format!(r#"["{tx}"]"#),
        );
    }
    let genesis = diff_chunks(&mut pair, &bls_field_calls("tip0"));
    pair.both("mine_1", "generatetodescriptor", r#"[1, "raw(51)"]"#);
    let legacy = diff_chunks(&mut pair, &bls_field_calls("tip1"));
    pair.both("mine_2", "generatetodescriptor", r#"[1, "raw(51)"]"#);
    let basic = diff_chunks(&mut pair, &bls_field_calls("tip2"));
    let tip2 = pair.result("mine_2");
    let tip2 = values_str(&tip2);
    pair.both("disconnect", "invalidateblock", &format!(r#"["{tip2}"]"#));
    let disconnected = diff_chunks(&mut pair, &bls_field_calls("disconnected"));
    pair.both("reconnect", "reconsiderblock", &format!(r#"["{tip2}"]"#));
    let reconnected = diff_chunks(&mut pair, &bls_field_calls("reconnected"));
    pair.finish();

    // dashd's own replies show the flip: the unlock's basic signature prints as zeros under the
    // legacy flag and as itself under the basic one (calls are cbtx, unlock, mnhf per kind).
    let unlock_sig = |replies: &[UniValue]| {
        let UniValue::Obj(f) = &replies[4] else {
            panic!()
        };
        let UniValue::Obj(u) = &f.iter().find(|(k, _)| k == "assetUnlockTx").unwrap().1 else {
            panic!()
        };
        u.iter().find(|(k, _)| k == "quorumSig").unwrap().1.get_str().unwrap().to_owned()
    };
    let [_, (_, basic_sig), ..] = bls_sig_kinds();
    assert_eq!(unlock_sig(&genesis), "0".repeat(192));
    assert_eq!(unlock_sig(&legacy), "0".repeat(192));
    assert_eq!(unlock_sig(&basic), basic_sig);
    assert_eq!(unlock_sig(&disconnected), "0".repeat(192));
    assert_eq!(unlock_sig(&reconnected), basic_sig);
}

/// The one block hash `generatetodescriptor 1` returned.
fn values_str(result: &UniValue) -> String {
    let UniValue::Arr(hashes) = result else {
        panic!("{}", result.write())
    };
    hashes[0].get_str().unwrap().to_owned()
}

/// Compressed public keys of the secret keys 1 to 16 (key 1 is [`GENERATOR_KEY`]).
const SECRET_PUBKEYS: [&str; 16] = [
    GENERATOR_KEY,
    "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    "02f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9",
    "02e493dbf1c10d80f3581e4904930b1404cc6c13900ee0758474fa94abe8c4cd13",
    "022f8bde4d1a07209355b4a7250a5c5128e88b84bddc619ab7cba8d569b240efe4",
    "03fff97bd5755eeea420453a14355235d382f6472f8568a18b2f057a1460297556",
    "025cbdf0646e5db4eaa398f365f2ea7a0e3d419b7e0330e39ce92bddedcac4f9bc",
    "022f01e5e15cca351daff3843fb70f3c2f0a1bdd05e5af888a67784ef3e10a2a01",
    "03acd484e2f0c7f65309ad178a9f559abde09796974c57e714c35f110dfc27ccbe",
    "03a0434d9e47f3c86235477c7b1ae6ae5d3442d49b1943c2b752a68e2a47e247c7",
    "03774ae7f858a9411e5ef4246b70c65aac5649980be5c17891bbec17895da008cb",
    "03d01115d548e7561b15c38f004d734633687cf4419620095bc5b0f47070afe85a",
    "03f28773c2d975288bc7d1d205c3748651b075fbc6610e58cddeeddf8f19405aa8",
    "03499fdf9e895e719cfd64e67f07d38e3226aa7b63678949e6e49b241a60e823e4",
    "02d7924d4f7d43ea965a465ae3095ff41131e5946f3c85f79e44adbcf8e27e080e",
    "03e60fce93b59e9ec53011aabc21c23e97b2a31369b87a5ae9c44ee89e2a6dec0a",
];

/// The secret keys 2 and 3 as compressed regtest WIFs.
const SECRET_2_WIF: &str = "cMahea7zqjxrtgAbB7LSGbcQUr1uX1ojuat9jZodMN87K7XCyj5v";
const SECRET_3_WIF: &str = "cMahea7zqjxrtgAbB7LSGbcQUr1uX1ojuat9jZodMN87KcLPVfXz";

/// `m`-of-the-first-`n` [`SECRET_PUBKEYS`] bare multisig script, as hex.
fn multisig_script(m: u8, n: usize) -> String {
    let mut s = format!("{:02x}", 0x50 + m);
    for key in &SECRET_PUBKEYS[..n] {
        s.push_str(&format!("21{key}"));
    }
    s.push_str(&format!("{:02x}ae", 0x50 + n));
    s
}

/// The P2SH scriptPubKey of the script `redeem` (hex).
fn p2sh_of(redeem: &str) -> String {
    use bitcoin_hashes::hash160;
    let h = hash160::Hash::hash(&parse_hex(redeem)).to_byte_array();
    format!("a914{}87", hex(&h))
}

/// `script`'s pushes (hex), `OP_0` as empty; panics on a non-push opcode.
fn pushes(script: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < script.len() {
        let op = script[i];
        i += 1;
        let len = match op {
            0 => 0,
            1..=0x4b => usize::from(op),
            0x4c => {
                i += 1;
                usize::from(script[i - 1])
            }
            0x4d => {
                i += 2;
                usize::from(u16::from_le_bytes([script[i - 2], script[i - 1]]))
            }
            _ => panic!("not a push: {op:02x}"),
        };
        out.push(script[i..i + len].to_vec());
        i += len;
    }
    out
}

/// A script pushing each item (empty as `OP_0`), as hex.
fn push_script(items: &[&[u8]]) -> String {
    let mut s = Vec::new();
    for item in items {
        match item.len() {
            0 => s.push(0),
            n @ 1..=0x4b => {
                s.push(u8::try_from(n).unwrap());
                s.extend_from_slice(item);
            }
            n @ 0x4c..=0xff => {
                s.extend([0x4c, u8::try_from(n).unwrap()]);
                s.extend_from_slice(item);
            }
            n => {
                s.push(0x4d);
                s.extend(u16::try_from(n).unwrap().to_le_bytes());
                s.extend_from_slice(item);
            }
        }
    }
    hex(&s)
}

/// The scriptSigs of the simple transaction `tx` (hex): no special payload, one-byte counts and
/// script lengths below 0xfd.
fn script_sigs(tx: &str) -> Vec<Vec<u8>> {
    let b = parse_hex(tx);
    let mut i = 4;
    let n = usize::from(b[i]);
    i += 1;
    let mut out = Vec::new();
    for _ in 0..n {
        i += 36;
        let len = usize::from(b[i]);
        assert!(len < 0xfd, "a one-byte script length");
        i += 1;
        out.push(b[i..i + len].to_vec());
        i += len + 4;
    }
    out
}

/// `tx` (hex, as [`script_sigs`] reads it) with input `n`'s scriptSig replaced by `script` (hex).
fn with_script_sig(tx: &str, n: usize, script: &str) -> String {
    let b = parse_hex(tx);
    let mut out = b[..5].to_vec();
    let mut i = 5;
    for k in 0..usize::from(b[4]) {
        out.extend_from_slice(&b[i..i + 36]);
        i += 36;
        let len = usize::from(b[i]);
        let old = &b[i..=i + len];
        i += 1 + len;
        if k == n {
            let new = parse_hex(script);
            out.push(u8::try_from(new.len()).unwrap());
            out.extend(new);
        } else {
            out.extend_from_slice(old);
        }
        out.extend_from_slice(&b[i..i + 4]);
        i += 4;
    }
    out.extend_from_slice(&b[i..]);
    hex(&out)
}

/// BL-688's signing utilities, `combinerawtransaction` and `signrawtransactionwithkey`, against
/// dashd v24 on the same arguments and the same chain (PLAN rev2 §6; Sol r1/r2):
///
/// - **Coins:** pkh, pk, pkh of the full key, bare 1-of-1, 2-of-3 and 3-of-3 multisig (dashd's bare limit is 3 keys), P2SH 2-of-3
///   and P2SH pk outputs mined on both nodes, matured, one of them spent.
/// - **signrawtransactionwithkey:** every coin with full, partial and no keys, the uncompressed
///   WIF, every sighash name, SINGLE without its output, multi-input partial signing, prevtxs for
///   unknown coins (amount given, omitted), a matching and a mismatching 547-byte redeem script,
///   every ParsePrevouts refusal in Core's key order, bad keys and hex, preflight and arity forms.
/// - **combinerawtransaction:** node-signed partials in every order, complete alternatives (first
///   wins), redeem scripts only in a later variant or mismatched, raw stacks with missing and
///   reordered placeholders and `[sig3, OP_0, OP_0, sig2]`, an incomplete nonstandard scriptSig, a
///   variant with other outputs signed over its own form, a shorter later variant, a zero-input
///   first transaction, and the errors in Core's order.
// Core: rpc/rawtransaction.cpp:1011-1190; rpc/rawtransaction_util.cpp:144-273; script/sign.cpp
#[test]
fn signing_utilities_match_dashd_v24() {
    let mut pair = Pair::start(&[]);
    for method in ["combinerawtransaction", "signrawtransactionwithkey"] {
        pair.both(&format!("help_{method}"), "help", &format!(r#"["{method}"]"#));
        pair.both(&format!("{method}_no_args"), method, "[]");
    }
    // Too few arguments is the help (arity before types), an explicit null is the type error.
    pair.both("sign_one_arg", "signrawtransactionwithkey", r#"["zz"]"#);
    pair.both("sign_null_keys", "signrawtransactionwithkey", r#"["zz", null]"#);
    pair.both("sign_null_hex", "signrawtransactionwithkey", "[null, []]");
    pair.both("combine_null", "combinerawtransaction", "[null]");
    pair.both("combine_extra", "combinerawtransaction", "[[], 1]");
    pair.both("sign_extra", "signrawtransactionwithkey", r#"["zz", [], [], "ALL", 1]"#);
    pair.both("combine_named", "combinerawtransaction", r#"{"txs":[]}"#);
    pair.both("sign_named", "signrawtransactionwithkey", r#"{"hexstring":"zz","privkeys":[]}"#);
    pair.mock_now();

    // The coins: one block per descriptor, then maturity.
    let k = SECRET_PUBKEYS;
    let descriptors = [
        ("pkh", format!("pkh({})", k[0])),
        ("pk", format!("pk({})", k[0])),
        ("pkh_full", format!("pkh({GENERATOR_KEY_FULL})")),
        ("multi11", format!("multi(1,{})", k[0])),
        ("multi23", format!("multi(2,{},{},{})", k[0], k[1], k[2])),
        ("multi33", format!("multi(3,{},{},{})", k[0], k[1], k[2])),
        ("sh23", format!("sh(multi(2,{},{},{}))", k[0], k[1], k[2])),
        ("shpk", format!("sh(pk({}))", k[0])),
        ("spent", format!("pkh({})", k[0])),
    ];
    let mut coins = std::collections::HashMap::new();
    for (name, descriptor) in &descriptors {
        let mine = format!("mine_{name}");
        pair.both(&mine, "generatetodescriptor", &format!(r#"[1, "{descriptor}"]"#));
        let mined = pair.result(&mine);
        let block =
            mined.get_values().unwrap_or_else(|_| panic!("{mine}: {}", pair.replies[&mine]))[0]
                .get_str()
                .unwrap()
                .to_owned();
        let got = format!("block_{name}");
        pair.both(&got, "getblock", &format!(r#"["{block}", 2]"#));
        let tx = field(&pair.result(&got), "tx").get_values().unwrap()[0].clone();
        let out = field(&tx, "vout").get_values().unwrap()[0].clone();
        let value: i64 = field(&out, "valueSat").write().parse().unwrap();
        let script = field(&field(&out, "scriptPubKey"), "hex").get_str().unwrap().to_owned();
        coins.insert(*name, (field(&tx, "txid").get_str().unwrap().to_owned(), value, script));
    }
    pair.both("mine_maturity", "generatetodescriptor", r#"[100, "raw(51)"]"#);
    let spend = |name: &str, outputs: &[(i64, &str)]| {
        let (txid, _, _) = &coins[name];
        spend_tx(1, &[(txid.as_str(), 0, "")], outputs)
    };
    let pay = |name: &str| spend(name, &[(coins[name].1 - 10_000, "51")]);
    let w1 = GENERATOR_WIF;
    let keys = |ks: &[&str]| {
        format!("[{}]", ks.iter().map(|k| format!(r#""{k}""#)).collect::<Vec<_>>().join(","))
    };
    let prevtx = |name: &str, redeem: Option<&str>| {
        let (txid, value, script) = &coins[name];
        let redeem = redeem.map_or(String::new(), |r| format!(r#","redeemScript":"{r}""#));
        format!(
            r#"{{"txid":"{txid}","vout":0,"scriptPubKey":"{script}","amount":{value}e-8{redeem}}}"#
        )
    };

    // One coin spent on both chains, for the spent-input errors.
    let spent_tx = pay("spent");
    pair.both(
        "sign_spent_source",
        "signrawtransactionwithkey",
        &format!(r#"["{spent_tx}", ["{w1}"]]"#),
    );
    let signed_spent =
        field(&pair.result("sign_spent_source"), "hex").get_str().unwrap().to_owned();
    pair.both("mine_spend", "generateblock", &format!(r#"["raw(51)", ["{signed_spent}"]]"#));

    let sh23 = multisig_script(2, 3);
    let shpk = format!("21{}ac", k[0]);
    let multi_in = spend_tx(
        1,
        &[
            (coins["pkh"].0.as_str(), 0, ""),
            (coins["multi23"].0.as_str(), 0, ""),
            (coins["sh23"].0.as_str(), 0, ""),
        ],
        &[(coins["pkh"].1, "51")],
    );
    let single_short = spend_tx(
        1,
        &[(coins["pkh"].0.as_str(), 0, ""), (coins["pk"].0.as_str(), 0, "")],
        &[(coins["pkh"].1 - 10_000, "51")],
    );
    let fake = "ab".repeat(32);
    let fake_in = spend_tx(1, &[(fake.as_str(), 3, "")], &[(1000, "51")]);
    let big_redeem = multisig_script(1, 16);
    assert_eq!(big_redeem.len() / 2, 547);
    let fake_prev = |script: &str, extra: &str| {
        format!(r#"[{{"txid":"{fake}","vout":3,"scriptPubKey":"{script}"{extra}}}]"#)
    };
    let pkh_script = coins["pkh"].2.clone();

    let mut sign: Vec<(String, String)> = Vec::new();
    let mut s = |name: &str, params: String| sign.push((format!("sign_{name}"), params));
    for (name, ks) in [
        ("pkh_k1", vec![w1]),
        ("pkh_k2", vec![SECRET_2_WIF]),
        ("pkh_none", vec![]),
        ("pk_k1", vec![w1]),
        ("pkh_full_uncompressed", vec![GENERATOR_WIF_UNCOMPRESSED]),
        ("pkh_full_compressed", vec![w1]),
        ("multi11_k1", vec![w1]),
        ("multi23_k1", vec![w1]),
        ("multi23_k2", vec![SECRET_2_WIF]),
        ("multi23_k3", vec![SECRET_3_WIF]),
        ("multi23_k1k2", vec![w1, SECRET_2_WIF]),
        ("multi23_k1k3", vec![w1, SECRET_3_WIF]),
        ("multi23_k2k3", vec![SECRET_2_WIF, SECRET_3_WIF]),
        ("multi23_all", vec![SECRET_3_WIF, w1, SECRET_2_WIF]),
        ("multi23_none", vec![]),
        ("multi33_none", vec![]),
        ("multi33_k1", vec![w1]),
        ("multi33_k1k2", vec![w1, SECRET_2_WIF]),
        ("sh23_k1_no_prevtx", vec![w1]),
        ("shpk_k1_no_prevtx", vec![w1]),
    ] {
        let coin = name.split('_').next().unwrap();
        s(name, format!(r#"["{}", {}]"#, pay(coin), keys(&ks)));
    }
    for (name, ks) in
        [("k1", vec![w1]), ("k2", vec![SECRET_2_WIF]), ("k1k2", vec![w1, SECRET_2_WIF])]
    {
        s(
            &format!("sh23_{name}"),
            format!(r#"["{}", {}, [{}]]"#, pay("sh23"), keys(&ks), prevtx("sh23", Some(&sh23))),
        );
    }
    s("shpk_k1", format!(r#"["{}", ["{w1}"], [{}]]"#, pay("shpk"), prevtx("shpk", Some(&shpk))));
    for sighash in
        ["ALL", "NONE", "SINGLE", "ALL|ANYONECANPAY", "NONE|ANYONECANPAY", "SINGLE|ANYONECANPAY"]
    {
        s(
            &format!("sighash_{sighash}"),
            format!(r#"["{}", ["{w1}"], null, "{sighash}"]"#, pay("pkh")),
        );
        s(
            &format!("multi23_k1_{sighash}"),
            format!(r#"["{}", ["{w1}"], null, "{sighash}"]"#, pay("multi23")),
        );
    }
    s("sighash_bogus", format!(r#"["{}", ["{w1}"], null, "all"]"#, pay("pkh")));
    s("sighash_numeric", format!(r#"["{}", ["{w1}"], null, 1]"#, pay("pkh")));
    s(
        "single_short",
        format!(r#"["{single_short}", ["{w1}", "{GENERATOR_WIF_UNCOMPRESSED}"], null, "SINGLE"]"#),
    );
    s("single_short_all", format!(r#"["{single_short}", ["{w1}"], null, "ALL"]"#));
    s(
        "multi_in_all",
        format!(
            r#"["{multi_in}", {}, [{}]]"#,
            keys(&[w1, SECRET_2_WIF]),
            prevtx("sh23", Some(&sh23))
        ),
    );
    s("multi_in_k1", format!(r#"["{multi_in}", ["{w1}"], [{}]]"#, prevtx("sh23", Some(&sh23))));
    s("multi_in_no_prevtx", format!(r#"["{multi_in}", ["{w1}"]]"#));
    s("spent_input", format!(r#"["{spent_tx}", ["{w1}"]]"#));
    s("unknown_input", format!(r#"["{fake_in}", ["{w1}"]]"#));
    s(
        "prev_amount",
        format!(r#"["{fake_in}", ["{w1}"], {}]"#, fake_prev(&pkh_script, r#","amount":"0.5""#)),
    );
    s("prev_no_amount", format!(r#"["{fake_in}", ["{w1}"], {}]"#, fake_prev(&pkh_script, "")));
    s(
        "prev_null_amount",
        format!(r#"["{fake_in}", ["{w1}"], {}]"#, fake_prev(&pkh_script, r#","amount":null"#)),
    );
    s(
        "prev_bad_amount",
        format!(r#"["{fake_in}", ["{w1}"], {}]"#, fake_prev(&pkh_script, r#","amount":"x""#)),
    );
    s(
        "prev_negative_amount",
        format!(r#"["{fake_in}", ["{w1}"], {}]"#, fake_prev(&pkh_script, r#","amount":-1"#)),
    );
    s(
        "prev_big_redeem",
        format!(
            r#"["{fake_in}", ["{w1}"], {}]"#,
            fake_prev(&p2sh_of(&big_redeem), &format!(r#","redeemScript":"{big_redeem}""#))
        ),
    );
    s(
        "prev_big_redeem_mismatch",
        format!(
            r#"["{fake_in}", ["{w1}"], {}]"#,
            fake_prev(&p2sh_of(&sh23), &format!(r#","redeemScript":"{big_redeem}""#))
        ),
    );
    s(
        "prev_redeem_mismatch",
        format!(
            r#"["{fake_in}", ["{w1}"], {}]"#,
            fake_prev(&p2sh_of(&sh23), &format!(r#","redeemScript":"{shpk}""#))
        ),
    );
    s(
        "prev_redeem_missing",
        format!(r#"["{fake_in}", ["{w1}"], {}]"#, fake_prev(&p2sh_of(&sh23), "")),
    );
    s(
        "prev_redeem_null",
        format!(
            r#"["{fake_in}", ["{w1}"], {}]"#,
            fake_prev(&p2sh_of(&sh23), r#","redeemScript":null"#)
        ),
    );
    s(
        "prev_redeem_number",
        format!(
            r#"["{fake_in}", ["{w1}"], {}]"#,
            fake_prev(&p2sh_of(&sh23), r#","redeemScript":5"#)
        ),
    );
    s(
        "prev_redeem_bad_hex",
        format!(
            r#"["{fake_in}", ["{w1}"], {}]"#,
            fake_prev(&p2sh_of(&sh23), r#","redeemScript":"0g""#)
        ),
    );
    s(
        "prev_redeem_without_keys",
        format!(r#"["{fake_in}", [], {}]"#, fake_prev(&p2sh_of(&sh23), "")),
    );
    let (pkh_txid, _, _) = &coins["pkh"];
    for (name, prev) in [
        ("prev_number", "[5]".to_owned()),
        ("prev_empty_object", "[{}]".to_owned()),
        ("prev_spk_number", r#"[{"scriptPubKey":5}]"#.to_owned()),
        ("prev_no_txid", r#"[{"scriptPubKey":"51"}]"#.to_owned()),
        ("prev_txid_number", r#"[{"scriptPubKey":"51","txid":5}]"#.to_owned()),
        ("prev_no_vout", format!(r#"[{{"scriptPubKey":"51","txid":"{fake}"}}]"#)),
        ("prev_vout_string", format!(r#"[{{"scriptPubKey":"51","txid":"{fake}","vout":"0"}}]"#)),
        ("prev_vout_negative", format!(r#"[{{"scriptPubKey":"51","txid":"{fake}","vout":-1}}]"#)),
        ("prev_vout_fraction", format!(r#"[{{"scriptPubKey":"51","txid":"{fake}","vout":1.5}}]"#)),
        ("prev_txid_short", r#"[{"scriptPubKey":"51","txid":"00","vout":0}]"#.to_owned()),
        ("prev_spk_bad_hex", format!(r#"[{{"scriptPubKey":"5","txid":"{fake}","vout":0}}]"#)),
        ("prev_spk_mismatch", format!(r#"[{{"scriptPubKey":"51","txid":"{pkh_txid}","vout":0}}]"#)),
        (
            "prev_spk_match",
            format!(r#"[{{"scriptPubKey":"{pkh_script}","txid":"{pkh_txid}","vout":0}}]"#),
        ),
        ("prev_object", "{}".to_owned()),
        ("prev_number_top", "5".to_owned()),
        ("prev_null", "null".to_owned()),
    ] {
        s(name, format!(r#"["{}", ["{w1}"], {prev}]"#, pay("pkh")));
    }
    // Content failures in argument order, and preflight before any of them.
    s("bad_hex", format!(r#"["zz", ["{w1}"]]"#));
    s("hex_number", format!(r#"[5, ["{w1}"]]"#));
    s("keys_number", format!(r#"["{}", 5]"#, pay("pkh")));
    s("keys_object", format!(r#"["{}", {{}}]"#, pay("pkh")));
    s("bad_hex_keys_number", r#"["zz", 5]"#.to_owned());
    s("two_mismatches", "[5, 5]".to_owned());
    s("bad_key", format!(r#"["{}", ["nope"]]"#, pay("pkh")));
    s("key_number", format!(r#"["{}", [5]]"#, pay("pkh")));
    s("bad_key_then_number", format!(r#"["{}", ["nope", 5]]"#, pay("pkh")));
    s("bad_hex_bad_key", r#"["zz", ["nope"]]"#.to_owned());
    s("bad_key_bad_prev", format!(r#"["{}", ["nope"], [5]]"#, pay("pkh")));
    s("bad_prev_bogus_sighash", format!(r#"["{}", ["{w1}"], [5], "bogus"]"#, pay("pkh")));
    s("bad_prev_numeric_sighash", format!(r#"["{}", ["{w1}"], [5], 1]"#, pay("pkh")));
    s(
        "mainnet_key",
        format!(r#"["{}", ["XBHddvWWiMu3nZhhpTXBQWJMmdz5JNKJD85b9fgKAckCT2coW3Y4"]]"#, pay("pkh")),
    );
    let calls: Vec<_> =
        sign.iter().map(|(n, p)| (n.clone(), "signrawtransactionwithkey", p.clone())).collect();
    let signed = diff_chunks(&mut pair, &calls);
    let sign_results: std::collections::HashMap<String, UniValue> =
        calls.iter().map(|(n, _, _)| n.clone()).zip(signed.iter().cloned()).collect();
    let signed: std::collections::HashMap<&str, String> = calls
        .iter()
        .zip(&signed)
        .filter_map(|((n, _, _), r)| match r {
            UniValue::Obj(_) => Some((
                n.strip_prefix("sign_").unwrap(),
                field(r, "hex").get_str().unwrap().to_owned(),
            )),
            _ => None,
        })
        .collect();

    // Combine: node-signed partials, hand-built stacks, and the errors.
    let sig = |name: &str, n: usize| -> Vec<Vec<u8>> { pushes(&script_sigs(&signed[name])[n]) };
    let multi = pay("multi23");
    let [s1, s2, s3] = ["multi23_k1", "multi23_k2", "multi23_k3"].map(|n| sig(n, 0)[1].clone());
    let s2_none = sig("multi23_k1_NONE", 0)[1].clone();
    let raw = |items: &[&[u8]]| with_script_sig(&multi, 0, &push_script(items));
    let e: &[u8] = &[];
    let sh_unsigned = pay("sh23");
    let sh_k1 = &signed["sh23_k1"];
    let sh_items = pushes(&script_sigs(sh_k1)[0]);
    let wrong_redeem =
        with_script_sig(&sh_unsigned, 0, &push_script(&[e, &sh_items[1], &parse_hex(&shpk)]));
    let other_out = spend("multi23", &[(coins["multi23"].1 - 20_000, "52")]);
    pair.both(
        "sign_other_out_k2",
        "signrawtransactionwithkey",
        &format!(r#"["{other_out}", ["{SECRET_2_WIF}"]]"#),
    );
    pair.both(
        "sign_other_out_full",
        "signrawtransactionwithkey",
        &format!(r#"["{other_out}", {}]"#, keys(&[w1, SECRET_2_WIF])),
    );
    let other_k2 = field(&pair.result("sign_other_out_k2"), "hex").get_str().unwrap().to_owned();
    let other_full =
        field(&pair.result("sign_other_out_full"), "hex").get_str().unwrap().to_owned();
    let shorter = pay("pkh");
    let zero_in = spend_tx(1, &[], &[(1000, "51")]);
    let pkh_unsigned = pay("pkh");
    let pkh_nonstd = with_script_sig(&pkh_unsigned, 0, "51");
    let multi_nonstd = with_script_sig(&multi, 0, "5151");
    let txs = |v: &[&str]| {
        format!("[[{}]]", v.iter().map(|t| format!(r#""{t}""#)).collect::<Vec<_>>().join(","))
    };
    let a = &signed["multi23_k1"];
    let b = &signed["multi23_k2"];
    let c = &signed["multi23_k3"];
    let full12 = &signed["multi23_k1k2"];
    let full23 = &signed["multi23_k2k3"];
    let mut combine: Vec<(String, String)> = vec![
        ("ab".into(), txs(&[a, b])),
        ("ba".into(), txs(&[b, a])),
        ("ac".into(), txs(&[a, c])),
        ("cb".into(), txs(&[c, b])),
        ("aa".into(), txs(&[a, a])),
        ("cc".into(), txs(&[c, c])),
        ("a".into(), txs(&[a])),
        ("unsigned_a".into(), txs(&[&multi, a])),
        ("a_unsigned".into(), txs(&[a, &multi])),
        ("full12_full23".into(), txs(&[full12, full23])),
        ("full23_full12".into(), txs(&[full23, full12])),
        ("a_full23".into(), txs(&[a, full23])),
        ("full12_c".into(), txs(&[full12, c])),
        ("none_k1_all_k2".into(), txs(&[&signed["multi23_k1_NONE"], b])),
        ("raw_s3_0_0_s2".into(), txs(&[&raw(&[&s3, e, e, &s2])])),
        ("raw_s3_0_0_s2_b".into(), txs(&[&raw(&[&s3, e, e, &s2]), c])),
        ("raw_reversed".into(), txs(&[&raw(&[e, &s2, &s1])])),
        ("raw_reversed_c".into(), txs(&[&raw(&[e, &s3, &s2]), a])),
        ("raw_s1_only".into(), txs(&[&raw(&[&s1])])),
        ("raw_s1_0".into(), txs(&[&raw(&[&s1, e])])),
        ("raw_0_0_s1".into(), txs(&[&raw(&[e, e, &s1])])),
        ("raw_0_s1_0".into(), txs(&[&raw(&[e, &s1, e])])),
        ("raw_s1_s2_no_dummy".into(), txs(&[&raw(&[&s1, &s2])])),
        ("raw_s2none_s3".into(), txs(&[&raw(&[e, &s2_none, &s3])])),
        ("raw_garbage_sig".into(), txs(&[&raw(&[e, &[0x30, 0x01, 0x02][..]])])),
        ("multi_nonstd".into(), txs(&[&multi_nonstd])),
        ("multi_nonstd_a".into(), txs(&[&multi_nonstd, a])),
        ("pkh_nonstd".into(), txs(&[&pkh_nonstd])),
        ("pkh_nonstd_signed".into(), txs(&[&pkh_nonstd, &signed["pkh_k1"]])),
        ("pkh_unsigned_signed".into(), txs(&[&pkh_unsigned, &signed["pkh_k1"]])),
        ("pkh_signed_unsigned".into(), txs(&[&signed["pkh_k1"], &pkh_unsigned])),
        ("pk_signed".into(), txs(&[&pay("pk"), &signed["pk_k1"]])),
        ("multi33_k1_k1k2".into(), txs(&[&signed["multi33_k1"], &signed["multi33_k1k2"]])),
        ("multi33_none".into(), txs(&[&pay("multi33")])),
        ("sh_k1_k2".into(), txs(&[sh_k1, &signed["sh23_k2"]])),
        ("sh_unsigned_k1".into(), txs(&[&sh_unsigned, sh_k1])),
        ("sh_unsigned_k1_k2".into(), txs(&[&sh_unsigned, sh_k1, &signed["sh23_k2"]])),
        ("sh_wrong_redeem".into(), txs(&[&wrong_redeem])),
        ("sh_wrong_redeem_k2".into(), txs(&[&wrong_redeem, &signed["sh23_k2"]])),
        ("sh_no_prevtx".into(), txs(&[&signed["sh23_k1_no_prevtx"], sh_k1])),
        ("shpk".into(), txs(&[&pay("shpk"), &signed["shpk_k1"]])),
        ("other_out_k2".into(), txs(&[a, &other_k2])),
        ("other_out_full".into(), txs(&[a, &other_full])),
        ("other_out_first".into(), txs(&[&other_k2, a])),
        ("multi_in_shorter".into(), txs(&[&signed["multi_in_k1"], &shorter])),
        (
            "multi_in_all_shorter".into(),
            txs(&[&multi_in, &signed["pkh_k1"], &signed["multi_in_all"]]),
        ),
        ("zero_inputs".into(), txs(&[&zero_in])),
        ("zero_inputs_a".into(), txs(&[&zero_in, a])),
        ("empty".into(), "[[]]".into()),
        ("number".into(), "[5]".into()),
        ("object".into(), "[{}]".into()),
        ("element_number".into(), "[[123]]".into()),
        ("element_null".into(), "[[null]]".into()),
        ("bad_hex_0".into(), txs(&["zz"])),
        ("bad_hex_1".into(), txs(&[a, "zz"])),
        ("bad_hex_then_number".into(), r#"[["zz", 5]]"#.into()),
        ("number_then_bad_hex".into(), r#"[[5, "zz"]]"#.into()),
        ("unknown_then_bad_hex".into(), txs(&[&fake_in, "zz"])),
        ("unknown".into(), txs(&[&fake_in])),
        ("spent".into(), txs(&[&spent_tx])),
        (
            "spent_second_input".into(),
            txs(&[&spend_tx(
                1,
                &[(coins["pkh"].0.as_str(), 0, ""), (fake.as_str(), 3, "")],
                &[(1000, "51")],
            )]),
        ),
    ];
    // The node-signed partials over every pair of the three keys, both orders.
    for (x, y) in [("k1", "k2"), ("k2", "k3"), ("k1", "k3")] {
        let (p, q) = (&signed[&*format!("multi23_{x}")], &signed[&*format!("multi23_{y}")]);
        combine.push((format!("pair_{x}_{y}"), txs(&[p, q])));
        combine.push((format!("pair_{y}_{x}"), txs(&[q, p])));
    }
    let calls: Vec<_> = combine
        .iter()
        .map(|(n, p)| (format!("combine_{n}"), "combinerawtransaction", p.clone()))
        .collect();
    let combined = diff_chunks(&mut pair, &calls);
    // dashd's own answers show the cases reach the branches they name.
    let got = |name: &str| -> String {
        let i = combine.iter().position(|(n, _)| n == name).unwrap();
        combined[i]
            .get_str()
            .unwrap_or_else(|_| panic!("{name}: {}", combined[i].write()))
            .to_owned()
    };
    assert_eq!(got("ab"), *full12, "two partials complete");
    assert_eq!(got("full12_full23"), *full12, "the first complete variant wins");
    assert_eq!(got("full23_full12"), *full23);
    let sig_of = |tx: &str| hex(&script_sigs(tx)[0]);
    assert_eq!(sig_of(&got("raw_s3_0_0_s2")), push_script(&[e, &s2, e]), "M3: K2 only");
    assert_eq!(sig_of(&got("multi_nonstd")), "0000", "an unsolved scriptSig is rebuilt");
    assert_eq!(sig_of(&got("pkh_nonstd")), "", "nothing to rebuild for pkh");
    assert_eq!(sig_of(&got("multi33_none")), "000000", "m1: the literal padding");
    assert_eq!(sig_of(&signed["multi23_none"]), "0000", "m1: 2-of-3 with no signature");
    assert_eq!(
        sig_of(&got("sh_unsigned_k1")),
        sig_of(sh_k1),
        "the redeem script of a later variant"
    );
    assert_ne!(sig_of(&got("other_out_k2")), sig_of(a), "a variant read against its own form");
    assert_eq!(got("zero_inputs"), zero_in);
    let unsigned_big = field(&sign_results["sign_prev_big_redeem"], "complete").write();
    assert_eq!(unsigned_big, "false", "M4: the oversized redeem script is not stored");
    pair.finish();
}

/// `v`'s member `key`.
fn field(v: &UniValue, key: &str) -> UniValue {
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
fn reply_on(port: u16, method: &str, params: &str) -> UniValue {
    let reply = String::from_utf8(exchange(port, &call(method, params))).unwrap();
    let body = reply.split_once("\r\n\r\n").unwrap().1;
    UniValue::read(body.as_bytes()).unwrap_or_else(|| panic!("{reply}"))
}

/// ALLOWED-DIFFS AD-BL688-1 (DEC-1202, closer MP1a): an input whose parent is only in dashd's
/// mempool. dashd combines it and signs it; rdashd, reading the coins tip only, answers -25 to
/// combinerawtransaction and gives signrawtransactionwithkey's `errors` entry, and both sign alike
/// once `prevtxs` supplies the coin. MP1a turns the rdashd assertions into dashd's answers and
/// deletes the row.
// Core: rpc/rawtransaction.cpp:1053-1077; node/coin.cpp:12-24; script/sign.cpp:432-437
#[test]
fn ad_bl688_1_tip_only_coin_reads() {
    let mut pair = Pair::start(&[]);
    pair.mock_now();
    pair.both("mine_coin", "generatetodescriptor", &format!(r#"[1, "pkh({GENERATOR_KEY})"]"#));
    let block = pair.result("mine_coin").get_values().unwrap()[0].get_str().unwrap().to_owned();
    pair.both("block", "getblock", &format!(r#"["{block}", 2]"#));
    let tx = field(&pair.result("block"), "tx").get_values().unwrap()[0].clone();
    let out = field(&tx, "vout").get_values().unwrap()[0].clone();
    let value: i64 = field(&out, "valueSat").write().parse().unwrap();
    let script = field(&field(&out, "scriptPubKey"), "hex").get_str().unwrap().to_owned();
    let coinbase = field(&tx, "txid").get_str().unwrap().to_owned();
    pair.both("mine_maturity", "generatetodescriptor", r#"[100, "raw(51)"]"#);

    // The parent pays the same pkh script; it goes to dashd's mempool only.
    let w1 = GENERATOR_WIF;
    let parent = spend_tx(1, &[(coinbase.as_str(), 0, "")], &[(value - 10_000, &script)]);
    pair.both("sign_parent", "signrawtransactionwithkey", &format!(r#"["{parent}", ["{w1}"]]"#));
    let parent = field(&pair.result("sign_parent"), "hex").get_str().unwrap().to_owned();
    let [core, ours] = pair.ports;
    let sent = reply_on(core, "sendrawtransaction", &format!(r#"["{parent}"]"#));
    let parent_txid = field(&sent, "result").get_str().unwrap().to_owned();
    assert_eq!(parent_txid, rpc_txid(&parent));

    let child = spend_tx(1, &[(parent_txid.as_str(), 0, "")], &[(value - 20_000, "51")]);
    let sign = format!(r#"["{child}", ["{w1}"]]"#);
    let theirs = field(&reply_on(core, "signrawtransactionwithkey", &sign), "result");
    assert_eq!(field(&theirs, "complete").write(), "true", "dashd signs from its mempool");
    let mine = field(&reply_on(ours, "signrawtransactionwithkey", &sign), "result");
    assert_eq!(field(&mine, "complete").write(), "false");
    assert_eq!(field(&mine, "hex").get_str().unwrap(), child);
    let errors = field(&mine, "errors").write();
    assert_eq!(
        errors,
        format!(
            r#"[{{"txid":"{parent_txid}","vout":0,"scriptSig":"","sequence":4294967295,"error":"Input not found or already spent"}}]"#
        )
    );

    // `prevtxs` supplies the coin: both sign alike.
    let prevtxs = format!(
        r#"["{child}", ["{w1}"], [{{"txid":"{parent_txid}","vout":0,"scriptPubKey":"{script}"}}]]"#
    );
    pair.both("sign_with_prevtxs", "signrawtransactionwithkey", &prevtxs);
    let signed = field(&pair.result("sign_with_prevtxs"), "hex").get_str().unwrap().to_owned();
    assert_eq!(field(&theirs, "hex").get_str().unwrap(), signed);

    let combine = format!(r#"[["{child}", "{signed}"]]"#);
    let theirs = reply_on(core, "combinerawtransaction", &combine);
    assert_eq!(
        field(&theirs, "result").get_str().unwrap(),
        signed,
        "dashd combines from its mempool"
    );
    let mine = reply_on(ours, "combinerawtransaction", &combine);
    assert_eq!(
        field(&mine, "error").write(),
        r#"{"code":-25,"message":"Input not found or already spent"}"#
    );
    pair.finish();
}

/// dashd's printed `quorumSig` of the basic-signature asset unlock among
/// [`bls_field_calls`]' results (cbtx, unlock, mnhf per kind; basic is the second kind).
fn basic_unlock_sig(results: &[UniValue]) -> String {
    field(&field(&results[4], "assetUnlockTx"), "quorumSig").get_str().unwrap().to_owned()
}

/// A height-10 block on `prev` (as RPC shows it) that both nodes store and refuse to connect
/// below DIP0003, as [`unconnectable_block`]'s: a version-2 CbTx coinbase, then the asset unlock
/// and the MnHF signal of [`stored_block_payloads`], each with its signature replaced by every
/// kind of [`bls_sig_kinds`] (unlock, then signal, per kind). Their envelopes stay the
/// fixture's. Returns its hex, its hash as RPC shows it, and its transaction count.
fn bls_candidate(prev: &str, time: u32) -> (String, String, usize) {
    use dash_primitives::Transaction;
    use dash_types::Numeric as _;
    use dash_types::codec::BaseCodec as _;
    let fixtures: Vec<Transaction> = stored_block_payloads()
        .into_iter()
        .map(|raw| Transaction::decode(&mut raw.as_slice()).unwrap())
        .collect();
    let of_type = |t: u16| {
        let found: Vec<_> = fixtures.iter().filter(|tx| tx.tx_type.to_base() == t).collect();
        assert_eq!(found.len(), 1, "one fixture of type {t}");
        found[0].clone()
    };
    let (unlock, signal) = (of_type(9), of_type(7));
    let mut txs = vec![cbtx_coinbase(10)];
    for (_, sig) in bls_sig_kinds() {
        for template in [&unlock, &signal] {
            let mut tx = template.clone();
            assert!(tx.version > 0);
            // Both payloads end with their 96-byte signature.
            let at = tx.extra_payload.len() - 96;
            tx.extra_payload.splice(at.., parse_hex(&sig));
            let mut bytes = Vec::new();
            tx.encode(&mut bytes);
            txs.push(bytes);
        }
    }
    let mut prev = parse_hex(prev);
    prev.reverse();
    let block = mine_txs(&prev.try_into().unwrap(), time, &txs);
    let mut hash = block.hash;
    hash.reverse();
    (block.hex, hex(&hash), txs.len())
}

/// AD-RP1c-1 (BL-701, RP1a D68): the process flag the RPC commands print the BLS fields under is
/// Core's after its startup rule, against dashd v24 on the same chains.
///
/// - **V19 at 1** (regtest's default): a fresh start is legacy, though the height rule at
///   genesis is basic; a restart whose coins are at genesis is basic; block 1 keeps basic, and
///   so does invalidating it.
/// - **V19 at 10:** a fresh start and block 1 are legacy; a restart at tip 8 is legacy and at
///   tip 9 basic. Then a height-10 block both nodes store and refuse to connect
///   (`bad-tx-type-dip3-inactive`) carries asset unlocks and MnHF signals with every kind of
///   signature bytes: getblock 2 and 3 and getspecialtxes 2 print them under the flag, basic
///   at tip 9 and legacy with 9 invalidated (tip 8), never under the stored block's own rule,
///   and print each payload object. `reconsiderblock` returns to basic.
///
/// decoderawtransaction prints the three process-flag fields with each kind of bytes at every
/// step. Transactions keep positive versions.
// Core: bls/bls.cpp:18, node/chainstate.cpp:413-433, validation.cpp:2547-2550, :2927,
// evo/specialtxman.cpp:1043-1047, evo/core_write.cpp:130, rpc/blockchain.cpp:180-195, :2540-2552
#[test]
fn ad_rp1c_1_bls_scheme_follows_cores_startup_rule() {
    let zeros = "0".repeat(192);
    let [_, (_, basic_sig), (_, legacy_sig), _] = bls_sig_kinds();

    let mut pair = Pair::start(&[]);
    let fresh = diff_chunks(&mut pair, &bls_field_calls("v19_1_fresh"));
    pair.restart();
    let restarted = diff_chunks(&mut pair, &bls_field_calls("v19_1_restarted_at_genesis"));
    pair.mock_now();
    pair.both("v19_1_mine_1", "generatetodescriptor", r#"[1, "raw(51)"]"#);
    let one = values_str(&pair.result("v19_1_mine_1"));
    let tip1 = diff_chunks(&mut pair, &bls_field_calls("v19_1_tip1"));
    pair.both("v19_1_invalidate_1", "invalidateblock", &format!(r#"["{one}"]"#));
    let invalidated = diff_chunks(&mut pair, &bls_field_calls("v19_1_invalidated_1"));
    pair.finish();
    assert_eq!(basic_unlock_sig(&fresh), zeros, "legacy after an empty start");
    for (step, results) in
        [("restart", &restarted), ("tip 1", &tip1), ("invalidated", &invalidated)]
    {
        assert_eq!(basic_unlock_sig(results), basic_sig, "{step}");
    }

    let mut pair = Pair::start(&["-testactivationheight=v19@10"]);
    pair.mock_now();
    pair.both("v19_10_mine_1", "generatetodescriptor", r#"[1, "raw(51)"]"#);
    let tip1 = diff_chunks(&mut pair, &bls_field_calls("v19_10_tip1"));
    pair.both("v19_10_mine_8", "generatetodescriptor", r#"[7, "raw(51)"]"#);
    pair.restart();
    let restarted8 = diff_chunks(&mut pair, &bls_field_calls("v19_10_restarted_at_8"));
    pair.mock_now();
    pair.both("v19_10_mine_9", "generatetodescriptor", r#"[1, "raw(51)"]"#);
    let nine = values_str(&pair.result("v19_10_mine_9"));
    pair.restart();
    let restarted9 = diff_chunks(&mut pair, &bls_field_calls("v19_10_restarted_at_9"));
    let now = pair.mock_now();

    // The stored candidate, after both restarts: rdashd keeps no refused block across one.
    let time = u32::try_from(now.as_secs() + 600).unwrap();
    let (candidate, hash, ntx) = bls_candidate(&nine, time);
    pair.both("v19_10_submit_candidate", "submitblock", &format!(r#"["{candidate}"]"#));
    let rejected = pair.result("v19_10_submit_candidate");
    assert_eq!(rejected.get_str().ok(), Some("bad-tx-type-dip3-inactive"), "{}", rejected.write());
    pair.both("v19_10_tip_after_submit", "getbestblockhash", "[]");
    assert_eq!(pair.result("v19_10_tip_after_submit").get_str().ok(), Some(nine.as_str()));
    let reads = |pair: &mut Pair, at: &str| {
        let calls = [
            ("getblock_2", "getblock", format!(r#"["{hash}", 2]"#)),
            ("getblock_3", "getblock", format!(r#"["{hash}", 3]"#)),
            ("getspecialtxes_2", "getspecialtxes", format!(r#"["{hash}", -1, {ntx}, 0, 2]"#)),
        ];
        calls.map(|(name, method, params)| {
            let name = format!("v19_10_candidate_{name}_{at}");
            pair.both(&name, method, &params);
            pair.result(&name)
        })
    };
    let at9 = reads(&mut pair, "tip9");
    pair.both("v19_10_invalidate_9", "invalidateblock", &format!(r#"["{nine}"]"#));
    let at8 = reads(&mut pair, "tip8");
    let tip8 = diff_chunks(&mut pair, &bls_field_calls("v19_10_tip8"));
    pair.both("v19_10_reconsider_9", "reconsiderblock", &format!(r#"["{nine}"]"#));
    pair.both("v19_10_tip_after_reconsider", "getbestblockhash", "[]");
    let reconsidered = diff_chunks(&mut pair, &bls_field_calls("v19_10_reconsidered"));
    let tip = pair.result("v19_10_tip_after_reconsider");
    pair.finish();
    assert_eq!(tip.get_str().ok(), Some(nine.as_str()), "the candidate is refused again");

    assert_eq!(basic_unlock_sig(&tip1), zeros, "block 1 below V19 at 10");
    assert_eq!(basic_unlock_sig(&restarted8), zeros, "restart at 8");
    assert_eq!(basic_unlock_sig(&restarted9), basic_sig, "restart at 9");
    assert_eq!(basic_unlock_sig(&tip8), zeros, "9 invalidated");
    assert_eq!(basic_unlock_sig(&reconsidered), basic_sig, "9 reconsidered");
    // Candidate transactions: the coinbase, then unlock and signal per kind (zeros, basic,
    // legacy, neither). The basic and legacy rows must print their payload objects.
    for (at, results, basic_prints, legacy_prints) in
        [("tip 9", &at9, &basic_sig, &zeros), ("tip 8", &at8, &zeros, &legacy_sig)]
    {
        for (read, result) in ["getblock 2", "getblock 3", "getspecialtxes 2"].iter().zip(results) {
            let txs = match result {
                UniValue::Arr(txs) => txs.clone(),
                block => field(block, "tx").get_values().unwrap().into_iter().cloned().collect(),
            };
            assert_eq!(txs.len(), ntx, "{at} {read}");
            for (row, prints) in [(3, basic_prints), (5, legacy_prints)] {
                let unlock = field(&txs[row], "assetUnlockTx");
                let sig = field(&unlock, "quorumSig");
                assert_eq!(sig.get_str().ok(), Some(prints.as_str()), "{at} {read} row {row}");
                let signal = field(&txs[row + 1], "mnhfTx");
                assert!(matches!(signal, UniValue::Obj(_)), "{at} {read} row {}", row + 1);
            }
        }
    }
}

/// `getchainlockbyheight` (BL-689a) on the recorded `feature_llmq_chainlocks` run, submitted to
/// both nodes: 44 carriers from 309, their certified heights 308 to 351 with gaps, during the
/// replay, at the tip for every height, and with the tip moved back and forth.
// Core: rpc/quorums.cpp:1404-1459; chainlock/clsig.cpp:126-246
#[test]
fn getchainlockbyheight_matches_dashd_v24() {
    const METHOD: &str = "getchainlockbyheight";
    let run = recorded_run("feature_llmq_chainlocks");
    assert_eq!(run.tip(), 352);
    let mut pair = Pair::start(&run.args());
    pair.both("help", "help", &format!(r#"["{METHOD}"]"#));
    let mut at = 0;
    for partial in [199, 309, 330, 347] {
        replay(&mut pair, &run, at + 1..=partial);
        at = partial;
        assert_tips(&pair, &run, partial);
        let from = i64::try_from(partial).unwrap() - 12;
        chainlocks_at(
            &mut pair,
            &format!("tip{partial}"),
            std::iter::once(0).chain(from..=from + 13),
        );
    }
    replay(&mut pair, &run, at + 1..=run.tip());
    assert_tips(&pair, &run, run.tip());
    let found = chainlocks_at(&mut pair, "tip352", (0..=353).chain([2_147_483_647]));
    let found_at = |h: usize| found[h] != UniValue::Null;
    assert!(!found_at(307) && found_at(308) && found_at(329) && !found_at(330));
    assert!(found_at(346) && found_at(351) && !found_at(352), "dashd's answers");
    assert_eq!(field(&found[346], "cbtx_height").write(), "348");
    for (name, params) in [
        ("none", "[]"),
        ("two", "[1, 2]"),
        ("null", "[null]"),
        ("string", r#"["100"]"#),
        ("bool", "[true]"),
        ("fraction", "[1.5]"),
        ("exponent", "[1e2]"),
        ("minus_zero", "[-0]"),
        ("minus_one", "[-1]"),
        ("big", "[2147483648]"),
        ("big_negative", "[-2147483649]"),
        ("named", r#"{"height":308}"#),
    ] {
        pair.both(&format!("arg_{name}"), METHOD, params);
    }
    pair.both("invalidate_352", "invalidateblock", &hash_param(&run, 352));
    assert_tips(&pair, &run, 351);
    chainlocks_at(&mut pair, "tip351", 340..=353);
    pair.both("invalidate_348", "invalidateblock", &hash_param(&run, 348));
    assert_tips(&pair, &run, 347);
    chainlocks_at(&mut pair, "tip347_again", 340..=353);
    pair.both("reconsider_348", "reconsiderblock", &hash_param(&run, 348));
    assert_tips(&pair, &run, 352);
    chainlocks_at(&mut pair, "reconsidered", 340..=353);
    pair.finish();
}

/// `getchainlockbyheight` at each of `heights`, named under `tag`, on both nodes in batches:
/// dashd's results.
fn chainlocks_at(
    pair: &mut Pair,
    tag: &str,
    heights: impl IntoIterator<Item = i64>,
) -> Vec<UniValue> {
    let calls: Vec<_> = heights
        .into_iter()
        .map(|h| (format!("{tag}_height_{h}"), "getchainlockbyheight", format!("[{h}]")))
        .collect();
    diff_chunks(pair, &calls)
}

/// The run's block at `height` as a one-hash parameter list.
fn hash_param(run: &RecordedRun, height: usize) -> String {
    format!(r#"["{}"]"#, run.hash(height))
}

/// The indexes the statuses calls ask for: each recorded unlock's index and its neighbours, 0
/// and the largest.
fn unlock_indexes(unlocks: &[(u64, usize, String)]) -> String {
    let mut indexes = std::collections::BTreeSet::from([0, u64::MAX]);
    for (index, _, _) in unlocks {
        indexes.extend([index - 1, *index, index + 1]);
    }
    let quoted: Vec<String> = indexes.iter().map(|i| format!(r#""{i}""#)).collect();
    format!("[{}]", quoted.join(","))
}

/// `getassetunlockstatuses` (BL-689a) on the recorded `feature_asset_locks` run: 19 unlocks from
/// 189, no ChainLock anywhere, so `mined` and `unknown` without a height and `chainlocked` with
/// one; on `feature_llmq_chainlocks`, whose tip coinbases carry ChainLocks and which has no
/// unlock, both nodes take the coinbase path; then the argument errors.
// Core: rpc/rawtransaction.cpp:781-903
#[test]
fn getassetunlockstatuses_matches_dashd_v24() {
    const METHOD: &str = "getassetunlockstatuses";
    let run = recorded_run("feature_asset_locks");
    let unlocks = run.asset_unlocks();
    assert_eq!(unlocks.len(), 19, "the run's unlocks");
    assert_eq!((unlocks[0].0, unlocks[0].1), (101, 189));
    assert_eq!(unlocks.last().map(|u| (u.0, u.1)), Some((806, 976)));
    let indexes = unlock_indexes(&unlocks);
    let mut pair = Pair::start(&run.args());
    pair.both("help", "help", &format!(r#"["{METHOD}"]"#));
    let mut at = 0;
    for tip in [188, 189, 212, 600, 978] {
        replay(&mut pair, &run, at + 1..=tip);
        at = tip;
        assert_tips(&pair, &run, tip);
        pair.both(&format!("tip{tip}"), METHOD, &format!("[{indexes}]"));
    }
    let mined = pair.result("tip978").write().matches(r#""status":"mined""#).count();
    assert_eq!(mined, 19, "{}", pair.result("tip978").write());

    let mut heights = std::collections::BTreeSet::from([0, 99, 100, 977, 978]);
    for (_, height, _) in &unlocks {
        heights.extend([height - 1, *height]);
    }
    let calls: Vec<_> = heights
        .iter()
        .map(|h| (format!("tip978_at_{h}"), METHOD, format!("[{indexes}, {h}]")))
        .collect();
    let at_heights = diff_chunks(&mut pair, &calls);
    let locked = |i: usize| at_heights[i].write().matches(r#""status":"chainlocked""#).count();
    assert_eq!(locked(heights.len() - 1), 19, "dashd's answer at 978");

    pair.both("invalidate_976", "invalidateblock", &hash_param(&run, 976));
    assert_tips(&pair, &run, 975);
    pair.both("invalidated_806_at_975", METHOD, r#"[["806"], 975]"#);
    pair.both("reconsider_976", "reconsiderblock", &hash_param(&run, 976));
    assert_tips(&pair, &run, 978);
    pair.both("reconsidered", METHOD, &format!("[{indexes}]"));

    for (name, params) in [
        ("max_indexes", format!("[[{}]]", vec![r#""1""#; 100].join(","))),
        ("too_many_indexes", format!("[[{}]]", vec![r#""1""#; 101].join(","))),
        ("height_minus_one", "[[], -1]".into()),
        ("height_past_tip", "[[], 979]".into()),
        ("height_tip", "[[], 978]".into()),
        ("number_index", "[[101]]".into()),
        ("number_after_string", r#"[["1", 2]]"#.into()),
        ("invalid_after_number", r#"[[2, "x"]]"#.into()),
        ("invalid_index", r#"[["x", 2]]"#.into()),
        ("invalid_index_bad_height", r#"[["x"], -1]"#.into()),
        ("fraction_height", r#"[["1"], 1.5]"#.into()),
        ("string_height", r#"[["1"], "5"]"#.into()),
        ("null_height", r#"[["1"], null]"#.into()),
        ("no_args", "[]".into()),
        ("null_indexes", "[null]".into()),
        ("object_indexes", r#"[{"1":2}]"#.into()),
        ("three_args", r#"[["1"], 5, 1]"#.into()),
        ("named", r#"{"indexes":["101"],"height":200}"#.into()),
        ("named_no_height", r#"{"indexes":["101"]}"#.into()),
    ] {
        pair.both(&format!("arg_{name}"), METHOD, &params);
    }
    let parsed = [
        "+5",
        "+-5",
        "++5",
        " 5",
        "5 ",
        "-0",
        "-1",
        "0x10",
        "1e3",
        "",
        "\\u0000",
        "18446744073709551615",
        "18446744073709551616",
        "9223372036854775808",
        "007",
    ];
    for (i, text) in parsed.iter().enumerate() {
        pair.both(&format!("parse_{i}"), METHOD, &format!(r#"[["{text}"]]"#));
    }
    pair.finish();

    let chainlocks = recorded_run("feature_llmq_chainlocks");
    let mut pair = Pair::start(&chainlocks.args());
    let mut at = 0;
    for tip in [199, 200, 308, 309, 352] {
        replay(&mut pair, &chainlocks, at + 1..=tip);
        at = tip;
        assert_tips(&pair, &chainlocks, tip);
        let ask = r#"["0", "1", "101", "18446744073709551615"]"#;
        let calls: Vec<_> =
            [String::new(), ", 0".into(), format!(", {}", tip - 1), format!(", {tip}")]
                .into_iter()
                .map(|h| (format!("chainlocks_tip{tip}{h}"), METHOD, format!("[{ask}{h}]")))
                .collect();
        for answer in diff_chunks(&mut pair, &calls) {
            assert_eq!(answer.write().matches(r#""status":"unknown""#).count(), 4);
        }
    }
    pair.finish();
}

/// `getassetunlockstatuses` on one node: its `result`, or its error object, written.
fn statuses_on(port: u16, params: &str) -> String {
    let reply = reply_on(port, "getassetunlockstatuses", params);
    match field(&reply, "result") {
        UniValue::Null => field(&reply, "error").write(),
        result => result.write(),
    }
}

/// ALLOWED-DIFFS AD-BL689a-1 (closers MP1a, LK1b): rdashd has no mempool, so an index whose
/// unlock is only in dashd's mempool is `mempooled` there and `unknown` on rdashd, with no
/// `instantlock` key; with a height, and once the unlock is mined, both agree. The unlocks are the
/// run's own: index 101's sent to dashd at 188, and index 806's back in dashd's mempool after its
/// block is disconnected.
// Core: rpc/rawtransaction.cpp:878-898; validation.cpp:441-459
#[test]
fn ad_bl689a_1_mempooled_index_is_unknown_on_rdashd() {
    let run = recorded_run("feature_asset_locks");
    let unlocks = run.asset_unlocks();
    let (index, height, tx) = unlocks.first().expect("an unlock").clone();
    assert_eq!((index, height), (101, 189));
    let mut pair = Pair::start(&run.args());
    let [core, ours] = pair.ports;
    replay(&mut pair, &run, 1..=188);
    assert_tips(&pair, &run, 188);

    let sent = reply_on(core, "sendrawtransaction", &format!(r#"["{tx}"]"#));
    let txid = field(&sent, "result");
    assert_eq!(txid.get_str().ok(), Some(&*rpc_txid(&tx)), "dashd refused: {}", sent.write());
    let ask = r#"[["101"]]"#;
    let mempooled = r#"[{"index":101,"status":"mempooled","instantlock":false}]"#;
    assert_eq!(statuses_on(core, ask), mempooled);
    assert_eq!(statuses_on(ours, ask), r#"[{"index":101,"status":"unknown"}]"#);
    pair.both("pooled_at_188", "getassetunlockstatuses", r#"[["101"], 188]"#);
    assert_eq!(pair.result("pooled_at_188").write(), r#"[{"index":101,"status":"unknown"}]"#);
    replay(&mut pair, &run, 189..=189);
    pair.both("mined_101", "getassetunlockstatuses", ask);
    assert_eq!(pair.result("mined_101").write(), r#"[{"index":101,"status":"mined"}]"#);

    replay(&mut pair, &run, 190..=run.tip());
    assert_tips(&pair, &run, run.tip());
    pair.both("invalidate_976", "invalidateblock", &hash_param(&run, 976));
    assert_tips(&pair, &run, 975);
    let ask = r#"[["806"]]"#;
    let mempooled = r#"[{"index":806,"status":"mempooled","instantlock":false}]"#;
    assert_eq!(statuses_on(core, ask), mempooled);
    assert_eq!(statuses_on(ours, ask), r#"[{"index":806,"status":"unknown"}]"#);
    pair.both("pooled_at_975", "getassetunlockstatuses", r#"[["806"], 975]"#);
    assert_eq!(pair.result("pooled_at_975").write(), r#"[{"index":806,"status":"unknown"}]"#);
    pair.both("reconsider_976", "reconsiderblock", &hash_param(&run, 976));
    assert_tips(&pair, &run, run.tip());
    pair.both("mined_806", "getassetunlockstatuses", ask);
    assert_eq!(pair.result("mined_806").write(), r#"[{"index":806,"status":"mined"}]"#);
    pair.finish();
}
