// SPDX-License-Identifier: MIT

//! The index host (IX1 §1, slice e): Core's `BaseIndex` lifecycle around each index's
//! [`SubscriberDriver`], from init's Step 8 to shutdown.
//!
//! - [`IndexHost::register`] takes the indexes in the caller's order (Core's Step 8: txindex,
//!   then the filters, then coinstats). With the wipe option (`-reindex`) it first removes the
//!   index's DB directory and extra paths. It then opens the DB read-only, and the index's reader
//!   serves the retained data, with Core's summary after `Init`: not synced, at the locator's
//!   height.
//! - [`IndexHost::open_all`] opens each driver in registration order on the caller's thread, as
//!   Core's `Start` runs `Init` (`index/base.cpp:503-520`), over its own guarded engine and a data
//!   object built just before, then moves it to its own `index-<name>` thread. Each reader drops
//!   its read-only handle before the index's engine opens, answers as with no DB until the
//!   driver's open, then moves to that engine before the first `sync_step`. The caller calls it after recovery's step 10,
//!   not before a `-reindex-chainstate` import ends; on W's thread, V's or one that steps a
//!   subscriber the hub refuses it.
//! - Each thread runs `sync_step` until `Live` (Core's `ThreadSync`), then `step` (Core's
//!   validation callbacks) until [`IndexHost::stop`]. It never holds a chain lock.
//! - [`IndexHost::interrupt`] ends catch-up (Core's `m_interrupt`). A live index keeps stepping,
//!   as Core's keeps its validation callbacks until `Stop` unregisters it
//!   (`index/base.cpp:522-529`), so the shutdown flush gets its acks. An interrupted catch-up
//!   still answers a flush's rewind gate until the stop.
//! - [`IndexHost::stop`], after the caller's last chainstate flush, joins each thread and runs its
//!   driver's `stop`: the final capped commit, then the deregistration.
//!
//! **Failures.** A thread's error goes to the node's fail-stop sink by the host rule. A
//! [`SubscriberError::Fatal`] latched at this index and nothing has reported it, so the host
//! reports `<name>: <message>`. A [`SubscriberError::Refused`] was reported where it latched, so
//! the host reports nothing. Any other error trips the latch at site `<name>` and is reported only
//! if that trip latched; a lost trip settles and reports nothing, like `Refused`. That is Core's
//! `FatalError`, `AbortNode` (`index/base.cpp:33-36`, `:224-245`, `:390-432`). A failed file
//! sync at a commit stops the node, where Core only logs it (`:270-302`). An error from a
//! driver's open is Core's `InitError` instead: it is returned, and only a `Fatal` one is
//! reported. An index thread's panic trips the latch at site `<name>` by the same rule, before
//! the driver leaves the hub. A panic in a spec's constructors or the driver's open is the
//! caller's own, in [`IndexHost::open_all`], and one in a driver's final `stop` reaches the
//! caller from [`IndexHost::stop`].

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering, fence};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{fs, hint, io, iter};

use dash_chain::{BlockRef, Notify, Parker};
use dash_primitives::BlockHash;
use dashd_chainstate::flush::SubscriberHub;
use dashd_chainstate::store::{AbortSink, Failure, GlobalFailStop, GuardedEngine};
use dashd_chainstate::subscriber::{
    BlockSource, DriverConfig, RowSchema, STEPPING_THREAD, SubStep, SubscriberClass,
    SubscriberData, SubscriberDriver, SubscriberError, SyncProgress, decode_locator,
};
use dashd_store::{ErrorKind, KvRead};
use thiserror::Error;

use crate::{
    Ancestry, BlockFilter, CoinStatsEntry, CoinStatsReader, FilterHash, FilterHeader,
    FilterIndexReader, IndexError, IndexStatus, IndexSummary, IndexedTx, TxIndexReader, Txid,
};

// Core: v24.0.0 src/index/base.cpp:30-31 (SYNC_LOG_INTERVAL, SYNC_LOCATOR_WRITE_INTERVAL)
const SYNC_LOG_INTERVAL: Duration = Duration::from_secs(30);
const SYNC_LOCATOR_WRITE_INTERVAL: Duration = Duration::from_secs(30);
/// The wait after a catch-up step that moved nothing before the retry: SB5's `HoldActive`
/// answer, whose release wakes it sooner, or a handoff the tip moved past, which waits it out.
const IDLE_RETRY: Duration = Duration::from_millis(10);
/// A [`TipCell`] read's spins before it yields to the writer.
const TIP_SPINS: u32 = 64;

