# B1 review, round 2 — GPT (focused)

**Verdict: CHANGES-REQUIRED**

Reviewed `node/bench-harness` at `f7a03e8f82fd3ed4eb7fa7ba774dcec4db3153f1`, worktree `/work/home/agent/workspace/rdc-wt/bench-harness`, against round-1 head `288c2a78c4e64df79120f92feee4a8d6b44f55ea`: 13 commits, 27 files, +2,761/−608. Read the round-1 report, the actual diff, current implementation/tests, DEC-23, baseline script, queued v24 launcher, existing markers and queue logs.

The original missing-metric blocker is fixed. Missing/NaN measurements, missing whole scenarios/rounds, explicit configuration differences, and normally labelled provisional runs now block an official PASS. There are nevertheless two reproduced false-PASS paths (confirmation and legacy official provenance), plus incomplete fixed-work endpoint enforcement, unchecked SPV exit status, and an incomplete protected-path boundary. These are five majors; no remaining finding is classified as a blocker.

No branch files were changed; no commits were made. All independent probes and stub artifacts are under `/work/scratch/b1-r2/`. No real benchmark, dashd, perf workload, cache drop, cgroup change, or operation on `/work/chains` was started. No signals were sent to PIDs 686828, 2731878, 4000276 or 3707248. Those PIDs were absent from the review environment's process snapshots; the logs and queued launcher were inspected read-only.

## Validation

- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s node-bench/tests -v`: **73 passed**. Saved output: `/work/scratch/b1-r2/unit-tests.log`.
- `PYTHONDONTWRITEBYTECODE=1 uvx ruff check node-bench`: **passed**.
- `bash -n` on both branch shell scripts and `/work/bench/logs/chain-v24.sh`: **passed**.
- `git diff --check 288c2a78..HEAD`: **passed**; branch working tree remained clean.
- Independent production-analyzer probes: complete inventory PASS; missing on both sides INCOMPLETE; NaN INCOMPLETE/ERROR row; missing expected scenario INCOMPLETE; provisional load PROVISIONAL; differing effective configurations INCOMPLETE. Existing tests additionally cover later-round omissions, first-round null, demotion to information, narrowed inventories, cold/warm mismatches, newly discovered confirmation FAILs, process-tree cleanup, and cgroup prefix ordering.
- A copied baseline script with a **stub run executable** completed all **22** default queue steps and validated their arguments through the real parser/`selected_scenarios`. The v24 `--skip-mainnet` path completed **11** steps; its second invocation skipped all 11 completed steps. The stub only wrote tiny synthetic documents; it executed no scenario. Logs: `queue-full.log`, `queue-first.log`, `queue-second.log` in the scratch directory.
- Short independent endpoint and SPV probes used a mocked node and a Python process that wrote two tiny files and exited 7. Protected-path reproduction used a temporary tree designated protected only in the probe, with fixture construction mocked out.

## Remaining majors

### R2-1 — MAJOR: confirmation still erases a qualifying FAIL using the wrong estimand (F2)

**Locations:** `node-bench/nodebench/pair.py:154–179`; `node-bench/nodebench/compare.py:123–146`; README's “Multiplicity and confirmation” explanation.

The repair correctly retains every confirmation row and keeps an initial FAIL when confirmation is merely nonsignificant with a wide CI. However, it still changes an initial FAIL to CLEARED when the confirmation's **geometric mean of paired ratios** CI excludes 1% slowdown. The gate's required quantity is the **relative worsening of the two medians**, with a one-sided Mann–Whitney test. Those are different estimands: the paired-ratio CI does not rule out a 1% median slowdown.

Production-path reproduction, with six official synthetic pairs and the actual `analyze` and `settle_confirmation` functions:

1. Initial A = `[100,100,100,100,100,100]`; B = `[102,102,102,102,102,102]`. Result: **FAIL**, 2% median slowdown, exact p = 1/924.
2. Confirmation A = `[100,200,300,400,500,600]`. Per-round A/B ratios = `[1.05,1.05,0.987,0.987,1.05,1.05]`, giving B ≈ `[95.2381,190.4762,303.9514,405.2685,476.1905,571.4286]`.
3. Confirmation median A = 350; median B ≈ 354.6099: **B is still 1.3171% worse**. Its Mann–Whitney p = 0.590909. The paired-ratio CI is `[0.994655,1.063632]`, above the clearance floor 1/1.01 ≈ 0.990099.
4. The original FAIL becomes **CLEARED**, and the merged gate is **PASS**.

The README/function claim that the chance of wrongly clearing a true slowdown is at most 2.5% is not established for the median criterion. DEC-23 does not approve this replacement/sequential acceptance rule.

**Fix:** Preserve qualifying FAILs and present confirmation as additional evidence. Make `--no-confirm` behavior the compliant default until a prespecified, approved policy using the same acceptance quantity exists. A CI for paired geometric ratios cannot supply that policy by itself.

Evidence: `/work/scratch/b1-r2/probes.py`, `probe-results.json`, `confirmation-merged.json`.

### R2-2 — MAJOR: reanalysis trusts legacy official/configuration flags rather than verifying the new evidence contract (F3/F6)

**Locations:** `node-bench/nodebench/pair.py:347–350`, `pair.py:429–431`; `node-bench/nodebench/results.py:14`; `pair.py:312–324` (expected-run metadata).

Newly produced runs require a reservation and quiet classification, and explicit unequal effective configurations generate ERROR. But analysis merely converts `load.official` to bool. It does not require a reservation, quiet-window admission, quiet classification, usable measurement telemetry, or a compatible run provenance version. The per-run schema remains `node-bench/1`. Effective-config consistency only compares dictionaries: when every run lacks it, all values are `null`, so there is no ERROR.

Reproduced with six complete `startup-regtest` rounds, all three required metrics present, old-style `load={class:quiet, official:true, quiet_window:0.1}`, and **no reservation or effective_config on either side**. The actual CLI:

```text
NODE_BENCH_ROOT=/work/scratch/b1-r2 PYTHONDONTWRITEBYTECODE=1 \
node-bench/run pair startup-regtest --analyze P \
  --out /work/scratch/b1-r2/legacy-startup --rounds 6
