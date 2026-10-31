# SB14 implementation review, Opus-h round 1 (c4177a609, node/sb13-spv-sqlite; range 201f7a04f..c4177a609)

Full independent review in a detached worktree. Every probe and mutant was temporary and has been reverted; no branch file was changed. Migration safety (item 2) was read statically by an Opus sub-reviewer, and I checked its findings against the code (marked "verified" below).

## Findings

### Blocker

None.

### Major

**1. A subscriber write that races a lowering publishes stale bounds and commits a locator above the marker (D16 (3) is not met).**
- **Where:**
  - `dash-spv/src/storage/store/legacy.rs:339-340` (`put_sub`) and `:361-364` (`truncate_sub`) compute `bounds` and the locator before taking any lock.
  - `commit_naming` (`dash-spv/src/storage/store/lower.rs:310-331`) re-checks only "is `names` covered". Its `force` retry, and the lock-free path when coverage still holds, both reuse the stale `build`/`publish` closures. They then publish the stale `bounds` (`legacy.rs:354`, `lower.rs:357`).
- **Why it is new:** SB13 computed both under the lease (`201f7a04f:legacy.rs:689-696`), and SB13's truncation did not rewind subscribers. SB14's lowering does rewind them in its first batch (`lower.rs:112-114`, `:151-170`), so this race is new.
- **Probe** (deterministic, reverted; the probe patch (kept by the manager)):
  - Setup: 12 headers and filter headers 0..=9, both made durable. `put_sub(FilterHeaders, 10)` computes its bounds and locator. Then a `truncate_headers(4)` lands. Then `put_sub` resumes exactly as `legacy.rs:341-355`.
  - Result: `marker=Some(4)`, `view=0..=10`, `locator=10`. `sub_range(0..11)` fails with `cfhdr has no row at 5`.
  - Two violations follow:
    1. §3 "no subscriber row names a height above the marker" is broken, although D16 (3) says header and filter storage "cannot race" on it.
    2. The in-memory view claims rows 5..9 that the rewind deleted. The filter tip reads 10, the sync would resume at 11, and reads of 5..9 fail until a restart. A reopen repairs it: `recover()` gives `Rewound(4)`.
  - The same happens with no `force`: a lowering followed by an activation back above 10 leaves `need` false, and the stale bounds and old-branch locator are published anyway.
- **Exposure:** dash-spv's only product lowering, `handle_fork` (`client/lifecycle.rs:283-301`), stops the sync loop first, so today's client does not hit this. The `StorageManager`/`BlockHeaderStorage` API allows it, though, and the design promises it.
- **Tests:** `raises_race_subscriber_commits_and_lowerings` (`store/tests.rs:727`) runs subscriber commits and lowerings on one thread, so nothing tests this race. Mutant M10, which removes the lease re-check, survives.
- **Fix:**
  - Compute `bounds` and the locator inside `build`, under the lease, and derive `names` from that locator.
  - If the recomputed `names` is not covered on the lock-free path, retry with the writer mutex. Under the mutex, make sure the raised slot covers `names`, or clamp the locator to the raised tip.
  - Add the probe as a test.
- **Possible downgrade:** if the manager instead rules that lowerings never run concurrently with subscriber writes, this drops to Minor. Then D16 (3) and the trait docs must say so.

### Minor

**2. The tests do not pin several crash and concurrency rules: these mutants survive all 145 storage tests.**
- **M13: each file raise rewrites the slot holding the newest file `seq`** (`store/mod.rs:727-730`, which breaks D2's A/B alternation; this is r2 M1's fix).
  - It survives `raises_are_clean_at_every_crash_point`, whose doc claims "a slot is torn while the other holds the newest file `seq`".
  - The harness runs at `Granularity::Sector` with `Adversary::Realistic` (`dash-kv/src/faulty/world.rs:572-573`). A 64-byte slot write inside one 512-byte sector is therefore atomic, and no slot is ever torn.
  - Fix: run the raise scenario at `Byte` granularity or under `Extended`, or add a direct test that corrupts the newest file slot and requires a fallback to the previous raise.
  - Under `Extended`, a torn block can read back as EIO. `Store::open` then propagates the read error (`store/mod.rs:381`) instead of treating that slot as invalid, so the open fails as `Io` (no D19 clear) and the valid other slot is never used. Decide this explicitly. PLAUSIBLE: I did not run `Extended`.
- **M7: Open's repair uses `db.write(b, Durable)` instead of `write_barrier`** (`store/mod.rs:458`).
  - It survives because `FaultyEngine` and `MemoryEngine` implement the barrier as a plain Durable write.
  - D22 is pinned only inside dash-kv-sqlite. Nothing checks that open calls it.
  - Fix: a spy engine that asserts open issues one `write_barrier`.
- **M2: `sync_wal`'s final `raise_locked` uses Buffered** (`store/mod.rs:840`).
  - This is D40's "validator m1" case (a fast append opens a run after the dirty check), and it is untested.
