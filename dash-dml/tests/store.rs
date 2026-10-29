//! `MemEvoStore`: ancestry-resolved commitment reads across forks, Core's write and overwrite
//! rules, all-or-nothing applies, and disconnects that erase nothing.

use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::creditpool::{
    BlockAt, CreditBlockData, CreditPool, CreditPoolInfo, PoolError, balance_at, credit_data_at,
    credit_pool_info, pool_after,
};
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::testkit::MemChain;
use dash_dml::{DmlDelta, MnList, MnListDiff, MnListDiffRecord};
use dash_dml::{DmlReader, EvoDelta, EvoStore, MemEvoStore, StoreError};
use dash_evo::MinedCommitmentReader;
use dash_evo::llmq::{LlmqCommitment, QuorumSnapshot};
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

/// A null-bodied v1 commitment of `llmq_type` for `quorum_hash`, decoded from Core's layout
/// (version, type, quorum hash, two empty bitsets, zero key, vvec hash and signatures).
fn commitment(llmq_type: LlmqType, quorum_hash: QuorumHash, version: u16) -> Arc<LlmqCommitment> {
    let mut bytes = version.to_le_bytes().to_vec();
    bytes.push(llmq_type.to_base());
    bytes.extend_from_slice(quorum_hash.as_bytes());
    if version == 2 || version == 4 {
        bytes.extend_from_slice(&0i16.to_le_bytes());
    }
    bytes.extend_from_slice(&[0, 0]); // signers, validMembers: CompactSize(0)
    bytes.extend_from_slice(&[0; 48 + 32 + 96 + 96]);
    let mut input = &bytes[..];
    let qc = LlmqCommitment::decode(&mut input).expect("a well-formed commitment");
    assert!(input.is_empty());
    Arc::new(qc)
}

fn delta(chain: &MemChain, block: BlockId) -> EvoDelta {
    EvoDelta {
        block: chain.hash(block),
        height: chain.header(block).height,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: None,
        erased_mined: Vec::new(),
        evo_tip: None,
    }
}

fn quorum_hash(chain: &MemChain, block: BlockId) -> QuorumHash {
    QuorumHash::from(*chain.hash(block).as_bytes())
}

/// Records a non-rotated commitment for the base block `base`, mined in `block`.
fn mine(chain: &MemChain, block: BlockId, base: BlockId) -> EvoDelta {
    let quorum_hash = quorum_hash(chain, base);
    EvoDelta {
        mined_commitments: vec![MinedCommitmentRecord {
            llmq_type: LlmqType::Llmq50_60,
            quorum_hash,
            base_height: chain.header(base).height,
            rotation_index: None,
            commitment: commitment(LlmqType::Llmq50_60, quorum_hash, 1),
        }],
        ..delta(chain, block)
    }
}

#[test]
fn mined_commitments_resolve_through_the_queried_ancestry() {
    // genesis - 1 - 2 - 3 - 4 - 5          (main)
    //                    \- 4' - 5'        (fork at 3)
    let mut chain = MemChain::new();
    let b3 = chain.extend(chain.genesis(), 3);
    let main4 = chain.child(b3);
    let main5 = chain.child(main4);
    let fork4 = chain.child(b3);
    let fork5 = chain.child(fork4);
    let b1 = chain.ancestor(b3, 1).unwrap();
    let b2 = chain.ancestor(b3, 2).unwrap();

    let mut store = MemEvoStore::new();
    store.apply(&mine(&chain, b2, b1)).unwrap();
    store.apply(&mine(&chain, main4, b3)).unwrap();
    store.apply(&mine(&chain, main5, main4)).unwrap();
    // The fork mines the same quorum (base 3) one block later, and another base.
    store.disconnect(&chain.hash(main5), Some(chain.hash(main4))).unwrap();
    store.disconnect(&chain.hash(main4), Some(chain.hash(b3))).unwrap();
    store.apply(&delta(&chain, fork4)).unwrap();
    store.apply(&mine(&chain, fork5, b3)).unwrap();

    let view = store.view(&chain);
    let t = LlmqType::Llmq50_60;
    assert_eq!(view.mined_until(t, main5, 10).unwrap(), [main4, b3, b1]);
    assert_eq!(view.mined_until(t, main5, 2).unwrap(), [main4, b3]);
    assert_eq!(view.mined_until(t, fork5, 10).unwrap(), [b3, b1]);
    assert_eq!(view.mined_until(t, fork4, 10).unwrap(), [b1]);
    assert_eq!(view.mined_until(t, b3, 10).unwrap(), [b1]);
    assert_eq!(view.mined_until(t, chain.genesis(), 10).unwrap(), []);
    assert_eq!(view.mined_until(LlmqType::Llmq400_60, main5, 10).unwrap(), []);

    // The same quorum, mined on both branches: each branch sees its own mining block.
    let q3 = quorum_hash(&chain, b3);
    let (_, mined_in) = view.mined_commitment(t, &q3, main5).unwrap().unwrap();
    assert_eq!(mined_in, chain.hash(main4));
    let (_, mined_in) = view.mined_commitment(t, &q3, fork5).unwrap().unwrap();
    assert_eq!(mined_in, chain.hash(fork5));
    assert!(view.mined_commitment(t, &q3, fork4).unwrap().is_none());
    // A disconnected branch keeps its records (nothing is erased).
    assert!(view.mined_commitment(t, &quorum_hash(&chain, main4), main5).unwrap().is_some());
    assert_eq!(view.best_block().unwrap(), Some(chain.hash(fork5)));
}

