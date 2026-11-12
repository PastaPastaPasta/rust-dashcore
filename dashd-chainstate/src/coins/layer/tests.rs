//! The layer against a `BTreeMap` model, its memory accounting against the tagging allocator
//! (with feature `alloc-tag`, as the gate's feature-tests line runs it), and the presized shards'
//! invariants: the decomposition and routing, the shard trigger, no reallocation below the presize
//! or under churn below the trigger, and growth past it one shard at a time.

use std::collections::BTreeMap;

use super::sharded::{BUCKET_BYTES, SHARDS, ShardSizes, TABLE_OVERHEAD};
use super::*;
use crate::coins::Salt;
use crate::coins::hasher::SaltedHasher;

/// SplitMix64: a deterministic stream for the tests.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        crate::coins::hasher::splitmix64(&mut self.0)
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub(crate) fn key(i: u64) -> OutPointKey {
    let mut r = Rng::new(i);
    let mut k = [0u8; 36];
    for w in 0..4 {
        k[w * 8..w * 8 + 8].copy_from_slice(&r.next().to_le_bytes());
    }
    k[32..].copy_from_slice(&((i % 7) as u32).to_le_bytes());
    OutPointKey(k)
}

/// Coin-shaped bytes of `len` (the layers never decode them).
fn coin(len: usize, fill: u8) -> Vec<u8> {
    (0..len).map(|i| fill.wrapping_add(i as u8)).collect()
}

/// A random entry: mostly inline lengths, some long, a few near the largest coin.
fn random_entry(r: &mut Rng) -> (Flags, Vec<u8>) {
    let flags = Flags(r.below(4) as u8);
    if r.below(10) == 0 {
        return (Flags::DIRTY | Flags::SPENT, Vec::new());
    }
    let len = match r.below(100) {
        0 => 9_000 + r.below(1_000) as usize,
        1..=4 => 35 + r.below(200) as usize,
        _ => 3 + r.below(32) as usize,
    };
    (flags, coin(len, r.next() as u8))
}

/// The tagging allocator's view: a fresh domain for the calling thread, and the live coins bytes
/// counted in one. Without feature `alloc-tag` nothing is counted, and the tests check the layer
/// alone.
mod tag {
    #[cfg(feature = "alloc-tag")]
    use crate::alloc_tag::{self, Tag};

    pub(super) fn domain() -> u8 {
        #[cfg(feature = "alloc-tag")]
        {
            let d = alloc_tag::new_domain();
            alloc_tag::set_thread_domain(d);
            d
        }
        #[cfg(not(feature = "alloc-tag"))]
        0
    }

    /// `None` without the tagging allocator.
    pub(super) fn bytes(d: u8) -> Option<u64> {
        #[cfg(feature = "alloc-tag")]
        return Some(alloc_tag::tagged_bytes(d, Tag::Coins) as u64);
        #[cfg(not(feature = "alloc-tag"))]
        {
            let _ = d;
            None
        }
    }
}

/// The tagged bytes of domain `d` equal `bytes` (with feature `alloc-tag`).
fn assert_tagged(d: u8, bytes: u64, what: impl std::fmt::Display) {
    if let Some(tagged) = tag::bytes(d) {
        assert_eq!(tagged, bytes, "{what}");
    }
}

/// The tagged bytes of domain `d` equal the layer's `allocated_bytes()`.
fn assert_accounted(d: u8, layer: &ShardedLayer, what: impl std::fmt::Display) {
    assert_tagged(d, layer.allocated_bytes(), what);
}

/// Random upserts and removals against the model. After every op: each touched key reads as the
/// model says, the counts and dirty bytes agree, and the tagged bytes equal `allocated_bytes()`.
fn model_check(presize: u64, seed: u64, ops: usize, keys: u64) {
    let d = tag::domain();
    let mut layer = ShardedLayer::new(presize, Salt::fixed(1));
    let mut model: BTreeMap<OutPointKey, (Flags, Vec<u8>)> = BTreeMap::new();
    let mut r = Rng::new(seed);
    for op in 0..ops {
        let k = key(r.below(keys));
        if r.below(4) == 0 {
            assert_eq!(layer.remove(&k), model.remove(&k).is_some());
        } else {
            let (flags, bytes) = random_entry(&mut r);
            layer.upsert(&k, flags, &bytes);
            model.insert(k, (flags, bytes));
        }
        assert_accounted(d, &layer, format_args!("op {op}"));
        let got = layer.get(&k).map(|e| (e.flags, e.coin.to_vec()));
        assert_eq!(got, model.get(&k).cloned(), "op {op}");
        assert_eq!(layer.len(), model.len());
        if op % 512 == 0 || op + 1 == ops {
            let dirty: Vec<_> = model.iter().filter(|(_, (f, _))| f.is_dirty()).collect();
            assert_eq!(layer.dirty_len(), dirty.len());
            let bytes: u64 = dirty.iter().map(|(_, (_, c))| 36 + c.len() as u64).sum();
            assert_eq!(layer.dirty_bytes(), bytes);
            let mut it: Vec<_> = layer.iter_dirty().map(|(k, f, c)| (k, (f, c.to_vec()))).collect();
            it.sort_by_key(|(k, _)| *k);
            let want: Vec<_> = dirty.iter().map(|(k, v)| (**k, (*v).clone())).collect();
            assert_eq!(it, want);
            assert_eq!(layer.iter().count(), model.len());
            for (k, v) in &model {
                let e = layer.get(k).unwrap();
                assert_eq!((e.flags, e.coin), (v.0, &v.1[..]));
            }
        }
    }
    drop(layer);
    assert_tagged(d, 0, "everything freed");
}

