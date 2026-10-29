//! The filesystem seam of the header files (SB14 §3): the OS's ([`OsIo`]), and under the crash
//! harness `dash-kv`'s `FaultyFlatFiles` ([`FaultyIo`]), which holds unsynced writes and unsynced
//! directory entries and models a process kill and a power cut.

use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;

/// An open file. Handles are shared (`Arc`): a raise's snapshot keeps the files it syncs open while
/// the fd cache closes its own (D16).
pub trait HdrFile: Send + Sync + 'static {
    /// `pread`: the bytes read, short at the end of the file.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
    /// `pwrite` of all of `data`.
    fn write_all_at(&self, data: &[u8], offset: u64) -> io::Result<()>;
    /// `File::sync_data`.
    fn sync_data(&self) -> io::Result<()>;
    /// `ftruncate`.
    fn set_len(&self, len: u64) -> io::Result<()>;

    /// Reads until `buf` is full or the file ends: the bytes read.
    fn read_full(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            match self.read_at(&mut buf[got..], offset + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(got)
    }
}

/// Filesystem calls on paths.
pub trait HdrIo: Send + Sync + 'static {
    type File: HdrFile;
    /// Opens `path` for reading and writing. With `create`, a missing file is created; without,
    /// `ENOENT` is `None` (only `ENOENT` means absent: D4 condition 2).
    fn open(&self, path: &Path, create: bool) -> io::Result<Option<Self::File>>;
    /// Unlinks a file; a missing one is no error.
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Makes `dir`'s entries durable (`sync_all` on a directory fd kept open after first use).
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Creates `dir`, whose parent exists; `false` if it existed.
    fn create_dir(&self, dir: &Path) -> io::Result<bool>;
    /// The names in `dir`, or `None` if it does not exist.
    fn list(&self, dir: &Path) -> io::Result<Option<Vec<String>>>;
}

/// The OS.
#[derive(Debug, Default)]
pub struct OsIo {
    /// Directory fds, kept open after their first sync.
    #[cfg_attr(not(unix), allow(dead_code))]
    dirs: Mutex<Vec<(PathBuf, std::fs::File)>>,
}

impl HdrFile for std::fs::File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }

    #[cfg(unix)]
    fn write_all_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::write_all_at(self, data, offset)
    }

    /// Windows' positional read moves the file cursor, which nothing here uses.
    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }

    #[cfg(windows)]
    fn write_all_at(&self, mut data: &[u8], mut offset: u64) -> io::Result<()> {
        while !data.is_empty() {
            match std::os::windows::fs::FileExt::seek_write(self, data, offset) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    data = &data[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn sync_data(&self) -> io::Result<()> {
        std::fs::File::sync_data(self)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        std::fs::File::set_len(self, len)
    }
}

impl HdrIo for OsIo {
    type File = std::fs::File;

    fn open(&self, path: &Path, create: bool) -> io::Result<Option<std::fs::File>> {
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true);
        if create {
            o.create(true).truncate(false);
        }
        match o.open(path) {
            Ok(f) => Ok(Some(f)),
            Err(e) if !create && e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// A no-op off Unix, as Core's `DirectoryCommit` is on Windows (`util/fs_helpers.cpp`):
    /// a directory there is not opened as a file to sync, and NTFS journals its entries.
    #[cfg(not(unix))]
    fn sync_dir(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }

    #[cfg(unix)]
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let mut dirs = self.dirs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, f)) = dirs.iter().find(|(p, _)| p == dir) {
            return f.sync_all();
        }
        let f = std::fs::File::open(dir)?;
        f.sync_all()?;
        dirs.push((dir.to_path_buf(), f));
        Ok(())
    }

    fn create_dir(&self, dir: &Path) -> io::Result<bool> {
        match std::fs::create_dir(dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn list(&self, dir: &Path) -> io::Result<Option<Vec<String>>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for e in entries {
            if let Some(name) = e?.file_name().to_str() {
                out.push(name.to_string());
            }
        }
        Ok(Some(out))
    }
}

/// `dash-kv`'s power-loss filesystem, as the crash harness's header files.
#[cfg(test)]
#[derive(Clone)]
pub struct FaultyIo(pub Arc<dash_kv::faulty::FaultyFlatFiles>);

#[cfg(test)]
pub struct FaultyFile {
    fs: Arc<dash_kv::faulty::FaultyFlatFiles>,
    path: PathBuf,
}

#[cfg(test)]
impl HdrFile for FaultyFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        use dash_kv::FlatFs as _;
        let len = self.fs.len(&self.path)?.ok_or(io::ErrorKind::NotFound)?;
        let n = (len.saturating_sub(offset) as usize).min(buf.len());
        if n == 0 {
            return Ok(0);
        }
        self.fs.read_exact_at(&self.path, offset, &mut buf[..n])?;
        Ok(n)
    }

    fn write_all_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
        use dash_kv::FlatFs as _;
        self.fs.write_all_at(&self.path, offset, data)
    }

    fn sync_data(&self) -> io::Result<()> {
        use dash_kv::FlatFs as _;
        self.fs.sync_data(&self.path)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        use dash_kv::FlatFs as _;
        self.fs.truncate(&self.path, len)
    }
}

#[cfg(test)]
impl HdrIo for FaultyIo {
    type File = FaultyFile;

    fn open(&self, path: &Path, create: bool) -> io::Result<Option<FaultyFile>> {
        use dash_kv::FlatFs as _;
        if create {
            self.0.create(path)?;
        } else if self.0.len(path)?.is_none() {
            return Ok(None);
        }
        Ok(Some(FaultyFile {
            fs: Arc::clone(&self.0),
            path: path.to_path_buf(),
        }))
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        use dash_kv::FlatFs as _;
        self.0.remove(path).map(drop)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        use dash_kv::FlatFs as _;
        self.0.sync_dir(dir)
    }

    fn create_dir(&self, dir: &Path) -> io::Result<bool> {
        use dash_kv::FlatFs as _;
        if self.0.dir_exists(dir) {
            return Ok(false);
        }
        self.0.create_dir_all(dir)?;
        Ok(true)
    }

    fn list(&self, dir: &Path) -> io::Result<Option<Vec<String>>> {
        Ok(self
            .0
            .list_dir(dir)?
            .map(|names| names.into_iter().filter_map(|n| n.into_string().ok()).collect()))
    }
}
