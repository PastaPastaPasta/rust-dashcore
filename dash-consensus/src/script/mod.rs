//! Dash Core's script system (CS1; A1 §2.4 `script`).
//!
//! A line-by-line port of Core's script layer at v24.0.0-rc.3 (CS1 §2). The mandatory flag mask
//! is [`flags::ScriptFlags::MANDATORY_V24_0`].
//!
//! | Module | Core |
//! |---|---|
//! | [`flags`] | `script/interpreter.h` `SCRIPT_VERIFY_*`, the policy masks, `GetBlockScriptFlags` |
//! | [`error`] | `script/script_error.{h,cpp}` |
//! | [`opcodes`] | `script/script.h` `opcodetype`, `script/script.cpp` `GetOpName` |
//! | [`parse`] | `script/script.{h,cpp}` `GetScriptOp`, `IsPushOnly`, `IsPayToScriptHash`, `CheckMinimalPush`, `CScript <<`; `script/interpreter.cpp` `FindAndDelete` |
//! | [`num`] | `script/script.{h,cpp}` `CScriptNum` |
//! | [`encoding`] | `script/interpreter.cpp` `CheckSignatureEncoding`, `CheckPubKeyEncoding` |
//! | [`ecdsa`] | `pubkey.{h,cpp}` `CPubKey::Verify`, `CheckLowS`, `ecdsa_signature_parse_der_lax` |
//! | [`checker`] | `script/interpreter.{h,cpp}` `BaseSignatureChecker`, `GenericTransactionSignatureChecker`, `VerifySignature` (the sigcache hook) |
//! | [`interpreter`] | `script/interpreter.cpp` `EvalScript`, `VerifyScript`, `CastToBool` |
//! | [`sighash`] | `script/interpreter.{h,cpp}` `SignatureHash`, `CTransactionSignatureSerializer`, `PrecomputedTransactionData` |
//! | [`sigops`] | `script/script.cpp` `CScript::GetSigOpCount` (both overloads); `consensus/tx_verify.cpp` `GetLegacySigOpCount`, `GetP2SHSigOpCount`, `GetTransactionSigOpCount` |
//! | [`input`] | `validation.cpp` `CScriptCheck`, `CheckInputScripts` (without caches) |

pub mod checker;
pub mod ecdsa;
pub mod encoding;
pub mod error;
pub mod flags;
pub mod input;
pub mod interpreter;
pub mod num;
pub mod opcodes;
pub mod parse;
pub mod sighash;
pub mod sigops;

pub use checker::{
    DirectVerifier, NoSignatureChecker, SigVerifier, SignatureChecker, TransactionSignatureChecker,
};
pub use ecdsa::{PubKeyRef, check_low_s, parse_der_lax, verify_lax};
pub use encoding::{
    SIGHASH_ALL, SIGHASH_ANYONECANPAY, SIGHASH_NONE, SIGHASH_SINGLE, check_pubkey_encoding,
    check_signature_encoding, is_valid_signature_encoding,
};
pub use error::{ScriptError, script_error_code, script_error_string};
pub use flags::{BuriedScriptRules, ScriptFlags, block_script_flags};
pub use input::{
    CheckInputsError, InputCheck, InputScriptFailure, InvalidScriptFlags, ScriptFailureKind,
    ScriptPolicy, check_input_scripts, classify_failure,
};
pub use interpreter::{Stack, cast_to_bool, eval_script, verify_script};
pub use num::{DEFAULT_MAX_NUM_SIZE, ScriptNum, ScriptNumError};
pub use parse::{
    Op, check_minimal_push, find_and_delete, get_op, instructions, is_pay_to_script_hash,
    is_push_only, push_data,
};
pub use sighash::{TxSigData, serialize_script_code, signature_hash, signature_hash_with};
pub use sigops::{
    legacy_sigop_count, p2sh_sigop_count, script_sigop_count, script_sigop_count_with_sig,
    transaction_sigop_count,
};
