# ND1 F2, Sol round 2 — shutdown flushes

**Verdict: CHANGES REQUIRED**

Reviewed `b618547f1..d5339bc43` against Dash Core v24.0.0-rc.3 (`0364ed626e`). The fixes close the three round-1 findings, but D16's stated limit on its deliberate retry deviation is incorrect.

## Finding

1. **P2 — A transient first-flush failure can change the durable state, not just duplicate error reporting.** `dashd-node/src/node.rs:541-547` treats the first WAL sync error as an `AbortNode`; `dashd-chainstate/src/store/guarded.rs:244-246,350-352` then refuses the second sync because the latch is set. The one-shot fault itself clears before that second call (`dashd-node/src/chainstate.rs:315-324`), demonstrating that the second operation could otherwise proceed. Core's `src/init.cpp:379-385,449-455` calls `ForceFlushStateToDisk` again regardless of the first `AbortNode` (`src/shutdown.cpp:27-37`); each call executes the forced flush path (`src/validation.cpp:3046-3052,3078-3093,3116-3127`). **Schedule:** a loaded chainstate has dirty state; the first shutdown flush encounters a temporary full disk or I/O error, then space is freed before the second flush. Core attempts the second flush and can persist the dirty coins and tip. Here the first failed WAL sync trips the guard, and the second is refused even after the error clears, leaving that WAL sync unperformed. Both processes exit 1, but the durable state and recovery after a subsequent power loss can differ; when Core's retry succeeds, rdashd also emits a second failure line that Core does not. D16's claim that the restart sees the same state because the refused sync wrote nothing omits the successful Core retry. Preserve the fail-stop invariant, but either design and prove a safe retry or explicitly approve and document this broader durability/restart deviation; add a transient-failure test that distinguishes the outcomes.

## Round-1 closure and scope checks

- **F1 closed:** `Node::force_flush` no longer checks `aborted()`. Both calls occur with a loaded chainstate, including after `AbortNode`; absent chainstate yields zero. The exit status remains 1 after abort.
- **F2 closed for the first failed forced flush:** its own error now invokes `AbortNode`, logs the failure, and exits 1. The remaining finding is the second-flush behavior and its stated impact.
- **F3 closed:** removing only `chain.drain_background_callbacks()` from `Node::shutdown` makes `shutdown_drains_the_signals_before_the_first_flush` fail with `[VStopped, Flush { aborted: false }, Flush { aborted: true }]` instead of two aborted flush observations. The production call was restored afterward.
- **WalFault isolation:** `dashd-node/src/chainstate.rs:103-106,249-250` selects `RocksEngine` when neither `cfg(test)` nor `test-hooks` is active. `dashd-node/src/lib.rs:7-10` rejects a release build with `test-hooks`. No other regression was found in the touched code. D18 is outside this review's scope.

## Validation

- `RUSTC_WRAPPER= CARGO_BUILD_JOBS=12 CC='ccache cc' CXX='ccache c++' cargo test -p dashd-node` — exit 0; 114 unit tests passed, 1 ignored, 11 integration checks passed.
- Same environment, `cargo test -p rdashd --features test-hooks --test hooks` — exit 0; 7 passed.
- Drain-call-only mutant, `cargo test -p dashd-node shutdown_drains_the_signals_before_the_first_flush --lib -- --exact node::tests::shutdown_drains_the_signals_before_the_first_flush` — expected exit 101, assertion failed on the first flush's `aborted: false`. The detached worktree was restored to a clean `d5339bc43` before cleanup.
- All builds used the private `CARGO_TARGET_DIR=/work/scratch/nd1-f2-r2-sol/target`.
