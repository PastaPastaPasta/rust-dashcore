//! Chained removal whose root lock is no longer stored (DEC-1631's cache rule).

use std::sync::Arc;

use dash_evo::locks::isdlock_hash;
use dash_kv::{
    Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, MemoryBatch, MemoryEngine, MemorySnapshot,
    Result, StoreError,
};
use parking_lot::Mutex;

use super::tests::{input, lock, rows, transaction, txid};
use super::{IS, IsDb, SCHEMA, keys};
use crate::is::IsLockHash;
use crate::is::seams::IsStore;

/// A memory engine whose `n`-th (0-based) point read of one key fails, once.
pub(crate) struct ReadFault {
    inner: MemoryEngine,
    armed: Mutex<Option<(Vec<u8>, u32)>>,
}

/// The text of the error [`ReadFault`] returns.
pub(crate) const READ_FAULT: &str = "injected read fault";

impl ReadFault {
    pub(crate) fn new() -> Self {
        Self {
            inner: MemoryEngine::new(&SCHEMA),
            armed: Mutex::new(None),
        }
    }

    /// Fails the `n`-th read of the lock row `hash`, counting from now.
    pub(crate) fn fail_lock_read(&self, hash: &IsLockHash, n: u32) {
        self.arm(keys::hash(keys::LOCK, hash), n);
    }

    /// Fails the `n`-th read of the input index row `input`, counting from now.
    pub(crate) fn fail_input_read(&self, input: &dash_consensus::primitives::OutPoint, n: u32) {
        self.arm(keys::input(input), n);
    }

    fn arm(&self, key: Vec<u8>, n: u32) {
        *self.armed.lock() = Some((key, n));
    }

    /// Whether the armed read has failed.
    pub(crate) fn fired(&self) -> bool {
        self.armed.lock().is_none()
    }

    fn check(&self, key: &[u8]) -> Result<()> {
        let mut armed = self.armed.lock();
        let Some((target, n)) = armed.as_mut() else {
            return Ok(());
        };
        if target.as_slice() != key {
            return Ok(());
        }
        if *n > 0 {
            *n -= 1;
            return Ok(());
        }
        *armed = None;
        Err(StoreError::engine("dashd-locks-test", ErrorKind::Io, READ_FAULT))
    }
}

impl KvRead for ReadFault {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.check(key)?;
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix(cf, prefix)
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix_from(cf, prefix, start)
    }
}

impl KvEngine for ReadFault {
    type Batch = MemoryBatch;
    type Snapshot<'a> = MemorySnapshot;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> MemoryBatch {
        self.inner.batch()
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> Result<()> {
        self.inner.write(batch, commit)
    }

    fn sync_wal(&self) -> Result<()> {
        self.inner.sync_wal()
    }

    fn flush_memtables(&self) -> Result<()> {
        self.inner.flush_memtables()
    }

    fn snapshot(&self) -> MemorySnapshot {
        self.inner.snapshot()
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        self.inner.approximate_size(cf)
    }
}

fn memory() -> (Arc<MemoryEngine>, IsDb<MemoryEngine>) {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let db = IsDb::open(engine.clone()).unwrap();
    (engine, db)
}

