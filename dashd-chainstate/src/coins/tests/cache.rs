//! The tip's own rules (CH2 §2.4, §7.1 a2): cache membership (§2.4.5), the prefetch fence
//! (§2.4.3) and its producer's read (CH4-1), freeze and release, `boundary_state` and the boundary
//! rebuild (§2.3, §2.5, DEC-108), and the planted bugs X10, X13, X32, X34, X35 and X36.

use dash_consensus::primitives::TxOut;
#[cfg(feature = "planted-bugs")]
use dashd_store::ErrorKind;
use dashd_store::KvEngine as _;

use super::*;
use crate::coins::layer::ShardSizes;
use crate::coins::model::expected_boundary;
use crate::coins::prefetch::read_from;
use crate::coins::{
    BlockOverlay, CoinSrc, CoinsBoundary, CoinsCacheSizeState, PrefetchBatch, PrefetchRequest,
    Resized, core_size_arith, read_batch,
};
#[cfg(feature = "planted-bugs")]
use crate::store::AbortMark;

fn src(coin: &PackedCoin) -> CoinSrc<'_> {
    CoinSrc::Bytes(coin.as_ref())
}

/// A coin whose script is `len` bytes of `OP_TRUE`: long past [`crate::coins::INLINE_MAX`].
fn long_coin(len: usize) -> PackedCoin {
    let out = TxOut {
        value: 5,
        script_pubkey: vec![0x51; len].into(),
    };
    PackedCoin::from_output(&out, 7, false).unwrap()
}

/// Connects a block through an overlay: its outputs added, best block set, committed.
fn connect(cache: &mut CoinsCache<ProbeEngine>, height: u32, outs: &[(OutPointKey, PackedCoin)]) {
    let mut overlay = BlockOverlay::new(cache);
    for (k, coin) in outs {
        overlay.add(k, src(coin), false).unwrap();
    }
    overlay.commit(block(height)).unwrap();
}

/// `have_in_cache`, asserting it read neither the database nor the prefetch map (§2.4.5).
fn member(w: &World, k: &OutPointKey) -> bool {
    let before = (w.db_reads(), w.cache.prefetch_lookups());
    let m = w.cache.have_in_cache(k);
    assert_eq!((w.db_reads(), w.cache.prefetch_lookups()), before, "have_in_cache read below F");
    m
}

/// `mempool_accept.py`'s case: a block's outputs are members (in active), still with F in flight
/// after an own-cause freeze, and not after the release. Core, with no flush in between, still
/// has them: ATMP answers `txn-already-known` there and `bad-txns-inputs-missingorspent` here.
/// That is D12, asserted as such.
#[test]
fn a_blocks_outputs_are_members_until_the_release_d12() {
    let mut w = World::with_dbcache(16);
    let mut r = Rng::new(11);
    let outs: Vec<_> = (0..3).map(|i| (random_key(&mut r, i), p2pkh(i.into()))).collect();
    connect(&mut w.cache, 1, &outs);
    assert!(outs.iter().all(|(k, _)| member(&w, k)));
    let frozen = w.cache.freeze_at(1, 0);
    assert!(w.cache.active().is_empty());
    assert!(outs.iter().all(|(k, _)| member(&w, k)), "F is still searched");
    w.release(frozen);
    // D12: Core's cache would still hold them.
    assert!(outs.iter().all(|(k, _)| !member(&w, k)));
    assert!(outs.iter().all(|(k, c)| w.db_get(k).as_ref() == Some(c)), "the coins are unspent");
}

/// A fetched coin is a member until `uncache` drops its clean entry; a DIRTY entry and an F entry
/// stay.
#[test]
fn uncache_drops_a_clean_active_entry_only() {
    let mut w = World::with_dbcache(16);
    let mut r = Rng::new(12);
    let [fetched, added, frozen] = [0, 1, 2].map(|i| (random_key(&mut r, i), p2pkh(i.into())));
    w.db_set(&fetched.0, Some(&fetched.1));
    assert!(!member(&w, &fetched.0));
    assert!(w.cache.fetch(&fetched.0).unwrap().is_some());
    assert!(member(&w, &fetched.0));
    w.cache.uncache(&fetched.0);
    assert!(!member(&w, &fetched.0));
    w.cache.add(&frozen.0, src(&frozen.1), false).unwrap();
    let f = w.cache.freeze_at(1, 0);
    w.cache.add(&added.0, src(&added.1), false).unwrap();
    for k in [&added.0, &frozen.0] {
        w.cache.uncache(k);
        assert!(member(&w, k));
    }
    // A clean active copy of an F entry is not made: F hits are not copied (§2.4.1).
    assert!(w.cache.fetch(&frozen.0).unwrap().is_some());
    assert!(w.cache.active().get(&frozen.0).is_none());
    drop(f);
}

/// An active tombstone hides F's unspent entry.
#[test]
fn an_active_tombstone_over_f_is_not_a_member() {
    let mut w = World::with_dbcache(16);
    let k = random_key(&mut Rng::new(13), 0);
    w.cache.add(&k, src(&p2pkh(1)), false).unwrap();
    let f = w.cache.freeze_at(1, 0);
    assert!(member(&w, &k));
    assert!(w.cache.spend(&k).unwrap());
    assert!(!member(&w, &k));
    assert!(f.coins().get(&k).is_some_and(|e| !e.flags.is_spent()));
    assert_eq!(w.cache.fetch(&k).unwrap(), None);
}

