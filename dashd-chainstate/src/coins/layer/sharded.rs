//! [`ShardedLayer`]: 64 hashbrown shards of 72-byte slots, each growing to its ceiling, the presize
//! (CH2 §2.3, CH2c D29), and doubling on its own once full past it (DEC-95).
//!
//! **The ceiling (CH2c D29, DEC-388).** The presize fixes the routing and each shard's ceiling, but
//! a shard starts at [`ShardSizes::start_buckets`], 1/2^d of its ceiling, and below the ceiling it
//! doubles as any hash table does. The rules below (the trigger, the rebuilds, doubling only at or
//! above the trigger) apply only at or past a shard's ceiling, so a layer grown to its ceiling is
//! the presized layer they describe. The cache charges each shard at least its ceiling
//! ([`ShardedLayer::charged_bytes`]), so it freezes and flushes a lazy layer where it would a
//! presized one. Tables of 64 KiB or more are mapped and unmapped ([`super::pages`]).
//!
//! The presize `p` (bytes) buys `(p − overhead) / 73` buckets (a slot and a control byte; the
//! overhead is the shard array and each shard's trailing group of control bytes), split into shard
//! sizes `2^k` and `2^(k+1)`: `s` shards of `2^(k+1)` and `64 − s` of `2^k`, the largest such sum
//! not above the target, so the rounding loss is under one small shard (< 1/64). A key goes to the
//! shard its salted hash falls in when the presized buckets are laid end to end, so expected loads
//! are equal fractions of each shard's presize, and the routing never changes.
//!
//! **Requirement (CH2 §2.3; r6 m8): the routing bits are disjoint from hashbrown's.** A shard is
//! chosen from the hash's bits 25..=56. hashbrown picks a bucket from the low bits (fewer than 25
//! while a shard has at most 2^25 buckets, which a debug assertion checks) and takes its 7-bit
//! control tag from the top seven, 57..=63. Routing on the top bits, as `(h · total) >> 64` would,
//! gives every key in a shard nearly the same tag, so the SIMD tag match stops filtering and each
//! probe compares several 36-byte keys.
//!
//! A shard has room for inserts up to hashbrown's growth load, 7/8 of its buckets. An insert into a
//! shard at or above its trigger (below) with no room left can double that shard alone (a shard is
//! at most 2/(64 + s) of the presized layer, and the old and new tables live together for the
//! move), so a layer grows past its presize in small steps and a block that needs more room than
//! the presize always completes. The
//! cache checks the budget after the block, at Core's points; the layer never refuses an entry.
//!
//! The shard trigger (CH2 §2.3, rev4) is 55/64 of a shard's buckets, 1/64 below the growth load
//! from 64-bucket shards up (a smaller shard's trigger is its growth load). The cache freezes
//! active at a block boundary once any shard holds that many entries
//! ([`ShardedLayer::at_shard_trigger`]). Below the trigger, a shard's room is its growth load less
//! its entries and its DELETED buckets (below): a rebuild leaves over 1/64 of its buckets, but
//! removes use that room up again while the shard stays below its trigger. So at each block
//! boundary with no F in flight the cache also rebuilds every shard whose entries and DELETED
//! buckets have reached its trigger while its entries have not
//! ([`ShardedLayer::boundary_rebuild`], DEC-108). A block then doubles a shard with no F in flight
//! only if it puts more than 1/64 of the shard's buckets into it.
//!
//! hashbrown marks a removed entry's bucket DELETED, still using up insert room, unless no probe
//! could have passed it; when the room runs out it rehashes in place only at a load of 7/16 or
//! less, and above that it doubles. So removes alone would make shards double far below the
//! trigger. An insert into a shard whose room is gone while it holds fewer entries than its trigger
//! rebuilds it at its size instead, dropping the removed entries: its old and new tables live
//! together for the move, as in a doubling, and the rebuild leaves room for over 1/64 of its
//! buckets. A shard therefore doubles only at or above its trigger. [`ShardedLayer::rebuilds`]
//! counts the rebuilds.
//!
//! Iteration walks shard 0 to 63 in each shard's table order, which depends on the salt: random in
//! production, fixed under the seam and in benches.

