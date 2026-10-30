//! The error model (SB4 §14). Rule failures are never `Err`; `Err` is never blame.

use dash_consensus::{BlockId, ParamsError};
use dash_primitives::BlockHash;

use crate::feed::FeedConfigError;

/// A cold-tier read or write failure (SB4 D5: a `dash-chain` type until SB10's `dash-kv` lands,
/// then a re-export). The kind tells `open` whether to resync (SB5 §8, D68).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The store could not be read or written.
    #[error("header store: {0}")]
    Io(String),
    /// The store read back fine but contradicts itself: a record under another id, an issued
    /// id with no record, or a parent or skip link that is missing or off its height. A backend
    /// that cannot decode a row returns this too.
    #[error("header store corrupt: {0}")]
    Corrupt(String),
}

impl StoreError {
    /// What failed.
    pub fn what(&self) -> &str {
        match self {
            Self::Io(what) | Self::Corrupt(what) => what,
        }
    }

    /// Whether the store contradicts itself, rather than failing to read or write.
    pub const fn is_corrupt(&self) -> bool {
        matches!(self, Self::Corrupt(_))
    }
}

/// The fail-stop latch (CH2's `GlobalFailStop` + `AbortSink`; SB4 D5). A failed cold read trips
/// it; the client then stops (P6).
pub trait FailStop: Send + Sync {
    /// Trips the latch and reports the read failure (`AbortSink::read_failed`).
    fn read_failed(&self, what: &str);

    /// Whether the latch has tripped.
    fn is_tripped(&self) -> bool;
}

/// A refused configuration.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The node runs with `window = None` (SB0 §4.4).
    #[error("a window is only allowed in SPV mode")]
    WindowInNodeMode,
    /// SPV runs windowed.
    #[error("SPV mode needs a window")]
    NoWindowInSpvMode,
    /// The node starts from genesis (SB0 §4.1.1).
    #[error("the node cannot start from an anchor")]
    AnchoredNode,
    /// An anchored chain needs `Evicting` residency: `Permanent`'s walks, work and children
    /// read the arena only, which holds nothing below the anchor's floor (SB5 D70).
    #[error("an anchored chain needs Evicting residency")]
    AnchoredPermanent,
    /// An SPV-only or node-only entry point was called in the other mode.
    #[error("{0} is not available in this mode")]
    WrongMode(&'static str),
    /// Core's proof of work cannot run with the chain parameters (CS2 §3.1).
    #[error("chain parameters: {0}")]
    Pow(ParamsError),
    /// The feeds' configuration (SB5-A16).
    #[error(transparent)]
    Feeds(#[from] FeedConfigError),
    /// SPV's cold cache holds at least one record (SB5-A6).
    #[error("cold_cache must be at least 1")]
    ColdCache,
    /// An SPV header step over B, refused before any work (SB5-A11 (a)): the caller slices.
    #[error("a step of {headers} headers exceeds step_headers = {cap}")]
    StepHeaders {
        headers: usize,
        cap: u32,
    },
}

/// A refused anchor pack (SB4 §11.1).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnchorError {
    /// The anchor header does not hash to the pack's hash.
    #[error("the anchor header does not hash to the pack's hash")]
    Hash,
    /// `headers[index]` does not hash to `headers[index + 1].prev_hash`.
    #[error("pack header {index} is not linked to its successor")]
    Link {
        index: usize,
    },
    /// The anchor is below `dgw_height + 24`.
    #[error("the anchor at {height} is not in the DGW era")]
    PreDgw {
        height: u32,
    },
    /// The absolute work does not cover the pack's proofs (checked subtraction, Sol r1 m3).
    #[error("the pack's chain work is below the sum of its proofs")]
    Work,
}

