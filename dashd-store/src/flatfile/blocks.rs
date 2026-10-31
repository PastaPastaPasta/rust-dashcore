//! The block-file half of Core's `BlockManager` (`node/blockstorage.cpp`): where blocks and undo
//! records go, when files roll over, are preallocated, finalized and flushed, and how they are
//! read back. The block index itself (`nStatus`, `nDataPos`, `nUndoPos`) belongs to the caller;
//! this type returns positions and keeps the per-file statistics (`CBlockFileInfo`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fmt, io};

use bitcoin_hashes::{Hash, HashEngine, sha256d};
use dash_consensus::coins::BlockUndoRef;
use dash_consensus::primitives::ReadErrorKind;

use crate::flatfile::bytes::{CoreBytes, CoreUndoBytes};
use crate::flatfile::error::{FlatFileError, FlatResult};
use crate::flatfile::info::BlockFileInfo;
use crate::flatfile::seq::{DIR_SYNC_FAILED, FlatFilePos, FlatFileSeq};
use crate::flatfile::{FlatFs, FlatRead, OsFs};

/// The pre-allocation chunk of blk files.
// Core: node/blockstorage.h
pub const BLOCKFILE_CHUNK_SIZE: u32 = 0x100_0000; // 16 MiB
/// The pre-allocation chunk of rev files.
pub const UNDOFILE_CHUNK_SIZE: u32 = 0x10_0000; // 1 MiB
/// The size at which a blk file rolls over.
pub const MAX_BLOCKFILE_SIZE: u32 = 0x800_0000; // 128 MiB
/// `magic ‖ u32 size` before every record.
pub const BLOCK_SERIALIZATION_HEADER_SIZE: u32 = 8;
/// The `SHA256d(prev_block_hash ‖ undo)` after every undo record.
pub const UNDO_CHECKSUM_SIZE: u32 = 32;
/// The largest record size field a block read accepts: Core's `MaxBlockSize()` (DIP0001, every
/// network), which `CheckBlock` enforces on every block Core stores and the `-reindex`/`-loadblock`
/// scanner on every record's size field (`nSize > nMaxBlockSize` is skipped). Core's reader
/// deserializes instead of trusting the size field; bounding it keeps a forged size from making
/// us allocate gigabytes.
// Core: consensus/consensus.h MaxBlockSize; validation.cpp LoadExternalBlockFile
pub const MAX_BLOCK_RECORD_SIZE: u32 = 2_000_000;
/// The smallest serialized `CTxIn`: outpoint (36), empty `scriptSig` (1), `nSequence` (4).
const MIN_TXIN_SIZE: u32 = 41;
/// The largest undo entry for one spent coin (`TxInUndoFormatter`): `VARINT(height·2 + coinbase)`
/// (5), the dummy version byte (1), `VARINT(CompressAmount)` (10), the script's `VARINT` size (2)
/// and the script itself, at most `MAX_SCRIPT_SIZE` (10,000): a longer script is unspendable and
/// never enters the UTXO set, so it is never spent.
// Core: undo.h TxInUndoFormatter; compressor.h; script/script.h MAX_SCRIPT_SIZE, IsUnspendable
const MAX_UNDO_COIN_SIZE: u32 = 5 + 1 + 10 + 2 + 10_000;
/// A conservative ceiling on any `CBlockUndo` Core writes for a valid block (at v24), not the
/// exact largest one: `5 + (MaxBlockSize() / 41) · (5 + MAX_UNDO_COIN_SIZE)`, about 489 MB.
///
/// Why it holds (GPT ST2-r3): let `I` be the block's spent inputs and `T` its non-coinbase
/// transactions. Every input takes at least 41 block bytes, and every transaction at least 10
/// bytes of framing, even one with no inputs (quorum commitments, MNHF signals and asset unlocks
/// may have none, and still get an empty `CTxUndo`). With the 80-byte header and a 1-byte
/// transaction count, `41·I + 10·T + 81 ≤ 2,000,000`, while the undo is at most
/// `5 + MAX_UNDO_COIN_SIZE·I + 5·T` (each `CompactSize` count at most 5 bytes). The maximum of
/// that relaxation is 488,658,019 bytes, below this ceiling (488,921,945); the real maximum is
/// lower still (it ignores the coinbase, outputs and smaller encodings).
///
/// The undo read reserves at most this, fallibly, so a forged size within it is an error rather
/// than an abort; it can still cost an allocation of that size (Core streams instead; BL-33).
pub const MAX_UNDO_RECORD_SIZE: u32 =
    5 + (MAX_BLOCK_RECORD_SIZE / MIN_TXIN_SIZE) * (5 + MAX_UNDO_COIN_SIZE);

/// Block files holding a block within this many blocks of the tip are not pruned.
// Core: validation.h
pub const MIN_BLOCKS_TO_KEEP: u32 = 288;

/// The room automatic pruning leaves under its target for the next allocation before the next
/// check (`nBuffer`'s first term).
// Core: node/blockstorage.cpp BlockManager::FindFilesToPrune
const PRUNE_BUFFER: u64 = BLOCKFILE_CHUNK_SIZE as u64 + UNDOFILE_CHUNK_SIZE as u64;

/// Automatic pruning's stop test, `nCurrentUsage + nBuffer < nPruneTarget`, without the IBD margin.
fn target_met(usage: u64, prune_target: u64) -> bool {
    usage + PRUNE_BUFFER < prune_target
}

/// The highest block automatic pruning may remove (Core's `nLastBlockWeCanPrune`):
/// `min(prune_height, tip − MIN_BLOCKS_TO_KEEP)` in `int`, then unsigned, so a tip below
/// `MIN_BLOCKS_TO_KEEP` wraps to every height as Core's does.
// Core: node/blockstorage.cpp BlockManager::FindFilesToPrune
pub fn last_prunable(prune_height: i32, tip_height: u32) -> u32 {
    prune_height.min(tip_height as i32 - MIN_BLOCKS_TO_KEEP as i32) as u32
}

/// The highest block `pruneblockchain` may remove (Core's `nLastBlockWeCanPrune`):
/// `min(manual_height, tip − MIN_BLOCKS_TO_KEEP)` in unsigned arithmetic.
// Core: node/blockstorage.cpp BlockManager::FindFilesToPruneManual
pub fn last_prunable_manual(manual_height: u32, tip_height: u32) -> u32 {
    manual_height.min(tip_height.wrapping_sub(MIN_BLOCKS_TO_KEEP))
}

