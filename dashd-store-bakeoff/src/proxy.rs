//! The DEC-43 proxy (CH2 §5.5, `CH2c-notes.md` D6–D11): one of ST1's traces through one arm, at a
//! coins share B with no mempool allowance.
//!
//! - `core`: the Core-model arm (D9, D10). Core's DIRTY/FRESH map, accounted by [`CoreUsage`] and
//!   flushed by Core's `FlushStateToDisk` arithmetic, in Core's order: the block index synced, the
//!   coins in the map's order under the head-blocks marker, the evo stand-in; then the map is
//!   reallocated. Every block boundary tops up a ballast of touched memory to Core's usage.
//! - `rdashd`: CH2's `CoinsCache`, a `BlockOverlay` per block, and `FlushControl` over W's
//!   `ThreadDriver` on the chainstate's `RocksEngine` (D11).
//!
//! Both arms write the same block-index record per block and run the same txindex-shaped
//! writer: 8 + 12 bytes per transaction in one batch per block on its own thread and engine, and
//! its locator after each flush. Core's locator is unsynced (`BaseIndex::Commit`'s `WriteBatch`,
//! `index/base.cpp:228`); ours is Durable, as `SubscriberDriver` commits it (DEC-166: a locator
//! never runs ahead of durable state), so that cost is ours. Each reports its time, V's stall, the flushes by cause,
//! the peak coins usage at block boundaries and the memory peaks.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dash_chain::IndexWrite;
use dash_consensus::BlockId;
use dash_consensus::coins::CoinRef;
use dash_consensus::primitives::BlockHash;
use dash_coreimport::trace::{Event, TraceBlock, TraceReader};
use dashd_chainstate::caches::{CacheSizes, IndexArgs};
use dashd_chainstate::coins::{
    BlockOverlay, BlockRef, CoinSrc, CoinsBudget, CoinsCache, CoinsCacheSizeState, OutPointKey,
    Ratio, Salt, core_size_arith,
};
use dashd_chainstate::flush::{
    CrashRoll, DATABASE_FLUSH_INTERVAL, DriverEvent, FlushAction, FlushCauses, FlushConfig,
    FlushControl, FlushCx, FlushDriver, FlushIndex, FlushMachine, FlushPolicy, FlushStateMode,
    NoEvo, NoPrune, NullObserver, NullSignals, SubscriberHub, ThreadDriver, WThread,
};
use dashd_chainstate::store::{Fatal, GlobalFailStop, GuardedEngine, GuardedFiles, RecordingSink};
use dashd_chainstate::tuning::{DEFAULT_DB_BATCH_SIZE, EngineTuning, RocksTuning, sub_batch};
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};
use dashd_store::{
    CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, RocksConfig, RocksEngine,
};
use serde_json::{Value, json};

use crate::cache::{DIRTY, FRESH, engine_key};
use crate::core_map::CoreMap;
use crate::stats::Histogram;
use crate::store::{BATCH_BYTES, EngineBudget, Level, LevelBatch, Res, SubBatch};

const MIB: u64 = 1 << 20;
/// About Core's `CDiskBlockIndex` without its hash (as `coins_flush`'s fixtures).
const RECORD_BYTES: usize = 120;
/// Mainnet's `pchMessageStart`: only the empty block files' configuration reads it.
const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];

/// A proxy run's settings.
pub struct ProxyConfig {
    /// B: the coins share, with no allowance.
    pub coins_share: u64,
    /// Stop after this many blocks (the fixed-overhead runs of D7); `None` for the whole trace.
    pub blocks: Option<u32>,
    /// Core's periodic flush (`DATABASE_FLUSH_INTERVAL` passed) after this height's step, in both
    /// arms (CH2c D43): a mid-run flush for a cell whose coins never reach LARGE.
    pub periodic_flush_height: Option<u32>,
}

/// `-dbcache=300` with txindex: the engines' shares (CH2 §5.5).
pub fn dec43_sizes() -> CacheSizes {
    CacheSizes::calculate(
        300,
        &IndexArgs {
            txindex: true,
            ..IndexArgs::default()
        },
    )
}

