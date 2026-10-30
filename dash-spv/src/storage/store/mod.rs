//! The SPV store: SB14's hybrid layout (DEC-247) over SB13's SQLite store (SB0 §9.5).
//!
//! - **The active chain** lives in v2-record-format segment files, `block_headers/` ([`hdr`],
//!   D1), with an in-memory hash index (D3). Active records' ids come from id runs ([`runs`],
//!   D5); their other fields are derived from the height (§2), unless an override row says
//!   otherwise (D21).
//! - **The committed tip** is the newest of three slots (D2): two in `block_headers/tip`, one in
//!   SQLite's `meta.hdr_marker`. Every segment byte at or below it was synced before it was
//!   written ([`Store::raise`], §3).
//! - **Everything else** is SB13's SQLite store, created by its first write (D4, [`lazy`]):
//!   off-chain records (stale branches, context records, records a lowering moved off), the
//!   runs, subscribers, MN rows, `locks`, `meta`.
//! - **Two locks (D16).** The writer mutex serializes raises, lowerings, slot-carrying commits,
//!   open and clear; the lookup lock guards the in-memory state, and lookups and plain appends
//!   take only it. Lock order: writer mutex → SB13 lease → fence → view → lookup.
//! - **Lowering** (an overwrite, a reorg, a truncation; D7, [`lower`]) moves records off-chain in
//!   batches, each with a SQLite slot below itself.
//! - **Open** reads every slot, checks read-only, rebuilds in memory, makes the winner durable,
//!   and truncates last (§3 Open, D17).

#[cfg(test)]
mod crash;
pub mod hdr;
pub(crate) mod lazy;
pub(crate) mod legacy;
mod lower;
mod recover;
pub(crate) mod runs;
mod scan;
pub mod schema;
mod step;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::hash::{BuildHasherDefault, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
};

use dash_chain::{BlockId, ChainMeta, HeaderRecord, HeaderRow, HeaderStore, Status, Work};
use dash_kv::{Cf, Commit, KvBatch, KvEngine, KvRead};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use hdr::io::{HdrFile, HdrIo, OsIo};
use hdr::slot::{Committed, Slot, SlotAt, FILE_SLOT_OFFSETS, SLOT_LEN};
use hdr::{Hdr, Rec};
use lazy::DbOpener;
use runs::{Run, Runs};

pub use recover::Recovered;
pub use schema::{Subscriber, SPV_SCHEMA};

use schema::*;

/// Upper bound of every key: deletes "from here up" end at it.
pub(crate) const END: [u8; 64] = [0xff; 64];
/// The header files' directory in the storage directory.
pub const HEADERS_DIR: &str = "block_headers";

/// A failure of the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The engine failed: the outcome of a write is unknown, and the engine refuses later ones
    /// (fail-stop, `KvEngine`).
    #[error(transparent)]
    Kv(#[from] dash_kv::StoreError),
    /// A header file call failed.
    #[error("SPV header files: {0}")]
    Io(#[from] std::io::Error),
    /// A stored row contradicts the schema or the rest of the store.
    #[error("SPV store corrupt: {0}")]
    Corrupt(String),
    /// The header files contradict their committed slot, or the slots each other (SB14 §3 Open
    /// step 2): `DiskStorageManager::new` clears the store (D19) and opens it once more.
    #[error("SPV header layout corrupt: {0}")]
    Layout(String),
    /// An earlier write or sync failed: the writer refuses until the store is reopened (D18).
    #[error("SPV store writer poisoned by an earlier failure: {0}")]
    Poisoned(String),
    /// The caller asked for something the store's contract refuses; nothing was written.
    #[error("SPV store: {0}")]
    Refused(String),
}

impl From<BadRow> for Error {
    fn from(e: BadRow) -> Self {
        Error::Corrupt(e.to_string())
    }
}

impl From<hdr::FilesError> for Error {
    fn from(e: hdr::FilesError) -> Self {
        match e {
            hdr::FilesError::Io(e) => Error::Io(e),
            hdr::FilesError::Corrupt(what) => Error::Layout(what),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<Error> for dash_chain::StoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Corrupt(what) | Error::Layout(what) => dash_chain::StoreError::Corrupt(what),
            e => dash_chain::StoreError::Io(e.to_string()),
        }
    }
}

impl From<Error> for crate::error::StorageError {
    fn from(e: Error) -> Self {
        use crate::error::StorageError;
        match e {
            Error::Kv(e) => StorageError::Io(std::io::Error::other(e.to_string())),
            Error::Io(e) => StorageError::Io(e),
            Error::Poisoned(what) => StorageError::Io(std::io::Error::other(what)),
            Error::Corrupt(what) | Error::Layout(what) => StorageError::Corruption(what),
            Error::Refused(what) => StorageError::InvalidArgument(what),
        }
    }
}

/// The lowest and highest stored height of a height-keyed table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub lo: Option<u32>,
    pub hi: Option<u32>,
}

impl Bounds {
    pub fn is_empty(&self) -> bool {
        self.hi.is_none()
    }

    /// The bounds after rows `start..=end` were written.
    fn extended(self, start: u32, end: u32) -> Bounds {
        Bounds {
            lo: Some(self.lo.map_or(start, |lo| lo.min(start))),
            hi: Some(self.hi.map_or(end, |hi| hi.max(end))),
        }
    }

    /// The bounds after every row above `h` was deleted.
    fn truncated(self, h: u32) -> Bounds {
        match self.lo {
            Some(lo) if h >= lo => Bounds {
                lo: self.lo,
                hi: self.hi.map(|hi| hi.min(h)),
            },
            _ => Bounds::default(),
        }
    }
}

/// What the store holds, for reads that must not touch the engine (the legacy traits' tip
/// queries, which cannot fail).
#[derive(Clone, Debug, Default)]
pub struct View {
    /// `meta`: SQLite's rows, with the tip, the next id and (with no rows) the anchor derived
    /// from the active chain.
    pub meta: Option<ChainMeta>,
    /// The active heights, `[start, tip]` in memory.
    pub active: Bounds,
    /// Each [`Subscriber`]'s heights, in [`Subscriber::ALL`] order.
    pub subs: [Bounds; 3],
    /// The largest id issued (SB5 n4).
    pub max_id: Option<BlockId>,
}

impl View {
    pub fn sub(&self, s: Subscriber) -> Bounds {
        self.subs[s as usize]
    }
}

/// What the store keeps of SQLite's rows: the published state of the committed database.
#[derive(Clone, Debug, Default)]
struct Rows {
    /// SQLite's `meta` rows, as committed.
    meta: Option<ChainMeta>,
    subs: [Bounds; 3],
}

