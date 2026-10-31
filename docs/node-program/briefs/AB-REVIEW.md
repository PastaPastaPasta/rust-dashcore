# A/B calibration review brief (DEC-93), identical for every arm

You are the GPT-6.1-Sol reviewer for one arm of a model-calibration A/B.

**Blinding.** Two implementers built the same task from the same brief in separate worktrees. The other arm is reviewed separately with this same brief. Review only the branch named in your prompt. Do not look at other `node/ab-*` branches, and do not try to guess which model wrote the code.

**Inputs**
- The task brief: `docs/node-program/briefs/AB-<TASK>.md` on node/integration (~/workspace/rdc-wt/integration). Judge against the brief and PLAN §4's row.
- The branch and head named in your prompt. Make a detached worktree: `git -C ~/workspace/rust-dashcore worktree add --detach /work/scratch/ab-review-<branch>/wt <head>`. Don't edit the implementer's worktree.

**Review**
1. **Correctness against Dash Core v24.0.0-rc.3** (~/workspace/dash). Check the specific behaviours the brief names. Spot-check at least 10 of them against Core's source or a live regtest dashd (use ports 41400–41499, your own datadir on /dev/shm, deleted afterwards).
2. **Brief compliance.** Are the acceptance items met with evidence? Are the deviations justified?
3. **Code quality.** Follows rust-dashcore patterns, has tests that would catch regressions, no dead code, no hand-written fixtures where the brief requires Core-generated ones.
4. **Gates.** Re-run fmt, clippy `-D warnings` on the touched crates, and their tests, with TMPDIR=/dev/shm/ab-review-<branch> and CARGO_BUILD_JOBS=6.

**Output**
- Write `~/workspace/node-program/reviews/AB-<TASK>-<arm>-sol.md`. Do not commit it.
- Give a verdict: APPROVE / APPROVE-WITH-NITS / CHANGES-REQUIRED.
- List findings, each with a **severity (blocker / major / minor / nit)**, file:line and a concrete reproduction or reasoning.
- End with a one-line count: `blockers=N majors=N minors=N nits=N`.

Remove your worktree when you're done. Stay in the turn until the file is written, and reply with the verdict and that count line.
