//! The members facade (CH7 §4.2.1; EV addendum §2.6): the process-wide LLMQ state V validates
//! with, behind one serialization gate. V's attempt holds the gate exclusively from
//! [`MembersFacade::begin_attempt`] until its log is applied and it is released (routes C and D);
//! every other route takes it shared, and a caller's raw-row read takes it on the caller's own
//! thread (route A, [`MembersFacade::external`]), as does its read of the mined rows, through a
//! view of the committed evo layer it builds there ([`RawRows::with_mined`], D94).
//!
//! The gate is the node's serialization of Core's `cs_main` schedules (DEC-113/117), and has no
//! Core site. It is writer-preferring, so readers cannot starve validation: once an attempt waits,
//! a new reader waits behind it. The lock order is V's dispatch, then the gate, then one of the
//! facade's two cache mutexes (the members caches or the quorum caches, never both), then a rows
//! section. The mineable pool's mutex is a leaf, taken with or without the gate: nothing else is
//! taken while it is held (Core's `minableCommitmentsCs`). So is step 10's cell, Core's
//! `cached_mutex` (`evo/specialtxman.cpp:147`), held for one read or one write in an attempt.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use dash_chain::ChainReader;
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::{Dmn, StoreError};
use dash_evo::llmq::{MinedCommitmentReader, ParamsOutOfDomain, QuorumMembers, QuorumSnapshot};
use dash_kv::ErrorKind;
use dash_params::ChainSetup;
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::cache::PerTypeLru;
use super::members::TemplateRows;
use super::quorum::QuorumCaches;
use super::rows::{RuntimeRows, SnapshotKey};
use crate::chain::{Fatal, VContext};
use crate::connect::AttemptId;
use crate::evo::cl::CbTxClCell;
use crate::evo::effects::EffectsApplied;
use crate::store::{Failure, GlobalFailStop};

/// The facade: the members caches, the quorum caches and the runtime rows, with the gate every
/// route takes and the node latch it trips.
pub struct MembersFacade {
    gate: RwLock<()>,
    /// Core's `cs_members` and `cs_indexed_members` as one mutex (D78): every members-cache access
    /// is under the gate, and none is held across a computation.
    caches: Mutex<MembersCaches>,
    /// Core's `m_cs_maps` (D90): the quorum objects and the scans, under the gate, never held
    /// across a members call.
    quorums: Mutex<QuorumCaches>,
    /// Core's `minableCommitmentsCs` and the two maps it guards, `minableCommitmentsByQuorum` and
    /// `minableCommitments` (D14): the effect log's erases and re-adds now; the miner (MN1) and
    /// the `qfcommit` intake (CH7b-5) read and add later. A leaf lock.
    mineable: Mutex<MineablePool>,
    /// Step 10's statics, Core's `cached_pindex` and `cached_chainlock` (CH7 X5): a leaf lock.
    cbtx_cl: Mutex<CbTxClCell>,
    rows: Arc<RuntimeRows>,
    /// The committed evo layer's mined rows, which route A reads ([`Self::with_mined_views`]).
    mined: Option<Arc<dyn MinedViews>>,
    latch: GlobalFailStop,
    #[cfg(test)]
    pub(crate) hooks: FacadeHooks,
}

/// The committed evo layer's mined rows, as route A reads them on a caller's thread (D94).
pub trait MinedViews: Send + Sync {
    /// `f`, once, over one view of the committed layer with `reader`'s ancestry. The layer's
    /// mutex is held while the view clones the layer's indexes and takes the database snapshot,
    /// the order V's own views use (D22), so a frozen index leaves only once the flush that wrote
    /// it is durable (D26).
    fn with_view(
        &self,
        reader: &ChainReader,
        f: &mut dyn FnMut(&dyn MinedCommitmentReader<Error = StoreError>),
    );
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
    /// applies the log's writes to, tripping `latch`, the node's, with cold members and quorum
    /// caches for `params`' configured types.
    ///
    /// # Errors
    ///
    /// A configured type whose cache size is below one.
    pub fn new(
        params: &ChainSetup,
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
            quorums: Mutex::new(QuorumCaches::new(params)?),
            mineable: Mutex::new(MineablePool::new()),
            cbtx_cl: Mutex::new(CbTxClCell::default()),
            rows,
            mined: None,
            latch,
            #[cfg(test)]
            hooks: FacadeHooks::default(),
        })
    }

    /// The facade with `mined`, the committed evo layer's mined rows, for route A's reads.
    #[must_use]
    pub fn with_mined_views(self, mined: Arc<dyn MinedViews>) -> Self {
        Self {
            mined: Some(mined),
            ..self
        }
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

    /// The quorum caches. Only a capability's calls take them, under the gate.
    pub(super) fn quorums(&self) -> MutexGuard<'_, QuorumCaches> {
        self.quorums.lock()
    }

    /// The mineable pool, a leaf lock: its holder takes nothing else.
    pub(crate) fn mineable(&self) -> MineableGuard<'_> {
        #[cfg(any(test, feature = "test-support"))]
        MINEABLE_HELD.with(|held| held.set(held.get() + 1));
        MineableGuard(self.mineable.lock())
    }

    /// Step 10's cell, a leaf lock: its holder reads or writes it and takes nothing else.
    pub(crate) fn cbtx_cl(&self) -> &Mutex<CbTxClCell> {
        &self.cbtx_cl
    }

    /// The runtime rows, under a capability's gate.
    pub(super) fn runtime_rows(&self) -> &RuntimeRows {
        &self.rows
    }

    /// A block template's rows over the runtime rows (MN0-3 C0): no gate, no members cache, no
    /// attempt.
    pub(crate) fn template_rows(&self) -> TemplateRows<'_> {
        TemplateRows::new(&self.rows)
    }

    /// The runtime rows, for tests that read or compare them.
    #[cfg(test)]
    pub(crate) fn rows_for_test(&self) -> &RuntimeRows {
        &self.rows
    }

    /// The members cached for `base`, unstamped, for tests that compare them.
    #[cfg(test)]
    pub(crate) fn cached_members(
        &self,
        llmq_type: LlmqType,
        base: &BlockHash,
    ) -> Option<QuorumMembers<Arc<Dmn>>> {
        self.caches().by_base.peek(llmq_type, base).cloned()
    }

    /// The members caches as they stand, stamps and all, for tests that compare them.
    #[cfg(test)]
    pub(crate) fn members_caches_state(&self) -> String {
        let caches = self.caches();
        format!("{:?} {:?}", caches.by_base, caches.by_cycle_index)
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

    /// The quorum caches, for tests that inspect them; `None` while another holder has them.
    #[cfg(test)]
    pub(crate) fn test_quorums(&self) -> Option<MutexGuard<'_, QuorumCaches>> {
        self.quorums.try_lock()
    }
}

