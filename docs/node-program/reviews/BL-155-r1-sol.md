# BL-155 independent review, round 1

Reviewed `ce7263c31` against `node/integration` and the proposed `/work/scratch/bl155/gate.sh.diff`. The branch was not edited. I assume the DEC-219 dispatcher still admits a gate at 150 GB free and may run several gates concurrently, as stated in the repository decisions.

## Findings

1. **Major — child checks survive a killed driver.** `contrib/node/selftest-checks.sh:82` starts `setpriv --pdeathsig TERM` on the shard shell only. If the driver receives SIGKILL, it cannot run `stop_shards`; parent death terminates the shard shell, but a foreground command launched by `expect` can remain alive. Its EXIT trap can remove the worktree while that command is still using it. I reproduced this with the same `setsid setpriv --pdeathsig TERM` launch and an `out="$(... sleep 60 ...)"` check: one second after driver SIGKILL, the shard was gone, its EXIT trap had run, and the check child was still sleeping. The real cargo checks can last far longer than the 15-second stub proof in D8. Also, lines 87–90 wait in shard-number order, so a failed later shard's group is not stopped until every earlier shard finishes. **Fix:** use a live supervisor that kills the entire shard process group when the driver dies; reap completed shards in completion order and stop a failed shard's group immediately. Verify with a long-running foreground check and an exact-PID driver kill.

2. **Major — the default four-shard gate exceeds the existing disk admission budget.** `/work/scratch/bl155/gate.sh.new:73` enables four fresh targets, measured in D18 at about 110 GB peak versus 27 GB serially. DEC-219 admits gates with only 150 GB free *at launch*, allows concurrent gates, and does not reserve space for later steps. Even with no other writes, this reduces the former 123 GB headroom to about 40 GB; other admitted gates can consume that before this selftest starts. A train gate can then hit ENOSPC, fail its checks, and disrupt other gates on `/work`. **Fix:** update the dispatcher/admission policy together with this gate change to reserve the measured per-shard space for each forced selftest, including concurrent gates and the configured shard count; check the reservation again before starting the step.

3. **Nit — trailing blank line fails the diff whitespace check.** `docs/node-program/designs/BL-155-notes.md:95` adds a blank line at EOF. `git diff --check $(git merge-base node/integration ce7263c31) ce7263c31` exits 2 for that line. **Fix:** remove the extra blank line.

## Checks and evidence

- The result merger checks an end marker, matching exit status and FAIL count, identical nonempty contiguous case lists, owner verdicts, clean-case verdicts from all shards, duplicate/malformed records, and non-owner execution. I found no fail-open path in the current record/merge logic.
- In a detached scratch worktree, I stubbed checks and changed the owner's case 17 result from `ok` to `skip` while leaving its shard exit status at 0. `--shards 4` exited 1 with `FAIL  case 17 was not run: shard 1/4 did not reach it` and `total: 322 ok, 1 FAIL, 323 cases`. The scratch worktree was removed.
- The saved merged-tree run has 323 result rows and `end 0` in each shard; its 86/89/92/89 executed-case counts imply 11 clean cases repeated across all four shards. Its report ends `total: 323 ok, 0 FAIL` and exit 0. `bash -n contrib/node/selftest-checks.sh` passes. The gate's feature marker is present in the new script and absent from the pre-BL-155 script; the niceness calculation gives an increment of 10 at nice 0 and 0 at nice 10.

VERDICT: CHANGES-REQUESTED
