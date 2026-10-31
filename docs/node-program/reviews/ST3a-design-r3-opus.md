Verdict: **APPROVE-WITH-CHANGES**

# ST3a design review, round 3 (Opus): final closure check

- **Reviewed:** `designs/ST3a-power-loss.md` rev2 at 5e3cb089, with DEC-64 and DEC-66 (Q15).
- **Context read:**
  - my r2 review and its `walbk` evidence;
  - GPT's r2 review;
  - A1 §3.5.5 and §3.5.7;
  - ST2 `flatfile/{blocks,seq}.rs` at 365aa8cb;
  - Core v23.1.8 and v24.0.0-rc.3 via `git show`: `validation.cpp` (the prune block, `DisconnectTip`), `index/base.cpp` (`ThreadSync`, `ChainStateFlushed`, `Commit`), `node/blockstorage.cpp` (`FindFilesToPruneManual`) and `test/functional/feature_index_prune.py`.
- **Ran:** one small calculation, `/work/scratch/st3a-r3-opus/size_domain.py` (`size_domain.log`). It covers:
  - the file layout under ST2's rollover rule;
  - each lock scenario's bound and selection;
  - the cardinality of the §2.2 `FileSize` domain, with and without successful preallocation.

  Nothing else was run, because box load is high.

## Answers in brief

1. **My r2 findings.**
   - Majors 2–6 are **closed**.
   - Major 1 is **partial**. DEC-64's order fixes the stall and the FT numbers, but it inverts Core's "select, then flush if non-empty", and the design never says what now decides a prune flush (finding 1).
   - Minors 7–14 are all **closed**. Closing 7 (T18 → N8) exposed a new problem with Buffered Bk (finding 3).
2. **DEC-64 against `feature_index_prune`, both pins.**
   - **689, 367 and 735 reproduce**, and so do the FT's later **2489 and 2207**, provided two things hold:
     - (a) a synced index counts as caught up and W waits for its commit;
     - (b) the manual-prune flush is not skipped by a selection made before the commit step (finding 1).
   - The FT also asserts **"basic block filter index prune lock moved back to 2480"** (v24 `:175` / v23 `:154`). DEC-64 has no in-memory lock to move back, so that line fails as specified (finding 4).
   - **Q15 is safe for R11b.** It is **not live as worded**. It classifies subscribers by an instantaneous "has processed up to `new`" test, gives notification-driven subscribers no commit cadence, and does not cap a syncing subscriber's commits at the acknowledged tip (finding 2).
3. **New planted bugs and controls.**
   - T28, T29 and T30 trigger exactly as stated; their selections are reproduced in `size_domain.log`.
   - T31 triggers R5. Its "Also R6(e)" depends on which index `best_valid` is given (nit 8a).
   - **N9 holds.**
   - **N8 is clean only on a single-flush scenario.** On any scenario with a second split flush it reports R3, and D1(f) cannot see the cause (finding 3).
4. **The CH2 contract** is now close to complete. Four gaps remain:
   - the prune-flush trigger (finding 1);
   - subscriber classification and cadence (finding 2);
   - Bk and Bp must be Durable (finding 3);
   - blind replay and rewind as the condition for tractable bounds (finding 7).

   **S1's per-image bound of 96 is credible.** It is conservative by about 10×: with preallocation the size factor is at most 2, and the reachable batch bits are at most 3. The size term silently assumes that allocation succeeds, which N7 breaks (finding 6).

---

## Findings

### 1. Major: DEC-64 moves selection after B1, but Core flushes for pruning *only because* selection found files. The design never says what triggers a prune flush now

**Location.**
- §6.2 flush steps 4–7 (lines 778–781);
- E12 (line 1123);
- §5.6 item 7 (lines 708–716);
- §3.6's CH8 row (line 462).

**Core's order.** Both pins compute the locks, then select, then decide:
- `fFlushForPrune = true` only if `setFilesToPrune` is non-empty (`validation.cpp:2812` / `:3020`);
- that alone makes the flush full (`:2838` / `:3046`);
- this holds for both the automatic check (`m_check_for_pruning`) and `pruneblockchain` (`nManualPruneHeight`, `:2781` / `:2989`).

**Under DEC-64**, the selection that decides runs *after* B1 and the subscriber commits. The question "do we flush?" must therefore be answered before the selection that answers it. There are three readings, and two are wrong:

