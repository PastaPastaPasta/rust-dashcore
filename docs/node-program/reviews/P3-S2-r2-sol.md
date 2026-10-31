# P3 S2 review — Sol round 2 (narrow)

**Head:** `d714bac05` (code in `dcedde7b7`), against `0e97ada2c`; Core reference `0364ed626e` (`v24.0.0-rc.3`). Review used a detached worktree. No changes to the branch under review.

## Findings

None at P1, P2, P3, or Nit severity in this fix round.

## Round 1 item status

| Item | Status | Evidence |
|---|---|---|
| F1, early block timeout origins | **Closed** | Section 12 now samples the node clock for each request after the walk (`dashd-p2p/src/processing/download.rs:578-587`), matching Core's `BlockRequested` clock read (`net_processing.cpp:1349-1350,6822-6825`). Both `block` removal paths sample after decode and mutation handling (`dashd-p2p/src/processing/blocks.rs:88-105`), matching Core's `RemoveBlockRequest` read (`net_processing.cpp:1307-1310,5352-5375`). The 250 ms walk and 200 ms decode schedule passes at the head. Restoring the old section 12 timestamp fails at `sim_blocks.rs:524` (150.100 s); restoring the old removal timestamp fails at `sim_blocks.rs:544` (160.100 s). |
| N1, missing stall log order assertion | **Closed** | `dashd-p2p/tests/sim_blocks.rs:363-366` asserts the disconnect line precedes the timeout raise line. Moving the raise before the disconnect log fails at line 366 and shows the reversed lines. This matches Core `net_processing.cpp:6754-6760`. |
| Nit, unbounded decay test | **Closed** | `dashd-p2p/src/processing/download.rs:1318-1323` bounds the raise and decay collections with `take(10)` and `take(20)` while retaining the exact expected sequences. |

## Call site and regression audit

- Direct fetch already samples at each `block_requested` call (`download.rs:608-617`; Core `net_processing.cpp:3230-3235`). The successful `ProcessBlock` tail samples at its `remove_block_request` call (`blocks.rs:147-152`; Core `net_processing.cpp:3720-3723`). S2 has no compact-block request or removal implementation yet; Core's compact paths belong to S3. `FetchBlock` is also outside S2. Peer finalization erases in-flight entries without a clock read in both implementations (`download.rs:673-689`; Core `net_processing.cpp:1803-1819`). `notfound` completes object requests and does not remove block in-flight entries in either implementation (`logic.rs:799-808`; Core `net_processing.cpp:5740-5760`).
- Section 9 still receives the pass-start `now_us` from `send_messages` (`logic.rs:997,1023`; `download.rs:528-545`), matching Core's `current_time` (`net_processing.cpp:6317,6748-6775`). Stall start still uses that same pass-start value (`logic.rs:1037`; `download.rs:587,376-387`; Core `net_processing.cpp:6828-6831`). The new clock reads do not move either decision to a later time.
- The read hook is confined to `SimChain` under the crate's `sim` feature (`dashd-p2p/src/lib.rs:27-28`; `sim/chain.rs:410-423,509-522,1208-1220`). It is unset by default and the production chain implementation does not use it. The hook runs before the two simulated chain reads, outside the hooks lock, so the timing test can advance the clock without changing production processing.
- The diff contains only the timing fix, its simulation hook and tests, and the D86/D87 notes. No further regression found in those changes.

## Validation

In `/work/scratch/p3-s2-r2-sol/wt`, with a private target and mode-0700 TMPDIR:

| Check | Result |
|---|---|
| `cargo test -p dashd-p2p --test sim_blocks s2c_timeout_origins_are_read_at_the_request_and_the_removal -- --exact` | Passed on the head. Both independent F1 revert mutants failed at their expected assertions. |
| `cargo test -p dashd-p2p --test sim_blocks s2a_ibd_stalling -- --exact` | Passed on the head. The reversed-order mutant failed at `sim_blocks.rs:366`. |
| `cargo test -p dashd-p2p` | Passed: 202 tests across all targets, including the two P3 transcript replays and the bounded decay unit. |
| `cargo clippy -p dashd-p2p --all-targets -- -D warnings` | Passed. |
| `git diff --check`; detached worktree status | Passed; clean after restoring all mutations. |

VERDICT: ACCEPT
