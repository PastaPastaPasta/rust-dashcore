//! The filesystem operations block files need, behind [`FlatFs`].
//!
//! [`OsFs`] is the real implementation. The trait is narrow and path-based so that the power-loss
//! model of A1 §3.5.8 (task ST3a, `FaultyFlatFiles`) can stand in for the OS: every write, size
//! change, data sync and directory sync goes through it, and nothing else touches block files.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::flatfile::{FlatFs, FlatRead, OpenFailed};

/// How many open file handles [`OsFs`] keeps. Core opens and closes a `FILE*` per operation; we
/// keep the recently used ones open instead. A handle written through since its last
/// `fdatasync` is never closed (so the cache can exceed this): if writeback of its pages failed
/// and the inode were evicted, an `fdatasync` on a fresh descriptor would report success for data
/// that is gone. The barrier syncs through the descriptor that wrote.
const HANDLE_CACHE: usize = 64;

struct Handle {
    file: Arc<File>,
    writable: bool,
    /// Written, allocated or truncated since the last successful `fdatasync`.
    dirty: bool,
    last_use: u64,
}

#[derive(Default)]
struct Handles {
    map: HashMap<PathBuf, Handle>,
    tick: u64,
}

/// The operating system's filesystem.
///
/// A read-only instance opens every file `O_RDONLY` and fails every mutating call with
/// `PermissionDenied`, which makes it safe to point at another node's datadir.
pub struct OsFs {
    read_only: bool,
    handles: Mutex<Handles>,
}

impl OsFs {
    pub fn new() -> Self {
        Self {
            read_only: false,
            handles: Mutex::default(),
        }
    }

    pub fn read_only() -> Self {
        Self {
            read_only: true,
            handles: Mutex::default(),
        }
    }

    fn deny(&self) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only block files"));
        }
        Ok(())
    }

    /// A cached handle for `path`; `writable` opens (and creates) it read-write.
    fn handle(&self, path: &Path, writable: bool) -> io::Result<Arc<File>> {
        self.handle_inner(path, writable, false)
    }

    /// A writable handle that is about to modify the file: pinned until it is synced.
    fn dirty_handle(&self, path: &Path) -> io::Result<Arc<File>> {
        self.handle_inner(path, true, true)
    }

    fn handle_inner(&self, path: &Path, writable: bool, dirty: bool) -> io::Result<Arc<File>> {
        if writable {
            self.deny()?;
        }
        let mut h = self.handles.lock().expect("handle cache poisoned");
        h.tick += 1;
        let tick = h.tick;
        if let Some(e) = h.map.get_mut(path)
            && (e.writable || !writable)
        {
            e.last_use = tick;
            e.dirty |= dirty;
            return Ok(Arc::clone(&e.file));
        }
        let file = if writable {
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?
        } else {
            File::open(path)?
        };
        let file = Arc::new(file);
        if h.map.len() >= HANDLE_CACHE
            && !h.map.contains_key(path)
            && let Some(oldest) = h
                .map
                .iter()
                .filter(|(_, e)| !e.dirty)
                .min_by_key(|(_, e)| e.last_use)
                .map(|(p, _)| p.clone())
        {
            h.map.remove(&oldest);
        }
        // A writable open replaces a read-only handle, which is never dirty.
        h.map.insert(
            path.to_path_buf(),
            Handle {
                file: Arc::clone(&file),
                writable,
                dirty,
                last_use: tick,
            },
        );
        Ok(file)
    }

    fn mark_synced(&self, path: &Path, file: &Arc<File>) {
        let mut h = self.handles.lock().expect("handle cache poisoned");
        if let Some(e) = h.map.get_mut(path)
            && Arc::ptr_eq(&e.file, file)
        {
            e.dirty = false;
        }
    }

    #[cfg(test)]
    pub(crate) fn open_handles(&self) -> (usize, usize) {
        let h = self.handles.lock().expect("handle cache poisoned");
        (h.map.len(), h.map.values().filter(|e| e.dirty).count())
    }

    fn forget(&self, path: &Path) {
        self.handles.lock().expect("handle cache poisoned").map.remove(path);
    }

    fn cached(&self, path: &Path) -> Option<Arc<File>> {
        let h = self.handles.lock().expect("handle cache poisoned");
        h.map.get(path).map(|e| Arc::clone(&e.file))
    }
}

/// A file [`OsFs::open_read`] opened.
struct OpenedFile(File);

impl FlatRead for OpenedFile {
    fn len(&self) -> io::Result<u64> {
        self.0.metadata().map(|m| m.len())
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.read_exact_at(buf, offset)
    }
}

impl Default for OsFs {
    fn default() -> Self {
        Self::new()
    }
}

fn not_found_as_none<T>(r: io::Result<T>) -> io::Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Core's macOS branch, which takes `offset` to be the file's size: reserve `len` bytes past the
/// physical end of the file, contiguous if it can, then set its size to `offset + len`. Failures
/// are ignored, as Core ignores them.
// Core: util/fs_helpers.cpp:249-263
#[cfg(target_os = "macos")]
fn allocate_file_range(file: &File, offset: u64, len: u64) {
    use std::os::unix::io::AsRawFd;
    // Core's length is an `unsigned int`, which always fits.
    let Ok(length) = libc::off_t::try_from(len) else {
        return;
    };
    let mut fst = libc::fstore_t {
        fst_flags: libc::F_ALLOCATECONTIG,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: length,
        fst_bytesalloc: 0,
    };
    // SAFETY: `file` is an open descriptor for the duration of the call, and `fst` is a valid
    // `fstore_t` that F_PREALLOCATE reads and writes.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut fst) } == -1 {
        fst.fst_flags = libc::F_ALLOCATEALL;
        // SAFETY: as above.
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut fst) };
    }
    let _ = file.set_len(offset.saturating_add(len));
}