#[test]
fn the_layer_matches_the_model() {
    model_check(16 << 20, 2, 40_000, 6_000);
}

/// A layer presized far below what the ops need grows through many shard doublings and stays
/// exact.
#[test]
fn the_layer_matches_the_model_while_growing() {
    model_check(0, 3, 40_000, 20_000);
}

/// About 60 % of a 256 KiB layer's buckets live (3/4 of the keys), with a quarter of the ops
/// removes: shards rebuild at their size, those that reach their trigger double, and both stay
/// exact.
#[test]
fn the_layer_matches_the_model_under_churn() {
    model_check(256 << 10, 4, 60_000, 2_800);
}

/// The decomposition is the largest `2^k (64 + s)` buckets whose tables fit, charged whole pages
/// from 64 KiB up: within `2^k` of the target, less a page a shard.
#[test]
fn shard_sizes_decompose_the_presize() {
    let mut r = Rng::new(5);
    for _ in 0..10_000 {
        let bytes = TABLE_OVERHEAD + 64 * 8 * BUCKET_BYTES as u64 + r.below(16 << 30);
        let z = ShardSizes::for_bytes(bytes);
        let target = (bytes - TABLE_OVERHEAD) / BUCKET_BYTES as u64;
        assert!(z.s < SHARDS as u32);
        assert!(z.total_buckets() <= target);
        assert!(z.table_bytes() <= bytes);
        let next = match z.s {
            63 => ShardSizes {
                k: z.k + 1,
                s: 0,
            },
            s => ShardSizes {
                k: z.k,
                s: s + 1,
            },
        };
        assert!(next.table_bytes() > bytes, "{bytes}: {z:?}");
        let pages = SHARDS as u64 * super::pages::PAGE as u64 / BUCKET_BYTES as u64;
        assert!(target - z.total_buckets() < (1 << z.k) + pages, "{bytes}: {z:?}");
        let sum: u64 = (0..SHARDS).map(|i| z.buckets(i) as u64).sum();
        assert_eq!(sum, z.total_buckets());
    }
    // A presize too small for 8 buckets a shard gets 8 anyway.
    assert_eq!(ShardSizes::for_bytes(1).total_buckets(), 512);
}

/// Keys land in shards in proportion to their ceilings' buckets, whatever their current size.
#[test]
fn routing_is_proportional_to_bucket_counts() {
    let mut l = ShardedLayer::new(55 << 20, Salt::fixed(4));
    let z = l.sizes();
    assert!(z.s > 0 && z.s < 64, "the test wants both shard sizes: {z:?}");
    let n = 200_000u64;
    for i in 0..n {
        l.upsert(&key(i), Flags::DIRTY, &coin(20, 0));
    }
    for (i, (count, buckets)) in l.shard_loads().enumerate() {
        assert!(buckets <= z.buckets(i), "no shard grew past its ceiling");
        let expect = n as f64 * z.buckets(i) as f64 / z.total_buckets() as f64;
        let sd = expect.sqrt();
        assert!((count as f64 - expect).abs() < 5.0 * sd, "shard {i}: {count} vs {expect:.0}");
    }
}

/// The shard comes from the hash's bits 25..=56 only (the routing requirement, CH2 r6 m8): flipping
/// any other bit keeps it, and the keys of one shard spread over all 128 control tags (the top seven
/// bits) as evenly as any keys do.
#[test]
fn routing_uses_bits_hashbrown_does_not() {
    let z = ShardSizes::for_bytes(55 << 20);
    let mut r = Rng::new(12);
    for _ in 0..10_000 {
        let h = r.next();
        for bit in (0..25).chain(57..64) {
            assert_eq!(z.shard_of(h ^ 1 << bit), z.shard_of(h), "bit {bit}");
        }
    }
    let hasher = SaltedHasher::new(Salt::fixed(13));
    let mut tags = [0u32; 128];
    let mut n = 0;
    for i in 0..2_000_000 {
        let h = hasher.hash(&key(i));
        if z.shard_of(h) == 0 {
            tags[(h >> 57) as usize] += 1;
            n += 1;
        }
    }
    let expect = f64::from(n) / 128.0;
    assert!(expect > 200.0, "{n} keys in shard 0");
    for (tag, &c) in tags.iter().enumerate() {
        assert!(
            (f64::from(c) - expect).abs() < 5.0 * expect.sqrt(),
            "tag {tag}: {c} vs {expect:.0}"
        );
    }
}

