# SB5 — `dash-chain` feeds and residency: design

| | |
|---|---|
| Row | PLAN §4 SB5 (`PLAN.md:748`); SB0 §6.1–§6.3, §4.4, §9.5 |
| Status | **rev3, final, for the manager's narrow check.** rev0 17750c58 → Sol r1 REWORK (3e3e7099) → DEC-151, **DEC-152** (51db3da2) → rev1 f6cb2226 + DEC-153 addendum a7297d13 → Sol r2 REWORK (83599c0a1) → **DEC-155** (4eafea4c7) → rev2 26556bfd0 → Opus high APPROVE-WITH-CHANGES (`reviews/SB5-design-opus-high.md`, ae529e3de) → **DEC-157** (same commit) → rev3 (§17 is the rev3 closure map; §16 rev2's, §15 rev1's) |
| Date | 2026-10-10 (rev2, rev3); 2026-10-09 (rev0, rev1) |
| Tier | D O-h; impl O-h; rev Sol → O-h. Size M (2 weeks) |
| Depends | SB4 (APPROVED rev7.1, DEC-149). Designed against SB4's C0 stubs at `node/sb4-dash-chain` @ f815ec15 (`dash-chain/src/{tip,snapshot,store,residency,chain,reader,host,error}.rs`); rev3's sizes are measured at 562a0f053 (C2 + benches) |
| Feeds | CH2b (the `SubscriberFeed` producer, `FeedSource`, the registry snapshot for H2/H7), CH1-node (V's producer loop), SB13 (the SQLite `HeaderStore` and the header step's write set), SB14 (the SPV driver's producer loop, the subscriber adapters), SB15 (SPV `open`), SB11 (`StatusEvent` consumers) |
| Inputs | SB0 rev6.1 §4.1, §4.4, §6.1–§6.3, §7, §9.5 and errata E1–E4 (DEC-119); SB4 rev7.1 §1.1, §3.4–§3.8, §6, §9, §10, §13, §15.2 (DEC-149); CH2 §2.9.1 (F1–F11), §4.4.1 rules 1–6, §4.4.3, §7.2 b1–b3, §10.1 P8 (DEC-103); EV addendum §2.6; DEC-95/99/102 (Core semantics where Core has them); DEC-151; DEC-152; DEC-153 (the tip feed is not Core's validation-signal queue); DEC-155 (memory bounds by construction); DEC-156 (LO-9, publish before notify); DEC-157 (SPV-LO9, registration never blocks on a hold, the overlay in the budget, ask labels); CH1-node rev3 f8ee73524 (CH1-A1, CH1-A11, CH1-B1–B4) |
| Core pin | "v24" means **v24.0.0-rc.3** (`git show v24.0.0-rc.3:<path>` in `~/workspace/dash`). Every line cite below is at that tag |

## 0. Summary

SB5 finishes `dash-chain` on top of SB4's core. It has two halves.

**Feeds.** Feeds are the producer side of CH2's `SubscriberFeed` contract (F1–F11) and SB0's `TipCell` (§6.1):
- one bounded queue per subscriber, filled by the tip writer inside SB4's tip-lock critical section;
- a `pending` backlog with per-target cursors, which the writer drains on space and deregistration wakes;
- registration that starts **inactive**, and the atomic catch-up-to-live handoff `subscribe_at` (F10);
- deregistration that is final (F11), with generational ids and a lock-free `closing` flag (SB4 H4, H7);
- **no `ChainFeeds` call ever blocks on a hold** (DEC-157 (2)): `register` and `subscribe_at` return `HoldActive` with a release wake, and `deregister` never waits for the tip lock;
- out-of-band control (F4), selected together with data in **one** slot critical section (`next()`), with **one outstanding message per control kind**: a same-kind send coalesces until the reader consumes it (DEC-155 (1));
- a reader that takes events by **swapping the queue out** in O(1) and compresses in its own staging;
- a coalescing `StatusEvent` cell, kept apart from the F1 stream;
- **budgets with configuration-derived hard caps** (DEC-155 (2)): a per-feed budget for the reserved buffers and control payloads, and an SPV step budget for every allocation on the header-step path. Deep composites run as sub-steps of at most E events inside one hold, and the write set streams in batches of at most W rows; past a cap the writer takes the Blocked path or flushes, never an unbounded allocation. Caps are `const fn`s of the configuration and the real `size_of` values; any other overrun is a loud internal error. The one adversarial step term is the mark count of a ChainLock's marking walk, held in a status-only overlay (DEC-157 (3); §6).

**Residency.** Residency is the SPV half of the resident window:
- `open`/reopen from the `HeaderStore` at stable ids;
- the cold LRU, read through under its lock (EV addendum §2.6);
- snapshot heights below the window, found by ancestry from the snapshot's own tip;
- `IndexAccess` over cold records, with fail-stop;
- the header step's write-set contract, plus a `hdr_leaf` table so `leaves()` is O(branches);
- `MemHeaderStore`'s commit and fault injection;
- the windowed ≡ unwindowed property suite.

**Three concurrency rules carry the design.**
1. The tip writer changes the tip, assigns `seq`, publishes the snapshot and fans the events out in one critical section under the tip lock (SB4 §9.1). Nothing a subscriber does to make progress takes that lock.
2. **Inside that critical section the writer blocks only on leaf mutexes whose every critical section is O(1), and every wake is notification-only, enforced by type** (DEC-152, as Core's `AddToProcessQueue` under `cs_main`). SB0 §6.3 P2 now says so (erratum E6, applied to SB0 §6.1–§6.3, §7 and §17 in rev2). Across a **hold** the holder may also block on its own I/O and on its in-hold service wait (§4.2, E6a).
3. **Nothing the tip writer waits on waits on the tip writer** (rev3). In the node that is CH1-node's LO-9 (DEC-156 (1)); in SPV it is **SPV-LO9** (§4.9, DEC-157 (1)); and no `ChainFeeds` call waits on a hold (DEC-157 (2)).

## 1. Scope

### 1.1 What SB5 owns

SB4 §1.1 sets the split. SB5's rows are:

| Item | SB5 |
|---|---|
| `TipCell` | feeds, `pending`, `register`, `subscribe_at`, `deregister`, `send_control`, producer wakes, the registry snapshot (H2), the `closing` flag and its wakes (H4, H7), the `StatusEvent` cell |
| `SubscriberFeed`, `FeedSource`, `ChainSnapshot` | the traits move from CH2 §2.9.1 into `dash-chain` (SB0 §6.1; DEC-151 OQ1), with the reader change in §3.4 |
| `Needs`/`Refused` | P8: SPV refuses any subscriber that needs block data or undo |
| `Residency::Evicting` | `open`/reopen, the cold LRU, snapshot `at_height` below the chunks, the cold variant of `IndexAccess`, the §4.4 property suite |
| `HeaderStore` | the additions in §3.7, the streamed write-set contract (`StepRow`, `HeaderStepSink`, `StepTail`), `MemHeaderStore`'s sink and step commit, and fault injection; SB13 writes the SQLite store against them |
| SPV `fork_target` and the CL state at reopen | `open` recomputes `best_header` (E5) and `fork_target`, restores `cl_best`, `cl_floor` and `enabled_observed`, and rebuilds the checkpoint cache (SB4 §11.4) |

### 1.2 Not in SB5

- CH2's hub (`SubscriberHub`): the `Control` types, the mirror, W's channel and the flush barrier (F7), prune locks, and the subscriber driver.
- CH1-node's service loop, and its Core-style validation-signal dispatcher (DEC-153 (2)). The F1 stream carries only `TipStep`; Core's `CValidationInterface` callbacks never go through a feed.
- SB14's SPV driver loop and its adapters.
- SB13's SQL.
- The SPORK_19 and ChainLock logic, which is SB4 §7.

SB5 provides the seams each of these uses, and states the protocol each must follow (§4.3).

## 2. Placement and modules

All new code is in `dash-chain`, which stays `#![forbid(unsafe_code)]` and adds no runtime dependency. The `Cargo.lock` already has `arc-swap`, `parking_lot` (with `arc_lock`), `hashbrown` and `smallvec`. `loom` is added as a `cfg(loom)` dev-dependency (MIT; cargo-deny allows it).

| Module | Content |
|---|---|
| `tip.rs` (SB4's) | `TipState` gains `feeds` and `pending`; `TipCell` gains `shared: Arc<FeedShared>` |
| `notify.rs` | `Notify`, `Parker` (§3.1) |
| `feed.rs` | `FeedId`, `FeedSlot`, `FeedTable`, `Pending` (cursors and segments), fanout and `retry_pending` |
| `feed/reader.rs` | `FeedReader`, `SubscriberFeed`, `Next`, intake, staging and compression |
| `feed/source.rs` | `ChainFeeds` (the `FeedSource` handle), `FeedSnapshot`, `Handoff`, the registry snapshot |
| `status_feed.rs` (`status.rs` is SB4's `Status`) | `StatusEvent`, `StatusCell`, `StatusReader` |
| `cold.rs` | `ColdCache` (the CLOCK cache) |
| `open.rs` | `HeaderChain::open` |
| `store.rs` (SB4's) | the `HeaderStore` additions (SB5-A18; erratum D35) |
| `step.rs` (erratum D35) | `StepRow`, `HeaderStepSink`, `StepTail`, `ActiveDiff`, `MetaDelta`, `ChainMeta`, `AnchorState`, `Durability`, `HeaderRow` and `HeaderRecord::{to_row, from_row}` |
| `mem_store.rs` (`cfg(any(test, feature = "test-util"))`; erratum D35) | `MemHeaderStore`: the store tables, the step sink and `commit_step`, durable/buffered layers, `power_loss`, faults |
| `sync.rs` | the loom facade (§10.2): `Mutex`, `Condvar`, atomics, `Arc`, `Published<T>` |
| `fault.rs` (`cfg(any(test, feature = "test-util"))`) | pause points inside slot critical sections and store faults (§10.1) |

**Erratum D35 (F6a, accepted by the manager).** rev3 placed the step types in SB4's `store.rs` and called `mem_store.rs` SB4's. SB5 landed both ahead of SB4's C4, in new SB5 files: the types and the row codec in `step.rs`, and the whole `MemHeaderStore` in `mem_store.rs`. SB4's C4 builds on this store for its A-33/A-34 and overlay tests, and does not write its own.

- **The trait methods.** The four §3.7 methods (`chain_meta`, `active_from`, `walk_down`, `max_id`) stay SB4's to add to `HeaderStore`, with `NoStore` impls (SB5-A18). Until then, `MemHeaderStore` has them as inherent methods with §3.7's signatures. C4 moves them into `impl HeaderStore for MemHeaderStore`; that one edit to `mem_store.rs` is SB4's.
- **Sink rules made explicit.**
  - `LeafRemove` of an id that is not a leaf is a no-op, as SQL's `DELETE` is. The producer may therefore emit it for every new accepted record's parent without knowing whether the parent was a leaf; `HeaderRecord::step_rows` does this.
  - `LeafAdd` of an existing leaf, or of a context record, is an error.
  - A step's error may come from `rows` (as SQLite's mid-stream UNIQUE or I/O errors) or from the commit; either abandons the step.
- **What `MemHeaderStore` adds beyond §3.7.**
  - A `fail_rows` fault.
  - `power_loss_keeping(k)`, which keeps the first k Buffered steps since the last Durable commit (SB0 §9.4: a suffix is lost), so P2 and P4 can reopen at any committed cut. `power_loss()` is k = 0.
  - Commit-time checks that §3.7's SQL would not make, so a producer bug fails at stage 3 rather than at reopen:
    - the active diff's fork is active at or below `old_height`, and its walk passes accepted records one height apart;
    - `meta.tip` is the active top.
  - `next_id` above every stored id is **not** enforced, so F7 can build the `NextIdNotAbove` state.

## 3. API

### 3.1 Events, ids and wakes

```rust
/// CH2's event type is SB4's `TipStep`, identical in shape (CH2 §2.9.1). 56 bytes on x86-64.
pub type ChainEvent = TipStep;

/// Generational: a reused table index gets a new `gen`; every call with a stale id is a no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FeedId { index: u32, gen: u32 }

/// CH2's `BlockNeeds` (P8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Needs { pub block: bool, pub undo: bool }

/// What this chain's host can keep readable for delivered events (F8): set once in `ChainConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockData { None, BlocksAndUndo }

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("this chain has no block data")] NoBlockData,
    #[error("this chain has no undo data")]  NoUndo,
    #[error("the fail-stop latch has tripped")] Latched,
    #[error("the feed table is full")] TableFull,
    /// A hold is open (DEC-157 (2)): register `on_release`, re-check, then wait for the wake (§4.3).
    #[error("the tip writer holds the tip")] HoldActive,
    /// Called on the hold owner's own thread: refused in every build (Opus high M1 case 4).
    #[error("called from the thread that holds the tip")] WouldDeadlock,
}

/// A control kind, `0..MAX_CONTROL_KINDS`. CH2b assigns one per `Control` variant
/// (`FlushCommit`, `ChainStateFlushed`, `RewindRequest`, …). DEC-155 (1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlKind(u8);
impl ControlKind {
    /// `outstanding` is a `u8` bit set: values ≥ MAX_CONTROL_KINDS panic (in const context, a
    /// compile error). CH2b defines its kinds as constants.
    pub const fn new(k: u8) -> Self;
}
pub const MAX_CONTROL_KINDS: usize = 8;
/// Payload bound, checked at compile time in `ControlMsg::new`.
pub const CONTROL_PAYLOAD_MAX: usize = 256;

/// Opaque to `dash-chain` (SB0 §6.1). CH2's hub wraps its `Control` in it.
/// `T: Copy` owns no heap, so a message's bytes are 24 B inline plus ≤ CONTROL_PAYLOAD_MAX boxed.
pub struct ControlMsg { kind: ControlKind, payload: Box<dyn Any + Send> }
impl ControlMsg {
    pub fn new<T: Any + Send + Copy>(kind: ControlKind, t: T) -> Self;  // const-asserts size_of::<T>() ≤ 256
    pub fn kind(&self) -> ControlKind;
    pub fn downcast<T: Any>(self) -> Result<T, ControlMsg>;
}
/// `send_control`'s result. `Coalesced`: a message of this kind was outstanding (queued, not yet
/// consumed by `next()`); its payload was replaced by the new one and the kind moved to the back.
pub enum SendControl { Queued, Coalesced, Stale }

/// The only wake type (DEC-152 (2)): notification-only by construction. No caller closure.
#[derive(Clone)]
pub enum Notify {
    /// `std::task::Waker::wake_by_ref`: tokio tasks (SPV subscribers, SB14's header task).
    Task(std::task::Waker),
    /// A permit parker: driver threads (CH2b's events threads, V's service loop, W).
    Thread(Arc<Parker>),
}
impl Notify { pub fn notify(&self); }

/// A permit: `notify` sets it and signals a condvar; `park`/`park_timeout` wait for it and clear it.
/// A notify before the park is not lost. Both critical sections are O(1).
pub struct Parker { permit: Mutex<bool>, cv: Condvar }
impl Parker {
    pub fn new() -> Arc<Self>;
    pub fn park(&self);
    pub fn park_timeout(&self, d: Duration) -> bool;
    /// For service loops that also wait on other sources: register this parker with each.
    pub fn notify(&self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish { Done, Blocked }
```

**Why `Notify` is closed (DEC-152 (2); Sol r1 R1-M2's second route).** rev0's `SubWaker::Fn` ran arbitrary caller code under the tip lock. Such code can do blocking I/O, wait for a response from V, or run a handler inline, and a closure that waits on V deadlocks it even without calling any tip-locking API. `Notify` has no closure variant:
- **`Task`** calls `Waker::wake_by_ref`, which schedules the task, as Core's queued callbacks are scheduled rather than run inline (`scheduler.cpp:176-183`).
- **`Thread`** sets a permit under the parker's own leaf mutex and signals the condvar. Both critical sections are O(1).

An eventfd user (DEC-152 lists it) wraps its fd in a `Waker`. A custom `RawWaker` must be notification-only too. That is a documented contract on `Notify::Task`, and tokio's wakers meet it; the type system cannot check a `RawWaker` vtable.

`FeedConfig` goes in SB4's `ChainConfig` (DEC-151 OQ2):

```rust
pub struct FeedConfig {
    /// Q: data events per subscriber, queue plus staging (F3). Default 1,024; mobile 256 (§6).
    pub capacity: u32,
    pub block_data: BlockData,       // the node: BlocksAndUndo; SPV: None
    /// The feed cap is max_feeds × the per-feed budget (§6.1). Node 16; SPV 8.
    pub max_feeds: u32,
    /// E: events per sub-step of an SPV composite (§4.8). Default = Q.
    pub segment_events: u32,
    /// W: rows per write batch of the SPV header step (§3.7). Default 1,024; mobile 256.
    pub write_batch: u32,
    /// B: headers per SPV header step. SB14 slices each `headers`/`headers2` message (≤ 2,000 /
    /// ≤ 8,000 headers) into steps of ≤ B. Default 2,000 (OQ4). A larger batch is refused with
    /// `ChainError::Config("step exceeds step_headers")` before any work (§6.2).
    pub step_headers: u32,
}
// The cold cache's `cap` (SB5-A6) also sizes A5's invalidation list (§5.2).
```

### 3.2 `TipCell` (SB4 §9.2, extended)

```rust
pub struct TipCell {
    pub(crate) state: Arc<Mutex<TipState>>,          // SB4: the tip lock; `lock_arc` for enforce_block
    pub(crate) published: ArcSwap<ChainReadSnapshot>, // SB4 D6
    pub(crate) shared: Arc<FeedShared>,               // SB5: everything reachable without the tip lock
}
pub(crate) struct TipState {
    tip: BlockRef, next_seq: u64, snapshot: Arc<ChainReadSnapshot>, epoch: u64,   // SB4
    // CH1-A11: the node's crate-private `pre_genesis`; `subscribe_at` returns `Moved` while it is set
    feeds: FeedTable,       // SB5: entries by index (slot Arc, gen, cursor, closing reclaim), free list
    pending: Pending,       // SB5: the events some target still lacks
}
struct TableEntry {
    gen: u32,
    slot: Option<Arc<FeedSlot>>,
    /// The seq of the first event this live target has not received; `None` = caught up.
    cursor: Option<u64>,
}
/// The backlog, in seq order. A segment is one step's event (node) or one sub-step's ≤ E events
/// (SPV, §4.8), held once, whatever the number of targets (§6.1, §6.2).
pub(crate) struct Pending { segs: VecDeque<Segment> }
enum Segment {
    One(ChainEvent),
    /// One SPV sub-step (≤ E events, §4.8): generated by index from SB4's sub-step vectors, shared
    /// with `TipChange` (A9) or copied without A9: disconnects first (tip down), then connects.
    Composite { first: u64, disconnected: Arc<Vec<BlockRef>>, connected: Arc<Vec<BlockRef>> },
}
pub(crate) struct FeedShared {
    registry: Published<Registry>,          // republished under the tip lock on every table change (H2)
    producer_wake: OnceLock<Notify>,
    close_hook: OnceLock<Notify>,           // W's wake (H4); W re-reads the registry
    /// The hold owner's thread id while a hold is open, else 0. Stored by `hold_tip` after the
    /// lock is taken and cleared by the release before the guard drops (§4.3, §4.7).
    held_owner: AtomicU64,
    /// One-shot release wakes (DEC-157 (2)): a leaf mutex, O(1) push; the release takes the list
    /// after dropping the guard and notifies each.
    release_waiters: Mutex<SmallVec<[Notify; 4]>>,
    status: StatusCell,
    fail: Arc<dyn FailStop>,
    cfg: FeedConfig,
}
pub(crate) struct FeedSlot {
    id: FeedId, needs: Needs,
    live: AtomicU64,                  // 0 = inactive; else seq_synced. Stored only under the tip lock
    closing: AtomicBool,              // H7's linearization point: SeqCst, monotone
    inner: Mutex<SlotInner>,          // the leaf lock: every critical section O(1) (§4.5)
}
struct SlotInner {
    queue: VecDeque<ChainEvent>,      // capacity ≥ Q, reserved at `register`; never reallocates
    /// Control (DEC-155 (1)): kinds in first-queued order, one payload per kind. A kind's bit in
    /// `outstanding` is set by `send_control` and cleared only when `next()` returns that message.
    ctl_order: ArrayVec<ControlKind, MAX_CONTROL_KINDS>,
    ctl_payload: [Option<ControlMsg>; MAX_CONTROL_KINDS],
    outstanding: u8,
    used: u32,                        // queued + the reader's staged data events; ≤ Q
    want_space: bool,                 // the producer is waiting on this slot
    waker: Option<Notify>,
}
```

**SB4 change (C0 stub only; DEC-151 OQ2):** `TipCell<E>` and `TipState<E>` lose the type parameter (D1).

**Slot objects are never reused.** A new registration at a reused index gets a new `Arc<FeedSlot>` and a new `gen`, so an old `FeedReader` can never see a later subscriber's events (D3). Cleanup under the tip lock revalidates by `Arc::ptr_eq` before removing anything (§4.3, `deregister` step 4). The index is retired, never reused, once `gen` reaches `u32::MAX`.

### 3.3 The producer API on `HeaderChain`

SB4's tip writers keep their signatures. Each step fans out inside its critical section (SB4 §9.1: node per step, SPV per sub-step, node `enforce_block` per disconnect under the owned guard). A hold's guard lives in a **`Hold` value on the writer's stack**, never in `HeaderChain`, so `HeaderChain` stays `Send` (m5; CH1-A1's shape). SB5 adds:

```rust
impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    /// Push what fits from `pending`, per target from its cursor, in seq order (no hold open:
    /// one critical section of the tip lock).
    pub fn retry_pending(&mut self) -> Publish;
    pub fn pending_is_empty(&self) -> bool;
    /// CH1-A1 / SB5-A12: open a hold. The guard (`lock_arc`) lives in the returned `Hold`, which
    /// mutably borrows the chain, so no other writer call is possible until `release`.
    pub fn hold_tip(&mut self) -> Hold<'_, S, R>;
    /// Set once, before the first `register`. Unset: no wakes (tests poll).
    pub fn set_producer_wake(&self, w: Notify);
    /// The subscriber-side handle (FeedSource, register, deregister, control, registry). Clone + Send + Sync.
    pub fn feeds(&self) -> ChainFeeds;
    pub fn status_feed(&self) -> StatusReader;
    /// SPV stage 3: stream the step's write set into `sink` in batches of ≤ W rows, inside the
    /// store transaction the sink holds open; returns the O(1) tail (§3.7).
    pub fn write_header_step(&mut self, target: Option<BlockRef>, sink: &mut dyn HeaderStepSink)
        -> Result<StepTail, ChainError>;
    /// SPV stage 4 (§4.8): move the tip to `target` in sub-steps of ≤ E events inside one hold,
    /// draining each sub-step's fanout before the next (`wait` parks the writer); one publish at
    /// release. Returns an O(1) summary; no leg vector outlives its sub-step.
    pub fn activate_segmented(&mut self, target: BlockRef, wait: &mut dyn FnMut())
        -> Result<TipSummary, ChainError>;
    /// The step budget's cap, the largest charge seen, and the marks account's peak (§6.2).
    pub fn step_budget(&self) -> StepBudgetStats;
    /// SPV reopen (§8).
    pub fn open(params: Arc<ChainParams>, cfg: ChainConfig, svc: Services, store: S) -> Result<Self, OpenError>;
}
static_assertions::assert_impl_all!(HeaderChain<MemHeaderStore, Evicting>: Send);   // F0 (m5)

/// A hold (CH1-A1): `!Send` (it owns a parking_lot guard), alive only in the writer's frame.
pub struct Hold<'c, S, R> { chain: &'c mut HeaderChain<S, R>, guard: ArcMutexGuard<RawMutex, TipState> }
impl<S: HeaderStore, R: Residency> Hold<'_, S, R> {
    pub fn connect_tip(&mut self, b: BlockId) -> Result<TipStep, ChainError>;     // node (CH1-node §2.5)
    pub fn disconnect_tip(&mut self) -> Result<TipStep, ChainError>;
    pub fn retry_pending(&mut self) -> Publish;                                     // the held guard (§4.7)
    pub fn pending_is_empty(&self) -> bool;
    /// Stage-4 side effects once, the snapshot and status stores, then the last segment's fanout,
    /// then the guard drops and the release wakes fire (§4.8; m2, m9). CH1-node's `release_tip(hold)`.
    pub fn release(self) -> Arc<ChainReadSnapshot>;
    /// After an `Err` with the latch tripped: drop the guard with **no** snapshot or status store (m3).
    pub fn abandon(self);
}
```

CH1-node's pseudo-code writes `chain.connect_tip(..)` and `chain.release_tip(hold)` inside a hold; with this shape they read `hold.connect_tip(..)` and `hold.release()`. Only the spelling differs (SB5-A12). `enforce_block` keeps its owned guard in its own frame in the same way.

**The writer's rule** (SB0 §6.1, F3; CH2 §4.4.1 rule 5): no tip step starts while `pending` is non-empty. An SPV sub-step's events (≤ E, §4.8) form one segment, so `pending` never holds more than one segment. A step that starts with a non-empty `pending` returns `ChainError::Internal("tip step with a pending fanout")`, under a `debug_assert!`.

The writer's loops are:
- **Node V, after every `connect_tip`/`disconnect_tip`:** `while hold.retry_pending() == Blocked { in_hold_service_wait() }`, before anything that could start another step. Every node tip step runs inside a hold: an `ActivateBestChain` inner loop, an `InvalidateBlock` iteration (CH1-node §2.5, CH1-A1 `hold_tip`) or `enforce_block` (SB4 H3). So this drain always runs **through the held composite guard**, between every two steps of the hold (DEC-153 (1); §4.7 gives the mechanics). This is CH2 rule 5's backpressure wait, narrowed inside holds (CH1-node E-SB5-1, CH1-B3): it answers subscribers' `FlushDue`, W's `PruneClears` and W's release and completion wakes, and **no class R or M command**. It wakes on the producer wake, the `Parker` V parks on. This is safe because no subscriber progress needs the tip lock (H4).
- **SPV header writer:** inside `activate_segmented`, between sub-steps, `while hold.retry_pending() == Blocked { wait() }` through the held guard (§4.8). The last sub-step's events are fanned out by the release, after the snapshot store (m9); that segment drains before stage 4 of the next step, `while chain.retry_pending() == Blocked { wait() }`, outside the lock as in rev1. The typical step (d + c ≤ E) is one sub-step and never waits inside the hold. One `Parker` serves both waits on the header writer's dedicated thread (n9; SB5-Y1). Everything that wait depends on obeys SPV-LO9 (§4.9).

### 3.4 Subscribers: `ChainFeeds`, `FeedSource`, `FeedReader`

```rust
#[derive(Clone)]
pub struct ChainFeeds { state: Arc<Mutex<TipState>>, shared: Arc<FeedShared>, reader: ChainReader }

impl ChainFeeds {
    /// P8. Allocates the slot and its three Q-sized buffers before touching the tip lock, then
    /// installs it inactive (no fanout, never a target; F10). **Never blocks on a hold**: while one
    /// is open it returns `HoldActive` (DEC-157 (2); §4.3).
    pub fn register(&self, needs: Needs) -> Result<FeedId, Refused>;
    /// F11. Sets `closing` (SeqCst), notifies the producer and W, then reclaims the entry only if
    /// the tip lock is free (`try_lock`); otherwise the writer's next critical section or the next
    /// `register` reclaims it. Never blocks. The flag alone retires the slot (H4, H7). Stale id: no-op.
    pub fn deregister(&self, id: FeedId);
    /// A one-shot wake when the current hold is released (DEC-157 (2)). Protocol: on `HoldActive`,
    /// `on_release(w)`, retry once, and wait for `w` only if the retry is `HoldActive` again.
    pub fn on_release(&self, w: Notify);
    /// F4: out of band, no tip lock; notifies the subscriber (F5). If a message of `c.kind()` is
    /// outstanding (queued and not yet returned by `next()`), its payload is replaced (DEC-155 (1)).
    pub fn send_control(&self, id: FeedId, c: ControlMsg) -> SendControl;
    /// The outstanding kinds, as a bit set (metrics, tests).
    pub fn control_outstanding(&self, id: FeedId) -> u8;
    /// H2: the lock-free registry snapshot W captures from.
    pub fn registry(&self) -> Arc<Registry>;
    pub fn is_closing(&self, id: FeedId) -> bool;
    /// W's wake on any slot closing (H4). Set once. W re-reads the registry and re-checks its predicate.
    pub fn set_close_hook(&self, w: Notify);
}

pub struct Registry { pub epoch: u64, pub feeds: Vec<RegistryEntry> }
pub struct RegistryEntry { pub id: FeedId, pub needs: Needs, pub seq_synced: Option<u64>, slot: Arc<FeedSlot> }
impl RegistryEntry { pub fn is_closing(&self) -> bool; }   // H7(b): filter after reading the snapshot

pub trait FeedSource<E>: Send + Sync {      // DEC-151 OQ1
    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_>;
    fn subscribe_at(&self, id: FeedId, p: &BlockRef, finish: &mut dyn FnMut() -> Result<(), E>)
        -> Result<Handoff, SubscribeError<E>>;
}
impl<E> FeedSource<E> for ChainFeeds { .. }
pub enum Handoff { Live { feed: Box<dyn SubscriberFeed>, seq_synced: u64 }, Moved }
pub enum SubscribeError<E> { Stale, AlreadyLive, Closing, Latched, HoldActive, WouldDeadlock, Finish(E) }

pub trait ChainSnapshot {                   // CH2's, unchanged
    /// Node: called only after genesis (ND1 starts subscribers after the first ABC, CH1 D8; m6).
    /// Before it, `subscribe_at` returns `Moved` (CH1-A11's `has_tip() == false`).
    fn tip(&self) -> BlockRef;
    fn tip_seq(&self) -> u64;
    fn at_height(&self, h: u32) -> Option<BlockRef>;
}
pub struct FeedSnapshot { snap: Arc<ChainReadSnapshot>, reader: ChainReader }
impl ChainSnapshot for FeedSnapshot { .. }  // §5.3; a cold error trips the latch and returns None
impl FeedSnapshot {
    pub fn at_height_checked(&self, h: u32) -> Result<Option<BlockRef>, ChainError>;
    pub fn range(&self, lo: u32, hi: u32) -> Result<Vec<BlockRef>, ChainError>;
    pub fn read(&self) -> &Arc<ChainReadSnapshot>;
}

/// What the subscriber does next. Control always comes before data (F4).
pub enum Next<'a> { Control(ControlMsg), Event(&'a ChainEvent), Empty }

pub trait SubscriberFeed: Send {
    /// ONE slot critical section: takes the oldest outstanding control message out of the slot
    /// (clearing its kind's flag) and moves newly queued data events into the reader's staging.
    /// Returns that control message if there was one, otherwise peeks the head data event (R1-M1).
    fn next(&mut self) -> Next<'_>;
    /// Removes the head event. Valid only right after `next` returned `Event` (debug assert).
    fn pop(&mut self) -> Option<ChainEvent>;
    /// Compression (CH2 §4.4.1 rule 3) on the reader's staging, after a data-only intake.
    fn remove_round_trip(&mut self, b: &BlockHash) -> Option<(u64, u64)>;
    /// F5. Park protocol: `set_waker`, then `next()` again; park only if it returns `Empty`.
    fn set_waker(&mut self, w: Notify);
    fn is_closed(&self) -> bool;
}
pub struct FeedReader {
    slot: Arc<FeedSlot>, shared: Arc<FeedShared>,
    staging: VecDeque<ChainEvent>,   // capacity ≥ Q
    spare: VecDeque<ChainEvent>,     // capacity ≥ Q, empty between calls
    selected: bool,                  // `next` returned `Event` and no call followed
}
impl Drop for FeedReader { /* sets `closing`, notifies the producer and W; never takes the tip lock (D9) */ }
```

**The reader API changes from rev0 and DEC-151 OQ1 (R1-M1, D24).** `front` and `pop_control` are merged into `next`, so CH2's driver can no longer choose between control and data in two separate calls. DEC-151 accepted `front(&mut self)`, `pop_control` and `is_closed`. rev1 replaces the first two with `next` and keeps `pop`, `remove_round_trip`, `set_waker` and `is_closed`. rev2 keeps the trait and moves control storage into the slot: the reader has no private control buffer, so a message is either outstanding in the slot or consumed (DEC-155 (1); R2-M2). CH2b's driver step becomes:

```rust
match feed.next() {
    Next::Control(c) => handle_control(c),                    // rule 1 reads the hub mirror
    Next::Event(e)   => { /* decide on *e */ feed.pop(); }    // or compress, rule 3
    Next::Empty      => { feed.set_waker(w); if let Next::Empty = feed.next() { park() } }
}
```

### 3.5 Errors

`ChainError` is unchanged; SB5 uses `Storage`, `Internal` and `Config`. New types:

```rust
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("store has no chain")] Empty,                      // caller runs new_anchored / new_genesis
    #[error("schema {0}, expected {1}")] Schema(u32, u32),
    #[error(transparent)] Storage(#[from] StoreError),         // latch tripped
    #[error("corrupt header store: {0}")] Corrupt(Corruption),  // latch tripped; SB15 resyncs from the anchor
    #[error(transparent)] Config(#[from] ConfigError),
}
pub enum Corruption { MissingRecord(BlockId), MissingParent(BlockId), ActiveMismatch { height: u32 },
                      SkipHeight(BlockId), NextIdNotAbove(BlockId), TipNotLeafPath(BlockId), CheckIndex(IndexInconsistency) }
