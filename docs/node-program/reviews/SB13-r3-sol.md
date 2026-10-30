# SB13 SQLite store + migrator v3 — Sol round 3

Reviewed `node/sb13-spv-sqlite` at `7544afe1e` in a detached scratch worktree. The branch under review was not changed.

## Findings

No findings in the requested narrow scope. The round-2 blocker is closed.

## Preservation and restart analysis

- I reran the exact round-2 public-path reproduction: a seven-file v2 fixture, `block_headers/` moved into `v2-unmigrated/`, the other five folders at the root, and `version.json` containing `not json`. `StorageMigrator::migrate` succeeded; all seven file digests matched in the aside and no v2 folder remained at the root. This was a temporary test in the scratch worktree, removed afterward. The committed `an_interrupted_set_aside_loses_no_folder` test also passed its 64 subsets crossed with an unreadable version and two opens per case, including the empty marker (`dash-spv/src/storage/migrator/v3.rs:682-728`).
- On the fallback path, `StorageMigrator::migrate` awaits `resume_set_aside` and propagates both task and I/O errors before entering the deletion loop (`dash-spv/src/storage/migrator/mod.rs:94-104`). `present` propagates lookup errors other than `NotFound`; `set_aside` propagates target lookup and rename errors (`dash-spv/src/storage/migrator/v3.rs:147-200`). A failure or crash mid-loop can therefore leave only a partial set of moves. No fallback deletion has started, and the unreadable version remains to route the next open through the same helper. Existing aside-name collisions take `.N` names, so retrying does not overwrite an earlier move.
- `set_aside` syncs the aside and then the storage root before returning (`v3.rs:199-200`). A sync error also stops the fallback before deletion. After successful syncs, all v2 folders have left the root; a crash during the later clear leaves the preserved aside for the next open. Migration and fallback both call this one helper (`v3.rs:110`, `mod.rs:100`), so their move and sync points are identical. I found no crash or I/O interleaving within the asked scope that deletes v2 data or makes the next open consume a partial root as v2.
- The unwritable-aside test is a real negative control on this non-root Linux host (`v3.rs:730-764`). With the aside at mode `0555`, the destination rename fails with `PermissionDenied` while root v2 folders remain deletable. I tried two temporary mutants: **A**, omit the fallback helper call; **B**, call it but discard its I/O error. The targeted test failed for both at its file-digest assertion: only the already moved header file survived (one of seven). Both mutants were removed.

## Validation

- `cargo test -p dash-spv --lib storage::migrator`: 9 passed.
- `cargo test -p dash-spv --lib sync::filters::manager::tests`: 61 passed.
- Temporary exact-reproduction test: 1 passed; the two mutant runs failed as expected.

Tests used this review's isolated Cargo target and `TMPDIR`. The scratch source was restored to a clean `7544afe1e` afterward. The tests model post-crash directory states; they do not simulate physical power loss. The no-marker fallback remains outside this round's scope (BL-186).

VERDICT: APPROVE