/// A coin F holds spent is not spent again (Core's flush has erased it, and its `SpendCoin`
/// returns false): no tombstone goes into active, before W writes (the database still holds the
/// coin) or after the release.
#[test]
fn spending_an_f_tombstone_returns_false_and_writes_nothing() {
    let mut w = World::with_dbcache(16);
    let k = random_key(&mut Rng::new(23), 0);
    w.db_set(&k, Some(&p2pkh(1)));
    assert!(w.cache.spend(&k).unwrap());
    let f = w.cache.freeze_at(1, 0);
    assert!(f.coins().get(&k).is_some_and(|e| e.flags.is_spent()));
    assert!(!w.cache.spend(&k).unwrap());
    assert!(w.cache.active().get(&k).is_none(), "no tombstone over F's");
    assert!(w.db_get(&k).is_some(), "W has not written");
    w.release(f);
    assert!(!w.cache.spend(&k).unwrap());
    assert!(w.cache.active().is_empty());
}

/// A coin only the prefetch map or only the database holds is not a member, and no
/// `have_in_cache` call above read either.
#[test]
fn prefetch_only_and_db_only_coins_are_not_members() {
    let mut w = World::with_dbcache(16);
    let mut r = Rng::new(14);
    let [db_only, prefetched] = [0, 1].map(|i| (random_key(&mut r, i), p2pkh(i.into())));
    w.db_set(&db_only.0, Some(&db_only.1));
    w.db_set(&prefetched.0, Some(&prefetched.1));
    let batch = PrefetchBatch {
        fetch_gen: w.cache.released_gen(),
        epoch: w.db.write_epoch(),
        entries: vec![prefetched.clone()],
    };
    assert_eq!(w.cache.admit_prefetch(batch, 0), 1);
    assert!(!member(&w, &db_only.0));
    assert!(!member(&w, &prefetched.0));
    assert_eq!((w.db_reads(), w.cache.prefetch_lookups()), (0, 0));
    // The prefetch hit moves into active, as a database hit is copied.
    assert!(w.cache.fetch(&prefetched.0).unwrap().is_some());
    assert_eq!((w.db_reads(), w.cache.prefetch_lookups()), (0, 1));
    assert!(member(&w, &prefetched.0));
    // It left the map: once uncached, the next fetch reads the database.
    w.cache.uncache(&prefetched.0);
    assert!(w.cache.fetch(&prefetched.0).unwrap().is_some());
    assert_eq!((w.db_reads(), w.cache.prefetch_lookups()), (1, 2));
}

/// `tip_coins` is the database laid over by F, then active: an F tombstone hides the
/// database's coin, an active coin replaces F's tombstone, an active tombstone hides F's coin.
/// It is in Core's `COutPoint` order: index 256 after index 1 of the same txid, where the key's
/// bytes put it first.
#[test]
fn tip_coins_lays_f_then_active_over_the_database() {
    let mut w = World::with_dbcache(16);
    let mut r = Rng::new(31);
    let one = random_key(&mut r, 1);
    let mut many = one;
    many.0[32..].copy_from_slice(&256u32.to_le_bytes());
    let [db_only, f_spent, replaced, a_spent, a_new] =
        [0, 2, 3, 4, 5].map(|i| random_key(&mut r, i));
    for k in [&db_only, &f_spent, &replaced, &one, &many] {
        w.db_set(k, Some(&p2pkh(1)));
    }
    assert!(w.cache.spend(&f_spent).unwrap());
    assert!(w.cache.spend(&replaced).unwrap());
    w.cache.add(&a_spent, src(&p2pkh(3)), false).unwrap();
    let _f = w.cache.freeze_at(1, 0);
    w.cache.add(&replaced, src(&p2pkh(4)), false).unwrap();
    assert!(w.cache.spend(&a_spent).unwrap());
    w.cache.add(&a_new, src(&p2pkh(5)), false).unwrap();
    let mut want = vec![
        (db_only, p2pkh(1)),
        (one, p2pkh(1)),
        (many, p2pkh(1)),
        (replaced, p2pkh(4)),
        (a_new, p2pkh(5)),
    ];
    want.sort_by_key(|(k, _)| (k.0[..32].to_vec(), k.vout()));
    assert_eq!(w.cache.tip_coins().unwrap(), want);
    let at = |k: &OutPointKey| want.iter().position(|(x, _)| x == k).unwrap();
    assert!(at(&one) < at(&many) && many < one, "by the index as a number");
}

/// Spends `k` at the tip, freezes, lets W write and releases: the database no longer has it.
fn spend_and_release(w: &mut World, k: &OutPointKey, height: u32) {
    assert!(w.cache.spend(k).unwrap());
    w.flush(height);
    assert_eq!(w.db_get(k), None);
}

/// A batch read before a release completes after it: the fence drops it, and the coin it carries,
/// spent and flushed since, does not come back.
#[test]
fn a_prefetch_completion_after_release_is_dropped() {
    let mut w = World::with_dbcache(16);
    w.cache.set_xcheck(true);
    let (k, coin) = (random_key(&mut Rng::new(15), 0), p2pkh(3));
    w.db_set(&k, Some(&coin));
    let stale = PrefetchBatch {
        fetch_gen: w.cache.released_gen(),
        epoch: w.db.write_epoch(),
        entries: vec![(k, coin)],
    };
    spend_and_release(&mut w, &k, 1);
    assert_eq!(w.cache.admit_prefetch(stale, 0), 0);
    assert_eq!(w.cache.fetch(&k).unwrap(), None);
    assert!(w.db.latch().tripped().is_none());
}

