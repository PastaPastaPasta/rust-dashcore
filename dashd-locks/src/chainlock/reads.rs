//! The chain reads the ChainLock signer and the first-seen map share.

use std::sync::Arc;

use dash_consensus::Block;
use dash_primitives::BlockHash;

/// Core's `TX_CONFIRM_THRESHOLD`: the depth below the tip past which transactions are safe.
// Core: v24.0.2 src/chainlock/chainlock.h:21
pub const TX_CONFIRM_THRESHOLD: i32 = 5;

/// A block index entry: `GetBlockHash()`, `nHeight` and `pprev`'s hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainEntry {
    pub hash: BlockHash,
    pub height: i32,
    pub prev: Option<BlockHash>,
}

/// The block tree and block files, read without waiting on V.
pub trait ChainReads: Send + Sync {
    /// `ActiveChain().Tip()`.
    fn tip(&self) -> Option<ChainEntry>;
    /// `LookupBlockIndex(hash)`.
    fn lookup(&self, hash: &BlockHash) -> Option<ChainEntry>;
    /// `GetAncestor(height)` of the block `hash`: the block itself at its own height.
    fn ancestor_hash(&self, hash: &BlockHash, height: i32) -> Option<BlockHash>;
    /// `ReadBlockFromDisk`: `None` for an unknown block or a failed read.
    fn read_block(&self, hash: &BlockHash) -> Option<Arc<Block>>;
}

/// The node's abort for a fault Core throws.
pub type Abort = Arc<dyn Fn(&str) + Send + Sync>;
