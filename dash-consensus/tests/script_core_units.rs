//! Dash Core's own unit tests of the script layer that need no transaction (CS1 §7.2), ported.
//! Anchors are v24.0.0-rc.3 `src/test/`. The expectations are the tests' own literals, except in
//! `scriptnum_tests`, whose reference implementation (`CScriptNum10`, `test/scriptnum10.h`) Core
//! runs in the recorder: its values are the `scriptnum10` section of `data/script_recorded.json`.
//!
//! The tests that need a transaction (`multisig_tests`, `script_p2sh_tests`,
//! `script_CHECKMULTISIG12`/`23`, `sighash_test`) arrive with the transaction checker (phase B).

#[path = "support/script.rs"]
mod script_support;

use std::collections::BTreeMap;

use dash_consensus::script::interpreter::MAX_SCRIPT_ELEMENT_SIZE;
use dash_consensus::script::opcodes::*;
use dash_consensus::script::{
    NoSignatureChecker, ScriptError, ScriptFlags, ScriptNum, eval_script, find_and_delete,
    is_push_only, push_data, script_sigop_count, script_sigop_count_with_sig, verify_script,
};
use dash_core_vectors::hex::{hex_str, parse_hex};
use script_support::{bytes, int, p2sh, record, recorded};
use secp256k1::{PublicKey, SecretKey};

// Core: script/script.h:398 CScript::push_int64 (`script << n`)
fn push_int(script: &mut Vec<u8>, n: i64) {
    if n == -1 || (1..=16).contains(&n) {
        script.push((n + i64::from(OP_1 - 1)) as u8);
    } else if n == 0 {
        script.push(OP_0);
    } else {
        push_data(script, &ScriptNum::new(n).encode());
    }
}

// ---------------------------------------------------------------------------------------------
// scriptnum_tests.cpp
// ---------------------------------------------------------------------------------------------

/// `values` and `offsets` (`scriptnum_tests.cpp:16-18`): no sum or difference overflows `i64`.
const VALUES: [i64; 13] = [
    0,
    1,
    -2,
    127,
    128,
    -255,
    256,
    (1 << 15) - 1,
    -(1 << 16),
    (1 << 24) - 1,
    1 << 31,
    1 - (1 << 32),
    1 << 40,
];
const OFFSETS: [i64; 9] = [1, 0x79, 0x80, 0x81, 0xFF, 0x7FFF, 0x8000, 0xFFFF, 0x10000];

/// `CScriptNum10` as Core computed it: `getvch()` and `getint()` of each value the tests build.
/// Its arithmetic and comparisons are those of the `int64_t` it holds.
struct ScriptNum10(BTreeMap<i64, (Vec<u8>, i32)>);

impl ScriptNum10 {
    fn recorded() -> Self {
        let rows = recorded("scriptnum10");
        Self(
            rows.iter()
                .map(|r| (int(r, "value"), (bytes(r, "vch"), int(r, "getint") as i32)))
                .collect(),
        )
    }

    fn get(&self, value: i64) -> &(Vec<u8>, i32) {
        self.0.get(&value).unwrap_or_else(|| panic!("CScriptNum10({value}) was not recorded"))
    }

    fn getint(&self, value: i64) -> i64 {
        i64::from(self.get(value).1)
    }

    /// `verify(bignum, scriptnum)` (`:20`): the same bytes and the same `getint()`.
    fn verify(&self, bignum: i64, scriptnum: ScriptNum) {
        let (vch, getint) = self.get(bignum);
        assert_eq!(hex_str(&scriptnum.encode()), hex_str(vch), "getvch of {bignum}");
        assert_eq!(scriptnum.getint(), *getint, "getint of {bignum}");
    }

    // Core: test/scriptnum_tests.cpp:25 CheckCreateVch
    fn check_create_vch(&self, num: i64) {
        let scriptnum = ScriptNum::new(num);
        self.verify(num, scriptnum);
        // CScriptNum10 reads its own bytes back as `num`.
        let scriptnum2 = ScriptNum::decode(&scriptnum.encode(), false, 4).unwrap();
        self.verify(num, scriptnum2);
        let scriptnum3 = ScriptNum::decode(&self.get(num).0, false, 4).unwrap();
        self.verify(num, scriptnum3);
    }

    // Core: test/scriptnum_tests.cpp:40 CheckCreateInt
    fn check_create_int(&self, num: i64) {
        let scriptnum = ScriptNum::new(num);
        let scriptnum_getint = i64::from(scriptnum.getint());
        self.verify(num, scriptnum);
        self.verify(self.getint(num), ScriptNum::new(scriptnum_getint));
        self.verify(scriptnum_getint, ScriptNum::new(self.getint(num)));
        self.verify(
            self.getint(scriptnum_getint),
            ScriptNum::new(i64::from(ScriptNum::new(self.getint(num)).getint())),
        );
    }

