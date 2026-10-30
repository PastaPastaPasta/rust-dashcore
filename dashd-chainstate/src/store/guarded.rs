//! The guarded engine and block files (CH2 §2.4.4, §2.10), over `dash_kv`'s fail-stop latch.
//!
//! Every chainstate read goes through [`GuardedEngine`] or one of its [`GuardedSnapshot`]s, both
//! [`GuardedRead`]. An engine error, or a stored coin that does not decode
//! ([`GuardedEngine::corrupt`], DEC-78), first trips the [`GlobalFailStop`], then calls
//! [`AbortSink::read_failed`], and only then returns a [`CoinsReadError`], which nothing else can
//! construct. That is Core's `CCoinsViewErrorCatcher` (`coins.cpp:355-379`, v23.1.8 :355-369):
//! log, then `std::abort()` before any verdict is formed. A write or barrier checks the latch first
//! and is refused once it has tripped; one that fails trips it. As a [`KvEngine`] the guarded
//! engine keeps those rules, with its reads' errors as plain [`StoreError`]s.
//!
//! The chainstate's load is the exception ([`GuardedEngine::init_reads`]): there a failed read
//! trips the latch and returns its error without the abort, as Core's `LoadChainstate` reads the
//! database directly and turns the exception into its init error. The block index load's scan
//! ([`GuardedEngine::init_index_scan`], inside that region) is the one read outside the guard: it
//! reads the engine itself and trips nothing, as Core's index lives in a database of its own.
//!
//! The latch ([`GlobalFailStop`], [`Failure`], `Parker`) and the [`AbortSink`] are `dash_kv`'s
//! (SB10, BL-109), shared with the SPV store, and re-exported from [`crate::store`]. Every wait in
//! W, V and the drivers registers a `Parker` with the latch before it parks, and a trip wakes them
//! all (E-b1.7).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dash_kv::{AbortSink, Failure, GlobalFailStop};
use dashd_store::flatfile::{BlockFiles, FlatFileError, FlatFs, FlatResult};
use dashd_store::{Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, StoreError};

use crate::alloc_tag;
use crate::planted::planted;

/// A coins read that failed: an engine error, or a stored coin that does not decode (kind
/// [`ErrorKind::Corruption`]). Only [`GuardedEngine`] constructs one, after tripping the latch, so a
/// caller holding one knows every later write is refused. It is never a missing coin and never a
/// validation result (CH2 §2.4.4 rule 1).
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct CoinsReadError(StoreError);

impl CoinsReadError {
    pub fn kind(&self) -> ErrorKind {
        self.0.kind().unwrap_or(ErrorKind::Other)
    }

    pub fn store_error(&self) -> &StoreError {
        &self.0
    }

    /// The engine's error, for a caller that reports plain [`StoreError`]s ([`KvRead`]).
    pub fn into_store_error(self) -> StoreError {
        self.0
    }
}

/// A read through the guarded engine or one of its snapshots (CH2 §2.6, O-R2-m9): an engine error
/// trips the latch and reaches the abort sink before the caller sees it. [`CoinsTable`] and
/// [`Meta`] read only through it.
///
/// [`CoinsTable`]: crate::coins::CoinsTable
/// [`Meta`]: super::Meta
pub trait GuardedRead {
    type E: KvEngine;

    /// The engine whose latch and abort sink the reads report to.
    fn engine(&self) -> &GuardedEngine<Self::E>;

    /// A point read.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] with the engine's error.
    fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, CoinsReadError>;

    /// Point reads in the order of `keys` (RocksDB's `MultiGet`): the prefetch path (CH4).
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] with the engine's error.
    fn multi_read(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, CoinsReadError>;
}

