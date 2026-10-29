# ST3a CP3.6 — narrow Sol review

Reviewed only `10dd298c8..7b42e6d63` in a detached worktree at `7b42e6d63`. **CP3.5-1 is closed for the equal-and-opposite cross-bucket nested miscount it identified.** The implementation's accounting is correct, including depth 2, but one new regression coverage gap prevents a full Sol gate.

## Closure and code review

- `search` snapshots `branches` after the outer branch increment and resets `r7a` around each `self.nested(...)` call. Its delta includes every reported depth-1 branch, sampled branch, and depth-2 branch. Each call merges that delta once into `(img.at.index, kind)`. Depth-2 branches have no R7a judgment, as before. In an independent depth-2 S1 event-53 probe, the buckets held **374 nested branches** (86 R7a judgments plus 288 depth-2 branches); bucket branches plus the five outer branches equaled the global 379 branches, and bucket R7a summed to global R7a. The same conservation assertions passed for every depth-2 shard at depth 2.
- The R7a restoration is exact: `mem::take` saves the prior counts and starts the nested call at zero; `mem::replace` captures only that call's counts and restores the prior counts; `merge` adds the captured counts once. The depth-2 full and shard probes independently reconciled every R7a field with its buckets.
- `fail_stop_scan`, `sanctioned_stop`, `r7a_judge`, `Report::is_clean`, and `Report::summary` have no diff in this range. `7b42e6d63` changes only I43 wording. The accounting does not alter verdict decisions or ledger rows, so the audited `8833a8d2f` campaign remains applicable; I did not rerun its 9,440 shards.
- The named API changes add `Report::per_image_nested`, `NestedTotals`, its re-export, and public `merge` helpers; no existing function signature or verdict API changed. A downstream crate constructing the public `Report` with a complete struct literal would need to add the new field, though this repository has no such construction.

## Finding

### CP3.6-1 — Minor: no independent assertion of bucket owner

`dashd-store/tests/power_loss/shard_check.rs:58–63` compares summed shard buckets against buckets from the full search. It never checks that a shard's bucket key equals that shard's `(image, kind)`. A one-line production mutation at `dashd-store/src/faulty/harness.rs:1197` from `(img.at.index, kind)` to `(img.at.index + 1, kind)` misfiles **every** nested branch under the wrong outer image in both searches. `shards_own_each_nested_search_once` and `cp35_shard_check_catches_a_nested_miscount_between_buckets` still pass. My independent assertion that every nonempty shard bucket has its shard's owner fails on `(54, ProcessKill)` for the event-53 shard. This is a regression gap, not an observed defect in the submitted accounting or an I43 verdict change. Pair each planned shard with its report and assert its nonempty nested keys equal `(shard.image, shard.kind)`; then the existing full-versus-shards comparison also checks the full report's attribution.

## Mutation table

| Mutation in disposable review checkout | Global branches and R7a | Result |
| --- | --- | --- |
| Move one converged depth-2-probe nested branch and its R7a count from `(53, ProcessKill)` to `(53, PowerLoss)` in a shard report | Unchanged | New per-bucket assertion fails as intended; independent test catches the expected panic. CP3.5-1's original failure mode is closed. |
| File shard reports under `image + 1`, leaving the full report's key correct | Unchanged | `shards_own_each_nested_search_once` fails at the per-bucket check at depth 0. |
| File both full and shard reports under `image + 1` | Unchanged | Shipped S1 shard regression and CP3.5 planted-miscount regression both pass; independent shard-owner assertion fails. CP3.6-1. |

## Validation and cleanup

Baseline release tests with isolated `CARGO_TARGET_DIR` and Nice 19: `power_loss_cp33_witness` **22/22 passed**, and the two `power_loss_toy` shard regressions **2/2 passed**. The temporary depth-2 conservation and cross-bucket probes passed. `git diff --check 10dd298c8..7b42e6d63` passed. Every mutation and temporary test was restored; the detached checkout was clean before removal. The implementer's worktree, branch, campaign artifacts, and remote were untouched.

**Verdict:** CP3.5-1 CLOSED; CP3.6-1 Minor regression coverage gap remains.
Sol gate satisfied: no
