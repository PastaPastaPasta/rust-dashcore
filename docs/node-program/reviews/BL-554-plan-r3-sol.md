<!-- Copy of /work/scratch/ch3/reviews/BL-554-plan-r3-sol.md for reviews/ (DEC-984), 2026-10-31. -->

# BL-554 plan re-review, round 3 — Sol

Verdict: **APPROVE-WITH-NITS**. Findings: **0 major (M), 2 minor (m), 1 nit (n)**.

Reviewed `/work/scratch/ch3-bl554/PLAN.md`, rev 2, and `AUDIT.md` against the frozen worktree `/work/home/agent/workspace/rdc-wt/ch3-bl554` at `d75af234488f53b36d083756a8a9d26446db91d4`, based on `b0ca84330b98b9d87e76a88ed6c9035fbb94505a`. `PLAN.md` references below mean that scratch file; source paths are relative to the frozen worktree. HEAD's changes from the base are comments only. Some plan/audit source line numbers precede those comments.

The new W-side argument resolves the production counterexamples from round 2. After a winning guard returns, the ordinary error path has no remaining dependency on the tip, coins cell, hub window, queue, or a V reply once `end_flush` moves after report completion. The ordinary panic fallback and scoped `Ended` also fix the missing-report paths. The remaining issues are an incorrectly ordered defensive assertion, incomplete coverage of one guard branch, and an overstated registration guarantee.

This is a static plan review, not implementation validation. No builds or tests were run, as explicitly requested. Only this review report was written.

## Findings

### m-1 — The owned-but-Ok debug assertion prevents its fallback report

**Severity:** minor.

**Locations:** `PLAN.md:98-99`, `PLAN.md:111-114`; existing unwind behavior in `dashd-chainstate/src/flush/driver.rs:345-356`.

Concrete sequence:

1. A scoped step wins the latch trip but returns `Ok`/`Refused` without reporting. This is the abnormal outcome for which the new branch is expressly intended.
2. `catch_unwind(|| job.step(env))` has already returned `Ok(other)`.
3. In a debug build, `scope.owns()` is true and `debug_assert!(false, ...)` panics before the fallback sink call. This assertion is outside the catch boundary.
4. Scope destruction clears pending unreported. The waiting reader proceeds. On ThreadDriver, `Ended` loses its trip and makes no report; InlineDriver has no `Ended` fallback.

Thus this branch still produces an unreported release in debug builds, contrary to the claimed “reports and asserts” policy. It is not another ordinary production counterexample: no current production step returning this outcome after owning a trip was found. That keeps the finding minor.

**Suggested fix:** call the fallback sink and `scope.reported()` before the assertion, or arrange to catch this assertion while the owning scope remains live and report before resuming the unwind. Add a debug-enabled case that deliberately returns a nonreporting outcome after an owned trip; tests 8a–c do not exercise this arm.

### m-2 — Test 4's file-failure schedule cannot exercise its losing-operation trip

**Severity:** minor.

**Locations:** `PLAN.md:171-173`, `PLAN.md:200`; `dashd-chainstate/src/store/guarded.rs:544-561`; `dashd-chainstate/src/store/blocks.rs:61-103`, `:160-179`.

Test 4 says W's **file write** has already tripped and is held before reporting, then another thread calls `save_block`, and claims coverage of both refusal branches. The files mutex makes the second branch unreachable under that schedule:

1. If W has already tripped, the save enters `mutate_as` after the failing files call releases the mutex. Its precheck sees the existing failure, so its operation never runs.
2. If the save enters its operation first, it retains that same mutex across the operation and its error-to-trip mapping. W's file operation cannot enter, fail, and win a trip during that interval.

The described test therefore covers the preexisting-latch refusal, not an operation error whose `trip()` loses. A mutant that keeps the precheck's after-unlock settle but omits only the losing-operation settle survives this schedule. Test 5 is not a substitute: `SerialCore::undo_failed` performs a later waiting `tripped()` at `connect/serial.rs:1219`, which can mask the omitted guard barrier before its caller logs. Refused `save_block` has no corresponding later observation.

**Suggested fix:** add a separate save schedule. Pause V inside a files operation after its clean precheck; make W fail an engine write/barrier that does not require the files mutex and hold its report; then make V's files operation fail and lose the trip. Require V's live-pending checkpoint, verify its files mutex has been released while it waits, and verify `Ok(None)` follows the report. Add a mutant removing only this branch's settle. Named mutant (d), which removes both settles, can still die through the existing precheck case, so its death alone does not prove both branches.

### n-1 — A waiting registration is not guaranteed to return Fatal

**Severity:** nit; behavior-description accuracy, without a new unsafe outcome.

