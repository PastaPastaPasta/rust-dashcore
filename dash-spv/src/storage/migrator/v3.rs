//! Storage version 4 from version 2 (SB14 D11 over SB0 §9.5 and SB13 D10), and from either
//! layout that wrote version 3.
//!
//! - **V2to4.** The version-2 headers stay in `block_headers/`, in their own record format (D1):
//!   a scan finds the first contiguous run from the first non-sentinel record, and the marker
//!   step writes `tip`'s slot A over it once the build (if any) is renamed into place. The other
//!   version-2 data is built into the SPV store in `spv.migrating/`, its last commit Durable,
//!   then renamed to `spv/`; `migration_loop` writes `version.json = 4` only after the marker is
//!   durable. A crash before the version write redoes it all from the intact version-2 folders,
//!   to the same slot. A header-only version-2 store gets no database (D4).
//! - Filter headers, filters and blocks are kept within the header heights, filter headers up
//!   to their first hole, each with its locator. The masternode message log is dropped: one
//!   qrinfo and a few diffs rebuild that state (SB0 §9.5). The metadata files move to `meta`.
//!   Peers (`peers/`) and `logs/` are not storage version data and stay.
//! - Version-2 data that does not decode is moved, intact, to `v2-unmigrated/` (`block_headers/`
//!   included: no marker exists yet), and the client starts a fresh sync. An I/O error fails the
//!   open. An interrupted set-aside resumes at the next open, and what it left is never read as
//!   version 2 (D39).
//! - **V3** goes by what is on disk (DEC-811), as two layouts wrote version 3:
//!   - SB13's unreleased SQLite store (`spv/`, `spv.migrating/`): its database is deleted (its
//!     known file names only), and an unknown entry refuses, with nothing deleted;
//!   - upstream's version 2 layout with the masternode engine snapshot in `masternodes/` (#1221):
//!     the snapshot is dropped (the masternode lists sync again), `version.json` goes back to 2,
//!     which that layout is, and V2to4 runs; a crash from there resumes as version 2;
//!   - both at once refuse, with nothing touched; neither is an empty storage path.

use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use dashcore::consensus::{encode, Decodable};

use super::{Migrator, MigratorError};
use crate::storage::store::hdr::index::HEIGHT_CAP;
use crate::storage::store::hdr::io::{HdrFile, HdrIo, OsIo};
use crate::storage::store::hdr::slot::{Committed, Slot, FILE_SLOT_OFFSETS};
use crate::storage::store::legacy::RawHeader;
use crate::storage::store::{self, Store, Subscriber};

/// The store's directory under the storage path.
pub(crate) const STORE_DIR: &str = "spv";
const MIGRATING_DIR: &str = "spv.migrating";
pub(super) const UNMIGRATED_DIR: &str = "v2-unmigrated";

/// The version-2 folders, and how many items a segment file of each holds.
const HEADERS: (&str, u32) = ("block_headers", 10_000);
const FILTER_HEADERS: (&str, u32) = ("filter_headers", 50_000);
const FILTERS: (&str, u32) = ("filters", 2_000);
const BLOCKS: (&str, u32) = ("blocks", 1_000);
const METADATA: &str = "metadata";
const MASTERNODES: &str = "masternodes";
pub(crate) const V2_FOLDERS: [&str; 6] =
    [HEADERS.0, FILTER_HEADERS.0, FILTERS.0, BLOCKS.0, METADATA, MASTERNODES];
/// The version-2 folders at version 4: `block_headers/` is the store's own (D11 (b)(2)).
const V2_FOLDERS_AT_4: [&str; 5] = [FILTER_HEADERS.0, FILTERS.0, BLOCKS.0, METADATA, MASTERNODES];

/// Rows per `WriteSet` while migrating, and the bytes at which a subscriber batch is cut.
const BATCH_ROWS: usize = 2_000;
const BATCH_BYTES: usize = 8 << 20;

/// The version-2 header sentinel: version `i32::MAX`, every other byte 0xff
/// (`segments.rs` `HashedBlockHeader::sentinel`).
fn is_sentinel_header(header: &[u8]) -> bool {
    header[..4] == i32::MAX.to_le_bytes() && header[4..80].iter().all(|&b| b == 0xff)
}

pub struct V2to4Migrator;

#[async_trait]
impl Migrator for V2to4Migrator {
    fn target(&self) -> u32 {
        4
    }

    async fn apply_migration(&self, storage_path: &Path) -> Result<(), MigratorError> {
        let path = storage_path.to_path_buf();
        tokio::task::spawn_blocking(move || migrate(&path))
            .await
            .map_err(|e| MigratorError::Io(io::Error::other(e)))?
    }
}

/// Version 3, by its layout on disk (see the module docs).
pub struct V3Migrator;

#[async_trait]
impl Migrator for V3Migrator {
    fn target(&self) -> u32 {
        4
    }

    async fn apply_migration(&self, storage_path: &Path) -> Result<(), MigratorError> {
        let path = storage_path.to_path_buf();
        tokio::task::spawn_blocking(move || migrate_v3(&path))
            .await
            .map_err(|e| MigratorError::Io(io::Error::other(e)))?
    }
}

/// The masternode engine snapshot upstream's version 3 keeps in `masternodes/` (#1221).
const UPSTREAM_ENGINE_FILE: &str = "engine.dat";

fn migrate_v3(storage_path: &Path) -> Result<(), MigratorError> {
    let sb13 =
        present(&storage_path.join(STORE_DIR))? || present(&storage_path.join(MIGRATING_DIR))?;
    let upstream = any_present(storage_path, &V2_FOLDERS)?;
    match (sb13, upstream) {
        (true, true) => Err(MigratorError::Refused(format!(
            "{} holds both version-3 layouts, an SPV store and version-2 folders",
            storage_path.display()
        ))),
        (true, false) => discard_v3(storage_path),
        (false, true) => from_upstream_v3(storage_path),
        (false, false) => Ok(()),
    }
}

