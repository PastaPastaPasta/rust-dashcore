//! The synchronous signals V hands the processor, queued in V's order (P3 §2.5).
//!
//! Core runs `BlockChecked` inline on the thread that validates (v:3432, v:4891). Here V pushes it
//! onto a bounded, ordered [`SignalQueue`] and rings; the actor applies it at its drain points.
//! Each entry carries a stamp, the first published `view_seq` whose state is consistent with
//! applying it, so every drain applies exactly the prefix its own snapshot covers (D37, D47).
//! `NewPoWValidBlock` is not queued: it runs whole on V's thread (D50).

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dash_consensus::BlockValidationError;
use dash_primitives::BlockHash;
use parking_lot::{Condvar, Mutex};

use super::seams::Interrupted;

/// The most entries a `Live` queue holds before V waits at a release point (D20).
pub const SIGNAL_QUEUE_MAX_ENTRIES: usize = 1_024;
/// The most bytes a `Live` queue holds before V waits at a release point: four 2 MB blocks
/// (consensus.h:10-14, D20).
pub const SIGNAL_QUEUE_MAX_BYTES: usize = 8 << 20;

/// A queued synchronous signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncSignal {
    /// `BlockChecked(block, state)` (np:2314; V emits it at v:3432 and v:4891): `invalid` is the
    /// failed state, `ibd` is V's `IsInitialBlockDownload()` at the signal.
    BlockChecked {
        /// The block.
        hash: BlockHash,
        /// The failed state; `None` when valid.
        invalid: Option<BlockValidationError>,
        /// V's IBD latch when it signalled.
        ibd: bool,
    },
}

impl SyncSignal {
    /// What the entry counts against [`SIGNAL_QUEUE_MAX_BYTES`]: its size plus its owned heap,
    /// the error's strings.
    fn cost(&self) -> usize {
        let heap = match self {
            Self::BlockChecked {
                invalid,
                ..
            } => invalid.as_ref().map_or(0, |e| e.reject_reason().len() + e.debug_message().len()),
        };
        std::mem::size_of::<Entry>() + heap
    }
}

#[derive(Debug)]
struct Entry {
    stamp: u64,
    signal: SyncSignal,
    cost: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Before the actor's first drain: V never waits, and pushes are discarded (DEC-230).
    PreStart,
    /// After it: lossless and bounded.
    Live,
}

#[derive(Debug)]
struct Inner {
    entries: VecDeque<Entry>,
    /// Entries queued or drained and not yet processed, and their bytes: the bound counts both,
    /// so space comes back per processed entry (m-3).
    held_entries: usize,
    held_bytes: usize,
    /// The highest number of entries held at once (tests read it).
    peak_entries: usize,
    mode: Mode,
    closed: bool,
}

impl Inner {
    fn within_bounds(&self) -> bool {
        self.held_entries <= SIGNAL_QUEUE_MAX_ENTRIES && self.held_bytes <= SIGNAL_QUEUE_MAX_BYTES
    }

    /// Whether V's waits end now: within bounds (C5), or no outstanding entry (C5′), and in
    /// both cases when the queue is `PreStart` or closed.
    fn released(&self, quiescent: bool) -> bool {
        self.closed
            || self.mode == Mode::PreStart
            || if quiescent {
                self.held_entries == 0
            } else {
                self.within_bounds()
            }
    }
}

/// The doorbell's state: whether it has rung since the last wait, and whether the actor is
/// asleep on it.
#[derive(Debug, Default)]
struct Door {
    rung: bool,
    parked: bool,
}

/// The actor's two wake-ups, which V's side rings: the doorbell ends the block wait (P3 §2.6), and
/// the wake flag ends the idle wait between passes (Core's `fMsgProcWake`). Both are leaf locks
/// any thread may take (LO-5, P2a-core D66 (2)).
#[derive(Debug, Default)]
pub struct Bells {
    doorbell: Mutex<Door>,
    door_cond: Condvar,
    pub(super) wake: Mutex<bool>,
    pub(super) cond: Condvar,
}

