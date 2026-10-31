//! `coins::stats` (CH3a) over mainnet blocks 0–1300 and the undo records Dash Core wrote for them
//! (the reference oracle's block files; `dash-coreimport/tests/data/mainnet-0-1300`).
//!
//! There are no per-block mainnet goldens yet (they need a mainnet node with the coinstats index,
//! O8 P-3): Core's own per-block values are compared in `dash-conformance`'s
//! `coin_stats_goldens`, over the regtest v24 recordings. Here the statistics are checked against
//! what the oracle reported for this range and against independent recomputation:
//! - at 1300, 1,269 outputs (the oracle's `gettxoutsetinfo none 1300`);
//! - at every 100th height and at 1300, the accumulated MuHash is that of the set the replay
//!   rebuilt, and the output count and amount are the set's;
//! - every block's unclaimed reward is its subsidy plus fees minus its coinbase's value (no
//!   platform transfer or unspendable output occurs in this range), and its `getblockstats`
//!   fields are the block's counts and its undo record's amounts;
//! - reversing every block from 1300 back to 1 returns each parent's totals and, at every 100th
//!   height, its MuHash; at 0 what is left is genesis's totals.

#[path = "support/core_undo.rs"]
mod core_undo;

use std::collections::BTreeMap;

use dash_consensus::coins::stats::{CoinStatsAccumulator, CoinStatsTotals, block_stats};
use dash_consensus::coins::{BlockUndo, Coin, MuHash3072, decode_block_undo, is_unspendable};
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::OutPoint;
use dash_consensus::subsidy::{COIN, SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_consensus::{Amount, Block, decode_block};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};

use core_undo::{mainnet_blocks, mainnet_undo_records};

fn block(bytes: &[u8]) -> Block {
    let mut cursor = bytes;
    decode_block(&mut cursor).expect("Core's block decodes").into_value()
}

/// The chain: each block with Core's undo record (the one whose rev-file checksum binds it to the
/// block's parent; none for genesis) and its subsidy.
fn chain() -> Vec<(Block, BlockUndo, Amount)> {
    let params = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0)
        .build()
        .expect("mainnet params");
    let genesis = genesis_reward(&params.genesis).expect("genesis reward");
    let subsidy = SubsidyParams::from_chain_params(&params, genesis).expect("subsidy params");
    let records = mainnet_undo_records();
    let blocks: Vec<Block> = mainnet_blocks().iter().map(|b| block(b)).collect();
    assert_eq!(blocks.len(), 1301);
    let mut chain: Vec<(Block, BlockUndo, Amount)> = Vec::with_capacity(blocks.len());
    for (height, block) in blocks.into_iter().enumerate() {
        let (undo, amount) = if height == 0 {
            (BlockUndo::default(), genesis)
        } else {
            let prev = *block.header().prev_hash().as_bytes();
            let (bytes, _) = records
                .iter()
                .find(|(undo, sum)| sha256d(&[&prev[..], undo].concat()) == *sum)
                .unwrap_or_else(|| panic!("no undo record for height {height}"));
            let undo = decode_block_undo(&mut &bytes[..]).expect("Core's record").into_value();
            // `GetBlockSubsidy(pindex)`: the parent's bits and height; V20 is far ahead.
            let prev_bits = chain[height - 1].0.header().bits();
            let amount =
                block_subsidy_inner(prev_bits, height as i32 - 1, false, &subsidy).unwrap();
            (undo, amount)
        };
        chain.push((block, undo, amount));
    }
    chain
}

/// The MuHash and totals of a set, by scanning it.
fn scan(set: &BTreeMap<OutPoint, Coin>) -> ([u8; 32], u64, Amount) {
    let mut muhash = MuHash3072::new();
    for (outpoint, coin) in set {
        muhash.insert_coin(outpoint, &coin.as_ref());
    }
    (muhash.finalize(), set.len() as u64, set.values().map(Coin::value).sum())
}

