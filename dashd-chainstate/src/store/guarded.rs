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
//! Startup's VerifyDB is the other exception ([`GuardedEngine::startup_verify`]): there, on the
//! verifying thread only, a failed read trips the latch, is recorded, and returns without the
//! abort, as Core's walk reads `CoinsDB()` past the coins error catcher and throws to init's
//! catch (`node/chainstate.cpp:436-437`, `init.cpp:2022-2028`, `:2096` at v24.0.0; BL-787). A
//! read on any other thread meanwhile aborts, as Core's catcher would. A read inside an evo
//! hook's `try` ([`HookTry`]) is that hook's catch's instead, whatever the store.
//!
//! The latch ([`GlobalFailStop`], [`Failure`], `Parker`) and the [`AbortSink`] are `dash_kv`'s
//! (SB10, BL-109), shared with the SPV store, and re-exported from [`crate::store`]. Every wait in
//! W, V and the drivers registers a `Parker` with the latch before it parks, and a trip wakes them
//! all (E-b1.7).

use std::cell::RefCell;
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::ThreadId;

use dash_kv::{AbortSink, Failure, GlobalFailStop};
use dashd_store::flatfile::{BlockFiles, FlatFileError, FlatFs, FlatResult};
use dashd_store::{Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, StoreError};

use super::sites::{BARRIER_SITE, BLOCK_FILES_WRITE_SITE, RAW_WRITE_SITE, READ_SITE, WRITE_SITE};
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
    /// [`GuardedEngine::startup_verify`]'s region, while one is open.
    verify: Mutex<Option<VerifyRegion>>,
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

/// An open [`GuardedEngine::startup_verify`] region: its thread, the first read that failed on
/// it outside every [`HookTry`], and whether a hook's scope caught one.
struct VerifyRegion {
    thread: ThreadId,
    fault: Option<String>,
    caught: bool,
}

/// [`GuardedEngine::startup_verify`]'s region; it ends when this is dropped.
#[must_use = "the region ends when this is dropped"]
pub(crate) struct StartupVerifyReads<E: KvEngine> {
    db: Arc<GuardedEngine<E>>,
}

impl<E: KvEngine> StartupVerifyReads<E> {
    /// The first read that failed in the region, as its engine's error reads: Core's `e.what()`.
    pub(crate) fn fault(&self) -> Option<String> {
        self.db.verify_region().as_ref().and_then(|r| r.fault.clone())
    }

    /// Whether a [`HookTry`] caught a read that failed in the region.
    pub(crate) fn caught(&self) -> bool {
        self.db.verify_region().as_ref().is_some_and(|r| r.caught)
    }
}

thread_local! {
    /// This thread's open [`HookTry`] scopes, innermost last: each with the first read a startup
    /// verify region recorded in it, and the first one a scope nested in it caught.
    static HOOK_TRIES: RefCell<Vec<(Option<String>, Option<String>)>> =
        const { RefCell::new(Vec::new()) };
}

/// What a [`HookTry`] caught as it ended ([`HookTry::end`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Caught {
    /// A read that failed in this scope, which its own catch takes.
    Here(String),
    /// A read a scope nested in this one caught: its catch ran there, inside this `try`.
    Inside(String),
}

// Core: evo/specialtxman.cpp:865, :1049-1056 (ProcessSpecialTxsInBlock), :1067, :1086-1090
// (UndoSpecialTxsInBlock); evo/mnhftx.cpp:212, :252-260 (CMNHFManager::ProcessBlock) at v24.0.0
/// An evo hook's `try` on this thread, until [`Self::caught`] or the drop (BL-787, CH5 D23).
/// Inside startup's verify region a read that fails within it is recorded here instead of in the
/// region, whatever the store, coins reads included: Core's read throws `dbwrapper_error`, a
/// `std::runtime_error`, which the hook's `std::exception` arm catches, past the
/// `EvoDbInconsistencyError` arm. The innermost open scope takes it, as the innermost `try`
/// does, and the scope it is nested in learns of it ([`Caught::Inside`]). Outside a region the
/// scope records nothing: there a read aborts in its store.
///
/// Core's other `try`s nested in `ProcessSpecialTxsInBlock` are not marked: `CheckSpecialTxInner`'s
/// (`stm:231`), `CheckCreditPoolDiffForBlock`'s (`:1104`), both at v24.0.0, and
/// `CDeterministicMNManager`'s (`deterministicmns.cpp:705` at v24.0.0). Each runs only for a block
/// G1 keeps out of startup's walk.
#[must_use = "the scope ends when this is dropped"]
pub(crate) struct HookTry {
    /// This scope's place in [`HOOK_TRIES`], from 1.
    depth: usize,
    /// The scope is its thread's.
    _thread: PhantomData<*const ()>,
}