/// Upstream's version 3 is version 2's layout with the engine snapshot in place of the
/// masternode message log: the snapshot is dropped, and the storage is version 2 again before
/// V2to4 builds anything, so that a crash in V2to4 is redone as from version 2.
fn from_upstream_v3(storage_path: &Path) -> Result<(), MigratorError> {
    match fs::remove_file(storage_path.join(MASTERNODES).join(UPSTREAM_ENGINE_FILE)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let version = super::version_json(2, false)?;
    crate::storage::io::atomic_write_sync(&storage_path.join("version.json"), version.as_bytes())?;
    migrate(storage_path)
}

/// The file names SQLite keeps in a store directory.
const SQLITE_FILES: [&str; 4] =
    ["kv.sqlite", "kv.sqlite-wal", "kv.sqlite-shm", "kv.sqlite-journal"];

/// Deletes `spv/` and `spv.migrating/` when each is a directory (not a link to one) holding
/// only regular files named in [`SQLITE_FILES`]; anything else refuses, with nothing deleted
/// (Sol r3 #1: a linked directory's files are outside the storage path).
fn discard_v3(storage_path: &Path) -> Result<(), MigratorError> {
    let dirs = [storage_path.join(STORE_DIR), storage_path.join(MIGRATING_DIR)];
    let refuse = |dir: &Path, what: String| {
        MigratorError::Refused(format!("version-3 store {} {what}", dir.display()))
    };
    for dir in &dirs {
        match fs::symlink_metadata(dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(refuse(dir, "is not a directory".into())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if !SQLITE_FILES.iter().any(|f| name == *f) {
                return Err(refuse(dir, format!("holds an unknown entry {name:?}")));
            }
            // `DirEntry::file_type` does not follow a link.
            if !entry.file_type()?.is_file() {
                return Err(refuse(dir, format!("holds {name:?}, which is not a regular file")));
            }
        }
    }
    for dir in &dirs {
        for f in SQLITE_FILES {
            match fs::remove_file(dir.join(f)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        match fs::remove_dir(dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    sync_dir(storage_path)?;
    Ok(())
}

/// Why a migration stopped.
enum Failure {
    /// The version-2 data does not decode.
    Corrupt(String),
    Io(io::Error),
    Store(crate::storage::store::Error),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Failure::Io(e)
    }
}

impl From<crate::storage::store::Error> for Failure {
    /// The store refusing the v2 data, or finding it inconsistent, is v2 data the client cannot
    /// use (kept aside for a fresh sync); only an engine failure fails the open.
    fn from(e: crate::storage::store::Error) -> Self {
        use crate::storage::store::Error;
        match e {
            Error::Refused(m) | Error::Corrupt(m) | Error::Layout(m) => {
                Failure::Corrupt(format!("the store: {m}"))
            }
            Error::Io(e) => Failure::Io(e),
            e @ (Error::Kv(_) | Error::Poisoned(_)) => Failure::Store(e),
        }
    }
}

fn remove_dir_if_exists(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub(super) fn migrate(storage_path: &Path) -> Result<(), MigratorError> {
    let building = storage_path.join(MIGRATING_DIR);
    let store_dir = storage_path.join(STORE_DIR);
    // A crash left a partial build, or a renamed store whose version was never written: the
    // version-2 folders are intact either way, so start over from them.
    remove_dir_if_exists(&building)?;
    remove_dir_if_exists(&store_dir)?;
    // What is left of version 2 beside an interrupted set-aside is partial: it is never read as
    // a store.
    if resume_set_aside(storage_path)? {
        return Ok(());
    }
    let run = match scan_headers(&storage_path.join(HEADERS.0)) {
        Ok(run) => run,
        Err(Failure::Corrupt(why)) => return set_aside_corrupt(storage_path, &building, &why),
        Err(Failure::Io(e)) => return Err(MigratorError::Io(e)),
        Err(Failure::Store(e)) => return Err(MigratorError::Storage(e.into())),
    };
    // `block_headers/` alone is no v2 data to build from (D11 (b)(1)).
    let mut others = false;
    for folder in V2_FOLDERS_AT_4 {
        others |= present(&storage_path.join(folder))?;
    }
    if others {
        if let Some(run) = run {
            match build(storage_path, &building, run) {
                Ok(true) => {
                    fs::rename(&building, &store_dir)?;
                    sync_dir(storage_path)?;
                }
                Ok(false) => remove_dir_if_exists(&building)?,
                Err(Failure::Corrupt(why)) => {
                    return set_aside_corrupt(storage_path, &building, &why);
                }
                Err(Failure::Io(e)) => return Err(MigratorError::Io(e)),
                Err(Failure::Store(e)) => return Err(MigratorError::Storage(e.into())),
            }
        }
    }
    // The marker step (D11 (b)(3)): with a build or without one.
    if let Some(run) = run {
        write_marker(&storage_path.join(HEADERS.0), run)?;
    }
    Ok(())
}

fn set_aside_corrupt(storage_path: &Path, building: &Path, why: &str) -> Result<(), MigratorError> {
    tracing::warn!(
        "Version-2 storage does not decode ({why}): moving it to {UNMIGRATED_DIR}/ and \
         starting a fresh sync"
    );
    remove_dir_if_exists(building)?;
    set_aside(storage_path, &V2_FOLDERS)?;
    Ok(())
}

/// The version-2 header run V2to4 keeps: from the first non-sentinel record to the end of the
/// first contiguous run from it (as `build` reads it), with its tip hash and whether it links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct V2Run {
    pub start: u32,
    pub tip: u32,
    pub tip_hash: [u8; 32],
    pub linked: bool,
}

impl V2Run {
    fn committed(self) -> Committed {
        Committed::Range {
            start: self.start,
            tip: self.tip,
            tip_hash: self.tip_hash,
            linked: self.linked,
        }
    }
}

/// Scans the version-2 headers in `dir`, segment by segment. The run's hashes must be unique
/// (the store's contract, which SB13's build enforced through `put_headers`): a repeat is v2
/// data the client cannot use, set aside before any marker names it (SB14 D41).
fn scan_headers(dir: &Path) -> Result<Option<V2Run>, Failure> {
    let mut run: Option<V2Run> = None;
    let mut hole = false;
    // Each run header's first eight hash bytes: 8 bytes a header, sorted once.
    let mut keys: Vec<u64> = Vec::new();
    for_each_item(dir, HEADERS.1, decode_header, |h, (hash, header): RawHeader| {
        // The store refuses heights from 2^24 (D3): such a run is data the client cannot use,
        // set aside as Open would otherwise clear it (Opus-h r1 m4).
        if hole {
            return Ok(());
        }
        if h >= HEIGHT_CAP {
            return Err(Failure::Corrupt(format!("a version-2 header at {h}, from 2^24 (D3)")));
        }
        match &mut run {
            None => {
                run = Some(V2Run {
                    start: h,
                    tip: h,
                    tip_hash: hash,
                    linked: true,
                })
            }
            Some(r) if r.tip + 1 != h => {
                hole = true;
                return Ok(());
            }
            Some(r) => {
                r.linked &= header[4..36] == r.tip_hash;
                r.tip = h;
                r.tip_hash = hash;
            }
        }
        keys.push(hash_key(&hash));
        Ok(())
    })?;
    if let Some(run) = run {
        check_unique(dir, run, keys)?;
    }
    Ok(run)
}

fn hash_key(hash: &[u8; 32]) -> u64 {
    u64::from_le_bytes(hash[..8].try_into().expect("8 bytes"))
}

/// Refuses a repeated hash in `run`. Keys that collide (practically never for distinct hashes)
/// are settled by a second pass over the full hashes of those records alone.
fn check_unique(dir: &Path, run: V2Run, mut keys: Vec<u64>) -> Result<(), Failure> {
    keys.sort_unstable();
    let suspects: std::collections::HashSet<u64> =
        keys.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0]).collect();
    drop(keys);
    if suspects.is_empty() {
        return Ok(());
    }
    let mut seen: std::collections::HashMap<[u8; 32], u32> = std::collections::HashMap::new();
    for_each_item(dir, HEADERS.1, decode_header, |h, (hash, _): RawHeader| {
        if !(run.start..=run.tip).contains(&h) || !suspects.contains(&hash_key(&hash)) {
            return Ok(());
        }
        match seen.insert(hash, h) {
            Some(first) => Err(Failure::Corrupt(format!(
                "header {:?} repeats at {first} and {h}",
                dash_primitives::BlockHash::from(hash)
            ))),
            None => Ok(()),
        }
    })
}

/// The marker step (D11 (b)(3)–(4)): the run's segment files synced, slot A written with `seq`
/// 1, `tip` synced, and `block_headers/` synced. The same run gives the same bytes on a redo.
fn write_marker(dir: &Path, run: V2Run) -> io::Result<()> {
    let io = OsIo::default();
    for id in run.start / HEADERS.1..=run.tip / HEADERS.1 {
        let f = io
            .open(&dir.join(store::hdr::segment_name(id)), false)?
            .ok_or_else(|| io::Error::other(format!("v2 header segment {id} vanished")))?;
        f.sync_data()?;
    }
    let tip = io.open(&dir.join(store::hdr::TIP_FILE), true)?.expect("created");
    let slot = Slot {
        seq: 1,
        committed: run.committed(),
    };
    tip.write_all_at(&slot.encode(), FILE_SLOT_OFFSETS[0])?;
    tip.sync_data()?;
    sync_dir(dir)
}

/// Makes the renames in `dir` durable (Unix, as `atomic_write`): Windows opens no directory as
/// a file, and NTFS journals its renames.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Whether `path` names an entry, a dangling symlink included. Only `NotFound` is absent: any
/// other error propagates, so a failed lookup never passes for "nothing there" on a path that
/// may delete (D42).
fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether any version-2 folder but `block_headers/` is at the storage path; a failed lookup is an error, not "none"
/// (D42).
pub(super) fn any_v2_folder(storage_path: &Path) -> io::Result<bool> {
    any_present(storage_path, &V2_FOLDERS_AT_4)
}

/// Whether any of `folders` is at the storage path; a failed lookup is an error, not "none" (D42).
fn any_present(storage_path: &Path, folders: &[&str]) -> io::Result<bool> {
    for folder in folders {
        if present(&storage_path.join(folder))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Finishes a set-aside a crash or an I/O error interrupted (D39): when `v2-unmigrated/` exists,
/// every version-2 folder still at the storage path joins it, and `true` is returned. The
/// migration calls it before reading version-2 data; on an error it stops, having deleted
/// nothing.
fn resume_set_aside(storage_path: &Path) -> io::Result<bool> {
    if !present(&storage_path.join(UNMIGRATED_DIR))? {
        return Ok(false);
    }
    tracing::warn!("Resuming the set-aside of version-2 storage to {UNMIGRATED_DIR}/");
    set_aside(storage_path, &V2_FOLDERS)?;
    Ok(true)
}

/// Moves every version-2 folder at the storage path into `v2-unmigrated/`, whether or not a
/// set-aside had begun: the fallback that clears unreadable storage calls it before it deletes
/// anything, so no path deletes version-2 data (D42, BL-186). On an error the fallback stops,
/// having deleted nothing.
///
/// `version`: the version `version.json` holds, `None` when it does not parse. Below version 4,
/// `block_headers/` is version-2 data and goes too (D11 (b)(2)). At version 4 it is the store's
/// own and stays. With no version to read, it is the store's only when it holds a `tip`, which
/// version 2 never writes (D11 (d)), and no other version-2 folder is beside it: the marker
/// step can leave a `tip` over version-2 segments before the version write, and no store opens
/// while those folders remain. A young store's headers with no `tip` yet are set aside too:
/// that keeps more than it must, never less.
pub(super) fn set_aside_before_clear(storage_path: &Path, version: Option<u32>) -> io::Result<()> {
    let headers = match version {
        Some(version) => version < super::CURRENT_VERSION,
        None => {
            !present(&storage_path.join(HEADERS.0).join(store::hdr::TIP_FILE))?
                || any_v2_folder(storage_path)?
        }
    };
    let folders: &[&str] = if headers {
        &V2_FOLDERS
    } else {
        &V2_FOLDERS_AT_4
    };
    if !any_present(storage_path, folders)? {
        return Ok(());
    }
    tracing::warn!("Moving version-2 storage to {UNMIGRATED_DIR}/ before clearing");
    set_aside(storage_path, folders)
}

/// Moves every version-2 folder still at the storage path into `v2-unmigrated/` (D39).
///
/// - The directory is durable before the first move: it records the intent, so [`migrate`]
///   resumes an interrupted set-aside rather than building a store from what is left.
/// - Nothing is deleted: a folder whose name is taken in the aside takes the next free
///   `<folder>.<n>`.
/// - The moves are durable before the version write: else a power loss could keep the version
///   and lose a move.
fn set_aside(storage_path: &Path, folders: &[&str]) -> io::Result<()> {
    let aside = storage_path.join(UNMIGRATED_DIR);
    if !present(&aside)? {
        fs::create_dir(&aside)?;
        sync_dir(storage_path)?;
    }
    for &folder in folders {
        let from = storage_path.join(folder);
        if !present(&from)? {
            continue;
        }
        let mut to = aside.join(folder);
        for n in 1.. {
            if !present(&to)? {
                break;
            }
            to = aside.join(format!("{folder}.{n}"));
        }
        fs::rename(&from, &to)?;
    }
    sync_dir(&aside)?;
    sync_dir(storage_path)
}

/// The version-2 folders a completed migration left (a crash after the version write): deleted
/// only when the store holds their data, else set aside (D39).
pub(crate) fn remove_v2_leftovers(storage_path: &Path) -> io::Result<()> {
    remove_dir_if_exists(&storage_path.join(MIGRATING_DIR))?;
    if !any_v2_folder(storage_path)? {
        return Ok(());
    }
    if !present(&storage_path.join(STORE_DIR))? {
        tracing::warn!(
            "Version-2 folders beside version 4 without a store: moving them to {UNMIGRATED_DIR}/"
        );
        return set_aside(storage_path, &V2_FOLDERS_AT_4);
    }
    for folder in V2_FOLDERS_AT_4 {
        remove_dir_if_exists(&storage_path.join(folder))?;
    }
    Ok(())
}

/// Builds the SPV store from the version-2 data but the headers (D11 (b)(1)), over the header
/// run `run` read in place. `false`: nothing was kept (no database was created).
fn build(storage_path: &Path, building: &Path, run: V2Run) -> Result<bool, Failure> {
    // What a completed build lets `remove_v2_leftovers` delete must be what it read (D39).
    for folder in [FILTER_HEADERS.0, FILTERS.0, BLOCKS.0] {
        owned_segments(&storage_path.join(folder))?;
    }
    let store = Store::open_v2(
        store::Layout {
            storage: storage_path.to_path_buf(),
            opener: Box::new(store::lazy::SqliteOpener::new(building)),
            io: std::sync::Arc::new(OsIo::default()),
            per: HEADERS.1,
        },
        run.committed(),
    )?;
    let (lo, hi) = (run.start, run.tip);

    // The subscribers, within the header heights; filter headers up to their first hole, from
    // the one checkpoint sync stores below a checkpoint anchor (`previous_filter_header`, at
    // `lo - 1`), which the first filter batch's verification reads.
    let mut hole = false;
    let mut expect = None;
    let mut batch = SubBatch::new(&store, Subscriber::FilterHeaders);
    for_each_item(
        &storage_path.join(FILTER_HEADERS.0),
        FILTER_HEADERS.1,
        decode_filter_header,
        |h, item| {
            if hole || h < lo.saturating_sub(1) || h > hi || expect.is_some_and(|e| e != h) {
                hole |= expect.is_some();
                return Ok(());
            }
            expect = Some(h + 1);
            batch.push(h, item)
        },
    )?;
    batch.flush()?;
    for (folder, sub, decode) in [
        (FILTERS, Subscriber::Filters, decode_filter as Decode<Vec<u8>>),
        (BLOCKS, Subscriber::Blocks, decode_block as Decode<Vec<u8>>),
    ] {
        let mut batch = SubBatch::new(&store, sub);
        for_each_item(&storage_path.join(folder.0), folder.1, decode, |h, item| {
            match (lo..=hi).contains(&h) {
                true => batch.push(h, item),
                false => Ok(()),
            }
        })?;
        batch.flush()?;
    }

    migrate_metadata(&store, &storage_path.join(METADATA))?;
    store.sync_wal()?;
    Ok(store.has_db())
}

/// SB14 D39's ownership rule for the folders a completed build deletes: an entry it may delete
/// unread is one that never held v2 data. That is a leftover of v2's atomic write
/// (`.<name>.<pid>.<n>.tmp`, never the value), or an OS artefact.
fn disposable(name: &str) -> bool {
    (name.starts_with('.') && name.ends_with(".tmp"))
        || matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini")
}

/// Checks that a segment folder holds only regular segment files the build reads (the names
/// Open reads) and [`disposable`] entries. Anything else is v2 data the build would leave for
/// `remove_v2_leftovers` to delete, so the tree is set aside (D39; Opus-h r2 m3).
fn owned_segments(dir: &Path) -> Result<(), Failure> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str();
        if name.is_some_and(disposable) {
            continue;
        }
        if name.and_then(store::hdr::segment_of).is_none() || !entry.file_type()?.is_file() {
            return Err(Failure::Corrupt(format!(
                "{} holds {:?}, which is not a segment file",
                dir.display(),
                entry.file_name()
            )));
        }
    }
    Ok(())
}

fn migrate_metadata<E: dash_kv::KvEngine>(store: &Store<E>, dir: &Path) -> Result<(), Failure> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str();
        if name.is_some_and(disposable) {
            continue;
        }
        // Anything else that is not a `<key>.dat` file is v2 data this migration would delete
        // with `metadata/`: the tree is set aside instead (D39; Sol r3 #2).
        let key = name.and_then(|n| n.strip_suffix(".dat"));
        let Some(key) = key.filter(|_| entry.file_type().is_ok_and(|t| t.is_file())) else {
            return Err(Failure::Corrupt(format!(
                "{METADATA}/ holds {:?}, which is not a value file",
                entry.file_name()
            )));
        };
        store.put_metadata(key, &fs::read(entry.path())?)?;
    }
    Ok(())
}

/// A subscriber's rows, cut into contiguous `WriteSet`s of bounded size.
struct SubBatch<'s, E: dash_kv::KvEngine> {
    store: &'s Store<E>,
    sub: Subscriber,
    start: u32,
    rows: Vec<Vec<u8>>,
    bytes: usize,
}