/// A spec's store engine.
pub type SpecEngine<S> = <<S as IndexSpec>::Data as SubscriberData>::E;

/// One index as the host runs it: its name and paths, and constructors for its read-only reader,
/// its engine, its data object and its reader over the engine.
pub trait IndexSpec: Send + 'static {
    type Data: SubscriberData + 'static;
    /// The read-only store the pre-open reader reads.
    type ReadOnly: KvRead + Send + Sync + 'static;
    /// The reader, a trait object such as `dyn TxIndexReader`.
    type Reader: ?Sized + Send + Sync + 'static;

    /// Core's `GetName()`.
    fn name(&self) -> &'static str;

    /// The index's DB directory.
    fn db_dir(&self) -> PathBuf;

    /// Further paths `-reindex` removes, such as the filter index's flat files.
    fn wipe_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// The data object's row schema: the host reads its locator before open.
    fn rows(&self) -> &'static RowSchema;

    /// The DB opened read-only, or `None` if it does not exist. The handle must not lock the
    /// directory. Its reader serves until the host opens the index: the host drops it from the
    /// reader before [`engine`](Self::engine), and a read that took it earlier may still finish
    /// on it while the engine opens.
    ///
    /// # Errors
    ///
    /// The DB exists and could not be opened.
    fn open_read_only(&self) -> Result<Option<Arc<Self::ReadOnly>>, IndexError>;

    /// The reader over the read-only DB.
    fn read_only_reader(
        &self,
        db: Arc<Self::ReadOnly>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<Self::Reader>;

    /// The index's own guarded engine on the node's latch, never the chainstate's (c2-fw B2).
    ///
    /// # Errors
    ///
    /// The open failed.
    fn engine(&self) -> Result<Arc<GuardedEngine<SpecEngine<Self>>>, IndexError>;

    /// The data object on `engine`, built once per open (b1 n4: a construction may write).
    ///
    /// # Errors
    ///
    /// The construction failed.
    fn data(&self, engine: &Arc<GuardedEngine<SpecEngine<Self>>>)
    -> Result<Self::Data, IndexError>;

    /// The reader over the driver's engine.
    fn reader(
        &self,
        engine: Arc<GuardedEngine<SpecEngine<Self>>>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<Self::Reader>;

    fn blocks(&self) -> Arc<dyn BlockSource>;

    fn config(&self) -> DriverConfig;
}

/// The host's failures to start an index; each is Core's `InitError` for the caller, except an
/// [`Open`](Self::Open) of a `Fatal` (reported by the host) or a `Refused` (reported where it
/// latched).
#[derive(Debug, Error)]
pub enum HostError {
    /// The spec's read-only open, its locator read, its engine or its data object failed.
    #[error("{name}: {error}")]
    Index {
        name: &'static str,
        error: IndexError,
    },
    /// A `-reindex` wipe failed.
    #[error("{name}: removing {}: {error}", path.display())]
    Wipe {
        name: &'static str,
        path: PathBuf,
        error: io::Error,
    },
    /// The driver's open failed, such as Core's unknown-locator or prune refusal, or the hub's
    /// `WouldDeadlock` refusal of an open during a flush the caller blocks.
    #[error(transparent)]
    Open(SubscriberError),
    /// The index thread could not be spawned.
    #[error("{name}: index thread: {error}")]
    Thread {
        name: &'static str,
        error: io::Error,
    },
}

impl HostError {
    /// The host or the store already reported this to the fail-stop sink.
    pub fn reported(&self) -> bool {
        matches!(self, Self::Open(SubscriberError::Fatal(_) | SubscriberError::Refused(_)))
    }
}

#[derive(Debug, Default)]
struct Progress {
    best: Option<BlockRef>,
    synced: bool,
    consumed: u64,
    /// The thread steps its driver; a waiter waits only on a running one.
    running: bool,
}

/// One index's status, shared by its thread, its reader and the host.
struct IndexState {
    name: &'static str,
    hub: Arc<SubscriberHub>,
    progress: Mutex<Progress>,
    moved: Condvar,
    parker: Arc<Parker>,
    /// [`Progress::consumed`] for readers that never wait. It and [`tip`](Self::tip) are each
    /// published values, not one snapshot: a read of both can straddle a step.
    consumed: AtomicU64,
    /// [`Progress::best`] for readers that never wait.
    tip: TipCell,
    interrupted: AtomicBool,
    stopping: AtomicBool,
}

impl IndexState {
    fn lock(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish<D: SubscriberData>(&self, d: &SubscriberDriver<D>, running: bool) {
        let mut p = self.lock();
        p.best = d.best_block();
        p.synced = d.class() == SubscriberClass::Synced;
        p.consumed = d.consumed_through();
        self.tip.store(p.best);
        self.consumed.store(p.consumed, Ordering::Release);
        p.running = running;
        drop(p);
        self.moved.notify_all();
    }

    fn ended(&self) {
        self.lock().running = false;
        self.moved.notify_all();
    }

    fn interrupt(&self, stop: bool) {
        self.interrupted.store(true, Ordering::Release);
        if stop {
            self.stopping.store(true, Ordering::Release);
        }
        self.parker.notify();
    }
}

impl IndexStatus for IndexState {
    // Core: v24.0.0 src/index/base.cpp:531-537 (GetSummary)
    fn summary(&self) -> IndexSummary {
        let p = self.lock();
        IndexSummary {
            name: self.name,
            synced: p.synced,
            best_block_height: p.best.map_or(0, |b| b.height),
        }
    }

    // Core: v24.0.0 src/index/base.cpp:474-496 (BlockUntilSyncedToCurrentChain)
    fn block_until_synced(&self) -> bool {
        let best = {
            let p = self.lock();
            if !p.synced {
                return false;
            }
            p.best
        };
        let (Some(table), Some(idx)) = (self.hub.table(), self.hub.index()) else {
            return true;
        };
        let (tip, tip_seq) = {
            let snap = table.snapshot();
            (snap.tip(), snap.tip_seq())
        };
        if best.is_some_and(|b| {
            b.height >= tip.height && idx.ancestor(b.id, tip.height) == Some(tip.id)
        }) {
            return true;
        }
        tracing::info!(
            "BlockUntilSyncedToCurrentChain: {} is catching up on block notifications",
            self.name
        );
        // Core's `SyncWithValidationInterfaceQueue`: the feed up to the tip read above.
        let p = self.lock();
        let _p = self
            .moved
            .wait_while(p, |p| p.running && p.consumed < tip_seq)
            .unwrap_or_else(PoisonError::into_inner);
        true
    }
}

/// The thread's last published best block for readers that never wait: a sequence lock with one
/// writer, the index thread. A read retries only across a concurrent write of a few words.
#[derive(Debug)]
struct TipCell {
    seq: AtomicU64,
    /// -1 for none.
    height: AtomicI64,
    hash: [AtomicU64; 4],
}

impl Default for TipCell {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            height: AtomicI64::new(-1),
            hash: Default::default(),
        }
    }
}

