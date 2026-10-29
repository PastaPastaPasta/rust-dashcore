//! SB5 F5b over SB4's SPV chain: the cold cache behind the shared seams (§5.2), stage 5's
//! invalidation, and snapshot heights below the window and through cold entries (§5.3), a
//! lagging reader's included.

use std::sync::Arc;

use dash_consensus::primitives::Header;
use dash_params::{ChainParams, NetworkKind};
use dash_primitives::BlockHash;

use super::super::spv_tests::{
    ANCHOR, Spv, T0, anchored, anchored_with, mine_n, regtest_pack, resident, step,
};
use crate::cold::ColdCache;
use crate::error::{ChainError, FailStop as _};
use crate::feed::{ChainSnapshot as _, FeedSnapshot};
use crate::mem_store::{Corrupt, Faults, MemHeaderStore, ReadKind};
use crate::testing::{Latch, params, services};
use crate::{
    AnchorPack, AnchorState, BlockId, CHAIN_SCHEMA, ChainConfig, ChainMeta, ClPersisted,
    HeaderChain, NoStore, Services, Status,
};

/// The anchored chain's floor: the pack's first context entry (SB4 D-C4-12).
const FLOOR: u32 = ANCHOR - 24;

fn cache(chain: &Spv) -> &ColdCache {
    chain.shared.cold.as_ref().expect("SPV has a cold cache")
}

fn reads(store: &MemHeaderStore, kind: ReadKind) -> u64 {
    store.faults(|f| f.reads(kind))
}

/// The pack's and `blocks`' `(hash, height)`, from the floor up.
fn chain_of(pack: &AnchorPack, blocks: &[Header]) -> Vec<(BlockHash, u32)> {
    let pack = pack.headers.iter().map(|h| Header::new(*h).hash());
    pack.chain(blocks.iter().map(Header::hash)).zip(FLOOR..).collect()
}

/// Every height `lo..=hi` one at a time.
fn heights(s: &FeedSnapshot, lo: u32, hi: u32) -> Vec<(BlockHash, u32)> {
    let at = |h| s.at_height_checked(h).expect("read").expect("on the snapshot's chain");
    (lo..=hi).map(at).map(|r| (r.hash, r.height)).collect()
}

/// `range(0, u32::MAX)`: every height the snapshot shows.
fn whole(s: &FeedSnapshot) -> Vec<(BlockHash, u32)> {
    s.range(0, u32::MAX).expect("read").iter().map(|r| (r.hash, r.height)).collect()
}

/// [`anchored`] at window 10 over services whose fail-stop is the returned latch.
fn latched(p: &Arc<ChainParams>, pack: &AnchorPack) -> (Arc<Latch>, Spv, MemHeaderStore) {
    let latch = Arc::new(Latch::default());
    let svc = Services {
        fail: latch.clone(),
        ..services(p)
    };
    let (chain, store) = anchored_with(p, pack, 10, svc);
    (latch, chain, store)
}

fn mark(chain: &mut Spv, id: BlockId) -> Status {
    let marked = |s: Status| Status(s.bits() | Status::CONFLICT_CHAINLOCK);
    chain.set_status(id, marked).expect("mark");
    chain.writer_status(id).expect("the writer reads its overlay")
}

/// §5.2: SPV's cold reads hit the cache after the first, by id and by hash (D43): a cold
/// record's parent, resolved by hash, included. The node keeps no cache.
#[test]
fn cold_reads_go_through_the_cache() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    step(&mut chain, &mut store, &trunk);
    let cold = BlockId(26);
    assert!(!resident(&chain, cold));
    cache(&chain).clear();
    let total = |s: &MemHeaderStore| reads(s, ReadKind::ById) + reads(s, ReadKind::ByHash);

    let before = total(&store);
    let e = chain.reader().try_entry(cold).expect("read").expect("held");
    assert_eq!(total(&store), before + 2, "the record by id, its parent by hash");
    assert_eq!(cache(&chain).peek(cold).map(|r| r.hash), Some(e.hash));
    assert!(cache(&chain).peek(BlockId(25)).is_some(), "the parent, cached by its id");
    assert_eq!(chain.reader().try_entry(cold).expect("read").map(|e| e.hash), Some(e.hash));
    assert_eq!(total(&store), before + 2, "a hit reads nothing");

    let id = chain.reader().try_lookup(&trunk[3].hash()).expect("read");
    assert_eq!(id, Some(BlockId(28)));
    assert_eq!(total(&store), before + 3);
    assert!(cache(&chain).peek(BlockId(28)).is_some(), "cached by its id");
    assert_eq!(chain.reader().try_lookup(&trunk[3].hash()), Ok(Some(BlockId(28))));
    assert_eq!(total(&store), before + 3, "a hit by hash");

    let node = HeaderChain::<NoStore>::new_genesis(
        p.clone(),
        ChainConfig::node(&p),
        services(&p),
        NoStore,
    )
    .expect("a valid configuration");
    assert!(node.shared.cold.is_none());
}

