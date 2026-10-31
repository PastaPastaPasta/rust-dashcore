# ST3b S6 rev1: narrow plan re-review r2 (Sol)

**Verdict: CHANGES-REQUIRED.** Four r1 findings CLOSED; three OPEN (two majors, one minor). One new major, N1. No blockers.

Read `REVIEW-COMMON.md` first. Reviewed `/work/scratch/st3b/PLAN-ST3b-S6.md` rev1 against rev0, `reviews/S6-plan-r1-sol.md`, the owner's §8 rulings, the approved parent plan §§3.5–3.6, the specified read-only harness/node/store sources, and Dash Core **v24.0.0-rc.3** through `git show`. These are findings about the plan, not an implementation review of the author's dirty files. Reference head: `294f84ea48a37050b70cc50387590ae0eec3cab8`, branch `node/st3b-s6`.

## Per-finding disposition

- **M1 — CLOSED.** Rev1 `PLAN-ST3b-S6.md:29` preserves the last-FLUSH prefix plus every later completed FUA before the exit mark, in log order, and takes that mark before repair/cleanup. This closes the specific impossible-loss counterexample `FLUSH; FUA; ack; exit`. `logwrites.rs:11–14` distinguishes these two durability mechanisms, and `image.rs` supports ordered extra entries. The revised exit boundary also excludes later repair/unmount writes. This closes the design finding; the implementation should still exercise the trailing-FUA regression requested in r1.
- **M2 — CLOSED.** Rev1 `:30–31` takes the latest actual successful durability response before exit, independently of the selected replay prefix, explicitly records the successful first half of a failed C2 pair, permits successes after arming, and applies the same acknowledgement floor to repaired-device recovery. It explicitly rejects the clean-stop floor and requires both counterexample tests. This repairs the inappropriate use of `campaign::floor` at the last FLUSH. The current baseline's mark-after-C2 behavior (`baseline.rs:144–152`, `checks.rs:87–98`) explains why the promised per-response recording is necessary.
- **M3 — CLOSED.** Rev1 `:27–28` requires natural exit 1 through an independent child waiter, uses the first causal abort for the no-`UpdateTip` rule, and allows the documented shutdown repeat reports. This matches `steps.rs:635–653,696–716` and Core `shutdown.cpp:26–37`. The remaining message-grammar problem is tracked under m2 below, rather than reopening the original exactly-one-line requirement.
- **M4 — OPEN (major).** Rev1 `:48–50,72` adds useful observation and mutant gates, but polling strace output does not ensure restoration before a retry or substitute failure, the direct guard permits retries that fail, and an XFS whole-device fault can still cause persistent filesystem failure. The plan therefore does not yet establish the promised independent immediate-propagation/retry discrimination. Details below.
- **M5 — OPEN (major).** Rev1 `:42–47` correctly treats missed coverage as a retry rather than a different case's pass, and rejects a DF-MID low-space-precheck abort. However, DF-MID still lacks an upper boundary/identity for the targeted flush; `Writing final batch` is incorrectly used as a flush-completion boundary; and FL-WAL accepts a WAL append failure without requiring the intended WAL sync failure. Details below.
- **m1 — CLOSED.** Rev1 `:32` explicitly removes the filler on every writable C0/C1 copy before opening it, tolerates absence, checks at least 64 MiB available, preserves the pristine replay image, and forbids deleting node files. This supersedes the older C1-only wording at `:23` and addresses C0's writable RocksDB recovery open (`inspect.rs:142–160`).
- **m2 — OPEN (minor).** Rev1 `:36` fixes exact Core spelling, includes both system-error prefixes, scopes out the false-return branches, and excludes startup errors. It still omits rdashd's actual flat-file suffix grammar required by r1: `FlatFileError::Io` displays `{context}: {path}: {source}` (`dashd-store/src/flatfile/error.rs:11`), and the flush path passes that full display string to AbortNode (`machine.rs:243–248`). An ordinary failed files-barrier sync is therefore rejected by the exact-only flat-file alternatives. Details below.

## Still-open findings

### M4 — major: the observed window is not an isolated failed operation, and failed retries pass the direct guard

**Locations:** `PLAN-ST3b-S6.md:48–50,72`.

**Scenario 1 — restoration races the retry.** At the syscall-exit stop, strace records `fdatasync(blk) = -1 EIO` and resumes rdashd. The independent watcher has yet to notice the trace line, invoke the wrapper, and complete suspend/load/resume. A mutant immediately retries; the still-down device returns another EIO. It then propagates that second failure and aborts normally with status 1. There is no successful sync before the first abort and no subsequent tip, so every stated direct check passes. A dropped-error mutant can instead reach a WAL operation during that same interval and abort on its error, recreating r1's substitute-failure masking. Switching back after either event does not undo the ambiguity.

