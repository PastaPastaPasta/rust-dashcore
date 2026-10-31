# BL-155 narrow review, round 4

Scope: `d7cdeb157..afd99478d` on `node/bl155-selftest-shard`, D31–D33, and the proposed `/work/scratch/bl155/gate.sh.new`. The branch delta changes only the notes; the behavior reviewed is in the proposed gate. I assume the sharded workers can keep writing their targets until their current foreground checks finish, as `selftest-checks.sh` states. No branch or gate file was changed.

## Finding

1. **Major — a killed step group can release its reservation while shard workers still run.** `/work/scratch/bl155/gate.sh.new:109,121-128,157-160`; `contrib/node/selftest-checks.sh:339-348,518-525`. `held()` retains a dead subshell's entry only while its process group has a member. Sharded workers lead separate sessions. If the subshell, driver, and supervisor are killed together with `SIGKILL` to the step group, those workers receive a parent-death `TERM`, but their Bash traps wait for the current foreground command to finish. Once the step group is empty, the next ledger scan drops its entry while the workers can still grow their targets. `df` sees only bytes already written, so another gate can reserve the same future 28 GiB per worker and consume the 100 GiB margin. **Fix:** keep a killed step's reservation until its shard sessions have ended and their target cleanup is complete, using run-specific worker/session identity or an equivalent step cgroup check; prove a whole-step-group `SIGKILL` leaves no unaccounted live shard before permitting a new grant.

A private reproduction used the exact proposed gate block, a private ledger, and a fake selftest with a worker in a separate session. After killing the step group, the worker was still alive. The next gate logged `dropping stale reservation ...`, then `second step ran` with `rc=0`. The private ledger and test files were removed. The existing supplied proof kills only the subshell PID and therefore keeps the driver in the step group; it does not cover this case.

## R3 Major and other checks

The serial admission path is fixed: old trees and `K=1` write a 28 GiB entry before selftest; no room, lock failure, failed write, or invalid `df` output waits and ultimately fails rather than starting unreserved. The supplied five-step and stop proofs support those paths. D33's six-hour cap and throttled log cadence do not add a separate issue in this scope. PID/start-time validation prevents an ordinary reused PID from retaining an entry, and I found no supported unrelated-process group that would retain one indefinitely. TERM during the wait exits promptly in the supplied sleep and flock proofs. `bash -n` on the proposed gate and `git diff --check d7cdeb157 afd99478d` pass; the branch is clean. I did not run the full selftest.

VERDICT: CHANGES-REQUESTED