/// A prune selection planned with no side effects (CH2 §2.7): the files a selector would prune,
/// and the line it would log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrunePlan {
    /// The selection, in file order.
    pub files: Vec<u32>,
    /// Automatic pruning: the usage after `files` meets the target
    /// ([`BlockFiles::prune_target_met`]'s test). Always true for a manual plan, which has no
    /// target.
    pub met_target: bool,
    /// What the selector logs; `None` where Core returns before its log line.
    log: Option<PruneLog>,
}

/// The values in a selector's log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PruneLog {
    Manual {
        last_prunable: u32,
    },
    Automatic {
        prune_target: u64,
        /// `nCurrentUsage` after the selection.
        usage: u64,
        last_prunable: u32,
    },
}

impl PrunePlan {
    /// An automatic plan Core returns from before it selects or logs.
    fn unlogged(usage: u64, prune_target: u64) -> Self {
        Self {
            files: Vec::new(),
            met_target: target_met(usage, prune_target),
            log: None,
        }
    }

    /// Logs the selection as Core's selector does once it has selected: the caller that decides
    /// on the plan logs it, once.
    // Core: node/blockstorage.cpp BlockManager::FindFilesToPruneManual, FindFilesToPrune
    pub fn log(&self) {
        match self.log {
            None => {}
            Some(PruneLog::Manual {
                last_prunable,
            }) => tracing::info!(
                "Prune (Manual): prune_height={} removed {} blk/rev pairs",
                last_prunable,
                self.files.len()
            ),
            Some(PruneLog::Automatic {
                prune_target,
                usage,
                last_prunable,
            }) => {
                tracing::debug!(target: "prune", "target={}MiB actual={}MiB diff={}MiB max_prune_height={} removed {} blk/rev pairs",
                    prune_target / 1024 / 1024, usage / 1024 / 1024,
                    (prune_target as i64 - usage as i64) / 1024 / 1024, last_prunable, self.files.len());
            }
        }
    }
}

/// `-fastprune`: 64 KiB blk files with 16 KiB chunks. `-tinyblk`: 64 KiB chunks for blk and rev.
// Core: node/blockstorage.cpp BlockFileSeq, UndoFileSeq, FindBlockPos
const FASTPRUNE_MAX_FILE: u32 = 0x1_0000;
const FASTPRUNE_BLOCK_CHUNK: u32 = 0x4000;
const TINYBLK_CHUNK: u32 = 0x1_0000;

const FLUSH_BLOCK_FAILED: &str =
    "Flushing block file to disk failed. This is likely the result of an I/O error.";
const FLUSH_UNDO_FAILED: &str =
    "Flushing undo file to disk failed. This is likely the result of an I/O error.";
const WRITE_BLOCK_FAILED: &str = "Failed to write block";
const WRITE_UNDO_FAILED: &str = "Failed to write undo data";

/// Block-file settings.
#[derive(Debug, Clone)]
pub struct BlockFilesConfig {
    /// The `blocks/` directory.
    pub dir: PathBuf,
    /// The network magic written before every record.
    pub magic: [u8; 4],
    /// `-fastprune` (tests): 64 KiB blk files, 16 KiB blk chunks.
    pub fastprune: bool,
    /// `-tinyblk` (tests): 64 KiB preallocation chunks for blk and rev files.
    pub tinyblk: bool,
    /// `-prune`: a new allocation sets [`BlockFiles::take_check_for_pruning`].
    pub prune_mode: bool,
}

impl BlockFilesConfig {
    pub fn new(dir: impl Into<PathBuf>, magic: [u8; 4]) -> Self {
        Self {
            dir: dir.into(),
            magic,
            fastprune: false,
            tinyblk: false,
            prune_mode: false,
        }
    }

    fn block_chunk(&self) -> u32 {
        if self.fastprune {
            FASTPRUNE_BLOCK_CHUNK
        } else if self.tinyblk {
            TINYBLK_CHUNK
        } else {
            BLOCKFILE_CHUNK_SIZE
        }
    }

    fn undo_chunk(&self) -> u32 {
        if self.tinyblk {
            TINYBLK_CHUNK
        } else {
            UNDOFILE_CHUNK_SIZE
        }
    }
}

/// The persisted state of the block files: Core's `m_blockfile_info` and `m_last_blockfile`, as
/// read back from the block index database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockFilesState {
    pub infos: Vec<BlockFileInfo>,
    pub last_file: u32,
}

/// What the next block-index write records about the files (Core's `WriteBatchSync` arguments):
/// the file infos changed since the last write (`m_dirty_fileinfo`) and `m_last_blockfile`
/// (`DB_LAST_BLOCK`). Only handed out after the durability barrier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyFiles {
    pub infos: Vec<(u32, BlockFileInfo)>,
    pub last_file: u32,
}

/// The block and undo files of one node.
///
/// Writers take `&mut self` (the node has one writer thread); [`reader`](Self::reader) hands out
/// a cheap handle for concurrent reads. After any write, allocation or sync failure every further
/// mutating call returns [`FlatFileError::Failed`] (A1 §3.5.6, I5).
pub struct BlockFiles<F: FlatFs = OsFs> {
    cfg: BlockFilesConfig,
    blocks: FlatFileSeq<F>,
    undo: FlatFileSeq<F>,
    infos: Vec<BlockFileInfo>,
    last_file: u32,
    /// Core v24 `m_undo_height_in_last_blockfile`; reset to 0 at startup and at every rollover.
    undo_height_in_last_file: u32,
    dirty: BTreeSet<u32>,
    check_for_pruning: bool,
    failed: bool,
}

impl BlockFiles<OsFs> {
    /// Opens the block files in `cfg.dir` with the real filesystem.
    pub fn open(cfg: BlockFilesConfig, state: BlockFilesState) -> Self {
        Self::with_fs(Arc::new(OsFs::new()), cfg, state)
    }
}

impl<F: FlatFs> BlockFiles<F> {
    pub fn with_fs(fs: Arc<F>, cfg: BlockFilesConfig, state: BlockFilesState) -> Self {
        let mut infos = state.infos;
        // Core: node/blockstorage.cpp LoadBlockIndexDB resizes m_blockfile_info to the last file
        // (and v24 asserts it before FlushBlockFile reads it).
        if infos.len() <= state.last_file as usize {
            infos.resize(state.last_file as usize + 1, BlockFileInfo::default());
        }
        Self {
            blocks: FlatFileSeq::new(Arc::clone(&fs), cfg.dir.clone(), "blk", cfg.block_chunk()),
            undo: FlatFileSeq::new(fs, cfg.dir.clone(), "rev", cfg.undo_chunk()),
            infos,
            last_file: state.last_file,
            undo_height_in_last_file: 0,
            dirty: BTreeSet::new(),
            check_for_pruning: false,
            failed: false,
            cfg,
        }
    }

