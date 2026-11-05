//! The credit pool's rules on crafted blocks (EV3-2): the indexes set and its codec, the block
//! data, the three limit eras, the per-block diff and the store's records. Core's own answers on
//! mined chains are node-l1's `diff_credit_pool`.

use std::sync::Arc;

use dash_consensus::MAX_MONEY;
use dash_consensus::payments::PaymentParams;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut};

use super::*;

const P2PKH: [u8; 3] = [0x76, 0xa9, 0x14];
const RETURN: [u8; 2] = [0x6a, 0x00];

fn out(value: Amount, script: &[u8]) -> TxOut {
    TxOut {
        value,
        script_pubkey: script.to_vec().into(),
    }
}

fn tx(version: i16, tx_type: u16, outputs: Vec<TxOut>, payload: Vec<u8>) -> TxRef {
    let input = TxIn {
        prevout: OutPoint {
            hash: TxHash::from([7; 32]),
            index: 0,
        },
        script_sig: Default::default(),
        sequence: u32::MAX,
    };
    let inputs = if tx_type == 9 {
        vec![]
    } else {
        vec![input]
    };
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs,
        outputs,
        lock_time: 0,
        extra_payload: payload,
    }))
}

/// A CbTx payload of `version` (2 or 3) at height 100, with `balance` from version 3.
fn cbtx_payload(version: u16, balance: Amount) -> Vec<u8> {
    let mut p = version.to_le_bytes().to_vec();
    p.extend(100i32.to_le_bytes());
    p.extend([0x11; 32]);
    p.extend([0x22; 32]);
    if version >= 3 {
        p.push(0);
        p.extend([0; 96]);
        p.extend(balance.to_le_bytes());
    }
    p
}

fn coinbase(payload: Vec<u8>) -> TxRef {
    let mut cb = (*tx(3, 5, vec![out(COIN, &P2PKH)], payload)).to_mutable();
    cb.inputs[0].prevout = OutPoint {
        hash: TxHash::from([0; 32]),
        index: u32::MAX,
    };
    cb.inputs[0].script_sig = vec![1, 100].into();
    Arc::new(Tx::new(cb))
}

/// An asset unlock payload (version 1, `index`, `fee`, requested height 100, a quorum hash, a
/// zero signature), then `trailing` bytes.
fn unlock_payload(index: u64, fee: u32, trailing: usize) -> Vec<u8> {
    let mut p = vec![1];
    p.extend(index.to_le_bytes());
    p.extend(fee.to_le_bytes());
    p.extend(100u32.to_le_bytes());
    p.extend([0xab; 32]);
    p.extend([0; 96]);
    p.extend(vec![0; trailing]);
    p
}

fn unlock(index: u64, fee: u32, outputs: Vec<TxOut>) -> TxRef {
    tx(3, 9, outputs, unlock_payload(index, fee, 0))
}

/// An asset lock of `credit` to one credit output, with `outputs`.
fn lock(credit: Amount, outputs: Vec<TxOut>) -> TxRef {
    let mut p = vec![1, 1];
    p.extend(credit.to_le_bytes());
    p.push(P2PKH.len() as u8);
    p.extend(P2PKH);
    tx(3, 8, outputs, p)
}

fn block(txs: Vec<TxRef>) -> Block {
    let params =
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest params");
    let genesis = dash_consensus::params::genesis_block(&params);
    Block::new(dash_consensus::Header::new(genesis.header), txs)
}

fn set(values: impl IntoIterator<Item = u64>) -> RangesSet {
    let mut s = RangesSet::default();
    for v in values {
        assert!(s.add(v), "{v} is new");
    }
    s
}

fn encoded(ranges: &[(u64, u64)]) -> Vec<u8> {
    let mut bytes = vec![ranges.len() as u8];
    for (begin, end) in ranges {
        bytes.extend(begin.to_le_bytes());
        bytes.extend(end.to_le_bytes());
    }
    bytes
}

