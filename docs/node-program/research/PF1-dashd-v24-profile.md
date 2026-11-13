# PF1 — where dashd v24.0.0-rc.3 spends CPU and memory

Status: **§1–§10 are measurements** on agentbox, 2026-10-09; **§11 is projection only** (ceilings and ranges with stated assumptions, revised after round-1 review by Sol). Task PF1 of the full-node program, plus DEC-108's metric #5 baseline (§8).
Profiled: **dashd v24.0.0-rc.3 only** (DEC-82; 23.1.8 skipped). Scripts: `contrib/node/pf1/`. Data and flamegraphs: `research/PF1/`.

Everything below counts **user-space instructions retired (`instructions:u`)** first, then on-CPU time (`cpu-clock`). Instruction counts are repeatable on this VM to ≈ 0.1 % for deterministic microbenchmarks (memory note `agentbox-perf-counters`) but only to ≈ 1.7 % for whole-process reruns (§1.2); wall time is not (agentbox load average was 30–185 during these runs). Wall-clock numbers are therefore secondary and labelled as such.

## 0. Read this first: what the data can and cannot say

| Limit | Consequence |
|---|---|
| **No mainnet data.** `/work/chains` and `/work/bench` are off-limits to this task. | Every block workload is testnet. Testnet blocks hold **1.6–1.9 transactions** on average (against ≈ 40–100 on mainnet today). Block connect on testnet is therefore a profile of the *Dash-specific* work (BLS, quorums, MN lists, chainlocks), not of transaction validation. |
| Transaction-heavy connect is **synthetic**: regtest blocks from the B1 fixture (≈ 8,880 one-input two-output P2PKH txs per 2 MB block). | Gives per-tx and per-txin costs and a script-verification ceiling. It is not a mainnet mix (no multisig, no large scripts, no special txs). |
| The windows replay with `-txindex=0 -disablegovernance`. | `-reindex-chainstate` refuses `-txindex`, and dashd refuses `-txindex=0` with governance on. The txindex and governance work of a default node is not in the window profiles. (The startup, getblock and IBD runs use the default config with both on.) |
| IBD is measured only for **blocks 0–100,000 from one local seed** over loopback, and the node also syncs the seed's **whole header chain (≈ 1.57 M headers)**. | It shows a header/PoW-dominated sync. Per-block IBD figures are meaningless (most of the X11 work is the 1.57 M headers, not the 100 k blocks). It does not show peer scheduling, download stalls or large-block behaviour. |
| Frame-pointer call graphs lose the callers of GMP/relic assembly leaves. | The BLS *subsystem* share (leaf-owner view) is reliable. Attribution of that cost to *callers* is not, so §2.2 uses DWARF-unwound runs for it. |
| Wall-time and latency numbers were taken on a loaded, shared VM. The `LevelDB` share of the assumevalid windows is **largely an unnamed background thread** (§2.1). | Use instructions for comparisons. Latency percentiles (§8) carry the load average of every run. |

## 1. Provenance

