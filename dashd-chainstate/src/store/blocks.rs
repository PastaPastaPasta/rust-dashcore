//! V's block files over ST2's (CH2b b4 M2, D-CH1-3): Core's `SaveBlockToDisk`,
//! `ReadBlockFromDisk` and `LoadBlockIndexDB`'s file check on the node's one [`GuardedFiles`],
//! which the flush and the connector's undo writes share. A stored block index then names blk
//! files that exist, and a restart that finds them loads (BL-127).

use std::sync::Arc;

use dash_chain::FlatFilePos as IndexPos;
use dash_consensus::coins::BlockUndo;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{Block, BlockHash, core_bytes, decode_block};
use dash_num::CompactTarget;
use dashd_store::flatfile::{BlockFileReader, BlockFiles, FlatFileError, FlatFilePos, FlatFs};

use super::{AbortSink, GuardedFiles};
use crate::recovery::FileRead;

// Core: node/blockstorage.cpp:869 (SaveBlockToDisk's AbortNode for WriteBlockToDisk)
const WRITE_BLOCK_FAILED: &str = "Failed to write block";
// Core: node/blockstorage.cpp:715 (FindBlockPos)
const DISK_SPACE_LOW: &str = "Disk space is too low!";

/// The node's block files as V reads and writes them ([`chain::BlockFiles`]).
///
/// [`chain::BlockFiles`]: crate::chain::BlockFiles
pub struct NodeBlockFiles<F: FlatFs> {
    files: Arc<GuardedFiles<F>>,
    reader: BlockFileReader<F>,
    /// `ReadBlockFromDisk`'s `CheckProofOfWork`.
    pow: PowParams,
    /// The node's `AbortNode`, for a failed write: the guard latches it and leaves the report
    /// to its caller.
    abort: Arc<dyn AbortSink>,
}

impl<F: FlatFs> NodeBlockFiles<F> {
    pub fn new(files: Arc<GuardedFiles<F>>, pow: PowParams, abort: Arc<dyn AbortSink>) -> Self {
        let reader = files.read(BlockFiles::reader);
        Self {
            files,
            reader,
            pow,
            abort,
        }
    }

    // Core: node/blockstorage.cpp:659-724 FindBlockPos, :577-603 FlushUndoFile and FlushBlockFile,
    // :748-771 WriteBlockToDisk; flatfile.cpp:56-80 FlatFileSeq::Allocate
    /// Core's report of a failed `SaveBlockToDisk`, whose null position it then returns (D62);
    /// for Core's exception, `AcceptBlock`'s "System error" (`v:4843-4844`), the exception's
    /// text instead. A failure the latch held already was reported by whoever tripped it.
    fn save_failed(&self, e: &FlatFileError) -> Option<String> {
        match e {
            FlatFileError::Failed => {}
            // :713-716, then :864
            FlatFileError::NoSpace => {
                self.abort.abort_node(DISK_SPACE_LOW, Some(DISK_SPACE_LOW));
                tracing::error!("SaveBlockToDisk: FindBlockPos failed");
            }
            // The write's open, and a failed write ST2 cannot tell from it (Core's `AutoFile`
            // exception, which `AcceptBlock` reports as a "System error").
            FlatFileError::Io {
                context: WRITE_BLOCK_FAILED,
                ..
            } => self.write_failed(e),
            // `Allocate`'s own I/O, under its space check's context. An `Open` that fails there
            // logs its line and allocates nothing, and the write's open then fails as above;
            // anything else (the space query, the directory) is an exception, `AcceptBlock`'s
            // "System error" (`validation.cpp:4843-4844`). The latch holds it, so V's stop would
            // not report it: the report is here, and V's `Err` path takes it from there.
            FlatFileError::Io {
                context: DISK_SPACE_LOW,
                source,
                ..
            } => {
                if e.open_failed().is_some() {
                    log_open_failed(e);
                    self.write_failed(e);
                } else {
                    let what = source.to_string();
                    self.abort.abort_node(&format!("System error: {what}"), None);
                    return Some(what);
                }
            }
            // :599-601 and FlushUndoFile's :580-582 at a rollover: their `AbortNode`, whose
            // text is the failure's context. Core goes on to write the block.
            FlatFileError::Io {
                context,
                ..
            } => self.abort.abort_node(context, None),
            FlatFileError::Corrupt {
                ..
            } => self.abort.abort_node(&e.to_string(), None),
        }
        None
    }

    // Core: node/blockstorage.cpp:751-754, then :869
    fn write_failed(&self, e: &FlatFileError) {
        log_open_failed(e);
        tracing::error!("WriteBlockToDisk: OpenBlockFile failed");
        self.abort.abort_node(WRITE_BLOCK_FAILED, None);
    }
}

// Core: flatfile.cpp:45 FlatFileSeq::Open
/// `FlatFileSeq::Open`'s line for a file that did not open.
fn log_open_failed(e: &FlatFileError) {
    if let Some(path) = e.open_failed() {
        tracing::info!("Unable to open file {}", path.display());
    }
}

