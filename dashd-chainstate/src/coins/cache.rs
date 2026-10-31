//! [`CoinsCache`]: Core's tip `CCoinsViewCache` over the coins database, in layers (CH2 §2.4.1,
//! §3.2).
//!
//! The tip is **active** (the layer V writes), at most one **frozen** layer F that a flush is
//! writing (A1 §3.4), the [`PrefetchMap`](super::prefetch), and the database. A lookup takes the
//! first of them that has the key: a tombstone in active or F is authoritative. A prefetch or
//! database hit is copied into active as clean, as Core's `FetchCoin` emplaces it
//! (`coins.cpp:44-59`); an F hit is not, since F lives until it is released.
//!
//! **Why active is Core's tip** (§3.2, Opus's equivalence argument): freezing at tip `new` hands W
//! every change since the last durable tip, and while W writes it, F over the database answers as a
//! database at `new` would. Core's `Flush()` leaves its cache empty, as a freeze leaves active, so
//! active is Core's `CoinsTip()` just after a flush at `new`, over a base whose `GetCoin` returns
//! unspent coins only. Every `CCoinsViewCache` rule applies to it unchanged. Core's cache holds
//! everything since its last flush and ours since the last durable one is active ∪ F, so the two
//! defensive checks (`AddCoin`'s overwrite check and `BatchWrite`'s FRESH check) consult active,
//! then F. FRESH marking consults active only: where F holds a tombstone and active nothing, a
//! re-added coin is FRESH where Core's would not be, with the same database outcome (D2).
//!
//! Cache membership ([`CoinsCache::have_in_cache`], DEC-102) answers from active, then F, and never
//! reads the prefetch map or the database. It equals Core's `HaveCoinInCache` unless a Core flush
//! or one of our releases falls between the coin entering the cache and the query (D12).

use std::sync::Arc;

use dash_chain::BlockRef;
use dash_consensus::coins::CoinRef;
use dash_consensus::primitives::TxOut;
use dashd_store::KvEngine;

use super::budget::{
    BoundaryCheck, CoinsBoundary, CoinsBudget, CoinsCacheSizeState, Usage, core_size_arith,
};
use super::hasher::Salt;
use super::key::OutPointKey;
use super::layer::{CoinLayerOps, Flags, ShardedLayer, SlotRef, dirty_bytes_of};
use super::ledger::HeldOut;
use super::packed::{PackedCoin, decode_trusted};
use super::prefetch::{PrefetchBatch, PrefetchMap};
use super::table::CoinsTable;
use crate::error::CoinsError;
#[cfg(feature = "planted-bugs")]
use crate::planted::Planted;
use crate::planted::planted;
use crate::store::{CoinsReadError, GuardedEngine};

/// A coin to add: a transaction output (a connect), or a coin restored from undo (a disconnect).
#[derive(Clone, Copy, Debug)]
pub enum CoinSrc<'a> {
    Output {
        out: &'a TxOut,
        height: u32,
        coinbase: bool,
    },
    Bytes(CoinRef<'a>),
}

impl CoinSrc<'_> {
    /// The coin's bytes, or `None` for an unspendable output, which Core never adds
    /// (`coins.cpp:72`).
    ///
    /// # Errors
    ///
    /// [`CoinsError::ValueRange`] for a null output: Core asserts `!coin.IsSpent()`.
    pub(crate) fn pack(self) -> Result<Option<PackedCoin>, CoinsError> {
        let unspendable = match self {
            Self::Output {
                out,
                height,
                coinbase,
            } => CoinRef::from_output(out, height, coinbase).script_pubkey().is_unspendable(),
            Self::Bytes(coin) => coin.script_pubkey().is_unspendable(),
        };
        let coin = match self {
            Self::Output {
                out,
                height,
                coinbase,
            } => PackedCoin::from_output(out, height, coinbase)?,
            Self::Bytes(coin) => PackedCoin::from_coin(coin),
        };
        Ok((!unspendable).then_some(coin))
    }
}

/// A layer a flush is writing. Immutable after the freeze; V and W each hold an `Arc` to it, and W
/// drops its own before it reports the flush done.
pub struct FrozenLayer {
    coins: ShardedLayer,
    generation: u64,
    tip: BlockRef,
}

impl FrozenLayer {
    pub fn coins(&self) -> &ShardedLayer {
        &self.coins
    }

