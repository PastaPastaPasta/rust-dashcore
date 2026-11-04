//! The block overlay (CH2 §2.4.2, §2.5 "Ownership"): the ownership witnesses, no child copy after
//! `have_inputs` (rev5, O-R6-m3), Core's `ccoins_flush_behavior` (its erase-flush half), and the
//! overlay's `CoinsView`.

use dash_consensus::coins::CoinsView as _;
use dash_consensus::primitives::TxOut;

use super::*;
use crate::coins::hasher::SaltedHasher;
pub(super) use crate::coins::model::spending;
use crate::coins::{BlockOverlay, CoinSrc};
use crate::error::CoinsError;

/// GPT's long-input coin, 9,583 bytes: a 9,577-byte script that an empty `scriptSig` spends.
/// Eighteen 520-byte pushes and one of 141 bytes, each dropped, then `OP_TRUE`: 19 counted ops,
/// no push over 520 bytes, under the 10,000-byte script limit. `i` fills the pushes.
pub(super) fn long_input(i: u64) -> PackedCoin {
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;
    const OP_DROP: u8 = 0x75;
    const OP_TRUE: u8 = 0x51;
    let mut script = Vec::with_capacity(9_577);
    for len in [520u16; 18].into_iter().chain([141]) {
        match u8::try_from(len) {
            Ok(len) => script.extend_from_slice(&[OP_PUSHDATA1, len]),
            Err(_) => {
                script.push(OP_PUSHDATA2);
                script.extend_from_slice(&len.to_le_bytes());
            }
        }
        script.resize(script.len() + usize::from(len), i as u8);
        script.push(OP_DROP);
    }
    script.push(OP_TRUE);
    assert_eq!(script.len(), 9_577);
    let out = TxOut {
        value: 10_000,
        script_pubkey: script.into(),
    };
    let coin = PackedCoin::from_output(&out, 20_000, false).unwrap();
    assert_eq!(coin.as_bytes().len(), 9_583);
    coin
}

/// The shard of active `k` falls in (the tests' salt).
fn shard(w: &World, k: &OutPointKey) -> usize {
    w.cache.active().sizes().shard_of(SaltedHasher::new(Salt::fixed(7)).hash(k))
}

/// The witnesses' inputs, in the database: eight long-input coins, then four inline coins in the
/// first one's shard of active, whose bytes a rehash of that shard moves.
fn stored_inputs(w: &World, r: &mut Rng) -> Vec<(OutPointKey, PackedCoin)> {
    let long: Vec<_> = (0..8).map(|i| (random_key(r, i), long_input(i.into()))).collect();
    let s = shard(w, &long[0].0);
    let inline = std::iter::repeat_with(|| random_key(r, 0))
        .filter(|k| shard(w, k) == s)
        .zip(1..=4)
        .map(|(k, i)| (k, p2pkh(i)));
    let inputs: Vec<_> = long.into_iter().chain(inline).collect();
    for (k, c) in &inputs {
        w.db_set(k, Some(c));
    }
    inputs
}

/// Fetches DB coins of active's shard `s` into active through `have_inputs` until `s` has grown:
/// a rehash of the shard the inputs are in. Each round adds 256 entries, and a shard grows by 7/8
/// of its buckets, so it must have grown within `buckets / 256` rounds (O7).
fn grow_shard(overlay: &mut BlockOverlay<'_, ProbeEngine>, w_db: &Db, r: &mut Rng, s: usize) {
    let hasher = SaltedHasher::new(Salt::fixed(7));
    let sizes = overlay.tip().active().sizes();
    let buckets =
        |o: &BlockOverlay<'_, ProbeEngine>| o.tip().active().shard_loads().nth(s).unwrap().1;
    let before = buckets(overlay);
    let table = CoinsTable::default();
    let coin = coin(7);
    for round in 0.. {
        if buckets(overlay) != before {
            break;
        }
        assert!(round <= before / 256, "shard {s} has not grown: have_inputs caches each input");
        let keys: Vec<_> = std::iter::repeat_with(|| random_key(r, 0))
            .filter(|k| sizes.shard_of(hasher.hash(k)) == s)
            .take(256)
            .collect();
        let mut b = w_db.batch();
        for k in &keys {
            table.put(&mut b, k, coin.as_bytes());
        }
        w_db.write(b, Commit::Buffered).unwrap();
        assert!(overlay.have_inputs(&spending(&keys)).unwrap());
    }
}

