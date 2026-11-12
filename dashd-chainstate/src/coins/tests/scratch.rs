//! VerifyDB's scratch layer (CH5-1a scope 1, DEC-981a): its reads below go through the parent
//! tip's fetching read on the `verifychain` path and to the database at startup; nothing else it
//! does reaches the tip; and the level-3 figure and its `<=` against the coins share.

use std::sync::{Arc, Mutex};

use super::*;
use crate::coins::{BlockOverlay, CoinSrc, CoinsBoundary, Usage};

/// A tip at block 5 whose database holds `db` and whose active layer holds `active` (dirty),
/// in a cell the scratch layer can take as its parent.
fn tip(db: &[(u8, i64)], active: &[(u8, i64)]) -> (World, Arc<Mutex<CoinsCache<ProbeEngine>>>) {
    let mut w = World::with_dbcache(4);
    for &(k, value) in db {
        w.db_set(&key(k), Some(&coin(value)));
    }
    w.cache.set_best_block(Some(block(5)));
    {
        let mut ov = BlockOverlay::new(&mut w.cache);
        for &(k, value) in active {
            let out = output(value);
            let src = CoinSrc::Output {
                out: &out,
                height: 5,
                coinbase: false,
            };
            ov.add(&key(k), src, false).unwrap();
        }
        ov.commit(block(5)).unwrap();
    }
    let cell = Arc::new(Mutex::new(std::mem::replace(
        &mut w.cache,
        CoinsCache::new(
            w.db.clone(),
            CoinsBudget::new(4 * MIB, 0, Ratio::HALF, 0),
            Salt::fixed(7),
            None,
        ),
    )));
    (w, cell)
}

fn key(n: u8) -> OutPointKey {
    OutPointKey::new(&dash_consensus::primitives::OutPoint::new([n; 32].into(), u32::from(n)))
}

/// The tip's active flags at `k`, if it has an entry.
fn tip_flags(cell: &Mutex<CoinsCache<ProbeEngine>>, k: &OutPointKey) -> Option<Flags> {
    cell.lock().unwrap().active().get(k).map(|e| e.flags)
}

/// Core's parent `FetchCoin` (`coins.cpp:44-65`): a database hit read through the scratch layer
/// becomes a clean tip entry, the tip's own entries keep their flags, a miss leaves nothing, and
/// each read is the tip's, so a second one needs no database read.
#[test]
fn a_scratch_read_fills_the_tip_as_cores_parent_fetch_does() {
    let (w, cell) = tip(&[(1, 10), (2, 20)], &[(3, 30)]);
    let mut scratch = CoinsCache::scratch(&cell.lock().unwrap(), Some(cell.clone()));
    assert_eq!(scratch.best_block(), Some(block(5)));
    assert_eq!(tip_flags(&cell, &key(1)), None);

    // A database hit: clean in the tip and in the scratch layer.
    assert_eq!(scratch.fetch(&key(1)).unwrap().map(|c| c.value()), Some(10));
    assert_eq!(tip_flags(&cell, &key(1)), Some(Flags::NONE));
    assert!(cell.lock().unwrap().have_in_cache(&key(1)));
    let reads = w.db_reads();
    // The tip's dirty entry keeps its flags.
    assert_eq!(scratch.fetch(&key(3)).unwrap().map(|c| c.value()), Some(30));
    assert_eq!(tip_flags(&cell, &key(3)), Some(Flags::DIRTY | Flags::FRESH));
    // A miss stays absent.
    assert!(scratch.fetch(&key(9)).unwrap().is_none());
    assert_eq!(tip_flags(&cell, &key(9)), None);
    assert!(w.db_reads() > reads);

    // A shared read (an overlay's `have_coin`) fills the tip too.
    {
        let ov = BlockOverlay::new(&mut scratch);
        use dash_consensus::coins::CoinsView as _;
        assert!(ov.have_coin(&key(2).outpoint()).unwrap());
    }
    assert_eq!(tip_flags(&cell, &key(2)), Some(Flags::NONE));

    // Mutations stay in the scratch layer: a spend and an add, committed into it.
    let before: Vec<_> = (1..=4).map(|k| tip_flags(&cell, &key(k))).collect();
    {
        let mut ov = BlockOverlay::new(&mut scratch);
        ov.spend(&key(1)).unwrap().expect("unspent");
        let out = output(40);
        let src = CoinSrc::Output {
            out: &out,
            height: 6,
            coinbase: false,
        };
        ov.add(&key(4), src, false).unwrap();
        ov.commit(block(4)).unwrap();
    }
    let after: Vec<_> = (1..=4).map(|k| tip_flags(&cell, &key(k))).collect();
    assert_eq!(after, before);
    let tip = cell.lock().unwrap();
    assert_eq!(tip.best_block(), Some(block(5)));
    assert!(tip.have_in_cache(&key(1)));
    assert!(!tip.have_in_cache(&key(4)));
    assert!(tip.frozen().is_none());
    tip.sanity_check().unwrap();
}

