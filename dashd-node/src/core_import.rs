//! Regtest's start on a Dash Core network directory (CH5-4b): Core's databases move aside into a
//! backup ([`dash_coreimport::regtest_import`]), and the node rebuilds from the block files Core
//! left through a stored reindex (CH5-3 D44), as Core's `-reindex` does.
//!
//! [`prepare`] runs under the network directory's lock after init's own refusals, before any
//! store opens: it resumes a journal, or proves the directory Core's ([`proven`]), with no cache
//! rdashd cannot read ([`unreadable_caches`]), and moves its databases (S1 to S5).
//! [`Pending::finish`] runs in the chainstate load between the fresh store's open and recovery's:
//! the reindex flag (S6), then `Complete` and the journal's move into the backup (S7, S8), so the
//! import is done before the scan reads its first record. Nothing is ever deleted.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use dash_coreimport::datadir_import::{ImportFs, OsImportFs};
use dash_coreimport::regtest_import::{self, MEMBERS, MoveAside, ObfuscateKey, Recovered};
use dash_p2p::store::{NetBlob, NetStore};
use dashd_chainstate::recovery::RequestReindexError;
use dashd_governance::store::{self as governance, GovernanceStore};
use dashd_masternode::meta::{self, MasternodeMetaStore};
use dashd_masternode::netfulfilled::{self, NetFulfilledStore};
use dashd_masternode::{flatdb, sporks};
use dashd_store::STORE_MARKER;

use crate::datadir;
use crate::logging::Logger;

/// The members that are one LevelDB database each; `llmq` and `indexes` hold several.
const SINGLE_DBS: [&str; 3] = ["chainstate", "blocks/index", "evodb"];

/// Whether the network directory `net` is Dash Core's, read without writing: `chainstate` is a
/// LevelDB database whose newest obfuscation key record is Core's `08 ‖ 8 bytes`, every member
/// is a database tree or absent, and nothing in them is a link or carries a sign of rdashd's
/// stores (its [`STORE_MARKER`], RocksDB's `IDENTITY` or `OPTIONS-*`). Anything else with a sign
/// of Core's is ambiguous and stays where it is.
// Core: dbwrapper.cpp:169-178 at v24.0.2
pub(crate) fn proven(net: &Path) -> bool {
    if std::fs::symlink_metadata(net.join("blocks")).is_ok_and(|m| !m.is_dir()) {
        return false;
    }
    let mut chainstate = false;
    for name in MEMBERS {
        match member(&net.join(name), SINGLE_DBS.contains(&name)) {
            Some(false) => {}
            Some(true) => chainstate |= name == MEMBERS[0],
            None => return false,
        }
    }
    chainstate
        && matches!(
            regtest_import::obfuscate_key(&net.join(MEMBERS[0])),
            Ok(ObfuscateKey::Present(_))
        )
}

/// `Some(false)` for a member that is missing or empty, `Some(true)` for a database tree (one
/// with LevelDB's `CURRENT` and a `MANIFEST-*` on top when `single`), `None` for anything else.
fn member(path: &Path, single: bool) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Ok(m) if m.is_dir() => {}
        _ => return None,
    }
    let mut top = Vec::new();
    let mut dirs = vec![path.to_path_buf()];
    let mut any = false;
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            let name = entry.file_name().into_string().ok()?;
            if kind.is_symlink()
                || name == STORE_MARKER
                || name == "IDENTITY"
                || name.starts_with("OPTIONS-")
            {
                return None;
            }
            any = true;
            if kind.is_dir() {
                dirs.push(entry.path());
            }
            if dir == path {
                top.push(name);
            }
        }
    }
    if !any {
        return Some(false);
    }
    let leveldb =
        top.iter().any(|n| n == "CURRENT") && top.iter().any(|n| n.starts_with("MANIFEST-"));
    (leveldb || !single).then_some(true)
}

type HasMagic = fn(&[u8]) -> bool;