    pub fn last_file(&self) -> u32 {
        self.last_file
    }

    /// The in-memory file infos, including bytes not yet durable: for inspection, never for
    /// persisting (use [`sync_and_state`](Self::sync_and_state) or
    /// [`sync_and_take_dirty_infos`](Self::sync_and_take_dirty_infos)).
    pub fn infos(&self) -> &[BlockFileInfo] {
        &self.infos
    }

    // Core: node/blockstorage.cpp BlockManager::GetBlockFileInfo
    pub fn file_info(&self, file: u32) -> Option<&BlockFileInfo> {
        self.infos.get(file as usize)
    }

    /// The durability barrier, then the whole state (every file info and the last file): what
    /// reopening needs. Every byte it describes is durable when it is returned (A1 §3.5.3 I1).
    pub fn sync_and_state(&mut self) -> FlatResult<BlockFilesState> {
        self.mutate(|s| {
            s.sync_inner()?;
            Ok(BlockFilesState {
                infos: s.infos.clone(),
                last_file: s.last_file,
            })
        })
    }

    /// The durability barrier, then what the next block-index write records about the files: the
    /// infos changed since the last call and the last file. Every byte they describe is durable
    /// when they are returned (A1 §3.5.3 I1); a new file's directory entry is, best-effort, as in
    /// Core (DEC-212, BL-124).
    pub fn sync_and_take_dirty_infos(&mut self) -> FlatResult<DirtyFiles> {
        self.mutate(|s| {
            s.sync_inner()?;
            let dirty = std::mem::take(&mut s.dirty);
            Ok(DirtyFiles {
                infos: dirty.into_iter().map(|n| (n, s.infos[n as usize])).collect(),
                last_file: s.last_file,
            })
        })
    }

    /// Takes over files another process wrote (a Core datadir import, IM1/CH6): every blk and rev
    /// file the infos describe, and the directory, are synced by the next barrier before anything
    /// references them.
    pub fn adopt_existing_files(&mut self) {
        for (n, info) in self.infos.iter().enumerate() {
            if info.size > 0 {
                self.blocks.mark_unsynced(n as u32);
            }
            if info.undo_size > 0 {
                self.undo.mark_unsynced(n as u32);
            }
        }
        self.blocks.mark_dir_unsynced();
    }

    /// Whether an allocation in prune mode asked for a pruning check (Core's
    /// `m_check_for_pruning`); clears the flag.
    pub fn take_check_for_pruning(&mut self) -> bool {
        std::mem::take(&mut self.check_for_pruning)
    }

    /// Arms the pruning check again: CH2 §4.2 step 7's re-arm, when the files it may prune leave
    /// the target unmet while its plan had met it (T40).
    pub fn rearm_check_for_pruning(&mut self) {
        self.check_for_pruning = true;
    }

    /// Whether a write or sync failed (the store refuses further writes).
    pub fn has_failed(&self) -> bool {
        self.failed
    }

    /// A handle for reading blocks and undo records, usable from other threads.
    pub fn reader(&self) -> BlockFileReader<F> {
        BlockFileReader::new(Arc::clone(self.blocks.fs()), self.cfg.dir.clone())
    }

    /// Runs a mutating operation: refuses after an earlier failure, and records a new one
    /// (A1 §3.5.6, I5).
    fn mutate<T>(&mut self, op: impl FnOnce(&mut Self) -> FlatResult<T>) -> FlatResult<T> {
        if self.failed {
            return Err(FlatFileError::Failed);
        }
        let r = op(self);
        self.failed |= r.is_err();
        r
    }

    fn info_mut(&mut self, file: u32) -> &mut BlockFileInfo {
        let n = file as usize;
        if self.infos.len() <= n {
            self.infos.resize(n + 1, BlockFileInfo::default());
        }
        &mut self.infos[n]
    }

    /// Stores a block (Core's `SaveBlockToDisk`). `block` is Core's serialization of it. With
    /// `known_pos` (`-reindex`: the block already sits at that data position) nothing is written
    /// and only the statistics are updated; `block` must then be exactly the block's serialization
    /// (Core's `GetSerializeSize`), never the whole record, whose size field may claim more.
    /// Returns the data position (after the 8-byte header).
    // Core: node/blockstorage.cpp BlockManager::SaveBlockToDisk
    pub fn save_block(
        &mut self,
        block: &CoreBytes,
        height: u32,
        time: u64,
        known_pos: Option<FlatFilePos>,
    ) -> FlatResult<FlatFilePos> {
        self.mutate(|s| s.save_block_inner(block.as_slice(), height, time, known_pos))
    }

    /// [`save_block`](Self::save_block) for raw bytes: the crate's unit tests, which store
    /// block-like payloads of chosen sizes and need no `CoreBytes`.
    #[cfg(test)]
    pub(crate) fn save_block_bytes(
        &mut self,
        block: &[u8],
        height: u32,
        time: u64,
        known_pos: Option<FlatFilePos>,
    ) -> FlatResult<FlatFilePos> {
        self.mutate(|s| s.save_block_inner(block, height, time, known_pos))
    }

    fn save_block_inner(
        &mut self,
        block: &[u8],
        height: u32,
        time: u64,
        known_pos: Option<FlatFilePos>,
    ) -> FlatResult<FlatFilePos> {
        let block_size = u32::try_from(block.len()).expect("blocks are far below 4 GiB");
        let mut size = block_size;
        if known_pos.is_none() {
            // A new record gets its header; a known position already points past it.
            size += BLOCK_SERIALIZATION_HEADER_SIZE;
        }
        let pos = self.find_block_pos(known_pos, size, height, time)?;
        if known_pos.is_some() {
            return Ok(pos);
        }
        // Core: node/blockstorage.cpp WriteBlockToDisk
        let header = self.header(block_size);
        self.blocks.write(pos.file, u64::from(pos.pos), &[&header, block], WRITE_BLOCK_FAILED)?;
        Ok(FlatFilePos::new(pos.file, pos.pos + BLOCK_SERIALIZATION_HEADER_SIZE))
    }

