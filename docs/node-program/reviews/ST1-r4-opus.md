# ST1 review, round 4 (Opus, focused): `dashd-store` + `dash-coreimport` fixes

- **Branch:** rust-dashcore `node/st1-store-bakeoff` at 4dbd0281, rebased onto node/integration a9d24aa6. Worktree `/work/home/agent/workspace/rdc-wt/st1-store-bakeoff`.
- **Commits reviewed:** the 13 commits 9e69b61e..4dbd0281. They answer `reviews/ST1-r3-opus.md` (M1, M2, m1–m10, n1–n9).
- **Reference sources:** RocksDB 11.8.1 as vendored in `librocksdb-sys-0.19.0+11.8.1`, and Core `validation.cpp` / `streams.h` at v24.0.0-rc.3 (`LoadExternalBlockFile` is unchanged in its scan logic since v23.1.8, per round 3).
- **GPT:** unavailable (DEC-42).
- **Branch state:** read-only. Nothing on the branch was modified.

## What I ran

| Check | Result |
|---|---|
| `cargo test -p dashd-store -p dash-coreimport --all-features` (TMPDIR=/dev/shm/st1-r4-tmp, private target dir) | All pass. dash-coreimport: 15 + 3 + 2. dashd-store: 26 unit tests and `fail_stop` 1 (2 ignored). |
| `DASHD_STORE_SMALL_FS` tests (`--ignored --test-threads=1`) on a 200 MiB tmpfs mounted at `/mnt/st1-r4-smallfs` (unmounted afterwards) | **Both pass** (25 s). |
| `refuses_writes_after_disk_full_even_once_space_is_freed` | The first failure is `NoSpace` (appending `000014.sst`). The write 12 s after freeing space is refused with `ReadOnly`, and reopen keeps every acknowledged durable batch. |
| `refuses_writes_after_a_background_error_rocksdb_recovered_from` | The LOG shows RocksDB resumed by itself (`resumed: true`). The next write is still refused, with message "2 background flush or compaction error(s), for which RocksDB did not stop writes: IO error: No space left on device …". The kind was read back from the LOG as `NoSpace`, so the LOG path works end to end on 11.8.1. |
| `cargo clippy -p dashd-store -p dash-coreimport --all-features --all-targets -- -D warnings` | clean |
| `cargo fmt --check` | clean |

## 1. M1 fail-stop

**The latch is correct, and I found no way for a write to slip past it.**

**Structure.**
- `FailStop` (`fail_stop.rs`) is a `OnceLock` that keeps the first failure. `RocksEngine::guarded` (`rocks.rs:272-277`) wraps `write`, `sync_wal` and `flush_memtables`. Each call does three things in order:
  1. Refuse if a failure is latched.
  2. Run `check_background`.
  3. Run the operation, and latch any error.
- `wait_for_compactions` latches background errors too (`rocks.rs:217`).

**Paths I checked against RocksDB 11.8.1:**

- **Background errors are counted DB-wide, monotonically, for every background flush or compaction failure.**
  - The counter is `bg_error_count_` on the default CF's `InternalStats`.
  - It is bumped in `BackgroundCallFlush` (`db_impl_compaction_flush.cc:3964`) and `BackgroundCallCompaction` (`:4067`).
  - These two functions run every flush, including atomic flushes, manual `flush_cfs_opt` flushes and memtable-full flushes, and every compaction, manual or automatic.
  - Exclusions:
    - Shutdown, dropped CF, paused or aborted compactions, and `Busy`.
    - The `kErrorRecovery` flush. That flush only exists after an earlier error that was already counted.
    - Benign statuses that don't fail anything: I found none in the default configuration, since mempurge's `Aborted` is off.
  - The counter is reset only by `InternalStats::Clear`, which only `DB::ResetStats` reaches. That is reachable only through `raw()`, which is documented as never-write; nothing in the branch calls it.
  - A new `InternalStats` per open means a count always refers to this open. Errors during open fail the open.
- **Errors in a column family:** the counter is DB-wide, so an error in any CF is seen.
- **Errors during atomic flush:**
  - A foreground `flush_memtables` error is returned and latched.
  - A background atomic flush error is counted.
  - A `NoSpace` flush with `paranoid_checks` is `kHardError`, so RocksDB stops the DB. `PreprocessWrite` (`db_impl_write.cc:2107`) then fails every write that joins later. So even a write racing the background failure is refused by RocksDB itself until auto-recovery, which starts at least 5 s later.