#[test]
fn dec1631_not_stored_root_clears_its_cached_entries() {
    let (engine, db) = memory();
    let root = lock(1, vec![input(9, 0), input(9, 1)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    db.write_mined(&hash, 5).unwrap();
    db.remove_confirmed(5).unwrap();
    assert_eq!(db.by_txid(&root.txid()).unwrap(), Some(root.clone()));
    assert_eq!(db.by_input(&input(9, 1)).unwrap(), Some(root.clone()));

    let removal = db.remove_chained(&hash, &root, 7).unwrap();
    assert!(!removal.root_present);
    assert_eq!(removal.hashes, vec![hash]);
    {
        let state = db.state.lock();
        assert!(!state.locks.entries().contains_key(&hash));
        assert!(!state.txids.entries().contains_key(&root.txid()));
        assert!(!state.inputs.entries().contains_key(&input(9, 0)));
        assert!(!state.inputs.entries().contains_key(&input(9, 1)));
    }
    assert_eq!(
        engine.get(IS, &keys::height(keys::ARCHIVE_HEIGHT, 7, &hash)).unwrap(),
        Some(vec![1])
    );
    assert_eq!(db.by_txid(&root.txid()).unwrap(), None);
    assert_eq!(db.by_input(&input(9, 0)).unwrap(), None);
    assert!(db.known(&hash).unwrap());
}

#[test]
fn not_stored_root_still_removes_children_of_its_txid() {
    let (engine, db) = memory();
    let root = lock(1, vec![input(9, 0)]);
    let child = lock(2, vec![input(1, 0)]);
    let grandchild = lock(3, vec![input(2, 4)]);
    let (hr, hc, hg) = (isdlock_hash(&root), isdlock_hash(&child), isdlock_hash(&grandchild));
    for (hash, object) in [(hr, &root), (hc, &child), (hg, &grandchild)] {
        db.write_new(&hash, object).unwrap();
    }
    db.write_mined(&hr, 5).unwrap();
    db.remove_confirmed(5).unwrap();

    let removal = db.remove_chained(&hr, &root, 8).unwrap();
    assert!(!removal.root_present);
    assert_eq!(removal.hashes, vec![hc, hg, hr]);
    assert_eq!(db.count().unwrap(), 0);
    let persisted = rows(&*engine);
    for hash in [hc, hg, hr] {
        assert!(persisted.contains_key(&keys::height(keys::ARCHIVE_HEIGHT, 8, &hash)));
        assert!(persisted.contains_key(&keys::hash(keys::ARCHIVE_HASH, &hash)));
    }
    let state = db.state.lock();
    assert!(state.locks.entries().is_empty());
    assert!(state.txids.entries().is_empty());
    assert!(state.inputs.entries().is_empty());
}

#[test]
fn not_stored_root_keeps_entries_another_lock_now_owns() {
    let (_engine, db) = memory();
    let x = input(99, 0);
    let a = lock(1, vec![x, input(99, 1)]);
    let b = lock(2, vec![x]);
    let (ha, hb) = (isdlock_hash(&a), isdlock_hash(&b));
    db.write_new(&ha, &a).unwrap();
    db.write_mined(&ha, 3).unwrap();
    db.remove_confirmed(3).unwrap();
    db.write_new(&hb, &b).unwrap();
    assert_eq!(db.by_input(&x).unwrap(), Some(b.clone()));

    db.remove_chained(&ha, &a, 4).unwrap();
    {
        let state = db.state.lock();
        assert_eq!(state.inputs.entries().get(&x).map(|(hash, _)| *hash), Some(hb));
        assert!(!state.inputs.entries().contains_key(&input(99, 1)));
        assert!(!state.txids.entries().contains_key(&a.txid()));
        assert!(!state.locks.entries().contains_key(&ha));
    }
    assert_eq!(db.by_input(&x).unwrap(), Some(b.clone()));
    assert_eq!(db.by_txid(&txid(2)).unwrap(), Some(b));
}

#[test]
fn dec1631_entries_another_lock_overwrote_keep_answering_for_it() {
    let (_engine, db) = memory();
    // `transaction(_)` spends this outpoint.
    let x = input(123, 0);
    let a = lock(1, vec![x, input(99, 1)]);
    let b = lock(2, vec![x]);
    let (ha, hb) = (isdlock_hash(&a), isdlock_hash(&b));
    db.write_new(&ha, &a).unwrap();
    db.write_new(&hb, &b).unwrap();
    db.write_mined(&ha, 3).unwrap();
    db.remove_confirmed(3).unwrap();
    assert_eq!(db.by_input(&x).unwrap(), Some(b.clone()));

    db.remove_chained(&ha, &a, 4).unwrap();
    assert_eq!(db.by_input(&x).unwrap(), Some(b.clone()));
    assert_eq!(db.conflicting_lock(&transaction(7)).unwrap(), Some((hb, b)));
    assert_eq!(db.by_input(&input(99, 1)).unwrap(), None);
}

#[test]
fn dec1631_txid_entry_another_lock_overwrote_keeps_answering_for_it() {
    let (_engine, db) = memory();
    let a = lock(1, vec![input(9, 0)]);
    let b = lock(1, vec![input(9, 1)]);
    let (ha, hb) = (isdlock_hash(&a), isdlock_hash(&b));
    db.write_new(&ha, &a).unwrap();
    db.write_new(&hb, &b).unwrap();
    db.write_mined(&ha, 3).unwrap();
    db.remove_confirmed(3).unwrap();
    assert_eq!(db.by_txid(&txid(1)).unwrap(), Some(b.clone()));

    db.remove_chained(&ha, &a, 4).unwrap();
    assert_eq!(db.hash_by_txid(&txid(1)).unwrap(), hb);
    assert_eq!(db.by_txid(&txid(1)).unwrap(), Some(b.clone()));
    assert_eq!(db.by_input(&input(9, 1)).unwrap(), Some(b));
    assert_eq!(db.by_input(&input(9, 0)).unwrap(), None);
}

#[test]
fn repeated_removal_inserts_and_clears_the_negative_root_entry() {
    let (engine, db) = memory();
    let root = lock(1, vec![input(9, 0)]);
    let hash = isdlock_hash(&root);
    db.write_new(&hash, &root).unwrap();
    db.remove_chained(&hash, &root, 4).unwrap();
    let once = rows(&*engine);
    let other = lock(5, vec![input(9, 5)]);
    db.write_new(&isdlock_hash(&other), &other).unwrap();
    let before = db.state.lock().locks.entries().len();

    let replay = db.remove_chained(&hash, &root, 6).unwrap();
    assert!(!replay.root_present);
    assert_eq!(db.state.lock().locks.entries().len(), before);
    assert!(!db.state.lock().locks.entries().contains_key(&hash));
    let after = rows(&*engine);
    assert!(after.contains_key(&keys::height(keys::ARCHIVE_HEIGHT, 6, &hash)));
    assert!(once.contains_key(&keys::height(keys::ARCHIVE_HEIGHT, 4, &hash)));
}

#[test]
fn a_child_read_fault_keeps_earlier_transitions_and_writes_nothing() {
    let engine = Arc::new(ReadFault::new());
    let db = IsDb::open(engine.clone()).unwrap();
    let root = lock(1, vec![input(9, 0)]);
    let first = lock(2, vec![input(1, 0)]);
    let second = lock(3, vec![input(1, 1)]);
    let [hr, h1, h2] = [&root, &first, &second].map(isdlock_hash);
    for (hash, object) in [(hr, &root), (h1, &first), (h2, &second)] {
        db.write_new(&hash, object).unwrap();
    }
    let before = rows(&*engine);
    engine.fail_lock_read(&h2, 0);

    let err = db.remove_chained(&hr, &root, 6).unwrap_err();
    assert!(engine.fired());
    assert!(err.to_string().contains(READ_FAULT));
    assert_eq!(rows(&*engine), before);
    let state = db.state.lock();
    assert!(!state.locks.entries().contains_key(&h1));
    assert!(!state.txids.entries().contains_key(&first.txid()));
    assert!(!state.inputs.entries().contains_key(&input(1, 0)));
    assert_eq!(state.locks.entries().get(&h2).map(|(lock, _)| lock.clone()), Some(Some(second)));
    assert!(state.locks.entries().contains_key(&hr));
    assert!(state.txids.entries().contains_key(&root.txid()));
}
