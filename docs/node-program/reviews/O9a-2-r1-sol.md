# O9a-2 independent Sol review — round 1

Reviewed `4acf5e2ef..8d7025c6b` (emit and `DigestTap`) and `6f2edca53..7a9b1a125` (replay) at exact head `7a9b1a125`, in a detached worktree. O9a-1 is outside scope. I read O8 §3–§10 and §12–§13, the O9a D-rows, both O9a-1 Sol reviews, the checkpoint plan, and the local Core patch's `statedigest.cpp` without modifying it.

## Findings

### Medium

1. **Unchecked stored files can make the all-node parity assertion vacuous.** `conformance/src/digest/replay.rs:124-128` reads `blocks.dat`, digest and decisions without `Node::verify`; `conformance/tests/digest_replay.rs:123-140` derives both the expected tip and comparison range from the same unchecked golden. In a copied `feature_dip3_deterministicmns/node0`, I retained only the genesis frame in `blocks.dat` and `H` plus genesis `C` in the golden, leaving `extract.json` untouched. Replay exited 0 with zero handed blocks. Compare exited 3, as incomplete coverage requires, but showed one pair, no header mismatch and all eight header keys equal; the test helper's tip, count and coverage assertions would also pass for that pair. Both edited files disagree with their existing `extract.json` hashes, and its summary still says tip 245 and 246 blocks. The original full golden correctly diverged at height 1. Bind replay and its parity test to `Node::verify` and/or independently check the extract summary's tip and block count before claiming 77 chains match.

2. **The input chain admits an empty file and repeated genesis without an error.** `conformance/src/digest/replay.rs:75-95` returns an empty block list as valid, `:204-207` silently skips *every* genesis occurrence, and `:244-246` treats the resulting genesis tip and handed count as success. An empty `blocks.dat` returned exit 0 and a genesis-only digest for a node whose stored chain ends at height 245. Inserting a second genesis frame into the otherwise intact file also returned exit 0 and produced a digest byte-identical to the unmodified replay. This violates the claimed rejection of repeated/duplicated blocks and can hide a malformed replay input. Require exactly one genesis frame at position zero and a nonempty, contiguous input before feeding V; checking the stored file hash would also reject altered corpus files.

### Low

3. **The fail-stop replay check has no exercising test.** `conformance/src/digest/replay.rs:236-238` correctly fails when the latch holds a reason, but `:261-270` tests only the latch data structure. Replacing the driver check with a constant `None` survived all nine `digest_replay` tests, including the AbortNode test. A future regression could report success after a latch trip. Add a test seam that trips the latch during `run` and asserts the first reason and removal of `out`.

4. **Cleanup silently ignores unlink failures.** `conformance/src/digest/replay.rs:134-138` discards `remove_file`'s error. If the output directory becomes non-writable after creation and V then rejects a block, the command reports the replay failure while its partial digest remains. Ordinary relative and absolute path failures do remove the file; this is the remaining failure path. Surface the cleanup error and avoid claiming unconditional removal when unlink fails.

## Claims checked

| Claim | Result and evidence |
|---|---|
| Emit fidelity | The eight `C.s` header keys use the O8/Core order and types; `prev`, `mrkl` and `work` use display-order hash text, and the actual Core patch uses the same class names and `R`/`D` result strings. `Record::to_line` gives the canonical encoding. All eight header values matched the Core golden at every golden height in the 77-node replay test. `R`/`D` parity against real Core attempts is untested because `blocks.dat` contains the final chain only. Derived, golden-only and `fx` values are absent, so a wrong value there remains uncovered, not equal. |
| `DigestTap` transparency and feature gate | The decorator returns the inner `ConnectOutcome`, `bool` or `Fatal` after recording, except that its own read/encoding/write failure becomes `Fatal` as specified. The reorg test checks `C`/`R`/`D` order and numbering; the replay log-write test checks AbortNode. `digest_tap` is behind `#[cfg(feature = "statedigest")]`, the dependency is optional, and no self dev-dependency enables it. Only `dash-conformance` enables the feature in its manifest; a feature-off `dashd-chainstate` library build passed. |
| 77 of 81 stored nodes | Counted 81 node directories: 77 single-parameter and four under `feature_mnehf` / `feature_evo_shares_activation`. The exact-head focused test passed: for the 77, final tip/height and every one of the eight header keys match every golden height from its first. The four are refused. The unchecked-file finding limits what this proves about later stale copies. |
| Clock and rejection handling | The `p2p_invalid_block` clock test passed; mutant M1 was killed. Dropped, transaction-mutated, repeated-last and duplicate-non-genesis cases fail with no output in the committed test; mutant M2 was killed. Duplicate genesis is the exception above. The AbortNode log-write test passed; latch storage is tested but the replay latch branch is not. |
| Exit codes and cleanup | Valid replay exits 0; replay failures and malformed CLI inputs exit 2. An unmodified replay against its full golden exits 3 for the uncovered keys; a shortened replay against the full golden exits 1 at the missing height. Existing-output protection passed. An independent relative-path invalid-block replay exited 2 and removed its output. Unlink failure remains unchecked. |
| Gate copy 4672/11, BL-20 rerun 54/0, fuzz smoke 50/50 | Not independently rerun or audited; the requested focused checks below passed. |
| Implementer's 19 mutants | Not independently audited. The five independent mutants below were run against exact-head copies. |

## Independent mutations

Each mutation was applied alone from a saved exact-head file; after each test I restored with `cp -p`, ran `touch`, and verified `cmp -s` equal. The final tracked tree was clean.

| Mutation | Focused test | Result | Survivor assessment |
|---|---|---|---|
| M1: set every block's clock back to the initial time | `a_mocktime_chain_replays_only_on_the_recorded_clock` | Killed: test failed | — |
| M2: omit the final tip-height comparison, retaining only tip hash | `a_block_v_does_not_connect_fails_the_replay` | Killed: test failed | — |
| M3: accept any array for `other_params_runs` | `params_come_from_extract_json` | Killed: test failed | — |
| M4: replace the replay's latch reason with constant `None` | all nine `digest_replay` tests | Survived: 9 passed | Real test gap: no test trips the latch during replay. The production check is present. |
| M5: emit the block time as `C.s.nonce` | `a_recorded_chain_replays_through_v_onto_its_golden` | Killed: test failed | — |

## Validation and limits

- `cargo test -p dash-conformance --test digest_replay`: 9 passed, twice (baseline and restored sources).
- `cargo test -p dash-statedigest`: 109 passed across unit and integration suites; `--test emit` alone: 2 passed.
- `cargo test -p dashd-chainstate --features statedigest --lib digest_tap`: 1 passed.
- `cargo clippy -p dash-statedigest -p dashd-chainstate -p dash-conformance --all-targets --features dashd-chainstate/statedigest -- -D warnings`: exit 0.
- `cargo check -p dashd-chainstate --lib --no-default-features`, `cargo fmt --all -- --check`, both range `git diff --check` calls, and `contrib/node/check-pure.sh`: exit 0; pure check reported 8 crates and layering OK.
- Independent CLI probes used copied local vectors only: empty input, repeated genesis, shortened corpus pair, and relative-path failure. No full gate, fuzz run, GitHub, live oracle, implementer worktree or implementer target was used. CH3a's real `ConnectBlock` is not present; this validates V with `NullConnector` and the header tap only.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: yes