    /// The release generation it froze in: [`CoinsCache::release`] takes it back.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The block its entries bring the database to.
    pub fn tip(&self) -> BlockRef {
        self.tip
    }
}

/// What a resize asks of the flush policy (`ResizeCoinsCaches`, `validation.cpp:5866-5892`). Core
/// returns early only when both cache sizes are unchanged; otherwise it flushes IF_NEEDED if the
/// coins share grew, and ALWAYS if not, including when only the coins database's size changed
/// (O9). b1's flush policy maps these onto Core's modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resized {
    /// Neither size changed: no flush.
    Unchanged,
    /// The coins share grew: an IF_NEEDED flush (`:5889`).
    Grew,
    /// The coins share shrank: an ALWAYS flush (`:5892`).
    Shrank,
    /// Only the coins database's cache size changed: an ALWAYS flush (`:5892`).
    DbOnly,
}

/// A coin below active and F, read without caching it.
pub(crate) enum Below<'a> {
    Prefetched(&'a PackedCoin),
    Read(PackedCoin),
}

/// The coins cache: Core's `CoinsTip()` over `F over the database`.
pub struct CoinsCache<E: KvEngine> {
    active: ShardedLayer,
    frozen: Option<Arc<FrozenLayer>>,
    prefetch: PrefetchMap,
    /// Releases so far: a prefetch batch is admitted only if read in the current generation.
    released_gen: u64,
    /// The database's write epoch at the last release: a batch of the current generation was read
    /// at or after it (CH2 §2.4.3 item 1).
    release_epoch: u64,
    /// Core's `hashBlock`; `None` is Core's null hash, which reads through to the base.
    best: Option<BlockRef>,
    /// The database's best block: as opened, then each released F's tip.
    durable: Option<BlockRef>,
    table: CoinsTable,
    db: Arc<GuardedEngine<E>>,
    budget: CoinsBudget,
    salt: Salt,
    held_out: HeldOut,
    /// The last block overlay's child, emptied by [`ShardedLayer::reset`], for the next overlay
    /// to take: a block reuses its tables rather than allocating 64 (CH2c D36). Counted in native
    /// Σ as `overlay`.
    spare: Option<ShardedLayer>,
    xcheck: bool,
    #[cfg(feature = "planted-bugs")]
    pub(super) planted: Option<Planted>,
}

impl<E: KvEngine> CoinsCache<E> {
    /// A cache over `db`, whose best block is `durable`, with an empty mempool's budget.
    pub fn new(
        db: Arc<GuardedEngine<E>>,
        budget: CoinsBudget,
        salt: Salt,
        durable: Option<BlockRef>,
    ) -> Self {
        Self {
            active: ShardedLayer::new(budget.presize(0, 0), salt),
            frozen: None,
            prefetch: PrefetchMap::new(salt),
            released_gen: 0,
            release_epoch: 0,
            best: None,
            durable,
            table: CoinsTable::default(),
            db,
            budget,
            salt,
            held_out: HeldOut::default(),
            spare: None,
            xcheck: false,
            #[cfg(feature = "planted-bugs")]
            planted: None,
        }
    }

    /// `-consensusxcheck` (A1 §3.4 item 5): a prefetch hit is also read from the database, and a
    /// difference aborts.
    pub fn set_xcheck(&mut self, on: bool) {
        self.xcheck = on;
    }

    #[cfg(feature = "planted-bugs")]
    pub fn plant(&mut self, bug: Planted) {
        self.planted = Some(bug);
        self.table.plant(bug);
    }

    // ---- Core's CCoinsViewCache over base := F over the database ----

