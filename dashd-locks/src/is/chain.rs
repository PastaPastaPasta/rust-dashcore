//! The InstantSend [`ChainHelper`] back end (PLAN-LK1b §5.3, slice S1b).
//!
//! - [`IsHelper`] decorates the node's helper. It answers `ConflictingISLockIfAny` from the IS
//!   store through the attempt's overlay, in live mode alone or, in replay mode, beside the
//!   recorded answer it must equal. It keeps a store fault of the removal for the IS loop's next
//!   query, and it opens and closes the attempt's ledger state.
//! - [`IsAttemptLedger`] is the attempt's ledger: the observations of staged removals in emission
//!   order, the applier's confirmation cursor and failed position, and the attempt's overlay.
//! - [`IsLedgerApplier`] is the [`IsLockApplier`]: it commits each removal's stage at the
//!   effect's place in the log.
//!
//! The removal itself (`RemoveConflictingISLockByTx`, Core `evo/chainhelper.cpp:93-99`,
//! `instantsend/instantsend.cpp:342-353`) is the removal bridge's, in `chain/removal.rs`.

mod removal;
#[cfg(test)]
mod removal_tests;
#[cfg(test)]
pub(super) mod test_log;

use std::sync::Arc;

use dash_consensus::Tx;
use dash_evo::EffectSink;
use dash_num::Hash256;
use dash_primitives::{BlockHash, TxOut};
use dashd_chainstate::chain::{ChainLockSig, ConnectCx, Fatal};
use dashd_chainstate::connect::{BlockAt, ChainHelper, IsLockRef};
use dashd_chainstate::evo::effects::IsLockApplier;
use parking_lot::Mutex;

use super::seams::{IsFxLedger, IsObservation, IsOverlay, IsStore, LedgerTake};
use super::{IsGate, IsLockHash, IsTipCache, LockedTx};

pub use removal::IsRemoval;

/// The attempt's effect ledger, shared by the helper, the applier and the digest's `fx` source.
///
/// Only those V-owned objects may hold it (PLAN-LK1b §5.3 G1-b): the ledger owns the attempt's
/// overlay and so its gate hold, whose drop latches the store and wakes parked callers when V
/// unwinds. Any other clone keeps the gate held past V's unwind, and every caller waits forever.
pub type SharedLedger = Arc<Mutex<dyn IsFxLedger>>;

/// Core's `IsInstantSendEnabled` inputs (SPORK_2, `-reindex`, `-loadblock`), read at each lookup.
pub type GateSource = Arc<dyn Fn() -> IsGate + Send + Sync>;

/// The start of the [`Fatal`] a replayed `is_conflict` returns when the IS store's answer is not
/// the recorded one; the transaction and both answers follow.
pub const REPLAY_DIVERGED: &str = "IS replay diverged";

/// The text of the [`Fatal`] the applier returns for a removal effect with no matching
/// observation at the ledger's cursor.
pub const UNSTAGED_REMOVAL: &str = "IS removal effect without its staged removal";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Live,
    Replaying,
}

/// The InstantSend [`ChainHelper`]: the IS store's answers around an inner helper, to which every
/// other query is delegated.
pub struct IsHelper<H> {
    inner: H,
    mode: Mode,
    store: Arc<dyn IsStore>,
    ledger: SharedLedger,
    gate: GateSource,
    tip: IsTipCache,
    /// The transaction whose `is_conflict` last found a lock, for the removal's second lookup.
    pending_tx: Option<Arc<Tx>>,
    /// A store fault of the removal, which the next `is_conflict` returns once.
    pending_fault: Option<Fatal>,
}

impl<H: ChainHelper> IsHelper<H> {
    /// The live helper around `inner` (the node's `NodeHelper`): `is_conflict` answers from
    /// `store`.
    pub fn live(
        inner: H,
        store: Arc<dyn IsStore>,
        ledger: SharedLedger,
        gate: GateSource,
        tip: IsTipCache,
    ) -> Self {
        Self::with_mode(Mode::Live, inner, store, ledger, gate, tip)
    }

