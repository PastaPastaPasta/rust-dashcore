//! Smoke tests of the script entry points (O1b), for every pin, against what Dash Core's
//! own tests compute:
//! - the EvalScript runs recorded from dip0020opcodes_tests.cpp and checkdatasig_tests.cpp
//!   (dash-core-vectors' transcribed records);
//! - script_tests.json, tx_valid.json and tx_invalid.json;
//! - the FindAndDelete, GetSigOpCount and CScriptNum checks of script_tests.cpp,
//!   sigopcount_tests.cpp and scriptnum_tests.cpp, recorded by the same instrumentation
//!   (tests/data/core-tests/, see PROVENANCE.md);
//! - the mainnet/testnet sample transactions, which dashd accepted.
//!
//! Never from the oracle itself.

mod common;

use std::iter;

use bitcoin_hashes::{Hash, sha256};
use common::{Coverage, hex, load_data, load_records, transcribed};
use dash_core_vectors::cases::{
    TxFlags, credit_tx, load_script_tests, load_tx_invalid, load_tx_valid, spend_tx,
};
use dash_core_vectors::flags::{
    DEFINED_FLAGS, SCRIPT_VERIFY_CLEANSTACK, SCRIPT_VERIFY_LOW_S, SCRIPT_VERIFY_MINIMALDATA,
    SCRIPT_VERIFY_P2SH,
};
use dash_core_vectors::read_core_text;
use dash_oracle_sys::{ErrorKind, Oracle, SpentOutput, Verdict, ffi::SCRIPT_ERROR_NAMES};
use dashcore::consensus::encode::{deserialize, serialize};
use dashcore::{Block, Transaction};
use serde::Deserialize;

fn error_name(code: i32) -> &'static str {
    SCRIPT_ERROR_NAMES.get(code as usize).copied().unwrap_or("?")
}

#[derive(Deserialize)]
struct EvalRecord {
    script: String,
    stack: Vec<String>,
    flags: u32,
    ok: bool,
    error: String,
    result_stack: Vec<String>,
}

impl EvalRecord {
    /// The record for assertion messages.
    fn describe(&self) -> String {
        format!("script {} stack {:?} flags {:#x}", self.script, self.stack, self.flags)
    }
}

fn hex_stack(stack: &[String]) -> Vec<Vec<u8>> {
    stack.iter().map(|s| hex(s)).collect()
}

#[test]
fn eval_script_matches_core_evalscript_records() {
    for file in ["dip0020opcodes_tests.json", "checkdatasig_tests.json"] {
        let records: Vec<EvalRecord> = transcribed(file);
        for oracle in Oracle::all() {
            let mut coverage = Coverage::default();
            for r in &records {
                let got =
                    oracle.eval_script(&hex(&r.script), &hex_stack(&r.stack), r.flags).unwrap();
                let case = format!("{} {file} {}", oracle.pin(), r.describe());
                assert_eq!(got.ok, r.ok, "{case}");
                assert_eq!(error_name(got.error), r.error, "{case}");
                assert_eq!(got.stack, hex_stack(&r.result_stack), "{case}");
                coverage.hit(if r.ok {
                    "ok"
                } else {
                    "error"
                });
            }
            coverage.require(&[("ok", 50), ("error", 50)]);
        }
    }
}

/// libsecp256k1's compact form (r || s, 32 bytes each, big-endian) of a strict-DER
/// signature followed by its hash-type byte.
fn der_to_compact(sig: &[u8]) -> [u8; 64] {
    assert_eq!(sig[0], 0x30);
    let int = |at: usize| -> (&[u8], usize) {
        assert_eq!(sig[at], 0x02);
        let len = sig[at + 1] as usize;
        (&sig[at + 2..at + 2 + len], at + 2 + len)
    };
    let (r, next) = int(2);
    let (s, _) = int(next);
    let mut out = [0u8; 64];
    let (r_half, s_half) = out.split_at_mut(32);
    for (value, half) in [(r, r_half), (s, s_half)] {
        let value = &value[value.iter().take_while(|&&b| b == 0).count()..];
        half[32 - value.len()..].copy_from_slice(value);
    }
    out
}

