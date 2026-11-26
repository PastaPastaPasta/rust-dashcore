//! The removal bridge over the scripted store and over `IsDb`'s attempt overlay.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::Tx;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_evo::locks::isdlock_hash;
use dash_evo::{Effect, EffectSink};
use dash_kv::{KvRead, MemoryEngine, Result};
use dash_num::Hash256;
use dash_primitives::{BlockHash, TxOut};
use dashd_chainstate::chain::{ChainLockSig, ConnectCx, Fatal};
use dashd_chainstate::connect::{BlockAt, ChainHelper, IsLockRef};
use parking_lot::Mutex;

use super::{IsHelper, IsRemoval, test_log};
use crate::is::db::removal_tests::{READ_FAULT, ReadFault};
use crate::is::db::{IS, IsDb, SCHEMA};
use crate::is::seams::{IsFxLedger, IsObservation, IsOverlay, IsStore, LedgerTake, StageId};
use crate::is::{IsGate, IsLockHash, IsTipCache, LockedTx};

#[derive(Default)]
struct TestLedger {
    observations: Vec<IsObservation>,
    overlay: IsOverlay,
}

impl IsFxLedger for TestLedger {
    fn push(&mut self, observation: IsObservation) {
        self.observations.push(observation);
    }

    fn confirm_next(
        &mut self,
        _hash: &IsLockHash,
        _store: &dyn IsStore,
    ) -> std::result::Result<(), Fatal> {
        unreachable!("the removal bridge confirms nothing")
    }

    fn overlay(&mut self) -> &mut IsOverlay {
        &mut self.overlay
    }

    fn take(&mut self) -> LedgerTake {
        LedgerTake {
            observations: std::mem::take(&mut self.observations),
            ..LedgerTake::default()
        }
    }

    fn clear(&mut self) {
        self.observations.clear();
    }
}

/// The inner helper: recorded `is_conflict` answers for a replay; no removal ever reaches it.
#[derive(Default)]
struct Inner {
    recorded: VecDeque<Option<IsLockRef>>,
}

