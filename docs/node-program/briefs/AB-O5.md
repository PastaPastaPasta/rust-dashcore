# A/B calibration brief: O5 (DEC-93)

**Task O5: quirk-register scaffolding** (PLAN §4 row O5).

R4's audit lists 34 Dash Core consensus quirks, and A1 rev1 adds 35–38. Find them in ~/workspace/node-program/research/ (the R4 report) and designs/A1-architecture.md §1.4 and around line 369 ("New quirks"). Build the register:
1. A machine-readable quirk register at `docs/node-program/quirks.toml` (or the location A1 names, if it names one). For each quirk give: id, a one-line neutral title, the Core v24 file:line, the owning task (quirk 1: CS0; 34: BS5; 35/37: BS11a-t; 36: LQ2; 38: LQ3; others per R4/A1), and its status.
2. **One test per quirk, 38 in total,** in the owning pure crate where it exists. dash-consensus, dash-x11, dash-sha256 and dash-params via base-sdk exist today. Use dash-conformance, or a `quirks` integration-test target, where the owning crate doesn't exist yet.
   - A quirk that's already implemented and tested gets a test that asserts the Core behaviour with Core-produced vectors (oracle or Core test data), or a test that references the existing one.
   - Unimplemented ones get `#[ignore = "quirk N: owner TASK"]`.
3. A check script, wired into `contrib/node/selftest-checks.sh` or a small test, asserting that the register and the tests agree: every id has exactly one test, and ignore reasons name the owner. It also prints the ignored count for STATUS.

**Quirk 1 is private** (PLAN §7). Its register entry and test must say only "quirk 1: see PLAN §7 handling". Its real test already exists in the private tree (`contrib/node/check-private-tests.sh`); reference it. Never describe it.

v24 only (DEC-82): where R4 lists a quirk as v23.1-only, mark it `dropped (DEC-82)` and add no test. Run the full gate copy: `cp /work/scratch/gate.sh` to your scratch and point it at your worktree.

**Contract.** Read and follow ~/workspace/node-program/briefs/IMPLEMENTER.md. It covers the read-first list, where to work, the hard rules, the definition of done and the final-reply format. Program decisions DEC-81…DEC-92 (DECISIONS.md) apply. In particular:
- **v24 only** (DEC-82): the Core reference is the v24.0 pin (`~/workspace/dash`, tag v24.0.0-rc.3; worktrees under ~/workspace/dash-wt). Don't add v23.1 variants.
- **Portable code** (DEC-84): no Linux-only assumptions.

**Worktree.** Create your own, named in this prompt, from `node/integration`:
`git -C ~/workspace/rust-dashcore worktree add -b <branch> <path> node/integration`.
Never push.

**Calibration run.** This task is part of a model calibration (an A/B). Another agent is implementing the same brief independently in a different worktree. Don't look at other `ab-*` worktrees or branches.

**Your report.** Make it complete, because it is compared directly. Give:
- the head SHA;
- the commits;
- the acceptance evidence with pass counts;
- the gate results;
- deviations from the brief;
- known gaps.

**Scratch, builds and long jobs.**
- Scratch: /work/scratch/<your-branch-slug>/.
- TMPDIR=/dev/shm/<your-branch-slug>.
- CARGO_BUILD_JOBS=12.
- Run long jobs under `systemd-run --user`, and poll them yourself. Stay in the turn until you're done.