use hashbrown::HashTable;

use super::long::{LongCoins, LongRef};
use super::pages::{self, Pages};
use super::{CoinLayerOps, Flags, LEN_LONG, SLOT_BYTES, Slot, SlotRef, dirty_bytes_of};
use crate::alloc_tag;
use crate::coins::hasher::{Salt, SaltedHasher};
use crate::coins::key::OutPointKey;
use crate::coins::packed::PackedCoin;

pub const SHARDS: usize = 64;
/// A shard's bytes per bucket: the slot and its control byte.
pub const BUCKET_BYTES: usize = SLOT_BYTES + 1;
/// The lowest routing bit: bits `ROUTE_SHIFT..ROUTE_SHIFT + 32` of the hash pick the shard.
const ROUTE_SHIFT: u32 = 25;
/// The smallest shard: hashbrown's 7/8 load needs at least 8 buckets.
const MIN_SHARD_LOG2: u32 = 3;
/// A new layer's smallest shard table: `2^8` buckets, each shard starting at the same fraction of
/// its ceiling (CH2c D29), but never below its ceiling's `2^-START_MAX_SHIFT`.
const START_LOG2: u32 = 8;
/// A shard starts at no less than 1/8 of its ceiling, so it doubles at most three times to reach it
/// (CH2c D30: DEC-388's CPU budget).
const START_MAX_SHIFT: u32 = 3;
/// The most buckets [`ShardedLayer::reset`] keeps a grown shard table at (CH2c D36): a block
/// overlay's child grows every shard past its ceiling, and keeping the tables a block of some
/// thousand coins needs spares the next block that regrowth, while a kept layer holds at most
/// `64 · 64` buckets (≈ 300 KiB, counted as `Usage::overlay`).
const KEEP_BUCKETS: usize = 64;
/// hashbrown's control bytes past the last bucket, at most: its `Group::WIDTH` is 16 with SSE2 and
/// 8 with NEON or the generic group.
const TRAILING_CTRL: usize = 16;
/// A layer's table bytes besides its buckets and their rounding to whole pages ([`pages`]), at
/// most: the shard array and each shard's trailing group.
pub const TABLE_OVERHEAD: u64 = (size_of::<[Table; SHARDS]>() + SHARDS * TRAILING_CTRL) as u64;

type Table = HashTable<Slot, Pages>;

/// Where keys go: `s` shards of `2^(k+1)` buckets, then `64 − s` of `2^k`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardSizes {
    pub k: u32,
    pub s: u32,
}

impl ShardSizes {
    /// The decomposition of a presize of `bytes`: the largest `2^k · (64 + s)` buckets, `s < 64`,
    /// whose tables fit in `bytes` (at least 8 buckets per shard).
    pub fn for_bytes(bytes: u64) -> Self {
        let target = (bytes.saturating_sub(TABLE_OVERHEAD) / BUCKET_BYTES as u64)
            .max((SHARDS as u64) << MIN_SHARD_LOG2);
        let k = (target / SHARDS as u64).ilog2();
        let s = (target >> k) - SHARDS as u64;
        debug_assert!(s < SHARDS as u64);
        let mut z = Self {
            k,
            s: s as u32,
        };
        // Tables of 64 KiB or more are charged whole pages, under one a shard: give up shard
        // sizes until they fit.
        while z.table_bytes() > bytes && z.k > MIN_SHARD_LOG2 {
            z = match z.s {
                0 => Self {
                    k: z.k - 1,
                    s: SHARDS as u32 - 1,
                },
                s => Self {
                    k: z.k,
                    s: s - 1,
                },
            };
        }
        z
    }

    /// A shard's buckets at its ceiling.
    pub fn buckets(self, shard: usize) -> usize {
        if shard < self.s as usize {
            2 << self.k
        } else {
            1 << self.k
        }
    }

