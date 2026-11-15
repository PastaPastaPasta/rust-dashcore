//! Feeds: the producer side of CH2's `SubscriberFeed` contract (F1–F11) on SB0's `TipCell`
//! (SB5 §3.1, §3.2, §4.3, §4.5, §6.1).
//!
//! Each subscriber has one slot behind one leaf mutex whose every critical section is O(1)
//! (DEC-152): the producer pushes into a queue reserved at `register`, the reader swaps the queue
//! out, and control messages wait in the slot, one per kind (DEC-155 (1)). No section allocates
//! or frees; a replaced payload or waker is freed after the section, and every `Notify` is cloned
//! inside and called after.

#![cfg_attr(all(test, loom), allow(dead_code, reason = "the loom build runs only the models"))]

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::chain::HeaderChain;
use crate::cold::ColdWalk;
use crate::error::{ChainError, FailStop};
use crate::fault::{Section, SlotFaults};
use crate::notify::Notify;
use crate::reader::ChainReader;
use crate::residency::Residency;
use crate::snapshot::ChainReadSnapshot;
use crate::store::HeaderStore;
use crate::sync::{
    AtomicBool, AtomicU64, Mutex, MutexGuard, Ordering, Published, thread_token, yield_now,
};
use crate::tip::TipStep;
use crate::tip::{BlockRef, TipCell, TipState};

mod reader;
mod source;
mod table;

pub use crate::status_feed::StatusReader;
use crate::status_feed::{ClFact, StatusCell, StatusFacts};
pub use reader::{FeedReader, Next, SubscriberFeed};
pub use source::{
    ChainFeeds, ChainSnapshot, FeedId, FeedSnapshot, FeedSource, Handoff, Registry, RegistryEntry,
    SubscribeError,
};
pub(crate) use table::{Fanout, FeedHandle, TipCtx, TipFeeds, TipLock};

/// CH2's event type is SB4's `TipStep`, identical in shape (CH2 §2.9.1). 56 bytes on x86-64.
pub type ChainEvent = TipStep;

/// CH2's `BlockNeeds` (P8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Needs {
    pub block: bool,
    pub undo: bool,
}

/// What this chain's host can keep readable for delivered events (F8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockData {
    None,
    BlocksAndUndo,
}

/// Why `register` or a tip-lock acquisition from `ChainFeeds` was refused (§4.3, DEC-157 (2)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("this chain has no block data")]
    NoBlockData,
    #[error("this chain has no undo data")]
    NoUndo,
    #[error("the fail-stop latch has tripped")]
    Latched,
    #[error("the feed table is full")]
    TableFull,
    /// A hold is open: register `on_release`, re-check, then wait for the wake (§4.3).
    #[error("the tip writer holds the tip")]
    HoldActive,
    /// Called on the hold owner's own thread, or from inside a handoff's `finish()`: refused in
    /// every build.
    #[error("called from the thread that holds the tip")]
    WouldDeadlock,
}

/// The writer's fanout result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    Done,
    Blocked,
}

/// The number of control kinds: `outstanding` is a `u8` bit set.
pub const MAX_CONTROL_KINDS: usize = 8;
/// A control payload's size bound, checked at compile time in [`ControlMsg::new`].
pub const CONTROL_PAYLOAD_MAX: usize = 256;

/// A control kind, `0..MAX_CONTROL_KINDS` (DEC-155 (1)). CH2b assigns one per `Control` variant
/// and defines them as constants, so an out-of-range kind is a compile error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct ControlKind(u8);

