APPROVE-WITH-NITS

# CH2b b2, round 3: narrow re-check of findings 1–3 (Opus)

**Range:** `722a565df..26b2b2506` (`node/ch2b-prune`, frozen), 8 commits, 13 files. `722a565df` is round 2's `da7e797d1` follow-merged with integration `cc940c818`. DEC-212's checker code moved with SB10 from `dashd-store/src/faulty/fs.rs` to `dash-kv/src/faulty/fs.rs`; the line numbers below are at `26b2b2506`.
- I read the range from git objects.
- I built and ran tests only in my own detached worktrees under `/work/scratch/ch2b-b2-opus-r3/` (`wt` for the suites, `wt-mut` for mutants and probes). Their targets were reflinked from the implementer's. `TMPDIR` was `/work/scratch/ch2b-b2-opus-r3/tmp` (mode 700). Both worktrees and their targets are deleted at the end.

**Core:** `v24.0.0-rc.3` (`~/workspace/dash`).
- `DirectoryCommit` (`src/util/fs_helpers.cpp:153-162`) syncs one directory and swallows both the `fopen` and the `fsync` failure.
- `FlatFileSeq::Flush` (`src/flatfile.cpp:82-102`) calls it once, on `m_dir` (`blocks/`).
- Nothing in Core syncs the datadir.

**Verdict:** findings 1–3 are fixed.
- **DEC-212's entitlement** now covers pending creates only. A later operation on the name, recreating an ancestor directory, or a successful sync ends it. The `dec212_*` tests pin every claim in the brief.
- **The two surviving mutants, V5 and V6,** are equivalent for every caller and sequence the model allows. I argue that below and give no counterexample.
- **The production change** (`seq.sync_dir`) can't change a Core-visible outcome. It only adds an `fsync` Core lacks, and the warning it can log has the prefix D-b2-11 already covers.
- **Two nits** remain:
  - One test gap: my mutant S1 survives every suite, but a five-line store test kills it.
  - One wording point about the warning's text.
- **One informational note for the manager** is outside the range.

## Findings

### 1. Nit: the "until **both** succeed" half of `parent_unsynced` isn't pinned for a datadir-only failure (mutant S1 survives)

- **Where:** `dashd-store/src/flatfile/seq.rs:248` (`if synced.is_ok() && parent_synced.is_ok()`).
- **Mutant S1:** `if synced.is_ok()`. With it, a sync where `blocks/` succeeds and the datadir fails clears `parent_unsynced`, so `blocks/`'s own entry is never synced again.
- **Result:** S1 survives `cargo test -p dashd-store --all-features`, `-p dashd-chainstate --lib --features alloc-tag,crash,planted-bugs`, and the CRASH PR set.
  - The store fixture's `fail_sync_dir` fails *every* directory sync, so no store test fails the parent alone.
  - `a_missing_blocks_directory_leaves_the_datadir_unsynced` covers the other half (`blocks/` fails, datadir succeeds). It kills S2, 9974e1e34's bug.
  - The checker can't see S1, and that's by design: the failed datadir sync records `/dd/blocks → Dir`, and with S1 no later datadir sync ever ends that record. So D1(a) stays silent and R1 entitles the loss.
- **Severity:** this is the Core-equivalent exposure (Core never syncs the datadir). S1 only gives up durability Core doesn't have, so it's a nit, not a parity defect. But D-b2-11, A1 I1 and the `sync_dir` doc comment all promise "until a sync of both succeeds", and nothing holds the code to that.
- **Fix:** add a store test. My probe, run in `wt-mut` and not committed, passes on the candidate and kills S1:
  ```rust
  let mut seq = FlatFileSeq::new(Arc::clone(&fx.fs), fx.dir.clone(), "blk", 1024);
  seq.write(0, 0, &[&[1]], "write").unwrap();
  fx.fs.take_ops();
  *fx.fs.fail_at.lock().unwrap() = Some(2);          // the parent's sync only
  assert!(seq.sync_dir(DIR_SYNC_FAILED).is_err());
  *fx.fs.fail_at.lock().unwrap() = None;
  fx.fs.take_ops();
  seq.sync_dir("sync").unwrap();
  assert_eq!(fx.fs.take_ops(), [Op::SyncDir("blocks".into()), parent]);
  ```

### 2. Nit: a datadir-only failure logs "Failed to sync the blocks directory: <datadir>: …"