/// OP_CHECKDATASIG (`ba`, `bb`, and `ba NOT VERIFY`) over (sig, message, pubkey): Core
/// checks CheckSignatureEncoding(sig), then CheckPubKeyEncoding, then
/// CPubKey(pubkey).Verify(SHA256(message), sig) for a non-empty sig.
#[test]
fn ecdsa_and_signature_encoding_match_core_checkdatasig_records() {
    let records: Vec<EvalRecord> = transcribed("checkdatasig_tests.json");
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for r in records.iter().filter(|r| r.stack.len() == 3) {
            let (sig, msg, pubkey) = (hex(&r.stack[0]), hex(&r.stack[1]), hex(&r.stack[2]));
            let digest = sha256::Hash::hash(&msg).to_byte_array();
            let case = format!("{} {}", oracle.pin(), r.describe());
            let encoding = oracle.check_signature_encoding(&sig, r.flags).unwrap();
            match r.error.as_str() {
                "SIG_DER" | "SIG_HIGH_S" | "SIG_HASHTYPE" => {
                    assert!(
                        !encoding.ok && error_name(encoding.error) == r.error,
                        "{case}: {encoding:?}"
                    );
                    if r.error == "SIG_HIGH_S" {
                        assert!(!oracle.check_low_s(&sig).unwrap(), "{case}");
                        coverage.hit("high S");
                    }
                    coverage.hit("bad encoding");
                    continue;
                }
                "INVALID_STACK_OPERATION" => continue,
                _ => assert!(encoding.ok, "{case}: {encoding:?}"),
            }
            coverage.hit("encoding passes");
            if r.error == "PUBKEYTYPE" || sig.is_empty() {
                continue;
            }
            // past both encoding checks with a signature: the result is Verify()'s
            let verified =
                (r.script == "ba" && r.result_stack == ["01"]) || (r.script == "bb" && r.ok);
            assert_eq!(oracle.pubkey_verify(&pubkey, &digest, &sig).unwrap(), verified, "{case}");
            if verified {
                assert_eq!(
                    oracle.parse_der_lax(&sig).unwrap(),
                    Some(der_to_compact(&sig)),
                    "{case}"
                );
                if r.flags & SCRIPT_VERIFY_LOW_S != 0 {
                    assert!(oracle.check_low_s(&sig).unwrap(), "{case}");
                    coverage.hit("low S");
                }
                coverage.hit("verifies");
            } else {
                coverage.hit("does not verify");
            }
        }
        assert_eq!(oracle.parse_der_lax(&[]).unwrap(), None);
        coverage.require(&[
            ("verifies", 4),
            ("does not verify", 20),
            ("high S", 4),
            ("low S", 2),
            ("bad encoding", 20),
            ("encoding passes", 50),
        ]);
    }
}

/// The opcode and data of `script` if it is a single push.
fn single_push(script: &[u8]) -> Option<(u8, &[u8])> {
    let (&op, rest) = script.split_first()?;
    let (len, rest) = match op {
        0x00..=0x4b => (op as usize, rest),
        0x4c => (*rest.first()? as usize, &rest[1..]),
        0x4d => (u16::from_le_bytes(rest.get(..2)?.try_into().ok()?) as usize, &rest[2..]),
        0x4e => (u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize, &rest[4..]),
        _ => return None,
    };
    (rest.len() == len).then_some((op, rest))
}

/// script_tests.json rows whose scriptSig is one push and whose scriptPubKey pushes
/// nothing: under MINIMALDATA the row fails with MINIMALDATA exactly when that push is
/// not minimal.
#[test]
fn check_minimal_push_matches_script_tests() {
    let rows = load_script_tests(&read_core_text("src/test/data/script_tests.json")).unwrap();
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for row in rows.iter().filter(|r| r.flags & SCRIPT_VERIFY_MINIMALDATA != 0) {
            let Some((op, data)) = single_push(&row.script_sig) else {
                continue;
            };
            if row.script_pub_key.iter().any(|&b| b <= 0x4e) {
                continue;
            }
            let minimal = oracle.check_minimal_push(data, op).unwrap();
            assert_eq!(
                minimal,
                row.expected_error != "MINIMALDATA",
                "{} row {}: {}",
                oracle.pin(),
                row.index,
                row.script_sig_text
            );
            coverage.hit(if minimal {
                "minimal"
            } else {
                "not minimal"
            });
        }
        coverage.require(&[("minimal", 3), ("not minimal", 10)]);
        assert_eq!(oracle.check_minimal_push(&[], 0x4f).unwrap_err().kind, ErrorKind::Argument);
    }
}

fn txid_of(tx: &[u8]) -> [u8; 32] {
    use dashcore::hashes::Hash as _;
    deserialize::<Transaction>(tx).unwrap().txid().to_byte_array()
}

