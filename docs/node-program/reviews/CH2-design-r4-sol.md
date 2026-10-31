# CH2 design rev3 — Sol round-4 closure

**Verdict: CHANGES-REQUIRED.** Rev3 closes the fixed-cap completion failure and supplies the missing atomic handoff mechanism. DEC-95's call sites and threshold arithmetic are correct. It does **not** make the value fed to those thresholds Core's `DynamicMemoryUsage` number: physical flushing compares CH2's native allocation ledger, while the membership shadow separately compares a Core-model meter. The review's requested Core-accounting equivalence is therefore not established. There are also contradictory live handoff instructions and several smaller ownership/arithmetic/text corrections below. Sol is not yet satisfied; the sequential Opus high pass should follow closure of these items under DEC-92.

Reviewed read-only: `docs/node-program/designs/CH2-coins-flush.md` at **a5cb08fc**, starting with “Changes in rev3,” its parent diff, the r3 review and relevant earlier dispositions. The checked-out design matches the pinned blob. Core reference: **v24.0.0-rc.3**, commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, read with `git show` in `~/workspace/dash`. Design line numbers below refer to a5cb08fc. DEC-94 other than Q1, DEC-95, DEC-96 and DEC-81/82/85 are binding; none is reopened. This is a design review, not a claim that the future Rust acceptance tests have run.

## Round-3 disposition

| r3 item | Status | Evidence and residual |
|---|---|---|
| **G-R3-M1** allocation list versus `T_max` | **MOOT for the bound; accounting corrections substantially closed** | DEC-95 removes the reservation proof. §2.2:360–361 and §2.5:724–730 now charge the whole serialized long coin, distinguish the owned DB buffer from its arena copy, borrow restored coins from undo, count child/active overlap for an arena merge, and retain capacity until release. The borrowed-input-to-owned-undo transition still needs G-R4-m2. Native accounting versus Core-model accounting is G-R4-M1, a separate issue. |
| **G-R3-M2** moving streaming ceiling / one retry | **MOOT** | No operation admission, retry or oversized-operation budget path remains in the operative algorithm. An operation grows; checks occur afterward. No replacement ceiling is requested. |
| **G-R3-M3** fixed fallback cannot complete a valid block | **CLOSED at design level** | §2.3:399,416–437 explicitly grows either layout, including a full individual fallback shard and uncapped long payloads. a2 requires the low-budget output and long-input/connect/disconnect witnesses on the selected layout; a1 runs both layouts with growth accounted. Growth estimates and the claimed absence of ordinary growth need G-R4-m3. |
| **G-R3-M4** snapshot-to-live gap | **PARTIAL: mechanism closed, contradictory rule remains** | F10/`subscribe_at`, coherent `tip_seq`, P7 and C8(j) supply the missing linearization. The model passes and catches both new plants. §4.4.1:1666 still prescribes the old pending-inclusive cut; §4.4:1659 also omits the new fatal exception. Correct these under G-R4-m1. |
| **G-R3-m1** byte admission / inexact-only exemption | **CLOSED** | §2.9:1113–1118,1148–1151 and §5.9 give inexact-only subscribers no journal and O(1) progress. Exact entries are computed before admission; an oversized entry is admitted alone after the journal is empty. This implements DEC-96 Q3. C8(k) covers variable sizes, empty/non-empty oversized cases and 10^5 txindex-shaped blocks. The missing trait declaration is a cleanup item below. |
| **G-R3-m2** hardcoded reserve | **CLOSED** | `CoinsBudget` receives `EngineTuning::batch_reserve(S)` (§2.5:666–673,733–736); MemoryTuning is zero. The Rocks value and its preallocation argument are adapter-owned. §2.5.1:815 and a3 require an `after_bk` assertion and custom tuning with different reserve/journal values. |
| **G-R3-n1** old signatures, names, citations and gate summaries | **CLOSED for the listed edits** | Guarded read/snapshot signatures, `tip_seq` in step 5, the chosen subscriber hooks, revised density workload, v24 fatal-error wording, MP1 bookkeeping versus actual Uncache calls, and F3's fanout/model qualification are corrected. Additional stale passages are G-R4-m1/n1. |
| r3 spot-check: absent height row must try archive | **CLOSED** | §2.9:1126–1128 and §2.11:1274 explicitly try the archive on absence or hash mismatch; C8(c) now exercises lookup after blind deletion. `height_ranges` takes an exclusive lower bound, so recovery retains L's height row after decoding state at L. |
| r3 note: Core flush count was estimated | **CLOSED as a disclosure** | §5.9:1971–1972 distinguishes the measured Core-model harness cell from the extrapolated 10–15 full-window count. Actual M3 measurement remains required; DEC-94 Q2's stall/bytes ruling stands. |