// Core: util/ranges_set.cpp:27-58, 92-111
#[test]
fn ranges_add_merges_neighbours() {
    let mut s = set([5, 7]);
    assert_eq!(s.ranges().collect::<Vec<_>>(), [(5, 6), (7, 8)]);
    assert!(!s.add(5));
    assert!(s.add(6));
    assert_eq!(s.ranges().collect::<Vec<_>>(), [(5, 8)]);
    assert!(s.add(4) && s.add(8));
    assert_eq!(s.ranges().collect::<Vec<_>>(), [(4, 9)]);
    assert_eq!((s.size(), s.range_count()), (5, 1));
    assert!((4..9).all(|v| s.contains(v)));
    assert!(!s.contains(3) && !s.contains(9));
    assert!(RangesSet::default().is_empty() && !s.is_empty());
}

/// CONS-10: the range holding `u64::MAX` ends at 0, and the index below it merges into it.
// Core: util/ranges_set.cpp:13-16 WrappedSuccessor
#[test]
fn ranges_wrap_at_the_top_index() {
    let mut s = set([u64::MAX]);
    assert_eq!(s.ranges().collect::<Vec<_>>(), [(u64::MAX, 0)]);
    assert!(s.contains(u64::MAX) && !s.contains(0));
    assert!(s.add(u64::MAX - 1));
    assert_eq!(s.ranges().collect::<Vec<_>>(), [(u64::MAX - 1, 0)]);
    assert!(!s.add(u64::MAX) && !s.add(u64::MAX - 1));
    assert_eq!(s.size(), 2);
    // 0 is not the top's successor in the set's order: two ranges, the wrapped one last.
    assert!(s.add(0));
    assert_eq!(s.ranges().collect::<Vec<_>>(), [(0, 1), (u64::MAX - 1, 0)]);
    assert_eq!((s.size(), s.range_count()), (3, 2));

    let mut bytes = Vec::new();
    s.encode(&mut bytes);
    assert_eq!(bytes, encoded(&[(0, 1), (u64::MAX - 1, 0)]));
    assert_eq!(RangesSet::decode(&mut bytes.as_slice()), Ok(s));
}

// Core: util/ranges_set.h:93-125 UnserializeBounded
#[test]
fn ranges_decode_only_the_canonical_form() {
    let cases: &[(&[(u64, u64)], RangesDecodeError)] = &[
        (&[(0, 0)], RangesDecodeError::FullDomain),
        (&[(5, 5)], RangesDecodeError::Empty),
        (&[(6, 5)], RangesDecodeError::Empty),
        (&[(1, 3), (3, 4)], RangesDecodeError::Noncanonical),
        (&[(1, 3), (2, 4)], RangesDecodeError::Noncanonical),
        (&[(5, 6), (1, 2)], RangesDecodeError::Noncanonical),
        (&[(5, 0), (1, 2)], RangesDecodeError::WrappedNotLast),
        (&[(5, 0), (6, 0)], RangesDecodeError::WrappedNotLast),
        // Both fire at the second range: Core checks the order first.
        (&[(1, 3), (2, 0), (9, 10)], RangesDecodeError::Noncanonical),
    ];
    for (ranges, error) in cases {
        assert_eq!(RangesSet::decode(&mut encoded(ranges).as_slice()), Err(*error), "{ranges:?}");
    }
    assert_eq!(
        RangesSet::decode_bounded(&mut encoded(&[(1, 2), (3, 4)]).as_slice(), 1),
        Err(RangesDecodeError::Oversized)
    );
    // Exactly the bound decodes.
    assert_eq!(
        RangesSet::decode_bounded(&mut encoded(&[(1, 2), (3, 4)]).as_slice(), 2),
        Ok(set([1, 3]))
    );
    assert_eq!(
        RangesSet::decode(&mut &encoded(&[(1, 2)])[..9]),
        Err(RangesDecodeError::Read(ReadErrorKind::EndOfData))
    );
    let good = encoded(&[(1, 3), (4, 6), (9, 0)]);
    let mut input = good.as_slice();
    let decoded = RangesSet::decode(&mut input).expect("canonical");
    assert!(input.is_empty());
    assert_eq!(decoded.size(), 4 + (u64::MAX - 9 + 1));
    assert_eq!(
        RangesDecodeError::WrappedNotLast.to_string(),
        "wrapped CRangesSet range must be last"
    );
}

