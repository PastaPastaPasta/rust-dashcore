//! `coins_flush` (CH2 §5.7's PF0 rows; CH2b b1): the flush, the shutdown flush, recovery and the
//! chainstate's open, on RocksDB at `-dbcache=300` with the default indexes, through the real
//! `FlushControl`, `InlineDriver` (or `ThreadDriver`) and `FlushMachine`.
//!
//! - `replay/synth-{mainnet-453371,testnet-435990}-p300`: V connects 2,000 blocks of a synthetic
//!   churn over a chainstate holding the window's live set, then flushes ALWAYS, as at shutdown,
//!   and W runs to its last step. The churn has the shape of ST1's trace over its last 2,000
//!   blocks (`data/*-churn.txt`, from `data/churn_profile.py`): the live set's size, lengths and
//!   ages, each block's creates and spends, and the ages of the coins spent.
//! - `replay/synth-{mainnet-453371,testnet-435990}-thread-p300` (CH2c D13): the same replay with W
//!   on its own thread (`ThreadDriver`), as the node runs it; V waits for the shutdown flush's
//!   release and stops.
//! - `flush/split-64m`: one 64 MiB F written as 16 MiB sub-batches. W's steps are checked exactly;
//!   the machine's unit tests check each step's store calls.
//! - `shutdown/always-p300`: one ALWAYS flush of a coins cache filled to LARGE of a B that is the
//!   coins share alone, without the mempool's allowance, so the run stays bounded. Its memtables
//!   hold the whole flush (below).
//! - `recovery/mainnet-2000`: the start after a crash image, the mainnet replay's flush stopped
//!   after B1: the engine's open, then recovery replays the 2,000 blocks from the block files. The
//!   node runs with `-dbbatchsize=1048576`, so that the window's F (about 4.5 MB) is split.
//! - `chainstate/open-b1-256k`: the engine's open and recovery's steps 0-3 over a crash image whose
//!   B1, with 262,144 index records, is only in the write-ahead log (its memtable flush stalled).
//!
//! D-b1-h: PF0 benches read only tracked inputs, so the replay rows run a synthetic churn with the
//! trace's shape rather than the ~1 GB traces themselves, which stay CH2c's wall-clock workloads.
//! `p4096` and `dec43` wait for those: in a 2,000-block window no budget binds.
//!
//! PF0 counts the whole process, RocksDB's threads included, so a region does the same work on
//! every run and ends with no background job left:
//! - the databases are quiet (`RocksConfig::quiet`, D-b1-i): no automatic compactions, RocksDB's
//!   own work, whose timing varies (CH2c's wall-clock rows keep them; level 0 then never stops
//!   writes), and a `LOG` of warnings and errors only, so no timer dumps statistics into it or
//!   writes it out. The open row opens as a node does: its log adds at most one level-0 table per
//!   column family to tables compacted to the last level, which starts no compaction, and the
//!   statistics dump and `LOG` flush that run as the open ends write a few KiB to the `LOG`;
//! - a region ends once no flush or compaction is scheduled or running (`RocksTuning::wait_idle`),
//!   and so starts, unless it opens the engine itself. The split row waits so after each of W's
//!   steps too, so flushes do not pile up across steps (a memtable marked full is switched only at
//!   the next write, so its flush still overlaps the next step);
//! - the shutdown row's database has memtables its flush does not fill ([`Sizing::unflushed`]):
//!   with the chainstate's 16 MiB ones, its sub-batches met the memtable-limit stop in about 1 run
//!   in 4, and DEC-164's retry added 98-182 allocations to that run. DEC-164 allows at most three
//!   memtables per column family, so a larger memtable, not more of them, keeps that stop out of
//!   reach: W's writes fill one, and the post-Bk flush writes it;
//! - fixtures are written with no background flush ([`Sizing::unflushed`]), then compacted.
//!
//! Setup runs outside the measured region and routines return their state, so it drops after. A
//! process runs one workload and builds only its own fixture. The bench builds without the crate's
//! test features, as `coins_micro` does.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_chain::{FlatFilePos as IndexPos, IndexAccess, IndexWrite, Status};
use dash_consensus::BlockId;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType, core_bytes,
};
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_chainstate::caches::{CacheSizes, IndexArgs};
use dashd_chainstate::coins::{
    BlockOverlay, BlockRef, CoinSrc, CoinsBudget, CoinsCache, CoinsCacheSizeState, CoinsTable,
    OutPointKey, PackedCoin, Ratio, Salt,
};
use dashd_chainstate::flush::{
    CrashRoll, FlushAction, FlushConfig, FlushControl, FlushCx, FlushDriver, FlushEnv, FlushPolicy,
    FlushStateMode, FlushStep, InlineDriver, NoEvo, NoPrune, NullObserver, NullSignals,
    SubscriberHub, ThreadDriver, WThread,
};
use dashd_chainstate::recovery::{ChainstateOpen, RecoveryConfig};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, Meta, RecordingSink,
};
use dashd_chainstate::tuning::{
    DEFAULT_DB_BATCH_SIZE, EngineMemory, EngineTuning, RocksTuning, sub_batch,
};
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};
use dashd_store::{Commit, KvBatch, KvEngine, RocksConfig, RocksEngine};
use tempfile::TempDir;

type Db = GuardedEngine<RocksEngine>;
type Ctl<'a> = FlushControl<InlineDriver<'a, RocksEngine, OsFs, RocksTuning, ()>>;

