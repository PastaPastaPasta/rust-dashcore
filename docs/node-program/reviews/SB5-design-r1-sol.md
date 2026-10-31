# SB5 design review, round 1 — Sol

Reviewed `docs/node-program/designs/SB5-dash-chain-feeds.md` at **17750c58**. All SB5 line references below refer to that blob, not a moving worktree. Inputs: REVIEW-COMMON; SB0 APPROVED rev6.1 and E1–E4; SB4 APPROVED rev7.1 (DEC-149), including §9 and H1–H7; C0 source blobs at **f815ec15**; CH2 P8, §2.9.1, §4.4.1 and §7.2 b1–b3; EV addendum §2.6; DEC-95/99/102, DEC-144/145/146 and **DEC-151**. Also read the charter, process, relevant architecture principles and PLAN §7. PLAN §7 handling was followed; no private inputs were read.

DEC-151 settles the API shape, the SB4 stub/arena changes, E5 and the interim instruction budget. Those decisions are inputs to this review, not unanswered questions.

**Result: REWORK.** Three major findings need closure: control priority, the producer's blocking path introduced by D6, and the SPV peak-memory bound. The pending-order, retirement, ancestry and restart arguments otherwise have a sound basis.

## Major findings

### R1-M1 — Separate control and data calls do not establish F4's priority

**Severity:** major. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:258`, `:420`, `:452`, `:703`; CH2 `§2.9.1 F4` and `§4.4.1 rule 1`.

Using the same mutex in two separate calls does not order the whole reader step against enqueues. The stated protocol admits this schedule, with an initially empty slot and staging deque:

1. S calls `pop_control()`, sees no control and releases the slot mutex.
2. H calls `send_control(C)`, enqueues C and returns.
3. V enqueues data event E and returns from its slot operation.
4. S calls `front()`, refills staging and observes E. C remains queued until S's next step.

C was pushed **before E**, but E is observed first. This violates even SB5's narrower claimed property at line 452, as well as the planned L5 assertion. A wake in step 2 does not repair an already-running reader. CH2's separate hub mirror remains useful, but the feed's promised priority cannot be proved by saying its two calls each use the same mutex.

**Suggested fix:** make the decision between control and data one synchronized operation. For example, have data acquisition check pending control under the slot mutex and return a distinct control-pending outcome without exposing staged or newly queued data. Preserve that check when staging is already nonempty. Coordinate the resulting driver protocol with CH2b; simply making an extra unsynchronized `pop_control` call leaves another gap. L5 must schedule the real separate operations and include the four-step witness above, rather than treating a reader step as atomic.

**Evidence:** an isolated loom 0.7.2 model with three threads and the stated slot mutex finds this counterexample. The expected-failure test passes because loom rejects the claimed priority invariant.

### R1-M2 — D6 adds a wait under the tip mutex that is neither bounded nor a V service-loop wait

**Severity:** major. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:390`, `:391`, `:403`, `:410`, `:416`, `:787`; SB0 `§6.3 P2`; CH2 `§2.9.1 F3`, `§4.4.1 rule 5`.

The mutex avoids the ring's space-wake handshake, but changes the producer's progress contract. `try_push` is only a capacity test: its mutex acquisition can block. A reader refills the **whole** queue under that mutex, so the protected work is O(Q), not a constant number of element operations. Short source code and an uncontended 20 ns estimate do not bound acquisition time.

Concrete schedule:

1. S takes its slot mutex to refill Q staged events. Pause S after acquisition, before release.
2. V takes the tip mutex, changes the tip and assigns `seq`, then enters fanout to S.
3. V parks in the slot-mutex acquisition. It cannot return `Publish::Blocked`, enter its service loop, or service an arriving `FlushDue` until S is scheduled and releases the mutex.

This requires no subscriber I/O or forbidden tip-lock acquisition. Weak fairness permits an arbitrarily long finite pause. It distinguishes eventual deadlock freedom from SB0 P2's “`change_tip` never blocks, so every wait is V's service loop.” Inside enforcement it also adds a wait beyond the stated backpressure and CRITICAL waits. H4's absence of a reverse tip-lock edge alone does not close this gap.

