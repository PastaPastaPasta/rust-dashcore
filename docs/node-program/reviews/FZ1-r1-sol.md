# FZ1 r1 — Sol review

**Scope:** `node/fz1-plateau`, exact head `2ecbc4edc154ee8fd73c97614e2e8d20711cebde`, against `c7ef911bbae5ff6104ddab58c4fe70f7420d0002`. Reviewed all three commits (`d0fe28fd2`, `d1372bab9`, `2ecbc4edc`) and the four changed files. Detached review worktree: `/work/scratch/fz1-sol/wt`.

**Binding requirements:** DEC-184 in full, including its corrections and the 60-minute gate-certification requirement; BACKLOG BL-83, BL-86 and BL-87; PLAN §7; `briefs/REVIEW-COMMON.md`; the brief's additional starvation rule. This is a gate-critical review: an erroneous `certifies: true` can approve consensus work without the required fuzzing evidence.

Four blockers, four major findings and one minor finding follow. The blockers were reproduced through the implementation's sampling/settlement paths, rather than merely constructing a desired plateau object.

## Findings

### R1-B1 — blocker: maxima hide a growing job and growth after coverage regression

**Location:** `node-fuzz/fuzzctl.py:1187–1193` (`LogScan`), `:1530` (`Farm.sample`).

**Scenario:** one job's old coverage is higher than another job's current coverage. The lower job discovers edges and features but remains below the higher job's maxima, so neither reported maximum changes. The same failure occurs after a new segment or slice reloads a corpus with lower coverage: both `LogScan` and the leg history retain the old maxima. The restarted fuzzer can grow substantially while the history reports a flat hour. A worker that has already finished can supply the maxima hiding the surviving worker's growth.

**Evidence:** `/work/scratch/fz1-sol/adversarial.py`, cases `lower-job-growth` and `reload-growth`; retained logs, coverage histories and verdicts under `fixtures/` and results in `adversarial-results.json`. In the first case job 0 remains at `1000/10000`, while job 1 grows from `900/9000` to `960/9600` during the hour. In the second, a segment at `1000/10000` is followed by a reloaded worker that grows from `100/1000` to `210/1550`. Both complete through `Farm.slice` with `status: "plateaued"`, `certifies: true`, and zero reported gains at `1000/10000`.

**Fix:** retain separate worker/segment coverage epochs and account for growth in every active worker. Do not treat an old scalar maximum as proof that the current fuzzers are flat. Restart or conservatively invalidate the plateau window when a reload regresses coverage. If a leg-wide union is desired, measure the union of covered PCs/features; maximum cardinalities cannot establish it. Add both negatives to the selftests, including across slices and worker restarts.

### R1-B2 — blocker: endpoint INITED checks credit corpus reloads as fuzzing time

**Location:** `node-fuzz/fuzzctl.py:1179–1194`, `:1515–1525`; `node-fuzz/run.sh:470–501`.

**Scenario:** segment A is INITED at one poll; it ends and segment B finishes loading its corpus before the next poll. Both samples say `fuzzing=True`, but they refer to different segments. `sample` credits the entire intervening wall time, including corpus replay. Reload CPU also qualifies as progress because the counter covers the whole slice process group. The boolean does not establish a full post-INITED interval in one execution epoch. With multiple jobs, `fuzzing` is also an OR: one initialized worker permits the total group's corpus-loading CPU to qualify an interval while others are still loading.

**Evidence:** `adversarial.py`, case `reload-between-polls-v2`. This uses 30-second polls and 90-second segment epochs. Every third interval contains a 29-second corpus reload followed by one second of fuzzing. After 120 intervals, the implementation records 3600 credited seconds and writes a 60-minute certified plateau, although only `3600 − 40 × 29 = 2440` seconds (40 minutes 40 seconds) were fuzzing. The test supplies the specified process-group CPU rate, including replay CPU, and otherwise keeps coverage flat.

