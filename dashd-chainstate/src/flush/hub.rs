//! The subscribers' side of a flush (CH2 §2.9, §4.2 steps 0, 3b, 5–7, §4.4.1; E-b1.5).
//!
//! The hub wraps SB5's feed table (a [`FeedTable`]: SB5's `ChainFeeds` in the node):
//! `register(Needs)`, `deregister` and the out-of-band `send_control` are SB5's, and the hub adds what W and the drivers share, in one
//! leaf mutex: each subscriber's name, class, durable locator and pending commit, the last
//! commit's mirror (`FlushCommit` and the set it targets), the rewind gate, the flush's commit
//! bound (D33), and the `FlushDue` post. W writes the
//! mirror and sends the controls itself (H2, D2 of `CH2b-b3-notes.md`); a control's payload only
//! names the flush, and the driver re-reads the mirror (§4.4.1 rule 1).
//!
//! Lock order: the hub lock is a leaf. No [`FeedTable`] call is made under it, since SB5's own
//! leaves (slot, registry) never nest (SB5 §4.2), and nobody waits holding it (H1). The
//! registration window's lock (D35) comes first: a registration takes the hub lock and SB5's
//! `register` (its tip lock, by `try_lock`, waiting out only an ordinary step section) under it,
//! and nobody takes it holding either, so W's step 3b may wait behind such a section.
//!
//! A hub [`new`](SubscriberHub::new) has no feeds (E-b1.6): step 5 records `T_ack`, and no flush
//! waits on a subscriber or opens a gate.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Wake, Waker};
use std::thread::{self, ThreadId};
use std::time::Duration;

use dash_chain::{BlockRef, ControlKind, ControlMsg, IndexAccess, Needs, Notify, Refused};

use crate::planted::planted;
use crate::store::{Fatal, GlobalFailStop, Parker};
use crate::subscriber::{FeedTable, SubscriberId};

/// The control kinds (SB5-C1, DEC-155 (1)): one per CH2 `Control` variant.
pub const FLUSH_COMMIT: ControlKind = ControlKind::new(0);
pub const REWIND_REQUEST: ControlKind = ControlKind::new(1);
pub const CHAIN_STATE_FLUSHED: ControlKind = ControlKind::new(2);

/// The commit target subscribers may make durable (§4.4.1): the last flush's `new`, and the `seq`
/// of the tip step that made it V's tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitTarget {
    pub tip: BlockRef,
    pub tip_seq: u64,
}

/// Step 5's `FlushCommit { flush, new, tip_seq }` as a target reads it from the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlushCommit {
    pub flush: u64,
    pub new: BlockRef,
    pub tip_seq: u64,
}

/// Step 3b's `RewindRequest { flush, to }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RewindRequest {
    pub flush: u64,
    pub to: BlockRef,
}

/// What is outstanding for one subscriber: a commit it has not acked, a gate it has not reported
/// rewound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outstanding {
    pub commit: Option<FlushCommit>,
    pub rewind: Option<RewindRequest>,
}

/// `ChainStateFlushed { flush, new, tip_seq }`'s payload: Core's locator tip, and the `seq` of
/// the tip step that made it V's tip, so a subscriber services it after the data queued before
/// it (Core's one validation queue; D38).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flushed {
    pub flush: u64,
    pub new: BlockRef,
    pub tip_seq: u64,
}

/// What a subscriber registers with (§2.9's `register`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration {
    pub name: &'static str,
    pub needs: Needs,
    /// Awaited by step 6 and the gate, and counted in the prune locks.
    pub mandatory: bool,
    /// The lock without a locator.
    pub start_height: u32,
    /// The locator read from its store at open (recovery step 6).
    pub durable: Option<BlockRef>,
}

/// A registered subscriber's handle: its id, and Core's `height_first` for the hub's emulation
/// of the prune-lock moveback (log-only).
#[derive(Clone, Debug)]
pub struct SubscriberHandle {
    pub id: SubscriberId,
    core_lock: Arc<AtomicU32>,
}

