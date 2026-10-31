# CH2 design review, round 1 — GPT

Reviewed `designs/CH2-coins-flush.md` rev0, integration commit **fca00cb85d48881d9ecd2b3bb4c0466f1fd0d727**, against ST3a rev5 §5.6, DEC-43/59/62/63/64/66/69/74/**77**, A1 §3.4–§3.5 and ST3a's superseding errata, and the PLAN CH2 row. DEC-77 is binding: the split, proxy gate, fallible view, import-time normalization, pure selector, layout gate and txindex exception are accepted decisions, not findings to reopen.

Core citations below name `src/` paths at **v23.1.8 / v24.0.0-rc.3**, respectively. I extracted the actual pinned blobs with `git show`; no reliance on the working Core checkout's line numbers. Merged ST1/ST2 and CS0 were inspected on `~/workspace/rdc-wt/integration`; the current CS5a branch is ahead of the design's recorded revision, so this review concerns its proposed interface, not an implementation sign-off.

Sources remained unchanged. This requested review file is the output exception. Scratch: `/work/scratch/ch2-gpt/`; test `TMPDIR=/dev/shm/ch2-gpt-tmp`, mode 700.

## Findings by severity

### B1 — BLOCKER: the out-of-band gate does not complete an already-reached flush, invalidating deadlock freedom

**Design:** `CH2-coins-flush.md:1035–1039`, `:1058–1070`; step 6 at `:973` and the queued control-event API at `:597`, `:626`.

A stable-chain schedule suffices; a reorg is not necessary:

1. Durable subscriber locator L=0, processed p=8, D_max=8. V freezes `new=8`, then queues `Connected(9)…Connected(17)` before W finishes B1.
2. W returns B1 and queues `FlushCommit(8)` behind those events. It waits for this Synced mandatory subscriber's ack.
3. Before block 9, the subscriber sees the outstanding commit out of band, checkpoints at `LCA(p,T_ack)=8`, and resumes. L is now 8.
4. It processes through block 16. Before block 17 it checkpoints again at `LCA(16,8)=8`. L does not advance. Block 17 remains forbidden, and the queued `FlushCommit` can never be dispatched to produce the ack.

Repeating the specified checkpoint livelocks; waiting for another flush deadlocks, since W is waiting for the ack and only one F is allowed. V's service loop and unbounded control-event *enqueueing* do not resolve blocked control-event *delivery*. The same problem is sharper on a branch where the cap is an LCA below p.

**Binding/Core evidence:** ST3a §5.6 item 6 explicitly distinguishes a path drain from the case where `Connected(new)` was already processed: the latter commits at `LCA(p,new)` without draining. Checkpoints only guarantee advancement on the path **toward new**. Core `index/base.cpp:290–327 / :332–369` receives its flush callback after connection notifications; Core has neither this D_max gate nor this asynchronous W→subscriber wait. Its queue ordering alone cannot prove the new mechanism live.

**Fix:** service the outstanding commit out of band as an actual commit/drain-and-ack operation. If `new` was already processed, commit and ack immediately at the allowed LCA; do not require processing later queued blocks first. If it was not, explicitly enter the path drain toward `new`, with checkpoints only on that leg. Mark the queued control event consumed/idempotently acknowledged. After ack, a later blocked event may request the **next** flush. Add this exact schedule, a fork variant, and two mandatory subscribers on one shared events thread to deterministic watchdog tests; update the wait-for proof accordingly.

**Proof:** `check_design.py` / `design-counterexamples.txt` finds the fixed point `L=8, p=16, blocked=17`, with W still waiting for ack.

### M1 — MAJOR: the hard coins ceiling has no legal block-completion path

**Design:** `:403–413`, `:203–205`, `:959`, `:984–990`, `:1360–1371`.

The design checks `active + F + prefetch + overlay + R + growth_headroom ≤ B(t)` before **every** growth and asserts it after every operation, but freezes only at block boundaries. If an overlay or tip fetch reaches the ceiling with no F, there is no W release to wait for, and an incomplete block cannot be frozen: the packet must represent a fully applied tip. Returning `BudgetExceeded` is also not a valid block rejection. Waiting for an existing F may help once, but does not solve a block whose own overlay exceeds B, or the overlap while child entries are merged into active.

This is reachable with allowed configurations and valid blocks. At `-dbcache=4`, txindex off and no lent allowance, Core's coins share is **1.75 MiB**. A sub-2-MB block containing 50,000 small spendable outputs is within the block size limit, but the proposed slots alone require **3,600,000 bytes**, before the index, chunk slack, overlay/parent overlap, reserve or growth headroom. At p300, a block can instead arrive just below the limit, or B can shrink while allocations are already held. Freezing later does not satisfy an invariant asserted now.

There is also a release-accounting race: step 11 (`:978`) sends `FlushDone` **before dropping W's Arc**. V can remove F from its accounting and allocate/freeze the next layer while W still owns the previous allocation. Count actual ownership or drop W's reference before advertising freed capacity.

**Core evidence:** `node/caches.cpp:12–33 / :15–42` permits the 4-MiB setting. `validation.cpp:2737–2759 / :2945–2967` deliberately detects `cacheSize > nTotalSpace`; it is a flush threshold, not a hard per-allocation ceiling. Flush calls occur after connection/disconnection (`:3207, :3070 / :3457, :3304`). `coins.cpp:182–252` at both pins merges an already-built child. `consensus/consensus.h:10–14` at both pins allows 2-MB DIP1 blocks. Equality of B with Core's nTotalSpace is correct, but does not supply Core with a strict B invariant.

**Fix:** specify an implementable overflow strategy before approval: bounded, accounted block transients with an explicit revised invariant, or a transactional spill mechanism that can complete/abort a block without exposing partial state. Pre-block eviction/flush alone is insufficient for a block larger than B. Include low-cache high-output and high-input blocks, merge peaks, a shrinking mempool allowance, and Arc-release ordering in allocator tests. Preserve DEC-77's arithmetic/tagging rule while resolving the exact bound with the manager.

### M2 — MAJOR: WBM charging does not prove the hard engine-memory bound

**Design:** `:419–426`, especially `:422`; A1 erratum A-2 at `:1495` and memory comparison `:1141–1148`.

The **74 MiB arithmetic is correct** at p300: 2 MiB block tree + 8 MiB coins DB + 64 MiB EvoDB. The assertion that WBM charging means “memtables and blocks together never exceed E” is not correct for the specified engine.

RocksDB's WBM triggers flushing/stalling at a **soft** threshold; it does not reject an allocation that crosses it. The design does not specify its required `allow_stall` argument. The existing ST1 engine constructs the default, non-strict LRU (`dashd-store/src/rocks.rs:152`) and pins L0/top-level metadata (`:407`, `:423`). Referenced cache blocks cannot be evicted, and default cache insertion may exceed capacity. Setting a strict cache alone is insufficient: RocksDB's WBM currently absorbs reservation failures rather than preventing the memtable allocation.

**Native witness:** linked against the locally built RocksDB 11.8.1, with a 74-MiB cache, 40 MiB of pinned cache charge, a 37-MiB WBM, and **allow_stall=true**, reserving 40 MiB prints:

```text
cache capacity MiB=74 cache usage MiB=80 memtables MiB=40 ShouldStall=1
```

This tests the actual accounting primitives, not process RSS or a full-node load. It proves that even enabling stalls does not make the claimed mathematical bound follow from those primitives.

**Code/Core evidence:** bundled RocksDB `include/rocksdb/cache.h:140–145` (default non-strict), `include/rocksdb/write_buffer_manager.h:112–156` (flush/stall checks), `memtable/write_buffer_manager.cc:56–83` (reservation occurs and reservation errors are absorbed); rust-rocksdb 0.25 `src/db_options.rs:75–110` describes the soft limit. Core `dbwrapper.cpp:112–117 / :113–118` divides each configured DB cache between block cache and up to two write buffers; `evo/evodb.cpp:35 / :38` supplies the fixed 64 MiB. These support equal **configured shares**, not a byte-exact bound on all engine allocations.

**Fix:** specify `allow_stall`, cache capacity behavior, pinned/transient allowance, and measurable upper-bound/overshoot accounting. Avoid treating cache charge and real memory as interchangeable. Exercise large B1/evo batches, all CFs, held snapshots/iterators, stalled flushes and compactions; report both charge and actual memtable memory, plus RSS. If E remains a configured target rather than a strict bound, say so and make DEC-43's peak-RSS gate the evidence. Keep the accepted 74-MiB split; repair its proof and enforcement claims.

### M3 — MAJOR: the generic subscriber API lacks a locator-target state contract

**Design:** `:603–637`, `:1030–1042`, `:1296–1298`; proposed per-index responsibilities at `:41–48`.

`SubscriberData` has only connect/disconnect staging and height-range deletion. It cannot add subscriber-specific checkpoint state to the private locator batch, or restore such state at the locator after recovery. Nor does the design require all subscriber data to be immutable, height-versioned records from which state at X can be reconstructed. The submit-before-sync rule is necessary, but a submitted-data tip s is not proof that cumulative data currently stored describes an earlier target X.

Coinstatsindex is a concrete required consumer: Core atomically commits its **full MuHash accumulator** with its locator. The CH2 driver may commit at X<p. Writing a fixed-key accumulator during each `stage_connected` is unsafe: it can be durable at p while the locator is X; deleting `(X,X+D_max]` does not restore that fixed key. Omitting the checkpoint write leaves no guaranteed accumulator at X either. A rewind commit similarly needs the checkpoint at its fork, not merely range deletes and the generic locator.

**Core evidence:** `index/coinstatsindex.cpp:230–232 / :231–233` deliberately leaves DB_MUHASH out of per-block writes; `:392–397 / :394–399` puts it in the locator commit. `index/base.cpp:210–221 / :218–234` supplies `CommitInternal`/`CustomCommit` and the locator in that batch. Initialization loads the committed accumulator (`coinstatsindex.cpp:349–389 / :350–392`). ST3a R11c(i) requires data equal to the reference **at L immediately after blind rewind**; it is not enough to repair it eventually at G. DEC-77's txindex exception does not exempt coinstatsindex.

**Fix:** add a checkpoint-state contract and hooks to encode/restore subscriber state **at the chosen X**, including fork rewinds, atomically with the locator. Alternatively mandate an explicit height-versioned schema and show how the driver/data object retrieves X's full accumulator without replay-key reads. State how staging through X is selected when p>X, and how state below X is protected from later writes. Test a coinstats-shaped subscriber with a fixed-key accumulator or the actual proposed replacement schema, capped commits behind p, rewind commits and every crash point. A txindex-shaped/per-height toy alone will not expose this gap.

### M4 — MAJOR: legacy undo normalization must resolve transaction occurrences on the correct branch

**Design:** `:853–859`, especially `:854–855`; CH6a/CH5 assignments at `:1427–1428` and DEC-77 Q3.

The proposed importer lists zero-height entries by prevout **txid**, then resolves “each txid's height” with a pass over adopted blocks. There need not be one height for that txid in those files. A normal transaction can be included at A100, become disconnected, and be included at B110 on another branch. Adopted Core block/rev files may contain both branches and their undo records. An undo in a descendant of A must restore height 100; one in a descendant of B must restore 110. A first/last txid→height table necessarily normalizes one incorrectly, despite both branch histories being healthy.

A pass restricted to the imported active chain avoids ambiguity only by failing to normalize the adopted stale-branch undo that can later be needed. This is unrelated to duplicate coinbases/BIP30 violations. The intended branch-aware resolution and handling of missing/pruned ancestors are unspecified, so the claim of byte identity with current Core undo is not established.

**Core evidence:** `undo.h:36–49` at both pins explains missing metadata; `validation.cpp:1972–1983 / :2258–2269` resolves from another output in the **current disconnect view**, thereby using the relevant branch. Core replay walks the actual old branch (`validation.cpp:4910–4934 / :5261–5285`), not an arbitrary physical-file ordering.

**Fix:** retain the spending block/index and prevout occurrence context. Resolve the creating transaction among ancestors of that block, respecting in-block order, and reject ambiguous or unavailable history explicitly. Batch candidate discovery in one scan if desired, but choose candidates by ancestry, not txid alone. Gate import on a two-fork fixture with the same normal transaction at different heights and legacy undo on both branches; then crash/recover a transition using each branch. Keep recovery blind and normalization at import, as DEC-77 requires.

### m1 — MINOR: lookup equivalence is valid, but cache-membership/uncache equivalence is not

**Design:** `:24`, `:262–263`, `:274–280`, `:726–745`, `:1271–1277`.

The proof establishes **unspent-value lookup equivalence** of F over DB to DB at new. It does not establish every cache API's equivalence. After a hidden freeze, F-only unspent hits are not copied to active; `have_in_cache` reports F, but `uncache` cannot remove a clean F entry because F is immutable. For a clean coin, `Get; Uncache; HaveInCache` returns false in Core but may remain true here. After a hidden freeze/release, Core can still retain clean coins while this implementation has dropped them. The L1 script includes these operations and demands per-op equality while scheduling hidden freezes; D2 exempts flags only, so that acceptance criterion is unsatisfiable.

**Core:** `coins.cpp:44–59, :165–170, :254–265, :284–297` at both pins. Core Uncache removes a flags-zero local entry and HaveCoinInCache checks an unspent local entry.

**Fix:** define the precise cache-membership and eviction semantics, including authoritative active tombstones over unspent F. Declare the justified internal deviation and compare contents/errors separately from membership/eviction in the asynchronous oracle, or align logical flushes between oracle and SUT. Do not describe the entire cache stack as identical to a just-flushed Core cache.

### m2 — MINOR: startup's ordering uses ancestry before loading the index/tip

**Design:** `:1004–1007` (§4.3 steps 1, 3, 4).

Step 1 computes subscriber locks with `LCA(locator,recovered tip)` before step 3 loads the block index and step 4 chooses the marker's new tip versus meta.tip. The before-any-pruning constraint is correct; the literal dependency order is not executable as written.

**Core:** `ReplayBlocks` looks up the indexed heads before computing the fork (`validation.cpp:4895–4912 / :5241–5266`). A1 §3.5.5 also loads the index first. ST3a §5.5 requires locks before activation/pruning, not before metadata/index reads.

**Fix:** open/barrier, read metadata and load index/file metadata, identify recovered G, then establish mandatory locks before any startup pruning or activation. Keep replay decision reads before replay writes.

### m3 — MINOR: VARINT does not preserve numeric lexicographic order

**Design:** `:155–156`.

Core VARINT(16383)=`fe7f`; VARINT(16512)=`808000`. The larger vout sorts first lexicographically. Dropping a constant 'C' prefix still preserves **Core's raw cursor order**, which is the required result, but describing that as numeric vout order risks a later “correction” that changes the UTXO hash.

**Core:** `txdb.cpp:53–59 / :62–68` uses VARINT for CoinEntry; cursor iteration `:217–265 / :226–274` follows the database's raw key order. `serialize.h::WriteVarInt` at both pins supplies this encoding.

**Fix:** say “identical serialized-key ordering to Core,” remove the numeric-order claim, and include a multi-byte boundary ordering vector in the cursor/hash tests.

## Contract audit and positive conclusions

The table below audits the actual statements behind §4.7, not just whether a mapping row exists.

| ST3a §5.6 item | Assessment |
|---|---|
| 1 Generic stores | Present: E/F and StateDelta/data interfaces; no async engine abstraction needed. |
| 2 Synchronous seams | Flush and recovery are steppable; subscriber open/event/sync operations are synchronous. Keep the real out-of-band scheduling state in the seam when fixing B1. |
| 3 Determinism | Fixed salt, producer/parallel writes off under the seam, deterministic arena iteration. The fallback explicitly fixes salt too. |
| 4 Marks | Required marks and provenance are assigned, including CH3a's `applied=false` for check-only paths via the binding contract. |
| 5 Scopes/barriers | Bk and recovery final have explicit completed pre-final sync_wal and Durable batches. B1's presumed-effect seq is captured only after V writes return. Sequential completed Bk/Bp covers PreviousFinalizer. Bp has no pending coin scope because one-batch B1 already returned Durable; its files barrier protects clears. No missing extra WAL barrier is required for that empty scope. |
| 6 Subscribers | m_synced classification, step-5 capture under lock, p/s distinction, submit-before-sync, capped cadence, Durable checkpoints/rewinds and LCA exception are present. **B1 and M3 prevent approval of the real mapping/progress claim.** |
| 7 Pruning | Pure planning addition is necessary: merged ST2's selectors really call prune_one_block_file. D is retained; trigger iff D nonempty; final D∩eligible does not rerun the target selector; rearming follows the contract. V clears, Bk/Bp durably records clears, unlink follows. Keep signed/clamped low-height lock arithmetic explicit: Core clamps last_prune to at least 1 (`validation.cpp:2787–2793 / :2995–3001`); the u32 API should not underflow at a null/start locator. |
| 8 Check profiles | CoinsSut/check_coins_profile and ChainSut/check_chainstate are assigned. |
| 9 Digest scope | Correct exclusions, including declared effects; not silently expanded to subscriber data. |
| 10 Standing extractor | Marker-new defines G; cross-store requirements are reevaluated at barriers. Locator paths, not individual data records, carry block requirements. Add real subscriber checkpoint state per M3. |
| 11 WAL serialization order | Returned-before-barrier-start is explicit; concurrent V writes are not presumed covered. |
| 12 Engine options | manual_wal_flush=false is set and asserted. |
| 13 Blind recovery | Coins replay uses puts/erases and no overwritten-key reads; height-range subscriber deletes are blind. Import is the right place for legacy metadata resolution, but needs M4's ancestry context. |

The head-blocks protocol matches the intended **stronger** replacement for Core: Durable index/evo/marker B1, Buffered coin chunks under subset semantics, completed sync_wal, Durable final tip/marker erase. Both pins' `txdb.cpp:104–165 / :113–174` supply the old/new marker and partial-batch behavior. The new durability order is intentionally not Core's unsynced coins→EvoDB order. Recovery's undo/replay operation order, genesis exclusion, normal activation above the recovered tip, -dbcrashratio during recovery, and zero-height refusal are consistent with the accepted contract, subject to normalized inputs.

The frozen layer correctly masks a partially written DB: every F-owned key, including tombstones, is consulted before DB. Clean F entries equal the old DB value and remain valid while only F's dirty keys are written. Active overrides F. This proves coin-value lookup consistency even with arbitrary surviving sub-batch subsets; it does not prove m1's membership claims. Sequential flushing and non-FRESH tombstones for spends of F-only coins are necessary and correctly specified. Flush empties/reallocates; Sync retains clean unspent entries and removes spent entries at both pins (`coins.cpp:254–282`). Not porting unused production Sync is acceptable under the stated non-goal.

The positive-only prefetch fence is sound under its stated invariant: snapshot generation capture is paired with the snapshot, active/F tombstones mask stale positives, release advances generation and clears the map, and disconnect clears it. Explicitly test a completion arriving after release and F-owned spent/recreated keys; producer snapshot/generation pairing must be one coherent operation in CH4. No need to cache absence.

Read errors are now represented as errors, cannot become validation outcomes, trip the global latch, and production abort matches `coins.cpp:355–370 / :356–379`. The v23 HaveCoin catch difference is an accepted CONS-22 deviation. Core DB write failures throw before the three boolean “Failed to …” paths, supporting the design's “System error while flushing” mapping (`dbwrapper.cpp:260–267 / :261–268`, `validation.cpp:2908 / :3117`). Profile-specific AbortNode status is explicitly preserved. File-sync fail-stop and directory synchronization are accepted robustness improvements.

## Memory and performance assessment

B(t)'s mempool allowance and p300 shares match both pinned calculations when optional Dash/filter indexes are off: txindex on gives **252.75 MiB coins**, txindex off **290 MiB**, and engine configured E is **74 MiB**. v24's extra index shares are correctly profile-gated. The arena's density remains a hypothesis, as DEC-77 demands, and the measured layout gate is appropriate. Fallback capacity 2.15M versus Core 2.20M is close, but is not a guarantee of no regression; the FRESH window and write volume tradeoff is real. M1/M2 must be resolved before the resource claims can be used as proof.

The route to “never slower” is a credible **measurement plan**, not yet performance evidence. Higher density and removing synchronous flush time can outweigh RocksDB miss cost, but releasing F can still empty the read cache when V made little progress; “at any time we hold more recent coins” (`:1148`) is overstated. Earlier freezes may increase DB churn, V still waits on the files mutex, shutdown drains two windows, and subscriber fsync/checkpoint work can become W's critical path. L3/L5/L8 are later protocol changes needing renewed crash checks, not free speedups. The design correctly treats 2× as an Amdahl hypothesis and leaves the remaining work to measured CH4/CS6/payee improvements.

PF0's micro/layout, replay, recovery, open and shutdown workloads are appropriately targeted; separate counter and threaded wall-time drivers are necessary. DEC-77 correctly moves the Core-model proxy to CH2c and the real dashd cell to M3. W-MC's multi-CF/effect/subscriber/read/reorg workload is useful. Add B1's already-past-new and shared-dispatcher schedules, M1's near-limit/single-block cases, and M3's cumulative-state subscriber. Reorg benchmarks should include a genuinely different branch, not only disconnect/reconnect of the same blocks. The end-to-end M3 gate remains essential: these traces omit script/evo computation and can only proxy cache/storage performance.

`flush-split` must assert **protocol barrier calls**, not an exact total of OS fsync syscalls while background compaction is enabled: SST/manifest creation and multi-CF flushes add variable syncs. Keep the OS counters as measured results with controlled workloads. Likewise `max_total_wal_size` is a flush trigger, not an unconditional hard bound on startup replay bytes; large batches/stalled flushes belong in the open workload.

## Validation performed

- `git show` extraction and side-by-side inspection of both Core pins: coins.cpp/h, txdb.cpp, validation.cpp, caches.cpp, dbwrapper.cpp, init.cpp, blockstorage.cpp, base.cpp, coinstatsindex.cpp, undo.h and coins_tests.cpp; extra pinned EvoDB/serialization/consensus citations as above.
- Existing merged code: `TMPDIR=/dev/shm/ch2-gpt-tmp CARGO_BUILD_JOBS=12 cargo test -p dashd-store --no-default-features` in the integration worktree: **35 passed, 1 pre-existing ignored, 0 failed**. This validates existing memory-engine/flat-file prerequisites; it does not test the unimplemented CH2 protocol or native RocksDB fail-stop paths.
- `/work/scratch/ch2-gpt/check_design.py`: deterministic subscriber fixed point, small-cache slot bound, branch-ambiguous legacy metadata witness, fixed-key subscriber-state witness and Core VARINT ordering counterexample. Output: `design-counterexamples.txt`.
- `/work/scratch/ch2-gpt/wbm_probe.cpp`, compiled with C++20 against the existing RocksDB 11.8.1 archive and local compression archives: native cache/WBM witness **74 MiB capacity, 80 MiB usage, 40 MiB memtable charge, ShouldStall=1**. Output: `wbm-probe.txt`.
- No CH2 implementation exists to run the proposed crash/oracle suites. The Python witnesses transcribe the specified rules and are design counterexamples, not claims of bugs in merged CH2 code. No dashd instances or chain datadirs were touched; no commits or pushes.

## Final verdict

**REWORK.** The durable batch/replay/prune ordering is substantially aligned with ST3a rev5 and DEC-77, but the threaded liveness proof fails on an explicit stable-chain schedule. Resolve B1 and M1–M4, then rerun the design review before implementing the crash-consistency core.
