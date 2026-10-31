# SB5 design review: sequential Opus high

**Under review:** `docs/node-program/designs/SB5-dash-chain-feeds.md` at **rev2 26556bfd0** on `node/integration`. The file is byte-identical at HEAD 9e8d26720. All SB5 line numbers below are at that blob.

**Reviewer:** Opus, high effort. This is the sequential review after Sol r1 REWORK, Sol r2 REWORK (concurrency accepted) and DEC-151/152/153/155. **PLAN §7 handling:** codenames only, and no private inputs were read.

**Inputs I read:**
- REVIEW-COMMON.
- The two Sol reports.
- DEC-151 to DEC-156.
- SB0 at 26556bfd0 (§4.4, §6.1–§6.3, §9.5 and the applied E6).
- SB4 rev7.1 (§3.7, D33 and D36).
- SB4's shipped code at **C2 6ef0a8e75** (`dash-chain/src/{tip,chain,store,arena,reader}.rs` and `Cargo.toml`).
- CH1-node at HEAD (rev3 text: §2.5–§2.6, LO-9, §8.3, §11).
- CH2 (`CH2-coins-flush.md` §2.9.1, §4.4.1 and the `Control` types).

**Core checks.** I read Core **v24.0.0-rc.3** directly:
- `validation.cpp:5313-5363` (`LoadBlockIndex`);
- the `m_best_header` sites `:2043`, `:3951-3971` and `:4089-4108`;
- `node/blockstorage.cpp:35-49` and `:100`;
- `chain.h:113-118` and `:300-307`.

**Evidence I produced:**
- A compile probe, `/work/scratch/sb5-opus-high/sendprobe` (finding m5).
- A read of the rev2 step-budget probe, `/work/scratch/sb5-rev2-probes/step_budget.rs` (finding M3).

## Verdict: APPROVE-WITH-CHANGES

**What already works.** The feed core is sound:
- the slot protocol, `next()`'s single selection, per-kind controls, cursors over shared segments, `closing`, the `ptr_eq` cleanup, the status arm/recheck, E5 and the loom plan;
- I found no ordering violation in F1–F11 and no deadlock in the V/W/subscriber protocol, provided that callers obey LO-9.

**What needs changing.** Three majors remain. Each has a bounded fix, decided below:
- M1: the tip-locking `ChainFeeds` calls wait out whole holds, and since rev2 SPV holds wait on subscribers;
- M2: SPV has no LO-9 analogue;
- M3: SB4's SPV overlay sits outside the step budget that DEC-155 says covers every allocation.

The fixes touch SB5's API in three places and add one ask each to SB4 and SB14. A narrow confirmation of M1–M3 and m1–m3 is enough; no full round is needed.

---

## Major

### M1. `register`, `subscribe_at` and `deregister` step 4 block for a whole hold, and their callers can be what the hold waits on

**Refs:** `:288-292` (`register`; `deregister`: "Blocks while V holds `enforce_block`'s guard"); `:585` (step 4 takes the tip lock); `:579` (`subscribe_at`); `:736` (the debug-only owner check); `:770` ("None of them is on a path V waits for"); §4.8 `:787-801`. CH1-node LO-9 (HEAD `:230`): a marked task "may wait only on leaf mutexes, I/O, and the feed and ack primitives".

**The defect.** During a hold, the tip mutex is not a leaf mutex: V or the SPV writer holds it across waits on subscribers. Any thread that blocks on it is therefore waiting on the writer. §4.7 asserts that none of these three calls is on a path V waits for, but nothing enforces it.

**What rev2 changed.** Rev2 makes it worse. Before rev2, the SPV writer held the lock only for one critical section of O(fanout). §4.8 now holds it across `wait()` for subscriber progress, for as long as the slowest subscriber takes.

**Concrete cases:**
1. **Node, shared events thread.** CH2 row 29 allows "a shared or per-subscriber thread".
   - S1 and S2 are driven on one events thread. V is in an ABC hold, and S2 is full with a cursor set.
   - S1's driver hits a fatal error and calls `deregister(S1)`. Steps 1–3 retire S1, then step 4 blocks on the tip mutex.
   - S2's driver is on that same blocked thread, so it never pops. V's `retry_pending` loop waits on S2.
   - **Deadlock.** H7 retires S1, not S2.
