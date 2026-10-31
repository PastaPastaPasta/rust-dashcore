//! BS7 differential helpers (see ../README.md).
//!
//! - `mine`: reads `{"prev": HEX80, "versions": [i32], "start_time": u32,
//!   "salt": u64}` and prints regtest headers (bits 0x207fffff) extending
//!   `prev`, one per version; `salt` makes sibling forks differ.
//! - `predict`: reads JSON lines `{"id", "cmd", "payload": HEX, "version"}` and
//!   prints base-sdk's verdict for each: `P2pMsg::decode_with_policy` under that
//!   common version, the bytes it reads, and for `headers`/`headers2` the hash
//!   and previous hash of every decoded header (Core's display order).
//! - `roundtrip`: reads JSON lines `{"id", "cmd", "payload": HEX, "version"}`
//!   and prints whether the decoded message re-encodes to the payload it read;
//!   for `headers2`, whether `Headers2::reply`/`announce` of the decoded
//!   headers give the same bytes, the count, the distinct versions and the
//!   first and last hash.
//! - `frame`: reads JSON lines `{"id", "magic": HEX4, "frame": HEX}` (a V1
//!   header and as much payload as was sent) and prints base-sdk's V1 verdict:
//!   `disconnect` or `drop` with the reason, or `deliver`.
//! - `fuzz-tx`/`fuzz-block SEED_FILE SECONDS RNG_SEED` (feature `oracle`):
//!   mutates the hex payloads of SEED_FILE (one per line) and compares the `tx`
//!   or `block` handler read, base-sdk against the O1 oracle at the v24 pin.

use std::io::{BufRead, Write};

use dash_p2p_core::{
    CommandString, Magic, P2pDecodeError, P2pMsg, ProtoCtx, ProtocolVersion, V1FrameError, V1Header,
};
use dash_pkc::bls::BlsCtx;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Hashable;
use dash_types::codec::BaseCodec;
use serde_json::{Value, json};

fn command(name: &str) -> CommandString {
    let mut bytes = [0u8; 12];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    CommandString::from_bytes(bytes)
}

/// Core's display order: the hash bytes reversed.
fn display(hash: &BlockHash) -> String {
    let mut buf = Vec::new();
    hash.encode(&mut buf);
    buf.reverse();
    hex::encode(buf)
}

fn header_hex(h: &BlockHeader) -> String {
    let mut buf = Vec::new();
    h.encode(&mut buf);
    hex::encode(buf)
}

fn error_kind(e: &P2pDecodeError) -> &'static str {
    match e {
        P2pDecodeError::TooManyEntries { .. } => "too_many",
        P2pDecodeError::PayloadTooLarge { .. } => "too_large",
        P2pDecodeError::InvalidCommand { .. } => "invalid_command",
        _ => "throw",
    }
}

mod class;

fn headers_of(msg: &P2pMsg) -> Option<Vec<BlockHeader>> {
    match msg {
        P2pMsg::Headers(h) => Some(h.headers.clone()),
        P2pMsg::Headers2(h) => Some(h.block_headers().copied().collect()),
        _ => None,
    }
}

fn predict_one(v: &Value) -> Value {
    let cmd = v["cmd"].as_str().unwrap_or_default();
    let payload = hex::decode(v["payload"].as_str().unwrap_or_default()).unwrap_or_default();
    let version = v["version"].as_i64().unwrap_or(70240) as i32;
    let ctx = ProtoCtx::new(ProtocolVersion::new(version), BlsCtx::BASIC);
    let mut out = json!({"id": v["id"].clone(), "cmd": cmd, "len": payload.len()});
    match P2pMsg::decode_with_policy(&command(cmd), &payload, &ctx) {
        Ok(d) => {
            out["ok"] = json!(true);
            out["consumed"] = json!(d.consumed);
            out["kind"] = json!(match &d.msg {
                P2pMsg::Unknown { .. } => "unknown",
                _ => "decoded",
            });
            if let Some(hs) = headers_of(&d.msg) {
                out["hashes"] = json!(hs.iter().map(|h| display(&h.hash())).collect::<Vec<_>>());
                out["prevs"] = json!(hs.iter().map(|h| display(&h.prev_hash)).collect::<Vec<_>>());
            }
            if v["want_msg"].as_bool().unwrap_or(false) {
                out["msg"] = serde_json::to_value(&d.msg).unwrap_or(Value::Null);
            }
        }
        Err(e) => {
            out["ok"] = json!(false);
            out["kind"] = json!(error_kind(&e));
            out["class"] = json!(class::ours(cmd, &e));
            out["error"] = json!(e.to_string());
        }
    }
    out
}

