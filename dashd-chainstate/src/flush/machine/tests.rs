//! W's flush on the probe engine and real block files (CH2 §4.2, §4.6; E-b1.8): what each step
//! writes, the split flush's marker, the barriers, the space checks, the crash roll, a failed
//! write, the tuning's hook, the copy point's order, and step 7's prune with its clears, Bp and
//! unlink.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use dash_chain::{BlockRef, IndexWrite};
use dash_consensus::BlockId;
use dash_consensus::primitives::{BlockHash, TxOut};
use dashd_store::flatfile::{
    BlockFileInfo, BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs, FlatRead,
    MIN_BLOCKS_TO_KEEP, MIN_DISK_SPACE, OsFs,
};
use dashd_store::{ErrorKind, KvEngine, KvRead, MemoryEngine, SchemaState, StoreError};

use super::*;
use crate::caches::CacheSizes;
use crate::coins::model::db_coins;
use crate::coins::{CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt};
use crate::flush::test_files::{blk_on_disk, create_on_disk, equal_files};
use crate::flush::{FlushCauses, FlushStateMode, PruneRule, RecordingObserver};
use crate::store::{Meta, ProbeEngine, RecordingSink};
use crate::tuning::{EngineMemoryReport, MemoryTuning};

type Db = GuardedEngine<ProbeEngine>;

/// The OS file system with a settable free space, and removals, data syncs and directory syncs
/// that can be made to fail.
struct SpaceFs {
    os: OsFs,
    free: AtomicU64,
    fail_remove: AtomicBool,
    fail_sync_data: AtomicBool,
    fail_sync_dir: AtomicBool,
}

impl SpaceFs {
    fn failing(flag: &AtomicBool, what: &str) -> io::Result<()> {
        if flag.load(Ordering::Relaxed) {
            return Err(io::Error::other(format!("{what} fails")));
        }
        Ok(())
    }
}

impl FlatFs for SpaceFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.os.create(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.os.open_read(path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.os.write_all_at(path, offset, data)
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        Self::failing(&self.fail_sync_data, "sync_data")?;
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        Self::failing(&self.fail_sync_dir, "sync_dir")?;
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        Self::failing(&self.fail_remove, "remove")?;
        self.os.remove(path)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, _dir: &Path) -> io::Result<u64> {
        Ok(self.free.load(Ordering::Relaxed))
    }
}

/// A tuning that counts its post-Bk hook and records where it ran: the custom-tuning test's
/// `FlushMachine` legs (E-b1.8).
#[derive(Default)]
struct CountingTuning {
    after_bk: AtomicU32,
}

impl EngineTuning for CountingTuning {
    type E = MemoryEngine;

    fn open_chainstate(
        &self,
        path: &Path,
        sizes: &CacheSizes,
        sub_batch: u64,
    ) -> Result<(MemoryEngine, SchemaState), StoreError> {
        MemoryTuning.open_chainstate(path, sizes, sub_batch)
    }

    fn batch_reserve(&self, sub_batch: u64) -> u64 {
        sub_batch / 3
    }

    /// Counts, then runs a barrier through the guard, so a test can fail it.
    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError> {
        self.after_bk.fetch_add(1, Ordering::Relaxed);
        db.flush_memtables()
    }

    fn journal_budget(&self) -> u64 {
        1 << 20
    }

    fn unbounded_delete_span(&self) -> u32 {
        1 << 10
    }

    fn memory_report(&self, _db: &GuardedEngine<MemoryEngine>) -> EngineMemoryReport {
        EngineMemoryReport::default()
    }
}

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// A block at `height`, never the null hash.
fn block(height: u32) -> BlockRef {
    let mut hash = [0xb1; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

fn key(i: u32) -> OutPointKey {
    let mut k = [0u8; 36];
    k[..4].copy_from_slice(&i.to_le_bytes());
    k[4] = 0x5a;
    OutPointKey(k)
}

fn output(value: i64, script_len: usize) -> TxOut {
    TxOut {
        value,
        script_pubkey: vec![0x51; script_len].into(),
    }
}

/// One node's W and V-side cache, over the probe engine and real block files.
struct World {
    _dir: tempfile::TempDir,
    db: Arc<Db>,
    files: GuardedFiles<SpaceFs>,
    fs: Arc<SpaceFs>,
    hub: SubscriberHub,
    obs: RecordingObserver,
    tuning: CountingTuning,
    cfg: FlushConfig,
    cache: CoinsCache<ProbeEngine>,
    covered_seq: u64,
    next_id: u64,
    /// V's answer to step 7's `PruneClears`.
    cleared: Vec<IndexWrite>,
    /// V declines step 7's selection instead (DEC-211).
    declines: bool,
}

impl World {
    fn new(sub_batch: u64) -> Self {
        Self::with_files(sub_batch, BlockFilesState::default())
    }

    /// A world whose block files start at `state`, each file on disk.
    fn with_files(sub_batch: u64, state: BlockFilesState) -> Self {
        Self::with_db(sub_batch, state, GuardedEngine::probe(Arc::new(RecordingSink::default())))
    }

    /// As [`with_files`](Self::with_files), with `bug` planted.
    #[cfg(feature = "planted-bugs")]
    fn planted(sub_batch: u64, state: BlockFilesState, bug: crate::Planted) -> Self {
        let mut db = GuardedEngine::probe(Arc::new(RecordingSink::default()));
        db.plant(bug);
        Self::with_db(sub_batch, state, db)
    }

    fn with_db(sub_batch: u64, state: BlockFilesState, db: Db) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let blocks = dir.path().join("blocks");
        create_on_disk(&blocks, &state);
        let db = Arc::new(db);
        let fs = Arc::new(SpaceFs {
            os: OsFs::new(),
            free: AtomicU64::new(u64::MAX),
            fail_remove: AtomicBool::new(false),
            fail_sync_data: AtomicBool::new(false),
            fail_sync_dir: AtomicBool::new(false),
        });
        let files = BlockFiles::with_fs(fs.clone(), BlockFilesConfig::new(&blocks, MAGIC), state);
        let budget = CoinsBudget::new(64 << 20, 0, Ratio::HALF, 0);
        Self {
            files: GuardedFiles::new(files, db.latch().clone()),
            cache: CoinsCache::new(db.clone(), budget, Salt::fixed(3), None),
            db,
            fs,
            hub: SubscriberHub::new(),
            obs: RecordingObserver::default(),
            tuning: CountingTuning::default(),
            cfg: FlushConfig {
                sub_batch,
                data_dir: dir.path().to_path_buf(),
                blocks_dir: blocks,
                live_copy: false,
            },
            _dir: dir,
            covered_seq: 0,
            next_id: 0,
            cleared: Vec::new(),
            declines: false,
        }
    }

    /// Whether file `n`'s blk file is on disk.
    fn on_disk(&self, n: u32) -> bool {
        blk_on_disk(&self.cfg.blocks_dir, n)
    }

    /// Connects `n` outputs of `script_len`-byte scripts and spends `spent` coins of the previous
    /// call, then sets the best block to `tip`.
    fn connect(&mut self, from: u32, n: u32, script_len: usize, spend: &[u32], tip: u32) {
        for i in from..from + n {
            let out = output(1_000 + i64::from(i), script_len);
            let src = CoinSrc::Output {
                out: &out,
                height: tip,
                coinbase: false,
            };
            self.cache.add(&key(i), src, false).unwrap();
        }
        for i in spend {
            assert!(self.cache.spend(&key(*i)).unwrap());
        }
        self.cache.set_best_block(Some(block(tip)));
    }

    /// Appends a block to the block files, so the next barrier has an info to write.
    fn save_block(&self, height: u32) {
        // A header and no transactions: decoding is all the writer asks of its bytes.
        let mut raw = [0; 81];
        raw[..4].copy_from_slice(&height.to_le_bytes());
        let bytes = dash_consensus::decode_block(&mut &raw[..]).unwrap().core_bytes();
        self.files.mutate(|f| f.save_block(&bytes, height, 1_700_000_000, None)).unwrap();
    }

    fn packet(&mut self, presumed_effects_seq: u64, copy_point: bool) -> FreezePacket<()> {
        self.packet_with(presumed_effects_seq, copy_point, None)
    }

    /// [`packet`](Self::packet), with `record` as the tip's block-index record if given.
    fn packet_with(
        &mut self,
        presumed_effects_seq: u64,
        copy_point: bool,
        record: Option<Vec<u8>>,
    ) -> FreezePacket<()> {
        let old = self.cache.durable();
        let coins = self.cache.freeze(0);
        self.next_id += 1;
        let tip = coins.tip();
        FreezePacket {
            flush_id: self.next_id,
            coins,
            old,
            delta: (),
            // B1's batch with the tuning's R at the current S, as the node's freeze reserves it
            // (CH2c D40): the tests change S between flushes.
            index: FrozenIndex::take(
                self.cache
                    .db()
                    .batch_with_capacity(batch_capacity(&self.tuning, self.cfg.sub_batch)),
                &mut vec![IndexWrite {
                    hash: tip.hash,
                    bytes: record.unwrap_or_else(|| vec![tip.height as u8; 40]),
                }],
                None,
            ),
            presumed_effects_seq,
            tip_seq: u64::from(tip.height) * 10,
            causes: FlushCauses::SCRIPTED,
            mode: if copy_point {
                FlushStateMode::Always
            } else {
                FlushStateMode::IfNeeded
            },
            prune: None,
            decided_at: Instant::now(),
            copy_point,
        }
    }

    fn env(&self, crash: CrashRoll) -> FlushEnv<'_, ProbeEngine, SpaceFs, CountingTuning> {
        FlushEnv {
            engine: &self.db,
            files: &self.files,
            fs: &self.fs,
            hub: &self.hub,
            obs: &self.obs,
            tuning: &self.tuning,
            cfg: &self.cfg,
            covered_seq: self.covered_seq,
            crash,
        }
    }

    /// Runs `m` to its end: the steps run, the report, and the database's meta at the release.
    fn run(&mut self, m: FlushMachine<()>) -> Run {
        self.run_rolling(m, CrashRoll::seeded(0, 0))
    }

    /// [`run`](Self::run) for a window with evo state.
    fn run_evo(&mut self, m: FlushMachine<crate::evo::EvoWindowDelta>) -> Run {
        self.run_rolling(m, CrashRoll::seeded(0, 0))
    }

    /// As [`run`](Self::run), under `crash`'s roll, which must not come up.
    fn run_rolling<D: StateDelta>(&mut self, mut m: FlushMachine<D>, crash: CrashRoll) -> Run {
        let mut env = self.env(crash);
        let mut run = Run::default();
        loop {
            let at = m.next_step();
            match m.step(&mut env).expect("no failure") {
                Step::Progress(s) => run.steps.push(s),
                Step::PruneClears(req) => {
                    run.steps.push(FlushStep::PruneSelect);
                    run.clears.push(req.files.clone());
                    run.bounds.push((req.highest, req.rule));
                    if self.declines {
                        req.decline();
                    } else {
                        req.reply(self.cleared.clone());
                    }
                }
                Step::Waiting => panic!("V answered at once"),
                Step::Released(report) => {
                    run.steps.push(FlushStep::Release);
                    run.meta_at_release = Some(Meta::read(env.engine).unwrap());
                    run.done_at_release = m.next_step().is_none();
                    run.report = Some(report);
                }
                Step::Done => break,
                Step::Crash => panic!("the roll came up"),
                Step::Refused => panic!("a refused store call"),
            }
            assert_eq!(Some(*run.steps.last().unwrap()), at);
        }
        let covered = env.covered_seq;
        self.covered_seq = covered;
        run
    }

    /// V's release of a full flush.
    fn release(&mut self, report: &FlushReport) {
        self.cache.release(report.coins.unwrap().generation);
    }

    fn meta(&self) -> Meta {
        Meta::read(&*self.db).unwrap()
    }

    fn cf(name: &str) -> dashd_store::Cf {
        CHAINSTATE_SCHEMA.cf(name).unwrap()
    }

    fn get(&self, cf: &str, key: &[u8]) -> Option<Vec<u8>> {
        self.db.inner().unprobed().get(Self::cf(cf), key).unwrap()
    }
}

