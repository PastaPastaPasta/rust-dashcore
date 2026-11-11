//! The evo layer (CH7 D22, D18, D7): merges, tombstones, Core's `WriteDerived` check through
//! every level, the byte count, and the window CH2 takes.

use dash_consensus::ChainAncestry;
use dash_dml::EvoStore as _;
use dash_dml::testkit::MemChain;
use dash_params::LlmqType;
use dashd_store::MemoryBatch;

use super::*;
use crate::evo::codec::{self, EvoCf, index_key, mined_key};
use crate::evo::testkit::{self, credit_data, flush, memory_store, write_b1};
use crate::store::Meta;

/// The index's byte count, recomputed from its entries.
fn recount(index: &EvoIndex) -> u64 {
    let by_hash: u64 =
        index.by_hash.iter().map(|((k, b), v)| (k.key(b).as_bytes().len() + v.len()) as u64).sum();
    let mined: u64 =
        index.mined.iter().map(|(k, v)| (k.len() + v.as_ref().map_or(0, |v| v.len())) as u64).sum();
    by_hash + mined
}

/// A lookup's kind, owned.
fn state(lookup: Lookup<'_>) -> &'static str {
    match lookup {
        Lookup::Live(_) => "live",
        Lookup::Erased => "erased",
        Lookup::Absent => "absent",
    }
}

#[test]
fn commits_merge_and_tombstones_hide_what_is_below() {
    let mut chain = MemChain::new();
    let b4 = chain.extend(chain.genesis(), 4);
    let b3 = chain.prev(b4).unwrap();
    let base = chain.ancestor(b4, 1).unwrap();
    let store = memory_store();
    let mut layer = EvoLayer::new();
    let record = testkit::mined(&chain, LlmqType::Llmq50_60, base, None);
    let mut d3 = testkit::delta(&chain, b3);
    d3.credit_data = Some(credit_data(3));
    store.commit(&mut layer, &d3).unwrap();
    let mut d4 = testkit::delta(&chain, b4);
    d4.mined_commitments = vec![record.clone()];
    store.commit(&mut layer, &d4).unwrap();
    assert_eq!(layer.evo_tip(), Some(chain.hash(b4)));
    assert_eq!(layer.active.bytes(), recount(&layer.active));
    let qkey = mined_key(LlmqType::Llmq50_60, &record.quorum_hash);
    let hkey = index_key(LlmqType::Llmq50_60, None, 4);
    assert!(matches!(layer.active.mined(&qkey), Lookup::Live(_)));

    // Flushed, then undone: the tombstones hide the durable rows from every read.
    flush(&store, &mut layer);
    assert_eq!(
        (layer.active.bytes(), layer.active.evo_tip(), layer.frozen.is_none()),
        (0, None, true)
    );
    store.commit(&mut layer, &testkit::undo(&chain, b4, std::slice::from_ref(&record))).unwrap();
    assert_eq!(layer.evo_tip(), Some(chain.hash(b3)));
    assert_eq!(layer.active.mined(&qkey), Lookup::Erased);
    assert_eq!(layer.active.mined(&hkey), Lookup::Erased);
    assert_eq!(layer.active.bytes(), recount(&layer.active));
    // B1 deletes them.
    flush(&store, &mut layer);
    let (cf, key) = codec::split_mined_key(&qkey);
    assert_eq!(store.db().get(cf.cf(), key).unwrap(), None);
    let (cf, key) = codec::split_mined_key(&hkey);
    assert_eq!(store.db().get(cf.cf(), key).unwrap(), None);
    // The credit data is never erased.
    let cf = EvoCf::CreditData;
    assert!(store.db().get(cf.cf(), cf.key(&chain.hash(b3)).as_bytes()).unwrap().is_some());
    assert_eq!(Meta::read(store.db()).unwrap().evo_tip, Some(chain.hash(b3)), "B1's meta.evo_tip");

    // A reconnect revives a tombstoned row in the active index.
    store.commit(&mut layer, &d4).unwrap();
    store.commit(&mut layer, &testkit::undo(&chain, b4, std::slice::from_ref(&record))).unwrap();
    assert_eq!(layer.active.mined(&qkey), Lookup::Erased);
    store.commit(&mut layer, &d4).unwrap();
    assert!(matches!(layer.active.mined(&qkey), Lookup::Live(_)));
    assert_eq!(layer.active.bytes(), recount(&layer.active));
}

/// Core's `WriteDerived` reads through its transaction to the DB: a differing rewrite meets the
/// row in the active index, in the frozen one and on disk alike, and leaves the layer unchanged;
/// an equal one writes nothing.
#[test]
fn a_differing_rewrite_is_refused_at_every_level() {
    let mut chain = MemChain::new();
    let b1 = chain.extend(chain.genesis(), 1);
    let store = memory_store();
    let mut d = testkit::delta(&chain, b1);
    d.credit_data = Some(credit_data(1));
    let mut differing = d.clone();
    differing.credit_data = Some(credit_data(2));
    let base = chain.genesis();
    let record = testkit::mined(&chain, LlmqType::Llmq50_60, base, None);
    let mut mined = d.clone();
    mined.mined_commitments = vec![record.clone()];
    let mut other = mined.clone();
    other.mined_commitments[0].commitment =
        Arc::new(testkit::commitment(LlmqType::Llmq50_60, record.quorum_hash, 3));

    let qkey = mined_key(LlmqType::Llmq50_60, &record.quorum_hash);
    let check = |layer: &mut EvoLayer| {
        let before = (layer.active.bytes(), layer.evo_tip());
        for bad in [&differing, &other] {
            let e = store.commit(layer, bad).unwrap_err();
            assert!(matches!(e, StoreError::Inconsistent { .. }), "{e}");
            assert_eq!((layer.active.bytes(), layer.evo_tip()), before, "unchanged");
        }
        // An equal rewrite adds no derived row: only the height index's plain write.
        let derived = (layer.active.by_hash.len(), state(layer.active.mined(&qkey)));
        store.commit(layer, &mined).unwrap();
        assert_eq!((layer.active.by_hash.len(), state(layer.active.mined(&qkey))), derived);
    };
    // In the active index.
    let mut layer = EvoLayer::new();
    store.commit(&mut layer, &mined).unwrap();
    check(&mut layer);
    // In the frozen one, its flush in flight.
    let window = layer.take();
    check(&mut layer);
    // On disk.
    write_b1(&store, &window, None);
    layer.release();
    let mut layer = EvoLayer::new();
    check(&mut layer);
    assert!(layer.active.by_hash.is_empty(), "nothing new to write");
    assert_eq!(layer.active.mined(&qkey), Lookup::Absent);
}

/// The window CH2 takes (D22): the active index moves to the frozen one, which reads keep until
/// W's release; the byte count covers both (D18); the best block is the active, then the frozen
/// evo tip (D7).
#[test]
fn a_take_freezes_the_active_index_until_the_release() {
    let mut chain = MemChain::new();
    let b2 = chain.extend(chain.genesis(), 2);
    let b1 = chain.prev(b2).unwrap();
    let store = memory_store();
    let mut layer = EvoLayer::new();
    assert_eq!((layer.bytes(), layer.best_block()), (0, None));
    let mut d1 = testkit::delta(&chain, b1);
    d1.credit_data = Some(credit_data(1));
    store.commit(&mut layer, &d1).unwrap();
    let frozen_bytes = layer.bytes();
    assert!(frozen_bytes > 0);
    let window = layer.take();
    assert_eq!((window.bytes(), window.index().evo_tip()), (frozen_bytes, Some(chain.hash(b1))));
    assert_eq!(layer.best_block(), Some(chain.hash(b1)), "the frozen evo tip");
    assert_eq!(layer.bytes(), frozen_bytes);
    let mut d2 = testkit::delta(&chain, b2);
    d2.credit_data = Some(credit_data(2));
    store.commit(&mut layer, &d2).unwrap();
    assert_eq!(layer.best_block(), Some(chain.hash(b2)));
    assert_eq!(layer.bytes(), frozen_bytes + layer.active.bytes());
    let (active, frozen) = layer.indexes();
    assert!(active.get(EvoCf::CreditData, &chain.hash(b2)).is_some());
    assert!(frozen.unwrap().get(EvoCf::CreditData, &chain.hash(b1)).is_some());
    layer.release();
    assert_eq!(layer.bytes(), layer.active.bytes());
    assert!(layer.indexes().1.is_none());
}

/// B1's entries: a put per live row and a delete per tombstone, with the column families' keys.
#[test]
fn b1_writes_a_put_per_row_and_a_delete_per_tombstone() {
    let mut chain = MemChain::new();
    let b2 = chain.extend(chain.genesis(), 2);
    let base = chain.genesis();
    let store = memory_store();
    let mut layer = EvoLayer::new();
    let record = testkit::mined(&chain, LlmqType::LlmqTestDip0024, base, Some(1));
    let mut d = testkit::delta(&chain, b2);
    d.credit_data = Some(credit_data(1));
    d.mined_commitments = vec![record.clone()];
    store.commit(&mut layer, &d).unwrap();
    let mut b = MemoryBatch::default();
    layer.take().write_b1(&mut b);
    assert_eq!((b.len(), b.size_bytes() > 0), (3, true));
    layer.release();
    store.commit(&mut layer, &testkit::undo(&chain, b2, &[record])).unwrap();
    let window = layer.take();
    let ops = window.index().mined.iter().filter(|(_, v)| v.is_none()).count();
    assert_eq!(ops, 2, "both rows' tombstones");
    let mut b = MemoryBatch::default();
    window.write_b1(&mut b);
    assert_eq!(b.len(), 2);
}

/// D37: the freeze check's evo tip is the last committed one, which a release does not forget, or
/// the durable one a layer resumes from; a fresh layer has none.
#[test]
fn the_freeze_checks_evo_tip_survives_the_release() {
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let store = memory_store();
    let mut layer = EvoLayer::new();
    assert_eq!(layer.best_block(), None);
    store.commit(&mut layer, &testkit::delta(&chain, b1)).unwrap();
    flush(&store, &mut layer);
    assert_eq!(layer.evo_tip(), None, "every row durable");
    assert_eq!(layer.best_block(), Some(chain.hash(b1)), "after the release");
    // A window with no commit since still names block 1, so a flush at block 2 fails the check.
    layer.take();
    assert_eq!(layer.best_block(), Some(chain.hash(b1)));
    let durable = chain.hash(b1);
    assert_eq!(EvoLayer::resume(Some(durable)).best_block(), Some(durable));
    assert_eq!(EvoLayer::resume(None).best_block(), None);
}

/// D37: an open resumes from the raw durable `meta.evo_tip`: none on a fresh store, the flushed
/// tip after a flush, and a genesis tip as written (reads normalise it, D38; the freeze check
/// compares the raw value).
#[test]
fn an_open_resumes_from_the_raw_durable_evo_tip() {
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let store = memory_store();
    assert_eq!(store.resume_layer().unwrap().best_block(), None);
    let genesis = chain.hash(chain.genesis());
    testkit::write_b1(&store, &EvoLayer::new().take(), Some(genesis));
    assert_eq!(store.resume_layer().unwrap().best_block(), Some(genesis));
    assert_eq!(store.view(&EvoLayer::new(), &chain).best_block().unwrap(), None, "read as absent");
    let mut layer = store.resume_layer().unwrap();
    store.commit(&mut layer, &testkit::delta(&chain, b1)).unwrap();
    flush(&store, &mut layer);
    assert_eq!(store.resume_layer().unwrap().best_block(), Some(chain.hash(b1)));
}
