//! Byte-for-byte parity of `dashd_store::flatfile::BlockFiles` with Dash Core's blk/rev files.
//!
//! Each case is a regtest blocks directory written by a real dashd (see
//! `scripts/gen-blockfile-chains.py` for how they are produced). The test rebuilds the same blk
//! and rev files from scratch: it takes the best chain's blocks, recomputes every block's undo data
//! by replaying the chain against an in-memory UTXO set (Core's undo bytes are never read), feeds
//! blocks and undo records to `BlockFiles` in the order the case's dashd did, and compares every
//! `blk?????.dat` and `rev?????.dat` (contents and lengths: record positions, rollover points,
//! finalize truncation and preallocated tails).
//!
//! Cases:
//! - `DASHD_TEST_DATA` (from `contrib/setup-dashd.py`): the `regtest-40000` dataset;
//! - `ST2_PARITY_DATADIRS`: colon-separated datadirs written by `gen-blockfile-chains.py`, each with
//!   its `st2-manifest.json`.
//!
//! Without either variable the test only reports that it found nothing to compare. `ST2_SCRATCH`
//! sets where the rebuilt files go (default: cargo's target tmp dir).

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{
    REGTEST_MAGIC, block_time, compare_dirs, core_bytes, is_v24, prev_hash, undo_for_tree,
};
use dash_coreimport::blockfiles::{list_files, scan_block_records};
use dash_coreimport::tx::block_hash;
use dash_coreimport::{Hash256, hash_from_hex};
use dashd_store::flatfile::{
    BlockFiles, BlockFilesConfig, BlockFilesState, CoreBytes, FlatFilePos, MIN_BLOCKS_TO_KEEP, OsFs,
};

/// The order in which a dashd saved blocks and wrote undo records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pattern {
    /// Each block is saved, then connected (`generatetoaddress`, `submitblock`).
    SaveThenConnect,
    /// `-loadblock`: every block is saved, then the chain is connected.
    SaveAllThenConnect,
    /// `-reindex`: every block is re-registered at its existing position (in file order), then
    /// the chain is connected; rev files are rewritten from scratch.
    Reindex,
}

struct Case {
    name: String,
    blocks_dir: PathBuf,
    fastprune: bool,
    tinyblk: bool,
    pattern: Pattern,
    /// Blocks before which dashd was restarted.
    restarts_before: Vec<u32>,
    /// The reindexed datadir's blocks directory (its blk files are the input of a reindex).
    reindex_source: Option<PathBuf>,
    best: Option<Hash256>,
    /// `-prune`: the height `pruneblockchain` was called with at the end, what it returned (the
    /// first block still stored, minus one), and the export the replay read (pruned blocks are
    /// gone from the case's own files).
    prune: Option<(u32, u32, PathBuf)>,
}

/// The case a manifest describes; `None` for a datadir a release other than v24 wrote (DEC-82).
fn manifest_case(datadir: &Path) -> Option<Case> {
    let text = std::fs::read_to_string(datadir.join("st2-manifest.json")).unwrap();
    let m: serde_json::Value = serde_json::from_str(&text).unwrap();
    let flags: Vec<String> =
        m["flags"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_owned()).collect();
    let dashd = m["dashd"].as_str().unwrap();
    if !is_v24(dashd) {
        eprintln!("skipped {}: written by {dashd}, not v24", datadir.display());
        return None;
    }
    let pattern = match m["kind"].as_str().unwrap() {
        "generate" | "replay" => Pattern::SaveThenConnect,
        "loadblock" => Pattern::SaveAllThenConnect,
        "reindex" => Pattern::Reindex,
        k => panic!("unknown manifest kind {k}"),
    };
    Some(Case {
        name: datadir.file_name().unwrap().to_string_lossy().into_owned(),
        blocks_dir: datadir.join("regtest/blocks"),
        fastprune: flags.iter().any(|f| f == "-fastprune"),
        tinyblk: flags.iter().any(|f| f == "-tinyblk"),
        pattern,
        restarts_before: m["restarts_before"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h.as_u64().unwrap() as u32)
            .collect(),
        reindex_source: (pattern == Pattern::Reindex)
            .then(|| PathBuf::from(m["source"].as_str().unwrap()).join("regtest/blocks")),
        best: Some(hash_from_hex(m["best"].as_str().unwrap()).unwrap()),
        prune: m["prune"].as_object().map(|p| {
            let source = PathBuf::from(m["source"].as_str().unwrap());
            (p["requested"].as_u64().unwrap() as u32, p["result"].as_u64().unwrap() as u32, source)
        }),
    })
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    if let Ok(data) = std::env::var("DASHD_TEST_DATA") {
        // The dataset was mined block by block and has a single blk/rev pair, so no rollover
        // reaches the rev-file finalize rule.
        out.push(Case {
            name: "regtest-40000".into(),
            blocks_dir: Path::new(&data).join("regtest-40000/regtest/blocks"),
            fastprune: false,
            tinyblk: false,
            pattern: Pattern::SaveThenConnect,
            restarts_before: Vec::new(),
            reindex_source: None,
            best: None,
            prune: None,
        });
    }
    if let Ok(dirs) = std::env::var("ST2_PARITY_DATADIRS") {
        out.extend(
            dirs.split(':').filter(|d| !d.is_empty()).filter_map(|d| manifest_case(Path::new(d))),
        );
    }
    out
}

