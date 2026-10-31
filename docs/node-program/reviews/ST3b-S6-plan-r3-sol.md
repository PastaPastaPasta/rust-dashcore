# ST3b S6 rev2 narrow re-review r3 (Sol)

Reviewed `/work/scratch/st3b/PLAN-ST3b-S6.md` rev2 against `S6-plan-r2-sol.md`, rev1, the owner rulings in §9, parent plan §§3.5–3.6, and the read-only `node/st3b-s6` sources. Correctness only; no worktree changes.

## Per-finding disposition

- **M4 — OPEN (major).** Narrowing the device-run claim to the parent acceptance (fail-stop and recovery) is sound and removes the r2 race/failed-retry false-positive claim. Moving no-drop/no-retry to seams is also the right kind of proof, but §3c currently cites seams/tests that do not establish the four required mutants.
  - `dashd-store/src/flatfile/tests.rs:390–401` exists and proves that `BlockFiles` latches after a failed sync and does not issue a later filesystem operation. However, its `TestFs` injects the error in `record` at :43–50, before `TestFs::sync_data` reaches `OsFs::sync_data` at :87–90. Therefore it cannot fail a mutant that makes the underlying `OsFs::sync_data` ignore its real `File::sync_data` error or retry that call. The same ordering applies to the flush-machine `SpaceFs` seam.
  - `dash-kv/src/latched.rs` has no test that makes the first `sync_wal` call fail and then verifies that a later call is refused/no engine call. Its cited tests inject a prior write failure (lines 290–319) or exercise the generic contract's failed `flush_memtables` barrier and then check `sync_wal` refusal. The analogous `dashd-chainstate/src/store/guarded.rs:633–654` test uses `MemoryEngine`, not RocksDB.
  - `dashd-store/src/faulty` contains the ST3a toy model (`faulty/toy`) over `dash_kv::faulty::FaultyEngine`; it has no `RocksEngine`/RocksDB `flush_wal` failure seam. Production `RocksEngine::sync_wal` is the direct `self.db.flush_wal(true)` call at `dashd-store/src/rocks.rs:1054–1056`, and the Rocks tests only exercise a successful sync (for example :1153). The flush-machine tests at :786–846 prove that the state machine stops after a ProbeEngine-injected store failure, not that a real RocksDB WAL-sync error is propagated once and never retried.
  - Consequently, the four §3c mutants are not presently tied to executable tier-1 tests: a RocksDB ignore/retry mutant has no path to inject an error at `flush_wal`, and the OsFs mutants bypass the cited injected failure. The conditional “if no test … S6 adds one” is not a concrete seam/acceptance definition.
  
  **Fix:** specify and implement a test-only seam at the actual `RocksEngine::sync_wal`/underlying `flush_wal` boundary that fails exactly one call, records invocation count, asserts the first error reaches the flush caller, and asserts subsequent calls are refused without another RocksDB call. Add an OsFs seam that fails the underlying sync after entry (and counts calls), plus a direct first-`sync_wal` failure test for the latched/guarded engine. Require each of the four mutants to fail a named tier-1 test; do not rely on the toy `faulty` model or a pre-engine ProbeEngine failure.

- **M5 — CLOSED.** Rev2 gives the targeted flush an upper boundary at the `Committed … to coin database` line, which is emitted after the final durable `engine.write(..., Commit::Durable)` (machine.rs:1411–1419); a causal abort remains the alternate end. DF-MID now attributes ENOSPC to a write-family call inside that interval and before the abort, and FL-WAL requires an actual failed `fsync`/`fdatasync` on a chainstate WAL, explicitly classifying WAL-write-only runs as coverage misses. Three coverage misses fail the case and behavior/recovery failures are not retried away. These changes close the prior boundary, late-operation, and append-only false-coverage scenarios.

- **m2 — CLOSED.** Rev2 includes the exact Core contexts, rdashd's actual `FlatFileError::Io` rendering as `<context>: <path>: <strerror> (os error <N>)`, and binds the path/errno to the observed fault. This matches `dashd-store/src/flatfile/error.rs:10–17` and the flush handoff at `dashd-chainstate/src/flush/machine.rs:243–248`. The requirement at §3a:40 to render a real `FlatFileError::Io` through the flush abort path prevents a matcher-only/invented-string test.

- **N1 — CLOSED.** Rev2 adds open/create and metadata calls, confirms all-thread attachment before arming, records effective injection time, parses unfinished/resumed calls by completion time, restricts failures to the faulted mount and post-injection interval, and excludes the documented advisory/tolerated/nonfatal categories. The failed-create, failed-allocation, failed-directory-sync, completion-order, final-batch, later-flush, and WAL-write-only controls directly cover the r2 counterexamples. The first-fatal-relevant-failure rule no longer treats every first traced error as causal.

## New findings

- **N2 — major: §3c's claimed RocksDB/OsFs seam coverage is not implementable from the cited paths as written.** This is the concrete new defect introduced by replacing the device proof: a review/implementation can report the four mutants while only exercising the in-memory toy/probe paths, leaving the production `flush_wal` and `OsFs::sync_data` error handling untested. The fix is the explicit underlying-call seams and invocation-count tests described under M4.

## Verdict

**CHANGES-REQUIRED.**


## Validation

Source-only verification was used for this plan review. Targeted cargo-test commands were attempted but blocked on the shared package-cache lock held by unrelated workspace jobs; no privileged devices or node instances were started, and no reference worktree files were modified.
