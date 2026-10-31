<!-- Copy of /work/scratch/ch3/reviews/BL-554-plan-r2-sol.md for reviews/ (DEC-984), 2026-10-31. -->

# BL-554 plan re-review, round 2 — Sol

Verdict: **REWORK**. Findings: **3 major (M), 3 minor (m), 0 nits (n)**.

Reviewed `/work/scratch/ch3-bl554/PLAN.md`, rev 1, against the frozen worktree
`/work/home/agent/workspace/rdc-wt/ch3-bl546` at
`b0ca84330b98b9d87e76a88ed6c9035fbb94505a`. Source paths below are relative to
that worktree; `PLAN.md` means the scratch plan above. This was a read-only
plan/code review. No builds or tests were run, as requested. Only this report
was written.

Rev 1 addresses the five original major counterexamples and the teardown
finding at the design level. The causal-test finding is only partly closed.
Moving the observation barrier into the latch is useful, especially for the refused
save that never reaches `stop_serving`. However, scope destruction does not
prove a report happened, the proposed lock classification misses an indirect
deadlock, and the call-site audit omits an existing losing-trip reporter.
Those prevent approval.

## Round-1 disposition

These dispositions concern the specific round-1 sequences, with an ordinary
W step reaching its sink. They do not override the new findings below.

| R1 finding | Disposition | Reason |
| --- | --- | --- |
| M-1: refused undo logs before settling | **Closed in rev 1's design** | `mutate_as` drops the files lock and settles before returning `Failed` (`PLAN.md:47-50`). Thus `undo_failed` and `abc.rs:692` cannot expose the refusal ahead of the pending report. Test 5 targets the earlier `ConnectTip` line as well as the outer line. |
| M-2: refused save explicitly replies before settling | **Closed in rev 1's design** | The same guard barrier precedes `save_failed`, its `Ok(None)`, `AcceptBlock FAILED`, and the explicit rejection reply. Keeping the existing position-error result preserves Core's behavior. The standalone `NodeBlockFiles` test exercises the important boundary even if the additional actor test proves impractical. |
| M-3: two reads can return true for an unsettled failure | **Closed in rev 1's design** | The final/only `AbortNode` read is a waiting `check` (`PLAN.md:53`). Every true result therefore settles that observation. The claimed mutant for this finding is equivalent, however; see new m-1. |
| M-4: normal completion clears busy without waking | **Closed / obsolete** | Rev 1 removes that predicate entirely. Clearing pending and notifying its condvar under the scope cleanup protocol supplies the relevant completion notification; an idle worker need not end. |
| M-5: V panic loses the trip and releases parked replies early | **Closed in rev 1's design** | `trip_latch` settles on loss before returning true (`PLAN.md:54-55`). V's `StopTail` and `ReleaseParked` run afterwards. Test 6 directly exercises the losing-trip boundary; an actor-level panic test would additionally verify D55 integration. This does not address a panic on W itself, new M-1. |
| m-1: elapsed silence is not a causal oracle | **Not fully closed** | The required checkpoint fixes the case where the intended reader never ran, but a bare waiter count can remain positive after pending has cleared. Mutant (c) can then survive; see new m-3. Also isolate/identify the intended reader. |
| m-2: failed assertion strands held-sink teardown | **Closed in rev 1's design** | Unconditional release before joins, plus an entered handshake and watchdogs, fixes the failure path. Apply the same ownership/order to the init-constructor thread: `Rig::with` blocks at `chain/tests/support.rs:740`, so the parent must own release before waiting for construction or joining it. A guard created only after the constructor returns cannot protect that phase. |

## New major findings

### M-1 — Unwinding clears pending without completing a report; Ended's own trip is also unprotected

**Severity:** major.

**Locations:** `PLAN.md:27-28`, `:41-45`, `:56-57`;
`dashd-chainstate/src/flush/driver.rs:118-134`, `:345-356`;
`dashd-chainstate/src/flush/machine.rs:264-268`, `:508-535`.

