# SB13 SQLite store review, round 2 (Sol)

Reviewed `node/sb13-spv-sqlite` at `f87b69b8b` in a detached scratch worktree. The branch under review was not changed.

## Finding

### Blocker: the corruption fallback deletes the unmoved half of an interrupted set-aside

`dash-spv/src/storage/migrator/mod.rs:89-110`; `dash-spv/src/storage/migrator/v3.rs:109-115`.

When `v2-unmigrated/` exists but only some v2 folders have moved, an unreadable `version.json` makes `migration_loop` fail before V3 can resume `set_aside`. The fallback keeps the aside directory but removes every v2 folder still at the storage root. It then reports successful migration. This defeats D39's preservation of the original v2 data in the corruption-fallback path.

I reproduced it through public `StorageMigrator::migrate`: write the v2 fixture, create `v2-unmigrated/`, move `block_headers/` into it, corrupt `version.json`, then migrate. Of seven original v2 files, only the header segment remained. The filter headers, filters, blocks, metadata and masternode file were deleted. The existing fallback test starts with a *completed* set-aside, so it misses this state. If an aside marker exists, complete the set-aside of the remaining root folders before clearing corrupted storage, and cover this combination through the public method.

## Round 1 items

- **Interrupted set-aside:** The ordinary crash or I/O interruption is fixed. The 64-subset test includes my round 1 reproduction (headers moved, other folders still at root) and passed. The corruption-fallback combination above remains a data-loss path.
- **Filter attribution on the runtime thread:** Fixed. `attribute_matches` performs union matching and per-wallet `match_any` inside one `CpuPool::run` closure (`dash-spv/src/sync/filters/manager.rs:49-98,1034-1043`). No filter matching remains in `scan_batch` on the task thread. The rescan path also submits matching to the pool. The pool await does not block a current-thread runtime; its one-thread and panic test passed. A canceled await can leave its bounded job running to completion, but I found no wait cycle or persistent starvation path in the manager task.

## Other checks

- **D39 restart states:** An empty aside marker, every subset of six interrupted moves, an EACCES interruption, and a missing `spv/` with version 3 passed the existing public-path tests. My temporary public-path tests also passed for two `.N` name collisions and for partial `spv/` and `spv.migrating/` directories before the version-3 write. Before the aside creation is synced, no folder has moved; a crash leaves either an intact v2 layout or an empty marker. `clear_storage` is explicitly a full persisted-storage wipe; its clear test passed.
- **B1 durability:** On Linux, `sync_dir` still opens and syncs the directory. It syncs the storage parent after creating the aside, both the aside and source parents after folder moves, and the parent after renaming `spv.migrating/` to `spv/` (`dash-spv/src/storage/migrator/v3.rs:119-123,139-179`). The later `version.json` atomic write syncs its parent. I found no unsynced rename in this sequence. The non-Unix branch skips directory sync as intended for Windows.
- **Merge `0e4a19d45`:** `git show --remerge-diff` shows only the expected resolution deleting the obsolete `dash-spv/src/storage/segments.rs`; I found no unrelated merge edit in the reviewed storage and filter paths.
- **D41 and remaining fix commits:** The FFI `worker_threads = 0` comment matches the two-worker default. No other regression found in the narrow diff.

## Validation

All tests used `TMPDIR=/work/scratch/sb13-r2-sol/tmp` (mode 0700) and that worktree's own `CARGO_TARGET_DIR`. The first compiler-cache attempt stalled; the direct-`rustc` run completed. Temporary review tests were removed after the reproduction.

- `cargo test -p dash-spv --lib storage::migrator -- --nocapture`: 8 passed.
- `cargo test -p dash-spv --lib sync::filters::manager::tests -- --nocapture`: 61 passed, including the dense 5,000-filter current-thread test. It observed 1,460,418 ticker yields during a 22.7-second scan; wall-clock pauses under this box's load are not a benchmark.
- `cargo test -p dash-spv --lib review_ -- --nocapture`: 2 temporary restart/collision tests passed; the partial-aside plus corrupted-version preservation test failed as described above.
- `cargo test -p dash-spv --lib pool::tests -- --nocapture`: 4 passed (including the expected worker panic propagation).
- `cargo test -p dash-spv --lib storage::tests::test_clear_clears_index -- --nocapture`: 1 passed.
- With `eval "$(python3 contrib/setup-dashd.py)"`, `cargo test -p dash-spv-ffi --features blst-no-threads --test thread_count -- --nocapture`: passed through live sync; baseline 3, peak 9, bound 9. The dashd skip path was not used.

These tests model post-crash directory states; they do not simulate a physical power loss.

VERDICT: CHANGES-REQUIRED
