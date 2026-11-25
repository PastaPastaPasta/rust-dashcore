// SPDX-License-Identifier: MIT

//! The block filter index's `fltr?????.dat` files: Core's `FlatFileSeq` for filters, each entry
//! `block_hash ‖ CompactSize filter`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_consensus::primitives::{read_compact_size, write_compact_size};
use dashd_chainstate::subscriber::{FileAppend, FilePos};
use dashd_store::flatfile::{FlatFs, MIN_DISK_SPACE};

use crate::BlockHash;

// Core: index/blockfilterindex.cpp:38
pub const MAX_FLTR_FILE_SIZE: u64 = 0x100_0000;
// Core: index/blockfilterindex.cpp:40
pub const FLTR_FILE_CHUNK_SIZE: u64 = 0x10_0000;

/// One entry's bytes: the block hash, then the encoded filter as a byte vector.
// Core: index/blockfilterindex.cpp:224-226, :262
pub fn encode_entry(block_hash: &BlockHash, encoded: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32 + 9 + encoded.len());
    bytes.extend_from_slice(block_hash.as_bytes());
    write_compact_size(encoded.len() as u64, &mut bytes);
    bytes.extend_from_slice(encoded);
    bytes
}

/// The last file and where its data ends.
#[derive(Clone, Copy, Debug)]
struct Cursor {
    file: u32,
    end: u64,
    exists: bool,
}

/// The numbered filter files in one directory.
pub struct FilterFiles {
    fs: Arc<dyn FlatFs>,
    dir: PathBuf,
    max_file_size: u64,
    chunk_size: u64,
    cursor: Option<Cursor>,
    unsynced: Option<u32>,
    dir_unsynced: bool,
    parents_unsynced: bool,
}

fn other(message: String) -> io::Error {
    io::Error::other(message)
}

impl FilterFiles {
    /// The files in `dir`, with Core's file and chunk sizes.
    pub fn new(fs: Arc<dyn FlatFs>, dir: PathBuf) -> Self {
        Self {
            fs,
            dir,
            max_file_size: MAX_FLTR_FILE_SIZE,
            chunk_size: FLTR_FILE_CHUNK_SIZE,
            cursor: None,
            unsynced: None,
            dir_unsynced: true,
            parents_unsynced: true,
        }
    }

