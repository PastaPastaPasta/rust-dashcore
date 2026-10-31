# SB14 implementation review, Opus-h round 2 (437d68732, node/sb13-spv-sqlite; range c4177a609..437d68732)

I reviewed the range in a detached worktree. Every probe and mutant was temporary and has been reverted, and no branch file was changed. The range covers the fix round r1 (6079c1e72..75b8f7919), the Sol r3 fixes (783044b71) and three follow-merges (4ed77fba9, 309a7f420, 437d68732).

## Verdict

**ACCEPT WITH NITS**

Every r1 finding is closed in the code. The Sol r3 fixes are correct, and I found no interleaving that breaks D16 (3) or D45. What remains:
- one lock-lifetime hole, older than SB14 but now load-bearing for D39;
- two rules the tests do not pin;
- one more instance of the Sol r3 #2 class (unknown v2 entries deleted);
- nits.

None is a crash-consistency or durability defect in the store.

## Findings

### Blocker

None.

### Major

None.

### Minor

**1. `DiskStorageManager` releases `<dir>.lock` when the manager drops, while the store it opened can still write. A second opener is then granted the directory (older than SB14; D39/D45 now rely on this lock).**
- **Where:**
  - `dash-spv/src/storage/mod.rs:134` (`_lock_file` is a field of the manager);
  - `:292-298` (`Drop` cancels the worker but does not wait for it);
  - `:270-273` (the worker upgrades its weak handle and runs `sync_wal` on the blocking pool);
  - the handles that `block_headers()`, `filter_headers()` and the others return are `Arc` clones of the same `SpvStore`.
  - The direct open already does this correctly: `handle.rs:21-24` keeps its lock in `Shared`, after `store`, so the lock is released only when the last handle closes the store.
- **Probe** (deterministic; reverted):
  1. `DiskStorageManager::new(dir)`, then `let held = first.block_headers()`, then `drop(first)`.
  2. A second `DiskStorageManager::new(dir)` **is admitted**.
  3. `held` stores 50 headers and the second manager stores 20. Both succeed against the same segment files, `tip` and `spv/kv.sqlite`.
  4. A reopen shows tip 19. The old handle's acknowledged writes are gone. If the two had written different chains below the new tip, the reopen would read a mixed chain: either a `Layout` error and D19's clear, or records the slot does not describe.
- **The same window with no stray handle:** a drop without `stop` while the worker's tick is mid-`sync_wal`. The raise writes segments, the file slot or the SQLite marker after the lock is gone, while a new opener runs migration, Open's truncation or a clear. That also contradicts D8's "a drop alone writes nothing".
- **Exposure:** FFI `_destroy` calls `stop`, which waits for the worker, so the FFI is not exposed. A Rust caller that drops a client without `stop` and reopens the directory in the same process is. This is the "schedule that grants two openers" asked about in question 4. It is not a flock race: the lock is handed over early.
- **Fix:**
  - Give the manager's lock to `Shared`, as the direct open does, so it lives as long as the last store handle.
  - The `Layout` → clear → reopen path in `new` needs the lock across the failed first open. Hold it as `Arc<LockFile>` in both the manager and `Shared`, or pass it into `open_migrated` only for the open that succeeds.
  - Add the probe above as a test that expects the second `new` to fail with `DirectoryLocked`.
  - If the manager defers this, file a BL row and add a sentence to D39 saying the lock covers the manager's lifetime, not the store's.

**2. Two of the D44 re-reads under the lease are not pinned. Mutants J and K survive all 167 `storage::` tests.**
- **J: `truncate_sub` builds from bounds read before the lease** (the re-read is at `store/legacy.rs:371`).
  - Mutant: reuse the lock-free pre-check's `sub_bounds` inside `build`. It survives.
  - Bug it would hide: the r1 M1 bug for truncation. Filter headers sit at 0..=10. `truncate_sub(f, 7)` passes its check, then `truncate_headers(4)` lands. The stale bounds publish `hi = 7` over rows 5..=7 that the rewind deleted.
  - The fix in the code is correct. My candidate test passes on 437d68732 and fails under J: `sub_race_store(.., 10)`, `interleave(Point::Naming, truncate_headers(4))`, `truncate_sub(f, 7)`, `sub_state` equal to the sequential order. It is a copy of `a_subscriber_commit_reads_a_lowering_before_its_lease` (`store/tests.rs:921`) with `truncate_sub`.
  - Fix: add that test. Do the same for recovery's `rewind_sub` if it can be driven (it runs at open, so this is optional).
