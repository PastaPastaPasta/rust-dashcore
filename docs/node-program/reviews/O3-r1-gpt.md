# O3 implementation review r1 — GPT

Reviewed `node/o3-fuzz-farm` at **2a4a89d6**, including 75f4c71a, dc896654 and fb64d024, in the fresh detached worktree `/work/scratch/o3-gpt/repo`. Read REVIEW-COMMON, PLAN row O3, DEC-24 D6, DEC-30 N5 and DEC-59 Q1. No branch edits, commits, pushes, private-corpus runs, private-tree reads or real farm launches. Tests use synthetic trees, `TMPDIR=/dev/shm/o3-gpt-tmp` (0700), and at most eight build jobs.

## Major findings

### 1. Persisted farm state is not validated; corruption can produce false completion

**Locations:** `node-fuzz/fuzzctl.py:802`, `node-fuzz/fuzzctl.py:773`, `node-fuzz/fuzzctl.py:948`.

`Farm` trusts any JSON returned for `plan.json`. A synthetic plan `{"night":"test","cpu_hours":4,"legs":{}}` produces `outcome=complete`, zero runner calls and a summary containing `cpu_hours=0.00/4`. An unreadable plan is silently recreated instead of distinguished from a first launch. `load_records` likewise silently drops malformed ledger rows and does not validate decoded row types, leg names or numeric values.

This defeats fail-closed handling and can discard progress or certify an empty plan as completed. Validate schema, finite nonnegative budgets, leg membership, share totals and provenance/configuration compatibility before scheduling. Treat an existing unreadable file as an error. Recover only a demonstrably interrupted final append, with explicit accounting/error reporting, rather than silently discarding arbitrary records.

### 2. Private-route findings leak their identities through process arguments

**Locations:** `node-fuzz/fuzzctl.py:265`, `node-fuzz/fuzzctl.py:509`, `node-fuzz/fuzzctl.py:535`, `node-fuzz/fuzzctl.py:558`; `node-fuzz/run.sh:301`, `node-fuzz/run.sh:344`, `node-fuzz/run.sh:348`.

Minimisation and replay pass the original finding/regression pathname as a positional argument to `run.sh`. Synthetic capture of `Popen` shows `run.sh diff_tx_decode --pin v24.0 --replay <synthetic-tree>/fuzz/diff_tx_decode/regress/open/<64-character-input-sha>`. Crash minimisation also passes the original `crash-<hash>` directly to the fuzzer. Copying divergence/replay inputs under SHA-derived names does not make them opaque: the hash still identifies the input, and the original identity has already appeared in the outer process argv. `--field` can additionally put the divergence class on argv; the subsequent `env -i NODE_FUZZ_ABORT_FIELD=...` exposes that class during process launch.

Redirecting stdout does not protect `/proc/*/cmdline` or `ps`. Keep private input identity and class out of executable arguments: pass them through a descriptor or private environment plumbing, stage inputs under fixed/random names unrelated to content, and keep original provenance solely in private files. Retest the entire subprocess chain, not just the final target's output. Aggregate STATUS/triage summaries otherwise avoid private classes; no real private inputs were examined.

### 3. Shared vectors break open-to-fixed promotion and can lose regression coverage

**Locations:** `node-fuzz/fuzzctl.py:519`, `node-fuzz/fuzzctl.py:523`, `node-fuzz/fuzzctl.py:525`, `node-fuzz/fuzzctl.py:553`, `node-fuzz/fuzzctl.py:560`.

Clusters deduplicate by target/kind/class, but vectors and metadata deduplicate only by input SHA. The same input can trigger different fields on v23.1 and v24.0, yielding two clusters that reference one vector. Rechecking the first cluster moves that file into `fixed`; the second is then silently skipped because its `open` file is missing. Synthetic reproduction ends with one fixed cluster and one open cluster whose vector does not exist. Updating one cluster's vector can also delete a file another cluster still owns, and its single metadata file overwrites the other cluster's provenance.

