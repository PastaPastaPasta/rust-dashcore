# CH1-node design r2 — Sol

Reviewed rev1 **7f49c748**, using `git diff 3c7abeda 7f49c748 -- docs/node-program/designs/CH1-node.md`, against the r1 report and rev1 §15. **APPROVE-WITH-CHANGES:** R1-B1, R1-M1–M5 and R1-m1–m4 are closed at the design level. The two-path resolution of R1-M6 is correct, and CH1 agrees with SB5 rev1 plus the **a7297d13 §4.7** addendum. Two local corrections to the new `ValidationSignals` specification remain required.

This is a narrow confirmation. DEC-152/153, the accepted hold scopes, ownership split and other settled rulings are retained. Followed `briefs/REVIEW-COMMON.md`; PLAN §7 handling: codenames only. No source, design, commit, push or GitHub message was changed; this requested report is uncommitted.

CH1 line numbers below refer to **7f49c748**, not rev0. Core citations are to **v24.0.0-rc.3**, resolved locally to commit **0364ed626ec2eaa518bf1ae4a0566ce638c7fe18** and read with `git show`. Abbreviations: `v` = `src/validation.cpp`, `bs` = `src/node/blockstorage.cpp`, `h` = `src/chainlock/handler.cpp`, `cl` = `src/chainlock/chainlock.cpp`, `vi` = `src/validationinterface.cpp`, `sch` = `src/scheduler.cpp`, `coins` = `src/coins.cpp`.

## Findings by severity

No blockers. Two majors; no additional minor or nit findings in this scope.

### R2-M1 — major: LO-8 must classify the awaited reply, including a class M command's class C tail

**Location:** `docs/node-program/designs/CH1-node.md:216`, `:935`, `:1010`; relevant completion contract at `:118`, command partition at `:150–153`, and successful `ProcessBlock` reply at `:568`.

LO-8 permits a listener to await class R/M commands and forbids class C commands. However, class M describes only the first phase of several commands. `ProcessBlock`'s acceptance is M, while its caller's reply is explicitly withheld until its own ABC, a C tail. Running the M phase during a signals barrier therefore does not necessarily complete the listener's wait.

**Concrete schedule:** a queued listener submits a valid `ProcessBlock` with a reply and waits for it. An existing ABC reaches `limit_queue` with more than ten pending callbacks and enqueues its barrier. V services the block's M acceptance and defers its C ABC tail, as §2.4 requires. The listener still waits for that tail's reply; the single signals thread cannot finish the listener or reach the barrier; V cannot finish its current ABC or run the deferred tail until the barrier completes. Neither party holds a leaf mutex, so LO-5/6 do not prevent this cycle. The same dependency must be checked for reconsider/precious completion APIs.

Core expressly warns against callbacks calling ABC while ABC waits for validation callbacks (`v:3738`, `v:3754–3760`). `ProcessNewBlock` itself calls ABC before returning (`v:4899–4906`), so allowing its complete reply via an M acceptance label does not avoid that warning. The proposed §9.6 listener model needs this case in addition to a plain M command whose reply is immediate.

**Suggested fix:** state LO-8 in terms of reply dependencies: a listener may await only a reply that V can complete entirely through R/M release-point service, with no dependency on a C command or C tail. Explicitly prohibit awaiting the full `ProcessBlock`, reconsider and precious completion replies; posting their work without awaiting completion remains possible. If an acceptance-only reply is needed, provide a distinct capability/API while retaining the ordinary full-reply semantics. Add the schedule above as a rejected-await or deadlock negative control, alongside a permitted `ProcessHeaders`/`Query` wait.

**Evidence:** the bounded signals model reaches a deadlocked state after three states when the M reply depends on a deferred C tail. The corresponding plain-M-reply model reaches the barrier and terminates.

### R2-M2 — major: the proposed `VecDeque` enqueue does not meet the O(1) leaf critical-section rule

**Location:** `docs/node-program/designs/CH1-node.md:905–906`, `:915`, `:934`; DEC-152 leaf rule at `:200`, `:212`.