/// An engine whose failures trip the node-wide latch.
pub struct GuardedEngine<E> {
    inner: E,
    latch: GlobalFailStop,
    abort: Arc<dyn AbortSink>,
    /// The writes committed so far: [`GuardedEngine::write_epoch`].
    writes: AtomicU64,
    /// Inside [`GuardedEngine::init_reads`]'s region.
    init: AtomicBool,
    /// The flush's, recovery's and this guard's planted bug (CH2 §6.4).
    #[cfg(feature = "planted-bugs")]
    planted: Option<crate::Planted>,
}

/// [`GuardedEngine::init_reads`]'s region; it ends when this is dropped.
#[must_use = "the region ends when this is dropped"]
pub(crate) struct InitReads<'a> {
    init: &'a AtomicBool,
}

impl Drop for InitReads<'_> {
    fn drop(&mut self) {
        self.init.store(false, Ordering::Release);
    }
}

impl<E: KvEngine> GuardedEngine<E> {
    pub fn new(inner: E, latch: GlobalFailStop, abort: Arc<dyn AbortSink>) -> Self {
        Self {
            inner,
            latch,
            abort,
            writes: AtomicU64::new(0),
            init: AtomicBool::new(false),
            #[cfg(feature = "planted-bugs")]
            planted: None,
        }
    }

    /// Arms planted bug `bug` in this guard and in the flush and recovery that run over it.
    #[cfg(feature = "planted-bugs")]
    pub fn plant(&mut self, bug: crate::Planted) {
        self.planted = Some(bug);
    }

    #[cfg(feature = "planted-bugs")]
    pub(crate) fn planted(&self) -> Option<crate::Planted> {
        self.planted
    }