- **Where:** `seq.rs:238-251`. `synced.and(parent_synced)` returns the parent's error when `blocks/` succeeded, wrapped in the `DIR_SYNC_FAILED` context (`seq.rs:14`). My probe printed `Failed to sync the blocks directory: /…/tmp/.tmpih67nb: Input/output error (os error 5)`, and the path it names is the datadir.
- **Parity:** none.
  - Core logs nothing in either case.
  - The line has the `Failed to sync the blocks directory` prefix that D-b2-11 (`crash/mod.rs:224-235`) already asks compat to allow.
  - There is still exactly one warning per failing `Flush`, as at round 2: when both fail, `blocks/`'s error is the one reported.
- **Fix (optional, docs only):** say in D-b2-11 that the line can name the datadir. Changing the text would need a new compat allowance, which isn't worth it.

### 3. Informational, outside the range (for the manager): Core refuses to start after losing a `HAVE_DATA` blk file; the node has no counterpart

- **Core:** `LoadBlockIndexDB` runs "Checking all blk files are present..." (`src/node/blockstorage.cpp:377-390`). It opens every file a `BLOCK_HAVE_DATA` entry names and returns `false` if one is missing. `LoadChainstate` then fails with "Error loading block database" (`src/node/chainstate.cpp:183-185`).
- **The node:** I found no such check in the node (`grep` over the tree for the log line, `setBlkDataFiles`, or a presence scan in `dashd-chainstate/src/recovery`).
- **Why it matters for DEC-212:** "Core's own exposure" to a blk file lost after a failed `DirectoryCommit` is a failed init in Core. In CRASH, R0 requires `Sut::open` to return `Ok`, and R1 entitles the dangling reference.
- **Scope:** this round didn't introduce it, and it isn't specific to DEC-212 (a missing blk file for any reason behaves the same way). It probably belongs to ND1/CH1-node's init order, next to `ScanAndUnlinkAlreadyPrunedFiles` (`designs/CH1-node.md:892`). It needs a BACKLOG row or a D-row. This isn't a security matter.

## Focus 1 (r2 finding 1): DEC-212 entitles pending creates only

The fix is 896c33a0b plus 621e6624c, in `dash-kv/src/faulty/fs.rs`:
- the record is a map `path → Entry` (`:669`);
- `pending_creates` (`:1539-1554`) builds it;
- it's stored at a failed `sync_dir` (`:2218`) and dropped at a successful one (`:2222-2225`);
- `push_dir_op` clears it (`:788-791`), and so does `create_dir_all`'s `retain` (`:2370`);
- the crash self-check is at `:1116-1123`;
- `lost_to_failed_dir_sync` (`:1561-1572`) walks the path from the root;
- D1(a)'s file arm is at `:1504`, and the `dir_durability` arm is at `:1590-1600`.

