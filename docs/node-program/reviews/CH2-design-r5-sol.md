# CH2 design rev4 — Sol round-5 confirmation

**Verdict: APPROVE-WITH-NITS.** All five round-4 items are closed at design level under the binding rulings. Two new, non-blocking inaccuracies in the fallback's quantitative prose are listed below. Neither requires changing the growth algorithm, reinstating a hard memory invariant, or reopening DEC-99/100. Sol is satisfied for the sequential Opus pass; the implementation acceptance gates and DEC-81's CH2b hold remain.

Reviewed read-only on `node/integration` in `~/workspace/rdc-wt/integration`, starting with **“Changes in rev4.”** Scope: `aeec85d4` plus `28bff6ce`, the round-4 review, the relevant operative sections and their acceptance criteria. The checked-out design matches `28bff6ce` exactly (blob `593d95e31b999b8abd559bb70c62d53332c223e0`; worktree HEAD was `7aae4dd7`). Line references below are to that design blob. DEC-95, DEC-96, DEC-99 and DEC-100 are accepted as binding. This is design approval, not a claim that CH2's future Rust tests or meter have been implemented or passed.

## Item statuses

| Item | Status | Confirmation |
|---|---|---|
| **G-R4-M1** | **CLOSED** | The counter table, typed API, physical policy, shadow epoch rule, deviations, evidence gates, a2/c3 and A-1 distinguish **native Σ / `NativeUsage`** from **Core-model usage / `CoreModelUsage`**. Physical flushing consumes native Σ at Core's call sites with Core's arithmetic; the shadow consumes the meter for D12 epochs. The operative text expressly permits physical LARGE/CRITICAL to fire earlier or later than Core's. Equal-input threshold tests are explicitly arithmetic tests, not an accounting-equivalence proof. `core_meter_vectors` requires equality against the pinned probe's `DynamicMemoryUsage()` **after every operation**, plus Core flags at IF_NEEDED/PERIODIC points (§2.4.5:711–720; a2:2251). The specified scripts include the initial pool chunk, chunk/free-list behavior, unreserved-map bucket growth, source-path script capacities, flush `ReallocateCache`, and the 20,000-output witness. |
| **G-R4-m1** | **CLOSED** | §4.4.1:1765–1770 now uses F10's next-sequence cut, initializes `last_seq = cut − 1`, clears `skipped`, excludes a pending pre-cut event and treats its `FlushCommit` as case A. This agrees with `FeedSource::subscribe_at`, F1/F10 and P7. The ignored-event paragraph preserves the fatal strict-ancestor `FeedOrder` exception. X28 survives only as an explicitly withdrawn convention/negative test, not an operative rule. The existing model's handoff at `liveness_model_v3.py:303` has the same initialization; no changed model required a new liveness sweep. |
| **G-R4-m2** | **CLOSED** | §2.4.2:584–586 and §2.5:822–835 distinguish child-owned moves from copies of parent/F/prefetch/scratch coins. Inline copies are by value; borrowed long coins get a new owned box. Arena bytes are copied out even for a child-owned coin. The parent remains unchanged until commit, and the child retains keys rather than parent pointers/offsets across rehashes. Long undo results carry `LedgerBox` accounting in `held_out` until their actual drop, including drops in CH3a or on another thread; ownership transfers do not double-count them. `held_out == 0` at flush checks is tied to writing/dropping undo before the block returns and asserted by §6.7. The DB Vec-to-Box path now includes possible reallocation/overlap. a2 requires abort-and-parent-read and commit/disconnect round trips with intervening rehashes. |
| **G-R4-m3** | **CLOSED, with the two nits below** | §2.3:474–489 replaces the mean shard cost with the correct per-size doubling costs and repeated-growth multiplier; it includes aggregate retained growth, the large-block witness and actual libstdc++ bucket transitions. The **55/64 live-load trigger** is a sound early-freeze heuristic at block boundaries, with unrestricted growth during a block or while F is in flight. `28bff6ce` correctly addresses tombstone exhaustion: below the trigger, rebuild at the same bucket count before an insert instead of accepting hashbrown's doubling. Dropping DELETED controls restores more than 1/64 of the buckets of insert room. Rebuild memory is charged, and frequency/cost is measured. This supplies completion and pressure control; it is not a deterministic promise that every block avoids rehashing. |
| **G-R4-n1** | **CLOSED** | D11:1635 has no per-operation bound T. §5.2 and §5.3 describe ordinary sizing and measured overshoot, and neither Core nor CH2 has a universal shutdown ceiling. `SubscriberData::max_entry_bytes()` is declared at 1158, includes owned row/state/container capacity, and returns zero for inexact-only subscribers. The admission contract applies to the retained journal; a computed but unadmitted entry is transient process memory. |

DEC-100 also closes rev4 §10 Q1: the native ≤ B criterion applies to block boundaries of the CH2c/DEC-43 gated workloads; mid-block peaks are reported, and the low-budget completion witnesses are exempt. It is not a further approval question.