/// Filling a presized layer reallocates nothing until a shard holds 7/8 of its presized buckets,
/// which happens with about 7/8 of all of them in use, and the shard trigger fires before that.
/// Past that, each growth step doubles one shard, and churn reuses the room.
#[test]
fn a_presized_layer_fills_its_presize_then_grows_a_shard_at_a_time() {
    let d = tag::domain();
    // 3.4 MiB: 48 shards of 2^10 buckets and 16 of 2^9.
    let want = ShardSizes {
        k: 9,
        s: 48,
    };
    let presize = want.table_bytes();
    let mut l = ShardedLayer::presized(presize, Salt::fixed(6));
    assert_eq!(l.sizes(), want);
    let tables = l.allocated_bytes();
    assert!(tables <= l.sizes().table_bytes() && tables <= presize);
    assert_accounted(d, &l, "presized");
    let total = l.sizes().total_buckets();
    let mut i = 0;
    let mut triggered = None;
    while l.allocated_bytes() == tables {
        if triggered.is_none() && l.at_shard_trigger() {
            triggered = Some(i);
        }
        l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
        i += 1;
    }
    assert!(triggered.is_some_and(|t| t < i - 1), "the trigger fired before the first doubling");
    // The insert that grew the layer found its shard at 7/8 of its presize; no other shard is past.
    let grown: Vec<_> =
        l.shard_loads().enumerate().filter(|&(s, (_, b))| b != l.sizes().buckets(s)).collect();
    assert_eq!(grown.len(), 1, "{grown:?}");
    let (s, (n, b)) = grown[0];
    assert_eq!((n, b), (l.sizes().buckets(s) / 8 * 7 + 1, 2 * l.sizes().buckets(s)));
    assert!(
        l.shard_loads().enumerate().all(|(t, (n, _))| t == s || n <= l.sizes().buckets(t) / 8 * 7)
    );
    // About 7/8 of the buckets are in use when the first shard fills (Poisson noise aside).
    assert!(i * 8 > total * 7 * 9 / 10, "{i} of {total}");
    assert_accounted(d, &l, "after the first doubling");
    // Fill to three times the presized load: every step is one shard's doubling.
    let buckets = |l: &ShardedLayer| l.shard_loads().map(|(_, b)| b).collect::<Vec<_>>();
    let mut last = buckets(&l);
    while i < total * 7 / 8 * 3 {
        l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
        i += 1;
        let now = buckets(&l);
        if now != last {
            let changed: Vec<_> = (0..SHARDS).filter(|&t| now[t] != last[t]).collect();
            assert!(changed.len() == 1 && now[changed[0]] == 2 * last[changed[0]], "{changed:?}");
            last = now;
        }
    }
    assert!(
        l.shard_loads().enumerate().all(|(t, (_, b))| b > l.sizes().buckets(t)),
        "every shard grew"
    );
    assert_eq!(l.len() as u64, i);
    assert!((0..i).step_by(97).all(|j| l.get(&key(j)).is_some()));
    assert_accounted(d, &l, "at three times the presize");
    // Churn: removing and inserting as many keys reuses the room.
    let grown = l.allocated_bytes();
    for j in 0..1000 {
        assert!(l.remove(&key(j)));
        l.upsert(&key(10_000_000 + j), Flags::DIRTY, &coin(25, 0));
    }
    assert_eq!(l.allocated_bytes(), grown);
}

/// Steady churn below the shard trigger never grows a layer: the buckets removed entries leave
/// are reclaimed by rebuilding a shard at its size. (Without the rebuild, hashbrown doubled every
/// shard of this layer at 60 % load.)
#[test]
fn churn_below_the_trigger_never_grows_the_layer() {
    for (load, seed) in [(40, 9), (60, 10)] {
        let d = tag::domain();
        let mut l = ShardedLayer::presized(4 << 20, Salt::fixed(seed));
        let presized = l.allocated_bytes();
        let total = l.sizes().total_buckets();
        let live = total * load / 100;
        let mut keys: Vec<u64> = (0..live).collect();
        for &i in &keys {
            l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, i as u8));
        }
        // Remove a random live key and insert a new one, ten times the bucket count.
        let mut r = Rng::new(seed);
        for next in live..live + 10 * total {
            let j = r.below(live) as usize;
            assert!(l.remove(&key(keys[j])));
            keys[j] = next;
            l.upsert(&key(next), Flags::DIRTY | Flags::FRESH, &coin(25, next as u8));
        }
        assert_eq!(l.allocated_bytes(), presized, "{load} %: no shard grew");
        assert!(!l.at_shard_trigger());
        assert!(l.rebuilds().count > 0, "{load} %: the churn used up the room");
        assert_eq!(l.len() as u64, live);
        assert!(keys.iter().all(|&i| l.get(&key(i)).is_some_and(|e| e.coin == coin(25, i as u8))));
        assert_accounted(d, &l, format_args!("{load} %"));
    }
}