/// Ownership witness 1: long and inline inputs fetched by `have_inputs` (no child copy), the
/// inputs' shard of active rehashed between the fetch and the spend, the spends, a failed check,
/// the overlay dropped. The parent then reads the original bytes, and `held_out` is back to 0 once
/// the undo drops.
#[test]
fn a_failed_long_input_connect_leaves_the_parent_whole() {
    let mut w = World::with_dbcache(2);
    let mut r = Rng::new(31);
    let inputs = stored_inputs(&w, &mut r);
    let keys: Vec<_> = inputs.iter().map(|(k, _)| *k).collect();
    let s = shard(&w, &keys[0]);
    let db = w.db.clone();
    {
        let mut overlay = BlockOverlay::new(&mut w.cache);
        assert!(overlay.have_inputs(&spending(&keys)).unwrap());
        assert!(overlay.child().is_empty(), "no child copy after have_inputs");
        assert!(keys.iter().all(|k| overlay.tip().active().get(k).is_some()), "cached in active");
        grow_shard(&mut overlay, &db, &mut r, s);
        let undo: Vec<_> = keys.iter().map(|k| overlay.spend(k).unwrap().unwrap()).collect();
        assert!(undo[..8].iter().all(PackedCoin::is_held));
        assert_eq!(overlay.usage().held_out, 8 * 9_583);
        for ((_, want), got) in inputs.iter().zip(&undo) {
            assert_eq!(got.as_bytes(), want.as_bytes());
        }
        // A check fails: the block is abandoned.
        drop(undo);
        assert_eq!(overlay.usage().held_out, 0);
    }
    for (k, c) in &inputs {
        assert_eq!(w.cache.fetch(k).unwrap().map(|c| c.to_coin()), Some(c.as_ref().to_coin()));
        assert!(
            w.cache
                .active()
                .get(k)
                .is_some_and(|e| e.flags == Flags::NONE && e.coin == c.as_bytes())
        );
    }
    w.cache.sanity_check().unwrap();
}

/// Ownership witness 2: the same block committed, then disconnected from its undo, with the
/// inputs' shard rehashed between the fetch and the spend. Every restored coin is identical to the
/// original.
#[test]
fn a_committed_long_input_connect_disconnects_to_identical_coins() {
    let mut w = World::with_dbcache(2);
    let mut r = Rng::new(32);
    let inputs = stored_inputs(&w, &mut r);
    let keys: Vec<_> = inputs.iter().map(|(k, _)| *k).collect();
    let s = shard(&w, &keys[0]);
    let db = w.db.clone();
    let out_key = random_key(&mut r, 0);
    let undo: Vec<PackedCoin> = {
        let mut overlay = BlockOverlay::new(&mut w.cache);
        assert!(overlay.have_inputs(&spending(&keys)).unwrap());
        grow_shard(&mut overlay, &db, &mut r, s);
        let undo = keys.iter().map(|k| overlay.spend(k).unwrap().unwrap()).collect();
        overlay.add(&out_key, CoinSrc::Bytes(coin(5).as_ref()), false).unwrap();
        overlay.commit(block(1)).unwrap();
        undo
    };
    assert_eq!(w.cache.usage().held_out, 8 * 9_583, "the undo outlives the overlay");
    assert!(keys.iter().all(|k| w.cache.fetch(k).unwrap().is_none()));
    w.flush(1);
    {
        // DisconnectBlock: spend the outputs, restore the inputs (ApplyTxInUndo), in reverse.
        let mut overlay = BlockOverlay::new(&mut w.cache);
        assert!(overlay.spend(&out_key).unwrap().is_some());
        for (k, u) in keys.iter().zip(&undo).rev() {
            let clean = !overlay.have_coin(&k.outpoint()).unwrap();
            assert!(clean);
            overlay.add(k, CoinSrc::Bytes(u.as_ref()), !clean).unwrap();
        }
        overlay.commit(block(0)).unwrap();
    }
    drop(undo);
    assert_eq!(w.cache.usage().held_out, 0);
    for (k, c) in &inputs {
        assert_eq!(w.cache.active().get(k).map(|e| e.coin), Some(c.as_bytes()));
    }
    w.flush(2);
    assert!(inputs.iter().all(|(k, c)| w.db_get(k).as_ref() == Some(c)));
    assert_eq!(w.db_get(&out_key), None);
}