#[test]
fn mainnet_coin_stats_through_1300() {
    let chain = chain();
    let mut set: BTreeMap<OutPoint, Coin> = BTreeMap::new();
    let mut acc = CoinStatsAccumulator::new();
    let mut history: Vec<(CoinStatsTotals, Option<[u8; 32]>)> = Vec::new();
    let mut spent = 0;
    for (height, (block, undo, subsidy)) in (0u32..).zip(&chain) {
        acc.append_block(height, block, undo, *subsidy).unwrap();
        let totals = *acc.totals();
        let coinbase = block.txs()[0].value_out();
        if height == 0 {
            // Core never connects genesis: its 50 DASH are unspendable, no coin is created.
            assert_eq!(*subsidy, 50 * COIN);
            assert_eq!(coinbase, *subsidy);
            let genesis = CoinStatsTotals {
                total_subsidy: *subsidy,
                total_unspendable_amount: *subsidy,
                total_unspendables_genesis_block: *subsidy,
                ..CoinStatsTotals::default()
            };
            assert_eq!(totals, genesis);
            // getblockstats 0 reads an empty undo record: one output, a 67-byte P2PK script.
            let stats = block_stats(block, undo, *subsidy).unwrap();
            assert_eq!(stats.txs, 1);
            assert_eq!((stats.ins, stats.outs, stats.utxo_increase), (0, 1, 1));
            assert_eq!(block.txs()[0].outputs()[0].script_pubkey.len(), 67);
            assert_eq!((stats.totalfee, stats.utxo_size_inc), (0, 8 + 1 + 67 + 41));
            history.push((totals, Some(acc.clone().finalize())));
            continue;
        }

        let stats = block_stats(block, undo, *subsidy).unwrap();
        let fees: Amount = undo
            .txs
            .iter()
            .zip(&block.txs()[1..])
            .map(|(tx_undo, tx)| {
                tx_undo.coins.iter().map(Coin::value).sum::<Amount>() - tx.value_out()
            })
            .sum();
        let ins = undo.txs.iter().map(|tx| tx.coins.len() as i64).sum::<i64>();
        let outs = block.txs().iter().map(|tx| tx.outputs().len() as i64).sum::<i64>();
        assert_eq!(stats.subsidy, *subsidy);
        assert_eq!(stats.totalfee, fees, "height {height}");
        assert_eq!((stats.txs, stats.ins, stats.outs), (block.txs().len() as i64, ins, outs));
        assert_eq!(stats.utxo_increase, outs - ins);

        let info = totals.block_info(history.last().map(|(t, _)| t));
        assert_eq!(info.coinbase, coinbase, "height {height}");
        assert_eq!(info.unspendables_scripts, 0, "height {height}");
        assert_eq!(
            info.unspendables_unclaimed_rewards,
            subsidy + fees - coinbase,
            "height {height}"
        );
        assert_eq!(info.unspendable, info.unspendables_unclaimed_rewards);
        assert_eq!(info.prevout_spent - info.new_outputs_ex_coinbase, fees);

        // The set after the block, as UpdateCoins leaves it.
        for (tx_index, tx) in block.txs().iter().enumerate() {
            if tx_index > 0 {
                for (coin, input) in undo.txs[tx_index - 1].coins.iter().zip(tx.inputs()) {
                    assert_eq!(set.remove(&input.prevout).as_ref(), Some(coin), "height {height}");
                    spent += 1;
                }
            }
            for (index, out) in (0u32..).zip(tx.outputs()) {
                assert!(!is_unspendable(out.script_pubkey.as_bytes()));
                let outpoint = OutPoint {
                    hash: tx.txid(),
                    index,
                };
                set.insert(outpoint, Coin::from_output(out, height, tx_index == 0));
            }
        }
        let muhash = if height % 100 == 0 {
            let (muhash, count, amount) = scan(&set);
            assert_eq!(acc.clone().finalize(), muhash, "height {height}");
            assert_eq!((totals.transaction_output_count, totals.total_amount), (count, amount));
            Some(muhash)
        } else {
            None
        };
        history.push((totals, muhash));
    }
    // The oracle's `gettxoutsetinfo none 1300`.
    assert_eq!(acc.totals().transaction_output_count, 1269);
    assert_eq!(set.len(), 1269);
    assert!(spent > 0);

    for height in (1..chain.len()).rev() {
        let (block, undo, subsidy) = &chain[height];
        acc.reverse_block(height as u32, block, undo, *subsidy).unwrap();
        let (totals, muhash) = &history[height - 1];
        assert_eq!(acc.totals(), totals, "reversed {height}");
        if let Some(muhash) = muhash {
            assert_eq!(&acc.clone().finalize(), muhash, "reversed {height}");
        }
    }
}
