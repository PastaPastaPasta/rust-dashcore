//! The cold cache (SB5 §5.2): read-through, CLOCK replacement, invalidation and its property
//! test against the store.

use proptest::prelude::*;

use super::test_store::{RowStore, record};
use super::*;
use crate::sync::Ordering;

/// A watermark that refuses no record.
static ALL: AtomicU32 = AtomicU32::new(u32::MAX);

fn resident(c: &ColdCache, ks: impl IntoIterator<Item = u32>) -> Vec<bool> {
    ks.into_iter().map(|k| c.peek(BlockId(k)).is_some()).collect()
}

/// A miss reads the store once and inserts; the hit then reads nothing.
#[test]
fn read_through_once() {
    let s = RowStore::with([record(1, 0)]);
    let c = ColdCache::new(4);
    assert_eq!(c.get_or_load(BlockId(1), &s, &ALL).unwrap().as_deref(), Some(&record(1, 0)));
    assert_eq!(c.get_or_load(BlockId(1), &s, &ALL).unwrap().as_deref(), Some(&record(1, 0)));
    assert_eq!(s.reads.load(Ordering::SeqCst), 1);
    assert_eq!(c.get_or_load(BlockId(2), &s, &ALL).unwrap(), None, "absent rows are not cached");
    assert_eq!(c.len(), 1);
}

/// A store error is returned and caches nothing (§5.4: the caller trips the latch).
#[test]
fn store_error_inserts_nothing() {
    let s = RowStore::with([record(1, 0)]);
    s.fail.store(true, Ordering::SeqCst);
    let c = ColdCache::new(4);
    assert!(c.get_or_load(BlockId(1), &s, &ALL).is_err());
    assert!(c.load_by_hash(&record(1, 0).hash, &s, &ALL).is_err());
    assert_eq!(c.len(), 0);
}

/// A record at or above the issued-id watermark is corruption, by id and by hash, and is never
/// cached (DEC-187 amendment 3); one below it is.
#[test]
fn refuses_a_record_not_issued() {
    let s = RowStore::with([record(1, 0), record(2, 0)]);
    let (c, issued) = (ColdCache::new(4), AtomicU32::new(2));
    for read in
        [c.get_or_load(BlockId(2), &s, &issued), c.load_by_hash(&record(2, 0).hash, &s, &issued)]
    {
        assert!(read.is_err_and(|e| e.is_corrupt() && e.what().contains("never issued")));
    }
    assert_eq!(c.len(), 0);
    assert!(c.get_or_load(BlockId(1), &s, &issued).unwrap().is_some());
    assert_eq!(c.len(), 1);
}

/// Second chance: the hand clears a referenced frame's bit and evicts the next unreferenced one.
#[test]
fn clock_second_chance() {
    let s = RowStore::with((1..=4).map(|k| record(k, 0)));
    let c = ColdCache::new(3);
    for k in 1..=3 {
        c.get_or_load(BlockId(k), &s, &ALL).unwrap();
    }
    c.get_or_load(BlockId(1), &s, &ALL).unwrap();
    c.get_or_load(BlockId(4), &s, &ALL).unwrap();
    assert_eq!(
        resident(&c, 1..=4),
        [true, false, true, true],
        "2 is the victim, 1 kept its chance"
    );
}

/// The hand moves past each victim, so misses with no hits evict oldest first; a hit then keeps
/// its frame through one sweep.
#[test]
fn clock_hand_advances() {
    let s = RowStore::with((1..=7).map(|k| record(k, 0)));
    let c = ColdCache::new(3);
    for k in 1..=6 {
        c.get_or_load(BlockId(k), &s, &ALL).unwrap();
    }
    assert_eq!(resident(&c, 1..=6), [false, false, false, true, true, true]);
    c.get_or_load(BlockId(4), &s, &ALL).unwrap();
    c.get_or_load(BlockId(7), &s, &ALL).unwrap();
    assert_eq!(resident(&c, 4..=7), [true, false, true, true], "4 kept its chance, 5 went");
}

/// R1-m3's worst case: every bit set, the hand clears all `cap` and evicts where it started.
#[test]
fn clock_all_referenced_sweep() {
    let s = RowStore::with((1..=4).map(|k| record(k, 0)));
    let c = ColdCache::new(3);
    for k in [1, 2, 3, 1, 2, 3] {
        c.get_or_load(BlockId(k), &s, &ALL).unwrap();
    }
    c.get_or_load(BlockId(4), &s, &ALL).unwrap();
    assert_eq!(resident(&c, 1..=4), [false, true, true, true]);
    assert_eq!(c.len(), 3);
}

/// `invalidate` and `clear` drop the stale records; the next read sees the committed row.
#[test]
fn invalidate_then_reload() {
    let s = RowStore::with([record(1, 0), record(2, 0)]);
    let c = ColdCache::new(4);
    c.get_or_load(BlockId(1), &s, &ALL).unwrap();
    c.get_or_load(BlockId(2), &s, &ALL).unwrap();
    s.commit(record(1, 7));
    c.invalidate(&[BlockId(1), BlockId(9)]);
    assert_eq!(resident(&c, 1..=2), [false, true]);
    assert_eq!(c.get_or_load(BlockId(1), &s, &ALL).unwrap().as_deref(), Some(&record(1, 7)));
    s.commit(record(2, 7));
    c.clear();
    assert_eq!(c.len(), 0);
    assert_eq!(c.get_or_load(BlockId(2), &s, &ALL).unwrap().as_deref(), Some(&record(2, 7)));
}