const MIB: u64 = 1 << 20;
/// Mainnet's `pchMessageStart`.
const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];
/// `-maxmempool`'s default (`txmempool.h:33`).
const MAX_MEMPOOL: u64 = 300_000_000;
const TIME: u32 = 1_700_000_000;
/// The serialized block index record's size, about Core's `CDiskBlockIndex` without its hash.
const RECORD_BYTES: usize = 120;
/// Core's `COINBASE_MATURITY`.
const MATURITY: u32 = 100;
/// The synthetic chains' seed.
const SEED: u64 = 0xc4a1;
const MAINNET: &str = include_str!("data/mainnet-453371-churn.txt");
const TESTNET: &str = include_str!("data/testnet-435990-churn.txt");

/// splitmix64: the fixtures' randomness, fixed by their seeds.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes()[..chunk.len()]);
        }
        out
    }
}

/// Values with counts, sampled in proportion.
#[derive(Default)]
struct Dist {
    values: Vec<u32>,
    cumulative: Vec<u64>,
}

impl Dist {
    fn push(&mut self, value: u32, count: u64) {
        self.cumulative.push(self.cumulative.last().copied().unwrap_or(0) + count);
        self.values.push(value);
    }

    fn sample(&self, rng: &mut Rng) -> u32 {
        let x = rng.below(*self.cumulative.last().expect("a non-empty distribution"));
        self.values[self.cumulative.partition_point(|&c| c <= x)]
    }
}

/// A churn profile (`data/churn_profile.py`).
#[derive(Default)]
struct Profile {
    first: u32,
    live: usize,
    live_len: Dist,
    start_age: Dist,
    len: Dist,
    blocks: Vec<BlockChurn>,
}

/// A block's creates, and the age buckets of the coins it spends.
struct BlockChurn {
    creates: u32,
    ages: Vec<u32>,
}

impl Profile {
    /// Reads the fields the generator uses: of `window` its first height, of `live` its count.
    /// `spend_age` sums the blocks' ages.
    fn parse(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut fields = line.split_whitespace();
            let tag = fields.next().expect("a tag");
            let mut num = || -> u64 { fields.next().expect("a field").parse().expect("a number") };
            match tag {
                "window" => p.first = num() as u32,
                "live" => p.live = num() as usize,
                "live_len" => p.live_len.push(num() as u32, num()),
                "start_age" => p.start_age.push(num() as u32, num()),
                "spend_age" => {}
                "len" => p.len.push(num() as u32, num()),
                "block" => {
                    let creates = num() as u32;
                    let spends = num() as usize;
                    let mut ages = Vec::with_capacity(spends);
                    for field in fields.by_ref() {
                        let (b, n) = field.split_once(':').expect("bucket:count");
                        let (b, n): (u32, usize) = (b.parse().unwrap(), n.parse().unwrap());
                        ages.extend(std::iter::repeat_n(b, n));
                    }
                    assert_eq!(ages.len(), spends, "{line}");
                    p.blocks.push(BlockChurn {
                        creates,
                        ages,
                    });
                }
                _ => panic!("profile line {line:?}"),
            }
        }
        p
    }
}

/// The youngest and oldest ages of log2 bucket `b`, `churn_profile.py`'s: `2^b - 1..=2^(b+1) - 2`.
fn bucket_ages(b: u32) -> (u32, u32) {
    ((1 << b) - 1, (1 << (b + 1)) - 2)
}

/// An age in bucket `b`, and at most `max`.
fn age_in(b: u32, max: u32, rng: &mut Rng) -> u32 {
    let (lo, hi) = bucket_ages(b);
    let (lo, hi) = (lo.min(max), hi.min(max));
    lo + rng.below(u64::from(hi - lo + 1)) as u32
}

fn outputs(len: &Dist, n: u32, rng: &mut Rng) -> Vec<TxOut> {
    (0..n).map(|_| output(len.sample(rng), rng)).collect()
}

/// An output whose Core coin serialization is about `len` bytes: P2PKH up to 34, P2PK above
/// (Core compresses both scripts, and ST1's lengths are Core's).
fn output(len: u32, rng: &mut Rng) -> TxOut {
    let script = if len < 35 {
        [&[0x76, 0xa9, 0x14][..], &rng.bytes::<20>(), &[0x88, 0xac]].concat()
    } else {
        [&[0x21, 0x02][..], &rng.bytes::<32>(), &[0xac]].concat()
    };
    TxOut {
        value: 1 + rng.below(1_000_000_000) as i64,
        script_pubkey: script.into(),
    }
}

fn coinbase(height: u32, mut outputs: Vec<TxOut>) -> Arc<Tx> {
    // A block that creates no coin still pays its coinbase, to an unspendable output.
    if outputs.is_empty() {
        outputs.push(TxOut {
            value: 0,
            script_pubkey: vec![0x6a].into(),
        });
    }
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([0; 32]),
                index: u32::MAX,
            },
            script_sig: height.to_le_bytes().to_vec().into(),
            sequence: u32::MAX,
        }],
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// A transaction spending `inputs` with P2PKH-sized signatures.
fn spend(inputs: Vec<OutPoint>, outputs: Vec<TxOut>) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: inputs
            .into_iter()
            .map(|prevout| TxIn {
                prevout,
                script_sig: vec![0x48; 107].into(),
                sequence: u32::MAX,
            })
            .collect(),
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// `tx`'s first `n` outpoints.
fn outpoints(tx: &Tx, n: u32) -> impl Iterator<Item = OutPoint> {
    let hash = tx.txid();
    (0..n).map(move |index| OutPoint {
        hash,
        index,
    })
}

