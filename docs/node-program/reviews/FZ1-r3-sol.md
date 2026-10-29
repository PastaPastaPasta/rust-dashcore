# FZ1 r3 — Sol review

**Scope:** `node/fz1-plateau`, exact head `1e06f2f2e5d709c59b080c637dd935172238d3b8`; review range `4b08cdcbc0b430e3f5b3c30c8f23ea5884759b4f..1e06f2f2e5d709c59b080c637dd935172238d3b8`. Reviewed all four commits and all five changed files: `node-fuzz/fuzzctl.py`, `run.sh`, `fuzzctl_test.py`, `README.md`, and `probes/sigint_probe.cpp`, with the earlier certifier, farm, triage, registry, sink, and locked libFuzzer implementation for context. Detached review worktree: `/work/scratch/fz1-r3-sol/wt`.

**Binding specification:** DEC-184 and DEC-186, including amendments 1 and 2, in `~/workspace/node-program/DECISIONS.md`. The controlling requirements are an own-process CPU window after INITED, the execution floor, successful before/after corpus replays, equal replay cov and at most 0.1% replay ft growth, no window growth signal, and no open or unreadable finding at completion. A successful `fuzzctl certify` is a merge-gate pass.

The changes fix most reported r2 mechanisms. Certification still has false-pass paths: two in the window evidence protocol and one in triage error handling. They are reproduced below. The additional status-reader finding does not make the already-failing command exit successfully, but misreports its result.

## Closure of the r2 items

The r2 scenario definitions, real-library interruption binary, and accelerated-clock experiment were reused or adapted to the new four-segment protocol. Adaptations give fixtures the newly required source/run identity, replay segments, and one-second overrun. Every new sink is an owned 0700 directory under `/work/scratch/fz1-r3-sol/fixtures/`. The old r2 artifacts were preserved.

- **R2-B1 — the reported torn-NEW scenario is fixed; the underlying interruption class is not closed.** Coverage now comes exclusively from segments 2 and 4. Missing or failed replays fail. The r2 binary's interrupted NEW line, with flat real replays, now produces `growing`, including with synthetic qualifying timing. The maintained `RealInterruption` regression also passes against the locally built locked archive. However, an interruption before any discovery text reaches the log still certifies; the one-second overrun is not a publication barrier. See **R3-B1**.
- **R2-B2 — the reported open-finding-during-run scenario is closed.** `certify()` triages before and after the runner and repeats the finding/pending-run check before returning. The adapted open-cluster, unreadable-triage, and untriaged-run fixtures all return **1**. Open and unreadable triage state also override the status reader. Two adjacent gaps remain: diagnosed vector/provenance errors do not block the command (**R3-B3**), and an untriaged run does not override the stored-evidence status (**R3-M1**).
- **R2-M1 — closed.** The stop proof now uses `/proc/<pid>/stat` utime + stime only, without cutime/cstime, and keeps that stop measurement instead of terminal descendant-inclusive rusage. Repeating the real child-CPU experiment reaps the 2.3-CPU-second child, then reaches the wall cap. Own CPU since INITED is **0.01 s**; the parent's independent own-CPU reading is **0.049388229 s** total. The child no longer supplies the requested window. INITED detection is anchored to a status line.
- **R2-M2 — closed.** The source sampler strips caller `GIT_*`, overrides fsmonitor and personal excludes, explicitly inspects tracked flags and untracked paths, and treats Git errors as unknown/dirty. The hidden-untracked sparse-clone fixture at the reviewed head reports `?? untracked-source.rs` with `status.showUntrackedFiles=no`, also with a foreign `GIT_WORK_TREE`. Sparse checkout also conservatively marks skip-worktree entries dirty; that can only block. The maintained source-state test covers personal excludes, info/exclude, an untracked ignore file, assume-unchanged, and a foreign index.
- **R2-M3 — closed for the reported identity/window contradictions.** The verdict, complete typed `window.json`, source record, and directory identity are cross-checked; CPU stop measurements must prove the window and overrun. The copied foreign-leg certificate is now `invalid`, as are the differing exit/window length, absent/short stop CPU, and missing/unknown commit fixtures. Unknown revisions with a matching source record remain uncertified. Source clean/dirty disagreement is rejected, and the copied binary's digest is part of the source record.
- **R2-M4 — closed.** Escalation is handled before the no-INITED branch. The original accelerated-clock, SIGINT-ignoring initialization fixture now finishes with **137**, writes `stopped_by=wall-cap`, `killed=true`, `status=-9`, and has no INITED CPU baseline. No fixture cleanup kill was required for that driver.
- **R2-m1 — closed.** `lexists` distinguishes absence from an existing unreadable/null certificate. Both old cases now return `invalid`; the readable mismatching certificate is also invalid. Derivation without an initially stored certificate remains supported.

