//! Loom L6 (SB5 §10.2): a cold reader's miss-read-insert ‖ the writer's commit and
//! `invalidate`, over the production cache, by id and by hash. After `invalidate` returns, the
//! cache never holds the pre-commit record. Loom L14: SPV's by-id resolution ‖ the writer's
//! issue, commit and eviction. Run with `RUSTFLAGS="--cfg loom" cargo test -p dash-chain --release
//! --lib loom_`.

use loom::thread;

use super::test_store::{RowStore, record};
use super::*;
use crate::feed::loom_tests::model;
use crate::shared::resolve_by_id;
use crate::sync::AtomicBool;

fn l6(by_hash: bool, unlock_around_read: bool) {
    model(move || {
        let store = Arc::new(RowStore::with([record(1, 0)]));
        let mut cache = ColdCache::new(2);
        cache.faults.unlock_around_read = unlock_around_read;
        let cache = Arc::new(cache);
        let reader = {
            let (store, cache) = (store.clone(), cache.clone());
            thread::spawn(move || {
                let read = match by_hash {
                    true => {
                        cache.load_by_hash(&record(1, 0).hash, &*store, &AtomicU32::new(u32::MAX))
                    }
                    false => cache.get_or_load(BlockId(1), &*store, &AtomicU32::new(u32::MAX)),
                };
                read.expect("no injected errors");
            })
        };
        store.commit(record(1, 7));
        cache.invalidate(&[BlockId(1)]);
        reader.join().expect("the reader");
        let held = cache.peek(BlockId(1));
        assert!(
            held.is_none_or(|r| *r == record(1, 7)),
            "a pre-commit record survived the invalidation"
        );
    });
}

#[test]
fn loom_l6_cold_invalidate() {
    l6(false, false);
}

#[test]
fn loom_l6_cold_invalidate_by_hash() {
    l6(true, false);
}

/// L6's negative controls: the read releases the mutex around its store read, so a pre-commit
/// read inserts after the invalidation.
#[test]
#[should_panic(expected = "a pre-commit record survived the invalidation")]
fn loom_l6_neg_unlock_around_read() {
    l6(false, true);
}

#[test]
#[should_panic(expected = "a pre-commit record survived the invalidation")]
fn loom_l6_neg_unlock_around_read_by_hash() {
    l6(true, true);
}

type Resolve = fn(
    &AtomicU32,
    &dyn Fn() -> Option<()>,
    &dyn Fn() -> Result<Option<()>, StoreError>,
) -> Result<Option<()>, StoreError>;

const ID: BlockId = BlockId(1);

fn missing() -> StoreError {
    StoreError::Corrupt("no record".into())
}

/// L14 (DEC-187 amendment 3): a reader resolves `ID` by id, through `resolve`, the production
/// cache and the watermark, while the writer sets its entry, issues it, commits its record and
/// evicts it. `resident` stands for the arena slot: set before the issue, evicted after the
/// commit, as the writer does. The reader never rejects the issued id as one with no record.
fn l14(resolve: Resolve) {
    model(move || {
        let store = Arc::new(RowStore::default());
        let cache = Arc::new(ColdCache::new(2));
        let issued = Arc::new(AtomicU32::new(ID.0));
        let resident = Arc::new(AtomicBool::new(false));
        let reader = {
            let (store, cache, issued, resident) =
                (store.clone(), cache.clone(), issued.clone(), resident.clone());
            thread::spawn(move || {
                let got =
                    resolve(&issued, &|| resident.load(Ordering::Acquire).then_some(()), &|| {
                        Ok(cache.get_or_load(ID, &*store, &issued)?.map(drop))
                    });
                assert!(got.is_ok(), "an issued id rejected as missing: {got:?}");
            })
        };
        resident.store(true, Ordering::Release);
        issued.store(ID.0 + 1, Ordering::Release);
        store.commit(record(ID.0, 0));
        resident.store(false, Ordering::Release);
        reader.join().expect("the reader");
    });
}

#[test]
fn loom_l14_issue_vs_by_id() {
    l14(|issued, resident, cold| resolve_by_id(issued, ID, resident, cold, missing));
}

/// L14's negative control: the watermark read after the resident lookup. An id issued between
/// the miss and a store read before its commit is rejected.
#[test]
#[should_panic(expected = "an issued id rejected as missing")]
fn loom_l14_neg_watermark_after_lookup() {
    l14(|issued, resident, cold| {
        let found = resident();
        resolve_by_id(issued, ID, || found, cold, missing)
    });
}
