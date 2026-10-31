# BL-173 narrow review — Sol r2

**Verdict: CHANGES REQUIRED**

## Findings

1. **Medium — A torn timeout report can hide a later unsaved crash in a merge run.** In `node-fuzz/fuzzctl.py:539-552`, when the last logged artifact is a saved `timeout-*` but no `SUMMARY: libFuzzer: timeout` follows, `log_evidence` sets `tail` to `[]`. libFuzzer's merge driver retries failed child processes into the same `fuzz.out` (`libfuzzer-sys-0.4.13/libfuzzer/FuzzerMerge.cpp:491-515`); `merge` is a triaged fuzzing mode. If one child is killed after its timeout write but before its SUMMARY, a later attempt's unsaved `ERROR: libFuzzer` is discarded. I reproduced this with a saved timeout write and timeout ERROR lacking SUMMARY, followed by `MERGE-OUTER: attempt 2` and an unsaved `ERROR: libFuzzer: deadly signal`: `log_evidence()['unsaved_crashes']` was `[]`. Adding the timeout SUMMARY made it `['deadly signal']`. This can lose a real crash count and cluster. Skip only the timeout's own report; when its SUMMARY is absent, preserve evidence from later attempts, and test that case. A second ERROR inside the skipped span is also dropped, although I found no ordinary single-worker path for one in a complete ALARM report.

## Requested checks

- **D8, r1 High:** Resolved for intact worker logs. `artifact_writes` records each write relative to that worker's own interrupt, and `phantom_timeouts` requires every observed write of the shared basename to be after an interrupt. The same-name two-worker tests cover either worker writing a real empty timeout. A missing or torn real-write line remains the documented provenance limit.
- **D9, r1 Medium:** Resolved for the stated manual-SIGINT path. A phantom-only verdict reaches `interrupted` after findings and refusals take precedence; a lone 70/72 exit or a parent 1 with observed worker exits limited to 0/70/72 explains it. The live-slice test checks repeated interruptions, ledger credit, and no leg retirement; the 137 case still fails.
- **D10, r1 Medium:** Resolved within the explicit scope. The resumed-night test keeps old `finding` rows and their credit reset, while new phantom slices earn interrupted credit. An affected old night still needs a fresh night; no ledger history is rewritten.
- **D11:** The locked `libfuzzer-sys` source confirms `AlarmCallback` writes `timeout-*` before its ERROR and SUMMARY (`FuzzerLoop.cpp:278-307`). Under `-jobs`, the parser reads separate `fuzz-N.log` files; the parent's copies are serialized by `WorkerThread` (`FuzzerDriver.cpp:231-252`), so another worker does not interleave with a complete ALARM report. The missing-SUMMARY merge case above remains a hole.
- **Cluster identity and legacy state:** `cluster_id(target, kind, klass)` is unchanged. A saved timeout already had class `unknown` before this delta because its ERROR follows the write; the change stops adding the duplicate, vectorless `crash` class `timeout after N seconds`. Existing open vectorless clusters remain open: `recheck` skips clusters without a vector, and eligibility rejects any open cluster. I would not block landing the code solely on that preexisting state if the manager owns an audited repair, but it must block certification of any affected leg until repaired. A new night does not repair target-wide triage state.

## Validation

`python3 -I node-fuzz/fuzzctl.py selftest`: **175 passed, 5 skipped**. The five skipped cases need a built real libFuzzer binary. Review was read-only; the worktree stayed clean.