- **K: `put_mn_message` ignores the `Cap`** (`store/legacy.rs:481`).
  - Mutant: `.map(|t| height.min(t))`. It survives.
  - Impact: the guard at `store/lower.rs:354` turns it into a spurious `Error::Corrupt` instead of a slot that names too high a height, so nothing bad becomes durable. The masternode path would fail on a fast append that races the raise.
  - Fix: a variant of `a_slot_carrying_subscriber_commit_names_at_most_its_slot` that uses `put_mn_message(15, ..)`.
- **Killed:** mutant M (rewind in the first lowering batch only) is killed by `a_later_lowering_batch_rewinds_a_subscriber_commit`.

**3. Unknown entries in `filter_headers/`, `filters/` and `blocks/` are still deleted with their folders (Sol r3 #2 fixed only `metadata/`).**
- **Where:** `migrator/v3.rs:614-628` (`segment_ids` skips every name that is not a six-digit segment), then `remove_v2_leftovers`, `:446-448` (`remove_dir_all` of each v2 folder once `spv/` exists).
- **Scenario:**
  1. A valid v2 store with a header run, plus `filters/notes`, or a leftover `filters/segment_12.dat`. Since n4, any name that is not exactly six digits is skipped, and v2's own parser accepted it.
  2. The build succeeds and `spv/` is renamed into place.
  3. The next `remove_v2_leftovers` deletes `filters/` with the stray entry, which is neither migrated nor set aside.
- **Why it matters:** Sol r3 #2's own rule, now in the code at `v3.rs:519-525`, says "anything that is not a `<key>.dat` file is v2 data this migration would delete with `metadata/`: the tree is set aside instead (D39)". The segment folders have the same exposure. In practice only v2's atomic-write leftovers (`.segment_NNNNNN.dat.<pid>.<n>.tmp`) occur there, so the data at risk is small.
- **Fix:** do one of these, and make D39 say which:
  - apply the same rule in `for_each_item`/`segment_ids`: an entry that is not a six-digit segment and not a v2 `.….tmp` leftover is a `Corrupt` build, so the tree is set aside;
  - record in D39 that `filter_headers/`, `filters/` and `blocks/` are owned whole by the migration, and that unknown entries there are discarded, as `masternodes/` is (SB13 D6).

### Nit

1. **`fn newer` was inserted between `migration_loop`'s doc comment and the function** (`migrator/mod.rs:286-293`). rustdoc now gives `newer` both paragraphs, and `migration_loop` has none. Move `newer` above the comment.
2. **The oracle's lowering prefixes also truncate the wallet path** (`store/crash.rs:460-463`, `for p in &mut paths`). The store never rewinds the wallet, and D51's `rewind_store_subs` (`:420`) correctly truncates only `paths[..3]`. So each lowering prefix adds a wallet state that E3 may never produce, which loosens the wallet half of S3 (a test-only looseness, older than the range). Probe L, `&mut paths[..3]`, passes all 167 `storage::` tests. Tighten it, or say why the wallet belongs in the prefix.
3. **The stricter `metadata/` rule now sets aside a whole valid v2 tree over an OS artefact**: a macOS Finder `.DS_Store`, or `Thumbs.db`/`desktop.ini`, in `metadata/` (`v3.rs:516-525`). Nothing is lost, but the client resyncs from scratch. Consider skipping those names, as v2's `.tmp` leftovers already are.
4. **(r1 n5, partly)** `open_dir` is now `pub(crate)`, but `Store::open(Layout)` is still `pub` under `test-utils` (`store/mod.rs:382`), so a `test-utils` build can still skip the admission check and the lock. That is acceptable as D47's test surface. A one-line note in D47 would close it.

## Answers to the questions

### 1. Are the r1 findings closed? Yes, all of them, checked against the code and a test.

| r1 | Code | Test / evidence |
|---|---|---|
| **Major 1** (subscriber vs lowering) | `commit_naming` builds under the lease and returns the named height (`lower.rs:324-395`). An uncovered height drops the batch, takes the writer mutex, raises, and rebuilds with `Cap`. `put_sub`, `truncate_sub`, `put_mn_message` and `rewind_sub` read their bounds and tip inside `build`. Every lowering batch rewinds under its lease (`lower.rs:142`). | `a_subscriber_commit_reads_a_lowering_before_its_lease` (my r1 probe, now passing), `…names_at_most_its_slot`, `a_lowering_rewinds…before_its_lease`, `a_later_lowering_batch_rewinds…` (kills M). The truncate_sub half is unpinned (Minor 2). |
| **m2: M13** (alternation) | unchanged code | `a_torn_newest_file_slot_leaves_the_previous_raise` (M13 makes `two_file_raises` find a missing slot) |
| **m2: M7** (open barrier) | — | `open_commits_through_the_write_barrier` (counts exactly one `write_barrier`) |
| **m2: M2, M8, M9, M11** | `Point` hooks (`store/mod.rs:315-326`) are a no-op outside `cfg(test)`, `#[inline(always)]` | `a_run_opened_after_sync_wals_check_is_durable`, `a_raise_refuses_a_failure_latched…`, `a_file_slot_raise_waits…`, `a_fast_append_refuses_a_latch…`. Each asserts the property that its mutant breaks. |
| **m2: EIO under Extended** | D43: only Unix EIO marks a slot invalid; a second EIO fails the open; the unreadable slot is zeroed and synced in step 4 before truncation (`store/mod.rs:402-499`). The slots are 64 KiB apart, so one torn sector cannot hit both. | `an_unreadable_file_slot_is_invalid`, `a_slot_read_error_other_than_eio_fails_the_open` |
| **m3** (direct open over an interrupted set-aside) | `admit_direct_open` refuses version-2 folders beside version 4 (`migrator/mod.rs:214-223`) | `a_direct_open_refuses_an_interrupted_set_aside` |
| **m4** (height cap) | `scan_headers` returns `Corrupt` at `h >= HEIGHT_CAP`, after the hole check | `a_v2_run_reaching_the_height_cap_is_set_aside` |
| **m5** (clear keep set) | the online `clear()` uses `kept_by_clear` (`storage/mod.rs:315-321`) | — (a one-line shared predicate) |
| **m6** (`read_syscalls`) | no code change, which is right; §7 records the strace attribution and leaves it to the full PF0 | doc only |
| **m7** (lockfile) | no truncate before the lock; dev/ino recheck; unlink only while locked; never unlinked off Unix | `a_refused_lock_leaves_the_holders_file`, `a_lock_on_an_unlinked_file_is_not_held` |
| **n1** (D42 floor) | b114d5d33 | see question 5 |
| **n2** (newer version) | refused before the clear intent and before the loop | `an_interrupted_set_aside_loses_no_folder`'s newer cases |
| **n3** (stop swallows its error) | `try_stop` on the trait, the client and the FFI | `try_stop_returns_a_failed_flush` |
| **n4** (segment names) | `hdr::segment_of`. Its side effect is Minor 3. | — |
| **n5** (`open_dir` public) | `pub(crate)`; Nit 4 | — |

### 2. Are the Sol r3 fixes complete? Yes for what they target. One more instance of #2's class remains (Minor 3).

- **`discard_v3`:**
  - `symlink_metadata` runs before `read_dir`, and every known name must be a regular file per `DirEntry::file_type`, which does not follow links.
  - Everything is validated before the first `remove_file`. `remove_dir` removes only an empty directory.
  - Test: `a_v3_discard_refuses_links_and_non_files`.
- **The set-aside:** only `rename`, which moves a link and never its target. `present` uses `symlink_metadata`, so a dangling link counts as present, and collisions take `<folder>.<n>`. Nothing is deleted.
- **`remove_v2_leftovers` and `migrate`'s restart:** `remove_dir_all` (std ≥ 1.58) removes a top-level link itself, does not follow links inside, and is TOCTOU-safe internally. The only remaining deletion of data the code does not own is Minor 3.
- **Clear and keep sets:** the online loop branches on `DirEntry::file_type` (not followed), so a linked directory is removed as a link. Both clears keep `logs`, `v2-unmigrated` and `version.json`. The lock file is a sibling, so neither clear sees it.
- **Lockfile unlink:** only the holder unlinks, only while it holds the lock, and only on Unix. `is_at` compares the held inode with `metadata(path)`. Through a symlinked lock path, the link is removed and the target stays; a later opener creates a regular file, and the inodes differ, so it is still sound.
- **Segment removal:** `clear_all`/`remove_segments` list and remove exact segment names inside `block_headers/`. If `block_headers/` is a link (a user moving data to another disk), it is honoured as the store's own directory. I consider that intended, not a hole.
- **TOCTOU between check and delete:** a window exists between `discard_v3`'s validation and its deletes, and between `present` and `rename`. Only a concurrent writer of the storage directory can use it, and the lock excludes every store process, modulo Minor 1. An adversarial local user with write access could delete the files anyway. Not a finding.

### 3. Concurrency in D44: I could not construct a violation.

- **A name above the durable tip:**
  - The lock-free path commits only when `committed.tip() >= names`, reading both under the lease.
  - `h.committed` changes only under the lease: in a lowering batch, after a slot-carrying `commit_naming` commit before `publish`, and in raises.
  - A committed tip from a Buffered SQLite slot is a WAL prefix of the subscriber commit, so a crash keeps the slot whenever it keeps the name. A file slot is synced before `committed` moves.
  - On the retry path, `Cap` bounds every name at the raised slot's tip:
    - `locator_at` clamps;
    - the MN height clamps;
    - an `empty` slot gives `None`.
  - The raise and the rebuild both run under the writer mutex, so no lowering or other raise moves the slot in between. A fast append moves only the in-memory tip, which `Cap` ignores.
- **Livelock:** at most three iterations:
  1. lock-free;
  2. under the mutex, unraised;
  3. under the mutex, raised.
  The `Corrupt` arm at `lower.rs:354` makes a fourth unreachable (mutant K shows it acting as the backstop).
- **Lost subscriber update:** every publisher of `rows.subs` holds the lease:
  - `put_sub`, `truncate_sub` and `clear_sub`;
  - recovery's `rewind_sub`;
  - every lowering batch, which publishes all three bounds wholesale, but reads them under the same lease.
  A subscriber commit between batches publishes its bounds, and the next batch's `rewind_subs` reads and rewinds them (mutant M is killed). The outcome always equals one of the sequential orders, which is what the four interleaving tests assert.
- **What I checked besides:**
  - `truncate_sub`'s lock-free early return uses stale bounds, but it is a linearizable "truncate before the concurrent put".
  - The locator ≤ `hi` invariant holds through the `None` keep-previous case of `put_sub`, so `rewind_subs`'s skip of `hi < f` never strands a locator above f − 1.
  - The activation after a lowering publishes `committed` only after its slot commit, so a lock-free commit that sees the new in-memory tip retries.
  - Concurrent `put_headers` inside a lowering remains excluded only by the header storage's `RwLock` (D16 (1)), as before.

### 4. D45 lockfile and D50 Windows: no schedule grants two holders of the lock.

- **Unix:**
  - An opener that locks an inode its holder unlinked fails `is_at`, because the path is absent or names a new inode, and the inode cannot be reused while the opener holds it open. It then reopens, up to 8 times.
  - Only the holder unlinks, while locked, so at most one locker passes `is_at` at a time.
  - In the holder's unlink→close window, a new opener can lock the new inode while the old holder still holds the old one. That is harmless only if nothing of the old holder still writes, which is Minor 1.
- **Windows:**
  - The path is never unlinked, so there is one file identity, and `LockFileEx` is exclusive per handle, both within one process and across processes.
  - `set_len(0)` and the PID write happen only after the lock.
  - Without lock support (`Unsupported`), there is no exclusion, by design.
- **The schedule I did find** grants the directory, not the flock, to a second opener: the manager hands the lock over while its store lives on. That is Minor 1, demonstrated by the probe.

### 5. D51 and D42 (test-only): neither weakens the oracle on the store.

- **D51:**
  - The post-op reference state now rewinds paths 0..3 to f − 1 whenever f = at + 1 is below the old tip. That is exactly D7, which the store does at every lowering, the reactivating reorg included.
  - S3 is exact equality against the candidate set. The change replaces an unreachable candidate (the subscribers left on re-activated records) with the one D7 requires. That tightens the oracle and admits nothing the design forbids.
  - The guard `f < chain.len()` matches the store's `f <= t`.
  - The wallet path is correctly untouched. Its prefixes, though, do truncate it (Nit 2).
- **D42:**
  - The floor is now the latest intervening lowering's lowest prefix, f₂, instead of the minimum over all of them.
  - That is sound: the last batch of a completed lowering is Durable at f₂ − 1, and its pre-sync made the files below it durable. So no crash can cut below f₂.
  - When f₂ is above the acknowledged length, the floor is f₂, which is tighter than before and still correct.
  - The step path still gets no cut.

### 6. The rest of the range

- **Merges:**
  - 4ed77fba9 resolves a `Cargo.lock` union and the `dash-spv/Cargo.toml` deduplication. `dash-consensus` keeps the `dash-x11` feature, and `dash-num`, `dash-primitives` and `dash-types` are listed once with the bridge.
  - Integration's perf commits to the v2 `header_hash_index.rs` (037bdc0ce, 27f43f977, 42c003c7a) touch only that file. Keeping it deleted is correct: SB13 removed the v2 index, and nothing references it.
  - 309a7f420 and 437d68732 have no conflict resolutions (the remerge-diff is empty).
- **`try_stop`:**
  - `stop_locked`'s `?` is its last statement, so no teardown is skipped.
  - `handle_fork` and `run_locked` now stop on a latched store.
  - FFI `_stop` returns the code, and `_destroy` logs it. FFI_API.md is regenerated.
- **Windows I/O:**
  - `seek_read`/`seek_write` change no cursor-dependent code. `read_full` retries `Interrupted`. Directory syncs are no-ops, as Core's are.
  - Every chmod/EIO test is `cfg(unix)`, `try_stop_returns_a_failed_flush` included.
- **The `reach` hooks** compile to nothing outside tests. `Option::take_if` is stable since 1.80, under the 1.89 MSRV.

## Validation

- **Suites:** `storage::` (lib, `--all-features`, 437d68732) passed 167 of 167 in 76 s at load 36–70. That matches the owner's number.
- **Mutants**, each run against all `storage::` tests and reverted (log: `/work/scratch/sb14-r2-opus/mutants.log`, removed with the scratch):

  | Mutant | Change | Result |
  |---|---|---|
  | J | `truncate_sub` builds from pre-lease bounds | **survived** (Minor 2) |
  | K | `put_mn_message` ignores `Cap` | **survived** (Minor 2; the backstop is the `Corrupt` guard) |
  | M | lowering rewinds in batch 0 only | killed by `a_later_lowering_batch_rewinds_a_subscriber_commit` |
  | L | oracle prefixes keep the wallet path (tightening) | all 167 pass (Nit 2) |

- **Probes** (temporary tests, reverted):
  - the second-manager probe (Minor 1) printed `second opener admitted: true`, then `old handle tip Some(49), new manager tip Some(19)`, then `reopened tip Some(19)`;
  - the J candidate test passes on 437d68732.
- **Not run:**
  - crash sweeps beyond the in-suite ones (owner: 11×500 clean, and the range does not touch the store's crash paths except D51's test-only reference);
  - the dashd integration suites;
  - perfgate (GATE-FAST EXIT 0 per the owner).
