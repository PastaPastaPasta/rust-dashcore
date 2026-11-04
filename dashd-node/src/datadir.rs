//! The data directory: Core's lock and pid file (`init.cpp`, `util/fs_helpers.cpp`), and the
//! refusal of a Dash Core data directory (DEC-201 Q6).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use dash_args::ArgsManager;
use dash_args::args::abs_path_for_config_val;
use dashd_store::STORE_MARKER;

use crate::InitFailure;

/// Core's `BITCOIN_PID_FILENAME` (DEC-201 Q3: rdashd keeps Core's name).
pub const PID_FILENAME: &str = "dashd.pid";

/// The lock file in the network data directory.
pub const LOCK_FILENAME: &str = ".lock";

/// Core's `SysErrorString(errno)`: the system's message, then the code in parentheses.
pub fn sys_error_string(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = io::Error::from_raw_os_error(code).to_string();
            let text = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text);
            format!("{text} ({code})")
        }
        None => e.to_string(),
    }
}

/// `std::error_code::message()`: the system's message alone.
fn error_message(e: &io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(code) => text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text).to_owned(),
        None => text,
    }
}

/// Core's `DirIsWritable`: whether a new file can be created in `dir` (it is removed again).
// Core: util/fs_helpers.cpp:94-105, util/getuniquepath.cpp
pub fn dir_is_writable(dir: &Path) -> bool {
    let path = dir.join(unique_name());
    match OpenOptions::new().append(true).create(true).open(&path) {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&path);
            true
        }
        Err(_) => false,
    }
}

/// `HexStr` of 8 random bytes (`GetUniquePath`'s name).
fn unique_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:016x}", h.finish())
}

/// Core's `dir_locks`: the lock files this process holds, by their path with symbolic links
/// resolved, each with its open file and the number of [`DirLock`]s that own it. A held one is
/// not opened again: on Unix, closing that second descriptor would release the lock. (Another
/// spelling through a hard or bind mount still would, as in Core.)
static HELD: Mutex<BTreeMap<PathBuf, (File, usize)>> = Mutex::new(BTreeMap::new());

fn held() -> MutexGuard<'static, BTreeMap<PathBuf, (File, usize)>> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An owner of a held data directory lock (Core's `dir_locks` entry). The lock is released when
/// its last owner drops.
///
/// On Unix the lock is Core's `fcntl(F_SETLK)` write lock, so rdashd and dashd exclude each
/// other. POSIX ties such a lock to the process: closing any descriptor of the lock file in this
/// process releases it, so nothing else opens the file while it is held, and the one descriptor
/// is closed only under the registry's mutex, once no owner is left.
#[derive(Debug)]
pub struct DirLock {
    path: PathBuf,
    key: PathBuf,
}

impl DirLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DirLock {
    // Core: util/fs_helpers.cpp:82-86 (UnlockDirectory)
    fn drop(&mut self) {
        let mut held = held();
        let Some((_, owners)) = held.get_mut(&self.key) else {
            return;
        };
        *owners -= 1;
        if *owners == 0 {
            // Closing the file unlocks it.
            held.remove(&self.key);
        }
    }
}

/// Core's `LockDirectory`: creates `<dir>/<name>` if needed and takes its write lock. With
/// `probe_only` the lock is released at once. A lock this process already holds succeeds without
/// touching the file: a probe returns `None`, any other call another owner of it (where Core
/// shares one entry, which its first `UnlockDirectory` releases). A failure logs Core's error
/// line and returns the reason.
// Core: util/fs_helpers.cpp:57-80, util/fs.cpp:49-79 (FileLock), :88-117 (Windows)
pub fn lock_directory(dir: &Path, name: &str, probe_only: bool) -> Result<Option<DirLock>, String> {
    lock(dir, name, probe_only, true)
}

/// [`lock_directory`] for a check that must not write to `dir`: the lock file is never created.
/// One that cannot be opened (missing, or not writable) is held by no node starting there, and
/// `Ok(None)` is returned.
fn lock_existing(dir: &Path, name: &str) -> Result<Option<DirLock>, String> {
    lock(dir, name, false, false)
}

