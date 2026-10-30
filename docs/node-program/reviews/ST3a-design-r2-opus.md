Verdict: **APPROVE-WITH-CHANGES**

# ST3a design review, round 2 (Opus): closure check

- **Reviewed:** `designs/ST3a-power-loss.md` rev1 at 06b4a92b (unchanged at 922fd8a7), with DEC-62 and DEC-63.
- **Context read:**
  - my r1 review and its evidence (`/work/scratch/st3a-review-opus/`);
  - GPT's r1 review;
  - A1 §2.10.1 and §3.4–§3.5;
  - PLAN row CH8;
  - ST2 `flatfile/blocks.rs` at 365aa8cb;
  - `rocks.rs` on `node/integration`;
  - the `librocksdb-sys` 0.19.0+11.8.1 sources;
  - the calibration program and logs in `/work/scratch/st3a-design/`;
  - Core v23.1.8 and v24.0.0-rc.3 via `git show`, including `test/functional/feature_index_prune.py`.
- **Ran:** one native experiment against the pinned RocksDB, in `/work/scratch/st3a-r2-opus/walbk/` (`src/main.rs`, `run.log`).

## Answers in brief

1. **The `sync_wal` claim is correct, and I withdraw my r1 fix ("make the locator Durable").**
   - A Durable commit is an append (`db_impl_write.cc:2343`) followed by a sync of every live WAL (`:2350-2387`).
   - The record it appends is **byte-identical** to a Buffered one. My run wrote Bk with `sync=true` and again with `sync=false`, and the 32,804-byte WALs compare equal.
   - So until the sync returns, a Durable record is just another pending record. Zeroing the Buffered coin sub-batch Bj in front of it and reopening gives `tip = new`, the marker erased and `coin_j` missing. The LOG says "Recovered to log #4 next seq #6" and reports no corruption. This is T26's state, silent, exactly as §2.4.4 says.
   - The rule is **sufficient for the three commits it names**:
     - `KvEngine::sync_wal` is `flush_wal(true)` (`rocks.rs:620`), which reaches `SyncWAL` → `SyncWalImpl(include_current_wal = true)` (`db_impl.cc:2019-2036`, `:2038`). That syncs every live WAL up to the current one.
     - The current WAL is synced *without flush* (`:2103-2106`). Completed writes are already in the page cache, because `manual_wal_flush` is off: `log_writer.cc:179-180` and `:264-268` flush after every record, and `rocks.rs` never sets it. Name this in A-DUR.
   - The rule is **not sufficient as stated** in one respect. B1 is also finalizing with respect to effects that the durable state presumes. Conversely, D1(f) is too broad for effects the state does not presume (finding 6).
2. **r1 majors:**
   - closed: 1, 2, 4 and 5;
   - partial: 3, 6 and 7.

   The table is in §A below.
