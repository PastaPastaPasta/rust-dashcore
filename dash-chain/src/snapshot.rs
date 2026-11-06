//! The published read state (P2a R3/R8, SB0 §4.1, CH2 P7; SB4 §4, §9).

use dash_consensus::{BlockId, BlsCtx};
use dash_primitives::BlockHash;

use crate::Work;
use crate::active::ActiveChain;
use crate::error::ChainError;
use crate::reader::{ChainReader, IndexAccess};
use crate::tip::BlockRef;

/// One coherent, retained view of the chain: the tip, the active chain and the status facts of
/// one publication. Immutable once published (R8).
#[derive(Clone, Debug)]
pub struct ChainReadSnapshot {
    pub tip: TipFacts,
    /// F2: the `seq` of the last tip step; 0 after `init_active_tip`.
    pub tip_seq: u64,
    pub active: ActiveChainView,
    /// `bls_ctx_for_block(params, tip_height + 1)` (P2a g2).
    pub bls: BlsCtx,
    /// Core's `bestChainLockBlockIndex` at publication, with its hash and height (SB5-A17).
    pub cl_floor: Option<BlockRef>,
    /// The ChainLock handler's `isEnabled` at publication (SB4 §7.5).
    pub cl_enabled_observed: bool,
    /// +1 on every publication. [`ChainReader::status_at`] finds the node's status journal for
    /// this snapshot by it (CH1-node D63, D68): the snapshot carries none, so a build counts no
    /// reference.
    pub epoch: u64,
}

impl ChainReadSnapshot {
    /// `false` only in the node before V connects genesis or runs `init_active_tip` (CH1-A11):
    /// the active chain is empty, `tip` names genesis as a placeholder and `tip_seq` is 0.
    pub fn has_tip(&self) -> bool {
        !self.active.chain.is_empty()
    }
}

/// The tip and best-header facts of one snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipFacts {
    pub tip: BlockId,
    pub tip_work: Work,
    pub tip_time: u32,
    pub tip_height: u32,
    pub tip_hash: BlockHash,
    /// Core's `m_best_header`.
    pub best_header: BlockId,
    /// `best_header`'s hash (SB5-A17).
    pub best_header_hash: BlockHash,
    pub best_header_time: u32,
    pub best_header_work: Work,
    pub best_header_height: u32,
}

/// A retained view of the active chain: height → id (SB4 §3.6). It holds its own root, so it
/// never reads the live chain (R8). In SPV it answers resident heights only; the writer's
/// enforcement never uses it for membership (§7.3). A published view is empty only before the
/// node has a tip ([`ChainReadSnapshot::has_tip`]); then `height`, `tip` and both
/// `fork_in_global_index`es panic, and the other methods answer `None` or `false`.
#[derive(Clone, Debug)]
pub struct ActiveChainView {
    pub(crate) chain: ActiveChain,
}

impl ActiveChainView {
    /// `CChain::Height()`.
    pub fn height(&self) -> u32 {
        self.chain.len().checked_sub(1).expect("the view has a tip (has_tip)")
    }

    /// `CChain::Tip()`.
    pub fn tip(&self) -> BlockId {
        self.chain.get(self.height()).expect("the tip is on the chain")
    }

    /// The lowest height this view answers: 0 from genesis, an anchored chain's floor
    /// (anchor − 24, §8.4), or in SPV `tip − w` once the window has moved past it (DEC-174).
    pub fn base(&self) -> u32 {
        self.chain.base()
    }

    /// `CChain::operator[]`: the id at height `h`, when `base() <= h <= height()`.
    pub fn at(&self, h: u32) -> Option<BlockId> {
        self.chain.get(h)
    }

    /// `CChain::Contains(pindex)`; the entry's height comes from `index`, so in SPV an entry
    /// that left the window answers `false` (D-C4-12).
    // Core: chain.h:465-468 CChain::Contains
    pub fn contains(&self, id: BlockId, index: &dyn IndexAccess) -> bool {
        index.height(id).is_some_and(|h| self.at(h) == Some(id))
    }