/// A header hash's first eight bytes, as Core's `BlockHasher` reads them for `mapBlockIndex`.
// Core: util/hasher.h:81-87 BlockHasher, node/blockstorage.h:58 BlockMap (dash 15db478750)
#[derive(Default)]
pub(crate) struct HashPrefix(u64);

impl Hasher for HashPrefix {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut word = [0; 8];
        let n = bytes.len().min(8);
        word[..n].copy_from_slice(&bytes[..n]);
        self.0 ^= u64::from_le_bytes(word);
    }

    fn write_usize(&mut self, _: usize) {}
}

pub(crate) type ByHash = BuildHasherDefault<HashPrefix>;

/// SB13's writer lease: what only a SQLite commit changes.
#[derive(Debug, Default)]
struct Writer {
    /// A Buffered commit is newer than the last Durable one: the barrier has work.
    buffered: bool,
}

/// The writer mutex's state (D16).
struct HdrWriter<F: HdrIo> {
    /// The global slot sequence (r3 n2): the newest valid slot's.
    seq: u64,
    /// The file slot holding the newest file `seq`, and that `seq`.
    file_slot: Option<(usize, u64)>,
    /// `block_headers/tip`, once it exists.
    tip: Option<Arc<F::File>>,
    /// D18's latch (the store's).
    latch: hdr::Latch,
    /// `block_headers/` was created (by an open or a clear), or an open found no slot naming
    /// records (an earlier open may have created it and died first): the storage directory is
    /// synced at the next raise (r4 nit e(ii)).
    storage_dir: bool,
    /// A Buffered SQLite commit carried a slot or runs since the last barrier: a file-slot raise
    /// waits for the barrier (§3 Raise step 5).
    sqlite_pending: bool,
    /// The crash oracle's negative controls: syncs removed, to show their failure is detected.
    #[cfg(test)]
    skip: Skip,
}

impl<F: HdrIo> HdrWriter<F> {
    /// D18: a failed write, sync or slot-carrying commit poisons the writer; the same error is
    /// returned.
    fn poison<T, X: std::fmt::Display + Into<Error>>(
        &mut self,
        r: std::result::Result<T, X>,
    ) -> Result<T> {
        r.map_err(|e| {
            self.latch.set(&e);
            e.into()
        })
    }
}

/// What raise steps 1–3 made durable: the range a slot may name, and the runs it must carry.
struct Raised {
    committed: Committed,
    runs: Option<Runs>,
}

/// Syncs the crash oracle removes (§3 Crash tests: "the test removes the sync and asserts that
/// the failure is detected").
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Skip {
    /// Raise step 3, `block_headers/`'s sync after a file was created.
    pub dir_sync: bool,
    /// D4's storage-directory sync after the database is created.
    pub storage_sync: bool,
}

/// Where the store lives, and how its files and database are reached.
pub struct Layout<E: KvEngine, F: HdrIo> {
    /// The storage directory: `block_headers/` and the database's directory are in it.
    pub storage: PathBuf,
    pub opener: Box<dyn DbOpener<E>>,
    pub io: Arc<F>,
    /// Records a segment file holds ([`hdr::SEGMENT`] in production).
    pub per: u32,
}

/// The records a lowering batch copies at most (D7).
#[cfg(not(test))]
pub(crate) const LOWER_BATCH: u32 = 2_048;
#[cfg(test)]
pub(crate) const LOWER_BATCH: u32 = 3;

/// The SPV store over engine `E` and header files `F`.
pub struct Store<E: KvEngine, F: HdrIo = OsIo> {
    storage: PathBuf,
    opener: Box<dyn DbOpener<E>>,
    io: Arc<F>,
    db: OnceLock<E>,
    /// The writer mutex (D16).
    hw: Mutex<HdrWriter<F>>,
    /// SB13's lease: one SQLite commit at a time.
    writer: Mutex<Writer>,
    /// Write-held across each commit, after the lease; read-held across the engine reads that
    /// check the view first, so a commit cannot truncate what they read.
    fence: RwLock<()>,
    rows: RwLock<Rows>,
    /// D18's latch, shared with the writer and the lookup lock's state: every mutating entry
    /// refuses once it is set ([`halted`](Self::halted)).
    latch: hdr::Latch,
    /// The lookup lock (D16).
    hdr: Mutex<Hdr<F>>,
    /// Active ids with an override row (D21).
    overrides: Mutex<BTreeSet<u32>>,
    /// Tests: a one-shot action run at a [`Point`], where another caller could interleave.
    #[cfg(test)]
    interleave: Interleave<E, F>,
}

/// Where a caller may interleave with another one, outside every lock (tests drive the
/// schedule through [`Store::interleave`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    /// [`Store::commit_naming`], before each attempt takes the lease.
    Naming,
    /// A lowering, before batch `n` (from 0) takes the lease.
    LowerBatch(u32),
    /// [`Store::sync_wal`], after its dirty-runs check and barrier.
    SyncWalChecked,
    /// Raise steps 1–3, after the syncs.
    RaiseSynced,
    /// The fast append, before it takes the lookup lock.
    AppendUnlocked,
}

#[cfg(test)]
type Interleave<E, F> = Mutex<Option<(Point, Box<dyn FnOnce(&Store<E, F>) + Send>)>>;

impl<E: KvEngine, F: HdrIo> std::fmt::Debug for Store<E, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("view", &self.view()).finish_non_exhaustive()
    }
}

pub(crate) fn kv<T>(r: dash_kv::Result<T>) -> Result<T> {
    r.map_err(Error::Kv)
}

