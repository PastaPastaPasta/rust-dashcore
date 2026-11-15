// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_chain::BlockRef;
use dash_consensus::coins::compress::MAX_SPECIAL_SCRIPT_LEN;
use dash_consensus::coins::stats::CoinStatsAccumulator;
use dash_consensus::coins::{BlockUndo, Coin, MuHash3072, TxUndo, is_unspendable};
use dash_consensus::hash::sha256d;
use dash_consensus::{Block, BlockId, decode_block};
use dash_primitives::{BlockHash, OutPoint};
use dashd_indexes::coinstats::{CoinStatsAdapter, codec, from_parts};
use dashd_indexes::{CoinStatsEntry, IndexError, SubsidyInputs};
use serde_json::Value;

#[derive(Default)]
struct Subsidies(HashMap<BlockHash, i64>);

impl SubsidyInputs for Subsidies {
    fn subsidy(&self, block: &BlockRef) -> Result<i64, IndexError> {
        self.0.get(&block.hash).copied().ok_or_else(|| IndexError::Io("missing subsidy".into()))
    }
}

fn hash(bytes: &[u8]) -> String {
    bytes.iter().rev().map(|byte| format!("{byte:02x}")).collect()
}

fn duffs(value: &Value) -> i64 {
    let text = value.to_string();
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap();
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: i64 = format!("{whole}{fraction}").parse().unwrap();
    let scale = 8 + exponent - fraction.len() as i32;
    if scale >= 0 {
        digits * 10i64.pow(scale as u32)
    } else {
        let divisor = 10i64.pow((-scale) as u32);
        assert_eq!(digits % divisor, 0, "whole duffs: {text}");
        digits / divisor
    }
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn reference(block: &Block, height: u32) -> BlockRef {
    BlockRef {
        id: BlockId(height),
        hash: block.hash(),
        height,
    }
}

// Core: validation.cpp:2102-2114; coins.cpp:118-125
fn connect(set: &mut HashMap<OutPoint, Coin>, block: &Block, height: u32) -> BlockUndo {
    let mut undo = BlockUndo::default();
    if height == 0 {
        return undo;
    }
    for tx in block.txs() {
        if !tx.is_coinbase() {
            undo.txs.push(TxUndo {
                coins: tx
                    .inputs()
                    .iter()
                    .map(|input| set.remove(&input.prevout()).unwrap())
                    .collect(),
            });
        }
        for (index, output) in (0u32..).zip(tx.outputs()) {
            if !is_unspendable(output.script_pubkey().as_bytes()) {
                set.insert(
                    OutPoint::new(tx.txid(), index),
                    Coin::from_output(output, height, tx.is_coinbase()),
                );
            }
        }
    }
    undo
}

// Core: rpc/blockchain.cpp:1297-1318
fn cumulative(parent: &CoinStatsEntry, subsidy: i64, bi: [i64; 7]) -> CoinStatsEntry {
    CoinStatsEntry {
        total_subsidy: parent.total_subsidy + subsidy,
        prevout_spent: parent.prevout_spent + bi[0],
        coinbase: parent.coinbase + bi[1],
        new_outputs_ex_coinbase: parent.new_outputs_ex_coinbase + bi[2],
        total_unspendable: parent.total_unspendable + bi[3],
        unspendables_genesis: parent.unspendables_genesis + bi[4],
        unspendables_scripts: parent.unspendables_scripts + bi[5],
        unspendables_unclaimed_rewards: parent.unspendables_unclaimed_rewards + bi[6],
        ..CoinStatsEntry::default()
    }
}

// Core: kernel/coinstats.cpp:41-49
fn bogo_size(set: &HashMap<OutPoint, Coin>) -> u64 {
    let mut buf = [0; MAX_SPECIAL_SCRIPT_LEN];
    set.values().map(|coin| 50 + coin.as_ref().script_pubkey().expand(&mut buf).len() as u64).sum()
}

fn assert_round_trip(entry: &CoinStatsEntry, state: &CoinStatsAccumulator) {
    let decoded = codec::decode(&codec::encode(entry)).unwrap();
    assert_eq!(&decoded, entry);
    let saved = MuHash3072::from_bytes(&state.muhash().to_bytes());
    let mut restored = from_parts(saved, &decoded).unwrap();
    assert_eq!(CoinStatsEntry::from_accumulator(&mut restored), *entry);
}

#[test]
fn codec_matches_core_layout_and_round_trips_signed_amounts() {
    let entry = CoinStatsEntry {
        muhash: std::array::from_fn(|i| i as u8),
        txouts: 0x0102_0304_0506_0708,
        bogo_size: u64::MAX,
        total_amount: i64::MIN,
        total_subsidy: i64::MAX,
        total_unspendable: -1,
        prevout_spent: 2,
        new_outputs_ex_coinbase: 3,
        coinbase: 4,
        unspendables_genesis: 5,
        unspendables_bip30: 6,
        unspendables_scripts: 7,
        unspendables_unclaimed_rewards: -8,
    };
    let mut expected: Vec<u8> = (0..32).collect();
    expected.extend([8, 7, 6, 5, 4, 3, 2, 1]);
    expected.extend([255; 8]);
    expected.extend([0, 0, 0, 0, 0, 0, 0, 128]);
    expected.extend([255, 255, 255, 255, 255, 255, 255, 127]);
    expected.extend([255; 8]);
    for value in 2..=7 {
        expected.extend([value, 0, 0, 0, 0, 0, 0, 0]);
    }
    expected.extend([248, 255, 255, 255, 255, 255, 255, 255]);
    assert_eq!(expected.len(), codec::DBVAL_SIZE);
    assert_eq!(codec::encode(&entry).as_slice(), expected);
    assert_eq!(codec::decode(&expected).unwrap(), entry);
    assert_eq!(
        codec::decode(&codec::encode(&CoinStatsEntry::default())).unwrap(),
        CoinStatsEntry::default()
    );
}

#[test]
fn codec_rejects_incomplete_and_trailing_bytes() {
    for length in 0..codec::DBVAL_SIZE {
        assert!(matches!(codec::decode(&vec![0; length]), Err(IndexError::Corrupt(_))));
    }
    assert!(matches!(codec::decode(&[0; codec::DBVAL_SIZE + 1]), Err(IndexError::Corrupt(_))));
}

#[test]
fn restore_rejects_a_muhash_that_does_not_match_the_entry() {
    let mut state = CoinStatsAccumulator::new();
    let entry = CoinStatsEntry::from_accumulator(&mut state);
    assert!(from_parts(MuHash3072::new(), &entry).is_ok());
    assert!(matches!(
        from_parts(MuHash3072::from_data(b"another set"), &entry),
        Err(IndexError::Corrupt(_))
    ));
}

fn recorded(name: &str) -> (usize, usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let rows = json(&root.join("blocks.json"));
    let rows: HashMap<BlockHash, &Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["hash"].as_str().unwrap().parse().unwrap(), row))
        .collect();
    let chain = json(&root.join("chain.json"));
    let bytes = fs::read(root.join("blocks/blk00000.dat")).unwrap();
    let rev = fs::read(root.join("blocks/rev00000.dat")).unwrap();
    let mut rev_records = Vec::new();
    let mut rest = &rev[..];
    while rest[..8] != [0; 8] {
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        rev_records.push((&rest[8..8 + size], &rest[8 + size..8 + size + 32]));
        rest = &rest[8 + size + 32..];
    }
    let mut states =
        HashMap::<BlockHash, (CoinStatsAccumulator, CoinStatsEntry, HashMap<OutPoint, Coin>)>::new(
        );
    let mut blocks = HashMap::new();
    let mut expected = HashMap::<BlockHash, CoinStatsEntry>::new();
    let mut subsidies = Subsidies::default();
    let mut negative = 0;
    let mut underclaimed = 0;
    let mut rest = &bytes[..];
    while rest[..8] != [0; 8] {
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let block = decode_block(&mut &rest[8..8 + size]).unwrap().into_value();
        rest = &rest[8 + size..];
        let row = rows[&block.hash()];
        let stats = &row["gettxoutsetinfo"];
        let height = stats["height"].as_u64().unwrap() as u32;
        let parent = block.header().prev_hash();
        let (mut state, prev, mut set) = if height == 0 {
            (CoinStatsAccumulator::new(), CoinStatsEntry::default(), HashMap::new())
        } else {
            states[&parent].clone()
        };
        let undo = connect(&mut set, &block, height);
        if height > 0 {
            let encoded = undo.core_bytes();
            let checksum = sha256d(&[parent.as_bytes(), encoded.as_slice()].concat());
            assert!(
                rev_records
                    .iter()
                    .any(|(bytes, sum)| *bytes == encoded.as_slice() && *sum == checksum),
                "Core undo at {height}"
            );
        }
        let total_subsidy =
            duffs(&stats["total_amount"]) + duffs(&stats["total_unspendable_amount"]);
        let subsidy = total_subsidy - prev.total_subsidy;
        subsidies.0.insert(block.hash(), subsidy);
        let info = &stats["block_info"];
        let bi = [
            duffs(&info["prevout_spent"]),
            duffs(&info["coinbase"]),
            duffs(&info["new_outputs_ex_coinbase"]),
            duffs(&info["unspendable"]),
            duffs(&info["unspendables"]["genesis_block"]),
            duffs(&info["unspendables"]["scripts"]),
            duffs(&info["unspendables"]["unclaimed_rewards"]),
        ];
        negative += usize::from(bi[6] < 0);
        underclaimed += usize::from(height > 0 && bi[6] > 0);
        let mut want = cumulative(&prev, subsidy, bi);
        let hash: BlockHash = stats["muhash"].as_str().unwrap().parse().unwrap();
        want.muhash = *hash.as_bytes();
        want.txouts = stats["txouts"].as_u64().unwrap();
        want.bogo_size = stats["bogosize"].as_u64().unwrap();
        want.total_amount = duffs(&stats["total_amount"]);
        assert_eq!(want.total_unspendable, duffs(&stats["total_unspendable_amount"]));
        let adapter =
            CoinStatsAdapter::new(Arc::new(Subsidies(HashMap::from([(block.hash(), subsidy)]))));
        let at = reference(&block, height);
        let entry = adapter.append_block(&mut state, &at, &block, &undo).unwrap();
        assert_eq!(entry, want, "{name} at {height}");
        assert_round_trip(&entry, &state);
        if height > 0 {
            let mut reversed = state.clone();
            assert_eq!(
                adapter.reverse_block(&mut reversed, &at, &block, &undo).unwrap(),
                prev,
                "{name}: reverse {height}"
            );
        }
        expected.insert(block.hash(), want);
        states.insert(block.hash(), (state, entry, set));
        blocks.insert(block.hash(), (block, undo, height));
    }
    assert_eq!(blocks.len(), rows.len());
    let adapter = CoinStatsAdapter::new(Arc::new(subsidies));
    let genesis = blocks.values().find(|(_, _, height)| *height == 0).unwrap();
    let mut state = CoinStatsAccumulator::new();
    adapter.append_block(&mut state, &reference(&genesis.0, 0), &genesis.0, &genesis.1).unwrap();
    let mut tip = genesis.0.hash();
    let mut visited = HashSet::from([tip]);
    for event in chain["events"].as_array().unwrap() {
        if let Some(value) = event.get("connect") {
            let block_hash = value.as_str().unwrap().parse().unwrap();
            let (block, undo, height) = &blocks[&block_hash];
            assert_eq!(block.header().prev_hash(), tip);
            assert_eq!(
                adapter.append_block(&mut state, &reference(block, *height), block, undo).unwrap(),
                expected[&block_hash]
            );
            tip = block_hash;
            visited.insert(tip);
        } else {
            let block_hash: BlockHash = event["disconnect"].as_str().unwrap().parse().unwrap();
            assert_eq!(block_hash, tip);
            let (block, undo, height) = &blocks[&tip];
            tip = block.header().prev_hash();
            assert_eq!(
                adapter.reverse_block(&mut state, &reference(block, *height), block, undo).unwrap(),
                expected[&tip]
            );
        }
    }
    assert_eq!(visited.len(), rows.len());
    (negative, underclaimed)
}