    /// A new layer's buckets in shard `shard`: its ceiling's `2^-d`, `d` putting the smallest shard
    /// at `2^START_LOG2` buckets (or its ceiling, if smaller), with `d` at most [`START_MAX_SHIFT`].
    pub fn start_buckets(self, shard: usize) -> usize {
        self.buckets(shard) >> self.k.saturating_sub(START_LOG2).min(START_MAX_SHIFT)
    }

    pub const fn total_buckets(self) -> u64 {
        (SHARDS as u64 + self.s as u64) << self.k
    }

    /// The bytes the tables are charged at their ceilings ([`ceiling_bytes`]): at least what they
    /// allocate there (exactly with SSE2 and 4 KiB pages).
    pub const fn table_bytes(self) -> u64 {
        let (small, big) = (ceiling_bytes(1 << self.k), ceiling_bytes(2 << self.k));
        let shards = (SHARDS - self.s as usize) * small + self.s as usize * big;
        (size_of::<[Table; SHARDS]>() + shards) as u64
    }

    /// The shard of hash `h`: `h`'s bits 25..=56 placed over the buckets laid end to end. hashbrown
    /// takes a shard's bucket from the low bits and its control byte from the top seven, so the
    /// three uses of the hash do not overlap (the module's requirement).
    #[inline]
    pub fn shard_of(self, h: u64) -> usize {
        debug_assert!(self.total_buckets() <= 1 << 32, "a presize of at most ≈ 290 GiB");
        let r = (u64::from((h >> ROUTE_SHIFT) as u32) * self.total_buckets()) >> 32;
        let big = u64::from(self.s) << (self.k + 1);
        if r < big {
            (r >> (self.k + 1)) as usize
        } else {
            self.s as usize + ((r - big) >> self.k) as usize
        }
    }
}

/// The shard rebuilds a layer made (CH2 §2.3, rev4: a1 reports their frequency and cost).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rebuilds {
    pub count: u64,
    /// The entries the rebuilds moved.
    pub entries: u64,
}

/// A position in a layer's iteration order: a shard, and an entry within its table. Valid while
/// the layer is unchanged, as a frozen layer is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LayerPos {
    shard: usize,
    slot: usize,
}

/// A DIRTY entry of a layer, by reference ([`ShardedLayer::iter_dirty_refs_from`]).
#[derive(Clone, Copy)]
pub(crate) struct DirtyRef<'a>(&'a Slot);

impl DirtyRef<'_> {
    pub(crate) fn key(&self) -> &OutPointKey {
        &self.0.key
    }
}

/// One coins cache layer.
pub struct ShardedLayer {
    shards: Box<[Table; SHARDS]>,
    sizes: ShardSizes,
    long: LongCoins,
    hasher: SaltedHasher,
    live: u32,
    dirty: u32,
    dirty_bytes: u64,
    /// Σ [`footprint`] of the shards, kept as they grow: what [`Self::table_bytes`] reads.
    tables: u64,
    /// Σ of each shard's ceiling bytes it does not yet hold: what [`Self::unbacked_bytes`] reads.
    unbacked: u64,
    rebuilds: Rebuilds,
}

impl ShardedLayer {
    /// A layer whose ceiling is `presize` bytes: the routing is the ceiling's, and every shard
    /// starts at [`ShardSizes::start_buckets`].
    pub fn new(presize: u64, salt: Salt) -> Self {
        Self::starting(presize, salt, ShardSizes::start_buckets, false)
    }

    /// A block overlay's child: [`Self::new`] at the smallest presize, its tables from the heap,
    /// never mapped (CH2c D47). It is emptied and refilled every block ([`Self::reset`]), and the
    /// heap reuses its tables' pages where a mapping would fault them in again each block.
    pub(crate) fn child(salt: Salt) -> Self {
        Self::starting(0, salt, ShardSizes::start_buckets, true)
    }