impl ChainHelper for Inner {
    fn cl_conflict(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result2<bool> {
        unreachable!()
    }

    fn ibd(&mut self, _: &ConnectCx<'_>) -> Result2<bool> {
        unreachable!()
    }

    fn cl_has(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result2<bool> {
        unreachable!()
    }

    fn cl_best(&mut self, _: &ConnectCx<'_>) -> Result2<Option<Arc<ChainLockSig>>> {
        unreachable!()
    }

    fn is_conflict(&mut self, _: &Tx) -> Result2<Option<IsLockRef>> {
        Ok(self.recorded.pop_front().expect("a recorded is_conflict answer"))
    }

    fn remove_conflicting_is_lock(&mut self, _: &IsLockRef, _: &mut dyn EffectSink) {
        panic!("the IS helper never delegates a removal");
    }

    fn sb_required(&mut self, _: &ConnectCx<'_>, _: &BlockAt) -> Result2<bool> {
        unreachable!()
    }

    fn gov_valid(&mut self) -> Result2<bool> {
        unreachable!()
    }

    fn sb_triggered(&mut self, _: u32) -> Result2<bool> {
        unreachable!()
    }

    fn sb_payments(&mut self, _: u32) -> Result2<Option<Vec<TxOut>>> {
        unreachable!()
    }
}

type Result2<T> = std::result::Result<T, Fatal>;

struct Fixture<S> {
    helper: IsHelper<Inner>,
    store: Arc<S>,
    ledger: Arc<Mutex<TestLedger>>,
    spork: Arc<AtomicBool>,
    tip: IsTipCache,
    effects: Vec<Effect>,
}

impl<S: IsStore + 'static> Fixture<S> {
    fn live(store: S) -> Self {
        Self::build(store, false, VecDeque::new())
    }

    fn replaying(store: S, recorded: Vec<Option<IsLockRef>>) -> Self {
        Self::build(store, true, recorded.into())
    }

    fn build(store: S, replay: bool, recorded: VecDeque<Option<IsLockRef>>) -> Self {
        let store = Arc::new(store);
        let ledger = Arc::new(Mutex::new(TestLedger::default()));
        let spork = Arc::new(AtomicBool::new(true));
        let tip = IsTipCache::new();
        tip.set(50);
        let gate = {
            let spork = spork.clone();
            Arc::new(move || IsGate::new(spork.load(Ordering::SeqCst), false, false))
        };
        let inner = Inner {
            recorded,
        };
        let shared: Arc<Mutex<dyn IsFxLedger>> = ledger.clone();
        let dyn_store: Arc<dyn IsStore> = store.clone();
        let helper = if replay {
            IsHelper::replaying(inner, dyn_store, shared, gate, tip.clone())
        } else {
            IsHelper::live(inner, dyn_store, shared, gate, tip.clone())
        };
        Self {
            helper,
            store,
            ledger,
            spork,
            tip,
            effects: Vec::new(),
        }
    }

    fn conflict(&mut self, tx: &Tx) -> Result2<Option<IsLockRef>> {
        self.helper.is_conflict(tx)
    }

    fn remove(&mut self) -> Option<IsRemoval> {
        self.helper.remove_conflicting(&mut self.effects)
    }

    /// `ConnectBlock`'s IS loop for one ChainLocked transaction (`validation.cpp:2806-2811`).
    fn connect_tx(&mut self, tx: &Tx) -> Result2<Vec<IsRemoval>> {
        let mut removals = Vec::new();
        while self.conflict(tx)?.is_some() {
            removals.extend(self.remove());
        }
        Ok(removals)
    }

    fn observations(&self) -> Vec<IsObservation> {
        self.ledger.lock().observations.clone()
    }

    /// Commits every observed stage in order, as the attempt's IS applier does; the last
    /// commit frees the gate for the store's own reads.
    fn commit(&self) {
        let mut ledger = self.ledger.lock();
        let stages: Vec<_> = ledger.observations.iter().map(|o| o.stage).collect();
        for stage in stages {
            self.store.commit_staged(ledger.overlay(), stage).unwrap();
        }
    }
}

fn effects(hashes: &[IsLockHash]) -> Vec<Effect> {
    hashes
        .iter()
        .map(|hash| Effect::RemoveConflictingIsLock {
            lock_hash: (*hash).into(),
        })
        .collect()
}

fn txid(tag: u32) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    TxHash::from(bytes)
}

fn outpoint(tag: u32, index: u32) -> OutPoint {
    OutPoint::new(txid(tag), index)
}

fn lock(tag: u32, inputs: Vec<OutPoint>) -> LockedTx {
    LockedTx::new(1, inputs, txid(tag), Default::default(), [0; 96].into())
}

fn spending(inputs: &[OutPoint], lock_time: u32) -> Tx {
    let inputs =
        inputs.iter().map(|outpoint| TxIn::new(*outpoint, Default::default(), u32::MAX)).collect();
    Tx::new(Transaction::new(1, TxType::new(0), inputs, vec![], lock_time, vec![]))
}

fn reference(hash: IsLockHash, lock: &LockedTx) -> IsLockRef {
    IsLockRef {
        hash: hash.into(),
        txid: lock.txid(),
    }
}

fn memory_db() -> (Arc<MemoryEngine>, IsDb<MemoryEngine>) {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let store = IsDb::open(engine.clone()).unwrap();
    (engine, store)
}

/// Core's first `RemoveConflictingLock` line (`instantsend.cpp:344-345`).
fn removing_line(hash: IsLockHash, root: &LockedTx) -> String {
    format!(
        "INFO CInstantSendManager::RemoveConflictingLock -- txid={}, islock={}: Removing ISLOCK \
         and its chained children",
        root.txid(),
        Hash256::from(hash)
    )
}

