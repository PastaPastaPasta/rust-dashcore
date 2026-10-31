# BL-547 round 2 — Sol review

**Verdict: CHANGES-REQUIRED. Counts: 1 major, 0 minor, 0 nits.**

Reviewed `2ece6d4ad..ba31b5c673f9240d6ba37c57ea8e69cef7ed45b2` in the frozen `node/bl547` worktree, both round-1 reviews, DEC-944, the supplied mutation script/results, and Dash Core `v24.0.0-rc.4`. Both new commits report good signatures. This was a source review: I did not build, execute tests or mutants, modify the frozen worktree, or write into `/work/scratch/bl547`. The sole write is this review; it is not committed.

## Findings

### M-1 — `Serve` still survives the production W panic propagated when the rejected flush driver drops

**Location:** `dashd-chainstate/src/chain/actor.rs:1498`, followed by `:1501`; production panic at `dashd-chainstate/src/flush/driver.rs:595-599`. An existing fixture already establishes this unwind: `dashd-chainstate/src/chain/tests/node_stores.rs:1501-1514`.

Moving the disposition after `stop_flush_at_exit` fixes the two synchronous stop-panic fixtures. It does not put the disposition after all of the flush side's panic-producing exit work. When `open` is false, `open.then_some(self.flush)` eagerly takes and drops the flush side. Its production `ThreadDriver::drop` joins W and explicitly panics on V's thread if W panicked and V is not already unwinding. The new `Serve` send is still **before** that drop. The adjacent comment at `actor.rs:1499` explicitly describes this propagation.

Concrete sequence, extending the existing production-adapter fixture without inventing a new failure mechanism:

1. Use `a_panic_on_w_while_v_waits_for_the_release_reaches_vs_joiner` (`node_stores.rs:1501`), registering an initialization-counting listener and keeping the rig/signals alive.
2. `always_flush_held` makes W panic in its gated space check while V waits for the ALWAYS flush. W's `Ended::drop` trips the latch and reports `"the chainstate flush thread panicked"` (`flush/driver.rs:345-355`). V's wait returns a store error; `NodeFlush` maps it to `Fatal::reported`, whose class is `AbortNode` (`flush/site.rs:210-212`, `chain/seams.rs:95-96`).
3. V leaves `serve` with that fatal. `run` selects `AfterStop::Serve` at `actor.rs:1460-1465`. `stop_serving` hands over the lanes and sees the already-tripped latch, so `open` is false and the stop call short-circuits (`:1488-1491`).
4. V sends `Serve` at `:1498`. At `:1501`, dropping the rejected `NodeFlush` drops its driver; joining the panicked W raises `"W's flush thread panicked"` on V. The existing fixture already asserts that exact panic payload at V's joiner.
5. After joining the panicked V, send `SetLoading(false)` and `InitializeCurrentBlockTip` through the surviving handle. The reaper consumes the buffered `Serve`, moves the DMN tip, runs the listener and replies successfully. Sender disconnection cannot replace a value already buffered in the disposition channel (`actor.rs:994-1001`).

This is the same observable violation as round-1 M-1: initialization is allowed after V has unwound, despite D72's panic/drop rule. It requires no race. It also applies if W panics while an orderly or non-store-abort stop is waiting: the stop can return the latched error normally, then driver disposal raises the panic **after** publication. A normally returned I/O failure and this deferred panic are distinguishable; accepting the former does not justify accepting the latter.

**Requested change:** dispose of a flush side that will not be retained before publishing the disposition. In particular, move the `open.then_some(self.flush)` operation before the send, preserving early lane handover and the tip captured after `serve`. Extend the existing `node_stores` W-panic fixture with the same three post-join assertions as the two new stop-panic tests: disconnected initialization reply, unchanged DMN-move count, and no listener call. Keep ordinary returned-error initialization covered separately so that fixing this does not suppress Core's abort-only behavior.

This remains a major against the requested D72 panic-stop guarantee, as in round 1. I am not claiming a new disk-state or consensus consequence; the observed wrong effects are initialization dispatch and in-memory state changes after a V panic.

## What I checked

### Exit paths and ordering

- **Orderly stop:** `serve` ends, the non-public tip is captured, stop service/wait completes, lanes are handed over, then the disposition is sent. A panic in `stop_flush_at_exit(false)` now leaves it unsent. The new test at `tests/flush.rs:1371` joins the panicked V before submitting initialization and checks all three required outcomes.
- **Abort:** lanes still move at `stop_serving`, before the final stop wait. The reaper can clear loading flags while V finishes; initialization waits on the disposition. A panic inside `stop_flush_at_exit(true)` now leaves it unsent. `tests/flush.rs:1343` checks the post-join outcome.
- **Abort followed by carried work:** `serve` still runs `run_carried_tails` before returning (`actor.rs:1660-1687`). The captured tip therefore includes carried work. A returned assert or contract failure from a tail selects `Drop`; a Rust unwind before the send disconnects the disposition sender. The new IBD test exercises a non-public-tip advance after the early abort/handover, although it uses `TestOnV` to create the advance rather than scheduling a full carried ABC tail.
- **Other unwinds before the send:** read-ahead stop, tip capture, stop reporting, `StopTail`/parked-reply release and flush stop all precede the send. If any unwinds, no `Serve` has been published. M-1 identifies the explicit production unwind that remains immediately **after** it.
- **Contract failure:** a fatal returned from `serve` with `is_contract()` now selects `Drop` at `actor.rs:1461`. The new test constructs the real `Fatal::from(ChainError::Internal(..))` classification and checks disconnection, zero additional moves and zero calls (`tests/dmn.rs:710-726`). It tests the classification/exit path, rather than inducing a partially applied SB4 operation.

