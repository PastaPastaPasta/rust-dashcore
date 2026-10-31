# CH2a a3 (Store): Opus high review

**Reviewed:** `node/ch2a-coins` at exact head `e5ffe1f7a`, worktree `~/workspace/rdc-wt/ch2a-coins`. The a3 range is `19bf058d..e5ffe1f7a`, first-parent. I glanced at the two integration merges, `72f59522` and `0416f066`, for their conflict resolutions only: the `Cargo.toml` members and profiles are a union of both sides, and so are the node-fuzz bins, models and targets. They are as Sol r1 described.
- During the review the implementer pushed `d4f6fbcc9` (Sol r2's C-API helper nit) on top. Its diff lies entirely inside `dashd-store/src/rocks.rs`'s `#[cfg(test)] mod tests` (from line 745), so every production line I reviewed is identical to `e5ffe1f7a`.
- I made no branch edits, commits, pushes or GitHub comments. This report is uncommitted.

**Basis:**
- REVIEW-COMMON;
- CH2 rev5 / DEC-103: the §7.1 a3 row and every §7.1 a3 tag, §2.5, §2.5.1, §2.6, §2.10, §3.9, §4.2, §4.3 and §4.6;
- DEC-78, DEC-95, DEC-99 and DEC-102;
- Sol r1 and r2, CH2a-a12-opus-high, both implementer reports, and BL-72.

Core references are `v24.0.0-rc.3`, read with `git show`. RocksDB references are the vendored 11.8.1 sources in `librocksdb-sys-0.19.0+11.8.1`. PLAN §7: codenames only; no private inputs opened.

**Evidence** is under `/work/scratch/ch2a-a3-opus/`:
- `probes/*.c`: native probes linked against this worktree's own `librocksdb.a` (`target/debug/build/librocksdb-sys-9292f4fb6380b7ff`) through RocksDB's C API, configured as `RocksTuning` configures the engine;
- `probes/*.log`: their outputs;
- `targeted-tests.log`.

All commands used `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=` and `TMPDIR=/work/scratch/ch2a-a3-opus/tmp` (mode 700, now empty). Cargo used the worktree's own target; no `CARGO_TARGET_DIR` was set. The two small tmpfs mounts used by the probes are unmounted.

**Result:**
- One **major**: a3's chainstate RocksDB configuration turns a failed background memtable flush into an unbounded hang of every chainstate writer, where Core and ST1's latch both fail-stop.
- Two **minor**: a copy-point iterator can hold the write stall open; a cursor I/O error is an undeclared deviation.
- Four **nits**.

Sol's areas hold up under my independent checks: the cache grids, S1–S4, the guard's read paths, wipe crash-resume and the merges.

## Findings

### O1 (major): a write stalled by the write buffer manager never returns if the flush it waits for fails

**Location:**
- `dashd-store/src/rocks.rs:181-183`: the `WriteBufferManager` is created with `allow_stall = true`, as CH2 §2.5 specifies;
- `:708-712`: `RocksEngine::write` uses default `WriteOptions`, a blocking write;
- `:306-311`: `guarded` runs `check_background` only before entering RocksDB;
- CH2 §2.5, bullet "A `WriteBufferManager` of E/2 …": "Writers stall above E/2 … whose stall clears through RocksDB's own flush threads, never ours."

**Mechanism (RocksDB 11.8.1).**
1. `PreprocessWrite` checks `IsDBStopped()` once, on entry (`db_impl_write.cc:2107-2110`).
2. If `write_buffer_manager_->ShouldStall()`, the write-group leader calls `WriteBufferManagerStallWrites()` (`:2172-2181`). That call does two things:
   - `write_thread_.BeginWriteStall()`, which holds back every later writer (`:2921`);
   - `wbm_stall_->Block()`.
3. Only `WriteBufferManager::FreeMem`/`SetAllowStall`/`SetBufferSize` reach `MaybeEndWriteStall` (`write_buffer_manager.cc:97-99, 141-159`; `write_buffer_manager.h:85-96`). Memory is freed only when a flushed memtable is destroyed.
4. The background error handler never signals the stall. Contrast the write-controller stall, whose `DelayWrite` loop exits on a background error (`:2865-2867`).

So when the flush that would free the memory fails, the stalled writer waits forever. It never reaches the guard's error path, the latch never trips, and no `AbortNode` runs.

**Core:** LevelDB's `MakeRoomForWrite` rechecks `bg_error_` on every loop iteration (`leveldb/db/db_impl.cc:1328-1330`), and `BackgroundCall` signals waiters after a failure (`:684, :695`). Core's write returns the error, `FlushStateToDisk` catches it, and the node aborts with "System error while flushing" / "Disk space is too low!". ST1 M1/D4 made `RocksEngine` match this: it latches every background error, even one RocksDB recovers from. The a3 configuration reopens that hole for the chainstate engine.

**Reproduced** (`probes/wbm_bgerror*.c`, logs alongside). The setup: a 4 MiB charged WBM on a non-strict LRU, `atomic_flush`, `paranoid_checks`, `manual_wal_flush` off. Write 1 takes the memtables past the WBM. Then the next flush is made to fail, and write 2 (blocking) runs on a thread.

| Case | allow_stall | Write 2 |
|---|---|---|
| `ENOSPC`: 16 MiB tmpfs filled | 1 | **still blocked** at 15 s and at 45 s, 30 s after the space was freed; `background-errors=1` |
| same | 0 (control) | returns the I/O error at once |
| portable: DB directory read-only, `wal_dir` writable (no mount needed) | 1 | **still blocked** at 45 s, also after write permission is restored |
| same | 0 (control) | returns at once; `RocksEngine`'s `check_background` latches the next write |
| `ENOSPC`, then `set_allow_stall(false)` at 15 s | 1 → 0 | returns within 0.5 s; the next write fails with the background error |

**Concrete failure:**
1. During IBD, W writes 16 MiB coins sub-batches (step 8) faster than the disk flushes them. With S = 16 MiB and three `coins` buffers (48 MiB) against E/2 = 37 MiB, the WBM stall is routinely active, and W is stalled inside `GuardedEngine::write`.
2. The disk fills during the memtable flush. `ENOSPC` while writing an SST mid-IBD is the classic disk-full case, and step 1's ≥ 50 MiB check at flush start does not cover space consumed during the flush. An EIO would do the same.
3. W never returns from step 8, V's next effect-row write blocks behind it, and the node stops making progress silently. The only trace is RocksDB's `LOG`: no debug.log error, no `AbortNode`, no exit status.
4. With `ENOSPC` and RocksDB's default `SstFileManager`, the writes resume by themselves once enough free space returns; the next write's `check_background` then latches. So the best case is a silent hang until an operator frees space. A non-retryable error (EIO, a permission error) hangs forever, as the portable probe shows.

**Fix (any one; the first is local to `dashd-store`):**
- (a) `RocksEngine::write` passes `no_slowdown = true`. On `Incomplete("Write stall")` it runs `check_background` (latching on a background error) and retries after a short sleep. This keeps §2.5's memory bound and restores fail-stop.
- (b) A native `EventListener::OnBackgroundError` calls `set_allow_stall(false)` (proven above to release the writer) and latches. This needs a C++ shim, because rust-rocksdb 0.25 exposes no listener.
- (c) `allow_stall = false`, bounding memtables with per-CF `max_write_buffer_number` instead. The write-controller stall does wake on background errors, but this changes §2.5's memory model, which is SB0/CH2c's call.

Whichever is chosen, add a portable regression test to `dashd-store`: the read-only DB directory with a writable `wal_dir` (this needs a `wal_dir` knob in `RocksConfig`), or the existing `DASHD_STORE_SMALL_FS` harness. A blocking write stalled on the WBM must return an error within a bounded time after the flush fails, and the engine must then refuse writes. Also correct §2.5's "stall clears through RocksDB's own flush threads" sentence.

**Scope:**
- The defect is in a3's shipped configuration and write path, and every writer reaches it once the WBM stalls.
- No a3 test can see it: the coins model's engines run at `-dbcache=4` shares (WBM ≈ 33 MiB) with writes of a few KB.
- If the manager rules the fix to b1 instead, BL-72 must carry it as a hard obligation with the named test. In that case the remaining a3 changes would be APPROVE-WITH-NITS.

### O2 (minor, CH2b/CH5 obligation): a copy point's iterator can hold the WBM stall open for its whole scan

**Location:** CH2 §2.6, "Copy points": W takes the `GuardedSnapshot` between Bk's return and `FlushDone`; step 11's `after_bk` flush comes after. In code: `RocksSnapshot::iter_prefix`, `rocks.rs:676-685`.

A RocksDB iterator pins the memtables of the SuperVersion it was created from. Their WBM charge is released only in `~MemTable`, so it outlives the flush. A snapshot alone pins nothing, and `MultiGet` holds its reference only for the call, so CH4's prefetch is unaffected.

**Reproduced** (`probes/wbm_iter_pin.c`, 4 MiB WBM):
- **Cursor created before `after_bk`'s flush:** the WBM usage stays at 4,198,400 B after the flush. The next blocking ~1 MiB write is **still blocked 10 s later**, and a no-wait write gets "Write stall". Both clear the moment the cursor is dropped.
- **Cursor created after the flush:** the write goes through at once.

**Failure scenario:**
1. `gettxoutsetinfo` or `dumptxoutset` creates its cursor from the handed-over snapshot before W's `after_bk` flush completes, with Bk's memtables near the WBM's 7/8 trigger.
2. For the minutes-long mainnet scan, every chainstate write stalls once usage reaches E/2: V's effect rows and W's next B1.
3. If the scanning thread ever waits on V (a `ChainCommand::Query` for block-index data, an interruption check routed through V), the system deadlocks. Otherwise block processing stalls for the scan's duration, which Core avoids by releasing `cs_main` after cursor creation.

**Add to BL-72 (CH2b/CH5):**
- create copy-point cursors only after `after_bk`'s flush has returned, or run `after_bk` before handing the snapshot over;
- never let a scan's thread wait on V or W while it holds an iterator;
- optionally re-create the iterator from the same snapshot every few MiB (seek past the last key) to bound how long a mutable memtable stays pinned. A cursor created after the flush still pins the then-mutable memtables as they grow; in principle that is up to S + 10 × 4 MiB = 56 MiB > E/2.

### O3 (minor, documentation): a cursor I/O error aborts, where Core's cursor ends silently; the deviation is undeclared

**Location:** `coins/table.rs:99-118` and `GuardedSnapshot::scan` (`guarded.rs:376-383`) send an engine error during a scan to `read_error`, which latches and calls `read_failed`, which aborts. The deviation table in CH2 §3.9, row D7, declares decode failures only.

**Core:**
- `CCoinsViewErrorCatcher` (`coins.cpp:355-379`) wraps only `GetCoin`/`HaveCoin`.
- A LevelDB iterator error, such as a checksum mismatch under Core's `verify_checksums`, makes `pcursor->Valid()` false. `CCoinsViewDBCursor::Next` (`txdb.cpp:263-273`) then ends the scan.
- `ComputeUTXOStats` (`kernel/coinstats.cpp:125-143`) returns success with partial totals.

So on a damaged SST, `gettxoutsetinfo` in Core returns wrong totals, while ours aborts the node from an RPC. That is the approved O-R2-m9 design, and it is fail-safe in DEC-78's spirit, so no code change is needed. But it is observable, so §3.9 should state it, as a D7 extension or a new row. `size_estimate` errors abort too, where Core's `EstimateSize` cannot fail; that path is practically unreachable, so the same note can cover it.

### O4 (nit): `CoreAbortSink`'s `log` closure must write through before it returns

**Location:** `store/abort.rs:23-24`, the doc of `log`.

Core's logger file is unbuffered (`logging.cpp:61, :505`), so both read-error lines are on disk when `std::abort()` runs. A node that supplies a buffered or non-blocking appender (for example, `tracing`'s `non_blocking`) loses both lines at the abort. State that requirement in the constructor's doc.

