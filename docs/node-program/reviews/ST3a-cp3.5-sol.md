# ST3a CP3.5 — narrow Sol review

Reviewed `86925887d..10dd298c8` in a detached checkout at the requested head. `8833a8d2f` is the code head; `10dd298c8` adds only I43's measured campaign figures. I read the CP3.4 report and the complete range diff, and applied the code-review-validator workflow. **The ownership and resume defects are functionally closed, and I43's figures match the ledger. One required multiplicity regression is incomplete.**

## Findings

### CP3.5-1 — Minor: the per-image/kind regression measures only outer branches

`dashd-store/tests/power_loss/shard_check.rs:33–40,65–71` compares `Report::per_image` and labels it “outer branches per image and kind.” That field is populated with the outer DFS count `n` at `dashd-store/src/faulty/harness.rs:1181–1182`; each nested branch increments only `Report::branches` at `:1311`. Thus the test checks outer multiplicity by image/kind, every **global** R7a counter, global total branches, and distinct findings, but it does not compare **nested branch multiplicity per outer image and kind** as the CP3.4 closure request specifies. Equal and opposite nested count errors in two image/kind buckets would pass these assertions if their R7a verdict classes and finding strings also match.

The actual `owned_before` algorithm and the checked cases passed independent review; this is a regression coverage gap, not an observed wrong verdict or omitted campaign branch. Add per-image/kind nested totals to the report or rerun each scenario narrowed to one outer image and kind, then compare its full and summed-shard `branches` and R7a counters. Keep the present global and distinct-finding checks. This explicit regression requirement remains open.

## Closure

| Item | Result | Evidence |
| --- | --- | --- |
| CP3.4-1, ownership mechanism | **Closed in code** | `owned_before` replays every outer branch before a shard's first branch, records its recovered key, and makes the earliest DFS representative the sole nested-search owner. The same `nested_key` is used in replay and `search`. The S1 event-53, Proj, Restarter, and R12 regressions pass. The per-image/kind nested regression gap is CP3.5-1. |
| CP3.4-2, resume | **Closed** | `read_last` drives both skip and final verdict. The seeded older-`ok`/latest-`FAILED` case reruns exactly that shard; independent stubs reject failures, absent `SHARD` results, and absent passing-test lines. |
| I43 campaign at `8833a8d2f` | **Verified against the supplied artifacts** | The exact 9,440 planned IDs occur once each in 9,440 `ok` ledger rows. Each row matches its shard log, successful one-test line, and time file. Totals: **189,034,654 branches; 0 findings, 0 truncated, 0 unjudged**. Five fresh Nice 19 spots match their rows. |
| DEC-161/162/165/185 verdict rules | **No new change found** | The range changes owner selection and runner resume, not `fail_stop_scan`, `sanctioned_stop`, `r7a_judge`, or `Report::is_clean`. The unmarked-fatal-return and sampled-truncation controls pass. Sampling still cannot certify a clean report; no abort mark is synthesized by this range. |

For a deterministic adaptive outer decision tree, `Dfs::limited(depth)` yields one prefix per outer-leaf class, and `Dfs::pinned(prefix)` visits that class only. Each recovered key has a first outer leaf in DFS order, and that leaf has one shard. `owned_before` gives later shards that key before their first branch; the owner shard starts without it and its local `nested_seen` admits it once. A standalone probe imported the checkout's actual `decide.rs` and compared outer-leaf multisets and nested `(key, canonical first representative)` maps over **300 adaptive trees × depths 0–8 = 2,700 comparisons**. All passed; 4,505 key/depth cases crossed shard boundaries. This tests the ownership formula independently. The concrete harness regressions exercise the private `owned_before` implementation.

The S1 event-53 `Extended`/`Nested::Depth1` search has **91 branches** at depths 0, 1, 2, 4, and 16 under both v23.1 and v24.0, including the R7a counters and findings. Proj fixed has **69**, Proj bug **54**, each at depths 0–4 and 16; Restarter ok/bad have **222** at the same depths; the R12 window has **37**. The S1 ownership shard `e53-pl-0.0` reports 53 branches and its non-owner sibling `e53-pl-0.1` reports 1, exactly as the ledger says.

The I43 R7a totals are converged **170,674,168**, sanctioned fail-stop **16,917,738**, recovered after fail-stop **1,412,172**, fail-stop after fail-stop **260**, both witnessed counters **0**. Their sum is **189,004,338**, plus **30,316** outer branches. Both releases have **94,517,327 branches** and identical R7a counters in each component. I43's rounded CPU total **131,479 s** matches the ledger's **131,479.13 s**. The older campaign's S1, S2, S4b, and S7 branch totals fall as I43 states; its three fail-stop counters are unchanged. A fresh binary regenerated a byte-identical depth-4 plan.

## Mutation checks

| Single production mutation, then restored | Focused assertion |
| --- | --- |
| Disable `owned_before` in `search` | S1 event-53 fails at depth 1: R7a converged **138 vs 86**, exit 101. Proj fails at depth 1: converged **54 vs 36**, witnessed-after **36 vs 18**, exit 101. Both pass again after restoration. |
| Revert resume to historical-any-`ok` skip | `cp33_shard_runner_ledger` fails at its seeded case: `NOT OK A-V23_1:e1-pk-1 FAILED`, **2/3** ok, exit 101. It passes after restoration. |

## Validation and limits

- `cargo test --release -p dashd-store --no-default-features --features faulty --lib --no-fail-fast`: **39 passed, 0 failed, 1 ignored**. All eight focused regression/control invocations passed before mutation. `cargo clippy --release -p dashd-store --no-default-features --features faulty --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
- Independent runner stubs: passing test, failed test, no `SHARD` result, no passing-test line, latest-failed resume, missing row, and another head all produced the expected fail-closed status or rerun. Five fresh spots include S1 owner/non-owner shards, both releases, and S2 shards from both releases; all exited 0 and match ledger rows exactly.
- Cargo builds and long tests used Nice 19, an isolated target, and `/work/scratch/st3a-cp35-sol/tmp`. The initial default-feature debug build was interrupted while compiling RocksDB; the complete release/no-default-feature library gate and independent release test binaries succeeded. The **36.5 CPU-hour** campaign was audited, not rerun. Ledger figures prove its recorded results and the five independent spots, not every execution afresh.
- The detached source tree was clean after both mutations were restored. Review evidence is under `/work/scratch/st3a-cp35-sol/`; the worktree and target are removed after this report is finalized.

**Verdict:** REQUIRES CHANGES — close CP3.5-1's per-image/kind nested multiplicity regression.
Sol gate satisfied: no
