# Program process

## Operating model (DEC-973, pasta 2026-10-31): landing owners
The manager sets the rules, priorities and critical path, rules on escalations and audits after the fact. It does not gate routine work. **The bars below are unchanged; what changes is who applies them.**

- **Landing owners.** Every task, slice or BL item has one owner thread (OWNERS.md). The owner is accountable for getting it *merged*, and runs the whole loop with its own delegates: plan, implementation, reviews, fix rounds, mutants, gates, the CI-S0 snapshot, and the hand-off to TRAINS (or a direct merge where the rules allow it). It reports outcomes, not requests. Contract: `briefs/OWNER.md`.
- **What an owner decides alone** (no manager ruling needed):
  - **Review findings.** Accept, fix, or defer to a filed BL with a reason. If the owner disputes a major finding, a fresh reviewer from the other provider arbitrates. A dispute that survives arbitration goes to E4 or E5.
  - **Delta checks and narrow re-reviews** after a fix round, and the READY ruling.
  - **CI-S0 snapshot requests**, sent straight to CI-S0.
  - **Scratch, worktree and branch cleanup** for its own items.
  - **Filing BL rows** (`BACKLOG.md`, under the commit lock) and owner decisions. Owner decisions are logged as `OD-<OWNER>-n` in `status/<OWNER>.md`; DECISIONS.md holds manager rulings only.
  - **Model and effort** for its delegates, per the DEC-92 table.
- **READY** (unchanged bars; the owner certifies):
  - two independent reviews, Opus-h and GPT-6.1-Sol, each fresh `delegate_task`s. Each is ACCEPT, or every major and minor finding is fixed and closed by a narrow re-review from the reviewer that raised it;
  - `RDC-CI GREEN` on the exact head (`rdc-ci-verdict-int.sh`);
  - has-trims, `cargo metadata --locked`, a clean status, signed commits;
  - for consensus work, the DEC-32 receipts. TRAINS takes them on the train in the gate's DEC-32 step; owners never read the restricted trees.
- **TRAINS (SB1)** cuts and merges trains end to end with no manager GO. A rider is any READY head; SB1 runs the two-parent pre-checks and the DEC-783 merge rule, and posts the integration SHA to the owners and the CLERK. A train's head must be RDC-CI GREEN as a whole.
- **Priorities.**
  1. CP: CH7a-4b → CH7a-5 → CH7b-1 → CH7b-2 → CH7b-3, with EV1-5b as their dependency. It is first in the CI-S0 queue and first to a gate slot. Within the caps, other owners defer a heavy start when a CP unit is waiting.
  2. SW-V24's pin-move train (mainnet V24 signalling begins 2026-11-09).
  3. Everything else, oldest READY first.
