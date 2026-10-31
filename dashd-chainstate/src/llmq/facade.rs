//! The members facade (CH7 §4.2.1; EV addendum §2.6): the process-wide LLMQ state V validates
//! with, behind one serialization gate. V's attempt holds the gate exclusively from
//! [`MembersFacade::begin_attempt`] until its log is applied and it is released (routes C and D);
//! every other route takes it shared, and a caller's raw-row read takes it on the caller's own
//! thread (route A, [`MembersFacade::external`]).
//!
//! The gate is the node's serialization of Core's `cs_main` schedules (DEC-113/117), and has no
//! Core site. It is writer-preferring, so readers cannot starve validation: once an attempt waits,
//! a new reader waits behind it. The lock order is V's dispatch, then the gate, then the facade's
//! cache mutex, then a rows section.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use dash_dml::{Dmn, StoreError};
use dash_evo::llmq::{ParamsOutOfDomain, QuorumMembers, QuorumSnapshot};
use dash_kv::ErrorKind;
use dash_params::ChainParams;
use dash_primitives::{BlockHash, LlmqType};
use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::cache::PerTypeLru;
use super::rows::{RuntimeRows, SnapshotKey};
use crate::chain::{Fatal, VContext};
use crate::connect::AttemptId;
use crate::evo::effects::EffectsApplied;
use crate::store::{Failure, GlobalFailStop};

/// The facade: the members caches and the runtime rows, with the gate every route takes and the
/// node latch it trips.
pub struct MembersFacade {
    gate: RwLock<()>,
    /// Core's `cs_members` and `cs_indexed_members` as one mutex (D78): every members-cache access
    /// is under the gate, and none is held across a computation.
    caches: Mutex<MembersCaches>,
    rows: Arc<RuntimeRows>,
    latch: GlobalFailStop,
    #[cfg(test)]
    pub(crate) hooks: FacadeHooks,
}

/// `GetAllQuorumMembers`' two static caches (`llmq/utils.cpp:636-639`), each a
/// `PerLlmqTypeCache` sized `keepOldConnections`, built with the facade where Core builds them on
/// first use (D79).
pub(crate) struct MembersCaches {
    /// `mapQuorumMembers`: a base's members.
    pub(crate) by_base: PerTypeLru<BlockHash, QuorumMembers<Arc<Dmn>>>,
    /// `mapIndexedQuorumMembers`: a rotated cycle's quorums, by cycle base and index.
    pub(crate) by_cycle_index: PerTypeLru<(BlockHash, u16), QuorumMembers<Arc<Dmn>>>,
}

/// An attempt's rows: its snapshot-row stores, last write wins (EV addendum §2.6).
pub(crate) type Overlay = RefCell<BTreeMap<SnapshotKey, QuorumSnapshot>>;

impl MembersFacade {
    /// The facade over `rows`, the rows [`StoreSnapshots`](crate::evo::effects::StoreSnapshots)
    /// applies the log's writes to, tripping `latch`, the node's, with cold members caches for
    /// `params`' configured types.
    ///
    /// # Errors
    ///
    /// A configured type whose cache size is below one.
    pub fn new(
        params: &ChainParams,
        rows: Arc<RuntimeRows>,
        latch: GlobalFailStop,
    ) -> Result<Self, ParamsOutOfDomain> {
        let caches = MembersCaches {
            by_base: PerTypeLru::new(params, true)?,
            by_cycle_index: PerTypeLru::new(params, true)?,
        };
        Ok(Self {
            gate: RwLock::new(()),
            caches: Mutex::new(caches),
            rows,
            latch,
            #[cfg(test)]
            hooks: FacadeHooks::default(),
        })
    }

    /// Route A: the gate, shared, on the caller's thread, for as long as the token lives. A
    /// caller that waited on an attempt that failed wakes to the latch's failure and reads no row
    /// (D74).
    ///
    /// # Errors
    ///
    /// The latched failure, reported already by whoever tripped the latch.
    pub fn external(&self) -> Result<ExternalToken<'_>, Fatal> {
        let gate = self.shared_gate();
        if let Some(failure) = self.latch.tripped() {
            return Err(Fatal::reported(failure.message.clone()));
        }
        Ok(ExternalToken {
            facade: self,
            _gate: gate,
        })
    }

    /// Opens V's attempt `id`: the gate, exclusively, until [`Self::release_attempt`]. It takes
    /// V's context, so only V opens one, one at a time; the connector supplies the id.
    pub fn begin_attempt<'v>(&'v self, _v: &'v mut VContext, id: AttemptId) -> AttemptHandle<'v> {
        AttemptHandle {
            facade: self,
            id,
            _gate: self.exclusive_gate(),
            overlay: Overlay::default(),
            released: false,
            _v_only: PhantomData,
        }
    }

    /// Releases the attempt whose whole log `applied` proves applied, and so the gate. A handle
    /// from another facade, or the proof of another attempt, trips the latch, and the gate is
    /// released after the trip (D75).
    ///
    /// # Errors
    ///
    /// The mismatch, an `AbortNode` V reports.
    pub fn release_attempt(
        &self,
        mut handle: AttemptHandle<'_>,
        applied: EffectsApplied,
    ) -> Result<(), Fatal> {
        let message = if !std::ptr::eq(handle.facade, self) {
            "an EV attempt released through another facade".to_owned()
        } else if applied.attempt() != handle.id {
            format!(
                "EV attempt {} released with attempt {}'s effects",
                handle.id.get(),
                applied.attempt().get()
            )
        } else {
            handle.released = true;
            return Ok(());
        };
        self.trip(message.clone());
        Err(Fatal::abort_node(message))
    }

    /// Trips the node latch with `message`, unless a failure is latched already (D4); whether
    /// this call latched it.
    pub(super) fn trip(&self, message: String) -> bool {
        self.latch.trip(Failure {
            kind: ErrorKind::Other,
            site: "chainstate validation",
            message,
        })
    }

    /// The members caches. Only a capability's calls take them, under the gate.
    pub(super) fn caches(&self) -> MutexGuard<'_, MembersCaches> {
        self.caches.lock()
    }

    /// The runtime rows, under a capability's gate.
    pub(super) fn runtime_rows(&self) -> &RuntimeRows {
        &self.rows
    }

    #[cfg(not(test))]
    fn shared_gate(&self) -> RwLockReadGuard<'_, ()> {
        self.gate.read()
    }

    #[cfg(not(test))]
    fn exclusive_gate(&self) -> RwLockWriteGuard<'_, ()> {
        self.gate.write()
    }

    #[cfg(test)]
    fn shared_gate(&self) -> RwLockReadGuard<'_, ()> {
        self.hooks.counted(|| self.gate.try_read(), || self.gate.read())
    }

    #[cfg(test)]
    fn exclusive_gate(&self) -> RwLockWriteGuard<'_, ()> {
        self.hooks.counted(|| self.gate.try_write(), || self.gate.write())
    }

    /// The gate, for tests that observe who holds it.
    #[cfg(test)]
    pub(crate) fn gate(&self) -> &RwLock<()> {
        &self.gate
    }

    /// The members caches, for tests that seed and inspect them.
    #[cfg(test)]
    pub(crate) fn test_caches(&self) -> MutexGuard<'_, MembersCaches> {
        self.caches()
    }
}

