# Program process

## Roles
- **Manager** (the orchestrating thread): owns the plan, `node/integration`, the task board, and merges. Does not write feature code.
- **Model and effort allocation (DEC-92; supersedes the earlier role definitions and refines DEC-88).** Every delegation sets the model and effort explicitly (`target.options`); PLAN §4's Model column carries each task's tier, so dispatch is mechanical.

  | Work | Model / effort |
  |---|---|
  | Most design work (`D O-h`) | Opus 5.5 high, single designer |
  | Very hard design (`DX`, rare) | Opus xhigh and/or Fable high. Never Fable for dashwallet-desktop |
  | Reviews (`rev Sol`) | GPT-6.1-Sol (`reasoningEffort` high; xhigh for critical reviews) |
  | Critical reviews (`rev Sol→O-h`: consensus, security, storage durability) | **Sequential**: GPT-6.1-Sol first; Opus high only after Sol is satisfied, or once the implementer stops accepting Sol's feedback. Never two reviewers in parallel |
  | Easy, well-defined implementation (`impl S-h`, ~50%) | Sonnet 5.5 high |
  | Semi-hard implementation (`impl O-m`, ~25%) | Opus 5.5 medium |
  | Hard implementation (`impl O-h`, ~25%) | Opus 5.5 high |
  | Exploration and searches | Haiku |

- **Escalation, not fix-round pile-up.** If a Sonnet implementer struggles (a second fix round on the same finding class, or a design-level misunderstanding), redo the task on Opus at the next tier (O-m), with the reviews so far as context. The same applies O-m → O-h.
- **Self-review** stays with the implementer (code-review-validator and code-simplifier passes at the implementer's own tier).
- **Running agents** started before DEC-92 finish at their current settings; new dispatches, new review rounds and continuations that start a new phase follow the table.

## Git model (local "git server")
- Repo: `~/workspace/rust-dashcore`. Integration branch is `node/integration`, owned by the manager, with its worktree at `~/workspace/rdc-wt/integration`.
- Each task gets its own branch `node/<slug>`, cut from the current `node/integration`, and its own worktree `~/workspace/rdc-wt/<slug>`. Never share a Rust `target/` across worktrees, and never set `CARGO_TARGET_DIR`.
- Dash Core changes (test hooks, vector exports, functional-test shims) go to branch `node/<slug>` in `~/workspace/dash`, with worktrees under `~/workspace/dash-wt/<slug>`.
- base-sdk changes go to branch `node/bsdk-<slug>` in `~/workspace/base-sdk`, cut from the fork branch `node/base-sdk`, with worktrees under `~/workspace/bsdk-wt/<slug>`. They follow base-sdk's own conventions and merge into `node/base-sdk` with `--no-ff`. rust-dashcore consumes one pinned revision of `node/base-sdk`; moving it is a rust-dashcore commit made with `contrib/node/bump-base-sdk.sh`. See `BASE-SDK-FORK.md`.
- Commits use conventional-commit style, are signed (automatic), and are added file by file (never `git add -A`). Nothing is pushed to GitHub unless pasta asks.
- To merge, the manager rebases the task branch onto `node/integration` when needed, merges with `--no-ff`, then runs the gate.

## Task lifecycle
1. **Brief**: the manager writes `briefs/<slug>.md` covering goal, scope, non-goals, acceptance criteria, dependencies, and the required tests.
2. **Design** (tasks tagged `D`/`DX` only): the designer writes `designs/<slug>.md`; a GPT-6.1-Sol review (plus, for `DX` or critical designs, an Opus high pass after Sol, per DEC-92) critiques it; the manager approves. Dual designs only for `DX`.
3. **Implement**: the implementer works in the task worktree. The task is done when it builds, has tests, passes `cargo fmt`, `cargo clippy --all-features --all-targets -- -D warnings`, and the relevant tests, and the implementer has self-reviewed (including the code-simplifier pass for large additions). The implementer reports back with a summary, the commits, the test evidence, and known gaps.
4. **Review**: GPT-6.1-Sol. For `rev Sol→O-h` tasks (consensus, security, storage durability), the Opus high pass follows only after Sol is satisfied or the implementer stops accepting Sol's feedback (DEC-92); consensus work still needs differential tests. Findings go to `reviews/<slug>-rN.md`.
5. **Fix loop**: findings go back to the implementer (or a fresh agent with the full context). Each review round is a new delegated task with the original brief, prior findings, and responses.
6. **Merge**: when the reviews are clean, the manager runs the PF0 counter gate on the branch (below), merges it into `node/integration` with `--no-ff` (the merge commit's tree must equal the ledger line's `measured_tree`), copies the gate's ledger line into the committed ledger, and updates `STATUS.md`. If `node/integration` moved during the gate (usually docs-only commits), make the gated merge on the gate's `base` in a temporary worktree (its tree then equals `measured_tree`) and merge that commit into the current tip with `--no-ff`; never rewrite integration history. Changes on integration since the gate's base to any PF0 bench input (as perfgate's README §4 defines it: the selected benches' package sources, `[[bench]]`, dev-dependencies, build scripts and their local closure, `Cargo.lock`, toolchain) mean a re-run; changes outside that set (docs, contrib scripts, packages outside every bench's closure) do not.