impl TipCell {
    fn store(&self, tip: Option<BlockRef>) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.height.store(tip.map_or(-1, |b| i64::from(b.height)), Ordering::Relaxed);
        let bytes = tip.map_or([0; 32], |b| *b.hash.as_bytes());
        for (word, chunk) in self.hash.iter().zip(bytes.as_chunks::<8>().0) {
            word.store(u64::from_le_bytes(*chunk), Ordering::Relaxed);
        }
        self.seq.store(seq + 2, Ordering::Release);
    }

    fn load(&self) -> Option<(i32, BlockHash)> {
        let mut spins = 0u32;
        loop {
            let seq = self.seq.load(Ordering::Acquire);
            if seq.is_multiple_of(2) {
                let height = self.height.load(Ordering::Relaxed);
                let mut bytes = [0; 32];
                for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(&self.hash) {
                    *chunk = word.load(Ordering::Relaxed).to_le_bytes();
                }
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == seq {
                    let height = i32::try_from(height).ok().filter(|&h| h >= 0)?;
                    return Some((height, BlockHash::from(bytes)));
                }
            }
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(TIP_SPINS) {
                thread::yield_now();
            } else {
                hint::spin_loop();
            }
        }
    }
}

/// An index's reader as the host serves it: the read-only one before open, none from the index's
/// engine open to the driver's, and the one over the driver's engine after. With none, every
/// lookup misses, as with no DB.
pub struct IndexReader<R: ?Sized> {
    state: Arc<IndexState>,
    slot: RwLock<Option<Arc<R>>>,
}

