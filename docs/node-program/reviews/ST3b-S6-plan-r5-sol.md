# ST3b S6 rev4 narrow re-review r5 (Sol)

Reviewed `REVIEW-COMMON.md` first, then rev4 §3c/§11, rev3, r4's findings, and the uncommitted sources in `/work/home/agent/workspace/rdc-wt/st3b`. Correctness only. No findings closed in r1–r3 are reopened.

**M4 — OPEN (major).** A retry of the actual production operation can still preserve the injected error and pass the seam assertions and the new boundary test. For example, in `OsFs::sync_data` (`dashd-store/src/flatfile/fs.rs:270–274`), replace the propagation with:

```rust
if let Err(first) = einval_is_ok(self.fdatasync(&file)) {
    let _ = File::sync_data(&file);
    return Err(first);
}
```

The seam sees one entry, the caller receives the first EIO, and `File::sync_data(&file)` matches none of the boundary's forbidden strings. This is a retry of the same operation, in the same production file, within rev4's explicit scope. In a scratch copy, after correcting the independent false positive described in N4, the boundary and both OsFs tests passed with this mutant; strace confirmed the additional `fdatasync`. Thus the source-boundary option proposed in r4 has not yet been enforced soundly.

**N2 — OPEN (major).** The production-call proof remains incomplete for the same reason: `CallSeam::enter` (`dashd-store/src/seam.rs:25–27`) counts helper entries, while an unobserved backend invocation can occur after that entry. A corresponding RocksDB mutant in `RocksEngine::sync_wal` (`dashd-store/src/rocks.rs:1071–1072`) also survives:

```rust
self.guarded(|| {
    let first = self.flush_wal();
    if first.is_err() {
        let _ = DB::flush_wal(&self.db, true);
    }
    first
})
```

It leaves all three RocksDB substring counts unchanged, performs the direct synced FlushWAL after the seam's error, and latches/returns that first error. The named RocksDB test and boundary test passed with this mutant. **The latched-engine sub-concern is CLOSED:** rev4 explicitly drops the inner-invocation-count claim, and `a_failed_first_sync_wal_latches` passes and supports the stated error-kind, barrier-site and later-refusal assertions.

**N3 — OPEN (major).** `each_seam_is_its_calls_only_path` (`dashd-store/src/seam.rs:33–52`) counts literal call-site spellings; it establishes neither that those are the only operation references nor that a permitted site executes once per seam entry. Besides the UFCS counterexamples above, two further mutants passed the boundary and all three production seam tests in the scratch copy:

- In the OsFs failure branch, call `self.sync_dir(path)` before returning `first`. Its existing `File::open(dir)?.sync_all()` site (`flatfile/fs.rs:277–278`) accepts the regular data-file path and makes an uncounted `fsync`. Strace with descriptor paths confirmed a successful `fsync` of the test's `f`, followed by the test's explicit later `fdatasync`. No backend call site was added.
- After the seam's early-return block in `RocksEngine::flush_wal` (`rocks.rs:463–470`), put the existing backend expression in `let attempt = || self.db.flush_wal(true).map_err(engine_error);`, call `attempt()`, and call it again if its result is an error, returning the first result. There is still exactly one backend call site and one helper invocation. A real RocksDB failure is retried, but the injected failure returns before this code, so the seam tests cannot detect the retry.

The scanner also does not parse/expand macros, and `production` assumes everything after the first test-module header is outside production, although Rust permits production items after that module. The two concrete cases above already disprove the claimed boundary without either technique. The explicit exclusion of syncs reached through other crates is clear, but does not cover these evasions: all are in the two named production files. Rev4's claim that **any** retry in that scope must fail a tier-1 test is therefore still too broad. Exact-spelling mutants 5 and 6 do not establish it.

**Fix for M4/N2/N3:** observe and inject failures at each actual backend attempt independently, or enforce a structural source boundary that includes operation references/call forms, permitted locations, and execution through the counted step. A source-only solution must also constrain the helper's body/control flow and reuse of existing uncounted sync paths; adding more substring spellings alone will not fix the single-site closure mutant. Keep the broad no-retry claim only if that boundary is actually enforced.

**N4 — NEW, major: the unmodified boundary test always fails on the existing Unix imports.** At `dashd-store/src/seam.rs:46–47`, `fs.contains("nix::")` matches `std::os::unix::fs`, `std::os::unix::io`, and `std::os::unix::ffi` already present in `flatfile/fs.rs:10`, `:11`, and `:319`. The unmodified `cargo test` run failed with `a sync around the seam: nix::`; the three production seam tests passed. Consequently rev4's new tier-1 acceptance cannot pass even before applying a mutant. **Fix:** recognize a `nix` path segment rather than a substring within `unix`, preferably as part of the structural check needed for N3, and establish a green unmutated baseline.

Validation used a owner-only reflink copy under `/work/scratch/st3b-r5-sol/tree`, its own copied `target/`, and `TMPDIR=/work/scratch/st3b-r5-sol/tmp`. `/work` had 249 GiB free before building, above the requested 200G threshold. The live worktree was not modified.

- Unmodified: `cargo test -p dashd-store --lib --all-features -- seam:: os_fs_reports_a_failed_fdatasync_once block_files_over_os_fs_latch_a_failed_sync a_failed_wal_sync_latches_and_is_never_retried`: **3 passed, boundary failed**. A standalone compilation of the exact boundary test reproduced the same failure.
- To isolate N3 from N4, only in the scratch copy, changed the forbidden `"nix::"` needle to `"use nix::"`. This is an experimental correction, not a proposed complete fix. Corrected-scanner baseline: **4 passed**.
- Corrected scanner plus the two UFCS retry mutants together: all four selected tests passed; the entire `dashd-store --lib --all-features` suite also passed (**79 passed, 1 ignored**). Both edited production files passed `rustfmt --check`.
- Corrected scanner plus the existing-site OsFs and RocksDB closure mutants: **4 passed**; both edited production files passed `rustfmt --check`.
- `cargo test -p dash-kv --lib --all-features a_failed_first_sync_wal_latches`: **1 passed**.

Logs, mutation diffs, syscall traces, original source copies and reviewed-source hashes are retained under `/work/scratch/st3b-r5-sol/`. No devices, node instances, commits, pushes or GitHub posts were used.

**Verdict: CHANGES-REQUIRED.**
