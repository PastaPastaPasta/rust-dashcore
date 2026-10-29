//! Readers on other threads (SB4 §4, §10.1, §13.1).

use std::sync::Arc;

use dash_clock::NodeTime;
use dash_consensus::BlockId;
use dash_primitives::{BlockHash, BlockHeader};

use crate::Work;
use crate::chain::MutableGroup;
use crate::error::{ChainError, IndexInconsistency, StoreError};
use crate::shared::ReaderState;
use crate::snapshot::ChainReadSnapshot;
use crate::status::Status;
use crate::status_journal::{JournalRetention, SnapshotExpired};
use crate::walk::{self, Link};

/// A position in a block or undo file: Core's `FlatFilePos` (`flatfile.h`). A `dash-chain`
/// copy of CH2's type, which lives in a node crate (SB0 §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlatFilePos {
    pub file: i32,
    pub pos: u32,
}

/// What CH2 reads from the block index (CH2 §2.11).
pub trait IndexAccess: Send + Sync {
    /// Core `LookupBlockIndex`.
    fn lookup(&self, h: &BlockHash) -> Option<BlockId>;
    /// Core `GetBlockHash`: the entry's hash.
    fn hash(&self, id: BlockId) -> Option<BlockHash>;
    /// Core `GetAncestor` (`chain.cpp:84-119`).
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;
    /// Core `LastCommonAncestor` (`chain.cpp:166-181`).
    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId>;
    fn height(&self, id: BlockId) -> Option<u32>;
    /// The block data position when `HAVE_DATA`; `None` in SPV.
    fn have_data_pos(&self, id: BlockId) -> Option<FlatFilePos>;
    /// The undo data position when `HAVE_UNDO`; `None` in SPV.
    fn undo_pos(&self, id: BlockId) -> Option<FlatFilePos>;
    fn status(&self, id: BlockId) -> Option<Status>;
}

/// An entry as a reader sees it: the immutable fields plus one seqlocked read of the mutable
/// group (R2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryView {
    pub id: BlockId,
    pub hash: BlockHash,
    pub header: BlockHeader,
    pub prev: Option<BlockId>,
    pub height: u32,
    pub chain_work: Work,
    /// `GetMedianTimePast()`; `MTP_UNKNOWN` for the lowest ten anchor-pack entries.
    pub mtp: i64,
    /// Core's `nTimeMax`.
    pub time_max: u32,
    pub status: Status,
    pub n_tx: u32,
    /// `nFile`; `None` is Core's -1.
    pub file: Option<i32>,
    pub data_pos: u32,
    pub undo_pos: u32,
}

/// Core's `pindexFirst*` state at one entry of `CheckBlockIndex`'s walk: the oldest
/// ancestor-or-self on the path from genesis with each property. The `not_*` fields skip the
/// genesis entry, as Core's do. Core's `pindexFirstNotTreeValid` is never set here (SB4
/// refuses an entry below `VALID_TREE`), nor `pindexFirstAssumeValid` (no assumeutxo).
// Core: validation.cpp:5640-5674 ChainstateManager::CheckBlockIndex
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstAncestors {
    /// `pindexFirstInvalid`: `FAILED_VALID`.
    pub invalid: Option<BlockId>,
    /// `pindexFirstConflicing`: `CONFLICT_CHAINLOCK`.
    pub conflicting: Option<BlockId>,
    /// `pindexFirstMissing`: no `HAVE_DATA`.
    pub missing: Option<BlockId>,
    /// `pindexFirstNeverProcessed`: `nTx == 0`.
    pub never_processed: Option<BlockId>,
    /// `pindexFirstNotTransactionsValid`.
    pub not_transactions_valid: Option<BlockId>,
    /// `pindexFirstNotChainValid`.
    pub not_chain_valid: Option<BlockId>,
    /// `pindexFirstNotScriptsValid`.
    pub not_scripts_valid: Option<BlockId>,
}

