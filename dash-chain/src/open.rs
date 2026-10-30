//! SPV `open`'s refusals (SB5 §3.5). `HeaderChain::open` itself is in `chain/sb5/open.rs`.

use dash_consensus::BlockId;

use crate::error::{ChainError, ConfigError, IndexInconsistency, StoreError};

/// Why `open` refused the store (§3.5). `Storage`, `Corrupt` and `Internal` trip the latch
/// first.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The caller runs `new_anchored` or `new_genesis`.
    #[error("store has no chain")]
    Empty,
    #[error("schema {0}, expected {1}")]
    Schema(u32, u32),
    /// A failed read; a read the store refused as a contradiction is `Corrupt` instead.
    #[error(transparent)]
    Storage(StoreError),
    /// SB15 clears the header tables and resyncs from the anchor (SB0 §4.4).
    #[error("corrupt header store: {0}")]
    Corrupt(Corruption),
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A chain call `open` makes failed without naming a contradiction: a bug, not the store's.
    #[error("internal: {0}")]
    Internal(ChainError),
}

/// The store's records contradict each other (§3.5).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Corruption {
    /// `meta.anchor` is genesis above height 0, or another block at a height no anchor pack has:
    /// 0, or below DGW's window (§8.4).
    #[error("the anchor at height {0} is neither genesis nor above DGW's window")]
    Anchor(u32),
    /// `meta.tip` has no record, or none at its height in the window.
    #[error("the tip {0:?} has no record")]
    MissingRecord(BlockId),
    /// A loaded record whose parent is neither loaded at the height below nor a cold record
    /// below the window base, and which is not the lowest held entry; or a FAILED leaf's path
    /// that leaves the height below.
    #[error("record {0:?} has no parent at the height below")]
    MissingParent(BlockId),
    /// The `active` rows are not the tip's ancestry, from the lowest accepted height up.
    #[error("the active rows disagree with the tip's ancestry at height {height}")]
    ActiveMismatch {
        height: u32,
    },
    /// A record's `skip` is not at `GetSkipHeight` of its height.
    #[error("record {0:?} has a skip at the wrong height")]
    SkipHeight(BlockId),
    /// A stored id at or above `meta.next_id`.
    #[error("stored id {0:?} is not below next_id")]
    NextIdNotAbove(BlockId),
    /// The tip is a context or failed record, or no leaf's path is as heavy as it.
    #[error("the tip {0:?} is on no leaf's valid path")]
    TipNotLeafPath(BlockId),
    /// `meta.cl.floor` names an id at or above `next_id`.
    #[error("the ChainLock floor {0:?} is not a held id")]
    ClFloor(BlockId),
    /// A read the store refused as a contradiction ([`StoreError::Corrupt`]): a record under
    /// another id or hash, an issued id with no record, or a link missing or off its height.
    #[error("{0}")]
    Store(String),
    /// Reserved for step 10, which needs a windowed `check_index` (SB4).
    #[error("index check: {0:?}")]
    CheckIndex(IndexInconsistency),
}

impl From<StoreError> for OpenError {
    /// A contradiction the store reports is `Corrupt`, so SB15 resyncs; any other failure is
    /// `Storage`, and SB15 stops.
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Corrupt(what) => OpenError::Corrupt(Corruption::Store(what)),
            e => OpenError::Storage(e),
        }
    }
}
