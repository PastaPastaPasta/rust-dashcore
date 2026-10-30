//! The script-error names the `expected_error` column of `script_tests.json` uses.
//!
//! This is `script_errors[]` from Dash Core's `script_tests.cpp`. Dash adds `CHECKDATASIGVERIFY`,
//! `SPLIT_RANGE`, `OPERAND_SIZE`, `INVALID_NUMBER_RANGE`, `DIV_BY_ZERO` and `MOD_BY_ZERO` to
//! Bitcoin's table. Note that `SCRIPT_ERR_SIG_NULLFAIL` is spelled `NULLFAIL` here.
//!
//! `SCRIPT_ERR_IMPOSSIBLE_ENCODING` (raised by `OP_NUM2BIN`/`OP_BIN2NUM` when a number cannot be
//! encoded in the requested width) is *not* in that table, so `script_tests.json` can never
//! expect it. It does occur: the DIP0020 opcode records (`transcribed/dip0020opcodes_tests.json`)
//! carry it, as [`IMPOSSIBLE_ENCODING`].

/// Names in the order of `script_errors[]`.
pub const SCRIPT_ERROR_NAMES: [&str; 40] = [
    "OK",
    "UNKNOWN_ERROR",
    "EVAL_FALSE",
    "OP_RETURN",
    "SCRIPT_SIZE",
    "PUSH_SIZE",
    "OP_COUNT",
    "STACK_SIZE",
    "SIG_COUNT",
    "PUBKEY_COUNT",
    "VERIFY",
    "EQUALVERIFY",
    "CHECKMULTISIGVERIFY",
    "CHECKSIGVERIFY",
    "CHECKDATASIGVERIFY",
    "NUMEQUALVERIFY",
    "BAD_OPCODE",
    "DISABLED_OPCODE",
    "INVALID_STACK_OPERATION",
    "INVALID_ALTSTACK_OPERATION",
    "UNBALANCED_CONDITIONAL",
    "NEGATIVE_LOCKTIME",
    "UNSATISFIED_LOCKTIME",
    "SIG_HASHTYPE",
    "SIG_DER",
    "MINIMALDATA",
    "SIG_PUSHONLY",
    "SIG_HIGH_S",
    "SIG_NULLDUMMY",
    "PUBKEYTYPE",
    "CLEANSTACK",
    "NULLFAIL",
    "DISCOURAGE_UPGRADABLE_NOPS",
    "SPLIT_RANGE",
    "OPERAND_SIZE",
    "INVALID_NUMBER_RANGE",
    "DIV_BY_ZERO",
    "MOD_BY_ZERO",
    "OP_CODESEPARATOR",
    "SIG_FINDANDDELETE",
];

/// The one Core script error the JSON harness has no name for.
pub const IMPOSSIBLE_ENCODING: &str = "IMPOSSIBLE_ENCODING";

/// `ParseScriptError`: whether `name` is in the harness table. Core reports an unknown name in
/// `script_tests.json` as a bad test, so this is what that file may use.
pub fn is_known_script_error(name: &str) -> bool {
    SCRIPT_ERROR_NAMES.contains(&name)
}

/// Every name Core's interpreter can report: the harness table plus [`IMPOSSIBLE_ENCODING`].
pub fn is_core_script_error(name: &str) -> bool {
    is_known_script_error(name) || name == IMPOSSIBLE_ENCODING
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_additions_are_present() {
        for n in [
            "CHECKDATASIGVERIFY",
            "SPLIT_RANGE",
            "OPERAND_SIZE",
            "INVALID_NUMBER_RANGE",
            "DIV_BY_ZERO",
            "MOD_BY_ZERO",
        ] {
            assert!(is_known_script_error(n), "{n}");
        }
        assert!(!is_known_script_error(IMPOSSIBLE_ENCODING));
        assert!(is_core_script_error(IMPOSSIBLE_ENCODING));
        assert!(!is_known_script_error("SIG_NULLFAIL"));
    }
}
