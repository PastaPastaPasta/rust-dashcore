//! `ChainView` (§6): SB4's snapshot plus the node fields a reader needs coherent with its tip,
//! stored by V after every snapshot store, unthrottled (Q5); [`ChainViewHandle`], the read side
//! other components hold (§6.3); and [`TipSeqFence`], which keeps F1-derived state behind the
//! published tip (§6.2).
//!
//! `mn_list` and `ConnChainView::mn_list_at_tip` come with CH7a (D40).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dash_chain::{
    BlockId, ChainReadSnapshot, ChainReader, EntryView, HeaderBatch, HeadersResult, PeerId,
    SnapshotExpired, Status, Work,
};
use dash_clock::NodeTime;
use dash_consensus::BlsCtx;
use dash_primitives::BlockHash;

use super::actor::ChainHandle;
use super::command::ChainQuery;
use super::locks::{ChainLockSig, ClSigs};
use super::reply::{ActorReply, WaitError, no_wait_marked};
use super::seams::SyncState;

/// Core's `LoadingBlocks()` inputs: `fImporting` and `fReindex`.
#[derive(Debug, Default)]
pub struct LoadingFlags {
    importing: AtomicBool,
    reindex: AtomicBool,
}

impl LoadingFlags {
    /// `LoadingBlocks()`: `fImporting || fReindex`.
    pub fn any(&self) -> bool {
        self.importing() || self.reindex()
    }

    pub fn importing(&self) -> bool {
        self.importing.load(Ordering::Acquire)
    }

    pub fn reindex(&self) -> bool {
        self.reindex.load(Ordering::Acquire)
    }

    /// Core `GetSynchronizationState(init)` (`validation.cpp:3684-3689`).
    pub(crate) fn sync_state(&self, ibd: bool) -> SyncState {
        if !ibd {
            SyncState::PostInit
        } else if self.reindex() {
            SyncState::InitReindex
        } else {
            SyncState::InitDownload
        }
    }

    pub(crate) fn set(&self, importing: Option<bool>, reindex: Option<bool>) {
        if let Some(v) = importing {
            self.importing.store(v, Ordering::Release);
        }
        if let Some(v) = reindex {
            self.reindex.store(v, Ordering::Release);
        }
    }
}

/// What V publishes beside SB4's snapshot (§6.1).
#[derive(Clone, Debug)]
pub struct ChainView {
    /// SB4's snapshot this view wraps: tip facts, `tip_seq`, the active chain, BLS, the floor.
    pub snap: Arc<ChainReadSnapshot>,
    /// The ChainLock signatures (§5.5); `snap.cl_floor` is their block.
    pub cl: Arc<ClSigs>,
    /// `GetMedianTimePast()` of `snap`'s tip; 0 before the node has a tip.
    pub mtp: i64,
    /// `nChainTx(tip)`: `GuessVerificationProgress`, `getblockchaininfo`.
    pub tip_chain_tx: u32,
    /// `getblockchaininfo.warnings`' large-work invalid chain (§4.3).
    pub large_work_invalid_chain: bool,
    /// Bumps on every store.
    pub view_seq: u64,
}

impl ChainView {
    pub fn bls(&self) -> &BlsCtx {
        &self.snap.bls
    }

    pub fn epoch(&self) -> u64 {
        self.snap.epoch
    }

    /// `GetBestChainLock()`: `None` is Core's null sig.
    pub fn best_chainlock(&self) -> Option<&Arc<ChainLockSig>> {
        self.cl.best.as_ref()
    }

    /// Core `IsInitialBlockDownload()` against this view's snapshot and the shared latch (R4): a
    /// method, so `setmocktime` takes effect at the next call (E-A1-2).
    pub fn ibd(&self, reader: &ChainReader, loading: &LoadingFlags, now: NodeTime) -> bool {
        reader.is_initial_block_download(&self.snap, now, loading.any())
    }
}

/// The read side (§6.3): SB4's snapshot, index and latch, lock-free; the published
/// [`ChainView`]; `Query` for cross-entry reads; and `ProcessHeaders`, the one command a reader
/// awaits. P2a-node's `ChainAccess` and `ConnChainView` forward to it method for method.
#[derive(Clone)]
pub struct ChainViewHandle {
    pub(super) chain: ChainHandle,
}

