//! The vectors transcribed from Core's C++ unit tests (`transcribed/*.json`), checked against
//! facts that do not come from the same run: arithmetic Core documents, other vector files, and
//! the harness audit.

use dash_core_vectors::hex::{hex_str, parse_hex, uint256_from_hex, uint256_to_hex};
use dash_core_vectors::univalue::UniValue;
use dash_core_vectors::{core_dir, read_core_text};
use dashcore_hashes::{Hash, sha256, sha256d};

fn records(name: &str) -> Vec<UniValue> {
    let text = read_core_text(&format!("transcribed/{name}.json"));
    let doc = UniValue::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(doc.find_value("source").get_str().unwrap(), format!("src/test/{name}.cpp"));
    doc.find_value("records").get_array().unwrap().to_vec()
}

fn s<'a>(v: &'a UniValue, key: &str) -> &'a str {
    v.find_value(key).get_str().unwrap_or_else(|e| panic!("{key}: {e}"))
}

fn n(v: &UniValue, key: &str) -> i64 {
    v.find_value(key).get_i64().unwrap_or_else(|e| panic!("{key}: {e}"))
}

#[test]
fn transcribed_files_match_their_checksums() {
    let list = std::fs::read_to_string(core_dir().join("transcribed/SHA256SUMS")).unwrap();
    let mut count = 0;
    for line in list.lines() {
        let (digest, file) = line.split_once("  ").unwrap();
        let bytes = std::fs::read(core_dir().join("transcribed").join(file)).unwrap();
        assert_eq!(sha256::Hash::hash(&bytes).to_string(), digest, "{file}");
        count += 1;
    }
    assert_eq!(count, 13);
}

/// `dynamic_activation_thresholds_tests.cpp:20-27`: `threshold(attempt) = max(th_start -
/// attempt^2 * window / 100 / 5, th_end)`, which Core's versionbits cache must report for every
/// period of the regtest deployment `testdummy:0:999999999999:0:100:80:60:5:0`.
#[test]
fn dynamic_thresholds_follow_the_documented_formula() {
    let rows = records("dynamic_activation_thresholds_tests");
    assert_eq!(rows.len(), 13);
    for r in rows {
        let (attempt, window, start, end) =
            (n(&r, "attempt"), n(&r, "window"), n(&r, "th_start"), n(&r, "th_end"));
        let formula = (start - attempt * attempt * window / 100 / 5).max(end);
        assert_eq!(n(&r, "core_threshold"), formula, "attempt {attempt}");
    }
}

/// The `bitcoin` merkle rule Core's `ComputeMerkleRoot` uses: pair up, duplicate the last of an
/// odd level.
fn merkle_root(mut level: Vec<[u8; 32]>) -> [u8; 32] {
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        level = level
            .chunks(2)
            .map(|p| {
                let mut buf = [0u8; 64];
                buf[..32].copy_from_slice(&p[0]);
                buf[32..].copy_from_slice(&p[1]);
                sha256d::Hash::hash(&buf).to_byte_array()
            })
            .collect();
    }
    level[0]
}

/// `evo_simplifiedmns_tests.cpp:simplifiedmns_merkleroots`. `CalcHash` hashes the entry without the
/// leading `SER_NETWORK`-only `nVersion`, so the pre-image is the wire form minus its first two
/// bytes; the merkle root is over the entries sorted by `proRegTxHash`.
#[test]
fn sml_entry_hashes_and_root_follow_from_the_wire_form() {
    let rows = records("evo_simplifiedmns_tests");
    let entries: Vec<_> = rows.iter().filter(|r| s(r, "kind") == "entry").collect();
    assert_eq!(entries.len(), 15);
    let mut hashes = Vec::new();
    for e in &entries {
        let wire = parse_hex(s(e, "wire"));
        assert_eq!(u16::from_le_bytes([wire[0], wire[1]]) as i64, n(e, "version"));
        assert_eq!(
            &wire[2..34],
            &uint256_from_hex(s(e, "pro_reg_tx_hash")),
            "proRegTxHash is first"
        );
        let hash = sha256d::Hash::hash(&wire[2..]).to_byte_array();
        assert_eq!(uint256_to_hex(&hash), s(e, "entry_hash"), "entry {}", s(e, "pro_reg_tx_hash"));
        hashes.push(hash);
    }
    // `proRegTxHash` i is `%064x` of i, so ascending hash order is the loop order.
    let root = merkle_root(hashes);
    let root_row = rows.iter().find(|r| s(r, "kind") == "merkle_root").unwrap();
    assert_eq!(uint256_to_hex(&root), s(root_row, "merkle_root"));
    // The root Core's test has hard-coded since it was written.
    assert_eq!(
        s(root_row, "merkle_root"),
        "0bae2176078cf42fa3e1fda761d4255d1c1c54777c6a793d0ab2b07c85ed4022"
    );
}