    /// A layer whose shards start at their ceilings, as every layer did before CH2c D29: the tests'
    /// reference for the lazy layer, and for the rules at the ceiling.
    #[cfg(test)]
    pub(crate) fn presized(presize: u64, salt: Salt) -> Self {
        Self::starting(presize, salt, ShardSizes::buckets, false)
    }

    fn starting(
        presize: u64,
        salt: Salt,
        start: fn(ShardSizes, usize) -> usize,
        heap: bool,
    ) -> Self {
        let _tag = alloc_tag::coins();
        let sizes = ShardSizes::for_bytes(presize);
        let pages = pages(heap);
        let mut layer = Self {
            shards: Box::new(std::array::from_fn(|i| table(start(sizes, i), pages))),
            sizes,
            long: LongCoins::default(),
            hasher: SaltedHasher::new(salt),
            live: 0,
            dirty: 0,
            dirty_bytes: 0,
            tables: 0,
            unbacked: 0,
            rebuilds: Rebuilds::default(),
        };
        layer.count_tables(start);
        layer
    }

    /// Counts [`Self::tables`] and [`Self::unbacked`], every shard being at `start`.
    fn count_tables(&mut self, start: fn(ShardSizes, usize) -> usize) {
        // Shards below `s` are the big ones (`shard_of`), and the last is always small: two sizes
        // to count, not 64 (a block overlay is a layer too).
        let big = u64::from(self.sizes.s);
        let (fb, fs) = (footprint(&self.shards[0]), footprint(&self.shards[SHARDS - 1]));
        self.tables = big * fb as u64 + (SHARDS as u64 - big) * fs as u64;
        self.unbacked = big * self.unbacked_of(0, fb)
            + (SHARDS as u64 - big) * self.unbacked_of(SHARDS - 1, fs);
        debug_assert!(
            self.shards.iter().enumerate().all(|(i, t)| t.num_buckets() == start(self.sizes, i))
        );
        debug_assert!(self.table_bytes() <= self.sizes.table_bytes());
    }

    pub fn sizes(&self) -> ShardSizes {
        self.sizes
    }

    /// Each shard's entries and current buckets, for the load and routing checks.
    pub fn shard_loads(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.shards.iter().map(|t| (t.len(), t.num_buckets()))
    }

    /// Whether any shard holds at least its trigger, 55/64 of its buckets at or past its ceiling:
    /// the fallback's THRESHOLD condition, decided at block boundaries (CH2 §2.3, §2.5, rev4). A
    /// shard below its ceiling holds at most 7/16 of the ceiling's buckets, so it never does.
    pub fn at_shard_trigger(&self) -> bool {
        self.shards
            .iter()
            .enumerate()
            .any(|(i, t)| at_trigger(t.len(), t.num_buckets().max(self.sizes.buckets(i))))
    }

    pub fn rebuilds(&self) -> Rebuilds {
        self.rebuilds
    }

    /// Shard `i`'s room: the inserts into new buckets it takes before it must rebuild or double.
    #[cfg(test)]
    pub(crate) fn shard_room(&self, i: usize) -> usize {
        self.shards[i].capacity() - self.shards[i].len()
    }

    /// Shard `i`'s table allocation in bytes.
    #[cfg(all(test, feature = "alloc-tag"))]
    pub(crate) fn shard_bytes(&self, i: usize) -> u64 {
        footprint(&self.shards[i]) as u64
    }

    /// Whether every shard's table comes from the heap ([`Self::child`]).
    #[cfg(test)]
    pub(crate) fn heap_tables(&self) -> bool {
        self.shards.iter().all(|t| t.allocator().is_heap())
    }

    /// Each shard's entries.
    #[cfg(test)]
    pub(crate) fn shard_lens(&self) -> Vec<usize> {
        self.shard_loads().map(|(n, _)| n).collect()
    }

