//! The host over the scripted producer, the toy and the real `TxIndexData`, with the recording's
//! blocks as block data: one test per ported Core rule.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use std::{fs, thread};

use dash_chain::{BlockRef, FlatFilePos};
use dash_consensus::{Block, decode_block};
use dash_primitives::BlockHash;
use dashd_chainstate::flush::SubscriberHub;
use dashd_chainstate::store::{
    AbortMark, Failure, GlobalFailStop, GuardedEngine, ProbeEngine, RecordingSink,
};
use dashd_chainstate::subscriber::{
    BlockSource, DriverConfig, NoBlocks, RowSchema, ScriptedFeeds, SubscriberData, SubscriberError,
    TOY_DB, ToyIndex, TreeIndex, tree_hash,
};
use dashd_store::flatfile::BlockFileReader;
use dashd_store::{Commit, ErrorKind, KvBatch, KvRead, MemoryEngine};

use super::*;
use crate::txindex::codec::decode_epoch;
use crate::txindex::{
    BEST_BLOCK, EPOCH, META, StoredTxIndex, TXINDEX_DB, TXINDEX_ROWS, TxFiles, TxIndexData,
};
use crate::{IndexSummary, TxPositions};

const BLOCKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dashd-v24/blocks");
/// The recording's blocks in use: heights 0 to 11.
const HEIGHTS: usize = 12;

type Db = GuardedEngine<ProbeEngine>;

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn config() -> DriverConfig {
    DriverConfig {
        journal_budget: 1 << 20,
        unbounded_delete_span: 64,
        d_max: None,
        cadence: 1000,
    }
}

/// The recording's blocks by height, standing in for every tree block at that height, with a
/// hold and failures on block reads.
struct Source {
    blocks: Vec<(FlatFilePos, Arc<Block>)>,
    /// Reads at or above this height wait until it is lifted.
    hold: Mutex<Option<u32>>,
    lifted: Condvar,
    /// A read is waiting on the hold.
    held: Mutex<bool>,
    /// A read at this height fails, after tripping this latch if one is given.
    fail: Mutex<Option<(u32, Option<GlobalFailStop>)>>,
    /// A read at this height panics.
    panic_at: Mutex<Option<u32>>,
}

impl Source {
    fn new() -> Self {
        let bytes = fs::read(format!("{BLOCKS}/blk00000.dat")).unwrap();
        let mut blocks = Vec::new();
        let mut offset = 0;
        while blocks.len() < HEIGHTS {
            let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let start = offset + 8;
            let end = start + size as usize;
            let block = decode_block(&mut &bytes[start..end]).unwrap().into_value();
            let pos = FlatFilePos {
                file: 0,
                pos: start as u32,
            };
            blocks.push((pos, Arc::new(block)));
            offset = end;
        }
        Self {
            blocks,
            hold: Mutex::new(None),
            lifted: Condvar::new(),
            held: Mutex::new(false),
            fail: Mutex::new(None),
            panic_at: Mutex::new(None),
        }
    }

    fn hold_from(&self, h: u32) {
        *self.hold.lock().unwrap() = Some(h);
    }

    fn lift(&self) {
        *self.hold.lock().unwrap() = None;
        self.lifted.notify_all();
    }

    fn wait_held(&self) {
        wait_for("a held block read", || *self.held.lock().unwrap());
    }

    fn txids(&self, h: u32) -> Vec<Txid> {
        self.blocks[h as usize].1.txs().iter().map(|tx| tx.txid()).collect()
    }

    fn hash(&self, h: u32) -> BlockHash {
        self.blocks[h as usize].1.hash()
    }
}

impl BlockSource for Source {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        let mut hold = self.hold.lock().unwrap();
        while hold.is_some_and(|h| b.height >= h) {
            *self.held.lock().unwrap() = true;
            hold = self.lifted.wait(hold).unwrap();
        }
        drop(hold);
        *self.held.lock().unwrap() = false;
        let panic_at = *self.panic_at.lock().unwrap();
        assert_ne!(panic_at, Some(b.height), "a panicking block read");
        if let Some((h, trip)) = &*self.fail.lock().unwrap()
            && *h == b.height
        {
            if let Some(latch) = trip {
                latch.trip(Failure {
                    kind: ErrorKind::Io,
                    site: "elsewhere",
                    message: "another failure".into(),
                });
            }
            return Err(SubscriberError::Blocks(format!("no block at {}", b.height)));
        }
        Ok(Arc::clone(&self.blocks[b.height as usize].1))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Err(SubscriberError::Blocks(format!("no undo for {}", b.hash)))
    }
}

impl TxPositions for Source {
    fn data_pos(&self, b: &BlockRef) -> Result<FlatFilePos, IndexError> {
        Ok(self.blocks[b.height as usize].0)
    }
}

/// The txindex DB in its directory: it exists while the directory does. The test keeps its own
/// probe on the guarded engine's memory, for power losses and injected failures.
struct Disk {
    dir: PathBuf,
    db: Mutex<Option<(ProbeEngine, Arc<Db>)>>,
    latch: GlobalFailStop,
    sink: Arc<RecordingSink>,
}