impl FirstAncestors {
    /// The parent's state carried to `e`, as Core's walk updates it on entering an entry.
    pub(crate) fn extend(mut self, e: &EntryView) -> FirstAncestors {
        let validity = e.status.bits() & Status::VALID_MASK;
        let not_genesis = e.prev.is_some();
        for (first, has) in [
            (&mut self.invalid, e.status.has(Status::FAILED_VALID)),
            (&mut self.conflicting, e.status.has(Status::CONFLICT_CHAINLOCK)),
            (&mut self.missing, !e.status.has(Status::HAVE_DATA)),
            (&mut self.never_processed, e.n_tx == 0),
            (
                &mut self.not_transactions_valid,
                not_genesis && validity < Status::VALID_TRANSACTIONS,
            ),
            (&mut self.not_chain_valid, not_genesis && validity < Status::VALID_CHAIN),
            (&mut self.not_scripts_valid, not_genesis && validity < Status::VALID_SCRIPTS),
        ] {
            if first.is_none() && has {
                *first = Some(e.id);
            }
        }
        self
    }
}

/// The node's half of `CheckBlockIndex` (CH1-A9): called once per entry, after SB4's header
/// checks pass for it, with Core's walk state there. An error ends the walk.
pub trait IndexVisitor {
    fn visit(
        &mut self,
        entry: &EntryView,
        first: &FirstAncestors,
    ) -> Result<(), IndexInconsistency>;
}

/// The reader side behind a [`ChainReader`], erased over store and residency.
pub(crate) trait ReadBackend: Send + Sync {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId>;
    fn entry(&self, id: BlockId) -> Option<EntryView>;
    fn link(&self, id: BlockId) -> Option<Link>;
    fn group(&self, id: BlockId) -> Option<MutableGroup>;
    fn readers(&self) -> &ReaderState;
    /// The lowest height held (§8.4).
    fn floor(&self) -> u32;
    /// The lowest accepted height: 0 from genesis, the anchor when anchored (§10.1 n6).
    fn anchor(&self) -> u32;
    fn try_lookup(&self, h: &BlockHash) -> Result<Option<BlockId>, StoreError>;
    fn try_entry(&self, id: BlockId) -> Result<Option<EntryView>, StoreError>;
    fn try_link(&self, id: BlockId) -> Result<Option<Link>, StoreError>;
    /// A walk reached an id with no entry or record: corruption, which trips the latch.
    fn missing(&self, id: BlockId) -> StoreError;
    /// A walk's link reached `id` off its height: corruption, which trips the latch.
    fn off_height(&self, id: BlockId) -> StoreError;
}

/// A `Clone + Send + Sync` handle for readers on other threads. It holds no lock between calls;
/// lookups take the hash index's read lock once (R1), and snapshots load lock-free (D6).
#[derive(Clone)]
pub struct ChainReader {
    pub(crate) backend: Arc<dyn ReadBackend>,
}

impl ChainReader {
    /// The last published snapshot, lock-free (`ArcSwap`).
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot> {
        self.backend.readers().tip().snapshot()
    }