#[derive(Default)]
struct Run {
    steps: Vec<FlushStep>,
    report: Option<FlushReport>,
    meta_at_release: Option<Meta>,
    /// W had nothing left once it released.
    done_at_release: bool,
    /// Step 7's requests to V.
    clears: Vec<Vec<u32>>,
    /// Each request's highest selected block and rule.
    bounds: Vec<(u32, PruneRule)>,
}

/// The coins F carried: each DIRTY entry as the database must hold it (a tombstone erased).
fn expected_coins(f: &FrozenLayer) -> Vec<(OutPointKey, Vec<u8>)> {
    let mut v: Vec<_> = f
        .coins()
        .iter_dirty()
        .filter(|(_, flags, _)| !flags.is_spent())
        .map(|(k, _, c)| (k, c.to_vec()))
        .collect();
    v.sort_by_key(|(k, _)| *k);
    v
}

fn sorted(mut v: Vec<(OutPointKey, Vec<u8>)>) -> Vec<(OutPointKey, Vec<u8>)> {
    v.sort_by_key(|(k, _)| *k);
    v
}

/// A small F fits in B1: one Durable batch with the index, the files' infos, the evo tip and every
/// coin at `meta.tip = new`, no marker, then the release and the hook.
#[test]
fn a_small_flush_is_one_batch() {
    let mut w = World::new(1 << 20);
    w.connect(0, 50, 25, &[], 1);
    w.save_block(1);
    let p = w.packet(0, false);
    let expected = expected_coins(&p.coins);
    let kb = p.coins.coins().allocated_bytes() / 1000;
    let r = w.run(FlushMachine::new(p));
    use FlushStep::*;
    assert_eq!(r.steps, [Space, Files, B1, Durable, Release, AfterBk]);
    // One batch, of the tuning's R (E-b1.8).
    assert_eq!(w.db.inner().capacities(), [(1 << 20) / 3]);
    let report = r.report.unwrap();
    assert_eq!(
        report.coins,
        Some(CoinsWritten {
            generation: 0,
            new: block(1),
            split: false,
            sub_batches: 0,
            changed: 50,
            count: 50,
        })
    );
    w.release(&report);
    assert_eq!(sorted(db_coins(w.db.as_ref())), expected);
    let meta = w.meta();
    assert_eq!(
        (meta.tip, meta.head_blocks, meta.evo_tip),
        (Some(block(1).hash), None, Some(block(1).hash))
    );
    assert_eq!(meta.last_file, 0);
    assert_eq!(w.get("block_index", block(1).hash.as_bytes()), Some(vec![1; 40]));
    let info = w.files.read(|f| *f.file_info(0).unwrap());
    let mut bytes = Vec::new();
    info.encode_value(&mut bytes);
    assert_eq!(w.get("block_files", &0u32.to_be_bytes()), Some(bytes));
    assert_eq!(w.hub.t_ack().map(|t| (t.tip, t.tip_seq)), Some((block(1), 10)));
    assert_eq!(w.tuning.after_bk.load(Ordering::Relaxed), 1);
    assert_eq!(
        w.obs.lines(LogCat::CoinDb)[1],
        "Committed 50 changed transaction outputs (out of 50) to coin database..."
    );
    assert!(w.obs.lines(LogCat::CoinDb)[0].starts_with("Writing final batch of 0.0"));
    // Core formats the integer `coins_mem_usage / 1000` with `%.2f`: tinyformat prints it whole.
    let timer = format!("FlushStateToDisk: write coins cache to disk (50 coins, {kb}kB) started");
    assert!(w.obs.lines(LogCat::Bench).contains(&timer));
    let marks = w.obs.marks();
    assert!(
        matches!(marks[0], ObsMark::FlushBegin { flush: 1, old: None, new } if new == block(1).hash)
    );
    assert!(marks.contains(&ObsMark::Durable {
        flush: 1
    }));
}

/// A large F is split (§4.2 steps 4, 8-10; `txdb.cpp:113-170`): B1 carries the marker `[new, old]`
/// with `meta.tip` erased, the coins go in sub-batches of more than S, each of the tuning's R, a
/// WAL barrier precedes Bk, and Bk clears the marker at `meta.tip = new`. Tombstones erase.
#[test]
fn a_large_flush_is_split_under_the_marker() {
    let mut w = World::new(1 << 20);
    w.connect(0, 400, 25, &[], 1);
    let first = w.packet(0, false);
    let r = w.run(FlushMachine::new(first));
    w.release(r.report.as_ref().unwrap());

    w.cfg.sub_batch = 4 << 10;
    w.connect(400, 600, 25, &[3, 5, 7, 400], 2);
    let mut expected: std::collections::BTreeMap<_, _> =
        db_coins(w.db.as_ref()).into_iter().collect();
    let p = w.packet(0, false);
    let expected_dirty = p.coins.coins().dirty_len() as u64;
    for (k, flags, coin) in p.coins.coins().iter_dirty() {
        match flags.is_spent() {
            true => expected.remove(&k),
            false => expected.insert(k, coin.to_vec()),
        };
    }
    let batches = w.db.inner().capacities().len();
    let mut m = FlushMachine::new(p);
    let mut env = w.env(CrashRoll::seeded(0, 0));
    let mut steps = Vec::new();
    let coins = loop {
        match m.step(&mut env).unwrap() {
            Step::Progress(s) => {
                steps.push(s);
                let meta = Meta::read(env.engine).unwrap();
                if s == FlushStep::B1 {
                    assert_eq!(meta.tip, None);
                    assert_eq!(meta.head_blocks, Some((block(2).hash, Some(block(1).hash))));
                }
                if s == FlushStep::Bk {
                    assert_eq!((meta.tip, meta.head_blocks), (Some(block(2).hash), None));
                }
            }
            Step::Released(report) => break report.coins.unwrap(),
            _ => panic!("a flush ends with its release"),
        }
    };
    steps.push(FlushStep::Release);
    assert!(coins.split);
    assert!(coins.sub_batches >= 4, "{coins:?}");
    assert_eq!(coins.changed, expected_dirty);
    assert_eq!(w.hub.t_ack().map(|t| t.tip), Some(block(2)));
    w.cache.release(coins.generation);
    let n = steps.iter().filter(|s| matches!(s, FlushStep::SubBatch(_))).count();
    use FlushStep::*;
    let mut want = vec![Space, Files, B1, Durable];
    want.extend((0..n as u32).map(SubBatch));
    want.extend([PreBkBarrier, Bk, Release]);
    assert_eq!(steps, want);
    // Every partial batch passed S, as Core's `SizeEstimate() > batch_size`.
    let partial =
        w.obs.lines(LogCat::CoinDb).iter().filter(|l| l.starts_with("Writing partial")).count();
    assert_eq!(partial, n - 1);
    // B1 once, V's from the freeze with the records in it (CH2c D40), then each sub-batch and
    // Bk: all of R.
    assert_eq!(w.db.inner().capacities()[batches - 1..], vec![(4 << 10) / 3; n + 2]);
    // The database holds exactly the coins the cache reads.
    assert_eq!(sorted(db_coins(w.db.as_ref())), expected.into_iter().collect::<Vec<_>>());
    for i in [3, 5, 7, 400] {
        assert_eq!(w.get("coins", &db_key(&key(i))), None, "coin {i}");
    }
}

/// The split's edges: a one-batch flush, and a split whose coins fit one sub-batch once B1's
/// index is out of the way, write no partial batch, so the crash roll never comes up even at
/// ratio 1, and Bk follows the first sub-batch with no pre-Bk barrier.
#[test]
fn no_partial_batch_no_crash_roll() {
    let mut w = World::new(4 << 10);
    w.connect(0, 20, 25, &[], 1);
    let p = w.packet(0, false);
    let r = w.run_rolling(FlushMachine::new(p), CrashRoll::seeded(1, 9));
    assert!(!r.report.as_ref().unwrap().coins.unwrap().split);
    w.release(r.report.as_ref().unwrap());

    w.connect(20, 30, 25, &[], 2);
    let p = w.packet_with(0, false, Some(vec![2; 3 << 10]));
    let expected = expected_coins(&p.coins);
    let r = w.run_rolling(FlushMachine::new(p), CrashRoll::seeded(1, 9));
    use FlushStep::*;
    assert_eq!(r.steps, [Space, Files, B1, Durable, SubBatch(0), Bk, Release, AfterBk]);
    let coins = r.report.unwrap().coins.unwrap();
    assert_eq!((coins.split, coins.sub_batches), (true, 0));
    let meta = w.meta();
    assert_eq!((meta.tip, meta.head_blocks), (Some(block(2).hash), None));
    let got = sorted(db_coins(w.db.as_ref()));
    assert!(expected.iter().all(|e| got.contains(e)));
    assert!(!w.obs.lines(LogCat::CoinDb).iter().any(|l| l.starts_with("Writing partial")));
}