impl Disk {
    fn open(&self, create: bool) -> Option<Arc<Db>> {
        let mut db = self.db.lock().unwrap();
        if !self.dir.exists() {
            if !create {
                return None;
            }
            fs::create_dir_all(&self.dir).unwrap();
            *db = None;
        }
        let (_, db) = db.get_or_insert_with(|| {
            let probe = ProbeEngine::new(&TXINDEX_DB);
            let sink = Arc::clone(&self.sink) as Arc<dyn AbortSink>;
            let db = Arc::new(GuardedEngine::new(probe.restart(), self.latch.clone(), sink));
            (probe, db)
        });
        Some(Arc::clone(db))
    }

    fn db(&self) -> Arc<Db> {
        self.open(false).expect("the DB exists")
    }

    fn memory(&self, f: impl FnOnce(&MemoryEngine)) {
        f(self.db.lock().unwrap().as_ref().expect("the DB exists").0.unprobed());
    }

    fn locator(&self) -> Option<BlockHash> {
        self.db().get(META, BEST_BLOCK).unwrap().as_deref().and_then(decode_locator)
    }

    /// The durable locator, as a power loss now leaves it.
    fn durable_locator(&self) -> Option<BlockHash> {
        self.memory(|m| m.simulate_power_loss(0));
        self.locator()
    }
}

struct TxSpec {
    disk: Arc<Disk>,
    source: Arc<Source>,
    extra: Vec<PathBuf>,
    opened: Arc<Mutex<Vec<&'static str>>>,
    at_engine: Option<Arc<AtEngine>>,
}

/// Whether the reader holds one, then its `find_tx` and `has_tx` answers.
type Seen = (bool, Result<Option<IndexedTx>, IndexError>, Result<bool, IndexError>);

/// What the host's reader answers as the engine opens, read in `engine()`.
#[derive(Default)]
struct AtEngine {
    reader: std::sync::OnceLock<Arc<IndexReader<dyn TxIndexReader>>>,
    txid: std::sync::OnceLock<Txid>,
    seen: Mutex<Option<Seen>>,
}

impl IndexSpec for TxSpec {
    type Data = TxIndexData<ProbeEngine>;
    type ReadOnly = Db;
    type Reader = dyn TxIndexReader;

    fn name(&self) -> &'static str {
        "txindex"
    }

    fn db_dir(&self) -> PathBuf {
        self.disk.dir.clone()
    }

    fn wipe_paths(&self) -> Vec<PathBuf> {
        self.extra.clone()
    }

    fn rows(&self) -> &'static RowSchema {
        &TXINDEX_ROWS
    }

    fn open_read_only(&self) -> Result<Option<Arc<Db>>, IndexError> {
        Ok(self.disk.open(false))
    }

    fn read_only_reader(
        &self,
        db: Arc<Db>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn TxIndexReader> {
        Arc::new(StoredTxIndex::new(db, files(), status))
    }

    fn engine(&self) -> Result<Arc<Db>, IndexError> {
        self.opened.lock().unwrap().push("txindex");
        if let Some(at) = &self.at_engine {
            let (r, txid) = (at.reader.get().unwrap(), at.txid.get().unwrap());
            *at.seen.lock().unwrap() =
                Some((r.current().is_some(), r.find_tx(txid), r.has_tx(txid)));
        }
        Ok(self.disk.open(true).expect("created"))
    }

    fn data(&self, engine: &Arc<Db>) -> Result<Self::Data, IndexError> {
        TxIndexData::new(&**engine, Arc::clone(&self.source) as Arc<dyn TxPositions>)
    }

    fn reader(&self, engine: Arc<Db>, status: Arc<dyn IndexStatus>) -> Arc<dyn TxIndexReader> {
        Arc::new(StoredTxIndex::new(engine, files(), status))
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::clone(&self.source) as Arc<dyn BlockSource>
    }

    fn config(&self) -> DriverConfig {
        config()
    }
}

fn files() -> Arc<dyn TxFiles> {
    Arc::new(BlockFileReader::open_read_only(BLOCKS))
}

/// The toy on its own in-memory engine, with no DB before open and its status as its reader.
struct ToySpec {
    name: &'static str,
    latch: GlobalFailStop,
    opened: Arc<Mutex<Vec<&'static str>>>,
}

impl IndexSpec for ToySpec {
    type Data = ToyIndex;
    type ReadOnly = Db;
    type Reader = dyn IndexStatus;

    fn name(&self) -> &'static str {
        self.name
    }

    fn db_dir(&self) -> PathBuf {
        PathBuf::from(self.name)
    }

    fn rows(&self) -> &'static RowSchema {
        ToyIndex::<ProbeEngine>::mandatory().schema()
    }

    fn open_read_only(&self) -> Result<Option<Arc<Db>>, IndexError> {
        Ok(None)
    }

    fn read_only_reader(&self, _: Arc<Db>, status: Arc<dyn IndexStatus>) -> Arc<dyn IndexStatus> {
        status
    }

    fn engine(&self) -> Result<Arc<Db>, IndexError> {
        self.opened.lock().unwrap().push(self.name);
        let sink = Arc::new(RecordingSink::default());
        Ok(Arc::new(GuardedEngine::new(ProbeEngine::new(&TOY_DB), self.latch.clone(), sink)))
    }

    fn data(&self, _: &Arc<Db>) -> Result<Self::Data, IndexError> {
        Ok(ToyIndex::mandatory())
    }

    fn reader(&self, _: Arc<Db>, status: Arc<dyn IndexStatus>) -> Arc<dyn IndexStatus> {
        status
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(NoBlocks)
    }

    fn config(&self) -> DriverConfig {
        config()
    }
}

