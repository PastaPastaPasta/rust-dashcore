//! P3's [`ChainAccess`] over CH1-node's V (P3 §2.4, S6a): the read side through
//! [`ChainViewHandle`], `ProcessHeaders` and `ProcessBlock` through [`ChainHandle`].
//!
//! Every snapshot the processor gets is built from one published [`ChainView`], which its
//! [`ViewToken`] holds (BL-236): `status`, `valid_scripts`, `in_active_chain` and `active_at`
//! answer at that view, and `is_initial_block_download` against that view's snapshot (P3 D111
//! R1, D112). A status read of a view more than `RETAIN_AT_MOST` status writes old is
//! `SnapshotExpired`. For `status` the adapter then reads the entry again, whole, on the newest
//! view, never `None` (CH1 D68 r1 M1, D120); `valid_scripts` answers `ViewExpired` and its caller
//! restarts its whole decision on the newest snapshot (DEC-292, D121).

use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dash_chain::{EntryView, FlatFilePos, HeaderStop, IndexAccess, SnapshotExpired, Status};
use dash_clock::NodeSeconds;
use dash_consensus::{Block, BlockId};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{BlockFiles, ChainHandle, ChainView, ChainViewHandle};
use dashd_p2p::processing::headers::MAX_BLOCKS_TO_ANNOUNCE;
use dashd_p2p::processing::seams::{
    BlockOutcome, BlockPos, BlockRead, BlockSubmit, BlockWait, ChainAccess, ChainSnapshot,
    EntryFacts, EntryStatus, HeaderBatch, HeadersResult, Interrupted, TipFacts, TreeRead, Validity,
    ViewExpired, ViewToken, Work,
};

/// The connected blocks [`ChainAccess::read_block`] can return. Section 7's `cmpctblock` reads
/// only a block `UpdatedBlockTip` gave it to announce, one of the last `MAX_BLOCKS_TO_ANNOUNCE`
/// an ABC call connected; a miss would need eight times that many connections between the
/// announcement and the peer's next `SendMessages`, each with its proof of work (D113).
const RECENT_BLOCKS: usize = 8 * MAX_BLOCKS_TO_ANNOUNCE;

/// The start of `NodeBlockFiles`' hash-mismatch error, the one check of the index variant of
/// `ReadBlockFromDisk` that the position variant lacks (blockstorage.cpp:840-846).
const HASH_MISMATCH: &str = "ReadBlockFromDisk(CBlock&, CBlockIndex*)";

/// The most entries the `nChainTx != 0` memo keeps before it starts again (D112).
const TXS_MEMO_MAX: usize = 8192;

/// P3's chain over V.
pub struct NodeChain {
    v: ChainHandle,
    view: ChainViewHandle,
    /// The snapshot built for the last `view_seq` asked for: every drain of one publication
    /// shares it.
    last: Mutex<Option<(u64, Arc<ChainSnapshot>)>>,
    /// Entries found to have `nChainTx != 0` off the active chain, each with the first
    /// `view_seq` it was found at. Core never clears `nChainTx` once set
    /// (`ReceivedBlockTransactions`), so a hit holds at that view and every later one.
    txs_downloaded: Mutex<HashMap<BlockId, u64>>,
    /// Status reads of an expired view, answered on the newest view (D120).
    #[cfg(test)]
    expired_reads: AtomicU64,
    /// The last [`RECENT_BLOCKS`] blocks V connected, newest last, with their hashes.
    recent: Mutex<VecDeque<(BlockHash, Arc<Block>)>>,
    /// A reader of V's block files of its own, for blocks not in `recent` (P4b D3); none in
    /// tests that serve only connected blocks. Only `msghand` reads.
    files: Option<Mutex<Box<dyn BlockFiles>>>,
}