```

### 3.6 `StatusEvent`

```rust
pub enum StatusEvent {
    /// Core `NotifyHeaderTip` (`validation.cpp:3693-3714`): `m_best_header` changed.
    HeaderAccepted { best_header: BlockRef, time: u32 },
    /// Core `NotifyChainLock` (`chainlock/handler.cpp:312-320`): the locked block is new and on the active chain.
    ChainLocked { block: BlockRef, lock: ChainLockRef },
}
pub struct StatusReader { slot: Arc<StatusSlot>, cell: Arc<FeedShared>, seen_version: u64, seen_best: BlockId, seen_cl: Option<BlockId> }
impl StatusReader {
    /// At most two events, coalesced: the latest best header and the latest active lock.
    pub fn next_events(&mut self) -> SmallVec<[StatusEvent; 2]>;
    /// Register-then-recheck (§4.3): true if a change is already visible; otherwise the waker is armed.
    pub fn arm(&mut self, w: Notify) -> bool;
    pub fn poll_changed(&mut self, cx: &mut Context<'_>) -> Poll<()>;
}
impl Drop for StatusReader { /* closes its slot and removes it from the reader list (R1-m1) */ }

struct StatusCell {
    facts: Published<StatusFacts>,           // stored first
    version: AtomicU64,                      // then bumped, SeqCst
    readers: Published<Vec<Arc<StatusSlot>>>,// the writer only loads it
    readers_lock: Mutex<()>,                 // add/remove a reader; the writer never takes it
}
struct StatusSlot { waker: Mutex<Option<Notify>>, closed: AtomicBool }   // leaf, O(1)
```

Each `StatusFacts` holds the best header and its time, and the CL with an `active` flag. The writer stores new facts in the same critical section as the snapshot, but only when they changed, then wakes the readers. It never blocks except on the O(1) per-reader waker mutexes. A reader that is behind sees only the latest facts.

The two events follow Core's deduplication:
- **`HeaderAccepted`** fires when `best_header` differs from the reader's last one, as `pindexHeaderOld` does (`validation.cpp:3696-3706`).
- **`ChainLocked`** fires when the floor block is on the active chain of the published snapshot and differs from the reader's last one. Core requires the same thing: the CL block must be an ancestor of the tip (`chainlock/handler.cpp:299-309`), and `lastNotifyChainLockBlockIndex` dedups the notification (`:312-316`).

**Erratum D38–D39 (F4b).** These number after §13's rev3 rows.
- **Where the facts come from (D38).** `after_publish` derives them from the snapshot it follows, before the fanout (§4.8's release order; pinned by `status_before_fanout`).
  - **The best header.** Its id, hash, height and time come from `TipFacts`.
  - **The floor.** It comes from `cl_floor` (SB5-A17). It is active when two things hold. The snapshot's observed spork state must be on, because Core notifies only from `EnforceBestChainLock`, which returns while the spork is off (`chainlock/handler.cpp:275`). And the view must hold the floor at its height, `active.at(floor.height) == Some(floor.id)`, which is O(1) under the tip lock and reads no index or store.
  - **The SPV edge.** An SPV floor below the view's window reads as inactive. So a floor that leaves the window before a reader has observed it active is never reported to that reader. That happens after one publication that moves the tip far, such as a segmented step, or while a reader lags.
- **Readers before the first publication (D39).**
  - **The gap.** `FeedShared::new` has no snapshot, and SB4's `TipCell::new` publishes the cell's first snapshot without `after_publish`. So until the writer's first status store, the cell holds no facts (`version` 0).
  - **The seed.** `status_feed()` then seeds a new reader from the cell's published snapshot, and re-checks `version` after it. While the version is still 0, the first publication is at most in progress, so its stored facts are no older than the seed. The first publication always bumps, because its best header is `Some`. Today every publication needs `&mut HeaderChain`, so the re-check is defensive.
  - **Why.** Without the seed, a reader attached right after construction would report the unchanged best header at the next publication. This needs no SB4 ask.
  - **Contrast with Core.** Core's `pindexHeaderOld` starts null, so its first `NotifyHeaderTip` after start always fires. A consumer that wants that initial emission reads the snapshot itself.

### 3.7 `HeaderStore` (SB4 §4, extended; SB13 implements)

```rust
pub struct HeaderRecord {
    pub id: BlockId,
    pub hash: BlockHash,        // NEW (DEC-151 OQ2)
    pub height: u32, pub header: BlockHeader, pub skip: Option<BlockId>, pub status: Status,
    pub chain_work: Work, pub time_max: u32, pub kind: RecordKind,
}

pub trait HeaderStore: Send + Sync {
    // SB4's six methods, unchanged: by_hash, by_id, active_at, from_height, leaves, children.
    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError>;
    fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, StoreError>;
    /// `n` records down the prev links from `from`, `from` first. SQLite: one recursive CTE.
    fn walk_down(&self, from: BlockId, n: u32) -> Result<Vec<HeaderRecord>, StoreError> { /* default: by_id loop */ }
    /// The largest stored id (n4): `open` checks `next_id` > it.
    fn max_id(&self) -> Result<Option<BlockId>, StoreError>;
}

pub struct ChainMeta {
    pub schema: u32, pub anchor: (BlockHash, u32), pub anchor_state: AnchorState,
    pub next_id: BlockId, pub tip: BlockId,
    pub best_header: BlockId,   // diagnostic only: `open` recomputes (D17, E5)
    pub sync_seq: u64, pub cl: ClPersisted,
}

/// The header step's WriteSet (SB0 §9.5), streamed (DEC-155 (2)): rows go to the sink in batches
/// of ≤ W from one reused buffer, all inside ONE store transaction the sink opened for the step.
pub enum StepRow {
    Record(HeaderRecord),                    // 1. a new record (inserted in this step)
    Status(BlockId, Status),                 //    a status-only change: each overlay mark (SB5-A13)
    LeafAdd(BlockId), LeafRemove(BlockId),   //    `hdr_leaf` deltas
}
pub trait HeaderStepSink {
    /// Executes `batch` in the open step transaction (SQLite: one BEGIN IMMEDIATE … COMMIT for
    /// the whole step). Called again whenever the buffer fills; never sees a partial row.
    /// Runs synchronously on the header writer's thread (SB0 E8: the leased write connection).
    fn rows(&mut self, batch: &[StepRow]) -> Result<(), StoreError>;
}
/// SB0 E8 (m7): stage 3 leases the store writer's write connection: one message out, the
/// connection back, `BEGIN IMMEDIATE`, the batches and the tail on the header thread, `COMMIT`,
/// the connection returned. Other `WriteSet`s queue meanwhile, as they would behind one long
/// `WriteSet`. Cold reads during stage 3 use the separate read connection (§3.7's read contract).
/// The O(1) tail, executed last, in order.
pub struct StepTail {
    pub active: Option<ActiveDiff>,          // 2. compact: rows rewritten by walking prev in the store
    pub cl: Option<ClPersisted>,             // 3. `locks` rows when the step changed them
    pub meta: MetaDelta,                     // 4. next_id, tip, best_header
}
/// `active` rows after the step: heights from_height ..= new_height are rewritten by walking
/// prev from `new_tip` (SQLite: one recursive CTE); heights above new_height are deleted (D44).
pub struct ActiveDiff { pub from_height: u32, pub new_tip: BlockId, pub new_height: u32, pub old_height: u32 }
```

**`leaves()` is O(branches) (D19).** SB5 adds the `hdr_leaf` table, maintained from each step's deltas. For each new accepted record r, the step adds r and removes `prev(r)` if `prev(r)` was a leaf. Context records are never leaves.

**SB13's read contract (Sol r1 confirmation).** A cache miss after a commit must read the committed row. SB13 must not serve a cold read from a read transaction or prepared-statement snapshot pinned before that commit. SB13's own test covers this (S1).

**`MemHeaderStore`** implements `HeaderStepSink` plus `begin_step()`/`commit_step(&StepTail, Durability)`; a step's rows are invisible until `commit_step`. It also answers `HeaderStore::max_id()` (n4; SQLite: `SELECT max(id) FROM hdr`, one index probe). It keeps two layers, durable and buffered. `power_loss()` drops the buffered layer and **invalidates every outstanding handle**: tests reopen with a fresh `HeaderChain`, `ChainReader` and cache, so no old instance outlives the modelled restart (Sol r1). Fault injection covers:
- fail the n-th read of a kind;
- fail the n-th commit;
- hand back a record with a bad skip or a missing parent.

The fault hooks live in a `#[cfg(any(test, feature = "test-util"))]` `Faults` struct.

**Erratum D44–D46 (F6b).**
- **`ActiveDiff::from_height` (D44).** `fork_height` becomes `from_height`, inclusive. Heights `from_height ..= new_height` are rewritten. Where the store already has rows, `from_height − 1` must be active and the walk must end at its row. A fresh store's first step writes from the lowest accepted active height, the anchor or genesis at 0, with `old_height` 0. `fork_height + 1` could not express genesis. SB13 implements the same.
- **What `write_header_step` writes (D45).**
  - **Records.** New records are the dirty ids above `HeaderStore::max_id()`, one probe per step. They must be every id issued since, and the store must not be ahead of `next_id`; otherwise the step fails `Internal`. SPV's other dirty ids are marks on stored records, and their rows are the overlay's `Status` rows (SB4 D-C4-11; SB5-A13). The records go first, with their leaf deltas, then the status rows. The dirty set and the overlay are read, not drained.
  - **The `active` diff.** It comes from `tip_target(target)` (O(1), SB5-A11 (c)), not the pending plan: `from_height = fork + 1`, `new_height` is the target's, `old_height` the tip's. A target whose height disagrees fails `Internal`, on a fresh store too; an unknown one fails `UnknownBlock`. Under debug, a pending plan from stage 1 must end at the target. `None` means the tip stays, and there is no diff. Stage 4 still runs then, since it applies the overlay to resident groups.
  - **Errors.** A failed probe or sink write trips the latch. A failed `commit_step` is the caller's (the SB13 driver's) to trip.
  - **The tail.** `cl` is `None` until SB4 C6's `cl_state`.
  - **Coverage.**
    - A-33 is covered for the header-batch step, failing mid-stream and at the commit. Its other step kinds, and A-34, are C6's.
    - P3 runs on the real step. P5 needs F7's reopen.
- **The store checks the first diff (D46).** With nothing active, `old_height` must be 0, and the walk must end above no accepted record (its parent is a context record or none). SB4's test helper `spv_tests::commit` writes no `active` rows. SB5's step tests therefore seed their chains through `write_header_step`. Switching that helper over is offered to SB4 rather than made here.

### 3.8 Changes SB5 asks of SB4, SB13, SB14 and CH2b

Labels carry the asking design (DEC-157 (4)): **SB5-A*n*** of SB4, **SB5-S1** of SB13, **SB5-Y*n*** of SB14, **SB5-C1** of CH2b. CH1-node's are **CH1-A*n*** and **CH1-B*n***. Where this document says "A5" or "Y2" without a prefix it means SB5's.

| # | To | Change | Why | Status |
|---|---|---|---|---|
| SB5-A1 | SB4 C0 | `TipCell`/`TipState` lose `<E>` | D1 | DEC-151 accepted; not yet applied at 6ef0a8e75 (trivial) |
| SB5-A2 | SB4 C0/C4 | `HeaderRecord` gains `hash` | `by_id` callers need it | DEC-151 accepted; not yet applied (trivial) |
| SB5-A3 | SB4 C4 | the `Evicting` arena accepts inserts at a given stable id (sparse chunks) | §8 step 3 | DEC-151 accepted; matches `Arena::set(&self, id, …)` |
| SB5-A4 | SB4 C5 | the step recipe calls `feeds.fanout` after `seq` (and after the snapshot store, outside a hold), inside the critical section; every snapshot store also calls `status.publish`. Inside a hold, fanout stays per step while the snapshot and status are stored once at release (CH1-A1, CH1-A13; §4.7). An SPV hold's last segment fans out at release, after the stores (m9) | SB4 §9.1 | F2 lands the call; F3 the hold path |
| SB5-A5 | SB4 C4 | `records_committed` (stage 5) calls `ColdCache::invalidate` with **at most `cold_cache` ids**: the written ids and the overlay's ids. When the step has more, it calls `ColdCache::clear()` instead: O(cap) under the cache mutex, outside the tip lock (DEC-157 (3)) | §5.2 | F5; matches the stub doc of `records_committed` |
| SB5-A6 | SB4 C3/C4 | `ChainConfig` gains `feeds: FeedConfig` and `cold_cache: u32` | §6 | F0 |
| SB5-A7 | SB4 §3.8 | the arena bound is ⌈(N_win + B)/1,024⌉ + 1 + S chunks, and never more than one chunk per resident entry (§6.3) | precision | note for SB4's next rev |
| SB5-A8 | SB4 C4 | **proposal**: when an eviction pass finds a chunk whose only resident entries are off the active chain, evict those entries too. It removes **stale-only pins only**, not pins by sparse **active** ids, so S is retained (§6.3) | shrinks S's stale part | OQ11; SB5 does not depend on it |
| SB5-A9 | SB4 C5 | *optional*: `TipChange.{disconnected, connected}` as `Arc<Vec<BlockRef>>`, shared with the feed backlog | saves 80 B per sub-step event (§6.2) | the step cap is derived for the no-A9 case |
| SB5-A10 | SB4 §3.7, §9.1 | stage 4's "the cold LRU drops the evicted ids" moves to stage 5, after the tip lock is released (for latency: §4.2) | DEC-152, m4 | F5 |
| **SB5-A11** (rev3: minimal) | SB4 C4/C5 | step-path allocations go through `StepBudget`, sized from `size_of` (§6.2): (a) accept results ≤ `size_of::<AcceptOne>()` × B, and a batch over B is refused with `ChainError::Config`; (b) new and status-only rows leave through the step sink in batches of ≤ W (SB5-A13); (c) **beside** the shipped `tip_plan`/`TipPlan`, which the node keeps, add `tip_target(target) -> TipTarget { fork, d, c }` (O(1) legs) and `tip_plan_bounded(sub_target)` with exact-capacity legs ≤ E; (d) an SPV `TipChange` lives only inside its sub-step (`TipChange.snap` is unused by SPV sub-steps); (e) the stage-4 side effects (overlay apply, eviction, the `max_cold_leaf_work` fold, the `residency_epoch` bump) run **once, at `Hold::release`**, immediately before the snapshot store and for the final tip (m2) | DEC-155 (2), DEC-157 (3) | **required; ruled in rev3 (OQ15)** |
| **SB5-A12** (rev3: restated) | SB4 C5/C6 | CH1-A1 is not node-gated, and has m5's shape: `hold_tip(&mut self) -> Hold<'_>` with the guard in the `Hold`, `Hold::{connect_tip, disconnect_tip, retry_pending, release, abandon}` and SPV's in-hold `set_active_tip` to a sub-target; `with_tip` is a method of `Hold` and of `HeaderChain`. `HeaderChain` is `Send` (F0's static assertion). `hold_tip` publishes `held_owner` after taking the lock; the release clears it before dropping the guard | §4.7, §4.8 | **required; ruled in rev3 (OQ15)** |
| **SB5-A13** (new, DEC-157 (3)) | SB4 C4 (amends SB4 §3.7 and D33) | the SPV overlay is **status-only**: `HashMap<BlockId, Status>`, written by marking and every other SPV status change, read first by the writer (D33's precedence) and applied to the resident seqlocked groups at release (SB5-A11 (e)). Each entry leaves at stage 3 as `StepRow::Status(id, status)`. The overlay's keys are the step's dirty marked ids, so there is no separate dirty-id set for marks; the dirty set for new records is the accept results. `take_dirty_records() -> Vec<HeaderRecord>` and "an evicted id's record is loaded" go away for marks: a cold id is marked without loading its record | M3: ~25 B per mark, not ~210 B (§6.2) | **required; ruled in rev3 (OQ15)** |
| SB5-S1 | SB13 | `hdr_leaf`, the expression index on `prev`, `active_from`, `max_id`, the `walk_down` CTE, the read-after-commit contract (§3.7), SQLite evidence for `open` cost (§8), the streaming step (one transaction per header step fed by `HeaderStepSink::rows` batches; `ActiveDiff` as one recursive CTE), **the write-connection lease of stage 3 and a separate read connection** (SB0 E8, m7), and `hdr_prev`'s `substr(header, 5, 32)` holding `prev` in internal byte order, so `children(prev)` binds the hash in that order (n10) | §7, §8, DEC-155, m7 | SB13's design |
| SB5-Y1 | SB14 | the header writer runs on a **dedicated thread** that parks one `Parker` for both its in-hold `wait` and its post-release drain (n9), counted in SB0 §7's thread-count test | §4.8 | OQ14 ruled: dedicated thread |
| **SB5-Y2** (new, DEC-157 (1)) | SB14 | adopt **SPV-LO9** (§4.9): SPV subscriber pop tasks carry the no-wait marker (CH1-node's mechanism: a task-local, propagated by SB14's `spawn`), await no actor reply, `JoinHandle` or header-task response; inbound `headers`/`headers2` reach the header task through a channel the network actor never blocks on (`try_send`; on `Full` it stops reading that peer's headers until the header task signals space, as Core's `fPauseRecv`; never a blocking `send`); and SB14's latch checks before every adapter persistence path (§5.4, n5) | M2 | required for SB14 |
| SB5-C1 | CH2b | `next()`/`Next` replaces `front` + `pop_control` (R1-M1); `Notify` replaces `SubWaker`; one `ControlKind` per `Control` variant, each payload `Copy` and ≤ 256 B, whose meaning tolerates replacement by a newer same-kind message (CH2 rule 1: controls re-read the hub mirror) (DEC-155 (1)), so `FlushCommit.to: Vec<_>` and `RewindRequest.targets: Vec<_>` stay in the mirror and the feed copy carries a mirror generation; CH2b treats `HoldActive` from `register`/`subscribe_at` by waiting on `on_release` **outside** its hub lock, and never holds the hub lock across any `ChainFeeds` wait (§4.2 lock order). The node's subscribers have no cold reads, so §5.4's latch check is SB14's (n5) | D24, D25, D29, D32 | CH2b's design |

## 4. Concurrency model

### 4.1 Actors

| Actor | Node | SPV |
|---|---|---|
| Tip writer (one) | V (CH1-node), with `&mut HeaderChain` | the header task (SB14) |
| Subscribers | CH2b drivers on their events threads | the cfheaders, filters, blocks, wallet and `dash-mnlist` tasks (tokio) |
| W | CH2's flush writer | — (SPV subscribers commit their own `WriteSet`s, SB0 §9.5) |
| Readers | P2a's actor, RPC, CH2's catch-up (`FeedSnapshot`) | the same through `ChainReader` |
| Hub | CH2b's `SubscriberHub` | SB14's driver |

### 4.2 Locks and atomics

| Name | Kind | Guards | Taken by | Critical sections | May block on |
|---|---|---|---|---|---|
| tip lock | `parking_lot::Mutex<TipState>` in an `Arc` (`lock_arc` for holds and `enforce_block`) | tip, `next_seq`, snapshot, epoch, the feed table and cursors, `pending` | the tip writer (steps, sub-steps, `retry_pending`, holds); **`try_lock` only** from `register`, `subscribe_at` (with `finish()`, DEC-96) and `deregister`'s step 4 (DEC-157 (2)) | a whole step: snapshot store, fanout, status publish, wakes. A hold (V's `hold_tip`, `enforce_block`, SPV's `activate_segmented`): every step of the hold, the per-step drains and the in-hold waits, with the snapshot stored at release (§4.7, §4.8, SB4 §13.4) | (i) the O(1) leaves below; (ii) the holder's own I/O and lock chains whose holders never wait for the tip lock (cache → store; SB4's index locks; V's block I/O); (iii) the in-hold service wait. The O(1) rule binds the fanout section and everything a subscriber's progress needs (m4, E6a) |
| slot lock | `parking_lot::Mutex<SlotInner>`, one per slot | queue, control kinds and payloads, `outstanding`, `used`, `want_space`, waker | producer push, reader `next`/intake/pop/compress decrement, `send_control`, `set_waker`, `control_outstanding` | **each O(1)**, §4.5 | nothing |
| parker lock | `Parker`'s `Mutex<bool>` + `Condvar` | the permit | `notify` (anyone), `park` (owner) | O(1); `park`'s wait releases it | nothing |
| status slot lock | `Mutex<Option<Notify>>` per status reader | that reader's waker | writer (read and notify), reader (arm) | O(1) | nothing |
| status list lock | `Mutex<()>` | adding and removing status readers | `status_feed()`, `StatusReader::drop`; **never the writer** | O(readers), off the writer's path | nothing |
| release waiters | `Mutex<SmallVec<[Notify; 4]>>` | the `on_release` wakes | `on_release` (push), the release (`mem::take` after the guard drops) | O(1) | nothing |
| `closing` | `AtomicBool`, SeqCst, monotone | retirement (H7) | stored by `deregister` and `FeedReader::drop`; loaded everywhere | — | — |
| `live` | `AtomicU64` | 0 or `seq_synced` | stored under the tip lock; loaded lock-free | — | — |
| registry, `published`, status facts and list | `ArcSwap` (via `Published`) | snapshots | stored under the tip lock (status list: under the status list lock); loaded anywhere | — | — |
| status `version` | `AtomicU64`, SeqCst | the publication count | bumped after the facts are stored | — | — |
| cold cache | `parking_lot::Mutex<ColdCache>` | the cold records cache | cold readers; the writer outside the tip lock (stage 5, A10) and inside a hold (§4.8's `tip_target`, sub-target legs) | one store read, O(cap) on an all-referenced CLOCK sweep, or `clear()` (A5) | the store's read connection |
| SB4's own | the hash index's `RwLock`, the chunk guards, the seqlocks | SB4 §13 | — | — | — |

**Lock order:** tip lock → {CH2b's hub lock | slot | parker | status slot}, and tip lock → cache → store read connection. The tip critical section of an ordinary step takes no cache mutex (A10, for latency); a hold may (m4). The status list lock and the release-waiters lock are independent leaves. No leaf nests another leaf: a slot-lock owner never calls `notify` under the slot lock, but clones the `Notify` (O(1)) and notifies after release.

**No `ChainFeeds` call blocks on the tip lock** (DEC-157 (2)). They use `try_lock` and return `HoldActive` while a hold is open, and outside a hold they retry across an ordinary O(fanout) section. So no thread, whatever locks it holds (CH2b's hub lock included), can wait on a hold that waits on it: Opus high's three M1 cases (a shared events thread, a 2-worker SPV runtime, the hub lock) cannot form. CH2b still never waits on `on_release` while holding its hub lock (SB5-C1).

**No callback runs under any SB5 lock.** The only foreign code is `Notify::notify`, which is notification-only by type (§3.1): it runs after the slot lock is released, possibly under the tip lock.

### 4.3 The protocols

**Fanout** (inside the step's critical section, after `seq` and, outside a hold, the snapshot store; SB4 §9.1. Inside a V or SPV hold the snapshot waits for the release, §4.7, §4.8). It takes the step's event e, or a sub-step's ordered events e₁…eₙ with n ≤ E.
1. Targets are the table entries whose slot is live (`live ≤ seq`) and not closing, at this moment (X28).
2. For a target with no cursor: push events in order while `try_push` succeeds. At the first failure, set its cursor to that event's `seq`.
3. A target that already has a cursor receives nothing directly: its earlier backlog comes first (F1).
4. If any cursor now points into the new events, append one `Segment` to `pending` holding them once.
5. Notify each target that received events. The `Notify` was cloned under the slot lock and is called after release.

**`try_push(e)`** takes the slot lock, an O(1) section:
- if `closing`, it returns `Closed`;
- if `used < Q`, it pushes into `queue`, which has reserved capacity and so never reallocates, sets `used += 1`, clones the waker and returns `Pushed`;
- otherwise it sets `want_space = true` and returns `Full`.

**`retry_pending`** visits each table entry with a cursor:
- a closing slot has its cursor cleared (F11);
- otherwise it pushes events from its cursor in `seq` order while `try_push` succeeds, advancing the cursor, and clears the cursor when it passes the last pending event.

Then it drops the leading segments below the minimum cursor and returns `Done` iff no cursor remains. Per-target order holds without a cross-target `blocked` set, because each target's position is its own cursor (D5).

**Data intake** (inside an O(1) slot section): if `queue` is non-empty, when `staging` is empty `swap(queue, staging)`; otherwise `swap(queue, spare)` and remember to append. After release the reader appends with `staging.append(&mut spare)`: a move of at most Q items into reserved capacity, after which `spare` is empty with its capacity kept. `used` is unchanged by intake, because staged items still count.

**`next()`** is one O(1) slot critical section:
1. If `ctl_order` is non-empty: remove its first kind k (≤ 8 entries), take `ctl_payload[k]`, clear bit k of `outstanding`.
2. Data intake, as above.

After release: if step 1 took a message, return `Control(msg)`; else peek the head of `staging` (`Event`, setting `selected`) or return `Empty`. A message is **consumed** when `next()` returns it, and only then is its kind free again (DEC-155 (1)). No control message is ever held by the reader between calls.

**`pop()`** removes the staging head. Then, in an O(1) slot section, it does `used -= 1` and `w = take(want_space)`. After release, if `w` was set, it notifies the producer.

**`remove_round_trip(b)`** runs a data-only intake (control stays in the slot for the next `next()`). It requires the staging head to be `Connected(b)`, finds the first later `Disconnected(b)` (CH2 §4.4.1 rule 3) and removes both, outside the lock. Then, in an O(1) section, it does `used -= 2` and takes `want_space`, and notifies the producer after release if needed. Queue plus staging is at most Q, so an intake sees every queued event.

**`send_control(id, c)`** (DEC-155 (1)).
1. Resolve the id through the registry (no tip lock). A stale or closing id returns `Stale`. The payload was boxed by the sender, outside any lock.
2. In an O(1) slot section, with k = `c.kind()`:
   - if bit k of `outstanding` is set: `old = replace(ctl_payload[k], c)`, and k moves to the back of `ctl_order` (O(8); **coalesced**, n1);
   - else: push k on `ctl_order`, `ctl_payload[k] = c`, set bit k (**queued**);
   - clone the waker.
3. After release: drop `old` (a free never happens inside the section; Sol r2's constraint), then notify the subscriber.

So per slot at most `MAX_CONTROL_KINDS` messages exist that `next()` has not returned, **by construction**, whatever the interleaving of sends and reads. Sol r2's interleaving (V queues a `FlushCommit` + `ChainStateFlushed` pair between every two reader calls) holds at most 2 (Appendix B).

**Coalescing is replacement.** The newer payload of a kind supersedes the queued one, which is never delivered. This is sound for CH2's controls because a control is a notification to re-read the hub mirror (CH2 §4.4.1 rule 1), so the latest one carries everything the dropped one did. CH2b assigns kinds so that this holds (SB5-C1). Because a coalesced kind moves to the back, the delivered sequence is always a subsequence of the send order: A₁, B₁, A₂ delivers B₁, A₂ (n1). F4's proof is unchanged, since the kind stays in `ctl_order` from its first send until consumed. The "+1 being handled" of §6.1 is a driver contract: the driver handles a returned `ControlMsg` before its next `next()` and never stashes it (n2).

**`set_waker(w)`** stores `w` in an O(1) section and drops the previous waker after release (no free inside the section).

**Taking the tip lock from `ChainFeeds`** (DEC-157 (2); Opus high M1). `register` and `subscribe_at` acquire it by this loop, never by a blocking `lock()`:
1. `try_lock()`. On success, run the operation's critical section.
2. On failure, read `held_owner`. If it is this thread, return `WouldDeadlock`, in every build. If it is non-zero, return `HoldActive`. If it is zero, the holder is in an ordinary O(fanout) step section: yield and go to 1. A hold that opens meanwhile is seen on the next pass.

**Waiting out a hold.** On `HoldActive` the caller runs `on_release(w)`, retries once, and waits for `w` only if the retry returns `HoldActive` again. The release clears `held_owner`, drops the guard, then takes the waiter list and notifies each, so a waiter registered before the retry is either served by the retry or woken (loom `m1_hold_active_release_wake`; without the retry the wake is lost: `m1_neg_hold_active_no_recheck`). An async caller awaits a `Notify::Task` built from its `Waker`; CH2b's driver parks its `Parker`. Neither holds CH2b's hub lock while waiting (SB5-C1). The release wake is a wait on the hold's owner, so a caller carrying the no-wait marker (LO-9, SPV-LO9) never waits on `w` alone: it returns to its loop and selects on `w` beside its pops, acks and `FlushDue` (CH1-node rev3.1 §2.6, CH1-B2).

**`register`.**
1. Check the policy: the latch has not tripped (`Latched`), and `needs` asks for nothing `block_data = None` cannot supply (`NoBlockData`, `NoUndo`).
2. Allocate the slot and three `VecDeque`s with capacity Q (queue, staging, spare), **before** the tip lock.
3. Take the tip lock as above. Then: reclaim closing entries (each by `ptr_eq`); take an index from the free list with `gen + 1` (`TableFull` past `max_feeds`); install the slot with `live = 0`; republish the registry.

**`subscribe_at(id, p, finish)`** takes the tip lock as above. A stale id gives `Stale`, a closing slot gives `Closing`, and a live one gives `AlreadyLive`. If `p != state.tip` (id and hash), or the node is pre-genesis (CH1-A11), it returns `Moved`. Otherwise it calls `finish()`; on `Err(e)` it returns `Finish(e)` with nothing changed. On success it stores `live = next_seq` with no cursor, republishes the registry and returns `Live { feed: Box::new(FeedReader::new(slot, buffers)), seq_synced: next_seq }`.

**`deregister(id)`.**
1. Resolve the id lock-free through the registry. A stale id is a no-op.
2. `closing.store(true, SeqCst)`. This is the H7 linearization point, and it is idempotent.
3. Notify the producer, then the close hook.
4. `try_lock()` the tip lock. On success: if the table entry at `id.index` still holds **this** slot (`gen == id.gen` and `Arc::ptr_eq`), clear its cursor, remove the entry, free the index (unless `gen == u32::MAX`) and republish the registry; otherwise do nothing (a lazy reclaim already removed it, and the index may now hold another subscriber: Sol r1's delayed-cleanup interleaving). On failure, return: the entry is **reclaimed lazily**.

**Lazy reclaim.** Every writer critical section that already scans the table (fanout, `retry_pending`, the release) and every `register` removes the entries whose slot is closing, by the same `ptr_eq` rule, in O(`max_feeds`). F11 does not depend on it: `closing` alone stops every push and every wait (§4.4). Only the entry's memory waits, and a closing entry is reclaimed before `register` can report `TableFull`.

`FeedReader::drop` does steps 2–3 only.

**The writer's wait** loops `retry_pending()` and parks on its `Parker` (node) or `Notify` (SPV) on `Blocked`. It cannot lose a wake. The `want_space = true` that a failed `try_push` stores is in the same slot lock order as the reader's later `take(want_space)`, which then notifies. A notify before the park leaves a permit (the `space_wake_*` probe, Appendix B). A deregistration wake follows `closing = true`, so the next `retry_pending` clears that cursor.

**Status: publish and arm (R1-m1).**
- **Writer** (under the tip lock, only when the facts changed). The order is **snapshot store → `facts.store` → `version` bump → wakes** (n7; N1 for `StatusEvent`):
  1. `facts.store(new)`, after the same critical section's snapshot store;
  2. `version.fetch_add(1, SeqCst)`;
  3. for each slot in `readers.load()` that is not closed, read its waker in an O(1) section, then notify it after release.
- **Reader `arm(w)`:**
  1. `v = version.load(SeqCst)`; if `v ≠ seen_version`, return true;
  2. store `w` in its slot in an O(1) section;
  3. re-load `version`; return `true` iff it changed.

  `next_events` sets `seen_version = version.load()` first, then loads `facts` and diffs them against `seen_best` and `seen_cl`. A fact newer than `seen_version` is therefore reported now and not again, because events are diffs, not counts.
- **No lost wake.** Take a reader that armed and found no change, and a writer bump.
  - If the writer's slot section comes after the reader's, the writer reads the new waker and notifies.
  - Otherwise the writer's bump (step 2) precedes its slot section, which precedes the reader's slot section, which precedes the reader's re-load (step 3). The re-load then sees the bump.

  The loom probe confirms both orders, and the no-recheck negative control fails (Appendix B).
- **`StatusReader::drop`** sets `closed` and, under the status list lock, republishes the list without the slot. A dropped reader therefore leaves no waker behind.

### 4.4 Ordering arguments, F1–F11

**F1, ordering from the cut, no loss, no duplication.**
- **Order.** Only the tip writer assigns `seq` and fans out, inside the tip-lock critical section, in tip-change order. A target with a cursor receives nothing directly (fanout step 3), and `retry_pending` pushes from the cursor upward in `seq` order. So each queue receives its events in `seq` order. Intake preserves order: swap, then append. Compression removes a matched pair, which CH2 allows.
- **No loss.** A cursor advances only past events pushed to that target, and is cleared only when the target is caught up or closing (F11). A segment is dropped only below the minimum cursor.
- **No duplication.** Each push advances the cursor past the event.
- **Exactly from the cut.** `live = next_seq` is stored under the tip lock with no cursor. An event with `seq ≥ seq_synced` is assigned later and targets the slot. An event with `seq < seq_synced` was fanned out before the slot was live, and the new slot has no cursor into it, so it never reaches the slot, even while still pending for others (X28).

**F2, `seq` at the tip change.** SB4's `step` assigns `seq = next_seq++` before fanout, under the lock, and stores `tip_seq` in the snapshot in the same critical section. Inside a V hold each step still gets its own `seq` and its own fanout, and the snapshot stored at release carries the last step's `seq` (§4.7); no `subscribe_at` can observe the gap, because it waits out the hold (F10). `FlushCommit`'s pair comes from the `FreezePacket` (SB0 E4).

**F3, bounded data queues.**
- **The bound.** `used ≤ Q` holds for every slot. Only `try_push` increments `used`, under the slot lock, and only when `used < Q`. The reader decrements only events that leave staging. Control is not counted.
- **A full target.** It gets a cursor. The writer waits:
  - outside the tip lock (V's service loop; the SPV task awaits its wake);
  - or, inside `enforce_block`, under the guard, which is sound by H4.
- **The writer's blocking.** Inside the critical section the writer blocks only on slot, parker and status-slot mutexes, all O(1) (§4.5).
- **When the wait ends.** It ends when every target has caught up or closed.

**F4, control before data (R1-M1).**
- **Claim.** Let C be a control message and E a data event for the same slot, with C's push before E's push in the slot lock's order. Then `next()` never returns `Event(E)` while C has not been returned.
- **Proof (rev2: controls stay in the slot).**
  - Suppose call N of `next()` returns `Event(E)`. Then in N's slot section `ctl_order` was empty, or N would have returned a control. E was pushed before N's section, because E is in `staging` when N looks.
  - C's push precedes E's push, so it precedes N's section. C (or a newer same-kind payload that replaced it, which carries C's notification) was therefore in `ctl_order` at N's section unless an earlier `next()` returned it. It was not in `ctl_order`, so an earlier call returned it.
  - Compression's data-only intake moves no control, so it cannot hide C from the next `next()`.
- **Sol's witness.**
  1. S calls `next()` (empty);
  2. H calls `send_control(C)`;
  3. V pushes E;
  4. S calls `next()`: its single section takes C out of the slot and E into staging. It returns `Control(C)`, and the next call returns `Event(E)`.

  The loom probe runs this with the real two-call boundary, and the rev0 split protocol fails as the negative control (Appendix B).
- **`pop` and `remove_round_trip`.** `pop` follows the `next()` that selected its event. A control message pushed after that selection is later than the event's observation, which F4 allows. `remove_round_trip` processes no event and takes no control.
- **The mirror.** CH2's hub mirror is CH2's and unchanged.

**F5, wake on enqueue.**
- **Waking.** Every data push and every control push clones the slot's `Notify` under the slot lock and notifies after release.
- **The park protocol.** `set_waker`, then `next()` again, then park on `Empty`. Both the store and the intake are slot critical sections. Either the push precedes the re-check's intake, which then sees the item, or the waker store precedes the push, which then reads the new waker.
- **The other direction.** The producer is woken on space (`pop` and `remove_round_trip` with `want_space`) and on deregistration (`closing`).

**F6, weak fairness.** SB5 runs no threads. A parked subscriber holds only its `Notify`. Fairness is the host's: CH2's events threads, tokio. Loom's results are safety results for the modelled schedules, not fairness or latency proofs (§10.2).

**F7.** CH2's. SB5 supplies the registry snapshot and `closing` (H2, H7).

**F8, data and ancestry (producer part).**
- **The snapshot.** `FeedSnapshot` wraps one `Arc<ChainReadSnapshot>`.
- **`at_height`** answers resident heights from the snapshot's own view. Lower heights come by ancestry from the view's lowest entry, over immutable fields (§5.3).
- **Delivered ids.** In SPV a delivered event's id resolves cold if it was evicted. The read succeeds on a healthy store, because the header step commits before stage 4 publishes (SB0 §9.5).
- **Events before publication (m9).** Inside a hold, an event can arrive before the snapshot that contains it: every node step in V's holds, and every intermediate SPV sub-step. A consumer that combines an event with a snapshot therefore first waits for `snapshot.tip_seq ≥ event.seq` (the status wake or the producer's next publish), or resolves the event's block by id through `IndexAccess`, whose fields are immutable. It never decides "is b still active?" from an older snapshot. CH1-node fences node consumers the same way (its §6.2). A single-segment SPV step, the typical one, fans out only after the release's snapshot store, as in rev1.
- **Block and undo data** do not exist in SPV (`Refused`, P8). In the node they are CH2's.

**F9.** CH2's.

**F10, the handoff.**
- **Atomicity.** `subscribe_at` holds the tip lock across the check `p == tip`, `finish()` and `live = next_seq`.
- **On `Moved`,** nothing changes.
- **During a hold** (V's `hold_tip`, `enforce_block`, SPV's `activate_segmented`; CH1-B2). `subscribe_at` and `register` return `HoldActive` and the caller retries after the release wake, so the handoff still happens only outside holds, as Core's `NextSyncBlock` check and `m_synced = true` run under `cs_main` (`index/base.cpp:163-170`). Nobody blocks on the hold (DEC-157 (2)).

**F11, deregistration is final.**
- **Nothing more is enqueued.** Once `deregister` returns, `closing` is set: fanout skips the slot and `try_push` refuses it, so a fanout racing with step 2 pushes at most what it pushed before the store. The table entry is removed by step 4 or by the next lazy reclaim (§4.3); only its memory waits.
- **No wait targets it.** From the `closing` store on, `retry_pending` clears that cursor, and the producer wake fires.
- **Stale ids.** A stale `FeedId` fails the `gen` check, and cleanup revalidates by pointer.

**H-rows (SB4 §13.4), SB5's parts.**
- **H2:** the registry is republished lock-free on every table change.
- **H4:** the subscriber-side operations take only O(1) leaf locks: `next`, `pop`, `remove_round_trip`, `set_waker`, the space wake, and `deregister` (whose step 4 is a `try_lock`).
- **H6's companion, CH1-B4 (LO-9):** a node subscriber's pop/ack path never awaits `ActorReply`, `JoinHandle` or `signals.sync()`, and runs with the no-wait marker; SB5's threaded tests spawn subscribers through the marked spawn. SPV's analogue is SPV-LO9 (§4.9).
- **H7:** the `closing` store is the single linearization point. `close_hook` notifies W after the store, so W's register-then-recheck predicate (H7(a)) sees it.

### 4.5 The producer's blocking contract (DEC-152; R1-M2)

**The contract (SB0 §6.3 P2 as amended by E6 and E6a).** In the fanout section, and on every path a subscriber's progress needs, the writer blocks only on leaf mutexes whose every critical section is O(1), and it calls no code but notification-only wakes. Across a hold it may also do its own I/O (cache → store, V's block I/O) and run its in-hold service wait; neither is something a subscriber's progress waits on (§4.2). Core's signal enqueue has the same shape: `ENQUEUE_AND_LOG_EVENT` runs under `cs_main` (`validationinterface.cpp:180-188`) and calls `AddToProcessQueue`, which takes the leaf `m_callbacks_mutex` for one `emplace_back` (`scheduler.cpp:176-183`). The scheduler thread holds that mutex only for O(1) `front`/`pop_front` and flag updates (`:145-155`, `:167-170`).

**Every critical section on the writer's path, and its cost:**

| Lock | Critical section | Cost |
|---|---|---|
| slot | producer `try_push` | one `closing` load, a compare, one `push_back` into reserved capacity, one `Notify` clone |
| slot | reader `next` / intake | at most one `ArrayVec::remove(0)` over ≤ 8 kinds, one `Option::take`, one bit clear; two `VecDeque` header swaps (`mem::swap` of 32-byte headers, no element moves) |
| slot | `pop` / `remove_round_trip` decrement | one subtraction, one `take` |
| slot | `send_control` | one bit test; either one `mem::replace` of a payload pointer (coalesce) or one `ArrayVec::push` and a store; one `Notify` clone. The replaced payload is freed after release |
| slot | `set_waker`, `control_outstanding` | one store (old waker freed after release) or load |
| parker | `notify` / `park` | set or clear a `bool`, signal (the wait releases the mutex) |
| status slot | writer read / reader arm | one `Option<Notify>` clone or store |

No section allocates, frees or moves elements. All three event buffers are reserved before the tip lock (`register`). Control payloads are boxed by the sender before the section; a replaced payload or waker is freed after it. The reader's O(Q) work (`append`, the compression search) runs outside the lock.

**What the writer can wait for, as Core can:** one O(1) section of the current holder, plus that thread's scheduling. A reader preempted inside its intake delays V by its time to be scheduled again and finish two swaps. It cannot delay V by I/O, by a response wait, or by Q.

**Why there is no `try_lock` fallback (DEC-152 (3)).** A `try_lock` that turned a contended slot into a pending target would need its own race-free unlock notification, which adds a second protocol to model. Under the O(1) contract it buys nothing that Core's design does not already accept.

**Evidence.**
- **Loom.** L1 and L9 (§10.2) preempt the reader inside each slot section while the producer fans out. The producer always completes, and the lost-permit negative control deadlocks (Appendix B).
- **Directed test.** `v_resumes_after_paused_slot_holder` uses the `fault.rs` pause point inside the reader's intake. While the reader is paused, V's step blocks on the slot. When the pause is released, V completes the step and returns to its service loop, which answers a queued `FlushDue` probe. V's measured wait equals the pause, and V never reports a spurious `Blocked`.
- **Benchmark.** `feed-publish-contended` (§11) measures the contended path.

### 4.6 Against Core

| Core (v24) | SB5 | Difference and reason |
|---|---|---|
| `MainSignalsImpl` evaluates its subscriber list at delivery (`Iterate`, `validationinterface.cpp:81-92`); `Register`/`Unregister` take only `m_mutex` (`:50-66`) | targets captured at `seq` assignment; registration and the cut under the tip lock | CH2's F10 cut is stricter, and was approved as such (X27/X28). Core's index ignores events while `!m_synced` (`index/base.cpp:264`, `:310`); here an inactive slot is never a target, so that ignore branch is unreachable |
| enqueue under `cs_main` into one serial queue behind the leaf `m_callbacks_mutex` (`scheduler.cpp:176-183`), drained by one scheduler thread (`:145-155`) | enqueue under the tip lock into per-subscriber queues behind O(1) leaf slot mutexes (DEC-152) | the same blocking contract; a slow subscriber delays only itself, until its Q is full |
| backpressure: `LimitValidationInterfaceQueue` syncs when more than 10 callbacks are pending (`validation.cpp:3716-3722`), called in `ActivateBestChain` (`:3760`) and `InvalidateBlock` (`:3940`), outside `cs_main` | the tip feed: V services pending fanout after every tip step, inside the hold too (DEC-153 (1)) | the tip feed is not Core's callback queue. Core's callbacks and their limiter belong to CH1-node's separate validation-signal dispatcher (DEC-153 (2)); the tip feed's bound is per subscriber, in events |
| `Unregister` with refcounted list entries (`validationinterface.cpp:58-66`, `:85-90`); `BaseIndex::Stop` unregisters, then joins (`index/base.cpp:419-425`) | `closing` first, then the lock | a subscriber in flight finishes its current event, and nothing new reaches it |
| `NotifyHeaderTip` outside `cs_main`, deduplicated by `pindexHeaderOld` (`validation.cpp:3693-3714`) | `StatusEvent::HeaderAccepted`, coalescing per reader, notification-only wake | Core calls its UI handlers synchronously outside `cs_main` (`:3708-3712`); SB5 only notifies, and readers run on their own threads |
| `NotifyChainLock` deduplicated by `lastNotifyChainLockBlockIndex`, only when the locked block is active (`chainlock/handler.cpp:299-320`) | `StatusEvent::ChainLocked`, the same two conditions, per reader | per-reader dedup; a slow reader sees only the latest lock |

### 4.7 V's holds: per-step drains through the held guard (DEC-153; CH1-B1–B3)

DEC-153 (1) and CH1-node Q1: V holds the tip mutex across a whole `ActivateBestChain` inner loop (`validation.cpp:3763-3827`), across each `InvalidateBlock` iteration (`:3936-4004`), and across `enforce_block`. Inside one hold V takes several tip steps. **Between every two of them V drains the pending fanout of the first**, through the guard it already holds, with no class R or M command serviced in between. SB5 states this as its node writer rule, and F3 (the commit) implements it for all three hold kinds.

**The held guard (rev3: in a `Hold` on V's stack; m5).** SB4's tip mutex is `Arc<parking_lot::Mutex<TipState>>`. A hold owns an `ArcMutexGuard` (`lock_arc`) inside the `Hold` value that `hold_tip` returns (§3.3). The guard is `!Send` (dash-chain enables parking_lot's `arc_lock` without `send_guard`), so it must not be a field of `HeaderChain`: rev2's `held: Option<Held>` made `HeaderChain` itself `!Send` (Opus high's compile probe), which ND1, SB15 and SB14 cannot accept. With the guard in `Hold<'_>`, only the writer's frame is `!Send`:

