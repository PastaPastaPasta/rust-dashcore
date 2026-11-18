// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, HashMap};
use std::iter;
use std::sync::Arc;

use dash_chain::{BlockRef, FlatFilePos};
use dash_consensus::{Block, BlockId, decode_block};
use dash_core_vectors::hex::parse_hex;
use dash_primitives::BlockHash;
use dashd_chainstate::flush::SubscriberHub;
use dashd_chainstate::store::{GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_chainstate::subscriber::{
    BlockSource, ConnectedBlock, DriverConfig, ScriptedFeeds, SubStep, SubscriberData,
    SubscriberDriver, SubscriberError, SyncProgress, TreeIndex,
};
use dashd_indexes::txindex::codec::{self, TxKey};
use dashd_indexes::txindex::data::SEQ_BITS;
use dashd_indexes::txindex::{TX, TXINDEX_DB, TxIndexData};
use dashd_indexes::{IndexError, TxPositions};
use dashd_store::{KvRead, MemoryEngine};
use serde_json::Value;

/// Block records in a `blk` file: each block's data position and bytes.
fn records(bytes: &[u8]) -> impl Iterator<Item = (FlatFilePos, &[u8])> {
    let mut offset = 0;
    iter::from_fn(move || {
        let prefix = bytes.get(offset..offset + 8)?;
        if prefix == [0; 8] {
            return None;
        }
        assert_eq!(&prefix[..4], &[0xfc, 0xc1, 0xb7, 0xdc]);
        let size = u32::from_le_bytes(prefix[4..].try_into().unwrap()) as usize;
        let start = offset + 8;
        offset = start + size;
        Some((
            FlatFilePos {
                file: 0,
                pos: start as u32,
            },
            &bytes[start..offset],
        ))
    })
}

struct ByHash(HashMap<BlockHash, FlatFilePos>);

impl TxPositions for ByHash {
    fn data_pos(&self, block: &BlockRef) -> Result<FlatFilePos, IndexError> {
        self.0.get(&block.hash).copied().ok_or_else(|| IndexError::Io("no position".into()))
    }
}

fn hash_bytes(hex: &str) -> Vec<u8> {
    parse_hex(hex).into_iter().rev().collect()
}

/// Every block of a recording connected through the data object: each row's position must be
/// where Core's `FindTx` reads the recorded transaction (`index/txindex.cpp:83-103`).
fn positions_match(dir: &str, blk: &[u8], blocks_json: &str, txs_json: &str) {
    let heights: HashMap<BlockHash, u32> = serde_json::from_str::<Vec<Value>>(blocks_json)
        .unwrap()
        .iter()
        .map(|row| {
            let height = row["getblockheader"]["height"].as_u64().unwrap() as u32;
            (row["hash"].as_str().unwrap().parse().unwrap(), height)
        })
        .collect();
    let txs: Vec<Value> = serde_json::from_str(txs_json).unwrap();
    // The genesis coinbase is recorded as an RPC error, without hex.
    let hex: HashMap<Vec<u8>, Vec<u8>> = txs
        .iter()
        .filter_map(|row| {
            let txid = hash_bytes(row["txid"].as_str().unwrap());
            Some((txid, parse_hex(row["common_result"]["hex"].as_str()?)))
        })
        .collect();
    assert_eq!(hex.len() + 1, txs.len());
    // (txid, block) pairs that `getrawtransaction <txid> true <block>` answered.
    let mut answered: Vec<(Vec<u8>, BlockHash)> = txs
        .iter()
        .flat_map(|row| {
            let txid = hash_bytes(row["txid"].as_str().unwrap());
            row["with_blockhash"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, reply)| reply.get("result").is_some())
                .map(move |(block, _)| (txid.clone(), block.parse().unwrap()))
        })
        .collect();

    let blocks: Vec<(FlatFilePos, Block)> = records(blk)
        .map(|(pos, bytes)| (pos, decode_block(&mut &bytes[..]).unwrap().into_value()))
        .collect();
    assert_eq!(blocks.len(), heights.len(), "{dir}: every recorded block is in the file");
    let positions = ByHash(blocks.iter().map(|(pos, b)| (b.hash(), *pos)).collect());
    let engine = MemoryEngine::new(&TXINDEX_DB);
    let data = TxIndexData::<MemoryEngine>::new(&engine, Arc::new(positions)).unwrap();
    let mut state = data.initial_state();
    let mut seen = Vec::new();
    let mut genesis = 0;
    for (id, (pos, block)) in blocks.iter().enumerate() {
        let height = heights[&block.hash()];
        let b = ConnectedBlock {
            block: BlockRef {
                id: BlockId(id as u32),
                hash: block.hash(),
                height,
            },
            data: Some(block),
            undo: None,
        };
        let (rows, next) = data.connect(&b, &state).unwrap();
        if height == 0 {
            genesis += 1;
            assert!(rows.rows.is_empty(), "{dir}: genesis is not indexed");
            continue;
        }
        assert_eq!(rows.rows.len(), block.txs().len());
        for row in &rows.rows {
            let dashd_chainstate::subscriber::Row::Put(_, key, value) = row else {
                panic!("{dir}: a delete");
            };
            assert_eq!(codec::decode_seq(value).unwrap(), state);
            let key = TxKey::decode(key).unwrap();
            assert_eq!(key.pos.block, *pos, "{dir}: the block's data position");
            // Core: index/txindex.cpp:83-95 (the header, then nTxOffset)
            let at = key.pos.block.pos as usize + 80 + key.pos.tx_offset as usize;
            let (txid, want) = hex
                .iter()
                .find(|(txid, want)| {
                    txid[..8] == key.prefix && blk.get(at..at + want.len()) == Some(&want[..])
                })
                .unwrap_or_else(|| panic!("{dir}: no recorded tx at {key:?} in {}", block.hash()));
            assert!(!want.is_empty());
            seen.push((txid.clone(), block.hash()));
        }
        state = next;
    }
    assert_eq!(genesis, 1);
    seen.sort();
    answered.sort();
    assert_eq!(seen, answered, "{dir}: one row per recorded (tx, block)");
}