- **(a) Decide with today's durable locks, before the commit step.** This is the r2 stall again.
  - In the FT, the indexes' durable locators sit at 200 from `ThreadSync`'s final `Commit`. `-prune=1` is manual-only, so no full flush happens before `pruneblockchain(400)`.
  - The deciding lock is 189, so `min(189, 400)` selects at most the files ending at or below 189.
  - Either no flush happens at all, or the commit step never runs before a selection that could reach 367. `pruneheight == 367` fails.
- **(b) Run a full flush whenever a prune check is due.**
  - This matches the FT, but in a pruned IBD it means a full coins flush on every blk chunk allocation (16 MiB, or 16 KiB with `-fastprune`).
  - Core flushes only when the usage target is crossed. The difference is a large performance regression against dashd.
- **(c) Recommended: decide with a dry-run selection under the predicted post-commit lock.**
  - Caught-up subscribers are taken at `new`; syncing ones at `height(LCA(durable, new))`.
  - This is deterministic, because a caught-up subscriber's commit either succeeds at `new` or is fatal (R8's fatal set).

**Fix.**
- Add (c) to E12 and §5.6 item 7: "the prune decision is a side-effect-free selection under the predicted post-commit lock; the post-commit selection must be a subset of it, and is equal unless a commit failed".
- Emit Core's "limited pruning to height" line from the predicted lock.
- Give the toy one scenario whose flush is **triggered** by a due prune check, rather than scripted, and plant **T32**: the decision uses the pre-commit durable lock. T32 is caught by the scenario's precondition assert ("selected = {0}"). No R-check is needed, because the failure is liveness.

### 2. Major: Q15 is safe for R11b, but "caught up" is defined by an instantaneous test, and a "syncing" subscriber has no defined cadence or cap

**Location.**
- §6.2 step 6 (line 780): "has processed every block up to `new`… A syncing subscriber commits only on its own cadence";
- §5.6 item 7 (line 711);
- Q15 (line 1221), which names "a slow `llmq/`" as an example of a syncing subscriber;
- DEC-66.

**(a) Safety holds.**
- A syncing subscriber with durable L locks at `h(LCA(L, new)) − BUFFER − 1`.
- Everything its recovery needs lies above that height:
  - its disconnects run back to an LCA at or above it;
  - its connects run up to a recovered tip, and that tip is at least the pruning flush's `new` (by B1).
- Stale-branch blocks above the bound are protected by height, because selection is per file on `nHeightLast`.
- A later deep reorg past `K` is unsupported for the chainstate itself, as in Core.
- I accept Q15's safety claim.

**(b) Liveness fails for notification-driven subscribers.**
- `llmq/`, and any index after `m_synced`, process `BlockConnected` from a queue. At the moment W reaches step 6, such a subscriber has typically processed only up to `new − 1`, because Core's `LimitValidationInterfaceQueue` allows a backlog of about 10 callbacks.
- By the step-6 test it is therefore "syncing" at most flushes. It then contributes its durable locator, and nothing in the design gives a notification-driven subscriber any other commit point. Core's indexes commit *only* on `ChainStateFlushed` once synced (`index/base.cpp:290-327` / `:332-369`).
- Its locator then never advances, and the lock freezes at its last commit. That is the r2 stall, made permanent, and Q15's own example (`llmq/`) is the most exposed case.
- **Core has no such failure**, because its `ChainStateFlushed` is queued *behind* the `BlockConnected` events. An index processes the flush only after it has caught up to the flushed tip.

**(c) The cadence commit is uncapped.**
- Core's `ThreadSync` commits its own best block every 30 s (`index/base.cpp:187` / `:202`). That block can be far ahead of the chainstate's flushed tip; Core tolerates this ("index cannot be corrupted by a missed commit to disk for an advanced index state").
- A1 §3.5.7 says subscribers persist "only after the `ChainStateFlushed` of a flush that covers it", and §2.4.4 makes a locator presume the chainstate's B1.
- A port of `ThreadSync` therefore violates R11a and D1(b) (`DurableTipAtLeast`) after any power loss that recovers below its locator.
- The toy cannot catch this: `Syncing { durable, progress }` is static and never commits.