impl<R: ?Sized> IndexReader<R> {
    /// The feed seq this index has consumed through; may lag the tip. It is the value
    /// [`block_until_synced`](IndexStatus::block_until_synced) waits on, published by the index
    /// thread after each step, and the read never waits. 0 before the index is live. Read it
    /// before [`consumed_tip`](Self::consumed_tip): that tip is then the one published with this
    /// seq or a newer one.
    pub fn consumed_through(&self) -> u64 {
        self.state.consumed.load(Ordering::Acquire)
    }

    /// The height and hash of the index's `best_block()` as its thread last published it after a
    /// step, or `None` before its first block. It may lag the tip; after a reorg it can name a
    /// block on the old branch at the tip's height, so callers compare the hash. The read never
    /// waits. Read after [`consumed_through`](Self::consumed_through), it is at least as new as
    /// that seq.
    pub fn consumed_tip(&self) -> Option<(i32, BlockHash)> {
        self.state.tip.load()
    }

    /// The reader serving now.
    pub fn current(&self) -> Option<Arc<R>> {
        self.slot.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set(&self, reader: Option<Arc<R>>) {
        let old = std::mem::replace(
            &mut *self.slot.write().unwrap_or_else(PoisonError::into_inner),
            reader,
        );
        drop(old);
    }
}

impl<R: ?Sized + Send + Sync> IndexStatus for IndexReader<R> {
    fn summary(&self) -> IndexSummary {
        self.state.summary()
    }

    fn block_until_synced(&self) -> bool {
        self.state.block_until_synced()
    }
}

impl TxIndexReader for IndexReader<dyn TxIndexReader> {
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        self.current().map_or(Ok(None), |r| r.find_tx(txid))
    }

    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError> {
        self.current().map_or(Ok(false), |r| r.has_tx(txid))
    }
}

impl FilterIndexReader for IndexReader<dyn FilterIndexReader> {
    fn filter(&self, block: &BlockRef) -> Result<Option<BlockFilter>, IndexError> {
        self.current().map_or(Ok(None), |r| r.filter(block))
    }

    fn filter_header(&self, block: &BlockRef) -> Result<Option<FilterHeader>, IndexError> {
        self.current().map_or(Ok(None), |r| r.filter_header(block))
    }

    fn filter_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<BlockFilter>>, IndexError> {
        self.current().map_or(Ok(None), |r| r.filter_range(start, stop, ancestry))
    }

    fn filter_hash_range(
        &self,
        start: u32,
        stop: &BlockRef,
        ancestry: &dyn Ancestry,
    ) -> Result<Option<Vec<FilterHash>>, IndexError> {
        self.current().map_or(Ok(None), |r| r.filter_hash_range(start, stop, ancestry))
    }
}

impl CoinStatsReader for IndexReader<dyn CoinStatsReader> {
    fn look_up_stats(&self, block: &BlockRef) -> Result<Option<CoinStatsEntry>, IndexError> {
        self.current().map_or(Ok(None), |r| r.look_up_stats(block))
    }
}

/// A stopped thread's driver, to stop after the joins.
type Stopper = Box<dyn FnOnce() + Send>;

type Thread = JoinHandle<Option<Stopper>>;

/// A registered index before `open_all`: opens its driver and spawns its thread, with the
/// spec's types erased.
type Start = Box<dyn FnOnce(Arc<dyn AbortSink>) -> Result<Thread, HostError> + Send>;

struct Entry {
    state: Arc<IndexState>,
    start: Option<Start>,
    thread: Option<Thread>,
    /// Empties the reader's slot at the stop, closing the index's database.
    clear: Box<dyn Fn() + Send>,
}

/// The node's indexes: registration, pre-open reads, open, threads, interrupt and stop.
pub struct IndexHost {
    hub: Arc<SubscriberHub>,
    sink: Arc<dyn AbortSink>,
    wipe: bool,
    entries: Vec<Entry>,
}

impl IndexHost {
    /// The host over the node's subscriber hub and fail-stop sink. `wipe` is `-reindex`:
    /// [`register`](Self::register) removes each index's data before anything opens it.
    pub fn new(hub: Arc<SubscriberHub>, sink: Arc<dyn AbortSink>, wipe: bool) -> Self {
        Self {
            hub,
            sink,
            wipe,
            entries: Vec::new(),
        }
    }

