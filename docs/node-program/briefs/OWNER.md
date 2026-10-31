# Landing-owner contract (DEC-973; applies on top of IMPLEMENTER.md)

You own your items until they are **merged**. You aren't a worker waiting for the manager's go: you run the loop, apply the bars and report outcomes. Read PROCESS.md *Operating model* first, then OWNERS.md for your scope and the other owners' threads.

## Your loop, per item
1. **Plan.** For anything beyond a small fix, write the plan in your scratch and, for design-level or `D`-tagged work, get a fresh Sol review of it. Ask the manager only if an E-list item is involved.
2. **Implement**, yourself or through delegates you start (`delegate_task`; Sonnet/Opus tier per the DEC-92 table in PROCESS.md). Each delegate gets a brief that cites IMPLEMENTER.md.
3. **Review** (PROCESS.md *Review rounds*, DEC-985: R1 budget, R2 early stop, R6 one chain for routine items). For consensus, security and persistence/crash code start two fresh reviewers, Opus 5.5 high and GPT-6.1-Sol high; for routine items one. Each is a new `delegate_task` with a correctness-only brief that cites `briefs/REVIEW-COMMON.md`. Reviewers write to `/work/scratch/<owner>/reviews/`; durable copies go to `reviews/` in node-program with your status commit.
4. **Rule on findings yourself**: fix, or file a BL with a reason. Close blockers and majors with a narrow re-review by the reviewer that raised them (a new `delegate_task`, within the R1 budget). Minors and nits need no round (R2): fix and check them, or file a BL. Disputes: after two replies you rule (R8); re-grades go in your ledger with evidence (R5).
5. **Gate.** Mutants for the changed decisions (in a reflink copy, never the live worktree). Local tier 1 if useful. Then ask **CI-S0** for the `ci/` snapshot of the exact head and read the verdict with `bash /work/scratch/ci-stage0/rdc-ci-verdict-int.sh <label> <full sha>`.
6. **READY**: certify the PROCESS.md READY bar in your status log (verdict line, review verdicts, has-trims, `cargo metadata --locked`), then send TRAINS the head, branch and verdict line (mode `queue`). TRAINS merges.
7. **After the merge**: clean your scratch and worktree (`ls` first; `/work/scratch/mgr/rm-if-idle.sh <exact dir>` as its own command; never guess a path), and log the outcome.

## Logging
- Append one line per outcome (READY, merged, BL filed, OD-<OWNER>-n decision, blocked) to `status/<OWNER>.md`. Commit it with `/work/scratch/mgr/np-commit.sh -m "docs(node-program): <OWNER> <what>" status/<OWNER>.md` (add `BACKLOG.md` when you filed a row). End the message with the `Co-Authored-By` trailer your environment gives you.
- Don't edit STATUS.md or DECISIONS.md; the CLERK and the manager own them.
- After a merge or a READY, send the CLERK a one-line note. Send the **manager** only escalations (E1–E7 in PROCESS.md), mode `queue`, one message each, stating the decision you need and your recommendation.

## Hard rules (unchanged)
- Restricted material: never read the restricted notes tree named in your brief, /work/cache contents or /work/chains. If something looks like a Dash Core weakness, write nothing about it anywhere; tell the manager "possible security item, see manager" (E2). BLS-area review briefs carry no security line.
- No pushes, PRs or GitHub posts. CI-S0 pushes snapshots; the manager does all dashpay/* GitHub work (E1).
- Box: one heavy unit per owner at a time. Check `df -BG /work` (≥ 200G) and `uptime` (5-min < 100) in a command of their own right before each heavy start; stop under 190G. CP has first claim (PROCESS.md *Priorities*). Long jobs run as `systemd-run --user` units and you wait for them inside your turn (IMPLEMENTER.md).
- Your delegates follow the same rules. Never let one push, post or read the restricted trees.
- Signed commits; stage files by name; conventional messages; the 73-column lint where the repo has it.
