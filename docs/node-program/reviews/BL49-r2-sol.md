# BL-49 code review, round 2 (Sol)

Reviewed `node/bl49-private-tests` at `4886e24c270eb7a1ce54ed73c3fcb2d4241d68c6`, the fix on top of r1's `af516a546bba2f75ea4b6e3a21efa4aa124698ef`. Narrow confirmation against `BL49-r1-sol.md` and `REVIEW-COMMON.md`, using a detached worktree under `/work/scratch/bl49-review-r2/` and `TMPDIR=/dev/shm/bl49-r2-tmp` (0700). No reviewed tracked files were modified, and nothing under `~/private` was opened by the reviewer. The two authorized private-corpus gate runs were captured without displaying their raw output; only pass/fail and receipt counts were reported.

## Finding

### Major — pruning still races a creator before it acquires its run lock

**Locations:** `contrib/node/check-private-tests.sh:160` (directory publication), `:163` (lock-file creation), `:164` (lock acquisition), and `:140`–`:143` (pruning).

`mktemp -d` publishes the run directory before `.lock` is created or locked. The age floor does not establish abandonment: a live creator can be suspended for over a day between these operations, just as a fully initialized run can. A later gate then deletes its directory. There are two concrete failure schedules:

1. Suspend A after `mktemp` creates its directory, before the `.lock` open. Once the directory is old, B's lockless-directory branch deletes it. Resuming A fails the `.lock` redirection with `No such file or directory` and exits 1 before its tests.
2. Suspend A after opening `.lock`, before `flock -x`. Once the directory is old, B acquires that unlocked file and deletes the directory. A resumes and successfully locks its still-open **unlinked inode**. The real receipt writer recreates the directory, but there is no `.lock` in that new directory and A's lock does not protect it. If A later pauses with its first receipt for over a day, C prunes the lockless directory; A resumes with a missing first receipt and fails the gate. Holding the file descriptor is insufficient when the path/inode it protects has already been removed.

**Evidence:** copied the unchanged reviewed `run_private_tests`, `prune_abandoned_runs`, and `check_receipts` functions into scratch, and used command wrappers to stop A at those boundaries. Backdating directory mtime by 48 hours modeled the suspension, as in selftest 1c. A Cargo stand-in recreated receipt directories as `node-sink` does and wrote only synthetic valid receipts. Both schedules reproduced on **XFS under `/work` and tmpfs under `/dev/shm`**. In schedule 1, B returned 0 with two PASS receipts, removed A's directory, and A returned 1 on the lock-file open. In schedule 2, B and C each returned 0 with two PASS receipts; after B, A recreated a directory without `.lock`; C removed it; A returned 1 with `quirk_01_embargo_1: receipt FAIL no receipt` and one replay PASS receipt.

**Suggested fix:** make publication and ownership atomic with respect to pruning. For example, create and lock a staging directory outside the pruner's namespace before publishing it, or serialize directory creation/lock acquisition and pruning with a shared namespace guard. Account for a pruner already racing initialization; checking age or existence again without synchronization is insufficient. Omitting opportunistic pruning also safely closes this finding. Add deterministic tests for suspension before lock-file creation and before lock acquisition, including the unlinked-inode case.

## Confirmation of r1 and lock behavior

- **r1 major: partially fixed, not fully closed.** A run that already holds its lock survives age-based pruning, including suspension after its first receipt. However, the startup gap above still lets pruning delete a live run and can reproduce the original missing-receipt failure.
- **r1 minors: closed.** The public environment path distinguishes an unset id from an empty or non-UTF-8 value. Invalid values return `InvalidInput`; the raw component comparison rejects `run-a/`, `run-a//`, and `run-a/.`. The subprocess test exercises the public function without mutating the parent environment. An additional subprocess probe confirmed that a non-UTF-8 value returns `InvalidInput` before creating any directory.
- **Case 1c is useful but starts too late.** It passes against this head and fails with the old age-only pruning function restored in a scratch copy: `the live run's old directory was pruned`. Its barrier is after A owns the lock, so it misses initialization. The failed scratch selftest's stand-in exits when cleanup removes its barrier directory rather than spinning indefinitely.
- **Held locks and cleanup:** once acquired on the linked run inode, the exclusive lock excludes a nonblocking pruner. Receipt validation and `rm -rf` precede descriptor closure. Concurrent pruners contend on the same inode; neither normal receipt creation nor modification removes `.lock`. These parts introduce no additional observed race.
- **Descriptor inheritance:** a separate Cargo test probe confirmed on both filesystems that the test inherits Bash's descriptor. A spawned surviving child retained the lock after the gate/Cargo exited, and the lock became available when that child exited. This conservatively delays pruning after an interrupted gate; it does not repair the startup publication gap or prevent the gate's own normal cleanup.
- **Filesystem scope:** `/work` is local XFS and `/dev/shm` is tmpfs; neither is NFS. Both provided the expected cross-process `flock` behavior in the probes. Cross-client NFS semantics were not tested and cannot be certified from this host; a deployment there would need functioning shared locks rather than mount options that make locks client-local.

## Validation

- `TMPDIR=/dev/shm/bl49-r2-tmp contrib/node/check-private-tests.sh --selftest`: **PASS**.
- `TMPDIR=/dev/shm/bl49-r2-tmp cargo test --locked -p node-sink`: **PASS**, 15 unit tests and doc-tests.
- `TMPDIR=/dev/shm/bl49-r2-tmp cargo clippy --locked -p node-sink --all-targets -- -D warnings`: **PASS**.
- `cargo fmt -p node-sink -- --check` and `bash -n contrib/node/check-private-tests.sh`: **PASS**.
- `TMPDIR=/dev/shm/bl49-r2-tmp contrib/node/selftest-checks.sh`: **PASS**, exit 0, `all selftests passed`.
- Two concurrent authorized private-corpus runs: **PASS / PASS**, **2/2 receipts each**, each reporting **522 cases** and **279 seeds (279 required)**.
- Synthetic startup-race probes: reproduced the major finding on both filesystems. Public non-UTF-8, descriptor inheritance, and old-pruner negative-control probes behaved as described above.

The detached worktree, scratch probes/builds/logs, and dedicated `/dev/shm` temporary tree were removed after validation. Only the requested review file remains. No commits, pushes, or GitHub comments were made.

Verdict: **CHANGES-REQUIRED — not mergeable**. The initialization race must be closed. This review is intentionally uncommitted.