#[test]
fn pool_codec_round_trips() {
    let pool = CreditPool {
        locked: 3 * COIN,
        current_limit: COIN,
        lately_unlocked: -1,
        indexes: set([1, 2, u64::MAX]),
    };
    let mut bytes = Vec::new();
    pool.encode(&mut bytes);
    assert_eq!(&bytes[..8], &(3 * COIN).to_le_bytes());
    assert_eq!(CreditPool::decode(&mut bytes.as_slice()), Ok(pool));
    assert!(CreditPool::decode(&mut &bytes[..20]).is_err());
}

// Core: evo/creditpool.h:148-158
#[test]
fn v24_limit() {
    // 20% of the window start's balance, at least 2000 DASH, less what the balance dropped.
    assert_eq!(unlock_limit_v24(20_000 * COIN, 20_000 * COIN), 4000 * COIN);
    assert_eq!(unlock_limit_v24(19_000 * COIN, 20_000 * COIN), 3000 * COIN);
    assert_eq!(unlock_limit_v24(5000 * COIN, 5000 * COIN), 2000 * COIN);
    assert_eq!(unlock_limit_v24(1000 * COIN, 5000 * COIN), 0);
    // Never more than the balance.
    assert_eq!(unlock_limit_v24(100 * COIN, 0), 100 * COIN);
    assert_eq!(unlock_limit_v24(0, 0), 0);
}

// Core: evo/creditpool.cpp:64-114
#[test]
fn block_data() {
    let unlocks = vec![
        coinbase(cbtx_payload(3, 50 * COIN)),
        unlock(9, 1000, vec![out(COIN, &P2PKH), out(2 * COIN, &P2PKH)]),
        tx(1, 0, vec![out(5 * COIN, &P2PKH)], vec![]),
        unlock(3, 0, vec![out(COIN, &P2PKH)]),
    ];
    let b = block(unlocks.clone());
    assert_eq!(credit_block_data(&b, false), Ok(None));
    let read = credit_block_data(&b, true).unwrap().expect("a CbTx");
    assert_eq!(
        read,
        BlockCredit {
            data: CreditBlockData {
                balance: 50 * COIN,
                unlocked: 4 * COIN + 1000,
            },
            indexes: vec![3, 9],
        }
    );

    // Two unlocks with one index: the index once (`std::set`, `cp:53, 102`), both amounts.
    let twice = block(vec![
        coinbase(cbtx_payload(3, 0)),
        unlock(7, 0, vec![out(COIN, &P2PKH)]),
        unlock(7, 0, vec![out(2 * COIN, &P2PKH)]),
    ]);
    let read = credit_block_data(&twice, true).unwrap().expect("a CbTx");
    assert_eq!((read.data.unlocked, read.indexes), (3 * COIN, vec![7]));

    // No CbTx data: an empty payload, a coinbase below version 3, trailing bytes.
    assert_eq!(credit_block_data(&block(vec![coinbase(vec![])]), true), Ok(None));
    let mut old = coinbase(cbtx_payload(3, COIN)).to_mutable();
    old.version = 2;
    assert_eq!(credit_block_data(&block(vec![Arc::new(Tx::new(old))]), true), Ok(None));
    let mut trailing = cbtx_payload(3, COIN);
    trailing.push(0);
    assert_eq!(credit_block_data(&block(vec![coinbase(trailing)]), true), Ok(None));
    // A version-2 CbTx has no balance: zeros.
    let v2 = credit_block_data(&block(vec![coinbase(cbtx_payload(2, 0))]), true).unwrap();
    assert_eq!(v2, Some(BlockCredit::default()));

    // An unlock Core cannot read throws.
    let bad_payload = tx(3, 9, vec![], unlock_payload(1, 0, 1));
    let b = block(vec![coinbase(cbtx_payload(3, 0)), bad_payload]);
    let err = credit_block_data(&b, true).unwrap_err();
    assert_eq!(err, PoolError::UnlockData(UnlockDataReason::Payload));
    assert_eq!(
        err.to_string(),
        "GetCreditDataFromBlock: GetDataFromUnlockTx failed: failed-creditpool-unlock-payload"
    );
    let b = block(vec![coinbase(cbtx_payload(3, 0)), unlock(1, 0, vec![out(-1, &P2PKH)])]);
    assert_eq!(
        credit_block_data(&b, true),
        Err(PoolError::UnlockData(UnlockDataReason::TxoutOutOfRange))
    );
}