fn refused(what: impl Into<String>) -> Error {
    Error::Refused(what.into())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The derived row of the active record `r` at `h` with id `id` (§2): valid, regular, its skip id
/// from the runs, `work` and `time_max` derived.
fn derived_row(
    r: &Rec,
    id: u32,
    h: u32,
    skip: Option<u32>,
    work: Work,
    time_max: u32,
) -> HeaderRow {
    HeaderRow {
        hash: hdr::rec_hash(r),
        id,
        height: h,
        header: hdr::rec_header(r),
        skip,
        status: Status::VALID_TREE,
        chain_work: work.to_bendian(),
        time_max,
        kind: 0,
    }
}

/// Whether `e` is EIO (5 on every Unix the client targets): a sector that fails its ECC.
fn is_eio(e: &std::io::Error) -> bool {
    cfg!(unix) && e.raw_os_error() == Some(5)
}

impl<E: KvEngine, F: HdrIo> Store<E, F> {
    /// Opens the store (§3 Open), under the writer mutex: every slot read, the read-only checks
    /// and the rebuild, the winner made durable, then the truncation. A contradiction between the
    /// slots and the files is [`Error::Layout`], with nothing changed.
    ///
    /// Public only under `test-utils` (SB13 D47), where it skips D39's admission check and
    /// `<dir>.lock`: a test surface, not an open path ([`SpvStore::open`] is that).
    ///
    /// [`SpvStore::open`]: crate::storage::SpvStore::open
    pub fn open(layout: Layout<E, F>) -> Result<Self> {
        let Layout {
            storage,
            opener,
            io,
            per,
        } = layout;
        let dir = storage.join(HEADERS_DIR);
        // Listed first: an existing directory costs no `mkdir` (D34).
        let (created, names) = match io.list(&dir)? {
            Some(names) => (false, names),
            None => match io.create_dir(&dir)? {
                true => (true, Vec::new()),
                false => (false, io.list(&dir)?.unwrap_or_default()),
            },
        };
        let segments = hdr::segments_in(&names);
        // Step 1: every slot, before anything changes (r2 M1).
        let tip = match names.iter().any(|n| n == hdr::TIP_FILE) {
            true => io.open(&dir.join(hdr::TIP_FILE), false)?.map(Arc::new),
            false => None,
        };
        let mut slots = Vec::with_capacity(3);
        // The file slot that read back as EIO, made durably invalid in step 4 (D43).
        let mut unreadable = None;
        if let Some(f) = &tip {
            for (i, off) in FILE_SLOT_OFFSETS.iter().enumerate() {
                let mut m = [0u8; SLOT_LEN];
                // A slot a power cut tore can read back as EIO: it is invalid, as one that does
                // not decode is. Only one slot is written at a time, so both unreadable is a
                // failed file, as is any other read error (D43).
                let n = match f.read_full(&mut m, *off) {
                    Ok(n) => n,
                    Err(e) if unreadable.is_none() && is_eio(&e) => {
                        tracing::warn!(
                            "block_headers/tip slot {i} reads back as {e}: invalid (D43)"
                        );
                        unreadable = Some(i);
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                if let Some(s) = Slot::decode(&m[..n]) {
                    slots.push((SlotAt::File(i), s));
                }
            }
        }
        let db = opener.open_existing()?;
        if let Some(db) = &db {
            if let Some(m) = kv(db.get(META, meta_key::HDR_MARKER))? {
                // A SQLite row is never torn: one that does not decode is corruption.
                let s = Slot::decode(&m)
                    .ok_or_else(|| Error::Layout("meta.hdr_marker does not decode".into()))?;
                slots.push((SlotAt::Sqlite, s));
            }
        }
        let winner = hdr::slot::winner(&slots).map_err(Error::Layout)?;
        let seq = slots.iter().map(|(_, s)| s.seq).max().unwrap_or(0);
        let file_slot = slots
            .iter()
            .filter_map(|&(at, s)| match at {
                SlotAt::File(i) => Some((i, s.seq)),
                SlotAt::Sqlite => None,
            })
            .max_by_key(|&(_, seq)| seq);
        let committed = winner.map_or(Committed::Empty, |(_, s)| s.committed);
        // `block_headers/`'s own entry is durable once a raise has synced the storage directory,
        // which comes before any slot naming records. Without such a slot an earlier open may
        // have created it and died before its first raise, so the first raise syncs again.
        let storage_dir = created || committed == Committed::Empty;
        // Steps 2–3: the checks and the rebuild, read-only.
        let mut h = Hdr::open(Arc::clone(&io), dir.clone(), per, committed, &segments)?;
        let range = h.start.zip(h.tip);
        let mut overrides = BTreeSet::new();
        let mut meta = None;
        if let Some(db) = &db {
            meta = read_meta(db)?;
            let rows = kv(db.iter_prefix(HDR_RUNS, &[]).collect::<dash_kv::Result<Vec<_>>>())?;
            let rows = rows
                .iter()
                .map(|(k, v)| Run::from_row(k, v))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Error::Layout)?;
            h.runs = match (&meta, rows.is_empty()) {
                (None, true) => Runs::base(range),
                _ => Runs::stored(rows, meta.map_or(0, |m| m.next_id.0), range)
                    .map_err(Error::Layout)?,
            };
            h.offchain = kv(db.iter_prefix(HDR_HASH, &[]).count_ok())?;
            if let Some(t) = h.tip {
                for (k, v) in kv(db.iter_prefix(HDR, &[]).collect::<dash_kv::Result<Vec<_>>>())? {
                    let id = read_u32("hdr", &k)?;
                    if let Some(at) = h.runs.height_of(id, t) {
                        let row = decode_row(id, &v)?;
                        if row.height != at {
                            return Err(Error::Layout(format!(
                                "override row {id} is at {}, its run at {at}",
                                row.height
                            )));
                        }
                        if Some(at) == h.start {
                            h.anchor = Some((Work::from_bendian(row.chain_work), row.time_max));
                        }
                        overrides.insert(id);
                    }
                }
            }
        }
        // Step 4 (D17): the winner durable before anything acts on it. An unreadable slot is
        // zeroed first (D43): its `seq` is unknown, so if it read back again it could outrank
        // the winner over files truncated below it.
        if let Some(i) = unreadable {
            let f = tip.as_ref().expect("a file slot was read from it");
            f.write_all_at(&[0; SLOT_LEN], FILE_SLOT_OFFSETS[i])?;
        }
        if unreadable.is_some() || matches!(winner, Some((SlotAt::File(_), _))) {
            tip.as_ref().expect("a file slot was read from it").sync_data()?;
        }
        if let Some(db) = &db {
            let mut b = db.batch();
            if h.runs.dirty {
                put_runs(&mut b, &h.runs);
                h.runs.written();
            }
            kv(db.write_barrier(b))?;
        }
        // Step 5: truncation, last.
        h.truncate(&segments)?;
        let store = Store {
            storage,
            opener,
            io,
            db: OnceLock::new(),
            hw: Mutex::new(HdrWriter {
                seq,
                file_slot,
                tip,
                latch: h.latch.clone(),
                storage_dir,
                sqlite_pending: false,
                #[cfg(test)]
                skip: Skip::default(),
            }),
            writer: Mutex::default(),
            fence: RwLock::default(),
            rows: RwLock::default(),
            latch: h.latch.clone(),
            hdr: Mutex::new(h),
            overrides: Mutex::new(overrides),
            #[cfg(test)]
            interleave: Mutex::default(),
        };
        if let Some(db) = db {
            let _ = store.db.set(db);
        }
        let mut rows = Rows {
            meta,
            ..Rows::default()
        };
        if let Some(db) = store.db.get() {
            for s in Subscriber::ALL {
                rows.subs[s as usize] = bounds(db, s.cf())?;
            }
        }
        *store.rows_write() = rows;
        Ok(store)
    }

    /// D11's build mode: the store over v2's `block_headers/` as `committed` (V2to4's scan),
    /// read-only, with a database created in the opener's directory by the first write. No slot
    /// is read or written and nothing is truncated: `block_headers/` keeps v2's bytes until the
    /// marker step, after the build.
    pub fn open_v2(layout: Layout<E, F>, committed: Committed) -> Result<Self> {
        let Layout {
            storage,
            opener,
            io,
            per,
        } = layout;
        let dir = storage.join(HEADERS_DIR);
        let segments = match io.list(&dir)? {
            Some(names) => hdr::segments_in(&names),
            None => Vec::new(),
        };
        let h = Hdr::open(Arc::clone(&io), dir, per, committed, &segments)?;
        Ok(Store {
            storage,
            opener,
            io,
            db: OnceLock::new(),
            hw: Mutex::new(HdrWriter {
                seq: 0,
                file_slot: None,
                tip: None,
                latch: h.latch.clone(),
                storage_dir: false,
                sqlite_pending: false,
                #[cfg(test)]
                skip: Skip::default(),
            }),
            writer: Mutex::default(),
            fence: RwLock::default(),
            rows: RwLock::default(),
            latch: h.latch.clone(),
            hdr: Mutex::new(h),
            overrides: Mutex::default(),
            #[cfg(test)]
            interleave: Mutex::default(),
        })
    }

    /// Whether the database exists.
    pub fn has_db(&self) -> bool {
        self.db.get().is_some()
    }

    /// The database, if it exists.
    fn db(&self) -> Option<&E> {
        self.db.get()
    }

    /// Removes syncs for the crash oracle's negative controls.
    #[cfg(test)]
    pub(crate) fn set_skip(&self, skip: Skip) {
        self.hw().skip = skip;
    }

    /// The database, created (D4) if absent, under the writer mutex: after `create` returns, the
    /// storage directory is synced, so `spv/`'s entry is durable before any commit that carries
    /// a slot or rows (r3 N2, r4 nit c).
    fn db_create(&self, hw: &mut HdrWriter<F>) -> Result<&E> {
        if let Some(db) = self.db.get() {
            return Ok(db);
        }
        self.halted()?;
        let db = self.opener.create()?;
        #[cfg(test)]
        let skip = hw.skip.storage_sync;
        #[cfg(not(test))]
        let skip = false;
        let synced = if skip {
            Ok(())
        } else {
            self.io.sync_dir(&self.storage)
        };
        hw.poison(synced)?;
        let _ = self.db.set(db);
        Ok(self.db.get().expect("just set"))
    }

    fn hw(&self) -> MutexGuard<'_, HdrWriter<F>> {
        lock(&self.hw)
    }

    fn hdr(&self) -> MutexGuard<'_, Hdr<F>> {
        lock(&self.hdr)
    }

    fn rows_read(&self) -> RwLockReadGuard<'_, Rows> {
        self.rows.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn rows_write(&self) -> RwLockWriteGuard<'_, Rows> {
        self.rows.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn fence(&self) -> RwLockWriteGuard<'_, ()> {
        self.fence.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn fence_read(&self) -> RwLockReadGuard<'_, ()> {
        self.fence.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// SB13's lease. A write that panicked rolled nothing in, so the state is sound.
    fn lease(&self) -> MutexGuard<'_, Writer> {
        lock(&self.writer)
    }

    /// D18: refuses once the latch is set (a write or sync failed, the writer's or one under the
    /// lookup lock). Lock-free: every mutating entry checks it first.
    fn halted(&self) -> Result<()> {
        match self.latch.get() {
            Some(what) => Err(Error::Poisoned(what.to_string())),
            None => Ok(()),
        }
    }

    /// Runs the action [`interleave`](Self::interleave) set for `at`, once, holding no lock.
    #[cfg(test)]
    fn reach(&self, at: Point) {
        let action = lock(&self.interleave).take_if(|(p, _)| *p == at);
        if let Some((_, f)) = action {
            f(self);
        }
    }

    /// Interleaving points are a no-op outside tests.
    #[cfg(not(test))]
    #[inline(always)]
    fn reach(&self, _: Point) {}

    /// Runs `f` the next time a caller reaches `at`, holding no lock there.
    #[cfg(test)]
    pub(crate) fn interleave(&self, at: Point, f: impl FnOnce(&Self) + Send + 'static) {
        *lock(&self.interleave) = Some((at, Box::new(f)));
    }

    /// A subscriber's bounds as published (read under the lease where they must be current).
    fn sub_bounds(&self, s: Subscriber) -> Bounds {
        self.rows_read().subs[s as usize]
    }

    /// What the store holds.
    pub fn view(&self) -> View {
        let rows = self.rows_read().clone();
        let h = self.hdr();
        View {
            meta: chain_meta(rows.meta, &h),
            active: Bounds {
                lo: h.start,
                hi: h.tip,
            },
            subs: rows.subs,
            max_id: max_id(rows.meta.as_ref(), &h),
        }
    }

    /// Commits `batch` under the lease `w`; an engine error poisons nothing here (the engine is
    /// fail-stop itself).
    fn commit(&self, w: &mut Writer, batch: E::Batch, durability: Commit) -> Result<()> {
        // D18: every SQLite write refuses once the store is latched, an empty one too.
        self.halted()?;
        if batch.is_empty() {
            return Ok(());
        }
        let db = self.db().ok_or_else(|| refused("a commit with no database"))?;
        kv(db.write(batch, durability))?;
        w.buffered = durability == Commit::Buffered;
        Ok(())
    }

    /// A Buffered SQLite commit is newer than the last Durable one (tests: what a cut could lose).
    #[cfg(test)]
    pub(crate) fn has_buffered(&self) -> bool {
        self.lease().buffered
    }

    /// SB0 §9.4's barrier: a Durable commit of `meta.sync_seq += 1` if a Buffered commit is newer
    /// than the last Durable one, else nothing.
    fn barrier(&self) -> Result<()> {
        self.halted()?;
        let mut w = self.lease();
        if !w.buffered {
            return Ok(());
        }
        let db = self.db().expect("a Buffered commit had a database");
        let Some(mut meta) = self.rows_read().meta else {
            kv(db.sync_wal())?;
            w.buffered = false;
            return Ok(());
        };
        meta.sync_seq += 1;
        let mut b = db.batch();
        b.put(META, meta_key::SYNC_SEQ, &meta.sync_seq.to_be_bytes());
        let _fence = self.fence();
        self.commit(&mut w, b, Commit::Durable)?;
        self.rows_write().meta = Some(meta);
        Ok(())
    }

    /// Raise steps 1–3 (§3), under the writer mutex: the dirty ranges written and the snapshot
    /// taken (under the lookup lock), then the syncs and the directory syncs outside it. With
    /// `tip_file`, `block_headers/tip` is created first if missing (a file-slot raise). Returns
    /// what the slot will commit. Any failure poisons the writer (D18).
    fn raise_files(&self, hw: &mut HdrWriter<F>, tip_file: bool) -> Result<Raised> {
        self.halted()?;
        let done = (|| -> std::io::Result<Raised> {
            let mut created = false;
            if tip_file && hw.tip.is_none() {
                let dir = self.storage.join(HEADERS_DIR);
                let f = self.io.open(&dir.join(hdr::TIP_FILE), true)?.expect("created");
                hw.tip = Some(Arc::new(f));
                created = true;
            }
            let snap = self.hdr().snapshot()?;
            snap.sync()?;
            #[cfg(test)]
            let skip = hw.skip.dir_sync;
            #[cfg(not(test))]
            let skip = false;
            if (snap.created || created) && !skip {
                self.io.sync_dir(&self.storage.join(HEADERS_DIR))?;
            }
            if hw.storage_dir {
                self.io.sync_dir(&self.storage)?;
                hw.storage_dir = false;
            }
            self.reach(Point::RaiseSynced);
            // The snapshot's files share their open file descriptions with the fd cache: an
            // eviction's failed `sync_data` during the syncs above consumed the error that
            // `snap.sync()` would have seen (D18), so the latch is checked after them.
            self.hdr().check_failed()?;
            Ok(Raised {
                committed: snap.committed,
                runs: snap.runs,
            })
        })();
        hw.poison(done)
    }

    /// The next slot: a new `seq`.
    fn next_slot(hw: &mut HdrWriter<F>, committed: Committed) -> Slot {
        hw.seq += 1;
        Slot {
            seq: hw.seq,
            committed,
        }
    }

    /// Raise step 4 with the file slot: the slot not holding the newest file `seq` (r3 n2), then
    /// `sync_data`.
    fn write_file_slot(&self, hw: &mut HdrWriter<F>, committed: Committed) -> Result<()> {
        let i = match hw.file_slot {
            Some((0, _)) => 1,
            _ => 0,
        };
        let slot = Self::next_slot(hw, committed);
        let tip = Arc::clone(hw.tip.as_ref().expect("raise_files created it"));
        let done =
            tip.write_all_at(&slot.encode(), FILE_SLOT_OFFSETS[i]).and_then(|()| tip.sync_data());
        hw.poison(done)?;
        hw.file_slot = Some((i, slot.seq));
        self.hdr().committed = committed;
        Ok(())
    }

    /// Raises the committed tip to the in-memory one (§3). Runs that have not reached SQLite go
    /// with a Buffered SQLite slot (D5 (3)), which a power cut can lose; a Buffered slot or run
    /// commit is made durable first; otherwise the file slot. What must be durable when it
    /// returns (`stop`, `persist`) takes [`sync_wal`](Self::sync_wal).
    pub fn raise(&self) -> Result<()> {
        let mut hw = self.hw();
        self.raise_locked(&mut hw, Commit::Buffered)
    }

    fn raise_locked(&self, hw: &mut HdrWriter<F>, durability: Commit) -> Result<()> {
        self.halted()?;
        if self.hdr().runs.dirty {
            return self.raise_sqlite(hw, durability);
        }
        if hw.sqlite_pending {
            self.barrier()?;
            hw.sqlite_pending = false;
        }
        {
            let h = self.hdr();
            if h.committed == h.live() && !h.dirty() {
                return Ok(());
            }
        }
        let raised = self.raise_files(hw, true)?;
        // An append started a run after the check above: its row goes with the slot (D5 (3)).
        if raised.runs.is_some() {
            return self.commit_sqlite_slot(hw, raised, durability);
        }
        if raised.committed == self.hdr().committed {
            return Ok(());
        }
        self.write_file_slot(hw, raised.committed)
    }

    /// A raise through the SQLite slot, with the runs (D5 (3)).
    fn raise_sqlite(&self, hw: &mut HdrWriter<F>, durability: Commit) -> Result<()> {
        // The database, and the storage directory's sync after its creation (D4), come before
        // the raise's own syncs.
        self.db_create(hw)?;
        let raised = self.raise_files(hw, false)?;
        self.commit_sqlite_slot(hw, raised, durability)
    }

    /// Raise step 4 with the SQLite slot over `raised`, and the runs taken with its snapshot.
    fn commit_sqlite_slot(
        &self,
        hw: &mut HdrWriter<F>,
        raised: Raised,
        durability: Commit,
    ) -> Result<()> {
        let db = self.db_create(hw)?;
        let slot = Self::next_slot(hw, raised.committed);
        let mut b = db.batch();
        b.put(META, meta_key::HDR_MARKER, &slot.encode());
        if let Some(runs) = &raised.runs {
            put_runs(&mut b, runs);
            self.put_next_id(&mut b, runs.next_id);
        }
        let mut w = self.lease();
        let _fence = self.fence();
        hw.poison(self.commit(&mut w, b, durability))?;
        let mut h = self.hdr();
        h.committed = raised.committed;
        if let Some(runs) = &raised.runs {
            h.runs.written_as(runs);
        }
        hw.sqlite_pending = durability == Commit::Buffered;
        Ok(())
    }

    /// `meta.next_id` (D5): the row of SQLite's `meta`, or, on a store with no `meta` rows yet
    /// (a legacy store), every `meta` row derived from the active chain.
    fn put_next_id(&self, b: &mut E::Batch, next_id: u32) {
        let stored = self.rows_read().meta;
        match stored {
            Some(_) => b.put(META, meta_key::NEXT_ID, &next_id.to_be_bytes()),
            None => {
                let derived = chain_meta(None, &self.hdr());
                if let Some(mut m) = derived {
                    m.next_id = BlockId(m.next_id.0.max(next_id));
                    put_meta(b, &m);
                    self.rows_write().meta = Some(m);
                }
            }
        }
    }

    /// SB0 §9.4's `sync_wal`, SB14's way: the committed tip raised to the in-memory one and every
    /// SQLite commit made durable (the worker's tick, `stop`).
    pub fn sync_wal(&self) -> Result<()> {
        let mut hw = self.hw();
        self.halted()?;
        if self.hdr().runs.dirty {
            return self.raise_sqlite(&mut hw, Commit::Durable);
        }
        self.barrier()?;
        hw.sqlite_pending = false;
        self.reach(Point::SyncWalChecked);
        // Durable too: a fast append can open a run after the check above (D40).
        self.raise_locked(&mut hw, Commit::Durable)
    }

    /// Clears the header tables, the active chain too, and writes `meta`, in one Durable commit
    /// carrying an `empty` SQLite slot: the initial candidate, and a Candidate fallback (SB0
    /// §9.5). Refused once the anchor is Established, after which nothing clears a header table.
    /// The segment files go after the commit (open would delete them under the empty slot).
    pub fn reset(&self, meta: ChainMeta) -> Result<()> {
        let mut hw = self.hw();
        self.halted()?;
        if self.view().meta.is_some_and(|m| m.anchor_state == dash_chain::AnchorState::Established)
        {
            return Err(Error::Refused("reset of an Established store".into()));
        }
        let db = self.db_create(&mut hw)?;
        let mut b = db.batch();
        for cf in HEADER_CFS {
            b.delete_range(cf, &[], &END);
        }
        put_meta(&mut b, &meta);
        if let Some(best) = meta.cl.best {
            let row = [&best.height.to_be_bytes()[..], best.hash.as_bytes()].concat();
            b.put(LOCKS, LOCK_BEST, &row);
        }
        let slot = Self::next_slot(&mut hw, Committed::Empty);
        b.put(META, meta_key::HDR_MARKER, &slot.encode());
        {
            let mut w = self.lease();
            let _fence = self.fence();
            hw.poison(self.commit(&mut w, b, Commit::Durable))?;
            let mut h = self.hdr();
            h.lower(None)?;
            h.committed = Committed::Empty;
            h.runs = Runs::stored(Vec::new(), meta.next_id.0, None).expect("no rows");
            h.offchain = 0;
            drop(h);
            lock(&self.overrides).clear();
            self.rows_write().meta = Some(ChainMeta {
                cl: dash_chain::ClPersisted {
                    floor: None,
                    ..meta.cl
                },
                ..meta
            });
        }
        hw.sqlite_pending = false;
        self.remove_segments(&mut hw)
    }

    /// Deletes every segment file outside the active range (all of them on an empty store).
    fn remove_segments(&self, hw: &mut HdrWriter<F>) -> Result<()> {
        let names = hw.poison(self.io.list(&self.storage.join(HEADERS_DIR)))?.unwrap_or_default();
        let done = self.hdr().truncate(&hdr::segments_in(&names));
        hw.poison(done)
    }

    /// Sets `meta.anchor_state = Established` in one Durable transaction (SB0 §4.3.1), before
    /// any MN, lock or subscriber write.
    pub fn establish(&self) -> Result<()> {
        let Some(mut meta) = self.view().meta else {
            return Err(Error::Refused("establish before any meta".into()));
        };
        meta.anchor_state = dash_chain::AnchorState::Established;
        self.commit_naming(
            Commit::Durable,
            |s, db, _| {
                let mut b = db.batch();
                let mut runs = None;
                match s.rows_read().meta {
                    Some(_) => b.put(META, meta_key::ANCHOR_STATE, &[meta.anchor_state as u8]),
                    None => {
                        put_meta(&mut b, &meta);
                        // The first `meta.next_id` carries the base run's row (D5 (0)).
                        let h = s.hdr();
                        if h.runs.owed() {
                            put_runs(&mut b, &h.runs);
                            runs = Some(h.runs.clone());
                        }
                    }
                }
                Ok((b, None, runs))
            },
            |s, runs| {
                if let Some(runs) = runs {
                    s.hdr().runs.written_as(&runs);
                }
                let mut rows = s.rows_write();
                rows.meta = Some(match rows.meta {
                    Some(m) => ChainMeta {
                        anchor_state: meta.anchor_state,
                        ..m
                    },
                    None => meta,
                });
            },
        )
    }

    /// The store's part of D19's clear (`StorageManager::clear`), under the writer mutex: unlink
    /// `block_headers/tip` and sync `block_headers/` (step 2), then every SQLite row, Durable
    /// (step 3), then the segment files; the store stays open, empty, on its implicit base run.
    /// The caller writes the clearing intent before and `version.json` after.
    pub fn clear_all(&self) -> Result<()> {
        let mut hw = self.hw();
        self.halted()?;
        let dir = self.storage.join(HEADERS_DIR);
        hw.tip = None;
        hw.file_slot = None;
        let unlinked =
            self.io.remove(&dir.join(hdr::TIP_FILE)).and_then(|()| self.io.sync_dir(&dir));
        hw.poison(unlinked)?;
        if let Some(db) = self.db() {
            let mut b = db.batch();
            for cf in db.schema().all_cfs().filter(|&cf| cf != META) {
                b.delete_range(cf, &[], &END);
            }
            // Every `meta` row but the schema record, which an open requires.
            b.delete_range(META, &[], dash_kv::SCHEMA_KEY);
            b.delete_range(META, &[dash_kv::SCHEMA_KEY, &[0]].concat(), &END);
            let mut w = self.lease();
            let _fence = self.fence();
            hw.poison(self.commit(&mut w, b, Commit::Durable))?;
        }
        {
            let _fence = self.fence();
            *self.rows_write() = Rows::default();
            let mut h = self.hdr();
            h.lower(None)?;
            h.committed = Committed::Empty;
            h.runs = Runs::base(None);
            h.offchain = 0;
            drop(h);
            lock(&self.overrides).clear();
        }
        hw.sqlite_pending = false;
        self.remove_segments(&mut hw)
    }

    /// The `hdr` row of `id`: the active record (derived, or its override row) or an off-chain
    /// one.
    fn row(&self, id: BlockId) -> Result<Option<HeaderRow>> {
        let at = {
            let h = self.hdr();
            h.tip.and_then(|t| h.runs.height_of(id.0, t))
        };
        match at {
            Some(at) => self.active_row(at).map(Some),
            None => self.stored_row(id),
        }
    }

    /// SQLite's `hdr` row of `id`.
    fn stored_row(&self, id: BlockId) -> Result<Option<HeaderRow>> {
        match self.db() {
            Some(db) => {
                kv(db.get(HDR, &id_key(id)))?.map(|v| Ok(decode_row(id.0, &v)?)).transpose()
            }
            None => Ok(None),
        }
    }

    /// The active record at `h` (in the active range): its override row (D21), else derived.
    fn active_row(&self, h: u32) -> Result<HeaderRow> {
        let id = {
            let hd = self.hdr();
            hd.tip.and_then(|t| hd.runs.id_at(h, t))
        };
        if let Some(id) = id.filter(|id| lock(&self.overrides).contains(id)) {
            return self
                .stored_row(BlockId(id))?
                .ok_or_else(|| Error::Corrupt(format!("override row {id} missing")));
        }
        self.derived_at(h)
    }

    /// The record stored under `hash`: active (the index) or off-chain (`hdr_hash`).
    fn by_hash_row(&self, hash: &[u8; 32]) -> Result<Option<HeaderRow>> {
        let (at, offchain) = {
            let mut h = self.hdr();
            (h.find(hash)?, h.offchain)
        };
        if let Some(at) = at {
            return self.active_row(at).map(Some);
        }
        if offchain == 0 {
            return Ok(None);
        }
        let Some(db) = self.db() else {
            return Ok(None);
        };
        let Some((id, _)) = id_height_in(db, hash)? else {
            return Ok(None);
        };
        let row = self
            .stored_row(id)?
            .ok_or_else(|| Error::Corrupt(format!("hdr_hash names id {} with no row", id.0)))?;
        if row.hash != *hash {
            return Err(Error::Corrupt(format!("hdr_hash names id {} of another hash", id.0)));
        }
        Ok(Some(row))
    }

    /// The id stored under `hash`.
    fn id_of(&self, hash: &[u8; 32]) -> Result<Option<BlockId>> {
        Ok(self.by_hash_row(hash)?.map(|r| BlockId(r.id)))
    }

    /// The active id at `h`.
    fn active_id(&self, h: u32) -> Option<BlockId> {
        let hd = self.hdr();
        let t = hd.tip?;
        (hd.holds(h)).then(|| hd.runs.id_at(h, t)).flatten().map(BlockId)
    }

    /// The off-chain records of every branch from height `h` up, in `(height, id)` order.
    fn offchain_from(&self, h: u32) -> Result<Vec<HeaderRow>> {
        let Some(db) = self.db() else {
            return Ok(Vec::new());
        };
        let mut ids = Vec::new();
        let mut bad = None;
        kv(scan::for_each_from(db, HDR_H, &height_id_key(h, BlockId(0)), |k, _| {
            match trailing_id("hdr_h", k) {
                Ok(id) => ids.push(id),
                Err(e) => bad = Some(e),
            }
            bad.is_none()
        }))?;
        if let Some(e) = bad {
            return Err(e.into());
        }
        ids.iter()
            .zip(rows_in(db, &ids)?)
            .map(|(id, row)| {
                row.ok_or_else(|| Error::Corrupt(format!("no hdr row for id {}", id.0)))
            })
            .collect()
    }
}

