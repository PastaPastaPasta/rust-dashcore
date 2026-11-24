//! Filesystem abstraction used by the import state machine.
//!
//! Every filesystem access of the importer goes through [`ImportFs`], so the
//! power-loss model (`CutFs`) sees each one. No method opens an existing path
//! for writing except `append` and `truncate`, which the importer uses only on
//! its own journal and report files.
use bitcoin_hashes::{HashEngine, sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(feature = "test-hooks")]
use std::sync::atomic::{AtomicBool, Ordering};

/// With the `test-hooks` feature, forces every [`OsImportFs::reflink`] to fail
/// with `EOPNOTSUPP` (O3-5). The node wiring slice owns the runtime trigger.
#[cfg(feature = "test-hooks")]
pub static FORCE_REFLINK_FAILURE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    pub len: u64,
    pub modified: u128,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
}

impl FileMetadata {
    pub fn id(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }
}

pub trait ImportFs: Send + Sync {
    /// Clones `src` into a new file `dst`; fails if `dst` exists.
    fn reflink(&self, src: &Path, dst: &Path) -> io::Result<()>;
    /// Links `src` at the new name `dst`; fails if `dst` exists.
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()>;
    /// Copies `src` into a new file `dst`; fails if `dst` exists.
    fn copy(&self, src: &Path, dst: &Path) -> io::Result<u64>;
    fn sync_data(&self, path: &Path) -> io::Result<()>;
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    fn rename(&self, src: &Path, dst: &Path) -> io::Result<()>;
    /// Removes a file or a directory tree; a missing path is not an error.
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()>;
    /// Appends to `path`, creating it if it does not exist.
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// `symlink_metadata`; never opens the path.
    fn metadata(&self, path: &Path) -> io::Result<Option<FileMetadata>>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn available_space(&self, path: &Path) -> io::Result<u64>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    /// The mount table as `/proc/self/mountinfo` text, or `None` on a target
    /// without one (not Linux).
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>>;
    /// The kernel's mount id for `path` (`statx` `stx_mnt_id`, symlinks not
    /// followed), the first field of its mount-table line; `None` where the
    /// kernel or target gives none (`ENOSYS`, or `STATX_MNT_ID` missing from
    /// `stx_mask`).
    fn mount_id(&self, path: &Path) -> io::Result<Option<u64>>;
    /// Streams the sha256 of a file.
    fn sha256(&self, path: &Path) -> io::Result<[u8; 32]> {
        Ok(sha256::Hash::hash(&self.read(path)?).to_byte_array())
    }
    /// Wall clock for the per-phase timings, in milliseconds since the epoch.
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OsImportFs;
impl OsImportFs {
    pub fn new() -> Self {
        Self
    }
}

fn create_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Opens for reading without following a final symlink.
fn open_ro(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
}

fn open_rw(path: &Path, append: bool) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = OpenOptions::new();
    o.custom_flags(libc::O_NOFOLLOW);
    if append {
        o.create(true).append(true);
    } else {
        o.write(true);
    }
    o.open(path)
}

impl ImportFs for OsImportFs {
    fn reflink(&self, src: &Path, dst: &Path) -> io::Result<()> {
        #[cfg(feature = "test-hooks")]
        if FORCE_REFLINK_FAILURE.load(Ordering::Relaxed) {
            return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let a = open_ro(src)?;
            let b = create_new(dst)?;
            // SAFETY: both descriptors are open for the duration of the call.
            let r = unsafe { libc::ioctl(b.as_raw_fd(), libc::FICLONE as _, a.as_raw_fd()) };
            if r == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            drop(b);
            let _ = fs::remove_file(dst);
            Err(e)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (src, dst);
            Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
        }
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        fs::hard_link(src, dst)
    }
    fn copy(&self, src: &Path, dst: &Path) -> io::Result<u64> {
        let mut input = open_ro(src)?;
        let mut output = create_new(dst)?;
        io::copy(&mut input, &mut output)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        open_ro(path)?.sync_data()
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }
    fn rename(&self, src: &Path, dst: &Path) -> io::Result<()> {
        fs::rename(src, dst)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        open_rw(path, false)?.set_len(len)
    }
    fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut f = open_rw(path, true)?;
        f.write_all(data)
    }
    fn metadata(&self, path: &Path) -> io::Result<Option<FileMetadata>> {
        use std::os::unix::fs::MetadataExt;
        match fs::symlink_metadata(path) {
            Ok(m) => Ok(Some(FileMetadata {
                len: m.len(),
                modified: (m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128) as u128,
                dev: m.dev(),
                ino: m.ino(),
                nlink: m.nlink(),
                is_dir: m.is_dir(),
                is_symlink: m.file_type().is_symlink(),
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> =
            fs::read_dir(path)?.map(|e| e.map(|e| e.path())).collect::<io::Result<_>>()?;
        out.sort();
        Ok(out)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        open_ro(path)?.read_to_end(&mut out)?;
        Ok(out)
    }
    fn sha256(&self, path: &Path) -> io::Result<[u8; 32]> {
        let mut file = open_ro(path)?;
        let mut engine = sha256::Hash::engine();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            engine.input(&buf[..n]);
        }
        Ok(sha256::Hash::from_engine(engine).to_byte_array())
    }
    fn available_space(&self, path: &Path) -> io::Result<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: `s` is a plain C struct that statvfs fills in.
        let mut s = unsafe { std::mem::zeroed::<libc::statvfs>() };
        // SAFETY: `c` is NUL-terminated and `s` is a valid out pointer.
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(s.f_bavail as u64 * s.f_frsize as u64)
        }
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
    fn mount_table(&self) -> io::Result<Option<Vec<u8>>> {
        #[cfg(target_os = "linux")]
        {
            fs::read(super::mounts::MOUNTINFO).map(Some)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(None)
        }
    }
    fn mount_id(&self, path: &Path) -> io::Result<Option<u64>> {
        #[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
        {
            use std::os::unix::ffi::OsStrExt;
            let c = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
            // SAFETY: `s` is a plain C struct that statx fills in.
            let mut s = unsafe { std::mem::zeroed::<libc::statx>() };
            // SAFETY: `c` is NUL-terminated and `s` is a valid out pointer.
            let r = unsafe {
                libc::statx(
                    libc::AT_FDCWD,
                    c.as_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                    libc::STATX_MNT_ID,
                    &mut s,
                )
            };
            if r != 0 {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::ENOSYS) => Ok(None),
                    _ => Err(e),
                };
            }
            Ok((s.stx_mask & libc::STATX_MNT_ID != 0).then_some(s.stx_mnt_id))
        }
        #[cfg(not(all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))))]
        {
            let _ = path;
            Ok(None)
        }
    }
}
