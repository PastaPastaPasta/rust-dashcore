# PF1 independent review, round 1 — Sol

Reviewed `node/pf1-profile` at `cdba1e7826d3c264eb5c1134672e1d7ecfee18d2`, against merge base `a2b6f7c5ea0df035002e36c001001c28bbe166c4` with `node/integration`. This is a research/evidence review, not a consensus audit. No profiles were rerun, no node was started, and neither `/work/chains` nor `/work/bench` was accessed. The branch under review was not modified. This review is uncommitted in the integration worktree.

The headline observations are mostly sound. The changes required concern what can be inferred or budgeted from them, especially §11. There is no blocker requiring a long profile rerun: unsupported forecasts can be made conditional or removed, and missing evidence can be archived if still available.

## Verification performed

- Created a detached worktree under `/work/scratch/pf1-review/`; used `TMPDIR=/dev/shm/pf1-review-tmp`, mode 700, for checks. Both are removed after delivery.
- Recomputed per-block/per-operation costs from committed totals, operation counts and window metadata; checked bench aggregate identities, parallel wall ratios, recorded zero throttle counts, and the microbenchmark JSON.
- Read numerical weights in the committed SVGs. Recovered root-to-leaf weights for the four DWARF flamegraphs and ran the **committed `dwarf_callers.py`** on those recovered inputs: every §2.2 row, including the no-stack percentages, reproduces to the published precision. SVGs shorten symbols and can omit small frames, so this is not a replacement for the original full-symbol folded inputs when validating the subsystem classifier.
- Recomputed **all ten metric #5 cells and all four rounds per cell** with the committed `metric5.stats`/`pct` functions, directly from `metric5-raw.json.gz`. The raw file's summary and the separate summary JSON agree exactly. All 40 runs exited successfully, have 1,990 full and narrow samples, record `[253.0, 4.8]` MiB, and cover `W+11..W+2000`. Parser spot checks against Core's timer ordering and skip boundaries passed. Python syntax and shell syntax checks passed.
- Verified the relevant source paths using `git show v24.0.0-rc.3:...` in `~/workspace/dash`. The peeled tag is the report's `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

## Claims supported by the committed evidence

- **BLS dominates the measured checked testnet windows.** The headline serial/no-bench shares are 84.97–96.98% excluding the identified background thread. The later checked windows cost 156.7–175.6 M instructions/block, with 173.1 M at 1,520k. The 421k checked window is substantially cheaper, 36.3 M; the report does disclose this. AV windows below the AV height cost 2.659–18.252 M/block on the same excluded-thread basis.
- **Microbenchmarks:** `BLS_Verify_Normal` is 39,804,609.7 instructions, `ECDSAVerify` 379,106.9, and 80-byte X11 169,630.0. The roughly 105× verify comparison is supported for these benchmarks.
- **LevelDB attribution:** the SVG stacks positively identify the unnamed thread as `PosixEnv::BackgroundThreadEntryPoint → DBImpl::BackgroundCall → BackgroundCompaction`. It is separate from the connecting thread. The small direct-connect share is supported, subject to the rounding correction below. This does not establish that its work is unrelated to block processing (M2).
- **Parallel runs:** the reported wall ratios recompute to 1.227, 1.251, 1.244, 1.132 and 1.305. The quoted commitment-phase maximum ratios at 925k and 1,520k are 3.309× and 2.979×. These are observations from individual loaded-VM run pairs, rather than controlled estimates of the effect (M3).
- **Mempool:** 4,254,007,799 / 5,000 = 850,801.6 instructions/tx. The inclusive `CSHA512` cost is 6.206%, and its stacks do lead through salted coins-view hashers. Attribution to one cache constructor needs correction (m2).
- **GBT:** 303.001 M and 596.001 M instructions/call, approximately 60.5 K and 67.1 K per reported template transaction. The measured RPC/HTTP/JSON subsystem shares are 65.02% and 58.89%; the 303/596 instruction-sample counts and six-call limitation are correctly disclosed.
- **Startup:** 35,184,064,504 whole-process instructions, 19.374 s to RPC ready and an 81.48% leaf-owner `validation` classification. That classification is broader than chain-work calculation (M4).
- **IBD:** 1,261.220 G instructions × 73.90% / 169,630 gives approximately **5.49 M X11-hash equivalents**, not an instrumented call count. The report's approximate wording is appropriate. `CBlockHeader::GetHash` inclusive weight is 73.928%, consistent with the leaf X11 share. The three expensive header paths exist in the pinned source; details below.
- **Metric #5:** every displayed p50/p95/p99/max agrees with recomputation to rounding. In particular, 1,520k AV full p50/p95/p99/max is **8.35 / 36.9605 / 201.4569 / 1,522.94 ms**. The worst per-round p99 deviation from its pooled cell p99 is **36.85%** (1,520k noAV); the worst max/min p99 ratio is **1.817×**. Thus **±40% is an honest descriptive envelope for these four rounds**, not a confidence interval or a bound on future quiet-host measurements. Recorded loads span 26.01–256.48. The B1 addition accurately labels this as testnet-derived, with a nominal coins-cache budget within 0.1% of B_DEC43 and an explicitly disclosed additional empty-mempool allowance.