/// A leaf lock: never held across a call out.
pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl NodeChain {
    pub fn new(v: ChainHandle, files: Option<Box<dyn BlockFiles>>) -> Self {
        Self {
            view: v.view_handle(),
            v,
            last: Mutex::new(None),
            txs_downloaded: Mutex::new(HashMap::new()),
            #[cfg(test)]
            expired_reads: AtomicU64::new(0),
            recent: Mutex::new(VecDeque::with_capacity(RECENT_BLOCKS)),
            files: files.map(Mutex::new),
        }
    }

    /// `BlockConnected`'s block, for [`ChainAccess::read_block`]. A block connected again
    /// (after a reorg back) moves to the newest place.
    pub(super) fn connected(&self, block: &Arc<Block>) {
        let hash = block.hash();
        let mut recent = lock(&self.recent);
        recent.retain(|(h, _)| *h != hash);
        if recent.len() == RECENT_BLOCKS {
            recent.pop_front();
        }
        recent.push_back((hash, Arc::clone(block)));
    }

    /// P3's snapshot of `view`, built once per `view_seq`.
    fn snapshot_of(&self, view: Arc<ChainView>) -> Arc<ChainSnapshot> {
        let mut last = lock(&self.last);
        if let Some((seq, snap)) = &*last
            && *seq == view.view_seq
        {
            return Arc::clone(snap);
        }
        let t = view.snap.tip;
        let snap = Arc::new(ChainSnapshot {
            tip: TipFacts {
                tip: t.tip,
                tip_work: t.tip_work,
                tip_time: t.tip_time,
                tip_height: t.tip_height,
                tip_hash: t.tip_hash,
                best_header: t.best_header,
                best_header_hash: t.best_header_hash,
                best_header_time: t.best_header_time,
                best_header_work: t.best_header_work,
                best_header_height: t.best_header_height,
            },
            bls: view.snap.bls,
            epoch: view.snap.epoch,
            view: ViewToken::new(Arc::clone(&view)),
        });
        *last = Some((view.view_seq, Arc::clone(&snap)));
        snap
    }

    /// The view `snap` was built from; the published one for a snapshot no adapter built.
    fn view_of(&self, snap: &ChainSnapshot) -> Arc<ChainView> {
        snap.view.get::<ChainView>().unwrap_or_else(|| self.view.view())
    }

    /// `read` at `snap`'s view; if that view has expired, on the newest one, which V's
    /// publication keeps readable (CH1 D65, D68; D120).
    fn at_view<R>(
        &self,
        snap: &ChainSnapshot,
        read: impl Fn(&ChainView) -> Result<R, SnapshotExpired>,
    ) -> R {
        if let Ok(r) = read(&self.view_of(snap)) {
            return r;
        }
        #[cfg(test)]
        self.expired_reads.fetch_add(1, Ordering::Relaxed);
        // Once, unless V writes past the bound between the load and the read.
        let mut tries = 0u32;
        loop {
            if let Ok(r) = read(&self.view.view()) {
                return r;
            }
            tries += 1;
            debug_assert!(tries < 1_000, "the newest view keeps expiring");
        }
    }

    /// `id`'s entry as `view` saw it: the view's status, and `nTx` with it. V sets `nTx`,
    /// `HAVE_DATA` and `VALID_TRANSACTIONS` in one status write (`ReceivedBlockTransactions`),
    /// and only pruning clears `HAVE_DATA`, so the view had `nTx` set exactly when its status
    /// has either (D120).
    fn entry_at(
        &self,
        view: &ChainView,
        id: BlockId,
    ) -> Result<Option<EntryView>, SnapshotExpired> {
        let (Some(mut e), Some(status)) = (self.view.entry(id), self.view.status(view, id)?) else {
            return Ok(None);
        };
        e.status = status;
        if !status.has(Status::HAVE_DATA) && validity(status) < Validity::Transactions {
            e.n_tx = 0;
        }
        Ok(Some(e))
    }

    /// Core's `HaveTxsDownloaded()` (`nChainTx != 0`, chain.h:264) as `view` saw it: V keeps
    /// `nChainTx` in its own index only, so it is derived here. Every block on `view`'s active
    /// chain has it. Off it, `ReceivedBlockTransactions` sets it when the block's transactions
    /// are in and its parent has it (validation.cpp:3593-3624), so it holds when every entry
    /// back to the active chain has `nTx` (D112).
    fn have_txs_downloaded(
        &self,
        view: &ChainView,
        e: &EntryView,
    ) -> Result<bool, SnapshotExpired> {
        let reader = self.view.reader();
        let seq = view.view_seq;
        let mut walked = Vec::new();
        let mut at = *e;
        let found = loop {
            if view.snap.active.contains(at.id, reader)
                || lock(&self.txs_downloaded).get(&at.id).is_some_and(|&s| s <= seq)
            {
                break true;
            }
            if at.n_tx == 0 {
                break false;
            }
            walked.push(at.id);
            // Genesis with its transactions.
            let Some(p) = at.prev else {
                break true;
            };
            match self.entry_at(view, p)? {
                Some(prev) => at = prev,
                None => break false,
            }
        };
        if found && !walked.is_empty() {
            let mut memo = lock(&self.txs_downloaded);
            if memo.len() + walked.len() > TXS_MEMO_MAX {
                memo.clear();
            }
            for id in walked {
                memo.entry(id).and_modify(|s| *s = (*s).min(seq)).or_insert(seq);
            }
        }
        Ok(found)
    }

    /// From the connected-block cache, else V's block files at `pos` (P4b D3). On a failed read
    /// the entry is read again: `!HAVE_DATA && nTx > 0` is Core's `IsBlockPruned` (only
    /// `PruneOneBlockFile` clears `HAVE_DATA` and keeps `nTx`, after setting `m_have_pruned`),
    /// else the read's own line is logged and it is [`BlockRead::Failed`]. `by_index` is Core's
    /// `pindex` variant, which logs a hash mismatch; the position variant leaves it to the
    /// caller. Section 7 asserts against a failed read as Core does; a panic there stops
    /// `msghand`, and the node aborts (`Net::start`).
    // Core: net_processing.cpp:2811-2829; node/blockstorage.cpp:443-447 IsBlockPruned, :833-846
    fn read(
        &self,
        id: BlockId,
        by_index: bool,
        pos: impl FnOnce() -> Option<FlatFilePos>,
    ) -> BlockRead {
        let Some(hash) = self.view.entry(id).map(|e| e.hash) else {
            return BlockRead::Failed;
        };
        if let Some((_, b)) = lock(&self.recent).iter().rev().find(|(h, _)| *h == hash) {
            return BlockRead::Block(Arc::clone(b));
        }
        let Some(files) = &self.files else {
            return BlockRead::Failed;
        };
        if let Some(pos) = pos() {
            match lock(files).read_block(pos, &hash) {
                Ok(block) => return BlockRead::Block(block),
                // Serving reads by position (np:2819), which checks no hash: a mismatch logs
                // only the caller's line.
                Err(e) if !by_index && e.starts_with(HASH_MISMATCH) => {}
                Err(e) => tracing::error!("{e}"),
            }
        }
        match self.view.entry(id) {
            Some(e) if !e.status.has(Status::HAVE_DATA) && e.n_tx > 0 => BlockRead::Pruned,
            _ => BlockRead::Failed,
        }
    }
}

