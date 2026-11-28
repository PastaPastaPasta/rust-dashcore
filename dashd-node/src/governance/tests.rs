//! GV1b-S5a's node wiring: the three load cases, the disabled node, the shutdown store and the
//! order of the binding, the registrations and the init callback.

use std::sync::{Mutex, PoisonError};

/// A wiring step, in the order it happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// V was bound to the executor.
    Bound,
    /// The listener registered; whether SP1's mnsync listener and the DSTX listener already
    /// were.
    Registered {
        after_sync: bool,
        after_dstx: bool,
    },
    /// `LoadCache(true)` read the store, after its effects reached the worker.
    Loaded,
    /// A command's effects reached the worker.
    Effects,
}

#[derive(Default)]
pub(crate) struct Log(Mutex<Vec<Event>>);

impl Log {
    pub(crate) fn note(&self, event: Event) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(event);
    }

    pub(crate) fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The late index refuses a read before step 7 binds it (fatal in debug builds), misses every
/// collateral when `-txindex=0` binds none, and once bound answers through the node's index.
#[test]
fn the_late_index_refuses_an_unbound_read_then_reads_the_node_s() {
    use std::sync::Arc;

    use dashd_indexes::testing::MemTxIndex;
    use dashd_indexes::{BlockHash, TxIndexReader, Txid};

    use super::LateIndex;

    let txid = Txid::from_hex(&"07".repeat(32)).unwrap();
    let tx = dash_primitives::Transaction::new(
        1,
        dash_primitives::TxType::new(0),
        vec![],
        vec![],
        0,
        vec![],
    );
    let mut mem = MemTxIndex::default();
    mem.entries.insert(txid, (Arc::new(tx), BlockHash::from_hex(&"08".repeat(32)).unwrap()));
    let mem: Arc<dyn TxIndexReader> = Arc::new(mem);

    let unbound = LateIndex::default();
    for read in [
        Box::new(|| drop(unbound.find_tx(&txid))) as Box<dyn Fn()>,
        Box::new(|| drop(unbound.has_tx(&txid))),
    ] {
        let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read));
        assert_eq!(fault.is_err(), cfg!(debug_assertions), "an unbound read");
    }
    let none = LateIndex::default();
    assert!(none.0.set(None).is_ok());
    assert!(!none.has_tx(&txid).unwrap() && none.find_tx(&txid).unwrap().is_none());
    let bound = LateIndex::default();
    assert!(bound.0.set(Some(mem)).is_ok());
    assert!(bound.has_tx(&txid).unwrap());
    assert!(bound.find_tx(&txid).unwrap().is_some());
}