## Major findings

### M1 — A faster verify is extrapolated to all BLS work

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:311`.

The 173 M → 48–62 M projection divides the **entire 96.7% BLS subsystem** by 3–4, but its stated premise is only “3–4× fewer instructions per verify.” That subsystem also includes decoding/subgroup checks, aggregation and other arithmetic. §2.2 explicitly reports substantial decode work and cannot supply a complete non-overlapping breakdown; its 23–25% identified verify shares are from different, shorter windows and cannot be transferred either. There is also no committed comparison establishing the proposed alternative library's 3–4× factor.

**Failure scenario:** PF2 budgets a nearly threefold checked-block improvement from a faster pairing verify, while much of the measured BLS work is unaffected or improves by a different factor.

**Suggested fix:** label 3–4× as an external assumption with a traceable basis, and state that 48–62 M requires a **3–4× reduction of all profiled BLS work**, not just verification. Use operation-specific conditional budgets until measured. Keep BLS as a high-priority investigation; its dominance is real.

### M2 — Background compaction is assumed to be a fixed startup artifact

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:50`, `:75`, `:92`, `:320`; exclusion implemented in `contrib/node/pf1/perf_fold.py`.

The thread identity is supported. The assertions “does not scale with the number of blocks,” “constant background-thread cost,” and that its entire cost is snapshot-opening work are not. The main matrix holds window length fixed at 1,999 timed blocks; there is no committed idle-only control or matched length sweep separating reopening compaction from compaction caused by replay writes. A thread rooted at `clone3` establishes where execution occurs, not what caused its database work. Row 10 additionally asserts that compaction scales with write volume without measuring that relationship.

**Failure scenario:** planning uses 2.7 M/block as the total cost of a cheap AV replay and dismisses storage, although the same recorded window retires 4.1 M/block including asynchronous storage work. A rewrite still has to pay any replay-induced compaction.

**Suggested fix:** retain both columns and describe `ex_bg` as a foreground/other-thread breakdown, not a causally isolated total block-processing cost. Make the reopen-only explanation a hypothesis. The earlier 16–36% figure was misattributed to the synchronous connect path; it was not a nonexistent all-thread LevelDB cost. Row 10's 1–5% lighter-write estimate also needs a write-only breakdown: the EvoDB/MN-list subsystem includes work beyond writes.

