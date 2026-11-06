# ST3a: the power-loss model harness (`dashd-store::faulty`)

| | |
|---|---|
| Status | **Revision 5, narrow** (PLAN §4 row ST3a, design step D; review class **STATE**). Rulings DEC-59, DEC-62, DEC-63, DEC-64, DEC-66 and DEC-69 are applied. Every round-1 to round-5 finding is dispositioned in §13. No code until the design is approved |
| Date | 2026-10-08. rev0 at 483137a1; rev1 at 06b4a92b; rev2 at 5e3cb089; rev3 at b5322f02; rev4 at 16c91a64; rev5 after `reviews/ST3a-design-r5-gpt.md` (REWORK; items 2–5 closed) |
| Author | ST3a design agent (Opus 5.5) |
| Task | `FaultyEngine` (wraps the in-memory engine: unsynced loss and ENOSPC/EIO per call) and `FaultyFlatFiles` (unsynced pages survive or vanish adversarially; sizes revert); reopen with invariant checks; the seam that CH2's `coins_cache_model` plugs into. FR-STO-03; the model level of US-NODE-04 and NFR-OPS-01; M0 exit 7; M3 exit 2 (model level) |
| Binding inputs | A1 §2.8, §2.9, §2.10.1, §2.11, §3.4, **§3.5 (all)**, §3.11, §4.12–§4.13; PLAN §0, §1, §4 rows ST1, ST2, ST3a, ST3b, CH2, CH8, CS5a, §8.2 row 9; DEC-30 (N3, N5), **DEC-59, DEC-62, DEC-63, DEC-64, DEC-66, DEC-69**; BACKLOG BL-26, BL-29, BL-32, **BL-34**; `designs/ST1-bakeoff.md` §6a, §9.5; `designs/CS0-node-primitives.md` §9.3; the ST2 reviews; **`ST3a-design-r{1,2,3}-{gpt,opus}.md`, `ST3a-design-r{4,5}-gpt.md`** |
| Code read | `dashd-store` on `node/integration` (ST1 merge 583d0fbc). `dashd-store/src/flatfile/` on `node/st2-block-files` @ 365aa8cb, as an interface only. `node-fuzz` @ dd85ed13. `librocksdb-sys` 0.19.0+11.8.1 (RocksDB 11.8.1). Rust std `sys/fs/unix.rs` |
| Core | `~/workspace/dash`, read with `git show` at **v23.1.8** and **v24.0.0-rc.3**. Line numbers are written `v23 / v24` |
| Evidence | r1: `/work/scratch/st3a-review-gpt/`, `/work/scratch/st3a-review-opus/`. r2: `/work/scratch/st3a-r2-gpt/` (`wal_finalize*`, `wal_multi*`, `prune-sizing.log`, `closure-probes.log`) and `/work/scratch/st3a-r2-opus/walbk/` (`run.log`). r3: `/work/scratch/st3a-r3-gpt/` (`prune_probe.rs`, `protocol-checks.log`) and `/work/scratch/st3a-r3-opus/size_domain.log`. r4: `/work/scratch/st3a-r4-gpt/` (`closure_checks.py`, `automatic_prune_probe.rs`, `n7-size-domain.log`). r5: `/work/scratch/st3a-r5-gpt/` (`closure_checks.py` with the submit-before-sync fix control). Calibration: `/work/scratch/st3a-design/calib/`, `calib.log`, `calib-debug-overrides.log` |

---

## Changes in rev5

This narrow revision answers GPT r5 (`reviews/ST3a-design-r5-gpt.md`, evidence in `/work/scratch/st3a-r5-gpt/`). Items 2–5 of r4 are closed there. §13.8 has the dispositions.

1. **A locator never certifies unsubmitted data** (GPT r5, new major).
   - **The hole.** rev4's checkpoint ran only `sync_wal` and then the Durable locator. The toy batches subscriber data, so in S11d (L = 0, p = 96, `new` = 98) the half-interval batch can cover only heights through 49 while 50–96 are still staged in memory. The checkpoint then makes locator 96 durable over data that stops at 49. A crash before or after it returns breaks R11c(i), and the blind rewind of `(96, 192]` cannot recreate 50–96.
   - **The fix.** Every locator commit at X (commit step, checkpoint, cadence or rewind) first **submits all staged data through X** and waits for that batch to return. Only then does its `sync_wal` start, and only after that is the locator appended. This is the pre-locator scope rule of §2.4.4, now stated over staged data as well as pending batches.
   - **Two tips.** The subscriber tracks its processed tip p and its **submitted-data tip s**, the highest block whose data is in a returned batch. Data in `(s, p]` lives only in memory and is lost on any crash. A locator commit at X requires X to be an ancestor of s, or equal to it.
   - **Batching.** The rules gain one batch per checkpoint (§6.2), and the budget is updated.
   - **Coverage.** S11d (b = 1, 2) has named crash points with staged data outstanding, before and after the checkpoint returns, with s and p asserted separately. New planted bug **T41**: a checkpoint without the submit, caught by R11c(i) under both crash kinds. D1 is silent on T41, because staged data was never submitted, so no pending batch is in the locator's scope.
2. **Nit:** §7.3 now says the prune *cause* matches Core's `fFlushForPrune` (`flush_for_prune` iff the selection is non-empty). Other causes still force a full flush.
3. **Counts and budgets.**
   - 50 planted bugs (P1–P8; T1–T41 with T23a/b).
   - PR gate ≈ 43.8 CPU-s (budget 60); nightly ≈ 0.65 CPU-h (1.5 approved; doubled, ≈ 1.29).

## Changes in rev4 (summary)

- **The `D_max` cycle** (GPT r4 1): a Durable checkpoint at the subscriber's processed tip during the path-based step-6 drain after B1. Each checkpoint raises L by `D_max`, at most `⌈(h(new) − h(F))/D_max⌉` of them (S11d, FA6, T37, T38).
- **Prune selection** (GPT r4 2): the final selection is the retained dry-run set restricted to files still eligible, `D ∩ eligible(actual)`, with a re-arm on shortfall (`prune_selection_protocol`, S14b, T39, T40).
- **FA1/E20** test the prune cause (`flush_for_prune ⇔ D ≠ ∅`) separately from the other full-flush causes (GPT r4 3).
- **DEC-69's `LCA(p, new)` exception** is carried through E12, E14, §0, §7.3 and the liveness paragraph (GPT r4 4).
- **N7** uses the real size product, 72 (GPT r4 5).

## Changes in rev3 (summary)

- **Requirements:** subscriber block requirements come only from the durable locator's path to G (the marker's `new`, else `meta.tip`); data records carry none (GPT r3 6).
- **W1** keeps T17's failure (GPT r3 7).
- **Prune decision:** a dry run under the predicted post-commit lock decides the prune flush (S14, T32).
- **Subscriber classes:** synced or syncing, as `m_synced`; synced subscribers drain to `new`; syncing ones commit on a cadence capped at `LCA(progress, T_ack)` (S11s, S11q, T33, T35, FA1–FA5).
- **Durable finalizers:** Bk, Bp and recovery's final batch are Durable; B1 presumes the previous finalizer (S4b, T34).
- **Blind replay and rewind** (contract item 13, Q16 → BL-38).
- **Beyond the rulings** (accepted in DEC-69): `sync_wal` at open; the Durable rewind commit (T36); `D_max` backpressure.
- **Minors:** the moveback log emulation and all six FT assertions; R6(e) on E's index; N7's window; S1's bound.

## Changes in rev2 (summary)

- **DEC-64's order**: B1 → `ChainStateFlushed` → subscriber commits → selection → Bk/Bp. Locks are taken at the LCA, never null after the commit step, loaded at open, and protect undo.
- **R11c** with rewind and catch-up.
- **Subscriber-free prune scenarios** with real ST2 boundaries and asserted preconditions.
- **T18** redefined, plus N8.
- **R5** against the WAL-order overlay.
- **Barrier scopes**, presumed effects, and the concurrent-barrier rule.
- `manual_wal_flush = false` in the contract.
- **Per-key scans** and recomputed bounds.
- **T23a/b**, `Connected { applied }`, coins-profile effect accessors, and standing cross-store D1.
- **Budgets recomputed.**

## Changes in rev1 (summary)

- **Subset semantics** for the engine. A Durable write is an `Append` event followed by a `Barrier` event. The native regression pins RocksDB 11.8.1's non-prefix recovery.
- **The barrier-before-finalizing-commit rule** (DEC-63 Q14).
- **A wider filesystem domain**: independent directory operations, truncation and zero-fill as data history, and intermediate sizes.
- **Rule L** for lazy cells.
- **The completed prune fix.**
- **Corrected Core process-kill claims.**
- **Invariant fixes**: R1 against the recorded file size, R6 provenance, R7a/b, R8 scope.
- **Exhaustive versus witness search.**
- **Fixed-point minimisation.**
- **Calibrated budgets.**
- **macOS additions on BL-32.**

---

## 0. Summary

1. **What ST3a builds.** A deterministic, in-memory model of the node's two kinds of storage, plus a harness that crashes a system under test (SUT) at every store call, reopens it and checks it:
   - **`FaultyEngine`** implements ST1's `KvEngine`. After a power loss, **any subset** of the Buffered batches since the last completed barrier survives, and each batch is atomic. This is what the pinned RocksDB does (BL-34, DEC-62).
   - **`FaultyFlatFiles`** implements ST2's `FlatFs`. Each unsynced sector holds a temporal prefix of its history, sizes revert, and directory operations survive independently while renames stay atomic.
   - Both inject ENOSPC/EIO per call, with unknown outcomes.
2. **At least as adversarial as ext4, XFS, btrfs and APFS** (§2.3). Five assumptions are granted by name, each with a witness test and an ST3b owner: A-DEV, A-PSOW (now covering the WAL), A-REN, A-ATOMIC and A-DUR (now requiring `manual_wal_flush = false`).
3. **Schedules** (§4).
   - Crash points are mutating store calls; a Durable write is two, its `Append` and its `Barrier`.
   - Crash-only schedules use copy-on-write capture in one forward run; fault schedules re-run the workload once per fault.
   - Post-crash state is lazy cells under rule L, and scans resolve per key, newest first.
   - Clean scenarios are explored exhaustively within computed bounds. Planted bugs use a labelled witness search.
   - Failures shrink to replayable text repros.
4. **The protocol the toy implements**: A1 §3.5 plus errata E1–E22.
   - The barrier-before-finalizing-commit rule, with explicit dependency scopes, including presumed effects and the previous Bk before B1. **Bk and Bp are Durable**, and recovery syncs each engine's WAL before its first write.
   - DEC-64's order: B1 → subscriber commits → selection → Bk.
   - The prune flush is decided by a dry-run selection under the predicted post-commit lock (`flush_for_prune ⇔ dry run ≠ ∅`); the final selection is the dry-run set restricted to files still eligible, and the check re-arms on a shortfall.
   - Pre-prune versions in B1; clears in Bk or Bp; `pruned_files` by Bk.
   - Prune locks from durable locators at the fork point.
   - Synced subscribers drain to `new` along the path and commit, checkpointing Durably at their `D_max` limit after B1; every locator commit first submits the staged data through its target; syncing ones commit on a cadence capped at `LCA(progress, T_ack)`; a Durable rewind commit precedes processing below the locator.
   - Replay and subscriber rewind are blind.
5. **Checks after reopening** (§5). R0–R13 restate A1's I1–I5, with R5 against the overlay and R11 with rewind semantics. The standing, cross-store D1 checker flags ordering errors at commit time. All are written once against `ChainstateView`, with a coins-only profile for CH2's cache model.
6. **Acceptance** (§6). Every prune scenario asserts its preconditions before searching, and the forward run asserts the liveness properties FA1–FA6.
   - 50 planted bugs: MicroLog's P1–P8, including the PLAN's commit-before-fsync and index-before-data, and ToyChain's T1–T41, with T23 split in two.
   - 7 negative controls.
   - 18 model self-tests, 5 assumption witnesses, and the native RocksDB regression.
7. **Core** (§7). Both pins flush in the same order. Core survives a SIGKILL only at `feature_dbcrash`'s sub-batch boundaries; it loses in the prune index window and the coins→EvoDB window, and a power loss can force a reindex. Our protocol survives all of these. Core's prune locks follow queue-lagging in-memory heights, and ours follow durable locators after a commit step:
   - with every mandatory subscriber synced, on a stable chain (no reorg below `new` during the flush; DEC-69), our bound is never lower than Core's, and equal whenever the queue lag is under `K − BUFFER − 1`;
   - while a subscriber is still syncing, ours is stricter. CH8 cross-checks all six `feature_index_prune` assertions.
8. **Cost** (§8): PR gate ≈ 43.8 CPU-s (budget 60); nightly ≈ 0.65 CPU-h (1.5 approved); 20–25 agent-days (DEC-63).

---

## 1. Scope and non-goals

### 1.1 In scope (PLAN row ST3a)

