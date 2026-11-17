//! Properties of the script system (CS1 §7.2): no panics on any input, undefined flag bits
//! (including the retired DIP0020 bit 15) ignored, and removal monotonicity of every defined flag.

#[path = "support/script.rs"]
mod script_support;

use dash_consensus::script::opcodes::*;
use dash_consensus::script::{
    NoSignatureChecker, ScriptError, ScriptFlags, Stack, eval_script, push_data, verify_script,
};
use dash_core_vectors::flags::FlagRng;
use proptest::prelude::*;
use script_support::{bytes, flags, stack, transcribed};

/// Every bit Core does not define: the retired bit 15 and the rest.
const UNDEFINED: u32 = !ScriptFlags::DEFINED.bits();

/// Opcodes the interpreter treats specially, mixed with raw bytes.
fn script_strategy() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        4 => prop::sample::select(vec![
            OP_0, OP_1NEGATE, OP_RESERVED, OP_1, OP_2, OP_16, OP_NOP, OP_VER, OP_IF, OP_NOTIF,
            OP_VERIF, OP_ELSE, OP_ENDIF, OP_VERIFY, OP_RETURN, OP_TOALTSTACK, OP_FROMALTSTACK,
            OP_2DUP, OP_3DUP, OP_2ROT, OP_2SWAP, OP_DEPTH, OP_DROP, OP_DUP, OP_PICK, OP_ROLL,
            OP_TUCK, OP_CAT, OP_SPLIT, OP_NUM2BIN, OP_BIN2NUM, OP_SIZE, OP_AND, OP_XOR, OP_EQUAL,
            OP_1ADD, OP_NEGATE, OP_ADD, OP_DIV, OP_MOD, OP_WITHIN, OP_SHA256, OP_HASH160,
            OP_CODESEPARATOR, OP_CHECKSIG, OP_CHECKMULTISIG, OP_CHECKMULTISIGVERIFY, OP_NOP1,
            OP_CHECKLOCKTIMEVERIFY, OP_CHECKSEQUENCEVERIFY, OP_CHECKDATASIG, OP_INVERT, 0xbc, 0xff,
        ]).prop_map(|op| vec![op]),
        2 => proptest::collection::vec(any::<u8>(), 0..10).prop_map(|d| {
            let mut v = Vec::new();
            push_data(&mut v, &d);
            v
        }),
        1 => any::<u8>().prop_map(|b| vec![b]),
    ];
    proptest::collection::vec(piece, 0..24).prop_map(|p| p.concat())
}

fn stack_strategy() -> impl Strategy<Value = Stack> {
    proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..6), 0..5)
}

fn eval(script: &[u8], stack: &Stack, flags: u32) -> (Result<(), ScriptError>, Stack) {
    let mut stack = stack.clone();
    let r = eval_script(&mut stack, script, ScriptFlags::from_bits(flags), &NoSignatureChecker);
    (r, stack)
}

