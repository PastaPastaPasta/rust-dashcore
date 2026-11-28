//! Mempool event staging; signal producers and dispatch belong with the pool implementation.
//!
//! The pool records each removal at `removeUnchecked` (`txmempool.cpp:710-717` at v24.0.0). These
//! functions move the records into V's ordered stream while the pool lock is still held: into a
//! hold's [`Staged`] list, or, off V, straight into the signals queue.

use dash_consensus::TxRef;
use dashd_chainstate::chain::{Queued, RemovalReason, Staged, ValidationSignals};

use crate::accept::AcceptSignals;
use crate::pool::{PendingEvent, PoolState};
use crate::types::MemPoolRemovalReason;

/// The chainstate name of a removal reason.
fn removal_reason(reason: MemPoolRemovalReason) -> RemovalReason {
    match reason {
        MemPoolRemovalReason::Expiry => RemovalReason::Expiry,
        MemPoolRemovalReason::SizeLimit => RemovalReason::SizeLimit,
        MemPoolRemovalReason::Reorg => RemovalReason::Reorg,
        MemPoolRemovalReason::Block => RemovalReason::Block,
        MemPoolRemovalReason::Conflict => RemovalReason::Conflict,
        MemPoolRemovalReason::Manual => RemovalReason::Manual,
    }
}

/// The pool's name of a reason V passes.
pub(crate) fn pool_reason(reason: RemovalReason) -> MemPoolRemovalReason {
    match reason {
        RemovalReason::Expiry => MemPoolRemovalReason::Expiry,
        RemovalReason::SizeLimit => MemPoolRemovalReason::SizeLimit,
        RemovalReason::Reorg => MemPoolRemovalReason::Reorg,
        RemovalReason::Block => MemPoolRemovalReason::Block,
        RemovalReason::Conflict => MemPoolRemovalReason::Conflict,
        RemovalReason::Manual => MemPoolRemovalReason::Manual,
    }
}

/// The validation signal of one pool record.
pub(crate) fn queued(event: PendingEvent) -> Queued {
    match event {
        PendingEvent::TransactionRemovedFromMempool {
            tx,
            reason,
            sequence,
        } => Queued::TransactionRemovedFromMempool {
            tx,
            reason: removal_reason(reason),
            sequence,
        },
    }
}

/// Moves the pool's records into a hold's stream, in the order they occurred.
pub(crate) fn stage(state: &mut PoolState, staged: &mut Staged) {
    for event in state.take_pending_events() {
        staged.push(queued(event));
    }
}

/// Queues the pool's records for an off-V operation, under the caller's write guard.
pub(crate) fn enqueue(state: &mut PoolState, signals: &ValidationSignals) {
    for event in state.take_pending_events() {
        signals.enqueue(queued(event));
    }
}

/// A reorg drain's events in the order they occur, held until its outcome is known: moved into
/// the hold's stream when it succeeds; queued straight to the pool's queue when it faults, since
/// V then discards the hold's stream while the drain's mutations stay applied.
#[derive(Default)]
pub(crate) struct Collect(Vec<Queued>);

impl AcceptSignals for Collect {
    // Core: txmempool.cpp:710-717 at v24.0.0
    fn removed(&mut self, event: PendingEvent) {
        self.0.push(queued(event));
    }

    // Core: validation.cpp:1455 at v24.0.0
    fn added(&mut self, tx: TxRef, accept_time: i64, sequence: u64) {
        self.0.push(Queued::TransactionAddedToMempool {
            tx,
            accept_time,
            sequence,
        });
    }

    // Core: validation.cpp:902-907 at v24.0.0
    fn double_spend_attempt(&mut self, tx: TxRef, conflict: TxRef) {
        self.0.push(Queued::NotifyInstantSendDoubleSpendAttempt {
            tx,
            conflict,
        });
    }

    // Core: validation.cpp:3124-3127 at v24.0.2
    fn flushed(&mut self, event: Queued) {
        self.0.push(event);
    }
}

impl Collect {
    /// Into `staged` when `ok`, else onto `signals`.
    pub(crate) fn release(self, ok: bool, staged: &mut Staged, signals: &ValidationSignals) {
        for event in self.0 {
            if ok {
                staged.push(event);
            } else {
                signals.enqueue(event);
            }
        }
    }
}