    fn header(&self, size: u32) -> [u8; 8] {
        let mut h = [0u8; 8];
        h[..4].copy_from_slice(&self.cfg.magic);
        h[4..].copy_from_slice(&size.to_le_bytes());
        h
    }

    /// Chooses the position of a new record of `add_size` bytes (header included), rolling over
    /// to the next file and finalizing the previous one when it would reach the maximum size.
    // Core: node/blockstorage.cpp BlockManager::FindBlockPos
    fn find_block_pos(
        &mut self,
        known: Option<FlatFilePos>,
        add_size: u32,
        height: u32,
        time: u64,
    ) -> FlatResult<FlatFilePos> {
        let mut file = known.map_or(self.last_file, |p| p.file);
        self.info_mut(file);
        let mut finalize_undo = false;
        let mut pos = known.unwrap_or_default();
        if known.is_none() {
            let mut max_file_size = MAX_BLOCKFILE_SIZE;
            // Use smaller blockfiles in test-only -fastprune mode, but avoid the possibility of
            // having a block not fit into the block file.
            if self.cfg.fastprune {
                max_file_size = FASTPRUNE_MAX_FILE;
                if add_size >= max_file_size {
                    max_file_size = add_size + 1;
                }
            }
            assert!(add_size < max_file_size, "a block cannot exceed MAX_BLOCKFILE_SIZE");
            while self.infos[file as usize].size + add_size >= max_file_size {
                // When the undo file keeps up with the block file, finalize it now; when it lags
                // (blocks arrive faster than they connect), the undo write of the file's last
                // block finalizes it.
                let height_last = self.infos[file as usize].height_last;
                // The rev file is finalized if its last block's height equals the highest height
                // whose undo was written to the current file since startup
                // (`m_undo_height_in_last_blockfile`; bitcoin#27746, Dash #7553).
                finalize_undo = height_last == self.undo_height_in_last_file;
                file += 1;
                self.info_mut(file);
            }
            pos = FlatFilePos::new(file, self.infos[file as usize].size);
        }

        if file != self.last_file {
            if known.is_none() {
                tracing::debug!(target: "blockstorage", "Leaving block file {}: {}", self.last_file, self.infos[self.last_file as usize]);
            }
            self.flush_block_file_inner(known.is_none(), finalize_undo)?;
            self.last_file = file;
            self.undo_height_in_last_file = 0;
        }

        let info = &mut self.infos[file as usize];
        info.add_block(height, time);
        if known.is_some() {
            info.size = info.size.max(pos.pos + add_size);
        } else {
            info.size += add_size;
        }

        if known.is_some() {
            // The bytes may still be only in the page cache of a process that was killed.
            self.blocks.mark_unsynced(file);
        } else {
            let allocated = self.blocks.allocate(pos, add_size)?;
            if allocated != 0 && self.cfg.prune_mode {
                self.check_for_pruning = true;
            }
        }
        self.dirty.insert(file);
        Ok(pos)
    }

    /// Stores the undo record of a block stored in blk file `file` at `height` (Core's
    /// `WriteUndoDataForBlock`, for a block without undo data yet). `undo` is Core's serialization
    /// of the `CBlockUndo`; the checksum binds it to `prev_hash`, the hash of the block's parent. Returns
    /// the data position, for the caller to record as `nUndoPos` with `BLOCK_HAVE_UNDO`.
    // Core: node/blockstorage.cpp BlockManager::WriteUndoDataForBlock
    pub fn write_undo(
        &mut self,
        undo: &CoreUndoBytes,
        file: u32,
        height: u32,
        prev_hash: &[u8; 32],
    ) -> FlatResult<FlatFilePos> {
        self.write_undo_slice(undo.as_slice(), file, height, prev_hash)
    }

    /// [`write_undo`](Self::write_undo) over a serialized `CBlockUndo` the caller built in place
    /// (CH3's connector streams it, CH3 §2.10.1): the same record, rules and errors. Nothing checks
    /// that `undo` decodes, so the caller answers for it, as the encoder of a [`CoreUndoBytes`] does.
    // Core: node/blockstorage.cpp BlockManager::WriteUndoDataForBlock
    pub fn write_undo_slice(
        &mut self,
        undo: &[u8],
        file: u32,
        height: u32,
        prev_hash: &[u8; 32],
    ) -> FlatResult<FlatFilePos> {
        self.mutate(|s| s.write_undo_inner(undo, file, height, prev_hash))
    }

    fn write_undo_inner(
        &mut self,
        undo: &[u8],
        file: u32,
        height: u32,
        prev_hash: &[u8; 32],
    ) -> FlatResult<FlatFilePos> {
        let undo_size = u32::try_from(undo.len()).expect("undo records are far below 4 GiB");
        let pos = self.find_undo_pos(
            file,
            undo_size + BLOCK_SERIALIZATION_HEADER_SIZE + UNDO_CHECKSUM_SIZE,
        )?;

        // Core: node/blockstorage.cpp UndoWriteToDisk
        // Undo records are small: one write. `FlatFs` has no vectored write, so the record's three
        // parts would be three writes; a copy is cheaper (CH3a D72).
        let mut record = Vec::with_capacity(undo.len() + 40);
        record.extend_from_slice(&self.header(undo_size));
        record.extend_from_slice(undo);
        record.extend_from_slice(&undo_checksum(prev_hash, undo));
        self.undo.write(pos.file, u64::from(pos.pos), &[&record], WRITE_UNDO_FAILED)?;

        // rev files are written in connection order and blk files in arrival order: flush (and
        // finalize) a rev file once the undo of its blk file's last block is written. Undo that
        // keeps up with the blocks is finalized at the blk rollover instead (find_block_pos).
        if pos.file < self.last_file && height == self.infos[pos.file as usize].height_last {
            self.flush_undo_file_inner(pos.file, true)?;
        } else if pos.file == self.last_file && height > self.undo_height_in_last_file {
            self.undo_height_in_last_file = height;
        }
        Ok(FlatFilePos::new(pos.file, pos.pos + BLOCK_SERIALIZATION_HEADER_SIZE))
    }

    // Core: node/blockstorage.cpp BlockManager::FindUndoPos
    fn find_undo_pos(&mut self, file: u32, add_size: u32) -> FlatResult<FlatFilePos> {
        let info = self.info_mut(file);
        let pos = FlatFilePos::new(file, info.undo_size);
        info.undo_size += add_size;
        self.dirty.insert(file);
        let allocated = self.undo.allocate(pos, add_size)?;
        if allocated != 0 && self.cfg.prune_mode {
            self.check_for_pruning = true;
        }
        Ok(pos)
    }

