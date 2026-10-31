# CH2b b3 subscribers, round 1 (Sol)

Reviewed `6993f2f2f..95b0653a2` against `CH2-coins-flush.md`, D1–D34, SB5, and Dash Core v24's `validationinterface.cpp`, `validation.cpp`, and `index/base.cpp`. The branch under review was not changed.

## Findings

### Blocker 1 — A registration can miss both the rewind gate and the prune lock snapshot

`dashd-chainstate/src/flush/hub.rs:283-304, 502-533, 739-746`; `dashd-chainstate/src/flush/machine.rs:580-627`; `dashd-chainstate/src/subscriber/driver.rs:326-335`.

Step 3b installs the cap and snapshots the then-registered locators, but registration can complete immediately afterward with a durable locator above `new`. The new mandatory subscriber is absent from the gate; W proceeds to B1 at the lower `new`, leaving its locator ahead of durable chainstate (R11a). The same split exists at step 7: W copies `actual_locks` and fixes `selection`, then waits for V's `PruneClears`. A new subscriber can register and pass `prune_violation` while the old `HAVE_DATA` bits remain, after which V clears and W unlinks files that subscriber needs. Even inside `SubscriberHub::register`, the feed slot is registered before the hub's lock entry is installed, leaving a smaller unprotected interval. Core serializes `BaseIndex::Init`'s registration/check and pruning through `cs_main` (`index/base.cpp:79-123`, `validation.cpp:2972-3017`).

Make registration and its prune check linearizable with the in-flight gate and prune decision. A late registration must either be included in a rechecked gate and lock bound before B1/prune apply, or wait until the selected prune finishes and then check the resulting file status. Test both windows with barriers, including a registration between `PruneSelect` and `PruneApply`.

### Major 2 — The disable record is durable only after the prune lock is gone

`dashd-chainstate/src/crash/subs.rs:431-443`.

`disable()` consumes `d.stop()`, whose `Drop` deregisters the subscriber, then writes `SUB_DISABLED`. In the threaded disable protocol, W may prune as soon as deregistration returns. If it prunes and the process crashes before the marker's durable write, the image has a locator whose required data is gone but no disable record. The next boot refuses the index, and the harness itself classifies a refusal without the record as R11b/R0. This helper is currently used by the sequential CRASH runner, which cannot interleave W at that boundary; the S11c sweep therefore does not validate the threaded ordering it is meant to represent.

Persist the disable intent before releasing the final prune lock, and keep the lock until that write returns. Add a crash/interleaving test at the deregistration-to-marker boundary.

### Major 3 — A Syncing-to-Synced handoff can miss `ChainStateFlushed`

`dashd-chainstate/src/flush/hub.rs:458-493`; `dashd-chainstate/src/subscriber/driver.rs:561-580, 602-645`.

W calls `table.synced()` before publishing `T_ack`. A Syncing subscriber can complete `subscribe_at` after this snapshot, committing at the old cap, and become Synced before W publishes the new cap. It is absent from both the captured mandatory set and `others`, so receives neither control. `wake_watchers()` does wake it, but `step()` has no outstanding commit or feed event to service and returns `Idle`. At an idle tip its durable locator can remain below the flushed tip until an unrelated later flush. In Core, every registered validation listener receives the queued `ChainStateFlushed` callback and `BaseIndex` tests `m_synced` when that callback executes (`validationinterface.cpp:250-255`, `index/base.cpp:332-368`).

Linearize the handoff with publication, or have a subscriber recheck `T_ack` after becoming Live and commit/respond when it missed the control. Exercise the handoff between the synced snapshot and publication, with no later tip event.

### Major 4 — Optional listeners lose Core's flush ordering when a control overtakes data

`dashd-chainstate/src/flush/hub.rs:471-492`; `dashd-chainstate/src/subscriber/driver.rs:627-650, 729-750`; `dash-chain/src/feed/reader.rs:117-135`.

Let an optional Synced subscriber be at p=L=5 while V enqueues `BlockConnected` events through block 10. W durably flushes 10 and sends it `ChainStateFlushed`, but no `FlushCommit`. `FeedReader::next` selects the control before its queued data. `on_control` can commit only `LCA(p, 10)=5`; the later data events advance p to 10 with no further commit, so its locator remains 5 at quiescence. Core enqueues `BlockConnected` and `ChainStateFlushed` on one validation queue, so a listener processes the earlier connects before the flush callback and commits its best block when the locator is on its chain. The same issue affects an optional listener at its journal limit.

