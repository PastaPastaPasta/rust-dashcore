//! Core's `FlatFileSeq` (`flatfile.{h,cpp}`): a numbered sequence of `<prefix>NNNNN.dat` files with
//! chunked preallocation, plus the durability bookkeeping and the link-count rule of A1 §2.11.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::flatfile::FlatFs;
use crate::flatfile::error::{FlatFileError, FlatResult};
use dash_kv::codec::{CodecError, read_varint_u32, write_varint};

/// A failed sync of `blocks/`, logged and passed over (DEC-212, BL-124).
pub(super) const DIR_SYNC_FAILED: &str = "Failed to sync the blocks directory";

/// A position in a flat file sequence. Core's null position (`nFile == -1`) is `Option::None`.
// Core: flatfile.h FlatFilePos
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FlatFilePos {
    pub file: u32,
    pub pos: u32,
}

impl FlatFilePos {
    pub const fn new(file: u32, pos: u32) -> Self {
        Self {
            file,
            pos,
        }
    }

    /// Core's serialization: `VARINT_MODE(nFile, NONNEGATIVE_SIGNED) ‖ VARINT(nPos)`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, u64::from(self.file));
        write_varint(out, u64::from(self.pos));
    }

    pub fn decode(input: &mut &[u8]) -> Result<Self, CodecError> {
        let file = read_varint_u32(input)?;
        // Core reads nFile as a non-negative int.
        if file > i32::MAX as u32 {
            return Err(CodecError::VarIntOverflow);
        }
        Ok(Self::new(file, read_varint_u32(input)?))
    }
}

impl fmt::Display for FlatFilePos {
    // Core: flatfile.cpp FlatFilePos::ToString
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FlatFilePos(nFile={}, nPos={})", self.file, self.pos)
    }
}

/// Core's `CheckDiskSpace` floor.
// Core: util/fs_helpers.cpp CheckDiskSpace
pub const MIN_DISK_SPACE: u64 = 50 * 1024 * 1024;

/// A sequence of flat files in one directory.
///
/// Durability (A1 §3.5.3 I1): every file written, allocated or truncated since its last data sync
/// is remembered, and [`sync_files`](Self::sync_files) covers them. [`flush`](Self::flush) is
/// Core's `Flush`, which syncs one file and the directory, every time (BL-124).
///
/// A new sequence starts with the directory's parent unsynced: the `blocks/` directory itself may
/// not be durable yet.
///
/// Link-count rule (A1 §2.11): before any call that modifies a file, its link count is checked. A
/// file with more than one link (a hardlink from an import or into an oracle datadir) is first
/// replaced by a private copy (copy, sync, rename, directory sync), so the other links never see
/// the change.
pub(crate) struct FlatFileSeq<F: FlatFs> {
    fs: Arc<F>,
    dir: PathBuf,
    prefix: &'static str,
    chunk_size: u32,
    unsynced: BTreeSet<u32>,
    parent_unsynced: bool,
}

impl<F: FlatFs> FlatFileSeq<F> {
    // Core: flatfile.cpp FlatFileSeq::FlatFileSeq
    pub fn new(fs: Arc<F>, dir: PathBuf, prefix: &'static str, chunk_size: u32) -> Self {
        assert!(chunk_size > 0, "chunk_size must be positive");
        Self {
            fs,
            dir,
            prefix,
            chunk_size,
            unsynced: BTreeSet::new(),
            parent_unsynced: true,
        }
    }

    pub fn fs(&self) -> &Arc<F> {
        &self.fs
    }

    /// The length of a file, or `None` if it does not exist.
    pub fn len(&self, file: u32) -> std::io::Result<Option<u64>> {
        self.fs.len(&self.file_name(file))
    }

    /// `<dir>/<prefix>NNNNN.dat`.
    // Core: flatfile.cpp FlatFileSeq::FileName
    pub fn file_name(&self, file: u32) -> PathBuf {
        self.dir.join(format!("{}{:05}.dat", self.prefix, file))
    }

