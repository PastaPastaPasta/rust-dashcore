//! Script errors: Core's `ScriptError_t` (`script/script_error.h`) with its numeric values, the
//! names Core's JSON test harness uses, and `ScriptErrorString` (`script/script_error.cpp`).
//!
//! `Display` and `Error` are written by hand rather than derived with `thiserror`: the messages are
//! Core's strings verbatim, and this pure crate takes no proc-macro dependency for them.

use core::fmt;

/// Core's `ScriptError_t` without `SCRIPT_ERR_OK` (success is `Ok(())`).
///
/// Discriminants are Core's values, so an oracle's integer compares directly with [`Self::code`].
#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum ScriptError {
    UnknownError = 1,
    EvalFalse,
    OpReturn,
    // Max sizes
    ScriptSize,
    PushSize,
    OpCount,
    StackSize,
    SigCount,
    PubkeyCount,
    // Failed verify operations
    Verify,
    EqualVerify,
    CheckMultisigVerify,
    CheckSigVerify,
    CheckDataSigVerify,
    NumEqualVerify,
    // Logical/format/canonical errors
    BadOpcode,
    DisabledOpcode,
    InvalidStackOperation,
    InvalidAltstackOperation,
    UnbalancedConditional,
    // Operand checks
    InvalidSplitRange,
    InvalidOperandSize,
    InvalidNumberRange,
    ImpossibleEncoding,
    // Divisor errors
    DivByZero,
    ModByZero,
    // CHECKLOCKTIMEVERIFY and CHECKSEQUENCEVERIFY
    NegativeLocktime,
    UnsatisfiedLocktime,
    // BIP62
    SigHashtype,
    SigDer,
    MinimalData,
    SigPushOnly,
    SigHighS,
    SigNullDummy,
    PubkeyType,
    CleanStack,
    SigNullFail,
    // Softfork safeness
    DiscourageUpgradableNops,
    // Constant scriptCode
    OpCodeSeparator,
    SigFindAndDelete,
}

/// `SCRIPT_ERR_ERROR_COUNT`: one past the last error code.
pub const SCRIPT_ERROR_COUNT: i32 = 41;

impl ScriptError {
    /// Every error, in code order (1..=40).
    pub const ALL: [ScriptError; 40] = {
        use ScriptError::*;
        [
            UnknownError,
            EvalFalse,
            OpReturn,
            ScriptSize,
            PushSize,
            OpCount,
            StackSize,
            SigCount,
            PubkeyCount,
            Verify,
            EqualVerify,
            CheckMultisigVerify,
            CheckSigVerify,
            CheckDataSigVerify,
            NumEqualVerify,
            BadOpcode,
            DisabledOpcode,
            InvalidStackOperation,
            InvalidAltstackOperation,
            UnbalancedConditional,
            InvalidSplitRange,
            InvalidOperandSize,
            InvalidNumberRange,
            ImpossibleEncoding,
            DivByZero,
            ModByZero,
            NegativeLocktime,
            UnsatisfiedLocktime,
            SigHashtype,
            SigDer,
            MinimalData,
            SigPushOnly,
            SigHighS,
            SigNullDummy,
            PubkeyType,
            CleanStack,
            SigNullFail,
            DiscourageUpgradableNops,
            OpCodeSeparator,
            SigFindAndDelete,
        ]
    };

    /// Core's numeric value (1..=40).
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// The error with Core's numeric value `code`; `None` for 0 (`SCRIPT_ERR_OK`) and out of range.
    pub fn from_code(code: i32) -> Option<Self> {
        usize::try_from(code)
            .ok()
            .and_then(|c| c.checked_sub(1))
            .and_then(|i| Self::ALL.get(i))
            .copied()
    }