/// A child-owned long coin moves out of the child without a copy and is counted once (§6.7): the
/// allocator's bytes equal native Σ − R before and after, with the box in `held_out` and no longer
/// in the child; a clone is a second box, counted too.
#[test]
#[cfg(feature = "alloc-tag")]
fn a_child_owned_long_coin_moves_out_counted_once() {
    let tagged = Tagged::start();
    let mut w = World::with_dbcache(2);
    let k = random_key(&mut Rng::new(33), 0);
    let mut overlay = BlockOverlay::new(&mut w.cache);
    overlay.add(&k, CoinSrc::Bytes(long_input(1).as_ref()), false).unwrap();
    tagged.check(overlay.usage(), "after the add");
    let moved = overlay.spend(&k).unwrap().unwrap();
    tagged.check(overlay.usage(), "after the spend");
    assert!(moved.is_held() && overlay.child().get(&k).is_none(), "FRESH: no tombstone");
    assert_eq!(overlay.usage().held_out, 9_583);
    let copy = moved.clone();
    assert_eq!(overlay.usage().held_out, 2 * 9_583);
    tagged.check(overlay.usage(), "with a clone");
    drop((moved, copy));
    assert_eq!(overlay.usage().held_out, 0);
    tagged.check(overlay.usage(), "after both drop");
}

/// At the commit a child-owned long coin's box moves into active (CH2 §2.5), as Core's `BatchWrite`
/// moves the `Coin`: the allocator never holds a second copy.
#[test]
#[cfg(feature = "alloc-tag")]
fn a_committed_long_coin_moves_into_active_without_a_copy() {
    let tagged = Tagged::start();
    let mut w = World::with_dbcache(2);
    let k = random_key(&mut Rng::new(34), 0);
    let mut overlay = BlockOverlay::new(&mut w.cache);
    overlay.add(&k, CoinSrc::Bytes(long_input(1).as_ref()), false).unwrap();
    let before = tagged.bytes();
    tagged.reset_peak();
    overlay.commit(block(1)).unwrap();
    assert!(tagged.peak() - before < 9_583, "a copy: peak {} over {before}", tagged.peak());
    tagged.check(w.cache.usage(), "after the commit");
    assert!(w.cache.active().get(&k).is_some_and(|e| e.coin == long_input(1).as_bytes()));
}

/// CH2c D49: dropping an overlay allocates and frees nothing of its own. Its child moves back to
/// the tip with nothing built in its place, and a child whose shards kept their tables resets in
/// place.
#[test]
#[cfg(feature = "alloc-tag")]
fn dropping_an_overlay_allocates_nothing() {
    use crate::alloc_tag::thread_alloc_events;
    let mut w = World::with_dbcache(2);
    drop(BlockOverlay::new(&mut w.cache));
    let mut overlay = BlockOverlay::new(&mut w.cache);
    let k = random_key(&mut Rng::new(38), 0);
    overlay.add(&k, CoinSrc::Bytes(p2pkh(1).as_ref()), false).unwrap();
    let before = thread_alloc_events();
    drop(overlay);
    assert_eq!(thread_alloc_events() - before, 0, "allocations and frees in the drop");
}

/// The loads a reset leaves (CH2c D36): a shard's table kept, emptied, up to 64 buckets (or its
/// start), else back at its start.
fn kept_loads(grown: &[usize], starts: &[usize]) -> Vec<usize> {
    grown
        .iter()
        .zip(starts)
        .map(|(&g, &s)| {
            if g <= s.max(64) {
                g
            } else {
                s
            }
        })
        .collect()
}