**Fix:** identify the segment and worker execution epochs in each sample. An interval spanning an unobserved transition must earn no fuzzing time; reset the sample baseline after the new INITED event. Either measure post-initialization worker CPU separately or exclude mixed initialization intervals conservatively. A slice-wide boolean and two endpoint observations are insufficient to certify a full interval.

### R1-B3 — blocker: CLOCK_REALTIME steps can manufacture a certified hour

**Location:** `node-fuzz/fuzzctl.py:1439` (default `clock=time.time`), `:1514–1525`.

**Scenario:** the first full post-INITED interval experiences a forward wall-clock correction. With no median yet, any positive CPU delta seeds the rate and credits the entire apparent interval. A backward correction is also unsafe when subsequently reversed: `sampled` is overwritten with the smaller timestamp even though that sample earned no time, and the recovery can credit the correction as elapsed fuzzing.

**Evidence:** `adversarial.py`, `forward-clock-step`: 30 actual seconds and 30 CPU seconds, but the clock advances 3600 seconds; the recorded window is 3600 seconds and certifies. `backward-clock-roundtrip`: 90 actual seconds, a backward correction and subsequent recovery; it credits 3660 seconds and certifies. Both write plateau verdicts through `Farm.slice`. The existing backward-step test covers an isolated backward step, not its recovery or a forward step.

**Fix:** use a monotonic clock for sampling intervals and launch durations. Keep a separate realtime clock for UTC benchmark reservations and timestamps. Never derive credited duration or the seeding rate from adjustable civil time. Add forward-step and backward/recovery fixtures.

### R1-B4 — blocker: an interrupted run without a verdict can certify despite a saved crash

**Location:** `node-fuzz/fuzzctl.py:1562` (`libfuzzer_status` defaults to 72), `:1694–1706` (live slice outcome).

**Scenario:** the farm requests SIGINT at a plateau, but the run records a crash and exits before writing a readable verdict, or its verdict is torn. `run_leg` supplies `{}`. The live path derives all finding counts from that empty dictionary, classifies the run as interrupted, and `settle` assumes the missing libFuzzer status is a clean SIGINT exit. It neither inspects the recorded crash nor requires a trustworthy verdict. `reconcile` does inspect artifacts when the verdict is missing; the live path does not use that protection.

**Evidence:** `adversarial.py`, `missing-verdict-with-crash`. After a sampled flat hour, the runner writes `crashes/crash-fixture`, returns code `-2` with no verdict, and indicates that the farm interrupted it. The done row records `status: "plateaued"`, `certifies: true`, `crashes: 0`, and `rev: "-"`, while the saved crash remains present. No run verdict exists. The exact ledger and crash are retained in that fixture directory.

**Fix:** missing or unreadable verdicts must fail closed for certification. Recover actual findings using the same artifact/log inspection as reconciliation, and require independently verified clean termination before settling a plateau. Do not default absent exit evidence to 72. Share one outcome-classification path between live and recovered slices, with saved-crash, sink-failure and torn-verdict negatives.

### R1-M1 — major: the normal multi-job SIGINT path retires legs without a plateau verdict

**Location:** `node-fuzz/fuzzctl.py:1562`, `:1663–1672`, `:1762–1763`; `node-fuzz/run.sh:495–497`.

**Scenario:** the default multi-job farm interrupts the entire process group when the history plateaus. On Linux, the libFuzzer `-jobs` parent does not terminate like a single fuzzing worker. Its abnormal exit is rejected by `settle`, which accepts only libFuzzer status 0 or 72. Nevertheless, the scheduler retires the leg from `plateau_of(leg)` even when settlement did not produce a plateau record. A later launch likewise finds the history plateaued and does not rerun it. The leg is stopped without the required certifying verdict, and `status-line` has no certifying done row to report.