/// The fence's order (DEC-142, O5): a producer reads the generation, then takes its snapshot. Two
/// producers race W's write of F, which spends `k`. The right order stamps the old generation and
/// is dropped. The other order takes the snapshot first and reads the generation after the release:
/// the generation check would admit `k`'s coin back, and the next fetch would return it. The epoch
/// check refuses it in debug builds; with it skipped (X32, or a release build), the test shows the
/// resurrection.
#[test]
fn the_fence_reads_the_generation_before_the_snapshot() {
    let mut w = World::with_dbcache(16);
    let (k, coin) = (random_key(&mut Rng::new(19), 0), p2pkh(5));
    w.db_set(&k, Some(&coin));
    assert!(w.cache.spend(&k).unwrap());
    let f = w.cache.freeze_at(1, 0);
    let read = |w: &World| vec![(k, w.db_get(&k).expect("W has not written F yet"))];
    // Both producers read before W's write lands; the wrong one reads the generation only after.
    let right = PrefetchBatch {
        fetch_gen: w.cache.released_gen(),
        epoch: w.db.write_epoch(),
        entries: read(&w),
    };
    let (epoch, entries) = (w.db.write_epoch(), read(&w));
    w.release(f);
    assert_eq!(w.db_get(&k), None, "F's spend is in the database");
    let fetch_gen = w.cache.released_gen();
    let wrong = || PrefetchBatch {
        fetch_gen,
        epoch,
        entries: entries.clone(),
    };
    assert_eq!(w.cache.admit_prefetch(right, 0), 0, "the right order's batch is stale");
    assert_eq!(fetch_gen, w.cache.released_gen(), "the generation alone admits it");
    if cfg!(debug_assertions) {
        assert_the_epoch_check_refuses(|| w.cache.admit_prefetch(wrong(), 0));
        #[cfg(feature = "planted-bugs")]
        w.cache.plant(crate::coins::Planted::X32);
    }
    if cfg!(any(not(debug_assertions), feature = "planted-bugs")) {
        assert_eq!(w.cache.admit_prefetch(wrong(), 0), 1);
        let resurrected = w.cache.fetch(&k).unwrap().map(|c| c.to_coin());
        assert_eq!(resurrected, Some(coin.as_ref().to_coin()), "the other order's effect");
    }
}

/// X10 (a stale batch admitted) resurrects the spent coin; xcheck catches it as corruption.
#[cfg(feature = "planted-bugs")]
#[test]
fn x10_is_caught_by_xcheck() {
    for xcheck in [false, true] {
        let mut w = World::with_dbcache(16);
        w.cache.plant(crate::coins::Planted::X10);
        w.cache.set_xcheck(xcheck);
        let (k, coin) = (random_key(&mut Rng::new(16), 0), p2pkh(4));
        w.db_set(&k, Some(&coin));
        let stale = PrefetchBatch {
            fetch_gen: w.cache.released_gen(),
            epoch: w.db.write_epoch(),
            entries: vec![(k, coin.clone())],
        };
        spend_and_release(&mut w, &k, 1);
        assert_eq!(w.cache.admit_prefetch(stale, 0), 1);
        if xcheck {
            let err = w.cache.fetch(&k).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Corruption);
            assert!(w.db.latch().tripped().is_some());
            assert!(
                matches!(&w.sink.marks()[..], [AbortMark::ReadFailed(m)] if m.contains("consensusxcheck"))
            );
        } else {
            let resurrected = w.cache.fetch(&k).unwrap().map(|c| c.to_coin());
            assert_eq!(resurrected, Some(coin.as_ref().to_coin()), "the bug's effect");
        }
    }
}

/// `read_batch` (CH4-1): the request's generation, the write epoch from just before its snapshot,
/// and the coins found in request order; an absent key is left out (item 3), and a write after
/// the snapshot is not seen.
#[test]
fn read_batch_reads_one_snapshot_and_leaves_out_absent_keys() {
    let w = World::with_dbcache(16);
    let mut r = Rng::new(41);
    let [a, absent, b] = [0, 1, 2].map(|i| random_key(&mut r, i));
    w.db_set(&a, Some(&p2pkh(1)));
    w.db_set(&b, Some(&p2pkh(2)));
    let epoch = w.db.write_epoch();
    let req = PrefetchRequest {
        fetch_gen: 7,
        keys: vec![b, absent, a],
    };
    let batch = read_batch(&*w.db, &CoinsTable::default(), req).unwrap();
    assert_eq!((batch.fetch_gen, batch.epoch), (7, epoch));
    assert_eq!(batch.entries, vec![(b, p2pkh(2)), (a, p2pkh(1))]);
    let snap = w.db.snapshot();
    w.db_set(&absent, Some(&p2pkh(3)));
    let req = PrefetchRequest {
        fetch_gen: 7,
        keys: vec![absent],
    };
    let batch = read_from(&snap, &CoinsTable::default(), req).unwrap();
    assert!(batch.entries.is_empty(), "a write after the snapshot");
    assert_eq!(batch.epoch, epoch);
}

/// The map never passes B/16 (CH2 §2.4.3 item 2): a batch larger than the headroom is admitted in
/// part. With X36 planted (the headroom ignored), all of it goes in and the map passes its cap.
#[test]
fn admission_stops_at_the_prefetch_headroom() {
    let run = |setup: &dyn Fn(&mut World)| {
        // -dbcache=1: a cap of 1/16 of the coins share.
        let mut w = World::new(CoinsBudget::new(MIB, 0, Ratio::HALF, 0));
        setup(&mut w);
        let mut r = Rng::new(42);
        let entries: Vec<_> = (0..4_000).map(|i| (random_key(&mut r, i), long_coin(60))).collect();
        let batch = PrefetchBatch {
            fetch_gen: w.cache.released_gen(),
            epoch: w.db.write_epoch(),
            entries,
        };
        let admitted = w.cache.admit_prefetch(batch, 0);
        (admitted, w.cache.usage().prefetch, w.cache.budget().prefetch_cap(0))
    };
    let (admitted, used, cap) = run(&|_| ());
    assert!(admitted > 0 && admitted < 4_000 && used <= cap, "{admitted} {used} {cap}");
    #[cfg(feature = "planted-bugs")]
    {
        let (admitted, used, cap) = run(&|w| w.cache.plant(crate::coins::Planted::X36));
        assert!(admitted == 4_000 && used > cap, "the bug's effect: {admitted} {used} {cap}");
    }
}

