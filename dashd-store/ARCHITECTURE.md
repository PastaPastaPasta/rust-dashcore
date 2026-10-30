# dashd-store

Storage layer of the Rust Dash node (node-program A1 §2.8, §2.11, §3.5).

## Modules and the Core files they mirror

| Module | Core (v24.0.0-rc.3) |
|---|---|
| `rocks` (ST1), over `dash-kv`'s `engine`, `memory`, `table`, `schema`, `codec` (moved there by SB10 and re-exported here) | `dbwrapper.{h,cpp}` |
| `flatfile::seq` (`FlatFileSeq`, `FlatFilePos`) | `flatfile.{h,cpp}`; `util/fs_helpers.cpp` (`AllocateFileRange`, `FileCommit`, `DirectoryCommit`, `TruncateFile`, `CheckDiskSpace`) |
| `flatfile::blocks` (`BlockFiles`, `BlockFileReader`) | `node/blockstorage.{h,cpp}`: `FindBlockPos`, `FindUndoPos`, `SaveBlockToDisk`, `WriteBlockToDisk`, `WriteUndoDataForBlock`, `UndoWriteToDisk`, `FlushBlockFile`, `FlushUndoFile`, `ReadBlockFromDisk`, `UndoReadFromDisk`, `PruneOneBlockFile`, `FindFilesToPrune(Manual)`, `UnlinkPrunedFiles`, `ScanAndUnlinkAlreadyPrunedFiles`, `CalculateCurrentUsage` |
| `flatfile::info` (`BlockFileInfo`) | `chain.h` `CBlockFileInfo` |

The external-file scanner (`-reindex`, `-loadblock`) is in `dash-coreimport::blockfiles::external`.

## Block files

- Files are byte-identical to Core's: `magic ‖ u32 size ‖ block` and `magic ‖ u32 size ‖ CBlockUndo ‖
  SHA256d(prev_hash ‖ CBlockUndo)`, the same rollover points (128 MiB, `-fastprune` 64 KiB),
  preallocation chunks (16 MiB / 1 MiB, `-fastprune` 16 KiB blk, `-tinyblk` 64 KiB) and finalize
  truncations. v24 does not XOR-obfuscate block files.
- The writer takes `CoreBytes` / `CoreUndoBytes`, never `&[u8]` (DEC-30 N3). Both are placeholders
  until CS0 (`dash_consensus::CoreBytes`) and CS5a (the `CBlockUndo` codec) land; their only
  constructor is behind the test-only `raw-bytes` feature. `flatfile/bytes.rs` lists the swap
  steps; `dashd-store` may then depend on the pure `dash-consensus`.
- The rev-file finalize rule at a blk rollover is v24's (bitcoin#27746 via Dash #7553): the
  file's last height equals the highest undo height written to the current file since startup.
  Also v24's: `-fastprune` blocks of 64 KiB or more grow the file, and already-pruned files are
  unlinked at startup, with unlink errors logged and skipped. The node targets v24 alone (DEC-82).
- **The barrier.** `sync()`, `sync_and_state()` and `sync_and_take_dirty_infos()` are the only ways to get
  durable state: each first does what Core's `FlushStateToDisk` does to the block files
  (`FlushBlockFile()`: sync the current pair, creating an empty rev file when the current blk
  file has no undo yet), then syncs everything else (deviation 1). `adopt_existing_files()`
  marks every file a Core datadir import references for that barrier.
- `FlatFs` (defined in `dash-kv::fs`, re-exported by `flatfile`) is the only path to the filesystem, so
  the power-loss model (ST3a, `dash-kv::faulty`) can replace `OsFs`.

## Deviations from Core (each visible only after an I/O error, a crash or corruption)

1. **`BlockFiles::sync`** syncs every blk and rev file written since its last sync, then the
   directory once if a file was created, truncated or removed (A1 §3.5.4 step 2). Core's
   `FlushBlockFile` syncs only the current pair, so undo written into an older, lagging rev file
   is not synced before the index records `BLOCK_HAVE_UNDO`. `sync_and_take_dirty_infos` returns
   the file statistics for the index write only after that barrier.
   - Nothing written by an earlier process is trusted: the first directory sync of a process
     also syncs `blocks/`'s parent, and a block registered at a known position (`-reindex`)
     marks its file for the next data sync.
2. **Directory `fsync` errors are reported** (Core's `DirectoryCommit` ignores them).
3. **Fail-stop (A1 §3.5.6, I5).** After a failed write, allocation, truncation or sync, every
   later mutating call returns `FlatFileError::Failed`; a failed `fdatasync` is never retried.
   Core logs `AbortNode` from `FlushBlockFile` and carries on. A failure to open a file during
   `Allocate` is an error here; Core silently skips the allocation.
4. **Link-count rule (A1 §2.11).** A file with more than one link is replaced by a private copy
   (copy or reflink, `fdatasync`, `rename`, directory `fsync`) before it is modified, so a hardlink
   from an import or an oracle datadir never sees our writes. A sync alone leaves it shared.
5. **`UnlinkPrunedFiles` syncs the directory** after unlinking.
6. **Reads use the record's size field** to know how many bytes to read; Core deserializes the
   block or `CBlockUndo` and ignores the size field.
   - Blocks: `read_block` returns the whole record, and the caller decodes the block from its
     start. Core's `-reindex` scanner accepts a size field larger than the block it holds, so a
     record can carry bytes after the block, which Core's read ignores.
   - Undo: the checksum covers exactly the size field's bytes, so a corrupt size field fails
     here where Core might still read the record.
   - The size field is bounded before anything is allocated: blocks by `MaxBlockSize()`
     (2,000,000, the limit Core's `CheckBlock` and `-reindex` scanner apply), undo by
     `MAX_UNDO_RECORD_SIZE` (about 489 MB), a conservative ceiling on any `CBlockUndo` of a valid
     block, not the exact maximum: the proof in `blocks.rs` counts every input at
     41 block bytes and every transaction, including the empty-input special transactions, at 10
     bytes of framing. A larger field is `Corrupt`; the buffer is reserved fallibly, so a forged
     size within the bound that the process cannot allocate is an `Io` error, not an abort. A
     forged size within the bound still costs an allocation of that size, where Core streams the
     decode and checksum (BL-33, for the CH2/CH3 integration).
   - So reads are not Core-identical on forged framing: a record whose size field was changed
     (e.g. to `0xffffffff`) over an intact block is `Corrupt` here, while Core's indexed reader,
     which deserializes from `nDataPos` and never reads the size field, still returns the block
     (GPT ST2-r3 checked this). Records Core writes, and records its scanner
     indexes, have size fields within the bound and read the same.
7. **Handles.** `OsFs` keeps recently used files open and uses `pread`/`pwrite`; Core opens a
   `FILE*` per operation. An undo record is written with one `pwrite`. The cache holds 64 handles,
   but never closes one written through since its last `fdatasync`: the barrier syncs through
   the descriptor that wrote, so a writeback error cannot be lost with a closed descriptor.
8. **The `blocks/` directory is created with its first file**; Core creates it at startup.
9. **`flush_undo_file` of a file without statistics** is refused; Core indexes past
    `m_blockfile_info`.
10. **Unlink errors** are logged and skipped (v24); a file the index no longer references is only
    wasted space.
11. **Private copies check disk space** (`CheckDiskSpace` for the file's size) before copying.
12. **Only `blocks/` and its parent are synced.** Directories above them that a fresh datadir
    created are the datadir lock's job (ND1).