impl ControlKind {
    /// Panics (in const context: fails to compile) for `k >= MAX_CONTROL_KINDS`.
    pub const fn new(k: u8) -> ControlKind {
        assert!((k as usize) < MAX_CONTROL_KINDS, "control kind out of range");
        ControlKind(k)
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    const fn bit(self) -> u8 {
        1 << self.0
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An out-of-band message for one subscriber, opaque to `dash-chain` (SB0 §6.1). CH2's hub
/// wraps its `Control` in it. `T: Copy` owns no heap, so a message is 24 B inline plus at most
/// `CONTROL_PAYLOAD_MAX` boxed.
pub struct ControlMsg {
    kind: ControlKind,
    payload: Box<dyn Any + Send>,
}

impl ControlMsg {
    /// Boxes `t`; the caller does this outside every lock.
    pub fn new<T: Any + Send + Copy>(kind: ControlKind, t: T) -> ControlMsg {
        const { assert!(size_of::<T>() <= CONTROL_PAYLOAD_MAX, "control payload too large") };
        ControlMsg {
            kind,
            payload: Box::new(t),
        }
    }

    pub fn kind(&self) -> ControlKind {
        self.kind
    }

    pub fn downcast<T: Any>(self) -> Result<T, ControlMsg> {
        let ControlMsg {
            kind,
            payload,
        } = self;
        match payload.downcast::<T>() {
            Ok(t) => Ok(*t),
            Err(payload) => Err(ControlMsg {
                kind,
                payload,
            }),
        }
    }
}

impl fmt::Debug for ControlMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlMsg").field("kind", &self.kind).finish_non_exhaustive()
    }
}

/// `send_control`'s result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendControl {
    /// The kind was free; the message is queued behind the other outstanding kinds.
    Queued,
    /// A message of this kind was outstanding: its payload was replaced by the new one and the
    /// kind moved to the back (n1).
    Coalesced,
    /// The subscriber is gone or closing; the message was dropped.
    Stale,
}

/// The feeds' configuration (SB5 §3.1). SB5-A6 adds it to SB4's `ChainConfig` as `feeds`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedConfig {
    /// Q: data events per subscriber, queue plus staging (F3). Default 1,024; mobile 256.
    pub capacity: u32,
    /// The node: `BlocksAndUndo`; SPV: `None`.
    pub block_data: BlockData,
    /// The feed cap is `max_feeds` × the per-feed budget (§6.1). Node 16; SPV 8.
    pub max_feeds: u32,
    /// E: events per sub-step of an SPV composite (§4.8). Default = Q.
    pub segment_events: u32,
    /// W: rows per write batch of the SPV header step (§3.7). Default 1,024; mobile 256.
    pub write_batch: u32,
    /// B: headers per SPV header step (§6.2). Default 2,000.
    pub step_headers: u32,
}

impl FeedConfig {
    /// The node's defaults.
    pub const fn node() -> FeedConfig {
        FeedConfig {
            capacity: 1024,
            block_data: BlockData::BlocksAndUndo,
            max_feeds: 16,
            segment_events: 1024,
            write_batch: 1024,
            step_headers: 2000,
        }
    }

    /// SPV desktop's defaults.
    pub const fn spv() -> FeedConfig {
        FeedConfig {
            block_data: BlockData::None,
            max_feeds: 8,
            ..FeedConfig::node()
        }
    }

    /// SPV mobile's defaults (§6, D22).
    pub const fn spv_mobile() -> FeedConfig {
        FeedConfig {
            capacity: 256,
            segment_events: 256,
            write_batch: 256,
            ..FeedConfig::spv()
        }
    }

    /// Refuses a configuration the feeds cannot run: every count must be at least 1 (Q = 0
    /// would leave the writer `Blocked` forever), and `max_feeds` at most [`MAX_FEEDS`].
    pub const fn validate(&self) -> Result<(), FeedConfigError> {
        if self.max_feeds > MAX_FEEDS {
            return Err(FeedConfigError::MaxFeeds(self.max_feeds));
        }
        let zero = if self.capacity == 0 {
            "capacity"
        } else if self.max_feeds == 0 {
            "max_feeds"
        } else if self.segment_events == 0 {
            "segment_events"
        } else if self.write_batch == 0 {
            "write_batch"
        } else if self.step_headers == 0 {
            "step_headers"
        } else {
            return Ok(());
        };
        Err(FeedConfigError::Zero(zero))
    }

    /// The feed cap: `max_feeds` × [`per_feed_cap`] (§6.1). `register` refuses past it.
    pub const fn feed_cap(&self) -> usize {
        self.max_feeds as usize * per_feed_cap(self.capacity)
    }
}

/// A refused [`FeedConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FeedConfigError {
    #[error("feeds: {0} must be at least 1")]
    Zero(&'static str),
    #[error("feeds: max_feeds {0} is past {MAX_FEEDS}")]
    MaxFeeds(u32),
}

