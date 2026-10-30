//! The evo row codecs (CH7 §7.2): round trips, one encoding per value, and the key layout.

use dash_consensus::ChainAncestry;
use dash_dml::creditpool::RangesSet;
use dash_dml::testkit::{MemChain, numbered};
use dash_dml::{DmlDelta, SameRecord};
use proptest::prelude::*;

use super::*;
use crate::evo::testkit::{self, commitment};

fn pool_state() -> impl Strategy<Value = PoolState> {
    let reason = prop_oneof![
        Just(UnlockDataReason::Payload),
        Just(UnlockDataReason::TxoutOutOfRange),
        Just(UnlockDataReason::Overflow),
    ];
    let pool =
        (any::<i64>(), any::<i64>(), any::<i64>(), prop::collection::vec(any::<u64>(), 0..8))
            .prop_map(|(locked, current_limit, lately_unlocked, indexes)| {
                let mut set = RangesSet::default();
                for i in indexes {
                    set.add(i);
                }
                CreditPool {
                    locked,
                    current_limit,
                    lately_unlocked,
                    indexes: set,
                }
            });
    prop_oneof![
        pool.prop_map(Ok),
        Just(Err(PoolError::PrevLocked)),
        Just(Err(PoolError::PrevIndexes)),
        Just(Err(PoolError::IndexDuplicated)),
        any::<i64>().prop_map(|l| Err(PoolError::NegativeLimit(l))),
        reason.prop_map(|r| Err(PoolError::UnlockData(r))),
    ]
}

fn signals() -> impl Strategy<Value = EhfSignals> {
    prop::collection::vec((0u8..29, any::<i32>()), 0..29).prop_map(EhfSignals::from_iter)
}

fn snapshot() -> impl Strategy<Value = QuorumSnapshot> {
    (
        any::<i32>(),
        prop::collection::vec(any::<bool>(), 0..70),
        prop::collection::vec(any::<i32>(), 0..9),
    )
        .prop_map(|(skip_list_mode, active_quorum_members, skip_list)| QuorumSnapshot {
            active_quorum_members,
            skip_list_mode,
            skip_list,
        })
}

/// Every strict prefix of `bytes` and `bytes` with a byte appended fail to decode.
fn strict<T>(bytes: &[u8], decode: impl Fn(&[u8]) -> Result<T, StoreError>) {
    for n in 0..bytes.len() {
        assert!(decode(&bytes[..n]).is_err(), "a {n}-byte prefix of {} decodes", bytes.len());
    }
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(decode(&longer).is_err(), "trailing bytes decode");
}

proptest! {
    #[test]
    fn pool_states_round_trip(state in pool_state()) {
        let bytes = encode_pool(&state);
        prop_assert_eq!(decode_pool(&bytes).unwrap(), state);
        strict(&bytes, decode_pool);
    }

    #[test]
    fn credit_data_round_trips(balance in any::<i64>(), unlocked in any::<i64>()) {
        let data = CreditBlockData { balance, unlocked };
        let bytes = encode_credit_data(&data);
        prop_assert_eq!(decode_credit_data(&bytes).unwrap(), data);
        strict(&bytes, decode_credit_data);
    }

    #[test]
    fn mnhf_signals_round_trip(signals in signals()) {
        let bytes = encode_mnhf(&signals);
        prop_assert_eq!(decode_mnhf(&bytes).unwrap(), signals);
        // An empty map is one byte, whose only prefix is empty.
        strict(&bytes, decode_mnhf);
    }

    #[test]
    fn quorum_snapshots_round_trip(snapshot in snapshot()) {
        let bytes = encode_quorum_snapshot(&snapshot);
        prop_assert_eq!(decode_quorum_snapshot(&bytes).unwrap(), snapshot);
        strict(&bytes, decode_quorum_snapshot);
    }

    #[test]
    fn index_rows_round_trip(base_height in any::<u32>(), block in any::<[u8; 32]>()) {
        let block = BlockHash::from(block);
        let bytes = encode_index(base_height, &block);
        prop_assert_eq!(decode_index(&bytes).unwrap(), IndexRow { base_height, block });
        strict(&bytes, decode_index);
    }

    #[test]
    fn mined_rows_round_trip(
        version in prop_oneof![Just(1u16), Just(2), Just(3), Just(4)],
        quorum in any::<[u8; 32]>(),
        block in any::<[u8; 32]>(),
        height in any::<u32>(),
    ) {
        let qc = commitment(LlmqType::Llmq50_60, QuorumHash::from(quorum), version);
        let block = BlockHash::from(block);
        let bytes = encode_mined(&qc, &block, height);
        let row = decode_mined(&bytes).unwrap();
        prop_assert_eq!(&*row.commitment, &qc);
        prop_assert_eq!((row.block, row.height), (block, height));
        strict(&bytes, decode_mined);
    }

    /// Height-index keys order newest first within a type and family, and the families and types
    /// never interleave.
    #[test]
    fn index_keys_order_newest_first(a in any::<u32>(), b in any::<u32>(), q in any::<i16>()) {
        for rotation in [None, Some(q)] {
            let (ka, kb) = (
                index_key(LlmqType::Llmq50_60, rotation, a),
                index_key(LlmqType::Llmq50_60, rotation, b),
            );
            prop_assert_eq!(ka.cmp(&kb), b.cmp(&a));
            prop_assert_eq!(index_key_height(&ka).unwrap(), a);
            prop_assert!(ka.starts_with(&index_prefix(LlmqType::Llmq50_60, rotation)));
        }
        let other = index_prefix(LlmqType::Llmq50_60, Some(q));
        prop_assert!(!index_key(LlmqType::Llmq50_60, None, a).starts_with(&other));
        prop_assert!(!index_key(LlmqType::Llmq400_60, None, a)
            .starts_with(&index_prefix(LlmqType::Llmq50_60, None)));
    }
}

