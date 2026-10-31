# BL-49 code review, round 3 (Sol)

Reviewed `node/bl49-private-tests` at `acd2e55fc4a296ef1cbc7be0a86e0d675d05f509`, limited to `4886e24c..acd2e55f`, against `BL49-r2-sol.md`, the manager's no-pruning ruling, and `REVIEW-COMMON.md`. The diff changes only `contrib/node/check-private-tests.sh`. The reviewed worktree remained unchanged. Experiments used `/work/scratch/bl49-review-r3-sol/` and mode-0700 `TMPDIR=/dev/shm/bl49-r3-sol-tmp`. No reviewer command opened anything under `~/private`; the two authorized private gates ran concurrently, and only pass/fail and counts were extracted from their captured output.

## Confirmation

**The r2 major is closed.** There is no automatic pruning, namespace scan, lock file, or flock left in the gate. Each invocation obtains an exclusive directory name through `mktemp -d`, exports only that name to its tests, checks only that directory, and limits its EXIT cleanup to that path. Suspending initialization or a receipt writer, including beyond the former age floor, gives another invocation no code path that can delete its directory. No new race between independent runs was found.

**SIGKILL leftovers cannot supply or erase a later run's receipts.** A synthetic parent-only SIGKILL left the held child's directory intact; the child completed its write afterward. After aging that directory by 48 hours, a later invocation passed with two receipts and left the old directory's contents and directory/file mtimes unchanged. `mktemp` refuses to reuse an existing name, and receipt validation selects the new run id. The manual cleanup command is appropriately conditional on no gate running; that operational condition is not enforced by `find` itself.

## Findings

### Minor — EXIT cleanup does not wait for the receipt writer on TERM/HUP

**Locations:** `contrib/node/check-private-tests.sh:14`–`:16`, `:141`–`:144`, and `:234`–`:245`.

The statement that Bash waits until the foreground command returns before running this EXIT trap is false for the tested Bash 5.3.9. With the unchanged gate blocked in a foreground Cargo stand-in, signaling **only the gate** with SIGTERM or SIGHUP ran cleanup and terminated the gate **before releasing the still-live child**. The directory was already gone while that child remained blocked. Shell wait statuses are 143 and 129, respectively; neither interrupted gate printed a receipt PASS. Python's direct subprocess statuses were -15 and -1, representing the same signal deaths.

A Rust child linked against the reviewed `node-sink` confirmed the concrete consequences. It held an open receipt file and paused between the directory-creation and pathname-write operations used by `write_receipt_for`. After gate cleanup, `write_private_file` returned `NotFound`; writing its already-open file still succeeded, but the file was unlinked. Calling the complete `write_receipt` afterward succeeded by recreating the run directory, leaving a directory behind after the EXIT cleanup had already finished. TERM/HUP can therefore leave leftovers too, and a surviving receipt writer can race its own gate's cleanup.

Case 1d sends TERM to both the gate and the stand-in immediately. It proves the tested exit status, absence of PASS output, and cleanup for that schedule, but does not establish ordering or exercise a writer that survives the gate. The observed behavior is not a new cross-run receipt race: the gate is already canceled, no PASS is emitted by that terminated gate, and a later invocation passed with both receipts while leaving the recreated directory untouched. This limits the finding to cancellation cleanup and its documentation, rather than reopening the r2 major.

SIGINT also needs qualification. Parent-only SIGINT while the child was blocked left the gate and directory alive; after the child exited normally, the gate continued and passed with two receipts. SIGINT to the foreground process group interrupted the child, and the gate exited with status 130, ran EXIT cleanup, and emitted no PASS. An inherited ignored SIGINT disposition, as with ordinary noninteractive background jobs, is another reason an INT need not terminate Bash. An EXIT trap on fatal termination is not a guarantee that every delivered INT is fatal.

**Suggested fix:** correct the header and inline timing claim, document that surviving children can recreate harmless leftovers after TERM/HUP, and add a parent-only signal test with a child held across gate exit. If cleanup must instead wait for all receipt writers and leave no directory after those signals, implement and test explicit child-lifetime coordination before removing the directory. Merely adding a signal trap that deletes it would not establish that ordering.

### Minor — case 1c does not compare foreign file contents

**Locations:** `contrib/node/check-private-tests.sh:222`–`:229`.

The claimed content comparison is absent. The assertion checks that the foreign receipt is still a regular file and that the two **directory** mtimes match at whole-second resolution. Truncating and rewriting an existing receipt changes its contents and file mtime without changing its parent directory mtime. A scratch mutant that replaced the foreign receipt's `x\n` with `changed\n` during each run passed the full selftest, exit 0.

**Suggested fix:** compare the sentinel's bytes or hash and its file mtime as well as the directory mtimes; add a sentinel to the unrelated directory if its contents are part of the claimed invariant. The production gate has no foreign-content access path, so this is a regression-test coverage issue.

## Validation

- `bash -n contrib/node/check-private-tests.sh` and `git diff 4886e24c..acd2e55f --check`: **PASS**.
- `TMPDIR=/dev/shm/bl49-r3-sol-tmp contrib/node/check-private-tests.sh --selftest`: **PASS**, exit 0.
- `TMPDIR=/dev/shm/bl49-r3-sol-tmp cargo test --locked -p node-sink`: **PASS**, 15 unit tests and doc-tests.
- The additional `contrib/node/selftest-checks.sh` run was deliberately stopped (143) after its pin/source checks, before the remaining unrelated checks. This unchanged repository-wide mutation suite does not exercise the private gate; its full claimed result was not independently reconfirmed in this narrow round.
- Two concurrent authorized real gates: **PASS / PASS**, **2/2 receipts each**, **522 cases** and **279 seeds (279 required)** each.
- Scratch negative controls: restoring age-based foreign-directory pruning failed case 1c, exit 1; removing EXIT cleanup while retaining normal-return cleanup failed case 1d, exit 1. The foreign-content mutation passed, establishing the second finding.
- Parent-only and process-group TERM/INT/HUP probes, a Rust `node-sink` writer held across gate termination, and a SIGKILL/48-hour-leftover follow-up confirmed the behavior above.

The extra selftest's temporary worktree, scratch probes/binary/logs, and dedicated `/dev/shm` tree were removed after validation. Only the requested review file remains from this review. No commits, pushes, or GitHub comments were made. This review is intentionally uncommitted.

Verdict: **APPROVE-WITH-NITS — mergeable** for the requested no-pruning receipt-isolation fix. The r2 major is resolved; the minor findings qualify cancellation behavior and test evidence, with no observed effect on another run's receipts.
