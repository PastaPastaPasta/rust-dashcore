# B1 — node-bench harness and dashd baseline

Status: **partial baseline, provisional** (2026-10-08). The harness is done on `node/bench-harness` (worktree `~/workspace/rdc-wt/bench-harness`, `node-bench/`). Two things are still running in the background:
- the mainnet network IBD;
- the rest of the baseline queue: seeded testnet IBD, reindex, spv-headers, then every mainnet scenario once the mainnet chain exists, and then the v24 regtest/testnet queue.

**Every number below is provisional.** agentbox was heavily shared throughout (see "Noise" below). The harness classes every run's foreign load, and none of these runs was quiet. Official numbers need `--quiet-window` runs in a reserved window.

## Pinned dashd

| | 23.1.8 (mainnet parity, primary) | 24.0.0-rc.3 (testnet; PLAN §8.1) |
|---|---|---|
| Release | v23.1.8, 2026-08-03 (latest stable) | v24.0.0-rc.3, 2026-10-07 (24.0.0 final not released) |
| Tarball sha256 (x86_64-linux-gnu) | `e664cad0fe860afa3d005be0db1eb06da598f2f83421083afae77191e3115825` | `9327f00018afd12f11676ac66e688c76b253e4783bf0d595f08476de4269ff52` |
| Signatures on SHA256SUMS.asc | pasta, knst, UdjinM6 (all good) | pasta (good) |
| Source commit (for bench_dash) | `728f5055836c6d29806412fc7223ac8fe05af991` | `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18` |
| bench_dash | built from the tag with depends in `debian:bookworm` (`scripts/build-dashd-bench.sh`) | build running |

`contrib/setup-dashd.py` pins 23.1.0 for the integration tests; the benchmark baseline deliberately uses the newest release. The develop checkout (`~/workspace/dash` @ `15db478750`) was not benchmarked. `build-dashd-bench.sh` takes any ref, if a develop baseline is wanted.

## Host

agentbox is a KVM guest:
- **CPU and memory:** 2 × Xeon E5-2687W v4 (32 vCPU, 2 NUMA nodes 0-15 / 16-31, no SMT), 157 GiB RAM.
- **Storage:** `/work` is XFS on a virtio disk (rotational=0, reflink=1, I/O scheduler `none`).
- **Software:** Ubuntu 26.04.1, kernel 7.0.0-38-generic, Python 3.14.4, rustc 1.98.1.
- **Harness revision:** `288c2a78`. Every result JSON records the full environment.

## Noise (manager note 2026-10-08)

The box ran, concurrently with every measurement:
- two reference dashd nodes (`/work/chains`);
- 7–43 cargo/rustc processes from other agents;
- clang builds;
- dashd integration tests.

Load average was 15–85 throughout. Measured foreign CPU on the regtest measurement slice (16-27@1) was **50–95%**, and on the host 17–30 cores. So every run so far is classed **heavy** and is provisional.

The methodology is built around this, and is detailed in `node-bench/README.md` §Fairness:

1. **Load record per run.** Snapshots record the load average, PSI, and foreign dashd/cargo/C-build/test processes. A 2 s monitor tracks foreign CPU host-wide and on the node's slice: busy CPU minus the CPU of processes carrying the run's `NODE_BENCH_MARK`. Each run is classed quiet, moderate or heavy, and labelled official or provisional.
2. **`--quiet-window`.** Waits, or refuses with `refused-noisy`, until a 30 s window shows slice foreign ≤ 10%, host foreign ≤ 25% and CPU PSI < 5%. It is required for official baselines.
3. **Paired comparisons only.** `run pair` measures A and B under the same conditions:
   - **interleaved** ABBA rounds on the same slice; or
   - **concurrent** runs on disjoint equal cpusets (0-11@0 + aux 12-15 vs 16-27@1 + aux 28-31). Slices swap every round, a barrier starts both sides together, the cache is dropped once per round, and identical cgroup v2 caps are optional (`--limits cpu=…,mem=…`; `io.weight` is ineffective with the `none` scheduler).

   Reports give the paired geometric-mean speedup with a 95% t-interval, both sides' medians and p95s, and the p-value.
4. **The verdict is PRD NFR-PERF-02(g)**: B's median worse by ≥ 1% and one-sided Mann-Whitney p < 0.05 over per-run values. Exact Mann-Whitney with ties.
   - Fewer runs than (f) requires gives INSUFFICIENT.
   - Errored, interrupted or missing sides give ERROR.
   - A FAIL is re-run in an independent pair and stands unless that re-run measures the metric cleanly without failing.
   - `--strict` adds non-inferiority (CI lower bound ≥ 0.98).
   - Gated metrics are independent quantities only; derived and self-reported metrics are information.