/// The caches rdashd keeps in its own format (A1 §2.9), in the order a refusal names them, each
/// with the test of its envelope. Core's formats are IM1b's to import (MNO-04); the other files
/// Core leaves (`peers.dat`, …) do not stop a start, and are not checked. `governance.dat` is
/// checked under `-disablegovernance` too (DEC-1940).
const CACHES: [(&str, HasMagic); 4] = [
    (sporks::file::FILE_NAME, sporks::file::has_magic),
    (netfulfilled::FILE_NAME, flatdb::has_magic::<NetFulfilledStore>),
    (meta::FILE_NAME, flatdb::has_magic::<MasternodeMetaStore>),
    (governance::FILE, flatdb::has_magic::<GovernanceStore>),
];

/// The caches in `net` rdashd cannot read: present, without rdashd's envelope. Its loaders
/// refuse them at init, the sporks load before the chainstate and the others once the reindex
/// clears them, so a directory holding one is not imported.
// Core: init.cpp:1962-1964, :2242-2268, governance.cpp:79 at v24.0.2
pub(crate) fn unreadable_caches(net: &Path) -> Vec<&'static str> {
    use std::io::Read as _;
    let head = |path: &Path| {
        let mut head = Vec::new();
        std::fs::File::open(path).and_then(|f| f.take(64).read_to_end(&mut head)).ok()?;
        Some(head)
    };
    CACHES
        .iter()
        .filter(|(name, ours)| {
            let path = net.join(name);
            std::fs::symlink_metadata(&path).is_ok() && !head(&path).is_some_and(|h| ours(&h))
        })
        .map(|(name, _)| *name)
        .collect()
}

/// The refusal of a Core directory holding `caches` ([`unreadable_caches`]).
pub(crate) fn caches_refusal(net: &Path, caches: &[&str]) -> String {
    format!(
        "{} holds {}, which {} cannot read; these are caches that are recreated when missing, so \
         move them out of the directory to import it.",
        net.to_string_lossy(),
        caches.join(", "),
        crate::PACKAGE_NAME
    )
}

/// An import whose databases have moved, waiting for the fresh store's reindex flag.
pub(crate) struct Pending {
    aside: MoveAside,
    fs: Arc<dyn ImportFs>,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending").field("aside", &self.aside).finish_non_exhaustive()
    }
}

fn failed(net: &Path, e: &dyn std::fmt::Display) -> String {
    format!("Importing the Dash Core data directory {} failed: {e}", net.to_string_lossy())
}

/// The import's S1 to S5 on the regtest network directory `net`, or the resumption of an earlier
/// one from its journal, under the lock init holds. `None` when there is nothing to import; a
/// directory with a sign of Core's that is not [`proven`] gets
/// [`refuse_core_dir`](datadir::refuse_core_dir)'s text, and one with a cache rdashd cannot read
/// [`caches_refusal`]'s, with nothing touched.
pub(crate) fn prepare(net: &Path, logger: &Logger) -> Result<Option<Pending>, String> {
    let fs = import_fs();
    let aside = match regtest_import::recover(&*fs, net).map_err(|e| failed(net, &e))? {
        Recovered::Moved(aside) => aside,
        Recovered::Finished {
            ..
        } => return Ok(None),
        Recovered::Clean => {
            if !datadir::regtest_core_dir(net)? {
                return Ok(None);
            }
            let aside = regtest_import::begin(&*fs, net).map_err(|e| failed(net, &e))?;
            logger.log_printf(&format!(
                "Moved the Dash Core databases of {} aside to {}; reindexing its block files\n",
                net.to_string_lossy(),
                aside.backup().to_string_lossy()
            ));
            aside
        }
    };
    Ok(Some(Pending {
        aside,
        fs,
    }))
}

impl Pending {
    /// S6 through `request`, the fresh store's reindex flag, then S7 and S8. A refusal of the
    /// flag leaves the journal at `Moved`.
    pub(crate) fn finish(
        self,
        net: &Path,
        request: impl FnOnce() -> Result<(), RequestReindexError>,
    ) -> Result<(), String> {
        if let Err(e) = request() {
            return Err(failed(
                net,
                &format!(
                    "the reindex could not be stored ({e}); {} holds the import's journal and {} \
                     Core's databases",
                    net.join(regtest_import::JOURNAL).to_string_lossy(),
                    self.aside.backup().to_string_lossy()
                ),
            ));
        }
        tick();
        self.aside.complete(&*self.fs).map(drop).map_err(|e| failed(net, &e))
    }
}