#[test]
fn rotated_commitments_are_read_per_quorum_index() {
    let mut chain = MemChain::new();
    let tip = chain.extend(chain.genesis(), 12);
    let at = |h| chain.ancestor(tip, h).unwrap();
    let t = LlmqType::LlmqTestDip0024;
    let mut store = MemEvoStore::new();
    // Cycles based at 2 and 6, two quorum indexes each, mined two blocks after their base.
    for (base, mined) in [(2, 4), (6, 8)] {
        let mut d = delta(&chain, at(mined));
        for index in 0..2i16 {
            let mut qh = *chain.hash(at(base)).as_bytes();
            qh[0] ^= index as u8 + 1;
            let quorum_hash = QuorumHash::from(qh);
            d.mined_commitments.push(MinedCommitmentRecord {
                llmq_type: t,
                quorum_hash,
                base_height: base + index as u32,
                rotation_index: Some(index),
                commitment: commitment(t, quorum_hash, 4),
            });
        }
        store.apply(&d).unwrap();
    }
    let view = store.view(&chain);
    assert_eq!(view.nth_mined_by_index(t, tip, 1, 0).unwrap(), Some(at(7)));
    assert_eq!(view.nth_mined_by_index(t, tip, 1, 1).unwrap(), Some(at(3)));
    assert_eq!(view.nth_mined_by_index(t, tip, 1, 2).unwrap(), None);
    assert_eq!(view.nth_mined_by_index(t, at(7), 0, 0).unwrap(), Some(at(2)));
    assert_eq!(view.newest_by_index(t, tip, 0, 5).unwrap(), [at(6), at(2)]);
    // Rotated records are not in the non-rotated family.
    assert_eq!(view.mined_until(t, tip, 10).unwrap(), []);
}

#[test]
fn rewrites_must_equal_the_stored_row_and_fail_atomically() {
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let b2 = chain.child(b1);
    let mut store = MemEvoStore::new();

    let record = |total| {
        Arc::new(MnListDiffRecord {
            height: 1,
            total_registered_after: total,
            diff: MnListDiff::default(),
            collision_buckets_after: Vec::new(),
        })
    };
    let dml = |total| DmlDelta {
        block: chain.hash(b1),
        height: 1,
        record: record(total),
        snapshot: Some(Arc::new(MnList::new(chain.hash(b1), 1, total))),
    };
    let first = EvoDelta {
        dml: Some(dml(3)),
        ..delta(&chain, b1)
    };
    store.apply(&first).unwrap();
    // Reconnecting the same block rewrites identical rows.
    store.disconnect(&chain.hash(b1), Some(chain.hash(chain.genesis()))).unwrap();
    store.apply(&first).unwrap();

    // A differing rewrite is a store inconsistency, and nothing of the delta is written.
    store.disconnect(&chain.hash(b1), None).unwrap();
    let pool = Arc::new(Ok(CreditPool {
        locked: 5,
        ..CreditPool::default()
    }));
    let bad = EvoDelta {
        dml: Some(dml(4)),
        credit_pool: Some(pool),
        ..delta(&chain, b1)
    };
    assert!(matches!(store.apply(&bad), Err(StoreError::Inconsistent { .. })));
    let view = store.view(&chain);
    assert_eq!(view.best_block().unwrap(), None);
    assert!(view.credit_pool(&chain.hash(b1)).unwrap().is_none());
    assert_eq!(view.mn_list_diff(&chain.hash(b1)).unwrap().unwrap().total_registered_after, 3);

    // A snapshot must be stamped with its own block and height.
    let mislabelled = DmlDelta {
        snapshot: Some(Arc::new(MnList::new(chain.hash(b1), 2, 3))),
        ..dml(3)
    };
    let bad = EvoDelta {
        dml: Some(mislabelled),
        ..delta(&chain, b1)
    };
    assert!(matches!(store.apply(&bad), Err(StoreError::Inconsistent { .. })));

    // A DML delta must name its own block.
    let misplaced = EvoDelta {
        dml: Some(dml(3)),
        ..delta(&chain, b2)
    };
    assert!(store.apply(&misplaced).is_err());
    // Only the tip can be disconnected.
    assert!(store.disconnect(&chain.hash(b2), None).is_err());
}