/// Core's `HAVE_POSIX_FALLOCATE` branch: reserve the file from its start to `offset + len`, else
/// fall back to writing zeros.
// Core: util/fs_helpers.cpp:265-269
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "illumos",
    target_os = "solaris"
))]
fn allocate_file_range(file: &File, offset: u64, len: u64) {
    use std::os::unix::io::AsRawFd;
    if let Ok(end) = libc::off_t::try_from(offset.saturating_add(len)) {
        // SAFETY: `file` is an open descriptor for the duration of the call; posix_fallocate
        // only reads its integer arguments.
        if unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, end) } == 0 {
            return;
        }
    }
    zero_fill(file, offset, len);
}

/// Any other Unix has only Core's fallback. Core builds its `MAC_OSX` branch for macOS alone, so
/// other Apple OSes get the fallback too.
#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "illumos",
    target_os = "solaris"
)))]
fn allocate_file_range(file: &File, offset: u64, len: u64) {
    zero_fill(file, offset, len);
}

/// Core's fallback: write `len` zeros from `offset`, 64 KiB at a time. Failures are ignored, as
/// Core ignores `fwrite`'s. A range that ends past `u64::MAX` writes nothing: the offset would
/// wrap into live data.
// Core: util/fs_helpers.cpp:270-282
#[cfg(not(target_os = "macos"))]
pub(super) fn zero_fill(file: &File, mut offset: u64, mut len: u64) {
    if offset.checked_add(len).is_none() {
        return;
    }
    let zeros = [0u8; 65536];
    while len > 0 {
        let now = len.min(zeros.len() as u64);
        let _ = file.write_all_at(&zeros[..now as usize], offset);
        offset += now;
        len -= now;
    }
}

fn einval_is_ok(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        r => r,
    }
}

impl FlatFs for OsFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        // fstat on an open handle sees the inode we would write through, even if the path was
        // replaced underneath us.
        if let Some(file) = self.cached(path) {
            let nlink = file.metadata()?.nlink();
            if nlink > 0 {
                return Ok(Some(nlink));
            }
            // Unlinked under us: the path no longer names this inode.
            self.forget(path);
        }
        not_found_as_none(std::fs::metadata(path)).map(|m| m.map(|m| m.nlink()))
    }

    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        if let Some(file) = self.cached(path) {
            return file.metadata().map(|m| Some(m.len()));
        }
        not_found_as_none(std::fs::metadata(path)).map(|m| m.map(|m| m.len()))
    }

    fn create(&self, path: &Path) -> io::Result<()> {
        self.handle(path, true).map(|_| ())
    }

    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.handle(path, false).map_err(OpenFailed::wrap)?.read_exact_at(buf, offset)
    }

    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.dirty_handle(path)?.write_all_at(data, offset)
    }

    // Core: flatfile.cpp:39-52 FlatFileSeq::Open (read_only)
    /// A descriptor of its own, closed when the reader drops: never one from the handle cache,
    /// which may hold a file since unlinked (CH3 D46).
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        let file = File::open(path).map_err(OpenFailed::wrap)?;
        Ok(Box::new(OpenedFile(file)))
    }

    // Core: util/fs_helpers.cpp:238 AllocateFileRange
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        let Ok(file) = self.dirty_handle(path) else {
            return;
        };
        allocate_file_range(&file, offset, len);
    }

    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.dirty_handle(path)?.set_len(len)
    }

    fn sync_data(&self, path: &Path) -> io::Result<()> {
        let file = self.handle(path, false)?;
        einval_is_ok(file.sync_data())?;
        self.mark_synced(path, &file);
        Ok(())
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        einval_is_ok(File::open(dir)?.sync_all())
    }

    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.deny()?;
        self.forget(to);
        // Written through the cached, pinned handle, so the caller's sync_data syncs the
        // descriptor that wrote (see HANDLE_CACHE).
        let out = self.dirty_handle(to)?;
        out.set_len(0)?;
        let mut src = File::open(from)?;
        io::copy(&mut src, &mut &*out)?;
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.deny()?;
        self.forget(from);
        self.forget(to);
        let r = std::fs::rename(from, to);
        // A concurrent reader may have cached the old file in between.
        self.forget(from);
        self.forget(to);
        r
    }

    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.deny()?;
        self.forget(path);
        let r = not_found_as_none(std::fs::remove_file(path)).map(|r| r.is_some());
        // A concurrent reader may have cached the old file in between.
        self.forget(path);
        r
    }

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.deny()?;
        std::fs::create_dir_all(dir)
    }

    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: `c` is a valid NUL-terminated path and `st` points to writable memory of the
        // right type; statvfs initialises it on success.
        if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: statvfs returned 0, so it filled the struct.
        let st = unsafe { st.assume_init() };
        // The field types differ between platforms (u64 on Linux).
        #[allow(clippy::unnecessary_cast)]
        Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
    }
}
