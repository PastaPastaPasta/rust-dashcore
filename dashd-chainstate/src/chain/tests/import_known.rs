//! The block import over the node's block files: a record the scan found is accepted at its own
//! position (`AcceptBlock`'s `dbp`, `FindBlockPos(fKnown)`), and the importer's read of a block
//! by position is `ReadBlockFromDisk(block, pos)`.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use dash_chain::FlatFilePos;
use dash_consensus::Block;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{Header, core_bytes};
use dash_num::CompactTarget;
use dash_params::ChainSetup;
use dash_primitives::BlockHeader;
use dashd_store::flatfile::{BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::support::{Mined, REGTEST_BITS, Rig, RigConfig, branch, regtest};
use crate::chain::{AbortPath, ImportKind, Imported, RecordingAbort};
use crate::store::{GlobalFailStop, GuardedFiles, NodeBlockFiles};

/// A blk file as a reindex finds it: bytes before the first record, then each block's record
/// (magic, size, block). The blocks' data positions.
fn blk(p: &ChainSetup, prefix: usize, blocks: &[&Block]) -> (Vec<u8>, Vec<FlatFilePos>) {
    let mut bytes = vec![0x5a; prefix];
    let mut at = Vec::new();
    for b in blocks {
        let raw = core_bytes(*b);
        bytes.extend_from_slice(&p.message_start);
        bytes.extend_from_slice(&u32::try_from(raw.len()).expect("small").to_le_bytes());
        at.push(FlatFilePos {
            file: 0,
            pos: u32::try_from(bytes.len()).expect("small"),
        });
        bytes.extend_from_slice(raw.as_slice());
    }
    (bytes, at)
}

struct Disk {
    _dir: tempfile::TempDir,
    blk: PathBuf,
    files: Arc<GuardedFiles<OsFs>>,
    sink: Arc<RecordingAbort>,
}

/// The node's block files over `bytes` as `blk00000.dat`, with the empty file table a reindex
/// starts from.
fn disk(p: &ChainSetup, bytes: &[u8]) -> (Disk, NodeBlockFiles<OsFs>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocks = dir.path().join("blocks");
    std::fs::DirBuilder::new().mode(0o700).create(&blocks).expect("dir");
    let blk = blocks.join("blk00000.dat");
    std::fs::write(&blk, bytes).expect("blk");
    let latch = GlobalFailStop::new();
    let cfg = BlockFilesConfig::new(blocks, p.message_start);
    let store = StoreFiles::with_fs(Arc::new(OsFs::new()), cfg, BlockFilesState::default());
    let files = Arc::new(GuardedFiles::new(store, latch.clone()));
    let sink = Arc::new(RecordingAbort::default());
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let node =
        NodeBlockFiles::new(files.clone(), pow, Arc::new(AbortPath::new(sink.clone(), latch)));
    let d = Disk {
        _dir: dir,
        blk,
        files,
        sink,
    };
    (d, node)
}

/// Each record past a prefix that is no record is indexed at its own data position, nothing is
/// written, and the file's used size is Core's `max(nPos + nAddSize, nSize)` over the records,
/// with `nAddSize` the block's size alone, as the position is past the record's header
/// (`node/blockstorage.cpp:709`, `:854-866` at v24.0.0).
#[test]
fn a_record_is_accepted_at_its_scanned_position() {
    let p = regtest();
    let g = Mined::genesis(&p);
    let a = branch(&p, &g, 2, 0);
    let blocks: Vec<&Block> = [&g].into_iter().chain(&a).map(|m| &*m.block).collect();
    let (bytes, at) = blk(&p, 13, &blocks);
    let (d, files) = disk(&p, &bytes);
    let mut rig = Rig::with(RigConfig {
        reindex: true,
        files: Some(Box::new(files)),
        ..RigConfig::default()
    });
    for (i, m) in [&g].into_iter().chain(&a).enumerate() {
        let imported = rig.v.import_block(m.block.clone(), Some(at[i]), ImportKind::Record).recv();
        assert_eq!(imported, Ok(Ok(Imported::Accepted)), "block {i}");
        if i == 0 {
            assert_eq!(rig.v.activate_best_chain().recv(), Ok(Ok(())));
        }
    }
    assert_eq!(rig.v.activate_best_chain().recv(), Ok(Ok(())));
    rig.assert_check_block_index();
    for (i, m) in [&g].into_iter().chain(&a).enumerate() {
        let e = rig.v.reader().entry(rig.id(m)).expect("indexed");
        assert_eq!((e.file, e.data_pos), (Some(0), at[i].pos), "block {i}");
    }
    assert_eq!(std::fs::read(&d.blk).expect("blk"), bytes, "nothing written");
    let last = core_bytes(blocks[2]).len() as u32;
    let size = d.files.read(|f| f.file_info(0).map(|i| (i.size, i.blocks)));
    assert_eq!(size, Some((at[2].pos + last, 3)));
    assert!(d.sink.messages().is_empty());
    assert!(rig.stop().fatal.is_none());
}

/// `g`'s block with a nonce its proof of work fails.
fn unworked(p: &ChainSetup, g: &Mined) -> Block {
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let h = g.block.header();
    let header = (0..)
        .map(|nonce| {
            let raw = BlockHeader::new(
                h.version(),
                h.prev_hash(),
                h.merkle_root(),
                h.time(),
                REGTEST_BITS,
                nonce,
            );
            Header::new(raw)
        })
        .find(|h| !check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow))
        .expect("a nonce");
    Block::new(header, g.block.txs().to_vec())
}

/// The importer's read by position: the block, or Core's line for a file that does not open,
/// bytes that do not deserialize, or a header that fails its proof of work.
#[test]
fn a_read_by_position_is_read_block_from_disk() {
    let p = regtest();
    let g = Mined::genesis(&p);
    let bad = unworked(&p, &g);
    let (bytes, at) = blk(&p, 0, &[&g.block, &bad]);
    let (_d, files) = disk(&p, &bytes);
    let read = files.block_at_pos();
    assert_eq!(read.read_block_at(at[0]).map(|b| b.hash()), Ok(g.hash()));
    let header = read.read_block_at(at[1]).map(|b| b.hash());
    let line = format!(
        "ReadBlockFromDisk: Errors in block header at FlatFilePos(nFile=0, nPos={})",
        at[1].pos
    );
    assert_eq!(header, Err(line));
    let missing = FlatFilePos {
        file: 3,
        pos: 8,
    };
    let open = read.read_block_at(missing).map(|b| b.hash());
    assert_eq!(
        open,
        Err("ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=3, nPos=8)".into())
    );
    let end = FlatFilePos {
        file: 0,
        pos: u32::try_from(bytes.len()).expect("small"),
    };
    let decode = read.read_block_at(end).map(|b| b.hash()).expect_err("past the end");
    assert!(decode.starts_with("ReadBlockFromDisk: Deserialize or I/O error - "), "{decode}");
    assert!(decode.ends_with(&format!(" at FlatFilePos(nFile=0, nPos={})", end.pos)), "{decode}");
}