## New findings

### G-R4-M1 — MAJOR: the Core-equivalence check compares the threshold function on CH2 bytes, not Core's accounted usage

**Where:** §2.5:672–676,701–710,739–740; §3.3:1366–1379; §6.7:2123–2124; a2:2144; A-1:2261. Compare §2.4.5:645–656.

There are two distinct counters in the design:

| Counter | Contents | What compares it |
|---|---|---|
| **Σ / `Usage::total()`** | Actual CH2 arena or hashbrown allocation capacities, whole compressed long payloads, retained F, prefetch, overlay/scratch/shadow allocations, plus adapter reserve R. At flush checks the overlay is gone. | `CoinsBudget::size_state`, then physical `FlushPolicy` CACHE_LARGE/CACHE_CRITICAL and the native memory evidence. |
| **`CoreCacheMeter`** | A simulated Linux Core map since **Core's** modeled last flush: pool chunks/list charges, prime-policy buckets and script heap, with Core's logical cache membership and FRESH behavior. | `CoreCacheShadow::core_size_state`, determining the virtual flush epochs used for `HaveCoinInCache`. |

Core actually compares `CoinsTip().DynamicMemoryUsage()` (`validation.cpp:2951`), defined as `memusage::DynamicUsage(cacheCoins) + cachedCoinsUsage` (`coins.cpp:40–42`). Its accounting is an explicit model, including `MallocUsage` rounding, the pool's chunk/list overhead, buckets and script capacity (`memusage.h:51–62,94–96,187–203`). A tagging allocator that adds Rust allocation request sizes proves the native ledger's internal exactness; it does not prove equality with that Core model. Different slot widths, different bucket policies, packed scripts and different retained cache epochs already prevent equality even without F, prefetch or R. D11's explanation as merely extra F/prefetch/R and “slightly earlier” is incomplete: native density can also make the same workload's Core flag fire **later**.

A concrete design-arithmetic witness is a single block leaving **20,000 inline P2PKH outputs**, starting with an empty tip cache, at `-dbcache=4`, no allowance. The outputs occupy roughly 620 kB in a transaction, within the 2 MB block limit. For the stated arena, six 3,640-slot chunks plus a 32,768-bucket five-byte index are **1,736,320 bytes**, before small bookkeeping, below B = **1,835,008** and above LARGE = **1,651,507**. A Linux Core-shaped 104-byte-node pool map has eight chunks and 20,753 buckets, giving **2,263,712 accounted bytes**, above B. Thus the native ledger yields LARGE while Core's meter yields CRITICAL; IF_NEEDED's CACHE_CRITICAL flags differ. The own THRESHOLD freeze can still fire, which is precisely why this is a counterexample to the claimed **Core flag/accounting equality**, not a claim that no physical flush occurs.

I checked those Core-shaped numbers with a compiled scratch probe using the **pinned Core PoolAllocator**, a 96-byte key/value pair, Linux libstdc++ and the pinned `memusage` formula. This probe validates the accounting shape and arithmetic; it is not a run of `ConnectBlock` in dashd or a Rust arena implementation.

The a2 boundary table feeds both functions the **same chosen integer Σ**. That correctly proves identical arithmetic at an equal numeric input. It cannot prove that the two implementations supply that same integer after the same operations. The membership meter's separate oracle comparison is the relevant route to Core-number equality, but the physical policy does not consume that meter.

**Closure:** explicitly distinguish native allocation accounting, Linux Core-model accounting and their consumers throughout the API, tests and A-1. The required comparison against Core's `DynamicMemoryUsage` must exercise the Core-model meter on the same operation stream, including its initial pool, chunk/list overhead, bucket growth and script capacities; physical policy tests must state which counter they consume. Under this review's requirement that the Core flag decision consume Core's accounted number, the current Σ input does not satisfy it. If native Σ is intended for physical pressure while the meter only governs membership, state that as the actual policy and reconcile the Core-identical-accounting claim; do not call equal-function tests an equal-accounting proof. D1 and DEC-94 Q2 already permit different asynchronous/THRESHOLD flush frequency and remain accepted. They do not turn Σ into Core's number.

