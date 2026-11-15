//! Extracts the churn trace from real mainnet block and undo files (tests/data/mainnet-0-1300,
//! see its PROVENANCE.md) and checks it against the reference node's UTXO statistics.

use std::path::{Path, PathBuf};

use dash_coreimport::blockfiles::{BlockReader, HeaderIndex};
use dash_coreimport::hash_from_hex;
use dash_coreimport::trace::{Event, ExtractConfig, TraceBlock, TraceReader, extract};

const ANCHOR: &str = "000000aab01a883a7689a567b7a912399f09ec59e967d8eb84ad356c7a4dc51f";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/mainnet-0-1300")
}

fn magic() -> [u8; 4] {
    dash_network::Network::Mainnet.magic().to_le_bytes()
}

fn config(anchor: [u8; 32], output: &Path) -> ExtractConfig {
    config_in(&fixture(), anchor, output)
}

fn config_in(blocks_dir: &Path, anchor: [u8; 32], output: &Path) -> ExtractConfig {
    ExtractConfig {
        blocks_dir: blocks_dir.to_path_buf(),
        network_magic: magic(),
        anchor,
        output: output.to_path_buf(),
        rev_cache_files: 1,
    }
}

#[test]
fn mainnet_0_1300_matches_the_reference_node() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("trace");
    let report = extract(&config(hash_from_hex(ANCHOR).unwrap(), &output), |_, _| {}).unwrap();
    assert_eq!(report.last_height, 1300);
    // `gettxoutsetinfo none 1300` on the reference node: txouts = 1269.
    assert_eq!(report.live_coins, 1269);
    assert_eq!(report.stale_undo_skipped, 1);
    // Every spend was checked against Core's undo bytes.
    assert_eq!(report.checked_spent_kinds.values().sum::<u64>(), report.spends);
    assert!(report.spends > 0);

    // The trace replays to the same totals.
    let mut reader = TraceReader::open(&output).unwrap();
    assert_eq!(reader.header.last_height, 1300);
    let (mut block, mut live, mut heights) = (TraceBlock::default(), 0i64, 0);
    while reader.next_block(&mut block).unwrap() {
        heights += 1;
        assert_eq!(block.height, heights);
        for e in &block.events {
            live += match e {
                Event::Spend(_) => -1,
                Event::Create {
                    ..
                } => 1,
            };
        }
    }
    assert_eq!((heights, live), (1300, 1269));
}

#[test]
fn an_anchor_outside_the_files_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(extract(&config([7; 32], &dir.path().join("trace")), |_, _| {}).is_err());
}

/// A crash can leave an older copy of a block whose header still hashes to the block but whose
/// body was overwritten. When that copy comes first in the files, extraction must use a later
/// intact copy (its merkle root matches), not the damaged one.
#[test]
fn a_damaged_first_copy_falls_back_to_an_intact_one() {
    let dir = tempfile::tempdir().unwrap();
    let blocks = dir.path().join("blocks");
    std::fs::create_dir(&blocks).unwrap();
    for name in ["blk00000.dat", "rev00000.dat"] {
        std::fs::copy(fixture().join(name), blocks.join(name)).unwrap();
    }
    // Block 1000's bytes with its last byte (a lock time) changed: it parses, the header still
    // hashes to block 1000, the merkle root no longer matches.
    let index = HeaderIndex::build(&blocks, magic()).unwrap();
    let chain = index.chain_to(&hash_from_hex(ANCHOR).unwrap()).unwrap();
    let mut damaged = Vec::new();
    BlockReader::new(&index.files).read(&chain[1000].1, &mut damaged).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    let mut file = magic().to_vec();
    file.extend((damaged.len() as u32).to_le_bytes());
    file.extend(&damaged);
    file.extend(std::fs::read(blocks.join("blk00000.dat")).unwrap());
    std::fs::write(blocks.join("blk00000.dat"), file).unwrap();
    let index = HeaderIndex::build(&blocks, magic()).unwrap();
    assert_eq!(index.copies(&chain[1000].0).count(), 2, "the damaged copy comes first");

    let anchor = hash_from_hex(ANCHOR).unwrap();
    let (output, reference) = (dir.path().join("trace"), dir.path().join("reference"));
    let report = extract(&config_in(&blocks, anchor, &output), |_, _| {}).unwrap();
    assert_eq!((report.last_height, report.live_coins), (1300, 1269));
    extract(&config(anchor, &reference), |_, _| {}).unwrap();
    assert!(std::fs::read(output).unwrap() == std::fs::read(reference).unwrap(), "same trace");
}