/// Route A's capability: the shared gate, held on the caller's thread.
pub struct ExternalToken<'f> {
    facade: &'f MembersFacade,
    _gate: RwLockReadGuard<'f, ()>,
}

impl<'f> ExternalToken<'f> {
    /// The runtime rows, for as long as the token holds the gate.
    pub fn rows(&self) -> RawRows<'_> {
        RawRows {
            rows: &self.facade.rows,
        }
    }

    /// The facade whose gate the token holds.
    pub(super) fn facade(&self) -> &'f MembersFacade {
        self.facade
    }
}

/// The runtime rows route A reads, under the token's gate: no members object and no writer.
pub struct RawRows<'t> {
    rows: &'t RuntimeRows,
}

impl RawRows<'_> {
    /// [`RuntimeRows::snapshot_for_block`].
    ///
    /// # Errors
    ///
    /// The read's fault; nothing is inserted.
    pub fn snapshot_for_block(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        self.rows.snapshot_for_block(llmq_type, cycle_base)
    }
}

/// V's open attempt: the gate, held exclusively, on V's thread (it is neither `Send` nor `Sync`).
/// Dropped without [`MembersFacade::release_attempt`], as an unwind drops it, it trips the latch
/// before it releases the gate, so a reader waiting on the gate wakes to the failure (EV addendum
/// §2.6).
pub struct AttemptHandle<'v> {
    facade: &'v MembersFacade,
    id: AttemptId,
    _gate: RwLockWriteGuard<'v, ()>,
    /// One overlay for every call of the attempt (EV addendum §2.6 case 1).
    overlay: Overlay,
    released: bool,
    _v_only: PhantomData<*const ()>,
}

impl<'v> AttemptHandle<'v> {
    /// The attempt the handle was opened for.
    pub fn id(&self) -> AttemptId {
        self.id
    }

    /// The facade the handle holds the gate of.
    pub(super) fn facade(&self) -> &'v MembersFacade {
        self.facade
    }

    /// The attempt's overlay.
    pub(super) fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// Ends the attempt whose log failed to apply (D4): the latch is tripped with `failure`,
    /// unless a failure is latched already, then (in tests) [`FacadeHooks::after_trip`] runs and
    /// the gate is released, so a reader waiting on the gate wakes to the failure. The trip
    /// reports nothing, so a failure it latched is V's to report whatever the latch then shows.
    pub(crate) fn fail(mut self, failure: Fatal) -> Fatal {
        self.released = true;
        let latched = self.facade.trip(failure.0.clone());
        #[cfg(test)]
        self.facade.hooks.after_trip();
        if latched {
            failure.latched_unreported()
        } else {
            failure
        }
    }
}

impl Drop for AttemptHandle<'_> {
    /// The trip, then (in tests) [`FacadeHooks::after_trip`], then the gate's release, as the
    /// guard drops after this body.
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.facade.trip("an EV attempt dropped without its release".to_owned());
        #[cfg(test)]
        self.facade.hooks.after_trip();
    }
}

/// The facade's test points (EV addendum §2.6 cases; F3's sync point).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FacadeHooks {
    /// Threads blocked on the gate now, shared or exclusive.
    pub(crate) gate_waiters: std::sync::atomic::AtomicUsize,
    /// Runs once in an unreleased handle's drop, after the latch trip, with the gate still held.
    pub(crate) after_trip: parking_lot::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Runs once in a route-B row store, before its raw write: C9's 6-D (b), C10's step 1.
    pub(crate) before_inline_write: parking_lot::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// The `OnV` commands V has taken from its lanes: with the lane's depth, C9's queue probe.
    pub(crate) on_v_started: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FacadeHooks {
    fn after_trip(&self) {
        let hook = self.after_trip.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn before_inline_write(&self) {
        let hook = self.before_inline_write.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn counted<G>(&self, try_lock: impl FnOnce() -> Option<G>, lock: impl FnOnce() -> G) -> G {
        use std::sync::atomic::Ordering::SeqCst;
        if let Some(guard) = try_lock() {
            return guard;
        }
        self.gate_waiters.fetch_add(1, SeqCst);
        let guard = lock();
        self.gate_waiters.fetch_sub(1, SeqCst);
        guard
    }
}