/// The most feeds a configuration may allow: §6.2's step cap reserves a registry republish
/// of `max_feeds` entries, so its registry term grows with it (DEC-187 amendment 2 (a)).
pub const MAX_FEEDS: u32 = 64;

/// §6.1's fixed term per feed: the slot and its `Arc`, the reader's box, the table and registry
/// entries, `ctl_order` and the `ctl_payload` array. Asserted at compile time to cover their
/// `size_of` sum (528 on x86-64; §6.1, erratum D14).
pub const FEED_FIXED_BYTES: usize = 576;

/// The control payload term: `MAX_CONTROL_KINDS` outstanding plus one being handled (a driver
/// contract, n2), each `size_of::<ControlMsg>()` + `CONTROL_PAYLOAD_MAX`.
pub const CONTROL_BYTES: usize =
    (MAX_CONTROL_KINDS + 1) * (size_of::<ControlMsg>() + CONTROL_PAYLOAD_MAX);

/// The per-feed budget (§6.1, DEC-155): the fixed term, three Q-sized event buffers reserved at
/// `register`, and the control payloads.
pub const fn per_feed_cap(capacity: u32) -> usize {
    FEED_FIXED_BYTES + 3 * capacity as usize * size_of::<ChainEvent>() + CONTROL_BYTES
}

/// The release-wake list's reserved entries (§4.2: `SmallVec<[Notify; 4]>`'s inline size).
const RELEASE_WAITERS: usize = 4;

/// An `Arc`'s heap header: the strong and weak counts.
const ARC_HEADER: usize = 2 * size_of::<usize>();

// The fixed term covers what one feed allocates besides its three event buffers and payloads:
// the slot, the reader's box, its table entry and free-list index, and its registry entry. The
// slot's fault hooks are test-only (a ZST in production), so they are not counted.
const _: () = assert!(
    ARC_HEADER
        + (size_of::<FeedSlot>() - size_of::<SlotFaults>())
        + size_of::<FeedReader>()
        + table::TABLE_ENTRY_BYTES
        + size_of::<u32>()
        + size_of::<RegistryEntry>()
        <= FEED_FIXED_BYTES,
    "FEED_FIXED_BYTES is below the size_of sum"
);

/// One subscriber's slot (§3.2). Slot objects are never reused (D3).
pub(crate) struct FeedSlot {
    /// H7's linearization point: SeqCst, monotone.
    closing: AtomicBool,
    /// Q.
    cap: u32,
    inner: Mutex<SlotInner>,
    pub(crate) faults: SlotFaults,
}

pub(crate) struct SlotInner {
    /// Capacity ≥ Q, reserved at `register`; never reallocates (`used ≤ Q`).
    queue: VecDeque<ChainEvent>,
    /// Kinds in first-queued order (coalescing moves a kind to the back).
    ctl_order: [ControlKind; MAX_CONTROL_KINDS],
    ctl_len: u8,
    ctl_payload: [Option<ControlMsg>; MAX_CONTROL_KINDS],
    /// Bit k: a message of kind k is queued and `next()` has not returned it.
    outstanding: u8,
    /// Queued plus the reader's staged data events; ≤ Q.
    used: u32,
    /// The producer is waiting for space on this slot.
    want_space: bool,
    waker: Option<Notify>,
}

/// The guard of one slot critical section. Test builds count the sections the thread is in, so
/// `tests/feed_alloc.rs`'s counting allocator can assert that none allocates or frees (§4.5).
pub(crate) struct SlotGuard<'a> {
    g: MutexGuard<'a, SlotInner>,
    _section: Section,
}

impl std::ops::Deref for SlotGuard<'_> {
    type Target = SlotInner;
    fn deref(&self) -> &SlotInner {
        &self.g
    }
}

impl std::ops::DerefMut for SlotGuard<'_> {
    fn deref_mut(&mut self) -> &mut SlotInner {
        &mut self.g
    }
}

/// `try_push`'s result.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code, reason = "F2's fanout reads it"))]
pub(crate) enum Push {
    /// Pushed; notify the subscriber's waker, if any, after the section.
    Pushed(Option<Notify>),
    /// `used = Q`: `want_space` is set, so the reader's next pop wakes the producer.
    Full,
    Closed,
}

