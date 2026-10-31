// The section tests' gates use raw channels and threads; LO-9's ban is for the crate's own wait
// paths.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dashd_store::{Commit, KvBatch, KvRead};

use super::*;
use crate::evo::{codec, testkit};

/// A row store that counts its reads and writes, and fails them on demand.
#[derive(Default)]
struct Counting {
    rows: Mutex<BTreeMap<SnapshotKey, QuorumSnapshot>>,
    reads: AtomicUsize,
    writes: AtomicUsize,
    fail_reads: AtomicBool,
    fail_writes: AtomicBool,
}

impl SnapshotStore for Counting {
    fn read_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(StoreError::Read {
                what: "planted read fault".into(),
            });
        }
        Ok(self.rows.lock().get(&key(llmq_type, cycle_base)).cloned())
    }

    fn write_snapshot(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(StoreError::Read {
                what: "planted write fault".into(),
            });
        }
        self.rows.lock().insert(key(llmq_type, cycle_base), snapshot.clone());
        Ok(())
    }
}

const T: LlmqType = LlmqType::LlmqTestDip0024;
const OTHER: LlmqType = LlmqType::LlmqTest;

fn hash(n: u32) -> BlockHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&n.to_le_bytes());
    BlockHash::from(bytes)
}

fn snapshot(mode: i32) -> QuorumSnapshot {
    QuorumSnapshot {
        active_quorum_members: vec![true, false, true],
        skip_list_mode: mode,
        skip_list: vec![2, 5],
    }
}

fn rows() -> (Arc<Counting>, Arc<EffectLedger>, RuntimeRows) {
    let store = Arc::new(Counting::default());
    let ledger = Arc::new(EffectLedger::default());
    let rows = RuntimeRows::new(store.clone(), ledger.clone());
    (store, ledger, rows)
}

/// `GetSnapshotForBlock`: a cold read goes to disk and inserts the row; a warm one does not read
/// the disk again. An absent row is no answer, and nothing is inserted for it.
#[test]
fn a_cold_read_inserts_and_a_warm_read_stays_off_the_disk() {
    let (store, _, rows) = rows();
    store.rows.lock().insert(key(T, &hash(1)), snapshot(0));
    assert_eq!(rows.peek(T, &hash(1)), None);
    assert_eq!(rows.snapshot_for_block(T, &hash(1)).unwrap(), Some(snapshot(0)));
    assert_eq!(rows.peek(T, &hash(1)), Some(snapshot(0)));
    assert_eq!(rows.snapshot_for_block(T, &hash(1)).unwrap(), Some(snapshot(0)));
    assert_eq!(store.reads.load(Ordering::SeqCst), 1);

    assert_eq!(rows.snapshot_for_block(T, &hash(2)).unwrap(), None);
    assert_eq!(rows.snapshot_for_block(T, &hash(2)).unwrap(), None);
    assert_eq!(store.reads.load(Ordering::SeqCst), 3);
}

/// `StoreSnapshotForBlock`: the write reaches the disk and the cache together; an overwrite
/// replaces both. Each write records one presumed effect.
#[test]
fn a_write_reaches_disk_and_cache_and_records_once() {
    let (store, ledger, rows) = rows();
    rows.write(T, &hash(1), &snapshot(0)).unwrap();
    assert_eq!(store.rows.lock().get(&key(T, &hash(1))), Some(&snapshot(0)));
    assert_eq!(rows.peek(T, &hash(1)), Some(snapshot(0)));
    assert_eq!(ledger.current(), 1);
    rows.write(T, &hash(1), &snapshot(1)).unwrap();
    assert_eq!(rows.snapshot_for_block(T, &hash(1)).unwrap(), Some(snapshot(1)));
    assert_eq!(store.rows.lock().get(&key(T, &hash(1))), Some(&snapshot(1)));
    assert_eq!((store.reads.load(Ordering::SeqCst), ledger.current()), (0, 2));
}