/// An entry's immutable fields, as P3 reads them.
pub(super) fn facts(e: &EntryView) -> EntryFacts {
    EntryFacts {
        id: e.id,
        hash: e.hash,
        prev: e.prev,
        height: e.height,
        time: e.header.time,
        chain_work: e.chain_work,
        header: e.header,
    }
}

/// `nStatus & BLOCK_VALID_MASK` in P3's terms.
fn validity(s: Status) -> Validity {
    match s.bits() & Status::VALID_MASK {
        Status::VALID_UNKNOWN => Validity::Unknown,
        Status::VALID_RESERVED => Validity::Reserved,
        Status::VALID_TREE => Validity::Tree,
        Status::VALID_TRANSACTIONS => Validity::Transactions,
        Status::VALID_CHAIN => Validity::Chain,
        _ => Validity::Scripts,
    }
}

impl TreeRead for NodeChain {
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.view.lookup(hash)
    }

    fn entry(&self, id: BlockId) -> Option<EntryFacts> {
        self.view.entry(id).as_ref().map(facts)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.view.ancestor(id, height)
    }
}

impl ChainAccess for NodeChain {
    /// The published view's snapshot: SB4's tip cell may be one store ahead of it, and every
    /// read here answers at a view (P3 §3.6, D47).
    fn snapshot(&self) -> Arc<ChainSnapshot> {
        self.drain_snapshot().0
    }

    fn valid_scripts(&self, snap: &ChainSnapshot, id: BlockId) -> Result<bool, ViewExpired> {
        // Never read again on the newest view: the caller restarts its decision (DEC-292).
        self.view.valid_scripts(&self.view_of(snap), id).map_err(|SnapshotExpired| ViewExpired)
    }

    fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash> {
        self.view.locator(id)
    }

    fn loading_blocks(&self) -> bool {
        self.view.loading_blocks()
    }

    fn min_chain_work(&self) -> Work {
        self.view.min_chain_work()
    }

    fn is_initial_block_download(&self, snap: &ChainSnapshot, now: NodeSeconds) -> bool {
        self.view.is_initial_block_download(&self.view_of(snap).snap, now)
    }

    /// The reply's own snapshot is not P3's: the actor reads the drain after the reply (D46), so
    /// the one here is the published view's, which R3 puts at or after the reply's.
    fn process_headers(
        &self,
        batch: HeaderBatch,
        from: dash_p2p::PeerId,
    ) -> Result<HeadersResult, Interrupted> {
        let batch = dash_chain::HeaderBatch {
            headers: batch.headers,
            hashes: batch.hashes,
        };
        let r = self
            .view
            .process_headers(batch, Some(dash_chain::PeerId(from)))
            .map_err(|_| Interrupted)?;
        Ok(HeadersResult {
            last_accepted: r.last_accepted,
            invalid: match r.stop {
                Some(HeaderStop::Invalid {
                    reject,
                    ..
                }) => Some(reject),
                // SPV only.
                Some(HeaderStop::BelowAnchor {
                    ..
                })
                | None => None,
            },
            snap: self.drain_snapshot().0,
        })
    }

    /// `ProcessBlock { reply: Activated }`, with `merkle_checked` (A-4b): V's `CheckBlock` skips
    /// the merkle pass `is_block_mutated` already made.
    fn process_block(&self, req: BlockSubmit) -> Result<BlockWait, Interrupted> {
        let reply = self.v.submit_block(
            req.block,
            req.force,
            Some(dash_chain::PeerId(req.from)),
            true,
            req.merkle_checked,
        );
        Ok(BlockWait(Box::pin(async move {
            reply
                .await
                .map(|r| BlockOutcome {
                    new_block: r.new_block,
                })
                .map_err(|_| Interrupted)
        })))
    }