| Item | Value |
|---|---|
| Source | Dash Core tag `v24.0.0-rc.3`, commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18` |
| Profiled binary | `bin/dashd-v24.0.0-rc.3-sym`, **sha256 `4ad82cf6d9e4ecd50884cbb33a1e1d0fac13c552f849497840e88cbc108068d8`** |
| Build | `contrib/node/pf1/build-dashd-symbols.sh`: docker `debian:bookworm` (`debian@sha256:2c037a04925515fdd6ea85ea14a682d0e79931f5e9f5d07b6dbfc6ba12f9e858`), gcc 12.2.0, depends rebuilt; `CFLAGS/CXXFLAGS = -O2 -g -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer`; `configure --enable-debug=no --enable-reduce-exports --disable-bench --disable-tests`. Container capped with `--cpus=8` (docker is not under a `systemd-run --user` scope). |
| Official release binary (seeds, prep, IBD server) | `dashcore-24.0.0-rc.3` tarball, `dashd` sha256 `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`; tarball sha256 `9327f00018afd12f11676ac66e688c76b253e4783bf0d595f08476de4269ff52` (B1 pin) |
| Core baselines | `bench_dash` built from the same tag by B1's `build-dashd-bench.sh` (`runs/microbench`, numbers in §9) |
| Flamegraphs | Brendan Gregg's FlameGraph, commit `41fee1f99f9276008b7cd112fca19dc3ea84ac32` |
| Host | agentbox: 2 × Xeon E5-2687W v4 (32 vCPU, no SHA-NI), kernel 7.0.0-38, perf as shipped with Ubuntu 26.04, `perf_event_paranoid=2` |
| perf sysctls | `perf_event_max_sample_rate` raised to 25,000 and `perf_cpu_time_max_percent=0` for the runs (restored at the end; see §1.2) |
| Regtest workloads | pinned to CPUs 28–31 (the testnet windows to CPUs 16–27) |
| Scratch layout | `/work/scratch/pf1/` (private `NODE_BENCH_ROOT`, datadirs, runs), deleted when this task closed. **Not archived:** the full-symbol folded stacks and TSVs of the general runs, the window `debug.log` files and `perf.data`. `report_tables.py` and `parse_bench.py` therefore need a fresh run directory to regenerate the subsystem tables and per-block phase distributions; committed instead are the derived `summary.json`, `bench-*.json` (phase percentiles and maxima), the DWARF inclusive TSVs, the flamegraphs, and the full metric #5 samples. Exact counts taken from the unarchived logs (blocks ≥ 1.6 s, §4's full-block-only statistics) are marked as such where they appear. |

### 1.1 Method

* **Counters.** `perf record -k monotonic -g -e instructions/period=6000011/u -e cpu-clock/freq=997/`. `-g` is frame-pointer unwinding. A second event (`cpu-clock`) gives on-CPU time shares. For DWARF spot checks: `--call-graph dwarf,16384` with `period=40000003`.
* **Window.** A run is a fixed range of blocks. Only samples between the `UpdateTip` log lines of the first and last block are folded, so startup, shutdown and the first block's cold effects are excluded. A window `W` with `N = 2000` therefore times blocks `W+2 .. W+N` (1,999 blocks).
* **Snapshots.** `walk-snapshots.sh` replays testnet once with `-reindex-chainstate`, stopping at each window start and keeping a reflink copy of the datadir. A window run clones the snapshot, starts dashd with `-connect=0 -stopatheight=W+N`, and measures `W+1..W+N`. Chainstate at a height does not depend on `-par` or `-assumevalid`, so one walk serves every configuration.
* **Configurations** (`run-matrix.sh`), 2,000 blocks each, CPUs 16–27:

  | name | flags | meaning |
  |---|---|---|
  | `serial-noav` | `-par=1 -parbls=1 -assumevalid=0` | every script and BLS check, one thread: the instruction profile |
  | `serial-av` | `-par=1 -parbls=1` | default assumevalid (testnet's is at height 1,400,000): what IBD pays below it |
  | `par8-noav` | `-par=8 -parbls=8 -assumevalid=0` | parallel checks: serial fraction |
  | `*-nobench` | as above without `-debug=bench` | headline counters (measured per block, ex background thread, the bench log costs 0.3–1.6 % of instructions in the no-assumevalid windows, 0.3–3.0 % in the assumevalid windows and 16 % in the cheapest one, 421,000 with assumevalid; the percent-level figures are at or below the ≈ 1.7 % run-to-run noise of §1.2) |

* **Subsystems.** Each sample goes to the **leaf-most** frame that matches a subsystem rule, with libc and allocator frames transparent (`perf_fold.py`, table `SUBSYSTEMS`). Inclusive tables (`INCLUSIVE`) and anchor tables (`ANCHORS`) are produced from the same samples.
* **Bench log.** `-debug=bench` lines are parsed per block (`parse_bench.py`). Dashd logs, per block and in this order: `Load block from disk`, the connect phases, `Connect total` (= `ConnectBlock` only), `Flush` (coins-view and EvoDB write-back), `Writing chainstate` (the `IF_NEEDED` disk flush), `UpdateTip`, `Connect postprocess`, and last the top-level `Connect block` (the whole `ConnectTip`). A block is attributed by closing it at the top-level `Connect block` line. Phase costs below are ms per block over the window; percentiles are per occurrence (`Loop` runs twice per block).
* **Unnamed background thread.** Every window run carries **2.9–6.8 G instructions** (mostly 5.4–6.8 G; 2.9–5.0 G at 421,000) on a thread named `dashd` whose root is `clone3`. The committed stacks identify it as LevelDB's `PosixEnv::BackgroundThreadEntryPoint → DBImpl::BackgroundCall → BackgroundCompaction`. It is classified separately (`perf_fold.py`, `BG_GROUP`) and **excluded from the `ex_bg` columns**. In the assumevalid windows it is 14–37 % of everything and almost all of the `LevelDB` share. **What caused its work is not established**: all windows have the same length (1,999 timed blocks), and there is no idle-only control and no window-length sweep, so "compaction after reopening the cloned snapshot" and "compaction induced by the replay's own writes" are both open hypotheses. `ex_bg` is therefore a foreground/other-thread breakdown, not a causally isolated total cost of block processing.
* **Throttle check.** Every run records `perf_throttle_events`; **all runs reported here have 0**. An earlier pass at `period=1000003` produced throttled (undercounted) perf data and was discarded; see §1.2.

### 1.2 Things that went wrong, and what they cost

1. **perf throttling** invalidated the first matrix: at `period=1000003` the instruction sampling rate exceeds the kernel's `perf_event_max_sample_rate` (2,000 Hz default; the kernel also lowers it by itself under load), perf throttles, and counts come out low and different between runs (a 2.5× spread between bench-on and bench-off runs of the same window). Fix: `period=6000011`, `perf_event_max_sample_rate=25000`, `perf_cpu_time_max_percent=0`, and a THROTTLE count recorded in every `run.json`. Three A/B `perf stat` runs of the same 301-block replay (whole process, `runs/ab-*`) gave 47.06, 47.53 and 47.86 G instructions: ≈ 1.7 % whole-process run-to-run noise, and no bench-flag effect distinguishable from it. (The earlier "±0.04 %" is for deterministic microbenchmarks, not whole-process runs.) The per-window bench cost in §1.1 comes from the `bench` vs `nobench` window pairs. The sysctls were restored afterwards.
2. **v24's one-time MN-list diff repair.** The first v24 start on a datadir made by 23.1.8 runs `RecalculateAndRepairDiffs` ("Successfully repaired 0 masternode list diffs, verified 2710 snapshots in 249s"). RPC answers while it still runs in the background. My first startup and RPC profiles were contaminated by it; they were redone on a repaired copy with an idle-wait before measuring. Anything that upgrades a 23.1.8 datadir pays this once.
3. A throttled/failed run is deleted and redone by `run-matrix.sh` (retry loop; resume skips only runs without `error`).

## 2. Block connect on testnet: the Dash-specific work dominates

Per-window instruction cost, `-nobench` runs (full table: `research/PF1/data/tables.md`, `summary.json`; its "inclusive" and "anchor" tables are regex-over-signature artifacts and are not used for conclusions here).

| window | config | tx/blk | txin/blk | **instr/block** (all threads) | **instr/block, ex background thread** | CPU ms/blk | wall ms/blk | parallelism |
|---|---|---|---|---|---|---|---|---|
| 421,000 | serial, assumevalid | 1.62 | 2.96 | 4.1 M | **2.7 M** | 1.07 | 1.30 | 0.82 |
| 421,000 | serial, no AV | | | 38.8 M | **36.3 M** | 5.82 | 5.44 | 1.07 |
| 925,000 | serial, AV | 1.93 | 1.0 | 20.4 M | **17.1 M** | 4.53 | 4.62 | 0.98 |
| 925,000 | serial, no AV | | | 178.8 M | **175.6 M** | 29.3 | 29.7 | 0.99 |
| 1,140,000 | serial, AV | 1.69 | | 21.6 M | **18.3 M** | 4.31 | 3.91 | 1.10 |
| 1,140,000 | serial, no AV | | | 176.2 M | **172.9 M** | 28.8 | 30.2 | 0.96 |
| 1,340,000 | serial, AV | 1.80 | | 19.1 M | **16.3 M** | 4.92 | 5.05 | 0.97 |
| 1,340,000 | serial, no AV | | | 159.6 M | **156.7 M** | 28.2 | 29.5 | 0.96 |
| 1,520,000 | serial (above AV: all checks run) | 1.83 | 1.19 | 176.4 M | **173.1 M** | 30.9 | 32.9 | 0.94 |

The two instruction columns differ by the background thread's instructions divided by 1,999 blocks (≈ 1.4–3.4 M/block). **The ex-background column is the cost on the connecting and other threads**; the all-threads column is what a `perf stat` of the whole process shows and includes the asynchronous storage work that a replay of these windows pays, whatever its cause. A rewrite has to pay whichever part of that work its own writes induce.

* With assumevalid the 2,000-block windows cost 2.7–18 M instructions per block (ex background thread); **without it (or above the assumevalid block) they cost 36–176 M**, and **84–97 % of that is BLS** (subsystem table below). The 10× jump between `serial-av` and `serial-noav` at 925,000 (17.1 M → 175.6 M) is almost entirely BLS verification that assumevalid skips.
* Per-tx figures are meaningless here (1.6–1.9 tx per block). Use §4 for per-tx and per-txin costs.

### 2.1 Subsystem shares (instructions:u, leaf-owner view, `-nobench` runs, **excluding the unnamed background thread**; the 1,520k column is the `serial-noav` run, the `serial-av` run is within 0.15 pp)

| subsystem | 421k AV | 421k noAV | 925k AV | 925k noAV | 1,520k (checks on) |
|---|---|---|---|---|---|
| BLS (relic + GMP leaves) | 36.7 % | 85.0 % | 85.7 % | 96.7 % | 96.7 % |
| X11 | 25.7 % | 1.8 % | 4.9 % | 0.5 % | 0.5 % |
| EvoDB / MN lists | 18.9 % | 1.4 % | 4.8 % | 0.5 % | 0.4 % |
| SHA-256 | 6.9 % | 5.4 % | 2.3 % | 0.6 % | 0.6 % |
| script verification (secp256k1) | | 4.0 % | | 0.3 % | 0.3 % |
| LevelDB (main threads) | 1.2 % | 0.9 % | 0.4 % | 0.7 % | 0.7 % |
| validation, coins cache, block I/O, tx/block codec | 8.9 % together | < 1 % | ≈ 1.5 % | < 1 % | < 1 % |

**The unnamed background thread** (2.9–6.8 G instructions per run; its dependence on window length was not tested) is what the all-threads shares call "LevelDB", a cost that is real but sits on another thread, not on the synchronous connect path: it is 36.0 / 7.2 / 16.2 / 2.5 / 2.5 % of the columns above (LevelDB share with it included), and 36 % of the 421k-AV run (35 % in the `nobench` run). **Direct LevelDB work on the block-connect threads is below 1.5 % in every window** (1.24 % at 421k AV without the bench log, 1.26 % with it; the table rounds to 1.2 %). The 421k-AV window's 4.1 M all-threads instructions per block is 2.7 M of block connect plus 1.4 M of this thread (2.9 G / 1,999 blocks in the `nobench` run). The same thread may also overlap the metric-#5 AV runs (§8), where it competes for CPU on a cloned datadir.

The top leaf symbols of every BLS-bound window are the same: `__gmpn_mul_basecase` + `__gmpn_addmul_1` are **45–51 %** of the main-thread instructions in the checked windows (22 % at 421k AV, 45–47 % in the other AV windows), then `dv_cmp`, `fp_rdcn_low`, `__gmpn_add_n`, `core_get` (relic's field arithmetic over GMP). Core's BLS library is relic with GMP assembly; **one BLS verify costs 39.8 M instructions** (`bench_dash` `BLS_Verify_Normal`), **an ECDSA verify 379 K** (a 105× gap).

### 2.2 Who calls the BLS work (DWARF-unwound runs)

Frame pointers lose the callers of the GMP leaves, so the `-g` inclusive table under-reports (it shows `ConnectBlock` at 2.7 % when BLS is ≈ 95 % of the all-threads instructions). DWARF runs (`dwarf-*`, 299–599 blocks each, `period=40000003`) recover them. Share of **all** instructions in the sampled window, inclusive (a function and its callees; rows overlap). Regenerate with `contrib/node/pf1/dwarf_callers.py`; the row regexes are in that file.

| inclusive | 1,520k, checks on (92 G; 2.3 % of samples without a stack) | 925k noAV (63 G; 5.7 %) | 421k noAV (25.8 G; 9.6 %) | 925k AV (10.4 G; **28 %**, 261 samples) |
|---|---|---|---|---|
| `ProcessSpecialTxsInBlock` | **32.9 %** | 30.6 % | 31.8 % | 22.2 % |
| `CQuorumBlockProcessor::ProcessBlock` (commitments) | **20.0 %** | 17.7 % | 25.2 % | 7.7 % |
| BLS `Verify*` / `AggregateVerify*` | **25.0 %** | 22.7 % | 25.3 % | (none: skipped) |
| BLS check-queue work (`CCheckQueue<BlsCheck>`) | 18.7 % | 16.8 % | 24.5 % | |
| **G2** (signature) `FromBytes` / `IsValid` | 8.8 % | 7.9 % | 5.1 % | **20.3 %** |
| G1 (public key) `FromBytes` / `IsValid` | 11.0 % | 10.3 % | 5.0 % | 2.7 % |
| **G1 + G2 decode and subgroup check (union)** | 19.8 % | 18.2 % | 10.1 % | 23.0 % |
| chainlock (`CheckCbTxBestChainlock`, `GetNonNullCoinbaseChainlock`, `VerifyChainLock`) | 6.0 % | 6.0 % | | 0.4 % |
| credit pool (`GetCreditDataFromBlock`) | 0.6 % | 0.4 % | | 6.9 % |
| `RebuildListFromBlock` | 0.5 % | 0.6 % | 0.8 % | 5.7 % |
| `CheckMerkleRoot` | 0.8 % | 0.8 % | | 5.0 % |
| `VerifyScript` | 0.7 % | | 6.4 % | |

The 925k-AV column rests on 261 samples and 28 % of them have no stack; read it as indicative only.

What it says:

1. **Even with assumevalid (no verification), G2 signature decoding is the largest single identified item in the AV window (≈ 20 % of its instructions, 23 % with G1 decode)**, reached from `GetCreditDataFromBlock`, `RebuildListFromBlock`, the coinbase-payload merkle check and the commitment path. I did not count how many times each signature is decoded per block, so "decoded repeatedly" is a hypothesis from the caller list, not a measurement. Whether the subgroup check may be skipped for chain data that was already validated is a design question for the BLS wrapper (memory `dash-bls-wrapper-decode-domain`).
2. **In the checked windows BLS verification is 23–25 % (inclusive) and the decode/check work is 10–20 % (G1+G2 union; the G2 part alone is 5–9 %).** The decode work is partly inside the verify path (so the two rows overlap), which is why the rows are not additive.
3. These DWARF windows are short (299–599 blocks, different block sets) and cost **11–31 % more per block** (230.6, 210.6, 43.0 and 26.2 M against 176.4, 178.8, 38.8 and 20.4 M all-threads for the 2,000-block windows). Do **not** apply their shares to the 2,000-block windows' absolute costs.

### 2.3 Where the wall time goes (bench log, window 1,520,000, `serial-av`, ms per block)

| phase | mean | p50 | p95 | max |
|---|---|---|---|---|
| `Connect block` (whole `ConnectTip`) | 33.1 | 13.1 | 52.3 | **3,353** |
| `Connect total` (`ConnectBlock` only; logged before the coins-view flush) | 32.8 | 12.7 | 51.2 | 3,352 |
| `ProcessSpecialTxsInBlock` | 31.3 | 11.3 | 46.1 | 3,350 |
| `m_qblockman.ProcessBlock` (commitments) | 17.9 | **0.04** | 19.3 | **3,031** |
| `CheckCbTxBestChainlock` | 7.0 | 7.15 | 9.7 | 56 |
| `GetQcHashes` | 2.2 | 0.69 | 2.4 | 281 |
| `m_dmnman.ProcessBlock` (MN list) | 0.95 | 0.36 | 4.1 | 73 |
| `Verify N txins` (log label; covers the whole connect-transactions loop) | 31.6 | 11.4 | 49.6 | 3,351 |
| `Loop` (per-tx special-tx loop; 2 occurrences per block) | 2.0 | 0.49 | 3.2 | 102 |
| `Load block from disk` | 0.175 | 0.14 | 0.23 | 13.7 |
| `Flush` (coins view + EvoDB write-back) / `Writing chainstate` (disk flush) / `Connect postprocess` | 0.09 / 0.02 / 0.06 | | | 2.3 / 0.1 / 2.2 |
| `Index writing`, `Write undo data`, `CheckCreditPoolDiffForBlock`, `IS filter` | < 0.05 | | | |

Block connect at this depth is *entirely* special-tx work: a median block spends 11.3 of 13.1 ms in `ProcessSpecialTxsInBlock`, of which `CheckCbTxBestChainlock` alone is a p50 of 7.15 ms (the coinbase chainlock verify, one BLS verify per block). **The mean is carried by the blocks that mine quorum commitments**: `m_qblockman.ProcessBlock` has p50 0.04 ms, p95 19 ms, and **7 blocks of the 2,000 take ≥ 1.6 s** (1.7–3.0 s; counted from the window's `debug.log`, which is not archived: the committed evidence is the phase percentiles and maxima in `PF1/data/bench-*.json`). Those blocks set the window's max, and are the likely source of the tail in §8.

Other windows (same method, per block): 925,000 `serial-av` 3.9 ms mean, 2.0 p50, 10.6 p95, **172 max**; 925,000 `serial-noav` 25.5 / 8.1 / 22.2 / 3,055 (3 blocks ≥ 1.6 s, same unarchived-log basis); 421,000 `serial-noav` 6.7 / 1.2 / 17.3 / 338.

## 3. Serial fraction: what `-par=8 -parbls=8` buys

| window (noAV) | serial wall ms/blk | par8 wall ms/blk | wall speed-up | CPU parallelism (par8) | share of CPU on check workers | main-thread CPU ms/blk (par8) |
|---|---|---|---|---|---|---|
| 421,000 | 6.93 | 5.64 | 1.23× | 1.2 | 58.3 % | 2.2 |
| 925,000 | 25.9 | 20.7 | 1.25× | 1.40 | 47.8 % | 14.3 |
| 1,140,000 | 29.1 | 23.4 | 1.24× | 1.29 | 46.0 % | 15.3 |
| 1,340,000 | 26.7 | 23.6 | 1.13× | 1.16 | 36.1 % | 16.7 |
| 1,520,000 | 33.4 | 25.6 | 1.30× | 1.30 | 46.2 % | 16.8 |

Parallel checks move roughly 36–58 % of the CPU time off the connecting thread, but wall time improves only 1.13–1.30× (each ratio is one serial run against one par8 run on a loaded VM, not a controlled estimate). **The connecting thread is also the check-queue master and executes checks itself while it waits**, so its 14–17 ms/block is an *upper bound* on the serial part. Amdahl's ceiling for N → ∞ workers is 1/s, with s the serial fraction of the single-thread time; the main-thread CPU share only bounds s from above, so 1/(main-thread CPU share) (2.0, 2.0, 1.6, 2.0× at 925k, 1,140k, 1,340k, 1,520k) is a **lower bound on the idealised ceiling, not a minimum achievable gain**. How much of that thread's time is pure serial work (G1/G2 decode, aggregation set-up, MN-list and quorum bookkeeping) versus stolen checks is not separated by this data.

Commitment-phase observations (maxima of two different executions, so an observation per run pair, **not an estimate of the effect and not a p99 ratio**): `m_qblockman.ProcessBlock` max 3,251 ms (serial) vs 1,091 ms (par8) at 1,520k (2.98×) and 2,834 vs 856 ms at 925k (3.31×); the whole `Connect block` max at 1,520k is 3,631 vs 1,317 ms (2.76×), and at 421,000 it is **worse** with par8 (337.85 → 389.51 ms).

## 4. Transaction-heavy connect (regtest, synthetic full blocks)

`connect-big`: four blocks (8,882 / 8,880 / 8,880 / 3,362 txs; three ≈ 2 MB and one 0.76 MB, one P2PKH input and two outputs per tx), `-reindex-chainstate`, `-assumevalid=0 -par=1`, 30,004 txs and **30,000 txins**.

| metric | value |
|---|---|
| instructions | 15.47 G over 4 blocks = 3.87 G/block average; **515 K per tx** (a full 8,880-tx block ≈ 4.6 G) |
| script verification share | **85.0 %**: `secp256k1_fe_mul_inner` 41.3 %, `fe_sqr_inner` 28.9 %, `gej_double` 3.7 %, `gej_add_ge_var` 2.6 % |
| SHA-256 | 8.6 % (txid, sighash) |
| coins cache | 2.7 % instructions, **10.5 % of CPU time** (the gap is consistent with cache misses; cache misses were not measured) |
| CPU time | 2.90 s for 30,000 txins = **96.5 µs/txin, 10.4 k sigops/s on one core** |
| wall (loaded VM) | 4.47 s = 6.7 k txin/s |
| peak RSS | 87.7 MiB |
| `-debug=bench` | `Connect total` mean 1,005 ms over the four blocks (the 3,362-tx block included), **p50 1,203 ms, p95 1,251 ms** for the full ones; `Verify txins` ≈ 97 % of it; `Load block from disk` 78 ms and `Flush` 36 ms per full block |

Per tx: **438 K instructions in script verification, against 379 K for the ECDSA verify alone in `bench_dash`** (the remainder is not broken down here). There is no cheap instruction win left in script checking itself; the wall-time lever is parallelism (`-par`), not run here for this workload (the testnet windows have too few txs to show it).

## 5. Mempool acceptance (regtest, `sendrawtransaction`, 5,000 fixture txs, 1 input / 2 outputs)

| | 1 client | 8 clients |
|---|---|---|
| instructions | 4.25 G = **850.8 K/tx** | 4.13 G = 825.6 K/tx |
| CPU time | 2.06 s = 0.41 ms/tx | 1.75 s = 0.35 ms/tx |
| loop wall (loaded VM) | 7.16 s = 698 tx/s | 3.09 s = 1,616 tx/s |
| peak RSS | 97.5 MiB | 97.3 MiB |

| subsystem | instructions | CPU time |
|---|---|---|
| script verification | **47.7 %** (`fe_mul_inner` 22.0 %, `fe_sqr_inner` 15.8 %) | 22.5 % |
| RPC / HTTP / JSON (the benchmark's own transport) | 14.2 % | **35.0 %** |
| SHA-256 | 11.4 % | |
| coins cache | 9.6 % (**6.2 pp is the inclusive `CSHA512` family** (leaf `sha512::Transform` 5.2 %): salted-hasher RNG seeding in **two** per-accept view constructors, 3.53 pp under `CCoinsViewCache::CCoinsViewCache` and 2.68 pp under `CCoinsViewMemPool::CCoinsViewMemPool`, both built by each `MemPoolAccept`) | 5.7 % |
| LevelDB | 5.9 % | |
| tx codec, validation, mempool bookkeeping | 3.4 / 2.7 / 2.7 % | 3.2 / 6.5 / 10.0 % |

ECDSA verification alone is 379 K = **44.6 % of a transaction's cost**. Subtracting the RPC transport (not present for P2P-origin txs) leaves ≈ 730 K, of which the verify is 52 %. **Instruction floor for beating dashd: ≈ 2× at best** unless signature checking itself gets cheaper; see §11 row 5.

## 6. `getblocktemplate` (regtest, mempool pre-filled with 5,000 / 20,000 txs, one new tx before each call)

Two unmeasured warm-up calls, then six measured calls, each after a 5.2 s wait (dashd answers from its cache otherwise).

| mempool | instr/call | CPU/call | wall/call (loaded VM) | peak RSS |
|---|---|---|---|---|
| 5,000 | **303 M** (60.5 K per template tx; 5,007 in the template) | 99 ms | 146–305 ms | 211 MiB |
| 20,000 | **596 M** (67.1 K per template tx: **8,880 txs in the template**, the rest of the 20,000-tx pool did not fit the block) | 262 ms | 279–540 ms | 246 MiB |

Subsystems (5,000): RPC/HTTP/JSON **65.0 %** (`json_escape` **23.8 %**, `memmove`, `std::vector<std::byte>`), LevelDB 8.9 %, tx/block codec 8.6 %, coins cache 7.3 %, mempool 4.3 %. 20,000: RPC/JSON 58.9 %, `SipHashUint256Extra` 4.7 %, LevelDB 9.4 %, coins cache 9.1 %.

**About 60–65 % of the instructions of a template call are RPC/JSON work (hex-encoding and escaping transactions).** Assembly proper (mempool, coins) is 11–13 % of instructions. `CreateNewBlock` is 28.7 % / 35.2 % inclusive, of which `TestBlockValidity` is 23.8 % / 28.5 %. Each figure is from only 6 measured calls (303 and 596 samples at the 6 M period), so shares below ≈ 2 % are noise. Block assembly is also redone from scratch on every call that follows a mempool change.

## 7. Startup, and RPC `getblock`

### 7.1 Startup

| | testnet, 1.57 M headers, txindex + governance on | regtest fixture (177 blocks) |
|---|---|---|
| time to RPC ready | **19.4 s** | 0.59 s |
| instructions | **35.2 G** | 1.39 G |
| CPU time | 18.4 s (89.5 % on the init thread) | 0.32 s |
| peak RSS | **1,051 MiB** | 88.4 MiB |

Testnet startup is **81.5 % "validation/index work"** (a leaf-owner bucket: it holds chain-work computation, work-ordered candidate-set insertion, index construction, height sorting, skip links and allocation, **not chain work alone**). In the committed flamegraph `GetBlockProof` is **31.3 % inclusive** (11.0 G of 35.2 G instructions); the round-1 review read the work comparator at ≈ 21.9 % and the union at ≈ 53.2 %. The leaf symbols under `LoadBlockIndex`: `base_uint<256>::operator>>=` 20.5 %, `CompareTo` 12.1 %, `operator/=` 7.7 %, `CBlockIndexWorkComparator` 5.7 %, `std::_Rb_tree<CBlockIndex*>` 4.4 % (together **50.4 %**), plus `CBlockTreeDB::LoadBlockIndexGuts` 9.9 %: ≈ **22.4 K instructions per header in total (1.568 M headers, 35.2 G including shutdown), 18.3 K of it in validation**: consistent with re-deriving cumulative work by 256-bit arithmetic and ordering a `std::set` by it (not separately instrumented). LevelDB (reading the index) is 11.5 %.

Regtest startup is 50 % `sha512` (RNG seeding) and 39 % LevelDB.

### 7.2 `getblock`, 2,000 calls, random heights in [421,000, 440,000) (testnet, small blocks; `-dbcache=1024`, txindex on)

| verbosity | instr/call | CPU/call | top costs |
|---|---|---|---|
| 0 (hex) | **3.53 M** | 0.90 ms | RPC/JSON 53.9 % (`json_escape` **32.9 %**), SHA-256 30.6 % (`Lloop*`), block I/O 9.3 %, X11 3.6 % |
| 1 (txids) | **2.23 M** | 0.83 ms | SHA-256 46.1 %, RPC/JSON 26.1 %, block I/O 15.6 % (`fread`), X11 6.1 % |
| 2 (full txs) | **25.0 M** | 6.5 ms | `blockToJSON` 57 % inclusive, `TxToUniv` 45 %, `UniValue::write` 23.5 %, script→JSON 21 %, `EncodeDestination`/`EncodeBase58` 13.4 %, SHA-256 9.7 %; peak RSS 2,132 MiB |

The block's hash (X11) and every tx's txid (double SHA-256) are **recomputed on each call**; v1 spends 52 % of its instructions on those two hashes (SHA-256 46.1 % + X11 6.1 %). Core already caches the txid inside a `CTransaction` object, but each call re-reads and deserialises the block into fresh objects (`rpc/blockchain.cpp`, `node/blockstorage.cpp`, `primitives/transaction.cpp`), so the hash is recomputed when the object is built; a cache that lives in the object does not avoid this work. Avoiding it needs hashes that survive a block re-read (a persisted or cross-call cache) or a raw-block serving path.

## 8. Metric #5 baseline: dashd per-block connect latency (DEC-108)

**Testnet-derived. This is not comparable with CH2a's mainnet trace (p99 ≈ 3–4 ms at B_DEC43); it is the dashd side of metric #5 on the data this task could reach.** The mainnet baseline comes later, from B1's bench windows.

**Method.** `contrib/node/pf1/metric5.py`. For each of 5 testnet heights and 2 configurations, 4 rounds (interleaved: round → height → config), the runner reflink-clones the snapshot, starts dashd with `-debug=bench -stopatheight=W+2000`, and parses the per-block log. No perf is attached. Per run, the first 10 blocks are dropped (cold start); **7,960 blocks per cell** (4 × 1,990).

| setting | value | why |
|---|---|---|
| coins cache | `-dbcache=263` → log line `Using 253.0 MiB for in-memory UTXO set (plus up to 4.8 MiB of unused mempool space)` (all 40 runs) | B_DEC43 = 252.75 MiB (DEC-43: `-dbcache=300`, txindex on). Window replays need `-txindex=0`, which returns the index's share to the cache, so 263 MiB reproduces the same coins budget to 0.1 %. |
| mempool allowance | `-maxmempool=5` | the empty mempool lends its unused space to the cache; the minimum keeps that to 4.8 MiB (DEC-43 asks for no allowance) |
| other | `-txindex=0 -disablegovernance -par=11 -parbls=11 -connect=0 -listen=0 -disablewallet`, pinned to CPUs 16–27 | B1's reindex scenario uses `-par` auto; `-par=11` / `-parbls=11` create 10 additional workers per queue (Core subtracts the connecting master), which with the master fills the 12-CPU slice |
| binary | the symbolized v24.0.0-rc.3 build (§1) | |

**Two latencies**, both from dashd's own timers (`-debug=bench`), in ms:

* **Full (`ConnectTip`)** = the log's top-level `- Connect block: X ms`. It covers block read from disk, `ConnectBlock`, the coins-view and EvoDB write-back (`Flush`), the `IF_NEEDED` disk flush (`Writing chainstate`), `UpdateTip` and post-processing. This is B1's `block_connect_ms_*` (`logparse.py`).
* **Narrow (`ConnectBlock`)** = the log's `- Connect total: X ms`: `ConnectBlock` only (validation and the in-cache coin updates). It is logged **before** the coins-view write-back, so it excludes the block read, `Flush`, `Writing chainstate` and tip bookkeeping.

(The harness's own name for the first is "Connect block"; dashd's label for the narrow one is "Connect total". They differ by 0.17–0.31 ms at p50, 0.25–1.7 ms at p95 and 0.27–0.60 ms at p99: the block read, `Flush` and the post-processing.)

| height | config | full p50 | p95 | **p99** | max | narrow p50 | p95 | **p99** | max | per-round full p99 |
|---|---|---|---|---|---|---|---|---|---|---|
| 421,000 | default assumevalid | 0.82 | 4.0 | 8.0 | 139 | 0.63 | 3.6 | 7.5 | 133 | 7–9 |
| 421,000 | `-assumevalid=0` | 1.12 | 11.4 | 133.5 | 337 | 0.93 | 10.5 | 133.2 | 337 | 91–153 |
| 925,000 | default assumevalid | 1.92 | 12.1 | 30.2 | 315 | 1.74 | 10.4 | 29.7 | 314 | 23–37 |
| 925,000 | `-assumevalid=0` | 9.68 | 31.7 | 240.9 | 1213 | 9.41 | 31.3 | 240.5 | 1212 | 203–279 |
| 1,140,000 | default assumevalid | 2.33 | 12.9 | 30.1 | 258 | 2.13 | 12.6 | 29.8 | 257 | 30–30 |
| 1,140,000 | `-assumevalid=0` | 10.21 | 42.8 | 261.1 | 1993 | 9.89 | 42.5 | 260.7 | 1992 | 200–286 |
| 1,340,000 | default assumevalid | 2.11 | 9.6 | 21.3 | 250 | 1.94 | 9.4 | 20.9 | 250 | 20–23 |
| 1,340,000 | `-assumevalid=0` | 8.99 | 31.7 | 193.3 | 1977 | 8.74 | 30.8 | 192.9 | 1976 | 178–220 |
| 1,520,000 | default assumevalid (above AV: checks on) | 8.35 | 37.0 | 201.5 | 1523 | 8.11 | 35.9 | 201.1 | 1522 | 163–247 |
| 1,520,000 | `-assumevalid=0` | 9.52 | 40.1 | 225.4 | 2088 | 9.26 | 39.5 | 224.8 | 2087 | 170–309 |

All numbers in ms; 7,960 blocks per row. The last column is the range of the four rounds' own p99.

**Reading it.**

1. **The tail is BLS, not the coins cache.** Where scripts and BLS checks run (`-assumevalid=0`, or above testnet's assumevalid block at 1,400,000), p99 is 133–261 ms and max 0.34–2.1 s. These are likely the quorum-commitment blocks of §2.3 (`m_qblockman.ProcessBlock` p95 19 ms; 7 blocks ≥ 1.6 s in the serial 1,520k window) and the blocks that call `GetQcHashes`; the metric-#5 runs did not keep per-block phase timings, so this attribution rests on the separate `bench` window runs and is not proven for the metric-#5 samples. With assumevalid skipping the checks (below 1.4 M) p99 is 8–30 ms.
2. **Headline cell for a node at the tip: 1,520,000, default config.** Full latency **p50 8.35 ms, p95 37.0 ms, p99 201 ms, max 1,523 ms** (per-round p99 163–247 ms). A rebuild stall of 3–4 ms is ≈ 40 % of this p50 and < 2 % of its p99.
3. **The quietest cell is 421,000 with assumevalid: p50 0.82 ms, p99 8.0 ms.** Even here a 3–4 ms rebuild is ≈ 4× dashd's p50 and ≈ 38–50 % of its p99. That is the window where a rebuild would show.
4. **This baseline does not establish how a populated-cache flush behaves.** Blocks hold 1.6–1.9 transactions, so filling a 253 MiB cache inside 2,000 blocks is implausible, but cache occupancy and the `Writing chainstate` phase were not preserved for the metric-#5 runs (Core's `UpdateTip` lines do log the cache occupancy; the runner discarded them). The only related evidence is from the separate `-dbcache=1024` window runs (`Flush` 0.04–0.10 ms per block on average, max ≤ 14 ms; `Writing chainstate` max 0.06–3.6 ms), which say nothing certain about the 263 MiB runs. **Whether the "Core empties its cache at every flush" effect behind CH2's metric #5 row appears here is therefore unknown.** Characterising it needs a mainnet window, or synthetic full blocks with a small `-dbcache`, with occupancy and flush phases kept; I did not run that.
5. **Noise.** The load average on agentbox was 26–257 across runs. Per-round spreads (four rounds per cell; **spread = max/min − 1** throughout): p50 up to 72 % (max/min 1.72×, i.e. ±27 % about the midpoint; 421,000 AV 0.65–0.96 ms, 925,000 AV 1.60–2.48, 1,520,000 AV 7.7–10.3, 1,520,000 noAV 7.8–13.4); p95 from 4 % (1,140,000 AV) and 10 % (1,340,000 AV) to 39–91 % in most cells and **3× at 421,000 noAV (5.1–15.6 ms)**; p99 from 3 % (1,140,000 AV, 29.6–30.5 ms, the steadiest) and 21 % (421,000 AV, 7.3–8.8) through 24–68 % (1,340,000 noAV 178–220; 925,000 AV 23.4–37.2; 421,000 noAV 91–153) to **82 % at 1,520,000 noAV (170–309)**; per-round max varies 2.5–3.4× in the cells with a commitment-block tail (1,140,000 noAV 810–1,993 ms; 1,520,000 noAV 607–2,088; 421,000 AV 53–139). **Read p99 as ±40 % (the worst per-round deviation from the pooled p99 is 36.9 %; max/min up to ×1.8 in the worst cell), max as order-of-magnitude, and p50/p95 of the cheap cells as ±25–50 %.** This is a descriptive envelope of four loaded-VM rounds, not a confidence interval and not a bound on quiet-host measurements. The raw samples and every run's load average are in `PF1/data/metric5/`.

**Where it is recorded.** The harness keeps baselines under `$NODE_BENCH_ROOT/baseline/<tag>/`. I wrote `baseline/pf1-metric5-testnet-v24/metric5.json` into my private root (the real `/work/bench` is fenced). The repo copy is `research/PF1/data/metric5/` (summary JSON, raw samples gzipped). **To import into the real root** (needs someone with `/work/bench` access): `mkdir -p /work/bench/baseline/pf1-metric5-testnet-v24 && gunzip -c research/PF1/data/metric5/metric5-raw.json.gz > /work/bench/baseline/pf1-metric5-testnet-v24/metric5.json`. Nothing in B1's harness reads this file; it is a record, not a gate input.


## 9. Core-relative instruction baselines (`bench_dash`, same tag)

| op | instructions |
|---|---|
| BLS verify (`BLS_Verify_Normal`) | 39.80 M |
| BLS sign | 12.86 M |
| BLS verify, 1,000-sig aggregated block | 18.3 G (18.3 M/sig) |
| ECDSA verify | 379 K |
| X11 (80 B header) | 169.6 K |
| double-SHA256, 80 B | 7.7 K |
| `DeserializeAndCheckBlockTest` (a stored block) | 44.8 M |
| `MempoolCheck` | 311.7 M |
| `AssembleBlock` | 3.42 M |

## 10. IBD: whole-chain headers + blocks 0–100,000 (testnet, one loopback seed, `-dbcache=1024`, default config incl. txindex)

| config | wall | blocks/s | instructions | CPU | top subsystems (instructions) |
|---|---|---|---|---|---|
| default AV, `-par=1` | 559 s | 188 | **1,261 G** | 299 s | **X11 73.9 %**, SHA-256 4.5 %, tx/block codec 3.7 %, BLS 3.7 %, validation 3.0 % |
| default AV, `-par=8 -parbls=8` | 572 s | 185 | 1,261 G | 328 s | identical |
| `-assumevalid=0`, `-par=1` | 836 s | 125 | **3,604 G** | 709 s | script verification 28.2 %, X11 25.9 %, BLS 25.6 %, SHA-256 13.8 % |

* **The node also downloads the seed's whole header chain (≈ 1.57 M headers) in these runs**, so instructions per block (1,261 G / 100 k) are **not** a per-block cost. X11 is 73.9 % ≈ 932 G ≈ **5.5 M header hashes at 169.6 K instructions**, i.e. ≈ 3.5 hashes per header counting the block path too. **Essentially all of the X11 (73.93 % inclusive against 73.90 % leaf) comes from `CBlockHeader::GetHash`**, called from **three** places per header (the inlined continuity loop `PeerManagerImpl::CheckHeadersAreContinuous`, called from `ProcessHeadersMessage` (21.5 %); `CompressibleBlockHeader::Uncompress`, which hashes the previous header to restore `hashPrevBlock` for compressed headers only and is skipped for the first header of a message (21.5 %); `ChainstateManager::ProcessNewBlockHeaders` (21.4 %), all as shares of all instructions; 29.1 / 29.0 / 29.0 % of the X11 work, together 87 %). That is ≈ 3.05 hashes per header (≈ 4.8 M); the remaining ≈ 9.5 pp of X11 is on the block path. Nothing caches the hash.
* **No parallel speed-up from `-par`**: 188 vs 185 blocks/s. 85 % of CPU is on the message-handler thread; script-check threads are idle because assumevalid skips the checks and the header PoW runs serially on the handler.
* Without assumevalid the bill is 2.9× higher and spreads across script verification, X11 and BLS.

## 11. Candidate opportunities for rdashd: projected ceilings, not gains

**Everything in this section is projection from the measured shares in §2–§10; nothing was measured on a rewrite.** Conventions:

* A **ceiling** assumes the targeted cost falls to zero (or to the stated residual) and nothing else changes. It is an upper bound on a gain, never an expected or promised one.
* Each row names the **measured cost it applies to**; a ceiling applies to that cost only.
* **Assumptions** are listed per row. An *external* assumption (a factor from outside this task) has no PF1 evidence and is marked so.
* The workloads differ (checked testnet connect, AV-IBD, synthetic cold blocks, regtest RPC calls), so the order below is **not** a mainnet ranking. The rule: **each row is ranked by its largest PF1-only conditional ceiling** (zero-cost removal of the measured share; no external factor A1/A8 takes part in the sort), descending; rows whose ceiling PF1 cannot quantify go last. The sort operands are: row 1 all-BLS zero-cost 1/(1 − 0.967) = **30.3×**; row 2 **4.53×**; row 3 v0 **3.04×**; row 4 **2.86×**; row 5 **2.24×**; row 6 the union-removal scenario **2.14×** (proof-only is 1.46×); row 7 hash reuse **1.75×**; row 8 decode G1+G2 zero-cost at 20 % **1.25×**; rows 9–10 none. These are bounds, not gains; the more realistic and external scenarios in the last column are not sort keys. Ratios are instruction ratios unless marked wall.

| # | opportunity, and the measured cost it targets | assumptions | ceiling / range (projected) |
|---|---|---|---|
| 1 | **BLS arithmetic.** Targets the BLS subsystem: 84–97 % of instructions in every checked window (96.7 % ex background at 1,520k, §2.1). Only part of it is identified by operation: BLS `Verify*`/`AggregateVerify*` 22.7–25.3 % inclusive and G1+G2 decode/check 10–20 % in the 299–599-block DWARF windows (§2.2; the rows overlap). | **External A1:** a 3–4× per-operation instruction reduction from a different BLS library (blst-class); PF1 contains no measurement of it. Legacy decode quirks apply (memory `dash-bls-wrapper-decode-domain`). | **(a) Verify only** at A1, **per DWARF window, each with its own total and its own verify share** (no window has a verify share *and* the long-window baseline, so nothing is translated to 173 M): 1,520k checks-on 230.6 M/block × 25.0 % → 192.1–187.3 M (1.20–1.23×); 925k noAV 210.6 M × 22.7 % → 178.7–174.7 M (1.18–1.21×); 421k noAV 43.0 M × 25.3 % → 35.8–34.8 M (1.20–1.23×). Applying the ≈ 1.2× to the 2,000-block windows would additionally assume the verify share is the same there (**unmeasured**). **(b) All profiled BLS work** (verify, decode, aggregation, field arithmetic under all of it) at A1: 173 M → ≈ 48–62 M/block (2.8–3.6×). (b) requires the 3–4× to apply to *all* BLS work, not to the pairing verify alone; its 173 M and 96.7 % come from the same window (1,520k `serial-noav-nobench`, ex background). |
| 2 | **Script-check parallelism for cold full blocks.** Targets `connect-big` (synthetic, `-par=1`): script verification is 85.03 % of instructions and 77.8 % of CPU time (§4). | A2: the checks scale perfectly over 12 workers; the shares stand in for time shares; the rest of the block stays serial; no pipeline overlap. **Wall critical path not measured.** | **Ideal Amdahl at 12 cores: 1/(0.1497 + 0.8503/12) = 4.5× (instruction share), 1/(0.222 + 0.778/12) = 3.5× (CPU share).** The earlier "6–8×" is retracted: it exceeds both. Two separate levers, *not* included above and unquantified: **pipeline overlap** (issue checks while the block is still being read), and a **signature cache**: a hit skips the ECDSA verify (379 K of 515 K instr/tx = 74 %), so instruction savings ≈ 0.74 × h for hit rate h, which was not measured. |
| 3 | **`getblock` response construction.** Targets measured buckets per call (§7.2): v0 SHA-256 30.6 % + X11 3.6 % + `json_escape` 32.9 %; v1 SHA-256 46.1 % + X11 6.1 %; v2 hashes 10.5 %, RPC/JSON tree 72.4 %. | A3: the named buckets drop to zero residual (not achievable: hex encoding of the raw block, response copy and, for v2, the output semantics of `TxToUniv`/Base58 still have to be produced). Hash buckets can only be removed by hashes that survive a block re-read (§7.2) or a raw-block serving path; a per-object cache does nothing. | v0: hashes only ≤ 1.5×, hashes + escaping ≤ 3.0×; **v1: ≤ 2.1×** (hashes only; its remaining 26 % RPC/JSON stays); v2: hashes only ≤ 1.1×; no defensible ceiling for the tree portion. The earlier ≈ 5× / ≈ 2× / ≈ 3× are retracted. |
| 4 | **`getblocktemplate` serialisation.** Targets the RPC/HTTP/JSON bucket: 65.0 % of instructions at 5,000 pool txs, 58.9 % at 20,000 (§6; 6 measured calls each). | A4: the bucket is serialisation that can be avoided; a fraction ρ of it remains as required hex/output generation, dispatch, copying and transport. | **≤ 1/(0.35 + 0.65 ρ) at 5,000 txs**: ρ = 0 → 2.9× (zero-cost bound, not an estimate), ρ = 0.25 → 2.0×, ρ = 0.5 → 1.5× (ρ values illustrative, not measured). At 20,000 txs the zero-cost bound is 2.4×. Incremental assembly (mempool 4.3 %, coins cache 7.3 %, `CreateNewBlock` 28.7 % / 35.2 % inclusive of which `TestBlockValidity` 23.8 % / 28.5 %) is unquantified. |
| 5 | **Mempool accept.** Targets 850.8 K instr/tx (§5): ECDSA 379 K (44.6 %); RPC transport 14.2 % (the benchmark's own, approximate); inclusive `CSHA512` 6.2 % in two salted-view constructors. | A5: everything except the verify is free (floor). The ex-transport 730 K is an approximation, not a measured P2P-origin cost. | **≤ 851/379 = 2.2×** (≤ 1.9× on the 730 K basis). Concrete items, budgeted separately: `CCoinsViewCache` constructor RNG ≤ 3.5 % of instructions, `CCoinsViewMemPool` constructor RNG ≤ 2.7 %. A signature-cache hit removes the verify itself (hit rate not measured). |
| 6 | **Startup index load.** Targets the 81.5 % "validation/index work" bucket of 35.2 G instructions / 19.4 s (§7.1): `GetBlockProof` 31.3 % inclusive; work comparator ≈ 21.9 %, union ≈ 53.2 % (round-1 reading of the committed flamegraph); `LoadBlockIndexGuts` 9.9 %; LevelDB reads 11.5 % of instructions, 5.9 % of CPU time (≈ 1.1 s). | A6: persisting cumulative work removes the proof computation (and only with the right layout the ordering cost); index construction, height sorting, skip links and allocation remain. | **≤ 1.46× instructions** from removing `GetBlockProof` only (1/(1 − 0.313)); **≤ 2.1×** if the union (53.2 %) also disappears. Wall time and RSS gains are **not quantified** (no layout or cost model; the earlier "1–3 s" and "≥ 2–3× RSS" are retracted). |
| 7 | **Header hashing in IBD.** Targets X11 = 73.9 % of the whole-chain-headers + 100 k-block AV IBD (1,261 G; §10). Header paths (`CheckHeadersAreContinuous`, `Uncompress` for compressed headers, `ProcessNewBlockHeaders`) carry 87 % of the X11; the other ≈ 13 % is on the block path. | A7: the three header-path hashes collapse to one per header (≈ 3.05 → 1 per header); the block-path 13 % is unchanged. **External A8** (second line only): a Core-exact fast X11 at 0.23× Core's instructions (memory `dash-x11-xh1`; not PF1 evidence). | Remaining instructions = 0.261 + 0.739 × (0.87/3 + 0.13) × c. **c = 1 (hash reuse only): 0.571, i.e. ≤ 1.75×.** c = 0.23 (A8): 0.332, i.e. ≤ 3.0× (the earlier 3.2× dropped the 13 % residual). Parallel PoW checking: wall effect unquantified. |
| 8 | **Decode each BLS signature and key once.** Targets G2 `FromBytes`/`IsValid` 5–9 % and G1+G2 10–20 % of instructions in the checked DWARF windows (inclusive, overlapping the verify path), ≈ 20 % G2 in the one AV DWARF window (261 samples, 28 % of them stackless) (§2.2). | A9: k decodes per signature today; the repeat count was **not measured**. Whether the subgroup check may be skipped on already-validated chain data is a BLS-wrapper design question. | **≤ share × (1 − 1/k) ≤ 5–9 % (G2) / 10–20 % (G1+G2)** of block instructions in checked windows. Cannot be narrowed without the repeat count. |
| 9 | **Special-tx BLS off the connecting thread.** Targets the connecting thread's 14–17 ms CPU/block (it also executes checks) and the commitment-block phase (§3). | The serial fraction s is not separated from stolen checks (§3). | **No ceiling is quantified.** Observed (single loaded-VM run pairs): wall 1.13–1.30× at par8; commitment-phase maxima 2.98× and 3.31× lower; whole-connect max 2.76× lower at 1,520k and *higher* at 421,000. These are observations, not p99 predictions. The idealised Amdahl ceiling is at least 1.6–2.0× (§3) but is a bound, not a target. A matched experiment should set the target. |
| 10 | **Storage write path.** Targets EvoDB/MN-list 4.8–4.9 % (AV windows ex background, 925k–1,340k; 18.9 % at 421k), which includes non-write work, and the background LevelDB thread: 14–37 % of all-thread instructions in AV windows (§2.1). | A10: its cause is not isolated (reopen vs replay-induced compaction; no idle-only control or length sweep), so a rewrite may pay part or all of it. ST1 kept RocksDB (≥ 1.6× slower than LevelDB in a tight-memory cell). | **Not quantified.** The earlier "1–5 %" is retracted: it assumed the compaction is a fixed artefact and treated all EvoDB work as writes. Needs a write-only breakdown and a length sweep first. |

Basis strength, for PF2 planning: the **measured shares** behind rows 1 (BLS dominance), 7 (header hashing: three code paths confirmed in the pinned source) and 3/4 (RPC/JSON buckets) are the best supported; their ceilings are still bounds. Rows 2, 5, 6 and 9 rest on modelling assumptions as stated; rows 8 and 10 need a further measurement before they can carry a number.

**Re-ranking.** Against the first draft, the order changed because the corrected ceilings changed: script parallelism moved up (9 → 2: its ideal ceiling is 3.5–4.5×, no longer 6–8×, but it was previously mixed with the cache lever), `getblock` (7 → 3) and `getblocktemplate` (6 → 4) now have zero-residual bounds of 2–3×, mempool accept (8 → 5, 2.24×) and startup (5 → 6, 2.14× union scenario) have the next bounds, header hashing fell (4 → 7: ≤ 1.75× from hash reuse alone), decode-once fell (2 → 8: ≤ 5–20 % with an unmeasured repeat count) and BLS off-thread and storage have no quantified ceiling. Round 2 (verify-only kept per window, ranking operands published) did not change the order.

Not an opportunity, though it looks like one: **the coins cache.** It is 0.03–1.8 % of instructions in every testnet window (2.8 % at 421k AV ex-background); on the synthetic full block it is 2.7 % of instructions but 10.5 % of CPU time (consistent with cache misses, not measured), which is where CH2's work shows up. The one concrete coins-cache cost found is the per-accept view construction in mempool accept (row 5).

Caveat for every row: the `tables.md` inclusive and anchor tables are regex-over-signature artifacts; the ceilings use the leaf-owner subsystem shares (§2.1), the DWARF caller table (§2.2), the bench log and the committed flamegraphs.

## 12. How to reproduce

```bash
# 1. symbolized build (docker, ~1 h); official binary for seeds comes from B1's fetch
contrib/node/pf1/build-dashd-symbols.sh v24.0.0-rc.3 8 /work/scratch/pf1