fn bucket_loads(layer: &ShardedLayer) -> Vec<usize> {
    layer.shard_loads().map(|(_, buckets)| buckets).collect()
}

/// A committed overlay's child is kept, emptied, for the next block (CH2c D36): the next overlay
/// starts from it with no entry and no long coin, each grown shard kept up to 64 buckets and the
/// larger ones back at their start, and native Σ counts the kept layer as `overlay` between blocks.
#[test]
fn a_committed_child_is_reset_for_the_next_block() {
    let mut w = World::with_dbcache(2);
    let mut r = Rng::new(36);
    let starts = bucket_loads(&ShardedLayer::new(0, Salt::fixed(7)));
    let keys: Vec<_> = (0..3_000).map(|_| random_key(&mut r, 0)).collect();
    let mut overlay = BlockOverlay::new(&mut w.cache);
    for (i, k) in (1..).zip(&keys) {
        overlay.add(k, CoinSrc::Bytes(p2pkh(i).as_ref()), false).unwrap();
    }
    let long = random_key(&mut r, 1);
    overlay.add(&long, CoinSrc::Bytes(long_input(1).as_ref()), false).unwrap();
    let grown = bucket_loads(overlay.child());
    let kept = kept_loads(&grown, &starts);
    assert!(kept.iter().zip(&starts).any(|(k, s)| k > s), "a grown shard kept");
    assert!(grown.iter().zip(&kept).any(|(g, k)| g > k), "a larger shard back at its start");
    overlay.commit(block(1)).unwrap();
    let spare = w.cache.usage().overlay;
    assert!(keys.iter().chain([&long]).all(|k| w.cache.active().get(k).is_some()));

    let mut overlay = BlockOverlay::new(&mut w.cache);
    let child = overlay.child();
    assert_eq!(child.len(), 0);
    assert_eq!(bucket_loads(child), kept);
    assert_eq!(child.allocated_bytes(), spare, "native Σ counted the kept layer");
    overlay.spend(&keys[0]).unwrap();
    let k = random_key(&mut r, 2);
    overlay.add(&k, CoinSrc::Bytes(p2pkh(7).as_ref()), false).unwrap();
    overlay.commit(block(2)).unwrap();
    assert!(w.cache.active().get(&keys[0]).is_none(), "FRESH and spent: dropped");
    assert_eq!(
        w.cache.active().get(&k).map(|e| e.coin.to_vec()),
        Some(p2pkh(7).as_bytes().to_vec())
    );
}

/// CH2c D36: an overlay dropped uncommitted (a failed block, or a test's) returns its child too,
/// and leaves the tip's coins untouched. The same coins added again to the reused child end at the
/// same shard sizes and the same table, unbacked and allocated bytes as in a new one: a reset
/// counts its kept tables right.
#[test]
fn a_dropped_child_is_reset_for_the_next_block() {
    let mut w = World::with_dbcache(2);
    let mut r = Rng::new(37);
    let starts = bucket_loads(&ShardedLayer::new(0, Salt::fixed(7)));
    let keys: Vec<_> = (0..3_000).map(|_| random_key(&mut r, 0)).collect();
    let long = random_key(&mut r, 1);
    let fill = |overlay: &mut BlockOverlay<'_, _>| {
        for (i, k) in (1..).zip(&keys) {
            overlay.add(k, CoinSrc::Bytes(p2pkh(i).as_ref()), false).unwrap();
        }
        overlay.add(&long, CoinSrc::Bytes(long_input(1).as_ref()), false).unwrap();
        let c = overlay.child();
        (bucket_loads(c), c.table_bytes(), c.unbacked_bytes(), c.allocated_bytes())
    };
    let mut overlay = BlockOverlay::new(&mut w.cache);
    let first = fill(&mut overlay);
    assert_eq!(overlay.tip().usage().overlay, 0, "no spare while an overlay holds it");
    drop(overlay);
    assert_eq!(w.cache.active().len(), 0, "nothing merged");

    let mut overlay = BlockOverlay::new(&mut w.cache);
    let child = overlay.child();
    assert_eq!(child.len(), 0);
    assert_eq!(bucket_loads(child), kept_loads(&first.0, &starts));
    assert!(keys.iter().chain([&long]).all(|k| child.get(k).is_none()));
    assert_eq!(fill(&mut overlay), first);
}