    /// Core's `FlushBlockFile`: sync the current blk file (truncating it to its used size when
    /// `finalize`), and its rev file unless finalizing a blk file whose undo lags.
    // Core: node/blockstorage.cpp BlockManager::FlushBlockFile
    pub fn flush_block_file(&mut self, finalize: bool, finalize_undo: bool) -> FlatResult<()> {
        self.mutate(|s| s.flush_block_file_inner(finalize, finalize_undo))
    }

    fn flush_block_file_inner(&mut self, finalize: bool, finalize_undo: bool) -> FlatResult<()> {
        // Unreachable: `with_fs` sizes the infos to the last file (v24's early return before the
        // infos are loaded).
        let Some(info) = self.infos.get(self.last_file as usize) else {
            return Ok(());
        };
        let pos = FlatFilePos::new(self.last_file, info.size);
        self.blocks.flush(pos, finalize, FLUSH_BLOCK_FAILED)?;
        if !finalize || finalize_undo {
            self.flush_undo_file_inner(self.last_file, finalize_undo)?;
        }
        Ok(())
    }

    /// Core's `FlushUndoFile`.
    // Core: node/blockstorage.cpp BlockManager::FlushUndoFile
    pub fn flush_undo_file(&mut self, file: u32, finalize: bool) -> FlatResult<()> {
        // Core would index past `m_blockfile_info`; refuse instead of truncating to 0.
        if self.infos.get(file as usize).is_none() {
            return Err(FlatFileError::Corrupt {
                what: "undo",
                pos: FlatFilePos::new(file, 0),
                detail: "no such block file".into(),
            });
        }
        self.mutate(|s| s.flush_undo_file_inner(file, finalize))
    }

    fn flush_undo_file_inner(&mut self, file: u32, finalize: bool) -> FlatResult<()> {
        let size = self.infos.get(file as usize).map_or(0, |i| i.undo_size);
        self.undo.flush(FlatFilePos::new(file, size), finalize, FLUSH_UNDO_FAILED)
    }

    /// The durability barrier before a block-index write (A1 §3.5.4 step 2), at every point Core's
    /// `FlushStateToDisk` calls `FlushBlockFile()`. It does what that call does (sync the current
    /// blk/rev pair and `blocks/`, creating an empty rev file if the current blk file has no undo
    /// yet), then `fdatasync`s every other blk and rev file written since its last sync. Core
    /// syncs only the current pair; undo written into an older, lagging rev file is synced too
    /// (†). Each `Flush`'s directory sync is Core's `DirectoryCommit` (DEC-212, BL-124): a failure
    /// is logged as a warning, `blocks/` stays unsynced, and the barrier completes.
    pub fn sync(&mut self) -> FlatResult<()> {
        self.mutate(Self::sync_inner)
    }

    fn sync_inner(&mut self) -> FlatResult<()> {
        // Core: validation.cpp FlushStateToDisk -> BlockManager::FlushBlockFile()
        self.flush_block_file_inner(false, false)?;
        self.blocks.sync_files(FLUSH_BLOCK_FAILED)?;
        self.undo.sync_files(FLUSH_UNDO_FAILED)?;
        Ok(())
    }

    /// `fsync` of `blocks/`, which holds both sequences.
    fn sync_blocks_dir(&mut self, context: &'static str) -> FlatResult<()> {
        self.blocks.sync_dir(context)?;
        self.undo.mark_dir_synced();
        Ok(())
    }

    /// Bytes used by all blk and rev files (Core's `CalculateCurrentUsage`).
    pub fn calculate_current_usage(&self) -> u64 {
        self.infos.iter().map(|i| u64::from(i.size) + u64::from(i.undo_size)).sum()
    }

    /// Forgets a file's statistics before it is unlinked (the file half of Core's
    /// `PruneOneBlockFile`; the caller clears `BLOCK_HAVE_DATA`/`BLOCK_HAVE_UNDO` and the positions
    /// of every index entry in `file`).
    // Core: node/blockstorage.cpp BlockManager::PruneOneBlockFile
    pub fn prune_one_block_file(&mut self, file: u32) {
        *self.info_mut(file) = BlockFileInfo::default();
        self.dirty.insert(file);
    }

    /// The files `pruneblockchain` removes: every file before the current one whose blocks are
    /// all at or below `min(manual_height, tip − MIN_BLOCKS_TO_KEEP)`. Each is pruned with
    /// [`prune_one_block_file`](Self::prune_one_block_file).
    // Core: node/blockstorage.cpp BlockManager::FindFilesToPruneManual
    pub fn find_files_to_prune_manual(
        &mut self,
        manual_height: u32,
        tip_height: Option<u32>,
    ) -> Vec<u32> {
        let plan = self.plan_files_to_prune_manual(manual_height, tip_height);
        self.apply_prune_plan(plan)
    }

    /// The files automatic pruning removes to get below `prune_target` bytes. Each is pruned with
    /// [`prune_one_block_file`](Self::prune_one_block_file).
    // Core: node/blockstorage.cpp BlockManager::FindFilesToPrune
    pub fn find_files_to_prune(
        &mut self,
        prune_target: u64,
        prune_after_height: u64,
        tip_height: Option<u32>,
        prune_height: i32,
        is_ibd: bool,
    ) -> Vec<u32> {
        let plan = self.plan_files_to_prune(
            prune_target,
            prune_after_height,
            tip_height,
            prune_height,
            is_ibd,
        );
        self.apply_prune_plan(plan)
    }

    /// The selectors' second half: prunes each planned file, in order, then logs the selection.
    fn apply_prune_plan(&mut self, plan: PrunePlan) -> Vec<u32> {
        for &file in &plan.files {
            self.prune_one_block_file(file);
        }
        plan.log();
        plan.files
    }

