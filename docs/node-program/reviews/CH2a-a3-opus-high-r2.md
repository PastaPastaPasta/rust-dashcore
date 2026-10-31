# CH2a a3 (Store): Opus high narrow re-check of O1 / DEC-164 (r2)

**Reviewed:** `node/ch2a-coins` at exact head `5ef7ff6d2`, worktree `~/workspace/rdc-wt/ch2a-coins`. The range is `d4f6fbcc9..5ef7ff6d2`: `7bc0a1a77` (the O1 fix), `319670546` (O6), `ed5319ffe` (O5), `de23c6e76` (O4) and `5ef7ff6d2` (docs, DEC-164 in CH2 §2.5, D15).
- **Scope:** only O1/DEC-164 and the nits O3–O6 from `CH2a-a3-opus-high.md`.
- **Inputs:** the implementer's report and logs in `/work/scratch/ch2a/r3/`, and DEC-164 in `DECISIONS.md`.
- **Read-only:** no branch edits, commits, pushes or GitHub comments. This file is uncommitted.

**Basis:**
- **RocksDB:** the vendored 11.8.1 sources in `librocksdb-sys-0.19.0+11.8.1`.
- **Core:** `v24.0.0-rc.3`, read with `git show`.
- **PLAN §7:** codenames only; no private inputs opened.