    /// Registers an index after those registered before it. It wipes the index's data if the
    /// host wipes, opens its DB read-only, and reads its locator for the pre-open summary.
    ///
    /// # Errors
    ///
    /// The wipe, the read-only open or the locator read failed.
    ///
    /// # Panics
    ///
    /// On a hub with no feed table.
    // Core: v24.0.0 src/init.cpp:2273-2318 (Step 8), src/index/base.cpp:94-166 (Init),
    // :531-537 (GetSummary)
    pub fn register<S: IndexSpec>(
        &mut self,
        spec: S,
    ) -> Result<Arc<IndexReader<S::Reader>>, HostError> {
        let name = spec.name();
        if self.wipe {
            // Core: v24.0.0 src/init.cpp:2278, :2306, :2313 (f_wipe = fReindex)
            for path in iter::once(spec.db_dir()).chain(spec.wipe_paths()) {
                wipe(name, &path)?;
            }
        }
        let index_err = |error| HostError::Index {
            name,
            error,
        };
        let state = Arc::new(IndexState {
            name,
            hub: Arc::clone(&self.hub),
            progress: Mutex::default(),
            moved: Condvar::new(),
            parker: Parker::new(),
            consumed: AtomicU64::new(0),
            tip: TipCell::default(),
            interrupted: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        });
        let reader = match spec.open_read_only().map_err(index_err)? {
            None => None,
            Some(db) => {
                let (cf, key) = spec.rows().locator;
                let locator = db
                    .get(cf, key)
                    .map_err(|e| index_err(IndexError::Io(format!("locator read: {e}"))))?;
                state.lock().best = locator.as_deref().and_then(decode_locator).and_then(|hash| {
                    let idx = self.hub.index().expect("a hub over a feed table");
                    let id = idx.lookup(&hash)?;
                    Some(BlockRef {
                        id,
                        hash,
                        height: idx.height(id)?,
                    })
                });
                Some(spec.read_only_reader(db, Arc::clone(&state) as Arc<dyn IndexStatus>))
            }
        };
        let reader = Arc::new(IndexReader {
            state: Arc::clone(&state),
            slot: RwLock::new(reader),
        });
        let start: Start = {
            let reader = Arc::clone(&reader);
            Box::new(move |sink| open(spec, &reader, sink))
        };
        let clear = {
            let reader = Arc::clone(&reader);
            Box::new(move || reader.set(None))
        };
        self.entries.push(Entry {
            state,
            start: Some(start),
            thread: None,
            clear,
        });
        Ok(reader)
    }

    /// Opens every registered index not yet open, in registration order: each driver's open on
    /// this thread, then its `index-<name>` thread with the opened driver.
    ///
    /// # Errors
    ///
    /// The first failure; the indexes opened before it keep running until [`stop`](Self::stop),
    /// and the failed one's reader answers as with no DB.
    /// On a thread that steps a subscriber it opens nothing and returns Core's `InitError` with
    /// [`STEPPING_THREAD`] (D39, D47). On W's thread during a flush, or on V's while the flush
    /// awaits V's clears, the hub refuses the open with `WouldDeadlock` (D43), returned as
    /// [`HostError::Open`]. Until an opened index's thread takes its first step the hub records
    /// this thread as that index's owner, so a flush awaiting that index in the gap refuses the
    /// next index's open the same way: a refusal, not a hang.
    ///
    /// # Panics
    ///
    /// A spec's `engine()`, `data()` or `reader()`, or the driver's open, panicked: they run on
    /// this thread.
    // Core: v24.0.0 src/index/base.cpp:498-520 (Start)
    pub fn open_all(&mut self) -> Result<(), HostError> {
        let Some(first) = self.entries.iter().find(|e| e.start.is_some()) else {
            return Ok(());
        };
        if self.hub.steps_on_this_thread() {
            let name = first.state.name;
            return Err(HostError::Open(SubscriberError::Data(format!("{name}{STEPPING_THREAD}"))));
        }
        for e in &mut self.entries {
            if let Some(start) = e.start.take() {
                e.thread = Some(start(Arc::clone(&self.sink))?);
            }
        }
        Ok(())
    }

    /// Each index's summary, in registration order (`getindexinfo`'s rows).
    pub fn summaries(&self) -> Vec<IndexSummary> {
        self.entries.iter().map(|e| e.state.summary()).collect()
    }

