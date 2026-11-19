//! Release-v24 ChainLock vectors consumed by LK1a's verifier tests.
//!
//! C2 supplies JSONL files under `tests/test-vectors/v24.0.0/`.  Keeping the loader in this
//! crate means the capture can land independently of the node harness; the verifier-side tests
//! can use the same records to rebuild the captured tip, fork, quorum and BLS-scheme state.

use std::fs;
use std::path::PathBuf;

use dash_consensus::hash::{sha256d, x11};
use dash_consensus::{Block, decode_block};
use dash_evo::bls::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::locks::ChainLockSig;
use dash_p2p_core::ClSig;
use dash_pkc::bls::BlsSigBytes;
use dash_primitives::BlockHash;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Record {
    case: String,
    node: String,
    outcome: String,
    state: Value,
    block: BlockRecord,
    input: InputRecord,
    clsig: ClsigRecord,
    expect_decode: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BlockRecord {
    bytes: String,
    hash: String,
    height: i32,
}

#[derive(Debug, Deserialize)]
struct InputRecord {
    carrier_block: Option<BlockRecord>,
}

#[derive(Debug, Deserialize)]
struct ClsigRecord {
    bytes: String,
    height: i32,
    block_hash: String,
    signature: String,
    hash: String,
    scheme: String,
}

fn bytes(text: &str, field: &str) -> Vec<u8> {
    assert!(!text.is_empty(), "{field} is empty");
    assert!(text.len().is_multiple_of(2), "{field} has odd hex length");
    assert!(text.bytes().all(|byte| byte.is_ascii_hexdigit()), "bad {field} hex");
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16).unwrap_or_else(|_| panic!("bad {field} hex"))
        })
        .collect()
}

#[test]
fn release_v24_chainlock_vectors_round_trip_and_hash() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/test-vectors/v24.0.0");
    let mut files = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("{}: {error}", root.display()))
        .map(|entry| {
            entry.unwrap_or_else(|error| panic!("reading {}: {error}", root.display())).path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    files.sort();

    let mut records = 0;
    for file in files {
        for line in fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()))
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            check_record(line, &file.display().to_string());
            records += 1;
        }
    }
    assert_eq!(records, EXPECTED_RECORDS, "ChainLock vector records in {}", root.display());
}

const EXPECTED_RECORDS: usize = 7;

/// Decodes `block` as a Dash block with no trailing bytes, and checks that the X11 of its 80-byte
/// header equals `block.hash` and that its transactions hash to the header's merkle root. Hashes
/// are compared in Core's internal byte order (the `uint256` order the CLSIG wire carries), not
/// the reversed RPC display order.
fn check_block(block: &BlockRecord, what: &str) -> Block {
    let wire = bytes(&block.bytes, &format!("{what}.bytes"));
    let hash = bytes(&block.hash, &format!("{what}.hash"));
    let mut cursor = wire.as_slice();
    let decoded = decode_block(&mut cursor)
        .unwrap_or_else(|error| panic!("{what} does not decode: {error:?}"))
        .into_value();
    assert!(cursor.is_empty(), "{what} has trailing bytes");
    assert_eq!(x11(&wire[..80]).as_slice(), hash.as_slice(), "{what} X11 header hash");
    assert_eq!(decoded.hash().as_bytes().as_slice(), hash.as_slice(), "{what} decoded hash");
    let (root, mutated) = decoded.merkle();
    assert!(!mutated && root == decoded.header().merkle_root(), "{what} merkle root");
    decoded
}