/// A batch read before W wrote F, admitted while F is in flight, is in the generation. X35 (the
/// release keeps the map) lets it outlive F: the coin F spent comes back from the map, which
/// xcheck catches as corruption.
#[cfg(feature = "planted-bugs")]
#[test]
fn x35_a_kept_map_brings_back_what_f_spent() {
    for xcheck in [false, true] {
        let mut w = World::with_dbcache(16);
        w.cache.plant(crate::coins::Planted::X35);
        w.cache.set_xcheck(xcheck);
        let (k, coin) = (random_key(&mut Rng::new(43), 0), p2pkh(6));
        w.db_set(&k, Some(&coin));
        let batch = read_batch(&*w.db, &CoinsTable::default(), w.request(vec![k])).unwrap();
        assert!(w.cache.spend(&k).unwrap());
        let f = w.cache.freeze_at(1, 0);
        assert_eq!(w.cache.admit_prefetch(batch, 0), 1, "the generation is current");
        w.release(f);
        assert_eq!(w.db_get(&k), None, "F's spend is in the database");
        if xcheck {
            assert_eq!(w.cache.fetch(&k).unwrap_err().kind(), ErrorKind::Corruption);
        } else {
            let resurrected = w.cache.fetch(&k).unwrap().map(|c| c.to_coin());
            assert_eq!(resurrected, Some(coin.as_ref().to_coin()), "the bug's effect");
        }
    }
}

/// X34 (the generation read at admission, after the batch's snapshot): a batch read before the
/// release that spent `k` is admitted after it as current, and the epoch check refuses it (a
/// planted build keeps debug assertions, `lib.rs`).
#[cfg(feature = "planted-bugs")]
#[test]
fn x34_a_restamped_batch_is_caught_by_the_epoch_check() {
    let mut w = World::with_dbcache(16);
    w.cache.plant(crate::coins::Planted::X34);
    let (k, coin) = (random_key(&mut Rng::new(44), 0), p2pkh(7));
    w.db_set(&k, Some(&coin));
    let batch = read_batch(&*w.db, &CoinsTable::default(), w.request(vec![k])).unwrap();
    spend_and_release(&mut w, &k, 1);
    assert_the_epoch_check_refuses(|| w.cache.admit_prefetch(batch, 0));
}

/// Keys F owns shadow the prefetch map (GPT): one spent in F, one spent and recreated in F, and one
/// spent in active.
#[test]
fn f_owned_spent_and_recreated_keys_shadow_the_prefetch_map() {
    let mut w = World::with_dbcache(16);
    w.cache.set_xcheck(true);
    let mut r = Rng::new(17);
    let [spent_f, recreated_f, spent_active] =
        [0, 1, 2].map(|i| (random_key(&mut r, i), p2pkh(i.into())));
    for (k, c) in [&spent_f, &recreated_f, &spent_active] {
        w.db_set(k, Some(c));
    }
    let batch = PrefetchBatch {
        fetch_gen: w.cache.released_gen(),
        epoch: w.db.write_epoch(),
        entries: vec![spent_f.clone(), recreated_f.clone(), spent_active.clone()],
    };
    let new = p2pkh(9);
    assert!(w.cache.spend(&spent_f.0).unwrap());
    assert!(w.cache.spend(&recreated_f.0).unwrap());
    w.cache.add(&recreated_f.0, src(&new), false).unwrap();
    let f = w.cache.freeze_at(1, 0);
    assert!(w.cache.spend(&spent_active.0).unwrap());
    assert_eq!(w.cache.admit_prefetch(batch, 0), 3, "same generation: admitted");
    let lookups = w.cache.prefetch_lookups();
    assert_eq!(w.cache.fetch(&spent_f.0).unwrap(), None);
    assert_eq!(
        w.cache.fetch(&recreated_f.0).unwrap().map(|c| c.to_coin()),
        Some(new.as_ref().to_coin())
    );
    assert_eq!(w.cache.fetch(&spent_active.0).unwrap(), None);
    assert_eq!(w.cache.prefetch_lookups(), lookups, "every key was answered above the map");
    w.release(f);
    assert_eq!(w.cache.usage().prefetch, 0, "a release clears the map");
    w.flush(2);
    assert_eq!(w.db_get(&spent_f.0), None);
    assert_eq!(w.db_get(&recreated_f.0), Some(new));
    assert_eq!(w.db_get(&spent_active.0), None);
}

/// The order of a flush: freeze, W writes and drops its handle, V releases. The best block reads
/// through each layer: the tip's, else F's, else the database's.
#[test]
fn release_ordering() {
    let mut w = World::with_dbcache(16);
    assert_eq!(w.cache.best_block(), None);
    let k = random_key(&mut Rng::new(18), 0);
    w.cache.add(&k, src(&p2pkh(1)), false).unwrap();
    w.cache.set_best_block(Some(block(1)));
    let f = w.cache.freeze(0);
    assert_eq!(f.tip(), block(1), "the tip's best block (O6)");
    w.cache.set_best_block(None);
    assert_eq!(w.cache.best_block(), Some(block(1)), "F's tip");
    assert_eq!(f.generation(), 0);
    write_layer(&w.db, f.coins());
    drop(f);
    w.cache.release(0);
    assert_eq!((w.cache.released_gen(), w.cache.best_block()), (1, Some(block(1))));
    let f = w.cache.freeze(0);
    assert_eq!((f.generation(), f.tip()), (1, block(1)), "the database's best block");
    drop(f);
    w.cache.release(1);
    let f = w.cache.freeze_at(2, 0);
    drop(f);
    w.cache.set_best_block(None);
    w.cache.release(2);
    assert_eq!(w.cache.best_block(), Some(block(2)), "the database's");
}

