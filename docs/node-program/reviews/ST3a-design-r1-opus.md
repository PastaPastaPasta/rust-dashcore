Verdict: **APPROVE-WITH-CHANGES**

# ST3a design review, round 1 (Opus)

- **Reviewed:** `designs/ST3a-power-loss.md` rev0 at 483137a1, with DEC-59 (Q1–Q9) and BL-32.
- **Context read:**
  - PLAN rows ST1, ST2, ST3a, ST3b, CH2 and CH8;
  - A1 §2.11, §3.4, §3.5 and §3.11;
  - `dashd-store` on `node/integration`: `engine.rs`, `memory.rs`, `fail_stop.rs`, `rocks.rs`, `model.rs`;
  - ST2's `flatfile/` at 0304cada. It is code-identical to the design's 9e31bfdf: `git diff 9e31bfdf 0304cada -- dashd-store dash-coreimport` is empty;
  - the ST2 reviews r1–r3;
  - Core v23.1.8 and v24.0.0-rc.3 via `git show`;
  - the `librocksdb-sys` 0.19.0+11.8.1 sources (RocksDB 11.8.1);
  - Rust std `sys/fs/unix.rs`.
- **Ran:** one experiment against the pinned RocksDB (finding 1). Scratch is in `/work/scratch/st3a-review-opus/`: `walhole/` holds the program and `core/` the extracted Core sources.

The design is careful, and most of it holds up:
- Lazy DFS with capture mode, D1 as a no-crash diagnostic, and planted bugs with negative controls as acceptance is a good architecture.
- The Core reading is accurate wherever I checked a citation.
- Q9 is real at both pins.

Changes are needed in six areas:
- what the engine model grants (A-PREFIX is not what the pinned RocksDB does);
- the completeness of the Q9 erratum;
- one check gap that lets a plausible CH2 bug through;
- the exact lazy-resolution rule;
- the budgets;
- the CH2 contract.

---

## Findings

### 1. Major: A-PREFIX is false for the pinned RocksDB. W1/W2 expect the wrong outcome, and N2/N4 are safe only under A-PREFIX

**Location.**
- §2.3, assumption A-PREFIX (line 157);
- §2.4, line 169: "Zeroed preallocation counts as the end of the log";
- §4.1 (line 433);
- §6.2, N2 and N4 (lines 754, 756);
- §6.4, W1 and W2 (lines 786–787).

**Code (RocksDB 11.8.1).**
- `db/log_reader.cc:611-619`: a header with type 0 and length 0 gives `buffer_.clear(); return kBadRecord;`. The comment says this "should never happen in DB written by new RocksDB versions".
- `ReadRecord` loops with `for (;;)` (`:95`). Outside a fragmented record, `case kBadRecord` just `break`s out of the switch without `ReportCorruption` (`:298-304`), and the loop reads the next 32 KiB log block.
- `DBImpl::MaybeReviseStopReplayForCorruption` (`db/db_impl/db_impl_open.cc:1613-1628`) only *clears* the stop flag when sequence numbers are consecutive. Nothing sets it on a gap.

So a zero-filled page that starts at a record header makes recovery silently skip the rest of that log block, and a valid record at the start of the next block is replayed.

**Zero pages are the normal post-crash content of unwritten WAL regions.**
- The WAL is preallocated: `CreateWAL` calls `SetPreallocationBlockSize` (`db_impl_open.cc:2465`), and `PosixWritableFile::Allocate` calls `fallocate(FALLOC_FL_KEEP_SIZE)` (`env/io_posix.cc:1900-1915`), with `allow_fallocate = true` and `ROCKSDB_FALLOCATE_PRESENT` on Linux. Unwritten extents read as zero.
- Without preallocation, a delalloc hole reads as zero too.
- Pages are written back in any order, as §2.3 row 5 itself says.

**Experiment.** The program is `walhole/src/main.rs`. It uses the pinned `rocksdb` 0.25 / RocksDB 11.8.1 with `rocks.rs`'s options: atomic flush, PointInTime, paranoid checks.
- Setup: B1 is Durable and ends at offset 4096. B2 is Buffered and fills [4096, 32768). B3 and B4 are Buffered and start the next log block.
- The crash image is a copy of the live database directory. The program zeroes a range of the WAL in the copy and reopens it.