Closure evidence: `r2-evidence-results.json`, `review-results.json`, `driver-results.json`, `real-interruption-test.log`, and the preserved fixture trees under `/work/scratch/fz1-r3-sol/`.

## New findings, by severity

### R3-B1 — blocker: the CPU overrun does not close the interval between coverage growth and discovery publication

**Location:** `node-fuzz/fuzzctl.py:1280–1282`, `:1381–1402`, `:1511–1523`; `node-fuzz/run.sh:631–638`, `:711–714`; `node-fuzz/README.md:207–208`.

**Scenario:** libFuzzer updates coverage/features and then enters `ReportNewCoverage`. That function prints the discovery, writes the corpus, and only then increments `NumberOfNewUnitsAdded`. SIGINT can terminate it before the first discovery byte is published. The replay corpus is unchanged, the log has no NEW/REDUCE/growing/torn status line, and `new_units_added` is still zero. Running an extra second of CPU establishes elapsed CPU, not that this in-flight operation completed. The README's assertion that such a discovery must therefore lie after the window is false.

**Evidence fixture:** `pre_header.cpp`, `rerun_pre_header.py`, `driver_probes.py`, `gate_probes.py`, `driver-results.json`, `gate-results.json`, and `fixtures/pre-header-discovery/`.

This probe uses the actual locked `libfuzzer-sys 0.4.13` archive. It is the r2 two-edge callback with its second PC marked as an edge within an already-covered function, so a NEW_FUNC message does not announce it first. A `vfprintf` wrapper lengthens the existing interval immediately before the NEW header's first write. It does not alter libFuzzer's coverage bookkeeping, signal handling, final statistics, or corpus writes. The wrapper records the library's **actual** `TPC.GetTotalPCCoverage()` at this point:

```text
ReportNewCoverage reached before NEW header; cov=2 calls=1200001 own_cpu=0.434691
```

The exact reviewed driver observed INITED at **0.41 own CPU-s**, requested **4 CPU-s**, and stopped at **5.42 CPU-s** with exit **72**. Thus the already-recorded coverage growth occurred about **0.025 CPU-s after the baseline**, inside the requested window, despite the one-second overrun. Real `-runs=0` replays both give **cov=1, ft=1**. The real log has **1,199,999** window executions, **zero** discovery lines, **zero** unit lines and **zero** units added. The actual short run is correctly `uncertified` solely because it is short.

With explicitly synthetic, mutually consistent 3600-second timing metadata and the same real logs/replays, both `read_certificate()` and the actual `certify()` orchestration return **`certified`**, and the latter exits **0**. The observed short timing is preserved separately as `window-observed.json` and `verdict-observed.json`. This is a fault-injection proof of the stop protocol's race, not a claim that an ordinary production target or a real hour-long run exhibited it. Its applicability does not depend on the requested window length.

**Fix:** stop cooperatively at a libFuzzer iteration boundary after pending coverage publication/corpus saving, rather than equating a fixed delay with completion. The locked library already has a SIGUSR1/SIGUSR2 graceful-exit request checked by `MaybeExitGracefully()` at the mutation-loop boundary; investigate and verify that protocol, including its different exit status, required terminal evidence, and fail-closed kill fallback. Preserve replays and growth vetoes. A pending operation or unacknowledged graceful stop must not certify. Retain this before-header probe alongside the torn-header regression.