    // Core: node/chainstate.cpp:239-251 (ReplayBlocks reads `CoinsDB()`, not the error catcher);
    // init.cpp:2035-2037
    /// The chainstate's load (CH2 §4.3, ST3a §4.4), until the region is dropped: a failed read
    /// trips the latch and returns its error, and [`AbortSink::read_failed`] is not called. The
    /// load ends with Core's init error for it, "Error opening block database".
    pub(crate) fn init_reads(&self) -> InitReads<'_> {
        let was = self.init.swap(true, Ordering::AcqRel);
        debug_assert!(!was, "nested init regions");
        InitReads {
            init: &self.init,
        }
    }

    /// The number of writes committed so far. A snapshot taken after reading `n` holds every one
    /// of those writes: a prefetch producer stamps it on its batch, and the fence checks it
    /// against the epoch at the last release (CH2 §2.4.3 item 1; DEC-142, O5).
    pub fn write_epoch(&self) -> u64 {
        self.writes.load(Ordering::Acquire)
    }

    pub fn latch(&self) -> &GlobalFailStop {
        &self.latch
    }

    /// Where the engine's fatal failures go: the flush drivers report a failed step to it too.
    pub fn abort_sink(&self) -> &dyn AbortSink {
        &*self.abort
    }

    /// The engine itself, bypassing the latch: the tests' probe, and an engine's diagnostics
    /// (`EngineTuning::memory_report`) and idle wait (`RocksTuning::wait_idle`). Nothing writes
    /// through it.
    #[cfg(any(test, feature = "model", feature = "rocksdb"))]
    pub(crate) fn inner(&self) -> &E {
        &self.inner
    }

    /// The `block_index` records for the block index load in this engine's init `region`, and
    /// for nothing else: an uncached scan of the engine outside the latch, which only
    /// [`Opened::block_index`] reads (ND1 2b-D20). Core reads `blocks/index` with
    /// `fill_cache = false` (`dbwrapper.cpp:136`), and a failed read there is the load's init
    /// error, which its caller reports: through the latch it would be this engine's fail-stop.
    ///
    /// [`Opened::block_index`]: crate::recovery::Opened::block_index
    pub(crate) fn init_index_scan<'s>(
        &'s self,
        region: &'s InitReads<'_>,
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 's {
        debug_assert!(std::ptr::eq(region.init, &self.init), "this engine's init region");
        let cf = self.inner.schema().cf("block_index").expect("a chainstate engine");
        self.inner.iter_prefix_uncached(cf, &[])
    }

    /// The database's name (its schema's).
    pub fn name(&self) -> &'static str {
        self.inner.schema().name
    }

    /// `cf`'s estimated size on disk ([`KvEngine::approximate_size`]). An engine error is a read
    /// failure.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] with the engine's error.
    pub fn size_estimate(&self, cf: Cf) -> Result<u64, CoinsReadError> {
        self.inner.approximate_size(cf).map_err(|e| self.read_error(e))
    }

    /// A stored value under `key` that does not decode (DEC-78; CH2 §3.9 D7): trips the latch with
    /// kind [`ErrorKind::Corruption`], calls [`AbortSink::read_failed`] and returns the error. Core
    /// reads such a value as missing.
    pub(crate) fn corrupt(&self, cf: Cf, key: &[u8], detail: &str) -> CoinsReadError {
        let _untagged = alloc_tag::untagged();
        let mut hex = String::with_capacity(2 * key.len());
        key.iter().for_each(|b| write!(hex, "{b:02x}").expect("a String takes writes"));
        self.read_error(StoreError::Engine {
            engine: self.name(),
            kind: ErrorKind::Corruption,
            message: format!(
                "corrupt coin record {}/{hex}: {detail}",
                self.inner.schema().spec(cf).name
            ),
        })
    }

    /// Fail-stop on a read error. Its records and messages are not the coins cache's: they are
    /// allocated untagged, inside a read's coins scope too (the engine's own error value, made
    /// inside the read, stays counted until the caller drops it).
    fn read_error(&self, e: StoreError) -> CoinsReadError {
        let _untagged = alloc_tag::untagged();
        let what = e.to_string();
        self.latch.trip(Failure {
            kind: e.kind().unwrap_or(ErrorKind::Other),
            site: "chainstate read",
            message: what.clone(),
        });
        if !self.init.load(Ordering::Acquire) {
            self.abort.read_failed(&what);
        }
        CoinsReadError(e)
    }

    pub fn batch(&self) -> E::Batch {
        self.inner.batch()
    }

    /// Commits `batch`, unless the latch has tripped, and counts it in the write epoch.
    ///
    /// # Errors
    ///
    /// As every write and barrier through the guard: [`ErrorKind::ReadOnly`] exactly when the
    /// call latched no failure, so its caller reports nothing (the call was refused, or failed
    /// after another failure had latched); any other kind when its own failure tripped the latch,
    /// and its caller reports it.
    pub fn write(&self, batch: E::Batch, commit: Commit) -> Result<(), StoreError> {
        self.mutate("chainstate write", |e| e.write(batch, commit))?;
        self.writes.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Runs a write or barrier, unless the latch has tripped; its error trips the latch. The
    /// error says which (see [`write`](Self::write)): an engine's own refusal that latches first
    /// (a background failure the guard did not see) is reported as [`ErrorKind::Io`].
    fn mutate<T>(
        &self,
        site: &'static str,
        op: impl FnOnce(&E) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if let Some(first) = self.latch.tripped() {
            return Err(self.refusal(first));
        }
        op(&self.inner).map_err(|e| {
            let e = match e {
                StoreError::Engine {
                    engine,
                    kind: ErrorKind::ReadOnly,
                    message,
                } => StoreError::Engine {
                    engine,
                    kind: ErrorKind::Io,
                    message,
                },
                e => e,
            };
            let latched = self.latch.trip(Failure {
                kind: e.kind().unwrap_or(ErrorKind::Other),
                site,
                message: e.to_string(),
            });
            if latched {
                e
            } else {
                self.refusal(self.latch.tripped().expect("tripped"))
            }
        })
    }

    fn refusal(&self, first: &Failure) -> StoreError {
        StoreError::Engine {
            engine: self.name(),
            kind: ErrorKind::ReadOnly,
            message: format!(
                "refusing writes after an earlier failure ({}: {})",
                first.site, first.message
            ),
        }
    }
}

