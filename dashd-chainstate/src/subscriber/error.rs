//! A subscriber's errors (CH2 §2.9). Every one trips the latch at the driver (§4.2 steps 3b and
//! 6: "a subscriber error trips the latch").

use crate::store::Fatal;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubscriberError {
    /// A store call failed, or another failure had tripped the latch.
    #[error(transparent)]
    Fatal(#[from] Fatal),
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