| Zeroed WAL range | Keys recovered |
|---|---|
| none | b1 b2 b3 b4 |
| [4096, 8192): B2's first page only | b1, **b3, b4**; no b2 |
| [4096, 32768) | b1, **b3, b4**; no b2 |
| [8192, 16384): inside B2's record | b1 (a prefix, as intended) |

The LOG shows "Recovered to log #4 next seq #6" and no corruption message.

The trigger needs a hole that begins at a record header, and a next record that begins at a 32 KiB block boundary: the previous record ended inside the 6-byte trailer or exactly at the boundary. That is rare per crash, but reachable and silent.

**What depends on A-PREFIX.**
- **The correct toy does not.** B1 and Bk are Durable. Every buffered batch that can be lost lies between two barriers, under a durable marker, and the coins-only replay converges whichever coin sub-batches survived. For the same reason a per-CF prefix of the buffered sub-batches is harmless: they touch only `coins`. So **W1 and W2 will pass on toy S4**, not fail as §6.4 expects. That is good news for the protocol, but the acceptance table is wrong, and an implementer will bend the toy or the witness to match it.
- **N2 and N4 do.**
  - N2 (B1 Buffered): a non-prefix recovery keeps B2 without B1, so coins change with no marker, and R3 fires.
  - N4 (Bk Buffered): a non-prefix recovery keeps Bk (`tip = new`, marker erased) without B3, and R3 fires.
  - A1 §3.5.4's sentence "A Buffered Bk would also be crash-safe, since every prefix recovers" holds only under A-PREFIX.
- **Subscribers do, if they commit block data and the locator in separate Buffered batches.** Core's `BaseIndex::Commit` uses an unsynced `WriteBatch`. The locator batch can then survive without an earlier data batch. The toy's subscriber holds only a locator, so nothing tests this.
- **§4.1's "a crash during an engine write needs no extra point" assumes a prefix.** Under subset semantics, a Durable write interrupted after its append and before its fsync gives "B_k kept, an earlier buffered batch lost". That state is in neither image k−1 nor image k.

**Fix.**
- Rewrite §2.4 row 3 and A-PREFIX. RocksDB 11.8.1's PointInTime recovery is a prefix *except* when a zero header at a record boundary is followed by a record at a block start; cite the lines above.
- Keep A-DUR and per-batch atomicity. Atomicity of one batch does hold: a hole inside a fragmented record is reported ("error in middle of record", `log_reader.cc:300` and `:333`) and stops replay.
- Choose one of:
  - **(a)** make the `Realistic` engine adversary grant only A-DUR and per-batch atomicity: any subset of the batches buffered since the last barrier, plus an "appended, not yet synced" point for each Durable write;
  - **(b)** keep the prefix in the PR gate for cost, and run the subset adversary over every scenario nightly.

  Either way the correct toy must be clean under subset.
- W1 and W2: run them on N2 and N4 with expected **fail**, and on N1 with expected **clean**.
- Reclassify N2 and N4: no longer "negative controls", but "safe only under A-PREFIX", or planted bugs under (a).
- Put the A1 §3.5.4 sentence in the BL-26 errata batch.
- Require subscriber locator commits to be Durable, or a `sync_wal` before persisting the locator. Put this in §5.6 and in the CH7a/IX/LK briefs.
- Give the toy subscriber per-block data batches plus a locator batch, so that W1 shows the dependency.
- Hand ST3b this exact case. `dm-log-writes` prefix replay will rarely land on it.

### 2. Major: Q9 is confirmed at both pins, but a process kill reaches it too. "Core survives a process crash anywhere" is false

**Location.** §0 item 6 (line 24); §7.2 "Process crash" (line 818) and item 3 (lines 822–827); §7.3.2; Q9.