/// Route A's capability: the shared gate, held on the caller's thread.
pub struct ExternalToken<'f> {
    facade: &'f MembersFacade,
    _gate: RwLockReadGuard<'f, ()>,
}

impl<'f> ExternalToken<'f> {
    /// The runtime rows and the mined rows, for as long as the token holds the gate.
    pub fn rows(&self) -> RawRows<'_> {
        RawRows {
            rows: &self.facade.rows,
            mined: self.facade.mined.as_deref(),
        }
    }

    /// The facade whose gate the token holds.
    pub(super) fn facade(&self) -> &'f MembersFacade {
        self.facade
    }
}

/// The rows route A reads, under the token's gate: no members object and no writer.
pub struct RawRows<'t> {
    rows: &'t RuntimeRows,
    mined: Option<&'t dyn MinedViews>,
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

    /// `f` over the committed evo layer's mined rows (`GetMinedCommitment`, and the scans'
    /// `GetMinedCommitments*UntilBlock`), one view built on this thread with `reader`'s ancestry.
    /// Every getter in `f` reads one state: a reader between an attempt's release and its commit
    /// reads the parent's, as a reader holding Core's `cs_main` does (D94).
    ///
    /// The view is the layer as committed when it is built. The tip the caller reads at, pinned
    /// from `ChainView.snap`, is taken apart from it, and V commits outside the gate (D33). So at
    /// a pinned tip the layer no longer holds, as when V disconnected it and committed before it
    /// republished, the getters answer at the tip's last common ancestor with the committed layer.
    /// Core's `ScanQuorums` reads so without `cs_main`: it pins `ActiveTip` (`quorumsman.cpp:151`)
    /// and reads the current EvoDB through `GetAncestor` (`blockprocessor.cpp:687-725`). A caller
    /// that needs the answers at its own tip checks the view's evo tip against it (RP2a, QM1a).
    ///
    /// # Errors
    ///
    /// A facade built without the evo layer's mined rows has none to read.
    pub fn with_mined<T>(
        &self,
        reader: &ChainReader,
        f: impl FnOnce(&dyn MinedCommitmentReader<Error = StoreError>) -> T,
    ) -> Result<T, StoreError> {
        let Some(mined) = self.mined else {
            return Err(StoreError::NotBuilt("the mined rows of a facade without the evo layer"));
        };
        let (mut f, mut out) = (Some(f), None);
        mined.with_view(reader, &mut |view| out = f.take().map(|f| f(view)));
        let Some(out) = out else {
            unreachable!("with_view calls its f once");
        };
        Ok(out)
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

/// [`MembersFacade::mineable`]'s guard. Under tests it counts this thread's holds, so a read that
/// must not run under the leaf lock can check ([`mineable_held_here`]).
pub(crate) struct MineableGuard<'a>(MutexGuard<'a, MineablePool>);

impl std::ops::Deref for MineableGuard<'_> {
    type Target = MineablePool;
    fn deref(&self) -> &MineablePool {
        &self.0
    }
}

impl std::ops::DerefMut for MineableGuard<'_> {
    fn deref_mut(&mut self) -> &mut MineablePool {
        &mut self.0
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for MineableGuard<'_> {
    fn drop(&mut self) {
        MINEABLE_HELD.with(|held| held.set(held.get() - 1));
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static MINEABLE_HELD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether this thread holds a mineable-pool guard: a read that takes another lock checks it is
/// not under the leaf lock.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn mineable_held_here() -> bool {
    MINEABLE_HELD.with(|held| held.get() > 0)
}
