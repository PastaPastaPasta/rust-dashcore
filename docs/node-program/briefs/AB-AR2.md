# A/B calibration brief: AR2 (DEC-93)

**Task AR2: `dash-rpc-spec`** (PLAN §4 row AR2; A1-architecture.md row `dash-rpc-spec`, ~line 581).

A shared library crate `dash-rpc-spec`: a declarative table of every Dash Core v24 RPC, **generated from Core** by a generator script with a provenance note, never hand-typed. For each RPC it holds:
- name, and Dash two-word sub-commands (`protx register`, `quorum info`, …);
- category;
- args: name, type, optional or default, `a|b` aliases, `skip_type_check`;
- hidden or wallet flags;
- the whitelist name;
- the **verbatim help text**;
- the PRD §5 class (see PRD.md §5).

It also holds the **client conversion table**, equal to `src/rpc/client.cpp`'s `vRPCConvertParams` at v24.

Getting the data:
- The generator should get help text by running a real regtest dashd v24.0.0-rc.3 (binaries via `contrib/setup-dashd.py`, or the release under /work/scratch/bs4a-decode-domain/dashd/), on your own datadir and port. Call `help` and `help <cmd>` for each command, including wallet commands with a wallet loaded, and Dash sub-command help.
- Parse the structure from Core's source (RPCHelpMan definitions) where help text alone isn't enough.

Acceptance:
- for every command, `help <cmd>` rendered from the table is byte-equal to v24 dashd's output;
- the conversion table equals `rpc/client.cpp`;
- a test re-runs the comparison against a live regtest dashd (gated like the existing dashd tests; read CLAUDE.md "Integration Tests (dashd)").

Add the crate to the workspace, deny.toml/R2 classification and CI groups. Run the full gate copy: `cp /work/scratch/gate.sh` to your scratch and point it at your worktree.

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