fn compact_size(n: usize) -> Vec<u8> {
    assert!(n < 0xfd);
    vec![n as u8]
}

/// `evo_islock_tests.cpp:deserialize_instantlock_from_realdata2`: the request id is the double
/// SHA-256 of the serialization of `("islock", inputs)`, and the sign hash that of
/// `(llmqType, quorumHash, id, txid)` (`llmq::SignHash`); both are what Core printed.
#[test]
fn islock_request_id_and_sign_hash_follow_from_the_inputs() {
    let rows = records("evo_islock_tests");
    assert_eq!(rows.len(), 1);
    let r = &rows[0];

    let mut pre = compact_size("islock".len());
    pre.extend_from_slice(b"islock");
    let inputs = r.find_value("inputs").get_array().unwrap();
    pre.extend(compact_size(inputs.len()));
    for input in inputs {
        pre.extend_from_slice(&uint256_from_hex(input.at(0).get_str().unwrap()));
        pre.extend_from_slice(&(input.at(1).get_i64().unwrap() as u32).to_le_bytes());
    }
    let request_id = sha256d::Hash::hash(&pre).to_byte_array();
    assert_eq!(uint256_to_hex(&request_id), s(r, "request_id"));

    let mut pre = vec![n(r, "llmq_type") as u8];
    pre.extend_from_slice(&uint256_from_hex(s(r, "quorum_hash")));
    pre.extend_from_slice(&request_id);
    pre.extend_from_slice(&uint256_from_hex(s(r, "txid")));
    assert_eq!(uint256_to_hex(&sha256d::Hash::hash(&pre).to_byte_array()), s(r, "sign_hash"));

    // The wire form starts with the version byte and the input count, and ends with the signature.
    let wire = parse_hex(s(r, "islock_hex"));
    assert_eq!(wire[0] as i64, n(r, "version"));
    let signature = parse_hex(s(r, "signature"));
    assert_eq!(signature.len(), 96);
    assert!(
        s(r, "islock_hex").contains(&hex_str(&signature)),
        "the BLS signature is part of the wire form"
    );
}

/// The proof embedded in `quorum_proofs_tests.cpp` is the one `test/functional/data/quorum_proof.json`
/// carries (`feature_quorum_proof_chain.py`), and Core verified it up to the recorded height.
#[test]
fn quorum_proof_is_the_functional_test_proof() {
    let rows = records("quorum_proofs_tests");
    let r = &rows[0];
    let functional =
        UniValue::parse(&read_core_text("test/functional/data/quorum_proof.json")).unwrap();
    assert_eq!(s(r, "proof_hex"), functional.find_value("proof_hex").get_str().unwrap());
    assert_eq!(n(r, "wire_bytes") as usize * 2, s(r, "proof_hex").len());
    assert_eq!(
        n(r, "anchor_height"),
        functional.find_value("checkpoint").find_value("height").get_i64().unwrap()
    );
    assert_eq!(
        n(r, "verified_target_height"),
        functional.find_value("target").find_value("height").get_i64().unwrap()
    );
    assert_eq!(n(r, "verified_target_height"), 1_549_547);
}

#[test]
fn pow_and_subsidy_records_are_complete() {
    let pow = records("pow_tests");
    let headers = pow.iter().find(|r| s(r, "kind") == "mainnet_headers").unwrap();
    assert_eq!(headers.find_value("headers_time_bits").len(), 24);
    let next: Vec<_> = pow.iter().filter(|r| s(r, "kind") == "next_work").collect();
    assert_eq!(next.len(), 6);
    for r in next {
        assert_eq!(n(r, "expected_bits"), n(r, "core_bits"));
    }
    let subsidy = records("subsidy_tests");
    assert_eq!(subsidy.len(), 12);
    // Block 4249 at low difficulty: the v20 rules change the subsidy regardless of height.
    assert_eq!(n(&subsidy[0], "subsidy"), 50_000_000_000);
    assert_eq!(n(&subsidy[1], "subsidy"), 500_000_000);
}