### R3-B2 — blocker: a torn final units-added statistic defeats the lost-output guard

**Location:** `node-fuzz/fuzzctl.py:1284`, `:1294`, `:1324–1327`, `:1383–1384`, `:1398–1402`, `:1513–1515`.

**Scenario:** the lost-output check trusts a decimal substring from the very output channel whose writes may have failed. `UNITS_ADDED.search()` accepts a numeric prefix with arbitrary trailing bytes and no terminating newline. A real count of **11** partially written as **1** is therefore accepted as a complete count of 1. One surviving harmless REDUCE line satisfies the guard while ten growth reports are missing. Later statistics can resume after the partial field without making `discoveries()` reject it.

**Evidence fixture:** `lost_output.cpp`, `rerun_lost_output.py`, `driver_probes.py`, `gate_probes.py`, `driver-results.json`, `gate-results.json`, and `fixtures/lost-output-torn-counter/`.

The C++ probe links the actual locked library and injects `vfprintf` failure/short-write outcomes. Its callback has eleven manually instrumented edges. A first harmless shrink prints REDUCE with unchanged **1/1** coverage. Subsequent real discoveries raise the library's cov/ft to **11/11**; their status output is lost. The wrapper records real values offered by libFuzzer to its print functions and final counter:

```text
new_units_added=11 cov=11 ft=11 calls=9001236 first_growth_cpu=0.086123 own_cpu=5.15099
```

The on-disk final statistics instead contain:

```text
stat::new_units_added:          1stat::slowest_unit_time_sec:    0
```

The reader obtains `units_added=1`, `unit_lines=1`, `new_lines=0`, and **9,001,234** window executions. Both real replays remain **1/1**: the controlled callback's new paths depend on accumulated process state and are not reproduced by loading the saved input once. In production, failure to save a discovery to a full corpus sink is another reason replays cannot recover missing live evidence; the locked library does not propagate its `Printf` write errors.

The real 4-CPU-second experiment remains short/uncertified. With the explicitly synthetic qualifying timing metadata, the unchanged real log/replays yield **`certified`** and the actual `certify()` function exits **0**. Restoring just the complete actual counter, `11`, makes the same evidence **`failed`** with “fewer NEW and REDUCE lines than units libFuzzer added.” This isolates the guard failure from the replay criterion. The wrapper is explicit fault injection, not a claim of a naturally observed ENOSPC incident.

Two smaller evidence fixtures also certify with a non-newline-terminated `new_units_added: 0`, or with an earlier complete zero followed by a torn newer units-added field. Selecting the last *parseable* occurrence silently falls back to stale evidence when the latest occurrence is incomplete.

**Fix:** require exactly one complete, anchored terminal statistic for each required final counter, retaining newline/record boundaries; reject numeric suffix garbage, unterminated fields, duplicate counters and malformed later occurrences. Apply the same completeness discipline to the execution count. Keep the units/lines check, but do not use a partial/stale statistic as its denominator. A stronger protocol should surface libFuzzer output/corpus write failures independently of parsing that output. Add the real-library short-write probe and both small malformed-counter fixtures.

### R3-B3 — blocker: mandatory triage errors are diagnosed but do not block `certify`

**Location:** `node-fuzz/fuzzctl.py:825–854` (`Triage.reconcile`), `:639–647` (`vector_problem`), `:1555–1565` (`open_finding`), `:2387–2389`, `:2409–2411`, `:2425`.

**Scenario:** a previously fixed finding has a missing regression vector, or its recorded provenance is unreadable. The triage state itself is well formed and says `fixed`. Both pre-run and completion triage diagnose an error, but `certify()` discards the first triage result and never tests the second one's error count. `open_finding()` tests cluster state and pending runs, without validating vectors/provenance. Certification can therefore succeed while the tool itself says its findings evidence is UNKNOWN, and a missing regression input may never reach the merge or confirmation window.