3. **Planted bugs and negative controls:**
   - T14, T16, T17, T20, T21, T24, T25, T26 and T27 are caught as named.
   - Not caught as specified:
     - T15 (S5c's dimensions; the lock masks it);
     - T19, and the S5-family bugs T5, T11 and T13 (the lock masks them);
     - T18 (clean as written).
   - Caught, but with a wrong "Also" entry: T22 (needs R11c fixed) and T23.
   - **N1 fails as specified**, three ways:
     - R11c raises a false positive;
     - D1(f) raises a false positive on S1;
     - the subscriber's pending batches exceed the exhaustive bound.

   The table is in §B below.
4. **R11a–c / Q10 / Q10a: sound in intent, not yet in mechanism.**
   - The durable lock is necessarily at or below `old`. Combined with Core's flush triggers, this **stalls pruning** and breaks CH8's own FT `feature_index_prune` (finding 1).
   - It ignores reorgs and null locators (finding 2).
   - In the toy it masks the whole S5 family (finding 3).
   - On restart with a disabled subscriber, Core's InitError is the right outcome, but R11b's scope has to say so (finding 2(f)).
5. **Budget arithmetic:**
   - Every per-operation constant in §8.1 matches `calib.log` and `calib-debug-overrides.log`.
   - Every row of §8.2 and §8.4 reproduces from its own inputs. The one slip: S12 is 20 × 15 × 7 × 0.8 ms = 1.68 s, not 1.5.
   - The **branch inventory omits the subscriber's pending batches**. Under L1's range rule, these make S1, S5 and S11 exceed the 1,024 cap (finding 5).
6. **The CH2 contract covers my r1 7(a)–(f).** It still lacks:
   - subscriber recovery semantics (finding 4);
   - the prune-lock rules (findings 1–2);
   - a dependency scope for `finalizes` (finding 6);
   - a cross-store, standing D1 view (finding 9);
   - an effect accessor for the coins profile (finding 10);
   - the `fJustCheck` rule for `Mark::Connected` (finding 11).

---

## Findings

### 1. Major: A prune lock that follows durable locators always sits at or below `old`. With Core's flush triggers it stalls pruning and fails CH8's FT

**Location.**
- §6.2 flush step 3 (line 797);
- §5.6 item 6 (line 747);
- E14 (line 1203);
- Q10a (line 1279);
- DEC-62 (3).

**Why the lock lags.**
- A subscriber persists its locator only after a `ChainStateFlushed` (A1 §3.5.7; R11a).
- Core emits `ChainStateFlushed` only on a **full** flush: `if (full_flush_completed)`, `validation.cpp:2903` / `:3111`. The hourly index-only write does not emit it.
- So at a flush's step 3, every durable locator L satisfies L ≤ `old`, and the bound is at most `old − 11`.
- `llmq/` is always mandatory (DEC-63), so this holds on **every** node, not only "when a subscriber lags by more than 288 + 10" as E14 says.

**Why that stalls pruning.** Core flushes for pruning only when selection finds a file:
- `m_check_for_pruning` is set on each chunk allocation (`blockstorage.cpp:665, 689` / `:718, 742`);
- it is cleared after selection (`validation.cpp:2809` / `:3017`);
- `fFlushForPrune = true` only if `setFilesToPrune` is non-empty (`:2812` / `:3020`).

The cycle:
1. The lock forbids every file above `old − 11`, so nothing is selected.
2. Nothing selected means no full flush.
3. No full flush means no `ChainStateFlushed`.
4. No `ChainStateFlushed` means the locators do not move, so the lock does not move either.

Pruning stays stuck until some other trigger fires a full flush:
- `fCacheLarge` or `fCacheCritical`;
- the 24-hour `DATABASE_FLUSH_INTERVAL` (`:99` / `:106`).

In a pruned IBD with a large `-dbcache`, disk use then grows far past `-prune`. A synced node keeps up to 24 hours more blocks than dashd. Both put CH8's BG gate ("pruned `disk_total_gib` ≤ dashd") at risk.

**Counterexample: CH8's own FT** (`feature_index_prune.py:77-81` / `:94-98`).
1. The indexes finish their initial sync at height 200, and `ThreadSync`'s final `Commit` makes 200 durable.
2. The test then mines 500 blocks with no full flush (`-prune=1`, small cache), and calls `pruneblockchain(400)`.
3. Core's lock is the index's in-memory height 700, so the log says "limited pruning to height 689" and `pruneheight == 367`.
4. Under Q10 the lock is 200 − 11 = 189. Both assertions fail.

**Fix: needs a manager ruling, because it changes how DEC-62 (3) is applied.** Recommended option:
- Move prune selection **after B1**, and let mandatory subscribers commit their locators at `new` between B1 and selection.
- N3 already shows that `ChainStateFlushed` after B1 is safe: B1's durable marker guarantees that recovery reaches `new`.
- The durable lock is then about `new − 11`. That matches Core's in-memory lock, keeps DEC-62 (3)'s guarantee, and fixes the FT.
- It also simplifies the prune fix. B1 is written before selection, so its dirty entries *are* the pre-prune versions, and the first `sync_and_take_dirty_infos` call needs no partition.
- The cost is that W waits for the mandatory subscribers' commits: one `sync_wal` plus one locator write each.

The alternative is to keep the durable lock and accept the deviation. Then:
- correct E14;
- make prune checks force a full flush even when nothing is selected;
- list `feature_index_prune` as an expected FT divergence in CH8's brief.

### 2. Major: Q10's lock mechanism ignores reorgs, null locators and startup order. R11b omits undo

**Location.**
- §6.2 step 3;
- R11b (line 645);
- §5.5 (lines 728–734);
- E14;
- Q10a.

**(a) Reorgs.**
- Core moves every lock back in `DisconnectTip` (`validation.cpp:3061-3065` / `:3295-3299`). A durable locator on a stale branch does not move.
- Counterexample:
  1. The subscriber's durable L is at height 100 on branch A.
  2. A reorg to branch B forks at 80. That is 20 deep, more than `PRUNE_LOCK_BUFFER`.
  3. The flush at `new = 130` locks at 100 − 11 = 89, so files whose last height is ≤ 89 may go. They hold B's blocks 81–89 (the subscriber's connect path) and A's blocks 81–89 (its disconnect path).
  4. A crash before the subscriber's next commit fails R11b on the correct toy.