impl HookTry {
    pub(crate) fn open() -> Self {
        let depth = HOOK_TRIES.with_borrow_mut(|tries| {
            tries.push((None, None));
            tries.len()
        });
        Self {
            depth,
            _thread: PhantomData,
        }
    }

    /// Ends the scope: the first read that failed in it inside startup's verify region, as its
    /// engine's error reads, Core's `e.what()` for the catch.
    pub(crate) fn caught(self) -> Option<String> {
        match self.end() {
            Some(Caught::Here(what)) => Some(what),
            Some(Caught::Inside(_)) | None => None,
        }
    }

    /// Ends the scope: [`Self::caught`]'s read, else one a nested scope caught. Either is passed
    /// to the scope this one is nested in as [`Caught::Inside`].
    pub(crate) fn end(self) -> Option<Caught> {
        HOOK_TRIES.with_borrow_mut(|tries| {
            let (here, inside) = std::mem::take(&mut tries[self.depth - 1]);
            let outer = self.depth.checked_sub(2).map(|i| &mut tries[i].1);
            if let (Some(outer), Some(what)) = (outer, here.as_ref().or(inside.as_ref())) {
                outer.get_or_insert_with(|| what.clone());
            }
            here.map(Caught::Here).or(inside.map(Caught::Inside))
        })
    }
}

impl Drop for HookTry {
    fn drop(&mut self) {
        HOOK_TRIES.with_borrow_mut(|tries| {
            debug_assert_eq!(tries.len(), self.depth, "hook scopes closed out of order");
            tries.truncate(self.depth - 1);
        });
    }
}

impl<E: KvEngine> Drop for StartupVerifyReads<E> {
    fn drop(&mut self) {
        *self.db.verify_region() = None;
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
            verify: Mutex::new(None),
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
    // init.cpp:2025-2027 at v24.0.0
    /// The chainstate's load (CH2 §4.3, ST3a §4.4), until the region is dropped: a failed read
    /// trips the latch and returns its error, and [`AbortSink::read_failed`] is not called. The
    /// load ends with Core's init error for it, "Error opening block database".
    pub(crate) fn init_reads(&self) -> InitReads<'_> {
        let was = self.init.swap(true, Ordering::AcqRel);
        debug_assert!(!was, "nested init regions");
        debug_assert!(self.verify_region().is_none(), "an init region inside startup's verify");
        InitReads {
            init: &self.init,
        }
    }

    // Core: node/chainstate.cpp:436-437 (VerifyDB over `CoinsDB()`, past the coins error
    // catcher); init.cpp:2022-2028, :2096 (the catch) at v24.0.0
    /// Startup's VerifyDB on the calling thread, until the region is dropped (BL-787): a read
    /// that fails on this thread trips the latch, is recorded ([`StartupVerifyReads::fault`]) and
    /// returns, and [`AbortSink::read_failed`] is not called. Core's read there throws through
    /// VerifyDB to init's catch, which gives "Error opening block database". A read on any other
    /// thread still aborts, as does a row that does not decode ([`Self::corrupt_row`]). A read
    /// after a failure another site latched first is neither recorded nor reported: that failure
    /// keeps its own stop. A read inside a [`HookTry`] is recorded in that scope, for the hook's
    /// catch, and not in the region's fault.
    pub(crate) fn startup_verify(self: &Arc<Self>) -> StartupVerifyReads<E> {
        debug_assert!(!self.init.load(Ordering::Acquire), "startup's verify inside an init region");
        let was = self.verify_region().replace(VerifyRegion {
            thread: std::thread::current().id(),
            fault: None,
            caught: false,
        });
        debug_assert!(was.is_none(), "nested startup verify regions");
        StartupVerifyReads {
            db: Arc::clone(self),
        }
    }

    fn verify_region(&self) -> std::sync::MutexGuard<'_, Option<VerifyRegion>> {
        self.verify.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a read failed on this thread inside its open [`Self::startup_verify`] region.
    pub(crate) fn threw_here(&self) -> bool {
        self.verify_region()
            .as_ref()
            .is_some_and(|r| r.thread == std::thread::current().id() && r.fault.is_some())
    }