**Evidence fixture:** `triage_errors.py`, `triage-error-results.json`, `triage-errors.log`, `fixtures/missing-fixed-vector/`, and `fixtures/unreadable-fixed-provenance/`.

Each fixture has a valid fixed cluster on v24.0 and an otherwise qualifying confirmation supplied by the existing fixture runner. No actual farm/certify unit or production target runs. In both cases:

- actual `certify()` calls the runner, prints **`status=certified ... (all checks hold)`**, and returns **0**;
- another triage pass reports **1 error**;
- `status_lines()` reports **`SW-FUZZ open findings: UNKNOWN (triage errors: 1)`**;
- `certificates()` reports the leg as **`certified`**.

**Fix:** propagate the target's pre-run and completion triage errors into certification refusal/failure, including vector and provenance validation. Use one shared fail-closed leg eligibility check for the command and status reader. Preserve the relevant completion snapshot in the certificate derivation; a fixed label must not substitute for readable evidence of the finding and its regression vector. Add both diagnosed-error fixtures at the command boundary.

### R3-M1 — major: the status reader reports a certificate that the completion check rejected

**Location:** `node-fuzz/fuzzctl.py:1587–1590`, `:2403–2411`; contrast `open_finding(..., untriaged=True, before=ended)` in the command.

**Scenario/evidence:** the `untriaged-during-certify` fixture creates a fresh, unresolved run directory of the same leg during a qualifying confirmation, with no mode/input/verdict published yet. This models a job killed between creating its run directory and writing its mode; jobs take no leg lock. It does not model a farm writer bypassing the exclusive corpus lock. `certify()` correctly returns **1**, prints `status=finding`, and leaves the certificate evidence on disk. Immediately reading `certificates()` reports **`certified`**, because that reader calls `open_finding()` without `untriaged=True` and the certificate evidence omits the command's final eligibility result. There is no open cluster yet precisely because that run's findings are still unknown. Evidence: `review_fixtures.py`, `review-results.json`, and `fixtures/untriaged-during-certify/`.

The command's exit code protects a gate that uses it directly, so this is lower severity than the three successful-command failures above. It is still an incorrect evidence-derived certification claim at the very completion where the command refused the leg.

**Fix:** apply the same completion-time pending/error/open-finding rule when deriving persisted leg certification. Record the cutoff/eligibility evidence needed to distinguish pre-completion unresolved runs from later farm activity; merely blocking every subsequent active run would change the intended lifetime of an otherwise valid certificate. Add a command-to-status consistency regression.

## Remaining focus checks

