//! SB0 §4.4's directed windowed ≡ unwindowed scenarios (SB5 §10.4), on P2's pair of chains
//! (the windowed one at mobile's E = 256): a
//! stale branch forking below the window, Sol's A9,000/B9,000 with and without a restart in
//! the middle, lagging readers across eviction and a deeper reorg, a 10,000-deep reorg with
//! ChainLocks off, and a cold read error. Then eviction after commit, and SB0 §9.4's
//! durability cut for a subscriber's Durable commit after a Buffered header step.

use std::sync::Arc;

use dash_consensus::primitives::Header;
use dash_params::Network;

use super::p2_tests::{Call, Side, UNWINDOWED, check, describe};
use super::step_tests::{Stage, committed_w, drive, real_step};
use crate::chain::spv_tests::{
    ANCHOR, Spv, T0, anchored_with, mine_n, regtest_pack, resident, step,
};
use crate::error::{ChainError, FailStop as _};
use crate::feed::{ChainSnapshot as _, FeedSnapshot};
use crate::mem_store::ReadKind;
use crate::shared::ColdInvalidation;
use crate::testing::{Latch, batch, params, services};
use crate::{BlockId, HeaderStore as _, Services};

/// The pack's last header is at `T0 + 150 × 24`; header `i` above the anchor at `at(i)`.
fn at(i: usize) -> u32 {
    T0 + 150 * (25 + i as u32)
}

/// `hs` at their heights, the first at `h0`.
fn at_heights(hs: &[Header], h0: u32) -> Vec<(Header, u32)> {
    hs.iter().copied().zip(h0..).collect()
}

/// Every `every`-th of `known`, and its last: what a check at this size compares. A last
/// common ancestor with the tip walks `pprev` down to the fork, as Core's does, so a dense
/// sample of a deep branch costs millions of reads.
fn sample(known: &[(Header, u32)], every: usize) -> Vec<(Header, u32)> {
    let last = known.len() - 1;
    known
        .iter()
        .enumerate()
        .filter(|(i, _)| i.is_multiple_of(every) || *i == last)
        .map(|(_, k)| *k)
        .collect()
}

/// Both chains take `hs` and the step's calls, with the same outcomes.
fn both(a: &mut Side, b: &mut Side, hs: &[Header], calls: &[Call]) {
    assert_eq!(a.step(&[hs.to_vec()], calls), b.step(&[hs.to_vec()], calls), "outcomes");
}

fn tip_hash(s: &Side) -> dash_primitives::BlockHash {
    s.chain.private_tip().expect("a tip").0.hash
}

/// `w_stale_below_floor`: a stale branch that forks below the window (w = 10) and is extended
/// later resolves its ancestry through the records: its ancestor at its first height is its own
/// first header, not the active chain's at that height.
#[test]
fn w_stale_below_floor() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut a, mut b) = (Side::new(&p, &pack, UNWINDOWED, 1_024), Side::new(&p, &pack, 10, 256));
    let trunk = mine_n(&p, pack.hash, at(0), 50);
    let stale = mine_n(&p, trunk[5].hash(), at(6) + 7, 8);
    let mut known = at_heights(&trunk, ANCHOR + 1);
    known.extend(at_heights(&stale, ANCHOR + 7));
    for c in trunk[..40].chunks(10) {
        both(&mut a, &mut b, c, &[]);
    }
    for hs in [&stale[..3], &trunk[40..], &stale[3..]] {
        both(&mut a, &mut b, hs, &[]);
        check(&a, &b, &known, 1, "stale");
    }
    let r = b.chain.reader();
    let end = b.id(&stale[7].hash()).expect("held");
    let first = b.id(&stale[0].hash()).expect("held");
    assert!(!resident(&b.chain, first), "the branch's first header is cold");
    assert_eq!(r.try_ancestor(end, ANCHOR + 7).expect("read"), Some(first));
    assert_eq!(tip_hash(&b), trunk[49].hash(), "the branch stays stale");
    let tips = b.chain.chain_tips().expect("tips");
    let t = tips.iter().find(|t| t.id == end).expect("the branch is a tip");
    assert_eq!(t.fork_point, b.id(&trunk[5].hash()).expect("held"));
}