## Gates on node/integration
- `cargo fmt --check`, `cargo clippy --all-features --all-targets -- -D warnings`, `cargo test --workspace`
- dashd integration tests: `eval $(python3 contrib/setup-dashd.py)`, then `DASHD_TEST_RETAIN_DIR=... cargo test -p dash-spv`
- A v24 dashd for the node's own live differentials (DEC-82). `setup-dashd.py` still defaults to v23.1.0 for the SDK's DKG tests, so the gate also exports `ST2_DASHD`: `export ST2_DASHD=$(DASHVERSION=24.0.0-rc.3 python3 contrib/setup-dashd.py 2>/dev/null | sed -n 's/^export DASHD_PATH=//p'); [ -x "$ST2_DASHD" ] || exit 2`. With it set, `dash-coreimport`'s `scanner_parity` fails when the dataset or a v24 binary is missing; without it the test only skips (visibly), and a gate that doesn't export it compares nothing there.
- Node checks: `contrib/node/check-base-sdk-pins.sh`, `cargo deny check`, the layer gates `python3 contrib/node/check-layers.py`, `contrib/node/check-pure.sh`, `contrib/node/check-shared.sh` and `contrib/node/check-mobile.sh` (SB0 §2.3 L1–L8; the last builds the Android library for L7's size gate, `BASE-SDK-FORK.md` *Layer rules*), `python3 .github/scripts/ci_config.py verify-groups`, `python3 -I contrib/node/check-quirks.py --core <Dash Core clone>` (the quirk register, below), and the DEC-82 guard `contrib/node/pin-delta/check-release-refs.sh --selftest` followed by `contrib/node/pin-delta/check-release-refs.sh` (gate step `release-refs`; the CI job that also runs it is skipped while the pin is local, so the gate is its only enforcement); `contrib/node/check-loom.sh` (gate step `loom`: every dash-chain loom model, refusing a renamed or cfg'd-out one) and `contrib/node/check-public-api.sh` (gate step `public-api`: dash-chain's public API listing); both skip on trees that predate SB4; plus `contrib/node/selftest-checks.sh` when `contrib/node/`, `deny.toml` or the base-sdk tables in the root `Cargo.toml` change. The `file://` base-sdk pin only resolves on agentbox, so the gate runs everything locally; the GitHub workflows skip their Cargo jobs while the pin is local (`BASE-SDK-FORK.md`, *CI and leaving agentbox*).
- Private tests (PLAN §7 handling): `DASH_EMBARGO_DIR=<private tree> contrib/node/check-private-tests.sh` must print `receipt PASS` for both tests and exit 0. Set the variable for this step only; every other step runs with it unset. Also run `contrib/node/check-private-tests.sh --selftest` when that script, `node-sink`, `node-l1` or the two private tests change (the agentbox gate runs it every time). DEC-32 merge checklist: for every consensus-task merge, run `check-private-tests.sh` with the variable set and record both `receipt PASS` lines in STATUS.
- Core differential suites (oracle feature; `cargo test --workspace` does not enable it): `cargo test -p dash-oracle-sys --features oracle` and `cargo test -p node-l1 --features oracle`, both `--no-fail-fast`. Gate step `oracle-diff` (added 2026-10-09 after RP-V24's oracle-only failures passed the gate). Implementer gate copies must include it.
- **Oracle pin moves** (2026-10-12): a merge that changes `dash-oracle-sys/pins.txt` must leave both pinned archives in the shared cache, `/work/cache/oracle/<core-sha>/{plain,fuzz}/`, before the task's scratch is cleared. Install with `check-oracle-repro.sh … --install /work/cache/oracle`, or copy the pin-verified archive (sha256 against pins.txt) into a dot-tmp dir and rename it into place. build.rs reads only that cache (or `DASH_ORACLE_CACHE`), so a missing directory fails every later gate's oracle-diff step at build time.
- PF0 harness unit tests: `python3 -I -m unittest discover -s contrib/node/perfgate/tests` (about 10 s; every gate run).
- Quirk register (O5, FR-CON-34): `python3 -I contrib/node/check-quirks.py` (every gate run; it builds and lists every workspace test binary, which `cargo test --workspace` reuses) checks that `quirks.toml`, the `quirk_NN_*` tests and the compiled test inventory agree and prints the tested/ignored counts that STATUS tracks. A task that lands a quirk un-ignores its test and sets the register's status in the same commit.
- **Gate copy TMPDIR** (SB1 r8, 2026-10-11): gate.sh's BL-99 `eatmydata` dashd wrapper lives in each gate copy's private TMPDIR, not in the shared `${TMPDIR:-/work/tmp}`. The shared copy was deleted mid-run on 10-11 and caused 54 false failures in SB1 r8.
- Once they exist: the consensus vector suites, then chain replay, then functional-test parity.
- **PF0 counter gate** (A1 §3.11, DEC-23, DEC-52): a merge gate the manager runs, from the integration worktree: `contrib/node/perfgate/run gate --head node/<slug>`. It is not an implementer step; implementers may run it for advice with their own `PERFGATE_ROOT=/work/scratch/<slug>/perfgate`.
  - It measures the tree that will be merged: the branch merged into the current `node/integration`, synthesized when the branch is behind. A conflict is an ERROR.
  - It refuses to run (ERROR) unless the running harness is integration's committed one, so a PR can't change the workloads or the policy it is judged by. It also refuses a branch that changes `rust-toolchain` or a `.cargo/config`.
  - The PR's code (builds, build scripts, benches, the node) runs in a bubblewrap sandbox with the filesystem read-only except its own build and scratch directories, so it can't write the gate's state or edit bench inputs.
  - It counts user-space instructions (region and whole process), allocations, syscalls, I/O, page faults and peak RSS on `contrib/node/perfgate/workloads.toml`, 5 runs per side, base and head interleaved.
  - **What blocks the merge:**
    - **REGRESSION:** a counter beyond max(0.5%, twice the workload's noise floor, a small absolute floor), where either the head runs separate from the base runs, or the base is deterministic (then a moved median is enough). Most workloads are deterministic.
    - **NOISY-UNRESOLVED:** any other shift beyond max(0.5%, the noise floor), and any regression on runs that weren't bit-stable, is re-run 10 more times per side. Every counter of that workload is then decided on all its runs: REGRESSION if an exact one-sided Mann-Whitney test at α = 0.01 confirms the shift. An unconfirmed shift towards worse blocks as NOISY-UNRESOLVED. The whole procedure is calibrated to a ≤ 1% false-FAIL rate on no-op runs.
    - **DRIFT:** cumulative drift above 2% against the anchor (the last milestone's recorded values). A gated workload or counter that the anchor lacks is an ERROR; anchor a new workload right after the merge that adds it.
    - **BENCH-CHANGED:** the PR changes anything that defines a gated bench's inputs: its sources and included data, `[[bench]]`, dev-dependencies (and their workspace entries and locked versions), `[profile.bench]`, or the build scripts, build-dependencies, `links` or features of a workspace crate it is built from. The head is measured with the base's bench sources.
    - **ERROR** (exit 2): any failure to build, measure or compare, a stale or incomparable anchor, a merge conflict, or a foreign harness.
  - **Approvals** are the manager's, and always recorded:
    - a bench input change: `--approve-bench-change "<why>"`, which goes into the ledger;
    - a blocking regression (for example a correctness fix): a justification in STATUS;
    - drift: `run anchor <rev> --justification "<why>"`, which re-anchors.
  - **Exit codes:** 0 PASS, 1 FAIL, 2 ERROR, 3 INCOMPLETE. ERROR takes precedence over FAIL. INCOMPLETE is a subset (`--only`), another workloads file, `--dev-harness`, or a base other than `node/integration`; it never counts as a PASS for a merge.
  - **Node workloads** gate memory signals only (page faults, allocated bytes, peak RSS) until rdashd has a deterministic bench mode and its own measured noise floors (DEC-52 4).
  - **After each merge:** run `contrib/node/perfgate/run warm &`, so the next gate starts with a hot base, and copy the merge's ledger line from `/work/perfgate/ledger.jsonl` into the committed `research/PF0-ledger.jsonl` (DEC-52 2).
  - **After a merge that changes bench inputs** (a base-sdk re-pin, a lockfile or toolchain change, or an approved bench change), re-anchor at once with `contrib/node/perfgate/run anchor <integration tip> --justification "…"`. Otherwise the next gate on any branch fails with `stale anchor` ERROR, which happened after the 2026-10-09 re-pin.
  - Wall-clock comparisons (`node-bench pair`) are milestone evidence, run only in reserved quiet windows.
  - **Benches measure production only** (SB5 D84, CH2b CP6.1, 2026-10-11). A crate never turns on its own test features (`test-util`, `crash`, `planted-bugs`, fault hooks) through a self dev-dependency. Under Cargo's dev-dependency feature unification that enables them in every dev target, benches included, so PF0 would measure test-only code. Tests that need them use `required-features`, and the gate runs them with an explicit `--features` line. Each such crate has a bench canary: `cargo bench -p <crate> --no-run` fails to build when a test feature is on.

## Desktop merges (dashwallet-desktop main)

- Bar: Sol ACCEPT or APPROVE-WITH-NITS, or a manager narrow delta check for small fix-forwards. Merge with `--no-ff` to main, then push. Main CI must go green afterwards; the Windows Swift job is a known non-blocking gap.
- A branch behind main follow-merges main first, then runs tsc, vitest, the Playwright specs it affects, and the Rust checks if the delta touches Rust.
- Any change to an m1–m4 §4 error-code table, or to any `docs/contracts` file a Swift test parses, runs the Swift T0 suite (`scripts/linux-docker-test.sh` on agentbox) before READY. The Rust-only desktop gate does not cover it. Lesson: DP3-01 added `send.cancelled` without a Swift binding, and main went red (10-15).

## Heartbeat (every 30 min, manager)
- Poll every running/in-review/fix-round/design task with `task_status`; process finished ones, resume cancelled ones, unstick stalled ones.
- CPU audit (pasta, 2026-10-14): don't just report load. List running user units and processes older than 1 h above 5% CPU, and map each one (cwd, then worktree, then task) to a live STATUS row. Fuzzers must be bounded (`-max_total_time`, `--cpu-hours`), and long legs must still be gaining coverage (DEC-184). Kill or rework anything orphaned or plateaued. Report a one-line unit-to-task breakdown.
- Open external PRs (pasta, 2026-10-15, after kwvg's review on base-sdk #65 sat unseen for 9 h): every heartbeat, call `list_thread_pull_requests`. Run `~/.claude/skills/babysit/scripts/pr_wait.py once` over all open PRs, handle and ack its events, re-arm `watch_pull_request` on any open PR showing `watching: false` (watches lapse on restarts), and record merges and closes in STATUS.
- Open-PR review sweep (pasta, 2026-10-22, after kwvg's #73 review sat 11 h with no answer while its fix waited on the comb lock, and #74 sat answered but never re-requested): every heartbeat, also run `python3 tools/pr-review-sweep.py`. pr_wait reports an event once; once a batch is acked into a fixer, nothing reminds you again. The sweep re-reads the current state of every open base-sdk PR and flags:
  - UNANSWERED: a human thread or comment whose last word isn't ours;
  - STALE-ASK: CHANGES_REQUESTED on a head we haven't pushed past yet;
  - REREQUEST: every thread answered and the head moved, but the reviewer isn't requested.

  Act on each flag in the same heartbeat. Re-request the reviewer (`gh api -X POST repos/dashpay/base-sdk/pulls/N/requested_reviewers -f 'reviewers[]=<login>'`). If a fix will take more than about 2 h, post a heads-up saying what's done and when the push lands, then nudge or unstick its fixer. Put the sweep's one-line result in the heartbeat line.
- base-sdk upstream policy (kwvg, 2026-10-15) binds every base-sdk PR: AGENTS.md; no expository comments; "reference implementation" instead of "Dash Core"; no C++ identifiers; corpus entries with citations (PR #20 style) for conformance claims; themed PRs of 6–14 non-churning commits; at most 5 open PRs; rebased onto develop; no tautological tests; `just comb` passes on every commit; descriptions not sparse.
- Manager-owned scratch lives under `/work/scratch/mgr/`. Cleanup pings to task agents name exact paths.

## Messaging between threads (DEC-386, 2026-10-23)
- `t3_thread_send` with mode `auto` (the default) *steers* a busy Claude thread, and a steer ends the target's turn and aborts its in-flight tool calls. The calls that hadn't started come back with Claude Code's stock rejection text ("The user doesn't want to take this action right now. STOP…"), which reads like a human refusal but isn't. That text cost about 8 h of held work on 10-22/23 and stalled child threads the same way.
- Use mode `queue` for anything that can wait for the target's turn to end: interim reports to the manager, FYIs, cleanup pings, answers to questions the target isn't blocked on. Use `auto`/`steer` only for a deliberate in-flight redirect (stop or cap a running job, a ruling the target is blocked on).
- Every agent contract (IMPLEMENTER, DW-IMPLEMENTER, DESIGNER, REVIEW-COMMON) tells agents to treat a STOP result that arrives with a new message as a steer, re-check state and re-run what is still needed.

## Escalate to pasta
Anything that needs blocked hosts, GitHub pushes or PRs upstream, product decisions not covered by the PRD, or coordination with upstream maintainers (e.g. base-sdk changes).
