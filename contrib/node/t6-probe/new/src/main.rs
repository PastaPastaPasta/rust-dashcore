//! T6 OK-3 D1, the new side: the node at OK-3's head (dash-consensus's `CorePayload` side-car,
//! dash-dml's sign-hash builders, node-l1's Core image) on the rc base-sdk pin. See README.md.
//!
//! - `t6-probe gen <rows.tsv> <values.json> <input>...`: the input payloads, mutated, as rows.
//!   An input is `scan:<file>` (hex runs decoding as transactions or blocks), `blk:<file>` (a
//!   Core block file), `rows:<file>` (`id\ttype\tpayload\ttx` lines) or `crafted:<file>` (base-sdk's
//!   `payload_core.json5` as JSON, rows kept unmutated as `crafted:<name>`).
//! - `t6-probe eval <rows.tsv>`: one JSON line per row, as `t6-probe-old` prints them.
//! - `t6-probe compare <old.jsonl> <new.jsonl> <crafted.json>`: one JSON line per row with both
//!   sides, the OK3-CARRY item 5 checks, and a summary line.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufWriter, Write};

use dash_consensus::primitives::{CorePayload, Tx, protx_key_scheme};
use dash_dml::specialtx::sig::{
    make_sign_string_with_key, payload_sig_hash, payload_sig_hash_with_key,
    pro_reg_message_hash_with_key, shared_reg_consent_hash_with_key,
};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::{
    BLS_SIG_LEN, BlsEagerSig, BlsLazyPk, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey,
    BlsSigId,
};
use dash_primitives::{Block, SpecialPayload, Transaction, TxType};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::{Value, json};

mod bls {
    include!("../../common/bls.rs");
}

use bls::hx;

const PROVIDER: [u16; 5] = [1, 2, 3, 4, 12];
const LEGACY: BlsSchemeKind = BlsSchemeKind::Legacy;
const BASIC: BlsSchemeKind = BlsSchemeKind::Basic;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn encode<E>(value: &impl BaseCodec<E>) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn compact_size(n: usize) -> Vec<u8> {
    match n {
        0..=252 => vec![n as u8],
        253..=0xffff => [&[253][..], &(n as u16).to_le_bytes()].concat(),
        _ => [&[254][..], &(n as u32).to_le_bytes()].concat(),
    }
}

// ---------------------------------------------------------------------------------------------
// gen

/// An input payload: its source, type, payload bytes and carrying transaction.
#[derive(Clone)]
struct Input {
    id: String,
    tx_type: u16,
    payload: Vec<u8>,
    tx: Vec<u8>,
}

/// A version-3 transaction of `tx_type` carrying `payload`: one input, no output.
fn carrier(tx_type: u16, payload: &[u8]) -> Vec<u8> {
    let mut tx = Vec::new();
    tx.extend(3u16.to_le_bytes());
    tx.extend(tx_type.to_le_bytes());
    tx.push(1);
    tx.extend([9; 32]);
    tx.extend(0u32.to_le_bytes());
    tx.push(0);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(0);
    tx.extend(0u32.to_le_bytes());
    tx.extend(compact_size(payload.len()));
    tx.extend(payload);
    tx
}

/// `tx` with its extra payload, the last field, replaced by `payload`; `None` when `tx` does not
/// end with `old`.
fn with_payload(tx: &[u8], old: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
    let tail = [compact_size(old.len()), old.to_vec()].concat();
    let head = tx.strip_suffix(&tail[..])?;
    Some([head, &compact_size(payload.len()), payload].concat())
}

fn special(tx: &Transaction, raw: Vec<u8>, id: String) -> Option<Input> {
    let tx_type = tx.tx_type().to_base();
    PROVIDER.contains(&tx_type).then(|| Input {
        id,
        tx_type,
        payload: tx.extra_payload().clone(),
        tx: raw,
    })
}

/// The provider transactions in the hex runs of `text`, each run read whole as a transaction or a
/// block.
fn scan(source: &str, text: &str, out: &mut Vec<Input>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        let run = std::str::from_utf8(&bytes[start..i]).expect("hex digits are ASCII");
        i += 1;
        if run.len() < 120 || !run.len().is_multiple_of(2) {
            continue;
        }
        let raw = unhex(run);
        let mut cursor = &raw[..];
        if let Ok(tx) = Transaction::decode(&mut cursor)
            && cursor.is_empty()
        {
            out.extend(special(&tx, raw.clone(), format!("{source}@{start}")));
            continue;
        }
        let mut cursor = &raw[..];
        if let Ok(block) = Block::decode(&mut cursor) {
            for (n, tx) in block.transactions().iter().enumerate() {
                out.extend(special(tx, encode(tx), format!("{source}@{start}#{n}")));
            }
        }
    }
}