fn db_key(k: &OutPointKey) -> Vec<u8> {
    let mut buf = [0; crate::coins::DB_KEY_MAX];
    let n = k.db_key(&mut buf);
    buf[..n].to_vec()
}

/// A crash between Bk's sub-batches leaves the marker: a power loss keeps B1 (Durable) and any
/// prefix of the sub-batches, never `meta.tip` (§4.3's replay input).
#[test]
fn a_crash_roll_after_a_partial_batch_leaves_the_marker() {
    let mut w = World::new(4 << 10);
    w.connect(0, 600, 25, &[], 1);
    let p = w.packet(0, false);
    let mut m = FlushMachine::new(p);
    let mut env = w.env(CrashRoll::seeded(1, 9));
    let crashed = loop {
        match m.step(&mut env).unwrap() {
            Step::Progress(_) => {}
            Step::Crash => break true,
            _ => break false,
        }
    };
    assert!(crashed, "ratio 1 crashes after the first partial batch");
    assert!(matches!(m.step(&mut env), Ok(Step::Done)), "a crashed machine does nothing");
    assert!(w.obs.lines(LogCat::Always).contains(&"Simulating a crash. Goodbye.".to_string()));
    w.db.inner().unprobed().simulate_power_loss(0);
    let meta = w.meta();
    assert_eq!((meta.tip, meta.head_blocks), (None, Some((block(1).hash, None))));
}

/// §4.1: the release follows a Durable final write, B1 in a one-batch flush and Bk in a split one.
/// A power loss right after W releases V keeps the whole flush at `meta.tip = new` with no marker,
/// so `ChainStateFlushed`, handed over at the release (DEC-166), never names a tip the database
/// can still lose.
#[test]
fn a_released_flush_survives_a_power_loss() {
    for split in [false, true] {
        let mut w = World::new(1 << 20);
        w.connect(0, 400, 25, &[], 1);
        if split {
            let first = w.packet(0, false);
            let r = w.run(FlushMachine::new(first));
            w.release(r.report.as_ref().unwrap());
            w.cfg.sub_batch = 4 << 10;
            w.connect(400, 600, 25, &[3, 5, 7, 400], 2);
        }
        let mut expected: std::collections::BTreeMap<_, _> =
            db_coins(w.db.as_ref()).into_iter().collect();
        let p = w.packet(0, false);
        let new = p.coins.tip();
        for (k, flags, coin) in p.coins.coins().iter_dirty() {
            match flags.is_spent() {
                true => expected.remove(&k),
                false => expected.insert(k, coin.to_vec()),
            };
        }
        let mut m = FlushMachine::new(p);
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let coins = loop {
            match m.step(&mut env).unwrap() {
                Step::Progress(_) => {}
                Step::Released(report) => break report.coins.unwrap(),
                _ => panic!("a flush ends with its release"),
            }
        };
        assert_eq!(coins.split, split);
        w.db.inner().unprobed().simulate_power_loss(0);
        let meta = w.meta();
        assert_eq!((meta.tip, meta.head_blocks), (Some(new.hash), None), "split: {split}");
        assert_eq!(sorted(db_coins(w.db.as_ref())), expected.into_iter().collect::<Vec<_>>());
    }
}

/// Step 3: W syncs the WAL before B1 only for presumed effects it has not covered, and records the
/// seq once the barrier returns.
#[test]
fn the_presumed_barrier_runs_only_for_new_effects() {
    let mut w = World::new(1 << 20);
    w.connect(0, 10, 25, &[], 1);
    let p = w.packet(3, false);
    let r = w.run(FlushMachine::new(p));
    assert_eq!(r.steps[2], FlushStep::PresumedBarrier);
    assert_eq!(w.covered_seq, 3);
    w.release(r.report.as_ref().unwrap());
    w.connect(10, 10, 25, &[], 2);
    let p = w.packet(3, false);
    let r = w.run(FlushMachine::new(p));
    assert!(!r.steps.contains(&FlushStep::PresumedBarrier));
}

/// Step 1 (`validation.cpp:3050`, `:3088`): `blocks/` needs 50 MiB, the datadir 50 MiB plus 192
/// bytes per entry; short, the flush aborts with Core's strings before writing anything, and the
/// latch trips so every waiter wakes.
#[test]
fn too_little_disk_space_aborts_before_any_write() {
    for (free, ok) in [
        (MIN_DISK_SPACE - 1, false),
        (MIN_DISK_SPACE + DISK_BYTES_PER_ENTRY * 20 - 1, false),
        (MIN_DISK_SPACE + DISK_BYTES_PER_ENTRY * 20, true),
    ] {
        let mut w = World::new(1 << 20);
        w.connect(0, 20, 25, &[], 1);
        w.fs.free.store(free, Ordering::Relaxed);
        let p = w.packet(0, false);
        let mut m = FlushMachine::new(p);
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let got = m.step(&mut env);
        if ok {
            assert!(matches!(got, Ok(Step::Progress(FlushStep::Space))));
            continue;
        }
        let Err(abort) = got else {
            panic!("free {free}: no abort")
        };
        let low = "Disk space is too low!".to_string();
        assert_eq!((abort.log.as_str(), abort.user.as_deref()), (low.as_str(), Some(low.as_str())));
        assert_eq!(m.next_step(), None);
        let tripped = w.db.latch().tripped().unwrap();
        assert_eq!((tripped.kind, tripped.site), (ErrorKind::NoSpace, "chainstate flush"));
        assert_eq!(w.meta(), Meta::default());
        assert_eq!(
            w.obs.marks().last(),
            Some(&ObsMark::Abort {
                message: low
            })
        );
    }
}

/// The flush's `LogCat::Always` lines and its failure marks, in order: of a failed flush, what
/// the node's debug log shows and where the `AbortNode` comes.
fn always_lines_and_failure(marks: &[ObsMark]) -> Vec<String> {
    (marks.iter())
        .filter_map(|m| match m {
            ObsMark::Log {
                cat: LogCat::Always,
                line,
            } => Some(line.clone()),
            ObsMark::StoreFailed {
                op,
            } => Some(format!("StoreFailed({op})")),
            ObsMark::Abort {
                message,
            } => Some(format!("*** {message}")),
            _ => None,
        })
        .collect()
}

/// BL-485: the end of a flush whose store call `op` failed with `abort`: Core's `HandleError`
/// lines, the engine's text and the `-debug=leveldb` hint, once, then the failure and its
/// `AbortNode` (`System error while flushing: <the engine's text>`).
// Core: dbwrapper.cpp:261-268; validation.cpp:3121-3123
fn assert_handle_error_then_abort(marks: &[ObsMark], op: &str, abort: &Abort, case: &str) {
    let e = abort.log.strip_prefix("System error while flushing: ").expect("a store failure");
    let lines = always_lines_and_failure(marks);
    let want = [e.to_owned(), DEBUG_LEVELDB.to_owned(), format!("StoreFailed({op})")];
    let want = [&want[..], &[format!("*** {}", abort.log)]].concat();
    assert!(lines.ends_with(&want), "{case}: {lines:?}");
    assert_eq!(lines.iter().filter(|l| *l == DEBUG_LEVELDB).count(), 1, "{case}: {lines:?}");
}

/// A failed write (§4.6): "System error while flushing: …", the store's failure on the latch, the
/// marks, and no step after it.
#[test]
fn a_failed_b1_aborts_with_cores_string() {
    let mut w = World::new(1 << 20);
    w.connect(0, 10, 25, &[], 1);
    let p = w.packet(0, false);
    let mut m = FlushMachine::new(p);
    w.db.inner().crash_at_write(1);
    let mut env = w.env(CrashRoll::seeded(0, 0));
    assert!(matches!(m.step(&mut env), Ok(Step::Progress(FlushStep::Space))));
    assert!(matches!(m.step(&mut env), Ok(Step::Progress(FlushStep::Files))));
    let abort = m.step(&mut env).err().unwrap();
    assert!(abort.log.starts_with("System error while flushing: "), "{}", abort.log);
    assert_eq!(abort.user, None);
    assert_eq!(m.next_step(), None);
    assert_eq!(w.db.latch().tripped().unwrap().site, "chainstate write");
    let marks = w.obs.marks();
    let n = marks.len();
    assert_eq!(
        marks[n - 2..],
        [
            ObsMark::StoreFailed {
                op: "write"
            },
            ObsMark::Abort {
                message: abort.log.clone()
            }
        ]
    );
    assert_handle_error_then_abort(&marks, "write", &abort, "B1");
    assert_eq!(w.hub.t_ack(), None);
    // CH3b D221: the latch's failure gives V the text W reported.
    assert_eq!(abort_text(w.db.latch().tripped().unwrap().clone()), abort.log);
}

/// CH3b D221: [`abort_text`] puts a chainstate write's or barrier's engine text inside the flush's
/// system error, and keeps every other site's latched text, which its reporter logged as is.
// Core: validation.cpp:3122-3123, :2245-2249
#[test]
fn abort_text_names_the_reporters_text() {
    let at = |site: &'static str| {
        abort_text(Failure {
            kind: ErrorKind::Io,
            site,
            message: "the text".into(),
        })
    };
    for site in ["chainstate write", "chainstate barrier"] {
        assert_eq!(at(site), "System error while flushing: the text", "{site}");
    }
    let kept = [
        "chainstate flush",
        "block files write",
        "block files read",
        "chainstate read",
        "chainstate validation",
        "dbcrashratio",
        "evodb write",
    ];
    for site in kept {
        assert_eq!(at(site), "the text", "{site}");
    }
}