    /// The first entry for `k` in active, then F.
    pub(crate) fn cached(&self, k: &OutPointKey) -> Option<SlotRef<'_>> {
        self.active.get(k).or_else(|| self.frozen.as_ref()?.coins.get(k))
    }

    /// The coin at `k` below active and F, moved out of the prefetch map or read from the
    /// database.
    fn read_below(&mut self, k: &OutPointKey) -> Result<Option<PackedCoin>, CoinsReadError> {
        let Some(coin) = self.prefetch.take(k) else {
            return self.table.get(&*self.db, k);
        };
        self.xcheck(k, &coin)?;
        Ok(Some(coin))
    }

    /// The coin at `k` below active and F, without caching it: borrowed from the prefetch map, or
    /// read from the database.
    pub(crate) fn peek_below(&self, k: &OutPointKey) -> Result<Option<Below<'_>>, CoinsReadError> {
        match self.prefetch.get(k) {
            Some(coin) => self.xcheck(k, coin).map(|()| Some(Below::Prefetched(coin))),
            None => Ok(self.table.get(&*self.db, k)?.map(Below::Read)),
        }
    }

    /// Under xcheck, reads a prefetched coin again from the database; a difference aborts through
    /// the corruption path (it trips the latch and reaches the abort sink).
    fn xcheck(&self, k: &OutPointKey, prefetched: &PackedCoin) -> Result<(), CoinsReadError> {
        if !self.xcheck || self.table.get(&*self.db, k)?.as_ref() == Some(prefetched) {
            return Ok(());
        }
        let mut buf = [0; super::key::DB_KEY_MAX];
        let n = k.db_key(&mut buf);
        let key = &buf[..n];
        Err(self.db.corrupt(
            self.table.cf(),
            key,
            "consensusxcheck: the prefetched coin differs from the database's",
        ))
    }

    // Core: coins.cpp:44-68 FetchCoin, GetCoin
    /// `GetCoin`: the unspent coin at `k`. A prefetch or database hit is cached in active as clean.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] from the database; the latch has tripped.
    pub fn fetch(&mut self, k: &OutPointKey) -> Result<Option<CoinRef<'_>>, CoinsReadError> {
        Ok(self.fetch_bytes(k)?.map(decode_trusted))
    }

    /// [`CoinsCache::fetch`], as the coin's bytes.
    pub(crate) fn fetch_bytes(&mut self, k: &OutPointKey) -> Result<Option<&[u8]>, CoinsReadError> {
        if self.cached(k).is_none() {
            let below = match self.read_below(k) {
                // X16a: a failed read as a missing coin, above the guard.
                Err(_) if planted!(self.planted, X16a) => return Ok(None),
                r => r?,
            };
            let Some(coin) = below else {
                return Ok(None);
            };
            let flags = if planted!(self.planted, X31) {
                Flags::DIRTY
            } else {
                Flags::NONE
            };
            self.active.upsert(k, flags, coin.as_bytes());
        }
        Ok(self.cached(k).filter(|e| !e.flags.is_spent()).map(|e| e.coin))
    }

    // Core: coins.cpp:167-170 HaveCoinInCache
    /// `HaveCoinInCache` (DEC-102): whether active, then F, holds `k` unspent. It never reads the
    /// prefetch map or the database.
    pub fn have_in_cache(&self, k: &OutPointKey) -> bool {
        self.cached(k).is_some_and(|e| !e.flags.is_spent())
    }

    // Core: coins.cpp:284-297 Uncache
    /// `Uncache`: drops `k`'s active entry if it is clean (flags ∅). F is immutable.
    pub fn uncache(&mut self, k: &OutPointKey) {
        if self.active.get(k).is_some_and(|e| e.flags == Flags::NONE) {
            self.active.remove(k);
        }
    }

    // Core: coins.cpp:70-108 AddCoin
    /// `AddCoin` on the tip. With `possible_overwrite` false, a coin unspent in active or F is an
    /// error, and the new entry is FRESH unless active's entry is DIRTY.
    ///
    /// # Errors
    ///
    /// [`CoinsError::OverwriteUnspent`] (Core's `logic_error`); [`CoinsError::ValueRange`] for a
    /// null output.
    pub fn add(
        &mut self,
        k: &OutPointKey,
        coin: CoinSrc<'_>,
        possible_overwrite: bool,
    ) -> Result<(), CoinsError> {
        let Some(coin) = coin.pack()? else {
            return Ok(());
        };
        let own = self.active.get(k).map(|e| e.flags);
        let mut fresh = false;
        if !possible_overwrite {
            let unspent = if planted!(self.planted, X9) {
                self.active.get(k).is_some_and(|e| !e.flags.is_spent())
            } else {
                self.have_in_cache(k)
            };
            if unspent {
                return Err(CoinsError::OverwriteUnspent);
            }
            fresh = !own.is_some_and(Flags::is_dirty);
        }
        let mut flags = own.unwrap_or(Flags::NONE).without(Flags::SPENT).union(Flags::DIRTY);
        if fresh {
            flags = flags | Flags::FRESH;
        }
        self.active.upsert(k, flags, coin.as_bytes());
        Ok(())
    }

    // Core: coins.cpp:129-149 SpendCoin (without moveout)
    /// `SpendCoin` on the tip: whether there was an entry to spend. A FRESH active entry is
    /// removed; any other becomes a DIRTY tombstone in active. A coin unspent in F gets a DIRTY,
    /// non-FRESH tombstone in active (F is immutable), and so does a coin read from below. F's
    /// tombstone is no entry: Core's flush has erased the coin, and Core returns false.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] from the database.
    pub fn spend(&mut self, k: &OutPointKey) -> Result<bool, CoinsReadError> {
        let tombstone = |flags: Flags| flags.union(Flags::DIRTY).union(Flags::SPENT);
        match self.active.get(k).map(|e| e.flags) {
            Some(flags) if flags.contains(Flags::FRESH) => {
                self.active.remove(k);
            }
            Some(flags) => self.active.upsert(k, tombstone(flags), &[]),
            None => {
                let unspent = match self.cached(k).map(|e| e.flags) {
                    Some(flags) => !flags.is_spent(),
                    None => self.read_below(k)?.is_some(),
                };
                if !unspent {
                    return Ok(false);
                }
                self.active.upsert(k, tombstone(Flags::NONE), &[]);
            }
        }
        Ok(true)
    }

    // Core: coins.cpp:172-180 GetBestBlock, SetBestBlock
    /// `GetBestBlock`: the cache's best block, else F's, else the database's.
    pub fn best_block(&self) -> Option<BlockRef> {
        self.best.or_else(|| self.frozen.as_ref().map(|f| f.tip)).or(self.durable)
    }

    pub fn set_best_block(&mut self, best: Option<BlockRef>) {
        self.best = best;
    }

    /// The database's best block as of the last release: the block a reopen starts from.
    pub fn durable(&self) -> Option<BlockRef> {
        self.durable
    }

    /// The database: its latch and abort sink are the flush's (V's side, §4.2).
    pub fn db(&self) -> &GuardedEngine<E> {
        &self.db
    }

    /// The database as a handle that outlives the cache's lock: `gettxoutsetinfo`'s scan.
    pub fn db_handle(&self) -> Arc<GuardedEngine<E>> {
        Arc::clone(&self.db)
    }

    /// R as a flush batch's capacity, and B1's at most: a freeze makes B1 with the smaller of R and
    /// what B1 can come to (BL-511), then puts the dirty block-index records into it (CH2c D40).
    pub fn batch_capacity(&self) -> usize {
        usize::try_from(self.budget.reserve(true)).unwrap_or(usize::MAX)
    }

    // Core: coins.cpp:182-252 BatchWrite (erase = true)
    /// Merges a block overlay's child into active, as `BatchWrite` merges a child cache into the tip:
    /// each DIRTY child entry in turn, with F counted as part of the tip, its coin moved up (a long
    /// coin's box without a copy, as Core moves the `Coin`). Ends with Core's
    /// `hashBlock = hashBlockIn`.
    /// `child` is left drained, its tables kept.
    pub(crate) fn merge(
        &mut self,
        child: &mut ShardedLayer,
        best: BlockRef,
    ) -> Result<(), CoinsError> {
        for (k, flags, coin) in child.drain_dirty() {
            let child_fresh = flags.contains(Flags::FRESH);
            let spent = flags.is_spent();
            let own = self.active.get(&k).map(|e| e.flags);
            // F's tombstone is no entry, as in `spend`: Core's flush has erased the coin, so the
            // child's entry moves up as it is (§3.2's table: FRESH kept, FRESH and spent dropped).
            let parent = own.or_else(|| {
                let f = self.frozen.as_ref()?.coins.get(&k)?.flags;
                (!f.is_spent()).then_some(f)
            });
            let Some(parent) = parent else {
                // Core: coins.cpp:191-214, the parent has no entry: the child's entry moves up as it
                // is, DIRTY, FRESH if the child's is, spent if the child's is.
                if !(child_fresh && spent) {
                    self.active.upsert_owned(&k, flags, coin);
                }
                continue;
            };
            if child_fresh && !parent.is_spent() {
                // Core: coins.cpp:217-223
                return Err(CoinsError::FreshMisapplied);
            }
            match own {
                // Core: coins.cpp:225-229, the grandparent has no entry: drop it.
                Some(own) if own.contains(Flags::FRESH) && spent => {
                    self.active.remove(&k);
                }
                // Core: coins.cpp:230-247, a normal modification; SPENT follows the child.
                Some(own) => {
                    let mut new = own.union(Flags::DIRTY).without(Flags::SPENT);
                    if spent {
                        new = new | Flags::SPENT;
                    }
                    self.active.upsert_owned(&k, new, coin);
                }
                // F's unspent coin (a FRESH child failed above), immutable, its flush landing
                // first: a DIRTY entry in active shadows it.
                None => {
                    let new = if spent {
                        Flags::DIRTY | Flags::SPENT
                    } else {
                        Flags::DIRTY
                    };
                    self.active.upsert_owned(&k, new, coin);
                }
            }
        }
        if !planted!(self.planted, X29) {
            self.best = Some(best);
        }
        Ok(())
    }

    /// A child layer for a block overlay: the spare if there is one, else a new one.
    pub(crate) fn take_child(&mut self) -> ShardedLayer {
        self.spare.take().unwrap_or_else(|| ShardedLayer::child(self.salt))
    }

    /// Keeps a dropped overlay's child, committed or not, emptied for the next block.
    pub(crate) fn keep_child(&mut self, mut child: ShardedLayer) {
        child.reset();
        self.spare = Some(child);
    }

    // ---- budget, freeze, release ----

    /// The native ledger (DEC-99, DEC-102): the cache's own allocations, and the shard ceilings
    /// not yet grown to (CH2c D29). An open block overlay reports its own usage, this plus its
    /// child's.
    pub fn usage(&self) -> Usage {
        let frozen = self.frozen.as_ref().map(|f| &f.coins);
        Usage {
            active: self.active.allocated_bytes(),
            frozen: frozen.map_or(0, ShardedLayer::allocated_bytes),
            prefetch: self.prefetch.allocated_bytes(),
            overlay: self.spare.as_ref().map_or(0, ShardedLayer::allocated_bytes),
            held_out: self.held_out.bytes(),
            reserve: self.budget.reserve(self.frozen.is_some()),
            unbacked: self.active.unbacked_bytes() + frozen.map_or(0, ShardedLayer::unbacked_bytes),
        }
    }

    /// [`core_size_arith`](super::budget::core_size_arith) of native Σ against B(t).
    pub fn size_state(&self, mempool_usage: u64) -> CoinsCacheSizeState {
        self.budget.size_state(self.usage().total(), mempool_usage)
    }

    /// The coin side of the flush decision at a block boundary (rev5, O-R6-m5): CRITICAL or LARGE
    /// from native Σ, and THRESHOLD when no flush is in flight and active has reached f·B(t) or a
    /// shard of active its trigger. A boundary that is both THRESHOLD and LARGE reports THRESHOLD,
    /// which freezes in every mode; CRITICAL comes first.
    pub fn boundary_state(&self, mempool_usage: u64, flush_in_flight: bool) -> CoinsBoundary {
        self.boundary(mempool_usage, flush_in_flight).state
    }

    /// [`boundary_state`](Self::boundary_state), with the Σ and B(t) it compared (BL-90(b)).
    pub fn boundary(&self, mempool_usage: u64, flush_in_flight: bool) -> BoundaryCheck {
        let in_flight = flush_in_flight || self.frozen.is_some();
        let mut usage = self.usage();
        usage.reserve = self.budget.reserve(in_flight);
        let (sigma, total) = (usage.total(), self.budget.total(mempool_usage));
        let threshold = !in_flight
            && (self.active.charged_bytes() >= self.budget.threshold(mempool_usage)
                || self.active.at_shard_trigger());
        let state = match core_size_arith(sigma, total) {
            CoinsCacheSizeState::Critical => CoinsBoundary::Critical,
            _ if threshold => CoinsBoundary::Threshold,
            CoinsCacheSizeState::Large => CoinsBoundary::Large,
            CoinsCacheSizeState::Ok => CoinsBoundary::Ok,
        };
        BoundaryCheck {
            state,
            sigma,
            total,
        }
    }

    /// The boundary rebuild (CH2 §2.3, DEC-108): with no F in flight, rebuilds every shard of
    /// active whose removed entries have used up its room below the trigger. V runs it at each
    /// block boundary that does not freeze. Returns the shards rebuilt.
    pub fn boundary_rebuild(&mut self) -> usize {
        if self.frozen.is_some() {
            return 0;
        }
        self.active.boundary_rebuild()
    }

    /// Freezes active in O(1) at the tip's best block: it becomes F, and a new active's ceiling is
    /// what is left beside F's charge (CH2 §2.3, O-R6-m6, CH2c D29). F's tip is the best block, so
    /// a flush cannot record coins and a best block that disagree (O6).
    ///
    /// # Panics
    ///
    /// If F is still in flight (there is at most one), or the tip has no best block: Core flushes
    /// the coins only at one (`validation.cpp:3078`; `txdb.cpp:119` asserts it).
    pub fn freeze(&mut self, mempool_usage: u64) -> Arc<FrozenLayer> {
        assert!(self.frozen.is_none(), "a freeze with F still in flight");
        let tip = self.best_block().expect("a freeze at a best block");
        let presize = self.budget.presize(mempool_usage, self.active.charged_bytes());
        let coins = std::mem::replace(&mut self.active, ShardedLayer::new(presize, self.salt));
        let frozen = Arc::new(FrozenLayer {
            coins,
            generation: self.released_gen,
            tip,
        });
        self.frozen = Some(frozen.clone());
        frozen
    }

    /// Drops F once its flush is durable: the database is at F's tip, the generation advances and
    /// the prefetch map is cleared.
    ///
    /// # Panics
    ///
    /// If there is no F, `generation` is not its generation, or W still holds it (W drops its `Arc`
    /// before it reports the flush done, so the memory is freed before V's ledger forgets it).
    pub fn release(&mut self, generation: u64) {
        let frozen = self.frozen.take().expect("a frozen layer to release");
        assert_eq!(frozen.generation, generation, "the release of another layer");
        if !planted!(self.planted, X13) {
            assert_eq!(Arc::strong_count(&frozen), 1, "W still holds F");
        }
        self.durable = Some(frozen.tip);
        drop(frozen);
        // The epoch before the generation: a producer that sees the new generation then reads an
        // epoch at or after this one, whatever else the database writes in between.
        self.release_epoch = self.db.write_epoch();
        self.released_gen += 1;
        if !planted!(self.planted, X35) {
            self.prefetch.clear();
        }
    }

    /// The current release generation: what V stamps on a prefetch request as it dispatches it.
    pub fn released_gen(&self) -> u64 {
        self.released_gen
    }

    /// The frozen layer in flight.
    pub fn frozen(&self) -> Option<&Arc<FrozenLayer>> {
        self.frozen.as_ref()
    }

    /// Admits a prefetch batch behind the fence (CH2 §2.4.3): only if it was read in the current
    /// generation, and only while the map stays within B(t)/16. Returns the entries admitted.
    ///
    /// # Panics
    ///
    /// In debug builds, a batch of the current generation read before the release that began it:
    /// its producer took the snapshot before it read the generation (DEC-142, O5).
    pub fn admit_prefetch(&mut self, batch: PrefetchBatch, mempool_usage: u64) -> usize {
        // X34: the generation read at admission, after the batch's snapshot.
        let fetch_gen = if planted!(self.planted, X34) {
            self.released_gen
        } else {
            batch.fetch_gen
        };
        if fetch_gen != self.released_gen && !planted!(self.planted, X10) {
            return 0;
        }
        debug_assert!(
            fetch_gen != self.released_gen
                || batch.epoch >= self.release_epoch
                || planted!(self.planted, X32),
            "a prefetch batch read before its generation began: the snapshot came first"
        );
        let cap = if planted!(self.planted, X36) {
            u64::MAX
        } else {
            self.budget.prefetch_cap(mempool_usage)
        };
        self.prefetch.admit(batch.entries, cap)
    }

    /// Clears the prefetch map: every disconnect does (CH2 §2.4.3 item 4).
    pub fn clear_prefetch(&mut self) {
        self.prefetch.clear();
    }

    /// The prefetch map's lookups so far (the membership tests check `have_in_cache` makes none).
    pub fn prefetch_lookups(&self) -> u64 {
        self.prefetch.lookups()
    }

    /// `ResizeCoinsCaches` (`validation.cpp:5866-5892`): the new coins share, and the flush it
    /// asks for. `db_resized` says whether the coins database's cache size changed too; its owner
    /// resizes the database, not the cache.
    pub fn resize(&mut self, coins_share: u64, db_resized: bool) -> Resized {
        let old = self.budget.coins_share();
        self.budget.set_coins_share(coins_share);
        match coins_share.cmp(&old) {
            std::cmp::Ordering::Greater => Resized::Grew,
            std::cmp::Ordering::Less => Resized::Shrank,
            std::cmp::Ordering::Equal if db_resized => Resized::DbOnly,
            std::cmp::Ordering::Equal => Resized::Unchanged,
        }
    }

    pub fn budget(&self) -> &CoinsBudget {
        &self.budget
    }

    pub fn active(&self) -> &ShardedLayer {
        &self.active
    }

    pub(crate) fn held_out(&self) -> &HeldOut {
        &self.held_out
    }

    // Core: coins.cpp:325-340 SanityCheck
    /// Core's `SanityCheck` over active and F: no entry is FRESH alone, spent alone, or spent,
    /// FRESH and DIRTY. Active also never holds a spent FRESH entry that is not DIRTY (§3.2 point
    /// 4), and each layer's counts match its entries.
    ///
    /// # Errors
    ///
    /// The first entry that breaks a rule.
    pub fn sanity_check(&self) -> Result<(), String> {
        check_layer("active", &self.active, true)?;
        self.frozen.as_ref().map_or(Ok(()), |f| check_layer("F", &f.coins, false))
    }

    /// The UTXO set as O2b's shim reads Core's (its `Utxo()`): the database's coins, laid over
    /// by F's entries and then active's (a spent entry removes its coin), in Core's `COutPoint`
    /// order (the txid's bytes, then the index as a number). The prefetched coins are the
    /// database's. Test support: it reads the whole database.
    ///
    /// # Errors
    ///
    /// [`CoinsReadError`] from the database's cursor.
    // Core: src/test/oracle/oracle_chain.cpp:650-668 at the pin, primitives/transaction.h:75-79
    #[cfg(any(test, feature = "test-support"))]
    pub fn tip_coins(&self) -> Result<Vec<(OutPointKey, PackedCoin)>, CoinsReadError> {
        use std::collections::BTreeMap;

        let order = |k: &OutPointKey| {
            let mut txid = [0; 32];
            txid.copy_from_slice(&k.0[..32]);
            (txid, k.vout())
        };
        let mut coins = BTreeMap::new();
        for item in self.table.cursor(&self.db.snapshot()) {
            let (k, coin) = item?;
            coins.insert(order(&k), (k, coin));
        }
        let layers = self.frozen.iter().map(|f| &f.coins).chain([&self.active]);
        for (k, e) in layers.flat_map(|layer| layer.iter()) {
            if e.flags.is_spent() {
                coins.remove(&order(&k));
            } else {
                coins.insert(order(&k), (k, PackedCoin::from_trusted(e.coin)));
            }
        }
        Ok(coins.into_values().collect())
    }

    /// Test seam: puts an entry with any flags into active, as Core's tests insert map entries.
    #[cfg(test)]
    pub(crate) fn put_active(&mut self, k: &OutPointKey, flags: Flags, coin: &[u8]) {
        self.active.upsert(k, flags, coin);
    }
}