/// The provider transactions of a Core block file.
fn blk(source: &str, bytes: &[u8], out: &mut Vec<Input>) {
    let mut at = 0;
    while at + 8 <= bytes.len() && bytes[at..at + 4] == [0xfc, 0xc1, 0xb7, 0xdc] {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
        let mut cursor = &bytes[at + 8..at + 8 + len];
        let block = Block::decode(&mut cursor).expect("a block of the block file");
        for (n, tx) in block.transactions().iter().enumerate() {
            out.extend(special(tx, encode(tx), format!("{source}@{at}#{n}")));
        }
        at += 8 + len;
    }
}

fn rows_file(text: &str, out: &mut Vec<Input>) {
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let tx_type = f[1].parse().expect("a type");
        let payload = unhex(f[2]);
        let tx = if f[3] == "-" {
            carrier(tx_type, &payload)
        } else {
            unhex(f[3])
        };
        out.push(Input {
            id: f[0].to_owned(),
            tx_type,
            payload,
            tx,
        });
    }
}

/// A BLS byte string's class: canonical in each scheme, or a valid point's non-canonical
/// encoding (`exotic`), or none of these (`neither`).
fn key_class(b: &[u8; 48]) -> &'static str {
    let get = |s| BlsLazyPk::from_bytes(b, BlsSchemeSource::Version(s)).get();
    let canon = |s| {
        let v = get(s);
        v.is_valid() && v.to_bytes(s) == *b
    };
    match (canon(LEGACY), canon(BASIC)) {
        (true, true) => "both",
        (true, false) => "legacy_only",
        (false, true) => "basic_only",
        _ if get(LEGACY).is_valid() || get(BASIC).is_valid() => "exotic",
        _ => "neither",
    }
}

/// A signature's class; `refused` when Core's payload read refuses it.
fn sig_class(b: &[u8; 96]) -> &'static str {
    if BlsEagerSig::decode(b, BlsSchemeSource::Version(BASIC)).is_err() {
        return "refused";
    }
    let canon = |s| {
        BlsEagerSig::decode(b, BlsSchemeSource::Version(s))
            .is_ok_and(|v| v.is_valid() && v.to_bytes(s) == *b)
    };
    match (canon(LEGACY), canon(BASIC)) {
        (true, true) => "both",
        (true, false) => "legacy_only",
        (false, true) => "basic_only",
        _ => "neither",
    }
}

/// The key mutation values' classes, [`PKC_KEYS`] aside.
const KEY_CLASSES: [&str; 6] = ["zeros", "ff", "legacy_only", "basic_only", "both", "exotic"];

/// The signature mutation values' classes.
const SIG_CLASSES: [&str; 6] = ["zeros", "ff", "legacy_only", "basic_only", "both", "refused"];

/// Key mutation values beside the classes: `ZZ_X1` and `ZZ_X2` of dash-pkc's
/// `pkgs/pkc/src/bls/elem.rs` tests and `G1_OFF_SUBGROUP_CHIA` and `G1_OFF_SUBGROUP_IETF` of its
/// `bls/tests.rs`, at the rc.
const PKC_KEYS: [(&str, &str); 4] = [
    (
        "pkc_zz_x1",
        "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001e",
    ),
    (
        "pkc_zz_x2",
        "15248e0fa7fb53ac007285570b6877260dc7f6e9e293c4f27734bc697f11ea88376dfaebe9096bf05858dbcf966cb9d6",
    ),
    (
        "pkc_g1_off_subgroup_chia",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004",
    ),
    (
        "pkc_g1_off_subgroup_ietf",
        "800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004",
    ),
];