/// A block on `prev` with `txs`, at the first nonce whose hash meets the regtest limit its header
/// claims (about every other one does): recovery checks each block it reads against it.
fn block(prev: BlockHash, txs: Vec<Arc<Tx>>) -> Block {
    let mut header = BlockHeader {
        version: 4,
        prev_hash: prev,
        merkle_root: MerkleRoot::from([0; 32]),
        time: TIME,
        bits: 0x207f_ffff,
        nonce: 0,
    };
    (header.merkle_root, _) = Block::new(Header::new(header), txs.clone()).merkle();
    loop {
        let block = Block::new(Header::new(header), txs.clone());
        if check_proof_of_work(block.hash().as_bytes(), CompactTarget::new(header.bits), &pow()) {
            return block;
        }
        header.nonce += 1;
    }
}

/// Regtest's proof-of-work parameters, whose limit the bench's blocks claim.
fn pow() -> PowParams {
    static POW: LazyLock<PowParams> = LazyLock::new(|| {
        let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build();
        PowParams::from_chain_params(&params.unwrap()).unwrap()
    });
    *POW
}

/// A block index record's bytes.
fn record(seed: u64) -> Vec<u8> {
    Rng(seed).bytes::<RECORD_BYTES>().to_vec()
}

/// The bench chain's block index: one branch from `old`, ids by height.
struct Index {
    base: u32,
    hashes: Vec<BlockHash>,
    ids: BTreeMap<BlockHash, BlockId>,
    data: Vec<Option<IndexPos>>,
}

impl Index {
    fn new(base: u32) -> Self {
        Self {
            base,
            hashes: Vec::new(),
            ids: BTreeMap::new(),
            data: Vec::new(),
        }
    }

    fn push(&mut self, hash: BlockHash) -> BlockRef {
        let id = BlockId(self.hashes.len() as u32);
        self.hashes.push(hash);
        self.ids.insert(hash, id);
        self.data.push(None);
        BlockRef {
            id,
            hash,
            height: self.base + id.0,
        }
    }

    fn contains(&self, id: BlockId) -> bool {
        (id.0 as usize) < self.hashes.len()
    }
}

impl IndexAccess for Index {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.ids.get(h).copied()
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.hashes.get(id.0 as usize).copied()
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (self.base..=self.height(id)?).contains(&height).then(|| BlockId(height - self.base))
    }

    /// One branch: the older block.
    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        (self.contains(a) && self.contains(b)).then(|| a.min(b))
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.contains(id).then(|| self.base + id.0)
    }

    fn have_data_pos(&self, id: BlockId) -> Option<IndexPos> {
        self.data.get(id.0 as usize).copied().flatten()
    }

    fn undo_pos(&self, _: BlockId) -> Option<IndexPos> {
        None
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        let data = self.have_data_pos(id).map_or(0, |_| Status::HAVE_DATA);
        self.contains(id).then_some(Status(data))
    }
}

/// The spendable outpoints by height, with a Fenwick tree of their counts, so that a spend takes a
/// coin uniformly from a range of heights.
struct Pool {
    coins: Vec<Vec<OutPoint>>,
    /// Node `i` covers heights `i - lowbit(i)..i`.
    tree: Vec<u32>,
}

impl Pool {
    fn new(top: u32) -> Self {
        Self {
            coins: vec![Vec::new(); top as usize],
            tree: vec![0; top as usize + 1],
        }
    }

    fn add(&mut self, height: u32, outs: impl IntoIterator<Item = OutPoint>) {
        for out in outs {
            self.coins[height as usize].push(out);
            self.update(height, 1);
        }
    }

    fn update(&mut self, height: u32, delta: i32) {
        let mut i = height as usize + 1;
        while i < self.tree.len() {
            self.tree[i] = self.tree[i].wrapping_add_signed(delta);
            i += i & i.wrapping_neg();
        }
    }

    /// The coins below `height`.
    fn below(&self, height: u32) -> u32 {
        let (mut i, mut n) = (height as usize, 0);
        while i > 0 {
            n += self.tree[i];
            i &= i - 1;
        }
        n
    }

    /// The height of coin `k` in height order.
    fn height_of(&self, mut k: u32) -> u32 {
        let mut pos = 0;
        let mut step = (self.tree.len() - 1).next_power_of_two();
        while step > 0 {
            if pos + step < self.tree.len() && self.tree[pos + step] <= k {
                pos += step;
                k -= self.tree[pos];
            }
            step /= 2;
        }
        pos as u32
    }

    /// A coin of a height in `lo..=hi`, taken uniformly.
    fn take(&mut self, lo: u32, hi: u32, rng: &mut Rng) -> Option<OutPoint> {
        if lo > hi {
            return None;
        }
        let first = self.below(lo);
        let n = self.below(hi + 1) - first;
        if n == 0 {
            return None;
        }
        let h = self.height_of(first + rng.below(u64::from(n)) as u32);
        let at = &mut self.coins[h as usize];
        let out = at.swap_remove(rng.below(at.len() as u64) as usize);
        self.update(h, -1);
        Some(out)
    }
}

/// The synthetic window: `old`, the live set there, and the profile's blocks. Each block creates and
/// spends as many coins as the profile's, and each spend takes a coin of the age bucket the profile
/// has for it, uniformly: one of the live set, a coin of an earlier block of the window (a
/// coinbase's once mature), or at age 0 an output of an earlier transaction in its block.
struct Chain {
    old: BlockRef,
    /// The coins at `old`.
    live: Vec<(OutPointKey, PackedCoin)>,
    blocks: Vec<(BlockRef, Block)>,
    index: Index,
}

