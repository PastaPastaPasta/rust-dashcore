# SB5 design review, round 2 — Sol

Narrow confirmation of **f6cb2226** plus the DEC-153 addendum **a7297d13**, using `git diff 17750c58 a7297d13 -- docs/node-program/designs/SB5-dash-chain-feeds.md docs/node-program/designs/SB0-shared-base.md`. All SB5 line references below are to **a7297d13**, blob `1b0cb39076e27b210b1dc431645a4551416773fc`. The integration worktree's SB5 file matched that blob when reviewed.

Inputs: REVIEW-COMMON, the Sol r1 report, the charter/process and relevant architecture context, PLAN §7, SB4 rev7.1 H1–H7 and its C0 source at **f815ec15**, CH2's feed and flush rules, CH1-node's hold/flush seams, and DEC-144/145/146/151/152/153. PLAN §7 handling: codenames only; no private inputs were read. DEC-152's permitted leaf-lock wait and DEC-153's separate notification paths are binding inputs.

**Result: REWORK, confined to the remaining memory-accounting claims.** R1-M1, the intended DEC-152 producer protocol, R1-m1–m3 and the DEC-153 hold protocol are accepted. R1-M3 is substantially improved but not fully closed: the permitted no-A9 fallback omits concurrent vectors from the combined peak, and slot-only control coalescing does not bound reader-owned controls. Two minor documentation/conditional-bound corrections are also recorded below.

## Major findings

### R2-M1 — The no-A9 fallback still undercounts the combined SPV reorg peak

**Severity:** major; remaining R1-M3 closure item. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:443`, `:815`, `:825`, `:842`, `:890`, `:1034`, `:1036`. Source cross-check: **f815ec15**, `dash-chain/src/tip.rs:63` (`TipChange`) and `:77` (`TipPlan`).

The no-A9 pending representation correctly costs `(d + c) × 56 B` for its copied events. That is not the combined plan/result-and-pending peak. Without A9, SB4's returned `TipChange` still owns separate `Vec<BlockRef>` disconnect/connect legs. Those vectors must coexist with the copied pending events at least while `set_active_tip` constructs and returns its result. The caller can also retain the result while the subscribers lag. Neither the SPV residency table nor its adversarial additions charge those vectors separately; the additions give only `× 56 B` for this case.

Use the same paused-reader witness: d = 9,000, c = 9,001, full queues, one new accepted header. Even with exact capacities and only one pair of BlockRef vectors:

- TipChange vectors: `18,001 × 40 = 720,040 B`.
- Copied pending events: `18,001 × 56 = 1,008,056 B`.
- Simultaneous allocation: **1,728,096 B**, before container headers, allocator overhead or any separately retained TipPlan.

That lower bound exceeds `1.25 × 1,008,056 = 1,260,070 B`. A test measuring only the pending container can satisfy line 1036 while missing the peak that the report and totals claim to describe. The A9 case avoids this duplication when its ownership transfer and sharing are implemented as specified.

**Suggested fix:** either make A9 a required dependency and remove the no-A9 alternative, or distinguish pending-only bytes from the combined peak. For the fallback, charge at least `(d + c) × (40 + 56) B`, using actual capacities, and account for any additional plan copy that the ownership protocol permits. Extend `peak_deep_reorg_paused_readers` to sample the allocation overlap with the returned TipChange alive, in both configurations if both remain supported.

**Evidence:** `/work/scratch/sb5-design-r2-sol/memory.rs`, compiled with `rustc -O`, reproduces the 40/56-byte layouts and the exact-capacity simultaneous allocation above. This is a representation/ownership probe, not an execution of SB5 production code.

### R2-M2 — `control_backlog` coalescing does not bound controls already taken into the reader

**Severity:** major; remaining R1-M3 closure item. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:262`, `:501`, `:505`, `:510`, `:807`, `:831`, `:839`, `:1038`.

