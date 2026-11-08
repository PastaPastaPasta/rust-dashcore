//! Core-checked regressions and a seeded random corpus (CS1 §7.2): what Dash Core's own script
//! code computes, recorded on both pinned releases by `contrib/node/cs1-script-recorder` (see the
//! file's `provenance`), compared field by field with the port.
//!
//! The named cases are the behaviours the CS1 design and phase-A reviews probed: CHECKMULTISIG's
//! key-count charge and cleanup order, failure stacks, limits, skipped-branch rules, encoding
//! precedence, FindAndDelete's cursor, both sigop overloads, every GetScriptOp truncation form,
//! the ECDSA edge scalars and deep conditionals. The `eval_mock` and `verify_mock` sections run
//! with [`MockChecker`], so the success side of the signature and lock-time opcodes, and the
//! scriptCode the checker receives, are compared with Core too. Phase B adds differentials against
//! O1b's oracle entry points; this fixture stays as the regression set (CS1 design §9).

#[path = "support/script.rs"]
mod script_support;

use dash_consensus::script::opcodes::{OP_INVALIDOPCODE, op_name};
use dash_consensus::script::{
    DirectVerifier, NoSignatureChecker, PubKeyRef, ScriptNum, SigVerifier, SignatureChecker,
    check_low_s, check_minimal_push, check_signature_encoding, eval_script, find_and_delete,
    get_op, instructions, parse_der_lax, push_data, script_error_code, script_error_string,
    script_sigop_count, script_sigop_count_with_sig, verify_lax, verify_script,
};
use dash_core_vectors::hex::hex_str;
use dash_core_vectors::univalue::UniValue;
use script_support::{MockChecker, boolean, bytes, flags, int, name, recorded, show, stack};

/// The records of one section, checked by `check`, which returns a description of the first
/// mismatching field. Fails with every mismatch listed.
fn run(section: &str, check: impl Fn(&UniValue) -> Option<String>) {
    let rows = recorded(section);
    assert!(!rows.is_empty(), "{section} is empty");
    let failures: Vec<String> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| check(row).map(|why| format!("{section}[{i}] {}: {why}", name(row))))
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} records differ from Core:\n{}",
        failures.len(),
        rows.len(),
        failures.join("\n")
    );
}

/// `Some(description)` when `ours != core`.
fn differ<T: PartialEq + std::fmt::Debug>(what: &str, ours: T, core: T) -> Option<String> {
    (ours != core).then(|| format!("{what}: ours {ours:?}, Core {core:?}"))
}

#[test]
fn error_strings() {
    run("error_strings", |row| {
        let code = int(row, "code") as i32;
        let text = row.find_value("text").get_str().unwrap();
        differ("text", script_error_string(code), text)
    });
}

/// An `EvalScript` section: the result, the error and the final stack, which Core leaves as it was
/// at the failure.
fn run_eval(section: &str, checker: &impl SignatureChecker) {
    run(section, |row| {
        let mut got = stack(row, "stack");
        let result = eval_script(&mut got, &bytes(row, "script"), flags(row), checker);
        differ("error", i64::from(script_error_code(&result)), int(row, "error"))
            .or_else(|| differ("ok", result.is_ok(), boolean(row, "ok")))
            .or_else(|| {
                let expected = stack(row, "result_stack");
                (got != expected)
                    .then(|| format!("stack: ours {}, Core {}", show(&got), show(&expected)))
            })
    });
}

/// A `VerifyScript` section: the result and the error.
fn run_verify(section: &str, checker: &impl SignatureChecker) {
    run(section, |row| {
        let (sig, spk) = (bytes(row, "script_sig"), bytes(row, "script_pubkey"));
        let result = verify_script(&sig, &spk, flags(row), checker);
        differ("error", i64::from(script_error_code(&result)), int(row, "error"))
            .or_else(|| differ("ok", result.is_ok(), boolean(row, "ok")))
    });
}

#[test]
fn eval_script_results_and_failure_stacks() {
    run_eval("eval", &NoSignatureChecker);
}

#[test]
fn verify_script_results() {
    run_verify("verify", &NoSignatureChecker);
}

#[test]
fn eval_script_with_the_mock_checker() {
    run_eval("eval_mock", &MockChecker);
}

#[test]
fn verify_script_with_the_mock_checker() {
    run_verify("verify_mock", &MockChecker);
}

#[test]
fn op_names_match_core() {
    run("op_names", |row| {
        let opcode = u8::try_from(int(row, "opcode")).unwrap();
        differ("GetOpName", op_name(opcode), row.find_value("name").get_str().unwrap())
    });
}

#[test]
fn find_and_delete_matches_core() {
    run("find_and_delete", |row| {
        let mut script = bytes(row, "script");
        let count = find_and_delete(&mut script, &bytes(row, "pattern"));
        differ("count", count as i64, int(row, "count"))
            .or_else(|| differ("result", hex_str(&script), hex_str(&bytes(row, "result"))))
    });
}

#[test]
fn sigop_counts_match_core() {
    run("sigops", |row| {
        let script = bytes(row, "script");
        differ("accurate", i64::from(script_sigop_count(&script, true)), int(row, "accurate"))
            .or_else(|| {
                let inaccurate = i64::from(script_sigop_count(&script, false));
                differ("inaccurate", inaccurate, int(row, "inaccurate"))
            })
    });
    run("sigops_with_sig", |row| {
        let n =
            script_sigop_count_with_sig(&bytes(row, "script_pubkey"), &bytes(row, "script_sig"));
        differ("count", i64::from(n), int(row, "count"))
    });
}