    /// The name `script_tests.cpp`'s `script_errors[]` gives this error (e.g. `NULLFAIL` for
    /// `SCRIPT_ERR_SIG_NULLFAIL`). `IMPOSSIBLE_ENCODING` is not in that table; Core's DIP0020
    /// recorder uses this spelling for it.
    pub const fn name(self) -> &'static str {
        use ScriptError::*;
        match self {
            UnknownError => "UNKNOWN_ERROR",
            EvalFalse => "EVAL_FALSE",
            OpReturn => "OP_RETURN",
            ScriptSize => "SCRIPT_SIZE",
            PushSize => "PUSH_SIZE",
            OpCount => "OP_COUNT",
            StackSize => "STACK_SIZE",
            SigCount => "SIG_COUNT",
            PubkeyCount => "PUBKEY_COUNT",
            Verify => "VERIFY",
            EqualVerify => "EQUALVERIFY",
            CheckMultisigVerify => "CHECKMULTISIGVERIFY",
            CheckSigVerify => "CHECKSIGVERIFY",
            CheckDataSigVerify => "CHECKDATASIGVERIFY",
            NumEqualVerify => "NUMEQUALVERIFY",
            BadOpcode => "BAD_OPCODE",
            DisabledOpcode => "DISABLED_OPCODE",
            InvalidStackOperation => "INVALID_STACK_OPERATION",
            InvalidAltstackOperation => "INVALID_ALTSTACK_OPERATION",
            UnbalancedConditional => "UNBALANCED_CONDITIONAL",
            InvalidSplitRange => "SPLIT_RANGE",
            InvalidOperandSize => "OPERAND_SIZE",
            InvalidNumberRange => "INVALID_NUMBER_RANGE",
            ImpossibleEncoding => "IMPOSSIBLE_ENCODING",
            DivByZero => "DIV_BY_ZERO",
            ModByZero => "MOD_BY_ZERO",
            NegativeLocktime => "NEGATIVE_LOCKTIME",
            UnsatisfiedLocktime => "UNSATISFIED_LOCKTIME",
            SigHashtype => "SIG_HASHTYPE",
            SigDer => "SIG_DER",
            MinimalData => "MINIMALDATA",
            SigPushOnly => "SIG_PUSHONLY",
            SigHighS => "SIG_HIGH_S",
            SigNullDummy => "SIG_NULLDUMMY",
            PubkeyType => "PUBKEYTYPE",
            CleanStack => "CLEANSTACK",
            SigNullFail => "NULLFAIL",
            DiscourageUpgradableNops => "DISCOURAGE_UPGRADABLE_NOPS",
            OpCodeSeparator => "OP_CODESEPARATOR",
            SigFindAndDelete => "SIG_FINDANDDELETE",
        }
    }

    // Core: script/script_error.cpp ScriptErrorString
    /// Core's `ScriptErrorString`, verbatim.
    pub const fn message(self) -> &'static str {
        use ScriptError::*;
        match self {
            EvalFalse => {
                "Script evaluated without error but finished with a false/empty top stack element"
            }
            Verify => "Script failed an OP_VERIFY operation",
            EqualVerify => "Script failed an OP_EQUALVERIFY operation",
            CheckMultisigVerify => "Script failed an OP_CHECKMULTISIGVERIFY operation",
            CheckSigVerify => "Script failed an OP_CHECKSIGVERIFY operation",
            CheckDataSigVerify => "Script failed an OP_CHECKDATASIGVERIFY operation",
            NumEqualVerify => "Script failed an OP_NUMEQUALVERIFY operation",
            ScriptSize => "Script is too big",
            PushSize => "Push value size limit exceeded",
            OpCount => "Operation limit exceeded",
            StackSize => "Stack size limit exceeded",
            SigCount => "Signature count negative or greater than pubkey count",
            PubkeyCount => "Pubkey count negative or limit exceeded",
            BadOpcode => "Opcode missing or not understood",
            DisabledOpcode => "Attempted to use a disabled opcode",
            InvalidStackOperation => "Operation not valid with the current stack size",
            InvalidAltstackOperation => "Operation not valid with the current altstack size",
            OpReturn => "OP_RETURN was encountered",
            UnbalancedConditional => "Invalid OP_IF construction",
            InvalidSplitRange => "Invalid OP_SPLIT range",
            InvalidOperandSize => "Invalid operand size",
            InvalidNumberRange => {
                "Given operand is not a number within the valid range [-2^31...2^31]"
            }
            ImpossibleEncoding => "The requested encoding is impossible to satisfy",
            DivByZero => "Division by zero error",
            ModByZero => "Modulo by zero error",
            NegativeLocktime => "Negative locktime",
            UnsatisfiedLocktime => "Locktime requirement not satisfied",
            SigHashtype => "Signature hash type missing or not understood",
            SigDer => "Non-canonical DER signature",
            MinimalData => "Data push larger than necessary",
            SigPushOnly => "Only push operators allowed in signatures",
            SigHighS => "Non-canonical signature: S value is unnecessarily high",
            SigNullDummy => "Dummy CHECKMULTISIG argument must be zero",
            SigNullFail => "Signature must be zero for failed CHECK(MULTI)SIG operation",
            DiscourageUpgradableNops => "NOPx reserved for soft-fork upgrades",
            PubkeyType => "Public key is neither compressed or uncompressed",
            CleanStack => "Stack size must be exactly one after execution",
            OpCodeSeparator => "Using OP_CODESEPARATOR",
            SigFindAndDelete => "Signature is found in scriptCode",
            UnknownError => "unknown error",
        }
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ScriptError {}

/// The integer Core reports for a script result: 0 for `Ok`, else [`ScriptError::code`].
pub fn script_error_code(r: &Result<(), ScriptError>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => e.code(),
    }
}

/// `ScriptErrorString` for any integer, as Core formats it: 0 is "No error", and every value
/// without a case (1, 41 and out-of-range values) is "unknown error".
pub fn script_error_string(code: i32) -> &'static str {
    match code {
        0 => "No error",
        c => ScriptError::from_code(c).map_or("unknown error", ScriptError::message),
    }
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::script_error as o4;

    use super::*;

    #[test]
    fn codes_run_in_core_order() {
        for (i, e) in ScriptError::ALL.iter().enumerate() {
            assert_eq!(e.code(), i as i32 + 1);
            assert_eq!(ScriptError::from_code(e.code()), Some(*e));
        }
        assert_eq!(ScriptError::SigFindAndDelete.code() + 1, SCRIPT_ERROR_COUNT);
        assert_eq!(ScriptError::from_code(0), None);
        assert_eq!(ScriptError::from_code(SCRIPT_ERROR_COUNT), None);
        assert_eq!(ScriptError::from_code(-1), None);
    }

    #[test]
    fn names_are_the_harness_names() {
        // O4 transcribed Core's `script_errors[]`; every name but IMPOSSIBLE_ENCODING is in it.
        for e in ScriptError::ALL {
            assert!(o4::is_core_script_error(e.name()), "{}", e.name());
            let in_table = o4::is_known_script_error(e.name());
            assert_eq!(in_table, e != ScriptError::ImpossibleEncoding, "{}", e.name());
        }
        assert_eq!(o4::SCRIPT_ERROR_NAMES.len(), ScriptError::ALL.len());
    }

    #[test]
    fn display_is_the_core_message() {
        for e in ScriptError::ALL {
            assert_eq!(e.to_string(), e.message());
        }
    }

    #[test]
    fn unnamed_codes_are_unknown_error() {
        assert_eq!(script_error_string(0), "No error");
        assert_eq!(script_error_string(1), "unknown error");
        assert_eq!(script_error_string(SCRIPT_ERROR_COUNT), "unknown error");
        assert_eq!(script_error_string(-7), "unknown error");
    }
}