### M3 — A theoretical ceiling and maximum ratios become promised wall/tail gains

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:154`, `:156`, `:313`.

§3 correctly explains that the connecting thread steals checks, and therefore reciprocal thread CPU share is only a lower bound on an idealized ceiling. Row 3 converts this into **“wall ≥1.6–2.0×”** for an optimization. A bound on the ceiling is not a minimum achievable gain. Likewise, the cited 3× figures divide maxima of different executions, without showing matched commitment-block timings, and cannot establish a **p99/max improvement of “3× or better.”** The reported whole-connect maximum at 1,520k improves only 2.756× in that pair; at 421k it worsens from 337.85 to 389.51 ms.

**Failure scenario:** performance work is planned against guaranteed average and p99 gains which the measured run pairs do not demonstrate.

**Suggested fix:** preserve the observed 1.13–1.30× ratios, explicitly conditional Amdahl discussion and phase-max ratios. Replace the proposed gains with targets/hypotheses; do not infer a p99 ratio from a maximum ratio. No new long profile is required to correct the text.

### M4 — Startup's validation bucket is not an eliminable chain-work budget

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:219`, `:315`.

The 81.5% classification is supported, but attributing all of it to re-deriving chain work and projecting **19.4 s → 1–3 s from persisting work** is not. The bucket includes index construction, candidate-set work, height sorting, skip links and allocation. In the committed instruction flamegraph, `GetBlockProof` is about **31.3% inclusive**, the work comparator about **21.9%**, and their union about **53.2%**. Persisting cumulative work avoids the proof calculation but does not automatically remove candidate ordering or these other tasks. LevelDB's 1.1 s CPU share is only one remaining cost, not the whole residual startup cost. No compact-index design or memory accounting supports the **≥2–3× RSS reduction** either, despite “not measured.”

**Failure scenario:** an index-persistence task is expected to remove almost all startup cost, or promised memory savings become a resource budget.

**Suggested fix:** distinguish work computation, index loading/construction and ordering. Give a conditional instruction budget for each proposed change; leave wall and RSS gains unquantified without a cost/layout model. Call 81.5% “validation/index work,” not measured chain-work computation alone.

### M5 — Serialization buckets are treated as entirely removable

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:206`, `:231`, `:316`, `:317`.

The measured GBT RPC/HTTP/JSON share is real, but **2.9× fewer instructions / 303 M → 106 M** assumes eliminating all 65%, including required hex/output generation, dispatch, copying and transport. Direct JSON serialization still has a cost. This is a zero-cost upper bound, not an estimate supported by the proposed implementation. The same issue affects **getblock v0 ≈5×, v1 ≈2×, v2 ≈3×**: no explicit disjoint removable-cost calculation and residual implementation cost is supplied for each.

There is a scope detail that matters to row 7: Core already caches `CTransaction::GetHash` (`src/primitives/transaction.h:289`). The observed per-call hashing is predominantly performed when `getblock` rereads/deserializes a fresh block and constructs new transactions (`rpc/blockchain.cpp:1051`, `node/blockstorage.cpp:807`, `primitives/transaction.cpp:127`). Adding another cache to that transaction object does not avoid the measured work. A cache that survives reads, persistent hashes, or a different raw-block serving path is needed. Similarly, v2's `TxToUniv`/Base58 shares include output semantics that a streaming serializer still must implement; they are not all tree overhead.

**Failure scenario:** an RPC rewrite claims a 3–5× budget by subtracting necessary response construction, or optimizes an object cache Core already has.

**Suggested fix:** present the measured buckets as opportunity bounds. State the cache lifetime/path required, distinguish avoidable tree/copy/escaping overhead from required encoding, and give conditional formulas with a nonzero residual. The roughly 2× v1 bound is mathematically plausible if almost all its hashing is removed, but that premise must be explicit.

### M6 — Synthetic-block parallelism estimate conflicts with its own evidence

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:319`.

Row 9 projects **6–8× wall gain on 12 cores** from parallel script checks and says “no instruction win.” Even treating the measured **85.03% instruction share** as a time share gives an ideal Amdahl speedup of only `1/(0.1497 + 0.8503/12) ≈ 4.53×`. The actual recorded script **CPU-time share is 77.8%**, giving about **3.49×** under that model. Neither model measures the wall critical path, so neither establishes an exact wall ceiling, but both contradict the row's claimed basis. Other pipeline improvements would need their own evidence.