impl Store<dash_kv_sqlite::SqliteEngine, OsIo> {
    /// The store on the OS's files in `storage`, with SQLite in `storage/spv` (production). No
    /// admission check and no lock: only `SpvStore` and tests call it
    /// (Opus-h r1 n5).
    pub(crate) fn open_dir(storage: &Path) -> Result<Self> {
        Store::open(Layout {
            storage: storage.to_path_buf(),
            opener: Box::new(lazy::SqliteOpener::new(
                &storage.join(crate::storage::migrator::STORE_DIR),
            )),
            io: Arc::new(OsIo::default()),
            per: hdr::SEGMENT,
        })
    }
}

/// `count` of an iterator of results, failing on the first error.
trait CountOk {
    fn count_ok(self) -> dash_kv::Result<u64>;
}

impl<T, I: Iterator<Item = dash_kv::Result<T>>> CountOk for I {
    fn count_ok(self) -> dash_kv::Result<u64> {
        let mut n = 0;
        for r in self {
            r?;
            n += 1;
        }
        Ok(n)
    }
}

/// The runs' rows: every row replaced (runs are few).
fn put_runs(b: &mut impl KvBatch, runs: &Runs) {
    b.delete_range(HDR_RUNS, &[], &END);
    for r in runs.runs() {
        let (k, v) = r.row();
        b.put(HDR_RUNS, &k, &v);
    }
}