    /// The whole read is redone on the newest view if `snap`'s has expired, so its fields never
    /// mix two views (D120).
    fn status(&self, snap: &ChainSnapshot, id: BlockId) -> Option<EntryStatus> {
        self.at_view(snap, |view| {
            let Some(e) = self.entry_at(view, id)? else {
                return Ok(None);
            };
            let s = e.status;
            let have_data = s.has(Status::HAVE_DATA);
            Ok(Some(EntryStatus {
                have_data,
                // Core: chain.h:216-225 CBlockIndex::GetBlockPos
                data_pos: have_data.then(|| BlockPos {
                    file: e.file.unwrap_or(-1),
                    pos: e.data_pos,
                }),
                validity: validity(s),
                failed: s.has(Status::FAILED_MASK),
                n_tx: e.n_tx,
                have_txs_downloaded: self.have_txs_downloaded(view, &e)?,
            }))
        })
    }

    fn in_active_chain(&self, snap: &ChainSnapshot, id: BlockId) -> bool {
        self.view_of(snap).snap.active.contains(id, self.view.reader())
    }

    fn active_at(&self, snap: &ChainSnapshot, height: u32) -> Option<BlockId> {
        self.view_of(snap).snap.active.at(height)
    }

    // Core: chain.cpp:166-181 LastCommonAncestor
    fn last_common_ancestor(&self, a: BlockId, b: BlockId) -> BlockId {
        let entry = |id| self.view.entry(id).expect("both are in the block tree");
        let (mut a, mut b) = (entry(a), entry(b));
        if a.height > b.height {
            a = entry(self.view.ancestor(a.id, b.height).expect("below a's height"));
        } else if b.height > a.height {
            b = entry(self.view.ancestor(b.id, a.height).expect("below b's height"));
        }
        while a.id != b.id {
            a = entry(a.prev.expect("a common genesis"));
            b = entry(b.prev.expect("a common genesis"));
        }
        a.id
    }

    fn drain_snapshot(&self) -> (Arc<ChainSnapshot>, u64) {
        let view = self.view.view();
        let seq = view.view_seq;
        (self.snapshot_of(view), seq)
    }

    /// C7 (b): V has published since its last synchronous signal wherever it serves a command,
    /// so one class R round trip covers the stamp (DEC-229 (b)).
    fn wait_published(&self, seq: u64) -> Result<(), Interrupted> {
        if self.view.view().view_seq >= seq {
            return Ok(());
        }
        self.view.query(|_| ()).recv().map_err(|_| Interrupted)?;
        debug_assert!(self.view.view().view_seq >= seq, "C7 (b): published before serving");
        Ok(())
    }

    /// [`NodeChain::read`] at the entry's position now (P4b D3).
    fn read_block(&self, id: BlockId) -> BlockRead {
        // No position: the data went between the caller's `HAVE_DATA` test and this read.
        self.read(id, true, || self.view.reader().have_data_pos(id))
    }

