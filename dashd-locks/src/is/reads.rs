//! The InstantSend reads the signers share, over the IS store (PLAN-LK1c §1.2, M1, m3).

use std::sync::Arc;

use dash_consensus::primitives::TxHash;
use dash_kv::Result;

use super::chain::GateSource;
use super::seams::IsManagerStore;
use crate::signing::IsLockView;

/// [`IsLockView`] over the IS store and the process gate.
pub struct StoreLockView {
    pub store: Arc<dyn IsManagerStore>,
    pub gate: GateSource,
}

impl IsLockView for StoreLockView {
    // Core: v24.0.2 src/instantsend/instantsend.cpp:513-516 IsInstantSendEnabled
    fn is_enabled(&self) -> bool {
        (self.gate)().enabled()
    }

    /// Known through the txid's lock hash, so a lock removed as confirmed still counts while the
    /// txid cache holds it and its archive marker stands.
    // Core: v24.0.2 src/instantsend/instantsend.cpp:400-407 IsLocked
    fn is_locked(&self, txid: &TxHash) -> Result<bool> {
        if !self.is_enabled() {
            return Ok(false);
        }
        self.store.known(&self.store.hash_by_txid(txid)?)
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use dash_consensus::primitives::OutPoint;
    use dash_kv::MemoryEngine;

    use super::*;
    use crate::is::db::{IsDb, SCHEMA};
    use crate::is::seams::IsStore;
    use crate::is::seams::test_support::{InMemoryIsStore, IsStoreCall};
    use crate::is::{IsGate, LockedTx};

    fn view(store: InMemoryIsStore, spork2: bool) -> (StoreLockView, Arc<InMemoryIsStore>) {
        let store = Arc::new(store);
        let gate: GateSource = Arc::new(move || IsGate::new(spork2, false, false));
        (
            StoreLockView {
                store: Arc::clone(&store) as Arc<dyn IsManagerStore>,
                gate,
            },
            store,
        )
    }

    fn lock(txid: TxHash) -> LockedTx {
        LockedTx::new(
            1,
            vec![OutPoint::new(TxHash::from([2; 32]), 0)],
            txid,
            Default::default(),
            [0; 96].into(),
        )
    }

    // Core: v24.0.2 src/instantsend/instantsend.cpp:402-404 IsLocked
    #[test]
    fn instantsend_disabled_reads_nothing_and_is_unlocked() {
        let txid = TxHash::from([1; 32]);
        let mut store = InMemoryIsStore::default();
        store.insert(lock(txid));
        let (view, store) = view(store, false);
        assert!(!view.is_locked(&txid).unwrap());
        assert!(store.calls().is_empty());
    }

    // Core: v24.0.2 src/instantsend/instantsend.cpp:406 IsLocked
    #[test]
    fn a_stored_lock_is_locked() {
        let txid = TxHash::from([1; 32]);
        let mut store = InMemoryIsStore::default();
        let hash = store.insert(lock(txid));
        let (view, store) = view(store, true);
        assert!(view.is_locked(&txid).unwrap());
        assert_eq!(store.calls(), [IsStoreCall::HashByTxid(txid), IsStoreCall::Known(hash)]);
    }

    // Core: v24.0.2 src/instantsend/db.cpp:235-239 KnownInstantSendLock, :286-297
    // GetInstantSendLockHashByTxidInternal
    #[test]
    fn a_confirmed_lock_whose_txid_is_cached_and_archived_is_locked() {
        let txid = TxHash::from([1; 32]);
        let hash = [9; 32];
        let mut store = InMemoryIsStore::default();
        store.archived.insert(hash);
        store.scripted.get_mut().unwrap().hash_by_txid.push_back(Ok(hash));
        let (view, store) = view(store, true);
        assert!(store.by_txid(&txid).unwrap().is_none());
        assert!(view.is_locked(&txid).unwrap());
    }

    // Core: v24.0.2 src/instantsend/db.cpp:148-154 RemoveConfirmedInstantSendLocks,
    // :286-297 GetInstantSendLockHashByTxidInternal
    #[test]
    fn the_database_keeps_a_confirmed_lock_locked_through_its_txid_cache() {
        let txid = TxHash::from([1; 32]);
        let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
        let object = lock(txid);
        let hash = dash_evo::locks::isdlock_hash(&object);
        db.write_new(&hash, &object).unwrap();
        db.write_mined(&hash, 5).unwrap();
        assert_eq!(db.remove_confirmed(5).unwrap().len(), 1);
        let view = StoreLockView {
            store: db,
            gate: Arc::new(|| IsGate::new(true, false, false)),
        };
        assert!(view.is_locked(&txid).unwrap());
    }

    // Core: v24.0.2 src/instantsend/db.cpp:290-293 GetInstantSendLockHashByTxidInternal
    #[test]
    fn an_unknown_txid_is_unlocked() {
        let (view, _) = view(InMemoryIsStore::default(), true);
        assert!(!view.is_locked(&TxHash::from([1; 32])).unwrap());
    }
}
