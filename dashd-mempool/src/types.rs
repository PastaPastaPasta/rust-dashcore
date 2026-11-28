//! Types shared by mempool acceptance and its consumers.

use std::collections::BTreeMap;

use dash_consensus::{Amount, BlockId, TxRef, TxValidationResult};
use dash_num::Hash256;
use dash_primitives::TxHash;

/// A removal cause from Dash Core v24.0.0 `src/txmempool.h`, `MemPoolRemovalReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemPoolRemovalReason {
    Expiry,
    SizeLimit,
    Reorg,
    Block,
    Conflict,
    Manual,
}

/// Cached sequence-lock constraints from Dash Core v24.0.0 `src/kernel/mempool_entry.h`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockPoints {
    pub height: i32,
    pub time: i64,
    /// The block-index identity replacing Core's `CBlockIndex* maxInputBlock`.
    pub max_input_block: Option<BlockId>,
}

/// An acceptance outcome from Dash Core v24.0.0 `src/validation.h`, `MempoolAcceptResult::ResultType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MempoolAcceptResultType {
    Valid,
    Invalid,
    MempoolEntry,
    /// Core's `EvoDbMissingStateError` thrown out of `PreChecks` or `TryAssetUnlockRefresh`: no
    /// verdict, the transaction is not admitted, and `reason` holds the exception's text.
    // Core: validation.cpp:1022, :1114, :1411; evo/mnhftx.cpp:286-290
    Unavailable,
}

/// An acceptance result from Dash Core v24.0.0 `src/validation.h`, `MempoolAcceptResult`.
/// Size and base fee are present for valid/existing entries; effective fee data only for valid ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MempoolAcceptResult {
    pub result_type: MempoolAcceptResultType,
    pub validation_result: TxValidationResult,
    pub reason: String,
    pub debug_message: String,
    pub vsize: Option<i64>,
    pub base_fee: Option<Amount>,
    /// The effective fee rate in duffs per kB, including fee deltas.
    pub effective_feerate: Option<i64>,
    pub txids_fee_calculations: Option<Vec<TxHash>>,
}

/// A conflicting IS lock from Dash Core v24.0.0 `src/evo/chainhelper.cpp`, `ConflictingISLockIfAny`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsConflict {
    pub lock_hash: Hash256,
    pub txid: TxHash,
}

/// Core's `MAX_PACKAGE_COUNT` (`policy/packages.h:16` at v24.0.2).
pub const MAX_PACKAGE_COUNT: usize = 25;

/// Core's `MAX_PACKAGE_SIZE`, in kvB (`policy/packages.h:18` at v24.0.2).
pub const MAX_PACKAGE_SIZE_KVB: u64 = 101;

/// An ordered list of transactions that do not conflict (`policy/packages.h:43` at v24.0.2).
pub type Package = Vec<TxRef>;

/// Core's `PackageValidationResult` (`policy/packages.h:34-39` at v24.0.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackageValidationResult {
    #[default]
    Unset,
    /// The package itself is invalid.
    Policy,
    /// At least one transaction is invalid.
    Tx,
    /// A mempool logic error.
    MempoolError,
}

/// Core's `PackageValidationState`: each `Invalid` call overwrites the last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageValidationState {
    pub result: PackageValidationResult,
    pub reason: String,
    pub debug_message: String,
}

impl PackageValidationState {
    // Core: consensus/validation.h:82-91 at v24.0.2
    pub(crate) fn invalid(
        &mut self,
        result: PackageValidationResult,
        reason: impl Into<String>,
        debug_message: impl Into<String>,
    ) {
        self.result = result;
        self.reason = reason.into();
        self.debug_message = debug_message.into();
    }
}

/// Core's `PackageMempoolAcceptResult` (`validation.h:224-246` at v24.0.2): a txid without a
/// result is one whose validation did not finish; a package-wide refusal leaves `tx_results`
/// empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageMempoolAcceptResult {
    pub state: PackageValidationState,
    pub tx_results: BTreeMap<TxHash, MempoolAcceptResult>,
    /// Core's `EvoDbMissingStateError` thrown out of `ProcessNewPackage`, its text: Core has no
    /// result then, so `state` and `tx_results` stay empty.
    pub unavailable: Option<String>,
}
