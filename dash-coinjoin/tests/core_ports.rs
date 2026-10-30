//! Dash Core v24.0.0-rc.3's CoinJoin unit tests, ported case by case. Each test names the Core
//! test it ports (`src/test/coinjoin_queue_tests.cpp`, `coinjoin_dstxmanager_tests.cpp`,
//! `coinjoin_inouts_tests.cpp`, `src/wallet/test/coinjoin_tests.cpp`). Core's fixtures run on
//! mainnet's parameters (3 to 20 participants), as [`mainnet_limits`] does.

mod common;

use std::collections::BTreeMap;

use common::*;
use dash_coinjoin::denom::{self, MixShape, MixSideCounts, PROMOTION_RATIO, SMALLEST_DENOMINATION};
use dash_coinjoin::dstx::{self, DstxManager, PoolLimits};
use dash_coinjoin::inouts::{
    InOutsKind, is_valid_in_outs, validate_demotion_entry, validate_final_tx_composition,
    validate_promotion_entry,
};
use dash_coinjoin::pool::{
    AcceptFlags, ENTRY_MAX_SIZE, FLAG_DEMOTION, FLAG_PROMOTION, PoolMessage, QUEUE_TIMEOUT,
};
use dash_coinjoin::queue::{self, QueueEntry, QueueManager};
use dash_coinjoin::rounds::{self, RoundsCache, RoundsOptions, RoundsWallet};
use dash_p2p_core::{DsAccept, DsQueue, DsTx};
use dash_pkc::bls::core::BlsVerdict;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut};
use dash_types::Hashable;

// ---------------------------------------------------------------------------------------------
// coinjoin_queue_tests.cpp
// ---------------------------------------------------------------------------------------------

/// `MakeQueue(denom, time, fReady, outpoint)` (coinjoin_queue_tests.cpp:30), ProRegTx `ONE`.
fn make_queue(denom: i32, time: i64, ready: bool, mn: OutPoint) -> QueueEntry {
    QueueEntry::new(
        DsQueue {
            denom,
            pro_tx_hash: u256(1),
            time,
            ready: ready.into(),
            sig: Vec::new(),
        },
        mn,
    )
}

/// Core: test/coinjoin_queue_tests.cpp:35 queue_sign_and_verify
#[test]
fn queue_sign_and_verify() {
    let op = Operator::new(7);
    let mut q = make_queue(SMALLEST, NOW, false, outpoint(u256(0xaa), 1));
    q.dsq.sig = op.sign(&q.dsq.signature_hash());
    assert_eq!(queue::check_queue_signature(&q.dsq, &op.public()), BlsVerdict::Valid);

    // Another key, or another queue, does not verify.
    assert_eq!(
        queue::check_queue_signature(&q.dsq, &Operator::new(8).public()),
        BlsVerdict::Invalid
    );
    q.dsq.time += 1;
    assert_eq!(queue::check_queue_signature(&q.dsq, &op.public()), BlsVerdict::Invalid);
}

/// Core: test/coinjoin_queue_tests.cpp:48 queue_hashes_and_equality
#[test]
fn queue_hashes_and_equality() {
    let a = make_queue(SMALLEST, NOW, true, outpoint(u256(0xbb), 2));
    let b = a.clone();
    assert!(a.same_queue(&b));
    assert_eq!(a.hash(), b.hash());
    assert_eq!(a.dsq.signature_hash(), b.dsq.signature_hash());
    // The hash is of the queue as written.
    assert_eq!(a.hash(), a.dsq.hash());
}

/// Core: test/coinjoin_queue_tests.cpp:59 queue_denomination_validation
#[test]
fn queue_denomination_validation() {
    assert!(denom::is_valid_denomination(denom::amount_to_denomination(SMALLEST_DENOMINATION)));
    assert!(!denom::is_valid_denomination(0));
    assert!(!denom::is_valid_denomination(-1));
    assert!(!denom::is_valid_denomination(999));
}

/// Core: test/coinjoin_queue_tests.cpp:72 queue_timestamp_validation and
/// coinjoin_inouts_tests.cpp:517 queue_timeout_bounds
#[test]
fn queue_timestamp_validation() {
    assert!(!queue::is_time_out_of_bounds(NOW, NOW));
    assert!(!queue::is_time_out_of_bounds(NOW + 15, NOW));
    assert!(!queue::is_time_out_of_bounds(NOW - 15, NOW));
    assert!(queue::is_time_out_of_bounds(NOW + 60, NOW));
    assert!(queue::is_time_out_of_bounds(NOW - 60, NOW));
    // The boundary: exactly QUEUE_TIMEOUT apart is in bounds.
    assert!(!queue::is_time_out_of_bounds(NOW, NOW + QUEUE_TIMEOUT));
    assert!(queue::is_time_out_of_bounds(NOW, NOW + QUEUE_TIMEOUT + 1));
    assert!(queue::is_time_out_of_bounds(NOW + QUEUE_TIMEOUT + 1, NOW));
}

/// Core: test/coinjoin_queue_tests.cpp:99 queue_timestamp_extreme_values
#[test]
fn queue_timestamp_extreme_values() {
    assert!(queue::is_time_out_of_bounds(i64::MIN, i64::MAX));
    assert!(queue::is_time_out_of_bounds(i64::MAX, i64::MIN));
    assert!(queue::is_time_out_of_bounds(i64::MIN, i64::MIN));
    assert!(!queue::is_time_out_of_bounds(i64::MAX, i64::MAX));
    assert!(queue::is_time_out_of_bounds(0, i64::MAX));
    assert!(queue::is_time_out_of_bounds(0, i64::MIN));
}

/// Core: test/coinjoin_queue_tests.cpp:125 calculate_amount_priority_guard
#[test]
fn calculate_amount_priority_guard() {
    assert_eq!(
        denom::calculate_amount_priority(denom::MAX_MONEY),
        -((denom::MAX_MONEY / denom::COIN) as i32)
    );
    assert_eq!(denom::calculate_amount_priority(i64::MAX), 0);
    assert_eq!(denom::calculate_amount_priority(-1), 0);
    assert_eq!(denom::calculate_amount_priority(denom::MAX_MONEY + 1), 0);
}

/// Core: test/coinjoin_queue_tests.cpp:142 queuemanager_checkqueue_removes_timeouts
#[test]
fn queuemanager_checkqueue_removes_timeouts() {
    let mut man = QueueManager::default();
    man.add(make_queue(SMALLEST, NOW, false, outpoint(u256(0x11), 0)));
    man.add(make_queue(SMALLEST, NOW - (QUEUE_TIMEOUT + 1), false, outpoint(u256(0x12), 0)));
    assert_eq!(man.len(), 2);
    man.check_queue(NOW);
    assert_eq!(man.len(), 1);
    assert_eq!(man.queues()[0].masternode_outpoint, outpoint(u256(0x11), 0));
}

/// Core: test/coinjoin_queue_tests.cpp:158 queuemanager_getqueueitem_marks_tried_once
#[test]
fn queuemanager_getqueueitem_marks_tried_once() {
    let mut man = QueueManager::default();
    man.add(make_queue(SMALLEST, NOW, false, outpoint(u256(0x21), 0)));
    assert!(man.get_queue_item_and_try(NOW, 0).is_some());
    assert!(man.get_queue_item_and_try(NOW, 0).is_none());
}