| Claim | Check |
|---|---|
| Removes are never recorded | `pending_creates` keeps a name only if its *newest* pending change is `Some(e)` and `durable_binding == Some(None)`; a `None` change `remove`s it. Tested by `dec212_removals…` (removal, with and without a later OK sync). Mutant K4 (a removal doesn't clear) is **killed** at `:623`. |
| Rename sources are never recorded | The source's change is `(a, None)`: removed. Tested ("a rename's source", with a later OK sync, which is r2's probe 3). |
| A create removed again isn't recorded | create → remove in one pending window: the newest change is `None`. Tested for both `later_sync` values. |
| A rename over a durable name isn't recorded | The target's `durable_binding` is `Some(Some(File))`, so the guard fails. Mutant K3 (drop the durable-binding guard) is **killed** at `:653` ("a durable name created again"), which is the same guard. |
| A durable name removed and created again isn't recorded | Same guard. Tested, including a power loss that keeps only the removal: `len == None`, `!lost`. |
| Ended by a later op on the name | `push_dir_op` clears it. K10 (no clear) is **killed** by the self-check at `:1121`. K10 with the self-check removed (K10+11) is **killed** by `dec212_other…`'s `!lost` at `:580`. So the self-check isn't the only guard. |
| Ended by recreating an ancestor | `retain(!p.starts_with(d))`. Mutant K8 (no retain) is **killed** at `:687`. |
| Ended by a later successful sync | The pending creates are dropped (`:2222-2225`). That is V5; see below. |
| The four negatives give D1(a) and `!lost`, also after a kill | `reported` asserts D1(a) and `!lost`, then `!lost` after a `ProcessKill` boot, for all four. D1(a) isn't re-asserted after the kill. That would add nothing: a kill keeps the pending ops `Present`, and none of the four names is ever recorded. |
| A durable reference into a create left pending is entitled before and after a crash that loses it, and not after a kill | `dec212_an_entry…`. The test also shows that a successful sync *after* the crash leaves the lost entry entitled (`:550-553`). That's right: the entry is no longer pending, and Core keeps the dangling reference. |
| The entitlement covers the entry, not the bytes | Unchanged; tested (`write(false)` → D1(a)). |

**The crash-time self-check vs r2's suggested inode check.** I agree with the implementer.
- `lost_to_failed_dir_sync` only returns `true` at an entry that is *unbound* after the crash, so there's no post-crash inode to compare with.
- The question r2's check asked was whether the entry the crash lost is the one recorded. The answer is fixed at crash time, and the self-check settles it there: any op on the name since the record either cleared it or would trip the assertion (K10).
- The only way a record can outlive its pending ops is if they became durable: through a successful sync (dropped), W4's `entry_on_file_sync` (the name is then durably bound to the recorded inode), or the power loss itself (the record is then what was lost).
- A query-time compare would therefore compare against nothing.

**V5 (no drop on a good sync): equivalent.**
- At a successful `sync_dir(dir)`, a recorded name of `dir` is in one of two states. Either it's still a pending create, so it's in `creates` and becomes durable with a `Present` bit, bound to the recorded entry. Or it's not pending, so the drop wouldn't touch it.
- So with V5, the only extra records left are for names durably and permanently bound to the recorded entry. `settle` only drops `Var` ops; ops made durable by a sync are `Present`, and the crash conversion skips ops below `durable_len`.
- Every consumer needs that name to become unbound or pending again:
  - D1(a)'s `Some(None)` arm (`:1504`);
  - `lost_to_failed_dir_sync`'s first unbound entry (`:1567`);
  - `dir_durability`'s `entitled` (`:1592-1600`);
  - the self-check's `pending_entries` (`:1120`).
- Becoming unbound or pending again takes an op on the name or a recreated ancestor, and both clear the record first.
- I found no sequence that tells V5 apart: create → failed → OK → kill or crash, W4 commits, the Dir arm with a later OK datadir sync, a rename onto the name. **SURVIVED** the checker tests and the CRASH PR set, as expected.

**V6 (the record checked at an undecided entry): equivalent for every caller.**
- `lost_to_failed_dir_sync` has two callers, and each runs it only *after* the read of the same path:
  - `crash/chain.rs:1370-1379`: `read` is evaluated before the `want` tuple that calls `lost`;
  - `faulty/toy/mod.rs:2309-2312`: `Ok(None) if lost(…)`.
- The read's `lookup` (`fs.rs:768`) goes through `dir_exists` (`:724-734`). That decides each component's binding from the root down, newest op first, and stops at the first missing one. `peek_binding` walks in the same order. So every entry `lost_to_failed_dir_sync` visits is decided.
- The two reads that fail before a lookup can't differ either:
  - An injected read fault (`read_fault`, `:2037`) is accepted by `io_failure`/`Unavailable` whatever `lost` says.
  - `read_sized`'s early `Corrupt` for `pos < 8` (`blocks.rs:881`) also raises R1's position violation (`chain.rs:1650`), so the kinds are the same.
- **SURVIVED**, as expected. It's a convention, not something enforced: the `World` method's doc says "Never decides", and a future caller that queries before reading would be safer with the current code than with V6. No change needed.

**My extra checker mutant K5** (D1(a)'s arm accepts a `Dir` record for a file path) **survives** and is equivalent: a referenced blk/rev path is never created as a directory.

## Focus 2 (r2 finding 2): the datadir sync, and `blocks/`'s own entry

- **Production** (c409ddab8, 9974e1e34):
  - `seq.sync_dir` (`seq.rs:238-251`) makes one attempt at `blocks/` and, while `parent_unsynced`, one at the datadir, each whatever the other returned. It reports `blocks/`'s error first.
  - `parent_unsynced` clears only when both succeed.
  - `flush` (`:218`) warns and goes on.
  - `sync_blocks_dir` (`blocks.rs:623-627`) marks the undo sequence synced only on `Ok`, which now means both succeeded.
- **The production bug 9974e1e34 fixes is real:**
  1. After a power loss lost `blocks/`, the startup scan's `sync_dir(blocks/)` fails with NotFound.
  2. The datadir sync succeeds.
  3. c409's version then cleared `parent_unsynced`.
  4. The entry `blocks/` gets from the next `create_dir_all` was then never synced.

  The checker would have flagged it: there's no failed sync, so D1(a) reports "/dd/blocks has a pending operation". `a_missing_blocks_directory_leaves_the_datadir_unsynced` pins it (S2 killed).
- **Against Core:**
  - Core's `Flush` syncs only `blocks/`. The datadir `fsync` is durability Core lacks.
  - Attempting it when `blocks/`'s sync failed adds an `fsync` call and nothing else. It can't abort, because no caller of `seq.sync_dir` propagates the error: `flush` warns, and unlink and scan warn with `Prune: `. `make_private`'s fatal `sync_dir` (`seq.rs:140`) is a separate call.
  - The number of warnings per failing `Flush` is still one, the text has the D-b2-11 prefix (finding 2), and `fdatasync` stays fatal.
  - I see no Core-visible outcome that changes.
- **Store tests:**
  - `a_failed_directory_sync_at_the_barrier_is_logged_and_not_retried` now counts 2 parent attempts (S3, which skips the parent after a `blocks/` failure, is killed) and checks that the datadir is synced once after success.
  - The datadir-only failure is the gap (finding 1).
- **Checker:**
  - The Dir arm (`fs.rs:1590-1600`) entitles a directory entry recorded as `Dir`. It skips the pending check and accepts `Some(None)`.
  - The record is create-only, by the same `pending_creates` rule.
  - `lost_to_failed_dir_sync` breaks at the first unbound entry from the root, so a file under a lost `blocks/` is entitled through `blocks/`'s record.
- **`dec212_a_fresh_directory_a_failed_sync_left_pending_is_entitled`** (`power_loss_checkers.rs:663-702`) covers:
  - both the "file's entry synced" and "file's sync failed too" cases;
  - a crash that loses the directory;
  - recreating it, which ends the entitlement through the `retain` (K8 killed) and the walk (K9, which checks only the leaf, killed);
  - a crash that keeps it;
  - the negative where only the child's sync was attempted.

  K6 (no `Some(None)` arm) and K7 (pending check always) are killed at `:678`. It's sound.
- **D-b2-11 and A1 I1 wording:** both are accurate.
  - A1 says "the node syncs it at each barrier". The datadir is also synced at a roll's `Flush` on V's side and at the step-12 and startup-scan `sync_blocks_dir`. "At each `Flush` and directory sync" would be exact; optional.
  - ST3a's R1 and D1(a) rows (`ST3a-power-loss.md:625`, `:699`) match the code. D1(a)'s row leaves out "recreating a directory above it", which R1's has; optional.

## Focus 3 (r2 finding 3): `Mark::PruneDeclined`

- **The mark:** it's new in `dash-kv/src/faulty/world.rs:228-233`, with its kind in `ALL_MARKS` (19). No checker matches on it.
- **The observer:** `crash/world.rs:160-169` forwards it with the trace's flush number, using the same `st.last` mapping as `PruneSelected`.
- **S5d** (`tests/crash.rs:204-211`) asserts `declines == [(2, [0])]`, and asserts `([], [2])` for the unrewound control.
- **Mutant P1** (the observer drops `PruneDeclined` again, r2's state) is **killed** at `crash.rs:204` by `--ignored s5d`. So r2's "regression that selects nothing at the rewound tip" now fails S5d.
- **The ST3a §3.2 listing** has the mark (`ST3a-power-loss.md:407-408`).

## Also

- **5e18aba7a (A-13):** it adds `NodeIndex::record(&self, BlockId) -> IndexWrite` (`dash-chain/src/chain/node_index.rs:98`), a read accessor that doesn't move an `Established` anchor. `contrib/node/check-public-api.sh` (cargo-public-api 0.52.0, nightly-2026-07-15) gives **rc 0, no diff**.
- **26b2b2506:**
  - It reflows D-b2-9 and restores the unrewound assert's message. It changes no logic.
  - The new D-b2-9 sentence (r2 nit 4) is right against Core. At 409, `FindFilesToPrune`'s bound is 121; file 0's `nHeightLast` is 122, so nothing is selected, and `m_check_for_pruning` is cleared (`validation.cpp:3016-3017`). "Prune-first linearization" describes the end state correctly.
  - It ties prune depth to nothing beyond block and undo files.
- **r2 nit 5 (`dir_unsynced`):** c5a2e9964 deletes it, along with its test assertions and the five writes.
  - No reference is left (`grep`). `mark_dir_unsynced` and `mark_dir_synced` now set only `parent_unsynced`, and their docs say so.
  - `adopt_existing_files` still marks the datadir unsynced. `blocks/` itself is synced by every `Flush` (BL-124).
  - `blocks.rs:609`'s "`blocks/` stays unsynced" is prose about the directory, not the flag. Fine.

## What I ran

Worktree `wt` at `26b2b2506`, `umask 022`, `CARGO_BUILD_JOBS=8 nice -n 10`, run as a `systemd-run --user` unit. The load average was 56–136. Logs are in `/work/scratch/ch2b-b2-opus-r3/logs/` (kept).

| Suite | Result |
|---|---|
| `cargo test --locked -p dash-kv --all-features` | rc 0: 18 passed |
| `cargo test --locked -p dashd-store --all-features` | rc 0: 309 passed, 20 ignored |
| `cargo test --locked -p dashd-chainstate --all-features` | rc 0: 402 passed, 6 ignored (includes the crash tests) |
| PR CRASH: `--features alloc-tag,crash --test crash` (built first with `--no-run`) | rc 0: 35 passed, 5 ignored. **54.5 user + 2.4 sys CPU-s**, 18 s wall at load ~100. That's within D-b2-8's 47–54 and under 60 CPU-s, but CPU-s inflate under this load: r2 measured 32.4 at load 19–37. |
| Nightly: `… --test crash -- --ignored` (S5, S5b, S5c, S14b, S5d) | rc 0: 5 passed, 107.8 user CPU-s |
| `--features alloc-tag,crash,planted-bugs --test crash_planted` | rc 0: 46 passed, 1 ignored |
| `cargo clippy --locked -p dash-kv -p dashd-store -p dashd-chainstate -p dash-chain --all-features --all-targets -- -D warnings` | rc 0 |
| `contrib/node/check-public-api.sh` | rc 0 |

**Mutants:** run in `wt-mut`, one at a time, applied by exact-string replacement and restored with `git checkout` + `touch`. `git diff --quiet` was checked after each. The checker set was `-p dashd-store --features faulty` and `-p dash-kv --all-features`.

| # | Mutant | Result |
|---|---|---|
| V5 | `fs.rs:2222-2225`: no drop on a good sync | SURVIVED checker + CRASH PR (equivalent, Focus 1) |
| V6 | `fs.rs:1568`: an undecided entry checks the record | SURVIVED checker + CRASH PR (equivalent for every caller, Focus 1) |
| K3 | `pending_creates` without the `durable_binding == Some(None)` guard | **Killed**: `dec212_removals…` `:653` |
| K4 | `pending_creates`: a removal doesn't clear an earlier create | **Killed**: `dec212_removals…` `:623` |
| K5 | D1(a)'s arm accepts any record kind | SURVIVED (equivalent: blk/rev paths are never directories) |
| K6 | `dir_durability` without `Some(None) if entitled` | **Killed**: `dec212_a_fresh_directory…` `:678` |
| K7 | `dir_durability`'s pending check ignores `entitled` | **Killed**: same, `:678` |
| K8 | `create_dir_all` without the `retain` | **Killed**: same, `:687` |
| K9 | `lost_to_failed_dir_sync` checks the leaf's record, not the break point's | **Killed**: same, `:681` |
| K10 | `push_dir_op` doesn't clear the record | **Killed**: the self-check `fs.rs:1121` |
| K11 | the self-check removed | SURVIVED checker + CRASH PR (an assertion, as expected) |
| K10+11 | both | **Killed**: `dec212_other…` `!lost` `:580` |
| S1 | `seq.rs:248`: clear when `blocks/` succeeded, whatever the datadir's result | **SURVIVED** store + chainstate lib + CRASH PR (finding 1). Killed by my probe. |
| S2 | `seq.rs:248`: clear when the datadir succeeded (c409's bug) | **Killed**: `a_missing_blocks_directory…` |
| S3 | skip the datadir after a `blocks/` failure (r2's `?`) | **Killed**: `a_missing_blocks_directory…`, `a_failed_directory_sync_at_the_barrier…` `:1066` |
| P1 | the observer drops `PruneDeclined` again | **Killed**: S5d `crash.rs:204` |

**Probe** (in `wt-mut`'s `flatfile/tests.rs`, not committed, reverted): `opus_r3_probe_a_failed_datadir_sync_alone_is_retried` (finding 1's test). It passes on the candidate and fails under S1. It also printed finding 2's text.

**Not done:** no gate copy, no PF0, and no live Core run. The Core side comes from source at the tag.
