# SB13 SQLite store review, round 1 (Sol)

Reviewed `6993f2f2f..e190e1429` in a detached worktree. The branch under review was not changed.

## Findings

### Blocker: an interrupted v2 set-aside can delete the rest of the original store

`dash-spv/src/storage/migrator/v3.rs:101-111,117-138,186-190`; `dash-spv/src/storage/migrator/mod.rs:71-79,136-150`.

After a corrupt v2 row, the migrator moves folders to `v2-unmigrated/` one by one. If a crash or an I/O error interrupts that loop after `block_headers/` moves, the next open sees the remaining v2 folders but no headers. `build` treats that as a successful empty migration, writes version 3, and `remove_v2_leftovers` deletes every folder still at the root. The aside holds only the already moved folders; the rest of the user's original v2 data is gone. The existing interruption test moves `metadata/` while the corrupt `filters/` input remains at the root, so it necessarily re-enters the set-aside path and misses this case.

I reproduced this in the detached worktree by writing the existing 3,000-header v2 fixture, moving `block_headers/` to `v2-unmigrated/`, then calling `migrate` and cleanup. The assertion that `filter_headers/` survived failed (`filter_headers lost after interrupted set-aside`). Preserve a durable set-aside intent before the first move, or detect an existing `v2-unmigrated/` and complete moving all remaining v2 folders before trying to build v3. Test interruption after each move and through the public `StorageMigrator::migrate` path.

### Major: filter attribution still blocks the async runtime thread

`dash-spv/src/sync/filters/manager.rs:1003-1053`.

`CpuPool::run` moves the union match to the pool, but after awaiting it `scan_batch` loops over every matched filter and every wallet and calls `filter.match_any` synchronously on the async task thread. A dense-match 5,000-filter batch, especially with several wallets or a large watched-script set, can keep a current-thread runtime from running its network and block tasks for the duration of that second pass. The same network starvation that motivated D38 therefore remains for the attribution work; the existing one-match tests do not exercise it. Move the entire union-match and per-wallet attribution calculation into one `CpuPool::run` job, returning the `BTreeMap` to the task.

## Validation and limits

All local tests used the detached worktree's own `CARGO_TARGET_DIR` and `TMPDIR=/dev/shm/sb13-r1-sol-tmp` (mode 700). The FFI test used `eval "$(python3 contrib/setup-dashd.py)"` and did not take the dashd skip path.

- `cargo test -p dash-spv --lib storage::store -- --nocapture`: 35 passed, including the R6-M2 witness, both crash paths, nested crashes, injected failures and the 500-case sweep.
- Existing `storage::migrator::v3::tests`: 4 passed. The temporary interruption regression failed as described above; it was added only in the detached scratch worktree.
- `cargo test -p dash-spv --test migrate_v3 -- --nocapture`: the 200,000 versus 20,000 header memory test passed (3.8 MiB peak heap for both; 14.5 MiB and 0.1 MiB incremental peak RSS respectively).
- `cargo test -p dash-spv-ffi --features blst-no-threads --test thread_count -- --nocapture`: passed through live sync and reorg; baseline 3, peak 9, bound 9 (2 async workers, 2 blocking threads, 2 CPU-pool threads).
- `cargo test -p dash-spv --lib pool::tests -- --nocapture`: 4 passed, including the current-thread-runtime await and panic-propagation test (the expected worker panic was caught).
- Inspected the O5 graph and Clippy checks and their planted cases. I found no gate bypass in the cases SB13 claims; the owner's seven-case selftest was not rerun here.
- The mainnet-sized migration and startup tests are `#[ignore]`; the branch's D17 and D25 measurements were inspected, not repeated here.
- The dense-match attribution risk is from the synchronous control flow and the 5,000-filter batch limit; I did not benchmark a crafted dense-match batch.

Compatibility checked: the FFI config's C layout is unchanged, but `worker_threads = 0` now means two workers instead of Tokio's automatic count. The public Rust validator constructors now require a `CpuPool`, and the v3 store opens only after the v2 migration. These changes are intentional in the `feat!` commits; the interrupted migration above is the compatibility failure.

## Verdict

**CHANGES-REQUIRED**