impl ChainViewHandle {
    /// SB4's last published snapshot (R8). It may be one store ahead of [`Self::view`]'s;
    /// `epoch` tells them apart (§6.2).
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot> {
        self.reader().snapshot()
    }

    /// The last published [`ChainView`]: node fields coherent with its `snap`.
    pub fn view(&self) -> Arc<ChainView> {
        self.chain.view()
    }

    /// The published ChainLock signatures: `ClReader` (P2b's `getdata` and `inv`, RPC, the
    /// miner; §6.3). LK1a's precheck uses `ClIntake`'s reply instead (§5.1).
    pub fn cl(&self) -> Arc<ClSigs> {
        self.view().cl.clone()
    }

    pub fn reader(&self) -> &ChainReader {
        self.chain.reader()
    }

    /// Core `LookupBlockIndex` (R1).
    pub fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.reader().lookup(hash)
    }

    /// An entry's immutable fields and one seqlocked status read (R2): the live status, which
    /// can be ahead of any published view's (D63). For a status bound to a view, use
    /// [`Self::status`] or [`Self::valid_scripts`].
    pub fn entry(&self, id: BlockId) -> Option<EntryView> {
        self.reader().entry(id)
    }

    /// `id`'s status as `view` saw it, Core's under the `cs_main` section that gave its tip
    /// (D63): `ChainAccess::status`. `None` if `id` is not in the index. Expired for a view more
    /// than `RETAIN_AT_MOST` status writes old (BL-224, D65, D68): take [`Self::view`] again, and
    /// redo the whole decision on it, or only the expired read where the decision is shown
    /// single-view-equivalent, as its owner's notes record per site (DEC-292; P3-notes D120).
    /// That view never expires: V's view is always the last build, as V publishes it after every
    /// publication with no status write between (asserted after every command).
    pub fn status(&self, view: &ChainView, id: BlockId) -> Result<Option<Status>, SnapshotExpired> {
        self.reader().status_at(&view.snap, id)
    }

    /// `IsValid(BLOCK_VALID_SCRIPTS)` as `view` saw it (D63): `ChainAccess::valid_scripts`.
    /// Expired as [`Self::status`] is.
    pub fn valid_scripts(&self, view: &ChainView, id: BlockId) -> Result<bool, SnapshotExpired> {
        Ok(self.status(view, id)?.is_some_and(|s| s.is_valid(Status::VALID_SCRIPTS)))
    }

    /// `ActiveChain().Contains(id)` as `view` saw it (BL-236): `ChainAccess::in_active_chain`.
    pub fn contains(&self, view: &ChainView, id: BlockId) -> bool {
        view.snap.active.contains(id, self.reader())
    }

    /// `ActiveChain()[height]` as `view` saw it (BL-236): `ChainAccess::active_at`.
    pub fn at(&self, view: &ChainView, height: u32) -> Option<BlockId> {
        view.snap.active.at(height)
    }

    /// Core `GetAncestor`.
    pub fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.reader().ancestor(id, height)
    }

    /// Core `GetLocator`; `None` is v24's empty locator (DEC-82).
    pub fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash> {
        self.reader().locator(id)
    }

    /// `nMinimumChainWork`.
    pub fn min_chain_work(&self) -> Work {
        self.reader().min_chain_work()
    }

    /// Core `LoadingBlocks()`.
    pub fn loading_blocks(&self) -> bool {
        self.chain.loading().any()
    }

    /// Core's latched `IsInitialBlockDownload()` against `snap` and `now` (R4).
    pub fn is_initial_block_download(
        &self,
        snap: &ChainReadSnapshot,
        now: impl Into<NodeTime>,
    ) -> bool {
        self.reader().is_initial_block_download(snap, now, self.loading_blocks())
    }

    /// Core's latched `IsInitialBlockDownload()` for a tip's chain work and time and the node
    /// clock now: a V query turn's, for V's own tip (`getblockchaininfo`).
    pub fn ibd_at(&self, tip: Option<(Work, u32)>) -> bool {
        self.reader().is_initial_block_download_at(
            tip,
            self.chain.node_time(),
            self.loading_blocks(),
        )
    }

    /// `ConnChainView::ibd_service_cache`: the latch against the published view and the node
    /// clock now.
    pub fn ibd_service_cache(&self) -> bool {
        self.is_initial_block_download(&self.view().snap, self.chain.node_time())
    }

    /// `ChainCommand::ProcessHeaders`, blocking until V replies (P2a §3.1.5); the reply's
    /// snapshot is published before it (R3). A marked task is refused before the send, so V
    /// never processes a batch whose result nobody can read (LO-9).
    pub fn process_headers(
        &self,
        batch: HeaderBatch,
        from: Option<PeerId>,
    ) -> Result<HeadersResult, WaitError> {
        if no_wait_marked() {
            return Err(WaitError::WaitOnVFromNoWaitTask);
        }
        self.chain.process_headers(batch, from).recv()
    }

    /// A class R read on V, for cross-entry consistency (`getchaintips`, confirmations;
    /// A1 §2.10.2).
    pub fn query<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ChainQuery<'_>) -> T + Send + 'static,
    ) -> ActorReply<T> {
        self.chain.query(f)
    }

    /// The fence for F1-derived state (§6.2): the published view's `tip_seq`. The view is
    /// stored after SB4's snapshot, so state served at or below it runs ahead of neither.
    pub fn fence_seq(&self) -> u64 {
        self.view().snap.tip_seq
    }
}

