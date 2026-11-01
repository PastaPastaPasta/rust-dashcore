//! `dash_consensus::coins::stats` (CH3a, §2.15) against Dash Core's own per-block coin
//! statistics: the goldens of the regtest v24 recordings (`test-vectors/statedigest/regtest-v24`,
//! O8 §5.3), which Core's coinstats index answered through `gettxoutsetinfo muhash <h> true` and
//! `getblockstats <h>` for every block of each node's final chain.
//!
//! Each distinct chain (the nodes' `blocks.dat`, every active-chain block in height order) is
//! replayed from genesis with an in-memory UTXO set, which gives each block's undo record. Then,
//! at every height a golden has:
//! - the undo record is Core's: its `sha256d` is the golden `undo` (Core's rev-file record);
//! - `txouts`, `amount`, `unspendable` and the seven `block_info` amounts (`bi`) are the
//!   accumulator's;
//! - the seven `getblockstats` fields (`stats`) are [`block_stats`]'s;
//! - reversing the block (`ReverseBlock`) gives back the parent's totals exactly;
//! - at every [`MUHASH_EVERY`]th height, at every height with an asset unlock, at the first
//!   [`MUHASH_SPENDS`] heights with a spend, and at the tip: the MuHash is the golden `muhash`,
//!   and reversing the block gives the parent's.
//!
//! The subsidy, which Core's index takes from `GetBlockSubsidy`, is an input here: the golden
//! `stats.subsidy` (`getblockstats` calls the same function), and at genesis, where the
//! extractor records no `stats` (Core would answer), the genesis coinbase's value.
//!
//! The MuHash is not compared at every height only because an unoptimised `Finalize` (a 3072-bit
//! inversion) takes about half a second, and `feature_block` alone has 1,119 blocks with spends;
//! the accumulator's state is cumulative, so a coin it got wrong at any height stays in every
//! later hash and the tip's comparison catches it.

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;

use dash_conformance::check::rev_hex;
use dash_conformance::digest::store::{Node, Vectors, open, read};
use dash_consensus::coins::stats::{
    BlockInfo, BlockStats, CoinStatsAccumulator, CoinStatsTotals, block_stats,
};
use dash_consensus::coins::{BlockUndo, Coin, TxUndo, is_unspendable};
use dash_consensus::hash::sha256d;
use dash_consensus::{Amount, Block, decode_block};
use dash_primitives::OutPoint;
use rayon::prelude::*;
use serde_json::Value;

/// Compare the MuHash at heights that are multiples of this.
const MUHASH_EVERY: u32 = 100;
/// And at this many of each chain's heights with a spend, the first ones.
const MUHASH_SPENDS: usize = 64;

/// One golden `C` record's §5.3 values.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Golden {
    block: String,
    undo: Option<String>,
    muhash: String,
    txouts: u64,
    amount: Amount,
    unspendable: Amount,
    bi: [Amount; 7],
    stats: Option<[i64; 7]>,
}

fn ints<const N: usize>(value: &Value) -> [i64; N] {
    let items: Vec<i64> =
        value.as_array().expect("an array").iter().map(|v| v.as_i64().expect("an int")).collect();
    items.try_into().expect("the golden's length")
}

/// Every golden `C` record of `node`, by height.
fn goldens(node: &Node) -> BTreeMap<u32, Golden> {
    let mut out = BTreeMap::new();
    for line in open(&node.golden()).unwrap().lines() {
        let record: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if record["k"] != "C" {
            continue;
        }
        let s = &record["s"];
        let golden = Golden {
            block: record["b"].as_str().unwrap().to_owned(),
            undo: s["undo"].as_str().map(str::to_owned),
            muhash: s["muhash"].as_str().unwrap().to_owned(),
            txouts: s["txouts"].as_u64().unwrap(),
            amount: s["amount"].as_i64().unwrap(),
            unspendable: s["unspendable"].as_i64().unwrap(),
            bi: ints(&s["bi"]),
            stats: (!s["stats"].is_null()).then(|| ints(&s["stats"])),
        };
        let height = u32::try_from(record["h"].as_u64().unwrap()).unwrap();
        assert!(out.insert(height, golden).is_none(), "{}: two goldens at {height}", node.label());
    }
    out
}

