# CH2b commit 6, round 2 — Sol independent review

Target: `b33ab7afd..b86a69573` on `node/ch2b-crash`, reviewed in a private detached worktree. Both range commits and the base commit report `G` signatures. The worktree was clean at the pinned head before review. This is a narrow review of DEC-196's warranted `INVALID_TIP` refusal and the round-1 R13 finding; no private Core-side material was read.

## Round-1 Medium closure

**Closed.** `ChainSut::open` now calls `sut.effects(&trace)` inside `world.checking` with the refusal's R1/R5 and booted R2 checks (`dashd-chainstate/src/crash/chain.rs:989-999`). A lost covered effect changes the sanctioned refusal into R0 whose detail names R13. The checked-in ordinary `LostEffect.repro` reports R13; `LostEffect-refused.repro` reports R0 naming R13, and both fail their unplanted control. The two targeted tests and `checked_in_planted_repros_replay` passed. Removing just the new `sut.effects` call made `lost_effect_under_a_warranted_refusal` fail because R0 was absent; the witness then found R13/R12 only on a later successful reopening/continuation. The new `LostEffect` hook is gated by `planted-bugs`, after recovery and before activation, and is a probe rather than a claim of a V bug. The x3/x4/x5 refusal regression passed unplanted with exactly one `INVALID_TIP` log each.

## New finding

| Severity | Location | Evidence and fix |
|---|---|---|
| **Medium — refusal-only R3 false negative** | `dashd-chainstate/src/crash/chain.rs:989-1001`, `:1120-1166`; the incomplete equivalence is documented at `src/crash/mod.rs:40-43` | The refusal's checks still skip the recovered coins and evo comparison against `ChainRef`. I injected a Durable deletion of one `coins` record in `ChainSut::open` after recovery, guarded by `rec.found.must_refuse()`. On `c5;f;x3` the adjacent pre-index image opened, was exhaustively clean (31 images, 240 branches), and never ran the deletion. On `c5;f;x3;i`, the failed-tip condition ran the deletion exactly once, the restart refused exactly once, and exhaustive search still reported no violation (6 images, 13 branches, no truncation). A control with the *same deletion unconditionally* yielded `R3: coins differ from the reference at the recovered tip` on the pre-index reopening, while the post-index refusal still reported `[]`. Thus the preceding full check does not judge code that executes only when the durable index marks the loaded tip failed. This is a model oracle gap, not evidence that current production recovery loses a coin. **Fix:** make the sanctioned-abort path run the reference-dependent recovered-state checks with the harness's `ChainRef` (at minimum recovered tip/coins/evo R3/R4), while excluding activation's expected refusal outcome. Add the conditional coin-deletion witness as a regression. |

## Audit and refusal-write argument

- **R1/R5 run** through `check_stores`, and booted **R2 runs** through `store_check`; **R13 now runs** through `effects`, all in checking mode. `may_refuse` gates the sanctioned DEC-196 abort (R6(e)). Activation returns before any connect in that case, so R6(c) is vacuous.
- `check()` does not run after `open` returns the refusal. This skips R3/R4, R6(a), R6(b), and the invalid-marked-valid check, which use `ChainRef`. It also skips two *recovered* R2 clauses in `check_tip`: `head_blocks` still present and `evo_tip != tip` (`chain.rs:1122-1129`). The booted R2 check constrains the latter in the current no-marker/no-write path, but saying that only the head clause is structurally skipped is too strong. C9 is also skipped, although its checker (`chain.rs:1377-1393`) takes only the trace and epoch, so lack of `ChainRef` is not its reason. With no recovery append here, its replay-batch condition is vacuous.
- **The current unmutated recovery-writes-nothing premise holds for reachable warranted refusals.** `x` disconnects before setting failed marks, and `i` persists those marks without flushing coins. A flush after `x` names V's now-valid active tip; its marker, if interrupted, replays to that valid `new` tip and does not yield this refusal. An interrupted wipe leaves no tip; the refusal needs one. For a no-marker tip, recovery's step 5 selects `Consistent`, then `End`, with no append (apart from step-0 `sync_wal`, which does not change logical records). A temporary trace probe on killed `c5;f;x3;i`, `x4;i`, and `x5;i` observed exactly one reopened recovery and no `Append` between its `RecoveryBegin` and `RecoveryEnd`. This did not expose a current reachable refusal image on which recovery changes records.
- **The inference from that premise fails.** The pre-index image may be clean while the reopened failed-tip state takes a different startup branch and damages state after recovery. The mutation above demonstrates such a refusal-only defect hidden from every adjacent successful `check`; therefore the manager's stated condition for accepting this gap is not met as a general model argument.

## Mutation table

Each mutation was applied alone from a saved original of `chain.rs`, then restored; `cmp` and a clean `git diff` confirmed restoration.

| Mutation | Result |
|---|---|
| Remove refusal's `sut.effects(&trace)` call | **Killed:** `lost_effect_under_a_warranted_refusal` failed for missing R0, despite later R13/R12. |
| Durable-delete one coin after recovery only when `found.must_refuse()` | **Survived:** pre-index full check clean, post-index refusal clean, deletion logged once on the refused restart. |
| Durable-delete the same coin unconditionally when present | **Caught on ordinary restart** by R3; **survived on warranted refusal** with `[]`. This is the control showing that R3 detects the defect when invoked. |

## Validation

Passed at the unmodified pinned head: `cargo test -p dashd-chainstate --lib` (177); `--features crash --test crash` (29); `--features alloc-tag,crash,planted-bugs --test crash_planted` (36 passed, one ignored generator); `--test test_feature_scope` (1); `cargo fmt --all -- --check`; and `cargo clippy -p dashd-chainstate --all-targets -- -D warnings` with default, `crash`, and `alloc-tag,crash,planted-bugs` features. The checked-in LostEffect repros replayed via the planted suite. A warmed rerun of the three PR-budget suites used 22.29 + 11.29 + 0.77 = **34.35 user CPU-s** (36.62 with system time), below the ≤60 CPU-s budget and consistent with D-c6-11's approximate 42 user CPU-s claim. Temporary probe files were removed and the source worktree was clean after the mutations. No full workspace, physical power-cut, or Core-side test was run.

**Verdict:** CHANGES-REQUIRED
Sol gate satisfied: no
