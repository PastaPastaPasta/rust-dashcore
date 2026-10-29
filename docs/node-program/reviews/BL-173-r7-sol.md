# BL-173 narrow review — Sol r7 (`ff2941ccf`, D18)

## Finding

1. **Nit — a test docstring still states the superseded behavior.** `node-fuzz/fuzzctl_test.py:3071-3075` says triage reads an interrupted empty timeout as a harness event. D18 makes it a finding with an advisory label, as the test at line 3175 verifies. Update the docstring so a reader does not mistake the old rule for the current one.

## Review evidence

- `crash_files` keeps every `timeout-*` file; `run_findings` counts it, and `findings_of` returns a timeout finding. `Triage.triage_run` records the finding before adding the optional `suspected_interrupt` metadata. The label has no branch in slice accounting, credit, retirement, certification, or nightly status. `Farm.accounted` takes the maximum of the saved verdict and on-disk findings, then returns `finding` before considering interruption. Certification likewise checks the counted crashes and open triage clusters. The ordinary `--recheck` replay can resolve any nonreproducing finding; it does not inspect this label or infer a phantom.
- `close_phantom` is reachable from the `close-phantom` CLI branch only. Argparse requires `--reason` and `--by`; the function strips and rejects empty values. It appends a synced audit row containing the reason, name, OS user, time, cluster and label evidence before marking the cluster fixed. It rejects unknown, nonopen and incompletely labelled clusters. There is no caller in triage, farm, certify or nightly. The static caller test is meaningful for a new direct call: a scratch mutant adding `close_phantom(...)` to `Farm.accounted` failed `test_no_automatic_route_reaches_close_phantom` (three occurrences instead of two).
- The old `logs_complete`, `segment_jobs`, `job_sections`, `JOB_EXIT`, exit-70 inference, `phantom_timeouts`, `verified_hit`, and `phantom_verdict` logic is gone. `run.sh` no longer writes `segment-N/jobs`. The README and D18's leading note state the new rule; D4–D17 remain explicitly marked as superseded history. The only contrary current statement found is the test docstring above.
- The round's `run.sh` diff only removes the worker-count writer and simplifies construction of the unchanged `-jobs` arguments. The run-mode stop-file argument and `Farm` stop-file → SIGINT → SIGKILL fallback are unchanged. Selftest includes the stop-path tests.

## Validation

- `TMPDIR=/dev/shm/bl173-r7-sol-tmp python3 -I node-fuzz/fuzzctl.py selftest`: **ok, 176 tests, 5 skipped**. The skipped real-window tests require a built libFuzzer binary.
- Scratch mutation: drop labelled timeouts from `crash_files`; `InterruptedSlices.test_an_empty_timeout_after_an_interrupt_is_a_finding_with_a_label` failed because it received `[]` instead of `['timeout-0123']`.
- Scratch mutation: permit an empty reason in `close_phantom`; `ClosePhantom.test_a_close_without_a_reason_or_a_name_changes_nothing` failed because the close succeeded.
- Scratch mutation: add a direct automatic `close_phantom(...)` call in `Farm.accounted`; `ClosePhantom.test_no_automatic_route_reaches_close_phantom` failed. Mutants were confined to `/work/scratch/bl173-r7-sol/mutants/`; the reviewed worktree stayed clean.

Verdict: APPROVE-WITH-NITS