    /// R1: Core `LookupBlockIndex`.
    pub fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.backend.lookup(h)
    }

    /// The immutable fields and one seqlocked status read (R2).
    pub fn entry(&self, id: BlockId) -> Option<EntryView> {
        self.backend.entry(id)
    }

    /// `id`'s status as `snap` saw it: Core's under the one `cs_main` section that also gave
    /// the tip (CH1-node D63). The live status, unless the node wrote it after `snap`'s
    /// publication; then the first old status journalled since. Read in that order: the writer
    /// journals before it writes the entry. An entry inserted after `snap` reads as of its
    /// insertion. `None` if `id` is not resident. Expired if the read needs a journal the writer
    /// dropped, `snap` being more than [`RETAIN_AT_MOST`] status writes old (BL-224, D65, D68).
    ///
    /// [`RETAIN_AT_MOST`]: crate::RETAIN_AT_MOST
    pub fn status_at(
        &self,
        snap: &ChainReadSnapshot,
        id: BlockId,
    ) -> Result<Option<Status>, SnapshotExpired> {
        let Some(group) = self.backend.group(id) else {
            return Ok(None);
        };
        #[cfg(test)]
        crate::status_journal::tests::hook(crate::status_journal::tests::Site::Reader);
        let journals = &self.backend.readers().journals;
        Ok(Some(journals.status_at(id, snap.epoch)?.unwrap_or(group.status)))
    }

    /// What the node's status journals keep (BL-224, D65, D68); SPV's keep nothing.
    pub fn journal_retention(&self) -> JournalRetention {
        self.backend.readers().journals.read()
    }

    /// Core `GetAncestor`, over immutable `prev`/`skip` of resident entries; `None` below the
    /// floor (§8.4).
    pub fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        if height < self.backend.floor() {
            return None;
        }
        walk::ancestor(id, height, |id| self.backend.link(id).ok_or(()), walk::resident_off_height)
            .ok()
            .flatten()
    }

    /// [`ChainReader::lookup`], then the store (§8.4, SB5 §5.4). `Err` only for a failed read,
    /// which has tripped the latch.
    pub fn try_lookup(&self, h: &BlockHash) -> Result<Option<BlockId>, ChainError> {
        Ok(self.backend.try_lookup(h)?)
    }

    /// [`ChainReader::entry`], then the committed record: a cold entry has [`MTP_UNKNOWN`],
    /// no block data and its committed status. `Ok(None)` for an id the chain never held.
    ///
    /// [`MTP_UNKNOWN`]: crate::MTP_UNKNOWN
    pub fn try_entry(&self, id: BlockId) -> Result<Option<EntryView>, ChainError> {
        Ok(self.backend.try_entry(id)?)
    }

    /// Core `GetAncestor` over resident entries and records: `Ok(None)` above `id`'s height,
    /// below the floor, or for an unknown `id` (§8.4).
    pub fn try_ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, ChainError> {
        if height < self.backend.floor() || self.backend.try_link(id)?.is_none() {
            return Ok(None);
        }
        Ok(walk::ancestor(id, height, |x| self.try_link(x), |x| self.backend.off_height(x))?)
    }

    /// Core `LastCommonAncestor` over resident entries and records: `Ok(None)` for an unknown
    /// id or branches that meet below the floor.
    pub fn try_lca(&self, a: BlockId, b: BlockId) -> Result<Option<BlockId>, ChainError> {
        if self.backend.try_link(a)?.is_none() || self.backend.try_link(b)?.is_none() {
            return Ok(None);
        }
        Ok(walk::last_common_ancestor(a, b, |x| self.try_link(x), |x| self.backend.off_height(x))?)
    }

    /// A walk's next link; an id with neither entry nor record is corruption.
    pub(crate) fn try_link(&self, id: BlockId) -> Result<Link, StoreError> {
        self.backend.try_link(id)?.ok_or_else(|| self.backend.missing(id))
    }

    /// Core `GetLocator` (`chain.cpp:32-54`); `None` is the empty locator, as v24's
    /// `LocatorEntries(nullptr)` (`chain.cpp:35-36`; DEC-82). [`ChainReader::try_locator`],
    /// except that a failed read (which has tripped the latch) answers the empty locator: the
    /// client is stopping, so it is never sent (§10.1, D7). An unknown id is empty too.
    pub fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash> {
        self.try_locator(id).unwrap_or_default()
    }

    /// Core `GetLocator` over resident entries and records (§10.1): Core's `LocatorEntries`
    /// density, ending at genesis, or in an anchored chain at the anchor, the lowest accepted
    /// entry, which lies on every chain the client accepts (n6): the hashes of `id`'s ancestors
    /// at Core's heights down to the anchor, each reached from the previous one, as Core walks,
    /// then the anchor unless it is already last. `Err` only for a failed read, which has
    /// tripped the latch.
    // Core: chain.cpp:32-49 LocatorEntries
    pub fn try_locator(&self, id: Option<BlockId>) -> Result<Vec<BlockHash>, ChainError> {
        let Some(id) = id else {
            return Ok(Vec::new());
        };
        let Some(height) = self.backend.try_link(id)?.map(|l| l.height) else {
            return Ok(Vec::new());
        };
        // A context entry's branch has no accepted entry below it: its walk is itself.
        let terminus = self.backend.anchor().min(height);
        let mut heights: Vec<u32> =
            walk::locator_heights(height).into_iter().take_while(|&h| h >= terminus).collect();
        if heights.last() != Some(&terminus) {
            heights.push(terminus);
        }
        // A plain loop: a `map` + `collect::<Result<_>>` shunt here moved with the inlining of
        // the store reads and cost a 64-byte copy per entry (PF0).
        let mut hashes = Vec::with_capacity(heights.len());
        let mut cur = id;
        for h in heights {
            cur = self.locator_step(cur, h)?;
            hashes.push(self.locator_hash(cur)?);
        }
        Ok(hashes)
    }

    /// `cur->GetAncestor(h)` for a locator height at or below `cur`'s and at or above the
    /// anchor, resident or cold.
    fn locator_step(&self, cur: BlockId, h: u32) -> Result<BlockId, StoreError> {
        let next = walk::ancestor(cur, h, |x| self.try_link(x), |x| self.backend.off_height(x))?;
        Ok(next.expect("locator heights descend from the start"))
    }

    fn locator_hash(&self, id: BlockId) -> Result<BlockHash, StoreError> {
        match self.backend.try_entry(id)? {
            Some(e) => Ok(e.hash),
            None => Err(self.backend.missing(id)),
        }
    }

    /// Core `IsInitialBlockDownload` against one snapshot, latched (R4, §9.4). `now` is the
    /// node clock, as in Core: [`Clock::node_time`](dash_clock::Clock::node_time) for Core's
    /// precision; a [`NodeSeconds`](dash_clock::NodeSeconds) is its whole second, exact under
    /// mock time.
    // Core: validation.cpp:1970-1993 Chainstate::IsInitialBlockDownload
    pub fn is_initial_block_download(
        &self,
        snap: &ChainReadSnapshot,
        now: impl Into<NodeTime>,
        loading_blocks: bool,
    ) -> bool {
        let tip = snap.has_tip().then_some((snap.tip.tip_work, snap.tip.tip_time));
        self.backend.readers().is_ibd(tip, now.into(), loading_blocks)
    }

    /// `nMinimumChainWork`: `-minimumchainwork`, else the network's.
    pub fn min_chain_work(&self) -> Work {
        self.backend.readers().min_chain_work
    }
}

impl IndexAccess for ChainReader {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        ChainReader::lookup(self, h)
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.backend.entry(id).map(|e| e.hash)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        ChainReader::ancestor(self, id, height)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let link = |id| self.backend.link(id).ok_or(());
        walk::last_common_ancestor(a, b, link, walk::resident_off_height).ok().flatten()
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.backend.link(id).map(|l| l.height)
    }

    // Core: chain.h:216-225 CBlockIndex::GetBlockPos
    fn have_data_pos(&self, id: BlockId) -> Option<FlatFilePos> {
        let g = self.backend.group(id)?;
        g.status.has(Status::HAVE_DATA).then(|| FlatFilePos {
            file: g.file.unwrap_or(-1),
            pos: g.data_pos,
        })
    }

    // Core: chain.h:227-236 CBlockIndex::GetUndoPos
    fn undo_pos(&self, id: BlockId) -> Option<FlatFilePos> {
        let g = self.backend.group(id)?;
        g.status.has(Status::HAVE_UNDO).then(|| FlatFilePos {
            file: g.file.unwrap_or(-1),
            pos: g.undo_pos,
        })
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        self.backend.group(id).map(|g| g.status)
    }
}
