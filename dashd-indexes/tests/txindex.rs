// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, HashMap};
use std::iter;
use std::sync::Arc;

use dash_chain::{BlockRef, FlatFilePos};
use dash_consensus::primitives::{compact_size_len, header_bytes};
use dash_consensus::{Block, BlockId, core_serialize, decode_block};
use dash_core_vectors::hex::parse_hex;
use dash_primitives::BlockHash;
use dashd_chainstate::flush::SubscriberHub;
use dashd_chainstate::store::{GlobalFailStop, GuardedEngine, RecordingSink};
use dashd_chainstate::subscriber::{
    BlockSource, ConnectedBlock, DriverConfig, ScriptedFeeds, SubStep, SubscriberData,
    SubscriberDriver, SubscriberError, SyncProgress, TreeIndex,
};
use dashd_indexes::txindex::codec::{self, TxKey, TxPos};
use dashd_indexes::txindex::data::SEQ_BITS;
use dashd_indexes::txindex::{StoredTxIndex, TX, TXINDEX_DB, TxFiles, TxIndexData};
use dashd_indexes::{IndexError, IndexStatus, IndexSummary, TxIndexReader, TxPositions, Txid};
use dashd_store::flatfile::{BlockFileReader, FlatFilePos as StorePos};
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

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn blk(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/blocks/blk00000.dat", fixture(name))).unwrap()
}

fn json(name: &str, file: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{}/{file}", fixture(name))).unwrap())
        .unwrap()
}

/// Every transaction of every block record: `read_tx` at its position is the matching slice of
/// `read_block`, and the reader's adapter returns it with the header's hash.
#[test]
fn read_tx_equals_the_read_block_slices() {
    for name in ["dashd-v24", "dashd-v24-reorg"] {
        let reader = BlockFileReader::open_read_only(format!("{}/blocks", fixture(name)));
        let bytes = blk(name);
        let mut read = 0;
        for (pos, _) in records(&bytes) {
            let mut record = Vec::new();
            reader.read_block(StorePos::new(0, pos.pos), &mut record).unwrap();
            let block = decode_block(&mut &record[..]).unwrap().into_value();
            let mut offset = compact_size_len(block.txs().len() as u64) as u32;
            for tx in block.txs() {
                let at = 80 + offset as usize;
                let slice = &record[at..at + tx.size() as usize];
                let (header, got) = reader.read_tx(StorePos::new(0, pos.pos), offset).unwrap();
                assert_eq!(header_bytes(header.raw())[..], record[..80], "{name}");
                assert_eq!(
                    core_serialize(&got),
                    slice,
                    "{name}: {} in {}",
                    tx.txid(),
                    block.hash()
                );
                let tx_pos = TxPos {
                    block: pos,
                    tx_offset: offset,
                };
                let (adapted, hash) = TxFiles::read_tx(&reader, &tx_pos).unwrap();
                assert_eq!((adapted.txid(), hash), (tx.txid(), block.hash()), "{name}");
                offset += tx.size();
                read += 1;
            }
        }
        assert!(read > 270, "{name}: {read} transactions");
    }
}

/// A recording's blocks on a test tree, by tree id.
struct Recorded(Vec<(FlatFilePos, Arc<Block>)>);

impl BlockSource for Recorded {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        Ok(Arc::clone(&self.0[b.id.0 as usize].1))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Err(SubscriberError::Blocks(format!("no undo for {}", b.hash)))
    }
}

impl TxPositions for Recorded {
    fn data_pos(&self, b: &BlockRef) -> Result<FlatFilePos, IndexError> {
        Ok(self.0[b.id.0 as usize].0)
    }
}

/// A recorded event: a connect of the tree block (with its recorded hash) or a disconnect.
enum Event {
    Connect(BlockRef, BlockHash),
    Disconnect,
}

struct Synced;