    /// Ends every catch-up at its next check; live indexes step on until [`stop`](Self::stop).
    // Core: v24.0.0 src/init.cpp:296-310 (Interrupt), src/index/base.cpp:498-501
    pub fn interrupt(&self) {
        self.entries.iter().for_each(|e| e.state.interrupt(false));
    }

    /// After the caller's last chainstate flush: joins each thread, then stops its driver (the
    /// final capped commit, then the deregistration) and empties its reader, which closes its
    /// database, in registration order. Dropping the host does the same.
    ///
    /// # Panics
    ///
    /// With the first index thread's panic, after every index has stopped.
    // Core: v24.0.0 src/init.cpp:421-441 (Shutdown), src/index/base.cpp:522-529 (Stop)
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for IndexHost {
    // Core: v24.0.0 src/index/base.cpp:88-92 (~BaseIndex)
    fn drop(&mut self) {
        self.entries.iter().for_each(|e| e.state.interrupt(true));
        let mut panicked = None;
        for e in &mut self.entries {
            if let Err(p) = join(e) {
                panicked.get_or_insert(p);
            }
            // Core: v24.0.0 src/init.cpp:423 (g_txindex.reset())
            (e.clear)();
        }
        if let Some(p) = panicked
            && !thread::panicking()
        {
            panic::resume_unwind(p);
        }
    }
}

/// Joins an entry's thread and stops its driver, returning a panic of either.
fn join(e: &mut Entry) -> Result<(), Box<dyn Any + Send>> {
    match e.thread.take().map(JoinHandle::join) {
        Some(Ok(Some(stop))) => panic::catch_unwind(AssertUnwindSafe(stop)),
        Some(Err(p)) => Err(p),
        Some(Ok(None)) | None => Ok(()),
    }
}

// Core: v24.0.0 src/dbwrapper.cpp:144-148 (CDBWrapper's fWipe)
fn wipe(name: &'static str, path: &Path) -> Result<(), HostError> {
    let removed = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
        Ok(m) => {
            tracing::info!("Wiping {name} index data in {}", path.display());
            if m.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
        }
    };
    removed.map_err(|error| HostError::Wipe {
        name,
        path: path.to_path_buf(),
        error,
    })
}

/// The host rule for a thread's error (see the module docs).
// Core: v24.0.0 src/index/base.cpp:33-36 (FatalErrorImpl), :224-245 and :390-432 (FatalError)
fn report(name: &'static str, e: &SubscriberError, latch: &GlobalFailStop, sink: &dyn AbortSink) {
    match e {
        SubscriberError::Fatal(f) => sink.abort_node(&format!("{name}: {}", f.0.message), None),
        SubscriberError::Refused(_) => {}
        e => {
            let message = format!("{name}: {e}");
            let failure = Failure {
                kind: ErrorKind::Other,
                site: name,
                message: message.clone(),
            };
            if latch.trip(failure) {
                sink.abort_node(&message, None);
            } else {
                latch.settle();
            }
        }
    }
}

/// An index thread's end, however it ends: waiters stop waiting on the thread.
struct Exit<'a>(&'a IndexState);

impl Drop for Exit<'_> {
    fn drop(&mut self) {
        self.0.ended();
    }
}

/// A panic after the driver's open, reported by the host rule while the driver is still in the
/// hub: dropped before it, so the latch trips before the prune lock goes (D33).
struct PanicReport<'a> {
    name: &'static str,
    latch: &'a GlobalFailStop,
    sink: &'a dyn AbortSink,
}

impl Drop for PanicReport<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            let e = SubscriberError::Data("the index thread panicked".to_owned());
            report(self.name, &e, self.latch, self.sink);
        }
    }
}