/// The reader's two Q-sized buffers, allocated with the slot before the tip lock.
pub(crate) struct ReaderBuffers {
    pub(crate) staging: VecDeque<ChainEvent>,
    pub(crate) spare: VecDeque<ChainEvent>,
}

#[cfg_attr(any(not(test), loom), allow(dead_code, reason = "F2 wires the slots into the TipCell"))]
impl FeedSlot {
    /// A slot with its queue and the reader's two buffers, each reserved at `cap` (DEC-152 (1)).
    pub(crate) fn new(cap: u32) -> (Arc<FeedSlot>, ReaderBuffers) {
        assert!(cap > 0, "Q = 0 would leave the writer Blocked forever");
        let q = cap as usize;
        let slot = FeedSlot {
            closing: AtomicBool::new(false),
            cap,
            inner: Mutex::new(SlotInner {
                queue: VecDeque::with_capacity(q),
                ctl_order: [ControlKind::default(); MAX_CONTROL_KINDS],
                ctl_len: 0,
                ctl_payload: Default::default(),
                outstanding: 0,
                used: 0,
                want_space: false,
                waker: None,
            }),
            faults: SlotFaults::default(),
        };
        let bufs = ReaderBuffers {
            staging: VecDeque::with_capacity(q),
            spare: VecDeque::with_capacity(q),
        };
        (Arc::new(slot), bufs)
    }

    pub(crate) fn lock(&self) -> SlotGuard<'_> {
        SlotGuard {
            g: self.inner.lock(),
            _section: Section::enter(),
        }
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    /// The producer's push (§4.3): one O(1) section.
    pub(crate) fn try_push(&self, e: ChainEvent) -> Push {
        let mut g = self.lock();
        if self.is_closing() {
            return Push::Closed;
        }
        if g.used < self.cap {
            debug_assert!(g.queue.len() < g.queue.capacity(), "the queue never reallocates");
            g.queue.push_back(e);
            g.used += 1;
            Push::Pushed(g.waker.clone())
        } else {
            g.want_space = true;
            Push::Full
        }
    }

    /// F4 out of band (§4.3, DEC-155 (1)): queue `c`, or replace the outstanding payload of its
    /// kind. The replaced payload is freed after the section, then the subscriber is notified.
    /// `closing` is checked before the section (step 1): a racing close may still queue `c`,
    /// which the slot frees with itself.
    pub(crate) fn send_control(&self, c: ControlMsg) -> SendControl {
        if self.is_closing() {
            return SendControl::Stale;
        }
        let (r, old, w) = {
            let mut g = self.lock();
            let (r, old) = g.put_control(c);
            (r, old, g.waker.clone())
        };
        drop(old);
        if let Some(w) = w {
            w.notify();
        }
        r
    }

    /// The outstanding kinds, as a bit set.
    pub(crate) fn control_outstanding(&self) -> u8 {
        self.lock().outstanding
    }

    /// F5: stores the subscriber's waker; the previous one is freed after the section. On a
    /// closed slot it fires `w` instead: `close` swaps `closing` before its section, so a waker
    /// stored after that section would never be woken.
    pub(crate) fn set_waker(&self, w: Notify) {
        let mut g = self.lock();
        if self.is_closing() {
            drop(g);
            w.notify();
            return;
        }
        let old = g.waker.replace(w);
        drop(g);
        drop(old);
    }

    /// `deregister` steps 2–3 and `FeedReader::drop` (§4.3): the `closing` store, then the
    /// producer and close-hook wakes, and the reader's, so a parked reader sees the close. Never
    /// takes the tip lock (D9). Idempotent.
    /// `deregister` and the reader's drop both close; only the first close wakes. Each takes the
    /// waker out, so the table's retire never drops it under the tip lock (§4.2).
    pub(crate) fn close(&self, shared: &FeedShared) {
        let first = !self.closing.swap(true, Ordering::SeqCst);
        let old = self.lock().waker.take();
        if first {
            shared.notify_producer();
            shared.notify_close();
            if let Some(w) = &old {
                w.notify();
            }
        }
        drop(old);
    }

    /// `used` decreases by `n` events that left staging; returns whether the producer waits.
    fn release_space(&self, n: u32) -> bool {
        let mut g = self.lock();
        debug_assert!(g.used >= n, "used counts every staged event");
        g.used -= n;
        std::mem::take(&mut g.want_space)
    }
}