/// A prefetched coin is consumed by the parent read and becomes a clean tip entry, as the tip's
/// own fetch would make it (the one sanctioned write besides the fills).
#[test]
fn a_scratch_read_takes_the_tips_prefetch_as_its_fetch_would() {
    let (_w, cell) = tip(&[(1, 10)], &[]);
    {
        let mut tip = cell.lock().unwrap();
        let batch = crate::coins::PrefetchBatch {
            fetch_gen: tip.released_gen(),
            epoch: tip.db().write_epoch(),
            entries: vec![(key(1), coin(10))],
        };
        assert_eq!(tip.admit_prefetch(batch, 0), 1);
    }
    let mut scratch = CoinsCache::scratch(&cell.lock().unwrap(), Some(cell.clone()));
    assert!(scratch.fetch(&key(1)).unwrap().is_some());
    let tip = cell.lock().unwrap();
    assert_eq!(tip.active().get(&key(1)).map(|e| e.flags), Some(Flags::NONE));
}

/// Startup's scratch layer reads the database at its durable block and fills nothing: the tip's
/// active layer is not its parent (`node/chainstate.cpp:437`).
#[test]
fn startups_scratch_reads_the_database_and_fills_nothing() {
    let (w, cell) = tip(&[(1, 10)], &[(3, 30)]);
    let tip = cell.lock().unwrap();
    // The database's best block, as a reopen would find it.
    let fresh = CoinsCache::new(
        w.db.clone(),
        CoinsBudget::new(4 * MIB, 0, Ratio::HALF, 0),
        Salt::fixed(7),
        Some(block(5)),
    );
    let mut scratch = CoinsCache::scratch(&fresh, None);
    assert_eq!(scratch.best_block(), Some(block(5)));
    assert!(scratch.fetch(&key(1)).unwrap().is_some());
    assert!(scratch.fetch(&key(3)).unwrap().is_none(), "the tip's unflushed coin is not read");
    assert_eq!(fresh.active().len(), 0);
    assert!(!fresh.have_in_cache(&key(1)));
    assert_eq!(tip.active().get(&key(1)).map(|e| e.flags), None);
}

/// DEC-981a's figure: the tip's native Σ with the scratch layer's allocations as an open
/// overlay; the gate passes at equality and skips one byte over (`validation.cpp:5123` at v24.0.0).
#[test]
fn the_level_3_gate_passes_at_equality_and_fails_one_byte_over() {
    let (_w, cell) = tip(&[(1, 10), (2, 20)], &[(3, 30)]);
    let mut scratch = CoinsCache::scratch(&cell.lock().unwrap(), Some(cell.clone()));
    scratch.fetch(&key(1)).unwrap();
    let mut tip = cell.lock().unwrap();
    let figure = tip.verify_usage(&scratch);
    let usage = tip.usage();
    let want = Usage {
        overlay: usage.overlay + scratch.usage().allocated(),
        ..usage
    }
    .total();
    assert_eq!(figure, want);
    assert!(scratch.usage().allocated() > 0);
    tip.resize(figure, false);
    assert!(tip.verify_fits(&scratch), "equality passes");
    tip.resize(figure - 1, false);
    assert!(!tip.verify_fits(&scratch), "one byte over skips");
    tip.resize(figure + 1, false);
    assert!(tip.verify_fits(&scratch));
}