impl<'s, E: dash_kv::KvEngine> SubBatch<'s, E> {
    fn new(store: &'s Store<E>, sub: Subscriber) -> Self {
        SubBatch {
            store,
            sub,
            start: 0,
            rows: Vec::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, h: u32, row: Vec<u8>) -> Result<(), Failure> {
        if self.rows.len() == BATCH_ROWS
            || self.bytes >= BATCH_BYTES
            || (!self.rows.is_empty() && self.start + self.rows.len() as u32 != h)
        {
            self.flush()?;
        }
        if self.rows.is_empty() {
            self.start = h;
        }
        self.bytes += row.len();
        self.rows.push(row);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Failure> {
        let refs: Vec<&[u8]> = self.rows.iter().map(Vec::as_slice).collect();
        self.store.put_sub(self.sub, self.start, &refs)?;
        self.rows.clear();
        self.bytes = 0;
        Ok(())
    }
}

/// Decodes one item at the cursor: `None` for a sentinel (an empty slot), an error when the
/// bytes do not decode. Running out of bytes is `UnexpectedEof`, which ends the file, as
/// `Segment::load` reads it.
type Decode<T> = fn(&mut Cursor<&[u8]>) -> Result<Option<T>, encode::Error>;

fn take<const N: usize>(c: &mut Cursor<&[u8]>) -> Result<[u8; N], encode::Error> {
    let mut out = [0; N];
    io::Read::read_exact(c, &mut out)?;
    Ok(out)
}

fn decode_header(c: &mut Cursor<&[u8]>) -> Result<Option<RawHeader>, encode::Error> {
    let header: [u8; 80] = take(c)?;
    let hash: [u8; 32] = take(c)?;
    Ok((!is_sentinel_header(&header)).then_some((hash, header)))
}

fn decode_filter_header(c: &mut Cursor<&[u8]>) -> Result<Option<Vec<u8>>, encode::Error> {
    let fh: [u8; 32] = take(c)?;
    Ok((fh != [0; 32]).then(|| fh.to_vec()))
}

fn decode_filter(c: &mut Cursor<&[u8]>) -> Result<Option<Vec<u8>>, encode::Error> {
    let filter = Vec::<u8>::consensus_decode(c)?;
    Ok((!filter.is_empty()).then_some(filter))
}

/// A block is stored as its version-2 item, `hash ‖ block`.
fn decode_block(c: &mut Cursor<&[u8]>) -> Result<Option<Vec<u8>>, encode::Error> {
    let at = c.position() as usize;
    let block = crate::types::HashedBlock::consensus_decode(c)?;
    let header = dashcore::consensus::serialize(&block.block().header);
    let raw = &c.get_ref()[at..c.position() as usize];
    Ok((!is_sentinel_header(&header)).then(|| raw.to_vec()))
}

/// The segment ids in `dir`, ascending.
fn segment_ids(dir: &Path) -> io::Result<Vec<u32>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        // Exactly the names Open reads (six digits, Opus-h r1 n4).
        ids.extend(name.to_str().and_then(store::hdr::segment_of));
    }
    ids.sort_unstable();
    Ok(ids)
}