/// One shard of 128 buckets, kept one entry below its trigger of 110 (55/64), is rebuilt whenever
/// churn uses up its room and never doubles; at its trigger the same churn doubles it once its
/// room is used up, with no rebuild first. Each rebuild moves the shard's entries.
#[test]
fn a_shard_rebuilds_below_its_trigger_and_doubles_at_it() {
    let salt = Salt::fixed(11);
    let mut l = ShardedLayer::new(TABLE_OVERHEAD + 64 * 128 * BUCKET_BYTES as u64, salt);
    assert_eq!(
        l.sizes(),
        ShardSizes {
            k: 7,
            s: 0
        }
    );
    let (sizes, hasher) = (l.sizes(), SaltedHasher::new(salt));
    let mut shard0 = (0..).filter(move |&i| sizes.shard_of(hasher.hash(&key(i))) == 0);
    let mut live: Vec<u64> = shard0.by_ref().take(109).collect();
    for &i in &live {
        l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
    }
    let shard = |l: &ShardedLayer| l.shard_loads().next().unwrap();
    assert_eq!(shard(&l), (109, 128));
    assert!(!l.at_shard_trigger());
    // Replaces the shard's entries `ops` (in turn, oldest first): inserts the new one, then removes
    // the old.
    fn churn(
        l: &mut ShardedLayer,
        live: &mut [u64],
        keys: &mut impl Iterator<Item = u64>,
        ops: std::ops::Range<usize>,
    ) {
        for j in ops {
            let slot = &mut live[j % live.len()];
            let new = keys.next().unwrap();
            l.upsert(&key(new), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
            assert!(l.remove(&key(std::mem::replace(slot, new))));
        }
    }
    churn(&mut l, &mut live, &mut shard0, 0..1_000);
    assert_eq!(shard(&l), (109, 128), "no doubling below the trigger");
    // The shard starts with room for 112 − 109 = 3 inserts, and a rebuild at 109 entries leaves it
    // the same. A remove that no probe could have passed gives its room back, so 1,000 inserts and
    // removes rebuild at most 333 times (at the 4th op, the 7th, …, the 1,000th).
    let below = l.rebuilds();
    assert!((1..=333).contains(&below.count), "{below:?}");
    assert_eq!(below.entries, below.count * 109);
    live.push(shard0.next().unwrap());
    // This insert, at 109 entries, rebuilds if the churn left no room.
    l.upsert(&key(*live.last().unwrap()), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
    assert!(l.at_shard_trigger());
    let at = l.rebuilds();
    assert_eq!(at.entries, at.count * 109);
    for j in 0..1_000 {
        churn(&mut l, &mut live, &mut shard0, j..j + 1);
        if shard(&l).1 > 128 {
            break;
        }
    }
    assert_eq!(shard(&l), (110, 256), "at its trigger the shard doubles");
    assert_eq!(l.rebuilds(), at, "with no rebuild first");
    assert!(live.iter().all(|&i| l.get(&key(i)).is_some()));
}

/// The boundary rebuild (CH2 §2.3, DEC-108). One 128-bucket shard (trigger 110, growth load 112)
/// is churned at 109 entries until removes leave it room for one insert. With no rebuild at the
/// boundary, a block of a few inserts doubles it; after the boundary rebuild, which restores its
/// room to 3 (over 1/64 of its buckets), the same block does not.
#[test]
fn the_boundary_rebuild_restores_the_trigger_margin() {
    let salt = Salt::fixed(12);
    let presize = TABLE_OVERHEAD + 64 * 128 * BUCKET_BYTES as u64;
    // The churned layer and the keys of shard 0 not yet used.
    let churned = || {
        let mut l = ShardedLayer::new(presize, salt);
        let (sizes, hasher) = (l.sizes(), SaltedHasher::new(salt));
        let mut shard0 = (0..).filter(move |&i| sizes.shard_of(hasher.hash(&key(i))) == 0);
        let mut live: std::collections::VecDeque<u64> = shard0.by_ref().take(109).collect();
        for &i in &live {
            l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
        }
        // Insert a new key, then remove the oldest, until one insert's room is left.
        for _ in 0..10_000 {
            if l.shard_room(0) == 1 {
                assert_eq!(l.shard_loads().next().unwrap(), (109, 128));
                return (l, shard0);
            }
            let new = shard0.next().unwrap();
            l.upsert(&key(new), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
            assert!(l.remove(&key(live.pop_front().unwrap())));
            live.push_back(new);
        }
        panic!("the churn never left one insert's room");
    };
    let shard = |l: &ShardedLayer| l.shard_loads().next().unwrap();

    // With no boundary rebuild: insert until the shard doubles.
    let (mut l, mut keys) = churned();
    assert!(!l.at_shard_trigger());
    let mut block = Vec::new();
    while shard(&l).1 == 128 {
        assert!(block.len() < 4, "four inserts and no doubling");
        let i = keys.next().unwrap();
        l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
        block.push(i);
    }
    assert!(block.len() <= 3, "the block doubled the shard in {} inserts", block.len());

    let (mut l, _) = churned();
    let before = l.rebuilds().count;
    assert_eq!(l.boundary_rebuild(), 1);
    assert_eq!((l.rebuilds().count, l.shard_room(0)), (before + 1, 3));
    assert_eq!(l.boundary_rebuild(), 0, "a rebuilt shard is left alone");
    for &i in &block {
        l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
    }
    assert_eq!(shard(&l), (109 + block.len(), 128), "after the boundary rebuild it does not");
}

/// The DEC-95 growth witnesses at the layer: one block that needs far more than the layer's presize
/// completes, and every entry reads back.
///
/// The presize is CH2 §2.3's `f · (B − R − B/16)` for `-dbcache=4`'s coins share with txindex off,
/// as CH2 §7.1 a2 sets the witnesses: B = 1.75 MiB (`node/caches.cpp:15-42`), and `R = S + 64 KiB`
/// with `S = B/8` at the default `-dbbatchsize`, so (1,835,008 − 294,912 − 114,688) / 2 = 712,704
/// bytes. (With txindex on, Dash's default, B is 1,605,632 B; a smaller presize only grows more.)
const DBCACHE4_PRESIZE: u64 = 712_704;

/// A block with 50,000 outputs, inserted with no boundary between them.
#[test]
fn a_50000_output_block_grows_a_dbcache_4_layer() {
    let d = tag::domain();
    let mut l = ShardedLayer::new(DBCACHE4_PRESIZE, Salt::fixed(7));
    for i in 0..50_000 {
        l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, i as u8));
    }
    assert_eq!(l.len(), 50_000);
    assert!(l.table_bytes() > l.sizes().table_bytes(), "it grew past its presize");
    assert!((0..50_000).all(|i| l.get(&key(i)).is_some_and(|e| e.coin == coin(25, i as u8))));
    assert_eq!(l.dirty_len(), 50_000);
    assert_accounted(d, &l, "after the block");
}

/// A disconnect restoring long coins: 1,000 coins of 9,583 bytes (a 9,577-byte script, GPT's
/// long-input witness, scaled down), well past the presize in long-coin boxes alone.
#[test]
fn a_long_coin_restoration_grows_a_dbcache_4_layer() {
    let d = tag::domain();
    let mut l = ShardedLayer::new(DBCACHE4_PRESIZE, Salt::fixed(8));
    let presized = l.allocated_bytes();
    let long = |i: u64| coin(9_583, i as u8);
    for i in 0..1_000 {
        l.upsert(&key(i), Flags::DIRTY, &long(i));
    }
    assert!(l.allocated_bytes() > presized + 1_000 * 9_583);
    assert!((0..1_000).all(|i| l.get(&key(i)).is_some_and(|e| e.coin == long(i))));
    assert_eq!(l.dirty_bytes(), 1_000 * (36 + 9_583));
    assert_accounted(d, &l, "after the block");
}

#[test]
fn tombstones_and_long_coins_round_trip() {
    let mut l = ShardedLayer::new(1 << 20, Salt::fixed(1));
    let long = coin(10_015, 3);
    l.upsert(&key(1), Flags::DIRTY | Flags::FRESH, &long);
    l.upsert(&key(2), Flags::DIRTY | Flags::SPENT, &[]);
    l.upsert(&key(3), Flags::NONE, &coin(34, 1));
    l.upsert(&key(4), Flags::NONE, &coin(35, 1));
    assert_eq!(l.get(&key(1)).unwrap().coin, &long[..]);
    assert_eq!(
        l.get(&key(2)).unwrap(),
        SlotRef {
            flags: Flags::DIRTY | Flags::SPENT,
            coin: &[]
        }
    );
    assert_eq!(l.get(&key(3)).unwrap().coin.len(), 34);
    assert_eq!(l.get(&key(4)).unwrap().coin, &coin(35, 1)[..]);
    // A long coin replaced by an inline one, and back.
    l.upsert(&key(1), Flags::DIRTY, &coin(10, 9));
    assert_eq!(l.get(&key(1)).unwrap().coin, &coin(10, 9)[..]);
    l.upsert(&key(3), Flags::DIRTY, &long);
    assert_eq!(l.get(&key(3)).unwrap().coin, &long[..]);
    assert_eq!(l.dirty_len(), 3);
    assert_eq!(l.dirty_bytes(), 36 * 3 + 10 + long.len() as u64);
}

/// A split flush resumes the DIRTY entries where its last sub-batch ended: resuming at every
/// returned position yields `iter_dirty`'s order exactly, across shards and empty ones.
#[test]
fn iter_dirty_resumes_at_any_position() {
    let mut l = ShardedLayer::new(1 << 16, Salt::fixed(1));
    let mut r = Rng::new(7);
    for i in 0..3_000 {
        let (flags, bytes) = random_entry(&mut r);
        l.upsert(&key(i), flags, &bytes);
    }
    let all: Vec<_> = l.iter_dirty().map(|(k, f, c)| (k, f, c.to_vec())).collect();
    assert!(all.len() > 1_000);
    let from = |at| {
        let entry = |(_, r)| {
            let (f, c) = l.dirty_entry(r);
            (*r.key(), f, c.to_vec())
        };
        l.iter_dirty_refs_from(at).map(entry).collect::<Vec<_>>()
    };
    assert_eq!(from(LayerPos::default()), all);
    for (i, (at, ..)) in l.iter_dirty_refs_from(LayerPos::default()).enumerate() {
        if i % 97 == 0 {
            assert_eq!(from(at), all[i + 1..], "resumed after entry {i}");
        }
    }
    let (end, ..) = l.iter_dirty_refs_from(LayerPos::default()).last().unwrap();
    assert!(from(end).is_empty());
}

/// §6.7's load band: once the layer has held its presize's capacity, every shard's load is at most
/// 7/8 (hashbrown's capacity), and while blocks only add, at least 7/16 (a shard doubles when its
/// 7/8 is used up). Under churn only the upper bound is checked: removes lower a doubled shard's
/// load and no table shrinks (600 adds and 400 removes a block reach 0.41; §6.7 as DEC-131 narrows
/// it).
#[test]
fn after_warm_up_shard_loads_stay_in_the_band() {
    for removes in [0, 400] {
        let presize = TABLE_OVERHEAD + 64 * 256 * BUCKET_BYTES as u64;
        let mut l = ShardedLayer::new(presize, Salt::fixed(3));
        let mut r = Rng::new(5);
        let (mut live, mut next, mut warm) = (Vec::new(), 0, false);
        for block in 0..600 {
            for _ in 0..600 {
                l.upsert(&key(next), Flags::DIRTY | Flags::FRESH, &coin(25, 0));
                live.push(next);
                next += 1;
            }
            for _ in 0..removes {
                let i = r.below(live.len() as u64) as usize;
                assert!(l.remove(&key(live.swap_remove(i))));
            }
            l.boundary_rebuild();
            warm |= l.len() >= 64 * 256 / 8 * 7;
            for (n, b) in l.shard_loads().filter(|_| warm) {
                assert!(8 * n <= 7 * b, "block {block}: {n} entries in {b} buckets");
                assert!(
                    removes > 0 || 16 * n >= 7 * b,
                    "block {block}: {n} entries in {b} buckets"
                );
            }
        }
        assert!(warm && l.shard_loads().all(|(_, b)| b >= 1024), "every shard doubled twice");
    }
}

/// A 16 MiB ceiling: 46 shards of 2^12 buckets and 18 of 2^11, starting at 1/8 of them.
const LAZY: ShardSizes = ShardSizes {
    k: 11,
    s: 46,
};
const LAZY_PRESIZE: u64 = LAZY.table_bytes();

/// The lazy layer and a presized one hold the same entries in the same shards, with the same
/// shard trigger and the same charged bytes (so the cache freezes and flushes them at the same
/// points), while the lazy one's tables are never larger (CH2c D29).
fn assert_matches_presized(lazy: &ShardedLayer, presized: &ShardedLayer, what: &str) {
    assert_eq!(lazy.sizes(), presized.sizes(), "{what}");
    assert_eq!(lazy.shard_lens(), presized.shard_lens(), "{what}: the same keys in each shard");
    assert_eq!(lazy.at_shard_trigger(), presized.at_shard_trigger(), "{what}");
    assert_eq!(lazy.charged_bytes(), presized.charged_bytes(), "{what}");
    assert_eq!(
        (lazy.dirty_len(), lazy.dirty_bytes()),
        (presized.dirty_len(), presized.dirty_bytes())
    );
    assert!(lazy.table_bytes() <= presized.table_bytes(), "{what}");
    for (i, ((_, a), (_, b))) in lazy.shard_loads().zip(presized.shard_loads()).enumerate() {
        assert!(
            a <= b && (a == b || b == lazy.sizes().buckets(i)),
            "{what}: shard {i}: {a} vs {b}"
        );
    }
}

/// Everything a flush or a read sees: the entries, as sets.
fn contents(l: &ShardedLayer) -> BTreeMap<OutPointKey, (Flags, Vec<u8>)> {
    l.iter().map(|(k, e)| (k, (e.flags, e.coin.to_vec()))).collect()
}

/// Filling a lazy layer from its start to three times its ceiling's load: after every insert it
/// matches the presized layer, and its shards reach their ceilings on the way.
#[test]
fn a_lazy_layer_grows_to_its_ceiling_as_a_presized_one_fills() {
    let d = tag::domain();
    let salt = Salt::fixed(21);
    let mut lazy = ShardedLayer::new(LAZY_PRESIZE, salt);
    let mut presized = ShardedLayer::presized(LAZY_PRESIZE, salt);
    let z = lazy.sizes();
    assert_eq!(z, LAZY);
    assert!(lazy.shard_loads().enumerate().all(|(i, (_, b))| b == z.buckets(i) / 8));
    assert!(lazy.table_bytes() * 7 < presized.table_bytes());
    let both = |a: &ShardedLayer, b: &ShardedLayer| a.allocated_bytes() + b.allocated_bytes();
    assert_tagged(d, both(&lazy, &presized), "new");
    let total = z.total_buckets();
    let mut trigger = None;
    for i in 0..total * 7 / 8 * 3 {
        let coin = if i % 101 == 0 {
            coin(9_000, i as u8)
        } else {
            coin(25, i as u8)
        };
        lazy.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin);
        presized.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin);
        if i % 997 == 0 || trigger.is_none() && presized.at_shard_trigger() {
            assert_matches_presized(&lazy, &presized, &format!("insert {i}"));
            assert_tagged(d, both(&lazy, &presized), format_args!("insert {i}"));
        }
        if trigger.is_none() && lazy.at_shard_trigger() {
            trigger = Some(i);
        }
    }
    assert!(trigger.is_some());
    assert_matches_presized(&lazy, &presized, "full");
    assert_eq!(lazy.unbacked_bytes(), 0, "every shard reached its ceiling");
    assert_eq!(contents(&lazy), contents(&presized));
    assert_eq!(lazy.rebuilds(), Rebuilds::default());
}