/// Every store call of a split flush with a presumed effect can fail (§4.6): the step stops with
/// "System error while flushing: …", nothing runs after it, the marker stays once B1 is durable,
/// and a failed presumed barrier leaves `covered_seq` alone.
#[test]
fn every_store_call_can_fail() {
    use FlushStep::*;
    let world = || {
        let mut w = World::new(4 << 10);
        w.connect(0, 600, 25, &[], 1);
        w
    };
    let mut reference = world();
    let p = reference.packet(1, false);
    let all = reference.run(FlushMachine::new(p)).steps;
    let writes = 2 + all.iter().filter(|s| matches!(s, SubBatch(_))).count() as u32 - 1;
    // (failure, the step that fails, B1 durable)
    let cases = [
        (Err(1), PresumedBarrier, false),
        (Ok(1), B1, false),
        (Ok(2), SubBatch(0), true),
        (Err(2), PreBkBarrier, true),
        (Ok(writes), Bk, true),
        (Err(3), AfterBk, true),
    ];
    for (fail, at, durable) in cases {
        let mut w = world();
        let p = w.packet(1, false);
        let mut m = FlushMachine::new(p);
        match fail {
            Ok(write) => w.db.inner().crash_at_write(write),
            Err(barrier) => w.db.inner().fail_barrier_at(barrier),
        }
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let abort = loop {
            let next = m.next_step();
            match m.step(&mut env) {
                Ok(Step::Done) => panic!("{at:?}: no failure"),
                Ok(_) => {}
                Err(abort) => {
                    assert_eq!(next, Some(at));
                    break abort;
                }
            }
        };
        assert!(abort.log.starts_with("System error while flushing: "), "{at:?}: {}", abort.log);
        let op = w.obs.marks().iter().find_map(|m| match m {
            ObsMark::StoreFailed {
                op,
            } => Some(*op),
            _ => None,
        });
        let op = op.expect("a store call failed");
        assert_handle_error_then_abort(&w.obs.marks(), op, &abort, &format!("{at:?}"));
        assert_eq!(m.next_step(), None, "{at:?}");
        assert!(w.db.latch().tripped().is_some(), "{at:?}");
        assert_eq!(env.covered_seq, u64::from(at != PresumedBarrier), "{at:?}");
        let meta = Meta::read(env.engine).unwrap();
        let marker = Some((block(1).hash, None));
        match at {
            AfterBk => assert_eq!((meta.tip, meta.head_blocks), (Some(block(1).hash), None)),
            _ if durable => assert_eq!((meta.tip, meta.head_blocks), (None, marker)),
            _ => assert_eq!(meta, Meta::default(), "{at:?}"),
        }
    }
}

/// E-b1.8: an ordinary flush reports `FlushDone` before the engine's hook; a copy point runs the
/// hook first and then releases with nothing left to do, so the snapshot V takes once it sees the
/// release is at `new` with no marker, and the RPC's cursor never pins the memtables Bk filled.
#[test]
fn a_copy_point_runs_the_hook_before_the_release() {
    let mut w = World::new(1 << 20);
    w.connect(0, 30, 25, &[], 1);
    let p = w.packet(0, false);
    let r = w.run(FlushMachine::new(p));
    use FlushStep::*;
    assert_eq!(r.steps[r.steps.len() - 2..], [Release, AfterBk]);
    assert!(!r.done_at_release);
    w.release(r.report.as_ref().unwrap());

    w.connect(30, 30, 25, &[], 2);
    let p = w.packet(0, true);
    let r = w.run(FlushMachine::new(p));
    assert_eq!(r.steps, [Space, Files, B1, Durable, AfterBk, Release]);
    assert!(r.done_at_release);
    let meta = r.meta_at_release.unwrap();
    assert_eq!((meta.tip, meta.head_blocks), (Some(block(2).hash), None));
    assert_eq!(w.tuning.after_bk.load(Ordering::Relaxed), 2);
    w.release(r.report.as_ref().unwrap());

    // Split, too.
    w.cfg.sub_batch = 4 << 10;
    w.connect(60, 600, 25, &[], 3);
    let p = w.packet(0, true);
    let r = w.run(FlushMachine::new(p));
    assert_eq!(r.steps[r.steps.len() - 3..], [Bk, AfterBk, Release]);
    assert!(r.done_at_release);
    let meta = r.meta_at_release.unwrap();
    assert_eq!((meta.tip, meta.head_blocks), (Some(block(3).hash), None));
    assert_eq!(w.tuning.after_bk.load(Ordering::Relaxed), 3);
}

/// The index-only write (Core's `fPeriodicWrite`): the files barrier and one Durable batch of the
/// index, no coins, no `meta.tip`, no hook; under the live-copy rule an ALWAYS one ends with the
/// barrier before its release.
#[test]
fn an_index_only_write_writes_the_index_alone() {
    let mut w = World::new(1 << 20);
    w.save_block(5);
    let write = |always, b| IndexOnlyWrite {
        flush_id: 7,
        index: FrozenIndex::take(
            b,
            &mut vec![IndexWrite {
                hash: block(5).hash,
                bytes: vec![5; 12],
            }],
            None,
        ),
        decided_at: Instant::now(),
        always,
        copy_point: false,
    };
    let b = w.cache.db().batch();
    let r = w.run(FlushMachine::index_only(write(false, b)));
    use FlushStep::*;
    assert_eq!(r.steps, [Space, Files, B1, Durable, Release]);
    assert_eq!(r.report.unwrap().coins, None);
    assert_eq!(w.get("block_index", block(5).hash.as_bytes()), Some(vec![5; 12]));
    assert!(w.get("block_files", &0u32.to_be_bytes()).is_some());
    assert_eq!((w.meta().tip, w.hub.t_ack()), (None, None));
    assert_eq!(w.tuning.after_bk.load(Ordering::Relaxed), 0);

    w.cfg.live_copy = true;
    let b = w.cache.db().batch();
    let r = w.run(FlushMachine::index_only(write(true, b)));
    assert_eq!(r.steps, [Space, Files, B1, Durable, AfterBk, Release]);
    assert_eq!(w.tuning.after_bk.load(Ordering::Relaxed), 0);
}

/// `-dbcrashratio`: never with 0, about 1/N otherwise, and reproducible from a seed.
#[test]
fn the_crash_roll() {
    let mut off = CrashRoll::seeded(0, 1);
    assert!((0..1000).all(|_| !off.roll()));
    let rolls = |seed| {
        let mut r = CrashRoll::seeded(8, seed);
        (0..8000).map(|_| r.roll()).collect::<Vec<_>>()
    };
    assert_eq!(rolls(5), rolls(5));
    let hits = rolls(5).iter().filter(|h| **h).count();
    assert!((800..1200).contains(&hits), "{hits}");
}

/// Four 16 MiB files of ten blocks each, the last one current: the automatic selector may prune
/// the first two at [`PRUNE_TIP`], and leaves a 40 MiB target unmet.
fn four_files() -> BlockFilesState {
    equal_files(4, 12 << 20, 4 << 20)
}

/// The tip at which Core's `nLastBlockWeCanPrune` is 19.
const PRUNE_TIP: u32 = 19 + MIN_BLOCKS_TO_KEEP;

fn decision(files: Vec<u32>, met_target: bool, manual: Option<u32>) -> PruneDecision {
    PruneDecision {
        files,
        met_target,
        manual,
        tip_height: PRUNE_TIP,
        target: 40 << 20,
        prune_after_height: 0,
    }
}

/// A cleared record of V's.
fn cleared(n: u8) -> IndexWrite {
    IndexWrite {
        hash: BlockHash::from([n; 32]),
        bytes: vec![n; 9],
    }
}

/// Step 7 to 12 of a one-batch flush (CH2 §4.2): the selection is `D ∩ eligible`, never the
/// selector again; V's clears, the pruned files' infos and `meta.pruned_files` go in Bp after B1,
/// with no marker; the release comes before the unlink, which is best-effort; and the automatic
/// check is re-armed, as the selection leaves the target D had met unmet (T40).
#[test]
fn a_one_batch_flush_prunes_in_bp_and_unlinks_after_its_release() {
    let mut w = World::with_files(1 << 20, four_files());
    w.cleared = vec![cleared(1), cleared(2)];
    w.connect(0, 20, 25, &[], PRUNE_TIP);
    let mut p = w.packet(0, false);
    // File 1 is eligible but not in D; file 2 is in D but not eligible at 19.
    p.prune = Some(decision(vec![0, 2], true, None));
    let r = w.run(FlushMachine::new(p));
    use FlushStep::*;
    let want = [
        Space,
        Files,
        B1,
        Durable,
        PruneSelect,
        PruneApply,
        PruneBarrier,
        Bp,
        Release,
        AfterBk,
        Unlink,
    ];
    assert_eq!(r.steps, want);
    assert_eq!(r.clears, [vec![0]]);
    let at_release = r.meta_at_release.unwrap();
    assert!(at_release.pruned_files);
    assert_eq!((at_release.tip, at_release.head_blocks), (Some(block(PRUNE_TIP).hash), None));
    for c in &w.cleared {
        assert_eq!(w.get("block_index", c.hash.as_bytes()), Some(c.bytes.clone()));
    }
    let mut pruned = Vec::new();
    BlockFileInfo::default().encode_value(&mut pruned);
    assert_eq!(w.get("block_files", &0u32.to_be_bytes()), Some(pruned));
    assert_eq!(w.get("block_files", &1u32.to_be_bytes()), None);
    assert_eq!(w.meta().last_file, 3);
    assert_eq!(w.files.read(|f| *f.file_info(0).unwrap()), BlockFileInfo::default());
    assert_eq!((0..4).map(|n| w.on_disk(n)).collect::<Vec<_>>(), [false, true, true, true]);
    assert!(w.files.mutate(|f| Ok(f.take_check_for_pruning())).unwrap(), "re-armed");
    let marks = w.obs.marks();
    let selected = ObsMark::PruneSelected {
        flush: 1,
        files: vec![0],
        bound: 19,
        rearmed: true,
        target_unmet: true,
        lock: None,
    };
    let at = |m: &ObsMark| marks.iter().position(|x| x == m).unwrap();
    assert!(
        at(&ObsMark::Durable {
            flush: 1
        }) < at(&selected)
    );
    assert!(at(&selected) < at(&ObsMark::BestEffortBegin));
    assert!(at(&ObsMark::BestEffortBegin) < at(&ObsMark::BestEffortEnd));
    let bench = w.obs.lines(LogCat::Bench);
    assert!(bench.contains(&"FlushStateToDisk: unlink pruned files started".to_string()));
}