/// Core flushes the coins only at a best block (`validation.cpp:3078`), and `CCoinsViewDB`
/// asserts one (`txdb.cpp:119`): a freeze needs one (O6).
#[test]
#[should_panic(expected = "a freeze at a best block")]
fn a_freeze_needs_a_best_block() {
    let mut w = World::with_dbcache(16);
    drop(w.cache.freeze(0));
}

#[test]
#[should_panic(expected = "W still holds F")]
fn a_release_waits_for_w() {
    let mut w = World::with_dbcache(16);
    let _w_handle = w.cache.freeze_at(1, 0);
    w.cache.release(0);
}

#[test]
#[should_panic(expected = "the release of another layer")]
fn a_release_names_its_generation() {
    let mut w = World::with_dbcache(16);
    drop(w.cache.freeze_at(1, 0));
    w.cache.release(1);
}

#[test]
#[should_panic(expected = "a freeze with F still in flight")]
fn one_frozen_layer_at_a_time() {
    let mut w = World::with_dbcache(16);
    drop(w.cache.freeze_at(1, 0));
    drop(w.cache.freeze_at(2, 0));
}

/// X13 (V releases F before W has written it) passes the guard's place unchecked; a lookup then
/// misses a coin only F held.
#[cfg(feature = "planted-bugs")]
#[test]
fn x13_without_the_guard_is_a_lookup_mismatch() {
    let mut w = World::with_dbcache(16);
    w.cache.plant(crate::coins::Planted::X13);
    let (k, coin) = (random_key(&mut Rng::new(19), 0), p2pkh(2));
    w.cache.add(&k, src(&coin), false).unwrap();
    let w_handle = w.cache.freeze_at(1, 0);
    w.cache.release(w_handle.generation());
    assert_eq!(w.cache.fetch(&k).unwrap(), None, "the coin only F held is lost");
    drop(w_handle);
}

/// THRESHOLD at f·B with no F in flight, suppressed by a flush in flight; LARGE and CRITICAL from
/// native Σ against B; CRITICAL before THRESHOLD before LARGE. At B = 1.75 MiB, f = 1/2, with long
/// coins so that bytes, not a shard trigger, cross f·B.
#[test]
fn boundary_state_follows_native_sigma_and_f_b() {
    let mut w = World::new(CoinsBudget::new(B_4, 0, Ratio::HALF, 64 << 10));
    let mut r = Rng::new(20);
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Ok);
    let mut seen = Vec::new();
    let step = |w: &World, seen: &mut Vec<CoinsBoundary>| {
        for in_flight in [false, true] {
            let got = w.cache.boundary_state(0, in_flight);
            assert_eq!(got, expected_boundary(&w.cache, 0, in_flight));
            seen.push(got);
        }
    };
    let threshold = w.cache.budget().threshold(0);
    while w.cache.active().allocated_bytes() < threshold {
        assert!(!w.cache.active().at_shard_trigger());
        step(&w, &mut seen);
        w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
    }
    step(&w, &mut seen);
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Threshold);
    assert_ne!(w.cache.boundary_state(0, true), CoinsBoundary::Threshold);
    // F in flight: no THRESHOLD; Σ grows to LARGE, then past B to CRITICAL.
    let _w_handle = w.cache.freeze_at(1, 0);
    while w.cache.usage().total() <= B_4 {
        step(&w, &mut seen);
        w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
    }
    step(&w, &mut seen);
    for state in
        [CoinsBoundary::Ok, CoinsBoundary::Threshold, CoinsBoundary::Large, CoinsBoundary::Critical]
    {
        assert!(seen.contains(&state), "never {state:?}");
    }
    // With no F in flight: LARGE and THRESHOLD at once report THRESHOLD; CRITICAL comes first.
    let mut w = World::new(CoinsBudget::new(B_4, 0, Ratio::HALF, 0));
    while core_size_arith(w.cache.usage().total(), B_4) == CoinsCacheSizeState::Ok {
        w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
    }
    assert_eq!(w.cache.size_state(0), CoinsCacheSizeState::Large);
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Threshold);
    while w.cache.usage().total() <= B_4 {
        w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
    }
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Critical);
}

