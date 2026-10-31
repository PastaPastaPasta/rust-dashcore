# SB13 SQLite store + migrator v3 + thread sources: independent Opus-h round 1

I reviewed `node/sb13-spv-sqlite` at `7544afe1e`, SB13's own changes only (`0e4a19d45^2..7544afe1e`, 55 files; the train-14 follow-merge excluded). I worked in a detached scratch worktree with a private target and `TMPDIR` under `/work/scratch/sb13-opus-h` (mode 0700). I did not modify the branch, commit or push.

## Verdict

**ACCEPT-WITH-CHANGES.** I found no correctness or durability defect in the store, recovery, the migrator or the thread sources. One major issue must be resolved before READY-MERGE: the store write path is a large performance regression that the D-rows and the READY report don't disclose (M1).

## Findings

### M1 (major): `put_headers` costs about 10× the CPU and 25× the wall time of v2 per header, undisclosed, and PF0 will flag it

- **Where:** `dash-spv/src/storage/store/legacy.rs:101` (`id_of(hash)` per header), `:118-121` (the parent, for the first header of a call) and `:128-131` (`legacy_active_row(skip_height(h))`: an `active` read and an `hdr` read per header, because the skip height is almost never in this call's `written` map).
- **Why it costs this much:** each of these calls is a separate `KvEngine::get`/`multi_get`. In dash-kv-sqlite every such call takes a pooled reader and runs `BEGIN DEFERRED` + the pinning read + the query + rollback (`dash-kv-sqlite/src/lib.rs`, `pin`). That is about 3 pinned reads (around 12 statements) per new header before its 6 inserts.
- **Measured:** release bench `dash-spv/storage`, base `df42294e2` vs head `7544afe1e`, same box, run back to back:

  | row (perfgate) | base | head |
  |---|---|---|
  | `storage/disk/store` (`spv-storage-store`, 260 000 headers in 13 000-header calls) | 0.72 s/iter (2.48 s on the rerun at load 100) | 18.8 s/iter (69 s on the rerun) |
  | same, `perf stat -e instructions:u` over the whole bench run (warm-up + 10 samples each) | 28.5 G | 270 G |
  | `storage/disk/reverse_index` (`spv-storage-revindex`) | 19.3 µs | 133 µs |
  | `storage/disk/get` (`spv-storage-get`) | 595 µs | 122 µs |
  | `storage/disk/persist` (`spv-storage-persist`, D15 bench-input change) | 16.6 ms | 4.0 ms |

- **Failure scenario:**
  - **Process:** PF0 measures `spv-storage-store` and `spv-storage-revindex` far outside any tolerance. SB12a was dropped from train 14 for +7.1 % instructions on `spv-storage-store`. SB13 then drops at the train step for a regression that review could have caught. The D-rows pre-register only `persist` (D15); STATUS's READY line reports only "spv-startup 62 s → 0.2 s … all else SAME".
  - **Product:** a mainnet initial header sync (about 2.4 M headers through `store_headers`) pays about 70 µs of store work per header in release on agentbox, around 170 s in total, against about 7 s for v2. On a phone this is likely worse.
  - **Product:** `get_header_height_by_hash` is about 7× slower. It sits on the masternode path (`MasternodeListEngine::height_of`, `sml_engine.rs:160-172`: one blocking-pool hop per quorum hash) and on the startup replay (m1).
- **Fix:**
  1. Batch `put_headers`'s reads: one `multi_get(HDR_HASH, all hashes)`; collect the skip heights not in `written` and resolve them with one `multi_get(ACTIVE)` then one `multi_get(HDR)`; take the parent of the first header from the same batch. Alternatively, run all of the call's reads in one `snapshot()`. That takes about 3 pinned reads per call instead of about 3 per header.
  2. Re-measure the four rows.
  3. In the READY report, pre-register the rows that change by design (`revindex`, `get`, and `store` if it stays slower) and get a ruling before PF0.

### m1 (minor): D25's `spv-startup` measures the storage open, not open-to-ready

- **Where:** `dash-spv/tests/migrate_v3.rs:173-203` times `DiskStorageManager::new` plus two tip reads. The client's real startup also runs `DashSpvClient::new`, which replays the whole masternode log (`client/lifecycle.rs:69`, `load_engine`). The replay reads each message twice and resolves every base and quorum hash through `get_header_height_by_hash`, now a blocking-pool SQLite hop (M1: 7× slower than v2's in-memory index). `build_managers` and the wallet load come on top.
- **Failure scenario:** a migrated store starts with an empty `mn_log` (D6), so the first start is fast. From the second start on, a mainnet client that has run the old masternode path replays dozens of QRInfo and diff messages with hundreds of lookups each. The 0.2 s figure doesn't describe that case, and nothing measures it. The v2 → v3 comparison is still fair for the storage part, since v2 also replayed the log.
- **Fix:** either time `DashSpvClient::new` with a populated `mn_log` (a few mainnet-shaped QRInfo messages), or restate D25 as "storage open-to-tips" and leave open-to-ready to SB17's cell.