/// The mutation values: zeros, 0xff, then the first candidate of each class. A sig's `refused` is
/// the first of `flagged` that Core's payload read refuses.
fn values<const N: usize>(
    candidates: &[[u8; N]],
    flagged: &[[u8; N]],
    class: fn(&[u8; N]) -> &'static str,
) -> BTreeMap<&'static str, [u8; N]> {
    let mut out = BTreeMap::from([("zeros", [0; N]), ("ff", [0xff; N])]);
    for c in candidates {
        out.entry(class(c)).or_insert(*c);
    }
    if N == BLS_SIG_LEN {
        out.remove("refused");
        if let Some(c) = flagged.iter().find(|c| class(c) == "refused") {
            out.insert("refused", *c);
        }
    }
    out.remove("neither");
    out
}

type Candidates = (Vec<[u8; 48]>, Vec<[u8; 96]>, Vec<[u8; 96]>);

/// Key and signature candidates: valid keys and signatures of both encodings, variants of
/// them, and every key and signature the inputs hold.
fn candidates(inputs: &[Input]) -> Candidates {
    let mut keys = Vec::new();
    let mut sigs = Vec::new();
    let mut flagged = Vec::new();
    for n in 1..=16u8 {
        let sk = BlsSecretKey::<BlsScIetf>::from_ikm(&[n; 32]).expect("key material");
        let basic = sk.public_key().to_bytes();
        let point = BlsLazyPk::from_bytes(&basic, BlsSchemeSource::Version(BASIC)).get();
        for b in [basic, point.to_bytes(LEGACY)] {
            keys.push(b);
            for bit in [0x80, 0x40, 0x20] {
                let mut v = b;
                v[0] ^= bit;
                keys.push(v);
            }
        }
        let sig = sk.sign_with(&[n; 32], BlsSigId::Basic).to_bytes();
        let eager =
            BlsEagerSig::decode(&sig, BlsSchemeSource::Version(BASIC)).expect("a valid signature");
        for b in [sig, eager.to_bytes(LEGACY)] {
            sigs.push(b);
            for bit in [0x80, 0x40, 0x20] {
                let mut v = b;
                v[0] ^= bit;
                flagged.push(v);
            }
        }
    }
    for input in inputs {
        let mut cursor = &input.payload[..];
        match SpecialPayload::decode(TxType::from_base(input.tx_type), &mut cursor) {
            Ok(SpecialPayload::ProviderRegister(p)) => {
                keys.push(*p.pub_key_operator().as_bytes().as_bytes())
            }
            Ok(SpecialPayload::ProviderUpdateRegistrar(p)) => {
                keys.push(*p.pub_key_operator().as_bytes().as_bytes())
            }
            Ok(SpecialPayload::ProviderUpdateSharedRegistrar(p)) => {
                keys.push(*p.pub_key_operator().as_bytes().as_bytes());
            }
            Ok(SpecialPayload::ProviderUpdateService(p)) => {
                sigs.push(*p.sig().as_bytes().as_bytes())
            }
            Ok(SpecialPayload::ProviderUpdateRevoke(p)) => {
                sigs.push(*p.sig().as_bytes().as_bytes())
            }
            _ => {}
        }
    }
    sigs.extend(flagged.iter().copied());
    (keys, sigs, flagged)
}

/// The operator key's or signature's wire bytes and their offset in `payload`: the one offset
/// where writing other bytes changes the decoded field to them.
fn bls_field(tx_type: u16, payload: &[u8]) -> Option<(usize, usize)> {
    let decode = |bytes: &[u8]| {
        let mut cursor = bytes;
        let field = match SpecialPayload::decode(TxType::from_base(tx_type), &mut cursor).ok()? {
            SpecialPayload::ProviderRegister(p) => {
                p.pub_key_operator().as_bytes().as_bytes().to_vec()
            }
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                p.pub_key_operator().as_bytes().as_bytes().to_vec()
            }
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
                p.pub_key_operator().as_bytes().as_bytes().to_vec()
            }
            SpecialPayload::ProviderUpdateService(p) => p.sig().as_bytes().as_bytes().to_vec(),
            SpecialPayload::ProviderUpdateRevoke(p) => p.sig().as_bytes().as_bytes().to_vec(),
            _ => return None,
        };
        Some(field)
    };
    let raw = decode(payload)?;
    let n = raw.len();
    let probe = vec![0x5a; n];
    let mut found = (0..=payload.len().saturating_sub(n)).filter(|&at| {
        payload[at..at + n] == raw[..] && {
            let mut copy = payload.to_vec();
            copy[at..at + n].copy_from_slice(&probe);
            decode(&copy).is_some_and(|f| f == probe)
        }
    });
    let at = found.next()?;
    found.next().is_none().then_some((at, n))
}

