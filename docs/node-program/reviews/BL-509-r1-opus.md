# BL-509 r1 review (Opus)

- Branch `node/bl509`, head 5ccb13851, base node/integration 69e0af4e2.
- Scope: DEC-718 option 1. A store marker (`RDASHD`) is written before the first chainstate is created, and `is_leveldb` trusts it.

**Verdict: CHANGES-REQUIRED (M 1, m 2, n 3).**

The core mechanism is sound:
- The marker is written and synced before RocksDB's first file.
- The probe checks the marker by name only.
- The store accepts the marker as creation debris.
- On an empty or missing directory, I found no crash point that the probe misclassifies.

The gap is the precondition "missing or empty". The store also creates a new database in a directory that holds a `lost+found` (a supported configuration) or leftover creation debris, and `mark_new_store` leaves such directories unmarked. The BL-509 crash therefore still bricks the datadir on an ext4 filesystem mounted at `chainstate/`.

---

## Findings

### M-1: a chainstate on its own ext4 filesystem is never marked, so BL-509 still bricks it

**Where:** `dashd-node/src/datadir.rs:323-326` against `dashd-store/src/rocks.rs:564-596` (`classify_uncreated`) and the test `rocks.rs:1226` (`creates_on_a_dedicated_filesystem_with_lost_found`).

The store explicitly supports a filesystem dedicated to the database: a directory holding only `lost+found` "becomes a new database" (rocks.rs:180-184, :559-562). `mark_new_store` returns early at line 326 for any non-empty directory, and `lost+found` makes the directory non-empty.