/// The toy as a filter index with no DB, for the reads before open.
struct FilterSpec(ToySpec);

impl IndexSpec for FilterSpec {
    type Data = ToyIndex;
    type ReadOnly = Db;
    type Reader = dyn FilterIndexReader;

    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn db_dir(&self) -> PathBuf {
        self.0.db_dir()
    }

    fn rows(&self) -> &'static RowSchema {
        self.0.rows()
    }

    fn open_read_only(&self) -> Result<Option<Arc<Db>>, IndexError> {
        self.0.open_read_only()
    }

    fn read_only_reader(&self, _: Arc<Db>, _: Arc<dyn IndexStatus>) -> Arc<dyn FilterIndexReader> {
        unreachable!("no DB before open")
    }

    fn engine(&self) -> Result<Arc<Db>, IndexError> {
        self.0.engine()
    }

    fn data(&self, engine: &Arc<Db>) -> Result<Self::Data, IndexError> {
        self.0.data(engine)
    }

    fn reader(&self, _: Arc<Db>, _: Arc<dyn IndexStatus>) -> Arc<dyn FilterIndexReader> {
        unreachable!("the test never opens it")
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        self.0.blocks()
    }

    fn config(&self) -> DriverConfig {
        self.0.config()
    }
}

struct NoAncestry;

impl Ancestry for NoAncestry {
    fn ancestor(&self, _: &BlockRef, _: u32) -> Option<BlockRef> {
        None
    }
}

/// The block index, block data and index DB that outlive a node process.
struct World {
    _tmp: tempfile::TempDir,
    tree: TreeIndex,
    main: Vec<BlockRef>,
    source: Arc<Source>,
    disk: Arc<Disk>,
    latch: GlobalFailStop,
    sink: Arc<RecordingSink>,
    opened: Arc<Mutex<Vec<&'static str>>>,
}

/// One node process: V's feed, the hub after recovery's step 10 at V's tip, and the host.
struct Node {
    feeds: ScriptedFeeds,
    hub: Arc<SubscriberHub>,
    host: IndexHost,
    flushes: AtomicU64,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let tree = TreeIndex::new();
        let g = tree.genesis();
        let main = std::iter::once(g).chain(tree.branch(g, HEIGHTS - 1)).collect();
        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingSink::default());
        let disk = Arc::new(Disk {
            dir: tmp.path().join("indexes").join("txindex"),
            db: Mutex::new(None),
            latch: latch.clone(),
            sink: Arc::clone(&sink),
        });
        Self {
            _tmp: tmp,
            tree,
            main,
            source: Arc::new(Source::new()),
            disk,
            latch,
            sink,
            opened: Arc::default(),
        }
    }

    fn tx(&self) -> TxSpec {
        TxSpec {
            disk: Arc::clone(&self.disk),
            source: Arc::clone(&self.source),
            extra: Vec::new(),
            opened: Arc::clone(&self.opened),
            at_engine: None,
        }
    }

    /// A process whose V has `chain` (genesis first).
    fn node(&self, chain: &[BlockRef], wipe: bool) -> Node {
        let feeds = ScriptedFeeds::new(4, chain.to_vec());
        let hub = Arc::new(SubscriberHub::with_feeds(
            Arc::new(feeds.clone()),
            Arc::new(self.tree.clone()),
        ));
        hub.recovered(*chain.last().unwrap());
        let host =
            IndexHost::new(Arc::clone(&hub), Arc::clone(&self.sink) as Arc<dyn AbortSink>, wipe);
        Node {
            feeds,
            hub,
            host,
            flushes: AtomicU64::new(0),
        }
    }

    fn marks(&self) -> Vec<AbortMark> {
        self.sink.marks()
    }

    /// Every transaction of the block at height `h` is found, in the recorded block.
    fn indexed(&self, r: &dyn TxIndexReader, h: u32) -> bool {
        self.source.txids(h).iter().all(|txid| {
            r.find_tx(txid).unwrap().is_some_and(|(_, hash)| hash == self.source.hash(h))
        })
    }
}

impl Node {
    fn summary(&self, i: usize) -> IndexSummary {
        self.host.summaries()[i].clone()
    }

    fn wait_synced(&self, i: usize, height: u32) {
        wait_for(&format!("index {i} synced at {height}"), || {
            let s = self.summary(i);
            s.synced && s.best_block_height == height
        });
    }

    /// W's flush at V's tip, acked by every synced subscriber.
    fn flush(&self) {
        let id = self.flushes.fetch_add(1, Ordering::Relaxed) + 1;
        let tip = self.feeds.tip().unwrap();
        self.hub.publish_durable(id, tip, self.feeds.tip_seq());
        wait_for("the flush's acks", || self.hub.acks_done(id));
    }
}

