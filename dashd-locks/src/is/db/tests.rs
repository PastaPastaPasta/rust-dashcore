use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread;

use dash_consensus::Tx;
use dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_evo::locks::isdlock_hash;
use dash_kv::faulty::{
    Dfs, EventSel, Fault, FaultKind, FaultOutcome, OpKind, StoreSel, World, WorldConfig,
};
use dash_kv::{Commit, ErrorKind, KvBatch, KvEngine, KvRead, MemoryEngine, StoreError};
use dash_types::codec::BaseCodec;
use dashd_store::{RocksConfig, RocksEngine};

use super::{IS, IsDb, SCHEMA, keys, lru::Lru};
use crate::is::seams::{IsOverlay, IsStore};
use crate::is::{LockedTx, lock_inputs};

pub(super) fn txid(tag: u32) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    TxHash::from(bytes)
}

pub(super) fn input(tag: u32, index: u32) -> OutPoint {
    OutPoint::new(txid(tag), index)
}

pub(super) fn lock(tag: u32, inputs: Vec<OutPoint>) -> LockedTx {
    LockedTx::new(1, inputs, txid(tag), Default::default(), [0; 96].into())
}

pub(super) fn transaction(tag: u32) -> Tx {
    Tx::new(Transaction::new(
        1,
        TxType::new(0),
        vec![TxIn::new(input(123, 0), Default::default(), u32::MAX)],
        vec![],
        tag,
        vec![],
    ))
}

fn asset_unlock(index: u64, lock_time: u32) -> Tx {
    let mut payload = vec![0; ASSET_UNLOCK_PAYLOAD_SIZE];
    payload[1..9].copy_from_slice(&index.to_le_bytes());
    Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], lock_time, payload))
}

pub(super) fn rows<E: KvEngine>(engine: &E) -> BTreeMap<Vec<u8>, Vec<u8>> {
    engine.iter_prefix(IS, &[]).map(Result::unwrap).collect()
}

fn memory() -> (Arc<MemoryEngine>, IsDb<MemoryEngine>) {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let db = IsDb::open(engine.clone()).unwrap();
    (engine, db)
}

pub(super) fn spending(input: OutPoint, tag: u32) -> Tx {
    Tx::new(Transaction::new(
        1,
        TxType::new(0),
        vec![TxIn::new(input, Default::default(), u32::MAX)],
        vec![],
        tag,
        vec![],
    ))
}

#[test]
fn staged_removal_reads_overlay_and_commits_core_batch() {
    let (engine, db) = memory();
    let x = input(99, 0);
    let y = input(99, 1);
    let a = lock(1, vec![x, y]);
    let b = lock(2, vec![x]);
    let (ha, hb) = (isdlock_hash(&a), isdlock_hash(&b));
    db.write_new(&ha, &a).unwrap();
    db.write_new(&hb, &b).unwrap();
    let tx_y = spending(y, 40);
    let tx_x = spending(x, 41);
    let mut overlay = IsOverlay::default();
    assert_eq!(db.conflicting_lock(&tx_y).unwrap(), Some((ha, a.clone())));
    let root = db.by_hash(&ha, false).unwrap().unwrap();
    let (removal, stage) = db.stage_remove_chained(&mut overlay, &ha, &root, 10).unwrap();
    assert_eq!(removal.hashes, vec![ha]);
    assert_eq!(db.conflicting_lock_in(&overlay, &tx_x).unwrap(), None);
    assert!(engine.get(IS, &keys::hash(keys::LOCK, &ha)).unwrap().is_some());
    db.commit_staged(&mut overlay, stage).unwrap();
    assert_eq!(db.by_input(&x).unwrap(), None);
    assert_eq!(db.by_txid(&a.txid()).unwrap(), None);
    assert_eq!(db.by_txid(&b.txid()).unwrap(), Some(b));
    assert!(db.known(&ha).unwrap());
}