#[test]
fn recorded_g_matches_every_entry_and_reverses_each_height() {
    let (negative, _) = recorded("dashd-v24");
    assert!(negative > 0, "asset unlocks have negative unclaimed rewards");
}

#[test]
fn recorded_g2_matches_underclaimed_coinbase_and_reorgs() {
    let (_, underclaimed) = recorded("dashd-v24-reorg");
    assert!(underclaimed > 0, "g2 records the underclaimed coinbase");
}

fn compressed(path: &Path) -> impl BufRead + use<> {
    BufReader::new(zstd::stream::read::Decoder::new(File::open(path).unwrap()).unwrap())
}

fn conformance_chain(dirs: &[PathBuf]) -> (usize, usize) {
    let dir = &dirs[0];
    let mut goldens = BTreeMap::<u32, Value>::new();
    for dir in dirs {
        let rows: BTreeMap<u32, Value> = compressed(&dir.join("golden.jsonl.zst"))
            .lines()
            .filter_map(|line| {
                let row: Value = serde_json::from_str(&line.unwrap()).unwrap();
                (row["k"] == "C").then(|| (row["h"].as_u64().unwrap() as u32, row))
            })
            .collect();
        for (height, row) in rows {
            if let Some(kept) = goldens.get(&height) {
                assert_eq!(kept["b"], row["b"]);
                for field in ["muhash", "txouts", "amount", "unspendable", "bi", "stats", "undo"] {
                    assert_eq!(
                        kept["s"][field],
                        row["s"][field],
                        "{} at {height}: {field}",
                        dir.display()
                    );
                }
            } else {
                goldens.insert(height, row);
            }
        }
    }
    let mut blocks = compressed(&dir.join("blocks.dat.zst"));
    let mut state = CoinStatsAccumulator::new();
    let mut parent = CoinStatsEntry::default();
    let mut set = HashMap::new();
    let mut negative = 0;
    for (&height, row) in &goldens {
        let mut prefix = [0; 8];
        blocks.read_exact(&mut prefix).unwrap();
        let size = u32::from_le_bytes(prefix[4..8].try_into().unwrap()) as usize;
        let mut bytes = vec![0; size];
        blocks.read_exact(&mut bytes).unwrap();
        let block = decode_block(&mut &bytes[..]).unwrap().into_value();
        assert_eq!(hash(block.hash().as_bytes()), row["b"].as_str().unwrap());
        let undo = connect(&mut set, &block, height);
        let golden = &row["s"];
        let subsidy = if height == 0 {
            block.txs()[0].value_out()
        } else {
            assert_eq!(
                hash(&sha256d(undo.core_bytes().as_slice())),
                golden["undo"].as_str().unwrap()
            );
            golden["stats"][0].as_i64().unwrap()
        };
        let adapter =
            CoinStatsAdapter::new(Arc::new(Subsidies(HashMap::from([(block.hash(), subsidy)]))));
        let entry =
            adapter.append_block(&mut state, &reference(&block, height), &block, &undo).unwrap();
        let bi: [i64; 7] = std::array::from_fn(|i| golden["bi"][i].as_i64().unwrap());
        negative += usize::from(bi[6] < 0);
        let mut want = cumulative(&parent, subsidy, bi);
        let muhash: BlockHash = golden["muhash"].as_str().unwrap().parse().unwrap();
        want.muhash = *muhash.as_bytes();
        want.txouts = golden["txouts"].as_u64().unwrap();
        want.total_amount = golden["amount"].as_i64().unwrap();
        want.bogo_size = bogo_size(&set);
        assert_eq!(want.total_unspendable, golden["unspendable"].as_i64().unwrap());
        assert_eq!(entry, want, "{} at {height}", dir.display());
        if height > 0 {
            let mut reversed = state.clone();
            assert_eq!(
                adapter
                    .reverse_block(&mut reversed, &reference(&block, height), &block, &undo)
                    .unwrap(),
                parent,
                "{} reverse {height}",
                dir.display()
            );
        }
        parent = entry;
    }
    assert_eq!(blocks.read(&mut [0]).unwrap(), 0, "one golden per block");
    (goldens.len(), negative)
}

