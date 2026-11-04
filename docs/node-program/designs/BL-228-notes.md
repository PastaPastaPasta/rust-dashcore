# BL-228: farm segments stop on CPU time, not wall time

## Problem

`run.sh` gave each farm segment the CPU budget still missing, as libFuzzer's `-max_total_time`, which
libFuzzer counts in wall seconds. The farm runs at Nice 19 and CPUWeight 10-30 on a box at load
60-190, so a starved segment hit its wall clock after about one corpus reload (~400 CPU-s) and the
next segment reloaded the corpus again. On P2a-net round 1, 4-5 of 12 segments fuzzed nothing and a
1.5 CPU-h leg fuzzed about 2,300 of 5,400 CPU-s. CPU caps and DEC-184 plateau windows bought far less
fuzzing than they claimed. Certify's window already stopped on its own CPU time.

## Decisions

- **D1. A segment is stopped by its CPU time, through libFuzzer's stop file.** `run.sh` gives a
  budgeted segment `-stop_file=$seg/stop` and `-max_total_time=86400` (a day: a backstop that is
  never the normal stop) and runs it under `node-fuzz/window_driver.py --farm`. The driver measures
  the segment's process tree from /proc, the way certify measures its window: each live member's
  utime and stime, and its reaped children's (cutime and cstime), so the `-jobs` workers count. When
  the sum reaches the allotment it writes the stop file; every worker reads it at the top of its loop,
  prints its final stats and exits 0. No signal is involved, so BL-173's guard holds: no SIGINT, no
  hung interrupt path, no phantom 0-byte timeout.
- **D2. One driver, two modes.** The certify window's driver moved out of `run.sh`'s heredoc into
  `window_driver.py`, unchanged in certify mode (same arguments, `window.json`, grace, poll and
  selftest overrides; its tests now read the file). `--farm RUN_STOP` is the segment mode. The
  differences are the measure (the tree's CPU, not the process's own), the start (the segment's
  start, load included, not INITED: the farm's `--cpu-hours` budget is total CPU), the poll (2 s, not
  10 s: the overshoot is at most 2 CPU-s per worker), and the record (`segment.json`, same keys).
- **D3. The allotment is the CPU still missing, plus one second.** The segment loop sums the finished
  segments' `cpu` files (as before), and gives the next segment `ceil(missing * 3600) + 1` CPU
  seconds, over all its processes, not per worker. The driver stops at or past it and the final
  rusage is at least that, so the budget is met in one segment; the extra second keeps 10 ms tick
  rounding from starting a segment that reloads the corpus for a fraction of a second. A leg now
  loads its corpus once, and again only after a finding, a crash or a farm stop, which end a run
  anyway. `--cpu-hours`, the slice sizes and the DEC-184 plateau logic in `fuzzctl.py` are unchanged.
- **D4. The farm's stop file still works.** `Farm.request_stop` writes `$run/stop`. libFuzzer takes one
  `-stop_file`, so the driver copies a non-empty `$run/stop` to `$seg/stop` (within 1 s) and records
  `stopped_by: run-stop`. `run.sh` still starts no segment once `$run/stop` exists (BL-173 D2).
- **D5. The driver ignores SIGINT and SIGTERM in segment mode.** The farm's fallbacks signal the run's
  process group, which reaches libFuzzer and its workers directly. A driver killed by the same signal
  would take libFuzzer down with SIGKILL (its parent-death signal) before the final stats. The driver
  outlives the exit to write `cpu` and `segment.json`; the SIGKILL fallback still kills the group.
- **D5a. Workers and the leg lock (Opus review r1).** `libFuzzer` starts `-jobs` workers with `system()`,
  so they have no parent-death signal: the grace SIGKILL walks the process tree and kills every member,
  and the segment's libFuzzer keeps run.sh's leg lock (fd 8) open as it did under `/usr/bin/time`, so a
  certify run cannot take the exclusive lock while a worker lives. The driver installs its SIGINT and
  SIGTERM handlers before it starts libFuzzer. The `NODE_FUZZ_CERTIFY_POLL` and `_GRACE` overrides
  apply to segments too (poll: any value up to 10 s, default 2 s).
- **D6. Unbudgeted modes are unchanged.** `--runs N` (the selftest), `--merge`, `--job` and certify's
  replays keep `/usr/bin/time` and their own stop rules; only the budgeted run-mode segment loop uses
  the driver.

## Tests

`fuzzctl_test.py`: `FarmSegmentDriver` (the tree's CPU ends a segment, children included; the
allotment counts the load; the run's stop file is copied and an empty one is not; a signal to the
driver alone leaves the segment running; the wall backstop interrupts), `RunShCpuSegments` (the
segment loop on a stand-in libFuzzer that keeps `-max_total_time` as wall seconds and burns CPU for a
fraction of its wall time, and the same pinned to one CPU with two busy hogs at a higher nice value),
`RunShStopFile` (updated: the segment's `-stop_file` is its own and the run's stop reaches it).
`test_the_wall_clock_stop_fails_the_scenario` is the mutant: the pre-BL-228 stop (`-max_total_time` =
the missing CPU seconds, no driver) breaks the scenario (a second segment, CPU short of the budget).
The same two scenario tests also fail against `a47bf2f61`'s `run.sh`.

## Measurement

`kv_model` (0.107 CPU-s per input, close to `diff_p2p_addr`'s 0.1), a working corpus of 1,733 inputs
(a reload of about 185 CPU-s), `--cpu-hours 0.25` (900 CPU-s), both legs at Nice 15 and CPUWeight 10 on a
box at load 130-150, started together on copies of one private tree. Before is `run.sh` at
`a47bf2f61`, after is this branch. Fuzzing CPU is the segment's total minus its reload, with the reload
priced at segment-1 of the before leg (which ended right after its load: 185.2 CPU-s for 1,733 inputs).

| | segments | total CPU-s | reload CPU-s | fuzzing CPU-s | fuzzing share | inputs fuzzed | per total CPU-s |
|---|---|---|---|---|---|---|---|
| before (wall-clock stop) | 3 | 1047.7 | 561 | 487 | 46 % | 1,952 | 1.86 |
| after (CPU stop) | 1 | 903.3 | 185 | 718 | 80 % | 4,206 | 4.66 |

The before leg's segment-1 ended after its load with no fuzzing (1,733 inputs executed, none mutated)
and its wall-clock segments overshot the cap by 16 % (0.291 CPU-h). The after leg loaded once and
stopped 0.3 % past the cap. Inputs fuzzed per CPU-second of the cap rose 2.5 times. The reload price
is the same for both legs, so the shares are estimates; the segment and total figures are measured.
Walls: 2,381 s before, 2,243 s after.
