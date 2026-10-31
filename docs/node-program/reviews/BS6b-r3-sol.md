# BS6b — Sol review, round 3

2026-10-11. Narrow review of **R1-B1 only**, under `briefs/REVIEW-COMMON.md`.
The code and seed findings closed in Sol r2 remain closed. PLAN §7 handling:
target names, counts, digests and run identifiers only.

**R1-B1 remains open. Nine targets satisfy the requested DEC-184 rule;
`diff_payload_1`, `diff_payload_3` and `diff_payload_9` need qualifying reruns.**
The receipt's 11/12 plateau claim uses different thresholds and a different clock.
Its zero-divergence and zero-crash claims are supported.

## Scope and governing rule

Reviewed the campaign receipt at rust-dashcore `55e8ba562` on
`campaign/bs6b-r1`, in `rdc-wt/bs6b-campaign`, plus its permitted private
run logs and farm ledgers. The reviewed implementation heads remain
rust-dashcore `cb8071cf9` and base-sdk `c156bc6`. The executables recorded by
the campaign are `acb06e88bee3` / `738c1c68e543`, using the v24 fuzz oracle
`a9a483e51efd`; their eligibility was established in r2 and is not reopened.

Applied DEC-184(1)–(2), at `DECISIONS.md:195`: at least one full **60-minute
window of the leg's own fuzzing time**, **zero additional cov edges**, and
`(ft_end - ft_start) / ft_start <= 0.001`, with zero divergences and crashes.
The window must be wholly within slices eligible under r2's corrected-seed
ruling. No shorter window was requested or recorded for this review.
The former 24 CPU-h per target / 288 CPU-h aggregate requirement is retired.

DEC-184(5)'s retrospective statement describes these campaigns as accepted
using “≤ 1 edge.” Per the explicit r3 brief, I checked the zero-edge rule in
DEC-184(1) itself; I have not treated that retrospective statement as a waiver.

## Per-target verdict

`cov/ft` below are the running maxima over the worker logs, not sums across
workers. Growth is over the **final 60 active fuzzing minutes**, with time
counted once for parallel workers and excluding other legs, build time and
gaps between slices. Every listed final window loaded the required seeds and
has **0 divergences / 0 crashes / 0 sink failures**.

| Target | Required seeds: count / digest | Eligible slices | Window-start cov/ft | Final peak cov/ft | Last 60 min Δcov / Δft | ft growth | DEC-184 verdict |
|---|---|---|---|---|---|---|---|
| diff_payload_1 | 153 / `6b0340c09493a062` | 3/3 | 1485/6146 | 1485/6156 | 0 / 10 | 0.1627% | **FAIL: ft growth** |
| diff_payload_2 | 225 / `3c884f1366c71016` | 2/2 | 2012/6857 | 2012/6863 | 0 / 6 | 0.0875% | PASS |
| diff_payload_3 | 153 / `5d6a757951b14307` | 2/2 | 507/1252 | 507/1255 | 0 / 3 | 0.2396% | **FAIL: ft growth** |
| diff_payload_4 | 225 / `693a7c3bdb614a53` | 2/2 | 1069/1876 | 1069/1876 | 0 / 0 | 0% | PASS |
| diff_payload_5 | 296 / `a7617124958e6ea8` | 2/2 | 1141/2000 | 1141/2000 | 0 / 0 | 0% | PASS |
| diff_payload_6 | 785 / `8bcbec990a5a2921` | 3/3 | 1448/3033 | 1448/3033 | 0 / 0 | 0% | PASS |
| diff_payload_7 | 75 / `852137c391675beb` | 2/2 | 1034/1772 | 1034/1772 | 0 / 0 | 0% | PASS |
| diff_payload_8 | 4 / `8d7d0695efddaf0f` | 2/2 | 311/1019 | 311/1019 | 0 / 0 | 0% | PASS |
| diff_payload_9 | 296 / `ccd22412ce843b74` | 1/2 | 1068/1874 | 1069/1875 | 1 / 1 | 0.0534% | **FAIL: cov growth** |
| diff_payload_10 | 2 / `faebf0c4d9ea9ffc` | 2/2 | 223/404 | 223/404 | 0 / 0 | 0% | PASS |
| diff_payload_11 | 2 / `4ddfe196d86fdde1` | 3/3 | 325/498 | 325/498 | 0 / 0 | 0% | PASS |
| diff_payload_12 | 51 / `bf4701b1b0f048c7` | 3/3 | 307/477 | 307/477 | 0 / 0 | 0% | PASS |

Types 7 and 10 pass because their observed final qualifying windows are flat.
Their receipt's “first-point plateau” labels do not establish the strict
plateau by themselves: the early points are respectively 1033/1770 and
222/403, below the eventual cov maxima. The inherited working corpora do not
invalidate the subsequent measured windows or require rerunning these legs.
Likewise, type 9's historical plateau location is not the decisive failure:
its new-seed slice gained a cov edge inside its final window.