/// Sol's A9,000/B9,000 at w = 2,048: A grows to 9,000 above the anchor; B forks at A's 101st
/// header, far below the window, and is extended in batches of 1,500 until it wins by work in
/// the last one. With `restart`, both chains reopen halfway through B, with the same ids.
fn a9000_b9000(restart: bool) {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut a, mut b) =
        (Side::new(&p, &pack, UNWINDOWED, 1_024), Side::new(&p, &pack, 2_048, 256));
    let trunk = mine_n(&p, pack.hash, at(0), 9_000);
    let branch = mine_n(&p, trunk[100].hash(), at(101) + 7, 9_000);
    let mut known = at_heights(&trunk, ANCHOR + 1);
    known.extend(at_heights(&branch, ANCHOR + 102));
    let known = sample(&known, 997);
    for c in trunk.chunks(1_500) {
        both(&mut a, &mut b, c, &[]);
        check(&a, &b, &known, 997, "A");
    }
    for (i, c) in branch.chunks(1_500).enumerate() {
        if restart && i == 3 {
            let ids: Vec<_> = known.iter().map(|(h, _)| b.id(&h.hash())).collect();
            a.restart(&p);
            b.restart(&p);
            let reopened: Vec<_> = known.iter().map(|(h, _)| b.id(&h.hash())).collect();
            assert_eq!(ids, reopened, "the same ids after the reopen");
            check(&a, &b, &known, 997, "reopened");
        }
        both(&mut a, &mut b, c, &[]);
        check(&a, &b, &known, 997, &format!("B {i}"));
        let won = 1_500 * (i + 1) > 8_899;
        let want = if won {
            branch[8_999].hash()
        } else {
            trunk[8_999].hash()
        };
        assert_eq!(tip_hash(&b), want, "B {i}");
    }
}

#[test]
fn w_a9000_b9000() {
    a9000_b9000(false);
}

#[test]
fn w_restart_mid_extension() {
    a9000_b9000(true);
}

/// `w_lagging_readers` (w = 64): every published snapshot is held to the end, across eviction
/// and a reorg forking 550 below the tip, and still describes its own chain at every height,
/// as the unwindowed chain's does. The first one held after the reorg's start still answers A
/// above the fork.
#[test]
fn w_lagging_readers() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut a, mut b) = (Side::new(&p, &pack, UNWINDOWED, 1_024), Side::new(&p, &pack, 64, 256));
    let trunk = mine_n(&p, pack.hash, at(0), 600);
    let branch = mine_n(&p, trunk[49].hash(), at(50) + 7, 700);
    let mut held: Vec<(FeedSnapshot, FeedSnapshot)> = Vec::new();
    let floor = b.chain.floor;
    for c in trunk.chunks(100).chain(branch.chunks(100)) {
        both(&mut a, &mut b, c, &[]);
        held.push((a.chain.feeds().feed_snapshot(), b.chain.feeds().feed_snapshot()));
        for (sa, sb) in &held {
            assert_eq!(describe(sa, floor, 1), describe(sb, floor, 1));
        }
    }
    assert_eq!(tip_hash(&b), branch[699].hash());
    let (_, a600) = &held[5];
    assert_eq!(a600.tip().hash, trunk[599].hash());
    let h = ANCHOR + 300;
    assert_eq!(a600.at_height(h).map(|r| r.hash), Some(trunk[299].hash()), "A, not B");
    assert!(!resident(&b.chain, a600.tip().id), "A's tip is stale and cold");
}

/// `w_reorg_10k_no_cl` at the default w = 10,000: A grows to 10,500; B forks at A's 50th header
/// with ChainLocks off and wins after 10,500, a reorg 10,450 deep.
#[test]
fn w_reorg_10k_no_cl() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut a, mut b) =
        (Side::new(&p, &pack, UNWINDOWED, 1_024), Side::new(&p, &pack, 10_000, 256));
    let trunk = mine_n(&p, pack.hash, at(0), 10_500);
    let branch = mine_n(&p, trunk[49].hash(), at(50) + 7, 10_500);
    let mut known = at_heights(&trunk, ANCHOR + 1);
    known.extend(at_heights(&branch, ANCHOR + 51));
    let known = sample(&known, 997);
    both(&mut a, &mut b, &[], &[Call::Spork(false)]);
    for c in trunk.chunks(2_000).chain(branch.chunks(2_000)) {
        both(&mut a, &mut b, c, &[]);
        check(&a, &b, &known, 997, "10k");
    }
    assert_eq!(tip_hash(&b), branch[10_499].hash());
    let fork = b.id(&trunk[49].hash()).expect("held");
    assert!(!resident(&b.chain, fork), "the fork is 10,500 below the tip, cold");
    assert_eq!(b.chain.snapshot().cl_floor, None);
}