There is a second unbounded route in the same critical section: `SubWaker::Fn` is arbitrary caller code. Line 403 forbids tip-locking APIs, but does not forbid blocking I/O, waiting for a V-serviced response, or inline subscriber work. A closure waiting for a response from V can deadlock while obeying that literal prohibition. Core's queued validation callbacks run on the scheduler; its header notification explicitly runs outside `cs_main` (`v24 validation.cpp:3708–3712`). A notification-only wake is safe, but must be an explicit contract for data wakes, status wakes, `ProducerWake` and `close_hook`.

**Suggested fix:** preserve a nonblocking producer path. A contended slot can become a pending target using `try_lock`, with a race-free unlock/space notification so the retry cannot strand; alternatively retain the ring or another proven nonblocking enqueue. State the bounded work under subscriber locks, reserve buffers, and define wakes as notification-only operations with no I/O, response waits or inline handlers. If a blocking leaf mutex is intentionally retained, obtain an explicit amendment to P2/F3 and document the permitted latency and servicing behavior; D6's uncontended cost claim is not such an amendment. Add a directed paused-slot-holder test that checks V returns to servicing, plus a contended-path benchmark. Do not use the 1,500-instruction budget as a bound on waiting time.

### R1-M3 — The SPV peak-memory estimate excludes allowed connect legs and understates pending entries

**Severity:** major. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:171`, `:554`, `:556`, `:578`, `:582`, `:590`, `:594`, `:725`, `:730`.

B bounds newly accepted headers in a batch. It does **not** bound `TipPlan.connect`, which can reconnect a previously stored branch. In the design's own two-9,000-header-branch fixture, let the fork be 9,000 blocks behind the current tip, retain both branches in the store, and give B one additional header so that it wins. A one-header acceptance step can disconnect 9,000 blocks and connect 9,001. With a full subscriber, its pending stream contains 18,001 events, not at most `d + 2,000 = 11,000`. Pausing the readers while SPV completes its composite fanout is sufficient; per-event ordering does not reduce the pending list.

The per-entry estimate is also incorrect for the specified representation. `SmallVec<[Arc<FeedSlot>; 8]>` reserves eight inline pointers even with five targets, and has its own metadata. On this x86-64 machine with the repository's smallvec 1.16.2 and default features, an equivalent 56-byte event plus that SmallVec is **136 bytes**, not 96. The 18,001-entry witness therefore uses about **2.45 MB for pending entries alone**, before deque spare capacity, the subscriber buffers and residency. This already exceeds the advertised mobile feed peak of 0.35 MB by several times. A SmallVec feature/layout change would require measurement; it would not justify charging only five inline pointers.

Two other terms need explicit conditions:

- The arena expression includes S, but the 4.0/1.5 MB totals use S = 0. The proposed 100-out-of-order-chunk fixture can contribute `100 × 1,024 × 200 = 20.48 MB` through S alone. PoW limits arrival rate; it does not prove S = 0 for all valid histories. Likewise, resident stale-branch entries are additional to the active window.
- H2 permits multiple completed freezes during an enforcement hold while V defers feed mirrors until the hold ends. “One outstanding commit and gate” in W does not by itself bound the backlog of opaque control copies subsequently enqueued by V. CH2b must supply an actual bound/coalescing rule before the table can charge <1 KB per subscriber.

**Suggested fix:** express pending allocation in terms of the actual `d + c`, measured `size_of::<PendingEntry>()`, target spill allocation and container capacity. Label the desktop/mobile totals as typical estimates with their S/stale-branch assumptions; give the adversarial terms separately. Include a paused-reader deep reorg into an already stored branch, the S fixture and the enforcement mirror backlog in A-F12, using the corrected terms. If a fixed mobile peak is required, provide a protocol that bounds the whole plan's buffering while retaining the composite handoff/publication exclusion. Do not silently split the tip critical section. The current 1.25× acceptance threshold is testing an invalid bound.

## Minor findings

### R1-m1 — The status cell lacks a specified park/recheck protocol and corresponding model

**Severity:** minor. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:298`, `:303`, `:307`, `:396`, `:691`, `:693`.

