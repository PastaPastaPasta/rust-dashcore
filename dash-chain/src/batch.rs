//! Header batches and their results (P2a R7; SB4 §4, D17).

use std::sync::Arc;

use dash_consensus::{BlockId, BlockValidationError, Header};
use dash_primitives::BlockHash;

use crate::snapshot::ChainReadSnapshot;

/// The peer a batch came from: Core's `NodeId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub i64);

/// Core's `BlockValidationState` for a rejected header (result, reason, debug message).
pub type HeaderReject = BlockValidationError;

/// The `ProcessHeaders` payload (P2a R7; SB4 §4): each header decoded with its X11 hash, once
/// (CS0 deviation 7), and `hashes[i]` = `headers[i].hash()`, which `accept` checks only under
/// `debug_assertions`, as Core's `ASSERT_IF_DEBUG` (`validation.cpp:4602`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderBatch {
    pub headers: Vec<Header>,
    pub hashes: Vec<BlockHash>,
}

/// The reply to a batch.
#[derive(Clone, Debug)]
pub struct HeadersResult {
    /// Core's `pindexLast`: the last header accepted or already known (`validation.cpp:4742`).
    pub last_accepted: Option<BlockId>,
    /// Why the batch stopped early, if it did.
    pub stop: Option<HeaderStop>,
    /// A late header became the ChainLock floor (`chainlock/chainlock.cpp:152-172`): enforce
    /// next. Set at the header that moved it, so also when the batch stops later; never on a
    /// height mismatch (`:161-166`) (CH1-A12).
    pub floor_moved: Option<BlockId>,
    /// The ids Core's `AcceptedBlockHeader` fires for (`validation.cpp:4724`), in order:
    /// CH1-node's listeners run once per entry (CH1-A12). Not a header inserted as
    /// ChainLock-conflicting, which Core rejects without the signal (`:4694-4698`); a resent
    /// genesis header is here, as Core's short-cut fires it (`:4606`).
    pub inserted: Vec<BlockId>,
    /// Node: published before the reply (R3), also when the batch stopped; the host's
    /// `NotifyHeaderTip` runs only when `stop` is `None`, as Core returns first (`:4739-4741`).
    /// SPV: the snapshot published before the step; the step publishes at stage 4 of §9.1's
    /// recipe.
    pub snap: Arc<ChainReadSnapshot>,
}

/// The reply to `accept_header` (CH1-A12): the header's result and, as for a batch, the
/// floor a late header moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderAccepted {
    /// `Accepted(id)` is the id `AcceptedBlockHeader` fires for.
    pub one: AcceptOne,
    /// As [`HeadersResult::floor_moved`].
    pub floor_moved: Option<BlockId>,
}

/// The result of one header (Core's `AcceptBlockHeader` alone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcceptOne {
    /// Core's `AcceptedBlockHeader` fires for it: a new entry, or genesis resent.
    Accepted(BlockId),
    Known(BlockId),
    Invalid(HeaderReject),
    /// SPV: a fork below the anchor, the genesis header, or a context parent (D13).
    BelowAnchor,
    /// SPV: a known context entry, skipped (D13).
    ContextSkipped,
}

/// Why a batch stopped early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderStop {
    /// The header at `index` was rejected.
    Invalid {
        index: usize,
        reject: HeaderReject,
    },
    /// SPV: the header at `index` is below the anchor; ignored, no blame (SB0 §4.1.1).
    BelowAnchor {
        index: usize,
    },
}
