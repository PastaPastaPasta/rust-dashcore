# SB14 implementation review, round 3 (Sol, gpt-6-sol xhigh)

Head: `309a7f420` (`node/sb13-spv-sqlite`), reviewed against the fix-round range
`c4177a609..309a7f420`. The review covers the non-merge first-parent commits listed in the
manager brief. No source changes were made.

## Verdict

**CHANGES REQUIRED**

## Findings

### Major

1. **V3 discard can delete files outside the storage directory through a symlinked store directory.**

   - **Where:** `dash-spv/src/storage/migrator/v3.rs:102-122`
   - **Scenario:**
     1. Set `version.json = 3`, and make `spv/` a symlink to an unrelated directory that contains only names from `SQLITE_FILES`.
     2. The validation `read_dir(dir)` follows the symlink and accepts those names.
     3. `remove_file(dir.join("kv.sqlite"))` then resolves the directory symlink and unlinks the unrelated target file.
     4. `remove_dir(dir)` reports that the symlink is not a directory, but only after the deletion has happened.

     This violates the documented "known names only, otherwise refuse with nothing deleted" behavior and D39's no-loss rule.
   - **Fix:** Validate each store directory with `symlink_metadata` and require a real directory before scanning or deleting. Also validate that allowed children are regular files. Refuse the migration before the deletion loop if either store path is a symlink, or if it contains an allowed-name entry that is not a regular file. The fix round left this existing path unaddressed, but it is still within the migration safety scope.

### Minor

2. **Unrecognized files in a migrated v2 `metadata/` directory are silently deleted.**

   - **Where:** `dash-spv/src/storage/migrator/v3.rs:498-512`
   - **Scenario:** Add `metadata/notes` (or another non-`.dat` entry) to an otherwise valid v2 store with a header run. `migrate_metadata` skips the entry and the build succeeds. Because `spv/` exists, `remove_v2_leftovers` later removes the whole `metadata/` directory. The file is neither migrated nor set aside.
   - **Fix:** Do one of the following:
     - treat an unrecognized or non-regular metadata entry as migration corruption, and route the whole v2 tree through `set_aside_corrupt`;
     - explicitly preserve such entries in `v2-unmigrated/`.

     If these entries are meant to be disposable, document that ownership rule and test it.

## Requested checks

- **Subscriber lease/retry race.** The r1 Major 1 race is closed.
  - `commit_naming` builds the bounds, locators and named height while holding the lease. If that height is not covered, it drops the lease, takes the header writer mutex, raises files, and retries. The retry's `Cap` clamps the locator to the raised slot tip, so a capped build cannot produce a name above the cap and the retry terminates.
  - Lowering holds the writer mutex and rewinds subscribers under every batch lease. It publishes the lowered header state and subscriber bounds before releasing that lease, and makes the final batch Durable.
  - `put_sub`, `truncate_sub`, `put_mn_message`, recovery rewind, metadata and establish all use the corrected commit path. `truncate_sub`'s lock-free no-op check is re-evaluated under the lease before any mutation.
  - The deterministic subscriber/lowering interleaving tests pass.
- **Lockfile protocol.** The D45 fix is sound for the normal two-opener/unlink schedule.
  - The lock path is opened without truncation, and the PID bytes change only after a successful lock.
  - On Unix, a successful lock is checked against the current path's device/inode. An opener that locked an inode the previous holder unlinked discards it and reopens.
  - The holder unlinks only while it still holds the lock. On non-Unix the path is kept, which avoids the inode-replacement race where identity cannot be checked.
- **D43 EIO handling.** The EIO handling and the crash order are sound.
  - Only Unix EIO turns into an invalid file slot.
  - The unreadable slot is zeroed and the tip file is synced before any truncation.
  - If both slot reads are EIO, the second error is returned.
  - With one EIO and a valid other slot, the valid slot is checked and made durable before truncation.
- **Migration fixes.** The cases this round addressed are closed:
  - a direct open at version 4 refuses remaining v2 folders;
  - a v2 run reaching `HEIGHT_CAP` is set aside;
  - online and resumed clears share the keep set;
  - a newer storage version is refused before clear or migration;
  - segment scanning accepts only exact names.

  The two findings above are the remaining destructive paths.
- **D51 and D46.**
  - The D51 oracle change is justified: the reference rewinds subscriber paths when a lowering reactivates records, and the recovery floor considers the latest intervening lowering prefix.
  - The D46 no-code ruling is justified for this round: there is no new production read, and the glibc overcommit probe explains the extra read syscalls.
- **Previously reported points.** These all remain closed:
  - direct v2 open no longer deletes headers;
  - `persist` uses `sync_wal`;
  - the writer latch covers all write paths;
  - duplicate v2 hashes are set aside;
  - the MN log exception is documented;
  - the lock PID and flock/unlink race are fixed;
  - D42 and the Windows I/O changes are covered.

  The pre-existing `handle_fork` path and the `filter_headers.rs:76` view read are outside this round (BL-234, BL-235).

## Validation

- `cargo test -p dash-spv --lib storage::store::tests --all-features`: 42 passed
- `cargo test -p dash-spv --lib storage::migrator::v3::tests --all-features`: 17 passed
- `cargo test -p dash-spv --lib storage::store::crash::tests --all-features`: 24 passed
- `git diff --check c4177a609..309a7f420`: passed