/// O1 review M4: VerifyScript() reads CLEANSTACK only in its last check, where it asserts
/// P2SH. script_tests.json rows evaluated without flags fail identically with CLEANSTACK
/// alone (the shim must return Core's failure), and rows that pass reach the assert (the
/// shim refuses them).
#[test]
fn cleanstack_without_p2sh_is_refused_only_where_core_asserts() {
    let rows = load_script_tests(&read_core_text("src/test/data/script_tests.json")).unwrap();
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for row in rows.iter().filter(|r| r.flags == 0) {
            let credit = credit_tx(&row.script_pub_key);
            let spend = spend_tx(&row.script_sig, &txid_of(&credit));
            let got =
                oracle.verify_script(&row.script_pub_key, &spend, 0, 0, SCRIPT_VERIFY_CLEANSTACK);
            let case = format!(
                "{} row {}: {} / {}",
                oracle.pin(),
                row.index,
                row.script_sig_text,
                row.script_pub_key_text
            );
            if row.expected_error == "OK" {
                assert_eq!(got.unwrap_err().kind, ErrorKind::Argument, "{case}");
                coverage.hit("passes: refused");
            } else {
                let r = got.unwrap_or_else(|e| panic!("{case}: {e}"));
                assert!(!r.ok && error_name(r.error) == row.expected_error, "{case}: {r:?}");
                coverage.hit("fails: Core's error");
            }
        }
        coverage.require(&[("passes: refused", 50), ("fails: Core's error", 10)]);
    }
}

#[derive(Deserialize)]
struct FindAndDeleteRecord {
    script: String,
    pattern: String,
    count: i32,
    result: String,
}

#[derive(Deserialize)]
struct SigOpRecord {
    script: String,
    accurate: Option<bool>,
    script_sig: Option<String>,
    count: u32,
}

#[derive(Deserialize)]
struct ScriptNumRecord {
    value: i64,
    vch: String,
    getint: i32,
    decoded_getint: Option<i32>,
}

#[test]
fn find_and_delete_matches_core_script_tests() {
    let records: Vec<FindAndDeleteRecord> = load_records("core-tests/script_tests.json");
    assert_eq!(records.len(), 16);
    for oracle in Oracle::all() {
        for r in &records {
            let (script, count) =
                oracle.find_and_delete(&hex(&r.script), &hex(&r.pattern)).unwrap();
            assert_eq!(
                (script, count),
                (hex(&r.result), r.count),
                "{} {} - {}",
                oracle.pin(),
                r.script,
                r.pattern
            );
        }
    }
}

#[test]
fn sigop_counts_match_core_sigopcount_tests() {
    let records: Vec<SigOpRecord> = load_records("core-tests/sigopcount_tests.json");
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for r in &records {
            let script = hex(&r.script);
            let got = match (&r.script_sig, r.accurate) {
                (Some(sig), _) => {
                    coverage.hit("with scriptSig");
                    oracle.script_sigops_with_sig(&script, &hex(sig)).unwrap()
                }
                (None, Some(accurate)) => {
                    coverage.hit(if accurate {
                        "accurate"
                    } else {
                        "legacy"
                    });
                    oracle.script_sigops(&script, accurate).unwrap()
                }
                (None, None) => panic!("record without accurate or script_sig"),
            };
            assert_eq!(got, r.count, "{} {}", oracle.pin(), r.script);
        }
        coverage.require(&[("with scriptSig", 2), ("accurate", 4), ("legacy", 3)]);
    }
}