    /// [`find_files_to_prune_manual`](Self::find_files_to_prune_manual)'s selection, with no side
    /// effects and no log (CH2 §2.7: step 0's dry run). Every selected file keeps its statistics
    /// until [`prune_one_block_file`](Self::prune_one_block_file).
    // Core: node/blockstorage.cpp BlockManager::FindFilesToPruneManual
    pub fn plan_files_to_prune_manual(
        &self,
        manual_height: u32,
        tip_height: Option<u32>,
    ) -> PrunePlan {
        let Some(tip) = tip_height else {
            return PrunePlan {
                files: Vec::new(),
                met_target: true,
                log: None,
            };
        };
        let last_prunable = last_prunable_manual(manual_height, tip);
        let files = (0..self.last_file)
            .filter(|&file| {
                let info = self.infos[file as usize];
                info.size != 0 && info.height_last <= last_prunable
            })
            .collect();
        PrunePlan {
            files,
            met_target: true,
            log: Some(PruneLog::Manual {
                last_prunable,
            }),
        }
    }

    /// [`find_files_to_prune`](Self::find_files_to_prune)'s selection, with no side effects and
    /// no log (CH2 §2.7: step 0's dry run). Core's loop lowers its usage as it selects, so the plan
    /// does too, on a copy: the files stay as they are until
    /// [`prune_one_block_file`](Self::prune_one_block_file).
    // Core: node/blockstorage.cpp BlockManager::FindFilesToPrune
    pub fn plan_files_to_prune(
        &self,
        prune_target: u64,
        prune_after_height: u64,
        tip_height: Option<u32>,
        prune_height: i32,
        is_ibd: bool,
    ) -> PrunePlan {
        let mut usage = self.calculate_current_usage();
        let mut files = Vec::new();
        let Some(tip) = tip_height else {
            return PrunePlan::unlogged(usage, prune_target);
        };
        if prune_target == 0 || u64::from(tip) <= prune_after_height {
            return PrunePlan::unlogged(usage, prune_target);
        }
        let last_prunable = last_prunable(prune_height, tip);
        // Leave room under the target for the next allocation before the next pruning check.
        let mut buffer = PRUNE_BUFFER;
        if usage + buffer >= prune_target {
            if is_ibd {
                buffer += prune_target / 10;
            }
            for file in 0..self.last_file {
                let info = self.infos[file as usize];
                let bytes = u64::from(info.size) + u64::from(info.undo_size);
                if info.size == 0 {
                    continue;
                }
                if usage + buffer < prune_target {
                    break;
                }
                if info.height_last > last_prunable {
                    continue;
                }
                files.push(file);
                usage -= bytes;
            }
        }
        PrunePlan {
            files,
            met_target: target_met(usage, prune_target),
            log: Some(PruneLog::Automatic {
                prune_target,
                usage,
                last_prunable,
            }),
        }
    }

    /// Whether step 7 may still prune `file` under `bound` (CH2 §4.2: `D ∩ eligible(actual)`): a
    /// file before the current one, not yet pruned, whose blocks are all at or below `bound`. This
    /// is the selectors' per-file test with the bound the prune locks give at step 7; it never
    /// re-applies the target, so a selection filtered by it is a subset of the plan's (T39).
    // Core: node/blockstorage.cpp BlockManager::FindFilesToPrune[Manual] (the per-file checks)
    pub fn eligible(&self, file: u32, bound: u32) -> bool {
        file < self.last_file
            && self.infos.get(file as usize).is_some_and(|i| i.size != 0 && i.height_last <= bound)
    }

    /// Whether the files' usage is below `prune_target` with the next allocation's room: the test
    /// that ends automatic pruning's loop, without its IBD margin. Step 7 re-arms the automatic
    /// check when its selection leaves this false while the plan had made it true (T40).
    pub fn prune_target_met(&self, prune_target: u64) -> bool {
        target_met(self.calculate_current_usage(), prune_target)
    }

    /// Unlinks the blk and rev files of pruned files, then syncs the directory, best-effort (Core
    /// does not sync it). A1 §3.5.4 step 7: call this only after the index write that cleared the
    /// files' blocks is durable. As v24 (O17 CONS-28, every profile), a file that cannot be
    /// removed is logged and left: once the index no longer references it, it is only wasted
    /// space, and the startup scan retries it. A failed directory sync is logged the same way,
    /// and nothing stops, as Core keeps running (DEC-95): a removal a power loss undoes is such a
    /// file. Core logs neither failure, so the warnings are extra log lines (CH2b's D-b2-10).
    ///
    /// # Errors
    ///
    /// [`FlatFileError::Failed`] after an earlier failure.
    // Core: node/blockstorage.cpp UnlinkPrunedFiles
    pub fn unlink_pruned_files(&mut self, files: &[u32]) -> FlatResult<()> {
        self.mutate(|s| s.unlink_pruned_files_inner(files))
    }

    fn unlink_pruned_files_inner(&mut self, files: &[u32]) -> FlatResult<()> {
        for &file in files {
            let remove = |seq: &mut FlatFileSeq<F>| match seq.remove(file) {
                Ok(removed) => removed,
                Err(e) => {
                    tracing::warn!(target: "blockstorage", "Prune: {e}");
                    false
                }
            };
            let removed_block = remove(&mut self.blocks);
            let removed_undo = remove(&mut self.undo);
            if removed_block || removed_undo {
                tracing::debug!(target: "blockstorage", "Prune: UnlinkPrunedFiles deleted blk/rev ({:05})", file);
            }
        }
        if let Err(e) = self.sync_blocks_dir(DIR_SYNC_FAILED) {
            tracing::warn!(target: "blockstorage", "Prune: {e}");
        }
        Ok(())
    }

    /// At startup, removes the files of already-pruned entries (size 0) that are still on disk.
    // Core: node/blockstorage.cpp BlockManager::ScanAndUnlinkAlreadyPrunedFiles
    pub fn scan_and_unlink_already_pruned_files(&mut self, have_pruned: bool) -> FlatResult<()> {
        if !have_pruned {
            return Ok(());
        }
        let files: Vec<u32> = (0..self.last_file)
            .filter(|&n| self.infos.get(n as usize).is_some_and(|i| i.size == 0))
            .collect();
        self.unlink_pruned_files(&files)
    }
}

/// Reads blocks and undo records by position, with `pread`.
pub struct BlockFileReader<F: FlatFs = OsFs> {
    blocks: FlatFileSeq<F>,
    undo: FlatFileSeq<F>,
}

impl BlockFileReader<OsFs> {
    /// A reader over a blocks directory that opens every file read-only (safe on another node's
    /// datadir).
    pub fn open_read_only(dir: impl Into<PathBuf>) -> Self {
        Self::new(Arc::new(OsFs::read_only()), dir.into())
    }
}