- **WAL write and sync failures:** these are foreground, on the write or `sync_wal` call. The error is returned and latched. Whether RocksDB itself stops (for example, whether `FlushWAL(sync)` failures call `SetBGError`) no longer matters.
- **Race between the counter check and the write.** It can happen only for a background failure that RocksDB does not stop for, that is, a soft error such as compaction `NoSpace` (`error_handler.cc:26-29`, `kSoftError`). It needs the bump to land between the check and the write. Such a write is concurrent with the failure, which the trait contract explicitly leaves unordered (`engine.rs:142`). A background compaction failure is not a commit whose outcome is unknown, so I5 is not at stake. Acceptable.
- **The probe write** (`rocks.rs:254`) is an empty batch. When the DB is stopped it returns the stopping error; otherwise it commits nothing. It can block in `DelayWrite` only where a real write would block too. Not an issue.

**Is reading the error kind from RocksDB's LOG robust?** It is safe: every failure to parse falls back to kind `Io`, and the latch never depends on the LOG. It is not fully reliable for the kind, though; see **m1** below.
- **Rotation:** RocksDB rotates `LOG` only on open (`max_log_file_size` = 0, `log_file_time_to_roll` = 0). A counted error always belongs to the current open, so its line is in the current `LOG`.
- **Format across versions:** the two `ROCKS_LOG_ERROR` formats are the only matches for "after background" in the tree. The parse (`split_once("after background ")`, then the first `"error: "`) extracts `IO error: No space left …` correctly. If the wording ever changes, the result degrades to `Io`. Nothing pins the format in CI except the ignored test.
- **Missing LOG** (or `db_log_dir` set some day): falls back to `Io`, and the message points at the LOG. Safe.

## 2. m3 directory classification

**No loss path found.** Every place a missing `CURRENT` could lead:
- **Only creation debris left.** RocksDB creates `CURRENT` once, in `NewDB` after `MANIFEST-000001`. Later MANIFEST rolls replace it with an atomic rename from `NNNNNN.dbtmp`. So a missing `CURRENT` means either a creation that died early or outside damage.
- **Outside damage.** A database that has ever held its schema record keeps it either in a WAL (`*.log`) or in an SST, because the Durable stamp goes to the WAL and a flush moves it to an SST. `is_database_data` matches every numbered RocksDB data name (`.sst`, `.log`, `.blob`, legacy `.ldb`, any digit count), so such a directory is reported as `Corruption` and never recreated. The test `reports_a_damaged_database_as_corruption` covers this and asserts that the directory is unchanged.
- **A broken-symlink `CURRENT`.** `try_exists` returns false, the `CURRENT` entry is not debris, and the result is `UnknownDatabase`. Nothing is overwritten.
- **An unreadable directory or `CURRENT`.** The result is an `Io` error.
- **A truncated or empty `CURRENT`.** `list_cf` fails with RocksDB's error, and no creation is attempted.
- **A LevelDB chainstate missing `CURRENT`.** Its `.ldb` files make it `Corruption`. The message is misleading ("ours"), but nothing is replaced.
- **Debris only.** The only bytes RocksDB overwrites when creating over debris are debris itself (`MANIFEST-000001`, `OPTIONS`, `LOG`). The test `creates_over_the_debris_of_a_failed_first_creation` builds debris from a real creation.

The one judgement call is `lost+found`. See n2.

## 3. dash-coreimport m1 and m2

**The blk scan stops where Core does.** I traced `LoadExternalBlockFile` together with `CBufferedFile` at v24.0.0-rc.3:
- **A size that runs past EOF.** `SkipTo(nBlockPos + nSize)` → `AdvanceStream` → `Fill` throws at EOF, and the inner catch logs. Then `eof()`, which is `m_read_pos == nSrcPos && feof`, is true and the loop exits.
- **A header cut short.** Reading the 80-byte header throws in the same way and the loop exits the same way.
- **A record ending exactly at EOF.** Core exits, and Rust finds no further magic. Same result.
- **Magic in the last 4–7 bytes.** `blkdat >> buf` / `>> nSize` throws, and the outer catch breaks. Rust breaks.
- **An out-of-range size.** Both resume at magic+1.

`scan_block_records` (`blockfiles.rs:88-118`) matches every one of these cases, and the test encodes Core's rule.