    /// The replaying helper around `inner` (a `ReplayChainHelper`): `is_conflict` consumes the
    /// recorded answer and runs the live lookup too, and the two must be equal (§5.3 item 3).
    /// Valid only from the recording's initial store and cache state, with the same event
    /// schedule.
    pub fn replaying(
        inner: H,
        store: Arc<dyn IsStore>,
        ledger: SharedLedger,
        gate: GateSource,
        tip: IsTipCache,
    ) -> Self {
        Self::with_mode(Mode::Replaying, inner, store, ledger, gate, tip)
    }

    fn with_mode(
        mode: Mode,
        inner: H,
        store: Arc<dyn IsStore>,
        ledger: SharedLedger,
        gate: GateSource,
        tip: IsTipCache,
    ) -> Self {
        Self {
            inner,
            mode,
            store,
            ledger,
            gate,
            tip,
            pending_tx: None,
            pending_fault: None,
        }
    }

    /// `GetConflictingLock(tx)` through the attempt's overlay: `None` while InstantSend is
    /// disabled, else the first lock input's lock for another transaction. A store fault is a
    /// [`Fatal::new`], as Core's thrown `dbwrapper_error` is.
    // Core: instantsend/instantsend.cpp:428-445
    fn live_conflict(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>, Fatal> {
        if !(self.gate)().enabled() {
            return Ok(None);
        }
        let mut ledger = self.ledger.lock();
        self.store.conflicting_lock_in(ledger.overlay(), tx).map_err(|e| Fatal::new(e.to_string()))
    }

    fn clear_attempt(&mut self) {
        self.pending_tx = None;
        self.pending_fault = None;
    }
}

impl<H: ChainHelper> ChainHelper for IsHelper<H> {
    fn cl_conflict(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        self.inner.cl_conflict(cx, h, hash)
    }

    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal> {
        self.inner.ibd(cx)
    }

    fn cl_has(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        self.inner.cl_has(cx, h, hash)
    }

    fn cl_best(&mut self, cx: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
        self.inner.cl_best(cx)
    }

    /// `ConflictingISLockIfAny(tx)`: a kept removal fault first, once; else the store's answer,
    /// which a replay checks against the recorded one.
    // Core: evo/chainhelper.cpp:79-85
    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        self.pending_tx = None;
        if let Some(fault) = self.pending_fault.take() {
            return Err(fault);
        }
        let recorded = match self.mode {
            Mode::Live => None,
            Mode::Replaying => Some(self.inner.is_conflict(tx)?),
        };
        let live = self.live_conflict(tx)?.map(|(hash, lock)| IsLockRef {
            hash: Hash256::from(hash),
            txid: lock.txid(),
        });
        if let Some(recorded) = recorded
            && recorded != live
        {
            let txid = tx.txid();
            return Err(Fatal::new(format!(
                "{REPLAY_DIVERGED}: tx {txid} recorded {recorded:?}, store {live:?}"
            )));
        }
        self.pending_tx = live.map(|_| Arc::new(Tx::new(tx.raw().clone())));
        Ok(live)
    }

    /// `RemoveConflictingISLockByTx(tx)` for the pending transaction: the removal bridge's. A
    /// replay runs the live removal and never the inner helper's, which only emits.
    fn remove_conflicting_is_lock(&mut self, _lock: &IsLockRef, sink: &mut dyn EffectSink) {
        self.remove_conflicting(sink);
    }

