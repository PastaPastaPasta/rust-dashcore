//! CH1-node's index tables (§3): what Core keeps beside `CBlockIndex` and SB4's shared entries.
//!
//! `nChainTx`, `nSequenceId`, the two sequence counters, `nLastPreciousChainwork`,
//! `setBlockIndexCandidates` (Core's comparator, one live key per member: D2), the
//! `m_blocks_unlinked` multimap and `m_best_invalid`. Nothing here is persisted: Core's
//! `CDiskBlockIndex` stores none of it, and [`NodeBlockIndex::load`] rebuilds it with Core's load
//! rules.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, VecDeque};

use dash_chain::{ChainReader, EntryView, FlatFilePos, HeaderChain, IndexWrite, Status, Work};
use dash_consensus::BlockId;
use dash_types::Numeric as _;

use super::seams::Fatal;

/// A candidate's key, ordered as Core's `CBlockIndexWorkComparator` (`blockstorage.cpp:32-49`):
/// more work is better; at equal work the smaller `nSequenceId`; then the lower address, for
/// which the arena id stands (SB4 D9, D1). The greatest key is the best (Core's `rbegin()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CandKey {
    pub work: Work,
    pub seq: Reverse<i32>,
    pub id: Reverse<BlockId>,
}

impl CandKey {
    pub fn id(&self) -> BlockId {
        self.id.0
    }
}

/// CH1-node's side tables (§3.1). The design's `NodeIndex`, renamed so SB4's public
/// `dash_chain::NodeIndex` keeps its name (D22, DEC-192).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeBlockIndex {
    /// Core `nChainTx` (`unsigned int`, `chain.h:184`); 0 = `!HaveTxsDownloaded()`. Dense by id.
    n_chain_tx: Vec<u32>,
    /// Core `nSequenceId`: 0 for loaded and header-only entries (`blockstorage.cpp:100`).
    sequence_id: Vec<i32>,
    /// `nBlockSequenceId`, from 1 (`validation.h:996`).
    next_sequence_id: i32,
    /// `nBlockReverseSequenceId`, from -1 (`:998`).
    reverse_sequence_id: i32,
    /// `nLastPreciousChainwork`, from 0 (`:1000`).
    last_precious_chainwork: Work,
    /// `setBlockIndexCandidates`.
    candidates: BTreeSet<CandKey>,
    /// The one live key of each member of `candidates` (invariant C1, §3.3).
    cand_key: HashMap<BlockId, CandKey>,
    /// `m_blocks_unlinked`: prev → children, insertion order, duplicates kept (a multimap).
    unlinked: HashMap<BlockId, Vec<BlockId>>,
    /// `m_best_invalid`.
    best_invalid: Option<BlockId>,
    /// `NotifyHeaderTip`'s static `pindexHeaderOld` (`validation.cpp:3696`).
    last_notified_header: Option<BlockId>,
    /// `SetfLargeWorkInvalidChainFound` (§4.3).
    large_work_invalid_chain: bool,
    /// DEC-196 (2) kept candidates ranked below the failed chainstate tip, so V holds commands
    /// until startup's ABC completes (D38).
    tied_fallback: bool,
    /// `LoadGenesisBlock` made this index on a devnet, so V accepts the devnet genesis block
    /// before anything else (D18).
    devnet_pending: bool,
}

impl Default for NodeBlockIndex {
    fn default() -> Self {
        NodeBlockIndex::new()
    }
}

/// The entry's key fields; panics for an id the index does not hold (node entries are resident).
pub(crate) fn entry(r: &ChainReader, id: BlockId) -> EntryView {
    r.entry(id).unwrap_or_else(|| panic!("node entry {} is resident", id.0))
}

impl NodeBlockIndex {
    /// Empty tables, Core's initial counters.
    pub fn new() -> NodeBlockIndex {
        NodeBlockIndex {
            n_chain_tx: Vec::new(),
            sequence_id: Vec::new(),
            next_sequence_id: 1,
            reverse_sequence_id: -1,
            last_precious_chainwork: Work::ZERO,
            candidates: BTreeSet::new(),
            cand_key: HashMap::new(),
            unlinked: HashMap::new(),
            best_invalid: None,
            last_notified_header: None,
            large_work_invalid_chain: false,
            tied_fallback: false,
            devnet_pending: false,
        }
    }