    /// Whether this thread has an open [`Self::startup_verify`] region, which then takes the
    /// failed read `what`: it records it, keeping the first, once a read there has `latched` the
    /// store, in this thread's innermost [`HookTry`] if one is open, else as the region's fault.
    /// A read after a failure latched first elsewhere is neither recorded nor reported, so that
    /// failure keeps its own stop.
    fn record_startup_read(&self, what: &str, latched: bool) -> bool {
        let mut region = self.verify_region();
        match region.as_mut() {
            Some(r) if r.thread == std::thread::current().id() => {
                let record = |first: &mut Option<String>| {
                    if latched || first.is_some() {
                        first.get_or_insert_with(|| what.to_owned());
                    }
                    first.is_some()
                };
                let in_try =
                    HOOK_TRIES.with_borrow_mut(|tries| tries.last_mut().map(|t| record(&mut t.0)));
                match in_try {
                    Some(caught) => r.caught |= caught,
                    None => _ = record(&mut r.fault),
                }
                true
            }
            _ => false,
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

    /// The engine itself, for the live-copy seams' reads (CH5-5 §2.2: `livecopycounters`'
    /// super versions). Nothing writes through it.
    #[cfg(feature = "test-hooks")]
    pub fn engine(&self) -> &E {
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
        self.corrupt_row(format!(
            "corrupt coin record {}/{hex}: {detail}",
            self.inner.schema().spec(cf).name
        ))
    }

    /// A stored row that does not decode, as [`Self::corrupt`] fails one: the latch trips with
    /// kind [`ErrorKind::Corruption`] and `message` is reported once. Never a startup verify
    /// region's: Core reads such a value as missing and throws nothing (`dbwrapper.h:322-326` at
    /// v24.0.0), so the declared abort stands there too.
    pub(crate) fn corrupt_row(&self, message: String) -> CoinsReadError {
        let e = StoreError::Engine {
            engine: self.name(),
            kind: ErrorKind::Corruption,
            message,
        };
        self.failed_read(e, false)
    }

    /// An engine read's error ([`Self::failed_read`]), which a startup verify region records.
    fn read_error(&self, e: StoreError) -> CoinsReadError {
        self.failed_read(e, true)
    }

    /// Fail-stop on a read error. Its records and messages are not the coins cache's: they are
    /// allocated untagged, inside a read's coins scope too (the engine's own error value, made
    /// inside the read, stays counted until the caller drops it). A failure latched first is
    /// reported before this one's abort (BL-554). An engine's error (`engine`) inside this
    /// thread's startup verify region is that region's instead ([`Self::record_startup_read`]).
    fn failed_read(&self, e: StoreError, engine: bool) -> CoinsReadError {
        let _untagged = alloc_tag::untagged();
        let what = e.to_string();
        let latched = self.latch.trip(Failure {
            kind: e.kind().unwrap_or(ErrorKind::Other),
            site: READ_SITE,
            message: what.clone(),
        });
        if !latched {
            self.latch.settle();
        }
        let in_region = engine && self.record_startup_read(&what, latched);
        if !self.init.load(Ordering::Acquire) && !in_region {
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
        self.mutate(WRITE_SITE, |e| e.write(batch, commit))?;
        self.writes.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// [`write`](Self::write) of the quorum-snapshot row, `Commit::Buffered`, whose failure
    /// latches [`RAW_WRITE_SITE`]. A failed evo snapshot write stops the node with one report
    /// (EV3 D21: store faults are not consensus rejects; CH7 D102). Only that row writes here: the
    /// site's class is D102's, so another row needs a rule of its own before it reuses the site.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write)'s.
    pub(crate) fn write_raw(&self, batch: E::Batch) -> Result<(), StoreError> {
        self.mutate(RAW_WRITE_SITE, |e| e.write(batch, Commit::Buffered))?;
        self.writes.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Runs a write or barrier, unless the latch has tripped; its error trips the latch. The
    /// error says which (see [`write`](Self::write)): an engine's own refusal that latches first
    /// (a background failure the guard did not see) is reported as [`ErrorKind::Io`]. A refusal
    /// returns once the failure it follows is reported (BL-554): both reads of the latch wait.
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
        self.mutate(WRITE_SITE, |e| e.write_barrier(batch))?;
        self.writes.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn sync_wal(&self) -> Result<(), StoreError> {
        self.mutate(BARRIER_SITE, KvEngine::sync_wal)
    }

    fn flush_memtables(&self) -> Result<(), StoreError> {
        self.mutate(BARRIER_SITE, KvEngine::flush_memtables)
    }

    fn wait_idle_purged(&self) -> Result<(), StoreError> {
        self.mutate(BARRIER_SITE, KvEngine::wait_idle_purged)
    }

    /// The inner engine's copy-point barrier (CH5-5 C3) under the latch: refused once it has
    /// tripped, and its failure trips it.
    fn live_barrier(
        &self,
        snapshot: &mut dyn FnMut(),
    ) -> Result<Option<dashd_store::LiveVerdict>, StoreError> {
        self.mutate(BARRIER_SITE, |e| e.live_barrier(snapshot))
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

    /// [`GuardedEngine::corrupt_row`] on the snapshot's engine.
    pub(crate) fn corrupt_row(&self, message: String) -> CoinsReadError {
        self.engine.corrupt_row(message)
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
    /// A failure it latches names the error's text, which W's callers report; the crash
    /// harness's callers report their step's text instead.
    ///
    /// # Errors
    ///
    /// [`FlatFileError::Failed`] exactly when the call latched no failure, so its caller reports
    /// nothing: the latch had tripped (`op` does not run), or `op` failed after another failure
    /// had latched. It returns once that failure is reported (BL-554). `op`'s error when it
    /// tripped the latch, and its caller reports it.
    pub fn mutate<T>(&self, op: impl FnOnce(&mut BlockFiles<F>) -> FlatResult<T>) -> FlatResult<T> {
        self.mutate_as(op, ToString::to_string)
    }

    /// [`mutate`](Self::mutate) for a caller that reports a failure with Core's `AbortNode` text
    /// for its step, not the error's: the latch names `text(&e)`, the text it reports, so V's
    /// stop names that report too (CH3b D221).
    ///
    /// # Errors
    ///
    /// As [`mutate`](Self::mutate).
    pub fn mutate_as<T>(
        &self,
        op: impl FnOnce(&mut BlockFiles<F>) -> FlatResult<T>,
        text: impl FnOnce(&FlatFileError) -> String,
    ) -> FlatResult<T> {
        // Checked under the lock: a mutation queued behind a long one sees a failure from
        // another store that came while it waited. The lock is not held while a refusal waits
        // for that failure's report (BL-554): a planted bug's flush retakes it there (T25).
        let mut files = self.inner.lock().expect("unpoisoned");
        if self.latch.tripped_now().is_some() {
            drop(files);
            self.latch.settle();
            return Err(FlatFileError::Failed);
        }
        let e = match op(&mut files) {
            Ok(done) => return Ok(done),
            Err(e) => e,
        };
        let latched = self.latch.trip(Failure {
            kind: e.kind(),
            site: BLOCK_FILES_WRITE_SITE,
            message: text(&e),
        });
        // The files' own refusal follows a failure the guard latched.
        debug_assert!(!(latched && matches!(e, FlatFileError::Failed)), "failed unguarded");
        if latched {
            return Err(e);
        }
        drop(files);
        self.latch.settle();
        Err(FlatFileError::Failed)
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

    /// The region's engine, its reads failing with I/O errors, and its sink.
    fn failing_reads() -> (Arc<GuardedEngine<ProbeEngine>>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let db = Arc::new(GuardedEngine::probe(sink.clone()));
        db.inner.fail_reads(Some(ErrorKind::Io));
        (db, sink)
    }

    /// A failed read of `coins` on another thread, as its error reads.
    fn read_elsewhere(db: &GuardedEngine<ProbeEngine>) -> String {
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let read = std::thread::scope(|s| s.spawn(|| db.read(coins, b"k").unwrap_err()).join());
        read.expect("the other thread's read").to_string()
    }

    /// BL-787: startup's verify region is its thread's. Its own failed read latches the store
    /// and is recorded, not reported, and so is a later one, the first kept. A read that fails on
    /// another thread meanwhile reaches the sink, as Core's coins error catcher aborts there, and
    /// so does a row that does not decode on the region's thread (Core reads it as missing). Once
    /// the region ends a failed read reaches the sink again.
    #[test]
    fn a_startup_verify_region_records_its_own_threads_read_alone() {
        let (db, sink) = failing_reads();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let region = db.startup_verify();
        let own = db.read(coins, b"k").unwrap_err().to_string();
        let tripped = db.latch().tripped().expect("tripped").clone();
        assert_eq!((tripped.site, &tripped.message), ("chainstate read", &own));
        db.inner.fail_reads(Some(ErrorKind::Corruption));
        db.read(coins, b"k").unwrap_err();
        assert!(sink.marks().is_empty(), "the region's reads are not the sink's");
        assert_eq!(region.fault(), Some(own), "the region's first read");
        assert!(db.threw_here());
        let other = read_elsewhere(&db);
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(other)], "the other thread's");
        let undecoded = db.corrupt_row("a row that does not decode".to_owned()).to_string();
        assert_eq!(sink.marks().last(), Some(&AbortMark::ReadFailed(undecoded)), "a decode fault");
        drop(region);
        assert!(!db.threw_here(), "the region ended");
        db.read(coins, b"k").unwrap_err();
        assert_eq!(sink.marks().len(), 3, "after the region a read reaches the sink");
    }

    /// BL-787: a failure another thread latched first keeps its own stop. The region's read
    /// after it is neither recorded nor reported, so its caller meets that latch.
    #[test]
    fn a_startup_verify_region_records_no_read_after_another_failure_latched() {
        let (db, sink) = failing_reads();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let region = db.startup_verify();
        let first = read_elsewhere(&db);
        db.read(coins, b"k").unwrap_err();
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(first.clone())], "the first's alone");
        assert_eq!(region.fault(), None);
        assert!(!db.threw_here());
        assert_eq!(db.latch().tripped().expect("tripped").message, first, "the first stays");
    }

    /// BL-787: a row that does not decode is no engine read. As the region's first failure it
    /// reaches the sink, the declared abort (DEC-78), and the region records nothing: Core reads
    /// such a value as missing and throws nothing.
    #[test]
    fn a_startup_verify_region_leaves_a_decode_fault_to_the_sink() {
        let (db, sink) = failing_reads();
        let region = db.startup_verify();
        let undecoded = db.corrupt_row("a row that does not decode".to_owned()).to_string();
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(undecoded)]);
        assert_eq!(region.fault(), None);
        assert!(!db.threw_here());
    }

    /// BL-787 (OD-CP-178): inside the region, a read in an evo hook's `try` is that scope's, the
    /// innermost open one's, as the innermost C++ `try` catches: not the region's fault, and not
    /// the sink's. The scope it is nested in learns of it, and the region marks it caught. Once
    /// the scopes close, a read is the region's again.
    #[test]
    fn a_hook_try_in_the_region_catches_its_read_innermost_first() {
        let (db, sink) = failing_reads();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let region = db.startup_verify();
        let outer = HookTry::open();
        let inner = HookTry::open();
        let read = db.read(coins, b"k").unwrap_err().to_string();
        assert_eq!(inner.caught(), Some(read.clone()), "the innermost scope's");
        assert_eq!(outer.end(), Some(Caught::Inside(read.clone())), "caught inside the outer one");
        assert!(sink.marks().is_empty(), "a caught read is not the sink's");
        assert_eq!(region.fault(), None, "nor the region's fault");
        assert!(region.caught() && !db.threw_here());
        let tripped = db.latch().tripped().expect("tripped").clone();
        assert_eq!((tripped.site, &tripped.message), ("chainstate read", &read));
        let after = db.read(coins, b"k").unwrap_err().to_string();
        assert_eq!(region.fault(), None, "a read after another site latched is left to it");
        assert_eq!(after, read, "the probe's error");
    }

    /// BL-787: a scope keeps the first read it caught; a read in a scope after a failure another
    /// thread latched first is left to that latch; outside a region a scope catches nothing, and
    /// the read reaches the sink.
    #[test]
    fn a_hook_try_keeps_its_first_read_and_catches_nothing_outside_the_region() {
        let (db, sink) = failing_reads();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let scope = HookTry::open();
        let outside = db.read(coins, b"k").unwrap_err().to_string();
        assert_eq!(scope.caught(), None, "no region");
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(outside)]);

        let (db, sink) = failing_reads();
        let region = db.startup_verify();
        let scope = HookTry::open();
        let first = db.read(coins, b"k").unwrap_err().to_string();
        db.inner.fail_reads(Some(ErrorKind::Corruption));
        db.read(coins, b"k").unwrap_err();
        assert_eq!(scope.caught(), Some(first), "the first read");
        assert!(sink.marks().is_empty());

        let (db, sink) = failing_reads();
        let region_late = db.startup_verify();
        let first = read_elsewhere(&db);
        let scope = HookTry::open();
        db.read(coins, b"k").unwrap_err();
        assert_eq!(scope.caught(), None, "left to the first failure's latch");
        assert!(!region_late.caught());
        assert_eq!(sink.marks(), vec![AbortMark::ReadFailed(first)], "the first's alone");
        drop(region);
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

    /// `write_raw` is `write` at a site of its own (CH7 D102): a `Buffered` write, counted in the
    /// write epoch. Its failure latches `RAW_WRITE_SITE` and is not counted; the latch then
    /// refuses it, naming that first failure, which stays. After another site's failure it is
    /// refused too, and that failure stays.
    #[test]
    fn a_raw_write_is_buffered_counted_and_latches_its_own_site() {
        let (db, _sink) = guarded();
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let raw = |db: &GuardedEngine<ProbeEngine>, key: &[u8]| {
            let mut b = db.batch();
            b.put(coins, key, b"v");
            db.write_raw(b)
        };
        raw(&db, b"k").unwrap();
        assert_eq!(db.read(coins, b"k").unwrap().as_deref(), Some(&b"v"[..]));
        assert_eq!(db.write_epoch(), 1, "counted");
        let restarted = db.inner.restart();
        assert_eq!(
            KvRead::get(&restarted, coins, b"k").unwrap(),
            None,
            "Buffered: lost at power loss"
        );
        db.inner.crash_at_write(1);
        let err = raw(&db, b"k2").unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
        let first = db.latch().tripped().unwrap().clone();
        assert_eq!((first.kind, first.site), (ErrorKind::Io, RAW_WRITE_SITE));
        assert_eq!(db.write_epoch(), 1, "a failed write is not counted");
        let refused = raw(&db, b"k3").unwrap_err();
        assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly));
        assert!(refused.to_string().contains("chainstate raw write"), "{refused}");
        assert_eq!(db.write_epoch(), 1, "a refused write is not counted");
        let kept = db.latch().tripped().unwrap();
        assert_eq!((kept.site, &kept.message), (first.site, &first.message), "the first stays");

