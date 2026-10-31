# BL-509 r2 review (Opus)

- Branch `node/bl509`, r1 head 5ccb13851 → new head 28d44ef00 (one signed commit, `G`).
- Scope: the fix diff `5ccb13851..28d44ef00` (4 files, +274/−69), against the r1 review (`BL-509-r1-opus.md`) and DEC-726.

**Verdict: ACCEPT-WITH-NITS (M 0, m 1, n 3).**

The marking now sits where DEC-726 put it. `RocksEngine::open` calls `mark_store` right after `classify_uncreated` accepts a directory without `CURRENT`, before `DB::open_cf_descriptors` (`dashd-store/src/rocks.rs:200-201`). So every directory the store creates a database in is marked, including `lost+found` and pre-mark debris, and no refused directory is written. The three syncs are ordered correctly and pinned through the `FlatFs` seam. I found no crash point between `classify_uncreated` and RocksDB's first file whose next start is refused. I also found no way for the marker to widen what create-if-missing overwrites.

The one minor gap is test coverage, not ordering: nothing tests that a failed marking fails the open (m-1). The ST3b pre-phase rerun that DEC-726 requires as close-out evidence is still outstanding. It isn't part of this commit and stays a READY condition.

---

## r1 findings

| r1 | Status | Where | Note |
|---|---|---|---|
| M-1 (`lost+found` dir unmarked) | **FIXED** | `rocks.rs:200-201` (call), `rocks.rs:548-560` (`mark_store`); tests `rocks.rs:1425` `the_open_marks_every_store_it_creates` (`lost+found` case), `datadir.rs:509` `every_chainstate_the_store_creates_is_marked` | The marking runs on exactly the set `classify_uncreated` accepts. The datadir test opens a `lost+found` chainstate for real, strips `OPTIONS-*`, asserts the probe says `None`, then removes the marker and asserts `LevelDb`, so it is sensitive in both directions. M10 (5ccb13851's empty-only rule) dies. |
| m-1 (pre-mark debris unmarked) | **FIXED** | same call site; "pre-mark debris" cases in both tests above | `LOG LOCK IDENTITY` without a marker is marked, then created over. The r1 docs that called debris "the store's open to judge" went with `mark_new_store`. The `refuse_core_dir` doc is rewritten (`datadir.rs:313-319`). |
| m-2 (sync order untested) | **FIXED** (tier 1); ST3b evidence **outstanding** | `rocks.rs:1370` `the_mark_is_synced_before_the_store_is_created`, Recorder `rocks.rs:1281-1362` | The exact op sequence `create_dir_all, sync_dir(parent), create, write, sync_data, sync_dir(store)`, the keep path (only `sync_dir(store)`), and a failure at each of the 6 steps stopping everything after it are all asserted. M6/M7/M9 (each sync dropped) and M8 (no sync on the keep path) die (`/work/scratch/nd1-bl509/probe/mutants.log`). DEC-726's ST3b pre-phase rerun (ext4 + xfs, plus one ext4 mounted at `chainstate/`) is still a close-out item. |
| n-1 (124-char doc line) | **FIXED** | `rocks.rs:182-188`, `:594-599` | No added line exceeds 100 characters. |
| n-2 (`.ldb` beats the marker at the BL-147(b) merge) | **DEFERRED** (by DEC-726, to the merge) | `datadir.rs:306-310` unchanged | Still applies when BL-147(b) merges. Nothing at this head contradicts it. |
| n-3 (existing unsynced marker skips syncs) | **FIXED** (better than asked) | `rocks.rs:559` (unconditional `fs.sync_dir(path)`), PLAN D1 | DEC-726 asked only for a D-row. The code now re-syncs the store dir whenever no `CURRENT` exists, so it no longer relies on RocksDB's `SetCurrentFile` fsync or journal ordering. D1 says so, and correctly notes that the kept marker's *bytes* may stay unsynced, which is harmless because matching is by name. |

---

## Check 2: crash ordering on the new placement

The order is `classify_uncreated` → `fs.len(marker)` → [no marker: `create_dir_all` → `sync_dir(parent)` → `create` → `write` → `sync_data`] → `sync_dir(store)` → `DB::open_cf_descriptors` (RocksDB's first file).

| Crash after… | What the next start sees | Accepted? |
|---|---|---|
| `classify_uncreated` (nothing written) | the directory as it was (missing, empty, `lost+found`, or debris) | Yes. Same classification, marked this time. |
| `create_dir_all`, before the parent sync | the store dir absent, or present and empty (power loss). After a kill it is present. | Yes. Fresh state. |
| parent sync, before `create` | an empty store dir, or `lost+found`/debris | Yes. |
| `create`/`write`, before `sync_data` | power loss: no marker, or a zero-length `RDASHD`. Kill: the marker, possibly empty. | Yes. The probe (`datadir.rs:306`) and `is_creation_debris` (`rocks.rs:582`) match by name. The keep path then re-syncs the dir (D1). |
| `sync_data`, before `sync_dir(store)` | the marker's data durable, its entry possibly not, so either no marker or a full one | Yes. No RocksDB file exists yet, so an unmarked store dir is still in the fresh or debris class. |
| `sync_dir(store)`, then any subset of RocksDB's files | `RDASHD` durable, so `is_leveldb` is false whatever RocksDB left | Yes. Without `CURRENT`, the store creates over the debris (the marker counts as debris). With `CURRENT`, `inspect_existing` decides. |

- **Is the parent sync done before the marker can matter?** Yes. `sync_dir(parent)` (`rocks.rs:552-554`) precedes `create(marker)`, and the marker only matters once a RocksDB file exists, which is after `sync_dir(store)`.
  - On the keep path the parent is not re-synced. That's sound: a marker that exists was created by a run that had already completed the parent sync (`create` comes after it in program order, and a kill doesn't undo page-cache state).
  - The only other way a marker appears is the old 5ccb13851 binary's `mark_new_store`, which also synced the parent first, or a test helper.
- **Multi-level `create_dir_all`:** only the immediate parent is synced (D3). If an ancestor's entry is lost, the subtree is absent, which is the fresh state. Agreed.
- **"Kept marker, dir re-synced on every open":** this runs only in the `!has_current` branch (`rocks.rs:199-201`), so a normal open of an existing database does no extra work and adds no fsync.
  - On a creation open, the cost is one directory fsync (keep path) or three fsyncs plus one create (new path), next to RocksDB's own creation fsyncs.
  - Read-only filesystem: a creation open there fails anyway, because RocksDB must write. With no marker, the open now fails at `create` with EROFS, reported as `<store>/RDASHD: Read-only file system` (kind `Io`). With a marker, `OsFs::sync_dir` opens the dir `O_RDONLY` (`fs.rs:260-261`), and fsync on a read-only-mounted ext4/xfs returns 0, so the open falls through to RocksDB's own error. Either way no open that used to succeed now fails.
  - Full filesystem: `create` may hit ENOSPC/EDQUOT, which `io_error` maps to `NoSpace` (`rocks.rs:528-533`), the same kind RocksDB's failure would give. `sync_dir` allocates nothing.
  - `OsFs::sync_dir` error handling: `File::open(dir)?` propagates real errors, and `einval_is_ok` treats EINVAL as success only on filesystems without directory fsync (D2, unchanged from r1, the same trade-off as every rdashd dir sync). `sync_data` syncs the cached writable handle that `write_all_at` wrote through (`fs.rs:253-257`, `handle(path,false)` returns the writable entry), so the right descriptor is synced.

## Check 3: false positives

`classify_uncreated` (`rocks.rs:600-633`) accepts a directory only if it is:
- missing;
- empty;
- or holds nothing but `LOCK`, `LOG`, `LOG.old.*`, `IDENTITY`, `OPTIONS-*`, `MANIFEST-*`, `*.dbtmp`, `RDASHD`, and a *directory* named `lost+found`.

Any numbered `.sst/.log/.ldb/.blob` file returns `Corruption`, and any other name returns `UnknownDatabase`, both before `mark_store` runs (M5 dies).

- **Real database, ours or Core's:** a LevelDB or RocksDB that was ever opened after creation has a numbered `.log` (or `.ldb`/`.sst`) next to it, so it is never in the accepted class. With `CURRENT` present, the marking branch isn't taken at all. A Core LevelDB without `CURRENT` and without data files is LevelDB's own early creation debris (`MANIFEST-000001`, `LOG`, `LOCK`). Marking and overwriting it loses nothing. Also, a Core datadir is refused by `probe_core_data_dir` (evodb/llmq first) before `open_node` runs.
- **`lost+found`-only:** this is the intended case. It is marked, which is correct, because the store is about to create there.
- **Pre-fix leftovers:** the debris class, marked and then created over, which is m-1's intended behaviour.
- **Does a stray marker beside a real DB widen create-if-missing?** No.
  - With `CURRENT`: `inspect_existing` decides, and the marker is ignored.
  - Without `CURRENT`: data files return `Corruption` whether or not the marker exists. The marker only adds itself to the debris set (as at r1).
  - The only effect of a stray marker is on the *probe* side (a Core LevelDB next to `RDASHD` stops looking like LevelDB). That is r1 n-2, caught by evodb today and by the `.ldb` rule at the BL-147(b) merge.
- **`blocks/index`:** no `RocksEngine::open` caller targets it (grepped every caller). It is never marked.

## Check 4: other store users

- **Callers** (grepped): `open_node` via `RocksTuning::open_chainstate` (`tuning.rs:206`), the bake-off (`dashd-store-bakeoff/src/store.rs:216`, `proxy.rs:682/728`, including `txindex/`), the `coins_flush` benches, `evo/testkit.rs`, `recovery/tests.rs`, `node-fuzz` `kv_model.rs`, and the dashd-store integration tests. All of them now get an `RDASHD` file on creation.
  - Nothing among them lists a store directory and expects an exact set. `creates_over_the_debris_of_a_failed_first_creation` copies only debris, and the marker is debris.
  - The cost (≤ 3 fsyncs + 1 create, once per created store) is acceptable. Wall-clock benches that create a store per iteration will carry it, but those are noise on this box anyway. PF0 counts `instructions:u`/allocations, and the marking adds a handful of path allocations per creation (see n-3 below).
- **Relative path:** `parent_dir` (`rocks.rs:564-572`) maps `Path::parent() == Some("")` to `.`. That is correct for `"store"` and `"store/"`, both asserted (`rocks.rs:1398-1401`), and M12 dies.
  - `"/"` returns `None`, which is right: a root has no parent entry.
  - `"."` and `".."` also map to `.`, which is not their real parent. But such a store dir exists before the open (it is the cwd or above), so its own entry is not this open's to make durable. Harmless.
  - The node always passes absolute paths (`abs_path_for_config_val`), so only the bake-off and tests can reach the relative case.

## Check 5: mutants

The implementer's M1, M2 and M4–M12 all die per `probe/mutants.log` (there is no M3; the numbering skips it). I agree each one tests what its name says.

**Surviving cheap mutant (m-1):** `rocks.rs:201` `mark_store(…).map_err(…)?;` → `let _ = mark_store(&OsFs::new(), path);`. See m-1 for the run.

---

## New findings

### m-1: nothing tests that a failed marking fails the open; ignoring the error survives

**Where:** `dashd-store/src/rocks.rs:201`.

The Recorder test (`rocks.rs:1393-1397`) proves that a failing step stops `mark_store`. No test proves that `RocksEngine::open` *propagates* that failure.
- `the_store_is_marked_before_rocksdb_opens_it` (`:1407`) fails in RocksDB, not in the marking.
- Every other open test succeeds at marking.

So the mutant `let _ = mark_store(&OsFs::new(), path);` passes every test the implementer's harness runs (I ran it: 28/28 + 13/13). It also passes the "error text names `<store>/RDASHD`" claim, which is untested.

**Why it matters:** this `?` is what makes "no unmarked store is ever created" hold. Without it, any marking failure that RocksDB doesn't share silently reopens the BL-509 window for that store. Examples: a failed `sync_dir` (EIO), or a `create` that fails where RocksDB's later writes don't.

**Concrete input:** a store dir holding a dangling symlink `RDASHD -> missing/RDASHD`.
1. `classify_uncreated` accepts it (debris by name).
2. `fs.len` follows the link and gets NotFound, so `None`.
3. `create` follows the link into a missing directory and fails with ENOENT.
4. RocksDB, which ignores the unparsable name, would create the database.

With the head, the open fails with `Io` and a message naming `<store>/RDASHD`. With the mutant, it returns `Created` and no marker file exists.

**Fix:** add a small test. This exact body is verified in the review copy: it passes on the head and fails under the mutant.

```rust
#[test]
fn a_failed_marking_fails_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join(STORE_MARKER);
    std::os::unix::fs::symlink(dir.path().join("missing").join(STORE_MARKER), &marker).unwrap();
    let err = RocksEngine::open(dir.path(), &SCHEMA, &RocksConfig::default()).err().unwrap();
    assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
    assert!(err.to_string().contains(&marker.display().to_string()), "{err}");
    assert!(!dir.path().join("CURRENT").exists());
}
```

Then add the mutant to `mutants.sh` (M13). The message assertion also kills `io_error(path, e)` in place of `io_error(&path.join(STORE_MARKER), e)`.

### n-1: `mark_store`'s doc overstates what is kept and synced

**Where:** `rocks.rs:543-547`.

"A mark already there … is kept, and its entry synced again, so no crash after this returns leaves the database's files without it." That is true. But it reads as if the kept marker is fully durable, and on the keep path its bytes are never synced (`sync_data` is skipped). D1 says this correctly. Add "(its bytes need not be: the mark is told by name)" to the doc, so a later reader doesn't "fix" it by re-adding `sync_data` or start reading the contents.

### n-2: the keep path accepts a non-file `RDASHD`

**Where:** `rocks.rs:550`.

`fs.len(&marker)?.is_none()` is false for a *directory* named `RDASHD`, so a directory takes the keep path. `classify_uncreated` treats it as debris by name, and the probe as a marker. Nothing produces this, and the outcome is benign: the store creates, the probe calls it rdashd's, and the "marker" is a directory.

Not worth code. If you touch the line anyway, `std::fs::symlink_metadata(&marker).map(|m| m.is_file())` would make the keep path exact and would also turn m-1's dangling-symlink input into a clean rewrite. Optional.

### n-3: expect small PF0 alloc deltas on rows that create a store

**Where:** `rocks.rs:201`, which affects every `RocksEngine::open` creation, not only `open_node` as at r1.

The marking allocates a few `PathBuf`s and an `OsFs` handle-map entry per created store. Rows whose measured region creates a RocksDB (node-startup-genesis, and any `coins_flush`/bake-off row that opens a fresh store inside the timed loop) may move by a few hundred bytes of allocation. The instruction change is negligible, since the fsyncs are kernel time.

Nothing to fix. Mention it in the train's PF0 bench-change note, so a DRIFT on those rows isn't chased as a code regression.

---

## What I checked and ran

- Read:
  - the fix diff in full;
  - `RocksEngine::open`, `mark_store`, `parent_dir`, `is_creation_debris`, `is_database_data`, `classify_uncreated` and the new tests;
  - `OsFs` (`create`, `write_all_at`, `sync_data`, `sync_dir`, `len`, `create_dir_all`, `einval_is_ok`) in `dashd-store/src/flatfile/fs.rs`;
  - `datadir.rs` `probe_core_data_dir` and `is_leveldb`, and the rewritten tests;
  - PLAN.md D1–D4;
  - `probe/mutants.sh` and `mutants.log` (M1, M2, M4–M12 all fail at least one test).
- Grepped every `RocksEngine::open`/`open_chainstate` caller and every test that chmods or touches `STORE_MARKER`.
- Built in a reflink copy, `/work/scratch/bl509-r2-opus/src`, with its target seeded by reflink from the implementer's probe target, CARGO_BUILD_JOBS=8, nice 19, TMPDIR inside the copy. Ran the implementer's harness subset (`dashd-store --lib rocks::` plus `dashd-node --features test-support,test-hooks --lib datadir:: node::tests::{starts_and_stops_cleanly,a_chainstate_cut_off}`) with the m-1 mutant.
  - Result: **survives**. `rocks::` passed 28/28 and the dashd-node subset passed 13/13, 1 ignored (`/work/scratch/bl509-r2-opus/mutant-ignore-err.log`).
  - Then I added the m-1 test as `r2_probe_a_failed_marking_fails_the_open`, in the copy only. With the mutant it FAILS: the open returns `Ok`, so `.err().unwrap()` panics. With the head code restored it passes.
  - The copy now holds head code plus that probe test. It is left in place.
- Built nothing in the worktree and changed nothing there. No ST3b image was touched.