There are two concrete failures of the proposed report-completion invariant.

**Panic after a winning store trip:**

1. W's guarded mutation fails and wins the scoped trip. Another thread is
   waiting on pending.
2. Before `run_step` reaches its sink, W panics. There are explicit callback
   boundaries here: `Halt::system` logs through the observer after the guard
   returned, and the failure path calls `obs.mark(StoreFailed)` and
   `obs.mark(Abort)` at `machine.rs:528/532`. An observer can panic at either.
3. The proposed `DeferScope::drop` clears pending and notifies during unwind.
   The waiting reader returns the failure as if it had been reported.
4. `Ended::drop` then tries to trip the latch, loses to the original store
   failure, and skips its sink (`driver.rs:354`). **No first abort report is
   made at all.** V can suppress its own report and release replies because
   its latch observation claims the original failure was reported.

The InlineDriver has the same premature cleanup when a step panics after its
winning guard trip. It has no `Ended` fallback; V's later panic stop loses to
the already-latched failure and can likewise suppress its report.

**Panic before a winning store trip:**

1. W panics in a step before any mutation trips the latch, for example the
   existing `PanicOnW` observer at `FlushBegin`
   (`flush/control/tests.rs:1027-1058`). The step scope unwinds without pending.
2. `Ended` wins its own unscoped panic trip, waking V, and then enters a held
   abort sink.
3. V's waiting reads see no pending and can return/log/release replies before
   that panic report. `PLAN.md:44` explicitly preserves this gap. The existing
   panic test checks the report only after joining W; it does not establish
   the new ordering invariant.

**Suggested change:** distinguish report completion from scope exit. Keep the
owning pending state live through a report or an unwind fallback, then clear
and notify. One approach is to catch a step panic *inside* the live scope,
complete the appropriate owned failure report before releasing pending, and
then resume unwinding. Account for whether a report already completed and
preserve the failure text/report policy. Protect `Ended`'s own winning
trip-plus-sink with the same protocol. Include InlineDriver in that design.
The exceptional crash/_Exit path should remain explicitly separate.

Add held-fallback-sink tests for a W panic both before its first trip and after
a guard wins but before the normal sink, and check actual V observations.
The proposed unit test that a panicking scope merely clears pending would
pass the broken design. Also specify what happens if a scoped trip is owned
but the step returns `Ok`/`Refused`: silently clearing cannot certify a report.
The existing same-thread `TripOn` test (`flush/control/tests.rs:978-1024`)
simulates an external trip without reporting; it will need to model the real
external reporter rather than accidentally owning a deferred trip on W/V.

### M-2 — GuardedEngine's waiting precheck can close a tip/window/pending deadlock

**Severity:** major.

**Locations:** `PLAN.md:51`, `:59-69`;
`dashd-chainstate/src/store/guarded.rs:267`;
`dashd-chainstate/src/subscriber/driver.rs:617-621`, `:1075-1088`, `:1117`;
`dash-chain/src/feed/table.rs:419-439`, `:623-628`, `:648`, `:662-663`;
`dash-chain/src/feed.rs:673-689`;
`dashd-chainstate/src/flush/hub.rs:391`, `:424`, `:443-449`;
`dashd-chainstate/src/flush/machine.rs:508-511`.

The precheck holds no lock acquired *inside* `mutate`, but its caller can hold
the tip lock. Checking only whether W itself takes that lock is insufficient.
This concrete cycle uses the documented node-wide shared latch:

1. A prior successful flush established `T_ack`. A syncing subscriber S has
   reached the current tip with a locator below its eligible capped target.
   S enters `handoff`; `subscribe_at` calls its final `capped_commit` **under
   the tip lock**. Pause S before its guarded WAL sync (for example while its
   index files sync).
2. A registration R enters `register_outside_flush` while the hub window is
   closed. R holds the hub's **window mutex** and calls table registration at
   `hub.rs:424`.
