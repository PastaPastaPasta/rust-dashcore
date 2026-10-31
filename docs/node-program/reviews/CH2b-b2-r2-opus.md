APPROVE-WITH-NITS

# CH2b b2, round 2: second independent review (Opus)

**Range:** `ea98ef4be..da7e797d1` on `0b6376d29` (`node/ch2b-prune`), 11 commits, 41 files. I read it from git objects. I built and ran tests only in my own detached worktree, `/work/scratch/ch2b-opus-r2/wt`, whose target was reflinked from the implementer's; it is deleted at the end.

**Core:** `v24.0.0-rc.3` (`~/workspace/dash`).
- `DirectoryCommit` is at `src/util/fs_helpers.cpp:153-162` at this tag, not `util/system.cpp` as DEC-212's text says. It ignores both the `fopen` failure and the `fsync` failure.
- `FlatFileSeq::Flush` makes one `DirectoryCommit(m_dir)` (`src/flatfile.cpp:99`).
- `FlushBlockFile` (`node/blockstorage.cpp:588-610`) calls `Flush` for blk, then, unless finalizing without undo, for rev.
- `FlushStateToDisk` clears `m_check_for_pruning` after every automatic `FindFilesToPrune` (`validation.cpp:3016-3017`).

**Verdict:** no production-code defect. The node's behaviour under DEC-211 and DEC-212 is correct and Core-equivalent. One Medium finding is in the *checker*: DEC-212's entitlement is wider than the decision says, and I confirmed it with a probe. It needs a small fix before the checker relaxation is trusted, but it doesn't block the production commits. The rest are Low or Nit.

## Findings

### 1. Medium: DEC-212's entitlement also covers names whose pending op was a **remove** or a rename **source**, and a later *successful* sync never clears it, so a reference into a deleted file passes D1(a) and R1

- **Where:** `dashd-store/src/faulty/fs.rs`
  - `:2176-2178`: on failure, `pending_names(dir)` (`:1525-1531`) adds *every* name changed by a pending op, including `(name, None)` removals and both names of a rename.
  - `:784-787`: only a later op on the name removes it. A later successful `sync_dir` (`:2180-2184`) leaves the set alone.
  - `:1492`: D1(a)'s `Some(None) if entitled => None` then leaves `candidates` empty, and `range_durability` returns `Durable` vacuously.
  - `:1535-1537`: `lost_to_failed_dir_sync` is true whenever the name is in the set and the *live* lookup is `None`. That holds after a remove with no crash at all, and after a process kill.
  - The R1 view then accepts any read error: `crash/chain.rs:1659` (`Err(_) if lost`) and `toy/mod.rs:2283ff` (`Ok(None) if lost`).
