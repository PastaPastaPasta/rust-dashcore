# FZ1 r4 — narrow Sol review

**Scope:** `node/fz1-plateau`, exact head `b25d94b14e21d03ed60b276be833e9dfcd57933b`; the single commit after r3 head `1e06f2f2e5d709c59b080c637dd935172238d3b8`. Reviewed all seven changed files, with the locked `libfuzzer-sys 0.4.13` sources and the previous r3 evidence. Detached review worktree: `/work/scratch/fz1-r4-sol/wt`.

**Binding specification:** DEC-186 amendment 3(a)–(d) in `~/workspace/node-program/DECISIONS.md`, including the accepted `-stop_file` substitution and the 1,500-second grace. Amendment 3(e)'s termination rule applies: a further issue requiring injected faults inside libFuzzer's I/O or scheduling is a BACKLOG residual; a bypass of (a)–(d) still blocks. This review does not reopen the earlier general certification/farm review.

The r3 findings are closed. One new **BLOCKING** issue remains: the required upper bound on stop completion can be increased or disabled through the caller's environment. Two minor **RESIDUAL** findings concern grammar usability and documentation. No false certification of an ordinary, default-configured hour-long production run was observed or attempted.

## Findings

### R4-B1 — BLOCKING: caller-controlled grace bypasses the required 1,500-second stop bound

**Locations:** `node-fuzz/run.sh:673`, `:710`, `:728`, `:734`; `node-fuzz/fuzzctl.py:324`, `:1577`.

Amendment 3(a)'s accepted implementation requires: “A process that outlives the stop by 1,500 s is killed and gets `failed`.” The driver instead uses:

```python
grace = float(os.environ.get("NODE_FUZZ_CERTIFY_GRACE", "1500"))
...
if not killed and now - stopped > grace:
    os.kill(proc.pid, signal.SIGKILL)
```

`run_leg()` inherits the caller's environment. The clean environment in `run.sh` is applied to the fuzzer, after the driver has read this variable; it does not protect the driver. A direct `fuzzctl certify` therefore accepts `NODE_FUZZ_CERTIFY_GRACE=60000`, `inf`, or `nan`. A larger finite value extends the bound; comparisons against NaN are always false, disabling the kill. The comment says the selftest “waits less,” but the implementation permits waiting arbitrarily longer. The explicitly launched systemd worker's environment whitelist does not pass this variable, but the supported direct command does.

This can matter without a libFuzzer I/O fault: its stop file is checked outside the default five-input mutation round, and several individually non-timeout inputs can keep the process alive past 1,500 seconds. If it eventually exits 0, the reader checks `stopped_by == "cpu-window"` and `killed == false`; it has no stop-to-exit duration to veto an overdue success. `window.json`'s elapsed/wall duration ends at the stop request, not the exit.

**Controlled proof:** `grace_target.cpp`, `grace_probe.py`, `grace_gate.py`, `grace-results.json`, and `grace-gate-results.json` under `/work/scratch/fz1-r4-sol/`.

The callback has one fixed coverage edge and ordinary CPU work/sleeps after 1.2 million calls. It links the worktree's actual locked libFuzzer archive without print wrappers or changes to libFuzzer's scheduling, clocks, timeouts, or I/O. To avoid spending a real 1,500 seconds on a fixture, only the extracted driver's monotonic clock is accelerated by 1,000; an extra diagnostic retains its stop-to-exit interval. The production driver logic is otherwise unchanged.

- **1,500 control:** driver-measured stop-to-exit **1,555.882 s**; SIGKILL, exit **137**, `killed=true`, no DONE; reader/status **`failed`**, actual `certify()` fixture exits **1**.
- **60,000 override:** driver-measured stop-to-exit **4,217.700 s** (4.218 s real); normal exit **0**, `killed=false`; complete flat DONE **1/1**, **1,200,005** window executions, zero units/discoveries; reader/status **`certified`**, actual `certify()` fixture exits **0**.
- **NaN override:** driver-measured stop-to-exit **4,169.466 s**; normal exit **0**, `killed=false`; the same flat complete evidence; reader/status **`certified`**, actual `certify()` fixture exits **0**.

Both real `-runs=0` replays are **1/1**. The qualifying 3,600-second CPU metadata is explicitly synthetic, as in the r3 gate probes; observed short-run metadata and the measured stop delay are retained separately. This demonstrates bypass of the stop-bound rule and the reader/command's acceptance of an otherwise qualifying overdue exit. It does **not** claim an actual 3,600-CPU-second false pass. The defect itself is the unbounded production environment override, not the fixture's clock acceleration. It bypasses (a), so it is **BLOCKING** under (e), not a libFuzzer-internal fault residual.

**Remediation:** remove the production override, or accept only finite positive test values no greater than 1,500 seconds. Preserve shortened fixture graces. Retaining/checking the monotonic stop-to-exit interval would also let completion reject an overdue exit rather than relying solely on whether the polling loop managed to kill it. Add oversized, NaN, and infinity override regressions.

### R4-m1 — RESIDUAL, minor: a natural slow-unit line fails with an apostrophe in the sink path