### O5 (nit): the wipe test's keys miss the lower edge of the range

**Location:** `store/meta.rs` `fill` (the wipe test helper): each CF gets only `b"k"` and `[0xff; 40]`.

A start bound mutated from `[]` to `[0x01]` would leave every key beginning with 0x00 (about 1 in 256 txids) and still pass. Add `[]`, `[0x00]` and `[0xff; 37]` keys. The upper bound is already discriminated.

### O6 (nit): `log_lines` and `calculate` take the filter set separately

**Location:** `caches.rs:63, :117`.

`calculate(.., n_filter)` and `log_lines(.., filters: &[&str], ..)` can disagree; Core derives both from `g_enabled_filter_types` (`init.cpp:1984, 2000-2003`). Derive `n_filter` from `filters.len()`, or pass one value.

Also, Core logs the "chain state database" line, then validates `-maxmempool` (an `InitError` can occur there), and only then logs the UTXO line (`init.cpp:2004-2023`). `log_lines` returns all the lines at once, so the caller must emit the prefix before that check to match Core's debug.log on that error path.

### O7 (nit, for CH5): `-reindex-chainstate` wipes more than the chainstate in Core

Core also wipes the InstantSend DB (`init.cpp:2043`), the LLMQ context DBs (`node/chainstate.cpp:158-160`, `.wipe = to_wipe_data`) and the masternode-side DBs built from `dash_db_params` (`init.cpp:2165`). `wipe_chainstate` correctly covers only the chainstate schema; CH5 must wipe the other stores' equivalents. Suggest adding this to BL-72's CH5 caveat.