**Evidence:** `/work/scratch/fz1-sol/sigint_fixture.py --instrumented`, using the exact `run.sh` segment loop and verdict tail with a small coverage-instrumented C++ callback linked to the locked `libfuzzer-sys 0.4.13` sources. No farm service was used. SIGINT to the owned slice group produced status 72 with one job, but status 134 with two jobs. The multi-job parent's log contains `Fuzzer::InterruptExitCode(): Assertion 'F' failed`; its workers print normal interruption statistics. Feeding these verdicts and a full flat history into the reviewed `Farm.settle` yields `plateaued` for one job and `interrupted`, with no plateau, for two. Evidence: `instrumented-sigint-results.json` and `fixtures/instrumented-sigint-jobs-{1,2}/`. The parent path in `FuzzerUtilLinux.cpp:25` calls `InterruptExitCode` on SIGINT, while `FuzzerDriver.cpp` enters the jobs path before constructing its `Fuzzer` instance.

**Fix:** implement an interruption protocol that preserves and validates individual worker outcomes while avoiding this parent failure, or explicitly derive a verified clean interruption from those outcomes. Do not broadly accept parent status 1/134 as clean. Retire a leg as plateaued only after its clean plateau outcome has been durably recorded; a failed settlement needs recovery or an actionable failure. Keep a real multi-job interruption test in addition to the current fake-72 test.

### R1-M2 — major: a median learned with more jobs can permanently reject healthy progress

**Location:** `node-fuzz/fuzzctl.py:1477`, `:1523–1527`, `:1537–1538`; coverage rows do not record the worker configuration.

**Scenario:** a night seeds a process-group rate with four workers and resumes with one worker. The median is restored unchanged. A healthy single worker at 0.5 CPU seconds per second is below 25% of the old aggregate rate of 4. All its intervals are starved, so none can adjust the median downward. The same lockout can occur within a slice after several jobs finish. Repeated relaunches retain the poisoned threshold and never reach a plateau despite continued execution.

**Evidence:** `/work/scratch/fz1-sol/state_fixtures.py`, `jobs-change`: persist three credited rate-4 intervals with `jobs=4`, release the worker, reopen the same night with `jobs=1`, and sample two hours at rate 0.5. Rates remain `[4,4,4]`; credited time remains 90 seconds, starved time becomes 7200 seconds, and no plateau exists. `adversarial.py` also demonstrates the equivalent rate collapse within one slice. State and results are retained in `fixtures/jobs-change/` and `state-results.json`.

**Fix:** define the median relative to stable active-worker capacity, persist that capacity, and handle worker completion/configuration changes explicitly. At minimum, reject incompatible same-night resumes or conservatively re-baseline their histories and rates. Provide a bounded, explicit rework/insufficient-progress outcome rather than letting an unrecoverable threshold repeat forever. Individual launches still have deadlines and CPU caps; the demonstrated runaway is repeated wasted work and an unreachable plateau, not an unlimited deadline-free service.

### R1-M3 — major: removed private-route targets are named by the public STATUS line

**Location:** `node-fuzz/fuzzctl.py:1955–1981`, especially `:1974–1975`.

**Scenario:** an old night's ledger contains a private-route target that has since been removed or renamed in the current registry. `farm_plateau_line` intentionally reads the old plan, but classifies privacy using only current private targets. Every unknown target is consequently treated as public and named, violating the counts-only rule for private-route STATUS output.

**Evidence:** `state_fixtures.py`, `removed-private-route`, using only the synthetic name `fixture_private`. An old valid plateau ledger plus a current registry containing only the public fixture produces:

```text
SW-FUZZ farm night=old-night: 1/1 legs plateaued, 1 certifying (DEC-184): fixture_private@v24.0=plateaued@100/1000
```

**Fix:** name only targets positively identified as public, and count unknown historical targets as private. Persist the original route in new plans/records, retain conservative behavior for old ledgers, and ensure later route changes cannot declassify old private evidence. Add a removed/renamed-private-target STATUS fixture.

### R1-M4 — major: redistributed CPU shares can be donated repeatedly

**Location:** `node-fuzz/fuzzctl.py:1125–1127` (`shares`), `:1791–1798` (recalculation after retirement).