**Location:** `node-fuzz/fuzzctl.py:1297` (`artifact_prefix='[^']*'`).

The private-path validation does not forbid apostrophes. LibFuzzer prints the prefix literally with `artifact_prefix='%s'; Test unit written to %s`; it does not escape an apostrophe in that path. The grammar consequently rejects a legitimate slow-unit message from such a sink.

**Proof:** `natural_slow.cpp`, `stop_fixtures.py`, `stop-results.json`, and `fixtures/quoted'slow/` under `/work/scratch/fz1-r4-sol/`. The actual locked library executes one ordinary 11-second input, with no interposition or library fault injection. Both the plain-path and apostrophe-path windows end normally at their stop files with flat **1/1** INITED/DONE and zero new units. The plain log has no grammar problems; the apostrophe log has “1 unknown, torn or misplaced lines after INITED,” solely for its natural artifact-prefix line.

This is a false failure, not a false pass, and does not block the consensus gate. Parse the literal prefix through its fixed `'; Test unit written to ` delimiter, or explicitly validate/document supported sink-path characters. Keep a natural slow-unit regression for the supported form.

### R4-m2 — RESIDUAL, minor: the README omits eligibility from the required certificate shape

**Location:** `node-fuzz/README.md:255`.

The invalid-certificate description still says the required shape is `{"evidence", "cutoff"}`. `certificate_record()` correctly requires exactly `{"evidence", "cutoff", "eligibility"}`. Update that sentence to match the implementation and amendment 3(d). This is documentation drift; the reader fails closed.

## Closure of r3 and amendment checks

### (a) Graceful stop

The locked `FuzzerLoop.cpp:883` checks `StopFile` only at the outer fuzzing-loop boundary, before `MutateAndTestOne()`. `ReportNewCoverage()` prints the discovery, saves the corpus, and increments `NumberOfNewUnitsAdded` synchronously before control can reach that check. The stop file cannot be checked midway through publication. Normal loop termination prints DONE, then the optional dictionary, `Done N runs`, and the five final stats; the driver exits 0. SIGUSR1's distinct `_Exit(0)` path is correctly not used.

Node-sink adds `stop_file` to both its allowed flags and path flags: an absolute path under the private target/run tree, with parent traversal rejected. `fork`, `reload`, and `features_dir` remain refused. The sink tests exercise an accepted private stop path, an outside-tree refusal, and refusal of fork/reload combinations. This review retains the private tree's existing trust assumption for same-user symlink/manual modifications.

**R3-B1 closed:** reran the original r3 pre-header C++ probe, retaining its non-function second edge (no earlier NEW_FUNC announcement). Only its CPU spin was shortened from 120 to 6 seconds. Growth was recorded at **1.416 CPU-s**, inside the requested four-CPU-second window; the driver requested its stop at **4.01 CPU-s after INITED** while publication was in flight. Publication then completed: one NEW/unit, DONE **1/1→2/2**, and a saved discovery. Real replays stayed **1/1**. Both short and synthetic-hour evidence read **`growing`**; `certify()` exits **1**, status reads **`growing`**. The maintained `RealWindowStop` regressions also passed, including killing an in-flight publication with a shortened fixture grace and reporting `failed`.

**Stale/planted stop:** `run.sh` exclusively creates a fresh run directory and segment-3; it does not reuse an earlier run's stop path. A colliding directory makes creation fail. Independently planting a nonempty stop file before launching the extracted driver caused a normal early libFuzzer exit but **no recorded CPU stop/baseline**; otherwise qualifying fixture evidence reads **`failed`**. A stop that is consumed early cannot supply the missing own-CPU window proof. The review does not assume a hostile same-user writer also fabricating the evidence files.

**Wall cap and kill:** maintained driver/verdict tests pass for SIGINT wall-cap stops (`starved`), ignored SIGINT before INITED, and an unacknowledged CPU stop followed by SIGKILL (`failed`). CPU-stop certification requires exit 0 and no kill.

**Grace versus timeout:** the default 1,500 seconds exceeds the locked 1,200-second timeout for one callback. It is safe as a fail-closed bound, but does not guarantee that every clean round finishes: `mutate_depth=5`, and the stop-file check is outside that round. Several slow inputs, post-callback work, or I/O can exhaust the grace. A default-grace kill cannot certify. The bounded-failure behavior is the accepted rule; R4-B1 is the separate ability to disable that rule.

### (b) In-process cov/ft

`window_log()` requires DONE to repeat the highest cov/ft printed since INITED, rejects backward counts, and `judge()` separately compares DONE against INITED: equal cov and ft growth at most 0.1%. Replay comparisons remain independent. The maintained tests cover raised/lowered DONE, growth under REDUCE/RELOAD, and a DONE value with no preceding publication. No bypass found in this check.

### (c) Whole-log grammar

Every post-INITED line must be newline terminated and match the allowed phase/form. DONE, `Done N runs`, and final execution totals agree; each of the five stats appears exactly once in order with no trailing bytes. NEW/REDUCE lines must equal `new_units_added` exactly.