/// The blocks of a `blocks.dat` (every active-chain block in height order, in `-loadblock` framing:
/// magic, LE32 size, block), decoded one at a time (`feature_block`'s is 1.09 GB).
fn blocks(mut rest: &[u8]) -> impl Iterator<Item = Block> + '_ {
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let mut payload = &rest[8..8 + size];
        let block = decode_block(&mut payload).expect("Core's block decodes").into_value();
        assert!(payload.is_empty(), "one block per record");
        rest = &rest[8 + size..];
        Some(block)
    })
}

fn bi(info: &BlockInfo) -> [Amount; 7] {
    [
        info.prevout_spent,
        info.coinbase,
        info.new_outputs_ex_coinbase,
        info.unspendable,
        info.unspendables_genesis_block,
        info.unspendables_scripts,
        info.unspendables_unclaimed_rewards,
    ]
}

fn stats_array(s: &BlockStats) -> [i64; 7] {
    [s.subsidy, s.totalfee, s.txs, s.ins, s.outs, s.utxo_increase, s.utxo_size_inc]
}

/// Connects `block` to the in-memory set as `UpdateCoins` does and returns its undo record: each
/// transaction spends its inputs, then adds its spendable outputs.
fn connect(utxo: &mut HashMap<OutPoint, Coin>, block: &Block, height: u32) -> BlockUndo {
    let mut undo = BlockUndo::default();
    for tx in block.txs() {
        let coinbase = tx.is_coinbase();
        if !coinbase {
            let coins = tx
                .inputs()
                .iter()
                .map(|input| utxo.remove(&input.prevout).expect("the input's coin"))
                .collect();
            undo.txs.push(TxUndo {
                coins,
            });
        }
        let txid = tx.txid();
        for (index, out) in (0u32..).zip(tx.outputs()) {
            if !is_unspendable(out.script_pubkey.as_bytes()) {
                let outpoint = OutPoint {
                    hash: txid,
                    index,
                };
                utxo.insert(outpoint, Coin::from_output(out, height, coinbase));
            }
        }
    }
    undo
}

/// What one chain's replay compared.
#[derive(Default)]
struct Checked {
    heights: usize,
    muhashes: usize,
    spends: u64,
    unlocks: usize,
    negative_unclaimed: usize,
}

/// Replays one chain against the goldens of every node that recorded it.
fn replay(
    label: &str,
    blocks: impl Iterator<Item = Block>,
    goldens: &BTreeMap<u32, Golden>,
) -> Checked {
    let tip = *goldens.keys().next_back().unwrap();
    let mut checked = Checked::default();
    let mut spend_heights = 0;
    let mut utxo = HashMap::new();
    let mut acc = CoinStatsAccumulator::new();
    let mut prev: Option<CoinStatsTotals> = None;
    for (height, block) in (0u32..).zip(blocks) {
        let block = &block;
        let at = format!("{label} at {height}");
        let golden = goldens.get(&height).unwrap_or_else(|| panic!("{at}: no golden"));
        assert_eq!(rev_hex(block.hash().as_bytes()), golden.block, "{at}: blocks.dat is the chain");

        let (undo, subsidy) = if height == 0 {
            // Core never connects genesis: its output is no coin and it has no undo record.
            let value = block.txs()[0].value_out();
            (BlockUndo::default(), value)
        } else {
            let undo = connect(&mut utxo, block, height);
            let want = golden.undo.as_deref().unwrap_or_else(|| panic!("{at}: no golden undo"));
            assert_eq!(rev_hex(&sha256d(undo.core_bytes().as_slice())), want, "{at}: undo record");
            let stats = golden.stats.unwrap_or_else(|| panic!("{at}: no golden stats"));
            (undo, stats[0])
        };

        acc.append_block(height, block, &undo, subsidy).unwrap();
        let totals = *acc.totals();
        assert_eq!(totals.transaction_output_count, golden.txouts, "{at}: txouts");
        assert_eq!(totals.transaction_output_count, utxo.len() as u64, "{at}: txouts vs set");
        assert_eq!(totals.total_amount, golden.amount, "{at}: amount");
        assert_eq!(totals.total_unspendable_amount, golden.unspendable, "{at}: unspendable");
        assert_eq!(totals.total_unspendables_bip30, 0, "{at}: bip30");
        let info = totals.block_info(prev.as_ref());
        assert_eq!(bi(&info), golden.bi, "{at}: block_info");
        if info.unspendables_unclaimed_rewards < 0 {
            checked.negative_unclaimed += 1;
        }

        let spends: u64 = undo.txs.iter().map(|tx| tx.coins.len() as u64).sum();
        let unlocks = block.txs().iter().filter(|tx| tx.is_platform_transfer()).count();
        checked.spends += spends;
        checked.unlocks += unlocks;
        if spends > 0 {
            spend_heights += 1;
        }
        let compare_muhash = height % MUHASH_EVERY == 0
            || height == tip
            || unlocks > 0
            || (spends > 0 && spend_heights <= MUHASH_SPENDS);
        // Genesis has no golden `stats` and is never reversed (Core's ReverseBlock would take its
        // output out).
        if height > 0 {
            let stats = block_stats(block, &undo, subsidy).unwrap();
            assert_eq!(Some(stats_array(&stats)), golden.stats, "{at}: getblockstats");

            let mut reversed = acc.clone();
            reversed.reverse_block(height, block, &undo, subsidy).unwrap();
            assert_eq!(Some(reversed.totals()), prev.as_ref(), "{at}: reversed totals");
            if compare_muhash {
                let parent = &goldens[&(height - 1)];
                assert_eq!(rev_hex(&reversed.finalize()), parent.muhash, "{at}: reversed muhash");
            }
        }
        if compare_muhash {
            assert_eq!(rev_hex(&acc.clone().finalize()), golden.muhash, "{at}: muhash");
            checked.muhashes += 1;
        }
        checked.heights += 1;
        prev = Some(totals);
    }
    assert_eq!(checked.heights, goldens.len(), "{label}: a block per golden");
    checked
}