- **DEC-212's text:** it entitles "a durable reference into a file whose directory entry a failed sync-dir left pending". A removed name has no file. Core's exposure is a *create* (or rename target) that power loss undoes. A remove that power loss undoes resurrects a file and never loses one.
- **Probe** (added to my worktree's `tests/power_loss_checkers.rs`, run with `--features faulty`):
  1. `synced(/dd/n)` (sync_dir #0 OK);
  2. `remove(/dd/n)`;
  3. sync_dir #1 fails;
  4. sync_dir #2 OK;
  5. `rec(/dd/n, 512)` (a Durable index record referencing the deleted file).
  - Result: `d1=[]`, `lost_to_failed_dir_sync=true` with no crash, and still true after a `ProcessKill` boot.
  - Control without the failed sync: D1(a) "its name is not durable".
  - The rename source (`/dd/a` after `rename(a, b)`, failed sync, OK sync, `rec(/dd/a)`) gives the same `d1=[]`.
  - Without step 4, D1(a) still fires ("a pending operation unbinds its name"), so the hole needs the later successful sync. That is exactly the node's sequence after a failed step-12 unlink sync, because the next files barrier's `Flush` syncs `blocks/`.
- **Failure scenario:**
  1. A random-fault run (`harness.rs:2148ff` `pick_fault` offers `SyncDir`/`Applied`) fails the step-12 unlink's `sync_dir` after file f's blk and rev are removed.
  2. The next barrier syncs `blocks/`.
  3. A T-bug (or real regression) then writes a stale `HAVE_DATA`/`HAVE_UNDO` record into f, for example a cleared record overwritten by an older lent copy.
  4. D1(a) is silent, and R1's view form calls the missing file "lost to a failed dir sync".
- **What still catches it:** today, only R1's position check catches this, and only because the prune also zeroes f's info. A bug that also keeps or rewrites f's info is invisible. The no-fault branch of the same scenario still catches the T-bug, so this is reduced coverage in the faulted subspace, not a blind spot everywhere. That's why it's Medium, not High.
- **Fix:**
  1. At failure, record only names whose **newest pending change binds a file**. That means `Some(Entry::File(ino))`, stored as `path → ino`.
  2. Drop the directory's entries when a later `sync_dir` of that directory succeeds, because the create is then durable and nothing can be lost.
  3. Keep "any later op on the name clears it".
  4. Optionally have `lost_to_failed_dir_sync` require that the recorded inode is what the pre-crash binding named.
  5. Add the three probes above as `dec212_*` negatives. Remove/OK-sync and rename-source must give D1(a), and `!lost_to_failed_dir_sync`.

### 2. Low: the entitlement doesn't cover a pending entry of `blocks/` *itself*, so a failed first sync gives a D1(a) false positive for Core-equivalent behaviour

- **Where:**
  - `dashd-store/src/faulty/fs.rs:1540-1559` (`dir_durability` has no DEC-212 arm);
  - `dashd-store/src/flatfile/seq.rs:248-259`. `sync_dir`'s `?` on `blocks/` returns before the parent sync, so a failed `blocks/` sync also skips the datadir's, and `parent_unsynced` stays set.
- **Core:** `DirectoryCommit(m_dir)` only syncs `blocks/`. Core never syncs the datadir's entry for `blocks/` at all, so losing `blocks/` with its files is Core's exposure too.
- **Probe:** `create_dir_all(/dd/sub)`, write and `sync_data` `/dd/sub/f`, then sync_dir(`/dd/sub`) fails, then `rec(/dd/sub/f)`. Result: D1(a) "/dd/sub has a pending operation".
- **Scenario:** CRASH's `/dd/blocks` is created lazily by the first `Allocate` (`seq.rs:186`). A random fault on the first FS `SyncDir` (pick_fault) makes the node warn and continue, as DEC-212 requires, and the next index write reports D1(a). Before DEC-212 this was fatal, so no index write followed. The false positive is new with this commit.
- **Fix:** either one of these:
  - entitle a pending *directory* entry whose child directory's (or its own parent's) sync failed, mirroring finding 1's create-only rule, in `dir_durability`;
  - or make `seq.sync_dir` attempt the parent independently of the child's result (still one attempt each), and entitle the parent's pending `blocks` entry when it fails.
- Add a `dec212_*` case with a fresh directory.

### 3. Nit: S5d infers the decline from an *absent* `PruneSelected`. It can't see `PruneDeclined`, which the CRASH observer drops.

- **Where:**
  - `dashd-chainstate/tests/crash.rs:202-209`;
  - `dashd-chainstate/src/crash/world.rs:161-163` (`ObsMark::PruneDeclined { .. } => return`).
- **Scenario:** a regression that makes flush 2's step-7 rerun select nothing, for example in `rerun_selector` at the rewound tip, never sends `PruneClears`. It still yields `selection_flushes == [3]` and bound `(122, false)`. S5d would pass without exercising V's decline.
- **Fix:** forward `PruneDeclined { flush, files }` as a trace mark that the checkers ignore, and assert `[(2, [0])]` in S5d. Alternatively, count `FlushStep::PruneApply` steps in flush 2.
- The decline itself goes through the real path. `FlushRewind` → `flush_during(…, rewind=true)` (`crash/chain.rs:744-772`) runs the real `FlushControl` over the `InlineDriver`, and `finish` services the clears through `FlushControl`'s `allows` check at `cx.tip`.

### 4. Nit (informational, no change): after a decline, the timing of S5d's re-armed prune is DEC-211's, not Core's sequential order

- Core's `DisconnectTip` → `FlushStateToDisk(IF_NEEDED)` at 409 would run `FindFilesToPrune` (bound 121, nothing selected) and then clear `m_check_for_pruning` (`validation.cpp:3017`). Reconnecting 410 writes no new block or undo bytes (410's undo position survives the disconnect), so Core would not prune at 410's reconnect. It would prune only when a later `Allocate` re-arms the check (`blockstorage.cpp:718/742`).
- Ours re-arms on the decline (DEC-211) and prunes file 0 in flush 3 at 410.
- Both end states are Core-reachable: Core pruning at 410 before the `invalidateblock` gives the same files and index. So this is a timing difference that DEC-211 chose.
- **S5d's bounds are right:**
  - At 410, `FindFilesToPrune`'s bound is `min(410, 410−288) = 122`.
  - At 409, it is 121.
  - File 0's `nHeightLast` = 122, which the "unrewound, V clears" control (`selection_flushes == [2]`) and the rewound `[3]` pin together.
  - S5d's target (`A{BUFFER+1}`) is past `nPruneAfterHeight`.