## Findings

### R1-B1 — blocker, remains open: three legs fail the binding plateau rule

**Locations:** `node-fuzz/campaigns/bs6b-r1/receipt.py:71`, `:101`, `:127`,
`:138`, `:156`, `:259`; `README.md:20`, `:32`, `:34`, `:40` in the same
directory; `receipt.json:1590`, `:3125`, `:6537`.

**Scenario:** accepting the stated 11/12 result would close targets 1 and 3
despite their feature growth exceeding 0.1% in their final qualifying hour.
The generator allows `cov >= final - 1` and `ft >= 0.99 * final`, then adds
qualifying CPU-hours after that historical point. It never requires the
last-window `cov` gain to be zero or the last-window `ft` growth to be at
most 0.1%. Its `last_cpu_hour_gain` is informational and does not decide
`flagged`. An exact-cov companion point still uses the 1% feature threshold
and is not an acceptance check.

**Evidence:** independently replaying the original pure `segments()` and
`plateau()` functions reproduced all 12 committed plateau objects exactly.
Isolated controls with a one-edge gain or a 0.5% feature gain both return
`flagged: false`. The actual worker-log reconstruction gives the failures in
the table above. The following are concrete peak observations; log locations
are relative to the indicated target's `runs/` directory in the permitted sink:

| Target | Run | Evidence location | Observation |
|---|---|---|---|
| diff_payload_1 | `20261011T011021Z-v24.0-1401892-cf84afd985100b05` | `logs/segment-6/fuzz-1.log:133` | 1485/6156 reached about 5 minutes before the leg ended; the final-hour baseline is 1485/6146. |
| diff_payload_3 | `20261010T082919Z-v24.0-285638-eeb309d74b9e5381` | `logs/segment-6/fuzz-0.log:56` | 507/1255 reached about 19 minutes before the leg ended; the final-hour baseline is 507/1252. |
| diff_payload_9 | `20261010T195458Z-v24.0-2429046-647987fa5b9a5072` | `logs/segment-9/fuzz-1.log:20` | 1069/1875 reached about 8 minutes before the leg ended, adding one edge over 1068/1874. |

**Clock defect:** `receipt.py:87` uses `/usr/bin/time` user + system CPU
seconds, and `:116` accumulates them per leg. These are genuine per-leg CPU
measurements, not farm wall time, and are not doubled by `fuzz.out`. However,
one CPU-hour summed across two workers is not DEC-184's 60 minutes of that
leg actively running libFuzzer. Conversely, on this loaded host an active
hour can consume much less than one CPU-hour. Thus the README's “Last CPU-h”
column cannot answer the requested last-60-minute check. For example,
target 2 reports +29 ft over the receipt's sampled CPU-hour interval, but
its own last active hour gained +6 ft and passes the strict rule. Targets
8 and 11 also pass when their actual final active windows are examined.

**Fix / closure:** after FZ1 lands, rerun **only `diff_payload_1`,
`diff_payload_3` and `diff_payload_9`**, with the required seed counts and
digests above and the existing working corpora retained. Require a full
60-minute qualifying active-fuzzing window with Δcov = 0 and ft growth
≤ 0.1%, and zero divergences/crashes. Record the seed identity, clock,
window-start/end cov/ft and strict plateau verdict. Correct and regenerate
the receipt so its policy and flags use that rule, retaining the other nine
targets' evidence. No restored CPU-hour budget is required.

The generator should decide acceptance from a qualifying final window,
rather than require the earliest historical near-final point to lie after
seed replacement. A later full window with the corrected seeds can satisfy
the rule even when an earlier plateau was reached with old seeds.

### R1-B1 receipt accounting — minor: execution totals inherit BL-86

**Locations:** `node-fuzz/campaigns/bs6b-r1/receipt.py:235`, `:236`;
`node-fuzz/run.sh:511`; representative `receipt.json:3078`.

**Scenario:** the receipt copies `executions` from the farm ledger/verdicts,
whose runner sums `logs/*/fuzz*`. With `-jobs=2`, `fuzz.out` repeats the
workers' final execution statistics, overstating the reported exploration.

**Evidence:** across all 28 slices, worker logs contain **598,926,092**
executions; the parent's repeated statistics add **597,368,826**. The sum
is exactly the receipt's **1,196,294,918** `executions_all`. For target 3,
the correct worker-only count is **62,507,573**, while the receipt reports
**125,015,146**. The two interrupted runs have additional worker statistics
not repeated in the parent, explaining why the aggregate is slightly less
than an exact doubling.

**Fix:** recompute receipt execution totals from `fuzz-*.log` only, applying
the same seed-eligible slice filter for credited totals, and use FZ1's BL-86
runner fix for future verdicts. This accounting error does not double the
cov/ft maxima or CPU time and does not create a divergence/crash finding.