#[test]
fn rotation_snapshots_are_overwritten_unconditionally() {
    let base = BlockHash::from([5u8; 32]);
    let t = LlmqType::LlmqTestDip0024;
    let mut store = MemEvoStore::new();
    let chain = MemChain::new();
    assert!(store.view(&chain).quorum_snapshot(t, &base).unwrap().is_none());

    let first = QuorumSnapshot {
        active_quorum_members: vec![true, false],
        ..Default::default()
    };
    let second = QuorumSnapshot {
        skip_list_mode: 1,
        skip_list: vec![2],
        ..first.clone()
    };
    store.store_quorum_snapshot(t, base, first);
    store.store_quorum_snapshot(t, base, second.clone());
    let read = store.view(&chain).quorum_snapshot(t, &base).unwrap().unwrap();
    assert_eq!(*read, second);
    assert!(store.view(&chain).quorum_snapshot(LlmqType::LlmqTest, &base).unwrap().is_none());
}

/// Records a rotated commitment with quorum index 0 for the base block `base`, mined in `block`.
fn mine_rotated(chain: &MemChain, block: BlockId, base: BlockId) -> EvoDelta {
    let t = LlmqType::LlmqTestDip0024;
    let quorum_hash = quorum_hash(chain, base);
    EvoDelta {
        mined_commitments: vec![MinedCommitmentRecord {
            llmq_type: t,
            quorum_hash,
            base_height: chain.header(base).height,
            rotation_index: Some(0),
            commitment: commitment(t, quorum_hash, 4),
        }],
        ..delta(chain, block)
    }
}

/// EV0 API review r1, M2: Core's qrinfo builder reads the **active** chain's index under the
/// request block's height and maps bases through the request block, which the ancestry-resolved
/// read at a stale request block does not give.
#[test]
fn qrinfo_reads_the_active_index_for_a_stale_request_block() {
    //            /- A5 - A6 - A7        (active; A5 mines base 2, A7 mines base A6)
    // 0 - 1 - 2 - 3 - 4
    //                 \- S5 - S6 - S7   (stale; S5 mines base 3)
    let t = LlmqType::LlmqTestDip0024;
    let mut chain = MemChain::new();
    let b4 = chain.extend(chain.genesis(), 4);
    let (b2, b3) = (chain.ancestor(b4, 2).unwrap(), chain.ancestor(b4, 3).unwrap());
    let s5 = chain.child(b4);
    let s7 = chain.extend(s5, 2);
    let a5 = chain.child(b4);
    let a6 = chain.child(a5);
    let a7 = chain.child(a6);

    let mut store = MemEvoStore::new();
    for id in 1..=4 {
        store.apply(&delta(&chain, chain.ancestor(b4, id).unwrap())).unwrap();
    }
    store.apply(&mine_rotated(&chain, s5, b3)).unwrap();
    store.disconnect(&chain.hash(s5), Some(chain.hash(b4))).unwrap();
    store.apply(&mine_rotated(&chain, a5, b2)).unwrap();
    store.apply(&delta(&chain, a6)).unwrap();
    store.apply(&mine_rotated(&chain, a7, a6)).unwrap();
    let view = store.view(&chain);
    let tip = a7;
    let rotated = dash_evo::MinedFamily::Rotated {
        quorum_index: 0,
    };

    // The request block S6: the ancestry read gives the stale branch's commitment (base 3).
    let s6 = chain.ancestor(s7, 6).unwrap();
    assert_eq!(view.nth_mined_by_index(t, s6, 0, 0).unwrap(), Some(b3));
    // Core's read: the active index capped at h(S6), the base mapped through S6 (base 2).
    let rows = view.mined_index(t, rotated, tip, chain.header(s6).height, 1).unwrap();
    assert_eq!(
        rows,
        [dash_evo::MinedIndexEntry {
            mined_height: 5,
            base_height: 2
        }]
    );
    let base = dash_evo::llmq::base_through::<StoreError>(&chain, s6, rows[0].base_height).unwrap();
    assert_eq!(base, b2);
    assert!(view.mined_commitment(t, &quorum_hash(&chain, base), tip).unwrap().is_some());

    // The request block S7: the newest active row (mined 7, base 6) maps through S7 to S6, a
    // stale block the active chain never mined a commitment for, which fails Core's request.
    let rows = view.mined_index(t, rotated, tip, chain.header(s7).height, 1).unwrap();
    assert_eq!(
        rows,
        [dash_evo::MinedIndexEntry {
            mined_height: 7,
            base_height: 6
        }]
    );
    let base = dash_evo::llmq::base_through::<StoreError>(&chain, s7, rows[0].base_height).unwrap();
    assert_eq!(base, s6);
    assert!(view.mined_commitment(t, &quorum_hash(&chain, base), tip).unwrap().is_none());
    // The ancestry read at S7 still sees only its own branch.
    assert_eq!(view.nth_mined_by_index(t, s7, 0, 0).unwrap(), Some(b3));
}