fn store_pos(p: IndexPos) -> Option<FlatFilePos> {
    Some(FlatFilePos::new(u32::try_from(p.file).ok()?, p.pos))
}

/// Core's `FlatFilePos::ToString`, for a position ST2 has none for.
fn core_pos(p: IndexPos) -> String {
    format!("FlatFilePos(nFile={}, nPos={})", p.file, p.pos)
}

impl<F: FlatFs + 'static> crate::chain::BlockFiles for NodeBlockFiles<F> {
    // Core: node/blockstorage.cpp:850-874 SaveBlockToDisk
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<IndexPos>,
    ) -> Result<Option<IndexPos>, String> {
        let known = known
            .map(|p| {
                store_pos(p).ok_or_else(|| format!("a known position in no file: {}", core_pos(p)))
            })
            .transpose()?;
        let bytes = core_bytes(&**block);
        let time = u64::from(block.header().time());
        match self.files.mutate(|f| f.save_block(&bytes, height, time, known)) {
            Ok(pos) => Ok(Some(IndexPos {
                file: i32::try_from(pos.file).expect("blk file numbers fit Core's int"),
                pos: pos.pos,
            })),
            Err(e) => self.save_failed(&e).map_or(Ok(None), Err),
        }
    }

    // Core: node/blockstorage.cpp:805-848 ReadBlockFromDisk
    fn read_block(&self, pos: IndexPos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        let at = store_pos(pos).ok_or_else(|| {
            format!("ReadBlockFromDisk: OpenBlockFile failed for {}", core_pos(pos))
        })?;
        let mut bytes = Vec::new();
        self.reader.read_block(at, &mut bytes).map_err(|e| {
            log_open_failed(&e);
            FileRead::Block(at).line(&e)
        })?;
        let block =
            decode_block(&mut &bytes[..]).map_err(|e| FileRead::decode_line(at, &e))?.into_value();
        let got = block.hash();
        if !check_proof_of_work(
            got.as_bytes(),
            CompactTarget::new(block.header().bits()),
            &self.pow,
        ) {
            return Err(format!("ReadBlockFromDisk: Errors in block header at {at}"));
        }
        if got != *hash {
            // As recovery's (DEC-165(3)): the entry by its hash, not `CBlockIndex::ToString`.
            return Err(format!(
                "ReadBlockFromDisk(CBlock&, CBlockIndex*): GetHash() doesn't match index for {hash} at {at}"
            ));
        }
        Ok(Arc::new(block))
    }

    // Core: node/blockstorage.cpp:386-389, flatfile.cpp:35-46
    fn open_block_file(&self, file: i32) -> bool {
        // Core's null position opens nothing, silently.
        let Ok(file) = u32::try_from(file) else {
            return false;
        };
        self.reader.open_block_file(file).inspect_err(log_open_failed).is_ok()
    }

    fn current_usage(&self) -> u64 {
        self.files.read(BlockFiles::calculate_current_usage)
    }

    // Core: node/blockstorage.cpp:539-575 UndoReadFromDisk
    fn read_undo(&self, pos: IndexPos, parent: &BlockHash) -> Result<BlockUndo, String> {
        let at = store_pos(pos).ok_or("UndoReadFromDisk: OpenUndoFile failed")?;
        let mut buf = Vec::new();
        self.reader
            .read_undo_record(at, parent.as_bytes(), &mut buf, |u| u.to_block_undo())
            .map_err(|e| {
                if let Some(path) = e.open_failed() {
                    // Core: flatfile.cpp:45 FlatFileSeq::Open
                    tracing::info!("Unable to open file {}", path.display());
                }
                format!("UndoReadFromDisk: {e}")
            })
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::header::HeaderParams;
    use dash_params::{ChainParamsBuilder, NetworkKind, Release};
    use dashd_store::flatfile::{BlockFilesConfig, BlockFilesState, OsFs};

    use super::*;
    use crate::chain::RecordingAbort;
    use crate::store::GlobalFailStop;

    /// D62 (Opus r1 m-5): ST2's save path returns no `Corrupt` today; should one come, it is
    /// reported as an `AbortNode` with ST2's text, and the save is Core's null position.
    #[test]
    fn a_corrupt_save_is_an_abort_node_with_its_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = BlockFilesConfig::new(dir.path().join("blocks"), [0; 4]);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), cfg, BlockFilesState::default());
        let sink = Arc::new(RecordingAbort::default());
        let files = Arc::new(GuardedFiles::new(files, GlobalFailStop::new()));
        let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build();
        let params = params.expect("regtest");
        let pow = *HeaderParams::from_chain_params(&params).expect("regtest").pow();
        let node = NodeBlockFiles::new(files, pow, sink.clone());
        let e = FlatFileError::Corrupt {
            what: "block",
            pos: FlatFilePos::new(0, 8),
            detail: "a test's corruption".into(),
        };
        assert_eq!(node.save_failed(&e), None);
        assert_eq!(sink.reports(), [(e.to_string(), None)]);
    }
}