/// `boundary_state` at band edges worked out by hand from Core's arithmetic, not from
/// `expected_boundary`: B(t) is 1.75 MiB plus what is left of a 1 MiB mempool allowance, moved by
/// the mempool's usage; f = 1/2, so THRESHOLD is active ≥ ⌊B/2⌋; R = 64 KiB, counted only with a
/// flush in flight; CRITICAL is Σ > B and LARGE Σ > ⌊9B/10⌋ (B is under 100 MiB).
#[test]
fn boundary_state_at_hand_worked_edges() {
    const R: u64 = 64 << 10;
    let mut w = World::new(CoinsBudget::new(B_4, MIB, Ratio::HALF, R));
    let mut r = Rng::new(24);
    // The mempool usage at which B(t) is `b`.
    let at = |b: u64| {
        assert!((B_4..=B_4 + MIB).contains(&b), "B = {b} is out of reach");
        B_4 + MIB - b
    };
    let state = |w: &World, b: u64, in_flight: bool| w.cache.boundary_state(at(b), in_flight);
    // Long coins until Σ reaches `to`: active's bytes and Σ, no other layer, no R.
    let mut fill = |w: &mut World, to: u64| {
        while w.cache.usage().total() < to {
            w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
        }
        assert!(!w.cache.active().at_shard_trigger());
        let u = w.cache.usage();
        assert_eq!((u.frozen, u.prefetch, u.held_out, u.reserve), (0, 0, 0, 0));
        (u.active, u.total())
    };
    // THRESHOLD's edge: active's a against ⌊B/2⌋.
    let (a, sigma) = fill(&mut w, 1_100_000);
    assert_eq!(a, sigma);
    assert_eq!(state(&w, 2 * a, false), CoinsBoundary::Threshold);
    assert_eq!(state(&w, 2 * a + 1, false), CoinsBoundary::Threshold, "⌊(2a + 1)/2⌋ = a");
    assert_eq!(state(&w, 2 * a + 2, false), CoinsBoundary::Ok, "Σ = a, under ⌊9B/10⌋");
    assert_eq!(state(&w, 2 * a, true), CoinsBoundary::Ok, "no THRESHOLD in flight");
    // CRITICAL's and LARGE's edges in flight: s = Σ + R against B and ⌊9B/10⌋.
    let (a, sigma) = fill(&mut w, 1_900_000);
    let s = sigma + R;
    assert_eq!(state(&w, s - 1, true), CoinsBoundary::Critical);
    assert_eq!(state(&w, s, true), CoinsBoundary::Large);
    // The largest B with ⌊9B/10⌋ < s, that is 9B < 10s.
    let large = (10 * s).div_ceil(9) - 1;
    assert_eq!(state(&w, large, true), CoinsBoundary::Large);
    assert_eq!(state(&w, large + 1, true), CoinsBoundary::Ok);
    // No flush in flight: no R, and CRITICAL before THRESHOLD.
    assert!(a >= sigma / 2);
    assert_eq!(state(&w, sigma - 1, false), CoinsBoundary::Critical);
    assert_eq!(state(&w, sigma, false), CoinsBoundary::Threshold);
}

/// BL-90(b): the check reports the Σ and B(t) it compared, R included in flight, so the CRITICAL
/// line cannot drift from the decision.
#[test]
fn the_boundary_check_reports_the_sigma_and_b_it_compared() {
    const R: u64 = 64 << 10;
    let mut w = World::new(CoinsBudget::new(B_4, MIB, Ratio::HALF, R));
    let mut r = Rng::new(25);
    let mut states = Vec::new();
    // Below the coins share, then past it, where a full mempool leaves B(t) under Σ.
    for fill in [1_000_000, B_4 + 1] {
        while w.cache.usage().total() < fill {
            w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
        }
        let sigma = w.cache.usage().total();
        for (mempool, in_flight) in [(0, false), (MIB / 2, false), (0, true), (MIB, true)] {
            let check = w.cache.boundary(mempool, in_flight);
            let reserve = if in_flight {
                R
            } else {
                0
            };
            assert_eq!((check.sigma, check.total), (sigma + reserve, B_4 + MIB - mempool));
            assert_eq!(check.state, expected_boundary(&w.cache, mempool, in_flight));
            // The pair is the one Core's arithmetic found CRITICAL, or not.
            assert_eq!(
                check.state == CoinsBoundary::Critical,
                core_size_arith(check.sigma, check.total) == CoinsCacheSizeState::Critical
            );
            assert_eq!(check.state, w.cache.boundary_state(mempool, in_flight));
            states.push(check.state);
        }
    }
    assert!(states.contains(&CoinsBoundary::Ok));
    assert!(states.contains(&CoinsBoundary::Critical));
    // F in flight counts R whatever the caller says, as `usage` does.
    let _f = w.cache.freeze_at(1, 0);
    assert_eq!(w.cache.boundary(0, false).sigma, w.cache.usage().total());
    assert_eq!(w.cache.usage().reserve, R);
}

/// A shard at its trigger is THRESHOLD before active's bytes reach f·B (the fallback's condition).
#[test]
fn a_shard_at_its_trigger_is_threshold() {
    let mut w = World::new(CoinsBudget::new(B_4, 0, Ratio::HALF, 0));
    let mut r = Rng::new(21);
    let mut n = 0u64;
    while !w.cache.active().at_shard_trigger() {
        assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Ok);
        w.cache.add(&random_key(&mut r, 0), src(&p2pkh(n)), false).unwrap();
        n += 1;
    }
    assert!(w.cache.active().allocated_bytes() < w.cache.budget().threshold(0));
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Threshold);
    assert_eq!(w.cache.boundary_state(0, true), CoinsBoundary::Ok);
}

