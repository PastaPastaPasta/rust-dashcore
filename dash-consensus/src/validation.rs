//! Validation results: Core's `TxValidationState` and `BlockValidationState` for the invalid case
//! (CS0 §8.1, FR-CON-31).
//!
//! A rejection carries Core's result class (with Core's integer values), its reject reason
//! verbatim and its debug message. System failures (`ValidationState::Error()`) are not
//! represented here; runtime crates wrap this type in their own error for those.

use std::borrow::Cow;
use std::fmt;

use dash_primitives::TxHash;

// Core: consensus/validation.h:24 TxValidationResult (identical in v23.1.8)
/// Core's `TxValidationResult`. The discriminants are Core's integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum TxValidationResult {
    /// `TX_RESULT_UNSET`: not rejected.
    Unset = 0,
    /// `TX_CONSENSUS`: invalid by consensus rules.
    Consensus,
    /// `TX_RECENT_CONSENSUS_CHANGE`.
    RecentConsensusChange,
    /// `TX_INPUTS_NOT_STANDARD`: inputs failed policy rules.
    InputsNotStandard,
    /// `TX_NOT_STANDARD`: otherwise failed local policy rules.
    NotStandard,
    /// `TX_MISSING_INPUTS`.
    MissingInputs,
    /// `TX_PREMATURE_SPEND`: coinbase spent too early, or locktime/sequence locks.
    PrematureSpend,
    /// `TX_BAD_SPECIAL`: a special transaction broke a rule not worth a ban.
    BadSpecial,
    /// `TX_CONFLICT`: already in the mempool or the chain.
    Conflict,
    /// `TX_CONFLICT_LOCK`: conflicts with an InstantSend lock.
    ConflictLock,
    /// `TX_MEMPOOL_POLICY`: mempool fee/size/descendant limits.
    MempoolPolicy,
    /// `TX_NO_MEMPOOL`.
    NoMempool,
}

// Core: consensus/validation.h:68 BlockValidationResult (identical in v23.1.8)
/// Core's `BlockValidationResult`. The discriminants are Core's integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum BlockValidationResult {
    /// `BLOCK_RESULT_UNSET`: not rejected.
    Unset = 0,
    /// `BLOCK_CONSENSUS`: invalid by consensus rules.
    Consensus,
    /// `BLOCK_RECENT_CONSENSUS_CHANGE`.
    RecentConsensusChange,
    /// `BLOCK_CACHED_INVALID`.
    CachedInvalid,
    /// `BLOCK_INVALID_HEADER`: invalid proof of work or time too old.
    InvalidHeader,
    /// `BLOCK_MUTATED`: the data does not match what the proof of work commits to.
    Mutated,
    /// `BLOCK_MISSING_PREV`.
    MissingPrev,
    /// `BLOCK_INVALID_PREV`.
    InvalidPrev,
    /// `BLOCK_TIME_FUTURE`.
    TimeFuture,
    /// `BLOCK_CHECKPOINT`.
    Checkpoint,
    /// `BLOCK_CHAINLOCK`: conflicts with a ChainLock.
    ChainLock,
}

/// A result class: [`TxValidationResult`] or [`BlockValidationResult`].
pub trait ResultClass: Copy + Eq + fmt::Debug {
    /// Core's integer value of the class.
    fn as_i32(self) -> i32;
}

impl ResultClass for TxValidationResult {
    fn as_i32(self) -> i32 {
        self as i32
    }
}

impl ResultClass for BlockValidationResult {
    fn as_i32(self) -> i32 {
        self as i32
    }
}

/// A rejection: Core's `ValidationState` after `Invalid(result, reason, debug)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError<C: ResultClass> {
    result: C,
    reason: Cow<'static, str>,
    debug: String,
}

/// Core's `TxValidationState`, invalid.
pub type TxValidationError = ValidationError<TxValidationResult>;
/// Core's `BlockValidationState`, invalid.
pub type BlockValidationError = ValidationError<BlockValidationResult>;

impl<C: ResultClass> ValidationError<C> {
    /// `state.Invalid(result, reason)`. Most reasons are literals; Core formats a few
    /// (`bad-version(0x%08x)`, the script-verify-flag reasons), hence the `Cow`.
    pub fn new(result: C, reason: impl Into<Cow<'static, str>>) -> Self {
        Self {
            result,
            reason: reason.into(),
            debug: String::new(),
        }
    }

    /// Sets (replaces) the debug message: `state.Invalid(result, reason, debug)`.
    pub fn with_debug(mut self, debug: impl Into<String>) -> Self {
        self.debug = debug.into();
        self
    }

    /// `GetResult()`.
    pub fn result(&self) -> C {
        self.result
    }

    /// `GetRejectReason()`.
    pub fn reject_reason(&self) -> &str {
        &self.reason
    }

    /// `GetDebugMessage()`; empty when there is none, as in Core.
    pub fn debug_message(&self) -> &str {
        &self.debug
    }
}

// Core: consensus/validation.h:105 ValidationState::ToString (the invalid case)
impl<C: ResultClass> fmt::Display for ValidationError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.debug.is_empty() {
            f.write_str(&self.reason)
        } else {
            write!(f, "{}, {}", self.reason, self.debug)
        }
    }
}

impl<C: ResultClass> std::error::Error for ValidationError<C> {}

impl TxValidationError {
    /// The `ConnectBlock` conversion: reason and debug message unchanged.
    ///
    // Core: validation.cpp:2701 ConnectBlock (CheckTxInputs) and :2754 (CheckInputScripts):
    // state.Invalid(BLOCK_CONSENSUS, tx_state.GetRejectReason(), tx_state.GetDebugMessage())
    pub fn into_block(self, result: BlockValidationResult) -> BlockValidationError {
        ValidationError {
            result,
            reason: self.reason,
            debug: self.debug,
        }
    }

    /// The `CheckBlock`/`ContextualCheckBlock` conversion: the reason unchanged, the debug
    /// message wrapped as `Transaction check failed (tx hash <txid>) <debug>`, with the trailing
    /// space even when the transaction's debug message is empty. `txid` is the transaction's
    /// `GetHash()` under the block's rules, printed as `uint256::ToString()`.
    ///
    // Core: validation.cpp:4411 CheckBlock and :4567 ContextualCheckBlock:
    // strprintf("Transaction check failed (tx hash %s) %s", tx->GetHash().ToString(), tx_state.GetDebugMessage())
    pub fn into_block_wrapped(
        self,
        result: BlockValidationResult,
        txid: &TxHash,
    ) -> BlockValidationError {
        ValidationError {
            result,
            debug: format!("Transaction check failed (tx hash {}) {}", txid, self.debug),
            reason: self.reason,
        }
    }
}