```

exited **0**, reported **PASS**, and labelled the report profile `nfr-perf-02/b1-r2`. This is a realistic legacy shape: the old producer could set official from quiet-window/class alone and did not record effective config. A contradictory synthetic `class=heavy, official=true` record also returns PASS, with only a warning.

This is not a request to authenticate arbitrary JSON against deliberate forgery. Existing result documents from an older harness must not automatically satisfy a newly strengthened official-evidence contract, and missing evidence is not equal evidence.

**Fix:** Version the per-run provenance contract and require its evidence for official analysis. Validate reservation/admission/class/telemetry explicitly; older or incomplete records must be PROVISIONAL or INCOMPLETE. Require complete verified effective configuration for node scenarios, with explicit scenario-specific exceptions for bench/SPV tools. Do not stamp the current profile on incompatible legacy evidence solely because its numeric metric inventory is complete.

Evidence: `probes.py`, `legacy-startup-cli-report.md`, `legacy-startup/`, `heavy-official-flag/` in the scratch directory. Separately, `config_problems` accepts `maxmempool_bytes=None` when the requested cap uses its default; that missing value should not count as a verified equal resource budget.

### R2-3 — MAJOR: fixed-work workloads accept overshoot and can omit the independent endpoint hash (F5/F7)

**Locations:** `node-bench/nodebench/scenarios/common.py:283–296`; `node-bench/nodebench/workloads.py:255–280`, `workloads.py:290–300`.

The new endpoint helper verifies that the requested block appeared somewhere in this invocation, then returns the final tip without requiring it to equal the target. `stop_at` writes `overshoot_blocks` but returns success. Its caller reports `ops=a.height`, although whole-lifetime counters include the additional work. Consequently sides that stop at different actual heights are accepted as the same fixed workload.

Reproduced with a clean-exit mocked node whose fresh log connects the correct target `(10, aaaa…)` and then `(11, bbbb…)`: `stop_at(..., height=10, reference=aaaa…)` returned **0**, with an accepted endpoint reporting height 10 and `overshoot_blocks=1`. This affects the PF0 workload path, even though bounded wall-clock IBD is now correctly informational.

In addition, `w_reindex_chainstate_testnet` proceeds with only a note if `master_hash` cannot find the requested historical hash. Missing/rotated seed logs therefore disable independent hash verification instead of making the fixed-input contract fail closed.

**Fix:** Require the final observed height/hash to equal the fixed target and independently established reference; reject overshoot/undershoot. Obtain reference hashes from frozen input metadata or a seed RPC before measurement, and refuse when unavailable. If variable work is intentionally supported, declare a different workload and account for its actual work rather than assigning fixed ops.

Evidence: `/work/scratch/b1-r2/endpoint_and_spv.py`, `overshoot-result.json`.

### R2-4 — MAJOR: the SPV scenario can record successful gate metrics from a failed tool

**Locations:** `node-bench/nodebench/scenarios/spv.py:79–109`, `spv.py:158`.

`spv_header_sync` waits for the process and cleans up its group, but never checks `p.returncode`. A syntactically valid header-time summary is enough to return measurements. Unlike the repaired microbench and counter-workload paths, a tool failure does not mark this gated scenario ERROR.

A short Python fake tool wrote `block_headers_ms: 1000`, a valid first-log timestamp and RSS, then **exited 7**. The production helper returned `header_sync_s ≈ 1.1` and no exception. `run_spv` will record that number as a gated successful measurement; successful numeric values can therefore survive an abnormal tool exit.

**Fix:** Check for a clean exit before ingesting the summary and propagate failure to the scenario status. Preserve the summary/stdout as diagnostic evidence, but do not treat the failed attempt as gate evidence.

Evidence: `/work/scratch/b1-r2/fake-spv.py`, `endpoint_and_spv.py`, `spv-exit-result.json`.

### R2-5 — MAJOR: protected-path checks do not cover fixture writes or every entry point (F10)

**Locations:** `node-bench/nodebench/fixture.py:53–65`; `node-bench/nodebench/impls.py:304` (`Node.start`); `node-bench/scripts/baseline.sh:50–51`; build script's ROOT/SRC/OUT construction.

`clone_datadir` now rejects escaping symlinks, and canonical validation of explicit out/datadir paths is useful. But root validation does not inspect later descendants. Fixture setup uses `paths.data_dir` followed by raw mkdir/open/tempdir operations, without `paths.writable`; `Node.start` also performs raw datadir/stdout writes. A symlink in a writable root descendant can still redirect writes into a protected tree.

Safe reproduction used `/work/scratch/b1-r2/path-boundary/root` as the bench root, designated its sibling `protected` as the protected tree, and linked `root/data/fixtures -> protected`. `paths.check_root()` passed. Calling `fixture.ensure` with fixture construction mocked out **created `protected/regtest-v1.lock`**. A real first fixture build would also create its temporary directory/datadir there. No real reference directory was involved.

The shell baseline/build entry points additionally derive and write ROOT-based paths before calling any Python boundary check; an invalid root can create directories/logs before `run` refuses it.

**Fix:** Validate canonical destinations at every write boundary, including fixture directories/locks/tempdirs and Node datadir/stdout setup. Apply the same check before shell entry points create output state. Keep clone sanitization, but do not rely on it to guard paths that bypass cloning.

Evidence: `/work/scratch/b1-r2/queue_and_paths.py`, `queue-and-path-results.json`.

## Round-1 disposition

| Finding | Round-2 status | Evidence / remaining boundary |
|---|---|---|
| F1 blocker: missing required measurements | **Closed for the declared B1 inventory** | Versioned inventories, union over defaults/added parameters/all documents, required-as-info ERROR, missing values/rounds/scenarios blocking. Independent missing/NaN probes and unit tests pass. |
| F2: confirmation hides regressions | **Open, R2-1 major** | New FAIL/ERROR/MISSING rows are retained, but paired-geometric CI clearance still masks an initial median-rule FAIL. |
| F3: noisy/provisional official PASS | **Partially closed, R2-2 major** | Current producer requires quiet admission/reservation, robust slice peak catches the original spike, measurement start excludes preparation. Analyzer still accepts incompatible legacy official flags. PSI during work is explicitly informational because own work contributes; reservation is an operational assumption. |
| F4: PF0 isolation/determinism/subtraction | **Addressed within B1; PF0 work remains deferred** | Aux pinning, par=1 default, digests and begin/end control markers for regtest fixed work. README now rejects generic startup subtraction and describes whole-lifetime, timer, multi-seed, historical-window and noise-floor limits. This is an entry point, not a completed PF0 guard. See R2-3 for a remaining correctness issue in that entry point. |
| F5: failed counter workload succeeds | **Counter exit/timeout handling repaired; endpoint aspect remains R2-3** | Nonzero wrapper exits and timeouts raise and produce error manifests. A separate unchecked SPV tool exit remains R2-4. |
| F6: cache labels/config fairness | **Partially closed, R2-2 major** | Failed drops raise; concurrent drop coordinated after preparation; repeated concurrent cold starts refused; slices validated; unused flags refused; applied settings recorded and compared. Explicit configuration mismatch probe blocks. Missing legacy effective settings still pass equality. |
| F7: unequal/incomplete sync endpoints | **Mostly repaired; fixed-work overshoot/hash fallback remains R2-3** | Full seeded IBD requires leaving IBD, named indexes and reference hash; reindex mismatch raises; stale instrumentation excluded and invocation timestamps bounded. |
| F8: PRD coverage gaps | **Explicit approved staging, not closure of the full PRD** | DEC-23(d) assigns B1b before M3 comparison. README inventories scope PASS and enumerate rescan/mempool/GBT/index/RPC/MN/CI gaps; per-component directory disk is now gated. No unrestricted performance approval claimed. |
| F9: wrapped-process/cgroup race | **Closed for ordinary stop/error/interruption paths** | Dedicated group cleanup/drain, pre-exec cgroup join, unit fork/orphan tests; interruption cleanup for load workers. SIGKILL of a harness with forking wrapper is accurately documented as a remaining operational limitation. Actual privileged cgroup execution was not tested here. |
| F10: protected path boundary | **Partially closed, R2-5 major** | Explicit canonical/clone checks work; fixture, Node and shell write paths remain incompletely guarded. |
| F11: invalid numeric measurements | **Closed for compared headline values** | Finite/nonnegative/nonboolean validation, zero support, invalid/missing rounds block, strict JSON publishing; same valid rounds for statistics and ratios. |
| F12: disk retention/cache races | **Improved, residual medium** | Free-space checks, successful fixture-run cleanup, locked atomic cache preparation and pruning added. But `common.cloned` unconditionally removes its clone on an exception before `cli.retain` can honor `--keep-failed`/long-run retention; failed reindex/rescan diagnostics/datadirs can disappear. The retention tests exercise `retain` alone, not this composed path. |
| F13: stale resumability | **Improved, residual medium** | Quiet/reservation/argv/version profile changes invalidate markers; old empty markers intentionally retained only for provisional queues; expected pair runs and atomic JSON saved; newest failure/binary identity handled in unpaired grouping. Marker profile still lacks harness, actual binary/input digests and result existence/status validation: same argv/version can skip obsolete or missing evidence. Baselines are now correctly documented as collection, not gate repetitions. |

## Baseline queue compatibility

The existing v23.1.8 first five `.done` files are empty legacy markers. The new script preserves their provisional skip behavior, so it does not automatically redo those steps. With RUNS=1 the dependent `spv-headers-testnet-rust.done` filename is unchanged, preserving `/work/bench/logs/chain-v24.sh`'s trigger. That launcher invokes the new script with `--dashd-version 24.0.0-rc.3 --skip-mainnet`, which the stubbed execution validated. Every existing queued scenario/option combination, including bench-log, indexed reindex/keep-datadir and Rust SPV, passed the real parser's applicability checks in the 22-step stub run.

No code-level queue incompatibility was identified. This is argument/control-flow validation, not a claim that the live processes have completed or that future full daemon runs will succeed. The protected PIDs were not visible from this review's process namespace; continuing their live operation could not be independently verified here. Existing progress logs were left unchanged, and neither cancellation markers nor completion markers under `/work/bench` were written.

## Acceptance boundary

This review accepts DEC-23's scoped B1/B1b/PF0 division and does not require the deferred PRD workload expansions to land in this round. The five majors concern correctness of the infrastructure already claimed to be implemented. Fixing the missing-metric blocker is substantial progress, but the reproduced false PASS and protected-write escape mean this head is not ready for approval. No end-to-end Rust daemon, real PMU, masternode network, mainnet replay or cold-cache run was performed or claimed.
