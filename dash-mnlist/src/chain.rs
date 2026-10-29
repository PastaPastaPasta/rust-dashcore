//! The header chain as the engine reads it (D2): SB4's [`AnchoredAncestry`] plus what a diff names
//! by hash.

use dash_chain::{
    AncestryError, AnchoredAncestry, BlockRef, ChainError, HeaderChain, HeaderStore, Residency,
};
use dash_consensus::BlockId;
use dash_primitives::{BlockHash, MerkleRoot};

/// The header chain as the engine reads it: ancestry that can end below the anchor's floor, a
/// lookup by hash, the active tip, and a header's merkle root (SB0 §5.2 steps 1–2).
pub trait MnChain: Sync {
    /// The chain's ancestry, down to its floor.
    fn ancestry(&self) -> &dyn AnchoredAncestry;

    /// Core's `LookupBlockIndex`: any accepted header, on the active chain or not.
    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError>;

    /// The active chain's tip.
    fn active_tip(&self) -> Result<BlockRef, AncestryError>;

    /// The header's `hashMerkleRoot`; `None` for an id the chain never issued.
    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError>;

    /// The lowest height that covers: 0 from genesis, else the anchor. The entries from the floor
    /// up to it are the anchor pack's context: accepted, read by step 5, but they cover nothing
    /// (SB0 §4.3.1, D25). By default the floor: a chain with no context entries.
    fn anchor(&self) -> u32 {
        self.ancestry().floor()
    }
}

/// The context entries an anchor pack holds below its anchor (`dash_chain`'s `CONTEXT_ENTRIES`,
/// its floor being anchor − 24, or 0 from genesis).
const CONTEXT_ENTRIES: u32 = 24;

impl<S: HeaderStore, R: Residency> MnChain for HeaderChain<S, R> {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self.anchored()
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        self.reader().try_lookup(hash).map_err(chain_error)
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        let tip = self.snapshot().tip;
        Ok(BlockRef {
            id: tip.tip,
            hash: tip.tip_hash,
            height: tip.tip_height,
        })
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        let entry = self.reader().try_entry(id).map_err(chain_error)?;
        Ok(entry.map(|e| e.header.merkle_root))
    }

    /// The floor is fixed at creation (`floor_below`): 0 from genesis, else anchor − 24. An
    /// anchor at exactly 24 reads as genesis, its context 0–23 far below any A_q.
    fn anchor(&self) -> u32 {
        match self.anchored().floor() {
            0 => 0,
            floor => floor + CONTEXT_ENTRIES,
        }
    }
}

/// A reader error as an ancestry error. `try_lookup` and `try_entry` fail only on a read, which
/// has tripped the latch (`Storage`).
fn chain_error(e: ChainError) -> AncestryError {
    match e {
        ChainError::Storage(s) => AncestryError::Storage(s),
        other => unreachable!("a reader read failed with {other}"),
    }
}
