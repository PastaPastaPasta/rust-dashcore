//! A subscriber's errors (CH2 §2.9). Every one trips the latch at the driver (§4.2 steps 3b and
//! 6: "a subscriber error trips the latch").

use crate::store::Fatal;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubscriberError {
    /// This subscriber's own failure latched the node: a store call of its, or a failed file
    /// append or sync. The host reports it (Core's `FatalError`, `AbortNode`,
    /// `index/base.cpp:33-36` at v24.0.0).
    #[error(transparent)]
    Fatal(Fatal),
    /// Refused: a failure had latched before this call, at another site or at an earlier step of
    /// this subscriber. The host reports nothing; the site that latched it did.
    #[error(transparent)]
    Refused(#[from] Fatal),
    /// The data object's own failure (Core's `CustomAppend` or `CustomRewind` returning false).
    #[error("{0}")]
    Data(String),
    /// A block or undo the feed promised readable was not (F8).
    #[error("{0}")]
    Blocks(String),
    /// An event whose parent (a connect) or which itself (a disconnect) is a strict ancestor of
    /// the subscriber's tip: the cut excludes it, so the producer broke F1/F10 (§2.9.1).
    #[error("feed order: {0}")]
    FeedOrder(String),
}