fn check_record(line: &str, file: &str) {
    let record: Record =
        serde_json::from_str(line).unwrap_or_else(|error| panic!("{file}: {error}"));
    let _metadata = (&record.node, &record.outcome, &record.state);
    let clsig = &record.clsig;
    let wire = bytes(&clsig.bytes, "bytes");
    assert_eq!(wire.len(), 132, "{} has a non-132-byte CLSIG", record.case);
    assert_eq!(&wire[4..36], bytes(&clsig.block_hash, "block_hash").as_slice());
    assert_eq!(&wire[36..], bytes(&clsig.signature, "signature").as_slice());
    let expected_hash = bytes(&clsig.hash, "hash");
    assert_eq!(expected_hash.as_slice(), sha256d(&wire), "{} hash", record.case);

    let height = i32::from_le_bytes(wire[..4].try_into().expect("height"));
    assert_eq!(height, clsig.height, "{} height", record.case);
    let block_hash = BlockHash::from(<[u8; 32]>::try_from(&wire[4..36]).expect("hash"));
    let signature = BlsSigBytes::from(<[u8; 96]>::try_from(&wire[36..]).expect("signature"));
    let message = ClSig::new(height, block_hash, signature);
    let scheme = match clsig.scheme.as_str() {
        "basic" => BlsCtx::BASIC,
        "legacy" => BlsCtx::LEGACY,
        other => panic!("unknown BLS scheme {other}"),
    };
    let decoded = ChainLockSig::from_wire(&message, scheme);
    if record.expect_decode.unwrap_or(true) {
        let decoded =
            decoded.unwrap_or_else(|error| panic!("{} did not decode: {error}", record.case));
        assert_eq!(decoded.to_wire(scheme), message, "{} round trip", record.case);
        assert_eq!(decoded.serialize_hash(scheme).as_slice(), expected_hash.as_slice());
    } else {
        assert!(decoded.is_err(), "{} unexpectedly decoded", record.case);
    }

    check_block(&record.block, &format!("{} block", record.case));
    assert_eq!(
        bytes(&record.block.hash, "block.hash"),
        bytes(&clsig.block_hash, "block_hash"),
        "{} block.hash is not the CLSIG's block",
        record.case
    );
    assert_eq!(
        record.block.height, clsig.height,
        "{} block.height is not the CLSIG's height",
        record.case
    );

    if let Some(carrier) = &record.input.carrier_block {
        let what = format!("{} carrier_block", record.case);
        let block = check_block(carrier, &what);
        let carrier_height = u32::try_from(carrier.height).expect("carrier height");
        let (cb_sig, diff) = nonnull_coinbase_chainlock(
            &CoinbaseFacts::of(Some(&block)),
            carrier_height,
            true,
            scheme,
        )
        .unwrap_or_else(|| panic!("{what} CbTx carries no ChainLock"));
        assert_eq!(
            i64::from(carrier.height) - i64::from(diff) - 1,
            i64::from(clsig.height),
            "{what} CbTx ChainLock height"
        );
        assert_eq!(
            &cb_sig.to_bytes(scheme.scheme())[..],
            &wire[36..],
            "{what} CbTx ChainLock signature"
        );
    }
}

#[test]
fn malformed_records_are_rejected() {
    let wire = format!("05000000{}{}", "11".repeat(32), "00".repeat(96));
    let hash: String = sha256d(&bytes(&wire, "bytes")).iter().map(|b| format!("{b:02x}")).collect();
    let line = |height: &str, wire: &str, decode: &str, state: bool| {
        let state = if state {
            "\"state\":{},"
        } else {
            ""
        };
        format!(
            "{{\"case\":\"c\",\"node\":\"n\",\"outcome\":\"o\",{state}\"block\":{{\"bytes\":\"00\",\
             \"hash\":\"00\",\"height\":5}},\"input\":{{}},\"expect_decode\":{decode},\"clsig\":{{\"bytes\":\"{wire}\",\
             \"height\":{height},\"block_hash\":\"{}\",\"signature\":\"{}\",\
             \"hash\":\"{hash}\",\"scheme\":\"basic\"}}}}",
            "11".repeat(32),
            "00".repeat(96),
        )
    };
    rejects(line("5", &wire, "true", false), "missing field `state`");
    rejects(line("5", &wire, "\"false\"", true), "invalid type: string");
    rejects(line("6", &wire, "true", true), "c height");
    rejects(line("5", &wire.replacen("05", "+5", 1), "true", true), "bad bytes hex");
}

fn rejects(line: String, why: &str) {
    let panic = std::panic::catch_unwind(|| check_record(&line, "case")).expect_err(why);
    let text = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(text.contains(why), "{why}: {text}");
}

