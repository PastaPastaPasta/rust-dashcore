//! Loaders for undo records Dash Core wrote, shared by `dash-consensus`'s record tests and
//! `node-l1`'s oracle comparisons (CS5a). Provenance: `tests/data/PROVENANCE.md` (`coins/`) and
//! `dash-coreimport/tests/data/mainnet-0-1300/PROVENANCE.md`.

#![allow(dead_code)]

use std::path::PathBuf;

use dash_core_vectors::hex::parse_hex;
use serde::Deserialize;

/// One spent coin as Core's `getblock <hash> 3` reports it (from the same undo record).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prevout {
    pub height: u32,
    pub coinbase: bool,
    pub value: i64,
    pub script_pubkey: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockJson {
    height: u32,
    hash: String,
    prev_hash: String,
    block: String,
    undo: String,
    checksum: String,
    prevouts: Vec<Vec<Prevout>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegtestJson {
    core: String,
    dashd_sha256: String,
    chain: String,
    script_kinds: Vec<String>,
    blocks: Vec<BlockJson>,
}

/// A block, its undo record, the record's rev-file checksum and Core's view of its prevouts.
pub struct UndoVector {
    pub height: u32,
    /// In Core's internal byte order (`uint256`).
    pub prev_hash: [u8; 32],
    pub block: Vec<u8>,
    pub undo: Vec<u8>,
    pub checksum: [u8; 32],
    /// Per transaction after the coinbase, per input.
    pub prevouts: Vec<Vec<Prevout>>,
}

/// A display-order hash in Core's internal order.
pub fn internal_hash(display: &str) -> [u8; 32] {
    let mut bytes: [u8; 32] =
        parse_hex(display).try_into().unwrap_or_else(|_| panic!("{display}: not a 32-byte hash"));
    bytes.reverse();
    bytes
}

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dash-consensus/tests/data")
}

/// The regtest chain recorded with release `pin` (`v24.0`) by
/// `contrib/node/coins-undo-vectors.py`.
pub fn regtest(pin: &str) -> Vec<UndoVector> {
    let path = data_dir().join(format!("coins/regtest-undo-{pin}.json"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{} is missing", path.display()));
    let parsed: RegtestJson = serde_json::from_str(&text)
        .unwrap_or_else(|_| panic!("{}: not a coins-undo-vectors.py file", path.display()));
    parsed
        .blocks
        .into_iter()
        .map(|b| UndoVector {
            height: b.height,
            prev_hash: internal_hash(&b.prev_hash),
            block: parse_hex(&b.block),
            undo: parse_hex(&b.undo),
            checksum: parse_hex(&b.checksum)
                .try_into()
                .unwrap_or_else(|_| panic!("height {}: not a 32-byte checksum", b.height)),
            prevouts: b.prevouts,
        })
        .collect()
}

/// The records of a Core block or undo file: `magic ‖ u32 size ‖ payload`, then for undo files
/// a 32-byte checksum. Returns each payload and, for undo files, its checksum.
pub fn file_records(bytes: &[u8], checksums: bool) -> Vec<(Vec<u8>, [u8; 32])> {
    let magic = &bytes[..4];
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 8 <= bytes.len() && &bytes[pos..pos + 4] == magic {
        let size =
            u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]])
                as usize;
        let payload = bytes[pos + 8..pos + 8 + size].to_vec();
        pos += 8 + size;
        let mut checksum = [0u8; 32];
        if checksums {
            checksum.copy_from_slice(&bytes[pos..pos + 32]);
            pos += 32;
        }
        out.push((payload, checksum));
    }
    assert_eq!(pos, bytes.len(), "trailing bytes after the last record");
    out
}

fn mainnet_file(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../dash-coreimport/tests/data/mainnet-0-1300")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|_| panic!("{} is missing", path.display()))
}

/// Mainnet blocks 0–1300 in height order, from the reference oracle's block files.
pub fn mainnet_blocks() -> Vec<Vec<u8>> {
    file_records(&mainnet_file("blk00000.dat"), false).into_iter().map(|(b, _)| b).collect()
}

/// The undo records of mainnet blocks 1–1300 with their checksums, in file order (one record
/// appears twice; see the fixture's provenance).
pub fn mainnet_undo_records() -> Vec<(Vec<u8>, [u8; 32])> {
    file_records(&mainnet_file("rev00000.dat"), true)
}