3. R cannot acquire S's tip mutex. Contrary to the nearby "never blocks"
   comment, another thread's handoff is not a held-tip refusal:
   `held_owner == 0`, `finish_owner != R`, so `acquire_tip` yields/retries at
   `feed.rs:685` while R retains the window mutex.
4. W's new flush is in its Files step, before B1 opens the window. Its file
   barrier fails and wins the scoped trip.
5. S reaches `self.engine.sync_wal()` at `subscriber/driver.rs:1117`. The
   proposed waiting guard precheck now waits for W's report **while S still
   owns the tip mutex**.
6. Before returning the failure to `run_step` and its sink, W executes
   `hub.end_flush` at `machine.rs:511`. This takes the window mutex even when
   no window is open, so W waits for R.

The cycle is **S waits for pending W; W waits for R's window mutex; R waits
for S's tip mutex**. No sink is reached and no scope ends. This is an indefinite
pending state through deadlock, rather than a missing RAII clear. Before the
new waiting precheck, S could return the refusal and release the tip lock.

There is also a direct audit item: `hub.rs:393` currently calls `check` while
holding that same window mutex when the window is open. W's failure cleanup
needs it. Rev 1's promised audit can fix that direct reader, but doing so alone
does not fix the three-thread cycle above.

**Suggested change:** audit the transitive wait graph and caller-held locks,
including `dash-chain`'s tip/handoff code, not just the files containing latch
reads. Explicitly resolve the handoff boundary: propagate a nonwaiting failure
out of the tip critical section and settle only after releasing it, or change
the cleanup/registration/report order to break the cycle. Do not approve the
unqualified statement that `GuardedEngine::mutate` can wait because it holds
no lock locally.

The raw-read/settle rule is sufficient only when **all** locks involved in
such dependency chains have been released at the settle point. Add a
deterministic shared-latch subscriber/handoff/registration regression. The
existing watchdog rig creates a separate subscriber latch at
`subscriber/watchdog_tests.rs:462-468`; that setup will not catch this cycle.
Give a deliberately deadlocking mutant a rescue/subprocess watchdog so its
joins cannot strand the test runner.

### M-3 — The read-site audit misses AbortPath's losing trip followed immediately by a report

**Severity:** major.

**Locations:** `PLAN.md:32-36`, `:46-55`, `:65-69`;
`dashd-chainstate/src/chain/seams.rs:254-263`;
`dashd-chainstate/src/connect/serial.rs:507-509`, `:720-725`;
`dashd-chainstate/src/chain/abc.rs:709-711`.

The new rule explicitly requires a losing `trip()` caller to settle before
reporting. The implementation list does not change `AbortPath`, and the
promised audit searches only `tripped()`/`check()` sites. `AbortPath` has
neither: it ignores the trip result and always calls its sink immediately.

Concrete sequence:

1. V is already inside `ConnectBlock`, beyond its last flush-side latch check.
   W fails, wins its scoped trip, and is held before recording its report.
2. V reaches an independent connector abort, for example the failed evo
   best-block verification at `serial.rs:720-725`.
3. `SerialCore::abort_node` calls `AbortPath::abort_node`. Its `trip()` loses to
   W, but that Boolean is ignored. V immediately invokes the node sink at
   `seams.rs:263`: an observable abort report and shutdown request before W's
   first report, despite having learned of W's pending failure.
4. The connector returns `SystemError`; `ConnectTip` can also emit its caller
   line at `abc.rs:710` before any later waiting stop check.

GuardedFiles' refusal handling cannot protect this non-store connector path.

**Suggested change:** extend the audit to every `trip` caller and abort/report
boundary, not only waiting-read sites. Make this losing-trip path settle at a
verified safe lock boundary before its sink and caller diagnostics. Preserve
mandatory additional reports; the fix must order them, not suppress them.
Add a held-W-sink connector-abort test that takes this path. Separately classify
the read-error catchers, whose production `read_failed` intentionally aborts
the process; do not blindly turn those into a graceful waiting stop.

## New minor findings

### m-1 — Mutant (f) is equivalent under the revised latch contract