impl Chain {
    fn new(churn: &str) -> Self {
        let p = Profile::parse(churn);
        let mut rng = Rng(SEED);
        let mut pool = Pool::new(p.first + p.blocks.len() as u32);
        let mut live = Vec::with_capacity(p.live);
        for i in 0..p.live {
            let age = age_in(p.start_age.sample(&mut rng), p.first - 1, &mut rng);
            let height = p.first - age;
            let out = output(p.live_len.sample(&mut rng), &mut rng);
            let prevout = OutPoint {
                hash: TxHash::from(rng.bytes::<32>()),
                index: (i % 3) as u32,
            };
            live.push((
                OutPointKey::new(&prevout),
                PackedCoin::from_output(&out, height, false).unwrap(),
            ));
            pool.add(height, [prevout]);
        }
        let mut index = Index::new(p.first - 1);
        let old =
            index.push(block(BlockHash::from([0; 32]), vec![coinbase(p.first - 1, vec![])]).hash());
        let mut blocks = Vec::with_capacity(p.blocks.len());
        // The window's coinbase outputs by height, until they mature.
        let mut maturing = VecDeque::new();
        let mut prev = old;
        for (height, churn) in (p.first..).zip(&p.blocks) {
            let (creates, spends) = (churn.creates, churn.ages.len() as u32);
            while let Some(&(h, _)) = maturing.front()
                && height - h >= MATURITY
            {
                let (h, outs) = maturing.pop_front().expect("a front");
                pool.add(h, outs);
            }
            // The coinbase pays one coin, or all when nothing is spent.
            let paid = if spends == 0 {
                creates
            } else {
                creates.min(1)
            };
            let rest = creates - paid;
            // Each spend's age bucket, the profile's in a random order. A coin of age 0 is an output
            // of an earlier transaction: the first spends older coins only, and creates those the
            // block spends at age 0.
            let mut ages = churn.ages.clone();
            for i in (1..ages.len()).rev() {
                ages.swap(i, rng.below(i as u64 + 1) as usize);
            }
            let zeros = ages.iter().filter(|&&b| b == 0).count() as u32;
            assert!(zeros <= rest && (zeros < spends || spends == 0), "{height}: {zeros} at age 0");
            ages.sort_by_key(|&b| b == 0);
            // Two inputs a transaction, but the first spends one older coin, or two.
            let older = spends - zeros;
            let (head, tail) = ages.split_at(older.clamp(1, 2).min(spends) as usize);
            let groups: Vec<&[u32]> =
                std::iter::once(head).filter(|g| !g.is_empty()).chain(tail.chunks(2)).collect();
            let shared = rest - zeros;
            let n_tx = groups.len() as u32;
            let cb = coinbase(height, outputs(&p.len, paid, &mut rng));
            maturing.push_back((height, outpoints(&cb, paid).collect::<Vec<_>>()));
            let mut txs = vec![cb];
            let mut fresh = Vec::new();
            for (t, group) in (0..).zip(&groups) {
                let inputs = group
                    .iter()
                    .map(|&b| match b {
                        0 => fresh.swap_remove(rng.below(fresh.len() as u64) as usize),
                        b => {
                            let (young, old) = bucket_ages(b);
                            let (lo, hi) = (height.saturating_sub(old).max(1), height - young);
                            pool.take(lo, hi, &mut rng).expect("a coin of the profile's age")
                        }
                    })
                    .collect();
                let n_out = shared / n_tx
                    + u32::from(t < shared % n_tx)
                    + if t == 0 {
                        zeros
                    } else {
                        0
                    };
                let tx = spend(inputs, outputs(&p.len, n_out, &mut rng));
                fresh.extend(outpoints(&tx, n_out));
                txs.push(tx);
            }
            pool.add(height, fresh);
            let blk = block(prev.hash, txs);
            prev = index.push(blk.hash());
            blocks.push((prev, blk));
        }
        Self {
            old,
            live,
            blocks,
            index,
        }
    }

    fn new_tip(&self) -> BlockRef {
        self.blocks.last().expect("a window").0
    }
}

/// `-dbcache=300` at the default indexes: the caches' split, and S (`sub_batch`).
#[derive(Clone, Copy)]
struct Sizing {
    sizes: CacheSizes,
    sub_batch: u64,
}

fn p300() -> Sizing {
    p300_batch(DEFAULT_DB_BATCH_SIZE)
}

/// `-dbcache=300 -dbbatchsize=db_batch_size`.
fn p300_batch(db_batch_size: u64) -> Sizing {
    let sizes = CacheSizes::calculate(300, &IndexArgs::default());
    Sizing {
        sub_batch: sub_batch(db_batch_size, &sizes),
        sizes,
    }
}

impl Sizing {
    /// The chainstate's engine configuration (§2.5), quiet.
    fn config(&self) -> RocksConfig {
        quiet().config(&EngineMemory::for_chainstate(&self.sizes, self.sub_batch))
    }

    /// The chainstate's configuration with memtables that neither a fixture nor the shutdown
    /// row's flush fills (it fills 225 MiB), and no charged, stalling write buffer manager
    /// (RocksDB's own, of `total_write_buffer_bytes`, flushes only near 1 GiB): nothing is flushed
    /// in the background while they are written, and no write can stall. A crash image's B1 is
    /// then only in the write-ahead log, as after a stalled memtable flush.
    fn unflushed(&self) -> RocksConfig {
        RocksConfig {
            charged_write_buffer_bytes: None,
            write_buffer_bytes: 256 << 20,
            total_write_buffer_bytes: 1 << 30,
            cf_write_buffers: vec![("coins", 512 << 20, 2)],
            max_total_wal_bytes: 1 << 30,
            ..self.config()
        }
    }

    /// The chainstate's open with `tuning`.
    fn open_engine(&self, chainstate: &Path, tuning: &RocksTuning) -> RocksEngine {
        tuning.open_chainstate(chainstate, &self.sizes, self.sub_batch).unwrap().0
    }
}

/// The chainstate's tuning with quiet databases (see the module's notes).
fn quiet() -> RocksTuning {
    RocksTuning {
        base: RocksConfig {
            quiet: true,
            ..RocksConfig::default()
        },
    }
}