/// What both arms measure.
#[derive(Default)]
struct Measured {
    blocks: u64,
    spends: u64,
    creates: u64,
    txs: u64,
    connect_s: f64,
    /// V's waits for the flush (D8): Core's whole synchronous flush, our waits on W.
    v_stall_s: f64,
    /// V's whole time at its flush sites: as `v_stall_s` for Core; ours adds each site's decision,
    /// freeze and poll (reported).
    v_site_s: f64,
    v_stall: Histogram,
    /// Flushes by cause.
    flushes: std::collections::BTreeMap<&'static str, u64>,
    /// The largest coins usage at a block boundary: native Σ, or Core's `DynamicMemoryUsage`.
    peak_boundary_usage: u64,
    /// The same peak less R and the unbacked shard ceilings: the cache's allocations alone
    /// (`Usage::allocated`); Core's usage for the Core arm.
    peak_boundary_allocated: u64,
    /// Block boundaries whose usage was above B.
    boundaries_above_b: u64,
    misses: u64,
    last_height: u32,
}

impl Measured {
    fn boundary(&mut self, usage: u64, allocated: u64, b: u64) {
        self.peak_boundary_usage = self.peak_boundary_usage.max(usage);
        self.peak_boundary_allocated = self.peak_boundary_allocated.max(allocated);
        if usage > b {
            self.boundaries_above_b += 1;
        }
    }

    /// A site since `t`, of which V waited `waited` on W. D8 gates the waits; the Core arm, whose
    /// flushes are synchronous, waits for the whole of each.
    fn stall(&mut self, t: Instant, waited: Duration) {
        self.v_site_s += t.elapsed().as_secs_f64();
        self.v_stall_s += waited.as_secs_f64();
        self.v_stall.record(waited.as_nanos() as u64);
    }

    /// A block connected since `t0`: counted, and its transactions sent to the txindex.
    fn connected(
        &mut self,
        block: &TraceBlock,
        t0: Instant,
        txids: &mut Vec<[u8; 8]>,
        txindex: &TxIndex,
    ) {
        self.connect_s += t0.elapsed().as_secs_f64();
        self.blocks += 1;
        self.last_height = block.height;
        block_txids(block, txids);
        self.txs += txids.len() as u64;
        txindex.send(TxMsg::Block(block.height, std::mem::take(txids)));
    }

    /// The shutdown flush, begun at `t`, done: counted, then the txindex's locator and its end.
    fn shut_down(&mut self, t: Instant, waited: Duration, txindex: &mut TxIndex) -> Res<()> {
        self.stall(t, waited);
        *self.flushes.entry("always").or_default() += 1;
        txindex.send(TxMsg::Flushed(self.last_height));
        txindex.finish()
    }

    fn json(&self) -> Value {
        json!({
            "blocks": self.blocks,
            "spends": self.spends,
            "creates": self.creates,
            "txs": self.txs,
            "connect_s": self.connect_s,
            "v_stall_s": self.v_stall_s,
            "v_site_s": self.v_site_s,
            "v_stall_ms": {
                "count": self.v_stall.count(),
                "p50": self.v_stall.quantile(0.5) as f64 / 1e6,
                "p99": self.v_stall.quantile(0.99) as f64 / 1e6,
                "max": self.v_stall.quantile(1.0) as f64 / 1e6,
            },
            "flushes": self.flushes,
            "peak_boundary_usage": self.peak_boundary_usage,
            "peak_boundary_allocated": self.peak_boundary_allocated,
            "boundaries_above_b": self.boundaries_above_b,
            "misses": self.misses,
            "last_height": self.last_height,
        })
    }
}

/// The transactions of a block: the distinct txids of its creates (the trace keeps no others).
fn block_txids(block: &TraceBlock, out: &mut Vec<[u8; 8]>) {
    out.clear();
    for e in &block.events {
        if let Event::Create {
            key,
            ..
        } = e
        {
            let id: [u8; 8] = key[..8].try_into().expect("8 bytes");
            if out.last() != Some(&id) {
                out.push(id);
            }
        }
    }
}

/// A block's index record, the same bytes on both arms.
fn index_record(height: u32) -> Vec<u8> {
    let mut r = vec![0x5a; RECORD_BYTES];
    stamp_height(&mut r, height);
    r
}