#[cfg(not(feature = "test-hooks"))]
fn import_fs() -> Arc<dyn ImportFs> {
    Arc::new(OsImportFs::new())
}

#[cfg(not(feature = "test-hooks"))]
fn tick() {}

#[cfg(feature = "test-hooks")]
fn import_fs() -> Arc<dyn ImportFs> {
    Arc::new(ExitFs(OsImportFs::new()))
}

/// `RDASHD_TEST_HOOK=core-import-exit` with `RDASHD_TEST_HOOK_STEP=<n>`: the process `_exit`s
/// with status 3 right after the import's `n`th write (each mutating filesystem call, and the
/// stored reindex flag).
#[cfg(feature = "test-hooks")]
fn tick() {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    static STEP: OnceLock<Option<u64>> = OnceLock::new();
    static DONE: AtomicU64 = AtomicU64::new(0);
    let step = STEP.get_or_init(|| {
        (std::env::var("RDASHD_TEST_HOOK").as_deref() == Ok("core-import-exit")).then(|| {
            std::env::var("RDASHD_TEST_HOOK_STEP")
                .ok()
                .and_then(|s| s.parse().ok())
                .expect("RDASHD_TEST_HOOK_STEP")
        })
    });
    if step.is_some_and(|step| DONE.fetch_add(1, Ordering::SeqCst) + 1 == step) {
        crate::node::exit_now(3);
    }
}

#[cfg(feature = "test-hooks")]
struct ExitFs(OsImportFs);

#[cfg(feature = "test-hooks")]
impl ExitFs {
    fn after<T>(r: std::io::Result<T>) -> std::io::Result<T> {
        if r.is_ok() {
            tick();
        }
        r
    }
}

#[cfg(feature = "test-hooks")]
impl ImportFs for ExitFs {
    fn reflink(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        Self::after(self.0.reflink(src, dst))
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        Self::after(self.0.hard_link(src, dst))
    }
    fn copy(&self, src: &Path, dst: &Path) -> std::io::Result<u64> {
        Self::after(self.0.copy(src, dst))
    }
    fn sync_data(&self, path: &Path) -> std::io::Result<()> {
        Self::after(self.0.sync_data(path))
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        Self::after(self.0.sync_dir(path))
    }
    fn rename(&self, src: &Path, dst: &Path) -> std::io::Result<()> {
        Self::after(self.0.rename(src, dst))
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        Self::after(self.0.remove(path))
    }
    fn truncate(&self, path: &Path, len: u64) -> std::io::Result<()> {
        Self::after(self.0.truncate(path, len))
    }
    fn append(&self, path: &Path, data: &[u8]) -> std::io::Result<()> {
        Self::after(self.0.append(path, data))
    }
    fn metadata(
        &self,
        path: &Path,
    ) -> std::io::Result<Option<dash_coreimport::datadir_import::FileMetadata>> {
        self.0.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
        self.0.read_dir(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.0.read(path)
    }
    fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
        Self::after(self.0.create_dir_all(path))
    }
    fn available_space(&self, path: &Path) -> std::io::Result<u64> {
        self.0.available_space(path)
    }
    fn canonicalize(&self, path: &Path) -> std::io::Result<std::path::PathBuf> {
        self.0.canonicalize(path)
    }
    fn mount_table(&self) -> std::io::Result<Option<Vec<u8>>> {
        self.0.mount_table()
    }
    fn mount_id(&self, path: &Path) -> std::io::Result<Option<u64>> {
        self.0.mount_id(path)
    }
    fn sha256(&self, path: &Path) -> std::io::Result<[u8; 32]> {
        self.0.sha256(path)
    }
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
}