fn guarded(engine: RocksEngine) -> Db {
    GuardedEngine::new(engine, GlobalFailStop::new(), Arc::new(RecordingSink::default()))
}

/// A new datadir whose `chainstate/` is a copy of the closed database at `db`.
fn datadir_from(db: &Path) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let to = dir.path().join("chainstate");
    std::fs::create_dir_all(&to).unwrap();
    for entry in std::fs::read_dir(db).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
    dir
}

/// Asserts, when dropped, that the database at `.0` has `.1` table files: after the measured
/// region, which drops what a routine returns.
struct TablesAfter(PathBuf, usize);

impl Drop for TablesAfter {
    fn drop(&mut self) {
        assert_eq!(tables(&self.0).len(), self.1, "{}", self.0.display());
    }
}

/// The table files of the database at `db`.
fn tables(db: &Path) -> BTreeSet<OsString> {
    std::fs::read_dir(db)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().ends_with(".sst"))
        .collect()
}

/// A datadir: `chainstate/` on RocksDB and `blocks/`, with W's inputs.
struct Node {
    db: Arc<Db>,
    files: Arc<GuardedFiles<OsFs>>,
    fs: Arc<OsFs>,
    hub: Arc<SubscriberHub>,
    tuning: RocksTuning,
    cfg: FlushConfig,
    sizing: Sizing,
    dir: TempDir,
}

impl Node {
    /// Opens `dir`'s chainstate with `config`, with its block files empty.
    fn open(dir: TempDir, sizing: Sizing, config: &RocksConfig) -> Self {
        let blocks = dir.path().join("blocks");
        std::fs::create_dir_all(&blocks).unwrap();
        let chainstate = dir.path().join("chainstate");
        let (engine, _) = RocksEngine::open(&chainstate, &CHAINSTATE_SCHEMA, config).unwrap();
        let db = Arc::new(guarded(engine));
        let fs = Arc::new(OsFs::new());
        let files = BlockFiles::with_fs(
            fs.clone(),
            BlockFilesConfig::new(&blocks, MAGIC),
            BlockFilesState::default(),
        );
        Self {
            files: Arc::new(GuardedFiles::new(files, db.latch().clone())),
            db,
            fs,
            hub: Arc::default(),
            tuning: quiet(),
            cfg: FlushConfig {
                sub_batch: sizing.sub_batch,
                data_dir: dir.path().to_path_buf(),
                blocks_dir: blocks,
                live_copy: false,
            },
            sizing,
            dir,
        }
    }

    /// A node over an empty chainstate.
    fn empty(sizing: Sizing) -> Self {
        Self::open(tempfile::tempdir().unwrap(), sizing, &sizing.config())
    }

    /// A node over a copy of `template`'s chainstate.
    fn copy_of(template: &Path, sizing: Sizing) -> Self {
        Self::open(datadir_from(&template.join("chainstate")), sizing, &sizing.config())
    }

    fn control(&self) -> Ctl<'_> {
        let env = FlushEnv {
            engine: &*self.db,
            files: &self.files,
            fs: &self.fs,
            hub: &self.hub,
            obs: &NullObserver,
            tuning: &self.tuning,
            cfg: &self.cfg,
            covered_seq: 0,
            crash: CrashRoll::seeded(0, 0),
        };
        FlushControl::new(
            InlineDriver::new(env),
            FlushPolicy::new(false),
            Arc::new(NullSignals),
            Arc::default(),
            Arc::new(NullObserver),
        )
    }

    /// The control with W on its own thread (CH2c D13): the production `ThreadDriver`.
    fn thread_control(&self) -> FlushControl<ThreadDriver<()>> {
        let driver = ThreadDriver::spawn(WThread {
            engine: self.db.clone(),
            files: self.files.clone(),
            fs: self.fs.clone(),
            hub: self.hub.clone(),
            obs: Arc::new(NullObserver),
            tuning: Arc::new(self.tuning.clone()),
            cfg: self.cfg.clone(),
            covered_seq: 0,
            crash: CrashRoll::seeded(0, 0),
            on_crash: Box::new(|| {}),
        })
        .unwrap();
        FlushControl::new(
            driver,
            FlushPolicy::new(false),
            Arc::new(NullSignals),
            Arc::default(),
            Arc::new(NullObserver),
        )
    }

    /// V with `-maxmempool`'s allowance, over the chainstate, whose coins are at `durable`.
    fn v(&self, tip: BlockRef, durable: Option<BlockRef>) -> V {
        self.v_with(tip, durable, MAX_MEMPOOL)
    }

    fn v_with(&self, tip: BlockRef, durable: Option<BlockRef>, max_mempool: u64) -> V {
        let Sizing {
            sizes,
            sub_batch,
        } = self.sizing;
        let budget =
            CoinsBudget::for_engine(&sizes, max_mempool, Ratio::HALF, sub_batch, &self.tuning);
        V {
            coins: CoinsCache::new(self.db.clone(), budget, Salt::fixed(5), durable),
            tip,
            seq: 0,
            dirty: Vec::new(),
            scripted: false,
            now: Instant::now(),
        }
    }

    /// Waits until no flush or compaction is left: a region's end.
    fn idle(&self) {
        RocksTuning::wait_idle(&self.db).unwrap();
    }

    /// Closes the database, which nothing else may hold, and returns the datadir.
    fn close(self) -> TempDir {
        let Self {
            db,
            dir,
            ..
        } = self;
        drop(Arc::into_inner(db).expect("only the node holds the database"));
        dir
    }
}

/// V: its coins cache, its tip and `seq`, and the block index's dirty records.
struct V {
    coins: CoinsCache<RocksEngine>,
    tip: BlockRef,
    seq: u64,
    dirty: Vec<IndexWrite>,
    scripted: bool,
    now: Instant,
}

