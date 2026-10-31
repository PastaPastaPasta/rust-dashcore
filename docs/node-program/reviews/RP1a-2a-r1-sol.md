# RP1a-2a review (round 1)

Verdict: ACCEPT-WITH-NITS

## Findings

### m-1 — slot registration is not actually refused after the server has started

- **Location:** `dashd-rpc/src/server.rs:297-308` (`RpcServer::on_started` / `on_stopped`); related lifecycle at `:313-340`.
- **Core reference:** `src/rpc/server.cpp:76-84` (signal connection), `:307-335` (`StartRPC`/`InterruptRPC`/`StopRPC`).
- **Scenario:** `server.start(); server.interrupt(); server.on_started(slot)`. `is_running()` is now false, so the advertised “connect before the RPC server runs” assertion accepts the slot, but `Started` has already fired and the callback is never called. The same window accepts an `on_stopped` slot after interrupt; that one is called later, despite being registered after startup. A concurrent `start` can also pass the check before `running.store(true)` and then append after the started signal has taken its snapshot. This violates the commit’s stated late-slot refusal and can silently leave a late wait/notifier unconnected.
- **Suggested fix:** track a one-way `started`/lifecycle state independent of `is_running()` (or serialize registration with start), and reject both slot methods once `start()` has begun; add a regression test covering registration after `interrupt()` and the start/check race.

### n-1 — timeout behavior is not tested as a millisecond timeout

- **Location:** `dashd-rpc/src/blockchain/wait/tests.rs:71-72`; diff cases in `rdashd/tests/rpc_diff.rs:1680-1690`.
- **Core reference:** `src/rpc/blockchain.cpp:330-342`, where the argument is passed to `std::chrono::milliseconds`.
- **Scenario/mutant:** change the Rust deadline to `Duration::from_secs(timeout_ms as u64)` (or accidentally treat a negative timeout as zero/no timeout). The current unit test only checks the returned tip after a 20-ms call and never measures that it returns promptly; the diff cases compare outputs but likewise have no elapsed-time assertion. The behavior can regress to seconds or an indefinite wait while all listed tests still pass.
- **Suggested fix:** use a test with an unmet predicate and assert a small timeout returns within a bounded millisecond window; separately assert negative timeout returns immediately and zero remains unbounded until a notification/stop.

### n-2 — the stop test covers only one of the three wait predicates

- **Location:** `dashd-rpc/src/blockchain/wait/tests.rs:123-139`; node-level stop coverage at `dashd-node/src/chain/tests.rs:691-695`.
- **Core reference:** `src/rpc/blockchain.cpp:334-343`, `:382-388`, `:429-435`; `src/init.cpp:563-568`.
- **Scenario/mutant:** remove the `running()` check (or the stop wake) from only `waitfornewblock` or `waitforblock`. The tests still pass because shutdown is exercised only through `waitforblockheight`; the shared stop slot is not independently verified for each command shape. A regression in one command’s predicate could therefore leave a client hung at shutdown.
- **Suggested fix:** parameterize the shutdown test over all three RPCs (with an unmet new-block/hash/height condition), and assert each returns the pre-stop tip after `interrupt` + `stop`.

## Checked and ran

- Read both requested commits with `git show`: `b0306ac01` and `519819a40`; left merge `f34e810a7` untouched.
- Compared wait parsing/predicates/result shape, shutdown wake/disconnect, signal ordering, initialization tip publication, notifier/chain-slot wiring, and lock/wakeup structure against Dash Core v24.0.0-rc.4 (`src/rpc/blockchain.cpp`, `src/rpc/server.cpp`, `src/init.cpp`).
- Built and ran the isolated scratch copy (reflink source at `/work/scratch/rp1a-2a-rv-sol/src`) with `CARGO_BUILD_JOBS=12`; targeted `dashd-rpc` wait tests: **5 passed**.
- After creating the scratch `TMPDIR`, the targeted `dashd-node` integration test passed (**1 passed**); no live worktree files were modified.
