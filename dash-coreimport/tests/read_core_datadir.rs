//! The read path on a real node's block files: every block and undo record of a Dash Core
//! `blocks/` directory is read back through `dashd_store::flatfile::BlockFileReader` (read-only
//! handles, `pread` at the record's data position) and checked:
//! - a block read equals the record's bytes and decodes as a block;
//! - an undo read passes Core's checksum against the parent hash and decodes as a `CBlockUndo`.
//!
//! Undo records carry no block hash: each is matched to a block of the same-numbered blk file by
//! its checksum (rev files are in connection order, so blocks are tried in chain order first).
//!
//! `ST2_READ_DIRS` lists `network=blocks_dir` pairs separated by `:` (network is `mainnet`,
//! `testnet` or `regtest`). Nothing is ever opened for writing. A live node's newest files may end
//! in a torn record, which the scan stops at, and each rev file is read before its blk file.

use std::collections::HashMap;
use std::path::Path;

use dash_coreimport::Hash256;
use dash_coreimport::blockfiles::{
    UNDO_CHECKSUM_LEN, list_files, scan_block_records, scan_undo_candidates, undo_checksum,
};
use dash_coreimport::coin::read_block_undo;
use dash_coreimport::tx::{block_hash, read_block};
use dashd_store::flatfile::{BlockFileReader, FlatFilePos};

#[derive(Debug, Default)]
struct Totals {
    files: usize,
    blocks: usize,
    undo: usize,
    unmatched_undo: usize,
    bytes: u64,
}

fn check_dir(dir: &Path, magic: [u8; 4]) -> Totals {
    let reader = BlockFileReader::open_read_only(dir);
    let mut t = Totals::default();
    let revs: HashMap<u32, _> = list_files(dir, "rev").unwrap().into_iter().collect();
    let mut buf = Vec::new();
    let start = std::time::Instant::now();
    for (n, path) in list_files(dir, "blk").unwrap() {
        t.files += 1;
        // A live node appends a block before its undo: reading the rev file first means every
        // undo record read has its block in the blk file read after it.
        let rev = revs.get(&n).map(|p| std::fs::read(p).unwrap());
        let data = std::fs::read(&path).unwrap();
        t.bytes += data.len() as u64;
        let (records, _) = scan_block_records(&data, magic);
        // (hash, prev) of every block, in file order.
        let mut blocks: Vec<(Hash256, Hash256)> = Vec::with_capacity(records.len());
        for r in &records {
            let bytes = &data[r.offset..r.offset + r.len];
            reader.read_block(FlatFilePos::new(n, r.offset as u32), &mut buf).unwrap();
            assert_eq!(buf, bytes, "blk{n:05} at {}", r.offset);
            read_block(&buf).unwrap_or_else(|e| panic!("blk{n:05} at {}: {e}", r.offset));
            blocks.push((block_hash(&buf[..80]), bytes[4..36].try_into().unwrap()));
            t.blocks += 1;
        }
        let Some(rev) = rev else {
            continue;
        };
        t.bytes += rev.len() as u64;
        let (candidates, _) = scan_undo_candidates(&rev, magic);
        let order = chain_order(&blocks);
        let mut cursor = 0;
        // End of the last record accepted: candidates inside it are magics in undo bytes.
        let mut covered = 0;
        for r in candidates {
            if r.offset - 8 < covered {
                continue;
            }
            let undo = &rev[r.offset..r.offset + r.len];
            let stored = &rev[r.offset + r.len..r.offset + r.len + UNDO_CHECKSUM_LEN];
            // Try the blocks after the previous match first, then all of them.
            let found = (cursor..order.len())
                .chain(0..cursor)
                .find(|&k| undo_checksum(&blocks[order[k]].1, undo) == stored);
            let Some(k) = found else {
                t.unmatched_undo += 1;
                continue;
            };
            cursor = k + 1;
            covered = r.offset + r.len + UNDO_CHECKSUM_LEN;
            let prev = &blocks[order[k]].1;
            reader.read_undo(FlatFilePos::new(n, r.offset as u32), prev, &mut buf).unwrap();
            assert_eq!(buf, undo);
            read_block_undo(&buf).unwrap_or_else(|e| panic!("rev{n:05} at {}: {e}", r.offset));
            t.undo += 1;
        }
        eprintln!("  {} file {n} ({:.0}s): {t:?}", dir.display(), start.elapsed().as_secs_f64());
    }
    t
}

/// The file's blocks along their prev links (connection order, which rev files follow): each
/// chain segment starts at a block whose parent is in another file; segments in file order.
fn chain_order(blocks: &[(Hash256, Hash256)]) -> Vec<usize> {
    let in_file: HashMap<Hash256, usize> =
        blocks.iter().enumerate().map(|(i, b)| (b.0, i)).collect();
    let mut children: HashMap<Hash256, Vec<usize>> = HashMap::new();
    for (i, b) in blocks.iter().enumerate() {
        children.entry(b.1).or_default().push(i);
    }
    let mut order = Vec::with_capacity(blocks.len());
    let mut seen = vec![false; blocks.len()];
    for (i, b) in blocks.iter().enumerate() {
        if in_file.contains_key(&b.1) {
            continue;
        }
        let mut stack = vec![i];
        while let Some(j) = stack.pop() {
            if std::mem::replace(&mut seen[j], true) {
                continue;
            }
            order.push(j);
            if let Some(c) = children.get(&blocks[j].0) {
                stack.extend(c.iter().rev());
            }
        }
    }
    // Anything unreachable (duplicate copies of a block) goes last.
    order.extend((0..blocks.len()).filter(|&i| !seen[i]));
    order
}

#[test]
fn reads_every_record_of_a_core_datadir() {
    let Ok(dirs) = std::env::var("ST2_READ_DIRS") else {
        eprintln!("skipped: set ST2_READ_DIRS=network=blocks_dir[:...]");
        return;
    };
    for entry in dirs.split(':').filter(|e| !e.is_empty()) {
        let (net, dir) = entry.split_once('=').expect("network=dir");
        let network = match net {
            "mainnet" => dash_network::Network::Mainnet,
            "testnet" => dash_network::Network::Testnet,
            "regtest" => dash_network::Network::Regtest,
            _ => panic!("unknown network {net}"),
        };
        let t = check_dir(Path::new(dir), network.magic().to_le_bytes());
        eprintln!("{net} {dir}: {t:?}");
        assert!(t.blocks > 0);
        // Every undo record belongs to a block of its file.
        assert_eq!(t.unmatched_undo, 0, "{net}: undo records without a block");
    }
}