impl V {
    fn with_cx<T>(&mut self, f: impl FnOnce(&mut FlushCx<'_, RocksEngine, ()>) -> T) -> T {
        f(&mut FlushCx {
            now: self.now,
            coins: &mut self.coins,
            mempool_usage: 0,
            tip: self.tip,
            last_seq: self.seq,
            index: &mut self.dirty,
            evo: &mut NoEvo,
            prune: &mut NoPrune,
            manual_prune_height: None,
            reindex: false,
            is_ibd: false,
            left_ibd: false,
            scripted: self.scripted,
        })
    }

    /// The connector's coins for `blk` at `b`, as Core's `ConnectBlock`: each transaction's
    /// inputs checked and spent, then its outputs added; and `b`'s dirty index record.
    fn connect(&mut self, b: BlockRef, blk: &Block) {
        let mut o = BlockOverlay::new(&mut self.coins);
        for tx in blk.txs() {
            let is_coinbase = tx.is_coinbase();
            if !is_coinbase {
                assert!(o.have_inputs(tx).unwrap(), "the inputs are unspent");
                for input in tx.inputs() {
                    o.spend(&OutPointKey::new(&input.prevout)).unwrap();
                }
            }
            let hash = tx.txid();
            for (index, out) in (0..).zip(tx.outputs()) {
                let src = CoinSrc::Output {
                    out,
                    height: b.height,
                    coinbase: is_coinbase,
                };
                let prevout = OutPoint {
                    hash,
                    index,
                };
                o.add(&OutPointKey::new(&prevout), src, is_coinbase).unwrap();
            }
        }
        o.commit(b).unwrap();
        self.dirty.push(IndexWrite {
            hash: b.hash,
            bytes: record(u64::from(b.height)),
        });
    }

    /// The tip step to `tip`.
    fn step(&mut self, tip: BlockRef) -> u64 {
        self.tip = tip;
        self.seq += 1;
        self.seq
    }

    /// Core's IF_NEEDED site around the tip step to `tip`, then V's poll at the block boundary
    /// (W's events, and the freeze of a cause kept for a released job), and the rebuild at a
    /// boundary that does not freeze (CH2 §2.3).
    fn site_and_step<D: FlushDriver<Delta = ()>>(
        &mut self,
        ctl: &mut FlushControl<D>,
        tip: BlockRef,
    ) {
        let pending = self.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
        let froze = matches!(pending.action(), FlushAction::Freeze { .. });
        let seq = self.step(tip);
        self.with_cx(|cx| pending.freeze_after_step(ctl, cx, tip, seq)).unwrap();
        self.with_cx(|cx| ctl.poll(cx)).unwrap();
        if !froze {
            self.coins.boundary_rebuild();
        }
    }

    /// Connects `blocks`, each at its boundary.
    fn connect_all<D: FlushDriver<Delta = ()>>(
        &mut self,
        ctl: &mut FlushControl<D>,
        blocks: &[(BlockRef, Block)],
    ) {
        for (b, blk) in blocks {
            self.connect(*b, blk);
            self.site_and_step(ctl, *b);
        }
    }

    /// An ALWAYS flush, as at shutdown, and W's step after its release: the post-Bk memtable
    /// flush, which waits for every earlier one.
    fn flush_always(&mut self, ctl: &mut Ctl<'_>) {
        self.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
        let rest: Vec<FlushStep> = std::iter::from_fn(|| ctl.driver().step()).collect();
        self.with_cx(|cx| ctl.poll(cx)).unwrap();
        assert_eq!(rest, [FlushStep::AfterBk]);
    }

    /// The ALWAYS flush on W's thread: V waits for its release, then stops, so W's last step has
    /// run when the control drops.
    fn flush_always_thread(&mut self, ctl: &mut FlushControl<ThreadDriver<()>>) {
        self.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
        self.with_cx(|cx| ctl.stop(cx)).unwrap();
    }
}

/// A synthetic block ref, outside any chain's index.
fn synthetic(height: u32) -> BlockRef {
    let mut hash = [0x5c; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

/// A P2PKH coin's output.
fn p2pkh(i: u64) -> TxOut {
    TxOut {
        value: 1_000_000 + i as i64,
        script_pubkey: [&[0x76, 0xa9, 0x14][..], &[0x42; 20], &[0x88, 0xac]].concat().into(),
    }
}

/// Outpoint `i`: 4 outputs a transaction.
fn key(i: u64) -> OutPointKey {
    OutPointKey::new(&OutPoint {
        hash: TxHash::from(Rng(i / 4).bytes::<32>()),
        index: (i % 4) as u32,
    })
}

/// Adds P2PKH coins `from..to` at height 1.
fn add_coins(coins: &mut CoinsCache<RocksEngine>, from: u64, to: u64) {
    for i in from..to {
        let out = p2pkh(i);
        let src = CoinSrc::Output {
            out: &out,
            height: 1,
            coinbase: false,
        };
        coins.add(&key(i), src, false).unwrap();
    }
}

/// The coins of an F a little under 64 MiB, so that its last 16 MiB goes into Bk: 63 MiB over the
/// mean record size of a sample of them.
fn split_coins(sizing: Sizing) -> u64 {
    const SAMPLE: u64 = 4096;
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) = RocksEngine::open(dir.path(), &CHAINSTATE_SCHEMA, &sizing.config()).unwrap();
    let mut batch = engine.batch();
    let empty = batch.size_bytes();
    for i in 1..=SAMPLE {
        let coin = PackedCoin::from_output(&p2pkh(i), 1, false).unwrap();
        CoinsTable::default().put(&mut batch, &key(i), coin.as_bytes());
    }
    63 * MIB * SAMPLE / (batch.size_bytes() - empty) as u64
}

/// A datadir whose chainstate holds `chain`'s live set at `old`, compacted to its last level, so
/// that every copy reads the same tables.
fn template_datadir(chain: &Chain, sizing: Sizing) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _) =
        RocksEngine::open(&dir.path().join("chainstate"), &CHAINSTATE_SCHEMA, &sizing.unflushed())
            .unwrap();
    let table = CoinsTable::default();
    for chunk in chain.live.chunks(1 << 16) {
        let mut b = engine.batch();
        for (k, coin) in chunk {
            table.put(&mut b, k, coin.as_bytes());
        }
        engine.write(b, Commit::Buffered).unwrap();
    }
    let mut b = engine.batch();
    Meta::put_tip(&mut b, &chain.old.hash);
    Meta::put_evo_tip(&mut b, &chain.old.hash);
    engine.write(b, Commit::Durable).unwrap();
    engine.flush_memtables().unwrap();
    let raw = engine.raw();
    for spec in CHAINSTATE_SCHEMA.cfs {
        let cf = raw.cf_handle(spec.name).expect("every column family");
        raw.compact_range_cf(&cf, None::<&[u8]>, None::<&[u8]>);
    }
    dir
}

