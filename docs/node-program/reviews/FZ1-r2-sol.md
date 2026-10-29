# FZ1 r2 — Sol review

**Scope:** `node/fz1-plateau`, exact head `4b08cdcbc0b430e3f5b3c30c8f23ea5884759b4f`, range `c7ef911bbae5ff6104ddab58c4fe70f7420d0002..4b08cdcbc0b430e3f5b3c30c8f23ea5884759b4f`. Reviewed all seven commits and all five changed files: `node-fuzz/fuzzctl.py`, `run.sh`, `fuzzctl_test.py`, `README.md`, and `targets.toml`. Detached worktree: `/work/scratch/fz1-r2-sol/wt`.

**Binding specification:** DEC-184, including its corrections, and DEC-186, including its amendment, in `~/workspace/node-program/DECISIONS.md`. The amendment controls: live plateau detection is a stop heuristic; certification requires a separate single-process confirmation, 3600 seconds of that process's own CPU after INITED, the execution floor, flat coverage, clean findings/exit evidence, and evidence-derived status. This review treats a false `certify` success as a merge-gate blocker.

The redesign closes the original live-certification paths. It does not yet make the replacement certifier safe. Two blockers, four major findings, and one minor finding follow.

## New findings

### R2-B1 — blocker: SIGINT supplies no final cov/ft snapshot; a partially printed growing result certifies as flat

**Location:** `node-fuzz/fuzzctl.py:1296–1303`, `:1337–1354`, `:1408–1419`; `node-fuzz/run.sh:565–600`; `node-fuzz/fuzzctl_test.py:1774–1815`.

**Scenario:** certification stops libFuzzer asynchronously with SIGINT, then calls the last earlier parseable `cov: N ft: M` pair its final coverage. In the locked `libfuzzer-sys 0.4.13`, `Fuzzer::InterruptCallback()` calls `PrintFinalStats()` and exits 72. `PrintFinalStats()` prints execution totals and other `stat::` fields, **not cov/ft**. The ordinary `DONE` coverage line belongs to the normal loop exit, not the SIGINT path. Coverage and feature bookkeeping can already have grown when SIGINT interrupts a `NEW` line before both fields have been printed. The parser silently falls back to the preceding flat pulse. A genuine CPU window and sufficient executions therefore do not prove the claimed zero growth.

**Evidence:** `/work/scratch/fz1-r2-sol/final_stats.cpp`, `driver_fixtures.py`, `driver-results.json`, and `fixtures/partial-final-line/`. A locally compiled, manually instrumented two-edge C++ callback linked to the actual locked libFuzzer archive gains its second edge/feature after 1.2 million callbacks. A `vfprintf` wrapper only lengthens the real interval between libFuzzer printing the new cov and printing the new ft; it does not change coverage bookkeeping or the interrupt handler. The exact reviewed run.sh CPU driver stops it with exit 72. The real log ends:

```text
#1048576 pulse  cov: 1 ft: 1 ...
#1200001 NEW    cov: 2==...== libFuzzer: run interrupted; exiting
stat::number_of_executed_units: 1200001
```

The probe at the interrupted ft write records `cov=2 ft=2 calls=1200001`. The reviewed reader instead derives baseline **1/1**, final **1/1**, and **1,199,998** window executions. Its real 2-CPU-second experiment correctly remains `uncertified` because it is short. With explicitly synthetic hour-window timing metadata, leaving that real log unchanged, the reader returns **`certified`**. `window-observed.json` and `verdict-observed.json` preserve the actual short experiment; this is not a claim that a real hour was run. The scheduling race is independent of the window length.

The existing driver stand-in prints a full `DONE cov/ft` in its SIGINT handler, unlike the real locked libFuzzer. It consequently cannot catch this failure.

**Fix:** obtain and require a complete, authoritative final coverage/feature snapshot after stopping the single fuzzer. A last pulse/NEW/INITED line is not that snapshot. If a graceful-stop or libFuzzer change is needed to produce it, implement and verify that protocol with the locked library and reconcile its exit status with the spec. Fail closed when the final snapshot is absent or incomplete; merely rejecting this particular torn NEW line still leaves the interval before any NEW text is printed. Retain a real-library interruption regression.

### R2-B2 — blocker: a finding opened during the confirmation run is ignored by the gate command

**Location:** `node-fuzz/fuzzctl.py:2248–2249`, `:2261–2276`; compare `:1457–1460`.