**Crash point:**
1. `mkfs.ext4` a device and mount it at `<datadir>/chainstate`. Mounting chainstate on a separate SSD is common practice for Core users. mke2fs creates `lost+found`.
2. Start rdashd for the first time.
3. `mark_new_store` sees one entry and writes nothing.
4. RocksDB creates `IDENTITY`, `MANIFEST-000001` and `CURRENT`.
5. Power fails before `OPTIONS-*` (any of DEC-718's 17 points).
6. On reboot the directory holds `lost+found CURRENT MANIFEST-000001 IDENTITY LOCK LOG`, with no marker.
7. `is_leveldb` returns true, and every later start is refused as Dash Core's. This is exactly BL-509.

xfs creates no `lost+found`, so ST3b's xfs points would pass while ext4 fails here. ST3b mounts its image at the datadir, so `lost+found` sits one level up and the harness cannot see this case.

**Fix:** mark exactly when the store is about to create a database, not only when the directory is empty. That means: no `CURRENT`, and the directory holds only creation debris, `lost+found` or the marker. In other words, `classify_uncreated` would say "create". Two ways to do it:
- (a) Export a schema-free predicate from dashd-store, e.g. `rocks::may_create(path) -> io::Result<bool>`, sharing the loop in `classify_uncreated`. `mark_new_store` marks when it returns true.
- (b) Preferred: move the marking into `RocksEngine::open` itself, right after `classify_uncreated` returns `Ok` and before `DB::open_cf_descriptors`. The store is the only party that knows it is about to create a database. It already owns `STORE_MARKER` and `is_creation_debris`. `open_node` then needs no extra call.

Either way:
- Write the marker with create-or-keep semantics.
- Always sync the store dir when no `CURRENT` exists, even if the marker is already present (see n-3).

Neither variant widens what the store will overwrite: `classify_uncreated` already decides creation. A Core LevelDB without `CURRENT` always has `.ldb` or `.log` files, which `is_database_data` rejects, and Core's datadir is refused by the probe before `open_node` runs (see "False positives" below).

**Test:**
- Add `lost+found/` (and separately `LOG` + `LOCK`, for m-1) to an empty chainstate.
- Call the marking entry point, then write `CRASHED_STORES[0]`.
- Assert `probe_core_data_dir == None`.
- The current head fails this test.

### m-1: a chainstate holding a pre-fix rdashd's early-failure debris is not marked either

**Where:** `datadir.rs:326`.

**Crash point:**
1. An rdashd from before this commit died in its first creation before `CURRENT`. Causes include disk full, kill, or a bad option (rocks.rs:536-541). That leaves `LOG LOCK IDENTITY` (and possibly `OPTIONS-*`/`MANIFEST-*`), with no marker.
2. The fixed rdashd starts. The directory is not empty, so it is not marked.
3. The store creates over the debris, as `is_creation_debris` allows.
4. Power fails before `OPTIONS-*`.
5. The directory is unmarked and LevelDB-shaped, so it is refused for good.

This is narrower than M-1, since it needs a failed pre-fix first start, but it is the same hole. The fix for M-1 covers it. The docs at datadir.rs:315-321 and :340-342 should then stop saying that debris is "the store's open to judge" and is not marked.

### m-2: nothing tests the fsync ordering, which is the substance of the fix; prove it with an ST3b pre-phase rerun before READY

**Where:** `datadir.rs:329-335`.

The five killed mutants all test naming and placement. These mutants survive every unit test, by construction:
- delete `fs.sync_dir(store)` (line 335). This is the one sync that matters: it makes the `RDASHD` dirent durable before RocksDB's first file.
- delete `fs.sync_dir(parent)` (line 330);
- delete `file.sync_all()` (line 334).

**Failing input for the first mutant:**
1. On a filesystem whose metadata is not journaled in order, the marker's dirent may be lost while RocksDB's later `CURRENT`/`MANIFEST` survive a power loss.
2. The result is pt27 again.

On ext4 and xfs, in-order metadata journaling hides this, so even ST3b may not see it. Still, a dropped sync is the regression this fix most needs to guard against.

**Fix:** as close-out evidence, rerun ST3b's pre-phase window with the BL-509 binary. That is every point up to the first chainstate's `OPTIONS-*`, on both ext4 and xfs.
- The old images (pt27 etc.) can't serve: the unfixed binary made them unmarked.
- The marker's writes shift the point indices, so run the window, not the old point numbers.
- Record that the 17 DEC-718 failures, and every new marker-step point, pass C1.

Optionally, take `fs: &impl FlatFs` in the marking function so a later ST3a-style faulty FS can drop unsynced entries in a unit test.

### n-1: overlong doc line

`dashd-store/src/rocks.rs:182` is 124 characters, and `wrap_comments = false` means rustfmt won't fix it (`max_width`/`comment_width` = 100). Rewrap it by hand.

### n-2: BL-147(b) merge order. Let `.ldb` win over the marker

**Where:** `datadir.rs:308`, merging with 50967d825.

The planned combined rule ("marker first, then IDENTITY/`.ldb`") is sound in practice, but folding the marker into BL-147(b)'s RocksDB-sign set is strictly safer at no cost:

```rust
names.iter().any(|n| n == "CURRENT")
    && names.iter().any(|n| n.starts_with("MANIFEST-"))
    && (names.iter().any(|n| n.ends_with(".ldb"))
        || !names.iter().any(|n| n == STORE_MARKER || n == "IDENTITY" || n.starts_with("OPTIONS-")))
```

**Reason:** RocksDB never writes `*.ldb`, so a table file is proof of LevelDB even next to a stray `RDASHD`.

**Reachability:**
1. rdashd marks an empty `chainstate/` and dies before RocksDB writes anything.
2. The user then runs dashd on the same datadir.
3. LevelDB creates the database around the marker. LevelDB keeps names it can't parse (`RemoveObsoleteFiles` skips them).
4. Core still makes `evodb/` first, so the DashDir sign refuses the directory anyway. Core v24.0.0-rc.4 creates evodb at `src/init.cpp:2041`, before `CBlockTreeDB` at `src/node/chainstate.cpp:155` and `InitCoinsDB` at `src/node/chainstate.cpp:233`.

So with "marker first", only a user-deleted `evodb/` would slip through. With `.ldb` first, nothing would.

**Merge note:** both branches rewrite the doc of `CoreDataDirSign::LevelDb` and of `refuse_core_dir`.
- Keep BL-509's sentence about the marker.
- Drop the stale "part-way through a RocksDB looks like LevelDB" sentence only once both rules are in.
- BL-147(b)'s `a_rocksdb_the_node_makes_has_its_identity_file` test stays valid (no marker is involved).

### n-3: an already-present but unsynced marker skips the syncs

**Where:** `datadir.rs:326`.

**Scenario:**
1. rdashd is SIGKILLed (no power loss) between `File::create` (line 332) and `sync_dir(store)` (line 335).
2. An immediate restart sees `RDASHD` in the page cache.
3. `mark_new_store` returns without syncing, and RocksDB proceeds.

This is safe today, for two reasons:
- RocksDB fsyncs the database directory after `CURRENT` (`SetCurrentFile`), which persists every entry including `RDASHD`.
- ext4 and xfs journal the earlier `RDASHD` create no later than `CURRENT`'s rename.

That is reliance on RocksDB internals and filesystem ordering, which the comment at datadir.rs:317-318 ("durable … before any of the database's files exists") doesn't state. The M-1 fix should sync the store directory whenever no `CURRENT` exists, marker present or not. That costs one fsync and removes the reliance.

---

## Focus answers

1. **Crash ordering** (for a missing or empty directory):
   - Dir created, no parent sync yet: after the crash the dir is either absent or empty. The probe says None (`is_leveldb` needs `CURRENT`) and the next start marks it.
   - Parent synced, no marker yet: the dir is empty. Same result.
   - Marker created but not yet synced: either no marker (empty dir) or a zero-length/torn marker. The probe and `is_creation_debris` match on the name only (datadir.rs:308, rocks.rs:543), so a zero-length marker is recognised. It should be: the contents are never read, and only rdashd creates that name in an empty store dir.
   - Marker durable, then any subset of RocksDB's files: marked, so `is_leveldb` is false. The store then either creates over debris (no `CURRENT`) or opens the database (pt27 opens and loads; node.rs test).
   - Ordering: the parent-dir sync (line 330) runs before the marker is created, and the store-dir sync (line 335) runs before RocksDB starts. Both are in the right order.
   - Only the store's immediate parent is synced when `create_dir_all` makes several levels. That is harmless: if a missing ancestor's entry is lost, the whole subtree is absent, which is the fresh state.
   - `OsFs::sync_dir`'s EINVAL tolerance (dashd-store/src/flatfile/fs.rs:175-180, :260-262) hides nothing on ext4 or xfs. Both implement `fsync` on directory fds (`ext4_sync_file`, `xfs_dir_fsync`), and EINVAL comes only from filesystems without an fsync op. On those it silently drops durability, which is a pre-existing, documented trade-off of the helper.
   - Gaps: M-1 and m-1 (non-empty creatable dirs) and n-3.
2. **False positives:**
   - `mark_new_store` runs inside `open_node`, after `lock_data_directory(args, false)` (node.rs:173), and after the probe (`init_config` → `refuse_core_base_dir`, init/mod.rs:60). A concurrent Core or rdashd holding `.lock` stops the start before any marking, so the empty check and the create cannot race another starter.
   - The under-lock re-probe in `refuse_core_dir` (datadir.rs:346-360) still covers a concurrent start reading a part-made directory. Once marked, the marker exists before `CURRENT`, and a later `readdir` that sees `CURRENT` also sees `RDASHD`.
   - A Core datadir can't get a marker: it is refused first. The only way Core's files end up next to a marker is if Core runs after a dead rdashd start (n-2), and evodb catches that case.
   - `blocks/index` is never marked by rdashd. The probe's generality there is harmless.
3. **Store side:** `is_creation_debris` + `STORE_MARKER` widens creation only to a directory holding the marker alone or the marker plus debris. A real database without `CURRENT` has numbered `.sst/.log/.ldb/.blob` files, so `classify_uncreated` still returns `Corruption`. With `CURRENT` present, `inspect_existing` decides as before. I found no path where a real database is treated as debris because of the marker. RocksDB keeps the marker because `ParseFileName("RDASHD")` fails in `PurgeObsoleteFiles`, and the node.rs test asserts it survives a shutdown.
4. **Tests and mutants:** the implementer's M1–M5 log (`/work/scratch/nd1-bl509/probe/mutants.log`) shows each one killed. The datadir test removes the marker and re-asserts `LevelDb`, so it is sensitive without a mutant too. Mutants that survive:
   - drop each of the three syncs (m-2);
   - any mutant on the `lost+found`/debris path, since there is no test for it (M-1, m-1).
5. **BL-147(b) interaction:** sound, but prefer `.ldb` over the marker (n-2). With BL-147(b) merged, pt27's set (it has `IDENTITY`) is already non-LevelDB. The marker is still needed, as DEC-718 rules: it covers the window before `IDENTITY` and orders against `CURRENT`, and it doesn't depend on RocksDB's `write_identity_file` default.

## What I checked and ran

- Read:
  - the full diff `69e0af4e2..5ccb13851`;
  - `datadir.rs` (probe, `refuse_core_dir`, lock helpers);
  - `chainstate.rs` `open_node` / `Opening::on`;
  - `node.rs` init order (lock at :173, load at :303);
  - `init/mod.rs:60`;
  - `rocks.rs` `open`, `is_creation_debris`, `is_database_data`, `classify_uncreated` and its tests;
  - `flatfile/fs.rs` `OsFs::sync_dir` and `einval_is_ok`.
- Grepped every `open_chainstate` caller. The only production path is `open_node`; the others are benches, the bake-off and tests. No production code removes or wipes the chainstate directory.
- Read BL-147(b)'s `is_leveldb` diff (50967d825 against merge-base cf5079316).
- Read DEC-718 and BACKLOG BL-509.
- Read the implementer's `repro.sh`, `probe/mutants.sh` and `mutants.log`, and the pt27 chainstate file list under `/work/scratch/nd1-bl509/img`.
- Core v24.0.0-rc.4: checked the evodb, blocks/index and chainstate creation order in `src/init.cpp` and `src/node/chainstate.cpp`.
- Built or ran nothing. M-1 follows directly from line 326 (`Ok(false) => return Ok(())`) and the store's documented `lost+found` acceptance. No reflink copy was made, so `/work/scratch/bl509-r1-opus` does not exist.