fn credit(balance: Amount, unlocked: Amount, indexes: Vec<u64>) -> BlockCredit {
    BlockCredit {
        data: CreditBlockData {
            balance,
            unlocked,
        },
        indexes,
    }
}

const PRE_V22: PoolEras = PoolEras {
    v24: false,
    withdrawals: false,
};
const V22: PoolEras = PoolEras {
    v24: false,
    withdrawals: true,
};
const V24: PoolEras = PoolEras {
    v24: true,
    withdrawals: true,
};

// Core: evo/creditpool.cpp:173-247
#[test]
fn construct_without_block_data() {
    let empty = CreditPool::default();
    assert_eq!(construct_pool(&empty, None, None, V24), Ok(CreditPool::default()));
    let locked = CreditPool {
        locked: 1,
        ..CreditPool::default()
    };
    assert_eq!(construct_pool(&locked, None, None, V24), Err(PoolError::PrevLocked));
    let used = CreditPool {
        indexes: set([4]),
        ..CreditPool::default()
    };
    assert_eq!(construct_pool(&used, None, None, V24), Err(PoolError::PrevIndexes));
    assert_eq!(
        PoolError::PrevIndexes.to_string(),
        "Failed to create CreditPool but asset unlock transactions already mined"
    );
}

#[test]
fn construct_adds_indexes_and_slides_the_window() {
    let prev = CreditPool {
        locked: 900 * COIN,
        current_limit: 0,
        lately_unlocked: 30 * COIN,
        indexes: set([1]),
    };
    let window = CreditBlockData {
        balance: 1000 * COIN,
        unlocked: 10 * COIN,
    };
    let pool =
        construct_pool(&prev, Some(&credit(800 * COIN, 5 * COIN, vec![2, 7])), Some(&window), V22)
            .unwrap();
    assert_eq!(pool.locked, 800 * COIN);
    assert_eq!(pool.lately_unlocked, 25 * COIN);
    assert_eq!(pool.indexes, set([1, 2, 7]));
    // WITHDRAWALS: the balance, at most 2000 DASH.
    assert_eq!(pool.current_limit, 800 * COIN);
    let big = construct_pool(&prev, Some(&credit(5000 * COIN, 0, vec![])), None, V22).unwrap();
    assert_eq!(big.current_limit, LIMIT_AMOUNT_V22);
    // V24 reads the window start's balance: 20 % of 21,000 DASH less the 1000 DASH drop since,
    // where WITHDRAWALS would give 2000 DASH, and a window balance of zero or of the window's
    // unlocked amount the whole balance.
    let wide = CreditBlockData {
        balance: 21_000 * COIN,
        unlocked: 10 * COIN,
    };
    let at = |eras| {
        construct_pool(&prev, Some(&credit(20_000 * COIN, 0, vec![])), Some(&wide), eras)
            .map(|p| p.current_limit)
    };
    assert_eq!(at(V24), Ok(3200 * COIN));
    assert_eq!(at(V22), Ok(LIMIT_AMOUNT_V22));
    assert_eq!(unlock_limit_v24(20_000 * COIN, 0), 20_000 * COIN);
    assert_eq!(unlock_limit_v24(20_000 * COIN, 10 * COIN), 20_000 * COIN);

    // A used index throws, before the window is read.
    assert_eq!(
        construct_pool(&prev, Some(&credit(0, 0, vec![1])), None, V24),
        Err(PoolError::IndexDuplicated)
    );
    assert_eq!(
        PoolError::IndexDuplicated.to_string(),
        "ConstructCreditPool: failed-getcreditpool-index-duplicated"
    );
}