/// Core: coins_tests.cpp:877-1072 TestFlushBehavior and ccoins_flush_behavior, the erase-flush half
/// (`Sync()` has no production caller), with Core's two caches as the tip and an overlay. Our flush
/// of everything is a freeze, W's write and the release.
#[test]
fn ccoins_flush_behavior_erase_half() {
    let mut w = World::with_dbcache(2);
    let mut r = Rng::new(34);
    let tip_entry =
        |w: &World, k: &OutPointKey| w.cache.active().get(k).map(|e| (e.coin.to_vec(), e.flags));
    let the_coin = coin(r.below(1 << 31) as i64);
    let k = random_key(&mut r, 0);
    assert_eq!(w.db_get(&k), None);
    assert!(w.cache.fetch(&k).unwrap().is_none());
    // 1. Add to the tip.
    w.cache.add(&k, CoinSrc::Bytes(the_coin.as_ref()), false).unwrap();
    assert_eq!(w.db_get(&k), None);
    assert_eq!(
        tip_entry(&w, &k),
        Some((the_coin.as_bytes().to_vec(), Flags::DIRTY | Flags::FRESH))
    );
    // 4. Flush everything (erasing).
    let size = w.cache.active().len();
    w.flush(1);
    assert!(w.cache.active().len() < size);
    // 5. Gone from the cache, in the base; an access brings it back clean.
    assert_eq!(tip_entry(&w, &k), None);
    assert!(w.cache.fetch(&k).unwrap().is_some());
    assert_eq!(tip_entry(&w, &k), Some((the_coin.as_bytes().to_vec(), Flags::NONE)));
    assert!(matches!(
        w.cache.add(&k, CoinSrc::Bytes(the_coin.as_ref()), false),
        Err(CoinsError::OverwriteUnspent)
    ));
    // 6. Spend: spent and DIRTY in the tip, still unspent in the base, then gone from both.
    assert!(w.cache.spend(&k).unwrap());
    assert_eq!(tip_entry(&w, &k), Some((Vec::new(), Flags::DIRTY | Flags::SPENT)));
    assert!(w.cache.fetch(&k).unwrap().is_none());
    assert!(w.db_get(&k).is_some());
    w.flush(2);
    assert!(w.cache.fetch(&k).unwrap().is_none() && w.db_get(&k).is_none());
    assert!(!w.cache.spend(&k).unwrap());
    // Bonus 1: a coin added through the tip and flushed is spent by an overlay that never saw it.
    let k = random_key(&mut r, 0);
    w.cache.add(&k, CoinSrc::Bytes(the_coin.as_ref()), false).unwrap();
    w.flush(3);
    assert!(w.db_get(&k).is_some() && !w.cache.have_in_cache(&k));
    {
        let mut overlay = BlockOverlay::new(&mut w.cache);
        assert!(overlay.spend(&k).unwrap().is_some());
        overlay.commit(block(4)).unwrap();
    }
    w.flush(4);
    assert!(w.db_get(&k).is_none() && w.cache.fetch(&k).unwrap().is_none());
    // Bonus 2: a FRESH coin spent before a flush leaves no trace.
    let k = random_key(&mut r, 0);
    w.cache.add(&k, CoinSrc::Bytes(the_coin.as_ref()), false).unwrap();
    assert_eq!(tip_entry(&w, &k).map(|e| e.1), Some(Flags::DIRTY | Flags::FRESH));
    assert!(w.cache.spend(&k).unwrap());
    w.flush(5);
    assert_eq!(tip_entry(&w, &k), None);
    assert!(!w.cache.have_in_cache(&k) && w.db_get(&k).is_none());
    // Core's view = the child: added in an overlay (DIRTY|FRESH in the child), committed and
    // flushed. A later overlay reads it with no child copy (rev5).
    let k = random_key(&mut r, 0);
    {
        let mut overlay = BlockOverlay::new(&mut w.cache);
        overlay.add(&k, CoinSrc::Bytes(the_coin.as_ref()), false).unwrap();
        assert_eq!(overlay.child().get(&k).map(|e| e.flags), Some(Flags::DIRTY | Flags::FRESH));
        overlay.commit(block(6)).unwrap();
    }
    w.flush(6);
    let overlay = BlockOverlay::new(&mut w.cache);
    assert!(overlay.coin(&k.outpoint()).unwrap().is_some());
    assert!(overlay.child().is_empty());
}