/// A split flush that prunes writes the clears in Bk, after the second files barrier (§4.2 step
/// 10): `meta.pruned_files` becomes durable with `meta.tip`, not before.
#[test]
fn a_split_flush_carries_the_clears_in_bk() {
    let mut w = World::with_files(4 << 10, four_files());
    w.cleared = vec![cleared(3)];
    w.connect(0, 600, 25, &[], PRUNE_TIP);
    let mut p = w.packet(0, false);
    p.prune = Some(decision(vec![0, 1], false, None));
    let mut m = FlushMachine::new(p);
    let mut env = w.env(CrashRoll::seeded(0, 0));
    let (mut steps, mut pruned_at, mut report) = (Vec::new(), None, None);
    while let Some(at) = m.next_step() {
        match m.step(&mut env).unwrap() {
            Step::PruneClears(req) => req.reply(w.cleared.clone()),
            Step::Progress(_) => {}
            Step::Released(r) => report = Some(r),
            s => panic!("{s:?}"),
        }
        steps.push(at);
        let meta = Meta::read(env.engine).unwrap();
        if pruned_at.is_none() && meta.pruned_files {
            assert_eq!(meta.tip, Some(block(PRUNE_TIP).hash));
            pruned_at = Some(at);
        }
    }
    use FlushStep::*;
    assert_eq!(pruned_at, Some(Bk));
    let n = steps.iter().filter(|s| matches!(s, SubBatch(_))).count() as u32;
    let mut want = vec![Space, Files, B1, Durable, PruneSelect, PruneApply];
    want.extend((0..n).map(SubBatch));
    want.extend([PreBkBarrier, PruneBarrier, Bk, Release, AfterBk, Unlink]);
    assert_eq!(steps, want);
    w.release(&report.unwrap());
    assert_eq!(w.get("block_index", cleared(3).hash.as_bytes()), Some(vec![3; 9]));
    assert_eq!((0..4).map(|n| w.on_disk(n)).collect::<Vec<_>>(), [false, false, true, true]);
    // D left the target unmet too: no re-arm.
    assert!(!w.files.mutate(|f| Ok(f.take_check_for_pruning())).unwrap());
}

/// Every store call a pruning flush adds can fail (§4.6): the second files barrier with Core's
/// `FlushBlockFile` text, and Bp or Bk, the clears in it, with "System error while flushing:
/// …". The flush stops there, one-batch or split: nothing runs after it, the latch has tripped,
/// no file is unlinked and `meta.pruned_files` is not durable.
#[test]
fn every_store_call_of_a_pruning_flush_can_fail() {
    use FlushStep::*;
    for (split, at) in [(false, PruneBarrier), (false, Bp), (true, PruneBarrier), (true, Bk)] {
        let mut w = World::with_files(
            if split {
                4 << 10
            } else {
                1 << 20
            },
            four_files(),
        );
        w.connect(
            0,
            if split {
                600
            } else {
                20
            },
            25,
            &[],
            PRUNE_TIP,
        );
        let mut p = w.packet(0, false);
        p.prune = Some(decision(vec![0], true, None));
        let mut m = FlushMachine::new(p);
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let abort = loop {
            let next = m.next_step();
            if next == Some(at) {
                match at {
                    PruneBarrier => w.fs.fail_sync_data.store(true, Ordering::Relaxed),
                    _ => w.db.inner().crash_at_write(1),
                }
            }
            match m.step(&mut env) {
                Ok(Step::PruneClears(req)) => req.reply(vec![cleared(1)]),
                Ok(Step::Done) => panic!("{at:?}: no failure"),
                Ok(_) => {}
                Err(abort) => {
                    assert_eq!(next, Some(at), "split {split}");
                    break abort;
                }
            }
        };
        let (text, op) = match at {
            PruneBarrier => ("Flushing block file to disk failed.", "prune files barrier"),
            _ => ("System error while flushing: ", "write"),
        };
        assert!(abort.log.starts_with(text), "{at:?}: {}", abort.log);
        // One report: the failed store call, then the abort with its message (Sol r2 1).
        let failures: Vec<_> = (w.obs.marks().into_iter())
            .filter(|m| matches!(m, ObsMark::StoreFailed { .. } | ObsMark::Abort { .. }))
            .collect();
        let report = [
            ObsMark::StoreFailed {
                op,
            },
            ObsMark::Abort {
                message: abort.log.clone(),
            },
        ];
        assert_eq!(failures, report, "split {split} {at:?}");
        let case = format!("split {split} {at:?}");
        if at == PruneBarrier {
            let lines = always_lines_and_failure(&w.obs.marks());
            assert!(!lines.iter().any(|l| l == DEBUG_LEVELDB), "{case}: {lines:?}");
        } else {
            assert_handle_error_then_abort(&w.obs.marks(), op, &abort, &case);
        }
        assert_eq!(m.next_step(), None, "{at:?}");
        assert!(w.db.latch().tripped().is_some(), "{at:?}");
        let meta = Meta::read(env.engine).unwrap();
        assert!(!meta.pruned_files, "{at:?}");
        assert_eq!(meta.tip.is_some(), !split, "B1's tip, one-batch; the marker, split");
        assert!((0..4).all(|n| w.on_disk(n)), "split {split} {at:?}: nothing unlinked");
        assert!(!w.obs.marks().contains(&ObsMark::BestEffortBegin), "{at:?}");
    }
}

/// `pruneblockchain` replies once V sees the release, so its unlink comes before it (§3.3: after
/// step 12); its bound is `min(height, tip − MIN_BLOCKS_TO_KEEP)`, and it never re-arms.
#[test]
fn a_manual_prune_unlinks_before_its_release() {
    let mut w = World::with_files(1 << 20, four_files());
    w.connect(0, 20, 25, &[], PRUNE_TIP);
    let mut p = w.packet(0, false);
    p.prune = Some(decision(vec![0, 1], true, Some(15)));
    let r = w.run(FlushMachine::new(p));
    use FlushStep::*;
    let want = [
        Space,
        Files,
        B1,
        Durable,
        PruneSelect,
        PruneApply,
        PruneBarrier,
        Bp,
        Unlink,
        Release,
        AfterBk,
    ];
    assert_eq!(r.steps, want);
    assert_eq!(r.clears, [vec![0]]);
    assert!(r.meta_at_release.unwrap().pruned_files);
    assert!(w.obs.marks().contains(&ObsMark::PruneSelected {
        flush: 1,
        files: vec![0],
        bound: 15,
        rearmed: false,
        target_unmet: false,
        lock: None,
    }));
    assert!(!w.on_disk(0) && w.on_disk(1));
    assert!(!w.files.mutate(|f| Ok(f.take_check_for_pruning())).unwrap());
}

/// An empty selection asks V for nothing and writes nothing of the prune, and re-arms the
/// automatic check exactly when D met a target the files now miss (T40).
#[test]
fn an_empty_selection_writes_nothing_and_rearms_only_if_d_met_the_target() {
    for (met_target, target, rearmed) in
        [(true, 40 << 20, true), (false, 40 << 20, false), (true, 1 << 30, false)]
    {
        let mut w = World::with_files(1 << 20, four_files());
        w.connect(0, 20, 25, &[], PRUNE_TIP);
        let mut p = w.packet(0, false);
        let mut d = decision(vec![2], met_target, None);
        d.target = target;
        p.prune = Some(d);
        let r = w.run(FlushMachine::new(p));
        use FlushStep::*;
        assert_eq!(r.steps, [Space, Files, B1, Durable, PruneSelect, Release, AfterBk]);
        assert!(r.clears.is_empty());
        assert!(!w.meta().pruned_files);
        assert!(w.obs.marks().contains(&ObsMark::PruneSelected {
            flush: 1,
            files: vec![],
            bound: 19,
            rearmed,
            target_unmet: target == 40 << 20,
            lock: None,
        }));
        assert_eq!(w.files.mutate(|f| Ok(f.take_check_for_pruning())).unwrap(), rearmed);
        assert!((0..4).all(|n| w.on_disk(n)));
    }
    // Split: the sub-batches follow at once, with no second files barrier, and Bk carries no
    // clears.
    let mut w = World::with_files(4 << 10, four_files());
    w.connect(0, 600, 25, &[], PRUNE_TIP);
    let mut p = w.packet(0, false);
    p.prune = Some(decision(vec![2], true, None));
    let r = w.run(FlushMachine::new(p));
    use FlushStep::*;
    assert_eq!(r.steps[..6], [Space, Files, B1, Durable, PruneSelect, SubBatch(0)]);
    assert_eq!(r.steps[r.steps.len() - 4..], [PreBkBarrier, Bk, Release, AfterBk]);
    assert!(r.clears.is_empty());
    assert!(!w.meta().pruned_files);
    assert!((0..4).all(|n| w.on_disk(n)));
}

/// V declines a selection its tip no longer allows (DEC-211): W prunes nothing (no second files
/// barrier, no clears, Bp not written, no file info, no unlink) and re-arms the automatic check,
/// whatever the rule and D's target, so the next flush selects at V's tip. The request carries
/// the selection's highest block and its rule.
#[test]
fn a_declined_selection_prunes_nothing_and_rearms() {
    use FlushStep::*;
    // (split, manual, the selection, its highest block)
    for (split, manual, selection, highest) in
        [(false, None, vec![0, 1], 19), (true, None, vec![0, 1], 19), (false, Some(15), vec![0], 9)]
    {
        let mut w = World::with_files(
            if split {
                4 << 10
            } else {
                1 << 20
            },
            four_files(),
        );
        w.declines = true;
        w.connect(
            0,
            if split {
                600
            } else {
                20
            },
            25,
            &[],
            PRUNE_TIP,
        );
        let mut p = w.packet(0, false);
        // D left the target unmet: the re-arm is the decline's alone.
        p.prune = Some(decision(vec![0, 1], false, manual));
        // Each selected file's info: persisted, and the files' own (the next barrier's).
        let infos = |w: &World| -> Vec<_> {
            (selection.iter())
                .map(|&n: &u32| {
                    let held = w.files.mutate(|f| Ok(f.file_info(n).copied())).unwrap();
                    (w.get("block_files", &n.to_be_bytes()), held)
                })
                .collect()
        };
        let infos_before = infos(&w);
        let r = w.run(FlushMachine::new(p));
        let case = format!("split {split}, manual {manual:?}");
        assert_eq!(r.clears, vec![selection.clone()], "{case}");
        let rule = manual.map_or(
            PruneRule::Automatic {
                after: 0,
            },
            PruneRule::Manual,
        );
        assert_eq!(r.bounds, [(highest, rule)], "{case}");
        assert_eq!(r.steps[..6], [Space, Files, B1, Durable, PruneSelect, PruneApply], "{case}");
        let tail: &[FlushStep] = if split {
            &[PreBkBarrier, Bk, Release, AfterBk]
        } else {
            &[Release, AfterBk]
        };
        assert_eq!(r.steps[r.steps.len() - tail.len()..], *tail, "{case}");
        assert!(!r.steps.iter().any(|s| matches!(s, PruneBarrier | Bp | Unlink)), "{case}");
        assert!(!w.meta().pruned_files, "{case}");
        // Every selected file's info is as it was (Sol r2 2).
        assert_eq!(infos(&w), infos_before, "{case}");
        assert!((0..4).all(|n| w.on_disk(n)), "{case}");
        assert!(w.files.mutate(|f| Ok(f.take_check_for_pruning())).unwrap(), "{case}");
        let marks = w.obs.marks();
        assert!(
            marks.contains(&ObsMark::PruneDeclined {
                flush: 1,
                files: selection,
            }),
            "{case}"
        );
        assert!(!marks.iter().any(|m| matches!(m, ObsMark::PruneSelected { .. })), "{case}");
        assert!(!marks.contains(&ObsMark::BestEffortBegin), "{case}");
    }
}