**Q9 confirmed.** Citations are v23.1.8 / v24.0.0-rc.3.
- Selection uses the in-memory tip: `node/blockstorage.cpp:202` / `:198` (`nLastBlockWeCanPrune = min(prune_height, tip − MIN_BLOCKS_TO_KEEP)`); the manual path is at `:179` / `:175`.
- `PruneOneBlockFile` clears `HAVE_DATA` and `HAVE_UNDO` and zeroes the positions: `:143-147` / `:139-143`.
- The synced index write precedes the unlink and the coins write: `validation.cpp:2850, 2857, 2865, 2884` / `:3058, 3065, 3073, 3092`. `WriteBatchSync` ends in `WriteBatch(batch, true)` (`txdb.cpp:276` / `:285`).
- Without `HAVE_DATA`, `GetBlockPos()` returns a null position (`chain.h:208-217` / `:216-225`), and `FlatFileSeq::Open` returns `nullptr` for it (`flatfile.cpp:35-37` / `:36-38`). So `ReadBlockFromDisk` fails **whether or not the file still exists**.
- `RollforwardBlock` then fails (`validation.cpp:4780` / `:5189`), `ReplayBlocks` returns false, and startup stops with "Unable to replay blocks…" (`init.cpp:2066` / `node/chainstate.cpp:251-252`).
- Prune mode refuses `-reindex-chainstate` (`init.cpp:1204` / `:1256`), so the only way out is a full `-reindex`.

**Practical trigger: the *first* prune flush of a fresh pruned IBD.**
- Until usage first exceeds the target, the coins cache may never have flushed, because the early UTXO set fits in `-dbcache`.
- So `old` is null or far behind, and replay starts near height 1, in files this same flush pruned.
- The flush's coins `BatchWrite` is large (many sub-batches), so the crash window is long.

**What the design gets wrong.** Everything above survives a **process kill**: the index write was synced, and a SIGKILL loses nothing the kernel already holds. The design lists the hole only under "Power loss".

**A second process-kill counterexample.**
- `CoinsTip().Flush()` (`:2884` / `:3092`) runs before `m_evoDb.CommitRootTransaction` (`:2889` / `:3097`).
- A SIGKILL between the two leaves coins at `new` with no marker, so `ReplayBlocks` returns early, and EvoDB at `old`.
- The next `DisconnectBlock` (VerifyDB) or `ConnectBlock` then refuses with "Found EvoDB inconsistency, you must reindex to continue" (`validation.cpp:2002, 2322` / `:2288, 2527`).

**Why `feature_dbcrash` never hits either window.** `-dbcrashratio` exits only between coin sub-batches (`txdb.cpp:147-154` / `:156-163`), and the test never prunes.

**Fix.**
- Correct §0 and §7.2. Core survives a process kill at the coin sub-batch boundaries that `feature_dbcrash` exercises, not anywhere; list the prune window and the coins→EvoDB window as process-kill holes.
- Say that T13 and T5 fire under ProcessKill, and make S5 run both crash kinds. §7.4's "exhaustively" claim then covers the right set.
- DEC-59 already classified this hole as a durability bug; the wider trigger does not change that ruling.

### 3. Major: Moving the prune's index changes into Bk is necessary but not sufficient

**Location.** §12.6 (lines 958–963); Q9 (line 993); T13 (line 745); R9 and R11 (lines 573, 575); S5 (line 722).

The Bk placement closes the T13 window as stated. Pruned files are never unlinked before their cleared entries are durable, because step 7 follows a Durable Bk; ST2's `unlink_pruned_files` doc already requires this. I looked for other consequences and found four.

**(a) Selection height under freeze-and-flush.**
- Core's selection uses `m_chain.Height()`. Core flushes synchronously, so that is the tip being flushed.
- A1 §3.4 lets V keep connecting while W writes the frozen layer F (tip `new`).
- If the prune bound uses V's tip T, with T > new + 288, then this Bk clears and step 7 unlinks the files holding blocks in (new, T − 289].
- The next flush's marker is [new′, new], and its replay needs exactly those blocks. A crash between B1′ and Bk′ then gives the same "Unable to replay".
- D1 would flag it at B1′, because the marker needs blocks with no durable position. But the toy flushes synchronously, so V is never ahead, and S5 cannot produce the case.
- **Fix:**
  - The erratum must also say "selection uses the frozen layer's tip `new` as `chain_tip_height`", which is what Core effectively computes.
  - Add a "V ahead by N blocks" knob to the toy.
  - Add planted bug **T15**: selection against V's tip.

