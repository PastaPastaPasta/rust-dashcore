# ND1 independent review, round 2 — Sol

Range: `6e88abddd..54a53108a` on `node/nd1-rdashd` (11 good `G` signatures). Review checkout: detached `54a53108a` under `/work/scratch/nd1-sol-r2/repo`; the implementer's checkout and branch were not changed. Core source pin: `v24.0.0-rc.3` = `da567d7dba945143eee4d2ce1621c21b32089989`. The pinned oracle binary identifies itself as `Dash Core version v24.0.0-rc.3`.

## Round 1 closure

| ID | Result | Evidence |
| --- | --- | --- |
| S1 | **Closed.** Possible security note: see manager. Location: `rdashd/src/main.rs:108-125`, `rdashd/tests/process.rs:86-115`. | Rebuilt-binary canaries for the argument and config-path variants, with raw and lossy checks over both process streams; the echo-value mutant failed the process test. |
| P1 | **Closed.** | `rdashd/src/signals.rs:31-63, 132-151` stores `SHUTDOWN`/`REOPEN` with `SeqCst` before a nonblocking write; the byte only wakes the reader. The full-pipe test passes and both regressions in the mutation table fail. The handler uses atomic operations, `write`, and errno save/restore; it neither allocates nor locks. |
| P2a | **Closed.** | `Node::interrupt()` swaps a once flag; `shutdown()` calls it before teardown and `Drop` calls `shutdown()` (`dashd-node/src/node.rs:300-355`). A double call, direct shutdown, and Drop after `app_init_early()` each execute one notify. The guard-removal mutant fails. |
| P2b | **Closed.** | `DirLock` increments a registry owner count on each real acquisition and closes the one `fcntl` descriptor only after the last Drop (`dashd-node/src/datadir.rs:68-147`). A reviewer probe with three owners, a same-process probe, out-of-order Drops, and an external lock probe after each Drop passed. First-owner release mutant failed. |
| P2c | **Closed.** | `Node::shutdown()` removes the pid at `node.rs:334-336`, logs `Shutdown: done`, then releases the lock at `:338`. The callback at `:628-645` observes `(pid absent, external lock refused)` between those operations. Releasing first fails that test. Core: `init.cpp:504-519`. |

For P1, the relevant interleavings are: (1) reader checks before a new store, then blocks: the subsequent write wakes it, and its next loop sees the latch; (2) the reader consumes an older wake after the new store but before its write: that read sees and clears the latch, so the later wake is redundant; (3) the write returns `EAGAIN`: the pipe was full at the attempted write and already has readable bytes, so the reader loops and checks both latches. With store before write, a wake attributable to that handler cannot precede its latch store. An exact “check once after each successful read” variant remains sound and was run separately; moving both checks to a single pre-loop read fails the full-pipe test. `SA_RESTART`, no panic hook, macOS daemon reaping, and Windows behavior retain the manager's accepted round 1 disposition.

## C10 audit

`contrib/node/asserted-logs.py check --core ~/workspace/dash` passed against the pinned tag. The checked-in list has **307 rows**: 262 literal, 40 f-string, and 5 unresolved expression rows; 14 are `implemented`, 293 `pending`. The generator reads the pinned tree with `git show`; it handles direct `assert_debug_log`, the two pinned `busy_wait_for_debug_log` sites, `expected_msgs`/`unexpected_msgs`, keyword `reject_reason`, named lists, if/else bindings, tuple loop targets, the governance direct read, and `invalid_txs.py` templates. The pin has no separate `.wait_for_debug_log` call. Core's `test_framework/test_node.py:489-518, 520-553` confirms these helpers match fragments, including negative assertions. The single direct `debug_log_path` read in a test file is `feature_governance_cl.py:152-156`.

I spot-checked these **20 list rows** against pinned Core source, including each binding or helper when the assertion site held a name:

| Shape | Source rows checked | Result |
| --- | --- | --- |
| Direct and named `assert_debug_log` | `feature_config_args.py:58,159,318`; `p2p_timeouts.py:98`; `p2p_v2_transport.py:132`; `wallet_resendwallettransactions.py:72` | Six source matches, including an unexpected row and both branches of the timeout list. |
| `busy_wait_for_debug_log` | `feature_init.py:88` for `scheduler thread start` and `net thread start` | Two byte-string loop values resolved. |
| Reject reasons and templates | `p2p_invalid_block.py:84`; `p2p_invalid_tx.py:89`; `feature_block.py:181`; `p2p_permissions.py:127` | Four matches, including both `invalid_txs.py` template consumers. |
| F-strings and `.format` | `feature_addrman.py:160`; `p2p_dstx.py:150`; `feature_asset_locks.py:1286`; `p2p_blocksonly.py:45`; `feature_config_args.py:144` | Five matches with runtime placeholders. |
| Direct log read | `feature_governance_cl.py:149` (read at `:152`) | One match. |
| Unresolved expressions | `p2p_invalid_messages.py:137`; `feature_asmap.py:60` | Two rows name expressions but omit the strings they assert; finding C10-1. |

The row-flip mutant fails `the_ratchet_holds`, and a pending fragment injected into an inline test module and an `#[cfg(any(test, feature = "test-hooks"))]` item leaves it passing. Its `cfg` parser strips test-only items under nested modules and the current fixture cases. Its matching rule requires each group's pieces, in order, inside one Rust literal. The extra-emitter and inactive-platform probes below expose limitations beyond those passing cases. The local implementer gate runs the Core regeneration check and exits 0, but no checked-in CI step invokes that check (C10-4).

### New findings