/// A block as stored in Core's files.
struct StoredBlock {
    hash: Hash256,
    prev: Hash256,
    bytes: CoreBytes,
    /// Data position in the source files.
    pos: FlatFilePos,
}

/// Every block record of a blocks directory, in file order.
fn stored_blocks(dir: &Path) -> Vec<StoredBlock> {
    let mut out = Vec::new();
    for (n, path) in list_files(dir, "blk").unwrap() {
        let data = std::fs::read(&path).unwrap();
        let (records, stats) = scan_block_records(&data, REGTEST_MAGIC);
        assert_eq!(stats.resync_bytes, 0, "{}: garbage between records", path.display());
        for r in records {
            let bytes = core_bytes(&data[r.offset..r.offset + r.len]);
            out.push(StoredBlock {
                hash: block_hash(&bytes.as_slice()[..80]),
                prev: prev_hash(bytes.as_slice()),
                pos: FlatFilePos::new(n, r.offset as u32),
                bytes,
            });
        }
    }
    out
}

/// The blocks of an export (`gen-blockfile-chains.py export`: `u32 LE length ‖ block`), up to and
/// including `height`, as a replay saved them (height order).
fn exported_blocks(path: &Path, height: u32) -> Vec<StoredBlock> {
    let data = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() && out.len() <= height as usize {
        let len = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let bytes = core_bytes(&data[at + 4..at + 4 + len]);
        out.push(StoredBlock {
            hash: block_hash(&bytes.as_slice()[..80]),
            prev: prev_hash(bytes.as_slice()),
            pos: FlatFilePos::default(),
            bytes,
        });
        at += 4 + len;
    }
    out
}

/// The best chain from genesis, as indices into `blocks` (index = height). Without a known tip
/// (the dataset), the last block stored is the tip and the chain must cover every block.
fn best_chain(blocks: &[StoredBlock], best: Option<Hash256>) -> Vec<usize> {
    let by_hash: HashMap<Hash256, usize> =
        blocks.iter().enumerate().map(|(i, b)| (b.hash, i)).collect();
    let mut chain = Vec::new();
    let mut cur = best.unwrap_or_else(|| blocks.last().unwrap().hash);
    loop {
        let i = by_hash[&cur];
        chain.push(i);
        if blocks[i].prev == [0; 32] {
            break;
        }
        cur = blocks[i].prev;
    }
    chain.reverse();
    if best.is_none() {
        assert_eq!(chain.len(), blocks.len(), "the dataset holds blocks off its best chain");
    }
    chain
}

/// What a rebuild did, for the report.
struct Rebuilt {
    blocks: usize,
    side_blocks: usize,
    pruned_files: usize,
}