    /// Makes `file` safe to modify: creates it if absent and applies the link-count rule.
    fn prepare_write(&mut self, file: u32, context: &'static str) -> FlatResult<PathBuf> {
        let path = self.file_name(file);
        let io = |e| FlatFileError::io(context, path.clone(), e);
        match self.fs.link_count(&path).map_err(io)? {
            // A link count of 0: the file was unlinked under us; it is missing.
            None | Some(0) => {
                self.fs.create_dir_all(&self.dir).map_err(io)?;
                self.fs.create(&path).map_err(io)?;
            }
            Some(1) => {}
            Some(_) => self.make_private(&path, context)?,
        }
        Ok(path)
    }

    /// Replaces a multiply-linked file by a private copy, after Core's `CheckDiskSpace` for it.
    fn make_private(&mut self, path: &Path, context: &'static str) -> FlatResult<()> {
        let io = |e| FlatFileError::io(context, path.to_path_buf(), e);
        let len = self.fs.len(path).map_err(io)?.unwrap_or(0);
        if self.fs.available_space(&self.dir).map_err(io)? < MIN_DISK_SPACE + len {
            return Err(FlatFileError::NoSpace);
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".unlink");
        let tmp = PathBuf::from(tmp);
        self.fs.remove(&tmp).map_err(io)?;
        self.fs.copy(path, &tmp).map_err(io)?;
        self.fs.sync_data(&tmp).map_err(io)?;
        self.fs.rename(&tmp, path).map_err(io)?;
        self.fs.sync_dir(&self.dir).map_err(io)?;
        Ok(())
    }

    /// Writes `parts` back to back at `offset`.
    pub fn write(
        &mut self,
        file: u32,
        offset: u64,
        parts: &[&[u8]],
        context: &'static str,
    ) -> FlatResult<()> {
        let path = self.prepare_write(file, context)?;
        self.unsynced.insert(file);
        let mut at = offset;
        for part in parts {
            self.fs
                .write_all_at(&path, at, part)
                .map_err(|e| FlatFileError::io(context, path.clone(), e))?;
            at += part.len() as u64;
        }
        Ok(())
    }

    /// Reads exactly `buf.len()` bytes at `offset`.
    pub fn read(&self, file: u32, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.fs.read_exact_at(&self.file_name(file), offset, buf)
    }

    /// Core's `Allocate`: grows the file to the next chunk boundary when `pos.pos + add_size`
    /// crosses one. Returns the number of bytes added (0 if none). Fails with
    /// [`FlatFileError::NoSpace`] when `CheckDiskSpace` fails.
    // Core: flatfile.cpp FlatFileSeq::Allocate
    pub fn allocate(&mut self, pos: FlatFilePos, add_size: u32) -> FlatResult<u64> {
        let chunk = u64::from(self.chunk_size);
        let old_chunks = u64::from(pos.pos).div_ceil(chunk);
        let new_chunks = (u64::from(pos.pos) + u64::from(add_size)).div_ceil(chunk);
        if new_chunks <= old_chunks {
            return Ok(0);
        }
        let inc = new_chunks * chunk - u64::from(pos.pos);
        let context = "Disk space is too low!";
        let dir_io = |e| FlatFileError::io(context, self.dir.clone(), e);
        // Core creates the blocks directory at startup; ours appears with its first file.
        self.fs.create_dir_all(&self.dir).map_err(dir_io)?;
        let space = self.fs.available_space(&self.dir).map_err(dir_io)?;
        if space < MIN_DISK_SPACE + inc {
            return Err(FlatFileError::NoSpace);
        }
        // Core's `Open` (flatfile.cpp:68): a failure is the open's, which its caller's write
        // then meets.
        let path =
            self.prepare_write(pos.file, context).map_err(FlatFileError::into_open_failure)?;
        tracing::debug!(target: "validation", "Pre-allocating up to position 0x{:x} in {}{:05}.dat", new_chunks * chunk, self.prefix, pos.file);
        self.fs.allocate(&path, u64::from(pos.pos), inc);
        self.unsynced.insert(pos.file);
        Ok(inc)
    }