**The documented remaining difference (slack after a deserialized block).**
- Core resumes at `blkdat.GetPos()` after `blkdat >> *pblock` only when the parse *succeeds*. A throw takes the catch, with `nRewind` already set to the claimed end, so Core skips the full span exactly as Rust does.
- `WriteBlockToDisk` writes `nSize = GetSerializeSize(block)`, so an intact Core-written record has no slack.
- The difference is therefore reachable only from a torn overwrite: a header written with the new block's correct size, followed by a body mixing new and old bytes that happens to deserialize as a block shorter than claimed. Even then, a record must be hidden in the slack for anything to differ.
- That is not literally "records Core never writes" (it is Core-written header plus crash), but it is practically unreachable.
- If it ever happens, the extractor fails loudly: the hidden block is missing, so `chain_to` reports "not in the blk files". It does not produce a wrong trace.
- See n1 for the wording.

**Rev scanning.**
- `scan_undo_candidates` (`blockfiles.rs:125-160`) records every magic whose payload plus 32-byte checksum fits in the file, overlaps included.
- `RevCursors::take` accepts a candidate only if `SHA256d(prev ‖ undo)` matches *and* `connect` accepts it.
- Stale or garbage headers can no longer hide records (test `undo_scan_finds_records_a_stale_header_covers`).
- Wrong acceptance needs a SHA-256d collision.
- Parking the candidates with the cursor (n2 of round 3) is correct.
- There is a cost issue: see m2.

**Intact block copy (m2 of round 3).**
- `intact_block` (`trace.rs` ~355) requires three things of a copy: it parses, its header hashes to the block, and its merkle root matches.
- Later copies are tried in order.
- `read_block_prefix` ignores trailing bytes, as `ReadBlockFromDisk` does.
- Skipping the CVE-2012-2459 mutation check is justified: Core's `CheckBlock` rejects mutated blocks before storing them, and a torn copy cannot match the root by accident.

## 4. kv_model

- **A fresh database per input.** `kv_model.rs:44-46` removes the per-process directory before each input and after it. The directories of dead pids are swept on first use.
- **Reopen.** `check_with` splits the ops at `Reopen` and reopens between segments: up to 4 times per fuzz input, and without a limit in the unit `model_check` (6 seeds, more than 6 reopens asserted). The model expects every committed batch, buffered ones included, to survive a reopen. That is correct for RocksDB, because Buffered writes reach the OS before returning.
- **`MemoryEngine`.** `guarded` (`memory.rs:114-126`) holds one write lock for three things: the latch check, applying the batch, and the Durable barrier. `simulate_power_loss` takes the same lock, so it can fall before or after a Durable write but never between applying it and its barrier. The latch is stored under that lock as well. Fixed as asked.
- **A small gap:** see n3.

## 5. The rest (spot checks) and the §10.3 table

I checked the following resolutions against the code:

| Item | Verified |
|---|---|
| m4 | Name and version in the record. `check_record` is the only decoder, and the layout is frozen. |
| m5 | `batch_with_capacity` uses `WriteBatch::with_capacity_bytes`. |
| m7 | Exact two-sided sign test, and "range" wording. |
| m9 | Timeout, plus a background-error check that latches. |
| m10 | See section 4. |
| n1 | `disk quota exceeded` and `QuotaExceeded` map to `NoSpace`. |
| n2 | Rev candidates are parked across eviction. |
| n3 | `capacity_for`'s minimum element sizes are right: 41 per input, 9 per output, 10 per transaction, 1 per `CTxUndo`, 3 per coin. |
| n4 | Wrapping matches Core's `uint64_t`; `d ≥ 1` on the `e < 9` path. |
| n5 | Cites the tags. |
| n9 | The debug assertion on drop. |

- **D4 (§9.5):** corrected, and its rerun row is consistent with the tests above.
- **D1 overhead claim:** "background-errors" is a mutex-taking property (`need_out_of_mutex = false`), so each write takes one DB-mutex acquisition. That matches the text.
- **§9.4 and §1:** route the open items to DEC-43's cell, and OI-6/OI-7 have been added. This answers r3 M2 as far as this branch can; the PLAN change is DEC-43's.
- **The §10.3 disposition table** is accurate, with two small exceptions:
  - The m1 row says "Core never writes such records" (n1).
  - The m3 row's "`lost+found` … whatever it holds" is a deliberate choice that I'd narrow (n2).

## Findings

### Blocker
None.

### Major
None.

### Minor