Use explicit multi-cluster ownership/reference tracking or separate cluster-owned vectors. Recheck all referencing clusters/pins before a shared promotion, retain complete provenance, and never delete a referenced vector. Missing open vectors must be errors, not skipped successes. Promotion also needs crash recovery: interruption after `os.replace` but before saving triage state creates the same missing-vector state.

### 4. Triage errors return success and STATUS presents unknown findings as zero

**Locations:** `node-fuzz/fuzzctl.py:458`, `node-fuzz/fuzzctl.py:469`, `node-fuzz/fuzzctl.py:585`, `node-fuzz/fuzzctl.py:1180`, `node-fuzz/fuzzctl.py:1183`.

After creating known synthetic findings, replacing their `triage.json` with invalid JSON makes `triage --no-minimize` return **0**, print `errors=1`, then report `SW-FUZZ open findings: 0 (private 0, public 0); fixed 0`. `status-line` suppresses the same error. A run-level triage error is also marked processed unconditionally, preventing a later normal pass from retrying its lost vector creation.

Propagate triage/state errors through CLI exit status; report aggregate unknown/error counts without exposing private details. Keep failed runs pending for bounded retry or a persistent explicit error state. Do not certify a zero-findings STATUS when state is unreadable.

### 5. A worker kill between verdict and ledger append loses a completed slice

**Locations:** `node-fuzz/fuzzctl.py:863`, `node-fuzz/fuzzctl.py:875`, `node-fuzz/fuzzctl.py:937`.

The slice ledger is appended only after `run.sh` returns, and restart accounting consults only that ledger. A worker killed after a completed `verdict.json` is written but before `record` leaves an orphan verdict. There is no slice-start record or startup reconciliation. A synthetic orphan verdict with a crash and the public leg's full budget results in that full budget being scheduled again; the resulting ledger summary says `outcome=complete` and `crashes=0`. Startup triage can preserve the finding separately, but it does not repair slice CPU/failure accounting. For a kill without a verdict, even available segment accounting is ignored.

Persist slice identity at launch and reconcile unfinished entries against private run/verdict/segment files on restart. Recover completed verdicts exactly once; represent interrupted or unknown accounting explicitly and retain failure evidence in the farm summary. The mock stop test does not cover this kill window.

### 6. A fix does not reset a failed leg's CPU budget

**Locations:** `node-fuzz/fuzzctl.py:349`, `node-fuzz/fuzzctl.py:755`, `node-fuzz/fuzzctl.py:802`, `node-fuzz/fuzzctl.py:937`.

Plans and slice records have no code/provenance epoch. `spent_by_leg` adds every slice's CPU, including findings from previous code. After a fix, relaunching the same night retains that credit. A synthetic ledger where the failed leg consumed its share makes the next launch return `complete` with **zero fresh runner calls**, even if startup recheck has just promoted its vector to fixed. If the original failure happened partway through its share, only the remainder is required. This contradicts the documented fresh budget after a fix.

Store the code/provenance identity and the budget epoch for each leg. A fix/relevant code change must invalidate the failed leg's previous clean-budget credit while preserving historical usage and findings for reporting. Verify that a corrected leg completes its entire prescribed fresh budget.

## Minor findings

### 7. The smoke cost is larger and less bounded than its 360 CPU-second label

**Locations:** `node-fuzz/run.sh:375`, `node-fuzz/run.sh:343`, `node-fuzz/fuzzctl.py:272`, `node-fuzz/fuzzctl.py:657`.

The seven registry legs total 360 CPU-s, but that is a minimum CPU floor. Segment duration is `max(60, ceil(missing CPU-s/jobs)+30)` in wall seconds, and only after the segment ends is CPU measured. Five 60-second legs therefore request at least 90 seconds each; two 30-second legs request 60 each: at least **570 wall-seconds of fuzzer process time** on the initial segments. Model I/O and contention affect actual CPU, and repeated minimum-length segments can overshoot the remaining budget substantially. Builds, runner self-test, replay and triage are additional costs. Crash minimisation has an execution limit per internal step, but no overall wall/CPU deadline around the subprocess; its attempts and replays can dominate a failing PR's duration.