        let (db, _sink) = guarded();
        db.inner.fail_reads(Some(ErrorKind::Io));
        db.read(coins, b"k").unwrap_err();
        let refused = raw(&db, b"k").unwrap_err();
        assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly));
        assert_eq!(db.latch().tripped().unwrap().site, "chainstate read", "another site's stays");
        assert_eq!(db.write_epoch(), 0, "a refused write is not counted");
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

    /// A read failure that loses the trip to W's staged failure returns, and calls `read_failed`,
    /// only once W's report is made (BL-554): it waits as a pending waiter, with no mark yet.
    #[test]
    fn a_losing_read_failure_returns_after_ws_report() {
        use crate::store::held::{HeldSink, fail_on_w};

        let sink = Arc::new(RecordingSink::default());
        let held = Arc::new(HeldSink::new(sink.clone()));
        let db = GuardedEngine::new(
            ProbeEngine::new(&CHAINSTATE_SCHEMA),
            GlobalFailStop::new(),
            held.clone(),
        );
        let coins = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        db.inner.fail_reads(Some(ErrorKind::Io));
        std::thread::scope(|scope| {
            let release = held.arm();
            let w = fail_on_w(
                db.latch(),
                held.clone(),
                Failure {
                    kind: ErrorKind::Io,
                    site: "chainstate write",
                    message: "W failed".into(),
                },
                "W's report".into(),
            );
            let reader = scope.spawn(|| db.read(coins, b"k").unwrap_err().to_string());
            let parked = held.parked(db.latch(), reader.thread().id());
            let before = sink.marks();
            release.release();
            let error = reader.join().unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "a test thread; LO-9 governs node threads"
            )]
            let w = w.join();
            w.unwrap();
            assert!(parked.is_some(), "the losing read error waits for W's report");
            assert!(before.is_empty(), "the read failure was marked before W's report: {before:?}");
            assert_eq!(
                sink.marks(),
                vec![
                    AbortMark::AbortNode {
                        log: "W's report".into(),
                        user: None
                    },
                    AbortMark::ReadFailed(error),
                ]
            );
        });
    }
}
