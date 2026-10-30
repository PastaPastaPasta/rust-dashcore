# CH2b b3 subscribers — Sol round 2 (narrow)

Reviewed `node/ch2b-b3` at `1cffd18f9` against `6993f2f2f`, the round-1 review, CH2 §4.2/§4.4/§7.2 b3, and D35–D44. I used a detached scratch worktree; the branch under review was not changed.

## Findings

### Blocker 1 — A post-window registration can return with L ahead of durable G

`dashd-chainstate/src/flush/hub.rs:373-412, 632-646`; `dashd-chainstate/src/subscriber/driver.rs:333-350, 369-397, 225-252`.

D35 correctly prevents a late registration from entering either the step-3b gate or step-7 prune snapshot: it waits until release. It does not rebase an already durable index locator when that release follows a backward B1. Let old durable G be block 10 and a persisted, currently unregistered mandatory index have L=9. W opens the window and flushes backward to block 6. The waiting `open` then registers L=9 after release; `prune_violation` checks the still-present blocks 6..6 and accepts it, and `open` returns without calling `sync_step`. The hub now has a mandatory L=9 with `T_ack=6`. A crash before the caller steps the driver leaves R11a's forbidden image. The new `a_registration_after_step_3b_waits_for_the_flush` test itself asserts the accepted L=9 after `T_ack=6`; it tests the window, not the post-window locator invariant. Require a durable rewind to `LCA(L,G)` before `open` succeeds, or refuse this registration and state an explicit R11a exception for newly enabled indexes. Add the backward-B1/late-open/crash case to the model.

Core can start an index with a stale locator and let its sync thread rewind it; the stricter R11a guarantee is CH2's own rule. The current fix does not meet that rule for this registration path.

### Major 2 — An optional flushed target can skip archived stale blocks

`dashd-chainstate/src/subscriber/driver.rs:639-650, 819-855, 949-973`.

D38 applies case B's off-leg drop to an optional listener whenever its `ChainStateFlushed` control overtakes data. At p=L=5, queue `Connected(A6), Connected(A7), Disconnected(A7), Disconnected(A6), Connected(B6), Connected(B7)`, then flush at B7. Core's validation queue delivers all six data callbacks before `ChainStateFlushed`: the A blocks are processed, then their disconnects archive their rows. Here the control sets `flushed=B7`; case B drops both A connects and ignores their disconnects, so an archive-capable optional subscriber never stores A6/A7. The forward-only D38 tests do not cover this. Preserve the pre-flush data effects for optional listeners, or explicitly constrain optional subscribers so they cannot promise stale-block archive queries. Add a reorg backlog regression with archive assertions.

### Minor 3 — The `open` scheduling guard still misses an unstepped driver

`dashd-chainstate/src/subscriber/driver.rs:287-289, 376`; `dashd-chainstate/src/flush/hub.rs:301-314, 373-383`.

D39 records only the last thread that called `step` or `sync_step`. If one thread opens A at L=9, then a backward flush to 6 gates on A before its first step, an `open(B)` on that thread can wait in the registration window. The thread cannot begin stepping A, so W cannot release the window. D43's W/V refusals do not cover this events thread. D39 documents the hole; it remains a liveness limitation of the public blocking `open` API. A retryable, nonblocking registration would close it.

## Round-1 disposition

| Item | Status at `1cffd18f9` |
|---|---|
| B1 | **Partial.** D35 closes registration versus gate/prune snapshots, but post-release registration can expose L above the new durable tip (Blocker 1). |
| M2 | **Closed.** `finish()` keeps the lock, `SUB_DISABLED` is Durable, then drop deregisters; `Dp410` places a prune immediately after deregistration. |
| M3 | **Closed for the specified handoff.** Step 5 publishes `T_ack` before reading Synced; `handoff` rechecks after the cut. Both orderings have barrier tests. |
| M4 | **Partial.** The forward optional backlog and journal-limit cases are repaired, but the new case B path loses stale-branch archive effects (Major 2). |
| m5 | **Partial.** A known stepping thread is refused; an opened but unstepped driver is not (Minor 3). |
| m6 | **Closed.** Wrong-flush, uncaptured and gateless reports return before updating the mirrored locator. |

## D43 and Core semantics

`WouldDeadlock` is returned before allocating a hub entry on W's thread while its window is open, and on V's recorded submit thread while W awaits `PruneClears`; setting `awaits_v` wakes a V-thread registrant already waiting. Feed-slot refusal removes its temporary hub entry, and a failed prune check deregisters. No refusal-path prune-lock leak was found. The guard depends on `FlushControl::set_hub` using W's hub, which D44 correctly calls out for future node wiring. A W panic leaves the window open but trips the shared fail-stop latch; registration's timed wait then returns fatal.

Core queues `BlockConnected`/`BlockDisconnected` and `ChainStateFlushed` on one `CValidationInterface` queue (`validationinterface.cpp:232-255`); `BaseIndex::ChainStateFlushed` skips when not Synced or when its locator is off the known best chain (`index/base.cpp:332-368`). D37's handoff recheck guarantees an idle-tip commit even when Core's callback could have run while `m_synced` was false, and D38's LCA commit may advance an off-branch locator where Core would warn and skip. Both are observable in the durable locator. Step 6's mandatory ack barrier also delays release/prune beyond Core (`validation.cpp:2972-3017, 3109-3112`), as D41 now documents. These are CH2 guarantees, not Core parity.

## Validation

- `CHAINSTATE_CASES=5000` `chainstate_crash_smoke` passed on seeds `50321` and `50322`: each had 5,000 images, 5,000 branches, zero truncations and `found {}` (146.74s and 134.21s).
- Targeted D35, D37, D38, D39, D40 and D43 unit tests all passed. Unplanted S11cW passed with 27 images, 73 branches and `found {}`. The new `LateRecord` plant passed its witness test: 14 images, 28 branches and R11b found, demonstrating that moving the record after deregistration/prune breaks the model.
- Three scratch-only unit regressions failed at their intended assertions (`reviewer_regressions.log`): a post-window hub registration had L=9 with `T_ack=6`; an actual `SubscriberDriver::open` after a backward flush returned L=9 with G=6; and an optional reorg backlog had no archive row for A6. They were run only in the detached worktree.
- `git diff --check 95b0653a2..1cffd18f9` passed. For focused tests I temporarily removed the self dev-dependency that forces unrelated RocksDB compilation from the scratch manifest; reviewed source code was unchanged. Logs are under `/work/scratch/ch2b-b3-r2-sol/`.
- Invoked the requested node-fuzz smoke with `DASH_EMBARGO_DIR` and `--no-selftest --only chainstate_crash coins_replay_blind`. It was stopped during a fresh optimized RocksDB build under the box load; no fuzz cases ran.

## Summary

The registration window, durable disable order, handoff recheck, and stale-report validation close their direct round-1 races. The fix still admits a registered locator above durable chainstate after a backward flush, and D38's optional case B loses stale-block archive rows when a flush control overtakes a reorg backlog; both reproduced in scratch tests. The unstepped-driver `open` deadlock remains a documented scheduling gap. The requested CRASH soaks and LateRecord witness passed, so the verdict rests on these uncovered state and liveness paths.

VERDICT: CHANGES-REQUIRED