- Optional: one sentence in D-b2-9 saying "Core's sequential order would consume the check at 409; the re-arm prunes earlier, and the state is the prune-first linearization's".

### 5. Nit: `FlatFileSeq::dir_unsynced` is now write-only in production

- **Where:** `dashd-store/src/flatfile/seq.rs:80,116`; `blocks.rs:624`.
- After BL-124 every `Flush` syncs `blocks/` unconditionally, and the barrier no longer reads the flag. It is maintained in five places and read only under `#[cfg(test)]`. So n3's property ("the flag survives a failed sync, so the next barrier retries") is now true by construction: the next barrier's `Flush` always syncs. The flag no longer guards anything.
- **Fix (optional):** delete `dir_unsynced` and its test assertions, or document it as test-only observability. `parent_unsynced` is still load-bearing.

### 6. Nit: DEC-212's Core citation

- DECISIONS.md DEC-212 cites `util/system.cpp:153`. At `v24.0.0-rc.3` it is `src/util/fs_helpers.cpp:153`. The code comment (`seq.rs:211`) already has the right file. This is the manager's text.

## Focus 1: DEC-212's checker change (5091e64e5)

| Question | Answer |
|---|---|
| Entitlement = `ops[durable_len..]` names at the failed sync, cleared by a later op on the name? | Yes as written (`:2176-2178`, `:784-787`), but that set is **too wide**: it includes removals and rename sources, and a successful later sync doesn't end it (finding 1). |
| Failed sync **before** the create | Not entitled: the name isn't pending at the failure. Tested (`dec212_other…`, case 2). Correct. |
| Remove + recreate after the failure | The recreate's `push_dir_op` clears it. Tested (case 3). Correct. |
| Remove (pending) at the failure, then a successful sync | **Hole** (finding 1, probe 1). |
| Rename | The target: Core-like entitlement, fine. The source: **hole** (probe 3). make_private's rename + `sync_dir` still propagates its error and stops (stricter than Core, but make_private has no Core counterpart). Fine. |
| Two failures in one epoch | `extend`s the set. Fine. |
| Successful sync after the failure | For a create, the binding is durable, so the entitlement is moot (`durable_binding` is `Some(Some(File))`; `peek_lookup` is `Some`). For a remove or rename source it stays wrongly active (finding 1). |
| Unsynced data bytes | Still D1(a): the bound inode's bytes are judged (`:1494-1517`). Tested (case 4). Correct. |
| Process kill vs power loss | For a create, the name survives a kill, so it isn't `lost`. Tested. For a remove, `lost` is true after a kill (finding 1). |
| Across epochs | The set is carried into the booted world (the test asserts `lost` after boot). That's needed so a later recovery's R1 still accepts the reference. It's broader than DEC-212's "in that epoch", but Core keeps the dangling reference forever too. Acceptable once finding 1 restricts it to creates. |
| `fatal_kind` precedence | `*op != Allocate && (store != FS \|\| (best_effort == 0 && *op != SyncDir))` (`harness.rs:642-644`). The grouping is right: engine stores stay fatal except Allocate; FS sync_dir is never fatal; other FS mutations are fatal outside BestEffort. |
| Missing state | The fatal path that a failed dir sync *previously* produced, "abort before the index write", is now "write after a failed dir sync", and that's what the entitlement must cover. It covers files, not the directory itself (finding 2). |
| Tests `dec212_*` | Each one asserts what it names. They miss the remove/rename-source/later-OK-sync negatives (finding 1) and the fresh-directory positive (finding 2). |
| Mutants m1–m5 | The implementer's list isn't in any file I could find (not in the commit messages, `/work/scratch/ch2b/`, or `node-program/`). I couldn't audit it. My probes act as two surviving "mutants" of the *spec*: the current tests can't tell create-only entitlement from all-pending-names entitlement. |

## Focus 2: BL-124 (11037149f) against Core