/// The LLMQ type is part of the key, on disk and in the cache.
#[test]
fn the_type_is_part_of_the_key() {
    let (store, _, rows) = rows();
    rows.write(T, &hash(1), &snapshot(0)).unwrap();
    assert_eq!(rows.peek(OTHER, &hash(1)), None);
    assert_eq!(rows.snapshot_for_block(OTHER, &hash(1)).unwrap(), None);
    rows.write(OTHER, &hash(1), &snapshot(1)).unwrap();
    assert_eq!(rows.snapshot_for_block(T, &hash(1)).unwrap(), Some(snapshot(0)));
    assert_eq!(rows.snapshot_for_block(OTHER, &hash(1)).unwrap(), Some(snapshot(1)));
    assert_eq!(store.rows.lock().len(), 2);
}

/// The cache is Core's LRU of 32: past 64 entries the 32 most recently stamped stay; a row that
/// left the cache is read from disk again.
#[test]
fn the_cache_keeps_the_newest_thirty_two_past_sixty_four() {
    let (store, _, rows) = rows();
    let n = u32::try_from(2 * SNAPSHOT_CACHE_SIZE).unwrap();
    for i in 0..n {
        rows.write(T, &hash(i), &snapshot(0)).unwrap();
    }
    assert!((0..n).all(|i| rows.peek(T, &hash(i)).is_some()));
    rows.write(T, &hash(n), &snapshot(0)).unwrap();
    let kept: Vec<u32> = (0..=n).filter(|i| rows.peek(T, &hash(*i)).is_some()).collect();
    assert_eq!(kept, (n + 1 - 32..=n).collect::<Vec<_>>());
    assert_eq!(rows.snapshot_for_block(T, &hash(0)).unwrap(), Some(snapshot(0)));
    assert_eq!(store.reads.load(Ordering::SeqCst), 1);
}

/// A failed write leaves the cache as it was and records nothing, so the next read still answers
/// the old row (M18a's kill: an insert ahead of the write would answer the new one).
#[test]
fn a_failed_write_leaves_the_cache_and_the_ledger() {
    let (store, ledger, rows) = rows();
    rows.write(T, &hash(1), &snapshot(0)).unwrap();
    store.fail_writes.store(true, Ordering::SeqCst);
    assert!(rows.write(T, &hash(1), &snapshot(1)).is_err());
    assert_eq!(rows.peek(T, &hash(1)), Some(snapshot(0)));
    assert_eq!(rows.snapshot_for_block(T, &hash(1)).unwrap(), Some(snapshot(0)));
    assert_eq!(ledger.current(), 1);
    // A write that fails on a cold key inserts nothing either.
    assert!(rows.write(T, &hash(2), &snapshot(1)).is_err());
    assert_eq!(rows.peek(T, &hash(2)), None);
}

/// A failed read is the read's fault and inserts nothing.
#[test]
fn a_failed_read_inserts_nothing() {
    let (store, _, rows) = rows();
    store.rows.lock().insert(key(T, &hash(1)), snapshot(0));
    store.fail_reads.store(true, Ordering::SeqCst);
    assert!(rows.snapshot_for_block(T, &hash(1)).is_err());
    assert_eq!(rows.peek(T, &hash(1)), None);
}

/// Over the evo store: the row goes to `llmq_snapshot` with Core's encoding, and a fresh runtime
/// (a cold cache, as after a restart) reads it back from there.
#[test]
fn the_evo_store_keeps_the_row_across_a_cold_cache() {
    let store = Arc::new(testkit::memory_store());
    let ledger = Arc::new(EffectLedger::default());
    let rows = RuntimeRows::new(store.clone(), ledger.clone());
    assert_eq!(rows.snapshot_for_block(T, &hash(7)).unwrap(), None);
    rows.write(T, &hash(7), &snapshot(3)).unwrap();
    let raw = store
        .db()
        .get(codec::cf("llmq_snapshot"), &codec::quorum_snapshot_key(T, &hash(7)))
        .unwrap()
        .expect("written");
    assert_eq!(raw, codec::encode_quorum_snapshot(&snapshot(3)));
    let cold = RuntimeRows::new(store, ledger);
    assert_eq!(cold.peek(T, &hash(7)), None);
    assert_eq!(cold.snapshot_for_block(T, &hash(7)).unwrap(), Some(snapshot(3)));
    assert_eq!(cold.peek(T, &hash(7)), Some(snapshot(3)));
}