**Scenario:** the leg has no open finding when certification begins. A concurrent farm/triage run opens one during the hours-long confirmation. At completion, `certify()` judges only its own run and exits 0; it never repeats the open-finding check. The separate status reader correctly overrides the same run with `finding`. A gate invoking the authorized certifier therefore gets a false success while the leg has an open finding.

**Evidence:** `state_fixtures.py`, `state-results.json`, `fixtures/finding-during-certify/`. The fixture runner writes a valid open sink-failure cluster during an otherwise qualifying confirmation. The actual `certify()` path prints `status=certified ... (all checks hold)` and returns **0**. Immediately afterwards, `open_finding()` is true and `certificates()` reports **`finding`**. No real runner or unit is invoked.

**Fix:** apply the same open-finding override to the final command result and published certificate decision, using a consistent triage-state snapshot. An open or unreadable state at completion must prevent success, not only one present at launch. Add the finding-during-run negative alongside the existing finding-before-run test.

### R2-M1 — major: the CPU window includes children's CPU, contrary to the amended own-process rule

**Location:** `node-fuzz/run.sh:555–561`, `:593–600`; `node-fuzz/fuzzctl.py:1340`, `:1352`.

**Scenario:** `cpu_now()` sums `/proc/<pid>/stat`'s utime, stime, **cutime and cstime**. `wait4` rusage likewise includes reaped descendant work in the demonstrated Linux case. A helper process can supply the confirmation's CPU window even when the fuzzer itself has used almost none. Counting one selected log does not establish that the measured CPU belongs to one fuzzer. The original DEC-186 process-tree accounting was superseded by the amendment's own-process accounting.

**Evidence:** `driver_fixtures.py`, `driver-results.json`, `fixtures/reaped-child-cpu/`. The exact driver runs a stand-in parent that reaps a child burning 2.3 CPU seconds, then sleeps. For a requested 2-second window it records `cpu-window`, exit 72, and **2.318** CPU seconds after its baseline, while the parent reports only **0.049606791** total own CPU seconds. The reviewed verdict tail passes it. The short run remains uncertified solely because its requested window is under 3600; both counters would make the same attribution error at 3600. This is a real process-accounting experiment, not a fake `/proc` counter.

I found no production callback at this head that forks such a helper. This is therefore a demonstrated certifier contract defect and a conditional false-pass path, not a claim that the supplied production smoke used children.

**Fix:** use only utime + stime for the stop decision and certification proof. Preserve a trustworthy own-process terminal measurement, or use the own-CPU measurement at the stop to prove the completed window; do not substitute descendant-inclusive final rusage. Report descendant CPU separately if useful. Match the driver's INITED detection to the anchored status parser as well: the current substring check can recognize an unrelated diagnostic containing `INITED` before the actual baseline.

### R2-M2 — major: an ordinary Git setting suppresses untracked files in the built-source record

**Location:** `node-fuzz/run.sh:460–466`; `node-fuzz/fuzzctl.py:1415–1416`.

**Scenario:** `git status --porcelain` honors `status.showUntrackedFiles=no`. The source recorder neither overrides that setting nor explicitly requests untracked files. Before and after a build, a tree with untracked source can therefore be recorded as clean, and the certifier accepts that dirty flag. This violates the explicit untracked-files requirement.

**Evidence:** `state_fixtures.py`, corrected `state-results.json`, `fixtures/source-hidden-untracked/`. An isolated local sparse clone at the exact reviewed head has that Git setting and an untracked Rust source fixture. Executing the exact source-recording shell statements with run.sh's `set -euo pipefail` writes:

```text
4b08cdcbc0b4
clean
```

An explicit `git status --porcelain --untracked-files=all` shows the untracked source. The `dirty` negative in `evidence-results.json` confirms that certification depends on the flag being recorded correctly. The local clone creates no commits and changes no settings in either real repository.

**Fix:** explicitly include all untracked files, independent of Git configuration, in both source-state samples. Preserve fail-closed Git error handling and add a hidden-untracked configuration fixture. A Git-less source directory was also tested with the real shell options: it exits 128 before recording source; that suspected failure is **not** a finding.

### R2-M3 — major: persisted evidence is not bound to its leg, and contradictory window evidence is ignored

**Location:** `node-fuzz/fuzzctl.py:1323–1357`, `:1380–1397`, `:1447–1458`.