/// A crash image of `chain`'s first `n` blocks, whose positions it records in `chain.index`: they
/// are in the block files, V connected them over `template`'s chainstate, and the flush at the
/// last one, with `extra` more index records, stopped after B1. Returns the datadir; its closed
/// `chainstate/` is the image.
fn crash_image(
    chain: &mut Chain,
    template: &Path,
    n: usize,
    extra: u64,
    sizing: Sizing,
) -> TempDir {
    let node = Node::open(datadir_from(&template.join("chainstate")), sizing, &sizing.unflushed());
    let chainstate = node.dir.path().join("chainstate");
    let copied = tables(&chainstate);
    // As in IBD, each block is in the block files before it is connected.
    for (b, blk) in &chain.blocks[..n] {
        let pos = node
            .files
            .mutate(|f| f.save_block(&core_bytes(blk), b.height, TIME.into(), None))
            .unwrap();
        chain.index.data[b.id.0 as usize] = Some(IndexPos {
            file: pos.file as i32,
            pos: pos.pos,
        });
    }
    let mut ctl = node.control();
    let mut v = node.v(chain.old, Some(chain.old));
    let ((last, blk), rest) = chain.blocks[..n].split_last().expect("a block");
    v.connect_all(&mut ctl, rest);
    v.connect(*last, blk);
    v.dirty.extend((0..extra).map(|i| IndexWrite {
        hash: BlockHash::from(Rng(i).bytes::<32>()),
        bytes: record(i),
    }));
    v.scripted = true;
    v.site_and_step(&mut ctl, *last);
    while ctl.driver().step().expect("W runs to B1") != FlushStep::B1 {}
    drop((ctl, v));
    let dir = node.close();
    // Neither W nor the close flushed a memtable: what the image adds to the template is only in
    // the log.
    assert_eq!(tables(&chainstate), copied, "a memtable was flushed");
    dir
}

fn replay(c: &mut Criterion) {
    let sizing = p300();
    for (name, churn) in [("mainnet-453371", MAINNET), ("testnet-435990", TESTNET)] {
        let mut fx = None;
        c.bench_function(&format!("replay/synth-{name}-p300"), |b| {
            let (chain, template) = fx.get_or_insert_with(|| {
                let chain = Chain::new(churn);
                let template = template_datadir(&chain, sizing);
                (chain, template)
            });
            b.iter_batched(
                || {
                    let node = Node::copy_of(template.path(), sizing);
                    let v = node.v(chain.old, Some(chain.old));
                    node.idle();
                    (node, v)
                },
                |(node, mut v)| {
                    let mut ctl = node.control();
                    v.connect_all(&mut ctl, &chain.blocks);
                    v.flush_always(&mut ctl);
                    node.idle();
                    assert_eq!(v.coins.durable(), Some(chain.new_tip()));
                    // The control borrows the node (its inline W's pump has drop glue).
                    drop(ctl);
                    (v, node)
                },
                BatchSize::PerIteration,
            );
        });
    }
}

/// `replay/synth-*-thread-p300` (CH2c D13): the replay with W on its own thread. W parks and wakes
/// as the scheduler runs it, so the rows' syscalls and page faults are reported, not gated.
fn replay_thread(c: &mut Criterion) {
    let sizing = p300();
    for (name, churn) in [("mainnet-453371", MAINNET), ("testnet-435990", TESTNET)] {
        let mut fx = None;
        c.bench_function(&format!("replay/synth-{name}-thread-p300"), |b| {
            let (chain, template) = fx.get_or_insert_with(|| {
                let chain = Chain::new(churn);
                let template = template_datadir(&chain, sizing);
                (chain, template)
            });
            b.iter_batched(
                || {
                    let node = Node::copy_of(template.path(), sizing);
                    let v = node.v(chain.old, Some(chain.old));
                    node.idle();
                    (node, v)
                },
                |(node, mut v)| {
                    let mut ctl = node.thread_control();
                    v.connect_all(&mut ctl, &chain.blocks);
                    v.flush_always_thread(&mut ctl);
                    drop(ctl);
                    node.idle();
                    assert_eq!(v.coins.durable(), Some(chain.new_tip()));
                    (v, node)
                },
                BatchSize::PerIteration,
            );
        });
    }
}

