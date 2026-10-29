# O3 confirmation review r3 — GPT

**Verdict: APPROVE. A, B and C are CLOSED. No regression confirmed.**

Reviewed `node/o3-fuzz-farm` at **4e628a2be24cc8ccf58d330cdaee75c30c1fe5ac**, including `38536c54`, `39c6bffe` and `4e628a2b`, in `/work/home/agent/workspace/rdc-wt/o3-fuzz-farm`. Read the round-2 report, its retained reproduction harness, REVIEW-COMMON, and the fix diff. This report was created before testing and finalized afterward.

This was a narrow confirmation review of the three residuals, not a renewed full audit. Synthetic trees only, with **TMPDIR=/dev/shm/o3-r3-gpt-tmp** (0700). No reads of `~/private/`, real farm, systemd invocation, branch changes, commit, push or GitHub comment. The worktree remained clean at the requested head.

## Dispositions

### A — CLOSED: malformed triage state cannot certify zero findings

Code: `node-fuzz/fuzzctl.py:511` (`load_triage`), `:559` (`state_problem`), `:844` (`status_lines`).

Reran the round-2 known-divergence fixture with `triage.json` replaced by **[]**, **false**, **0** and **""**. Every `status-line` invocation now returns **1** and reports **SW-FUZZ open findings: UNKNOWN**. Independent checks also run `triage --no-minimize` for those values and for `null`, `{}`, a non-string run list and a boolean run-error count: all return 1 and UNKNOWN. Invalid JSON also remains rejected.

Only an absent file receives empty-state defaults. The 50-test suite additionally verifies foreign cluster IDs, foreign vector names, invalid states, missing fixed-promotion records and missing/foreign vector provenance. No residual false-zero behavior was observed in these cases.

Evidence: `round2-probes.out` (`edge-falsey-triage`), `probes.out` (`A-malformed-state`), `selftest.log`.

### B — CLOSED: interrupted recovery retains sink failure and gives no clean credit

Code: `node-fuzz/fuzzctl.py:399` (`log_evidence`), `:1270` (`Farm.reconcile`), `:817` (`ack_sink_failures`).

Reran the exact no-verdict fixture: persisted start, tagged `kv_model@v24.0` run, finished segment containing **2880 0** CPU seconds, **sink write failed** in `fuzz.out`, no saved finding artifact. Startup triage was enabled. Recovery now produces:

```
status=finding cpu_hours=0.8 sink_failures=1 recovered=true
```

The measured 0.8 CPU-h is preserved, but its **credited CPU is zero**. The leg runs no fresh slice while held; summary reports `sink_failures=1` and `outcome=legs-retired`, rather than complete. STATUS shows one open public finding with zero triage errors.

Independent lifecycle checks confirm that a second launch still holds the leg, ordinary `--recheck` does not close the sink finding, and `triage --ack-sink-failures` closes it explicitly. The next launch schedules the full **0.4444 CPU-h** share afresh and completes. A repeated reconciliation adds no duplicate row.

Evidence: `round2-probes.out` (`edge-killed-sink-failure`), `probes.out` (`B-killed-sink-failure`), and the sink-failure selftests.

### C — CLOSED: active startup triage responds to bench and stop transitions

Code: `node-fuzz/fuzzctl.py:283` (`run_leg`), `:626` (`Triage.job`), `:1318` (`Farm.triage`).

Reran the round-2 startup-style `farm.triage()` process probe. Its observation delay was changed from 0.3 to **1.3 seconds** to cover the documented one-second cancellation check. After a synthetic bench becomes active, the job is no longer running; cleanup joins successfully.

Independent process-group probes used a stand-in runner with a parent and child, both handling SIGINT. Each trigger was applied only after the job and child had started. Bench transition canceled in **0.919 s**; stop-event transition canceled in **1.021 s**. Both issued SIGINT to the job's process group, returned 130, terminated the child, emptied tracked processes and removed the staged job file. These checks did not call external `interrupt_all` to obtain successful cancellation.

An initial scratch fixture used a background shell `sleep`, which inherits ignored SIGINT and therefore waits for the existing interruption grace period. That fixture was replaced with explicit SIGINT handlers for the one-second response assertions; it was a fixture mismatch, not a confirmed product regression. No synthetic process was left running.

Evidence: `round2-probes.out` (`edge-startup-bench-monitor`), `probes.out` (`C-active-startup-cancellation`), and the active-startup cancellation selftest.

## Validation and regression checks

- `TMPDIR=... PYTHONDONTWRITEBYTECODE=1 python3 -I node-fuzz/fuzzctl.py selftest`: **PASS, 50 tests** (5.519 s).
- Independent A/B/C assertion harness: **PASS**. Includes malformed-state CLI checks, recovery/restart/acknowledgement/fresh-credit lifecycle, and real synthetic parent/child process-group cancellation.
- Retained round-2 reproduction harness rerun against this head: **PASS** with the C observation-delay adaptation above. Previously closed shared-vector promotion, malformed-plan refusal, crash-verdict recovery/exactly-once accounting, post-fix full-share scheduling and misspelt smoke-selector rejection also remain intact.
- `python3 -I node-fuzz/fuzzctl.py check`: **PASS**, five targets, seven smoke legs, seven farm legs.
- `git diff --check 45d1b9fd..4e628a2b`, `bash -n node-fuzz/run.sh`, Python AST parsing of both changed Python files: **PASS**.
- No source/build configuration changed during review. No real-runner rebuild or production smoke was needed for this narrow synthetic confirmation; `run.sh` is unchanged by these three fixes.

Evidence retained under **/work/scratch/o3-r3-gpt/**: `selftest.log`, `probes.py`, `probes.out`, `round2-probes.py`, `round2-probes.out`. Synthetic trees and the dedicated TMPDIR were removed after testing.

## Final verdict

**APPROVE** — all three round-2 residuals are CLOSED; no new finding or regression confirmed within this scope.