The new direct check forbids only **successful** syncs on the failed path. Core's failed data barrier is not retried at all (`util/fs_helpers.cpp:122–152`, `flatfile.cpp:96–105`); the node's flat-file regression explicitly requires that a failed sync latches and that a later sync causes no filesystem operations (`flatfile/tests.rs:390–401`). Thus “failed retry, then sanctioned abort” is a real false acceptance against the specified no-retry contract, regardless of the filesystem.

**Scenario 2 — restored mapping does not establish a healthy filesystem.** The supplied measurement demonstrates that a particular XFS data-error/fsync sequence permits a successful retried fsync after repair. It does not establish that every whole-device error window in the three cases leaves XFS operational. The same target also rejects journal/metadata writes; XFS shutdown is already acknowledged at `:16,54`. A shutdown before restoration can keep later operations failing and mask a dropped error on XFS too. The ext4 caveat is appropriate, but restricting the mandatory mutant evidence to XFS alone does not remove this possibility. `errors=continue` likewise does not undo an ext4 journal abort, and EROFS is not a universal post-error errno for all ext4 operations.

**Validation:** an unprivileged C probe under strace 6.19, with two injected fdatasync EIO results, retries and then exits 1 with an I/O-abort text. Its trace contains no successful sync, so the proposed direct retry predicate accepts it. A separate single-injected-error control performs an immediate successful retry on the healthy backing file. These are syscall/oracle counterexamples, not dm-flakey acceptance runs.

**Fix:** retain persistent device runs, but add a controlled first-failure test that prevents the affected operation from returning to application code until restoration has completed, with unrelated application I/O also prevented from acquiring substitute errors. Alternatively use a narrowly targeted one-call failure seam on an otherwise healthy filesystem for the error-propagation/mutant proof, alongside the real-device runs. Verify the actual failed **data/WAL persistence operation** is exercised in the mutant run. Reject a retry of that failed persistence operation before its causal abort whether the retry succeeds **or fails**; permit separately identified shutdown flushes after the abort. Record effective restoration and the relevant filesystem health evidence instead of inferring health from the filesystem name. Both mutants must be rejected for the intended behavioral violation; a wrong-phase or unhealthy-filesystem run must be inconclusive/missed coverage, not mutant detection.

### M5 — major: revised coverage still confuses flush boundaries and WAL append with sync

**Locations:** `PLAN-ST3b-S6.md:44–47` (with triggers at `:14–15,22`).

**Scenario 1 — final-batch failure is incorrectly outside FL-FLUSH.** `Writing final batch` is emitted **before** the final durable write (`machine.rs:1411–1412`), not at the end of that flush. A correctly observed EIO in that final write/fsync therefore falls after the proposed end marker and is classified as a coverage miss. When that same final-batch line is the trigger, the nominal interval can end at its beginning. Three seed retries do not correct this false boundary.

**Scenario 2 — DF-MID still accepts another operation/flush.** DF-MID now demands an rdashd write-family ENOSPC and excludes a low-space-precheck abort, which is an improvement. But its only temporal condition is “after the trigger line”; it has no requirement that the failed write belongs to the flush whose partial-batch line triggered filling. That flush can finish before filling takes effect. A subsequent flush can pass its space checks while filling is still in progress and then suffer write ENOSPC, or another storage write can fail afterward. A system-error abort then satisfies the stated predicate despite missing the targeted in-flight flush. The predicate also does not require this ENOSPC to be the failure responsible for that abort.

**Scenario 3 — WAL path alone is insufficient for the stated WAL-sync case.** A `write`/`pwrite64` EIO on `chainstate/*.log` satisfies FL-WAL even if no WAL fsync/fdatasync reports failure. Such a run cannot exercise a bug that drops/retries the WAL barrier error. The approved parent §3.5 defines FL-WAL as landing on the next WAL sync; `machine.rs:692–703` separates buffered WAL-producing writes from the later WAL barrier.

**Fix:** define a real interval/identity for the targeted flush, from its trigger through completion of its durable final batch or its causal abort. The existing successful `Committed ... to coin database...` line (`machine.rs:1413–1419`) is after the final durable write; a test observer/seam can supply a stronger identity when necessary. Apply that same interval and operation attribution to DF-MID, requiring the targeted flush's fatal ENOSPC, not merely an ENOSPC somewhere later. Require an actual failed WAL sync for FL-WAL; report append-only failures separately. Record effective injection/fill time and failure completion. Keep the bounded seed retries, but after three misses fail coverage explicitly; never retry away an actual behavioral/recovery failure. Validate final-batch failure, later-flush ENOSPC, and WAL-write-only negative examples.

### m2 — minor: exact flat-file alternatives reject the flush path's legitimate suffixes

**Location:** `PLAN-ST3b-S6.md:36`.

