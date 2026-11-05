//! Input checks and failure classification: Core's `CScriptCheck` and the pure part of
//! `CheckInputScripts` (`validation.cpp:2106-2230`; CS1 §4.11, §6.2).
//!
//! Script flags are checked where they are put together: [`InputCheck::new`] refuses
//! `CLEANSTACK` without `P2SH`, which Core's `VerifyScript` asserts, so a bad mask fails where it
//! is built rather than inside a check-queue worker; [`ScriptPolicy::V24_0`]'s masks are checked
//! at compile time.

use std::fmt;

use super::checker::{SigVerifier, TransactionSignatureChecker};
use super::error::ScriptError;
use super::flags::ScriptFlags;
use super::interpreter::verify_script;
use super::sighash::TxSigData;
use crate::primitives::canonical::is_coinbase;
use crate::validation::{TxValidationError, TxValidationResult};

/// Script flags with `CLEANSTACK` but not `P2SH` (`IsValidFlagCombination`), which Core's
/// `VerifyScript` asserts against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidScriptFlags(pub ScriptFlags);

impl fmt::Display for InvalidScriptFlags {
    // census: rust-only (a caller's bad mask; Core asserts IsValidFlagCombination)
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "script flags {:#x} set CLEANSTACK without P2SH", self.0.bits())
    }
}

impl std::error::Error for InvalidScriptFlags {}

fn valid(flags: ScriptFlags) -> Result<ScriptFlags, InvalidScriptFlags> {
    if flags.is_valid_combination() {
        Ok(flags)
    } else {
        // census: rust-only (a caller's bad mask; Core asserts IsValidFlagCombination)
        Err(InvalidScriptFlags(flags))
    }
}

// Core: validation.cpp:2106 CScriptCheck::operator()
/// Core's `CScriptCheck`: `VerifyScript(vin[n_in].scriptSig, spent scriptPubKey, flags,
/// TransactionSignatureChecker(txdata, n_in, verifier))`.
///
/// Pure and `Send + Sync`: the unit CS6's check queue runs. It borrows the spent script, so a
/// caller that queues checks keeps the coins alive until the queue drains (CS1 §4.11).
#[derive(Clone, Copy)]
pub struct InputCheck<'a> {
    txdata: &'a TxSigData<'a>,
    n_in: usize,
    spent_script_pubkey: &'a [u8],
    flags: ScriptFlags,
}

impl<'a> InputCheck<'a> {
    /// The check of input `n_in` spending `spent_script_pubkey` under `flags`.
    ///
    /// # Errors
    ///
    /// [`InvalidScriptFlags`] when `flags` has `CLEANSTACK` without `P2SH`.
    ///
    /// # Panics
    ///
    /// If `n_in` is not an input of the transaction.
    pub fn new(
        txdata: &'a TxSigData<'a>,
        n_in: usize,
        spent_script_pubkey: &'a [u8],
        flags: ScriptFlags,
    ) -> Result<Self, InvalidScriptFlags> {
        assert!(n_in < txdata.tx().inputs.len(), "the input index is out of range");
        Ok(Self {
            txdata,
            n_in,
            spent_script_pubkey,
            flags: valid(flags)?,
        })
    }

    /// Verifies the input with a checker of its own, signatures going through `verifier`.
    pub fn run<V: SigVerifier + ?Sized>(&self, verifier: &V) -> Result<(), ScriptError> {
        let checker = TransactionSignatureChecker::new(self.txdata, self.n_in, verifier);
        let script_sig = self.txdata.tx().inputs[self.n_in].script_sig.as_bytes();
        verify_script(script_sig, self.spent_script_pubkey, self.flags, &checker)
    }
}

/// The two masks `CheckInputScripts` reads: `STANDARD_SCRIPT_VERIFY_FLAGS` and
/// `MANDATORY_SCRIPT_VERIFY_FLAGS`. Core's are [`ScriptPolicy::V24_0`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptPolicy {
    standard: ScriptFlags,
    mandatory: ScriptFlags,
}

impl ScriptPolicy {
    /// Core v24.0's policy: [`ScriptFlags::STANDARD`] and [`ScriptFlags::MANDATORY_V24_0`].
    pub const V24_0: Self = Self {
        standard: ScriptFlags::STANDARD,
        mandatory: ScriptFlags::MANDATORY_V24_0,
    };

    /// `STANDARD_NOT_MANDATORY_VERIFY_FLAGS`: 0x141CA under [`ScriptPolicy::V24_0`].
    pub const fn standard_not_mandatory(self) -> ScriptFlags {
        self.standard.difference(self.mandatory)
    }
}

// The classification re-run removes `standard & !mandatory` from a valid mask, so the result stays
// valid when the standard mask is and the re-run removes `P2SH` only together with `CLEANSTACK`.
const _: () = {
    let policy = ScriptPolicy::V24_0;
    let removed = policy.standard_not_mandatory();
    assert!(policy.standard.is_valid_combination());
    assert!(!removed.contains(ScriptFlags::P2SH) || removed.contains(ScriptFlags::CLEANSTACK));
};