    // Core: test/scriptnum_tests.cpp:144 RunCreate
    fn run_create(&self, num: i64) {
        self.check_create_int(num);
        if ScriptNum::new(num).encode().len() <= 4 {
            self.check_create_vch(num);
        } else {
            // CScriptNum10's constructor throws on more than 4 bytes; so does the port's decode.
            self.verify(num, ScriptNum::new(num));
            assert!(ScriptNum::decode(&self.get(num).0, false, 4).is_err(), "{num}");
        }
    }

    // Core: test/scriptnum_tests.cpp:156 RunOperators. The port has no `CScriptNum + int64_t`
    // overload, so `ScriptNum::new(n)` stands in for the plain operand.
    fn run_operators(&self, num1: i64, num2: i64) {
        let (s1, s2) = (ScriptNum::new(num1), ScriptNum::new(num2));
        // CheckAdd (:51); int64 overflow is undefined, so those pairs are skipped.
        if let Some(sum) = num1.checked_add(num2) {
            self.verify(sum, s1 + s2);
            self.verify(sum, s2 + s1);
        }
        // CheckSubtract (:83)
        if let Some(diff) = num1.checked_sub(num2) {
            self.verify(diff, s1 - s2);
        }
        if let Some(diff) = num2.checked_sub(num1) {
            self.verify(diff, s2 - s1);
        }
        // CheckNegate (:73)
        if num1 != i64::MIN {
            self.verify(-num1, -s1);
        }
        // CheckCompare (:108)
        for (a, b, sa, sb) in [(num1, num1, s1, s1), (num1, num2, s1, s2)] {
            let expected = [a == b, a != b, a < b, a > b, a >= b, a <= b];
            assert_eq!([sa == sb, sa != sb, sa < sb, sa > sb, sa >= sb, sa <= sb], expected);
            assert_eq!([sa == b, sa != b, sa < b, sa > b, sa >= b, sa <= b], expected);
        }
    }
}

// Core: test/scriptnum_tests.cpp:164 creation
#[test]
fn scriptnum_tests_creation() {
    let reference = ScriptNum10::recorded();
    for value in VALUES {
        for offset in OFFSETS {
            reference.run_create(value);
            reference.run_create(value + offset);
            reference.run_create(value - offset);
        }
    }
}