**Scenario:** the status reader derives a leg from the directory name but never checks it against the verdict/certificate target. An intact certificate copied or backfilled under another leg can certify that other leg without any executions of its target. It also drops critical fields of `window.json` before validation, so final exit evidence and stop-window facts can contradict the verdict without invalidating the certificate. A missing/unknown source revision also passes if the dirty flag says false.

**Evidence:** `r2_evidence.py`, `evidence-results.json`, `fixtures/evidence/`:

- `foreign-leg-certificate`: a qualifying fixture run and its intact matching `certificate.json` identify `kv_model` at `4b08cdcbc0b4`. Copying the unchanged run under another registered target's `runs/` directory makes `certificates()` report that other leg as **`certified`**, although returned evidence still names `kv_model`.
- `window-status-disagrees`: window status **1**, verdict libFuzzer status **72** → `certified`.
- `window-length-disagrees`: window CPU length **60**, verdict requested length **3600** → `certified`.
- `stop-cpu-short` / `stop-cpu-absent`: window stop CPU **41** against baseline **40**, or no stop measurement, beside a claimed 3600-second CPU stop and later rusage → `certified`.
- `source-commit-absent` / `source-commit-unknown`: no revision or `-`, with dirty false → `certified`.

These are deliberate persisted-evidence corruption/backfill fixtures. They do not establish that the intact runner normally produces these contradictions. They do establish that re-reading evidence does not check the promised invariants and identity. They require no trust in a stored `certified` boolean.

**Fix:** validate a strict evidence schema before judging it, bind it to the expected target/pin and source identity, and cross-check the duplicated window length, wall cap, exit status and stop measurements. Require a valid source commit rather than accepting `certified@-`. Derive status only after validating all facts needed to prove that this leg completed its post-INITED CPU window. Preserve the original stop evidence in the stored evidence so certificate comparison covers it.

### R2-M4 — major: the wall-cap kill fallback is unreachable before INITED

**Location:** `node-fuzz/run.sh:574–589`.

**Scenario:** the wall cap sends SIGINT while initialization is still pending. If the child does not respond to that signal, the subsequent `elif inited is None` branch wins forever. The 300-second SIGKILL fallback is never reached until INITED appears. The wall cap can leave the controller and child alive indefinitely; a child doing initialization work could keep consuming CPU. This is not a path to certification, but it breaks the bounded-run guarantee.

**Evidence:** `wall_cap_fixture.py`, `wall-cap-results.json`, `fixtures/no-inited-wall-cap/`. The exact extracted driver runs an owned stand-in with no INITED line that acknowledges but ignores SIGINT. A test clock advances its monotonic elapsed time by 600 seconds after the wall-cap interrupt, avoiding a real five-minute wait. The controller remains alive, and no new window verdict is written. The fixture then kills only its own process group by exact ID. This tests an unresponsive-child path; normal libFuzzer SIGINT handling does respond.

**Fix:** handle already-stopped runs and their kill deadline before any initialization/progress branches. The escalation must apply regardless of INITED. Add the no-INITED/unresponsive child negative, including cleanup.

### R2-m1 — minor: an existing unreadable or JSON-null certificate is treated as absent

**Location:** `node-fuzz/fuzzctl.py:1425–1428`; `read_json` at `:254–260`.

**Scenario/evidence:** `torn-certificate` and `null-certificate` in `evidence-results.json` replace an existing certificate with `{"baseline":` or `null`. Both re-read as `certified`, because parse failure/null is conflated with a missing certificate and comparison is skipped. A readable mismatching object correctly yields `invalid`. This alone does not upgrade a nonqualifying underlying run, so its severity is lower than the identity/window failures above.

**Fix:** distinguish a genuinely absent certificate, which is needed for initial derivation, from an existing unreadable or malformed one. The latter should be `invalid`. Keep status derived from the original run evidence.

## Closure of the r1 findings

The original scenario functions and interruption binary were reused from `/work/scratch/fz1-sol/`. Scripts were adapted only for removed r2 APIs/output fields and the monotonic-clock fixture; all replay sinks are newly owned 0700 trees under `/work/scratch/fz1-r2-sol/`. Evidence: `r1-replay-results.json`, `instrumented-sigint-results.json`, `state-results.json`, and the r2 selftest.

