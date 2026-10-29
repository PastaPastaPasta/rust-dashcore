//! SB5's half of the holds (SB5 §3.3, §4.7, §4.8): the writer's drains, through the held guard
//! inside a [`Hold`], through the composite state inside the node's `enforce_block` (SB4
//! D-C6-1), and through one critical section of the tip lock otherwise; a hold's per-step
//! fanout; its release wakes.

use std::ops::{Deref, DerefMut};

use parking_lot::{ArcMutexGuard, RawMutex};

use crate::chain::{HeaderChain, Hold};
use crate::error::ChainError;
use crate::feed::{self, FeedShared, Publish};
use crate::residency::Residency;
use crate::store::HeaderStore;
use crate::tip::{TipState, TipStep};

/// The owned guard of the tip mutex in a [`Hold`] and in `enforce_block`'s composite, which their
/// `Drop` releases before it fires the release wakes (§4.8 step 4). It derefs to the guard itself.
pub(crate) struct HoldGuard(Option<ArcMutexGuard<RawMutex, TipState>>);

impl HoldGuard {
    pub(crate) fn new(guard: ArcMutexGuard<RawMutex, TipState>) -> HoldGuard {
        HoldGuard(Some(guard))
    }

    /// Drops the guard: the tip mutex is free when this returns. Only a hold's end calls it.
    pub(crate) fn release(&mut self) {
        self.0 = None;
    }
}

impl Deref for HoldGuard {
    type Target = ArcMutexGuard<RawMutex, TipState>;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("held until the hold ends")
    }
}

impl DerefMut for HoldGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("held until the hold ends")
    }
}

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// The feeds' state reachable without the tip lock.
    pub(crate) fn feed_shared(&self) -> &FeedShared {
        &self.shared.readers.tip().shared
    }

    /// Pushes what fits from the pending backlog, per target from its cursor, in `seq` order.
    /// Inside `enforce_block` it runs on the walk's state, whose mutex the walk already holds;
    /// otherwise it is one critical section of the tip lock. Inside a hold the writer calls
    /// [`Hold::retry_pending`]; through [`Hold::chain`] this panics rather than deadlock.
    pub fn retry_pending(&mut self) -> Publish {
        let cell = self.shared.readers.tip();
        match &mut self.composite {
            Some(st) => st.feeds.retry_pending(&cell.shared),
            None => {
                assert!(!self.held, "inside a hold, call Hold::retry_pending");
                cell.state.lock().feeds.retry_pending(&cell.shared)
            }
        }
    }

    /// No live target lacks events: the writer may take its next tip step (§3.3). Inside a hold,
    /// [`Hold::pending_is_empty`]; through [`Hold::chain`] this panics rather than deadlock.
    pub fn pending_is_empty(&self) -> bool {
        match &self.composite {
            Some(st) => st.feeds.pending_is_empty(),
            None => {
                assert!(!self.held, "inside a hold, call Hold::pending_is_empty");
                self.shared.readers.tip().state.lock().feeds.pending_is_empty()
            }
        }
    }
}

impl<S: HeaderStore, R: Residency> Hold<'_, S, R> {
    /// The writer's only route to `TipState` while the hold is open: the held guard. Re-locking
    /// would deadlock (parking_lot's mutex is not reentrant).
    pub(super) fn with_tip<T>(&mut self, f: impl FnOnce(&mut TipState, &FeedShared) -> T) -> T {
        f(&mut self.guard, self.chain.feed_shared())
    }

    /// [`HeaderChain::retry_pending`] on the held guard: V's drain between two steps of a hold,
    /// and the SPV writer's between two sub-steps (§4.7, §4.8).
    pub fn retry_pending(&mut self) -> Publish {
        self.with_tip(|st, shared| st.feeds.retry_pending(shared))
    }

    /// [`HeaderChain::pending_is_empty`] on the held guard.
    pub fn pending_is_empty(&self) -> bool {
        self.guard.feeds.pending_is_empty()
    }

    /// A node step on the held guard, then its fanout: inside a hold each step's event goes out
    /// in the step, while snapshot readers keep the pre-hold state (§4.7).
    pub(crate) fn fanned(
        &mut self,
        step: impl FnOnce(&mut HeaderChain<S, R>, &mut TipState) -> Result<TipStep, ChainError>,
    ) -> Result<TipStep, ChainError> {
        let step = step(self.chain, &mut self.guard)?;
        feed::after_step(&mut self.guard, self.chain.shared.readers.tip(), step);
        Ok(step)
    }

    /// The hold's end, from its `Drop`: the owner cleared, the guard dropped, then the release
    /// wakes, so a woken `register` finds the tip mutex free (§4.8 step 4).
    pub(crate) fn end(&mut self) {
        self.chain.feed_shared().hold_closing();
        self.guard.release();
        self.chain.feed_shared().fire_release_wakes();
    }
}