/// Churn on a lazy layer below the trigger: the entries, shards, trigger and charged bytes stay
/// the presized layer's, and no shard below its ceiling is ever rebuilt, inside an op or at a
/// boundary (it grows, or hashbrown rehashes it in place, as any table).
#[test]
fn churn_on_a_lazy_layer_matches_the_presized_one() {
    let salt = Salt::fixed(22);
    let mut lazy = ShardedLayer::new(LAZY_PRESIZE, salt);
    let mut presized = ShardedLayer::presized(LAZY_PRESIZE, salt);
    let mut r = Rng::new(22);
    let mut live: Vec<u64> = Vec::new();
    let mut next = 0;
    // Grow to 30 % of the ceiling's buckets while churning, then churn there.
    let target = lazy.sizes().total_buckets() * 3 / 10;
    for op in 0..400_000u64 {
        if (live.len() as u64) < target && r.below(3) != 0 || live.is_empty() {
            let (flags, bytes) = random_entry(&mut r);
            lazy.upsert(&key(next), flags, &bytes);
            presized.upsert(&key(next), flags, &bytes);
            live.push(next);
            next += 1;
        } else {
            let k = key(live.swap_remove(r.below(live.len() as u64) as usize));
            assert!(lazy.remove(&k) && presized.remove(&k));
        }
        if op % 1_000 == 0 {
            // A block boundary: the lazy layer rebuilds no shard below its ceiling.
            presized.boundary_rebuild();
            assert_eq!(lazy.boundary_rebuild(), 0, "op {op}");
        }
        if op % 4_999 == 0 {
            assert_matches_presized(&lazy, &presized, &format!("op {op}"));
        }
    }
    assert_matches_presized(&lazy, &presized, "end");
    assert_eq!(contents(&lazy), contents(&presized));
    assert!(!lazy.at_shard_trigger());
    let below = lazy.shard_loads().enumerate().any(|(i, (_, b))| b < lazy.sizes().buckets(i));
    assert!(below, "the test wants shards below their ceilings");
    assert_eq!(lazy.rebuilds().count, 0, "no rebuild below the ceiling");
}