- **R1-B1 — closed as originally reported.** Per-process growth now sums changes from each process's own INITED. Lower-job growth and reload growth both remain visible and prevent suspicion. Neither replay writes a certificate. Certification uses a separate single process, so the concurrent-maxima union problem is moot there. R2-B1 is a different final-snapshot defect.
- **R1-B2 — closed as originally reported.** Live intervals spanning a segment/worker epoch change receive no fuzzing time. The reload-between-polls replay credits **2400**, not 3600, seconds and does not suspect a plateau. Confirmation has one segment and starts its CPU baseline after observing initialization; it does not reuse live samples. See R2-M1 for the separate own-process accounting problem.
- **R1-B3 — closed.** The farm's default duration clock is `time.monotonic`; realtime is retained for timestamps/reservations. Replayed forward and backward civil steps contribute only the actual monotonic 30/90 seconds and mint no certification. The confirmation driver also uses monotonic elapsed time and a kernel CPU counter. An early civil-time libFuzzer backstop exit cannot pass the required exit-72/stop checks.
- **R1-B4 — closed.** Missing/torn certify verdicts produce `no-evidence`; absent typed counts also prevent certification. The saved-crash/missing-verdict live replay becomes `finding`, records the crash, resets coverage and invokes triage. Live and recovery use `accounted()`; no clean-72 default creates a certificate.
- **R1-M1 — made moot for certification; stop behavior corrected.** The real one-job/two-job SIGINT replay still gives libFuzzer exits **72/134**. Both farm slices are now plain `interrupted`, with no certificate. A suspicion does not claim a clean certified outcome. Certification uses one process, and its abnormal exits fail. The one-process final-stat assumption still needs R2-B1's fix.
- **R1-M2 — closed by removal.** No starvation median or inherited worker-capacity rate survives. The old rate-collapse scenario now accumulates monotonic heuristic time and can stop the flat leg; it writes no certificate. The original persisted jobs-4 → jobs-1 fixture was also replayed: **7290** heuristic seconds, no rates/median, a suspicion and no certificate. The separate certifier uses a CPU window and execution floor instead of that median.
- **R1-M3 — closed.** The removed private-route scenario reports only `1 other legs`; current private targets likewise remain unnamed. Naming is based on positively public targets. Farm journal reasons, certify summaries, random non-public certify unit names, and fixed unit descriptions follow that policy in the inspected and fixture-tested paths.
- **R1-M4 — closed for the repeated-donation defect.** `launch_shares()` splits the initial cap among selected legs; retirement does not redistribute. The original A/B/C allocation stays **10/10/10**, rather than 14/14 followed by a second donation; a fresh `--only B` launch allocates the cap to B but the global guard subtracts every night's recorded CPU and pending allowance. Selftests cover sequential launches and time voided by a finding.
- **R1-m1 — original paths closed; replacement validation remains incomplete.** The old impossible `plateaued/certifies` ledger is unusable rather than certifying. A new coverage history beginning at 3600 without its zero-time row is rejected. Short confirmations stay uncertified, and stored boolean assertions are rejected. R2-M3 and R2-m1 describe remaining weaknesses in the new evidence/certificate reader.

## Other requested checks

