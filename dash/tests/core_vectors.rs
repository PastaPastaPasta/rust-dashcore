//! Dash Core's JSON test vectors (`test-vectors/core/<commit>/`) run against `dashcore`.
//!
//! Loaders follow the row rules of the Core test that consumes each file; see `dash_core_vectors`.
//! The wider L0 gate (script interpreter, special-tx validity, key formats) belongs to the crates
//! that implement them; this file covers what `dashcore` already has: the legacy signature hash,
//! base58, decoding of every transaction in the tx vectors, and the simplified-masternode-list
//! entries and the InstantSend lock Core's unit tests print (`transcribed/`).

use dash_core_vectors::cases::{
    TxFlags, load_base58, load_sighash, load_trivial, load_tx_invalid, load_tx_valid,
};
use dash_core_vectors::hex::{hex_str, parse_hex, uint256_from_hex, uint256_to_hex};
use dash_core_vectors::read_core_text;
use dash_core_vectors::script::reference_serialize_script_code;
use dash_core_vectors::univalue::UniValue;
use dashcore::consensus::encode::VarInt;
use dashcore::consensus::{deserialize, deserialize_partial, serialize};
use dashcore::hash_types::QuorumHash;
use dashcore::hashes::Hash;
use dashcore::sighash::SighashCache;
use dashcore::sml::llmq_type::LLMQType;
use dashcore::sml::masternode_list_entry::MasternodeListEntry;
use dashcore::transaction::special_transaction::TransactionPayload;
use dashcore::{InstantLock, Script, ScriptBuf, Transaction};

fn data(name: &str) -> String {
    read_core_text(&format!("src/test/data/{name}"))
}

/// The script code Core hashes for `script`, as the `Script` that `legacy_signature_hash` has to be
/// given to hash the same bytes. `legacy_signature_hash` writes its own length prefix, so this only
/// exists for scripts where Core's prefix equals the length of its body, i.e. everything except a
/// script ending in a truncated push; that case panics instead of hashing something else.
///
/// This is test scaffolding for BL-08, not a model for the fix: a `legacy_signature_hash` that
/// handles `OP_CODESEPARATOR` must do what [`reference_serialize_script_code`] does, including for
/// malformed scripts.
fn stripped_script_code(script: &Script) -> ScriptBuf {
    let serialized = reference_serialize_script_code(script.as_bytes());
    let (VarInt(len), used): (VarInt, usize) = deserialize_partial(&serialized).unwrap();
    assert_eq!(
        len as usize,
        serialized.len() - used,
        "script {} ends in a truncated push: Core's script code is shorter than its length prefix",
        script
    );
    ScriptBuf::from(serialized[used..].to_vec())
}

/// `sighash_tests.cpp:sighash_from_data`: `SignatureHash(scriptCode, tx, nIn, nHashType, 0, BASE)`
/// must print as the expected hex.
///
/// Known gap (BL-08): `SighashCache::legacy_signature_hash` documents that it does not support
/// `OP_CODESEPARATOR`, while Core strips it from the script code before hashing. So every vector
/// is hashed twice: with the script code as given (all 331 that contain no `OP_CODESEPARATOR` must
/// match, and exactly the other 169 must not), and with the opcode stripped as Core does, where
/// **all 500** must match, including the `SIGHASH_SINGLE` quirk cases. The test asks for the
/// allowance to be removed once the first form matches too.
#[test]
fn sighash_json() {
    let cases = load_sighash(&data("sighash.json")).unwrap();
    assert_eq!(cases.len(), 500);
    let mut unstripped_mismatches = 0;
    let mut out_of_range = 0;
    for c in cases {
        let tx: Transaction = deserialize(&c.tx).unwrap_or_else(|e| panic!("row {}: {e}", c.index));
        if c.n_in < 0 || c.n_in as usize >= tx.input.len() {
            // Core's `SignatureHash` returns `uint256::ONE` here before hashing anything; the
            // vector must say so. `legacy_signature_hash` is not asked.
            let mut one = [0u8; 32];
            one[0] = 1;
            assert_eq!(uint256_to_hex(&one), c.expected, "row {}", c.index);
            out_of_range += 1;
            continue;
        }
        let script = ScriptBuf::from(c.script_code.clone());
        let hash = |script: &Script| {
            let hash = SighashCache::new(&tx)
                .legacy_signature_hash(c.n_in as usize, script, c.hash_type as u32)
                .unwrap_or_else(|e| panic!("row {}: {e}", c.index));
            // `LegacySighash` stores the double SHA-256 in hashing order; Core prints it reversed.
            uint256_to_hex(&hash.to_byte_array())
        };
        let stripped = stripped_script_code(&script);
        assert_eq!(hash(&stripped), c.expected, "sighash.json row {} (stripped)", c.index);
        if hash(&script) != c.expected {
            assert_ne!(stripped, script, "row {} differs without OP_CODESEPARATOR", c.index);
            unstripped_mismatches += 1;
        }
    }
    assert_eq!(out_of_range, 0, "the vectors all name an existing input");
    assert_eq!(
        unstripped_mismatches, 169,
        "legacy_signature_hash changed how it treats OP_CODESEPARATOR (BL-08): if it now matches \
         Core, drop the stripping from this test"
    );
}

