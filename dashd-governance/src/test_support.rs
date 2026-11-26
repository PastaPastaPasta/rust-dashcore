//! The test fakes `test-support`'s replay rig shares with this crate's tests: the transaction
//! index the replays fill from the blocks, and V's read-failure sink.
#![cfg_attr(not(test), allow(dead_code, reason = "this crate's tests use the rest"))]

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier, Mutex, PoisonError, mpsc};

use dash_chain::FailStop;
use dash_consensus::Tx;
use dash_governance::object;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::RecordingAbort;
use dashd_indexes::{IndexError, IndexStatus, IndexSummary, IndexedTx, TxIndexReader, Txid};

use crate::exec::fakes::current_command;

pub(crate) type Entry = Result<IndexedTx, IndexError>;
/// The txid whose lookup waits: it reports entry, then waits at the barrier for release.
type Hold = ([u8; 32], mpsc::Sender<()>, Arc<Barrier>);

/// A transaction index that records each `find_tx` with the command it ran in, and can hold
/// one lookup at a pair of barriers and run a probe at every lookup.
#[derive(Default)]
pub(crate) struct FakeIndex {
    pub(crate) entries: Mutex<BTreeMap<[u8; 32], Entry>>,
    pub(crate) reads: Mutex<Vec<([u8; 32], Option<u64>)>>,
    pub(crate) hold: Mutex<Option<Hold>>,
    pub(crate) probe: Mutex<Option<Probe>>,
}

/// Runs at each lookup, with its txid.
type Probe = Box<dyn Fn([u8; 32]) + Send>;

impl FakeIndex {
    pub(crate) fn put(&self, tx: &Tx, at: BlockHash) {
        self.set(object::txid_bytes(&tx.txid()), Ok((Arc::new(tx.raw().clone()), at)));
    }

    pub(crate) fn set(&self, key: [u8; 32], entry: Entry) {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).insert(key, entry);
    }

    pub(crate) fn reads(&self) -> Vec<([u8; 32], Option<u64>)> {
        self.reads.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub(crate) fn read_count(&self, key: &[u8; 32]) -> usize {
        self.reads().iter().filter(|(k, _)| k == key).count()
    }

    /// The next lookup of `key` reports its entry, then waits for the barrier.
    pub(crate) fn hold(&self, key: [u8; 32]) -> (mpsc::Receiver<()>, Arc<Barrier>) {
        let (entered, entered_rx) = mpsc::channel();
        let release = Arc::new(Barrier::new(2));
        *self.hold.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((key, entered, release.clone()));
        (entered_rx, release)
    }

    pub(crate) fn set_probe(&self, probe: impl Fn([u8; 32]) + Send + 'static) {
        *self.probe.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(probe));
    }
}

impl IndexStatus for FakeIndex {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "txindex",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

impl TxIndexReader for FakeIndex {
    fn find_tx(&self, txid: &Txid) -> Result<Option<IndexedTx>, IndexError> {
        let key = txid.to_lendian();
        self.reads.lock().unwrap_or_else(PoisonError::into_inner).push((key, current_command()));
        if let Some(probe) = &*self.probe.lock().unwrap_or_else(PoisonError::into_inner) {
            probe(key);
        }
        let hold = self.hold.lock().unwrap_or_else(PoisonError::into_inner).take();
        match hold {
            Some((k, entered, release)) if k == key => {
                let _ = entered.send(());
                release.wait();
            }
            other => *self.hold.lock().unwrap_or_else(PoisonError::into_inner) = other,
        }
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned().transpose()
    }

    fn has_tx(&self, txid: &Txid) -> Result<bool, IndexError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&txid.to_lendian()))
    }
}

pub(crate) struct NoReads(pub(crate) Arc<RecordingAbort>);

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        dashd_chainstate::chain::AbortSink::read_failed(&*self.0, what);
    }

    fn is_tripped(&self) -> bool {
        false
    }
}