/// Makes `record` [`index_record`]`(height)`.
fn stamp_height(record: &mut [u8], height: u32) {
    record[..4].copy_from_slice(&height.to_le_bytes());
}

/// The txindex-shaped writer's work.
enum TxMsg {
    /// One block: 8 + 12 bytes per transaction.
    Block(u32, Vec<[u8; 8]>),
    /// A flush ended at this height: the locator, synced.
    Flushed(u32),
}

/// A txindex-shaped writer on its own thread (D9, D11).
struct TxIndex {
    tx: Option<mpsc::Sender<TxMsg>>,
    handle: Option<JoinHandle<Res<()>>>,
}

/// One txindex engine: a batch per block, and a locator after each flush.
trait TxStore: Send + 'static {
    fn block(&mut self, height: u32, txids: &[[u8; 8]]) -> Res<()>;
    fn locator(&mut self, height: u32) -> Res<()>;
}

impl TxIndex {
    fn spawn(mut store: impl TxStore) -> Self {
        let (tx, rx) = mpsc::channel::<TxMsg>();
        let handle = std::thread::Builder::new()
            .name("txindex".into())
            .spawn(move || {
                for m in rx {
                    match m {
                        TxMsg::Block(h, ids) => store.block(h, &ids)?,
                        TxMsg::Flushed(h) => store.locator(h)?,
                    }
                }
                Ok(())
            })
            .expect("the txindex thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }

    fn send(&self, m: TxMsg) {
        // A failed writer is reported at `finish`.
        let _ = self.tx.as_ref().expect("open").send(m);
    }

    /// Drains the queue and stops the thread.
    fn finish(&mut self) -> Res<()> {
        drop(self.tx.take());
        self.handle
            .take()
            .map_or(Ok(()), |h| h.join().map_err(|_| "txindex panicked".to_string())?)
    }
}

fn tx_value(height: u32, i: usize) -> [u8; 12] {
    let mut v = [0; 12];
    v[..4].copy_from_slice(&height.to_le_bytes());
    v[4..8].copy_from_slice(&(i as u32).to_le_bytes());
    v
}

// ---- The Core-model arm ----

// SAFETY: LevelDB's `DB` is safe for concurrent use; each `Level` is used by one thread at a time.
unsafe impl Send for Level {}

struct LevelTx(Level);

impl TxStore for LevelTx {
    fn block(&mut self, height: u32, txids: &[[u8; 8]]) -> Res<()> {
        let mut b = LevelBatch::new();
        for (i, id) in txids.iter().enumerate() {
            b.op(false, id, Some(&tx_value(height, i)));
        }
        self.0.write(b, false)
    }

    fn locator(&mut self, height: u32) -> Res<()> {
        // `BaseIndex::Commit`: CDBWrapper::WriteBatch without sync.
        let mut b = LevelBatch::new();
        b.op(true, b"best", Some(&height.to_le_bytes()));
        self.0.write(b, false)
    }
}

/// Touched anonymous memory standing for Core's script heap, which the map keeps inline (D10).
#[derive(Default)]
struct Ballast(Vec<u8>);

impl Ballast {
    /// Grows to `bytes`, touching each new page; it never shrinks before `release`, as Core's
    /// freed scripts stay with the allocator.
    fn reach(&mut self, bytes: usize) {
        if bytes > self.0.len() {
            let from = self.0.len();
            self.0.resize(bytes, 0);
            for i in (from..bytes).step_by(4096) {
                // A volatile write, so the page is touched.
                unsafe { std::ptr::write_volatile(self.0.as_mut_ptr().add(i), 1) };
            }
        }
    }

    fn release(&mut self) {
        self.0 = Vec::new();
    }
}

/// The Core-model arm's engines (D9).
struct CoreStores {
    chainstate: Level,
    index: Level,
    evo: Level,
}

impl CoreStores {
    /// Core's `Flush()`: the block index synced, the coins under the head-blocks marker in the
    /// map's order, the evo stand-in; then `ReallocateCache`.
    fn flush(
        &self,
        map: &mut CoreMap,
        dirty_blocks: &mut Vec<u32>,
        tip: u32,
        old_tip: u32,
    ) -> Res<()> {
        let mut b = LevelBatch::new();
        for h in dirty_blocks.drain(..) {
            b.op(false, &h.to_be_bytes(), Some(&index_record(h)));
        }
        self.index.write(b, true)?;
        let mut heads = tip.to_le_bytes().to_vec();
        heads.extend(old_tip.to_le_bytes());
        let mut batch = LevelBatch::new();
        batch.op(true, b"tip", None);
        batch.op(true, b"head_blocks", Some(&heads));
        let mut failed = Ok(());
        map.for_each(|k, flags, coin| {
            if flags & DIRTY == 0 || failed.is_err() {
                return;
            }
            batch.op(false, engine_key(k).as_slice(), coin);
            if batch.size() > BATCH_BYTES {
                failed =
                    self.chainstate.write(std::mem::replace(&mut batch, LevelBatch::new()), false);
            }
        });
        failed?;
        batch.op(true, b"head_blocks", None);
        batch.op(true, b"tip", Some(&tip.to_le_bytes()));
        self.chainstate.write(batch, false)?;
        let mut e = LevelBatch::new();
        e.op(true, b"best", Some(&tip.to_le_bytes()));
        self.evo.write(e, false)?;
        map.reallocate();
        Ok(())
    }
}

/// `FRESH` where `fresh`, else no flag.
fn fresh_flag(fresh: bool) -> u8 {
    if fresh {
        FRESH
    } else {
        0
    }
}

/// The child view's `Flush()`: `CCoinsViewCache::BatchWrite` of its DIRTY entries into the tip
/// (`coins.cpp:196-249`), then the child is dropped.
fn batch_write(child: &mut CoreMap, tip: &mut CoreMap) -> Res<()> {
    let mut failed = Ok(());
    child.for_each(|k, flags, coin| {
        if flags & DIRTY == 0 || failed.is_err() {
            return;
        }
        let fresh = flags & FRESH != 0;
        match tip.get(k) {
            None if fresh && coin.is_none() => {}
            None => tip.insert(*k, DIRTY | fresh_flag(fresh), coin),
            Some((_, Some(_))) if fresh => failed = Err("FRESH flag misapplied".to_string()),
            Some((tip_flags, _)) if tip_flags & FRESH != 0 && coin.is_none() => tip.remove(k),
            Some((tip_flags, _)) => tip.update(k, tip_flags | DIRTY, coin),
        }
    });
    child.reallocate();
    failed
}

pub fn run_core(trace: &Path, dir: &Path, cfg: &ProxyConfig) -> Res<Value> {
    let sizes = dec43_sizes();
    let open = |name: &str, cache: u64| {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        Level::open(
            &d,
            EngineBudget {
                block_cache: 0,
                total: cache as usize,
            },
        )
    };
    // Core: init.cpp: the chainstate at nCoinDBCache, the block tree at nBlockTreeDBCache, evodb
    // at 64 MiB, txindex at nTxIndexCache.
    let stores = CoreStores {
        chainstate: open("chainstate", sizes.coins_db)?,
        index: open("blocks-index", sizes.block_tree_db)?,
        evo: open("evodb", 64 * MIB)?,
    };
    let mut txindex = TxIndex::spawn(LevelTx(open("txindex", sizes.tx_index)?));
    let b = cfg.coins_share;

    let mut reader = TraceReader::open(trace).map_err(|e| e.to_string())?;
    let mut block = TraceBlock::default();
    let mut m = Measured::default();
    let mut tip = CoreMap::new();
    let mut child = CoreMap::new();
    let mut ballast = Ballast::default();
    let mut dirty_blocks: Vec<u32> = Vec::new();
    let mut txids = Vec::new();
    let mut value = Vec::with_capacity(64);
    let mut last_flush = 0u32;
    let mut peak_ballast = 0u64;
    let started = Instant::now();

    while reader.next_block(&mut block).map_err(|e| e.to_string())? {
        if cfg.blocks.is_some_and(|n| m.blocks >= u64::from(n)) {
            break;
        }
        let t0 = Instant::now();
        // ConnectBlock on a child view of the tip, then its Flush into the tip (BatchWrite).
        for e in &block.events {
            match e {
                Event::Spend(k) => {
                    m.spends += 1;
                    if child.get(k).is_none() {
                        // The child's FetchCoin through the tip's: a tip miss reads the database
                        // and caches the coin in the tip, clean.
                        let coin = match tip.get(k) {
                            Some((_, Some(c))) => c.to_vec(),
                            Some((_, None)) => return Err("spent in the tip".into()),
                            None => {
                                m.misses += 1;
                                if !stores
                                    .chainstate
                                    .get_coin(engine_key(k).as_slice(), &mut value)?
                                {
                                    return Err(format!("height {}: coin missing", block.height));
                                }
                                tip.insert(*k, 0, Some(&value));
                                value.clone()
                            }
                        };
                        child.insert(*k, 0, Some(&coin));
                    }
                    // SpendCoin in the child.
                    match child.get(k) {
                        Some((_, None)) => return Err("double spend".into()),
                        Some((flags, _)) if flags & FRESH != 0 => child.remove(k),
                        _ => child.update(k, DIRTY, None),
                    }
                }
                Event::Create {
                    key: k,
                    coin_start,
                    coin_len,
                } => {
                    m.creates += 1;
                    let coin = block.coin(*coin_start, *coin_len);
                    // AddCoin(possible_overwrite = coinbase): FRESH unless a coinbase output, or
                    // over a DIRTY entry.
                    let coinbase = CoinRef::decode(&mut &coin[..])
                        .map_err(|_| "undecodable coin")?
                        .is_coinbase();
                    match child.get(k) {
                        Some((_, Some(_))) if !coinbase => return Err("overwrites a coin".into()),
                        Some((flags, _)) => {
                            let fresh = !coinbase && flags & DIRTY == 0;
                            child.update(k, flags | DIRTY | fresh_flag(fresh), Some(coin));
                        }
                        None => child.insert(*k, DIRTY | fresh_flag(!coinbase), Some(coin)),
                    }
                }
            }
        }
        batch_write(&mut child, &mut tip)?;
        m.connected(&block, t0, &mut txids, &txindex);
        dirty_blocks.push(block.height);
        // UpdateTip's `cache=`: Core's usage at the boundary, the scripts' heap held as touched
        // memory beside the map.
        let u = tip.usage().dynamic_usage();
        m.boundary(u, u, b);
        let extra = u.saturating_sub(tip.bytes());
        ballast.reach(extra as usize);
        peak_ballast = peak_ballast.max(extra);
        // FlushStateToDisk(PERIODIC) after the step: a full flush at LARGE, or when the flush
        // interval has passed.
        let state = core_size_arith(u, b);
        let periodic = cfg.periodic_flush_height == Some(block.height);
        if state != CoinsCacheSizeState::Ok || periodic {
            let t = Instant::now();
            stores.flush(&mut tip, &mut dirty_blocks, block.height, last_flush)?;
            ballast.release();
            m.stall(t, t.elapsed());
            let cause = match state {
                CoinsCacheSizeState::Critical => "critical",
                CoinsCacheSizeState::Large => "large",
                CoinsCacheSizeState::Ok => "periodic",
            };
            *m.flushes.entry(cause).or_default() += 1;
            txindex.send(TxMsg::Flushed(block.height));
            last_flush = block.height;
        }
    }
    // The shutdown flush (ALWAYS).
    let t = Instant::now();
    stores.flush(&mut tip, &mut dirty_blocks, m.last_height, last_flush)?;
    ballast.release();
    m.shut_down(t, t.elapsed(), &mut txindex)?;
    let replay_s = started.elapsed().as_secs_f64();
    // Compaction debt is paid inside the time: until LevelDB's statistics last changed.
    let quiesce_s = [&stores.chainstate, &stores.index, &stores.evo]
        .into_iter()
        .map(level_settle)
        .fold(0f64, f64::max);
    let stats = stores.chainstate.property("leveldb.stats");
    let t = Instant::now();
    drop(stores);
    let close_s = t.elapsed().as_secs_f64();
    Ok(json!({
        "arm": "core",
        "coins_share": b,
        "measured": m.json(),
        "peak_ballast": peak_ballast,
        "time_replay_s": replay_s,
        "quiesce_s": quiesce_s,
        "close_s": close_s,
        "time_total_s": replay_s + quiesce_s + close_s,
        "engine": { "chainstate_stats": stats },
    }))
}

/// The time until LevelDB's compaction statistics last changed, polled for 2 s of stability.
fn level_settle(db: &Level) -> f64 {
    let start = Instant::now();
    let mut last = db.property("leveldb.stats");
    let mut changed = 0f64;
    let mut stable = Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let now = db.property("leveldb.stats");
        if now != last {
            last = now;
            changed = start.elapsed().as_secs_f64();
            stable = Instant::now();
        } else if stable.elapsed().as_secs_f64() >= 2.0 {
            return changed;
        }
    }
}

// ---- Our arm ----

static TXINDEX_SCHEMA: DbSchema = DbSchema {
    name: "txindex-shape",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("tx", CfProfile::PointLookup)],
};
const TX_CF: dashd_store::Cf = dashd_store::Cf::new(1);