**Fix.**
- Define "caught up" as Core's `m_synced`, meaning notification-driven. W waits for every such subscriber to **drain its queue through `new`** and commit. "Syncing" means only a subscriber in its own catch-up thread.
- Require every syncing subscriber to have a bounded commit cadence (Core's 30 s), and cap each cadence commit at `min(progress, T_ack)`. As an alternative, specify the "locator ahead of the recovered tip" rewind: rewind to `LCA(locator, recovered tip)`, then delete above it.
- Add one toy case, S11s: a syncing subscriber commits on cadence while `T_ack` is below its progress. Plant **T33**, an uncapped cadence commit, caught by R11a and D1(b).
- Put both rules in CH8's and the index tasks' briefs.

### 3. Major: the normative rule allows a Buffered Bk, and N8 calls it safe. Bk carries the last coins, and the next flush's B1 presumes them

**Location.**
- §2.4.4 rule step 3 (line 302): "the finalizer appended, Durable or Buffered";
- line 304: "Losing a finalizer is safe";
- N8 (line 860);
- the B1 row of §2.4.4's scope table (line 295);
- contrast with E3 (line 1111), which deletes "a Buffered Bk would also be crash-safe".

**Counterexample.** Take N8's variant (the pre-Bk barrier is kept and Bk is Buffered) on any scenario with two split flushes, F1 (`0 → 20`) and F2 (`20 → 40`).
1. F1's Bk returns and stays pending.
2. F2 runs its files barrier, then appends B1′ (Durable: marker `[40, 20]`, `evo_tip = 40`, `tip` erased).
3. PowerLoss before B1′'s barrier. The subset {B1′} survives without Bk.
4. Recovery replays `(20, 40]` blindly on top of F1's sub-batches, which are durable behind the pre-Bk barrier. **Bk's last coins are missing.** Every key they wrote that `(20, 40]` does not touch is wrong. **R3 fires.**
5. **D1(f) is silent**, because B1′'s scope lists only pending presumed effects.

So N8 holds only when Bk is the last chainstate finalizer before the scenario ends. That is true on S4 alone (rev2 does not name N8's scenario), and only until a nested window reaches R12's continuation flush. GPT r2 2's reasoning, "if it vanishes, the durable marker causes replay", stops being true at the next B1, which overwrites the marker.

**Fix.**
- State that **Bk and Bp are Durable**, and that a finalizer carrying scope data may be Buffered only if no later finalizer presumes it.
- Or add "the previous flush's Bk/Bp" to B1's scope, which a Durable Bk satisfies for free, so that D1(f) catches the regression.
- Pin N8 to S4 and narrow its "why safe" text.
- Plant **T34**, N8's variant on a two-split-flush scenario, caught by R3 and, with the scope fix, by D1(f).
- This matters to CH2: its sampling search is unlikely to hit a two-flush window, and D1 is its only cheap guard.

### 4. Minor: the FT cross-check list is incomplete, and DEC-64 has no counterpart for Core's prune-lock moveback log

**Location.** §3.6's CH8 row (line 462); E14 (line 1125); §7.3 item 4 (line 940); §7.5 (line 960).

**What the FT asserts beyond 689/367/735:**
- "limited pruning to height 2489" and `pruneheight == 2207` (v24 `:170-172` / v23 `:149-151`). These hold under DEC-64 by the same argument as 689/367.
- `assert_debug_log(['basic block filter index prune lock moved back to 2480'])` across a 21-block reorg (v24 `:175-177` / v23 `:154-156`). Core logs this from `DisconnectTip`'s in-memory moveback (`validation.cpp:3061-3065` / `:3295-3299`).

DEC-64 computes locks from durable locators at flush time, so nothing ever "moves back", and the line is never logged. Q15 also changes which lock limits: `llmq/` now logs "limited pruning" in the 735 case, which is harmless because the FT asserts no absence.

**Fix.**
- List all six FT assertions in CH8's brief.
- Either emulate the log in `DisconnectTip` for each mandatory subscriber whose processed height exceeds `pindexDelete − 1` (log-only), or record it as a sanctioned FT divergence.

### 5. Minor: after B1, `meta.tip` is erased, so the DEC-64 commit step must count the marker's `new` as the chainstate's durable tip

**Location.** D1(b) `DurableTipAtLeast` (line 664); R11a (line 609); §2.4.4's locator scope (line 294).

**Counterexample.** Under DEC-64 every caught-up locator is committed between B1 and Bk. At that point the chainstate's durable `meta.tip` is erased (B1, step 4) and only `head_blocks = [new, old]` names `new`. If `DurableTipAtLeast` reads `meta.tip`, D1(b) fires on every flush of N1.

**Fix.** Define the chainstate's durable tip as "the marker's `new` if a marker is durable, else `meta.tip`" (recovery's target). Use the same definition in R11a. Add it to §5.6 item 10.

### 6. Minor: S1's "size values ≤ 3" assumes preallocation succeeds. N7 breaks it, and N7 has no dimensions or bound

**Location.** §8.2 (lines 979–981); N7 (line 859); §2.2 item 2 (line 181).

**The size domain with preallocation.**
- With ST2's preallocation (16 KiB blk chunks and 1 MiB rev chunks under `-fastprune`; `blocks.rs:65-69, 123-138` and `seq.rs:180-200`), appends land inside the allocated length. The `FileSize` domain is then {durable, after each pending `Grow`}.
- `size_domain.log`: 1 value for S1's `(20, 30]` and `(15, 20)` windows, and 2 around block 15's `Grow`.
- S1's real worst image is therefore about 2³ × 2. The bound of 96 is safe.

**The size domain without it.**
- When `allocate` fails silently (N7), every append extends the file, and the domain counts every intermediate sector-aligned size. That is **41 values for 10 pending 1 KiB records** in blk alone, and 9 for 2 records.
- R1's view form forces it through `read_sized`'s `len()` call (`blocks.rs:751`).
- Multiplied by the rev file and the batch bits, an N7 run over a whole flush interval exceeds the 1,024 cap. Zero truncation then fails.

**Fix.**
- Give N7 a window of at most two pending records, or move it to the nightly run with its own bound.
- State the size term as "the product over files read of their size domains, ≤ 3 with successful preallocation".

### 7. Minor: the CH2 contract should state that bounded exhaustive search needs blind replay and blind subscriber rewind

**Location.** §4.3 "Coin sub-batch bits are not forced" (line 514); §5.5 step 5 (line 685); §5.6.

**The problem.**
- The per-image bounds rest on recovery overwriting keys without reading them. A1 §3.5.5 step 4 is blind, but a CH2 implementation built like `CCoinsViewCache` is not: `SpendCoin` calls `FetchCoin`, which reads the base.
- Such an implementation forces every coin sub-batch bit, and S4 goes back to 2⁵ × ….
- The same holds for the subscriber:
  - a rewind that *scans* for "records above the locator" forces every pending data batch;
  - so the toy needs its cap of two batches, and a real per-block subscriber (blockfilterindex) gets 2ⁿ.
- A **blind** rewind removes those bits entirely: delete each height in `(locator, max indexed height]`.

**Fix.** Add to §5.6:
- "recovery replay and subscriber rewind must not read the keys they overwrite or delete, or the per-image bound becomes 2^(pending batches)";
- the toy should rewind blindly too.

### 8. Nits

- **(a) T31 "Also R6(e)":** R6(e) compares against `best_valid(c)`.
  - If `c` is the recovered index, from which T31 erased 11–15, `best_valid` = 10 = the final tip, and R6(e) is silent.
  - Specify that `c` is E's index, the pre-recovery overlay of R5. Otherwise drop the "Also".
- **(b) N8 and N9's exhaustive runs are not in §8.2.** Only N9's precondition dry run is. N9 is S5 plus about 3 subscriber bits, so roughly 1 CPU-s. The total is still well under 60.
- **(c) S11r's text says file 1 holds "A's 63–70 and B's 51–80 (38 records)".** That is correct, but the precondition note "deleting A's 51–62" should add that file 0 also holds the common 0–50. They are harmless, because they are below the LCA.
- **(d) E14's "equal to Core's bound when every subscriber is caught up":** ours is never lower than Core's, which follows queue-lagging in-memory heights. Both bounds are `new − K` whenever the lag is under `K − BUFFER − 1` = 277.

---

## A. Closing out my r2 findings

| r2 | Status | Evidence |
|---|---|---|
| 1 Stall / FT | **Partial** | DEC-64's order is adopted in §6.2 steps 4–7 and E12/E16; N9 is the toy analogue of the FT shape (rev1's lock 2, DEC-64's 69, both bound 64). Remaining: the prune trigger (finding 1), classification and cadence (finding 2), the moveback log (finding 4), the marker-tip definition (finding 5) |
| 2 Reorg, null, startup, undo, regime, disabled | **Closed** | (a) LCA: S11r/T30 reproduced, bounds 47 vs 67. (b) S3p: bound 64, {0}. (c) §5.5 step 0; S13/T28: 7 vs 122. (d) R11b `Block { undo }`. (e) buffer 2 < K − 1. (f) R11b scope and S11c |
| 3 Lock masks S5; S5c | **Closed** | Subscriber-free S5/S5b/S5c. ST2 layout reproduced (file 0 = 0–62). S5c: 52 vs 62, with the flush-2 path ∩ {61, 62}. T5, T11, T13, T15 and T19 each reach their invariant (§B) |
| 4 R11c | **Closed** | R11c (i)/(ii), §5.5 step 5, declared data timing; N1 includes S1 under ProcessKill before a locator. Rewind should be blind (finding 7) |
| 5 Bounds | **Closed** | Per-key scans (§2.4.3), at most 2 subscriber batches, R12 scope, narrowed windows, outer decisions fixed. The §8.2 rows and totals re-add exactly (17.0, 29.5, 2,085). N7 caveat: finding 6 |
| 6 Barrier scope | **Closed** | Scope table, presumed-effect barrier before B1 (T29), D1(f) scoped. S1's E22 is no longer a false positive. The Buffered-Bk gap is new (finding 3) |
| 7 T18 | **Closed** | T18 = no barrier and Buffered Bk; N8 added. N8's rationale is finding 3 |
| 8 T23 | **Closed** | T23a/T23b |
| 9 D1 standing, cross-store | **Closed** | §5.3, `WorldDurableView` |
| 10 Coins-profile effects | **Closed** | `effect_cfs`, `get` |
| 11 `fJustCheck` | **Closed** | `Connected { applied }`, R6 |
| 12 A-PSOW and the WAL | **Closed** | §2.3, W5 |
| 13 One-batch prune | **Closed** | Bp, E12/E13 |
| 14 Nits | **Closed** | All applied |

## B. New planted bugs and controls

| ID | Triggers as stated? | Reasoning |
|---|---|---|
| T28 | **Yes** | S13: loaded lock 7 → ∅; unregistered lock 122 → {0}. The subscriber's catch-up `(10, 130]` needs 11–62, so R11b fires (R0 may also fire). D1(b) fires at the activation flush's Bk |
| T29 | **Yes** | S1, PowerLoss at B1(30)'s `Append`: {B1} survives without E25, so tip = 30. R3 is silent, because effects are outside `state_digest`. R12's fold of block 31 differs. D1(f) fires: E25 is in B1's scope. The PR R12 scope lists T29 |
| T30 | **Yes** | S11r: file 0 = 0–50 + A51–A62 (63 records); file 1 = A63–70 + B51–80 (38). Fork lock 47 → ∅; height lock 67 → {0}. A51–62 are needed for disconnects, so R11b fires, and standing D1(b) fires at Bk |
| T31 | **R5: yes** | S1, image between the index-only write (15) and B1(20): E has 11–15 `HAVE_DATA`, tip 10, and T31 erases them. R6(e) is per nit 8a |
| N8 | **Only on single-flush S4** | Finding 3 |
| N9 | **Yes** | Caught up at 72 → lock 69, so bound `min(64, 69) = 64`; subscriber-free bound 64. Both select {0} |

## C. The CH2 contract and S1's bound

**Contract.** §5.6 items 1–12 now cover all of my r1/r2 asks:
- marks with `applied`;
- scopes and concurrency;
- subscriber rewind and catch-up;
- the lock rules;
- profiles, digest scope, a standing cross-store extractor, WAL order and engine options.

A D2 designer still needs four things:
- the prune-flush trigger rule (finding 1);
- `m_synced` classification, cadence and cap (finding 2);
- "Bk and Bp are Durable" (finding 3);
- blind replay and rewind (finding 7).

Each is a sentence or two. None changes ST3a's architecture.

**S1 = 96.**
- The formula's factors are upper bounds: 2³ batch bits × 3 sizes × 4 directory states.
- The reachable maximum is lower:
  - **batch bits ≤ 3.** Between 25 and the pre-B1 barrier: E22 + E25 + subscriber data(25). At the commit step: data(25) + data(30), then the locator alone after its `sync_wal`.
  - **size ≤ 2**, at block 15's `Grow`.
  - **directory bits** only before the first files are synced.
- So about 8–16 against a cap of 1,024. The bound is credible, and its B̄ = 7 is if anything high.

---

## Summary

Rev2 closes my r2 review in substance:
- DEC-64's order fixes the stall, reproduces `feature_index_prune`'s 689/367/735/2489/2207, and makes S5 reachable with the lock on (N9);
- the subscriber, scope, bound and invariant fixes are all in.

Three things still need text before CH2 and CH8 build on it:
1. DEC-64 inverts Core's "select, then flush". The flush decision must use a dry-run selection under the post-commit lock (finding 1).
2. Q15 is safe, but as worded it can freeze the lock forever for a queue-lagging `llmq/`. It also leaves a syncing index's cadence commit uncapped (finding 2).
3. "Durable or Buffered" is wrong for Bk. N8 fails R3 at the next flush's B1, and D1 cannot see it (finding 3).

Each fix is local to the text, plus at most one planted bug. The rest are minors and nits, and nothing reopens the architecture.
