# CH1-node round 5 — independent Sol review

Reviewed only `73a22f6e0..f55a9fc97` (the two N4 closure commits) in a detached worktree at `f55a9fc97` under `/work/scratch/ch1-node-sol-r5`. The approved `73a22f6e0` baseline was excluded. Core reference: Dash Core v24.0.0-rc.3 (`0364ed626e`), especially `src/node/chainstate.cpp` (`cs`) and `src/validation.cpp` (`v`). No implementer worktree, branch, or commit was edited.

## Findings

No new findings. Both r4 findings are closed, and the delta introduces no production behavior change.

## Closure table

| Item | Status | Evidence |
|---|---|---|
| F1: D39 ruling and ownership | **CLOSED** | `docs/node-program/designs/CH1-node-notes.md` now records D39 as **Ruled: option (b), BL-119**, says V matches Core only at `-checklevel ≤ 2`, and identifies the default-level difference. `node/integration` commit `69b2392d9` carries BL-119 in `docs/node-program/BACKLOG.md`: CH5 owns Core's post-VerifyDB `ResetBlockFailureFlags(nullptr)` at `-checklevel >= 3` plus the default-checklevel regression. |
| F2: retained D38 release-point regression | **CLOSED** | `held_commands_run_at_a_deferred_commands_release_point` stages one held command, queues one newer lane command, invokes the release point, and deterministically asserts `["held", "lane"]`. The focused test passes on the clean head and fails under G4 with `["lane", "held"]`. |
| Test seam isolation | **CLOSED** | `V::stage_held` is gated `#[cfg(all(test, not(loom)))]`. The chain test module is itself excluded by `#[cfg(all(test, not(loom)))]`; the loom build therefore has neither the seam nor the test reference, and the explicit loom model build passed. Non-test builds cannot compile the method. |
| Production delta | **CLOSED** | `git diff --stat 73a22f6e0..f55a9fc97` is 6 lines in `actor.rs`, 19 test lines in `failure.rs`, and one documentation row edit. The `actor.rs` addition is test-only under the cfg above; no production path changed. |

The D39 ledger is accurate against Core v24.0.0-rc.3: `cs:463-465` calls `ResetBlockFailureFlags(nullptr)` only when `options.check_level >= 3`; `v:4155-4159` selects `m_best_invalid` for the null reset. The ledger's `v:3528-3529`, `v:3812`, `v:4233-4263`, and `v:5755` citations describe the corresponding no-reset path correctly. It explicitly states that V matches Core only at `-checklevel ≤ 2` and cites BL-119 for the default-level work.

## Mutation table

| Mutant | Witness | Result |
|---|---|---|
| G4: release points skip `held` (`actor.rs:763`) | `chain::tests::failure::held_commands_run_at_a_deferred_commands_release_point` | **Caught.** The mutant compiled and failed the assertion with `["lane", "held"]` instead of `["held", "lane"]`. I restored `actor.rs` with `cp -p`, ran `touch`, and `cmp` returned zero. The post-restore test rebuilt `dashd-chainstate` and passed. |
| G5: arm the hold on every tie (`index.rs:254`) | all-features chain suite (86 tests) | **Survives; accepted as unobservable under the declared startup contract.** The mutation only widens the hold for equal-work loads whose retained candidates already outrank the failed tip. The startup ABC is exempt and is the required startup action; no in-model non-exempt command is allowed to depend on running before it. A test could observe extra delay/order only by deliberately queuing a non-startup command before that ABC, which violates the ND1/CH5 startup-driver contract. |

## Validation and limits

- `cargo test -p dashd-chainstate --all-features 'chain::'`: **86 passed**.
- `cargo test -p dashd-chainstate 'chain::'`: **86 passed**.
- `cargo test -p dashd-chainstate`: **277 unit tests, 2 quirk integration tests, and 2 doctests passed**.
- `RUSTFLAGS='--cfg loom' cargo test -p dashd-chainstate --release --lib -- 'chain::signals::fifo::loom_tests::' --test-threads=1`: **1 loom model passed**.
- `cargo fmt --all -- --check`: passed.
- `cargo check -p dashd-chainstate --all-features --lib`: passed.
- `cargo clippy -p dashd-chainstate --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy -p dashd-chainstate --all-targets -- -D warnings`: passed.
- `git diff --check 73a22f6e0..f55a9fc97`: passed.

Core verification was source-level at the v24.0.0-rc.3 tag; I did not rerun the prior live rc3 regtest reproduction. The isolated worktree and its target were removed after the checks.

**Verdict:** APPROVE
Sol gate satisfied: yes