```rust
pub struct Hold<'c, S, R> { chain: &'c mut HeaderChain<S, R>, guard: ArcMutexGuard<RawMutex, TipState> }
impl<S, R> Hold<'_, S, R> {
    /// The writer's ONLY route to `TipState` while a hold is open. `HeaderChain::with_tip`, its
    /// sibling, takes the lock for one critical section and exists only when no hold is open,
    /// which the `&mut` borrow guarantees. parking_lot's mutex is not reentrant.
    fn with_tip<T>(&mut self, f: impl FnOnce(&mut TipState, &FeedShared) -> T) -> T { f(&mut self.guard, &self.chain.tip.shared) }
    pub fn retry_pending(&mut self) -> Publish { self.with_tip(|st, sh| st.retry_pending(sh)) }
}
```

- **Every writer path goes through `with_tip`.** That covers each step (`connect_tip`, `disconnect_tip`, the SPV sub-steps), `retry_pending` and `pending_is_empty`. So inside a hold, `retry_pending` reads and advances the cursors, pushes to slots and drops segments on the same `TipState` the steps mutate. It never releases or re-acquires the mutex.
- **Re-locking under a hold is impossible from the writer, and refused from `ChainFeeds`.** `Hold` borrows the chain mutably, so the writer cannot call a `HeaderChain` method that locks. `ChainFeeds`' `register` and `subscribe_at` on the owner's thread return `WouldDeadlock` in every build (`held_owner`, §4.3), and `deregister` never blocks. The loom negative control `hold_neg_relock_tip` still shows the deadlock a re-lock would cause (Appendix B).
- **What a hold defers.** The snapshot and status stores are deferred to the release, which publishes once (CH1-A1, CH1-A13). Fanout is not deferred for node steps: each step's event is assigned its `seq` and fanned out in the step, so subscribers keep receiving every step while snapshot readers see the pre-hold state, as Core's `cs_main` readers do. Consumers fence by `tip_seq` (§4.4 F8).
- **The writer rule holds inside holds.** No step starts with `pending` non-empty (§3.3). In the node, `pending` is therefore at most one step's event, `Segment::One`, at any instant (§6.1).

**V's loop inside a hold** (CH1-node §2.5, §4.1, §4.5; SB5 provides `retry_pending`, CH1-node provides the rest):

```text
let mut hold = chain.hold_tip()               // the guard lives in `hold`, on V's stack
loop over Core's statements {
    hold.disconnect_tip(..) | hold.connect_tip(..)     // seq, fanout (with_tip → held guard)
    [freeze_after_step + CRITICAL / -dbcrashratio waits: each is in_hold_service_wait()]
    while hold.retry_pending() == Blocked {   // with_tip → held guard; no unlock
        in_hold_service_wait()                // park V's Parker; on wake: FlushDue, PruneClears,
    }                                         //   W release/completion; NOT class R/M commands
}                                             // only now may the next step run
hold.release()                                // snapshot + status stored once; guard dropped;
                                              //   release wakes (on_release) fire
```

