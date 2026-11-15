use super::*;
use crate::dkg::bls::{BlsSchemeKind, BlsSkValue, secret_to_public};
use crate::quorum::{VvecReadError, from_store};
use dash_params::{ChainParamsBuilder, Network, Release};
use std::cell::Cell;

type Chain = BTreeMap<BlockId, (Option<BlockId>, BlockHash)>;

fn params(rotation: bool, keep: u16, active: u16, interval: u16) -> (LlmqType, LlmqParams) {
    let t = if rotation {
        LlmqType::LlmqTestDip0024
    } else {
        LlmqType::LlmqTest
    };
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    (
        t,
        LlmqParams {
            use_rotation: rotation,
            keep_old_keys: keep,
            signing_active_quorum_count: active,
            dkg_interval: interval,
            ..*setup.consensus.llmq(t).unwrap()
        },
    )
}

fn id(height: u8) -> BlockId {
    BlockId(5000 - u32::from(height) * 17)
}

fn hash(n: u8) -> BlockHash {
    BlockHash::from([n; 32])
}

fn key(n: u8) -> QuorumDbKey {
    QuorumDbKey([n; 32])
}

fn chain(height: u8) -> Chain {
    (0..=height).map(|h| (id(h), (h.checked_sub(1).map(id), hash(h)))).collect()
}

fn sk(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap()
}

fn contributions(n: u8) -> StoredContributions {
    StoredContributions {
        vvec: Ok(Some(vec![secret_to_public(&sk(n))])),
        sk_share: Some(sk(n)),
    }
}

#[test]
fn due_heights_match_cores_signed_remainder() {
    for height in [58, 634, 1210] {
        assert!(cleanup_due(height));
    }
    for height in [i32::MIN, -518, -58, 0, 57, 59, 633, 635, i32::MAX] {
        assert!(!cleanup_due(height));
    }
}