#[cfg(test)]
impl FeedSlot {
    /// (`used`, queued events, outstanding control messages): the F3 and DEC-155 (1) checks.
    pub(crate) fn counts(&self) -> (u32, usize, u32) {
        let g = self.lock();
        debug_assert_eq!(g.ctl_len as u32, g.outstanding.count_ones());
        (g.used, g.queue.len(), g.outstanding.count_ones())
    }

    /// (control messages held, outstanding kinds as a bit set), unchecked: loom's release build
    /// has no `debug_assert` to compare them.
    #[cfg(loom)]
    pub(crate) fn control_held(&self) -> (u8, u8) {
        let g = self.lock();
        (g.ctl_len, g.outstanding)
    }
}

impl SlotInner {
    /// `next()`'s first half: the oldest outstanding control message, its kind freed.
    fn take_control(&mut self) -> Option<ControlMsg> {
        if self.ctl_len == 0 {
            return None;
        }
        let k = self.ctl_order[0];
        let len = self.ctl_len as usize;
        self.ctl_order.copy_within(1..len, 0);
        self.ctl_len -= 1;
        self.outstanding &= !k.bit();
        let msg = self.ctl_payload[k.index()].take();
        debug_assert!(msg.is_some(), "an ordered kind has a payload");
        msg
    }

    /// Queues `c` (a free kind) or replaces its kind's payload and moves the kind to the back.
    /// Returns the replaced payload for the caller to free after the section.
    fn put_control(&mut self, c: ControlMsg) -> (SendControl, Option<ControlMsg>) {
        let k = c.kind;
        let len = self.ctl_len as usize;
        if self.outstanding & k.bit() != 0 {
            let pos = self.ctl_order[..len]
                .iter()
                .position(|&o| o == k)
                .expect("outstanding kind is ordered");
            self.ctl_order[pos..len].rotate_left(1);
            let old = self.ctl_payload[k.index()].replace(c);
            (SendControl::Coalesced, old)
        } else {
            self.ctl_order[len] = k;
            self.ctl_len += 1;
            self.outstanding |= k.bit();
            debug_assert!(self.ctl_payload[k.index()].is_none(), "a free kind has no payload");
            self.ctl_payload[k.index()] = Some(c);
            (SendControl::Queued, None)
        }
    }

    /// Data intake (§4.3): moves the queue out by swapping 32-byte headers. Returns true if the
    /// caller must append `spare` to `staging` after the section.
    fn intake(
        &mut self,
        staging: &mut VecDeque<ChainEvent>,
        spare: &mut VecDeque<ChainEvent>,
    ) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        debug_assert!(spare.is_empty(), "spare is empty between calls");
        if staging.is_empty() {
            std::mem::swap(&mut self.queue, staging);
            false
        } else {
            std::mem::swap(&mut self.queue, spare);
            true
        }
    }
}

/// Everything about the feeds reachable without the tip lock (§3.2).
pub(crate) struct FeedShared {
    /// H2: republished under the tip lock on every table change.
    pub(crate) registry: Published<Registry>,
    producer_wake: OnceLock<Notify>,
    /// W's wake (H4); W re-reads the registry.
    close_hook: OnceLock<Notify>,
    /// The hold owner's thread token while a hold is open, else 0. Stored after the tip lock is
    /// taken, cleared before the guard drops (§4.3, §4.7).
    held_owner: AtomicU64,
    /// The thread token of a handoff running `finish()` under the tip lock, else 0: a
    /// `ChainFeeds` call from inside `finish()` is `WouldDeadlock`, as on a hold's thread.
    finish_owner: AtomicU64,
    /// One-shot release wakes (DEC-157 (2)). The release takes the list after the guard drops
    /// and hands its buffer back, so steady-state pushes reuse it.
    release_waiters: Mutex<Vec<Notify>>,
    /// The status facts and their readers' wakes (§3.6).
    pub(crate) status: StatusCell,
    /// The SPV writer's step budget peaks (§6.2).
    pub(crate) budget: crate::budget::StepBudget,
    pub(crate) cfg: FeedConfig,
    /// The fail-stop latch (SB5-A15): `register` and `subscribe_at` refuse once it trips.
    fail: Arc<dyn FailStop>,
    #[cfg(all(test, not(loom)))]
    pub(crate) faults: SharedFaults,
}