- **Race-only re-checks, also unpinned:**
  - **M8:** the post-sync `check_failed` in `raise_files` (`store/mod.rs:706`, the 0b75784ed fix).
  - **M10:** see finding 1.
  - **M11:** the fast append's latch re-check under the lookup lock (`legacy.rs:92-93`).
  - Each needs a test hook that drives the interleaving.
- **M9: no barrier before a file-slot raise when `sqlite_pending`** (`store/mod.rs:755-758`).
  - It is reachable only through `Store::raise()`, which production never calls: the tick, `stop` and `persist` all use `sync_wal`, which barriers first.
  - Either test `raise()` (a Buffered run commit, then `raise()`, then a cut) or make it test-only.

**3. A direct open can turn an interrupted v2 set-aside into a deletion (verified).**
- **Schedule:**
  1. A v2 directory with `filters/` or `metadata/` but no header run reaches version 4.
  2. `remove_v2_leftovers` starts `set_aside` (no `spv/`), and a crash hits mid-move.
  3. The D39 direct open admits the directory, because it checks only `version.json` (`migrator/mod.rs:209-213`), and its first write creates `spv/`.
  4. The next `DiskStorageManager::new` sees `spv/` and deletes the remaining v2 folders (`migrator/v3.rs:420-435`).
- **Effect:** set-aside data is deleted instead of moved. The data is low-value, but this breaks D39's own rule.
- **Fix:** have the direct open refuse when v2 folders or `v2-unmigrated/` are in flight, or resume the set-aside before deleting.

**4. The v2 scan has no height cap (verified).**
- `scan_headers` (`migrator/v3.rs:251-284`) never checks `HEIGHT_CAP`. A header-only v2 run reaching 2²⁴ gets a marker and version 4.
- `Hdr::open` then reports `Layout` (`hdr/mod.rs:262`), and `DiskStorageManager::new` clears it under D19.
- With other v2 folders present, `open_v2` fails first and the store is set aside. So the two paths disagree on malformed input.
- **Fix:** return `Failure::Corrupt` in the scan.

**5. The online `clear()` deletes `logs/` and `v2-unmigrated/`, but a resumed clear keeps them (verified).**
- The online loop is `storage/mod.rs:303-314`; the resumed clear uses `kept_by_clear` (`migrator/mod.rs:31-33`).
- D19 states one order for both paths.
- **Fix:** reuse `kept_by_clear` in the online loop, or make D19 say which one wins.

**6. `store.read_syscalls` sits at +2 against §7's "bit-stable at ≤ +1".**
- Samples: `[6,6,5]` against base 4. The prior head gave `[5,5,6]`, so this is not a code regression, and nothing new reads in the region:
  - the fresh open sees `ENOENT` for `version.json`, `tip` and `kv.sqlite`;
  - no segment is loaded from disk on sequential appends;
  - `stop` reads nothing.