**Severity:** minor.

**Locations:** `PLAN.md:99-100`, `:115`;
`dashd-chainstate/src/flush/site.rs:359-363`.

Sequence: the first read sees open and skips a preliminary settle; W trips;
the second read, specified to be waiting, sees pending and waits. Its true
result is therefore safe. If the first read already sees a trip, the final
waiting read is still safe. If both see open, returning false is also a valid
single-read outcome. The latch never untrips.

Thus restoring two reads **with a waiting final read** does not restore r1
M-3. No behavioral test of the BL-554 ordering property should be required to
kill it; treating survival as failure can reject correct implementations.

**Suggested change:** mark (f) equivalent/remove it, or replace it with an
actually nonwaiting final observation (`tripped_now`) and a controlled trip
between the preliminary open observation and that final observation.

### m-2 — wait_tripped(timeout) inherits an unbounded pending wait

**Severity:** minor.

**Locations:** `PLAN.md:29-38`, `:80-82`;
`dash-kv/src/latch.rs:183-197` (especially `:189`).

Sequence: A wins a scoped trip and remains held in its sink. B calls
`wait_tripped(Duration::ZERO)` or a finite timeout. Its first `tripped()` now
blocks on pending before the helper reaches its deadline check. B can remain
blocked arbitrarily past its timeout. The existing public contract says it
parks until failure or timeout, and the plan does not revise that contract or
the helper. Sink/log I/O is not universally bounded.

No non-test caller was found in the frozen tree, so this is a public API
correctness regression rather than a demonstrated rdashd production hang.
It occurs only with an opted-in producer; clients with no deferred scopes do
not gain this wait.

**Suggested change:** specify `wait_tripped` explicitly. Prefer a
deadline-aware pending wait if true is to mean a settled failure. Alternatively
retain a raw trip notification with clearly documented mandatory settling
before treating it as reported. Merely routing through unbounded `tripped`
does not preserve the timeout. Add held-scope zero/finite-timeout tests.

### m-3 — A stale waiter count can let the early-scope-drop mutant survive

**Severity:** minor; residual r1 m-1.

**Locations:** `PLAN.md:29-30`, `:75-79`, `:97-98`.

Counting a reader on entry to the condvar wait and decrementing when that
reader leaves does not prove pending is still live when the test observes
the count. A notify does not require the notified thread to run immediately.

Concrete legal schedule for mutant (c):

1. W trips inside its scope. V enters the pending wait; the counter is one.
2. The mutant drops the scope before the sink. Pending is cleared and V is
   notified, but V has not resumed/reacquired the mutex and decremented its
   count.
3. W enters the held sink and sends the entered handshake. The test reads
   `pending_waiters() == 1`. V has not run, so the negative reply/log
   assertions pass.
4. The test releases W. W completes the report. Only then does V resume and
   expose the result, so the post-release report-order assertions also pass.

The forbidden early release of pending happened, yet every planned assertion
can pass. This recreates r1 m-1's scheduling false negative without an elapsed
silence assertion.

**Suggested change:** make the checkpoint a synchronized snapshot that
requires both a live pending owner and the intended reader waiting on that
same predicate/generation. For example, `pending_waiters()` can return zero
unless pending is still active, with the predicate and count sampled under
the pending mutex. Take that checkpoint after the held-sink entered
handshake. Specify this explicitly; a bare atomic count is insufficient.
The release guard must still run on the resulting missing-checkpoint failure.

## Protocol, liveness, and test checks

- **Publication:** setting pending before publishing the first failure can
  close the original observation window. All trip contenders must participate
  in the same winner protocol: a loser must not replace or clear the winner's
  pending owner. A reader must acquire the published failure before relying
  on the pending fast-path state; clearing pending must publish completed
  report effects. These are implementation requirements, not findings
  against an implementation that does not yet exist.