#[test]
fn dip14_records_carry_the_published_vectors() {
    let rows = records("dip14_tests");
    assert_eq!(rows.len(), 4);
    for r in &rows {
        assert_eq!(s(r, "key").len(), 64);
        assert_eq!(s(r, "chain_code").len(), 64);
    }
    // DIP-14 test vector 1 (dashpay/dips dip-0014.md).
    assert_eq!(
        s(&rows[0], "key"),
        "e8781fdef72862968cd9a4d2df34edaf9dcc5b17629ec505f0d2d1a8ed6f9f09"
    );
}

/// `dip0020opcodes_tests.cpp` / `checkdatasig_tests.cpp`: every record is a script run on a stack
/// under a flag set, with the result Core's interpreter produced.
#[test]
fn script_run_records_are_wellformed() {
    for (name, count) in [("dip0020opcodes_tests", 4512), ("checkdatasig_tests", 214)] {
        let rows = records(name);
        assert_eq!(rows.len(), count, "{name}");
        for r in &rows {
            let ok = r.find_value("ok").get_bool().unwrap();
            let error = s(r, "error");
            assert_eq!(ok, error == "OK", "{name}: {r:?}");
            assert!(dash_core_vectors::script_error::is_core_script_error(error), "{error}");
            assert!(r.find_value("stack").get_array().is_ok());
        }
    }
    // `script_tests.json` cannot expect IMPOSSIBLE_ENCODING; the DIP0020 records do.
    assert!(records("dip0020opcodes_tests").iter().any(|r| s(r, "error") == "IMPOSSIBLE_ENCODING"));
    // The positive CHECKDATASIG signatures that script_tests.json lacks.
    let cds = records("checkdatasig_tests");
    assert!(cds.iter().any(|r| s(r, "script") == "ba"
        && r.find_value("ok").get_bool().unwrap()
        && r.find_value("stack").len() == 3
        && r.find_value("stack").at(0).get_str().unwrap().len() > 2));
}

/// The harness audit (`o4_harness_audit_tests.cpp`) ran the flag-perturbation checks of Core's
/// harness over every defined flag against Core's interpreter. The corrected checks hold for
/// `script_tests.json` and `tx_valid.json`; for `tx_invalid.json` the monotonicity checks hold
/// but the "listed flags are minimal" check does not (so `load_tx_invalid` does not promise it).
#[test]
fn harness_audit_results() {
    let rows = records("harness_audit");
    let audit = &rows[0];
    assert_eq!(audit.find_value("defined_flags").get_str().unwrap().split(',').count(), 13);
    for file in ["script_tests", "tx_valid"] {
        let t = audit.find_value(file);
        assert_eq!(n(t, "violation_count"), 0, "{file}");
        assert!(n(t, "checks") > 10_000, "{file}");
    }
    let invalid = audit.find_value("tx_invalid");
    assert_eq!(n(invalid, "violation_count"), 64);
    let violations = invalid.find_value("violations").get_array().unwrap();
    let mut rows = std::collections::BTreeSet::new();
    let (mut with_no_flags, mut p2sh_dropped) = (0, 0);
    for v in violations {
        assert_eq!(s(v, "check"), "listed_flag_is_needed");
        rows.insert(n(v, "row"));
        match s(v, "flags") {
            // Dropping the only listed flag leaves the transaction failing with no flags at all.
            "NONE" => with_no_flags += 1,
            // The set that still fails has no P2SH, which the row lists next to the flag it needs.
            flags if !flags.split(',').any(|f| f == "P2SH") => p2sh_dropped += 1,
            _ => {}
        }
    }
    // 61 of the 66 script-checked vectors have a listed flag that is not needed: 9 fail with no
    // flags, in 52 further violations dropping P2SH still fails, and 3 drop another flag.
    assert_eq!(rows.len(), 61);
    assert_eq!((with_no_flags, p2sh_dropped), (9, 52));
}

#[test]
fn block_reward_reallocation_records_reach_the_final_state() {
    let rows = records("block_reward_reallocation_tests");
    assert_eq!(rows.len(), 71);
    // 2358 is the height Core's test pins; subsidy plus superblock subsidy is 164512828.
    let last = rows.last().unwrap();
    assert_eq!(s(last, "label"), "mn_rr_activated");
    assert_eq!(n(last, "height"), 2358);
    assert_eq!(n(last, "block_subsidy") + n(last, "superblock_subsidy"), 164_512_828);
    // era 2 = EvoReward: the masternode payment is split into a Core and a platform output.
    assert_eq!(n(last, "era"), 2);
    assert_eq!(last.find_value("template_masternode_payments").len(), 2);
}