/// EV0 API review r1, M1: checking a rewrite never decodes the stored row's lazy operator key,
/// so the stored list serializes as it did, also when the rewrite differs.
#[test]
fn a_rewrite_check_never_changes_the_stored_serialization() {
    use dash_dml::MnType;
    use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind, BlsSchemeSource};

    const UNDECODABLE: [u8; 48] = [0xff; 48];
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let list_with = |key: BlsLazyPk| {
        // Version 2: the state stores its operator key under the basic scheme.
        let mut cold = dash_dml::testkit::cold_state(1, MnType::Regular);
        cold.version = 2;
        cold.pub_key_operator = key;
        let dmn = dash_dml::testkit::dmn(1, MnType::Regular);
        let dmn = dash_dml::Dmn {
            cold: Arc::new(cold),
            ..(*dmn).clone()
        };
        let mut list = MnList::new(chain.hash(b1), 1, 2);
        list.insert_for_tests(Arc::new(dmn));
        Arc::new(list)
    };
    let dml = |list: Arc<MnList>| DmlDelta {
        block: chain.hash(b1),
        height: 1,
        record: Arc::new(MnListDiffRecord {
            height: 1,
            total_registered_after: 2,
            diff: MnListDiff::default(),
            collision_buckets_after: Vec::new(),
        }),
        snapshot: Some(list),
    };
    let raw = BlsLazyPk::from_bytes(&UNDECODABLE, BlsSchemeSource::Version(BlsSchemeKind::Basic));
    let stored = list_with(raw.clone());
    let mut store = MemEvoStore::new();
    store
        .apply(&EvoDelta {
            dml: Some(dml(Arc::clone(&stored))),
            ..delta(&chain, b1)
        })
        .unwrap();

    // The same row again, in a different allocation: accepted.
    store.disconnect(&chain.hash(b1), None).unwrap();
    store
        .apply(&EvoDelta {
            dml: Some(dml(list_with(raw))),
            ..delta(&chain, b1)
        })
        .unwrap();

    // A rewrite whose key is a decoded object: Core's operator== would decode the stored key,
    // fail, and leave it serializing as zeros. The check rejects it without decoding.
    store.disconnect(&chain.hash(b1), None).unwrap();
    // A G1 point canonical under both schemes (base-sdk's corpus `sub_100` key).
    const VALID: [u8; 48] = [
        0x88, 0x23, 0x54, 0xca, 0x87, 0xdf, 0x4b, 0xd9, 0x3d, 0x56, 0xce, 0xc1, 0x59, 0x93, 0xee,
        0xe0, 0x30, 0xcf, 0x14, 0x7a, 0x98, 0x49, 0x4e, 0x45, 0x2b, 0x95, 0x8d, 0xff, 0x9c, 0xb8,
        0xc5, 0x2e, 0x05, 0x0b, 0xc2, 0x02, 0xf5, 0xbc, 0x78, 0x1c, 0xd0, 0x62, 0x16, 0xc3, 0xc3,
        0xa5, 0x9b, 0xf8,
    ];
    let point = BlsLazyPk::from_bytes(&VALID, BlsSchemeSource::Version(BlsSchemeKind::Basic)).get();
    assert!(point.is_valid());
    let decoded = BlsLazyPk::from_value(point, BlsSchemeKind::Basic);
    let bad = EvoDelta {
        dml: Some(dml(list_with(decoded))),
        ..delta(&chain, b1)
    };
    assert!(matches!(store.apply(&bad), Err(StoreError::Inconsistent { .. })));
    let read = store.mn_list_snapshot(&chain.hash(b1)).unwrap().unwrap();
    assert!(Arc::ptr_eq(&read, &stored));
    let key = &read.iter().next().unwrap().cold.pub_key_operator;
    assert_eq!(key.to_bytes(), UNDECODABLE);
}