`control_backlog(id)` counts slot controls not yet taken by the reader. Intake takes the entire list, sets `ctl_len = 0`, and appends the messages to the reader's private `ctl`. `next()` returns only one of them. The proposed rule can therefore bound the slot and still accumulate arbitrarily many reader-owned messages, contradicting the combined control term's **≤ 3 messages / ≤ 600 B** claim.

This happens even under the stronger interpretation of "different kind" that allows at most one queued message of each kind:

1. During delivery of the completed hold's mirrors, V queues a FlushCommit and a ChainStateFlushed copy.
2. S calls `next()`: both move into private `ctl`, the slot count becomes zero, and only one control is returned. One remains private.
3. V observes zero and queues the next pair. S takes both and returns one; two remain private.
4. Repeat. After ten such calls the slot count is zero but **ten controls remain private**. Four iterations already leave more than three.

A long hold can supply all those mirrors; another tip step is unnecessary. The paused-subscriber test at line 1038 misses this interleaving because a fully paused reader never clears the slot counter. A count alone also does not identify which kinds remain outstanding.

**Suggested fix:** retain the general k-message accounting until CH2b supplies a protocol that bounds slot **plus reader-owned** outstanding messages. If the three-message bound is required, track an outstanding notification per kind until the reader consumes it, rather than clearing that obligation at intake, or provide another explicit bound for both buffers. Add a directed peak case with V delivering successive mirror pairs between reader intakes. Preserve W's independent channel, captures and ack semantics; this correction concerns the feed copies.

**Evidence:** `/work/scratch/sb5-design-r2-sol/control_peak.py` models the stated take-all/return-one boundary and the stronger per-kind slot coalescing rule. Ten rounds produce `slot backlog=0, reader controls=10`.

## Minor findings

### R2-m1 — E6's wording is acceptable, but it has not been inserted into SB0 in this range