**Scenario:** a donor's unused original share is distributed among several growing legs. One recipient spends its enlarged share and then plateaus. Recalculation distributes the original donor's entire unused share again, without deducting the amount that the retiring recipient already consumed. The night can schedule more CPU time than its stated cap. The same issue applies when the original donor was excluded by `--only`. Deadlines limit an individual launch, but repeated launches resume the same plan and can spend the excess allocation.

**Evidence:** `/work/scratch/fz1-sol/share_fixture.py` calls the actual `shares` function. A, B and C each have a 10 CPU-hour original share. C plateaus after 2 hours, so A and B receive 14 hours each. A spends 14 and plateaus. B is then offered 18 hours: its own 10 plus all of C's original 8-hour donation. Total scheduled CPU is `2 + 14 + 18 = 34`, above the 30-hour night cap. The valid remaining allocation for B is only 14. Results are retained in `share-results.json`.

**Fix:** redistribute the globally remaining budget after subtracting every leg's credited consumption, including credits above original shares. Track transfer consumption explicitly or derive open-leg allocations from that remaining pool, with an invariant that total credited consumption plus remaining allowances cannot exceed the original cap. Add the sequential-donor/recipient-retirement case and its `--only` variant.

### R1-m1 — minor: state validation accepts semantically impossible certificates

**Location:** `node-fuzz/fuzzctl.py:1212–1218`, `:1245–1254`, `:1353–1358`.

**Scenario:** a well-typed but damaged or externally backfilled state file is accepted even though it cannot prove a valid plateau. `plateau_problem` checks shape/types, but not `certifies` versus window length, zero edge gain, the feature bound, or sufficient fuzzing time. Separately, `load_coverage` accepts a first row at 3600 seconds with no baseline; `plateau` then selects index `-1` and compares that row to itself.

**Evidence:** `certificate_fixtures.py` / `certificate-results.json`: a 15-minute object with `certifies: true`, `cov_gain: 7` and `ft_gain: 50` at `ft: 1000` loads and produces `1 certifying` in STATUS. `adversarial.py`, `missing-baseline`, loads a single 3600-second row and certifies it; a torn suffix is safely ignored but does not validate the missing baseline. These are manually constructed inconsistent files; I did not establish that a normally generated torn final append alone creates either inconsistency.

**Fix:** enforce certificate invariants when reading verdicts and ledgers, and require a genuine row at/before the nominal window start. Reject a history without its zero-time baseline. Certification should be derived from validated evidence rather than trusted as an independent boolean.

## Confirmations