    fn slot(&mut self, id: BlockId) -> usize {
        let i = id.0 as usize;
        if i >= self.n_chain_tx.len() {
            self.n_chain_tx.resize(i + 1, 0);
            self.sequence_id.resize(i + 1, 0);
        }
        i
    }

    fn set_n_chain_tx(&mut self, id: BlockId, n: u32) {
        let i = self.slot(id);
        self.n_chain_tx[i] = n;
    }

    // ---- reads ---------------------------------------------------------------------------------

    /// Core `nChainTx`.
    pub fn n_chain_tx(&self, id: BlockId) -> u32 {
        self.n_chain_tx.get(id.0 as usize).copied().unwrap_or(0)
    }

    /// Core `HaveTxsDownloaded()`.
    pub fn have_txs_downloaded(&self, id: BlockId) -> bool {
        self.n_chain_tx(id) != 0
    }

    /// `IsValid(BLOCK_VALID_TRANSACTIONS) && HaveTxsDownloaded()`, never ChainLock-conflicting:
    /// the tip-independent part of Core's candidate tests.
    pub fn is_candidate_eligible(&self, e: &EntryView) -> bool {
        e.status.is_valid(Status::VALID_TRANSACTIONS)
            && !e.status.has(Status::CONFLICT_CHAINLOCK)
            && self.have_txs_downloaded(e.id)
    }

    /// Core `nSequenceId`.
    pub fn sequence_id(&self, id: BlockId) -> i32 {
        self.sequence_id.get(id.0 as usize).copied().unwrap_or(0)
    }

    pub fn next_sequence_id(&self) -> i32 {
        self.next_sequence_id
    }

    pub fn reverse_sequence_id(&self) -> i32 {
        self.reverse_sequence_id
    }

    pub fn last_precious_chainwork(&self) -> Work {
        self.last_precious_chainwork
    }

    pub fn best_invalid(&self) -> Option<BlockId> {
        self.best_invalid
    }

    pub fn large_work_invalid_chain(&self) -> bool {
        self.large_work_invalid_chain
    }