    /// [`NodeChain::read`] at the position the caller took with its `HAVE_DATA` test (P4b D16).
    fn read_block_at(&self, id: BlockId, pos: BlockPos) -> BlockRead {
        self.read(id, false, || {
            Some(FlatFilePos {
                file: pos.file,
                pos: pos.pos,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::io::{Seek as _, SeekFrom, Write as _};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use dash_chain::{IndexAccess, SnapshotExpired, Status};
    use dash_consensus::{Block, CoreSerialize as _};
    use dash_primitives::BlockHash;
    use dashd_chainstate::chain::{ChainView, genesis_block};
    use dashd_p2p::processing::seams::{
        BlockRead, ChainAccess, HeaderBatch, TreeRead, Validity, ViewExpired,
    };

    use super::super::amb::{branch, node, regtest};
    use super::super::listener::announcements;
    use super::{HASH_MISMATCH, NodeChain, RECENT_BLOCKS, lock, validity};
    use crate::node::Node;

    fn submit(node: &Node, blocks: &[Arc<Block>]) {
        let v = node.chain().expect("V runs").handle();
        for b in blocks {
            v.process_block(Arc::clone(b), true, None, true).recv().expect("V");
        }
    }

    #[test]
    fn validity_is_core_s_valid_mask() {
        let cases = [
            (Status::VALID_UNKNOWN, Validity::Unknown),
            (Status::VALID_RESERVED, Validity::Reserved),
            (Status::VALID_TREE, Validity::Tree),
            (Status::VALID_TRANSACTIONS, Validity::Transactions),
            (Status::VALID_CHAIN, Validity::Chain),
            (Status::VALID_SCRIPTS, Validity::Scripts),
        ];
        for (bits, v) in cases {
            assert_eq!(validity(Status(bits | Status::HAVE_DATA | Status::FAILED_CHILD)), v);
        }
    }

    /// A main branch of 3, a lighter side branch of 2 with data, and a header-only third side
    /// entry, through real V: the reads P3 takes from the adapter. The main branch holds only
    /// coinbases: V's `CoreConnector` (ND1) rejects the fillers' missing inputs, and the reorg
    /// to the side branch that would follow needs `DisconnectBlock` (CH3a a5). The side branch
    /// keeps its fillers: lighter, it is stored and never connected.
    #[test]
    fn the_adapter_reads_v_as_core_s_index() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let genesis = *genesis_block(&p).header();
        let main = branch(&p, genesis, 0, &[1, 1, 1], 0x01);
        let side = branch(&p, genesis, 0, &[2, 2, 2], 0x02);
        submit(&node, &main);
        submit(&node, &side[..2]);
        let v = node.chain().unwrap().handle().clone();
        let chain = NodeChain::new(v.clone(), None);
        let r = chain.process_headers(
            HeaderBatch {
                headers: vec![*side[2].header()],
                hashes: vec![side[2].hash()],
            },
            7,
        );
        assert!(r.expect("served").invalid.is_none());
        let id = |b: &Arc<Block>| chain.lookup(&b.hash()).expect("indexed");
        let snap = chain.snapshot();
        assert_eq!(snap.tip.tip_height, 3);

        let main_tip = chain.status(&snap, id(&main[2])).unwrap();
        assert!(main_tip.have_data && main_tip.have_txs_downloaded && !main_tip.failed);
        assert_eq!(main_tip.validity, Validity::Scripts);
        assert_eq!(main_tip.n_tx, 1);
        // Off the active chain, with every entry back to it holding transactions.
        let side2 = chain.status(&snap, id(&side[1])).unwrap();
        assert!(side2.have_data && side2.have_txs_downloaded);
        assert_eq!((side2.validity, side2.n_tx), (Validity::Transactions, 2));
        assert!(lock(&chain.txs_downloaded).contains_key(&id(&side[1])), "memoised");
        // A header only: no data, no transactions downloaded.
        let side3 = chain.status(&snap, id(&side[2])).unwrap();
        assert!(!side3.have_data && !side3.have_txs_downloaded);
        assert_eq!(side3.validity, Validity::Tree);
        // `GetBlockPos()` is null exactly without `HAVE_DATA` (chain.h:216-225); serving reads
        // only the position.
        for s in [&main_tip, &side2, &side3] {
            assert_eq!(s.data_pos.is_some(), s.have_data, "{s:?}");
        }

        let g = chain.active_at(&snap, 0).expect("genesis");
        assert_eq!(chain.last_common_ancestor(id(&main[2]), id(&side[2])), g);
        assert_eq!(chain.last_common_ancestor(id(&main[0]), id(&main[2])), id(&main[0]));
        assert!(chain.in_active_chain(&snap, id(&main[1])));
        assert!(!chain.in_active_chain(&snap, id(&side[0])));
        assert_eq!(chain.active_at(&snap, 2), Some(id(&main[1])));
        assert_eq!(chain.valid_scripts(&snap, id(&main[2])), Ok(true));
        assert_eq!(chain.valid_scripts(&snap, id(&side[1])), Ok(false));

        // `read_block` holds what `BlockConnected` gave it, nothing submitted only.
        assert!(chain.read_block(id(&main[2])).block().is_none());
        chain.connected(&main[2]);
        chain.connected(&main[2]);
        assert_eq!(lock(&chain.recent).len(), 1, "deduped");
        assert_eq!(chain.read_block(id(&main[2])).block().unwrap().hash(), main[2].hash());
        for _ in 0..RECENT_BLOCKS {
            chain.connected(&side[0]);
            chain.connected(&main[0]);
        }
        assert_eq!(lock(&chain.recent).len(), 3);

        // The listener's `vHashes`: none in IBD, newest first, stopping at the fork.
        let view = v.view_handle();
        assert!(announcements(&view, id(&main[2]), Some(g), true).is_empty());
        assert_eq!(
            announcements(&view, id(&main[2]), Some(id(&main[0])), false),
            [main[2].hash(), main[1].hash()]
        );
        assert_eq!(announcements(&view, id(&main[2]), None, false).len(), 4);
    }

    /// P4b D3: a block outside the connected-block cache comes from V's block files (genesis,
    /// the active chain, a stored side branch that never connected); an entry without data is
    /// [`BlockRead::Failed`]; and [`HASH_MISMATCH`] is the start of the files' own mismatch error,
    /// which the read leaves to its caller's line.
    #[test]
    fn blocks_outside_the_cache_come_from_v_s_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let genesis = genesis_block(&p);
        let main = branch(&p, *genesis.header(), 0, &[1, 1, 1], 0x0a);
        let side = branch(&p, *genesis.header(), 0, &[2, 2, 2], 0x0b);
        submit(&node, &main);
        submit(&node, &side[..2]);
        let files = node.take_test_block_files().expect("step 7");
        let chain = NodeChain::new(node.chain().unwrap().handle().clone(), Some(files));
        let batch = HeaderBatch {
            headers: vec![*side[2].header()],
            hashes: vec![side[2].hash()],
        };
        assert!(chain.process_headers(batch, 7).expect("served").invalid.is_none());
        let id = |h: &BlockHash| chain.lookup(h).expect("indexed");

        assert!(lock(&chain.recent).is_empty());
        for b in std::iter::once(&genesis).chain(&main).chain(&side[..2]) {
            let read = chain.read_block(id(&b.hash())).block();
            assert_eq!(read.map(|r| r.hash()), Some(b.hash()));
        }
        assert!(matches!(chain.read_block(id(&side[2].hash())), BlockRead::Failed));

        let pos = chain.view.reader().have_data_pos(id(&main[0].hash())).expect("stored");
        let files = chain.files.as_ref().expect("files");
        let Err(e) = lock(files).read_block(pos, &main[1].hash()) else {
            panic!("read under another block's hash");
        };
        assert!(e.starts_with(HASH_MISMATCH), "{e}");
    }

    /// The lines a call logs on this thread, as (level, message).
    fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<(tracing::Level, String)>) {
        struct Lines(Mutex<Vec<(tracing::Level, String)>>);
        impl tracing::Subscriber for Lines {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                struct Message(String);
                impl tracing::field::Visit for Message {
                    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
                        if f.name() == "message" {
                            self.0 = format!("{v:?}");
                        }
                    }
                }
                let mut m = Message(String::new());
                event.record(&mut m);
                self.0.lock().unwrap().push((*event.metadata().level(), m.0));
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }
        let lines = Arc::new(Lines(Mutex::new(Vec::new())));
        let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
        let lines = std::mem::take(&mut *lines.0.lock().unwrap());
        (out, lines)
    }

    /// P4b D3 (review r1 N-3): faults in V's own block files, read at the position the entry
    /// still holds with `HAVE_DATA`. Bytes cut short are the read's own error, logged once as
    /// `ReadBlockFromDisk:` at error level, then [`BlockRead::Failed`]; another valid block at
    /// that position is only the caller's hash test (np:2819-2820), with no line of the read's,
    /// then [`BlockRead::Failed`] too; read by index (section 7) it is the read's own
    /// [`HASH_MISMATCH`] line (blockstorage.cpp:843).
    #[test]
    fn a_faulty_block_file_read_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let genesis = genesis_block(&p);
        let main = branch(&p, *genesis.header(), 0, &[1, 1, 1], 0x0c);
        submit(&node, &main);
        let files = node.take_test_block_files().expect("step 7");
        let chain = NodeChain::new(node.chain().unwrap().handle().clone(), Some(files));
        let target = chain.lookup(&main[0].hash()).expect("indexed");
        let pos = chain.status(&chain.snapshot(), target).unwrap().data_pos.expect("HAVE_DATA");
        assert_eq!(pos.file, 0);
        let (read, lines) = logged(|| chain.read_block_at(target, pos));
        assert_eq!(read.block().map(|b| b.hash()), Some(main[0].hash()));
        assert!(lines.is_empty(), "{lines:?}");

        let blk = dir.path().join("regtest/blocks/blk00000.dat");
        let mut file = std::fs::OpenOptions::new().write(true).open(&blk).unwrap();
        file.set_len(u64::from(pos.pos) + 10).unwrap();
        let (read, lines) = logged(|| chain.read_block_at(target, pos));
        assert!(matches!(read, BlockRead::Failed));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, tracing::Level::ERROR, "{lines:?}");
        assert!(lines[0].1.starts_with("ReadBlockFromDisk: "), "{lines:?}");