#[test]
fn the_keys_are_a1s_with_the_family_and_credit_data_tags() {
    let block = BlockHash::from([7; 32]);
    for kind in EvoCf::ALL {
        let key = kind.key(&block);
        assert_eq!(&key.as_bytes()[..32], &[7; 32]);
        assert_eq!(
            key.as_bytes().len(),
            if kind == EvoCf::CreditData {
                33
            } else {
                32
            }
        );
        assert!(CHAINSTATE_SCHEMA.cf(kind.name()).is_some());
    }
    assert_eq!(EvoCf::CreditData.key(&block).as_bytes()[32], CREDIT_DATA_TAG);
    let q = QuorumHash::from([9; 32]);
    let key = mined_key(LlmqType::Llmq400_85, &q);
    assert_eq!(split_mined_key(&key), (MinedCf::Mined, &[[3u8].as_slice(), &[9; 32]].concat()[..]));
    let key = index_key(LlmqType::LlmqTestDip0024, Some(-2), 5);
    let (cf, cf_key) = split_mined_key(&key);
    assert_eq!(cf, MinedCf::Index);
    assert_eq!(cf_key, [103, 1, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xfa]);
    let key = index_key(LlmqType::Llmq50_60, None, 5);
    assert_eq!(split_mined_key(&key).1, [1, 0, 0, 0, 0xff, 0xff, 0xff, 0xfa]);
}

#[test]
fn decoders_refuse_what_the_encoders_never_write() {
    // An index key one byte short or long, or a stray short key.
    let key = index_key(LlmqType::Llmq50_60, None, 7);
    assert_eq!(index_key_height(&key).unwrap(), 7);
    for bad in [&key[..8], &[&key[..], &[0]].concat(), &[1]] {
        assert!(matches!(index_key_height(bad), Err(StoreError::Read { .. })), "{bad:?}");
    }
    // An MNHF bit at VERSIONBITS_NUM_BITS, a repeated bit and bits out of order.
    for bytes in [
        vec![1, 29, 0, 0, 0, 0],
        vec![2, 3, 0, 0, 0, 0, 3, 0, 0, 0, 0],
        vec![2, 4, 0, 0, 0, 0, 3, 0, 0, 0, 0],
    ] {
        assert!(decode_mnhf(&bytes).is_err(), "{bytes:?}");
    }
    // A snapshot's padding bits, and a member count past the end.
    let mut bytes = encode_quorum_snapshot(&QuorumSnapshot {
        active_quorum_members: vec![true; 3],
        skip_list_mode: 0,
        skip_list: vec![],
    });
    assert_eq!(bytes[5], 0b111);
    bytes[5] |= 0b1000;
    assert!(decode_quorum_snapshot(&bytes).is_err());
    assert!(decode_quorum_snapshot(&[0, 0, 0, 0, 0xfd, 0xff, 0xff, 0]).is_err());
    // Unknown pool tags and unlock reasons.
    assert!(decode_pool(&[6]).is_err());
    assert!(decode_pool(&[5, 3]).is_err());
}