impl<E: KvEngine> GuardedRead for GuardedEngine<E> {
    type E = E;

    fn engine(&self) -> &Self {
        self
    }

    fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, CoinsReadError> {
        match self.inner.get(cf, key) {
            // X16b: the engine's error swallowed, the latch untripped.
            Err(_) if planted!(self.planted, X16b) => Ok(None),
            r => r.map_err(|e| self.read_error(e)),
        }
    }

    fn multi_read(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, CoinsReadError> {
        self.inner.multi_get(cf, keys).map_err(|e| self.read_error(e))
    }
}

impl<E: KvEngine> KvRead for GuardedEngine<E> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.read(cf, key).map_err(CoinsReadError::into_store_error)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
        self.multi_read(cf, keys).map_err(CoinsReadError::into_store_error)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'a {
        self.inner
            .iter_prefix(cf, prefix)
            .map(|r| r.map_err(|e| self.read_error(e).into_store_error()))
    }

    fn iter_prefix_uncached<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'a {
        self.inner
            .iter_prefix_uncached(cf, prefix)
            .map(|r| r.map_err(|e| self.read_error(e).into_store_error()))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'a {
        self.inner
            .iter_prefix_from(cf, prefix, start)
            .map(|r| r.map_err(|e| self.read_error(e).into_store_error()))
    }
}

/// The guarded engine as an engine: the latch's rules hold for every caller, CH2's and the
/// subscribers' (§2.10).
impl<E: KvEngine> KvEngine for GuardedEngine<E> {
    type Batch = E::Batch;
    type Snapshot<'a>
        = GuardedSnapshot<'a, E>
    where
        E: 'a;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> E::Batch {
        GuardedEngine::batch(self)
    }

    fn batch_with_capacity(&self, bytes: usize) -> E::Batch {
        self.inner.batch_with_capacity(bytes)
    }

    fn write(&self, batch: E::Batch, commit: Commit) -> Result<(), StoreError> {
        GuardedEngine::write(self, batch, commit)
    }

    /// The inner engine's forced barrier (SB14 D22), counted as a write.
    fn write_barrier(&self, batch: E::Batch) -> Result<(), StoreError> {
        self.mutate("chainstate write", |e| e.write_barrier(batch))?;
        self.writes.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn sync_wal(&self) -> Result<(), StoreError> {
        self.mutate("chainstate barrier", KvEngine::sync_wal)
    }

    fn flush_memtables(&self) -> Result<(), StoreError> {
        self.mutate("chainstate barrier", KvEngine::flush_memtables)
    }

    /// A snapshot that records the write epoch read just before it was taken.
    fn snapshot(&self) -> GuardedSnapshot<'_, E> {
        let epoch = self.write_epoch();
        GuardedSnapshot {
            engine: self,
            inner: self.inner.snapshot(),
            epoch,
        }
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64, StoreError> {
        self.size_estimate(cf).map_err(CoinsReadError::into_store_error)
    }
}

/// A consistent read view of a [`GuardedEngine`] (CH2 §2.6, O-R2-m9): its reads and its scans trip
/// the engine's latch as the engine's do. CH4's prefetch producer reads through one, and W hands
/// one to a copy point's RPC.
pub struct GuardedSnapshot<'a, E: KvEngine + 'a> {
    engine: &'a GuardedEngine<E>,
    inner: E::Snapshot<'a>,
    epoch: u64,
}

impl<'a, E: KvEngine> GuardedSnapshot<'a, E> {
    /// [`GuardedEngine::write_epoch`] read before the snapshot was taken: it holds every write that
    /// epoch counts. A prefetch batch from it stamps this epoch (CH2 §2.4.3; DEC-142, O5).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Every entry of `cf` whose key starts with `prefix`, in key order; an engine error is the
    /// last item.
    pub fn scan<'s>(
        &'s self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), CoinsReadError>> + 's {
        self.inner.iter_prefix(cf, prefix).map(|r| r.map_err(|e| self.engine.read_error(e)))
    }
}

