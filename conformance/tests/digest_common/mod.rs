//! What the L2 and memory tests share to build regtest blocks of their own (CH3a a7-2).

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use dash_conformance::digest::store::{Node, Vectors};
use dash_consensus::Block;
use dash_consensus::primitives::{BlockHeader, Header, Tx, decode_tx};

/// Regtest's PoW limit, which every synthetic header meets.
const REGTEST_BITS: u32 = 0x207f_ffff;

/// A temporary directory with mode `0700`.
pub fn tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// The stored node `label` (`test/node<i>`) of the regtest v24 set.
pub fn stored(label: &str) -> Node {
    let (test, name) = label.split_once('/').unwrap();
    let nodes = Vectors::regtest_v24().nodes(test).unwrap();
    nodes.into_iter().find(|n| n.name == name).unwrap_or_else(|| panic!("no node {label}"))
}

/// `CScript() << height`, as BIP34 wants it first in the coinbase's scriptSig.
fn height_push(n: u32) -> Vec<u8> {
    match n {
        1..=16 => vec![0x50 + n as u8],
        _ => {
            let mut num = n.to_le_bytes().to_vec();
            while num.last() == Some(&0) {
                num.pop();
            }
            if num.last().is_some_and(|b| b & 0x80 != 0) {
                num.push(0);
            }
            let mut out = vec![num.len() as u8];
            out.extend(num);
            out
        }
    }
}

/// The coinbase scriptSig at `height`: the height and a 4-byte push (Core wants 2..=100 bytes).
pub fn coinbase_sig(height: u32) -> Vec<u8> {
    let mut sig = height_push(height);
    sig.push(4);
    sig.extend(height.to_le_bytes());
    sig
}

/// The transaction serialized in `raw`, exactly.
pub fn decode(raw: &[u8]) -> Arc<Tx> {
    let mut cursor = raw;
    let tx = decode_tx(&mut cursor).unwrap().into_value();
    assert!(cursor.is_empty());
    Arc::new(tx)
}

/// A block on `prev` holding `txs`, 150 s after it, mined under regtest's limit.
pub fn mine(prev: &Block, txs: Vec<Arc<Tx>>) -> Arc<Block> {
    mine_at(prev, txs, prev.header().time() + 150)
}

/// A block on `prev` holding `txs` at `time`, mined under regtest's limit: the hash's top byte
/// below `0x7f` is under `0x207fffff`'s target.
pub fn mine_at(prev: &Block, txs: Vec<Arc<Tx>>, time: u32) -> Arc<Block> {
    let mut header = BlockHeader {
        version: 0x2000_0000,
        prev_hash: prev.hash(),
        time,
        bits: REGTEST_BITS,
        ..BlockHeader::default()
    };
    header.merkle_root = Block::new(Header::new(header), txs.clone()).merkle().0;
    let header = (0..)
        .map(|nonce| {
            Header::new(BlockHeader {
                nonce,
                ..header
            })
        })
        .find(|h| h.hash().as_bytes()[31] < 0x7f)
        .unwrap();
    Arc::new(Block::new(header, txs))
}
