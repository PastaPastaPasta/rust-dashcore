# BL-547 round 3 — Sol narrow re-review

**Verdict: ACCEPT. Counts: 0 major, 0 minor, 0 nits.**

Round-2 **M-1 is closed** for the production W-driver disposal path identified in DEC-953. Reviewed `ba31b5c673f9240d6ba37c57ea8e69cef7ed45b2..107b4f26b15e5844be83c01302938f6d08c5fa9d` on `node/bl547`: four files, +32/−8. All four commits report good signatures. This review accepts this delta only; it does not reopen the earlier review or certify the later follow-merge.

This was a source review. I did not build, run tests or mutants, modify the frozen worktree, or write into `/work/scratch/bl547`. I read the supplied mutation script and round-2 log. The only write is this uncommitted review.

## Findings

- M-n: none.
- m-n: none.
- n-n: none.

## Closure of round-2 M-1

At `dashd-chainstate/src/chain/actor.rs:1497`, `open.then_some(self.flush)` now completes before the disposition send at `:1504`. `then_some` takes its argument eagerly. When `open` is false, the boxed flush side is destroyed in that expression; its destruction is not deferred to V's remaining-field cleanup.

The original counterexample now has the required outcome:

1. W panics during its gated space check while V waits for the ALWAYS flush. W's `Ended::drop` trips/reports the latch and wakes V (`flush/driver.rs:345-363`).
2. V returns the reported storage failure from `serve`. Its abort stop observes the latch, selects `open == false`, and leaves the initially captured `Serve` unpublished (`chain/actor.rs:1488-1492`).
3. The expression at `:1497` destroys `NodeFlush`, including `FlushControl<ThreadDriver<_>>`. The real driver joins W and raises `"W's flush thread panicked"` on V (`flush/driver.rs:578-600`).
4. V unwinds before `:1504`. Its disposition sender disconnects without a buffered value. The reaper consequently selects `AfterStop::Drop` at `chain/actor.rs:994`; initialization disconnects without a DMN move or listener call.

For `open == true`, the flush box instead moves into the local `Some`, then into `VExit.flush` at `:1510`. It is not destroyed on V's normal return. The `VExit` literal at `:1505-1516` consists of moves and a scalar copy: it calls no fallible constructor, clone, flush, or join. Moving a kept flush side does not run its destructor. Its later shutdown flushes/destruction remain later lifecycle operations.

I also checked the cleanup after this literal, rather than assuming the send is literally V's last executed operation. `V` has no custom destructor. The remaining fields include the checks, files, connector, mempool/InstantSend seams, UI/shutdown references, read-ahead, lanes, deferred/held commands and caches; `ParkDrops` also has drop glue. The concrete node implementations inspected contain no second W-style panic-propagating join there. `ReadAhead::stop` has already consumed its source/running thread at `actor.rs:1445`; its later destructor repeats an empty stop (`chain/read_ahead.rs:445-461`). The returned state retains the signals, reader, clock, loading flags and abort sink. Abort-path held/deferred commands and parked replies are cleared/released by `stop_serving` before publication (`actor.rs:1698-1726`).

This is not a proof that arbitrary extension code cannot unwind after the send: a caller-supplied seam destructor or reply waker can panic during remaining cleanup (`chain/reply.rs:80-85`, `:293-319`), and orderly shutdown can leave held/deferred commands to drop. Thus the literal answer to “can anything still panic?” is that such user code remains possible. I found no concrete rdashd production sequence through those remnants that reproduces M-1, and do not count an invented panicking destructor/waker as a new correctness finding in this narrow round. The concrete, explicit production propagator raised in round 2 now runs before publication.

## Preserved ordering

- **D220 lane handover:** unchanged. `stop_serving` still gives the lanes to the reaper through `StopTail`, before the final flush stop/drop. The reaper can still serve `SetLoading` early; only initialization waits for the disposition (`actor.rs:987-1001`, `:1028-1033`).
- **Final tip:** unchanged. `self.tip()` is captured after `serve` returns (`actor.rs:1443-1465`), including `run_carried_tails` (`:1663-1689`). The reorder adds no chain operation between capture and publication.
- **Flush order:** the existing stop still services/waits for earlier work (`actor.rs:1522-1526`; `flush/site.rs:347-349`; `flush/control.rs:423-425`). An unkept driver now finishes disposal before initialization. A kept driver remains available for the shutdown flushes. `after_import` still clears importing and waits for initialization (`dashd-node/src/chain.rs:259-263`); `Chain::stop` joins loadblk before joining V (`:396-401`), and `ChainSteps::shutdown` obtains that exit before `force_flush(First)` (`dashd-node/src/steps.rs:548-558`). No shutdown force-flush moves ahead of initialization.