/// Core: test/coinjoin_queue_tests.cpp:174 queuemanager_getqueueitem_denom_filter_leaves_others_untried
#[test]
fn queuemanager_getqueueitem_denom_filter_leaves_others_untried() {
    let denom_small = SMALLEST;
    let denom_other = denom::amount_to_denomination(SMALLEST_DENOMINATION * 10);
    assert!(denom::is_valid_denomination(denom_other));

    let mut man = QueueManager::default();
    man.add(make_queue(denom_other, NOW, false, outpoint(u256(0x51), 0)));
    man.add(make_queue(denom_small, NOW, false, outpoint(u256(0x52), 0)));

    assert_eq!(
        man.get_queue_item_and_try(NOW, denom_small).expect("filtered").dsq.denom,
        denom_small
    );
    assert!(man.get_queue_item_and_try(NOW, denom_small).is_none());
    assert_eq!(man.get_queue_item_and_try(NOW, 0).expect("unfiltered").dsq.denom, denom_other);
    assert!(man.get_queue_item_and_try(NOW, 0).is_none());
}

/// Core: test/coinjoin_queue_tests.cpp:207 queuemanager_has_queue_from_masternode_readiness
#[test]
fn queuemanager_has_queue_from_masternode_readiness() {
    let mut man = QueueManager::default();
    let mn = outpoint(u256(0x31), 0);
    let other = outpoint(u256(0x32), 0);
    man.add(make_queue(SMALLEST, NOW, false, mn));

    assert!(man.has_queue_from_masternode(&mn, false));
    assert!(!man.has_queue_from_masternode(&mn, true));
    assert!(!man.has_queue_from_masternode(&other, false));
    assert!(!man.has_queue_from_masternode(&other, true));
    // The TRY variant ignores readiness.
    assert!(man.has_any_queue_from_masternode(&mn));
    assert!(!man.has_any_queue_from_masternode(&other));
}

/// Core: test/coinjoin_queue_tests.cpp:235 queuemanager_try_check_duplicate
#[test]
fn queuemanager_try_check_duplicate() {
    let mut man = QueueManager::default();
    let mn = outpoint(u256(0x41), 0);
    let other_mn = outpoint(u256(0x42), 0);
    let seed = make_queue(SMALLEST, NOW, false, mn);
    man.add(seed.clone());

    assert!(man.is_duplicate(&seed));
    assert!(man.is_duplicate(&make_queue(SMALLEST, NOW + 1, false, mn)));
    assert!(!man.is_duplicate(&make_queue(SMALLEST, NOW, true, mn)));
    assert!(!man.is_duplicate(&make_queue(SMALLEST, NOW, false, other_mn)));
}

// ---------------------------------------------------------------------------------------------
// coinjoin_dstxmanager_tests.cpp
// ---------------------------------------------------------------------------------------------

/// `MakeDSTX(count)` (coinjoin_dstxmanager_tests.cpp:21): `count` inputs from `0a` and as many
/// smallest-denomination P2PKH outputs, ProRegTx `ONE`.
fn make_dstx(count: u32) -> DsTx {
    let count = count.max(1);
    let inputs = (0..count).map(|i| txin(outpoint(u256(0x0a), i))).collect();
    let outputs = (0..count).map(|i| txout(SMALLEST_DENOMINATION, p2pkh(i as u8))).collect();
    DsTx {
        tx: tx(inputs, outputs),
        pro_tx_hash: u256(1),
        sig: Vec::new(),
        sig_time: 0,
    }
}

/// Core: test/coinjoin_dstxmanager_tests.cpp:38 add_get_dstx
#[test]
fn add_get_dstx() {
    let dstx = make_dstx(3);
    let txid = dstx::txid(&dstx.tx);
    let mut man = DstxManager::default();
    assert!(man.get(&txid).is_none());
    man.add(dstx.clone());
    assert_eq!(dstx::txid(&man.get(&txid).expect("kept").dstx.tx), txid);
}

/// Core: test/coinjoin_dstxmanager_tests.cpp:53 broadcasttx_expiry_height_logic and
/// coinjoin_inouts_tests.cpp:537 broadcasttx_expiry_height_logic.
///
/// Core's test sets the height on its copy before `AddDSTX`; a received DSTX never carries one,
/// so here the block connection sets it.
#[test]
fn broadcasttx_expiry_height_logic() {
    let dstx = make_dstx(3);
    let txid = dstx::txid(&dstx.tx);
    let mut man = DstxManager::default();
    man.add(dstx);
    man.update_confirmed_height(&txid, Some(100));

    man.check(124, false);
    assert!(man.contains(&txid));
    man.check(125, false);
    assert!(!man.contains(&txid));

    assert!(!dstx::is_expired(Some(100), 124, false));
    assert!(dstx::is_expired(Some(100), 125, false));
    // A ChainLock at the tip expires a confirmed one at once; an unconfirmed one never expires,
    // nor one confirmed above the tip.
    assert!(dstx::is_expired(Some(100), 100, true));
    assert!(!dstx::is_expired(None, 1_000, true));
    assert!(!dstx::is_expired(Some(101), 100, true));
}

/// Core: test/coinjoin_dstxmanager_tests.cpp:92 update_heights_block_connect_disconnect
#[test]
fn update_heights_block_connect_disconnect() {
    let dstx = make_dstx(3);
    let txid = dstx::txid(&dstx.tx);
    let mut man = DstxManager::default();
    man.add(dstx);

    man.update_confirmed_height(&txid, Some(100));
    assert_eq!(man.get(&txid).expect("kept").confirmed_height, Some(100));
    man.update_confirmed_height(&txid, None);
    assert_eq!(man.get(&txid).expect("kept").confirmed_height, None);
}

/// Core: test/coinjoin_dstxmanager_tests.cpp:123 mempool_reentry_clears_confirmed_height and
/// :156 mempool_event_unrelated_tx_does_not_mutate_dstx
#[test]
fn mempool_events_update_only_the_tracked_dstx() {
    let dstx = make_dstx(3);
    let txid = dstx::txid(&dstx.tx);
    let mut other = dstx.tx.clone();
    other.inputs[0].prevout = outpoint(u256(0xee), 7);
    let other_txid = dstx::txid(&other);
    assert_ne!(other_txid, txid);

    let mut man = DstxManager::default();
    man.add(dstx);
    man.update_confirmed_height(&txid, Some(321));
    man.update_confirmed_height(&other_txid, None);
    assert_eq!(man.get(&txid).expect("kept").confirmed_height, Some(321));
    assert!(!man.contains(&other_txid));

    man.update_confirmed_height(&txid, None);
    assert_eq!(man.get(&txid).expect("kept").confirmed_height, None);
}

/// `AddDSTX` keeps the first DSTX for a transaction (`mapDSTX.insert`).
#[test]
fn add_dstx_keeps_the_first() {
    let first = make_dstx(3);
    let txid = dstx::txid(&first.tx);
    let mut second = first.clone();
    second.sig_time = 99;
    let mut man = DstxManager::default();
    man.add(first);
    man.add(second);
    assert_eq!(man.len(), 1);
    assert_eq!(man.get(&txid).expect("kept").dstx.sig_time, 0);
}

// ---------------------------------------------------------------------------------------------
// coinjoin_inouts_tests.cpp: IsValidStructure
// ---------------------------------------------------------------------------------------------