/// EV3 D8, D9: the credit records resolve by hash and deployment; a missing record where one is
/// due is a store fault.
#[test]
fn credit_records_resolve_by_deployment() {
    let mut chain = MemChain::new();
    let b2 = chain.extend(chain.genesis(), 2);
    let b1 = chain.ancestor(b2, 1).unwrap();
    let pool = CreditPool {
        locked: 9,
        current_limit: 2,
        lately_unlocked: 1,
        ..CreditPool::default()
    };
    let data = CreditBlockData {
        balance: 9,
        unlocked: 1,
    };
    let mut store = MemEvoStore::new();
    store
        .apply(&EvoDelta {
            credit_data: Some(CreditBlockData {
                balance: 4,
                unlocked: 0,
            }),
            ..delta(&chain, b1)
        })
        .unwrap();
    store
        .apply(&EvoDelta {
            credit_data: Some(data),
            credit_pool: Some(Arc::new(Ok(pool.clone()))),
            ..delta(&chain, b2)
        })
        .unwrap();
    let view = store.view(&chain);
    let (h1, h2) = (chain.hash(b1), chain.hash(b2));

    assert_eq!(pool_after(&view, &h2, true).unwrap(), Ok(pool.clone()));
    assert_eq!(pool_after(&view, &h1, false).unwrap(), Ok(CreditPool::default()));
    assert!(matches!(pool_after(&view, &h1, true), Err(StoreError::Inconsistent { .. })));
    assert_eq!(credit_data_at(&view, &h2, true).unwrap(), data);
    assert_eq!(
        credit_data_at(&view, &chain.hash(chain.genesis()), false).unwrap(),
        CreditBlockData::default()
    );
    assert!(matches!(
        credit_data_at(&view, &chain.hash(chain.genesis()), true),
        Err(StoreError::Inconsistent { .. })
    ));
    assert_eq!(balance_at(&view, &h1, true).unwrap(), 4);

    let at = |hash, height| BlockAt {
        hash,
        height,
        dip3: true,
        v20: true,
    };
    let w = BlockAt {
        v20: false,
        ..at(&h1, 1)
    };
    assert_eq!(
        credit_pool_info(&view, at(&h2, 2), Some(w), 576).unwrap(),
        Ok(CreditPoolInfo {
            pool: pool.clone(),
            window_blocks: 576,
            window_start: Some((1, 4)),
        })
    );
    let info = credit_pool_info(&view, at(&h2, 2), None, 576).unwrap().unwrap();
    assert_eq!(info.window_start, None);

    // A stored exception is the RPC's answer; a rewrite must be the same exception.
    let mut store = MemEvoStore::new();
    let failed = EvoDelta {
        credit_data: Some(data),
        credit_pool: Some(Arc::new(Err(PoolError::PrevLocked))),
        ..delta(&chain, b2)
    };
    store.apply(&failed).unwrap();
    store.disconnect(&h2, None).unwrap();
    store.apply(&failed).unwrap();
    store.disconnect(&h2, None).unwrap();
    let other = EvoDelta {
        credit_pool: Some(Arc::new(Err(PoolError::PrevIndexes))),
        ..failed.clone()
    };
    assert!(matches!(store.apply(&other), Err(StoreError::Inconsistent { .. })));
    let view = store.view(&chain);
    assert_eq!(credit_pool_info(&view, at(&h2, 2), None, 576).unwrap(), Err(PoolError::PrevLocked));
}