/// `base58_tests.cpp:base58_DecodeBase58`. The encode direction (`EncodeBase58`, no checksum) is not
/// exercised: `base58ck` only exposes the checked encoding.
#[test]
fn base58_decode_json() {
    for c in load_base58(&data("base58_encode_decode.json")).unwrap() {
        assert_eq!(dashcore::base58::decode(&c.base58).unwrap(), c.data, "row {}", c.index);
    }
}

/// Every transaction in `tx_valid.json` and `tx_invalid.json` decodes and re-encodes to the same
/// bytes. Core's harness deserializes with `CTransaction(deserialize, stream)` and does not
/// require the stream to be fully consumed, so a tail after the transaction is allowed here too.
#[test]
fn tx_vectors_decode() {
    let valid = load_tx_valid(&data("tx_valid.json")).unwrap();
    let invalid = load_tx_invalid(&data("tx_invalid.json")).unwrap();
    let mut bad_tx_rows = 0;
    for c in valid.iter().chain(&invalid) {
        let (tx, used): (Transaction, usize) = deserialize_partial(&c.tx)
            .unwrap_or_else(|e| panic!("row {} ({}): {e}", c.index, c.tx_hex));
        assert_eq!(serialize(&tx), c.tx[..used], "row {} does not re-encode", c.index);
        if c.flags == TxFlags::BadTx {
            bad_tx_rows += 1;
        }
    }
    assert_eq!(bad_tx_rows, 10);
}

/// `trivially_valid.json` / `trivially_invalid.json` carry special transactions (version 3) whose
/// txid is the first column. Every row decodes, and its payload is typed exactly when Core could
/// extract it: Core fails the extraction only in the `bad-protx-payload` row (version 0, of which
/// it reads nothing more), kept raw here. That includes the version 1 Evo ProRegTx of rows 1 and 2
/// of `trivially_invalid.json`: Core reads its platform node id and ports because of the Evo type
/// alone, and rejects the version in validation (`bad-protx-evo-version`).
#[test]
fn trivial_validation_transactions_hash_to_their_txid() {
    for (file, expected_len) in [("trivially_valid.json", 25), ("trivially_invalid.json", 12)] {
        let cases = load_trivial(file, &data(file)).unwrap();
        assert_eq!(cases.len(), expected_len);
        for c in cases {
            let tx: Transaction = deserialize(&c.tx)
                .unwrap_or_else(|e| panic!("{file} row {}: decode failed: {e}", c.index));
            assert_eq!(tx.txid().to_byte_array(), c.tx_hash, "{file} row {}", c.index);
            assert_eq!(serialize(&tx), c.tx, "{file} row {} does not re-encode", c.index);
            let raw = matches!(
                tx.special_transaction_payload,
                Some(TransactionPayload::UnknownPayloadType(..))
            );
            let core_fails_extraction = c.expected_error.as_deref() == Some("bad-protx-payload");
            assert_eq!(
                raw, core_fails_extraction,
                "{file} row {}: raw iff Core can't extract",
                c.index
            );
        }
    }
}