/// B(t) moves with the mempool between ops: the same layers are OK, THRESHOLD, LARGE or CRITICAL
/// as the allowance shrinks; the prefetch headroom and a freeze's presize follow B(t) at the time.
#[test]
fn a_shrinking_mempool_allowance_moves_every_check() {
    let max_mempool = 2 << 20;
    let mut w = World::new(CoinsBudget::new(B_4, max_mempool, Ratio::HALF, 0));
    let mut r = Rng::new(22);
    while w.cache.active().allocated_bytes() < 1_400_000 {
        w.cache.add(&random_key(&mut r, 0), src(&long_coin(9_000)), false).unwrap();
    }
    let mut states = Vec::new();
    for mp in (0..=6).map(|i| i * max_mempool / 4) {
        let got = w.cache.boundary_state(mp, false);
        assert_eq!(got, expected_boundary(&w.cache, mp, false));
        assert_eq!(
            w.cache.size_state(mp),
            core_size_arith(w.cache.usage().total(), w.cache.budget().total(mp))
        );
        states.push(got);
    }
    assert_eq!(states.first(), Some(&CoinsBoundary::Ok));
    assert!(states.contains(&CoinsBoundary::Threshold));
    // Past the maximum, B(t) is the coins share (saturating).
    assert_eq!(w.cache.budget().total(3 * max_mempool), B_4);
    // The prefetch headroom shrinks: what fits at an empty mempool does not at a full one.
    let entries: Vec<_> = (0..2_000).map(|i| (random_key(&mut r, 1), p2pkh(i))).collect();
    let full = w.cache.admit_prefetch(
        PrefetchBatch {
            fetch_gen: 0,
            epoch: w.db.write_epoch(),
            entries: entries.clone(),
        },
        max_mempool,
    );
    w.cache.clear_prefetch();
    let empty = w.cache.admit_prefetch(
        PrefetchBatch {
            fetch_gen: 0,
            epoch: w.db.write_epoch(),
            entries,
        },
        0,
    );
    assert!(full < empty, "{full} vs {empty}");
    assert!(w.cache.usage().prefetch <= w.cache.budget().prefetch_cap(0));
    // A freeze presizes the next active for B(t) at its time.
    let frozen = w.cache.active().allocated_bytes();
    let f = w.cache.freeze_at(1, max_mempool);
    assert_eq!(
        ShardSizes::for_bytes(w.cache.budget().presize(max_mempool, frozen)),
        w.cache.active().sizes()
    );
    drop(f);
}

/// Sol's divergence case (CH2 §7.1 a2): 20,000 inline P2PKH outputs connected to an empty tip at
/// `-dbcache=4`. `boundary_state` follows native Σ on the kept layout; recorded, not compared with
/// Core's flag.
#[test]
fn sols_divergence_case_is_recorded() {
    let mut w = World::new(CoinsBudget::new(B_4, 0, Ratio::HALF, 0));
    let mut r = Rng::new(23);
    let outs: Vec<_> =
        (0..20_000).map(|i| (random_key(&mut r, (i % 3) as u32), p2pkh(i))).collect();
    connect(&mut w.cache, 1, &outs);
    let state = w.cache.boundary_state(0, false);
    assert_eq!(state, expected_boundary(&w.cache, 0, false));
    let u = w.cache.usage();
    let buckets: usize = w.cache.active().shard_loads().map(|(_, b)| b).sum();
    println!(
        "sol-divergence: B = {B_4}, sigma = {}, active = {}, presized buckets = {}, buckets = \
         {buckets}, boundary = {state:?}",
        u.total(),
        u.active,
        w.cache.active().sizes().total_buckets()
    );
}

/// DEC-108's churn test at 76.99 MiB: V's loop over blocks that create coins, spend most of them
/// soon (removed entries) and some old ones, freezing at THRESHOLD and otherwise running the
/// boundary rebuild. Active starts at 72 % of its buckets, as if it had been running.
///
/// With the rebuild, every shard at its ceiling and below its trigger starts each block with no F
/// in flight with room for more than 1/64 of its buckets (DEC-108's invariant; the blocks put fewer
/// into a shard), and no shard grows past its ceiling during such a block (below it a shard grows
/// as any table does, CH2c D29). Without it, the same blocks start with shards short of that room.
#[test]
fn dec108_no_shard_grows_without_f_in_flight() {
    let (with, without) = std::thread::scope(|s| {
        let with = s.spawn(|| churn(true));
        (with.join().unwrap(), churn(false))
    });
    println!("dec108-churn: with rebuild {with:?}; without {without:?}");
    assert_eq!((with.short_room, with.grew_no_f), (0, 0), "{with:?}");
    assert!(with.freezes > 0 && with.rebuilt > 0, "{with:?}");
    assert!(without.short_room > 0, "the control never ran short: {without:?}");
}

#[derive(Debug, Default)]
struct Churn {
    blocks: u32,
    freezes: u32,
    rebuilt: usize,
    /// Shards at their ceiling and below their trigger that started a block with no F in flight
    /// and room for at most 1/64 of their buckets.
    short_room: u32,
    /// Blocks with no F in flight during which a shard grew past its ceiling.
    grew_no_f: u32,
}