    /// Clears what an earlier attempt left: its pending transaction and fault, and the ledger's
    /// observations, cursor and failure when no digest record took them (§5.3 item 5).
    fn begin_attempt(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<(), Fatal> {
        self.clear_attempt();
        self.ledger.lock().clear();
        self.inner.begin_attempt(cx, at)
    }

    /// After `finish_attempt`: a kept fault must have been surfaced; the pending state is
    /// cleared, and any stage the applier never reached is discarded. Observations stay for the
    /// digest's `fx` source.
    fn end_attempt(&mut self) {
        debug_assert!(self.pending_fault.is_none(), "IS fault not surfaced");
        self.clear_attempt();
        {
            let mut ledger = self.ledger.lock();
            let overlay = ledger.overlay();
            debug_assert!(overlay.gate_held_only_with_stages(), "IS gate held without a stage");
            self.store.discard_staged(overlay);
        }
        self.inner.end_attempt();
    }

    fn sb_required(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        self.inner.sb_required(cx, at)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        self.inner.gov_valid()
    }

    fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal> {
        self.inner.sb_triggered(h)
    }

    fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        self.inner.sb_payments(h)
    }

    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        self.inner.interim_refusal(cx, h)
    }
}

/// The attempt's ledger (§5.3 item 5): immutable observations in emission order, the applier's
/// confirmation cursor and failed position, and the overlay the attempt's stages live in.
#[derive(Debug, Default)]
pub struct IsAttemptLedger {
    observations: Vec<IsObservation>,
    cursor: usize,
    failed_at: Option<usize>,
    overlay: IsOverlay,
}

impl IsAttemptLedger {
    /// An empty ledger, shareable by the helper, the applier and the `fx` source.
    pub fn shared() -> SharedLedger {
        Arc::new(Mutex::new(IsAttemptLedger::default()))
    }

    fn fail(&mut self, fault: Fatal) -> Fatal {
        self.failed_at = Some(self.cursor);
        fault
    }
}

impl IsFxLedger for IsAttemptLedger {
    fn push(&mut self, observation: IsObservation) {
        self.observations.push(observation);
    }

    /// The observation at the cursor must name `hash`; its stage is committed, and only then
    /// does the cursor advance. Any failure records the position and is a [`Fatal`].
    fn confirm_next(&mut self, hash: &IsLockHash, store: &dyn IsStore) -> Result<(), Fatal> {
        let stage = match self.observations.get(self.cursor) {
            Some(observation) if observation.lock_hash == *hash => observation.stage,
            _ => return Err(self.fail(Fatal::new(UNSTAGED_REMOVAL))),
        };
        match store.commit_staged(&mut self.overlay, stage) {
            Ok(()) => {
                self.cursor += 1;
                Ok(())
            }
            Err(e) => Err(self.fail(Fatal::new(e.to_string()))),
        }
    }

    fn overlay(&mut self) -> &mut IsOverlay {
        &mut self.overlay
    }

    /// The observations, the hashes the cursor confirmed and the failed position; the ledger is
    /// empty after it. The overlay is the store's to discard.
    fn take(&mut self) -> LedgerTake {
        let observations = std::mem::take(&mut self.observations);
        let confirmed = observations[..self.cursor].iter().map(|o| o.lock_hash).collect();
        let failed_at = self.failed_at.take();
        self.cursor = 0;
        LedgerTake {
            observations,
            confirmed,
            failed_at,
        }
    }

    fn clear(&mut self) {
        self.observations.clear();
        self.cursor = 0;
        self.failed_at = None;
    }
}

/// The IS effect's applier (§5.3 item 4): each `RemoveConflictingIsLock` commits the stage its
/// observation names, at its place in the effect log. A log that carries the effect without a
/// staged removal (no observation at the cursor, or another hash) is a [`Fatal`].
pub struct IsLedgerApplier {
    store: Arc<dyn IsStore>,
    ledger: SharedLedger,
}

impl IsLedgerApplier {
    /// The applier over the store and ledger the attempt's [`IsHelper`] uses.
    pub fn new(store: Arc<dyn IsStore>, ledger: SharedLedger) -> Self {
        Self {
            store,
            ledger,
        }
    }
}

impl IsLockApplier for IsLedgerApplier {
    fn remove(&mut self, lock_hash: &Hash256) -> Result<(), Fatal> {
        self.ledger.lock().confirm_next(lock_hash.as_bytes(), &*self.store)
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "chain_tests.rs"]
mod tests;
