# ND1 F1, Sol round 1 — shared chainstate steps and NodeSut

**Verdict: CHANGES REQUIRED**

Reviewed `3d14e7f09..4352535d9` in a detached worktree against Dash Core v24.0.0-rc.3 (`0364ed626e`) and ND1-D8/D10–D13/D15/D20/D21. The production node's moved step order, cache sizes, reindex question, shutdown logs, flush reporting, and exit status match the F2 baseline. `ChainSteps` is used by both the node and SUT; no duplicate implementation of steps 7a, 7, 11, shutdown, or kill was found. A clean or killed open releases V and its signals before the next open; each open gets new shutdown flags and steps, while the memory engine and clock remain session owned.

## Findings

1. **P2 — `test-support` can enter an optimized `rdashd` build.** `dashd-node/src/lib.rs:9-10,27` guards only `test-hooks` with the release `compile_error!`, while `dashd-node/Cargo.toml:20` exposes `test-support` independently. A build such as `cargo build -p rdashd --release --features dashd-node/test-support` enables and compiles `sut` without tripping the canary; `cargo tree -p rdashd --features dashd-node/test-support -e features -i dashd-node` confirms the feature reaches the dependency. The ordinary workspace graph enables only `dashd-node/default`, so this is an explicit-feature or future feature-unification path. Guard both test-only features in optimized non-clippy builds. No Core counterpart applies.
2. **P2 — The SUT tests do not protect production parameter interaction.** `dashd-node/src/sut.rs:176,469-481` calls `init_parameter_interaction`, but its SUT tests use no accepted argument whose result depends on that call. Removing it passes all seven existing SUT tests. With `-prune=550`, production interaction soft-sets `-txindex=0` and `-disablegovernance=1` (`dashd-node/src/init/interaction.rs:104-109`; Core `src/init.cpp:1084-1090`); without it, `app_init_parameter_interaction` refuses the same SUT argv at `dashd-node/src/init/checks.rs:72-88`. A temporary positive SUT control passed on the head and failed on the mutant. Add that control (or an equivalent accepted interaction), so a production init change cannot silently miss the harness.

## Core and isolation checks

- `Node::try_app_init_main` still loads sporks before cache sizing and step 7, then imports at step 11 (`dashd-node/src/node.rs:186-266`; Core `src/init.cpp:1967-1972,1983-2025,2419-2458`). `ChainSteps::load` retains the devnet V-start branch, non-devnet coin-open branch, and attach error handling from the baseline (`dashd-node/src/steps.rs:116-163`). The shared cache helper reproduces the former `Node` expression (`dashd-node/src/init/caches.rs:19-40`). The shared reindex question preserves the baseline string (`dashd-node/src/chainstate.rs:157-160`; Core `src/init.cpp:2119-2133`).
- `ChainSteps::shutdown` retains V stop, callback drain, flush #1, final callback flush, spork store, flush #2, then coins reset (`dashd-node/src/steps.rs:207-235`; Core `src/init.cpp:355-455`). `force_flush` retains F2's two failure reports through `NodeAbort`, even after an abort (`dashd-node/src/steps.rs:264-287`; Core `src/validation.cpp:3116-3126`, `src/init.cpp:379-385,449-455`). `Node` still calls the shared shutdown after stopping RPC and the scheduler, with the same surrounding log lines (`dashd-node/src/node.rs:346-366`).
- On `restart`, `NodeSut` tears down the current steps, resets the memory engine's fail-stop latch, and constructs new shutdown flags and steps (`dashd-node/src/sut.rs:198-223`; `dash-kv/src/memory.rs:129-135`). `kill` joins V and discards sporks and chainstate without flushing or storing (`dashd-node/src/steps.rs:241-248`); the down-state paths have no live chainstate or spork file to write. The session clock is preserved. The queued-signal behavior and the CH3a consumer work explicitly left in ND1-D13 were not treated as F1 findings.

## Validation and mutation checks

- `cargo test -p dashd-node`: **PASS**, 114 unit tests plus 11 integration tests; one helper test ignored.
- `cargo test -p dashd-node --features test-support,test-hooks`: **PASS**, 121 unit tests plus 11 integration tests; one helper test ignored.
- `cargo test -p rdashd --features test-hooks --no-fail-fast`: **PASS**, 51 tests across seven test binaries; no port-race rerun needed.
- `cargo tree --workspace -e features -i dashd-node`: only the default feature through `rdashd`. An explicit `dashd-node/test-support` appears in the `rdashd` graph.
- `cargo clippy -p dashd-node --all-targets -- -D warnings` and the same command with `--features test-support,test-hooks`: **PASS**.

| Mutation in the private worktree | Check | Result |
|---|---|---|
| Hand the previous open's shutdown flags to new `ChainSteps` by constructing steps before replacing the flags (`sut.rs:213-214`) | `sut::tests::each_open_has_its_own_shutdown_flags` | **Rejected**: next open is down with `Shutdown requested. Exiting.` instead of loaded. |
| Remove flush #2 (`steps.rs:232-235`) | `sut::tests::a_clean_restart_flushes_twice_and_keeps_the_sporks` | **Rejected**: observed flush count 1 instead of 2. |
| Skip `init_parameter_interaction` (`sut.rs:176`) | All existing `sut::` tests | **Survived**: 7/7 passed. A temporary `-prune=550` control passed on the head and failed on this mutant. |

The original seven SUT tests passed again after restoring the worktree. The new SUT tests use no elapsed-time assertions or sleeps; the process suite also passed without a port-race rerun. No optimized `rdashd` build was run: the release-feature finding follows from the explicit feature graph and `lib.rs`'s `cfg` predicate.

All Cargo commands used `CARGO_TARGET_DIR=/work/scratch/nd1-f1-r1-sol/target`.
