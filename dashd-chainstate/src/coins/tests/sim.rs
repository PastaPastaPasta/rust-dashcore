//! Core's two simulations (`coins_tests.cpp:102-497`, v24) over our stack (CH2 §6.1). Core's stack
//! of up to four caches becomes the tip and at most one open block overlay, as V uses them; deeper
//! stacks have no counterpart. Between Core's steps the simulations also, at random points: freeze
//! active, let a synthetic W write F, release it, read prefetch batches and admit them (some after
//! a release, which the fence drops), rebuild shards at the boundary, flush everything, and drop an
//! overlay instead of committing it (a failed connect). Every lookup equals a [`MemoryCoins`]
//! reference, `sanity_check` holds after every step, and at the end the database equals the
//! reference byte for byte.
//!
//! Core's `coins_cache_simulation_test` adds with `possible_overwrite` false only for an
//! unspendable coin, which returns before the FRESH rules. [`Adds::Fresh`] also adds a spent or
//! missing coin with `possible_overwrite` false half the time, as `AddCoins` may, so the FRESH
//! paths over F are simulated too.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::coins::{Coin, CoinWrite, CoinsView as _, MemoryCoins};
use dash_consensus::primitives::TxOut;

use super::*;
use crate::coins::{BlockOverlay, CoinSrc, FrozenLayer, PrefetchBatch};
use crate::error::CoinsError;

/// Core's `NUM_SIMULATION_ITERATIONS`.
const ITERATIONS: u32 = 40_000;
const MAX_MONEY: u64 = 21_000_000 * 100_000_000;

/// Core's `stack.back()`: the tip, or the open overlay.
trait Level {
    /// `AccessCoin`: the tip caches a database hit, the overlay does not.
    fn access(&mut self, k: &OutPointKey) -> Option<Coin>;
    fn have(&mut self, k: &OutPointKey) -> bool;
    fn add(
        &mut self,
        k: &OutPointKey,
        coin: CoinSrc<'_>,
        possible_overwrite: bool,
    ) -> Result<(), CoinsError>;
    /// `SpendCoin` with moveout; `None` if there is no unspent coin to spend.
    fn spend(&mut self, k: &OutPointKey) -> Option<Coin>;
    fn sanity(&self) -> Result<(), String>;
}

impl Level for CoinsCache<ProbeEngine> {
    fn access(&mut self, k: &OutPointKey) -> Option<Coin> {
        self.fetch(k).unwrap().map(|c| c.to_coin())
    }

    fn have(&mut self, k: &OutPointKey) -> bool {
        self.fetch(k).unwrap().is_some()
    }

    fn add(
        &mut self,
        k: &OutPointKey,
        coin: CoinSrc<'_>,
        possible_overwrite: bool,
    ) -> Result<(), CoinsError> {
        CoinsCache::add(self, k, coin, possible_overwrite)
    }

    fn spend(&mut self, k: &OutPointKey) -> Option<Coin> {
        let coin = self.access(k)?;
        assert!(CoinsCache::spend(self, k).unwrap());
        Some(coin)
    }

    fn sanity(&self) -> Result<(), String> {
        self.sanity_check()
    }
}

impl Level for BlockOverlay<'_, ProbeEngine> {
    fn access(&mut self, k: &OutPointKey) -> Option<Coin> {
        self.coin(&k.outpoint()).unwrap().map(|c| c.to_coin())
    }

    fn have(&mut self, k: &OutPointKey) -> bool {
        self.have_coin(&k.outpoint()).unwrap()
    }

    fn add(
        &mut self,
        k: &OutPointKey,
        coin: CoinSrc<'_>,
        possible_overwrite: bool,
    ) -> Result<(), CoinsError> {
        BlockOverlay::add(self, k, coin, possible_overwrite)
    }

    fn spend(&mut self, k: &OutPointKey) -> Option<Coin> {
        BlockOverlay::spend(self, k).unwrap().map(|c| c.as_ref().to_coin())
    }

    fn sanity(&self) -> Result<(), String> {
        self.sanity_check()
    }
}

/// How often each branch ran: Core's coverage booleans, and ours.
#[derive(Debug, Default)]
struct Coverage {
    added: u32,
    unspendable: u32,
    updated: u32,
    removed: u32,
    found: u32,
    missed: u32,
    uncached: u32,
    full_flushes: u32,
    overlays: u32,
    overlays_dropped: u32,
    freezes: u32,
    w_writes: u32,
    releases: u32,
    prefetched: usize,
    stale_batches: u32,
    rebuilt: usize,
    duplicate_coinbase_spent: u32,
}

