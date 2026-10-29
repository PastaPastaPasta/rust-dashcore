# O3 narrow closure review r2 — GPT

Reviewed `node/o3-fuzz-farm` at **45d1b9fd84d91f4d6439c53346b1b520b45a1175**, including the fixes **46c72607 through 45d1b9fd**, in its existing clean worktree `/work/home/agent/workspace/rdc-wt/o3-fuzz-farm`. Read the round-1 report, reproduction harness and fix commit messages, REVIEW-COMMON and the relevant PLAN entries. This file was created before testing and finalized after verification.

**Verdict: CHANGES-REQUIRED.** Five findings are CLOSED; three are PARTIAL. The original repros are addressed, but malformed-state variants and incomplete recovery still permit false success. Startup triage also does not interrupt an active job for a bench window or stop request.

Synthetic trees only. No reads of `~/private/`, real farm launch, systemd unit, GitHub action, branch edit, commit or push. Tests used `TMPDIR=/dev/shm/o3-r2-gpt-tmp` (0700) and eight build jobs for the runner selftest. Evidence is retained under `/work/scratch/o3-r2-gpt/`; temporary synthetic trees and the dedicated TMPDIR were removed.

## Dispositions

| Round-1 finding | Disposition | Evidence at the new head |
| --- | --- | --- |
| 1. Malformed farm plans/ledger can certify completion | **CLOSED** | Original empty-leg plan is refused. The regressions reject unreadable/foreign plans, bad budgets/shares, registry changes and invalid/duplicate/orphan ledger rows; a torn final append is explicitly repaired and reported. |
| 2. Private finding hashes, paths and classes reach argv/environment | **CLOSED** | Job files replace finding-bearing arguments. Python descendant scans and the real runner's minimisation/replay scans pass; installed `strace` also exercises the execve check. Target, pin and random job name remain visible, within the explicitly accepted limit. |
| 3. Shared vectors strand clusters on promotion | **CLOSED** | The original same-input/two-pin/two-class fixture produces two separately owned fixed files, both present, with two promotions and zero errors. Recovery/missing-vector regressions also pass. |
| 4. Triage errors return success / STATUS shows false zero | **PARTIAL** | The original invalid-JSON repro now exits 1 and reports UNKNOWN. Retry/error regressions pass. However, an existing state containing `[]`, `false`, `0` or `""` is still treated as missing and produces exit 0 with zero findings; see residual A. |
| 5. Worker kills lose slice accounting | **PARTIAL** | A tagged run with a persisted start and completed crash verdict is recovered exactly once, keeps the crash/CPU count and holds its leg. Finished-segment/lost-run regressions pass. Recovery without a verdict does not recover sink failures from logs and can credit failed time as clean; see residual B. |
| 6. A fix retains stale failed-leg CPU credit | **CLOSED** | Adapted original ledger repro schedules the entire failed leg's share afresh (0.4444 CPU-h), while other completed legs run nothing. Earlier clean credit is reset by a finding; open findings hold a leg until recheck fixes them. |
| 7. Smoke cost / unbounded triage / bench overlap | **PARTIAL** | README now states the 360 CPU-s floor and extra costs; summary reports floor, measured fuzz CPU and elapsed wall time. Jobs have overall deadlines. Bench/stop predicates prevent subsequent jobs, but cannot interrupt an already active startup triage job; see residual C. |
| 8. Misspelt smoke selector silently passes | **CLOSED** | Original `only=['typo_target']` returns false and prints `legs=0 unknown_targets=1 status=fail`. Selecting only a target without production smoke legs is also rejected. |

The worker-kill and post-fix probes were adapted to the new start/done ledger and cluster-owned-vector formats. Reusing the old untagged orphan directly would not exercise the new launch invariant: a start row is now fsynced before the runner can start. The original private-argv path is gone, so its replacement was tested through the job interface and the real subprocess chain.

## Residual A — major: falsy malformed triage state still certifies zero findings

**Location:** `node-fuzz/fuzzctl.py:472–482`, especially **477** (`state = state or ...`). Related caller: `status_lines`, line 740.

First create and triage a known synthetic divergence. Replace its target's `triage.json` with each of `[]`, `false`, `0` and `""`. `fuzzctl status-line` returns **0** for every case and prints:

```
SW-FUZZ open findings: 0 (private 0, public 0); fixed 0
```

Those are readable JSON values of the wrong schema, not absent state. The fallback runs before validation and silently converts them into empty state. The original `{broken` case is fixed, but the same false-zero failure remains for these malformed variants. A triage pass may reconstruct some findings from retained runs, but the independent STATUS command still certifies zero over unreadable-as-state data, and reconstruction cannot recover the original fixed/attempt/provenance state.

**Suggested fix:** use the empty-state default only when the state file is absent; require an existing decoded state to be a dictionary of the expected shape before applying defaults. Add CLI regressions for falsy non-object JSON values, requiring nonzero and UNKNOWN.

**Evidence:** `probes.py`, `edge-falsey-triage`, in `probes.out`.