/// A DSTX of `ins` inputs from `ONE` and these outputs, ProRegTx `ONE`.
fn dstx_of(ins: usize, outputs: Vec<TxOut>) -> DsTx {
    let inputs = (0..ins).map(|i| txin(outpoint(u256(1), i as u32))).collect();
    DsTx {
        tx: tx(inputs, outputs),
        pro_tx_hash: u256(1),
        sig: Vec::new(),
        sig_time: 0,
    }
}

/// `n` smallest-denomination outputs with Core's `P2PKHScript(i % 256)`.
fn smallest_outputs(n: usize) -> Vec<TxOut> {
    (0..n).map(|i| txout(SMALLEST_DENOMINATION, p2pkh((i % 256) as u8))).collect()
}

/// Pre-V24 at the tip (Core's `pindex == nullptr`): valid?
fn valid_pre(d: &DsTx) -> bool {
    dstx::is_valid_structure(d, mainnet_limits(), false).valid
}

/// Pre-V24 at the tip: `fPossiblyValidPostV24`.
fn possibly_post(d: &DsTx) -> bool {
    let s = dstx::is_valid_structure(d, mainnet_limits(), false);
    assert!(!s.valid);
    s.possibly_valid_post_v24
}

/// Core: test/coinjoin_inouts_tests.cpp:52 broadcasttx_isvalidstructure_good_and_bad
#[test]
fn broadcasttx_isvalidstructure_good_and_bad() {
    let good = dstx_of(3, smallest_outputs(3));
    assert!(valid_pre(&good));

    let mut bad_ids = good.clone();
    bad_ids.pro_tx_hash = TxHash::from_lendian([0; 32]);
    assert!(!valid_pre(&bad_ids));

    let mut bad_sizes = good.clone();
    bad_sizes.tx.outputs.pop();
    assert!(!valid_pre(&bad_sizes));

    let mut bad_script = good.clone();
    bad_script.tx.outputs[0].script_pubkey = vec![0x6a, 0x01, b'x'].into();
    assert!(!valid_pre(&bad_script));

    let mut bad_amount = good.clone();
    bad_amount.tx.outputs[0].value = 42;
    assert!(!valid_pre(&bad_amount));
}

/// Core: test/coinjoin_inouts_tests.cpp:803 isvalidstructure_postfork_unbalanced_valid
#[test]
fn isvalidstructure_postfork_unbalanced_valid() {
    let promo = dstx_of(10, vec![txout(denom::denomination_to_amount(1 << 1), p2pkh(1))]);
    assert!(possibly_post(&promo));
    // Post-V24 at the tip it is valid.
    assert!(dstx::is_valid_structure(&promo, mainnet_limits(), true).valid);
}

/// Core: test/coinjoin_inouts_tests.cpp:839 isvalidstructure_demotion_structure
#[test]
fn isvalidstructure_demotion_structure() {
    let small = denom::denomination_to_amount(1 << 2);
    let demo = dstx_of(1, (0..10).map(|i| txout(small, p2pkh(i))).collect());
    assert!(!possibly_post(&demo));

    let garbage = dstx_of(10, vec![txout(1_234_567, p2pkh(1))]);
    assert!(!possibly_post(&garbage));
}

/// Core: test/coinjoin_inouts_tests.cpp:889 input_limit_prefork and :929
/// input_limit_postfork_constants
#[test]
fn input_limits() {
    let limits = mainnet_limits();
    assert_eq!(
        limits,
        PoolLimits {
            min_participants: 3,
            max_participants: 20
        }
    );
    assert_eq!(limits.max_pool_in_outs(), 180);
    assert_eq!(limits.max_pool_in_outs_post_v24(), 200);
    assert_eq!(limits.max_pool_in_outs_post_v24() - limits.max_pool_in_outs(), 20);

    assert!(valid_pre(&dstx_of(180, smallest_outputs(180))));
    assert!(!valid_pre(&dstx_of(181, smallest_outputs(181))));
}

/// Core: test/coinjoin_inouts_tests.cpp:942 output_limit_postfork
#[test]
fn output_limit_postfork() {
    let max = mainnet_limits().max_pool_in_outs_post_v24();
    let ratio = PROMOTION_RATIO as usize;
    assert!(possibly_post(&dstx_of(max / ratio, smallest_outputs(max))));
    assert!(!possibly_post(&dstx_of(max / ratio + 1, smallest_outputs(max + 10))));
}

/// Core: test/coinjoin_inouts_tests.cpp:1042 isvalidstructure_mixed_session_postfork
#[test]
fn isvalidstructure_mixed_session_postfork() {
    let mut outputs = smallest_outputs(3);
    outputs.push(txout(denom::denomination_to_amount(1 << 3), p2pkh(0x0d)));
    let mixed = dstx_of(13, outputs);
    assert!(!valid_pre(&mixed));
    // Not given in Core's test: valid post-V24 (3 standard and one promotion).
    assert!(dstx::is_valid_structure(&mixed, mainnet_limits(), true).valid);
}

/// Core: test/coinjoin_inouts_tests.cpp:1170 isvalidstructure_boundary_input_counts
#[test]
fn isvalidstructure_boundary_input_counts() {
    assert!(valid_pre(&dstx_of(180, smallest_outputs(180))));
    assert!(!valid_pre(&dstx_of(181, smallest_outputs(181))));
    assert!(!valid_pre(&dstx_of(200, smallest_outputs(200))));
    // Post-V24 both are within the cap.
    assert!(
        dstx::is_valid_structure(&dstx_of(200, smallest_outputs(200)), mainnet_limits(), true)
            .valid
    );
    assert!(
        !dstx::is_valid_structure(&dstx_of(201, smallest_outputs(201)), mainnet_limits(), true)
            .valid
    );
}

/// Core: test/coinjoin_inouts_tests.cpp:1225 isvalidstructure_postv24_count_relations
#[test]
fn isvalidstructure_postv24_count_relations() {
    let max = mainnet_limits().max_pool_in_outs_post_v24();
    let pre_cap = mainnet_limits().max_pool_in_outs();
    let valid_post_v24 = |ins: usize, outs: usize| {
        let d = dstx_of(ins, smallest_outputs(outs));
        let flag = possibly_post(&d);
        // The flag is the post-V24 verdict, as Core's comment explains.
        assert_eq!(flag, dstx::is_valid_structure(&d, mainnet_limits(), true).valid);
        flag
    };

    assert!(valid_post_v24(100, 10));
    assert!(valid_post_v24(10, 100));
    assert!(valid_post_v24(19, 10));
    assert!(valid_post_v24(max, max / 10));
    assert!(valid_post_v24(max / 10, max));

    assert!(!valid_post_v24(39, 3));
    assert!(!valid_post_v24(3, 39));
    assert!(!valid_post_v24(4, 3));
    assert!(!valid_post_v24(199, 20));
    assert!(!valid_post_v24(max + 10, max / 10 + 1));
    assert!(!valid_post_v24(max / 10 + 1, max + 10));
    assert!(!valid_post_v24(3, 0));

    let max_balanced = max - (PROMOTION_RATIO as usize - 1);
    assert!(max_balanced > pre_cap);
    assert!(valid_post_v24(max_balanced, max_balanced));
}

// ---------------------------------------------------------------------------------------------
// coinjoin_inouts_tests.cpp: promotion, demotion, composition
// ---------------------------------------------------------------------------------------------