/// A stored row that does not decode reads as absent and is not cached, as `CDBWrapper::Read`
/// answers false when deserialization throws; the next write replaces it.
#[test]
fn an_undecodable_row_reads_as_absent() {
    let store = Arc::new(testkit::memory_store());
    let rows = RuntimeRows::new(store.clone(), Arc::new(EffectLedger::default()));
    let mut b = store.db().batch();
    b.put(codec::cf("llmq_snapshot"), &codec::quorum_snapshot_key(T, &hash(7)), &[1, 2]);
    store.db().write(b, Commit::Buffered).unwrap();
    assert_eq!(rows.snapshot_for_block(T, &hash(7)).unwrap(), None);
    assert_eq!(rows.peek(T, &hash(7)), None);
    rows.write(T, &hash(7), &snapshot(3)).unwrap();
    let cold = RuntimeRows::new(store, Arc::new(EffectLedger::default()));
    assert_eq!(cold.snapshot_for_block(T, &hash(7)).unwrap(), Some(snapshot(3)));
}

/// The two sections are one lock: a write that starts while a getter holds the section (after its
/// disk read, before its insert) waits before its raw write, so the getter's stale row is
/// replaced, not left behind (EV addendum §2.6 rev6, O1).
#[test]
fn a_write_waits_for_a_getter_inside_its_section() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (store, _, rows) = rows();
    let rows = Arc::new(rows);
    store.rows.lock().insert(key(T, &hash(1)), snapshot(0));
    let (paused_tx, paused) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    *rows.hooks.after_read.lock() = Some(Box::new(move || {
        paused_tx.send(()).unwrap();
        resume_rx.lock().recv().unwrap();
    }));
    let getter = {
        let rows = rows.clone();
        std::thread::spawn(move || rows.snapshot_for_block(T, &hash(1)).unwrap())
    };
    paused.recv_timeout(Duration::from_secs(60)).expect("the getter read the disk");
    let writer = {
        let rows = rows.clone();
        std::thread::spawn(move || rows.write(T, &hash(1), &snapshot(1)).unwrap())
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while rows.hooks.section_waiters.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the writer never waited on the section");
        std::thread::yield_now();
    }
    // Waiting, and not yet at the disk (M18b's kill).
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    resume.send(()).unwrap();
    assert_eq!(getter.join().unwrap(), Some(snapshot(0)));
    writer.join().unwrap();
    assert_eq!(store.rows.lock().get(&key(T, &hash(1))), Some(&snapshot(1)));
    assert_eq!(rows.peek(T, &hash(1)), Some(snapshot(1)));
    assert_eq!(rows.snapshot_for_block(T, &hash(1)).unwrap(), Some(snapshot(1)));
}

/// The negative control: a getter that drops the section around its read lets the write finish
/// during the pause, then inserts the row it read over the one written (M17's shape).
#[test]
fn an_unlocked_getter_leaves_a_stale_row_cached() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (store, _, rows) = rows();
    let rows = Arc::new(rows);
    store.rows.lock().insert(key(T, &hash(1)), snapshot(0));
    rows.hooks.unlocked_read.store(true, Ordering::SeqCst);
    let (paused_tx, paused) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel::<()>();
    let resume_rx = Mutex::new(resume_rx);
    *rows.hooks.after_read.lock() = Some(Box::new(move || {
        paused_tx.send(()).unwrap();
        resume_rx.lock().recv().unwrap();
    }));
    let getter = {
        let rows = rows.clone();
        std::thread::spawn(move || rows.snapshot_for_block(T, &hash(1)).unwrap())
    };
    paused.recv_timeout(Duration::from_secs(60)).expect("the getter read the disk");
    rows.write(T, &hash(1), &snapshot(1)).unwrap();
    resume.send(()).unwrap();
    assert_eq!(getter.join().unwrap(), Some(snapshot(0)));
    assert_eq!(store.rows.lock().get(&key(T, &hash(1))), Some(&snapshot(1)));
    assert_eq!(rows.peek(T, &hash(1)), Some(snapshot(0)));
}