/// The level-3 gate at 0 coins with the node's budget (DEC-1007): the shards' full ceilings
/// (backed and unbacked) and the scratch layer's first page against the coins share. At rdashd's
/// default `-maxmempool` of 5 MB (its default `-blocksonly`, DEC-322) the threshold is
/// `-dbcache=10` with `-txindex` (Dash's default) and 9 without, and the default `-dbcache` passes
/// by 147,510,784 bytes with `-txindex` and 168,629,760 without. At Core's 300 MB
/// (`-blocksonly=0`) it is 281 and 246, and the margins 8,771,072 and 31,086,080.
#[test]
fn the_node_s_level_3_gate_at_0_coins() {
    use crate::caches::{CacheSizes, IndexArgs};

    let gate = |dbcache: i64, txindex: bool, max_mempool: u64| {
        let args = IndexArgs {
            txindex,
            ..IndexArgs::default()
        };
        let sizes = CacheSizes::calculate(dbcache, &args);
        let w = World::new(CoinsBudget::node(&sizes, max_mempool));
        let scratch = CoinsCache::scratch(&w.cache, None);
        let figure = w.cache.verify_usage(&scratch);
        (sizes.coins, figure, w.cache.verify_fits(&scratch))
    };
    let blocks_only = 5_000_000;
    assert_eq!(gate(300, true, blocks_only), (265_027_584, 117_516_800, true));
    assert_eq!(gate(300, false, blocks_only), (304_087_040, 135_457_280, true));
    assert_eq!(gate(10, true, blocks_only), (4_014_080, 3_973_024, true));
    assert_eq!(gate(9, true, blocks_only), (3_612_672, 3_811_296, false));
    assert_eq!(gate(9, false, blocks_only), (4_128_768, 4_013_456, true));
    assert_eq!(gate(8, false, blocks_only), (3_670_016, 3_811_296, false));
    let relaying = 300_000_000;
    assert_eq!(gate(300, true, relaying), (265_027_584, 256_256_512, true));
    assert_eq!(gate(300, false, relaying), (304_087_040, 273_000_960, true));
    assert_eq!(gate(281, true, relaying), (247_595_008, 246_688_256, true));
    assert_eq!(gate(280, true, relaying), (246_677_504, 246_688_256, false));
    assert_eq!(gate(246, false, relaying), (247_463_936, 246_688_256, true));
    assert_eq!(gate(245, false, relaying), (246_415_360, 246_688_256, false));
}

/// A world with rdashd's budget at `-dbcache=<dbcache>`, `-txindex` (Dash's default) and its default
/// 5 MB `-maxmempool` (DEC-322).
fn node_world(dbcache: i64) -> World {
    use crate::caches::{CacheSizes, IndexArgs};

    let args = IndexArgs {
        txindex: true,
        ..IndexArgs::default()
    };
    World::new(CoinsBudget::node(&CacheSizes::calculate(dbcache, &args), 5_000_000))
}

/// The level-3 figure a VerifyDB starting at this tip compares with the share at its first block.
fn gate(cache: &CoinsCache<ProbeEngine>) -> u64 {
    cache.verify_usage(&CoinsCache::scratch(cache, None))
}

/// Block `height` as V connects one, through an overlay: one transaction of `outputs` outputs to an
/// empty script, 9 bytes each.
fn connect(w: &mut World, height: u32, outputs: u32) {
    let out = output(1_000);
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    let mut ov = BlockOverlay::new(&mut w.cache);
    for index in 0..outputs {
        let k = OutPointKey::new(&dash_consensus::primitives::OutPoint::new(hash.into(), index));
        let src = CoinSrc::Output {
            out: &out,
            height,
            coinbase: false,
        };
        ov.add(&k, src, false).unwrap();
    }
    ov.commit(block(height)).unwrap();
}

/// DEC-1027 (2), (3): rdashd's cold window above the threshold. At `-dbcache=10` startup's figure is
/// C(x) + P = 3,973,024, 41,056 within S. One block of 2,501 outputs, far from any trigger (its
/// boundary is OK), leaves the block overlay's emptied tables as the spare, counted in the ledger
/// (up to 64 shards of 64 buckets, 302,592 bytes): with active's charge unchanged at its ceiling,
/// the figure is over S before any freeze, so level 3 would skip.
#[test]
fn above_the_threshold_one_block_s_spare_overlay_can_skip_level_3_before_any_freeze() {
    let mut w = node_world(10);
    let s = w.cache.budget().coins_share();
    let cold = gate(&w.cache);
    assert_eq!((s, cold), (4_014_080, 3_973_024));
    let ceiling = w.cache.active().charged_bytes();
    connect(&mut w, 1, 2_501);
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Ok);
    assert_eq!(w.cache.active().charged_bytes(), ceiling);
    let spare = w.cache.usage().overlay;
    assert!((40_960..=302_592).contains(&spare), "{spare}");
    assert_eq!(gate(&w.cache), cold + spare);
    assert!(gate(&w.cache) > s);
}