/// The network core's store for a directory init left to import ([`SelectedParams::core_import`]):
/// until [`prepare`] has moved the databases, every write of `peers.dat`, `banlist.json` and
/// `anchors.dat` waits in order, so a start refused before then leaves the files it found as they
/// were; [`HeldStore::release`] writes them. Core writes them at step 6 and at shutdown, and a
/// refused start's shutdown is where they would differ.
///
/// [`SelectedParams::core_import`]: crate::init::params::SelectedParams::core_import
// Core: addrdb.cpp:207-225, banman.cpp:16-26, init.cpp:349, :366-367 at v24.0.2
pub(crate) struct HeldStore {
    inner: dashd_net::FileStore,
    held: Mutex<Option<Vec<(NetBlob, Held)>>>,
}

enum Held {
    Save(Vec<u8>),
    Remove,
    Backup,
}

impl HeldStore {
    pub(crate) fn new(net: &Path) -> HeldStore {
        HeldStore {
            inner: dashd_net::FileStore::new(net),
            held: Mutex::new(Some(Vec::new())),
        }
    }

    /// Writes what waited, in order, and every later write at once. A backup that fails, Core's
    /// unrenamable `peers.dat`, is returned and drops every write queued after it, as Core's
    /// init stops there before its ban manager exists, so the file Core would have kept is not
    /// overwritten.
    // Core: addrdb.cpp:219-221, init.cpp:1683, :1706 at v24.0.2
    pub(crate) fn release(&self) -> std::io::Result<()> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        for (blob, op) in held.take().unwrap_or_default() {
            match self.apply(blob, &op) {
                Err(e) if matches!(op, Held::Backup) => return Err(e),
                Err(e) => {
                    tracing::warn!(target: "net", "Failed to write {}: {e}", blob.file_name())
                }
                Ok(()) => {}
            }
        }
        Ok(())
    }

    fn apply(&self, blob: NetBlob, op: &Held) -> std::io::Result<()> {
        match op {
            Held::Save(bytes) => self.inner.save(blob, bytes),
            Held::Remove => self.inner.remove(blob),
            Held::Backup => self.inner.backup(blob),
        }
    }

    fn write(&self, blob: NetBlob, op: Held) -> std::io::Result<()> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        match held.as_mut() {
            Some(waiting) => {
                waiting.push((blob, op));
                Ok(())
            }
            None => self.apply(blob, &op),
        }
    }
}

impl std::fmt::Debug for HeldStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner).is_some();
        f.debug_struct("HeldStore").field("held", &held).finish_non_exhaustive()
    }
}