A warm signature cache is a different workload and can save instructions: `CachingTransactionSignatureChecker::VerifySignature` returns on a cache hit before calling the ECDSA verifier (`src/script/sigcache.cpp:93`). It cannot simultaneously be included as a lever and dismissed as “no instruction win.” Its hit rate was not measured here.

**Failure scenario:** PF work is prioritized against an unsupported 6–8× script-only target, while a distinct potentially large cache benefit is missed or mixed into the parallelism estimate.

**Suggested fix:** separate cold-check parallelism, pipeline overlap and cache-hit scenarios. Publish the conditional Amdahl arithmetic; keep cache savings conditional on a stated hit rate.

## Minor findings and unsupported details

### m1 — Raw aggregation inputs and per-block phase evidence are incomplete

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:140`, `:142`, `:346`; `contrib/node/pf1/report_tables.py:45`, `parse_bench.py:36`.

The committed general-profile data contains derived summaries and bench aggregates, but not the original full-symbol folded stacks/TSVs or debug logs used by the aggregation scripts. `report_tables.py` cannot regenerate the published subsystem tables from that committed layout, and `parse_bench.py` cannot regenerate phase distributions. In particular, **“7 blocks ≥1.6 s” at 1,520k and “3 blocks ≥1.6 s” at 925k**, their association with quorum commitments, and the full-block-only statistics in §4 cannot be independently verified from the phase aggregates. Metric #5 has per-block full/narrow samples, but no matching per-block phase arrays to prove the “mostly commitments/GetQcHashes” tail attribution in §8. These are plausible explanations, not fully archived evidence.

**Suggested fix:** commit compressed folded/TSV inputs and compact per-height phase samples, or remove/qualify unarchived exact counts and provide an aggregate-only reproduction path. No need to commit giant `perf.data` files. Keep the exact metric #5 recomputation path, which already works.

### m2 — The 6.2% RNG cost belongs to two view constructors

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:189`, `:318`.

Inclusive `CSHA512` weight is 6.206%, but about **3.526 pp** is below `CCoinsViewCache::CCoinsViewCache` and **2.680 pp** below `CCoinsViewMemPool::CCoinsViewMemPool`. Core's per-accept `MemPoolAccept` constructs both views (`validation.cpp:563`). Reusing only the first cannot eliminate the entire quoted cost. The `sha512::Transform` leaf alone is 5.219%, rather than 6.2%; 6.2% is the inclusive SHA-512 family cost.

**Suggested fix:** name both salted-view constructors and the inclusive basis; budget each separately.

### m3 — Header optimization drops the residual X11 work; call-site names need precision

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:301`, `:314`.

The three paths in pinned Core are:

1. `PeerManagerImpl::CheckHeadersAreContinuous`, `src/net_processing.cpp:3174`, called from `ProcessHeadersMessage` at `:3352`; this is the inlined continuity-loop hash, not the one-per-message `headers.back().GetHash()` at `:3359`.
2. `CompressibleBlockHeader::Uncompress`, `src/primitives/block.cpp:122`, hashes the preceding header to restore `hashPrevBlock` when compressed; the first header of a message is exempt.
3. **`ChainstateManager::ProcessNewBlockHeaders` (plural)**, `src/validation.cpp:4736`, passes `header.GetHash()` to `AcceptBlockHeader`.

`CBlockHeader::GetHash` at `src/primitives/block.cpp:13` serializes and runs X11 every time; it has no hash cache. The recorded header-hash weights below these paths are approximately 21.5% each after separating the nested processing path. This supports the main header-caching observation.

However, row 4 applies a threefold call reduction to **all X11 work**, although §10 says about 13% of X11 is outside those three header paths. Retaining that residual and assuming the external 0.23× fast-X11 factor gives approximately `0.261 + 0.739 × (0.87/3 + 0.13) × 0.23 = 0.332`, or **3.01×**, rather than 3.2×. The fast-X11 factor itself is referenced only through a memory note, not committed PF1 evidence.

**Suggested fix:** correct the function name, state the compressed-header condition, retain the residual in the formula, and link a durable measurement for the external factor or mark it as an assumption.

### m4 — Flush absence is inferred, not established at the metric #5 budget

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:273`.

