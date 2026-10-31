<!-- Copy of /work/scratch/ch3/reviews/BL-554-plan-r1-sol.md for reviews/ (DEC-984), 2026-10-31. -->

# BL-554 plan review, round 1 — Sol

Verdict: **REWORK**. Counts: **5 major (M), 2 minor (m), 0 nits (n)**.

Reviewed `/work/scratch/ch3-bl554/PLAN.md`, rev 0, against the frozen worktree
`/work/home/agent/workspace/rdc-wt/ch3-bl546` at
`b0ca84330b98b9d87e76a88ed6c9035fbb94505a`. Source paths below are relative to
that worktree. Read-only review; no builds or tests were run, as requested.

The barrier is useful for failures that reach `stop_serving` before becoming
observable. It does not establish the plan's stronger claim that all relevant
failure replies and caller lines pass through that point first. Two existing
paths violate that premise. The proposed barrier also needs a complete wake
protocol and consistent latch sampling.

## Major findings

### M-1 — A refused undo write logs the reporter's text before the proposed barrier

**Locations:** `PLAN.md:9`, `PLAN.md:44`;
`dashd-chainstate/src/connect/serial.rs:1218`;
`dashd-chainstate/src/chain/abc.rs:692`, `:954`.

Concrete sequence:

1. V is inside `ConnectBlock`, beyond its last flush-side latch check. W's
   chainstate write fails and trips the guard latch; hold W before its sink
   records the report.
2. V attempts its undo write. `GuardedFiles` refuses it.
   `undo_failed(FlatFileError::Failed)` returns
   `Fatal::reported(abort_text(latched))`, specifically W's report text.
3. `InHold::connect_tip` immediately executes its `inspect_err` and logs
   `ConnectTip: ConnectBlock <hash> failed, <W's text>` at `abc.rs:692`.
4. Only after this error unwinds out of `abc` does `activate_best_chain` call
   `abort_at_fault`, and therefore `stop_serving` and the proposed settle.

The later `ProcessNewBlock`/ChainLock lines can be fixed while this earlier
caller line still precedes W's `***` line. Core's corresponding `ConnectTip`
line is after `ConnectBlock` returns, following its synchronous abort report
(`validation.cpp:3437-3443`, rc.4).

**Change the plan:** explicitly order this diagnostic after the first report.
Either defer the diagnostic until the fault-stop barrier has completed, or
provide a report-settling operation usable before this log. If the latter runs
inside `InHold`, document the held tip lock and verify that W's remaining work
does not acquire it; the connector call must have released its coins/files
guards first. Add a held-W-sink test that forces the refused undo write and
captures this exact line, not just the outer ABC line.

### M-2 — A refused block save sends an explicit failure reply without first stopping V

**Locations:** `PLAN.md:8`, `PLAN.md:17`, `PLAN.md:44`;
`dashd-chainstate/src/store/blocks.rs:64`, `:178`;
`dashd-chainstate/src/chain/accept.rs:153`, `:161`, `:320`.

Concrete sequence:

1. V has begun processing a new block. W trips the latch during its flush and
   remains held before recording its report. Arrange the trip after V's prior
   service point and before the block-file mutation checks the latch.
2. `GuardedFiles::mutate_as` returns `FlatFileError::Failed` to the save.
   `save_failed` does nothing for that variant and returns `None`.
   `NodeBlockFiles::save_block` consequently returns **`Ok(None)`**, not `Err`.
3. `accept_block` returns a rejected `BlockResult` with Core's position-error
   state. It never reaches its later flush/carry-past call.
4. `process_block_accept` emits `ProcessNewBlock: AcceptBlock FAILED (...)`,
   calls `BlockChecked`, and explicitly sends the rejection reply. No
   `stop_serving` or proposed settle has run.

D55 does not protect this reply: `ReplySender::send` closes immediately
(`chain/reply.rs:285`); only unsent drops are parked. A later bell/service and
stop cannot retroactively order the line or reply.

**Change the plan:** add a latch-aware stop/settle before exposing a failed
save's result. Preserve the position-error `BlockState` and explicit rejection
reply required by Core; do not turn it into an ABC error or disconnected reply.
For example, have the failure branch service/detect the latched flush failure
and carry it past through the stop barrier before returning the existing
result. Add a held-sink test for this refusal, including `AcceptBlock FAILED`,
the actual reply, and the node flag. The plan's current ABC-only tests do not
exercise it.

### M-3 — The two latch reads allow `abort` to claim an unsettled failure was reported

