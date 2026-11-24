//! EV2a (the list model, codecs and store) against EV0: the record walk `resolve_list` starts
//! with, and the per-block delta `process_block` hands over.

use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::testkit::MemChain;
use dash_dml::{DmlDelta, MnList, MnListDiff, MnListDiffRecord};
use dash_dml::{DmlReader, EvoDelta, MemEvoStore, StoreError};

use super::empty_delta;

/// The first half of Core's `GetListForBlockInternal` (EV2 §2.10): walk back from `at` to the
/// nearest stored snapshot, collecting the diff records to apply forward. EV2a's `resolve_list`
/// adds the DIP3 gate, the list cache, the initial-snapshot case and `apply_record`.
pub fn records_to_replay(
    at: BlockId,
    chain: &dyn ChainAncestry,
    store: &dyn DmlReader,
) -> Result<(Arc<MnList>, Vec<Arc<MnListDiffRecord>>), StoreError> {
    let mut records = Vec::new();
    let mut cursor = at;
    loop {
        let hash = chain.header(cursor).hash;
        if let Some(snapshot) = store.mn_list_snapshot(&hash)? {
            records.reverse();
            return Ok((snapshot, records));
        }
        match store.mn_list_diff(&hash)? {
            Some(record) => records.push(record),
            // A DIP3-active block above the first with no diff is corruption (EV2 §2.10).
            None => return Err(StoreError::inconsistent("missing mn_diff row")),
        }
        cursor =
            chain.prev(cursor).ok_or_else(|| StoreError::inconsistent("walked past genesis"))?;
    }
}

/// The DML delta of one block, as EV2b's `process_block` builds it (snapshot cadence: every 576
/// blocks and the first DIP3 block).
pub fn dml_delta(list: &MnList, record: MnListDiffRecord, first_dip3_block: bool) -> DmlDelta {
    let height = list.height();
    DmlDelta {
        block: *list.block_hash(),
        height,
        record: Arc::new(record),
        snapshot: (height % 576 == 0 || first_dip3_block).then(|| Arc::new(list.clone())),
    }
}

#[test]
fn the_walk_stops_at_the_nearest_snapshot() {
    let mut chain = MemChain::new();
    let tip = chain.extend(chain.genesis(), 3);
    let mut store = MemEvoStore::new();
    for height in 1..=3u32 {
        let id = chain.ancestor(tip, height).unwrap();
        let list = MnList::new(chain.hash(id), height as i32, height);
        let record = MnListDiffRecord {
            height: height as i32,
            total_registered_after: height,
            diff: MnListDiff::default(),
            collision_buckets_after: Vec::new(),
        };
        let delta = EvoDelta {
            dml: Some(dml_delta(&list, record, height == 1)),
            ..empty_delta(&chain, id)
        };
        store.apply(&delta).unwrap();
    }
    let (snapshot, records) = records_to_replay(tip, &chain, &store).unwrap();
    assert_eq!(snapshot.height(), 1);
    let heights: Vec<_> = records.iter().map(|r| r.height).collect();
    assert_eq!(heights, [2, 3]);
}