`queue: Mutex<VecDeque<Queued>>` plus `push_back` under that mutex is amortized O(1), not O(1) for every critical section. A full Rust `VecDeque` grows its contiguous allocation. A moving reallocation copies its existing contents while the queue mutex remains held. Swapping the queue against a spare makes consumer extraction constant-time, but does not remove producer growth.

**Concrete case:** a deep ABC reorg enqueues many `BlockDisconnected` callbacks during one hold while the consumer is delayed. When the deque fills, the next enqueue grows and copies the accumulated queue under L2s and L1. The ten-callback threshold is checked only at the next outer release point, so it is not a capacity bound that can justify a fixed reservation. Other producers can also enqueue concurrently. Consequently the cost of the leaf section can depend on the backlog, contrary to DEC-152/153 and §8.2's explicit promise.

Core's queue takes its callback mutex for enqueue and individual removal, releasing it before invoking listeners (`sch:145–155`, `sch:173`, `sch:176–183`); its limiter remains at `v:3716–3722`, called at `v:3760` and `v:3940`. This finding enforces the settled O(1) requirement; it does not request a different backpressure site or a `try_lock` fallback.

**Suggested fix:** specify a FIFO representation with constant-size linking/removal under L2s, such as preconstructed linked nodes whose allocation and destruction occur outside that leaf critical section. Keep enqueue independent of consumer progress and retain the existing limiter/barrier sites. Preserve the pending count for all not-yet-started callbacks, including callbacks moved to a private consumer batch (`sch:196–200`), and test growth under a paused consumer.

**Evidence:** compiled and ran a Rust witness using the specified mutex/deque push and a valid allocator that always moves reallocations. One push into full queues of 1, 128 and 16,384 entries copied respectively **32, 4,096 and 524,288 bytes inside the queue mutex**. No CH1 implementation is claimed by this witness.

## Confirmation of r1 closures