        // `main[1]`'s record (its size field, then its bytes) where `main[0]`'s was.
        let other = main[1].core_serialize();
        let size = u32::try_from(other.len()).unwrap().to_le_bytes();
        file.seek(SeekFrom::Start(u64::from(pos.pos) - 4)).unwrap();
        file.write_all(&[&size[..], &other].concat()).unwrap();
        let (read, lines) = logged(|| chain.read_block_at(target, pos));
        assert!(matches!(read, BlockRead::Failed));
        assert!(lines.is_empty(), "{lines:?}");
        let (read, lines) = logged(|| chain.read_block(target));
        assert!(matches!(read, BlockRead::Failed));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, tracing::Level::ERROR, "{lines:?}");
        assert!(lines[0].1.starts_with(HASH_MISMATCH), "{lines:?}");
        let other_id = chain.lookup(&main[1].hash()).expect("indexed");
        let (read, _) = logged(|| chain.read_block_at(other_id, pos));
        assert_eq!(read.block().map(|b| b.hash()), Some(main[1].hash()), "the bytes are main[1]'s");
    }

    /// D121 (S6b r1 n-3): the `HaveTxsDownloaded` memo keeps the first view that saw each entry,
    /// so a walk at a newer snapshot never answers for an older one, where the entry had no
    /// data yet.
    #[test]
    fn the_txs_memo_never_answers_for_an_older_view() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let genesis = *genesis_block(&p).header();
        let main = branch(&p, genesis, 0, &[1, 1, 1], 0x08);
        let side = branch(&p, genesis, 0, &[2, 2], 0x09);
        submit(&node, &main);
        let chain = NodeChain::new(node.chain().unwrap().handle().clone(), None);
        let batch = HeaderBatch {
            headers: side.iter().map(|b| *b.header()).collect(),
            hashes: side.iter().map(|b| b.hash()).collect(),
        };
        assert!(chain.process_headers(batch, 7).expect("served").invalid.is_none());
        let old = chain.snapshot();
        let h = chain.lookup(&side[1].hash()).expect("indexed");

        // The lighter side branch gets its data and stays off the active chain.
        submit(&node, &side);
        let new = chain.snapshot();
        assert!(chain.status(&new, h).unwrap().have_txs_downloaded);
        assert!(lock(&chain.txs_downloaded).contains_key(&h), "memoised at the newer view");
        let s = chain.status(&old, h).unwrap();
        assert!(!s.have_data && !s.have_txs_downloaded, "{s:?}");
    }

    /// BL-236: a snapshot held across a publication reads its own view's statuses, `nTx` with
    /// them, and its own active chain; a new one reads the new view's. Coinbase-only blocks: V's
    /// `CoreConnector` rejects the fillers' missing inputs (T22).
    #[test]
    fn a_held_snapshot_reads_its_own_view() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let main = branch(&p, *genesis_block(&p).header(), 0, &[1; 3], 0x06);
        submit(&node, &main[..2]);
        let chain = NodeChain::new(node.chain().unwrap().handle().clone(), None);
        let batch = HeaderBatch {
            headers: vec![*main[2].header()],
            hashes: vec![main[2].hash()],
        };
        assert!(chain.process_headers(batch, 7).expect("served").invalid.is_none());
        let old = chain.snapshot();
        let h = chain.lookup(&main[2].hash()).expect("indexed");

        let header_only = |snap| {
            let s = chain.status(snap, h).expect("indexed at the view");
            assert!(!s.have_data && !s.have_txs_downloaded);
            assert_eq!((s.validity, s.n_tx), (Validity::Tree, 0));
            assert_eq!(chain.valid_scripts(snap, h), Ok(false));
            assert!(!chain.in_active_chain(snap, h));
        };
        header_only(&old);
        submit(&node, &main[2..]);
        header_only(&old);
        assert_eq!(chain.active_at(&old, 3), None);

        let new = chain.snapshot();
        let s = chain.status(&new, h).unwrap();
        assert!(s.have_data && s.have_txs_downloaded);
        assert_eq!((s.validity, s.n_tx), (Validity::Scripts, 1));
        assert_eq!(chain.valid_scripts(&new, h), Ok(true));
        assert!(chain.in_active_chain(&new, h));
        assert_eq!(chain.active_at(&new, 3), Some(h));
    }

    /// CH1 D68 r1 M1 (D120): a snapshot more than `RETAIN_AT_MOST` status writes old has
    /// expired; the adapter redoes a status read on the newest view, never `None`, and answers
    /// `valid_scripts` with `ViewExpired` so its caller restarts (DEC-292). Invalidating the
    /// active chain's first block disconnects and rewrites every block's status on the way out
    /// and back (CH3a a5, D126).
    #[test]
    fn an_expired_snapshot_reads_again_on_the_newest_view() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let main = branch(&p, *genesis_block(&p).header(), 0, &[1; 40], 0x07);
        submit(&node, &main);
        let v = node.chain().unwrap().handle().clone();
        let view = v.view_handle();
        let chain = NodeChain::new(v.clone(), None);
        let snap = chain.snapshot();
        let old = snap.view.get::<ChainView>().expect("the adapter's view");
        let (first, tip) = (main[0].hash(), chain.lookup(&main[39].hash()).unwrap());
        // Each round rewrites every block's status on the way out and back.
        let mut rounds = 0;
        while view.status(&old, tip).is_ok() {
            v.invalidate_block(first).recv().unwrap().unwrap();
            v.reconsider_block(first, false).recv().unwrap().unwrap();
            rounds += 1;
            assert!(rounds < 5_000, "never expired");
        }
        assert_eq!(view.view().snap.tip.tip_hash, main[39].hash(), "every round reconnects");
        assert_eq!(view.valid_scripts(&old, tip), Err(SnapshotExpired));

        assert_eq!(chain.valid_scripts(&snap, tip), Err(ViewExpired));
        assert_eq!(chain.valid_scripts(&chain.snapshot(), tip), Ok(true));
        let s = chain.status(&snap, tip).expect("read again, not None");
        assert!(s.have_data && s.have_txs_downloaded && !s.failed);
        assert_eq!(s.validity, Validity::Scripts);
        assert_eq!(chain.expired_reads.load(Ordering::Relaxed), 1);
        assert!(chain.in_active_chain(&snap, tip));
    }

    /// `vHashes` stops at `MAX_BLOCKS_TO_ANNOUNCE` (np:2290-2293).
    #[test]
    fn announcements_stop_at_eight() {
        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let blocks = branch(&p, *genesis_block(&p).header(), 0, &[1; 12], 0x03);
        submit(&node, &blocks);
        let view = node.chain().unwrap().handle().view_handle();
        let tip = view.lookup(&blocks[11].hash()).unwrap();
        let got = announcements(&view, tip, None, false);
        let want: Vec<_> = blocks[4..].iter().rev().map(|b| b.hash()).collect();
        assert_eq!(got, want);
    }

    /// DEC-606: `getblock`'s read is Core's `GetBlockChecked` over the RPC slot's own block files,
    /// never the connected-block cache. A stored block reads back with no line; an unknown hash
    /// is `NotFound`; a header-only entry's null position, bytes cut short, and another block's
    /// record at the position are each `NotOnDisk`, with the read's own `ReadBlockFromDisk` line
    /// (the last its hash-mismatch line, blockstorage.cpp:843), while P3's reader still serves
    /// the cut block from its cache.
    // Core: rpc/blockchain.cpp:786-804 GetBlockChecked; node/blockstorage.cpp:833-846
    #[test]
    fn getblock_reads_the_slot_s_own_block_files() {
        use dashd_rpc::blockchain::{BlockReadError, NodeChain as _};

        let dir = tempfile::tempdir().unwrap();
        let p = regtest();
        let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0"]);
        node.stop_net();
        let genesis = genesis_block(&p);
        let main = branch(&p, *genesis.header(), 0, &[1, 1, 1], 0x0e);
        let side = branch(&p, *genesis.header(), 0, &[1], 0x0f);
        submit(&node, &main);
        let files = node.take_test_block_files().expect("step 7");
        let v = node.chain().unwrap();
        let slot = crate::rpc::ChainSlot::default();
        slot.attach(v.handle(), v.signals(), Some(files), None, None);
        let rpc = slot.chainman().expect("attached");
        let p3 = NodeChain::new(v.handle().clone(), None);
        let batch = HeaderBatch {
            headers: vec![*side[0].header()],
            hashes: vec![side[0].hash()],
        };
        assert!(p3.process_headers(batch, 7).expect("served").invalid.is_none());

        let (read, lines) = logged(|| rpc.read_block(&main[0].hash()));
        let (facts, block) = read.expect("stored");
        assert_eq!((block.hash(), facts.height, facts.confirmations), (main[0].hash(), 1, 3));
        assert!(lines.is_empty(), "{lines:?}");
        let unknown = BlockHash::from([0x11; 32]);
        assert_eq!(rpc.read_block(&unknown).err(), Some(BlockReadError::NotFound));

        let not_on_disk = |hash: &BlockHash, line: &str| {
            let (read, lines) = logged(|| rpc.read_block(hash));
            assert_eq!(read.err(), Some(BlockReadError::NotOnDisk));
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(lines[0].0, tracing::Level::ERROR, "{lines:?}");
            assert!(lines[0].1.starts_with(line), "{lines:?}");
            lines[0].1.clone()
        };
        // Core's `GetBlockPos()`: the null position, whole, for an entry without its data.
        let line = not_on_disk(&side[0].hash(), "");
        assert_eq!(
            line,
            "ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=-1, nPos=0)"
        );

        let target = p3.lookup(&main[0].hash()).expect("indexed");
        let pos = p3.status(&p3.snapshot(), target).unwrap().data_pos.expect("HAVE_DATA");
        let blk = dir.path().join("regtest/blocks/blk00000.dat");
        let mut file = std::fs::OpenOptions::new().write(true).open(&blk).unwrap();
        // P3's connected-block cache holds the block; the RPC read never looks there.
        p3.connected(&main[0]);
        let bytes = std::fs::read(&blk).unwrap();
        file.set_len(u64::from(pos.pos) + 10).unwrap();
        not_on_disk(&main[0].hash(), "ReadBlockFromDisk: ");
        assert_eq!(p3.read_block(target).block().map(|b| b.hash()), Some(main[0].hash()));

        // `main[1]`'s record (its size field, then its bytes) where `main[0]`'s was.
        file.set_len(0).unwrap();
        file.write_all(&bytes).unwrap();
        let other = main[1].core_serialize();
        let size = u32::try_from(other.len()).unwrap().to_le_bytes();
        file.seek(SeekFrom::Start(u64::from(pos.pos) - 4)).unwrap();
        file.write_all(&[&size[..], &other].concat()).unwrap();
        not_on_disk(&main[0].hash(), HASH_MISMATCH);
    }
}