/// `meta` as the store reports it: SQLite's rows, with the active tip's id, the next id over
/// every issued one, and (with no rows) the anchor at the start, Established, as the legacy
/// path's first store set it.
fn chain_meta<F: HdrIo>(stored: Option<ChainMeta>, h: &Hdr<F>) -> Option<ChainMeta> {
    let tip_id = h.tip.and_then(|t| h.runs.id_at(t, t)).map(BlockId);
    let next = h.runs.issued(h.tip);
    let mut m = match stored {
        Some(m) => m,
        None => {
            let start = h.start?;
            ChainMeta {
                schema: SPV_SCHEMA.version,
                anchor: (BlockHash::from(h.start_hash), start),
                anchor_state: dash_chain::AnchorState::Established,
                next_id: BlockId(0),
                tip: BlockId(0),
                best_header: BlockId(0),
                sync_seq: 0,
                cl: dash_chain::ClPersisted {
                    best: None,
                    floor: None,
                    enabled_observed: false,
                },
            }
        }
    };
    m.next_id = BlockId(m.next_id.0.max(next));
    if let Some(t) = tip_id {
        if stored.is_none() || m.tip != t {
            m.best_header = t;
        }
        m.tip = t;
    }
    Some(m)
}

/// The largest id issued: below `meta.next_id` and the runs' next.
fn max_id<F: HdrIo>(stored: Option<&ChainMeta>, h: &Hdr<F>) -> Option<BlockId> {
    let next = h.runs.issued(h.tip).max(stored.map_or(0, |m| m.next_id.0));
    next.checked_sub(1).map(BlockId)
}