    /// The candidates in comparator order, worst first.
    pub fn candidates(&self) -> impl DoubleEndedIterator<Item = BlockId> + '_ {
        self.candidates.iter().map(CandKey::id)
    }

    /// The candidates' keys as the set holds them, worst first.
    #[cfg(test)]
    pub fn candidate_keys(&self) -> impl DoubleEndedIterator<Item = CandKey> + '_ {
        self.candidates.iter().copied()
    }

    /// Breaks C1: the best candidate leaves the set, not `cand_key`.
    #[cfg(test)]
    pub fn break_c1(&mut self) {
        self.candidates.pop_last();
    }

    /// Core `setBlockIndexCandidates.rbegin()`.
    pub fn best_candidate(&self) -> Option<BlockId> {
        self.candidates.last().map(CandKey::id)
    }

    pub fn is_candidate(&self, id: BlockId) -> bool {
        self.cand_key.contains_key(&id)
    }

    /// `m_blocks_unlinked` as (prev, child) pairs, each prev's children in insertion order.
    pub fn unlinked(&self) -> impl Iterator<Item = (BlockId, BlockId)> + '_ {
        self.unlinked.iter().flat_map(|(&p, cs)| cs.iter().map(move |&c| (p, c)))
    }

    /// The children of `prev` in `m_blocks_unlinked`, duplicates included.
    pub fn unlinked_children(&self, prev: BlockId) -> &[BlockId] {
        self.unlinked.get(&prev).map_or(&[], Vec::as_slice)
    }

    /// `id`'s key from its current work and sequence id.
    pub fn key(&self, e: &EntryView) -> CandKey {
        CandKey {
            work: e.chain_work,
            seq: Reverse(self.sequence_id(e.id)),
            id: Reverse(e.id),
        }
    }

    pub fn key_of(&self, r: &ChainReader, id: BlockId) -> CandKey {
        self.key(&entry(r, id))
    }

    // ---- candidates, one live key per member (§3.3) --------------------------------------------

    /// Inserts `id` with its current key, replacing a key it already has (D2).
    pub fn candidates_insert(&mut self, r: &ChainReader, id: BlockId) {
        let key = self.key_of(r, id);
        if let Some(old) = self.cand_key.insert(id, key) {
            self.candidates.remove(&old);
        }
        self.candidates.insert(key);
    }

    /// Core `setBlockIndexCandidates.erase(pindex)`; a non-member is a no-op.
    pub fn candidates_erase(&mut self, id: BlockId) {
        if let Some(key) = self.cand_key.remove(&id) {
            self.candidates.remove(&key);
        }
    }

    /// Writes `nSequenceId`, re-keying a member (§3.3).
    pub fn set_sequence_id(&mut self, r: &ChainReader, id: BlockId, seq: i32) {
        let member = self.cand_key.contains_key(&id);
        if member {
            self.candidates_erase(id);
        }
        let i = self.slot(id);
        self.sequence_id[i] = seq;
        if member {
            self.candidates_insert(r, id);
        }
    }

    /// Core `TryAddBlockIndexCandidate` (`validation.cpp:4233-4263`), active chainstate only (D9):
    /// never a ChainLock-conflicting block; never one worse than the tip.
    pub fn try_add_candidate(&mut self, r: &ChainReader, id: BlockId, tip: Option<BlockId>) {
        let e = entry(r, id);
        if e.status.has(Status::CONFLICT_CHAINLOCK) {
            return;
        }
        if let Some(t) = tip
            && self.key(&e) < self.key_of(r, t)
        {
            return;
        }
        self.candidates_insert(r, id);
    }

    /// Core `PruneBlockIndexCandidates` (`validation.cpp:3584-3593`): drops every candidate worse
    /// than the tip.
    pub fn prune_block_index_candidates(&mut self, r: &ChainReader, tip: BlockId) {
        let tip_key = self.key_of(r, tip);
        self.prune_until(|k| *k >= tip_key);
        debug_assert!(!self.candidates.is_empty(), "validation.cpp:3592 assert");
    }

    /// Drops every candidate with less work than the failed `tip`: `LoadChainTip`'s prune at an
    /// invalid tip that a valid block ties (DEC-196 (2)). A kept candidate ranked below the tip,
    /// a state Core's prune never leaves, marks the hold for V (D38).
    pub(crate) fn prune_candidates_below(&mut self, r: &ChainReader, tip: BlockId) {
        let tip_key = self.key_of(r, tip);
        self.prune_until(|k| k.work >= tip_key.work);
        self.tied_fallback = self.candidates.first().is_some_and(|k| *k < tip_key);
    }

    /// Whether V holds commands until startup's ABC completes (D38), once.
    pub(crate) fn take_tied_fallback(&mut self) -> bool {
        std::mem::take(&mut self.tied_fallback)
    }

    /// `LoadGenesisBlock` on a devnet: V accepts the devnet genesis block at its start (D18).
    pub(crate) fn set_devnet_pending(&mut self) {
        self.devnet_pending = true;
    }

    /// Whether V accepts the devnet genesis block at its start (D18), once.
    pub(crate) fn take_devnet_pending(&mut self) -> bool {
        std::mem::take(&mut self.devnet_pending)
    }

    /// Drops the lowest candidates until one is to be kept.
    fn prune_until(&mut self, keep: impl Fn(&CandKey) -> bool) {
        while let Some(&lowest) = self.candidates.first() {
            if keep(&lowest) {
                break;
            }
            self.candidates.pop_first();
            self.cand_key.remove(&lowest.id());
        }
    }

    /// Invariant C1: `cand_key` and `candidates` are a bijection.
    pub fn check_c1(&self) -> Result<(), &'static str> {
        if self.cand_key.len() != self.candidates.len() {
            return Err("C1: cand_key and candidates differ in size");
        }
        for (id, key) in &self.cand_key {
            if key.id() != *id || !self.candidates.contains(key) {
                return Err("C1: a member's key is not in the set");
            }
        }
        Ok(())
    }

    // ---- other writes --------------------------------------------------------------------------

    /// `PreciousBlock`'s counter (`validation.cpp:3868-3879`) for a tip with `tip_work`: the
    /// sequence id to give the block. It restarts at -1 once the chain has gained work since the
    /// last call, and stops decreasing at `i32::MIN`.
    pub fn take_precious_sequence_id(&mut self, tip_work: Work) -> i32 {
        if tip_work > self.last_precious_chainwork {
            self.reverse_sequence_id = -1;
        }
        self.last_precious_chainwork = tip_work;
        let seq = self.reverse_sequence_id;
        if seq > i32::MIN {
            self.reverse_sequence_id -= 1;
        }
        seq
    }

    pub fn set_best_invalid(&mut self, id: Option<BlockId>) {
        self.best_invalid = id;
    }

    pub fn set_large_work_invalid_chain(&mut self, on: bool) {
        self.large_work_invalid_chain = on;
    }

    /// `m_blocks_unlinked.insert({prev, id})`.
    pub fn unlinked_insert(&mut self, prev: BlockId, id: BlockId) {
        self.unlinked.entry(prev).or_default().push(id);
    }

    /// `NotifyHeaderTip`'s compare-and-record: whether `best` differs from the last notified.
    pub fn take_header_notification(&mut self, best: BlockId) -> bool {
        let changed = self.last_notified_header != Some(best);
        self.last_notified_header = Some(best);
        changed
    }

    // ---- ReceivedBlockTransactions (§4.9) -------------------------------------------------------

    /// Core `ReceivedBlockTransactions` (`validation.cpp:4266-4305`): `id`'s block data at `pos`
    /// with `n_tx` transactions, then `nChainTx`, sequence ids and candidates for `id` and every
    /// unlinked descendant now reachable, in Core's queue order.
    pub fn received_block_transactions(
        &mut self,
        chain: &mut HeaderChain,
        r: &ChainReader,
        id: BlockId,
        n_tx: u32,
        pos: FlatFilePos,
    ) -> Result<(), Fatal> {
        let mut ix = chain.node_index()?;
        // :4269-4276
        ix.modify(id, |g| {
            g.n_tx = n_tx;
            g.file = Some(pos.file);
            g.data_pos = pos.pos;
            g.undo_pos = 0;
            g.status = Status(g.status.bits() | Status::HAVE_DATA)
                .raise_validity(Status::VALID_TRANSACTIONS)
                .0;
        });
        self.set_n_chain_tx(id, 0);
        ix.mark_dirty(id);

        let e = entry(r, id);
        let tip = chain.private_tip().map(|(t, _)| t.id);
        if e.prev.is_none_or(|p| self.have_txs_downloaded(p)) {
            // :4278-4298
            let mut queue = VecDeque::from([id]);
            while let Some(b) = queue.pop_front() {
                let eb = entry(r, b);
                let prev_tx = eb.prev.map_or(0, |p| self.n_chain_tx(p));
                // Core's unsigned addition wraps; mainnet's total is far below 2^32.
                self.set_n_chain_tx(b, prev_tx.wrapping_add(eb.n_tx));
                // :4286. Core's signed overflow is UB; abort instead (OH-n3).
                let seq = self.next_sequence_id;
                self.next_sequence_id =
                    seq.checked_add(1).ok_or_else(|| Fatal::new("nSequenceId overflow"))?;
                self.set_sequence_id(r, b, seq);
                self.try_add_candidate(r, b, tip);
                if let Some(children) = self.unlinked.remove(&b) {
                    queue.extend(children);
                }
            }
        } else if let Some(p) = e.prev
            && entry(r, p).status.is_valid(Status::VALID_TREE)
        {
            // :4301-4303
            self.unlinked_insert(p, id);
        }
        Ok(())
    }

    // ---- the prune's clears (CH2 §4.2 step 7) -----------------------------------------------------

    // Core: node/blockstorage.cpp:131-163 BlockManager::PruneOneBlockFile, the index half, for
    // each of `files`
    /// Step 7's clears (CH1-node §7.2): every entry whose block data is in one of `files` loses
    /// HAVE_DATA and HAVE_UNDO, its file and its positions, and is dirtied; and every
    /// `m_blocks_unlinked` pair of an entry Core's `nFile` places in `files` is erased, duplicates
    /// included (OH-m8). Core's `nFile` is 0 for an entry with no block data, so pruning file 0
    /// erases such entries' pairs too; Core also dirties them, which rewrites their records
    /// unchanged, so here they are left clean. The cleared entries' records, in id order, for Bk
    /// or Bp; they stay dirty for the next B1.
    ///
    /// # Errors
    ///
    /// [`Fatal`] for a chain that is not the node's.
    pub fn prune_clear(
        &mut self,
        chain: &mut HeaderChain,
        files: &[u32],
    ) -> Result<Vec<IndexWrite>, Fatal> {
        let r = chain.reader();
        let in_files = |file: i32| u32::try_from(file).is_ok_and(|f| files.contains(&f));
        // `ids` ascend.
        let cleared: Vec<BlockId> =
            chain.ids().filter(|&id| entry(&r, id).file.is_some_and(in_files)).collect();
        // The one call that can fail, before anything changes.
        let mut ix = chain.node_index()?;
        // :150-157, before the clears move the entries' files to Core's 0.
        self.unlinked.retain(|_, children| {
            children.retain(|&c| !in_files(entry(&r, c).file.unwrap_or(0)));
            !children.is_empty()
        });
        for &id in &cleared {
            // :139-144
            ix.modify(id, |g| {
                g.status = Status(g.status.bits() & !(Status::HAVE_DATA | Status::HAVE_UNDO));
                g.file = None;
                g.data_pos = 0;
                g.undo_pos = 0;
            });
            ix.mark_dirty(id);
        }
        Ok(cleared.into_iter().map(|id| ix.record(id)).collect())
    }

    // ---- load (§7.3 step 4) ---------------------------------------------------------------------

    /// Core's load rules over SB4's loaded index, in height order (ids within a height, D1):
    /// `nChainTx` and `m_blocks_unlinked` (`blockstorage.cpp:305-320`), the candidates
    /// (`validation.cpp:5351-5358`; the tip is still null) and `m_best_invalid` (`:5359-5361`).
    /// Every sequence id is 0 and the counters start over, as for Core's loaded entries.
    pub fn load(chain: &HeaderChain) -> NodeBlockIndex {
        NodeBlockIndex::load_until(chain, || false).expect("never interrupted")
    }

    /// [`load`](Self::load), asking `interrupted` before each entry of both passes, as Core's
    /// passes ask `ShutdownRequested()` (`blockstorage.cpp:296`, `validation.cpp:5342`); `None`
    /// once it says yes.
    pub fn load_until(
        chain: &HeaderChain,
        interrupted: impl Fn() -> bool,
    ) -> Option<NodeBlockIndex> {
        let r = chain.reader();
        let mut idx = NodeBlockIndex::new();
        let mut order: Vec<EntryView> = chain.ids().map(|id| entry(&r, id)).collect();
        order.sort_unstable_by_key(|e| (e.height, e.id));
        for e in &order {
            if interrupted() {
                return None;
            }
            if e.n_tx > 0 {
                match e.prev {
                    None => idx.set_n_chain_tx(e.id, e.n_tx),
                    Some(p) if idx.have_txs_downloaded(p) => {
                        idx.set_n_chain_tx(e.id, idx.n_chain_tx(p).wrapping_add(e.n_tx));
                    }
                    Some(p) => idx.unlinked_insert(p, e.id),
                }
            }
        }
        for e in &order {
            if interrupted() {
                return None;
            }
            if e.status.is_valid(Status::VALID_TRANSACTIONS)
                && (idx.have_txs_downloaded(e.id) || e.prev.is_none())
            {
                idx.try_add_candidate(&r, e.id, None);
            }
            if e.status.has(Status::FAILED_MASK)
                && idx.best_invalid.is_none_or(|b| e.chain_work > entry(&r, b).chain_work)
            {
                idx.best_invalid = Some(e.id);
            }
        }
        Some(idx)
    }
}