fn lock(dir: &Path, name: &str, probe_only: bool, create: bool) -> Result<Option<DirLock>, String> {
    let mut held = held();
    let path = dir.join(name);
    // `realpath` opens nothing.
    let key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()).join(name);
    if let Some((_, owners)) = held.get_mut(&key) {
        if probe_only {
            return Ok(None);
        }
        *owners += 1;
        return Ok(Some(DirLock {
            path,
            key,
        }));
    }
    if create {
        // Core creates the file (`fopen "a"`), then opens it read-write.
        let _ = OpenOptions::new().append(true).create(true).open(&path);
    }
    let file = OpenOptions::new().read(true).write(true).open(&path);
    if !create && file.is_err() {
        return Ok(None);
    }
    let locked = file.and_then(|f| try_lock(&f).map(|()| f));
    match locked {
        Ok(file) if probe_only => {
            drop(file);
            Ok(None)
        }
        Ok(file) => {
            held.insert(key.clone(), (file, 1));
            Ok(Some(DirLock {
                path,
                key,
            }))
        }
        Err(e) => {
            let reason = sys_error_string(&e);
            tracing::error!(
                "Error while attempting to lock directory {}: {reason}",
                dir.to_string_lossy()
            );
            Err(reason)
        }
    }
}

#[cfg(unix)]
fn try_lock(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: an all-zero `flock` is valid; the fields Core sets are set below.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = 0;
    fl.l_len = 0;
    // SAFETY: the descriptor is open for the call, and `fl` is a valid `flock`.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &fl) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn try_lock(file: &File) -> io::Result<()> {
    // Windows: `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)` over the whole
    // range, as Core's `FileLock::TryLock`.
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::Error(e) => e,
        std::fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
    })
}

/// Core's `LockDataDirectory`: the network data directory must be writable and lockable.
// Core: init.cpp:1470-1481
pub fn lock_data_directory(
    args: &ArgsManager,
    probe_only: bool,
) -> Result<Option<DirLock>, InitFailure> {
    let datadir = args.get_data_dir_net()?;
    let shown = datadir.to_string_lossy();
    if !dir_is_writable(&datadir) {
        return Err(InitFailure::Error(format!(
            "Cannot write to data directory '{shown}'; check permissions."
        )));
    }
    lock_directory(&datadir, LOCK_FILENAME, probe_only)
        .map_err(|_| InitFailure::Error(lock_failure(&datadir)))
}

/// Core's error for a data directory another process holds.
fn lock_failure(datadir: &Path) -> String {
    format!(
        "Cannot obtain a lock on data directory {}. {} is probably already running.",
        datadir.to_string_lossy(),
        crate::PACKAGE_NAME
    )
}

/// Core's `GetPidFile`: `-pid`, relative to the network data directory.
pub fn pid_file_path(args: &ArgsManager) -> Result<PathBuf, InitFailure> {
    let pid = args.get_path_arg("-pid", Path::new(PID_FILENAME))?;
    Ok(abs_path_for_config_val(args, &pid, true)?)
}

/// The pid file this process wrote (Core's `g_generated_pid`). [`PidFile::remove`] is Core's
/// `RemovePidFile`.
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// Core's `CreatePidFile`: this process's id and a newline.
    // Core: init.cpp:214-227
    pub fn create(args: &ArgsManager) -> Result<PidFile, InitFailure> {
        let path = pid_file_path(args)?;
        match std::fs::write(&path, format!("{}\n", std::process::id())) {
            Ok(()) => Ok(PidFile {
                path,
            }),
            Err(e) => Err(InitFailure::Error(format!(
                "Unable to create the PID file '{}': {}",
                path.to_string_lossy(),
                sys_error_string(&e)
            ))),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Core's `RemovePidFile`: a failure is only logged.
    // Core: init.cpp:229-237
    pub fn remove(self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            let msg = if e.kind() == io::ErrorKind::NotFound {
                "File does not exist".to_owned()
            } else {
                error_message(&e)
            };
            tracing::info!("Unable to remove PID file ({}): {msg}", self.path.to_string_lossy());
        }
    }
}

/// What makes a directory a Dash Core data directory (DEC-201 Q6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreDataDirSign {
    /// `chainstate/` or `blocks/index/` is a LevelDB database: `CURRENT` and a `MANIFEST-*`,
    /// and either a `*.ldb` table or none of rdashd's [`STORE_MARKER`] and RocksDB's `IDENTITY`
    /// and `OPTIONS-*`.
    LevelDb(PathBuf),
    /// `evodb/` or `llmq/` exists: only Dash Core writes them.
    DashDir(PathBuf),
}