fn roundtrip_one(v: &Value) -> Value {
    let cmd = v["cmd"].as_str().unwrap_or_default();
    let payload = hex::decode(v["payload"].as_str().unwrap_or_default()).unwrap_or_default();
    let version = v["version"].as_i64().unwrap_or(70240) as i32;
    let ctx = ProtoCtx::new(ProtocolVersion::new(version), BlsCtx::BASIC);
    let mut out = json!({"id": v["id"].clone(), "cmd": cmd, "len": payload.len()});
    match P2pMsg::decode_with_policy(&command(cmd), &payload, &ctx) {
        Ok(d) => {
            let mut re = Vec::new();
            d.msg.encode_payload(&mut re);
            out["ok"] = json!(true);
            out["consumed"] = json!(d.consumed);
            out["kind"] = json!(if d.msg.is_stub() {
                "stub"
            } else if d.msg.is_unknown() {
                "unknown"
            } else {
                "typed"
            });
            out["reencodes"] = json!(re == payload[..d.consumed]);
            if let P2pMsg::Headers2(h) = &d.msg {
                let headers: Vec<BlockHeader> = h.block_headers().copied().collect();
                let mut reply = Vec::new();
                dash_p2p_core::Headers2::reply(&headers).encode(&mut reply);
                let mut announce = Vec::new();
                dash_p2p_core::Headers2::announce(&headers).encode(&mut announce);
                let mut versions: Vec<i32> = headers.iter().map(|h| h.version).collect();
                versions.sort_unstable();
                versions.dedup();
                out["count"] = json!(headers.len());
                out["reply_mode"] = json!(reply == payload[..d.consumed]);
                out["announce_mode"] = json!(announce == payload[..d.consumed]);
                out["versions"] = json!(versions.len());
                out["first_prev"] = json!(headers.first().map(|h| display(&h.prev_hash)));
                out["first"] = json!(headers.first().map(|h| display(&h.hash())));
                out["last"] = json!(headers.last().map(|h| display(&h.hash())));
            }
        }
        Err(e) => {
            out["ok"] = json!(false);
            out["error"] = json!(e.to_string());
        }
    }
    out
}

fn frame_one(v: &Value) -> Value {
    let magic = hex::decode(v["magic"].as_str().unwrap_or_default()).unwrap_or_default();
    let bytes = hex::decode(v["frame"].as_str().unwrap_or_default()).unwrap_or_default();
    let mut out = json!({"id": v["id"].clone()});
    let (Ok(magic), Some(header)) = (<[u8; 4]>::try_from(magic.as_slice()), bytes.first_chunk())
    else {
        out["verdict"] = json!("bad input");
        return out;
    };
    let header = V1Header::from_bytes(header);
    let verdict = header
        .check(Magic::from_bytes(magic))
        .and_then(|()| header.check_payload(&bytes[V1Header::SIZE..]));
    out["verdict"] = json!(match verdict {
        Ok(()) => "deliver".to_owned(),
        Err(e @ (V1FrameError::WrongMagic { .. } | V1FrameError::TooLarge { .. })) =>
            format!("disconnect: {e}"),
        Err(e) => format!("drop: {e}"),
    });
    out
}

/// Regtest's target for bits 0x207fffff, big-endian: 7fffff then zeros.
fn meets_regtest_target(h: &BlockHeader) -> bool {
    let mut buf = Vec::new();
    h.hash().encode(&mut buf);
    buf.reverse();
    let mut target = [0u8; 32];
    target[..3].copy_from_slice(&[0x7f, 0xff, 0xff]);
    buf.as_slice() <= target.as_slice()
}

fn mine(v: &Value) -> Value {
    let prev_bytes = hex::decode(v["prev"].as_str().unwrap_or_default()).unwrap_or_default();
    let mut prev = BlockHeader::decode(&mut prev_bytes.as_slice()).expect("prev header");
    let mut time = v["start_time"].as_u64().unwrap_or(u64::from(prev.time) + 1) as u32;
    let mut out = Vec::new();
    for (i, version) in v["versions"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        let mut h = BlockHeader {
            version: version.as_i64().unwrap_or(0x2000_0000) as i32,
            prev_hash: prev.hash(),
            merkle_root: BlockHeader::default().merkle_root,
            time,
            bits: 0x207f_ffff,
            nonce: 0,
        };
        // A distinct merkle root per header keeps sibling forks apart.
        let mut root = [0u8; 32];
        root[..8].copy_from_slice(&(v["salt"].as_u64().unwrap_or(0) ^ (i as u64)).to_le_bytes());
        h.merkle_root = BaseCodec::decode(&mut &root[..]).expect("root");
        while !meets_regtest_target(&h) {
            h.nonce += 1;
        }
        out.push(json!(header_hex(&h)));
        prev = h;
        time += 1 + (i as u32 % 3);
    }
    json!(out)
}

/// Answers each JSON line on stdin with `f`'s JSON line.
fn serve(stdin: &std::io::Stdin, stdout: &mut impl Write, f: fn(&Value) -> Value) {
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).expect("json line");
        writeln!(stdout, "{}", f(&v)).expect("stdout");
        stdout.flush().expect("flush");
    }
}

#[cfg(feature = "oracle")]
mod fuzz;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    match args.get(1).map(String::as_str) {
        Some("predict") => serve(&stdin, &mut stdout, predict_one),
        Some("roundtrip") => serve(&stdin, &mut stdout, roundtrip_one),
        Some("frame") => serve(&stdin, &mut stdout, frame_one),
        Some("mine") => {
            let v: Value = serde_json::from_reader(stdin.lock()).expect("json");
            writeln!(stdout, "{}", mine(&v)).expect("stdout");
        }
        #[cfg(feature = "oracle")]
        Some(target @ ("fuzz-tx" | "fuzz-block")) => fuzz::run(target, &args[2..]),
        _ => {
            eprintln!("usage: bs7-tool predict|roundtrip|frame|mine|fuzz-tx|fuzz-block");
            std::process::exit(2);
        }
    }
}
