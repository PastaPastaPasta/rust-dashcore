//! The `isdlock` handler's checks, scores and queueing against Core v24.0.0
//! `instantsend/net_instantsend.cpp:33-37, :64-102, :238-289`.

use std::sync::atomic::Ordering;

use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::locks::{ISLOCK_MAX_INPUTS, isdlock_hash};

use super::*;
use crate::is::IsLockHash;
use crate::is::s3_fixtures::{BASIC, Event, FakeView, Rig, lock};

fn payload(lock: &LockedTx) -> Vec<u8> {
    let mut bytes = Vec::new();
    lock.encode(&mut bytes);
    bytes
}

fn with_inputs(l: &LockedTx, inputs: Vec<OutPoint>) -> LockedTx {
    LockedTx::new(l.version(), inputs, l.txid(), l.cycle_hash(), l.sig())
}

fn send(rig: &Rig, view: &FakeView, from: NodeId, l: &LockedTx) {
    let res = rig.net.process_message(view, from, &payload(l), BlsCtx::LEGACY);
    assert_eq!(res.expect("no fault"), IsdLockIntake::Handled);
}

fn queued(rig: &Rig) -> Vec<(NodeId, IsLockHash, bool)> {
    rig.net
        .manager()
        .fetch_pending_locks()
        .pending_is
        .into_iter()
        .map(|e| (e.node_id, e.islock_hash, e.legacy))
        .collect()
}

/// A lock that passes is queued with its read scheme, after its request from the sender is
/// erased and before every peer's request is forgotten; a second copy is already had.
// Core: net_instantsend.cpp:259-261, :281-288
#[test]
fn a_valid_lock_is_queued_once() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let l = lock(1, 1);
    let hash = isdlock_hash(&l);
    send(&rig, &view, 4, &l);
    send(&rig, &view, 5, &l);
    assert_eq!(rig.events(), [Event::Erase(4, hash), Event::Forget(hash), Event::Erase(5, hash)]);
    assert_eq!(queued(&rig), [(4, hash, true)]);
}

/// More than `MAX_INPUTS` inputs: +100 before the lock is hashed or its request erased.
// Core: net_instantsend.cpp:34, :251-257
#[test]
fn an_oversized_lock_scores_100_before_anything_else() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let base = lock(1, 1);
    let inputs = (0..=ISLOCK_MAX_INPUTS as u32).map(|i| OutPoint::new(base.txid(), i)).collect();
    send(&rig, &view, 4, &with_inputs(&base, inputs));
    assert_eq!(rig.events(), [Event::Misbehaving(4, INVALID_ISLOCK_MISBEHAVIOR_SCORE)]);
    assert!(queued(&rig).is_empty());
}

/// Not `TriviallyValid` (no inputs, a repeated input, a null txid): +100 after the erase.
// Core: net_instantsend.cpp:64-72, :263-265; instantsend/lock.cpp:30-45
#[test]
fn a_lock_not_trivially_valid_scores_100() {
    let view = FakeView::with_quorums(1);
    let base = lock(1, 1);
    let input = base.inputs()[0];
    let null_txid = LockedTx::new(
        base.version(),
        base.inputs().clone(),
        TxHash::from([0; 32]),
        base.cycle_hash(),
        base.sig(),
    );
    for bad in [with_inputs(&base, vec![]), with_inputs(&base, vec![input, input]), null_txid] {
        let rig = Rig::new(false);
        send(&rig, &view, 4, &bad);
        let hash = isdlock_hash(&bad);
        assert_eq!(
            rig.events(),
            [Event::Erase(4, hash), Event::Misbehaving(4, INVALID_ISLOCK_MISBEHAVIOR_SCORE)]
        );
        assert!(queued(&rig).is_empty());
    }
}

/// A cycle hash naming no known block (or the null hash): +1.
// Core: net_instantsend.cpp:35, :40-45, :267-272
#[test]
fn an_unknown_cycle_scores_1() {
    let view = FakeView::with_quorums(1);
    for cycle in [[9u8; 32], [0; 32]] {
        let rig = Rig::new(false);
        let base = lock(1, 1);
        let l = LockedTx::new(
            base.version(),
            base.inputs().clone(),
            base.txid(),
            cycle.into(),
            base.sig(),
        );
        send(&rig, &view, 4, &l);
        assert_eq!(
            rig.events(),
            [
                Event::Erase(4, isdlock_hash(&l)),
                Event::Misbehaving(4, UNKNOWN_CYCLE_HASH_MISBEHAVIOR_SCORE)
            ]
        );
        assert!(queued(&rig).is_empty());
    }
}

/// A cycle block off the `dkgInterval` boundary: +100.
// Core: net_instantsend.cpp:90-102, :277-279
#[test]
fn a_cycle_off_the_interval_scores_100() {
    let rig = Rig::new(false);
    let mut view = FakeView::with_quorums(1);
    view.heights.insert(crate::is::s3_fixtures::quorum_hash(1), 25);
    let l = lock(1, 1);
    send(&rig, &view, 4, &l);
    assert_eq!(
        rig.events(),
        [
            Event::Erase(4, isdlock_hash(&l)),
            Event::Misbehaving(4, INVALID_ISLOCK_MISBEHAVIOR_SCORE)
        ]
    );
}

/// With InstantSend off, nothing: the payload is not even read.
// Core: net_instantsend.cpp:244
#[test]
fn nothing_happens_while_instantsend_is_disabled() {
    let rig = Rig::new(false);
    rig.spork2.store(false, Ordering::SeqCst);
    let view = FakeView::with_quorums(1);
    let res = rig.net.process_message(&view, 4, b"\x01", BASIC);
    assert_eq!(res.expect("no fault"), IsdLockIntake::Handled);
    send(&rig, &view, 4, &lock(1, 1));
    assert!(rig.events().is_empty());
    rig.spork2.store(true, Ordering::SeqCst);
    assert!(queued(&rig).is_empty());
}

/// A 2-byte payload is not an `isdlock`: Core's read throws, and `ProcessMessages` only logs
/// it. The outcome is not an error, and nothing is scored, erased or queued.
// Core: net_instantsend.cpp:246-247, net_processing.cpp:5958-5962
#[test]
fn a_two_byte_isdlock_is_undecodable_and_changes_nothing() {
    let rig = Rig::new(false);
    let view = FakeView::with_quorums(1);
    let res = rig.net.process_message(&view, 4, b"\x01\x05", BASIC);
    assert!(matches!(res, Ok(IsdLockIntake::Undecodable(_))), "{res:?}");
    assert!(rig.events().is_empty());
    assert!(queued(&rig).is_empty());
}