fn generate(args: &[String]) {
    let (rows_path, values_path) = (&args[0], &args[1]);
    let mut inputs = Vec::new();
    let mut crafted = Vec::new();
    for arg in &args[2..] {
        let (kind, path) = arg.split_once(':').expect("kind:path");
        let name = std::path::Path::new(path).file_name().expect("a file").to_string_lossy();
        match kind {
            "scan" => scan(&name, &std::fs::read_to_string(path).expect("a file"), &mut inputs),
            "blk" => blk(path, &std::fs::read(path).expect("a file"), &mut inputs),
            "rows" => rows_file(&std::fs::read_to_string(path).expect("a file"), &mut inputs),
            "crafted" => {
                let cases: Value =
                    serde_json::from_str(&std::fs::read_to_string(path).expect("a file"))
                        .expect("JSON");
                for (name, case) in cases.as_object().expect("an object") {
                    let raw = unhex(case["raw"].as_str().expect("raw"));
                    let tx = Transaction::decode(&mut &raw[..]).expect("a crafted transaction");
                    crafted.push(Input {
                        id: format!("crafted:{name}"),
                        tx_type: tx.tx_type().to_base(),
                        payload: tx.extra_payload().clone(),
                        tx: raw,
                    });
                }
            }
            _ => panic!("input kind {kind}"),
        }
    }
    let mut seen = BTreeSet::new();
    inputs.retain(|i| seen.insert((i.tx_type, i.payload.clone(), i.tx.clone())));
    let (keys, sigs, flagged) = candidates(&inputs);
    let mut key_values = values(&keys, &[], key_class);
    for (name, value) in PKC_KEYS {
        key_values.insert(name, unhex(value).try_into().expect("a 48-byte key"));
    }
    let sig_values = values(&sigs, &flagged, sig_class);
    let mut out = BufWriter::new(std::fs::File::create(rows_path).expect("rows"));
    let mut row = |id: String, input: &Input, payload: &[u8]| {
        let tx = with_payload(&input.tx, &input.payload, payload)
            .unwrap_or_else(|| carrier(input.tx_type, payload));
        writeln!(out, "{id}\t{}\t{}\t{}", input.tx_type, hx(payload), hx(&tx)).expect("rows");
    };
    for c in &crafted {
        row(c.id.clone(), c, &c.payload);
    }
    let (mut placed, mut unplaced) = (0usize, Vec::new());
    for input in &inputs {
        let mut fields: Vec<(String, Vec<u8>)> = vec![("none".into(), input.payload.clone())];
        match bls_field(input.tx_type, &input.payload) {
            Some((at, n)) => {
                placed += 1;
                let what = if n == 48 {
                    "key"
                } else {
                    "sig"
                };
                let classes: Vec<&str> = if n == 48 {
                    KEY_CLASSES.into_iter().chain(PKC_KEYS.map(|(name, _)| name)).collect()
                } else {
                    SIG_CLASSES.to_vec()
                };
                for class in classes {
                    let value: Option<Vec<u8>> = if n == 48 {
                        key_values.get(class).map(|v| v.to_vec())
                    } else {
                        sig_values.get(class).map(|v| v.to_vec())
                    };
                    if let Some(value) = value {
                        let mut p = input.payload.clone();
                        p[at..at + n].copy_from_slice(&value);
                        fields.push((format!("{what}={class}"), p));
                    }
                }
            }
            None => unplaced.push(input.id.clone()),
        }
        let versions: Vec<Option<u16>> = if (1..=4).contains(&input.tx_type) {
            vec![None, Some(1), Some(2), Some(3)]
        } else {
            vec![None]
        };
        for (field, payload) in &fields {
            for version in &versions {
                let mut p = payload.clone();
                let label = match version {
                    Some(v) if p.len() >= 2 => {
                        p[..2].copy_from_slice(&v.to_le_bytes());
                        format!("v{v}")
                    }
                    Some(_) => continue,
                    None => "v=".into(),
                };
                row(format!("{}|{field}|{label}", input.id), input, &p);
            }
        }
    }
    // OK3-CARRY item 4: a ProUpServTx and a ProUpRevTx whose signature Core's payload read
    // refuses, from corpus rows, the signature the only change.
    let refused = sigs
        .iter()
        .chain(&flagged)
        .find(|s| sig_class(s) == "refused" && **s != [0xff; 96])
        .copied();
    let mut item4 = Vec::new();
    for tx_type in [2u16, 4] {
        let source = inputs.iter().find(|i| {
            i.tx_type == tx_type
                && !i.id.starts_with("bsdk:")
                && bls_field(tx_type, &i.payload).is_some()
        });
        if let (Some(input), Some(sig)) = (source, refused) {
            let (at, n) = bls_field(tx_type, &input.payload).expect("placed");
            let mut p = input.payload.clone();
            p[at..at + n].copy_from_slice(&sig);
            let id = format!("item4:{tx_type}:{}", input.id);
            row(id.clone(), input, &p);
            item4.push(id);
        }
    }
    let values = json!({
        "inputs": inputs.len(),
        "inputs_by_type": PROVIDER.iter().map(|t| (t.to_string(), inputs.iter().filter(|i| i.tx_type == *t).count())).collect::<BTreeMap<_, _>>(),
        "crafted": crafted.len(),
        "bls_field_placed": placed,
        "bls_field_unplaced": unplaced,
        "key_values": key_values.iter().map(|(k, v)| (*k, hx(v))).collect::<BTreeMap<_, _>>(),
        "sig_values": sig_values.iter().map(|(k, v)| (*k, hx(v))).collect::<BTreeMap<_, _>>(),
        "missing_key_classes": KEY_CLASSES.iter().filter(|c| !key_values.contains_key(*c)).collect::<Vec<_>>(),
        "missing_sig_classes": SIG_CLASSES.iter().filter(|c| !sig_values.contains_key(*c)).collect::<Vec<_>>(),
        "item4": item4,
        "item4_sig": refused.map(|sig| {
            sig_values
                .iter()
                .find(|(_, v)| **v == sig)
                .map_or_else(|| hx(&sig), |(class, _)| format!("sig_values.{class}"))
        }),
    });
    std::fs::write(values_path, format!("{values:#}\n")).expect("values");
}