- **A likely cause of the 5/6 split (PLAUSIBLE):** glibc caches `may_shrink_heap` without synchronization. Two blocking-pool threads that shrink their arenas for the first time at once can both read `/proc/sys/vm/overcommit_memory`.
- **Gate risk:** perfgate's threshold is max(0.5 %, noise floor, 1), so the full gate may flag a +2 median.
- **Before merge:** run the gate. If it flags, the D14 lever applies: allocate the `spawn_blocking` input (`raw()`'s ~1.4 MB `Vec`) off the thread arenas, or reuse it.

**7. (Known, being fixed) `lockfile.rs:17-23` truncates the holder's PID before `try_lock`.**
- Separately, and older than SB14: `Drop` unlinks the path (`lockfile.rs:76-81`), which allows the classic flock+unlink race, where two openers each "hold" a different inode.
- D39's mutual exclusion now relies on this lock, so it is worth a backlog row. PLAUSIBLE: it needs three openers.

### Nit

1. **D42's floor uses the minimum over every lowering since the acknowledged boundary** (`store/crash.rs:1002-1005`), not the latest one. A completed lowering's Durable last batch keeps the chain at or above its own prefix, so the oracle is looser than the store needs to be. That is sound against the public contract (`truncate_above` is documented as Buffered). M1, which makes the last batch Buffered, is still killed by 6 tests, so the looseness hides nothing measured.
2. **A `version.json` newer than 4 is wiped by `DiskStorageManager::new`** (it becomes `Corruption`, then the clear arm, `migrator/mod.rs:159-176`), while D39's direct open refuses it (`:215-221`). This predates SB14, but the two paths now disagree visibly. A downgrade loses the store.
3. **`stop()` logs and swallows a failed `sync_wal`** (`storage/mod.rs:340-347`), and FFI `dash_spv_ffi_client_stop`/`_destroy` return Success regardless (`dash-spv-ffi/src/client.rs:165`, `:372`). This predates SB14, but `stop` is now the only clean flush besides `persist`.
4. **The scan accepts any `segment_<u32>.dat`** (`migrator/v3.rs:598-600`), while Open requires exactly six digits (`hdr/mod.rs:46-49`). Use `hdr::segment_of`.
5. **`Store::open_dir` is `pub` under `test-utils`** (`store/mod.rs:1083`), which bypasses D39's admission check and the lock.

## Item verdicts

1. **Crash consistency and durability: ACCEPT.**
   - **`persist`:** it is now `sync_wal`, which is Durable on both branches. M3, which makes the dirty-runs branch Buffered, is killed by 4 tests.
   - **Tick and `stop`:** both use `sync_wal`, and `stop` is called by the FFI's stop and destroy. No production path takes the Buffered `raise()`.
   - **D42:** the relaxation only covers states after the acknowledged `sync_wal` boundary, and it lowers the floor only through an intervening lowering, which the public contract leaves Buffered (Nit 1). No acknowledged `persist` or `sync_wal` write falls under it.
   - **Other durability mutants are killed:** M1 (the lowering's last batch made Buffered), M12 (the eviction sync skipped) and M14 (open's `tip` sync skipped).
   - **Buffered acknowledgements:** none leaks into a contract callers rely on. `store_headers`, `truncate_above`, subscriber, metadata and MN writes were already Buffered in SB13 and are documented as such.
2. **Migration safety: ACCEPT WITH NITS** (Minors 3–5).
   - **v2 → 4:** every crash point either redoes to the same slot A bytes or leaves a finished store. Nothing writes `block_headers/` at version 2. The marker's segments, `tip` and directory are synced before version 4.
   - **D41:** keys cover only run records, and the full-hash compare is bounded to `[start, tip]`. A repeat across segments is set aside with no marker.
   - **Set-aside:** below version 4 it includes `block_headers`, and at version 4 it never does.
   - **V3Discard:** it refuses before it deletes.
3. **Concurrency: CHANGES REQUIRED** (Major 1).
   - The lock order matches D16 everywhere I traced: writer → lease → fence → view → lookup, with `overrides` taken last. I found no deadlock: the lookup lock is never held while taking the writer mutex or the lease.
   - The fast append's engine reads under the lookup lock use dash-kv-sqlite's reader pool, so they do not wait on a commit's fsync.
   - The latch is checked lock-free at every entry, again under the lookup lock, and after a raise's syncs. No slot or commit lands after it is set (M5 killed). There are no condvars and no lost wakeups.
   - The open problem is D16 (3)'s subscriber-against-lowering race.
4. **Bit-exactness and compatibility: ACCEPT.**
   - Records are v2's: the 80 B header plus the stored hash, served `with_trusted_hash` as before.
   - **Index:** after lowerings, it keeps every active entry (lowering drops only buffer entries above the tip, and merges only drop entries above the current top), and every candidate is verified against the stored hash and bounded by the tip.
   - **Resident buffers:** a lowering cuts the dirty ranges at the tip, and later appends overwrite stale records.
   - **Bases** are truncated correctly.
   - **Restart:** open re-derives linkage and the tip hash, and truncates past the tip.
   - **Overwrite below the top** keeps SB13/v2 semantics: the old records above are re-activated with their ids.
   - The property test passes against the memory-engine model.
5. **Performance against §7: ACCEPT, with a gate check** (Minor 6).
   - `persist` and the tick add no syscalls: an unbuffered barrier is a no-op, and the raise is unchanged.
   - No measured region reads, opens SQLite or loads a segment from disk.
6. **Tests: ACCEPT WITH NITS** (Minor 2).
   - **Pinned (killed):** the r1 fixes for `persist` (M3), the latch at `commit` (M5), the direct-open refusal (M6), D41 (M4), the lowering's Durable last batch (M1), the eviction sync (M12) and open's `tip` sync (M14).
   - **Unpinned (survived):** M2, M7, M8, M9, M10, M11 and M13. Five are race re-checks or test-only paths; M7 and M13 are real coverage holes for D22 at open and D2's alternation.

## Validation

- **Baseline:** `cargo test -p dash-spv --lib --all-features storage::` passed 145 tests (117 s at load ~170).
- **Fresh crash sweeps:** seeds `0x0b055eed`, `0x5b140b01` and `0x5b140b02`, 700 images each, all clean (2,100 images).
- **Probe:** the subscriber-against-lowering race (Major 1) failed as described. The patch is kept at the probe patch (kept by the manager); it was not applied to the branch.
- **14 production mutants,** each run against all `storage::` tests in a separate worktree and reverted:

  | Result | Mutants |
  |---|---|
  | Killed | M1, M3, M4, M5, M6, M12, M14 |
  | Survived | M2, M7, M8, M9, M10, M11, M13 |

  Log: the mutant log (kept by the manager). M1 needed one rerun after a whitespace slip in its edit site.
- **Migration:** a static Opus sub-review of `migrator/`, `storage/mod.rs`, `lockfile.rs` and `io.rs`. I verified its Minors 3, 4 and 5 against the code.
- **Not run:** the dashd integration tests (`dash-spv`/`dash-spv-ffi` `dashd_sync`) and perfgate. The box was at load 130–170 throughout.

CHANGES REQUIRED
