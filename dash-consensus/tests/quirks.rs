//! The register quirks CS1 owns (R4 §1.4; CS1 §5.1): 4 (OP_CHECKDATASIG), 5 (NUM2BIN, exceptions
//! and unexecuted failures) and 7 (the ECDSA layer). Quirk 6 (the signature hash) and quirk 39
//! (scriptCode serialization of a malformed tail) need transactions and come with CS1 phase B.
//!
//! Each test names the quirk's cases, asserts what Dash Core computes for them (recorded from Core
//! on both pins in `data/script_recorded.json`, so the expectation is Core's, not ours) and that
//! the port reproduces it.

#[path = "support/script.rs"]
mod script_support;

use std::cell::Cell;

use dash_consensus::script::{
    NoSignatureChecker, PubKeyRef, ScriptError, ScriptFlags, ScriptNum, SignatureChecker,
    check_low_s, eval_script, opcodes, parse_der_lax, script_error_code, script_sigop_count,
    verify_lax,
};
use script_support::{boolean, bytes, flags, int, record, stack};

/// Runs a recorded `eval` case through the port, checks it reproduces Core and returns Core's
/// error code (0 for success) and final stack.
fn eval_case(name: &str) -> (i32, Vec<Vec<u8>>) {
    let r = record("eval", name);
    let mut got = stack(r, "stack");
    let result = eval_script(&mut got, &bytes(r, "script"), flags(r), &NoSignatureChecker);
    let (core_error, core_stack) = (int(r, "error") as i32, stack(r, "result_stack"));
    assert_eq!(script_error_code(&result), core_error, "{name}: error");
    assert_eq!(got, core_stack, "{name}: stack");
    (core_error, core_stack)
}

const OK: i32 = 0;

/// A checker that counts every call, so a test can show `OP_CHECKDATASIG` never reaches it.
#[derive(Default)]
struct CountingChecker(Cell<u32>);

impl SignatureChecker for CountingChecker {
    fn check_sig(&self, _sig: &[u8], _pubkey: &[u8], _script_code: &[u8]) -> bool {
        self.0.set(self.0.get() + 1);
        true
    }
}

#[test]
fn quirk_04_checkdatasig() {
    use ScriptError::*;
    // DER plus exactly one trailing byte under DERSIG, any value at consensus; STRICTENC alone
    // still checks it as a hash type; no byte or two bytes are not DER.
    let dersig = ScriptFlags::DERSIG.bits();
    let strictenc = ScriptFlags::STRICTENC.bits();
    for (variant, under_dersig, under_strictenc) in [
        ("no trailing byte", SigDer.code(), SigDer.code()),
        ("trailing 01", OK, OK),
        ("trailing ff", OK, SigHashtype.code()),
        ("trailing 01 02", SigDer.code(), SigDer.code()),
    ] {
        let (e, _) = eval_case(&format!("checkdatasig {variant}, flags {dersig}"));
        assert_eq!(e, under_dersig, "{variant} under DERSIG");
        let (e, _) = eval_case(&format!("checkdatasig {variant}, flags {strictenc}"));
        assert_eq!(e, under_strictenc, "{variant} under STRICTENC");
        // Without any encoding flag the lax parser accepts every form, the trailing bytes included.
        assert_eq!(eval_case(&format!("checkdatasig {variant}, flags 0")), (OK, vec![vec![1]]));
    }
    // LOW_S checks S without the trailing byte.
    let low_s = (ScriptFlags::DERSIG | ScriptFlags::LOW_S).bits();
    assert_eq!(eval_case(&format!("checkdatasig high S + 01, flags {low_s}")).0, SigHighS.code());
    // A single SHA-256 of the message: a signature over SHA256d verifies nothing.
    assert_eq!(eval_case("checkdatasig signed over SHA256d instead"), (OK, vec![vec![]]));
    // NULLFAIL, and the VERIFY form.
    assert_eq!(eval_case("checkdatasig wrong message under NULLFAIL").0, SigNullFail.code());
    assert_eq!(eval_case("checkdatasigverify wrong message").0, CheckDataSigVerify.code());
    assert_eq!(eval_case("checkdatasig empty signature under NULLFAIL"), (OK, vec![vec![]]));

    // Never the checker, so never the signature cache: a checker that accepts everything changes
    // nothing and is never called.
    let r = record("eval", "checkdatasig wrong message");
    let checker = CountingChecker::default();
    let mut got = Vec::new();
    let result = eval_script(&mut got, &bytes(r, "script"), ScriptFlags::NONE, &checker);
    assert_eq!((result, got, checker.0.get()), (Ok(()), vec![vec![]], 0));

    // Zero sigops, in both counting modes.
    let script = [opcodes::OP_CHECKDATASIG, opcodes::OP_CHECKDATASIGVERIFY];
    assert_eq!((script_sigop_count(&script, true), script_sigop_count(&script, false)), (0, 0));
}