**R3-B2 closed:** rebuilt and reran the original r3 torn-counter C++ probe. The real library offered **11** units and cov/ft **11/11**, while the on-disk counter is still `1stat::slowest_unit_time_sec: ...` and discovery/DONE output is lost. The stop is now normal exit 0, but short and synthetic-hour evidence both read **`failed`**; `certify()` exits **1**, persisted status **`failed`**. Restoring the full counter still fails because the rest of the log is incomplete.

The exact two r3 malformed-counter fixtures now fail. Additional independent fixtures start from a complete valid DONE/final-stat record, so the result is not merely rejection of obsolete SIGINT output: an unterminated units counter, a complete stale counter followed by a torn one, numeric suffix garbage, and duplicating/removing each of the five stats all yield **`failed`**. The unchanged complete control certifies.

**Natural formats:** inspected `PrintStats`, `PrintMutationSequence`, `PrintASCII`, recommended-dictionary output, slow-unit output, and final-stat branches against the allow-list. The supplied real 300-second smoke was read through a separate 0700 scratch copy: exit **0**, stop **`cpu-window`**, no kill, DONE present, **8,603 NEW/REDUCE lines = 8,603 units added**, no grammar problems, recommended dictionary present, status **`growing`**. Its evidence still matches its certificate. Default `-print_final_stats=1` emits the five supported counters; extra coverage/corpus-stat options are not supplied by this runner. The harness's natural strict-flag count is allowed; divergence/sink-failure/refusal diagnostics correctly prevent a pass. The apostrophe-path false failure is R4-m1.

### (d) One eligibility function

`eligibility(fdir, target, pin, cutoff)` is used before the runner, after completion triage, and by `certificates()` at the recorded cutoff. Certificates contain exactly `{evidence, cutoff, eligibility}`. Recorded refusal reasons persist, and current open/unreadable findings still veto the stored run.

- **R3-B3 closed:** the original missing-fixed-vector and unreadable-fixed-provenance fixtures now refuse before the runner with exit **2**, zero runner calls, and no certificate. Additional completion-time versions return **1**, persist the vector/provenance reason, and read **`finding`**; completion triage reports the error.
- **R3-M1 closed:** the exact untriaged-during-certify fixture returns **1** and reads **`finding`** immediately. The maintained regression also checks that later clean triage cannot upgrade its recorded refusal. Open and torn-state completion fixtures likewise return **1** and read **`finding`**.
- A pending run after the recorded completion cutoff leaves the certificate **`certified`**, while eligibility without that cutoff correctly reports pending work. This confirms that the reader uses the recorded cutoff rather than now.
- An old-format evidence-only certificate reads **`invalid`**. A triage error on another pin of the target refuses the requested leg before its runner. These behavior changes are deliberate and fail closed.

No new bypass of (b)–(d) was found within this narrow review.

## Validation, evidence, and limits

All builds/probes/tests used `nice -n 10`, on-disk 0700 `TMPDIR=/work/scratch/fz1-r4-sol/tmp`, and private fixture sinks under that scratch tree. A narrow `mktemp` shim redirected only `run.sh --selftest`'s fixed scratch prefix into this tree. No real embargo tree, `~/private`, farm/certify systemd unit, or GitHub action was accessed or launched/stopped. No reviewed repository source was edited, committed, or pushed; the detached worktree remains clean at the requested head. This report is uncommitted.

- `run.sh --selftest`: **passes**, with its local instrumented selftest build and locked archive in this worktree's own target directory.
- `cargo test -p node-sink --locked`: **15 passed**; `cargo clippy -p node-sink --all-targets --locked -- -D warnings`: **passes**.
- `fuzzctl check`: **passes**, 40 targets. `bash -n node-fuzz/run.sh`, `git diff --check 1e06f2f2e b25d94b14`, and `cargo fmt --check -p node-sink`: **pass**.
- `fuzzctl selftest`: both full runs execute **117 tests, zero skipped**; **116 pass**, with one failure in the unchanged live-heuristic test's 30-second wall-time assertion (`Suspecting.test_a_suspected_leg_stops_for_the_night_and_its_slice_is_cut_within_a_poll`): **85.059 s** and **341.055 s**. An isolated rerun **passes in 0.991 s**. All scoped certification regressions, including the three `RealWindowStop` tests, pass. This review does not claim to reproduce the reported 117/0 full-suite result; the wall-time/order-sensitive failure remains a validation limitation outside (a)–(d).
- Independent r3 reruns, complete-counter controls, completion eligibility/cutoff controls, natural-output/stale-stop probes, and grace-bound controls: retained in `/work/scratch/fz1-r4-sol/` with scripts, original real logs, results, and separately identified synthetic metadata.

No actual hour-long certification was attempted. The r3 print-wrapper probes retain explicit library fault injection as regression tests; they now fail or report growth. R4-B1 uses no such injection and is classified as a direct bypass of amendment 3(a). The minor usability/documentation findings are residuals, not merge blockers.

**Verdict:** REQUIRES CHANGES. R3-B1, B2, B3 and M1 are closed, but R4-B1 allows the mandatory 1,500-second stop bound to be increased or disabled and an overdue run to certify.

Sol gate satisfied: no
