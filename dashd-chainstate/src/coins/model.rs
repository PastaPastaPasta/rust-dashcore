//! The coins model (CH2 §6.5): the op drivers behind the fuzz targets `coins_cache_model` and
//! `coins_overlay_diff`, which run [`CoinsCache`] and [`BlockOverlay`] beside the
//! [`RefCoinsCache`] twin, op by op.
//!
//! - **`coins_cache_model`** (v0, no crash; CH2b adds crash kinds): tip ops, blocks through an
//!   overlay (committed, or dropped as a failed connect), disconnects from undo, freezes, W's two
//!   write steps, releases, prefetch requests V stamps as it dispatches them, a producer's read of
//!   one (its snapshot taken then, whatever ran since the stamp; CH4-1), batches admitted with the
//!   current and with a stale generation, mempool and `-dbcache` changes, block-boundary checks at
//!   Core's call sites, and an injected read failure.
//! - **`coins_overlay_diff`**: a random parent (active entries with random flags, F, the database),
//!   then one overlay's ops and its commit.
//!
//! **The twin.** A tip twin over a [`MemoryCoins`] database twin, and a child twin for an open
//! overlay. At the cache's freeze the tip twin flushes, Core's one-layer flush; from then on the
//! cache's active → F → prefetch → database reads equal the twins' for every key. Checks:
//! - every lookup equals the twin's, and the whole view does at each boundary;
//! - the tip's entry after every `have_inputs` and fetch (DEC-142 O1): where both hold one, the
//!   same DIRTY, FRESH and spent flags; where only the twin does, a clean coin with one of two
//!   causes ([`TwinOnly`]): unspent in the last frozen layer, which the cache reads in place, or
//!   read by an overlay's `coin` or `have_coin`, which Core's child caches and ours does not. The
//!   cache never holds an entry the twin lacks;
//! - the best block (O8): the tip's against the tip twin's after every block, disconnect, freeze
//!   and release, as a whole [`BlockRef`]; F's tip against the database twin's at each freeze, and
//!   the cache's durable block against it at each release;
//! - `logic_error` parity: an add with `possible_overwrite` false fails exactly when the coin is
//!   unspent in the cache, Core's in its own map, the cache's in active then F (an overlay's: its
//!   child, else the tip; §3.2). Where the two memberships agree, so do the errors. They differ
//!   only where the cache's membership does (D12, DEC-102), and the model checks that the
//!   difference is one of those: a coin unspent in F only, which the twin has flushed; a clean
//!   twin entry fetched from F, which the cache never copied (and lost at the release); in an
//!   overlay, a coin only the tip holds (Core's child checks its own entry, then raises `FRESH
//!   flag misapplied` at the commit or overwrites a coin it never fetched), or a clean entry
//!   Core's child fetched and the cache's child does not copy (O-R6-m3). A commit fails exactly
//!   when a FRESH child entry meets a coin unspent in the tip, by the same memberships. A
//!   `logic_error` on either side ends the run, as it ends Core;
//! - `have_in_cache` is active then F, unspent, with no database or prefetch read (DEC-102);
//! - the database equals the database twin at every release;
//! - native Σ (§6.7): with the tagging allocator installed, the allocator's coins bytes equal
//!   `usage().allocated()` (Σ − R less the unbacked ceilings, CH2c D29) after every op, plus the long coins of prefetch batches read but not
//!   yet admitted (the producer's, in flight to V; the read path tags them); `size_state` and
//!   `boundary_state` are called only at block boundaries (no overlay open, `held_out` 0) and
//!   decide from `core_size_arith` on native Σ;
//! - fail-stop: after an injected read failure, the failing op returns `CoinsReadError` with the
//!   latch tripped and the abort sink told; the run ends there. A read failure with none injected
//!   is a finding (xcheck's corruption abort, for instance).
//!
//! **The engine.** [`run_on`] opens the database through an `EngineTuning`, takes R from it and
//! calls its `after_bk` after each release of a written F (§2.5.1, step 11). The fuzz targets run
//! [`ModelTuning`] over `MemoryTuning`; the tests also run `MemoryTuning`, `RocksTuning`, and
//! [`ModelTuning`] over RocksDB, which must run each input as the targets do (§7.1 a3).
//!
//! R0, R2–R4, R7, R8 and R13 of the target's oracle are ST3a's checks after a reopen: CH2b's.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::coins::{Coin, CoinsView as _, MemoryCoins};
use dash_consensus::primitives::{BlockHash, Transaction, Tx, TxIn, TxOut, TxType};
use dash_types::Numeric as _;
use dashd_store::{Commit, ErrorKind, KvEngine, MemoryEngine, SchemaState, StoreError};

#[cfg(feature = "planted-bugs")]
use super::Planted;
use super::key::DB_KEY_MAX;
use super::layer::CoinLayerOps;
use super::reference::{Over, RefCoinsCache, RefLogicError};
use super::{
    BlockOverlay, BlockRef, CoinSrc, CoinsBoundary, CoinsBudget, CoinsCache, CoinsCacheSizeState,
    CoinsTable, Flags, FrozenLayer, OutPointKey, PackedCoin, PrefetchBatch, PrefetchRequest, Ratio,
    Resized, Salt, Usage, core_size_arith, read_batch,
};
use crate::CoinsError;
use crate::caches::{CacheSizes, IndexArgs, MIN_DB_CACHE};
use crate::planted::planted;
use crate::store::{
    AbortMark, CoinsReadError, GlobalFailStop, GuardedEngine, ProbeEngine, RecordingSink,
};
use crate::tuning::{
    DEFAULT_DB_BATCH_SIZE, EngineMemoryReport, EngineTuning, MemoryTuning, sub_batch,
};

type Db<E = MemoryEngine> = GuardedEngine<ProbeEngine<E>>;

const MIB: u64 = 1 << 20;

/// A run's parameters.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub coins_share: u64,
    pub max_mempool: u64,
    pub f: Ratio,
    /// Whether the cache cross-checks each prefetched coin against the database.
    pub xcheck: bool,
    /// Whether the run checks the tagging allocator after every op, when it counts. The planted-bug
    /// tests turn it off to see the check that catches X13 behind it (O2).
    pub meter: bool,
    /// A planted bug (feature `planted-bugs`).
    #[cfg(feature = "planted-bugs")]
    pub planted: Option<Planted>,
}

impl Config {
    /// The configuration an input's first byte picks: f ∈ {1/4, 1/2, 3/4}, a coins share of 2, 4 or
    /// 8 MiB, a mempool allowance of 0 or 1 MiB, and xcheck off with bit 7 set.
    pub fn from_byte(b: u8) -> Self {
        Self {
            coins_share: [2, 4, 8][usize::from(b >> 2) % 3] * MIB,
            max_mempool: if b & 0x40 == 0 {
                0
            } else {
                MIB
            },
            f: [Ratio::new(1, 4), Ratio::HALF, Ratio::new(3, 4)][usize::from(b & 3) % 3],
            xcheck: b & 0x80 == 0,
            meter: true,
            #[cfg(feature = "planted-bugs")]
            planted: None,
        }
    }
}