The added disposal ordering creates no new reaper dependency in the driver join: unread prune requests disconnect via the flush subsystem's own one-shot transport (`flush/clears.rs:43-48`), not V's parked actor replies or an initialization callback.

## Tests and mutation evidence

The W-panic extension at `chain/tests/node_stores.rs:1504-1520` reaches the production adapter and destructor. `Stores::config_with` constructs the real `ThreadDriver::spawn`, `NodeFlushParts` and `NodeFlush` (`:421-454`); `node`/`node_with` retain those in the rig (`:835-845`). The fault seam is the filesystem space-check panic (`:215-217`). `always_flush_held` calls production `V::flush_now(Always)` and establishes that V is waiting before releasing W (`:1448-1472`). There is no substituted driver-drop implementation.

The fixture registers `InitCalls` while keeping the rig, signals and signals thread alive, verifies V's exact driver-drop panic at its joiner, then calls the shared helper. That helper sends `SetLoading(false)` followed by initialization and requires **a disconnected reply, unchanged DMN-move count, and zero listener calls** (`tests/flush.rs:1338-1351`). It distinguishes disconnection from timeout. With the old placement, a buffered `Serve` is consumed even after sender disconnection, the surviving signals upgrade succeeds, and the reply becomes `Ok(())`. This is a deterministic M16 kill after V has been joined, not a race against V's exit.

The returned-error fixture at `tests/flush.rs:1196-1226` deliberately enters the other path. Its stop-start handshake occurs before `fail_job`, so `serve` has ended orderly and V is waiting in `stop_flush_at_exit(false)`. The normally returned failure reaches the `None`/`Err(f)` arm at `actor.rs:1474`. The new initialization assertions require success, exactly one DMN move to genesis, and exactly one listener call. M15's change of that arm to `Drop` therefore fails. This fixture uses `ModelW` to control the returned error, but exercises the real V exit and reaper; it does not claim production destructor coverage.

The supplied `mut-run-r2.log` reports a passing baseline of 39 chainstate tests and one node test, all 16 mutants killed, and these relevant failures:

| Mutant | Source-based assessment and reported failing tests |
| --- | --- |
| M15: orderly-stop returned error selects `Drop` | Killed by `a_job_failing_while_vs_stop_waits_takes_the_fatal_path`, as above. |
| M16: send before the unkept flush drops | Killed by `a_panic_on_w_while_v_waits_for_the_release_reaches_vs_joiner`, as above. The script adds the early send while retaining the final send; a buffered early value is sufficient to reproduce the bug. |
| M11: send before the flush stop | Killed by both synchronous flush-stop panic tests and the production W-panic test. Their post-join initialization checks observe the incorrectly buffered `Serve`. |

These are inspected claims, not independent test executions.

**Cheap surviving mutant, inferred rather than run:** select `Drop` only when V entered the `Some(f)` branch at `actor.rs:1479` with an ordinary non-store `AbortNode` and `stop_flush_at_exit(true)` then returns an ordinary error. Keep the orderly `None`/`Err(f)` arm unchanged. The new positive test covers that orderly arm; the abort-stop panic fixture unwinds; the production W-panic fixture must already drop init. None requires successful initialization after this distinct normally returned abort-stop error. A controlled sequence would be non-store abort → stop-start handshake → `ModelW::fail_job` → join V → initialization. It should still succeed under the current correct implementation; the mutant would disconnect it. This is a coverage observation, not an implementation finding or a condition on accepting the requested M-1 fix.

I also checked the D72 sentence against the implementation: it now expressly includes W's deferred driver-drop panic and distinguishes it from a normally returned stop error. The four-file delta passes `git diff --check`; the frozen worktree remains clean.