5. **A/A noise floor** (`pair --a dashd --b dashd`). Measured concurrently under heavy load with cgroup caps, 3 rounds: `startup_s` 1.08× [0.86–1.36], `tx_accept_tps` 1.01× [0.86–1.19], `idle_rss_mib` 1.00× [1.00–1.00]. Wall-clock resolution on this box under load is therefore about 15–25%.

## Controlled network

Seeded IBD uses **4** local dashd seeds over localhost by default (`--seeds`). Each seed is a reflink clone of the frozen chain, whitelisted. One seed is latency-bound: dashd keeps at most 16 blocks in flight per peer. Measured on testnet:

| configuration | blocks/s |
|---|---|
| one seed (both processes under one core) | ~110 |
| four seeds | ~445 |
| real network, 10 peers (same block range) | ~670 |

The 4-seed run was taken at load 50–70.

## Baseline (dashd 23.1.8)

Unless stated, values are medians. "n" is repetitions or samples. "p95" is over samples. cv is the coefficient of variation.

### Regtest and microbenchmarks

All runs on node slice `16-27@1`, aux `28-31`, class heavy.

| # | scenario · metric | value | n | p95 | cv | definition / note |
|---|---|---|---|---|---|---|
| 9 | `startup-regtest` · `startup_s` | 0.452 s | 10 | 0.596 | 14% | spawn → RPC out of warmup, fixture chain (177 blocks), warm |
| 9 | `startup-regtest` · `shutdown_s` | 0.560 s | 10 | 0.747 | 14% | `stop` → process exit |
| 10 | `startup-regtest` · `idle_rss_mib` | 128.0 MiB | 10 | 128.5 | 0.4% | 5 s after ready |
| 7 | `mempool-accept` · `tx_accept_latency_ms` | 1.08 ms | 5 reps | 1.61 | 27% | median of per-rep medians, 2000 sequential sendrawtransaction; overall p99 4.99 ms |
| 7 | `mempool-accept` · `tx_accept_tps` | 2900 tx/s | 5 | 3077 | 10% | 20,000 txs from 8 client processes. Client ceiling 10,600 ops/s, so this is not client-bound. |
| 10 | `mempool-accept` · `mempool_rss_mib` | 158.7 MiB | 5 | 160.1 | 1% | 22,000 txs in the mempool |
| 8 | `gbt-regtest` · `gbt_ms_mempool_0` | 2.03 ms | 10 | 3.38 | 29% | fresh template each sample (+1 tx, 5.2 s wait) |
| 8 | `gbt-regtest` · `gbt_ms_mempool_1000` | 46.1 ms | 10 | 164 | 73% | CreateNewBlock 22.8 ms |
| 8 | `gbt-regtest` · `gbt_ms_mempool_5000` | 172 ms | 10 | 262 | 25% | CreateNewBlock 81.8 ms |
| 8 | `gbt-regtest` · `gbt_ms_mempool_20000` | 341 ms | 10 | 731 | 45% | block full at 8,880 txs (2 MB); CreateNewBlock 227 ms |
| 13 | `rpc-regtest` · latency (c1) | getblockchaininfo 0.93, getbestblockhash 0.92, getblockcount 0.76, getblockhash 1.18, getblockheader 1.43, getblock v0/v1/v2 1.46/1.58/1.66, getrawtransaction v0/v1 4.09/3.94, gettxout 0.85, getmempoolinfo 2.03, getnetworkinfo 1.45, getaddressbalance 0.72, getaddressutxos 1.87, getaddresstxids 1.15, masternode count 1.01, masternodelist 1.01, protx list 0.99, quorum list 1.00 (ms) | 500 each | 3–48 | 57–490% | txindex + addressindex, keep-alive client |
| 13 | `rpc-regtest` · throughput, 8 clients | getblockchaininfo 2272, getbestblockhash 3160, getblock v1 2347, getrawtransaction v1 289, gettxout 2720, getaddressbalance 3216 (ops/s) | 5 s each | | | Recorded before c16/c64 existed (see pending) |
| 6 | `microbench` · ECDSAVerify | 82.4 µs/op | 5 | 234 | 61% | bench_dash v23.1.8, median of nanobench medians |
| 6 | `microbench` · ECDSAVerify_LargeBlock | 78.3 ms | 5 | | 76% | |
| 6 | `microbench` · BLS_Verify_Normal / Batched | 6.25 / 3.93 ms | 5 | | 34/21% | |
| 6 | `microbench` · BLS_Verify_LargeBlock1000 / LargeAggregatedBlock1000 | 7.27 / 3.34 s | 5 | | 19/29% | |
| 6 | `microbench` · CCheckQueueSpeedPrevectorJob | 2.40 µs/job | 5 | | 36% | |
| 6 | `microbench` · DeserializeBlockTest / DeserializeAndCheckBlockTest | 6.18 / 9.39 ms/block | 5 | | 96/91% | |
| 6 | `microbench` · Pow_X11_0080b / Pow_X11_1M | 383 / 4.85 ns/byte | 5 | | 81/49% | |
| 6 | `microbench` · DSHA256_1M, SHA256_STANDARD | 4.19, 8.00 ns/byte | 5 | | | |
| 8 | `microbench` · AssembleBlock, ComplexMemPool, MempoolCheck | 0.82 ms, 410 ms, 166 ms | 5 | | | |