### G-R4-m1 — MINOR, required: the operative sequence rule contradicts F10 and reproduces the X28 convention

**Where:** §4.4:1659; §4.4.1:1666, versus `FeedSource`/F1/F10 at 1191–1198,1205,1213–1216 and P7:2295.

Line 1666 still says `seq_synced` is **the pending event's seq if V has one**. Rev3's chosen handoff and model instead set it to **the next sequence V will assign**, excluding that pending event. These are incompatible instructions in the live case-test section, not revision history. The stale convention is exactly the `include-pending` plant X28 that the fresh run catches.

For a pending connect with seq k whose result is already p, the correct handoff sets the cut to k+1; delivering k again produces `Connected(p)` whose parent is a strict ancestor of p. The model rejects it. Keeping k as the advertised first sequence while excluding k from the feed can also leave a case-B implementation waiting for an event outside its subscription, depending on `last_seq` initialization. The model avoids both by setting `last_seq = cut - 1` and clearing `skipped` at handoff.

Line 1659's unconditional “is ignored” also contradicts the new `FeedOrder`/latch rule for an ancestor violation at line 1216.

**Closure:** use F10's next-sequence cut everywhere; state the handoff initialization of `last_seq`/`skipped`; make the generic ignore paragraph refer to the permitted dropped/compressed lineage and the fatal ancestor exception. Keep the re-check, final capped commit and feed installation under the accepted tip lock. No change to DEC-96 Q1 is needed.

### G-R4-m2 — MINOR, required: an input borrowed from active cannot become an owned undo coin by the stated move alone

**Where:** §2.5:725–728; `BlockOverlay::spend` at 535; `PackedCoin` representation at 359–361.

The new ownership paragraph makes a fetched input live **once in active**, with the child referring to those bytes. It then says spending writes a tombstone and **moves the bytes out for undo**, although `spend` returns an owned `PackedCoin` and the fallback's long payload is an owned Box. A borrower cannot move that Box. Moving it from active would also mutate the parent before overlay commit.

The rollback witness is: fetch a long input into a clean parent entry, borrow it in the child, spend it while constructing undo, then fail a later validation check and drop the overlay/undo. The parent must still return the original unspent coin. Transferring its only owned payload to undo cannot satisfy that requirement without an additional ownership/rollback mechanism. An arena offset cannot simply outlive the layer owning its bytes either.

**Closure:** preserve the parent until commit. Specify an owned copy into undo for a borrowed input, or a concrete shared owner with correctly counted lifetime; reserve “move” for child-owned coins. State how child references survive fallback rehashes, for example by resolving parent keys rather than retaining pointers into moving inline slots. Require long-input spend followed by overlay abort and then parent read/undo round-trip in a2. This is an ownership clarification; DEC-95 does not require restoring a per-operation memory bound.

The other two accounting repairs are sound as descriptions: the whole long serialization is charged, and restored undo bytes are borrowed then copied into the child, with a further arena-copy overlap at merge. For the generic DB path, “Vec to Box without a copy” must be qualified: `KvRead` promises an owned Vec, not `capacity == len`; conversion may shrink/reallocate. Include any such live capacity/overlap in native tagging. The existing Rocks point path uses `to_vec`, so this qualification principally preserves the engine-neutral contract.

### G-R4-m3 — MINOR: the fallback growth cost is a mean, and the stated threshold does not prevent ordinary shard growth

**Where:** §2.3:435–439, especially 438; §2.5:738; rev3 disposition rows 29,31.

At DEC-43, B = 252.75 MiB, R = 16 MiB + 64 KiB and prefetch headroom B/16 give a presize of **110.4453125 MiB**. Twice its mean per-shard share is indeed **3.451416 MiB**. But the stated binary decomposition uses **two shard sizes**, not 64 equal shards. Rounding down the `cap/73` bucket target gives 32 shards of 16,384 buckets and 32 of 32,768:

| Shard | Old table, approximately | New table allocated during doubling | Net retained increment |
|---|---:|---:|---:|
| 16,384 buckets | 1.140625 MiB | 2.28125 MiB | 1.140625 MiB |
| 32,768 buckets | 2.28125 MiB | **4.5625 MiB** | 2.28125 MiB |

These use the design's 73 bytes/bucket, before tiny table alignment/control-tail adjustments. “3.45 MiB per shard” is an average-scale estimate, not the maximum of one actual doubling; nor is every shard 1/64 of the layer. After repeated growth the transient doubles again.