- **Corpus changes and locking:** all supplied working-corpus writers (`run`, `merge`, `certify`) use the same `fuzz/<target>/run.lock`. Certify holds it exclusively through merge, both replays and the window. Farm/merge hold it shared and cannot overlap that exclusive interval. Triage jobs stage their own `run/input` and do not write the working corpus; promotion changes regression directories rather than the already-merged working corpus. No supplied corpus writer bypass was found. External/manual writers do not participate in flock; this review does not assume a hostile actor modifying the evidence tree.
- **Lock crashes and starvation:** real run.sh fixture invocations reject certify under a shared holder and merge under an exclusive holder, before creating a run/build. Killing only each fixture-owned holder by exact PID leaves the lock file present but permits a new exclusive kernel lock. Thus the file is not a stale-lock marker. A surviving child can legitimately retain an inherited descriptor until it exits; cargo/sccache explicitly receive neither descriptor. Certify uses `flock -n`, so a live farm slice makes it fail immediately. There is no waiting reservation/fairness rule to give it the next turn; repeated busy slices can prevent repeated certify attempts from acquiring the leg. Once the exclusive lock is acquired, farm slices cannot starve its window by writing the corpus. Busy retries/coordination are an availability limitation, not a false-success path; these failures are not `starved` CPU windows. Evidence: `lock_fixtures.py`, `lock-results.json`.
- **Replay determinism and removal of INITED equality:** identical cov/ft numbers are cardinality measurements, not proof of identical features or of every transient path. The supplied replay-noise observations also do not prove deterministic replay. This is why a trustworthy live-growth veto remains necessary. Removing INITED-equals-pre-replay is not itself a violation of amendment 2, and the lock/binary mechanisms establish the same supplied corpus and executable. It is defensible as noise handling provided that the live protocol closes R3-B1/B2. Rejecting lower post-replay cov, and ft drops beyond 0.1%, only blocks. Neither replay repetition nor reinstating INITED equality would recover the two probes above: both have equal before/window-INITED/after cardinalities.
- **Live detection only blocks:** the farm's `suspected()`/`plateau-suspected` paths never write a certificate or authorize success. In `judge()`, `new_lines`, positive cov gain, and excess ft gain are vetoes after earlier validity checks; none selects `certified` positively. The stricter NEW/REDUCE/RELOAD/garbled-line discovery predicate is therefore conservative when its input log is trustworthy. The whole live-log evidence protocol still supplies *positive* obligations—initialization, execution totals and output-integrity counts. Those obligations fail in R3-B1/B2; “the growth predicate only blocks” does not establish complete evidence.
- **Binary identity:** the explicit `--target-dir "$here/target"`, serialized build/copy, reassignment `bin="$run/bin"`, and hash of that copy are consistent. Merge, both replay calls and the driver all receive this same `bin`; another wrapper's build after fd 9 closes cannot replace it. The source record binds its digest/run/revision and dirty flag to the verdict. The digest's authenticity ultimately comes from this runner protocol, not from a retained binary after deletion. No binary-selection mismatch was found.
- **Execution-floor registry:** `load_registry()` rejects explicit zero, negative, boolean, float and string floors; a missing override uses 10^6, and positive integer overrides are honored. The independent fixture checks all those types. Allowing an explicit smaller positive floor is authorized by the binding per-target exception. No floor-validation bypass was found.

## Validation and limits

All test/build commands used `nice -n 10`, on-disk 0700 `TMPDIR=/work/scratch/fz1-r3-sol/tmp`, and owned fixture sinks. No real farm or certify systemd unit was launched/stopped; no real embargo directory, `~/private`, or GitHub action was touched. No repository source was edited, committed or pushed. The review worktree remains detached at the requested head. This report is uncommitted.

- `fuzzctl selftest`: **111 tests**, initially **110 passed / 1 skipped** because the fresh worktree had no libFuzzer archive. After the local build produced it, the one skipped `RealInterruption` test was run separately and **passed**. All 111 distinct tests have therefore executed successfully.
- `run.sh --selftest`: **passes** after building the actual instrumented selftest target in this worktree's own `node-fuzz/target`; includes the copy/digest and leg-lock checks. A narrow `mktemp` shim redirects only its hard-coded selftest sink into `/work/scratch/fz1-r3-sol/fixtures/`; reviewed runner source is unchanged.
- `fuzzctl check`: **40 targets**, passes.
- `bash -n node-fuzz/run.sh` and `git diff --check 4b08cdcbc..1e06f2f2e`: pass.
- Independent r2 closure, short-write/interruption, registry, triage-error, command/status and lock fixtures: results retained under `/work/scratch/fz1-r3-sol/`.

The real-library probes use controlled manually instrumented callbacks and explicit output/scheduling fault injection. They do not claim to reproduce a naturally occurring production-hour incident. Their real short timing and logs are retained separately; hour-window metadata is deliberately synthetic to isolate what the gate accepts after its other numerical conditions hold. No actual hour-long certification was attempted. These limits do not remove the demonstrated protocol and error-propagation defects.

**Verdict:** REQUIRES CHANGES. The gate can still accept incomplete growth evidence and unreadable findings evidence; R2-B1's publication race is not closed.

Sol gate satisfied: no