fn read_meta(db: &impl KvRead) -> Result<Option<ChainMeta>> {
    let rows = kv(db.multi_get(META, &META_KEYS))?;
    let locks = kv(db.multi_get(LOCKS, &[LOCK_BEST, LOCK_FLOOR]))?;
    Ok(decode_meta(&rows, locks[0].as_deref(), locks[1].as_deref())?)
}

/// The lowest and highest key of a height-keyed table.
fn bounds(db: &impl KvRead, cf: Cf) -> Result<Bounds> {
    let height = |k: Option<Vec<u8>>| k.map(|k| read_u32("bounds", &k)).transpose();
    Ok(Bounds {
        lo: height(kv(scan::first_key(db, cf))?)?,
        hi: height(kv(scan::last_key(db, cf, 4))?)?,
    })
}

/// The id and height of the off-chain record stored under `hash`.
fn id_height_in(r: &impl KvRead, hash: &[u8; 32]) -> Result<Option<(BlockId, u32)>> {
    kv(r.get(HDR_HASH, hash))?.map(|v| Ok(read_hash_value(&v)?)).transpose()
}

/// The `hdr` row of each of `ids`, in their order.
fn rows_in(r: &impl KvRead, ids: &[BlockId]) -> Result<Vec<Option<HeaderRow>>> {
    let keys: Vec<[u8; 4]> = ids.iter().map(|&id| id_key(id)).collect();
    let refs: Vec<&[u8]> = keys.iter().map(|k| &k[..]).collect();
    ids.iter()
        .zip(kv(r.multi_get(HDR, &refs))?)
        .map(|(id, v)| v.map(|v| Ok(decode_row(id.0, &v)?)).transpose())
        .collect()
}