- **BL-86:** the worker-log selection removes the duplicated parent copies. Existing fixture tests establish 357 executions (`100 + 250 + 7`) and one sink failure across multi-worker and single-worker segments, and triage reads each worker once. Execution and sink-failure accounting pass for the tested intact logs. Missing verdicts remain subject to R1-B4.
- **BL-83:** slow-unit artifacts are excluded from finding/crash counts and from the segment stopping condition, counted separately, and do not fail an otherwise clean verdict. The fixture's real timeout still fails; ordinary crash/OOM/leak prefixes remain findings. This matches the scoped backlog requirement.
- **CPU time versus `#N`:** the choice is justified. The locked libFuzzer prints `pulse` statistics at powers of two, as well as coverage/reduction events; silence does not establish starvation. `/proc` rate can distinguish that silence from lack of CPU. The stat indices correctly cover `utime`, `stime`, `cutime`, `cstime`, and the existing fake-proc test includes live members, reaped children, an unrelated group, and command names with parentheses. The metric still needs execution-epoch and capacity handling (R1-B2/M2).
- **Starvation mechanics in the stable configuration:** the first positive full post-INITED interval seeds the median; intervals below its threshold add only starved time; starved intervals neither advance fuzzing time nor erase prior credited time. Rates and history survive the tested worker restart. Genuine coverage growth observed in logs is recorded even during uncredited intervals.
- **Window selection for valid histories:** choosing the sample at/before the nominal start is conservative. A full nonzero-coverage window is required; one edge or feature growth above 0.1% prevents the ordinary plateau. Short-window verdicts generated by the tool carry `certifies: false`; the default is 60 minutes. See R1-m1 for malformed persisted evidence.
- **Recovery and compatibility:** intact old ledger rows without `slow_units` or coverage history still load. A torn final coverage/ledger suffix is dropped, while malformed complete rows and backward coverage histories fail. The tested intact-verdict recovery is accounted once. Findings with intact verdicts reset coverage and dominate plateaus, and known abnormal exits do not get a plateau verdict. These confirmations do not cover R1-B4's live missing-verdict path or R1-M1's scheduler retirement.
- **`--only` and redistribution:** the option is forwarded and validated, and the existing intact-state fixture redistributes unused shares from excluded/plateaued legs. It does not cover sequential retirements; R1-M4 shows that the budget cap is not preserved in that case. Redistribution itself does not add coverage time. Changing the window from a short stop to a 60-minute launch does not upgrade the old short window into certification without sufficient credited history.
- **Priority:** the launch command sets exactly `Nice=19`, `IOSchedulingClass=idle`, `CPUWeight=10`. Its existing command-fixture test also checks CPUQuota and KillMode. No real unit was launched to verify applied properties.
- **Naming in the current-registry case:** journal messages count private-route legs without naming them, and the normal `status-line` private-route fixture reports counts only. Historical unknown routes break this property as described in R1-M3. The detailed `farm status` output is target-bearing and must remain an operator/private diagnostic.
- **BL-87:** coverage review and target rework are explicitly documented as the next action at low plateaus; the backlog assigns those source-coverage reports and seed/mutator work to FZ2 after FZ1. Their absence is not an additional FZ1 finding.

## Validation and limits

Executed with `nice -n 10`, `TMPDIR=/work/scratch/fz1-sol/tmp` (0700, on disk), and Python bytecode writing disabled. Persistent adversarial fixtures live under `/work/scratch/fz1-sol/fixtures/`, protected by 0700 directories. Scripts and JSON results cited above are retained outside the reviewed branch.

```text
python3 -I node-fuzz/fuzzctl.py selftest
  97 tests, 102.009 seconds: OK
python3 -I node-fuzz/fuzzctl.py check
  OK: 40 targets, 39 smoke legs, 39 farm legs
bash -n node-fuzz/run.sh
  passed
git diff --check c7ef911bb..2ecbc4edc
  passed
adversarial.py, state_fixtures.py, certificate_fixtures.py, share_fixture.py
  reproduced the outcomes stated above
sigint_fixture.py --instrumented
  one-job 72 versus two-job 134, reproduced through the actual run.sh tail
```

The interruption binary was compiled locally from the Cargo.lock-selected libFuzzer sources, with a tiny callback using inline 8-bit counters, PC tables and trace comparisons. Its run loop and verdict generation were extracted unchanged from the exact reviewed `run.sh`. Synthetic plateau tests deliberately inject clocks and process-group CPU values, as the existing fixture harness does; they prove the controller's accounting/settlement behavior, not a completed real 60-minute farm campaign. The real SIGINT experiment uses only short-lived owned process groups and signals them by their exact IDs.

No real farm unit, embargo sink, ledger, or private file was read or modified. No GitHub action occurred. The detached worktree remains clean at the exact requested head; no source edits, commits or pushes were made. No full Rust workspace build or live consensus fuzz campaign was run: this review concerns the Python/shell controller and uses its selftests, adversarial fixtures and the small compiled interruption reproducer.

The `/proc` scan is a non-atomic snapshot and has no process-start identity tracking. PID-reuse races, namespace/cgroup changes and live-to-reaped transitions were inspected but not stress-tested with real production workloads. Unobservable groups yield no credit rather than automatically certifying; they can still prevent progress. The normal runner starts a fresh process group and does not itself change namespaces/cgroups. I make no claim that the existing fake-proc test establishes correctness for all those races. Filesystem log replacement/truncation and read failures also need fail-closed handling before this controller should be treated as a robust gate authority; the established blockers already suffice to reject this head.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