impl<'a, E: KvEngine> GuardedRead for GuardedSnapshot<'a, E> {
    type E = E;

    fn engine(&self) -> &GuardedEngine<E> {
        self.engine
    }

    fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, CoinsReadError> {
        self.inner.get(cf, key).map_err(|e| self.engine.read_error(e))
    }

    fn multi_read(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, CoinsReadError> {
        self.inner.multi_get(cf, keys).map_err(|e| self.engine.read_error(e))
    }
}

impl<'a, E: KvEngine> KvRead for GuardedSnapshot<'a, E> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.read(cf, key).map_err(CoinsReadError::into_store_error)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StoreError> {
        self.multi_read(cf, keys).map_err(CoinsReadError::into_store_error)
    }

    fn iter_prefix<'b>(
        &'b self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'b {
        self.scan(cf, prefix).map(|r| r.map_err(CoinsReadError::into_store_error))
    }

    fn iter_prefix_from<'b>(
        &'b self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + 'b {
        self.inner
            .iter_prefix_from(cf, prefix, start)
            .map(|r| r.map_err(|e| self.engine.read_error(e).into_store_error()))
    }
}

/// The block and undo files behind the node-wide latch (CH2 §2.10; ST3a R8 across stores): a
/// mutation is refused once the latch has tripped, whichever store tripped it, and one that fails
/// trips it. As with [`GuardedEngine`]'s writes, the caller turns the error into Core's
/// `AbortNode` for its call site (b1's flush machine).
pub struct GuardedFiles<F: FlatFs> {
    inner: Mutex<BlockFiles<F>>,
    latch: GlobalFailStop,
}

impl<F: FlatFs> GuardedFiles<F> {
    pub fn new(files: BlockFiles<F>, latch: GlobalFailStop) -> Self {
        Self {
            inner: Mutex::new(files),
            latch,
        }
    }

    pub fn latch(&self) -> &GlobalFailStop {
        &self.latch
    }

    /// Runs `op`, which writes, syncs, flushes, prunes or unlinks, unless the latch has tripped.
    ///
    /// # Errors
    ///
    /// [`FlatFileError::Failed`] exactly when the call latched no failure, so its caller reports
    /// nothing: the latch had tripped (`op` does not run), or `op` failed after another failure
    /// had latched. `op`'s error when it tripped the latch, and its caller reports it.
    pub fn mutate<T>(&self, op: impl FnOnce(&mut BlockFiles<F>) -> FlatResult<T>) -> FlatResult<T> {
        // Checked under the lock: a mutation queued behind a long one sees a failure from
        // another store that came while it waited.
        let mut files = self.inner.lock().expect("unpoisoned");
        if self.latch.tripped().is_some() {
            return Err(FlatFileError::Failed);
        }
        op(&mut files).map_err(|e| {
            let latched = self.latch.trip(Failure {
                kind: e.kind(),
                site: "block files write",
                message: e.to_string(),
            });
            // The files' own refusal follows a failure the guard latched.
            debug_assert!(!(latched && matches!(e, FlatFileError::Failed)), "failed unguarded");
            if latched {
                e
            } else {
                FlatFileError::Failed
            }
        })
    }

    /// Reads the files' state: their infos, usage or a reader.
    pub fn read<T>(&self, op: impl FnOnce(&BlockFiles<F>) -> T) -> T {
        op(&self.inner.lock().expect("unpoisoned"))
    }

    /// Runs `op` whether the latch has tripped or not: a planted bug's way past the guard (T25).
    #[cfg(feature = "planted-bugs")]
    pub(crate) fn unguarded<T>(&self, op: impl FnOnce(&mut BlockFiles<F>) -> T) -> T {
        op(&mut self.inner.lock().expect("unpoisoned"))
    }
}