#[test]
fn construct_pre_v22_limits() {
    let limit = |balance, lately| {
        let prev = CreditPool {
            lately_unlocked: lately,
            ..CreditPool::default()
        };
        construct_pool(&prev, Some(&credit(balance, 0, vec![])), None, PRE_V22)
            .map(|p| p.current_limit)
    };
    // Up to 100 DASH in the window, the whole balance.
    assert_eq!(limit(60 * COIN, 40 * COIN), Ok(60 * COIN));
    // Above it, max(100, balance / 10) less what the window unlocked, at most 1000 less it.
    assert_eq!(limit(500 * COIN, 40 * COIN), Ok(60 * COIN));
    assert_eq!(limit(5000 * COIN, 40 * COIN), Ok(460 * COIN));
    assert_eq!(limit(50_000 * COIN, 40 * COIN), Ok(960 * COIN));
    assert_eq!(limit(500 * COIN, 150 * COIN), Ok(0));
    // A window that unlocked more than 1000 DASH leaves a negative limit: Core throws.
    let negative = limit(500 * COIN, 1000 * COIN + 5);
    assert_eq!(negative, Err(PoolError::NegativeLimit(-5)));
    assert_eq!(negative.unwrap_err().to_string(), "Negative limit for CreditPool: 0.-0000005\n");
    assert_eq!(
        PoolError::NegativeLimit(-3 * COIN - 5).to_string(),
        "Negative limit for CreditPool: -3.-0000005\n"
    );
}

fn params() -> PaymentParams {
    let chain =
        ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("Core's parameters");
    PaymentParams::from_chain_params(&chain).expect("payment parameters")
}

// Core: evo/creditpool.cpp:291-294
#[test]
fn platform_reward_from_mn_rr() {
    let p = params();
    assert_eq!(platform_reward(false, 2_200_000, 2 * COIN, &p), Ok(0));
    let mn = masternode_payment(2_200_000, 2 * COIN, MnRewardEra::EvoReward, &p);
    assert_eq!(platform_reward(true, 2_200_000, 2 * COIN, &p), platform_share(mn));
    assert!(platform_reward(true, 2_200_000, 2 * COIN, &p).unwrap() > 0);
}

fn starter() -> CreditPool {
    CreditPool {
        locked: 100 * COIN,
        current_limit: 10 * COIN,
        lately_unlocked: 0,
        indexes: set([1]),
    }
}

fn reason<T: fmt::Debug>(r: Result<T, TxValidationError>) -> String {
    r.unwrap_err().reject_reason().to_owned()
}