#[test]
fn positions_equal_cores_on_the_recorded_block_files() {
    positions_match(
        "dashd-v24",
        include_bytes!("fixtures/dashd-v24/blocks/blk00000.dat"),
        include_str!("fixtures/dashd-v24/blocks.json"),
        include_str!("fixtures/dashd-v24/transactions.json"),
    );
    positions_match(
        "dashd-v24-reorg",
        include_bytes!("fixtures/dashd-v24-reorg/blocks/blk00000.dat"),
        include_str!("fixtures/dashd-v24-reorg/blocks.json"),
        include_str!("fixtures/dashd-v24-reorg/transactions.json"),
    );
}

/// Tree blocks carry the recording's block at their height, at a position named by their id.
struct TreeBlocks(Vec<Arc<Block>>);

impl TreeBlocks {
    fn new() -> Self {
        let blk = include_bytes!("fixtures/dashd-v24/blocks/blk00000.dat");
        Self(
            records(blk)
                .take(12)
                .map(|(_, bytes)| Arc::new(decode_block(&mut &bytes[..]).unwrap().into_value()))
                .collect(),
        )
    }
}

impl BlockSource for TreeBlocks {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        Ok(Arc::clone(&self.0[b.height as usize]))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Err(SubscriberError::Blocks(format!("no undo for {}", b.hash)))
    }
}

struct ById;

impl TxPositions for ById {
    fn data_pos(&self, block: &BlockRef) -> Result<FlatFilePos, IndexError> {
        Ok(FlatFilePos {
            file: 0,
            pos: block.id.0,
        })
    }
}

type Driver = SubscriberDriver<TxIndexData<MemoryEngine>>;