    /// `CChain::Next(pindex)`: the active successor of `id` when `id` is on the chain.
    // Core: chain.h:471-477 CChain::Next
    pub fn next(&self, id: BlockId, index: &dyn IndexAccess) -> Option<BlockId> {
        let h = index.height(id)?;
        match self.at(h) == Some(id) {
            true => self.at(h.checked_add(1)?),
            false => None,
        }
    }

    /// `CChain::FindFork(pindex)`: the highest entry on this chain that is an ancestor of `id`;
    /// `None` on an empty view, as Core's on an empty chain.
    // Core: chain.cpp:61-70 CChain::FindFork
    pub fn find_fork(&self, id: BlockId, index: &dyn IndexAccess) -> Option<BlockId> {
        let top = self.chain.len().checked_sub(1)?;
        let mut walk = Some(id);
        if index.height(id)? > top {
            walk = index.ancestor(id, top);
        }
        while let Some(w) = walk {
            if self.contains(w, index) {
                return Some(w);
            }
            walk = index.height(w)?.checked_sub(1).and_then(|h| index.ancestor(w, h));
        }
        None
    }

    /// Core `FindForkInGlobalIndex`: the first locator hash that `index` knows and this chain
    /// contains; else the chain's lowest accepted entry (genesis, or an anchored chain's anchor,
    /// §3.6, §10.1 n6), which stays known after SPV's window drops it. SPV skips locator entries
    /// that left the window (D-C4-12); [`ActiveChainView::try_fork_in_global_index`] reads them.
    /// So SPV wallet recovery (SB14, E3) must call the fallible form: here a fork below the
    /// window answers the anchor, a rewind far past it.
    // Core: validation.cpp:182-197 Chainstate::FindForkInGlobalIndex
    pub fn fork_in_global_index(&self, locator: &[BlockHash], index: &dyn IndexAccess) -> BlockId {
        locator
            .iter()
            .filter_map(|h| index.lookup(h))
            .find(|&id| self.contains(id, index))
            .unwrap_or_else(|| self.anchor())
    }

    /// [`ActiveChainView::fork_in_global_index`] over resident entries and records (SPV wallet
    /// recovery, D41): each hash through [`ChainReader::try_lookup`], and an entry below the
    /// view's base is on this chain when it is the tip's ancestor at its height. `Err` for a
    /// failed read, or for a hash the index or store resolves to an id with neither entry nor
    /// record (corruption); either has tripped the latch.
    // Core: validation.cpp:182-197 Chainstate::FindForkInGlobalIndex
    pub fn try_fork_in_global_index(
        &self,
        locator: &[BlockHash],
        reader: &ChainReader,
    ) -> Result<BlockId, ChainError> {
        let (tip, top) = (self.tip(), self.height());
        for hash in locator {
            let Some(id) = reader.try_lookup(hash)? else {
                continue;
            };
            let on_chain = match reader.try_link(id)?.height {
                h if h > top => false,
                h if h >= self.base() => self.at(h) == Some(id),
                h => reader.try_ancestor(tip, h)? == Some(id),
            };
            if on_chain {
                return Ok(id);
            }
        }
        Ok(self.anchor())
    }

    /// The lowest accepted entry: `FindForkInGlobalIndex`'s fallback (Core's `Genesis()`).
    /// Every writer keeps it on a published chain: `tip_target` and `init_active_tip` refuse a
    /// tip below it, and the node never disconnects genesis. Were it ever off, the view's lowest
    /// entry answers instead of a reader panicking: a lower fork only makes the peer send more.
    fn anchor(&self) -> BlockId {
        self.chain.anchor_id().or_else(|| self.at(self.base())).unwrap_or_else(|| self.tip())
    }
}