/// How `CheckInputScripts` classifies a failing input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptFailureKind {
    /// `TX_CONSENSUS`: `mandatory-script-verify-flag-failed (…)`.
    Mandatory,
    /// `TX_NOT_STANDARD`: `non-mandatory-script-verify-flag (…)`.
    NonMandatory,
}

/// The first failing input of a transaction, classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputScriptFailure {
    pub kind: ScriptFailureKind,
    /// The first run's error, which Core reports whatever the re-run gives.
    pub error: ScriptError,
    pub n_in: usize,
}

impl InputScriptFailure {
    /// Core's reject reason.
    pub fn reject_reason(&self) -> String {
        match self.kind {
            ScriptFailureKind::Mandatory => {
                format!("mandatory-script-verify-flag-failed ({})", self.error.message())
            }
            ScriptFailureKind::NonMandatory => {
                format!("non-mandatory-script-verify-flag ({})", self.error.message())
            }
        }
    }

    /// Core's `state.Invalid(…)`: the result class and the reason, with an empty debug message.
    pub fn into_tx_error(self) -> TxValidationError {
        let result = match self.kind {
            ScriptFailureKind::Mandatory => TxValidationResult::Consensus,
            ScriptFailureKind::NonMandatory => TxValidationResult::NotStandard,
        };
        TxValidationError::new(result, self.reject_reason())
    }
}

// Core: validation.cpp:2197 CheckInputScripts (the failure branch of the serial loop)
/// Classifies an input whose check failed with `first`: when its flags include any of the
/// policy's non-mandatory flags, it is re-run without them, and a pass makes the failure
/// non-mandatory. The reported error is always `first`.
pub fn classify_failure<V: SigVerifier + ?Sized>(
    check: &InputCheck<'_>,
    first: ScriptError,
    policy: ScriptPolicy,
    verifier: &V,
) -> InputScriptFailure {
    let standard_not_mandatory = policy.standard_not_mandatory();
    let mut kind = ScriptFailureKind::Mandatory;
    if check.flags.intersects(standard_not_mandatory) {
        let check2 = InputCheck {
            flags: check.flags.difference(standard_not_mandatory),
            ..*check
        };
        // The policy's masks keep this a valid combination (checked below, at compile time).
        debug_assert!(check2.flags.is_valid_combination());
        if check2.run(verifier).is_ok() {
            kind = ScriptFailureKind::NonMandatory;
        }
    }
    InputScriptFailure {
        kind,
        error: first,
        n_in: check.n_in,
    }
}

// Core: validation.cpp:2153 CheckInputScripts (no caches, no pvChecks)
/// `CheckInputScripts` run serially without caches: `Ok` for a coinbase; otherwise every input
/// in order, stopping at the first failure, which is classified.
///
/// `txdata` is Core's `PrecomputedTransactionData& txdata` (the caller builds it once per
/// transaction), and `spent_script_pubkey(i)` the script of the output input `i` spends. CS6 wraps
/// this with the script-execution cache and a caching verifier; its parallel mode runs
/// [`InputCheck::run`] and reports Core's generic result instead.
///
/// # Errors
///
/// The first failing input, classified, or [`CheckInputsError::Flags`] when `flags` has
/// `CLEANSTACK` without `P2SH` (nothing runs then; a coinbase returns first, as in Core).
pub fn check_input_scripts<'s, V: SigVerifier + ?Sized>(
    txdata: &TxSigData<'_>,
    spent_script_pubkey: impl Fn(usize) -> &'s [u8],
    flags: ScriptFlags,
    policy: ScriptPolicy,
    verifier: &V,
) -> Result<(), CheckInputsError> {
    let tx = txdata.tx();
    if is_coinbase(tx) {
        return Ok(());
    }
    let flags = valid(flags)?;
    for n_in in 0..tx.inputs.len() {
        let check = InputCheck {
            txdata,
            n_in,
            spent_script_pubkey: spent_script_pubkey(n_in),
            flags,
        };
        if let Err(first) = check.run(verifier) {
            return Err(CheckInputsError::Script(classify_failure(
                &check, first, policy, verifier,
            )));
        }
    }
    Ok(())
}

/// Why [`check_input_scripts`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckInputsError {
    /// An input failed: Core's `state.Invalid(…)`.
    Script(InputScriptFailure),
    /// The flags are not a valid combination; Core would abort.
    Flags(InvalidScriptFlags),
}

impl From<InvalidScriptFlags> for CheckInputsError {
    // census: rust-only (a caller's bad mask; Core asserts IsValidFlagCombination)
    fn from(e: InvalidScriptFlags) -> Self {
        Self::Flags(e)
    }
}