/// A shard below its ceiling never reaches the shard trigger: filled to its growth load (7/8 of
/// its current buckets, past 55/64 of them) it does not trigger, and the insert after doubles it.
#[test]
fn below_its_ceiling_a_shard_never_triggers() {
    let salt = Salt::fixed(23);
    let mut l = ShardedLayer::new(LAZY_PRESIZE, salt);
    let (sizes, hasher) = (l.sizes(), SaltedHasher::new(salt));
    let ceiling = sizes.buckets(0);
    let mut shard0 = (0..).filter(move |&i| sizes.shard_of(hasher.hash(&key(i))) == 0);
    let mut doublings = 0;
    loop {
        let (n, b) = l.shard_loads().next().unwrap();
        if b >= ceiling {
            break;
        }
        if n == b / 8 * 7 {
            assert!(64 * n >= 55 * b, "past the current size's trigger");
            assert!(!l.at_shard_trigger(), "{n} entries in {b} buckets of {ceiling}");
            doublings += 1;
        }
        l.upsert(&key(shard0.next().unwrap()), Flags::DIRTY, &coin(25, 0));
    }
    assert_eq!(doublings, 3, "from 1/8 of the ceiling");
    while !l.at_shard_trigger() {
        l.upsert(&key(shard0.next().unwrap()), Flags::DIRTY, &coin(25, 0));
    }
    assert_eq!(l.shard_loads().next().unwrap(), (ceiling * 55 / 64, ceiling));
}

