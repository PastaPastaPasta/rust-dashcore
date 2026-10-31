# BL-547 round 1 — Sol review

**Verdict: CHANGES-REQUIRED. Counts: 1 major, 1 minor, 0 nits.**

Reviewed frozen head `2ece6d4adbc13e30a3b916c7eda1a26fe06b29c3` against `30955743a`, the plan, DEC-924/DEC-890, and Dash Core `v24.0.0-rc.4`. This was a source review. I did not build, run tests or mutants, or modify the worktree or `/work/scratch/bl547`. The mutant results below are the supplied results, checked against the mutation script.

## Findings

### M-1 — The reaper's permission to dispatch survives a subsequent V panic

**Location:** `dashd-chainstate/src/chain/actor.rs:1466` (also `:1469`, `:1490`, `:1526`, `:993`).

`run` sends `AfterStop::Serve` before `stop_flush_at_exit`. That stop can still panic. `stop_flush_at_exit` correctly treats this as V's panic, trips the latch and resumes unwinding, but it cannot retract the already-buffered `Serve`. The reaper will consequently move the DMN tip, run listeners and return success even when the initialization command arrives **after V's panic has finished**. The disconnected-sender fallback to `AfterStop::Drop` does not help: the receiver first obtains the buffered `Serve`.

Concrete failing sequence, using an existing fixture:

1. Use the setup of `chain::tests::flush::a_panic_in_the_orderly_flush_stop_is_vs_panic` (`tests/flush.rs:1339`): keep the rig's signals alive and set `ModelW.stop_panics = true`.
2. Send shutdown. `serve` returns `None`; `run` queues `Serve` at `actor.rs:1466`.
3. `ModelW::stop` panics (`tests/flush.rs:449`). V reports its panic, hands over the lanes through `StopTail`, and unwinds. Join V and observe the panic, as that existing test already does.
4. Send `SetLoading(false)` and `InitializeCurrentBlockTip` through the surviving handle. The reaper obtains the old `Serve`, moves the DMN tip and calls the listener; the reply is `Ok(())`.

The same counterexample extends `a_panic_in_the_flush_stop_after_an_abort_trips_the_latch` (`tests/flush.rs:1315`): after the original abort, `run` publishes `Serve`, then its flush stop panics. This is a stop-phase failure, distinct from the assert inside `serve` covered by the new test at `tests/dmn.rs:635`.

This violates D72's panic outcome and the review's requirement that a panic-stop command be dropped. D62 explicitly classifies this existing failure path as V's panic, with the process-ending Core-assert semantics; it is not a successful abort-only exit. There is no race needed to demonstrate the wrong outcome: issue the initialization command after joining the panicked V.

**Requested change:** finalize the reaper disposition after the fallible stop work, or otherwise ensure that an unwind before completion leaves `Drop`. Preserve the tip captured after `serve` and preserve early lane handover for D220. Add initialization assertions to both existing stop-panic fixtures: disconnected reply, unchanged DMN-move count and no listener call. Actual shutdown force-flushes still remain after loadblk's join, so delaying this disposition need not move the callback past those flushes.

### m-1 — Production shutdown still removes the actual sync listener before loadblk's final call

**Location:** `docs/node-program/designs/CH7-notes.md:76`; underlying code at `dashd-node/src/p2p/mod.rs:829`, `dashd-node/src/node.rs:479`, and `dashd-node/src/chain.rs:398`.

This is a **pre-existing integration dependency**, not a regression introduced by the actor change. It nevertheless leaves the listener-lifetime condition in DEC-924 unmet. D72 acknowledges the deviation but does not fix it.

Concrete failing sequence:

1. Start import with the production `SyncListener` registered by `Net::attach` (`p2p/mod.rs:686`). Hold startup ABC before it returns.
2. Request shutdown and let `Node::shutdown` reach `net.stop()` (`node.rs:479`). `Net::stop` unregisters `SyncListener` at `p2p/mod.rs:830`.
3. `steps.shutdown()` calls `Chain::stop`, which stops V and joins loadblk. Release ABC. Loadblk clears importing and submits its initialization command.
4. The new reaper moves the DMN tip and replies, but the production sync listener has already been removed. `MasternodeSync::updated_block_tip` is never called for this initialization event.

Core unregisters peerman/clhandler before joining loadblk (`init.cpp:346–355`), but keeps `CDSNotificationInterface` through that join and the first shutdown flush, unregistering it at `:404–406`. Its `InitializeCurrentBlockTip` invokes the sync update (`dsnotificationinterface.cpp:31–35`, `:60`). After an abort, the Rust outcome additionally depends on whether shutdown or loadblk wins this race.

