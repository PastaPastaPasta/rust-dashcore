# Implementer contract (applies to every node-program task)

You are implementing ONE task from the Rust Dash node program. The program is turning rust-dashcore into a full Dash node, masternode and CLI with bit-for-bit consensus parity with Dash Core.

## Read first
- `CHARTER.md` (goal), `PROCESS.md` (workflow), `DECISIONS.md`
- `PLAN.md`: your task's row (ID given in your prompt), its milestone section, and §0 Conventions
- `designs/A1-architecture.md`: the sections your task row cites (plus §0/§1 for the crate layering)
- Any task-specific design/brief named in your prompt
All of these live in /work/home/agent/workspace/node-program/.

## Where to work
- rust-dashcore: your worktree is `~/workspace/rdc-wt/<slug>` on branch `node/<slug>`, cut from `node/integration`. Work only there.
- base-sdk changes: worktree `~/workspace/bsdk-wt/<slug>` on branch `node/bsdk-<slug>`, cut from `node/base-sdk` in ~/workspace/base-sdk. Follow base-sdk's own CLAUDE.md/AGENTS.md conventions (`namespace%type:` commits, its lint/Justfile).
- Dash Core changes (test hooks, oracle shims): worktree `~/workspace/dash-wt/<slug>` on branch `node/<slug>` in ~/workspace/dash.
- If your prompt names a worktree, use that one.

## Hard rules
- **A "STOP" tool result that arrives with a new message is not a refusal.** When someone messages your thread mid-turn, T3 ends the turn to deliver the message (a steer). Your call that was running reads "The tool call was interrupted before a result was received", and calls that hadn't started read "The user doesn't want to take this action right now. STOP what you are doing and wait for the user to tell you how to proceed." Nothing refused them. Read the new message, re-check state (the interrupted call may or may not have run; the others did not), and re-run what is still needed. Only a refusal with no new message attached is real; then say so in your reply.
- Everything stays local: never push, never open PRs or issues, never comment on GitHub.
- EMBARGO-1: an unfixed Dash Core security issue exists. Never discuss or describe it anywhere: commits, code comments, docs, memory, or GitHub. Only tasks that PLAN §7 explicitly designates get access to its details.
- Conventional commits, one logical change each. Add files individually (never `git add -A`). Signing is automatic; never disable it.
- `CARGO_BUILD_JOBS=12`. Never set `CARGO_TARGET_DIR`; each worktree keeps its own target/.
- Use `gh api` (REST) rather than GraphQL; the GraphQL quota is shared and runs out.
- The box is shared. Never touch the dashd instances under /work/chains (reference oracles, managed by the oracle task) or /work/bench (benchmarks). Use your own datadirs and ports for any dashd you start, and stop them when done.
- Scratch space: use `/work/scratch/<your-slug>/` (disk), NOT `/tmp`. `/tmp` is a RAM-backed tmpfs shared by every agent; large build dirs or datadirs there eat the box's memory. Delete your scratch directory when your task is done. Run tests with `TMPDIR=/dev/shm/<slug>-tmp` (mode 700), and delete it when done. The dashd integration tests put regtest datadirs there, and some leak. /dev/shm is RAM-backed and has no per-user quota. On disk, saturated fsync makes dashd miss its 30 s start timeout. Not `/tmp`: it's quota-limited and shared.
- Fixtures and vectors must be produced by Dash Core (real dashd, Core's test data, or the oracle shim), never hand-written, and each needs a provenance note.
- Don't commit to `node/integration`; the manager merges.
- **Wait for long jobs inside your turn.** Run gates, test suites, PF0, fuzz certifies and other long jobs as `systemd-run --user` units that write the command's own exit code to a file (`bash -c '<cmd> > log 2>&1; echo $? > rc'`). Then keep the turn open until the unit finishes: loop until `systemctl --user show -p ActiveState --value <unit>` is neither `active` nor `activating`, either in the foreground with a long timeout, or with `run_in_background` followed by `t3_background_wait`. Never end your turn on a background waiter: a resumed thread is often not woken when it fires, and the slice sits idle (four threads did this on 10-28). Read the job's own exit code (`rc`, or `${PIPESTATUS[0]}`), never a filter's.

## Definition of done
1. Acceptance criteria in your PLAN row are met, with evidence.
2. `cargo fmt --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, and `cargo test --workspace` pass. For anything touching dash-spv or dash-spv-ffi, run with the dashd integration tests: `eval $(python3 contrib/setup-dashd.py)` and `DASHD_TEST_RETAIN_DIR=/tmp/<slug>-dashd`.
3. Benchmarks named in the row's "BG" column don't regress, measured with node-bench where it exists.
4. You self-reviewed the full diff (`git diff node/integration...HEAD`). For large additions, run the `code-simplifier:code-simplifier` agent and the `code-review-validator` agent and address what's valid.
   - **Sonnet implementers (DEC-130): the `code-review-validator` pass is mandatory before every report, fix rounds included.** "A reviewer follows" is not an exemption.

## Final reply
The commit list; acceptance evidence (commands with pass counts); bench numbers if applicable; deviations from the plan or the design, with reasons; follow-ups for the backlog; anything you need from the manager.