- **Fork-mode claim:** verified against `node-sink/src/sink.rs:138–229`, including its refusal tests. `fork`, `reload`, `print_funcs`, `stop_file`, `features_dir` and `seed_inputs` are not admitted; the first output corpus must be the target's working corpus, which also excludes fork-mode temporary corpora. Using one process is consistent with the amendment. `certify_engine()` refuses a future fork-capable allowlist until an engine is implemented.
- **Merge and INITED baseline:** segment 1 performs `-merge=1` with the working corpus first and private/public/fixed seeds as additional inputs. Segment 2 fuzzes that working corpus alone. LibFuzzer keeps feature-adding merge inputs and loads its initial corpus before printing INITED; the baseline reader selects an anchored `#N INITED` with cov/ft and ignores earlier load statistics. Merge failure cannot certify. I found no dropped-corpus baseline problem in that sequence. The driver's looser substring recognition should be aligned with the reader as noted in R2-M1.
- **Executions:** the single-process window uses final `number_of_executed_units` minus the INITED count, with negative arithmetic invalidated and the default/per-target floor applied. This is not the merge's execution count or the multi-job duplicated parent total. The real interrupted snapshot experiment still exceeds the floor, which is why that floor does not fix R2-B1.
- **PID reuse, namespaces and cgroups:** the driver observes its own unreaped child, uses `wait4` for that exact PID, and stops polling on reap. Ordinary PID reuse cannot insert another process into that polling interval. No process-group scan or cgroup aggregate supplies certification credit. Same-namespace `/proc` visibility failures cannot manufacture CPU time. Namespace changes and malicious PID-namespace manipulation were not stress-tested; the concrete child-accounting defect is R2-M1.
- **7fa7b9438:** the finishing done row and removal of its `given` reservation happen under the same lock; the scheduler reads records plus reservations under that lock. The retained fixture observes **2 CPU-h recorded, 0 pending, 2 CPU-h left** in a 4-hour plan, rather than double-counting the finishing slice. Inspection also checked that an exceptional future clears its reservation. Normal resume/cap tests pass.
- **Cap guard limit:** the guard constrains new allowances using all recorded slice CPU, including findings, plus pending reservations. It does not preempt actual running CPU at the allowance. `run.sh:514` gives segments a 60-second minimum and a 30-second wall cushion, so ordinary slice overshoot remains possible; the README's literal claim that actual CPU can never exceed the cap is stronger than the implementation. This is not the original repeated-redistribution bug. Builds and triage also remain outside that slice ledger cap. No large live cap experiment was performed.
- **Live heuristic:** it cannot mint a certificate or a `plateaued` ledger status. It can stop a CPU-starved but initialized flat leg after a wall-time heuristic window, which the amended spec permits. Growth, loads, findings, malformed histories and resumed nights are covered by the replay/selftests. Shares and monotonic launch deadlines normally bound work; a farm receiver that ignores its single SIGINT would also require an external stop/escalation because the executor waits for running futures. R2-M4 is the separately reproduced certifier escalation error.
- **BL-83/BL-86:** retained tests verify slow-unit exclusion from findings/stopping, actual timeout failure, separate slow-unit counts, and exactly one worker-log contribution to execution/sink counts. The mixed-log fixture still totals **357** executions and one sink failure rather than double-counting parent copies. Triage selects each worker once.
- **Priority and privacy:** launch-command fixtures verify `Nice=19`, idle IO, `CPUWeight=10`, quotas and fixed descriptions. Non-public certify unit names are random and target identity is passed through the unit environment, as specified. Public output names only positively public legs; private/removed legs are counted. Certificates stay in the private run tree, with atomic 0600 writes and no private path in the certifier summary. No real journal/unit was inspected or launched. Direct foreground `certify` sets niceness; the full IO/cgroup properties are supplied by `--launch`.

## Validation and limits

Commands used private scratch sinks and on-disk `TMPDIR=/work/scratch/fz1-r2-sol/tmp` (0700), with test/build processes at `nice -n 10` and Python bytecode disabled. The run.sh selftest's hard-coded `/work/scratch/node-fuzz-selftest.*` destination was redirected by a narrowly scoped `mktemp` shim into `/work/scratch/fz1-r2-sol/fixtures/`; the runner source was unchanged. The shared oracle archive cache is `/work/cache/oracle`; the detached worktree keeps its own target directory.

```text
python3 -I node-fuzz/fuzzctl.py selftest
  101 tests, 585.936 seconds: OK
python3 -I node-fuzz/fuzzctl.py check
  OK: 40 targets, 39 smoke legs, 39 farm legs
bash -n node-fuzz/run.sh
  passed
git diff --check c7ef911bb..4b08cdcbc
  passed
run.sh --selftest
  passed: node-fuzz selftest: ok (actual Rust target built in the detached worktree)
r2_evidence.py, state_fixtures.py, driver_fixtures.py, sigint_replay.py --instrumented,
wall_cap_fixture.py
  results and persistent fixtures retained under /work/scratch/fz1-r2-sol/
```

The interruption reproducer uses the actual Cargo.lock-selected libFuzzer library; the r1 interruption binary is reused for the one-/two-job replay. Confirmation timing is exercised with short owned processes and synthetic evidence where explicitly identified. No new real 3600-CPU-second confirmation was run. The supplied 300-second production smoke is consistent with `growing`, but does not exercise a successful full gate or the final-snapshot race.

No real farm/certify unit, real embargo sink, or `~/private` file was read or changed. Owned fixture processes were signalled only by exact IDs/groups. No GitHub action occurred. No reviewed source was edited, committed or pushed. The report is uncommitted; the review worktree remains detached and clean at the requested head. No full Rust workspace build was needed for these Python/shell changes; the runner selftest builds its actual Rust selftest target locally.

**Verdict:** REQUIRES CHANGES — the replacement certifier still has false-success paths; the original live-certification blockers are closed, but FZ1 is not safe as a consensus merge gate.

Sol gate satisfied: no