**m1. The kind read back from the LOG can be wrong.**
- **Where:** `rocks.rs:95-109`, `243-269`.
- **The race:**
  - RocksDB bumps `bg_error_count_` under the DB mutex, unlocks, and only then writes the "Waiting after background … error" line (`db_impl_compaction_flush.cc:3964-3971`, `4067-4075`). For compaction it first flushes the job's buffered log lines.
  - So a write can see `errors > 0` before the line exists.
  - In the case the LOG lookup exists for, a soft compaction `NoSpace`, the probe write succeeds, `last_background_error` finds nothing, and the latched kind is `Io` with the message "the cause is in the database's LOG".
- **Last, not first:** the reverse search returns the *last* background error in the 1 MiB tail. The contract (`engine.rs:145-147`) promises the first error's kind.
- **Tail too short:** a node idle for hours after a soft error (stats dumps every 600 s) can push the line out of the 1 MiB tail.
- **Impact:** the latch is unaffected. Only A1 §3.5.6's operator message ("disk full" vs I/O error) is wrong, and only in this window.
- **Fix:**
  - Parse the `Accumulated background error counts: N` suffix and choose the line with the smallest N, which is the first error. Read the whole current LOG if necessary; it rotates only on open.
  - If no line with N ≤ `errors` exists yet, re-read for up to about 100 ms.
  - Add a non-ignored unit test of `last_background_error` against a literal 11.8.1 line, so that a format change shows up somewhere other than the ignored test.

**m2. Rev candidates make the undo pass hash attacker-chosen spans.**
- **Where:** `blockfiles.rs:125-160`, `355-373`; `trace.rs:434`.
- **The scan:** every magic followed by a length that fits becomes a candidate.
- **Planting one is cheap:** undo records contain the spent coins' scripts, and a standard 1-of-3 bare multisig output carries 2 × 32 attacker-chosen "pubkey" bytes. Once it is spent, its undo holds `magic ‖ len`, with `len` up to the rest of the rev file.
- **The cost:** each such candidate is SHA-256d'd over `len` bytes by the next `take` that crosses it. A `take` that returns `None` (trying a non-connected duplicate's rev file) rehashes every remaining candidate of that file.
- **Effect:** with thousands of planted outputs, extraction takes hours of hashing instead of minutes. This is an offline tool and the effect is a slowdown only, hence minor. It matters if `dash-coreimport` later backs A1 §2.21's import path.
- **Fix:** before hashing, apply the cheap check that the candidate's first compact size equals `block.txs.len() - 1` (the `CBlockUndo` tx count). Hash only candidates that pass it. Equivalently, call `accept`'s parse before the checksum.

### Nits

- **n1.** `blockfiles.rs:81-86` and the §10.3 m1 row say Core "never writes" records with slack. More precisely: an intact Core record has none, and slack arises only from a torn overwrite whose mixed body happens to deserialize short. The extractor then fails loudly ("not in the blk files"). Say so, so that ST2 knows where the case comes from.
- **n2.** `rocks.rs:352` ignores a `lost+found` "whatever it holds".
  - An empty or unreadable one (as mke2fs makes it) is fine.
  - A *readable, non-empty* `lost+found` next to debris and no `CURRENT` is what fsck leaves after it disconnected a damaged database's entries. Creating a fresh database there overwrites nothing; the fragments stay in `lost+found`. But it silently restarts from empty where `Corruption` would tell the operator.
  - Consider treating a readable non-empty `lost+found` as `Corruption` or `UnknownDatabase`.
- **n3.** `model.rs:220`: `check_with` reopens and then continues without comparing. WAL replay is checked only by later `Get`/`Scan` ops and the final comparison, so a replay error in a key that is overwritten later is missed. Call `model.compare(&engine)` right after each reopen. It is cheap: the model's databases are small.
- **n4.** Within the scope of this round, `check_background`'s probe write uses default `WriteOptions`. If RocksDB is in a write stall while a soft error's recovery is in progress, the probe blocks where it only wants a status. A real write would block there too, so this is cosmetic.

## Verdict

**APPROVE-WITH-NITS.**

M1 is fixed properly:
- An engine-level latch covers foreground errors (write, WAL sync, flush, atomic flush) and background errors (the DB-wide monotonic counter, which also catches errors RocksDB has auto-recovered from).
- It is held under the data lock in `MemoryEngine`.
- Both real-filesystem tests pass on a 200 MiB tmpfs.

The m3 classification has no loss path, and the blk scan matches Core's stopping rule. The remaining slack difference cannot arise from intact Core files, and if it arises at all, extraction fails loudly. m1 (the LOG kind can be wrong) and m2 (hashing cost of planted candidates) are worth fixing, but neither affects safety or ST1's results. They can be follow-ups, in ST2 for the scanner and CH2/ST3a for the engine's error reporting.