#[test]
fn conformance_goldens_match_every_height_and_parent() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/statedigest/regtest-v24");
    let mut chains = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut nodes = 0;
    for test in fs::read_dir(root).unwrap() {
        let test = test.unwrap().path();
        if !test.is_dir() {
            continue;
        }
        for node in fs::read_dir(test).unwrap() {
            let node = node.unwrap().path();
            if !node.join("extract.json").exists() {
                continue;
            }
            nodes += 1;
            let extract = json(&node.join("extract.json"));
            chains
                .entry(extract["blocks_dat_sha256"].as_str().unwrap().to_owned())
                .or_default()
                .push(node);
        }
    }
    assert_eq!(nodes, 81);
    assert_eq!(chains.len(), 21);
    let chains: Vec<_> = chains.into_values().collect();
    let checked = std::thread::scope(|scope| {
        let handles: Vec<_> = chains
            .chunks(3)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk.iter().map(|dirs| conformance_chain(dirs)).collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
    });
    let heights: usize = checked.iter().map(|(heights, _)| heights).sum();
    let negative: usize = checked.iter().map(|(_, negative)| negative).sum();
    assert_eq!(heights, 8746);
    assert!(negative > 0);
    eprintln!(
        "checked {heights} heights across {} chains, {negative} negative unclaimed rewards",
        chains.len()
    );
}