impl<F: FlatFs> BlockFileReader<F> {
    /// Chunk sizes only matter for allocation, which a reader never does.
    fn new(fs: Arc<F>, dir: PathBuf) -> Self {
        Self {
            blocks: FlatFileSeq::new(Arc::clone(&fs), dir.clone(), "blk", 1),
            undo: FlatFileSeq::new(fs, dir, "rev", 1),
        }
    }

    /// Reads the record at `pos`: its size field's bytes plus `trailer`. A size above `max` (a
    /// forged or damaged field) is [`FlatFileError::Corrupt`] before anything is allocated; the
    /// buffer is reserved fallibly, so an allocation failure is an error, not an abort.
    fn read_sized(
        &self,
        seq: &FlatFileSeq<F>,
        what: &'static str,
        pos: FlatFilePos,
        max: u32,
        trailer: u32,
        out: &mut Vec<u8>,
    ) -> FlatResult<()> {
        let path = || seq.file_name(pos.file);
        let corrupt = |detail: String| FlatFileError::Corrupt {
            what,
            pos,
            detail,
        };
        if pos.pos < BLOCK_SERIALIZATION_HEADER_SIZE {
            return Err(corrupt("position inside the file header".into()));
        }
        let mut size = [0u8; 4];
        seq.read(pos.file, u64::from(pos.pos - 4), &mut size)
            .map_err(|e| FlatFileError::io("Failed to read the record size", path(), e))?;
        let size = u32::from_le_bytes(size);
        if size > max {
            return Err(corrupt(format!("record size {size} exceeds the {max}-byte limit")));
        }
        let len = u64::from(size) + u64::from(trailer);
        let file_len = seq
            .len(pos.file)
            .map_err(|e| FlatFileError::io("Failed to read the record", path(), e))?
            .unwrap_or(0);
        if u64::from(pos.pos) + len > file_len {
            return Err(corrupt(format!("record of {size} bytes runs past the end of the file")));
        }
        let len = usize::try_from(len).map_err(|_| corrupt(format!("record of {size} bytes")))?;
        out.clear();
        out.try_reserve_exact(len).map_err(|e| {
            let e = std::io::Error::new(std::io::ErrorKind::OutOfMemory, e);
            FlatFileError::io("Failed to allocate the record", path(), e)
        })?;
        out.resize(len, 0);
        seq.read(pos.file, u64::from(pos.pos), out)
            .map_err(|e| FlatFileError::io("Failed to read the record", path(), e))
    }

    // Core: node/blockstorage.cpp OpenBlockFile(FlatFilePos(file, 0), true), flatfile.cpp
    // FlatFileSeq::Open
    /// Opens blk file `file` read-only, as `LoadBlockIndexDB`'s check of the files that hold
    /// blocks does. A file that does not open is an error whose
    /// [`open_failed`](FlatFileError::open_failed) names it.
    pub fn open_block_file(&self, file: u32) -> FlatResult<()> {
        self.blocks
            .read(file, 0, &mut [])
            .map_err(|e| FlatFileError::io("Unable to open file", self.blocks.file_name(file), e))
    }

    /// Reads the record at a block's data position: as many bytes as the record's size field says,
    /// which must be at most [`MAX_BLOCK_RECORD_SIZE`] (else [`FlatFileError::Corrupt`]). The
    /// caller decodes the block from the start of them and checks its hash and proof of work, as
    /// Core's `ReadBlockFromDisk` does. The record can hold bytes after the block: Core's
    /// `-reindex` scanner accepts a size field larger than the block (up to the same limit), and
    /// Core reads such a block by deserializing it, ignoring the rest; so must the caller.
    // Core: node/blockstorage.cpp ReadBlockFromDisk
    pub fn read_block(&self, pos: FlatFilePos, out: &mut Vec<u8>) -> FlatResult<()> {
        self.read_sized(&self.blocks, "block", pos, MAX_BLOCK_RECORD_SIZE, 0, out)
    }

    /// Reads a serialized `CBlockUndo` at a data position and checks its checksum against
    /// `prev_hash`, the hash of the block's parent (Core's "Checksum mismatch"). A size field above
    /// [`MAX_UNDO_RECORD_SIZE`] is [`FlatFileError::Corrupt`].
    // Core: node/blockstorage.cpp UndoReadFromDisk
    pub fn read_undo(
        &self,
        pos: FlatFilePos,
        prev_hash: &[u8; 32],
        out: &mut Vec<u8>,
    ) -> FlatResult<()> {
        self.read_sized(&self.undo, "undo", pos, MAX_UNDO_RECORD_SIZE, UNDO_CHECKSUM_SIZE, out)?;
        let at = out.len() - UNDO_CHECKSUM_SIZE as usize;
        if undo_checksum(prev_hash, &out[..at]) != out[at..] {
            return Err(FlatFileError::Corrupt {
                what: "undo",
                pos,
                detail: "Checksum mismatch".into(),
            });
        }
        out.truncate(at);
        Ok(())
    }