/// `w_cold_read_error`: a reorg forking below the window (w = 10) needs the fork's record; its
/// read fails, so stage 1 returns `ChainError::Storage` with the latch tripped, and nothing is
/// published.
#[test]
fn w_cold_read_error() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let latch = Arc::new(Latch::default());
    let svc = Services {
        fail: latch.clone(),
        ..services(&p)
    };
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let trunk = mine_n(&p, pack.hash, at(0), 40);
    for c in trunk.chunks(10) {
        step(&mut chain, &mut store, c);
    }
    let branch = mine_n(&p, trunk[4].hash(), at(5) + 7, 50);
    chain.accept(batch(&branch), None).expect("accept");
    chain.shared.invalidate_cold(ColdInvalidation::Clear);
    store.faults(|f| f.fail_read(ReadKind::ById, 1));
    let before = chain.snapshot();
    let vt = chain.virtual_tip();
    let r = chain.fork_target(&vt).and_then(|t| chain.tip_plan(t.id));
    assert!(matches!(r, Err(ChainError::Storage(_))), "{r:?}");
    assert!(latch.is_tripped());
    assert!(Arc::ptr_eq(&chain.snapshot(), &before), "nothing is published");
    assert_eq!(chain.feeds().feed_snapshot().tip().hash, trunk[39].hash());
}

/// `evict_after_commit_spv` (SB0 §4.4 m2): a batch of 12 at w = 4 evicts nothing at `accept`
/// or at the commit (the pack's first step already evicted its low context); stage 4 evicts
/// every entry below `tip − w`, each already in the store.
#[test]
fn evict_after_commit_spv() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = committed_w(&p, &pack, services(&p), 4);
    let trunk = mine_n(&p, pack.hash, at(0), 12);
    let first = chain.next_id;
    let held: Vec<_> = (0..first).map(BlockId).filter(|&id| resident(&chain, id)).collect();
    chain.accept(batch(&trunk), None).expect("accept");
    let ids: Vec<_> = (0..chain.next_id).map(BlockId).collect();
    let new = &ids[first as usize..];
    assert_eq!(new.len(), 12);
    let live = |c: &Spv| held.iter().chain(new).all(|&id| resident(c, id));
    assert!(live(&chain), "accept evicts nothing");
    let (_, target) = drive(&mut chain, &mut store, |_, _| {}, Stage::Commit).expect("the step");
    assert!(live(&chain), "the commit evicts nothing");
    let to = chain.block_ref_of(target.id).expect("held");
    chain.activate_segmented(to, &mut || {}).expect("stage 4");
    chain.records_committed();
    let tip = ANCHOR + 12;
    let reader = chain.reader();
    for &id in &ids {
        let rec = store.by_id(id).expect("read").expect("committed before stage 4");
        assert_eq!(resident(&chain, id), rec.height >= tip - 4, "{id:?} at {}", rec.height);
        let e = reader.try_entry(id).expect("read").expect("resolves");
        assert_eq!((e.hash, e.height), (rec.hash, rec.height));
    }
    assert!(new.iter().any(|&id| !resident(&chain, id)), "stage 4 evicted part of the batch");
}

/// `durable_subscriber_after_buffered_header` (Sol r1): a Buffered header step, then another
/// writer's Durable commit on the same WAL (`sync_wal` stands in for the subscriber's), then
/// power loss. The header records survive, as a sequential WAL keeps every commit before a
/// durable one, so the subscriber's locator resolves at `open`. Without the Durable commit,
/// the step is lost.
#[test]
fn durable_subscriber_after_buffered_header() {
    let p = params(Network::Regtest);
    let pack = regtest_pack(&p);
    for durable in [true, false] {
        let (mut chain, mut store) = committed_w(&p, &pack, services(&p), 10);
        store.sync_wal().expect("the pack is durable");
        let trunk = mine_n(&p, pack.hash, at(0), 20);
        real_step(&mut chain, &mut store, &trunk);
        let tip = chain.private_tip().expect("a tip").0.id;
        let locator = chain.reader().locator(Some(tip));
        if durable {
            store.sync_wal().expect("the subscriber's commit");
        }
        let store = store.power_loss();
        let cfg = crate::ChainConfig::spv(&p, 10);
        let reopened = Spv::open(p.clone(), cfg, services(&p), store).expect("open");
        let r = reopened.reader();
        let found = locator.iter().filter(|h| r.try_lookup(h).expect("read").is_some()).count();
        let tip_hash = reopened.private_tip().expect("a tip").0.hash;
        if durable {
            assert_eq!(found, locator.len(), "every locator entry resolves");
            assert_eq!(tip_hash, trunk[19].hash());
        } else {
            assert_eq!(r.try_lookup(&trunk[19].hash()).expect("read"), None, "the step is lost");
            assert_eq!(tip_hash, pack.hash);
        }
    }
}
