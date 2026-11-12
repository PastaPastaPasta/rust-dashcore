# node-bench

Reproducible side-by-side benchmarks of **dashd** and the **rust-dashcore node**.

The program rule is that the Rust node is never slower than dashd on any tracked
metric (same hardware, network conditions and configuration), and aims for 2x
dashd wherever the node's own work is the bottleneck. node-bench measures that.
It records a pinned dashd baseline today, and the Rust node plugs into the same
scenarios once it exists (see [Plugging in the Rust node](#plugging-in-the-rust-node)).

```bash
node-bench/run list -v                        # scenarios and exact metric definitions
node-bench/run fetch-dashd                    # pinned Dash Core release, sha256 + signatures checked
node-bench/run run quick                      # regtest: startup, RPC, mempool, getblocktemplate
node-bench/run run microbench                 # bench_dash (build: scripts/build-dashd-bench.sh)
node-bench/run run ibd-mainnet-net --keep-datadir /work/bench/data/seed-main   # hours
node-bench/run pair quick --rust-bin <rust-node> --quiet-window --reservation <window> --tag m3   # wall-clock gate
node-bench/run summary --impl dashd           # latest result per scenario, as Markdown
node-bench/run workload mempool-accept --wrap "perf stat -D -1 --control fifo:ctl,ack -x, -o c.csv --" \
               --ctl-fifo ctl,ack                                       # one fixed workload under counters (PF0)
```

Per DEC-23, wall-clock side-by-side comparisons (`pair`) are milestone evidence and run only in
manager-reserved quiet windows. Per-PR gating uses PF0's deterministic counters, for which `run workload`
provides the fixed work and its measurement boundaries.

Python 3.11+, stdlib only. Lint with `uvx ruff check node-bench`; unit tests
(parsers, statistics, the gate) with `python3 -m unittest discover -s node-bench/tests`.

## Where things live

Nothing large goes in the repository. Everything else is under `NODE_BENCH_ROOT`
(default `/work/bench`):

| path | contents |
|---|---|
| `bin/dashcore-<v>/` | pinned release, from `run fetch-dashd` |
| `bin/bench_dash-v<v>` | dashd microbenchmarks, from `scripts/build-dashd-bench.sh` |
| `data/seed-main`, `data/seed-test` | fully synced datadirs produced by the `*-net` IBD runs. They serve the local-seed scenarios, and every run gets a reflink clone of them. |
| `data/workloads/<impl>-<binary>-<fixture>` | PF0 workload inputs, prepared per binary (newest three kept per implementation) |
| `data/indexed-main` | mainnet with txindex+addressindex, for `rpc-mainnet` |
| `data/fixtures/regtest-v1` | regtest chain + 30,000 pre-signed transactions |
| `results/<run_id>.json` / `.md` | one result per (scenario, implementation) run |
| `results/runs/<run_id>/` | artifacts: harness.log, samples.csv, `logs/` (node log tails); datadirs only while running (see [Retention](#write-boundary-and-retention)) |
| `results/pairs/<pair_id>/report.{md,json}`, `expect.json` | paired comparisons, and what each pair set out to run |
| `results/ledger.jsonl` | one line per paired comparison: the per-PR history |

## Pinned dashd

- **Baseline:** Dash Core **v23.1.8**, the newest release (2026-08-03), x86_64 linux-gnu tarball.
  - sha256 `e664cad0fe860afa3d005be0db1eb06da598f2f83421083afae77191e3115825`, matching the release's `SHA256SUMS.asc`.
  - That file carries good signatures from pasta, Konstantin Akimov and UdjinM6.
  - `paths.py` pins the hash, and `fetch-dashd` re-verifies it.
- `contrib/setup-dashd.py` pins 23.1.0 for the integration tests. The benchmark baseline is deliberately the newest release.
- **Second baseline: v24.0.0-rc.3** (2026-10-07; 24.0.0 final hasn't shipped yet). Testnet runs v24, while mainnet parity stays pinned to 23.1.8 (PLAN §8.1), so both are recorded.
  - sha256 `9327f00018afd12f11676ac66e688c76b253e4783bf0d595f08476de4269ff52`, verified against `SHA256SUMS.asc` (signed by pasta).
  - Fetch it with `run fetch-dashd --version 24.0.0-rc.3`.
  - Select it with `NODE_BENCH_DASHD_VERSION=24.0.0-rc.3`, or run `scripts/baseline.sh --dashd-version 24.0.0-rc.3 --skip-mainnet`.
  - Replace it with 24.0.0 when that ships.
- **bench_dash:** the release tarball doesn't ship it.
  - `scripts/build-dashd-bench.sh v23.1.8` builds it from the tag (commit `728f5055`) with depends, inside `debian:bookworm`.
  - For a develop baseline, the same script works with any ref in `~/workspace/dash`.

## Metrics

Each metric is defined in code (`run list -v` prints the definitions) and stored with its result:
- `value`: the headline figure
- `statistic`: how that figure is computed
- `samples` and descriptive `stats` (n, min, median, p95, p99, cv, MAD)
- `better`: lower or higher
- `gate`: whether it participates in the merge gate
- `target_2x`: whether the 2x target applies

| group | scenario | headline metrics | config | cold/warm | reps · statistic |
|---|---|---|---|---|---|
| sync | `ibd-{mainnet,testnet}-net[-noav]` | `ibd_time_s`, `header_sync_s`, `blocks_per_s`, `cpu_ms_per_block`, peak/steady RSS, disk per component | real P2P network, txindex=1, dbcache=4096, par=auto, no wallet; `-noav` = `-assumevalid=0` | cold (empty datadir) | 1 run. Informational: the network is not controlled, so not gated. |
| sync | `ibd-{mainnet,testnet}-seed[-noav]` | same | as above, but the only peers are local dashd seeds serving a frozen chain; the node also gets `-maxtipage` so it can leave IBD at the frozen tip | cold | 1 run per round. **Gated:** `ibd_time_s` (2x), `header_sync_s`, `cpu_s`, `peak_rss_mib`, `disk_total_gib`, `disk_{blocks,chainstate,evodb,llmq,indexes}_gib`. Done = the seed's tip with the same hash, `initialblockdownload=false`, every enabled index synced. `--stop-height` runs stop via `-stopatheight` and are information only. |
| sync | `reindex-{mainnet,testnet}`, `reindex-chainstate-mainnet[-noav]`, `reindex-chainstate-testnet-noav` | `reindex_time_s` (gated, 2x), and the same footprint metrics as IBD. With `--bench-log 1`: `block_connect_ms_*` and `script_verify_txin_per_s` as information, and nothing gated. | reflink clone of the seed, no peers | cold | 1 run. Done = the source's tip hash (a mismatch is an error) and every enabled index synced. |
| sync | `spv-headers-{mainnet,testnet}` | `header_sync_s` | localhost seed with a block filter index (built once into the master seed). `--impl rust` runs **dash-spv** (dash-spv-bench, `--rust-bench-bin`) until the full Rust node exists. dash-spv is timed from spawn: the offset to its first log line plus its `block_headers_ms`. | cold | 5 · median |
| wallet | `rescan-mainnet` | `rescan_time_s` | watch-only descriptor wallet watching the coinbase payee of every 20,000th block | warm (1 untimed rescan first) | 2 · median |
| resources | `startup-regtest`, `startup-mainnet` | `startup_s` (spawn → RPC out of warmup), `startup_cold_s`, `shutdown_s`, `idle_rss_mib` | fixture chain / synced mainnet clone, no peers | warm (+ cold on mainnet with `--drop-caches`; not in concurrent pairs) | 10 / 5 after 1 warmup · median |
| mempool | `mempool-accept` | `tx_accept_latency_ms` (+p99 info), `tx_accept_tps`, `mempool_rss_mib` | regtest fixture, empty mempool per rep. Latency: 2,000 sequential sendrawtransaction. Throughput: 20,000 more txs from 8 client processes. | warm | 5 · median of per-rep medians |
| mining | `gbt-regtest` | `gbt_ms_mempool_{0,1000,5000,20000}`, `cnb_total_ms_*` | each sample adds one tx and waits 5.2 s, so dashd's template cache can't answer it | warm | 10 · median |
| mining | `gbt-mainnet` | `gbt_ms_mainnet`, `cnb_total_ms_mainnet` (information: the live mempool differs between runs) | synced mainnet clone on the live network, waits for tip + mnsync; before each sample a mempool tx is re-prioritised ±1 duff (bumps the update counter) and we wait 5.2 s | warm | 20 · median |
| rpc | `rpc-regtest`, `rpc-mainnet` | `rpc_<call>_ms` (+p99), `rpc_<call>_ops_c<N>` | txindex+addressindex, no peers. Fixed parameter sets (seed 42): blocks, txs, outpoints and addresses sampled from the chain. Calls: getblockchaininfo, getbestblockhash, getblockcount, getblockhash, getblockheader, getblock v0/1/2, getrawtransaction v0/1, gettxout, getmempoolinfo, getnetworkinfo, getaddress{balance,utxos,txids}, masternode count, masternodelist, protx list, quorum list (+ quorum info on mainnet). All are required. | warm (1 untimed pass) | 500 / 300 calls · median; throughput for six calls at `--clients` 16 and 64 × 5 s |
| validation | `microbench` | `micro_<bench_dash name>_ns` | curated bench_dash set: ECDSA/BLS verify and sign, script checks, block (de)serialisation, X11/SHA256, CCoinsCaching, mempool, AssembleBlock | warm | 5 processes · median of nanobench medians |
| masternode | `mn-{dkg,recsig,islock,chainlock,coinjoin}-regtest` | `dkg_round_s`, `recsig_ms`, `islock_ms`, `chainlock_ms`, `coinjoin_round_s` | **pending:** needs the Rust node to run as a masternode. The method is in `scenarios/masternode.py`. | | |

Note: `sendrawtransaction` latency and throughput live in `mempool-accept`, not in the RPC scenarios.

Other things to know about these metrics:
- **RSS, disk size and other footprint metrics** are gated as "never worse", but the 2x target does not apply to them.
- **Per-block latency and script-check throughput** come from the node's own instrumentation:
  - dashd: `-debug=bench` lines in `debug.log`
  - the Rust node: the JSONL bench log described below

  `script_verify_txin_per_s` only means something when scripts are actually checked: the `-noav` variants, or blocks above the assumevalid point.
- **Regtest is configured like mainnet.** `-checkmempool=0 -checkblockindex=0` disable regtest's default consistency checks. With them on, dashd runs a full mempool check after every accepted transaction: acceptance falls to about 8 tx/s at 6k mempool entries, which measures the check, not the node.
- **Client ceiling.** `rpc_ceiling_ops`, recorded with every mempool run, is the load generator's own rate on a trivial call. A note is added when throughput gets within 30% of it.

## Fairness methodology

### The same work under the same conditions

- **One configuration, verified.** Every implementation receives the same `NodeConfig` (`impls.py`), rendered as the same dashd-style flags: chain, indexes, dbcache, script threads, assumevalid, peers.
  - `run` refuses scenario options the scenario does not use, so a result never describes a setting that had no effect.
  - After startup, each node under test reports what it actually applied (`effective_config` in the result). Indexes and the mempool cap come over RPC: `getindexinfo`, plus the index RPCs for dashd v23, which does not list address/spent/timestamp indexes. dbcache (the sum of the "Cache configuration" lines), script threads and assumevalid come from the startup log of that invocation, because dashd has no RPC for them.
  - A value that differs from the request, or that cannot be verified, fails the run. `pair` additionally requires identical effective configurations on both sides. With `--par 0` (auto) the two implementations must therefore pick the same thread count, or the pair is an ERROR: pass an explicit `--par` when their auto rules differ.
- **Fixed CPU slices.**
  - The node under test runs on `--node-slice` (default `numa:0`: cores 0-15 *and* NUMA node 0's memory, via `numactl`).
  - Seeds, load generators and the harness itself run on `--aux-slice` (default `numa:1`).
  - Nothing the harness does competes with the node for its cores.
  - A slice can also be `CPUS@NODE`, e.g. `16-27@1`.
- **A controlled network.** Gated IBD uses local dashd seeds: same binary, frozen chain, over localhost, whitelisted so they never throttle. The network is identical for both implementations and is not the bottleneck. The `*-net` variants measure the real network and are reported for information only.
  - There are 4 seeds by default (`--seeds`). dashd keeps at most 16 blocks in flight per peer, so one localhost peer is latency-bound.
  - Measured on testnet, blocks 5k–60k: ~110 blocks/s from one seed, with both processes under one core; ~445 blocks/s from four. The real network with 10 peers gives ~670.
  - Each seed is its own reflink clone.
- **Identical inputs.**
  - Regtest scenarios sync the same fixture chain from a seed over P2P, so any implementation works: nobody reads dashd's LevelDB.
  - They then submit the same pre-signed transactions.
  - RPC parameter sets are sampled deterministically from the node's own chain.
- **Cold starts.**
  - IBD, reindex and spv runs start from empty storage.
  - `--drop-caches` also drops the page cache right before cold measurements, after all preparation. It is opt-in because it is host-wide and slows every other process on a shared box.
  - A drop that fails is an error (a "cold" result is never silently warm), and every drop is recorded (`page_cache_drops`).
  - In a concurrent pair, both sides meet at a barrier and side A drops once while side B waits. A second drop in the same run (repeated cold measurements: spv reps, `startup_cold_s`) is refused there; use `--mode interleave`.
- **Timing is the node's own where possible.**
  - IBD and reindex end when block *target* became the tip, read from the node's per-block log (dashd `UpdateTip` timestamps), not from polling. When indexes are enabled, they end when the last index reports synced, if that is later (polled).
  - Only this invocation's log entries count: dashd lines after the byte offset at spawn, and timestamps between spawn and now. Clones drop old `debug.log` and `bench.jsonl`.
  - Readiness is polled every 5 ms.
  - Shutdown is timed with a blocking `waitpid`.
  - cgroup cleanup happens after the clock stops.
- **Instrumentation is not measured against itself.**
  - `-debug=bench` costs dashd about 10 log lines per block. The Rust node's JSONL costs one.
  - So runs with `--bench-log 1` keep their timings as information only.
  - Self-reported per-block numbers (`block_connect_ms_*`, `script_verify_txin_per_s`) are never gated: each implementation instruments itself differently.
- **What is gated.** Derived metrics are information only: `blocks_per_s`, `cpu_ms_per_block`, p99s. So are metrics whose inputs differ between runs (`gbt-mainnet` on the live mempool), and `steady_rss_mib`, which is not yet the PRD's settled-tip RSS (see [PRD coverage](#prd-coverage-and-b1b)). Per-component disk sizes are gated next to the total, because components can trade against each other inside it.

### Shared, noisy hosts

agentbox is shared. Two long-running reference dashd nodes (`/work/chains`, owned by another task, never touched by node-bench), cargo builds and other test suites run next to the benchmark. Background load cannot be removed, so node-bench measures it, labels runs by it, and cancels it out by pairing.

1. **A load record on every result** (`doc["load"]`, plus the `noise:` line in every Markdown summary).
   - **Own processes.** Everything the run started carries `NODE_BENCH_MARK` in its environment: nodes, seeds, load workers and bench binaries. Both sides of a pair share one mark.
   - **Snapshots** at start and end:
     - the load average
     - CPU/IO/memory PSI
     - foreign processes by kind: dashd (with their datadirs), rust builds, C builds, test binaries, containers
     - the top CPU users
   - **Foreign load during the run**, sampled every 2 s:
     - **host foreign** = busy cores on the host − CPU of own processes
     - **slice foreign** = busy cores on the node slice − CPU of own processes pinned to it
     - Guest time is not double-counted.
   - Every interval also records CPU, IO and memory pressure (PSI some-avg10). The intervals and the measurement boundaries are stored with the result.
   - **Classes**, from the intervals of the measured work only (from the scenario's `start` barrier, so a quiet preparation phase cannot dilute a busy measurement), using p50/p90/max rather than means:
     - **quiet:** p90 slice foreign ≤ 10% of the slice and no interval above 30%, p90 host foreign ≤ 25%
     - **heavy:** median slice foreign > 30%, or median host foreign > 50%
     - **moderate:** everything else
   - PSI is host-wide, so during a measurement it includes the node under test and our own load generators: there it is recorded, not classified. It does decide admission to a quiet window (before any of our work runs): p90 cpu < 5%, io < 10%, memory < 5%. Foreign disk contention during a run therefore shows only in the stored intervals; the reserved window is what rules it out.
   - A result is **official** only if it ran with `--quiet-window`, in a manager-reserved window (`--reservation <id>`, DEC-23(b)), and its measured work classified quiet. Everything else is **provisional**. Results from before noise accounting existed have no load record and are provisional by definition.
   - The quiet thresholds tolerate small foreign load. That is why official wall-clock evidence also needs a reservation: the window, not the classifier, is what keeps other work off the box.
2. **Quiet-window mode for official baselines.**
   - `run ... --quiet-window[=0.10] [--quiet-timeout 21600]` waits before each scenario until a 30 s measurement is quiet.
   - If the host never gets quiet within the timeout, the scenario is refused and stored as status `refused-noisy`.
   - `pair` applies the same check before each round and records it in both sides' results.
3. **Paired comparisons for every verdict** (`run pair`). dashd (A) and the Rust node (B) always run under the same conditions, and only their *ratio* is judged:
   - **`--mode interleave`** (default): fresh processes in A B B A A B … order (ABBA) on the same slices, so drift cancels.
   - **`--mode concurrent`:**
     - A and B run at the same time on disjoint, equally sized cpusets.
     - Each side has its own seed/load-generator CPUs: A gets node `0-11@0` + aux `12-15`, B gets `16-27@1` + aux `28-31`.
     - Slices swap every round, so NUMA and core asymmetry cancel.
     - Both sides wait at a barrier before measuring.
     - With `--drop-caches`, side A drops the page cache once per round at the shared measurement barrier, after both sides are prepared; side B waits for it.
     - `--limits cpu=12,mem=24G,io=100` puts each node and bench binary in its own cgroup v2 group with identical `cpu.max` / `memory.max` (swap off) / `io.weight`. This needs sudo. `io.weight` only takes effect with the BFQ scheduler; agentbox's `/work` disk uses `none`, and the harness warns about this.
   - **Layout checks:** in concurrent mode the four slices must be pairwise disjoint, with node slices and aux slices of equal size; in interleave mode both sides use the same slices. CPUs that do not exist are refused, and the pinning method is recorded (`env.pinning`; without numactl memory is not bound).
   - **Rounds:** the default is 6, and the count must be even, so ABBA ordering and slice swaps balance.
   - **Per-round ratios:** for each metric and round, `speedup_r = A_r/B_r` (lower is better) or `B_r/A_r` (higher is better).
   - **Headline:** the geometric-mean speedup, with a 95% t-interval over the rounds' log-ratios (≥ 3 rounds). The report also shows per-round values and both sides' medians.
4. **A/A noise floor.** `run pair <scenario> --a dashd --b dashd` compares dashd with itself. The CI must contain 1, and its width shows the smallest regression the setup can currently resolve.

   Measured on 2026-10-08 under heavy load: 3 concurrent rounds gave `tx_accept_tps` 1.01× [0.86–1.19] and `startup_s` 1.08× [0.86–1.36]. So, on that day, about 15–25% resolution. Quiet windows and more rounds narrow it.
5. **The gate is PRD NFR-PERF-02(f)/(g).** It works on per-run values (one headline value per run or round), with A = dashd (or the previous integration head) and B = the candidate.
   - **Required inventory (profile `nfr-perf-02/b1-r2`).** Every scenario declares the gated metrics a comparison must observe (`required=` in `scenarios/*.py`; `compare.required_metrics`). The inventory is never inferred from what a run happened to record. A required metric, or a metric either side reports as gated, needs a valid value on both sides of every complete round.
     - The inventory always includes the one for the scenario's default parameters. Narrowing options cannot shrink it: a `--filter`, fewer `--mempool-sizes` or `--clients`, `--stop-height`, `--bench-log`, `--txindex 0`. The metrics they drop come out MISSING, or as an ERROR for a required metric recorded as information.
     - A scenario without an inventory (network IBD, `gbt-mainnet`, pending masternode scenarios) adds an `(inventory)` row: its other rows can never make a PASS.
   - **Row verdicts:**
     - **FAIL (slower):** B's median is worse than A's by ≥ 1% (`--effect`), *and* a one-sided Mann-Whitney test gives p < 0.05 (`--alpha`). The test is exact, with midranks, up to C(n+m, n) = 200,000 splits (6 vs 6 and 10 vs 10 are exact), and the normal approximation above that. Each row names the method used.
     - **MISSING:** a required or gated metric is absent on a side in some round, or required on both and absent on both. A Rust node that lacks a feature shows up here.
     - **ERROR:** a side errored, was interrupted or wrote no result; a value is invalid (not finite, or negative; zero is a value); the sides ran with different parameters, seed/source chains, timing methods, cold/warm state or effective configuration; or a re-analysis has neither `expect.json` nor a scenario list.
     - **INSUFFICIENT:** fewer runs than (f) requires: 5, or 4 for the long sync scenarios (DEC-23(a); with 3 vs 3 the minimum p is 0.05, so "slower" could never be shown).
     - **INCONCLUSIVE:** with `--strict` only. Not shown slower, but the 95% CI of the paired speedup reaches below 1 − 2% (`--margin`), or there is no CI.
     - **PASS**, or **2x** when the speedup is ≥ 2 on a metric where the target applies.
     - **CLEARED:** a FAIL that an independent confirmation showed is not a slowdown (see below).
   - **Overall gate**, most severe first:
     - **FAIL:** some row is FAIL.
     - **INCOMPLETE:** some row is ERROR, MISSING, INSUFFICIENT or INCONCLUSIVE, or no gated row exists. Partial coverage is never a pass.
     - **PROVISIONAL:** every required metric was observed and none is slower, but the evidence is not official: some run was not official (above), or `--effect`/`--alpha` were weaker than the PRD's. It is never merge or milestone evidence.
     - **PASS:** none of the above. `pair` exits 0 only for PASS.
   - **Ratios alongside the verdict.** Every row also reports the paired speedup (geometric mean of per-round ratios) with its 95% t-interval, the medians and p95s of both sides over the rounds, and p. The test, the medians and the CI all use exactly the same rounds.
   - **`--strict`** adds non-inferiority on the paired CI. It narrows, but does not close, the gap left by an under-powered test. With the 2% margin, a precisely measured 1.5% paired slowdown still passes, and Mann-Whitney over rounds ignores the pairing. The PRD rule is the unpaired test; PF0 counters and reserved windows are the program's answer to weak wall-clock power.
   - **Multiplicity and confirmation.** With dozens of gated metrics, about 5% of truly equal metrics come out "significant" by chance. So `pair` re-runs the scenarios that FAILed in an independent pair (`--no-confirm` skips this).
     - A FAIL is CLEARED only when the confirmation measured that metric cleanly, passed, and its paired 95% CI excludes a slowdown of the effect size (lower bound ≥ 1/(1+effect)). For a true slowdown of that size, that happens at most 2.5% of the time, so the confirmation keeps the first run's detection power. Mere non-significance never clears a FAIL.
     - Every confirmation row is added to the report, so a FAIL (or ERROR, MISSING) that only the confirmation found decides the gate.
     - The report is official only if the confirmation's runs are too.
   - **PR vs previous head** (A1 §3.11): `pair --a rust --a-bin <previous head> --b rust --b-bin <PR>` runs the same rule. It covers node scenarios only. Bench-binary scenarios (microbench, spv-headers) are paired by running them twice with different `--rust-bench-bin`.
   - **`compare` is indicative only.** It lines up separately recorded results: successful, non-paired runs per scenario from the newest run's binary (sha256) and parameters. A newer failed attempt stays visible as an ERROR. It applies the same rule and inventory, warns when hosts, slices, parameters, seed/source chains, cache state or load class differ, and is never official: PROVISIONAL at best.
6. **Long runs don't get lost.**
   - IBD and reindex write a `partial` checkpoint as soon as the core numbers exist.
   - Secondary extraction failures (disk sizes, log parsing) become notes. A required metric lost that way then blocks the gate as MISSING.
   - Poll RPCs retry transient timeouts.
   - `run recover <run_dir> [--keep-datadir PATH]` rebuilds an IBD result from `samples.csv` and the node's log if the harness failed after the node finished. The datadir must still be there: run long IBDs with `--keep-datadir` or `--keep-failed`.
7. **No process outlives its run.**
   - Every node and bench binary starts as a session and process-group leader. Every stop path, including timeouts and SIGTERM/SIGHUP to the harness, signals the group. It ends by killing and draining the whole group, wrapper children included, before the datadir or cgroup is removed. An atexit hook reaps leftovers, and `run` notes and kills any group that outlives its scenario.
   - With `--limits`, the cgroup exists before the child starts. The child moves itself in before exec'ing the wrapper or the node, so nothing it forks escapes the caps. Removal kills whatever is still inside, and a failed removal is noted in the result.
   - The group leader gets SIGTERM if the harness dies (`setpriv --pdeathsig`). If the harness is SIGKILLed while a *forking* wrapper (perf, strace) runs, the wrapper's child can survive: Linux offers no reaper for that case. Find and kill such a group by its process group id (`ps -eo pid,pgid,args`).

## Write boundary and retention

- node-bench writes, moves and deletes only under `NODE_BENCH_ROOT` (default `/work/bench`). It never reads from or writes to `/work/chains`, the reference oracles; `NODE_BENCH_PROTECTED` adds more protected trees, colon-separated.
- Paths are checked after resolving symlinks. That covers `--out`, `--datadir`, `--keep-datadir`, `--source-datadir`, seeds, the filter-index build, the workload caches and every cleanup. The root and its first two levels (`bin/`, `data/seed-*`, `results/`, ...) are never deleted.
- Clones refuse a source with a symlink that leaves the datadir, and empty `dash.conf`: every option comes from the command line. Clones for a node under test drop the seed masters' block filter index (built for spv-headers), so it is neither carried nor charged on disk.
- Per-component disk sizes follow the dashd version's layout: v23 keeps the address/spent/timestamp indexes in `blocks/index`, v24 in `indexes/`. A Rust `disk_parts` mapping must follow the layout of the dashd it is compared with.
- **Retention.** Once a scenario ends, the datadirs in its run directory are removed and the last 64 MiB of each node log is kept in `run_dir/logs/`. Kept:
  - the datadir `--keep-datadir` asked for;
  - after a failure, every datadir of a long scenario (so `recover` works), or of any scenario when `--keep-failed` or `--keep-datadir` was given.
- **Space.** A scenario refuses to start with less than `--min-free-gib` free: 200 GiB for long scenarios, 20 GiB otherwise. A clone on a filesystem without reflink support first checks that a full copy fits.
- Result, report and manifest files are strict JSON (no NaN), written atomically.

## Counter workloads (PF0)

The per-PR gate counts deterministic events instead of wall-clock time (task PF0: instructions, allocations, syscalls via `perf stat` / cachegrind). Wall-clock side-by-side runs (`pair`) remain for milestone reports, in reserved quiet windows (DEC-23(b)).

`run workload <name> [--impl dashd|rust] --wrap "<counter command> --" [--ctl-fifo CTL[,ACK]] [--json manifest.json]` is the entry point. It does not parse counters, measure noise floors or compare heads; that is PF0's.

1. It prepares fixed inputs without the wrapper. The regtest fixture chain is synced once per implementation binary and cached under `data/workloads/`: built under a lock, published by rename, newest three kept per implementation.
2. It reflink-clones those inputs for the run, and pins itself to the aux slice.
3. It runs the node under test *inside* `--wrap` for one fixed piece of work, with a fixed `-par` (default 1), and returns when the wrapper has exited and written its counters.
4. **Boundaries.** Markers go to `markers.jsonl` and the manifest: spawn, ready, begin/end around each piece of fixed work, stop, exit. Each marker has monotonic and wall time.
   - With `--ctl-fifo`, the harness writes `enable` at each begin and `disable` at each end to `perf stat -D -1 --control fifo:CTL[,ACK]`. The counters then cover only the fixed work, not startup, setup or shutdown.
   - Measured: `mempool-accept --txs 300` counted 268 M `instructions:u` this way.
   - The whole-lifetime workloads enable at spawn and disable at exit.
5. **Failure is loud.** The command exits nonzero and the manifest says `status: error` when:
   - the wrapper or the node exits nonzero (e.g. perf's 129 for a refused event);
   - a workload times out (`--timeout`);
   - a stop-at-height run did not connect exactly its target block. The hash is checked against a seed, or against the seed master's own log for the reindex.
6. **Manifest** (atomic, also in the run directory): workload, impl (binary sha256), harness commit, full argv, `NodeConfig`, effective config, exit code, endpoint, markers, operation count (`ops` and `unit`), and input digests (fixture txs sha256, RPC call-list sha256).

| workload | fixed work (inside the begin/end markers) | ops |
|---|---|---|
| `startup` | the whole lifetime: start on the fixture chain, RPC ready, `stop` | 1 |
| `mempool-accept` | `sendrawtransaction` of the first `--txs` (2000) fixture txs, one client | txs |
| `gbt` | each of `--gbt-calls` (3) `getblocktemplate` calls, after filling the mempool with `--gbt-mempool` (5000) txs and adding one tx + 5.2 s idle before each call (both outside the markers) | templates |
| `rpc --call <label>` | `--calls` (200) sequential calls from rpc-regtest's seed-42 parameter set; discovery of the set is outside | calls |
| `reindex-chainstate-testnet` | the whole lifetime: `-reindex-chainstate` of testnet blocks 0..`--height` (300000), no peers, ends via `-stopatheight` | blocks |
| `ibd-testnet-seed` | the whole lifetime: IBD of testnet 0..`--height` from `--seeds` (4) local seeds, ends via `-stopatheight` | blocks |

Known limits, PF0's to settle (documented here, not redesigned in B1):
- **Startup subtraction is not sound.** A plain startup run on the fixture datadir is not the startup/shutdown overhead of an indexed RPC node, an empty IBD node, a replay node, or a node with a full mempool. Subtracting independently noisy values also adds their variance. Use the markers with `--ctl-fifo` instead. For the whole-lifetime workloads, any subtraction needs a matched configuration and propagated uncertainty. It is meaningless for non-additive metrics such as peak RSS.
- **Timer-driven work.** dashd's background threads (scheduler, mempool/GBT timers, polling) run during the measured window in proportion to its wall time. Measured on agentbox (2026-10-08, heavily loaded): whole-lifetime `startup` user-space instructions varied about 30% (1.02 G vs 1.33 G), and `mempool-accept --txs 1000` repeated within 2%. Neither demonstrates repeatability at a 0.5% threshold: PF0 must measure its own noise floors (five runs) before gating.
- **Windows.** The replay workloads run blocks 0..`--height`, not the fixed 2,000-block historical windows of A1. A mid-chain window needs a snapshot at its start height, which PF0 has to prepare. `ibd-testnet-seed` with several seeds has a nondeterministic message order; `--seeds 1` fixes the peer order at the cost of speed.
- **Previous-head comparison and noise floors** (max(noise floor, 0.5%)) are not implemented here.
- **perf permissions.** `kernel.perf_event_paranoid=2` permits user-space hardware/software counters such as `instructions:u` and `page-faults`. Syscall tracepoints (`syscalls:*`) are refused, and perf exits 129, which the workload now reports as a failure. Count syscalls with `strace -c -f` as the wrapper instead.
- **Microbenchmarks** need no wrapper: run `bench_dash` / the Rust bench binary directly under the counter tool.

## Plugging in the Rust node

`--impl rust --rust-bin <daemon>` (or `NODE_BENCH_RUST_BIN`). The contract:

- **Flags:** the dashd-style flags `NodeConfig` emits.
  - `-datadir -server -daemon=0 -printtoconsole=0 -logtimemicros`
  - `-rpcuser -rpcpassword -rpcbind -rpcallowip -rpcport -rpcthreads -rpcworkqueue`
  - `-dbcache -par -txindex -testnet/-regtest -listen -bind -port`
  - `-connect` (`-connect=0` = no peers) `-dnsseed=0 -fixedseeds=0`
  - `-addressindex -spentindex -timestampindex -blockfilterindex -peerblockfilters`
  - `-assumevalid -disablewallet -debug=bench -reindex -reindex-chainstate -maxmempool -stopatheight`
  - regtest extras: `-maxtipage -persistmempool=0 -checkmempool=0 -checkblockindex=0`

  Flags with no meaning for the Rust node must be accepted and ignored. Anything mapped differently goes in `RustImpl.translate` (`impls.py`). Seeded IBD also passes `-maxtipage`, and bounded runs and workloads pass `-stopatheight` (stop cleanly, exit 0, at that height).
- **Effective configuration** (checked after every start, see Fairness):
  - indexes through `getindexinfo` with dashd's names (`txindex`, `addressindex`, `spentindex`, `timestampindex`, `basic block filter index`), each with `synced`;
  - the mempool cap through `getmempoolinfo.maxmempool`;
  - dashd's startup lines in `debug.log`: `Cache configuration:` followed by `* Using X MiB for ...` lines adding up to `-dbcache`; `Script verification uses N additional threads` (N = par − 1); and `Assuming ancestors of block <hash> have valid signatures.` or `Validating signatures for all blocks.`.
- **Disk layout:** per-component sizes are read from `blocks`, `chainstate`, `indexes`, `evodb` and `llmq` under the chain directory. A different layout maps its directories onto these names in `RustImpl.disk_parts`.
- **RPC:**
  - JSON-RPC 1.0 over HTTP with basic auth, including batches.
  - `-28` while warming up; `-32601` for unknown methods. Metrics for unknown methods are reported as unsupported or missing, not as failures.
  - Calls used:
    - chain: getblockcount, getblockchaininfo, getbestblockhash, getblockhash, getblock (0/1/2), getblockheader, getconnectioncount, getindexinfo, stop
    - transactions and mempool: getrawtransaction, gettxout, getmempoolinfo, getnetworkinfo, sendrawtransaction, getblocktemplate
    - address index: getaddressbalance, getaddressutxos, getaddresstxids
    - masternodes and quorums: masternode count, masternodelist, protx list, quorum list/info
    - wallet: createwallet, importdescriptors, getdescriptorinfo, rescanblockchain, getbalances, unloadwallet
- **Per-block log:** `<datadir>/bench.jsonl`, one object per connected block:

  ```json
  {"height": 123, "hash": "0000…", "ts": 1700000000.123456, "connect_ms": 1.9, "verify_txins": 42, "verify_ms": 0.8}
  ```

  `height`, `hash` and `ts` are required: endpoints are verified by hash.

  Alternatively, emit dashd-style `UpdateTip:` / `- Connect block:` lines in `debug.log` and set `NODE_BENCH_RUST_LOG_FORMAT=dashd`.
- **Microbenchmarks:** `--rust-bench-bin` must print nanobench-style JSON:

  ```json
  {"results": [{"name": "ECDSAVerify", "unit": "op", "median(elapsed)": 4.9e-05}]}
  ```

  Use bench_dash's names for the operations the Rust benches reproduce exactly (same inputs and the same unit of work), so rows line up.
- **dash-spv today:** `spv-headers-*` with `--impl rust` runs `target/release/dash-spv-bench` (`cargo build --release -p dash-spv-bench`), or whatever `--rust-bench-bin` points to. This is the Rust piece that can be compared now.

The seed and the fixture generator are always the pinned dashd: they are test infrastructure, not the node under test.

## Recording results

1. **Per PR** (DEC-23(b)): PF0's counter gate, built on `run workload`. Wall-clock pairs on a shared host are PROVISIONAL and never merge evidence.
2. **Milestones** (M3 onwards): `pair` in a manager-reserved quiet window. Long scenarios need ≥ 4 rounds (DEC-23(a)):

   ```bash
   node-bench/run pair quick microbench --rust-bin target/release/<node> --rust-bench-bin <benches> \
       --quiet-window --reservation <window-id> --tag m3
   node-bench/run pair ibd-mainnet-seed reindex-chainstate-mainnet --rounds 4 --quiet-window --reservation <window-id> --tag m3
   ```
3. `results/pairs/<pair_id>/report.md` goes into the milestone report. Only gate PASS is a pass. FAIL, INCOMPLETE and PROVISIONAL are not.
4. Every comparison appends one line to `results/ledger.jsonl`: tag, gate, official, profile, gate options, counts, harness commit, load classes and the report path. Raw results stay under `results/`, each linked by its `run_id`.
5. **Baselines.** `scripts/baseline.sh [--quiet --reservation <id>] [--runs N]` records dashd's numbers per scenario. It is resumable: a step's marker holds its profile and result files, and a step is redone when the profile changes. With `--quiet`, a step is only marked done when its result came out official. A baseline is a record, not a comparison: one run per step by default. Refresh it when the pinned dashd changes or the harness changes a definition. `summary --impl dashd --md …` produces the table for the research notes.

## PRD coverage and B1b

The scenarios cover part of the PRD's tracked metrics (NFR-PERF-01/02, metrics 1–18) exactly, and part of them only approximately. A PASS covers exactly the required inventory of the scenarios it ran, never the unrestricted contract. Per DEC-23(d), the gaps below are task **B1b**, due before the first Rust-vs-dashd comparison (M3). Pending masternode scenarios are a staged limitation until the Rust node can be a masternode; they do not fulfil metrics 14–18.

| PRD metric | Covered now | Gap (B1b unless noted) |
|---|---|---|
| 1, 2: IBD to tip, default / `assumevalid=0` | seeded IBD to the seed's tip: hash checked, out of IBD, indexes synced | network runs are information (uncontrolled); 4-seed throughput is below the real network's, and seed saturation is not checked |
| 3: reindex / reindex-chainstate | mainnet and testnet variants, tip hash and indexes enforced | a Rust import path from a Core-format clone is untested end to end |
| 4: rescan, wallet ≥ 100,000 txs | `rescan-mainnet` watches coinbase payees every 20,000 blocks (warm) | a real ≥ 100k-tx wallet, asserted |
| 5: ConnectBlock + flush p50/p95, last 50,000 mainnet blocks and live tip | own-log connect times, information (median/mean/p99) | p95 rows, the last-50k window, a live-tip scenario, gating |
| 6: ECDSA/BLS checks/s, fixed corpora | bench_dash per-unit times, required by name | corpus/unit equality with the Rust benches is a manual contract; no input hash |
| 7: replayed mainnet tx stream, rate and p50/p95 | regtest P2PKH fixture into an empty mempool; median latency and tps gated | mainnet replay corpus; p95 gated |
| 8: GBT p50/p95 at typical and full 300 MB mempool | 0/1k/5k/20k fixture txs; `gbt-mainnet` (information) | full 300 MB fixture; p95 gated |
| 9: synced mainnet startup with/without txindex/addressindex | `startup-mainnet` (txindex; cold with `--drop-caches`) | the with/without index variants (addressindex is refused today, not ignored) |
| 10: IBD peak RSS, tip steady RSS, node/MN/MN-all-indexes | peak RSS gated; idle RSS at startup | settled-tip RSS; MN modes |
| 11: disk per blocks+undo, chainstate, MN/LLMQ, each index | total and per directory gated | each index separately |
| 12: CPU per block at tip, MN mode | whole-run CPU gated; CPU/block information | tip/MN workload |
| 13: RPC mix at concurrency 1/16/64 | per-call latency (c1), throughput at c16/c64 for six calls | concurrent latency, throughput for every call, a fixed mixed schedule, heavy addresses |
| 14–18: DKG, recsig, ISDLOCK, CLSIG, CoinJoin | pending (draft methods in `scenarios/masternode.py`) | end points per the PRD (masternode milestone) |
| NFR-PERF-02(f): CIs for median and p95 | paired ratio CI, medians and p95s over rounds | per-side median/p95 CIs; per-call p95 rather than p95 of per-run medians |

## Layout

```
run                    entry point
nodebench/cli.py       commands: run, pair, compare, summary, recover, list, env, fetch-dashd, workload
nodebench/impls.py     NodeConfig, dashd/Rust implementations, effective config, Node lifecycle, process groups
nodebench/paths.py     NODE_BENCH_ROOT, the write boundary (/work/chains protected), the pinned dashd
nodebench/workloads.py fixed-input counter workloads and their measurement boundaries (PF0)
nodebench/proc.py      pinning (numactl/taskset), /proc sampler, footprint
nodebench/noise.py     foreign-load accounting, snapshots, quiet window
nodebench/limits.py    cgroup v2 caps
nodebench/pair.py      paired runs + analysis + ledger
nodebench/compare.py   unpaired comparison, the non-inferiority rule
nodebench/load.py      multi-process RPC load generator
nodebench/fixture.py   regtest fixture (chain + signed txs), seed helpers
nodebench/logparse.py  dashd debug.log / JSONL per-block parsing
nodebench/scenarios/   one module per scenario group; common.py has seeds, clones, tracking
scripts/               build-dashd-bench.sh, baseline.sh
tests/                 unit tests (no node needed)
```

## Relation to dash-spv-bench

`dash-spv-bench/` (docker peers, `tc netem` link shaping, wallet scenarios) stays the tool for SPV client tuning. node-bench reuses its client binary for `spv-headers-*`, and its local-peer idea for the seeds. It doesn't use docker: native processes with cpusets and cgroups make CPU and memory accounting exact.