fn summary(name: &'static str, synced: bool, best_block_height: u32) -> IndexSummary {
    IndexSummary {
        name,
        synced,
        best_block_height,
    }
}

fn locator_bytes(hash: &BlockHash) -> Vec<u8> {
    let mut v = 0i32.to_le_bytes().to_vec();
    v.push(1);
    v.extend_from_slice(hash.as_bytes());
    v
}

/// Core's Step 8 order is the caller's: the drivers open, and register with the hub, in the
/// order the indexes were registered, whatever the index type.
#[test]
fn indexes_open_in_registration_order() {
    let w = World::new();
    let mut n = w.node(&w.main[..=4], false);
    let toy = |name| ToySpec {
        name,
        latch: w.latch.clone(),
        opened: Arc::clone(&w.opened),
    };
    n.host.register(toy("toy-a")).unwrap();
    n.host.register(w.tx()).unwrap();
    n.host.register(toy("toy-b")).unwrap();
    let names: Vec<_> = n.host.summaries().iter().map(|s| s.name).collect();
    assert_eq!(names, ["toy-a", "txindex", "toy-b"]);
    n.host.open_all().unwrap();
    assert_eq!(*w.opened.lock().unwrap(), ["toy-a", "txindex", "toy-b"]);
    for i in 0..3 {
        n.wait_synced(i, 4);
    }
    n.host.stop();
}

/// Core's `GetSummary` after `Init`: before open the index is not synced, at its locator's height
/// (0 with no DB), and the read-only reader serves the retained rows. Open drops the read-only
/// reader for the driver's.
#[test]
fn before_open_the_summary_is_the_locators_and_retained_data_is_served() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    let r = n.host.register(w.tx()).unwrap();
    assert_eq!(n.summary(0), summary("txindex", false, 0));
    assert!(r.current().is_none());
    assert_eq!(r.find_tx(&w.source.txids(3)[0]), Ok(None));
    assert!(!r.block_until_synced());
    n.host.open_all().unwrap();
    n.wait_synced(0, 8);
    n.host.stop();

    let mut n = w.node(&w.main[..=8], false);
    let r = n.host.register(w.tx()).unwrap();
    assert_eq!(n.summary(0), summary("txindex", false, 8));
    assert!((1..=8).all(|h| w.indexed(&*r, h)));
    assert!(!r.block_until_synced());
    let read_only = Arc::downgrade(&r.current().unwrap());
    n.host.open_all().unwrap();
    assert!(read_only.upgrade().is_none(), "the read-only reader is dropped at open");
    n.wait_synced(0, 8);
    assert!((1..=8).all(|h| w.indexed(&*r, h)));
    n.host.stop();
}

/// e1 r1 m2: the read-only reader is dropped before the engine opens, and a read then gets the
/// no-DB answer, so none reaches the read-only handle while the engine may write.
#[test]
fn the_read_only_reader_is_dropped_before_the_engine_opens() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 8);
    n.host.stop();

    let mut n = w.node(&w.main[..=8], false);
    let at = Arc::new(AtEngine::default());
    let r = n
        .host
        .register(TxSpec {
            at_engine: Some(Arc::clone(&at)),
            ..w.tx()
        })
        .unwrap();
    let txid = w.source.txids(3)[0];
    assert!(w.indexed(&*r, 3), "the read-only reader serves the retained rows");
    assert!(at.reader.set(Arc::clone(&r)).is_ok() && at.txid.set(txid).is_ok());
    n.host.open_all().unwrap();
    let seen = at.seen.lock().unwrap().take().expect("engine() ran");
    assert_eq!(seen, (false, Ok(None), Ok(false)));
    n.wait_synced(0, 8);
    assert!(w.indexed(&*r, 3));
    n.host.stop();
}