`in_hold_service_wait()` parks V's `Parker`. On each wake it does one round of:
- answering `FlushDue` (CH2 §4.2, with CH1-node §7.1's latch);
- running `PruneClears`, a status-only write under the index lock, never the tip lock (H3);
- consuming W's release and completion notices.

Class R and M commands stay queued until the release point (CH1-node §2.4, E-SB5-1). Every wake source is notification-only (DEC-152):
- a subscriber's `pop` with `want_space`;
- `deregister` steps 1–3 and `FeedReader::drop`;
- W's acks;
- the arrival of a `FlushDue` or `PruneClears`.

A wake that arrives before the park leaves a permit, so none is lost.

**Why every in-hold wait ends** (SB4 H1–H7; CH1-node §2.6). Each thing V can be waiting for makes progress without the tip mutex:
- **A subscriber.** Its progress (`next`, `pop`, compression, `set_waker`) takes only its O(1) slot lock (H4). Its own wait on V (a `FlushDue` answer) is served by the in-hold wait. A subscriber that closes instead retires through `closing` alone (H7), and `retry_pending` clears its cursor.
- **W.** Its freeze-to-release path takes no tip mutex (H1). W's completion under `-dbcrashratio` may need a subscriber's ack of the step just taken, and every in-hold wait, the completion wait included, runs `retry_pending` first (CH1-node §7.1).
- **Nobody else waits for the hold** (rev3, DEC-157 (2)). `register` and `subscribe_at` return `HoldActive` and wait for the release wake without holding anything V needs; `deregister` never blocks. rev2 let them block on the tip mutex, which deadlocks when their caller is what V waits for (Opus high M1: a shared events thread, a 2-worker SPV runtime, CH2b's hub lock). CH1-B2's "wait out the hold" is kept as a protocol, not a blocking call.

**DEC-153's case, worked** (Q = 1, an ABC hold with two consecutive disconnects, subscriber S parked on its own `FlushDue`):
1. V takes the hold. `disconnect_tip(d₁)` assigns `seq` q+1 and fanout pushes `Disconnected(d₁)`, filling S's queue (`used = 1`). The drain returns `Done`.
2. `disconnect_tip(d₂)` assigns q+2. `try_push` finds `used = Q`, so it sets `want_space` and S's cursor is q+2. `pending` = [q+2], and the drain returns `Blocked`.
3. V parks. S's `FlushDue` request has already notified V's `Parker` (a permit), or will. V wakes and answers it, which notifies S. No class R or M command runs.
4. S wakes, takes the answer, and runs `next()` → `Event(d₁)` and `pop()`. `used = 0` and `want_space` was set, so S notifies V.
5. V's `retry_pending`, through the held guard, pushes q+2 and returns `Done`. Only now can a third step start.
6. **Variant: S closes while parked.** S runs `deregister`: `closing`, the producer wake, and a `try_lock` that fails and returns at once. `retry_pending` clears S's cursor (F11) and reclaims the entry lazily. The hold proceeds.
7. **Variant: `-dbcrashratio`.** A freeze follows d₂, and its completion needs S's ack of q+2. V's completion wait runs `retry_pending` on every wake, so q+2 reaches S after S pops d₁. S acks, W completes, and the drain finds `pending` empty.

Loom L10 checks all three cases (§10.2), and so does CH1-node's threaded test of the same case (its §9.6). The probe results are in Appendix B.

**Erratum D56 (F3: the seams in SB4's files).** SB4's writer calls into SB5 at these sites. They are one-line calls, approved like D41, in their own commit.
1. **`hold_tip`.** After the lock it calls `hold_opened` (§4.3, DEC-157 (2)). Without it, `register` and `subscribe_at` see no owner and wait out the whole hold by yielding on `try_lock`, instead of returning `HoldActive` (or `WouldDeadlock` on the owner's thread).
2. **`Hold`'s `Drop`.** It calls `Hold::end`: `hold_closing`, then the guard drops, then `fire_release_wakes` (§4.8 step 4), so a woken `register` finds the mutex free.
   - `Hold.guard` is a `HoldGuard`, an owned guard in an `Option` that derefs to the `ArcMutexGuard`, so no caller changes.
   - `ManuallyDrop` would need `unsafe`, which the crate forbids.
3. **`Hold::connect_tip` and `disconnect_tip`.** They call `Hold::fanned`: the step, then `after_step`. Node steps inside a hold fan out per step (§4.7).
4. **`enforce_block`.** It calls `hold_opened` after the lock. `Composite.guard` is a `HoldGuard` too, and `Composite`'s `Drop` ends the same way on both paths:
   1. `hold_closing`;
   2. on an unwind only, the state is put back and the latch trips;
   3. the guard is released;
   4. the release wakes fire, with the mutex free (SB4's review).
5. **The writer's rule (§3.3, D8).** `before_step` runs at `connect_step`, `disconnect_step`, SPV `set_active_tip` (after `lock_tip`) and `Hold::set_active_tip`.
   - With `pending` non-empty, **every build** returns `ChainError::Internal("tip step with a pending fanout")`. Debug builds also `debug_assert`, which panics first.
   - Two consequences follow:
     - SB4's node `set_active_tip`, a convenience that V does not use (V steps in holds), fails partway through when a target is full. Callers drain between steps.
     - SPV stage 4, even with an empty plan, needs the previous step's last segment drained, as §3.3 already requires.

Each seam has a test that fails without its call, or with a wake fired before its guard drops (`chain/sb5/hold_tests.rs`, mutation-checked).

### 4.8 SPV composites in sub-steps (DEC-155 (2); R2-M1; rev3: m2, m3, m9, m10)

rev1 ran an SPV `set_active_tip` as one composite whose plan, `TipChange` and pending segment all scaled with d + c, which no configuration bounds (Sol r1's stored-branch witness, d = 9,000, c = 9,001). Without A9 the three coexisted: 1,728,096 B for that witness (Sol r2; reproduced in Appendix B). rev2 bounds them by construction.

**`activate_segmented(target, wait)`** (stage 4, on the header writer's dedicated thread, SB5-Y1):
1. `let mut hold = chain.hold_tip()` (SB5-A12).
2. `t = tip_target(target)` (SB5-A11 (c)): `{ fork, d, c }`, found by skip-list `lca` over resident, cached or stored records. No leg is materialized.
3. While the private tip is not `target`, pick the next sub-target. **There are no mixed sub-steps** (m10): every sub-step has d_i + c_i ≤ E.
   - while disconnects remain (private tip above `fork`): `ancestor(tip, max(fork.height, tip.height − E))`;
   - else: `ancestor(target, min(target.height, tip.height + E))`.

   Charge the sub-step's vectors to the step budget, run SB4's in-hold `set_active_tip(sub_target)` (`tip_plan_bounded`, legs ≤ E, exact capacity), and assign the events their `seq`s. **If this is not the last sub-step**, fan them out now as one segment (§4.3), then `while hold.retry_pending() == Blocked { wait() }`. The vectors and segment are freed, and their charge released, before the next sub-step is planned. A sub-step changes only the private active chain, `seq` and fanout.
4. **`hold.release()`**, in this order (m2, m9):
   1. SB4's stage-4 side effects, **once, for the final tip**: the overlay apply to the resident seqlocked groups, eviction, the `max_cold_leaf_work` fold, the `residency_epoch` bump (SB5-A11 (e)). D36's window stays "a few instructions before the new snapshot", however long the hold was;
   2. the snapshot store, then the status publish (n7);
   3. the **last** sub-step's fanout;
   4. `held_owner` cleared, the guard dropped, the release wakes fired.

   So a single-segment step, the typical one, publishes before it notifies, as rev1 did (DEC-156 (2)'s order). Intermediate sub-steps' events necessarily precede publication; consumers fence (§4.4 F8).

**Errors (m3).** Sub-steps k ≥ 2 do cold reads inside the hold (`ancestor`, the leg materialization below the window). Every cold `StoreError` trips the latch (§5.4). Any `Err` after the first fanout is handled by `hold.abandon()`: the guard drops with **no** snapshot or status store and no side effects, so nothing is published after the trip (§9.1) and no reader ever sees the intermediate tip. The events already delivered stay valid: stage 3 committed `meta.tip = target`, and `open` restores it. An `Err` before any fanout (in `tip_target`) also abandons; nothing was changed.

**Ordering and the F-rows.** Each sub-step assigns its events consecutive `seq`s in the same order the single composite would have (disconnects tip-down, then connects fork-up), so the F1 stream is identical; only the timing of pushes differs. `segmented_equals_single` asserts it, and asserts d_i + c_i ≤ E for every sub-step. F2 holds at release, as for V's holds (§4.7). F10: `subscribe_at` returns `HoldActive` during the hold. `deregister` retires a slot during the hold (H7). Every wait in step 3 is the writer's own wait under the hold: subscribers progress through O(1) slot sections, closing retires a target, and SPV-LO9 (§4.9) excludes every subscriber wait that needs the header writer.

**Cold reads inside the hold.** `tip_target` and the sub-target legs below the window take the cache mutex and the store's read connection while the tip mutex is held. That is case (ii) of §4.2's rule: no holder of the cache mutex or of the read connection ever waits for the tip mutex (A10 keeps the stage-5 invalidation out of the tip section, for latency). V's holds do block I/O the same way (`ConnectTip`).

**`wait`.** It is called on the writer's thread with the guard held, in the `Hold`. It must not take the tip mutex, and it must leave subscribers runnable. SB14 runs the header writer on a dedicated thread that parks one `Parker` here and in the post-release drain (SB5-Y1; n9). A guard is `!Send`, so the hold never crosses an `.await`.

**The typical step is unchanged.** With ChainLocks on, d + c ≤ ~4 ≤ E: one sub-step, fanned out at release after the snapshot store, drained after release outside the lock (§3.3). Only composites longer than E wait inside the hold.

**Why not lift the cap by splitting publications instead?** Publishing at each sub-step would show readers intermediate tips that are worse than the pre-step tip. Core never shows those: `ActivateBestChain` keeps `cs_main` until the tip is not worse than `starting_tip` (`validation.cpp:3812`). The hold keeps that property.

**Erratum D57 (F3, DEC-174: SPV's window inside a hold).** SB4 now windows SPV's active chain at `max(floor, tip − w)` after every step (DEC-174), and re-extends it by ancestry when a disconnect goes below its base.
- **Each sub-step windows.** Step 3 calls `HeaderChain::window_active` (through the hold) right after each sub-step's `set_active_tip`, the last one's included. Stage 4's side effects at release still evict once. Otherwise a run of connects longer than `w` would keep every height it passed until the release. After each call, a `debug_assert!` checks that an evicting chain holds at most `w + 1` active heights.
- **Both sub-targets by ancestry.** The disconnect sub-target is `ancestor(tip, tip.height − k)`, as step 3 says, not a read of the active chain: below the base those heights are gone.
- **Test.** `segmented_windows_each_sub_step`: a reorg 17 below the tip and 30 long, at w = 4 and E = 3. It runs 16 sub-steps, each within the bound, and the base lands at `tip − w`. In debug builds, which the gate runs, removing the call fails it through the bound. In release builds the release's eviction hides the difference.

**Erratum D60 (F3: review fixes to `activate_segmented`).**
- **Driver bugs are refused before the hold, and before a stage-1 plan is taken.** Each is `Internal`:
  - a `target` that is not its block's own ref, since a wrong height could walk the tip somewhere else or loop under the lock;
  - a previous step's fanout that has not drained (§3.3), even when there is nothing to step.
- **An empty sub-step is `Internal`.** It would otherwise repeat forever under the lock.
- **The latch on a partial abandon.** When an error abandons the hold after a sub-step moved the private tip, the latch trips, unless a cold read already tripped it. The writer's private chain and its published state then differ, so the writer must not step on. No reachable path is known, so this is defensive and untested.
- **Inside a hold, `HeaderChain::retry_pending` and `pending_is_empty` panic** instead of deadlocking on the held mutex. The writer uses `Hold`'s versions.
- **Fixed by SB4, in their files (558f54403).** `Hold::release` fans out nothing, so an SPV caller that stepped through `Hold::set_active_tip` and then called `Hold::release` directly would leave a gap in the seq stream. `Hold::set_active_tip` is now `pub(crate)`, so `activate_segmented` is the only SPV route.
- **The node `set_active_tip`.** A step refused for a pending fanout stops the change there, and the steps already taken stay published. Its doc says so (the seam commit).

### 4.9 SPV-LO9: nothing the SPV header writer waits on waits on it (DEC-157 (1); Opus high M2)

V's in-hold wait *services* what its subscribers can block on (`FlushDue`, `PruneClears`, W's wakes), and CH1-node's LO-9 forbids every task V waits on from awaiting anything else. The SPV header writer's `wait()` and its post-release drain only park. So SPV needs the rule stated, not inherited.

> **SPV-LO9 (normative).** Nothing the SPV header writer waits on awaits, directly or through another actor, anything that needs the header writer's progress. The header writer waits on every live subscriber's pop task (the cfheaders, filters, blocks, wallet and `dash-mnlist` tasks), so each of them may **post** to any actor but never **awaits** an actor reply, a `JoinHandle`, or a response that the header task or the network actor produces.

**Opus high's cycle, excluded.** The filters task, on `Connected(b)`, requests `cfilter(b)` through the network actor and awaits the reply before popping. The actor forwards an inbound `headers` message to the header task over a bounded channel; the header task is in `wait()`; the channel is full; the actor blocks on `send`, stops reading peers, and the reply never arrives. SPV-LO9 forbids the await. Independently, SB5-Y2 forbids the blocking send.

**Enforcement (SB5-Y2).**
- **The marker.** SB14 spawns each SPV subscriber's pop task with CH1-node's no-wait marker (a task-local, propagated by SB14's `spawn` wrapper). SB14's actor reply type and `JoinHandle` check it on first poll and fail at once with `WaitOnNoWaitTask`, as CH1-node's `ActorReply` does (DEC-156 (1)). A subscriber that needs a peer's data posts the request and receives the answer as an input it handles when it arrives: the filters adapter keeps a bounded "requested" set and pops `Connected(b)` once the request is posted, and the filter itself is processed when it arrives, in height order, from the adapter's own buffer. Raw channel receivers are banned in marked paths by clippy `disallowed_types`, as in the node.
- **Header ingress never blocks behind the header task.** The network actor delivers `headers`/`headers2` with `try_send`. On `Full` it stops reading that peer's headers until the header task signals space (Core's `fPauseRecv` on a full process queue), and never blocks its own loop. So a parked header task cannot stop the actor from serving other messages, `cfilter` replies included.

**Evidence.** The loom probe `m2_ingress_try_send` (a header writer in `wait()`, a filters task awaiting the actor, an actor with a full ingress channel) completes; with a blocking `send`, `m2_neg_ingress_blocking_send` deadlocks (Appendix B). §10.1's `spv_lo9_actor_cycle` runs the same shape threaded, with the marker on (the await is refused) and off (deadlock within the test's timeout, as the negative control), plus the compliant routing.

## 5. Residency (SPV)

### 5.1 Ids, skip ids, records

- **Stable ids.** An SPV id comes from `meta.next_id` and is written with the record in the header step's transaction.
- **Id reuse (D16; the durable-prefix rule, accepted by Sol r1).** An id that was ever durable is never reused. Ids issued in a Buffered suffix that power loss drops may be issued again after restart, because no durable state can name them:
  - subscriber locators store hashes and heights, in later `WriteSet`s in the same WAL (SB0 §9.5);
  - the wallet stores a hash locator (SB0 E3);
  - `locks` and `meta` are in the same transaction as the records they name;
  - in-memory caches die with the process, which the tests model by discarding every instance (§3.7).

  SB0's absolute wording ("never reused") is reconciled by the proposed erratum E7 (Appendix A).
- **Skip ids.** Each record's `skip` is Core's `pskip`, the ancestor at `GetSkipHeight(height)` (`chain.cpp:81-92`, set by `BuildSkip` `:126-130`). It is persisted, so `ancestor` over cold records costs O(log n) `by_id` reads (`GetAncestor`, `chain.cpp:94`). `open` checks skip heights.
- **Records are immutable** except `status`, so SB5's ancestry walks never need status coherence.

### 5.2 The cold cache (`ColdCache`)

```rust
pub(crate) struct ColdCache { map: HashMap<BlockId, usize>, ring: Vec<(BlockId, Arc<HeaderRecord>, bool /* referenced */)>, hand: usize, cap: u32 }
```

- **Replacement.** CLOCK (second chance), with `cap` = 1,024 by default and 256 on mobile.
- **Cost (R1-m3).** A hit is O(1). An insert is amortized O(1), but O(cap) in the worst case: when every reference bit is set, the hand clears all `cap` bits before it selects a victim. That sweep runs under the cache mutex, which is never taken under the tip lock (A10). The `chain-cold-lru` bench includes the all-referenced miss.
- **Read-through under one lock** (EV addendum §2.6; Core `snapshotCacheCs`, `llmq/snapshot.cpp:307-315`, `:326-328`). `get_or_load(id, store)` takes the cache mutex, looks up, reads `store.by_id(id)` on a miss, inserts and returns, all under that one mutex. `by_hash` misses insert by id.
- **Invalidation.** `invalidate(ids)` takes the same mutex. SB4's stage 5 (`records_committed`) calls it, after the tip lock is released, for every written id and every overlay id of the step (SB5-A5, A10). The list holds at most `cap` ids; a step with more calls `clear()` instead, O(cap) under the cache mutex (DEC-157 (3)). Clearing is always correct: it removes a superset of the stale entries, after the commit.
- **Why no stale insert survives** (accepted by Sol r1, probe L6). A reader's miss-read-insert is atomic with respect to `invalidate`, and every invalidation follows the commit that changed the row. Either the reader's read ran before the commit, and its insert is removed by the invalidation that follows, or it ran after, and it read the new row. This needs SB13's read-after-commit contract (§3.7). The guarantee is that no old cache entry survives the invalidation, not that the cache is coherent at the commit instant. That gap is SB4 D36's one-step window.
- **Bypass.** `walk_down` and range walks bypass the cache (D14).
- **Issued ids** (DEC-187 amendment 3, D81). `Shared` keeps the writer's `next_id` as a watermark, stored with Release at every issue and restored by `open` from the store's `next_id`. A read-through loads it after the store read and refuses a record whose id is at or above it: corruption, which trips the latch, and nothing is cached. A record read by hash is still admitted under the id it names. An issued id with no record is corruption at every by-id read that misses the cache (D82): `Shared::try_node`, under `try_entry`, the links and the locator, ancestor and LCA guards, reads the watermark before its resident lookup, and an id below it that is neither resident nor stored trips the latch. An id at or above it still answers `Ok(None)`. The bulk reads (`walk_down`, `children_from`, `active_from`, `from_height`, `leaves_from`) bypass the cache and are not checked: the ruling covers admission.

### 5.3 Snapshot heights below the window

`FeedSnapshot::at_height_checked(h)`, for the snapshot's view V with base b and height t:
- **h > t:** `None`.
- **b ≤ h ≤ t:** `V.at(h)`, with the hash from the resident entry or the cache.
- **anchor ≤ h < b:** `ancestor(V.at(b), h)`. This is skip-id ancestry over immutable fields, through the cache and the store, and it is always on the snapshot's chain (DEC-105 F3; accepted by Sol r1). It never reads `active_at`.
- **h < anchor height** (SPV anchored): `Ok(None)`. That is the anchored chain's normal boundary, as `AnchoredAncestry` returns `BelowFloor` (SB4 §8.4). It is never corruption.
- **Cost.** O(log(b − h)) record reads.

`range(lo, hi)` is the resident part from V plus one `ancestor` and `walk_down` batches of 1,024 for the cold part, about n reads (SB0 §4.4).

`ChainSnapshot::at_height(h)` maps an error to `None`; the error has tripped the latch (§5.4).

**What a retained snapshot pins:** its `ActiveChainView` leaves, ⌈(N_win + B)/1,024⌉ + 1 leaves of 4 KiB. That is ≤ 56 KB at w = 10,000 and ≤ 20 KB at w = 2,048 (§6.3). It pins no arena chunk.

### 5.4 `IndexAccess` in SPV (the cold variant)

- `lookup`, `ancestor`, `lca`, `height` and `status` go resident first, then through the cache, then to the store.
- A `StoreError` trips the latch and returns `None` (D21; accepted by Sol r1 as conditionally safe).
- **The condition.** Every operation that persists anything checks `fail.is_tripped()` after its dependent reads and before it writes, and aborts if the latch has tripped. That covers:
  - `subscribe_at`'s `finish()` (the subscriber's handoff commit);
  - subscriber data writes and locator commits;
  - rewinds;
  - wallet effects and checkpoints (SB0 E3);
  - SB13's store writer, before it executes each queued `WriteSet`.

  CH2b and SB14 assert each path with fault injection (C1, OQ5). An answer derived from a failed read therefore never becomes durable.
- `ChainReader::{try_lookup, try_ancestor, try_lca, try_entry}` return `Result<Option<_>, ChainError>` for callers that branch on the error.

**Erratum D41–D43 (F5b).**
- **Where F5b edits SB4's files (D41).** These are the seams SB4 C4 left, plus the minimum around them:
  - `Shared` holds the cache, `cold: Option<ColdCache>`. It is sized `cold_cache` in SPV mode, the mode `records_committed` invalidates for, and absent for the node. `cold_by_id` and `cold_by_hash` read through it, and `invalidate_cold` calls `invalidate` or `clear`.
  - `HeaderChain::feeds` passes `ChainFeeds` the shared state as a `ColdWalk`: the store's `walk_down` past the cache. A failed read trips the latch, and so does a batch that comes back short or unlinked, as corruption.
  - `chain.rs` gains `mod sb5;`. SB5's writer half (F6b, F7) and its SPV tests live under `src/chain/sb5/`, reusing six of SB4's `spv_tests` fixtures, made `pub(super)`.
- **`range` and the floor (D42).** SB4 C4's SPV view is not windowed yet (D-C4-7), so V can hold cold heights. `range` is therefore one walk down `prev` from `hi`'s block (`V.at(hi)`, or one `ancestor` below the window). Resident entries are read in place, and cold runs go by `walk_down` in batches of 1,024, plus one record that names the next batch's start. Heights are clamped at the floor, anchor − 24 (D-C4-12), which is the "anchor" boundary of §5.3: `AnchoredAncestry` returns `BelowFloor` there. `at_height_checked` reads a cold height in V through the cache.
- **The cache's hash index (D43).** SB4 resolves every cold record's `prev` by hash (`cold_node`). Without a hash index, every cold read made one store read even on a hit, and §5.2's "always reads `store.by_hash`" made the cache save nothing on links. `Clock` gains `by_hash: HashMap<BlockHash, BlockId>`, kept in step with `map` on insert, victim, `invalidate` and `clear`, so `load_by_hash` hits as `get_or_load` does, checking the hit's hash. Coherence is unchanged: hash ↔ id never changes, and a record leaves both maps together. The hasher is fixed (D34). Miners choose hashes, but the map holds at most `cap`, so the worst probe is O(cap), like the CLOCK sweep.
  - Memory: 2,048 buckets × 37 B + 16 = 75,792 B at `cap` 1,024, and 18,960 B at 256. That is about 74 B per record, so ≈ 306 B instead of ≈ 232 B. §6.3's cold cache row becomes 313,360 B (desktop) and 78,352 B (mobile), and its typical totals ≈ 4.50 MB and ≈ 1.60 MB.
  - PF0 `chain-cold-lru` counts move with the second map.

## 6. Memory bounds (DEC-155: caps by construction; DEC-157 (3))

rev1 stated memory as formulas over d and c, with adversarial additions. Sol r2 found two such formulas still wrong (R2-M1: the no-A9 overlap; R2-M2: reader-owned controls). DEC-155 replaces proof by arithmetic with **budgets enforced by construction**:
- Every allocation on the SPV header-step path, and every per-feed reservation, is charged to a counted byte budget before it is made.
- Each budget's cap is a **`const fn` of the configuration and the real `size_of` values** (Q, E, W, B, the cache `cap`, `max_feeds`, `MAX_CONTROL_KINDS`, `CONTROL_PAYLOAD_MAX`; `size_of::<AcceptOne>()`, `size_of::<StepRow>()`, `size_of::<BlockRef>()`, `size_of::<ChainEvent>()`, `size_of::<ControlMsg>()`). The numbers below are those functions evaluated at x86-64 sizes measured on SB4's code at 562a0f053, with SB5-A2's 32-byte `hash` added to `HeaderRecord`: `BlockRef` 40 B, `ChainEvent` 56 B, `AcceptOne` 56 B, `HeaderRecord` 176 B, `StepRow` 176 B, `ControlMsg` 24 B. rev2's 16-byte accept result and 184-byte row were estimates and are gone (m1).
- **One adversarial step term** is not a function of the configuration: the mark count m of a ChainLock's marking walk (§6.2). DEC-157 (3) records it as the one exception to "every allocation", as Sol r2 accepted S for residency.

**What happens at a cap (m1, decided).** Only one sub-cap can be relieved by waiting:
- **the pending segment**: the next sub-step waits until `pending` drains (the Blocked path, §4.8);
- **accept results over B**: refused with `ChainError::Config("step exceeds step_headers")` before any work (the caller's bug; SB14 slices);
- **the write batch**: flushes at W rows (`sink.rows`), by construction never over;
- **any other overrun** (the legs, `TipChange`, the A5 list, the fixed term): `ChainError::Internal`, the latch trips, and a `debug_assert!` fires. It is a derivation bug, so it fails loudly and never waits: a wait with only the writer's own charges held would never end.

This section derives each cap. The "observed" columns come from the representation probes (Appendix B); at implementation A-F12 (§10.4) replaces them with the production measurement and asserts **observed ≤ cap** (and ≤ cap + M(m) when marks are present).

### 6.1 The per-feed budget (both products)

| Term | Allocation | Sub-cap | Q = 1,024 | Q = 256 | Past the cap |
|---|---|---|---|---|---|
| fixed | slot, `Arc`s, the reader's box, table and registry entries, `ctl_order`, the `ctl_payload` array (8 × `size_of::<Option<ControlMsg>>()` = 8 × 24 B) | 576 B (asserted ≥ the `size_of` sum; erratum D14) | 576 | 576 | constant |
| event buffers | queue + staging + spare, reserved at `register` (DEC-152 (1)) | 3 · Q · `size_of::<ChainEvent>()` | 172,032 | 43,008 | `used ≤ Q` (F3): `try_push` returns `Full`, the target gets a cursor, the writer gets `Blocked` |
| control payloads | ≤ `MAX_CONTROL_KINDS` outstanding + 1 being handled (a driver contract, n2), each ≤ 24 B + `CONTROL_PAYLOAD_MAX` | (8 + 1) · 280 | 2,520 | 2,520 | a same-kind send coalesces (§4.3); `T: Copy` owns no heap |
| **per-feed cap** | | | **175,128** | **46,104** | |
| **feed cap** = `max_feeds` × per-feed | | | node 16: **2,802,048**; SPV desktop 8: **1,401,024** | SPV mobile 8: **368,832** | `register` returns `TableFull` |

**Erratum D14 (F2, signed off by the manager).** rev3's fixed term was 512 B. F2's measured `size_of` sum on x86-64 is **528 B**: the slot's `Arc` header 16, `FeedSlot` 284 (304 in test builds, whose 20 B of fault hooks are excluded), the reader's `FeedReader` box 88, the table entry 96 (it holds the reserved reader buffers inline, `Option<ReaderBuffers>` 64 B, from `register` until `subscribe_at` moves them into the reader), its free-list index 4, and the `RegistryEntry` 40. The sub-cap is **576 B**, leaving 48 B of headroom; `FEED_FIXED_BYTES` is asserted at compile time to cover the sum. The per-feed cap rises by 64 B and the feed caps by 64 B × `max_feeds`, as in the table above. §6.3's rounded feed rows (1.4 MB) and totals are unchanged at their precision.

Observed (probes): Sol r2's control interleaving, 10,000 rounds: at most **2** messages held (rev1: 10 after 10 rounds, unbounded). 2,000 random schedules over 3 kinds: at most **3** (= the kinds). Event buffers: exactly reserved; `used ≤ Q` is the F3 invariant.

The node's control-mirror backlog during an `enforce_block` hold (rev1's 400 B × (d_h + 1) term) is now ≤ 8 messages per subscriber by construction, whatever d_h is. OQ12 is closed.

**The node's pending backlog.** At most one step's event (56 B) at any instant, because V drains after every step (§4.7).

**The status cell.** Facts ~150 B, plus ~100 B per status reader, plus each reader's `Notify`.

### 6.2 The SPV step budget (DEC-155 (2), DEC-157 (3))

Configuration: E = `segment_events`, W = `write_batch`, B = `step_headers`, C = the cold cache's `cap`.

| Term | Allocation | Sub-cap | Desktop (E = W = C = 1,024, B = 2,000) | Mobile (E = W = C = 256, B = 2,000) | Past the cap |
|---|---|---|---|---|---|
| accept results | SB4's per-header outcomes; also the dirty set of new records (SB5-A11 (a), A13) | `size_of::<AcceptOne>()` · B = 56 · B | 112,000 | 112,000 | `Config` error before any work |
| write batch | one reused `StepRow` buffer (§3.7) | `size_of::<StepRow>()` · W = 176 · W | 180,224 | 45,056 | flush at W |
| sub-step plan legs | `tip_plan_bounded`'s vectors, exact capacity | 40 · E | 40,960 | 10,240 | `Internal`: legs ≤ E by the choice of sub-target |
| sub-step `TipChange` legs | copies (no A9); 0 with A9 | 40 · E | 40,960 | 10,240 | `Internal` |
| pending segment | `Segment::Composite`'s copied `BlockRef`s (no A9); 0 with A9 | 40 · E (rev2 charged 56; n3) | 40,960 | 10,240 | Blocked: the next sub-step waits for the drain |
| A5 invalidation list | ≤ C ids, else `clear()` (§5.2) | 4 · C | 4,096 | 1,024 | clear-all, by construction never over |
| registry republish | a step's retires each build a registry while the last is published: two of an `Arc`'s counts, the `Registry` and `max_feeds` reserved entries (DEC-187 amendment 2 (a), D76) | 2 · (16 + 32 + 40 · `max_feeds`), `max_feeds` ≤ 64 (`MAX_FEEDS`) | 736 (8 feeds) | 736 (8 feeds) | `Config` error past 64 |
| active chain paths | `ActiveChain`'s copy-on-publish: a push or pop copies the tip's root-to-leaf path out of the published view, and a disconnect below the window's base the base's. A path is a 4,096 B leaf and an inner node of 512 B per level to 4 (every `u32` height), each in a 48 B `Arc` (amendment 2 (a), D76). Fresh nodes, and the published view's originals a reorg rebuilds, are §6.3's | 2 · 6,384 | 12,768 | 12,768 | by construction |
| fixed | the two containers a release publishes, each an `Arc`: the snapshot's 288 B and the status facts' 112 B (`STEP_FIXED_BYTES`, from `size_of`; D80) | 400 | 400 | 400 | `Internal` |
| **step cap** | | **56B + 176W + 120E + 4C + registry + 12,768 + 400** | **433,104** | **202,704** | |
| **marks term M(m)** (adversarial) | the status-only overlay, `HashMap<BlockId, Mark>` (12-byte entries since SB4's fix r2, D74), during its last rehash: old + new tables | 13 · (b + b/2) + 32, where b = hashbrown's bucket count for m entries (the next power of two ≥ 8m/7), at least 128 (the 64 entries the overlay keeps between steps); M(m) ≤ 45 · m + 2,528 | m = 9,000: 319,520 | m = 9,000: 319,520 | uncapped by design (DEC-157 (3)); counted in its own budget account and asserted ≤ M(m) |

The two shares that dominate are the write batch (42% desktop) and, on mobile, the accept results (55%). Mobile with B = 500 has a cap of 118,704 B (OQ4). The registry, path and fixed rows are sized from the types (`registry_bytes`, `ACTIVE_PATHS_BYTES`, `STEP_FIXED_BYTES`), and A-F12 asserts each exactly: one path copy at the deepest heights is 6,384 B, one registry at 64 feeds is 2,608 B (D76), and one release's two containers are 400 B (D80).

**Observed** (probe with a counting allocator around a model of the step path with the real sizes, Appendix B `step_budget_r3.rs`):

| Case | Desktop | Mobile |
|---|---|---|
| typical (d + c ≤ 5), no marks | 296,320 B | 158,080 B |
| Sol's witness (d = 9,000, c = 9,001), no A9 | **415,104 B** ≤ 420,224 | **187,776 B** ≤ 189,824 |
| the same, d = 50,000, c = 50,001 | 415,104 B | 187,776 B |
| Sol's witness with A9 | 333,184 B | 167,296 B |
| 9,000 marks (overlay peak 221,216 B = M(9,000)) | 443,792 B ≤ cap + M = 641,440 | 333,216 B ≤ 411,040 |
| 50,000 marks (overlay peak 884,768 B = M(50,000), 17.7 B per mark) | 996,768 B ≤ 1,304,992 | 996,768 B ≤ 1,074,592 |
| Sol's witness and 9,000 marks together, no A9 | 562,576 B ≤ 641,440 | 335,248 B ≤ 411,040 |
| rev1's unsegmented no-A9 witness, for contrast (not reachable since rev2) | 1,728,096 B | — |

The capped part is independent of d + c and of the number of lagging readers; the marks term is independent of the configuration and grows ~26–45 B per mark (35.5 B at m = 9,000; the table's marks rows are rev3's probe at a 9 B bucket, D74 has fix r2's 13 B). rev2's full-record overlay cost ~210 B per mark: 1.9 MB for 9,000 marks (Opus high M3).

**The marks term, stated (DEC-157 (3)).** m is bounded by the length of a stored conflicting branch: the ChainLock's marking walk marks the sibling roots of the locked path that are not active, and the blocks it disconnects (SB0 §4.1, `EnforceBlock` `validation.cpp:4047-4077`, `:4112-4113`). Every marked header passed PoW and the contextual rules. **Worst case:** m = the stored branch length; 9,000 marks need ~9,000 valid-PoW headers on a branch the ChainLock rejects: costly on mainnet, cheap on test networks, where M(9,000) = 319,520 B and M(50,000) = 1,277,984 B (D74). **Rejected alternative:** write-through cold marks with read-your-writes through the step transaction, which would remove the term but change SB0 §9.5's one-writer model and SB13's read contract for a test-network-only cost (Opus high M3 (c)).

**Outside the step budget, and bounded elsewhere:**
- **The decoded peer message.** It is dash-p2p's: ≤ 2,000 headers per `headers` and ≤ 8,000 per `headers2` (`validation.h:82-83`; `net_processing.cpp:1209-1215`), enforced on receipt (`:5323-5327`). That is ≤ 640,000 B serialized per message, and ≤ 1,152,000 B decoded at 144 B per header (`Header` and its hash; DEC-187). A-F12 builds the `HeaderBatch` before the measured call and leaves it out of both accounts; a copy SB5 made of it would be charged (amendment 2 (b)).
- **Residency.** The arena's chunks and tables, the hash index with its rehash transient, SPV's resident-height set and chunk counts, and the `ActiveChain` leaves are the resident set (§6.3). A step that crosses chunk boundaries allocates up to ⌈B/1,024⌉ + 1 chunks before stage 4's eviction frees as many; A-F12 charges that turnover, the index's growth and the resident set's to a residency account bounded by §6.3, not to the step (amendment 2 (c), D77).
- **The cold cache.** A fixed `cap`.
- **SQLite's page cache.** SB13's.

**Erratum D58 (F3: the step budget as built).** `step_cap` is the table's formula over the configuration (420,224 B and 189,824 B, asserted; 433,728 B and 203,328 B at D76; 433,104 B and 202,704 B since D80). `HeaderChain::step_budget()` reports it with two peaks that `StepBudget` records:
- **The sub-step peak.** The `TipChange` legs' capacities, plus the pending copy when a sub-step blocked.
  - SB4 builds both legs at exact capacity (`walk_down`, since DEC-174's 8b765ae74).
  - A change whose legs and pending copy could exceed the sub-step term `3 · 40 · E` is `Internal` before its fanout. It can't fire on a sound plan, because `tip_plan_bounded` keeps the legs at most E.
  - The tests check the exact charge for a connect run and for a disconnect run.
- **The marks peak.** The overlay's table at release, the current table only. It is not M(m)'s transient old-plus-new peak during the last rehash: that needs a counting allocator, which F8's A-F12 cases bring.

### 6.3 Residency

**Node** (`window = None`, every entry resident; SB4 §3.8 scaled; SB5 adds no per-header term):

| | 2.1 M headers | 2,551,766 (H-M) |
|---|---|---|
| arena (200 B) | 420 MB | 510 MB |
| hash index (4,194,304 slots × 5 B) | 21 MB | 21 MB |
| `links` (8 B) | 17 MB | 20 MB |
| `ActiveChain` (4 B per height) | 8.4 MB | 10 MB |
| feeds (§6.1, 8 in use) | 1.4 MB | 1.4 MB |
| **total** | **≈ 468 MB** | **≈ 563 MB** |

The arena row is rev3's 200 B estimate. At the measured 240 B per entry (DEC-187; `Entry` is the same in both residencies) it is ≈ 504 MB and ≈ 612 MB, and the totals ≈ 552 MB and ≈ 664 MB.

**SPV.** N_win is the resident in-window entries: the w + 1 active heights plus any stale entries with heights in the window. B is the step's newly accepted, not yet evicted, headers. S is the chunks pinned only by out-of-order ids. The arena holds ⌈(N_win + B)/1,024⌉ + 1 + S chunks of 1,024 × 240 B = 245,760 B (DEC-187: `size_of::<Entry>()`, measured; rev3 estimated 200 B).

| Term (typical: N_win = w + 1, S = 0, 5 feeds) | Desktop, w = 10,000, B = 2,000 | Mobile, w = 2,048, B = 2,000 |
|---|---|---|
| arena (245,920 B a chunk with its `Arc` and eviction bits) | 13 chunks = 3,196,960 B | 5 chunks = 1,229,600 B |
| arena fixed tables (first level 4,096 × 32 B; one second-level table per 2^20 ids) | 139,264 B | 139,264 B |
| hash index: b · 5 + 16 B, b at most the smallest power of two with 7b/16 ≥ N_win + B (D77) | 163,856 B | 81,936 B |
| index rehash transient: the old table of b/2 while the table grows into b (DEC-187 amendment 2 (c)) | 81,936 B | 40,976 B |
| SPV resident-height set, `BTreeSet<(u32, BlockId)>` (bound: 104 B leaves and 200 B internal nodes, each ≥ 5 keys but the root; D77) | 345,904 B | 116,840 B |
| leaves, children, overlay (SB4 §3.8), chunk counts | < 10 KB | < 10 KB |
| `ActiveChain` leaves | 53,248 B | 20,480 B |
| each retained snapshot | ≤ 53,248 B | ≤ 20,480 B |
| cold cache (`cap` × ≈ 232 B) | 237,568 B | 59,392 B |
| feeds (5 × per-feed cap) | 875,320 B | 230,200 B |
| step budget (cap) | 433,104 B | 202,704 B |
| decoded `headers` message during accept (2,000 × 144 B; dash-p2p's account, §6.2; a `headers2` message holds up to 8,000) | 288,000 B | 288,000 B |
| **total, typical** | **≈ 5.83 MB** | **≈ 2.42 MB** |

The feeds and step rows are caps, not estimates. The rest are typical. During a step, the published view keeps the active-chain leaves a reorg rebuilds, at most one more `ActiveChain` row; that is the retained-snapshot row's bound (D76). Feeds at the full cap of 8 add 3 × per-feed: 525,192 B on desktop, 138,120 B on mobile. A step with m marks adds M(m) transiently (§6.2). Retained snapshots are additional, one row each. The totals are DEC-187's (D72): rev3's 4.42 MB and 1.58 MB had the arena at 200 B per entry and left out the arena's fixed tables and the decoded message. A-F12's `mem_spv_typical_*` bound the peak at these totals (measured 5,021,953 B and 2,082,185 B), and PF0's `chain-spv-typical-*` lines gate the residency per PR (§11, D73). D77 raised them from 5.38 MB and 2.21 MB: the step cap's registry and path terms (D76), the index at its derived bound with its rehash transient, the resident-height set, and the chunks' `Arc` headers. A-F12's `peak_full_step_*` bound the residency blocks a 2,000-header step holds at the arena, index, resident-set and chunk-count rows for N_win + B, and measure 3,522,632 B on desktop and 1,357,768 B on mobile.

**Adversarial residency terms** (Sol r2 accepted these with S retained):
- **Stale branches in the window.** Each adds 240 B to N_win, at chunk granularity, until the tip passes it by w.
- **S, the out-of-order chunks.** A chunk is pinned past its time by an in-window entry whose id was issued long before its height's neighbours. The pinning entry may be stale *or active* (R2-m2): active-path headers accepted interleaved with side-branch headers put successive active ids in different chunks. A8 removes the stale-only pins, not the active ones, so S is retained after A8. Each pinned chunk costs 245,760 B: the 100-chunk fixture is 24,576,000 B.
- **The worst case by construction.** A chunk is freed when it holds no resident entry, so chunks ≤ N_win + B. At w = 2,048, B = 2,000 with no stale entries, that is ≤ 4,049 chunks = 995,082,240 B. Reaching it needs ≥ 1,023 other accepted valid-PoW headers per pinned chunk, about 4.1 M headers: costly on mainnet, cheap on test networks.

**The mobile batch saving.** B = 500 instead of 2,000 saves one arena chunk (245,920 B), 84,000 B of the accept-results term, 61,440 B of the index rows and 43,200 B of the resident set, a typical total of about 1.99 MB (OQ4). The decoded message does not shrink with B: a peer sends up to 2,000 headers (8,000 in `headers2`) however the host slices them, unless SB14 decodes in B-sized pieces.

**w's floor.** w ≥ L_est + H_keep, which is 1,826 on mainnet (SB0 §4.4). The host checks it (SB15), because `dash-chain` takes no LLMQ parameters (SB4 D26).

**Disk (SB13's, for scale).** About 330 B per `hdr` row with its indexes. An anchored mainnet client from 2.0 M holds about 180 MB; a genesis sync of 2.55 M rows, about 840 MB.

## 7. Persistence format and keys (SB0 §9.5; SB13 owns the SQL)

Logical schema. Integers are SQLite `INTEGER`; byte strings are `BLOB`; `Work` is 32 bytes big-endian.

| Table | Key | Value | Written by |
|---|---|---|---|
| `meta` | `k TEXT` | `schema` u32 = 3; `anchor` hash ‖ height; `anchor_state` u8; `next_id` u32; `tip` id; `best_header` id (diagnostic, E5); `sync_seq` u64; `cl_enabled_observed` u8 | the header step (§4); `sync_wal` increments `sync_seq` |
| `hdr` | `hash BLOB(32)` PK | `id` UNIQUE, `height`, `header BLOB(80)`, `skip` NULL-able id, `status` u32 (Core `nStatus` bits), `chain_work BLOB(32)`, `time_max` u32, `kind` u8 (0 accepted, 1 context) | step 1 |
| `hdr_id` | `id` | `hash`. SB13 may make it the `UNIQUE(id)` index of `hdr` | step 1 |
| `hdr_h` | `(height, id)` | — | step 1 |
| `hdr_prev` | expression index on `substr(header, 5, 32)` (`prev_hash`) | — | — (index); answers `children` (DEC-123 Q7) |
| `hdr_leaf` | `id` PK | — | step 1's `leaf_add`/`leaf_remove` |
| `active` | `height` PK | `id` | step 2 |
| `locks` | `kind TEXT` PK ∈ {`best`, `floor`} | `best`: height, hash, the 96-byte signature; `floor`: id, height, hash | step 3, when the step changed the CL state |

**Transaction.** The header step's `WriteSet` is one `BEGIN IMMEDIATE … COMMIT`: the `StepRow` batches as the step produces them, then the `StepTail` in order (SB0 §9.5). It commits **before** `set_active_tip` (stage 3 before stage 4). Its durability is Buffered. A Durable commit (`sync_wal`) runs at checkpoint boundaries, at sync-complete and on stop.

**Encoding.** `dash-chain` exports `HeaderRecord::{to_row, from_row}`, used by `MemHeaderStore` and SB13 (P3).

## 8. `open` (SPV reopen)

`HeaderChain::<S, Evicting>::open(params, cfg, svc, store)`. Every `StoreError` trips the latch and returns `OpenError::Storage`, including those from the leaf and failed-path walks in step 7. Every `Corruption` trips the latch and returns `OpenError::Corrupt`; SB15 then clears the header tables and resyncs from the anchor (SB0 §4.4).

1. **Meta.** `chain_meta()`. `None` → `Empty`; schema ≠ 3 → `Schema`. Restore `cl_best`, `cl_floor` and `enabled_observed`.
2. **Records.** `from_height(height(tip) − w)`. `tip` must be among them (`MissingRecord`).
3. **Arena, at the stable ids (A3).**
   - Insert the records in height order, then id order. Every loaded record's prev must be loaded, below the window base, or the anchor pack's (`MissingParent`).
   - Check skip heights, and `next_id` > `HeaderStore::max_id()` (n4): a stored id at or above `next_id`, loaded or cold, is `Corruption::NextIdNotAbove`, found at `open` rather than at the first insert's UNIQUE failure.
   - Build the resident hash index, the resident leaves and `max_cold_leaf_work` from `leaves()`.
4. **Active chain.** Walk prev from the tip to the window base. Cross-check against `active_from(base)`; any mismatch is `ActiveMismatch` (D18).
5. **MTP.** For the lowest ten window entries, read the 11-ancestor median from cold records.
6. **Checkpoints.** Rebuild the checkpoint cache: one `by_hash` per checkpoint, highest first (SB4 §11.4).
7. **`best_header` is recomputed (E5, D17; DEC-151 OQ6).**
   - **Selection.** The maximum by `CBlockIndexWorkComparator` over entries with `IsValid(BLOCK_VALID_TREE)`, with a lower id winning a work tie (D9's surrogate). This is Core's load recompute (`validation.cpp:5337-5360`; comparator `node/blockstorage.cpp:31-49`).
   - **Eligibility.** `IsValid` excludes `BLOCK_FAILED_MASK` = `FAILED_VALID | FAILED_CHILD` (`chain.h:116`, `:300-307`), **not** `BLOCK_CONFLICT_CHAINLOCK` (`:118`). A CONFLICT-marked leaf is eligible here even though `fork_target` rejects it (SB4 §6.2 (a)). The two predicates are distinct.
   - **The reduction.** For each leaf from `leaves()`, take the leaf if it is not FAILED, else its highest ancestor that is not FAILED (walked through the cache). Context records are excluded. Accepted entries are at least `VALID_TREE`, and work strictly increases along ancestry, so every eligible entry is at or below some leaf's candidate with no more work. The maximum over the candidates is therefore the maximum over all eligible entries.
8. **`fork_target`.** Computed with a fresh `VirtualTip`. Under `debug_assert!` it equals the tip. A release mismatch logs once and keeps the tip.
9. **Tip state.** `init_active_tip(tip)`: `tip_seq = 0`, `next_seq = 1`, an empty feed table, and the first snapshot and status facts published.
10. **Debug check.** `check_index()` under debug.

**Cost (conditional; Sol r1).** At w = 10,000 there is one range read of about 10–12 k rows and one `active_from` of 10 k rows, plus work that depends on branches and paths: `leaves()` over `hdr_leaf`, one walk per FAILED leaf, and the checkpoint probes. `chain-open-spv-w10000` measures `MemHeaderStore` only. The ≤ 150 ms SQLite figure is an estimate until SB13 supplies the measurement (S1).

**Erratum D47–D50 (F7, the C4 half).**
- **Where `open` lives (D47).** `HeaderChain::<S, Evicting>::open` is in `chain/sb5/open.rs`, which can set the chain's private fields. `OpenError` and `Corruption` are in `src/open.rs`, so that lib.rs can export them without widening `chain::sb5`'s visibility. `TipNotLeafPath` names a tip that is a context or FAILED record, or one that no leaf's eligible path (step 7) is as heavy as.
- **The active chain (D48).** C4's SPV view is unwindowed (SB4 D-C4-7), so `open` needs an id for every height from the floor.
  - **The rows.** One `active_from(floor)` read supplies them. They must be dense from the lowest accepted height up to `(tip.height, tip)`, and none may lie below the floor.
  - **The window.** The walk down from the tip, over the loaded entries, must equal the rows (D18).
  - **Below the window.** The rows are checked only for their link to the window's lowest entry, at `base − 1`. A corrupt row strictly between the lowest accepted height and `base − 1` goes undetected and gives wrong `at_height` answers there; catching it would cost O(tip − floor) reads. The context heights below the lowest row come from one `walk_down` whose top must be at its height.
  - **Why not `init_active_tip`.** Its cold walk to the floor costs O(tip − floor) record reads. `open` costs O(tip − floor) row pairs instead and publishes as `init_active_tip` does: the tip cell, at `tip_seq` 0.
- **Checks `open` makes cheaply or not at all (D49).**
  - **Skip heights.** They are checked after the whole window is set, against resident targets only; a cold target would cost one read per record. A skip naming the record itself, a wrong-height window record or an id at or above `next_id` is refused.
  - **Parents.** Only a record at the window's base may have a cold parent; elsewhere a parent missing from the window is `MissingParent`, and a tip missing from it is `MissingRecord`. Step 7's walk from a FAILED leaf checks each parent's height too, so corrupt records cannot loop it; a parent off the height below is `MissingParent`.
  - **Step 10.** It does not run: SB4's `check_index` needs every entry resident and a genesis root.
- **What `open` leaves to others (D50).**
  - **The cold leaves.** `max_cold_leaf_work` is recomputed from the stored leaves outside the window. It is at most the pre-reopen value, which only ever grows.
  - **Contradictions found by cold reads come back as `Storage`.** Examples are a cold parent with no record, an unlinked `walk_down`, and a missing `cl.floor` record. `Shared`'s cold reads report them as a `StoreError`, so SB15 sees an I/O error, not `Corrupt`. Separating them needs a corruption kind on `StoreError`, which is SB4's and SB13's.
  - **MTP at a floor of 0.** An anchor at height 24 has its floor at 0. There `open` computes the pack's low medians as Core does, while `new_anchored` leaves them `MTP_UNKNOWN`.
  - **Duplicates.** A duplicate id from `from_height` hits the arena's set-once assertion. A duplicate hash under two ids hits only the hash index's `debug_assert!`; a release build indexes it twice. SQLite's `UNIQUE` constraints rule both out.
- **SB4 C6's half.** Done in F7's second commit (D52–D54).

**Erratum D52–D54 (F7, the C6 half).**
- **The ChainLock rows (D52).**
  - **The write.** `write_header_step` writes `StepTail.cl = Some(cl_state())` with every step. It is O(1), and the writer keeps no committed copy to compare against. Skipping an empty step is the driver's job (SB4 §9.1 n8).
  - **The restore.** Steps 1 and 9 restore the rows and the observed flag through SB4's `restore_cl_state` (D-C6-3), after `set_tip_cell`, which publishes them in a status snapshot. A floor at or above `next_id` is `Corruption::ClFloor`. Any other error from a chain call in `open` that is not `Storage` is a bug, `OpenError::Internal`: it trips the latch but is not `Corrupt`, so SB15 does not clear the store over it.
  - **Step 8.** `fork_target` from a fresh cursor must be the stored tip. A mismatch is a `debug_assert!`; a release build warns and keeps the stored tip.
  - **The step kinds' tests (A-33, A-34).** In SPV, `EnforceBlock`'s walk changes nothing while the floor is on the active chain. The tests' timer step is the one whose check observes the spork off: the floor stops refusing a heavier branch, and the step writes the observed flag and an active change. While the tip trails the floor (SB4 DEC-136), every timer step re-marks the sibling on the floor's path, and SB4's overlay records the unchanged status, so the step is never empty and commits every 5 s; that is reported to SB4. No chain code trips the latch on a failed commit; SB5 §9.1's table gives that to SB13's writer, and the tests' driver does it in SB13's place.
- **`best_header` after ChainLock marks (D53).** `MarkConflictingBlock` steps the writer's in-memory `best_header` to its parent when it is the marked block or a disconnected one, as Core steps `m_best_header`. `open` recomputes it from the stored leaves, where E5 keeps CONFLICT leaves eligible, as Core's `LoadBlockIndex` does. So after a restart `best_header` can be a marked leaf the running writer had stepped back from, as in Core. The reopen tests compare `best_header` with P4's scan of the store, not with the pre-restart writer.
- **The window after a step that lowers the tip (D54).** Entries the writer evicted while the tip was higher stay cold until something reads them. `open` loads the window from the new `tip − w`, so the reopened resident set and leaves are a superset of the writer's, and every extra entry is at or above `tip − w`. The reopen tests allow exactly that difference.

**Erratum D59 (F7 on DEC-174).** §8 step 4 still checks every `active` row from the floor, then windows the restored chain at `base = max(floor, tip − w)`, as the writer leaves it after a step. The rows below are checked, not kept. The writer's base can sit higher: after a disconnect below the window it stays at the fork until the tip is `w` past it. So the reopen tests require the reopened base to be `tip − w`, at or below the writer's, with equal ids at the heights both hold. On the rebase onto SB4's DEC-174 head, these changes went into the two F7 commits, so each commit passes its own tests.

## 9. Failure and fail-stop

### 9.1 Table

| Failure | Where | Effect | Recovery |
|---|---|---|---|
| store read error | any cold read | `FailStop::read_failed()` trips the latch; the writer gets `ChainError::Storage`, a `try_*` reader `Err`, an infallible reader `None` | the client stops (P6); nothing published after the trip; nothing persisted after it (§5.4) |
| header-step commit error | SB13's writer, stage 3 | the latch trips; stage 4 never runs (SB4 A-33) | restart; the lost suffix is re-downloaded |
| corruption at open | §8 | `OpenError::Corrupt`, latch | SB15 resyncs from the anchor |
| missing record after open | cold resolution | treated as corruption | the same resync |
| `register` or `subscribe_at` after the latch | hub, driver | `Refused::Latched` or `SubscribeError::Latched`; `finish()` not called | — |
| `finish()` returns `Err(e)` | driver | `SubscribeError::Finish(e)`; the slot stays inactive; the tip state is untouched | CH2's |
| `finish()` panics | under the tip lock | `parking_lot` does not poison; the state is untouched (nothing changes before `finish` returns `Ok`) | CH2's latch policy |
| a `Notify` panics | `Waker::wake_by_ref` of a misbehaving custom waker | every push and cursor update is recorded before the notify, so the state stays consistent; the panic unwinds into the writer | a writer panic aborts (CH1's `GuardedEngine`, SB14's supervisor) |
| a subscriber never takes events | — | its queue fills; the writer waits with a cursor set; V keeps servicing (rule 5) | the hub deregisters it (CH2's watchdog); `deregister` releases the writer even inside the hold, and never blocks (H4, DEC-157 (2)) |
| `register`/`subscribe_at` during a hold | any caller | `HoldActive`; nothing changed | `on_release`, retry (§4.3) |
| `register`/`subscribe_at` on the hold owner's thread | the writer's own code | `WouldDeadlock`, every build | a bug in the caller |
| an SPV step over `step_headers` | SB14 | `ChainError::Config`, before any work | the caller slices (SB5-Y2) |
| any other step sub-cap overrun | the step budget | `ChainError::Internal`, the latch trips, `debug_assert!` | a derivation bug: the client stops, loudly (§6.2) |
| a cold error inside `activate_segmented` | sub-step k ≥ 1 | the latch trips; `Hold::abandon`: no snapshot or status stored | restart; `open` restores `meta.tip` (§4.8) |
| a `FeedReader` dropped without `deregister` | subscriber | `closing` set, the producer and W notified; the slot is reclaimed lazily | the writer's next table scan, or the next `register` (§4.3) |

### 9.2 What the latch means for feeds

**Feeds keep delivering after the latch (D20).** Every queued event was published after a successful commit. Every persistence path checks the latch (§5.4), so nothing derived from a failed cold read becomes durable.

## 10. Test plan

Tests run on `MemHeaderStore`, except where SQLite is named; SB13 runs the same suites against SQLite (OQ3).

### 10.1 F-rows → tests

| Row | Test(s) | What it checks |
|---|---|---|
| F1 | `f1_order_from_cut` (P1); `f1_cursor_order` | random steps, compressions and takes: each subscriber's applied sequence equals the writer's from its cut; a lagging target never receives k+1 before k |
| F2 | `f2_seq_before_fanout` | `seq` assigned while the target is full; `snapshot().tip_seq` equals it while it is pending |
| F3 | `f3_capacity_with_staging`; loom L1 | `used ≤ Q` under push, intake, pop and compression; a full queue sets the cursor and the writer is `Blocked` until space |
| F4 | `f4_control_before_data` (Sol's four-step witness as a directed threaded test, with a pause point between the two `next()` calls); `f4_control_bypasses_full_queue`; `f4_coalesced_moves_to_back`; loom L5 | control pushed before data is returned before it; control is delivered while data is full; a coalesced payload is delivered once, after the other outstanding kinds, carrying the newest payload |
| F5 | `f5_wake_on_enqueue`, `f5_wake_on_control` (X23 analogue); loom L2 | no lost wake with the `set_waker`-then-`next()` protocol |
| producer wakes | `space_wake_resumes_parked_producer`, `dereg_wake_releases_producer`; loom L1, L3 | — |
| DEC-152 | `v_resumes_after_paused_slot_holder` (§4.5); `slot_sections_do_not_allocate` (a counting allocator asserts zero allocations **and zero deallocations** inside slot sections over 10⁵ operations, including coalescing sends and waker replacement; Sol r2); loom L9 | the writer's blocking contract |
| DEC-155 (1) | `control_outstanding_per_kind` (proptest: random sends, `next`, compressions and pops; at every point the slot holds ≤ `MAX_CONTROL_KINDS` messages, each kind ≤ 1, every consumed payload is the newest sent for its kind, and the delivered sequence is a subsequence of the send order, n1); `control_kind_out_of_range` (n8); `peak_control_mirror_pairs_between_intakes` (A-F12) | per-kind bound and replacement semantics |
| §4.8 sub-steps | `segmented_equals_single` (proptest: random reorgs with d + c up to 5E; the F1 stream of `activate_segmented` equals the unsegmented reference event for event, **every sub-step has d_i + c_i ≤ E** (m10), and readers see only the pre- and post-step snapshots); `segmented_subscribe_returns_hold_active`; `segmented_close_during_hold`; **`segmented_status_invisible_during_hold`** (m2: a reader polls `entry(id).status` during a paused `wait()` and sees no new CONFLICT mark until the release); **`segmented_cold_error_mid_hold`** (m3: a store fault at sub-step 2's leg read trips the latch; no snapshot or status is stored after it; `open` restores `meta.tip`); **`spv_single_segment_publish_before_notify`** (m9: a subscriber woken by the step's `Connected(b)` finds `snapshot().tip_seq ≥ seq`); `fence_by_tip_seq` (an intermediate sub-step's event is resolved by id, never judged against the older snapshot); loom L11 | §4.8 |
| m5 | F0's `static_assertions::assert_impl_all!(HeaderChain<MemHeaderStore, Evicting>: Send)`; `header_chain_moves_to_writer_thread` | §3.3 |
| F6 | `f6_parked_costs_no_thread` | 64 parked subscribers on a 2-thread pool make progress after wakes (X17 analogue) |
| F8 | `f8_snapshot_coherent`, `f8_cold_delivered_id_resolves`, `snapshot_below_anchor_is_none`, `open_rejects_stored_id_at_next_id` (n4) | — |
| F10 | `x28_pending_at_handoff`; `x27_moved_on_reorg`; `t35_subscribe_stress`; loom L4 | below |
| F11 | `f11_dereg_with_pending_fanout`; `f11_slot_reuse_stale_gen`; **`f11_delayed_cleanup_after_reuse`**; `f11_closing_during_hold`; `f11_lazy_reclaim_before_table_full` (a closing entry whose step 4 lost the `try_lock` is reclaimed by the writer's next scan, and `register` at `max_feeds` reclaims before reporting `TableFull`) | below |
| H2/H4/H7 | `h7_closing_retires_during_hold`; loom L7; CH2b's A-40/A-41 against the real slots | below |
| **DEC-157 (2), M1** | `hold_dereg_from_shared_events_thread` (Opus high case 1: S1 and S2 on one events thread, V in an ABC hold with S2 full; S1's driver deregisters, then the same thread pops S2; negative control: a blocking step 4 deadlocks); `spv_hold_subscribe_on_two_workers` (case 2, on `current_thread` and on a 2-worker runtime: two subscriber tasks call `subscribe_at`/`deregister` during a multi-segment `activate_segmented`; both return without blocking a worker and the lagging subscriber is polled); `dereg_under_hub_lock_during_hold` (case 3: a model hub lock that V takes inside the hold to answer `FlushDue`, and a watchdog holding it while it calls `deregister` and `register`; negative control: blocking calls deadlock); `hold_active_release_wake` (register-then-retry: no lost wake); `hold_owner_refused` (release build: `register`/`subscribe_at` on V's thread inside a hold return `WouldDeadlock`); loom L3 with step 4 contended, L12 | §4.3 |
| **DEC-157 (1), M2** | `spv_lo9_actor_cycle` (a model network actor with a bounded header ingress channel; the filters task awaits a `cfilter` reply before popping while the header writer waits in a multi-segment hold): with the marker on, the await fails with `WaitOnNoWaitTask` and the run completes; with the marker off and a blocking `send`, the run deadlocks within the test's timeout (negative control); the compliant shape (post, then pop; `try_send` ingress) completes; loom L13 | §4.9 |
| DEC-153 / CH1-B1–B3 (§4.7) | `hold_q1_two_disconnects_parked` (threaded, real `HeaderChain` with a model V loop: Q = 1, an ABC-style `hold_tip` with two consecutive disconnects, S parked on its `FlushDue`); its `_closing` and `_crashratio` variants; `hold_no_rm_command_inside` (a class R query queued during the hold is answered only after `release_tip`); `hold_snapshot_at_release` (readers see the pre-hold snapshot throughout; the release snapshot's `tip_seq` is the last step's); `hold_owner_refused` (every build: `register`/`subscribe_at` on V's thread inside a hold return `WouldDeadlock`, never deadlock); loom L10 | §4.7 |
| status | `status_events_not_in_f1`; `status_coalesces`; `chainlocked_only_when_active`; `status_arm_race` (a directed publish between `arm`'s check and its registration); `status_reader_drop_removes_waker`; `status_publish_order` (n7: a woken reader's snapshot already has the facts' tip); loom L8 | — |
| wake adapters | `parker_permit_before_park`, `parker_timeout`, `tokio_notify_adapter_permit` (a `Notify::Task` from a tokio task: a wake before the task's `poll` returns `Pending` is not lost), `registry_publish_stress` (the production `ArcSwap` path under 8 threads; `cargo miri test` on its small variant) | the production adapters loom does not model (§10.2) |

**The directed cases** (ports of CH2 §4.4.3 and SB0 §6.1):
- `x28_pending_at_handoff`. Subscriber S1 is full, so `Connected(b, 7)` sets S1's cursor to 7. S2 registers and runs `subscribe_at(p = b)`, getting `seq_synced = 8` and no cursor. S1 takes; `retry_pending` pushes 7 to S1 only. The writer then connects c (8), which reaches both. S2 never sees 7.
- `x27_moved_on_reorg`. The snapshot ends at A3, and the writer reorgs to B3. `subscribe_at(A3)` returns `Moved`, `finish` is not called, and the fresh snapshot's `at_height(3)` is B3.
- `t35_subscribe_stress`. There are 16 subscribers, each running catch-up followed by `subscribe_at`, retried on `Moved`. They race one writer doing random reorgs of depth ≤ 20 for 10⁵ steps, with Q ∈ {1, 2, 8}. At the end every local chain equals the writer's, and no `FeedOrder` occurs. It runs under TSan in CI, plus a 10-minute nightly soak.
- `f11_dereg_with_pending_fanout`. Three full targets hold cursors at 5. One deregisters, and `retry_pending` clears its cursor. No push reaches it after `deregister` returns, and the writer finishes when the other two take.
- `f11_slot_reuse_stale_gen`. Deregister A, then register B at the same index. Every call with A's id is a no-op or `Stale`, and A's old reader sees nothing of B's.
- **`f11_delayed_cleanup_after_reuse`** (Sol r1). A's `deregister` sets `closing` and pauses before step 4. B's `register` reclaims A's entry lazily and installs B at the same index. When A's step 4 resumes, it finds B (`ptr_eq` fails) and removes nothing. B keeps receiving events.
- `h7_closing_retires_during_hold`. In the node's `enforce_block` with three disconnects and a full target T, T's hub calls `deregister` while V waits under the guard. `deregister` returns at once (its `try_lock` fails); V's `retry_pending` clears T's cursor and reclaims the entry, and the enforcement proceeds. W's `Parker` is notified once.

### 10.2 Loom models (`RUSTFLAGS="--cfg loom" cargo test -p dash-chain --release --test loom`, nightly CI group)

**What runs through the facade (R1-m2).**
- **Production code under loom.** The production algorithms run unchanged through the `sync` facade: the slot protocol (`try_push`, intake, `next`, `pop`, compression, `send_control`), the cursors and `retry_pending`, `closing` and `deregister` with its `ptr_eq` revalidation, the `Parker`, the status `arm`/publish, and `ColdCache::get_or_load`/`invalidate`. Each model drives the real operations with their real lock boundaries. A model never merges two API calls into one action, which would hide R1-M1-class bugs.
- **Replacements and the synchronization they add.**
  - `Published<T>` (`ArcSwap`) becomes `loom::sync::Mutex<Arc<T>>`. This adds a lock edge, so loads become acquire-and-release, which is stronger than `ArcSwap`'s. No model may rely on that edge: every property that crosses a publication is also justified by an explicit SeqCst atomic (`closing`, `live`, `version`) or a slot lock, and the production `ArcSwap` path is tested by `registry_publish_stress` and Miri.
  - `lock_arc` becomes a borrowed guard. The owned-guard path is covered by `h7_*`, A-40 and A-41.
  - The store is an in-memory map with a loom mutex.
- **Wakes are modelled explicitly.** `Notify::Thread` is the real `Parker` on loom's `Mutex` and `Condvar`. `Notify::Task` is modelled as a `Parker` (permit semantics), and its production adapter is the tokio test above.
- **Scope.** Loom checks safety and detects deadlocks for the modelled schedules, under a bounded number of preemptions (`LOOM_MAX_PREEMPTIONS=3`). It does not prove scheduler fairness or bound the producer's latency (§4.5 states the contract; the directed test and benchmark measure it).

| # | Threads | Property | Planted negative control (must fail) |
|---|---|---|---|
| L1 | producer (push, park, retry) ‖ reader (intake, pop), reader preempted inside each slot section | the producer always resumes; `used ≤ Q` | lost permit (`Parker` without a stored permit): deadlock |
| L2 | producer (push, notify) ‖ reader (`set_waker`, `next()`, park) | the reader never parks with an item queued and no wake pending | missing re-check after `set_waker` |
| L3 | producer with a cursor on S ‖ `deregister(S)`; and `register` reusing the index ‖ A's paused step 4; rev3: step 4's `try_lock` contended by a hold, with the lazy reclaim | the producer resumes; no push to S after `deregister` returns; A's cleanup never removes B; the entry is reclaimed exactly once | `retry_pending` ignoring `closing`; cleanup without `ptr_eq`; a blocking step 4 (deadlock) |
| L4 | writer (two steps, one pending for S1) ‖ `subscribe_at(S2)` | X28: S2 never receives the pending event; every event with `seq ≥ seq_synced` arrives exactly once | the slot gets a cursor at `subscribe_at` (rev2's handoff) |
| L5 | `send_control` ‖ producer push ‖ reader `next()`, `next()`, `pop()` as separate operations | Sol's witness: control pushed before data is returned first; rev2's slot-resident controls (Appendix B) | rev0's `pop_control()` then `front()` (two critical sections) |
| L6 | cold reader (miss, read, insert) ‖ writer (commit row, invalidate) | after `invalidate` returns, the cache never holds the pre-commit record | unlock around the store read |
| L7 | `deregister` ‖ a model W with two barriers: one captured **before** the `closing` store and one **after** | each barrier's wait ends: the first by the predicate re-check after `close_hook`, the second by the capture filter (H7 (a), (b)) | W without the re-check after arming its parker |
| L8 | status writer publish ‖ reader `arm` | a reader that armed and saw no change is notified | `arm` without the re-load |
| L9 | producer fanning out to two slots ‖ reader A preempted inside its intake ‖ reader B popping | the producer's step completes once A proceeds; the only thing it blocks on is A's slot section (loom's deadlock detection) | an intake that allocates inside the section (asserted by a facade allocation hook) |
| L11 | SPV writer: one hold with three sub-steps (E = 1, Q = 1), `retry_pending` on the held guard and `wait()` between sub-steps ‖ subscriber at Q = 1 ‖ a `send_control` thread queuing two same-kind messages | no deadlock; no sub-step starts with `pending` non-empty; events arrive in the single-composite order; ≤ 1 outstanding message of the kind, and the consumed payload is the newest | the next sub-step planned before the drain (the step budget's charge assertion fires); the kind flag cleared at intake instead of consumption (the held count exceeds the kinds) |
| L12 | V: one hold with S2 full ‖ one events thread running `deregister(S1)` then S2's pop; variant: the events thread holds a hub lock that V takes inside the hold; and V: one hold ‖ `register` from another thread (`HoldActive`, `on_release`, retry) | no deadlock; S1's entry reclaimed exactly once; `register` completes after the release | a blocking step 4 (both variants deadlock); `on_release` without the retry (lost wake) |
| L13 | SPV header writer in an in-hold `wait()` with a pending event for F ‖ F awaiting an actor reply before popping ‖ the actor forwarding a `headers` message to the writer's full ingress channel, then replying | no deadlock | a blocking ingress `send` |
| L10 | V: one hold (the tip mutex held throughout), two consecutive disconnects, per-step drains via `retry_pending` on the held guard, the in-hold wait (FlushDue only), then a third step ‖ S at Q = 1, parked on its `FlushDue` answer before taking events; variants: S closes while parked; `-dbcrashratio` (a W thread completes only after S acks event 2, V's completion wait runs `retry_pending`) | no deadlock; no step starts with `pending` non-empty; S gets 1 then 2; the hold is never released between the steps | the in-hold wait not answering `FlushDue`; `retry_pending` re-locking the tip mutex; closing ignored by fanout and `retry_pending`; the completion wait without `retry_pending` (each a deadlock) |
| L14 | SPV by-id reader (`resolve_by_id` over the production cache) ‖ writer (set the entry, issue its id, commit its record, evict it) | an issued id is never refused as one with no record | the watermark read after the resident lookup |

The probes in Appendix B already exercise L1, L5 (rev1 and rev2 selection), L8, L10, L12 and L13 and their negative controls on minimal models. L12 models the tip mutex at user level (a flag and a condvar): loom 0.7.2's own `Mutex::try_lock` is unusable for this race, because a successful acquire marks every thread whose next operation touches the mutex, a pending `try_lock` included, as blocked (`rt/mutex.rs:116`), which reports a false deadlock. Each negative control must fail in its own test binary, because loom's deadlock panic during an unwinding test aborts the whole binary (observed in the probes).

**Erratum D61–D66 (F3's tests).**
- **D61: names.** Several §10.1 rows landed inside one test, whose doc names them.
  - `segmented_hold_waits` holds `segmented_status_invisible_during_hold`, `segmented_subscribe_returns_hold_active` and `segmented_close_during_hold`.
  - `hold_q1_two_disconnects_parked` holds `hold_snapshot_at_release`.
  - `hold_owner_refused` is two tests: `register_is_refused_during_a_hold` (`register` on V's thread) and the model test `hold_owner_refused_and_release_wake` (`subscribe_at`).
  - `header_chain_moves_to_writer_thread` has no test of its own: `spv_hold_subscribe_on_two_workers` and `spv_lo9_actor_cycle` move a `HeaderChain` to the writer's thread.
  - The negative controls of `hold_dereg_from_shared_events_thread` ("a blocking step 4 deadlocks") and `dereg_under_hub_lock_during_hold` ("blocking calls deadlock") are loom's `loom_l3_neg_blocking_step4` and `loom_l12_neg_blocking_step4_under_hub`, which plant a blocking step 4 (`deregister`). The threaded tests have none of their own, and no test plants a blocking `register`: one would fail the threaded tests' hub-lock deadline and `elsewhere` timeouts rather than a negative control.
  - `hold_no_rm_command_inside` is CH1-node's. SB5 has no command queue; the class R/M policy is V's loop (CH1-node §9.6).
- **D62: M1 case 2's two hub tasks.** In `spv_hold_subscribe_on_two_workers`, task X deregisters a feed and calls `register`, and task X2 calls `subscribe_at`. Both get `HoldActive` while L lags inside the hold; L takes the rest only after both are refused, so a blocking hub call would take both workers on the 2-worker runtime. After the release X registers and X2 gets `Moved`. The test's writer drains after `activate_segmented` returns, as the node's loop does: the last sub-step fans out at the release and can stay pending. The 30 s deadline runs on the test thread, because a worker blocked on a hold would stop the runtime's own timer.
- **D63: L11's second negative does not apply.** "The kind flag cleared at intake" assumes controls staged in the reader. Since D29, `next()` consumes a control inside the slot section, so there is no intake to plant it in. L11 still asserts that the slot holds at most one message of the kind, one per outstanding kind, and that the newest payload is consumed last. Its planted negative is the sub-step planned before the drain, which the writer's own check (`before_step`, the one the chain runs) refuses with `Internal`; this replaces the L11 row's "the step budget's charge assertion fires". The model table runs no such check of its own, because the table must work when a writer breaks the rule (F1). The negative fails while other threads unwind, which aborts loom's binary too, so it runs in a child process as the deadlock negatives do. L12's case without the hub lock is L3 (c); `hold_loom` adds the hub-lock and `register` variants. L10 plants "closing ignored" in `retry_pending` only: a fanout that ignored `closing` would queue to a closing slot that no step waits on, so it would not deadlock.
- **D64: M2 models the marker and the actor.** `spv_lo9_actor_cycle` uses a thread-local no-wait marker, a model `ActorReply` that checks it, and a model network actor over a capacity-one ingress channel. SB14 owns the real ones (SB5-Y2). A fourth shape, F awaiting with no marker while the actor uses `try_send`, shows that rule alone breaks the cycle too. The negative control waits up to 30 s for the cycle to form (F awaiting, the actor inside its blocking `send`). It counts as deadlocked once the writer has stayed unfinished for 2 s after that with the actor still in `send`; the test then drops the header task's ingress, which breaks the cycle, and checks the run finishes.
- **D65: A-40/A-41's feed halves.** `enforce_deep_walk_feed_half` runs a 200-block `enforce_block` walk on the real slots, with V's waits being the per-step drains alone. It covers H4 (a `deregister` returns at once), H5 (every step reaches a live subscriber during the hold), H7 (V's drain drops what a closing target was owed), `subscribe_at`'s `HoldActive` then `Moved`, and one snapshot store. W, the CRITICAL waits and the `FlushCommit` checks stay with CH2b.
- **D66: A-23 as SB5 runs it.** The every-store observer is a test-only hook in `feed::after_publish`, which follows the one snapshot store, so SB4's files are unchanged. n8 is asserted as exactly one store per committed step: SB4's stage 4 publishes on a quiet step too. Stage 4 runs through `activate_segmented` at E = 2.

**Erratum D67–D69 (the rebase onto SB4's fix r1, 69745378a).**
- **D67: SB4's two corruption tests clear SB5's cold cache.** `a_cold_link_off_its_height_is_corruption` and `a_resident_entrys_cold_parent_off_its_height_is_corruption` corrupt a record after the step that read it, so the cold cache (F5b) still holds the clean copy and the walk never sees the fault. Each test now calls `invalidate_cold(Clear)` after planting it, as SB5's own cold-error tests do. The two lines sit in SB4's `spv_tests.rs`, in the F5b commit, which is the one that adds the cache; they await SB4's approval, or SB4 takes them on its branch.
- **D68: an off-height cold parent is a store corruption at `open`.** SB4's `cold_parent` (Sol r1 m1) refuses a stored parent off the height below as it reads the record, before §8 step 7's own check. Before SB4's fix r2 its store errors carried no kind, so `open` returned `OpenError::Storage`, not `Corrupt`, and SB15 would stop rather than resync. Fix r2's `StoreError::{Io, Corrupt}` restores §8's mapping: `open` maps `StoreError::Corrupt`, from its own reads, a chain call's cold read or a `?`, to `OpenError::Corrupt(Corruption::Store)`, and anything else stays `Storage`. `open_refuses_a_failed_path_off_its_heights` expects `Corruption::Store` with the latch tripped, and `open_read_failures_trip_the_latch` expects `Storage(Io)` for every injected read failure. Step 7's own check (a FAILED path's parent off the height below) can no longer fire, since every cold `prev` passes `cold_parent` and every resident one `load_record`, and stays as a second line.
- **D69: `open` checks every loaded skip's target height, cold targets too.** This supersedes §8's "against resident targets only". SB4's in-memory walks now panic on a resident link off its height, as Core asserts; `open` already refused such links (prev at `load_record`, resident skip targets at `check_skip`). A cold target never becomes resident again (Evicting's arena takes new ids only; re-extension below the window re-pushes ids, not entries), so a bad cold skip cannot panic, but a later cold walk would report it only as a store corruption (D68). `check_skip` therefore reads each cold target's record, its id and height, so the fault is refused at `open` as `Corrupt(SkipHeight)` and SB15 resyncs. Only skips that jump below the window base are cold: 57–71 reads at w = 10,000 over tips from 40,030 to 2,345,678, about 0.4 ms on SQLite. The PF0 `open` line moves by those reads. `open_refuses_window_links_off_their_heights` plants a record one above its parent's next height (`MissingParent`) and a skip to a resident and to a cold record off `GetSkipHeight` (`SkipHeight`), each with the latch tripped.

**Erratum D70–D72 (F8).**
- **D70: P2 and the directed scenarios as built.**
  - **The reference chain.** `Permanent` cannot be anchored (SB4's `new_anchored` refuses it with a `ConfigError` in its fix r2), so P2's unwindowed side is the same SPV chain, `HeaderChain<MemHeaderStore, Evicting>` at w = 1,000,000: nothing it holds ever leaves the window. Its stage 4 runs in one sub-step (E = 1,024) against the windowed side's E = 4, so P2 also checks segmented against unsegmented activation. Both sides restart at the same committed cuts: §8 recomputes `best_header` and starts `tip_seq` at 0, so a side that did not reopen would differ by design, not by a fault.
  - **The operations.** P2 extends from the tip, from a branch end or from any held block (stale branches below the floor included), applies locks and toggles the spork; SPV has no `invalidateblock`. Coverage over 64 cases: 134 reorgs deeper than w forking below the base, 539 floors with marks, 220 CONFLICT marks, 105 restarts. `check` compares answers, every retained snapshot and the locators for equality. Until SB4's fix r2, whose SPV locator walks cold ancestors to the anchor (DEC-140), it compared the locator as a prefix (D74).
  - **Sizes.** The directed scenarios sample `ancestor`/`lca` every 997 heights, not at every height, which kept the suite at seconds rather than 186 s. `w_a9000_b9000` and `w_restart_mid_extension` share one body. `budget_feed_cap` runs 2,000 steps, not 10⁵, with its readers draining after each step, so the controls' payloads are checked to net to nothing; the per-feed and total caps are asserted on what `register` and `subscribe_at` allocate, which includes each slot's queues at their full capacity Q. The 50,000-header variants of the deep reorg and the mark walk and the 100-chunk out-of-order fixture are `#[ignore]`d for the nightly group, like SB4's A-31. `peak_deep_reorg_paused_readers` runs without A9 only, the configuration the cap is derived for.
  - **What A-F12 asserts beyond §10.4's list.** The counting allocator charges a growing buffer's old and new allocations together (a copying `realloc`). A step's observed size subtracts what the step keeps; pre-step bytes freed before its peak and residency allocated after it would understate the peak, and in these steps both happen after it (stage 5's cache clear and overlay drain). `peak_deep_reorg_paused_readers` asserts the writer waited, equal results for 1 and 5 readers (and for d = 9,000 and 50,000, nightly), and nothing pending after the drain; the budget account keeps peaks only (D58), so §10.4's "back to its base" is the empty pending fanout. The mark walk checks the clear-all on an unmarked cold record (the anchor), and that the step reads fewer records than it marks (1,150 by-id reads for 9,000 marks). `peak_out_of_order_chunks` cross-checks its chunk count against the bytes the rounds keep (1,969,360 B for 8 pinned chunks of 245,760 B), so a chunk kept alive past its last resident entry would show. `budget_feed_cap` asserts the controls' payloads net to exactly zero.
  - **`budget_overrun_is_internal_error`** covers the one sub-cap with a runtime check: the sub-step term (legs, `TipChange` and pending copy, §6.2's erratum D58), inflated by a test-only hook, is `Internal` with the latch tripped and nothing published; no `debug_assert!` fires first. The A5 list and the fixed term have no runtime check, by construction: the list clears the whole cache past C, and the fixed term's containers are sized by the configuration. A batch over `step_headers` is `Config` before any work.
  - **Names.** n4's `open_rejects_stored_id_at_next_id` is `open::tests::open_refuses_inconsistent_meta_and_rows`; m2's visibility test is `segmented_status_invisible_during_hold`, inside `segmented_hold_waits` (D61).
- **D71: the marks term misses SB4's enforcement lists, and stage 1 builds no plan.**
  - **Lists.** A lock's marking walk keeps each disconnected id twice: in the step's `VirtualTip.disconnected` and in `EnforceReport.disconnected`, two `Vec<BlockId>` that grow together, so at their last doubling both hold an old and a new buffer. §6.2's M(m) counts only the overlay. The adversarial bound is cap + M(m) + L(m), with L(m) = 12 · next_pow2(m) ≤ 24m; M(m) + L(m) ≤ 55m + 64. Measured: 9,000 marks observe 408,568 B ≤ cap + M + L = 607,648 B (mobile: 189,824 + 221,216 + 196,608); 50,000 marks observe 1,465,336 B ≤ 1,861,024 B, against cap + M = 1,074,592 B, which it exceeds. The marks account's own peak stays within M(m) (589,840 B ≤ 884,768 B at 50,000, 11.8 B per mark). SB4 could drop the report's copy in SPV, where the `VirtualTip` already holds the list.
  - **Stage 1.** The SPV step's stage 1 must not build SB4's full `tip_plan`: its legs scale with d + c (Sol's witness alone allocates 1.44 MB), which is what the sub-steps exist to avoid. Stage 3 reads the move's shape from `tip_target`, and each sub-step plans its own legs with `tip_plan_bounded` (A11 (c)). `SpvRig` (test-util) runs the step this way. SB14's driver and SB4 §9.1's stage list should say so.
- **D72: §6.3's typical totals at the measured sizes.** `mem_spv_typical_w10000` and `_w2048` count everything the writer's thread allocates from construction on (the pack's 25 store rows included), except the store's work during later steps, and peak at 5,021,953 B and 2,082,185 B against §6.3's 4.42 MB and 1.58 MB. Three terms account for the difference, and the tests' bounds add them to the table's totals:
  - **The arena entry is 240 B**, not SB4 §3.8's 200 B estimate (`size_of::<Entry>()`: the `OnceLock<EntryImm>` and the seqlocked group). A chunk's entries are 245,760 B (plus a 160 B header and eviction bitmap): +532,480 B over 13 chunks on desktop and +204,800 B over 5 on mobile. The worst case by construction (chunks ≤ N_win + B) scales by the same 1.2, and §6.3's 100-chunk fixture is 24,576,000 B.
  - **The arena's fixed tables:** the first level, 4,096 slots × 32 B = 131,072 B, and one second-level table of 1,024 chunk pointers per 2^20 ids, 8,192 B.
  - **The decoded `headers` message**, B × (112 + 32) B = 288,000 B at B = 2,000, is on the writer's thread until `accept` consumes it. §6.2 places it outside the step budget (dash-p2p's account), and §6.3 leaves it out of the total; a resident-memory figure for the SPV process should include it.
  - **Corrected typical totals:** 5.38 MB on desktop and 2.21 MB on mobile, as bounds. DEC-187 adopts them: §6.3's table now carries the measured arena row, the fixed tables and the message, and PF0's memory rows gate the residency (D73). The design's "typical rows at estimate × 1.25" margin is not used: the arena row is at its measured size, and the other typical rows came in under their estimates.
- **D74: the rebase onto SB4's fix r2.**
  - **The duplicate list (DEC-187 (1)).** In SPV, `EnforceReport.disconnected` is now empty and the list is the step's `VirtualTip`'s; SB5's SPV assertions read `vt.disconnected()`. The mark walk drops by one list, 4 · next_pow2(m) B: 65,536 B at 9,000 marks and 262,144 B at 50,000, measured by reverting only that change. A-F12 keeps its bound at cap + M(m) + L(m) with L(m) for both lists.
  - **The overlay entry.** Fix r2 made the overlay `HashMap<BlockId, Mark>` (the status, and whether stage 4 applied it), a 12 B entry and a 13 B bucket. M(m) is now 13 · (b + b/2) + 32 ≤ 45m + 2,528: 319,520 B at 9,000 marks and 1,277,984 B at 50,000. The step budget's marks account sized the table as `(BlockId, Status)` and under-reported it (147,472 B for a 213,008 B table at 9,000); it now sizes the overlay from the map's own types, and A-F12's M(m) uses `test_util::OVERLAY_BUCKET_BYTES`. The two changes net: 9,000 marks observe 440,824 B ≤ cap + M + L = 705,952 B (mobile: 189,824 + 319,520 + 196,608), and 50,000 observe 1,595,896 B ≤ 2,254,240 B. The account equals the new table exactly, 13 · b + 16: 213,008 B at 9,000 marks and 851,984 B at 50,000 (17 B per mark), and A-F12 asserts that equality, so a misreported table fails it. The bucket count has a floor of 128, the capacity of 64 the overlay keeps between steps, so M(m) covers a later step of few marks too.
  - **Typed corruption.** D68's mapping is applied (`Corruption::Store`). SB4's new rows `an_unissued_id_read_from_a_record_is_corruption` and `a_parent_at_the_last_height_is_corruption` plant a fault after the step that read the record, so they clear the cold cache after planting it, as D67's rows do.
  - **The rest.** P2 compares locators for equality. The status bench calls V's best-header hooks through `node_index()`, fix r2's node-only view. SB4's public-API listing (A-13) had SB5's stubs; an SB5 commit blesses the listing with SB5's API.
- **D75: paged children and cold leaves (Sol r1 R1-M1).**
  - **The read.** `HeaderStore::children` and `leaves` become `children_from(prev, from, out)` and `leaves_from(from, out)`: up to `out.len()` ids from `from` up, in id order. The chain passes a page of 64 on its stack, so enumerating a wide stored tree allocates nothing per sibling or leaf, and reads on from past each page's last id until a page is empty. A short page is not the end: a store that drops rows from a page (as `MemHeaderStore` does for a record a fault hides) would otherwise silently lose the rest, and fork choice with it. SB13 answers both with an id-ordered range read (`id >= from … LIMIT n` over `hdr_prev` and `hdr_leaf`); the end costs one empty read.
  - **The merge.** `Children` merges the store's pages with the step's dirty set in id order, deduplicating equal ids, so fork choice sees the same children in the same order. `cold_leaves` skips resident ids and the uncommitted parents (one parent mask per page) and folds `max_cold_leaf_work` only after a complete read; a stale value stays a safe upper bound.
  - **Corruption.** A page that is out of id order, or below `from`, is `StoreError::Corrupt` and trips the latch.
  - **Cost.** A quiet step still reads every cold leaf's page: O(width) store reads, O(1) memory. The parent mask scans the dirty set once per page, O(|dirty| · pages) arena reads; the dirty set is the step's new records.
  - **Tests.** `paged_tests` (three pages merged with the step's ids, a page boundary at 128, short pages before the end for children and leaves, a page out of order, the dirty set's id-order scan from below its words and at `u32::MAX`), and `mem_store`'s `pages_match_the_full_reads`.
  - **SB4 files.** `store.rs` (the trait), `chain.rs` (`Children`, `cold_leaves`, `Dirty::first_from`), `enforce.rs` (its two call sites), `shared.rs` (the checked page reads) and `spv_tests.rs` (call sites), as the manager cleared for R1-M1.
  - **A-F12.** Sol's wide tree (9,000 siblings, w = 32, mobile) observes 432,528 B on the lock step (was 4,651,904 B), within cap + M + L, and 44,768 B on the quiet step after it (was 4,289,664 B); after a reopen, 385,056 B and 44,768 B.
- **D76: the registry and path terms (Sol r1 R1-M2; DEC-187 amendment 2 (a)).**
  - **The registry.** `MAX_FEEDS` = 64, checked by `FeedConfig::validate` (`FeedConfigError::MaxFeeds`). The registry is reserved at `max_feeds` entries at construction and at every republish. `registry_bytes(max_feeds)` = 2 · (16 + `size_of::<Registry>()` + `max_feeds` · `size_of::<RegistryEntry>()`), 736 B at 8 feeds and 5,216 B at 64. Sol's 64-feed scenario (B = 5, E = W = C = 1, 8 deregistrations at each wait) observes 7,344 B ≤ the cap, 19,588 B.
  - **The paths.** `ACTIVE_PATHS_BYTES` = 2 · `PATH_COPY_BYTES`: a leaf and an inner node per level to `MAX_DEPTH` = 4, each in its `Arc`, 6,384 B a path. Two, not one: a step copies the tip's path out of the published view, and a disconnect below the window's base copies the base's (`extend_down`). A pop that empties a copied leaf frees it, so a deep disconnect holds one copied leaf at a time. `active.rs` (SB4's) gains the constants.
  - **What the term does not cover (Opus review M1).** Stage 4 publishes once per step, so the pre-step view holds its tree until the release. A reorg that crosses leaves pops them and pushes fresh ones for heights the view still holds: up to the window's leaves when d ≈ c ≈ w. Those are not copies. A fresh node is residency, allocated under `fault::residency` (`active.rs`'s `fresh`), and the originals are the published view's, one retained snapshot (§6.3's row, ≤ 53,248 B on desktop and 20,480 B on mobile) until the release. A-F12's residency bound counts the active chain twice for it.
  - **Exact.** `exact_path_term` counts one push at the deepest heights against a retained view, 6,384 B; `exact_registry_term` counts one deregistration at 64 feeds against a retained registry, 2,608 B.
  - **The fixed term stays at 1,024 B.** It no longer claims the registry, and the paths were never in it. It covers what a release publishes, the snapshot's 288 B and the status's 112 B containers, and two segment `Arc`s. Amendment 2 (a) has the two terms replace the fixed term; the publish containers still need one, so the row keeps them.
  - **Caps.** 433,728 B on desktop (was 420,224 B), 203,328 B on mobile (was 189,824 B), and 119,328 B on mobile at B = 500.
- **D77: A-F12's two accounts (DEC-187 amendment 2 (b), (c), (d)).**
  - **The batch (b).** `SpvRig::batch` builds a `HeaderBatch` and `step_batch` takes it. A-F12 builds it before the measured call and excludes its two buffers, whose free inside `accept` is not counted; each case asserts `accept` freed both.
  - **The accounts (c).** `fault::residency` (a test-util flag, a no-op otherwise, restored on unwind) wraps `insert_entry`'s arena set and index insert, SPV's resident-height and chunk-count updates, `open`'s window load, and the active chain's fresh nodes. The counting allocator holds each block allocated under the flag by its address, and its free is residency wherever it happens on the thread (a dropped snapshot's chunk included); a realloc classifies the old block before the new one is held. Everything else is the step's. So each counted allocation is in exactly one account by construction. The only bytes in neither are the store's (gated) and the batch's, and each case asserts the uncounted bytes are exactly the batch's two buffers.
  - **The resident-height set and chunk counts are residency too.** The ruling named the arena and the index. SB4's `resident: BTreeSet<(u32, BlockId)>` and `chunk_live` are also per resident entry, sized by N_win + B, and §6.2 has no term for them: in the step account the set's growth (38,980 B at B = 2,000) fit the cap only by slack. §6.3 gains the set's row, bounded from std's node layout (`RESIDENT_LEAF_BYTES` 104, `RESIDENT_INTERNAL_BYTES` 200, every node but the root at ≥ 5 keys; `exact_resident_nodes` checks the layout).
  - **The index transient.** hashbrown grows a full table of b buckets to 2b only when more than 7b/16 entries are live, and otherwise rehashes in place, so the table stays at most the smallest b with 7b/16 ≥ N_win + B, and while it grows the old table of b/2 is live beside it. On mobile that is 16,384 buckets (81,936 B), not the 8,192 §6.3 had; a long sync reaches it once a growth triggers above 3,584 live entries. `exact_index_table` checks hashbrown's table size per capacity.
  - **The cases (d)**, each a 2,000-header (or 500) step on a chain primed past its window, no marks; step ≤ cap + M(0) + L(0), and the thread's residency blocks ≤ (⌈(N_win + B)/1,024⌉ + 1) chunks of 245,920 B + the 8,192 B second-level table + the index with its transient + the resident set + the chunk counts + the active chain twice (`active_bytes`):

    | Case | step account | ≤ | residency account | held peak | ≤ | both | batch (outside) |
    |---|---|---|---|---|---|---|---|
    | mobile, w = 2,048, B = 2,000 | 53,440 | 205,868 | 534,824 | 1,366,056 | 1,528,192 | 584,120 | 288,000 |
    | desktop, w = 10,000, B = 2,000 | 188,416 | 436,268 | 535,128 | 3,551,512 | 3,914,016 | 715,256 | 288,000 |
    | mobile, B = 500 | 47,104 | 121,868 | 255,608 | 1,066,256 | 1,169,344 | 302,712 | 72,000 |

    A primed step's residency account holds two new chunks before stage 4 frees two: its 2,000 ids cross two chunk boundaries.
  - **SB4 files.** `chain.rs` (`insert_entry`'s two `fault::residency` wraps), `arena.rs` (`L2` crate-visible, for `ARENA_TABLE_BYTES`) and `active.rs` (`fresh`, `LEAF` crate-visible, the node-size constants).
  - **Review.** Opus (fix r1): approve with suggestions. M1 is the paragraph on what the path term does not cover (D76); m1 the empty-page end (D75); m2 `open`'s load under the flag; m3 the realloc order and the per-thread note; m4 the batch-bytes check, which replaced a joint-within-the-sum assertion that could not fail as described.
  - **The fix-r1 case.** The 601,080 B step (w = 2,048, mobile, 2,000 headers then 2,000, unprimed) splits into 53,248 B of step (the write batch 45,056 B and the dirty set's 8,192 B) and 296,696 B of residency: one chunk 245,920 B, the old index table 20,496 B and the resident set's 30,280 B. The 288,000 B batch is outside both.
- **D78: SB5's step helpers run without a full plan (Sol r1 R1-m1).** P2's `Side::step`, A-23's `run`, and `step_tests`' `drive` and `plan` pass `fork_target`'s `BlockRef` to `write_header_step` when the tip moves and run stage 4 through `activate_segmented` (DEC-187). `evict_after_commit_spv` follows `drive`. The tests that compare against SB4's single `set_active_tip`, `segmented_tests`' reference chain and `hold_tests`, keep their full plans.
- **D79: the rebase onto SB4 fix r2 (20422f034).** SB4 rewrote r2-g, so the branch moved with `rebase --onto 20422f034 78eb1f79e`. One textual conflict: `spv_tests`' `regtest_pack` takes SB4's `pack_of` form and stays `pub(super)` for SB5's tests.
  - **`open`.** The anchor API: `HeaderChain::empty` takes the anchor and derives the floor, and `restore_active` builds `ActiveChain::starting_at(anchor)`. A stored anchor must be genesis at 0, or another block at 24 or above (§8.4); anything else is the new `Corruption::Anchor(height)`, which trips the latch (`open_refuses_an_anchor_off_its_heights`). A tip below the anchor, not just below the floor, is `ActiveMismatch`. The test utility `DeepActive` anchors at its floor plus 24.
  - **SB4's corrupt-locator test under the cold cache.** SB4's `try_fork_in_global_index_refuses_an_id_with_no_record` corrupts a cold record's id to one with no record, then expects the by-id read to report corruption. In SPV the cold cache admits a by-hash read under the id the record names (§5.2), and the walks resolve parents by hash, so the by-id read is the cache's. `try_fork_in_global_index` therefore answers the misfiled id without tripping the latch, and the store's gap shows only once the cache drops the record. The test is now `try_fork_in_global_index_reads_an_id_with_no_record_from_the_cache` and pins that behaviour. Restoring SB4's detection needs either a by-id read after every by-hash miss, which doubles cold lookups, or a watermark of issued ids that `Shared` checks on admission, which would catch unissued ids like the test's `u32::MAX` but not a missing issued one. Neither is done here; this is for a manager ruling. Ruled: the watermark (DEC-187 amendment 3), D81.
  - **Unchanged.** The SB5 tests that read `EnforceReport.disconnected` (`enforce_feed_tests`, `hold_tests`) are node-mode, where DEC-187 keeps the list; the SPV sites already read `vt.disconnected()`. D67's `invalidate_cold(Clear)` lines re-applied cleanly. The public API gains one line, `Corruption::Anchor`. The gate's loom run adds `entry::loom_tests`.
- **D80: the fixed term from the types (DEC-187 amendment 3 (2)).** `STEP_FIXED_BYTES` is the two containers a release publishes, each an `Arc` laid out as std's `ArcInner` (two counts, then the value, padded): the snapshot, 288 B, and the status facts, 112 B, so 400 B on x86-64. A-F12's `exact_fixed_term` measures one release's publication with the old snapshot and facts held, and asserts it equals the term; the first sum, counts plus value without padding, came to 396 B and the test caught it.
  - **No segment term.** rev3's "two segment `Arc`s" are now the composite segment's two `Box<[BlockRef]>`s, which the legs term's pending copy (40 · E) already charges. The pending queue's own buffer was a one-time allocation inside the first step that pended; `TipFeeds::new` now reserves its one slot, as it reserves `entries` and `free`, since the writer rule leaves at most one segment pending. `composite_fanout_and_retry` checks that the segment takes that slot.
  - **Not in the term.** arc-swap's first `store` on a thread allocates that thread's debt node, which is reused until the thread exits: once per writer thread, never per step. `exact_fixed_term` steps once before it measures, so it never sees it; it asserts both the peak and the end at 400 B. Fanout with live feeds allocates nothing: the slot queues are reserved and assert that they never grow.
  - **Caps.** 433,104 B on desktop, 202,704 B on mobile, 118,704 B on mobile at B = 500 (624 B lower). A-F12's step bounds (cap + M(0) + L(0)) are 435,644, 205,244 and 121,244 B; the measured steps, 188,416, 53,440 and 47,104 B, are unchanged. §6.3's totals do not move at their precision.
- **D81: the issued-id watermark (DEC-187 amendment 3 (3); replaces D79's re-expressed test).** `Shared.issued` holds the writer's `next_id`: `insert_entry` stores it after each issue, the node's load after its last id, and `open` from `meta.next_id` before any record is read. `ColdCache::get_or_load` and `load_by_hash` take it and refuse a record at or above it before admission (`StoreError::Corrupt`, "never issued"); `Shared`'s read wrappers trip the latch. The watermark is loaded after the store read, never before: a reader that loaded it first could see a record the writer committed after issuing its id and refuse a valid one.
  - **Tests.** SB4's corrupt-locator test is two: `try_fork_in_global_index_refuses_an_unissued_id` (u32::MAX is refused at admission, the latch trips, and the cache does not hold it) and `an_issued_id_with_no_record_shows_past_the_cache` (a stale branch's evicted id, gone from the store, named by a trunk record: `try_fork_in_global_index` answers it from the cache, and `try_link` after the cache drops it reports "has no record" and trips the latch). SB4's `an_unissued_id_read_from_a_record_is_corruption` reads the parent by hash, so it now meets the cache's refusal before the walk's own check; the cache uses SB4's wording ("a record names entry …, never issued (next …)"), and the test asserts the cache's suffix. The walk's check keeps its own test, `a_skip_never_issued_is_corruption`: a cold record's skip of u32::MAX, read by id, through a new `Corrupt::Skip(id)` store fault. The one-slot backlog is pinned by `composite_fanout_and_retry`, not by a `debug_assert!` in fanout: the F1/P1 model tests drive a rule-breaking writer that pends twice on purpose. `cold::tests::refuses_a_record_not_issued` covers both read kinds, and `open_restores_the_issued_watermark` reads a cold record after a reopen and refuses one that comes back under `next_id`.
  - **SB4 files.** `shared.rs` (the field, `issue`, the two read-through calls) and `chain.rs` (one `issue` after `next_id` moves in `insert_entry`, one in the node's load). SB4 is told through the manager.
- **D82: an issued id with no record, at every by-id miss (Sol r2 R2-M1; DEC-187 amendment 3 (c)).** D81 refused unissued records at admission, but `ColdCache::admit` returns a store miss before it reads the watermark, and `Shared::try_node` passed the miss on, so `try_entry`, `try_locator`, `try_ancestor` and `try_lca` answered an issued id whose record was gone as unknown, with the latch clear (Sol's paging witness).
  - **Nothing deletes an issued record.** Checked first, as the ruling asks. `HeaderStore` has no delete, and a step's rows (`StepRow`) are records, status and leaf adds and removes. `write_header_step` requires the step's new ids to be dense up to `next_id`, so every issued id is written. By the step protocol (stage 3's commit, then `activate_segmented`), stage 4 evicts only after the step's commit; nothing in `activate_segmented` or `evict` enforces it, and a caller that evicted past a failed commit would now trip the latch. `MemHeaderStore::rollback` drops only a failed transaction's rows; their ids are still resident and no stage 4 follows a failed commit (tripping the latch for it is the caller's). `reset` has no caller outside tests. SB0 §9.5's fallback rebuilds with a new `Shared`, so a new watermark; the old chain's readers must be retired before the reset, since the store and the latch are shared and an old reader of an evicted id would now trip it. A reopen re-derives the watermark from the durable `meta.next_id`. So no separating rule is needed, and (c) stands as ruled.
  - **The rule.** `shared::resolve_by_id`, SPV's by-id resolution under `try_node`: read the watermark (Acquire), then the resident entry, then the cold read. An id below the watermark with neither is `missing_record`'s corruption, which trips the latch; any other miss is `Ok(None)`. The node's arenas never evict, so their path is unchanged.
  - **The order.** `insert_entry` sets the arena slot before it issues the id (Release), and stage 4 evicts it (the evicted bit, Release; the chunk release) only after its record commits. A reader whose watermark covers the id therefore finds it resident, or sees the eviction and with it the commit, so its store read returns the record under the store's read-after-commit contract (§3.7). Read after the lookup instead, the watermark could cover an id issued between a resident miss and a store read before the commit, and refuse a valid entry. A failed step leaves its ids resident and dirty, and the next step's write rewrites them.
  - **Tests.** `an_issued_id_with_no_record_is_corruption_at_every_by_id_read` ports Sol's witness (window 10, a one-record cache, the stale sibling gone and `trunk[7]` read by hash under its id, a second lookup evicting it): `try_entry`, `try_locator`, `try_ancestor` and `try_lca` each make a store read by id, report "entry y has no record" and trip the latch on a fresh chain, and an id at `next_id` answers `None` with the latch clear. Loom L14 runs the production `resolve_by_id` and cache against a writer that sets, issues, commits and evicts; its negative control reads the watermark after the lookup, and loom finds the false refusal. `by_id_readers_racing_issuance_are_never_refused` is a threaded smoke test over the real chain: it passes with the planted order too, because the window is too short, so L14 is the control. `contrib/node/check-loom.sh` runs L6 and L14 with every other dash-chain loom model (D83). The watermark moves to the loom facade (`sync::AtomicU32`) so L14 can model its ordering.
- **D83: the rebase onto merged SB4, and every loom model in `check-loom.sh` (the manager's ruling after Sol r3).** The branch moved from SB4 fix r2 (20422f034) onto `node/integration` at ad0084fc1, which carries SB4's merge (1a35c3a80). It applied without conflicts: SB4's later commits touch only its scenario tests, and the rebased tree differs from the old head by exactly integration's changes. `contrib/node/check-loom.sh` ran only SB4's seqlock model. It now runs the six dash-chain loom modules (`cold::loom_tests` 6, `entry::loom_tests` 1, `feed::hold_loom` 16, `feed::loom_tests` 8, `feed::table_loom` 11, `status_feed::loom_tests` 5) and checks each module's count. A model counts when it passes, or when it is a negative control ignored in the run because its deadlock aborts the binary; its module's `*_negatives_fail` test runs it in a child process and checks that it ran and failed. A module named `*loom*` in the loom build's test list that the script lacks fails the check, as does any count that differs. Checked by planting both: a wrong count and a dropped module each fail with their own message. The run takes about 2.5 minutes on agentbox, one test thread.
- **D84: no bench build turns on `test-util` (the manager's ruling for PF0).** The self dev-dependency `dash-chain = { path = ".", features = ["test-util"] }` turned the feature on in every dev build, benches included, so PF0 measured the residency and slot-section hooks (`fault::residency` in the arena, index and active-chain growth, `Section::enter`, the intake fault check), which production never runs. The dependency is gone.
  - **Tests.** Every integration test that needs the feature declares it: `full_chain`, `spv_memory` and `step_budget` already did, and `feed_alloc` now does. The self dependency had turned the feature on for `cargo test --workspace` too, so the workspace gate ran `feed_alloc` (4 tests), `step_budget` (19, 3 ignored) and the lib test `append_spv_records_is_the_steps_image`. It no longer builds them. `cargo test -p dash-chain --all-features` runs them all: CI's per-crate step does, and the merge gate needs that line. `cargo test -p dash-chain` without features passes too (the lib tests without the hooks).
  - **What the benches build.** `benches/feeds.rs` builds what it took from `test_util` on the public API: `append_spv_records`, with Core's `GetSkipHeight` and `GetAncestor` over the records it writes and a fixed hasher; the SPV rig (`SpvRig`: the anchored chain, the header step with segmented stage 4, five subscribers, `settle`); and the subscribe and drain helpers. It compiles `src/mem_store.rs` by `#[path]`, so it steps the store the library's tests pass. That file and its test module import from the crate root, which both crates provide; a crate-internal path there would also break PF0's build of the base's benches. `test_util::ColdBench` is gone. `test_util`'s appender and rig stay for `step_budget` and the lib test, two copies of the fixture where there was one.
  - **What `cold/*` measure.** They read through the public `ChainReader::try_entry` of an SPV chain (window 10, C = 1,024) over the anchored regtest store, where they called the crate-private cache over a synthetic table. Each read now includes the resident miss and the entry; a miss also reads the parent by hash through the same cache. Both fixtures still check their premise: about 85% hits over the 90/10 probes, and a second pass over ids 0..C with no store read, so the cache holds exactly those ids, every bit set. The group runs 10 samples, as `lru-miss-all-referenced` opens a chain in each setup.
  - **PF0.** Bench IDs are unchanged, but the bench inputs changed: the base's `feeds.rs` imports `test_util`, which the head's bench build lacks, so PF0 needs the bench-change approval. Every `dash-chain` row then compares a head without the hooks against a base with them, and `cold/*` against a different workload.
  - **The canary.** `benches/support/mod.rs` fails a release build of either bench with `test-util` on (`compile_error!` under `not(debug_assertions)`), so `clippy --all-features --all-targets` and the debug test builds still compile. It does not catch `--release --all-features --all-targets` test or clippy runs (none of the scripts run them; they now fail), nor a bench profile with debug assertions forced on.
- **D85: `snapshot/at-height-cold-1k`'s fixture checks the path the row measures (amends D51).** Its setup asserted that each of the 1,000 heights misses the cache and is one `by_id`. Below the window, `at_height` is `ancestor` from the window's base down the skip links, each link through the cold cache (a record's `prev` resolved by hash), then the entry by id, which the walk's last link has just cached. The walks share their upper links, so a pass reads the store about once per height: 297 `by_id` and 720 `by_hash` reads on the first pass, then about 294 and 716 on each pass after, the by-hash count varying by one as CLOCK's hand cycles. The assertion failed on every commit since the row landed (508274fdd), the unchanged head included; the per-commit check builds benches but runs none. The setup now runs one pass, then asserts that each of the next four reads the store at least 500 times and that they differ by at most 10 reads. The measured region is unchanged.
- **D86: a `ChainFeeds` call from inside `finish()` is `WouldDeadlock` (Opus-high review F-1).** `subscribe_at` runs `finish()` under the tip lock in an ordinary section, where `held_owner` is 0. A `register` or `subscribe_at` from inside it failed `try_lock` forever and yielded forever, and every later step and registration then waited on the lock: the chain stalled. The same mistake inside a hold already returned `WouldDeadlock` (DEC-157 (2)). `FeedShared` now keeps `finish_owner`, the subscribing thread's token while `finish()` runs, set by `in_finish` and cleared on return or unwind. `acquire_tip` refuses that thread with `WouldDeadlock`; other threads still wait the section out by yielding, since a separate token keeps them from reading it as a hold (`HoldActive` would send them to wait for a release wake the handoff never fires). The `FeedSource::subscribe_at` doc states the refusal. `reentry_from_finish_is_refused` re-enters `subscribe_at` and `register` from `finish()`, checks the outer handoff completes and that the owner is cleared after a failed `finish()`; without the change it times out at its 30 s bound.
- **D87: the registry's `seq_synced` is the handoff's (Opus-high review F-3).** `activate` set the entry's `live` from `ctx.next_seq` and returned the handoff's `seq_synced` from `ctx.next_seq` separately, and the only registry check ran at a fresh table, where seq 1 clamps an off-by-one away: the review's mutant M10 (`live` one seq early) passed every feed test. The handoff's `seq_synced` now comes from the entry's `live`, one value for both (H2). `registry_seq_synced_is_the_handoffs` subscribes after four steps and checks the handoff's `seq_synced` (5), the registry entry's, and the first event's seq. M10 on the new code (`live` one seq early, clamped at 1) fails it and six other feed tests.
- **D88: the Opus-high review's nits.**
  - **Taken.** F-7: the first close (`deregister` or the reader's drop) also notifies the reader's waker, after the slot section, so a parked reader sees the close without a timed wake; the approved close contract promised only the producer and close-hook wakes (`dereg_wakes_the_parked_reader`). F-9: `ColdCache::insert`'s replace path asserts the hash is unchanged, the invariant that lets it leave `by_hash` alone. F-5: `ChainFeeds::on_release` documents that each call adds a wake until the release, so a caller waits as the protocol says rather than looping; a cap would change the API.
  - **Skipped.** F-2 (SPV `open` reads O(tip − floor) active rows): the manager's backlog item; it changes `open`'s reads and D48/D59/§8 step 4, too much to add before merge. F-4 (public SPV `set_active_tip` is the unbounded composite): the API and its doc are SB4's `chain.rs`. F-6 (the loom hold models restate the release order): the production order is pinned by `release_wakes_after_the_guard_drops`, which killed M6. F-8 (`try_push` clones the waker per push): a node step pushes a run of one event per feed, so taking the waker once after the run would add a second slot section to every push in the common case. F-10 (a per-feed lag diagnostic): new API, for SB15. S-1 to S-3: S-1 depends on a cap (F-5), S-2 is F-2, S-3 is a candidate only.
- **D89: the review of D86–D88 (Opus validator; amends D86 and D88).**
  - **A waker set on a closed slot fires at once.** D88's close wakes the waker it takes, but a reader that took `Handoff::Live` and had not yet set one could lose the close: `deregister` closes and takes nothing, the reader's `set_waker` stores its waker after that, and the park protocol's second `next()` returns `Empty` on a closed slot, so it parks with nothing left to wake it. That window predates D88. `set_waker` now reads `closing` inside its section and, when it is set, notifies the waker after the section instead of storing it. `close` swaps `closing` before its own section, so a `set_waker` section after it sees the flag, and one before it stores the waker for `close` to take. The park protocol is unchanged. `dereg_wakes_the_parked_reader` sets a waker after the close and checks that it fires and that a later close finds none stored; with the old `set_waker` it fails.
  - **D86's unwind half is tested.** `reentry_from_finish_is_refused` also runs a handoff whose `finish()` panics, catches the unwind, and checks that the same thread can `register` and hand off again. With the drop guard replaced by a store after `finish()` it fails. `in_finish` asserts that no owner is set before it stores its own, so a failing debug assertion no longer leaves the token behind.
  - **Docs.** `WouldDeadlock`'s docs (on `Refused`, `SubscribeError`, `with_tip`, and `subscribe_at`'s unreachable arm) name the `finish()` case. `FeedSource::subscribe_at` says that a writer call locking the tip, such as `HeaderChain::pending_is_empty`, deadlocks inside `finish()`. D85–D88 moved below D84's sub-bullets, which they had split. D87's mutant count was recounted.
  - **Skipped.** The reader's own drop, when it is the first close, still notifies its own waker: a spurious wake nobody is parked on, which every consumer tolerates, and skipping it would give `close` a caller flag. §4.3's body is not rewritten for D86 or D88; the rows amend it. `ColdCache::insert`'s D88 debug assertion stays a panic on a corrupt store in debug builds, as the replace path's behaviour was before it.
- **D90: a compression that finds nothing keeps `next`'s selection (the merge gate's new `bench-test` step).** `reader/next-pop-compress-1k` tries `remove_round_trip` on each `Connected` and `pop`s when it finds none, as its row says, but `remove_round_trip` cleared the selection on every call, so `pop`'s debug assertion ("pop without a selecting next()") fired on the first fallback. The bench has failed in test mode since it landed (9f9255d73); `cargo bench` builds release, where the assertion is off, and the per-commit check builds benches but runs none (as with D85). A `None` changes nothing the selection points at: the intake is data-only and appends behind the head, and the return comes before any removal. So `remove_round_trip` now clears the selection only when it removes, and `pop`'s contract admits a `pop` after a compression that found nothing. The measured region is unchanged. `pop_after_a_failed_compression` pops after a `None` and checks that a successful compression still clears the selection.
- **D91: the tip step's publish keeps SB4's codegen (train 7's PF0: `chain-tip-step-publish` +8.5%, +8.8% for SB5 alone on c68b1c3e4).** SB5 added about 390 instructions to each `connect_tip`:
  - **Unchanged status facts.** `StatusCell::publish` loaded the published facts (an arc-swap load and its release) to compare them, on every step, about 130 instructions. The writer now keeps the key of the facts it last stored, `TipFeeds::status`, under the tip lock, and builds and publishes the facts only when the key changes. The key is the best header's id, and the ChainLock floor's id with whether it is active, packed in two words. A chain instance issues each id once and never changes its block, so equal keys are equal facts; a reopen builds a new table, whose key is unset. `enforce_block` walks with the real table and leaves a vacant one in the mutex, which nothing reads, so the key cannot go stale. The key is stored after the facts. A debug build checks, on an equal key, that the cell holds the snapshot's facts, so a store that bypasses `after_publish` fails the tests. The only other store, the test-util `republish`, unsets the key under the lock before it stores the default facts.
  - **An empty table.** `TipFeeds::fanout` was an out-of-line call even with no entry, about 75 instructions. Its empty test is inlined; the rest is `fanout_to_entries`.
  - **Lost inlines.** SB4's `HeaderChain::step` and `publish_locked`, and SB5's `after_publish`, went out of line once SB5's code was in the crate; `step` returned its `TipStep` through a stack copy. They are `#[inline(always)]`, `#[inline(always)]` and `#[inline]`; `#[inline]` did not inline `step`. The two attributes in `chain.rs` are SB4's lines.
- **D92: SB4's read paths keep their codegen under SB5's state (train 7's PF0: `chain-ancestor-random` +0.93%, `chain-accept-spv-2000` +2.7% and peak RSS +6.9%; `chain-locator-tip` +0.48%, under the threshold).**
  - **The arena's displacement.** F5b put the cold cache in `Shared` itself, ahead of `arena`, which moved from offset 96 to 240. Past a signed byte from the `Arc` pointer, x86-64 has no 8-bit displacement for the field, and every skip link of an ancestor walk formed the address with one more instruction. The cache is boxed (`Option<Box<ColdCache>>`), the arena is back at 96, and `chain-ancestor-random` equals the base. A const assertion in `shared.rs` keeps `arena` within reach for `NoStore` in both residencies (the benches' layouts; `Shared` is `repr(Rust)`, so another store's may differ). It fails with the cache unboxed.
  - **Lost inlines.** SB4's `RuleContext::height_on_path` went out of line (about 2.9M of the row's 3.7M per iteration); it is `#[inline(always)]`, and `header_of` is `#[inline]`, SB4's lines in `rules.rs`. `Shared::node` is `#[inline]`, and `try_node`'s resident read is `resident_node`, `#[inline]`; the permanent branch matches it and rewraps, because `Ok(self.resident_node(id))` returns the `Node` through a stack copy (about 40 instructions per tip step). `try_node` itself is not `#[inline]`.
  - **The locator.** `try_locator`'s `map` and `collect::<Result<_>>` copied a 64-byte `ControlFlow` per entry once the store reads inlined (+1.06% with the inlines above). It is a plain loop into a `Vec` reserved to the heights' count, which also drops the growth reallocations: 72,706 instructions and 3 allocations per call, against the base's 75,206 and 7. SB4's lines in `reader.rs`.
  - **The by-hash cold read.** An SPV lookup that misses the resident index reads by hash, and a new header's lookup is such a miss. Through the cache (§5.2, D43) that was the probe, then an out-of-line `Clock::admit` for the store's miss, then a `Result<Option<HeaderRecord>>` of about 200 bytes copied out; a hit cloned the whole record out of its `Arc`. `cold_hashed`, which absorbs `cold_by_hash`, returns the id and height its two callers use, read from the cached `Arc` after the hash check. `admit` is inlined for a miss, and the admission is `admit_found`. The latch and the corruption checks are unchanged.
  - **The cold cache's memory.** `ColdCache::new` reserved `cap` in both maps, the ring and the free list: about 110 KB of tables per SPV chain before any cold read (`chain-accept-spv-2000`'s +300 KiB, with the larger binary). They now grow with the records read. At `cap` they reach the sizes D43 counts (2,048 buckets per map at 1,024 records; the ring at `cap`, or its next power of two), so §6.3's row and §6's "a fixed `cap`" stand. The growth adds one rehash transient, the previous table, outside the step budget as the cache is.
- **D93: train 7's PF0 rows after D91–D92, and the residuals.** SB5 alone (84268fa27 merged into c68b1c3e4, an advisory `perfgate --only` run) reproduced every instruction, RSS and allocation delta of the train's report, so none is SB6's or EV2a's. The one exception is `chain-accept-headers`' page faults (2 → 4 in the train): the SB5-alone run measured 4 for the base as well as the head.
  - **`chain-tip-step-publish`, +0.94% (about 42 instructions per step of the base's 4,465).** About 20 are SB5's per-step contract (§3.3, §3.6), inlined: the status-key compare, the empty-table test, and the registers they take. About 20 are arc-swap's `Slots::help`, which SB4's unchanged `TipState::publish` store now calls out of line (+26), where the base's `Debt::pay_all` inlined it (−6 in `pay_all`). That is LLVM's partition of the crate's generic code. `#[inline]` on `TipState::publish` saved 1 instruction, and arc-swap's internals cannot be marked from here. The rest is codegen drift in unchanged code.
  - **`chain-load-records-256k`, allocations 340 → 348.** These are SB5's per-chain feed state, built once when a `HeaderChain` is constructed: `FeedShared` 3 (the registry's `Published`, its reserved table, the release waiters), `TipFeeds` 3 (the entries, the free list, the pending queue) and `StatusCell` 2 (its two `Published` cells). They are reserved at construction so that no tip-lock section allocates (§4.5, DEC-155 (2)). The count is constant per chain, whatever the record count.
  - **`chain-accept-headers`, allocations 28 → 29 and page faults 2 → 4.** The 2,000-header step moves the best header, so the status cell stores its facts once (§3.6): one 112-byte `Arc`. Page faults count first touches and move with the heap's layout; on agentbox the base itself gives 4 at one iteration.

### 10.3 Property tests (proptest)

- **P1, the feed model.** A sequential reference model of the writer, the cursors and the readers, against `TipCell`. Random operation sequences (step, `next`, `pop`, compress, register, `subscribe_at`, deregister, control, `retry_pending`) are applied to both and compared after each.
- **P2, windowed ≡ unwindowed** (SB4 §15.2, SB0 §4.4).
  - **Setup.** Random header trees over H-M-like regtest params: stale branches below the floor, locks, invalidations, reorgs deeper than w, and restarts at random committed cuts. Each runs on `HeaderChain<NoStore, Permanent>` and on `HeaderChain<MemHeaderStore, Evicting>` at w ∈ [w_min, 2 × depth].
  - **Compared after every step:** tip, `best_header`, `fork_target`, `chain_tips`, `locator`, `ancestor`/`lca`, status after commit, and every retained snapshot's `at_height` for all its heights.
- **P3, codecs.** The row round trip, and the `hdr_leaf` image equal to a brute-force "no accepted child" scan after any sequence.
- **P4, reopen.** At any committed cut, `open` reproduces tip, `fork_target`, CL state, checkpoint cache and window. **`best_header` is checked against an independent full scan** over every stored record with Core's predicate (`IsValid(VALID_TREE)`, work, then id). It does not use the production leaf reduction (Sol r1).
- **P5, ids.** No durable id is ever reissued. Ids reissued after a lost Buffered suffix appear in no durable row. Every power loss discards all instances (§3.7).

### 10.4 PLAN row acceptance → tests

| PLAN SB5 acceptance item | Test |
|---|---|
| X17–X24 analogues | X17 → `f6_parked_costs_no_thread`. X18 and X22 → `f2_seq_before_fanout` plus CH2b's driver over the real feed. X19 and X24 → CH2b's, over the real feed. X20 → `compress_then_wake`. X23 → `f5_wake_on_enqueue`. X21 is not a feed property |
| SB0 §6.1's ports | §10.1 |
| the T35 analogue | `t35_subscribe_stress` |
| windowed ≡ unwindowed over SB0 §4.4's scenarios | P2, plus the directed `w_stale_below_floor`, `w_a9000_b9000`, `w_restart_mid_extension`, `w_lagging_readers`, `w_reorg_10k_no_cl` and `w_cold_read_error` |
| a lagging reader across a reorg deeper than w | `lagging_reader_deep_reorg` (A12,000 retained, w = 2,048, reorg to B12,500 at 9,000: every `at_height` answers A, live queries answer B) |
| eviction after commit | `evict_after_commit_spv` |
| R6-M1 across restart and cold records | `r6m1_reopen_a100`, `r6m1_reopen_cold` (w = 8) |
| E5 (Sol r1) | **`e5_reopen_heavier_leaf`**: the persisted state has `best_header` walked back by a mark, and A18-style `reconsiderblock` that did not restore it, with a heavier `VALID_TREE` leaf stored; `open` selects the heavier leaf, not `meta.best_header`. **`e5_conflict_leaf_eligible`**: a CONFLICT-marked, not FAILED, heaviest leaf is `best_header` after `open`, while `fork_target` excludes it. **`e5_failed_leaf_parent`**: a FAILED leaf whose parent is `VALID_TREE`; the parent is selected. **`e5_tie_membership`**: equal-work leaves; the result is one of them (D9) |
| durability cut (Sol r1) | `durable_subscriber_after_buffered_header`: a Buffered header step, then a Durable subscriber commit naming its block, then power loss; the header record survives, because the WAL is sequential, and the subscriber's locator resolves at `open` |
| BG: PF0 `feed_publish` | §11 |
| memory (R1-M3, DEC-155) | **A-F12** below |

**A-F12, memory: observed ≤ cap (DEC-155, DEC-157 (3)).** These run under a counting global allocator. Charges are made on the header-writer thread, with a thread-local counter around the step (the stage-3 batches run on that thread too, through the leased connection, SB0 E8), and on the feed paths. Each test asserts the observed maximum is ≤ the §6 cap from the `const fn`s at the test's own configuration (plus M(m) where marks occur). Each also asserts the budget's own `max_seen` is ≤ the cap, so a missed charge shows as observed > charged.
- **`budget_feed_cap`**: 8 subscribers at Q, all full, over 10⁵ steps with random controls: per-feed observed ≤ per-feed cap; the feed total ≤ the feed cap; `register` past `max_feeds` returns `TableFull`.
- **`peak_control_mirror_pairs_between_intakes`** (Sol r2, `control_peak.py`): V sends a `FlushCommit` + `ChainStateFlushed` pair between every two reader `next()` calls, for 10⁴ rounds. The slot never holds more than 2 messages, never more than `MAX_CONTROL_KINDS`, and the control bytes stay ≤ (K + 1) · 272. Variants: the reader also compresses between calls; the reader is paused for 100 rounds (the held count stays 2).
- **`peak_enforcement_mirror_backlog`**: the node's `enforce_block` with d_h = 50 disconnects and a freeze at each, a paused subscriber, and a model hub. Control held ≤ the kinds, independent of d_h.
- **`peak_deep_reorg_paused_readers`** (Sol's witness, both A9 configurations): two stored 9,000-block branches, w = 2,048, Q = 256, five paused readers, and a one-header step that switches (d = 9,000, c = 9,001). The step-path maximum is sampled with each sub-step's `TipChange` alive, and is ≤ the step cap. It is rerun with d = 50,000 and with 1 reader: the maximum does not change. After the readers resume, `pending` is empty and the budget is back to its base.
- **`peak_mark_walk_write_batches`** (re-derived for SB5-A13): an E2-style lock whose marking walk sets 9,000 cold CONFLICT marks in one step, **without loading the cold records**. The overlay holds `(BlockId, Status)` only; the sink receives ⌈(new + marks) / W⌉ batches of ≤ W rows, the marks as `StepRow::Status`; the step-path observed maximum is ≤ cap + M(9,000), and the marks account's peak is ≤ M(9,000); the A5 list overflows and the cache is cleared; the reopened store equals the in-memory status. A **50,000-mark variant** shows the slope: marks-account peak ≤ M(50,000), about 18 B per mark. It passes against SB4 only with SB5-A13; against SB4's shipped full-record overlay it fails, which is the check that A13 landed.
- **`budget_pending_takes_blocked`** (test-util): a retained charge holds the **pending** sub-cap full. The next sub-step is not planned, `activate_segmented` waits, and it proceeds when the subscribers drain. No allocation passes the cap.
- **`budget_overrun_is_internal_error`** (test-util): an injected charge over each other sub-cap (legs, `TipChange`, A5 list, fixed) returns `ChainError::Internal` and trips the latch (with `debug_assert!` disabled for the test); a batch over `step_headers` returns `ChainError::Config` before any work.
- **`peak_out_of_order_chunks`** (R2-m2): 100 stale out-of-order ids, and a **sparse active-path** variant with active headers interleaved with side-branch headers so successive active ids land in different chunks. The arena holds ⌈(N_win + B)/1,024⌉ + 1 + S chunks with S measured. With A8, if adopted, the stale variant's S drops to 0 and the sparse-active variant's does not. Always chunks ≤ N_win + B.
- **`mem_spv_typical_w10000`** and **`mem_spv_typical_w2048`**: SB4's A-31 extended with the cache, feeds and step budget: ≤ 4.42 MB and ≤ 1.58 MB (typical rows at estimate × 1.25; cap rows exact).

## 11. PF0 workload lines

The bench target is `dash-chain/benches/feeds.rs` (criterion):

```toml
feed-publish             = { bench = "dash-chain/feeds", id = "publish/connect-8sub-1k" }          # BG gate
feed-publish-contended   = { bench = "dash-chain/feeds", id = "publish/connect-8sub-1k-contended" }
feed-publish-full        = { bench = "dash-chain/feeds", id = "publish/cursor-retry-8sub-1k" }
feed-reader-next         = { bench = "dash-chain/feeds", id = "reader/next-pop-compress-1k" }
feed-subscribe-at        = { bench = "dash-chain/feeds", id = "handoff/subscribe-at-64" }
status-publish-coalesce  = { bench = "dash-chain/feeds", id = "status/publish-1k-2readers" }
chain-cold-lru           = { bench = "dash-chain/chain", id = "cold/lru-hit-miss-90-10" }
chain-cold-lru-sweep     = { bench = "dash-chain/chain", id = "cold/lru-miss-all-referenced" }
chain-snapshot-cold-h    = { bench = "dash-chain/chain", id = "snapshot/at-height-cold-1k" }
chain-open-spv-w10000    = { bench = "dash-chain/chain", id = "open/mem-w10000" }
```

- **`feed-publish` (the BG gate; DEC-151 OQ8).**
  - **What is timed.** The measured operation is 1,000 `connect_tip` steps on a preloaded H-M index, each fanned out to 8 live subscribers, **plus joining all 8 reader threads after they have drained every event**. The readers' work is therefore inside the measurement.
  - **How it is counted.** PF0's inherited process and thread instruction counters (`contrib/node/perfgate/README.md`).
  - **The interim gate.** An absolute **1,500 instructions per (step × subscriber)**, producer plus reader. SB4's tip and snapshot work is counted separately and subtracted, by running the same steps with no subscriber. The gate is a cost budget, not a latency bound and not a comparison with Core.
  - **The Core comparison.** BL-70's `--bench-signals` mode, counting the same delivered events and equivalent wake and dispatch work.
- **`feed-publish-contended`.** The same steps, with each reader looping intake on its slot from its own thread, so the producer meets held slot locks. It reports instructions and wall time, and gates nothing until PF1. It is the R1-M2 contended-path measurement.
- **Expected costs.** Per step: the tip lock (SB4's), a target scan over ≤ 8 entries, 8 slot-lock push and clone pairs, and 8 `Parker` notifies. Nothing is allocated. Per take: one intake (two header swaps) amortized over the batch, and one decrement section per `pop`.
- **Risks.** The bench uses `Notify::Thread`. A tokio `Waker` costs more, and SB14 measures it.

**Erratum D31–D33 (F9, signed off by the manager).** F9 landed six of the ten lines, as follows. The block above is rev3's plan; `contrib/node/perfgate/workloads.toml` is the record.

- **Where the lines live (D31).** All six are in `dash-chain/feeds`. The two cold lines are `chain-cold-lru` and `chain-cold-lru-sweep`, with ids `cold/lru-hit-miss-90-10` and `cold/lru-miss-all-referenced`. They drove the records cache through `test_util::ColdBench`, a by-id row store over a `Vec` (superseded by D84), so SB4's `benches/chain.rs` is unchanged. `feeds.rs` holds a copy of SB4's preloaded-index fixture until SB4 shares it as a module.
- **What `feed-publish` measures (D32).** Pinned to one CPU, readers that drain interleaved with the writer spread the in-region instruction count by 17–20% between runs. The readers therefore wait at a barrier until the last step and drain after it, inside the measured region. This gives a 0.107% spread over 7 runs. The line is a lower bound on the interleaved cost: it covers every push, wake, intake and pop, but no contended slot lock. At 1,000 steps the queues never fill, and the bench asserts this.
- **The interleaved lines (D32).** `feed-publish-full` uses Q = 32, so the readers must interleave. The number of blocked steps varies between runs, and so do the pending allocations. Its `instructions`, `instructions_total`, `instructions_process`, `allocs`, `alloc_bytes` and `page_faults` are therefore `ungated`, and it gates syscalls and I/O only. `feed-publish-contended` is omitted until PF1: under one pinned CPU it cannot meet a held slot lock, so it would measure nothing. The bench keeps it as an unlisted id.
- **Deferred lines (D33).**
  - `status-publish-coalesce` waits on SB5-A17, the writer's publish hook.
  - `chain-snapshot-cold-h` and `chain-open-spv-w10000` wait on SB4's C4 cold reads and on F6's store.
  - Each lands with the commit that unblocks it, in `dash-chain/feeds`.
- **The 1,500 budget.** This is a hand check, not a PF0 key. F9 measured `feed-publish` at 26,760,601 instructions, against 16,877,221 for SB4's `tip/connect-publish-1k` with no subscriber. That is about **412** per (step × subscriber). Allocations equal the baseline (24,000): fan-out allocates nothing.
- **Determinism (D34).** The cold cache's index uses a fixed hasher, `BuildHasherDefault<DefaultHasher>`. A `RandomState` map's probe lengths, and so the instruction counts, vary between processes.

**Erratum D40 (F4b).** `status-publish-coalesce` landed with F4b, in `dash-chain/feeds`.
- **What it drives.** Each publication follows V's `best_header_walk_back`, so each stores new facts. The setup restores the best header with `best_header_invalid_chain(best)`, which leaves the index unchanged across iterations.
- **The readers.** Both are on the bench thread and arm their wakers in setup, so every publication notifies them. After the last publication, each takes one coalesced report. No thread runs beside the writer.
- **The baseline.** `status/publish-1k-0readers` is the unlisted no-reader baseline.
- **The fixture.** `feeds.rs` now uses SB4's shared `benches/support/fixture.rs`, which closes D31's copy. Measured on f55ccbede, `feed-publish` minus `chain-tip-step-publish` is still about **411** per (step × subscriber).
- **Local counts.** Taken with the shim, pinned, per iteration of 1,000 publications:
  - **0 readers:** about 1,474,200 instructions and 2,000 allocations (the snapshot's `Arc` and the facts' `Arc`).
  - **2 readers:** about 1,594,571 instructions, repeating within 3 over 5 runs. The readers allocate nothing. Their share (two notifies per publication and two reports) is about **60** per (publication × reader).

**Erratum D51 (F7).** `chain-snapshot-cold-h` and `chain-open-spv-w10000` landed with F7, in `dash-chain/feeds` (D31), which closes D33's deferral.
- **The chain** (superseded by D55). Both run on a mined regtest SPV chain anchored at 40,000, stepped as the host steps it (SB4 §9.1) over `MemHeaderStore`, with a two-header stale branch every 1,000 heights. SB4's mainnet-shaped fixture is a node index; it has no SPV store.
- **`snapshot/at-height-cold-1k`.** The chain has 1,100 trunk headers, w = 10 and a cold cache of 256. Each iteration reads 1,000 heights below the window, ascending. That is a cyclic scan past CLOCK's capacity, so every read misses and is one `by_id`; the setup asserts this over two passes (superseded by D85: about one store read per height, the walks sharing their upper links). It measures §5.3's cold `at_height`: the view, the cache miss, the store read, the parent's lookup by hash (a cache hit but for each pass's first height) and the reference.
- **`open/mem-w10000`.** The chain has 12,000 trunk headers at w = 10,000. Each iteration opens it: one `from_height` of 10,021 records, one `active_from` of 12,001 rows, and 13 leaves, 2 of them cold. The reopened chain is dropped outside the measurement (`iter_with_large_drop`). It measures `MemHeaderStore` only; SB13 measures SQLite (§8's cost note).
- **Determinism.** `MemHeaderStore` keeps `BTreeMap`s, and the hash index's hash is fixed. The only `RandomState` map `open` fills is SB4's `chunk_live`, with at most 12 entries here; SB4's `checkpoints` map is too, but regtest's set is tiny. Three processes differed by about 2·10⁻⁶ in `instructions:u`.
- **Setup share** (superseded by D55). Each fixture is built inside its bench closure, so `instructions_total` and peak RSS include it: about 98% of `chain-open-spv-w10000`'s `instructions_total` is mining and stepping 12,025 headers. A change to `open` shows in `instructions`.

**Erratum D55 (F7, amends D51).** The setup no longer mines: at about 98% of `chain-open-spv-w10000`'s `instructions_total`, it made that counter blind to `open`.
- **The chain.** Only the pack's step is real. `test_util::append_spv_records` appends the trunk and the stale branches in one step, as the header steps would write them, under SB4's `synthetic_hash` ids, which are below the target. A unit test checks that it leaves the same store image as the real steps over a mined chain. No route through `accept` avoids X11: `accept` checks a batch's hashes only under `debug_assertions`, but `Header::new` always hashes (about 260 k instructions each, about two per mined header).
- **Setup share.** `open/mem-w10000`'s setup fell from about 9.83 G to 0.17 G instructions, including the reads that check the window's 10,021 records and 13 leaves. At the gate's calibration (two iterations of about 101 M) it is 45.2% of `instructions_total`, so a 10% regression in `open` moves that counter by about 5.5%. `instructions` was already the measured region only. `snapshot/at-height-cold-1k`'s setup is about 55 M.
- **Determinism.** The builder's hash map has a fixed hasher. Processes' `instructions_total` agree within about 20, or sit 12 k (0.006%) higher, matched by the same step in the region.
- **Peak RSS.** It rose from about 21 to 28 MiB, since the one-step append holds all 12,024 appended records at once, in several copies.
- **Coverage.** No gated line now runs SPV `accept` or `write_header_step`. Before, X11 was about 98% of what this setup counted, so that coverage was nominal.

**Erratum D73 (F9's memory rows, DEC-187).** Two lines gate §6.3's typical residency per PR, in `dash-chain/feeds` (D31):
- **`spv/typical-w10000` and `spv/typical-w2048`** (`chain-spv-typical-*`). Each iteration syncs a fresh anchored regtest chain past its window in steps of B = 2,000: 14,000 headers at w = 10,000 with desktop's feeds and a 1,024-record cold cache, and 6,000 at w = 2,048 with mobile's and 256. Each step is SPV's whole header step through `test_util::SpvRig` (the bench's own rig since D84) (accept, `fork_target`, stage 3 committed Buffered, `activate_segmented`, `records_committed`), with five live subscribers draining at each wait and after each step. `iter_batched`'s setup builds the chain and subscribers, and they are dropped after the region. One sync in the fixture checks it: the tip at the last header, and exactly w + 1 entries resident.
- **What gates.** Peak RSS and page faults carry the residency; a chunk kept past its last resident entry shows only there. `allocs` and `alloc_bytes` carry the step's allocation volume, `MemHeaderStore`'s row work included; a larger entry shows there too. `instructions` carries the step's cost. SPV `accept` is already gated by SB4's `chain-accept-spv-2000`; these are the first gated lines through `write_header_step`, `activate_segmented` and the fanout in one step.
- **Setup.** `accept` checks proof of work and `Header::new` always computes X11, so the headers are mined once per process, about two X11 hashes each, outside the region. The fixture then syncs once and checks the tip and the window. The setup, mostly the mining, is about 90% of `instructions_total` at two iterations (12.07 G on desktop, against 0.56 G of `instructions`). Mining is deterministic, and `instructions_total` stays gated, like the other setup-heavy lines.
- **Local counts** (shim, pinned, two iterations, three processes, on SB4's fix r2): desktop 561,089,930 instructions (spread 256), 55,004 allocations, 98,508,848 allocated bytes, 4,116 page faults, 24,640 KiB peak RSS; mobile 239,647,855 (spread 293), 23,630, 42,125,824, 1,102 and 13,996 KiB. Every counter but `instructions` and peak RSS (±48 KiB) repeated exactly. On fix r1 the step took 666,307,079 and 284,361,208 instructions and 167,004 and 71,630 allocations: fix r2 makes 8 fewer allocations per accepted header. The fixture's checking sync runs in the setup and pre-faults the heap, so the region's page faults count only what a sync touches beyond it. Peak RSS includes the store's tables and the mined headers, so a residency change reaches it diluted: 2% of 24,640 KiB is about 493 KiB, two arena chunks.
- **The absolute bounds** stay A-F12's (`mem_spv_typical_*`); PF0 compares each head with its base.

## 12. Commit plan

PLAN sizes SB5 at M, two weeks (10 days); rev3's estimate is 20 days (m8). F0's stub edits follow DEC-151: the SB4 implementer owns `dash-chain` until SB5 starts, so F0 lands as a coordinated edit on their branch or right after C0's successor, whichever they choose.

| # | Commit (conventional) | Content | Est. (days) | After (SB4) | Unblocks |
|---|---|---|---|---|---|
| F0 | `feat(dash-chain): feed and residency API stubs` | §3 signatures with `todo!()`; A1, A2, A6; `Notify`, `Parker`, `Next`; the `sync` facade; the traits moved in | 1 | C0 (coordinated) | CH2b, CH1-node and SB14 code against the API |
| F1 | `feat(dash-chain): feed slots, swap intake, next(), per-kind controls, wakes, feed budget` | §3.1, §3.4, §4.3 slot and reader halves, §4.5, §6.1; the `HoldActive`/`on_release` API and the non-blocking tip-lock helper (DEC-157 (2)); F3–F5, DEC-152 and DEC-155 (1) tests; loom L1, L2, L5, L9 with negatives | 3 | C0 | — |
| F2 | `feat(dash-chain): TipCell fanout, cursors, register, subscribe_at, deregister` | §4.3 under the tip lock (SB5-A4); `try_lock` step 4 and the lazy reclaim; X28, X27, F11 incl. delayed cleanup and lazy reclaim, T35, P1; loom L3 (with the contended step 4), L4, L7 | 2 | C5 | CH2b's b3 |
| F3 | `feat(dash-chain): per-step drains under holds, SPV sub-step composites, step budget` | the `Hold<'c>` guard (m5) with `with_tip` and `retry_pending` for `hold_tip` and `enforce_block`; snapshot/status and side effects at release, the release wakes; `activate_segmented` and `StepBudget` with the marks account (§4.8, §6.2); the SPV-LO9 marker check (§4.9); `h7_*`, the M1 and M2 tests, the §4.7 and §4.8 tests, loom L10–L13 | 3 | C6 + **CH1-A1, SB5-A11, SB5-A12, SB5-A13** (SB4's rows; E-SB5-2) | CH1-node N3 (CH1-B1–B4), `ApplyChainLock`; CH2b A-40/A-41; SB14 |
| F4 | `feat(dash-chain): StatusEvent cell` | §3.6, the `arm` protocol; status tests; loom L8 | 1 | C5 | SB11, SB14 |
| F5 | `feat(dash-chain): cold cache, cold IndexAccess, snapshot heights by ancestry` | §5.2–§5.4 (A5, A10); loom L6; `lagging_reader_deep_reorg` | 1.5 | C4 | — |
| F6 | `feat(dash-chain): streamed header step write set, hdr_leaf deltas, MemHeaderStore sink and faults` | §3.7, §7 (`StepRow`, `HeaderStepSink`, `StepTail`, compact `ActiveDiff`); P3, P5 | 1.5 | C4 | SB13 |
| F7 | `feat(dash-chain): SPV open` | §8 (A3); P4 with the independent scan; the E5 tests; `r6m1_reopen_*`; the checkpoint reopen half | 2 | C4, C6, F5, F6 | SB15 |
| F8 | `test(dash-chain): windowed and unwindowed equivalence and budget peaks` | P2 with restarts, the SB0 §4.4 directed scenarios, `evict_after_commit_spv`, `durable_subscriber_after_buffered_header`, A-F12 (observed ≤ cap, + M(m) for marks, the 50,000-mark slope), m2's visibility test | 4 | F7, C7 | the SB5 row |
| F9 | `perf(dash-chain): feed benches and PF0 lines` | §11, both feed-publish paths, the cache sweep | 1 | F2 | the BG gate |

**Arithmetic** (estimates, rev3 per Opus high m8): 1 + 3 + 2 + 3 + 1 + 1.5 + 1.5 + 2 + 4 + 1 = **20 days**. rev0 had 13.5, rev1 16, rev2 17.5. rev3 adds +0.5 F1 (the `HoldActive` API), +1 F3 (the `Hold` refactor against SB4 APIs that do not exist yet, both hold kinds, M1/M2 tests, L12/L13) and +1 F8 (P2 with restarts, eight A-F12 cases under a counting allocator, the marks term, m2's visibility test). For one implementer that is 10 days over M. **Two implementers (recommended):**
- **A (feeds):** F0 → F1 → F2 → F4 → F9 → F3 = 11 days (F4 and F9 before C6, F3 after it).
- **B (residency):** F5 → F6 → F7 → F8 = 9 days.

**SB4's side.** CH1-A1, SB5-A11, SB5-A12 and SB5-A13 change SB4's approved public API (`TipChange`'s lifetime, `tip_target`/`tip_plan_bounded` beside `tip_plan`, the step sink in place of `take_dirty_records`, the status-only overlay, `Hold`). About **3 SB4 days** in C4–C6, on SB4's critical path. They must be adopted **before SB4 starts C4** (OQ15, ruled in §14), or C4 implements the full-record overlay and `take_dirty_records -> Vec<HeaderRecord>` only to rework them.

**Critical path.** It runs through SB4's C6, now about 3 days later:
- F3 follows C6 and SB4's rows above, taking A to about 3 days after C6.
- F7 and F8 follow C6 too, so B finishes about 6 days after C6.
- SB5 therefore ends about 7 working days after SB4 ends. PLAN's CPM leaves 2.5 weeks between SB4 (week 8.5) and SB5 (week 11.0); with SB4's ~3 extra days, about **2.5 days of slack** remain before the sequential review (rev2 said 6.5).

Review time is not included.

## 13. Decisions

| # | Decision | Why |
|---|---|---|
| D1 | `TipCell` is not generic; `ChainEvent = TipStep` | DEC-151 OQ2 |
| D2 | targets are captured at `seq` assignment; a slot that goes live later is never a target of an assigned event | F10's cut, X28 |
| D3 | slot objects are never reused; an index is reused with a new `gen`, retired at `u32::MAX`, and cleanup revalidates by `ptr_eq` | stale ids and old readers cannot alias; Sol r1's delayed-cleanup interleaving |
| D4 | `SubscriberFeed`'s methods take `&mut self` | `forbid(unsafe_code)`; DEC-151 OQ1 |
| **D5 (rev1)** | the backlog is per-target cursors over shared, seq-ordered segments; the rev0 per-entry target lists and `blocked` set are gone | one copy of each event whatever the target count (R1-M3); per-target order by construction |
| **D6 (rev1, DEC-152)** | each slot is one leaf `Mutex<SlotInner>` whose every critical section is O(1): three Q-sized buffers reserved at `register`, reader intake by swap, control as a sender-allocated linked list, no `try_lock` path. The producer may block on such a section, as Core's `AddToProcessQueue` blocks on `m_callbacks_mutex` under `cs_main` | rev0's O(Q) refill under the lock and arbitrary wakers broke SB0 P2 (R1-M2); E6 amends P2 to Core's contract |
| D7 | control is an opaque `ControlMsg`, out of band, with no tip lock | SB0 §6.1; H2 |
| D8 (rev2) | the writer's rule: no step, and no SPV sub-step, with `pending` non-empty | SB0 §6.1, F3; D30 |
| D9 (rev3) | `FeedReader::drop` closes the slot lock-free (`closing`, the producer wake, `close_hook`). Every closing entry is reclaimed lazily, by whoever next holds the tip lock: `deregister`'s step 4 if its `try_lock` succeeds, else the writer's `retry_pending`/release scan or the next `register` | a leaked reader must not wedge the writer; the drop and `deregister` can run during any hold (DEC-157 (2)) |
| D10 | `FeedSource<E>` is generic over the finish error | DEC-151 OQ1 |
| D11 | `Needs`/`Refused` from one host-wide `BlockData` | P8; Core's prune incompatibilities are init checks (`init.cpp:1247-1260`; `index/base.cpp:91-123`) |
| D12 | `StatusEvent` is a watch cell with per-reader coalescing and the `arm` register-then-recheck protocol | SB0; Core's two dedups; R1-m1 |
| D13 | the cold cache holds its lock across lookup, read and insert; invalidation follows the commit under the same lock | EV addendum §2.6; Core `snapshotCacheCs` |
| D14 | range walks and `walk_down` bypass the cache | immutable fields only |
| D15 | snapshot heights below the window come by ancestry from the view's lowest entry; below the anchor, `None` | SB0 §4.4, DEC-105 F3 |
| D16 | durable ids are never reused; ids lost with a Buffered suffix may be reissued | §5.1; E7 |
| D17 | `open` recomputes `best_header` over `IsValid(VALID_TREE)` (CONFLICT eligible) | E5; DEC-151 OQ6 |
| D18 | `open` treats an `active` mismatch as corruption | one transaction writes both |
| D19 | a `hdr_leaf` table from writer-supplied deltas | `leaves()` in O(branches) |
| D20 | feeds keep delivering after the latch | persistence paths check the latch (§5.4) |
| D21 | SPV `IndexAccess` returns `None` plus the latch on a cold error; every persistence path checks the latch | CH2's trait stays infallible; Sol r1's condition |
| D22 | Q = 1,024 (node, SPV desktop) or 256 (mobile); cache 1,024 or 256 | §6 |
| D23 | loom on a `sync` facade, with explicit boundaries, modelled wakes and negative controls; the production adapters by directed tests | R1-m2 |
| **D24 (rev1)** | control and data are selected in one slot critical section (`next()`), control first | R1-M1; the F4 proof in §4.4 |
| **D25 (rev1, DEC-152)** | the only wake type is `Notify` (`Task(Waker)` or `Thread(Arc<Parker>)`) for data, control, status, the producer and `close_hook` | notification-only by type |
| **D26 (rev1)** | the cold cache's invalidation moves out of the tip critical section into stage 5 (SB5-A10) | latency: the cache mutex's sections include a store read (rev3: allowed inside a hold by E6a, kept out of the step's critical section for latency) |
| **D27 (rev2; rev3)** | memory is bounded by budgets with configuration-derived caps, computed by `const fn`s from the real `size_of` values; the design derives the caps, and tests assert observed ≤ cap. Two adversarial terms are stated, not capped: S in residency and the mark count's M(m) in the step (D34) | DEC-155; DEC-157 (3); R1-M3, R2-M1, Opus high M3, m1 |
| **D29 (rev2, DEC-155 (1))** | control is stored in the slot, one outstanding message per kind; `next()` consumes (clears the kind's flag); a same-kind send replaces the payload in place; the reader holds no control between calls | R2-M2: slot + reader-owned ≤ #kinds by construction |
| **D30 (rev2, DEC-155 (2); rev3)** | SPV composites run as sub-steps of ≤ E events in one hold (`activate_segmented`), each a pure disconnect or connect run (no mixed sub-step, m10), and each drains before the next is planned; one publish at release, after which the last sub-step fans out (D37) | R2-M1: legs, `TipChange` and pending are ≤ E-sized whatever d + c is; readers never see an intermediate tip (Core `validation.cpp:3812`) |
| **D31 (rev2, DEC-155 (2))** | the header step's write set streams in batches of ≤ W rows into one store transaction; `active` rows are written by a compact `ActiveDiff` | no step-sized write set in memory |
| **D28 (rev1, DEC-153)** | every node tip step is followed by a drain of its pending fanout, inside the same hold and through the held guard (`with_tip`), before any other step; V's in-hold wait answers `FlushDue`, `PruneClears` and W's wakes only, never class R/M commands | DEC-153 (1); CH1-node Q1, CH1-B1–B3, E-SB5-1 |
| **D32 (rev3, DEC-157 (2))** | no `ChainFeeds` call blocks on a hold. `register` and `subscribe_at` take the tip lock by `try_lock` and return `HoldActive` while a hold is open (`WouldDeadlock` on the holder's own thread, in every build), with `on_release` as the release wake; `deregister`'s step 4 is a `try_lock` with lazy reclaim (D9) | Opus high M1: a caller can be what the hold waits on (a shared events thread, a 2-worker runtime, CH2b's hub lock); Core's `NextSyncBlock` waits for `cs_main` only because no `cs_main` holder waits for the index thread |
| **D33 (rev3, DEC-157 (1))** | SPV-LO9: nothing the SPV header writer waits on in a hold may wait on the header writer. Subscriber pop tasks carry the no-wait marker; header ingress is `try_send` | Opus high M2; the node's LO-9 (DEC-156) has the same shape |
| **D34 (rev3, DEC-157 (3))** | SB4's SPV overlay is status-only (SB5-A13); A5's list is bounded with clear-all; the mark count is the step's one adversarial term, M(m) ≤ 31·m + 64, counted in its own account | Opus high M3: the full-record overlay cost ~210 B per mark, uncapped; write-through marks rejected (they change SB0 §9.5's writer model) |
| **D35 (rev3)** | the hold's guard lives in a `Hold<'c>` on V's (or the SPV writer's) stack, borrowing `&mut HeaderChain`; `HeaderChain` stays `Send`, asserted at compile time | Opus high m5: a `ArcMutexGuard` field made `HeaderChain` `!Send`; probe `sendprobe` |
| **D36 (rev3)** | exhaustion semantics: only the pending sub-cap is relieved by waiting (Blocked); a batch over B is `ChainError::Config`; every other overrun is `ChainError::Internal` plus the latch plus `debug_assert!` | Opus high m1: a wait whose only relief is the writer itself would never end |
| **D37 (rev3)** | release order: SB4's stage-4 side effects once, then the snapshot store and status publish, then the last sub-step's fanout, then the owner cleared, the guard dropped, the release wakes; an error inside the hold calls `abandon` (latch, no publish) | Opus high m2, m3, m9 and N1 (publish before notify, DEC-156) |

## 14. Open questions

DEC-151 ruled rev0's OQ1, OQ2, OQ6 and OQ8. OQ3, OQ4, OQ5, OQ7, OQ9 and OQ10 keep the provisional answers, to be confirmed by their owning tasks. rev1 added OQ11–OQ13. In rev2 DEC-155 closes OQ12 and makes OQ13's SB5-A9 optional, and rev2 adds OQ14–OQ15. **rev3 rules OQ14 and OQ15** (the manager's instruction: SB4 needs OQ15 before C4).

| # | Question | Provisional decision | Owner |
|---|---|---|---|
| OQ1′ | CH2b's driver adopts `next()`/`Next` in place of DEC-151's `front` + `pop_control` (R1-M1) | yes (D24) | CH2b |
| OQ3 | SB13 adopts §3.8's SB5-S1 (with the E8 lease and a separate read connection) and runs P2–P5 against SQLite | yes | SB13 |
| OQ4 | Mobile w and step size (B = 500 saves one chunk, 245,760 B, and 84,000 B of the step cap: a typical total of ≈ 1.88 MB, DEC-187; the peer's decoded message stays) | w = 2,048, B left to SB14 | SB14 |
| OQ5 | SPV `IndexAccess`'s `None` plus the latch, with the persistence latch checks in §5.4 | keep it infallible | CH2b, SB14 |
| OQ7 | Q = 1,024 vs CH2's `d_max` | 1,024; PF1/SB17 measure | CH2b, PF1 |
| OQ9 | Per-reader `ChainLocked` coalescing | latest only. This differs from Core, which queues every `NotifyChainLock` that passes its dedup (`chainlock/handler.cpp:312-320`), so a slow Core listener sees each lock (n6). SB11 confirms latest-only is enough for its consumers (each re-reads the facts), or asks for a per-reader queue | SB11 |
| OQ10 | The hub deregisters on `close_hook` after the hold | yes | CH2b |
| **OQ11** | Does SB4 adopt A8 (early eviction of stale entries in otherwise stale-only chunks)? It removes stale-only pins, not sparse-active ones (R2-m2) | SB5 retains S either way | SB4 |
| ~~OQ12~~ | ~~CH2b coalescing with `control_backlog`~~ | **closed by DEC-155 (1)**: SB5 coalesces per kind (D29); CH2b assigns kinds (C1) | — |
| **OQ13** | Does SB4 adopt SB5-A9 (shared sub-step vectors) and SB5-A10 (cache invalidation at stage 5)? | A10 yes; A9 optional: the step cap is derived for no A9 | SB4 |
| ~~OQ14~~ | ~~Where does SB14 run the header writer's `wait` (SB5-Y1)?~~ | **Ruled in rev3:** a dedicated header-writer thread parking one `Parker` for both its in-hold `wait` and its post-release drain (n9), counted in SB0 §7's thread-count test | SB14 |
| ~~OQ15~~ | ~~Does SB4 adopt SB5-A11, SB5-A12 and SB5-A13?~~ | **Ruled in rev3: yes, all three, before SB4 starts C4.** A11 in its minimal form (the node keeps `tip_plan`/`TipPlan`; SPV adds `tip_target` and `tip_plan_bounded`; accept results and the sink through `StepBudget`); A12 with the `Hold<'c>` shape; A13's status-only overlay amending SB4 D33. Without them DEC-155 (2) and DEC-157 (3) do not hold, and C4 would build the full-record overlay only to rework it (m8). About 3 SB4 days (§12) | SB4 |

## 15. rev1 closure map (Sol r1)

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **R1-M1** separate control and data calls break F4 | major | **Fixed.** `next()` selects control and data in one slot critical section, control first. A proof against Sol's four-step witness; the directed test `f4_control_before_data`; loom L5 runs the real two-call boundary and the rev0 split as a negative control (both reproduced in Appendix B's probe). DEC-151's `front` + `pop_control` replaced (OQ1′) | §3.4, §4.3, §4.4 F4, §10.1, §10.2, D24 |
| **R1-M2** the producer can block under the tip lock (O(Q) refill; arbitrary `Fn` wakers) | major | **Fixed per DEC-152.** Every slot section is O(1) (the table in §4.5): swap-out intake, buffers reserved at `register`, sender-allocated control nodes. The only wake type is `Notify` (`Waker::wake_by_ref` or a `Parker` permit). There is no `try_lock` path. The cache invalidation leaves the tip critical section (A10). SB0 P2 is amended by E6, with the text drafted in Appendix A and covering SB0 §6.1, §6.2, §6.3 and §7. Evidence: loom L1/L9 preempt the reader inside the slot sections, plus a lost-permit negative (probe in Appendix B); the directed `v_resumes_after_paused_slot_holder`; `slot_sections_do_not_allocate`; the contended benchmark | §3.1, §3.2, §4.2, §4.3, §4.5, §10, §11, D6, D25, D26, App. A |
| **R1-M3** SPV peak memory excluded connect legs and understated pending entries | major | **Fixed.** The backlog is shared segments with per-target cursors (D5), so events are held once. With A9 the extra bytes are 0 (SB4's vectors, (d + c) × 40 B); without A9, (d + c) × 56 B. d and c are stated as unbounded by B, with their real bounds. Sol's witness: 720,040 B or 1,008,056 B. S and stale branches are separate adversarial terms (100 chunks = 20.48 MB), with A8 proposed. The control mirror backlog is a CH2b term with a coalescing rule (OQ12). The totals are relabelled typical (4.0 MB, 1.38 MB). A-F12 tests each formula, with directed peaks for the paused-reader deep reorg, out-of-order chunks and the enforcement mirror backlog | §6, §10.4, D5, D27, A8, A9 |
| **R1-m1** the status cell had no park/recheck protocol | minor | **Fixed.** Facts are stored, then the version is bumped (SeqCst); `arm` loads, registers, then re-loads; `next_events` diffs; `StatusReader::drop` removes its slot. Status waker sections are O(1) and the list lock is off the writer's path. Loom L8 with a no-recheck negative (probe in Appendix B); directed `status_arm_race` and `status_reader_drop_removes_waker` | §3.6, §4.3, §10.1, §10.2, D12 |
| **R1-m2** loom boundaries and wake semantics | minor | **Fixed.** §10.2 states what runs through the facade, what is replaced and which edges the replacement adds, how wakes are modelled, and what loom does not prove. Every model has a planted negative control. L7 has barriers before and after closing. The production adapters (`Parker`, tokio, `ArcSwap`) get directed tests and Miri | §10.1, §10.2, D23 |
| **R1-m3** CLOCK and mobile arithmetic | minor | **Fixed.** CLOCK insert is amortized O(1) with an O(cap) worst case under the cache mutex, which is now outside the tip lock; the bench includes the all-referenced miss. The mobile saving is one chunk, 204,800 B | §5.2, §6.2, §11 |
| Sol r1 constraints: `blocked` identity, delayed cleanup, E5 eligibility and tests, the P4 independent reference, below-anchor `None`, SQLite read-after-commit, latch-check paths, power loss discarding instances, Durable-after-Buffered cut, `open` cost, the PF0 accounting, F0 ownership | — | **Folded in.** `blocked` is gone (cursors per slot object); `ptr_eq` cleanup with `f11_delayed_cleanup_after_reuse`; E5 states FAILED vs CONFLICT with four tests; the P4 scan is independent; §5.3; §3.7; §5.4; §3.7; §10.4; §8; §11; §12 | as listed |
| DEC-153 (landed after Sol r1; manager addendum) | — | **Adopted.** The F1 stream stays `TipStep` only. V drains pending fanout between **every** node tip step inside one hold (ABC inner loop, `InvalidateBlock` iteration, `enforce_block`), through the held composite guard (`with_tip`), with no class R/M command serviced in between; §4.7 shows `retry_pending` under the hold, the in-hold wait and why each wait ends, and works the Q = 1 two-disconnect case with its closing and `-dbcrashratio` variants. Loom L10 models it with four planted negatives (probe results in Appendix B). CH1-node's asks B1 (F3 for V's holds), B2 (`subscribe_at`/`register`/`deregister` wait out V's holds) and B3 (E-SB5-1, no queries in the in-hold wait) are accepted. Core's validation callbacks and `LimitValidationInterfaceQueue` belong to CH1-node's separate dispatcher | §1.2, §3.3, §3.8 A4, §4.2, §4.3, §4.4 F2/F10, §4.6, §4.7, §6.1, §10.1, §10.2 L10, §12 F3, D28 |

## 16. rev2 closure map (Sol r2; DEC-155)

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **R2-M1** the no-A9 fallback undercounted the combined SPV reorg peak (plan/`TipChange` vectors alive with the copied pending events: 1,728,096 B for the witness) | major | **Fixed by construction (DEC-155 (2)).** Every header-step allocation is charged to a step budget with cap 184W + 136E + 16B + 1,024 (360,704 B desktop, 114,944 B mobile), derived from the no-A9 worst case, so A9 is optional. Composites run as sub-steps of ≤ E events in one hold. Each sub-step's plan, `TipChange` and pending segment are freed before the next is planned, and the next waits on Blocked until the segment drains. The write set streams in ≤ W-row batches into one transaction, with a compact `ActiveDiff`. One publish at release, so readers see no intermediate tip. Probe: Sol's witness peaks at 359,680 / 113,920 B, the same at d + c = 100,001; rev1's 1,728,096 B is reproduced for contrast. Tests assert observed ≤ cap with the `TipChange` alive (`peak_deep_reorg_paused_readers`, both A9 configurations), plus the mark-walk batches and exhaustion → Blocked. The peak table is now the cap's derivation plus the observed maxima | §0, §3.1, §3.3, §3.7, §3.8 A9/A11/A12/S1/Y1, §4.8, §6.2, §10, §12, D27, D30, D31, OQ14, OQ15 |
| **R2-M2** `control_backlog` coalescing did not bound controls taken into the reader | major | **Fixed by construction (DEC-155 (1)).** Controls stay in the slot: one payload per kind, kinds in first-queued order, an `outstanding` bit set by `send_control` and cleared only when `next()` returns the message. The reader holds no private control buffer. A same-kind send while outstanding replaces the payload in place, and the old payload is freed after the section. So slot plus reader-owned ≤ `MAX_CONTROL_KINDS` (8), plus one in the handler. Payloads are `Copy` and ≤ 256 B at compile time. F4's proof is restated for slot-resident controls; loom L5's rev2 variant passes. Sol's directed case (mirror pairs between reader intakes) is `peak_control_mirror_pairs_between_intakes`: probe peak 2 over 10,000 rounds, and 3 (= the kinds) over 2,000 random schedules. W's channel, captures and acks are unchanged; only the feed copies coalesce | §3.1, §3.2, §3.4, §4.3, §4.4 F4, §4.5, §6.1, §10, D29, C1 |
| **R2-m1** E6 was drafted but not inserted into SB0 | minor | **Fixed.** E6 is applied in SB0 itself in this commit: §6.1 (slot, reader and wake sketch, the capacity and producer-wake bullets), §6.2 F3/F5, §6.3 P2, §7 (a Wakes row) and §17 (the E6 entry). Each amended passage points to §17 | SB0 §6.1–§6.3, §7, §17; App. A |
| **R2-m2** A8 does not give S = 0 for sparse active ids | minor | **Fixed.** A8 is qualified as removing stale-only pins. S is retained after A8, with the sparse-active construction stated, and a hard by-construction ceiling (chunks ≤ N_win + B) is added. `peak_out_of_order_chunks` gains a sparse active-path variant | §3.8 A7/A8, §6.3, §10.4, OQ11 |
| Sol r2 implementation constraints (DEC-152) | — | **Folded in.** A replaced waker or coalesced payload is moved out and freed after unlock; `slot_sections_do_not_allocate` also counts deallocations; the `RawWaker` contract on `Notify::Task` stands | §4.3, §4.5, §10.1 |

## 17. rev3 closure map (Opus high; DEC-157)

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **M1** `register`, `subscribe_at` and `deregister` step 4 block for a whole hold, and their callers can be what the hold waits on | major | **Fixed (DEC-157 (2), D32).** Every `ChainFeeds` call takes the tip lock only by `try_lock`. `register` and `subscribe_at` return `HoldActive` plus `on_release`, with register-then-retry against the lost wake; on the holder's own thread they return `WouldDeadlock` in every build. `deregister` step 4 is a `try_lock`; a lost race leaves the entry to the lazy reclaim (writer scan, release, next `register`), and `TableFull` is reported only after a reclaim. All three cases are covered: the shared events thread, the 2-worker runtime, CH2b under its hub lock. Probe `rev3.rs`: the try_lock models pass, and the blocking variants deadlock as negative controls. A marked caller selects on the release wake beside its pops (CH1-node rev3.1) | §0, §3.1, §3.2, §3.4, §4.2, §4.3, §10.1, L3, L12, D9, D32 |
| **M2** SPV has no LO-9 | major | **Fixed (DEC-157 (1), D33).** New §4.9 SPV-LO9: the rule, the filters/network/header cycle, the marker (CH1-node's task-local, via SB14's `spawn`), `try_send` header ingress with `fPauseRecv`-style back-pressure, and the latch checks moved to SB14's adapters (n5). New ask **SB5-Y2** to SB14. Probe: `m2_ingress_try_send` passes, and the blocking-send negative control deadlocks. Test `spv_lo9_actor_cycle` with its negative control; loom L13 | §0, §3.8 SB5-Y2, §4.9, §10.1, L13, D33 |
| **M3** the step budget omits the SPV status overlay, the dirty ids and A5's list | major | **Fixed (DEC-157 (3), D34).** (a) New ask **SB5-A13**: the overlay is status-only, `HashMap<BlockId, Status>`, emitted as `StepRow::Status`; cold marks never load records. (b) SB5-A5's list holds ≤ `cold_cache` ids, else `clear()`. (c) The mark count is the one adversarial step term, M(m) = 9·(b + b/2) + 32 (old + new tables at the last rehash), worst case ≤ 31·m + 64; 9,000 marks = 221,216 B, 50,000 = 884,768 B (17.7 B per mark), against rev2's ~1.9 MB for 9,000. Write-through marks are rejected. (d) `peak_mark_walk_write_batches` is re-derived to assert observed ≤ cap + M(m), with a 50,000-mark variant; the probe's observed overlay equals M(m) exactly | §0, §3.7, §3.8 SB5-A5/A11/A13, §5.2, §6.2, §10.4, D34 |
| **m1** exhaustion underspecified; most "waits" would hang | minor | **Fixed (D36).** Only the pending sub-cap waits (Blocked); over B is `Config`; every other overrun is `Internal` + latch + `debug_assert!`. Caps are `const fn`s of the config and `size_of`; the measured sizes correct rev2's (`AcceptOne` 56 B, not 16; `StepRow` 176, not 184). New caps 420,224 / 189,824 B; the probe's observed peaks are 415,104 / 187,776 B at Sol's witness | §6, §10.4, D27, D36 |
| **m2** where SB4's stage-4 side effects happen relative to sub-steps | minor | **Fixed (D37).** Side effects run once, at release, under the guard; status changes stay invisible until then (`segmented_status_invisible_during_hold`) | §4.8, SB5-A11 (e), §10.1 |
| **m3** `activate_segmented` has no error path | minor | **Fixed (D37).** Any error inside the hold calls `Hold::abandon`: latch, no snapshot or status store, the guard dropped, the release wakes fired; `open` restores the last committed tip (`segmented_cold_error_mid_hold`) | §3.3, §4.8, §9.1 |
| **m4** in-hold cold reads contradict §4.2/A10; SB0's E6 still says the SPV writer waits without the lock | minor | **Fixed.** §4.2's tip-lock row adopts the reviewer's (i)–(iii) wording; the lock order adds tip → cache → store; SB5-A10 stays for latency. **E6a** is applied to SB0 (§6.1, §6.2 F3, §17) | §0, §4.2, §4.5; SB0; App. A |
| **m5** the guard in `HeaderChain` makes it `!Send` | minor | **Fixed (D35).** The guard lives in `Hold<'c> { chain: &'c mut HeaderChain, guard }` on the holder's stack; F0 asserts `HeaderChain: Send` (`static_assertions`). Probe `sendprobe` compiles and moves the chain to a thread | §3.3, §4.7, §10.1 |
| **m6** ask labels collide with CH1-node's; A12 duplicates CH1-A1; CH1-A11's pre-genesis flag | minor | **Fixed (DEC-157 (4)).** Every ask is prefixed (SB5-A1…A13, SB5-S1, SB5-Y1/Y2, SB5-C1); SB5-A12 is restated as CH1-A1's shape plus SPV's needs; `ChainSnapshot::tip`, `subscribe_at` and the `TipState` comment follow CH1-A11 (`pre_genesis`, non-`Option` types) | §3.2, §3.4, §3.8, §4.3 |
| **m7** the streamed transaction contradicts SB0 §9.5's one writer | minor | **Fixed.** Stage 3 leases the write connection (one message out, one back); the batches run on the header thread, so A-F12's counter stays thread-local; cold reads use a separate read connection. **E8** is applied to SB0 §9.5, and the lease is in SB5-S1 | §3.7, §3.8 SB5-S1, §10.4; SB0 §9.5, §17 |
| **m8** the plan is optimistic; OQ15 must be ruled before SB4 C4 | minor | **Fixed.** 20 days (F1 3, F3 3, F8 4); A 11 days, B 9; SB4 +3 days; slack ≈ 2.5 days. **OQ15 ruled** (yes, A11 minimal + A12 + A13, before C4) and OQ14 ruled; F3's row names CH1-A1, SB5-A11, SB5-A12 and SB5-A13 (E-SB5-2) | §12, §14 |
| **m9** every SPV step fans out before publication | minor | **Fixed (D37).** A single-segment SPV step fans out after the snapshot store (N1, as the node's); in a multi-segment hold the last segment fans out at release, after the stores. Intermediate segments' events are fenced by `tip_seq`: §4.4's "Events before publication" rule tells consumers to resolve by id and never judge an event against an older snapshot | §3.8 SB5-A4, §4.4, §4.8, §10.1 |
| **m10** the sub-target formula is wrong for the mixed sub-step | minor | **Fixed (D30).** No mixed sub-steps: disconnect runs to the fork, then connect runs; each ≤ E; `tip_target` picks the sub-target per run. The proptest asserts d_i + c_i ≤ E | §4.8, §10.1 |
| **n1** a coalesced control keeps its first position | nit | **Changed:** coalescing moves the kind to the back, so delivery is a subsequence of the send order (`f4_coalesced_moves_to_back`) | §3.1, §4.3, §10.1 |
| **n2** the "+1 being handled" is a driver contract | nit | Stated in §6.1 | §6.1 |
| **n3** two cap terms are conservative | nit | The pending sub-cap is aligned to 40·E (copied `BlockRef`s); the plan-leg term stays at 40·E | §6.2 |
| **n4** `open` misses a cold id ≥ `next_id` | nit | `HeaderStore::max_id()` (SB5-S1), checked at §8 step 3; `open_rejects_stored_id_at_next_id` | §3.7, §8, §10.1 |
| **n5** C1's latch check is moot for CH2b | nit | Moved to SB14's adapters (SB5-Y2) | §3.8 |
| **n6** OQ9 differs from Core | nit | Recorded in OQ9; SB11 confirms or asks for a queue | §14 |
| **n7** the status publish order | nit | Stated: snapshot store → `facts.store` → `version` bump → wakes (`status_publish_order`) | §4.3, §10.1 |
| **n8** `ControlKind` must reject ≥ 8 | nit | `ControlKind::new` is a `const fn` that refuses ≥ `MAX_CONTROL_KINDS` (`control_kind_out_of_range`) | §3.1, §10.1 |
| **n9** the SPV writer mixes `.await` and a blocking `wait` | nit | SB5-Y1: the dedicated thread parks one `Parker` for both | §3.3, §3.8, OQ14 |
| **n10** `hdr_prev`'s byte order | nit | Noted in SB5-S1: `children(prev)` binds the hash in internal byte order | §3.8 |
| DEC-157 SB0 errata | — | **Applied in this commit:** E5 (`open` recomputes `best_header`), E6a (in-hold waits), E7 (durable ids never reused), E8 (the stage-3 lease) in SB0 §4.4, §6.1, §6.2, §9.5 and §17 | SB0; App. A |

## Appendix A. SB0 errata (E6 applied to SB0 in rev2; E5, E6a, E7 and E8 applied in rev3)

**E5 — `open` recomputes `best_header`, as Core's load does** (DEC-151 OQ6; SB5 §8 step 7, D17). **Applied to SB0 in rev3** (§4.4 reopen, §9.5 `meta`, §17). §4.4's reopen step reads the best-header id from `meta`. Core never persists `m_best_header`: `ChainstateManager::LoadBlockIndex` recomputes it over every entry with `IsValid(BLOCK_VALID_TREE)` using `CBlockIndexWorkComparator` (`v24 validation.cpp:5337-5360`; v23.1.8 `:5035-5036`; comparator `node/blockstorage.cpp:31-49`). The amended rule: `open` computes `best_header` as the maximum over the eligible entries, with a lower id winning a work tie (SB4 D9). The candidates are each stored leaf, or for a FAILED leaf its highest non-FAILED ancestor. CONFLICT is not a FAILED bit (`chain.h:116-118`, `:300-307`), so a CONFLICT-marked leaf stays eligible here, though not for `fork_target`. `meta.best_header` stays as a diagnostic. After a reopen, `best_header` can therefore move forward to a heavier stored leaf that a walk-back had left behind, which is exactly Core's behaviour after a restart.

**E6 — the producer may block only on O(1) leaf mutexes; wakes are notification-only** (DEC-152, Sol r1 R1-M2 on SB5 rev0; SB5 §4.5, D6, D25). **Applied to SB0 in rev2** (§6.1–§6.3, §7, §17); the text below is the draft it was applied from. Since rev2, control is stored in the slot one payload per kind (SB5 D29, DEC-155), which SB0's applied text states.
- **§6.3 P2**, "`change_tip` never blocks, so every wait is V's service loop", reads: "`change_tip` never blocks except on leaf mutexes whose every critical section is O(1), and calls no code except notification-only wakes; every other wait is V's service loop." Core has the same contract: validation callbacks are enqueued under `cs_main` through `SingleThreadedSchedulerClient::AddToProcessQueue`, which takes the leaf `m_callbacks_mutex` for one `emplace_back` (`v24 scheduler.cpp:176-183`). The scheduler thread holds it only for O(1) pops (`:145-155`).
- **§6.1's sketch.**
  - The `ArrayQueue` + `staged` + `SegQueue` slot is replaced by one leaf `Mutex<SlotInner>`. Its queue is reserved at Q, and its control list is linked from sender-allocated nodes. The reader takes events by swapping the queue against a spare buffer it owns, so no section is O(Q).
  - `SubWaker` (a `Fn()` or a `Waker`) is replaced by `Notify`: `Task(Waker)`, used through `wake_by_ref`, or `Thread(Arc<Parker>)`, a condvar permit. There is no caller closure, so no wake runs I/O, waits for a response or handles inline.
  - `front` and `pop_control` are replaced by `next()`, which selects control before data in one slot critical section (SB5 D24).
- **§6.2.** F3's row adds "the producer's only blocking is E6's O(1) leaf sections". F5's row reads `Notify` for `SubWaker`.
- **§7's runtime model.** Driver threads wake through a `Parker`, and tokio tasks through their `Waker`. No wake runs subscriber code.
- **Not changed:** F1–F11's semantics and the tip lock's scope (SB4 §9.1).

**E7 — durable ids are never reused** (Sol r1 confirmation on SB5 §5.1, D16). **Applied to SB0 in rev3** (§4.4, §17). §4.4's "stable ids from `meta.next_id`, never reused" reads: "an id that was ever durable is never reused. Ids issued in a Buffered suffix lost at power loss may be issued again after restart. No durable state names them: subscriber locators and the wallet's locator store hashes, and `locks` and `meta` commit with their records."

**E6a — waits inside a hold** (DEC-157 (3) note on m4; Opus high m4; SB5 §4.2, §4.5). **Applied to SB0 in rev3** (§6.1 "Producer wakes", §6.2 F3, §17). E6's "only O(1) leaf sections" applies to the fanout section and to every path a subscriber's progress needs. Across a hold the holder may also block on (ii) its own I/O and lock chains whose holders never wait for the tip mutex (cache → store, SB4's index locks, V's block I/O) and (iii) its in-hold service wait: V's (DEC-153, CH1-node LO-9) and the SPV header writer's (SB5 §4.8, SPV-LO9).

**E8 — the header step leases the write connection** (Opus high m7; SB5 §3.7, SB5-S1). **Applied to SB0 in rev3** (§9.5, §17). §9.5's one-writer bullet keeps "one writer" and adds: the SPV header step leases the store writer's write connection for its stage 3 (one message out, the connection back); `BEGIN IMMEDIATE`, the `HeaderStepSink::rows` batches, the O(1) tail and `COMMIT` run synchronously on the header writer's thread, and the connection is returned. Other `WriteSet`s queue meanwhile, exactly as behind one long `WriteSet`. Cold reads during stage 3 use a separate read connection. The step's rows stream in batches of ≤ W instead of one `take_dirty()` vector (SB5 D31).

## Appendix B. Evidence (specification probes)

The probes are in `/work/scratch/sb5-rev1-probes` (loom 0.7.2, offline). They are minimal models of the rev1 protocols, not production code. Each test was run alone with `cargo test --release <name>` (the `hold::` tests in `src/hold.rs`, with `LOOM_MAX_PREEMPTIONS=3`):

| Probe | Result |
|---|---|
| `f4_rev1_single_selection_holds` (Sol's three-thread witness, reader `next()` × 3 + `pop`) | passes |
| `f4_rev0_split_calls_negative_control` | fails as expected ("data observed before earlier control") |
| `space_wake_with_permit_completes` (Q = 1, swap intake, reader preempted inside both slot sections, permit parkers both ways) | passes |
| `space_wake_lost_permit_negative_control` | fails as expected: loom reports `deadlock; threads = [… Blocked …]` (its unwinding aborts the test binary, hence one negative control per binary in §10.2) |
| `f4_rev2_per_kind_consume_holds` (rev2 `next()`: controls stay in the slot and are consumed one per call, data intake in the same section) | passes |
| `hold::hold_q1_two_disconnects_flushdue_parked` (L10: V holds the tip mutex across two disconnects and a third step, Q = 1, S parked on its `FlushDue`) | passes |
| `hold::hold_q1_two_disconnects_closing_while_parked` | passes |
| `hold::hold_q1_two_disconnects_crashratio_completion` (adds W, whose completion needs S's ack of event 2) | passes (7 s) |
| `hold::hold_neg_no_flushdue_service` | fails as expected (loom deadlock: V and S blocked) |
| `hold::hold_neg_relock_tip` | fails as expected (loom deadlock) |
| `hold::hold_neg_retry_ignores_closing` (fanout and `retry_pending` both ignore `closing`) | fails as expected (loom deadlock: V blocked, S terminated) |
| `hold::hold_neg_completion_wait_without_retry` | fails as expected (loom deadlock: V, S and W blocked) |
| `status_register_then_recheck_holds` | passes |
| `status_missing_recheck_negative_control` | fails as expected ("parked status reader never woken") |

Sizes (x86-64, rustc release, `sz.rs` in the same directory): `BlockRef` 40 B, `TipStep`/`ChainEvent` 56 B, `VecDeque<ChainEvent>` header 32 B, `Option<Box<_>>` 8 B.

**rev2 probes** (`/work/scratch/sb5-rev2-probes`, representation and schedule models, not production code):

| Probe | Result |
|---|---|
| `step_budget.rs` (`rustc -O`; a counting global allocator around a model of the segmented step path: accept results, the reused write batch, and per sub-step the plan legs, the `TipChange` copy and the pending copy, each charged before allocation) | the observed peaks in §6.2: no A9, 359,680 B desktop and 113,920 B mobile at d = 9,000, c = 9,001 and at d = 50,000, c = 50,001; with A9, 261,376 B and 89,344 B; all ≤ the caps 360,704 B and 114,944 B. rev1's unsegmented no-A9 witness measures 1,728,096 B, matching Sol r2 |
| `control_kinds.py` (`python3 -I`; per-kind flags cleared on consumption) | Sol r2's interleaving, 10,000 rounds: at most 2 messages held (rev1's rule: 10 after 10 rounds). 2,000 random schedules × 400 operations over 3 kinds: at most 3; every consumed payload is the newest of its kind |

**rev3 probes** (Opus high M1–M3, m1, m5; specification models, not production code):

| Probe | Result |
|---|---|
| `src/rev3.rs` in `/work/scratch/sb5-rev1-probes` (loom 0.7.2; the tip mutex modelled at user level, §10.2): `m1_shared_events_thread_try_lock` (case 1), `m1_hub_lock_try_lock` (case 3: V takes the hub lock inside the hold while the events thread deregisters under it), `m1_hold_active_release_wake`, `m2_ingress_try_send` | all pass (6.4 s together) |
| `m1_neg_shared_events_thread_blocking`, `m1_neg_hub_lock_blocking` (rev2's blocking step 4) | fail as expected: loom deadlock |
| `m1_neg_hold_active_no_recheck` (`on_release` without the retry) | fails as expected: loom deadlock (V terminated, the registrant blocked) |
| `m2_neg_ingress_blocking_send` (the network actor forwards `headers` with a blocking send) | fails as expected: loom deadlock |
| `step_budget_r3.rs` in `/work/scratch/sb5-rev3-probes` (`rustc -O`; counting allocator; real sizes from SB4 562a0f053 via `examples/sb5_sizes.rs`; the status-only overlay as `HashMap<u32, u32>`; the bounded A5 list; no mixed sub-steps; 42 cases over desktop, mobile and mobile B = 500, with and without A9, d + c up to 100,001, marks 0 / 9,000 / 50,000) | all 42 within cap (+ M(m) with marks). The §6.2 observed table. The marks account equals M(m) exactly (221,216 B and 884,768 B) |
| `sendprobe` in `/work/scratch/sb5-rev3-probes` (Opus high's, with the `Hold<'c>` fix) | `assert_send::<HeaderChain>()` compiles; the chain moves into a thread, and the hold is released at seq 2 |

Measured sizes (x86-64, release, SB4 at 562a0f053): `BlockId` 4 B, `BlockRef` 40 B, `TipStep` 56 B, `Status` 4 B, `HeaderRecord` 144 B (176 B with SB5-A2's `hash`), `AcceptOne` 56 B, `ControlMsg` 24 B; `StepRow` 176 B with the hash.