struct RocksTx(RocksEngine);

impl TxStore for RocksTx {
    fn block(&mut self, height: u32, txids: &[[u8; 8]]) -> Res<()> {
        let mut b = self.0.batch();
        for (i, id) in txids.iter().enumerate() {
            b.put(TX_CF, id, &tx_value(height, i));
        }
        self.0.write(b, Commit::Buffered).map_err(|e| e.to_string())
    }

    fn locator(&mut self, height: u32) -> Res<()> {
        let mut b = self.0.batch();
        b.put(DbSchema::META, b"best", &height.to_le_bytes());
        self.0.write(b, Commit::Durable).map_err(|e| e.to_string())
    }
}

/// A driver that times V's waits on W: D8's V stall for our arm.
struct TimedDriver<D> {
    inner: D,
    waited: Duration,
}

impl<D: FlushDriver> FlushDriver for TimedDriver<D> {
    type Delta = D::Delta;

    fn submit(&mut self, job: FlushMachine<Self::Delta>) {
        self.inner.submit(job);
    }

    fn poll(&mut self) -> Option<DriverEvent> {
        self.inner.poll()
    }

    fn wait(&mut self) -> Result<DriverEvent, Fatal> {
        let t = Instant::now();
        let r = self.inner.wait();
        self.waited += t.elapsed();
        r
    }

