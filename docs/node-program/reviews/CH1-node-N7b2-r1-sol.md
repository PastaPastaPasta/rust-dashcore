# CH1-node N7b-2 — Sol round 1

Reviewed `f1c695664` and `3b55ebbea`, plus the `node-fuzz/src/models.rs` resolution in `288c2d996`, in a detached worktree at `288c2d996`. CH2b b3 is treated as the separately reviewed dependency.

## Verdict

**ACCEPT.** No blocking or minor defect found in N7b-2's changed code.

## Findings

None. I found no failing V schedule introduced by these commits.

## Review trace

- **Wait coverage:** `dashd-chainstate/src/chain/actor.rs:733,870,1111` answers due on VWait's bell, the idle select's bell, and a release point's bell. `dashd-chainstate/src/chain/abc.rs:262-265` answers after a blocked `retry_pending` and before the in-hold park, including `Steps::Enforce`. `VWait` is the `ServiceWait` used by `limit_queue` and its `sync`; it continues to select the bell after the command lanes close while the bell exists. A disconnected bell is removed rather than spun on (`chain/flush.rs:133-140`). `run`'s final `service` and both sides of `attach_coins` remain event-only, as D58 requires. No additional V park that can be awakened by a subscriber's `FlushDue` was found.
- **Commit ordering:** `chain/abc.rs:565-574,596-611` puts `at_site` after each connector commit, `after_step` after the tip step, and `drain_fanout` after both. Thus a due raised during a commit is taken by that site's decision and can freeze only at its post-step pair; the new `answer_due` cannot run in the commit-to-step gap. Dash Core v24 `validation.cpp:3280-3316,3427-3473` places its coin commits before the IF_NEEDED call and its tip step after it. The model's `at_site` takes the due and its `after_step` records the freeze at the stepped pair (`chain/tests/flush.rs:246-300`).
- **Reentrancy and progress:** The bell's `try_send` and parker notification do not synchronously enter V (`chain/flush.rs:166-170`). The in-hold path retries pending fanout before entering `answer_due`; the parker keeps a notification sent before `park`. The future production adapter must retain D47's `host.retry_pending` rule in any W completion wait, since a synchronous freeze can wait on a subscriber. I found no new lock or callback wait in this V slice that reverses that dependency.
- **D57–D59:** D57's no-tip warning is correct: `FlushControl::answer_due` in `flush/control.rs:213-222` has no tip guard, whereas `FlushHost::tip` can be `None` before genesis; the production adapter must keep item 6's guard. X15's note is correct because V calls `answer_due` directly in `drain_fanout`, bypassing `FlushControl::backpressure_wait` and its planted X15 branch (`flush/control.rs:233-250`). D59's m-D and m-F concern `SubscriberHub` registration and driver ownership; this V slice neither opens nor moves a `SubscriberDriver`.
- **Tests:** The four directed tests at `chain/tests/flush.rs:1381-1479` isolate idle, release-point, `VWait`, and in-hold answers. Reverting each call to `service` would leave the expected freeze absent or at a later pair. The `VWait` test raises due inside the running command immediately before `limit_queue` (`:1428-1432`), so a prior release point cannot take it. The hold test gates the subscriber and checks the unpublished stepped pair before releasing the gate; its 120-second watchdog is only a failure bound. The commit, no-tip, stop, and attach tests cover the negative positions (`:1517-1581`, `:1248-1284`). These are model-W tests; they do not exercise the later production `FlushSite` adapter or its poll-first retained-cause implementation.
- **Merge:** `node-fuzz/src/models.rs:9-10,118-119,138-139` keeps both `coins_replay_blind` and `connman_sched`; both entries also remain in `node-fuzz/targets.toml`, `node-fuzz/Cargo.toml`, their model source files, and their fuzz bins. The combined diff is a union of the two parent registrations.

## Validation

- `git diff --check` on the N7b-2 changes and merge resolution: passed.
- `cargo test -p dashd-chainstate` with `CARGO_TARGET_DIR` and `TMPDIR` inside `/work/scratch/ch1-n7b2-r1-sol`, `RUSTC_WRAPPER=` and a same-fingerprint prebuilt RocksDB archive copied into that scratch directory: **exit 0**. Library: 470 passed. Integration: 21 passed, 2 ignored. Doc tests: 4 passed. The copied archive matched its source SHA-256 (`7a9d0746a783ad2c7bd9b8da49e015a866072c3ae2c74c4d389592e5867aed7b`).
- The owner's eight-mutant script was inspected but not run: it hard-codes the owner's active branch worktree and its hold mutant waits up to 120 seconds, so it is not a cheap independent check under the current load.
