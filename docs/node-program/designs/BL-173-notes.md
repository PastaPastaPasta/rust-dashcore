# BL-173: slice stops that hang file phantom 0-byte timeouts

## Problem

The farm stopped a slice with a SIGINT to its `run.sh` process group. A libFuzzer worker could print
its final stats and then hang in its interrupt path. About 1200 s later (libFuzzer's `-timeout`)
its ALARM fired and wrote an empty `timeout-*` artifact. Triage clustered it as a finding; the empty
input replays clean, so `triage --recheck` closed it, and the next hanging stop reopened it. Certify
refused legs and gates showed phantom open findings.

## Decisions

**D18 reverses the automatic part of D4-D6, D8-D10 and D13-D17.** Six review rounds (Sol r1-r6) each
broke the last inference that told a phantom timeout from a real one; the manager ruled (DEC-221) that
no log reading drops, discounts or closes a timeout. What stays: D1-D3 (the stop file and the
fallbacks, the real fix), D7, D11-D12 (a timeout's own report is no second crash), and the label and
manual close of D18. The rows below are the history of what was tried.

- **D1. Slices stop through libFuzzer's stop file.** `run.sh` passes `-stop_file=$run/stop` in
  `run` mode (one file per run, which the harness's flag check already allows inside the private
  tree). Every worker reads it at the top of its loop, prints its final stats and exits 0. The farm
  writes it (`Farm.request_stop`, one byte, as libFuzzer ignores an empty file) for every interrupt:
  a suspected plateau, a bench window, the deadline, a unit stop. Certify's window already ends this
  way; its driver is a separate path and is unchanged.
- **D2. `run.sh` starts no segment once the stop file exists.** A stopped worker exits 0 short of the
  CPU budget, which would otherwise start the next segment, which would stop at once, without end.
  The check is at the top of the segment loop, so a stop written during the build or between two
  segments also ends the run. The verdict of such a run is `fail` (budget unmet), which the farm
  records as `interrupted` (its own flag, or the stop file in the run for a recovered slice).
- **D3. SIGINT and SIGKILL fall back after a grace period.** A slice that outlives `Farm.stop_wait`
  (90 s) after its stop file gets the SIGINT; one that outlives `Farm.int_wait` (30 s) after a
  SIGINT gets a SIGKILL. Both are well inside the 1200 s `-timeout`, so a hang is killed before its
  ALARM can write anything, and together (120 s) inside `imminent` and the unit's
  `TimeoutStopSec=180`. libFuzzer reads the stop file only once fuzzing, so `request_stop` reports
  whether a worker of the latest segment has printed INITED; a slice that has not (it is building,
  which `run.sh` does inside its run directory, or loading its corpus) and triage's run.sh jobs get
  the SIGINT at once, with the stop file also written so `run.sh` starts no segment, and the SIGKILL
  fallback. `Farm.escalate` runs in a daemon thread per interrupted process and ends when the
  process leaves `Farm.procs` (it never signals a reaped group). The farm's worker therefore ends
  within about two minutes of a stop, however a worker hangs.
- **D4. Triage refuses an interrupted timeout (the safety net).** `phantom_timeouts(run)` is the
  empty `timeout-*` artifacts that the run's worker logs write after a `libFuzzer: run interrupted`
  line (an interrupted process exits at once, so an artifact after that line is the ALARM of a hung
  one). `crash_files`, and so `findings_of` and `run_findings`, leave them out, and
  `Triage.triage_run` counts them in `harness_events` (printed in the triage summary). This covers
  runs stopped by anything else (a manual SIGINT, a T3 restart) and runs that exist already.
- **D5. A slice's verdict does not count a phantom either.** `run.sh` counts every file in
  `crashes/`, so `Farm.accounted` subtracts the phantom count from the verdict's `crashes` (the
  recount from the run's files still applies). The slice is `interrupted`, not `finding`; its leg is
  not retired, its credit is not voided, and no triage is started. `Farm.slice` logs the count.
- **D6. What stays a finding.** A timeout of an input with bytes, even one logged after an
  interrupt (the hung ALARM writes the empty current unit, and a real input that hangs is not
  empty), and an empty input that hangs with no interrupt before its ALARM.
- **D7. The startup-job cancel test no longer depends on the shell's SIGINT.** A background job
  (GATE-FAST's nightly lanes) starts with SIGINT ignored, every child inherits that, and bash cannot
  reset a signal ignored on entry, so the old `exec sleep` stub ignored the cancel and `run_leg`'s
  30 s wait outlasted the test's 10 s. The stub is now a Python script that resets SIGINT to
  `SIG_DFL` (as libFuzzer installs its own handler) and touches a ready file, so the cancel is timed
  from there. `test_a_job_is_cancelled_when_sigint_is_inherited_ignored` runs the same scenario with
  SIGINT ignored in the test process. `run_leg` and real farm stops are unchanged: libFuzzer
  overrides an inherited SIG_IGN.

- **D8. Phantom classification is per worker (Sol r1 #1).** libFuzzer names a timeout artifact by the
  hash of its input, so every empty input shares one name across the `-jobs` workers of a run, and a
  run-wide union of "written after an interrupt" basenames could hide a real timeout of the empty
  input behind another worker's later, interrupted write. `artifact_writes` reads each worker log as
  `(name, after_this_worker's_own_interrupt)`, and `phantom_timeouts` calls an empty `timeout-*` file
  a phantom only when it has at least one logged write and every logged write is after its own
  worker's `run interrupted`. A write before an interrupt (in any worker, or earlier in the same log)
  keeps the finding, and so does a file no log shows being written, or any worker log of the run
  being unreadable. A worker that logged nothing about the file is no evidence either way (it is
  not what wrote it), so another worker's interrupted write alone makes a phantom. Residual: a real
  write whose log line is torn, or never printed (a SIGKILL between the file write and the
  `Printf`), is invisible; a replay would not show it either (the empty input replays clean).
- **D9. A verified phantom alone makes a slice `interrupted` (Sol r1 #3).** Without a farm stop (a
  manual SIGINT, a T3 restart) there is no `interrupted` flag, stop file or exit 72, and the `-jobs`
  parent exits 1 while a lone worker exits 70, so such a slice was a `fail` and retired its leg on the
  second one. `Farm.accounted` now returns `interrupted` when, after the phantom is subtracted, the
  run has no divergence, crash, sink failure or refusal, `phantom_timeouts` is non-empty and the exit
  is explained (`explained_exit`: a lone process's 70 or 72, or a `-jobs` parent's 1 whose workers'
  `Job N exited with exit code` lines are all 0, 70 or 72 and not all 0). A sibling the kernel killed
  (libFuzzer reports its raw wait status, 9 for SIGKILL) or an unknown status still fails the slice. Other failure evidence (a real crash, a refusal)
  still wins, and the slice's CPU time is credited like any interrupted slice.
- **D10. Old ledger rows are not rewritten (Sol r1 #2).** The fix is scoped to slices accounted from
  now on, resumed nights included. A `finding` row the old code recorded for a phantom stays a
  record, voids the leg's earlier credit in `credit_by_leg`, and is never reclassified: an audit that
  rewrites a night's ledger needs a row type the validator and the status readers do not have, for
  a night's worth of credit. An affected night is repaired by starting a new night (credit is per
  night; the ledger is append-only). Test: a resumed night keeps its old rows and credits its new
  slices.

- **D11. A timeout artifact's own ERROR line is not an unsaved crash (validator, real-order logs).**
  libFuzzer's `AlarmCallback` writes the `timeout-` artifact first and prints `ERROR: libFuzzer:
  timeout after N seconds`, `SUMMARY` and the final stats after it, then exits 70 (`FuzzerLoop.cpp`);
  crashes print their ERROR line first. `log_evidence` read everything after a log's last
  `Test unit written to` line as an unsaved crash if it held `ERROR: libFuzzer`, so every timeout (a
  phantom too) also counted as a crash libFuzzer never saved, a second finding with no vector that
  `--recheck` cannot close, and D4, D5 and D9 never took effect on real logs: the earlier fixtures had
  the two lines the other way round. Now the lines from a saved `timeout-` write through its `SUMMARY: libFuzzer: timeout` are that
  artifact's own report and are skipped; an ERROR after the SUMMARY (a merge's next attempt) still
  counts, and so does a timeout whose artifact is not in `crashes/` (the write failed: the report is
  then the only evidence). The BL-173 fixtures use libFuzzer's real order (two older ones are marked synthetic). Consequences, none changing a
  cluster id that exists: a real saved timeout is one finding, classed `unknown` as before (the
  second, vectorless `crash` cluster `timeout after N seconds` is no longer made); open vectorless
  clusters of that class that old runs made stay open: fuzzctl has no way to close a vectorless
  cluster, triage state is per target, not per night, so such a cluster holds its leg, and D10's
  new-night repair does not release it (D13 is the repair).

- **D12. Only the timeout's own report is skipped (Sol r2 #1).** D11 dropped everything after a saved
  `timeout-` write, so a torn report (a merge child killed after the write, before its SUMMARY) hid
  the unsaved crash of the retry that libFuzzer's merge driver appends to the same log. The span now
  ends at the report's `SUMMARY: libFuzzer: timeout`, or, without one, at the next `MERGE-OUTER:` /
  `MERGE-INNER:` line or the next `ERROR: libFuzzer` (a second ERROR is not the timeout's own and
  counts). Only the first timeout report after the write is skipped (review of D13: a flag that
  re-armed on every timeout ERROR hid a later attempt's own timeout). A torn report with nothing
  after it is still skipped. Test: `test_a_torn_timeout_report_hides_no_later_attempts_crash`.
  A saved artifact's report also ends, unstarted, at a merge's next attempt (D14).
- **D13. `fuzzctl close-phantom`, the audited repair of the vectorless clusters D11 leaves (Sol r2).**
  Triage state is per target, so an open vectorless `crash` cluster of class `timeout after N
  seconds` (the duplicate the old tail rule filed for every timeout's own report) holds its legs
  against certify (`eligibility`: any open cluster) and nothing else can close it (`--recheck` needs
  a vector). The subcommand lists every open cluster without a vector, per target, and closes one
  only when it is that class and D16's per-hit check passes; every other vectorless cluster (a
  sink failure, a real unsaved crash, a vector-bearing cluster) is listed as kept and never touched.
  The default is a dry run. `--apply --by NAME` (the two go together) takes the triage lock (the dry
  run takes it too, so it can wait behind a farm triage's minimisation jobs), decides every cluster
  of a target and builds every audit row first, then appends the rows
  (`fuzz/<target>/triage-audit.jsonl`, 0600, fsynced: the event, UTC time, `--by`, the OS user,
  cluster id, kind, class, pins, hits, first/last seen, every hit's run, log and artifact, and
  the rule), and only then sets the clusters `fixed` with `fixed_at="close-phantom"` and
  `closed_by` and saves the state once: a failure while deciding leaves no row for a cluster that
  stays open. `fixed_stamp` is empty, so the next run triaged that finds the class again reopens
  the cluster (no code fixed anything; a run that began before the close counts too). Private-route
  targets are counted on stdout, never named (PLAN §7.5); their per-cluster listing goes to
  `fuzz/.close-phantom/<stamp>.txt` (0600).
  What it checks is D16's: per-hit sources, hit by hit. Tests: `ClosePhantom`.

- **D14. close-phantom fails closed (Sol r3).** Two ways a real finding could still be closed:
  1. A merge child that died after a phantom's `timeout-*` write and before its own ERROR left no
     report to skip, and `without_timeout_report` took the next attempt's real timeout ERROR for it:
     `log_evidence` lost the crash and `legacy_unsaved_crash` called the member a phantom. The
     report's eligibility now ends at the first `MERGE-OUTER:` / `MERGE-INNER:` line after the
     write even if its ERROR never appeared. Tests through both: `log_evidence` and
     `close-phantom` (`test_a_report_that_never_began_before_the_next_attempt_hides_no_timeout`).
  2. Counting members against hits, restricted to the runs up to `last_seen` and with a missing-log
     check, was an inference that Sol r4 broke again; D15 and D16 replace it and it is gone.

- **D15. A phantom needs every worker's log (Sol r4 #1).** `phantom_timeouts` knew of a missing
  worker log only through D14's close-phantom check, so a same-name empty timeout that worker 1 wrote
  for real looked like a phantom once worker 1's log was gone, and `Farm.accounted` credited the slice.
  An empty `timeout-*` is now a phantom only when `logs_complete(run)` can show the logs all there:
  segments numbered 1..n without a gap; in a `-jobs` segment (a `fuzz-<i>.log`, or a `Job <i> exited`
  line in the parent's fuzz.out, which libFuzzer prints as each worker exits) the parent's named
  workers are numbered 0..k-1 and are exactly the logs present; a lone process has its fuzz.out
  (superseded by D17: the run now records its worker count, and a lone or merge log is never read).
  A worker whose exit the parent never printed (it was killed, or the parent was) is not shown and the
  run is not complete: its empty timeouts stay findings in
  `crash_files`, `findings_of`, `run_findings`, triage and `Farm.accounted`, which all go through
  `phantom_timeouts`. Besides D8's rule (every logged write after its own worker's interrupt, at least
  one write), read from two places: each worker's `fuzz-<i>.log` and the parent's copy of that log
  after its Job line (`job_sections`), so a log that is present but emptied or cut afterwards cannot
  take a real write with it (validator r5 F1). `artifact_writes` takes libFuzzer's `run interrupted`
  line only at the start of a line, so input text that a crash printed cannot make a real empty
  timeout a phantom (F2; its per-attempt reset for merge logs went with D17). A worker the parent saw exit 70, the
  exit of an ALARM, which writes the artifact first, must also show a `timeout-` write in its log or
  the parent's copy, or the run is not complete (r5b R1: a log emptied and the parent cut just after
  its Job line, as a full disk would leave them). The two residuals this listed, a lone process's
  log emptied (no copy) and a parent that never printed a worker's exit beside that worker's log also
  gone, were fail-open and are closed by D17. `missing_logs` (D14) is gone. Tests: `test_a_known_missing_worker_log_keeps_a_same_name_timeout_a_finding`
  (triage, `run_findings`, `findings_of`, slice accounting) and `test_a_phantom_needs_logs_that_the_run_shows_complete`
  (truncated or absent parent, an unnamed third worker, non-contiguous workers, a missing segment,
  an unreadable parent). A slice killed by the SIGKILL fallback has no phantom (the kill comes before
  the ALARM), and a hung worker's phantom is normally in a run whose parent printed every exit.
- **D16. close-phantom stops inferring (Sol r4 #2).** Four rounds on the same machinery (counts of
  members, hits, ranges and missing logs) each had a way for a later phantom to stand in for a deleted
  old real hit. Removed: `timeout_report_members`, `missing_logs`, the `last_seen` range and the
  totals comparison. Instead triage records where each hit of a vectorless unsaved-crash cluster came
  from: `hit_sources`, a list of `{run, log}` (log as `segment-N/fuzz-<i>.log` or `segment-N/fuzz.out`;
  D17 closes only the former),
  appended only while it covers every hit so far (`len(hit_sources) == hits`); a cluster that has a hit
  without a source, a saved input's or all that predate D16, never has a list that looks complete.
  `close-phantom` closes a cluster only when it is the old `crash` / `timeout after N seconds` class,
  `hit_sources` is a complete list (as many entries as hits, at least one), and every source
  `verified_hit`: the run and log (a plain `segment-N/` log name of a run named by its own basename)
  still readable, the log's pre-D11 reading (`legacy_unsaved_crash`) is that class and is only the
  report of the empty timeout artifact written just before it (no later attempt's ERROR), and that
  artifact is in `phantom_timeouts(run)` (D15), and the log is a `-jobs` worker's `fuzz-<i>.log`
  (D17: a hit read from a `fuzz.out`, a lone process's or a merge's, is never closable). Anything else is kept; a cluster without a complete
  record is listed as `needs manual review` (the `review=` count; private targets in the 0600 file
  under `fuzz/.close-phantom/`) and changed in no way, which is every cluster the old rule made. Since
  D11 triage never counts a phantom's report as a hit, no cluster triage makes now is closable this
  way: the closing path is the audited, tested one for a cluster someone can attribute, and for
  everything older the repair is a human's. The live corpus tree had none open. Audit rows carry
  `hits_checked` (run, log, artifact, phantom per hit) and the rule `BL-173 D15, D16`. Tests: the
  `ClosePhantom` class, among them Sol's intervening post-D11 run (the cluster stays open) and
  `test_triage_records_where_each_unsaved_hit_came_from`.

- **D17. A phantom needs positive proof (Sol r5 #1, #2).** Sol r5 showed both D15 residuals fail open:
  (1) two workers wrote the same empty `timeout-0123`, worker 0 after an interrupt and worker 1 before
  any; with `fuzz-1.log` gone and `fuzz.out` cut just before worker 1's exit line the surviving
  records named one worker and looked whole, so the real timeout became a phantom; (2) a lone-process
  merge log (`MERGE-OUTER: attempt 1`, an interrupted empty write, attempt 2, a real write) cut before
  attempt 2 kept its first attempt's write and, with the saved artifact, classified as a phantom, and
  a recorded `hit_sources` entry for that log made the cluster closable. Rule: missing or incomplete
  evidence is a finding; no more inference from what survived.
  1. run.sh writes the segment's worker count to `segment-N/jobs` (`<J>\n`, mode 0600, the file and its
     directory fsynced) after creating the segment and before starting any worker, only when it passes
     `-jobs` (J > 1; nothing in other modes, nor for one process). `logs_complete` requires, in every
     segment, a `jobs` file holding a count (`segment_jobs`; any other content reads as no file), the
     logs present exactly `fuzz-0.log` .. `fuzz-<J-1>.log`, and the parent's `Job <i> exited` lines
     naming exactly those workers. The exit-70-must-show-its-write guard stays (a worker's log and the
     parent's copy both cut after its exit line).
  2. A segment without the file, so a lone process, a merge or certify run, or a run from before the
     file, makes the run incomplete: an empty timeout in it is a finding, `explained_exit` no longer
     needs a lone 70, and `artifact_writes` no longer resets at merge boundaries. `verified_hit` and so
     `close-phantom` take the same rule through `phantom_timeouts`, and `HIT_LOG` admits only
     `segment-N/fuzz-<i>.log`, so a cluster whose hit came from a `fuzz.out` is kept.
  Runs written before D17 never yield a phantom, so a phantom of an older night shows as a finding
  (the repair is a new night, as for D10). Tests: `test_a_last_worker_gone_from_both_its_log_and_the_parent_is_known_by_the_recorded_count`
  and `test_a_lone_process_or_merge_log_never_yields_a_phantom` (Sol's two reproducers),
  `test_a_phantom_needs_logs_that_the_run_shows_complete` (no file, a count of 3 over two workers,
  malformed counts), `test_a_hit_read_from_a_lone_process_or_merge_log_is_never_closable`,
  `test_a_jobs_segment_records_its_worker_count_before_its_workers_start` (run.sh). Mutation-checked:
  deriving the workers from the surviving records, allowing a segment without the file, admitting
  `fuzz.out` in `HIT_LOG`, and not writing the file each fail their test.

- **D18. A label and a manual close replace automatic classification (Sol r6; DEC-221).** Sol r6 forged
  a worker log line shaped like the parent's `Job 1 exited` and, with a truncated parent, made a real
  timeout a phantom through every route, and a 5,000-digit `jobs` file raised instead of keeping the
  finding. Every layer since r2 (D8 per-worker writes, D15 complete logs, D17 the worker count) had a
  forgeable input; the stop file (D1-D3) makes interrupted empty timeouts rare, so the inference is
  removed rather than filtered again.
  1. Triage, `run_findings`, `findings_of`, `crash_files` and `Farm.accounted` never drop, discount
     or close a timeout on phantom grounds. A slice with one is a `finding` (credit voided for it,
     the leg retiring as for any finding): at most a night's cost. `suspected_interrupts(run)` labels
     an empty `timeout-*` whose every logged write followed its worker's own `run interrupted` line
     (line-start match, so input text cannot forge it; a same-name write before an interrupt
     withholds it): `{segment, worker (None for a lone process), interrupt line}`. It reads the same
     worker logs as the rest of triage, is advisory, and can be wrong in either direction without
     changing a count. Triage keeps `interrupts` (labelled hits) and `interrupt_evidence` (the first
     20: run plus the label) on the cluster, counts `suspected_interrupts` in its STATUS line, and the
     farm logs the count per slice.
  2. `close-phantom CLUSTER --reason TEXT --by NAME` is manual. It closes one open cluster whose
     `interrupts` equals its `hits` (every hit labelled; a cluster from before the label has none and
     is refused), writes the audit row (`by`, the OS user, `at`, `reason`, cluster, class, hits,
     the labelled evidence, rule `BL-173 D18`) with fsync before the state, sets `fixed` /
     `fixed_stamp=""` / `fixed_at="close-phantom"` / `closed_by` / `closed_reason`, and moves the
     cluster's vector to `regress/fixed/` as `recheck` does (`reconcile` finishes a move that dies).
     Without a reason or a name it refuses, with argparse also requiring both. A private-route target
     is never named. No farm, nightly or triage code calls it.
  3. Deleted: `logs_complete`, `segment_jobs`, `job_sections`, the exit-70 guard, `explained_exit`,
     `JOB_EXIT`, `phantom_timeouts`, D16's `hit_sources`, `verified_hit`, `phantom_verdict`,
     `legacy_unsaved_crash`, `scan_run`/`unsaved_logs`, and `run.sh`'s `segment-N/jobs` writer (the
     label needs no worker count). D15-D17 are superseded for the reason above.
  Tests (`InterruptedSlices`, `ClosePhantom`): the label rule; `test_nothing_in_the_logs_drops_a_timeout_as_a_finding`
  (a missing or emptied worker log, Sol r5 #1, Sol r6's forged line, huge and binary `jobs` files, and
  Sol r5 #2's merge logs: each stays a finding in `crash_files`, `run_findings`, `findings_of`,
  `Farm.accounted` and triage); the manual-close tests. Mutation-checked: dropping a labelled timeout
  in `crash_files`, and closing without a reason, each fail a test.

## Not covered

- Found by the r5 validator, outside this change, for their own rows: a fuzzing run with no verdict
  whose directory mtime is over an hour old (`stale_s`) is triaged while still running, and what it
  finds afterwards is never triaged (a long slice under load; the run id ends in run.sh's pid); and a
  crash or timeout whose dumped input text contains `sink write failed` or `divergence limit reached`
  reads as a sink failure through `signature` (a crafted seed).

- Certify's wall-cap path (SIGINT, then a SIGKILL after 1500 s, which is more than the 1200 s
  `-timeout`) can still end in an ALARM after a hung interrupt. Its verdict counts crash files and
  certify refuses on them, which fails safe, and a 12 h cap makes it rare. The grace is DEC-186's.
- The SIGKILL fallback kills `/usr/bin/time` with the worker, so the killed segment writes no `cpu`
  file and its CPU time is not in the ledger (the slice is still `interrupted`). It needs a worker
  that ignored the stop file and the SIGINT, which is the hang this fixes; the loss is at most one
  slice's CPU time, and the cap is still checked against what the ledger holds.
- The `-jobs` parent that `Farm` stops survives a group SIGINT while a worker runs (glibc's `system()`
  ignores it), so a hung worker's phantom is normally in the verdict. A phantom written after the
  verdict (the parent killed some other way) is in no verdict; triage reads the run's files and logs
  later and files it as a labelled finding (D18).
- A non-empty timeout artifact after an interrupt (an interrupt that arrived while a unit ran) is a
  finding; it replays clean, so a recheck closes it.

## Tests

`InterruptedSlices` and `RunShStopFile` in `node-fuzz/fuzzctl_test.py` (the latter runs run.sh's real
argument block and segment loop on a stand-in libFuzzer). The suspected-plateau test's fake slice now
reads the stop file, as libFuzzer does, and asserts that no signal was sent.

## DEC-243 evidence for tier 1 on 5a40d93ab (run `bl173-t1p`, GATE EXIT 1)

t1p (run dir `/work/scratch/gate-fast/runs/1017-183851-gate-5a40d93ab-iGr`, slot 3) failed only at the
`test` step: 15 failures, 13 dashd tests (the gate's own `RUST_TEST_THREADS=2` rerun passed all three
dashd suites) and 2 `dash-chain` tests, which the 900 s binary timeout killed
(`dash_chain-80a6660cf3f75186`, "test-run: TIMEOUT after 900s").

1. **Every failed step is `test`.** Yes (`steps.tsv`).
2. **No failing test is in a crate the diff touches or that depends on one.** The diff against
   a5d2a9c59 is `node-fuzz/{README.md,fuzzctl.py,fuzzctl_test.py,run.sh}` and this file. `node-fuzz`
   is its own `[workspace]` (`node-fuzz/Cargo.toml`), outside the root workspace. `cargo metadata` on
   the root workspace (51 members): `node-fuzz` is not a package in the resolve, and no package lists
   a dependency named `node-fuzz`. `cargo tree -i node-fuzz --workspace` fails with "package ID
   specification `node-fuzz` did not match any packages", for the same reason. The failing crates
   (`dash-chain`, `dash-spv`, `dash-spv-ffi`) are untouched.
3. **Each timed-out test passes alone from t1p's own binary.** The binary is t1p's
   `target/debug/deps/dash_chain-80a6660cf3f75186` (slot 3), copied to
   `/work/scratch/bl173-d243/dash_chain-t1p` (sha256 `543df3389b58e8d0135b...`) because slot 3 has
   since been reused. One test at a time, from `dash-chain/`, no timeout cap, load 127–162:

   ```
   dash_chain-t1p --exact chain::sb5::p2_tests::prop_windowed_equals_unwindowed --nocapture --test-threads=1
     ok, 1 passed, test 45.83 s, wall 46 s
   dash_chain-t1p --exact chain::scenario_tests::rule_rows::rules_cs3_header_rows_genesis --nocapture --test-threads=1
     ok, 1 passed, wall 58 s
   ```

   Both exit 0 (22:35:15Z–22:36:59Z, unit `bl173-d243-rerun`, script `run.sh` beside the binary). Each
   takes about a minute alone against the 900 s the loaded binary was given.
4. **The merge is the gated head or tree-equal to it.** The head that carries this section differs
   from 5a40d93ab only in this file. The train merge message must cite DEC-243.

The selftest is still required. `bl173-st9` (on 5a40d93ab) never ran: it waited 360 min for a lane-s2
disk reservation ("112 GiB reserved by other selftest steps, so 0 shard(s): no room") and exited
`FAILED-STEP: selftest`, `GATE EXIT 1`. That is a capacity timeout, not a result. It was requeued as
`bl173-st10` on the head that carries this section (f9a04e394).

`bl173-st10` (lane s, 4 shards, wall 26752 s): 325 ok, 0 FAIL, `all selftests passed`, `GATE EXIT 0`
(`/work/scratch/gate-fast/logs/bl173-st10.log`). Code on every later head is tree-equal to 5a40d93ab.