The data reader states “register waker, then recheck,” but `StatusReader::poll_changed` has no corresponding protocol for its separate facts, version and waker slab. A version check followed by waker registration permits: reader sees unchanged version; writer publishes and wakes the old slab; reader installs its waker and returns Pending. No later status change is required, so it can remain asleep indefinitely. The coalescing test does not exercise this case.

**Suggested fix:** specify publication/version ordering and register-then-recheck for status, including the relation between `seen_version` and the facts returned by `next_events`. Model publication between every check/registration pair, with a negative control. Also specify `StatusReader::drop` removing its slab entry so repeated status subscriptions do not retain wakers.

### R1-m2 — Loom coverage must preserve the actual operation boundaries and wake semantics

**Severity:** minor. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:78`, `:119`, `:434`, `:695`, `:699`, `:705`.

The seven models are useful, but the facade's mutex replacement for ArcSwap gives stronger synchronization and can prove properties that the production publication path does not provide. Similarly, an ideal permit primitive assumes the critical behavior the node channel and SPV Notify adapters must implement. A concrete way to obtain a false clean L5 result is to combine `pop_control` and `front` into one model action; the scratch probe only finds R1-M1 when their unlock/relock boundary is preserved.

**Suggested fix:** say which production algorithms execute through the facade, which adapters are modeled, and which synchronization edges the replacement adds. Model permits and waker registration explicitly; require planted lost-permit, missing-recheck, ignored-closing and stale-insert variants to fail. Cover actual node/SPV wake adapters and ArcSwap publication with directed threaded tests. L7 must include captures both before and after closing and more than one barrier/gate, while CH2b A-40/A-41 verify the real W and retained prune lock. Loom safety assertions and bounded deadlock detection are evidence for the modeled schedules, not a proof of scheduler fairness or producer latency.

### R1-m3 — CLOCK and mobile batch-saving statements need narrower arithmetic

**Severity:** minor. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:516`, `:582`, `:592`.

If every CLOCK reference bit is set, one insertion scans/clears cap entries before selecting a victim. Insert is amortized O(1), with O(cap) worst-case work under the cache mutex. Also, the stated chunk formula at mobile w = 2,048 gives five chunks for B = 2,000 and four for B = 500: the saving is **one chunk, 204,800 bytes** at 200 B/entry, not 1.5 chunks/~300 KB.

**Suggested fix:** state amortized versus worst-case CLOCK cost, include the all-referenced miss case in the cold-cache bench, and correct the batch-saving estimate.

## Confirmations and implementation constraints

### Feed ordering, retirement and the W seam

- **The blocked-set order argument is sound.** Suppose S has pending sequences 7 and 8 and another subscriber has room. On retry, failure to push S's 7 adds S to `blocked`; 8 cannot pass it even if a concurrent pop frees space between the two visits. The other subscriber may receive both. On a later retry S receives 7 first; successful delivery removes that entry's target exactly once. `blocked` must identify the slot generation/object, not merely a reusable table index.
- **The cut excludes the backlog.** S1 has pending 7; S2 hands off at the already changed tip with `next_seq = 8`. Pending 7 retains S1's captured Arc and never acquires S2. After it drains, step 8 captures both. This satisfies X28 and F10. `finish` under the tip mutex remains DEC-96's authorized exception, corresponding to Core `index/base.cpp:163–170`.
- **Closing before taking the tip mutex resolves the enforcement retirement cycle.** If V holds the composite guard with an event pending for S, S sets closing, then wakes V and W before waiting for the guard. V's next retry discards S. W either filters S out of a later capture or satisfies an earlier capture by observing closing on its predicate recheck. It can release the flush without the disconnect being preserved for S. SeqCst, monotonicity and the captured slot Arc provide the required H7 lifetime/order; CH2b still owns ignoring late reports and retaining the last durable prune locator until full deregistration.
- **Drop must stay independent of tip-lock completion.** Its flag/wake half is sufficient for retirement; lazy table cleanup is safe for obligations. Prune-lock release must remain the hub's post-hold deregistration action, as OQ10 says, rather than an immediate consequence of `close_hook`.
- **Revalidate generations under the tip mutex during cleanup.** A deregister can resolve A, set A closing and pause; another registration can reclaim A and install B at the same index. Resuming A's cleanup must compare the current generation/slot before removing anything. D3's never-reused slot object and generation-exhaustion retirement make this possible. Extend the reuse test with this delayed-cleanup interleaving; testing only calls begun after B exists is insufficient.
- **H1/H2/P10 remain separate node requirements.** SB5's immutable registry does not require W to take the tip or slot mutex. `FreezePacket` owns the intermediate `(new, tip_seq)` pair; the pre-enforcement published snapshot is not its substitute (E4/DEC-146). The node keeps its block index in CH2 B1 and loads it before recovery/activation; the SPV HeaderStore transaction must not be introduced into CH2b's node path. This is consistent with the C0 `NoStore` seam and CH2 b1–b3 split.