/// Core's per-hash `RemoveConflictingLock` line (`instantsend.cpp:349-352`).
fn removed_line(hash: IsLockHash, root: &LockedTx, removed: IsLockHash) -> String {
    format!(
        "INFO CInstantSendManager::RemoveConflictingLock -- txid={}, islock={}: removed (child) \
         ISLOCK {}",
        root.txid(),
        Hash256::from(hash),
        Hash256::from(removed)
    )
}

fn core_hex(bytes: &[u8]) -> String {
    bytes.iter().rev().map(|byte| format!("{byte:02x}")).collect()
}

fn write(store: &impl IsStore, lock: &LockedTx) -> IsLockHash {
    let hash = isdlock_hash(lock);
    store.write_new(&hash, lock).unwrap();
    hash
}

#[test]
fn two_inputs_locked_by_two_locks_are_removed_in_input_order() {
    let (_engine, store) = memory_db();
    let a = lock(1, vec![outpoint(9, 1)]);
    let b = lock(2, vec![outpoint(9, 0)]);
    let (ha, hb) = (write(&store, &a), write(&store, &b));
    let tx = spending(&[outpoint(9, 1), outpoint(9, 0)], 1);
    let mut f = Fixture::live(store);

    let removals = f.connect_tx(&tx).unwrap();
    assert_eq!(removals.iter().map(|r| r.lock_hash).collect::<Vec<_>>(), vec![ha, hb]);
    assert_eq!(f.effects, effects(&[ha, hb]));
    assert!(removals.iter().all(|r| r.root_stored));
    f.commit();
    assert_eq!(f.store.count().unwrap(), 0);
}

#[test]
fn chained_children_are_removed_with_their_root() {
    let (engine, store) = memory_db();
    let root = lock(1, vec![outpoint(9, 0)]);
    let child = lock(2, vec![outpoint(1, 0)]);
    let grandchild = lock(3, vec![outpoint(2, 0)]);
    let hashes = [&root, &child, &grandchild].map(|object| write(&store, object));
    let tx = spending(&[outpoint(9, 0)], 1);
    let child_spender = spending(&[outpoint(1, 0)], 2);
    let mut f = Fixture::live(store);

    let removals = f.connect_tx(&tx).unwrap();
    assert_eq!(removals.len(), 1);
    assert_eq!(removals[0].removed, vec![hashes[1], hashes[2], hashes[0]]);
    assert_eq!(f.conflict(&child_spender).unwrap(), None);
    f.commit();
    for hash in hashes {
        assert!(f.store.known(&hash).unwrap());
        assert_eq!(f.store.by_hash(&hash, true).unwrap(), None);
    }
    assert_eq!(engine.iter_prefix(IS, b"\x04is_i").count(), 0);
}

#[test]
fn core_log_lines_name_the_root_then_every_removed_lock() {
    let (_engine, store) = memory_db();
    let root = lock(1, vec![outpoint(9, 0)]);
    let child = lock(2, vec![outpoint(1, 0)]);
    let grandchild = lock(3, vec![outpoint(2, 0)]);
    let [hr, hc, hg] = [&root, &child, &grandchild].map(|object| write(&store, object));
    let tx = spending(&[outpoint(9, 0)], 1);
    let mut f = Fixture::live(store);
    assert!(f.conflict(&tx).unwrap().is_some());

    let (removal, lines) = test_log::capture(|| f.remove());
    assert_eq!(removal.unwrap().removed, vec![hc, hg, hr]);
    assert_eq!(
        lines,
        vec![
            removing_line(hr, &root),
            removed_line(hr, &root, hc),
            removed_line(hr, &root, hg),
            removed_line(hr, &root, hr),
        ]
    );
    let first = &lines[0];
    assert!(first.contains(&format!("txid={},", core_hex(root.txid().as_bytes()))));
    assert!(first.contains(&format!("islock={}:", core_hex(&hr))));
}