impl IndexStatus for Synced {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "txindex",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

type Db = GuardedEngine<MemoryEngine>;

/// The recorded event order through the real data object and driver, with the reader over the
/// recorded block files.
struct Replay {
    tree: TreeIndex,
    blocks: Arc<Recorded>,
    events: Vec<Event>,
    db: Arc<Db>,
    chain: Vec<BlockRef>,
    live: Option<(Driver, ScriptedFeeds)>,
    index: StoredTxIndex<Db>,
}

impl Replay {
    fn new(name: &str) -> Self {
        let bytes = blk(name);
        let mut by_hash: HashMap<BlockHash, (FlatFilePos, Arc<Block>)> = records(&bytes)
            .map(|(pos, b)| {
                let block = decode_block(&mut &b[..]).unwrap().into_value();
                (block.hash(), (pos, Arc::new(block)))
            })
            .collect();
        let chain = json(name, "chain.json");
        let genesis: BlockHash = chain["phases"][0]["tip"].as_str().unwrap().parse().unwrap();
        let tree = TreeIndex::new();
        let mut blocks = vec![by_hash.remove(&genesis).unwrap()];
        let mut refs = HashMap::from([(genesis, tree.genesis())]);
        let events = chain["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let Some(hash) = e.get("connect") else {
                    return Event::Disconnect;
                };
                let hash: BlockHash = hash.as_str().unwrap().parse().unwrap();
                if let Some(b) = refs.get(&hash) {
                    return Event::Connect(*b, hash);
                }
                let (pos, block) = by_hash.remove(&hash).unwrap();
                let b = tree.child(refs[&block.header().prev_hash()]);
                assert_eq!(b.id.0 as usize, blocks.len());
                blocks.push((pos, block));
                refs.insert(hash, b);
                Event::Connect(b, hash)
            })
            .collect();
        let db = Arc::new(GuardedEngine::new(
            MemoryEngine::new(&TXINDEX_DB),
            GlobalFailStop::new(),
            Arc::new(RecordingSink::default()),
        ));
        let files = Arc::new(BlockFileReader::open_read_only(format!("{}/blocks", fixture(name))));
        let mut replay = Self {
            chain: vec![tree.genesis()],
            tree,
            blocks: Arc::new(Recorded(blocks)),
            events,
            index: StoredTxIndex::new(Arc::clone(&db), files, Arc::new(Synced)),
            db,
            live: None,
        };
        replay.restart();
        replay
    }

    /// Drops the driver, then opens a new data object (a new epoch) and driver on the same store
    /// and V's current chain, and catches up.
    fn restart(&mut self) {
        self.live = None;
        let feeds = ScriptedFeeds::new(4, self.chain.clone());
        let hub = Arc::new(SubscriberHub::with_feeds(
            Arc::new(feeds.clone()),
            Arc::new(self.tree.clone()),
        ));
        let positions: Arc<dyn TxPositions> = self.blocks.clone();
        let data = TxIndexData::new(&*self.db, positions).unwrap();
        let cfg = DriverConfig {
            journal_budget: 1 << 20,
            unbounded_delete_span: 64,
            d_max: None,
            cadence: 1000,
        };
        let mut d =
            SubscriberDriver::open(data, Arc::clone(&self.db), hub, self.blocks.clone(), cfg)
                .expect("opens");
        for _ in 0..1000 {
            if d.sync_step().expect("syncs") == SyncProgress::Live {
                self.live = Some((d, feeds));
                return;
            }
        }
        panic!("never live");
    }

    fn connect(&mut self, b: BlockRef) {
        let (d, feeds) = self.live.as_mut().unwrap();
        feeds.connect(b);
        drain(d);
        self.chain.push(b);
    }

    fn disconnect(&mut self) {
        let (d, feeds) = self.live.as_mut().unwrap();
        feeds.disconnect();
        drain(d);
        self.chain.pop();
    }

    /// Disconnects the last `n` blocks and connects them again.
    fn redeliver(&mut self, n: usize) {
        let tail = self.chain[self.chain.len() - n..].to_vec();
        for _ in 0..n {
            self.disconnect();
        }
        for b in tail {
            self.connect(b);
        }
    }

    /// `find_tx` as the transaction's bytes and block, after checking that `has_tx` agrees.
    fn answer(&self, txid: &Txid) -> Option<(Vec<u8>, BlockHash)> {
        let found = self.index.find_tx(txid).unwrap();
        assert_eq!(self.index.has_tx(txid), Ok(found.is_some()), "has_tx({txid})");
        found.map(|(tx, hash)| (core_serialize(&*tx), hash))
    }
}