| Finding | r2 disposition and evidence |
|---|---|
| **R1-B1** | **Closed.** Connect/disconnect freeze from the private post-step tip and the step's sequence (`CH1:412–414`, `:436–438`, `:821–825`). Disconnecting B110 labels B109, not the removed event block. Every build checks `new.hash == coins.best_block() == evo.best_block()` when evo tracks the best block, and `new == private_tip`. Core commits the disconnect before `SetTip(parent)` (`v:3285–3287`, `v:3304`, `v:3316`); connect has the same gap (`v:3446–3448`, `v:3457`, `v:3473`). Full flush writes the coins cache's own best-block hash (`v:3092`, `coins:254–255`). A `FlushDue` in that gap is latched into `PendingFreeze`; no cause may freeze mismatched coins/tip, and the causes coalesce after the step. |
| **R1-M1** | **Closed.** A12 reports the late-header transition from both header entry points, including accepted prefixes of failed batches (`CH1:545`, `:576`, `:722–726`, `:1089`). V updates `best_known` before wrapping/publishing its view or replying. Height mismatch retains the old pair. This matches `cl:156–171`; the two late-header scenarios cover plain headers, accepted/rejected blocks, a partial batch and a prior lower known lock (`CH1:979–981`). |
| **R1-M2** | **Closed.** Enforcement captures `(floor, signature)` once and uses that pair through ABC and notification (`CH1:689–703`). A newer record or reset at an ABC release point does not replace the captured signature, and no “still latest” check was added. Matches `h:283`, `h:296–320`; directed schedules are specified at `CH1:999`. |
| **R1-M3** | **Closed.** Every CLSIG entering intake first completes `ClIntake`'s actual `CheckActiveState`, reconciliation and publication before duplicate/stale/bad-signature rejection (`CH1:654–666`). The coinbase-derived CLSIG enters the same intake. Matches the order of `ProcessNewChainLock` (`h:136–164`) and off→on reset (`h:209–220`). Quorum materialization remains the whole dispatched EV route. |
| **R1-M4** | **Closed.** `scope_end()` is normative on every non-fatal M exit and hold release, before replying (`CH1:642–644`); failed acceptance explicitly calls it (`:564–566`). It publishes status/mirror/view without signals. Thus the accepted header/floor and contextual failure are visible, while Core's early return still skips `NotifyHeaderTip` (`v:4819–4826`, `v:4890–4897`). |
| **R1-M5** | **Closed.** C1 requires a bijection between set and stored keys; insert replaces, sequence changes re-key, erase and prune remove the map entry (`CH1:246–256`, `:535–536`, `:608`). Duplicate unlinked visits and both sequence increments remain, matching Core's queue visits (`v:4287–4297`). Precious erase-before-change and low-end pruning remain aligned with `v:3873–3882` and `v:3584–3593`. Directed C1/termination controls are specified at `CH1:992`. D2 is explicit. |
| **R1-M6** | **Partially closed.** DEC-153's path split, callback sites, FIFO thread, barrier and per-step F1 servicing are correctly specified. R2-M1/M2 prevent accepting the new dispatcher's complete liveness/O(1) claim. Neither requires changing the ruling. |
| **R1-m1** | **Closed.** R6 now uses the full comparator and early exits, permits an equal-work precious release, and withdraws a universal invalid-candidate/time bound (`CH1:157`, `:998`). Matches `v:3777–3780`, `v:3791–3794`, the actual loop predicate at `v:3811`, and the sequence preference in `bs:38–40`. |
| **R1-m2** | **Closed.** The reference checks the non-active suffix and retains the active tip over pruned ancestors (`CH1:991`; `v:3533–3546`). Load tests apply dirty deltas to a retained database image and compare reconstructed Core state (`CH1:993`; `bs:305–323`, `v:5341–5357`). Crash tests use a durable B1 prefix plus ordinary ABC or the permitted DEC-148 fail-stop, including lost invalidation (`CH1:1016`). |
| **R1-m3** | **Closed.** Failed batches publish their accepted prefix but neither notify nor advance `last_notified_header`; a following successful batch notifies (`CH1:547–555`, `:982`). Matches `v:4739–4747`. |
| **R1-m4** | **Closed.** H1/`RecordedConnector` consume an ordered `(phase, hash, occurrence, result, state)` transcript, with acceptance checks separate and complete consumption asserted (`CH1:896`, `:1094`). H4 preserves the association. This permits state-dependent outcomes for repeated bytes (`v:2516–2518`). |

**New crash tests:** §9.5 requires both single-batch and split disconnect freezes, `(B109, q+1)`, crash after B1, reopen/replay to the labelled coins state, and synced-subscriber classification/ack; it repeats for connect and latched `FlushDue` (`CH1:1006`). §9.7 extends the power-loss cases and uses the corrected durable-prefix oracle (`:1016`). These are appropriate design acceptance requirements. They remain planned tests with model W until CH2b, which must repeat them with real W; they have not been presented as already passing implementation tests.

## ValidationSignals, SB5 and SB4 compatibility

**FIFO/barrier semantics:** the queued versus synchronous signal lists in §8.2 match `vi:193–330`. Core serializes queued callback execution and invokes callbacks outside its callback mutex (`sch:145–173`); CH1 assigns one FIFO consumer thread. `Barrier` completes after preceding entries, rather than requiring later producers to stop, matching `vi:160–174`. Limiting is `pending > 10`, at both Core sites outside `cs_main`, while C serialization remains active (`v:3716–3722`, `v:3738`, `v:3760`, `v:3940`). R/M service during that wait is necessary for safe listener replies; R2-M1 supplies the missing completion restriction. No queue wait is introduced under a tip hold.

**SB5 counterpart:** CH1 §2.5/§4.4/§4.5 and B1–B3 agree with a7297d13 §3.3 and §4.7. F1 remains exactly `TipStep`; fanout happens at every node step, pending drains before the next step, and `retry_pending` uses the already-held guard. Snapshot/status stores wait for release. `freeze_after_step` may precede the drain, but its completion waits run `retry_pending`. In-hold service answers `FlushDue`, `PruneClears` and W's notices; R/M commands and handoffs wait for release. The addendum explicitly covers all three hold kinds and forbids re-locking the same mutex.