#[test]
fn quirk_05_num2bin_exceptions_and_unexecuted_failures() {
    use ScriptError::*;
    // NUM2BIN's size goes through getint and then uint64_t: a negative size is huge.
    assert_eq!(eval_case("num2bin negative size").0, PushSize.code());
    assert_eq!(eval_case("num2bin 521").0, PushSize.code());
    // The number operand is minimized in place before the width check, which leaves it so.
    assert_eq!(
        eval_case("num2bin impossible encoding"),
        (ImpossibleEncoding.code(), vec![vec![1]])
    );
    assert_eq!(eval_case("num2bin pads and moves the sign"), (OK, vec![vec![1, 0, 0, 0x80]]));
    // Exceptions are UNKNOWN_ERROR: a five-byte result is fine until a numeric op reads it.
    assert_eq!(eval_case("1ADD overflows to five bytes"), (OK, vec![vec![0, 0, 0, 0x80, 0]]));
    assert_eq!(eval_case("a five-byte result fails the next numeric op").0, UnknownError.code());
    assert_eq!(eval_case("non-minimal number operand under MINIMALDATA").0, UnknownError.code());
    // Failures in branches that do not execute.
    assert_eq!(eval_case("disabled opcode in a skipped branch").0, DisabledOpcode.code());
    assert_eq!(eval_case("VERIF in a skipped branch").0, BadOpcode.code());
    assert_eq!(eval_case("push 521 in a skipped branch").0, PushSize.code());
    assert_eq!(eval_case("VER in a skipped branch").0, OK);
    // BIN2NUM leaves its minimized operand on a range failure.
    let (e, stack) = eval_case("bin2num out of range");
    assert_eq!(e, InvalidNumberRange.code());
    assert_eq!(stack, vec![vec![1, 0, 0, 0, 1]]);
    // The numbers themselves.
    assert_eq!(ScriptNum::new(-1).encode(), vec![0x81]);
}

#[test]
fn quirk_07_ecdsa() {
    // (record, Core's verify, low_s, lax_ok)
    for (name, verify, low_s, lax_ok) in [
        // Normalize-S: high S verifies, but is not low.
        ("compressed / msg 1 / valid high S (msg 1)", true, false, true),
        // Lax DER: padding, trailing garbage and long length forms parse.
        ("compressed / msg 1 / padded R (msg 1)", true, true, true),
        ("compressed / msg 1 / trailing garbage (msg 1)", true, true, true),
        ("compressed / msg 1 / R length with 9 length bytes, 8 of them zero", true, true, true),
        // Four significant length bytes do not parse; three parse as a length that overruns.
        ("compressed / msg 1 / R length with four significant length bytes", false, false, false),
        ("compressed / msg 1 / R length with three significant length bytes", false, false, false),
        // An overflowing scalar zeroes both: parses, never verifies, counts as low S.
        ("compressed / msg 1 / s = n", false, true, true),
        ("compressed / msg 1 / r = n", false, true, true),
        ("compressed / msg 1 / high S with R = n", false, true, true),
        // Hybrid keys with the right parity verify; the wrong parity does not.
        ("hybrid / msg 1 / valid low S (msg 1)", true, true, true),
        ("hybrid with the wrong parity / msg 1 / valid low S (msg 1)", false, true, true),
        // GetLen validity: an uncompressed header with 33 bytes is not a key.
        ("uncompressed header, 33 bytes / msg 1 / valid low S (msg 1)", false, true, true),
    ] {
        let r = record("ecdsa", name);
        assert_eq!(boolean(r, "verify"), verify, "{name}: Core's verify");
        assert_eq!(boolean(r, "low_s"), low_s, "{name}: Core's low S");
        assert_eq!(boolean(r, "lax_ok"), lax_ok, "{name}: Core's lax parse");
        let (pubkey, sig) = (bytes(r, "pubkey"), bytes(r, "sig"));
        let msg: [u8; 32] = bytes(r, "msg").try_into().unwrap();
        let ours = PubKeyRef::new(&pubkey).is_some_and(|k| verify_lax(k, &msg, &sig));
        assert_eq!(ours, verify, "{name}: verify");
        assert_eq!(check_low_s(&sig), low_s, "{name}: low S");
        assert_eq!(parse_der_lax(&sig).is_some(), lax_ok, "{name}: lax parse");
    }
    // The zeroed signature is exactly 64 zero bytes.
    let r = record("ecdsa", "compressed / msg 1 / s = n");
    assert_eq!(parse_der_lax(&bytes(r, "sig")), Some([0; 64]));
}