/// `MakeDenomInput(i)` (coinjoin_inouts_tests.cpp:564).
fn denom_inputs(n: usize) -> Vec<TxIn> {
    (0..n).map(|i| txin(outpoint(u256(1), i as u32))).collect()
}

/// Core: test/coinjoin_inouts_tests.cpp:577 validate_promotion_entry_valid, :603 _wrong_input_count,
/// :624 _wrong_output_count, :646 _non_adjacent_denoms, :1503 _edge_cases
#[test]
fn validate_promotion_entry_cases() {
    let session = 1 << 2;
    let larger = denom::denomination_to_amount(1 << 1);
    let one = vec![txout(larger, p2pkh(1))];

    assert_eq!(validate_promotion_entry(&denom_inputs(10), &one, session), Ok(()));
    // Core's order (coinjoin.cpp:662-699): counts, the larger denomination, then the script.
    let promote = |inputs: usize, outputs: &[TxOut], session| {
        validate_promotion_entry(&denom_inputs(inputs), outputs, session)
    };
    assert_eq!(promote(9, &one, session), Err(PoolMessage::ErrSizeMismatch));
    assert_eq!(promote(11, &one, session), Err(PoolMessage::ErrSizeMismatch));
    let two = vec![txout(larger, p2pkh(1)), txout(larger, p2pkh(2))];
    assert_eq!(promote(10, &two, session), Err(PoolMessage::ErrSizeMismatch));
    let halves = vec![txout(larger / 2, p2pkh(1)), txout(larger / 2, p2pkh(1))];
    assert_eq!(promote(10, &halves, session), Err(PoolMessage::ErrSizeMismatch));
    // 0.01 to 1.0 skips a step.
    assert_eq!(promote(10, &one, 1 << 3), Err(PoolMessage::ErrDenom));
    // The output at the session denomination, not the larger.
    let same = vec![txout(denom::denomination_to_amount(session), p2pkh(1))];
    assert_eq!(promote(10, &same, session), Err(PoolMessage::ErrDenom));
    // The largest denomination has nothing to promote to.
    assert_eq!(promote(10, &one, 1 << 0), Err(PoolMessage::ErrDenom));
    let bare = vec![txout(larger, vec![0x51])];
    assert_eq!(promote(10, &bare, session), Err(PoolMessage::ErrInvalidScript));
    // A wrong count wins over a wrong script.
    assert_eq!(promote(9, &bare, session), Err(PoolMessage::ErrSizeMismatch));
}

/// Core: test/coinjoin_inouts_tests.cpp:666 validate_demotion_entry_valid, :692, :713, :733
/// _largest_denom_has_no_source, :1552 _edge_cases
#[test]
fn validate_demotion_entry_cases() {
    let session = 1 << 2;
    let small = denom::denomination_to_amount(session);
    let ten: Vec<TxOut> = (0..10).map(|i| txout(small, p2pkh(i))).collect();

    assert!(denom::are_adjacent_denominations(session, 1 << 1));
    assert_eq!(validate_demotion_entry(&denom_inputs(1), &ten, session), Ok(()));
    // Core's order (coinjoin.cpp:713-749): counts, a larger source, then each output's
    // denomination and script.
    assert_eq!(
        validate_demotion_entry(&denom_inputs(2), &ten, session),
        Err(PoolMessage::ErrSizeMismatch)
    );
    assert_eq!(
        validate_demotion_entry(&denom_inputs(1), &ten[..9], session),
        Err(PoolMessage::ErrSizeMismatch)
    );
    let mut off_denom = ten.clone();
    off_denom[9].value = denom::denomination_to_amount(1 << 1);
    assert_eq!(
        validate_demotion_entry(&denom_inputs(1), &off_denom, session),
        Err(PoolMessage::ErrDenom)
    );
    let mut bare = ten.clone();
    bare[3].script_pubkey = vec![0x51].into();
    assert_eq!(
        validate_demotion_entry(&denom_inputs(1), &bare, session),
        Err(PoolMessage::ErrInvalidScript)
    );
    // Outputs are checked in order: the first bad one decides.
    off_denom[0].script_pubkey = vec![0x51].into();
    assert_eq!(
        validate_demotion_entry(&denom_inputs(1), &off_denom, session),
        Err(PoolMessage::ErrInvalidScript)
    );

    let largest = 1 << 0;
    assert_eq!(denom::larger_adjacent_denom(largest), 0);
    let tens: Vec<TxOut> =
        (0..10).map(|i| txout(denom::denomination_to_amount(largest), p2pkh(i))).collect();
    assert_eq!(
        validate_demotion_entry(&denom_inputs(1), &tens, largest),
        Err(PoolMessage::ErrDenom)
    );
    assert_eq!(denom::smaller_adjacent_denom(1 << 4), 0);
}

/// Core: test/coinjoin_inouts_tests.cpp:1129 validate_entry_session_denom_consistency
#[test]
fn validate_entry_session_denom_consistency() {
    let session = 1 << 2;
    let larger = denom::larger_adjacent_denom(session);
    assert_eq!(larger, 1 << 1);
    let promo_out = vec![txout(denom::denomination_to_amount(larger), p2pkh(1))];
    assert_eq!(validate_promotion_entry(&denom_inputs(10), &promo_out, session), Ok(()));
    assert_eq!(
        validate_promotion_entry(&denom_inputs(10), &promo_out, 1 << 0),
        Err(PoolMessage::ErrDenom)
    );

    let demo_out: Vec<TxOut> =
        (0..10).map(|i| txout(denom::denomination_to_amount(session), p2pkh(i))).collect();
    assert_eq!(validate_demotion_entry(&denom_inputs(1), &demo_out, session), Ok(()));
}

/// Core: test/coinjoin_inouts_tests.cpp:754 denomination_adjacency_checks and :779
/// get_adjacent_denomination_helpers
#[test]
fn denomination_adjacency() {
    for i in 0..4 {
        assert!(denom::are_adjacent_denominations(1 << i, 1 << (i + 1)));
        assert_eq!(denom::larger_adjacent_denom(1 << (i + 1)), 1 << i);
        assert_eq!(denom::smaller_adjacent_denom(1 << i), 1 << (i + 1));
    }
    assert!(!denom::are_adjacent_denominations(1 << 0, 1 << 2));
    assert!(!denom::are_adjacent_denominations(1 << 1, 1 << 3));
    assert!(!denom::are_adjacent_denominations(1 << 0, 1 << 4));
    assert!(!denom::are_adjacent_denominations(1 << 2, 1 << 2));
    assert!(!denom::are_adjacent_denominations(0, 1 << 1));
    assert!(!denom::are_adjacent_denominations(1 << 1, 0));
    assert!(!denom::are_adjacent_denominations(999, 1 << 1));
    for invalid in [0, 999] {
        assert_eq!(denom::larger_adjacent_denom(invalid), 0);
        assert_eq!(denom::smaller_adjacent_denom(invalid), 0);
    }
    assert_eq!(denom::larger_adjacent_denom(1 << 0), 0);
    assert_eq!(denom::smaller_adjacent_denom(1 << 4), 0);
}