/// Core's entry rules over one layer, and its counts. `active` adds rule 4 of §3.2: no spent FRESH
/// entry that is not DIRTY (Core's `FetchCoin` makes one only over a base that returns spent coins).
pub(crate) fn check_layer(name: &str, layer: &ShardedLayer, active: bool) -> Result<(), String> {
    let (mut dirty, mut dirty_bytes, mut len) = (0, 0, 0);
    for (k, e) in layer.iter() {
        let attr = u8::from(e.flags.is_dirty())
            | u8::from(e.flags.contains(Flags::FRESH)) << 1
            | u8::from(e.flags.is_spent()) << 2;
        if matches!(attr, 2 | 4 | 7) || (active && attr == 6) {
            return Err(format!("{name}: {k:?} has flags {:?}", e.flags));
        }
        if e.flags.is_spent() != e.coin.is_empty() {
            return Err(format!("{name}: {k:?} is spent iff it has no coin, broken"));
        }
        len += 1;
        if e.flags.is_dirty() {
            dirty += 1;
            dirty_bytes += dirty_bytes_of(e.coin.len());
        }
    }
    if (len, dirty, dirty_bytes) != (layer.len(), layer.dirty_len(), layer.dirty_bytes()) {
        return Err(format!("{name}: its counts do not match its entries"));
    }
    Ok(())
}