**Locations:** `PLAN.md:79-81`; `dashd-chainstate/src/flush/hub.rs:391-403`, `:404-432`, `:443-449`.

Concrete permitted sequence:

1. A registrant is asleep in the open-window condvar wait.
2. W reports, calls `reported()`, and then acquires the window first in `end_flush`.
3. W clears `window.open` and notifies.
4. The registrant reacquires the window. Its `while let Some(...)` predicate is false, so it skips `latch.check()` and proceeds to registration.

Moving the close after the report makes a Fatal possible while the window remains open; it does not eliminate the successful-after-close outcome. That outcome is already possible in the old ordering and is safe for the BL-554 report-ordering property.

**Suggested fix:** qualify the plan/D222 wording accordingly. If a deterministic Fatal is actually required, that requires an explicit check after the window loop and a corresponding behavioral test; the cleanup move alone does not provide it.

## Round-2 dispositions

“Closed” below means closed by the revised design, subject to implementation and its tests.

| R2 finding | Disposition | Evidence / remaining qualification |
| --- | --- | --- |
| M-1: unwind clears pending without a report; unscoped Ended | **Production panic cases closed; defensive-arm residue m-1** | The catch remains inside the live scope, reports the stable latched failure before clearing, and resumes the original unwind. `Ended` scopes its own winning panic report before taking the queue. InlineDriver uses the same catch. The owned-but-Ok branch's assertion order still needs the small correction above. |
| M-2: tip/window/pending cycle, plus direct window-held read | **Closed** | W reports and clears before acquiring the window in `end_flush`. S can finish its refusal and release the tip; R can then release the window; W can finish cleanup. A registrant waiting on the latch under the window likewise cannot block the report. Tests 9a/b target both forms. |
| M-3: AbortPath loses then reports immediately | **Closed** | Section 4 explicitly settles `AbortPath` before its sink and also classifies `read_error`, preserving its process-abort policy after settling. The connector evo-verification seam can exercise the former. |
| m-1: equivalent mutant f | **Closed** | Replacing the final waiting observation with `tripped_now()` is non-equivalent. Its test must independently reach the stop-path arm while W is pending, as specified by the stop-path variant, rather than arrive there through an already-settled failed-flush route. |
| m-2: timed helper waits indefinitely on pending | **Closed** | Both trip waiting and pending waiting share one deadline, including zero timeout; true means settled. Tests and mutant (k) directly address the regression. |
| m-3: stale waiter count permits early-clear mutant | **Closed** | Thread IDs sampled under the pending mutex identify the intended reader, and an empty snapshot after clear prevents stale waiters from certifying a live pending interval. Sampling after held-sink entry rejects early `reported()`. |

The inherited init teardown residue is also addressed: the parent owns unconditional release before spawning the constructor, which otherwise blocks in `Rig::with`.

## Liveness, unwinding, and cleanup checks

I traced the failing Files, B1, SubBatch, Bk, Bp, AfterBk, PruneApply and Unlink routes, the disk-space trip, `Halt::system`, observer marks and timer cleanup, and the driver report/cleanup paths. Apart from the explicitly planted T25 exception, the path after a winning trip returns from its guarded operation only formats/logs/marks, performs the duplicate trip, and reports. Moving the reporting error's `end_flush` out of `MachineFlush::step` removes the relevant non-report lock. `run_step` is the only non-test caller of `step`; direct machine tests must respect the changed cleanup responsibility.

The node sink was followed through `NodeAbort` (`dashd-node/src/abort.rs:61-88`), warnings, logger, UI and shutdown. The report-vector mutex is released before the core sink. UI's error route logs and writes stderr; it does not invoke the init-message hook. Shutdown's condvar waits release their mutex. The logger retains its mutex across callbacks and writes, so the callback restriction matters; the production registrations found print or capture, without a pending latch read. Tracing field formatting and the test capture formatting occur before their capture/logger locks. No reverse edge from those checked critical sections into a pending wait was found. This establishes absence of the identified internal lock cycle, not a bound on filesystem or console I/O latency.

I also checked `LogObserver`, recording/capture observers and the crash `WorldObserver`/`World.mark` route. The latter drops its observer state lock before recording into World, and the crash model is inline. The latch wake callback is V's bell: a nonblocking `try_send` and parker notification (`chain/flush.rs:280-284`), with no latch observation. Parker condvar waits release their mutex. Latch implementation must preserve publication-before-observation and avoid carrying the pending mutex into callbacks.