/// X33 (DEC-211's negative): W ignores V's decline, and unlinks a file V's index still says it
/// holds.
#[cfg(feature = "planted-bugs")]
#[test]
fn x33_prunes_a_declined_selection() {
    let mut w = World::planted(1 << 20, four_files(), crate::Planted::X33);
    w.declines = true;
    w.connect(0, 20, 25, &[], PRUNE_TIP);
    let mut p = w.packet(0, false);
    p.prune = Some(decision(vec![0], true, None));
    let r = w.run(FlushMachine::new(p));
    assert!(r.steps.contains(&FlushStep::Unlink));
    assert!(w.meta().pruned_files);
    assert!(!w.on_disk(0));
}

/// Step 12's place after the durable writes: after the release, but before it for
/// `pruneblockchain`, which replies once V sees the release, and for a copy point and a live-copy
/// ALWAYS, after whose release W does nothing more (E-b1.8); one-batch or split alike.
#[test]
fn the_unlink_comes_before_a_release_after_which_w_is_idle() {
    use FlushStep::*;
    // (split, copy point, live-copy ALWAYS, `pruneblockchain`, the tail)
    let cases = [
        (false, true, false, None, vec![Bp, Unlink, AfterBk, Release]),
        (false, false, true, None, vec![Bp, Unlink, AfterBk, Release]),
        (true, true, false, None, vec![Bk, Unlink, AfterBk, Release]),
        (true, false, true, Some(15), vec![Bk, Unlink, AfterBk, Release]),
        (true, false, false, Some(15), vec![Bk, Unlink, Release, AfterBk]),
        (true, false, false, None, vec![Bk, Release, AfterBk, Unlink]),
    ];
    for (split, copy_point, live_always, manual, tail) in cases {
        let case =
            format!("split {split}, copy point {copy_point}, live {live_always}, {manual:?}");
        let mut w = World::with_files(
            if split {
                4 << 10
            } else {
                1 << 20
            },
            four_files(),
        );
        w.cfg.live_copy = live_always;
        w.connect(
            0,
            if split {
                600
            } else {
                20
            },
            25,
            &[],
            PRUNE_TIP,
        );
        let mut p = w.packet(0, copy_point);
        if live_always {
            p.mode = FlushStateMode::Always;
        }
        p.prune = Some(decision(vec![0], true, manual));
        let r = w.run(FlushMachine::new(p));
        assert_eq!(r.steps[r.steps.len() - tail.len()..], tail[..], "{case}");
        assert_eq!(r.done_at_release, tail.last() == Some(&Release), "{case}");
        assert!(r.meta_at_release.unwrap().pruned_files, "{case}");
        assert_eq!((w.on_disk(0), w.on_disk(1)), (false, true), "{case}");
    }
}

/// Step 12 is best-effort (CONS-28): a file it cannot remove, and a failed sync of the blocks
/// directory after the removals, are logged and left for the startup scan, and the flush ends as
/// usual, as Core's does (DEC-95).
#[test]
fn the_unlink_logs_what_fails_and_the_flush_goes_on() {
    for sync_fails in [false, true] {
        let mut w = World::with_files(1 << 20, four_files());
        w.connect(0, 20, 25, &[], PRUNE_TIP);
        let mut p = w.packet(0, false);
        p.prune = Some(decision(vec![0], true, None));
        let mut m = FlushMachine::new(p);
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let mut warnings = Vec::new();
        loop {
            let unlink = m.next_step() == Some(FlushStep::Unlink);
            w.fs.fail_remove.store(unlink && !sync_fails, Ordering::Relaxed);
            w.fs.fail_sync_dir.store(unlink && sync_fails, Ordering::Relaxed);
            let (step, warned) = logged_warnings(|| m.step(&mut env));
            warnings.extend(warned);
            match step.expect("no stop") {
                Step::PruneClears(req) => req.reply(vec![cleared(1)]),
                Step::Progress(_) | Step::Released(_) => {}
                Step::Done => break,
                s => panic!("{s:?}"),
            }
        }
        let fails = if sync_fails {
            "Prune: Failed to sync the blocks directory"
        } else {
            "Prune: Failed to remove file"
        };
        assert!(warnings.iter().any(|l| l.starts_with(fails)), "{warnings:?}");
        assert!(env.engine.latch().tripped().is_none());
        assert!(Meta::read(env.engine).unwrap().pruned_files);
        let marks = w.obs.marks();
        assert!(marks.contains(&ObsMark::BestEffortEnd));
        assert!(
            !marks.iter().any(|m| matches!(m, ObsMark::StoreFailed { .. } | ObsMark::Abort { .. }))
        );
        assert_eq!(w.on_disk(0), !sync_fails, "removed unless the removal failed");
    }
}

/// BL-124: a failed sync of `blocks/` at the files barrier is logged and passed over, as Core's
/// `DirectoryCommit` ignores it. The flush completes, one-batch or split, and nothing stops.
#[test]
fn a_failed_directory_sync_at_the_files_barrier_is_logged_and_the_flush_completes() {
    use FlushStep::*;
    for split in [false, true] {
        let mut w = World::new(if split {
            4 << 10
        } else {
            1 << 20
        });
        w.connect(
            0,
            if split {
                600
            } else {
                20
            },
            25,
            &[],
            7,
        );
        let p = w.packet(0, false);
        w.fs.fail_sync_dir.store(true, Ordering::Relaxed);
        let (r, warnings) = logged_warnings(|| w.run(FlushMachine::new(p)));
        assert_eq!(r.steps[..3], [Space, Files, B1], "split {split}");
        assert_eq!(r.steps.last(), Some(&AfterBk), "split {split}");
        let failed = |l: &String| l.starts_with("Failed to sync the blocks directory");
        assert!(warnings.iter().any(failed), "{warnings:?}");
        assert!(w.db.latch().tripped().is_none());
        assert_eq!(w.meta().tip, Some(block(7).hash), "split {split}");
        assert!(!w.obs.marks().iter().any(|m| matches!(m, ObsMark::StoreFailed { .. })));
    }
}

/// The warnings logged while `f` runs on this thread.
fn logged_warnings<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    struct Capture(Mutex<Vec<String>>);
    impl tracing::Subscriber for Capture {
        fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
            *m.level() == tracing::Level::WARN
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = format!("{v:?}");
                    }
                }
            }
            let mut m = Message(String::new());
            event.record(&mut m);
            self.0.lock().unwrap().push(m.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    // BL-479: never the only registered dispatcher, or a thread without one that registers a
    // callsite first caches `Interest::never` for it (dashd-store's `keep_callsites_registered`).
    static KEEP: OnceLock<tracing::Dispatch> = OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&capture), f);
    let lines = std::mem::take(&mut *capture.0.lock().unwrap());
    (out, lines)
}

/// W waits for V's answer without running anything; V's dropped request stops the flush after B1
/// with nothing pruned, unless a failure elsewhere has latched, and then W only stops.
#[test]
fn unanswered_clears_wait_and_dropped_ones_stop_the_flush() {
    for elsewhere in [false, true] {
        let mut w = World::with_files(1 << 20, four_files());
        w.connect(0, 20, 25, &[], PRUNE_TIP);
        let mut p = w.packet(0, false);
        p.prune = Some(decision(vec![0], true, None));
        let mut m = FlushMachine::new(p);
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let req = loop {
            match m.step(&mut env).unwrap() {
                Step::PruneClears(req) => break req,
                Step::Progress(_) => {}
                s => panic!("{s:?}"),
            }
        };
        assert_eq!(m.step(&mut env), Ok(Step::Waiting));
        assert_eq!(m.step(&mut env), Ok(Step::Waiting));
        assert_eq!(m.next_step(), Some(FlushStep::PruneApply));
        if elsewhere {
            env.engine.latch().trip(Failure {
                kind: ErrorKind::Other,
                site: "test",
                message: "a failure elsewhere".into(),
            });
        }
        drop(req);
        m.park(env.engine.latch()).unwrap();
        let r = m.step(&mut env);
        if elsewhere {
            assert_eq!(r, Ok(Step::Refused));
        } else {
            let abort = r.unwrap_err();
            assert_eq!(abort.log, "The block index dropped the prune clears unanswered");
            assert!(env.engine.latch().tripped().is_some());
        }
        assert_eq!(m.next_step(), None);
        let meta = Meta::read(env.engine).unwrap();
        assert_eq!((meta.tip, meta.pruned_files), (Some(block(PRUNE_TIP).hash), false));
        assert_ne!(w.files.read(|f| *f.file_info(0).unwrap()), BlockFileInfo::default());
        assert!(w.on_disk(0));
    }
}

/// D35/D43's registration tests: a world whose hub has a feed table, and a machine for a full
/// flush at `PRUNE_TIP` with a manual prune of files 0 and 1 (step 7 asks V for clears).
fn registration_world() -> (World, FlushMachine<()>) {
    use crate::subscriber::{ScriptedFeeds, TreeIndex};

    let mut w = World::with_files(1 << 20, four_files());
    let tree = TreeIndex::new();
    let feeds = ScriptedFeeds::new(4, vec![tree.genesis()]);
    w.hub = SubscriberHub::with_feeds(Arc::new(feeds), Arc::new(tree));
    w.connect(0, 20, 25, &[], PRUNE_TIP);
    let mut p = w.packet(0, false);
    p.prune = Some(decision(vec![0, 1], true, Some(15)));
    (w, FlushMachine::new(p))
}

fn late_registration(name: &'static str) -> crate::flush::Registration {
    crate::flush::Registration {
        name,
        needs: dash_chain::Needs::default(),
        mandatory: true,
        start_height: 0,
        durable: None,
    }
}

