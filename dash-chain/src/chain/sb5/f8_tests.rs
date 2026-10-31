//! SB5 §4.4 F8, the producer part (§10.1's F8 row): a published snapshot describes one chain
//! state at every height, including after later steps and reorgs; a delivered event's block
//! resolves by id after eviction; nothing answers below the anchored chain's floor. n4's
//! `open_rejects_stored_id_at_next_id` is `open::tests::open_refuses_inconsistent_meta_and_rows`.

use dash_params::NetworkKind;

use super::hold_tests::{subscribe, take};
use super::step_tests::{committed_w, real_step};
use crate::BlockId;
use crate::chain::spv_tests::{ANCHOR, Spv, T0, mine_n, regtest_pack};
use crate::feed::{ChainSnapshot, FeedSnapshot};
use crate::mem_store::{MemHeaderStore, ReadKind};
use crate::testing::{params, services};

/// Header `i` of the trunk is mined at `at(i)`.
fn at(i: usize) -> u32 {
    T0 + 150 * (i as u32 + 26)
}

/// The resolved `at_height` answers of `s` from the floor to its tip: each must be the tip's
/// ancestor at that height, by immutable ancestry through `chain`'s reader, with its own hash
/// and height; above the tip, nothing.
fn check_coherent(s: &FeedSnapshot, chain: &Spv, what: &str) {
    let reader = chain.reader();
    let tip = s.tip();
    for h in chain.floor..=tip.height {
        let b = s.at_height(h).unwrap_or_else(|| panic!("{what}: no block at {h}"));
        let want = reader.try_ancestor(tip.id, h).expect("read").expect("an ancestor");
        assert_eq!(b.id, want, "{what}: at_height({h}) is the tip's ancestor");
        let e = reader.try_entry(b.id).expect("read").expect("held");
        assert_eq!((e.hash, e.height), (b.hash, b.height), "{what}: at {h}");
    }
    assert_eq!(s.at_height(tip.height + 1), None, "{what}: above the tip");
}

/// `f8_snapshot_coherent`: `{tip, tip_seq, at_height}` describe one chain state. Each published
/// snapshot's `tip_seq` is its step's last event's `seq`, and every retained snapshot, rechecked
/// after every later step, answers its own chain at every height: through a deep reorg forking
/// below the window (w = 10) and back.
#[test]
fn f8_snapshot_coherent() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed_w(&p, &pack, services(&p), 10);
    let feeds = chain.feeds();
    let (_, mut f) = subscribe(&feeds);
    let trunk = mine_n(&p, pack.hash, at(0), 30);
    // Forks at trunk[4], below the window once the trunk is in, and outgrows it; then the trunk
    // grows back past it.
    let side = mine_n(&p, trunk[4].hash(), at(5) + 7, 40);
    let back = mine_n(&p, trunk[29].hash(), at(30), 20);
    let steps: Vec<&[_]> =
        vec![&trunk[..10], &trunk[10..20], &trunk[20..], &side[..28], &side[28..], &back];
    let mut retained: Vec<FeedSnapshot> = Vec::new();
    for (k, headers) in steps.into_iter().enumerate() {
        real_step(&mut chain, &mut store, headers);
        let s = feeds.feed_snapshot();
        let events = take(&mut *f);
        let last = events.last().expect("the step moved the tip");
        assert_eq!(s.tip_seq(), last.seq(), "step {k}: tip_seq is the step's last event");
        assert_eq!(s.tip().id, chain.private_tip().expect("a tip").0.id, "step {k}");
        assert_eq!(s.tip().hash, headers[headers.len() - 1].hash(), "step {k} took the tip");
        retained.push(s);
        for (j, r) in retained.iter().enumerate() {
            check_coherent(r, &chain, &format!("snapshot {j} after step {k}"));
        }
    }
}

/// `f8_cold_delivered_id_resolves`: a subscriber that lags while its events' blocks leave the
/// window (w = 4) resolves each delivered block by id, cold, with the event's hash and height
/// (the step committed before stage 4 published it, SB0 §9.5).
#[test]
fn f8_cold_delivered_id_resolves() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed_w(&p, &pack, services(&p), 4);
    let (_, mut f) = subscribe(&chain.feeds());
    let trunk = mine_n(&p, pack.hash, at(0), 24);
    for c in trunk.chunks(3) {
        real_step(&mut chain, &mut store, c);
    }
    let events = take(&mut *f);
    assert_eq!(events.len(), trunk.len());
    let reads = |s: &MemHeaderStore| s.faults(|f| f.reads(ReadKind::ById));
    let before = reads(&store);
    let reader = chain.reader();
    let mut cold = 0;
    for e in &events {
        let b = e.block();
        let resident = chain.shared.arena.imm(b.id, |_| ()).is_some();
        cold += usize::from(!resident);
        let entry = reader.try_entry(b.id).expect("read").expect("resolves");
        assert_eq!((entry.hash, entry.height), (b.hash, b.height), "{:?}", b.id);
    }
    assert!(cold >= trunk.len() - 5, "{cold} delivered blocks left the window");
    assert!(reads(&store) > before, "cold blocks resolve from the store");
}

/// `snapshot_below_anchor_is_none`: an anchored chain holds nothing below its floor (the pack's
/// first context header, `ANCHOR − 24`); the snapshot answers none of those heights, nor does a
/// range reaching below the floor, which starts at it.
#[test]
fn snapshot_below_anchor_is_none() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed_w(&p, &pack, services(&p), 4);
    real_step(&mut chain, &mut store, &mine_n(&p, pack.hash, at(0), 12));
    let s = chain.feeds().feed_snapshot();
    let floor = chain.floor;
    assert_eq!(floor, ANCHOR - 24);
    for h in [0, 1, floor - 25, floor - 1] {
        assert_eq!(s.at_height(h), None, "{h}");
        assert_eq!(s.at_height_checked(h).expect("no read fails"), None, "{h}");
    }
    let below = s.range(floor - 10, floor + 1).expect("a range");
    assert_eq!(below.iter().map(|b| b.height).collect::<Vec<_>>(), [floor, floor + 1]);
    assert_eq!(s.at_height(floor).map(|b| b.id), Some(BlockId(0)), "the floor is the lowest id");
}