impl Bells {
    /// Rings the doorbell; a ring while one is pending is coalesced.
    pub fn ring(&self) {
        self.doorbell.lock().rung = true;
        self.door_cond.notify_all();
    }

    /// Core's `WakeMessageHandler`.
    pub fn wake(&self) {
        *self.wake.lock() = true;
        self.cond.notify_one();
    }

    /// Both: the doorbell and the wake flag.
    pub fn ring_all(&self) {
        self.ring();
        self.wake();
    }

    /// Blocks until the doorbell rings (or has rung since the last wait), then clears it.
    pub fn wait_doorbell(&self) {
        let mut door = self.doorbell.lock();
        if !door.rung {
            door.parked = true;
            self.door_cond.notify_all();
            self.door_cond.wait_while(&mut door, |d| !d.rung);
            door.parked = false;
        }
        door.rung = false;
    }

    /// Whether the actor is asleep in [`Bells::wait_doorbell`] with no ring pending. Once true
    /// after a ring, the actor has woken on that ring, run its loop, and gone back to sleep.
    /// For tests of the block wait's wake-ups (P3 S1 r1 F1).
    #[cfg(any(test, feature = "sim"))]
    pub fn is_parked(&self) -> bool {
        let door = self.doorbell.lock();
        door.parked && !door.rung
    }

    /// Blocks until [`Bells::is_parked`] holds.
    #[cfg(any(test, feature = "sim"))]
    pub fn wait_parked(&self) {
        let mut door = self.doorbell.lock();
        self.door_cond.wait_while(&mut door, |d| !(d.parked && !d.rung));
    }
}

/// A `std::task::Wake` that rings the doorbell: the waker the actor polls V's reply with (P3
/// §2.6).
pub(super) struct DoorbellWaker(pub(super) Arc<Bells>);

impl std::task::Wake for DoorbellWaker {
    fn wake(self: Arc<Self>) {
        self.0.ring();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.ring();
    }
}

/// A generation counter V's wait re-tests its condition on: the queue bumps it whenever space
/// comes back, the queue empties, or it closes.
#[derive(Default)]
pub struct WaitEvent {
    generation: Mutex<u64>,
    cond: Condvar,
    /// Rung after every bump: the host's ring of V's `wait_until` wake (S6b, D111).
    hook: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for WaitEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitEvent")
            .field("generation", &*self.generation.lock())
            .field("hooked", &self.hook.get().is_some())
            .finish_non_exhaustive()
    }
}

impl WaitEvent {
    /// The current generation.
    pub fn generation(&self) -> u64 {
        *self.generation.lock()
    }

    /// Bumps the generation, wakes every waiter, then runs the hook. Every caller has made its
    /// change under the queue's lock and released it first, so a waiter the hook wakes sees the
    /// change.
    pub fn notify(&self) {
        *self.generation.lock() += 1;
        self.cond.notify_all();
        if let Some(hook) = self.hook.get() {
            hook();
        }
    }

    /// See [`SignalQueue::on_notify`].
    fn on_notify(&self, hook: Box<dyn Fn() + Send + Sync>) {
        assert!(self.hook.set(hook).is_ok(), "one notify hook");
    }

    /// Blocks until the generation differs from `seen`, or `timeout` passes. Returns the
    /// generation then.
    pub fn wait_past(&self, seen: u64, timeout: Duration) -> u64 {
        let mut g = self.generation.lock();
        self.cond.wait_while_for(&mut g, |g| *g == seen, timeout);
        *g
    }
}