/// State derived from F1 events, served behind the `tip_seq` fence (§6.2, OH-M1): F1 fans out
/// per step inside a hold, before any snapshot names the step's tip, so an owner records its
/// state after each event and serves the newest state whose event the published view covers
/// ([`ChainViewHandle::fence_seq`]). It never runs ahead of `getblockcount`. An owner that folds
/// several events into one state records it once, at the last event's `seq`.
#[derive(Debug)]
pub struct TipSeqFence<T> {
    served: T,
    /// Recorded states the fence has not reached, by ascending `seq`: at most one hold's steps.
    pending: VecDeque<(u64, T)>,
    /// The last `seq` recorded, or the subscription's.
    last: u64,
}

impl<T> TipSeqFence<T> {
    /// `initial` is the state at the subscription's `tip_seq`, which the fence must already
    /// cover: take both from the published view, or check `seq <= fence_seq()`.
    pub fn new(seq: u64, initial: T) -> TipSeqFence<T> {
        TipSeqFence {
            served: initial,
            pending: VecDeque::new(),
            last: seq,
        }
    }

    /// Records the state after the event `seq`, and drops what `fence` covers, so the queue
    /// stays bounded however rarely the state is served. Seqs ascend within a run (SB4 restarts
    /// `seq` only at `LoadChainTip`, before any event).
    pub fn record(&mut self, seq: u64, state: T, fence: u64) {
        debug_assert!(seq > self.last, "seqs ascend: {seq} after {}", self.last);
        self.last = seq;
        self.pending.push_back((seq, state));
        self.serve(fence);
    }

    /// The states the fence has not reached.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The newest state whose event `seq <= fence`.
    pub fn serve(&mut self, fence: u64) -> &T {
        let covered = self.pending.partition_point(|&(seq, _)| seq <= fence);
        if let Some((_, state)) = self.pending.drain(..covered).next_back() {
            self.served = state;
        }
        &self.served
    }
}

/// Core `g_best_block` and `g_best_block_cv` (`validation.cpp:3212-3216`), which
/// `getblocktemplate`'s long-poll waits on. V sets it once per hold, after publication (N1,
/// §4.11).
#[derive(Debug, Default)]
pub struct TipWatch {
    best: Mutex<Option<BlockHash>>,
    changed: Condvar,
}

impl TipWatch {
    /// The last tip set; `None` before the first.
    pub fn get(&self) -> Option<BlockHash> {
        *self.best.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn set(&self, tip: BlockHash) {
        *self.best.lock().unwrap_or_else(|p| p.into_inner()) = Some(tip);
        self.changed.notify_all();
    }

    /// Waits until the tip is not `seen` or `timeout` passes, then returns the tip. It waits on
    /// V, so a marked task is refused (LO-9).
    pub fn wait_changed(
        &self,
        seen: Option<BlockHash>,
        timeout: Duration,
    ) -> Result<Option<BlockHash>, WaitError> {
        if no_wait_marked() {
            return Err(WaitError::WaitOnVFromNoWaitTask);
        }
        let deadline = Instant::now() + timeout;
        let mut best = self.best.lock().unwrap_or_else(|p| p.into_inner());
        while *best == seen {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            best = self.changed.wait_timeout(best, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        Ok(*best)
    }
}