// Core: evo/creditpool.cpp:307-369
#[test]
fn diff_locks_and_unlocks() {
    let mut diff = CreditPoolDiff::new(starter(), 7);
    // The first OP_RETURN output is the locked amount.
    let l = lock(3 * COIN, vec![out(COIN, &P2PKH), out(3 * COIN, &RETURN), out(COIN, &RETURN)]);
    assert_eq!(diff.process_lock_unlock_transaction(&l), Ok(None));
    assert_eq!(
        diff.process_lock_unlock_transaction(&unlock(2, 100, vec![out(COIN, &P2PKH)])),
        Ok(Some(2))
    );
    assert_eq!(diff.total_locked(), Some(100 * COIN + 3 * COIN - (COIN + 100) + 7));

    // Not special, or another special type: nothing.
    assert_eq!(diff.process_lock_unlock_transaction(&tx(1, 9, vec![], vec![])), Ok(None));
    assert_eq!(diff.process_lock_unlock_transaction(&tx(3, 1, vec![], vec![0])), Ok(None));

    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&lock(COIN, vec![out(COIN, &P2PKH)]))),
        "failed-creditpool-lock-invalid"
    );
    // An empty script is not an OP_RETURN.
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&lock(COIN, vec![out(COIN, &[])]))),
        "failed-creditpool-lock-invalid"
    );
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&tx(3, 8, vec![], vec![9]))),
        "failed-creditpool-lock-payload"
    );
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&tx(3, 9, vec![], unlock_payload(5, 0, 1)))),
        "failed-creditpool-unlock-payload"
    );
    // Index 1 is in the pool, 2 in the block.
    for index in [1, 2] {
        assert_eq!(
            reason(diff.process_lock_unlock_transaction(&unlock(
                index,
                0,
                vec![out(COIN, &P2PKH)]
            ))),
            "failed-creditpool-unlock-duplicated-index"
        );
    }
    // The limit counts every unlock of the block, fees included; the limit check comes first.
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&unlock(1, 0, vec![out(9 * COIN, &P2PKH)]))),
        "failed-creditpool-unlock-too-much"
    );
    assert_eq!(
        diff.process_lock_unlock_transaction(&unlock(3, 0, vec![out(9 * COIN - 100, &P2PKH)])),
        Ok(Some(3))
    );
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&unlock(4, 1, vec![]))),
        "failed-creditpool-unlock-too-much"
    );
    assert_eq!(diff.pool(), &starter());
}

/// Outputs of `MAX_MONEY` each: 4392 of them stay below `i64::MAX`, 4393 do not.
fn max_money_outputs(n: usize) -> Vec<TxOut> {
    vec![out(MAX_MONEY, &P2PKH); n]
}

/// CE4-05: rc.4's checked sums (`creditpool.cpp` rc.4:46-50, 316-320, 336-337) where rc.3's
/// overflowed.
// Core: evo/creditpool.cpp:33-53, 307-349
#[test]
fn diff_sums_are_checked() {
    assert!(4392 * MAX_MONEY < i64::MAX - i64::from(u32::MAX));
    // An unlock whose fee and outputs overflow.
    let mut diff = CreditPoolDiff::new(starter(), 0);
    let over = unlock(5, 0, max_money_outputs(4393));
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&over)),
        "failed-creditpool-unlock-overflow"
    );
    let b = block(vec![coinbase(cbtx_payload(3, 0)), over]);
    let err = credit_block_data(&b, true).unwrap_err();
    assert_eq!(err, PoolError::UnlockData(UnlockDataReason::Overflow));
    assert_eq!(
        err.to_string(),
        "GetCreditDataFromBlock: GetDataFromUnlockTx failed: failed-creditpool-unlock-overflow"
    );

    // The session's locks.
    let mut diff = CreditPoolDiff::new(starter(), 0);
    let all = lock(i64::MAX, vec![out(i64::MAX, &RETURN)]);
    assert_eq!(diff.process_lock_unlock_transaction(&all), Ok(None));
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&lock(1, vec![out(1, &RETURN)]))),
        "failed-creditpool-lock-overflow"
    );

    // The session's unlocks: an overflowing sum is too much, under any limit.
    let mut diff = CreditPoolDiff::new(
        CreditPool {
            current_limit: i64::MAX,
            ..starter()
        },
        0,
    );
    let first = unlock(5, u32::MAX, max_money_outputs(4392));
    assert_eq!(diff.process_lock_unlock_transaction(&first), Ok(Some(5)));
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&unlock(6, 0, max_money_outputs(4392)))),
        "failed-creditpool-unlock-too-much"
    );
    // The index was not taken.
    assert_eq!(diff.process_lock_unlock_transaction(&unlock(6, 0, vec![])), Ok(Some(6)));
}