/// V's release-point wait (CH1-node's `ServiceWait`, ask A-1b): it serves V's class R and M
/// commands, takes both lanes and defers class C, holding no lock a `ChainAccess` read or
/// `read_block` takes, until `done` holds. `done` may have become true whenever `event`'s
/// generation moves. A `ProcessBlock` accept outside IBD is deferred too, not run inside the
/// wait (C5′, DEC-240). In IBD V may run one there (CH1 A-1b r2 R2-1): it can insert headers and
/// push `BlockChecked` onto this queue inside a [`SignalQueue::wait_for_space`] wait, and C8
/// never fires in IBD. `done` takes only the queue's leaf lock, which no thread holds while it
/// waits on V (LO-5, R2-3; S6b D120).
pub trait ServiceWait {
    /// Serves until `done()`; `Err` on shutdown.
    fn wait(
        &mut self,
        event: &WaitEvent,
        done: &mut dyn FnMut() -> bool,
    ) -> Result<(), Interrupted>;
}

/// A [`ServiceWait`] that serves nothing: for a V stand-in with no command lane.
#[derive(Debug, Default)]
pub struct ParkingWait;

impl ServiceWait for ParkingWait {
    fn wait(
        &mut self,
        event: &WaitEvent,
        done: &mut dyn FnMut() -> bool,
    ) -> Result<(), Interrupted> {
        loop {
            let seen = event.generation();
            if done() {
                return Ok(());
            }
            event.wait_past(seen, Duration::from_millis(50));
        }
    }
}