    /// Core's `Flush`: truncate to `pos.pos` if `finalize`, then sync the file's data and the
    /// directory. Creates the file if it does not exist, as Core's `Open` does. The directory's
    /// sync is Core's `DirectoryCommit`, one attempt whose failure is ignored (DEC-212, BL-124):
    /// it is logged as a warning, which Core does not log, and the next `Flush` syncs it again.
    // Core: flatfile.cpp FlatFileSeq::Flush; util/fs_helpers.cpp DirectoryCommit
    pub fn flush(
        &mut self,
        pos: FlatFilePos,
        finalize: bool,
        context: &'static str,
    ) -> FlatResult<()> {
        let path = self.file_name(pos.file);
        let io = |e| FlatFileError::io(context, path.clone(), e);
        // A sync alone does not modify the file, so a shared file stays shared.
        if finalize || self.fs.len(&path).map_err(io)?.is_none() {
            self.prepare_write(pos.file, context)?;
        }
        if finalize {
            self.fs.truncate(&path, u64::from(pos.pos)).map_err(io)?;
        }
        self.fs.sync_data(&path).map_err(io)?;
        self.unsynced.remove(&pos.file);
        if let Err(e) = self.sync_dir(DIR_SYNC_FAILED) {
            tracing::warn!(target: "blockstorage", "{e}");
        }
        Ok(())
    }

    /// `fdatasync` every file written since its last sync.
    pub fn sync_files(&mut self, context: &'static str) -> FlatResult<()> {
        while let Some(&file) = self.unsynced.first() {
            let path = self.file_name(file);
            self.fs.sync_data(&path).map_err(|e| FlatFileError::io(context, path, e))?;
            self.unsynced.remove(&file);
        }
        Ok(())
    }

    /// `fsync` the directory, and its parent (which holds the directory's own entry) until a call
    /// syncs both. Each is one attempt, made whatever the other's result (DEC-212); the
    /// directory's failure is the one reported. Core's `DirectoryCommit` syncs only the directory
    /// and never the datadir, so the parent's sync is durability Core does not have.
    pub fn sync_dir(&mut self, context: &'static str) -> FlatResult<()> {
        let io = |dir: &Path, e| FlatFileError::io(context, dir.to_path_buf(), e);
        let synced = self.fs.sync_dir(&self.dir).map_err(|e| io(&self.dir, e));
        let parent_synced = match self.dir.parent() {
            Some(p) if self.parent_unsynced && !p.as_os_str().is_empty() => {
                self.fs.sync_dir(p).map_err(|e| io(p, e))
            }
            _ => Ok(()),
        };
        // A directory whose sync failed may not exist yet: the entry it gets later is unsynced.
        if synced.is_ok() && parent_synced.is_ok() {
            self.parent_unsynced = false;
        }
        synced.and(parent_synced)
    }

    /// Records that another sequence in the same directory synced it and its parent.
    pub fn mark_dir_synced(&mut self) {
        self.parent_unsynced = false;
    }

    /// Records that the directory's own entry may not be durable: the next sync syncs the parent.
    pub fn mark_dir_unsynced(&mut self) {
        self.parent_unsynced = true;
    }

    /// Records that a file's bytes may not be durable (bytes another process wrote and that are
    /// now being referenced, as with `-reindex`'s known positions).
    pub fn mark_unsynced(&mut self, file: u32) {
        self.unsynced.insert(file);
    }

    /// Removes a file; returns whether it existed. The directory is left unsynced.
    pub fn remove(&mut self, file: u32) -> FlatResult<bool> {
        let path = self.file_name(file);
        let removed = self
            .fs
            .remove(&path)
            .map_err(|e| FlatFileError::io("Failed to remove file", path, e))?;
        self.unsynced.remove(&file);
        Ok(removed)
    }
}