/// No event is missed across open: blocks connected while the index opens and catches up, and
/// after it is live and idle, are all indexed (Core registers before `Init`, base.cpp:503-520).
#[test]
fn no_event_is_missed_across_open() {
    let w = World::new();
    let mut n = w.node(&w.main[..=3], false);
    let r = n.host.register(w.tx()).unwrap();
    let producer = {
        let feeds = n.feeds.clone();
        let blocks = w.main[4..=9].to_vec();
        thread::spawn(move || {
            for b in blocks {
                feeds.connect(b);
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    n.host.open_all().unwrap();
    producer.join().unwrap();
    n.wait_synced(0, 9);
    for b in &w.main[10..] {
        n.feeds.connect(*b);
    }
    assert!(r.block_until_synced());
    n.wait_synced(0, 11);
    assert!((1..=11).all(|h| w.indexed(&*r, h)));
    n.host.stop();
}

/// FR-IDX-06: an index enabled on a node already synced catches up from genesis, goes live, then
/// follows the tip.
#[test]
fn a_late_enable_catches_up_then_follows_the_tip() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    n.feeds.connect(w.main[9]);
    n.feeds.connect(w.main[10]);
    let r = n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 10);
    n.feeds.connect(w.main[11]);
    n.wait_synced(0, 11);
    assert!(r.block_until_synced());
    assert!((1..=11).all(|h| w.indexed(&*r, h)));
    n.host.stop();
}

/// `-reindex` removes the DB and the extra paths before anything opens: no retained data, and a
/// fresh index (epoch 1) from genesis.
#[test]
fn the_wipe_removes_the_db_and_extra_paths_before_open() {
    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 6);
    n.host.stop();
    let extra = w.disk.dir.with_file_name("fltr00000.dat");
    fs::write(&extra, b"filters").unwrap();
    let extra_dir = w.disk.dir.with_file_name("extra");
    fs::create_dir_all(extra_dir.join("nested")).unwrap();

    let mut n = w.node(&w.main[..=6], true);
    let mut spec = w.tx();
    spec.extra = vec![extra.clone(), extra_dir.clone(), w.disk.dir.with_file_name("absent")];
    let r = n.host.register(spec).unwrap();
    assert!(!w.disk.dir.exists() && !extra.exists() && !extra_dir.exists());
    assert_eq!(n.summary(0), summary("txindex", false, 0));
    assert!(r.current().is_none());
    n.host.open_all().unwrap();
    n.wait_synced(0, 6);
    let epoch = w.disk.db().get(META, EPOCH).unwrap().unwrap();
    assert_eq!(decode_epoch(&epoch), Ok(1), "a fresh DB");
    assert!((1..=6).all(|h| w.indexed(&*r, h)));
    n.host.stop();
}

/// Interrupt ends a catch-up at its next check, while a live index keeps stepping, so the
/// shutdown flush is acked; the stop's final commit is durable.
#[test]
fn interrupt_then_stop_makes_the_final_commit_durable() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 8);
    n.host.interrupt();
    n.feeds.connect(w.main[9]);
    n.wait_synced(0, 9);
    n.flush();
    n.host.stop();
    assert_eq!(w.disk.durable_locator(), Some(w.main[9].hash));

    // A catch-up interrupted at block 5 stops there, and the stop commits it.
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    n.host.register(w.tx()).unwrap();
    w.source.hold_from(5);
    n.host.open_all().unwrap();
    w.source.wait_held();
    n.host.interrupt();
    w.source.lift();
    wait_for("block 5", || n.summary(0).best_block_height == 5);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(n.summary(0), summary("txindex", false, 5));
    n.host.stop();
    assert_eq!(w.disk.durable_locator(), Some(w.main[5].hash));
}

/// The stop empties each reader after its final commit, so a handle a service still holds lets
/// the index's database close and answers as with no index (Core's `g_txindex.reset()`).
// Core: v24.0.0 src/init.cpp:421-424
#[test]
fn the_stop_empties_the_reader() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    let r = n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 8);
    assert!(w.indexed(&*r, 3));
    let engine_reader = Arc::downgrade(&r.current().expect("the engine's reader"));
    n.host.stop();
    assert!(r.current().is_none());
    assert!(engine_reader.upgrade().is_none(), "the handle keeps no reader alive");
    assert!(!w.indexed(&*r, 3));
}

/// An interrupted catch-up connects nothing more but answers a flush's rewind gate, so the
/// shutdown flush below its locator does not wait on it.
#[test]
fn an_interrupted_catch_up_answers_the_rewind_gate() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 8);
    n.host.stop();

    let mut n = w.node(&w.main[..=10], false);
    n.host.register(w.tx()).unwrap();
    w.source.hold_from(9);
    n.host.open_all().unwrap();
    w.source.wait_held();
    n.host.interrupt();
    w.source.lift();
    wait_for("the interrupted step", || n.summary(0).best_block_height == 9);
    let ids = n.hub.open_rewind_gate(1, w.main[6]);
    assert_eq!(ids.len(), 1, "the index's durable locator is above the gate");
    wait_for("the index's rewind", || n.hub.gate_done(1));
    assert_eq!(w.disk.locator(), Some(w.main[6].hash));
    assert_eq!(n.summary(0), summary("txindex", false, 6));
    n.host.stop();
    assert_eq!(w.disk.durable_locator(), Some(w.main[6].hash));
}

/// A restart mid catch-up resumes from the durable locator after a power loss.
#[test]
fn a_restart_mid_catch_up_resumes_from_the_locator() {
    let w = World::new();
    let mut n = w.node(&w.main[..=10], false);
    n.host.register(w.tx()).unwrap();
    w.source.hold_from(7);
    n.host.open_all().unwrap();
    w.source.wait_held();
    n.host.interrupt();
    w.source.lift();
    n.host.stop();
    assert_eq!(w.disk.durable_locator(), Some(w.main[7].hash));

    let mut n = w.node(&w.main[..=10], false);
    let r = n.host.register(w.tx()).unwrap();
    assert_eq!(n.summary(0), summary("txindex", false, 7));
    assert!((1..=7).all(|h| w.indexed(&*r, h)));
    n.host.open_all().unwrap();
    n.wait_synced(0, 10);
    assert!((1..=10).all(|h| w.indexed(&*r, h)));
    let epoch = w.disk.db().get(META, EPOCH).unwrap().unwrap();
    assert_eq!(decode_epoch(&epoch), Ok(2));
    n.host.stop();
}

