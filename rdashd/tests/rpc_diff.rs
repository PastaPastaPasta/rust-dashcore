//! DIFF-RPC (RP0): the RPC server's bytes against dashd v24.0.0-rc.4's. The same raw requests go
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
use dash_consensus::merkle::compute_merkle_root;

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
/// the comparison includes (`<closed>`): a kept-alive connection stays quiet.
const CLOSE_WATCH: Duration = Duration::from_secs(1);

/// The reply bytes to `request`: the header block with its `Date` header's value masked, the body
/// read through `Content-Length` (libevent sends a body to `HEAD` too), or to the close when no
/// length is sent, then `<closed>` if the server closes within [`CLOSE_WATCH`]. A reply that ends
/// early, a read error, or more bytes after the body fail the test; no bytes at all, then a close,
/// is `<closed>`.
fn exchange(port: u16, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    stream.write_all(request).unwrap();
    let name = String::from_utf8_lossy(&request[..request.len().min(40)]).into_owned();
    let read = |stream: &mut TcpStream, buf: &mut [u8]| {
        read_retrying(stream, buf).unwrap_or_else(|e| panic!("{name:?}: reading the reply: {e}"))
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
        stream.set_read_timeout(Some(CLOSE_WATCH)).unwrap();
        closed = match read_retrying(&mut stream, &mut buf) {
            Ok(0) => true,
            Ok(n) => panic!("{name:?}: {n} bytes after the reply"),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => false,
            Err(e) => panic!("{name:?}: watching for the close: {e}"),
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
            (theirs.join().unwrap(), mine.join().unwrap())
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
        let (merkle, _) = compute_merkle_root(txs.iter().map(|tx| txid(tx)).collect());
        blocks.push(mine(&blocks[99].hash, GENESIS_TIME + 60 * 101, txs, Some(merkle), true));
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

/// `[<hash>]`, its bytes reversed as RPC shows them, then `rest`.
fn hash_params(hash: &[u8; 32], rest: &str) -> String {
    let mut shown = *hash;
    shown.reverse();
    format!(r#"["{}"{rest}]"#, hex(&shown))
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
/// the tip whose transactions after its plain coinbase are the special transactions of
/// dashd-rpc's txtouniv fixtures verbatim, the crafted ones included. Below DIP0003
/// `CheckBlock` and `ContextualCheckBlock` accept any special type and payload, so the block is
/// written to disk; `ConnectBlock` refuses it (`bad-tx-type-dip3-inactive`). getblock 1, 2 and 3
/// print it with every payload object, without undo data (n-3), before and after both nodes
/// restart with `-deprecatedrpc=service`. A type-5 coinbase would stop rdashd instead (D18's
/// refusal of a special-version coinbase; BL-516), so the block-level `cbTx` waits for G1 (D42).
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
    both("submitblock_stored", "submitblock", &format!(r#"["{stored}"]"#));
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
    let service3 = &replies["getblock_stored_service_3"];
    assert!(service3.contains(r#""service":"#), "{service3}");
    assert!(service3.contains(r#""platformHTTPPort":0"#), "{service3}");
}

/// A block at `height` on `prev` (as RPC shows it) that `CheckBlock` accepts below DIP0003 and
/// `ConnectBlock` refuses: a plain coinbase, then [`stored_block_payloads`]. Returns its hex and
/// its hash as RPC shows it.
fn unconnectable_block(prev: &str, height: u8, time: u32) -> (String, String) {
    let mut txs = vec![coinbase(height, 1)];
    txs.extend(stored_block_payloads());
    let (merkle, _) = compute_merkle_root(txs.iter().map(|tx| txid(tx)).collect());
    let mut prev = parse_hex(prev);
    prev.reverse();
    let block = mine(&prev.try_into().unwrap(), time, &txs, Some(merkle), true);
    let mut hash = block.hash;
    hash.reverse();
    (block.hex, hex(&hash))
}

/// The secret key 1 (`GENERATOR_KEY`'s), as a regtest WIF.
const GENERATOR_WIF: &str = "cMahea7zqjxrtgAbB7LSGbcQUr1uX1ojuat9jZodMN87JcbXMTcA";

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
/// and last on a fresh pair of nodes with `-deprecatedrpc=softforks`: `testdummy` through its
/// first two windows, then a header-only tip from `submitheader`.
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

    // `softforks` under `-deprecatedrpc=softforks`, on a second pair of fresh nodes: rdashd does
    // not keep generated blocks across a restart yet (BL-504, RP1a D36), so these
    // nodes mine their own.
    let (core, core_port) =
        start(&dashd, "core_softforks", &["-disablewallet", "-deprecatedrpc=softforks"]);
    let (ours, our_port) =
        start(Path::new(RDASHD), "ours_softforks", &["-deprecatedrpc=softforks"]);
    let ports = [core_port, our_port];
    both(ports, "softforks_setmocktime", "setmocktime", &format!("[{}]", now.as_secs()));
    both(ports, "info_softforks_genesis", "getblockchaininfo", "[]");
    both(ports, "softforks_mine", "generatetodescriptor", r#"[3, "raw(51)"]"#);
    both(ports, "info_softforks", "getblockchaininfo", "[]");
    // `testdummy` across its first two 144-block windows: DEFINED to STARTED at 144, then
    // LOCKED_IN at 288 as every block signals; below DIP0003's height 432.
    let mut height = 3;
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