2. **SPV, tokio.** F6 itself specifies a 2-worker pool, and SPV subscribers are tokio tasks.
   - During a multi-segment `activate_segmented`, two subscriber tasks reach `subscribe_at` (a reconnect catch-up) or `deregister` (a shutdown) on the two workers. Both block on the parking_lot mutex, not inside `block_in_place`.
   - No worker is left to poll the lagging subscriber that `wait()` is waiting for. **Deadlock.**
   - With one blocked worker it is only starvation.
3. **Lock order with CH2b's hub.** In V's hold, V takes the tip mutex and then the hub lock (answering `FlushDue`). If CH2b's watchdog or W calls `deregister` while it holds the hub lock, the order is hub → tip. **Inversion.** SB5 states no order between the tip lock and the hub lock.
4. **The owner thread.** On V's own thread, `register`, `subscribe_at` and `deregister` self-deadlock in release builds; the check at `:736` is a `debug_assert!` only.

**Fix (decided).** No `ChainFeeds` method may block on a hold.
- **`deregister` becomes non-blocking.**
  - Steps 1–3 stay as they are.
  - Step 4 uses `try_lock()`. On failure it returns, and the entry is reclaimed lazily: by `register`, as now, and also by `retry_pending`, which already visits closing cursors and can drop the entry under the guard it holds.
  - F11 is unaffected. `closing` already guarantees "nothing more is enqueued" and "no wait targets it"; only reclaiming the memory is deferred.
- **`register` and `subscribe_at` stop blocking on a hold.**
  - They `try_lock()`. If that fails and `held_owner ≠ 0` (already published at `:736`), they return a new `Busy::HoldActive`.
  - If it fails and no hold is open, they yield and retry. An ordinary critical section is O(fanout).
  - SB5 adds `ChainFeeds::on_release(Notify)` or an equivalent hold-released wake, so async callers await it rather than blocking. `register`'s policy checks and allocation stay outside the lock.
- **Release-mode checks.** The owner-thread check runs in every build and returns `Refused::WouldDeadlock`.
- **Lock order.** State it as **tip → {hub, slot, parker, status slot}**. CH2b never calls a locking `ChainFeeds` method under its hub lock; with the change above, none of them blocks.
- **Tests:**
  - `hold_dereg_from_shared_events_thread`: case 1 above, with a negative control using a blocking step 4.
  - `spv_hold_subscribe_on_two_workers`: case 2 on `current_thread` and on 2-worker runtimes.
  - An L3/L10 variant with step 4 contended.

### M2. SPV has no LO-9: the SPV writer's waits service nothing, and SB5 puts no constraint on what its subscribers may await

**Refs:** §3.3 `:277` (the SPV drain before stage 4); §4.8 `:797` ("Every wait in step 3 is the writer's own service wait under the hold, exactly like V's in-hold wait"); `:799` (`wait`); Y1 `:493`; DEC-156 (1).

**Why the analogy to V fails.** V's in-hold wait *services* the requests its subscribers can be blocked on (`FlushDue`, `PruneClears`, W's wakes), and LO-9 forbids marked tasks from awaiting anything else. The SPV writer's `wait()` only parks. SB5 gives SB14 no rule about what its subscriber tasks may await, so the header task can sit in a cycle.

**Concrete cycle.** It needs only an ordinary SB14 shape:
1. The filters task, on `Connected(b)`, requests `cfilter(b)` through the network actor and awaits the reply before it pops the next event.
2. The network actor forwards inbound `headers` and `headers2` messages to the header task over a bounded channel. The header task is parked in `wait()` (in the hold) or in the pre-stage-4 drain, and is not draining that channel. The channel fills and the actor blocks on `send`.
3. The actor therefore stops reading peers, so the `cfilter` reply never arrives. The filters task never pops, and the header task never wakes. **Deadlock.**

This is DEC-156's cycle B with "V" read as "the SPV header task". The same applies to a subscriber that awaits a reply from SB14's driver if the driver runs on, or waits for, the header task.

**Fix (decided).** SB5 states an SPV rule normatively, **SPV-LO9**:

> Nothing the SPV header writer waits on (every subscriber's pop task) awaits, directly or through another actor, anything that needs the header writer's progress.

Two consequences follow:
- Inbound header messages must never be routed so that a subscriber's reply path can block behind the header task's queue. Either the channel is unbounded with drop-oldest, or the actor never blocks on it (it `try_send`s and disconnects or slows the peer, as Core's message handler does).
- SB14 applies DEC-156's marker or an equivalent to its subscriber tasks.

Add this as **Y2** to SB14, alongside Y1. Add a directed test with a model network actor: the subscriber awaits a reply that the actor can deliver only after the header task drains, as a negative control, plus the compliant routing.

### M3. The step budget leaves out SB4's SPV status overlay, the dirty-id set and A5's invalidation list. These are step-sized, and no configuration bounds them

**Refs:**
- SB5: §0; §6 `:867` ("every allocation on the SPV header-step path … charged"); A11(b) `:490`; A5 `:484`; §5.2 `:829`; `peak_mark_walk_write_batches` `:1117`; App. B `step_budget.rs`.
- SB4: §3.7 lines 150–157 (`overlay: HashMap<BlockId, HeaderRecord>`, every SPV status change, "an evicted id's record is loaded", applied at stage 4, dropped at stage 5); D33, D36.
- SB4 shipped stubs at 6ef0a8e75: `take_dirty_records() -> Vec<HeaderRecord>`, and `records_committed()` "invalidates the written ids".

**What lives through a step.** From stage 2 (accept and enforcement marking) to stage 5, three things are alive:
- SB4's overlay holds a **full `HeaderRecord` copy for every status change**. The writer must read it first (D33 read precedence), and it is applied to the resident seqlocked groups at stage 4 (D36).
- The dirty-id set covers the step's new ids plus every marked id.
- After the commit, A5 needs **all** written and overlay ids, to invalidate the cache after the commit (§5.2). So that list survives stage 3.

**The size.** All three scale with the number of marks, and nothing in the configuration bounds that. SB5's own directed case sets 9,000 cold CONFLICT marks in one step. That is about 9,000 × (176 B + hashbrown overhead) ≈ **1.9 MB** of overlay, plus about 36–72 KB of ids. That is 5× the desktop step cap (360,704 B) and 17× the mobile one (114,944 B).

**Why A11(b) does not fix it.** A11(b) says dirty and overlay records "leave as `StepRow`s through the step sink in batches of ≤ W, so no step-sized dirty set accumulates". Streaming rows out at stage 3 does not free the overlay, though: it must stay readable by the writer from marking onward and be applied at release. So as written, `peak_mark_walk_write_batches` cannot pass against SB4.

**The probe is no counter-evidence.** The App. B probe that reports "observed ≤ cap" models exactly the cap's own six terms (accept results, write batch, plan, change, pending, headers; `step_budget.rs:40-54`). It checks the arithmetic, not whether the budget covers everything.

**Fix (decided):**
- **(a) A status-only overlay.** Records are immutable except `status` (§5.1). The overlay becomes `HashMap<BlockId, Status>` plus a `StepRow::Status(id, status)` emission; that row type already exists at `:444`. That costs about 11 B per mark at hashbrown's load factor, versus about 210 B. `take_dirty_records` and the "an evicted id's record is loaded" read go away for cold marks. This is a new ask to SB4, **S5-A13**, amending D33.
- **(b) A bounded invalidation list.** A5 keeps at most `cold_cache.cap` ids. When the list overflows, stage 5 clears the whole cache: O(cap) under the cache mutex, which is still outside the tip lock. Correctness is unchanged: it still invalidates after the commit. This term is bounded by construction.
- **(c) Marks as an adversarial term.** The mark count is bounded by the length of the stored conflicting branch, not by the configuration. Like S in §6.3, it becomes a stated **adversarial step term**: about 11 B × marks for the overlay plus 4 B × marks of dirty ids. On mainnet a 9,000-header stored conflicting branch needs about 9,000 blocks of real proof of work. On test networks it is cheap: 9,000 marks ≈ 135 KB.
  - DEC-155's "every allocation" wording needs this one exception, recorded as a note on DEC-155, the same way Sol r2 accepted S for residency.
  - The full alternative is write-through cold marks with read-your-writes through the step transaction. That is rejected: it changes SB0 §9.5's one-writer model and SB13's read contract for a test-network-only cost.
- **(d) A-F12 cases.** `peak_mark_walk_write_batches` asserts **step-path observed ≤ cap + the marks term** (computed from the test's mark count). It also runs a 50,000-mark variant to show the per-mark slope is about 15 B.

---

## Minor

### m1. Budget exhaustion is underspecified, and for four of the five sub-caps the specified "wait" is a hang

**Refs:** `:867`, `:873` ("Past the cap"), `:1118` (`budget_exhaustion_takes_blocked`).

**The problem.** Only the **pending segment** sub-cap can be relieved by waiting: subscribers drain it, and that is the Blocked path. The other sub-caps are structural:
- accept results, because B is sliced by SB14;
- the write batch, because it flushes at W;
- the plan and `TipChange` legs, which are ≤ E by the choice of sub-target;
- the headers.

If one of them would be exceeded, the only charges held are the writer's own. Waiting on `retry_pending` with `pending` empty then returns `Done`, and the re-charge fails forever. Or, if the design means a park, it parks with no wake. The test `budget_exhaustion_takes_blocked` uses an artificial external charge, so it enshrines a behaviour that production cannot reach except as a hang.

**What happens when the budget is exhausted (decided):**
- **The pending segment** takes Blocked. This is the only legitimate case.
- **Accept results over B** are refused with `ChainError::Config("step exceeds step_headers")` before any work. That is the caller's bug.
- **Any other sub-cap overrun** is `ChainError::Internal`, trips the latch and fires a `debug_assert!`. It is a derivation bug, so the failure must be loud, never a wait.
- **The cap is a `const`** computed from `size_of::<StepRow>()`, `size_of::<BlockRef>()` and `size_of::<ChainEvent>()`. It must not use the document's estimates (184 and 176 are labelled estimates at `:869`).
- **The test** keeps the injected-charge case only for the pending sub-cap, and adds `budget_overrun_is_internal_error`.

**Are the caps realistic?** Yes.
- The arithmetic checks: 184·1,024 + 136·1,024 + 16·2,000 + 1,024 = 360,704 B (352.25 KiB, not 360 KiB); the mobile cap is 114,944 B (112.25 KiB).
- Mainnet reorgs under ChainLocks are d + c ≤ about 4. So both caps are dominated by the write-batch buffer (52% and 41%), not by reorg depth, and segmentation makes them independent of depth.
- They are small next to the feed caps (1.40 MB / 0.37 MB) and the decoded `headers2` message (≤ 640 KB). Nothing needs to move.

### m2. Where SB4's stage-4 side effects happen relative to sub-steps is unspecified, and a long hold widens D36's window from "a few instructions" to unbounded

**Refs:** §4.8 `:787-801`; SB4 §3.7 (the overlay applied to resident seqlocked groups "a few instructions before the new snapshot", D36); SB4 §10 (eviction is "the last part of `set_active_tip`").

**The problem.** §4.8 runs SB4's in-hold `set_active_tip(sub_target)` per sub-step, but it does not say where these happen:
- the overlay apply;
- eviction;
- the `max_cold_leaf_work` fold;
- the `residency_epoch` bump.

If they run at the first sub-step, `ChainReader::entry` readers (`reader.rs:88`, live seqlock status) see CONFLICT on B82…B110 while the published tip is still B110. That lasts for the whole hold, including every `wait()`. This is exactly the incoherence that D36 rejected.

**Fix (decided).** These run **once, at `release_tip`**, immediately before the snapshot store and for the final tip. Sub-steps change only the private active chain, `seq`, and fanout. A11 and A12 say so. Add `segmented_status_invisible_during_hold`: a reader polls `entry(id).status` during a paused `wait()`.

### m3. `activate_segmented` has no error path

**Refs:** `:787-797`; §9.1.

**The problem.** Sub-steps k ≥ 2 do cold reads inside the hold: `plan_tip`'s `lca`, `ancestor`, and leg materialization below the window. A `StoreError` there arrives after sub-steps 1…k−1 have been fanned out, so the private tip is intermediate and worse than the pre-hold tip. The design does not say whether `release_tip` publishes, or how F2 (the snapshot's `tip_seq` versus the fanned-out seqs) is left.

**Fix (decided).** Any `Err` after the first fanout trips the latch. The hold is dropped **without** a snapshot or status store, so nothing is published after the trip (§9.1) and readers never see the intermediate tip. Events already delivered stay valid, because stage 3 had committed `meta.tip = target` and `open` restores it. Test: `segmented_cold_error_mid_hold`.

### m4. §4.8's in-hold cold reads contradict §4.2 and A10, and SB0's applied E6 still says the SPV writer waits without the lock

**Refs:** `:512` (the tip lock "May block on: the O(1) leaves below only (E6)"); `:524` ("cache mutex … never taken under the tip lock (A10)"); `:789` (`plan_tip` after `hold_tip`); SB0 `:1132` and `:1153` ("waits in its service loop **without holding the lock**").

**The problem.** In §4.8, `plan_tip` and the per-sub-step leg walks take the cache mutex and the store read **inside** the hold. V's holds already do block I/O (`ConnectTip`). So the rule E6 actually needs is not "only O(1) leaves under the tip mutex".

**Fix (decided).** Restate §4.2's row and the lock-order sentence as follows:

> The holder of the tip mutex may block on (i) O(1) leaf mutexes, (ii) its own I/O and lock chains whose holders never wait for the tip mutex (cache → store; SB4's index locks), and (iii) the in-hold service wait. The O(1) rule applies to the fanout section and to everything a subscriber's progress needs.

Then:
- A10 stays, for latency.
- SB0 needs an E6 follow-up (call it E6a): §6.1's "producer wakes" and the F3 row allow in-hold waits (DEC-153 for V, §4.8 for SPV).

### m5. Storing the hold's guard in `HeaderChain` makes `HeaderChain` `!Send`

**Refs:** `:720` (`Held { guard: ArcMutexGuard<RawMutex, TipState>, … }`), `:722` ("set and clear `self.held`").

**The problem.** dash-chain enables parking_lot with only `arc_lock` (6ef0a8e75 `dash-chain/Cargo.toml:28`). Its guards then carry `GuardNoSend(*mut ())`. A struct with a field `held: Option<Held>` is `!Send` at the type level, whether or not a hold is open.

**Evidence.** The probe `/work/scratch/sb5-opus-high/sendprobe` fails `assert_send::<HeaderChain>()` with E0277 ("`*mut ()` cannot be sent between threads safely … required for `ArcMutexGuard<RawMutex, TipState>` to implement `Send`").

**Consequences:**
- ND1 cannot construct `HeaderChain` and move it into V's thread.
- SB15's `open` result cannot move into the header task.
- §3.3's SPV writer, which holds `&mut HeaderChain` across `.await`, cannot be `tokio::spawn`ed.

**Fix (decided).** Use CH1-node A1's shape: `hold_tip(&mut self) -> Hold<'_>`. The guard lives in a `Hold` value on the caller's stack, which is `!Send` there and nowhere else. The step and `retry_pending` methods take `&mut Hold` while a hold is open; `with_tip` becomes a method on `Hold` and on `HeaderChain`. The rejected alternative is enabling parking_lot's `send_guard`: it is sound, but it is workspace-wide and excludes `deadlock_detection`. Add `static_assertions::assert_impl_all!(HeaderChain<MemHeaderStore, Evicting>: Send)` to F0.

### m6. SB5's ask labels collide with CH1-node's; A12 duplicates CH1-A1; CH1-A11's `Option` tip is not reflected

**Refs:** §3.8 `:478-495`; CH1-node HEAD §11 `:1108-1120`.

**The collision.** Both designs label asks to SB4 **A1–A12/13**, with different meanings:

| Label | SB5 | CH1-node |
|---|---|---|
| A1 | `TipCell` loses `<E>` | `hold_tip` |
| A9 | `Arc` vectors | `check_index_with` |
| A11 | `StepBudget` | node empty active chain |
| A12 | product-agnostic hold | `accept` reports ids |

SB5 itself mixes them. §12's F3 row says "after … SB4 A1, A11, A12", where A1 means CH1's `hold_tip`, while §3.8's A1 is the `<E>` removal. The SB4 implementer cannot resolve this.

**The `Option` tip.** CH1-A11 makes `TipState.tip`, `TipFacts.tip` and `ActiveChainView::tip` `Option<BlockRef>` for the node. SB5 still uses `tip: BlockRef` (`:192`), `p != state.tip` in `subscribe_at`, and `ChainSnapshot::tip() -> BlockRef`.

**Fix:**
- Prefix SB5's asks as **S5-A1…S5-A13**, and SB13/SB14/CH2b's as **S5-S1, S5-Y1, S5-Y2, S5-C1**.
- Restate S5-A12 as "CH1-A1 is not node-gated".
- State that before genesis `subscribe_at` returns `Moved` while the tip is `None`, and that `ChainSnapshot::tip()` is only called after the node's genesis (ND1 starts subscribers after it, CH1 D8). Alternatively, return `Option`; CH2b decides.

### m7. The streamed header-step transaction contradicts SB0 §9.5's one-writer model, and SB0 has no erratum for it

**Refs:** §3.7 `:440-456`; `write_header_step` `:259`; S1 `:492`; SB0 §9.5 ("A single store-writer task owns the write connection … each `WriteSet` is exactly one `BEGIN IMMEDIATE … COMMIT`", assembled by the header task and submitted).

**The problem.** `HeaderStepSink::rows` is a synchronous call on the header writer, inside a transaction that "the sink holds open". There are two ways to implement it, and each changes something:
- the header task leases the write connection for the step;
- the store-writer task keeps `BEGIN IMMEDIATE` open across cross-task batch messages, and every other `WriteSet` waits for the whole stage 3.

Either choice also moves part of the allocations off the header thread, so A-F12's thread-local counter misses them. A single read/write connection on mobile would self-deadlock any stage-3 cold read.

**Fix (decided):**
- The header task **leases** the write connection from the store writer for stage 3: one message out, one back.
- Batches execute synchronously on the header thread, so the counter stays thread-local and correct.
- Cold reads use a separate read connection (the read-after-commit contract already implies one).
- Add SB0 erratum E8 for §9.5, and put the lease in S1.

### m8. The 17.5-day plan is optimistic, and OQ15 must be ruled before SB4 C4

**Refs:** §12 `:1148-1175`.

**Where the estimate is short:**
- **F3 (2 days)** carries the following, depending on SB4 APIs that do not exist at 6ef0a8e75 (no `hold_tip`; `tip_plan` and `TipChange` still have the old shape):
  - `with_tip` and the `Hold` refactor (m5);
  - both hold kinds;
  - `activate_segmented`;
  - `StepBudget`;
  - the §4.7 and §4.8 threaded tests;
  - L10 and L11 with negatives.
- **F8 (3 days)** carries:
  - P2 with restarts;
  - eight A-F12 cases under a counting allocator;
  - M3's term;
  - m2's visibility test.
- **SB4's side.** A11 and A12 are estimated at "+1 SB4 day". They change SB4's approved public API (`tip_plan → TipTarget`, `take_dirty_records →` sink, `TipChange` lifetime, M3's overlay). That work lands in C4 and C5, and SB4 is at C2 now.

**Estimate (decided):** F3 at 3 days, F8 at 4 days and F1 at 3 days (the M1 API), so **20 days**. SB4 A11/A12/A13 come to about 3 SB4 days on its critical path. The resulting slack is about 2.5 days, not 6.5.

**OQ15** (A11/A12) and **S5-A13** need a manager ruling **before SB4 starts C4**. Otherwise C4 implements `take_dirty_records -> Vec<HeaderRecord>` and the full-record overlay, only to rework them.

### m9. Every SPV step now fans out before publication, and consumers are not told to fence

**Refs:** §4.8 step 1 `:788` (every SPV step, even a single-segment one, runs inside `hold_tip`); §4.3 fanout `:530`; DEC-156 (2).

**The change.** In rev1, the SPV composite stored the snapshot and then fanned out (SB4 §9.1). In rev2 a subscriber woken by `Connected(b)` can read `chain.snapshot()` and still find the old tip, even on the typical d + c ≤ 4 step. For the node, CH1-node fences F1 consumers by `tip_seq` (§6.2). SB5 states no such rule for SB14 or SB11 consumers. For example, a filters adapter that checks "is b still active?" against the snapshot would drop a live block.

**Fix (decided):**
- `release_tip` stores the snapshot and status, then fans out the **last** segment, then drops the guard. That restores rev1's order for every single-segment step, and matches DEC-156 (2)'s publish-before-notify.
- Intermediate sub-step events necessarily precede publication. §4.4 F8 says a consumer combining an event with a snapshot first waits for `snapshot.tip_seq ≥ event.seq`, or resolves by id through `IndexAccess`, which uses immutable fields.

### m10. The sub-target formula in §4.8 step 3 is wrong for the mixed sub-step

**Refs:** `:791-792`.

**The problem.** "then connects: `ancestor(target, min(target.height, tip.height + (E − d_i)))`". If "tip" is the pre-sub-step tip at fork.h + d_rem, this gives height fork.h + E when d_i = d_rem, so d_i + c_i = d_rem + E > E. It is correct only if "tip" means the fork.

**Fix:** Use either:
- no mixed sub-steps: if d_rem > 0, `ancestor(tip, max(fork.h, tip.h − E))`; else `ancestor(target, min(target.h, tip.h + E))`;
- or the mixed form, with target height fork.h + (E − d_rem).

`segmented_equals_single` should also assert d_i + c_i ≤ E for every sub-step.

---

## Nits

- **n1.** A coalesced control keeps its first-queued position (`:570`). The delivered sequence is therefore not a subsequence of the send order. For example, sending A₁, B₁, A₂ delivers A₂ before B₁. This is harmless for CH2, whose queued copies are no-ops or read the mirror (CH2 rule 1). Moving the kind to the back on coalesce is still O(8), keeps F4's proof unchanged, and makes the property simple to state. Prefer it, or document the inversion in C1.
- **n2.** The "+1 being handled" in the control term (`§6.1`) is a driver contract: an owned `ControlMsg` can be stashed. Say so.
- **n3.** Two cap terms are conservative:
  - `Segment::Composite` stores `BlockRef`s (40 B), but the pending sub-cap charges 56·E. Either align the two or note it.
  - `[Option<ControlMsg>; 8]` is 8 × 24 B, not 8 × 16 B.
- **n4.** `open` checks `next_id` > every **loaded** id (`:988`). A cold stored id ≥ `next_id` is corruption that this misses until the first insert hits the UNIQUE constraint. Add a `max(id)` probe (`ChainMeta` or `HeaderStore::max_id`) and map a failure to `Corruption::NextIdNotAbove`.
- **n5.** C1's "latch check before every persistence operation (§5.4)" is moot for CH2b, because the node has no cold reads. It belongs to SB14's adapters (Y-row).
- **n6.** OQ9, which coalesces `ChainLocked` to the latest lock, differs from Core. Core queues every `NotifyChainLock` that passes the dedup into the callback queue, so a slow listener still sees each one (`chainlock/handler.cpp:312-320`). SB11 must confirm that it needs only the latest.
- **n7.** State the status publish order: snapshot store → `facts.store` → `version` bump → wakes. That is N1 for `StatusEvent`.
- **n8.** The `ControlKind` constructor must reject values ≥ `MAX_CONTROL_KINDS`, because `outstanding` is a `u8` bit set.
- **n9.** The SPV writer mixes `.await` (the §3.3 drain) with a blocking `wait` (§4.8). OQ14's dedicated thread should use one `Parker` for both.
- **n10.** `hdr_prev`'s `substr(header, 5, 32)` is internal byte order, so `children(prev)` must bind the hash in that order. SB13 should note it.

---

## Confirmations (fresh eyes)

**1. Concurrency (V, W, subscribers, signals thread)**
- **Slot sections.** Every slot section is O(1) and allocation-free as specified, and every wake is notification-only. Lock order is tip → leaf with no leaf nesting.
- **H7.** The `closing` store linearizes retirement, and `retry_pending` and `try_push` both honour it.
- **The signals thread** never touches the tip mutex, because SB5 has no API to it (DEC-153).
- **The F1 pop task.** On the reader side, `next`, `pop`, `remove_round_trip`, `set_waker` and `Drop` take only leaf locks. A pop task never awaits V **through `FeedReader`**. The LO-9 exposure is M1's `ChainFeeds` calls.
- **One snapshot and one status per hold.** §4.8 publishes the snapshot and status only at `release_tip`. Each sub-step gets its own `seq` and fanout, in the single composite's order, so the F1 stream is unchanged (subject to m10's formula), and `subscribe_at` waits out the hold. The publish-count property holds. m2 is the residual reader-visible status leak, and m9 the notify-before-publish order.

**2. DEC-155**
- **The per-kind flags bound the held controls.** Bit k is set iff k ∈ `ctl_order` iff `ctl_payload[k]` is `Some`; it is cleared only on the `next()` that returns the message. So the slot plus the reader hold ≤ 8 messages that `next()` has not returned, whatever the interleaving.
- **The F4 proof holds** with slot-resident controls, including compression's data-only intake.
- **The arithmetic** of §6.1, §6.2 and §6.3 rechecks:
  - per-feed: 174,992 / 45,968;
  - feed caps: 2,799,872 / 1,399,936 / 367,744;
  - step caps: 360,704 / 114,944;
  - typical totals: 4.36 MB (excluding retained snapshots) / 1.49 MB.
- What remains is completeness (M3) and exhaustion semantics (m1).

**3. E5 against Core**
- **The load recompute is correct.** Core recomputes `m_best_header` over every entry with `IsValid(BLOCK_VALID_TREE) && CBlockIndexWorkComparator()(m_best_header, pindex)` (`validation.cpp:5359-5360`).
- **Eligibility is right.** `IsValid` rejects only `BLOCK_FAILED_MASK = FAILED_VALID | FAILED_CHILD` (`chain.h:116`, `:303-304`), and `BLOCK_CONFLICT_CHAINLOCK = 128` is outside the mask (`:118`). So CONFLICT is eligible, as SB5 says.
- **The tie surrogate is right.** Loaded entries all have `nSequenceId = 0` (`blockstorage.cpp:100`), so Core breaks ties by pointer address (`blockstorage.cpp:44-46`). D9's lower-id surrogate is a deterministic stand-in.
- **The CONFLICT case matches Core.** Core's runtime walk-backs (`validation.cpp:3951-3971`, `:4089-4108`) leave a heavier CONFLICT-marked leaf unreferenced, and Core's reload re-selects it. `e5_conflict_leaf_eligible` pins exactly that.
- **The leaf reduction is sound.** Take an eligible e and any leaf L below it. Either L is eligible, or L's highest non-FAILED ancestor is at or above e in height; in both cases its work is ≥ e's.
- **P4's independent full scan** is the right oracle.
- **CH2b.** The node's load recompute is SB4/CH1-node's, not SB5's. SPV recovery (blind, from locators, under SB0 §9.5's sequential WAL) is consistent with `open`, with `durable_subscriber_after_buffered_header` and with E7.

**4. API fit**
- **CH2b:** `next`/`Next`, `ControlKind`, `Notify` and the registry fit, given C1's `Copy` payloads. CH2's `FlushCommit.to: Vec<_>` and `RewindRequest.targets: Vec<_>` must leave the feed copy, which C1 implies; state it.
- **SB13:** S1 plus m7.
- **SB14:** Y1 plus M2's Y2, m5, m9 and n9.
- **SB11:** `StatusEvent` plus n6.
- **The SB4 asks against 6ef0a8e75:**
  - A1 and A2 are not yet applied (`TipCell<E>` with `PhantomData`; `HeaderRecord` without `hash`), and are trivial.
  - A3 matches `Arena::set(&self, id, …)`.
  - A10 matches the stub doc of `records_committed`.
  - A11 is not minimal: it replaces the shipped `tip_plan`/`TipPlan`, `take_dirty_records` and `TipChange.snap`. Keep `tip_plan` for the node and add `tip_target`/`tip_plan_bounded` beside it.

**5. The test plan**
- **The loom set** (L1–L11 with planted negatives, one binary each) is realistic.
- **Missing cases**, all specified above:
  - M1's shared-thread and two-worker cases;
  - M2's actor cycle;
  - m2's status visibility during a hold;
  - m3's mid-hold error;
  - m1's overrun-is-error.
- **M3** changes `peak_mark_walk_write_batches`.

## Required changes (summary)

| # | Change | Owner |
|---|---|---|
| M1 | Non-blocking `deregister` step 4 (`try_lock`, then lazy reclaim); `register`/`subscribe_at` return `HoldActive` with a release wake instead of blocking; the owner check in every build; lock order tip → hub; tests | SB5; CH2b and SB14 adopt |
| M2 | SPV-LO9, normative in SB5; Y2 to SB14 (non-blocking header ingress, marker); actor-cycle test | SB5, SB14 |
| M3 | Status-only overlay (S5-A13); a bounded A5 list with clear-all; the marks term stated as adversarial (DEC-155 note); updated A-F12 | SB5, SB4, manager |
| m1–m10 | as decided above | SB5 (m6 jointly with CH1-node; m7 with SB0 E8; m8 needs OQ15 ruled before SB4 C4) |

**Verdict: APPROVE-WITH-CHANGES.** A narrow confirmation of M1–M3 and m1–m3 after the edits is enough.