/// Case (iii): a known locator off V's chain. Before open the summary is the locator's height and
/// its retained rows are read; the open rewinds it durably to the fork (D45), then it syncs the
/// new branch.
#[test]
fn a_known_locator_off_the_chain_rewinds_at_open() {
    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 6);
    n.host.stop();
    assert_eq!(w.disk.durable_locator(), Some(w.main[6].hash));

    let fork = w.main[3];
    let chain: Vec<_> = w.tree.path(fork).into_iter().chain(w.tree.branch(fork, 5)).collect();
    let mut n = w.node(&chain, false);
    let r = n.host.register(w.tx()).unwrap();
    assert_eq!(n.summary(0), summary("txindex", false, 6));
    assert!(w.indexed(&*r, 5), "a stale branch's retained rows");
    w.source.hold_from(4);
    n.host.open_all().unwrap();
    w.source.wait_held();
    assert_eq!(n.summary(0), summary("txindex", false, 3));
    assert_eq!(w.disk.locator(), Some(fork.hash), "the open's durable rewind commit");
    w.source.lift();
    n.wait_synced(0, 8);
    n.flush();
    n.host.stop();
    assert_eq!(w.disk.durable_locator(), Some(chain[8].hash));
}

/// Case (iv): a locator the block index does not know is refused at open with Core's `InitError`,
/// and nothing goes to the fail-stop sink.
#[test]
fn an_unknown_locator_is_refused_at_open() {
    let w = World::new();
    let db = w.disk.open(true).unwrap();
    let mut b = db.batch();
    b.put(META, BEST_BLOCK, &locator_bytes(&tree_hash(9999)));
    db.write(b, Commit::Durable).unwrap();
    let mut n = w.node(&w.main[..=4], false);
    n.host.register(w.tx()).unwrap();
    assert_eq!(n.summary(0), summary("txindex", false, 0));
    let e = n.host.open_all().unwrap_err();
    let HostError::Open(SubscriberError::Data(m)) = &e else {
        panic!("{e:?}");
    };
    assert_eq!(m, "txindex: best block of the index not found. Please rebuild the index.");
    assert!(w.marks().is_empty());
    assert!(w.latch.check().is_ok());
    n.host.stop();
}

/// Core's `BlockUntilSyncedToCurrentChain`: false while syncing; true at once when caught up;
/// otherwise it waits until the index has consumed the feed up to the tip of the call.
#[test]
fn block_until_synced_follows_cores_three_cases() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    let r = n.host.register(w.tx()).unwrap();
    w.source.hold_from(3);
    n.host.open_all().unwrap();
    w.source.wait_held();
    assert!(!r.block_until_synced(), "syncing");
    w.source.lift();
    n.wait_synced(0, 8);
    assert!(r.block_until_synced(), "caught up");

    w.source.hold_from(9);
    n.feeds.connect(w.main[9]);
    w.source.wait_held();
    let waiter = {
        let r = Arc::clone(&r);
        thread::spawn(move || r.block_until_synced())
    };
    thread::sleep(Duration::from_millis(50));
    assert!(!waiter.is_finished(), "waits for block 9's event");
    w.source.lift();
    assert!(waiter.join().unwrap());
    assert_eq!(n.summary(0), summary("txindex", true, 9));
    n.host.stop();
}

/// `consumed_through` and `consumed_tip` on the handle never wait: not on a thread held mid
/// catch-up, nor on one held mid step, nor on the progress lock the waiters use. They lag the tip
/// until the step ends, and across a reorg `consumed_tip` names the old branch's block at the
/// tip's height.
#[test]
fn consumed_through_never_blocks_and_may_lag_the_tip() {
    let w = World::new();
    let mut n = w.node(&w.main[..=8], false);
    let r = n.host.register(w.tx()).unwrap();
    assert_eq!(r.consumed_tip(), None, "before open");
    let read_while_locked = |r: &Arc<IndexReader<dyn TxIndexReader>>| {
        let _progress = r.state.lock();
        let reader = Arc::clone(r);
        let read = thread::spawn(move || (reader.consumed_through(), reader.consumed_tip()));
        wait_for("a read under the progress lock", || read.is_finished());
        read.join().unwrap()
    };
    let at = |b: BlockRef| Some((i32::try_from(b.height).unwrap(), b.hash));
    w.source.hold_from(3);
    n.host.open_all().unwrap();
    w.source.wait_held();
    assert_eq!(read_while_locked(&r), (0, at(w.main[2])), "mid catch-up");
    w.source.lift();
    n.wait_synced(0, 8);
    let live = r.consumed_through();
    assert_eq!(live, n.feeds.tip_seq());
    assert_eq!(r.consumed_tip(), at(w.main[8]));

    w.source.hold_from(9);
    n.feeds.connect(w.main[9]);
    w.source.wait_held();
    assert_eq!(read_while_locked(&r), (live, at(w.main[8])), "mid step: lags the tip");
    assert!(live < n.feeds.tip_seq());
    w.source.lift();
    wait_for("block 9's event", || r.consumed_through() == n.feeds.tip_seq());
    assert_eq!(r.consumed_tip(), at(w.main[9]));

    // A reorg to another block 9 behind a step held on block 10.
    w.source.hold_from(10);
    n.feeds.connect(w.main[10]);
    w.source.wait_held();
    n.feeds.disconnect();
    n.feeds.disconnect();
    let other = w.tree.child(w.main[8]);
    n.feeds.connect(other);
    let (seq, tip) = read_while_locked(&r);
    assert!(seq < n.feeds.tip_seq());
    assert_eq!(tip, at(w.main[9]), "the old branch's block at the tip's height");
    assert_ne!(tip, at(other));
    w.source.lift();
    wait_for("the reorg's events", || r.consumed_through() == n.feeds.tip_seq());
    assert_eq!(r.consumed_tip(), at(other));
    n.host.stop();
}