/// The DML rows go through dash-dml's record codec and come back as they were built.
#[test]
fn a_deltas_rows_decode_to_its_records() {
    let mut chain = MemChain::new();
    let at = chain.extend(chain.genesis(), 3);
    let hash = chain.hash(at);
    let prev = MnList::default_pre_dip3();
    let mut next = prev.clone().with_block_hash(hash).with_height(3);
    next.try_add(numbered(0), true).unwrap();
    let mut d = testkit::delta(&chain, at);
    d.dml = Some(testkit::dml(&prev, &next, true));
    d.credit_data = Some(CreditBlockData {
        balance: 5,
        unlocked: 1,
    });
    d.credit_pool = Some(Arc::new(Err(PoolError::NegativeLimit(-3))));
    d.mnhf_signals = Some(EhfSignals::from_iter([(4, 3)]));
    let base = chain.ancestor(at, 1).unwrap();
    d.mined_commitments = vec![testkit::mined(&chain, LlmqType::LlmqTestDip0024, base, Some(1))];
    let codec = testkit::codec();
    let rows = codec.rows(&d).unwrap();
    let row = |kind| &rows.by_hash.iter().find(|(k, b, _)| *k == kind && *b == hash).unwrap().2;
    let diff = codec.decode_diff(row(EvoCf::MnDiff)).unwrap();
    assert!(diff.same_record(&d.dml.as_ref().unwrap().record));
    let list = codec.decode_snapshot(row(EvoCf::MnSnapshot), &hash).unwrap();
    assert!((*list).same_record(&next));
    assert!(codec.decode_snapshot(row(EvoCf::MnSnapshot), &BlockHash::from([1; 32])).is_err());
    assert_eq!(decode_credit_data(row(EvoCf::CreditData)).unwrap(), d.credit_data.unwrap());
    assert_eq!(decode_pool(row(EvoCf::CreditPool)).unwrap(), Err(PoolError::NegativeLimit(-3)));
    assert_eq!(decode_mnhf(row(EvoCf::Mnhf)).unwrap(), d.mnhf_signals.unwrap());
    let record = &d.mined_commitments[0];
    assert_eq!(rows.mined[0].0, mined_key(record.llmq_type, &record.quorum_hash));
    let mined = decode_mined(&rows.mined[0].1).unwrap();
    assert_eq!((&*mined.commitment, mined.block, mined.height), (&*record.commitment, hash, 3));
    assert_eq!(rows.index[0].0, index_key(record.llmq_type, Some(1), 3));
    assert_eq!(
        decode_index(&rows.index[0].1).unwrap(),
        IndexRow {
            base_height: 1,
            block: hash,
        }
    );
    assert_eq!(rows.evo_tip, hash);
    // A delta that disagrees with itself is refused.
    let mut bad = d.clone();
    bad.mined_commitments[0].base_height = 4;
    assert!(matches!(codec.rows(&bad), Err(StoreError::Inconsistent { .. })));
    let mut at_its_block = d.clone();
    at_its_block.mined_commitments[0].base_height = 3;
    assert!(codec.rows(&at_its_block).is_ok(), "a base at the mining height, as MemEvoStore");
    let mut bad = d.clone();
    bad.height = 2;
    assert!(matches!(codec.rows(&bad), Err(StoreError::Inconsistent { .. })));
    // So is a DML part or snapshot of another block or height, each field alone.
    let other = BlockHash::from([9; 32]);
    let wrong = next.clone().with_height(4);
    let changes: [&dyn Fn(&mut DmlDelta); 5] = [
        &|dml| dml.block = other,
        &|dml| dml.height = 4,
        &|dml| dml.record = testkit::dml(&prev, &wrong, false).record,
        &|dml| dml.snapshot = Some(Arc::new(next.clone().with_block_hash(other))),
        &|dml| dml.snapshot = Some(Arc::new(wrong.clone())),
    ];
    for (i, change) in changes.iter().enumerate() {
        let mut bad = d.clone();
        change(bad.dml.as_mut().unwrap());
        assert!(matches!(codec.rows(&bad), Err(StoreError::Inconsistent { .. })), "change {i}");
    }
}

/// A disconnect's tombstones name both rows of each erased commitment.
#[test]
fn tombstones_name_both_rows() {
    let mut chain = MemChain::new();
    let at = chain.extend(chain.genesis(), 4);
    let base = chain.ancestor(at, 2).unwrap();
    let records = [
        testkit::mined(&chain, LlmqType::Llmq50_60, base, None),
        testkit::mined(&chain, LlmqType::LlmqTestDip0024, base, Some(0)),
    ];
    let rows = testkit::codec().rows(&testkit::undo(&chain, at, &records)).unwrap();
    assert_eq!(
        rows.erased,
        [
            mined_key(LlmqType::Llmq50_60, &records[0].quorum_hash),
            index_key(LlmqType::Llmq50_60, None, 4),
            mined_key(LlmqType::LlmqTestDip0024, &records[1].quorum_hash),
            index_key(LlmqType::LlmqTestDip0024, Some(0), 4),
        ]
    );
    assert_eq!(rows.evo_tip, chain.hash(chain.prev(at).unwrap()));
    assert!(rows.by_hash.is_empty() && rows.mined.is_empty() && rows.index.is_empty());
    // An erase of another block's rows, or a delta that also mines, is refused.
    let mut bad = testkit::undo(&chain, at, &records);
    bad.erased_mined[1].mined_height = 3;
    assert!(matches!(testkit::codec().rows(&bad), Err(StoreError::Inconsistent { .. })));
    let mut bad = testkit::undo(&chain, at, &records);
    bad.mined_commitments = vec![records[0].clone()];
    assert!(matches!(testkit::codec().rows(&bad), Err(StoreError::Inconsistent { .. })));
    // So is a delta naming no evo tip, which `MemEvoStore` defaults to its block (D40).
    let mut bad = testkit::undo(&chain, at, &records);
    bad.evo_tip = None;
    assert!(matches!(testkit::codec().rows(&bad), Err(StoreError::Inconsistent { .. })));
}