- **Fix:** lock at `height(LCA(L, new)) − PRUNE_LOCK_BUFFER − 1`. This mirrors Core's `FindFork` in `BaseIndex::Init` (`index/base.cpp:93` / `:105`).

**(b) Null locators.**
- Core's lock default (`height_first = max`) means "no lock" (`validation.cpp:2788` / `:2996`, `continue`).
- For `llmq/`, a null locator is exactly the first prune flush of a fresh pruned IBD, which is §7.2's practical trigger.
- Counterexample: that flush prunes freely. A crash after Bk and the unlink, but before `llmq/`'s first locator commit, leaves `llmq/` needing pruned blocks.
- **Fix:**
  - A mandatory subscriber with no durable locator locks at its start height.
  - Under the current mechanism, this makes finding 1's stall total until the first non-prune full flush. Under finding 1's recommended option, the null case disappears after the first B1.
  - Add S3 with pruning: `old` null and the subscriber null.

**(c) Startup order.**
- Locks must be loaded from the durable locators at open, **before** any flush of the epoch can prune. That includes recovery's activation, R12's flush, and the equivalent of Core's init `PruneAndFlush`.
- §5.5 does not say so.
- **Fix:** add it as step 0 of §5.5 and to CH8's brief. Plant **T28**: the lock is registered only when the subscriber starts, after activation.

**(d) Undo.**
- Core's coinstatsindex reads undo on both append and rewind (`index/coinstatsindex.cpp:123, 411` / `:129, 413`).
- **Fix:** R11b and D1(b) take each subscriber's declared needs (`Requirement::Block { undo }`), not only `HAVE_DATA`.

**(e) Toy regime.**
- With `K = 8` and `PRUNE_LOCK_BUFFER = 10`, the buffer always binds, because `new − 11 < new − 8`. In production, `MIN_BLOCKS_TO_KEEP` binds (288 > 11).
- So T16 and S11 test an inverted regime.
- **Fix:** scale the buffer with K, for example to 2.

**(f) A subscriber that was disabled.** Q10a says a disabled index holds no lock.
- The sequence: the index is enabled with locator L; at a restart it is disabled, and pruning passes L; at the next restart it is enabled again.
- Core then refuses with "best block of the index goes beyond pruned data…" (`index/base.cpp:109-110` / `:121-122`). That is the right outcome.
- **Fix:**
  - State that R11b covers only subscribers enabled in every epoch since their locator was written, and that the re-enable case is a sanctioned refusal, not an R0 violation.
  - Optionally add S11b: the subscriber is disabled for one epoch, and the expected result is the sanctioned message.
  - Note for Q10a: `llmq/`'s lock also protects every wallet that commits on each `ChainStateFlushed`, since a wallet's locator is at least `llmq/`'s. Only lagging wallets fall back to Core's init check.

### 3. Major: In the toy, the mandatory subscriber's lock masks the whole S5 family, and S5c cannot trigger T15

**Location.**
- §6.2 stores (line 790), step 3, S5/S5b/S5c (lines 822–824);
- T5, T11, T13, T15, T19;
- §7.3 item 3 (lines 970–975);
- §7.4.