/// The host rule: a `Fatal` reaches the sink once, as `<name>: <message>`, and the index stops.
#[test]
fn a_fatal_reaches_the_sink_exactly_once() {
    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    let r = n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 6);
    w.disk.memory(|m| m.inject_failure(ErrorKind::Io));
    n.feeds.connect(w.main[7]);
    wait_for("the report", || !w.marks().is_empty());
    assert!(r.block_until_synced(), "a stopped index is not waited on");
    n.feeds.connect(w.main[8]);
    n.host.stop();
    let marks = w.marks();
    assert_eq!(marks.len(), 1, "{marks:?}");
    let AbortMark::AbortNode {
        log,
        user: None,
    } = &marks[0]
    else {
        panic!("{marks:?}");
    };
    assert!(log.starts_with("txindex: "), "{log}");
    assert_eq!(format!("txindex: {}", w.latch.tripped().unwrap().message), *log);
    assert_eq!(r.summary().best_block_height, 6);
}

/// The host rule: a `Refused` (a failure latched and reported elsewhere) reports nothing.
#[test]
fn a_refused_step_reports_nothing() {
    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    let r = n.host.register(w.tx()).unwrap();
    n.host.open_all().unwrap();
    n.wait_synced(0, 6);
    assert!(w.latch.trip(Failure {
        kind: ErrorKind::Io,
        site: "chainstate write",
        message: "W's failure".into(),
    }));
    n.feeds.connect(w.main[7]);
    assert!(r.block_until_synced(), "the index stopped at the refusal");
    n.host.stop();
    assert!(w.marks().is_empty(), "{:?}", w.marks());
    assert_eq!(r.summary().best_block_height, 6);
}

/// The host rule for any other error: it trips the latch at the index's site and is reported if
/// that trip latched; a lost trip reports nothing.
#[test]
fn another_error_is_reported_only_if_its_trip_latched() {
    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    n.host.register(w.tx()).unwrap();
    *w.source.fail.lock().unwrap() = Some((4, None));
    n.host.open_all().unwrap();
    wait_for("the report", || !w.marks().is_empty());
    n.host.stop();
    let failure = w.latch.tripped().unwrap();
    assert_eq!(failure.site, "txindex");
    assert_eq!(failure.message, "txindex: no block at 4");
    assert_eq!(
        w.marks(),
        [AbortMark::AbortNode {
            log: "txindex: no block at 4".into(),
            user: None,
        }]
    );

    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    n.host.register(w.tx()).unwrap();
    *w.source.fail.lock().unwrap() = Some((4, Some(w.latch.clone())));
    n.host.open_all().unwrap();
    wait_for("the index to stop", || {
        w.latch.tripped().is_some() && n.summary(0).best_block_height == 3
    });
    n.host.stop();
    assert_eq!(w.latch.tripped().unwrap().site, "elsewhere");
    assert!(w.latch.any_site(|s| s == "txindex"), "the host's trip lost");
    assert!(w.marks().is_empty(), "{:?}", w.marks());
}

/// A sink that records the hub's prune locks at each report.
struct HubSink {
    hub: Arc<SubscriberHub>,
    inner: RecordingSink,
    locks: Mutex<Vec<Vec<&'static str>>>,
}

/// The names of the hub's prune locks (Step 7's).
fn locks(hub: &SubscriberHub) -> Vec<&'static str> {
    let tip = hub.table().unwrap().snapshot().tip();
    hub.actual_locks(&tip).into_iter().map(|(name, _)| name).collect()
}

impl AbortSink for HubSink {
    fn abort_node(&self, log: &str, user: Option<&str>) {
        self.locks.lock().unwrap().push(locks(&self.hub));
        self.inner.abort_node(log, user);
    }

    fn read_failed(&self, what: &str) {
        self.inner.read_failed(what);
    }
}