### m2 (minor): the v3 migrator's "is there any v2 data" check still uses `Path::exists`

- **Where:** `dash-spv/src/storage/migrator/v3.rs:114`.
- **Failure scenario:** D42 introduced `present()` because `Path::exists` reads any lookup error as "absent", and line 114 still uses `exists()`:
  1. A transient `stat` failure other than ENOENT (EIO on flaky flash or SD, EOVERFLOW, ENOMEM) makes every `exists()` there return false.
  2. `migrate` returns `Ok` without building, and `migration_loop` writes `version.json = 3` with the v2 folders still at the root.
  3. `SpvStore::open` creates an empty `spv/`.
  4. At the next open, `remove_v2_leftovers` (`:205-219`) sees the v2 folders and `spv/`, and deletes them unmigrated.

  This is the D39 invariant ("only folders whose data is in v3 may be deleted") broken on the main path by the same mechanism D42 fixed on the fallback. Lines 207 and 210 fail safe (skip, or set aside); line 114 does not.
- **Fix:** use `present()` at line 114 (`V2_FOLDERS.iter().map(|f| present(&storage_path.join(f))).collect::<io::Result<Vec<_>>>()?`). For consistency, also at 207 and 210.

### m3 (minor): `1fc549686 feat(dash-spv)!` doesn't say what broke, and the store's internals became public API

- **Where:**
  - `6c12b0d35` has a correct `BREAKING CHANGE:` footer covering the validators' `new(CpuPool)`, `ClientConfig::cpu_pool` and FFI `worker_threads = 0 → 2`. The brief's question is answered yes for those.
  - `1fc549686` has `!` but no footer. It removes the `PersistentStorage` impls (`open`/`persist`) from the five `Persistent*Storage` types, turns `PersistentBlockHeaderStorage::open` from async into a sync constructor with a different meaning, and removes `persist()` from the storage types.
  - `dash-spv/src/storage/mod.rs:18` makes `storage::store` public. With `store/mod.rs:30` (`pub mod schema`) and `handle.rs:49` (`SpvStore::store() -> &Store<SqliteEngine>`), that exposes the synchronous store to async callers, which D12 says must never happen ("no store call may block an async worker").