/// SB5-A5 through the seam: stage 5 drops the step's written and marked records, so a cold
/// mark is read back committed; a step with more ids than `cold_cache` clears the cache.
#[test]
fn stage5_drops_the_steps_records() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    step(&mut chain, &mut store, &trunk[..20]);
    let cold = BlockId(26);
    let before = chain.reader().try_entry(cold).expect("read").expect("held").status;
    assert!(cache(&chain).peek(cold).is_some());

    let marked = mark(&mut chain, cold);
    assert_ne!(marked, before);
    let other = BlockId(27);
    chain.reader().try_entry(other).expect("read");
    step(&mut chain, &mut store, &trunk[20..22]);
    assert!(cache(&chain).peek(cold).is_none(), "the marked record is dropped");
    assert!(cache(&chain).peek(other).is_some(), "an untouched one stays");
    assert_eq!(chain.reader().try_entry(cold).expect("read").expect("held").status, marked);

    chain.cfg.cold_cache = 2;
    step(&mut chain, &mut store, &trunk[22..25]);
    assert_eq!(cache(&chain).len(), 0, "three written ids, cap 2: cleared");
}

/// A failed cold read trips the latch and caches nothing; the next read loads the record.
#[test]
fn a_failed_cold_read_caches_nothing() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (latch, mut chain, mut store) = latched(&p, &pack);
    step(&mut chain, &mut store, &mine_n(&p, pack.hash, T0 + 150 * 25, 30));
    cache(&chain).clear();
    store.faults(|f| f.fail_read(ReadKind::ById, 1));
    let cold = BlockId(26);
    assert!(matches!(chain.reader().try_entry(cold), Err(ChainError::Storage(_))));
    assert!(latch.is_tripped());
    assert_eq!(cache(&chain).len(), 0);
    assert!(chain.reader().try_entry(cold).expect("read").is_some());
    assert!(cache(&chain).peek(cold).is_some());
}

/// §5.3 over an SPV chain whose window left 1,114 entries cold: every height and the whole
/// range answer the active chain, from the floor; nothing below the floor or above the tip.
/// The range reads the cold run in two `walk_down` batches past the cache. With the view
/// windowed at tip − 10, the heights below go through `ancestor`, with the same answers.
#[test]
fn snapshot_heights_through_cold_entries() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 1_100);
    for c in trunk.chunks(100) {
        step(&mut chain, &mut store, c);
    }
    let tip = ANCHOR + 1_100;
    let want = chain_of(&pack, &trunk);
    let s = chain.feeds().feed_snapshot();
    assert_eq!(s.tip().height, tip);
    assert!(!resident(&chain, BlockId(1_000)));

    cache(&chain).clear();
    let (walks, by_id) = (reads(&store, ReadKind::WalkDown), reads(&store, ReadKind::ById));
    assert_eq!(whole(&s), want);
    assert_eq!(reads(&store, ReadKind::WalkDown), walks + 2, "1,114 cold records: 1,024 + 90");
    assert_eq!(reads(&store, ReadKind::ById), by_id, "the range reads nothing through the cache");
    assert_eq!(cache(&chain).len(), 0);
    let mid = s.range(ANCHOR, ANCHOR + 2).expect("read");
    assert_eq!(mid.iter().map(|r| r.height).collect::<Vec<_>>(), [ANCHOR, ANCHOR + 1, ANCHOR + 2]);
    assert_eq!(s.range(tip + 1, u32::MAX), Ok(Vec::new()));
    assert_eq!(s.range(0, FLOOR - 1), Ok(Vec::new()));

    assert_eq!(heights(&s, FLOOR, tip), want);
    assert_eq!(s.at_height_checked(FLOOR - 1), Ok(None));
    assert_eq!(s.at_height_checked(tip + 1), Ok(None));
    assert_eq!(s.at_height(FLOOR).map(|r| r.id), Some(BlockId(0)));

    let w = s.windowed(tip - 10);
    assert_eq!(heights(&w, FLOOR, tip), want);
    assert_eq!(whole(&w), want);
    assert_eq!(w.range(ANCHOR, ANCHOR + 2), Ok(mid), "a range wholly below the window");
    assert_eq!(w.at_height_checked(FLOOR - 1), Ok(None));
}