### Cold reads, ancestry and fail-stop

- **The stale-insert invalidation proof is valid for fresh store reads.** If a miss reads the old row while holding the cache mutex, a later invalidation cannot complete until that insert finishes, then removes it. If invalidation completes first, a later miss reads the committed new row. A scratch L6 model passes with the lock held across read/insert and fails with the lock dropped around the read. SQLite must not reuse a read transaction pinned before the commit for a new cache miss; test that contract in SB13. The proof guarantees no old cache entry after invalidation, not instantaneous cache coherence at the earlier COMMIT instruction.
- **Snapshots must use their own ancestry after a deep reorg.** For an A12,000 snapshot with w = 2,048 and a later B12,500 reorg at 9,000, its base entry still identifies A. Following that entry's immutable prev/skip links returns A at heights 9,001 through the base, although `active_at` now returns B there. Evicted in-view ids also retain A identity through their persisted records. No whole-chain pin or active projection is needed. Below an established anchor, preserve the normal BelowFloor/None boundary rather than treating absent pre-anchor history as corruption.
- **SPV None-plus-latch is conditionally safe under the stated adapter obligation.** A failed ancestry read can return None and take an unknown-block branch, but the same operation trips the latch before returning. Checking the latch after the dependent reads and before every persistence operation prevents that branch from becoming durable state. The check applies to `subscribe_at`'s `finish`, subscriber data writes, rewinds, wallet effects and locator transactions, and the store writer's execution of queued WriteSets. SB14/CH2b need fault-injection assertions for those paths. The fallible `try_*` methods are appropriate when a caller needs to distinguish absence immediately. No change to DEC-151's accepted trait shape is required.
- **P8 refusal is correctly placed.** Block/undo needs are refused before an SPV feed exists; node pruning compatibility remains an initialization/prune-lock concern. Status events remain outside F1.

### E5, stable ids, persistence and open