/// Core: test/coinjoin_inouts_tests.cpp:982 validate_final_tx_composition
#[test]
fn validate_final_tx_composition_cases() {
    let r = PROMOTION_RATIO as usize;
    assert!(validate_final_tx_composition(5, 5, 0, 0));
    assert!(validate_final_tx_composition(3 + r, 3 + 1, 0, 1));
    assert!(validate_final_tx_composition(3 + 1, 3 + r, 1, 0));
    assert!(validate_final_tx_composition(3 + r + 1, 3 + 1 + r, 1, 1));
    assert!(validate_final_tx_composition(r, 1, 0, 1));
    assert!(!validate_final_tx_composition(r - 1, 1, 0, 1));
    assert!(!validate_final_tx_composition(1, r - 1, 1, 0));
    assert!(!validate_final_tx_composition(5, 5, 6, 0));
    assert!(!validate_final_tx_composition(5, 5, 0, 6));
    assert!(!validate_final_tx_composition(r, 2, 0, 2));
}

/// Core: test/coinjoin_inouts_tests.cpp:1017 promotion_demotion_value_preservation
#[test]
fn promotion_demotion_value_preservation() {
    for i in 0..4 {
        let larger = denom::denomination_to_amount(1 << i);
        let smaller = denom::denomination_to_amount(1 << (i + 1));
        assert!(larger > smaller);
        assert_eq!(smaller * i64::from(PROMOTION_RATIO), larger);
    }
}

/// Core: test/coinjoin_inouts_tests.cpp:1088 entry_type_detection_logic and :1604
/// entry_mix_shape_classification
#[test]
fn entry_mix_shape_classification() {
    let r = PROMOTION_RATIO as usize;
    for (ins, outs) in [(1, 1), (3, 3), (5, 5), (9, 9)] {
        assert_eq!(MixShape::of_entry(ins, outs), MixShape::Standard);
    }
    assert_eq!(MixShape::of_entry(r, 1), MixShape::Promotion);
    assert_eq!(MixShape::of_entry(1, r), MixShape::Demotion);
    for (ins, outs) in
        [(0, 0), (r, 0), (0, r), (9, 1), (1, 9), (r + 1, 1), (r, 2), (5, 3), (2, 10), (20, 2)]
    {
        assert_eq!(MixShape::of_entry(ins, outs), MixShape::Unknown, "{ins}:{outs}");
    }
}

/// Core: test/coinjoin_inouts_tests.cpp:1634 mix_side_counts_invariant and :1735
/// honest_session_always_covers_both_sides
#[test]
fn mix_side_counts_invariant() {
    use MixShape::{Demotion as D, Promotion as P, Standard as S, Unknown as U};
    let sides = |shapes: &[MixShape]| shapes.iter().copied().collect::<MixSideCounts>();

    let covered: [&[MixShape]; 7] =
        [&[S, S, S], &[S, S, P], &[S, S, D], &[P, P], &[D, D], &[P, P, D, D], &[]];
    for covered in covered {
        assert!(sides(covered).is_covered(), "{covered:?}");
    }
    let uncovered: [&[MixShape]; 6] = [&[P], &[P, D, D], &[D], &[D, P, P], &[S], &[S, P]];
    for uncovered in uncovered {
        assert!(!sides(uncovered).is_covered(), "{uncovered:?}");
    }
    assert_eq!(sides(&[U, U]), MixSideCounts::default());

    let limits = mainnet_limits();
    assert!(limits.min_participants >= 2);
    for promoters in 0..=(limits.max_participants - limits.min_participants) {
        let mut counts = MixSideCounts::default();
        (0..limits.min_participants).for_each(|_| counts.add(S));
        (0..promoters).for_each(|_| counts.add(P));
        assert!(counts.is_covered());
    }
}

/// Core: test/coinjoin_inouts_tests.cpp:1677 dsa_rebalance_flag_version_gated_serialization, the
/// flag predicates (the serialization is BS9b's).
#[test]
fn dsa_rebalance_flags() {
    let collateral =
        tx(vec![txin(outpoint(u256(1), 0))], vec![txout(denom::COLLATERAL_AMOUNT, p2pkh(1))]);
    let dsa = |flags: Option<u8>| DsAccept {
        denom: 1 << 2,
        collateral: collateral.clone(),
        flags,
    };

    let plain = dsa(None);
    let promotion = dsa(Some(FLAG_PROMOTION));
    let demotion = dsa(Some(FLAG_DEMOTION));
    let both = dsa(Some(FLAG_PROMOTION | FLAG_DEMOTION));
    assert!(!plain.is_rebalance() && !dsa(Some(0)).is_rebalance());
    assert!(promotion.is_promotion() && !promotion.is_demotion() && promotion.is_rebalance());
    assert!(demotion.is_demotion() && !demotion.is_promotion() && demotion.is_rebalance());
    assert!(plain.has_valid_flags() && promotion.has_valid_flags() && demotion.has_valid_flags());
    assert!(!both.has_valid_flags());
    assert_eq!(plain.declared_shape(), MixShape::Standard);
    assert_eq!(promotion.declared_shape(), MixShape::Promotion);
    assert_eq!(demotion.declared_shape(), MixShape::Demotion);
}

// ---------------------------------------------------------------------------------------------
// coinjoin_inouts_tests.cpp: ShouldPromoteDenoms / ShouldDemoteDenoms
// ---------------------------------------------------------------------------------------------

// Core: coinjoin/options.h:19 MIN_COINJOIN_DENOMS_GOAL, :29 DEFAULT_COINJOIN_DENOMS_GOAL
const MIN_COINJOIN_DENOMS_GOAL: i32 = 10;
const DEFAULT_COINJOIN_DENOMS_GOAL: i32 = 50;

/// `TestShouldPromote` (coinjoin_inouts_tests.cpp:1297): every coin fully mixed.
fn should_promote(smaller: i32, larger: i32, goal: i32) -> bool {
    denom::should_promote_denoms(smaller, larger, smaller, goal)
}

/// `TestShouldDemote` (coinjoin_inouts_tests.cpp:1302).
fn should_demote(larger: i32, smaller: i32, goal: i32) -> bool {
    denom::should_demote_denoms(larger, smaller, larger, goal)
}

/// Core: test/coinjoin_inouts_tests.cpp:1310 should_promote_requires_fully_mixed_coins and :1319
/// should_demote_requires_fully_mixed_coin
#[test]
fn rebalance_requires_fully_mixed_coins() {
    assert!(denom::should_promote_denoms(100, 0, PROMOTION_RATIO, 100));
    assert!(!denom::should_promote_denoms(100, 0, PROMOTION_RATIO - 1, 100));
    assert!(!denom::should_promote_denoms(100, 0, 0, 100));
    assert!(denom::should_demote_denoms(100, 0, 1, 100));
    assert!(!denom::should_demote_denoms(100, 0, 0, 100));
}