/// The `meta` rows of `m` (its ChainLock state's `locks` rows are written apart).
fn put_meta(b: &mut impl KvBatch, m: &ChainMeta) {
    for (k, v) in meta_rows(m) {
        b.put(META, k, &v);
    }
}

fn decode_record(row: &HeaderRow) -> Result<HeaderRecord> {
    HeaderRecord::from_row(row).map_err(|e| Error::Corrupt(format!("hdr row {}: {e}", row.id)))
}

impl<E: KvEngine + 'static, F: HdrIo> HeaderStore for Store<E, F> {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, dash_chain::StoreError> {
        Ok(self.by_hash_row(h.as_bytes())?.as_ref().map(decode_record).transpose()?)
    }

    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, dash_chain::StoreError> {
        Ok(self.row(id)?.as_ref().map(decode_record).transpose()?)
    }

    fn active_at(&self, height: u32) -> Result<Option<BlockId>, dash_chain::StoreError> {
        Ok(self.active_id(height))
    }

    fn from_height(&self, from: u32) -> Result<Vec<HeaderRecord>, dash_chain::StoreError> {
        let range = {
            let h = self.hdr();
            h.start.zip(h.tip).map(|(s, t)| s.max(from)..t + 1).filter(|r| !r.is_empty())
        };
        let mut rows = self.offchain_from(from)?;
        if let Some(range) = range {
            for at in range {
                rows.push(self.active_row(at)?);
            }
        }
        rows.sort_unstable_by_key(|r| (r.height, r.id));
        Ok(rows.iter().map(decode_record).collect::<Result<_>>()?)
    }

    fn leaves_from(
        &self,
        from: BlockId,
        out: &mut [(BlockId, Work)],
    ) -> Result<usize, dash_chain::StoreError> {
        let mut ids: Vec<BlockId> = match self.db() {
            Some(db) => kv(scan::collect_from(db, HDR_LEAF, &id_key(from), out.len()))?
                .iter()
                .map(|(k, _)| read_u32("hdr_leaf", k).map(BlockId))
                .collect::<Result<Vec<_>, _>>()
                .map_err(Error::from)?,
            None => Vec::new(),
        };
        ids.extend(self.active_leaves()?.into_iter().filter(|&id| id >= from));
        ids.sort_unstable_by_key(|id| id.0);
        ids.dedup();
        ids.truncate(out.len());
        for (slot, id) in out.iter_mut().zip(&ids) {
            let row = self
                .row(*id)?
                .ok_or_else(|| Error::Corrupt(format!("leaf {} has no record", id.0)))?;
            *slot = (*id, Work::from_bendian(row.chain_work));
        }
        Ok(ids.len())
    }

    fn children_from(
        &self,
        prev: &BlockHash,
        from: BlockId,
        out: &mut [BlockId],
    ) -> Result<usize, dash_chain::StoreError> {
        let mut ids = match self.db() {
            Some(db) => {
                let rows: Vec<_> = kv(db.iter_prefix(HDR_PREV, prev.as_bytes()).collect())?;
                rows.iter()
                    .map(|(k, _)| trailing_id("hdr_prev", k))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(Error::from)?
            }
            None => Vec::new(),
        };
        ids.extend(self.active_children(prev.as_bytes())?);
        ids.retain(|&id| id >= from);
        ids.sort_unstable_by_key(|id| id.0);
        ids.dedup();
        let n = ids.len().min(out.len());
        out[..n].copy_from_slice(&ids[..n]);
        Ok(n)
    }

    fn chain_meta(&self) -> Result<Option<ChainMeta>, dash_chain::StoreError> {
        Ok(self.view().meta)
    }

    fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, dash_chain::StoreError> {
        let h = self.hdr();
        let (Some(s), Some(t)) = (h.start, h.tip) else {
            return Ok(Vec::new());
        };
        Ok((s.max(lo)..=t).filter_map(|at| Some((at, BlockId(h.runs.id_at(at, t)?)))).collect())
    }

    fn max_id(&self) -> Result<Option<BlockId>, dash_chain::StoreError> {
        Ok(self.view().max_id)
    }
}