// Core: test/scriptnum_tests.cpp:177 operators
#[test]
fn scriptnum_tests_operators() {
    let reference = ScriptNum10::recorded();
    for vi in VALUES {
        // Core indexes `values` with the offsets' index: the first nine values.
        for vj in VALUES.into_iter().take(OFFSETS.len()) {
            for (a, b) in [
                (vi, vi),
                (vi, -vi),
                (vi, vj),
                (vi, -vj),
                (vi + vj, vj),
                (vi + vj, -vj),
                (vi - vj, vj),
                (vi - vj, -vj),
                (vi + vj, vi + vj),
                (vi + vj, vi - vj),
                (vi - vj, vi + vj),
                (vi - vj, vi - vj),
            ] {
                reference.run_operators(a, b);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// sigopcount_tests.cpp
// ---------------------------------------------------------------------------------------------

// Core: test/sigopcount_tests.cpp:27 GetSigOpCount
#[test]
fn sigopcount_tests_get_sig_op_count() {
    let mut s1 = Vec::new();
    assert_eq!(script_sigop_count(&s1, false), 0);
    assert_eq!(script_sigop_count(&s1, true), 0);

    let dummy = [0u8; 20]; // a zero uint160
    s1.push(OP_1);
    push_data(&mut s1, &dummy);
    push_data(&mut s1, &dummy);
    s1.extend([OP_2, OP_CHECKMULTISIG]);
    assert_eq!(script_sigop_count(&s1, true), 2);
    s1.extend([OP_IF, OP_CHECKSIG, OP_ENDIF]);
    assert_eq!(script_sigop_count(&s1, true), 3);
    assert_eq!(script_sigop_count(&s1, false), 21);

    let mut script_sig = vec![OP_0];
    push_data(&mut script_sig, &s1);
    assert_eq!(script_sigop_count_with_sig(&p2sh(&s1), &script_sig), 3);

    // GetScriptForMultisig(1, keys) over three compressed keys. Core draws them at random; only
    // their push sizes matter here.
    let mut s2 = vec![OP_1];
    for secret in 1u8..=3 {
        let key = SecretKey::from_secret_bytes([secret; 32]).unwrap();
        push_data(&mut s2, &PublicKey::from_secret_key(&key).serialize());
    }
    s2.extend([OP_3, OP_CHECKMULTISIG]);
    assert_eq!(script_sigop_count(&s2, true), 3);
    assert_eq!(script_sigop_count(&s2, false), 20);

    let p2sh2 = p2sh(&s2);
    assert_eq!(script_sigop_count(&p2sh2, true), 0);
    assert_eq!(script_sigop_count(&p2sh2, false), 0);
    let mut script_sig2 = vec![OP_1];
    push_data(&mut script_sig2, &dummy);
    push_data(&mut script_sig2, &dummy);
    push_data(&mut script_sig2, &s2);
    assert_eq!(script_sigop_count_with_sig(&p2sh2, &script_sig2), 3);
}

// ---------------------------------------------------------------------------------------------
// script_tests.cpp
// ---------------------------------------------------------------------------------------------

fn eval(script: &[u8], flags: ScriptFlags) -> (Result<(), ScriptError>, Vec<Vec<u8>>) {
    let mut stack = Vec::new();
    let result = eval_script(&mut stack, script, flags, &NoSignatureChecker);
    (result, stack)
}

// Core: test/script_tests.cpp:909 script_PushData
#[test]
fn script_push_data() {
    // PUSHDATA1, PUSHDATA2 and PUSHDATA4 put the same value on the stack as a direct push.
    let (result, direct) = eval(&[1, 0x5a], ScriptFlags::P2SH);
    assert_eq!(result, Ok(()));
    let pushes: [&[u8]; 3] =
        [&[OP_PUSHDATA1, 1, 0x5a], &[OP_PUSHDATA2, 1, 0, 0x5a], &[OP_PUSHDATA4, 1, 0, 0, 0, 0x5a]];
    for script in pushes {
        assert_eq!(eval(script, ScriptFlags::P2SH), (Ok(()), direct.clone()), "{script:02x?}");
    }
    let truncated: [&[u8]; 3] =
        [&[OP_PUSHDATA1, 1], &[OP_PUSHDATA2, 1, 0], &[OP_PUSHDATA4, 1, 0, 0, 0]];
    for script in truncated {
        assert_eq!(eval(script, ScriptFlags::P2SH).0, Err(ScriptError::BadOpcode), "{script:02x?}");
    }
}

// Core: test/script_tests.cpp:951 script_cltv_truncated
#[test]
fn script_cltv_truncated() {
    let (result, _) = eval(&[OP_CHECKLOCKTIMEVERIFY], ScriptFlags::CHECKLOCKTIMEVERIFY);
    assert_eq!(result, Err(ScriptError::InvalidStackOperation));
}

// Core: test/script_tests.cpp:1213 script_standard_push
#[test]
fn script_standard_push() {
    let check = |script: &[u8], what: &str| {
        assert!(is_push_only(script), "{what} is not pure push");
        let result = verify_script(script, &[OP_1], ScriptFlags::MINIMALDATA, &NoSignatureChecker);
        assert_eq!(result, Ok(()), "{what} push is not minimal data");
    };
    for i in 0..67_000 {
        let mut script = Vec::new();
        push_int(&mut script, i);
        check(&script, &format!("number {i}"));
    }
    for i in 0..=MAX_SCRIPT_ELEMENT_SIZE {
        let mut script = Vec::new();
        push_data(&mut script, &vec![0o111; i]);
        check(&script, &format!("length {i}"));
    }
}

// Core: test/script_tests.cpp:1234 script_IsPushOnly_on_invalid_scripts
#[test]
fn script_is_push_only_on_invalid_scripts() {
    // A push truncated by the end of the script is not push-only.
    assert!(!is_push_only(&[1]));
}

// Core: test/script_tests.cpp:1283 script_FindAndDelete
#[test]
fn script_find_and_delete() {
    // (script, pattern, count, result); scripts built with `<<` are written as their bytes.
    let cases = [
        ("5152", "", 0, "5152"), // deleting nothing is a no-op
        ("515253", "52", 1, "5153"),
        ("535153535453", "53", 4, "5154"),
        ("0302ff03", "0302ff03", 1, ""),
        ("0302ff030302ff03", "0302ff03", 2, ""),
        ("0302ff030302ff03", "02", 0, "0302ff030302ff03"), // whole opcodes only
        ("0302ff030302ff03", "ff", 0, "0302ff030302ff03"),
        // An odd edge case: stripping the push-three-bytes prefix leaves 02ff03, a push of two.
        ("0302ff030302ff03", "03", 2, "02ff0302ff03"),
        // Byte sequences spanning several opcodes.
        ("02feed5169", "feed51", 0, "02feed5169"), // no match inside opcodes
        ("02feed5169", "02feed51", 1, "69"),
        ("516902feed5169", "feed51", 0, "516902feed5169"),
        ("516902feed5169", "02feed51", 1, "516969"),
        ("00005151", "0051", 1, "0051"), // single pass
        ("000051005151", "0051", 2, "0051"),
        // Another weird edge case: a script ending in a truncated push...
        ("0003feed", "03feed", 1, "00"), // ... can have the truncated push removed
        ("0003feed", "00", 1, "03feed"),
    ];
    for (i, (script, pattern, count, result)) in cases.into_iter().enumerate() {
        // The literals are Core's: the recorder ran the same cases through Core's FindAndDelete.
        let r = record("find_and_delete", &format!("script_FindAndDelete {}", i + 1));
        let core = (bytes(r, "script"), bytes(r, "pattern"), int(r, "count"), bytes(r, "result"));
        let ours = (parse_hex(script), parse_hex(pattern), count as i64, parse_hex(result));
        assert_eq!(ours, core, "case {}", i + 1);
        let mut s = parse_hex(script);
        assert_eq!(find_and_delete(&mut s, &parse_hex(pattern)), count, "{script} - {pattern}");
        assert_eq!(hex_str(&s), result, "{script} - {pattern}");
    }
}