#[test]
fn get_op_steps_and_failure_cursor_match_core() {
    run("get_op", |row| {
        let script = bytes(row, "script");
        let steps = row.find_value("steps").get_array().unwrap();
        let mut pc = 0;
        for (i, step) in steps.iter().enumerate() {
            let op = get_op(&script, &mut pc);
            let invalid = i64::from(OP_INVALIDOPCODE);
            let (opcode, data) =
                op.map_or((invalid, Vec::new()), |op| (i64::from(op.opcode), op.data.to_vec()));
            let mismatch = differ("ok", op.is_some(), boolean(step, "ok"))
                .or_else(|| differ("opcode", opcode, int(step, "opcode")))
                .or_else(|| differ("data", hex_str(&data), hex_str(&bytes(step, "data"))))
                .or_else(|| differ("cursor", pc as i64, int(step, "cursor")));
            if let Some(why) = mismatch {
                return Some(format!("step {i}: {why}"));
            }
        }
        // `instructions` yields the instructions Core read before its first failure, then stays
        // finished.
        let core: Vec<i64> = steps
            .iter()
            .take_while(|step| boolean(step, "ok"))
            .map(|step| int(step, "opcode"))
            .collect();
        let mut iter = instructions(&script);
        let ours: Vec<i64> = iter.by_ref().map(|op| i64::from(op.opcode)).collect();
        differ("instructions", ours, core).or_else(|| differ("fused", iter.next().is_some(), false))
    });
}

#[test]
fn push_data_matches_core() {
    // CScript << vector: the opcode and length bytes Core writes, then the data.
    run("push_data", |row| {
        let data = vec![0x5a; int(row, "len") as usize];
        let mut script = Vec::new();
        push_data(&mut script, &data);
        let prefix = bytes(row, "prefix");
        differ("prefix", hex_str(&script[..prefix.len().min(script.len())]), hex_str(&prefix))
            .or_else(|| differ("data", script.get(prefix.len()..) == Some(&data[..]), true))
    });
}

#[test]
fn check_minimal_push_matches_core() {
    run("check_minimal_push", |row| {
        let data = vec![int(row, "byte") as u8; int(row, "len") as usize];
        let opcode = u8::try_from(int(row, "opcode")).unwrap();
        differ("minimal", check_minimal_push(&data, opcode), boolean(row, "minimal"))
    });
}

#[test]
fn script_numbers_match_core() {
    run("script_num_decode", |row| {
        let vch = bytes(row, "vch");
        let max_size = int(row, "max_size") as usize;
        let decoded = ScriptNum::decode(&vch, boolean(row, "minimal"), max_size);
        differ("ok", decoded.is_ok(), boolean(row, "ok"))
            .or_else(|| {
                decoded.ok().and_then(|n| {
                    differ("value", n.value(), int(row, "value"))
                        .or_else(|| differ("getint", i64::from(n.getint()), int(row, "getint")))
                })
            })
            .or_else(|| {
                let minimal = ScriptNum::is_minimally_encoded(&vch, max_size);
                differ("is_minimally_encoded", minimal, boolean(row, "is_minimally_encoded"))
            })
    });
    run("script_num_encode", |row| {
        let ours = ScriptNum::new(int(row, "value")).encode();
        differ("bytes", hex_str(&ours), hex_str(&bytes(row, "bytes")))
    });
    run("minimally_encode", |row| {
        let mut data = bytes(row, "vch");
        let changed = ScriptNum::minimally_encode(&mut data);
        differ("changed", changed, boolean(row, "changed"))
            .or_else(|| differ("result", hex_str(&data), hex_str(&bytes(row, "result"))))
    });
}

#[test]
fn ecdsa_layer_matches_core() {
    run("ecdsa", |row| {
        let (pubkey, sig) = (bytes(row, "pubkey"), bytes(row, "sig"));
        let msg: [u8; 32] = bytes(row, "msg").try_into().expect("32-byte message");
        // CPubKey(pubkey).Verify: an invalid shape is false.
        let verify = PubKeyRef::new(&pubkey).is_some_and(|key| verify_lax(key, &msg, &sig));
        // TransactionSignatureChecker's VerifySignature is CPubKey::Verify too.
        let direct =
            PubKeyRef::new(&pubkey).is_some_and(|key| DirectVerifier.verify(&sig, key, &msg));
        let lax = parse_der_lax(&sig);
        differ("verify", verify, boolean(row, "verify"))
            .or_else(|| differ("DirectVerifier", direct, boolean(row, "verify")))
            .or_else(|| differ("low_s", check_low_s(&sig), boolean(row, "low_s")))
            .or_else(|| differ("lax_ok", lax.is_some(), boolean(row, "lax_ok")))
            .or_else(|| {
                let compact = lax.map_or_else(String::new, |c| hex_str(&c));
                differ("lax_compact", compact, hex_str(&bytes(row, "lax_compact")))
            })
    });
}

#[test]
fn signature_encoding_matches_core() {
    run("signature_encoding", |row| {
        let result = check_signature_encoding(&bytes(row, "sig"), flags(row));
        differ("error", i64::from(script_error_code(&result)), int(row, "error"))
            .or_else(|| differ("ok", result.is_ok(), boolean(row, "ok")))
    });
}