/// `load_by_hash` caches the record by its id; then reads by id and by hash both hit (D43),
/// until the record is evicted or invalidated.
#[test]
fn by_hash_inserts_by_id() {
    let s = RowStore::with([record(5, 0), record(6, 0)]);
    let c = ColdCache::new(1);
    let h = record(5, 0).hash;
    assert_eq!(c.load_by_hash(&h, &s, &ALL).unwrap().as_deref(), Some(&record(5, 0)));
    assert!(c.peek(BlockId(5)).is_some());
    c.get_or_load(BlockId(5), &s, &ALL).unwrap();
    assert_eq!(c.load_by_hash(&h, &s, &ALL).unwrap().as_deref(), Some(&record(5, 0)));
    assert_eq!(s.reads.load(Ordering::SeqCst), 1, "the id and hash reads hit");
    c.invalidate(&[BlockId(5)]);
    c.load_by_hash(&h, &s, &ALL).unwrap();
    assert_eq!(s.reads.load(Ordering::SeqCst), 2, "invalidated: read again");
    c.get_or_load(BlockId(6), &s, &ALL).unwrap();
    c.load_by_hash(&h, &s, &ALL).unwrap();
    assert_eq!(s.reads.load(Ordering::SeqCst), 4, "evicted by 6: read again");
    assert_eq!(c.len(), 1);
}

#[derive(Clone, Debug)]
enum Op {
    Load(u32),
    LoadHash(u32),
    /// A committed status change, invalidated after the commit (SB5-A5).
    Commit(u32, u32),
    Clear,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..12u32).prop_map(Op::Load),
        1 => (0..12u32).prop_map(Op::LoadHash),
        2 => (0..12u32, 0..4u32).prop_map(|(k, s)| Op::Commit(k, s)),
        1 => Just(Op::Clear),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Every read answers the store's committed row (invalidation follows each commit), a hit
    /// reads nothing, a found record is resident after its load, and the cache never holds more
    /// than `cap` records.
    #[test]
    fn cold_cache_model(cap in 1u32..6, ops in proptest::collection::vec(op(), 1..200)) {
        let s = RowStore::with((0..10).map(|k| record(k, 0)));
        let c = ColdCache::new(cap);
        for op in ops {
            match op {
                Op::Load(k) => {
                    let cached = c.peek(BlockId(k)).is_some();
                    let before = s.reads.load(Ordering::SeqCst);
                    let got = c.get_or_load(BlockId(k), &s, &ALL).unwrap();
                    prop_assert_eq!(s.reads.load(Ordering::SeqCst) == before, cached);
                    let want = s.by_id(BlockId(k)).unwrap();
                    prop_assert_eq!(got.as_deref(), want.as_ref());
                    prop_assert_eq!(c.peek(BlockId(k)).is_some(), want.is_some(), "a load is resident");
                }
                Op::LoadHash(k) => {
                    let cached = c.peek(BlockId(k)).is_some();
                    let before = s.reads.load(Ordering::SeqCst);
                    let got = c.load_by_hash(&record(k, 0).hash, &s, &ALL).unwrap();
                    prop_assert_eq!(s.reads.load(Ordering::SeqCst) == before, cached);
                    let want = s.by_id(BlockId(k)).unwrap();
                    prop_assert_eq!(got.as_deref(), want.as_ref());
                    prop_assert_eq!(c.peek(BlockId(k)).is_some(), want.is_some(), "a load is resident");
                }
                Op::Commit(k, status) => {
                    s.commit(record(k, status));
                    c.invalidate(&[BlockId(k)]);
                    prop_assert!(c.peek(BlockId(k)).is_none());
                }
                Op::Clear => c.clear(),
            }
            prop_assert!(c.len() <= cap as usize);
        }
    }
}

/// `IdHasher` reads a `BlockId` as its one `u32`, and distinct ids spread over hashbrown's control
/// bits (the top seven) and its low index bits, consecutive ones and ones at a power-of-two stride
/// (D63b: unrotated, ids at strides 576 and 1,024 reach 32 and 2 of 2,048 low values).
#[test]
fn the_id_hasher_spreads_ids() {
    use std::collections::HashSet;
    use std::hash::BuildHasher;
    let b = BuildHasherDefault::<IdHasher>::default();
    assert_eq!(b.hash_one(BlockId(5)), 5u64.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(26));
    for stride in [1, 576, 1_024] {
        let hashes: Vec<u64> = (0..1_024).map(|i| b.hash_one(BlockId(i * stride))).collect();
        let top: HashSet<u64> = hashes.iter().map(|h| h >> 57).collect();
        let low: HashSet<u64> = hashes.iter().map(|h| h & 2_047).collect();
        assert!(
            top.len() == 128 && low.len() > 400,
            "stride {stride}: {} {}",
            top.len(),
            low.len()
        );
    }
}