**Counterexample.**
- In S5, coins last flushed at 5, so the subscriber's durable locator is at most 5.
- The lock is `max(1, min(70, 5 − 11)) = 1`, so the bound is `min(1, 70 − 8) = 1`.
- File 0's last height is about 62. A 1 KiB block plus its 8-byte header fits about 63 times in a 64 KiB `-fastprune` file, not 55. So file 0 is **never selected**.
- Consequences:
  - T13 and T19 have no prune records to misplace.
  - T5 and T11 have nothing to unlink.
  - T15 cannot change the bound.
  - N1 on S5 never exercises the prune protocol.
  - §7.3's "T13, T19 and T24 under ProcessKill prove it" cannot hold for T13 and T19.

**S5c independently.** Selection against V's tip prunes extra blocks only if a blk-file boundary lies in `(new, new + K]`. That interval is (70, 78], and file 1 spans about 63–125. T15's R0 also needs a **second** flush, whose B1′ marker needs the pruned blocks.

**Fix.** It depends on finding 1's ruling.
- **If the durable lock stays:**
  - Add a toy knob, `prune_lock: Durable | Off`. Off models "no lock binds": a reorg deeper than the buffer, or a null locator under Core semantics.
  - Run S5/S5b/S5c, T5, T11, T13, T15, T19 and T25 with Off, and N1 on S5 with both settings.
  - Add a control, S5 with the lock: the replay path is never selected. This documents that Q10 alone closes Q9 for shallow reorgs, and that E12/E13 are defence in depth.