## Focus areas: what I checked and found sound

**1. Cache split and log lines.**
- `calculate_capped` is `node/caches.cpp:15-42` line for line:
  - the sequential deductions;
  - `timestamp_index` at `/64`;
  - the filter `/8` capped at 1 GiB, then divided by n, with n × share deducted;
  - `coins_db = min(rem/2, rem/4 + 8 MiB, 8 MiB)`;
  - the remainder to `coins`.
- `wrapping_shl(20)` reproduces C++20's modular `int64_t << 20` across the i64 grid. `usize::BITS > 32` equals `sizeof(void*) > 4` on every Rust target.
- The log strings match `init.cpp:1986-2023`, including `%.1f` versus `{:.1}`: both round the exact binary value half-to-even, and the 37.25 → "37.2" case is in the dashd fixture.
- Downstream:
  - E = `block_tree_db + coins_db + 64 MiB` is Core's two LevelDB caches plus EvoDB's (`evo/evodb.cpp:38`). LevelDB's n/2 + 2·n/4 split totals n, so E is the right total.
  - `coins` becomes `CoinsBudget::coins_share`, Core's `m_total_coinstip_cache` (`node/chainstate.cpp:330`). Core's temporary 20 % init sizing before `MaybeRebalanceCaches` (`:222-256, :293`) affects flush timing only (D1).