/// Core: test/coinjoin_inouts_tests.cpp:1327-1501, the count-based decision cases.
#[test]
fn rebalance_decisions() {
    let goal = 100;
    // should_promote_larger_deficit_greater, should_promote_below_half_goal_false,
    // should_promote_small_gap_false
    assert!(
        should_promote(60, 0, goal) && should_promote(100, 0, goal) && should_promote(50, 0, goal)
    );
    assert!(!should_promote(30, 0, goal) && !should_promote(49, 0, goal));
    assert!(!should_promote(90, 80, goal) && !should_promote(85, 80, goal));
    // should_demote_smaller_deficit_greater, should_demote_below_half_goal_false
    assert!(should_demote(60, 0, goal) && should_demote(99, 0, goal) && should_demote(50, 0, goal));
    assert!(!should_demote(30, 0, goal) && !should_demote(49, 0, goal));
    // promote_demote_mutually_exclusive
    for (smaller, larger, promote, demote) in [
        (100, 100, false, false),
        (90, 90, false, false),
        (100, 0, true, false),
        (0, 100, false, true),
        (0, 60, false, true),
        (60, 0, true, false),
    ] {
        assert_eq!(should_promote(smaller, larger, goal), promote, "{smaller}/{larger}");
        assert_eq!(should_demote(larger, smaller, goal), demote, "{smaller}/{larger}");
    }
    // decision_logic_example_cases_from_plan
    assert!(should_demote(99, 0, goal) && !should_promote(0, 99, goal));
    assert!(
        should_demote(60, 0, goal) && !should_demote(30, 0, goal) && !should_promote(0, 30, goal)
    );
    assert!(should_promote(60, 0, goal) && !should_promote(30, 0, goal));
    assert!(!should_promote(100, 100, goal) && !should_demote(100, 100, goal));
    assert!(!should_promote(90, 90, goal) && !should_demote(90, 90, goal));
}

/// Core: test/coinjoin_inouts_tests.cpp:1355 gap_threshold_reachable_at_every_goal
#[test]
fn gap_threshold_reachable_at_every_goal() {
    let min = MIN_COINJOIN_DENOMS_GOAL;
    assert!(should_promote(min, 0, min) && should_demote(min, 0, min));
    assert!(!should_promote(min, min - 1, min) && !should_demote(min, min - 1, min));
    assert_eq!(denom::gap_threshold(DEFAULT_COINJOIN_DENOMS_GOAL), 10);
}