## Seed-loading evidence

The supplied seed evidence is sufficient for **27/28 slices** under r2's
ruling and the documented append-only public-seed directory lifecycle.
I independently recomputed all public seed-set counts and digests from Git
objects at `cb8071cf9` and `acb06e88`: all **24 sets match** the manifests
(2,267 required seeds versus 1,786 campaign seeds). The committed and
private `seed-sets.json` files match, as do the committed and private receipts.
I did not read or rehash private corpus/input files.

`receipt.py:176` chooses a timely count/digest snapshot, otherwise compares
the slice start with the recorded target-specific copy completion. The first
snapshot was actually taken at **2026-10-09 21:26:22Z**. Late backfilled
snapshots are correctly rejected by their lag, rather than retroactively
proving the earlier run's seed identity. All timely snapshots used here have
nonnegative lags of **0–5 seconds**, required counts and required digests.

For the earlier corrected-seed slices, the all-target copy-complete time is
**2026-10-09 19:37:42Z**. Types 7, 6, 5 and 4 start at **20:10:48Z,
20:13:18Z, 21:05:42Z and 21:07:45Z**, respectively, comfortably after that
time. `run.sh:397` adds the campaign checkout's seeds without removing the
corrected additions, and `run.sh:449` supplies that persistent public-seed
directory to libFuzzer. The unchanged sets for types 8, 10 and 11 require
no replacement-time cutoff.

The excluded type-9 slice `b6b4313807545edf` began at **18:36:13Z**.
Its later snapshot has the required 296-seed digest, but a **10,209-second
lag**; the receipt correctly falls back to the old **228-seed** set and
does not credit it. Its aggregate initial seed-corpus count of 299 would
not, by itself, prove that all 296 corrected public seeds loaded. The
qualifying slice `647987fa5b9a5072` starts at **2026-10-10 19:54:52Z**,
has a one-second-lag required-set snapshot, and both workers report 485
initial seed files.

I checked the first seed-corpus count in **both workers of every slice**,
not just the single first matching log returned by `receipt.py:64`.
The two workers agree for every slice and meet the selected set's count;
both reach initialized fuzzing statistics. The count is supporting evidence
of startup, while the snapshot/copy chronology supplies seed identity.
Timing credit depends on the recorded completion time and append-only
history; it must not be inferred from a later snapshot alone. Future runs
should snapshot the complete required set before worker startup.

## Independent verification and receipt assessment

Audited **28 slices, 374 segments and 748 worker logs**, with on-disk
`TMPDIR`, scratch directory mode 0700 and `nice -n 10`. No fuzzing or farm
unit was launched, stopped or changed.

The coverage parser correctly excludes `fuzz.out`: `receipt.py:91` reads
only `fuzz-*.log` and takes maxima rather than summing workers. Every worker's
cov/ft sequence is monotone in these logs; independent maxima over all status
lines match all 12 receipt final cov/ft values. First/last segment points are
accurate summaries, but cannot supply exact active-time window boundaries.

For the strict window, I reconstructed each leg's active trajectory using
the workers' `Done ... in ... second(s)` durations and status-line
execution counts / `exec/s`. The libFuzzer implementation computes the
printed rate as integer `runs / elapsed_seconds`, allowing bounded elapsed
times; interrupted workers also print final runs and average rate. Time
advances only for that leg's libFuzzer segments, with parallel workers
counted once. I also checked worker log end timestamps to account for
worker-start skew and subsecond timing uncertainty, measuring time from
the first status rather than building or waiting between segments. Both
reconstructions yield identical window-boundary cov/ft and all 12 verdicts.
Even the shortest qualifying leg exceeds two hours of observed active
fuzzing, so the full-window minimum is established. The final window of
type 9 is wholly within its one qualifying slice.

All 28 verdicts report **0 divergences, 0 crashes, 0 sink failures and
0 refusals**. Read-only artifact-directory counts independently give zero
divergence and crash files. There are 26 passing verdicts and two clean
manager-interrupted runs, recorded as interrupted in the farm ledger.
The measured aggregate CPU time is **27.3351 CPU-h**; it is informational.

The receipt retains `status: flagged` and flags type 9, so it does not claim
that the whole gate is closed. It nevertheless overstates the compliant
target count as **11/12 rather than 9/12**, mislabels its loose rule as
DEC-184, and overstates executions through BL-86. Its seed eligibility,
coverage maxima, measured CPU time and zero-finding counts are supported.
The retired budget fields, explicitly informational, are not an additional
acceptance failure.

Only this requested review file was written in the workspace; it is left
uncommitted. Reviewed branches were not changed, committed or pushed.
Private access was limited to the authorized fuzz run logs, verdicts,
farm ledgers and artifact counts. No input bytes or corpus files were
copied into the report or tracked files. No GitHub action was taken.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
