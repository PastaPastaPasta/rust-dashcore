//! Block and undo files (A1 §2.11): Core-identical `blk?????.dat` / `rev?????.dat`.
//!
//! - `FlatFileSeq` (internal): Core's numbered file sequence with chunked preallocation, `fdatasync` and
//!   directory `fsync`, durability bookkeeping and the link-count rule.
//! - [`BlockFiles`]: the file half of Core's `BlockManager`: block and undo positions, rollover,
//!   finalization, flushing, the per-file statistics, pruning selection and unlinking.
//! - [`BlockFileReader`]: `pread`-based reads of blocks and checksummed undo records.
//! - [`FlatFs`]: the filesystem calls all of the above make, so the power-loss model (ST3a) can
//!   replace [`OsFs`].
//!
//! Record layout (identical to Core; no XOR obfuscation at either pinned release):
//! - blk: `magic ‖ u32 LE size ‖ block`;
//! - rev: `magic ‖ u32 LE size ‖ CBlockUndo ‖ SHA256d(prev_block_hash ‖ CBlockUndo)`.
//!
//! Only Core's serialization can be stored (DEC-30 N3): the writer takes [`CoreBytes`] and
//! [`CoreUndoBytes`], never `&[u8]`.
//!
//! Positions point at the payload, after the 8-byte header, as Core's `nDataPos`/`nUndoPos` do.

mod blocks;
mod bytes;
mod error;
mod fs;
mod info;
mod seq;

pub use blocks::{
    BLOCK_SERIALIZATION_HEADER_SIZE, BLOCKFILE_CHUNK_SIZE, BlockFileReader, BlockFiles,
    BlockFilesConfig, BlockFilesState, DirtyFiles, MAX_BLOCK_RECORD_SIZE, MAX_BLOCKFILE_SIZE,
    MAX_UNDO_RECORD_SIZE, MIN_BLOCKS_TO_KEEP, PrunePlan, UNDO_CHECKSUM_SIZE,
    UNDO_STREAM_FIRST_READ, UNDOFILE_CHUNK_SIZE, UndoReadError, UndoReadFailure, last_prunable,
    last_prunable_manual, read_undo_stream, undo_checksum,
};
pub use bytes::{CoreBytes, CoreUndoBytes};
pub use dash_kv::fs::{FlatFs, FlatRead, OpenFailed};
pub use error::{FlatFileError, FlatResult};
pub use fs::OsFs;
pub use info::BlockFileInfo;
pub use seq::{FlatFilePos, MIN_DISK_SPACE};

#[cfg(test)]
mod tests;
