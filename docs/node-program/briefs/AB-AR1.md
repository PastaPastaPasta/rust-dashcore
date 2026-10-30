# A/B calibration brief: AR1 (DEC-93)

**Task AR1: `dash-args`** (PLAN §4 row AR1; A1-architecture.md §1.x row `dash-args`, ~line 583).

A shared library crate `dash-args`: a faithful port of Dash Core's `ArgsManager` (src/common/args.{h,cpp}, src/util/settings.{h,cpp} and their v24 equivalents). It must cover:
- `dash.conf` parsing, with network sections (`[main]`, `[test]`, `[regtest]`, `[devnet]`) and `includeconf`;
- NETWORK_ONLY options;
- `-no` negation and the double-negation rules;
- command-line versus config-file precedence, and `settings.json` (read, write, and precedence);
- chain selection (`-testnet`, `-regtest`, `-devnet=<name>` and the conflict errors);
- `-datadir` and `-conf` resolution;
- help generation;
- the **full option table**: every option name Core v24 registers, with its category and flags, extracted from Core's source by a generator script and not hand-typed. Keep a provenance note for it.

Acceptance:
- a port of Core's `getarg_tests` and `settings_tests` (src/test/getarg_tests.cpp and settings_tests.cpp at v24), with the same cases and expectations;
- every Core option name recognized;
- error messages byte-identical to Core's where Core prints them.

The functional tests (`feature_config_args`, `feature_includeconf`, `feature_settings`) can't run yet, because there is no rdashd. Instead, list the behaviours they check and add Rust tests for each that doesn't need a running node.

Add the crate to the workspace. Classify it in deny.toml and the R2 rule (a shared library, A1 §1.1); R2 is suspended by DEC-81, but keep the tooling green. Add it to the CI groups (`.github/ci-groups.yml`, `ci_config.py verify-groups`).

Run the full gate copy: `cp /work/scratch/gate.sh` to your scratch, then point its worktree path at your worktree.

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