#[test]
fn removing_a_hides_the_input_b_overwrote_and_b_stays_locked() {
    let (_engine, store) = memory_db();
    let (x, y) = (outpoint(99, 0), outpoint(99, 1));
    let a = lock(1, vec![x, y]);
    let b = lock(2, vec![x]);
    let (ha, _hb) = (write(&store, &a), write(&store, &b));
    let overriding = spending(&[y], 1);
    let spends_x = spending(&[x], 2);
    let mut f = Fixture::live(store);

    let removals = f.connect_tx(&overriding).unwrap();
    assert_eq!(removals.iter().map(|r| r.lock_hash).collect::<Vec<_>>(), vec![ha]);
    assert_eq!(f.conflict(&spends_x).unwrap(), None, "through the overlay");
    f.commit();
    assert_eq!(f.conflict(&spends_x).unwrap(), None, "after the commit");
    assert_eq!(f.store.by_txid(&b.txid()).unwrap(), Some(b));
}

#[test]
fn dec1631_cache_cleared_after_conflict_removal() {
    let (engine, store) = memory_db();
    let locked_tx = spending(&[outpoint(9, 0), outpoint(9, 1)], 0);
    let locked = LockedTx::new(
        1,
        vec![outpoint(9, 0), outpoint(9, 1)],
        locked_tx.txid(),
        Default::default(),
        [0; 96].into(),
    );
    let hash = write(&store, &locked);
    store.write_mined(&hash, 10).unwrap();
    store.remove_confirmed(10).unwrap();
    assert_eq!(store.hash_by_txid(&locked.txid()).unwrap(), hash);
    let before = store.cache_snapshot();
    assert!(before.locks.iter().any(|(k, _)| *k == hash), "{before:?}");
    assert!(before.txids.contains(&(locked.txid(), hash)), "{before:?}");
    assert!(locked.inputs().iter().all(|i| before.inputs.contains(&(*i, hash))), "{before:?}");
    let overriding = spending(&[outpoint(9, 1)], 1);
    let mut f = Fixture::live(store);

    let removals = f.connect_tx(&overriding).unwrap();
    assert_eq!(removals.len(), 1);
    assert_eq!(removals[0].lock_hash, hash);
    assert!(!removals[0].root_stored);
    assert_eq!(f.effects, effects(&[hash]));
    f.commit();
    let after = f.store.cache_snapshot();
    assert!(!after.locks.iter().any(|(k, _)| *k == hash), "{after:?}");
    assert!(!after.txids.iter().any(|(_, v)| *v == hash), "{after:?}");
    assert!(!after.inputs.iter().any(|(_, v)| *v == hash), "{after:?}");

    assert_eq!(f.store.hash_by_txid(&locked.txid()).unwrap(), [0; 32]);
    assert_eq!(f.store.by_input(&outpoint(9, 0)).unwrap(), None);
    assert!(f.store.known(&hash).unwrap());
    f.store.block_connected(std::slice::from_ref(&locked_tx), 12).unwrap();
    assert_eq!(engine.iter_prefix(IS, b"\x04is_m").count(), 0);
}

#[test]
fn second_lookup_fault_on_the_store_reads_the_uncached_input_again() {
    let engine = Arc::new(ReadFault::new());
    let store = IsDb::open(engine.clone()).unwrap();
    let unlocked = outpoint(8, 0);
    let locked = lock(1, vec![outpoint(9, 0)]);
    let hash = write(&store, &locked);
    let tx = spending(&[unlocked, outpoint(9, 0)], 1);
    let mut f = Fixture::live(store);

    engine.fail_input_read(&unlocked, 1);
    let found = f.conflict(&tx).unwrap();
    assert_eq!(found, Some(reference(hash, &locked)));
    assert_eq!(f.remove(), None);
    assert!(engine.fired());
    assert!(f.effects.is_empty());
    assert!(f.observations().is_empty());
    assert_eq!(f.store.count().unwrap(), 1);

    let fault = f.conflict(&tx).unwrap_err();
    assert!(fault.0.contains(READ_FAULT));
    assert_eq!(f.conflict(&tx).unwrap(), found);
}