# 2. testnet snapshots (one reindex-chainstate walk, reflink copies at each window start)
contrib/node/pf1/walk-snapshots.sh <dashd> <synced testnet datadir> <out> 421000 925000 1140000 1340000 1520000

# 3. window matrix (perf record, 5 configs per window)
contrib/node/pf1/run-matrix.sh bin/sym/dashd <out> runs 2000 421000 925000 1140000 1340000 1520000
#    DWARF spot checks
contrib/node/pf1/run_window.py --dashd bin/sym/dashd --callgraph dwarf --period 40000003 --freq 97 --bench 0 \
    --snap <out>/snap-925000 --start 925000 --blocks 400 --out runs/dwarf-w925000-serial-av

# 4. RPC, startup, regtest and IBD workloads
contrib/node/pf1/run_regtest.py --dashd bin/sym/dashd --workload {startup-testnet,startup-regtest,getblock,mempool-accept,gbt,ibd-testnet,prep-big} ...
contrib/node/pf1/run_window.py --net regtest --reindex-chainstate --snap runs/big-prep/datadir --start 176 --blocks 5 --assumevalid 0 ...   # connect-big

# 5. tables, flamegraphs
contrib/node/pf1/parse_bench.py runs/w*/ ; contrib/node/pf1/report_tables.py --out tables runs/*/   # skip ab-*, microbench, big-prep
contrib/node/pf1/dwarf_callers.py runs/dwarf-*/                          # section 2.2
contrib/node/pf1/flamegraphs.sh <FlameGraph dir> research/PF1 runs/...

# 6. metric #5 (§8)
contrib/node/pf1/metric5.py --dashd bin/sym/dashd --snaps <out> --out <NODE_BENCH_ROOT>/baseline/pf1-metric5-testnet-v24
```

Fixture for the regtest workloads: B1's `regtest-v1` (177 blocks, 30,000 fan-out txs, generated by dashd 23.1.8, copied into the private root before use). Flamegraphs: `research/PF1/*.svg`, 31 files (instruction-weighted for every run; CPU-time for the parallel and IBD runs).