**(b) Subscribers' durable locators.**
- A1 §3.5.7's subscribers are `llmq/`, every index, and wallets. Each persists its locator only after `ChainStateFlushed`, in its own database.
- A crash after Bk and the unlink, but before a subscriber's commit, leaves it at its previous locator, about `old`. To catch up it must re-apply `BlockConnected` for (old, new], and some of those blocks were just pruned. The condition is the same >288-block gap.
- Core has this hole for indexes and wallets. An index's prune lock is its **in-memory** best block: `SetBestBlockIndex` is called from `BlockConnected`, before `ChainStateFlushed` commits the locator (`index/base.cpp:390-396` / `:437-444`). Wallets rely on an init error.
- A1 makes it worse: `llmq/` is a mandatory subscriber that re-applies blocks, so a pruned node could not start without a reindex.
- Neither R9 nor R11 covers it.
- **Fix:**
  - Add check **R11b**: every block in (subscriber's recovered locator, recovered chainstate tip] is `HAVE_DATA` in the recovered index.
  - Add a D1 extractor rule: a durable subscriber locator requires those blocks.
  - Add planted bug **T16**: the prune lock comes from the subscriber's in-memory height.
  - Give the toy's subscriber a prune lock.
  - The protocol fix is the manager's call; I suggest it as **Q10**. A prune lock taken from the durable locator changes selection only when a subscriber lags by more than 288 blocks. Deferring the clear to a later flush changes `pruneblockchain`'s answer. Either way it belongs in CH8's brief, next to Q9.

**(c) `meta.pruned_files`.**
- ST2's startup scan runs only when `have_pruned` is set (`flatfile/blocks.rs:677-686`). Core v24 has the same gate (`blockstorage.cpp:412-417`), and v23.1.8 has no scan at all.
- After the Bk fix, a crash between Bk and the unlink's `sync_dir` leaves files whose durable size is 0, and R9 relies on the scan to remove them. So the flag must be durable no later than Bk.
- Core writes the flag unsynced into the same LevelDB, just before `WriteBatchSync` (`validation.cpp:2814` / `:3022`).
- State where the toy and CH2 put it.

**(d) ST2 returns pruned and unpruned infos together.**
- `find_files_to_prune*` calls `prune_one_block_file`, which marks the file dirty (`blocks.rs:561-565`). `sync_and_take_dirty_infos` returns every dirty info at once (`:226-236`).
- So "step 3 takes every dirty record except those `PruneOneBlockFile` changed" means the toy and CH2 must partition `DirtyFiles`, and the index entries, by the pruned set.
- Say so in §12.6, or ask ST2 for a split accessor. "No ST2 change is needed" stays true if the partition is the caller's job.

### 4. Major: A planted bug that passes R0–R13 and D1. B1 drops the dirty `block_files` record

**Location.** R1 view form (line 565); D1 (§5.3, line 638); R12 (lines 576, 656).

**Bug T14.** B1 writes the dirty `block_index` entries but drops the `DirtyFiles` infos, or writes a stale snapshot of them. This is a plausible CH2 slip, for example:
- on an error path;
- in the periodic index-only write (step 8) writing only `block_index`.

ST2's `sync_and_take_dirty_infos` *takes* the set, so a second call returns nothing.

**Run.**
1. Before the flush, blk N's durable `size` is 4000.
2. Block b is appended at 5000 and synced, and B1 records b as `HAVE_DATA` at 5000, but not blk N's new `size`.
3. Crash. Recovery opens `BlockFiles` from the durable state, with size 4000.

**What passes, and why.**
- R0 and R2–R6 pass.
- R1's store and view forms pass: b reads back, and `size ≤ length` holds.
- D1 passes, because b's bytes were durable at B1.
- R12's continuation appends the next block at 4000, overwriting b, then flushes, restarts and "compares the state". Nothing re-reads b.

Core keeps the invariant implicitly: blk files are append-only from `nSize`.

**Fix.**
- R1 view form: every `HAVE_DATA` range lies within `[8, block_files[nFile].size)` of the recovered state, and every `HAVE_UNDO` range within `undo_size`.
- D1: any `write_all_at`, `truncate` or `allocate` that touches a range a durable record references is a violation. Referenced bytes are immutable until the reference is durably cleared.
- R12: re-run the store-level and view-level checks after the clean restart.
- Add T14 to the T-list.

### 5. Major: The exhaustiveness claim needs an exact lazy-resolution rule; as written it allows a coverage gap

**Location.**
- §2.2 table: `sync_data` makes "durable := cur" (line 93);
- §4.3 (lines 444 and 448);
- M10 (line 773);
- §4.2, nested crashes.

**The gap.** §4.3's argument is that two assignments agreeing on every variable a run read give the same run. That holds only if no operation evaluates an undecided variable without recording it. The text forces variables only for reads, extending writes and partial-sector writes. But some operations copy or promote undecided cells in a booted world:
- `sync_data` (durable := cur);
- `copy`;
- `truncate` and `allocate`: re-extending a file exposes or zeroes cells depending on the size variable.

Nested crashes add a fourth case: an outer variable that the first recovery never read must keep a single value across epochs.

**Counterexample.**
1. A crash image has blk N durable up to D, plus a pending pre-crash block b2 at [D, E), with sector variables v and size variable S.
2. A buggy recovery adopts trailing bytes, for example `adopt_existing_files` plus "take the info from the file length". It reads `len()`, which forces S (choice 0 gives E).
3. It runs the barrier. That barrier's `sync_data` promotes [D, E) using v's choice 0 ("b2 survived"), without recording v.
4. It commits b2 as `HAVE_DATA`. D1 passes, because b2's bytes are durable by then. R1's read sees b2 intact.
5. The DFS never tries v = lost: the real state where b2's pages are zero but the size survived, in which recovery indexes a block of zeros.

Whether this is caught depends on an implementation choice the design does not state.

**Fix.**
- State the rule:
  - A booted image is an overlay of lazy cells.
  - Every operation whose result or post-state depends on a cell either forces it or carries it symbolically.
  - `sync_data`, `copy` and `rename` move undecided cells into the durable image as the same `Var`.
  - A `Var` is decided at most once per branch, across all epochs.
- Symbolic promotion is also what keeps R12 and nested runs cheap: forcing every sector of a synced file would make the search explode.
- Widen M10's generator to apply random post-crash mutations (all 14 `FlatFs` calls, plus engine writes) and nested crashes before its reads.
- R7's "compared logically" must go through `ChainstateView` and digests, never raw file bytes; otherwise it forces every cell.

### 6. Major: The 60 CPU-s PR gate and the 1 CPU-h nightly are not credible as specified

**Location.** §8 (lines 866–868); §4.7; §11 (line 944); §6 (a minimised repro for every planted bug).

**Why the numbers don't add up.**
- **Minimisation runs in the PR gate.** Each of the 21 planted bugs asserts that "the minimised repro replays to the same invariant".
  - `minimize` defaults to 2,000 replays, and each replay re-runs the workload from scratch.
  - S5 is about 320 blocks, with ST2 writes, connects, SHA-256 and undo checksums, all in debug.
  - Even at 0.1–0.3 s per replay, the three S5 bugs alone (T5, T11, T13) cost hundreds of CPU-seconds.
- **The budget table leaves things out.** W1–W4 run every PR (§11), and so do the twin's 10³ seeded inputs; neither is in the table.
- **S5's recoveries are expensive.** Each one replays about 310 blocks, then R1 re-reads about 310 blocks and checks about 310 undo checksums: per branch, in debug.
- **Size variables multiply branches even for a correct protocol.** ST2's `BlockFileReader` calls `len()` on every record read (`blocks.rs:741`). That forces the size variable of every file it reads that has pending size changes. §4.3's cost argument leaves this out.
- **The nightly is internally inconsistent.**
  - 10⁵ cases in 3,600 CPU-s is 36 ms per case.
  - Nested depth 2 at one crash point costs about (recovery events)² recoveries: around 400 ms per case for a 20-event recovery in release mode.
  - That is roughly 10× over.

**Fix.**
- The PR gate replays checked-in, pre-minimised repros for P1–P8 and T1–T16. Minimisation runs nightly.
- Make `MIN_BLOCKS_TO_KEEP` a toy parameter (for example 8), so S5 needs about 40 blocks in the PR gate. Use 288 nightly.
- Add `[profile.dev.package.dashd-store] opt-level = 1`, or run the suite in release, and say which.
- Add W1–W4 and the twin to the budget table, with the twin at about 10² cases per PR.
- Nightly: run nested depth 2 on a 1% sample.
- Put a back-of-envelope estimate (events × branches × per-recovery cost) for S5 and S4 into §8 now. Phase B's calibration then confirms a target instead of discovering one.

### 7. Major: Gaps in the CH2 contract (§5.6, Q5) that will break the seam

**Location.** §4.9 (line 546); §5.2; §5.6 (lines 658–671).

**(a) Determinism versus A1 §3.4.**
- The coins cache is a `hashbrown::HashMap<OutPointKey, Entry, SaltedFxBuild>`, and the flush iterates it.
- A random salt changes which coins go into which sub-batch, and in what order, from run to run. The crash images change with it, so M11, replay and minimisation all fail on CH2.
- **Requirement:** a fixed salt, or a seeded hasher, under the test seam.
- §4.9's "no HashMap in faulty/ or its SUTs" cannot hold literally for CH2. Reword it as: no dependence of store calls on iteration order or thread scheduling. The same goes for "no threads": a rayon index decode at load is fine if no store call happens on a worker thread.

**(b) `coins_cache_model` is cache-level, not chain-level.**
- Its ops are spend, create, flush completion, prefetch, eviction and reorg (A1 §3.4 point 5).
- It has no blocks or undo with which to implement `ChainRef::block`, `undo` or `best_valid`.
- Make `check_chainstate` composable, so that R0, R2, R3 (coins), R4, R7 and R8 run with a `CoinsRef`, and say which subset the coins model runs.

**(c) Digest scope.**
- Define `evo_digest` (or a `state_digest`) as covering every column family other than `meta`, `block_index` and `block_files`, in key order.
- Otherwise a CH2 digest over the masternode list alone passes R3 even when B1 drops `llmq_mined_h` or the credit-pool deltas.

**(d) `Meta` lacks `pruned_files` and `last_file`.** Both are A1 `meta` keys. R9's scan and `BlockFiles` reopening depend on them.

**(e) V running ahead of W.** Finding 3(a) belongs in this list too.

**(f) `replayed()` is a self-report.** R6's real teeth are its black-box S7 clause. Say so, so that CH2 does not treat `replayed()` as the check.

### 8. Minor: `Realistic` is weaker than ext4, XFS and btrfs in two places, so "at least as adversarial" holds only for `Extended`

**Location.** §2.2 items 1–2 (lines 108–109); §2.3 row 7; §0 item 2.

- **Sizes.**
  - ext4 delalloc and XFS update the on-disk size when write-back completes. One extending `pwrite` can therefore leave an intermediate, block-aligned size: the `i_disksize` after only part of the write was written back.
  - That size was never "held since the last sync", so `Realistic` cannot produce it.
  - **Fix:** add the granule boundaries inside each extending write to the `Realistic` size domain. This is cheap, because ST2 preallocates and rarely extends a file by writing; MicroLog and scanners do.
- **Directories.**
  - ext4 `fast_commit` and btrfs's log tree persist a fsynced new file's own dentry without the earlier ops in the same directory, so "B created, A not" can happen.
  - A per-directory prefix never produces that state.
  - **Fix:** add it to `Extended`: creations of files that were later fsynced may persist out of order.
- **Wording.** In §0 and §2.3, say that `Extended` is at least as adversarial as these filesystems, and that `Realistic` has these two documented exceptions.

### 9. Minor: Sector granularity is enough for pending bytes, but one assumption is missing

**Location.** §2.2 item 3; the §2.3 assumptions table; §6.4.

512-byte sectors are finer than 4K pages. Per-sector temporal prefixes therefore already include torn 4K pages and 4Kn atomic units. Nothing needs to change for pending bytes.

Not modelled, and not named:
- **A-PSOW: an unsynced write never changes bytes outside its own range.**
  - ST2 appends each record right after the last synced one, in the same sector.
  - On 512e drives, a torn read-modify-write of that physical sector can destroy the synced tail, or leave the sector unreadable (EIO) after the reboot.
  - SQLite names this assumption "powersafe overwrite".
  - **Fix:** name A-PSOW and give it to A-DEV's owner. Add witness **W5**: a power loss damages the sector shared by the last synced record and the next append. Expect R1 to fail; that documents the dependency. Padding is not an option, because blk files must stay byte-identical to Core's.
- **Unreadable sectors.**
  - §4.4 makes every read error during recovery fatal. So a scanner that ought to treat an unreadable unsynced tail as end-of-data is never exercised.
  - **Fix:** add an "EIO on read" version for unsynced sectors to `Extended`.
- **Misdirected or lost writes after a flush.** These fall under A-DEV and cannot be modelled. Say so.

### 10. Minor: Check definitions

**Location.** R3/R4 allowed set (line 558); R8 (line 572); R7 (line 571).

- **The allowed set A comes from the SUT's own `FlushBegin` marks.** A SUT that marks a wrong `new` widens A. Build A from the reference's flush list (`ChainRef::flush`) instead, and flag any mark that disagrees with it.
- **R8 counts any store error, reads included.**
  - Core aborts on a failed read in `ConnectTip`, but not on one in RPC `getblock` or in P2P `getdata`.
  - **Fix:** restrict R8 to I5's set: writes, syncs, commits, and reads on the connect and recovery paths. Otherwise CH2 will see false positives.
- **R7's "compared logically"** must be defined as comparing through `ChainstateView` (see finding 5).

### 11. Minor: BL-32 is verified, and it puts us below Core

**Verified.**
- `librocksdb-sys` 0.19.0's `build.rs:239-242` defines only `OS_MACOSX`, `ROCKSDB_PLATFORM_POSIX` and `ROCKSDB_LIB_IO_POSIX` for darwin, and never `HAVE_FULLFSYNC`.
- RocksDB's own `CMakeLists.txt:606-608` and `build_tools/build_detect_platform:792-801` do define it.
- Without it, these paths fall back to plain `fdatasync`/`fsync`:
  - `PosixWritableFile::Sync` (`env/io_posix.cc:1826-1838`) and `Fsync` (`:1840-1852`);
  - the directory path (`:2146-2157`);
  - the mmap path (`:1605-1635`);
  - the random-RW path (`:2024-2047`).
- Rust std maps both `sync_data` and `sync_all` to `F_FULLFSYNC` on Apple (`library/std/src/sys/fs/unix.rs:1385-1402`), so the design's claim about ST2's flat files holds.

**Additions for BL-32.**
- **This is a regression below Core.**
  - Core's LevelDB uses `F_FULLFSYNC` on macOS at both pins, via `configure.ac:1338-1342` / `:1331-1335`, then `src/Makefile.leveldb.include:23`, then `leveldb/util/env_posix.cc:376-384`.
  - Core's `DirectoryCommit` is a plain `fsync`, so ST2's `sync_dir` is stronger than Core's there.
- **RocksDB's `F_FULLFSYNC` path has no fallback.** It returns `IOError` (`io_posix.cc:1829-1831`), while LevelDB falls back to `fsync` (`env_posix.cc:381-384`).
  - So defining `HAVE_FULLFSYNC` turns a volume that lacks `F_FULLFSYNC` support into a fail-stop on the first durable commit.
  - Include that case in pasta's Mac check, and decide whether to patch in a fallback.
- **The cargo `[env]` route works.** The `cc` crate honours `CXXFLAGS_<target>`, so no `[patch]` is needed.

### 12. Minor: Smaller model gaps

- **Reads after a failed fsync.**
  - After a failed write-back, ext4 keeps the pages but marks them clean. A later read returns the cached new data until the pages are evicted, then the old bytes from disk. XFS may invalidate them sooner.
  - The model makes poisoned data only a crash-time variable.
  - **Fix:** add a read-time variable in `Extended`. P5 is caught either way.
- **`flush_memtables` is missing from A-DUR,** although §2.4 treats it as a barrier.
- **The assumption count is wrong.** §13 says "four named assumptions", but the table in §2.3 has five.

### 13. Minor: The ~15 agent-day estimate is light

Phase C is budgeted at 4 days but has to build:
- a reduced CH2: a split flush, replay with reorg undo, validating activation, manual and automatic prune, a subscriber, effects, both releases, and a reference;
- R0–R13, D1 and an extractor;
- ten scenarios;
- 13 or more planted bugs, each a coherent knob that its named invariant must catch under both releases.

ST2, also size M, needed three review rounds.

**Fix.**
- Plan 6–8 days for Phase C, plus a fix round per checkpoint: about 20–25 agent-days in total.
- Consider closing M0 exit 7 at review checkpoint 1, after Phase B. The exit text needs only the self-tests.

### 14. Nits

- **Method count.** "all 13 methods" (line 321): `FlatFs` has 14, counting `available_space`.
- **The `cargo tree` check (line 232).** `cargo tree -e features` follows dev edges by default, so a downstream crate's test-only use of `faulty` will trip it. Use `-e features,no-dev --target all`.
- **ST2 revision.** The header cites ST2 at 9e31bfdf. The branch head 0304cada is code-identical; cite the head to avoid confusion.
- **Truncate, then re-extend.** Define this case: a `SetLen` below a sector should reset that sector's history to zero, so re-extension reads zeros, as ext4 and XFS do when a file is extended. Today the model would show older durable or pending bytes there. That is over-adversarial and therefore safe, but M3's brute-force oracle needs the rule.
- **Truncate durability.** A1 §3.5.1 and ST2's `fs.rs:38` say a directory fsync makes truncations durable. On Linux, `fdatasync` covers the size change from `ftruncate` (see fdatasync(2)); under POSIX a directory fsync does not. The model, which persists size through `sync_data`, is right. Add the wording to BL-26.
- **R9 and the startup scan.** R9's text should say three things:
  - the scan exists only in v24 Core (`blockstorage.cpp:412`);
  - v23.1.8 has no such scan;
  - the toy runs ST2's scan under both `Release` values (CONS-28).

---

## Summary

ST3a's core is sound:
- a lazy, capture-mode power-loss model;
- D1 as a no-crash diagnostic;
- planted bugs with negative controls as acceptance;
- a careful Core analysis, accurate wherever I checked it.

Q9 is real at both pins.

Changes required:
1. **Stop granting A-PREFIX as RocksDB behaviour.** The pinned RocksDB recovers non-prefix states; I reproduced it (finding 1). The correct protocol is unaffected, but:
   - W1 and W2 expect the wrong outcome;
   - N2, N4 and split-batch subscribers depend on A-PREFIX.
2. **Correct "Core survives a process crash anywhere".** The prune window and the coins→EvoDB window break it under SIGKILL (finding 2).
3. **Complete the Q9 erratum** (finding 3) with:
   - selection against the frozen tip;
   - subscribers' durable locators (R11b, T16);
   - `pruned_files` durability;
   - the `DirtyFiles` partition.
4. **Add the referenced-range checks that catch T14** (finding 4).
5. **Pin down the lazy-resolution rule and widen M10** (finding 5).
6. **Re-budget the PR gate and the nightly** (finding 6): minimisation, the twin, S5's scale, debug cost, nested depth.
7. **Extend CH2's contract before its D2 designs start** (finding 7): determinism, a coins-level check profile, digest scope.

None of these needs a redesign. **APPROVE-WITH-CHANGES.**