    /// The boundary rebuild (CH2 §2.3, DEC-108): rebuilds at its size every shard whose entries and
    /// DELETED buckets have reached its trigger while its entries have not, so each shard below its
    /// trigger has room for over 1/64 of its buckets when the next block starts. The cache runs it
    /// at block boundaries with no F in flight. Returns the number of shards rebuilt.
    pub fn boundary_rebuild(&mut self) -> usize {
        let _tag = alloc_tag::coins();
        let mut n = 0;
        for (i, t) in self.shards.iter_mut().enumerate() {
            if needs_boundary_rebuild(t, self.sizes.buckets(i)) {
                rebuild(t, &self.hasher, &mut self.rebuilds);
                n += 1;
            }
        }
        n
    }

    /// Removes `k`'s entry, or turns it into a tombstone with flags `tombstone` (which must hold
    /// [`Flags::SPENT`]), and returns the entry's flags and coin (`None` for a tombstone). A long
    /// coin's box moves out without a copy, still counted by the allocator as coins bytes: the
    /// caller keeps it in the ledger (a block overlay's `spend`, CH2 §2.5 "Ownership").
    pub fn take(
        &mut self,
        k: &OutPointKey,
        tombstone: Option<Flags>,
    ) -> Option<(Flags, Option<PackedCoin>)> {
        // A tombstone's place on the free list can grow it.
        let _tag = alloc_tag::coins();
        let (h, i) = self.locate(k);
        let Ok(entry) = self.shards[i].find_entry(h, |s| s.key == *k) else {
            return None;
        };
        let old = *entry.get();
        let coin = if old.flags().is_spent() {
            None
        } else if old.len == LEN_LONG {
            Some(PackedCoin::from_long(self.long.take(LongRef::read(&old.coin))))
        } else {
            Some(PackedCoin::from_trusted(&old.coin[..usize::from(old.len)]))
        };
        match tombstone {
            None => {
                entry.remove();
                self.live -= 1;
            }
            Some(flags) => {
                debug_assert!(flags.is_spent());
                entry.into_mut().set(flags, &[], &mut self.long);
                if flags.is_dirty() {
                    self.dirty += 1;
                    self.dirty_bytes += dirty_bytes_of(0);
                }
            }
        }
        self.unaccount(&old);
        Some((old.flags(), coin))
    }

    /// [`CoinLayerOps::upsert`] with the coin by value (`None` for a tombstone): a long coin's box
    /// moves in without a copy, as a child entry moves into active at the merge (CH2 §2.5).
    pub(crate) fn upsert_owned(&mut self, k: &OutPointKey, flags: Flags, coin: Option<PackedCoin>) {
        match coin.map(PackedCoin::into_long) {
            None => self.upsert(k, flags, &[]),
            Some(Ok(bytes)) => {
                self.put_slot(k, flags, bytes.len(), |s, long| s.set_long(flags, bytes, long));
            }
            Some(Err(coin)) => self.upsert(k, flags, coin.as_bytes()),
        }
    }

