//! Scripted CoinJoin and external transaction-intake effects.

use std::collections::VecDeque;
use std::sync::Arc;

use dash_p2p::PeerId;
use dash_p2p_core::DsTx;
use dash_primitives::TxHash;
use parking_lot::Mutex;

use crate::processing::seams::{CoinJoinObjects, DstxCheck, TxIntakeHooks};

#[derive(Default)]
struct CoinJoinState {
    replies: VecDeque<DstxCheck>,
    checked: Vec<(DsTx, PeerId)>,
    added: Vec<DsTx>,
}

/// A CoinJoin adapter with scripted validation decisions and recorded calls.
#[derive(Clone, Default)]
pub struct SimCoinJoinObjects(Arc<Mutex<CoinJoinState>>);

impl SimCoinJoinObjects {
    /// Queue a validation outcome; an unscripted call stops processing.
    pub fn push_result(&self, result: DstxCheck) {
        self.0.lock().replies.push_back(result);
    }
    /// Wrappers checked, including their senders, in call order.
    pub fn checked(&self) -> Vec<(DsTx, PeerId)> {
        self.0.lock().checked.clone()
    }
    /// Wrappers stored after admission, in call order.
    pub fn added(&self) -> Vec<DsTx> {
        self.0.lock().added.clone()
    }
}

impl CoinJoinObjects for SimCoinJoinObjects {
    fn enabled(&self) -> bool {
        true
    }

    fn validate_dstx(&self, dstx: &DsTx, from: PeerId) -> DstxCheck {
        let mut state = self.0.lock();
        state.checked.push((dstx.clone(), from));
        state.replies.pop_front().unwrap_or(DstxCheck::Stop)
    }
    fn add_dstx(&self, dstx: DsTx) {
        self.0.lock().added.push(dstx);
    }

    fn dstx_by_hash(&self, _: &TxHash) -> Option<Vec<u8>> {
        None
    }

    fn queue_by_hash(&self, _: &[u8; 32]) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Default)]
struct IntakeState {
    priorities: Vec<(TxHash, i64)>,
}

/// Recorded fee-priority effects.
#[derive(Clone, Default)]
pub struct SimTxIntakeHooks(Arc<Mutex<IntakeState>>);

impl SimTxIntakeHooks {
    /// Applied fee deltas in call order.
    pub fn priorities(&self) -> Vec<(TxHash, i64)> {
        self.0.lock().priorities.clone()
    }
}

impl TxIntakeHooks for SimTxIntakeHooks {
    fn prioritize_transaction(&self, txid: &TxHash, delta: i64) {
        self.0.lock().priorities.push((*txid, delta));
    }
}