/// A live index thread's panic is reported by the host rule while its driver is still in the
/// hub, holding its prune lock, and a waiter stops waiting on the thread.
#[test]
fn a_panicking_thread_is_reported_before_its_driver_leaves_the_hub() {
    let w = World::new();
    let n = w.node(&w.main[..=6], false);
    let sink = Arc::new(HubSink {
        hub: Arc::clone(&n.hub),
        inner: RecordingSink::default(),
        locks: Mutex::default(),
    });
    let mut host =
        IndexHost::new(Arc::clone(&n.hub), Arc::clone(&sink) as Arc<dyn AbortSink>, false);
    let r = host.register(w.tx()).unwrap();
    host.open_all().unwrap();
    wait_for("synced at 6", || r.summary() == summary("txindex", true, 6));
    *w.source.panic_at.lock().unwrap() = Some(7);
    n.feeds.connect(w.main[7]);
    wait_for("the report", || !sink.inner.marks().is_empty());
    assert!(r.block_until_synced(), "a panicked index is not waited on");
    assert_eq!(*sink.locks.lock().unwrap(), [["txindex"]]);
    assert_eq!(w.latch.tripped().unwrap().site, "txindex");
    let [
        AbortMark::AbortNode {
            log,
            user: None,
        },
    ] = &sink.inner.marks()[..]
    else {
        panic!("{:?}", sink.inner.marks());
    };
    assert!(log.starts_with("txindex: ") && log.contains("the index thread panicked"), "{log}");
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.stop()));
    assert!(stopped.is_err(), "the thread's panic");
    assert_eq!(locks(&n.hub), [""; 0]);
}

/// A thread's panic reaches the caller at the stop, after every other index has stopped.
#[test]
fn a_thread_panic_is_resumed_after_the_other_indexes_stop() {
    let w = World::new();
    let mut n = w.node(&w.main[..=6], false);
    n.host.register(w.tx()).unwrap();
    n.host
        .register(ToySpec {
            name: "toy",
            latch: GlobalFailStop::new(),
            opened: Arc::clone(&w.opened),
        })
        .unwrap();
    *w.source.panic_at.lock().unwrap() = Some(4);
    n.host.open_all().unwrap();
    wait_for("the report", || !w.marks().is_empty());
    assert_eq!(locks(&n.hub).len(), 1, "the toy runs on");
    let host = n.host;
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.stop()));
    assert!(stopped.is_err(), "the thread's panic");
    assert_eq!(locks(&n.hub), [""; 0], "the toy stopped before the panic resumed");
}

/// Core's `Start` runs `Init` on the caller's thread: on W's thread with a flush window open, the
/// hub refuses the open (D43) and `open_all` returns it, rather than wait on its own flush.
#[test]
fn open_all_during_the_callers_own_flush_is_refused_not_hung() {
    let w = World::new();
    let mut n = w.node(&w.main[..=4], false);
    n.host.register(w.tx()).unwrap();
    assert_eq!(n.hub.open_rewind_gate(1, w.main[4]), [], "a window with no targets");
    let (done, wait) = std::sync::mpsc::channel::<()>();
    let watchdog = {
        let hub = Arc::clone(&n.hub);
        thread::spawn(move || {
            let hung = matches!(
                wait.recv_timeout(Duration::from_secs(10)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            );
            if hung {
                hub.end_flush(1);
            }
            hung
        })
    };
    let r = n.host.open_all();
    let _ = done.send(());
    assert!(!watchdog.join().unwrap(), "open_all waited on its own flush window");
    let Err(e @ HostError::Open(SubscriberError::Data(m))) = &r else {
        panic!("{r:?}");
    };
    assert!(m.contains("WouldDeadlock"), "{m}");
    assert!(!e.reported());
    assert_eq!(n.summary(0), summary("txindex", false, 0));
    n.hub.end_flush(1);
    n.host.stop();
}

/// D39, D47: on a thread that steps a subscriber, `open_all` opens nothing and refuses.
#[test]
fn open_all_on_a_stepping_thread_is_refused() {
    let w = World::new();
    let mut n = w.node(&w.main[..=4], false);
    n.host.register(w.tx()).unwrap();
    let toy = ToySpec {
        name: "toy",
        latch: GlobalFailStop::new(),
        opened: Arc::new(Mutex::default()),
    };
    let engine = toy.engine().unwrap();
    let mut stepped = SubscriberDriver::open(
        toy.data(&engine).unwrap(),
        engine,
        Arc::clone(&n.hub),
        toy.blocks(),
        toy.config(),
    )
    .unwrap();
    stepped.sync_step().unwrap();
    let r = n.host.open_all();
    let Err(HostError::Open(SubscriberError::Data(m))) = &r else {
        panic!("{r:?}");
    };
    assert_eq!(*m, format!("txindex{STEPPING_THREAD}"));
    assert!(w.opened.lock().unwrap().is_empty(), "nothing opened");
    drop(stepped);
    n.host.stop();
}

/// Before open with no DB, each filter read misses.
#[test]
fn before_open_with_no_db_every_filter_read_misses() {
    let w = World::new();
    let mut n = w.node(&w.main[..=4], false);
    let r = n
        .host
        .register(FilterSpec(ToySpec {
            name: "basic block filter index",
            latch: w.latch.clone(),
            opened: Arc::clone(&w.opened),
        }))
        .unwrap();
    let stop = w.main[3];
    assert!(r.current().is_none());
    assert!(r.filter(&stop).unwrap().is_none());
    assert!(r.filter_header(&stop).unwrap().is_none());
    assert!(r.filter_range(1, &stop, &NoAncestry).unwrap().is_none());
    assert!(r.filter_hash_range(1, &stop, &NoAncestry).unwrap().is_none());
    assert_eq!(r.summary(), summary("basic block filter index", false, 0));
    n.host.stop();
}