“Normal blocks never trigger it, because the THRESHOLD freeze comes first” is also false for inline-only growth under the written policy: the presized table allocates about 110 MiB from creation, while `active >= f·B` requires about **126.375 MiB**. Filling the presize does not increase that allocation ledger. Ordinary single-output blocks can therefore fill and grow shards before THRESHOLD fires. The next sentence's “first of 64 shards to fill stops the layer” is the withdrawn fixed-cap rule.

The arena arithmetic **20 MiB new over 10 MiB old** and **640 MiB new over 320 MiB old** is correct for the specified index doublings. Core's eight-byte bucket-array overlap is real, but its exact growth is prime-policy/toolchain-dependent. A compiled local Linux libstdc++ policy probe starting from Core's unreserved empty-map construction gives **22.420341 MiB** for 1,447,153 → 2,938,679 buckets, and **1,572.132347 MiB** for 101,473,717 → 206,062,531. The quoted ≈34 MiB/≈2 GiB is not a verified value at a named Core rehash; report the pinned oracle's actual old/new bucket counts if those comparisons are retained.

**Closure:** label mean versus maximum, record actual per-shard and repeated growth, remove the “never” and stopping claims, and use actual Core bucket counts for the comparison. Keep the corrected block-by-block layout gate. The gate already counts both layouts' growth and reports peaks, so this does not reopen the old fixed-cap completion defect or impose a transient invariant.

### G-R4-n1 — NIT: one rule-O remnant, old ceiling summaries and a missing declared hook

**Where:** D11:1537; §5.2:1852; §5.3:1867; `SubscriberData` at 1035–1064 versus 1116.

- D11 still says the budget counts **“each op's bound T.”** This is an operative deviation row. Remove T and align it with the current native ledger and comparison scope. Historical “Changes in rev1/rev2” and expressly historical estimates may retain their old descriptions; they are not an active rule-O path.
- The performance summaries still claim `active <= f·B`, `active + F <= B-R` and a worst-case shutdown footprint `<= B`. DEC-95 permits a completed block to exceed these values. Rewrite these as ordinary sizing estimates with measured overshoot, not ceilings; likewise do not state Core's shutdown cache is universally `<= B-10 MiB`.
- The prose says every `SubscriberData` declares `max_entry_bytes()`, but the trait sketch omits it. Add the method and specify that entry bytes include owned row/state/container capacity. Admission applies to the retained journal; the computed-but-not-admitted entry also consumes transient memory and must be visible in the process evidence.

## Binding rulings and carried PARTIAL items

| Item previously carried | r4 status |
|---|---|
| **G-R1-M1** hard-ceiling completion | **MOOT** under DEC-95; growth supplies completion. The W-Arc-before-FlushDone release rule remains closed. |
| **G-R2-M1** long inputs missing from `T` | **MOOT** for the bound; whole long bytes are included. G-R4-m2 addresses the remaining borrowed-spend ownership transition. |
| **Opus F1** mid-block ceiling | **MOOT** under DEC-95. DEC-96's block-boundary maximum plus `rss_anon_peak` is stated in §2.5:722–723, §5.5:1890 and Q2:2280; mid-block tagged peaks are reported. No hard peak invariant is requested. |
| **O-R2-m1** growth absent from T | **MOOT**; actual growth is native Σ and peak evidence. Correct its new estimates under G-R4-m3. |
| **O-R2-n6** fixed-table baseline / RULE_O | **MOOT** for the bound; capacity is charged from creation, including retained allocations. The separate stale T sentence is G-R4-n1. |
| **O-R2-M4** txindex-driven flush cliff | **CLOSED**: no journal or byte/block limit for inexact-only subscribers. Their no-DMAX result is engine-independent. Full-window Core count remains expressly estimated. |
| **O-R2-M5** fallback/gate cliff | **CLOSED as the former gate/completion defect**: both layouts grow and are run block by block under one policy, with growth and peaks included. The arena-winner thresholds, sweep and Core floor remain. Growth-cost prose needs G-R4-m3. |
| **O-R2-m10** journal memory | **CLOSED at the retained-journal contract level**: pre-entry byte admission and DEC-96's one oversized entry alone. C8(k), declared entry bound and process memory reporting remain implementation gates. |
| **SubscriberFeed F1–F9 / P1–P9** sufficiency | **PARTIAL only because of G-R4-m1's contradictory live instructions**. F10/P7 now supply the previously missing atomic operation; the corrected contract is sufficient for the modeled cut. |
| **EngineTuning** isolation | **CLOSED**: reserve, journal sizing, post-Bk hook and configuration have adapter homes. RocksDB remains provisional under DEC-85. |