#[test]
fn scriptnum_matches_core_scriptnum_tests() {
    let records: Vec<ScriptNumRecord> = load_records("core-tests/scriptnum_tests.json");
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for r in &records {
            let vch = hex(&r.vch);
            let case = format!("{} value {}", oracle.pin(), r.value);
            // the reference encoding (CScriptNum10) is minimal by construction
            assert_eq!(oracle.scriptnum_encode(r.value).unwrap(), vch, "{case}");
            assert!(oracle.scriptnum_is_minimally_encoded(&vch, 8).unwrap(), "{case}");
            assert_eq!(
                oracle.scriptnum_minimally_encode(&vch).unwrap(),
                (vch.clone(), false),
                "{case}"
            );
            let decoded = oracle.scriptnum_decode(&vch, true, 8).unwrap().expect("decodes");
            assert_eq!((decoded.value, decoded.getint), (r.value, r.getint), "{case}");
            match r.decoded_getint {
                Some(getint) => {
                    let d =
                        oracle.scriptnum_decode(&vch, false, 4).unwrap().expect("4 bytes or fewer");
                    assert_eq!(d.getint, getint, "{case}");
                    coverage.hit("4 bytes");
                }
                None => {
                    // Core throws for more than nDefaultMaxNumSize bytes
                    assert_eq!(oracle.scriptnum_decode(&vch, false, 4).unwrap(), None, "{case}");
                    coverage.hit("longer");
                }
            }
        }
        coverage.require(&[("4 bytes", 20), ("longer", 5)]);
        // More than 8 bytes that pass the size check would make set_vch() shift by >= 64 bits.
        assert_eq!(
            oracle.scriptnum_decode(&[1; 9], false, 9).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(oracle.scriptnum_decode(&[1; 9], false, 8).unwrap(), None);
    }
}