#[test]
fn staged_root_is_re_read_after_the_walk_and_can_truncate_the_cache() {
    let (_engine, db) = memory();
    let root = lock(1, vec![input(82, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    for i in 0..30_000u32 {
        let mut miss = [0xee; 32];
        miss[..4].copy_from_slice(&i.to_le_bytes());
        assert_eq!(db.by_hash(&miss, false).unwrap(), None);
    }
    let before = db.cache_snapshot().locks;
    assert_eq!(before.len(), 20_000);
    assert!(before.iter().all(|(cached, _)| *cached != hash));
    let mut overlay = IsOverlay::default();
    let (removal, stage) = db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    assert!(removal.root_present);
    // Core's root re-read (db.cpp:379) inserts it, truncating to the 10,000 newest, and the
    // removal then erases it.
    let after = db.cache_snapshot().locks;
    assert_eq!(after.len(), 9_999);
    assert_eq!(after[..], before[before.len() - 9_999..]);
    db.commit_staged(&mut overlay, stage).unwrap();
}

#[test]
fn later_stage_reads_deletions_from_the_first_overlay() {
    let (_engine, db) = memory();
    let first = lock(1, vec![input(70, 0)]);
    let second = lock(1, vec![input(70, 1)]);
    let child = lock(2, vec![OutPoint::new(txid(1), 0)]);
    let (h1, h2, hc) = (isdlock_hash(&first), isdlock_hash(&second), isdlock_hash(&child));
    db.write_new(&h1, &first).unwrap();
    db.write_new(&h2, &second).unwrap();
    db.write_new(&hc, &child).unwrap();
    let mut overlay = IsOverlay::default();
    let (_, first_stage) = db.stage_remove_chained(&mut overlay, &h1, &first, 10).unwrap();
    let (second_removal, second_stage) =
        db.stage_remove_chained(&mut overlay, &h2, &second, 10).unwrap();
    assert_eq!(second_removal.hashes, vec![h2]);
    db.commit_staged(&mut overlay, first_stage).unwrap();
    db.commit_staged(&mut overlay, second_stage).unwrap();
    assert_eq!(db.by_hash(&hc, false).unwrap(), None);
    assert_eq!(db.count().unwrap(), 0);
}

#[test]
fn staged_commit_requires_oldest_and_write_failures_latch() {
    let (engine, db) = memory();
    let first = lock(1, vec![input(71, 0)]);
    let second = lock(2, vec![input(71, 1)]);
    let (h1, h2) = (isdlock_hash(&first), isdlock_hash(&second));
    db.write_new(&h1, &first).unwrap();
    db.write_new(&h2, &second).unwrap();
    let mut overlay = IsOverlay::default();
    let (_, s1) = db.stage_remove_chained(&mut overlay, &h1, &first, 10).unwrap();
    let (_, s2) = db.stage_remove_chained(&mut overlay, &h2, &second, 10).unwrap();
    assert!(db.commit_staged(&mut overlay, s2).is_err());
    db.commit_staged(&mut overlay, s1).unwrap();
    db.commit_staged(&mut overlay, s2).unwrap();

    let third = lock(3, vec![input(71, 2)]);
    let h3 = isdlock_hash(&third);
    db.write_new(&h3, &third).unwrap();
    let mut failing = IsOverlay::default();
    let (_, stage) = db.stage_remove_chained(&mut failing, &h3, &third, 10).unwrap();
    engine.inject_failure(ErrorKind::NoSpace);
    assert!(db.commit_staged(&mut failing, stage).is_err());
    assert!(db.by_hash(&h3, false).is_err());
}

#[test]
fn staged_discard_latches_and_preserves_rows_until_reopen() {
    let (engine, db) = memory();
    let root = lock(1, vec![input(72, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    let before = rows(&*engine);
    let mut overlay = IsOverlay::default();
    db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    db.discard_staged(&mut overlay);
    assert!(db.by_hash(&hash, false).is_err());
    assert!(db.write_mined(&hash, 10).is_err());
    assert_eq!(rows(&*engine), before);
    let reopened = IsDb::open(engine).unwrap();
    assert_eq!(reopened.by_hash(&hash, false).unwrap(), Some(root));
}

#[test]
fn owner_read_refuses_after_a_discard_and_without_the_gate() {
    let (_engine, db) = memory();
    let x = input(81, 0);
    let root = lock(1, vec![x]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    let tx = spending(x, 81);
    assert_eq!(
        db.conflicting_lock_in(&IsOverlay::default(), &tx).unwrap(),
        Some((hash, root.clone()))
    );
    let mut overlay = IsOverlay::default();
    db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    assert_eq!(db.conflicting_lock_in(&overlay, &tx).unwrap(), None);
    db.discard_staged(&mut overlay);
    assert!(db.conflicting_lock_in(&overlay, &tx).is_err());
    assert!(db.conflicting_lock_in(&IsOverlay::default(), &tx).is_err());

    let (_engine, db) = memory();
    db.write_new(&hash, &root).unwrap();
    let mut overlay = IsOverlay::default();
    db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    assert!(db.conflicting_lock_in(&IsOverlay::default(), &tx).is_err());
    assert!(db.conflicting_lock_in(&overlay, &tx).is_err());
}

#[test]
fn the_overlay_holds_the_gate_only_while_it_has_stages() {
    let (_engine, db) = memory();
    let root = lock(1, vec![input(74, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    let mut overlay = IsOverlay::default();
    assert!(overlay.gate_held_only_with_stages());
    let (_, stage) = db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    assert!(overlay.gate_owner.is_some() && overlay.gate_held_only_with_stages());
    db.commit_staged(&mut overlay, stage).unwrap();
    assert!(overlay.gate_owner.is_none() && overlay.gate_held_only_with_stages());
    let stale = IsOverlay {
        gate_owner: Some(thread::current().id()),
        ..IsOverlay::default()
    };
    assert!(!stale.gate_held_only_with_stages());
}

#[test]
fn staging_thread_non_overlay_operation_is_fatal() {
    for write in [false, true] {
        let (engine, db) = memory();
        let root = lock(1, vec![input(73, 0)]);
        let hash = isdlock_hash(&root);
        db.write_new(&hash, &root).unwrap();
        let before = rows(&*engine);
        let mut overlay = IsOverlay::default();
        db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
        if write {
            let other = lock(2, vec![input(73, 1)]);
            assert!(db.write_new(&isdlock_hash(&other), &other).is_err());
        } else {
            assert!(db.by_hash(&hash, true).is_err());
        }
        assert_eq!(db.gate_waiters(), 0);
        let stage = overlay.stages[0].id;
        assert!(db.commit_staged(&mut overlay, stage).is_err());
        db.discard_staged(&mut overlay);
        assert!(overlay.gate_held_only_with_stages(), "the latch ended the overlay's gate");
        assert!(db.by_hash(&hash, false).is_err());
        assert_eq!(rows(&*engine), before);
    }
}

/// Spins until `done` holds; a gate regression fails here instead of hanging the suite.
pub(super) fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        thread::yield_now();
    }
}

/// `handle`'s result once its thread ends; a thread that never ends fails the test instead of
/// hanging it.
pub(super) fn join_within<T>(what: &str, handle: thread::JoinHandle<T>) -> thread::Result<T> {
    wait_until(what, || handle.is_finished());
    handle.join()
}

#[test]
fn concurrent_write_waits_for_commit_and_lands_after_it() {
    let (_engine, db) = memory();
    let db = Arc::new(db);
    let root = lock(1, vec![input(74, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    let mut overlay = IsOverlay::default();
    let (_, stage) = db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    let extra = lock(2, vec![input(74, 1)]);
    let extra_hash = isdlock_hash(&extra);
    let writer_db = Arc::clone(&db);
    let writer_extra = extra.clone();
    let writer = thread::spawn(move || writer_db.write_new(&extra_hash, &writer_extra));
    wait_until("the writer to park on the gate", || db.gate_waiters() == 1 || writer.is_finished());
    assert!(!writer.is_finished());
    db.commit_staged(&mut overlay, stage).unwrap();
    join_within("the writer", writer).unwrap().unwrap();
    assert_eq!(db.by_hash(&extra_hash, false).unwrap(), Some(extra));
}

#[test]
fn reader_queued_before_a_stage_leaves_no_stale_cache_entry() {
    use std::sync::mpsc;
    use std::time::Duration;

    // Only the stager-first order (the reader parks) exposes the race, so repeat until it ran.
    let (mut reps, mut stager_first) = (0, 0);
    while reps < 20 || (stager_first == 0 && reps < 200) {
        reps += 1;
        let (_engine, db) = memory();
        let db = Arc::new(db);
        let root = lock(1, vec![input(79, 0)]);
        let hash = isdlock_hash(&root);
        db.write_new(&hash, &root).unwrap();
        let held = db.state.lock();
        let (staged_tx, staged_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel::<()>();
        let stager_db = Arc::clone(&db);
        let stager_root = root.clone();
        let stager = thread::spawn(move || {
            let mut overlay = IsOverlay::default();
            let (_, stage) =
                stager_db.stage_remove_chained(&mut overlay, &hash, &stager_root, 10).unwrap();
            staged_tx.send(()).unwrap();
            commit_rx.recv().unwrap();
            stager_db.commit_staged(&mut overlay, stage).unwrap();
        });
        thread::sleep(Duration::from_millis(5));
        let reader_db = Arc::clone(&db);
        let reader = thread::spawn(move || reader_db.by_hash(&hash, true));
        thread::sleep(Duration::from_millis(5));
        parking_lot::MutexGuard::unlock_fair(held);
        staged_rx.recv().unwrap();
        wait_until("the reader to finish or park", || {
            reader.is_finished() || db.gate_waiters() == 1
        });
        let before_commit = reader.is_finished();
        stager_first += usize::from(!before_commit);
        commit_tx.send(()).unwrap();
        join_within("the stager", stager).unwrap();
        let answer = join_within("the reader", reader).unwrap().unwrap();
        // Core: a reader before the removal sees the row and the removal erases its entry; a
        // reader after it caches the miss. Either way no stored lock survives in the cache.
        assert_eq!(answer, before_commit.then(|| root.clone()));
        let expected = if before_commit {
            vec![]
        } else {
            vec![(hash, None)]
        };
        assert_eq!(db.cache_snapshot().locks, expected, "stale cache entry survives the commit");
        assert_eq!(db.by_hash(&hash, true).unwrap(), None);
    }
    assert!(stager_first > 0, "the stager-first order never ran in {reps} reps");
}

#[test]
fn iterator_fault_fails_closed_without_a_partial_staged_removal() {
    let world = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Engine("is".into()),
                op: OpKind::Read,
                n: 0,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        }],
        ..WorldConfig::default()
    });
    let engine = world.engine("is", &SCHEMA);
    let db = world.checking(|| IsDb::open(engine.clone()).unwrap());
    let root = lock(1, vec![input(77, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    let before = world.checking(|| rows(&*engine));
    let mut overlay = IsOverlay::default();
    assert!(db.stage_remove_chained(&mut overlay, &hash, &root, 10).is_err());
    assert!(overlay.stages.is_empty());
    assert_eq!(db.gate_waiters(), 0);
    assert_eq!(rows(&*engine), before);
    assert!(world.fault_fired());
    assert!(db.gate.state.lock().latched.is_none());
    assert_eq!(db.by_hash(&hash, false).unwrap(), Some(root));
    let read_failures = world
        .trace()
        .entries
        .iter()
        .filter(|entry| matches!(entry, dash_kv::faulty::TraceEntry::ReadFailed { .. }))
        .count();
    assert_eq!(read_failures, 1);
}

#[test]
fn staging_point_read_fault_keeps_the_earlier_stage_gate_and_its_commit() {
    let world = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Engine("is".into()),
                op: OpKind::Read,
                n: 2,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        }],
        ..WorldConfig::default()
    });
    let engine = world.engine("is", &SCHEMA);
    let db = world.checking(|| IsDb::open(engine.clone()).unwrap());
    let first = lock(1, vec![input(80, 0)]);
    let second = lock(2, vec![input(80, 1)]);
    let c1 = lock(3, vec![OutPoint::new(txid(2), 0)]);
    let c2 = lock(4, vec![OutPoint::new(txid(2), 1)]);
    let [h1, h2, hc1, hc2] = [&first, &second, &c1, &c2].map(isdlock_hash);
    for (hash, object) in [(h1, &first), (h2, &second), (hc1, &c1), (hc2, &c2)] {
        db.write_new(&hash, object).unwrap();
    }
    let mut overlay = IsOverlay::default();
    let (_, s1) =
        world.checking(|| db.stage_remove_chained(&mut overlay, &h1, &first, 10).unwrap());
    // Reads 0..=2 of the second stage: the child scan, c1, then c2 (faulted).
    assert!(db.stage_remove_chained(&mut overlay, &h2, &second, 10).is_err());
    assert!(world.fault_fired());
    assert_eq!(overlay.stages.len(), 1);
    {
        let gate = db.gate.state.lock();
        assert_eq!(gate.owner, Some(thread::current().id()));
        assert!(gate.latched.is_none());
    }
    let cached: Vec<_> = db.cache_snapshot().locks.into_iter().map(|(hash, _)| hash).collect();
    assert!(!cached.contains(&hc1), "c1's transitions before the fault stay");
    assert!(cached.contains(&hc2), "the faulted read inserts nothing and erases nothing");
    db.commit_staged(&mut overlay, s1).unwrap();
    assert!(db.gate.state.lock().owner.is_none());
    assert_eq!(db.by_hash(&h1, false).unwrap(), None);
    assert_eq!(db.by_hash(&h2, false).unwrap(), Some(second));
    assert_eq!(db.by_hash(&hc1, false).unwrap(), Some(c1));
}

#[test]
fn committed_stage_recovers_as_an_atomic_pending_batch_after_power_loss() {
    let world = World::new(WorldConfig::default());
    let engine = world.engine("is", &SCHEMA);
    let db = IsDb::open(engine.clone()).unwrap();
    let root = lock(1, vec![input(78, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    engine.sync_wal().unwrap();
    let base = rows(&*engine);
    let mut overlay = IsOverlay::default();
    let (_, stage) = db.stage_remove_chained(&mut overlay, &hash, &root, 10).unwrap();
    db.commit_staged(&mut overlay, stage).unwrap();
    let committed = rows(&*engine);
    let dfs = Dfs::new();
    world.crash_in_place(dash_kv::faulty::CrashKind::PowerLoss, dfs.decider());
    drop(db);
    let reopened = IsDb::open(engine.clone()).unwrap();
    let recovered = rows(&*engine);
    assert!(recovered == base || recovered == committed);
    assert_eq!(rows(&*engine), rows(&*reopened.engine));
}

#[test]
fn core_serialization_vectors() {
    let hash = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];
    for (prefix, expected) in [
        (keys::LOCK, b"\x04is_i".as_slice()),
        (keys::TX, b"\x05is_tx".as_slice()),
        (keys::ARCHIVE_HASH, b"\x05is_a2".as_slice()),
    ] {
        let mut vector = expected.to_vec();
        vector.extend_from_slice(&hash);
        assert_eq!(keys::hash(prefix, &hash), vector);
    }
    for (prefix, expected) in
        [(keys::MINED, b"\x04is_m".as_slice()), (keys::ARCHIVE_HEIGHT, b"\x05is_a1".as_slice())]
    {
        let mut vector = expected.to_vec();
        vector.extend_from_slice(&[0xfe, 0xfd, 0xfc, 0xfb]);
        vector.extend_from_slice(&hash);
        assert_eq!(keys::height(prefix, 0x01020304, &hash), vector);
        let mut zero = expected.to_vec();
        zero.extend_from_slice(&[0xff; 4]);
        zero.extend_from_slice(&hash);
        assert_eq!(keys::height(prefix, 0, &hash), zero);
    }
    let mut vector = b"\x05is_in".to_vec();
    vector.extend_from_slice(&hash);
    vector.extend_from_slice(&[4, 3, 2, 1]);
    assert_eq!(keys::input(&OutPoint::new(TxHash::from(hash), 0x01020304)), vector);
    let (engine, db) = memory();
    assert_eq!(rows(&*engine), BTreeMap::from([(b"\x04is_v".to_vec(), vec![1, 0, 0, 0])]));
    let object = lock(1, vec![input(2, 3)]);
    let object_hash = isdlock_hash(&object);
    db.write_new(&object_hash, &object).unwrap();
    let mut wire = vec![1, 1];
    wire.extend_from_slice(txid(2).as_bytes());
    wire.extend_from_slice(&[3, 0, 0, 0]);
    wire.extend_from_slice(txid(1).as_bytes());
    wire.extend_from_slice(&[0; 128]);
    assert_eq!(engine.get(IS, &keys::hash(keys::LOCK, &object_hash)).unwrap(), Some(wire));
    db.write_mined(&object_hash, 7).unwrap();
    assert_eq!(engine.get(IS, &keys::height(keys::MINED, 7, &object_hash)).unwrap(), Some(vec![1]));
}

#[test]
fn overlapping_inputs_are_unconditionally_erased() {
    let (engine, db) = memory();
    let x = input(99, 0);
    let y = input(99, 1);
    let a = lock(1, vec![x, y]);
    let b = lock(2, vec![x]);
    let (ha, hb) = (isdlock_hash(&a), isdlock_hash(&b));
    db.write_new(&ha, &a).unwrap();
    db.write_new(&hb, &b).unwrap();
    assert_eq!(db.by_input(&x).unwrap(), Some(b.clone()));
    assert_eq!(db.by_input(&y).unwrap(), Some(a.clone()));
    assert_eq!(db.remove_chained(&ha, &a, 10).unwrap().hashes, vec![ha]);
    assert_eq!(db.by_input(&x).unwrap(), None);
    assert_eq!(db.by_txid(&txid(2)).unwrap(), Some(b));
    assert!(!rows(&*engine).contains_key(&keys::input(&x)));
}

#[test]
fn chained_order_repeats_snapshot_reads_and_missing_root() {
    let (engine, db) = memory();
    let locks = [
        lock(1, vec![input(90, 0)]),
        lock(2, vec![input(1, 1), input(1, 2)]),
        lock(3, vec![input(1, 256)]),
        lock(4, vec![input(2, 0)]),
        lock(5, vec![input(3, 0)]),
        lock(6, vec![input(4, 0)]),
    ];
    let hashes: Vec<_> = locks.iter().map(isdlock_hash).collect();
    for (hash, object) in hashes.iter().zip(&locks) {
        db.write_new(hash, object).unwrap();
    }
    assert_eq!(db.by_parent(&txid(1)).unwrap(), vec![hashes[2], hashes[1], hashes[1]]);
    let result = db.remove_chained(&hashes[0], &locks[0], 42).unwrap();
    assert!(result.root_present);
    assert_eq!(
        result.hashes,
        vec![hashes[2], hashes[1], hashes[1], hashes[3], hashes[5], hashes[4], hashes[0]]
    );
    assert_eq!(db.count().unwrap(), 0);
    assert!(db.state.lock().locks.entries().is_empty());
    let persisted = rows(&*engine);
    let replay = db.remove_chained(&hashes[0], &locks[0], 42).unwrap();
    assert_eq!(replay.hashes, vec![hashes[0]]);
    assert!(!replay.root_present);
    assert_eq!(rows(&*engine), persisted);
    assert!(!db.state.lock().locks.entries().contains_key(&hashes[0]));
    let missing = [0xff; 32];
    let child = lock(8, vec![input(77, 0)]);
    let hc = isdlock_hash(&child);
    db.write_new(&hc, &child).unwrap();
    let absent_root = lock(77, vec![input(76, 0)]);
    assert_eq!(db.remove_chained(&missing, &absent_root, 43).unwrap().hashes, vec![hc, missing]);
    assert!(db.known(&missing).unwrap());
}

#[test]
fn confirmation_keeps_cache_bypass_replaces_it_and_restart_resets_guard() {
    let (engine, db) = memory();
    let object = lock(1, vec![input(9, 0)]);
    let hash = isdlock_hash(&object);
    db.write_new(&hash, &object).unwrap();
    db.write_mined(&hash, 0).unwrap();
    assert!(db.remove_confirmed(0).unwrap().is_empty());
    assert_eq!(db.count().unwrap(), 1);
    assert_eq!(db.remove_confirmed(1).unwrap(), BTreeMap::from([(hash, object.clone())]));
    assert_eq!(db.count().unwrap(), 0);
    assert_eq!(db.by_txid(&txid(1)).unwrap(), Some(object.clone()));
    assert_eq!(db.by_input(&input(9, 0)).unwrap(), Some(object));
    assert_eq!(db.by_hash(&hash, false).unwrap(), None);
    assert_eq!(db.by_txid(&txid(1)).unwrap(), None);
    assert!(db.known(&hash).unwrap());
    db.write_mined(&[3; 32], 1).unwrap();
    assert!(db.remove_confirmed(1).unwrap().is_empty());
    assert!(rows(&*engine).contains_key(&keys::height(keys::MINED, 1, &[3; 32])));
    let reopened = IsDb::open(engine.clone()).unwrap();
    assert_eq!(reopened.state.lock().best_confirmed_height, 0);
    assert!(reopened.state.lock().txids.entries().is_empty());
    reopened.remove_confirmed(1).unwrap();
    assert!(!rows(&*engine).contains_key(&keys::height(keys::MINED, 1, &[3; 32])));
    reopened.remove_archived(0).unwrap();
    assert!(reopened.known(&hash).unwrap());
    reopened.remove_archived(1).unwrap();
    assert!(!reopened.known(&hash).unwrap());
}

#[test]
fn lru_truncates_only_above_twice_target_and_refreshes_reads_and_inserts() {
    let mut cache = Lru::default();
    for i in 0..20_000 {
        cache.insert(i, i);
    }
    assert_eq!(cache.entries().len(), 20_000);
    assert_eq!(cache.get(&0), Some(0));
    cache.insert(1, 99);
    assert_eq!(cache.get(&30_000), None);
    cache.insert(20_000, 20_000);
    assert_eq!(cache.entries().len(), 10_000);
    assert_eq!(cache.get(&0), Some(0));
    assert_eq!(cache.get(&1), Some(99));
    assert_eq!(cache.get(&10_002), None);
    assert_eq!(cache.get(&10_003), Some(10_003));
}

#[test]
fn retained_locks_disappear_on_eviction_but_archives_remain_known() {
    let (_engine, db) = memory();
    let object = lock(1, vec![input(99, 0)]);
    let hash = isdlock_hash(&object);
    db.write_new(&hash, &object).unwrap();
    db.write_mined(&hash, 1).unwrap();
    db.remove_confirmed(1).unwrap();
    for tag in 2..=20_001 {
        let next = lock(tag, vec![input(99, tag)]);
        db.write_new(&isdlock_hash(&next), &next).unwrap();
    }
    assert_eq!(db.state.lock().locks.entries().len(), 10_000);
    assert_eq!(db.state.lock().txids.entries().len(), 10_000);
    assert_eq!(db.state.lock().inputs.entries().len(), 10_000);
    assert_eq!(db.by_txid(&object.txid()).unwrap(), None);
    assert_eq!(db.by_input(&input(99, 0)).unwrap(), None);
    assert!(db.known(&hash).unwrap());
}

#[test]
fn stored_hash_is_checked_and_only_hash_misses_are_cached() {
    let (engine, db) = memory();
    let object = lock(1, vec![input(9, 0)]);
    let hash = isdlock_hash(&object);
    let mut wire = Vec::new();
    object.encode(&mut wire);
    let wrong = [7; 32];
    let mut batch = engine.batch();
    batch.put(IS, &keys::hash(keys::LOCK, &wrong), &wire);
    batch.put(IS, &keys::hash(keys::LOCK, &hash), &[1]);
    engine.write(batch, Commit::Buffered).unwrap();
    assert_eq!(db.by_hash(&wrong, false).unwrap(), None);
    assert_eq!(db.by_hash(&hash, false).unwrap(), None);
    assert_eq!(db.by_hash(&[0; 32], false).unwrap(), None);
    assert_eq!(db.by_input(&input(9, 0)).unwrap(), None);
    assert_eq!(db.hash_by_txid(&txid(1)).unwrap(), [0; 32]);
    let state = db.state.lock();
    assert_eq!(state.locks.entries().len(), 2);
    assert!(state.inputs.entries().is_empty());
    assert!(state.txids.entries().is_empty());
}

#[test]
fn conflicting_lock_uses_asset_unlock_lock_input() {
    let (_engine, db) = memory();
    let first = asset_unlock(41, 0);
    let second = asset_unlock(41, 1);
    let other_index = asset_unlock(42, 0);
    let lock =
        LockedTx::new(1, lock_inputs(&first), first.txid(), Default::default(), [0; 96].into());
    let hash = isdlock_hash(&lock);
    db.write_new(&hash, &lock).unwrap();

    assert_eq!(db.conflicting_lock(&second).unwrap(), Some((hash, lock.clone())));
    assert_eq!(db.conflicting_lock(&first).unwrap(), None);
    assert_eq!(db.conflicting_lock(&other_index).unwrap(), None);
}

#[test]
fn block_marks_filter_lock_inputs_and_use_retained_txid_indexes() {
    let (engine, db) = memory();
    let txs = [
        transaction(1),
        Tx::new(Transaction::new(
            1,
            TxType::new(0),
            vec![TxIn::new(input(0, u32::MAX), Default::default(), u32::MAX)],
            vec![],
            0,
            vec![],
        )),
        Tx::new(Transaction::new(3, TxType::new(6), vec![], vec![], 0, vec![0])),
        Tx::new(Transaction::new(
            3,
            TxType::new(9),
            vec![],
            vec![],
            0,
            vec![0; ASSET_UNLOCK_PAYLOAD_SIZE],
        )),
    ];
    let mut hashes = Vec::new();
    for tx in &txs {
        let object = LockedTx::new(
            1,
            vec![input(99, hashes.len() as u32)],
            tx.txid(),
            Default::default(),
            [0; 96].into(),
        );
        let hash = isdlock_hash(&object);
        db.write_new(&hash, &object).unwrap();
        hashes.push(hash);
    }
    db.block_connected(&txs, 5).unwrap();
    for (index, hash) in hashes.iter().enumerate() {
        assert_eq!(
            engine.get(IS, &keys::height(keys::MINED, 5, hash)).unwrap().is_some(),
            index == 0 || index == 3
        );
    }
    db.remove_confirmed(5).unwrap();
    db.block_connected(&txs, 6).unwrap();
    assert!(engine.get(IS, &keys::height(keys::MINED, 6, &hashes[0])).unwrap().is_some());
    db.block_disconnected(&txs, 6).unwrap();
    assert!(engine.get(IS, &keys::height(keys::MINED, 6, &hashes[0])).unwrap().is_none());
}

#[test]
fn version_upgrade_wipes_and_newer_refuses_without_writes() {
    for version in [None, Some(vec![0; 4]), Some(vec![0xff; 4]), Some(vec![1])] {
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let mut batch = engine.batch();
        batch.put(IS, b"foreign", b"data");
        if let Some(value) = version {
            batch.put(IS, keys::VERSION, &value);
        }
        engine.write(batch, Commit::Buffered).unwrap();
        IsDb::open(engine.clone()).unwrap();
        assert_eq!(rows(&*engine), BTreeMap::from([(b"\x04is_v".to_vec(), vec![1, 0, 0, 0])]));
        engine.simulate_power_loss(0);
        assert_eq!(rows(&*engine), BTreeMap::from([(b"\x04is_v".to_vec(), vec![1, 0, 0, 0])]));
    }
    let (engine, _) = memory();
    let mut batch = engine.batch();
    batch.put(IS, keys::VERSION, &[2, 0, 0, 0]);
    batch.put(IS, b"sentinel", b"keep");
    engine.write(batch, Commit::Buffered).unwrap();
    let before = rows(&*engine);
    assert!(matches!(
        IsDb::open(engine.clone()),
        Err(StoreError::SchemaTooNew {
            found: 2,
            ..
        })
    ));
    assert_eq!(rows(&*engine), before);
}

#[test]
fn a_wiping_open_empties_the_store_whatever_its_version() {
    let current = BTreeMap::from([(b"\x04is_v".to_vec(), vec![1, 0, 0, 0])]);
    for version in [[1, 0, 0, 0], [2, 0, 0, 0]] {
        let engine = Arc::new(MemoryEngine::new(&SCHEMA));
        let mut batch = engine.batch();
        batch.put(IS, keys::VERSION, &version);
        engine.write(batch, Commit::Durable).unwrap();
        if version[0] == 1 {
            let db = IsDb::open(engine.clone()).unwrap();
            let object = lock(1, vec![input(9, 0)]);
            db.write_new(&isdlock_hash(&object), &object).unwrap();
            db.write_mined(&isdlock_hash(&object), 7).unwrap();
            assert!(rows(&*engine).len() > 1);
        }
        let db = IsDb::open_wiping(engine.clone(), true).unwrap();
        assert_eq!(rows(&*engine), current, "version {version:?}");
        engine.simulate_power_loss(0);
        assert_eq!(rows(&*engine), current, "the wipe is durable");
        assert_eq!(db.by_input(&input(9, 0)).unwrap(), None);
    }
    let (engine, db) = memory();
    let object = lock(2, vec![input(9, 1)]);
    db.write_new(&isdlock_hash(&object), &object).unwrap();
    let before = rows(&*engine);
    drop(db);
    IsDb::open_wiping(engine.clone(), false).unwrap();
    assert_eq!(rows(&*engine), before, "no wipe keeps the rows");
}

#[test]
fn rocks_reopen_preserves_rows_and_drops_caches() {
    let temp = tempfile::tempdir().unwrap();
    let object = lock(1, vec![input(9, 0)]);
    let hash = isdlock_hash(&object);
    let before;
    {
        let (engine, _) = RocksEngine::open(temp.path(), &SCHEMA, &RocksConfig::default()).unwrap();
        let engine = Arc::new(engine);
        let db = IsDb::open(engine.clone()).unwrap();
        db.write_new(&hash, &object).unwrap();
        db.write_mined(&hash, 12).unwrap();
        db.remove_confirmed(12).unwrap();
        assert_eq!(db.by_txid(&object.txid()).unwrap(), Some(object.clone()));
        before = rows(&*engine);
    }
    let (engine, _) = RocksEngine::open(temp.path(), &SCHEMA, &RocksConfig::default()).unwrap();
    let engine = Arc::new(engine);
    let db = IsDb::open(engine.clone()).unwrap();
    assert_eq!(rows(&*engine), before);
    assert_eq!(db.by_txid(&object.txid()).unwrap(), None);
    assert!(db.known(&hash).unwrap());
}

#[test]
fn failed_writes_propagate_and_do_not_insert_new_lock_cache_entries() {
    let (engine, db) = memory();
    let object = lock(1, vec![input(9, 0)]);
    let hash = isdlock_hash(&object);
    engine.inject_failure(ErrorKind::NoSpace);
    assert_eq!(db.write_new(&hash, &object).unwrap_err().kind(), Some(ErrorKind::NoSpace));
    assert!(db.state.lock().locks.entries().is_empty());
    assert_eq!(db.write_mined(&hash, 1).unwrap_err().kind(), Some(ErrorKind::ReadOnly));
}