/// Pause points on the subscriber side (§10.1's directed interleavings).
#[cfg(all(test, not(loom)))]
#[derive(Default)]
pub(crate) struct SharedFaults {
    /// `deregister`, between its `closing` store and step 4.
    pub(crate) before_step4: crate::fault::PausePoint,
    /// SB4 A-23's observer: every published snapshot, under the tip lock, right after its store.
    pub(crate) on_store: crate::sync::Mutex<Option<SnapshotObserver>>,
    /// A charge over §6.2's sub-step term: slots of spare capacity reserved on each sub-step's
    /// connect leg before its check.
    pub(crate) inflate_legs: std::sync::atomic::AtomicUsize,
}

#[cfg(all(test, not(loom)))]
impl SharedFaults {
    /// `change` with at least [`SharedFaults::inflate_legs`] spare slots on its connect leg.
    pub(crate) fn inflated(&self, mut change: crate::tip::TipChange) -> crate::tip::TipChange {
        let n = self.inflate_legs.load(std::sync::atomic::Ordering::Relaxed);
        change.connected.reserve_exact(n);
        change
    }
}

#[cfg(all(test, not(loom)))]
pub(crate) type SnapshotObserver = Box<dyn Fn(&Arc<ChainReadSnapshot>) + Send + Sync>;

impl FeedShared {
    pub(crate) fn new(cfg: FeedConfig, fail: Arc<dyn FailStop>) -> FeedShared {
        FeedShared {
            registry: Published::new(Registry {
                epoch: 0,
                feeds: Vec::with_capacity(cfg.max_feeds as usize),
            }),
            producer_wake: OnceLock::new(),
            close_hook: OnceLock::new(),
            held_owner: AtomicU64::new(0),
            finish_owner: AtomicU64::new(0),
            release_waiters: Mutex::new(Vec::with_capacity(RELEASE_WAITERS)),
            status: StatusCell::new(),
            budget: Default::default(),
            cfg,
            fail,
            #[cfg(all(test, not(loom)))]
            faults: SharedFaults::default(),
        }
    }

    /// The fail-stop latch has tripped: `register` and `subscribe_at` refuse.
    pub(crate) fn is_latched(&self) -> bool {
        self.fail.is_tripped()
    }

    /// Set once; a second call is ignored. Unset: no wakes (tests poll).
    pub(crate) fn set_producer_wake(&self, w: Notify) {
        let _ = self.producer_wake.set(w);
    }

    /// Set once; a second call is ignored.
    pub(crate) fn set_close_hook(&self, w: Notify) {
        let _ = self.close_hook.set(w);
    }

    pub(crate) fn notify_producer(&self) {
        if let Some(w) = self.producer_wake.get() {
            w.notify();
        }
    }

    pub(crate) fn notify_close(&self) {
        if let Some(w) = self.close_hook.get() {
            w.notify();
        }
    }

    /// Takes the tip lock for a `ChainFeeds` call without ever blocking on a hold (§4.3,
    /// DEC-157 (2)). `try_lock` is the tip mutex's. A hold or a handoff's `finish()` on this
    /// thread is `WouldDeadlock`, a hold on another `HoldActive`; an ordinary step section is
    /// waited out by yielding. `#[inline]`: the feed calls' fast path is one `try_lock`, and
    /// without it the crate's codegen units decide whether `register` keeps it inline (D68,
    /// PF0 feed-subscribe-at).
    #[inline]
    pub(crate) fn acquire_tip<G>(
        &self,
        mut try_lock: impl FnMut() -> Option<G>,
    ) -> Result<G, Refused> {
        loop {
            if let Some(g) = try_lock() {
                return Ok(g);
            }
            match self.held_owner.load(Ordering::SeqCst) {
                0 if self.finish_owner.load(Ordering::SeqCst) == thread_token() => {
                    return Err(Refused::WouldDeadlock);
                }
                0 => yield_now(),
                owner if owner == thread_token() => return Err(Refused::WouldDeadlock),
                _ => return Err(Refused::HoldActive),
            }
        }
    }