- I found no edge-flag or downstream defect.

**2. The fail-stop latch as a system.**
- Reads: point, `multi_get`, snapshot point and multi, scan, `size_estimate`, and the meta decode all reach `read_error`/`corrupt`.
- Writes and barriers (`write`, `sync_wal`, `flush_memtables`, `after_bk`, a `delete_range` batch) go through `mutate`. `GuardedFiles::mutate` checks the latch under its lock.
- `memory_report`'s unguarded property read is acceptable as a diagnostics-only exception: it reads no coin, forms no verdict and writes nothing, and Core has no analogue.
- Concurrent writers:
  - `GuardedEngine` and ST1's `FailStop` both refuse writes that *start* after a trip. A write already inside the engine may land after a peer's failure, which is ST1's documented `KvEngine` contract.
  - The epoch counts only writes that returned Ok. Reading it before the snapshot makes it a lower bound (O5).
- `CoreAbortSink` matches Core's order:
  1. `noui_ThreadSafeMessageBox` logs and prints `"Error: " + message.original` (`noui.cpp:22-47`);
  2. `ExecuteBackedWrapper` logs "Error reading from database: %s";
  3. `std::abort()`.
- S1's guard makes the abort unconditional; Sol r2's 28 SIGABRT cases cover it.
- The system-level gaps are O1 (a background error is invisible to a stalled writer) and O3.