    fn wait_pumped(&mut self, pump: &mut dyn FnMut()) -> Result<DriverEvent, Fatal> {
        let t = Instant::now();
        let r = self.inner.wait_pumped(pump);
        self.waited += t.elapsed();
        r
    }
}

/// The arm's dirty block index: heights, encoded at the flush that takes them, as the node's
/// `take_dirty` does (`dash-chain`'s `Chain::take_dirty`, Core's `m_dirty_blockindex`).
#[derive(Default)]
struct DirtyHeights(Vec<u32>);

impl FlushIndex for DirtyHeights {
    fn take_dirty_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8])) {
        let mut record = index_record(0);
        for h in self.0.drain(..) {
            stamp_height(&mut record, h);
            put(&block_ref(h).hash, &record);
        }
    }

    fn prune_clear(&mut self, _: &[u32]) -> Result<Vec<IndexWrite>, String> {
        Ok(Vec::new())
    }
}

/// A trace block's ref: its hash derived from its height.
fn block_ref(height: u32) -> BlockRef {
    let mut hash = [0x5c; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

fn cause_names(c: FlushCauses) -> Vec<&'static str> {
    let names = [
        (FlushCauses::ALWAYS, "always"),
        (FlushCauses::CACHE_LARGE, "large"),
        (FlushCauses::CACHE_CRITICAL, "critical"),
        (FlushCauses::EVO_CRITICAL, "evo_critical"),
        (FlushCauses::PERIODIC_FLUSH, "periodic"),
        (FlushCauses::PRUNE, "prune"),
        (FlushCauses::THRESHOLD, "threshold"),
        (FlushCauses::DMAX, "dmax"),
        (FlushCauses::LEFT_IBD, "left_ibd"),
        (FlushCauses::SCRIPTED, "scripted"),
    ];
    names.iter().filter(|(f, _)| c.contains(*f)).map(|(_, n)| *n).collect()
}

/// Our arm (`rdashd`) with `tuning`: CH2's shipped one, D15's split since DEC-383 A3.
pub fn run_rdashd<T: EngineTuning<E = RocksEngine> + 'static>(
    trace: &Path,
    dir: &Path,
    cfg: &ProxyConfig,
    tuning: Arc<T>,
    arm: &str,
) -> Res<Value> {
    let mut sizes = dec43_sizes();
    sizes.coins = cfg.coins_share;
    let sub = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
    let blocks_dir = dir.join("blocks");
    std::fs::create_dir_all(&blocks_dir).map_err(|e| e.to_string())?;
    let (engine, _) =
        tuning.open_chainstate(&dir.join("chainstate"), &sizes, sub).map_err(|e| e.to_string())?;
    let db = Arc::new(GuardedEngine::new(
        engine,
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    ));
    let fs = Arc::new(OsFs::new());
    let files = Arc::new(GuardedFiles::new(
        BlockFiles::with_fs(
            fs.clone(),
            BlockFilesConfig::new(&blocks_dir, MAGIC),
            BlockFilesState::default(),
        ),
        db.latch().clone(),
    ));
    let hub = Arc::new(SubscriberHub::default());
    let driver = ThreadDriver::spawn(WThread {
        engine: db.clone(),
        files: files.clone(),
        fs: fs.clone(),
        hub: hub.clone(),
        obs: Arc::new(NullObserver),
        tuning: tuning.clone(),
        cfg: FlushConfig {
            sub_batch: sub,
            data_dir: dir.to_path_buf(),
            blocks_dir: blocks_dir.clone(),
            live_copy: false,
        },
        covered_seq: 0,
        crash: CrashRoll::seeded(0, 0),
        on_crash: Box::new(|| {}),
    })
    .map_err(|e| e.to_string())?;
    let mut ctl = FlushControl::new(
        TimedDriver {
            inner: driver,
            waited: Duration::ZERO,
        },
        FlushPolicy::new(false),
        Arc::new(NullSignals),
        Arc::default(),
        Arc::new(NullObserver),
    );
    ctl.set_hub(hub.clone());
    let tx_dir = dir.join("txindex");
    let (tx_engine, _) = RocksEngine::open(
        &tx_dir,
        &TXINDEX_SCHEMA,
        // nTxIndexCache spent as Core's `CDBWrapper` spends it (`dbwrapper.cpp`): half as the
        // block cache, a quarter as each of the two memtables. The txindex is a stand-in (D11),
        // the same budget in both arms.
        &RocksConfig {
            block_cache_bytes: (sizes.tx_index / 2) as usize,
            write_buffer_bytes: (sizes.tx_index / 4) as usize,
            max_write_buffers: 2,
            total_write_buffer_bytes: (sizes.tx_index / 2) as usize,
            ..RocksConfig::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let mut txindex = TxIndex::spawn(RocksTx(tx_engine));
    let budget = CoinsBudget::for_engine(&sizes, 0, Ratio::HALF, sub, &*tuning);
    let mut coins = CoinsCache::new(db.clone(), budget, Salt::random(), None);
    let b = cfg.coins_share;

    let mut reader = TraceReader::open(trace).map_err(|e| e.to_string())?;
    let mut block = TraceBlock::default();
    let mut m = Measured::default();
    let mut dirty = DirtyHeights::default();
    let mut txids = Vec::new();
    let mut tip: Option<BlockRef> = None;
    let mut seq = 0u64;
    let started = Instant::now();
    let mut now = Instant::now();
    macro_rules! cx {
        ($tip:expr) => {
            FlushCx {
                now,
                coins: &mut coins,
                mempool_usage: 0,
                tip: $tip,
                last_seq: seq,
                index: &mut dirty,
                evo: &mut NoEvo,
                prune: &mut NoPrune,
                manual_prune_height: None,
                reindex: false,
                is_ibd: true,
                left_ibd: false,
                scripted: false,
            }
        };
    }

    let mut created = std::collections::HashSet::new();
    while reader.next_block(&mut block).map_err(|e| e.to_string())? {
        if cfg.blocks.is_some_and(|n| m.blocks >= u64::from(n)) {
            break;
        }
        let here = block_ref(block.height);
        // Core's misses: a spend whose coin is neither in the tip's cache nor created earlier in
        // the block reads the database (`FetchCoin`). Counted before the timed connect.
        created.clear();
        for e in &block.events {
            match e {
                Event::Spend(k) => {
                    let k = OutPointKey(*k);
                    if !created.contains(&k) && !coins.have_in_cache(&k) {
                        m.misses += 1;
                    }
                }
                Event::Create {
                    key,
                    ..
                } => {
                    created.insert(OutPointKey(*key));
                }
            }
        }
        let t0 = Instant::now();
        let mut o = BlockOverlay::new(&mut coins);
        for e in &block.events {
            match e {
                Event::Spend(k) => {
                    m.spends += 1;
                    o.spend(&OutPointKey(*k))
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("height {}: coin missing", block.height))?;
                }
                Event::Create {
                    key: k,
                    coin_start,
                    coin_len,
                } => {
                    m.creates += 1;
                    let c = CoinRef::decode(&mut block.coin(*coin_start, *coin_len))
                        .map_err(|_| "undecodable coin")?;
                    o.add(&OutPointKey(*k), CoinSrc::Bytes(c), c.is_coinbase())
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        o.commit(here).map_err(|e| e.to_string())?;
        m.connected(&block, t0, &mut txids, &txindex);
        dirty.0.push(block.height);
        // Core's IF_NEEDED site around the tip step, then V's poll (as `coins_flush`'s
        // `site_and_step`), with W on its own thread. The site's state is at the old tip.
        let t = Instant::now();
        let w0 = ctl.driver().waited;
        let site_tip = tip.unwrap_or_else(|| block_ref(block.height.saturating_sub(1)));
        // Core's PERIODIC call after the step, with the flush interval passed: the clock moves on.
        let mode = if cfg.periodic_flush_height == Some(block.height) {
            now += DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
            FlushStateMode::Periodic
        } else {
            FlushStateMode::IfNeeded
        };
        let pending = ctl.at_site(mode, &mut cx!(site_tip)).map_err(|e| e.0.message.to_string())?;
        let froze = match pending.action() {
            FlushAction::Freeze {
                causes,
                ..
            } => Some(*causes),
            _ => None,
        };
        tip = Some(here);
        seq += 1;
        pending
            .freeze_after_step(&mut ctl, &mut cx!(here), here, seq)
            .map_err(|e| e.0.message.to_string())?;
        ctl.poll(&mut cx!(here)).map_err(|e| e.0.message.to_string())?;
        if let Some(causes) = froze {
            for n in cause_names(causes) {
                *m.flushes.entry(n).or_default() += 1;
            }
            txindex.send(TxMsg::Flushed(block.height));
        } else {
            coins.boundary_rebuild();
        }
        m.stall(t, ctl.driver().waited - w0);
        let u = coins.usage();
        m.boundary(u.total(), u.allocated(), b);
    }
    // The shutdown flush: ALWAYS, waited for, then W's last step.
    let t = Instant::now();
    let w0 = ctl.driver().waited;
    if let Some(here) = tip {
        ctl.flush(FlushStateMode::Always, &mut cx!(here)).map_err(|e| e.0.message.to_string())?;
        ctl.stop(&mut cx!(here)).map_err(|e| e.0.message.to_string())?;
    }
    let waited = ctl.driver().waited - w0;
    m.shut_down(t, waited, &mut txindex)?;
    let replay_s = started.elapsed().as_secs_f64();
    let t = Instant::now();
    drop(ctl);
    RocksTuning::wait_idle(&db).map_err(|e| e.to_string())?;
    let quiesce_s = t.elapsed().as_secs_f64();
    let memory =
        tuning.memory_report(&db).0.into_iter().collect::<std::collections::BTreeMap<_, _>>();
    let t = Instant::now();
    drop(coins);
    drop(files);
    drop(Arc::into_inner(db).ok_or("the database is still shared")?);
    let close_s = t.elapsed().as_secs_f64();
    Ok(json!({
        "arm": arm,
        "coins_share": b,
        "measured": m.json(),
        "time_replay_s": replay_s,
        "quiesce_s": quiesce_s,
        "close_s": close_s,
        "time_total_s": replay_s + quiesce_s + close_s,
        "engine_memory": memory,
    }))
}