    /// Core's `UndoReadFromDisk` at a non-null undo position (CH3 §2.10.2, D45, D46): decodes the
    /// `CBlockUndo` from `pos` as Core's stream does, reads the checksum right after the bytes the
    /// decode consumed, and checks it against `prev_hash`, the hash of the block's parent. On
    /// success it calls `apply` with the record, borrowed from `buf` (kept by the caller for its
    /// capacity), and returns what `apply` returns.
    ///
    /// The rev file is opened by path for this read ([`FlatFs::open_read`]), never through a
    /// cached handle. The record's size field is only a hint ([`read_undo_stream`]).
    // Core: node/blockstorage.cpp:539-575 UndoReadFromDisk, :649-652 OpenUndoFile
    pub fn read_undo_record<T>(
        &self,
        pos: FlatFilePos,
        prev_hash: &[u8; 32],
        buf: &mut Vec<u8>,
        apply: impl FnOnce(BlockUndoRef<'_>) -> T,
    ) -> Result<T, UndoReadError> {
        let path = self.undo.file_name(pos.file);
        match self.undo.fs().open_read(&path) {
            Ok(file) => {
                read_undo_stream(&*file, pos.pos, prev_hash, buf, UNDO_STREAM_FIRST_READ, apply)
            }
            Err(source) => Err(UndoReadError::Open {
                path,
                source,
            }),
        }
    }
}

/// [`BlockFileReader::read_undo_record`] over an open file, for the record at byte `pos`: Core's
/// `verifier >> blockundo; filein >> hashChecksum` (D45).
///
/// Core never reads the record's size field. Here it is a hint: when it is within
/// [`MAX_UNDO_RECORD_SIZE`] and fits in the file, the first read is that many bytes plus the
/// checksum, which on an intact file is the whole read. Otherwise the first read is `first` bytes
/// (at least one; 64 KiB for the node). Whenever the
/// decode, or the checksum after it, runs past what was read, the read doubles, up to the end of
/// the file. A decode error other than the end of the bytes is final: Core's decoder fails at the
/// same byte on the same prefix. So the outcome is Core's whatever the size field says, and the
/// buffer never outgrows the file.
///
/// `pub` for the `undo_decode` fuzz target, which checks that neither the size field nor `first`
/// changes the outcome; its files are far smaller than 64 KiB, so only a smaller `first` reaches
/// the doubling.
#[doc(hidden)]
pub fn read_undo_stream<T>(
    file: &dyn FlatRead,
    pos: u32,
    prev_hash: &[u8; 32],
    buf: &mut Vec<u8>,
    first: u64,
    apply: impl FnOnce(BlockUndoRef<'_>) -> T,
) -> Result<T, UndoReadError> {
    let first = first.max(1);
    let checksum = UNDO_CHECKSUM_SIZE as usize;
    let at = u64::from(pos);
    let avail = file.len().map_err(UndoReadFailure::io)?.saturating_sub(at);
    let hinted = size_hint(file, pos)
        .filter(|&size| size <= MAX_UNDO_RECORD_SIZE)
        .map(|size| u64::from(size) + checksum as u64);
    let mut want = hinted.filter(|&n| n <= avail).unwrap_or(avail.min(first));
    buf.clear();
    loop {
        let have = buf.len();
        let len = usize::try_from(want).map_err(|_| UndoReadFailure::out_of_memory())?;
        buf.try_reserve_exact(len - have).map_err(|_| UndoReadFailure::out_of_memory())?;
        buf.resize(len, 0);
        file.read_exact_at(at + have as u64, &mut buf[have..]).map_err(UndoReadFailure::io)?;
        let mut rest = &buf[..];
        match BlockUndoRef::decode(&mut rest) {
            Ok(undo) if rest.len() >= checksum => {
                let record = &buf[..len - rest.len()];
                if undo_checksum(prev_hash, record)[..] != rest[..checksum] {
                    return Err(UndoReadError::Checksum);
                }
                return Ok(apply(undo));
            }
            Err(e) if e.kind != ReadErrorKind::EndOfData => {
                return Err(UndoReadError::Read(UndoReadFailure::Decode(e.kind)));
            }
            // The record or its checksum runs past the bytes read.
            _ if want == avail => return Err(UndoReadError::Read(UndoReadFailure::EndOfFile)),
            _ => want = want.saturating_mul(2).max(first).min(avail),
        }
    }
}

/// The node's first read of [`read_undo_stream`] when the size field is no use.
#[doc(hidden)]
pub const UNDO_STREAM_FIRST_READ: u64 = 64 * 1024;

/// The record's size field, the 4 bytes before `pos`, if they read.
fn size_hint(file: &dyn FlatRead, pos: u32) -> Option<u32> {
    let mut size = [0; 4];
    file.read_exact_at(u64::from(pos.checked_sub(4)?), &mut size).ok()?;
    Some(u32::from_le_bytes(size))
}

/// Why [`BlockFileReader::read_undo_record`] failed: the classes of Core's `UndoReadFromDisk`
/// lines (`node/blockstorage.cpp:549-571`), each after `"UndoReadFromDisk: "`. Each fails the
/// disconnect (`DISCONNECT_FAILED`); none is a storage fault (CH3 §2.10.2).
#[derive(Debug, thiserror::Error)]
pub enum UndoReadError {
    /// The rev file did not open. Core logs "Unable to open file <path>" (`flatfile.cpp:45`)
    /// first ([`open_failed`](Self::open_failed)).
    #[error("OpenUndoFile failed")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Reading or decoding threw.
    #[error("Deserialize or I/O error - {0}")]
    Read(UndoReadFailure),
    #[error("Checksum mismatch")]
    Checksum,
}

impl UndoReadError {
    /// The rev file that did not open.
    pub fn open_failed(&self) -> Option<&Path> {
        match self {
            Self::Open {
                path,
                ..
            } => Some(path),
            _ => None,
        }
    }
}

/// What Core's read throws, by class; its display is the exception's `what()` (CH3 §2.10.2).
#[derive(Debug)]
pub enum UndoReadFailure {
    /// The file ended inside the record or its checksum (`AutoFile::read`, `streams.cpp:27`).
    EndOfFile,
    /// A read failed (`AutoFile::read`'s `fread` short of the end), or the buffer could not grow,
    /// which Core has no line for (D45: log text only).
    Io(io::Error),
    /// The decoder refused the bytes (`serialize.h`'s `ReadCompactSize`, `ReadVarInt`).
    Decode(ReadErrorKind),
}

impl UndoReadFailure {
    fn io(e: io::Error) -> UndoReadError {
        UndoReadError::Read(match e.kind() {
            io::ErrorKind::UnexpectedEof => Self::EndOfFile,
            _ => Self::Io(e),
        })
    }

    fn out_of_memory() -> UndoReadError {
        UndoReadError::Read(Self::Io(io::ErrorKind::OutOfMemory.into()))
    }
}

/// libstdc++'s suffix to the message of a `std::ios_base::failure`, which every exception of
/// Core's read is (`streams.cpp`, `serialize.h`).
const IOS_FAILURE: &str = ": iostream error";

impl fmt::Display for UndoReadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndOfFile => write!(f, "AutoFile::read: end of file{IOS_FAILURE}"),
            Self::Io(e) if e.kind() == io::ErrorKind::OutOfMemory => f.write_str("std::bad_alloc"),
            Self::Io(_) => write!(f, "AutoFile::read: fread failed{IOS_FAILURE}"),
            Self::Decode(kind) => write!(f, "{kind}{IOS_FAILURE}"),
        }
    }
}

/// The checksum after every undo record: `SHA256d(prev_block_hash ‖ undo)`.
// Core: node/blockstorage.cpp UndoWriteToDisk
pub fn undo_checksum(prev_hash: &[u8; 32], undo: &[u8]) -> [u8; 32] {
    let mut engine = sha256d::Hash::engine();
    engine.input(prev_hash);
    engine.input(undo);
    sha256d::Hash::from_engine(engine).to_byte_array()
}
