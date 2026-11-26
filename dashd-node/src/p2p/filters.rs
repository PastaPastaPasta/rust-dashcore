//! The basic filter index the compact-filter handlers serve from: Core's `GetBlockFilterIndex`
//! (BASIC), present once the index has opened and until shutdown destroys it.
// Core: init.cpp:2306, :441-442; index/blockfilterindex.cpp:526-530 at v24.0.0

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use dash_consensus::BlockId;
use dash_num::Hash256;
use dash_primitives::BlockHash;
use dashd_indexes::api::{Ancestry, BlockRef, FilterIndexReader, IndexError};
use dashd_p2p::processing::seams::{BlockFilterIndex, ChainAccess};

/// The processor's [`BlockFilterIndex`] over an attachable [`FilterIndexReader`], resolving
/// block ids on the chain the handlers read.
#[derive(Default)]
pub(crate) struct FilterSlot {
    chain: OnceLock<Arc<dyn ChainAccess>>,
    reader: Mutex<Option<Arc<dyn FilterIndexReader>>>,
}

impl FilterSlot {
    fn slot(&self) -> MutexGuard<'_, Option<Arc<dyn FilterIndexReader>>> {
        self.reader.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Step 7c: the chain the processor reads.
    pub(crate) fn bind_chain(&self, chain: Arc<dyn ChainAccess>) {
        assert!(self.chain.set(chain).is_ok(), "the processor attaches once");
    }

    /// The index has opened.
    pub(crate) fn attach(&self, reader: Arc<dyn FilterIndexReader>) {
        *self.slot() = Some(reader);
    }

    /// Shutdown: the handlers find no index.
    pub(crate) fn detach(&self) {
        self.slot().take();
    }

    /// A reader `Err` is Core's failed lookup: logged, then the handler ignores the request.
    // Core: index/blockfilterindex.cpp:380-524; net_processing.cpp:3507, :3541, :3549, :3589 at
    // v24.0.0
    fn read<T>(
        &self,
        id: BlockId,
        read: impl FnOnce(&dyn FilterIndexReader, &BlockRef, &Tree<'_>) -> Result<Option<T>, IndexError>,
    ) -> Option<T> {
        let reader = self.slot().clone()?;
        let tree = Tree(self.chain.get()?.as_ref());
        let block = tree.block(id)?;
        read(reader.as_ref(), &block, &tree).inspect_err(|e| tracing::error!("{e}")).ok()?
    }
}

struct Tree<'a>(&'a dyn ChainAccess);

impl Tree<'_> {
    fn block(&self, id: BlockId) -> Option<BlockRef> {
        let entry = self.0.entry(id)?;
        Some(BlockRef {
            id: entry.id,
            hash: entry.hash,
            height: entry.height,
        })
    }
}

impl Ancestry for Tree<'_> {
    fn ancestor(&self, block: &BlockRef, height: u32) -> Option<BlockRef> {
        self.block(self.0.ancestor(block.id, height)?)
    }
}

impl BlockFilterIndex for FilterSlot {
    fn available(&self) -> bool {
        self.slot().is_some()
    }

    fn filter(&self, id: BlockId) -> Option<Vec<u8>> {
        self.read(id, |r, block, _| r.filter(block)).map(|f| f.encoded().to_vec())
    }

    fn filter_header(&self, id: BlockId) -> Option<Hash256> {
        self.read(id, |r, block, _| r.filter_header(block))
    }

    fn filters(&self, start: u32, stop: BlockId) -> Option<Vec<(BlockHash, Vec<u8>)>> {
        self.read(stop, |r, stop, tree| {
            let Some(filters) = r.filter_range(start, stop, tree)? else {
                return Ok(None);
            };
            let hashes: Option<Vec<_>> =
                (start..=stop.height).map(|h| tree.ancestor(stop, h).map(|b| b.hash)).collect();
            Ok(hashes.filter(|h| h.len() == filters.len()).map(|hashes| {
                hashes.into_iter().zip(filters).map(|(h, f)| (h, f.encoded().to_vec())).collect()
            }))
        })
    }

    fn filter_hashes(&self, start: u32, stop: BlockId) -> Option<Vec<Hash256>> {
        self.read(stop, |r, stop, tree| r.filter_hash_range(start, stop, tree))
    }

    /// The summary's height on the active chain, when that block is indexed: Core's summary also
    /// reports zero before any block, and its best block may be stale after a reorg.
    fn best_height(&self) -> Option<u32> {
        let reader = self.slot().clone()?;
        let chain = self.chain.get()?;
        let snap = chain.snapshot();
        let height = reader.summary().best_block_height.min(snap.tip.tip_height);
        let block = Tree(chain.as_ref()).block(chain.active_at(&snap, height)?)?;
        reader.filter_header(&block).ok()?.map(|_| height)
    }
}