**Q = 1:** disconnect 1 can fill the parked subscriber's slot and disconnect 2 can become pending. Servicing its `FlushDue` lets it pop, retry delivers disconnect 2, and only then may another step start. The closing and synchronous-freeze/direct-ack variants also progress in the bounded model. Disabling that service produces the expected deadlock control. This confirms the specified schedule, not an unimplemented threaded test or a general liveness proof.

| SB4 rev7.1 contract | Narrow regression check |
|---|---|
| **H1** W lock order | W still takes only its permitted file/store locks on freeze→release; no tip/feed lock dependency is added. |
| **H2** W-owned channel | Subscriber commit/rewind acknowledgements go directly to W; deferred control mirrors are not prerequisites for completing a hold. |
| **H3** in-hold service | V retains only the guard across waits, services index-only `PruneClears`, and latches `FlushDue` only during the coins/tip mismatch window. |
| **H4** feed progress | Reader/pop/space/closing paths remain independent of L1; held-guard retry avoids self-relocking. |
| **H5** event timing | Every step keeps its own sequence and immediate live-or-pending fanout, including enforcement disconnects. |
| **H6** subscriber progress | Processing, commit and direct ack do not require hold release; serviced `FlushDue` does not strand a live subscriber. |
| **H7** closing retirement | Atomic closing still retires prior/current/future ack and rewind obligations and clears pending; the last durable prune locator is retained until deregistration finishes. |

**Revised SB4 asks A1–A13:** no additional regression found. A1/A13 generalize the accepted hold/publication boundary while preserving per-step events and handoff exclusion; A2 supplies the private destination/sequence and private-chain hooks; A3 adds node traversal without changing SPV residency; A4 is optional/read-only. A5 keeps record-only semantics and SPV's fused composition; A6 surfaces resets; A7 retains `Some` for performed no-op enforcement and reports the captured floor. A8 uses the existing IBD latch. A9 shares the DFS state; A10 is non-inserting contextual validation; A11 makes the node's absent tip concrete while retaining SPV anchors; A12 surfaces late headers even in failed batches. The D28 and flush-site refinements remain explicit errata, and N3/N5 remain gated on the counterpart APIs. The permitted trailing-floor tip and Core's existing invalidate/batch-top choices are retained.

## Executed validation and limits

- Read the exact design revisions, r1 report, SB4 rev7.1/H1–H7, SB5 at a7297d13, relevant charter/process/PLAN/A1 context and DEC-152/153. Verified the cited bodies from the pinned Core tag. Current CH1/SB5 documents matched these requested revisions during the review.
- `python3 /work/scratch/ch1-node-design-r2-sol/design_models.py`: **12 tests passed**. Includes the post-step/latched-freeze transitions, closure witnesses, FIFO barrier, Q=1 variants and missing-service control, permitted M completion, and the M-with-C-tail deadlock witness. The four Q=1 variants explored 12, 11, 25 and 24 reachable states respectively with no deadlocked terminal state.
- `rustc --edition=2024 -D warnings .../queue_growth.rs -o .../queue_growth`, then the binary: **passed**, producing the three measured growth-copy witnesses in R2-M2. Compiler: rustc 1.98.1. This is a compiled Rust container/allocator experiment, not a repository build.
- Sources, extracted Core files and logs are under `/work/scratch/ch1-node-design-r2-sol/`. Tests used mode-700 `/dev/shm/ch1-node-design-r2-sol-tmp`, removed afterward. No dashd instance or shared chain datadir was touched.
- CH1 and its revised APIs/real-W tests are a design specification, so no CH1 implementation, loom run, crash suite or full workspace gate is claimed here.

## Verdict

**APPROVE-WITH-CHANGES.** Keep the r1 closures and DEC-153 architecture. Before closing the Sol design gate, tighten LO-8 around awaited completion dependencies and replace the growable-deque leaf enqueue with a representation that actually satisfies DEC-152. The remaining corrections are local to the new dispatcher.

Sol gate satisfied: no