/// A writer or reader failure (SB4 §14).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    /// A cold read failed, or the store contradicts itself ([`StoreError::is_corrupt`]); the
    /// fail-stop latch has tripped.
    #[error(transparent)]
    Storage(#[from] StoreError),
    /// The block is below the anchor (SPV).
    #[error("below the anchor")]
    BelowAnchor,
    /// SPV `set_active_tip` to a target that conflicts with the floor while ChainLocks are
    /// observed enabled (DEC-136).
    #[error("target {to:?} conflicts with the ChainLock floor {floor:?}")]
    ConflictsWithFloor {
        to: BlockId,
        floor: BlockId,
    },
    /// An id the index never issued.
    #[error("unknown block {0:?}")]
    UnknownBlock(BlockId),
    /// A broken invariant: a bug (debug builds panic first).
    #[error("internal: {0}")]
    Internal(&'static str),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Anchor(#[from] AnchorError),
}

/// A failure of `apply_chainlock`/`enforce_best_chainlock`: the chain's or the host's.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError<E> {
    #[error(transparent)]
    Chain(#[from] ChainError),
    /// The host's `DisconnectTip` failed; Core asserts (`validation.cpp:4067`).
    #[error("host: {0:?}")]
    Host(E),
}

/// A malformed `BlockIndexRecord` value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("record truncated")]
    Truncated,
    #[error("VARINT too large")]
    VarIntOverflow,
    #[error("{0} trailing bytes")]
    Trailing(usize),
}

/// A corrupt or inconsistent block index at `load` (SB4 §10.3). The node refuses to start, as
/// Core's `LoadBlockIndex` returning false does.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Read(#[from] StoreError),
    #[error("record {hash:?}: {err}")]
    Decode {
        hash: BlockHash,
        err: CodecError,
    },
    /// A record is keyed by the null hash, which Core's `InsertBlockIndex` maps to no entry.
    #[error("a record is keyed by the null hash")]
    NullHash,
    /// `CheckProofOfWork` failed against the stored hash (`txdb.cpp:328-331`).
    #[error("record {hash:?}: proof of work failed")]
    HighHash {
        hash: BlockHash,
    },
    /// A height has no entry (`blockstorage.cpp:297-300`).
    #[error("no entry at height {missing_height}")]
    NonContiguous {
        missing_height: u32,
    },
    /// A prev hash is mentioned but never defined (D21).
    #[error("record {hash:?}: parent {prev:?} is missing")]
    MissingParent {
        hash: BlockHash,
        prev: BlockHash,
    },
    /// A second root, or a root other than the network's genesis (D21, n3).
    #[error("genesis mismatch")]
    GenesisMismatch,
    /// No records: a fresh datadir; the caller writes genesis (D21, n3).
    #[error("empty block index")]
    Empty,
    /// [`HeaderChain::load_records`]' `interrupted` asked to stop before a record was read: Core's
    /// `ShutdownRequested()` in `LoadBlockIndexGuts` (`txdb.cpp:307-309`), which init reports as
    /// INTERRUPTED.
    ///
    /// [`HeaderChain::load_records`]: crate::HeaderChain::load_records
    #[error("the block index load was interrupted")]
    Interrupted,
    /// A record's height is not its parent's plus one, or a root's is not 0. Core loads such an
    /// index and only `-checkblockindex` asserts on it; the skip walks need consistent heights
    /// (D-C7-2).
    #[error("record {hash:?}: height inconsistent with its parent")]
    HeightMismatch {
        hash: BlockHash,
    },
    /// `load` refused the configuration (D-C7-1).
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// A partial-ancestry failure (SB0 §4.1.1).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AncestryError {
    /// The height is below the chain's floor: the history is not held. An ordinary answer.
    #[error("height {height} is below the floor")]
    BelowFloor {
        height: u32,
    },
    /// A cold read failed, or the store contradicts itself; the latch has tripped.
    #[error(transparent)]
    Storage(#[from] StoreError),
}

/// A `-checkblockindex` failure (the header subset of `CheckBlockIndex`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("block index inconsistency at {id:?}: {what}")]
pub struct IndexInconsistency {
    pub id: BlockId,
    pub what: &'static str,
}