#[test]
fn depth_walk_counts_chain_steps_and_excludes_the_depth_boundary() {
    let chain = chain(20);
    for rotation in [false, true] {
        let params = [params(rotation, 4, 2, 3)];
        let depth = if rotation {
            6
        } else {
            12
        };
        let boundary = 20 - depth;
        let mut cache = CleanupCache::default();
        cache.init(&params);
        cache.insert(params[0].0, hash(boundary), key(1));
        cache.insert(params[0].0, hash(boundary + 1), key(2));
        let visited = RefCell::new(Vec::new());
        let mut requested = None;
        let keep = keys_to_keep(
            &mut cache,
            &params,
            id(20),
            &|id| {
                visited.borrow_mut().push(id);
                chain[&id]
            },
            &mut |t, count| {
                assert_eq!(t, params[0].0);
                requested = Some(count);
                Vec::new()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(visited.into_inner(), (boundary + 1..=20).rev().map(id).collect::<Vec<_>>());
        assert_eq!(requested, Some(3));
        assert_eq!(keep, BTreeSet::from([key(2)]));
    }
}

#[test]
fn genesis_is_included_and_the_walk_stops_at_its_null_predecessor() {
    let params = [params(false, 2, 1, 24)];
    let mut cache = CleanupCache::default();
    cache.init(&params);
    cache.insert(params[0].0, hash(0), key(1));
    let chain = chain(1);
    let visited = Cell::new(0);
    let keep = keys_to_keep(
        &mut cache,
        &params,
        id(1),
        &|id| {
            visited.set(visited.get() + 1);
            chain[&id]
        },
        &mut |_, count| {
            assert_eq!(count, 1);
            Vec::new()
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(visited.get(), 2);
    assert_eq!(keep, BTreeSet::from([key(1)]));
}

#[test]
fn cached_keys_shorten_the_scan_and_the_next_call_reuses_scanned_keys() {
    let params = [params(false, 3, 1, 24)];
    let mut cache = CleanupCache::default();
    cache.init(&params);
    cache.insert(params[0].0, hash(9), key(9));
    let chain = chain(9);
    let keep = keys_to_keep(
        &mut cache,
        &params,
        id(9),
        &|id| chain[&id],
        &mut |_, count| {
            assert_eq!(count, 2);
            vec![(hash(8), key(8)), (hash(7), key(7))]
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(keep, BTreeSet::from([key(9), key(8), key(7)]));
    let visits = Cell::new(0);
    let mut requested = None;
    let next = keys_to_keep(
        &mut cache,
        &params,
        id(9),
        &|id| {
            visits.set(visits.get() + 1);
            chain[&id]
        },
        &mut |_, count| {
            requested = Some(count);
            Vec::new()
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(next, keep);
    assert_eq!(requested, Some(0));
    assert_eq!(visits.get(), 3);
}

#[test]
fn cache_walk_caps_unique_keys_even_when_the_lru_holds_more_than_keep_old_keys() {
    let params = [params(false, 2, 1, 24)];
    let mut cache = CleanupCache::default();
    cache.init(&params);
    for h in 1..=4 {
        cache.insert(params[0].0, hash(h), key(h));
    }
    assert_eq!(cache.caches[&params[0].0.to_base()].entries.len(), 4);
    let chain = chain(4);
    let visits = Cell::new(0);
    let keep = keys_to_keep(
        &mut cache,
        &params,
        id(4),
        &|id| {
            visits.set(visits.get() + 1);
            chain[&id]
        },
        &mut |_, count| {
            assert_eq!(count, 0);
            Vec::new()
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(keep, BTreeSet::from([key(4), key(3)]));
    assert_eq!(visits.get(), 2);
}

#[test]
fn duplicate_cached_keys_count_once_and_tip_scan_duplicates_are_not_replaced() {
    let params = [params(false, 3, 1, 24)];
    let mut cache = CleanupCache::default();
    cache.init(&params);
    cache.insert(params[0].0, hash(2), key(2));
    cache.insert(params[0].0, hash(1), key(2));
    let chain = chain(2);
    let keep = keys_to_keep(
        &mut cache,
        &params,
        id(2),
        &|id| chain[&id],
        &mut |_, count| {
            assert_eq!(count, 2);
            vec![(hash(2), key(2)), (hash(0), key(3))]
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(keep, BTreeSet::from([key(2), key(3)]));
}

#[test]
fn configured_types_keep_caller_order_and_share_a_union_without_sharing_caches() {
    let params = [params(true, 2, 1, 24), params(false, 3, 1, 24)];
    let mut cache = CleanupCache::default();
    let mut calls = Vec::new();
    let chain = chain(1);
    let keep = keys_to_keep(
        &mut cache,
        &params,
        id(1),
        &|id| chain[&id],
        &mut |t, count| {
            calls.push((t, count));
            if t == params[0].0 {
                vec![(hash(1), key(1)), (hash(0), key(2))]
            } else {
                vec![(hash(1), key(3))]
            }
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(calls, [(params[0].0, 2), (params[1].0, 3)]);
    assert_eq!(keep, BTreeSet::from([key(1), key(2), key(3)]));
    assert_eq!(cache.get(params[0].0, &hash(1)), Some(key(1)));
    assert_eq!(cache.get(params[1].0, &hash(1)), Some(key(3)));
    assert_eq!(cache.get(LlmqType::Llmq400_60, &hash(1)), None);
    cache.insert(LlmqType::Llmq400_60, hash(1), key(4));
    assert_eq!(cache.caches.len(), 2);
}

#[test]
fn lru_refreshes_reads_and_overwrites_and_trims_only_past_twice_capacity() {
    let mut cache = TypeCache::new(2);
    for h in 1..=4 {
        cache.insert(hash(h), key(h));
    }
    assert_eq!(cache.entries.len(), 4);
    assert_eq!(cache.get(&hash(1)), Some(key(1)));
    cache.insert(hash(2), key(22));
    assert_eq!(cache.entries.len(), 4);
    cache.insert(hash(5), key(5));
    assert_eq!(cache.entries.len(), 2);
    assert_eq!(cache.get(&hash(1)), None);
    assert_eq!(cache.get(&hash(2)), Some(key(22)));
    assert_eq!(cache.get(&hash(5)), Some(key(5)));
}

#[test]
fn lru_get_refreshes_an_entry_so_truncation_keeps_it() {
    let mut cache = TypeCache::new(2);
    for h in 1..=4 {
        cache.insert(hash(h), key(h));
    }
    assert_eq!(cache.get(&hash(1)), Some(key(1)));
    cache.insert(hash(5), key(5));
    assert_eq!(cache.entries.len(), 2);
    assert_eq!(cache.get(&hash(4)), None);
    assert_eq!(cache.get(&hash(1)), Some(key(1)));
    assert_eq!(cache.get(&hash(5)), Some(key(5)));
}

#[test]
fn interruption_before_a_type_or_after_a_scan_never_erases_store_rows() {
    let params = [params(false, 2, 1, 24), params(true, 2, 1, 24)];
    let chain = chain(1);
    let store = MemQuorumStore::new();
    store.write(&key(1), &contributions(1)).unwrap();
    let before = store.rows.borrow().clone();
    for initially_interrupted in [false, true] {
        let interrupted = Cell::new(initially_interrupted);
        let scans = Cell::new(0);
        let keep = keys_to_keep(
            &mut CleanupCache::default(),
            &params,
            id(1),
            &|id| chain[&id],
            &mut |_, _| {
                scans.set(scans.get() + 1);
                interrupted.set(true);
                vec![(hash(1), key(2))]
            },
            &|| interrupted.get(),
        );
        if let Some(keep) = keep.as_ref() {
            store.erase_except(keep).unwrap();
        }
        assert_eq!(keep, None);
        assert_eq!(scans.get(), usize::from(!initially_interrupted));
        assert_eq!(*store.rows.borrow(), before);
    }
    let interrupted = Cell::new(false);
    assert!(
        keys_to_keep(
            &mut CleanupCache::default(),
            &params[..1],
            id(1),
            &|id| chain[&id],
            &mut |_, _| {
                interrupted.set(true);
                Vec::new()
            },
            &|| interrupted.get(),
        )
        .is_none()
    );
    assert_eq!(*store.rows.borrow(), before);
}

#[test]
fn store_round_trips_both_prefixes_and_preserves_unwritten_rows() {
    let store = MemQuorumStore::new();
    assert!(store.read(&key(1)).unwrap().is_none());
    let first = contributions(1);
    store.write(&key(1), &first).unwrap();
    let read = store.read(&key(1)).unwrap().unwrap();
    assert_eq!(read.vvec, first.vvec);
    assert_eq!(read.sk_share, first.sk_share);
    assert_eq!(
        store.rows.borrow()[&(DB_QUORUM_VVEC, key(1))],
        encode_vvec(first.vvec.as_ref().unwrap().as_ref().unwrap())
    );
    assert_eq!(
        store.rows.borrow()[&(DB_QUORUM_SK_SHARE, key(1))],
        encode_sk(first.sk_share.as_ref().unwrap())
    );
    store
        .write(
            &key(1),
            &StoredContributions {
                vvec: Ok(None),
                sk_share: None,
            },
        )
        .unwrap();
    store
        .write(
            &key(1),
            &StoredContributions {
                vvec: Ok(Some(Vec::new())),
                sk_share: Some(BlsSkValue::invalid()),
            },
        )
        .unwrap();
    let read = store.read(&key(1)).unwrap().unwrap();
    assert_eq!(read.vvec, Ok(Some(Vec::new())));
    assert_eq!(read.sk_share, first.sk_share);
    let mut second = contributions(2);
    second.vvec = Ok(None);
    store.write(&key(1), &second).unwrap();
    assert_eq!(store.read(&key(1)).unwrap().unwrap().sk_share, second.sk_share);
}

#[test]
fn store_requires_the_vector_row_but_secret_share_is_optional() {
    let store = MemQuorumStore::new();
    store
        .write(
            &key(1),
            &StoredContributions {
                vvec: Ok(None),
                sk_share: Some(sk(1)),
            },
        )
        .unwrap();
    assert!(store.read(&key(1)).unwrap().is_none());
    store
        .write(
            &key(2),
            &StoredContributions {
                vvec: Ok(Some(Vec::new())),
                sk_share: None,
            },
        )
        .unwrap();
    assert!(store.read(&key(2)).unwrap().unwrap().sk_share.is_none());
    assert_eq!(
        store.erase_except(&BTreeSet::new()).unwrap(),
        CleanupCounts {
            vvec: 1,
            sk_share: 1
        }
    );
}

#[test]
fn unreadable_vector_row_is_an_error_and_never_a_store_miss() {
    let store = MemQuorumStore::new();
    store
        .write(
            &key(1),
            &StoredContributions {
                vvec: Ok(Some(Vec::new())),
                sk_share: Some(sk(1)),
            },
        )
        .unwrap();
    let mut unreadable = vec![1];
    unreadable.extend_from_slice(&[0xff; 48]);
    store.rows.borrow_mut().insert((DB_QUORUM_VVEC, key(1)), unreadable);

    let stored = store.read(&key(1)).unwrap().expect("row exists");
    assert_eq!(
        stored.vvec,
        Err(VvecReadError::UndecodableElement {
            index: 0
        })
    );
    assert_eq!(
        from_store(stored).unwrap_err(),
        VvecReadError::UndecodableElement {
            index: 0
        }
    );
}

#[test]
fn every_vector_and_share_row_state_reads_as_core_reads_contributions() {
    let vvec = vec![secret_to_public(&sk(1))];
    let mut undecodable_vvec = vec![1];
    undecodable_vvec.extend_from_slice(&[0xff; 48]);
    let vvec_rows = [None, Some(encode_vvec(&vvec)), Some(undecodable_vvec)];
    let sk_rows = [None, Some(encode_sk(&sk(1)).to_vec()), Some(vec![1; 31])];
    for (v, vvec_row) in vvec_rows.iter().enumerate() {
        for (k, sk_row) in sk_rows.iter().enumerate() {
            let store = MemQuorumStore::new();
            if let Some(row) = vvec_row {
                store.rows.borrow_mut().insert((DB_QUORUM_VVEC, key(1)), row.clone());
            }
            if let Some(row) = sk_row {
                store.rows.borrow_mut().insert((DB_QUORUM_SK_SHARE, key(1)), row.clone());
            }
            let read = store.read(&key(1)).unwrap();
            let case = format!("vvec row {v}, sk row {k}");
            let Some(stored) = read else {
                assert_eq!(v, 0, "{case}: only a missing vector row is a store miss");
                continue;
            };
            assert_ne!(v, 0, "{case}: a missing vector row must be a store miss");
            let share = stored.sk_share.as_ref().map(encode_sk);
            let expected_share = (k == 1).then(|| encode_sk(&sk(1)));
            assert_eq!(share, expected_share, "{case}");
            if v == 1 {
                assert_eq!(stored.vvec, Ok(Some(vvec.clone())), "{case}");
                let keys = from_store(stored).unwrap().expect("a readable vector row loads");
                assert_eq!(keys.sk_share.as_ref().map(encode_sk), expected_share, "{case}");
            } else {
                let unreadable = VvecReadError::UndecodableElement {
                    index: 0,
                };
                assert_eq!(stored.vvec, Err(unreadable.clone()), "{case}");
                assert_eq!(from_store(stored).unwrap_err(), unreadable, "{case}");
            }
        }
    }
}

#[test]
fn store_cleanup_keeps_both_rows_and_leaves_other_prefixes_untouched() {
    let store = MemQuorumStore::new();
    for n in 1..=3 {
        store.write(&key(n), &contributions(n)).unwrap();
    }
    store.rows.borrow_mut().insert(("unrelated", key(4)), vec![7]);
    let counts = store.erase_except(&BTreeSet::from([key(2), key(99)])).unwrap();
    assert_eq!(
        counts,
        CleanupCounts {
            vvec: 2,
            sk_share: 2
        }
    );
    assert!(store.read(&key(1)).unwrap().is_none());
    assert!(store.read(&key(3)).unwrap().is_none());
    let keep = contributions(2);
    let read = store.read(&key(2)).unwrap().unwrap();
    assert_eq!(read.vvec, keep.vvec);
    assert_eq!(read.sk_share, keep.sk_share);
    assert_eq!(store.rows.borrow()[&("unrelated", key(4))], [7]);
    assert_eq!(store.erase_except(&BTreeSet::from([key(2)])).unwrap(), CleanupCounts::default());
}