    /// A hold is open on this thread: call right after the tip lock is taken.
    pub(crate) fn hold_opened(&self) {
        let prev = self.held_owner.swap(thread_token(), Ordering::SeqCst);
        debug_assert_eq!(prev, 0, "holds do not nest");
    }

    /// The hold is closing: call right before the guard drops.
    pub(crate) fn hold_closing(&self) {
        self.held_owner.store(0, Ordering::SeqCst);
    }

    /// Runs a handoff's `finish()`, under the tip lock, as this thread's: a re-entrant
    /// `register` or `subscribe_at` refuses rather than spinning on the lock it holds. Cleared on
    /// unwind too.
    pub(crate) fn in_finish<T>(&self, finish: impl FnOnce() -> T) -> T {
        struct Clear<'a>(&'a AtomicU64);
        impl Drop for Clear<'_> {
            fn drop(&mut self) {
                self.0.store(0, Ordering::SeqCst);
            }
        }
        debug_assert_eq!(
            self.finish_owner.load(Ordering::SeqCst),
            0,
            "one handoff at a time, under the tip lock"
        );
        self.finish_owner.store(thread_token(), Ordering::SeqCst);
        let _clear = Clear(&self.finish_owner);
        finish()
    }

    /// A one-shot wake at the next release. Protocol: on `HoldActive`, `on_release(w)`, retry
    /// once, and wait for `w` only if the retry is `HoldActive` again.
    pub(crate) fn on_release(&self, w: Notify) {
        self.release_waiters.lock().push(w);
    }

    /// After the guard dropped: wake every `on_release` waiter. The list is moved out of the
    /// section and freed after it.
    pub(crate) fn fire_release_wakes(&self) {
        let mut ws = std::mem::take(&mut *self.release_waiters.lock());
        for w in ws.drain(..) {
            w.notify();
        }
        // Return the emptied buffer unless a waiter already started a new one (a swap: nothing
        // is allocated or freed in the section).
        let mut g = self.release_waiters.lock();
        if g.capacity() == 0 {
            std::mem::swap(&mut *g, &mut ws);
        }
    }
}

// ---- SB5-A14's seams: what SB4's `tip.rs`, `config.rs` and `chain.rs` call ------------------

/// The tip lock as `ChainFeeds` takes it: `try_lock` only (§4.3).
impl TipLock for parking_lot::Mutex<TipState> {
    fn try_with(&self, f: &mut dyn FnMut(&mut TipFeeds, TipCtx)) -> bool {
        let Some(mut st) = self.try_lock() else {
            return false;
        };
        let ctx = TipCtx {
            tip: st.tip,
            next_seq: st.next_seq,
            pre_genesis: st.pre_genesis,
        };
        f(&mut st.feeds, ctx);
        true
    }
}

/// Called under the tip lock immediately after every snapshot store but the cell's first
/// (§3.3, SB5-A4): publishes the snapshot's status facts when their [`StatusKey`] changed (n7),
/// then fans the step's events out.
/// A node step publishes one event, an SPV composite all of its steps; `snap` carries the last
/// one's `seq` (F2).
#[inline]
pub(crate) fn after_publish(
    st: &mut TipState,
    cell: &TipCell,
    snap: &Arc<ChainReadSnapshot>,
    ev: Fanout<'_>,
) {
    debug_assert_eq!(
        snap.epoch, st.snapshot.epoch,
        "after_publish follows the state's snapshot store"
    );
    debug_assert!(
        ev.end().is_none_or(|end| end == snap.tip_seq + 1),
        "the step's events end at the published tip_seq"
    );
    #[cfg(all(test, not(loom)))]
    if let Some(observe) = &*cell.shared.faults.on_store.lock() {
        observe(snap);
    }
    let key = status_key(snap);
    if st.feeds.status != key {
        cell.shared.status.publish(status_facts(snap));
        st.feeds.status = key;
    } else {
        // Every store into the cell is the branch above, or `republish`'s, which unsets the
        // key: an equal key means the cell holds these facts already.
        #[cfg(all(debug_assertions, not(loom)))]
        debug_assert_eq!(*cell.shared.status.facts(), status_facts(snap), "status key desync");
    }
    st.feeds.fanout(&cell.shared, ev);
}