impl SubscriberHandle {
    /// MPIX-11's `best_block()` moved: Core's `SetBestBlockIndex` sets the lock at its height
    /// (`index/base.cpp`'s `UpdatePruneLock`).
    pub fn set_best_height(&self, h: u32) {
        self.core_lock.store(h, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct Sub {
    id: SubscriberId,
    name: &'static str,
    mandatory: bool,
    start_height: u32,
    durable: Option<BlockRef>,
    /// A commit between [`begin_commit`](SubscriberHub::begin_commit) and its return: the gate
    /// counts it as durable (D33).
    pending: Option<BlockRef>,
    /// Core's `height_first`: the driver's best height, moved back by `DisconnectTip`.
    core_lock: Arc<AtomicU32>,
    /// The thread its driver last stepped on (D39).
    stepper: Option<ThreadId>,
    /// The thread that registered it, until its driver steps elsewhere: a flush that waits on
    /// it waits on that thread (D47).
    owner: Option<ThreadId>,
}

#[derive(Debug)]
struct CommitMirror {
    commit: FlushCommit,
    to: Vec<SubscriberId>,
    acked: Vec<SubscriberId>,
}

#[derive(Debug)]
struct Gate {
    req: RewindRequest,
    targets: Vec<SubscriberId>,
    rewound: Vec<SubscriberId>,
}

/// D35's registration window, and who must not wait on it (D43).
#[derive(Debug, Default)]
struct Window {
    /// The flush between step 3b and its release, and W's thread, which steps it.
    open: Option<(u64, ThreadId)>,
    /// That flush's step 7 waits on V's `PruneClears` answer.
    awaits_v: bool,
    /// V's thread: the last to submit a flush, which answers its `PruneClears`.
    v: Option<ThreadId>,
}

#[derive(Debug, Default)]
struct HubState {
    next_id: u32,
    t_ack: Option<CommitTarget>,
    /// Step 3b's `LCA(T_ack, new)` until step 5 (D33): the commit cap while a flush is in
    /// flight, gate or not.
    bound: Option<BlockRef>,
    subs: Vec<Sub>,
    commit: Option<CommitMirror>,
    gate: Option<Gate>,
    flush_due: bool,
}

impl HubState {
    fn sub(&self, id: SubscriberId) -> Option<&Sub> {
        self.subs.iter().find(|s| s.id == id)
    }

    fn sub_mut(&mut self, id: SubscriberId) -> Option<&mut Sub> {
        self.subs.iter_mut().find(|s| s.id == id)
    }

    fn mandatory(&self, id: SubscriberId) -> bool {
        self.sub(id).is_some_and(|s| s.mandatory)
    }

    fn cap(&self) -> Option<BlockRef> {
        self.bound.or(self.t_ack.map(|t| t.tip))
    }
}

struct Feeds {
    table: Arc<dyn FeedTable>,
    idx: Arc<dyn IndexAccess>,
}

/// A latch parker as a `Notify::Task`, notification-only (a permit): W's wake for SB5's close
/// hook, and V's while it waits for W (D23).
pub(super) struct ParkerWake(pub(super) Arc<Parker>);

impl Wake for ParkerWake {
    fn wake(self: Arc<Self>) {
        self.0.notify();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify();
    }
}

/// V's wake for a `FlushDue` post: the adapter's `FlushBell` (b3 adapter item 10).
type DueWake = Arc<dyn Fn() + Send + Sync>;

pub struct SubscriberHub {
    feeds: Option<Feeds>,
    state: Mutex<HubState>,
    /// W's one waiter (H7): acks, rewinds, deregistrations and SB5's close hook notify it, and
    /// W registers it with the latch before it parks (E-b1.7).
    w: Arc<Parker>,
    due: Mutex<Option<DueWake>>,
    /// The drivers' wakes on a new `T_ack` or gate: Syncing drivers read no feed (§4.4.2's edge
    /// "Syncing thread → the next `T_ack`").
    watchers: Mutex<Vec<(SubscriberId, Notify)>>,
    /// The full flush in flight from step 3b to its release (D35): Core's `cs_main` over
    /// `FlushStateToDisk`, which `BaseIndex::Init`'s registration and prune check also take. A
    /// registration runs while it is `None`, under this lock.
    window: Mutex<Window>,
    window_end: Condvar,
    #[cfg(feature = "planted-bugs")]
    planted: Mutex<Option<crate::Planted>>,
    /// Run once in step 5, after its synced read (D37's test).
    #[cfg(test)]
    after_synced_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Default for SubscriberHub {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SubscriberHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriberHub").field("state", &self.state).finish_non_exhaustive()
    }
}

impl SubscriberHub {
    /// The hub with no feeds (E-b1.6).
    pub fn new() -> Self {
        Self {
            feeds: None,
            state: Mutex::default(),
            w: Parker::new(),
            due: Mutex::default(),
            watchers: Mutex::default(),
            window: Mutex::default(),
            window_end: Condvar::new(),
            #[cfg(feature = "planted-bugs")]
            planted: Mutex::default(),
            #[cfg(test)]
            after_synced_read: Mutex::default(),
        }
    }

    /// The hub over a feed table, with the chain's lock-free reader for the gate's and the
    /// locks' ancestry. It takes the table's close hook (set once).
    pub fn with_feeds(table: Arc<dyn FeedTable>, idx: Arc<dyn IndexAccess>) -> Self {
        let w = Parker::new();
        table.set_close_hook(Notify::Task(Waker::from(Arc::new(ParkerWake(Arc::clone(&w))))));
        Self {
            feeds: Some(Feeds {
                table,
                idx,
            }),
            state: Mutex::default(),
            w,
            due: Mutex::default(),
            watchers: Mutex::default(),
            window: Mutex::default(),
            window_end: Condvar::new(),
            #[cfg(feature = "planted-bugs")]
            planted: Mutex::default(),
            #[cfg(test)]
            after_synced_read: Mutex::default(),
        }
    }

    /// Arms a planted bug in the hub (T16, T30, T35).
    #[cfg(feature = "planted-bugs")]
    pub fn plant(&self, bug: Option<crate::Planted>) {
        *self.planted.lock().unwrap_or_else(PoisonError::into_inner) = bug;
    }

    #[cfg(feature = "planted-bugs")]
    fn planted(&self) -> Option<crate::Planted> {
        *self.planted.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn feeds(&self) -> &Feeds {
        self.feeds.as_ref().expect("a hub over SB5's feeds")
    }

    /// The feed table: the drivers' `subscribe_at`, `on_release` and snapshots.
    pub fn table(&self) -> Option<&Arc<dyn FeedTable>> {
        self.feeds.as_ref().map(|f| &f.table)
    }

    /// `id`'s driver steps on this thread (D39).
    pub fn stepping_here(&self, id: SubscriberId) {
        if let Some(s) = self.state().sub_mut(id) {
            s.stepper = Some(thread::current().id());
            s.owner = s.stepper;
        }
    }

    /// This thread steps a registered subscriber that is not closing, so a flush or a hold may
    /// wait on it, and a blocking [`register`](Self::register) here could deadlock (D39).
    pub fn steps_on_this_thread(&self) -> bool {
        let me = thread::current().id();
        let ids: Vec<SubscriberId> =
            self.state().subs.iter().filter(|s| s.stepper == Some(me)).map(|s| s.id).collect();
        ids.into_iter().any(|id| !self.closing(id))
    }

    /// This thread owns a subscriber that is not closing and that `flush` waits on: a gate
    /// target not yet rewound, or a commit not yet acked (D47).
    fn owns_awaited(&self, flush: u64, me: ThreadId) -> bool {
        let ids: Vec<SubscriberId> =
            self.state().subs.iter().filter(|s| s.owner == Some(me)).map(|s| s.id).collect();
        ids.into_iter().any(|id| {
            let o = self.outstanding(id);
            (o.rewind.is_some_and(|r| r.flush == flush)
                || o.commit.is_some_and(|c| c.flush == flush))
                && !self.closing(id)
        })
    }

    /// The chain's reader the hub was given.
    pub fn index(&self) -> Option<&Arc<dyn IndexAccess>> {
        self.feeds.as_ref().map(|f| &f.idx)
    }

    /// True once `id` is closing, or gone.
    fn closing(&self, id: SubscriberId) -> bool {
        self.feeds.as_ref().is_none_or(|f| f.table.is_closing(id))
    }

    /// P8, linearized with the flush (D35): outside a flush's window (step 3b to its release),
    /// the hub's entry with its prune lock, then SB5's slot, so no flush's gate or step 7 starts
    /// between them. SB5 §4.3's `HoldActive` is answered here (D33): `on_release`, retry once,
    /// then wait for the wake (or the latch) and start again. On W's thread, on V's while step 7
    /// waits on V, or on the thread that owns a subscriber the flush waits on, the wait would wait
    /// on the caller: it is `WouldDeadlock` instead (D43, D47).
    ///
    /// # Errors
    ///
    /// Any other [`Refused`], or [`Fatal`] once the latch has tripped. The latch must be the
    /// chainstate's: a flush that ends without its machine's last step trips it (D43).
    ///
    /// # Panics
    ///
    /// On a hub with no feeds.
    pub fn register(
        &self,
        r: Registration,
        latch: &GlobalFailStop,
    ) -> Result<Result<SubscriberHandle, Refused>, Fatal> {
        let mut armed: Option<HubWait> = None;
        loop {
            match self.register_outside_flush(r, latch)? {
                Err(Refused::HoldActive) => match armed.take() {
                    None => {
                        let w = Parker::new();
                        self.feeds().table.on_release(Notify::Task(Waker::from(Arc::new(
                            ParkerWake(Arc::clone(&w)),
                        ))));
                        armed = Some(HubWait(w));
                    }
                    Some(w) => w.park(latch)?,
                },
                other => return Ok(other),
            }
        }
    }

    fn window(&self) -> MutexGuard<'_, Window> {
        self.window.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn register_outside_flush(
        &self,
        r: Registration,
        latch: &GlobalFailStop,
    ) -> Result<Result<SubscriberHandle, Refused>, Fatal> {
        let me = thread::current().id();
        let mut window = self.window();
        while let Some((flush, w)) = window.open {
            latch.check()?;
            if w == me || (window.awaits_v && window.v == Some(me)) || self.owns_awaited(flush, me)
            {
                return Ok(Err(Refused::WouldDeadlock));
            }
            window = self
                .window_end
                .wait_timeout(window, Duration::from_millis(20))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let core_lock = Arc::new(AtomicU32::new(r.durable.map_or(r.start_height, |l| l.height)));
        let id = {
            let mut s = self.state();
            s.next_id += 1;
            let id = SubscriberId(s.next_id);
            s.subs.push(Sub {
                id,
                name: r.name,
                mandatory: r.mandatory,
                start_height: r.start_height,
                durable: r.durable,
                pending: None,
                stepper: None,
                owner: Some(me),
                core_lock: Arc::clone(&core_lock),
            });
            id
        };
        // SB5's `register` never blocks (a held tip answers `HoldActive`), so the window waits
        // on nothing here.
        if let Err(e) = self.feeds().table.register(id, r.needs) {
            self.state().subs.retain(|s| s.id != id);
            return Ok(Err(e));
        }
        drop(window);
        Ok(Ok(SubscriberHandle {
            id,
            core_lock,
        }))
    }

    /// Step 3b opens the window (D35), on W's thread.
    fn begin_flush(&self, flush: u64) {
        let mut w = self.window();
        w.open = Some((flush, thread::current().id()));
        w.awaits_v = false;
    }

    /// The flush released (step 11), or ended without: registrations run again. Idempotent.
    pub fn end_flush(&self, flush: u64) {
        let mut w = self.window();
        if w.open.is_some_and(|(f, _)| f == flush) {
            w.open = None;
            w.awaits_v = false;
            self.window_end.notify_all();
        }
    }

    /// Runs `f` once, in the next step 5, after its synced read (D37's test).
    #[cfg(test)]
    pub(crate) fn set_after_synced_read(&self, f: impl FnOnce() + Send + 'static) {
        *self.after_synced_read.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(f));
    }

    /// V submits a flush from this thread, and answers its `PruneClears` here (D43).
    pub fn submitted_here(&self) {
        self.window().v = Some(thread::current().id());
    }

    /// Step 7 asked V for its clears (`true`), or has V's answer (`false`) (D43).
    pub fn awaits_v(&self, flush: u64, on: bool) {
        let mut w = self.window();
        if w.open.is_some_and(|(f, _)| f == flush) {
            w.awaits_v = on;
            self.window_end.notify_all();
        }
    }

    /// `id`'s driver wakes on every new `T_ack` and gate (replacing an earlier wake).
    pub fn watch(&self, id: SubscriberId, w: Notify) {
        let mut ws = self.watchers.lock().unwrap_or_else(PoisonError::into_inner);
        ws.retain(|(s, _)| *s != id);
        ws.push((id, w));
    }

    fn wake_watchers(&self) {
        let ws: Vec<Notify> = self
            .watchers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, w)| w.clone())
            .collect();
        ws.iter().for_each(Notify::notify);
    }

    /// F11, then H7's last clause: SB5 marks the slot closing, which retires its obligations at
    /// once and wakes W; the hub's entry, and with it the prune lock at the durable locator, goes
    /// only after SB5's `deregister` has returned.
    pub fn deregister(&self, id: SubscriberId) {
        if let Some(f) = &self.feeds {
            f.table.deregister(id);
        }
        self.state().subs.retain(|s| s.id != id);
        self.watchers.lock().unwrap_or_else(PoisonError::into_inner).retain(|(s, _)| *s != id);
        self.w.notify();
    }

    /// `T_ack`: `None` before the first flush or recovery.
    pub fn t_ack(&self) -> Option<CommitTarget> {
        self.state().t_ack
    }

    /// The cap on every subscriber commit (R11a): `T_ack`, lowered to `LCA(T_ack, new)` from a
    /// full flush's step 3b to its step 5 (D33).
    pub fn commit_cap(&self) -> Option<BlockRef> {
        self.state().cap()
    }

    /// A driver's upward commit at `x` on its p's path (R11a; D33): `x`, or `LCA(x, cap)` when
    /// `x` is not on the cap's path, recorded as pending under the same hold that reads the cap,
    /// so step 3b's read sees either the commit or the lowered cap. `None` with no cap. The
    /// commit's return is [`set_durable`](Self::set_durable); one with nothing to write ends
    /// with [`end_commit`](Self::end_commit).
    pub fn begin_commit(&self, id: SubscriberId, x: BlockRef) -> Option<BlockRef> {
        let idx = &self.feeds.as_ref()?.idx;
        loop {
            let cap = self.commit_cap()?;
            // The ancestry is read outside the leaf lock, then the cap re-checked under it.
            let at = if is_ancestor_or_eq(&**idx, x, cap) {
                Some(x)
            } else {
                lca_ref(&**idx, x, cap)
            };
            let mut s = self.state();
            if s.cap() != Some(cap) {
                continue;
            }
            let at = at?;
            if let Some(sub) = s.sub_mut(id) {
                sub.pending = Some(at);
            }
            return Some(at);
        }
    }

    /// A begun commit that writes nothing (its capped target is at or below L). A failed write
    /// stays pending: the latch has tripped, and it may be durable.
    pub fn end_commit(&self, id: SubscriberId) {
        if let Some(s) = self.state().sub_mut(id) {
            s.pending = None;
        }
    }

    /// Recovery step 10: `T_ack := G`, with no commit to send. The node calls it before it opens
    /// any subscriber: a mandatory one opened before it, with a locator, skips D45's open rewind,
    /// as one does when there is no durable tip (b4 m-D).
    pub fn recovered(&self, g: BlockRef) {
        let mut s = self.state();
        s.t_ack = Some(CommitTarget {
            tip: g,
            tip_seq: 0,
        });
        s.bound = None;
    }

    pub fn durable_locator(&self, id: SubscriberId) -> Option<BlockRef> {
        self.state().sub(id).and_then(|s| s.durable)
    }

    /// A driver's commit returned at `l` (any kind), ending a begun one. The locator is ignored
    /// from a closing slot (H7).
    pub fn set_durable(&self, id: SubscriberId, l: BlockRef) {
        let closing = self.closing(id);
        if let Some(s) = self.state().sub_mut(id) {
            s.pending = None;
            if !closing {
                s.durable = Some(l);
            }
        }
    }

    /// Step 5 (§4.2; F7): B1 returned Durable at `new`, so `T_ack := new`, closing any gate. Then
    /// the registered mandatory subscribers that are Synced now, filtered on closing after the
    /// registry snapshot is read (H7(b)), are captured, and each gets `FlushCommit` and
    /// `ChainStateFlushed` out of band; every other Synced subscriber gets `ChainStateFlushed`,
    /// as each of Core's listeners does. Returns the captured set.
    pub fn publish_durable(&self, flush: u64, new: BlockRef, tip_seq: u64) -> Vec<SubscriberId> {
        let commit = FlushCommit {
            flush,
            new,
            tip_seq,
        };
        let Some(f) = &self.feeds else {
            let mut s = self.state();
            s.t_ack = Some(CommitTarget {
                tip: new,
                tip_seq,
            });
            s.gate = None;
            s.bound = None;
            return Vec::new();
        };
        {
            // `T_ack` is published before the synced set is read (D37): a handoff the read misses
            // marks itself Synced after it, so its recheck of `T_ack` sees `new`.
            let mut s = self.state();
            s.t_ack = Some(CommitTarget {
                tip: new,
                tip_seq,
            });
            s.gate = None;
            s.bound = None;
        }
        let mut synced = f.table.synced();
        #[cfg(test)]
        if let Some(hook) =
            self.after_synced_read.lock().unwrap_or_else(PoisonError::into_inner).take()
        {
            hook();
        }
        if planted!(self.planted(), T35) {
            // T35: "has it processed `new`?" in place of the class.
            synced.retain(|&id| self.core_lock(id).is_some_and(|h| h >= new.height));
        }
        let (to, others) = {
            let mut s = self.state();
            let (to, others): (Vec<_>, Vec<_>) = synced
                .into_iter()
                .filter(|&id| s.sub(id).is_some())
                .partition(|&id| s.mandatory(id));
            s.commit = Some(CommitMirror {
                commit,
                to: to.clone(),
                acked: Vec::new(),
            });
            (to, others)
        };
        let flushed = Flushed {
            flush,
            new,
            tip_seq,
        };
        for &id in &to {
            f.table.send_control(id, ControlMsg::new(FLUSH_COMMIT, flush));
            f.table.send_control(id, ControlMsg::new(CHAIN_STATE_FLUSHED, flushed));
        }
        for id in others {
            f.table.send_control(id, ControlMsg::new(CHAIN_STATE_FLUSHED, flushed));
        }
        self.wake_watchers();
        to
    }

    /// Step 3b (§4.4.1 rule 4): the commit cap drops to `LCA(T_ack, to)` until step 5 (D33),
    /// and under the same hold the candidates are read: each registered mandatory subscriber's
    /// durable L and its pending commit. The targets are those of them that strictly descend
    /// from `to`, filtered on closing; a commit begun later is capped below `to`. With targets,
    /// the gate opens and each gets `RewindRequest`. First the flush's registration window
    /// opens (D35), so the candidates are every subscriber this flush will see.
    pub fn open_rewind_gate(&self, flush: u64, to: BlockRef) -> Vec<SubscriberId> {
        self.begin_flush(flush);
        let Some(f) = &self.feeds else {
            return Vec::new();
        };
        // `T_ack` moves only at step 5 and recovery, both on W's side of this call.
        let t_ack = self.state().t_ack;
        let bound =
            t_ack.map(|t| lca_ref(&*f.idx, t.tip, to).expect("two indexed blocks share genesis"));
        let candidates: Vec<(SubscriberId, BlockRef)> = {
            let mut s = self.state();
            s.bound = bound;
            s.subs
                .iter()
                .filter(|s| s.mandatory)
                .flat_map(|s| s.durable.into_iter().chain(s.pending).map(move |l| (s.id, l)))
                .collect()
        };
        let mut targets: Vec<SubscriberId> = candidates
            .into_iter()
            .filter(|&(id, l)| strictly_descends(&*f.idx, l, to) && !f.table.is_closing(id))
            .map(|(id, _)| id)
            .collect();
        targets.dedup();
        if targets.is_empty() {
            return targets;
        }
        let req = RewindRequest {
            flush,
            to,
        };
        self.state().gate = Some(Gate {
            req,
            targets: targets.clone(),
            rewound: Vec::new(),
        });
        for &id in &targets {
            f.table.send_control(id, ControlMsg::new(REWIND_REQUEST, flush));
        }
        self.wake_watchers();
        targets
    }

    /// Rule 1's read: the commit `id` has not acked and the gate it has not reported.
    pub fn outstanding(&self, id: SubscriberId) -> Outstanding {
        let s = self.state();
        Outstanding {
            commit: s
                .commit
                .as_ref()
                .filter(|c| c.to.contains(&id) && !c.acked.contains(&id))
                .map(|c| c.commit),
            rewind: s
                .gate
                .as_ref()
                .filter(|g| g.targets.contains(&id) && !g.rewound.contains(&id))
                .map(|g| g.req),
        }
    }

    /// Step 6's ack: `id` serviced `flush`'s commit, with its durable locator now `locator`.
    /// Ignored, locator and all, from a closing slot (H7) or for another flush.
    pub fn ack(&self, id: SubscriberId, flush: u64, locator: BlockRef) {
        if self.closing(id) {
            return;
        }
        {
            let mut s = self.state();
            match &mut s.commit {
                Some(c) if c.commit.flush == flush && c.to.contains(&id) => {
                    if !c.acked.contains(&id) {
                        c.acked.push(id);
                    }
                }
                _ => return,
            }
            // Validated under the same hold: a stale report leaves the mirror alone.
            if let Some(sub) = s.sub_mut(id) {
                sub.durable = Some(locator);
            }
        }
        self.w.notify();
    }

    /// Step 3b's report: `id`'s durable locator `locator` no longer strictly descends from the
    /// gate's `to`. Ignored, locator and all, from a closing slot (H7) or for another gate.
    pub fn rewound(&self, id: SubscriberId, flush: u64, locator: BlockRef) {
        if self.closing(id) {
            return;
        }
        {
            let mut s = self.state();
            match &mut s.gate {
                Some(g) if g.req.flush == flush && g.targets.contains(&id) => {
                    if !g.rewound.contains(&id) {
                        g.rewound.push(id);
                    }
                }
                _ => return,
            }
            // Validated under the same hold: a stale report leaves the mirror alone.
            if let Some(sub) = s.sub_mut(id) {
                sub.durable = Some(locator);
            }
        }
        self.w.notify();
    }

    /// H7's predicate for step 6: every captured subscriber has acked `flush` or is closing.
    pub fn acks_done(&self, flush: u64) -> bool {
        let pending: Vec<SubscriberId> = {
            let s = self.state();
            match &s.commit {
                Some(c) if c.commit.flush == flush => {
                    c.to.iter().filter(|id| !c.acked.contains(id)).copied().collect()
                }
                _ => return true,
            }
        };
        pending.into_iter().all(|id| self.closing(id))
    }

    /// H7's predicate for step 3b: every gate target has rewound or is closing.
    pub fn gate_done(&self, flush: u64) -> bool {
        let pending: Vec<SubscriberId> = {
            let s = self.state();
            match &s.gate {
                Some(g) if g.req.flush == flush => {
                    g.targets.iter().filter(|id| !g.rewound.contains(id)).copied().collect()
                }
                _ => return true,
            }
        };
        pending.into_iter().all(|id| self.closing(id))
    }

    /// W's wait handle (H7): acks, rewinds, deregistrations and closings notify it.
    pub(crate) fn waiter(&self) -> HubWait {
        HubWait(Arc::clone(&self.w))
    }

    /// `HubWait::park` on W's handle.
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped.
    pub fn park_w(&self, latch: &GlobalFailStop) -> Result<(), Fatal> {
        self.waiter().park(latch)
    }

    /// A subscriber at its limit (§4.4.1 rule 3) posts `FlushDue(DMax)`: the flag, then V's
    /// wake.
    pub fn post_flush_due(&self) {
        self.state().flush_due = true;
        let wake = self.due.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// V takes a pending `FlushDue` (adapter item 10), right before its freeze capture.
    pub fn take_flush_due(&self) -> bool {
        std::mem::take(&mut self.state().flush_due)
    }

    pub fn flush_due(&self) -> bool {
        self.state().flush_due
    }

    /// Every Synced subscriber's locator is at `tip`. §4.2's ALWAYS shortcut sends no
    /// `FlushCommit`, so it is taken only then (D27): Core's empty full flush still signals
    /// `ChainStateFlushed`, and every synced index commits at its locator.
    // Core: index/base.cpp:332-368 (BaseIndex::ChainStateFlushed)
    pub fn synced_at(&self, tip: &BlockRef) -> bool {
        let Some(f) = &self.feeds else {
            return true;
        };
        let synced = f.table.synced();
        self.state().subs.iter().filter(|s| synced.contains(&s.id)).all(|s| s.durable == Some(*tip))
    }

    /// V's wake for `FlushDue`: the adapter's bell, from `attach`.
    pub fn set_due_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.due.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(wake));
    }

    /// Step 0's locks (§4.2; DEC-64): each registered mandatory subscriber's `height_first`,
    /// Synced ones at `new`, the others at `height(LCA(L, new))`, and the start height with no
    /// locator. [`prune_lock_bound`](super::prune_lock_bound) takes them.
    pub fn predicted_locks(&self, new: &BlockRef) -> Vec<(&'static str, u32)> {
        let Some(f) = &self.feeds else {
            return Vec::new();
        };
        let synced = f.table.synced();
        let t16 = planted!(self.planted(), T16);
        let t30 = planted!(self.planted(), T30);
        self.lock_rows()
            .into_iter()
            .map(|(id, name, start, l)| {
                let h = match l {
                    _ if synced.contains(&id) => new.height,
                    // T16 and T30 as in `actual_locks`.
                    _ if t16 => self.core_lock(id).unwrap_or(start),
                    Some(l) if t30 => l.height,
                    Some(l) => lca_height(&*f.idx, l, *new),
                    None => start,
                };
                (name, h)
            })
            .collect()
    }

    /// Step 7's locks (§4.2): every mandatory subscriber's `height(LCA(L, new))`, or its start
    /// height with no locator, for every entry still registered, closing ones included (H7).
    pub fn actual_locks(&self, new: &BlockRef) -> Vec<(&'static str, u32)> {
        let Some(f) = &self.feeds else {
            return Vec::new();
        };
        let t16 = planted!(self.planted(), T16);
        let t30 = planted!(self.planted(), T30);
        self.lock_rows()
            .into_iter()
            .map(|(id, name, start, l)| {
                let h = match l {
                    // T16: the in-memory best height. T30: the locator's own height.
                    _ if t16 => self.core_lock(id).unwrap_or(start),
                    Some(l) if t30 => l.height,
                    Some(l) => lca_height(&*f.idx, l, *new),
                    None => start,
                };
                (name, h)
            })
            .collect()
    }

    /// The subscriber's Core-side lock: its best height as last reported.
    fn core_lock(&self, id: SubscriberId) -> Option<u32> {
        self.state().sub(id).map(|s| s.core_lock.load(Ordering::Relaxed))
    }

    fn lock_rows(&self) -> Vec<(SubscriberId, &'static str, u32, Option<BlockRef>)> {
        let s = self.state();
        s.subs
            .iter()
            .filter(|s| s.mandatory)
            .map(|s| (s.id, s.name, s.start_height, s.durable))
            .collect()
    }

    // Core: validation.cpp:3292-3301 (DisconnectTip's prune-lock moveback)
    /// The moveback's log lines at a disconnect of the block at `height`: each lock Core would
    /// hold above `height − 1` moves to `height − 1`. Log-only: CH2's locks come from the durable
    /// locators (DEC-64).
    pub fn on_disconnect_tip(&self, height: u32) -> Vec<(&'static str, u32)> {
        let max_height_first = height.saturating_sub(1);
        self.state()
            .subs
            .iter()
            .filter(|sub| {
                sub.core_lock.fetch_min(max_height_first, Ordering::Relaxed) > max_height_first
            })
            .map(|sub| (sub.name, max_height_first))
            .collect()
    }
}

/// W's wait on the hub, as a machine holds it across a `Step::Waiting`.
pub(crate) struct HubWait(Arc<Parker>);

impl HubWait {
    /// W's wait after a `Step::Waiting` on the hub (E-b1.7, H7): registers W's waker with the
    /// latch, then parks unless the latch has tripped. The step re-checks its predicate when it
    /// runs again; a wake that came between that check and this park is a kept permit.
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped.
    pub(crate) fn park(&self, latch: &GlobalFailStop) -> Result<(), Fatal> {
        let _registered = latch.register(&self.0);
        latch.check()?;
        self.0.park();
        Ok(())
    }
}

/// L strictly descends from `to`: `to` is a strict ancestor of L.
fn strictly_descends(idx: &dyn IndexAccess, l: BlockRef, to: BlockRef) -> bool {
    l.height > to.height && idx.ancestor(l.id, to.height) == Some(to.id)
}

/// `a` is `b` or an ancestor of it.
fn is_ancestor_or_eq(idx: &dyn IndexAccess, a: BlockRef, b: BlockRef) -> bool {
    a.height <= b.height && idx.ancestor(b.id, a.height) == Some(a.id)
}

fn lca_ref(idx: &dyn IndexAccess, a: BlockRef, b: BlockRef) -> Option<BlockRef> {
    let id = idx.lca(a.id, b.id)?;
    Some(BlockRef {
        id,
        hash: idx.hash(id)?,
        height: idx.height(id)?,
    })
}

/// `height(LCA(l, new))`; a block the index cannot place locks at genesis, the strictest.
fn lca_height(idx: &dyn IndexAccess, l: BlockRef, new: BlockRef) -> u32 {
    idx.lca(l.id, new.id).and_then(|x| idx.height(x)).unwrap_or(0)
}

#[cfg(test)]
mod tests;