// ---------------------------------------------------------------------------------------------
// eval

fn networks() -> [(&'static str, ChainSetup); 3] {
    let params = |network| {
        ChainParamsBuilder::new(network, Release::V24_0).build().expect("Core's parameters")
    };
    [
        ("mainnet", params(Network::Main)),
        ("testnet", params(Network::Testnet3)),
        ("regtest", params(Network::Regtest)),
    ]
}

/// node-l1 `payload.rs` `core_bytes` at OK-3's head for the provider payloads: base-sdk's
/// encoding, the operator signature as the side-car's object under the version's scheme.
fn core_image(decoded: &CorePayload) -> Option<Vec<u8>> {
    let with_sig = |mut out: Vec<u8>, version| {
        let at = out.len().checked_sub(BLS_SIG_LEN)?;
        out[at..].copy_from_slice(&decoded.operator_sig()?.to_bytes(protx_key_scheme(version)));
        Some(out)
    };
    match decoded.payload() {
        SpecialPayload::ProviderRegister(p) => Some(encode(p)),
        SpecialPayload::ProviderUpdateRegistrar(p) => Some(encode(p)),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => Some(encode(p)),
        SpecialPayload::ProviderUpdateService(p) => with_sig(encode(p), p.version()),
        SpecialPayload::ProviderUpdateRevoke(p) => with_sig(encode(p), p.version()),
        _ => None,
    }
}

fn row(
    tx_type: u16,
    payload: &[u8],
    tx: Option<&Transaction>,
    nets: &[(&str, ChainSetup)],
) -> Value {
    let accepted =
        tx.map(|tx| matches!(Tx::new(tx.clone()).payload(), Some(Ok(d)) if d.is_complete()));
    let decoded = match CorePayload::decode(TxType::from_base(tx_type), payload) {
        Err(e) => return json!({"verdict": {"err": e.to_string()}, "accepted": accepted}),
        Ok(d) => d,
    };
    let verdict = json!({
        "ok": true,
        "consumed": decoded.consumed(),
        "complete": decoded.is_complete(),
    });
    let mut out = json!({"verdict": verdict, "accepted": accepted});
    out["key"] = decoded.operator_key().map_or(Value::Null, bls::key);
    out["sig"] = decoded.operator_sig().map_or(Value::Null, bls::sig);
    let mut hashes = serde_json::Map::new();
    let key = decoded.operator_key();
    let mut sig_hash = |h: Option<[u8; 32]>| {
        hashes.insert("sig_hash".into(), json!(hx(&h.expect("a sign hash"))));
    };
    match decoded.payload() {
        SpecialPayload::ProviderRegister(p) => {
            let key = key.expect("a key");
            sig_hash(payload_sig_hash_with_key(p, key));
            for (name, params) in nets {
                let text = make_sign_string_with_key(p, key, params).expect("a sign string");
                let hash = pro_reg_message_hash_with_key(p, key, params).expect("a hash");
                hashes.insert(format!("sign_string_{name}"), json!(text));
                hashes.insert(format!("message_hash_{name}"), json!(hx(&hash)));
            }
            if let Some(tx) = tx {
                let consent = shared_reg_consent_hash_with_key(p, key, &Tx::new(tx.clone()));
                hashes.insert("consent".into(), json!(hx(&consent.expect("a hash"))));
            }
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            sig_hash(payload_sig_hash_with_key(p, key.expect("a key")))
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            sig_hash(payload_sig_hash_with_key(p, key.expect("a key")));
        }
        SpecialPayload::ProviderUpdateService(p) => sig_hash(payload_sig_hash(p)),
        SpecialPayload::ProviderUpdateRevoke(p) => sig_hash(payload_sig_hash(p)),
        _ => {}
    }
    out["hashes"] = Value::Object(hashes);
    out["core_image"] = core_image(&decoded).map_or(Value::Null, |b| json!(hx(&b)));
    out
}

fn eval(args: &[String]) {
    let nets = networks();
    let rows = std::io::BufReader::new(std::fs::File::open(&args[0]).expect("rows"));
    let mut out = BufWriter::new(std::io::stdout().lock());
    for line in rows.lines() {
        let line = line.expect("a row");
        let f: Vec<&str> = line.split('\t').collect();
        let tx = (f[3] != "-").then(|| unhex(f[3]));
        let tx = tx.and_then(|raw| Transaction::decode(&mut &raw[..]).ok());
        let value = row(f[1].parse().expect("a type"), &unhex(f[2]), tx.as_ref(), &nets);
        writeln!(out, "{}", json!({"id": f[0], "v": value})).expect("stdout");
    }
}

// ---------------------------------------------------------------------------------------------
// compare

/// The rows of a JSON lines file by id, and the ids that occur more than once.
fn read_jsonl(path: &str) -> (BTreeMap<String, Value>, Vec<String>) {
    let text = std::fs::read_to_string(path).expect("a JSON lines file");
    let (mut rows, mut duplicates) = (BTreeMap::new(), Vec::new());
    for line in text.lines() {
        let mut v: Value = serde_json::from_str(line).expect("JSON");
        let id = v["id"].as_str().expect("an id").to_owned();
        if rows.insert(id.clone(), v["v"].take()).is_some() {
            duplicates.push(id);
        }
    }
    (rows, duplicates)
}

/// The deleted `payload_core.rs` test's `core_object`: Core's payload object for `flag`, `None`
/// when Core refuses the payload, or an error for an RPC error or other than one object.
fn core_object<'a>(case: &'a Value, flag: &str) -> Result<Option<&'a Value>, String> {
    let outcome = &case["core"][V24][flag];
    if outcome.is_null() {
        return Ok(None);
    }
    if outcome.get("rpc-error").is_some() {
        return Err(format!("rpc error: {outcome}"));
    }
    match outcome.as_object() {
        Some(o) if o.len() == 1 => Ok(o.values().next()),
        _ => Err(format!("not one payload object: {outcome}")),
    }
}

