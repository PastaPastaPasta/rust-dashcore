//! A block tree for the hub's and the drivers' tests: `IndexAccess` over blocks with fake hashes,
//! growable through a shared handle.

use std::sync::{Arc, PoisonError, RwLock};

use dash_chain::{BlockId, BlockRef, FlatFilePos, IndexAccess, Status};
use dash_consensus::primitives::BlockHash;

#[derive(Clone, Copy)]
struct Node {
    hash: BlockHash,
    prev: Option<BlockId>,
    height: u32,
    /// `HAVE_DATA`, until [`TreeIndex::prune`].
    data: bool,
}

/// The tree; genesis is block 0. `Clone` shares it.
#[derive(Clone)]
pub struct TreeIndex(Arc<RwLock<Vec<Node>>>);

/// A unique hash for block `k`.
pub fn tree_hash(k: u32) -> BlockHash {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&k.to_le_bytes());
    b[31] = 0xb3;
    BlockHash::from(b)
}

impl Default for TreeIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeIndex {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(vec![Node {
            hash: tree_hash(0),
            prev: None,
            height: 0,
            data: true,
        }])))
    }

    fn nodes(&self) -> std::sync::RwLockReadGuard<'_, Vec<Node>> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn genesis(&self) -> BlockRef {
        self.block(BlockId(0))
    }

    pub fn block(&self, id: BlockId) -> BlockRef {
        let n = self.nodes()[id.0 as usize];
        BlockRef {
            id,
            hash: n.hash,
            height: n.height,
        }
    }

    /// A child of `parent`.
    pub fn child(&self, parent: BlockRef) -> BlockRef {
        let mut nodes = self.0.write().unwrap_or_else(PoisonError::into_inner);
        let id = BlockId(u32::try_from(nodes.len()).expect("small"));
        let height = parent.height + 1;
        nodes.push(Node {
            hash: tree_hash(id.0),
            prev: Some(parent.id),
            height,
            data: true,
        });
        BlockRef {
            id,
            hash: tree_hash(id.0),
            height,
        }
    }

    /// Clears `b`'s `HAVE_DATA`, as a prune does.
    pub fn prune(&self, b: BlockRef) {
        self.0.write().unwrap_or_else(PoisonError::into_inner)[b.id.0 as usize].data = false;
    }

    /// `n` blocks on `from`, lowest first.
    pub fn branch(&self, from: BlockRef, n: usize) -> Vec<BlockRef> {
        let mut at = from;
        (0..n)
            .map(|_| {
                at = self.child(at);
                at
            })
            .collect()
    }

    /// The blocks from genesis to `tip`.
    pub fn path(&self, tip: BlockRef) -> Vec<BlockRef> {
        let mut out = Vec::new();
        let mut cur = Some(tip.id);
        while let Some(id) = cur {
            out.push(self.block(id));
            cur = self.nodes()[id.0 as usize].prev;
        }
        out.reverse();
        out
    }
}

impl IndexAccess for TreeIndex {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        let id = self.nodes().iter().position(|n| n.hash == *h)?;
        Some(BlockId(u32::try_from(id).ok()?))
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.nodes().get(id.0 as usize).map(|n| n.hash)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        let nodes = self.nodes();
        while nodes.get(id.0 as usize)?.height > height {
            id = nodes[id.0 as usize].prev?;
        }
        (nodes[id.0 as usize].height == height).then_some(id)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let h = self.height(a)?.min(self.height(b)?);
        let (mut a, mut b) = (self.ancestor(a, h)?, self.ancestor(b, h)?);
        let nodes = self.nodes();
        while a != b {
            (a, b) = (nodes[a.0 as usize].prev?, nodes[b.0 as usize].prev?);
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.nodes().get(id.0 as usize).map(|n| n.height)
    }

    fn have_data_pos(&self, _: BlockId) -> Option<FlatFilePos> {
        None
    }

    fn undo_pos(&self, _: BlockId) -> Option<FlatFilePos> {
        None
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        let data = |n: &Node| {
            if n.data {
                Status::HAVE_DATA
            } else {
                0
            }
        };
        self.nodes().get(id.0 as usize).map(|n| Status(Status::VALID_TREE | data(n)))
    }
}