/// Sol r1's blocker 1 (D35): a registration that arrives after step 3b (once B1 ran), or between
/// step 7's selection and its apply (once PruneSelect ran), waits until the flush releases, so
/// its lock entry and prune check come after the prune.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for a registrant")]
fn a_registration_waits_from_step_3b_to_the_release() {
    use std::sync::mpsc;
    use std::time::Duration;

    for at in [FlushStep::B1, FlushStep::PruneSelect] {
        let (w, mut m) = registration_world();
        let latch = GlobalFailStop::new();
        let cleared = w.cleared.clone();
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let hub = env.hub;
        std::thread::scope(|sc| {
            let (tx, rx) = mpsc::channel();
            let mut registrant = Some(move || {
                let h = hub.register(late_registration("late"), &latch);
                tx.send(h.expect("not latched").expect("registers").id).expect("sent");
            });
            loop {
                // Checked before the step: the one that returns Released closes the window.
                if registrant.is_none() {
                    assert!(rx.try_recv().is_err(), "{at:?}: registered before the release");
                }
                let ran = m.next_step();
                let step = m.step(&mut env).expect("no failure");
                if ran == Some(at)
                    && let Some(f) = registrant.take()
                {
                    sc.spawn(f);
                    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "{at:?}: waits");
                }
                match step {
                    Step::PruneClears(req) => req.reply(cleared.clone()),
                    Step::Released(_) => break,
                    Step::Progress(_) => {}
                    s => panic!("{s:?}"),
                }
            }
            let id = rx.recv_timeout(Duration::from_secs(60)).expect("registered after it");
            assert_eq!(hub.durable_locator(id), None);
            assert_eq!(hub.actual_locks(&block(PRUNE_TIP)), [("late", 0)]);
        });
    }
}

/// D43: a registration on W's thread, or on V's once step 7 waits on V's clears, would wait on
/// itself, so it is `WouldDeadlock`; V waiting in the window before then is woken and refused
/// when W asks it, and W's clears are then answered.
#[test]
#[expect(clippy::disallowed_methods, reason = "plain test threads standing in for V")]
fn a_registration_that_the_flush_waits_on_is_refused() {
    use std::sync::mpsc;
    use std::time::Duration;

    use dash_chain::Refused;

    let (w, mut m) = registration_world();
    let latch = GlobalFailStop::new();
    let cleared = w.cleared.clone();
    let mut env = w.env(CrashRoll::seeded(0, 0));
    let hub = env.hub;
    let refused = |r: Result<Result<crate::flush::SubscriberHandle, Refused>, Fatal>| {
        matches!(r.expect("not latched"), Err(Refused::WouldDeadlock))
    };
    std::thread::scope(|sc| {
        let (tx, rx) = mpsc::channel();
        let mut v = None;
        loop {
            let ran = m.next_step();
            let step = m.step(&mut env).expect("no failure");
            if ran == Some(FlushStep::B1) {
                assert!(refused(hub.register(late_registration("w"), &latch)), "W's own thread");
                let (tx, latch) = (tx.clone(), &latch);
                v = Some(sc.spawn(move || {
                    hub.submitted_here();
                    tx.send(refused(hub.register(late_registration("v"), latch))).expect("sent");
                }));
                assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "V waits");
            }
            match step {
                Step::PruneClears(req) => {
                    assert!(rx.recv_timeout(Duration::from_secs(60)).expect("woken"), "refused");
                    req.reply(cleared.clone());
                }
                Step::Released(_) => break,
                Step::Progress(_) => {}
                s => panic!("{s:?}"),
            }
        }
        v.expect("spawned").join().expect("V's thread");
        assert!(
            !refused(hub.register(late_registration("after"), &latch)),
            "no window: W's thread registers"
        );
    });
}

/// A batch that records its operations, sized as RocksDB's: a 12-byte header, then per record a
/// tag, the column family, both lengths and the bytes.
#[derive(Default)]
struct OpLog(Vec<(Vec<u8>, Option<Vec<u8>>)>);

impl KvBatch for OpLog {
    fn put(&mut self, _: dashd_store::Cf, key: &[u8], value: &[u8]) {
        self.0.push((key.to_vec(), Some(value.to_vec())));
    }
    fn delete(&mut self, _: dashd_store::Cf, key: &[u8]) {
        self.0.push((key.to_vec(), None));
    }
    fn delete_range(&mut self, _: dashd_store::Cf, _: &[u8], _: &[u8]) {
        unreachable!("no range in a coin batch");
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn size_bytes(&self) -> usize {
        12 + self
            .0
            .iter()
            .map(|(k, v)| 4 + k.len() + v.as_ref().map_or(0, |v| 2 + v.len()))
            .sum::<usize>()
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

/// D40: a freeze puts V's dirty block-index records into B1's batch, in `take_dirty`'s order and
/// keyed by hash, as B1 put them before; only the planted bugs keep a copy.
#[test]
fn a_frozen_index_puts_the_records_into_b1_s_batch_in_order() {
    let writes: Vec<IndexWrite> = (1..4)
        .map(|h| IndexWrite {
            hash: block(h).hash,
            bytes: vec![h as u8; 5 + h as usize],
        })
        .collect();
    /// Puts with their column family.
    #[derive(Default)]
    struct CfLog(Vec<(usize, Vec<u8>, Vec<u8>)>);
    impl KvBatch for CfLog {
        fn put(&mut self, cf: dashd_store::Cf, key: &[u8], value: &[u8]) {
            self.0.push((cf.index(), key.to_vec(), value.to_vec()));
        }
        fn delete(&mut self, _: dashd_store::Cf, _: &[u8]) {
            unreachable!("records are puts");
        }
        fn delete_range(&mut self, _: dashd_store::Cf, _: &[u8], _: &[u8]) {
            unreachable!("records are puts");
        }
        fn len(&self) -> usize {
            self.0.len()
        }
        fn size_bytes(&self) -> usize {
            0
        }
        fn clear(&mut self) {
            self.0.clear();
        }
    }
    let (b, _, copy) =
        FrozenIndex::take(CfLog::default(), &mut writes.clone(), None).into_parts::<CfLog>();
    let cf = CHAINSTATE_SCHEMA.cf("block_index").unwrap().index();
    let want: Vec<_> =
        writes.iter().map(|w| (cf, w.hash.as_bytes().to_vec(), w.bytes.clone())).collect();
    assert_eq!(b.0, want);
    #[cfg(feature = "planted-bugs")]
    assert_eq!(copy, writes);
    #[cfg(not(feature = "planted-bugs"))]
    assert!(copy.is_empty());
}

/// D37: a sub-batch puts its coins in key order, yet holds the entries layer order put in it and
/// ends where layer order ends, at every bound; a batch already holding the head counts too.
#[test]
fn a_sub_batch_puts_layer_orders_entries_in_key_order() {
    use crate::coins::layer::{CoinLayerOps, Flags};
    let mut layer = ShardedLayer::new(1 << 20, Salt::fixed(7));
    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    for i in 0..2_000u32 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let mut k = [0; 36];
        k[..8].copy_from_slice(&x.to_le_bytes());
        k[32..].copy_from_slice(&(i % 3).to_le_bytes());
        let (flags, coin) = match i % 5 {
            0 => (Flags::DIRTY | Flags::SPENT, vec![]),
            1 => (Flags::NONE, vec![1; 30]),
            _ => (Flags::DIRTY, vec![2; 20 + (i % 40) as usize]),
        };
        layer.upsert(&OutPointKey(k), flags, &coin);
    }
    let reference = |head: usize, at: LayerPos, bound: u64| {
        let (mut b, table) = (OpLog::default(), CoinsTable::default());
        b.0.extend((0..head).map(|i| (vec![i as u8], None)));
        let mut end = at;
        for (next, r) in layer.iter_dirty_refs_from(at) {
            match layer.dirty_entry(r) {
                (f, _) if f.is_spent() => table.erase(&mut b, r.key()),
                (_, coin) => table.put(&mut b, r.key(), coin),
            }
            end = next;
            if b.size_bytes() as u64 > bound {
                return (b, end, true);
            }
        }
        (b, end, false)
    };
    // One buffer for every sub-batch, at exactly the DIRTY count (D46).
    let mut kept = Vec::new();
    for bound in [0, 100, 4_096, 20_000, 1 << 30] {
        for head in [0, 3] {
            let mut at = LayerPos::default();
            loop {
                let (mut want, want_end, want_passed) = reference(head, at, bound);
                let mut b = OpLog::default();
                b.0.extend((0..head).map(|i| (vec![i as u8], None)));
                let (end, passed) =
                    put_dirty(&mut b, &layer, at, Some((bound, &mut OpLog::default())), &mut kept);
                assert_eq!((end, passed), (want_end, want_passed), "bound {bound}");
                assert_eq!(
                    (kept.len(), kept.capacity()),
                    (0, layer.dirty_len()),
                    "kept, no growth"
                );
                let (mut got, mut want) = (b.0.split_off(head), want.0.split_off(head));
                assert!(got.is_sorted_by(|x, y| x.0 <= y.0), "key order, bound {bound}");
                got.sort();
                want.sort();
                assert_eq!(got, want, "the same entries, bound {bound}");
                if !passed {
                    break;
                }
                at = end;
            }
        }
    }
}

/// `recycle` hands its allocation on: the sub-batches' buffer is allocated once per flush (D46).
#[test]
fn recycling_a_buffer_keeps_its_allocation() {
    let mut v: Vec<usize> = Vec::with_capacity(1_000);
    v.extend(0..10);
    let p = v.as_ptr() as usize;
    let refs: Vec<&u64> = recycle(v);
    assert_eq!((refs.len(), refs.capacity(), refs.as_ptr() as usize), (0, 1_000, p));
    let back: Vec<usize> = recycle(refs);
    assert_eq!((back.capacity(), back.as_ptr() as usize), (1_000, p));
}

/// `p` carrying `delta` in place of its `()`.
fn with_delta<D: StateDelta>(p: FreezePacket<()>, delta: D) -> FreezePacket<D> {
    let FreezePacket {
        flush_id,
        coins,
        old,
        delta: (),
        index,
        presumed_effects_seq,
        tip_seq,
        causes,
        mode,
        prune,
        decided_at,
        copy_point,
    } = p;
    FreezePacket {
        flush_id,
        coins,
        old,
        delta,
        index,
        presumed_effects_seq,
        tip_seq,
        causes,
        mode,
        prune,
        decided_at,
        copy_point,
    }
}

/// An evo window at `tip`: a credit-data row of `tip` and the tombstones of a commitment mined
/// in it, whose rows are already on disk.
fn evo_window(
    db: &Db,
    tip: BlockRef,
) -> (crate::evo::EvoWindowDelta, Vec<(dashd_store::Cf, Vec<u8>)>) {
    use crate::evo::codec::{self as evo_codec, EvoCf};
    use crate::evo::{EvoLayer, testkit};
    use crate::flush::EvoWindow;
    use dash_dml::creditpool::CreditBlockData;
    use dash_dml::store::MinedErase;
    use dash_primitives::{LlmqType, QuorumHash};

    let q = QuorumHash::from([5; 32]);
    let qkey = evo_codec::mined_key(LlmqType::Llmq50_60, &q);
    let hkey = evo_codec::index_key(LlmqType::Llmq50_60, None, tip.height);
    let mut b = db.batch();
    for key in [&qkey, &hkey] {
        let (cf, key) = evo_codec::split_mined_key(key);
        b.put(cf.cf(), key, &[1]);
    }
    db.write(b, Commit::Durable).unwrap();
    let delta = dash_dml::EvoDelta {
        block: tip.hash,
        height: tip.height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: Some(CreditBlockData {
            balance: 7,
            unlocked: 0,
        }),
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: vec![MinedErase {
            llmq_type: LlmqType::Llmq50_60,
            quorum_hash: q,
            mined_height: tip.height,
            rotation_index: None,
        }],
        evo_tip: Some(tip.hash),
    };
    let mut layer = EvoLayer::new();
    layer.commit(testkit::codec().rows(&delta).unwrap(), db).unwrap();
    let erased = [qkey, hkey]
        .iter()
        .map(|k| {
            let (cf, key) = evo_codec::split_mined_key(k);
            (cf.cf(), key.to_vec())
        })
        .collect();
    let data = (EvoCf::CreditData.cf(), EvoCf::CreditData.key(&tip.hash).as_bytes().to_vec());
    (layer.take(), [vec![data], erased].concat())
}

/// The evodb pair's lines among `marks`, and where B1 returned Durable.
fn evodb_pair(marks: &[ObsMark]) -> (Vec<usize>, Option<usize>) {
    let pair = marks
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            ObsMark::Log {
                cat: LogCat::Always,
                line,
            } if line.starts_with("FlushStateToDisk: write evodb cache to disk ") => Some(i),
            _ => None,
        })
        .collect();
    let durable = marks.iter().position(|m| matches!(m, ObsMark::Durable { .. }));
    (pair, durable)
}