/// What a run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub ops: u64,
    pub blocks: u64,
    pub dropped_blocks: u64,
    pub disconnects: u64,
    pub freezes: u64,
    pub releases: u64,
    pub admitted: u64,
    pub stale_batches: u64,
    pub boundaries: u64,
    /// Resizes that asked for an ALWAYS flush: a shrunk share or a database-only change.
    pub always_flushes: u64,
    /// Ended by a `logic_error` both sides raised, or one only the cache raises (§3.2).
    pub logic_errors: u64,
    /// Ended by the injected read failure.
    pub read_failure: bool,
    /// The allocator counted the coins bytes (the tagging allocator was installed).
    pub allocator_checked: bool,
}

/// The first check that failed, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch(pub &'static str);

/// How a run ends early: a finding, or a fatal error that ends Core too.
enum Stop {
    Mismatch(&'static str),
    Fatal,
}

impl From<Mismatch> for Stop {
    fn from(m: Mismatch) -> Self {
        Self::Mismatch(m.0)
    }
}

type Step<T = ()> = Result<T, Stop>;

fn ensure(ok: bool, what: &'static str) -> Step {
    if ok {
        Ok(())
    } else {
        Err(Stop::Mismatch(what))
    }
}

/// `coins_cache_model`: runs `data`'s ops; the first byte picks the [`Config`].
///
/// # Errors
///
/// The first check that failed.
pub fn coins_cache_model(data: &[u8]) -> Result<Stats, Mismatch> {
    let Some((&b, ops)) = data.split_first() else {
        return Ok(Stats::default());
    };
    run(Config::from_byte(b), ops, false)
}

/// `coins_overlay_diff`: a random parent state from the first bytes, then one overlay.
///
/// # Errors
///
/// The first check that failed.
pub fn coins_overlay_diff(data: &[u8]) -> Result<Stats, Mismatch> {
    let Some((&b, ops)) = data.split_first() else {
        return Ok(Stats::default());
    };
    run(Config::from_byte(b), ops, true)
}

/// Runs `ops` under `config`: [`coins_cache_model`]'s op stream, or with `diff`
/// [`coins_overlay_diff`]'s parent and overlay.
///
/// # Errors
///
/// The first check that failed.
pub fn run(config: Config, ops: &[u8], diff: bool) -> Result<Stats, Mismatch> {
    run_on(ModelTuning(MemoryTuning), Path::new(""), config, ops, diff)
}

/// [`run`] over `tuning`'s engine, opened in `dir`, an empty directory (unused by an engine in
/// memory), with the tuning's R.
///
/// # Errors
///
/// The first check that failed.
///
/// # Panics
///
/// If the engine does not open, or `dir` holds a database.
pub fn run_on<T: EngineTuning>(
    tuning: T,
    dir: &Path,
    config: Config,
    ops: &[u8],
    diff: bool,
) -> Result<Stats, Mismatch> {
    let meter = if config.meter {
        Meter::start()
    } else {
        None
    };
    let mut m = Model::new(config, tuning, dir);
    m.stats.allocator_checked = meter.is_some();
    let mut input = Input(ops);
    let result = if diff {
        m.overlay_diff(&mut input, meter.as_ref())
    } else {
        m.ops(&mut input, meter.as_ref())
    };
    match result {
        Ok(()) | Err(Stop::Fatal) => Ok(m.stats),
        Err(Stop::Mismatch(what)) => Err(Mismatch(what)),
    }
}

/// The fuzz targets' tuning: `T`'s engine and hooks with a reserve of its own, S + S/4, not an
/// engine's, so that every input charges R while F is in flight (`MemoryTuning`'s R is 0) and two
/// engines run an input with one budget.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelTuning<T = MemoryTuning>(pub T);

impl<T: EngineTuning> EngineTuning for ModelTuning<T> {
    type E = T::E;

    fn open_chainstate(
        &self,
        path: &Path,
        sizes: &CacheSizes,
        sub_batch: u64,
    ) -> Result<(T::E, SchemaState), StoreError> {
        self.0.open_chainstate(path, sizes, sub_batch)
    }

    fn batch_reserve(&self, sub_batch: u64) -> u64 {
        sub_batch + sub_batch / 4
    }

    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError> {
        self.0.after_bk(db)
    }

    fn journal_budget(&self) -> u64 {
        self.0.journal_budget()
    }

    fn unbounded_delete_span(&self) -> u32 {
        self.0.unbounded_delete_span()
    }

    fn memory_report(&self, db: &GuardedEngine<T::E>) -> EngineMemoryReport {
        self.0.memory_report(db)
    }
}

/// The input bytes, read as ops and their arguments.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    /// A byte, or 0 once the input has ended (the op's argument).
    fn arg(&mut self) -> u8 {
        self.byte().unwrap_or(0)
    }
}

/// One of 256 outpoints: 32 transactions of 8 outputs, so ops meet.
fn key(b: u8) -> OutPointKey {
    let mut k = [b % 32 + 1; 36];
    k[32..].copy_from_slice(&u32::from(b / 32).to_le_bytes());
    OutPointKey(k)
}

/// An add's key: `start` one time in four, else the first outpoint from `start` on that `unspent`
/// says is free, as a connect's outputs are, so most runs are not cut short by a `logic_error`.
fn add_key(start: u8, unspent: impl Fn(&OutPointKey) -> bool) -> u8 {
    if start & 3 == 0 {
        return start;
    }
    (0..=u8::MAX).map(|i| start.wrapping_add(i)).find(|&b| !unspent(&key(b))).unwrap_or(start)
}

/// Whether the twins hold `k` unspent: the child, then the tip, then the database.
fn twin_unspent(
    child: Option<&RefCoinsCache>,
    tip: &RefCoinsCache,
    db: &MemoryCoins,
    k: &OutPointKey,
) -> bool {
    match child.and_then(|c| c.entry(k)).or_else(|| tip.entry(k)) {
        Some(e) => e.coin.is_some(),
        None => db.coin(&k.outpoint()).unwrap_or_else(|e| match e {}).is_some(),
    }
}

/// Whether the cache's view holds `k` unspent: active, then F, then the database, read past the
/// guard (no read counted, no injected failure, no latch).
fn view_unspent<E: KvEngine>(
    cache: &CoinsCache<ProbeEngine<E>>,
    db: &Db<E>,
    k: &OutPointKey,
) -> bool {
    match cache.cached(k) {
        Some(e) => !e.flags.is_spent(),
        None => {
            let mut buf = [0; DB_KEY_MAX];
            let n = k.db_key(&mut buf);
            let cf = CoinsTable::default().cf();
            db.inner().unprobed().get(cf, &buf[..n]).expect("an unprobed read").is_some()
        }
    }
}