fn transcribed_records(name: &str) -> Vec<UniValue> {
    let doc = UniValue::parse(&read_core_text(&format!("transcribed/{name}.json"))).unwrap();
    doc.find_value("records").get_array().unwrap().to_vec()
}

fn text<'a>(v: &'a UniValue, key: &str) -> &'a str {
    v.find_value(key).get_str().unwrap_or_else(|e| panic!("{key}: {e}"))
}

/// `evo_simplifiedmns_tests.cpp:simplifiedmns_merkleroots`: the 15 entries Core serialized with
/// `SER_NETWORK` decode, re-encode to the same bytes, and hash to Core's `CalcHash`.
#[test]
fn simplified_masternode_list_entries_hash_as_core_does() {
    let mut entries = 0;
    for r in transcribed_records("evo_simplifiedmns_tests") {
        if text(&r, "kind") != "entry" {
            continue;
        }
        let wire = parse_hex(text(&r, "wire"));
        let what = format!("entry {}", text(&r, "pro_reg_tx_hash"));
        let entry: MasternodeListEntry =
            deserialize(&wire).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(serialize(&entry), wire, "{what} does not re-encode");
        assert_eq!(i64::from(entry.version), r.find_value("version").get_i64().unwrap(), "{what}");
        assert_eq!(
            entry.pro_reg_tx_hash.to_byte_array(),
            uint256_from_hex(text(&r, "pro_reg_tx_hash")),
            "{what}"
        );
        assert_eq!(entry.is_valid, r.find_value("is_valid").get_bool().unwrap(), "{what}");
        assert_eq!(
            uint256_to_hex(&entry.calculate_entry_hash().to_byte_array()),
            text(&r, "entry_hash"),
            "{what}"
        );
        entries += 1;
    }
    assert_eq!(entries, 15);
}

/// `evo_islock_tests.cpp:deserialize_instantlock_from_realdata2`: the lock Core deserialized
/// decodes to the fields Core read from it, re-encodes to the same bytes, and has the request id
/// and sign hash Core computed.
#[test]
fn instant_lock_is_what_core_read_and_hashed() {
    let rows = transcribed_records("evo_islock_tests");
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    let wire = parse_hex(text(r, "islock_hex"));
    let lock: InstantLock = deserialize(&wire).unwrap();
    assert_eq!(serialize(&lock), wire);

    assert_eq!(i64::from(lock.version), r.find_value("version").get_i64().unwrap());
    let inputs = r.find_value("inputs").get_array().unwrap();
    assert_eq!(lock.inputs.len(), inputs.len());
    for (outpoint, input) in lock.inputs.iter().zip(inputs) {
        assert_eq!(outpoint.txid.to_byte_array(), uint256_from_hex(input.at(0).get_str().unwrap()));
        assert_eq!(i64::from(outpoint.vout), input.at(1).get_i64().unwrap());
    }
    assert_eq!(lock.txid.to_byte_array(), uint256_from_hex(text(r, "txid")));
    assert_eq!(lock.cyclehash.to_byte_array(), uint256_from_hex(text(r, "cycle_hash")));
    assert_eq!(hex_str(lock.signature.as_bytes()), text(r, "signature"));

    let request_id = lock.request_id().unwrap();
    assert_eq!(uint256_to_hex(&request_id.to_byte_array()), text(r, "request_id"));
    let sign_id = lock
        .sign_id(
            LLMQType::from(r.find_value("llmq_type").get_i64().unwrap() as u8),
            QuorumHash::from_byte_array(uint256_from_hex(text(r, "quorum_hash"))),
            None,
        )
        .unwrap();
    assert_eq!(uint256_to_hex(&sign_id.to_byte_array()), text(r, "sign_hash"));
}