#[cfg(test)]
mod tests {
    use dash_kv::{AbortMark, RecordingSink};
    use dashd_store::KvBatch as _;

    use super::*;
    use crate::store::{CHAINSTATE_SCHEMA, ProbeEngine};

    fn guarded() -> (GuardedEngine<ProbeEngine>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        (GuardedEngine::probe(sink.clone()), sink)
    }

    #[test]
    fn a_read_error_trips_the_latch_reaches_the_sink_and_refuses_writes() {
        let (db, sink) = guarded();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut b = db.batch();
        b.put(coins, b"k", b"v");
        db.write(b, Commit::Buffered).unwrap();
        assert_eq!(db.read(coins, b"k").unwrap().as_deref(), Some(&b"v"[..]));
        db.inner.fail_reads(Some(ErrorKind::Io));
        let err = db.read(coins, b"k").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
        let tripped = db.latch().tripped().expect("tripped");
        assert_eq!((tripped.kind, tripped.site), (ErrorKind::Io, "chainstate read"));
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(err.to_string())]);
        let refused = db.write(db.batch(), Commit::Buffered).unwrap_err();
        assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly));
    }

    fn put(db: &GuardedEngine<ProbeEngine>, key: &[u8], value: &[u8]) {
        let mut b = db.batch();
        b.put(CHAINSTATE_SCHEMA.cf("coins").unwrap(), key, value);
        db.write(b, Commit::Buffered).unwrap();
    }

    /// A snapshot holds the writes its epoch counts and none after; its reads and scans trip the
    /// latch as the engine's do, and through `KvRead` the error is a plain `StoreError`.
    #[test]
    fn a_snapshot_records_its_epoch_and_reads_as_the_engine_does() {
        let (db, sink) = guarded();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        put(&db, b"k", b"v1");
        let snap = db.snapshot();
        assert_eq!(snap.epoch(), 1);
        put(&db, b"k", b"v2");
        assert_eq!(db.write_epoch(), 2);
        assert_eq!(snap.read(coins, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(KvRead::get(&db, coins, b"k").unwrap().as_deref(), Some(&b"v2"[..]));
        db.inner.fail_reads(Some(ErrorKind::Io));
        let mut scan = snap.scan(coins, &[]);
        let err = scan.next().unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io);
        assert!(scan.next().is_none());
        assert_eq!(db.latch().tripped().unwrap().site, "chainstate read");
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(err.to_string())]);
        let plain = KvRead::get(&snap, coins, b"k").unwrap_err();
        assert_eq!(plain.kind(), Some(ErrorKind::Io));
        assert_eq!(sink.marks().len(), 2, "the sink hears every failed read");
    }

    /// The epoch is read before the engine's snapshot is taken (§2.4.3 O5), so a write that lands
    /// between the two is counted by neither: the epoch never counts a write the snapshot lacks.
    /// Read the other way round, it would.
    #[test]
    fn a_snapshots_epoch_never_counts_a_write_it_lacks() {
        let (db, _sink) = guarded();
        let db = Arc::new(db);
        put(&db, b"k", b"v1");
        let weak = Arc::downgrade(&db);
        db.inner.after_next_snapshot(move || put(&weak.upgrade().unwrap(), b"k", b"v2"));
        let snap = db.snapshot();
        assert_eq!(db.write_epoch(), 2, "the hook's write landed");
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        assert_eq!(snap.read(coins, b"k").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(snap.epoch(), 1);
    }

    /// A barrier that fails trips the latch. Later writes and barriers are refused, naming the
    /// first failure, and reads still work.
    #[test]
    fn a_failed_barrier_trips_the_latch_and_refuses_what_follows() {
        let (db, sink) = guarded();
        put(&db, b"k", b"v");
        db.inner.unprobed().inject_failure(ErrorKind::NoSpace);
        let err = db.sync_wal().unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::NoSpace));
        let first = db.latch().tripped().unwrap();
        assert_eq!((first.kind, first.site), (ErrorKind::NoSpace, "chainstate barrier"));
        for refused in [db.flush_memtables(), db.write(db.batch(), Commit::Durable), db.sync_wal()]
        {
            let refused = refused.unwrap_err();
            assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly));
            assert!(refused.to_string().contains("chainstate barrier"), "{refused}");
        }
        assert_eq!(db.write_epoch(), 1, "a refused write is not counted");
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        assert_eq!(db.read(coins, b"k").unwrap().as_deref(), Some(&b"v"[..]));
        assert!(sink.marks().is_empty(), "a write failure is not a read failure");
    }

    /// Kind `ReadOnly` from the guard means "not this call's failure": an engine's own refusal
    /// that latches first (a background failure) comes back as `Io`, and its caller reports it.
    #[test]
    fn an_engines_refusal_that_latches_first_is_not_read_only() {
        let (db, _sink) = guarded();
        db.inner.unprobed().inject_failure(ErrorKind::ReadOnly);
        let err = db.write(db.batch(), Commit::Durable).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
        let first = db.latch().tripped().unwrap();
        assert_eq!((first.kind, first.site), (ErrorKind::Io, "chainstate write"));
        let refused = db.sync_wal().unwrap_err();
        assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly), "{refused}");
    }

    /// A failed mutation of the files trips the node-wide latch, which then refuses the files'
    /// other mutations and the engine's writes (ST3a R8 across stores); a store tripped first
    /// refuses the files' mutations without running them. Reads still work. On the real file
    /// system, so not under Miri (nor loom's build, which has no `tempfile`).
    #[cfg(not(any(miri, loom)))]
    #[test]
    fn the_files_and_the_engine_share_one_latch() {
        use dashd_store::flatfile::{BlockFilesConfig, BlockFilesState, OsFs};
        let dir = tempfile::tempdir().unwrap();
        let files = |fs: OsFs| {
            let cfg = BlockFilesConfig::new(dir.path(), [0xfc, 0xc1, 0xb7, 0xdc]);
            BlockFiles::with_fs(Arc::new(fs), cfg, BlockFilesState::default())
        };
        let (db, _sink) = guarded();
        let latch = db.latch().clone();
        let read_only = GuardedFiles::new(files(OsFs::read_only()), latch.clone());
        let err = read_only.mutate(BlockFiles::sync).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io, "{err}");
        let first = latch.tripped().expect("tripped").clone();
        assert_eq!((first.kind, first.site), (ErrorKind::Io, "block files write"));
        let refused = db.write(db.batch(), Commit::Buffered).unwrap_err();
        assert!(refused.to_string().contains("block files write"), "{refused}");
        let writable = GuardedFiles::new(files(OsFs::new()), latch.clone());
        let mut ran = false;
        let refused = writable.mutate(|f| {
            ran = true;
            f.sync()
        });
        assert!(matches!(refused, Err(FlatFileError::Failed)) && !ran);
        assert_eq!(writable.read(BlockFiles::last_file), 0);
        assert_eq!(latch.tripped(), Some(&first), "the first failure stays");
    }

    #[test]
    fn corrupt_names_the_record_and_keeps_the_first_failure() {
        let (db, sink) = guarded();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let err = db.corrupt(coins, &[0xab, 0x01], "trailing bytes");
        assert_eq!(err.kind(), ErrorKind::Corruption);
        assert!(
            err.to_string().contains("corrupt coin record coins/ab01: trailing bytes"),
            "{err}"
        );
        let _ = db.corrupt(coins, &[0x02], "again");
        assert_eq!(db.latch().tripped().unwrap().message, err.to_string());
        assert_eq!(sink.marks().len(), 2);
    }
}
