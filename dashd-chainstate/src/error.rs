//! The coins cache's errors (CH2 §2.10).

/// An error from a coins cache operation. None is a block verdict: Core's two `logic_error`s
/// escape `ConnectBlock` and end dashd, so the caller treats every variant as fatal (DEC-58).
#[derive(thiserror::Error, Debug)]
pub enum CoinsError {
    // Core: coins.cpp:82 CCoinsViewCache::AddCoin
    #[error("Attempted to overwrite an unspent coin (when possible_overwrite is false)")]
    OverwriteUnspent,
    // Core: coins.cpp:222 CCoinsViewCache::BatchWrite
    #[error("FRESH flag misapplied to coin that exists in parent cache")]
    FreshMisapplied,
    /// A null output (value −1): Core's `AddCoin` asserts against it.
    #[error("coin value out of range")]
    ValueRange,
    /// A coins read failed; the node-wide latch has tripped (CH2 §2.4.4).
    #[error(transparent)]
    Read(#[from] crate::store::CoinsReadError),
    #[error("database corrupted: {0}")]
    Corruption(String),
}
