//! MN0-2b-1: [`ChainReader::with_dummy`], Core's `indexDummy` in `TestBlockValidity`.

use dash_primitives::{BlockHash, BlockHeader};

use super::tests::{branch, node};
use super::*;
use crate::{DUMMY_ID, Work};

/// A header on `prev` whose time is `time`; its fields beyond `prev_hash` and `time` are
/// irrelevant to the overlay.
fn header_on(chain: &HeaderChain, prev: BlockId, time: u32) -> BlockHeader {
    let prev_hash = chain.reader().entry(prev).expect("resident").hash;
    BlockHeader {
        prev_hash,
        time,
        ..BlockHeader::default()
    }
}

#[test]
fn the_dummy_is_the_parents_child_and_reaches_its_ancestors() {
    let mut chain = node();
    let trunk = branch(&mut chain, BlockId(0), 1, 30, 1_700_000_000);
    let tip = trunk[trunk.len() - 1];
    let tip_height = chain.reader().entry(tip).expect("held").height;
    let hash = BlockHash::from([0xd5; 32]);
    let reader = chain.reader().with_dummy(hash, header_on(&chain, tip, 1_700_000_100), tip);
    let reader = reader.expect("a resident parent");

    let e = reader.entry(DUMMY_ID).expect("the dummy");
    assert_eq!((e.id, e.hash, e.prev, e.height), (DUMMY_ID, hash, Some(tip), tip_height + 1));
    assert_eq!((e.chain_work, e.n_tx, e.file, e.time_max), (Work::ZERO, 0, None, 0));
    assert_eq!(reader.try_entry(DUMMY_ID).expect("no store read"), Some(e));
    // `GetAncestor` from the dummy walks `pprev` (no skip), to every height.
    for h in 0..=tip_height {
        let want = chain.reader().ancestor(tip, h);
        assert_eq!(reader.ancestor(DUMMY_ID, h), want, "height {h}");
        assert_eq!(reader.try_ancestor(DUMMY_ID, h).expect("resident"), want, "height {h}");
    }
    assert_eq!(reader.ancestor(DUMMY_ID, tip_height + 1), Some(DUMMY_ID));
    assert_eq!(reader.ancestor(DUMMY_ID, tip_height + 2), None);
    // Every other id reads as the chain's.
    for &id in &trunk {
        assert_eq!(reader.entry(id), chain.reader().entry(id));
    }
}

/// `LookupBlockIndex` never finds `indexDummy`: it is not inserted.
#[test]
fn the_dummy_is_not_in_the_hash_index() {
    let mut chain = node();
    let trunk = branch(&mut chain, BlockId(0), 1, 3, 1_700_000_000);
    let tip = trunk[2];
    let hash = BlockHash::from([0xd6; 32]);
    let reader = chain.reader().with_dummy(hash, header_on(&chain, tip, 1_700_000_010), tip);
    let reader = reader.expect("a resident parent");
    assert_eq!(reader.lookup(&hash), None);
    assert_eq!(reader.try_lookup(&hash).expect("no fault"), None);
    assert_eq!(chain.reader().entry(DUMMY_ID), None, "the chain's own reader has no dummy");
    let tip_hash = chain.reader().entry(tip).expect("held").hash;
    assert_eq!(reader.lookup(&tip_hash), Some(tip));
}

/// Core's `GetMedianTimePast` over the dummy's time and its ten ancestors' times.
#[test]
fn the_dummys_median_time_past_is_cores() {
    let mut chain = node();
    let trunk = branch(&mut chain, BlockId(0), 1, 12, 1_700_000_000);
    let tip = trunk[11];
    let mut times: Vec<i64> = vec![1_800_000_000];
    let mut at = Some(tip);
    while let Some(id) = at.filter(|_| times.len() < 11) {
        let e = chain.reader().entry(id).expect("held");
        times.push(i64::from(e.header.time));
        at = e.prev;
    }
    times.sort_unstable();
    let reader = chain.reader().with_dummy(
        BlockHash::from([1; 32]),
        header_on(&chain, tip, 1_800_000_000),
        tip,
    );
    let e = reader.expect("a resident parent").entry(DUMMY_ID).expect("the dummy");
    assert_eq!(e.mtp, times[5]);
    assert_ne!(e.mtp, chain.reader().entry(tip).expect("held").mtp, "its own time counts");
}

#[test]
fn no_dummy_without_a_resident_parent() {
    let chain = node();
    let header = BlockHeader::default();
    assert!(chain.reader().with_dummy(BlockHash::from([2; 32]), header, BlockId(99)).is_none());
}