The inference that the cache should not fill is reasonable. But timings from the **1,024 MiB** runs do not prove no populated-cache flush occurred in the **263 MiB** runs, nor does an unmeasured timing threshold establish absence. The latter runner discarded its logs and does not retain `Writing chainstate` or occupancy. Core's `UpdateTip` lines actually contain occupancy (`validation.cpp:3180`); the runner does not preserve it. The assertion that CH2's flush effect **“is therefore not in this baseline”** is stronger than the archived evidence.

**Suggested fix:** say the baseline does not establish/characterize populated-cache flush behavior. Preserve occupancy/flush phase evidence on any future measurement, without rerunning these long workloads merely for this review.

### m5 — Small numerical/method labels need correction

**Location:** `docs/node-program/research/PF1-dashd-v24-profile.md:89`, `:92`, `:163`, `:243`, `:274`.

- Direct LevelDB is **1.24%** at 421k AV/no-bench and 1.26% with bench; “≤1.2% in every window” is a rounded display presented as a strict bound. Say approximately 1.3% or below 1.5%.
- §4's CPU row says 30,004 **txins**; committed `bench-connect-big.json` has 30,004 transactions and 30,000 txins.
- `-par=11` and `-parbls=11` create **10 additional workers per respective queue**, not 11, because Core subtracts the checking master (`init.cpp:1562`, `:2079`). The settings are recorded correctly; only the explanation is wrong.
- §8 describes spreads as `max/min − 1` but labels p50 “up to ±27%,” a different midpoint-based convention. The largest p50 ratio is **1.724×**; 925k AV p95 spans 6.899–13.175 ms, a **91%** spread rather than the stated typical ceiling of 85%. Keep the observed ranges and define the envelope convention. This does **not** invalidate the conservative ±40% p99 caveat.

### m6 — Generated inclusive CPU columns are false zeros

**Location:** `contrib/node/pf1/report_tables.py:110`; `docs/node-program/research/PF1/data/tables.md:20` and analogous tables.

The script looks up inclusive-group names in `subsystems_cpu`, whose keys are subsystem names, then defaults to zero. Thus the tables claim `ConnectTip`, `VerifyScript`, etc. have 0.00% CPU time regardless of their measured execution. This is separate from the documented regex-over-signature caveat.

**Suggested fix:** load `cpu-clock.inclusive.tsv` for that column, or omit it. The report does not rely on these zeros, so this is a data-presentation correction rather than a headline-result failure.

## Priority assessment for PF2+

BLS investigation deserves high priority for **checked testnet blocks**; header-hash reuse has a strong concrete basis for **this compressed-header-heavy IBD**; RPC representation overhead is another well-supported investigation. Their measured importance is supported, their exact rewrite gains are not.

The single top-10 order should not be treated as an evidence-backed ranking for a mainnet node. Checked testnet connect, AV IBD, cold synthetic full-block replay and live mempool/template calls have different bottlenecks. In particular, repeated BLS decode is explicitly uncounted, storage cannot be demoted on a presumed fixed compaction artifact, and signature-cache savings should be separated from parallel checks. Retain row 2 as an upper-bound hypothesis needing repeat counts; treat rows 1/3/5/6/7/9's numeric gains as conditional budgets or targets. Rows 8's roughly 2× uncached-ECDSA instruction floor and row 2's disclosed inclusive upper bounds are reasonable within their stated limits. The mempool RPC subtraction is an approximation, not a measured P2P acceptance workload.

**Verdict: CHANGES-REQUIRED.** The measured baseline is useful and the metric #5 table passes; the six major planning inferences above need correction before PF2+ uses §11 as its performance budget.