impl<E: KvEngine, F: HdrIo> Store<E, F> {
    /// The active records with no child (D12): the tip, when no off-chain record names it; on an
    /// unlinked store, a scan of the files.
    fn active_leaves(&self) -> Result<Vec<BlockId>> {
        let (linked, range, tip_hash) = {
            let h = self.hdr();
            (h.linked, h.start.zip(h.tip), h.tip_hash)
        };
        let Some((start, tip)) = range else {
            return Ok(Vec::new());
        };
        let has_offchain_child = |hash: &[u8; 32]| match self.db() {
            Some(db) => lower::has_child(db, hash),
            None => Ok(false),
        };
        if linked {
            if has_offchain_child(&tip_hash)? {
                return Ok(Vec::new());
            }
            return Ok(self.active_id(tip).into_iter().collect());
        }
        let recs = self.hdr().recs(start..tip + 1)?;
        let prevs: std::collections::HashSet<[u8; 32]> = recs.iter().map(hdr::rec_prev).collect();
        let mut out = Vec::new();
        for (i, r) in recs.iter().enumerate() {
            let hash = hdr::rec_hash(r);
            if !prevs.contains(&hash) && !has_offchain_child(&hash)? {
                out.extend(self.active_id(start + i as u32));
            }
        }
        Ok(out)
    }

    /// The active records whose `prev` is `prev`: on a linked store the one above `prev`'s
    /// height; on an unlinked store, a scan of the files (D12).
    fn active_children(&self, prev: &[u8; 32]) -> Result<Vec<BlockId>> {
        let mut h = self.hdr();
        let (Some(start), Some(tip)) = (h.start, h.tip) else {
            return Ok(Vec::new());
        };
        if h.linked {
            let Some(at) = h.find(prev)? else {
                return Ok(Vec::new());
            };
            if at == tip {
                return Ok(Vec::new());
            }
            return Ok(h.runs.id_at(at + 1, tip).map(BlockId).into_iter().collect());
        }
        let recs = h.recs(start..tip + 1)?;
        Ok(recs
            .iter()
            .enumerate()
            .filter(|(_, r)| hdr::rec_prev(r) == *prev)
            .filter_map(|(i, _)| h.runs.id_at(start + i as u32, tip).map(BlockId))
            .collect())
    }
}
