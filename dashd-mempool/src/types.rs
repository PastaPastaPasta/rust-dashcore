//! Types shared by mempool acceptance and its consumers.

use dash_consensus::{Amount, BlockId, TxValidationResult};
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
