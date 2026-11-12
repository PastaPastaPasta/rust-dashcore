//! Test builders (feature `test-utils`), for this crate's tests and its consumers': an in-memory
//! block tree and proTxHashes. The DML's builders (masternodes, lists) are `dash_dml::testkit`
//! (EV addendum §1.2).

use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_num::{Arith256, CompactTarget};
use dash_primitives::BlockHash;
use dash_types::Numeric;

use crate::types::ProTxHash;

/// A proTxHash whose internal bytes start with `n` (the rest zero).
pub fn pro_tx(n: u8) -> ProTxHash {
    let mut bytes = [0u8; 32];
    bytes[0] = n;
    ProTxHash::from(bytes)
}

/// An in-memory block tree with forks, implementing [`ChainAncestry`] as CH1's arena does.
///
/// Block hashes are synthetic and unique: the block's id and height in the first eight bytes.
/// `ancestor` walks parents, which is linear and enough for tests.
#[derive(Clone, Debug)]
pub struct MemChain {
    blocks: Vec<(HeaderCtx, Option<BlockId>)>,
}

impl MemChain {
    /// A tree holding only a genesis block, [`MemChain::genesis`].
    pub fn new() -> MemChain {
        let mut chain = MemChain {
            blocks: Vec::new(),
        };
        chain.push(None, 0);
        chain
    }

    /// The genesis block.
    pub fn genesis(&self) -> BlockId {
        BlockId(0)
    }

    /// Adds a child of `parent` and returns it.
    pub fn child(&mut self, parent: BlockId) -> BlockId {
        let height = self.header(parent).height + 1;
        self.push(Some(parent), height)
    }

    /// Adds `n` blocks on top of `tip`, returning the new tip.
    pub fn extend(&mut self, tip: BlockId, n: u32) -> BlockId {
        (0..n).fold(tip, |tip, _| self.child(tip))
    }

    /// The block's hash.
    pub fn hash(&self, id: BlockId) -> BlockHash {
        self.header(id).hash
    }

    fn push(&mut self, parent: Option<BlockId>, height: u32) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("fewer than 2^32 test blocks"));
        let mut hash = [0u8; 32];
        hash[..4].copy_from_slice(&id.0.to_le_bytes());
        hash[4..8].copy_from_slice(&height.to_le_bytes());
        hash[31] = 0xe0; // never the null hash
        let header = HeaderCtx {
            height,
            hash: BlockHash::from(hash),
            time: 0,
            bits: CompactTarget::new(0),
            version: 0,
            mtp: 0,
            chain_work: Arith256::ZERO,
        };
        self.blocks.push((header, parent));
        id
    }
}

impl Default for MemChain {
    fn default() -> Self {
        MemChain::new()
    }
}

impl ChainAncestry for MemChain {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.blocks[id.0 as usize].0
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let mut at = id;
        while self.header(at).height > height {
            at = self.prev(at)?;
        }
        (self.header(at).height == height).then_some(at)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.blocks[id.0 as usize].1
    }
}