/// What the simulation keeps beside the cache.
struct Env {
    db: Arc<Db>,
    rng: Rng,
    reference: MemoryCoins,
    /// Every outpoint the simulation touched (Core's `result` keys), and the same as a list to
    /// draw from.
    touched: BTreeSet<OutPointKey>,
    pool: Vec<OutPointKey>,
    /// W's handle on F, and whether W has written it.
    w: Option<(Arc<FrozenLayer>, bool)>,
    /// Prefetch batches read and not yet admitted.
    pending: Vec<PrefetchBatch>,
    height: u32,
    cov: Coverage,
}

impl Env {
    fn new(seed: u64) -> (Self, CoinsCache<ProbeEngine>) {
        let World {
            db,
            mut cache,
            ..
        } = World::with_dbcache(2);
        cache.set_xcheck(true);
        let env = Self {
            db,
            rng: Rng::new(seed),
            reference: MemoryCoins::new(),
            touched: BTreeSet::new(),
            pool: Vec::new(),
            w: None,
            pending: Vec::new(),
            height: 0,
            cov: Coverage::default(),
        };
        (env, cache)
    }

    fn expect(&self, k: &OutPointKey) -> Option<Coin> {
        self.reference.coin(&k.outpoint()).unwrap().map(|c| c.to_coin())
    }

    fn set(&mut self, k: &OutPointKey, coin: Option<Coin>) {
        if self.touched.insert(*k) {
            self.pool.push(*k);
        }
        let change = coin.map_or(CoinWrite::Erase, CoinWrite::Put);
        self.reference.write(k.outpoint(), change);
    }

    /// W's step, at any level: it writes F once, some time after the freeze.
    fn w_step(&mut self) {
        if let Some((frozen, written)) = &mut self.w
            && !*written
            && self.rng.one_in(8)
        {
            write_layer(&self.db, frozen.coins());
            *written = true;
            self.cov.w_writes += 1;
        }
    }

    /// A touched outpoint at random.
    fn any_touched(&mut self) -> Option<OutPointKey> {
        (!self.pool.is_empty()).then(|| self.pool[self.rng.below(self.pool.len() as u64) as usize])
    }

    /// V's steps between blocks: freeze, release, prefetch, the boundary rebuild, uncache.
    fn tip_step(&mut self, cache: &mut CoinsCache<ProbeEngine>) {
        if cache.frozen().is_none() && self.rng.one_in(60) {
            self.height += 1;
            self.w = Some((cache.freeze_at(self.height, 0), false));
            self.cov.freezes += 1;
        }
        if matches!(self.w, Some((_, true))) && self.rng.one_in(4) {
            self.release(cache);
        }
        if self.rng.one_in(25) {
            let fetch_gen = cache.released_gen();
            let epoch = self.db.write_epoch();
            let table = CoinsTable::default();
            let picked: Vec<_> = (0..8).filter_map(|_| self.any_touched()).collect();
            let entries = picked
                .into_iter()
                .filter_map(|k| Some((k, table.get(&*self.db, &k).unwrap()?)))
                .collect();
            self.pending.push(PrefetchBatch {
                fetch_gen,
                epoch,
                entries,
            });
        }
        if !self.pending.is_empty() && self.rng.one_in(8) {
            let batch = self.pending.remove(0);
            let stale = batch.fetch_gen != cache.released_gen();
            let admitted = cache.admit_prefetch(batch, 0);
            if stale {
                assert_eq!(admitted, 0, "a stale batch admitted");
                self.cov.stale_batches += 1;
            }
            self.cov.prefetched += admitted;
        }
        if self.rng.one_in(50) {
            self.cov.rebuilt += cache.boundary_rebuild();
        }
        if self.rng.one_in(10)
            && let Some(k) = self.any_touched()
        {
            let had = cache.active().get(&k).is_some();
            cache.uncache(&k);
            self.cov.uncached += u32::from(had && cache.active().get(&k).is_none());
        }
    }

    fn release(&mut self, cache: &mut CoinsCache<ProbeEngine>) {
        let (frozen, written) = self.w.take().expect("F in flight");
        if !written {
            write_layer(&self.db, frozen.coins());
        }
        let generation = frozen.generation();
        drop(frozen);
        cache.release(generation);
        self.cov.releases += 1;
    }

