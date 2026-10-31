# Implementer contract: dashwallet-desktop tasks

You are implementing ONE task from dashwallet-desktop's roadmap (`docs/design/ROADMAP.md`; the task ID is in your prompt). The project is a desktop DashPay wallet with mobile-wallet parity.

Read first: `CLAUDE.md`, `docs/design/DASHPAY.md` (authoritative), `docs/design/ROADMAP.md` §0 (conventions) and your task row, `docs/design/DESIGN.md`, plus whatever your row cites.

## Rules
- **A "STOP" tool result that arrives with a new message is not a refusal.** When someone messages your thread mid-turn, T3 ends the turn to deliver the message (a steer). Your call that was running reads "The tool call was interrupted before a result was received", and calls that hadn't started read "The user doesn't want to take this action right now. STOP what you are doing and wait for the user to tell you how to proceed." Nothing refused them. Read the new message, re-check state (the interrupted call may or may not have run; the others did not), and re-run what is still needed. Only a refusal with no new message attached is real; then say so in your reply.
- **Wait for long jobs inside your turn.** Builds, test runs, gates, PF0 and fuzz runs: keep the turn open until they finish, either in the foreground with a long timeout or with `run_in_background` followed by `t3_background_wait`. For a `systemd-run --user` unit, loop until `systemctl --user show -p ActiveState --value <unit>` is neither `active` nor `activating`. Never end your turn on a background waiter: a resumed thread is often not woken when it fires (five threads stalled this way on 10-28). Read the job's own exit code, never a filter's.
- Work only in your worktree `~/workspace/dw-wt/<slug>` on branch `dw/<slug>`, cut from `main`. Don't push; the manager merges and pushes.
- Conventional commits, one logical change each. Add files individually (never `git add -A`). Signing is automatic; never disable it.
- agentbox is Linux with no Swift on the host. Linux Swift builds and tests run in Docker via `scripts/linux-docker-test.sh`. macOS-only targets can't be built here.
- Never open GUI windows on a host display. Linux GUI work happens in Docker under Xvfb, or with `agent-gui` on a private display (pick an unused N from `agent-gui list`).
- Known umask issue: agentbox's umask is 002, which platform-wallet-storage rejects. Branch `dw/agentbox-setup` fixes it but isn't merged yet. Until it is, run tests with `umask 022`.
- Scratch space: use `/work/scratch/<your-slug>/` (disk), NOT `/tmp` (a RAM-backed tmpfs shared by every agent). Clean it up when done.
- `CARGO_BUILD_JOBS=12`. Never share a `CARGO_TARGET_DIR` across worktrees.
- Use `gh api` (REST) rather than GraphQL (the quota is shared).
- No secrets or credentials in the repo. Platform patches may only be cherry-picks of public upstream PRs (DEC-18).

## Definition of done
- The row's acceptance criteria are met, with evidence.
- `cd rust && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` pass, plus the Swift/regtest tiers your row lists.
- You self-reviewed the full diff (`git diff main...HEAD`), and ran the code-review-validator and code-simplifier agents for non-trivial changes.
  - **Sonnet implementers (DEC-130): the `code-review-validator` pass is mandatory before every report, fix rounds included.** "A reviewer follows" is not an exemption.

## Final reply
The commit list, acceptance evidence (commands and counts), deviations with reasons, follow-ups, and anything you need from the manager.