/// The recorded nodes grouped by chain (`extract.json`'s `blocks_dat_sha256`).
fn chains() -> BTreeMap<String, Vec<Node>> {
    let vectors = Vectors::regtest_v24();
    let mut chains: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for test in vectors.tests().unwrap() {
        for node in vectors.nodes(&test).unwrap() {
            let extract: Value =
                serde_json::from_slice(&std::fs::read(node.dir.join("extract.json")).unwrap())
                    .unwrap();
            let sha = extract["blocks_dat_sha256"].as_str().unwrap().to_owned();
            chains.entry(sha).or_default().push(node);
        }
    }
    chains
}

#[test]
fn coin_stats_reproduce_cores_goldens() {
    let chains = chains();
    assert_eq!(chains.values().map(Vec::len).sum::<usize>(), 81, "every recorded node");
    let results: Vec<(String, Checked)> = chains
        .par_iter()
        .map(|(_, nodes)| {
            // Core's index is deterministic: nodes with the same chain have the same goldens.
            let mut merged: BTreeMap<u32, Golden> = BTreeMap::new();
            for node in nodes {
                for (height, golden) in goldens(node) {
                    let label = node.label();
                    let kept = merged.entry(height).or_insert_with(|| golden.clone());
                    assert_eq!(kept, &golden, "{label} at {height}: nodes of a chain disagree");
                }
            }
            let label = nodes[0].label();
            let bytes = read(&nodes[0].blocks()).unwrap();
            let checked = replay(&label, blocks(&bytes), &merged);
            (label, checked)
        })
        .collect();

    let total = |f: fn(&Checked) -> u64| results.iter().map(|(_, c)| f(c)).sum::<u64>();
    let heights = total(|c| c.heights as u64);
    eprintln!(
        "{} chains, {heights} blocks, {} MuHashes, {} coins spent, {} asset unlocks, {} blocks \
         with negative unclaimed rewards",
        results.len(),
        total(|c| c.muhashes as u64),
        total(|c| c.spends),
        total(|c| c.unlocks as u64),
        total(|c| c.negative_unclaimed as u64),
    );
    // The set holds 21 chains: every test's, plus a second for feature_masternode_shares and
    // two more for rpc_invalidateblock.
    assert_eq!(results.len(), 21);
    // The recordings exercise spends, asset unlocks and the negative unclaimed rewards they
    // cause (feature_asset_locks).
    assert!(total(|c| c.spends) > 10_000);
    assert!(total(|c| c.unlocks as u64) > 0);
    assert!(total(|c| c.negative_unclaimed as u64) > 0);
}