**Location:** `PLAN.md:36` (proposed `NodeFlush::abort`), applied at
`dashd-chainstate/src/flush/site.rs:359`.

Concrete sequence:

1. V enters `stop_serving` for a failure of its own while W is working.
2. The proposed `if self.latch.tripped().is_some()` reads **false**, so V skips
   settle.
3. W fails, trips the latch, and is held before its abort sink records anything.
4. The separate `self.latch.check().is_err()` reads **true**. V now treats L-3
   as satisfied, can suppress an `UnlessLatched` report, and releases its stop
   tail/replies without having waited for W's report. An `Always` report can
   likewise be emitted before the W report that this latch result claims came
   first.

This is a reachable concurrent transition, not an issue solved by the queue
mutex: neither latch read is part of that mutex protocol.

**Change the plan:** use one latch observation to decide both settling and the
returned Boolean. Every return of `true` must follow settle for the observation
that justified it. A single `match`/`if` returning `false` on its open-latch
branch and `true` only after settling its latched branch suffices for this
particular race. Add a controlled interleaving or equivalent protocol test;
the current held-sink tests start with an already tripped latch and miss it.

### M-4 — Clearing `busy` on normal job completion needs its own wake

**Locations:** `PLAN.md:25`, `PLAN.md:29`;
`dashd-chainstate/src/flush/driver.rs:110`, `:531`, `:534`;
`dashd-chainstate/src/flush/machine.rs:799`, `:1114`.

The plan specifies waking on terminal events and thread end, but only clearing
`busy` under the lock for `Ran::Ended`. Normal job completion has no terminal
event in that arm, and W normally stays alive in `next_job`.

Concrete sequence:

1. W has sent `Released` and is finishing a normal job, for example its final
   `AfterBk` hook. Another store trips the latch and reports. V enters settle.
2. V observes `busy == true` and an empty job queue. It consumes any retained
   latch/release notification, rechecks the same predicate, and parks again.
3. W finishes the already-running successful step with `next_step() == None`.
   `run_step` checks that condition **before** checking the latch, so it returns
   `Ran::Ended`, without a `Failed` event.
4. The proposed code clears `busy` and breaks. W parks in `next_job`; it has not
   ended its thread. Without a completion wake, V remains parked forever even
   though its settle predicate is now true.

The same window exists after a final `Released` when the next run produces
`Ran::Ended`. Keeping the flag mutation under one lock prevents early return,
but does not supply this missing notification.

**Change the plan:** require every state transition that can satisfy settle's
predicate to wake its waiter after updating the queue, including the
event-free `Ran::Ended` transition. Wake `shared.v` and the configured bell
through the existing `wake_v` mechanism, or explicitly choose `shared.v` for
settle and notify it on every completion. Add a gated final-step/normal-end
test that consumes earlier permits before permitting the clear, plus a mutant
that removes this completion wake. Do not clear `busy` at `Released`: hooks
and automatic-prune unlink can still follow that event.

### M-5 — Skipping settle for every panic is wrong when W tripped first

**Locations:** `PLAN.md:37`;
`dashd-chainstate/src/flush/site.rs:361`;
`dashd-chainstate/src/flush/control.rs:437`;
`dashd-chainstate/src/chain/actor.rs:1352`, `:1581`, `:1603`.

The rationale that the panic report is V's own holds only when V wins the
latch. `trip_latch` returns `true` when a failure was already latched, and
`Fatal::new`/`Fatal::assert` then suppress V's report.

Concrete sequence:

1. W fails and trips the latch; its abort sink is held before recording.
2. V panics while executing a command, dropping that command's unsent reply
   into D55's parking scope. Alternatively, V returns a `Fatal::assert`.
3. V stops with `AbortCause::Panic`. `trip_latch` finds W's existing failure and
   returns `true`. The unchanged panic arm does not settle; V's sink stays
   silent.
4. `StopTail` sets the flag and `ReleaseParked` disconnects the reply while W's
   first report is still absent. The later driver drop/join happens too late
   to order that reply.

**Change the plan:** distinguish a panic that newly trips the latch from one
that finds a preexisting failure. Preserve the immediate V-first panic path,
but settle the preexisting W work before treating its report as complete and
releasing replies. Add a held-W-report test with V's panic/assert and a parked
reply; the existing tests that check only the shutdown flag are insufficient.
This does not require emitting ABC caller lines for asserts.

## Minor findings

### m-1 — The two-second negative assertion is not a timing-independent mutant oracle

**Location:** `PLAN.md:56`.

