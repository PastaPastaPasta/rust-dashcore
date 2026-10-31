# FZ1 r5 — narrow Sol review

**Scope:** `node/fz1-plateau`, exact head `2b38a0c917a15361759492f54aaae0f574e31b25`, the one commit after r4 head `b25d94b14e21d03ed60b276be833e9dfcd57933b`. Detached scratch worktree: `/work/scratch/fz1-r5-sol/wt`. Reviewed R4-B1 and R4-m1/m2 only, across the five changed files. No repository source was edited, committed, or pushed. This report is uncommitted.

**Specification:** DEC-186 amendment 3(a) and its accepted `-stop_file` implementation, in `~/workspace/node-program/DECISIONS.md:197`: “A process that outlives the stop by 1,500 s is killed and gets `failed`.” Amendment 3(e) makes an issue requiring injected faults inside libFuzzer's own I/O or scheduling a **RESIDUAL**, but a bypass of (a)–(d), or certification of a natural run, remains **BLOCKING**.

The unbounded environment override is fixed, and both r4 residuals are closed. One **BLOCKING** edge remains in the stop-bound fix: rounding the measured interval down permits a just-overdue exit to certify. Its practical extent is less than half a millisecond; the requested rule has no such tolerance.

## Finding

### R5-B1 — BLOCKING: rounding the stop-to-exit interval can hide an overdue exit

**Locations:** `node-fuzz/run.sh:769`, `:837`; `node-fuzz/fuzzctl.py:1587`.

The driver measures the interval with the monotonic clock, but writes:

```python
"stop_to_exit": None if stopped is None else round(ended - stopped, 3)
```

Both run.sh's verdict and `judge()` then compare only this rounded number against 1,500. An actual measured interval in `(1500, 1500.0005)` can become `1500.0`, which is deliberately accepted. The unrounded duration is not retained elsewhere for either reader to veto it.

**Controlled proof:** `/work/scratch/fz1-r5-sol/boundary_probe.py`, `boundary-results.json`, `boundary-probe-final.log`, and the `fixtures/boundary-repeat-*` trees.

The probe runs the unchanged r4 C++ callback against this worktree's locally built locked libFuzzer archive. Only the extracted driver's monotonic clock is substituted: it follows the real clock until `wait4` reaps the normal-exit process, then supplies an exact boundary timestamp. The production serialization, driver grace, stop-file handling, and library are unchanged. This models the completion branch that misses the grace check: production checks `wait4(WNOHANG)` before testing the deadline. No fault is injected inside libFuzzer's I/O or scheduling.

The real logs are complete and flat, INITED/DONE **1/1**, with more than one million window executions, no discoveries, and both actual replays **1/1**. Qualifying CPU-hour metadata and fixture source provenance are explicitly synthetic, as in the r4 gate probes; this is not an observed natural hour-long certification.

- **Exact bound control:** raw **1500.0**, recorded **1500.0**; run.sh exits **0**, verdict **`pass`**; `read_certificate()` and `certificates()` return **`certified`**; actual `certify()` with the fixture runner exits **0**.
- **Just overdue:** raw **1500.000400000019**, recorded **1500.0**; the same verdict and both readers still pass/certify, and `certify()` exits **0**.
- **One millisecond overdue control:** raw **1500.0009999999893**, recorded **1500.001**; run.sh exits **1**, verdict **`fail`**; both readers return **`failed`**, and `certify()` exits **1**.

This is a narrow arithmetic bypass of the strict bound in (a), rather than a libFuzzer-internal fault residual. It is therefore **BLOCKING** under (e), despite its small practical extent. No natural production false certification was attempted or claimed.

**Remediation:** retain `ended - stopped` without downward rounding for the eligibility comparison. Alternatively, round upward conservatively. Add a driver-to-verdict/reader regression for a raw interval such as `1500.0004`, alongside the existing exact-bound and `1500.001` controls. The existing comparisons can otherwise stay unchanged.

## R4-B1 verification

**Environment bypass closed.** The three constants are **1,500**: `stop_grace` in run.sh, `GRACE` in the driver, and `STOP_GRACE` in fuzzctl. `test_the_grace_is_one_constant` passes and checks their synchronization. Both entry points accept only `0 < value <= limit`, with the poll limited to **10** seconds. NaN and either infinity fail that chained comparison.

An independent matrix tried, for each override, **60000, nan, NaN, inf, -inf, 0, -1, bound + 0.001, empty, garbage, and an underflow-to-zero value**. All **22** cases exit **2** in run.sh's argument checks, before any fixture target directory/build is created, and exit **1** in the extracted driver, with no child started and an empty segment directory. Exact-bound and shortened controls pass the argument checks. Results: `/work/scratch/fz1-r5-sol/override-results.json`.

**Interval validation works for recorded values.** `window.json` requires the new field, finite and nonnegative when present; a stop without an interval, or an interval without a stop, reads **`invalid`**. Independent complete reader fixtures at **1500.001** and **60000.0** read **`failed`**, and **1500.0** still certifies, through both `read_certificate()` and `certificates()`. The maintained verdict tests also reject overdue normal exit 0, SIGINT exit 72, and kill exit 137. Results: `reader-results.json`.

