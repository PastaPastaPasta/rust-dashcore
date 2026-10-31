//! The SPV store's children and leaves, read a page at a time (§6.2): a wide tree's
//! enumeration holds one fixed page, merges the step's uncommitted entries in id order, and
//! refuses a page out of id order as corruption.

use std::sync::Arc;

use dash_consensus::primitives::Header;
use dash_params::NetworkKind;

use super::super::spv_tests::{
    ANCHOR, Spv, T0, anchored, anchored_with, mine_n, regtest_pack, resident, step,
};
use super::super::*;
use crate::error::FailStop as _;
use crate::mem_store::{Corrupt, MemHeaderStore, ReadKind};
use crate::testing::{Latch, batch, params, services};

/// `n` one-header siblings on `prev`, the `k`-th mined `k` s after `T0`'s slot past the pack.
fn siblings(p: &ChainParams, prev: BlockHash, ks: std::ops::Range<u32>) -> Vec<Header> {
    ks.map(|k| mine_n(p, prev, T0 + 150 * 25 + k, 1)[0]).collect()
}

fn id_of(chain: &Spv, h: &Header) -> BlockId {
    chain.reader().try_lookup(&h.hash()).expect("read").expect("held")
}

/// 150 committed siblings on the anchor span three pages (64, 64, 22); three more accepted in
/// the step and one committed sibling also dirty join them once each, in id order.
#[test]
fn children_merge_pages_and_the_step() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let committed = siblings(&p, pack.hash, 0..150);
    step(&mut chain, &mut store, &committed);
    let late = siblings(&p, pack.hash, 150..153);
    chain.accept(batch(&late), None).expect("accept");
    let anchor = chain.reader().try_lookup(&pack.hash).expect("read").expect("held");
    let mut want: Vec<_> = committed.iter().chain(&late).map(|h| id_of(&chain, h)).collect();
    want.sort_unstable();
    chain.mark_dirty(want[100]);
    let reads = |store: &MemHeaderStore| store.faults(|f| f.reads(ReadKind::Children));
    let before = reads(&store);
    assert_eq!(chain.child_ids(anchor), Ok(want));
    assert_eq!(reads(&store) - before, 4, "two full pages, a short one and the empty end");
    store.faults(|f| f.short_pages(10));
    let want = chain.child_ids(anchor).expect("children");
    assert_eq!(want.len(), 153, "short pages before the end lose none");
    assert_eq!(reads(&store) - before, 4 + 16, "the 150 stored in 15 pages of 10, then the end");
}

/// Exactly 128 committed siblings, two full pages: the third read is the empty end.
#[test]
fn children_end_on_a_page_boundary() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let committed = siblings(&p, pack.hash, 0..128);
    step(&mut chain, &mut store, &committed);
    let anchor = chain.reader().try_lookup(&pack.hash).expect("read").expect("held");
    let mut want: Vec<_> = committed.iter().map(|h| id_of(&chain, h)).collect();
    want.sort_unstable();
    let before = store.faults(|f| f.reads(ReadKind::Children));
    assert_eq!(chain.child_ids(anchor), Ok(want));
    assert_eq!(store.faults(|f| f.reads(ReadKind::Children)) - before, 3);
}

/// 150 siblings leave the window as leaves; a header accepted on one of them, not yet
/// committed, takes it out of the cold leaves, though its work still bounds them.
#[test]
fn cold_leaves_page_and_skip_uncommitted_parents() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let (mut chain, mut store) = anchored(&p, &pack, 10);
    let sibs = siblings(&p, pack.hash, 0..150);
    step(&mut chain, &mut store, &sibs);
    step(&mut chain, &mut store, &mine_n(&p, pack.hash, T0 + 150 * 30, 20));
    let ids: Vec<_> = sibs.iter().map(|h| id_of(&chain, h)).collect();
    assert!(ids.iter().all(|&id| !resident(&chain, id)));
    let on = mine_n(&p, sibs[70].hash(), T0 + 150 * 40, 1);
    chain.accept(batch(&on), None).expect("accept on a cold parent");
    let work = chain.node(ids[70]).expect("read").chain_work;
    *chain.max_cold_leaf_work.lock() = Work::ZERO;
    let mut want = ids.clone();
    want.remove(70);
    want.sort_unstable();
    let before = store.faults(|f| f.reads(ReadKind::Leaves));
    assert_eq!(chain.cold_leaf_ids(), Ok(want));
    assert!(store.faults(|f| f.reads(ReadKind::Leaves)) - before >= 3, "read in pages");
    assert_eq!(*chain.max_cold_leaf_work.lock(), work, "every cold leaf bounds the scan");
    store.faults(|f| f.short_pages(7));
    let mut want = ids.clone();
    want.remove(70);
    want.sort_unstable();
    assert_eq!(chain.cold_leaf_ids(), Ok(want), "short pages before the end lose none");
}

/// A page whose ids do not ascend from where it was asked is corruption: the read trips the
/// latch, and the enumeration stops rather than resume from a wrong id.
#[test]
fn a_page_out_of_id_order_is_corruption() {
    let p = params(NetworkKind::Regtest);
    let pack = regtest_pack(&p);
    let latch = Arc::new(Latch::default());
    let svc = Services {
        fail: latch.clone(),
        ..services(&p)
    };
    let (mut chain, mut store) = anchored_with(&p, &pack, 10, svc);
    let sibs = siblings(&p, pack.hash, 0..100);
    step(&mut chain, &mut store, &sibs);
    let anchor = chain.reader().try_lookup(&pack.hash).expect("read").expect("held");
    let mut ids: Vec<_> = sibs.iter().map(|h| id_of(&chain, h)).collect();
    ids.sort_unstable();
    store.faults(|f| f.corrupt(ids[80], Corrupt::Id(ids[3])));
    let e = chain.child_ids(anchor).expect_err("out of order");
    assert!(matches!(e, ChainError::Storage(ref e) if e.is_corrupt()), "{e:?}");
    assert!(latch.is_tripped());
    assert_eq!(chain.private_tip().expect("a tip").0.height, ANCHOR + 1);
}

/// The dirty set's id-order scan, which the children cursor merges with the store's pages:
/// from below its lowest word, inside and between words, and at the top of the id space (a
/// set spans its own words only, so the top is a set of its own).
#[test]
fn dirty_first_from_scans_in_id_order() {
    let mut d = Dirty::default();
    for id in [200, 130, 263] {
        d.insert(BlockId(id));
    }
    let next = |d: &Dirty, from| d.first_from(BlockId(from)).map(|id| id.0);
    assert_eq!(next(&d, 0), Some(130), "from below the lowest word");
    assert_eq!(next(&d, 130), Some(130));
    assert_eq!(next(&d, 131), Some(200));
    assert_eq!(next(&d, 201), Some(263), "across a word");
    assert_eq!(next(&d, 264), None);
    let mut top = Dirty::default();
    top.insert(BlockId(u32::MAX));
    top.insert(BlockId(u32::MAX - 70));
    assert_eq!(next(&top, 0), Some(u32::MAX - 70));
    assert_eq!(next(&top, u32::MAX - 69), Some(u32::MAX));
    assert_eq!(next(&top, u32::MAX), Some(u32::MAX));
    assert_eq!(next(&Dirty::default(), 0), None);
}