fn hex(raw: &[u8]) -> Value {
    Value::String(raw.iter().map(|b| format!("{b:02x}")).collect())
}

/// Rewrites the CLSIG's wire bytes and re-derives its height, block hash, signature and SHA256d
/// from them, so only checks beyond the CLSIG's own consistency can reject the record.
fn reseal(record: &mut Value, edit: impl FnOnce(&mut [u8])) {
    let clsig = &mut record["clsig"];
    let mut wire = bytes(clsig["bytes"].as_str().expect("bytes"), "bytes");
    edit(&mut wire);
    clsig["height"] = i32::from_le_bytes(wire[..4].try_into().unwrap()).into();
    clsig["block_hash"] = hex(&wire[4..36]);
    clsig["signature"] = hex(&wire[36..]);
    clsig["hash"] = hex(&sha256d(&wire));
    clsig["bytes"] = hex(&wire);
}

/// Flips the low bit of the hex byte at `index` (negative counts from the end) of `field`.
fn flip(record: &mut Value, field: &[&str], index: isize) {
    let slot = field.iter().fold(record, |value, key| &mut value[*key]);
    let mut raw = bytes(slot.as_str().expect("hex field"), "field");
    let index = index.rem_euclid(raw.len() as isize) as usize;
    raw[index] ^= 1;
    *slot = hex(&raw);
}

#[test]
fn corrupted_block_vectors_are_rejected() {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/test-vectors/v24.0.0/chainlocks.jsonl");
    let text =
        fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    let records: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("record"))
        .collect();
    let mut carriers = 0;
    for original in &records {
        let case = original["case"].as_str().expect("case").to_owned();
        let mutate = |field: &[&str], index: isize, why: &str| {
            let mut record = original.clone();
            flip(&mut record, field, index);
            rejects(record.to_string(), why);
        };
        let at = |why: &str| format!("{case} {why}");
        mutate(&["block", "bytes"], 0, &at("block X11 header hash"));
        mutate(&["block", "bytes"], -1, &at("block merkle root"));
        mutate(&["block", "hash"], 0, &at("block X11 header hash"));
        // The wire check fires first on the CLSIG's own copy of the hash; with the wire and
        // its SHA256d kept consistent, only the block linkage rejects it.
        mutate(&["clsig", "block_hash"], 0, "assertion `left == right` failed");
        let mut record = original.clone();
        reseal(&mut record, |wire| wire[4] ^= 1);
        rejects(record.to_string(), &at("block.hash is not the CLSIG's block"));
        let mut record = original.clone();
        record["block"]["height"] = (original["block"]["height"].as_i64().unwrap() + 1).into();
        rejects(record.to_string(), &at("block.height is not the CLSIG's height"));

        if original["input"]["carrier_block"].is_object() {
            carriers += 1;
            mutate(&["input", "carrier_block", "bytes"], -1, &at("carrier_block merkle root"));
            mutate(&["input", "carrier_block", "hash"], 0, &at("carrier_block X11 header hash"));

            let mut record = original.clone();
            reseal(&mut record, |wire| wire[0] ^= 1);
            record["block"]["height"] = record["clsig"]["height"].clone();
            rejects(record.to_string(), &at("carrier_block CbTx ChainLock height"));

            // A different valid signature under the same scheme, so the CLSIG still decodes.
            let other = records
                .iter()
                .find(|other| {
                    other["clsig"]["scheme"] == original["clsig"]["scheme"]
                        && other["clsig"]["signature"] != original["clsig"]["signature"]
                        && other["expect_decode"].as_bool().unwrap_or(true)
                })
                .expect("another decodable signature");
            let signature = bytes(other["clsig"]["signature"].as_str().unwrap(), "signature");
            let mut record = original.clone();
            reseal(&mut record, |wire| wire[36..].copy_from_slice(&signature));
            rejects(record.to_string(), &at("carrier_block CbTx ChainLock signature"));
        }
    }
    assert_eq!(carriers, 1, "records with a carrier block");
}