- **One attempt per Flush:**
  - `seq.rs:227-232`: `flush` calls `sync_dir` once and warns on `Err`.
  - `sync_inner` (`blocks.rs:614-620`) is `flush_block_file_inner(false,false)` (blk `Flush` + rev `Flush`), then `sync_files` ×2 (data only). The second `sync_blocks_dir` call (the retry) is gone.
  - So there are exactly 2 `blocks/` fsyncs per barrier, matching Core's two `DirectoryCommit`s. The store test asserts `dir_syncs == 2` and 2 warnings.
  - The datadir parent fsync (`parent_unsynced`, once per sequence) is a pre-existing extra over Core. It's also one attempt, and it's skipped when the `blocks/` sync fails (see finding 2).
- **No retry anywhere:**
  - `grep sync_dir(` in `dashd-store/src` (non-test, non-faulty) gives `seq.rs:151` (make_private, fatal, non-Core), `:230` (flush), `:250/:254` (the `sync_dir` body), and `blocks.rs:630` (`sync_blocks_dir`, now called only by the step-12 unlink and the startup scan, both best-effort, one attempt).
  - There's no loop anywhere. "Retry" now only means the next barrier's own `Flush`, which Core does too.
- **fdatasync stays fatal:** `flush`'s `sync_data(..).map_err(io)?` (`seq.rs:228`) and `sync_files` (`:237-243`). The machine test injects only `fail_sync_dir`. `every_store_call_of_a_pruning_flush_can_fail` still sees PruneBarrier's `sync_data` failure abort with `Flushing block file to disk failed.` (reran: green).
- **Warning:** `target: "blockstorage"`, text `Failed to sync the blocks directory …` (`DIR_SYNC_FAILED`); the unlink path is prefixed `Prune: `. D-b2-11 records the extra line (`crash/mod.rs:207-213`). Core logs nothing (`fs_helpers.cpp:153-162`). Correct.
- **A1 §3.5 notes:**
  - The I1 sub-bullet and the §3.5.6 change ("ENOSPC or EIO from a write or `fdatasync` aborts; a failed directory `fsync` is logged…"; "A failed `fdatasync` is never retried") are accurate.
  - One nuance for the I1 bullet: its "a new file's entry" exposure also applies to the `blocks/` directory's own entry. Core never syncs the datadir; we sync it once and skip that on failure (finding 2).

## Focus 3: DEC-211 end to end, including S5d

