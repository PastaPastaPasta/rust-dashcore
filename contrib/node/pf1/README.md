# PF1 profiling scripts

Profiling pipeline for dashd v24.0.0-rc.3 (PF1, DEC-82, DEC-108). Report: `docs/node-program/research/PF1-dashd-v24-profile.md`, which has the exact commands in its last section.

| script | purpose |
|---|---|
| `build-dashd-symbols.sh` | docker build of the tag with `-g` and frame pointers |
| `walk-snapshots.sh` | one `-reindex-chainstate` testnet replay, reflink snapshots at each window start |
| `run_window.py` | one window under `perf record` (fp or DWARF call graphs) or `perf stat`; records `perf_throttle_events` |
| `run-matrix.sh` | the window × configuration matrix (resumable, retries) |
| `run_regtest.py` | startup, mempool, getblocktemplate, getblock, testnet-IBD and prep workloads |
| `perf_fold.py`, `refold.py` | fold `perf script` output into subsystem/anchor/inclusive tables and folded stacks |
| `parse_bench.py` | per-phase `-debug=bench` costs of a window |
| `report_tables.py`, `flamegraphs.sh` | summary.json / tables.md and the SVGs |
| `dwarf_callers.py` | inclusive caller shares from DWARF runs (report §2.2) |
| `metric5.py` | per-block connect latency percentiles, no perf (DEC-108) |

Gotchas: perf throttles above `kernel.perf_event_max_sample_rate` (use `--period 6000011` or raise the limit, and check `perf_throttle_events`); concurrent perf sessions exhaust `perf_event_mlock_kb` (run_window/run_regtest take `/work/scratch/pf1/perf.lock`); DWARF `perf script` needs `--time` to avoid empty stacks; the first v24 start of a 23.1.8 datadir runs a one-time MN-list diff repair; an unnamed `clone3` thread (LevelDB compaction after opening a cloned snapshot) adds 2.9–6.8 G instructions per window run and is split out as the `ex_bg` columns.
