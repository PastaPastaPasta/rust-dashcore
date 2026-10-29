# B1 review, round 1 — GPT

**Verdict: CHANGES-REQUIRED**

Reviewed `node/bench-harness` at `288c2a78c4e64df79120f92feee4a8d6b44f55ea`, in `/work/home/agent/workspace/rdc-wt/bench-harness`, against `git diff node/integration...HEAD` (12 commits; 34 new files, 5,160 lines). The integration ref at review time was `cc23d55a63d77c49bb14246c38de69cc6e3dceb8`. References: current PRD NFR-PERF-01/02 and metrics 1–18, A1 §3.11, and research/B1-dashd-baseline.md.

The statistical primitive is sound for the intended default sample sizes. The merge decision is not trustworthy yet: it can approve missing tracked measurements, ignore a confirmed significant regression, accept provisional measurements, or report a failed counter workload as successful. The counter entry point also does not yet provide the deterministic measurement boundaries PF0 needs. This is useful baseline-collection infrastructure, but cannot currently substantiate a merge approval under the program rule.

No branch files were changed and no commits were made. No real benchmarks, dashd instances, or operations on `/work/chains` were started by this review. Temporary process tests used Python sleeping children and explicitly killed the surviving child.

## Validation

- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s node-bench/tests -v`: **22 tests passed**, about 1.9 seconds.
- `PYTHONDONTWRITEBYTECODE=1 uvx ruff check node-bench`: **passed**.
- `bash -n node-bench/scripts/baseline.sh node-bench/scripts/build-dashd-bench.sh`: **passed**.
- `git diff --check node/integration...HEAD`: **passed**.
- Independent direct pairwise-U enumeration agreed with the exact Mann–Whitney implementation on **1,566** small tied/untied cases (group sizes 1/2, 2/2, 3/3 and 2/4; pooled values from {0,1,2}).
- Temporary JSON and mocked-workload checks reproduced the missing-metric/provisional PASS, discarded confirmation FAIL, NaN PASS, empty-index acceptance, mean-only noise classification, and nonzero-wrapper-success issues below. A real, short Python process-lifecycle check reproduced a surviving wrapped child.

## Findings

### F1 — BLOCKER: required metric coverage fails open

**Locations:** `node-bench/nodebench/compare.py:44`, `compare.py:89`; `node-bench/nodebench/pair.py:294`; `node-bench/nodebench/scenarios/rpc.py:89`; `node-bench/nodebench/scenarios/common.py:245`.

**Scenario:** Candidate B returns method-not-found for an RPC A measures. The RPC scenario records an unsupported-method note and still finishes `ok`. Pair analysis emits MISSING, but MISSING is outside both GATED and BLOCKING. If one other metric passes, the whole pair passes. Reproduced with six complete rounds: A has `present` and `missing`, B has only `present`; verdict **PASS, n_missing=1**. Furthermore, the inventory is taken only from the first A run, and a first-run `value=None` is skipped. A metric missing on both sides, appearing only in later rounds, or lost during best-effort resource extraction can disappear without any blocking row. `record_resources` failures become notes even though they can lose CPU, peak RSS and disk measurements.

This is particularly consequential because several PRD quantities have deliberately been turned into information (F8). A positive result on some available metrics is not proof of the tracked contract.

**Fix:** Define a versioned required metric/configuration inventory for each gate profile and require complete, valid observations in every required round. Missing/unsupported metrics and failed extraction of required measurements must block. During staged node implementation, use an explicitly limited profile with declared applicability, rather than allowing a partial profile to claim an unrestricted merge PASS. Never infer the required inventory solely from one successful result document.

### F2 — MAJOR: confirmation reduces detection power and discards new regressions

**Locations:** `node-bench/nodebench/pair.py:110`, `pair.py:123`, especially `pair.py:133` and `pair.py:136`.

**Scenario:** A metric satisfies the actual PRD slowdown rule, but its independent confirmation has p ≥ 0.05. Default behavior erases the first FAIL and declares PASS. Non-significance on a second small sample is not evidence that the first regression was false. If a real regression is detected with power q in each independent run, requiring both to fail reduces detection to approximately q²: at q=0.5, the false-negative probability grows from 50% to 75%. Even q=0.8 falls to 64% detection. The composite rule is not NFR-PERF-02(g), which blocks on a qualifying slowdown; it is an undocumented change to the acceptance rule.

There is also a concrete information-loss bug independent of that statistical policy. Initial report: `t=FAIL, u=PASS`. Confirmation: `t=PASS, u=FAIL`. `settle_confirmation` converts t to PASS and imports only ERROR rows, silently discarding u's new FAIL. Reproduced final result: **PASS with both t and u reported PASS**. INCONCLUSIVE/INSUFFICIENT/MISSING on other confirmation metrics are also not carried forward. The displayed first-run p-value and values remain attached to a row relabelled PASS; the row's note is not rendered by `fmt_row`.

**Fix:** Preserve every blocking result from both rounds and the full evidence for each. Do not erase a valid FAIL solely because another test is non-significant. If the program wants a sequential/multiplicity policy, predefine and approve it, with measured power and an inconclusive outcome; alternatively combine prespecified additional samples in a valid analysis. At minimum, `--no-confirm` must be the compliant default until this is settled.

### F3 — MAJOR: noisy/provisional wall-clock evidence can authorize a merge

**Locations:** `node-bench/nodebench/pair.py:287`, `pair.py:318`; `node-bench/nodebench/noise.py:223`, `noise.py:240`; `node-bench/nodebench/cli.py:274`.

**Scenario:** Pair analysis reads load classes but never requires `load.official`, quiet-window admission, or quiet operation. Heavy load only adds a warning. Twelve heavy, explicitly unofficial documents in a six-round pair still returned **PASS** in the synthetic check. Moderate and unknown runs do not even receive the pair's heavy-load warning. This bypasses A1 §3.11's reserved-window contract for milestone/release wall-clock evidence.

The assertion that a run “stayed quiet throughout” is also unsupported by the stored classification. It uses whole-scenario means, including preparation, rather than per-interval bounds over measured work. Nineteen idle intervals plus one interval with all 12 slice CPUs foreign-busy and CPU PSI 60% classify **quiet** (slice mean 5%, PSI mean 3%). Such a run can be labelled official. IO/memory PSI are only start/end snapshots; sustained shared-disk contention can be missed entirely. Quiet thresholds permit other work even when CPU pressure is low, so they do not by themselves prove NFR-PERF-02(a)'s “nothing else running.”

**Fix:** Distinguish exploratory results from a wall-clock acceptance profile. The latter must require approved reservation/dedicated-host evidence, valid load telemetry, and absence of disqualifying noise during each actual measurement. Store measurement boundaries and interval data, including disk/memory pressure and slice peaks; invalidate interrupted/noisy intervals rather than averaging them away. Shared-box exploratory PASS must not be eligible merge evidence; per-PR shared-box approval belongs to validated PF0 counters.

### F4 — MAJOR: PF0 work is not isolated or deterministic; startup subtraction is unsound

**Locations:** `node-bench/nodebench/workloads.py:12`, `workloads.py:50`, `workloads.py:70`, `workloads.py:85`, `workloads.py:113`; `node-bench/nodebench/cli.py:150`; `node-bench/nodebench/fixture.py:155`.

**Scenario:** Every counter workload wraps the entire daemon lifetime. Regtest workloads inherit `par=0` (auto), and the replay/IBD constructors also omit par, rather than using the specified `-par=1`. The workload CLI cannot set par. Its harness is not moved to the aux CPUs as `cmd_run` is, so sequential clients/preparation can compete on the node slice. GBT includes transaction submission and 5.2-second timer periods; RPC includes discovery of all fixture blocks/transactions and quorum metadata inside the measured lifetime. Replay is 0..300,000 testnet by default, not the fixed 2,000-block historical windows in A1. Multi-seed IBD adds nondeterministic message ordering.

A startup run on an unmodified regtest datadir is not the same overhead as startup/shutdown of an indexed RPC node, an empty IBD node, a replay node, or a node with a dirty mempool/cache. Timer/polling work depends on elapsed time and load. The existing baseline itself reports 1.02 G vs 1.33 G startup instructions and 2% mempool variation; that does not support a 0.5% guard. Subtracting independently noisy startup values can amplify uncertainty. GBT's suggested mempool subtraction even uses a different transaction subset: GBT uses its initial fill plus transactions from the end of the fixture, while mempool-accept uses the first N transactions.

The wrapper entry point does not parse/store counters, measure five-run noise floors, enforce max(noise floor, 0.5%), or compare against the previous integration head. This may be PF0's separate implementation scope, but the B1 README's merge-gate guidance must not imply that this entry point already fulfills that contract.

**Fix:** Provide explicit deterministic measurement begin/end hooks around the fixed work, with setup and shutdown outside the counter boundary; use fixed thread counts/configuration and pin the controller to aux CPUs. Prepare immutable hashed transaction/RPC/replay corpora before measurement. Implement or integrate PF0's five-run noise-floor baseline and previous-head comparison, including the required historical windows/header replay. Treat whole-lifetime counters as exploratory until repeatability is demonstrated at the actual threshold. If any subtraction remains, use matched configurations/work and propagate its uncertainty; subtraction is not meaningful for non-additive metrics such as peak RSS.

### F5 — MAJOR: a failed counter wrapper or incomplete workload can be reported successful

**Locations:** `node-bench/nodebench/workloads.py:100`, `workloads.py:118`, `workloads.py:127`, `workloads.py:169`; `node-bench/nodebench/impls.py:226`.

**Scenario:** `stop_at` returns the process exit code; replay and IBD put it in `node_exit`; `cmd_workload` unconditionally sets `status=ok` and returns 0. A `perf` permissions failure (129), a daemon abort, or any immediate early exit can therefore look like a successful a.height-block workload. It never verifies the achieved target height/hash. There is no deadline if `-stopatheight` is ignored or the target cannot be reached. Regtest workloads do not retain/check final process or wrapper exit codes at all, so a wrapper that fails while flushing its counters can also be missed. Most manifests lack the promised full wrapped argv/configuration, exit code and complete input digest.

Reproduced with a mocked workload returning `node_exit=129`: **CLI exit 0, manifest status ok**.

**Fix:** Require successful wrapper/daemon completion and independently validate the achieved fixed-work endpoint before assigning ops or status. Bound execution and abort hung workloads. Persist counters, full argv/configuration, hashes and exit codes in an atomic manifest. Any unsupported counter, missing counter output, nonzero exit, or incomplete workload must return nonzero and block PF0 ingestion.

### F6 — MAJOR: cold-cache labels can be false, and effective fairness is not checked

**Locations:** `node-bench/nodebench/pair.py:102`, `pair.py:170`; `node-bench/nodebench/scenarios/common.py:23`; `node-bench/nodebench/scenarios/startup.py:66`; `node-bench/nodebench/fixture.py:155`; `node-bench/nodebench/impls.py:55`, `impls.py:61`.

**Scenario:** Concurrent mode ignores the cache-drop command's status and always sets `NODE_BENCH_CACHES_DROPPED=1`. Both sides then record a successful drop even if sudo/write failed. It drops before setup, clone/index/seed preparation and the start barrier, not immediately at the common measurement boundary. Repeated SPV measurements cannot drop again in concurrent mode but continue to record the inherited drop. Interleaved drop failure is recorded as false but still does not disqualify a requested cold gate.

CPU slice defaults are reasonable, but custom concurrent slices are never checked for disjointness, equal capacity or overlap with the aux/controller CPUs; interleaved users can select unequal node slices. `par=0` does not guarantee equal actual validation thread counts across Core and Rust (A1 explicitly allows a higher Rust cap). `maxmempool_mib=0` relies on each implementation's default, with no CLI override. Many forwarded options are accepted but ignored: for example startup-mainnet ignores addressindex and par; fixture-based scenarios ignore supplied dbcache/par/txindex. The report can therefore describe parameters that the actual NodeConfig did not use. Pair analysis does not compare effective configurations, input hashes, units or endpoints before treating documents as comparable. A single process barrier per scenario also does not keep later repetitions aligned in concurrent mode.

**Fix:** Validate the entire CPU/aux layout before execution and verify effective affinity/NUMA/caps. Resolve explicit equal thread, memory/mempool and index budgets in every scenario, reject irrelevant options, and persist the actual effective configuration. Compare it across all pairs. A required cache drop must fail closed and happen at a coordinated measurement boundary after preparation; report each repetition's actual warm/cold state. Use per-repetition alignment or explicitly limit concurrent-mode claims.

### F7 — MAJOR: “completed the same work” is not enforced for sync/index measurements

**Locations:** `node-bench/nodebench/scenarios/ibd.py:79`; `node-bench/nodebench/scenarios/reindex.py:58`, `reindex.py:79`; `node-bench/nodebench/scenarios/common.py:186`; `node-bench/nodebench/logparse.py:88`.

**Scenario:** Seeded IBD stops on height alone, without requiring the reference hash, `initialblockdownload=false`, or all configured indexes to be synced. Its duration is the tip-update log timestamp, which can precede those completion conditions. A truncated `--stop-height` run can process extra blocks before polling stops it, but resources are divided/reported against the target rather than identical actual work. Reindex notices a wrong target hash but only adds a note; `indexes_synced(..., required=True)` accepts `{}` and accepts index entries lacking `synced` (default True). The synthetic empty-index check returned True. A node doing less validation/index work can appear faster and smaller.

Precision timestamps are trusted without checking that they fall within this invocation. `clone_datadir` excludes debug.log but not the Rust bench.jsonl, and `update_tip` takes the first matching historical entry. A Rust source datadir containing its prior replay log can produce a stale/negative elapsed time on a new replay.

**Fix:** Specify and verify scenario endpoints: fixed height AND hash/state, IBD readiness for full-tip scenarios, and the exact named enabled indexes at the target. Any mismatch is an ERROR. Freeze bounded runs at an exact workload boundary for resource comparisons. Clear every implementation's instrumentation log or parse only the new invocation's offset/run identifier, and validate timestamps against the harness clock/window.

### F8 — MAJOR: the tracked PRD contract is materially incomplete

**Locations:** `node-bench/nodebench/scenarios/common.py:202`, `common.py:219`; `node-bench/nodebench/logparse.py:129`; `node-bench/nodebench/scenarios/reindex.py:110`; `node-bench/nodebench/scenarios/mempool.py:44`; `node-bench/nodebench/scenarios/rpc.py:21`; `node-bench/nodebench/scenarios/masternode.py:26`.

**Scenario:** Independent required quantities such as steady RSS and component disk usage are classified as derived/information. They are not determined by peak RSS or aggregate disk: a Rust node can increase chainstate/index size while shrinking block storage and still pass total disk; or increase tip RSS while decreasing peak IBD RSS. Connect latency is not gated at all. Other required scenarios are missing or materially smaller than their definitions. See the metric-by-metric table below.

**Fix:** Keep informational exploratory metrics, but implement and require the PRD scenarios/quantities in the relevant acceptance profiles. Gate each required disk component, tip/mode memory quantity, and connect-latency statistic. Where a feature is not applicable until a later milestone, state that explicitly in a scope-bound profile and prevent an unrestricted performance claim. Do not silently redefine the charter's metric list to reduce multiplicity.

### F9 — MAJOR: wrapped process cleanup can orphan the actual node; caps have a fork race

**Locations:** `node-bench/nodebench/impls.py:193`, `impls.py:201`, `impls.py:246`, `impls.py:269`; `node-bench/nodebench/limits.py:84`; `node-bench/nodebench/load.py:65`.

**Scenario:** With a forking wrapper (perf/strace), `self.proc` and the parent-death signal refer to the wrapper, not necessarily the daemon. `kill()`/timeout escalation signal only that PID despite creating a new session. Killing the wrapper need not kill the node. A short real check with a Python wrapper and sleeping child confirmed **the child survives Node.kill()**; the review explicitly killed it afterward. In `stop_at` cancellation this can leave an actual dashd working on a datadir that surrounding cleanup subsequently removes. Load workers similarly lack an encompassing finally to terminate/join all children on interruption.

Cgroup creation/attachment occurs after spawn and involves sudo calls. The wrapper or daemon can fork children before the parent is moved; already-forked children are not retroactively moved by writing only the parent PID to cgroup.procs. Direct startup work is also outside the cap. Thus identical configured limits do not prove identical applied limits. Failed cgroup creation/attach may leave a group, and removal failures are hidden.

**Fix:** Create/enter the cgroup before executing/forking the workload, and own a dedicated process group/cgroup for the entire wrapper tree. Terminate and reap that tree, verify it is empty, then delete its datadir and cgroup. Preserve graceful RPC shutdown first, but make every exceptional path tree-safe, including controller SIGTERM/HUP and load-worker interruption. Treat absent parent-death support explicitly instead of claiming unconditional orphan prevention.

### F10 — MAJOR: the `/work/chains` noninterference rule has no path boundary

**Locations:** `node-bench/nodebench/paths.py:6`; `node-bench/nodebench/impls.py:307`; `node-bench/nodebench/scenarios/common.py:272`; `node-bench/nodebench/scenarios/spv.py:34`; `node-bench/nodebench/cli.py:46`.

**Scenario:** Normal defaults are under `/work/bench`, and seeds usually run on clones; that is good. However arbitrary root/out/datadir/keep paths are accepted, and clone/delete/move operations do not enforce an owned scratch boundary or protect canonical `/work/chains` paths. `cp -a` preserves directory symlinks and clone_datadir preserves dash.conf, so a copied datadir can still redirect block/wallet writes to an external reference location. `ensure_filter_index` explicitly writes a lock, index and marker into its master seed path. A misconfigured root/output path or seed symlink can therefore reach protected oracle state. The code contains no hard `/work/chains` exclusion, despite the documentation's absolute promise.

**Fix:** Centralize canonical-path validation before every write/move/removal, reserve `/work/chains` as protected, and require an ownership marker beneath the bench scratch root for destructive cleanup. Refuse external writable directory symlinks/config paths in cloned datadirs, or sanitize them into the clone. Validate filter-index destinations and custom output/keep paths too. Add temporary-tree tests for symlink/path escapes; never test against real oracle directories.

### F11 — MEDIUM: numeric validation can turn invalid measurements into PASS

**Locations:** `node-bench/nodebench/compare.py:65`; `node-bench/nodebench/pair.py:249`, `pair.py:301`; `node-bench/nodebench/results.py:48`.

**Scenario:** Numeric acceptance only checks int/float, so NaN/infinity/negative timings can reach median and Mann–Whitney. Directly evaluating six baseline ones against six candidate NaNs returned **PASS** (`p=1.0`, slowdown NaN). Python JSON permits these nonstandard values by default. Pair ratios discard nonpositive/NaN values while the test arrays can retain invalid data, so its CI can use fewer/different observations than the gate's n. Zero is treated as absence by truthiness checks; a real zero count is not the same as missing data.

**Fix:** Validate finite values, metric-specific domains and consistent samples before analysis. Reject invalid measurements as ERROR and serialize with `allow_nan=False`. Check None/missing explicitly, support legitimate zero counters, and require the same valid rounds for CIs, summaries and tests.

### F12 — MEDIUM: successful/failed runs and caches have unbounded disk retention

**Locations:** `node-bench/nodebench/scenarios/startup.py:52`; `node-bench/nodebench/scenarios/mempool.py:48`; `node-bench/nodebench/scenarios/rpc.py:123`; `node-bench/nodebench/scenarios/ibd.py:95`; `node-bench/nodebench/workloads.py:36`; `node-bench/nodebench/impls.py:313`.

**Scenario:** Successful fixture-based startup/RPC/mempool/GBT scenarios leave their datadir in the run directory; there is no corresponding context-managed cleanup. Failed/interrupted IBD skips keep_or_remove and retains its potentially huge datadir. Prepared workload caches accumulate one datadir per binary hash, are built without a lock/atomic publish, and are never pruned. `cp --reflink=auto` may fall back to a full copy; there is no free-space/quota preflight. Repeated gates, confirmations, binary revisions and interrupted long runs can exhaust the shared `/work` filesystem. Concurrent first PF0 runs for the same binary can delete/rebuild each other's cache before `.ready` exists.

**Fix:** Use owned cleanup contexts for successful runs, explicit retention policy for failures, and bounded/prunable caches. Lock cache construction and publish atomically with content/config hashes. Preflight actual copy-on-write support and free-space needs, and stop before shared storage is exhausted. Preserve only the evidence needed for failed-run recovery.

### F13 — MEDIUM: resumability can silently reuse stale/provisional baselines

**Locations:** `node-bench/scripts/baseline.sh:44`; `node-bench/nodebench/compare.py:118`; `node-bench/nodebench/results.py:83`; `node-bench/nodebench/pair.py:88`.

**Scenario:** `.done` is keyed only by tag/step and is not tied to harness definition, binary/input/config hashes, output status, or official/provisional quality. Re-running a provisional queue with `--quiet` and the same tag skips every completed step. The baseline queue runs each scenario only once, so its completion marker also does not establish the four/five independent-run minimum. `runs_by_scenario` discards newer failures whenever older successful runs exist and groups them without implementation-binary hashes; stale successes can hide the most recent failure or mix different heads. Checkpoint/result JSON writes are not atomic. `pair --analyze` without explicit scenarios has no persisted expected-run manifest, so an entirely missing scenario cannot be detected on reanalysis.

**Fix:** Make resumable work state refer to immutable result IDs and their complete provenance/quality, invalidate markers when the requested profile changes, and generate the required repetitions. Preserve the latest attempted failure. Group comparisons by binary/harness/input/config identities. Atomically publish JSON and save the pair's expected scenarios/rounds before starting, so interrupted reanalysis remains fail-closed.

## Statistics assessment

- **Mann–Whitney implementation:** `stats.py:77` is correct for finite valid values. Exact enumeration chooses labelled index subsets of pooled midranks, retaining multiplicity for equal ranks; ties are handled as an exact conditional permutation test. The independent pairwise-U check above supports this. At the 200,000-combination cutoff, 6/6 defaults are exact (924 allocations), and 10/10 is still exact (184,756); 11/11 switches to the normal approximation. Its tie variance correction, greater-tail direction, and continuity subtraction of 0.5 are standard. Degenerate all-equal samples return 1. No statistical arithmetic blocker was found. Document which method was used per row; do not describe all possible runs as exact. Highly tied/imbalanced larger samples can need an exact dynamic-programming/permutation alternative rather than relying on asymptotic calibration.
- **Effect threshold:** The default ≥1% relative worsening plus p<0.05 follows the operational PRD rule for lower/higher metrics. A <1% change is intentionally outside that rule; this is not a harness bug. Nonfinite/domain validation is F11. Overrides of alpha/effect/margin need to be recorded in gate provenance and constrained in an official profile.
- **Run counts:** Gate analysis uses independent run/round headline values, not thousands of RPC calls as pseudoreplicates. This is good. Four long-scenario/five other-scenario minimums and six default rounds match the **current** PRD, which already says ≥4 for long scenarios (DEC-23). The research note and README still mention the former three-run conflict. Three vs three indeed has minimum one-sided exact p=0.05. A mere combinatorial possibility of rejecting at four/five runs is not a power calculation; the reported shared-box 15–25% noise floor does not establish sensitivity to a 1% effect. Odd rounds only warn and leave ordering/slices unbalanced.
- **Pairing and “strict”:** Mann–Whitney ignores paired drift, as the baseline admits. A concrete example A=[100,200,300,400,500,600], B=1.015*A returns p=0.349567 and **PASS even with strict**: the paired speedup CI is exactly [0.9852217,0.9852217], above the default 0.98 bound. Thus the README statement that strict “closes” the underpower gap is too strong: it knowingly permits a precisely measured 1.5% paired slowdown, and targets a different estimand from the median rule. The 2% speedup margin also corresponds to about 2.04% increased cost for lower-is-better metrics. This example alone conforms to the literal unpaired PRD test; changing that policy needs an explicit program decision, not an accusation that the U implementation is wrong. PF0 and reserved windows are the approved ways to avoid shared-box acceptance on weak wall-clock evidence.
- **Confidence intervals:** The paired log-ratio geometric mean with a t interval is implemented sensibly, assuming independent approximately normal log ratios. Small-sample quantiles are conservative rounded values; df>30 always uses 2.04 rather than approaching 1.96, also conservative. The unpaired bootstrap independently resamples run values, appropriate for an unpaired exploratory ratio. Neither is a CI for each side's median and p95 as required by NFR-PERF-02(f). Pair reports calculate p95 over the per-run headline medians, not the per-call latency p95 required in several scenarios. Raw within-run descriptive p95 values exist but are not separately evaluated/gated, and no p95 CI is produced. Add explicitly labelled median/p95 estimates and CIs at the correct level, with adequate repetitions; retain ratio CIs as additional evidence. A significance test does not itself supply those CIs.
- **Re-run-once:** F2 is a substantive acceptance-policy regression, not a correction to Mann–Whitney. The family false-positive problem is real, but resolving it by requiring repeated rejection changes power and does not justify losing failures found on confirmation.

## PRD metric definition/coverage mismatches

| PRD metric | Current harness difference | Relevant code |
|---|---|---|
| 1 — IBD to tip, default assumevalid | Seeded run ends/times height connection, rather than IBD=false at the reference hash with all configured work complete; bounded overshoot possible. Local four-seed setup is useful and network runs are correctly informational. | `scenarios/ibd.py:79`, `common.py:186` |
| 2 — IBD assumevalid=0 | Same endpoint issue; explicit noav flags are present. | `scenarios/ibd.py:29`, `ibd.py:79` |
| 3 — reindex/reindex-chainstate | Mainnet variants exist, but hash mismatch only notes and required index set is not checked. For Rust, a shared Core-format clone requires a supported import/setup path; that assumption is not yet tested end to end. | `scenarios/reindex.py:29`, `reindex.py:58`, `reindex.py:79` |
| 4 — rescan wallet with ≥100,000 transactions | Watches sparse coinbase payees sampled every 20,000 blocks; does not establish a ≥100,000-transaction wallet or record/assert that criterion. All measured rescans are warm after a warmup. | `scenarios/reindex.py:110` |
| 5 — ConnectBlock+flush p50/p95, last 50,000 mainnet blocks and live tip | Own-log ConnectTip values for whole replay by default, optional lower-height filter; stored headline statistics are median/mean/p99, no dedicated p95 row or live-tip scenario; all informational. | `logparse.py:105`, `logparse.py:127`, `common.py:235` |
| 6 — ECDSA/BLS checks/s, fixed single/aggregated corpora | bench_dash named per-unit time can represent inverse throughput, but corpus equality/unit/batch equality for future Rust benches is a manual contract, not verified. Some batches are block-sized rather than checks. No input-hash/unit comparison or PF0 benchmark-head selection exists in pair. | `scenarios/microbench.py:69`, `pair.py:98` |
| 7 — replayed mainnet stream, sustained accept rate, p50/p95 | Independent regtest P2PKH fixture into an empty/near-empty mempool, not replayed mainnet traffic. Gates median latency and TPS; p99 is informational, p95 latency not independently evaluated. | `fixture.py:8`, `scenarios/mempool.py:44`, `mempool.py:74` |
| 8 — GBT p50/p95 at typical and full 300 MB mempool | Sizes 0/1k/5k/20k small fixture transactions; no full 300 MB fixture or asserted full budget, no p95 gate. Live-mainnet variant has differing mempools and is correctly informational rather than a substitute. | `scenarios/mempool.py:86`, `mempool.py:137` |
| 9 — synced mainnet startup with/without txindex/addressindex | Mainnet txindex flag exists, but addressindex option is ignored and required variants are not enumerated. No-index clones can still retain old index disk state. Cold metric omitted in concurrent mode; regtest startup is always warm. | `scenarios/startup.py:49`, `startup.py:66`, `startup.py:75` |
| 10 — IBD peak RSS and tip steady RSS as node/MN/MN-all-indexes | Peak IBD measured; “steady” is latter half of sync, not a settled tip measurement, and is informational. Idle RSS only covers ordinary startup modes. No MN modes; half-RSS target disabled. | `proc.py:183`, `common.py:210`, `scenarios/startup.py:46` |
| 11 — disk per blocks+undo, chainstate, MN/LLMQ and each index | Gates only aggregate directory size; components including all indexes as one group are information. Per-component regressions can cancel in total. | `common.py:15`, `common.py:219` |
| 12 — user+system CPU per block at tip, MN mode | Average CPU over complete IBD/reindex, includes startup; CPU/block is informational. No tip/MN workload. | `common.py:207`, `common.py:214` |
| 13 — fixed RPC mix at concurrency 1/16/64 | Single-client sequential latency; only six labels get throughput at c16/c64, no default c1 throughput or concurrent latency. Required getblock v2, getaddressutxos, protx, quorum info, masternodelist and GBT lack those throughput measurements. sendrawtransaction is measured separately at default c8. Addresses are random historical samples rather than asserted heavy addresses; no fixed mixed request schedule. | `scenarios/rpc.py:21`, `rpc.py:55`, `rpc.py:82`, `scenarios/mempool.py:45` |
| 14 — local DKG phases and session completion | Pending-only method; not executable. Description says “4-of-5” but proposed `llmqtestparams=5:3` is a 3-of-5 threshold. | `scenarios/masternode.py:26`, `masternode.py:31` |
| 15 — sign request to locally recovered signature | Pending; proposed observer endpoint is not explicitly the node whose local recovery is under test. | `scenarios/masternode.py:36` |
| 16 — first-seen transaction to ISDLOCK | Pending; proposed getrawtransaction.instantlock timing can conflate internal availability and notification/poll delay; no executable received-ISDLOCK boundary. | `scenarios/masternode.py:40` |
| 17 — received block to CLSIG | Pending; proposed generatetoaddress→getbestchainlock includes block generation and uses a state observer rather than the received-block/CLSIG endpoints. | `scenarios/masternode.py:45` |
| 18 — dsa→dsc success, saturation sessions/hour, CPU/session | Pending; proposed latency ends at dstx rather than dsc and omits saturated rate and CPU/session. | `scenarios/masternode.py:49` |

Pending masternode functionality is a reasonable staged limitation before the node exists, but neither a skipped scenario nor reduced metric coverage can count as fulfillment of that part of the performance contract. The baseline report acknowledges several deviations; acknowledgement is not an approved change to the acceptance criteria.

## Fairness and quality observations

The implementation has useful foundations: standard-library-only Python, clear module separation, a canonical configuration type, immutable fixture transactions within a shared stored fixture, local whitelisted seed clones, controller/worker pinning in the wall-clock path, ABBA ordering, alternating NUMA slices, context-managed unwrapped nodes, partial checkpoints, and error rows for missing whole pair sides. Its current tests correctly avoid pseudoreplication and cover the exact tail direction and basic failure accounting. These strengths do not repair the fail-open paths above.

The four local seeds have identical tips and are isolated on aux CPUs by default, but “not network-bound” is not proved by their existence: the baseline itself measures four-seed throughput below the real ten-peer network, and there is no saturation/capacity check or recorded netem profile. Concurrent pairs additionally share the physical disk and can perturb each other's I/O; CPU/NUMA swapping and optional cpu/memory caps do not cancel workload-dependent disk interference. `io.weight` ineffectiveness under the none scheduler is accurately acknowledged and warned about. NUMA pinning falls back to CPU-only taskset when numactl is absent, without marking lost memory binding; VM physical-vCPU pinning cannot be verified from this guest's slice flags alone.

The 22 tests are fast and pass, but are concentrated on arithmetic/parsing and a small number of canned verdicts. They do not exercise wrapper trees, abnormal node/wrapper exits, cgroup entry, cache-drop failure, input/cache concurrency, cold/warm provenance, canonical-path isolation, missing-plus-present gates, new confirmation failures, stale baseline markers, or required metric coverage. These need focused temporary/fake-process regression tests before the harness becomes a merge guard.

Other lower-priority issues: workload mempool/GBT reads txs.jsonl before calling fixture preparation, so a first invocation on a new root can fail unnecessarily; default version selection is global v23.1.8 rather than automatically/enforced per-network v24 for regtest/testnet (the v24 queue is explicit); microbench build uses a mutable Debian tag and unpinned apt packages, and can reuse old extracted source while replacing its COMMIT marker for a moved ref. README scenario defaults for SPV repetitions/RPC throughput are stale, as is its three-run PRD warning. Correct documentation and preserve actual source/toolchain provenance.

## Open assumptions and acceptance boundary

This review treats the PRD's current four-run long-scenario rule as authoritative, and A1 §3.11 as the approved split between PF0 per-PR counters and reserved-window wall-clock milestone evidence. There is no observed approval to replace those rules with default confirmation, 2% non-inferiority, omission of tracked quantities, or provisional shared-box PASS. Some PF0 automation may belong to a separate task; until integrated, describe this branch as a collection/workload component, not a complete per-PR gate.

No end-to-end Rust daemon, live masternode network, PMU run, mainnet replay, large-wallet scan, or real cold-cache experiment was performed. Those remain validation gaps, not claimed successes. The code-level failures and synthetic/process reproductions are sufficient to require changes independently of those expensive runs.