Give optional subscribers a non-awaited flush target with `tip_seq` and service it through case A/B, or defer their flush callback until the corresponding data events have been consumed. Add an optional-backlog regression with the flush control delivered before the data.

### Minor 5 — The blocking `open` scheduling constraint is only documented

`dashd-chainstate/src/subscriber/driver.rs:262-265, 326-328`; `dashd-chainstate/src/flush/hub.rs:312-333`.

`open` calls `register_waiting`, which parks on `HoldActive`. No API or runtime check prevents a caller from invoking it on a shared events thread that must step an existing subscriber. If V's hold is waiting for that subscriber's queue to drain, this use deadlocks. Current call sites open before event stepping; the constraint should be enforced at the adapter boundary or exposed as a nonblocking open operation before dynamic registration is supported.

### Minor 6 — Rejected acks and rewind reports still overwrite the mirrored locator

`dashd-chainstate/src/flush/hub.rs:563-604`.

Both `ack` and `rewound` assign `sub.durable` before checking the flush ID and target set, despite their documented contract to ignore a report for another flush. The existing `step_5_captures_the_synced_mandatory_set` calls `ack(a, 2, new)` while flush 3 is outstanding but checks only that step 6 remains waiting; it misses the locator change. Validate the report under the hub hold before changing `durable`, and assert the locator is unchanged after a stale report. This is defense against a delayed or duplicated callback; the current single-driver path appears to prevent such a callback in normal operation.

### Parity note — Step 6 is a stronger barrier than Dash Core provides

`dashd-chainstate/src/flush/machine.rs:556-585`; `dashd-chainstate/src/flush/hub.rs:442-495`.

Core enqueues `ChainStateFlushed` asynchronously and `BaseIndex::ChainStateFlushed` can skip a stale locator; `FlushStateToDisk` neither waits for an index ack nor moves pruning after those callbacks (`validation.cpp:2987-3017, 3109-3112`; `index/base.cpp:332-368`). This implementation waits for every captured mandatory ack before selecting prunable files. That is an intentional extra coupling implied by CH2's R11a/R11b, not a Core guarantee; the program should record the liveness and latency tradeoff explicitly rather than describe it as parity.

## D33 and other checks

The existing `step_3b_sees_a_begun_commit_and_caps_later_ones` test covers the basic pending-before-gate and cap-after-gate cases. For subscribers already in the hub, `begin_commit` records the pending target under the same mutex used to install the bound, and `publish_durable` clears the bound at step 5. An ordinary B1 failure trips the global latch, so it does not create a continuing flush with a stuck cap. The liveness model does not include registration during a flush, the table snapshot/publish split, or the pending-commit race; these omissions leave the findings above untested. The single `dashd-node` change maps `LogCat::Prune` to `LogFlags::PRUNE` at Debug and is appropriate.

## Validation

- `cargo test -p dashd-chainstate --all-features` completed with exit 0. It passed 467 library tests, 35 CRASH tests, 61 `crash_planted` tests, 16 `crash_subscriber` tests, 14 liveness-model tests, the other integration tests, and doctests. The full 120-configuration liveness grid is ignored by the PR suite; its 20-configuration PR subset ran.
- Two scratch-only regressions compiled and failed at their intended assertions: registering after step 3b produced no rewind request (`reviewer_regressions.log`), and an optional subscriber consumed queued blocks 5–7 but retained locator 4 after a flush at 7 (`reviewer_regressions.log`).
- Two independent D33 mutations were caught by `step_3b_sees_a_begun_commit_and_caps_later_ones`: removing the pending record made the gate miss the begun commit (`mutant_no_pending.log`); removing the step-3b bound let a later commit target block 10 instead of 6 (`mutant_no_bound.log`). The scratch and reviewed worktrees were clean after restoring the mutations. Logs are under `/work/scratch/ch2b-b3-r1-sol/`.

## Verdict

CHANGES-REQUIRED
