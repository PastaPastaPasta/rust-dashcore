//! A datadir's `blk?????.dat` files read back as one chain from genesis, for the tests that
//! replay a recorded network's blocks into a [`NodeSut`](super::NodeSut) (`CH7B2_TESTNET_WINDOW`
//! and `dashd-quorums`' `QM1A5_TESTNET_WINDOW`).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_consensus::Block;
use dash_consensus::primitives::{BlockHash, decode_block, decode_header};

/// Where a located block's bytes are.
pub struct Located {
    file: PathBuf,
    offset: u64,
    size: usize,
}

impl Located {
    /// The block, decoded from its record.
    pub fn read(&self) -> Arc<Block> {
        let mut f = File::open(&self.file).unwrap();
        f.seek(SeekFrom::Start(self.offset)).unwrap();
        let mut raw = vec![0; self.size];
        f.read_exact(&mut raw).unwrap();
        let block = decode_block(&mut raw.as_slice()).expect("a block");
        Arc::new(block.into_value())
    }
}

/// The chain from genesis to the first block met at height `to`, located in `blocks`'s
/// `blk?????.dat` files, read in file order: each record's header is hashed and linked to its
/// parent, so the heights follow from genesis.
pub fn locate(
    blocks: &Path,
    magic: [u8; 4],
    genesis: BlockHash,
    to: u32,
) -> Vec<(BlockHash, Located)> {
    assert!(!blocks.join("xor.dat").exists(), "an obfuscated copy");
    let mut files: Vec<PathBuf> = std::fs::read_dir(blocks)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_str().unwrap();
            name.starts_with("blk") && name.ends_with(".dat")
        })
        .collect();
    files.sort();
    let mut height = HashMap::from([(genesis, 0_u32)]);
    let mut parent: HashMap<BlockHash, BlockHash> = HashMap::new();
    let mut at: HashMap<BlockHash, Located> = HashMap::new();
    let mut waiting: HashMap<BlockHash, Vec<BlockHash>> = HashMap::new();
    let mut target = None;
    'files: for file in files {
        let bytes = std::fs::read(&file).unwrap();
        let mut i = 0;
        while i + 8 <= bytes.len() && bytes[i..i + 4] != [0; 4] {
            assert_eq!(bytes[i..i + 4], magic, "{}@{i}: a record", file.display());
            let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
            let offset = i + 8;
            i = offset + size;
            let Some(raw) = bytes.get(offset..offset + 80) else {
                break;
            };
            let header = decode_header(&mut &raw[..]).expect("a header").into_value();
            let (hash, prev) = (header.hash(), header.prev_hash());
            let file = file.clone();
            at.insert(
                hash,
                Located {
                    file,
                    offset: offset as u64,
                    size,
                },
            );
            parent.insert(hash, prev);
            let Some(&h) = height.get(&prev) else {
                waiting.entry(prev).or_default().push(hash);
                continue;
            };
            // `hash` and every block waiting on it, at their heights.
            let mut linked = vec![(hash, h + 1)];
            while let Some((hash, h)) = linked.pop() {
                height.insert(hash, h);
                if h == to {
                    target = Some(hash);
                    break 'files;
                }
                let children = waiting.remove(&hash).unwrap_or_default();
                linked.extend(children.into_iter().map(|c| (c, h + 1)));
            }
        }
    }
    let mut chain = vec![target.unwrap_or_else(|| panic!("no block at {to}"))];
    while chain.len() <= to as usize {
        chain.push(parent[chain.last().unwrap()]);
    }
    assert_eq!(chain.last(), Some(&genesis), "linked to genesis");
    chain.reverse();
    // Genesis's own record is in the files too.
    chain.into_iter().map(|hash| (hash, at.remove(&hash).expect("located"))).collect()
}