1. **C10-1 — P2: concrete Core assertions remain absent from the list.** Location: `contrib/node/asserted-logs.py:146-222, 244-291` and `dashd-node/tests/asserted_log_lines.txt`'s five `expr` rows. Core references: `feature_asmap.py:35-37,60` returns two asserted strings; `p2p_blockfilters.py:245-282` puts at least three distinct asserted reasons in tuple loop targets; `p2p_invalid_messages.py:133-137` selects two strings with an `if` expression assigned before the assertion; `p2p_v2_misbehaving.py:156-171` indexes a list containing three distinct asserted strings. Probe: searched all 307 fragments by file and text; **ten distinct asserted strings** across these four files have no row, while the five expression rows are exempted from the ratchet. The parser resolves direct if/else arguments and some tuple loops, but not these bound/indexed/helper-return forms. Fix: resolve these finite forms into concrete rows, or add pinned, reviewed expansion rules for them; fail generation when an unresolved assertion remains unless its expansion is explicit. The other unresolved `feature_asmap.py:79` row is the same helper call.

2. **C10-2 — P2: an unlisted production emitter passes the ratchet.** Location: `dashd-node/tests/asserted_log_lines.rs:516-545`. Core reference: `test_framework/test_node.py:489-518` asserts the emitted log text; A1 §2.18 requires an asserted fragment to map to its emitting call site. Probe: added `tracing::info!("ND1_NEW_EMITTER_CANARY")` to production `Node::new`; `the_ratchet_holds` exited 0, and the pinned list check is unaffected. The ratchet only asks whether each existing row is found somewhere; it does not inventory emitters or enforce a row/allowlist for a new one. Fix: add a reverse emitter inventory with explicit nonasserted allowances, or state and gate the narrower one-directional contract. In particular, a new line that should be an `unexpected` fragment is not guarded by the present `sense == "expected"` ratchet filter.

3. **C10-3 — P2: the source scanner treats inactive platform code as an emitted literal.** Location: `dashd-node/tests/asserted_log_lines.rs:149-190, 448-511`. Core reference: `test_framework/test_node.py:489-518` checks the running binary's log, so only active emitters can satisfy an assertion. Probe: added a pending fragment inside a `#[cfg(windows)]` `tracing` function on Linux; the ratchet failed even though rustc excludes that function. The synthetic `any(test, test-hooks)` and nested-test-module probes passed, so this is specifically non-test cfg evaluation, including platform and ordinary feature gates. Fix: evaluate cfg for the tested target and enabled features, or make list status target/feature specific.

4. **C10-4 — P2: Core/list synchronization is not in a checked-in gate.** Location: `contrib/node/asserted-logs.py:370-380`, `.github/workflows/build-and-test.yml`, `.github/workflows/rust.yml`. Core reference: `v24.0.0-rc.3:test/functional/` is the generator input. Probe: `check --core` passes locally and appears in `/work/scratch/nd1/c10-gate.sh`, but neither checked-in workflow nor `contrib/test.sh` calls it; the Rust ratchet reads only the checked-in text. A Core pin/list drift can therefore pass committed CI. Fix: add a CI step that fetches or checks out the pinned Core source and runs the script's `check` mode.

## Mutation results

Each code mutant was applied alone in the detached checkout, tested, restored, and its file touched. Exit 101 below is a target assertion failure, not a compilation error. The worktree diff was empty afterward.

| Mutant | Test | Result |
| --- | --- | --- |
| Write wake without setting shutdown latch | `a_full_pipe_keeps_the_shutdown` | Killed, 101 |
| Consume latches only before entering the wake loop | `a_full_pipe_keeps_the_shutdown` | Killed, 101 |
| Consume latches once after each successful read | `a_full_pipe_keeps_the_shutdown` | **Survived, 0**; sound with latch-before-write and a full pipe's queued bytes. |
| Remove `interrupt()` once guard | `interrupt_runs_once_on_every_path` | Killed, 101 |
| Release lock on first owner's Drop | `the_lock_is_released_by_its_last_owner` | Killed, 101 |
| Release lock before removing pid | `the_lock_outlives_the_pid_file` | Killed, 101 |
| Echo the non-UTF-8 argv value | `a_non_utf8_argument_is_refused_without_its_value` | Killed, 101 |
| Flip one implemented ratchet row to pending | `the_ratchet_holds` | Killed, 101 |
| Add production `tracing` line with no row | `the_ratchet_holds` | **Survived, 0**; C10-2 |
| Add pending literal in a Windows-only emitter on Linux | `the_ratchet_holds` | False failure, 101; C10-3 |

## Validation and limits

- Restored checkout: `cargo build -p rdashd`; `cargo test -p dashd-node -p rdashd --no-fail-fast` passed (90 node unit tests, 5 asserted-log tests, 4 rdashd unit tests, and 3/6/7 integration groups). `cargo test -p rdashd --features test-hooks --test hooks` passed all four. The package suite passed both before and after the requested mutations. The final ratchet test passed after the cfg probe was restored.
- The 75-argument `init_errors_match_dashd_v24` test ran separately with `DASHD_V24_PATH` explicitly set to the pinned binary and `SKIP_DASHD_TESTS` unset, then ran again in the package suite. Both passed; the test did not skip. The case count is 75 from `CASES`, and `find_dashd()` verifies the binary's version.
- The final rebuilt process canaries checked raw bytes, lossy text, and both streams for `-rpcpassword`, `-conf`, `-datadir`, `-includeconf`, and a config-file value. The three-owner and test-literal-immunity reviewer probes passed. `asserted-logs.py check --core` passed. The implementer's gate copy reports format, workspace clippy, bench, hooks, and workspace tests with `GATE EXIT 0`; those were not independently repeated here beyond the targeted commands above.
- Linux process behavior and the source-level C10 audit were covered. The manager accepted macOS/Windows process validation deferral under plan §11b. No implementer files or branch were changed, and the detached review checkout was clean before removal.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
