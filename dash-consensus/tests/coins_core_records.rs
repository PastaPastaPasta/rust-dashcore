//! Undo records Dash Core wrote, read with the coins codecs (CS5a):
//! - the regtest chain `contrib/node/coins-undo-vectors.py` recorded with the pinned release:
//!   every record decodes to the coins Core's `getblock 3` reports, is Core's serialization, and
//!   matches its rev-file checksum;
//! - mainnet blocks 1–1300 from the reference oracle's files: replaying the chain, every coin in
//!   every record is the output its input spends, and the database serialization of every coin
//!   created reads back unchanged.

#[path = "support/core_undo.rs"]
mod core_undo;

use std::collections::BTreeMap;

use dash_consensus::coins::{
    BlockUndo, Coin, CoinWrite, CoinsView, MemoryCoins, coin_bytes, decode_block_undo,
};
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_consensus::{Block, decode_block};
use dash_core_vectors::hex::parse_hex;

use core_undo::{UndoVector, mainnet_blocks, mainnet_undo_records, regtest};

/// The rev-file checksum: `SHA256d(prev_hash ‖ undo)`.
fn checksum(prev_hash: &[u8; 32], undo: &[u8]) -> [u8; 32] {
    sha256d(&[&prev_hash[..], undo].concat())
}

fn decode_exact(bytes: &[u8]) -> BlockUndo {
    let mut cursor = bytes;
    let decoded = decode_block_undo(&mut cursor).expect("Core's record decodes");
    assert!(cursor.is_empty(), "the record is one CBlockUndo");
    assert!(decoded.wire_is_canonical(), "Core's record is Core's serialization");
    assert_eq!(decoded.core_bytes().as_slice(), bytes);
    let undo = decoded.into_value();
    assert_eq!(undo.core_bytes().as_slice(), bytes, "re-encoding");
    assert_eq!(undo.serialized_size(), bytes.len());
    undo
}

fn block(bytes: &[u8]) -> Block {
    let mut cursor = bytes;
    decode_block(&mut cursor).expect("Core's block decodes").into_value()
}

fn check_regtest(vectors: &[UndoVector]) -> usize {
    let mut coins = 0;
    for v in vectors {
        assert_eq!(checksum(&v.prev_hash, &v.undo), v.checksum, "height {}: checksum", v.height);
        let undo = decode_exact(&v.undo);
        let block = block(&v.block);
        assert_eq!(undo.txs.len() + 1, block.txs().len(), "height {}: one entry per tx", v.height);
        assert_eq!(undo.txs.len(), v.prevouts.len());
        for ((tx_undo, tx), prevouts) in undo.txs.iter().zip(&block.txs()[1..]).zip(&v.prevouts) {
            assert_eq!(tx_undo.coins.len(), tx.raw().inputs.len());
            assert_eq!(tx_undo.coins.len(), prevouts.len());
            for (coin, core) in tx_undo.coins.iter().zip(prevouts) {
                assert_eq!(coin.height(), core.height, "height {}", v.height);
                assert_eq!(coin.is_coinbase(), core.coinbase);
                assert_eq!(coin.value(), core.value);
                assert_eq!(&coin.script_pubkey()[..], &parse_hex(&core.script_pubkey)[..]);
                // The coin's database serialization reads back too.
                assert_eq!(Coin::decode(&mut &coin.serialize()[..]).as_ref(), Ok(coin));
                coins += 1;
            }
        }
    }
    coins
}

#[test]
fn regtest_records_decode_to_cores_coins() {
    // Two coinbases spent at 102; twelve script kinds, a coinbase and an output of the same
    // block spent at 103.
    assert_eq!(check_regtest(&regtest("v24.0")), 16);
}

/// Every script kind of the recorded chain, by `ScriptCompression` form: P2PKH and P2SH (codes
/// 0, 1), compressed P2PK (2, 3), uncompressed P2PK with even and odd y (4, 5), and raw scripts
/// up to `MAX_SCRIPT_SIZE`.
#[test]
fn regtest_records_cover_every_compression_form() {
    let mut forms = BTreeMap::new();
    for v in regtest("v24.0") {
        for coin in decode_exact(&v.undo).txs.iter().flat_map(|tx| &tx.coins) {
            let compressed = dash_consensus::coins::compress::compress_script(coin.script_pubkey());
            let form = compressed.map_or(6, |(code, _)| code);
            *forms.entry(form).or_insert(0) += 1;
        }
    }
    for form in [0u8, 1, 2, 3, 4, 5, 6] {
        assert!(forms.contains_key(&form), "no coin with ScriptCompression form {form}");
    }
}

#[test]
fn mainnet_records_hold_the_spent_outputs() {
    let blocks: Vec<Block> = mainnet_blocks().iter().map(|b| block(b)).collect();
    assert_eq!(blocks.len(), 1301);
    let records = mainnet_undo_records();

    let mut utxo = MemoryCoins::new();
    let mut matched = 0;
    let mut spent = 0;
    for (height, block) in blocks.iter().enumerate() {
        let height = height as u32;
        // Core never connects the genesis block: it has no undo record and its output is not a
        // coin.
        if height == 0 {
            continue;
        }
        // The record whose checksum binds it to this block's parent.
        let prev = *block.header().prev_hash().as_bytes();
        let (bytes, _) = records
            .iter()
            .find(|(undo, sum)| checksum(&prev, undo) == *sum)
            .unwrap_or_else(|| panic!("no undo record for height {height}"));
        let undo = decode_exact(bytes);
        assert_eq!(undo.txs.len() + 1, block.txs().len());
        for (tx_undo, tx) in undo.txs.iter().zip(&block.txs()[1..]) {
            assert_eq!(tx_undo.coins.len(), tx.raw().inputs.len());
            for (coin, input) in tx_undo.coins.iter().zip(&tx.raw().inputs) {
                let Ok(stored) = utxo.coin(&input.prevout);
                let stored = stored.map(|c| c.to_coin());
                assert_eq!(stored.as_ref(), Some(coin), "height {height}");
                spent += 1;
            }
        }
        matched += 1;
        // Connect: spend the inputs, add the outputs (no unspendable output occurs here).
        for tx in block.txs() {
            if !tx.is_coinbase() {
                for input in &tx.raw().inputs {
                    utxo.write(input.prevout, CoinWrite::Erase);
                }
            }
            let txid: TxHash = tx.txid();
            for (index, out) in tx.raw().outputs.iter().enumerate() {
                let coin = Coin::from_output(out, height, tx.is_coinbase());
                let mut bytes = Vec::new();
                coin_bytes(out, height, tx.is_coinbase(), &mut bytes);
                assert_eq!(bytes, coin.serialize());
                assert_eq!(Coin::decode(&mut &bytes[..]).as_ref(), Ok(&coin));
                utxo.write(
                    OutPoint {
                        hash: txid,
                        index: index as u32,
                    },
                    CoinWrite::Put(coin),
                );
            }
        }
    }
    assert_eq!(matched, 1300);
    assert!(spent > 0);
    // Core's `gettxoutsetinfo none 1300` (the fixture's provenance): 1,269 unspent outputs.
    assert_eq!(utxo.len(), 1269);
}