/// Calls `f` with each stored item of the segment files in `dir` and its height, in height
/// order. One segment file is in memory at a time.
fn for_each_item<T>(
    dir: &Path,
    per_segment: u32,
    decode: Decode<T>,
    mut f: impl FnMut(u32, T) -> Result<(), Failure>,
) -> Result<(), Failure> {
    for id in segment_ids(dir)? {
        let path: PathBuf = dir.join(format!("segment_{id:06}.dat"));
        let bytes = fs::read(&path)?;
        let mut cursor = Cursor::new(&bytes[..]);
        for offset in 0..per_segment {
            let item = match decode(&mut cursor) {
                Ok(item) => item,
                Err(encode::Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(Failure::Corrupt(format!("{}: {e}", path.display()))),
            };
            let height = id
                .checked_mul(per_segment)
                .and_then(|h| h.checked_add(offset))
                .ok_or_else(|| Failure::Corrupt(format!("{}: height overflow", path.display())))?;
            if let Some(item) = item {
                f(height, item)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::StorageMigrator;
    use super::*;
    use crate::test_utils::v2_fixture::*;
    use crate::types::HashedBlock;
    use dashcore_hashes::{sha256, Hash};

    /// The migrated store at the storage path `dir` (SB14 Open, as `SpvStore::open` does).
    fn open(dir: &Path) -> Store<dash_kv_sqlite::SqliteEngine> {
        Store::open_dir(dir).unwrap()
    }

    /// The version-2 layout (its reader was checked against the real writer in 4ce7e14d3):
    /// a checkpoint start, segment edges, sentinel padding above the tip, a hole that ends
    /// the headers, sparse filters and blocks.
    #[test]
    fn v2_segments_migrate() {
        let dir = tempfile::tempdir().unwrap();
        let (lo, hi) = (19_990, 30_005);
        V2Fixture {
            lo,
            hi,
            filter_every: 3,
            blocks: vec![lo + 1, lo + 2_500, hi + 7],
        }
        .write(dir.path())
        .unwrap();
        // Headers past a hole (as a truncation that left a stale higher segment would) are
        // dropped.
        let mut stale = sentinel_header().to_vec();
        stale.extend_from_slice(&[0; 32]);
        write_segments(
            &dir.path().join(HEADERS.0),
            HEADERS_PER_SEGMENT,
            [40_000],
            |h| Some([&synthetic_header(h)[..], &synthetic_hash(h)[..]].concat()),
            &stale,
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("peers")).unwrap();
        migrate(dir.path()).unwrap();
        assert!(!dir.path().join(MIGRATING_DIR).exists());
        assert!(dir.path().join("peers").exists(), "peers are not storage-version data");
        let s = open(dir.path());
        let v = s.view();
        let keep = hi - 1;
        assert_eq!((v.active.lo, v.active.hi), (Some(lo), Some(keep)));
        let m = v.meta.unwrap();
        assert_eq!(m.anchor, (synthetic_hash(lo).into(), lo));
        assert_eq!(m.anchor_state, dash_chain::AnchorState::Established);
        for h in [lo, 19_999, 20_000, 29_999, 30_000, keep] {
            assert_eq!(s.headers(h..h + 1).unwrap(), [(synthetic_hash(h), synthetic_header(h))]);
        }
        // Linked synthetic headers: the leaf is the tip alone.
        let mut leaves = [(dash_chain::BlockId(0), dash_chain::Work::from_u64(0)); 4];
        assert_eq!(
            dash_chain::HeaderStore::leaves_from(&s, dash_chain::BlockId(0), &mut leaves).unwrap(),
            1
        );
        let fh = Subscriber::FilterHeaders;
        let bounds = |lo, hi| crate::storage::store::Bounds {
            lo: Some(lo),
            hi: Some(hi),
        };
        // From the checkpoint's previous filter header, below the anchor.
        assert_eq!(s.view().sub(fh), bounds(lo - 1, keep));
        assert_eq!(s.sub_get(fh, lo - 1).unwrap(), Some(synthetic_filter_header(lo - 1).to_vec()));
        assert_eq!(s.locator(fh).unwrap(), Some((synthetic_hash(keep), keep)));
        assert_eq!(s.sub_get(fh, 25_000).unwrap(), Some(synthetic_filter_header(25_000).to_vec()));
        let f = Subscriber::Filters;
        // Filters at every third height from `lo`: the last one is `keep` itself.
        assert_eq!(s.view().sub(f), bounds(lo, keep));
        assert_eq!(s.sub_get(f, lo).unwrap(), Some(lo.to_le_bytes().to_vec()));
        assert_eq!(s.sub_get(f, lo + 1).unwrap(), None);
        let b = Subscriber::Blocks;
        assert_eq!(
            s.view().sub(b),
            bounds(lo + 1, lo + 2_500),
            "a block above the headers is dropped"
        );
        let raw = s.sub_get(b, lo + 2_500).unwrap().unwrap();
        let block: HashedBlock = dashcore::consensus::deserialize(&raw).unwrap();
        assert_eq!(*block.hash().as_byte_array(), synthetic_hash(lo + 2_500));
        assert_eq!(s.metadata("last_target_height").unwrap().as_deref(), Some(&b"123"[..]));
        assert_eq!(s.mn_log_rows().unwrap(), 0, "the masternode log is dropped");
        assert_eq!(s.recover().unwrap(), [crate::storage::store::Recovered::Kept; 3]);
    }

    fn small(dir: &Path) {
        V2Fixture {
            lo: 0,
            hi: 3_000,
            filter_every: 1,
            blocks: vec![5],
        }
        .write(dir)
        .unwrap();
    }

    #[test]
    fn a_crashed_build_is_redone_and_corrupt_v2_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        small(dir.path());
        // A crash left a partial build and a renamed store without the version write.
        fs::create_dir_all(dir.path().join(MIGRATING_DIR).join("junk")).unwrap();
        fs::create_dir_all(dir.path().join(STORE_DIR).join("junk")).unwrap();
        migrate(dir.path()).unwrap();
        assert_eq!(open(dir.path()).view().active.hi, Some(2_999));
        remove_v2_leftovers(dir.path()).unwrap();
        // `block_headers/` is the store's own, with its marker (D11 (b)(2)).
        assert!(dir.path().join(HEADERS.0).join("tip").exists());
        assert!(V2_FOLDERS_AT_4.iter().all(|f| !dir.path().join(f).exists()));

        let dir = tempfile::tempdir().unwrap();
        small(dir.path());
        corrupt_filters(dir.path());
        migrate(dir.path()).unwrap();
        assert!(!dir.path().join(STORE_DIR).exists(), "a fresh sync");
        let aside = dir.path().join(UNMIGRATED_DIR);
        for folder in [HEADERS.0, FILTERS.0, METADATA, MASTERNODES] {
            assert!(aside.join(folder).exists(), "{folder} kept intact");
            assert!(!dir.path().join(folder).exists());
        }

        // A crash midway through the moves: what was set aside stays, the rest joins it.
        let dir = tempfile::tempdir().unwrap();
        small(dir.path());
        corrupt_filters(dir.path());
        let aside = dir.path().join(UNMIGRATED_DIR);
        fs::create_dir_all(&aside).unwrap();
        fs::rename(dir.path().join(METADATA), aside.join(METADATA)).unwrap();
        migrate(dir.path()).unwrap();
        for folder in [HEADERS.0, FILTERS.0, METADATA, MASTERNODES] {
            assert!(aside.join(folder).exists(), "{folder} kept after the interrupted moves");
        }
    }

    /// Writes the fixture's headers with `hash_at(h)` as each record's stored hash.
    fn headers_with(dir: &Path, hash_at: impl Fn(u32) -> [u8; 32]) {
        let mut sentinel = sentinel_header().to_vec();
        sentinel.extend_from_slice(&[0; 32]);
        write_segments(
            &dir.join(HEADERS.0),
            HEADERS_PER_SEGMENT,
            0..3_000,
            |h| Some([&synthetic_header(h)[..], &hash_at(h)[..]].concat()),
            &sentinel,
        )
        .unwrap();
    }

    /// SB14 D41: a hash repeated in the retained run (within one batch and across two, on a store
    /// with other v2 data, and Sol r1's header-only case: record 1 holding record 0's hash) is
    /// v2 data the client cannot use. It is set aside before any marker is written, and the
    /// store starts empty.
    #[test]
    fn a_repeated_v2_header_hash_is_set_aside() {
        for (repeat_at, of, header_only) in
            [(10, 5, false), (BATCH_ROWS as u32 + 5, 5, false), (1, 0, true)]
        {
            let dir = tempfile::tempdir().unwrap();
            small(dir.path());
            if header_only {
                for f in V2_FOLDERS_AT_4 {
                    remove_dir_if_exists(&dir.path().join(f)).unwrap();
                }
            }
            headers_with(dir.path(), |h| {
                synthetic_hash(if h == repeat_at {
                    of
                } else {
                    h
                })
            });
            migrate(dir.path()).unwrap();
            assert!(!dir.path().join(STORE_DIR).exists(), "{repeat_at}");
            assert!(dir.path().join(UNMIGRATED_DIR).join(HEADERS.0).exists(), "{repeat_at}");
            assert!(!dir.path().join(HEADERS.0).join("tip").exists(), "no marker: {repeat_at}");
            assert_eq!(open(dir.path()).view().active.hi, None, "{repeat_at}");
        }
    }

    /// Two distinct hashes sharing their first eight bytes (the scan's key) are no repeat: the
    /// second pass compares the full hashes.
    #[test]
    fn a_shared_hash_prefix_is_no_repeat() {
        let dir = tempfile::tempdir().unwrap();
        small(dir.path());
        headers_with(dir.path(), |h| {
            let mut hash = synthetic_hash(h);
            if h == 7 {
                hash = synthetic_hash(3);
                hash[31] ^= 1;
            }
            hash
        });
        migrate(dir.path()).unwrap();
        assert!(!dir.path().join(UNMIGRATED_DIR).exists());
        assert_eq!(open(dir.path()).view().active.hi, Some(2_999));
    }

    /// The digest of every file of the version-2 folders under `dir`, by its path relative to
    /// `dir`.
    fn v2_files(dir: &Path) -> BTreeMap<PathBuf, sha256::Hash> {
        let mut files = BTreeMap::new();
        let mut dirs: Vec<PathBuf> = V2_FOLDERS.iter().map(|f| dir.join(f)).collect();
        while let Some(d) = dirs.pop() {
            let Ok(entries) = fs::read_dir(&d) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    let rel = path.strip_prefix(dir).unwrap().to_path_buf();
                    files.insert(rel, sha256::Hash::hash(&fs::read(&path).unwrap()));
                }
            }
        }
        files
    }

    fn copy_tree(from: &Path, to: &Path) {
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let dest = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                fs::create_dir(&dest).unwrap();
                copy_tree(&entry.path(), &dest);
            } else {
                fs::copy(entry.path(), dest).unwrap();
            }
        }
    }

    fn corrupt_filters(dir: &Path) {
        // A varint that is not canonical does not decode.
        fs::write(dir.join(FILTERS.0).join("segment_000000.dat"), [0xfd, 0x01, 0x00]).unwrap();
    }

    /// Runs the migration with `blocks/` unwritable by its owner, so that moving it to another
    /// parent fails `rename(2)` with EACCES (root is exempt, and there the migration succeeds),
    /// then restores the permissions wherever the folder ended.
    #[cfg(unix)]
    async fn migrate_with_unmovable_blocks(path: &Path) -> crate::error::StorageResult<()> {
        use std::os::unix::fs::PermissionsExt;
        let set_mode = |dir: &Path, mode| {
            fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
        };
        let blocked = path.join(BLOCKS.0);
        set_mode(&blocked, 0o555);
        let result = StorageMigrator::migrate(path).await;
        for dir in [blocked, path.join(UNMIGRATED_DIR).join(BLOCKS.0)] {
            if dir.exists() {
                set_mode(&dir, 0o755);
            }
        }
        result
    }

    /// The version-2 files at the storage path and in `v2-unmigrated/`, as one map.
    #[cfg(unix)]
    fn v2_files_at_root_and_aside(path: &Path) -> BTreeMap<PathBuf, sha256::Hash> {
        let mut files = v2_files(path);
        files.extend(v2_files(&path.join(UNMIGRATED_DIR)));
        files
    }

    /// A set-aside interrupted after any subset of its moves (a crash between two), resumed
    /// through the public path: every version-2 folder ends in the aside byte for byte, and what
    /// was left is neither migrated nor deleted (D39). Each open runs the migration and the
    /// leftover cleanup.
    ///
    /// Each case runs again with `version.json` unreadable as well (unparsable), so the fallback
    /// that clears unreadable storage meets the interrupted set-aside: it finishes the set-aside
    /// before it clears anything (D42). In odd cases it is a version newer than this client
    /// instead, which is refused with nothing moved or deleted (Opus-h r1 n2). Case 0 is an empty
    /// marker.
    #[tokio::test]
    async fn an_interrupted_set_aside_loses_no_folder() {
        let template = tempfile::tempdir().unwrap();
        small(template.path());
        let want = v2_files(template.path());
        assert_eq!(want.len(), 7, "the fixture's segment and metadata files");
        for unreadable_version in [false, true] {
            for moved in 0..1u32 << V2_FOLDERS.len() {
                let case = format!("moved {moved:06b}, unreadable version {unreadable_version}");
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().to_path_buf();
                copy_tree(template.path(), &path);
                let aside = path.join(UNMIGRATED_DIR);
                fs::create_dir(&aside).unwrap();
                for (i, folder) in V2_FOLDERS.iter().enumerate() {
                    if moved & 1 << i != 0 {
                        fs::rename(path.join(folder), aside.join(folder)).unwrap();
                    }
                }
                if unreadable_version {
                    let newer = moved % 2 == 1;
                    let bytes: &[u8] = if newer {
                        br#"{"version": 99}"#
                    } else {
                        b"not json"
                    };
                    fs::write(path.join("version.json"), bytes).unwrap();
                    if newer {
                        let before = (v2_files(&path), v2_files(&aside));
                        assert!(StorageMigrator::migrate(&path).await.is_err(), "{case}");
                        assert_eq!((v2_files(&path), v2_files(&aside)), before, "{case}");
                        continue;
                    }
                }
                for open in 0..2 {
                    StorageMigrator::migrate(&path).await.unwrap();
                    assert_eq!(v2_files(&aside), want, "{case}, open {open}");
                    assert!(V2_FOLDERS.iter().all(|f| !path.join(f).exists()), "{case}");
                    assert!(!path.join(STORE_DIR).exists(), "{case}: not a partial migration");
                }
            }
        }
    }

    /// The fallback that clears unreadable storage, meeting an interrupted set-aside it cannot
    /// finish: it fails and deletes nothing, and the next open finishes (D42). The aside is
    /// unwritable, so no folder can join it while every folder at the root stays deletable: a
    /// fallback that went on clearing would delete them. Root ignores the mode and finishes at
    /// once.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_corruption_fallback_deletes_nothing_when_the_set_aside_fails() {
        use crate::error::StorageError;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        let want = v2_files(&path);
        let aside = path.join(UNMIGRATED_DIR);
        fs::create_dir(&aside).unwrap();
        fs::rename(path.join(HEADERS.0), aside.join(HEADERS.0)).unwrap();
        fs::write(path.join("version.json"), b"not json").unwrap();
        let mode = |m| fs::set_permissions(&aside, fs::Permissions::from_mode(m)).unwrap();
        mode(0o555);
        let first = StorageMigrator::migrate(&path).await;
        mode(0o755);
        assert_eq!(v2_files_at_root_and_aside(&path), want, "nothing deleted ({first:?})");
        match first {
            Err(StorageError::Io(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
                assert!(V2_FOLDERS[1..].iter().all(|f| path.join(f).exists()), "none moved");
                StorageMigrator::migrate(&path).await.unwrap();
            }
            Ok(()) => eprintln!("rename(2) ignored the mode here (root, or a platform that does)"),
            Err(e) => panic!("{e}"),
        }
        assert_eq!(v2_files(&aside), want);
        assert!(V2_FOLDERS.iter().all(|f| !path.join(f).exists()));
    }

    /// An I/O error midway through the set-aside fails the open and loses nothing; the next open
    /// finishes it (D39). A directory its owner cannot write cannot move to another parent
    /// (rename(2) EACCES: its `..` entry); root is exempt, and there the first open succeeds.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_io_error_midway_through_the_set_aside_loses_no_folder() {
        use crate::error::StorageError;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        corrupt_filters(&path);
        let want = v2_files(&path);
        let aside = path.join(UNMIGRATED_DIR);
        let first = migrate_with_unmovable_blocks(&path).await;
        match first {
            Err(StorageError::Io(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
                // The folders before `blocks` moved, the rest stayed, and nothing is lost.
                let (moved, left) = V2_FOLDERS.split_at(3);
                assert!(moved.iter().all(|f| aside.join(f).exists() && !path.join(f).exists()));
                assert!(left.iter().all(|f| path.join(f).exists() && !aside.join(f).exists()));
                assert_eq!(v2_files_at_root_and_aside(&path), want);
                StorageMigrator::migrate(&path).await.unwrap();
            }
            Ok(()) => eprintln!("rename(2) ignored the mode here (root, or a platform that does)"),
            Err(e) => panic!("{e}"),
        }
        assert_eq!(v2_files(&aside), want);
        assert!(V2_FOLDERS.iter().all(|f| !path.join(f).exists()));
    }

    /// Version-2 folders beside version 4 without a store (none of their data is in it) are set
    /// aside, never deleted (D39); a name the aside already holds takes the next free one.
    /// `block_headers/` is the store's own at version 4 and stays (D11 (b)(2)).
    #[tokio::test]
    async fn leftovers_without_a_store_are_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        let headers = path.join(HEADERS.0);
        let want: BTreeMap<_, _> =
            v2_files(&path).into_iter().filter(|(f, _)| !f.starts_with(HEADERS.0)).collect();
        StorageMigrator::write_current_version(&path).await.unwrap();
        let aside = path.join(UNMIGRATED_DIR);
        fs::create_dir_all(aside.join(FILTERS.0)).unwrap();
        fs::write(aside.join(FILTERS.0).join("older"), b"kept").unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert!(V2_FOLDERS_AT_4.iter().all(|f| !path.join(f).exists()));
        assert!(headers.exists());
        assert_eq!(fs::read(aside.join(FILTERS.0).join("older")).unwrap(), b"kept");
        let renamed = format!("{}.1", FILTERS.0);
        fs::rename(aside.join(FILTERS.0), aside.join("older-filters")).unwrap();
        fs::rename(aside.join(&renamed), aside.join(FILTERS.0)).unwrap();
        assert_eq!(v2_files(&aside), want);
    }

    /// SB14 §3 D11 (r3 N5, r4 nit a): a header-only v2 store migrates to a marker over its
    /// segments, with no database, nothing set aside and no segment changed; at version 4 it
    /// reopens with `block_headers/` in place, and the next open's migrate leaves it there.
    #[tokio::test]
    async fn a_header_only_v2_store_gets_a_marker_and_keeps_its_segments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        for f in V2_FOLDERS_AT_4 {
            remove_dir_if_exists(&path.join(f)).unwrap();
        }
        let before = v2_files(&path);
        StorageMigrator::migrate(&path).await.unwrap();
        let tip = PathBuf::from(HEADERS.0).join("tip");
        let mut after = v2_files(&path);
        assert!(after.remove(&tip).is_some(), "the marker");
        assert_eq!(after, before, "no segment changed");
        assert!(!path.join(STORE_DIR).exists(), "no database (D4)");
        assert!(!path.join(UNMIGRATED_DIR).exists());
        assert_eq!(open(&path).view().active.hi, Some(2_999));
        StorageMigrator::migrate(&path).await.unwrap();
        assert!(path.join(&tip).exists() && !path.join(UNMIGRATED_DIR).exists());
        assert_eq!(open(&path).view().active.hi, Some(2_999));
    }

    /// D11 (c): a cut after the marker, before the version write, and a cut before the marker
    /// (no `tip`), each redo the step to the same slot bytes, with no version-2 data deleted.
    #[test]
    fn an_interrupted_conversion_redoes_to_the_same_slot() {
        let dir = tempfile::tempdir().unwrap();
        small(dir.path());
        let v2 = v2_files(dir.path());
        let tip = dir.path().join(HEADERS.0).join("tip");
        migrate(dir.path()).unwrap();
        let slot = fs::read(&tip).unwrap();
        migrate(dir.path()).unwrap();
        assert_eq!(fs::read(&tip).unwrap(), slot, "after the marker");
        fs::remove_file(&tip).unwrap();
        migrate(dir.path()).unwrap();
        assert_eq!(fs::read(&tip).unwrap(), slot, "before the marker");
        let mut now = v2_files(dir.path());
        now.remove(&PathBuf::from(HEADERS.0).join("tip"));
        assert_eq!(now, v2, "no version-2 data deleted");
        assert_eq!(open(dir.path()).view().active.hi, Some(2_999));
    }

    /// D11 (d): a version-3 store is discarded by its known file names; an unknown entry in
    /// `spv/` refuses the open with nothing deleted.
    #[tokio::test]
    async fn a_v3_store_is_discarded_unless_it_holds_an_unknown_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("version.json"), br#"{"version":3}"#).unwrap();
        let spv = path.join(STORE_DIR);
        fs::create_dir(&spv).unwrap();
        fs::write(spv.join("kv.sqlite"), b"v3").unwrap();
        fs::write(spv.join("notes"), b"not ours").unwrap();
        let refused = StorageMigrator::migrate(&path).await;
        assert!(matches!(refused, Err(crate::StorageError::InvalidArgument(_))), "{refused:?}");
        assert!(spv.join("kv.sqlite").exists() && spv.join("notes").exists(), "nothing deleted");
        fs::remove_file(spv.join("notes")).unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert!(!spv.exists(), "discarded");
        let version = fs::read_to_string(path.join("version.json")).unwrap();
        assert!(version.contains('4'), "{version}");
    }

    /// Upstream's version 3 (#1221): the version-2 fixture with the engine snapshot in place of
    /// the message log.
    fn upstream_v3(dir: &Path) {
        small(dir);
        let masternodes = dir.join(MASTERNODES);
        fs::remove_dir_all(&masternodes).unwrap();
        fs::create_dir(&masternodes).unwrap();
        fs::write(masternodes.join(UPSTREAM_ENGINE_FILE), b"snapshot").unwrap();
        fs::write(dir.join("version.json"), br#"{"version":3}"#).unwrap();
    }

    /// DEC-811: upstream's version 3 migrates as version 2 does; its engine snapshot is dropped,
    /// and the engine opens empty, to sync again.
    #[tokio::test]
    async fn an_upstream_v3_store_migrates_as_version_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        upstream_v3(&path);
        let storage = crate::storage::DiskStorageManager::new(
            &crate::ClientConfig::testnet().with_storage_path(&path),
        )
        .await
        .unwrap();
        let version = fs::read_to_string(path.join("version.json")).unwrap();
        assert!(version.contains('4'), "{version}");
        assert!(V2_FOLDERS_AT_4.iter().all(|f| !path.join(f).exists()), "no version-2 folder left");
        assert!(!path.join(UNMIGRATED_DIR).exists(), "nothing set aside");
        assert_eq!(storage.store().view().active.hi, Some(2_999));
        use crate::storage::StorageManager;
        let masternodes = storage.masternodes();
        let engine = masternodes.read().await.engine();
        assert!(engine.read().await.masternode_lists.is_empty(), "the engine syncs again");
    }

    /// A crash in V2to4 after the step back to version 2 (here a partial build) is redone as from
    /// version 2.
    #[tokio::test]
    async fn an_upstream_v3_migration_interrupted_in_v2to4_is_redone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        upstream_v3(&path);
        fs::remove_file(path.join(MASTERNODES).join(UPSTREAM_ENGINE_FILE)).unwrap();
        fs::write(path.join("version.json"), super::super::version_json(2, false).unwrap())
            .unwrap();
        fs::create_dir_all(path.join(MIGRATING_DIR).join("junk")).unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert_eq!(open(&path).view().active.hi, Some(2_999));
        assert!(V2_FOLDERS_AT_4.iter().all(|f| !path.join(f).exists()));
    }

    /// Both version-3 layouts at once: the open is refused, and nothing changes.
    #[tokio::test]
    async fn both_v3_layouts_at_once_are_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        upstream_v3(&path);
        let spv = path.join(STORE_DIR);
        fs::create_dir(&spv).unwrap();
        fs::write(spv.join("kv.sqlite"), b"v3").unwrap();
        let before = crate::storage::io::files_of(&path);
        let refused = StorageMigrator::migrate(&path).await;
        assert!(matches!(refused, Err(crate::StorageError::InvalidArgument(_))), "{refused:?}");
        assert_eq!(crate::storage::io::files_of(&path), before);
    }

    /// Version 3 with neither layout on disk is an empty storage path at version 4.
    #[tokio::test]
    async fn an_empty_v3_storage_path_becomes_version_4() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("version.json"), br#"{"version":3}"#).unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        let version = fs::read_to_string(path.join("version.json")).unwrap();
        assert!(version.contains('4'), "{version}");
        let left: Vec<_> = fs::read_dir(&path).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["version.json"]);
    }

    /// The fallback that clears storage it cannot read (here an unparsable `version.json`) keeps
    /// what a migration set aside (D39).
    #[tokio::test]
    async fn the_corruption_fallback_keeps_the_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        corrupt_filters(&path);
        let want = v2_files(&path);
        StorageMigrator::migrate(&path).await.unwrap();
        fs::write(path.join("version.json"), b"not json").unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert_eq!(v2_files(&path.join(UNMIGRATED_DIR)), want);
    }

    /// The fallback that clears unreadable storage, with no set-aside begun: it moves the
    /// version-2 folders to the aside before it clears, `block_headers/` included (it has no
    /// `tip`, so it is version 2), and the open starts a fresh sync (BL-186).
    #[tokio::test]
    async fn the_corruption_fallback_sets_v2_aside_without_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        let want = v2_files(&path);
        fs::write(path.join("version.json"), b"not json").unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert_eq!(v2_files(&path.join(UNMIGRATED_DIR)), want);
        assert!(V2_FOLDERS.iter().all(|f| !path.join(f).exists()));
        assert!(!path.join(STORE_DIR).exists(), "a fresh sync");
    }

    /// The same fallback over a version-4 store: its `block_headers/` has a `tip`, so it is the
    /// store's, not version-2 data, and is cleared with the rest; nothing is set aside.
    #[tokio::test]
    async fn the_corruption_fallback_clears_a_v4_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        StorageMigrator::migrate(&path).await.unwrap();
        assert!(path.join(HEADERS.0).join(store::hdr::TIP_FILE).exists());
        fs::write(path.join("version.json"), b"not json").unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert!(!path.join(UNMIGRATED_DIR).exists());
        assert!(v2_files(&path).is_empty());
        assert!(!path.join(STORE_DIR).exists(), "a fresh sync");
        let version = fs::read_to_string(path.join("version.json")).unwrap();
        assert!(version.contains('4'), "{version}");
    }

    /// The fallback after version 1's own migrator calls the data corrupt (a crash between its
    /// rename of the new segments and its delete of the legacy ones leaves both): every folder
    /// is set aside, `block_headers/` included (BL-186). The same with no `version.json`, which
    /// the first step writes as 1.
    #[tokio::test]
    async fn the_corruption_fallback_sets_v1_data_aside() {
        for version_file in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_path_buf();
            small(&path);
            fs::write(path.join(FILTERS.0).join("segment_0000.dat"), b"legacy").unwrap();
            match version_file {
                true => fs::write(path.join("version.json"), br#"{"version": 1}"#).unwrap(),
                false => fs::remove_file(path.join("version.json")).unwrap(),
            }
            let want = v2_files(&path);
            StorageMigrator::migrate(&path).await.unwrap();
            assert_eq!(v2_files(&path.join(UNMIGRATED_DIR)), want, "{version_file}");
            assert!(V2_FOLDERS.iter().all(|f| !path.join(f).exists()), "{version_file}");
        }
    }

    /// A crash after the marker step and before the version write leaves a `tip` over the
    /// version-2 headers, with the other version-2 folders still beside them. With `version.json`
    /// then unreadable, the headers are version-2 data and are set aside with the rest.
    #[tokio::test]
    async fn the_corruption_fallback_keeps_v2_headers_under_a_tip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        let Ok(Some(run)) = scan_headers(&path.join(HEADERS.0)) else {
            panic!("the fixture's header run");
        };
        write_marker(&path.join(HEADERS.0), run).unwrap();
        let want = v2_files(&path);
        assert!(want.contains_key(&PathBuf::from(HEADERS.0).join(store::hdr::TIP_FILE)));
        fs::write(path.join("version.json"), b"not json").unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        assert_eq!(v2_files(&path.join(UNMIGRATED_DIR)), want);
    }

    /// A storage path its owner cannot search (EACCES on every lookup under it) fails the open:
    /// nothing is migrated, deleted or marked, and the next open, searchable again, migrates
    /// (SB13 D42, D44). Root ignores the mode and migrates at once.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unsearchable_storage_path_fails_the_open_and_marks_nothing() {
        use crate::error::StorageError;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        let want = v2_files(&path);
        let version = fs::read(path.join("version.json")).unwrap();
        let mode = |m| fs::set_permissions(&path, fs::Permissions::from_mode(m)).unwrap();
        mode(0o600);
        let first = StorageMigrator::migrate(&path).await;
        mode(0o755);
        match first {
            Err(StorageError::Io(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
                let unmarked = fs::read(path.join("version.json")).unwrap() == version;
                assert!(unmarked, "the version is still 2");
                assert!(!path.join(STORE_DIR).exists());
                assert_eq!(v2_files(&path), want, "nothing moved or deleted");
                StorageMigrator::migrate(&path).await.unwrap();
            }
            Ok(()) => eprintln!("the lookups ignored the mode here (root)"),
            Err(e) => panic!("{e}"),
        }
        assert_eq!(open(&path).view().active.hi, Some(2_999));
    }

    /// A lookup that fails for one version-2 folder alone is an error, not "no such folder":
    /// `migrate` returning `Ok` is what has the loop mark version 3, over data it never saw, and
    /// the leftover cleanup must not read it as "nothing to clean" (SB13 D42, D44). Here the storage
    /// path is long enough that `<path>/filter_headers` alone does not fit PATH_MAX (4096 bytes
    /// with its NUL; ENAMETOOLONG) while every other name the migration looks up does.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_folder_lookup_is_not_an_absent_folder() {
        const LEN: usize = 4096 - "/filter_headers".len();
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        let mut rest = LEN - path.as_os_str().len();
        while rest > 0 {
            // Components of up to 201 bytes with their separator, never leaving 1 byte over.
            let take = if rest <= 201 {
                rest
            } else if rest == 202 {
                200
            } else {
                201
            };
            path.push("d".repeat(take - 1));
            rest -= take;
        }
        assert_eq!(path.as_os_str().len(), LEN);
        fs::create_dir_all(&path).unwrap();
        let lookup = |name: &str| fs::symlink_metadata(path.join(name)).unwrap_err().kind();
        assert_eq!(lookup(HEADERS.0), io::ErrorKind::NotFound, "the other names fit");
        assert_ne!(lookup(FILTER_HEADERS.0), io::ErrorKind::NotFound, "this one does not");
        assert!(migrate(&path).is_err(), "a failed lookup must not read as no version-2 data");
        assert!(remove_v2_leftovers(&path).is_err());
        assert!(!present(&path.join(STORE_DIR)).unwrap());
    }

    #[test]
    fn an_empty_storage_path_migrates_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        migrate(dir.path()).unwrap();
        assert!(!dir.path().join(STORE_DIR).exists());
    }

    /// D39, Opus-h r1 m3: at version 4, version-2 folders beside a partial `v2-unmigrated/` are
    /// a set-aside to finish. The direct open refuses them with nothing changed, so it cannot
    /// create the store that would make `DiskStorageManager::new` delete them; the migration then
    /// moves them aside.
    #[tokio::test]
    async fn a_direct_open_refuses_an_interrupted_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        StorageMigrator::write_current_version(&path).await.unwrap();
        let aside = path.join(UNMIGRATED_DIR);
        fs::create_dir(&aside).unwrap();
        fs::rename(path.join(METADATA), aside.join(METADATA)).unwrap();
        let before = (v2_files(&path), v2_files(&aside));
        assert!(super::super::admit_direct_open(&path).is_err());
        assert_eq!((v2_files(&path), v2_files(&aside)), before);
        assert!(!path.join(STORE_DIR).exists());
        StorageMigrator::migrate(&path).await.unwrap();
        assert!(V2_FOLDERS_AT_4.iter().all(|f| !path.join(f).exists() && aside.join(f).exists()));
    }

    /// DEC-811: a version-4 store written before the engine snapshot holds SB13 D6's message
    /// log. The open drops it in one Durable commit, and later opens have nothing to drop, so a
    /// client that still replays the log finds none and syncs the lists again.
    #[tokio::test]
    async fn the_open_drops_the_masternode_message_log_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        StorageMigrator::migrate(&path).await.unwrap();
        {
            let s = open(&path);
            s.put_mn_log_row(5, 0, b"diff").unwrap();
            s.put_mn_log_row(5, 1, b"qrinfo").unwrap();
            s.sync_wal().unwrap();
            assert_eq!(s.mn_log_rows().unwrap(), 2);
        }
        let config = crate::ClientConfig::testnet().with_storage_path(&path);
        let storage = crate::storage::DiskStorageManager::new(&config).await.unwrap();
        assert_eq!(storage.store().run(|s| s.mn_log_rows()).await.unwrap(), 0);
        // Durable: the store reopened without the manager's flush holds no row.
        drop(storage);
        let s = open(&path);
        assert_eq!(s.mn_log_rows().unwrap(), 0);
        assert!(!s.drop_mn_log().unwrap(), "nothing left to drop");
    }

    /// DEC-811: `version.json` names the layout at the current version only, and one written
    /// before it did (no `layout`) is this client's.
    #[tokio::test]
    async fn the_current_version_names_its_layout() {
        let at = |v| -> serde_json::Value {
            serde_json::from_str(&super::super::version_json(v, false).unwrap()).unwrap()
        };
        assert_eq!(at(4)["layout"], "hybrid");
        assert!(at(2).get("layout").is_none(), "version 2 is upstream's layout too");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        small(&path);
        StorageMigrator::migrate(&path).await.unwrap();
        fs::write(path.join("version.json"), br#"{"version":4}"#).unwrap();
        StorageMigrator::migrate(&path).await.unwrap();
        super::super::admit_direct_open(&path).unwrap();
        assert_eq!(open(&path).view().active.hi, Some(2_999));
    }

    /// DEC-811: a `version.json` naming another layout is refused, by the migration and the
    /// direct open, at any version, with nothing changed.
    #[tokio::test]
    async fn another_layout_is_refused_untouched() {
        for version in [2, 3, 4] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_path_buf();
            small(&path);
            let json = format!(r#"{{"version":{version},"layout":"other"}}"#);
            fs::write(path.join("version.json"), json).unwrap();
            let before = crate::storage::io::files_of(&path);
            let refused = StorageMigrator::migrate(&path).await;
            assert!(
                matches!(refused, Err(crate::StorageError::InvalidArgument(_))),
                "{version}: {refused:?}"
            );
            assert!(super::super::admit_direct_open(&path).is_err(), "{version}");
            assert_eq!(crate::storage::io::files_of(&path), before, "{version}");
        }
    }

    /// Sol r3 #1: a version-3 store directory that is a link, or a known name in it that is not
    /// a regular file, refuses the discard with nothing deleted, here or behind the link.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_v3_discard_refuses_links_and_non_files() {
        use std::os::unix::fs::symlink;
        // `spv/` a link to a directory outside the storage path holding a known name.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&path).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("kv.sqlite"), b"not ours").unwrap();
        fs::write(path.join("version.json"), br#"{"version":3}"#).unwrap();
        symlink(&outside, path.join(STORE_DIR)).unwrap();
        let refused = StorageMigrator::migrate(&path).await;
        assert!(matches!(refused, Err(crate::StorageError::InvalidArgument(_))), "{refused:?}");
        assert_eq!(fs::read(outside.join("kv.sqlite")).unwrap(), b"not ours");
        assert!(fs::symlink_metadata(path.join(STORE_DIR)).unwrap().is_symlink());

        // A known name that is a link to a file, or a directory.
        for child in ["link", "dir"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("storage");
            let spv = path.join(STORE_DIR);
            fs::create_dir_all(&spv).unwrap();
            fs::write(path.join("version.json"), br#"{"version":3}"#).unwrap();
            fs::write(spv.join("kv.sqlite"), b"v3").unwrap();
            let target = dir.path().join("target");
            fs::write(&target, b"not ours").unwrap();
            match child {
                "link" => symlink(&target, spv.join("kv.sqlite-wal")).unwrap(),
                _ => fs::create_dir(spv.join("kv.sqlite-wal")).unwrap(),
            }
            let refused = StorageMigrator::migrate(&path).await;
            assert!(matches!(refused, Err(crate::StorageError::InvalidArgument(_))), "{child}");
            assert_eq!(fs::read(&target).unwrap(), b"not ours", "{child}");
            assert_eq!(fs::read(spv.join("kv.sqlite")).unwrap(), b"v3", "{child}: nothing deleted");
        }
    }

    /// Sol r3 #2 (D39): a `metadata/` entry that is not a `<key>.dat` file is not deleted with
    /// the folder: the version-2 tree is set aside. v2's atomic-write leftovers are not values.
    #[test]
    fn an_unknown_metadata_entry_sets_the_v2_tree_aside() {
        for (stray, aside) in [("notes", true), (".x.dat.7.0.tmp", false)] {
            let dir = tempfile::tempdir().unwrap();
            small(dir.path());
            let meta = dir.path().join(METADATA);
            fs::create_dir_all(&meta).unwrap();
            fs::write(meta.join("key.dat"), b"value").unwrap();
            fs::write(meta.join(stray), b"kept").unwrap();
            migrate(dir.path()).unwrap();
            remove_v2_leftovers(dir.path()).unwrap();
            let kept = dir.path().join(UNMIGRATED_DIR).join(METADATA).join(stray);
            assert_eq!(kept.exists(), aside, "{stray}");
            assert_eq!(dir.path().join(STORE_DIR).exists(), !aside, "{stray}");
            if aside {
                assert_eq!(fs::read(kept).unwrap(), b"kept");
            }
        }
        // A directory under a value's name is not a value either.
        let dir = tempfile::tempdir().unwrap();
        small(dir.path());
        fs::create_dir_all(dir.path().join(METADATA).join("sub.dat")).unwrap();
        migrate(dir.path()).unwrap();
        assert!(dir.path().join(UNMIGRATED_DIR).join(METADATA).join("sub.dat").is_dir());
    }

    /// Opus-h r2 m3 (D39): an entry in `filter_headers/`, `filters/` or `blocks/` the build does
    /// not read (here an unknown name, a segment name Open does not read, or a directory under a
    /// segment's name) sets
    /// the tree aside rather than being deleted with its folder; v2's `.tmp` leftovers and OS
    /// artefacts, there or in `metadata/`, do not.
    #[test]
    fn an_unknown_segment_folder_entry_sets_the_v2_tree_aside() {
        for (folder, stray, aside) in [
            (FILTERS.0, "notes", true),
            (BLOCKS.0, "segment_12.dat", true),
            (FILTER_HEADERS.0, "segment_000099.dat", true),
            (FILTERS.0, ".segment_000000.dat.9.0.tmp", false),
            (FILTERS.0, ".DS_Store", false),
            (METADATA, "Thumbs.db", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            small(dir.path());
            let at = dir.path().join(folder);
            fs::create_dir_all(&at).unwrap();
            // The segment name in `filter_headers/` is a directory.
            match folder == FILTER_HEADERS.0 {
                true => fs::create_dir(at.join(stray)).unwrap(),
                false => fs::write(at.join(stray), b"kept").unwrap(),
            }
            migrate(dir.path()).unwrap();
            remove_v2_leftovers(dir.path()).unwrap();
            let kept = dir.path().join(UNMIGRATED_DIR).join(folder).join(stray);
            assert_eq!(kept.exists(), aside, "{folder}/{stray}");
            assert_eq!(dir.path().join(STORE_DIR).exists(), !aside, "{folder}/{stray}");
        }
    }

    /// D3, Opus-h r1 m4: a version-2 header run that reaches 2^24 is data the store cannot hold.
    /// The scan sets it aside, as it is with other version-2 folders present, instead of writing
    /// a marker that Open then clears.
    #[test]
    fn a_v2_run_reaching_the_height_cap_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let mut sentinel = sentinel_header().to_vec();
        sentinel.extend_from_slice(&[0; 32]);
        write_segments(
            &dir.path().join(HEADERS.0),
            HEADERS_PER_SEGMENT,
            HEIGHT_CAP - 3..HEIGHT_CAP + 2,
            |h| Some([&synthetic_header(h)[..], &synthetic_hash(h)[..]].concat()),
            &sentinel,
        )
        .unwrap();
        migrate(dir.path()).unwrap();
        assert!(dir.path().join(UNMIGRATED_DIR).join(HEADERS.0).exists());
        assert!(!dir.path().join(HEADERS.0).join("tip").exists(), "no marker");
        assert_eq!(open(dir.path()).view().active.hi, None);
    }
}