#[test]
fn adapter_errors_preserve_state_and_genesis_cannot_be_reversed() {
    let bytes = include_bytes!("fixtures/dashd-v24/blocks/blk00000.dat");
    let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let block = decode_block(&mut &bytes[8..8 + size]).unwrap().into_value();
    let mut state = CoinStatsAccumulator::new();
    let before = state.muhash().to_bytes();
    let totals = *state.totals();
    let adapter = CoinStatsAdapter::new(Arc::new(Subsidies::default()));
    assert!(matches!(
        adapter.append_block(&mut state, &reference(&block, 0), &block, &BlockUndo::default()),
        Err(IndexError::Io(_))
    ));
    assert_eq!(state.muhash().to_bytes(), before);
    assert_eq!(*state.totals(), totals);
    let adapter =
        CoinStatsAdapter::new(Arc::new(Subsidies(HashMap::from([(block.hash(), 50_0000_0000)]))));
    let undo = BlockUndo {
        txs: vec![TxUndo::default()],
    };
    assert!(matches!(
        adapter.append_block(&mut state, &reference(&block, 1), &block, &undo),
        Err(IndexError::Corrupt(_))
    ));
    assert!(matches!(
        adapter.reverse_block(&mut state, &reference(&block, 1), &block, &undo),
        Err(IndexError::Corrupt(_))
    ));
    assert_eq!(state.muhash().to_bytes(), before);
    assert_eq!(*state.totals(), totals);
    let genesis = adapter
        .append_block(&mut state, &reference(&block, 0), &block, &BlockUndo::default())
        .unwrap();
    let before = state.muhash().to_bytes();
    assert!(matches!(
        adapter.reverse_block(&mut state, &reference(&block, 0), &block, &BlockUndo::default()),
        Err(IndexError::Corrupt(_))
    ));
    assert_eq!(state.muhash().to_bytes(), before);
    assert_eq!(CoinStatsEntry::from_accumulator(&mut state), genesis);
}