/// CE4-05: `GetTotalLocked` subtracts the unlocks first and throws where a step overflows
/// (`creditpool.cpp` rc.4:297-305), which `CheckCreditPoolDiffForBlock` catches as
/// `failed-checkcreditpooldiff` (`specialtxman.cpp` rc.4:1121-1124).
#[test]
fn the_total_is_checked() {
    let pool = |locked| CreditPool {
        locked,
        current_limit: i64::MAX,
        ..starter()
    };
    // `locked - unlocked + locked_in + reward`, each step checked.
    let mut diff = CreditPoolDiff::new(pool(i64::MAX), 0);
    let txs = [lock(1, vec![out(1, &RETURN)]), unlock(5, 1, vec![])];
    diff.process_lock_unlock_transactions(&txs).unwrap();
    assert_eq!(diff.total_locked(), Some(i64::MAX));
    let mut diff = CreditPoolDiff::new(pool(i64::MAX), 1);
    diff.process_lock_unlock_transactions(&txs).unwrap();
    assert_eq!(diff.total_locked(), None);
    let mut diff = CreditPoolDiff::new(pool(i64::MIN), 0);
    diff.process_lock_unlock_transactions(&txs).unwrap();
    assert_eq!(diff.total_locked(), None);
    let mut diff = CreditPoolDiff::new(pool(i64::MAX), 0);
    diff.process_lock_unlock_transactions(&txs[..1]).unwrap();
    assert_eq!(diff.total_locked(), None);

    let b = block(vec![coinbase(cbtx_payload(3, 0)), lock(1, vec![out(1, &RETURN)])]);
    let err = check_block_diff(&b, 0, &pool(i64::MAX), 0, true, true).unwrap_err();
    assert_eq!(err.result(), BlockValidationResult::Consensus);
    assert_eq!(err.reject_reason(), "failed-checkcreditpooldiff");
}

// Core: evo/creditpool.cpp:371-395
#[test]
fn diff_package_rolls_back() {
    let mut diff = CreditPoolDiff::new(starter(), 0);
    diff.process_lock_unlock_transactions(&[unlock(2, 0, vec![out(COIN, &P2PKH)])]).unwrap();
    let before = diff.total_locked();
    let package = [
        lock(COIN, vec![out(COIN, &RETURN)]),
        unlock(3, 0, vec![out(COIN, &P2PKH)]),
        unlock(2, 0, vec![out(COIN, &P2PKH)]),
    ];
    assert_eq!(
        reason(diff.process_lock_unlock_transactions(&package)),
        "failed-creditpool-unlock-duplicated-index"
    );
    assert_eq!(diff.total_locked(), before);
    // Index 3 was rolled back, index 2 stays from the earlier package.
    assert_eq!(diff.process_lock_unlock_transaction(&unlock(3, 0, vec![])), Ok(Some(3)));
    assert_eq!(
        reason(diff.process_lock_unlock_transaction(&unlock(2, 0, vec![]))),
        "failed-creditpool-unlock-duplicated-index"
    );
}

// Core: evo/creditpool.cpp:397-426, evo/specialtxman.cpp:1096-1127
#[test]
fn block_diff_against_the_cbtx() {
    let pool = starter();
    let bad = unlock(1, 0, vec![out(COIN, &P2PKH)]);
    let b = block(vec![
        coinbase(cbtx_payload(3, 0)),
        lock(COIN, vec![out(2 * COIN, &RETURN)]),
        bad.clone(),
    ]);
    // Before DIP0008 or V20 at the block nothing is read.
    assert_eq!(check_block_diff(&b, 0, &pool, 0, false, true), Ok(None));
    assert_eq!(check_block_diff(&b, 0, &pool, 0, true, false), Ok(None));
    let err = check_block_diff(&b, 0, &pool, 0, true, true).unwrap_err();
    assert_eq!(err.result(), BlockValidationResult::Consensus);
    assert_eq!(err.reject_reason(), "failed-creditpool-unlock-duplicated-index");
    assert_eq!(
        err.debug_message(),
        format!("Process Lock/Unlock Transaction failed at Credit Pool (tx hash {}) ", bad.txid())
    );

    // A version-2 unlock (V24's stable txid): the text names its txid, not its instance hash.
    let mut v2 = unlock_payload(1, 0, 0);
    v2[0] = 2;
    let v2 = tx(3, 9, vec![out(COIN, &P2PKH)], v2);
    assert_ne!(v2.txid(), v2.instance_hash());
    let b = block(vec![coinbase(cbtx_payload(3, 0)), v2.clone()]);
    let err = check_block_diff(&b, 0, &pool, 0, true, true).unwrap_err();
    assert_eq!(err.reject_reason(), "failed-creditpool-unlock-duplicated-index");
    assert_eq!(
        err.debug_message(),
        format!("Process Lock/Unlock Transaction failed at Credit Pool (tx hash {}) ", v2.txid())
    );

    // The coinbase is skipped, even when it is an asset lock.
    let b = block(vec![
        lock(COIN, vec![]),
        lock(COIN, vec![out(2 * COIN, &RETURN)]),
        unlock(5, 10, vec![out(COIN, &P2PKH)]),
    ]);
    let total = 100 * COIN + 2 * COIN - (COIN + 10) + 3;
    assert_eq!(check_block_diff(&b, total, &pool, 3, true, true), Ok(Some(total)));
    let err = check_block_diff(&b, total + 1, &pool, 3, true, true).unwrap_err();
    assert_eq!(err.reject_reason(), "bad-cbtx-assetlocked-amount");
}