#[test]
fn the_seam_removal_stages_observes_and_emits() {
    let (_engine, store) = memory_db();
    let locked = lock(1, vec![outpoint(9, 0)]);
    let hash = write(&store, &locked);
    let tx = spending(&[outpoint(9, 0)], 1);
    let mut f = Fixture::live(store);

    let found = f.conflict(&tx).unwrap().unwrap();
    f.helper.remove_conflicting_is_lock(&found, &mut f.effects);
    assert_eq!(f.effects, effects(&[hash]));
    let observation = IsObservation {
        lock_hash: hash,
        overriding_txid: tx.txid(),
        locked_txid: locked.txid(),
        archive_height: 50,
        root_stored: true,
        removed: vec![hash],
        stage: StageId(0),
    };
    assert_eq!(f.observations(), vec![observation]);
    assert_eq!(f.conflict(&tx).unwrap(), None);
    f.commit();
    assert_eq!(f.store.count().unwrap(), 0);
}

#[test]
fn live_and_replayed_removals_leave_equal_rows_effects_and_observations() {
    let root = lock(1, vec![outpoint(9, 0)]);
    let child = lock(2, vec![outpoint(1, 0)]);
    let tx = spending(&[outpoint(9, 0)], 1);
    let run = |recorded: Option<Vec<Option<IsLockRef>>>| {
        let (engine, store) = memory_db();
        let hash = write(&store, &root);
        write(&store, &child);
        let mut f = match recorded {
            None => Fixture::live(store),
            Some(recorded) => Fixture::replaying(store, recorded),
        };
        f.tip.set(33);
        let removals = f.connect_tx(&tx).unwrap();
        assert!(f.helper.inner.recorded.is_empty());
        f.commit();
        let rows: Vec<_> = engine.iter_prefix(IS, &[]).map(Result::unwrap).collect();
        (hash, removals, rows, f.observations(), f.effects)
    };
    let live = run(None);
    let replayed = run(Some(vec![Some(reference(live.0, &root)), None]));
    assert_eq!(live.1.len(), 1);
    assert_eq!(live.1[0].archive_height, 33);
    assert_eq!(live, replayed);
}

#[test]
fn spork_off_finds_and_removes_nothing() {
    let (engine, store) = memory_db();
    write(&store, &lock(1, vec![outpoint(9, 0)]));
    let before: Vec<_> = engine.iter_prefix(IS, &[]).map(Result::unwrap).collect();
    let mut f = Fixture::live(store);
    f.spork.store(false, Ordering::SeqCst);

    assert!(f.connect_tx(&spending(&[outpoint(9, 0)], 1)).unwrap().is_empty());
    assert!(f.effects.is_empty());
    assert_eq!(engine.iter_prefix(IS, &[]).map(Result::unwrap).collect::<Vec<_>>(), before);
}

#[cfg(feature = "test-support")]
mod scripted {
    use super::*;
    use crate::is::seams::test_support::{InMemoryIsStore, IsStoreCall};
    use dash_kv::StoreError;

    fn io_error() -> StoreError {
        StoreError::engine("dashd-locks-test", dash_kv::ErrorKind::Io, "scripted read fault")
    }

    fn scripted(lock: &LockedTx) -> (InMemoryIsStore, IsLockHash) {
        let mut store = InMemoryIsStore::default();
        let hash = store.insert(lock.clone());
        (store, hash)
    }

    fn stage_calls(calls: &[IsStoreCall]) -> usize {
        calls.iter().filter(|call| matches!(call, IsStoreCall::StageRemoveChained { .. })).count()
    }

