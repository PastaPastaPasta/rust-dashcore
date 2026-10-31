//! The filesystem seam block files go through, behind [`FlatFs`].
//!
//! The trait is narrow and path-based so that the power-loss model of A1 §3.5.8 (task ST3a,
//! `faulty::FaultyFlatFiles`, feature `faulty`) can stand in for the OS: every write, size change,
//! data sync and directory sync goes through it. The node's block files
//! (`dashd-store::flatfile`) and its real implementation (`OsFs`) stay in `dashd-store`.

use std::io;
use std::path::{Path, PathBuf};

/// Filesystem primitives for flat files. Paths are absolute file paths inside one directory.
///
/// Every mutating call creates the file if it does not exist (Core opens with `rb+`, then `wb+`).
pub trait FlatFs: Send + Sync {
    /// The file's link count (`st_nlink`), or `None` if it does not exist.
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>>;
    /// The file's length, or `None` if it does not exist.
    fn len(&self, path: &Path) -> io::Result<Option<u64>>;
    /// Creates an empty file if none exists.
    fn create(&self, path: &Path) -> io::Result<()>;
    /// `pread` of exactly `buf.len()` bytes. A failure to open the file is [`OpenFailed`], and so
    /// is a missing file for an implementation that opens nothing.
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    /// Opens the file `path` names now, for reading, apart from any handle the implementation
    /// keeps: Core opens a file for each read (`flatfile.cpp:39-52`), so a file unlinked or
    /// replaced since an earlier open is not what it reads (CH3 D46). A failure is [`OpenFailed`],
    /// and so is a missing file. An implementation that keeps no handles can use
    /// [`open_by_path`]; one that wraps another forwards to it, so the wrapped one's rule holds.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>>;
    /// `pwrite` of all of `data`.
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()>;
    /// Core's `AllocateFileRange(file, offset, len)`: make the file at least `offset + len` bytes
    /// long, zero-filled, and reserve the space where the OS supports it. Advisory: failures are
    /// not reported (Core ignores them too); a later write reports a real shortage.
    fn allocate(&self, path: &Path, offset: u64, len: u64);
    /// `ftruncate`.
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()>;
    /// `fdatasync` (Core's `FileCommit`; `EINVAL` from filesystems without sync support is
    /// success, as in Core).
    fn sync_data(&self, path: &Path) -> io::Result<()>;
    /// `fsync` of a directory: makes file creation, removal, renames and truncations durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Copies `from` to a new file `to`.
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Atomically replaces `to` with `from`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Removes a file; `Ok(false)` if it did not exist.
    fn remove(&self, path: &Path) -> io::Result<bool>;
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;
    /// Bytes available to an unprivileged writer on the filesystem holding `dir`.
    fn available_space(&self, dir: &Path) -> io::Result<u64>;
}

/// A file [`FlatFs::open_read`] opened: every read goes to that file, whatever its path names
/// later.
#[expect(clippy::len_without_is_empty, reason = "a file's length is a read, as `FlatFs::len`'s")]
pub trait FlatRead {
    /// The file's length now (`fstat`).
    fn len(&self) -> io::Result<u64>;
    /// `pread` of exactly `buf.len()` bytes; a read past the end is `UnexpectedEof`.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

/// [`FlatFs::open_read`] for an implementation that keeps no handles: the file must exist now,
/// and each read goes by path through [`FlatFs::len`] and [`FlatFs::read_exact_at`].
///
/// # Errors
///
/// [`OpenFailed`] when the file does not exist or its length does not read.
pub fn open_by_path<'f, F: FlatFs>(fs: &'f F, path: &Path) -> io::Result<Box<dyn FlatRead + 'f>> {
    match fs.len(path).map_err(OpenFailed::wrap)? {
        Some(_) => Ok(Box::new(ByPath {
            fs,
            path: path.to_owned(),
        })),
        None => Err(OpenFailed::wrap(io::Error::from(io::ErrorKind::NotFound))),
    }
}

/// A file [`open_by_path`] opened.
struct ByPath<'f, F> {
    fs: &'f F,
    path: PathBuf,
}

impl<F: FlatFs> FlatRead for ByPath<'_, F> {
    fn len(&self) -> io::Result<u64> {
        self.fs.len(&self.path)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.fs.read_exact_at(&self.path, offset, buf)
    }
}

/// The failure of [`FlatFs::read_exact_at`] to open its file, whatever the error, as against a
/// failed read of an open file. Core opens the file for each read and logs a failed open
/// (`flatfile.cpp:44-46`) apart from a failed read, so the stage decides, not the error: `EIO` can
/// come from either, and a list of the errors only an open returns is never complete (`EMFILE` was
/// missing from one).
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct OpenFailed(io::Error);

impl OpenFailed {
    /// `e` marked as an open's failure, with its kind and message.
    pub fn wrap(e: io::Error) -> io::Error {
        io::Error::new(e.kind(), Self(e))
    }

    /// Whether `e` is an open's failure.
    pub fn is(e: &io::Error) -> bool {
        e.get_ref().is_some_and(|inner| inner.is::<Self>())
    }

    /// `e`'s OS error code, an open's failure's included.
    pub fn os_error(e: &io::Error) -> Option<i32> {
        e.raw_os_error().or_else(|| e.get_ref()?.downcast_ref::<Self>()?.0.raw_os_error())
    }
}