/// Replays the case into `out_dir` with `BlockFiles`. Every durability point Core's
/// `FlushStateToDisk` would flush the block files at (a restart, the prune, shutdown) is
/// `BlockFiles`' own barrier; nothing calls `flush_block_file` directly.
fn rebuild(case: &Case, out_dir: &Path) -> Rebuilt {
    // Blocks in the order dashd saved them: file order, or height order for a pruned replay.
    let blocks = match &case.prune {
        Some((_, _, source)) => {
            let height = case_height(case);
            exported_blocks(source, height)
        }
        None => stored_blocks(case.reindex_source.as_deref().unwrap_or(&case.blocks_dir)),
    };
    let chain = best_chain(&blocks, case.best);
    let on_chain: std::collections::HashSet<usize> = chain.iter().copied().collect();
    let side: Vec<usize> = (0..blocks.len()).filter(|i| !on_chain.contains(i)).collect();
    let (undo, side_undo) = undo_for_tree(
        &chain.iter().map(|&i| blocks[i].bytes.as_slice()).collect::<Vec<_>>(),
        &side.iter().map(|&i| blocks[i].bytes.as_slice()).collect::<Vec<_>>(),
    );
    let height_of: HashMap<usize, u32> =
        chain.iter().enumerate().map(|(h, &i)| (i, h as u32)).collect();
    let mut cfg = BlockFilesConfig::new(out_dir, REGTEST_MAGIC);
    cfg.fastprune = case.fastprune;
    cfg.tinyblk = case.tinyblk;
    cfg.prune_mode = case.prune.is_some();
    let fs = Arc::new(OsFs::new());
    let mut bf = BlockFiles::with_fs(Arc::clone(&fs), cfg.clone(), BlockFilesState::default());
    let mut undo_file = vec![0u32; chain.len()];

    match case.pattern {
        Pattern::SaveThenConnect => {
            // Each block is connected right after it is saved: on a fork (a reorg's competing
            // branch) as on the best chain. Reconnecting the original branch writes nothing.
            for (i, b) in blocks.iter().enumerate() {
                let (h, block_undo) = match height_of.get(&i) {
                    Some(&h) => (h, &undo[h as usize]),
                    None => {
                        let (h, u) = &side_undo[&b.hash];
                        (*h, u)
                    }
                };
                if on_chain.contains(&i) && case.restarts_before.contains(&h) {
                    // Shutdown and startup: the barrier, then what the index kept.
                    let state = bf.sync_and_state().unwrap();
                    bf = BlockFiles::with_fs(Arc::clone(&fs), cfg.clone(), state);
                }
                let pos = bf.save_block(&b.bytes, h, block_time(b.bytes.as_slice()), None).unwrap();
                if h > 0 {
                    bf.write_undo(block_undo, pos.file, h, &b.prev).unwrap();
                }
            }
        }
        Pattern::SaveAllThenConnect => {
            for (height, &i) in chain.iter().enumerate() {
                let b = &blocks[i];
                // Only genesis is connected while the import runs.
                let time = block_time(b.bytes.as_slice());
                let pos = bf.save_block(&b.bytes, height as u32, time, None).unwrap();
                undo_file[height] = pos.file;
            }
        }
        Pattern::Reindex => {
            // The blk files are reused in place: copy them, then register every stored block.
            std::fs::create_dir_all(out_dir).unwrap();
            for (_, path) in list_files(case.reindex_source.as_ref().unwrap(), "blk").unwrap() {
                std::fs::copy(&path, out_dir.join(path.file_name().unwrap())).unwrap();
            }
            for (i, b) in blocks.iter().enumerate() {
                let h = height_of[&i];
                let time = block_time(b.bytes.as_slice());
                bf.save_block(&b.bytes, h, time, Some(b.pos)).unwrap();
                undo_file[h as usize] = b.pos.file;
            }
        }
    }
    if case.pattern != Pattern::SaveThenConnect {
        // ActivateBestChain after the import: connect the chain in height order.
        for (height, &i) in chain.iter().enumerate().skip(1) {
            bf.write_undo(&undo[height], undo_file[height], height as u32, &blocks[i].prev)
                .unwrap();
        }
    }
    let mut pruned_files = 0;
    if let Some((requested, result, _)) = case.prune {
        // pruneblockchain: clamped to tip - MIN_BLOCKS_TO_KEEP, then FlushStateToDisk with the
        // manual height (no prune locks without indexes): select, flush, write the index, unlink.
        let tip = chain.len() as u32 - 1;
        let manual = requested.min(tip - MIN_BLOCKS_TO_KEEP);
        let files = bf.find_files_to_prune_manual(manual.min(tip), Some(tip));
        bf.sync_and_take_dirty_infos().unwrap();
        bf.unlink_pruned_files(&files).unwrap();
        pruned_files = files.len();
        // Core's answer: GetFirstStoredBlock(tip)->nHeight - 1.
        let first_stored = bf.infos().iter().filter(|i| i.size > 0).map(|i| i.height_first).min();
        assert_eq!(
            first_stored,
            Some(result + 1),
            "{}: pruneblockchain returned {result}",
            case.name
        );
    }
    // Shutdown: FlushStateToDisk(ALWAYS).
    bf.sync().unwrap();
    Rebuilt {
        blocks: chain.len(),
        side_blocks: side.len(),
        pruned_files,
    }
}

fn case_height(case: &Case) -> u32 {
    let text = std::fs::read_to_string(case.blocks_dir.join("../../st2-manifest.json")).unwrap();
    let m: serde_json::Value = serde_json::from_str(&text).unwrap();
    m["height"].as_u64().unwrap() as u32
}

#[test]
fn block_files_are_byte_identical_to_core() {
    let cases = cases();
    if cases.is_empty() {
        eprintln!("no parity cases: set DASHD_TEST_DATA and/or ST2_PARITY_DATADIRS");
        return;
    }
    let scratch = std::env::var("ST2_SCRATCH")
        .map_or_else(|_| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    let mut failures = Vec::new();
    for case in &cases {
        let out = tempfile::Builder::new().prefix("st2-parity-").tempdir_in(&scratch).unwrap();
        let ours = out.path().join("blocks");
        let r = rebuild(case, &ours);
        match compare_dirs(&case.blocks_dir, &ours) {
            Ok(files) => eprintln!(
                "{}: {} blocks (+{} off the best chain), {files} blk/rev files byte-identical, {} pruned ({:?}, fastprune={}, tinyblk={}, {} restarts)",
                case.name,
                r.blocks,
                r.side_blocks,
                r.pruned_files,
                case.pattern,
                case.fastprune,
                case.tinyblk,
                case.restarts_before.len()
            ),
            Err(e) => failures.push(format!("{}: {e}", case.name)),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