/// `TrimFlags`: CLEANSTACK only with P2SH.
fn trim(flags: u32) -> ScriptFlags {
    let f = ScriptFlags::from_bits(flags);
    if f.contains(ScriptFlags::P2SH) {
        f
    } else {
        f.difference(ScriptFlags::CLEANSTACK)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// No input panics: the interpreter turns everything into an error code.
    #[test]
    fn never_panics(script in proptest::collection::vec(any::<u8>(), 0..200), stack in stack_strategy(), flags in any::<u32>()) {
        let _ = eval(&script, &stack, flags);
        let _ = verify_script(&script, &script, trim(flags), &NoSignatureChecker);
    }

    #[test]
    fn structured_scripts_never_panic(sig in script_strategy(), spk in script_strategy(), flags in any::<u32>()) {
        let _ = verify_script(&sig, &spk, trim(flags), &NoSignatureChecker);
    }

    /// Undefined bits, the retired DIP0020 bit 15 among them, change nothing.
    #[test]
    fn undefined_bits_are_ignored(script in script_strategy(), stack in stack_strategy(), flags in any::<u32>(), noise in any::<u32>()) {
        let defined = flags & !UNDEFINED;
        prop_assert_eq!(eval(&script, &stack, defined), eval(&script, &stack, defined | (noise & UNDEFINED)));
    }
}

/// CS1 §4.1: every L0 `EvalScript` record gives the same result with bit 15 (the retired
/// `SCRIPT_ENABLE_DIP0020_OPCODES`) toggled, and with all undefined bits set.
#[test]
fn transcribed_records_ignore_bit_15() {
    for file in ["dip0020opcodes_tests", "checkdatasig_tests"] {
        for (i, r) in transcribed(file).iter().enumerate() {
            let (script, stack, flags) = (bytes(r, "script"), stack(r, "stack"), flags(r).bits());
            let base = eval(&script, &stack, flags);
            assert_eq!(eval(&script, &stack, flags ^ (1 << 15)), base, "{file}[{i}] bit 15");
            assert_eq!(eval(&script, &stack, flags | UNDEFINED), base, "{file}[{i}] undefined");
        }
    }
}

/// A spend that is often valid and whose validity depends on the flags: non-minimal pushes
/// (MINIMALDATA), NOPs and lock times (DISCOURAGE, CLTV, CSV), failing signature checks (NULLFAIL,
/// encodings), non-push scriptSigs (SIGPUSHONLY), P2SH spends with extra items (P2SH, CLEANSTACK).
fn flag_sensitive_spend(rng: &mut FlagRng) -> (Vec<u8>, Vec<u8>) {
    let mut pick = |n: u32| rng.next_u32() % n;
    let mut sig = Vec::new();
    for _ in 0..pick(3) {
        match pick(4) {
            0 => sig.push(OP_1),
            1 => sig.extend([OP_PUSHDATA1, 1, 7]),
            2 => sig.push(OP_NOP),
            _ => push_data(&mut sig, &[0x30, 0x01]),
        }
    }
    let mut body = Vec::new();
    for _ in 0..pick(4) {
        match pick(9) {
            0 => body.push(OP_NOP),
            1 => body.push(OP_NOP1),
            2 => body.extend([OP_1, OP_CHECKLOCKTIMEVERIFY, OP_DROP]),
            3 => body.extend([OP_1NEGATE, OP_CHECKSEQUENCEVERIFY, OP_DROP]),
            4 => body.extend([OP_PUSHDATA1, 1, 7, OP_DROP]),
            5 => body.extend([0x02, 0x30, 0x01, 0x02, 0x02, 0x02, OP_CHECKSIG, OP_DROP]),
            6 => body.extend([OP_0, 0x02, 0x02, 0x02, OP_CHECKSIG, OP_DROP]),
            7 => body.extend([OP_1, OP_DUP, OP_DROP]),
            _ => body.extend([OP_CODESEPARATOR]),
        }
    }
    body.push(OP_1);
    if pick(3) == 0 {
        // Pay to the body's script hash, spent with the redeem script as the last push.
        let spk = script_support::p2sh(&body);
        push_data(&mut sig, &body);
        (sig, spk)
    } else {
        (sig, body)
    }
}

/// Removal monotonicity: a spend valid under some flags stays valid with any one defined flag
/// removed (CLEANSTACK goes with P2SH). CS1 §6.2's one-direction argument rests on it, and O4's
/// audit found no violation in Core over the vector files.
#[test]
fn removing_a_flag_never_invalidates() {
    let mut rng = FlagRng::new(0xC51);
    let defined: Vec<u32> =
        (0..32).map(|b| 1u32 << b).filter(|b| b & ScriptFlags::DEFINED.bits() != 0).collect();
    let (mut valid, mut cases) = (0, 0);
    for _ in 0..20_000 {
        let (sig, spk) = flag_sensitive_spend(&mut rng);
        let flags = trim(rng.random_defined_flags());
        cases += 1;
        if verify_script(&sig, &spk, flags, &NoSignatureChecker).is_err() {
            continue;
        }
        valid += 1;
        for bit in &defined {
            let fewer = trim(flags.bits() & !bit);
            assert_eq!(
                verify_script(&sig, &spk, fewer, &NoSignatureChecker),
                Ok(()),
                "valid under {:#x}, not under {:#x}: sig {sig:02x?} spk {spk:02x?}",
                flags.bits(),
                fewer.bits()
            );
        }
    }
    // Not vacuous: a good share of the spends are valid under their flags.
    assert!(valid * 10 >= cases, "only {valid} of {cases} spends were valid");
}