**Environment:**
- `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and `TMPDIR=/work/scratch/ch2a-a3-o1-recheck/tmp` (mode 700, now empty).
- Builds used the worktree's own target; no `CARGO_TARGET_DIR` was set.
- Every tmpfs mount was unmounted afterwards.
- The machine's load average was about 196 throughout.

**Evidence** is under `/work/scratch/ch2a-a3-o1-recheck/`:
- `probes/wbm_dec164.c`, with logs `probes/dec164_{ro,enospc}.log`;
- `probes/soft_during_stall.c`, with log `probes/soft_during_stall.log`;
- `tests.log`.

Both probes link the `librocksdb.a` that this head's `dashd-store --all-features` tests link: `target/debug/build/librocksdb-sys-73eacf8c80b1dc4a`, as confirmed from cargo's `build-script-executed` message.

**Result:** O1 is fixed, and DEC-164 holds for every write, flush and barrier path that CH2a uses. I found no blocker and no major, and three nits.

## 1. The original probes, re-run against this head

`wbm_dec164.c` is my original `wbm_bgerror*.c` with two changes:
- **Write 2** is issued exactly as `RocksEngine::write_batch` issues it:
  - `no_slowdown`;
  - on exactly `"Result incomplete: Write stall"`, read `rocksdb.background-errors`; if it is above 0, write an empty `no_slowdown` batch and return its non-stall error, or else the error from the property;
  - otherwise pause 1 ms, doubling to 64 ms, and retry the same batch.
- **The column families** are configured as `cf_options` now does: L0 slowdown = stop = 36, soft pending-compaction limit 0, and 3 memtables.

Both probes then call `rocksdb_flush_cfs` with `allow_write_stall`, as `flush_memtables` does.

| Case (allow_stall = 1, 4 MiB WBM) | At `e5ffe1f7a` (blocking write) | At `5ef7ff6d2` (the DEC-164 loop) |
|---|---|---|
| DB directory read-only, `wal_dir` writable | still blocked at 45 s, and after the permission was restored | returns `IO error: … 000009.sst: Permission denied` at **0.89 s**, after 8 attempts (7 refused as stalls). The flush afterwards returns the same error in 0.00 s |
| `ENOSPC`: a 16 MiB tmpfs filled | still blocked at 15 s, and 30 s after the space was freed | returns `IO error: No space left on device: … 000009.sst` at **1.25 s**, after 11 attempts. The flush afterwards returns the same error in 0.00 s |

**The latch** is engine state, so I checked it on the engine itself (`tests.log`):
- `fail_stop::a_stalled_write_fails_when_its_flush_fails` is my read-only-dir case through `RocksEngine`. It passes: kind `Io`, `failed() == Some(Io)`, and every later write and barrier is refused.
- With a 200 MiB tmpfs in `DASHD_STORE_SMALL_FS`, all four `fail_stop` tests pass (`--include-ignored`, 36.8 s), including both `ENOSPC` cases through the new write path.

No engine-level test combines `ENOSPC` with a *WBM-held* write. The probe shows that RocksDB returns the hard flush error to the retried write, which takes the same engine path as the read-only case. So I don't ask for one.

**Mutation evidence:** I read the implementer's logs (`mutants*.log`). Mutation (a), a plain blocking write, fails the portable test at its 60 s bound (`fail_stop.rs:118`), and (b)–(f) fail as reported.

**Point 1: confirmed.**

## 2. No write or flush path can still wait unobserved

**Write path** (`PreprocessWrite`, `db_impl_write.cc:2101-2185`). Every `RocksEngine` write goes through `write_once` with `no_slowdown` (`rocks.rs:352-369`). That includes `check_background`'s empty batch (`:316`) and every `KvEngine::write` (`:882-885`, via `guarded` → `write_batch` → `retry_stalled`). The places where the write path can wait:
- **`IsDBStopped`** (`:2107-2110`): returns the background error at once. The retried write returns it, and `guarded` latches it. This is how both probes end.
- **The WBM stall** (`:2172-2181`): with `no_slowdown`, `Incomplete("Write stall")`. `BeginWriteStall` and `wbm_stall_->Block()` run only for a blocking writer, so they are now unreachable. `WriteBufferManager::ShouldStall` then reduces to `usage >= buffer_size`, because `stall_active_` is never set. The memory bound is unchanged (`a_stalled_write_waits_until_memory_is_freed` passes).
- **`DelayWrite`** (`:2814-2905`):
  - A **delay** returns "Write stall" only after `GetDelay` has charged it (`:2827-2836`). That is the livelock the triggers rule out (next item).
  - A **stop** (`:2867-2873`) returns "Write stall" while there is no background error or recovery is in progress; `retry_stalled`'s check then sees the error count.
  - With a background error and no recovery, the loop is skipped, and `:2891-2898` return `Incomplete(<bg error>)` or the error itself. Neither is `WRITE_STALL`, so `retry_stalled` maps it through `status_error` and `guarded` latches it.
- **`SwitchWAL`, `HandleWriteBufferManagerFlush` and `ScheduleFlushes`** run before the stall check on each attempt:
  - `WaitForPendingWrites` returns at once without pipelined or unordered writes, and we use neither.
  - `SwitchWAL` returns early while the oldest log's flush is in progress (`getting_flushed`), so retries don't pile up memtable switches.
  - A WBM flush switches only non-empty memtables.
- **WAL sync** (`need_wal_sync`, and `SyncWalImpl`'s `wal_sync_cv_` at `db_impl.cc:2058-2060`): these wait only for another thread's WAL `fsync`. That is either a `Durable` write, `sync_wal`, or a background flush's closed-WAL sync, all I/O-bound and signalled on success or failure. Core's `fsync` can block the same way, so this is not a DEC-164 wait.

**Write-controller tokens under the new triggers.** I grepped every `GetDelayToken`/`GetStopToken` in 11.8.1:
- **Delay tokens:** only `SetupDelay` (`column_family.cc:933`), reached from `RecalculateWriteStallConditions` for a `kDelayed` condition. `GetWriteStallConditionAndCause` (`:1010-1049`) has three delay arms:
  - **memtable:** needs `max_write_buffer_number > 3`, which the assert at `rocks.rs:572` rules out;
  - **L0:** `>= level0_slowdown_writes_trigger`, but the stop arm with the same 36 is tested first;
  - **pending bytes:** `soft > 0 && >= soft`, with soft sanitized to hard (`column_family.cc:362-364`), which the stop arm tests first. The hard limit is RocksDB's 256 GiB default, never 0, so the sanitized soft limit is never 0 by accident.
  - The remaining tokens in that function (`:1160-1192`) are compaction-pressure tokens. They speed up compactions and never delay writes.
- **Stop tokens:** the three stop arms, plus `LockWAL` (`db_impl.cc:2215`). CH2a never calls `LockWAL`.

**The 3-memtable bound, including column families created later.**
- `cf_options` asserts `buffers <= 3` on the resolved value, whether from `cf_write_buffers` or the default (`rocks.rs:557-572`).
- Every schema CF is opened through `cf_options`, `RocksEngine` has no `create_cf`, and no workspace caller sets `max_write_buffers` or `cf_write_buffers` above 3. `tuning.rs:169` gives `coins` 3; the bake-off, node-fuzz and `probe.rs` use the default of 2.
- RocksDB's implicit `default` CF keeps its own options (2 buffers, L0 20/36). `inspect_existing` refuses a non-empty `default` (`rocks.rs:545`), and the engine never writes it, so it never has L0 files or immutable memtables. Its triggers can never fire.
- The tuning OPTIONS test asserts both triggers at 36 and soft = hard per CF, read from the sanitized OPTIONS file. It passed here (6/6, `tests.log`).

**`flush_memtables`** (`rocks.rs:897-913`): `rocksdb_flush_cfs` → `DBImpl::Flush(opts, cfhs)` → `AtomicFlushMemTables` (`db_impl_compaction_flush.cc:2872-3005`).
- With `allow_write_stall`, `WaitUntilFlushWouldNotStallWrites` is skipped (`:2906`).
- `EnterUnbatched` (`:2946`) waits only for the current write-group leader. No writer can be parked in RocksDB's queue now, because each refused attempt leaves it.
- `WaitForFlushMemTables` (`:3183-3250`) ends on `IsDBStopped`, on a recovery error, or on a soft error with `IsBGWorkStopped`. The remaining case is a soft flush error that leaves background work running. Which errors become soft (`error_handler.cc:20-60, :467-528`):
  - A flush error is soft only when it is retryable without a WAL (`kFlushNoWAL`). We always write the WAL. That case also sets `soft_error_no_bg_work_`, which ends the wait.
  - Flush `NoSpace` is hard.
  - Posix marks only `ENOSPC` retryable (`env/io_posix.cc:70-87`).
- So no flush error leaves this wait unbounded. Both probes measured the flush after a failure at 0.00 s.

**Other engine calls CH2a makes:**
- **`sync_wal`:** `FlushWAL(true)` → `SyncWAL`, the WAL-sync wait above only.
- **`snapshot`, iterators, `get_pinned_cf`, `batched_multi_get_cf`:** no waits.
- **Property reads** (`check_background`, `memory_usage`, `approximate_size`): these take `mutex_` briefly.
- **`wait_for_compactions`:** polls with `check_background` and a timeout.
- **`compact_range` and other manual flushes:** no production caller. `raw()` is used only by a test property read (`fail_stop.rs:232`), and its doc now warns that writes through it can wait for ever.
- **Close:** it does not flush, because `has_unpersisted_data_` is set only for `disableWAL` writes. Close is outside DEC-164 in any case.

**The background-error signal covers soft errors too.** `rocksdb.background-errors` is `bg_error_count_`. It is bumped on every failed background flush and compaction, whatever the severity (`db_impl_compaction_flush.cc:3957-3966, 4061-4068`). A flush's own retry under error recovery (`kErrorRecovery`) is not counted, but the error that started the recovery already was. So `check_background` sees soft compaction `NoSpace`, the one soft background error posix can produce here.

**Point 2: confirmed.**

## 3. Divergence from Core's triggers

Core's LevelDB slows each write by 1 ms at 8 L0 files and stops at 12. RocksDB's defaults, which a3 had, slow at 20 and stop at 36. This head stops at 36 with no slowdown.

**Correctness is unaffected:**
- Every stop is a wait in our loop, which observes the background error and the latch every ≤ 64 ms.
- Core's wait at its stop trigger is the same kind of wait: `MakeRoomForWrite` waits on `background_work_finished_signal_` and re-checks `bg_error_`.
- The coins model and S-tests are unaffected. No result depends on the timing of a write.

**Performance risks** (CH2c/PF3 items, not blockers):
- **L0 read amplification up to 36 files.** The chainstate profile does not pin L0 filters (`pin_l0_filter_and_index_blocks_in_cache = false`), so up to 36 files' filter partitions compete for the shared cache E during coin misses. CH2c's `engine-memory` and W-MC workloads should report L0 count and miss latency at peak.
- **Hard stops instead of a throttle.** IBD writers now run at full speed into a stop, which shows as larger, rarer V and W stall spikes, plus up to 64 ms of polling at each stall's end. §2.5 already says CH2c times stalls around `write()`.
- **PF3's tuning space shrinks.** More than 3 write buffers per CF, or any slowdown trigger below the stop, is now ruled out by DEC-164 itself. A PF3 change that wants either needs a different no-wait mechanism, for example a write-controller-aware backoff that doesn't charge `GetDelay`. The assert makes that visible at once. Suggest adding this to PF3's notes.

**Point 3: no correctness-relevant divergence.**

## 4. FFI soundness

- **`Raw<T>`** (`rocks.rs:652-663`):
  - each object is destroyed exactly once, by the destructor that matches its creator;
  - locals drop in reverse order, so the options are destroyed after the call that uses them.
- **`Send` for the write batch:** sound. A `WriteBatch` is a `std::string` plus counters, with no thread affinity. `RocksBatch` is consumed by value in `write`, so a batch has one user at a time. It is not `Sync`, and nothing needs it to be.
- **Batch mutation through `&Raw`:** RocksDB may set the sequence number in the batch during a successful group write. That memory is C++-owned and reached through a raw pointer, so no Rust aliasing rule applies.
- **A refused write leaves the batch untouched.** `PreprocessWrite` fails before `SetSequence`, the WAL append and the memtable insert. A follower in a refused group gets the leader's status, and its batch was never applied. Retrying the same batch is therefore exact.
- **Signatures match `c.cc`:**
  - `rocksdb_writebatch_create_with_params(reserved, 0, 0, 0)` matches rust-rocksdb's `with_capacity_bytes`;
  - `rocksdb_writebatch_count` returns `int`, from a `uint32` count; the `expect` fires only past 2³¹ operations;
  - `rocksdb_writebatch_data` sets `size` to `GetDataSize()`, as rust-rocksdb's `size_in_bytes` does;
  - `rocksdb_writebatch_{put,delete,delete_range}_cf` take pointer and length, and RocksDB copies the bytes. An empty slice's dangling-but-non-null pointer is never dereferenced at length 0; O5's `[]` key exercises this;
  - `rocksdb_flush_cfs` takes `(db, opts, handle**, int, err**)` and only reads the handle array;
  - `take_error` frees the `strdup`'d string once, with `rocksdb_free`.
- **Handles:**
  - `as_raw_ptr()` comes from rust-rocksdb's `raw-ptr` feature (`raw_ptr.rs:44-50`), and it is called inside `unsafe`;
  - column-family handles stay valid while `CfHandles::_db` is alive;
  - `write` asserts the batch belongs to this engine (`Arc::ptr_eq`).
- **`status_error`** reproduces the old `engine_error` mapping: `Status::ToString` prefixes are `IO error` and `Corruption`, and `kind_from_message` still runs first.

**Point 4: sound.**

## 5. The stated gap

**The implementer's wording is imprecise.** Replacing the whole check closure with `|| Ok(())` *is* caught: `a_stalled_write_ends_at_another_writes_failure` then waits past its 10 s bound, because the latch check is gone too. The uncovered mutation is narrower: removing only `self.check_background()?` from the closure in `write_batch` (`rocks.rs:341-347`) while keeping the latch check. A fatal or hard error still reaches the retried write through `IsDBStopped`. Only a soft error, which in practice means compaction `ENOSPC`, needs the check.

**Is a cheap engine-level test possible?** I tried one in C (`probes/soft_during_stall.c`, 9 runs):
- **Setup:** DB on a 16 MiB tmpfs and the WAL elsewhere. Three 1 MiB L0 files, then a 2 MiB memtable held by an iterator, so the WBM stall stays active after its flush. The filesystem is then filled to leave 2.5 MiB: room for the flush, not for the 5 MiB L0→L1 compaction.
- **With the check:**
  - in 1 of 3 runs the compaction failed with `NoSpace`; the held write ended in 0.6 s through the check alone, while the DB was not stopped and the pin was still held;
  - in 2 of 3 runs the compaction happened to fit, and the write correctly waited the full 20 s for the pin.
- **Without the check** (3 of 3 runs): a later flush also hit `ENOSPC`, a hard error. The retried write returned it at 1–2.6 s, so the mutation was not discriminated.

**Conclusion:**
- A soft-only error during a stall is hard to produce deterministically without a fault-injecting `FileSystem`. rust-rocksdb 0.25 doesn't expose one, and a C++ shim would be expensive.
- In practice the same full disk soon fails a flush too, so the hard path ends the wait anyway.
- The soft case on its own already reaches the next write's `guarded` pre-check, which runs `check_background` before every write.
- **The gap is acceptable as stated.** One cheap structural guard (optional, nit N2): have the loop and `guarded` call one function for "latch, then background" checks, say `self.refuse_if_failed()`. Then the small-FS test `refuses_writes_after_a_background_error_rocksdb_recovered_from`, which already discriminates `check_background` in `guarded`, covers the loop's check too.

## 6. The nits O3–O6

- **O3 (D15):** present in §3.9 and accurate. I re-checked the Core references at `v24.0.0-rc.3`:
  - `CCoinsViewDBCursor::Next` ends on `!Valid()` (`txdb.cpp:265-274`);
  - `ComputeUTXOStats` stops at `!pcursor->Valid()` and returns true, failing only on a `GetKey`/`GetValue` decode failure, which is D7 (`kernel/coinstats.cpp:125-143`);
  - `ExecuteBackedWrapper` is at `coins.cpp:355` and following;
  - `EstimateSize` returns `GetApproximateSizes` without a status (`dbwrapper.h:394-407`).
- **O4:** `CoreAbortSink::new`'s doc states the write-through requirement. `logging.cpp:61` and `:505` are the two `setbuf(…, nullptr)` lines. As I asked.
- **O5:** `FILL_KEYS` adds `[]`, `[0x00]` and `[0xff; 37]` in every CF, and the kept CFs count `FILL_KEYS.len()`. `a_crashed_wipe_resumes` passes, and the implementer's `[0x01]` start-bound mutation fails it. As I asked.
- **O6:**
  - `IndexArgs::filters` feeds both `calculate` (n = `filters.len()`) and `log_lines`;
  - the grid tests use `FILTERS[..n_filter]`, and the log fixture sets `args.filters`;
  - `log_lines`' doc names the `-maxmempool` order;
  - `caches` 4/4 and `tuning` 6/6 pass.
  - As I asked.

## Findings

### N1 (nit, docs): the old §2.5 sentence still says the stall "clears through RocksDB's own flush threads, never ours"

**Location:** `docs/node-program/designs/CH2-coins-flush.md:886`. The new DEC-164 bullet directly below it explains the failure case, so the pair is not wrong. Read alone, though, the old sentence is what O1 disproved. **Suggested fix:** end it with "… never ours; a stalled write waits in `RocksEngine::write`'s loop (DEC-164, below)."

### N2 (nit, test structure): the loop's `check_background` call is the one unit not covered at engine level

**Location:** `dashd-store/src/rocks.rs:341-347`. See §5. **Suggested fix:** optionally share one check function between `guarded` and the loop. Also correct the gap statement: replacing the whole closure is caught; removing only `check_background` is not. No new test is needed.

### N3 (nit, PF3 note): DEC-164 constrains PF3's RocksDB tuning space

**Location:** CH2 §2.5's last DEC-164 sub-bullet, and the `rocks.rs:572` assert. See §3. **Suggested fix:** say in §2.5 or in PF3's backlog that more than 3 buffers per CF, or any slowdown trigger below its stop, needs a new no-wait design. A delay charged by `GetDelay` would livelock the retry.

## Tests run (at `5ef7ff6d2`)

- `cargo test --locked -p dashd-store --all-features --lib -- --test-threads=4`: **58 passed, 1 ignored** (167.6 s under load ~196). These include the five new `rocks::tests`.
- `cargo test --locked -p dashd-store --all-features --test fail_stop -- --include-ignored --test-threads=1`, with `DASHD_STORE_SMALL_FS` on a 200 MiB tmpfs: **4 passed**.
- `cargo test --locked -p dashd-chainstate --all-features --lib -- tuning | caches | store::`: **6 + 4 + 15 passed**, including `the_chainstate_config_is_in_its_options_file` and `a_crashed_wipe_resumes`.
- Probes: `dec164_ro.log` and `dec164_enospc.log` (§1); `soft_during_stall.log` (§5).

## Verdict

O1 is fixed. The fix is DEC-164 as ruled:
- No `RocksEngine` write, flush or barrier can wait inside RocksDB without observing the background error and the latch.
- Both of my original scenarios now end with the flush's error, latched, in about 1 s.
- The memory bound of `allow_stall` is kept.
- The trigger changes are correctness-neutral; their performance cost is for CH2c/PF3.
- The FFI is sound, and O3–O6 are as I asked.

The three nits are optional.

**Verdict:** APPROVE-WITH-NITS
