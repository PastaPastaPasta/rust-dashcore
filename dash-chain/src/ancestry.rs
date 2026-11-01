//! Partial ancestry for anchored and windowed clients (SB0 §4.1.1; SB4 §8.4).

use dash_consensus::{BlockId, HeaderCtx};

use crate::error::AncestryError;

/// Ancestry that can end below a floor and can need a cold read. The node's complete,
/// infallible ancestry is `dash_consensus::ChainAncestry`, unchanged (SB0 §4.1.1).
pub trait AnchoredAncestry: Sync {
    /// The lowest height held: anchor − 24 (the pack's lowest), or 0 for genesis.
    fn floor(&self) -> u32;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError>;

    /// `GetAncestor`: `Ok(None)` above `id`'s height, `BelowFloor` below `floor()`.
    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError>;

    /// `pprev`: `Ok(None)` for genesis, `BelowFloor` for the lowest pack entry.
    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError>;
}