The added wait does not put initialization behind the node's shutdown force-flushes: `Chain::stop` joins loadblk before V (`dashd-node/src/chain.rs:396-401`), and `ChainSteps::shutdown` calls it before `force_flush(First)` (`steps.rs:548-558`). Loadblk waits for initialization in `after_import`, so that callback/reply still precedes those flushes.

I found no new wait cycle from delaying the disposition. The reaper blocks there only for initialization. V's flush stop services W and pumps pending fanout; W's `PruneClears` are answered through `FlushHost`, without the reaper or loadblk. The real `SyncListener` reads the shared index/view and updates sync state; it does not issue a V command. Existing possible indefinite waits on storage, subscribers or a blocking listener are not newly introduced by the send movement.

### Item 6: ordinary returned flush errors against Core rc.4

**Serving initialization after an ordinary returned storage error is correct.** Core's `AbortNode` requests shutdown and returns false (`src/shutdown.cpp:27-37`); it does not terminate the process. `FlushStateToDisk` returns it for low disk space, block-index/coins/EvoDB write failures, and caught `std::runtime_error` (`src/validation.cpp:3055-3057`, `:3071-3072`, `:3094-3104`, `:3122-3123`).

Inside startup ABC, the connect/disconnect IF_NEEDED flushes propagate false (`validation.cpp:3310-3311`, `:3463-3464`), as does ABC's concluding PERIODIC flush (`:3857-3858`). `ThreadImport` reports its second abort and returns (`src/node/blockstorage.cpp:947-951`); `CImportingNow` clears importing on scope exit. The loadblk lambda then unconditionally reads the active tip/IBD and calls initialization (`src/init.cpp:2458-2473`). There is no intervening shutdown/error check suppressing that call.

The flush correspondence matters:

- `V::stop_flush` calls `service` and `stop`; production `NodeFlush::stop` waits for the already submitted job's release (`flush/site.rs:347-349`, `flush/control.rs:423-425`). It does not initiate a shutdown ALWAYS flush. It settles deferred work from earlier Core `FlushStateToDisk` sites, which Core executes synchronously at those sites. Waiting for its result before initialization preserves the relevant order.
- `ChainSteps::force_flush(First)` is Core's shutdown `ForceFlushStateToDisk` at `init.cpp:379-385`, which follows loadblk's join at `:355`. The later shutdown flush remains later too. A returned failure at those shutdown sites cannot suppress an initialization call that has already completed. `ForceFlushStateToDisk` itself only logs a returned failure (`validation.cpp:3128-3133`).

Thus item 6 should preserve init for ordinary returned errors. It must still account for the deferred driver-drop panic in M-1; the fact that `stop_flush_at_exit` returned an error does not prove V will finish without unwinding.

### New tests and M11–M14

I read `/work/scratch/bl547/mut-patch.py` and `mut-run-r1.log`; I did not rerun them. The log reports a passing baseline (37 selected chainstate tests and one node test), and all 14 mutants killed. The four new kills are supported by the assertions and production paths:

| Mutant | Assessment |
| --- | --- |
| M11: send disposition before stop | Both stop-panic tests join V before submitting initialization. The old buffered `Serve` yields success and a listener call, so their new assertions kill it. These use `ModelW`, not the production driver destructor; they do not cover M-1 above. |
| M12: IBD from the published tip | `tests/dmn.rs:661-703` keeps importing true, publishes a stale b1, then privately advances to recent b2 after abort. The shared IBD latch has not flipped. Initialization must report false; using b1 reports true. The test kills this mutant. |
| M13: cache a strong signals upgrade | `tests/dmn.rs:748-768` uses a real handle-owning listener and a drop sentinel. After one reaper initialization, V and the signals thread are joined/dropped and outside owners released. A cached strong signals reference retains the listener/handle/lane cycle and the sentinel stays false. This kills the specified cache mutant. It establishes release of that cycle, rather than directly joining the detached reaper thread. |
| M14: serve a contract failure | `tests/dmn.rs:710-726` checks all three negative outcomes. Removing `f.is_contract()` restores a successful dispatch, so the test kills it. |

The node regression now registers the actual `SyncListener` (`dashd-node/src/chain/tests.rs:675-676`) and checks `is_reached_best_header()` after loadblk's initialization (`:697`). The import sets loading before genesis ABC, so preceding tip notifications are in IBD; the initialization out of IBD is what reaches the best-header state. This exercises production `Chain::import` → `after_import` → reaper → `SyncListener`, with seam storage/connectors. It still does not exercise `Node::shutdown`/`Net::stop`; D72 now explicitly records that separate BL-551 dependency.

**Cheap surviving mutant, inferred from source:** change only the orderly-stop `Err(f)` arm at `actor.rs:1474` to select `AfterStop::Drop` instead of the captured `Serve`. The selected D72 tests use successful stops or panicking stops, not a normally returned stop error. The existing returned-error fixture `a_job_failing_while_vs_stop_waits_takes_the_fatal_path` (`tests/flush.rs:1194-1211`) checks the fatal/report/latch, but never initialization. This mutant suppresses the Core-required call after an ordinary failed stop and should survive the reported selection. Add `SetLoading(false)` plus a successful initialization/DMN/listener assertion to that fixture. This is a coverage observation, not a second implementation finding.

The round closes the stale-view IBD gap, strong-upgrade cycle gap, contract disposition and real-listener test gap. The narrowed D72 wording and explicit BL-551 dependency resolve the round-1 documentation/dependency requests. The remaining blocker is M-1's production driver-drop panic.