    /// Takes the layer's DIRTY entries, in [`CoinLayerOps::iter_dirty`]'s order, with their coins
    /// (`None` for a tombstone): long coins' boxes move out without a copy. The tables keep their
    /// buckets until [`Self::reset`]: a block overlay's child drains into active at the merge and
    /// is reset for the next block rather than dropped (CH2c D36).
    pub(crate) fn drain_dirty(
        &mut self,
    ) -> impl Iterator<Item = (OutPointKey, Flags, Option<PackedCoin>)> + '_ {
        let Self {
            shards,
            long,
            ..
        } = self;
        shards.iter_mut().flat_map(HashTable::drain).filter(|s| s.flags().is_dirty()).map(|s| {
            let coin = if s.flags().is_spent() {
                None
            } else if s.len == LEN_LONG {
                // Taking a box can grow the free list.
                let _tag = alloc_tag::coins();
                Some(PackedCoin::from_long(long.take(LongRef::read(&s.coin))))
            } else {
                Some(PackedCoin::from_trusted(&s.coin[..usize::from(s.len)]))
            };
            (s.key, s.flags(), coin)
        })
    }

    /// Empties the layer for reuse: no entries and no long coins, the counts zeroed, and each
    /// shard's table kept, emptied, up to [`KEEP_BUCKETS`] (or its start, if larger), else back at
    /// [`ShardSizes::start_buckets`]. Its routing and salt are kept, so nothing is computed again.
    pub(crate) fn reset(&mut self) {
        let _tag = alloc_tag::coins();
        for (i, t) in self.shards.iter_mut().enumerate() {
            let start = self.sizes.start_buckets(i);
            if t.num_buckets() <= start.max(KEEP_BUCKETS) {
                t.clear();
            } else {
                *t = table(start, *t.allocator());
            }
        }
        self.long = LongCoins::default();
        (self.live, self.dirty, self.dirty_bytes) = (0, 0, 0);
        self.rebuilds = Rebuilds::default();
        // Kept tables differ in size: every shard is counted.
        self.tables = self.shards.iter().map(|t| footprint(t) as u64).sum();
        self.unbacked = (0..SHARDS).map(|i| self.unbacked_of(i, footprint(&self.shards[i]))).sum();
    }

    /// The DIRTY entries from `at` on, by reference, in [`CoinLayerOps::iter_dirty`]'s order, each
    /// with the position after it: a split flush resumes its sub-batches there (CH2 §4.2 step 8).
    /// Resuming skips the entries before `at` in its shard only. A flush sorts each sub-batch's
    /// references by key before it puts them (CH2c D37); [`Self::dirty_entry`] reads one.
    pub(crate) fn iter_dirty_refs_from(
        &self,
        at: LayerPos,
    ) -> impl Iterator<Item = (LayerPos, DirtyRef<'_>)> + '_ {
        self.shards
            .iter()
            .enumerate()
            .skip(at.shard)
            .flat_map(move |(shard, t)| {
                let skip = if shard == at.shard {
                    at.slot
                } else {
                    0
                };
                t.iter().enumerate().skip(skip).map(move |(slot, s)| {
                    let next = LayerPos {
                        shard,
                        slot: slot + 1,
                    };
                    (next, s)
                })
            })
            .filter(|(_, s)| s.flags().is_dirty())
            .map(|(next, s)| (next, DirtyRef(s)))
    }

    /// The flags and coin bytes (empty for a tombstone) of an entry of this layer.
    pub(crate) fn dirty_entry<'a>(&'a self, r: DirtyRef<'a>) -> (Flags, &'a [u8]) {
        (r.0.flags(), r.0.coin(&self.long))
    }

    /// What the budget charges beyond [`CoinLayerOps::allocated_bytes`] (CH2c D29): each shard's
    /// ceiling ([`ShardSizes::table_bytes`]'s share) less its table, where that is more. With SSE2
    /// and 4 KiB pages it is zero once every shard reached its ceiling.
    pub fn unbacked_bytes(&self) -> u64 {
        debug_assert_eq!((self.tables, self.unbacked), self.walk_tables());
        self.unbacked
    }

    /// Σ [`footprint`] and Σ unbacked ceiling bytes, from the shards themselves.
    fn walk_tables(&self) -> (u64, u64) {
        let (mut tables, mut unbacked) = (0, 0);
        for (i, t) in self.shards.iter().enumerate() {
            tables += footprint(t) as u64;
            unbacked += self.unbacked_of(i, footprint(t));
        }
        (tables, unbacked)
    }

    /// Shard `i`'s ceiling bytes beyond `footprint`.
    fn unbacked_of(&self, i: usize, footprint: usize) -> u64 {
        ceiling_bytes(self.sizes.buckets(i)).saturating_sub(footprint) as u64
    }

    /// [`CoinLayerOps::allocated_bytes`] with each shard charged at least its ceiling: what the
    /// budget's freeze rules read, so they fire where a presized layer's would.
    pub fn charged_bytes(&self) -> u64 {
        self.allocated_bytes() + self.unbacked_bytes()
    }

    /// The bytes of the shard tables, mapped ones in whole pages, the shard array included:
    /// `sizes().table_bytes()` or less until a shard grows past its ceiling.
    pub fn table_bytes(&self) -> u64 {
        debug_assert_eq!((self.tables, self.unbacked), self.walk_tables());
        size_of::<[Table; SHARDS]>() as u64 + self.tables
    }

    /// Inserts or overwrites `k`'s entry, `set` writing its flags and coin of `coin_len` bytes.
    fn put_slot(
        &mut self,
        k: &OutPointKey,
        flags: Flags,
        coin_len: usize,
        set: impl FnOnce(&mut Slot, &mut LongCoins),
    ) {
        let _tag = alloc_tag::coins();
        let (h, i) = self.locate(k);
        if let Some(s) = self.shards[i].find_mut(h, |s| s.key == *k) {
            let old = *s;
            old.free_long(&mut self.long);
            set(s, &mut self.long);
            self.unaccount(&old);
        } else {
            let mut s = Slot {
                key: *k,
                ..Slot::EMPTY
            };
            set(&mut s, &mut self.long);
            // Only a shard with no room left rebuilds or resizes.
            let t = &mut self.shards[i];
            if t.len() == t.capacity() {
                self.insert_without_room(i, h, s);
            } else {
                let hasher = &self.hasher;
                t.insert_unique(h, s, |s| hasher.hash(&s.key));
            }
            self.live += 1;
        }
        if flags.is_dirty() {
            self.dirty += 1;
            self.dirty_bytes += dirty_bytes_of(coin_len);
        }
    }

    /// Inserts `s` into shard `i`, which has no room left, rebuilding or resizing it as the
    /// module's rules say. Out of line (CH2c D47): it is rare, and kept inline its counts stayed
    /// live across every insert.
    #[cold]
    #[inline(never)]
    fn insert_without_room(&mut self, i: usize, h: u64, s: Slot) {
        let hasher = &self.hasher;
        let t = &mut self.shards[i];
        if needs_rebuild(t, self.sizes.buckets(i)) {
            rebuild(t, hasher, &mut self.rebuilds);
        }
        let old = footprint(t);
        // Below the ceiling, or at or above the trigger, this doubles the shard, unless the probe
        // reuses a DELETED bucket or hashbrown rehashes in place. After a rebuild there is room,
        // so it does not.
        t.insert_unique(h, s, |s| hasher.hash(&s.key));
        debug_assert!(t.num_buckets() <= 1 << ROUTE_SHIFT, "bucket bits below the routing bits");
        self.resized(i, old);
    }

    fn locate(&self, k: &OutPointKey) -> (u64, usize) {
        let h = self.hasher.hash(k);
        (h, self.sizes.shard_of(h))
    }

    /// Moves the table counts to shard `i`'s footprint, `old` bytes before an insert that may
    /// have resized it. Only an insert with no room left resizes a shard; a rebuild keeps its size.
    fn resized(&mut self, i: usize, old: usize) {
        let new = footprint(&self.shards[i]);
        self.tables = self.tables - old as u64 + new as u64;
        self.unbacked = self.unbacked - self.unbacked_of(i, old) + self.unbacked_of(i, new);
    }

    /// Takes an overwritten or removed entry out of the dirty counts.
    fn unaccount(&mut self, old: &Slot) {
        if old.flags().is_dirty() {
            self.dirty -= 1;
            self.dirty_bytes -= dirty_bytes_of(old.coin_len());
        }
    }
}