/// CH7 D42: a failed B1 still closes the evodb pair, as Core's `Timer` logs its completed line while
/// the failed write's exception unwinds, before the flush's abort: B1 with its coins, and a split
/// flush's B1 alone.
#[test]
fn a_failed_b1_still_closes_the_evodb_pair() {
    for sub_batch in [1 << 20, 4 << 10] {
        let mut w = World::new(1 << 20);
        w.cfg.sub_batch = sub_batch;
        w.connect(0, 400, 25, &[], 1);
        w.save_block(1);
        let (window, _) = evo_window(&w.db, block(1));
        let mut m = FlushMachine::new(with_delta(w.packet(0, false), window));
        w.db.inner().crash_at_write(1);
        let mut env = w.env(CrashRoll::seeded(0, 0));
        let next = loop {
            let next = m.next_step();
            match m.step(&mut env) {
                Ok(Step::Done) => panic!("sub-batch {sub_batch}: no failure"),
                Ok(_) => {}
                Err(_) => break next,
            }
        };
        assert_eq!(next, Some(FlushStep::B1), "sub-batch {sub_batch}");
        let (pair, durable) = evodb_pair(&w.obs.marks());
        assert_eq!((pair.len(), durable), (2, None), "sub-batch {sub_batch}");
        // BL-485: Core's `HandleError` logs inside the pair, before `~Timer`'s completed line.
        let lines = always_lines_and_failure(&w.obs.marks());
        let started = "FlushStateToDisk: write evodb cache to disk started";
        let at = lines.iter().position(|l| l == started).expect("the pair");
        assert_eq!(lines[at + 2], DEBUG_LEVELDB, "sub-batch {sub_batch}: {lines:?}");
        let completed = "FlushStateToDisk: write evodb cache to disk completed";
        assert!(lines[at + 3].starts_with(completed), "sub-batch {sub_batch}: {lines:?}");
        assert_eq!(lines[at + 4], "StoreFailed(write)", "sub-batch {sub_batch}: {lines:?}");
    }
}

/// BL-485: a store call the guard refuses (another failure latched, and its caller reports it)
/// logs no `HandleError` lines and no `AbortNode`: the flush is refused.
#[test]
fn a_refused_store_call_logs_no_handle_error_lines() {
    let mut w = World::new(1 << 20);
    w.connect(0, 10, 25, &[], 1);
    let p = w.packet(0, false);
    let mut m = FlushMachine::new(p);
    let mut env = w.env(CrashRoll::seeded(0, 0));
    assert!(matches!(m.step(&mut env), Ok(Step::Progress(FlushStep::Space))));
    assert!(matches!(m.step(&mut env), Ok(Step::Progress(FlushStep::Files))));
    env.engine.latch().trip(Failure {
        kind: ErrorKind::Other,
        site: "test",
        message: "a failure elsewhere".into(),
    });
    assert_eq!(m.step(&mut env), Ok(Step::Refused));
    let lines = always_lines_and_failure(&w.obs.marks());
    assert!(!lines.iter().any(|l| l == DEBUG_LEVELDB || l.starts_with("***")), "{lines:?}");
}

/// CH7 D7 (§7.2 L0): a split flush's durable B1, before any coin batch, holds the evo window's
/// row and its tombstones' deletes at `new`, the marker naming (`new`, `old`), `meta.evo_tip =
/// new` and no tip: the image a kill between B1 and Bk leaves. The recovery tests replay from it.
#[test]
fn a_split_flushs_b1_alone_holds_the_evo_window_and_the_marker() {
    let mut w = World::new(1 << 20);
    w.connect(0, 50, 25, &[], 1);
    w.save_block(1);
    let p = w.packet(0, false);
    let r = w.run(FlushMachine::new(p));
    w.release(&r.report.unwrap());
    w.cfg.sub_batch = 4 << 10;
    w.connect(50, 400, 25, &[], 2);
    w.save_block(2);
    let (window, rows) = evo_window(&w.db, block(2));
    let mut m = FlushMachine::new(with_delta(w.packet(0, false), window));
    let mut env = w.env(CrashRoll::seeded(0, 0));
    let mut steps = Vec::new();
    while let Step::Progress(s) = m.step(&mut env).expect("no failure") {
        steps.push(s);
        if s == FlushStep::Durable {
            break;
        }
    }
    use FlushStep::*;
    assert_eq!(steps, [Space, Files, B1, Durable]);
    let unprobed = w.db.inner().unprobed();
    assert_eq!(unprobed.get(rows[0].0, &rows[0].1).unwrap().map(|v| v.len()), Some(16));
    for (cf, key) in &rows[1..] {
        assert_eq!(unprobed.get(*cf, key).unwrap(), None, "a tombstone's delete");
    }
    let meta = w.meta();
    assert_eq!(
        (meta.head_blocks, meta.tip, meta.evo_tip),
        (Some((block(2).hash, Some(block(1).hash))), None, Some(block(2).hash))
    );
}

/// CH7 D7 and D20: B1 carries the evo window (its row puts, its tombstones' deletes) with
/// `meta.evo_tip`, and Core's "write evodb cache to disk" pair brackets B1's durable write, coins
/// included when they fit; a window with no evo state prints no pair.
#[test]
fn b1_writes_the_evo_window_inside_the_evodb_pair() {
    let mut w = World::new(1 << 20);
    w.connect(0, 50, 25, &[], 1);
    w.save_block(1);
    let (window, rows) = evo_window(&w.db, block(1));
    let p = with_delta(w.packet(0, false), window);
    let r = w.run_evo(FlushMachine::new(p));
    use FlushStep::*;
    assert_eq!(r.steps, [Space, Files, B1, Durable, Release, AfterBk]);
    let unprobed = w.db.inner().unprobed();
    assert_eq!(unprobed.get(rows[0].0, &rows[0].1).unwrap().map(|v| v.len()), Some(16));
    for (cf, key) in &rows[1..] {
        assert_eq!(unprobed.get(*cf, key).unwrap(), None, "a tombstone's delete");
    }
    assert_eq!(r.meta_at_release.unwrap().evo_tip, Some(block(1).hash));
    let marks = w.obs.marks();
    let (pair, durable) = evodb_pair(&marks);
    assert_eq!(pair.len(), 2);
    let line = |i: usize| match &marks[i] {
        ObsMark::Log {
            line,
            ..
        } => line.clone(),
        _ => unreachable!(),
    };
    assert_eq!(line(pair[0]), "FlushStateToDisk: write evodb cache to disk started");
    let done = line(pair[1]);
    assert!(
        done.starts_with("FlushStateToDisk: write evodb cache to disk completed (")
            && done.ends_with("s)"),
        "{done}"
    );
    assert!(pair[1] < durable.unwrap());
    // One batch: the coins' commit line is inside the pair.
    let committed = marks.iter().position(|m| {
        matches!(m, ObsMark::Log { cat: LogCat::CoinDb, line } if line.starts_with("Committed "))
    });
    assert!((pair[0]..pair[1]).contains(&committed.unwrap()));

    // A split flush: the pair is around B1 alone, before the coin batches.
    let report = r.report.unwrap();
    w.release(&report);
    w.cfg.sub_batch = 4 << 10;
    w.connect(50, 400, 25, &[], 2);
    w.save_block(2);
    let (window, _) = evo_window(&w.db, block(2));
    let p = with_delta(w.packet(0, false), window);
    let r = w.run_evo(FlushMachine::new(p));
    assert_eq!(r.steps[..5], [Space, Files, B1, Durable, SubBatch(0)]);
    let marks = w.obs.marks();
    let (pair, _) = evodb_pair(&marks);
    let committed = marks.iter().rposition(|m| {
        matches!(m, ObsMark::Log { cat: LogCat::CoinDb, line } if line.starts_with("Committed "))
    });
    assert_eq!(pair.len(), 4);
    assert!(pair[3] < committed.unwrap(), "the coins commit after the pair");

    // No evo state, no pair.
    w.release(&r.report.unwrap());
    w.cfg.sub_batch = 1 << 20;
    w.connect(450, 10, 25, &[], 3);
    w.save_block(3);
    let p = w.packet(0, false);
    w.run(FlushMachine::new(p));
    assert_eq!(evodb_pair(&w.obs.marks()).0.len(), 4);
}