- **Raw observations and wakes:** `trip() == false` and `tripped_now()` can
  intentionally observe W's failure before its report; they are safe only
  with the proposed subsequent settle before observable use. New M-3 shows
  why auditing those users matters. Existing registered callbacks may run at
  the trip, before the report, but their contract already prohibits blocking
  or touching the latch. `FlushBell::ring` uses `try_send` and notification
  (`chain/flush.rs:280-283`); it does not wait on V. Notify pending waiters
  using the predicate/condvar protocol, without retaining the predicate mutex
  across the wait or callbacks.
- **Lifecycle:** ordinary successful, waiting, refused, and failed returns
  can all drop the scope and clear owned pending. Unwind also clears it as
  proposed, but that is exactly the false-completion bug in M-1. I found no
  normal production mutation-error path that intentionally swallows its
  winning guard failure and returns success; the generic scope still needs
  an explicit owned-trip/no-report policy. A thread-affine scope must not be
  movable to another thread, and its latch identity must remain alive through
  TLS cleanup. Otherwise TLS/owner cleanup can itself strand pending.
- **Crash:** the production `_Exit` callback precedes the unscoped crash trip
  (`driver.rs:535-540`), and never returns. Returning crash seams intentionally
  have no abort report. Neither driver should create pending for that
  synthetic post-exit notification. A panic in the callback instead takes
  the `Ended` issue in M-1. W trips outside `run_step` need explicit
  classification; the panic reporter is not covered merely by scoping steps.
- **InlineDriver:** ordinary failure reporting remains synchronous on V;
  owner exemption prevents a self-wait. Another thread observing that
  InlineDriver's scoped trip still must wait. Owner exemption is not a
  substitute for a report on an unwinding/no-report return.
- **Files and engine internals:** the files mutex is a real W dependency, so
  switching the under-lock refusal observation to raw and dropping the mutex
  before settle is necessary. Engine mutation calls have returned before
  their outer guard trips; for example MemoryEngine's state write guard ends
  with the operation (`dash-kv/src/memory.rs:151-163`). This does not eliminate
  caller-held locks, as the subscriber cycle demonstrates. No additional
  concrete engine-internal mutex cycle was identified in the reviewed paths.
- **Coins, hub, queue, logging:** V's flush adapter holds the coins cell at
  `flush/site.rs:171`, but W uses frozen state and its failure report does not
  acquire that V cell. The hub window and tip locks are the concrete liveness
  problem above. `next_event`'s usual check at `driver.rs:471` is outside the
  queue lock; its ended branch at `:467` is inside, so the plan's blanket
  "already outside" description is not literal. In the proposed single-W
  normal protocol, ended is published after its step/report scope, so I did
  not identify a separate cycle there. W reports queue events after
  `run_step`. The node sink takes warning, logger, UI, and shutdown state;
  Logger calls callbacks while holding its mutex (`logging/mod.rs:274-317`).
  Test capture holds its mutex only while pushing a formatted record
  (`chain/tests/support.rs:341-353`). Keep all pending waits/checkpoints out
  of logger/capture/sink critical sections; the audit must include indirect
  callback calls. I found no existing concrete logger callback that waits
  on this latch.
- **No opted-in producer / SPV:** with pending never set, waiting reads have
  no deferred predicate to wait on; their functional result, wake behavior,
  and first-failure identity can stay unchanged. `LatchedEngine`'s refusal
  path therefore retains its behavior for ordinary SPV clients. Preserve
  the callback contract and release internal publication locks before
  callbacks. The timeout issue above concerns a latch that does have an
  opted-in producer, not an unavoidable change to every SPV client.
- **Causal checkpoints:** `pending_waiters()` works when counted at the
  actual wait predicate, decremented on every exit, and tied to the intended
  reader **and a still-live pending owner** (m-3). An unrelated waiter on the
  same latch must not satisfy the checkpoint for a reader that escaped.
  The standalone save and `trip_latch`
  tests can isolate this cleanly. Keep the sink held until the checkpoint
  assertion is evaluated, and make a missing checkpoint fail under a
  watchdog. A pre-report gate must precede forwarding/recording the report.
  The node sink's own report vector is appended before its actual Core sink
  call (`dashd-node/src/abort.rs:81-82`), so that vector alone is not proof
  that the `***` line and shutdown effects completed.