    /// Core's "remove the top cache" with only the tip left: everything reaches the database.
    fn flush_all(&mut self, cache: &mut CoinsCache<ProbeEngine>) {
        if self.w.is_some() {
            self.release(cache);
        }
        self.height += 1;
        self.w = Some((cache.freeze_at(self.height, 0), false));
        self.release(cache);
        assert!(cache.active().is_empty() && cache.frozen().is_none());
        self.cov.full_flushes += 1;
    }

    /// Core's full verification: every touched outpoint reads as the reference says.
    fn verify(&mut self, level: &mut dyn Level, tip: bool) {
        for k in self.touched.clone() {
            let have = level.have(&k);
            let coin = level.access(&k);
            assert_eq!(have, coin.is_some());
            assert_eq!(coin, self.expect(&k), "{k:?}");
            if coin.is_some() {
                self.cov.found += 1;
            } else {
                self.cov.missed += 1;
            }
        }
        if tip {
            level.sanity().unwrap();
        }
    }

    /// The end: everything flushed, and the database equal to the reference.
    fn finish(mut self, cache: &mut CoinsCache<ProbeEngine>) -> Coverage {
        self.flush_all(cache);
        let stored: Vec<_> =
            db_coins(&self.db).into_iter().map(|(k, v)| (k.outpoint(), v)).collect();
        let want: Vec<_> = self.reference.iter().map(|(op, v)| (*op, v.to_vec())).collect();
        let stored: BTreeMap<_, _> = stored.into_iter().collect();
        assert_eq!(stored, want.into_iter().collect::<BTreeMap<_, _>>());
        assert!(self.db.latch().tripped().is_none());
        self.cov
    }
}

/// A simulation's own bookkeeping, which a dropped overlay rolls back with the reference.
trait Rollback {
    type Saved;
    fn save(&self) -> Self::Saved;
    fn restore(&mut self, saved: Self::Saved);
}

impl Rollback for () {
    type Saved = ();
    fn save(&self) {}
    fn restore(&mut self, (): ()) {}
}