- **E5 follows Core.** v24 `ChainstateManager::LoadBlockIndex` scans all indices and recomputes `m_best_header` using `IsValid(BLOCK_VALID_TREE)` and `CBlockIndexWorkComparator` (`validation.cpp:5337–5360`); v23.1.8 does the same at `:5035–5036`. `IsValid` excludes FAILED, but not CONFLICT (`v24 chain.h:299–307`). Therefore a conflicting VALID_TREE leaf must remain eligible for this recompute even when it is ineligible for `fork_target`. The leaf/highest-unfailed-ancestor reduction is valid because accepted entries are at least TREE-valid and work increases along ancestry; context records are excluded. The comparator's address tie is already the declared D9 allowed difference (`v24 node/blockstorage.cpp:32–49`). Do not conflate the two selection predicates.
- **Add a non-tie E5 regression.** Persist a state whose in-memory best header walked back or was not restored by reconsideration, with a heavier TREE-valid leaf still stored. Open must select that heavier leaf, rather than the diagnostic meta id. Include a conflicting-but-not-failed leaf and a failed leaf with a TREE-valid parent, plus D9 equal-work membership tests. P4's reference must independently scan, not share the production leaf reduction.
- **Durable-id nonreuse is sound with the same-WAL premise.** Record and next_id advance in one header transaction; a later same-WAL subscriber commit cannot survive without that preceding header transaction. Wallet checkpoints use hashes and E3 recovery, not volatile ids. A lost Buffered suffix may therefore reuse its ids after process restart without aliasing surviving state. MemHeaderStore power-loss tests must discard old chain/read/cache instances when modeling that restart. SB0's older absolute “issued ids never reused” wording should be reconciled with this durable-prefix qualification.
- **Atomic active/CL/meta writes provide a recoverable cut.** Crash before the header transaction leaves the old whole step; crash after its commit can leave the new whole step even before publication. Open then rebuilds its active path and restarts sequence numbering for new feeds. The active cross-check is appropriate. Fault tests must also exercise a Durable subscriber commit after a Buffered header commit and show that its referenced record survives. Node recovery remains CH2b's B1/marker protocol, not this SPV open routine.
- **Open cost is conditional.** Leaves and cold failed-path walks add branch/path-dependent work to the window read. The ≤150 ms SQLite statement is an estimate, while `open/mem-w10000` measures MemHeaderStore. SB13 must supply the SQLite evidence and propagate errors on all those additional queries. This is a measurement limitation, not a reason to reject E5.

### PF0 and commit plan

DEC-151's **1,500 instructions per step per subscriber**, producer plus reader, is a reasonable interim absolute gate for a small enqueue/dequeue and notification path. It is not evidence that the implementation is faster than Core, nor a latency/lock-contention budget. The Core comparison remains BL-70. Both sides must count the same delivered events and equivalent wake/dispatch work; explicitly account for Rust's tip/snapshot work if Core's comparator measures only signal enqueue/dispatch. Join/drain all eight reader threads within the measured operation so their work is included. PF0's inherited process/thread instruction counters support that accounting (`contrib/node/perfgate/README.md:98`, `:213–220`). Include both uncontended and contended paths; a no-op Fn wake does not establish the tokio cost, which the design appropriately assigns to SB14.

The F0–F9 estimate sums correctly to **13.5 days**, with feeds A = 7 and residency B = 6.5. Preserve DEC-151's ownership coordination for F0/A1–A3: the SB4 implementer owns the stubs until SB5 starts. The current design is not ready to authorize overlapping implementation of the unresolved feed synchronization protocol.

## Validation and limits

- Core source checked at `v24.0.0-rc.3` **da567d7dba945143eee4d2ce1621c21b32089989** and `v23.1.8` **79b6c5215f33bd305ddf095747b932f516b7a9a7**. Checked load recomputation, comparator, IsValid, GetAncestor/BuildSkip, cache critical sections, notification lock scopes and BaseIndex handoff/Stop.
- Scratch artifacts: `/work/scratch/sb5-design-r1-sol/SB5.md` and `probes/`. Ran `TMPDIR=/dev/shm/sb5-design-r1-sol-tmp CARGO_BUILD_JOBS=12 cargo test --offline -- --nocapture` in the isolated probe crate: **5 tests passed**. Two are expected failures: loom finds the F4 counterexample and the unlocked stale-insert negative control. The locked read-through model passes; other probes establish the connect-leg arithmetic and the 136-byte default SmallVec pending layout.
- These are specification probes, not execution of SB5 production code, a SQLite integration test or a PF0 measurement. SB5 is a design, and the integration tree has no dash-chain implementation to run; C0 was inspected at its exact commit. Loom coverage of production adapters remains subject to R1-m2.
- No reviewed source, live chain instance, private input, GitHub message, commit or push was involved. The mode-700 probe TMPDIR was removed after validation. This review is left uncommitted.

## Verdict

**REWORK.** Close R1-M1 with synchronized control/data selection, R1-M2 with a nonblocking producer and notification contract or an explicit binding-contract amendment, and R1-M3 with truthful plan/allocation bounds and directed peak tests. The ancestry, durable-prefix id and E5/Core arguments are accepted subject to the implementation constraints above. The minor findings should be folded into the next design revision.

Sol gate satisfied: no