**Wall-cap behavior is conservative.** The interval veto precedes the `starved` return. An overdue wall-cap kill now reads **`failed`**, preserving the fact that it violated the stop bound; it cannot upgrade any run to a certificate. A normal wall-cap SIGINT with prompt exit still reads **`starved`** in both readers. A kill with a shortened fixture grace, whose interval stays within the fixed bound, may still be `starved`; that does not certify either. The rounding gap above is the exception to the strict measured-duration rule.

**R4 grace probes rerun.** `grace_target.cpp` is a byte-for-byte copy of r4's callback (SHA-256 `293a86479255b68b919fdcc51154eddd4bf09a23bd712df0b296ca59b69cf614`). `grace_probe.py` extracts the new driver's code; `grace_gate.py` reruns the actual `certify()` command logic through a fixture runner and reads aggregate status.

- **60000 and NaN:** driver exit **1**, no files written or fuzzer launched; the old override proofs cannot run.
- **Explicit 1500:** measured interval **2034.210 s**, SIGKILL/exit **137**; hour-equivalent reader **`failed`**, `certify()` exit **1**, aggregate **`failed`**.
- **Default grace:** measured interval **2012.465 s**, SIGKILL/exit **137**; reader, command, and aggregate likewise fail.
- **Missed-kill control:** suppress only the driver's kill to exercise the independent interval veto. Normal library exit **0**, unkilled, complete flat DONE, measured interval **8948.238 s**; reader **`failed`**, `certify()` exit **1**, aggregate **`failed`**. Changing only its interval to a prompt **2.0 s** makes the otherwise qualifying fixture certify.

The final overdue runs accelerate only the driver's clock by **10,000**; the library's clock, timeouts, scheduling, output, and callback remain unchanged. Initial ×1,000 repeats sometimes exited within the bound (recorded **603.696** or **862.439** scaled seconds), and correctly passed the synthetic-hour reader control. Those are under-bound controls, not surviving overdue bypasses. Original attempt logs are retained separately. Final evidence: `grace-results.json`, `grace-gate-results.json`, `grace-probe-final.log`, and `grace-gate.log` under `/work/scratch/fz1-r5-sol/`.

## R4 residuals

### R4-m1 — RESIDUAL, closed: apostrophe in the artifact prefix

`LOOP_LINES` now permits the literal prefix through libFuzzer's fixed `'; Test unit written to ` delimiter. This matches the locked library's `Printf("artifact_prefix='%s'; Test unit written to %s\n", ...)` format.

The maintained complete-log apostrophe fixture passes. The new real-library slow-unit regression passes for both the plain and `o'b` sink paths, with normal stop-file exit 0, complete flat DONE, and no grammar problems. Re-reading the exact r4 plain/apostrophe real logs with the new parser also returns no problems for either. Evidence: `real-tests.log` and `r4-slow-log-results.json`. No remaining issue found in this residual's scope.

### R4-m2 — RESIDUAL, closed: README certificate shape

`node-fuzz/README.md:263` now names exactly `{"evidence", "cutoff", "eligibility"}`, matching `certificate_record()` and amendment 3(d). The added text describes bounded overrides, stop/interval consistency, the overdue-exit veto, and the wall-cap kill behavior. No remaining documentation issue found in the requested residual's scope.

## Validation and limits

- **37 maintained tests passed:** `Certifying`, `RunShCertifyWindow`, and `RunShVerdict`, including override rejection, synchronization of constants, interval/schema consistency, reader boundary fixtures, wall-cap status, and run.sh's verdict.
- **2 focused real-library regressions passed, zero skips:** the accelerated-clock overdue-stop test (kill and missed-kill paths), and the natural slow-unit test (plain and apostrophe paths). The new C++ probe was built locally against this worktree's locked `libfuzzer-sys 0.4.13` archive.
- `fuzzctl check`: **passes**, 40 targets. `bash -n node-fuzz/run.sh` and `git diff --check b25d94b14 2b38a0c91`: **pass**. Independent override, reader, r4 callback/gate, and raw-boundary proofs completed with the outcomes above.
- **Full runner selftest did not complete.** `run.sh --selftest` built the locked libFuzzer archive, but continued compiling unchanged dependencies. After the scoped proofs finished, I stopped only that optional local exec process group, verified from its exact cargo PID, scratch cwd, and isolated session/group IDs. It ended **143**; no full-runner pass is claimed. Initial real-probe links attempted while the archive was still being written failed; reruns against the completed archive passed. This was a build-order issue in my first attempt, not a finding against this head.

All builds/probes/tests used `nice -n 10` and on-disk 0700 `TMPDIR=/work/scratch/fz1-r5-sol/tmp` (test-local temporary directories are beneath it). Only fixture sinks were used. A narrow mktemp shim redirected the runner selftest's scratch prefix into this review's fixture tree. No `~/private` reads, real farm/certify-unit starts or stops, process kills by command-line pattern, GitHub actions, commits, or pushes occurred. The detached reviewed worktree remains clean at the exact requested head. No actual 3,600-CPU-second certification was attempted. The earlier general review and r3 findings are not reopened.

**Verdict:** REQUIRES CHANGES. The unbounded overrides and both r4 residuals are fixed, but R5-B1 permits a measured stop-to-exit interval just above 1,500 seconds to round down and certify. Preserve the unrounded duration, or round upward, before applying the strict bound.

Sol gate satisfied: no