impl NetStore for HeldStore {
    fn load(&self, blob: NetBlob) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.load(blob)
    }
    fn save(&self, blob: NetBlob, bytes: &[u8]) -> std::io::Result<()> {
        self.write(blob, Held::Save(bytes.to_vec()))
    }
    fn remove(&self, blob: NetBlob) -> std::io::Result<()> {
        self.write(blob, Held::Remove)
    }
    fn backup(&self, blob: NetBlob) -> std::io::Result<()> {
        self.write(blob, Held::Backup)
    }
    fn location(&self, blob: NetBlob) -> String {
        self.inner.location(blob)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use dash_coreimport::regtest_import::{JOURNAL, JournalState, Phase, read_journal};

    use super::*;

    /// A chainstate dashd v24.0.0 built: the functional-test framework's regtest cache (node0),
    /// whose obfuscation key is in its table only (dash-coreimport's
    /// `tests/data/regtest_import/PROVENANCE.md`; these are that test's literals).
    const CURRENT: [u8; 16] = *b"MANIFEST-000004\n";
    const MANIFEST: [u8; 103] = [
        0x56, 0xf9, 0xb8, 0xf8, 0x1c, 0x00, 0x01, 0x01, 0x1a, 0x6c, 0x65, 0x76, 0x65, 0x6c, 0x64,
        0x62, 0x2e, 0x42, 0x79, 0x74, 0x65, 0x77, 0x69, 0x73, 0x65, 0x43, 0x6f, 0x6d, 0x70, 0x61,
        0x72, 0x61, 0x74, 0x6f, 0x72, 0x73, 0x84, 0x8e, 0x66, 0x3d, 0x00, 0x01, 0x02, 0x06, 0x09,
        0x00, 0x03, 0x07, 0x04, 0x01, 0x07, 0x00, 0x05, 0xc6, 0x01, 0x17, 0x0e, 0x00, 0x6f, 0x62,
        0x66, 0x75, 0x73, 0x63, 0x61, 0x74, 0x65, 0x5f, 0x6b, 0x65, 0x79, 0x01, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x17, 0x0e, 0x00, 0x6f, 0x62, 0x66, 0x75, 0x73, 0x63, 0x61, 0x74,
        0x65, 0x5f, 0x6b, 0x65, 0x79, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    const TABLE: [u8; 198] = [
        0x00, 0x17, 0x09, 0x0e, 0x00, 0x6f, 0x62, 0x66, 0x75, 0x73, 0x63, 0x61, 0x74, 0x65, 0x5f,
        0x6b, 0x65, 0x79, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x5c, 0x9b, 0x2f,
        0x71, 0xf0, 0x03, 0x66, 0xe3, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x56,
        0xc9, 0x41, 0x1b, 0x40, 0x80, 0x00, 0x01, 0x02, 0x04, 0x08, 0x00, 0x06, 0x00, 0x00, 0x00,
        0x00, 0x09, 0x00, 0x00, 0x00, 0x0b, 0x00, 0xe9, 0xbb, 0xf7, 0xbe, 0x00, 0x22, 0x02, 0x66,
        0x69, 0x6c, 0x74, 0x65, 0x72, 0x2e, 0x6c, 0x65, 0x76, 0x65, 0x6c, 0x64, 0x62, 0x2e, 0x42,
        0x75, 0x69, 0x6c, 0x74, 0x69, 0x6e, 0x42, 0x6c, 0x6f, 0x6f, 0x6d, 0x46, 0x69, 0x6c, 0x74,
        0x65, 0x72, 0x32, 0x30, 0x12, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x4a,
        0xc8, 0x29, 0xdc, 0x00, 0x09, 0x02, 0x0f, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0x00, 0x2b, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xd6, 0xa1, 0x74, 0x43,
        0x47, 0x2f, 0x7b, 0x16, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x57, 0xfb, 0x80, 0x8b, 0x24,
        0x75, 0x47, 0xdb,
    ];
    const KEY: &[u8] = b"\x0e\0obfuscate_key";

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Files named as LevelDB leaves them, with no content.
    fn leveldb_names(db: &Path) {
        for f in ["000003.log", "CURRENT", "LOCK", "MANIFEST-000002"] {
            write(&db.join(f), b"");
        }
    }

    /// A regtest network directory in Core's layout: the cache chainstate, LevelDB-named
    /// `blocks/index`, `evodb`, `llmq/*db` and `indexes/txindex`, and a block file.
    pub(crate) fn write_core(net: &Path) {
        let chainstate = net.join("chainstate");
        write(&chainstate.join("CURRENT"), &CURRENT);
        write(&chainstate.join("MANIFEST-000004"), &MANIFEST);
        write(&chainstate.join("000005.ldb"), &TABLE);
        write(&chainstate.join("000006.log"), b"");
        write(&chainstate.join("LOCK"), b"");
        for db in ["blocks/index", "evodb", "llmq/dkgdb", "llmq/quorumdb", "indexes/txindex"] {
            leveldb_names(&net.join(db));
        }
        if !net.join("blocks/blk00000.dat").exists() {
            write(&net.join("blocks/blk00000.dat"), b"");
        }
    }

    fn crc32c(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0x82f6_3b78 & 0u32.wrapping_sub(crc & 1));
            }
        }
        !crc
    }

    /// One full WAL record of a batch at sequence 2 holding `op`: a put of `value`, or a deletion.
    fn wal(value: Option<&[u8]>) -> Vec<u8> {
        let mut batch = 2u64.to_le_bytes().to_vec();
        batch.extend(1u32.to_le_bytes());
        batch.push(u8::from(value.is_some()));
        batch.push(KEY.len() as u8);
        batch.extend(KEY);
        if let Some(value) = value {
            batch.push(value.len() as u8);
            batch.extend(value);
        }
        let mut typed = vec![1u8];
        typed.extend(&batch);
        let crc = crc32c(&typed);
        let masked = crc.rotate_right(15).wrapping_add(0xa282_ead8);
        let mut out = masked.to_le_bytes().to_vec();
        out.extend((batch.len() as u16).to_le_bytes());
        out.extend(typed);
        out
    }

    /// Every path under `dir`, with the bytes of each file.
    pub(crate) fn contents(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut out = BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let path = e.unwrap().path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let rel = path.strip_prefix(dir).unwrap().to_path_buf();
                if meta.is_dir() {
                    todo.push(path);
                    out.insert(rel, None);
                } else {
                    out.insert(rel, Some(std::fs::read(&path).unwrap_or_default()));
                }
            }
        }
        out
    }

    fn core_net() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        write_core(&net);
        (dir, net)
    }

    #[test]
    fn a_core_proven_regtest_directory_is_deferred_to_import() {
        let (_dir, net) = core_net();
        let before = contents(&net);
        assert!(proven(&net));
        assert_eq!(datadir::regtest_core_dir(&net), Ok(true));
        assert_eq!(contents(&net), before, "nothing written, not even a lock file");
        for name in MEMBERS.iter().skip(1) {
            let (_dir, net) = core_net();
            std::fs::remove_dir_all(net.join(name)).unwrap();
            assert!(proven(&net), "{name} absent");
            std::fs::create_dir(net.join(name)).unwrap();
            assert!(proven(&net), "{name} empty");
        }
    }

    #[test]
    fn an_ambiguous_leveldb_is_refused_intact() {
        let refusal = |net: &Path| {
            format!(
                "{} holds a Dash Core data directory; rdashd does not write to it; to import it, \
                 start rdashd on an empty -datadir with -importcoredatadir=<its base datadir>.",
                net.display()
            )
        };
        type Damage = fn(&Path);
        let cases: [(&str, Damage); 11] = [
            ("a tombstoned key", |net| {
                write(&net.join("chainstate/000006.log"), &wal(None));
            }),
            ("a seven-byte key", |net| {
                write(&net.join("chainstate/000006.log"), &wal(Some(&[7, 1, 2, 3, 4, 5, 6, 7])));
            }),
            ("a key of another length prefix", |net| {
                write(&net.join("chainstate/000006.log"), &wal(Some(&[9; 9])));
            }),
            ("no table, so no key", |net| {
                std::fs::remove_file(net.join("chainstate/000005.ldb")).unwrap();
            }),
            ("a half-created chainstate", |net| {
                std::fs::remove_file(net.join("chainstate/MANIFEST-000004")).unwrap();
            }),
            ("a half-created evodb", |net| {
                std::fs::remove_file(net.join("evodb/CURRENT")).unwrap();
            }),
            ("an own store's marker in llmq", |net| {
                write(&net.join("llmq/isdb").join(STORE_MARKER), b"");
            }),
            ("RocksDB's IDENTITY in indexes", |net| {
                write(&net.join("indexes/txindex/IDENTITY"), b"");
            }),
            ("RocksDB's OPTIONS in blocks/index", |net| {
                write(&net.join("blocks/index/OPTIONS-000007"), b"");
            }),
            ("a symlinked member", |net| {
                std::fs::rename(net.join("evodb"), net.join("evodb.real")).unwrap();
                std::os::unix::fs::symlink(net.join("evodb.real"), net.join("evodb")).unwrap();
            }),
            ("a file as a member", |net| {
                std::fs::remove_dir_all(net.join("indexes")).unwrap();
                write(&net.join("indexes"), b"x");
            }),
        ];
        for (case, make) in cases {
            let (_dir, net) = core_net();
            make(&net);
            let before = contents(&net);
            assert!(!proven(&net), "{case}");
            assert_eq!(datadir::regtest_core_dir(&net), Err(refusal(&net)), "{case}");
            assert_eq!(contents(&net), before, "{case}");
        }
        let (_dir, net) = core_net();
        std::fs::remove_dir_all(net.join("blocks")).unwrap();
        std::os::unix::fs::symlink(net.join("chainstate"), net.join("blocks")).unwrap();
        assert!(!proven(&net), "a symlinked blocks");
    }

    #[test]
    fn own_store_is_never_core() {
        use dashd_chainstate::store::CHAINSTATE_SCHEMA;
        use dashd_store::RocksEngine;
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path().join("regtest");
        drop(
            RocksEngine::open(&net.join("chainstate"), &CHAINSTATE_SCHEMA, &Default::default())
                .unwrap(),
        );
        drop(
            RocksEngine::open(&net.join("llmq/isdb"), &CHAINSTATE_SCHEMA, &Default::default())
                .unwrap(),
        );
        assert!(!proven(&net));
        assert_eq!(datadir::regtest_core_dir(&net), Ok(false));

        let (_dir, net) = core_net();
        drop(
            RocksEngine::open(&net.join("llmq/isdb"), &CHAINSTATE_SCHEMA, &Default::default())
                .unwrap(),
        );
        assert!(!proven(&net), "Core's databases beside an own store");
        assert!(datadir::regtest_core_dir(&net).is_err());
    }

    #[test]
    fn a_journal_defers_a_partially_moved_regtest_directory() {
        let (_dir, net) = core_net();
        let fs = OsImportFs::new();
        let aside = regtest_import::begin(&fs, &net).unwrap();
        assert_eq!(datadir::probe_core_data_dir(&net), None, "every sign moved");
        assert_eq!(datadir::regtest_core_dir(&net), Ok(true));
        std::fs::write(net.join(JOURNAL), b"CH5RJ1 torn").unwrap();
        let before = contents(&net);
        let refused = datadir::regtest_core_dir(&net).unwrap_err();
        assert!(refused.contains("is corrupt"), "{refused}");
        assert!(refused.contains("nothing was changed"), "{refused}");
        let logger = Logger::new();
        assert!(prepare(&net, &logger).unwrap_err().contains("is corrupt"));
        assert_eq!(contents(&net), before);
        assert!(aside.backup().join("chainstate/000005.ldb").is_file());
    }

    /// `sporks.dat`, `netfulfilled.dat` and `mncache.dat` without rdashd's envelope stop the
    /// import, each named in that order; with rdashd's they do not.
    #[test]
    fn caches_rdashd_cannot_read_are_named_in_order() {
        let ms = [0xfc, 0xc1, 0xb7, 0xdc];
        let ours: [(&str, Vec<u8>); 4] = [
            ("sporks.dat", sporks::file::encode(&dash_evo::spork::SporkStore::new(), ms)),
            ("netfulfilled.dat", flatdb::encode(&NetFulfilledStore::default(), ms)),
            ("mncache.dat", flatdb::encode(&MasternodeMetaStore::default(), ms)),
            ("governance.dat", flatdb::encode(&GovernanceStore::default(), ms)),
        ];
        let core = |magic: &str| {
            let mut out = vec![magic.len() as u8];
            out.extend(magic.as_bytes());
            out.extend(ms);
            out
        };
        let theirs = [
            ("sporks.dat", core("magicSporkCache")),
            ("netfulfilled.dat", core("magicFulfilledCache")),
            ("mncache.dat", core("magicMasternodeCache")),
            ("governance.dat", core("magicGovernanceCache")),
        ];
        let (_dir, net) = core_net();
        assert_eq!(unreadable_caches(&net), Vec::<&str>::new());
        for (name, bytes) in &ours {
            write(&net.join(name), bytes);
        }
        assert_eq!(unreadable_caches(&net), Vec::<&str>::new(), "rdashd's own");
        assert_eq!(datadir::regtest_core_dir(&net), Ok(true));
        for pick in 1u8..16 {
            let (_dir, net) = core_net();
            let mut named = Vec::new();
            for (i, ((name, ours), (_, theirs))) in ours.iter().zip(&theirs).enumerate() {
                let bytes = if pick & (1 << i) != 0 {
                    named.push(*name);
                    theirs
                } else {
                    ours
                };
                write(&net.join(name), bytes);
            }
            let before = contents(&net);
            assert_eq!(unreadable_caches(&net), named, "{pick}");
            assert_eq!(datadir::regtest_core_dir(&net), Err(caches_refusal(&net, &named)));
            assert!(prepare(&net, &Logger::new()).is_err());
            assert_eq!(contents(&net), before, "{pick}");
        }
        let (_dir, net) = core_net();
        std::fs::create_dir(net.join("mncache.dat")).unwrap();
        write(&net.join("sporks.dat"), b"");
        assert_eq!(unreadable_caches(&net), ["sporks.dat", "mncache.dat"]);
        assert_eq!(
            caches_refusal(&net, &["sporks.dat", "mncache.dat"]),
            format!(
                "{} holds sporks.dat, mncache.dat, which rdashd cannot read; these are caches that \
                 are recreated when missing, so move them out of the directory to import it.",
                net.display()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_held_lock_is_core_s_lock_error_before_any_read() {
        let (_dir, net) = core_net();
        let mut holder = datadir::tests::spawn_lock_holder(&net);
        assert_eq!(
            datadir::regtest_core_dir(&net),
            Err(format!(
                "Cannot obtain a lock on data directory {}. rdashd is probably already running.",
                net.display()
            ))
        );
        drop(holder.stdin.take());
        assert!(holder.wait().unwrap().success());
    }

    fn journal(net: &Path) -> Option<JournalState> {
        read_journal(&OsImportFs::new(), net).unwrap()
    }

    #[test]
    fn the_store_holds_reindexing_before_the_journal_completes() {
        let (_dir, net) = core_net();
        let earlier = net.join("core-import-20200101T000000Z");
        write(&earlier.join("kept"), b"an earlier backup");
        let core = contents(&net);
        let logger = Logger::new();
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
        assert!(logger.start_logging());
        let pending = prepare(&net, &logger).unwrap().expect("an import");
        let backup = pending.aside.backup();
        assert_ne!(backup, earlier);
        let line = format!(
            "Moved the Dash Core databases of {} aside to {}; reindexing its block files\n",
            net.display(),
            backup.display()
        );
        let lines = lines.lock().unwrap().clone();
        assert!(lines.len() == 1 && lines[0].ends_with(&line), "{lines:?}");
        let mut asked = false;
        pending
            .finish(&net, || {
                asked = true;
                let state = journal(&net).expect("the journal is still in the network directory");
                assert_eq!(state.phase, Phase::Moved);
                assert!(!backup.join(JOURNAL).exists());
                Ok(())
            })
            .unwrap();
        assert!(asked);
        assert_eq!(journal(&net), None);
        let state = read_journal(&OsImportFs::new(), &backup).unwrap().unwrap();
        assert_eq!(state.phase, Phase::Complete);
        for name in MEMBERS {
            let before: BTreeMap<_, _> = core
                .iter()
                .filter_map(|(p, c)| Some((p.strip_prefix(name).ok()?.to_path_buf(), c.clone())))
                .collect();
            let after = contents(&backup.join(name));
            let before: BTreeMap<_, _> =
                before.into_iter().filter(|(p, _)| !p.as_os_str().is_empty()).collect();
            assert_eq!(after, before, "{name} moved unchanged");
            assert!(!net.join(name).exists(), "{name}");
        }
        assert_eq!(std::fs::read(earlier.join("kept")).unwrap(), b"an earlier backup");
        assert!(net.join("blocks/blk00000.dat").is_file(), "the block files stay");
        assert!(prepare(&net, &logger).unwrap().is_none(), "nothing more to import");
    }

    #[test]
    fn a_request_reindex_refusal_leaves_the_journal_at_moved() {
        let (_dir, net) = core_net();
        let logger = Logger::new();
        let pending = prepare(&net, &logger).unwrap().unwrap();
        let backup = pending.aside.backup();
        let refused = pending.finish(&net, || Err(RequestReindexError::NotEmpty)).unwrap_err();
        assert_eq!(
            refused,
            format!(
                "Importing the Dash Core data directory {} failed: the reindex could not be stored \
                 (the block index is not empty); {} holds the import's journal and {} Core's \
                 databases",
                net.display(),
                net.join(JOURNAL).display(),
                backup.display()
            )
        );
        assert_eq!(journal(&net).unwrap().phase, Phase::Moved);
        let again = prepare(&net, &logger).unwrap().expect("resumed at Moved");
        assert_eq!(again.aside.backup(), backup);
    }
}