The other 21 bench_dash rows (BLS DKG, GCS filters, CCoinsCaching, MuHash, …) are in the result JSON.

### Sync (network IBD, information only; not gated)

| # | scenario · metric | value | note |
|---|---|---|---|
| 1 | `ibd-testnet-net` · `ibd_time_s` | **6,069 s** (1 h 41 m) to height 1,568,368 | real P2P (10 peers), txindex=1, dbcache 4096, numa:1, default assumevalid, cold |
| 1 | `ibd-testnet-net` · header sync | 243 s | end of the initial header burst. The JSON shows 5,681 s from the pre-fix definition; recomputable from `samples.csv` |
| 1 | `ibd-testnet-net` · time to height 250k / 500k / 1M / 1.5M | 670 / 1,223 / 2,213 / 4,753 s | |
| 10 | `ibd-testnet-net` · peak / steady RSS | 2,327 / 2,081 MiB | |
| 12 | `ibd-testnet-net` · CPU | 8,070 s = 5.15 ms/block | |
| 11 | `ibd-testnet-net` · disk | total 6.22 GiB (blocks 4.53, evodb 0.59, indexes/txindex 0.40, chainstate 0.11, llmq 0.03) | writes 12.6 GiB |
| 1 | `ibd-mainnet-net` | **running**: height 1,704,261 / 2,551,728 at t = 13,210 s (06:59 UTC), ~60 blocks/s under load | ETA ~4 h |

## Running in the background

| what | process | log | result |
|---|---|---|---|
| mainnet network IBD (`--keep-datadir /work/bench/data/seed-main`) | harness PID 686700, dashd PID 686828 | `/work/bench/logs/ibd-mainnet-net.log` | `/work/bench/results/20261008T031906Z-ibd-mainnet-net-dashd.{json,md}`, and the datadir moves to `data/seed-main` |
| 23.1.8 baseline queue (`scripts/baseline.sh`) | bash PID 2731878 | `/work/bench/logs/baseline.log` | `/work/bench/results/*.json`, step markers in `/work/bench/baseline/baseline-v23.1.8/` |
| v24 regtest + testnet queue (starts after the 23.1.8 testnet steps) | `chain-v24.sh` (PID 4000276); `touch /work/bench/baseline/STOP-v24` cancels it | `/work/bench/logs/baseline-v24.log` | tag `baseline-v24.0.0-rc.3` |

Queue order:
1. `ibd-testnet-seed` (running: 4 seeds, 190k blocks at 600 s)
2. `ibd-testnet-seed-noav`
3. `reindex-testnet`
4. `reindex-chainstate-testnet-noav --bench-log 1` (per-block connect latency, script-check throughput)
5. `spv-headers-testnet` for dashd, then dash-spv
6. Once `data/seed-main` exists, mainnet in this order: `startup-mainnet`, `gbt-mainnet`, `ibd-mainnet-seed`, `reindex-chainstate-mainnet`, `reindex-chainstate-mainnet-noav --bench-log 1`, `reindex-mainnet --addressindex 1` (builds `data/indexed-main`), `rpc-mainnet`, `rescan-mainnet`, `ibd-mainnet-seed-noav`, `spv-headers-mainnet` (dashd, dash-spv)

The queue resumes where it stopped if re-run. Collect results with:

```
node-bench/run summary --impl dashd --md /tmp/b1.md
```

If the mainnet IBD's harness dies after the node finishes, recover it with:

```
node-bench/run recover /work/bench/results/runs/20261008T031906Z-ibd-mainnet-net-dashd --keep-datadir /work/bench/data/seed-main
```

That run started before the robustness fixes, so it uses the original code.

## Pending metrics