/// The overlay's `&self` reads cache nothing: a database hit lives in scratch (counted as overlay
/// usage), and every borrow survives the reads after it.
#[test]
fn overlay_reads_cache_nothing_and_borrows_survive() {
    let mut w = World::with_dbcache(2);
    let mut r = Rng::new(35);
    let stored: Vec<_> = (0..200).map(|i| (random_key(&mut r, 0), coin(i))).collect();
    for (k, c) in &stored {
        w.db_set(k, Some(c));
    }
    let overlay = BlockOverlay::new(&mut w.cache);
    let empty = overlay.usage().overlay;
    let refs: Vec<_> =
        stored.iter().map(|(k, _)| overlay.coin(&k.outpoint()).unwrap().unwrap()).collect();
    for ((_, c), got) in stored.iter().zip(&refs) {
        assert_eq!(got.to_coin(), c.as_ref().to_coin());
    }
    assert!(overlay.usage().overlay > empty);
    assert!(stored.iter().all(|(k, _)| overlay.have_coin(&k.outpoint()).unwrap()));
    assert!(overlay.child().is_empty() && overlay.tip().active().is_empty());
    assert_eq!(overlay.best_block(), None);
    drop(refs);
    drop(overlay);
    w.cache.set_best_block(Some(block(9)));
    let overlay = BlockOverlay::new(&mut w.cache);
    assert_eq!(overlay.best_block(), Some(block(9).hash));
}

/// The block's best block is [`BlockOverlay::commit`]'s argument (O4). The reviewer's probe, a
/// commit without one that left the tip reading the durable block, no longer compiles (the
/// `compile_fail` example on `commit`). The overlay reads the tip's best block, as Core's child
/// reads its parent's (`coins.cpp:172-176`), and the commit sets the tip's to the block's.
#[test]
fn a_commit_sets_the_tips_best_block() {
    let w = World::with_dbcache(2);
    let budget = CoinsBudget::new(2 * MIB, 0, Ratio::HALF, 0);
    let mut cache = CoinsCache::new(w.db.clone(), budget, Salt::fixed(1), Some(block(1)));
    assert_eq!(BlockOverlay::new(&mut cache).best_block(), Some(block(1).hash), "the durable");
    BlockOverlay::new(&mut cache).commit(block(2)).unwrap();
    assert_eq!(cache.best_block(), Some(block(2)));
    let overlay = BlockOverlay::new(&mut cache);
    assert_eq!(overlay.best_block(), Some(block(2).hash), "the parent's, as Core's child reads");
    overlay.commit(block(3)).unwrap();
    assert_eq!(cache.best_block(), Some(block(3)));
}

/// Two documented differences from Core's child (CH2 §2.4.2): with no child entry, `add` without
/// `possible_overwrite` consults the tip (Core's child knows only what it fetched; the deviation
/// DEC-131 declares), and spending a child tombstone returns no coin (Core returns true with a
/// spent one; the undo gets nothing either way).
#[test]
fn the_overlays_documented_differences() {
    let mut w = World::with_dbcache(2);
    let k = random_key(&mut Rng::new(36), 0);
    w.cache.add(&k, CoinSrc::Bytes(coin(3).as_ref()), false).unwrap();
    let mut overlay = BlockOverlay::new(&mut w.cache);
    assert!(matches!(
        overlay.add(&k, CoinSrc::Bytes(coin(4).as_ref()), false),
        Err(CoinsError::OverwriteUnspent)
    ));
    assert!(overlay.spend(&k).unwrap().is_some());
    assert_eq!(overlay.child().get(&k).map(|e| e.flags), Some(Flags::DIRTY | Flags::SPENT));
    assert!(overlay.spend(&k).unwrap().is_none());
}