/// DEC-1027 (4), O2-2: below the threshold (`-dbcache=9`: the cold figure 3,811,296 is over
/// S = 3,612,672) a running node can run level 3 with coins cached. One block of 60,000 outputs
/// (0.54 MB) grows active's shards past their ceilings, to a charge of 7,490,048, over f·B: its
/// boundary is THRESHOLD and the node freezes, the figure over S until then and with F in flight.
/// `freeze` sizes the next active beside F's charge, `presize(B, F)`, here the smallest, and
/// `release` keeps it: with F released the figure is that ceiling, the block overlay's emptied
/// tables and the scratch layer's, 1,199,616 + 40,960 + 40,960 = 1,281,536, within S, and 100 more
/// coins leave it there. (A block of 40,000 does not do it: it leaves active at its ceiling, under
/// every trigger, and the THRESHOLD freeze that follows sizes the next active at the same ceiling.)
#[test]
fn below_the_threshold_level_3_runs_after_a_large_block_s_freeze_and_release() {
    use crate::coins::budget::MIN_PRESIZE;
    use crate::coins::layer::ShardSizes;

    let mut w = node_world(9);
    let s = w.cache.budget().coins_share();
    assert_eq!((s, gate(&w.cache)), (3_612_672, 3_811_296));
    connect(&mut w, 1, 60_000);
    assert_eq!(w.cache.active().charged_bytes(), 7_490_048);
    assert!(gate(&w.cache) > s, "before the freeze");
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Threshold);
    let f = w.cache.freeze(0);
    assert!(gate(&w.cache) > s, "F in flight");
    w.release(f);
    assert_eq!(
        w.cache.active().sizes(),
        ShardSizes {
            k: 8,
            s: 0
        }
    );
    assert_eq!(gate(&w.cache), MIN_PRESIZE + 40_960 + 40_960);
    assert_eq!(gate(&w.cache), 1_281_536);
    connect(&mut w, 2, 100);
    assert!(gate(&w.cache) <= s, "{}", gate(&w.cache));
}

/// DEC-1027 (4), O2-1: above the threshold, past the first freeze, the figure follows flush timing.
/// At `-dbcache=32` (cold figure 10,045,952, S = 19,136,512) blocks of 2,501 outputs fill active to
/// its first THRESHOLD, the figure within S at every block before it. Frozen there, with F in
/// flight the figure is F's charge, the new active's ceiling, R, the spare and the scratch layer:
/// over S, so level 3 would skip. After the release it is the new active's ceiling and the rest:
/// within S again.
#[test]
fn above_the_threshold_level_3_skips_while_the_first_freeze_is_in_flight() {
    let mut w = node_world(32);
    let s = w.cache.budget().coins_share();
    assert_eq!((s, gate(&w.cache)), (19_136_512, 10_045_952));
    let mut height = 0;
    while w.cache.boundary_state(0, false) != CoinsBoundary::Threshold {
        height += 1;
        assert!(height <= 60, "no THRESHOLD by {height}");
        connect(&mut w, height, 2_501);
        assert!(gate(&w.cache) <= s, "{height}: cold, within S");
    }
    let charge = w.cache.active().charged_bytes();
    let f = w.cache.freeze(0);
    let u = w.cache.usage();
    assert_eq!(u.reserve, w.cache.budget().reserve(true));
    assert_eq!(u.frozen + u.unbacked - w.cache.active().unbacked_bytes(), charge);
    let in_flight = gate(&w.cache);
    assert!(in_flight > s, "{in_flight}");
    w.release(f);
    let after = gate(&w.cache);
    assert!(after <= s, "{after}");
    assert_eq!(in_flight - after, charge + w.cache.budget().reserve(true));
}