**3. `wipe_chainstate`.**
- Crash-resume: each of the three Durable batches is atomic. After the first one survives, `wiping=1` and no tips remain, and repeating the delete batch is idempotent. Sol's 48 schedules and the committed test agree.
- Separation from Core's `-reindex` flag: what the wipe keeps (`last_file`, `pruned_files`, `reindexing`, `block_index`, `block_files`) is exactly what Core keeps in the block-tree DB on `-reindex-chainstate`. What it erases (`tip`, `head_blocks`, the coins and `evo_tip` plus the evo CFs) is the coins DB and EvoDB, which Core destroys (`node/chainstate.cpp:153-160, 233-236`; `init.cpp:2041`). `reindexing` is untouched.
- Range completeness: `[ε, 0xff × 1024)` covers every schema-v1 key.
- Acceptable for a3: `delete_range` leaves the SST bytes and a range tombstone until bottommost compaction. That is a disk-size and transient read-cost issue, not a semantic one. BL-72's CH5 caveat should add an explicit `compact_range` on the wiped CFs after the wipe. That reclaims the space and drops the tombstones before the reindex repopulates millions of keys.

**4. RocksDB configuration for a real node.**
- O1 and O2 above.
- `manual_wal_flush = false` is right for ST3a item 12, and it is asserted from `OPTIONS`.
- `atomic_flush`, `PointInTime` and `paranoid_checks` are unchanged from ST1.
- The per-CF buffers are coins S × 3 and 4 MiB × 2 elsewhere. `max_total_wal_size = E` acts as a flush trigger, as designed.
- Deadlock analysis for CH2b's flush protocol: W never waits on a writer that waits on W.
  - The WBM stall is released by RocksDB's own flush thread, and `sync_wal` does not enter the write queue.
  - `flush_memtables`' `EnterUnbatched` waits behind a stalled leader, but that leader is released by the background flush.
  - The two ways out of this are a failed background flush (O1) and memtables pinned by a holder that waits on V or W (O2).

**5. Test quality.**
- The a3 tests discriminate what they claim:
  - the grids for both widths;
  - S3's strict cache versus the non-strict one, and the stall versus `allow_stall=false`;
  - S4's live read versus the snapshot read;
  - the epoch order, R ≥ the sub-batch peak, and `after_bk` counted and refused after a trip;
  - `delete_range` on both engines with batch order;
  - wipe crash-resume.
- The model's runs over `RocksTuning` prove that the two engines agree logically. They run at `-dbcache=4` shares (E ≈ 66 MiB, WBM ≈ 33 MiB) with writes of a few KB, so the configuration's pressure paths go unexercised: stall, cache overflow and background error. O1 is a realistic divergence that passes every a3 test. CH2c's `engine-memory` workload, plus the O1 regression test, should cover that class.
- O5 is a smaller gap of the same kind.

**Tests run (at `d4f6fbcc9`, whose production code is identical to `e5ffe1f7a`):** `cargo test --locked -p dashd-chainstate -p dashd-store --all-features --lib -- --test-threads=4`: **116 passed** (chainstate) and **54 passed, 1 ignored** (store), exit 0.

## Verdict

O1 must be fixed (or explicitly ruled into b1 with a named regression test in BL-72) before this configuration backs a real writer. O2 and O7 are BL-72 additions. O3 is a §3.9 doc fix. O4–O6 are nits.

**Verdict:** CHANGES-REQUIRED