## Residual B — major: interrupted recovery drops logged sink failures and credits failed CPU

**Location:** `node-fuzz/fuzzctl.py:1156–1173`, especially **1160–1162**. Compare live runner counting at `node-fuzz/run.sh:506–507` and verdict generation thereafter.

The no-verdict recovery path reconstructs CPU, divergence filenames and crash filenames, but never reads the segment logs for `sink write failed`. `done_row` defaults the absent `sink_failures` count to zero; with no saved artifact the status becomes `interrupted`, which earns clean credit.

Synthetic reproduction: persist a start for `kv_model@v24.0`, make the corresponding tagged run with a finished segment containing `2880 0` CPU seconds and `kv_model: sink write failed` in `fuzz.out`, leave no verdict or saved finding artifact, then resume. This models a kill before verdict creation when the failed sink also could not preserve an artifact. Startup triage is enabled in this probe. The recovered row is:

```
status=interrupted cpu_hours=0.8 sink_failures=0 recovered=true
```

The public leg runs **no fresh slice**, and the final summary reports `sink_failures=0 triage_errors=0 outcome=complete`. Available evidence of failure has been erased from the accounting. Even when a crash artifact exists and prevents this particular false completion, the logged sink-failure count is still omitted by this recovery path.

**Suggested fix:** reconstruct log-derived failure evidence as well as artifact counts, using the live runner's rules. At minimum a logged sink failure must produce a non-clean recovered row, reset its leg's credit and remain visible in the summary. If recovery cannot distinguish clean interruption from known failure, retain an explicit failed/unknown outcome with no clean credit. Add no-verdict recovery tests with a logged sink failure, including an unavailable artifact sink.

**Evidence:** `probes.py`, `edge-killed-sink-failure`, in `probes.out`. This is a gap in the newly added reconciliation code, grouped under round-1 finding 5 rather than counted as a separate ninth finding.

## Residual C — minor: startup triage checks bench/stop only between jobs

**Location:** `node-fuzz/fuzzctl.py:1200–1211`, **1269–1270**; `Triage.job`, **535–539**; blocking `run_leg`, **300–313**.

The new stop predicate is evaluated between triage operations. Startup triage runs synchronously before the farm scheduling loop, and a running job blocks in `communicate(timeout=...)`. No concurrent monitor calls `interrupt_all` there when a bench window starts or the farm's stop event is set. The preflight one-hour bench check also cannot predict an unscheduled live benchmark or a window added after that check.

Synthetic process probe: start startup-style `farm.triage()` with a real stand-in run.sh that sleeps, then make `bench.blocked()` return a live bench. After 0.3 seconds the tracked triage process is still running and no interruption is recorded. Cleanup explicitly sets the stop event and calls `interrupt_all`; the process then exits and the thread joins. Without that external call it waits for the job's own deadline (up to 15 minutes, plus interruption grace). SIGTERM sets the event, but startup likewise has no loop to act on it while the job is blocked.

**Suggested fix:** monitor bench/stop/deadline while an active startup job runs and interrupt its process group, or make `run_leg` accept a cancellation predicate that it checks during bounded communication waits. Test a bench/stop transition after the child has started, not only a predicate true before launch.

**Evidence:** `probes.py`, `edge-startup-bench-monitor`, in `probes.out`. The cost-label and overall-job-deadline portions of finding 7 are closed; its bench-monitoring portion remains.

## Validation

- `TMPDIR=... python3 -I node-fuzz/fuzzctl.py selftest`: **PASS, 46 tests**. Log: `selftest.log`.
- `TMPDIR=... python3 -I node-fuzz/fuzzctl.py check`: **PASS**, five targets, seven smoke legs, seven farm legs.
- `TMPDIR=... CARGO_BUILD_JOBS=8 node-fuzz/run.sh --selftest`: **PASS**, real instrumented runner, including minimisation/replay process scans and the installed-strace execve check. Log: `runner-selftest.log`.
- Independent synthetic closure/adversarial probes: `probes.py` and `probes.out`. Shared-vector, corrupt-state, invalid-plan, kill recovery, fresh-budget and selector reproductions rerun against this head, with the format adaptations described above.
- Original direct SIGINT runner check adapted to this head: **PASS**. Exit 1, failing verdict, libFuzzer status 72, 0.0003 CPU-h and 14,840 executions preserved. Synthetic tree removed. Evidence: `interrupt.py`, `interrupt.out`.
- `git diff --check 2a4a89d6 45d1b9fd`, `bash -n node-fuzz/run.sh`, Python AST parsing: **PASS**. Under-review worktree remains clean at the exact requested head.
- No full production smoke or systemd farm/stop test was run; no unit cleanup was necessary. The synthetic process probe explicitly terminated its child process group.

No additional independent finding was confirmed in the changed code beyond the three residuals above. This is a closure review of the fixes, not a renewed full-node or full-workspace audit.

## Final verdict

**CHANGES-REQUIRED** — close the malformed-state and no-verdict failure-accounting gaps; finish active startup-triage cancellation before treating all eight round-1 findings as closed.
