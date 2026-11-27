//! T6 OK-3 D1, the old side: base-sdk's fork at 29e01c83, before the re-pin, read the way the node
//! read it then (its decoder, its payload's own BLS fields, its sign-hash helpers and node-l1's
//! Core image). Reads the rows `t6-probe gen` wrote and prints one JSON line per row, which
//! `t6-probe compare` sets against the node at the rc.
//!
//! `t6-probe-old <rows.tsv> > old.jsonl`

use std::io::{BufRead, BufWriter, Write};

use dash_params::Network;
use dash_primitives::__private::bitcoin_hashes::sha256d;
use dash_primitives::{NetInfo, PayloadDecode, SpecialPayload, Transaction, TxType};
use dash_types::codec::BaseCodec;
use dash_types::{Numeric, Signable};
use serde_json::{Value, json};

mod bls {
    include!("../../common/bls.rs");
}

use bls::hx;

const NETWORKS: [(&str, Network); 3] =
    [("mainnet", Network::Main), ("testnet", Network::Testnet3), ("regtest", Network::Regtest)];

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn hash(bytes: &[u8]) -> String {
    hx(sha256d::Hash::hash(bytes).as_byte_array())
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// node-l1 `payload.rs` `core_bytes` at be8167b657 for the provider payloads: base-sdk's encoding,
/// with the raw bytes of an operator key that does not resolve (`patch_lazy_key`).
fn core_image(payload: &SpecialPayload) -> Option<Vec<u8>> {
    fn patch_lazy_key(out: &mut [u8], offset: usize, key: &dash_pkc::bls::BlsLazyPk) {
        let raw = key.to_bytes();
        if !key.clone().get().is_valid()
            && let Some(dst) = out.get_mut(offset..offset + raw.len())
        {
            dst.copy_from_slice(&raw);
        }
    }
    Some(match payload {
        SpecialPayload::ProviderRegister(p) => {
            let mut out = encode(p);
            let net_info = match p.net_info() {
                NetInfo::Legacy(svc) => encode(svc).len(),
                NetInfo::Extended(ext) => encode(ext).len(),
            };
            patch_lazy_key(&mut out, 2 + 2 + 2 + 32 + 4 + net_info + 20, p.pub_key_operator());
            out
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            let mut out = encode(p);
            patch_lazy_key(&mut out, 2 + 32 + 2, p.pub_key_operator());
            out
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            let mut out = encode(p);
            patch_lazy_key(&mut out, 2 + 32, p.pub_key_operator());
            out
        }
        SpecialPayload::ProviderUpdateService(p) => encode(p),
        SpecialPayload::ProviderUpdateRevoke(p) => encode(p),
        _ => return None,
    })
}

fn row(tx_type: u16, payload: &[u8], tx: Option<&Transaction>) -> Value {
    let mut cursor = payload;
    let decoded = SpecialPayload::decode(TxType::from_base(tx_type), &mut cursor);
    let accepted = tx.map(|tx| {
        tx.decode_payload().and_then(Result::ok).filter(PayloadDecode::is_complete).is_some()
    });
    let p = match decoded {
        Err(e) => return json!({"verdict": {"err": e.to_string()}, "accepted": accepted}),
        Ok(p) => p,
    };
    let verdict = json!({
        "ok": true,
        "consumed": payload.len() - cursor.len(),
        "complete": cursor.is_empty(),
    });
    let mut out = json!({"verdict": verdict, "accepted": accepted});
    let key = match &p {
        SpecialPayload::ProviderRegister(p) => Some(p.pub_key_operator()),
        SpecialPayload::ProviderUpdateRegistrar(p) => Some(p.pub_key_operator()),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => Some(p.pub_key_operator()),
        _ => None,
    };
    let sig = match &p {
        SpecialPayload::ProviderUpdateService(p) => Some(p.sig()),
        SpecialPayload::ProviderUpdateRevoke(p) => Some(p.sig()),
        _ => None,
    };
    out["key"] = key.map_or(Value::Null, bls::key);
    out["sig"] = sig.map_or(Value::Null, bls::sig);
    let mut hashes = serde_json::Map::new();
    match &p {
        SpecialPayload::ProviderRegister(p) => {
            hashes.insert("sig_hash".into(), json!(hash(&p.sig_hash_bytes())));
            for (name, network) in NETWORKS {
                let addr = network.addr();
                hashes.insert(format!("sign_string_{name}"), json!(p.sign_string(addr)));
                hashes.insert(format!("message_hash_{name}"), json!(hx(&p.payload(addr))));
            }
            if let Some(tx) = tx {
                hashes.insert("consent".into(), json!(hash(&p.shared_consent_bytes(tx))));
            }
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            hashes.insert("sig_hash".into(), json!(hash(&p.sig_hash_bytes())));
        }
        SpecialPayload::ProviderUpdateService(p) => {
            hashes.insert("sig_hash".into(), json!(hash(&p.sig_hash_bytes())));
        }
        SpecialPayload::ProviderUpdateRevoke(p) => {
            hashes.insert("sig_hash".into(), json!(hash(&p.sig_hash_bytes())));
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            hashes.insert("sig_hash".into(), json!(hash(&p.sig_hash_bytes())));
        }
        _ => {}
    }
    out["hashes"] = Value::Object(hashes);
    out["core_image"] = core_image(&p).map_or(Value::Null, |b| json!(hx(&b)));
    out
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: t6-probe-old <rows.tsv>");
    let rows = std::io::BufReader::new(std::fs::File::open(path).expect("rows"));
    let mut out = BufWriter::new(std::io::stdout().lock());
    for line in rows.lines() {
        let line = line.expect("a row");
        let mut fields = line.split('\t');
        let (id, tx_type, payload, tx) = (
            fields.next().expect("id"),
            fields.next().expect("type").parse::<u16>().expect("a type"),
            unhex(fields.next().expect("payload")),
            fields.next().expect("tx"),
        );
        let tx = (tx != "-").then(|| unhex(tx));
        let tx = tx.and_then(|raw| Transaction::decode(&mut &raw[..]).ok());
        let value = row(tx_type, &payload, tx.as_ref());
        writeln!(out, "{}", json!({"id": id, "v": value})).expect("stdout");
    }
}