/// The crafted rows the run needs at least (the deleted test asked for 70; the rc has 84).
const CRAFTED_MIN: usize = 84;

const V24: &str = "v24.0.0-rc.3";

fn compare(args: &[String]) {
    let ((old, old_dups), (new, new_dups)) = (read_jsonl(&args[0]), read_jsonl(&args[1]));
    let crafted: Value =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).expect("a file")).expect("JSON");
    let input_rows = std::fs::read_to_string(&args[3]).expect("the rows file").lines().count();
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut differences = Vec::new();
    for (id, n) in &new {
        let o = old.get(id).unwrap_or(&Value::Null);
        let same = o == n;
        if !same {
            differences.push(id.clone());
        }
        writeln!(out, "{}", json!({"id": id, "same": same, "old": o, "new": n})).expect("stdout");
    }
    let missing: Vec<&String> = old.keys().filter(|id| !new.contains_key(*id)).collect();
    // OK3-CARRY item 5: every crafted row under each flag.
    let (mut covered, mut item5_failures, mut sig_unprinted) = (0usize, Vec::new(), Vec::new());
    let mut unknown_version_defaults = Vec::new();
    let item4: Vec<(&String, bool)> = new
        .iter()
        .filter(|(id, _)| id.starts_with("item4:"))
        .map(|(id, n)| {
            let refused = |v: &Value| {
                v["verdict"]["err"].as_str().is_some_and(|e| e.ends_with("malleable BLS object"))
            };
            (id, refused(n) && old.get(id).is_some_and(refused))
        })
        .collect();
    for (name, case) in crafted.as_object().expect("an object") {
        let id = format!("crafted:{name}");
        let (Some(n), Some(o)) = (new.get(&id), old.get(&id)) else {
            item5_failures.push(format!("{id}: no row"));
            continue;
        };
        for flag in ["basic", "legacy"] {
            covered += 1;
            let label = format!("{name} ({flag})");
            let core = match core_object(case, flag) {
                Ok(core) => core,
                Err(e) => {
                    item5_failures.push(format!("{label}: {e}"));
                    continue;
                }
            };
            let ours = n["accepted"].as_bool().expect("a verdict");
            if ours != core.is_some() {
                item5_failures.push(format!("{label}: node {ours}, Core {}", core.is_some()));
            }
            if o["accepted"] != n["accepted"] {
                item5_failures.push(format!("{label}: node {ours}, old pin {}", o["accepted"]));
            }
            let Some(core) = core else {
                continue;
            };
            let unknown_version = core["version"].as_u64().is_some_and(|v| v == 0 || v > 3);
            if let Some(want) = core.get("pubKeyOperator") {
                if unknown_version && n["key"].is_null() {
                    // The deleted test's rule: Core reads the version alone and prints its
                    // defaults for the rest; the node holds no key.
                    if want.as_str().is_some_and(|s| s.bytes().all(|b| b == b'0')) {
                        unknown_version_defaults.push(label);
                    } else {
                        item5_failures.push(format!("{label}: Core key {want} is no default"));
                    }
                } else if n["key"]["print"] != *want {
                    item5_failures.push(format!(
                        "{label}: operator key {} vs Core {want}",
                        n["key"]["print"]
                    ));
                }
            } else if !n["sig"].is_null() {
                sig_unprinted.push(label);
            }
        }
    }
    let crafted_rows = crafted.as_object().map_or(0, |o| o.len());
    let item4_refused = item4.iter().filter(|(_, r)| !r).map(|(id, _)| id).collect::<Vec<_>>();
    let checks = [
        (differences.is_empty(), "differences"),
        (missing.is_empty(), "old rows without new"),
        (old_dups.is_empty() && new_dups.is_empty(), "duplicate ids"),
        (item5_failures.is_empty(), "item 5 failures"),
        (item4.len() == 2 && item4_refused.is_empty(), "item 4"),
        (crafted_rows >= CRAFTED_MIN, "crafted rows"),
        (new.len() == input_rows && old.len() == input_rows, "row count"),
    ];
    let failed: Vec<&str> = checks.iter().filter(|(ok, _)| !ok).map(|(_, what)| *what).collect();
    let summary = json!({
        "summary": {
            "failed": failed,
            "input_rows": input_rows,
            "rows": new.len(),
            "old_rows": old.len(),
            "duplicate_ids": {"old": old_dups, "new": new_dups},
            "crafted": crafted_rows,
            "differences": differences.len(),
            "differing_rows": differences.iter().take(50).collect::<Vec<_>>(),
            "old_rows_without_new": missing,
            "item5_rows_x_flags": covered,
            "item5_failures": item5_failures,
            "item5_sig_not_printed_by_core": sig_unprinted,
            "item5_unknown_version_key_defaults": unknown_version_defaults,
            "item4_rows": item4.len(),
            "item4_not_refused_by_both": item4_refused,
        }
    });
    writeln!(out, "{summary}").expect("stdout");
    out.flush().expect("stdout");
    eprintln!("{summary:#}");
    if !failed.is_empty() {
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("gen") => generate(&args[1..]),
        Some("eval") => eval(&args[1..]),
        Some("compare") => compare(&args[1..]),
        _ => panic!("usage: t6-probe gen|eval|compare ..."),
    }
}