Describe 360 CPU-s as a floor and measure/report actual gate cost, or enforce a tighter CPU ceiling. Give gate minimisation/replay a bounded deadline and preserve the original finding on timeout; failure evidence should already make the smoke nonzero. Farm startup triage is synchronous before the scheduling loop, so it also needs bench/deadline monitoring during triage, not only a preflight check.

### 8. A misspelt smoke selector passes without exercising a target

**Locations:** `node-fuzz/fuzzctl.py:631`, `node-fuzz/fuzzctl.py:663`.

Synthetic `smoke(..., only=['typo_target'], selftest=False)` returns true with `legs=0 ... status=pass`. The CLI accepts such names without validation. Enabling selftest merely runs the runner tests; it still exercises none of the selected production legs. Reject unknown selectors and empty production-leg selections. The proposed unfiltered gate invocation is not affected, but future targeted gates could silently skip coverage.

## Validation and gate fit

- `python3 -I node-fuzz/fuzzctl.py check`: **PASS**, 5 targets / 7 smoke legs / 7 farm legs. Current Cargo bins, target files, TARGETS and MODELS agree. Model dispatch selects `Release` without adding a node-l1 → dashd-store dependency.
- `python3 -I node-fuzz/fuzzctl.py selftest`: **PASS**, all 23 tests in about one second.
- `git diff --check 2a4a89d6~4 2a4a89d6`: **PASS**.
- Adversarial synthetic probes: `/work/scratch/o3-gpt/probes.py`, output `/work/scratch/o3-gpt/probes.out`. They reproduce shared-vector promotion, private argv exposure, malformed plan false completion, malformed triage success/zero STATUS, unreconciled verdicts, stale post-fix CPU credit, and empty smoke selections. Orphan-verdict scheduling probe isolates accounting by disabling startup triage; startup triage does not write the slice ledger.
- Real `run.sh --selftest`: **PASS**, `node-fuzz selftest: ok`; fresh instrumented release build took about seven minutes with eight jobs. Log: `/work/scratch/o3-gpt/runner-selftest.log`. A narrow `mktemp` shim redirects only its hard-coded selftest prefix into `/work/scratch/o3-gpt/`, leaving the branch unchanged.
- Direct synthetic runner SIGINT check: **PASS for graceful interruption**, exit 1, libFuzzer status 72, a written failing verdict, and recorded CPU/execution counts (0.0002 CPU-h / 13,090 executions in this short run). Log: `/work/scratch/o3-gpt/interrupt.out`; harness: `/work/scratch/o3-gpt/interrupt.py`. The initial attempt was stopped while waiting for the Cargo lock; the rerun tested an executing binary. This validates the runner process-group interruption path, not hard-kill recovery or systemd stop.
- `bash -n node-fuzz/run.sh` and Python byte-compilation: **PASS**. Detached worktree tracked/untracked Git status remains clean.
- No full smoke, private-route production target run, real farm or systemd unit was launched. CPUQuota/real systemd-stop behaviour is therefore not claimed verified. All temporary test trees under the dedicated TMPDIR were removed.

The proposed `fuzz-check` and `fuzz-selftest` gate steps are cheap and useful. `fuzz-smoke` is correctly nonzero when a run.sh leg/selftest fails in its ordinary path, and runner source checks sink/refusal/crash/divergence/budget failures. Its full end-to-end cost and termination should be bounded as above. The current 23 tests mostly use injected runners and miss the persisted-state and shared-vector cases. Do not treat their success as evidence that resumption/promotion/private argv handling is correct.

## Final verdict

**CHANGES-REQUIRED**.