/// An index's open on the caller's thread: its engine and data object, the driver's open, the
/// reader's move, then its thread with the opened driver.
// Core: v24.0.0 src/index/base.cpp:503-520 (Start)
fn open<S: IndexSpec>(
    spec: S,
    reader: &IndexReader<S::Reader>,
    sink: Arc<dyn AbortSink>,
) -> Result<Thread, HostError> {
    let state = Arc::clone(&reader.state);
    let name = state.name;
    let index_err = move |error| HostError::Index {
        name,
        error,
    };
    // e1 r1 m2: no new read reaches the read-only handle once the engine may write.
    reader.set(None);
    let engine = spec.engine().map_err(index_err)?;
    let data = spec.data(&engine).map_err(index_err)?;
    let latch = engine.latch().clone();
    let open = SubscriberDriver::open(
        data,
        Arc::clone(&engine),
        Arc::clone(&state.hub),
        spec.blocks(),
        spec.config(),
    );
    let mut driver = open.map_err(|e| {
        if matches!(e, SubscriberError::Fatal(_)) {
            report(name, &e, &latch, &*sink);
        }
        HostError::Open(e)
    })?;
    reader.set(Some(spec.reader(engine, Arc::clone(&state) as Arc<dyn IndexStatus>)));
    driver.set_waker(Notify::Thread(Arc::clone(&state.parker)));
    state.publish(&driver, true);
    let ours = Arc::clone(&state);
    thread::Builder::new()
        .name(format!("index-{name}"))
        .spawn(move || run(driver, &ours, latch, sink))
        .map_err(|error| {
            state.ended();
            HostError::Thread {
                name,
                error,
            }
        })
}

/// An index thread: catch-up, the live steps, and the driver back to the host at the stop, or
/// `None` after a failure.
fn run<D: SubscriberData + 'static>(
    driver: SubscriberDriver<D>,
    state: &IndexState,
    latch: GlobalFailStop,
    sink: Arc<dyn AbortSink>,
) -> Option<Stopper> {
    let name = state.name;
    let _exit = Exit(state);
    let mut driver = driver;
    let panicked = PanicReport {
        name,
        latch: &latch,
        sink: &*sink,
    };
    let ran = catch_up(&mut driver, state).and_then(|live| {
        if live {
            follow(&mut driver, state)
        } else {
            await_stop(&mut driver, state)
        }
    });
    if let Err(e) = ran {
        report(name, &e, &latch, &*sink);
        return None;
    }
    state.publish(&driver, false);
    drop(panicked);
    Some(Box::new(move || {
        if let Err(e) = driver.stop() {
            report(name, &e, &latch, &*sink);
        }
    }))
}

/// Core's `ThreadSync` catch-up: true once live, false when interrupted.
// Core: v24.0.0 src/index/base.cpp:189-268 (ThreadSync)
fn catch_up<D: SubscriberData>(
    d: &mut SubscriberDriver<D>,
    state: &IndexState,
) -> Result<bool, SubscriberError> {
    let name = state.name;
    let mut last_log = Instant::now();
    let mut last_locator = last_log;
    loop {
        if state.interrupted.load(Ordering::Acquire) {
            return Ok(false);
        }
        let before = (d.best_block(), d.class());
        let progress = d.sync_step()?;
        state.publish(d, true);
        match progress {
            SyncProgress::Live => {
                match d.best_block() {
                    Some(b) => tracing::info!("{name} is enabled at height {}", b.height),
                    None => tracing::info!("{name} is enabled"),
                }
                return Ok(true);
            }
            SyncProgress::Blocked => state.parker.park(),
            SyncProgress::Progress => {
                if (d.best_block(), d.class()) == before {
                    state.parker.park_timeout(IDLE_RETRY);
                }
                let now = Instant::now();
                if now.duration_since(last_log) >= SYNC_LOG_INTERVAL {
                    if let Some(b) = d.best_block() {
                        tracing::info!("Syncing {name} with block chain from height {}", b.height);
                    }
                    last_log = now;
                }
                if now.duration_since(last_locator) >= SYNC_LOCATOR_WRITE_INTERVAL {
                    d.tick()?;
                    last_locator = now;
                }
            }
        }
    }
}

/// A live index's steps (Core's validation callbacks) until the stop.
fn follow<D: SubscriberData>(
    d: &mut SubscriberDriver<D>,
    state: &IndexState,
) -> Result<(), SubscriberError> {
    while !state.stopping.load(Ordering::Acquire) {
        let step = d.step()?;
        state.publish(d, true);
        if step != SubStep::Progress {
            state.parker.park();
        }
    }
    Ok(())
}

/// An interrupted catch-up connects nothing more, but answers a flush's rewind gate until the
/// stop, so a shutdown flush does not wait on it.
fn await_stop<D: SubscriberData>(
    d: &mut SubscriberDriver<D>,
    state: &IndexState,
) -> Result<(), SubscriberError> {
    while !state.stopping.load(Ordering::Acquire) {
        if state.hub.outstanding(d.id()).rewind.is_some() {
            d.sync_step()?;
            state.publish(d, true);
        } else {
            state.parker.park();
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "crash"))]
mod tests;