/// OP_BIN2NUM is MinimallyEncode() followed by IsMinimallyEncoded(4), OP_DIV and OP_MOD
/// decode two 4-byte CScriptNums: their records pin both helpers.
#[test]
fn scriptnum_helpers_match_core_dip0020_records() {
    let records: Vec<EvalRecord> = transcribed("dip0020opcodes_tests.json");
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for r in &records {
            let stack = hex_stack(&r.stack);
            let case = format!("{} {}", oracle.pin(), r.describe());
            match (r.script.as_str(), stack.as_slice()) {
                ("81", [x]) => {
                    let (encoded, _) = oracle.scriptnum_minimally_encode(x).unwrap();
                    if r.ok {
                        assert_eq!(vec![encoded], hex_stack(&r.result_stack), "{case}");
                        coverage.hit("BIN2NUM");
                    } else if r.error == "INVALID_NUMBER_RANGE" {
                        assert!(
                            !oracle.scriptnum_is_minimally_encoded(&encoded, 4).unwrap(),
                            "{case}"
                        );
                        coverage.hit("BIN2NUM range");
                    }
                }
                ("96" | "97", [a, b]) => {
                    let minimal = r.flags & SCRIPT_VERIFY_MINIMALDATA != 0;
                    let (x, y) = (
                        oracle.scriptnum_decode(a, minimal, 4).unwrap(),
                        oracle.scriptnum_decode(b, minimal, 4).unwrap(),
                    );
                    match r.error.as_str() {
                        "UNKNOWN_ERROR" => {
                            assert!(x.is_none() || y.is_none(), "{case}");
                            coverage.hit("scriptnum error");
                        }
                        "DIV_BY_ZERO" => assert_eq!(y.map(|n| n.value), Some(0), "{case}"),
                        "OK" => {
                            let (x, y) = (x.unwrap().value, y.unwrap().value);
                            let q = if r.script == "96" {
                                x / y
                            } else {
                                x % y
                            };
                            assert_eq!(
                                vec![oracle.scriptnum_encode(q).unwrap()],
                                hex_stack(&r.result_stack),
                                "{case}"
                            );
                            coverage.hit("DIV/MOD");
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        coverage.require(&[
            ("BIN2NUM", 100),
            ("BIN2NUM range", 2),
            ("scriptnum error", 20),
            ("DIV/MOD", 100),
        ]);
    }
}

/// One `SpentOutput` per script in `scripts`, with `amounts` in order.
fn spent_outputs(
    amounts: impl IntoIterator<Item = i64>,
    scripts: &[Vec<u8>],
) -> Vec<SpentOutput<'_>> {
    amounts
        .into_iter()
        .zip(scripts)
        .map(|(amount, script_pubkey)| SpentOutput {
            amount,
            script_pubkey,
        })
        .collect()
}

/// The spent scriptPubKeys of `tx` as Core's tx_valid/tx_invalid harness builds them.
fn tx_case_spent(
    case: &dash_core_vectors::cases::TxCase,
    tx: &Transaction,
) -> Option<Vec<Vec<u8>>> {
    use dashcore::hashes::Hash as _;
    tx.input
        .iter()
        .map(|i| {
            case.prevout_script(&i.previous_output.txid.to_byte_array(), i.previous_output.vout)
                .map(<[u8]>::to_vec)
        })
        .collect()
}

#[test]
fn check_input_scripts_follows_tx_valid_and_tx_invalid() {
    let valid = load_tx_valid(&read_core_text("src/test/data/tx_valid.json")).unwrap();
    let invalid = load_tx_invalid(&read_core_text("src/test/data/tx_invalid.json")).unwrap();
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for (cases, expect_valid) in [(&valid, true), (&invalid, false)] {
            for case in cases.iter() {
                let TxFlags::Script(flags) = case.flags else {
                    continue;
                };
                // tx_valid lists the excluded flags; Core verifies with every other one
                let flags = if expect_valid {
                    !flags & DEFINED_FLAGS
                } else {
                    flags
                };
                if flags & SCRIPT_VERIFY_CLEANSTACK != 0 && flags & SCRIPT_VERIFY_P2SH == 0 {
                    continue;
                }
                let Ok(tx) = deserialize::<Transaction>(&case.tx) else {
                    continue;
                };
                let Some(scripts) = tx_case_spent(case, &tx) else {
                    continue;
                };
                if !oracle.check_transaction(&case.tx).unwrap().is_accept() {
                    coverage.hit("fails CheckTransaction");
                    continue;
                }
                // Core's harness spends outputs of value 0
                let spent = spent_outputs(iter::repeat(0), &scripts);
                // The adapter's contract: spent outputs no coins view can hold are refused
                // (CScript::IsUnspendable(): OP_RETURN first, or above MAX_SCRIPT_SIZE),
                // except for a coinbase, whose spent outputs Core never reads.
                let refused = !tx.is_coin_base()
                    && scripts.iter().any(|s| s.first() == Some(&0x6a) || s.len() > 10_000);
                let label = format!(
                    "{} {} row {}",
                    oracle.pin(),
                    if expect_valid {
                        "tx_valid"
                    } else {
                        "tx_invalid"
                    },
                    case.index
                );
                match oracle.check_input_scripts(&case.tx, &spent, flags) {
                    Ok(Verdict::Accept) => {
                        assert!(expect_valid && !refused, "{label}: accepted");
                        coverage.hit("valid");
                    }
                    Ok(Verdict::Reject(r)) => {
                        assert!(!expect_valid && !refused, "{label}: {r:?}");
                        assert!(
                            r.reason.starts_with("mandatory-script-verify-flag-failed (")
                                || r.reason.starts_with("non-mandatory-script-verify-flag ("),
                            "{label}: {r:?}"
                        );
                        coverage.hit("invalid");
                    }
                    Err(e) if e.kind == ErrorKind::Argument => {
                        assert!(refused, "{label}: refused: {e}");
                        coverage.hit("refused");
                    }
                    Err(e) => panic!("{label}: {e}"),
                }
            }
        }
        coverage.require(&[("valid", 80), ("invalid", 60)]);
    }
}

#[derive(Deserialize)]
struct Samples {
    blocks: Vec<SampleBlock>,
}

#[derive(Deserialize)]
struct SampleBlock {
    height: i32,
    hex: String,
    txs: Vec<SampleTx>,
}

#[derive(Deserialize)]
struct SampleTx {
    prevouts: Vec<Prevout>,
}

#[derive(Deserialize)]
struct Prevout {
    script_pubkey: String,
    amount: i64,
}

/// Every non-coinbase transaction of the mainnet/testnet samples passes
/// CheckInputScripts() with the consensus flags dashd applied (P2SH and the buried
/// deployments active at its height), and its sigop counts are consistent with the
/// per-script counts (themselves checked against Core's tests above).
#[test]
fn check_input_scripts_and_sigops_accept_the_chain() {
    for oracle in Oracle::all() {
        let mut coverage = Coverage::default();
        for net in ["mainnet", "testnet"] {
            let samples: Samples = load_data(&format!("{net}.json"));
            for b in &samples.blocks {
                let block: Block = deserialize(&hex(&b.hex)).unwrap();
                for (tx, s) in block.txdata.iter().zip(&b.txs).skip(1) {
                    if s.prevouts.is_empty() {
                        continue;
                    }
                    let bytes = serialize(tx);
                    let scripts: Vec<Vec<u8>> =
                        s.prevouts.iter().map(|p| hex(&p.script_pubkey)).collect();
                    let spent = spent_outputs(s.prevouts.iter().map(|p| p.amount), &scripts);
                    let case = format!("{} {net} {}", oracle.pin(), b.height);
                    // P2SH is always on in Dash; the other flags only tighten checks
                    assert_eq!(
                        oracle.check_input_scripts(&bytes, &spent, SCRIPT_VERIFY_P2SH).unwrap(),
                        Verdict::Accept,
                        "{case}"
                    );
                    let counts =
                        oracle.transaction_sigops(&bytes, &spent, SCRIPT_VERIFY_P2SH).unwrap();
                    assert_eq!(counts.legacy, oracle.legacy_sigops(&bytes).unwrap(), "{case}");
                    let p2sh: u32 = tx
                        .input
                        .iter()
                        .zip(&scripts)
                        .filter(|(_, spk)| {
                            spk.len() == 23 && spk[0] == 0xa9 && spk[1] == 0x14 && spk[22] == 0x87
                        })
                        .map(|(i, spk)| {
                            oracle.script_sigops_with_sig(spk, i.script_sig.as_bytes()).unwrap()
                        })
                        .sum();
                    assert_eq!((counts.p2sh, counts.total), (p2sh, counts.legacy + p2sh), "{case}");
                    assert_eq!(
                        oracle.transaction_sigops(&bytes, &spent, 0).unwrap().total,
                        counts.legacy,
                        "{case}"
                    );
                    coverage.hit(if p2sh > 0 {
                        "P2SH sigops"
                    } else {
                        "inputs"
                    });
                }
            }
        }
        coverage.require(&[("inputs", 50)]);
    }
}

#[derive(Deserialize)]
struct P2shSamples {
    transactions: Vec<P2shTx>,
    provenance: P2shProvenance,
}

#[derive(Deserialize)]
struct P2shTx {
    hex: String,
    sigops: u32,
    spent: Vec<Prevout>,
}

#[derive(Deserialize)]
struct P2shProvenance {
    dashd: String,
}

/// O1 review r2: transactions spending P2SH multisig outputs, with the sigop count the
/// releases' own block templates report: GetTransactionSigOpCount(tx, view,
/// STANDARD_SCRIPT_VERIFY_FLAGS), P2SH included (tests/data/regtest-p2sh-<pin>.json).
#[test]
fn transaction_sigops_match_dashd_block_templates() {
    for oracle in Oracle::all() {
        let data: P2shSamples = load_data(&format!("regtest-p2sh-{}.json", oracle.pin().name()));
        assert!(data.provenance.dashd.contains(&oracle.pin().name()[1..]));
        let mut coverage = Coverage::default();
        for t in &data.transactions {
            let tx = hex(&t.hex);
            let scripts: Vec<Vec<u8>> = t.spent.iter().map(|p| hex(&p.script_pubkey)).collect();
            let spent = spent_outputs(t.spent.iter().map(|p| p.amount), &scripts);
            let case = format!("{} {}", oracle.pin(), &t.hex[..16]);
            let counts = oracle.transaction_sigops(&tx, &spent, SCRIPT_VERIFY_P2SH).unwrap();
            assert_eq!(counts.total, t.sigops, "{case}: {counts:?}");
            assert_eq!(counts.total, counts.legacy + counts.p2sh, "{case}");
            // without P2SH, Core counts the legacy sigops only
            assert_eq!(
                oracle.transaction_sigops(&tx, &spent, 0).unwrap().total,
                counts.legacy,
                "{case}"
            );
            // dashd accepted it into its mempool with the standard flags, P2SH among them
            assert_eq!(
                oracle.check_input_scripts(&tx, &spent, SCRIPT_VERIFY_P2SH).unwrap(),
                Verdict::Accept,
                "{case}"
            );
            if counts.p2sh > 0 {
                coverage.hit("P2SH sigops");
            }
        }
        coverage.require(&[("P2SH sigops", 3)]);
    }
}

/// The adapters' contract (CS1 design 7.5): a prevout spent by two inputs is refused.
#[test]
fn a_prevout_spent_twice_is_refused() {
    let data: P2shSamples = load_data("regtest-p2sh-v24.0.json");
    let t = &data.transactions[0];
    let mut tx: Transaction = deserialize(&hex(&t.hex)).unwrap();
    tx.input.push(tx.input[0].clone());
    let bytes = serialize(&tx);
    let script = hex(&t.spent[0].script_pubkey);
    let spent = vec![
        SpentOutput {
            amount: t.spent[0].amount,
            script_pubkey: &script,
        };
        2
    ];
    for oracle in Oracle::all() {
        let e = oracle.transaction_sigops(&bytes, &spent, SCRIPT_VERIFY_P2SH).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{e}");
        let e = oracle.check_input_scripts(&bytes, &spent, SCRIPT_VERIFY_P2SH).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{e}");
    }
}