The new node test registers `HeardInit` directly and joins loadblk before stopping the chain (`chain/tests.rs:654–665`); it never traverses `Node::shutdown`/`Net::stop`, so its success cannot establish the production listener ordering.

**Requested change:** retain/unregister the sync listener at the chain/signals shutdown stage corresponding to Core's DS interface lifetime, while keeping the peerman listener's earlier unregister. Add a gated production-node shutdown-during-import test. If this requires a separate network-owned change, record it as an explicit dependency/manager deferral; BL-547 alone does not establish this end-to-end parity condition.

## What I checked

- **Tip and carried work:** the handoff reads `self.tip()` after `serve` returns. `serve` finishes the interrupted operation and `run_carried_tails` before returning (`actor.rs:1651–1683`). A `Fatal::assert` returned there selects `Drop`; a Rust panic within `serve` unwinds without a disposition. The new synthetic disconnect test distinguishes the non-public tip from the last published `ChainView`. M-1 concerns the remaining fallible work after this point.
- **IBD and command order:** production loadblk executes the second abort report before `after_import`; that function sends importing=false and then initialization from the same unmarked thread. Both use the same FIFO lane. The reaper reads the loading flags when it serves initialization, obtains work/time from the non-public tip's entry, and calls the same shared, latched IBD implementation as V. Its time, loading, null-tip, minimum-work and maximum-age decisions match Core's `validation.cpp:1976–1998`. It does not publish a view.
- **Exactly once:** for one initialization command, the lane removes one command and either V's handler or the reaper consumes it. Each calls `initialize_dmn_tip` once before listeners. There is no forwarding of an already-consumed command. A null tip moves nothing, as Core's null-tip/null-fork guard does. A withheld view still moves the tip but suppresses listeners, per the existing D65 policy. Panics/asserts before the handoff drop the command; M-1 identifies the uncovered later panic.
- **Flush ordering:** `Chain::stop` joins loadblk before returning to `ChainSteps::shutdown`; its two `force_flush` calls follow. Therefore a completed reaper callback/reply precedes those shutdown flushes. Listener removal has the separate problem in m-1.
- **Liveness and ownership:** if the weak signals upgrade fails, the reaper returns success without dispatch and releases the reply. V's unsent disposition disconnects on unwind. Dropping the reply receiver does not suppress dispatch or block the sender. The reaper drains both lanes through disconnection and exits after the last sender is gone. Holding signals weakly avoids the reaper → signals → listener → `ChainViewHandle` → lane-sender cycle. A callback that blocks indefinitely blocks loadblk and its shutdown join, just as Core's synchronous initialization callback would; the current production initialization callbacks do not introduce that wait.
- **Real callbacks and re-entry:** `SyncListener` uses actual `ChainViewHandle::entry` and `snapshot`, which reach `ChainReader`/ArcSwap rather than a V command, then updates `MasternodeSync`'s own atomics. The peerman listener and RPC catcher inherit the no-op initialization method. I found no production initialization callback waiting on V. `InitProbe` uses real `ChainReader` and `ChainViewHandle` objects, but is not the production `SyncListener` and reads `view()` rather than its `snapshot()` route; it tests the primitives, not the whole registered listener.
- **Node regression test:** `FailingFlushAfterOne` permits genesis's connect flush and fails a later startup ABC site. The test invokes production `Chain::import`, the ABC handler, loadblk's second abort, `after_import`, and the reaper. Its final tip is non-null and its listener assertion requires exactly one `(tip, false, 2)` event. Thus the callback is after both recorded abort reports and after importing clears. It uses seam connectors/flushes, not a complete `Node` shutdown.

## Mutant coverage

The supplied log records M1–M10 killed. The script confirms coverage for dropping the command, fixing IBD at stop, using the published tip, skipping the DMN move, reading loading too early, freezing the disposition at the first abort, uncaught callback panic, ignoring withholding, a duplicate V-handler move, and serving after a returned assert.

Two relevant gaps remain:

- **Cache the first successful weak upgrade for the reaper's lifetime.** This should survive the reported suites: they assert callback results while strong signals owners remain and do not assert eventual destruction/thread exit. It restores the signals/listener/handle ownership cycle. A drop sentinel around a listener holding a real `ChainViewHandle`, with the signals thread stopped and all external owners dropped, would detect it. This survivor is inferred from source, not experimentally run.
- **Stop-phase panic:** the existing flush-stop panic tests do not send initialization afterward, and the mutation runner selects only `dmn`/`actor` plus the one node test. M10 checks a returned assert before disposition publication, not M-1's buffered permission after a later unwind.

A reply-before-listeners mutant is already covered: `tests/dmn.rs:544–547` holds the entered listener and checks that the reply remains pending. Cached IBD from V's stop is covered by M2; it should not be listed as an untested survivor.