/// The allocator for a layer's tables: never mapping if `heap` ([`ShardedLayer::child`]).
fn pages(heap: bool) -> Pages {
    if heap {
        Pages::heap()
    } else {
        Pages::current()
    }
}

/// An empty table of exactly `buckets` buckets (a power of two, at least 8): hashbrown's capacity
/// for `2^j` buckets is 7/8 of them, and that capacity maps back to exactly `2^j` buckets.
fn table(buckets: usize, pages: Pages) -> Table {
    HashTable::with_capacity_in(buckets / 8 * 7, pages)
}

/// The bytes table `t` holds, mapped ones in whole pages.
fn footprint(t: &Table) -> usize {
    t.allocator().footprint(t.allocation_size())
}

/// The bytes a shard table of `buckets` buckets is charged: what it allocates, at most (exactly
/// with SSE2), in whole pages from 64 KiB up ([`pages::charged`]).
const fn ceiling_bytes(buckets: usize) -> usize {
    pages::charged(buckets * BUCKET_BYTES + TRAILING_CTRL)
}

/// Whether `len` entries reach the trigger of a shard of `buckets` buckets: 55/64 of them.
fn at_trigger(len: usize, buckets: usize) -> bool {
    64 * len >= 55 * buckets
}

/// Whether an insert into `t`, at or past its ceiling of `ceiling` buckets, would find its room
/// used up by removed entries: no room left, and fewer entries than its trigger. Below its ceiling
/// a shard grows as any hash table does.
fn needs_rebuild(t: &Table, ceiling: usize) -> bool {
    t.num_buckets() >= ceiling && t.len() == t.capacity() && !at_trigger(t.len(), t.num_buckets())
}