T25 deliberately retakes files while pending (`machine.rs:1154-1165`), so retaining raw reads under `GuardedFiles.inner` and settling only after unlocking remains justified. The general W-side proof does not subsume that planted exception. `PruneClears` does not introduce a pending-time wait on V: W parks only after `run_step` returns Waiting, and V's latch check is outside the request-slot critical section. `Ended` finishes its report before taking the driver queue, making the queue-held ended read safe.

The catch is sound for the proposed usage of `AssertUnwindSafe`: after a caught panic it reads stable latch/sink handles, completes the fallback, then resumes unwinding. It must not continue using the partially modified job or env. Once fallback reports and clears, `Ended` loses and adds no second report. Reporting `abort_text(latched)` preserves the original winning failure text; a panic before the two engine lines can omit those lines as documented. In InlineDriver the deferrer is V itself; another reader thread is needed to test the barrier, since the owner exemption would otherwise make the assertion vacuous. A panicking sink remains the explicitly allowed unreported escape. A second panic from a sink during `Ended` unwinding can terminate the process; this is not a new graceful-recovery guarantee.

Successful D35/D43 window boundaries and self-registration refusal remain intact. A reporting failure keeps the window open through reporting and then closes it; registration outcome is subject to n-1. Returning crash trips remain outside scopes, and production `_Exit` does not return. No new crash-path dependency was found.

## Trip callers and test/mutant assessment

I compared the section-4 table with trip callers, including guarded engine/files, `AbortPath`, `read_error`, `trip_latch`, flush control's abort/fatal route, machine space failure, `Ended`, both crash trips, `AttemptUnwind`, recovery, `LatchedEngine`, and crash-model stores/subscribers. No additional production losing-trip path that reports was found. The table's “22” count does not match the 16 non-test source call sites represented by its rows; this is a counting discrepancy, not evidence of an omitted reporter. The node's test-support trip is test-gated. Crash/model and no-deferrer clients do not acquire the new report-pending behavior accidentally.

The revised tests generally target the right production boundaries, with m-2's exception. Specific reachability/teardown details remain important:

- Tests 2/3 must hold release before constructing/spawning init, and identify V's waiter in both wake modes.
- Test 5 must use the promised real second-thread fault, not the old same-thread synthetic WLatched path.
- Test 7 must pause V after its earlier flush/latch checks and then trip W; otherwise an earlier waiting read can mask the missing `AbortPath` settle. The evo best-block verification seam is reachable with the existing connector test setup.
- Test 8c needs an independent reader while inline V owns pending. Tests 8a/b's held fallback/Ended reports otherwise reach the intended paths.
- Test 9a must acquire its window hold after gate setup, or use a Files failure; holding the window before a B1 gate can stop W before the intended fault for an unrelated reason.
- Test 9b must share the actual same latch; the old subscriber watchdog rig creates a separate one. Its rescue must break an actual dependency edge or use subprocess termination. Releasing only setup/sink gates cannot rescue the mutant's intrinsic S-pending/W-window/R-tip cycle, since W never reaches the sink.

These are static predictions, not mutant-run results:

| Mutant | Assessment |
| --- | --- |
| a: raw tripped | Required live-pending reader checkpoints in tests 1–3 should kill it. |
| b: no run_step scope | Tests 2/3 should fail the live-pending checkpoint. |
| c: early reported | Checkpoint after sink entry must be empty, killing it without relying on stale waiter scheduling. |
| d: no mutate_as settles | Test 4's precheck variant kills the named mutant; a loss-only mutant survives the described test. See m-2. |
| e: no trip_latch settle | Direct held-scope test 6 should kill it. |
| f: raw final AbortNode read | Non-equivalent; the explicit independent stop-path variant should kill it. Ordinary failed-flush propagation alone can mask it. |
| g: no AbortPath settle | Test 7 should kill it if V reaches the targeted abort before any other pending wait. |
| h: no panic fallback report | Test 8a fails its held fallback/report oracle. |
| i: unscoped Ended | Test 8b cannot establish live pending during the held panic report. |
| j: end_flush before sink | Test 9a cannot reach sink entry until rescue; 9b needs the effective rescue described above. |
| k: pending wait ignores deadline | Held-scope zero/finite-timeout tests kill it under their external watchdog. |

No named equivalent mutant remains. The missing loss-only guard mutant and debug owned-Ok case are the additional coverage recommended in the findings.

Checked inputs include the common reviewer contract, both prior reviews, the plan/audit, relevant program decisions (D35/D43 and CH3b abort/report policy), and the frozen source paths above. The synchronous report-before-observation comparison was also checked read-only against Dash Core v24.0.0-rc.4's validation/AbortNode paths. No implementation, build, runtime, or mutant result is claimed by this review.

**Final verdict: APPROVE-WITH-NITS — 0 major, 2 minor, 1 nit.**
