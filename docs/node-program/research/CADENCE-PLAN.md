# Train cadence plan (DEC-707 draft, 2026-10-29)

The data comes from `/work/scratch/mgr/cadence/T43-46-timeline.md`, which is built from unit journals, perfgate reports and merge-commit times.

## 1. Where the time goes (cut → merge)

| Train | Cut→merge | PF0 queue wait | PF0 measure | Tier 1 + --final + fuzz build | Slack before PF0 starts | Ruling latency |
|---|---|---|---|---|---|---|
| T43 | 6.96 h | 5.04 h | 1.71 h | ~1.5 h, in parallel | 3.47 h | minutes |
| T44 | 4.24 h | 1.32 h | 2.42 h | ~1.1 h, in parallel | 0.20 h | minutes (noise ruling) |
| T45 | 5.30 h | 3.45 h | 1.70 h | ~1.8 h, in parallel | 1.69 h | minutes |
| T46 | 7.16 h | 2.63 h | 3.92 h (2 runs) | ~1.5 h, in parallel | 1.11 h | minutes (rerun + noise ruling) |

- **PF0 is about 96% of the critical path (22.6 of 23.7 h)**, and queue wait is the biggest part: on average 3.1 h of wait against 2.4 h of measuring.
- The PF0 lane is a single serial lane and was about 98.6% busy. **Task PF0s and anchors used 13.7 h of it, against 9.75 h for the trains' own PF0s.**
- Tier 1 already runs in parallel and finished before PF0 started in every train. **Tier 1 is not on the critical path.**
- Cuts came 0–3 min after the previous merge, and manager rulings took minutes. Neither batching nor review is the bottleneck.
- Avoidable losses:
  - T46 needed a full PF0 rerun (~2.2 h) after the T45 merge missed an anchor obligation. DEC-692 has since fixed that in the process.
  - T43 probably lost ~2.9 h to a 60 s polling race for the lane (est.).

## 2. What GitHub CI removes, honestly

- **It removes almost nothing from the critical path directly.** Tier 1 was never the long pole. PF0 can't run on hosted runners, because they have no perf counters.
- **What it does give:**
  - about 1.1–1.8 h × most of the box's CPU per train, plus every task's tier 1, comes off the box;
  - lower load, so fewer noisy PF0 rows (all 5 recent PF0 runs escalated a noisy row);
  - free gate slots.
- The noise rulings cost minutes, not hours. **Expected cadence gain from GitHub alone: ≤ 0.5 h per train.**

## 3. Options that cut the PF0 critical path

| | Option | Expected saving per train | Risk / cost | Who decides |
|---|---|---|---|---|
| c | **Stop standalone task PF0s; the train PF0 measures every rider** (already done for BL-271 and EV1-3a, DEC-686/688). Replace the 60 s poll with a FIFO flock queue. | Queue wait 3.1 h → ~0.3 h (the 13.7 h of task PF0s leave the lane) | A train-level regression has to be bisected among the riders (~2 h, only when it happens) | manager, now |
| b | **Per-package PF0:** measure only the workloads whose bench package depends on a crate the train changed. Use the reverse dependency closure of the changed crates' library code. **Not** "skip when bench_inputs are unchanged": bench_inputs deliberately exclude library `src/`. Run the full PF0 when Cargo.lock, a profile, features or the toolchain change, and nightly on integration. | Measure 2.4 h → ~0.5–1.5 h, depending on which crates move | A codegen shift outside the closure would show up only in the nightly full run. Needs a small perfgate selector (SB1, ~half a day) | manager, now |
| e | **Continuous merges:** cut as soon as a rider is READY and the lane is free, without batching | Removes the 0.5–4 h that a rider waits for the next cut | Without d, each merge costs a PF0 run, so this only helps together with b+c, or with d | manager, now |
| a | **A second PF0 lane:** a separate PERFGATE_ROOT on cpuset-isolated cores here (e.g. 2 lanes × 8 cores, --jobs 4), or a dedicated quiet benchmark VM on the R730 (M-11) with PMU passthrough, which also allows a higher --jobs | Two lanes halve the queue wait. A quiet VM also cuts noisy rows and measure time (~30–50%) | Here: co-running lanes add page_fault/syscall noise, and each root needs its own anchor. VM: setup, and its counters need to be checked against this box | **pasta** (VM); manager (cpuset trial) |
| d | **PF0 after the merge:** merge on green GitHub tier 1 + review, then run PF0 on each integration merge asynchronously. If a gated row regresses beyond noise, the owner has 4 h to fix forward, or the manager reverts the merge. | Takes PF0 off the merge path entirely. Cut→merge ≈ GitHub tier 1 (~1–1.5 h with a warm cache) | A regression sits on integration for up to ~4–8 h, and follow-merges inherit it. If merges outpace PF0, one PF0 covers several merges and needs a bisect. Needs b (and ideally a) so PF0 keeps up | **pasta** |

## 4. Recommendation and target

- **Now, manager-decidable:** c + b + e, the FIFO lane lock, and the anchor-obligation step (already in PROCESS).
  - Expected cut→merge ≈ max(GitHub tier 1 ~1–1.5 h, a selected PF0 ~0.5–1.5 h) + ~0.3 h queue ≈ **2–2.5 h**.
  - **Target: one merge every ~2.5 h** with no pasta decision needed.
- **With pasta's yes on d (and a):**
  - PF0 leaves the merge path, and each rider merges ~1–1.5 h after READY plus review.
  - **Target: a merge every ≤ 2 h, as riders become ready (6–10 a day).** PF0 lag stays ≤ 4 h under the revert rule.
- **What I need from pasta:**
  1. **(d)** yes or no, and if yes, the revert rule: 4 h to fix forward, otherwise revert.
  2. **(a)** a benchmark VM on the R730 (PMU passthrough, `cpu: host`, ~8–16 cores, ~32 GB, ~200 GB disk). Or say no, and I trial the cpuset second lane here first.