- **W:**
  - `machine.rs` PruneSelect computes `highest = max(height_last)` over the selection from W's files view. That is correct because `eligible` restricts to files below `last_file`, whose `nHeightLast` V no longer moves.
  - PruneApply's `Declined` arm (non-X33) calls `declined()`, which re-arms, emits `PruneDeclined`, clears `selection`, and calls `after_selection`.
  - I confirmed there is no PruneBarrier, Bp or Unlink step on a decline, in one-batch and split (`a_declined_selection_prunes_nothing_and_rearms`; Sol nit 2's every-selected-file info snapshot is now in).
- **V:** `control.rs` checks `req.rule.allows(req.highest, cx.tip.height)` after `latch().check()?` and before `prune_clear`, so a decline mutates nothing.
- **`PruneRule::allows`:**
  - Its gate is `tip > after` (Core's `chain_tip_height <= nPruneAfterHeight` early return), together with `highest <= bound(tip)`.
  - The manual rule's bound is `last_prunable_manual(min(m, tip), tip)`.
  - Sol r2 already checked the wrap arithmetic. I re-checked the 287/288/289 and fastprune-100 cases in the unit table against `FindFilesToPrune` and `FindFilesToPruneManual`, and agree.
- **Subset argument:** "every selected file ≤ bound(tip′)" is sufficient for Core-reachability at the lower tip. Core's loop at tip′ walks files in order with usage ≥ the usage at `new`, so it would take at least the same prefix. Declining when *any* file exceeds it is the conservative DEC-211 rule; it doesn't prune the still-allowed prefix. That's a delay, not a divergence.
- **S5d exercises the real path:**
  - `D1` → `flush_during(s, 1, true)` steps the real `FlushControl`/`InlineDriver` to `Files`, then `disconnect_to(409)`, then `finish`, which answers `PruneClears` through `FlushControl` at V's working tip.
  - Its assertions match Core's selector at those tips (finding 4). It runs clean (see below).
  - Its weakness is finding 3 (the decline isn't asserted directly).
- **Manual decline:** unreachable in practice, because V waits in `WaitThenFreeze` for a manual prune, so its tip can't move. D-b2-9 says so. Fine.

## Focus 4: round-1 closure

| Item | Status |
|---|---|
| m1 | Closed by DEC-211 (above). |
| m2 | Closed. My r1 mutant M6 (Bp `Buffered`) is now killed by S5p's `writes == [Durable]` assertion (`tests/crash.rs:127-152`), plus its clean run from the unlink. The direct matrix `every_store_call_of_a_pruning_flush_can_fail` covers PruneBarrier/Bp (one-batch) and PruneBarrier/Bk (split), now with an exact `[StoreFailed{op}, Abort{message}]` report (Sol nit 1, da7e797d1). |
| n1 | Closed (`chain/index.rs:407-408`; `a_failed_prune_clear_changes_nothing`). |
| n3 | Closed at e5b07c1ec. BL-124 then made it moot (finding 5). |
| n4 | Closed (D-b2-10). |
| Sol r2 nits 1–3 | All three are fixed in da7e797d1 as described. The per-flush deadline is at `control/tests.rs:1248`. |

No regressions against my round-1 interleaving table. The new rows (decline before clears; decline + latch; X33) behave as Sol r2 describes.

## Focus 5: tests and CI cost

The PR CRASH run took 32.4 user CPU-s here, under 60. The nightly S5d took 6.3 user CPU-s (D-b2-8 says about 10). D-b2-8 lists the nightly set (S5, S5b, S5c, S14b, S5d) with its cargo command (`crash/mod.rs:193-198`).

## What I ran

Worktree `/work/scratch/ch2b-opus-r2/wt` at `da7e797d1`, `umask 022`, `CARGO_BUILD_JOBS=12`. Logs are in `/work/scratch/ch2b-opus-r2/logs/` (kept); the worktree itself is deleted. The load average was about 19–37.

| Suite | Result |
|---|---|
| `cargo test --locked -p dashd-store --features faulty` | rc 0: 326 passed, 0 failed (includes my probes, which print only) |
| `cargo test --locked -p dashd-chainstate --lib --features alloc-tag,crash,planted-bugs` | rc 0: 310/310 |
| PR CRASH: `cargo test --locked -p dashd-chainstate --features alloc-tag,crash --test crash` | rc 0: 35 passed, 5 ignored. **32.4 user + 2.6 sys CPU-s** for the test run (built beforehand with `--no-run`), 6.0 s wall. That's under the 60 CPU-s budget. The implementer's 53.3 and 51.4 probably include other overhead. |
| Nightly S5d: `-- --ignored s5d` | rc 0: 1 passed, 6.3 user CPU-s |
| `cargo test --locked -p dashd-chainstate --features alloc-tag,crash,planted-bugs --test crash_planted` | rc 0: 46 passed, 1 ignored |

**Mutants** (each applied alone and restored with `cp -p` + `touch`; `git diff` afterwards showed only my probe file):

| # | Mutant | Result |
|---|---|---|
| R1 | `harness.rs:644`: SyncDir counted as fatal again (DEC-212's R8 change reverted) | **Killed**: `power_loss` `n1_s9_eio_per_call_and_recovery_reads`; CRASH `s9_c7_eio_and_read_faults`, `coins_replay_blind_smoke` |
| R2 | `blocks.rs:618`: the barrier's extra `sync_blocks_dir(FLUSH_BLOCK_FAILED)?` restored (BL-124's retry, fatal) | **Killed**: `a_failed_directory_sync_at_the_barrier_is_logged_and_not_retried`, `sync_covers_every_written_file_and_the_directory`, `a_failed_directory_sync_at_the_files_barrier_is_logged_and_the_flush_completes` |

So the R8 relaxation and the no-retry rule are both pinned. What isn't pinned is the *width* of the entitlement (finding 1).

**Probes:** four extra tests appended to my copy of `dashd-store/tests/power_loss_checkers.rs`. They print and don't assert; they were never committed.
- `opus_r2_probe_removed_name_after_failed_then_ok_sync` → `d1=[] lost=true`; after a kill, `lost=true` (finding 1).
- `opus_r2_probe_removed_name_after_failed_sync` → D1(a) "a pending operation unbinds its name" (the hole needs the later OK sync).
- `opus_r2_probe_rename_source` → `d1=[] lost=true` (finding 1).
- `opus_r2_control_removed_name_no_failed_sync` → D1(a) "its name is not durable", `lost=false`.
- `opus_r2_probe_new_directory` → D1(a) "/dd/sub has a pending operation" (finding 2).

**Not done:** no gate copy, no PF0, and no live Core run. The Core side comes from source at the tag.