fn drain_events(replay: &mut Replay, range: std::ops::Range<usize>) {
    for i in range {
        match replay.events[i] {
            Event::Connect(b, _) => replay.connect(b),
            Event::Disconnect => replay.disconnect(),
        }
    }
}

/// Core's `getrawtransaction <txid>` answer in a recorded row: the transaction's bytes and block,
/// or `None` for an RPC error.
fn core_answer(row: &Value) -> Option<(Vec<u8>, BlockHash)> {
    let reply = row["without_blockhash"].get("result")?;
    let common = &row["common_result"];
    let hash = reply.get("blockhash").unwrap_or_else(|| &common["blockhash"]);
    Some((parse_hex(common["hex"].as_str().unwrap()), hash.as_str().unwrap().parse().unwrap()))
}

#[derive(Clone, Copy, Debug)]
enum Then {
    Nothing,
    Restart,
    Redeliver,
}

/// r1 M3's case (`dashd-v24-reorg`): T in A10; B forks below 9 with T in B9 and wins at 11; C,
/// without T, wins at 12. After each switch `find_tx(T)` is the recorded `getrawtransaction T`,
/// including Core's stale B9 once C has won; after each check, `then` happens and the answer
/// must not change.
fn two_reorgs(then: Then) {
    let mut replay = Replay::new("dashd-v24-reorg");
    let chain = json("dashd-v24-reorg", "chain.json");
    let t: Txid = chain["transaction_t"].as_str().unwrap().parse().unwrap();
    let mut done = 0;
    let mut answers = Vec::new();
    for phase in chain["phases"].as_array().unwrap() {
        let Some(row) = phase.get("transaction_t") else {
            continue;
        };
        let want = core_answer(row).unwrap();
        let tip: BlockHash = phase["tip"].as_str().unwrap().parse().unwrap();
        let name = phase["name"].as_str().unwrap();
        if name == "after_restart" {
            assert_eq!(done, replay.events.len(), "every event replayed");
            replay.restart();
        } else {
            let at = replay
                .events
                .iter()
                .position(|e| matches!(e, Event::Connect(_, h) if *h == tip))
                .unwrap();
            drain_events(&mut replay, done..at + 1);
            done = at + 1;
        }
        assert_eq!(replay.answer(&t).as_ref(), Some(&want), "{name}");
        match then {
            Then::Nothing => {}
            Then::Restart => replay.restart(),
            Then::Redeliver => replay.redeliver(3),
        }
        assert_eq!(replay.answer(&t).as_ref(), Some(&want), "{name}, then {then:?}");
        answers.push((name.to_owned(), want.1));
    }
    let names: Vec<_> = answers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["original_a10", "switched_to_b11", "switched_to_c12", "after_restart"]);
    assert_ne!(answers[0].1, answers[1].1, "B9 replaces A10");
    assert_eq!(answers[1].1, answers[2].1, "C keeps the stale B9");
    assert_eq!(answers[2].1, answers[3].1);
}

#[test]
fn two_reorgs_answer_as_core() {
    two_reorgs(Then::Nothing);
}

#[test]
fn two_reorgs_answer_as_core_across_restarts() {
    two_reorgs(Then::Restart);
}

#[test]
fn two_reorgs_answer_as_core_after_redelivery() {
    two_reorgs(Then::Redeliver);
}

/// DIFF vs g and g2: after the whole recorded event order, and again after a restart, `find_tx`
/// gives every recorded `getrawtransaction <txid>` answer's bytes and block; the genesis
/// coinbase, an RPC error in Core, is not indexed.
#[test]
fn every_recorded_getrawtransaction_matches() {
    for name in ["dashd-v24", "dashd-v24-reorg"] {
        let mut replay = Replay::new(name);
        let all = 0..replay.events.len();
        drain_events(&mut replay, all);
        let rows = json(name, "transactions.json");
        for restarted in [false, true] {
            if restarted {
                replay.restart();
            }
            let mut misses = 0;
            for row in rows.as_array().unwrap() {
                let txid: Txid = row["txid"].as_str().unwrap().parse().unwrap();
                let want = core_answer(row);
                misses += usize::from(want.is_none());
                assert_eq!(replay.answer(&txid), want, "{name} {txid}, restarted {restarted}");
            }
            assert_eq!(misses, 1, "{name}: only the genesis coinbase");
        }
    }
}