DEC-81's **entire CH2b hold** remains; the corrected producer/storage contract must go into SB0. DEC-82's only parity reference is v24. DEC-94 Q2's accepted flush-frequency tradeoff, Q3/Q4's oracle/Linux-accounting obligations and Q6/Q7's CH2a/CH2b scope stand. DEC-96 accepts the tip-lock handoff commit, the prescribed peak evidence and the single oversized journal entry; none is a new approval question here.

## Handoff correctness and model evidence

With F10 interpreted consistently, the cut is atomic in the needed sense. While the tip lock is held, V cannot assign another sequence or change the tip. If p differs from the live tip, no feed is installed and the driver refreshes/rewinds. Otherwise the final commit and installation occur under that lock, with `cut = next_seq`. Every change assigned before the cut is reflected in the current tip p and excluded, including any pending emission. Every later change has sequence at least the cut and is delivered exactly once by F1. A tip moving away and back to p before acquisition is harmless: the subscriber already has p's data and the subscription starts from that current state. This closes r3's A3→B3 gap without requiring replay of pre-cut backlog. The feed must also initialize the hub's class consistently with F7's capture; the model makes the class/queue/cut one transition.

The model is an abstraction of that contract, not a threaded implementation proof. Its stale-branch rewind check precedes `ready`/subscription; the no-recheck plant's smallest witness is forward movement, not the exact stale-branch producer from r3. Reorg cases are exercised by the grid and the explicit C8(j) requirement remains appropriate. Independent partial fanout, real commit scheduling, data/undo lookup, ownership, allocation bytes and oversized journal admission are outside this Python model and remain Rust/threaded/CRASH acceptance work.

Fresh runs used an unchanged scratch copy of `/work/scratch/ch2-rev3/liveness_model_v3.py`, SHA-256 `49d5440f3a31af26857612ff2248376cce93d94b4f3ccd24f993edd70f308015`:

| Run | Evidence | Result |
|---|---|---|
| Full-grid sample | SS/SY/YS × shared/per-subscriber × no reorg/depth-2 reorg/depth-2 invalidate; D=2,Q=2,ACT=3,MAXH=5 | **18 configurations / 1,854,502 states**; safety and AG EF completion pass, zero deadlocks/cannot-finish states. |
| Directed | `python3 liveness_model_v3.py --part directed` | All **8** positive/negative expectations pass, including B1, pending-emission case test and parked-state regressions. |
| X27, no live-tip recheck | SY, shared, D=1,Q=1,ACT=1, no reorg, MAXH=5 | **Caught**: 43,063 explored states, 44 deadlocks, 29,393 cannot-finish states. Shortest bad prefix: `Y1.ready@(0,0); V.conn(1,0)`. |
| X28, include pre-cut pending event | Same configuration | **Caught by the contract check**: `V.conn(1,0); Y1.step(1,0); Y1.ready@(1,0); Y1.synced@(0,0)(pending); V.emit C(1,0)`. Ignored connect's parent is a strict ancestor of p=(1,0). |
| Handoff coverage | `python3 liveness_model_v3.py --part handoff`, all **60 Syncing+reorg configurations** | **ALL PASS**; independently reproduced **983,348 failed re-check transitions** and **160,320 pending-emission handoffs**, exactly the published counters. This coverage traversal is separate from the sample's safety/AG EF check. |
| Supplied full-grid logs independently counted | 12 `run-grid-*.txt` files | **120 successful state records / 19,367,358 states**, no FAIL. This is log verification, not a fresh full-grid safety rerun. |

Scratch arithmetic/C++ probes also verified both threshold boundaries, shard decomposition, arena doubling sizes and the Core-shaped allocator/policy examples above. No production source, input model or design was changed; no commit was made. Temporary review scratch is removed at handoff.

## Closure needed before the Opus pass

Resolve **G-R4-M1**'s accounting/comparison claim; align the live handoff rules under **G-R4-m1**; complete the borrowed-spend ownership contract under **G-R4-m2**; correct the fallback growth estimates under **G-R4-m3** and remove **G-R4-n1**'s stale text. The discarded rule-O bound and the three accepted DEC-96 choices need no further design round. Retain the specified implementation gates and CH2b's SB0 hold, then request Sol closure before the sequential Opus high review.