// EV3 D8, D9: what connecting writes.
#[test]
fn connect_records_follow_the_deployments() {
    let all = ConnectAt {
        dip3: true,
        v20: true,
        eras: V24,
    };
    let b =
        block(vec![coinbase(cbtx_payload(3, 40 * COIN)), unlock(2, 0, vec![out(COIN, &P2PKH)])]);
    let prev = CreditPool {
        locked: 41 * COIN,
        ..CreditPool::default()
    };
    let r = connect_records(&b, all, &prev, None);
    assert_eq!(
        r.data,
        Some(CreditBlockData {
            balance: 40 * COIN,
            unlocked: COIN,
        })
    );
    let pool = r.pool.unwrap().unwrap();
    assert_eq!((pool.locked, pool.lately_unlocked, pool.indexes), (40 * COIN, COIN, set([2])));
    assert_eq!(pool.current_limit, 40 * COIN);

    // The limit follows the era and reads the window start's balance (as construct_pool does).
    let big = block(vec![
        coinbase(cbtx_payload(3, 20_000 * COIN)),
        unlock(2, 0, vec![out(COIN, &P2PKH)]),
    ]);
    let wide = CreditBlockData {
        balance: 21_000 * COIN,
        unlocked: 10 * COIN,
    };
    let rich = CreditPool {
        locked: 20_001 * COIN,
        ..CreditPool::default()
    };
    let limit = |eras| {
        let at = ConnectAt {
            eras,
            ..all
        };
        connect_records(&big, at, &rich, Some(&wide)).pool.unwrap().unwrap().current_limit
    };
    assert_eq!(limit(V24), 3200 * COIN);
    assert_eq!(limit(V22), LIMIT_AMOUNT_V22);

    let pre_v20 = ConnectAt {
        v20: false,
        ..all
    };
    assert_eq!(connect_records(&b, pre_v20, &prev, None).pool, None);
    assert_eq!(connect_records(&b, ConnectAt::default(), &prev, None), CreditRecords::default());

    // No CbTx: zeros, and the parent's balance makes Core's construction throw.
    let bare = block(vec![coinbase(vec![])]);
    let r = connect_records(&bare, all, &prev, None);
    assert_eq!(r.data, Some(CreditBlockData::default()));
    assert_eq!(r.pool, Some(Err(PoolError::PrevLocked)));

    // A data read that throws: no data, and the pool is the error.
    let bad = block(vec![coinbase(cbtx_payload(3, 0)), unlock(1, 0, vec![out(-1, &P2PKH)])]);
    let r = connect_records(&bad, all, &prev, None);
    assert_eq!(r.data, None);
    assert_eq!(r.pool, Some(Err(PoolError::UnlockData(UnlockDataReason::TxoutOutOfRange))));
}
