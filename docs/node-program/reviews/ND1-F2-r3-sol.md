# ND1 F2, Sol round 3 — shutdown flushes

**Verdict: ACCEPT WITH NITS**

Reviewed `d5339bc43..3d14e7f09` in a detached worktree against Dash Core v24.0.0-rc.3 (`0364ed626e`), DEC-253, and round 2's P2. The sticky log-sync and background-error classes now report both failed shutdown flushes with the first failure's text. I found no blocking issue in this narrow range.

## Round-2 finding and Core comparison

- **P2 resolved for F2's reachable failure classes (DEC-253 rows 1/1b).** `Chainstate::force_flush` returns the first latched error text when `GuardedEngine::sync_wal` refuses the second call with `ReadOnly`; `Node::force_flush` invokes `AbortNode` on both errors. `GlobalFailStop` prevents another store write after the first failure, and both attempts report failure with exit status 1. Core's LevelDB records a failed log sync as `bg_error_` (`leveldb/db/db_impl.cc:1227-1244,650-655,1328-1331`); the block-index `WriteBatchSync` in each forced flush reaches it. A background error uses the same sticky status. Core's `init.cpp:379-385,449-455` calls the forced flush twice even after `AbortNode`.
- **Report form matches Core with the engine text substitution already recorded in D16.** Core `dbwrapper.cpp:261-268` logs the thrown `Fatal LevelDB error: ...` and the `-debug=leveldb` hint; `validation.cpp:3116-3117` catches it and calls `AbortNode("System error while flushing: ...")`; `shutdown.cpp:27-37` and `noui.cpp:42-45` produce the `***` and `Error:` lines; `validation.cpp:3122-3126` logs the failed forced flush. The new code emits those five lines in that order for each attempt, and two identical stderr lines. I independently compared the saved one-shot-EIO Core and rdashd logs in `/work/scratch/nd1/dashd-fault/run2`: after replacing each engine's error text with `<E>`, all ten report lines are equal and stderr is byte equal. The hook and unit tests also pass at the reviewed head.
- **Rows 2 and 4 are recorded deviations, with no success over a refused flush.** D16/D19 in `/work/scratch/nd1/plan-slice2.md` include proposed ALLOWED-DIFFS entries A (a write failing before sync can leave Core's erased coins or cleared index-dirty sets absent on retry) and B (a retried block/undo fsync can report success after writeback loss). DEC-253 keeps the latch for both; the current guard refuses any later store operation and `Node::force_flush` reports the refusal as failure. Those failure sites are not on F2's WAL-only flush path and require the CH2b adapter tests. Row 3's `CheckDiskSpace` handling remains BL-213, outside this range.

## Mutation checks

Starting from the clean head each time, I ran both `node::tests::a_failed_shutdown_flush_aborts_the_node` and the `rdashd` hooks test `a_failed_shutdown_flush_exits_1` under a private Cargo target:

| Mutant | Unit | Process hook | Observed rejection |
|---|---:|---:|---|
| Call `AbortNode` only on flush 1 | exit 101 | exit 101 | One stderr line instead of two |
| Remove both `HandleError` log lines | exit 101 | exit 101 | Missing error and diagnostic-hint lines |
| Return the `ReadOnly` refusal text instead of the latched first error | exit 101 | exit 101 | Second report contains `refusing writes after an earlier failure` |

The original unit and process hook tests passed before mutation. The worktree source was restored after the last mutant.

## Release seam and validation

- `WalFault` is selected only under `cfg(test)` or `feature = "test-hooks"`; the normal `Engine` alias is `RocksEngine`. The process hook module and call sites are also behind `test-hooks`. `cargo check -p rdashd --release` passed; `cargo check -p rdashd --release --features test-hooks` failed as intended at `dashd-node/src/lib.rs:10` with the test-only `compile_error!`.
- After restoring the original sources, both targeted unit and process hook tests passed again. All Cargo commands used `CARGO_TARGET_DIR=/work/scratch/nd1-f2-r3-sol/target`; that directory was removed after the review.
- The brief records GATE-FAST `nd1-flush2-r3` EXIT 1 only on the known `sim_dial::connect_only` harness failure; it passed five of six isolated reruns from that gate's binary, and bench inputs were all SAME. I did not rerun the full gate.

## Nit

- `dashd-node/src/chainstate.rs` says the returned error text is "as Core's `HandleError` throws it." It is the engine's text; `Node::force_flush` supplies Core's surrounding lines. D16 already states that distinction, and the CH2b plan tracks this wording correction.