## Probe identity and independent checks

**The specified a2 probe is a real v24 `CCoinsViewCache`, not the synthetic map used in round 4.** §2.4.5:712 explicitly requires the pinned `coins.cpp` implementation over a stub `CCoinsView` with Linux libstdc++. The stub replaces database persistence, not cache accounting. The pin is **v24.0.0-rc.3 / `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`**. The generator `contrib/node/core-coins-meter` and its checked-in vectors do **not yet exist** in this design-stage checkout. Their provenance, compiler/library versions and per-op comparisons are future a2 acceptance requirements. The phrase “as Sol's r4 probe was” should be read only as referring to the pin/toolchain: r4 explicitly used a Core-shaped map, not `CCoinsViewCache`.

For independent confirmation, I compiled the pinned, unmodified `coins.cpp` in scratch against byte-verified pinned `coins.h`, `memusage.h` and `pool.h`, using **GCC 15.2.0, Linux 64-bit libstdc++ `__GLIBCXX__=20260321`**. A small stub base and derived inspection class drove the real cache; the hash-constructor shim used the pin's exact deterministic salts, and the normal SipHash implementation remained in use. This is a spot check, not the future meter-vectors suite or a `ConnectBlock` run.

| Check | Result |
|---|---|
| Empty constructor / after flush reallocation | **262,240** accounted bytes, one pool chunk and one bucket |
| 20,000 P2PKH-length (25-byte) script coins | **2,263,712** bytes, eight pool chunks and **20,753** buckets; reproduces the accounting witness |
| Spend all FRESH entries, then refill | Size falls to zero while chunks/buckets and usage remain retained; refill reuses the pool without increased usage |
| Fetch/uncache lengths 0, 28, 29, 10,000 | Script heap charges **0, 0, 48, 10,016** bytes respectively; uncache removes those charges |
| Child `BatchWrite` of a 29-byte script coin | Parent includes the **48-byte** script charge |
| Grow beyond 1,447,153 entries | Bucket transition **1,447,153 → 2,938,679**, as stated in rev4 |

The fallback arithmetic also checks: `1/64 ÷ (7/8) = 1/56 = 1.7857%`; applying the margin to the old estimate gives **2,644,149** coins and **0.823398** against 3,211,264 arena coins. A scratch rerun of the prior seeded capacity model with 55/64 in place of 7/8 gives **2,641,286** coins and **0.822507**, consistent with the rounded **2.64M / 0.823** claim. That legacy model remains only an estimate; a1's actual block-by-block density gate decides the layout. The per-shard table sizes **1.140625 / 2.28125 MiB**, doubling allocations **2.28125 / 4.5625 MiB**, and repeated doubling multiplier are correct before the explicitly excluded small alignment/control-tail adjustments.

## New non-blocking nits in rev4

### G-R5-n1 — The 20,000-entry no-growth statement needs probabilistic wording

**Where:** §2.3:470.

The expected increment plus 3σ does fit each DEC-43 shard's margin: approximately **251.4 < 256** entries for a small shard and **477.3 < 512** for a large one. That does not establish the categorical statement “a block adding up to ≈ 20,000 entries grows no shard.” Even under independent uniform routing, a small shard's binomial probability of receiving more than 256 entries is about **0.000580**; over 32 such shards the union upper bound is about **1.86%**. A concentrated routing outcome is also possible. The trigger remains sound as a heuristic because exceeding the margin grows the table rather than rejecting the block.

**Nit:** say that 20,000 entries ordinarily fit under the stated uniform-routing estimate, with occasional growth permitted. Retain the trace-specific a1 assertion as an empirical acceptance check. No new hard no-growth invariant is needed.

### G-R5-n2 — The strict CRITICAL growth-headroom bound omits rounding slack

**Where:** §2.3:483.

The target layer cap is **110.4453125 MiB**, but the stated binary decomposition allocates `73 × (32 × 16,384 + 32 × 32,768)` = **109.5 MiB** per table. Each presized layer therefore leaves **0.9453125 MiB** of rounding slack. In the inline-only estimate, two actual tables plus R leave **17.6875 MiB − prefetch** before B, not the claimed “at most B/16 − prefetch (≤ 15.8 MiB).” The strict upper bound is false even before tiny metadata adjustments. “≈ 110.4 MiB” is acceptable cap-scale prose, but the aggregate actual table doubling keeps **109.5 MiB** under this decomposition.

**Nit:** include `2 × (cap − actual_table_bytes)` in the headroom estimate, or state an approximate cap-based estimate without “at most.” Physical CRITICAL still consumes the exact native ledger, so this arithmetic correction does not change the algorithm or invalidate the rounded density figures.

No blocking defect was found in the rev4 changes. No source/design was modified, no commit was made, and the review scratch directory was removed after writing this report.