**Severity:** minor. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:38`, `:1158`, `:1162`; `docs/node-program/designs/SB0-shared-base.md:1084`, `:1145`, `:1160`.

The Appendix A draft correctly amends P2 to permit only O(1) leaf-lock sections and notification-only wakes, and updates the slot, selection and runtime sketches consistently with DEC-152. However, the requested range contains **no SB0 diff**: its blob is `7aea3305023c2e00337c94998be71926c738b6e4` at both endpoints. SB0 still says `change_tip` never blocks and retains the ArrayQueue/SegQueue/SubWaker sketch; there is no E6 entry there. A downstream implementer reading SB0 alone gets the superseded contract.

**Suggested fix:** the manager should apply the accepted E6 to SB0, or explicitly mark its old sections as superseded by DEC-152 and SB5 Appendix A. Until then, describe the erratum as approved/drafted rather than textually applied. This is a publication gap; it does not reopen the manager's ruling.

### R2-m2 — A8's proposed eviction rule does not establish S = 0 for sparse active ids

**Severity:** minor, conditional on adopting the optional optimization. **Refs:** `docs/node-program/designs/SB5-dash-chain-feeds.md:442`, `:883`, `:1037`.

A8 proposes evicting a chunk whose remaining resident entries are all off the active chain. S is defined more broadly: an in-window entry with an out-of-order id can pin a chunk. Such an entry can be active. Accept active-path headers interleaved with enough valid side-branch headers to put successive active ids in separate chunks. Removing every off-active entry still leaves each active entry pinning its chunk; A8's condition cannot evict those entries. It therefore does not imply the advertised zero-S result.

**Suggested fix:** keep the measured S term after A8 unless a stronger residency/compaction policy proves its elimination. Add sparse **active-path** ids to `peak_out_of_order_chunks`; qualify A8 as removing stale-only pins, rather than all sparse-id pins. The existing formula with S retained is acceptable.

## Confirmations and closure status

| Item | r2 disposition |
|---|---|
| R1-M1, synchronized selection | **Closed.** Intake transfers controls and data in the same slot section; every `next()` consults private controls before staged data. This includes already staged data. C pushed before E is transferred no later than E's intake and must be returned first. The original split-call witness fails in the negative control and passes with the new selection. |
| R1-M2, DEC-152 | **Protocol closed under the manager's contract.** Reserved queue/staging/spare buffers, header swaps, constant-size decrements, preallocated control links and constant-size waker sections remove the O(Q) slot-holder work. Reader append, control reversal and compression run outside the slot lock. Wakes run after leaf unlock; cache invalidation moves outside the tip hold. E6 publication remains R2-m1. |
| R1-M3, memory | **Partially closed.** d and c are independent of B; shared segments and generation-bound per-target cursors remove per-event target arrays. The SmallVec estimate is withdrawn. Reserved buffer bytes, typical totals, stale branches and S are stated separately, and directed peaks are specified. R2-M1/M2 and the conditional R2-m2 remain. |
| R1-m1, status | **Closed at design level.** Facts precede the SeqCst version bump; `arm` checks, registers and rechecks. `next_events` reads version before facts and diffs identities, so an early observation of newer facts does not duplicate events. Drop removes the status slot from the reader list. The publish/arm model passes and its missing-recheck control fails. |
| R1-m2, loom boundaries | **Closed at design level.** §10.2 identifies the production operations, preserves API boundaries, models permits, lists the stronger edges from replacing ArcSwap and states fairness/latency limits. Real ArcSwap/tokio/owned-guard checks and W integration checks remain implementation acceptance tests. |
| R1-m3, CLOCK/mobile | **Closed.** CLOCK insert is amortized O(1), with the all-referenced O(cap) sweep stated and benchmarked separately. Mobile B = 2,000 → 500 saves one 204,800-byte chunk under the stated typical assumptions. |
| DEC-153, §4.7 | **Accepted.** TipStep remains the F1 stream; Core callbacks belong to CH1-node's separate dispatcher. Every node step drains through the held guard before another step. Completion waits also retry pending fanout. No class R/M command runs inside the hold. |

### DEC-152 implementation constraints

The closed Notify enum removes the Fn variant and applies the same permitted wake types to data, control, status, producer and close notifications. DEC-152 explicitly permits `Waker::wake_by_ref`. As §3.1 acknowledges, a custom Waker is not intrinsically notification-only: the adapter contract must cover its foreign operations. Preserve the no-callback/no-free leaf-section rule when replacing a waker: move the previous handle out and destroy it after unlocking. The allocation check should cover deallocation as well as allocation. These are implementation obligations of the specified O(1) sections.

Inside the producer's held critical section, no specified reader append, handler, persistence operation or response wait is on a leaf-lock holder's path. A preempted reader can still delay V until it is scheduled and completes its O(1) section; DEC-152 authorizes that delay. The existing DEC-96 handoff `finish()` exception is unchanged and is not a subscriber-progress path during a V hold.

### Held-guard and V/W/subscriber argument

`with_tip` selects the existing guard **or** a fresh acquisition, never both. Its exclusive borrow prevents another writer operation through the same HeaderChain during the closure. Step, retry and pending checks must all use that selection; V must not invoke ChainFeeds' locking registration/handoff/cleanup methods during its own hold. The debug owner check and planted re-lock model cover the prohibited route.

With Q = 1, event 2 can be pending while S waits for FlushDue. V's in-hold loop answers the request; S consumes event 1 and signals space; retry through the guard delivers event 2. In the completion variant W waits for S's ack of event 2, so V must retry during that wait, before parking. W requires no tip/feed/hub lock. Closing instead satisfies both V's pending predicate and W's captured obligations, with notifications after the closing store. The parties waiting for hold release are registration, handoff and final deregistration cleanup; none is a prerequisite of those progress paths.

I found no additional deadlock in this specified protocol. The original completion probe does not park S on FlushDue. I therefore also ran a scratch variant that combines **S parked on FlushDue, V inside the hold, and W awaiting event 2's ack**. It completes; removing retry from that completion wait produces the expected three-party deadlock.

### SB4 H1–H7 and DEC-144/145/146

- **H1/H2:** W's freeze-to-release path retains its independent channel, lock-free registry capture and direct subscriber acks. Feed mirrors remain V's post-hold copies. The memory correction in R2-M2 must not alter that channel.
- **H3:** in-hold servicing permits PruneClears, FlushDue and W release/completion notices. The CH1-node latch prevents a FlushDue freeze between a connector commit and its corresponding tip step. Queries and mutations wait for release.
- **H4/H5/H6:** subscriber intake, pop, compression and notification take no tip mutex; each disconnect is fanned out at its step. Pending delivery is retried during every dependent wait, rather than postponed to hold release. Snapshot/status publication remains deferred.
- **H7:** closing remains monotone SeqCst on the captured slot object; producer and W notifications follow it. Earlier and later W captures retire closing targets, while their durable locator and prune lock remain until post-hold deregistration. Delayed cleanup revalidates both generation and pointer.
- **DEC-144:** per-disconnect pressure decisions/CRITICAL waits remain at their required sites; the new drains do not reinstate an end-of-hold pressure deferral. **DEC-145:** per-step events remain visible during the hold. **DEC-146:** closing retirement and the freeze's private `(new, tip_seq)` pair remain intact; the pre-hold snapshot is not used to label an intermediate freeze.

The real CH2b W/retained-prune-lock cases A-40/A-41 and the CH1-node threaded hold/flush cases remain required integration checks. The scratch models do not replace them.

## Validation and limits

- Checked the prescribed exact range and C0 source blobs. Rechecked Core's callback leaf lock and callback execution scope at both **v24.0.0-rc.3** and **v23.1.8**, `src/scheduler.cpp:145–183`. At v24 also checked validation enqueue, limiter sites (`src/validation.cpp:3716`, `:3760`, `:3940`), DisconnectTip's flush/tip ordering (`:3304`, `:3316`), and BaseIndex handoff/Stop (`src/index/base.cpp:163–170`, `:419–425`).
- Copied the supplied probe crate into `/work/scratch/sb5-design-r2-sol/probes`, with its own target directory. Ran each original test separately with `TMPDIR=/dev/shm/sb5-design-r2-sol-tmp CARGO_BUILD_JOBS=12 LOOM_MAX_PREEMPTIONS=3 cargo test --offline --release <name> -- --exact --nocapture`. Initial compilation exceeded the per-command deadline for the first two tests; both were rerun successfully after the build completed.
- **All 13 original probes produced the expected results:** six positive passes and seven planted-negative detections. L10's three positive variants pass; all four planted bugs produce loom deadlock reports: missing FlushDue service, re-locking the tip, ignoring closing, and waiting for completion without retry. Each deadlock negative ran in its own process; panic-unwind failure gives a nonzero process exit, which was accepted only with the expected loom deadlock diagnostic.
- The added combined FlushDue/completion positive passes and its no-retry negative deadlocks: **15 verified cases total**, seven positive passes and eight expected negative detections. Logs and aggregate results are in `/work/scratch/sb5-design-r2-sol/verified-results.json` and the adjacent per-case logs. The memory/control counterexamples are the standalone artifacts cited above.
- These are specification probes. SB5 production code is not present in the integration tree; the directed peak, real-adapter and real-W tests described in the design have not been executed. No workspace source was changed and no live chain, private input, GitHub operation, commit or push was involved. The temporary test directory was removed after validation. This report is left uncommitted.

## Verdict

**REWORK.** Retain the accepted selection, O(1) leaf-lock and hold-servicing protocols. Close R1-M3 by correcting the no-A9 combined peak and the control-buffer coalescing bound, with directed tests covering their allocation/interleaving witnesses. Apply E6 to SB0 and qualify the optional A8 zero-S claim.

Sol gate satisfied: no