- **Failure scenario:** a downstream user upgrading over this commit finds no migration note. A host that calls `storage.store().store().headers(..)` from an async task blocks a runtime worker on SQLite and fsync-contended reads. The only in-tree uses are tests (`tests/dashd_masternode/helpers.rs:50`, `sync/masternodes/sync_manager.rs:785` under `cfg(test)`).
- **Fix:** add a `BREAKING CHANGE:` note (in the PR or squash message if history isn't rewritten), and put `SpvStore::store()` and `pub mod store` behind `test-utils` or `#[doc(hidden)]`.

### n1 (nit): D17's timings don't reproduce, and the migration is I/O-bound

`migration_memory_does_not_grow_with_the_chain` passed, but 200 000 headers took 441 s wall for about 35 s user CPU. D17 measured 37.9 s at load ~110. The memory claim holds (peak heap 3.8 MiB at both sizes, RSS 15.3 vs 10.5 MiB). The wall time depends on disk contention: WAL checkpoints at `synchronous=NORMAL` still fsync. `dash_spv_ffi_client_new` blocks for the whole migration with no progress signal. Consider stating that D17's times are machine-state dependent, and noting the FFI blocking as a follow-up.

### n2 (nit, pre-existing, not SB13's): a reorg below the lowest stored block fails `handle_fork`

`truncation` (`legacy.rs:45-53`) refuses `truncate_above(h)` below the lowest stored height, as v2's `SegmentCache::truncate_above` did. `blocks` is sparse (wallet-matched blocks), so its lowest stored height is the first matched block. A reorg forking below a new wallet's first matched block makes `BlockStorage::truncate_above` fail in `handle_fork` (`client/lifecycle.rs:298-301`, `?`). The client then never runs the new branch's sync. This is unchanged from v2; it's a candidate for the backlog.

## What I checked and found sound

1. **Crash and durability.**
   - Every write is one `KvEngine::write` under the writer lease. Commits are Buffered (`synchronous=NORMAL` in WAL) and the barrier is Durable (`FULL`). `commit` clears `buffered` on a Durable write, and `sync_wal` covers earlier frames because the WAL is sequential.
   - Recovery runs the engine barrier first, and its rewinds are Durable.
   - Lock order (lease → fence → view) holds on every path I traced. `headers`/`sub_range` take only `fence_read` → `view.read`, so there is no inversion.
   - The production write paths outside the crash Sut are `put_mn_message`, `clear_all` and `clear_sub(Buffered)`. Each is a single `WriteSet` with no locator dependency, so a lost suffix leaves the before or after state, both consistent. In-range `put_headers` overwrite without a truncation has no production caller: the checkpoint anchor goes into an empty store, and reorgs truncate first.
   - Migrator directory durability is sound. SQLite fsyncs the new directory on the WAL's first sync. The rename is followed by `sync_dir`, then `version.json` via `atomic_write`, then the leftover delete. `set_aside` makes the aside durable before the first move and syncs the aside and the root after.
   - The R6-M2 witness (`crash/tests.rs:188-238`) asserts the A70 floor, the A100 tip, no B record and the E3 rewind to A69.
2. **Migrator v3.**
   - Every v2 field is carried over: headers with hash, filter headers from `lo − 1`, filters, blocks (`hash ‖ block`) and the `metadata/*.dat` keys. The only v2 metadata key is `last_target_height`.
   - The MN log is dropped by design (D6); peers and logs stay.
   - `resume_set_aside` is the one helper both paths call, and errors propagate before any delete. `.N` collision names never overwrite.
   - On a read-only disk, the open fails and nothing is deleted. On a full disk, `SQLITE_FULL` → `Failure::Store` → the open fails, v2 stays intact and `spv.migrating/` is removed at the next open. After open, a full disk latches the engine fail-stop, and the worker logs each 5 s until a restart.
   - A missing `version.json` on a live v3 store re-runs v1 → v3 and deletes `spv/`. Only `clear` can produce that state by crashing, and `clear` wanted an empty store anyway.
3. **Thread sources.**
   - One `CpuPool` per client, passed to every set of sync managers. `CpuPool::run` resumes panics in the caller.
   - The FFI runtime is bounded at 2 workers + 2 blocking threads. There is no long-lived `spawn_blocking` task (I grepped dash-spv, dash-spv-ffi, key-wallet-manager and key-wallet-ffi), so the bound of 2 cannot deadlock. It only queues.
   - D40's attribution runs inside one pool job, and the `debug_assert!` pins its placement. What remains on async threads is the validators' `install` (documented, D38 follow-up) and the O(scripts) wallet snapshot.
   - The only rayon user in the SPV's dependencies is key-wallet-manager's `matching.rs`, which is gated on `current_thread_index()`.
4. **FFI.** The only change is the runtime builder. I found no new `unsafe` and no lifetime change. `worker_threads` 0 → 2 is documented in the header comment (D41) and in the `6c12b0d35` footer.
5. **Filter pipeline (D37).** `requested` is cleared on `init`, and a fork rebuilds the managers. `download_start ≥ header_start_height`, so `load_headers(start..=end)` can't be refused below the header range and re-queue forever.

## Tests run (all at `7544afe1e`)

| Command | Result |
|---|---|
| `dash_spv` lib `storage` (includes `storage::migrator`, the crash sweep, nested crashes, the legacy path, R6-M2) | 103 passed, 0 failed (163 s) |
| `--test migrate_v3` | 1 passed, 2 ignored (mainnet-sized); 200k: 441 s, heap 3.8 MiB, RSS 15.3 MiB; 20k: heap 3.8 MiB, RSS 10.5 MiB |
| `cargo test -p dash-spv-ffi --features blst-no-threads --test thread_count` | passed: baseline 3, peak 9, bound 9 (2 workers, 2 blocking, 2 CPU pool) |
| `dashd_sync tests_restart::{test_sync_restart_consistency, test_clear_storage_stops_running_client}` (dashd 23.1.0 via `setup-dashd.py`, `DASHD_TEST_RETAIN_DIR` under scratch) | 2 passed |
| `cargo bench -p dash-spv --bench storage`, base `df42294e2` vs head | see M1 |

There were no flakes, so nothing needed a solo rerun. The store row was run twice, with the same conclusion both times.