/// A coin of one of four shapes: P2PKH (inline), a long script, an unspendable output (never
/// added) and a coinbase P2PKH.
fn coin(b: u8, height: u32) -> Coin {
    let p2pkh = |fill: u8| {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&[fill; 20]);
        s.extend_from_slice(&[0x88, 0xac]);
        s
    };
    let value = 1 + i64::from(b) * 1_000;
    match b % 4 {
        0 => Coin::new(value, &p2pkh(b), height, false),
        1 => Coin::new(value, &vec![0x51; 40 + usize::from(b)], height, false),
        2 => Coin::new(value, &[0x6a, b], height, false),
        _ => Coin::new(value, &p2pkh(b), height, true),
    }
}

/// A block at `height` (its id and hash derived from the height; never the null hash).
fn block(height: u32) -> BlockRef {
    let mut hash = [0xb1; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

/// The block whose hash is `hash`, if [`block`] made it: its height is in the first four bytes.
fn block_of(hash: BlockHash) -> Option<BlockRef> {
    let bytes = hash.to_lendian();
    let b = block(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    (b.hash == hash).then_some(b)
}

/// A transaction spending `inputs`, with one output.
pub(crate) fn spending(inputs: &[OutPointKey]) -> Tx {
    Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: inputs
            .iter()
            .map(|k| TxIn {
                prevout: k.outpoint(),
                script_sig: Default::default(),
                sequence: u32::MAX,
            })
            .collect(),
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: Vec::new().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    })
}

/// Writes `dirty` (a layer's DIRTY entries, or a run of them) to the database in one batch, a spent
/// entry as an erase: W's write without the commit protocol a3 adds.
pub(crate) fn write_dirty<'a, E: KvEngine>(
    db: &Db<E>,
    dirty: impl IntoIterator<Item = (OutPointKey, Flags, &'a [u8])>,
) -> Result<(), StoreError> {
    let table = CoinsTable::default();
    let mut b = db.batch();
    for (k, flags, coin) in dirty {
        if flags.is_spent() {
            table.erase(&mut b, &k);
        } else {
            table.put(&mut b, &k, coin);
        }
    }
    db.write(b, Commit::Buffered)
}

/// The `coins` column family in the engine's order: each key and its stored bytes.
pub(crate) fn db_coins<E: KvEngine>(db: &Db<E>) -> Vec<(OutPointKey, Vec<u8>)> {
    let cf = CoinsTable::default().cf();
    db.inner()
        .unprobed()
        .iter_prefix(cf, &[])
        .map(|r| {
            let (k, v) = r.expect("an unprobed read");
            (OutPointKey::from_db_key(&k).expect("a coin key"), v)
        })
        .collect()
}

/// The boundary CH2 §6.7 expects of `cache` at a block boundary: CRITICAL, THRESHOLD (f·B or a
/// shard trigger, with no F in flight), LARGE, in that order, from `core_size_arith` on native Σ
/// with R charged while F is in flight (said so, or held).
pub(crate) fn expected_boundary<E: KvEngine>(
    cache: &CoinsCache<ProbeEngine<E>>,
    mempool: u64,
    in_flight: bool,
) -> CoinsBoundary {
    let in_flight = in_flight || cache.frozen().is_some();
    let budget = cache.budget();
    let mut usage = cache.usage();
    usage.reserve = budget.reserve(in_flight);
    let threshold = !in_flight
        && (cache.active().charged_bytes() >= budget.threshold(mempool)
            || cache.active().at_shard_trigger());
    match core_size_arith(usage.total(), budget.total(mempool)) {
        CoinsCacheSizeState::Critical => CoinsBoundary::Critical,
        _ if threshold => CoinsBoundary::Threshold,
        CoinsCacheSizeState::Large => CoinsBoundary::Large,
        CoinsCacheSizeState::Ok => CoinsBoundary::Ok,
    }
}

/// The allocator check of §6.7, when the tagging allocator is installed: this run's coins bytes
/// in its thread's domain, against native Σ − R.
struct Meter {
    #[cfg(feature = "alloc-tag")]
    domain: u8,
    #[cfg(feature = "alloc-tag")]
    base: i64,
}

impl Meter {
    /// A meter, if the allocator counts: a probe allocation in the thread's domain must show up.
    /// A thread takes a domain at its first run and keeps it: runs on one thread are sequential,
    /// and the meter counts from `base`.
    #[cfg(feature = "alloc-tag")]
    fn start() -> Option<Self> {
        use crate::alloc_tag::{
            Tag, coins, new_domain, set_thread_domain, tagged_bytes, thread_domain,
        };
        let domain = match thread_domain() {
            0 => {
                let d = new_domain();
                set_thread_domain(d);
                d
            }
            d => d,
        };
        let base = tagged_bytes(domain, Tag::Coins);
        let probe = {
            let _tag = coins();
            std::hint::black_box(Box::new([0u8; 64]))
        };
        let counts = tagged_bytes(domain, Tag::Coins) == base + 64;
        drop(probe);
        counts.then_some(Self {
            domain,
            base,
        })
    }

    #[cfg(not(feature = "alloc-tag"))]
    fn start() -> Option<Self> {
        None
    }

    /// The allocator against native Σ − R and the batches in flight.
    fn check(&self, u: Usage, pending: &VecDeque<PrefetchBatch>) -> Step {
        #[cfg(feature = "alloc-tag")]
        {
            let tagged = crate::alloc_tag::tagged_bytes(self.domain, crate::alloc_tag::Tag::Coins);
            let in_flight: usize =
                pending.iter().flat_map(|b| &b.entries).map(|(_, c)| c.heap_bytes()).sum();
            let want = u.allocated() + in_flight as u64;
            ensure(tagged - self.base == want as i64, "tagged != sigma - R")
        }
        #[cfg(not(feature = "alloc-tag"))]
        {
            let _ = (u, pending);
            Ok(())
        }
    }
}

/// Whether a coin is unspent in the cache (its `logic_error` condition) and in the twin, and
/// whether a difference has the documented cause in each direction.
struct Members {
    ours: bool,
    twin: bool,
    ours_only: bool,
    twin_only: bool,
}

impl Members {
    /// In the tip: active then F, against the tip twin. The cache alone holds a coin unspent in F
    /// only, which the twin has flushed: its database holds it. The twin alone holds a clean entry
    /// for one of [`TwinOnly`]'s causes, a coin the cache's view holds.
    fn tip<E: KvEngine>(
        cache: &CoinsCache<ProbeEngine<E>>,
        db: &Db<E>,
        twin: &RefCoinsCache,
        twin_db: &MemoryCoins,
        causes: &TwinOnly,
        k: &OutPointKey,
    ) -> Self {
        let none = cache.active().get(k).is_none();
        Self {
            ours: cache.have_in_cache(k),
            twin: twin.have_coin_in_cache(k),
            ours_only: none && twin_unspent(None, twin, twin_db, k),
            twin_only: none
                && twin.entry(k).is_some_and(|e| !e.dirty && !e.fresh)
                && causes.explains(k)
                && view_unspent(cache, db, k),
        }
    }

    fn explained(&self) -> bool {
        self.ours == self.twin || (self.ours && self.ours_only) || (self.twin && self.twin_only)
    }
}

/// An add's result on both sides, against their memberships: each fails exactly when the add
/// checks (`possible_overwrite` false, a spendable output) and its coin is unspent; where the
/// memberships differ, the cause must be documented. A `logic_error` ends the run.
fn add_parity(
    ours: Result<(), CoinsError>,
    twin: Result<(), RefLogicError>,
    checked: bool,
    m: Members,
    stats: &mut Stats,
) -> Step {
    let ours_failed = match ours {
        Ok(()) => false,
        Err(CoinsError::OverwriteUnspent) => true,
        Err(_) => return Err(Stop::Mismatch("add error")),
    };
    let twin_failed = twin.is_err();
    ensure(ours_failed == (checked && m.ours), "add: the cache's logic_error")?;
    ensure(twin_failed == (checked && m.twin), "add: the twin's logic_error")?;
    ensure(m.explained(), "add: membership")?;
    if ours_failed || twin_failed {
        stats.logic_errors += 1;
        return Err(Stop::Fatal);
    }
    Ok(())
}

/// The keys at which only a twin may hold a clean entry, for the two documented causes (O1):
/// - `frozen`: a coin the cache read from F in place, never copying it (D12). These are the unspent
///   keys of the last frozen layer: the tip twin's flush at that freeze emptied its map, so each
///   clean entry it holds was fetched since.
/// - `read`: a coin a block overlay's `coin` or `have_coin` read without caching it (§2.4.2), where
///   Core's child caches it, and its parent too. Kept since the last freeze, for the same reason.
#[derive(Default)]
struct TwinOnly {
    frozen: BTreeSet<OutPointKey>,
    read: BTreeSet<OutPointKey>,
}

impl TwinOnly {
    fn explains(&self, k: &OutPointKey) -> bool {
        self.frozen.contains(k) || self.read.contains(k)
    }
}

/// The tip's entry at `k` against the tip twin's (O1): where both have one, the same DIRTY, FRESH
/// and spent; where only the twin has one, a clean coin with a cause in `causes`. The cache alone
/// never has one.
fn tip_entry<E: KvEngine>(
    cache: &CoinsCache<ProbeEngine<E>>,
    twin: &RefCoinsCache,
    causes: &TwinOnly,
    k: &OutPointKey,
) -> Step {
    let same = match (cache.active().get(k), twin.entry(k)) {
        (Some(e), Some(t)) => {
            e.flags.is_dirty() == t.dirty
                && e.flags.contains(Flags::FRESH) == t.fresh
                && e.flags.is_spent() == t.coin.is_none()
        }
        (None, Some(t)) => !t.dirty && !t.fresh && t.coin.is_some() && causes.explains(k),
        (None, None) => true,
        (Some(_), None) => false,
    };
    ensure(same, "the tip's entry")
}

/// `GetBestBlock` on the tip and on the tip twin, which falls back to the database twin's as
/// Core's does (Sol's S1). The tip's is a whole [`BlockRef`], so its id and height are compared
/// too, against the block the twin's hash names (O8).
fn tip_best<E: KvEngine>(
    cache: &CoinsCache<ProbeEngine<E>>,
    twin: &mut RefCoinsCache,
    twin_db: &mut MemoryCoins,
    what: &'static str,
) -> Step {
    let theirs = twin.best_block(twin_db).map(block_of);
    ensure(theirs.is_none_or(|b| b.is_some()) && cache.best_block() == theirs.flatten(), what)
}

/// `GetBestBlock` on a block overlay and on its child twin, which falls back to the tip twin's:
/// both the tip's while the block is open, as `ConnectBlock` asserts (`validation.cpp:2523`).
fn overlay_best<E: KvEngine>(
    overlay: &BlockOverlay<'_, ProbeEngine<E>>,
    child: &mut RefCoinsCache,
    twin: &mut RefCoinsCache,
    twin_db: &mut MemoryCoins,
    what: &'static str,
) -> Step {
    let mut base = Over {
        cache: twin,
        base: twin_db,
    };
    ensure(overlay.best_block() == child.best_block(&mut base), what)
}

/// F in flight: W's handle and how many of its two write steps it has done.
struct W {
    frozen: Arc<FrozenLayer>,
    steps: u8,
    /// V has released F before W finished (X13 planted): W writes on, and drops F at the end.
    released: bool,
}

/// A committed block, for its disconnect: the outputs it added and the coins it spent.
#[derive(Default)]
struct BlockRecord {
    created: Vec<OutPointKey>,
    undo: Vec<(OutPointKey, Coin)>,
}

struct Model<T: EngineTuning> {
    config: Config,
    tuning: T,
    db: Arc<Db<T::E>>,
    sink: Arc<RecordingSink>,
    cache: CoinsCache<ProbeEngine<T::E>>,
    twin: RefCoinsCache,
    twin_db: MemoryCoins,
    /// Why only the tip twin may hold a clean entry: [`TwinOnly`].
    twin_only: TwinOnly,
    w: Option<W>,
    /// Requests V has dispatched that no producer has read yet.
    requests: VecDeque<PrefetchRequest>,
    /// Batches read and not yet admitted.
    pending: VecDeque<PrefetchBatch>,
    blocks: Vec<BlockRecord>,
    /// Undo coins still alive (`held_out`), until the undo is written.
    undo: Vec<PackedCoin>,
    mempool: u64,
    height: u32,
    injected: bool,
    stats: Stats,
}

impl<T: EngineTuning> Model<T> {
    fn new(config: Config, tuning: T, dir: &Path) -> Self {
        let mut twin_db = MemoryCoins::new();
        twin_db.set_best_block(Some(block(1).hash));
        // A `-dbcache=4` node's engine shares, and the run's coins share.
        let sizes = CacheSizes {
            coins: config.coins_share,
            ..CacheSizes::calculate(MIN_DB_CACHE, &IndexArgs::default())
        };
        let s = sub_batch(DEFAULT_DB_BATCH_SIZE, &sizes);
        let (engine, state) = tuning.open_chainstate(dir, &sizes, s).expect("the engine opens");
        assert_eq!(state, SchemaState::Created, "the model starts from an empty database");
        let sink = Arc::new(RecordingSink::default());
        let db = Arc::new(GuardedEngine::new(
            ProbeEngine::over(engine),
            GlobalFailStop::new(),
            sink.clone(),
        ));
        let budget = CoinsBudget::for_engine(&sizes, config.max_mempool, config.f, s, &tuning);
        // The database is at block 1, as a chainstate is once genesis is connected.
        let mut cache = CoinsCache::new(db.clone(), budget, Salt::fixed(9), Some(block(1)));
        cache.set_xcheck(config.xcheck);
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = config.planted {
            cache.plant(bug);
        }
        Self {
            config,
            tuning,
            db,
            sink,
            cache,
            twin: RefCoinsCache::new(),
            twin_db,
            twin_only: TwinOnly::default(),
            w: None,
            requests: VecDeque::new(),
            pending: VecDeque::new(),
            blocks: Vec::new(),
            undo: Vec::new(),
            mempool: 0,
            height: 1,
            injected: false,
            stats: Stats::default(),
        }
    }

    /// A read error: the end of the run if it was injected (fail-stop checked), else a finding.
    fn read_failed(
        db: &Db<T::E>,
        sink: &RecordingSink,
        injected: bool,
        e: &CoinsReadError,
    ) -> Stop {
        if !injected || e.kind() != ErrorKind::Io {
            return Stop::Mismatch("an unexpected read failure");
        }
        let told = sink.marks().iter().any(|m| matches!(m, AbortMark::ReadFailed(_)));
        if db.latch().tripped().is_none() || !told {
            return Stop::Mismatch("a read failure without fail-stop");
        }
        Stop::Fatal
    }

    fn fail(&mut self, e: &CoinsReadError) -> Stop {
        self.stats.read_failure = true;
        Self::read_failed(&self.db, &self.sink, self.injected, e)
    }

    // ---- coins_cache_model ----

    fn ops(&mut self, input: &mut Input<'_>, meter: Option<&Meter>) -> Step {
        while let Some(op) = input.byte() {
            self.stats.ops += 1;
            match op % 20 {
                0 | 1 => self.tip_add(input.arg(), input.arg())?,
                2 => self.tip_spend(input.arg())?,
                3 => self.tip_fetch(input.arg())?,
                4 => self.tip_have(input.arg())?,
                5 => {
                    let k = key(input.arg());
                    self.cache.uncache(&k);
                    self.twin.uncache(&k);
                }
                6..=8 => self.block(input, meter)?,
                9 => self.disconnect(meter)?,
                10 => self.freeze()?,
                11 => self.w_step()?,
                12 => self.release()?,
                // A request read at once (half the time, as the inline producer reads), one V
                // leaves in flight, or the oldest in flight read now.
                13 => {
                    let arg = input.arg() % 4;
                    let ks = [input.arg(), input.arg(), input.arg()];
                    let req = match arg {
                        0 | 1 => Some(Self::request(&self.cache, ks)),
                        2 => {
                            self.requests.push_back(Self::request(&self.cache, ks));
                            None
                        }
                        _ => self.requests.pop_front(),
                    };
                    if let Some(req) = req {
                        Self::read(&self.db, req, &mut self.pending).map_err(|e| self.fail(&e))?;
                    }
                }
                14 => self.admit()?,
                15 => self.boundary()?,
                16 => self.undo.clear(),
                17 => self.mempool = u64::from(input.arg()) * self.config.max_mempool / 255,
                18 => self.resize(input.arg())?,
                _ => {
                    if input.arg() == 0xfa {
                        self.db.inner().fail_reads(Some(ErrorKind::Io));
                        self.injected = true;
                    }
                }
            }
            if let Some(meter) = meter {
                meter.check(self.cache.usage(), &self.pending)?;
            }
        }
        Ok(())
    }

    /// An add on the tip, against the twin's.
    fn tip_add(&mut self, kb: u8, cb: u8) -> Step {
        let kb = add_key(kb, |k| twin_unspent(None, &self.twin, &self.twin_db, k));
        let (k, coin) = (key(kb), coin(cb, self.height));
        let overwrite = cb & 0x80 != 0;
        let (cache, twin) = (&self.cache, &self.twin);
        let members = Members::tip(cache, &self.db, twin, &self.twin_db, &self.twin_only, &k);
        let checked = !overwrite && !coin.as_ref().script_pubkey().is_unspendable();
        let ours = self.cache.add(&k, CoinSrc::Bytes(coin.as_ref()), overwrite);
        let twin = self.twin.add_coin(&k, coin, overwrite);
        add_parity(ours, twin, checked, members, &mut self.stats)
    }

    fn tip_spend(&mut self, kb: u8) -> Step {
        let k = key(kb);
        let ours = self.cache.spend(&k).map_err(|e| self.fail(&e))?;
        let twin = self.twin.spend_coin(&mut self.twin_db, &k).is_some();
        ensure(ours == twin, "spend")
    }

    fn tip_fetch(&mut self, kb: u8) -> Step {
        let k = key(kb);
        let ours = match self.cache.fetch(&k) {
            Ok(c) => c.map(|c| c.to_coin()),
            Err(e) => return Err(self.fail(&e)),
        };
        ensure(ours == self.twin.access_coin(&mut self.twin_db, &k), "fetch")?;
        tip_entry(&self.cache, &self.twin, &self.twin_only, &k)
    }

    /// `have_in_cache` (DEC-102): active, else F, unspent; no read below F.
    fn tip_have(&mut self, kb: u8) -> Step {
        let k = key(kb);
        let reads = (self.db.inner().reads(), self.cache.prefetch_lookups());
        let ours = self.cache.have_in_cache(&k);
        ensure(
            reads == (self.db.inner().reads(), self.cache.prefetch_lookups()),
            "have_in_cache read below F",
        )?;
        let want = match self.cache.active().get(&k) {
            Some(e) => !e.flags.is_spent(),
            None => self
                .cache
                .frozen()
                .and_then(|f| f.coins().get(&k))
                .is_some_and(|e| !e.flags.is_spent()),
        };
        ensure(ours == want, "have_in_cache")?;
        let (cache, twin) = (&self.cache, &self.twin);
        let members = Members::tip(cache, &self.db, twin, &self.twin_db, &self.twin_only, &k);
        ensure(members.explained(), "have_in_cache: membership")?;
        tip_entry(cache, twin, &self.twin_only, &k)
    }

    /// A block through an overlay and a child twin: ops until a commit or a drop (or the input's
    /// end, a failed connect).
    fn block(&mut self, input: &mut Input<'_>, meter: Option<&Meter>) -> Step {
        let height = self.height + 1;
        let Self {
            db,
            sink,
            cache,
            twin,
            twin_db,
            requests,
            pending,
            injected,
            stats,
            undo,
            blocks,
            w,
            twin_only,
            ..
        } = self;
        let fail = |e: &CoinsReadError, stats: &mut Stats| {
            stats.read_failure = true;
            Self::read_failed(db, sink, *injected, e)
        };
        let mut overlay = BlockOverlay::new(cache);
        let mut child = RefCoinsCache::new();
        overlay_best(&overlay, &mut child, twin, twin_db, "the overlay's best block at its open")?;
        let mut record = BlockRecord::default();
        let mut spent = Vec::new();
        loop {
            let Some(op) = input.byte() else {
                drop(overlay);
                stats.dropped_blocks += 1;
                return tip_best(cache, twin, twin_db, "the best block after a dropped block");
            };
            stats.ops += 1;
            let mut base = Over {
                cache: twin,
                base: twin_db,
            };
            match op % 10 {
                0 | 1 => {
                    let (kb, cb) = (input.arg(), input.arg());
                    let kb = add_key(kb, |k| twin_unspent(Some(&child), twin, twin_db, k));
                    let (k, coin) = (key(kb), coin(cb, height));
                    let overwrite = cb & 0x80 != 0;
                    let own = overlay.child().get(&k).map(|e| e.flags);
                    let tip = overlay.tip();
                    let members = Members {
                        ours: own.map_or_else(|| tip.have_in_cache(&k), |f| !f.is_spent()),
                        twin: child.have_coin_in_cache(&k),
                        // Only the tip holds it, and the twins' view does; or Core's child
                        // fetched it (a clean entry) where the overlay read it without caching,
                        // and the cache's view holds it.
                        ours_only: own.is_none() && twin_unspent(Some(&child), twin, twin_db, &k),
                        twin_only: own.is_none()
                            && child.entry(&k).is_some_and(|e| !e.dirty && !e.fresh)
                            && twin_only.read.contains(&k)
                            && view_unspent(tip, db, &k),
                    };
                    let ours = overlay.add(&k, CoinSrc::Bytes(coin.as_ref()), overwrite);
                    let spendable = !coin.as_ref().script_pubkey().is_unspendable();
                    let theirs = child.add_coin(&k, coin, overwrite);
                    add_parity(ours, theirs, !overwrite && spendable, members, stats)?;
                    if spendable {
                        record.created.push(k);
                    }
                }
                2 | 3 => {
                    let k = key(input.arg());
                    let ours = overlay.spend(&k).map_err(|e| match e {
                        CoinsError::Read(e) => fail(&e, stats),
                        _ => Stop::Mismatch("overlay spend error"),
                    })?;
                    let theirs = child.spend_coin(&mut base, &k).flatten();
                    ensure(ours.as_ref().map(|c| c.as_ref().to_coin()) == theirs, "overlay spend")?;
                    if let (Some(c), Some(coin)) = (ours, theirs) {
                        record.undo.push((k, coin));
                        spent.push(c);
                    }
                }
                4 => {
                    let n = usize::from(input.arg() % 4) + 1;
                    let ks: Vec<_> = (0..n).map(|_| key(input.arg())).collect();
                    let ours = overlay.have_inputs(&spending(&ks)).map_err(|e| fail(&e, stats))?;
                    let theirs = ks.iter().all(|k| child.have_coin(&mut base, k));
                    ensure(ours == theirs, "have_inputs")?;
                    // Each input fetched is cached in active, as Core's parent caches it.
                    for k in &ks {
                        tip_entry(overlay.tip(), twin, twin_only, k)?;
                    }
                }
                5 => {
                    let k = key(input.arg());
                    twin_only.read.insert(k);
                    let ours = match overlay.coin(&k.outpoint()) {
                        Ok(c) => c.map(|c| c.to_coin()),
                        Err(e) => return Err(fail(&e, stats)),
                    };
                    ensure(ours == child.access_coin(&mut base, &k), "overlay coin")?;
                    let ours = overlay.have_coin(&k.outpoint()).map_err(|e| fail(&e, stats))?;
                    ensure(ours == child.have_coin(&mut base, &k), "overlay have_coin")?;
                }
                6 => {
                    // Another thread's work, while V connects: a W step, or a prefetch read of the
                    // oldest request in flight (or one V stamps now).
                    if input.arg() & 1 == 0 {
                        Self::w_write(db, w)?;
                    } else {
                        let ks = [input.arg(), input.arg(), input.arg()];
                        let req = requests
                            .pop_front()
                            .unwrap_or_else(|| Self::request(overlay.tip(), ks));
                        Self::read(db, req, pending).map_err(|e| fail(&e, stats))?;
                    }
                }
                7 => {
                    drop(overlay);
                    stats.dropped_blocks += 1;
                    return tip_best(cache, twin, twin_db, "the best block after a dropped block");
                }
                _ => {
                    let best = block(height);
                    child.set_best_block(Some(best.hash));
                    // Each side fails where a FRESH child entry meets a coin its tip holds unspent.
                    let ours_fresh: Vec<_> = overlay
                        .child()
                        .iter_dirty()
                        .filter(|(_, f, _)| f.contains(super::Flags::FRESH))
                        .map(|(k, _, _)| k)
                        .collect();
                    let twin_fresh: Vec<_> = child
                        .entries()
                        .filter(|(_, e)| e.dirty && e.fresh)
                        .map(|(k, _)| *k)
                        .collect();
                    let tip = overlay.tip();
                    let ours_fails = ours_fresh.iter().any(|k| tip.have_in_cache(k));
                    let twin_fails = twin_fresh.iter().any(|k| twin.have_coin_in_cache(k));
                    let explained = ours_fresh
                        .iter()
                        .chain(&twin_fresh)
                        .all(|k| Members::tip(tip, db, twin, twin_db, twin_only, k).explained());
                    let ours = overlay.commit(best);
                    let theirs = twin.batch_write(child);
                    ensure(explained, "commit: membership")?;
                    match (ours, theirs) {
                        (Ok(()), Ok(())) if !ours_fails && !twin_fails => {}
                        (o, t)
                            if matches!(o, Ok(()) | Err(CoinsError::FreshMisapplied))
                                && o.is_err() == ours_fails
                                && t.is_err() == twin_fails =>
                        {
                            stats.logic_errors += 1;
                            return Err(Stop::Fatal);
                        }
                        _ => return Err(Stop::Mismatch("commit")),
                    }
                    tip_best(cache, twin, twin_db, "the best block after a commit")?;
                    stats.blocks += 1;
                    blocks.push(record);
                    undo.extend(spent);
                    self.height = height;
                    return Ok(());
                }
            }
            if let Some(meter) = meter {
                meter.check(overlay.usage(), pending)?;
            }
        }
    }

    /// Disconnects the last block from its record: its outputs spent, its inputs restored as
    /// `ApplyTxInUndo` restores them, in reverse.
    fn disconnect(&mut self, meter: Option<&Meter>) -> Step {
        let Some(record) = self.blocks.pop() else {
            return Ok(());
        };
        let Self {
            db,
            sink,
            cache,
            twin,
            twin_db,
            injected,
            stats,
            pending,
            twin_only,
            ..
        } = self;
        let fail = |e: CoinsError, stats: &mut Stats| match e {
            CoinsError::Read(e) => {
                stats.read_failure = true;
                Self::read_failed(db, sink, *injected, &e)
            }
            _ => Stop::Mismatch("disconnect error"),
        };
        let mut overlay = BlockOverlay::new(cache);
        let mut child = RefCoinsCache::new();
        overlay_best(&overlay, &mut child, twin, twin_db, "the overlay's best block at its open")?;
        for k in record.created.iter().rev() {
            let mut base = Over {
                cache: twin,
                base: twin_db,
            };
            let ours = overlay.spend(k).map_err(|e| fail(e, stats))?;
            let theirs = child.spend_coin(&mut base, k).flatten();
            ensure(ours.map(|c| c.as_ref().to_coin()) == theirs, "disconnect spend")?;
        }
        for (k, coin) in record.undo.into_iter().rev() {
            let mut base = Over {
                cache: twin,
                base: twin_db,
            };
            twin_only.read.insert(k);
            let clean = !overlay.have_coin(&k.outpoint()).map_err(|e| fail(e.into(), stats))?;
            ensure(clean == !child.have_coin(&mut base, &k), "disconnect have_coin")?;
            let ours = overlay.add(&k, CoinSrc::Bytes(coin.as_ref()), !clean);
            ensure(ours.is_ok() && child.add_coin(&k, coin, !clean).is_ok(), "disconnect add")?;
        }
        self.height -= 1;
        let best = block(self.height);
        child.set_best_block(Some(best.hash));
        if let Some(meter) = meter {
            meter.check(overlay.usage(), pending)?;
        }
        match (overlay.commit(best), twin.batch_write(child)) {
            (Ok(()), Ok(())) => {}
            _ => return Err(Stop::Mismatch("disconnect commit")),
        }
        tip_best(cache, twin, twin_db, "the best block after a disconnect")?;
        // Every disconnect clears the prefetch map (CH2 §2.4.3 item 4).
        self.cache.clear_prefetch();
        self.stats.disconnects += 1;
        Ok(())
    }

    /// Freezes active at its best block (one F at a time); the tip twin flushes its own. The first
    /// check's `GetBestBlock` is Core's before its flush (`validation.cpp:3078`): it caches the
    /// database's best block, which `Flush` then writes back.
    fn freeze(&mut self) -> Step {
        tip_best(&self.cache, &mut self.twin, &mut self.twin_db, "the best block before a freeze")?;
        if self.w.is_some() {
            return Ok(());
        }
        let frozen = self.cache.freeze(self.mempool);
        self.twin.flush(&mut self.twin_db);
        let flushed = self.twin_db.best_block().and_then(block_of);
        ensure(Some(frozen.tip()) == flushed, "F's tip against the flushed database")?;
        self.twin_only = TwinOnly {
            frozen: frozen
                .coins()
                .iter()
                .filter(|(_, e)| !e.flags.is_spent())
                .map(|(k, _)| k)
                .collect(),
            read: BTreeSet::new(),
        };
        self.w = Some(W {
            frozen,
            steps: 0,
            released: false,
        });
        self.stats.freezes += 1;
        tip_best(&self.cache, &mut self.twin, &mut self.twin_db, "the best block after a freeze")
    }

    /// One of W's two write steps: each writes half of F's DIRTY entries. A W whose F V has already
    /// released (X13) drops it after its last step.
    fn w_write(db: &Db<T::E>, slot: &mut Option<W>) -> Step {
        let Some(w) = slot.as_mut().filter(|w| w.steps < 2) else {
            return Ok(());
        };
        let layer = w.frozen.coins();
        let half = layer.dirty_len().div_ceil(2);
        let (skip, take) = if w.steps == 0 {
            (0, half)
        } else {
            (half, usize::MAX)
        };
        // A write after the latch tripped is refused: the run is over.
        write_dirty(db, layer.iter_dirty().skip(skip).take(take)).map_err(|_| Stop::Fatal)?;
        w.steps += 1;
        if w.steps == 2 && w.released {
            *slot = None;
        }
        Ok(())
    }

    fn w_step(&mut self) -> Step {
        Self::w_write(&self.db, &mut self.w)
    }

    /// Releases F once W has written it, then checks the database against the database twin.
    /// With X13 planted, V may release before W has finished; W writes on in later steps, and
    /// reads in between miss F's changes.
    fn release(&mut self) -> Step {
        let early = planted!(self.config.planted, X13);
        let Some(mut w) = self.w.take_if(|w| !w.released && (w.steps == 2 || early)) else {
            return Ok(());
        };
        let generation = w.frozen.generation();
        let done = w.steps == 2;
        if done {
            drop(w);
        } else {
            w.released = true;
            self.w = Some(w);
        }
        self.cache.release(generation);
        if done {
            // Step 11: the engine's hook after a Bk. A refusal ends the run, as a refused write
            // does.
            self.tuning.after_bk(&*self.db).map_err(|_| Stop::Fatal)?;
        }
        self.stats.releases += 1;
        tip_best(&self.cache, &mut self.twin, &mut self.twin_db, "the best block after a release")?;
        let flushed = self.twin_db.best_block().and_then(block_of);
        ensure(self.cache.durable() == flushed, "the durable best block after a release")?;
        if done {
            let ours = self.db_coins();
            let theirs: Vec<_> =
                self.twin_db.iter().map(|(op, v)| (OutPointKey::new(op), v.to_vec())).collect();
            ensure(ours == theirs, "the database after a release")?;
        }
        Ok(())
    }

    /// The database's coins, in the database twin's (outpoint) order.
    fn db_coins(&self) -> Vec<(OutPointKey, Vec<u8>)> {
        let mut coins = db_coins(&self.db);
        coins.sort_by_key(|a| a.0.outpoint());
        coins
    }

    /// V's prefetch request for three keys, stamped with `cache`'s current release generation as
    /// V dispatches it (CH4-1: the stamp comes before any snapshot that reads it, O5).
    fn request(cache: &CoinsCache<ProbeEngine<T::E>>, ks: [u8; 3]) -> PrefetchRequest {
        PrefetchRequest {
            fetch_gen: cache.released_gen(),
            keys: ks.map(key).to_vec(),
        }
    }

    /// A producer reads `req` ([`read_batch`]): its snapshot is taken now, after whatever ran
    /// since V stamped it.
    fn read(
        db: &Db<T::E>,
        req: PrefetchRequest,
        pending: &mut VecDeque<PrefetchBatch>,
    ) -> Result<(), CoinsReadError> {
        pending.push_back(read_batch(db, &CoinsTable::default(), req)?);
        Ok(())
    }

    /// Admits the pending batches in order; a stale one must admit nothing, and the map stays
    /// within its headroom (B/16).
    fn admit(&mut self) -> Step {
        let x10 = planted!(self.config.planted, X10);
        while let Some(batch) = self.pending.pop_front() {
            let stale = batch.fetch_gen != self.cache.released_gen();
            let admitted = self.cache.admit_prefetch(batch, self.mempool);
            if stale {
                self.stats.stale_batches += 1;
                ensure(admitted == 0 || x10, "a stale batch admitted")?;
            }
            // A shrinking budget lowers the cap under a full map, which only admissions must heed.
            let cap = self.cache.budget().prefetch_cap(self.mempool);
            ensure(admitted == 0 || self.cache.usage().prefetch <= cap, "the prefetch headroom")?;
            self.stats.admitted += admitted as u64;
        }
        Ok(())
    }

    /// A block boundary at Core's call site (CH2 §6.7): the undo is written, no overlay is open,
    /// and the size checks decide from `core_size_arith` on native Σ. THRESHOLD freezes; CRITICAL
    /// waits for F, then freezes; a boundary that does not freeze runs the boundary rebuild.
    fn boundary(&mut self) -> Step {
        self.undo.clear();
        self.stats.boundaries += 1;
        let u = self.cache.usage();
        ensure(u.held_out == 0, "held_out at a boundary")?;
        let size = self.cache.size_state(self.mempool);
        let b = self.cache.budget().total(self.mempool);
        ensure(size == core_size_arith(u.total(), b), "size_state")?;
        let in_flight = self.w.is_some();
        let state = self.cache.boundary_state(self.mempool, in_flight);
        ensure(state == expected_boundary(&self.cache, self.mempool, in_flight), "boundary_state")?;
        self.cache.sanity_check().map_err(|_| Stop::Mismatch("sanity_check"))?;
        self.view()?;
        match state {
            CoinsBoundary::Threshold => self.freeze()?,
            CoinsBoundary::Critical => self.flush_always()?,
            CoinsBoundary::Large | CoinsBoundary::Ok => {
                self.cache.boundary_rebuild();
            }
        }
        Ok(())
    }

    /// Every key's coin through the cache's layers equals the twins', without caching a coin; so
    /// does the best block.
    fn view(&mut self) -> Step {
        for b in 0..=u8::MAX {
            let k = key(b);
            let ours = match self.cache.cached(&k) {
                Some(e) => {
                    (!e.flags.is_spent()).then(|| super::packed::decode_trusted(e.coin).to_coin())
                }
                None => match self.cache.peek_below(&k) {
                    Ok(below) => below.map(|b| match b {
                        super::cache::Below::Prefetched(c) => c.as_ref().to_coin(),
                        super::cache::Below::Read(c) => c.as_ref().to_coin(),
                    }),
                    Err(e) => return Err(self.fail(&e)),
                },
            };
            let theirs = match self.twin.entry(&k) {
                Some(e) => e.coin.clone(),
                None => self
                    .twin_db
                    .coin(&k.outpoint())
                    .unwrap_or_else(|e| match e {})
                    .map(|c| c.to_coin()),
            };
            ensure(ours == theirs, "the view")?;
            tip_entry(&self.cache, &self.twin, &self.twin_only, &k)?;
        }
        tip_best(&self.cache, &mut self.twin, &mut self.twin_db, "the view's best block")
    }

    /// `ResizeCoinsCaches`: a new coins share, and perhaps a new coins database size, then the
    /// flush Core runs: IF_NEEDED (the boundary check) for a grown share, ALWAYS for a shrunk one
    /// or a database-only change (O9).
    fn resize(&mut self, b: u8) -> Step {
        let share = self.config.coins_share / 2 * (1 + u64::from(b % 4));
        match self.cache.resize(share, b & 4 != 0) {
            Resized::Unchanged => Ok(()),
            Resized::Grew => self.boundary(),
            Resized::Shrank | Resized::DbOnly => {
                self.stats.always_flushes += 1;
                self.boundary()?;
                self.flush_always()
            }
        }
    }

    /// A flush that empties the cache: W finishes F if it is in flight, then active freezes.
    fn flush_always(&mut self) -> Step {
        if self.w.is_some() {
            self.w_step()?;
            self.w_step()?;
            self.release()?;
        }
        self.freeze()
    }

    // ---- coins_overlay_diff ----

    /// A random parent, then one overlay over it.
    fn overlay_diff(&mut self, input: &mut Input<'_>, meter: Option<&Meter>) -> Step {
        // The database and F: coins in the database; some, changed, in F.
        for _ in 0..usize::from(input.arg() % 32) {
            let (k, coin) = (key(input.arg()), coin(input.arg() & 0x7d, 1));
            let mut b = self.db.batch();
            CoinsTable::default().put(&mut b, &k, PackedCoin::from_coin(coin.as_ref()).as_bytes());
            self.db.write(b, Commit::Buffered).map_err(|_| Stop::Fatal)?;
            self.twin_db.write(k.outpoint(), dash_consensus::coins::CoinWrite::Put(coin));
        }
        if input.arg() & 1 == 1 {
            for _ in 0..usize::from(input.arg() % 16) {
                self.tip_add(input.arg(), input.arg() | 0x80)?;
                // A third of them spent.
                let kb = input.arg();
                if kb.is_multiple_of(3) {
                    self.tip_spend(kb)?;
                }
            }
            self.freeze()?;
        }
        // Active: entries with every flag state Core's tables use.
        for _ in 0..usize::from(input.arg() % 32) {
            let (kb, state) = (input.arg(), input.arg());
            self.parent_entry(kb, state)?;
        }
        if let Some(meter) = meter {
            meter.check(self.cache.usage(), &self.pending)?;
        }
        self.block(input, meter)?;
        self.view()
    }

    /// Puts an entry in active and the same in the tip twin: clean, DIRTY, FRESH or DIRTY|FRESH,
    /// spent or not. A clean entry is the database's coin (a fetch); an entry Core never builds
    /// (clean and spent, or FRESH over a coin F or the database holds) is skipped.
    fn parent_entry(&mut self, kb: u8, state: u8) -> Step {
        let k = key(kb);
        if self.cache.active().get(&k).is_some() || self.twin.entry(&k).is_some() {
            return Ok(());
        }
        let below = self.cache.frozen().and_then(|f| f.coins().get(&k)).is_some()
            || self.twin_db.coin(&k.outpoint()).unwrap_or_else(|e| match e {}).is_some();
        match state % 5 {
            // Clean: fetched.
            0 => self.tip_fetch(kb),
            // DIRTY, unspent: added over whatever is below.
            1 => self.tip_add(kb, state | 0x80),
            // DIRTY, spent: fetched (or not) and spent.
            2 => self.tip_spend(kb),
            // FRESH (DIRTY|FRESH): added over nothing.
            3 if !below => self.tip_add(kb, state & 0x7d),
            // FRESH then spent: removed again, as Core removes it.
            4 if !below => {
                self.tip_add(kb, state & 0x7d)?;
                self.tip_spend(kb)
            }
            _ => Ok(()),
        }
    }
}