thread_local! {
    /// Whether this thread (V's) is inside a [`SignalQueue::wait_for_space`] or
    /// [`SignalQueue::wait_quiescent`] wait: a C5 point reached inside one returns at once, so the
    /// waits never nest (D29).
    static IN_SIGNAL_WAIT: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is inside a signal-queue wait (V's no-nesting flag, D29).
pub fn in_signal_wait() -> bool {
    IN_SIGNAL_WAIT.with(Cell::get)
}

struct NestGuard;

impl NestGuard {
    fn enter() -> Self {
        IN_SIGNAL_WAIT.with(|f| f.set(true));
        Self
    }
}

impl Drop for NestGuard {
    fn drop(&mut self) {
        IN_SIGNAL_WAIT.with(|f| f.set(false));
    }
}

/// The ordered, bounded queue of V's synchronous signals (P3 §2.5). A leaf lock: V's push never
/// waits behind per-peer work, and nothing is called under it.
pub struct SignalQueue {
    inner: Mutex<Inner>,
    bells: Arc<Bells>,
    event: WaitEvent,
    /// `mapBlockSource.size()`, kept by the actor's insertion paths: DEC-230's guard reads it.
    source_len: Arc<AtomicUsize>,
}

impl SignalQueue {
    /// An empty `PreStart` queue ringing `bells`, guarded by `source_len` (DEC-230).
    pub fn new(bells: Arc<Bells>, source_len: Arc<AtomicUsize>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: VecDeque::new(),
                held_entries: 0,
                held_bytes: 0,
                peak_entries: 0,
                mode: Mode::PreStart,
                closed: false,
            }),
            bells,
            event: WaitEvent::default(),
            source_len,
        }
    }

    /// V's push (its thread only). Never blocks. Discards once closed, and in `PreStart`, where
    /// every effect of the signal needs a `mapBlockSource` entry that cannot exist yet.
    ///
    /// # Panics
    ///
    /// In `PreStart` with `mapBlockSource` non-empty: the discard would then lose a signal
    /// (DEC-230's guard; on in every build).
    pub fn push(&self, stamp: u64, signal: SyncSignal) {
        let mut q = self.inner.lock();
        if q.closed {
            return;
        }
        if q.mode == Mode::PreStart {
            assert_eq!(
                self.source_len.load(Ordering::SeqCst),
                0,
                "DEC-230: a mapBlockSource entry exists before the first drain, so discarding \
                 a PreStart signal loses it"
            );
            return;
        }
        debug_assert!(q.entries.back().is_none_or(|e| e.stamp <= stamp), "stamps decrease");
        let cost = signal.cost();
        q.held_entries += 1;
        q.held_bytes += cost;
        q.peak_entries = q.peak_entries.max(q.held_entries);
        q.entries.push_back(Entry {
            stamp,
            signal,
            cost,
        });
    }

    /// The queued prefix stamped ≤ `cutoff`, the `view_seq` the caller's `drain_snapshot()`
    /// returned with its snapshot (D37, D47); later entries stay queued. Switches `PreStart` to
    /// `Live`. Each entry's cost comes back when its item drops, after the actor has processed it.
    pub fn drain(&self, cutoff: u64) -> Drained<'_> {
        let mut q = self.inner.lock();
        q.mode = Mode::Live;
        let n = q.entries.iter().take_while(|e| e.stamp <= cutoff).count();
        let items: VecDeque<Entry> = q.entries.drain(..n).collect();
        Drained {
            queue: self,
            items,
        }
    }

    fn release(&self, cost: usize) {
        let mut q = self.inner.lock();
        q.held_entries -= 1;
        q.held_bytes -= cost;
        drop(q);
        self.event.notify();
    }

    /// The actor has stopped: later pushes are dropped and the queued entries released, so a
    /// parked V resumes at once (the actor's drop guard calls it, m-6).
    pub fn close(&self) {
        let mut q = self.inner.lock();
        q.closed = true;
        let freed: Vec<Entry> = q.entries.drain(..).collect();
        for e in freed {
            q.held_entries -= 1;
            q.held_bytes -= e.cost;
        }
        drop(q);
        self.event.notify();
    }

    /// Runs `hook` whenever space comes back, the queue empties, or it closes, after the change
    /// is visible to a [`ServiceWait`]'s `done`: a host whose V waits on a channel of its own
    /// points it at that channel's sender, so this crate takes no channel type (S6b, D111). The
    /// hook must not block or take a lock `done` takes.
    ///
    /// # Panics
    ///
    /// When a hook is already set: the queue has one V.
    pub fn on_notify(&self, hook: Box<dyn Fn() + Send + Sync>) {
        self.event.on_notify(hook);
    }

    /// Whether [`SignalQueue::close`] ran.
    pub fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }

    /// Whether the actor's first drain has happened.
    pub fn is_live(&self) -> bool {
        self.inner.lock().mode == Mode::Live
    }

    /// `(entries, bytes)` held: queued, plus drained and not yet processed.
    pub fn held(&self) -> (usize, usize) {
        let q = self.inner.lock();
        (q.held_entries, q.held_bytes)
    }

    /// The highest number of entries held at once.
    pub fn peak_entries(&self) -> usize {
        self.inner.lock().peak_entries
    }

    /// Rings the actor when a `Live` queue holds entries, so it applies what V's last
    /// publication made eligible without waiting for a later ring (D45).
    fn ring_if_queued(&self) {
        let ring = {
            let q = self.inner.lock();
            q.mode == Mode::Live && !q.closed && !q.entries.is_empty()
        };
        if ring {
            self.bells.ring_all();
        }
    }

    /// C5: V's release-point wait. First rings the actor whenever a `Live` queue holds entries
    /// (D45). Returns at once while the queue is within both bounds, `PreStart` or closed, and
    /// when V is already inside a signal-queue wait (no nesting, D29); otherwise serves through
    /// `wait` until one of those holds.
    pub fn wait_for_space(&self, wait: &mut dyn ServiceWait) -> Result<(), Interrupted> {
        self.wait_until(wait, false)
    }

    /// C5′ (DEC-240, D55): before an accept outside IBD, V waits until no entry is outstanding
    /// (none queued, none drained and unprocessed), ringing first. Ends as
    /// [`SignalQueue::wait_for_space`] does when the queue is `PreStart` or closed, or V is
    /// already inside a signal-queue wait.
    pub fn wait_quiescent(&self, wait: &mut dyn ServiceWait) -> Result<(), Interrupted> {
        self.wait_until(wait, true)
    }

    fn wait_until(&self, wait: &mut dyn ServiceWait, quiescent: bool) -> Result<(), Interrupted> {
        if in_signal_wait() {
            return Ok(());
        }
        self.ring_if_queued();
        if self.inner.lock().released(quiescent) {
            return Ok(());
        }
        let _nest = NestGuard::enter();
        // An earlier ring may have been coalesced into one the actor already consumed.
        self.bells.ring_all();
        wait.wait(&self.event, &mut || self.inner.lock().released(quiescent))
    }
}