Sequence: W enters the held sink, but V is descheduled for the entire two-second
window. The negative receive assertion passes even with settle removed. The
test releases W, W records, and V finally runs and replies; the final sink
assertion passes too. Thus both proposed mutants can survive a legal schedule.
Waiting for W to enter the sink alone does not establish that V attempted its
stop or wait.

**Change the plan:** add an explicit V-side synchronization point for the
settle protocol and observe causal events, not elapsed silence. One option is
a test hook at settle's actual wait boundary and a reply-close observation on
the same event stream: the next event with W held must be the wait boundary,
not reply close; only then release W. A no-settle/early-idle mutant produces
the wrong event (or misses a required checkpoint). Retain deadlines as
watchdogs. Extend this to the early logs/refused-save cases above and exercise
both bell and direct-parker driver modes.

### m-2 — A failing held-sink assertion can strand test teardown

**Locations:** `PLAN.md:54`, `PLAN.md:57`;
`dashd-chainstate/src/chain/tests/support.rs:888`.

Sequence: a no-settle mutant returns the early reply; the planned timeout
assertion fails before the explicit gate release. `Rig::drop` joins V, whose
flush-driver drop joins W. W is still blocked on the held sink, so the
mutation run hangs instead of reporting the intended failed assertion. The
existing filesystem gate has a bounded wait; the proposed sink gate does not
specify one.

**Change the plan:** make gate release unconditional on every error/unwind
path, with a release guard that runs before any rig/join teardown, or capture
the observation, release/join, and only then assert. Use an entered handshake
and a watchdog as well. Apply the same discipline to the constructor thread
needed to observe init while `Rig::with` is awaiting its ABC reply.

## What I checked

- Read `REVIEW-COMMON.md` first, the BL-554 backlog row, and plan rev 0.
- Traced W's guard trip through machine failure, synchronous sink call,
  terminal event, queue pop, job completion, `Ended`, crash callback, and
  driver drop. The proposed atomic pop/`busy` set plus terminal-event/clear
  under the queue lock prevents the main popped-but-apparently-idle race.
  Queued-but-unpopped jobs must remain part of the settle predicate.
- Checked `PruneClears`' registered latch-aware wait, subscriber gate/ack
  `HubWait`, and W's queue registration. Once latched, those logical waits
  need neither a prune answer nor SB5 pumping to finish. An idle W need not
  exit for settle to return. A store operation or abort sink that never
  returns can still keep settle blocked; this is not a universally bounded
  wait. The proposed barrier moves that wait before flags/replies.
- Checked both parker implementations' retained permits, `set_bell`, latch
  callback registration, `wake_v`, and V's permit-restoration convention.
  Either proposed parker choice is workable with the complete completion
  notifications in M-4; do not re-notify inside each wait-loop iteration and
  create a self-waking spin.
- Kept `next_event`'s early latch return: D54's store-call scenario still
  requires leaving that event wait. The report barrier belongs at the
  relevant observation boundaries, not in every event wait. The current
  D54 tests observe return from the flush before the final actor stop; that
  distinction should remain explicit.
- Traced init's ABC error send, ProcessNewBlock's ABC tail, ChainLock's ABC
  line, InstantSend's asserted caller tail, carried-past failures, parked
  replies, explicit sends, `StopTail`, orderly stop, and `force_flush`.
  For ordinary ABC `AbortNode` errors, the explicit init reply and outer
  caller lines do follow `abort_at_fault`; settling there also orders
  `dashd-node/src/chain.rs:366`'s startup second abort. `force_flush` already
  joins the failed flush side before its caller gets the error.
- Checked W's panic report precedes publishing `ended`; with an active job
  kept busy, that is a valid barrier even without a `Failed` event. Checked
  that the crash callback runs before the crash latch/event; production
  `_Exit` does not return, while returning test callbacks must remain
  independent of V's stop. M-5 concerns V's separate panic path.
- Read CH3b D216/D220/D221, CH1-node D45/D54/D55/D60/D62, and CH2b-b4 D54,
  plus T3–T5, shutdown-flush, W-panic/crash and latch-wake test scaffolding.
  The two listed mutants are useful but omit the new completion wake,
  panic-first/second distinction, refusal paths and queued/idle wait states.
- Verified the synchronous Core order against **v24.0.0-rc.4**:
  `validation.cpp:2245-2249`, `:3122-3123`, `:3437-3443`, `:4843-4850`,
  `:4905-4908`; `node/blockstorage.cpp:949-951`;
  `chainlock/handler.cpp:299-305`; `shutdown.cpp:27-37`.

**Final verdict: REWORK.**
