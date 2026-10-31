# BL-271 review round 1 (Sol)

Verdict: **ACCEPT-WITH-NITS**

## Findings

### n-1 — No test distinguishes a successful first shutdown flush from a failing second flush

- **Location:** `dashd-node/src/steps.rs:491-520` (the two `force_flush` calls); Core reference `src/init.cpp:378-385` and `:449-455`, `src/validation.cpp:3120-3125` (the second `ForceFlushStateToDisk`).
- **Scenario:** the first `ALWAYS` flush completes, then a disk/WAL failure occurs before or during the second `ALWAYS` flush. Core reports the second `AbortNode` and `ForceFlushStateToDisk: failed to flush state (...)`, and exits aborted. A test mutant that skips the second call, or turns it into a no-op, silently misses that failure. The current suite asserts the count/order of flush attempts and exercises a first-flush failure, but does not arm a fault specifically between the two calls and assert the second-flush report.
- **Suggested fix:** add a test hook/probe that arms `fail_next_wal_sync` after the first `Point::Flush` (or otherwise injects a failure only on the second flush), and assert the second report, exit status, and log ordering. This is coverage only; the reviewed implementation does make both calls in the Core order.

## Mutant assessment

- **Skip shutdown flush #2:** not equivalent. A second-only WAL/disk failure distinguishes it, as above. The existing two-call counter catches omission but not the semantic effect.
- **Remove `VExit`'s own `AbortNode` report:** not equivalent for the `FlushSite` contract. A flush can return an unreported fatal with the latch still clear; Core then reports it before the `failed to flush state` line. `VExit::force_flush` correctly preserves that report. The production `NodeFlush` paths generally report W failures themselves, but the seam and its tests make the ordering contract observable.
- **Run a fresh devnet through the regular stored-chain path:** equivalent for a genuinely fresh devnet's successful behavior; the distinguishing case is a devnet with coins/transition metadata but no block-index records, which is a lost-index recovery case and is explicitly handled by `devnet_pending()` before V starts.

## What I checked and ran

- Reviewed `git diff bfec24a81 c5421b04a` (13 files, including `VExit`, `ChainSteps::shutdown/kill`, load-failure paths, abort reporting, and CONS-11 tests).
- Compared shutdown ordering and conditions with the supplied Core v24.0.0-rc.4 references (`init.cpp`, `validation.cpp`, `shutdown.cpp`). A local `/work/home/agent/workspace/dash` checkout exists but is at a newer commit, so the supplied v24 line references were treated as the authority and not as an exact local-version verification.
- Traced V hand-back ownership, W joining/panic handling, latch transitions, signal delivery (`deliver=true` only on flush #1), kill drops, and restart-tip persistence.
- Created the required reflink scratch copy at `/work/scratch/bl271-rv-sol-2`; no source edits were made.
- `cargo fmt --all -- --check` passed on the reviewed worktree.
- In the scratch copy, `CARGO_BUILD_JOBS=12 TMPDIR=/work/scratch/bl271-rv-sol-2/tmp cargo test -p dashd-node --features test-support --lib shutdown_flush` passed (3 tests, including first-flush ordering, V-state durability, and failure-log ordering).
- In the scratch copy, `CARGO_BUILD_JOBS=12 TMPDIR=/work/scratch/bl271-rv-sol-2/tmp cargo test -p dashd-chainstate --features test-support shutdown_flushes_on_vs_exit` passed (1 targeted unit test; the other integration targets ran zero filtered tests).