/// Core: test/coinjoin_inouts_tests.cpp:1372 rebalance_never_immediately_reverses
#[test]
fn rebalance_never_immediately_reverses() {
    let ratio = PROMOTION_RATIO;
    for goal in MIN_COINJOIN_DENOMS_GOAL..=200 {
        for smaller in 0..=2 * goal {
            for larger in 0..=2 * goal {
                assert!(
                    !(should_promote(smaller, larger, goal)
                        && should_demote(larger + 1, smaller - ratio, goal)),
                    "goal {goal}: promoting {smaller}/{larger} is undone by a demotion"
                );
                assert!(
                    !(should_demote(larger, smaller, goal)
                        && should_promote(smaller + ratio, larger - 1, goal)),
                    "goal {goal}: demoting {larger}/{smaller} is undone by a promotion"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// coinjoin_inouts_tests.cpp:321 validation_uses_session_denom_snapshot
// ---------------------------------------------------------------------------------------------

/// Core: test/coinjoin_inouts_tests.cpp:321 validation_uses_session_denom_snapshot
#[test]
fn validation_uses_session_denom_snapshot() {
    let chain = Chain::default();
    let vin = vec![txin(outpoint(u256(1), 0))];
    let vout = vec![txout(SMALLEST_DENOMINATION, p2pkh(1))];

    let r = is_valid_in_outs(&chain, &vin, &vout, SMALLEST, false, InOutsKind::Entry)
        .expect_err("unknown input");
    assert_eq!(r.message, PoolMessage::ErrMissingTx);
    assert!(!r.consume_collateral);

    let other = denom::amount_to_denomination(denom::STANDARD_DENOMINATIONS[0]);
    assert_ne!(other, SMALLEST);
    let r = is_valid_in_outs(&chain, &vin, &vout, other, false, InOutsKind::Entry)
        .expect_err("wrong denom");
    assert_eq!(r.message, PoolMessage::ErrDenom);
    assert!(r.consume_collateral);
}

/// `IsValidInOuts` (coinjoin.cpp:264) on entries and final transactions: each rejection in
/// Core's order, with whether the collateral is charged.
#[test]
fn is_valid_in_outs_rejections() {
    let mut chain = Chain::default();
    let vin = chain.smallest_coins(2);
    let vout = vec![txout(SMALLEST_DENOMINATION, key(1)), txout(SMALLEST_DENOMINATION, key(2))];
    let check = |chain: &Chain, vin: &[TxIn], vout: &[TxOut], rebalance: bool| {
        is_valid_in_outs(chain, vin, vout, SMALLEST, rebalance, InOutsKind::Entry)
            .map_err(|r| (r.message, r.consume_collateral))
    };
    let counts = check(&chain, &vin, &vout, false).expect("valid entry");
    assert_eq!((counts.inputs, counts.outputs), (2, 2));

    // Size mismatch: charged.
    assert_eq!(check(&chain, &vin, &vout[..1], false), Err((PoolMessage::ErrSizeMismatch, true)));
    // A non-P2PKH output: charged.
    let mut bad = vout.clone();
    bad[0].script_pubkey = vec![0x51].into();
    assert_eq!(check(&chain, &vin, &bad, false), Err((PoolMessage::ErrInvalidScript, true)));
    // A script twice: charged.
    let dup = vec![txout(SMALLEST_DENOMINATION, key(1)), txout(SMALLEST_DENOMINATION, key(1))];
    assert_eq!(check(&chain, &vin, &dup, false), Err((PoolMessage::ErrAlreadyHave, true)));
    // A null input: charged.
    let null = vec![vin[0].clone(), txin(outpoint(TxHash::from_lendian([0; 32]), u32::MAX))];
    assert_eq!(check(&chain, &null, &vout, false), Err((PoolMessage::ErrInvalidInput, true)));
    // An unlocked mempool coin is missing (free); locked, it is usable.
    let op = vin[1].prevout;
    let coin = chain.coins[&op].out.clone();
    chain.add_coin(op, coin, true);
    assert_eq!(check(&chain, &vin, &vout, false), Err((PoolMessage::ErrMissingTx, false)));
    chain.locked.insert(op.hash);
    assert!(check(&chain, &vin, &vout, false).is_ok());
    // An input at another denomination: charged.
    let big = chain.fund(SMALLEST_DENOMINATION * 10);
    let other_in = vec![vin[0].clone(), txin(big)];
    let r =
        is_valid_in_outs(&chain, &other_in, &vout, SMALLEST, false, InOutsKind::Entry).unwrap_err();
    assert_eq!((r.message, r.consume_collateral), (PoolMessage::ErrDenom, true));
}

/// At session denomination 0, which no session has, every non-denominated amount passes the
/// denomination check, so the fee sums see unchecked values: they wrap rather than panic, and
/// value-preserving non-denominated entries are accepted as in Core.
#[test]
fn is_valid_in_outs_at_denomination_zero_does_not_panic() {
    let mut chain = Chain::default();
    let funded = chain.fund(30_000);
    let vin = vec![txin(funded)];
    for value in [i64::MIN, i64::MAX, -1] {
        let vout = vec![txout(value, key(1))];
        let r = is_valid_in_outs(&chain, &vin, &vout, 0, false, InOutsKind::Entry).unwrap_err();
        assert_eq!(r.message, PoolMessage::ErrFees, "{value}");
    }
    let vout = vec![txout(30_000, key(1))];
    let counts = is_valid_in_outs(&chain, &vin, &vout, 0, false, InOutsKind::Entry)
        .expect("a value-preserving entry at denomination 0");
    assert_eq!((counts.inputs, counts.outputs), (1, 1));
}

/// A promotion entry checks its inputs' coins at the session denomination and its output at the
/// next larger one; without rebalance shapes it is a size mismatch.
#[test]
fn is_valid_in_outs_rebalance_entries() {
    let mut chain = Chain::default();
    let larger = denom::denomination_to_amount(denom::larger_adjacent_denom(SMALLEST));
    let vin = chain.smallest_coins(10);
    let vout = vec![txout(larger, key(1))];
    assert!(is_valid_in_outs(&chain, &vin, &vout, SMALLEST, true, InOutsKind::Entry).is_ok());
    let r = is_valid_in_outs(&chain, &vin, &vout, SMALLEST, false, InOutsKind::Entry).unwrap_err();
    assert_eq!((r.message, r.consume_collateral), (PoolMessage::ErrSizeMismatch, true));

    // A demotion: one larger coin to ten session outputs.
    let big = vec![txin(chain.fund(larger))];
    let ten: Vec<TxOut> = (10..20).map(|i| txout(SMALLEST_DENOMINATION, key(i))).collect();
    assert!(is_valid_in_outs(&chain, &big, &ten, SMALLEST, true, InOutsKind::Entry).is_ok());

    // The final transaction of one promotion and one demotion: composable, and the counts are
    // of the session-denomination coins.
    let mut inputs = vin.clone();
    inputs.extend(big);
    let mut outputs = vout.clone();
    outputs.extend(ten);
    let counts = is_valid_in_outs(&chain, &inputs, &outputs, SMALLEST, true, InOutsKind::FinalTx)
        .expect("composable");
    assert_eq!((counts.inputs, counts.outputs), (10, 10));
    // Drop one promotion input: no longer composable, and not charged.
    let r = is_valid_in_outs(&chain, &inputs[1..], &outputs, SMALLEST, true, InOutsKind::FinalTx)
        .unwrap_err();
    assert_eq!(r.message, PoolMessage::ErrFees);
}

// ---------------------------------------------------------------------------------------------
// coinjoin collateral (wallet/test/coinjoin_tests.cpp:64 and coinjoin.cpp:458)
// ---------------------------------------------------------------------------------------------

/// Core: wallet/test/coinjoin_tests.cpp:64 coinjoin_collateral_tests
#[test]
fn coinjoin_collateral_amounts() {
    for good in [10_000, 12_345, 32_123, 19_000] {
        assert!(denom::is_collateral_amount(good), "{good}");
    }
    for bad in [9_999, 40_001, 100_000, 100_001] {
        assert!(!denom::is_collateral_amount(bad), "{bad}");
    }
}

/// `IsCollateralValid` (coinjoin.cpp:458), each rule in order.
#[test]
fn is_collateral_valid_rules() {
    use dash_coinjoin::collateral::{MAX_RAW_TX_FEE, is_collateral_valid};
    let mut chain = Chain::default();
    let good = chain.collateral();
    assert!(is_collateral_valid(&chain, &good));

    let mut no_outputs = good.clone();
    no_outputs.outputs.clear();
    assert!(!is_collateral_valid(&chain, &no_outputs));
    let mut locked = good.clone();
    locked.lock_time = 1;
    assert!(!is_collateral_valid(&chain, &locked));
    // OP_RETURN outputs are allowed, other scripts are not.
    let mut op_return = good.clone();
    op_return.outputs[0].script_pubkey = vec![0x6a].into();
    assert!(is_collateral_valid(&chain, &op_return));
    let mut p2sh = good.clone();
    p2sh.outputs[0].script_pubkey =
        vec![0xa9, 0x14].into_iter().chain([0; 20]).chain([0x87]).collect::<Vec<u8>>().into();
    assert!(!is_collateral_valid(&chain, &p2sh));
    // Too little fee.
    let mut cheap = good.clone();
    cheap.outputs[0].value += 1;
    assert!(!is_collateral_valid(&chain, &cheap));
    // An unknown input.
    let mut unknown = good.clone();
    unknown.inputs[0].prevout = outpoint(u256(0x77), 0);
    assert!(!is_collateral_valid(&chain, &unknown));
    // The mempool's verdict and its fee cap.
    chain.accept_fee = None;
    assert!(!is_collateral_valid(&chain, &good));
    chain.accept_fee = Some(MAX_RAW_TX_FEE);
    assert!(is_collateral_valid(&chain, &good));
    chain.accept_fee = Some(MAX_RAW_TX_FEE + 1);
    assert!(!is_collateral_valid(&chain, &good));
}

/// `MoneyRange` on every amount and running sum, as ATMP applies it (outputs:
/// consensus/tx_check.cpp:38-45; inputs: consensus/tx_verify.cpp:188-191). The mock test-accept
/// ignores amounts, so only the crate's own checks can refuse these. Every refused case but two
/// would otherwise pass the fee check (by wrapping or through an out-of-range amount). The two
/// exceptions also fall short of the fee and stay as boundary verdicts: the MAX_MONEY + 1 output
/// from a MAX_MONEY input (fee -1), and the output total above MAX_MONEY.
#[test]
fn is_collateral_valid_money_range() {
    use dash_coinjoin::collateral::is_collateral_valid;
    use dash_coinjoin::denom::{COLLATERAL_AMOUNT, MAX_MONEY};
    let pay = |v: i64| txout(v, p2pkh(9));
    let mut chain = Chain::default();
    chain.accept_fee = Some(COLLATERAL_AMOUNT);

    // In range at the top: a MAX_MONEY coin, the fee left over.
    let max_in = txin(chain.fund(MAX_MONEY));
    assert!(is_collateral_valid(
        &chain,
        &tx(vec![max_in.clone()], vec![pay(MAX_MONEY - COLLATERAL_AMOUNT)])
    ));

    // Outputs: a negative one, one above MAX_MONEY, and a pair whose i64 sum wraps negative.
    let input = txin(chain.fund(2 * COLLATERAL_AMOUNT));
    assert!(!is_collateral_valid(
        &chain,
        &tx(vec![input.clone()], vec![pay(COLLATERAL_AMOUNT), pay(-1)])
    ));
    assert!(!is_collateral_valid(&chain, &tx(vec![max_in.clone()], vec![pay(MAX_MONEY + 1)])));
    assert!(!is_collateral_valid(
        &chain,
        &tx(vec![input.clone()], vec![pay(i64::MAX), pay(i64::MAX)])
    ));
    // Each output in range, their total above MAX_MONEY.
    assert!(!is_collateral_valid(&chain, &tx(vec![max_in.clone()], vec![pay(MAX_MONEY), pay(1)])));

    // Inputs: a coin above MAX_MONEY, a negative coin, and two coins whose total is above it.
    let huge = txin(chain.fund(MAX_MONEY + 1));
    assert!(!is_collateral_valid(&chain, &tx(vec![huge], vec![pay(COLLATERAL_AMOUNT)])));
    let negative = txin(chain.fund(-1));
    let topup = txin(chain.fund(3 * COLLATERAL_AMOUNT));
    assert!(!is_collateral_valid(&chain, &tx(vec![negative, topup], vec![pay(COLLATERAL_AMOUNT)])));
    let second_max = txin(chain.fund(MAX_MONEY));
    assert!(!is_collateral_valid(
        &chain,
        &tx(vec![max_in, second_max], vec![pay(COLLATERAL_AMOUNT)])
    ));
}

// ---------------------------------------------------------------------------------------------
// wallet/test/coinjoin_tests.cpp: rounds
// ---------------------------------------------------------------------------------------------

/// A wallet that owns every output of its transactions (Core's tests pay `scriptOurs`).
#[derive(Default)]
struct Wallet {
    txs: BTreeMap<TxHash, Transaction>,
}

impl Wallet {
    fn add(&mut self, t: Transaction) -> TxHash {
        let txid = dstx::txid(&t);
        self.txs.insert(txid, t);
        txid
    }

    fn prev_value(&self, txin: &TxIn) -> Option<i64> {
        let prev = self.txs.get(&txin.prevout.hash)?;
        prev.outputs.get(txin.prevout.index as usize).map(|o| o.value)
    }
}

impl RoundsWallet for Wallet {
    fn wallet_tx(&self, txid: &TxHash) -> Option<&Transaction> {
        self.txs.get(txid)
    }

    fn input_is_mine(&self, txin: &TxIn) -> bool {
        self.prev_value(txin).is_some()
    }

    fn debit_equals_credit(&self, txid: &TxHash) -> bool {
        let t = &self.txs[txid];
        let debit: i64 = t.inputs.iter().filter_map(|i| self.prev_value(i)).sum();
        let credit: i64 = t.outputs.iter().map(|o| o.value).sum();
        debit == credit
    }
}

/// The 0.100001 DASH denomination of Core's rounds tests.
const DENOM_AMOUNT: i64 = 10_000_100;

/// `GetTallyItem(amounts)`: a funding transaction (from outside the wallet, paying a fee) with
/// these outputs.
fn fund(wallet: &mut Wallet, amounts: &[i64]) -> Vec<OutPoint> {
    let outputs = amounts.iter().map(|&a| txout(a, p2pkh(0x99))).collect();
    let txid = wallet.add(tx(vec![txin(outpoint(u256(0xfe), 0))], outputs));
    (0..amounts.len() as u32).map(|i| outpoint(txid, i)).collect()
}

/// Core: wallet/test/coinjoin_tests.cpp:497 coinjoin_rebalance_rounds_reset_tests
#[test]
fn coinjoin_rebalance_rounds_reset() {
    let larger_amount = DENOM_AMOUNT * i64::from(PROMOTION_RATIO);
    assert!(
        denom::is_denominated_amount(DENOM_AMOUNT) && denom::is_denominated_amount(larger_amount)
    );
    let opts = RoundsOptions::default();
    let mut wallet = Wallet::default();
    let mut cache = RoundsCache::default();
    let coins = fund(&mut wallet, &vec![DENOM_AMOUNT; PROMOTION_RATIO as usize + 1]);
    assert_eq!(cache.real_rounds(&wallet, opts, &coins[0]), 0);

    let mix = wallet.add(tx(vec![txin(coins[0])], vec![txout(DENOM_AMOUNT, p2pkh(1))]));
    cache.clear();
    assert_eq!(cache.real_rounds(&wallet, opts, &outpoint(mix, 0)), 1);

    let promo = wallet.add(tx(
        coins[1..].iter().map(|&c| txin(c)).collect(),
        vec![txout(larger_amount, p2pkh(1))],
    ));
    cache.clear();
    assert_eq!(cache.real_rounds(&wallet, opts, &outpoint(promo, 0)), 0);

    let remix =
        wallet.add(tx(vec![txin(outpoint(promo, 0))], vec![txout(larger_amount, p2pkh(1))]));
    cache.clear();
    assert_eq!(cache.real_rounds(&wallet, opts, &outpoint(remix, 0)), 1);

    let demo = wallet.add(tx(
        vec![txin(outpoint(remix, 0))],
        (0..PROMOTION_RATIO).map(|_| txout(DENOM_AMOUNT, p2pkh(1))).collect(),
    ));
    cache.clear();
    for n in 0..PROMOTION_RATIO as u32 {
        assert_eq!(cache.real_rounds(&wallet, opts, &outpoint(demo, n)), 0);
    }
}

/// Core: wallet/test/coinjoin_tests.cpp:555 coinjoin_rounds_cache_unknown_tx_tests
#[test]
fn coinjoin_rounds_cache_unknown_tx() {
    let opts = RoundsOptions::default();
    let mut wallet = Wallet::default();
    let mut cache = RoundsCache::default();
    let coins = fund(&mut wallet, &[DENOM_AMOUNT]);
    let mix = tx(vec![txin(coins[0])], vec![txout(DENOM_AMOUNT, p2pkh(1))]);
    let mixed = outpoint(dstx::txid(&mix), 0);

    assert_eq!(cache.real_rounds(&wallet, opts, &mixed), rounds::sentinel::UNKNOWN_TX);
    wallet.add(mix);
    // The miss was not memoised.
    assert_eq!(cache.real_rounds(&wallet, opts, &mixed), 1);
}

/// `GetRealOutpointCoinJoinRounds` sentinels and the cap, and `IsFullyMixed`.
#[test]
fn rounds_sentinels_cap_and_fully_mixed() {
    let opts = RoundsOptions::default();
    let mut wallet = Wallet::default();
    let mut cache = RoundsCache::default();

    // Out of range, a collateral amount, not a denomination.
    let coins = fund(&mut wallet, &[DENOM_AMOUNT, denom::COLLATERAL_AMOUNT, 12_345_678]);
    assert_eq!(
        cache.real_rounds(&wallet, opts, &outpoint(coins[0].hash, 9)),
        rounds::sentinel::OUT_OF_RANGE
    );
    assert_eq!(cache.real_rounds(&wallet, opts, &coins[1]), rounds::sentinel::COLLATERAL);
    assert_eq!(cache.real_rounds(&wallet, opts, &coins[2]), rounds::sentinel::NOT_DENOMINATED);

    // A chain of 25 mixes: counted to MAX_COINJOIN_ROUNDS + random rounds.
    let mut coin = coins[0];
    let mut chain = Vec::new();
    for i in 0..25u8 {
        let txid = wallet.add(tx(vec![txin(coin)], vec![txout(DENOM_AMOUNT, p2pkh(i))]));
        coin = outpoint(txid, 0);
        chain.push(coin);
    }
    let max = rounds::MAX_COINJOIN_ROUNDS + opts.random_rounds;
    assert_eq!(cache.real_rounds(&wallet, opts, &chain[0]), 1);
    assert_eq!(cache.real_rounds(&wallet, opts, &chain[3]), 4);
    cache.clear();
    assert_eq!(cache.real_rounds(&wallet, opts, &chain[24]), max);
    assert_eq!(cache.capped_rounds(&wallet, opts, &chain[24]), opts.rounds);

    // Below the rounds never fully mixed; at rounds + random rounds always.
    cache.clear();
    let salt = [7u8; 32];
    assert!(!cache.is_fully_mixed(&wallet, opts, &salt, &chain[2]));
    assert!(cache.is_fully_mixed(&wallet, opts, &salt, &chain[6]));
    // In between, the coin flip decides.
    for c in &chain[3..6] {
        assert_eq!(cache.is_fully_mixed(&wallet, opts, &salt, c), rounds::stops_mixing(c, &salt));
    }
}

/// The pool's entry cap (`COINJOIN_ENTRY_MAX_SIZE`) is BS9b's.
#[test]
fn entry_max_size() {
    assert_eq!(ENTRY_MAX_SIZE, 9);
}