/// Looks, without writing, for a sign that `dir` (a network data directory) is Dash Core's.
/// The certain signs come first (see `is_leveldb` for why that order matters).
pub fn probe_core_data_dir(dir: &Path) -> Option<CoreDataDirSign> {
    if let Some(d) = [dir.join("evodb"), dir.join("llmq")].into_iter().find(|d| d.exists()) {
        return Some(CoreDataDirSign::DashDir(d));
    }
    [dir.join("chainstate"), dir.join("blocks").join("index")]
        .into_iter()
        .find(|db| is_leveldb(db))
        .map(CoreDataDirSign::LevelDb)
}

/// Whether `db` is a LevelDB database, as Core's `CDBWrapper` leaves one. A database exists once
/// its `CURRENT` does, in LevelDB as in RocksDB, and Core reopens one a killed first start left.
/// RocksDB (11.8.1, `write_identity_file` set by the store) writes `IDENTITY` durably before its
/// first `MANIFEST` and `CURRENT`, and `OPTIONS-*` only at the end of the open; LevelDB never
/// writes `IDENTITY`. So a RocksDB rdashd was killed while making is never taken for Core's
/// (BL-147 (b)). LevelDB's tables are `*.ldb`, which RocksDB never writes (its are `*.sst`), so a
/// Core database a RocksDB tool was killed while opening is still LevelDB once it holds a table;
/// one with none yet (or only old `*.sst` ones) passes here, and is refused only after init's
/// first writes. In a Dash Core data directory `evodb/` is refused before this check runs.
/// rdashd's [`STORE_MARKER`], which the store makes durable before it creates a database, is a
/// RocksDB sign too (BL-509), for a RocksDB that wrote no `IDENTITY`; a table still wins over it.
// Core: dbwrapper.cpp:130-158, leveldb/db/db_impl.cc:290-317, leveldb/db/filename.cc:33-36
// RocksDB 11.8.1: db/db_impl/db_impl_open.cc:329-389, db/db_impl/db_impl_files.cc:1164-1166,
// file/filename.cc:113-119 and 469-500
fn is_leveldb(db: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(db) else {
        return false;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.iter().any(|n| n == "CURRENT")
        && names.iter().any(|n| n.starts_with("MANIFEST-"))
        && (names.iter().any(|n| n.ends_with(".ldb"))
            || !names
                .iter()
                .any(|n| n == STORE_MARKER || n == "IDENTITY" || n.starts_with("OPTIONS-")))
}

/// The refusal text for the network data directory `dir` if it is Dash Core's (DEC-201 Q6).
/// LevelDB alone is decided under `dir`'s lock, as Core opens its databases only once it holds
/// it: a node starting there at the same time makes its own under the lock. While the lock is
/// held elsewhere, the answer is Core's lock error instead (D21). Nothing is written, not even a
/// lock file.
// Core: init.cpp:1470-1505
pub(crate) fn refuse_core_dir(dir: &Path) -> Result<(), String> {
    let _lock;
    let sign = match probe_core_data_dir(dir) {
        None => return Ok(()),
        Some(sign @ CoreDataDirSign::DashDir(_)) => sign,
        Some(CoreDataDirSign::LevelDb(_)) => {
            _lock = lock_existing(dir, LOCK_FILENAME).map_err(|_| lock_failure(dir))?;
            let Some(sign) = probe_core_data_dir(dir) else {
                return Ok(());
            };
            sign
        }
    };
    tracing::info!("Dash Core data directory found: {sign:?}");
    let shown = dir.to_string_lossy();
    Err(format!(
        "{shown} holds a Dash Core data directory; {} does not write to it. To import it, use \
         -importcoredatadir={shown}.",
        crate::PACKAGE_NAME
    ))
}

/// The refusal of a Dash Core data directory for the selected network (DEC-201 Q6): rdashd
/// never writes to one.
pub fn refuse_core_data_dir(args: &ArgsManager) -> Result<(), InitFailure> {
    refuse_core_dir(&args.get_data_dir_net()?).map_err(InitFailure::Error)
}

/// The refusal, before the config file is read (which creates an empty `dash.conf` in it), of a
/// base data directory that holds a Dash Core data directory for any network: the base itself
/// (mainnet), `testnet3`, `regtest` or a `devnet-*`.
pub fn refuse_core_base_dir(args: &ArgsManager) -> Result<(), InitFailure> {
    let base = args.get_data_dir_base()?;
    refuse_core_dir(&base).map_err(InitFailure::Error)?;
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Ok(());
    };
    let mut nets: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name == "testnet3" || name == "regtest" || name.starts_with("devnet-")
        })
        .map(|e| e.path())
        .collect();
    nets.sort();
    nets.iter().try_for_each(|net| refuse_core_dir(net)).map_err(InitFailure::Error)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    use dashd_chainstate::store::CHAINSTATE_SCHEMA;
    use dashd_store::{RocksConfig, RocksEngine};

    use super::*;

    fn args_for(datadir: &Path, extra: &[&str]) -> ArgsManager {
        let args = ArgsManager::new();
        dash_args::Program::Dashd.setup_args(&args);
        let datadir = format!("-datadir={}", datadir.display());
        let mut argv = vec!["dashd", datadir.as_str(), "-regtest"];
        argv.extend_from_slice(extra);
        args.parse_parameters(&argv).unwrap();
        args.select_base_params("regtest").unwrap();
        // `InitConfig` creates it.
        std::fs::create_dir_all(args.get_data_dir_net().unwrap()).unwrap();
        args
    }

    #[test]
    fn the_pid_file_holds_the_pid_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let args = args_for(dir.path(), &[]);
        let pid = PidFile::create(&args).unwrap();
        assert_eq!(pid.path(), dir.path().join("regtest").join(PID_FILENAME));
        assert_eq!(
            std::fs::read_to_string(pid.path()).unwrap(),
            format!("{}\n", std::process::id())
        );
        let path = pid.path().to_owned();
        pid.remove();
        assert!(!path.exists());

        // `-pid` names another file, relative to the network data directory.
        let args = args_for(dir.path(), &["-pid=sub/x.pid"]);
        let Err(InitFailure::Error(text)) = PidFile::create(&args) else {
            panic!()
        };
        let path = dir.path().join("regtest/sub/x.pid");
        assert_eq!(
            text,
            format!(
                "Unable to create the PID file '{}': No such file or directory (2)",
                path.display()
            )
        );
    }

    #[test]
    fn a_core_data_directory_is_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let net = dir.path();
        assert_eq!(probe_core_data_dir(net), None);

        // RocksDB (rdashd's own) is not LevelDB.
        let chainstate = net.join("chainstate");
        std::fs::create_dir_all(&chainstate).unwrap();
        for f in ["CURRENT", "MANIFEST-000005", "OPTIONS-000007"] {
            std::fs::write(chainstate.join(f), b"").unwrap();
        }
        assert_eq!(probe_core_data_dir(net), None);
        std::fs::remove_file(chainstate.join("OPTIONS-000007")).unwrap();
        assert_eq!(probe_core_data_dir(net), Some(CoreDataDirSign::LevelDb(chainstate.clone())));
        std::fs::remove_dir_all(&chainstate).unwrap();

        // What a first start killed at RocksDB's `WriteOptionsFile` leaves (BL-147 (b)): its
        // `IDENTITY` says RocksDB. Without it, the same files would be LevelDB's.
        std::fs::create_dir_all(&chainstate).unwrap();
        for f in ["000004.log", "CURRENT", "IDENTITY", "LOCK", "LOG", "MANIFEST-000001"] {
            std::fs::write(chainstate.join(f), b"").unwrap();
        }
        std::fs::write(chainstate.join("MANIFEST-000005"), b"").unwrap();
        assert_eq!(probe_core_data_dir(net), None);
        std::fs::remove_file(chainstate.join("IDENTITY")).unwrap();
        assert_eq!(probe_core_data_dir(net), Some(CoreDataDirSign::LevelDb(chainstate.clone())));
        std::fs::remove_dir_all(&chainstate).unwrap();

        // A Core chainstate with a table, as dashd v24 leaves one: its `*.ldb` says LevelDB even
        // after a RocksDB tool killed while opening it added `IDENTITY`.
        std::fs::create_dir_all(&chainstate).unwrap();
        for f in ["000005.ldb", "000006.log", "CURRENT", "IDENTITY", "LOCK", "MANIFEST-000004"] {
            std::fs::write(chainstate.join(f), b"").unwrap();
        }
        assert_eq!(probe_core_data_dir(net), Some(CoreDataDirSign::LevelDb(chainstate.clone())));
        // So does a mark, which the store never leaves beside a table (BL-509 r1 n-2).
        std::fs::write(chainstate.join(STORE_MARKER), b"").unwrap();
        assert_eq!(probe_core_data_dir(net), Some(CoreDataDirSign::LevelDb(chainstate.clone())));
        std::fs::remove_dir_all(&chainstate).unwrap();

        // LevelDB's own files, as Core's `blocks/index` holds them.
        let index = net.join("blocks/index");
        std::fs::create_dir_all(&index).unwrap();
        for f in ["000003.log", "CURRENT", "LOCK", "MANIFEST-000002"] {
            std::fs::write(index.join(f), b"").unwrap();
        }
        assert_eq!(probe_core_data_dir(net), Some(CoreDataDirSign::LevelDb(index.clone())));
        std::fs::remove_dir_all(&index).unwrap();

        std::fs::create_dir(net.join("llmq")).unwrap();
        assert_eq!(probe_core_data_dir(net), Some(CoreDataDirSign::DashDir(net.join("llmq"))));
    }

    /// [`is_leveldb`] rests on RocksDB writing `IDENTITY` when it creates a database, before its
    /// `CURRENT`: the node's engine does, with the chainstate's schema, and so the database is not
    /// taken for Core's even without its `OPTIONS` file (BL-147 (b)). That `IDENTITY` comes before
    /// `CURRENT` is [`rocksdb_writes_its_identity_before_its_current`]'s.
    #[test]
    fn a_rocksdb_the_node_makes_has_its_identity_file() {
        let dir = tempfile::tempdir().unwrap();
        let chainstate = dir.path().join("chainstate");
        let schema = &dashd_chainstate::store::CHAINSTATE_SCHEMA;
        drop(dashd_store::RocksEngine::open(&chainstate, schema, &Default::default()).unwrap());
        assert!(
            chainstate.join("IDENTITY").is_file(),
            "RocksDB no longer writes IDENTITY (a RocksDB upgrade, or `write_identity_file` off): \
             datadir::is_leveldb would refuse a first start killed before OPTIONS for good"
        );
        for e in std::fs::read_dir(&chainstate).unwrap() {
            let path = e.unwrap().path();
            if path.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-") {
                std::fs::remove_file(path).unwrap();
            }
        }
        assert_eq!(probe_core_data_dir(dir.path()), None);
    }

    /// The order of [`a_rocksdb_the_node_makes_has_its_identity_file`]'s `IDENTITY` (Sol r1 M1):
    /// a RocksDB that made `CURRENT` first, or that dropped `IDENTITY` on a reopen, would leave a
    /// window in which a killed first start is refused as Core's for good, while the file is still
    /// there after every open. The node's engine creates the chainstate and reopens it, watched
    /// by inotify: `IDENTITY` appears before the first `CURRENT`, and no open removes it. Each is
    /// a rename of a synced temporary file followed by a directory fsync (RocksDB 11.8.1
    /// `SetIdentityFile`, `SetCurrentFile`), so the order they appear in is the order they are
    /// durable in.
    #[cfg(target_os = "linux")]
    #[test]
    fn rocksdb_writes_its_identity_before_its_current() {
        let dir = tempfile::tempdir().unwrap();
        let chainstate = dir.path().join("chainstate");
        std::fs::create_dir(&chainstate).unwrap();
        let schema = &dashd_chainstate::store::CHAINSTATE_SCHEMA;
        let open = || {
            drop(dashd_store::RocksEngine::open(&chainstate, schema, &Default::default()).unwrap())
        };
        let created = names_changed_in(&chainstate, open);
        let added = |name: &str| created.iter().position(|(added, n)| *added && n == name);
        let (Some(identity), Some(current)) = (added("IDENTITY"), added("CURRENT")) else {
            panic!("RocksDB made no IDENTITY or no CURRENT: {created:?}")
        };
        assert!(identity < current, "RocksDB made CURRENT before IDENTITY: {created:?}");
        let reopened = names_changed_in(&chainstate, open);
        for events in [&created, &reopened] {
            assert!(!events.contains(&(false, "IDENTITY".into())), "{events:?}");
        }
        assert!(chainstate.join("IDENTITY").is_file());
    }

    /// The names added to `dir` (`true`: created or moved in) and removed from it (`false`) while
    /// `f` runs, in order, from an inotify watch on `dir`.
    #[cfg(target_os = "linux")]
    fn names_changed_in(dir: &Path, f: impl FnOnce()) -> Vec<(bool, String)> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).unwrap();
        // SAFETY: no arguments but flags; the descriptor is owned below.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0, "{}", io::Error::last_os_error());
        // SAFETY: `fd` is a new descriptor nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mask = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_DELETE | libc::IN_MOVED_FROM;
        // SAFETY: `fd` is open and `path` is a NUL-terminated path, both alive for the call.
        let watch = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), path.as_ptr(), mask) };
        assert!(watch >= 0, "{}", io::Error::last_os_error());
        f();
        let mut events = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            // SAFETY: `buf` is writable for its length and `fd` is open.
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            let Ok(n) = usize::try_from(n) else {
                let e = io::Error::last_os_error();
                assert_eq!(e.kind(), io::ErrorKind::WouldBlock, "{e}");
                return events;
            };
            // Each event: `wd: i32, mask: u32, cookie: u32, len: u32`, then `len` bytes of
            // NUL-padded name.
            let mut rest = &buf[..n];
            while !rest.is_empty() {
                let word = |i: usize| u32::from_ne_bytes(rest[i..i + 4].try_into().unwrap());
                let (mask, len) = (word(4), word(12) as usize);
                assert_eq!(mask & libc::IN_Q_OVERFLOW, 0, "the inotify queue overflowed");
                let name = &rest[16..16 + len];
                let name = name.split(|&b| b == 0).next().unwrap_or_default();
                let added = mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0;
                events.push((added, String::from_utf8_lossy(name).into_owned()));
                rest = &rest[16 + len..];
            }
        }
    }

    /// The files a power loss left in `chainstate/` while rdashd made its first one (ST3b
    /// run 1793266802-2137011, point 27; DEC-718), and with the WAL RocksDB makes next: no
    /// `OPTIONS-*` yet.
    const CRASHED_STORES: [&[&str]; 2] = [
        &["CURRENT", "MANIFEST-000001", "IDENTITY", "LOCK", "LOG"],
        &["CURRENT", "MANIFEST-000001", "IDENTITY", "LOCK", "LOG", "000004.log"],
    ];

    /// The mark as the store's open leaves it before RocksDB writes anything.
    pub(crate) fn mark(store: &Path) {
        std::fs::create_dir_all(store).unwrap();
        std::fs::write(store.join(STORE_MARKER), b"rdashd store\n").unwrap();
    }

    /// BL-509: a chainstate cut off while rdashd makes it is LevelDB-shaped, which nobody's lock
    /// explains after the crash, so without its mark every later start refused it as Dash Core's.
    /// With the mark, written before RocksDB makes anything, it is rdashd's, and so is the mark
    /// alone (a power loss right after it) or the directory alone (one before it).
    #[test]
    fn a_chainstate_cut_off_while_rdashd_makes_it_is_rdashd_s() {
        for files in CRASHED_STORES {
            let dir = tempfile::tempdir().unwrap();
            let args = args_for(dir.path(), &[]);
            let net = args.get_data_dir_net().unwrap();
            let chainstate = net.join("chainstate");
            mark(&chainstate);
            for f in files {
                std::fs::write(chainstate.join(f), b"").unwrap();
            }
            assert_eq!(probe_core_data_dir(&net), None, "{files:?}");
            assert_eq!(refuse_core_data_dir(&args), Ok(()), "{files:?}");
            // The same files unmarked, as an rdashd before the mark left them: its `IDENTITY`
            // says RocksDB (BL-147 (b)). Without that too, refused.
            std::fs::remove_file(chainstate.join(STORE_MARKER)).unwrap();
            assert_eq!(probe_core_data_dir(&net), None, "{files:?} unmarked");
            std::fs::remove_file(chainstate.join("IDENTITY")).unwrap();
            let sign = Some(CoreDataDirSign::LevelDb(chainstate.clone()));
            assert_eq!(probe_core_data_dir(&net), sign, "{files:?}");
        }

        let dir = tempfile::tempdir().unwrap();
        let chainstate = dir.path().join("chainstate");
        std::fs::create_dir(&chainstate).unwrap();
        assert_eq!(probe_core_data_dir(dir.path()), None, "the directory alone");
        mark(&chainstate);
        assert_eq!(probe_core_data_dir(dir.path()), None, "the mark alone");

        // The mark covers `blocks/index` too, though rdashd keeps its index in `chainstate/`.
        let index = dir.path().join("blocks/index");
        mark(&index);
        for f in CRASHED_STORES[0] {
            std::fs::write(index.join(f), b"").unwrap();
        }
        assert_eq!(probe_core_data_dir(dir.path()), None);
    }

    /// BL-509 r1 M-1 and m-1: the store marks every chainstate it creates, not only an empty
    /// one: a filesystem of its own (`lost+found`), or a pre-mark rdashd's early debris. A power
    /// loss before RocksDB's `OPTIONS-*` (a created store without them) is then rdashd's.
    #[test]
    fn every_chainstate_the_store_creates_is_marked() {
        for case in ["lost+found", "pre-mark debris"] {
            let dir = tempfile::tempdir().unwrap();
            let chainstate = dir.path().join("chainstate");
            if case == "lost+found" {
                std::fs::create_dir_all(chainstate.join("lost+found")).unwrap();
            } else {
                std::fs::create_dir(&chainstate).unwrap();
                for f in ["LOG", "LOCK", "IDENTITY"] {
                    std::fs::write(chainstate.join(f), b"").unwrap();
                }
            }
            let config = RocksConfig::default();
            drop(RocksEngine::open(&chainstate, &CHAINSTATE_SCHEMA, &config).unwrap());
            for e in std::fs::read_dir(&chainstate).unwrap() {
                let path = e.unwrap().path();
                if path.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-") {
                    std::fs::remove_file(path).unwrap();
                }
            }
            assert_eq!(probe_core_data_dir(dir.path()), None, "{case}");
            std::fs::remove_file(chainstate.join(STORE_MARKER)).unwrap();
            assert_eq!(probe_core_data_dir(dir.path()), None, "{case} unmarked (IDENTITY)");
            std::fs::remove_file(chainstate.join("IDENTITY")).unwrap();
            let sign = Some(CoreDataDirSign::LevelDb(chainstate));
            assert_eq!(probe_core_data_dir(dir.path()), sign, "{case} unmarked, no IDENTITY");
        }
    }

    #[test]
    fn the_refusal_names_the_directory_and_the_import_option() {
        let dir = tempfile::tempdir().unwrap();
        let args = args_for(dir.path(), &[]);
        let net = args.get_data_dir_net().unwrap();
        assert_eq!(refuse_core_data_dir(&args), Ok(()));
        std::fs::create_dir_all(net.join("evodb")).unwrap();
        let shown = net.display();
        assert_eq!(
            refuse_core_data_dir(&args),
            Err(InitFailure::Error(format!(
                "{shown} holds a Dash Core data directory; rdashd does not write to it. To import \
                 it, use -importcoredatadir={shown}."
            )))
        );
    }

    /// A LevelDB-shaped database is decided under the lock (D21): while another process holds the
    /// directory the answer is the lock error, and the refusal once it is free. The process's own
    /// lock does not stand in the way, and survives the check. A certain sign is refused at once,
    /// held or not. Nothing is written, not even a lock file.
    #[cfg(unix)]
    #[test]
    fn a_leveldb_sign_is_decided_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let args = args_for(dir.path(), &[]);
        let net = args.get_data_dir_net().unwrap();
        let chainstate = net.join("chainstate");
        std::fs::create_dir_all(&chainstate).unwrap();
        for f in ["CURRENT", "MANIFEST-000001"] {
            std::fs::write(chainstate.join(f), b"").unwrap();
        }
        let Err(refusal) = refuse_core_data_dir(&args) else {
            panic!()
        };
        assert!(!net.join(LOCK_FILENAME).exists());

        let mut holder = spawn_lock_holder(&net);
        assert_eq!(
            refuse_core_data_dir(&args),
            Err(InitFailure::Error(format!(
                "Cannot obtain a lock on data directory {}. rdashd is probably already running.",
                net.display()
            )))
        );
        std::fs::create_dir(net.join("evodb")).unwrap();
        assert_eq!(refuse_core_data_dir(&args), Err(refusal.clone()));
        std::fs::remove_dir(net.join("evodb")).unwrap();
        drop(holder.stdin.take());
        assert!(holder.wait().unwrap().success());
        assert_eq!(refuse_core_data_dir(&args), Err(refusal.clone()));

        let _own = lock_directory(&net, LOCK_FILENAME, false).unwrap().unwrap();
        assert_eq!(refuse_core_data_dir(&args), Err(refusal));
        assert!(another_process_is_refused(&net));
    }

    #[test]
    fn writable_and_lockable_in_process() {
        let dir = tempfile::tempdir().unwrap();
        assert!(dir_is_writable(dir.path()));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(!dir_is_writable(&dir.path().join("missing")));
        assert!(lock_directory(dir.path(), LOCK_FILENAME, true).unwrap().is_none());
        let lock = lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().unwrap();
        assert_eq!(lock.path(), dir.path().join(LOCK_FILENAME));
        // As Core: the process's own lock is not taken again (nor released by a probe); another
        // acquisition is another owner of it.
        assert!(lock_directory(dir.path(), LOCK_FILENAME, true).unwrap().is_none());
        let second = lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().unwrap();
        assert_eq!(held()[&second.key].1, 2);
        drop(lock);
        drop(second);
        assert!(!held().contains_key(&dir.path().canonicalize().unwrap().join(LOCK_FILENAME)));
        assert!(lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().is_some());
    }

    /// The process's own lock survives its probes: another process still cannot take it.
    #[cfg(unix)]
    #[test]
    fn probing_a_held_lock_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let _lock = lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().unwrap();
        assert!(lock_directory(dir.path(), LOCK_FILENAME, true).unwrap().is_none());
        let spelled = dir.path().join(".").join("..").join(dir.path().file_name().unwrap());
        assert!(lock_directory(&spelled, LOCK_FILENAME, true).unwrap().is_none());
        assert!(another_process_is_refused(dir.path()));
    }

    /// The lock outlives its first owner: another process can take it only once every owner in
    /// this process has dropped.
    #[cfg(unix)]
    #[test]
    fn the_lock_is_released_by_its_last_owner() {
        let dir = tempfile::tempdir().unwrap();
        let first = lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().unwrap();
        let second = lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().unwrap();
        drop(first);
        assert!(another_process_is_refused(dir.path()));
        drop(second);
        assert!(!another_process_is_refused(dir.path()));
    }

    /// Whether a probe of `dir`'s lock from another process ([`lock_holder`]) is refused.
    #[cfg(unix)]
    pub(crate) fn another_process_is_refused(dir: &Path) -> bool {
        let probe = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "datadir::tests::lock_holder", "--ignored", "--nocapture"])
            .env("DASHD_NODE_TEST_LOCK_DIR", dir)
            .env("DASHD_NODE_TEST_LOCK_PROBE", "1")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&probe.stdout);
        assert!(out.contains("probe "), "{out}");
        out.contains("probe refused")
    }

    /// Holds the lock on `DASHD_NODE_TEST_LOCK_DIR` until its stdin closes; with
    /// `DASHD_NODE_TEST_LOCK_PROBE` set, only probes it and prints the outcome.
    #[test]
    #[ignore = "helper process"]
    fn lock_holder() {
        let Some(dir) = std::env::var_os("DASHD_NODE_TEST_LOCK_DIR") else {
            return;
        };
        if std::env::var_os("DASHD_NODE_TEST_LOCK_PROBE").is_some() {
            let refused = lock_directory(Path::new(&dir), LOCK_FILENAME, true).is_err();
            println!(
                "{}",
                if refused {
                    "probe refused"
                } else {
                    "probe locked"
                }
            );
            return;
        }
        let _lock = lock_directory(Path::new(&dir), LOCK_FILENAME, false).unwrap().unwrap();
        println!("locked");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    /// Starts [`lock_holder`] on `dir` and returns once it holds the lock; closing the child's
    /// stdin releases it.
    pub(crate) fn spawn_lock_holder(dir: &Path) -> std::process::Child {
        let mut holder = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "datadir::tests::lock_holder", "--ignored", "--nocapture"])
            .env("DASHD_NODE_TEST_LOCK_DIR", dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(holder.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("locked") {
            line.clear();
            assert_ne!(out.read_line(&mut line).unwrap(), 0, "the holder exited");
        }
        // The rest of its test harness output.
        std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        holder
    }

    #[cfg(unix)]
    #[test]
    fn another_process_cannot_take_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut holder = spawn_lock_holder(dir.path());
        let err = lock_directory(dir.path(), LOCK_FILENAME, true).unwrap_err();
        // EAGAIN or EACCES, as `fcntl(F_SETLK)` reports a conflict.
        assert!(
            err == sys_error_string(&io::Error::from_raw_os_error(libc::EAGAIN))
                || err == sys_error_string(&io::Error::from_raw_os_error(libc::EACCES)),
            "{err}"
        );
        drop(holder.stdin.take());
        assert!(holder.wait().unwrap().success());
        assert!(lock_directory(dir.path(), LOCK_FILENAME, false).unwrap().is_some());
    }

    #[test]
    fn sys_error_strings_are_core_s() {
        assert_eq!(
            sys_error_string(&io::Error::from_raw_os_error(2)),
            if cfg!(windows) {
                "The system cannot find the file specified. (2)"
            } else {
                "No such file or directory (2)"
            }
        );
    }
}