| PLAN text | Where |
|---|---|
| `FaultyEngine` wraps the in-memory engine | §2.4, §3.3 |
| models unsynced loss | §2.4 (subset semantics, DEC-62; the PLAN's "as a WAL prefix" is superseded) |
| injects ENOSPC/EIO per call | §4.4 |
| `FaultyFlatFiles`: unsynced pages survive or vanish adversarially; sizes revert | §2.2 |
| reopen + invariant checks | §5 |
| integration into `coins_cache_model` | §5.6. CH2 owns that target. ST3a ships the traits, the coins-only check profile, the crash-op decoder, the reference SUT and the scenario templates |
| acceptance: planted ordering bugs (a commit before fsync; an index before data) found in a toy flush; self-tests | §6 |

- **PRD:** FR-STO-03 (the fsync policy as modelled is §2.6; A1 §3.5 plus §12's errata stay normative); US-NODE-04 and NFR-OPS-01 at model level.
- **Milestones:** M0 exit 7 closes at checkpoint 1 (DEC-63). M3 exit 2 is the model level on CH2's chainstate.

### 1.2 Non-goals

- **The real engine and real filesystems:** ST3b's. §6.4's native regression pins the engine behaviour the reviews disproved.
- **Engine-side prefix enforcement:** an ST1 option under BL-34.
- **Thread interleavings:** CH2's model fuzz. CH2 must expose synchronous seams (§5.6).
- **Corruption of durable data** (A-DEV). EIO on reads is injected.
- **Core's LevelDB stack:** analysed in §7, not emulated.
- **Windows.**
- **Process-kill smoke tests against binaries:** ST3b and CH2.

---

## 2. The storage semantics modelled

### 2.1 Two crash kinds

| Kind | What the next epoch sees | Real cause |
|---|---|---|
| **ProcessKill** | Every completed `pwrite`, size change and directory operation, and every returned `KvEngine::write`. All of it stays **pending and volatile**: a ProcessKill never promotes the page cache to durability, so a later PowerLoss can still lose it and only a later barrier makes it durable. Data whose sync failed, and an engine with a latched failure, stay in an unknown state. A multi-page `pwrite` interrupted by `SIGKILL` keeps a page prefix | SIGKILL, panic, OOM kill, `-dbcrashratio` |
| **PowerLoss** | The durable image, plus an adversarial selection of the pending state, resolved lazily (§4.3). What survives becomes the next epoch's durable base | power cut, VM kill, kernel panic |

### 2.2 `FaultyFlatFiles`: the filesystem model

**State.** A tree of directories and inodes under `/dd`:

```text
Inode  { durable: Image, cur: Image, hist: Vec<DataOp>, poisoned: BTreeSet<seq> }
Image  { len: Cell<u64>, sectors: BTreeMap<u64, Cell<[u8; 512]>> }   // sparse; cells may be symbolic (§4.3)
DataOp = Write { seq, off, bytes } | Shrink { seq, len } | Grow { seq, len } | Zero { seq, off, len }
Dir    { durable: BTreeMap<Name, Entry>, cur: BTreeMap<Name, Entry>, ops: Vec<DirOp> }
DirOp  { seq, bit: VarId, changes: Vec<(Name, Option<Entry>)> }        // a rename is ONE op changing two names
```

**Operations.** One per `FlatFs` method; there are 14.

| Call | Effect on `cur` | History |
|---|---|---|
| `create(p)` | bind a new empty inode if absent | `DirOp` |
| `write_all_at(p, off, d)` | write; zero-fill a gap past the end | `Write` (+ `Grow`) |
| `allocate(p, off, n)` | advisory; `len = max(len, off+n)` if capacity allows | `Grow`; in `AllocateMode::ZeroFill` also `Zero` (ST2 `OsFs`'s fallback) |
| `truncate(p, n)` | `len = n` | `Shrink` or `Grow` |
| `sync_data(p)` | `durable := cur` (data and size); poisoned writes excepted | (cells carried, L2) |
| `sync_dir(d)` | the directory's ops become durable | (bits carried, L2) |
| `copy(a, b)` | `b` bound or truncated to 0 (as `OsFs::copy`), then `a`'s content written | `DirOp` + `Shrink(0)` + `Write` carrying `a`'s cells |
| `rename(a, b)` | one `DirOp`; same-directory only (all ST2 does) | `DirOp` |
| `remove(p)` | unbind; `Ok(false)` if absent | `DirOp` |
| `create_dir_all(d)` | bind missing directories | `DirOp`s |
| `read_exact_at`, `len`, `link_count` | reads | (observations) |
| `available_space(d)` | capacity − Σ maximum possible sizes | (not an observation) |

`link_external(existing, new)` adds a durable second name in another directory, for R10.

**What a sync covers.**
- `sync_data` covers one inode's data and size. On Linux `fdatasync` also persists an `ftruncate`'s size.
- `sync_dir` covers one directory's operations.
- Nothing else.

**The post-crash image (PowerLoss, `Realistic`).** Independent lazy cells:
1. **Directory operations:** one survival bit per pending `DirOp`. A name's binding is its newest surviving op, so a rename is atomic (A-REN), and btrfs tree-log or ext4 `fast_commit` states such as "`b` without the earlier `a`" are reachable.
2. **File size:** one cell per inode, over the durable size, the size after each pending op, and every intermediate sector-aligned size inside an extending `Write` (delalloc write-back).
3. **Data per 512-byte sector:** a temporal prefix `j ∈ 0..=m` of the sector's history over its durable content.
   - A `Shrink` zeroes the tail of its sector, so truncate-then-re-extend reads zeros.
   - Sectors are independent, so pages tear at sector boundaries and write back in any order.
4. **Poisoned data** stays pending for good.

**`Extended`** adds:
- a garbage version;
- byte granularity;
- any page-aligned size in range;
- deferred ENOSPC/EIO at `sync_data`;
- EIO on reading an unsynced sector;
- reads after a failed sync that return either the new or the old bytes.

**`Witness`** (§6.5) breaks one assumption, or grants one bonus, at a time.

**2.2.5 Sync failures (fsyncgate).** A failed `sync_data` poisons the inode's pending writes. A later sync returns `Ok` with no effect. The poisoned writes stay cells at every later crash. Retrying and continuing (P5) is caught.

**Capacity.** Optional, shared with the engines if configured. After a crash, the image boots with a fresh `WorldConfig`.

### 2.3 Why the model is at least as adversarial as real filesystems

| # | Property | POSIX | ext4 (`data=ordered`), XFS, btrfs | APFS | Model `Realistic` |
|---|---|---|---|---|---|
| 1 | Data durable after `fsync`/`fdatasync` | yes | yes (flush/FUA when barriers are on) | **only with `F_FULLFSYNC`** | only `sync_data` |
| 2 | Size persisted by `fdatasync` | yes | yes, including `ftruncate` | with `F_FULLFSYNC` | yes |
| 3 | A new file's entry persisted by fsync of the file | no | **bonus** | varies | **no** (W4 grants it) |
| 4 | fsync of one file persists others' metadata | no | **bonus** | — | **no** |
| 5 | Directory-op order after a crash | unspecified | journal prefix; **btrfs tree-log and ext4 `fast_commit`: an fsynced file's entry without earlier ops** | journalled | each op independent; renames atomic |
| 6 | Data write-back order | none | none for overwrites | none | per-sector temporal prefixes |
| 7 | Torn writes | unspecified | sector-atomic on most devices | same | per sector; `Extended` per byte, plus garbage |
| 8 | Size after a crash | unspecified | a journalled size, or a delalloc point | journalled | every held size plus intermediate sector-aligned sizes |
| 9 | Truncate, then re-extend | — | zeros | zeros | `Shrink` zeroes in history |
| 10 | `posix_fallocate` | — | revertible size; unwritten extents read as zero | — | `Grow`; `ZeroFill` mode |
| 11 | `unlink` durability | needs a directory fsync | same | same | `sync_dir`; otherwise the file can reappear |
| 12 | `rename` crash-atomic | not specified | journalled | atomic | **A-REN**; W3 breaks it |
| 13 | Replace-via-rename flushes data | no | **bonus** (`auto_da_alloc`) | — | **no** |
| 14 | After a failed fsync | a retry reports or persists | **weaker than POSIX**: pages marked clean (fsyncgate); `errseq_t` since 4.13 | — | poisoned for good |
| 15 | ENOSPC timing | at `write` | at `write`; at write-back on thin devices and NFS | — | at `write`; `Extended` at `sync_data` |
| 16 | Synced data unchanged; an unsynced write confined to its range | assumed | assumed if the device honours flush/FUA and writes sectors atomically | with `F_FULLFSYNC` | **assumed** (A-DEV, A-PSOW) |

**Where real filesystems are weaker than POSIX:**
- failed fsync (row 14; fsyncgate 2018; Rebello et al., ATC 2020);
- macOS `fsync` (row 1; BL-32 for RocksDB);
- disabled or ignored flushes (Zheng et al., FAST 2013): A-DEV;
- `ext4 data=writeback`: the `Extended` garbage version;
- tmpfs, FUSE and NFS treating `EINVAL` as success (Core's `FileCommit` `util/system.cpp:1234` / `util/fs_helpers.cpp:140`; ST2 `einval_is_ok`): no durability at all. This is documented, not modelled.

Applications rely on filesystem bonuses (Pillai et al., OSDI 2014; Mohan et al., OSDI 2018), which is why rows 3, 4 and 13 are refused. W4 grants row 3 and hides P3.

**Assumptions granted beyond "only completed barriers make durable":**

| ID | Assumption | Granted because | Witness | Validated by |
|---|---|---|---|---|
| A-DEV | Synced data is never lost or changed; no misdirected or lost writes after a flush | A1 §3.5.1 | — | ST3b (VM `cache=none`, `dm-log-writes` at FUA) |
| A-PSOW | An unsynced write never changes bytes outside its range. This covers the **WAL** too: RocksDB packs records, so Bk's append shares a sector with the tail the barrier just synced (Opus r2 12) | ST2's byte-identical blk/rev layout cannot pad, and the WAL packs | W5 | ST3b, as far as `dm-log-writes` shows it (ST3b-D23, D24): states that keep or lose whole bios, and split states that keep one seeded 512 B-sector subset (first k sectors, or a random subset) of one unflushed write (r1; the harness now splits in 4 KiB device units, ST3b-D24), on ext4 (`data=ordered`) and XFS over loop devices; no such state showed a WAL with lost bytes before kept ones, and no split state changed a WAL file's visible bytes, 17 of 256 of them by tearing a WAL write. Not witnessed: tearing inside one sector, other filesystems; §6.4's layouts as targeted `dm-log-writes` cases are still owed |
| A-REN | `rename` is crash-atomic | journalled on ext4/XFS/btrfs/APFS; ST2's `make_private` relies on it | W3 | ST3b |
| A-ATOMIC | A batch is all or nothing across column families after recovery, including a Durable batch in flight | WAL record atomicity + `atomic_flush = true` (`rocks.rs:163`) | W2 | ST3b |
| A-DUR | A **completed** `Commit::Durable`, `sync_wal` or `flush_memtables` makes that batch and every batch that **returned before the barrier started** durable. Requires `manual_wal_flush = false`, so each record is handed to the OS on write (`log_writer.cc:179-180`, `:264-268`) | `WriteOptions.sync` (`rocks.rs:615`) syncs every live WAL (`db_impl_write.cc:2350-2387`); `sync_wal` = `flush_wal(true)` (`rocks.rs:620`) → `SyncWalImpl(include_current_wal = true)` (`db_impl.cc:2038`). ST1 must **set and assert** `manual_wal_flush = false` (today it relies on the default; BL-34 note) | — | ST3b |
| ~~A-PREFIX~~ | — | **withdrawn** (BL-34, DEC-62) | W1 grants it | — |

### 2.4 `FaultyEngine`: subset semantics

#### 2.4.1 The contract (DEC-62 (1))

- Between completed barriers, Buffered batches survive a power loss as an **arbitrary subset**.
- Each batch is atomic, and a group commit is one atomic record (`db_impl_write.cc:2233-2245`; a sync follower never joins a non-sync leader, `write_thread.cc:510-512`). The model treats every batch independently, which is a superset of the group-atomic states.
- A Durable commit makes itself, and every batch that **returned before its barrier started**, durable **once it returns** (A-DUR). Until then it is one more pending batch.
- **Concurrency (normative; GPT r2 5).** A barrier covers only the writes that had returned before it started. RocksDB lets a `SyncWAL` finish before a concurrent write it raced with (`db_wal_test.cc:346-370`, `SyncWALNotWaitWrite`). So WAL serialization order alone never proves coverage. §2.4.4's rule therefore requires an explicit returned-before-started order.

#### 2.4.2 Why: RocksDB 11.8.1 is not prefix-recovering

The mechanism:
- A zero-type, zero-length header is skipped silently (`db/log_reader.cc:611-619`).
- Outside a fragmented record, `kBadRecord` just breaks out of the switch (`:298-304`).
- `MaybeReviseStopReplayForCorruption` only ever clears the stop flag (`db_impl_open.cc:1613-1628`).
- The WAL is preallocated (`SetPreallocationBlockSize`, `:2465`).

So a zeroed record region at a block start drops that batch and replays the later ones. Reproductions:
- GPT r1, `wal_probe`, all four recovery modes;
- Opus r1, `walhole`, `b1, b3, b4` without `b2`.

Round 2 confirmed the finalizer case natively:
- GPT stopped a `sync=true` finalizer at WAL `Sync()` entry. With the earlier coin record zeroed, recovery gave `tip = new` with the marker and the coin both gone. With a completed `FlushWAL(true)` first, both outcomes kept the dependencies. Eight concurrent writers, merged into one 16-operation record over five WALs, all recovered behind the barrier (`wal_finalize*`, `wal_multi*`).
- Opus showed that a Durable record is **byte-identical** to a Buffered one until its sync returns (`walbk`).

A real checksum mismatch, by contrast, stops PointInTime at a prefix.

#### 2.4.3 The model

- **Pending.** Every Buffered batch since the last completed barrier, and every Durable batch whose barrier has not completed, is pending. After a PowerLoss each pending batch has a survival bit (a lazy cell).
- **Point reads** resolve, newest first, the bits of the pending batches that wrote the key, stopping at the first survivor.
- **Scans resolve per key, newest first** (Opus r2 5). A scan's result depends, for each key, only on the newest surviving write to it. A key the post-crash epoch has already overwritten or deleted forces nothing. This is exact (M10 covers scans).
- **ProcessKill** keeps every returned batch pending and volatile.
- **A Durable write is two events**, `Append` and `Barrier` (§4.1).
- **Failures** leave their own batch, and every pending batch, with an unknown outcome. The fail-stop latch comes from `fail_stop.rs`.
- **Background memtable flushes** make a seqno-prefix durable, which is a subset already in the domain.
- **Separate databases** have independent bits.
- **Implementation.** Built on `MemoryEngine` with crate-internal seams:
  - `unsynced_batches`, `fork`;
  - durable maps and pending ops (`MemoryBatch::ops`);
  - a constructor from those parts.

  The lazy overlay is the durable maps, then the pending batches with their bits, then post-crash writes.

#### 2.4.4 Finalizing commits and their dependency scope (DEC-63 Q14, refined)

**Definition.** A record **finalizes** a **scope** if its correctness, once durable, presumes that every batch in that scope survived. In our protocol:

| Finalizer | Scope it presumes |
|---|---|
| **Bk** (`tip = new`, marker erased) | this flush's coin sub-batches |
| **Recovery's final batch** | recovery's coin sub-batches |
| **A subscriber's locator** (commit step, checkpoint, cadence or rewind) | that subscriber's data **through the locator's target X**, which must all be **submitted** in returned batches, never left staged in memory (GPT r5; T41); its data batches since its previous locator; plus, **cross-store**, a chainstate durable tip G that the locator is not ahead of. So B1 must have returned before a synced commit at `LCA(p, new)` or a step-6 checkpoint, a syncing cadence commit is capped at `LCA(progress, T_ack)`, and a rewind commits an ancestor of the old locator (§5.6 item 6) |
| **B1** | every **presumed effect** that is pending (Opus r2 6a), and **the previous chainstate finalizer**: the previous flush's Bk or Bp, or recovery's final batch (`Scope::PreviousFinalizer`; Opus r3 3) |

A **presumed effect** is an immediate Buffered effect (A1 §2.10.1) that a later consensus computation reads, while coins-only recovery cannot recreate it. The example is a rotation snapshot: Core's member computation silently takes another path when a previous cycle's snapshot is missing (`llmq/utils.cpp:496-503` at v24: `break`), and §3.5.5's replay does not re-run `ProcessBlock`. Effects whose loss is harmless or self-healing are **independent** and lie in no scope. CH7a classifies every effect.

**The rule** (normative):
1. Before a finalizer is appended, every batch in its scope must have **returned**.
2. A `sync_wal` must then **start** and **return** (§2.4.1).
3. Only then is the finalizer appended.
4. **Bk, Bp and recovery's final batch are Durable** (ruling 5). A finalizer may be Buffered only if nothing later presumes it.
   - A forward subscriber locator (commit step or cadence) may be Buffered if `D_max` is counted from the latest **barrier-covered** locator: losing it only lengthens the rewind, and no chainstate finalizer presumes it.
   - A step-6 **checkpoint** (§6.2 step 6) exists to raise that allowance, so the subscriber resumes past the old limit only after it returns; in the toy it is Durable (T38).
   - A **rewind** commit at a fork below the locator must be Durable, because the subscriber's later data below the old locator's height presumes it (T36).
   - The toy commits every locator Durable.

For a cross-store scope, the other store's barrier must have returned.

**Staged data** (GPT r5). A scope is about writes the finalizer presumes, not only batches already pending. A subscriber that batches its data holds blocks in `(s, p]` in memory, where p is its processed tip and s its submitted-data tip, the highest block whose data is in a returned batch. Before a locator commit at X, step 1 therefore includes **submitting all staged data through X**: X must be an ancestor of s, or equal to it, before the `sync_wal` starts. D1(f) cannot see a violation, because unsubmitted data is in no pending batch. R11c(i) catches it after any crash (T41).

**Losing a finalizer** is safe only until the next finalizer that presumes it. A Buffered Bk is lost safely while B1's marker still names its transition. Once the next flush's B1 replaces that marker, the coins Bk carried are gone for good, and recovery has nothing left to replay them from (T34). A Durable Bk returns, with its barrier, before the next flush begins, so it satisfies the next B1's scope at no extra cost.

**Costs:**
- one WAL fsync per split flush, and one per completed recovery;
- one per flush with a pending presumed effect;
- for each mandatory subscriber, one fsync plus one locator write per flush (DEC-64), per cadence commit while it syncs, per rewind below its locator, and per `D_max` checkpoint (one per `D_max` blocks of drain).

ST1's D2 measured `sync_wal` at 8–88 ms after 16 MiB. Full flushes are minutes to 24 hours apart (`DATABASE_FLUSH_INTERVAL`), so this is negligible. A1's "no more durably than Core" yields to US-NODE-04 here (erratum E5).

**Coverage:**
- planted bugs T26 (Bk), T27 (recovery's final batch), T22 (locator), T29 (presumed effect before B1), T34 (Buffered Bk presumed by the next B1), T33 (uncapped cadence locator), T36 (Buffered rewind commit), T38 (resuming past the limit before the checkpoint returns) and T41 (a checkpoint over staged, unsubmitted data);
- D1(f) flags an append whose scope is not covered, with no crash needed;
- the native regression (§6.4) carries the finalizer cases.

### 2.5 Platform mapping

- **Linux:** `fdatasync` and directory `fsync`; RocksDB `fdatasync`.
- **macOS (BL-32, ST1):**
  - Rust std uses `F_FULLFSYNC` for ST2's flat files.
  - `librocksdb-sys` never defines `HAVE_FULLFSYNC` (`build.rs:239-242`), so the WAL syncs with `fdatasync` (`io_posix.cc:1826-1852`).
  - This is a regression below Core: LevelDB uses `F_FULLFSYNC` with an `fsync` fallback (`leveldb/util/env_posix.cc:376-384`).
  - RocksDB's `F_FULLFSYNC` path has **no fallback** (`io_posix.cc:1829-1831`), so pasta's Mac check must include a volume without support.
  - `CXXFLAGS_<target>` in cargo `[env]` suffices to set the define.
- **Windows:** out of scope.

### 2.6 The fsync policy, as modelled (FR-STO-03)

| Barrier | Call | Covers | Issued at | Core's counterpart (v23 / v24) |
|---|---|---|---|---|
| File data | `FlatFs::sync_data` | one file's data and size | the files barrier (every file written since its last sync, via ST2 `BlockFiles::sync`); rollover finalize; `make_private` | `FileCommit`, current pair only (`flatfile.cpp:81-99` / `:82-103`) |
| Directory | `FlatFs::sync_dir` | one directory's operations | after create/truncate; after `make_private`'s rename; after unlinks; `blocks/`'s parent once | `DirectoryCommit`, result ignored (`util/system.cpp:1247-1256` / `util/fs_helpers.cpp:153-162`) |
| DB, durable | completed `Commit::Durable`, `sync_wal`, `flush_memtables` | the batch plus every batch that returned before the barrier started | B1; the pre-B1 barrier if a presumed effect is pending; each subscriber's pre-locator barrier and locator (commit step, checkpoint, cadence, rewind); the pre-Bk barrier; Bk or Bp; the index-only write; recovery's pre-final barrier and final batch | `WriteBatchSync`, block index only (`txdb.cpp:267-277` / `:276-286`) |
| DB, buffered | `Commit::Buffered` | survives a process kill; a **subset** survives a power loss | coin sub-batches, effects, subscriber data, recovery's sub-batches | every coins and EvoDB write |
| **Rule** | — | a finalizer is appended only after a barrier that started after its scope returned (§2.4.4); Bk, Bp and recovery's final batch are Durable | Bk, Bp, recovery's final batch, locators, B1 (presumed effects, previous Bk/Bp) | none |

---

## 3. Data model and public API

### 3.1 Placement

```text
dashd-store/
  Cargo.toml                 [features] faulty = ["model", "raw-bytes"]   # raw-bytes until CS0's swap
  src/faulty/{mod,world,engine,fs,decide,sched,harness,check,repro,fuzz,micro}.rs, toy/{mod,block,flush,recovery,subscriber,reference}.rs
  tests/power_loss_model.rs  M1–M18
  tests/power_loss_micro.rs  P1–P8, N0
  tests/power_loss_toy.rs    scenarios, T1–T41, N-controls, FA1–FA6, W1–W5, prune_selection_protocol, checked-in repros
  tests/power_loss_toy_twin.rs
  tests/power_loss_deep.rs   #[ignore]: nightly
  tests/rocks_wal_nonprefix.rs   native regression (feature `rocksdb`)
  tests/power_loss/regress/*.repro
node-fuzz/fuzz_targets/power_loss_toy.rs   (N5; DEC-59 Q1)
```

- No new dependencies.
- A test runs `cargo tree -e features,no-dev --target all` on every runtime binary crate and fails if it shows `faulty` or `raw-bytes`.
- `node-fuzz` uses `default-features = false`.

### 3.2 World, events, marks

```rust
#[derive(Clone)] pub struct World(Arc<Mutex<WorldState>>);
pub struct WorldConfig {
    pub faults: Vec<Fault>, pub capacity: Option<u64>, pub shared_capacity: bool,
    pub granularity: Granularity, pub adversary: Adversary, pub allocate: AllocateMode,
    pub on_event: Option<Box<dyn FnMut(&World, EventId) + Send>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum CrashKind { ProcessKill, PowerLoss }
impl World {
    pub fn new(cfg: WorldConfig) -> Self;
    pub fn engine(&self, name: &'static str, schema: &'static DbSchema) -> Arc<FaultyEngine>;
    pub fn fs(&self) -> Arc<FaultyFlatFiles>;
    pub fn mark(&self, mark: Mark); pub fn freeze(&self); pub fn trace(&self) -> Trace;
    pub fn capture(&self) -> CrashImage;
    pub fn set_extractor(&self, x: Arc<dyn RefExtractor>);
    pub fn durable_view(&self) -> WorldDurableView;          // spans every store (§5.3)
    pub fn work(&self) -> Work;
}
impl CrashImage { pub fn boot(&self, kind: CrashKind, decider: Box<dyn Decider>, cfg: WorldConfig) -> World; }

pub enum Mark {
    FlushBegin { flush: u64, old: Option<Hash>, new: Hash },
    Durable { flush: u64 },                                   // ChainStateFlushed: after B1 returns (DEC-64)
    Locator { store: &'static str, tip: Hash, kind: LocatorKind }, // after the commit returned; kind = CommitStep | Checkpoint | Cadence | Rewind
    /// The final selection D ∩ eligible(actual), and whether the automatic check re-armed (FA2, R5, preconditions).
    PruneSelected { flush: u64, files: Vec<u32>, bound: u32, rearmed: bool },
    /// V declined the flush's selection (CH2 DEC-211): the flush prunes nothing. No checker reads it.
    PruneDeclined { flush: u64, files: Vec<u32> },
    /// A due prune check: the retained dry-run set D under the predicted post-commit lock, whether D met the
    /// automatic target, the prune cause, and every full-flush cause (FA1, FA2).
    PruneDecision { due: PruneDue, dry_run: Vec<u32>, met_target: bool, predicted_lock: Option<u32>,
                    flush_for_prune: bool, full_flush: Vec<FlushCause> },  // Prune | Cache | Interval | Scripted | DMax
    /// Subscriber state for the liveness assertions FA3–FA6, including `Blocked` (at its limit while W waits).
    SubscriberState { store: &'static str, synced: bool, durable: Option<Hash>, progress: Hash, blocked: bool },
    /// Core's log lines CH8's FT asserts: "limited pruning to height X" and "<name> prune lock moved back to X".
    Log { line: String },
    /// From inside the validating connect, on success. `applied` is false for TestBlockValidity
    /// and VerifyDB connects (Core's fJustCheck); R6 ignores those (Opus r2 11).
    Connected { block: Hash, scripts: ScriptCheck, applied: bool },
    BestEffortBegin, BestEffortEnd, ConnectBegin, ConnectEnd, RecoveryBegin, RecoveryEnd,
    StoreFailed { op: &'static str }, Abort { message: String },
}
pub enum OpKind { Append(Commit), Barrier, SyncWal, FlushMemtables,
    Create, WriteAt, Allocate, Truncate, SyncData, SyncDir, Copy, Rename, Remove, CreateDir,
    Read, Len, LinkCount }
```

### 3.3 The two stores

```rust
pub struct FaultyEngine { /* MemoryEngine, name, World, lazy overlay */ }
impl KvRead for FaultyEngine { /* point reads newest-first; scans per key newest-first (§2.4.3) */ }
impl KvEngine for FaultyEngine { type Batch = MemoryBatch; type Snapshot<'a> = MemorySnapshot; }
pub struct FaultyFlatFiles { /* World, Mutex<FsState> */ }
impl FlatFs for FaultyFlatFiles { /* 14 methods */ }
impl FaultyFlatFiles { pub fn link_external(&self, e: &Path, n: &Path); pub fn read_all(&self, p: &Path) -> io::Result<Option<Vec<u8>>>; }
```

### 3.4 Schedules, faults, decisions

```rust
pub enum EventSel { Index(u64), Nth { store: StoreSel, op: OpKind, n: u32 }, AfterMark { mark: MarkKind, n: u32 } }
pub struct CrashWindow { pub from: EventSel, pub to: Option<EventSel> }
pub struct Fault { pub at: EventSel, pub kind: FaultKind, pub outcome: FaultOutcome }
pub enum FaultKind { NoSpace, Io }  pub enum FaultOutcome { NotApplied, Applied, Partial(u32) }
pub enum Granularity { Write, Page, Sector, Byte }
pub enum Adversary { Realistic, Extended, Witness(WitnessSet) }
pub struct WitnessSet { pub grant_prefix: bool, pub break_atomic: bool, pub break_rename: bool,
                        pub entry_on_file_sync: bool, pub break_psow: bool }
pub struct Var { pub kind: VarKind, pub name: String, pub domain: u32 }
pub enum VarKind { BatchSurvives, DirOpSurvives, FileSize, Sector, InsideWrite }
pub trait Decider: Send { fn choose(&mut self, var: &Var) -> u32; }
```

### 3.5 The harness

```rust
pub trait Reference { type Op; type Config; fn new(cfg: &Self::Config) -> Self; fn apply(&mut self, op: &Self::Op); }
pub trait Sut: Sized {
    type Config: Clone + fmt::Debug; type Op: Clone + fmt::Debug;
    type Ref: Reference<Op = Self::Op, Config = Self::Config>;
    fn open(world: &World, cfg: &Self::Config) -> Result<Self, Abort>;
    fn apply(&mut self, op: &Self::Op) -> Result<(), Abort>;
    fn check(&self, world: &World, refm: &Self::Ref, trace: &Trace) -> Result<(), Vec<Violation>>;
    fn continue_after_recovery(&mut self) -> Option<Self::Op> { None }
    /// Preconditions a scenario asserts before any search (§6.2): selected files, replay-path intersections.
    fn preconditions(&self, world: &World, refm: &Self::Ref, trace: &Trace) -> Result<(), String> { Ok(()) }
}
pub enum Mode { Exhaustive { max_branches_per_image: u32 }, Witness { expect: &'static [InvariantId] } }
pub struct Scenario<S: Sut> { pub name: &'static str, pub cfg: S::Config, pub ops: Vec<S::Op>, pub window: CrashWindow,
    pub kinds: &'static [CrashKind], pub faults: FaultPlan, pub adversary: Adversary,
    pub granularity: Granularity, pub nested: Nested }
pub fn explore<S: Sut>(sc: &Scenario<S>, mode: Mode) -> Report;
pub fn sweep<S: Sut>(gen: impl Fn(&mut Seeded) -> Scenario<S>, seed: u64, cases: u32, cap: Work) -> Report;
pub fn replay<S: Sut>(r: &Repro) -> Result<(), Vec<Violation>>;
pub fn minimize<S: Sut>(failing: &Repro, budget: u32) -> Minimized;
```

### 3.6 Neighbours

| Task | Direction | Types and obligations |
|---|---|---|
| **ST1** | upstream | engine traits, `MemoryEngine` and seams, fail-stop. **BL-34 note:** set and assert `manual_wal_flush = false` in `rocks.rs`; prefix enforcement is an option; doc errata E10 |
| **ST2** (i:ST2) | upstream | `FlatFs`, `BlockFiles<F>` and its public prune API, `BlockFileReader<F>`, `CoreBytes`. Public API only (DEC-59 Q4). Under DEC-64's order the first `sync_and_take_dirty_infos` precedes B1 and selection, and the second precedes Bk or Bp, so no partition helper is needed |
| **CS0** | upstream | `node-fuzz/run.sh`, `node_sink`, `CoreBytes` (DEC-59 Q1, Q8) |
| **O3** | upstream | DEC-59 Q6 budget |
| **CH2** | downstream | §5.6 contract |
| **CH7a** | downstream | effect classification (presumed or independent); presumed effects get a barrier before B1 (§2.4.4, E5) |
| **CH8** | downstream | DEC-64's flush order and lock rules (E12–E14, E16, E19, E20). **Acceptance cross-check:** all six `feature_index_prune` assertions (v23 / v24 lines of `test/functional/feature_index_prune.py`): "limited pruning to height 689" and `pruneheight == 367` (`:77-81` / `:94-98`); 735 (`:115-116` / `:134-135`); "limited pruning to height 2489" and 2207 (`:149-151` / `:170-172`); "basic block filter index prune lock moved back to 2480" (`:154` / `:175`), which needs the log-only moveback emulation of E14 |
| **CH3b** | downstream | S7; `Mark::Connected` with `applied` |
| **ST3b** | downstream | owns A-DEV, A-PSOW (including the WAL), A-REN, A-ATOMIC, A-DUR on the real stack; reuses `ChainstateView`; gets §6.4's WAL layouts as targeted cases |
| IX, LK, WL, `llmq/` owners | downstream | subscriber rules (§5.6 item 6) |

---

## 4. The schedule space

### 4.1 Events

- Every mutating store call is an event, and a Durable write is two: `Append(Durable)` and `Barrier`.
- A crash point is "after event k".
- Partial barrier or sync progress is already an adversary choice at the preceding `Append` or event.
- A crash inside a multi-page `pwrite` is ProcessKill's `InsideWrite` cell.

### 4.2 Execution modes

- **Capture mode** (crash-only): one forward run, with a copy-on-write capture at each event in the window.
- **Re-execution mode** (faults): one run per fault point, capturing from the fault to the stop.
- **Nested crashes:** `Depth1`, or `Depth2Sample(n‰)`. Outer cells keep their provenance (L4).

### 4.3 Lazy cells: rule L, and why the search is exhaustive

Cells: batch survival bits, directory-op survival bits, inode sizes, sector versions, `InsideWrite`.

- **L1, force on observation.** An operation whose **result** depends on a cell decides it. The list is **not exhaustive** (Opus r2 14):
  - `read_exact_at`: binding, size and the sectors read;
  - `len`: binding and size;
  - `link_count`: every binding of the inode;
  - engine `get` and `multi_get`: per key, newest first, until a survivor;
  - engine `iter_prefix`: **per key in the range, newest first** (§2.4.3);
  - `create`, `remove`, `write_all_at`, `truncate` and `copy` on an undecided binding: they force it, because their result depends on it.
- **L2, carry what is moved.**
  - `sync_data` promotes an inode's pending cells into the durable image as the same `Var`s.
  - `copy` references the source's cells.
  - `rename`, `remove` and `sync_dir` move or promote bindings and bits.
  - Engine barriers promote pending bits.
- **L3, force what is partly overwritten.** Partial-sector writes, `Shrink`/`Zero` inside a sector, and extending writes force their cells.
- **L4, one value per branch.** A `Var` is decided at most once per branch, across epochs. Undecided cells keep their first-crash provenance. After a ProcessKill, outer pending state stays pending.
- **L5:** `available_space` is not an observation.
- **L6:** checks observe like the SUT, except R7's projection.

**Exhaustiveness.** Cells are independent with fixed domains. Under rule L every dependency either decides a cell or carries it unread, so DFS over the decided cells covers every observable state. M10 checks this against eager enumeration, on observation traces, under post-crash mutations, scans and nested crashes.

**What a correct protocol forces:**
- the pending bits of meta and index keys read before recovery writes them (at most one in-flight chainstate finalizer);
- effects read by R13;
- the subscriber's locator bit. Its data bits are not forced: the rewind deletes blindly, and R11c reads only keys at or below the locator, which are durable, or keys catch-up has rewritten;
- the size cells of files `BlockFileReader` reads (`len()` per record, `blocks.rs:751`);
- the directory bits of names looked up.

**Blind replay and blind rewind** (Opus r3 7; normative for CH2, §5.6 item 13). Coin sub-batch bits are **not** forced, because replay erases inputs and puts outputs without reading them (A1 §3.5.5 step 4), so the coins digest reads keys recovery has already overwritten. In the same way, the subscriber rewind deletes heights `(L, L + D_max]` without reading them. A recovery that reads the keys it then overwrites or deletes, such as `CCoinsViewCache::SpendCoin` → `FetchCoin`, or a rewind that scans for "records above L", forces every pending batch it touches, and the per-image bound becomes 2^(pending batches). §8.2 derives the per-scenario bounds from this inventory.

### 4.4 Faults: ENOSPC and EIO per call

**Outcomes.**

| Call | `NotApplied` | `Applied` | `Partial(n)` |
|---|---|---|---|
| engine `Append` | absent | pending, unknown, latched | — |
| engine `Barrier`/`sync_wal`/`flush_memtables` | no barrier | no barrier, latched | — |
| `write_all_at` | nothing | written, poisoned | n granules |
| `sync_data` | — | poisons | — |
| `sync_dir` | — | ops stay pending | — |
| `create`/`rename`/`remove`/`copy`/`truncate` | no effect | applied, unsynced | `copy` prefix |
| `allocate` | advisory: less or nothing | — | partial |
| reads | fail | — | — |

**Fatal or not** (R8):
- **Fatal:** engine write and barrier errors; `FlatFs` mutating errors other than `sync_dir` outside `BestEffort` regions; reads inside `Recovery` or `Connect` regions; any `Mark::StoreFailed`.
- **Not fatal:** `allocate`; `sync_dir`, Core's `DirectoryCommit`, whose result Core ignores (DEC-212; the operations stay pending, and R1 and D1(a) entitle what a crash then loses); `BestEffort` regions (ST2's unlink and startup scan; checked by R9); reads elsewhere.

**After a fatal fault:**
1. Capture.
2. Boot both crash kinds, repaired.
3. Recover.

A read fault during recovery must end it with a sanctioned message and no mutation after the fault. A rerun without the fault must then succeed.

### 4.5 Exhaustive and witness search

- **`Exhaustive`** (clean scenarios and negative controls). Full DFS over every image in the window, both kinds. The per-image bound is computed from §4.3's inventory (§8.2), the cap (1,024 in PR, 65,536 nightly) is above every bound, and **zero truncated images** is asserted.
- **`Witness { expect }`** (planted bugs). The window starts at the bug's **trigger** (`CrashWindow::AfterMark` on the flush, prune or recovery step it corrupts), so earlier images are never explored (Opus r2 5). Nested witnesses fix the outer decisions to the first outer branch that reaches the trigger. The search stops once every expected invariant has been reported, fails if the scenario's bound passes first, and reports `found`, never coverage.
  - Large records (3–8 KiB) appear only in nightly witness runs. Choice 0 = survived, so the second branch already loses the last sector read.
- **Preconditions.** Before any search, `Sut::preconditions` runs on the forward trace. Each prune scenario asserts its selected files and its replay-path intersection; each subscriber scenario asserts that its two lock variants select differently. A failed precondition fails the test without searching (GPT r2 1).

### 4.6 Seeded sweeps

Per case:
- a workload;
- a crash point, weighted 10:1 toward flush and recovery windows;
- a kind;
- with p = 0.2, one fault;
- with p = 0.1, a depth-1 nested crash;
- a single `Seeded` branch.

Failures are reported with explicit decisions, against a deterministic work cap; a capped sweep reports `truncated`. M3's `regtest-40000` uses capture mode with sampling.

### 4.7 Minimisation

The steps, in order: workload ddmin, the earliest crash point, fault ddmin, decisions toward 0, coarser granularity, then dropping nesting and adversary.

- Decision minimisation repeats to a fixed point, with domain and invariant re-checks after each step.
- A result is labelled "1-minimal" only after a verified final pass (`minimality_verified`); otherwise it is a best effort.
- M13 includes GPT's non-monotone case.

### 4.8 Repros

A line format. All **40** planted bugs have checked-in minimised repros that replay in the PR gate. Fuzz findings go to the private sink (N5).

### 4.9 Determinism

- No store call or replayed state may depend on iteration order or thread scheduling: CH2 uses a fixed `SaltedFxBuild` salt under the seam, and store calls never run on worker threads.
- No wall clock, no global RNG; paths are virtual.
- Panics count as R0 violations.
- M11 compares trace hashes.

---

## 5. Invariants after reopening

### 5.1 The checks

**The allowed set A** is built from the reference's flush list: `T_ack` is the last flush with a `Mark::Durable`, emitted after its B1 returned (DEC-64). A = `{T_ack}` plus the new tip of each later begun flush. A mark that disagrees with the reference is a violation.

**Order:**
1. store-level checks;
2. R0;
3. view-level checks.

All violations are reported.

| ID | Check | A1 | Hook |
|---|---|---|---|
| **R0** Recovers | `Sut::open` returns `Ok` without panicking for every image of either kind, with no injected fault and no witness adversary. "Unable to replay blocks…" and "Found EvoDB inconsistency…" are violations. The **sanctioned** subscriber refusal ("…best block of the index goes beyond pruned data…", `index/base.cpp:110` / `:122`) is allowed only in S11c | I2 | §3.5.5 |
| **R1** DataBeforeReference | **Store form:** every flat-file range a durable record references holds the bytes it held when the record became durable. **View form:** every `HAVE_DATA` entry reads back as the reference block, and every `HAVE_UNDO` entry passes its checksum and equals the reference undo. **Position within recorded size:** every `HAVE_DATA` record `[pos−8, pos+len)` lies within `[0, block_files[f].size)`, and every `HAVE_UNDO` record `[pos−8, pos+len+32)`, **checksum trailer included**, within `undo_size`. **Entitled (DEC-212):** a file a crash lost whose directory entry, or a directory entry on whose path, a failed `sync_dir` left pending as a create reads nothing, as in Core: the path's first unbound entry from the root must be that entry; any other lost entry (a removal or a rename's source never loses a file), or another operation on the name since the failure (recreating a directory above it included), is a violation | I1 | load, reads |
| **R2** Markers | `head_blocks` absent, `tip` present, `evo_tip == tip` | I2 | 2–4 |
| **R3** StateAtAllowedTip | after step 4, `meta.tip ∈ A`, and the coins and state digests equal the reference's at that tip | I2, I4 | 4 |
| **R4** AckedDurable | the recovered tip is `T_ack` or a later flush's tip | §3.5.4 | — |
| **R5** IndexDurable (GPT r2 3) | Let **E** be the completed-barrier base overlaid, **in WAL order**, with the surviving pending batches: the engine's recovered state before recovery writes. After recovery, every `block_index` and `block_files` record of E is present with E's value, except for entitled changes: (a) status bits the reference allows (R6); (b) positions and `HAVE_UNDO` added by activation connects; (c) prune clears and resets in a completed Bk or Bp of a flush during recovery whose `PruneSelected` matches the reference's selection. Any other loss or change is a violation, in particular a lost `HAVE_DATA` or position (T31). A surviving in-flight Bk's clears are part of E, so they pass | §3.5.4 3, 5, 8 | load |
| **R6** ValidationProvenance | (a) every block on the marker's forward path has an `applied` `Connected` mark **before** its flush's `FlushBegin`, with the reference's assumevalid decision; (b) the blocks replayed coins-only are exactly `old → fork → new`; (c) every activation connect has an `applied` `Connected` mark in the recovery epoch; (d) invalid blocks end `FAILED_VALID`; (e) the final tip is `best_valid(c)` with **c = E's index**, the pre-recovery overlay of R5, not the recovered index. A recovery that erases entries can therefore not lower the reference tip with them (T31, Opus r3 8a). `applied = false` marks (`fJustCheck`) are ignored | I3 | 4–6 |
| **R7a** Converges | for a durable or stabilised outer image, an interrupted-then-completed recovery gives the same `ChainstateView` projection as an uninterrupted one | I4 | 4 |
| **R7b** MixedKinds | ProcessKill → PowerLoss, not stabilised: the nested result passes R0–R6, R11 and R13 over the combined trace | I4 | — |
| **R8** FailStop | after the first fatal error of an epoch (§4.4), no later mutating call in that epoch succeeds, and the epoch ends with `Mark::Abort` carrying a sanctioned string; after repair, recovery passes R0–R6 | I5 | §3.5.6 |
| **R9** PruneSafe | no durable `HAVE_DATA` references an absent file; a file left behind is unreferenced, `meta.pruned_files` is durable, and the startup scan (`blocks.rs:687`; Core v24 only, `blockstorage.cpp:412-417`) removes it; the toy runs the scan under both `Release` values | I1 | scan |
| **R10** LinkRule | the inode behind an external hardlink is byte-identical in every image | §2.11 | — |
| **R11a** SubscriberBehind | each subscriber's recovered locator L is **not ahead of G**, the chainstate's recovered durable tip (the marker's `new` while a marker is durable, else `meta.tip`; Opus r3 5). "Not ahead" means L is not a strict descendant of G: L is G, an ancestor of G, or on a stale branch. A synced commit at `LCA(p, new)` and a step-6 checkpoint follow a returned B1, a syncing cadence commit is capped at `LCA(progress, T_ack)` (T33), and a rewind commits an ancestor of L | §3.5.7 | — |
| **R11b** SubscriberCanCatchUp | for each **mandatory** subscriber enabled in every epoch since its locator was written, every block on the path from its recovered locator to the recovered tip (disconnects back to the LCA, connects up to the tip) is readable with **block data and undo**, per its declared needs (`Requirement::Block { undo }`). A subscriber disabled and later re-enabled past pruned data must end with the sanctioned refusal instead (S11c) | §3.5.7, DEC-64 | — |
| **R11c** SubscriberConsistent (Opus r2 4) | (i) after the subscriber's **blind rewind**, which deletes heights `(L, L + D_max]` without reading them: its data equals the reference at L; (ii) after **catch-up** to the recovered tip: its data equals the reference at the tip. Data ahead of the locator before recovery is legitimate (A1 §3.5.7). Event-driven `llmq/` records, IS marks and recsigs, stay outside R11c, as effects stay outside `state_digest` | §3.5.7 | subscriber recovery |
| **R12** Continues | after recovery: connect one block, flush, shut down cleanly, restart; same projection; **every** store-level and view-level check runs again. Scope: in the PR gate, on each image's all-survived branch and on every witness branch that lists R12 (T14, T29); nightly, on every branch | I2, I4 | — |
| **R13** Effects | every effect committed before the last completed barrier is present; any subset of later ones may be | §3.5.7 | — |

### 5.2 Views, references, check profiles

```rust
pub struct Meta { pub tip: Option<Hash>, pub head_blocks: Option<(Hash, Option<Hash>)>, pub evo_tip: Option<Hash>,
                  pub pruned_files: bool, pub last_file: u32 }
pub trait CoinsStateView {
    fn meta(&self) -> Result<Meta, Abort>;
    fn coins_digest(&self) -> Result<Hash, Abort>;
    fn effect_cfs(&self) -> &[Cf];                            // Opus r2 10: R13 in the coins profile
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, Abort>;
}
pub trait ChainstateView: CoinsStateView {
    fn index(&self) -> Result<Vec<IndexEntry>, Abort>; fn block_files(&self) -> Result<BlockFilesState, Abort>;
    fn read_block(&self, e: &IndexEntry) -> Result<Vec<u8>, Abort>; fn read_undo(&self, e: &IndexEntry) -> Result<Vec<u8>, Abort>;
    fn state_digest(&self) -> Result<Hash, Abort>;            // every CF except meta, block_index, block_files, coins, effects
    fn locators(&self) -> Result<Vec<(&'static str, Hash)>, Abort>;
    fn subscriber_digest(&self, store: &'static str) -> Result<Hash, Abort>;
    fn replayed(&self) -> &[Hash];
}
pub trait CoinsRef { fn coins_digest_at(&self, tip: &Hash) -> Hash; fn flushes(&self) -> &[(u64, Option<Hash>, Hash)]; }
pub trait ChainRef: CoinsRef {
    fn block(&self, h: &Hash) -> Option<&[u8]>; fn undo(&self, h: &Hash) -> Option<&[u8]>;
    fn state_digest_at(&self, tip: &Hash) -> Hash; fn valid(&self, h: &Hash) -> bool;
    fn assumevalid(&self, h: &Hash) -> ScriptCheck; fn best_valid(&self, c: &[IndexEntry]) -> Option<Hash>;
    fn subscriber_digest_at(&self, store: &str, tip: &Hash) -> Hash;
    fn selection(&self, flush: u64) -> Vec<u32>;              // reference prune selection (R5, preconditions)
}
pub fn check_coins_profile(..) -> Vec<Violation>;   // R0, R2, R3 (coins), R4, R7, R8, R13
pub fn check_chainstate(..) -> Vec<Violation>;      // the profile + R1, R3 (state), R5, R6, R9–R12
```

### 5.3 The dependency checker (D1): standing, cross-store

```rust
pub enum Requirement { Bytes(FileRef), Block { hash: Hash, undo: bool }, DurableTipAtLeast { store: &'static str, hash: Hash } }
pub enum Scope { SinceFlushStart, SincePreviousLocator { store: &'static str }, Keys { cf: Cf, range: Range<Vec<u8>> }, Store { store: &'static str },
                 PreviousFinalizer }   // the latest earlier finalizer in the same store (B1: the previous Bk/Bp; Opus r3 3)
pub trait RefExtractor: Send + Sync {
    /// Standing requirements of a durable record, evaluated against a view spanning ALL stores.
    fn refs(&self, db: &str, cf: Cf, key: &[u8], value: Option<&[u8]>, view: &WorldDurableView) -> Vec<Requirement>;
    /// The dependency scope this record finalizes (§2.4.4), or None. Independent effects are in no scope.
    fn finalizes(&self, db: &str, cf: Cf, key: &[u8], value: Option<&[u8]>) -> Option<Vec<Scope>> { None }
}
```

**Standing requirements** (Opus r2 9). After every completed barrier in **any** store, D1 re-evaluates the requirements of **every** durable record, in every store, against `World::durable_view()`. So a chainstate Bk that clears blocks still needed by a subscriber's durable locator is caught at Bk (T16, T28, T30).

**Which records carry block requirements** (GPT r3 6, ruling 1; normative for every extractor):
- Only a subscriber's **durable locator** L does. It requires each block, with its undo where the subscriber declares undo needs, on the path from L to G: the disconnects from L back to `LCA(L, G)`, then the connects up to G.
- **G** is the chainstate's guaranteed recovery tip, read from `durable_view`: the durable marker's `new` if a marker is durable, else `meta.tip` (Opus r3 5). After B1 erases `meta.tip`, the marker names the tip.
- L also requires `DurableTipAtLeast`: L is not a strict descendant of G (T10, T33).
- A subscriber's **processed data records** carry **no** block requirements. They describe blocks already applied, and only a rewind past them (L's path) or a catch-up (L's path) ever reads blocks.
- So once L = G, as in S3p and N9 after the commit step, the locator requires nothing. Bp/Bk and the unlink may then clear and remove files whose heights the subscriber's data still mentions. **S3p and N9 must stay D1-clean through Bp/Bk and the unlink.**
- Chainstate records keep their own requirements: index entries require their bytes, a durable marker its replay path.

| Kind | Violation |
|---|---|
| D1(a) | a required byte range is not durable: its binding, its length, or a pending or poisoned write overlapping it. **Entitled (DEC-212):** a name or a directory a failed `sync_dir` left pending as a create (it has no durable binding, and its newest pending change binds it) may be unbound, until another operation on the name or a later sync that makes the create durable; a removal or a rename's source is not entitled; the file's bytes are still checked while it is bound |
| D1(b) | a required block or undo record has no durable index entry with a position; or a `DurableTipAtLeast` requirement fails, as for a locator ahead of the chainstate (T10) |
| D1(c) | an unresolvable requirement |
| D1(d) | a write, truncate, zero-fill, copy-over, rename-over or remove that touches a durably referenced range. **Erratum (checkpoint 1.1):** a write touches a range only if it changes the range's bytes (an identical rewrite cannot, by A-PSOW). A **copy-over always touches** every referenced range: it truncates first and is not atomic. A **rename-over is exempt** for a referenced range only when the source's size and bytes over the whole range are known durable (no pending write or size reduction) and equal the destination's durable bytes, so either binding a crash keeps reads the same, as `make_private`'s synced copy does; anything undecided counts as a change. No comparison reads past EOF |
| D1(e) | an unlink of a file a durable record references |
| D1(f) | an `Append` of a finalizing record while a pending batch **in its scope** is not covered by a barrier that started after that batch returned (§2.4.4). Batches outside the scope are ignored, so independent effects and V's concurrent writes do not count. B1's scope includes the previous chainstate finalizer (`PreviousFinalizer`: the previous Bk or Bp; T34) |

### 5.4 Model self-checks (always on)

- A synced value never changes.
- Sizes stay in their domain.
- Bindings never name a missing inode.
- The engine recovers its durable state plus the surviving pending batches.
- No `Var` is decided twice in one branch.

### 5.5 What "reopen" means

**Engine promotion at open** (rev3). Each engine runs `sync_wal` before recovery's first write in any store.
- After a ProcessKill, the outer epoch's pending batches stay pending in the model (L4). Without this barrier, the epoch's next finalizer, B1 with its `PreviousFinalizer` scope or a presumed effect, would presume them unbarriered, and a later PowerLoss could keep it while losing them.
- With ST1's options, RocksDB already does this at open: `avoid_flush_during_recovery = false` (the default, `include/rocksdb/options.h:1518`; ST1's `rocks.rs` does not set it) writes the recovered memtables to synced L0 tables (`db/db_impl/db_impl_open.cc:1897-1898`, `db/builder.cc:454`). The explicit barrier keeps the protocol independent of that option, as T23b's promotion does for flat files.
- "Stabilised" (R7a) means the outer image after this barrier. R7b covers nested crashes before it.

0. **Load prune locks** from every mandatory subscriber's durable locator, at `height(LCA(locator, recovered tip))`, before anything in the epoch can prune: activation flushes, R12's flush, an init-time prune (DEC-64; Opus r2 2c; T28). A subscriber with no durable locator locks at its start height.
1. Open `BlockFiles` from the durable `block_files` and `meta.last_file`, never from file lengths.
2. Run the startup scan if `meta.pruned_files`.
3. Load the index.
4. Run §3.5.5 steps 2–6, with the pre-final barrier.
5. Recover each subscriber:
   - **Blind rewind:** delete each height in `(L, L + D_max]` without reading it. `D_max` is the subscriber's static bound on how far its processed data may run past its latest barrier-covered locator; the toy refuses to process further without a commit (backpressure). A Durable **rewind commit** precedes any processing below `h(L)`, and a step-6 checkpoint returns before the subscriber passes its old limit (§5.6 item 6). So every record that can differ from the reference at L lies in that range. The rewind is idempotent, and nested crashes are explored.
   - **Catch up:** re-apply `BlockConnected` for `(L, tip]`, using block and undo data.
   - The next locator commit follows §2.4.4 and §5.6 item 6.

### 5.6 The CH2 contract (DEC-59 Q5, DEC-63, DEC-64; CH2's D2 design input)

1. **Generic stores:** over `E: KvEngine` and `F: FlatFs`, monomorphised.
2. **Synchronous seams:** flush steps, startup recovery, subscriber recovery and V-ahead are drivable on one thread.
3. **Determinism:** a fixed salt under the seam; no store call on worker threads (§4.9).
4. **Marks:**
   - `FlushBegin`;
   - `Durable` (after B1 returns);
   - `Locator`;
   - `PruneSelected`;
   - `Connected { applied }`, emitted from `ConnectBlock`'s success path; **`applied = false` for `fJustCheck`** (TestBlockValidity, VerifyDB);
   - `BestEffort` regions;
   - `Connect` and `Recovery` regions;
   - `StoreFailed`, `Abort`;
   - `PruneDecision`, `SubscriberState`, and `Log` for Core's prune log lines.
5. **The barrier rule with scopes** (§2.4.4): `RefExtractor::finalizes` returns scopes; every presumed effect class is declared by CH7a. **Bk, Bp and recovery's final batch are Durable**, and B1's scope includes the previous Bk/Bp. **At open**, each engine runs `sync_wal` before recovery's first write (§5.5). **Concurrency:** every write in a finalizer's scope has returned before its barrier starts, and the finalizer is appended after the barrier returns. In CH2, W must not count V's in-flight writes as covered.
6. **Subscribers** (Opus r3 2, ruling 4):
   - **Classification as Core's `m_synced`** (`index/base.cpp:157` / `:169`). A **synced** subscriber is notification-driven: it processes `BlockConnected` from its queue. A **syncing** one runs its own catch-up thread (`ThreadSync`). The class never comes from an instantaneous "has processed `new`" test.
   - **Synced commits:** at flush step 6, W waits for every synced mandatory subscriber to **drain its queue through `new`**. The subscriber then runs `sync_wal` and commits its locator at `LCA(p, new)`, which is `new` unless V reorged below it meanwhile. This is the analogue of Core's `ChainStateFlushed`, queued behind the `BlockConnected` events (`index/base.cpp:290-327` / `:332-369`).
   - **Syncing commits:** on a bounded cadence (Core's `SYNC_LOCATOR_WRITE_INTERVAL` = 30 s, `index/base.cpp:24, 187` / `:29, 202`), each one **capped** at `LCA(progress, T_ack)`, after a `sync_wal` of its data. On reaching the tip it becomes synced. An uncapped commit is T33.
   - **Rewind commits:** before processing a disconnect below its locator's height, a subscriber commits the fork as its locator, as Core's `BaseIndex::Rewind` does (`:224-240` / `:238-259`). The commit is one **Durable** batch that blindly deletes `(h(F), h(L) + D_max]` (T36).
   - Per-block data written on `BlockConnected`, or batched (declare which).
   - **Backpressure and progress** (GPT r4 1). No data is ever written above `h(L) + D_max`, where L is the latest barrier-covered locator. At that limit:
     - a **syncing** subscriber makes a capped commit at `LCA(progress, T_ack)`; if that does not advance L, it makes a flush due and commits again once B1 has returned;
     - a **synced** subscriber outside step 6 waits, still processing disconnects, and makes a flush due;
     - during the **step-6 drain**, a synced subscriber **checkpoints**: it submits its staged data through its processed tip p and waits for that batch to return, then runs `sync_wal`, then makes a Durable locator commit at p, and it resumes once that returns (T37, T38, T41).
   - **Path-based drain.** A subscriber that has not yet processed `Connected(new)` follows p → `LCA(p, new)` → `new`, the net effect of its queue up to that event, and drops queued events before it that lie off that path. One that already has commits at `LCA(p, new)` without draining (DEC-69). Dropping them follows Core's synced index, which ignores queued blocks that do not connect to its chain (`index/base.cpp:263-268` / `:281-286`). Every connect on that leg is an ancestor of `new` = `T_ack`, so a checkpoint is its own cap and raises `h(L)` by exactly `D_max`. The drain therefore ends after at most `⌈(h(new) − h(F))/D_max⌉` checkpoints, with F = `LCA(p, new)` (FA6).
   - Forward locator commits may be Buffered (§2.4.4 rule 4); checkpoints and rewind commits are Durable.
   - **Before every locator commit at X:** submit all staged data through X and wait for it to return; then `sync_wal`; then the locator. The subscriber tracks its processed tip p and its submitted-data tip s separately, and X must be an ancestor of s, or equal to it (GPT r5; T22, T41).
   - Recovery by **blind** rewind then catch-up (§5.5 step 5).
   - Declared block and undo needs. **Block requirements come only from the durable locator's path to G** (§5.3); data records carry none.
   - The sanctioned refusal when re-enabled past pruned data.
7. **Prune decision and locks** (DEC-64; Opus r3 1, 4; rulings 3, 7):
   - **Decision:** when a check is due (Core's `m_check_for_pruning` or a manual height, `validation.cpp:2781` / `:2989`), run a side-effect-free **dry-run selection under the predicted post-commit lock** and **retain** its file set D and whether it met the automatic target. Synced subscribers count at `new`, syncing ones at `height(LCA(durable locator, new))`. **`flush_for_prune ⇔ D ≠ ∅`**, as Core's `fFlushForPrune` (`:2812` / `:3020`). A full flush happens iff `flush_for_prune` or another trigger is due (cache size, the interval, a scripted flush, the `D_max` limit), as Core's `fDoFullFlush` (`:2838` / `:3046`; GPT r4 3). Log "limited pruning to height X" from the predicted lock, as Core does from its in-memory locks.
   - **Final selection** at step 7 is **`D ∩ eligible(actual)`** (GPT r4 2): each file of D is rechecked against the actual height bound and post-commit locks, and the usage target is **not** re-applied. Re-running the automatic selector is wrong, because it stops at the target, so a lower bound or V-ahead growth can pick files outside D (T39). The selection equals D on a stable chain without a failed commit (R8 makes those fatal).
   - **Re-arming:** after an automatic selection, `m_check_for_pruning` is set again iff the target is still unmet once the selection is removed **and** D had met it at the dry run. That shortfall is what the gap between decision and selection causes: V-ahead growth, or a reorg below `new`. When the dry run itself ran out of eligible files, the check is cleared, as Core clears it (`:2809` / `:3017`; T40).
   - The flush order of E12; the lock is at `height(LCA(locator, new)) − PRUNE_LOCK_BUFFER − 1`; never null after the commit step; loaded at open before activation; block and undo protected.
   - **Moveback log (emulated, log-only):** each mandatory subscriber keeps an emulated in-memory lock height: its processed height, lowered by movebacks and reset when it processes a block, as Core's `UpdatePruneLock` does. On `DisconnectTip` of block h, for each subscriber whose emulated height exceeds `h − 1`, set it to `h − 1` and log "`<name>` prune lock moved back to `h − 1`", as Core's `validation.cpp:3061-3065` / `:3295-3299` does. Our real lock does not move, because it is computed at flush time.
   - CH8 cross-checks all six `feature_index_prune` assertions (§3.6).
8. **Check profile:** `coins_cache_model` runs `check_coins_profile`, with `effect_cfs` and `get`; the chainstate fuzz runs `check_chainstate`.
9. **Digest scope:** `state_digest` covers every CF except `meta`, `block_index`, `block_files`, `coins` and the declared effect CFs.
10. **`RefExtractor`:** standing, cross-store requirements against `World::durable_view()`, re-evaluated after every completed barrier in any store. The chainstate's durable tip G is the durable marker's `new` if a marker is durable, else `meta.tip` (Opus r3 5).
11. **WAL order:** the engine's serialization order, never call order; coverage requires returned-before-started (§2.4.1).
12. **Engine options:** `manual_wal_flush = false` (ST1 asserts it; BL-34 note).
13. **Blind replay and rewind** (Opus r3 7, ruling 6): recovery replay and subscriber rewind must **not read the keys they overwrite or delete**. Otherwise the per-image bound becomes 2^(pending batches) and exhaustive search is lost. Replay erases and puts without `FetchCoin`. A rewind deletes a bounded range blindly; Q16 asks for a range delete.

---

## 6. Planted bugs, controls, self-tests (acceptance)

For each planted bug:
- the listed invariant, and the "Also" one, are among the violations reported (witness mode);
- its checked-in repro replays;
- its preconditions hold.

Negative controls run in exhaustive mode and must report nothing.

### 6.1 `MicroLog`: the PLAN's toy flush

The rev1 protocol, unchanged:
- append `len ‖ bytes ‖ crc32c` records;
- commit = `sync_data`, `sync_dir` on create, a Durable `meta.committed`, then `Mark::Durable`;
- snapshot via tmp, sync, rename, `sync_dir`;
- recovery reads exactly the committed records.

| ID | Planted bug | Caught by | Also |
|---|---|---|---|
| **P1** | **commit before fsync** | R1 | D1(a) |
| **P2** | **index before data** | R1 | D1(a) |
| P3 | no `sync_dir` after a rollover create | R1 | D1(a) |
| P4 | Buffered commit acknowledged | R4 | — |
| P5 | retries a failed `sync_data` | R1 | R8 |
| P6 | ignores a short-write ENOSPC | R8 | R1 |
| P7 | recovers by CRC scan | R3 | — |
| P8 | rename without `sync_data` | R1 | D1(a) |
| N0 | correct MicroLog | none | — |

### 6.2 `ToyChain`: A1 §3.5 in miniature, with errata

**Configurations.**
- `subscribers` is one of:
  - `None`: the explicit subscriber-free configuration for isolated prune and replay tests (GPT r2 1);
  - `Mandatory(mode)`, with `mode` one of (Opus r3 2, ruling 4):
    - `Synced { backlog }`: notification-driven, as Core's `m_synced` (set at init when the index's best block is the tip, `index/base.cpp:78` / `:90`). It processes `Connected` and `Disconnected` from a queue that lags V by up to `backlog` blocks (0 or 2 in the PR gate; Core's `LimitValidationInterfaceQueue`), and commits only at step 6.
    - `Syncing { durable, progress, cadence }`: its own catch-up thread, as `ThreadSync`. Every `cadence` blocks of progress it submits its staged data through the commit's target, runs `sync_wal`, and commits its locator at **`LCA(progress, T_ack)`**. On reaching V's tip it makes the same capped commit and becomes `Synced { backlog: 0 }`; Core commits, then sets `m_synced` (`:156-157` / `:168-169`). The PR cadence is 25 blocks in S11s and 80 elsewhere, below `D_max`. So S11 (progress 70 from 10) and S13 (progress below 90 at its crash) make no cadence commit before their flush.
    - `DisabledOneEpoch`.
  - Every mode keeps a log-only emulation of Core's in-memory lock height for the moveback line (§5.6 item 7).
- **`D_max`** is 96 in the PR gate and 1,024 nightly. A subscriber never processes past `h(L) + D_max`, where L is its latest barrier-covered locator, or its start height − 1 if it has none (backpressure). At that limit (§5.6 item 6):
  - a syncing subscriber makes a capped commit, and makes a flush due if that does not advance L;
  - a synced subscriber outside step 6 makes a flush due (the `DMax` cause);
  - inside step 6 it checkpoints (step 6).
  The toy commits every locator Durable.
- **Rewind commit** (Core's `BaseIndex::Rewind`, which commits the fork as the locator, `index/base.cpp:224-240` / `:238-259`, called from `ThreadSync` and `BlockConnected`, `:160, 270` / `:172, 288`). Before a subscriber processes a disconnect below `h(L)`, that is, when the reorg's fork F is a strict ancestor of L, it runs `sync_wal` and commits **one Durable batch** that blindly deletes `(h(F), h(L) + D_max]` and sets the locator to F. F is an ancestor of L, so it is not ahead of G. This keeps every data record that can differ from the reference at L inside `(h(L), h(L) + D_max]`, which is exactly what §5.5's blind rewind deletes. A Buffered rewind commit is T36: the later data below `h(L)` presumes it.
- `K` (`MIN_BLOCKS_TO_KEEP`) is 8 in the PR gate and 288 nightly. The toy's `PRUNE_LOCK_BUFFER` is **2** in the PR gate, so `K` binds as in production, and 10 nightly (Core's value, `validation.cpp:115` / `:122`; Opus r2 2e).
- `Release` takes both values.

**Stores.**
- `BlockFiles<FaultyFlatFiles>`: ST2's code with `-fastprune` (64 KiB files).
- A `chainstate` `FaultyEngine`: `meta`, `block_index`, `block_files`, `coins`, `evo`, `effects`.
- Optionally a `subscriber` `FaultyEngine`: `data`, `locator`.

**Blocks.**
- Synthetic; genesis at height 0 in file 0. PR records are **1 KiB** (1,032 bytes with the header).
- ST2 rolls a file over when `size + record ≥ 65,536` (`blocks.rs` `find_block_pos`), so a file holds **63 records**: file 0 is heights 0–62, file 1 is 63–125 (GPT r2 `prune-sizing.log`).
- S6 uses 900-byte blocks (72 per file).
- Evo folds every connected block into `evo/state`. One **presumed effect** class models rotation snapshots: written immediately as a Buffered effect when its cycle-base block is connected, and read by a later block's evo fold.

**Flush** (DEC-64; errata E3–E5, E12–E16, E19–E21):

0. **Prune decision** (Opus r3 1, ruling 3), only when a check is due: the toy's `m_check_for_pruning`, set at each blk chunk allocation, or a manual height.
   - Run a side-effect-free **dry-run selection** against `new` under the **predicted post-commit lock**: each synced subscriber at `new`, even with no locator yet (S3p); each syncing one at `height(LCA(durable locator, new))`, or at its start height if it has no locator.
   - Log "`<name>` limited pruning to height X" exactly as Core's loop does (`validation.cpp:2784-2798` / `:2992-3006`), from the predicted locks.
   - **Retain** the dry-run set D and whether it met the automatic target (GPT r4 2). `flush_for_prune ⇔ D ≠ ∅`, as `fFlushForPrune` (`:2812` / `:3020`). An automatic check is cleared here (`:2809` / `:3017`) and may be re-armed at step 7.
   - A **full flush** happens iff `flush_for_prune` or another cause is due: cache size, the interval, a scripted flush, or the `D_max` limit. This mirrors Core's `fDoFullFlush` (`:2838` / `:3046`; GPT r4 3). `Mark::PruneDecision` records D, `met_target`, `flush_for_prune` and every cause.
   - With an empty dry run and no other cause, there is no flush. A pruned IBD therefore does not flush at every chunk allocation. With an empty dry run and another cause (S11d's `D_max` flush), the flush is full and prunes nothing.
1. `CheckDiskSpace`.
2. Files barrier: the **first** `sync_and_take_dirty_infos()`. No selection has happened, so these infos are pre-prune.
3. If a presumed effect is pending: `sync_wal` (§2.4.4).
4. **B1** (Durable): every dirty index entry and file info, evo deltas, `evo_tip`, the marker `[new, old]`, the erase of `tip`. In a **one-batch flush**, B1 instead carries all coins and `tip = new`, with no marker.
5. `Mark::Durable` (`ChainStateFlushed`), after B1 returns.
6. **Synced subscribers commit** (Opus r3 2; GPT r4 1). W waits for every **synced** mandatory subscriber to drain through `Connected(new)`.
   - **Path.** If the subscriber has not yet processed `Connected(new)`, the drain moves it from its processed tip p along p → F = `LCA(p, new)` → `new`, the net effect of its queue up to `Connected(new)`. If it already has (V ran ahead), there is nothing to drain. Queued events before `Connected(new)` that are off this path are dropped, as Core's synced index ignores blocks that do not connect to its chain (`index/base.cpp:263-268` / `:281-286`). The disconnect leg is blind deletes, plus a Durable rewind commit if F is a strict ancestor of L.
   - **Checkpoint.** On the connect leg, when the next connect would pass `h(L) + D_max`, the subscriber does three things in order (GPT r5):
     1. It submits all staged data through its processed tip p as one Buffered batch and waits for it to return, so its submitted-data tip s reaches p.
     2. It runs `sync_wal`.
     3. It commits its locator at p (Durable, `Mark::Locator { kind: Checkpoint }`).

     It resumes only after the commit returns. B1 has returned, so `T_ack = new` and G = `new`. Every block on the connect leg is an ancestor of `new`, so p is its own cap `LCA(p, T_ack)` and is not ahead of G (R11a, D1(b)).
   - **Commit.** At `new`, the subscriber submits its staged data through `LCA(p, new)` as its last data batch and waits for it to return, runs `sync_wal`, commits its locator at `LCA(p, new)` (Durable, `kind: CommitStep`) and emits `Mark::Locator`. After a drain p = `new`. It is lower only if the subscriber had already processed past `Connected(new)` onto a branch that leaves `new`, because V reorged below `new` meanwhile (DEC-69).
   - **Progress (FA6).** The disconnect leg only lowers the processed height, so it never meets the limit. Each checkpoint sets L to a block exactly `D_max` above the old L. So the connect leg, of length `h(new) − h(F)`, needs at most `⌈(h(new) − h(F))/D_max⌉` checkpoints. The drain terminates without any other flush.
   - **Bound.** No data is written above `h(L) + D_max` for the barrier-covered L: before a checkpoint returns, the subscriber stays at its old limit, and a rewind commit lowers L atomically with its deletes. So §5.5's blind rewind of `(L, L + D_max]` still covers every record that can differ from the reference at L.
   - W does not wait for a **syncing** subscriber, which commits only on its capped cadence. The class comes from the mode, never from an instantaneous "has processed `new`" test (T35).
7. **Prune selection**, only if `flush_for_prune`, against the frozen tip `new` (GPT r4 2):
   - The **actual bound** is `min(new − K, manual height, min over mandatory subscribers of the actual lock)`. The actual lock is `height(LCA(durable locator, new)) − BUFFER − 1` after step 6; a subscriber with no durable locator contributes its start height.
   - The selection is **`D ∩ eligible(actual)`**: each file of D whose current `nHeightLast` is within the actual bound and is not the current file. The usage target is not re-applied, so the selection is a **subset of D by construction** (FA2), and equal to D on a stable chain.
   - Files outside D are never selected, even if V-ahead growth or a lower bound would make a fresh automatic scan pick them (GPT's fixtures; T39).
   - **Re-arm:** after an automatic selection, if `usage − bytes(selection) + buffer ≥ target` while D had met the target, set `m_check_for_pruning` again (`PruneSelected.rearmed`; T40). The next due check then decides afresh under the locks of that moment.
   - Apply `prune_one_block_file` to each selected file, clear the in-memory index entries, and emit `Mark::PruneSelected`.
8. Coin sub-batches (Buffered), in a split flush only.
9. The pre-Bk `sync_wal`, in a split flush only.
10. **Bk** (Durable, ruling 5): the last coins; the **second** `sync_and_take_dirty_infos()`, which yields the reset infos plus any synced V-ahead appends; the prune clears; `meta.pruned_files`; `tip = new`; the marker's erase. In a **one-batch flush that prunes**: **Bp** (Durable) = clears, reset infos and `pruned_files`, because B1 already set `tip` (Opus r2 13).
11. Release; unlink in a `BestEffort` region, then `sync_dir`.
12. The periodic index-only write: files barrier, then one Durable batch.

**Recovery:** §5.5 steps 0–5.

**Subscriber data.** Keyed by height. At most **two** Buffered data batches per flush interval: one once half the interval's blocks are processed, one at the commit step (Opus r2 5). Add one per step-6 checkpoint, and one per cadence commit while syncing. Each of these extra batches carries the staged data through the commit's target.
- **Two tips** (GPT r5). The subscriber tracks its processed tip p and its submitted-data tip s, the highest block whose data is in a returned batch. Data in `(s, p]` is staged in memory and lost on any crash, so a locator commit at X requires X to be an ancestor of s, or equal to it, when its `sync_wal` starts (§2.4.4).
- **Blind rewinds.** A rewind commit's deletes are in its own batch. Data through its fork F is below the old locator, so it was submitted before that locator's commit.
- **Bounds.** Data bits are never forced (§4.3), so the extra batches add events, not branches. The subscriber declares undo needs (`undo: true`), modelling coinstatsindex's undo reads on append and rewind (`index/coinstatsindex.cpp:123, 411` / `:129, 413`).

**The toy's `RefExtractor`** (GPT r3 6, ruling 1; §5.3):
- **Chainstate.**
  - An index entry requires the `Bytes` of its block and undo records.
  - A `block_files` record requires its sizes.
  - A durable marker `[new, old]` requires `Block { undo }` on its replay path: undo for the disconnects from `old`, data for the connects up to `new`.
- **Subscriber.** The locator L requires `Block { hash, undo: true }` for each block on L → `LCA(L, G)` → G, with G read from `durable_view` (the durable marker's `new`, else `meta.tip`). It also requires `DurableTipAtLeast` (L not a strict descendant of G). **Data records require nothing.**
- **`finalizes`.**
  - Bk: `SinceFlushStart` (this flush's coin sub-batches).
  - Recovery's final batch: its sub-batches.
  - B1: `Keys` over the presumed-effect CFs, plus `PreviousFinalizer` (the previous Bk, Bp or recovery final batch; T34).
  - Every locator commit (commit step, checkpoint, cadence or rewind): `SincePreviousLocator`, plus the cross-store chainstate barrier.
- Under this extractor, S3p and N9 stay D1-clean through Bp/Bk and the unlink: after step 6, L = G = `new`, so the locator's path is empty.

**Scenarios.** Every prune and lock scenario asserts its preconditions first (§4.5).

| ID | Workload (PR dims) | Preconditions asserted | Exercises |
|---|---|---|---|
| S1 | 30 blocks; one-batch flushes at 10/20/30; index-only write at 15; independent effects at 12 and 22; a presumed effect at 25, read by block 31's fold (R12's continuation); synced subscriber (backlog 0) | — | order, step 12, effects, §2.4.4 scopes |
| S2 | reorg of depth 3 inside the third flush window; synced subscriber, **backlog 2** | the emulated moveback lines: one per disconnect whose `h − 1` is below the subscriber's emulated lock (with backlog 2, the first disconnects log nothing, as in Core), the last naming the fork's height (the FT's "moved back to 2480" shape) | fork ≠ old; subscriber disconnects; the step-6 drain |
| S3 | first flush (old null), 10 blocks; subscriber null until its first commit | — | null old |
| S3p | fresh pruned IBD: heights 0–72, first flush **one-batch with pruning**, synced subscriber with no locator → commits at 72 → lock 69 | selected = {0}; lock not null after step 6; D1-clean through Bp and the unlink | DEC-64 null rule; Bp; locator-path requirements |
| S4 | split flush: 20 blocks, 5 coin sub-batches; synced subscriber. The **only** chainstate finalizer after B1 is this flush's Bk (N8) | — | sub-batches, pre-Bk barrier |
| S4b | **two** split flushes, F1 (0 → 20) and F2 (20 → 40), 5 coin sub-batches each; synced subscriber (Opus r3 3) | F1's Bk writes coins that `(20, 40]` does not touch | B1′'s `PreviousFinalizer` scope |
| S5 | **subscriber-free**; heights 0–72; coins last flushed at 5; index-only write at 40; split prune flush at 72 (bound 64) | selected = {0}; replay path (5, 72] ∩ file 0 = heights 6–62 | prune protocol; both crash kinds; the clean image at Bk's `Append` with clears retained (R5) |
| S5b | S5 with **no** index write between 5 and 72 | as S5; blocks 6–72 newly dirty at B1 | pre-prune versions |
| S5c | **subscriber-free**, V ahead. Flush 1 at `new = 60` (prior flush 50), V at 70: correct bound 52 selects ∅, V-tip bound 62 selects {0}. Flush 2 at `new′ = 80` (V 80): marker `[80, 60]` | flush 1: correct = ∅, T15's = {0}; flush 2's replay path ∩ T15's pruned heights = {61, 62} | selection against the flushed tip |
| S6 | 900 B blocks: accepted ahead across a rollover; lagging undo; restart before the rollover | — | † step 2; BL-29 |
| S7 | invalid block persisted before connection; one valid block connected before B1; template and VerifyDB connects (`applied = false`) | — | R6 |
| S8 | disk full mid-flush | — | I5 |
| S9 | EIO per call on a 20-block non-prune flush window; read faults in recovery; on S5 dims: `Applied` failure of Bk (abort, no unlink) and EIO on a pruned file's `remove` after Bk | S5's selection for the prune faults | R8 |
| S10, S10b | hardlinked blk file; `adopt_existing_files` | — | R10, A-REN |
| S11 | **syncing** mandatory subscriber (cadence 80): durable locator 10, in-memory progress 70; heights 0–120; flush at 120. Correct bound `min(112, 10−3=7)` selects ∅; in-memory-lock bound `min(112, 70−3=67)` selects {0} | correct = ∅, T16's = {0}: **they differ** | Q10 lock |
| S11r | syncing subscriber, durable and processed at 70 on branch A; reorg to B forking at 50; B to 80; flush at 80 on B, before the subscriber has processed the reorg. File 0 holds the common 0–50 and A's 51–62 (63 records); A's 63–70 and B's 51–80 are in file 1 (38 records). Fork lock: `LCA = 50` → bound 47, selects ∅; height lock: 70 → bound 67, selects {0}, deleting A's 51–62, which the subscriber's disconnects need (the common 0–50 lie below the LCA and are harmless; Opus r3 8c). **Continuation:** the subscriber's rewind commit at 50 (Durable, deleting `(50, 166]`), then catch-up of B51–80 on its capped cadence | correct = ∅, T30's = {0} | lock at the fork (Opus r2 2a); the rewind commit (T36) |
| S11c | subscriber disabled for one epoch, pruning passes its locator, then re-enabled (nightly) | its locator is behind a selected file | sanctioned refusal (R0, R11b scope) |
| S11s | **syncing** subscriber, cadence 25, from height 0. Chainstate flushes at 20 and 40 and V reaches 60 while the subscriber syncs. It is at progress 25 when `T_ack` = 20, at 50 when `T_ack` = 40, and reaches the tip 60 next, then a flush at 60 (Opus r3 2) | capped commits at 20, 40, 40, then 60 after it becomes synced; T33's at 25, 50, 60; `T_ack` < progress at each cadence point | the capped cadence; syncing → synced |
| S11q | **synced** subscriber with **backlog 2**, no locator. Flushes at 40 (no prune) and 72 (prune due); at step 0 of each it is two blocks behind `new` | correct: commits at 40 and 72, selects {0} at 72; T35's: no commit, ∅ | the drain through `new`; FA3, FA5 |
| S11d | **the `D_max` cycle** (GPT r4 1): synced subscriber, chainstate and locator durable at 0 (L = 0), `D_max` = 96, **backlog b ∈ {0, 1, 2}** (one run each). No scripted flush. The subscriber reaches p = 96 when V is at 96 + b, which makes a `DMax` flush due; W freezes `new` = 96 + b. Pruning is on with an automatic target above usage, so the automatic checks at the chunk allocations (heights 63, 78 and 94 in file 1) are empty and cause no flush. A manual prune to height 50 is requested at the same flush check as the `DMax` cause; file 0 ends at 62, so that dry run is **empty** (GPT r4 3) | b = 0: no checkpoint, commit at 96. b = 1, 2: one checkpoint at 96 after B1, then the commit at 96 + b. Every run reaches its commit (**FA6**). `flush_for_prune` is false, and the full flush's only cause is `DMax`. T37 blocks at p = 96 for b = 1, 2. **Staged data** (GPT r5): the half-interval batch covers 1–49, so on reaching the limit s = 49 and p = 96. The checkpoint's submit raises s to 96 before its `sync_wal` starts, and both tips are asserted at each step. **Named crash points**, both kinds, b = 1, 2: (a) before the submit returns, with 50–96 staged; (b) after the submit, before `sync_wal` returns; (c) at the locator's `Append`, before it returns; (d) after it returns, before resuming; (e) after 97's data is staged. All pass R11c in N1, and T41 fails (c) and (d) | the checkpoint; the path drain; FA1's cause rule; staged data |
| S12 | ProcessKill → PowerLoss with buffered effects and unsynced blk appends | — | R7b (N5) |
| S13 | syncing subscriber (cadence 80) durable at 10, progress below 90; chainstate durable at 120 with 121–130 `HAVE_DATA` durable through an index-only write; crash; activation connects 121–130 and flushes with a prune due (bound `min(122, lock)`) | locks loaded at open: ∅; T28's: {0} | startup lock order (Opus r2 2c) |
| S14 | the FT shape, with a **triggered** prune flush (Opus r3 1): synced subscriber; chainstate and locator durable at 10; V connects to 72 with no scripted flush; automatic checks due at each 16 KiB chunk allocation through height 63, where file 1 opens; then a manual prune at 72 | every automatic dry run is ∅ (bound ≤ 55 < 62), so **no flush**; at 72 the dry run under the predicted lock 69 is {0} (bound `min(72, 64, 69)` = 64), the log reads "limited pruning to height 69", and the flush selects {0}; T32's dry run under the durable lock 7 is ∅ | the prune decision; FA1, FA2, FA5 |
| S14b | **automatic target with V-ahead growth** (GPT r4 2), subscriber-free: heights 0–200 in files 0–3 (0–62, 63–125, 126–188, current 189–200); a due automatic check at `new` = 200 (bound 192). The target is set from the observed usage so that removing file 0 leaves usage **516 bytes** under it. V appends block 201 (1,032 bytes) between step 0 and step 7 | D = {0}, met; selection {0}, the target unmet by 516, so the check is **re-armed**. The next due check selects {1}. T39's re-run selector picks {0, 1} ⊄ D; T40 does not re-arm | `D ∩ eligible(actual)`; re-arming |

**Selection-protocol test** (`prune_selection_protocol`, both releases; GPT r4 2). It drives step 0 and step 7 over ST2's public selectors (`find_files_to_prune`, `find_files_to_prune_manual`) on GPT's two metadata fixtures (`/work/scratch/st3a-r4-gpt/automatic_prune_probe.rs`):
- **Side branch:** closed files with last heights 62 and 40, then the current file. The target is met by deleting one closed file. The predicted bound 597 gives D = {0}. With an actual lock of 47 the selection is `D ∩ eligible(47)` = ∅, and the check re-arms; the next check selects {1}. T39's re-run gives {1} ⊄ D. The manual control gives {0, 1} then {1}, a subset either way.
- **V-ahead:** files with last heights 62, 125, 188 and the current file, and a target 516 bytes above the usage left after removing file 0. D = {0}. After one 1,032-byte append the selection is {0} and the check re-arms; T39's re-run gives {0, 1}.

**Planted bugs.** † marks Core's own behaviour.

| ID | Planted bug | Scenario, kinds | Caught by | Also |
|---|---|---|---|---|
| T1 | B1 before the files barrier | S1 | R1 | D1(a) |
| T2 | index-only write without the files barrier | S1 | R1 | D1(a) |
| T3 | marker in Bk | S4 | R3 | — |
| T4 | evo deltas and `evo_tip` in Bk | S4 | R0 | R2 |
| T5 † | unlink before Bk | S5, both kinds | R0 | D1(e) |
| T6 † | barrier syncs only the current pair | S6 | R1 | D1(a) |
| T7 | recovery erases the marker in its first batch | S4 nested | R7a | R3 |
| T8 | recovery replays every `HAVE_DATA` block above the tip | S7 | R6 | R3 |
| T9 † | continue after a failed `sync_data` | S9 | R8 | R1 |
| T10 | subscriber commits its locator at `new` before B1 returns | S1 | R11a | D1(b) (`DurableTipAtLeast`) |
| T11 | Bk Buffered (pre-Bk barrier kept), then unlink | S5, PowerLoss | R0 | D1(e) |
| T12 | `HAVE_UNDO` set before the undo write returns | S1 | R1 | D1(a) |
| T13 † | **A1 rev1 as written:** selection before B1, its clears in B1 | S5, both kinds | R0 | D1(b) |
| T14 | B1 drops the dirty `block_files` record | S1 + R12 | R1 (position within size) | D1(d) |
| T15 | selection against V's tip | S5c, flush 2's window | R0 | D1(b) at B1′ |
| T16 † | lock from the subscriber's **in-memory** height (Core's `index/base.cpp:390-397` / `:437-444`) | S11 | R11b | D1(b) (standing) |
| T17 | B1 Buffered | S4 | R3 | — |
| T18 | **no pre-Bk barrier and Bk Buffered** (rev0's N4; GPT r2 2) | S4, PowerLoss | R3 | D1(f) |
| T19 | **DEC-59 literal:** selection before B1, B1 omits every prune-modified record | S5b | R0 | D1(b) |
| T20 | B1 before the transition's blocks were connected (applied) | S7 | R6(a) | — |
| T21 | an invalid block in the marker path | S7 | R6(a, d) | R3 |
| T22 | subscriber locator without its `sync_wal` | S1, PowerLoss at the locator's `Append` | R11c | D1(f) |
| T23a | L2: the adopter reads only the 8-byte header, marks `HAVE_DATA`, runs `sync_data`, commits (Opus r2 8) | S1, PowerLoss | R1 (view) | — (D1(a) is silent by design) |
| T23b | non-promotion: the adopter commits without syncing | S1, ProcessKill → PowerLoss | R1 | D1(a) |
| T24 † | evo committed after Bk | S4, ProcessKill | R0 | R2 |
| T25 | unlink after an `Applied` failure of Bk | S9 | R8 | D1(e) |
| T26 | **A1 rev1 as written:** Bk Durable with no pre-Bk barrier | S4, PowerLoss at Bk's `Append` | R3 | D1(f) |
| T27 | recovery's final batch with no barrier | S4 nested | R7a | D1(f) |
| T28 | lock registered only when the subscriber starts, after activation | S13 | R11b | D1(b) |
| T29 | no barrier before B1 while a presumed effect is pending | S1 | R12 (the continuation's fold) | D1(f) |
| T30 | lock at the locator's height, not at the fork | S11r | R11b | D1(b) |
| T31 | recovery erases index entries above the recovered tip | S1 | R5 | R6(e), with `c` = E's index (Opus r3 8a) |
| T32 | the prune decision uses the **pre-commit durable** lock (Opus r3 1) | S14 | FA1 | the precondition (selected = {0}), FA5 |
| T33 † | a syncing subscriber's cadence commit is **uncapped**: it commits its progress, as Core's `ThreadSync` (`index/base.cpp:187-192` / `:202-207`) | S11s, PowerLoss | R11a | D1(b) (`DurableTipAtLeast`), FA4 |
| T34 | Bk Buffered behind the pre-Bk barrier, on two split flushes: N8's variant (Opus r3 3) | S4b, PowerLoss at B1′'s `Append` | R3 | D1(f) (B1′'s `PreviousFinalizer`) |
| T35 | a synced subscriber classified by an instantaneous "has processed `new`" test: with a backlog it counts as syncing and never commits (Opus r3 2b) | S11q | FA3 | FA5, the precondition |
| T36 | the subscriber's rewind commit is Buffered | S11r continuation, PowerLoss | R11c(i) | — |
| T37 | **rev3's limit rule:** a synced subscriber at `h(L) + D_max` only makes a flush due, with no checkpoint (GPT r4 1) | S11d, b = 1 and 2 | FA6 (W waits on a blocked subscriber) | — |
| T38 | the checkpoint is Buffered and the subscriber resumes past its old limit before any barrier covers it | S11d b = 2, PowerLoss after data for 97–98 | R11c(i): heights 97–98 survive the blind rewind of `(0, 96]` | — |
| T39 | **rev3's final selection:** re-run the automatic selector under `min(predicted, actual)` (GPT r4 2) | S14b; the protocol test's fixtures | FA2 (selection ⊄ D) | — |
| T40 | no re-arm after a shortfall | S14b | FA2 (re-arm clause) | — |
| T41 | the checkpoint runs `sync_wal` and the locator commit **without submitting its staged data** (rev4 as written; GPT r5) | S11d b = 1, 2, both kinds, at crash points (c) and (d) | R11c(i): locator 96 over data through 49; the blind rewind of `(96, 192]` cannot recreate 50–96 | — (D1 is silent: the staged data was never submitted, so no pending batch is in the locator's scope) |

**Negative controls** (exhaustive, nothing reported):

| ID | Variant | Why it is safe |
|---|---|---|
| N1 | the correct toy, every scenario, both releases. It includes S5's image at Bk's `Append` with the clears retained (R5 passes, GPT r2 3), S1 under ProcessKill before a locator commit (R11c passes, Opus r2 4), and S3p **D1-clean through Bp and the unlink** (GPT r3 6). FA1–FA6 hold on every forward run, and S11d reaches its commit for b = 0, 1 and 2 | — |
| N5 | S12: ProcessKill → PowerLoss with an unread effect and blk appends | R7b, not R7a, applies |
| N6 | EIO on a pruned file's `remove` after a completed Bk | best-effort by API; R9 |
| N7 | `allocate` fails silently while the data still fits; S1's window `(20, 22]`, **two pending records** (Opus r3 6). The bound is the real product (GPT r4 5): blk sizes 9; rev sizes 4 for 240-byte undo records, {4800, 5040, 5120, 5280}, since 5,120 is a permitted intermediate sector-aligned size; one effect bit. So 2 × 9 × 4 = **72**. No undo size or alignment constraint is declared; the bound is computed from the fixture's sizes | advisory by API |
| N8 | Bk Buffered **behind** the pre-Bk barrier, no prune; `Mark::Durable` after B1 (GPT r2 2); **on S4 only** (Opus r3 3) | on S4, Bk is the last chainstate finalizer, so a lost Bk leaves B1's marker and recovery reaches `new`. A later B1 would overwrite that marker: that is T34 |
| N9 | S5 with a synced mandatory subscriber selects the same files as subscriber-free S5 (asserted), and stays **D1-clean through Bk and the unlink** (GPT r3 6) | after the commit step the lock is at `new − BUFFER − 1`, above `new − K`, and L = G, so the locator's path is empty |

rev1's N3 (`ChainStateFlushed` after B1) is now the protocol's normal step 5 (DEC-64). rev0's N2 and N4 became T17 and T18.

#### 6.2.1 Forward assertions (Opus r3 1, 2; rulings 3, 4; GPT r4 1–4)

Liveness cannot fail after a crash, so these are checked on every **forward** run, clean or witness, against the reference. A failure is reported as a violation of the forward run, with no crash needed, as D1 is.

| ID | Assertion | Planted bug |
|---|---|---|
| **FA1** Decision and cause | at every due prune check, the dry run D equals the reference's selection under the predicted post-commit lock, and **`flush_for_prune ⇔ D ≠ ∅`**. A full flush happens iff `flush_for_prune` or another cause (cache, interval, scripted, `DMax`) is due; an empty D never suppresses a flush another cause requires (S11d) | T32 |
| **FA2** Subset, re-arm and log | `PruneSelected.files` = `D ∩ eligible(actual)` ⊆ D, equal to D on a stable chain. After an automatic selection, `rearmed` holds iff the target is still unmet while D had met it. The "limited pruning" line names the reference's predicted-lock height | T32 (log), T39, T40 |
| **FA3** Synced commit | at every flush, each synced mandatory subscriber's `Mark::Locator` at `LCA(p, new)` precedes `PruneSelected` | T35 |
| **FA4** Cadence | a syncing subscriber commits at least once per `cadence` blocks of progress, each commit at `LCA(progress, T_ack)`, and it becomes synced at V's tip with a capped commit | T33 |
| **FA5** No freeze | in S11q and S14, the prune flush selects **every** file whose `nHeightLast ≤ new − K` | T35, T32 |
| **FA6** Finite progress | W never waits on a subscriber that is blocked at its limit, and every step-6 drain ends after at most `⌈(h(new) − h(F))/D_max⌉` checkpoints. A blocked wait is reported at once; it needs no timeout, because the toy steps W and subscribers on one thread | T37 |

Together these give "pruning never freezes permanently" **on a stable chain** (no reorg below `new` during a flush; DEC-69):
- A synced subscriber's lock reaches `h(LCA(p, new)) − BUFFER − 1` at every flush, which is `new − BUFFER − 1` on a stable chain (FA3), and its drain terminates (FA6).
- A syncing one's lock trails its progress by at most `cadence` blocks, or by the distance to `T_ack` (FA4).
- So the bound reaches `new − K` within one flush of every subscriber being synced (FA5).
- A reorg below `new` during a flush lowers that flush's lock to the fork, for example frozen A80, processed B80, fork 50 gives lock 47. The selection then shrinks within D, and an automatic check re-arms if that leaves the target unmet (FA2). The next flush on the new chain restores the bound.

### 6.3 Model self-tests (M0 exit 7)

The rev1 set M1–M18 is unchanged except:
- **M10** now also generates **scans**, and compares observation traces of per-key scan resolution with eager enumeration.
- **M6** includes scans over overlapping pending batches.
- **M16** adds the finalizer states of §6.4, cases 3–4.

### 6.4 Native regression (DEC-62 (1); `rocks_wal_nonprefix.rs`, feature `rocksdb`, G0)

Every case runs `RocksEngine` with production options in a TMPDIR on `/dev/shm`, takes a copy of the **live** directory, zeroes a region in the copy and reopens it.

1. **Non-prefix.** A Durable baseline; Buffered `b2` ending at a 32 KiB boundary; Buffered `b3` and `b4`. With `b2` zeroed, the result is `{baseline, b3, b4}`, in the subset domain.
2. **Checksum mismatch** in the same place: recovery stops at a prefix.
3. **Finalizer without a barrier** (T26 natively). Buffered coin batch `bj`, then the finalizer `bk` (`tip`, marker erase). Opus's `walbk` shows a Durable record is byte-identical to a Buffered one until its sync returns, so a Buffered `bk` stands in for an interrupted Durable one. With `bj` zeroed: `bk` kept, `bj` lost.
4. **Finalizer behind a barrier.** As case 3, with `sync_wal` between `bj` and `bk`: `bj` is retained whatever happens to `bk`.

The pin records RocksDB 11.8.1's behaviour. An upgrade or a BL-34 continuity check that changes it fails visibly, and the model's default stays subset until ST3b rules otherwise. All four cases go to ST3b as targeted `dm-log-writes` layouts.

### 6.5 Assumption witnesses

| ID | Adversary | Run on | Expected |
|---|---|---|---|
| W1 | **grant A-PREFIX** | T18, T22, T26, T27, T34, T36, T38; and T17 | T18, T22, T26, T27, T34, T36 and T38: **no post-crash violation**, because each depends on the false prefix assumption. D1(f)'s production diagnostic **still fires** for T18, T22, T26, T27 and T34, because D1 is unconditional (GPT r2 4). **T17 is still caught** (R4, R3; GPT r3 7): B1 is Buffered but acknowledged when it returns, and an empty prefix recovers `old`. No prefix-only T17 control is added |
| W2 | break A-ATOMIC (per-CF subsets, including an in-flight Durable B1) | N1 on S4 | **fails** R3 |
| W3 | break A-REN | S10 | **fails** R1 |
| W4 | grant ext4's row-3 bonus | MicroLog P3 | P3 **not caught** |
| W5 | break A-PSOW (the shared tail sector of blk and WAL) | N1 on S1 | **fails** R1 |

---

## 7. Core's crash behaviour and what it makes us survive

### 7.1 Core-equivalence table (both pins)

| Core (v23.1.8 / v24.0.0-rc.3) | Durability effect | In ST3a |
|---|---|---|
| `FlushStateToDisk` (`validation.cpp:2761-2911` / `:2969-3120`) | orchestrates | the toy's flush |
| Prune selection: `FindFilesToPrune*` (`blockstorage.cpp:169-185, 192-248` / `:165-181, 188-244`); prune locks `min(last_prune, height_first − PRUNE_LOCK_BUFFER − 1)`, null = no lock (`validation.cpp:2787-2793` / `:2995-3001`); `PruneOneBlockFile` zeroes positions (`:135-166` / `:131-162`) | against the **in-memory** tip and **in-memory** index heights; selection **before** the index write | DEC-64: after B1 and the subscriber commits, against `new`, locks from durable locators at the LCA; T13, T15, T16, T19, T30 |
| Index locks: `SetBestBlockIndex` → `UpdatePruneLock` (`index/base.cpp:390-397` / `:437-444`); moveback in `DisconnectTip` (`validation.cpp:3061-3065` / `:3295-3299`); refusal at init (`index/base.cpp:110` / `:122`) | in-memory, moved back on disconnect; a re-enabled index behind pruned data refuses | durable plus LCA (S11r); the moveback **log** emulated, nothing moves (S2); sanctioned refusal (S11c) |
| `ChainStateFlushed` only when `full_flush_completed` (`:2903` / `:3111`) | locators move only on full flushes | ours after B1, followed by the commit step |
| The prune trigger: `fFlushForPrune` only if the selection is non-empty (`validation.cpp:2812` / `:3020`); `fDoFullFlush` = ALWAYS ∨ cache large ∨ cache critical ∨ EvoDB cache critical ∨ periodic ∨ `fFlushForPrune` (`:2838` / `:3046`); `m_check_for_pruning` cleared after an automatic selection (`:2809` / `:3017`) and set at each chunk allocation (`blockstorage.cpp:665, 689` / `:718, 742`); the automatic selector stops at the usage target (`:228` / `:224`); "limited pruning to height" from the in-memory locks (`validation.cpp:2784-2798` / `:2992-3006`) | Core selects once, so it has no gap between deciding and selecting | step 0 keeps D; step 7 selects `D ∩ eligible(actual)` and re-arms on a shortfall; the prune cause is separate from the other full-flush causes; T32, T39, T40; S14, S14b, S11d; FA1, FA2 |
| `m_synced`: at init (`index/base.cpp:78` / `:90`), or after `ThreadSync`'s final `Commit` (`:154-158` / `:166-170`) | notification-driven once synced | `Synced` and `Syncing` modes; T35, S11q |
| `ThreadSync` commits every `SYNC_LOCATOR_WRITE_INTERVAL` = 30 s (`:24, 187-192` / `:29, 202-207`) | **uncapped**: a locator can be ahead of the chainstate's durable tip, tolerated by Core | capped at `LCA(progress, T_ack)`; T33 †, S11s, FA4 |
| `BaseIndex::ChainStateFlushed` (`:290-327` / `:332-369`), queued behind `BlockConnected`; a synced index ignores queued blocks that do not connect to its chain (`index/base.cpp:263-268` / `:281-286`) | a synced index commits its processed tip on each full flush; it has no `D_max` | step 6 drains synced subscribers along the path to `new`, with a Durable checkpoint at the `D_max` limit; FA3, FA6, T37 |
| `BaseIndex::Rewind` commits the fork (`:224-240` / `:238-259`), from `ThreadSync` and `BlockConnected` (`:160, 270` / `:172, 288`) | the locator moves back to the fork on a reorg; unsynced | the rewind commit, Durable and blind; T36 |
| `WriteFlag("prunedblockfiles")` (`:2814` / `:3022`) | unsynced | `pruned_files` in Bk or Bp |
| `FlushBlockFile` (`blockstorage.cpp:557-567` / `:585-605`) | current pair only; returns `void` on failure | ST2 `sync()` †; T6, T9 |
| `FileCommit`, `DirectoryCommit` | `fdatasync`; directory fsync result ignored | `sync_data`, `sync_dir` |
| `WriteBatchSync` (`txdb.cpp:267-277` / `:276-286`) | synced index | B1 |
| `UnlinkPrunedFiles` (`:580-588` / `:618-629`) | before coins; no directory fsync | step 11 †; T5 |
| `CCoinsViewDB::BatchWrite` (`txdb.cpp:104-165` / `:113-174`) | unsynced; marker first; `-dbcrashratio` (`:147-154` / `:156-163`) | sub-batches; ProcessKill at every event |
| `CommitRootTransaction` (`evo/evodb.cpp:56-64` / `:98-108`), after coins (`validation.cpp:2884→2889` / `:3092→3097`) | unsynced, separate DB | evo in B1; T4, T24 |
| Rotation snapshot lookup (`llmq/utils.cpp:496-503` at v24) | a missing snapshot takes another path silently | presumed effect; T29 |
| `ScanAndUnlinkAlreadyPrunedFiles` (v24 `blockstorage.cpp:412-417`) | startup unlink | ST2's scan, both releases |
| `ReplayBlocks`/`RollforwardBlock` (`validation.cpp:4879-4954`, `:4772-4877` / `:5222-5305`, `:5180-5220`) | an indexed read (`:4779-4781` / `:5187-5190`); a no-op without heads (`:4887` / `:5230`) | coins-only §3.5.5 |
| "Unable to replay blocks…" (`init.cpp:2066` / `node/chainstate.cpp:251-252`); prune refuses `-reindex-chainstate` (`init.cpp:1204` / `:1256`) | — | R0 |

### 7.2 What Core survives, and what it does not

**Process kill.** Core survives a kill at `feature_dbcrash`'s coin sub-batch boundaries only: that test has `-dbcrashratio`, never prunes, and is an `EXTENDED_SCRIPTS` test. Two windows fail under SIGKILL:
1. **The prune index window.** Selection uses the in-memory tip, and the cleared positions are synced before the coins. `GetBlockPos()` then returns null (`chain.h:208-217` / `:216-225`), so `RollforwardBlock` fails. The result is "Unable to replay blocks…", and in prune mode only a full `-reindex` gets out.
2. **The coins→EvoDB window.** A kill between `:2884`/`:3092` and `:2889`/`:3097` leaves coins at `new` with no marker and EvoDB at `old`. The node stops with "Found EvoDB inconsistency" (`:2002, 2322` / `:2288, 2527`).

**Power loss** also loses:
- unsynced coins and EvoDB;
- lagging undo;
- the two windows above;
- the flush that continues after a failed fsync;
- LevelDB's skipped zero records, or its refusal to open on a mid-log checksum mismatch;
- `fclose` errors, which are ignored.

**Prune locks** (DEC-64 context). Core's lock follows in-memory index heights, so an index's durable state can fall behind pruned data, and Core then refuses at init. Core's `ThreadSync` locator can also run ahead of the chainstate's durable tip. Ours follows durable locators after a commit step, and cadence commits are capped, so neither state is reachable for subscribers enabled throughout.

### 7.3 The bar

1. **Direction:** files → index → coins → EvoDB. T5, T6, T9, T13, T16 and T24 reproduce Core's choices, and each is caught.
2. **Floor:** ProcessKill at every event.
3. **Ceiling (US-NODE-04):** every item of §7.2 is a violation. Our protocol survives both process-kill windows: evo is atomic with the marker in B1; Bk carries the tip, the marker erase and the clears; B1 is written before selection. **T13, T19 and T24 under ProcessKill prove it**, now that S5 is reachable and subscriber-free.
4. **Observable equivalence:**
   - **Errors** use Core's strings (`blockstorage.cpp:553, 562` / `:581, 600`; `validation.cpp:2858, 2885, 2908` / `:3066, 3093, 3117`; the startup message).
   - **Pruning:** with every mandatory subscriber synced, **on a stable chain** (no reorg below `new` during the flush; DEC-69), our bound is never lower than Core's, whose locks follow queue-lagging in-memory heights. Both are `new − K` whenever the lag is under `K − BUFFER − 1` (277 in production). A reorg below `new` during the flush can make ours lower for that flush: frozen A80, processed B80 and fork 50 give 47 where Core's can be 77. While a subscriber syncs, ours is stricter (the S11 shape). The prune **cause** matches Core's `fFlushForPrune`: `flush_for_prune` iff the selection, here the dry run, is non-empty (S14). Other causes still force a full flush with an empty selection (S11d).
   - **The FT:** CH8 cross-checks all six `feature_index_prune` assertions under our order: 689 and 367, 735, 2489 and 2207, and "moved back to 2480" through the emulated log (§3.6).

### 7.4 Fidelity

| Core behaviour | Modelled? | How |
|---|---|---|
| Flush order, head-blocks protocol | yes | toy knobs |
| `-dbcrashratio` | yes, exhaustively | ProcessKill at every event |
| Both SIGKILL windows | yes | S5, S5b, S4 under ProcessKill |
| `FileCommit`, `DirectoryCommit` | yes | `sync_data`, `sync_dir` |
| In-memory prune locks | as a bug (T16) | DEC-64 uses durable locators after commit |
| The prune-flush trigger | yes | step 0's dry run (S14) |
| `m_synced`, `ThreadSync`'s cadence, `Rewind`'s commit | yes, cadence capped | `Synced`/`Syncing` modes; the uncapped cadence is T33 |
| Prune-lock moveback | log only | emulated `DisconnectTip` lines (S2) |
| LevelDB log recovery | no | RocksDB's non-prefix behaviour is the default |
| Separate coins and EvoDB databases | no | T4, T24 |

### 7.5 Pin differences in scope

- **The rev-finalize rule** (BL-29) is ST2's `Release`; S6 and S10 cover it.
- **CONS-28:** v24 behaviour for every profile.
- **The startup scan:** v24 only.
- **Assumeutxo lifecycle:** NG-6.
- **Prune locks, the prune trigger, `ThreadSync`'s cadence, `Rewind`, and all six `feature_index_prune` assertions** are the same at both pins; only the line numbers differ.
- No `ReleaseProfile` field is needed beyond ST2's `Release`.

---

## 8. Performance

**Node metrics: none**, because the code is test-only. The `cargo tree` test of §3.1 enforces that.

### 8.1 Calibration

Unchanged from rev1 (`/work/scratch/st3a-design/calib*.log`; agentbox, load 30–41). The constants:
- debug with DEC-63's overrides: forward `f = 0.2 ms`/block; engine put/get ≈ 1/0.6 µs; digest ≈ 1 µs/entry; 1 KiB `read_block` ≈ 12 µs; `read_undo` ≈ 8 µs;
- release ≈ 0.25 × debug for branch costs.

GPT r2 reproduced every row of the arithmetic (`budget-arithmetic.txt`). These are forecasts; Phase B re-measures.

### 8.2 PR gate (debug + DEC-63 overrides; budget 60 CPU-s, work-capped)

**Per-image bound** = 2^(forced batch bits) × size values × 2^(forced directory bits), with:
- forced batch bits = at most 1 in-flight chainstate finalizer, plus R13's pending effects, plus at most 3 subscriber bits;
- size values = the **product over the files read** of their size domains, ≤ 3 with successful preallocation (Opus r3 6). N7, where allocation fails, has its own bound;
- directory bits = pending ops on names looked up.

Coin bits are never forced (§4.3). B̄ is the mean PowerLoss branches per image; rows count E × (B̄ + 1) branches.

| Item | E | B̄ (bound) | c_b | Rel. | CPU-s |
|---|---|---|---|---|---|
| S1: worst image between 25 and the pre-B1 barrier has 2 effects + 1 subscriber batch → 2³; at the commit step, 2 data + locator → 2³; × size 3 × dir 4. The bound is about **10× conservative**: the reachable maximum is ≈ 8–16 (at most 3 batch bits, size ≤ 2 at block 15's `Grow`, directory bits only before the first sync; Opus r3) | 165 | 7 (96) | 0.8 ms | 2 | 2.11 |
| S2 | 60 | 8 (96) | 1.0 ms | 2 | 1.08 |
| S3 | 50 | 7 (48) | 0.5 ms | 2 | 0.40 |
| S3p: replay none; R1 over 73 blocks | 25 | 8 (48) | 2.5 ms | 1 | 0.58 |
| S4: 1 finalizer + 3 subscriber bits, × size 3 | 30 | 5 (24) | 1.0 ms | 2 | 0.36 |
| S4b: S4's dims over two flushes | 60 | 5 (24) | 1.0 ms | 2 | 0.72 |
| S5: 1 finalizer × size 3 × 2 unlinked names; replay 67 blocks, R1 73 | 30 | 8 (24) | 3 ms | 1 | 0.81 |
| S5c: two flushes | 60 | 8 (24) | 2 ms | 1 | 1.08 |
| S6 | 40 | 12 (48) | 1.5 ms | 2 | 1.56 |
| S7 | 40 | 6 (24) | 1.0 ms | 2 | 0.56 |
| S8 | 3 | 6 | 0.8 ms | 2 | 0.05 |
| S9: 70 non-prune fault runs × (4 + 3 × 7 × 0.8) ms × 2, plus 3 prune faults on S5 dims × ≈ 95 ms | — | — | — | 2 | 3.2 |
| S10, S10b | 16 | 8 | 0.8 ms | 2 | 0.23 |
| S11: subscriber catch-up of 110 blocks in c_b | 30 | 12 (48) | 4 ms | 1 | 1.58 |
| S11s: ±5 events around each capped commit and the flushes at 40 and 60; catch-up ≤ 40 blocks | 60 | 7 (48) | 2 ms | 1 | 0.96 |
| S11q: the flush at 72, step 0 to the unlink; R1 over 73 blocks | 40 | 8 (48) | 3 ms | 1 | 1.08 |
| S14: the triggered flush at 72 | 30 | 8 (48) | 3 ms | 1 | 0.81 |
| S11d: three runs (b = 0, 1, 2), the `DMax` flush from step 0 to the final commit, including each checkpoint's data submit; catch-up and R1 over ≤ 99 blocks | 96 | 7 (48) | 3.5 ms | 1 | 2.69 |
| S11d nested depth 1 around the checkpoint: 10 outer images (b = 1, 2: crash points (a)–(e)) × 2 kinds × 20 recovery events × 4 branches × 1 ms | — | — | — | 1 | 1.60 |
| S14b: the prune flush at 200 with the V-ahead append; R1 over 201 blocks | 30 | 8 (48) | 5 ms | 1 | 1.35 |
| S12 (N5): 20 outer × 15 nested × 7 × 0.8 ms (Opus r2 14) | — | — | — | 1 | 1.68 |
| N7: two pending records, bound 72 (GPT r4 5) | 6 | 40 (72) | 0.8 ms | 1 | 0.20 |
| N8 on S4 | 30 | 5 (24) | 1.0 ms | 1 | 0.18 |
| N9: S5 + about 3 subscriber bits (Opus r3 8b) | 30 | 8 (48) | 3.5 ms | 1 | 0.94 |
| Blind rewinds: ≤ 96 deletes ≈ 0.1 ms on each of ≈ 8,500 subscriber branches | | | | | 0.85 |
| R12 on each image's all-survived branch (≈ 1,010 images × 2 kinds × 1.5 ms) | | | | | 3.02 |
| **Clean, exhaustive** | | | | | **29.7** |
| Witness searches, narrowed windows: 21 non-prune, non-nested T-bugs ≈ 0.06 s; 6 prune bugs (T5, T11, T13, T15, T19, T25) ≈ 0.17 s; 3 lock bugs (T16, T28, T30) ≈ 0.25 s; T7 and T27 with outer fixed ≈ 0.15 s; T6 under both releases; P1–P8 ≈ 0.03 s; T33, T34, T36 and T38 ≈ 0.1 s each; T32, T35, T39 and T40 one forward run each, and T37 two (b = 1, 2), ≈ 0.05 s each; T41 ≈ 0.1 s | | | | | 4.6 |
| Replays of 50 checked-in repros (36 × 10 ms + 14 prune/lock × 35 ms) | | | | | 0.85 |
| Precondition dry runs (selections for S3p, S5, S5b, S5c, S11, S11r, S11q, S13, S14, S14b, N9; S4b's, S11s's and S11d's asserts) and `prune_selection_protocol` (2 fixtures × 2 releases) | | | | | 0.2 |
| FA1–FA6 on every forward run | | | | | 0.1 |
| MicroLog N0 | | | | | 0.3 |
| M1–M18 | | | | | 5.0 |
| M13 minimisation self-test | | | | | 0.5 |
| Twin, 10² cases | | | | | 0.5 |
| W1 (8 bugs, exhaustive on narrowed windows under prefix, T27's outer fixed) ≈ 1.1; W2 0.1; W3–W5 0.3 | | | | | 1.5 |
| Native regression (4 cases, `/dev/shm`) | | | | | 0.6 |
| **Total** | | | | | **≈ 43.8** |

**CP2 calibration (DEC-101 Q1).** Measured at 30462db0 in debug with DEC-63's overrides, CPU-s per test process (`/usr/bin/time`), at a load average of 40–53. The toy suite is 223 G instructions, about 37 CPU-s on an idle core. Rows are replaced as follows:

| Item | §8.2 estimate | Measured |
|---|---|---|
| S1 (synced subscriber; non-zero padding, I5) | 2.11 | 2.79 |
| S2 | 1.08 | 1.19 |
| S3 / S3p | 0.40 / 0.58 | 0.10 / 0.64 |
| S4 / S4b | 0.36 / 0.72 | 0.33 / 2.54 |
| S5 | 0.81 | 0.64 |
| S5c | 1.08 | 3.15 → **nightly** (DEC-101 Q5) |
| S6 / S7 / S8 | 1.56 / 0.56 / 0.05 | 0.41 / 0.15 / 0.05 |
| S9 (non-prune and prune faults) | 3.2 | 1.08 |
| S10 / S10b (with the NoAdopt control) | 0.23 | 0.33 / 1.19 |
| S11 / S11s / S11q / S14 | 1.58 / 0.96 / 1.08 / 0.81 | 0.57 / 1.17 / 0.59 / 0.57 |
| S11d (b = 0, 1, 2) | 2.69 | 1.83 |
| S14b | 1.35 | 3.72 → **nightly** (DEC-101 Q5) |
| S12 (N5; nested, I21) | 1.68 | 3.25 |
| N7 (bound 216, I4) | 0.20 | 1.20 |
| N8 / N9 | 0.18 / 0.94 | 0.30 / 1.45 |
| S2b, review regressions, DEC-80 controls | — | 1.2 |
| Witness searches (T1–T42 and controls) | 4.6 | ≈ 9.5 |
| Checked-in repros (41 toy + 11 micro) | 0.85 | 1.91 + 0.1 |
| **Toy suite** | ≈ 35.5 | **45.0** (≈ 38.1 after moving S5c and S14b) |

**What runs nightly instead, and why:**
- full minimisation (up to 2,000 replays per bug; the PR gate replays the checked-in repros);
- production-scale prune (`K = 288`);
- S5b, S11r (with its rewind continuation), S11c and S13 clean, plus the second release of S3p, S5, S5c, S11, S11s, S11q, S11d, S12, S14 and S14b; their planted-bug witnesses stay in the PR gate;
- nested depth beyond S4, S12 and the witnesses;
- `Extended` and `Byte` granularity;
- R12 on every branch.

The PR gate asserts the `Work` counters and zero truncation.

### 8.3 Work accounting

`Work` counts:
- events, fault reruns, images, branches, recoveries;
- bytes read and bytes hashed;
- history entries scanned;
- cloned sectors and pages, scanned engine entries.

Each scenario's cap is 2× its Phase B measurement. A capped run reports `truncated` and fails. CPU time is reported but does not gate.

### 8.4 Nightly (release; 1.5 CPU-h approved, DEC-63)

| Item | Arithmetic | CPU-s |
|---|---|---|
| Clean matrix: every scenario, both releases, `Realistic` + `Extended` | (PR clean 29.7 + second releases 12.6 + S5b 1.6 + S11r 4.2 + S11c 1.0 + S13 3.2 ≈ 52.3) × 0.25 × 2.5 | ≈ 33 |
| Production-scale prune and lock (`K = 288`, buffer 10, `D_max` 1,024, ST2's `find_files_to_prune_manual`; 3 KiB blocks, 21 per file; S3p, S5, S5b, S5c, S11, S11q, S13, S14, S14b) | 9 × 2 releases × 30 images × 13 branches × 8 ms | ≈ 56 |
| Nested depth 1 on S1–S4 and S7 | (1,320 + 540 + 400 + 180 + 280) × 2 = 5,440 outer × 240 × 0.25 ms | ≈ 326 |
| Nested depth 1 on S11 | 780 outer × 240 × 1 ms | ≈ 187 |
| Nested depth 2 on a seeded 0.25% of outer branches | 16 × 240 × 240 × 0.4 ms | ≈ 369 |
| Sweep: 2 × 10⁵ cases | × 3 ms | ≈ 600 |
| Full minimisation, worst case | 36 × 6 s + 14 × 30 s | ≤ 636 |
| Witness searches with 3–8 KiB records, `Extended` + `Byte` | 50 × 0.5 s | ≈ 25 |
| R12 on every branch of the clean matrix | | ≈ 55 |
| S11d nested depth 1 at production `D_max` 1,024, both releases | | ≈ 10 |
| M10 at scale | | ≈ 30 |
| **Total** | | **≈ 2,327 (0.65 CPU-h)** |

The factor-2 upper bound is ≈ 1.29 CPU-h, inside DEC-63's 1.5. Components run in the listed order on up to 4 cores, and any capped component is reported `truncated`. A truncated component is never clean (DEC-185 (1)). `nested_depth1_extended` (I39, I43) is outside this budget: DEC-185 (2) has it enumerated exhaustively, uncapped, in shards that run as low-priority units, and its cost is many CPU-hours (I43).

**CP3 calibration.** One green nightly at 99fce744. It ran in release, one component at a time, with CPU-s per test process from `/usr/bin/time`, at a load average of 100–145. Under that load, CPU-s overstates idle cost by about 1.4× (the PR gate's 229 G toy instructions cost 55 CPU-s here, against ≈ 37 idle).

| Component | §8.4 estimate | Measured |
|---|---|---|
| Clean matrix, with R12 on every branch and `Extended` | 33 + 55 | 477.6 |
| Production-scale prune and lock | 56 | 252.9 |
| Nested depth 1 (S1–S4, S4b, S7, S11) | 326 + 187 | 461.7 |
| Depth 2 on a 0.3% sample | 369 | 192.3 |
| Sweep, 2 × 10⁴ cases (I32) | 600 | 625.5 |
| Full minimisation of the 43 planted bugs | ≤ 636 | 295.9 |
| Witness searches, `Extended` + `Byte` | 25 | 295.2 |
| S11d nested at `D_max` 1,024 | 10 | 547.5 |
| M10 at scale | 30 | 52.3 |
| Clean runs moved from the PR gate (S5b, S5c, S11r, S13, S14b, S11c; DEC-101 Q5) | (in the clean matrix) | 11.0 |
| **Total** | **≈ 2,327 (0.65 CPU-h)** | **3,212 (0.89 CPU-h)** |

The total is inside DEC-63's 1.5 CPU-h, but a factor-2 bound (1.78) no longer fits. S11d (55 × its estimate) and the witness searches (12×) account for most of the gap. The first 2 × 10⁴ sweep found the two I34 issues; the re-run at 99fce744 is clean. The PR gate at e0db8d66 is 250 G instructions, 61.5 CPU-s at load 120 and ≈ 42 idle.

---

## 9. Verification mapping

| Layer | ST3a |
|---|---|
| L0 | none |
| L1 | no new O1 entry points |
| L2 | M3's model level runs CH2's `Sut` in capture mode with sampling |
| Fuzz | `power_loss_toy`, through `run.sh` only (N5; DEC-59 Q1, Q6); the twin runs every PR |
| Native | `rocks_wal_nonprefix` (4 cases) in G0 |
| CRASH | this task is the model level |

---

## 10. Implementation phases (20–25 agent-days, DEC-63)

The branch is stacked on `node/st2-block-files`. Checkpoints come after Phase B (M0 exit 7 closes there) and after Phase E, each with a fix round.

- **Phase A (5–6 days):** `faulty` and the `MemoryEngine` seams; `World` and `FaultyEngine` (subset overlay, per-key scans, `Append`/`Barrier`); `FaultyFlatFiles`; the native regression (4 cases). **Acceptance:** M1–M9, M11, M15–M18.
- **Phase B (4 days):** explorer (rule L, both modes, preconditions, `Work`); repros and minimisation; MicroLog. **Acceptance:** P1–P8, N0, M10, M12–M14; the calibration re-derived. **Checkpoint 1.**
- **Phase C (7–8 days):** ToyChain:
  - the DEC-64 flush, with Bp, and step 0's prune decision;
  - the presumed-effect barrier;
  - recovery with lock loading;
  - subscriber modes (synced with backlog, syncing with the capped cadence), rewind commits, the blind rewind and catch-up, the moveback-log emulation;
  - FA1–FA6; the step-6 checkpoint and path drain; step 0's retained D and step 7's `D ∩ eligible(actual)` with re-arming;
  - the V-ahead knob and the reference;
  - views, the R-checks and the standing D1.

  **Acceptance:** N1 and N5–N9; T1–T41 found, with preconditions; FA1–FA6 green on N1; `prune_selection_protocol` green at both releases; zero truncation.
- **Phase D (3 days):** nested crashes; `Extended`; W1–W5; the nightly. **Acceptance:** T7 and T27; the W expectations, including T17 caught under W1; one green nightly.
- **Phase E (2 days):** the fuzz target; the CH2 contract docs. **Acceptance:** runner smoke on both pins, then the DEC-59 Q6 budget. **Checkpoint 2**, then merge after ST2.

---

## 11. Test plan (summary)

| Where | Tests | Runs |
|---|---|---|
| `power_loss_model.rs` | M1–M18 | PR |
| `power_loss_micro.rs` | P1–P8, N0 | PR |
| `power_loss_toy.rs` | S1–S14 with S3p, S4b, S5b, S5c, S11r, S11c, S11s, S11q, S11d and S14b (PR scope), T1–T41, N1 and N5–N9, FA1–FA6, W1–W5, `prune_selection_protocol`, 50 repros | PR |
| `power_loss_toy_twin.rs` | 10² cases | PR |
| `rocks_wal_nonprefix.rs` | 4 cases | PR (G0) |
| `power_loss_deep.rs` | §8.4 | nightly |
| `node-fuzz` `power_loss_toy` | via `run.sh` | merge budget; O3 |
| `cargo tree -e features,no-dev --target all` | `faulty` and `raw-bytes` absent from runtime binaries | PR |

---

## 12. Proposed A1 errata (BL-26 batch) and deviations

**Subset semantics** (DEC-62 (1), DEC-63; E1–E11 as in rev1, with E5 refined):

| # | A1 location | Proposed |
|---|---|---|
| E1 | §3.5.1 (line 1835), "a **prefix** of the commit order" | between completed barriers, any **subset** of Buffered batches, each atomic (BL-34) |
| E2 | §3.5.3 I2 (line 1850) | every state with all batches covered by completed barriers plus any subset of pending ones |
| E3 | §3.5.4 step 5 and "Why two Durable commits" (line 1879) | `sync_wal`, then a **Durable** Bk (E21); delete "a Buffered Bk would also be crash-safe": the next flush's B1 presumes Bk |
| E4 | §3.5.5 step 4 | `sync_wal` before the final batch |
| E5 | §3.5.7 effects (line 1916) | completed barriers make earlier effects durable; before that any subset may survive. Effects are classified **independent** or **presumed** (CH7a); a presumed effect pending at B1 requires `sync_wal` before B1. "No more durably than Core" yields to US-NODE-04 here (Opus r2 6a) |
| E6 | §3.5.7 subscribers | `sync_wal` before each locator commit; blind-rewind-then-catch-up recovery (R11c, E22); classification and cadence (E19) |
| E7–E9 | §3.5.4 "Prefixes", §3.5.8, §6 D27 | subset wording |
| E10 | ST1 docs (BL-34) | subset wording in `engine.rs`, `memory.rs`, `model.rs` |
| E11 | §2.9 live copy | a prefix because a copy has no holes, not because of §3.5.1 |

**Prune and locks** (DEC-59 Q9, DEC-62 (2), DEC-64):

| # | A1 location | Proposed |
|---|---|---|
| E12 | §3.5.4 steps 3–7 | Order: [prune decision: a dry run under the predicted post-commit lock, E20] → files barrier (first dirty-info take) → [`sync_wal` if a presumed effect is pending] → **B1** (all dirty records, pre-prune by construction; marker; evo) → `ChainStateFlushed` → synced mandatory subscribers drain along the path to `new` (checkpointing at their `D_max` limit) and commit locators at `LCA(p, new)` behind their `sync_wal`, which is `new` unless V reorged below it (E19, DEC-69) → **prune selection** `D ∩ eligible(actual)` against `new` (E20) → coin sub-batches → `sync_wal` → **Bk** (last coins, second dirty-info take, prune clears, `pruned_files`, `tip = new`, marker erase). A one-batch flush that prunes ends with **Bp**. Unlink after Bk or Bp returns |
| E13 | §3.5.4 step 5 | Bk or Bp carries the clears, the reset infos and `pruned_files` |
| E14 | §3.5.7 + CH8 brief | lock = min over mandatory subscribers of `height(LCA(durable locator, new)) − PRUNE_LOCK_BUFFER − 1`, after the commit step; never null after it (a subscriber with no locator contributes its start height); loaded at open before activation; protects block and undo; a re-enabled subscriber behind pruned data gets Core's refusal. With every subscriber synced, on a stable chain (no reorg below `new` during the flush; DEC-69), never lower than Core's bound, which follows queue-lagging in-memory heights, and equal whenever the lag is under `K − BUFFER − 1`; stricter while one syncs, and for one flush when V reorgs below `new` during it. The moveback line is emulated, log-only. **CH8 acceptance cross-checks all six `feature_index_prune` assertions** (689 / 367, 735, 2489 / 2207, "moved back to 2480") |
| E15 | §3.5.1, truncation durability | `fdatasync` persists the size from `ftruncate`; a directory fsync persists names |
| E16 | §3.5.4 step 6 | `ChainStateFlushed` is emitted after **B1** returns (DEC-64), not after Bk; safe because B1's marker guarantees recovery reaches `new` |
| E17 | §2.8 engine configuration | `manual_wal_flush = false` is part of the contract; ST1 sets and asserts it (BL-34 note) |
| E18 | §3.5.4 W and V concurrency | a barrier covers only writes that returned before it started; W must not count V's in-flight writes as covered (§2.4.1) |

**rev3 additions** (Opus r3, GPT r3; DEC rulings of this round):

| # | A1 location | Proposed |
|---|---|---|
| E19 | §3.5.7 subscribers ("only after the `ChainStateFlushed` of a flush that covers it") | Subscribers are **synced** or **syncing**, as Core's `m_synced`. A synced subscriber drains along the path p → `LCA(p, new)` → `new` in the commit step, dropping queued events off that path, and commits at `LCA(p, new)`. A syncing one commits on a bounded cadence, each commit capped at `LCA(progress, T_ack)`, and becomes synced with a capped commit at the tip. Before processing a disconnect below its locator, a subscriber makes a Durable rewind commit at the fork. **Backpressure:** no data is written above `h(L) + D_max` for the barrier-covered L. At the limit, a syncing subscriber makes a capped commit, or makes a flush due if that cannot advance L; a synced one makes a flush due outside the commit step, and inside it **checkpoints Durably at its processed tip** after B1, which raises L by `D_max` (GPT r4 1). **Every locator commit at X first submits all staged data through X**, waits for it to return, then runs `sync_wal`; the subscriber tracks its processed and submitted-data tips separately (GPT r5). Standing block and undo requirements come only from the durable locator's path to the chainstate's guaranteed recovery tip (Opus r3 2, GPT r3 6) |
| E20 | §3.5.4 prune trigger | When a check is due, a side-effect-free dry-run selection under the predicted post-commit lock (synced at `new`, syncing at `height(LCA(durable, new))`) is **retained** as D, with whether it met the automatic target. **`flush_for_prune ⇔ D ≠ ∅`**, and a full flush happens iff `flush_for_prune` or another cause (cache, interval, scripted, the `D_max` limit) is due (GPT r4 3). The final selection is **`D ∩ eligible(actual)`**, never a fresh selector run (GPT r4 2). After an automatic selection, the check re-arms iff the target is still unmet while D had met it. "limited pruning to height" is logged from the predicted lock; the `DisconnectTip` moveback line is emulated, log-only (Opus r3 1, 4) |
| E21 | §3.5.4 step 5, §3.5.5 steps 1 and 4 | Bk, Bp and recovery's final batch are **Durable**. B1's scope includes the previous chainstate finalizer (Opus r3 3). Recovery runs `sync_wal` on each engine before its first write, so no finalizer of the new epoch presumes an unbarriered batch left by a ProcessKill |
| E22 | §3.5.5 step 4, §3.5.7 recovery | Replay and subscriber rewind are **blind**: they never read the keys they overwrite or delete. The rewind deletes `(L, L + D_max]` (Opus r3 7; Q16) |

**Implementation errata** (checkpoint-1 reviews, on the implementation branch):

| # | Section | Erratum |
|---|---|---|
| I1 | §5.3 D1(d) | The replacement rule above: byte-identical writes are not D1(d); copy-over always is; rename-over is exempt only over a durable, byte-identical source range (GPT cp1 r2 R2-1, Opus cp1 r2 minor A) |
| I2 | §5.3 D1(a) | "its length" includes every **pending** size: a pending size below a referenced range's end (a truncate, a copy's shrink) makes the range not durable (Opus cp1 r2 minor A) |
| I3 | §5.3 | D1 reads the branch's decided cells and never decides one; a record whose presence is undecided protects its ranges, and a failure of it is held pending until it is decided present. D1 runs at boot, after every barrier, before a mutation once decisions changed, and when recovery ends (GPT cp1 major 1; Opus cp1 majors 2–3) |

**Phase C implementation errata:**

| # | Section | Erratum |
|---|---|---|
| I4 | §6.2 N7, §8.2 | N7's bound is **216**, not 72. R1 reads block 20's record and undo, whose last sectors it shares with block 21's pending ones, so those two cells (3 and 2 versions) are forced too: 9 × 4 × 3 × 2. The effect bit is never forced, because R13 reads only effects a completed barrier covers. N7 runs without R12, whose continuation overwrites block 21's first sector |
| I5 | §6.2 Blocks | The toy's `OP_RETURN` padding is non-zero and differs per block. With zero padding, a lost or stale sector inside a block body read back unchanged, so R1 could not see a torn record (found by T23a) |
| I6 | §5.1 R7a | The uninterrupted recovery's projection is taken before R12's continuation, which connects and flushes one more block |
| I7 | §6.2 S10b | ST2 syncs the current pair at every barrier and finalizes a file at its rollover, so `adopt_existing_files` matters only for files a writer that never syncs left behind, and for their directory entries. S10b imports blocks 11–70 across the rollover through such a writer. Its control, with adoption skipped (`NoAdopt`), is caught by R1 and D1(a) |
| I8 | §6.2 T23a/b | The adopter is recovery's scan of the records past each file's durable size, taken from `len()` (rev1's T23). Both run on S1's prefix `c10;f;c2`, crashing after block 12. After a PowerLoss the surviving sectors **are** the durable image: undecided cells with nothing pending. T23a's `sync_data` therefore has nothing to promote, L2 carries those cells trivially, and D1(a) stays silent, which T23a's test asserts. Only R1's read of the body sees the tear. So under a single PowerLoss, T23a and T23b behave the same. T23b's non-promotion shows only after ProcessKill, then a nested PowerLoss |
| I9 | §6.2 S11c | Disabling the index is recorded durably in its own store. While that record is present, the extractor waives the locator's requirements, as Core holds no lock for a disabled index. A re-enabled catch-up that succeeds deletes it. R11b's scope reads it |
| I10 | §5.1 R11c, DEC-80 Q1 (a) | The locator value carries the epoch and commit number that wrote it. The archive rule is: every present archive row has the block's value, and the rows of blocks archived in that epoch before that commit are present |
| I11 | §5.1 R5 (c) | (CP2.1, restoring the normative test.) A recovery flush's prune resets are entitled only for a selection whose every file's E record ends at or below its bound, whose bound is at or below `new − K` and the lock the epoch's durable locator gives, and whose Bk or Bp barrier completed in the epoch. `PruneSelected` records the actual lock bound |
| I12 | §6.2 S11s | A commit at the current locator writes again, as Core's `Commit` does, so S11s's commits are 20, 40, 40, then 60, as the row says |
| I13 | §8.2 | S11q's window starts at the flush at 72, and T28's at the index-only write. S5b, S11r and S13 run clean nightly; their preconditions and planted-bug witnesses stay in the PR gate |
| I14 | §5.1 R12 | The continuation runs whenever recovery opened, even after other violations, and D1 over its own writes is collected (tagged R12). Before this, T14's "Also D1(d)" was unreachable: R1 always fired first, and the continuation's D1 violations were dropped |
| I15 | §5.1 R3 | A recovered tip on a chain that contains an invalid block is an R3 violation: the reference has no state there. This is T21's "Also R3" |
| I16 | §5.1 R11c (ii), §5.5 step 5 | After a catch-up, R11c (ii) compares with the reference at V's tip, not with the subscriber's own chain. It requires that the catch-up reached that tip, and that the state key equals the reference at the locator. Recovery's catch-up that stops at its limit now makes a `DMax` flush and continues (§5.6 item 6). Before this it stalled silently |
| I17 | §6.2.1 FA3–FA5 | FA3's step-6 window closes at the subscriber's end-of-step mark, and only commit-step and checkpoint locators count. FA4 adds its liveness half: at most `cadence` blocks of progress between cadence commits, and a capped commit on turning synced. FA5 also compares the final selection with every eligible file. FA1 checks the dry run's lock against the reference formula; recomputing D needs the file layout, which the reference does not model |
| I18 | DEC-80 Q1 (a) | The archive rows a locator requires include those its epoch's booted locator required, back through earlier epochs: the set is monotone. Each recovery logs its booted locator's commit tag |
| I19 | §5.5, §5.6 item 7 | Init's prune check decides at recovery's end if no activation connect did. It is still under the locks loaded at step 0 |
| I21 | §4.2 nested | A nested search inherits only the cells the outer **recovery** decided, not the ones its checks or R12 forced later. It runs once per distinct set of such decisions in an image. Before this, the inner search pinned check-forced cells, and R7a's "stabilised" counted them as the outer recovery's |
| I22 | §4.8 | A repro records `r12 off` when the scenario runs without R12's continuation (N7), so it replays with the same checks |
| I23 | §5.1 R10, R13 | R10 hashes the linked inode's content right after a flush; its control, `LinkedWrite`, is caught. R13 counts barriers in the effect's own epoch only. That is safe, but narrower than R7b's combined trace |
| I24 | §6.2 T7, §5.1 R7a | R7a is evaluated for both inner crash kinds (T27 diverges only under an inner PowerLoss), and an interrupted recovery that cannot complete where the uninterrupted one did is a non-convergence too. Replay evaluates R7a. T7's "Also" is R0 and R2, not R3: after the marker's erase survives alone, recovery stops on Core's "Found EvoDB inconsistency" before any state is compared. T7 and T27 fix the outer crash right after B1 returned and explore every inner crash |
| I25 | §5.1 R13 (DEC-101 Q4) | An effect is required once a completed chainstate barrier follows it, in its epoch or a later one with only ProcessKills between; a PowerLoss in between may have lost it |
| I26 | §2.2, §6.5 W3 | Breaking A-REN also lets a rename's destination end **unbound** (unlink-then-link cut between the two), not only each name keep its old or new binding. Without that, ST2's private copy of a hardlinked file is safe under W3, because the destination's old binding always remains |
| I27 | §2.2 `Extended` | `Extended` keeps the granted assumptions. Its garbage version garbles only the bytes the pending writes change, and the new **unreadable** outcome (a read fails with EIO: a torn sector failing its ECC) applies only to units with nothing durable in them; either one over durable bytes would break A-PSOW, which only W5 breaks. Its page-aligned sizes stop at the last written unit, because larger sizes read alike; the allocated size stays an option. Deferred ENOSPC/EIO at `sync_data` is a nightly sweep of `Applied` faults on every `sync_data`, both kinds. Reads after a failed sync return the new bytes: the forward run makes no decisions, and a system that reads on after a failed sync already violates R8 (P5, T9) |
| I28 | §5.1, §5.3, §5.4 (CP2.1, reviews/ST3a-cp2-sol.md) | R5 compares every file record of E, and the immutable index fields: height, parent, file, data position, and undo position once set. Only growth of files the epoch wrote, undo and validity added by the epoch's applied connects, the reference's failed marks, and I11's prune resets are entitled (CP2-1). A D1 failure that came through an undecided read is re-derived, never emitted on the owner's presence alone (CP2-2). Store-level checks run as the checker: their reads cannot take or count toward an injected fault, and a failed checker read is a Model violation (CP2-3). R6 (a) and (c) compare each connect's script-check decision with the reference's (`ToyConfig::assumevalid`). R6 (b) requires the replay to equal the booted marker's path. R6 (c) requires an applied recovery-epoch mark for every activation connect (CP2-5). The boot D1 cache is keyed by the adversary, granularity and allocation mode, and a boot with D1 off neither reads nor fills it (CP2-6). R7a, CP2-4, is I24. The synced-value guard (§5.4) is asserted at the crash, where a synced version could be lost, rather than at each barrier, where by construction it becomes the only version |
| I29 | §4.6 | The seeded sweep (`sweep()`) generates a configuration and 3–9 ops per case (`random_toy`). Its first runs found three issues. Replay sliced a short block read; Core stops with "Unable to replay blocks". R6 (e) took an equal-height branch tie for a wrong tip; Core keeps its tip on equal work, so R6 (e) compares heights. FA4 required a capped commit before any flush had made `T_ack` |
| I30 | §4.2, §8.4 | Depth 2 is `Nested::Depth2Sample(per mille)`: a seeded sample of inner crashes gets a second crash after every event of its recovery, one branch each, with a `nested2` repro line. The nightly (`power_loss_deep.rs`, release) composes: the clean matrix (every scenario, both releases, `Realistic` with R12 on every branch, and `Extended`); nine production-scale prune scenarios (K = 288, buffer 10, `D_max` 1,024, 3 KiB blocks, file 0 = heights 0–20, so prunes at 309); nested depth 1 on S1–S4, S4b, S7 and S11 at both releases, `Realistic` and (since CP3.3, I39) `Extended`; depth 2 on a 0.3% sample; the 2 × 10⁵-case sweep; witness searches under `Extended` and byte granularity, with 3 or 8 KiB records except for prune scenarios and S6, whose file layouts their bugs depend on; S11d nested at `D_max` 1,024; full minimisation; and M10 at scale |
| I31 | §5.5 step 6, §5.3 D1(a) | Found by the 2,000-case sweep. (1) Recovery's activation is Core's `ActivateBestChain`: it reorgs to the most-work candidate whose whole path has data and no failed block. The toy used to extend its tip greedily along the first branch, which stopped below a longer branch after an index-only write. (2) D1(a) judges a poisoned unit the way it judges a pending one, by the bytes in the range: a failed write sharing a sector with synced bytes (A-PSOW) leaves them durable. (3) The sweep's generator reorgs only onto more work, as Core does. A reorg onto less work is `invalidateblock`'s, which marks the old branch failed |
| I32 | §8.4 | The sweep costs about 29 ms per case in release, not 3 ms, so the nightly runs 2 × 10⁴ cases (`ST3A_SWEEP_CASES` overrides), within §8.4's 600 s sweep allowance |
| I33 | §5.5, §4.2, §2.2 (the internal Phase D review) | (1) "Stabilised" now follows §5.5. At a crash, a cell built from state already pending before the crashed epoch began (a process kill kept it, and no barrier of the outer recovery has covered it yet) is marked `^`. A nested branch that decides such a cell, or any outer-crash cell, is not stabilised: R7a skips it and R7b's checks still run. Explore, replay and the sweep share this predicate, and the sweep's nested cases now evaluate R7a. (2) Depth 2 boots its third crash with a seeded branch, its decisions recorded in the repro; minimisation tries dropping `nested2`, then drops it together with `nested`. (3) `Extended` judges "nothing durable" by the durable size, not the content, so synced zero bytes stay A-PSOW-protected. A partial write into an unreadable unit fails with EIO, as Linux reads the page first. (4) R6 (c) covers every block of the final chain off the recovered tip's path, so a recovery reorg's connects count. Recovery's disconnects read undo, as Core's DisconnectBlock does. (5) `pick_fault` offers only `Applied` on `sync_dir` (§4.4) |
| I34 | §5.5 step 6, §6.2.1 FA4 | Found by the first 2 × 10⁴-case nightly sweep. (1) Recovery kept only the tip's evo state and filled the heights below it with zeros. Since activation is most-work (I31), its disconnects reach that range, and a later flush wrote a wrong evo state (R12/R3; `c6;f;r4:4;i`, a crash after the index write). Recovery now fills those heights with the path's fold, as EvoDB's undo restores them. (2) FA4's liveness half counted progress from the last cadence commit. No capped commit can land before `T_ack`, nor while `LCA(progress, T_ack)` is below the locator, so such marks restart the count. I29 had fixed only the turning-synced half. Both have PR-gate regressions |
| I35 | §2.2 `Extended`, §5.3 D1, I27, I33 (3) (Sol's CP3 review: CP3-1, CP3-4) | **Ruling:** partial-write and EIO granularity follow native Linux I/O boundaries; the adversary's units only choose where tears fall, not how much a call reads or writes. Native controls (Linux 7.0.0-38; ext4 and XFS with 4 KiB blocks; the last sector of a file's block made unreadable through a private loop and dm-error mapping, only that file's cache evicted) give the same table on both, and in Sol's independent run: `pread` of any sector of the block, or of the whole block, fails with EIO; `pwrite(0, 1)`, `pwrite(0, 512)` and `pwrite(3584, 512)` fail with EIO, since the kernel reads the rest of the block first; `pwrite(0, 4096)` and `pwrite(0, 8192)` succeed: they read nothing first. tmpfs has no ECC path and stays not modelled (§2.3). So under byte and sector granularity alike: a read fails if any 4 KiB block it touches, below the size, holds an unreadable unit; a write reads, and can fail on, only the blocks it covers partly; a write over whole blocks replaces them. A-PSOW stays granted (I27), wording unchanged: no pending write changes bytes outside its range. DEC-148 (2) (Sol's CP3.1 review, ext4 and XFS with a 512-byte fsynced prefix and an unsynced 3,584-byte tail whose last sector fails) adds the native outcome that A-PSOW does not exclude: a block holding durable bytes and a torn unsynced write may read back as EIO. The durable bytes stay unchanged but unavailable; a read that succeeds returns them (an `O_DIRECT` read of the prefix alone succeeds natively; the model's reads are buffered). So any block with a torn pending write may be unreadable, durable bytes in it or not. Readability is one cell per such block (`{epoch}:fs:i{ino}:eio{block}`, choice 1 = EIO), shared by the block's pending units; each unit's content cell is decided only if the block is readable. Per-unit unreadable outcomes made a read decide every unit cell in the block, so under byte granularity one torn block multiplied the search by up to 4,096 cells and stalled the nightly's byte witnesses; a 1-byte read of one torn page now has 33 branches (`cp3_4_one_eio_cell_per_block`). D1 (CP3-1, DEC-148): a referenced range's bytes in an unreadable block stay durable if every crash outcome leaves them alike (durable, unavailable: an engine reading them must fail-stop; a read's EIO is traced as `ReadFailed`, so R8 flags a recovery or connect that carries on, `cp31_2_recovery_must_fail_stop_on_unavailable_bytes`); bytes the torn write could have changed make the range NotDurable, or Unknown while the block's cell is undecided. The garbage outcome is built from the durable version's bytes, through an earlier crash's EIO wrapper (its decided content, else the bytes every outcome agrees on; a block an earlier crash left unreadable and the recovery then rewrote whole gets none while that content is undecided, which loses coverage but reports nothing false), and D1's content comparison falls back to those bytes for a unit an EIO left undecided (the CP3.2 internal review: the wrapper was read as zeros, so a second power loss turned untouched durable bytes into garbage). Zeroing part of an unreadable block reads it first, as ext4 does; a unit wholly past the old end is zeroed without reading its block: a truncate inside it, an extension from a size inside it, and the zero-fill fallback fail with EIO rather than zero its durable bytes. The toy chain fail-stops on such reads (its `accept_next` used to take an unreadable parent for branch 0, and its re-learn path reported EIO as "Unable to replay"); its R1 read-back check accepts EIO, D1 judging the bytes' durability. This replaces I33 (3)'s second sentence |
| I36 | §5.5, §4.2, R7a, I33 (1) (CP3-3) | I33's reading of §5.5 is refuted: the global skip of every nested branch that decides a `^` cell (or an outer-crash cell) is removed. Each `^` cell now records, per choice, what it leaves of the input it inherited (pending since before the outer recovery began): intact (the outer image's version, perhaps overwritten since by the recovery), a choice of the same cell at a crash before the recovery's first event (batch and directory-entry survival, a block's readability; temporal prefixes, past the recovery's own operations), or an older or torn version by content. R7a then compares each nested branch with a reference, judged on every cell the inner recovery and its projection decided. While every inherited input is intact, and every outer-image cell the uninterrupted run read reads the same, the reference is the uninterrupted projection. Otherwise it is an uninterrupted recovery from the same inputs: the outer image's cells as the branch decided them; if inherited inputs changed, the outer image crashed as the inner kind before its recovery's first event, its `^` cells chosen to match. Outer-crash cells no longer exempt a branch. The only exemption is cell-scoped: a branch whose interrupted recovery read, before the inner crash, an inherited input that the cell's choice changes (per choice: a prefix cell counts only the batches its cut loses). Reads count through lookups, link counts, reads, sizes and copies, by provenance (DEC-148 (1), CP3.1-1, superseding "any inherited version"): a read records a unit only if a byte it returns holds what an inherited pending write supplied, meaning a byte some pre-epoch pending version changed from the durable one (any byte under `break_psow`, whose torn write zeroes the unit), not replaced since by a write, zero-fill or truncate of the epoch. A complete overwrite drops the dependency; a partial one keeps it for the bytes it preserves (the Deriver and partial-overwrite controls); older versions stay crash outcomes but exempt nothing by themselves (Sol's Overwriter). Each `^` choice is then judged against what was read (the CP3.2 internal review): a unit choice counts only if it changes a byte the read saw (so an older version equal there, or `break_psow`'s zeros over zero bytes, does not); a size choice only if it changes what a read returned. The recovery's size is the inherited one extended by its writes and fallocates to at least a floor `e` (`max(size, e)`; a fallocate raises the floor even when it changes nothing under the inherited size) until it sets the size outright (a truncate, even to the same value). A read that returned its bytes depends on the inherited size up to the last byte the recovery did not write that a smaller size would change: one at or past the floor (cut), or one not zero (the recovery's own extensions would zero it; bytes zero under every size, such as absent units or a copy's, do not count). Bytes the recovery's own extension zero-filled from the inherited size are judged with their unit: a size past such a byte changes the read only where the unit there is not zero. Where that, or a size choice's effect on a read's end, depends on cells still undecided when the crash judges the choice (the unit's cell of the same crash under Sector granularity, or an earlier crash's cell inside the unit's versions or the size's options), it is judged on the branch's final decisions, once its recovery and projection have run. A cell of the same crash the branch never decided stands at choice 0, intact, as the reference takes it; a byte still unknown then (an earlier crash's cell) counts, as does an unreadable block. An intact unit choice stands for the version the read saw (the reference's), not one the recovery wrote later. A truncate takes over the bytes past its end even in a unit already zero, so a later regrow does not inherit them; a copy reads zero-filled bytes as a read does. Bytes the recovery zero-filled itself have its own provenance: a crash that keeps the recovery's size but loses the zero-fill, exposing the durable bytes under it, exempts nothing (no uninterrupted recovery reaches that state, so R7a reports a recovery whose result depends on such bytes; decided in the fifth review round under DEC-148 (1)). A read that hit the end of the file, and a read of the size (`len`, a copy), depend on it only through the floor (the second to eighth review rounds; `power_loss_cp32_reference.rs` judges each choice against the real reference); a Write-granularity cut only if it changes such a byte or size; a rename's per-name cell only for the name looked up. A value not known without deciding counts. A block's readability is its own input (`eio` key), read by a read that returned, from the block, bytes an inherited write supplied; a read of only durable or own bytes in the block, a write or a truncate does not count (the reference fails the same way, and R8 requires the fail-stop). A read that fails with EIO depends on the size only up to the end of its first unreadable block (a size past it leaves that block failing), or up to its own end while the floor lies below it (a smaller size ends the file first). A lookup records only the pending operations from the newest one whose change stands (undecided ones count, conservatively); the ones it hides are not read. That is §5.5's permitted pre-promotion divergence; after a promotion barrier no engine batch is inherited. A changed input that no crash before the recovery leaves alike is also exempt, and counted apart; under `Extended`, an inherited unit the recovery also wrote, torn to garbage, is one. Each report counts compared, constrained and exempt branches. Sol's engine and filesystem finalizer probes now report R7a. Replay, explore and the sweep share the reference. **Superseded by DEC-162 (I40):** every exemption above is removed, with the machinery behind it (the per-read provenance, the `observed` judgments, the size floors, the deferred judgments, and the compared, constrained and exempt counts). The `^` cells' starts remain, and only select the references R7a runs |
| I37 | §8.4 (CP3-5, CP3-6) | The nightly runs through `contrib/node/st3a-nightly.sh`. It builds the release test binaries and runs the executables Cargo reports for that build, one component at a time, continuing past failures. It exits non-zero if the build fails, any component fails or runs no test (a stale name under `--exact`), or a required artifact (`head.txt`, a component's log or time file, `summary.txt`) cannot be written (CP3.1-3); PR-gate tests plant each case, the last with a directory in `summary.txt`'s place. A component's old time file is removed before it runs, so a reused `OUT_DIR` cannot report stale times. The sweep reads `ST3A_SWEEP_SEED` (default `0x5733a5ee`) and prints it. The runner rotates it by the UTC date (`YYYYMMDD`) and records it first in `summary.txt`, so setting it replays a night. PR smoke seeds stay fixed; the 2 × 10⁴-case count is unchanged |
| I38 | §6.2 R5 (c) (CP3-2) | The implementation now follows R5 (c) exactly. A prune's resets are entitled once that flush's own finalizer (Bk or Bp) has returned from its Durable write; the toy logs this right after the write, and any later chainstate barrier no longer entitles them. A reset covers only the `HAVE_DATA` and `HAVE_UNDO` bits and the data and undo positions. A block's height, parent and file are checked whether or not its file was pruned |
| I39 | §5.5 R7a, `Extended` (CP3.2; closed in CP3.3 under DEC-161 and DEC-162 (4)) | Under `Extended` with nested crashes, an inner power loss can tear the recovery's own unsynced write so that the next recovery reads an unreadable block. R7a counts such a recovery as a **sanctioned fail-stop**: allowed, and reported apart (`R7aCounts::sanctioned_fail_stop`). It qualifies when: (1) it aborts with a message sanctioned for its call path. Core's "Failed to read block" (`validation.cpp:3412`) covers ConnectTip; the startup replay's "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." (`node/chainstate.cpp:252`) is sanctioned only after the epoch's first fatal error was a failed block-file read inside the replay, and it is the only message sanctioned there. `Sut::sanctioned_after` receives that error's `FatalCause` (a read, its store, and the Core call paths it failed inside, `Mark::PathBegin`…`PathEnd`); the default is `Sut::sanctioned`. I41 extends this to every call path (DEC-165 (3)) and corrects the line numbers. The same message after missing data, which no failed file read precedes, stays an R0 refusal. The engine's sanctioned ABORT also qualifies. (2) The **stop** failed on a block the boot's own crash tore. The stop is the operation that failed on an unreadable block at the epoch's first fatal error, or the latest such operation before it with nothing succeeding in between (an engine reporting the failure as its own error). Torn means the failing unit still holds the version the boot's crash left, wrapped in one of that crash's EIO cells (I35), whichever cell inside makes it unreadable: an inner power loss that keeps the recovery's extended size and loses its zero-fill exposes an earlier crash's unreadable block, and that is still the recovery's own write torn. A block a later write or copy replaced does not count. (3) Every earlier failure on an unreadable block in the epoch was on a torn block too. (4) R8 holds; it already reports anything that succeeds after the fatal error. These remain R7a failures ("did not complete", unless a reference reproduces the non-completion under the witness rule, I40): any other non-completion; an unsanctioned message (R0 and R8 report it too); a sanctioned abort with no unreadable block behind it; a write after the failed read; a block only an earlier crash tore, which only the interrupted recovery reads; a fatal read of such a block after an earlier, non-fatal failure on a torn one (the CP3.3 validator's P2); and a read failing on outer-crash EIO that a copy carried over a torn block (P3). A successful write between an earlier non-fatal failure and the fatal read does not disqualify the stop (P1). D1 and the checks still judge every successful read, so no read may return non-durable or wrong bytes. `power_loss_cp33.rs` holds the positive control, these negatives and P1–P3. P2 and P3 are pinned replays of the validator's branches, which 447834b8 masked; `power_loss_cp33_witness.rs` holds the toy's replay message. The nightly gains `nested_depth1_extended`: nested depth 1 under `Extended` on S1–S4, S4b, S7 and S11 at both releases |
| I40 | §5.5 R7a, I36, DEC-162 (CP3.2-1, CP3.2-2, CP3.2-3) | **The witness rule replaces R7a's provenance exemptions.** R7a compares **outcomes**: a recovery completed with its projection, or aborted with its message. A nested branch converges when it ends as the uninterrupted recovery did, decided every outer cell they share alike, and decided every outer cell the uninterrupted projection alone decided. Explore runs one nested search per set of cells the outer recovery decided, so such a cell's other values are judged nowhere else; the witness search enumerates it (the CP3.3 validator's second round). Otherwise it passes only if a concrete reference, run **uninterrupted under the same complete input assignment**, ends exactly as it did. A reference that panicked, broke R8 or aborted with a message not sanctioned for its cause witnesses nothing, and a branch that panicked fails R7a. The assignment fixes the outer image's cells as the branch decided them, else as the uninterrupted recovery decided them before its projection ran (explore, the sweep and replay all fix this set, so explore's findings replay; replay's single decision map can still move a projection-only cell an inner branch decided, which moves branches between converged and witnessed but not the verdict), and the inherited inputs as the inner crash left them. The cells the branch left undecided, including inherited inputs its inner crash never decided, are enumerated jointly: one value each, shared by every reference (an undecided input by its start, which each reference's crash maps to its own choice). The branch must be reproduced under **every** completion. The references are: (a′) the uninterrupted recovery, then a crash of the inner kind whose `^` cells leave the inherited inputs still pending as the branch did (the rest intact), then recovery; with every inherited input intact, the uninterrupted recovery itself; (b) I36's reference: the outer image crashed as the inner kind before its recovery's first event, its `^` cells matching every input the branch changed, then recovery. Provenance only selects references: with no inherited input changed, only (a′); (b) is tried first and (a′) only if (b) does not reproduce the branch. It never grants a pass. A branch with more completions than `Scenario::r7a_assignments` (default 64) is **unjudged**: counted, never passed. So is a branch whose references decide a cell out of order, under another domain, or stop short of a cell an earlier completion reached; the enumeration is poisoned rather than clamped. (a′)'s first leg must itself be a valid reference. With no inherited input changed, the uninterrupted run is the only reference and the inner crash's undecided inputs are not enumerated: such an input changed matters only to a recovery after the uninterrupted one, which is itself a nested branch of the search. `Report::is_clean` requires zero, so the PR gate does, and the nightly reports the count. A recovery that fail-stopped on a torn block (I39) is judged before any reference runs. The report counts converged, witnessed (a′), witnessed (b), sanctioned fail-stop and unjudged branches. Replay, explore and the sweep share the judge, memoised per outer branch. The provenance machinery is deleted rather than kept in parallel: per-read provenance, `observed` flags, deferred judgments, and the inodes' fresh, zero-filled and floor state. With no floors left, DEC-162 (3) (record the floor only after the allocation succeeds) has nothing to apply to. A failed advisory allocation records nothing, and Sol's SizeDeriver converges. The regressions are in `power_loss_cp33_witness.rs`, and each replay fails at dcca3731 and at 447834b8. Sol's CopyFinalizer (a copied earlier cell, CP3.2-1) and FloorFinalizer (an undecided earlier size under a Write-granularity cut, CP3.2-2) report R7a, also when the branch resolves the earlier cell. Their controls hold: no copy, R7a; the fixed finalizers, clean. SizeDeriver converges, with or without a failing allocation (CP3.2-3). The toy's replay message and its per-path table are checked there too. The CP3.3 validator's outcome hole has its own regressions, which fail at dcca3731 and at 1cff44ad6. In the Resumer, an interrupted recovery's abort is no longer witnessed by a reference that aborted with another message, or by one that panicked. Its control, the same abort, is witnessed or converges. `cp33_projection_only_cell_does_not_converge` reproduces the round-2 hole with the validator's probe: a resumed recovery revives a write that only the projection decides was lost. It fails at dcca3731, 1cff44ad6 and 8ede1e28d, and explore's finding must replay. `Joint`'s poisoning has unit tests. Two tests are coverage checks rather than regressions. The finalizer searches find the bug at dcca3731 too, through other branches; they also require the fixed finalizers clean and nothing unjudged. The bound test needs the new `r7a_assignments`: a bound of 1 leaves a witnessed branch of a two-file reader unjudged, and the report is then not clean. The positive controls pass under the witness rule: Deriver plain (witnessed by (b)) and sync-first, and the partial overwrite. The observed-flag oracles (`power_loss_cp32_reference.rs`, the provenance cases of `power_loss_cp32.rs`, and `power_loss_cp31.rs`'s observation tests) went with the mechanism |
| I41 | §5.5 R7a, §4.2, §4.4, I39, I40, DEC-165 (CP3.3 r3) | **(1) Recovered after a fail-stop.** When the uninterrupted recovery ends in a sanctioned fail-stop and a nested branch instead completes, R7a allows the branch, counted apart (`R7aCounts::recovered_after_failstop`), if and only if every other check passed on it: D1, R1–R8 and the projection checks. If any failed, R7a fails the branch too ("completed after the uninterrupted recovery's fail-stop and failed a check"). Core behaves the same way: a later start may succeed after a fail-stop. It is judged after convergence and the torn fail-stop (I39), before any reference runs, so it infers nothing about the recovery's own cells (option (A), rejected). A sanctioned fail-stop is an abort after the epoch's first fatal error, with a message sanctioned for that error's cause, R8 holding (`sanctioned_stop`). An abort with no fatal error before it is a refusal, even with a sanctioned message, and gets no exemption (the validator's M-2). Explore, the sweep and replay pass the branch's check result and the outer recovery's verdict. `Restarter` in `power_loss_cp33_witness.rs` pins it under `Extended`. A fresh recovery writes a marker durably, then fail-stops on an unreadable record. A later start rebuilds the record and completes. No reference reproduces that completion. "ok" is clean, with 14 branches recovered after a fail-stop. "bad"'s completed state fails its check: it reports R1 and R7a, and its finding replays. "voluntary" stops with the same message on a lost (readable) record: no fatal error, so its completing branches report R7a "diverged". A run that ends without aborting must leave every Core path it entered (a Model violation otherwise). **(2) Sampling past the cap.** With `Scenario::over_cap` set, a nested inner image whose branches pass the cap is explored up to it, then sampled. Up to `samples` more branches run, each from a seed derived from the recorded seed, the outer image, the inner event and the kind. Every cell the branch decides is drawn uniformly from its domain: the walk is uniform per cell, since leaf counts are unknown without enumeration. A draw that repeats an explored or sampled branch, judged by its full decision set, is not reported, and draws stop at 8× `samples`. Such images count in `Report::sampled`, their branches in `sampled_branches`. DEC-185 (1) supersedes DEC-165 (2) here (I43): a sampled image still counts in `truncated`, and `is_clean` fails on either count, so a sample is a smoke that can find a finding and never passes one. No component samples: `nested_depth1_extended` is enumerated in shards (I43), and every other component stays exhaustive under its cap. The seed also mixes in the outer crash kind and the cells the outer recovery decided, so each outer branch samples its own stream. `cp33_nested_images_past_the_cap_are_truncated_even_sampled` uses a cap that truncates Restarter's inner images and none of its outer ones. The four truncated inner images are sampled too; the sample reaches the full search's 222 branches with no repeat and reruns identically from its seed, and the report stays truncated and not clean. `cp33_sampled_branches_are_judged_like_explored_ones` checks "bad" the same way: the same branches, R7a counts and findings as the full search, a failed report, and findings that replay. **(3) Messages by call path.** `Mark::ReplayBegin`/`ReplayEnd` become labelled Core call-path marks, `Mark::PathBegin { path }`/`PathEnd`. `FatalCause` carries the failing store and the path stack. A failed read inside any marked path is fatal, as inside recovery. The toy marks `LoadChainstate` (recovery steps 0–5), `ReplayBlocks`, `DisconnectTip`, `ConnectTip` (activation, and `accept_next`'s parent read, a toy-only read), and the index's `BaseIndex::ThreadSync` (catch-up) with `CoinStatsIndex::CustomAppend` (a connect's undo read) and `BaseIndex::Rewind` inside it. It picks Core's v24.0.0-rc.3 message by path, and `ToyChain::sanctioned_after` sanctions only that message on that path; the table below lists both. The innermost path decides. The exception is a chainstate engine error anywhere inside `LoadChainstate`, the replay included: it throws out of the load. "Failed to write to coin database" is no longer produced or sanctioned inside `LoadChainstate`, the replay included. "Failed to read block" leaves the path-free list. The K_SNAP read in `connect_next` now reports a read ("Error reading from database"), not a flush. Missing data is unchanged: an abort without a failed read stays an R0 refusal. `first_recovery_reads_take_the_fault` now expects "Error opening block database" for recovery's first engine read. `recovery_read_faults_use_each_paths_message` boots every image of S4, S11, S11r and an activation scenario (index-only writes above the durable tip, then a heavier branch) with a read fault on each of the first six filesystem reads. Each abort must carry Core's message for the innermost path at the failed read, and the six fs-read rows must all be reached |
| I42 | §5.5 R7a, I39, I41, DEC-165 (1) (CP3.3 r3) | **A fail-stop after a fail-stop.** I41 (1) allows a nested branch that completes where the uninterrupted recovery fail-stopped sanctioned. A branch that instead fail-stops again, sanctioned, with R8 holding (`sanctioned_stop`, on the system's own abort mark, I43), but with an abort no reference reproduces, is allowed the same way and counted apart (`R7aCounts::failstop_after_failstop`, "fail-stop after fail-stop" in the summary), if and only if every other check passed on it (store-level checks, R0, R8, D1). If any failed, R7a fails it ("fail-stopped after the uninterrupted recovery's fail-stop and failed a check"). It applies only after the witness search misses, so a branch a reference reproduces stays counted as witnessed. Core behaves the same way: a later start may stop again on another fatal error. The `nested_depth1_extended` nightly at e5bd182bf found it in S1 under v23.1 (inner events 3 and 4). The uninterrupted recovery fail-stops on an unreadable block ("Failed to write block"). The inner power loss drops the block file's allocation, so the nested start writes the block and then fail-stops on an unreadable undo unit ("Failed to write undo data"). No reference reaches the undo write, so the witness rule reported "did not complete". `cp33_s1_fail_stop_after_a_fail_stop_replays_clean` replays both repros clean. `Restarter`'s "again" mode (a later start fail-stops on the unreadable record with its own message) is clean with 14 such branches; "again-bad" adds a store check that rejects every later start and reports R7a, and its finding replays. Disabling the rule fails all three tests. Recorded under "decide, record, continue"; the manager may overrule it. |
| I43 | §5.5 R8, §4.2, §8.4, I39, I41 (2), I42, DEC-185 (CP3.3 r4) | **(1) The system's own abort.** 500dabd09 had the harness mark an abort when `open` returned an error the system had not marked; that supplied exactly the mark whose absence R8 must detect, and it is reverted. `fail_stop_scan` takes how the epoch ended, `End::{Open, Returned, Refused}`, from the outcome: the forward run's op or open error, a recovery that returned an error, `torn_fail_stop` and `sanctioned_stop` are `Refused`; a recovery that completed is `Returned`; a panic is `Open`. A returned error closes the epoch's open Core paths for the path-balance check (Model) only. R8 ("hit a fatal error and did not abort"), the torn fail-stop and the sanctioned stop read the system's own `Mark::Abort` in the trace, and nothing else; `open_recorded`, `reference_outcome` and `reopen_and_check` all take `End` from the outcome the same way. The toy's refusals (T4, T5) report R0 with no Model and no mark (`refusals_close_their_paths`). `Marker` in `power_loss_cp33.rs` pins it: a marked abort is clean; a wrong message reports R0 and R8; a write after the fatal read reports R8; an unmarked fatal return reports R8 and no Model (`cp33_unmarked_fatal_return_fails_r8`). **(2) Truncation is never clean.** DEC-185 (1) supersedes DEC-165 (2): a capped image counts in `Report::truncated` whether or not it was then sampled, and `is_clean` fails on `truncated` and on `sampled`. The sampler stays as a smoke that can find a finding and never passes one; no component uses it. **(3) Sharded exhaustive enumeration.** `nested_depth1_extended` (S1–S4, S4b, S7 and S11 at both releases, `Nested::Depth1`, `Extended`) is enumerated uncapped in shards. `shards(sc, depth)` runs each image and kind's outer search to `depth` decisions (`Dfs::limited`) and returns one `Shard { image, kind, prefix }` per distinct prefix, named `e{image}-{pk|pl}-{prefix}`; `explore_shard` runs that image and kind with the prefix pinned (`Dfs::pinned`: the search never advances a pinned position) and no cap, with every nested branch under it. Fault passes are not sharded. R12 runs only where the unsharded search runs it (§5.1): on the image's all-survived branch, which is the first branch of the shard whose prefix is all zeros. The first run at dfd65fe40 also ran it on every shard's first branch, and under `Extended` a prefix that leaves a block unreadable made the continuation fail-stop, a false R12 (S1 v23.1 image 53 onward); it was stopped, fixed, and every shard rerun at the new head (`shards_match_the_search_with_r12`, S1 v23.1 events 52–55, fails without the fix). **Nested ownership (CP3.4-1).** A nested search is keyed by the decisions recovery itself made, and the search runs one per key per image and kind, under the key's first branch in DFS order. Checker and projection cells come after those decisions, so they can put one recovery's branches in several shards, and at 86925887d each such shard ran the nested search again: S1 event 53 summed to 247 branches over its shards against 91 unsharded, and `Proj` "fixed" to 105 against 69 (Sol, CP3.4). That campaign covered every branch but repeated some, so its 191.9M was a count of runs, not of the search. A shard now first replays the outer branches of its image and kind that precede its prefix, as `search` runs them (R12 on the first) but without nested searches and reporting nothing, and starts with their keys taken (`owned_before`). Each nested search then runs once, in the shard holding its key's first branch, under that branch's outcome and projection, as unsharded. Every outer branch, checker and projection alternatives included, still runs in its shard. The replay costs outer branches only. The shards are the search, exactly: `tests/power_loss/shard_check.rs` compares a scenario's shards at each depth with its uncapped search on: outer branches per image and kind; nested branches and their R7a counters per outer image and kind (`Report::per_image_nested`, depth 2 included; CP3.5-1); total branches; every R7a counter; and the distinct findings. The per-bucket comparison catches equal and opposite nested miscounts in two buckets, which the totals miss. `cp35_shard_check_catches_a_nested_miscount_between_buckets` plants one between `Proj` "fixed"'s buckets and must fail there. A mutation that files a shard's nested totals under the other crash kind fails the S1, `Proj` and Restarter regressions at that check only. Each shard must also file its nested branches under its own image and kind only (`assert_buckets_owned`, CP3.6-1). That pins the full search's buckets too: Sol's mutant files both searches under image + 1, and it now fails the S1, `Proj`, Restarter and planted-miscount tests there. The bucketing is accounting: it changes no verdict, `is_clean` or ledger row, so the campaign at 8833a8d2f stands (each shard is one image and kind already). It runs S1 event 53 under `Extended` with nested depth 1 at both releases (91 branches at depths 0, 1, 2, 4 and 16; `shards_own_each_nested_search_once`), `Proj` "fixed" (69) and "bug" (54, with its findings) at depths 0–4 and 16 (`cp34_projection_cells_do_not_repeat_nested_searches`), Restarter "ok" and "bad" (222; `cp33_shards_partition_the_search`), and the R12 window above. The first two fail without the ownership replay; Restarter, whose recovery reads its record itself, does not split a recovery and passes either way. **Resume (CP3.4-2).** The runner's resume skips a shard only if its last row at the head is ok, the rule the verdict uses; an older ok no longer skips a shard whose latest run failed (`cp33_shard_runner_ledger`). The depth is 4 (`SHARD_DEPTH`), chosen on S1 V23.1 at load 70–150: at depth 1 one power-loss shard of image 18 took 791 CPU-s for 1.23M branches and image 36's ran past 13 CPU-min; at depth 6 image 36 split into 76 shards, most about 18 CPU-s, but single outer branches over 5 CPU-min form a tail no depth splits. Depth 4 gives 9,440 shards, near that floor. `contrib/node/st3a-shards.sh` builds at HEAD, plans (`ST3A_SHARD_PLAN`), and runs each shard (`ST3A_SHARD=<component>:<shard>`) as its own systemd user unit with `Nice=19`, `IOSchedulingClass=idle` and `CPUWeight=10`. It appends a row per shard to `ledger.tsv` (head, shard, status, times, result). A rerun at the same head resumes past the shards already ok there. The component is clean only when every planned shard has an ok row at the head. No shard carries over from another head: every head reruns all of them (simple over clever, as DEC-185 allows). `st3a-nightly.sh` takes the runner's verdict for the component and no longer has `ST3A_NESTED_SEED`; `cp33_shard_runner_ledger` tests the runner and that hand-off with a stub binary. **(4) Sol's `Skew`.** Fifteen short exits, then two dense subtrees of 2^12 leaves with R1 in the second: under a cap of 4096 the capped run, sampled or not, misses it and reports truncation, so it is not clean; the sharded enumeration finds the R1, its repro reproduces, and the bug-free tree is clean (`cp33_skew_needs_sharded_enumeration`). **(5) The campaign at 8833a8d2f.** All 9,440 shards ok: 0 findings, 0 truncated, 0 unjudged. 189,034,654 branches, of which nested: converged 170,674,168, sanctioned fail-stop 16,917,738, recovered after a fail-stop 1,412,172, fail-stop after a fail-stop 260, witnessed 0. Both releases give the same branches and counters in every component. CPU (user + sys of the ok rows) 131,479 s, 36.5 CPU-h: S1 66,855 s at v23.1 and 63,734 s at v24.0, S4b 291 and 279, S2 104 and 102, S4 44 and 40, S3 10 and 10, S11 4 and 3, S7 1 and 1. The median shard takes 0.08 CPU-s and the 99th percentile 229. The longest is S1 image 40's power-loss shard `0.0.1.0`: 8,022 CPU-s (12,101 s wall) at v23.1 and 7,465 (8,553 s wall) at v24.0. That one outer branch sets the floor on wall time at any width. At 8 shards, Nice 19, under box load 25–190, the run took about 9 h of wall time: 3.5 h, then the box's unclean reboot at 18:22 UTC, then 20,076 s more. The rerun resumed past the 1,941 ok rows. The ledger survived the reboot intact, and the 8 shards that were in flight had no row and reran. Against 86925887d's repeated runs, branches per release fall in S1 from 94,610,231 to 94,318,147, in S2 from 297,618 to 23,630, in S4b from 983,847 to 105,359 and in S7 from 293 to 245. Every fail-stop counter is unchanged, and so are S3, S4 and S11. Total CPU falls from 140,310 s to 131,479. |
| I44 | §4.2, §8.4, I43, DEC-82, CH2a, CH2b | **Landing on node/integration (7ecbc6cc1).** **(1) RP-V24 (DEC-82).** The toy is v24.0 only. `ToyConfig` has no release: its `release=` field is gone from the config text, the 43 toy repros and every test. `BlockFilesConfig::new` and `save_block` lose the release and tip height. No test runs v23.1. The components of the nightly and `nested_depth1_extended` drop the release from their keys (`S1`, not `S1-V24_0`). The depth-4 plan is 4,720 shards, exactly the v24.0 half of the 8833a8d2f plan with the IDs renamed. That half of the I43 (5) campaign is the certified v24.0 run: 94,517,327 branches and 64,170 CPU-s, S1 63,734. Its two releases had matched branch for branch in every component. `cp33_s1_fail_stop_after_a_fail_stop_replays_clean` keeps I42's S1 repro: the same decisions at v24.0 reach the same fail-stop after a fail-stop. It replays clean, and reports R7a "did not complete" with I42's rule disabled. The cherry-picked `OpenFailed` commit is dropped, since node/integration carries it, and so is its v23-era test adaptation. **(2) Range deletes (CH2a's `BatchOp`, `delete_range`).** `FaultyEngine` takes CH2b's `Layer` design, one implementation. A batch records range tombstones beside its last put or delete per key. `Layer::op(k)` is the batch's last word on k: a range delete covers the keys that no later put or delete of the batch names. `get`, `durable_get`, `durable_records`, `Scope::Keys` overlap and the cf set all go through it. The fold reuses `memory::apply`. A range delete passes no record to `RefExtractor::finalizes`. `delete_range_passes_the_engine_check` runs CH2a's direct check on `FaultyEngine`, and M6's `kv_model` runs now include more than 100 range deletes under subset power loss. `m6b_range_deletes_belong_to_their_batch` checks that a range is kept or lost with its batch, and that D1's view is `Unknown` over the range until the batch is decided, then agrees with reads. |
| I20 | §6.2 `finalizes` | A locator's scope is `SincePreviousLocator` only. The "cross-store chainstate barrier" is the requirement that B1 has returned. On the toy's single thread a Durable B1 returns atomically, and `DurableTipAtLeast` (D1(b)) checks the result (T10). A `Store { chainstate }` scope would wrongly count V's pending effects |

**Per-path abort messages (I41, DEC-165 (3); Dash Core v24.0.0-rc.3).** The innermost path decides, except that a chainstate engine error anywhere inside `LoadChainstate` takes that row. Anything else falls to the path-free list.

| Path | First fatal error | Core's message | Citation |
|---|---|---|---|
| `ReplayBlocks` | a block or undo read | "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." | `ReplayBlocks` returns false (`ReadBlockFromDisk` in rollback/roll-forward); `node/chainstate.cpp:252` |
| `LoadChainstate` (the replay included) | a chainstate engine read, write or WAL sync | "Error opening block database" | `dbwrapper_private::HandleError` throws `dbwrapper_error` (`dbwrapper.cpp:261`, `:268`); init's `catch_exceptions` reports it (`init.cpp:2033`–`2037`) |
| `DisconnectTip` | an undo read | "Failed to disconnect block; see debug.log for details" | `DisconnectBlock` fails (`validation.cpp:2300`); `DisconnectTip` returns false (`:3282`); `ActivateBestChainStep` aborts (`:3621`) |
| `ConnectTip` | a block read | "Failed to read block" | `AbortNode`, `validation.cpp:3412` |
| `ConnectTip` | a chainstate read | "Error reading from database, shutting down." | `CCoinsViewErrorCatcher`'s callback, `init.cpp:2089` |
| `BaseIndex::ThreadSync` | a connect's block read | "ThreadSync: Failed to read block <hash> from disk" | `index/base.cpp:182`–`183` |
| `CoinStatsIndex::CustomAppend` | a connect's undo read | "ThreadSync: Failed to write block <hash> to index database" | `CustomAppend` fails (`index/coinstatsindex.cpp:129`); `index/base.cpp:189`–`190` |
| `BaseIndex::Rewind` | a rewind's block or undo read | "ThreadSync: Failed to rewind index toyindex to a previous chain tip" | `CustomRewind` (`index/coinstatsindex.cpp:287`) or `ReverseBlock` (`:413`) fails; `index/base.cpp:172`–`173` |
| none (path-free) | a coins flush, coins read, flat-file flush, block or undo write | "Failed to write to coin database"; "Error reading from database, shutting down."; "Flushing block file to disk failed…"; "Failed to write block"; "Failed to write undo data" | `validation.cpp:3093`; `init.cpp:2089`; `node/blockstorage.cpp:600`, `:869`, `:784` |
| none (toy-only, predating I41) | a non-coins chainstate read (the EFFECTS snapshot), the index store's reads; a failed index commit | "Error reading from database, shutting down."; "Failed to commit latest toyindex state" | not Core's on that path. Core's coins catcher covers only coin reads, and Core opens index databases after `LoadChainstate`. `index/base.cpp:232` only logs a failed commit; the toy stops, which is stricter |

**Deviations from the PLAN text:** "as a WAL prefix" is superseded (DEC-62), and "integration into `coins_cache_model`" is read as ST3a shipping the seam.

---

## 13. Review dispositions

### 13.1 GPT r1 (REWORK): unchanged from rev1

All nine findings were accepted:
1. subset domain;
2. independent directory ops;
3. data history for truncation and zero-fill;
4. pre-prune versions;
5. provenance marks;
6. R7a/b;
7. exhaustive versus witness search;
8. fatal set;
9. fixed-point minimisation.

GPT r2 closed 1–3, 5, 6, 8 and 9; 4 and 7 were partial and are completed below.

### 13.2 Opus r1 (APPROVE-WITH-CHANGES): unchanged from rev1

All fourteen findings were accepted, with one correction to finding 1 (the barrier rule). Opus r2 closed 1, 2, 4 and 5; 3, 6 and 7 were partial and are completed below.

### 13.3 GPT r2 (REWORK)

| # | Finding | Disposition | Where |
|---|---|---|---|
| — | Q14 confirmed natively (finalizer at `Sync()` entry; multi-WAL, multi-writer barrier) | noted; carried into §6.4 cases 3–4 | §2.4.2, §6.4 |
| 1 | Prune scenarios unreachable (file 0 holds 1–63; the lock masks S5; S5c cannot reach T15) | **Accepted** (ruling 3): subscriber-free configuration; real boundaries; asserted preconditions; S5c redesigned (flush 1 at 60 with V at 70, flush 2 at 80); S11 lock variants asserted to differ; budget rows recomputed. With DEC-64 the lock no longer masks S5 even with subscribers (N9) | §6.2, §8 |
| 2 | T18 safe as written | **Accepted** (ruling 4): T18 = no barrier **and** Buffered Bk; N8 is the safe variant; W1 and counts updated | §6.2, §6.5 |
| 3 | R5 conflicts with a surviving in-flight Bk | **Accepted** (ruling 5): R5 against the WAL-order overlay; required clean image in N1; T31 | §5.1 |
| 4 | W1 versus unconditional D1(f) | **Accepted**: W1 expects no post-crash violation, and D1(f) still fires | §6.5 |
| 5 | Concurrent-barrier precondition | **Accepted**: normative returned-before-started rule | §2.4.1, §2.4.4, §5.6, E18 |
| r1-4, r1-7 partial | — | completed by findings 1 and 3 above | — |
| Opus r1 1, 3, 6 partial | — | completed (T18, R5; reachable prune and locks; budgets) | — |

### 13.4 Opus r2 (APPROVE-WITH-CHANGES)

| # | Finding | Disposition | Where |
|---|---|---|---|
| 1 | Durable-locator lock stalls pruning; `feature_index_prune` fails | **Accepted** (DEC-64): the recommended ordering, B1 → commits → selection → Bk; CH8 cross-check | §6.2, E12, E14, E16 |
| 2 | Reorgs, null locators, startup order, undo, toy regime, disabled subscriber | **Accepted**: (a) LCA (S11r, T30); (b) never null after the commit step, with start height for a fresh syncing subscriber (S3p); (c) §5.5 step 0 (S13, T28); (d) block and undo (R11b); (e) toy buffer 2; (f) R11b scope plus sanctioned refusal (S11c). The note that `llmq/`'s lock also protects wallets is recorded in E14's context | §5.1, §5.5, §6.2 |
| 3 | The lock masks the S5 family; S5c cannot reach T15 | **Accepted**: subscriber-free S5/S5b/S5c with real boundaries; N9 shows that under DEC-64 a caught-up subscriber selects the same files | §6.2 |
| 4 | R11c contradicts A1 §3.5.7 | **Accepted**: rewind, then catch-up; R11c checks both; data timing declared | §5.1, §5.5, §6.2 |
| 5 | Subscriber bits break the bounds; R12 scope; witness windows; W1 on T27; production `c_b` | **Accepted**: per-key scans; at most 2 data batches per flush; bounds recomputed (S1 = 96); R12 PR scope stated; windows narrowed; outer decisions fixed; production `c_b` 8 ms | §2.4.3, §4.3, §4.5, §8 |
| 6 | Barrier scope: presumed effects; D1(f) too broad | **Accepted**: scopes; presumed-effect barrier before B1 (T29); D1(f) narrowed | §2.4.4, §5.3 |
| 7 | T18 clean | **Accepted** (as GPT r2 2) | §6.2 |
| 8 | T23 conflates two bugs | **Accepted**: T23a (L2, no D1(a)), T23b (non-promotion) | §6.2 |
| 9 | D1 needs a world-level, standing view | **Accepted** | §5.3 |
| 10 | Coins profile lacks effects | **Accepted**: `effect_cfs` and `get` on `CoinsStateView` | §5.2 |
| 11 | `fJustCheck` connects | **Accepted**: `Connected { applied }` | §3.2, R6, §5.6 |
| 12 | A-PSOW covers the WAL | **Accepted** | §2.3 |
| 13 | One-batch flush with pruning | **Accepted**: Bp | §6.2, E12 |
| 14 | Nits (S12 = 1.68; 63 per file; undo trailer; L1 non-exhaustive; 24 h) | **All applied** | §8.2, §6.2, R1, §4.3, §2.4.4 |
| §1 | `manual_wal_flush` should be named in A-DUR | **Accepted** (ruling 6): contract, plus the ST1 assert (BL-34 note) | §2.3, E17 |

### 13.5 GPT r3 (REWORK)

| # | Finding | Disposition | Where |
|---|---|---|---|
| 1–5 | r2 findings 1–5 closed (reachability, T18/N8, R5 overlay, unconditional D1(f), concurrent-barrier coverage) | noted. The budget forecasts it reproduced (29.5, 2,085) are superseded by §8's rev3 rows | — |
| 6 | Permanent subscriber data requirements make correct pruning fail D1(b) in S3p and N9 | **Accepted** (ruling 1). Standing block and undo requirements come only from the durable locator L's path L → `LCA(L, G)` → G, with G the chainstate's guaranteed recovery tip; processed data records carry none. The toy extractor is specified; S3p and N9 must stay D1-clean through Bp/Bk and the unlink (N1, N9) | §5.3, §6.2 |
| 7 | W1 does not hide T17: an empty prefix after B1's early acknowledgement recovers `old` | **Accepted** (ruling 2). T17 is removed from W1's "no post-crash violation" list and is expected caught (R4, R3). No prefix-only control is added | §6.5 |

### 13.6 Opus r3 (APPROVE-WITH-CHANGES)

| # | Finding | Disposition | Where |
|---|---|---|---|
| 1 | DEC-64 inverts "select, then flush"; nothing decides a prune flush | **Accepted** (ruling 3), reading (c). A dry run under the predicted post-commit lock decides; the final selection is a subset; the log comes from the predicted lock; S14 is a triggered flush; T32 (the pre-commit lock); FA1, FA2. 367 reproduces (synced indexes predicted at `new` → 689), and a pruned IBD flushes only when a dry run finds files | §5.6 item 7, §6.2 step 0, E20 |
| 2 | Q15's "caught up" is an instantaneous test; no cadence or cap | **Accepted** (ruling 4). Classification as `m_synced`; synced subscribers drain through `new` at step 6; syncing ones commit on a cadence capped at `LCA(progress, T_ack)` (the cap, not the "rewind ahead of the recovered tip" alternative, because it keeps R11a and D1(b) unchanged); the toy commits from syncing mode; S11s, S11q; T33 (Core's uncapped `ThreadSync` commit) and T35 (the instantaneous test freezes the lock); FA3–FA5. **Beyond the finding:** the Durable rewind commit, Core's `Rewind`, which the bounded blind rewind needs, and T36 | §5.6 item 6, §6.2, E19 |
| 3 | A Buffered Bk is unsafe once the next B1 overwrites the marker; N8 holds only on one flush | **Accepted** (ruling 5). Bk, Bp and recovery's final batch are Durable; B1's scope includes `PreviousFinalizer`; N8 pinned to S4; T34 on S4b, caught by R3 and D1(f). **Beyond the finding:** recovery syncs each engine's WAL before its first write, so a Bk left pending by a ProcessKill is never presumed unbarriered by the next B1; this also resolves the finding's "until a nested window reaches R12's continuation flush" caveat for N8 | §2.4.4, §5.3, §5.5, §6.2, E21 |
| 4 | Incomplete FT list; no moveback counterpart | **Accepted** (ruling 7). All six assertions in CH8's row and E14; the moveback line emulated, log-only (S2 asserts it) | §3.6, §5.6 item 7, E14 |
| 5 | After B1 erases `meta.tip`, the locator check must read the marker's `new` | **Accepted**. G = the durable marker's `new`, else `meta.tip`, in R11a, D1 and the contract | §5.1, §5.3, §5.6 item 10 |
| 6 | S1's size term assumes preallocation; N7 has no bound | **Accepted**. Size term = the product over the files read, ≤ 3 with preallocation; S1's 96 is stated as about 10× conservative (reachable ≈ 8–16); N7 on a two-record window, bound 54 | §8.2, §6.2 |
| 7 | Bounded search needs blind replay and blind rewind | **Accepted** (ruling 6). Contract item 13; the toy rewinds `(L, L + D_max]` blindly; Q16 asks for a range delete | §4.3, §5.5, §5.6 |
| 8a | T31's "Also R6(e)" depends on the index given to `best_valid` | **Accepted**: `c` = E's index | §5.1 |
| 8b | N8 and N9 not in §8.2 | **Accepted**: rows added | §8.2 |
| 8c | S11r should note file 0's common blocks | **Accepted** | §6.2 |
| 8d | E14's "equal" should be "never lower" | **Accepted, narrowed**: never lower than Core's **with every subscriber synced**, equal when the lag is under `K − BUFFER − 1`; stricter while one syncs | §0, §7.3, E14 |

### 13.7 GPT r4 (REWORK), the final closure check

| # | Finding | Disposition | Where |
|---|---|---|---|
| 1 | The forced flush at `D_max` deadlocks a synced subscriber with backlog 1 or 2 (L = 0, p = 96, `new` = 96 + b) | **Accepted**, the checkpoint direction. During the step-6 drain, a synced subscriber at its limit makes a **Durable checkpoint at its processed tip** after B1 and resumes once it returns. The drain is path-based (p → `LCA(p, new)` → `new`, dropping off-path queued events, as Core's synced index does), so every checkpoint is an ancestor of `new` = `T_ack`, is its own cap, and raises `h(L)` by `D_max`. Progress: at most `⌈(h(new) − h(F))/D_max⌉` checkpoints. The blind-rewind bound holds because nothing is written above `h(L) + D_max` for the barrier-covered L. Added S11d (b = 0, 1, 2), nested depth 1 around the checkpoint, FA6, T37 (rev3's rule) and T38 (resuming before the checkpoint returns). W's frozen tip is not clamped: the checkpoint needs no change to V or W | §5.6 item 6, §6.2 step 6, §6.2.1, E19 |
| 2 | `min(predicted, actual)` gives no subset for automatic pruning (target stopping; V-ahead growth) | **Accepted**, GPT's fix. Step 0 retains D and `met_target`; step 7 selects `D ∩ eligible(actual)` and never re-applies the target; the check re-arms iff the target is still unmet while D had met it. Added `prune_selection_protocol` with both of GPT's fixtures at both releases, the end-to-end S14b, T39 (re-run selector) and T40 (no re-arm) | §5.6 item 7, §6.2 steps 0 and 7, FA2, E20 |
| 3 | FA1/E20 forbid a full flush with an empty dry run | **Accepted**. `flush_for_prune ⇔ D ≠ ∅`; a full flush iff that or another cause (cache, interval, scripted, `DMax`), as Core's `fDoFullFlush`. S11d's `DMax` flush has an empty manual dry run | §6.2 step 0, FA1, E20, §0 |
| 4 | DEC-69's `LCA(p, new)` exception missing from E12, E14 and the liveness claims | **Accepted**. E12 uses `LCA(p, new)`; E14, §0, §7.3 and §6.2.1 qualify the bound comparison and the one-flush liveness with "on a stable chain", and give GPT's A80/B80/fork 50 example | E12, E14, §0, §6.2.1, §7.3 |
| 5 | N7's `rev sizes ≤ 3` is unjustified | **Accepted**, the real product. 9 × 4 × 2 = 72 for the 240-byte undo fixture; no undo size constraint declared; row recalibrated (0.20 CPU-s) | §6.2 N7, §8.2 |
| 6–9 | Closure of GPT r3, Opus r3, the new crash mechanisms, and budget arithmetic | noted. Opus r3 1, 2 and 6 and 8d were partial only through findings 1–5 above, which now close them | — |

### 13.8 GPT r5 (REWORK; r4 items 2–5 closed)

| # | Finding | Disposition | Where |
|---|---|---|---|
| 1 | r4 item 1 partial: the deadlock is gone, but the checkpoint can certify unwritten batched data (S11d: locator 96 over data through 49) | **Accepted**, GPT's fix control. Every locator commit at X submits all staged data through X and waits for it to return before its `sync_wal`; the subscriber tracks its processed tip p and submitted-data tip s separately. Batching gains one batch per checkpoint (and per cadence commit). S11d gains named crash points (a)–(e) with staged data outstanding, before and after the checkpoint returns. **T41** (rev4's checkpoint without the submit) is caught by R11c(i) under both crash kinds; D1 is silent, because unsubmitted data is in no pending batch. Budget +0.6 CPU-s | §2.4.4, §5.6 item 6, §6.2 step 6, §6.2 subscriber data, S11d, T41, E19, §8.2 |
| 2–5 | r4 items 2–5 | closed in r5 | — |
| nit | §7.3's "flush iff selection is non-empty" | **Fixed**: the prune cause matches `fFlushForPrune`; other causes still force a full flush | §7.3 |

---

## 14. Summary

ST3a is a test-only `faulty` feature of `dashd-store`:
- `FaultyEngine` with subset semantics, per-key scans, and `Append`/`Barrier` events;
- `FaultyFlatFiles` with sector histories, reverting sizes and independent directory operations;
- five named assumptions with witnesses;
- a harness with capture and re-execution modes, rule-L lazy cells, exhaustive clean search, labelled witness search and fixed-point minimisation;
- R0–R13 and a standing cross-store D1.

The toy implements A1 §3.5 with errata E1–E22:
- every finalizer is preceded by a barrier with an explicit scope, including presumed effects and the previous chainstate finalizer before B1;
- Bk, Bp and recovery's final batch are Durable, and recovery syncs each engine's WAL before its first write;
- the prune flush is decided by a dry run under the predicted post-commit lock (`flush_for_prune ⇔ D ≠ ∅`, separate from the other full-flush causes); the final selection is `D ∩ eligible(actual)`, and an automatic check re-arms on a shortfall;
- the DEC-64 order: B1 → subscriber commits → selection → Bk (or Bp);
- locks from durable locators at the LCA, loaded at open, protecting undo; standing block requirements only on the locator's path to G;
- synced subscribers drain along the path to `new`, checkpoint Durably at their `D_max` limit after B1, and commit at `LCA(p, new)`; every locator commit first submits the staged data through its target; syncing ones commit on a cadence capped at `LCA(progress, T_ack)`; a Durable rewind commit precedes processing below the locator;
- blind replay, and a blind subscriber rewind followed by catch-up.

Every prune and lock scenario asserts reachability before it searches, and every forward run asserts FA1–FA6. Acceptance is 50 planted bugs, 7 negative controls, 18 self-tests, 5 witnesses, 4 native cases and the selection-protocol test. The PR gate is ≈ 43.8 CPU-s, and the nightly ≈ 0.65 CPU-h.

## 15. Open questions

**Decided:**
- Q1–Q9 (DEC-59);
- Q10a–Q14 (DEC-63);
- Q10's mechanism (DEC-64);
- Q15 (DEC-66): a syncing subscriber does not block the flush and contributes its durable locator. rev3 adds the `m_synced` classification and the capped cadence that make it live.

All are applied.

**New:**

| # | Question | Recommendation |
|---|---|---|
| **Q16** | ST1's `KvBatch` has no range delete. The blind rewind (§5.5, contract item 13) deletes `D_max` keys one by one: 96 in the toy, but thousands for a production index whose `D_max` must cover a full flush interval. Should ST1 add `KvBatch::delete_range`, which is RocksDB's `DeleteRange` and a range tombstone in `MemoryEngine`? | **Yes**, as a BL item for ST1 before CH8 and the index tasks. `FaultyEngine` models it as one batch op that deletes every key in the range without reading, so rule L, the bounds and R11c are unchanged. Until then, point deletes are correct, only slower. CH8 chooses production `D_max` and its flush trigger with it |
