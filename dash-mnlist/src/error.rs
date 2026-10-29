//! The engine's errors: SPV's chain-view error (EV addendum §2.3, D8) and why a payload is rejected
//! (SB0 §5.2's steps).

use dash_chain::{AncestryError, StoreError};
use dash_evo::cbtx::CbTxProofError;
use dash_primitives::BlockHash;

/// SPV's chain and state view error, the `E` of every LQ1 call the engine makes (EV addendum
/// §2.3). A view never answers one of Core's absence values for an input it merely lacks (§2.1):
/// it answers one of these, or `Ok(None)` from the list source (LQ1's `Missing`, a request).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MnViewError {
    /// The height is below the anchored chain's floor: never fetched (no backfill), so the
    /// commitments that need it are `Unavailable` (SB0 §5.2, §4.3.1 step 4).
    #[error("height {height} is below the floor")]
    BelowFloor {
        /// The height read.
        height: u32,
    },
    /// The engine does not hold a rotation row a `qrinfo` serves: fetched.
    #[error("no snapshot row held for type {llmq_type} at cycle {cycle_base}")]
    RowNotHeld {
        /// The rotated type.
        llmq_type: u8,
        /// The cycle base block.
        cycle_base: BlockHash,
    },
    /// A store read failed; its latch has tripped. Stop.
    #[error(transparent)]
    Storage(StoreError),
}

impl From<AncestryError> for MnViewError {
    fn from(e: AncestryError) -> Self {
        match e {
            AncestryError::BelowFloor {
                height,
            } => MnViewError::BelowFloor {
                height,
            },
            AncestryError::Storage(s) => MnViewError::Storage(s),
        }
    }
}

/// Why `apply_diff` or `apply_qrinfo` refused a payload, by the trust-chain step that failed
/// (SB0 §5.2). A refused payload changes no state.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MnReject {
    /// Step 1: the target is not an accepted header.
    #[error("the diff's target {0} is not an accepted header")]
    UnknownTarget(BlockHash),
    /// The diff's base list is not held whole (nor null or genesis; D29), or lies above its
    /// target.
    #[error("the diff's base {0} is not a held list below its target")]
    UnknownBase(BlockHash),
    /// The coinbase is not a coinbase special transaction whose payload decodes, or its height is
    /// not the target's.
    #[error("the diff's coinbase is not the target's CbTx: {0}")]
    BadCoinbase(&'static str),
    /// Step 2: the partial-merkle proof does not prove the coinbase in the target's header.
    #[error("the coinbase proof fails: {0}")]
    CbTxProof(CbTxProofError),
    /// Step 3: the list after the diff does not hash to the CbTx's `merkleRootMNList`.
    #[error("the list root does not match the CbTx")]
    MnListRoot,
    /// Step 4: the quorum set after the diff does not hash to the CbTx's `merkleRootQuorums`.
    #[error("the quorum root does not match the CbTx")]
    QuorumsRoot,
    /// A `qrinfo` whose parts do not fit together: a diff that does not apply, a row without its
    /// cycle, a cycle that is not its diff's work block.
    #[error("the qrinfo is inconsistent: {0}")]
    QrInfo(&'static str),
    /// A chain read failed.
    #[error(transparent)]
    View(#[from] MnViewError),
}

impl From<AncestryError> for MnReject {
    fn from(e: AncestryError) -> Self {
        MnReject::View(e.into())
    }
}

/// Why [`MasternodeListEngine::fetch_lookup`](crate::MasternodeListEngine::fetch_lookup) refused a
/// lookup, or a chain read failed (D37, D44).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchLookupError {
    /// `max` lookups already wait on their fetch, each pinning up to two whole lists: this one
    /// is refused and none is dropped, as Core's RPC work queue refuses new work when full
    /// (`httpserver.cpp` v24.0.0-rc.3:91-101, 369). Waits resolve only in `apply_diff`,
    /// `apply_qrinfo`, `tip_changed` and `request_failed` (a wait that asked the failed key): the
    /// host asks again after one of them leaves `lookup_waits()` below `max` (D49).
    #[error("{max} lookups already wait on their fetch")]
    Busy {
        /// The engine's bound on waiting lookups.
        max: usize,
    },
    /// A chain read failed: only [`MnReject::View`], a store fault the host stops on (SB10
    /// fail-stop), never a peer's payload. Requests may have been asked before it.
    #[error(transparent)]
    Reject(#[from] MnReject),
}

impl From<AncestryError> for FetchLookupError {
    fn from(e: AncestryError) -> Self {
        FetchLookupError::Reject(e.into())
    }
}

/// Why [`MasternodeListEngine::load`](crate::MasternodeListEngine::load) failed (D38).
#[derive(Debug, thiserror::Error)]
pub enum MnLoadError {
    /// A store read failed.
    #[error(transparent)]
    Store(#[from] dash_kv::StoreError),
    /// A row does not decode, or does not match what it is stored under.
    #[error("a corrupt {table} row: {reason}")]
    Corrupt {
        /// The table.
        table: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// The block the rows reach, the tip their last retention ran on, is not on the chain (a
    /// lost suffix, or another chain's store): rows marked on a branch it no longer knows could
    /// answer what it would not. The host resets them with the locator (SB0 §9.5,
    /// [`MasternodeListEngine::reset`](crate::MasternodeListEngine::reset)).
    #[error("the rows reach block {hash} at {height}, which the chain does not know")]
    UnknownLocator {
        height: u32,
        hash: dash_primitives::BlockHash,
    },
    /// A chain read failed.
    #[error(transparent)]
    View(#[from] MnViewError),
}