**Scenario:** the files barrier fails syncing a block file. `seq.rs:228–231` constructs a `FlatFileError::Io` with the fixed Core flush context and the file path; `machine.rs:243–248` uses its full display string as the abort. A valid example is:

```
Flushing block file to disk failed. This is likely the result of an I/O error.: /mnt/rdashd/regtest/blocks/blk00000.dat: Input/output error (os error 5)
```

This is neither the exact Core alternative nor one of the two system-error prefixes, so a correct node gets a false red. Some other flat-file call sites intentionally strip to the Core context (`store/blocks.rs:93–98`, `connect/serial.rs:1224`); those exact alternatives should remain too.

**Fix:** specify both exact Core contexts and rdashd's structured `{context}: {observed path}: {OS error}` forms for the paths that emit them. Bind path and errno to the relevant observed operation rather than accepting an arbitrary suffix or merely the substring `IO error`. Exercise the actual `FlatFileError::Io` rendering through the flush abort path, not just invented exact strings.

## New rev1 finding

### N1 — major: the new “first failed syscall” oracle omits real storage failures and includes intentionally nonfatal ones

**Locations:** `PLAN-ST3b-S6.md:40,43–48`.

**Scenario 1 — unseen failed create/open.** The exact trace filter excludes `open`, `openat`, `openat2`, and file/directory creation calls. `OsFs::handle` opens or creates files (`fs.rs:93–96`) and can fail before any traced write/sync. The supplied ext4 measurement demonstrates precisely this hazard: creating a file under the always-down window can return EIO. RocksDB also creates WAL/SST/manifest files. The node can correctly abort on a failed create with no corresponding traced EIO, or an earlier unobserved failure can be swallowed and a later traced error credited as the first one. The one-shot window then does not close at the first failed storage syscall as claimed. `-y` cannot recover an operation that was excluded entirely.

**Scenario 2 — “first failure” is not necessarily fatal.** The trace includes fallocate and all fsync calls, but the contract intentionally permits some such errors. `OsFs::allocate` ignores failed posix_fallocate and falls back to zero writes (`fs.rs:225–243`), and flat-file directory-sync failures are warnings rather than fatal errors (`seq.rs:199–202,219–223`; Core `DirectoryCommit` ignores its fsync result). EINVAL from an unsupported data sync is explicitly accepted (`fs.rs:175–178`; Core `FileCommit`). Closing the window on one of these calls and demanding fail-stop rejects a correct node. An unrelated/off-device failed traced call must not be credited as device-fault coverage either.

**Validation:** the scratch control traced an injected openat EIO with an expanded filter; that call is outside the rev1 filter. A separate natural failed-open probe under the exact rev1 filter exited 1 with its open error, while the trace contained no failed syscall at all (the natural error was EACCES on this host; the supplied device observation establishes the EIO instance). No privileged devices were started.

**Fix:** trace the complete relevant storage lifecycle, including opens/creates and relevant metadata calls, and establish attachment to all existing threads before arming. Restrict observed errors to files on the faulted mount/device and to the effective fault interval. Classify failures by operation contract: advisory allocation, tolerated EINVAL, and permitted directory-sync failures are not fatal data/WAL-barrier failures. Select and correlate the first **fatal relevant storage failure** with the causal abort and case phase. Parse thread IDs, unfinished/resumed calls, and failure completion order; entry timestamps from `-tt` alone must not be treated as return times. If a failure category is deliberately outside the tested operation set, report a missed target and retry it without claiming first-failure or fsync coverage. Add failed-create and nonfatal-allocation/directory-sync controls.

## Evidence and limits

- Confirmed this box reports **strace 6.19** and `kernel.yama.ptrace_scope=0`. The plan's attach approach is available; the objections are to its observation/ordering contracts, not tool availability.
- Treated the user's 10-31 ext4/XFS device measurements as supplied measured facts. Did not rerun them, start nodes, or create privileged devices.
- Compiled the standalone probe with `cc -Wall -Wextra -Werror -O2` and ran four unprivileged trace controls plus a flat-file grammar counterexample. All final assertions passed. Artifacts: `/work/scratch/st3b/reviews/s6-r2-sol-checks/`, results in `result.txt`. Test TMPDIR was an owner-only mode-0700 `/dev/shm/s6-r2-sol-*` directory, removed afterward; binaries and data remain in disk scratch. Initial probe iterations were corrected when they showed that injection outside the trace set does not fire and the natural protected-path open returned EACCES rather than EROFS; no device-behavior claims are drawn from those iterations.
- No build of the reference workspace was warranted for this plan-only re-review. Its existing dirty state was left untouched. No commits, pushes, or posts.

**Verdict: CHANGES-REQUIRED.** Close M4, M5, m2 and N1 in the plan before treating §8's intended resolutions as established.