/// A shard starts at 2^8 buckets, its ceiling if that is smaller, and never below 1/8 of its
/// ceiling: at most three doublings to reach it (DEC-388's CPU budget).
#[test]
fn a_shard_starts_at_no_less_than_an_eighth_of_its_ceiling() {
    for (k, shift) in [(3, 0), (8, 0), (9, 1), (10, 2), (11, 3), (14, 3), (20, 3)] {
        let sizes = ShardSizes {
            k,
            s: 32,
        };
        for i in [0, 63] {
            assert_eq!(sizes.start_buckets(i), sizes.buckets(i) >> shift, "k {k}, shard {i}");
        }
    }
}

/// The pages of a new layer are its start, not its ceiling, and a dropped layer gives back its
/// mapped tables (CH2c D29, DEC-388). RssAnon is measured with the test [`alone`].
#[cfg(all(target_os = "linux", not(miri)))]
#[test]
fn a_lazy_layer_is_resident_at_its_entries_not_its_ceiling() {
    if !alone("coins::layer::tests::a_lazy_layer_is_resident_at_its_entries_not_its_ceiling") {
        return;
    }
    let rss = || -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status.lines().find(|l| l.starts_with("RssAnon:")).unwrap();
        line.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() << 10
    };
    // DEC-43's ceiling in the proxy: 96 × 2^14 buckets.
    let presize = ShardSizes {
        k: 14,
        s: 32,
    }
    .table_bytes();
    let coins: Vec<_> = (0..60_000u64).map(|i| (key(i), coin(25, i as u8))).collect();
    let before = rss();
    let mut l = ShardedLayer::new(presize, Salt::fixed(24));
    for (k, c) in &coins {
        l.upsert(k, Flags::DIRTY | Flags::FRESH, c);
    }
    let grown = rss().saturating_sub(before);
    let tables = l.table_bytes();
    drop(l);
    let kept = rss().saturating_sub(before);
    println!(
        "rss: ceiling {presize} B, tables {tables} B, resident {grown} B, after the drop {kept} B"
    );
    // The shards start at 1/8 of their ceilings, and 60k entries touch every page of that.
    assert!(grown < presize * 3 / 16, "resident {grown} B of a {presize} B ceiling");
    assert!(kept < grown / 2, "the drop kept {kept} B of {grown} B");
}

/// Whether this is the child process that runs test `name` alone. The parent runs the child, with
/// the other tests' allocations out of its address space, checks that it passed, and answers false.
#[cfg(all(target_os = "linux", not(miri)))]
pub(super) fn alone(name: &str) -> bool {
    const CHILD: &str = "CH2C_ALONE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && text.contains("1 passed"), "{text}");
    for line in text.lines().filter(|l| l.contains("rss:")) {
        println!("{line}");
    }
    false
}

/// A block overlay's child keeps its tables on the heap through growth and resets (CH2c D47).
#[test]
fn a_child_layer_never_maps_its_tables() {
    let mut l = ShardedLayer::child(Salt::fixed(5));
    assert!(l.heap_tables());
    for round in 0..2 {
        // Enough entries that some shards grow past a mapped table's size.
        for i in 0..80_000 {
            l.upsert(&key(i), Flags::DIRTY | Flags::FRESH, &coin(25, round));
        }
        assert!(l.heap_tables(), "round {round} grown");
        l.reset();
        assert!(l.heap_tables(), "round {round} reset");
    }
}