    #[test]
    fn chainlocked_conflict_is_found_again_staged_observed_and_emitted() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        let script = &store.scripted;
        script.lock().unwrap().conflicting_lock_in.extend([Ok(Some((hash, root.clone())))]);
        script.lock().unwrap().conflicting_lock_in.extend([Ok(Some((hash, root.clone())))]);
        let mut f = Fixture::live(store);
        f.tip.set(77);

        let found = f.conflict(&tx).unwrap().unwrap();
        assert_eq!(found, reference(hash, &root));
        let removal = f.remove().unwrap();
        assert_eq!(
            removal,
            IsRemoval {
                lock_hash: hash,
                overriding_txid: tx.txid(),
                locked_txid: root.txid(),
                archive_height: 77,
                root_stored: true,
                removed: vec![hash],
                stage: StageId(0),
            }
        );
        assert_eq!(f.observations(), vec![IsObservation::from(&removal)]);
        assert_eq!(f.effects, effects(&[hash]));
        assert_eq!(
            f.store.calls(),
            vec![
                IsStoreCall::ConflictingLockIn(tx.txid()),
                IsStoreCall::ConflictingLockIn(tx.txid()),
                IsStoreCall::StageRemoveChained {
                    hash,
                    root_txid: root.txid(),
                    archive_h: 77,
                },
            ]
        );
    }

    #[test]
    fn removal_follows_the_second_lookup_not_the_first_answer() {
        let first = lock(1, vec![outpoint(9, 0)]);
        let second = lock(2, vec![outpoint(9, 1)]);
        let mut store = InMemoryIsStore::default();
        let h1 = store.insert(first.clone());
        let h2 = store.insert(second.clone());
        let tx = spending(&[outpoint(9, 0), outpoint(9, 1)], 1);
        store
            .scripted
            .lock()
            .unwrap()
            .conflicting_lock_in
            .extend([Ok(Some((h1, first.clone()))), Ok(Some((h2, second.clone())))]);
        let mut f = Fixture::live(store);

        let found = f.conflict(&tx).unwrap().unwrap();
        assert_eq!(found.hash, h1.into());
        let removal = f.remove().unwrap();
        assert_eq!(removal.lock_hash, h2);
        assert_eq!(removal.locked_txid, second.txid());
        assert_eq!(f.effects, effects(&[h2]));
    }

    #[test]
    fn second_lookup_finding_nothing_removes_nothing() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store
            .scripted
            .lock()
            .unwrap()
            .conflicting_lock_in
            .extend([Ok(Some((hash, root))), Ok(None)]);
        let mut f = Fixture::live(store);

        assert!(f.conflict(&tx).unwrap().is_some());
        assert_eq!(f.remove(), None);
        assert_eq!(stage_calls(&f.store.calls()), 0);
        assert!(f.observations().is_empty());
        assert!(f.effects.is_empty());
        assert_eq!(f.conflict(&tx).unwrap(), None);
    }

    #[test]
    fn spork_off_before_the_second_lookup_removes_nothing() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (mut store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store.conflicts.insert(tx.txid(), hash);
        let mut f = Fixture::live(store);

        assert!(f.conflict(&tx).unwrap().is_some());
        f.spork.store(false, Ordering::SeqCst);
        assert_eq!(f.remove(), None);
        assert_eq!(f.store.calls(), vec![IsStoreCall::ConflictingLockIn(tx.txid())]);
        assert!(f.effects.is_empty());
        assert!(f.observations().is_empty());
    }

    #[test]
    fn second_lookup_fault_is_kept_and_surfaced_once() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (mut store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store.conflicts.insert(tx.txid(), hash);
        store
            .scripted
            .lock()
            .unwrap()
            .conflicting_lock_in
            .extend([Ok(Some((hash, root))), Err(io_error())]);
        let mut f = Fixture::live(store);

        let found = f.conflict(&tx).unwrap().unwrap();
        assert_eq!(f.remove(), None);
        assert_eq!(stage_calls(&f.store.calls()), 0);
        assert!(f.observations().is_empty());
        assert!(f.effects.is_empty());

        let calls = f.store.calls().len();
        assert_eq!(f.conflict(&tx).unwrap_err(), Fatal::new(io_error().to_string()));
        assert_eq!(f.store.calls().len(), calls, "the surfaced fault reads nothing");
        assert_eq!(f.conflict(&tx).unwrap(), Some(found));
    }

    #[test]
    fn staging_failure_is_kept_and_surfaced_once() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (mut store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store.conflicts.insert(tx.txid(), hash);
        store.scripted.lock().unwrap().stage_remove_chained.push_back(Err(io_error()));
        let mut f = Fixture::live(store);

        let found = f.conflict(&tx).unwrap().unwrap();
        assert_eq!(f.remove(), None);
        assert_eq!(stage_calls(&f.store.calls()), 1);
        assert!(f.observations().is_empty());
        assert!(f.effects.is_empty());
        assert_eq!(f.conflict(&tx).unwrap_err(), Fatal::new(io_error().to_string()));
        assert_eq!(f.conflict(&tx).unwrap(), Some(found));
    }

    #[test]
    fn a_second_lookup_fault_logs_nothing_and_a_staging_fault_logs_the_first_line() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (mut store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store.conflicts.insert(tx.txid(), hash);
        store
            .scripted
            .lock()
            .unwrap()
            .conflicting_lock_in
            .extend([Ok(Some((hash, root.clone()))), Err(io_error())]);
        let mut f = Fixture::live(store);

        assert!(f.conflict(&tx).unwrap().is_some());
        let (removal, lines) = test_log::capture(|| f.remove());
        assert_eq!(removal, None);
        assert!(lines.is_empty(), "{lines:?}");
        assert!(f.conflict(&tx).is_err());

        f.store.scripted.lock().unwrap().stage_remove_chained.push_back(Err(io_error()));
        assert!(f.conflict(&tx).unwrap().is_some());
        let (removal, lines) = test_log::capture(|| f.remove());
        assert_eq!(removal, None);
        assert_eq!(lines, vec![removing_line(hash, &root)]);
        assert!(f.conflict(&tx).is_err());
    }

    #[test]
    fn fault_after_an_earlier_removal_keeps_that_removal_alone() {
        let l1 = lock(1, vec![outpoint(9, 0)]);
        let l2 = lock(2, vec![outpoint(9, 1)]);
        let mut store = InMemoryIsStore::default();
        let h1 = store.insert(l1.clone());
        let h2 = store.insert(l2.clone());
        let (tx1, tx2) = (spending(&[outpoint(9, 0)], 1), spending(&[outpoint(9, 1)], 2));
        store.scripted.lock().unwrap().conflicting_lock_in.extend([
            Ok(Some((h1, l1.clone()))),
            Ok(Some((h1, l1.clone()))),
            Ok(None),
            Ok(Some((h2, l2))),
            Err(io_error()),
        ]);
        let mut f = Fixture::live(store);

        let removals = f.connect_tx(&tx1).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(f.connect_tx(&tx2).unwrap_err(), Fatal::new(io_error().to_string()));
        assert_eq!(f.observations(), vec![IsObservation::from(&removals[0])]);
        assert_eq!(f.effects, effects(&[h1]));
        assert_eq!(stage_calls(&f.store.calls()), 1);
    }

    #[test]
    fn removal_without_a_conflicting_transaction_is_a_fault() {
        let mut f = Fixture::live(InMemoryIsStore::default());
        assert_eq!(f.remove(), None);
        assert!(f.store.calls().is_empty());
        assert!(f.conflict(&spending(&[outpoint(9, 0)], 1)).is_err());
    }

    #[test]
    fn a_kept_fault_surfaces_while_the_spork_gate_is_off() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store
            .scripted
            .lock()
            .unwrap()
            .conflicting_lock_in
            .extend([Ok(Some((hash, root))), Err(io_error())]);
        let mut f = Fixture::live(store);

        assert!(f.conflict(&tx).unwrap().is_some());
        assert_eq!(f.remove(), None);
        f.spork.store(false, Ordering::SeqCst);
        let calls = f.store.calls().len();
        assert_eq!(f.conflict(&tx).unwrap_err(), Fatal::new(io_error().to_string()));
        assert_eq!(f.store.calls().len(), calls);
        assert_eq!(f.conflict(&tx).unwrap(), None);
    }

    #[test]
    fn pending_fault_is_taken_by_the_first_query_only() {
        let mut f = Fixture::live(InMemoryIsStore::default());
        let tx = spending(&[outpoint(9, 0)], 1);
        f.helper.pending_fault = Some(Fatal::new("kept"));
        assert_eq!(f.conflict(&tx).unwrap_err(), Fatal::new("kept"));
        assert_eq!(f.conflict(&tx).unwrap(), None);
        f.helper.end_attempt();
        assert_eq!(f.conflict(&tx).unwrap(), None);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "IS fault not surfaced")]
    fn end_attempt_with_an_unsurfaced_fault_asserts() {
        let mut f = Fixture::live(InMemoryIsStore::default());
        f.helper.pending_fault = Some(Fatal::new("kept"));
        f.helper.end_attempt();
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn end_attempt_clears_an_unsurfaced_fault() {
        let mut f = Fixture::live(InMemoryIsStore::default());
        let tx = spending(&[outpoint(9, 0)], 1);
        f.helper.pending_fault = Some(Fatal::new("kept"));
        f.helper.end_attempt();
        assert!(f.helper.pending_fault.is_none());
        assert_eq!(f.conflict(&tx).unwrap(), None);
    }

    #[test]
    fn replay_removal_runs_the_live_path_and_consumes_no_decision() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (mut store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store.conflicts.insert(tx.txid(), hash);
        store.scripted.lock().unwrap().conflicting_lock_in.extend([
            Ok(Some((hash, root.clone()))),
            Ok(Some((hash, root.clone()))),
            Ok(None),
        ]);
        let mut f = Fixture::replaying(store, vec![Some(reference(hash, &root)), None]);

        let removals = f.connect_tx(&tx).unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].lock_hash, hash);
        assert!(f.helper.inner.recorded.is_empty());
        assert_eq!(f.effects, effects(&[hash]));
        assert_eq!(
            f.store.calls(),
            vec![
                IsStoreCall::ConflictingLockIn(tx.txid()),
                IsStoreCall::ConflictingLockIn(tx.txid()),
                IsStoreCall::StageRemoveChained {
                    hash,
                    root_txid: root.txid(),
                    archive_h: 50,
                },
                IsStoreCall::ConflictingLockIn(tx.txid()),
            ]
        );
    }

    #[test]
    fn replay_second_lookup_fault_is_kept_and_surfaced_once() {
        let root = lock(1, vec![outpoint(9, 0)]);
        let (store, hash) = scripted(&root);
        let tx = spending(&[outpoint(9, 0)], 1);
        store
            .scripted
            .lock()
            .unwrap()
            .conflicting_lock_in
            .extend([Ok(Some((hash, root.clone()))), Err(io_error())]);
        let mut f = Fixture::replaying(store, vec![Some(reference(hash, &root)), None]);

        assert_eq!(f.connect_tx(&tx).unwrap_err(), Fatal::new(io_error().to_string()));
        assert_eq!(f.helper.inner.recorded.len(), 1, "the surfaced fault consumes no decision");
        assert!(f.effects.is_empty());
        assert!(f.observations().is_empty());
    }
}