    /// Other file and chunk sizes, for tests.
    pub fn with_sizes(mut self, max_file_size: u64, chunk_size: u64) -> Self {
        assert!(max_file_size > 0 && chunk_size > 0, "positive file and chunk sizes");
        self.max_file_size = max_file_size;
        self.chunk_size = chunk_size;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // Core: flatfile.cpp:24-27
    pub fn path(&self, n: u32) -> PathBuf {
        self.dir.join(format!("fltr{n:05}.dat"))
    }

    /// File `n`'s length, or `None` if it does not exist.
    pub fn len(&self, n: u32) -> io::Result<Option<u64>> {
        self.fs.len(&self.path(n))
    }

    /// The last file of the unbroken run from file 0, and its length.
    pub fn end(&self) -> io::Result<FilePos> {
        Ok(self.scan()?.map_or(FilePos::default(), |c| FilePos {
            file: c.file,
            offset: c.end,
        }))
    }

    fn scan(&self) -> io::Result<Option<Cursor>> {
        let mut last = None;
        let mut n = 0;
        while let Some(len) = self.len(n)? {
            last = Some(Cursor {
                file: n,
                end: len,
                exists: true,
            });
            n += 1;
        }
        Ok(last)
    }

    /// Where an entry of `len` bytes goes after `at`: at `at`, or at the next file's start when
    /// it would pass the file size. An entry at a file's start stays there, so no file is
    /// skipped; a basic filter's entry is far below the file size, so this is Core's rule.
    // Core: index/blockfilterindex.cpp:228-246
    pub fn start(&self, at: FilePos, len: u64) -> FilePos {
        if at.offset > 0 && at.offset.saturating_add(len) > self.max_file_size {
            FilePos {
                file: at.file + 1,
                offset: 0,
            }
        } else {
            at
        }
    }

    fn cursor(&mut self) -> io::Result<Cursor> {
        if let Some(c) = self.cursor {
            return Ok(c);
        }
        let c = self.scan()?.unwrap_or(Cursor {
            file: 0,
            end: 0,
            exists: false,
        });
        self.unsynced = c.exists.then_some(c.file);
        self.cursor = Some(c);
        Ok(c)
    }

    /// Writes `a` at or past the last file's data end. Moving to the next file truncates and
    /// syncs the last one first, as Core does, and creates the next one with a directory sync. A
    /// failed append rescans the files at the next call, so it never writes over torn bytes.
    // Core: index/blockfilterindex.cpp:220-264; flatfile.cpp:55-84
    pub fn append(&mut self, a: &FileAppend) -> io::Result<()> {
        let c = self.cursor()?;
        let rotate = c.exists && a.file == c.file + 1;
        if !(rotate || a.file == c.file) || (a.file == c.file && c.exists && a.offset < c.end) {
            return Err(other(format!(
                "a filter append at {}:{} after the files' end {}:{}",
                a.file, a.offset, c.file, c.end
            )));
        }
        let r = self.write(a, c, rotate);
        if r.is_err() {
            self.cursor = None;
        }
        r
    }

    fn write(&mut self, a: &FileAppend, c: Cursor, rotate: bool) -> io::Result<()> {
        if rotate {
            let last = self.path(c.file);
            if self.fs.len(&last)?.is_some_and(|len| len > c.end) {
                self.fs.truncate(&last, c.end)?;
            }
            self.fs.sync_data(&last)?;
            self.unsynced = None;
        }
        let path = self.path(a.file);
        if rotate || !c.exists {
            self.fs.create(&path)?;
            self.cursor = Some(Cursor {
                file: a.file,
                end: 0,
                exists: true,
            });
            self.dir_unsynced = true;
            self.sync_dir();
        }
        self.allocate(&path, a.offset, a.bytes.len() as u64)?;
        self.unsynced = Some(a.file);
        self.fs.write_all_at(&path, a.offset, &a.bytes)?;
        self.cursor = Some(Cursor {
            file: a.file,
            end: a.offset + a.bytes.len() as u64,
            exists: true,
        });
        Ok(())
    }

    // Core: flatfile.cpp:55-84
    fn allocate(&self, path: &Path, offset: u64, len: u64) -> io::Result<()> {
        let old_chunks = offset.div_ceil(self.chunk_size);
        let new_chunks = (offset + len).div_ceil(self.chunk_size);
        if new_chunks <= old_chunks {
            return Ok(());
        }
        let inc = new_chunks * self.chunk_size - offset;
        if self.fs.available_space(&self.dir)? < MIN_DISK_SPACE + inc {
            return Err(other("out of disk space".into()));
        }
        self.fs.allocate(path, offset, inc);
        Ok(())
    }

    /// One attempt; a failure is ignored, as Core's `DirectoryCommit` ignores one, and the next
    /// [`sync`](Self::sync) tries again. Until one succeeds, the three directories above
    /// (`indexes/blockfilter/basic` under the datadir) are synced too, so a power loss cannot
    /// lose the directory itself; Core syncs none of them.
    // Core: util/fs_helpers.cpp:153-166
    fn sync_dir(&mut self) {
        if self.fs.sync_dir(&self.dir).is_ok() {
            self.dir_unsynced = false;
        }
        if self.parents_unsynced {
            let mut synced = true;
            for parent in self.dir.ancestors().skip(1).take(3) {
                if !parent.as_os_str().is_empty() {
                    synced &= self.fs.sync_dir(parent).is_ok();
                }
            }
            self.parents_unsynced = !synced;
        }
    }

    /// Syncs the file written since the last sync, then the directory after a file was created
    /// (or once per process, for files an earlier one created).
    // Core: index/blockfilterindex.cpp:174-182
    pub fn sync(&mut self) -> io::Result<()> {
        self.cursor()?;
        if let Some(n) = self.unsynced {
            self.fs.sync_data(&self.path(n))?;
            self.unsynced = None;
        }
        if self.dir_unsynced || self.parents_unsynced {
            self.sync_dir();
        }
        Ok(())
    }

    /// The entry at `pos`: its block hash and encoded filter.
    // Core: index/blockfilterindex.cpp:189-218
    pub fn read(&self, pos: FilePos) -> io::Result<(BlockHash, Vec<u8>)> {
        let path = self.path(pos.file);
        let mut head = [0u8; 41];
        let len =
            self.fs.len(&path)?.ok_or_else(|| other(format!("no filter file {}", pos.file)))?;
        let avail = usize::try_from(len.saturating_sub(pos.offset)).unwrap_or(usize::MAX);
        let head = &mut head[..avail.min(41)];
        self.fs.read_exact_at(&path, pos.offset, head)?;
        let (hash, mut rest) = head
            .split_first_chunk::<32>()
            .ok_or_else(|| other(format!("a truncated filter entry at {pos:?}")))?;
        let before = rest.len();
        let n = read_compact_size(&mut rest, true)
            .map_err(|e| other(format!("a filter entry's size at {pos:?}: {e:?}")))?;
        let at = pos.offset + 32 + (before - rest.len()) as u64;
        let mut encoded = vec![0; usize::try_from(n).map_err(|e| other(e.to_string()))?];
        self.fs.read_exact_at(&path, at, &mut encoded)?;
        Ok((BlockHash::from(*hash), encoded))
    }
}