struct Fx {
    tree: TreeIndex,
    blocks: Arc<TreeBlocks>,
    db: Arc<GuardedEngine<MemoryEngine>>,
}

impl Fx {
    fn open(&self, chain: Vec<BlockRef>) -> (Driver, ScriptedFeeds) {
        let feeds = ScriptedFeeds::new(4, chain);
        let hub = Arc::new(SubscriberHub::with_feeds(
            Arc::new(feeds.clone()),
            Arc::new(self.tree.clone()),
        ));
        let data = TxIndexData::new(&*self.db, Arc::new(ById)).unwrap();
        let cfg = DriverConfig {
            journal_budget: 1 << 20,
            unbounded_delete_span: 64,
            d_max: None,
            cadence: 1000,
        };
        let mut d =
            SubscriberDriver::open(data, Arc::clone(&self.db), hub, self.blocks.clone(), cfg)
                .expect("opens");
        for _ in 0..100 {
            if d.sync_step().expect("syncs") == SyncProgress::Live {
                return (d, feeds);
            }
        }
        panic!("never live");
    }

    /// Each block id's rows: one sequence number per block, as every key is rewritten whole.
    fn seqs(&self) -> BTreeMap<u32, u64> {
        let mut out = BTreeMap::new();
        for row in self.db.iter_prefix(TX, &[]) {
            let (key, value) = row.unwrap();
            let id = TxKey::decode(&key).unwrap().pos.block.pos;
            let seq = codec::decode_seq(&value).unwrap();
            assert_eq!(*out.entry(id).or_insert(seq), seq, "rows of block {id} disagree");
        }
        out
    }
}

fn drain(d: &mut Driver) {
    for _ in 0..100 {
        if d.step().expect("steps") != SubStep::Progress {
            return;
        }
    }
    panic!("never idle");
}

#[test]
fn seqs_rise_over_connects_disconnects_redelivery_and_restart() {
    let tree = TreeIndex::new();
    let g = tree.genesis();
    let main: Vec<_> = iter::once(g).chain(tree.branch(g, 6)).collect();
    let fx = Fx {
        tree: tree.clone(),
        blocks: Arc::new(TreeBlocks::new()),
        db: Arc::new(GuardedEngine::new(
            MemoryEngine::new(&TXINDEX_DB),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        )),
    };
    let (mut d, feeds) = fx.open(main.clone());
    let first = fx.seqs();
    assert!(!first.contains_key(&g.id.0), "genesis is not indexed");
    let order: Vec<u64> = main[1..].iter().map(|b| first[&b.id.0]).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "connect order: {order:?}");
    assert!(order.iter().all(|s| s >> SEQ_BITS == 1));

    // A disconnect keeps the rows and never lowers the next sequence number.
    let tip = main[6];
    feeds.disconnect();
    drain(&mut d);
    assert_eq!(fx.seqs(), first, "a disconnect deletes nothing");
    let alt = tree.child(main[5]);
    feeds.connect(alt);
    drain(&mut d);
    let alt_seq = fx.seqs()[&alt.id.0];
    assert!(alt_seq > first[&tip.id.0], "after a disconnect");

    // Re-delivery overwrites the block's rows with a new, higher sequence number.
    feeds.disconnect();
    feeds.connect(tip);
    drain(&mut d);
    let again = fx.seqs();
    assert!(again[&tip.id.0] > alt_seq, "re-delivery");
    assert_eq!(again[&alt.id.0], alt_seq, "the stale branch's row stays");
    let before_restart = again.values().copied().max().unwrap();
    drop(d);

    // A restart re-delivers above L in a new epoch, outranking every surviving row.
    let (_d, _feeds) = fx.open(main.clone());
    let after = fx.seqs();
    assert_eq!(after[&alt.id.0], alt_seq);
    for b in &main[1..] {
        let s = after[&b.id.0];
        assert!(s >> SEQ_BITS == 2 && s > before_restart, "block {}: {s:#x}", b.height);
    }
}