/// A chain from genesis has floor 0: a range down to a cold genesis ends there.
#[test]
fn range_reaches_a_cold_genesis() {
    let p = params(NetworkKind::Regtest);
    let mut store = MemHeaderStore::new();
    let mut chain =
        Spv::new_genesis(p.clone(), ChainConfig::spv(&p, 10), services(&p), store.clone())
            .expect("genesis");
    let genesis = Header::new(p.genesis.header);
    store
        .reset(ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor: (genesis.hash(), 0),
            anchor_state: AnchorState::Candidate,
            next_id: BlockId(0),
            tip: BlockId(0),
            best_header: BlockId(0),
            sync_seq: 0,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        })
        .expect("reset");
    let trunk = mine_n(&p, genesis.hash(), T0, 30);
    step(&mut chain, &mut store, &trunk);
    assert!(!resident(&chain, BlockId(0)));
    let want: Vec<_> =
        std::iter::once(genesis.hash()).chain(trunk.iter().map(Header::hash)).zip(0..).collect();
    let s = chain.feeds().feed_snapshot();
    assert_eq!(whole(&s), want);
    assert_eq!(heights(&s, 0, 30), want);
    assert_eq!(whole(&s.windowed(25)), want);
}

/// §5.4: a failed cold read under a snapshot height or range returns `Storage` and trips the
/// latch, and so does a short `walk_down` (a stored record whose parent is not found);
/// `ChainSnapshot::at_height` maps the error to `None`. Each case on a fresh latch.
#[test]
fn snapshot_cold_errors_trip_the_latch() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 30);
    let fresh = |fault: fn(&mut Faults)| {
        let (latch, mut chain, mut store) = latched(&p, &pack);
        step(&mut chain, &mut store, &trunk);
        let s = chain.feeds().feed_snapshot();
        cache(&chain).clear();
        store.faults(fault);
        (latch, s)
    };

    let (latch, s) = fresh(|f| f.fail_read(ReadKind::ById, 1));
    assert!(matches!(s.at_height_checked(ANCHOR + 1), Err(ChainError::Storage(_))));
    assert!(latch.is_tripped());
    assert_eq!(s.at_height(ANCHOR + 1).map(|r| r.height), Some(ANCHOR + 1), "it failed once");

    let (latch, s) = fresh(|f| f.fail_read(ReadKind::ById, 1));
    assert_eq!(s.windowed(ANCHOR + 20).at_height(ANCHOR + 1), None, "through `ancestor`");
    assert!(latch.is_tripped());

    let faults: [fn(&mut Faults); 2] = [
        |f| f.fail_read(ReadKind::WalkDown, 1),
        |f| f.corrupt(BlockId(30), Corrupt::MissingParent),
    ];
    for fault in faults {
        let (latch, s) = fresh(fault);
        assert!(matches!(s.range(0, u32::MAX), Err(ChainError::Storage(_))));
        assert!(latch.is_tripped());
    }
}

/// §5.3's lagging reader: a snapshot taken before a reorg forking below the window still
/// answers its own chain, whose upper part is now stale and evicted; heights below a window go
/// through `ancestor` over immutable links, never `active_at`. The new snapshot answers the
/// new chain.
#[test]
fn lagging_reader_deep_reorg() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let trunk = mine_n(&p, pack.hash, T0 + 150 * 25, 40);
    for c in trunk.chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let old = chain.feeds().feed_snapshot();
    let branch = mine_n(&p, trunk[4].hash(), T0 + 150 * 30 + 11, 50);
    for c in branch.chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let new = chain.feeds().feed_snapshot();
    assert_eq!(new.tip().hash, branch[49].hash());
    assert!(!resident(&chain, old.tip().id), "the old tip is stale and cold");

    let was = chain_of(&pack, &trunk);
    let mut is = chain_of(&pack, &trunk[..5]);
    is.extend(branch.iter().map(Header::hash).zip(ANCHOR + 6..));
    for (s, want) in [(&old, &was), (&new, &is)] {
        let tip = s.tip().height;
        assert_eq!(heights(s, FLOOR, tip), *want);
        assert_eq!(whole(s), *want);
        let w = s.windowed(tip - 3);
        assert_eq!(heights(&w, FLOOR, tip), *want);
        assert_eq!(whole(&w), *want);
        let below = w.range(FLOOR, tip - 4).expect("read");
        assert_eq!(
            below.iter().map(|r| (r.hash, r.height)).collect::<Vec<_>>(),
            want[..want.len() - 4]
        );
    }
}