- **Release guards:** reverse declaration/drop order is suitable for an
  already-built rig, provided release precedes every explicit/scoped join
  as well as `Rig::drop`. Init needs a parent-owned guard before its blocked
  constructor/join. The M-2 deadlock occurs before sink entry; a sink-release
  guard by itself cannot rescue that different failure mode.

### Mutant assessment

This is source/protocol analysis, not a claim to have run the mutants.

| Mutant | Expected result from the planned tests |
| --- | --- |
| (a) `tripped` does not wait | **Dies:** the latch reader unit test cannot reach its required pending-wait checkpoint; actor tests also detect an early observation or missing checkpoint. |
| (b) no `run_step` scope | **Dies:** held-W init/PNB tests have no pending reader and cannot satisfy the required checkpoint. |
| (c) scope ends before sink | **Can survive as specified:** a notified reader can still contribute a stale positive count when W enters the held sink; see m-3. A live-pending snapshot makes this mutant fail. |
| (d) no `mutate_as` refusal settle | **Dies in test 4:** `NodeBlockFiles::save_block` returns `Ok(None)` without waiting. The undo test alone is insufficient because `undo_failed` has a later waiting `tripped` read. |
| (e) no losing `trip_latch` settle | **Dies in test 6:** the direct call returns true without reaching the checkpoint. |
| (f) two reads, final read waiting | **Equivalent:** see m-1. Replace it. |

Missing substantive cases are producer panic before/after the trip,
InlineDriver unwind, the indirect shared-latch deadlock, the losing
`AbortPath` report, and the timeout contract. Cover both refusal branches
(preexisting failure and an operation error whose trip loses), a competing
loser that must not disturb pending, and zero pending waiters after release.
The planned bell/direct-parker variants are useful; the pending condvar does
not depend on a later queue event to wake its readers.

## What I checked

- Read `node-program/briefs/REVIEW-COMMON.md` first; read the round-1 report,
  rev-1 plan/dispositions, relevant program context/D rows, and repository
  instructions. Confirmed the frozen HEAD above.
- Traced `GlobalFailStop`, its registered wake protocol and `wait_tripped`;
  the node guarded engine/files and SPV `LatchedEngine`; W's `run_step`,
  failure cleanup, `Halt`, `w_loop`, `Ended`, crash and inline drivers;
  queue/event waits, `PruneClears`, hub gate/ack waits, and the node sink.
- Traced V's early undo diagnostic, save rejection, single-read abort,
  losing-panic stop, parked-reply release, and connector `AbortPath` report.
  Checked inherited tip/coins locks and subscriber final commits through
  `dash-chain` handoff/registration, including the actual try-lock retry
  behavior rather than relying on its "never blocks" comment.
- Read held-fault/observer/panic and init/PNB test scaffolding, constructor
  and rig teardown, subscriber watchdog latch setup, and trace/logger capture
  lock lifetimes. Assessed each mutant without running builds or tests.
- Verified the relevant synchronous ordering against **Dash Core
  v24.0.0-rc.4**: `validation.cpp:2245-2248` (sink before state error),
  `:3122-3123` (flush catch), `:3437-3443` (ConnectBlock, BlockChecked,
  ConnectTip log), `:4843-4846` (null save position), `:4896-4899`
  (AcceptBlock failure), `:4905-4908` (ABC caller),
  `node/blockstorage.cpp:949-951` (startup's additional abort),
  `chainlock/handler.cpp:299-305`, and `shutdown.cpp:27-37`.

The proposed normal-path ordering and preserved save state are compatible
with those Core paths. No acceptance-rule change is proposed. The remaining
parity/reliability concerns are the missing/early reports and deadlock above;
preserve mandatory additional Core reports while fixing them.

**Final verdict: REWORK.**