fn churn(rebuild: bool) -> Churn {
    // 76.99 MiB: the layout gate sweep's budget below B_DEC43 / 3.
    let mut w = World::new(CoinsBudget::new(80_733_643, 0, Ratio::HALF, 0));
    let mut r = Rng::new(24);
    let coin = p2pkh(1);
    let prefill = w.cache.active().sizes().total_buckets() * 18 / 25;
    let mut young: Vec<_> = (0..prefill).map(|_| random_key(&mut r, 0)).collect();
    for k in &young {
        w.cache.put_active(k, Flags::DIRTY | Flags::FRESH, coin.as_bytes());
    }
    let mut old: Vec<OutPointKey> = Vec::new();
    let mut c = Churn::default();
    let (mut f_blocks, mut after_freeze) = (0, 0);
    for height in 1..=200 {
        let active = w.cache.active();
        let sizes = active.sizes();
        // Each shard's buckets, or its ceiling's while it is below it.
        let at_least_ceiling = |l: &ShardedLayer| -> Vec<usize> {
            l.shard_loads().enumerate().map(|(i, (_, b))| b.max(sizes.buckets(i))).collect()
        };
        let buckets_before = at_least_ceiling(active);
        let no_f = w.cache.frozen().is_none();
        if no_f {
            for (i, (len, buckets)) in active.shard_loads().enumerate() {
                let below_trigger = 64 * len < 55 * buckets;
                let short = 64 * active.shard_room(i) <= buckets;
                c.short_room += u32::from(buckets >= sizes.buckets(i) && below_trigger && short);
            }
        }
        for _ in 0..4_000 {
            let k = random_key(&mut r, 0);
            w.cache.add(&k, src(&coin), false).unwrap();
            young.push(k);
        }
        // Most spends take young coins (FRESH in active: removed entries), some old ones
        // (tombstones).
        for _ in 0..2_400 {
            let k = young.swap_remove(r.below(young.len() as u64) as usize);
            assert!(w.cache.spend(&k).unwrap());
        }
        for _ in 0..100.min(old.len()) {
            let k = old.swap_remove(r.below(old.len() as u64) as usize);
            assert!(w.cache.spend(&k).unwrap());
        }
        let buckets_after = at_least_ceiling(w.cache.active());
        if no_f && w.cache.frozen().is_none() && buckets_after != buckets_before {
            c.grew_no_f += 1;
        }
        c.blocks += 1;
        // The boundary: F completes after three blocks; THRESHOLD freezes; else the rebuild.
        if let Some(f) = w.cache.frozen().cloned() {
            f_blocks += 1;
            if f_blocks == 3 {
                w.release(f);
            }
        } else if w.cache.boundary_state(0, false) == CoinsBoundary::Threshold {
            old.append(&mut young);
            drop(w.cache.freeze_at(height, 0));
            f_blocks = 0;
            c.freezes += 1;
        } else if rebuild {
            c.rebuilt += w.cache.boundary_rebuild();
        }
        after_freeze += u32::from(c.freezes > 0);
        if after_freeze == 20 {
            break;
        }
    }
    c
}

/// `ResizeCoinsCaches`' flush (`validation.cpp:5866-5892`): none only with both sizes unchanged,
/// IF_NEEDED for a grown share, and ALWAYS for a shrunk one or a change to the coins database's
/// size alone (O9).
#[test]
fn a_resize_reports_a_database_only_change() {
    let mut w = World::new(CoinsBudget::new(B_4, 0, Ratio::HALF, 0));
    assert_eq!(w.cache.resize(B_4, false), Resized::Unchanged);
    assert_eq!(w.cache.resize(B_4, true), Resized::DbOnly);
    assert_eq!(w.cache.resize(2 * B_4, false), Resized::Grew);
    assert_eq!(w.cache.resize(B_4, true), Resized::Shrank);
    assert_eq!(w.cache.budget().coins_share(), B_4);
}

/// A freeze while active's shards are still growing to their ceilings (DEC-388's test): F holds,
/// routes and charges as a presized layer with the same entries, Σ is the presized one's, and the
/// next active's ceiling is the one a presized F leaves. At f = 3/4 that ceiling is `B − R − B/16`
/// less F's charge, so F charged at its allocations would give a larger one.
#[test]
fn a_layer_frozen_mid_growth_routes_as_a_presized_one() {
    let mut w = World::new(CoinsBudget::new(64 << 20, 0, Ratio::new(3, 4), 0));
    let sizes = w.cache.active().sizes();
    let presize = w.cache.budget().presize(0, 0);
    let mut presized = ShardedLayer::presized(presize, Salt::fixed(7));
    assert_eq!(presized.sizes(), sizes);
    let start: usize = (0..64).map(|i| sizes.start_buckets(i)).sum();
    let mut r = Rng::new(31);
    for i in 0..start as u64 * 7 / 8 {
        let (k, coin) = (random_key(&mut r, 0), p2pkh(i));
        w.cache.put_active(&k, Flags::DIRTY | Flags::FRESH, coin.as_bytes());
        presized.upsert(&k, Flags::DIRTY | Flags::FRESH, coin.as_bytes());
    }
    let active = w.cache.active();
    let grown = active.shard_loads().enumerate().filter(|&(i, (_, b))| b > sizes.start_buckets(i));
    let n = grown.count();
    assert!(0 < n && n < 64, "mid-growth: {n} shards grew");
    assert_eq!(active.shard_lens(), presized.shard_lens());
    let u = w.cache.usage();
    assert_eq!(u.active + u.unbacked, presized.charged_bytes());
    // No shard is at its trigger: a shard below its ceiling never is, whatever its load.
    assert!(!active.at_shard_trigger());
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Ok);
    let f = w.cache.freeze_at(1, 0);
    let frozen = f.coins();
    assert_eq!((frozen.sizes(), frozen.shard_lens()), (sizes, presized.shard_lens()));
    assert_eq!(frozen.charged_bytes(), presized.charged_bytes());
    let entries = |l: &ShardedLayer| {
        let mut v: Vec<_> = l.iter_dirty().map(|(k, fl, c)| (k, fl, c.to_vec())).collect();
        v.sort_by_key(|e| e.0);
        v
    };
    assert_eq!(entries(frozen), entries(&presized));
    let next = ShardSizes::for_bytes(w.cache.budget().presize(0, presized.charged_bytes()));
    assert_eq!(w.cache.active().sizes(), next);
    let lazy_next = ShardSizes::for_bytes(w.cache.budget().presize(0, frozen.allocated_bytes()));
    assert_ne!(lazy_next, next, "the test wants F's charge to set the ceiling");
    let u = w.cache.usage();
    let charged = presized.charged_bytes() + w.cache.active().charged_bytes();
    assert_eq!(u.frozen + u.active + u.unbacked, charged);
    drop(f);
}