fn flush(c: &mut Criterion) {
    use FlushStep::{AfterBk, B1, Bk, Durable, Files, PreBkBarrier, Release, Space, SubBatch};
    let sizing = p300();
    assert_eq!(sizing.sub_batch, 16 * MIB);
    let mut fx = None;
    c.bench_function("flush/split-64m", |b| {
        let n = *fx.get_or_insert_with(|| split_coins(sizing));
        b.iter_batched(
            || {
                let node = Node::empty(sizing);
                let mut v = node.v(synthetic(0), None);
                add_coins(&mut v.coins, 0, n);
                v.coins.set_best_block(Some(synthetic(1)));
                v.scripted = true;
                node.idle();
                (node, v)
            },
            |(node, mut v)| {
                let mut ctl = node.control();
                v.site_and_step(&mut ctl, synthetic(1));
                let steps: Vec<FlushStep> = std::iter::from_fn(|| {
                    let step = ctl.driver().step();
                    node.idle();
                    step
                })
                .collect();
                v.with_cx(|cx| ctl.poll(cx)).unwrap();
                // The last SubBatch step finds less than S left for Bk and writes nothing.
                assert_eq!(
                    steps,
                    [
                        Space,
                        Files,
                        B1,
                        Durable,
                        SubBatch(0),
                        SubBatch(1),
                        SubBatch(2),
                        SubBatch(3),
                        PreBkBarrier,
                        Bk,
                        Release,
                        AfterBk
                    ]
                );
                assert_eq!(v.coins.durable(), Some(synthetic(1)));
                drop(ctl);
                (v, node)
            },
            BatchSize::PerIteration,
        );
    });
}

fn shutdown(c: &mut Criterion) {
    let sizing = p300();
    c.bench_function("shutdown/always-p300", |b| {
        b.iter_batched(
            || {
                let node = Node::open(tempfile::tempdir().unwrap(), sizing, &sizing.unflushed());
                let mut v = node.v_with(synthetic(0), None, 0);
                let mut n = 0;
                while v.coins.size_state(0) == CoinsCacheSizeState::Ok {
                    add_coins(&mut v.coins, n, n + 4096);
                    n += 4096;
                }
                v.coins.set_best_block(Some(synthetic(1)));
                v.step(synthetic(1));
                node.idle();
                let chainstate = node.dir.path().join("chainstate");
                (node, v, chainstate)
            },
            |(node, mut v, chainstate)| {
                let mut ctl = node.control();
                v.flush_always(&mut ctl);
                node.idle();
                assert_eq!(v.coins.durable(), Some(synthetic(1)));
                // The post-Bk flush wrote the only tables: the coins' memtable and meta's. No
                // memtable was switched under W's writes, so none of them could stall. Dropped
                // first, before the node closes.
                drop(ctl);
                (TablesAfter(chainstate, 2), v, node)
            },
            BatchSize::PerIteration,
        );
    });
}

fn recovery(c: &mut Criterion) {
    let sizing = p300_batch(MIB);
    let mut fx = None;
    c.bench_function("recovery/mainnet-2000", |b| {
        let (chain, image) = fx.get_or_insert_with(|| {
            let mut chain = Chain::new(MAINNET);
            let template = template_datadir(&chain, sizing);
            let n = chain.blocks.len();
            let image = crash_image(&mut chain, template.path(), n, 0, sizing);
            (chain, image)
        });
        b.iter_batched(
            || (datadir_from(&image.path().join("chainstate")), Arc::new(OsFs::new())),
            |(dir, fs)| {
                // The replay's sub-batches fill several memtables.
                let db = guarded(sizing.open_engine(&dir.path().join("chainstate"), &quiet()));
                let blocks = BlockFilesConfig::new(image.path().join("blocks"), MAGIC);
                let opened = ChainstateOpen::open(&db, fs, blocks, &NullObserver).unwrap();
                let hub = SubscriberHub::new();
                let cfg = RecoveryConfig {
                    pow: pow(),
                    sub_batch: sizing.sub_batch,
                    crash: CrashRoll::seeded(0, 0),
                };
                let r = opened.recover(&chain.index, &hub, cfg).unwrap();
                // The row forces the replay's memtables out, a call production does not make
                // here, so that the region holds the cost of writing them.
                db.flush_memtables().unwrap();
                RocksTuning::wait_idle(&db).unwrap();
                assert_eq!((r.tip, r.replayed.len()), (Some(chain.new_tip()), chain.blocks.len()));
                (r, db, dir)
            },
            BatchSize::PerIteration,
        );
    });
}

fn chainstate(c: &mut Criterion) {
    let sizing = p300();
    let mut fx = None;
    c.bench_function("chainstate/open-b1-256k", |b| {
        let image = fx.get_or_insert_with(|| {
            let mut chain = Chain::new(MAINNET);
            let template = template_datadir(&chain, sizing);
            // F holds one block's coins: the index records alone (about 41 MB) split it, through
            // `put_dirty`'s bound, and so put the image under the marker this row opens.
            crash_image(&mut chain, template.path(), 1, 1 << 18, sizing)
        });
        b.iter_batched(
            || (datadir_from(&image.path().join("chainstate")), Arc::new(OsFs::new())),
            |(dir, fs)| {
                let tuning = RocksTuning::default();
                let db = guarded(sizing.open_engine(&dir.path().join("chainstate"), &tuning));
                let blocks = BlockFilesConfig::new(image.path().join("blocks"), MAGIC);
                let opened = ChainstateOpen::open(&db, fs, blocks, &NullObserver).unwrap();
                assert!(opened.meta().head_blocks.is_some(), "the image is under a marker");
                drop(opened);
                RocksTuning::wait_idle(&db).unwrap();
                (db, dir)
            },
            BatchSize::PerIteration,
        );
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = replay, replay_thread, flush, shutdown, recovery, chainstate
}
criterion_main!(benches);