/// Called under the tip mutex after each step that stores no snapshot: inside the node's
/// `enforce_block` (SB4 §9.1, D-C6-1) and inside a hold (§4.7). The step's event goes to the
/// live feeds, or to `pending`, at once; the writer drains before its next step (§3.3).
pub(crate) fn after_step(st: &mut TipState, cell: &TipCell, step: crate::tip::TipStep) {
    st.feeds.fanout(&cell.shared, Fanout::One(step));
}

/// The writer's rule (§3.3), `TipFeeds::before_step`: called under the tip mutex before each
/// step and each SPV composite or sub-step.
pub(crate) fn before_step(st: &TipState) -> Result<(), ChainError> {
    st.feeds.before_step()
}

/// The status facts `snap` holds (§3.6, SB5-A17): its best header and that header's time, and
/// its ChainLock floor, active when the observed spork state is on and the snapshot's active
/// chain holds the floor at its height. Core notifies only from `EnforceBestChainLock`, which
/// returns while the spork is off (`chainlock/handler.cpp:275`), and only for an ancestor of the
/// tip (`:299-309`). An SPV floor below the view's window reads as inactive. Always `Some` best
/// header, so the cell's first publication always bumps `version` (`StatusReader::new`).
fn status_facts(snap: &ChainReadSnapshot) -> StatusFacts {
    let t = &snap.tip;
    let best = BlockRef {
        id: t.best_header,
        hash: t.best_header_hash,
        height: t.best_header_height,
    };
    StatusFacts {
        best_header: Some((best, t.best_header_time)),
        cl: snap.cl_floor.map(|block| ClFact {
            block,
            active: cl_active(snap, block),
        }),
    }
}

#[inline]
fn cl_active(snap: &ChainReadSnapshot, floor: BlockRef) -> bool {
    snap.cl_enabled_observed && snap.active.at(floor.height) == Some(floor.id)
}

/// What decides [`status_facts`]: the best header's id, and the ChainLock floor's id with
/// whether it is active. A chain instance issues each id once and never changes its block, so
/// equal keys are equal facts: the writer compares keys under the tip lock and builds the facts
/// only when the key changes. Packed, so an unchanged step compares two words (PF0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusKey {
    best: u32,
    /// [`StatusKey::NO_FLOOR`], or the floor's id × 2 + whether it is active.
    floor: u64,
}

impl StatusKey {
    /// Before the writer's first store: equal to no snapshot's key.
    pub(crate) const UNSET: StatusKey = StatusKey {
        best: 0,
        floor: u64::MAX,
    };
    const NO_FLOOR: u64 = u64::MAX - 1;
}

#[inline]
fn status_key(snap: &ChainReadSnapshot) -> StatusKey {
    let floor = match snap.cl_floor {
        Some(floor) => (u64::from(floor.id.0) << 1) | u64::from(cl_active(snap, floor)),
        None => StatusKey::NO_FLOOR,
    };
    StatusKey {
        best: snap.tip.best_header.0,
        floor,
    }
}

/// `HeaderChain::feeds`.
pub(crate) fn chain_feeds(
    cell: &TipCell,
    reader: ChainReader,
    cold: Arc<dyn ColdWalk>,
) -> ChainFeeds {
    ChainFeeds {
        handle: FeedHandle {
            tip: cell.state.clone(),
            shared: cell.shared.clone(),
        },
        reader,
        cold,
    }
}

/// `HeaderChain::status_feed`.
pub(crate) fn status_reader(cell: &TipCell) -> StatusReader {
    StatusReader::new(cell.shared.clone(), || status_facts(&cell.snapshot()))
}

/// The writer's side of the feeds (§3.3).
impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// The writer's wake on space and on deregistration. Set once, before the first
    /// `register`; unset, there are no wakes (tests poll).
    pub fn set_producer_wake(&self, w: Notify) {
        self.reader().backend.readers().tip().shared.set_producer_wake(w);
    }
}

#[cfg(all(test, not(loom)))]
mod chain_tests;
#[cfg(all(test, loom))]
mod hold_loom;
#[cfg(all(test, loom))]
pub(crate) mod loom_tests;
#[cfg(all(test, loom))]
mod table_loom;
#[cfg(all(test, not(loom)))]
mod table_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(all(test, not(loom)))]
mod tests;