- **Waiting on the queue (hours to about two days of machine time):**
  - #1–2 local-seed IBD (mainnet, testnet, with and without assumevalid)
  - #3 reindex and reindex-chainstate
  - #4 rescan
  - #5 per-block connect latency
  - mainnet startup and GBT
  - mainnet RPC with addressindex
  - spv-headers
  - RPC throughput at c16/c64: the next `rpc-regtest` run records these (the code landed after the regtest baseline ran)
- **Masternode #14–18** (DKG round, recovered-sig, IS lock, ChainLock, CoinJoin) are defined (`mn-*-regtest`, PENDING). They need the Rust node to act as a masternode. Method:
  - Spin up a local MN regtest, via either `dashpay/regtest-blockchain` `regtest-mn` (already fetched by `setup-dashd.py`) or Dash's functional framework (`DashTestFramework`, `mine_quorum`, `-llmqtestparams`).
  - Time from the observer: quorum height → final commitment; `quorum sign` → `hasrecsig`; `sendrawtransaction` → `instantlock`; new block → `getbestchainlock`; CoinJoin dsa → dstx.
  - Run dashd-only and mixed networks with per-node binaries.
- **Official numbers.** Every scenario re-run with `--quiet-window`, or as PF0 counters (below).

## Counter workloads for PF0

`node-bench/run workload <name> --wrap "<perf stat …> --"` prepares fixed inputs (cached per binary), then runs the node under the wrapper for one fixed piece of work, and writes a manifest with the op count.

Workloads: `startup`, `mempool-accept`, `gbt`, `rpc --call <label>`, `reindex-chainstate-testnet` (to `-stopatheight`), `ibd-testnet-seed`.

Measured with `instructions:u` (allowed at `perf_event_paranoid=2`):
- `startup`: 1.02 G vs 1.33 G instructions between runs. dashd's timer threads scale with wall time.
- `mempool-accept --txs 1000`: 2.31 G vs 2.36 G (2%).

Syscall tracepoints are refused at paranoid=2 (perf exits 129). Use `strace -c -f` as the wrapper, or a privileged counter setup.

## Findings for the manager

1. **PRD NFR-PERF-02 (f) and (g) conflict for IBD.** (f) allows 3 runs, but with 3 vs 3 the smallest one-sided Mann-Whitney p is exactly 0.05, so (g)'s p < 0.05 can never flag a 3-run IBD as slower. The harness treats < 4 runs as INSUFFICIENT for long scenarios. Suggest amending (f) to "≥ 4 for IBD".
2. **Rule (g) can pass an under-powered slowdown.** Mann-Whitney over rounds ignores the pairing, and "not shown slower" counts as a pass. Recommend `--strict` (non-inferiority) for milestone reports. The per-PR gate moves to PF0 counters anyway.
3. **PRD NFR-PERF-02(a) ("nothing else running") is not achievable on agentbox today.** The oracles, builds and tests run continuously. Wall-clock milestone numbers need reserved quiet windows (R-15), and someone has to schedule them.
4. **Deviations from the PRD's metric definitions** (fixable; say which you want):
   - #4 rescan uses a small watch-only wallet, not one with ≥ 100k txs. Needs a real large wallet; the bait9 descriptor set from the SPV work could be used.
   - #7 uses a pre-signed regtest stream, not a replayed mainnet stream.
   - #8 measures 0/1k/5k/20k-tx mempools (block-full at about 8.9k txs), not a 300 MB mempool.
   - #5 is measured over the whole reindex (filter with `--block-stats-from` for the last 50k blocks) rather than only live at the tip.
   - #9 doesn't yet do with/without-index variants.
5. **Regtest is not mainnet-like by default.** Its consistency checks (`-checkmempool`, `-checkblockindex`) cut mempool acceptance to about 8 tx/s at 6k entries. node-bench turns them off. Anyone benchmarking regtest elsewhere needs to do the same.
6. **The test chains come from 23.1.8.** The regtest fixture and the testnet/mainnet seed chains were produced by 23.1.8. In the v24 queue, the seeds and the node under test run v24 on clones of those datadirs.

## Metric #5 (per-block connect latency) — dashd v24 testnet baseline (PF1, DEC-108)

Recorded by PF1, not by the B1 queue: **testnet-derived**, 253.0 MiB coins cache (B_DEC43 within 0.1 %), p50/p95/p99/max of the full `ConnectTip` and the narrow `ConnectBlock` latency at five testnet heights, 4 rounds each, with the load average of every run. Method, table and caveats: [PF1-dashd-v24-profile.md §8](PF1-dashd-v24-profile.md). Data: `PF1/data/metric5/`. A mainnet baseline is still to come from the B1 bench windows.