/// The entries one drain took, in push order. Each item releases its entry's cost when dropped;
/// entries not yet taken are released when this drops.
pub struct Drained<'q> {
    queue: &'q SignalQueue,
    items: VecDeque<Entry>,
}

impl<'q> Drained<'q> {
    /// How many entries remain.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether none remain.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl<'q> Iterator for Drained<'q> {
    type Item = Processing<'q>;

    fn next(&mut self) -> Option<Processing<'q>> {
        let e = self.items.pop_front()?;
        Some(Processing {
            queue: self.queue,
            signal: e.signal,
            cost: e.cost,
        })
    }
}

impl Drop for Drained<'_> {
    fn drop(&mut self) {
        for e in self.items.drain(..) {
            self.queue.release(e.cost);
        }
    }
}

/// One drained signal while the actor processes it; its cost comes back on drop.
pub struct Processing<'q> {
    queue: &'q SignalQueue,
    signal: SyncSignal,
    cost: usize,
}

impl std::ops::Deref for Processing<'_> {
    type Target = SyncSignal;
    fn deref(&self) -> &SyncSignal {
        &self.signal
    }
}

impl Drop for Processing<'_> {
    fn drop(&mut self) {
        self.queue.release(self.cost);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Instant;

    use dash_types::Numeric as _;

    use super::*;

    fn checked(i: u8) -> SyncSignal {
        SyncSignal::BlockChecked {
            hash: BlockHash::from_lendian([i; 32]),
            invalid: None,
            ibd: false,
        }
    }

    fn queue() -> (Arc<SignalQueue>, Arc<Bells>, Arc<AtomicUsize>) {
        let bells = Arc::new(Bells::default());
        let source = Arc::new(AtomicUsize::new(0));
        let q = Arc::new(SignalQueue::new(Arc::clone(&bells), Arc::clone(&source)));
        (q, bells, source)
    }

    fn live() -> (Arc<SignalQueue>, Arc<Bells>) {
        let (q, bells, _) = queue();
        drop(q.drain(0));
        (q, bells)
    }

    fn hashes(d: Drained<'_>) -> Vec<u8> {
        d.map(|s| {
            let SyncSignal::BlockChecked {
                hash,
                ..
            } = &*s;
            hash.as_bytes()[0]
        })
        .collect()
    }

    /// Whether `rx` hears from another thread within `ms`.
    fn finishes_within(ms: u64, rx: &mpsc::Receiver<()>) -> bool {
        rx.recv_timeout(Duration::from_millis(ms)).is_ok()
    }

    #[test]
    fn pre_start_discards_and_the_first_drain_goes_live() {
        let (q, _, _) = queue();
        q.push(1, checked(1));
        assert_eq!(q.held(), (0, 0));
        assert!(!q.is_live());
        assert!(q.drain(10).is_empty());
        assert!(q.is_live());
        q.push(11, checked(2));
        assert_eq!(q.held().0, 1);
    }

    #[test]
    #[should_panic(expected = "DEC-230")]
    fn a_pre_start_push_with_a_block_source_entry_panics() {
        let (q, _, source) = queue();
        source.store(1, Ordering::SeqCst);
        q.push(1, checked(1));
    }

    #[test]
    fn a_drain_takes_exactly_the_prefix_at_or_below_its_cutoff_in_push_order() {
        let (q, _) = live();
        for (stamp, i) in [(3, 1), (3, 2), (4, 3), (5, 4)] {
            q.push(stamp, checked(i));
        }
        assert_eq!(hashes(q.drain(2)), Vec::<u8>::new());
        assert_eq!(hashes(q.drain(3)), vec![1, 2]);
        // A later-stamped entry pushed after the snapshot stays for the next drain.
        q.push(6, checked(5));
        assert_eq!(hashes(q.drain(5)), vec![3, 4]);
        assert_eq!(hashes(q.drain(6)), vec![5]);
        assert_eq!(q.held(), (0, 0));
    }

    /// Fills `q` one entry past its entry bound.
    fn overfill(q: &SignalQueue) {
        for i in 0..=SIGNAL_QUEUE_MAX_ENTRIES {
            q.push(1, checked(i as u8));
        }
    }

    /// A thread standing in for V that parks in `wait_for_space` (or `wait_quiescent`) and
    /// reports when it returns.
    fn park(q: &Arc<SignalQueue>, quiescent: bool) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        let q = Arc::clone(q);
        std::thread::spawn(move || {
            let r = if quiescent {
                q.wait_quiescent(&mut ParkingWait)
            } else {
                q.wait_for_space(&mut ParkingWait)
            };
            assert_eq!(r, Ok(()));
            tx.send(()).unwrap();
        });
        rx
    }

    #[test]
    fn space_comes_back_per_processed_entry_not_at_the_drain() {
        let (q, _) = live();
        overfill(&q);
        let done = park(&q, false);
        assert!(!finishes_within(100, &done));
        let mut drained = q.drain(1);
        // Drained but unprocessed entries still count (m-3).
        assert!(!finishes_within(100, &done));
        let first = drained.next().unwrap();
        drop(first);
        assert!(finishes_within(5_000, &done));
        assert_eq!(q.held().0, SIGNAL_QUEUE_MAX_ENTRIES);
        drop(drained);
        assert_eq!(q.held(), (0, 0));
        assert_eq!(q.peak_entries(), SIGNAL_QUEUE_MAX_ENTRIES + 1);
    }

    #[test]
    fn the_byte_bound_parks_v_on_its_own() {
        // One entry whose debug message alone exceeds `SIGNAL_QUEUE_MAX_BYTES`: within the entry
        // bound, past the byte bound.
        let (q, _) = live();
        let big = BlockValidationError::new(dash_consensus::BlockValidationResult::Consensus, "x")
            .with_debug("d".repeat(SIGNAL_QUEUE_MAX_BYTES));
        q.push(
            1,
            SyncSignal::BlockChecked {
                hash: BlockHash::from_lendian([1; 32]),
                invalid: Some(big),
                ibd: false,
            },
        );
        assert_eq!(q.held().0, 1);
        assert!(q.held().1 > SIGNAL_QUEUE_MAX_BYTES);
        let done = park(&q, false);
        assert!(!finishes_within(100, &done));
        drop(q.drain(1));
        assert!(finishes_within(5_000, &done));
        assert_eq!(q.held(), (0, 0));
    }

    #[test]
    fn close_releases_a_parked_v_and_drops_later_pushes() {
        let (q, _) = live();
        overfill(&q);
        let done = park(&q, false);
        assert!(!finishes_within(100, &done));
        q.close();
        assert!(finishes_within(5_000, &done));
        q.push(2, checked(1));
        assert_eq!(q.held(), (0, 0));
        assert!(q.is_closed());
    }

    /// A V wait that, while serving, reaches another C5 point: it must return at once (D29).
    struct Nested<'a>(&'a SignalQueue, bool);

    impl ServiceWait for Nested<'_> {
        fn wait(
            &mut self,
            _event: &WaitEvent,
            _done: &mut dyn FnMut() -> bool,
        ) -> Result<(), Interrupted> {
            let start = Instant::now();
            assert_eq!(self.0.wait_for_space(&mut ParkingWait), Ok(()));
            assert_eq!(self.0.wait_quiescent(&mut ParkingWait), Ok(()));
            self.1 = start.elapsed() < Duration::from_secs(5);
            Ok(())
        }
    }

    #[test]
    fn waits_never_nest() {
        let (q, _) = live();
        overfill(&q);
        let mut nested = Nested(&q, false);
        assert_eq!(q.wait_for_space(&mut nested), Ok(()));
        assert!(nested.1);
        assert!(!in_signal_wait());
    }

    /// An RPC `ProcessBlock` queued to V: its block's entry, and where its reply goes.
    type Accept = (u8, mpsc::Sender<()>);

    /// V's space wait serving its command lane: each accept pushes its `BlockChecked`, reaches
    /// its own C5 point (which returns at once, D29) and replies.
    struct Serving<'a>(&'a SignalQueue, mpsc::Receiver<Accept>);

    impl ServiceWait for Serving<'_> {
        fn wait(
            &mut self,
            _event: &WaitEvent,
            done: &mut dyn FnMut() -> bool,
        ) -> Result<(), Interrupted> {
            while !done() {
                if let Ok((i, reply)) = self.1.recv_timeout(Duration::from_millis(5)) {
                    self.0.push(1, checked(i));
                    assert_eq!(self.0.wait_for_space(&mut ParkingWait), Ok(()));
                    reply.send(()).unwrap();
                }
            }
            Ok(())
        }
    }

    #[test]
    fn n_queued_rpc_accepts_are_served_inside_a_space_wait_within_the_maximum() {
        // §6.1's N-sender case: the peak stays within §2.5's maximum for Q = N + 1 (the
        // overfilling entry and one per sender), and every sender gets its reply.
        for n in [1u8, 4, 16] {
            let (q, _) = live();
            overfill(&q);
            let (lane, rx) = mpsc::channel::<Accept>();
            let (v_done_tx, v_done) = mpsc::channel();
            let vq = Arc::clone(&q);
            let v = std::thread::spawn(move || {
                assert_eq!(vq.wait_for_space(&mut Serving(&vq, rx)), Ok(()));
                v_done_tx.send(()).unwrap();
            });
            let senders: Vec<_> = (0..n)
                .map(|i| {
                    let lane = lane.clone();
                    std::thread::spawn(move || {
                        let (tx, rx) = mpsc::channel();
                        lane.send((i, tx)).unwrap();
                        rx.recv_timeout(Duration::from_secs(30)).expect("a reply");
                    })
                })
                .collect();
            for s in senders {
                s.join().unwrap();
            }
            assert!(!finishes_within(50, &v_done));
            // Each accept pushed while V was parked, and nothing else did.
            assert_eq!(q.peak_entries(), SIGNAL_QUEUE_MAX_ENTRIES + usize::from(n) + 1);
            drop(q.drain(1));
            assert!(finishes_within(5_000, &v_done));
            v.join().unwrap();
            assert_eq!(q.held(), (0, 0));
        }
    }

    #[test]
    fn a_live_non_empty_queue_rings_within_its_bounds_and_an_empty_one_does_not() {
        let (q, bells) = live();
        // The drain that went live left nothing rung.
        assert_eq!(q.wait_for_space(&mut ParkingWait), Ok(()));
        assert!(!bells.doorbell.lock().rung && !*bells.wake.lock());
        q.push(1, checked(1));
        assert_eq!(q.wait_for_space(&mut ParkingWait), Ok(()));
        assert!(bells.doorbell.lock().rung && *bells.wake.lock());
    }

    #[test]
    fn the_quiescent_wait_ends_when_no_entry_is_outstanding() {
        let (q, _) = live();
        q.push(1, checked(1));
        let done = park(&q, true);
        assert!(!finishes_within(100, &done));
        let mut drained = q.drain(1);
        let item = drained.next().unwrap();
        // Drained, not yet processed: still outstanding.
        assert!(!finishes_within(100, &done));
        drop(item);
        assert!(finishes_within(5_000, &done));
        // PreStart and closed queues end it at once.
        let (pre, _, _) = queue();
        assert_eq!(pre.wait_quiescent(&mut ParkingWait), Ok(()));
        let (closed, _) = live();
        closed.push(1, checked(1));
        closed.close();
        assert_eq!(closed.wait_quiescent(&mut ParkingWait), Ok(()));
    }
}