/// Runs `step` over the tip, opening an overlay at random points and running it there until the
/// overlay commits or drops.
fn drive<S: Rollback>(
    env: &mut Env,
    cache: &mut CoinsCache<ProbeEngine>,
    state: &mut S,
    mut step: impl FnMut(&mut Env, &mut S, &mut dyn Level, bool),
) {
    let mut i = 0;
    while i < ITERATIONS {
        step(env, state, cache, true);
        env.tip_step(cache);
        env.w_step();
        cache.sanity_check().unwrap();
        i += 1;
        if env.rng.one_in(200) {
            env.flush_all(cache);
        }
        if i < ITERATIONS && env.rng.one_in(100) {
            env.cov.overlays += 1;
            let saved = (env.reference.clone(), state.save());
            let mut overlay = BlockOverlay::new(cache);
            loop {
                step(env, state, &mut overlay, false);
                env.w_step();
                overlay.sanity_check().unwrap();
                i += 1;
                if i >= ITERATIONS || env.rng.one_in(100) {
                    break;
                }
            }
            if env.rng.one_in(4) {
                drop(overlay);
                env.reference = saved.0;
                state.restore(saved.1);
                env.cov.overlays_dropped += 1;
            } else {
                env.height += 1;
                overlay.commit(block(env.height)).unwrap();
            }
            cache.sanity_check().unwrap();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Adds {
    /// Core's choice of `possible_overwrite`.
    Core,
    /// False for half the adds of a spent or missing coin too.
    Fresh,
}

// Core: coins_tests.cpp:102-276 SimulationTest, coins_cache_simulation_test
fn cache_simulation(seed: u64, adds: Adds) -> Coverage {
    let (mut env, mut cache) = Env::new(seed);
    let keys: Vec<_> = (0..ITERATIONS / 8).map(|_| random_key(&mut env.rng, 0)).collect();
    drive(&mut env, &mut cache, &mut (), |env, (), level, tip| {
        let k = keys[env.rng.below(keys.len() as u64) as usize];
        let want = env.expect(&k);
        let before = env.rng.one_in(4);
        let after = env.rng.one_in(4);
        let have_before = before.then(|| level.have(&k));
        let got = level.access(&k);
        assert_eq!(got, want, "{k:?}");
        if let Some(have) = have_before {
            assert_eq!(have, got.is_some());
        }
        if after {
            assert_eq!(level.have(&k), got.is_some());
        }
        if env.rng.one_in(5) || want.is_none() {
            let value = env.rng.below(MAX_MONEY + 1) as i64;
            let unspendable = env.rng.one_in(16) && want.is_none();
            let script = if unspendable {
                vec![0x6a; 1 + env.rng.below(64) as usize]
            } else {
                vec![0; env.rng.below(64) as usize]
            };
            let out = TxOut {
                value,
                script_pubkey: script.into(),
            };
            let possible_overwrite = match adds {
                Adds::Core => !unspendable || env.rng.one_in(2),
                Adds::Fresh => want.is_some() || env.rng.one_in(2),
            };
            let src = CoinSrc::Output {
                out: &out,
                height: 1,
                coinbase: false,
            };
            level.add(&k, src, possible_overwrite).unwrap();
            if unspendable {
                env.cov.unspendable += 1;
            } else {
                if want.is_some() {
                    env.cov.updated += 1;
                } else {
                    env.cov.added += 1;
                }
                env.set(&k, Some(Coin::from_output(&out, 1, false)));
            }
        } else {
            env.cov.removed += 1;
            env.set(&k, None);
            assert_eq!(level.spend(&k), want);
        }
        if env.rng.below(1000) == 1 {
            env.verify(level, tip);
        }
    });
    env.verify(&mut cache, true);
    let cov = env.finish(&mut cache);
    for (n, what) in [
        (cov.added, "added"),
        (cov.unspendable, "unspendable"),
        (cov.updated, "updated"),
        (cov.removed, "removed"),
        (cov.found, "found"),
        (cov.missed, "missed"),
        (cov.uncached, "uncached"),
        (cov.full_flushes, "flushed everything"),
        (cov.overlays, "opened an overlay"),
        (cov.overlays_dropped, "dropped an overlay"),
        (cov.freezes, "froze"),
        (cov.w_writes, "W wrote"),
        (cov.releases, "released"),
        (cov.stale_batches, "dropped a stale batch"),
    ] {
        assert!(n > 0, "never {what}: {cov:?}");
    }
    assert!(cov.prefetched > 0, "{cov:?}");
    cov
}

#[test]
fn coins_cache_simulation_core_adds() {
    for seed in [1, 2] {
        cache_simulation(seed, Adds::Core);
    }
}

#[test]
fn coins_cache_simulation_fresh_adds() {
    for seed in [3, 4] {
        cache_simulation(seed, Adds::Fresh);
    }
}

/// A transaction of the update simulation: one input (none for a coinbase) and one output.
#[derive(Clone, Debug)]
struct SimTx {
    outpoint: OutPointKey,
    prevout: Option<OutPointKey>,
    out: TxOut,
    coinbase: bool,
}

/// Core's `utxoData` entry: the transaction, its undo, and the coin its input spent.
struct TxData {
    tx: SimTx,
    undo: Option<Coin>,
    old: Option<Coin>,
}

// Core: validation.cpp UpdateCoins (the `CTxUndo` overload)
fn update_coins(level: &mut dyn Level, tx: &SimTx, height: u32) -> Option<Coin> {
    let undo = tx.prevout.map(|p| level.spend(&p).expect("UpdateCoins spends an unspent coin"));
    let src = CoinSrc::Output {
        out: &tx.out,
        height,
        coinbase: tx.coinbase,
    };
    level.add(&tx.outpoint, src, tx.coinbase).unwrap();
    undo
}

// Core: validation.cpp ApplyTxInUndo
fn apply_tx_in_undo(level: &mut dyn Level, undo: &Coin, out: &OutPointKey) -> bool {
    let clean = !level.have(out);
    assert_ne!(undo.height(), 0);
    level.add(out, CoinSrc::Bytes(undo.as_ref()), !clean).unwrap();
    clean
}

/// The update simulation's sets (Core's), and its transactions: `data` only grows, and `log`
/// lists its keys in insertion order so a rollback can remove the newest.
#[derive(Default)]
struct Txs {
    coinbase_coins: BTreeSet<OutPointKey>,
    disconnected: BTreeSet<OutPointKey>,
    duplicates: BTreeSet<OutPointKey>,
    utxoset: BTreeSet<OutPointKey>,
    data: BTreeMap<OutPointKey, TxData>,
    log: Vec<OutPointKey>,
}

type SavedSets = [BTreeSet<OutPointKey>; 4];

impl Rollback for Txs {
    type Saved = (SavedSets, usize);

    fn save(&self) -> Self::Saved {
        let sets = [&self.coinbase_coins, &self.disconnected, &self.duplicates, &self.utxoset];
        (sets.map(Clone::clone), self.log.len())
    }

    fn restore(&mut self, ([coinbase_coins, disconnected, duplicates, utxoset], len): Self::Saved) {
        self.coinbase_coins = coinbase_coins;
        self.disconnected = disconnected;
        self.duplicates = duplicates;
        self.utxoset = utxoset;
        for k in self.log.drain(len..) {
            self.data.remove(&k);
        }
    }
}

/// Core's `FindRandomFrom`: the first element at or after a random key, else the first.
fn find_random_from(rng: &mut Rng, set: &BTreeSet<OutPointKey>) -> OutPointKey {
    let from = random_key(rng, 0);
    *set.range(from..).next().or_else(|| set.iter().next()).expect("a non-empty set")
}

// Core: coins_tests.cpp:303-497 updatecoins_simulation_test
fn update_simulation(seed: u64) -> Coverage {
    let (mut env, mut cache) = Env::new(seed);
    let mut n: i64 = 0;
    drive(&mut env, &mut cache, &mut Txs::default(), |env, txs, level, tip| {
        let Txs {
            coinbase_coins,
            disconnected,
            duplicates,
            utxoset,
            data,
            log,
        } = txs;
        n += 1;
        let randiter = env.rng.next() as u32;
        if randiter % 20 < 19 {
            let mut tx = SimTx {
                outpoint: random_key(&mut env.rng, 0),
                prevout: None,
                out: TxOut {
                    value: n,
                    script_pubkey: vec![0; (env.rng.next() & 0x3f) as usize].into(),
                },
                coinbase: true,
            };
            let height = ((env.rng.next() as u32) >> 1).max(1);
            let mut old = None;
            if randiter % 20 < 2 || coinbase_coins.len() < 10 {
                if env.rng.one_in(10) && !coinbase_coins.is_empty() {
                    let dup = find_random_from(&mut env.rng, coinbase_coins);
                    tx = data[&dup].tx.clone();
                    disconnected.remove(&dup);
                    duplicates.insert(dup);
                } else {
                    coinbase_coins.insert(tx.outpoint);
                }
            } else {
                tx.coinbase = false;
                let prevout;
                if randiter % 20 == 2 && !disconnected.is_empty() {
                    let re = find_random_from(&mut env.rng, disconnected);
                    tx = data[&re].tx.clone();
                    prevout = tx.prevout;
                    if !tx.coinbase && !utxoset.contains(&prevout.unwrap()) {
                        disconnected.remove(&re);
                        return;
                    }
                    if utxoset.contains(&re) {
                        assert!(tx.coinbase && duplicates.contains(&re));
                    }
                    disconnected.remove(&re);
                } else {
                    let p = find_random_from(&mut env.rng, utxoset);
                    prevout = Some(p);
                    tx.prevout = prevout;
                }
                if let Some(p) = prevout {
                    old = env.expect(&p);
                    env.set(&p, None);
                    utxoset.remove(&p);
                    if duplicates.contains(&p) {
                        env.cov.duplicate_coinbase_spent += 1;
                    }
                }
            }
            env.set(&tx.outpoint, Some(Coin::from_output(&tx.out, height, tx.coinbase)));
            let undo = update_coins(level, &tx, height);
            utxoset.insert(tx.outpoint);
            if let std::collections::btree_map::Entry::Vacant(e) = data.entry(tx.outpoint) {
                log.push(tx.outpoint);
                e.insert(TxData {
                    tx,
                    undo,
                    old,
                });
            }
        } else if !utxoset.is_empty() {
            let out = find_random_from(&mut env.rng, utxoset);
            let TxData {
                tx,
                undo,
                old,
            } = &data[&out];
            env.set(&out, None);
            if let Some(p) = tx.prevout {
                env.set(&p, old.clone());
            }
            assert!(level.spend(&out).is_some());
            if let Some(p) = tx.prevout {
                apply_tx_in_undo(level, undo.as_ref().unwrap(), &p);
            }
            disconnected.insert(out);
            utxoset.remove(&out);
            if let Some(p) = tx.prevout {
                utxoset.insert(p);
            }
        }
        if env.rng.below(1000) == 1 {
            env.verify(level, tip);
        }
    });
    env.verify(&mut cache, true);
    let cov = env.finish(&mut cache);
    assert!(cov.duplicate_coinbase_spent > 0, "{cov:?}");
    assert!(cov.overlays_dropped > 0 && cov.releases > 0 && cov.stale_batches > 0, "{cov:?}");
    cov
}

#[test]
fn updatecoins_simulation() {
    for seed in [5, 6] {
        update_simulation(seed);
    }
}