/// Whether `t`'s entries and DELETED buckets have reached its trigger while its entries have not:
/// removes have used up the room a rebuild left (DEC-108). hashbrown's `capacity` is its entries
/// plus its room, so the DELETED buckets are its full capacity (7/8 of the buckets) less that.
/// Only at or past its ceiling of `ceiling` buckets, as [`needs_rebuild`].
fn needs_boundary_rebuild(t: &Table, ceiling: usize) -> bool {
    let used = t.num_buckets() / 8 * 7 - (t.capacity() - t.len());
    t.num_buckets() >= ceiling
        && !at_trigger(t.len(), t.num_buckets())
        && at_trigger(used, t.num_buckets())
}

/// Rebuilds `t` at its size, dropping the buckets removed entries left.
fn rebuild(t: &mut Table, hasher: &SaltedHasher, stats: &mut Rebuilds) {
    stats.count += 1;
    stats.entries += t.len() as u64;
    let fresh = table(t.num_buckets(), *t.allocator());
    for s in std::mem::replace(t, fresh) {
        t.insert_unique(hasher.hash(&s.key), s, |s| hasher.hash(&s.key));
    }
}

impl CoinLayerOps for ShardedLayer {
    fn get(&self, k: &OutPointKey) -> Option<SlotRef<'_>> {
        let (h, i) = self.locate(k);
        let s = self.shards[i].find(h, |s| s.key == *k)?;
        Some(SlotRef {
            flags: s.flags(),
            coin: s.coin(&self.long),
        })
    }

    fn upsert(&mut self, k: &OutPointKey, flags: Flags, coin: &[u8]) {
        self.put_slot(k, flags, coin.len(), |s, long| s.set(flags, coin, long));
    }

    fn remove(&mut self, k: &OutPointKey) -> bool {
        // Freeing a long coin can grow the free list.
        let _tag = alloc_tag::coins();
        let (h, i) = self.locate(k);
        let Ok(entry) = self.shards[i].find_entry(h, |s| s.key == *k) else {
            return false;
        };
        let (old, _) = entry.remove();
        old.free_long(&mut self.long);
        self.unaccount(&old);
        self.live -= 1;
        true
    }

    fn iter(&self) -> impl Iterator<Item = (OutPointKey, SlotRef<'_>)> + '_ {
        self.shards.iter().flat_map(|t| t.iter()).map(|s| {
            (
                s.key,
                SlotRef {
                    flags: s.flags(),
                    coin: s.coin(&self.long),
                },
            )
        })
    }

    fn allocated_bytes(&self) -> u64 {
        self.table_bytes() + self.long.allocated()
    }

    fn len(&self) -> usize {
        self.live as usize
    }

    fn dirty_len(&self) -> usize {
        self.dirty as usize
    }

    fn dirty_bytes(&self) -> u64 {
        self.dirty_bytes
    }
}