- **If the locks reach `new` (finding 1's option):** the S5 window is live again with the lock on, so only re-dimension:
  - file 0's last height must be at or below `new − buffer − 1`;
  - S5c needs a file boundary in `(new, new + K]` and a following flush;
  - T11 is PowerLoss-only.

### 4. Major: R11c contradicts A1 §3.5.7's "data may be ahead of the locator". N1 fails it

**Location.** R11c (line 646); toy subscriber (line 810); §5.6.

**What A1 says.** Subscribers "rewind to that locator and re-apply `BlockConnected`… re-delivery is idempotent". `llmq/`'s records "can be ahead of the chainstate after a crash, exactly as in Core".

**Counterexample (S1, ProcessKill).**
1. The subscriber's data batches for blocks 11–20 have returned. Its locator is still 10.
2. A crash. ProcessKill keeps the data (§2.1).
3. R11c compares the data with the reference state at the locator, 10. They differ.

So N1 fails. PowerLoss does the same after the subscriber's `sync_wal` and before its locator commit.

**Fix.**
- Specify the toy subscriber's recovery: before catch-up, delete the records above its durable locator. The deletion must be idempotent, and a nested crash inside it must be explored.
- R11c checks two things:
  - after the rewind: data = the reference at the locator;
  - after catch-up: data = the reference at the recovered tip.
- T22 is still caught: the locator at `new` survives, a data batch is lost, and nothing re-applies it.
- `llmq/`'s event-driven records (IS marks, recsigs) stay outside R11c, as effects stay outside `state_digest`.
- State **when** the toy writes its per-block batches: on `BlockConnected`, as A1 says, or in a burst at `ChainStateFlushed`.

### 5. Major: The cell inventory omits the subscriber's pending batches, so exhaustive bounds and zero truncation cannot hold

**Location.**
- §2.4.3 "A scan resolves every batch touching its range" (line 243);
- L1 `iter_prefix` (line 494);
- §8.2 inventory (lines 1032–1035) and bounds.

**The problem.**
- `subscriber_digest` (R11c) and any recovery scan force **every** pending subscriber batch under the range rule.
- With per-block data batches, the image just before the subscriber's `sync_wal` has one pending batch per block in the flush interval:
  - S1: 10 batches, 2¹⁰ = 1,024. That already reaches the PR cap before multiplying by the chainstate's ≥ 6 branches.
  - S11 (K + 12 blocks): 2²⁰.
  - S5: 2⁶⁵.
- §8.2's bounds (≤ 24, ≤ 48) and its "zero truncated images" assertion therefore fail on N1.

**Fix.**
- **Resolve scans per key, newest first.**
  - This is exact. A scan's result depends, for each key, only on the newest surviving write. A key overwritten or deleted after the crash forces nothing.
  - It also removes most of the correct protocol's coin-bit forcing. Replay of `(fork, new]` erases every input and puts every output, which covers every key the sub-batches touched. So the coins digest forces bits only in buggy states such as T26. S4's B̄_P falls from 40 to a handful.
- **Bound the toy subscriber at two or fewer data batches per flush.** T22 needs only one.
- Add subscriber and effect bits to the inventory and recompute S1, S5 and S11.

**Smaller budget gaps.**
- **R12's PR scope.** §8.4's "R12 on every branch… ≈ 2×" implies the PR gate does not run R12 per branch. But §4.5's pseudo-code does, and T14 needs it. Say which.
- **Windows for witness searches.** Images before the trigger are explored exhaustively. With S4's whole window, T26 and T24 cost about 1 s each, not 0.05 s.
- **W1 on T27** is nested-exhaustive. One S4 outer image is about 41 outer branches × 240 nested branches × about 1 ms, roughly 10 s, against the 0.6 s budgeted. Fix the outer decisions to one branch.
- **Production-scale prune.** The branch cost of 5 ms is optimistic. R1's view form alone reads 330 × 3 KiB blocks plus their undo, about 3–5 ms in release. This is immaterial against 1.5 CPU-h.

### 6. Major: The barrier rule names three commits, but the dependency it encodes is wider (presumed effects) and narrower (D1(f)) than stated

**Location.**
- §2.4.4 (lines 264–278);
- E5 (line 1189);
- R13 (line 648);
- `RefExtractor::finalizes` (lines 702–703);
- D1(f) (line 715).

**(a) Presumed effects.**
- A1 §2.10.1 persists rotation snapshots as immediate Buffered single-row batches. Core's member computation silently takes another path when a previous cycle's snapshot is missing: `break`, then `BuildNewQuorumQuarterMembers` with empty quarters (`llmq/utils.cpp:496-503` at v24).
- Counterexample:
  1. A snapshot effect E for cycle base h is written at h + 3 and is still pending.
  2. B1 of a flush with `new` at the end of h's window is in flight.
  3. PowerLoss, with walbk's mechanism: B1 is kept and E is lost.
  4. Recovery replays coins only; it does not re-run `ProcessBlock`, so E is never re-stored.
  5. The next cycle's member computation finds h's snapshot missing and derives different members.
- Nothing in ST3a fires: R13 allows E's loss, and `state_digest` excludes effect column families.
- **Fix:**
  - State the rule generally: a barrier must precede any record whose correctness presumes an earlier Buffered batch, effects included.
  - CH7a classifies each `Effect` as independent or presumed.
  - Issue `sync_wal` before B1 whenever a presumed effect is pending. This is cheap, and A1's "no more durably than Core" must give way to US-NODE-04 here.
  - Give the toy a presumed effect that a later block's evo fold reads. Plant **T29**: no barrier before B1 while it is pending; caught by R12/R3 and D1(f).

**(b) D1(f) is too broad.**
- `finalizes() -> bool` means "presumes **every** earlier batch".
- Counterexample on N1, S1:
  - the effect at 22 is Buffered and pending at the flush at 30;
  - nothing between them is a barrier, since the index-only write is at 15;
  - S1's flushes are one-batch, so B1 writes `meta.tip` and erases `head_blocks` (A1 §3.5.4 step 3), which makes it finalizing by §2.4.4's definition;
  - so D1(f) flags the correct toy.
- In CH2, V's concurrent effect writes also land between the pre-Bk `sync_wal` and Bk.
- **Fix:** `finalizes` returns a dependency scope, such as the column families or key ranges it presumes, or "this flush's batches". Independent effects are excluded and presumed effects included.

### 7. Minor: T18 is clean as written

**Location.** T18 (line 854); §2.4.4 line 272.

**Why it is clean.** With the pre-Bk `sync_wal` kept, a Buffered Bk is safe. §2.4.4 itself says "can then be Durable or Buffered".
- If Bk is lost even after `Mark::Durable`, B1's durable marker makes recovery reach `new`, so R3 and R4 both hold.
- T11 remains a bug only because of the unlink.

**Fix:** define T18 as "Bk Buffered **and** no pre-Bk barrier", which is rev0's N4. Add "Also D1(f)". Or drop T18 as redundant with T26.

### 8. Minor: T23 conflates two bugs, and its "Also D1(a)" is wrong for the one that tests rule L2

**Location.** T23 (line 859).

**The two variants.**
- **(i) Tests L2.** The adopter reads only the 8-byte record header, marks the block `HAVE_DATA` without reading the body, runs `sync_data`, and commits.
  - The body's cells are carried symbolically, and R1's view form forces them.
  - D1(a) is silent, because the range *is* durable. That is the point.
- **(ii) Tests non-promotion.** Under ProcessKill→PowerLoss, the adopter commits without syncing. R1 and D1(a) both fire.
- An adopter that parses the whole record forces the cells by reading them, so it tests neither L2 nor non-promotion.

**Fix:** split T23 into T23a and T23b with these columns.

### 9. Minor: D1 needs a world-level durable view and standing requirements

**Location.** §5.3 `refs(…, view: &dyn DurableView)` (line 701); D1(b) (line 711); T16's "Also D1(b)".

**The gap.**
- A subscriber locator requires blocks in `(L, chainstate tip]`. That depends on another store's durable state, at a later time.
- T16's damage happens when Bk in **chainstate** clears entries that an earlier, durable locator in the **subscriber** store still needs.
- `refs` is evaluated once per committed record, against "the database as durable after this commit".

**Fix.**
- Requirements are standing. D1 re-checks every durable record's requirements after each completed barrier in **any** store.
- `DurableView` spans all stores.
- Put this in the CH2 extractor contract.

### 10. Minor: The coins profile lists R13, but nothing exposes effects

**Location.** `check_coins_profile` (line 688); `CoinsStateView`/`CoinsRef` (lines 663–681).

**The gap.** `effect_cfs` is on `ChainstateView` only, and `CoinsRef` has no effect reference.

**Fix:** add effect accessors to the coins traits, or drop R13 from the profile.

### 11. Minor: `Mark::Connected` must exclude `fJustCheck` connects

**Location.** §5.6 item 4 (line 741); R6(a).

**The gap.**
- The validating `connect_block` also runs for `TestBlockValidity` (templates, GBT proposals) and for VerifyDB level 4.
- Counterexample: a self-mined block H gets a `Connected` from its template check. A T20-style bug that writes B1 before H's real connect then passes R6(a).

**Fix:** emit `Connected` only for connects applied to the chain, or carry `just_check` and have R6 ignore those marks.

### 12. Minor: A-PSOW also covers the WAL

**Location.** §2.3 assumptions table, A-PSOW (line 208).

**The gap.**
- RocksDB packs records, so Bk's append shares a sector with the tail the barrier just synced.
- A torn read-modify-write corrupts that tail. PointInTime then stops on the checksum mismatch, before a **durable** batch.

**Fix:** state that A-PSOW's scope includes the WAL. ST3b's device-level check must cover it. No model change is needed.

### 13. Minor: The one-batch flush with pruning is unspecified

**Location.** §6.2 steps 4–7.

**The gap.** When B1 = Bk, the steps say B1 carries the pre-prune versions and Bk carries the clears. That is the same batch.

**Fix:** state that a one-batch flush has no marker, so its single batch carries the clears directly, and the unlink follows its return.

### 14. Nits

- **S12:** 20 × 15 × (6 + 1) × 0.8 ms = 1.68 s, not 1.5.
- **S5's "about 55 per file":** it is about 63 per file, which puts file 0's last height at about 62 = 70 − K, on the boundary (finding 3).
- **R1's `HAVE_UNDO` range** should include the 32-byte checksum trailer.
- **L1's list** should be marked non-exhaustive. `create`, `remove`, `write_all_at`, `truncate` and `copy` on an undecided binding force it, because each one's result depends on it. M10 would catch an omission, but the normative text should not read as complete.
- **§2.4.4 "flushes are minutes apart":** full flushes are up to 24 hours apart in steady state (`DATABASE_FLUSH_INTERVAL`). The cost argument only gets stronger.

---

## A. Closing out r1 majors 1–7

| r1 | Status | Evidence |
|---|---|---|
| 1 A-PREFIX | **Closed** | Subset is now the default (§2.4), with the `Append`/`Barrier` split, the native regression (§6.4), and W1/W2 re-derived; N2 and N4 became T17 and T18; errata E1–E11. My suggested Durable-locator fix was insufficient: walbk shows a sync record is indistinguishable from a buffered one, so I accept the designer's correction. T18's definition is finding 7 |
| 2 Process kill | **Closed** (text) | §0 item 6 and §7.2 are corrected, with both windows cited and T13/T24 run under ProcessKill. The *proof* through T13/T19 needs finding 3 |
| 3 Bk not sufficient | **Partial** | (c) `pruned_files` in Bk: closed. (d) the two-take partition: closed; V's dirty infos in the second take are synced by `sync_and_take_dirty_infos` (`blocks.rs:236-244`). (a) is partial: E12 and T15 are added, but S5c cannot trigger T15 (finding 3). (b) is partial: Q10 is adopted, but findings 1 and 2 remain |
| 4 T14 | **Closed** | R1's position-within-size check, D1(d), and R12 re-running every check. Nit: the undo trailer |
| 5 Lazy rule | **Closed** | L1–L6; M10 compares observation traces under mutations and nesting; M18. T23 needs splitting (finding 8) |
| 6 Budgets | **Partial** | The calibration matches the logs and the arithmetic reproduces, with minimisation moved to nightly, `K = 8` and Q11. Missing: subscriber bits, R12's scope, the windows (finding 5) |
| 7 CH2 contract | **Partial** | (a)–(f) are addressed: fixed salt, coins profile, digest scope, `Meta` keys, the V-ahead knob, `replayed()`. New gaps: findings 1, 2, 4, 6, 9, 10 and 11 |

## B. New planted bugs and controls

| ID | Caught as named? |
|---|---|
| T14 | Yes. R1 (position within size). D1(d) fires only in R12's continuation epoch |
| T15 | **No**: masked by the lock, and S5c has no file boundary in (70, 78] and no second flush (finding 3) |
| T16 | Yes, R11b, if S11 puts a file's last height in `(new − 20, new − 11]`. "Also D1(b)" needs finding 9. The regime is inverted at `K = 8` (finding 2(e)) |
| T17 | Yes, R3: B2 survives without B1 before the pre-Bk barrier |
| T18 | **No**: clean while the pre-Bk barrier remains (finding 7) |
| T19 | **No** in S5b with the lock. With it off: R0 and D1(b) at B1 |
| T20, T21 | Yes. Edge case in finding 11 |
| T22 | Yes, R11c and D1(f), once R11c is fixed (finding 4) |
| T23 | R1 view, yes. "Also D1(a)" is wrong for the L2 variant (finding 8) |
| T24 | Yes. A1 §3.5.5 step 2's check gives R0 ("Found EvoDB inconsistency"), plus R2 |
| T25 | Yes, R8 and D1(e), if S9's prune is dimensioned for the lock |
| T26 | Yes, R3 and D1(f); confirmed natively (walbk) |
| T27 | Yes, R7a (R3 also fires) and D1(f) |
| T5, T11, T13 (S5) | **Masked** by the lock (finding 3) |
| N1 | **Fails** as specified: R11c (finding 4), D1(f) on S1 (finding 6b), truncation (finding 5); and it does not exercise pruning on S5 |
| N3, N5, N7 | Clean |
| N6 | Clean, if S9 prunes under the lock |

---

## Summary

The core of rev1 is right:
- subset semantics;
- the `Append`/`Barrier` split;
- rule L;
- witness search separated from exhaustive search;
- the completed prune protocol.

The designer's §2.4.4 finding is correct, and the native run confirms it. My r1 Durable-locator fix was wrong, and the `sync_wal` rule is sufficient for Bk, recovery's final batch and subscriber locators.

Four of my seven r1 majors are closed; 3, 6 and 7 are partial.

What still needs work clusters around Q10 and subscribers:
- **Durable-locator locks always lag the flush.** With Core's flush triggers they stall pruning and fail CH8's `feature_index_prune`. This needs a ruling; I recommend moving selection after B1 with subscriber commits in between.
- **The lock ignores reorgs and null locators, and needs a load order** (finding 2).
- **In the toy, the lock masks every S5-family bug** (finding 3).
- **R11c contradicts A1's subscriber semantics** (finding 4).
- **The subscriber's pending batches break the exhaustive bounds** (finding 5).
- **The barrier rule needs a general dependency scope.** Rotation-snapshot effects need it, and it also removes D1(f)'s false positive (finding 6).

None of this changes ST3a's architecture.
