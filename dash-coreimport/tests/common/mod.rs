//! Helpers shared by the block-file parity tests.

// Each test binary uses its own subset.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dash_coreimport::Hash256;
use dash_coreimport::blockfiles::list_files;
use dash_coreimport::coin::{coin_bytes, is_unspendable, write_block_undo};
use dash_coreimport::tx::{OutPoint, block_hash, read_block};
use dashd_store::flatfile::{CoreBytes, CoreUndoBytes};

pub const REGTEST_MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// A block read from Core's files, as the writer takes it: decoded by CS0, whose `CoreBytes` must
/// equal the file's bytes (the record holds exactly Core's serialization of the block).
pub fn core_bytes(block: &[u8]) -> CoreBytes {
    let mut input = block;
    let decoded = dash_consensus::decode_block(&mut input).expect("CS0 decodes Core's block");
    assert!(input.is_empty(), "{} bytes after the block", input.len());
    let bytes = decoded.core_bytes();
    assert_eq!(bytes.as_slice(), block, "CS0's serialization differs from Core's file");
    bytes
}

/// A block's `nTime`.
pub fn block_time(block: &[u8]) -> u64 {
    u64::from(u32::from_le_bytes(block[68..72].try_into().unwrap()))
}

/// A block's `hashPrevBlock`.
pub fn prev_hash(block: &[u8]) -> Hash256 {
    block[4..36].try_into().unwrap()
}

/// Whether a `dashd -version` line names a v24 release: the only line `BlockFiles` reproduces
/// (DEC-82). The rev-file finalize rule of v23.1.8 differs.
pub fn is_v24(version: &str) -> bool {
    version.contains("v24.")
}

type Utxo = HashMap<OutPoint, Vec<u8>>;

/// Connects a block at `height` to `utxo`; returns its serialized `CBlockUndo`.
fn connect(utxo: &mut Utxo, height: u32, bytes: &[u8]) -> CoreUndoBytes {
    let block = read_block(bytes).unwrap();
    let mut spent: Vec<Vec<Vec<u8>>> = Vec::new();
    for (n, tx) in block.txs.iter().enumerate() {
        if n > 0 {
            let coins = tx.inputs.iter().map(|op| {
                utxo.remove(op).unwrap_or_else(|| {
                    panic!(
                        "height {height}: {}:{} is not in the UTXO set",
                        dash_coreimport::hash_to_hex(&op.txid),
                        op.vout
                    )
                })
            });
            spent.push(coins.collect());
        }
        for (vout, o) in tx.outputs.iter().enumerate() {
            if is_unspendable(o.script) {
                continue;
            }
            let mut coin = Vec::new();
            coin_bytes(height, n == 0, o.value, o.script, &mut coin);
            let op = OutPoint {
                txid: tx.txid,
                vout: vout as u32,
            };
            utxo.insert(op, coin);
        }
    }
    let mut undo = Vec::new();
    write_block_undo(spent.iter().map(|t| t.iter().map(Vec::as_slice)), &mut undo).unwrap();
    CoreUndoBytes::from_raw(undo)
}

/// Every block's serialized `CBlockUndo`, recomputed from the blocks alone by replaying them
/// against an in-memory UTXO set. `chain` is genesis first, in height order; the result is indexed
/// by height (genesis has an empty record and is never written).
pub fn undo_for_chain<'a>(chain: impl IntoIterator<Item = &'a [u8]>) -> Vec<CoreUndoBytes> {
    let chain: Vec<&[u8]> = chain.into_iter().collect();
    undo_for_tree(&chain, &[]).0
}

/// [`undo_for_chain`] for the best chain, plus the height and undo of every block off it (`side`:
/// blocks of branches that fork from the best chain, as a reorg leaves them), each replayed on
/// its own branch.
pub fn undo_for_tree(
    chain: &[&[u8]],
    side: &[&[u8]],
) -> (Vec<CoreUndoBytes>, HashMap<Hash256, (u32, CoreUndoBytes)>) {
    let mut children: HashMap<Hash256, Vec<&[u8]>> = HashMap::new();
    for b in side {
        children.entry(prev_hash(b)).or_default().push(b);
    }
    let mut utxo = Utxo::new();
    let mut out = Vec::with_capacity(chain.len());
    let mut side_undo = HashMap::new();
    for (height, bytes) in chain.iter().enumerate() {
        // Genesis is never connected: its outputs are not spendable.
        out.push(if height == 0 {
            CoreUndoBytes::from_raw(Vec::new())
        } else {
            connect(&mut utxo, height as u32, bytes)
        });
        // Branches forking here, each on a copy of this block's UTXO set.
        let hash = block_hash(&bytes[..80]);
        if !children.contains_key(&hash) {
            continue;
        }
        let mut stack = vec![(hash, height as u32, utxo.clone())];
        while let Some((parent, h, view)) = stack.pop() {
            for child in children.get(&parent).into_iter().flatten() {
                let mut view = view.clone();
                let undo = connect(&mut view, h + 1, child);
                let hash = block_hash(&child[..80]);
                side_undo.insert(hash, (h + 1, undo));
                stack.push((hash, h + 1, view));
            }
        }
    }
    assert_eq!(side_undo.len(), side.len(), "side blocks that do not fork from the chain");
    (out, side_undo)
}

/// Compares every blk/rev file of two blocks directories (contents and lengths); returns the
/// number of files compared.
pub fn compare_dirs(core: &Path, ours: &Path) -> Result<usize, String> {
    let names = |dir: &Path| -> std::collections::BTreeMap<String, PathBuf> {
        let mut m = std::collections::BTreeMap::new();
        for prefix in ["blk", "rev"] {
            for (_, p) in list_files(dir, prefix).unwrap() {
                m.insert(p.file_name().unwrap().to_string_lossy().into_owned(), p);
            }
        }
        m
    };
    let (a, b) = (names(core), names(ours));
    if a.keys().ne(b.keys()) {
        let only_core: Vec<_> = a.keys().filter(|k| !b.contains_key(*k)).collect();
        let only_ours: Vec<_> = b.keys().filter(|k| !a.contains_key(*k)).collect();
        return Err(format!("file sets differ: only Core {only_core:?}, only ours {only_ours:?}"));
    }
    for (name, pa) in &a {
        let (x, y) = (std::fs::read(pa).unwrap(), std::fs::read(&b[name]).unwrap());
        if x != y {
            let at = x.iter().zip(&y).position(|(p, q)| p != q).unwrap_or(x.len().min(y.len()));
            return Err(format!(
                "{name}: Core {} bytes, ours {} bytes, first difference at {at:#x}",
                x.len(),
                y.len()
            ));
        }
    }
    Ok(a.len())
}
