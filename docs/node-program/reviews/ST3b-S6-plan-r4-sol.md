# ST3b S6 rev3 narrow re-review r4 (Sol)

Reviewed `/work/scratch/st3b/PLAN-ST3b-S6.md` rev3 §3c and §10 against `/work/scratch/st3b/reviews/S6-plan-r3-sol.md` and the read-only `node/st3b-s6` sources. Correctness only; no worktree changes.

## Per-finding disposition

- **M4 — OPEN (major).** The seam is on the normal production path, and the ordinary four edits that call the seam helper again would be caught. However, the tests do not prove that every underlying production call is observed. `OsFs::fdatasync` increments `sync_seam` and then calls `File::sync_data` (`dashd-store/src/flatfile/fs.rs:130-138`); `RocksEngine::flush_wal` does the analogous pre-call injection and count (`dashd-store/src/rocks.rs:461-470`). A retry mutant can therefore take the injected error, issue a second direct `file.sync_data()`/`self.db.flush_wal(true)` outside the helper, and return the original injected error. `os_fs_reports_a_failed_fdatasync_once` and `block_files_over_os_fs_latch_a_failed_sync` still see one seam entry and the original EIO, while `a_failed_wal_sync_latches_and_is_never_retried` still sees the expected seam count and latches the original error. The retry happened, but all named assertions pass. Thus the four mutants do not *necessarily* fail unless the plan narrows “retry” to “call the helper again,” which is not stated and does not establish the production-call invariant.

  **Fix:** make the acceptance observe/count the actual production operation (or use an independent syscall/RocksDB invocation monitor), and include a retry mutant that bypasses the helper while preserving the first error. Alternatively, define and enforce a source-level mutation boundary that makes every retry go through the seam, then state that restriction explicitly; the current claim that a production-call mutant cannot bypass the seam is too broad.

- **N2 — OPEN (major).** Rev3 makes the seams implementable and places them immediately before the normal `fdatasync`/`flush_wal` calls, but the one-shot pre-call seams are still not a complete production-call proof for the reason above. The production tests pass (`os_fs_reports_a_failed_fdatasync_once`, `block_files_over_os_fs_latch_a_failed_sync`, `a_failed_wal_sync_latches_and_is_never_retried`, and `a_failed_first_sync_wal_latches`), yet a direct second underlying call is invisible to `CallSeam::calls()` (`dashd-store/src/seam.rs:19-27`). The stated no-retry acceptance remains vulnerable to a surviving mutant.

  The latched-engine test also asserts only later error values/messages (`dash-kv/src/latched.rs:322-347`), not an inner invocation count; if the “without reaching the inner engine” part is retained, use a counting inner test engine and assert zero calls after the first latch.

## New findings

- **N3 — major: rev3's call-count seam is bypassable.** Scenario: after the armed seam returns its synthetic EIO, `sync_data` or `sync_wal` performs one direct underlying sync and returns the first error. The latch behavior and all four named test assertions remain green, despite a retry. Fix as described for M4: instrument the actual operation independently, or explicitly constrain and test the mutation shape so retries cannot bypass the seam.

## Verdict

**CHANGES-REQUIRED.**

## Validation

The four named tests passed on the unmutated review worktree:

- `cargo test -p dashd-store --all-features os_fs_reports_a_failed_fdatasync_once`
- `cargo test -p dashd-store --all-features block_files_over_os_fs_latch_a_failed_sync`
- `cargo test -p dashd-store --all-features a_failed_wal_sync_latches_and_is_never_retried`
- `cargo test -p dash-kv --all-features a_failed_first_sync_wal_latches`

No privileged devices, node instances, commits, pushes, or worktree edits were made.