- **Commits to node-program docs** go through `/work/scratch/mgr/np-commit.sh -m MSG FILE...` (one lock; it runs the commit gate). Anything that writes `rdc-wt/integration`, train merges included, takes the same lock (`flock /work/scratch/mgr/np-commit.lock …`).
- **STATUS.** Owners append outcome lines to their own `status/<OWNER>.md`. The CLERK rolls them up into STATUS.md events and the task table. Only the CLERK and the manager edit STATUS.md.
- **Escalation list.** Only these reach the manager; everything else is decided below it:
  - **E1, outward-facing or pasta-owned:** dashpay/* GitHub replies, pushes and reviewer re-requests; upstream PRs; anything on the pasta ask list; messages to pasta.
  - **E2, restricted material:** a possible Core weakness (say only "possible security item, see manager"; nothing on disk); anything needing the restricted notes tree or a /work/cache install (oracle pin moves); codename wording questions.
  - **E3, a bar change or waiver:** a skipped or single review, a CI override (`--override DEC-n`), a rerun beyond the rule, an expected-red list that isn't in an approved plan, any PROCESS change.
  - **E4, consensus parity:** accepting any deliberate behaviour difference from Core (a new ALLOWED-DIFFS or D-row), or a reviewer–owner dispute about Core semantics that survives arbitration.
  - **E5, cross-owner:** an interface or design dispute between owners, a resource conflict the caps and priorities don't settle, or train ordering SB1 can't settle.
  - **E6, plan:** opening, closing or reprioritising rows; critical-path changes.
  - **E7, box emergencies:** /work under 190G that the CLERK's cleanup can't recover, or sustained load over the cap.
- **Manager audits.** After the fact, the manager spot-checks at least one landed item per day and every train. It confirms the verdict on the exact head, that the reviews exist and are closed, and the receipts, and records any gap as a PROCESS fix.

The sections below describe the bars; where they say "the manager" for routine steps (snapshot requests, delta checks, merges, heartbeat), read "the owner", "TRAINS" or "the CLERK" as above.

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
6. **Merge** (DEC-783): when the reviews are accepted and `RDC-CI GREEN` is in for the exact head, merge. PF0 is not a merge gate; it runs after the merge. The text below on gate trees and ledger lines is historical. Earlier rule: when the reviews are clean, the manager runs the PF0 counter gate on the branch (below), merges it into `node/integration` with `--no-ff` (the merge commit's tree must equal the ledger line's `measured_tree`), copies the gate's ledger line into the committed ledger, and updates `STATUS.md`. If `node/integration` moved during the gate (usually docs-only commits), make the gated merge on the gate's `base` in a temporary worktree (its tree then equals `measured_tree`) and merge that commit into the current tip with `--no-ff`; never rewrite integration history. Changes on integration since the gate's base to any PF0 bench input (as perfgate's README §4 defines it: the selected benches' package sources, `[[bench]]`, dev-dependencies, build scripts and their local closure, `Cargo.lock`, toolchain) mean a re-run; changes outside that set (docs, contrib scripts, packages outside every bench's closure) do not.

## Gates on node/integration
- **Tier 1 runs on GitHub** (DEC-705, DEC-715, from 2026-10-29 09:45Z). This replaces the local gate-fast tier 1 for READY and train candidates.
  - **The required check** is `contrib/node/rdc-ci-verdict.sh <label> <full-sha>` printing `RDC-CI GREEN`, for a `node-gate` run on PastaPastaPasta/rust-dashcore of the exact head. For a train, that's the train head.
    - Only a push of the `ci/<short>` snapshot counts, or a dispatch on master with `source=<sha>`. PENDING, RED and ERROR are not green.
    - Paste the verdict line into STATUS.
  - **Snapshots:**
    - Only the manager or CI-S0 builds and pushes them: `contrib/node/check-fork-push.sh` with checks 1 and 7 PASS, then the push script.
    - Implementers never push. The landing owner requests the snapshot from CI-S0 directly (DEC-973).
    - Fork master carries `node-gate.yml`, and a snapshot uses master's copy of the workflow.
    - **Text trims first** (DEC-723). Before reporting a head, the owner checks that integration's text-trim commits are in it: today that's 2f530258b and d1b4ffe6d.
      - A trim counts as present if it is an ancestor, or if a commit on the head carries the `git cherry-pick -x` trailer `(cherry picked from commit <full sha>)`. Ancestry alone can't see a cherry-pick.
      - `/work/scratch/mgr/has-trims.sh <repo> <head>` runs both checks.
      - If one is missing, `git cherry-pick -x` it as its own signed commit, without opening the changed passages.
      - A follow-merge of current integration also brings them in.
      - CI-S0 refuses a head that lacks one before running check 1.
      - When the manager adds a trim to integration, it joins this list and the helper.
  - **Still local and still required** (CI-FORK.md has the step inventory):
    - `oracle-diff`;
    - the private tests (DEC-32);
    - `selftest-checks.sh` is **not** a merge gate (DEC-783). It runs nightly on integration, and a failure opens a BL to the checker's owner.
    - PF0 is **not** a merge gate (DEC-783). It is a post-merge and nightly measurement with a loose threshold.
    Farms, nightly chain suites and tier 2 stay local as before.
  - **Reruns:**
    - At most one `gh run rerun --failed` per source commit, and only for a test on the BL-20 list or a runner or infrastructure failure. The verdict line names what attempt 1 failed. Attempt 3 or later is RED.
    - Re-dispatching or re-pushing a source that has a red run, or a run cancelled after a job failed, needs a DEC passed as `--override DEC-n`.
  - **Disagreements:** a GitHub-vs-local disagreement is filed as a BL and investigated. Gate-fast tier 1 is only a fallback, when GitHub is unavailable for more than 1 h, and needs a manager DEC.
  - **Cache** (DEC-713): `ci/*` push runs only restore it. The dependency closure is saved only by master-scope `deps_only` dispatches. A key miss prints the dispatch command.
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
- **PF0 since DEC-783 (2026-10-29): a post-merge and nightly measurement, not a merge gate.** It runs on a small set of headline workloads (sync, block validation, storage). A clear regression opens a BL to the owner: about 5–10% on a headline row, or 2× on any row. Everything else is logged and skipped: no approvals, no noise rulings, no re-anchor that blocks a merge (re-anchor in the background). The text below describes the tool; its "blocks the merge" rules no longer apply. BL-527 revisits this near the end of the plan.
- **PF0 counter gate** (A1 §3.11, DEC-23, DEC-52): historically a merge gate the manager ran, from the integration worktree: `contrib/node/perfgate/run gate --head node/<slug>`. It is not an implementer step; implementers may run it for advice with their own `PERFGATE_ROOT=/work/scratch/<slug>/perfgate`.
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

- Bar: Sol ACCEPT or APPROVE-WITH-NITS, or the landing owner's narrow delta check for small fix-forwards. The owner merges (DEC-973). Merge with `--no-ff` to main, then push. Main CI must go green afterwards; the Windows Swift job is a known non-blocking gap.
- A branch behind main follow-merges main first, then runs tsc, vitest, the Playwright specs it affects, and the Rust checks if the delta touches Rust.
- Any change to an m1–m4 §4 error-code table, or to any `docs/contracts` file a Swift test parses, runs the Swift T0 suite (`scripts/linux-docker-test.sh` on agentbox) before READY. The Rust-only desktop gate does not cover it. Lesson: DP3-01 added `send.cancelled` without a Swift binding, and main went red (10-15).

## Review rounds (DEC-985, pasta 2026-10-31; adapted from the `sol-loop` skill)
- **R1 Budget.** Code reviews: 3 rounds per reviewer. Extend to at most 5 only while a blocker or major with evidence is open in consensus or Core-divergence, security, persistence/crash/flush or concurrency code. At the cap the owner rules or escalates. Plan and design reviews: 3 rounds, then the owner rules; council only for consensus-critical designs.
- **R2 Early stop.** A round with no blocker and no major ends the loop: the owner fixes the minors and nits, checks them, and goes to READY with no further round. Doc, comment and format fixes never need a round.
- **R3 Narrow means narrow.** A re-review covers the fix diff, its callers and the open finding ids. Anything new outside that scope becomes a BL row with an owner, not a merge blocker, except a blocker in consensus code.
- **R4 Severity.** Blocker: the intended case is wrong, or the node diverges from Core on a reachable path. Major: a real bug callers or nodes will hit. Minor: a real bug that doesn't block the merge (BL row). Nit: everything else; decline freely. Rare paths in consensus, persistence, crash/flush, reorg or abort code are not downgraded for being rare. A major confined to test or tooling code does not block a product merge; it gets a BL row.
- **R5 Visible re-grades.** The owner may re-grade a finding; every downgrade goes in the owner's ledger (status/<OWNER>.md) with one line of evidence. The manager samples downgrades in audits.
- **R6 One reviewer chain** for routine items. Two (Sol + Opus) only for consensus, security and persistence/crash code.
- **R7 Minimal fixes.** Reviewers don't widen scope beyond the item's intent. Mutant, fault-sweep and Core-differential gates stay required; reviewers don't flag them as unneeded tests.
- **R8 Disputes.** After two replies the owner rules. A dispute reaches the manager only through the E-list.
- Applies to in-flight items from 2026-10-31 05:2xZ.

## Heartbeat (every 30 min, CLERK since DEC-973; escalations to the manager by the E-list)
- Open-row sweep and M3 exits block (DEC-976): every beat, the CLERK lists open rows whose dependency or wait condition has cleared, or that have had no owner or event for > 24 h, and pings the owner (or lists unowned rows to the manager, E6); it folds merged items into stale rows, checks the desktop floor (DEC-648), and keeps STATUS's `M3 exits` block (one line per PLAN M3 exit, owner ETAs only).
- Poll every running/in-review/fix-round/design task with `task_status`; process finished ones, resume cancelled ones, unstick stalled ones.
- CPU audit (pasta, 2026-10-14): don't just report load. List running user units and processes older than 1 h above 5% CPU, and map each one (cwd, then worktree, then task) to a live STATUS row. Fuzzers must be bounded (`-max_total_time`, `--cpu-hours`), and long legs must still be gaining coverage (DEC-184). Kill or rework anything orphaned or plateaued. Report a one-line unit-to-task breakdown.
- Open external PRs (pasta, 2026-10-15, after kwvg's review on base-sdk #65 sat unseen for 9 h): every heartbeat, call `list_thread_pull_requests`. Run `~/.claude/skills/babysit/scripts/pr_wait.py once` over all open PRs, handle and ack its events, re-arm `watch_pull_request` on any open PR showing `watching: false` (watches lapse on restarts), and record merges and closes in STATUS.
- Open-PR review sweep (pasta, 2026-10-22, after kwvg's #73 review sat 11 h with no answer while its fix waited on the comb lock, and #74 sat answered but never re-requested): every heartbeat, also run `python3 tools/pr-review-sweep.py`. pr_wait reports an event once; once a batch is acked into a fixer, nothing reminds you again. The sweep re-reads the current state of every open base-sdk PR and flags:
  - UNANSWERED: a human thread or comment whose last word isn't ours;
  - STALE-ASK: CHANGES_REQUESTED on a head we haven't pushed past yet;
  - REREQUEST: every thread answered and the head moved, but the reviewer isn't requested.

  Act on each flag in the same heartbeat. Re-request the reviewer (`gh api -X POST repos/dashpay/base-sdk/pulls/N/requested_reviewers -f 'reviewers[]=<login>'`). If a fix will take more than about 2 h, post a heads-up saying what's done and when the push lands, then nudge or unstick its fixer. Put the sweep's one-line result in the heartbeat line.
- base-sdk upstream policy (kwvg, 2026-10-15) binds every base-sdk PR: AGENTS.md; no expository comments; "reference implementation" instead of "Dash Core"; no C++ identifiers; corpus entries with citations (PR #20 style) for conformance claims; themed PRs of 6–14 non-churning commits; at most 5 open PRs; rebased onto develop; no tautological tests; `just comb` passes on every commit; descriptions not sparse.
- base-sdk checks (DEC-690, pasta 2026-10-29): green fork CI (GitHub Actions on PastaPastaPasta/base-sdk, dispatched with `/work/scratch/ci-stage1/tools/fork-ci-dispatch.sh`) on the exact head is the required check for READY-PUSH, `node-sdk-integration` landings and base-sdk branches. No local CodeQL comb as a gate; one local comb only as a manual last look right before pushing to a dashpay/* PR. Any GitHub-vs-local disagreement is filed as a BL and investigated. rust-dashcore follows the same rule since DEC-715 (see *Gates on node/integration*).
  - Wiring (DEC-691): push the READY-PUSH head to fork staging `ci/<theme>` first, dispatch, require `fork-ci-verdict.sh <label> <sha>` GREEN, then push the dashpay PR branch to the same SHA. dashpay/* bodies never link fork runs. DEC-239 landings: push, `fork-ci-dispatch.sh node-sdk-integration <old> <new>`, `fork-ci-verdict.sh node-sdk-integration <new>`.
- After every train merge (DEC-692): run the merged PF0 ledger's `anchor_obligations` (e.g. `run anchor <tip> --only <row>` for narrowed refusals) as well as the bi.py bench-row re-anchors, before the next PF0 measures.
- Manager-owned scratch lives under `/work/scratch/mgr/`. Cleanup pings to task agents name exact paths.

## Messaging between threads (DEC-386, 2026-10-23)
- `t3_thread_send` with mode `auto` (the default) *steers* a busy Claude thread, and a steer ends the target's turn and aborts its in-flight tool calls. The calls that hadn't started come back with Claude Code's stock rejection text ("The user doesn't want to take this action right now. STOP…"), which reads like a human refusal but isn't. That text cost about 8 h of held work on 10-22/23 and stalled child threads the same way.
- Use mode `queue` for anything that can wait for the target's turn to end: interim reports to the manager, FYIs, cleanup pings, answers to questions the target isn't blocked on. Use `auto`/`steer` only for a deliberate in-flight redirect (stop or cap a running job, a ruling the target is blocked on).
- Every agent contract (IMPLEMENTER, DW-IMPLEMENTER, DESIGNER, REVIEW-COMMON) tells agents to treat a STOP result that arrives with a new message as a steer, re-check state and re-run what is still needed.

## Escalate to pasta
Anything that needs blocked hosts, GitHub pushes or PRs upstream, product decisions not covered by the PRD, or coordination with upstream maintainers (e.g. base-sdk changes).
