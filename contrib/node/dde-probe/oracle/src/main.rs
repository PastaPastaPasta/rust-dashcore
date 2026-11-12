//! Transaction decode-domain cases through Dash Core (both pins, through the
//! O1 oracle), base-sdk at 33fcc59 and base-sdk with the BS4a changes.
//!
//! Prints one JSON line per case. `BSDK_ROOT` points at a base-sdk checkout
//! for the corpus files (default ~/workspace/base-sdk).

use dash_oracle_sys::{Oracle, Pin, Verdict};
use serde_json::{json, Value};

fn core(bytes: &[u8]) -> Value {
    let mut out = serde_json::Map::new();
    for pin in Pin::ALL {
        let o = Oracle::new(pin);
        let dec = match o.tx_decode(bytes) {
            Ok(d) => json!({"ok": true, "consumed": d.consumed, "len": bytes.len()}),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        };
        let chk = match o.check_transaction(bytes) {
            Ok(Verdict::Accept) => json!("accept"),
            Ok(Verdict::Reject(r)) => json!(r.reason),
            Err(e) => json!(format!("error: {e}")),
        };
        out.insert(pin.name().into(), json!({"decode": dec, "check": chk}));
    }
    Value::Object(out)
}

fn old(bytes: &[u8]) -> Value {
    use old_types::codec::BaseCodec;
    let mut c = bytes;
    match old_prim::Transaction::decode(&mut c) {
        Ok(_) => json!({"ok": true, "consumed": bytes.len() - c.len()}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

fn new(bytes: &[u8]) -> Value {
    use new_types::codec::BaseCodec;
    use new_types::Checkable;
    let mut c = bytes;
    match new_prim::Transaction::decode(&mut c) {
        Ok(tx) => json!({
            "ok": true,
            "consumed": bytes.len() - c.len(),
            "check": tx.check().map_or("accept".to_string(), |e| e.to_string()),
        }),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

fn case(name: &str, bytes: &[u8]) {
    let shown = if bytes.len() <= 512 { hex::encode(bytes) } else { format!("<{} bytes>", bytes.len()) };
    let v = json!({"case": name, "hex": shown, "core": core(bytes), "bsdk_33fcc59": old(bytes), "bsdk_bs4a": new(bytes)});
    println!("{v}");
}

fn h(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

/// Rebuilds a special tx with `extra` bytes appended to its payload.
fn pad_payload(tx: &[u8], payload_at: usize, payload_len: usize, extra: usize) -> Vec<u8> {
    let new_len = payload_len + extra;
    let mut out = tx[..payload_at].to_vec();
    compact(&mut out, new_len as u64);
    let prefix = compact_len(payload_len as u64);
    out.extend_from_slice(&tx[payload_at + prefix..payload_at + prefix + payload_len]);
    out.resize(out.len() + extra, 0);
    out
}

fn compact(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes())
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes())
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes())
        }
    }
}

fn compact_len(n: u64) -> usize {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

fn main() {
    let which = std::env::args().nth(1).unwrap_or_default();
    if which.is_empty() || which == "tx" {
        tx_cases();
    }
}

fn tx_cases() {
    // Dash Core src/test/data/tx_invalid.json, "Tests for CheckTransaction()".
    case("tx_invalid:negative_output", &h("01000000010001000000000000000000000000000000000000000000000000000000000000000000006d4830450220063222cbb128731fc09de0d7323746539166544d6c1df84d867ccea84bcc8903022100bf568e8552844de664cd41648a031554327aa8844af34b4f27397c65b92c04de0123210243ec37dee0e2e053a9c976f43147e79bc7d9dc606ea51010af1ac80db6b069e1acffffffff01ffffffffffffffff015100000000"));
    case("tx_invalid:max_money_plus_1", &h("01000000010001000000000000000000000000000000000000000000000000000000000000000000006e493046022100e1eadba00d9296c743cb6ecc703fd9ddc9b3cd12906176a226ae4c18d6b00796022100a71aef7d2874deff681ba6080f1b278bac7bb99c61b08a85f4311970ffe7f63f012321030c0588dc44d92bdcbf8e72093466766fdc265ead8db64517b0c542275b70fffbacffffffff010140075af0750700015100000000"));
    case("tx_invalid:max_money_output_plus_1_output", &h("01000000010001000000000000000000000000000000000000000000000000000000000000000000006d483045022027deccc14aa6668e78a8c9da3484fbcd4f9dcc9bb7d1b85146314b21b9ae4d86022100d0b43dece8cfb07348de0ca8bc5b86276fa88f7f2138381128b7c36ab2e42264012321029bb13463ddd5d2cc05da6e84e37536cb9525703cfd8f43afdb414988987a92f6acffffffff020040075af075070001510001000000000000015100000000"));

    // Mainnet CbTx ac9fc5d0..., block 000000000000003bb555..., base-sdk corpus cbtx.json5.
    let cbtx = cbtx_bytes();
    let (at, len) = payload_span(&cbtx);
    case("cbtx:mainnet_ac9fc5d0", &cbtx);
    for total in [10_000usize, 10_001, 10_240, 10_241, 1_000_000, 0x0200_0000] {
        case(&format!("cbtx:payload_len_{total}"), &pad_payload(&cbtx, at, len, total - len));
    }
    // A payload length prefix above MAX_SIZE with enough bytes behind it.
    let max_size = 0x0200_0000usize;
    case("cbtx:payload_len_max_size_plus_1", &pad_payload(&cbtx, at, len, max_size + 1 - len));
}

fn cbtx_bytes() -> Vec<u8> {
    let root = std::env::var("BSDK_ROOT").unwrap_or_else(|_| "/work/home/agent/workspace/base-sdk".into());
    let corpus = std::fs::read_to_string(format!("{root}/pkgs/primitives/corpus/cbtx.json5")).expect("corpus");
    let key = "\"ac9fc5d0c7229a1c9fb834d8c6f6be200ae5b55d1b731c38afd83adc0c54aa31\"";
    let rest = &corpus[corpus.find(key).expect("txid")..];
    let raw = &rest[rest.find("\"raw\": \"").expect("raw") + 8..];
    h(&raw[..raw.find('"').expect("end")])
}

/// Offset and length of the extra payload (the tx's last field).
fn payload_span(tx: &[u8]) -> (usize, usize) {
    use new_types::codec::BaseCodec;
    let mut c = tx;
    let decoded = new_prim::Transaction::decode(&mut c).expect("cbtx");
    let len = decoded.extra_payload.len();
    (tx.len() - len - compact_len(len as u64), len)
}
