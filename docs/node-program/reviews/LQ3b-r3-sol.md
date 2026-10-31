CHANGES-REQUIRED — 0 blocker, 0 major, 1 minor, 0 nit

Narrow correctness re-review of signed commit `e5761f8c73b42cbc51002c633a6a591f06a85c51` (`G`), the complete delta from `3733246f9`, on frozen `node/lq3b`. Read REVIEW-COMMON, the r2 review, the implementer's RESULTS and runs/g evidence, and all three changed files. Independently validated in `/work/scratch/cp/reviews/lq3b-sol-copy2`, synced to the frozen head; all 22,045 tracked files matched before testing.

1. R2 item 1 — CLOSED; all six header forms still reject a second owner.

Repeated the real repository guard with a temporary `review-second-owner` package in the review copy. Tested plain `[workspace]`, a commented header, a final header without a newline, top-level `workspace = {}`, quoted `["workspace"]`, and dotted `workspace.resolver = "2"`. Repeated each form separately for only `reload` and only `mined`: all 12 `cargo metadata --offline --no-deps` calls succeed, and all 12 guard invocations exit 101, naming the added owner and the corresponding gated feature. Removed the synthetic package afterward.

Evidence: `review-evidence/r3/guard-probe-results.json`, `guard-probes.py`, and the individual `guard-{feature}-{form}*.log` files inside the review copy.

2. R2 item 2 — remains CLOSED.

The commit changes no runtime code or differential tests. The clean 165-test dash-evo library suite passes, including both differentials added in r1. The fixed-type mutation proof from r2 was not repeated in this narrow round.

3. R2 item 3 — CLOSED for the original default-target reproduction.

With `CARGO_TARGET_DIR` unset, a freshly compiled default in-tree target, and `TMPDIR=/dev/shm/lq3b-r3-sol-tmp` (0700), ran:

```text
cargo test -p dash-evo --all-features --locked --no-fail-fast
```

Exit 0: 213 passed, 1 ignored, including all three feature-gate tests. The headerless excluded `second` case remains in the test. The temporary values live through the `owners(...)` call and are removed afterward; there is no premature-drop defect. The new dev-dependency and one-line lockfile addition are consistent.

Evidence: `review-evidence/r3/baseline.log`, `baseline.exit`, and `guard-binary.txt`. The compiled feature-gate executable is under this copy's `target/debug/deps`, not an external target.

4. New finding — Minor: the probe still depends on TMPDIR's Cargo ancestry, and its assertion does not establish the needed isolation.

Location: `dash-evo/tests/feature_gates.rs:155` and `dash-evo/tests/feature_gates.rs:157` (assertion at line 158), interacting with the existing manifest canonicalization at line 97.

`tempfile::tempdir()` honors TMPDIR; it does not promise a directory outside Cargo projects. Independently ran the newly compiled `a_second_owner_in_its_own_workspace_fails_the_guard` executable with the default target unchanged and CARGO_TARGET_DIR unset:

- TMPDIR outside any Cargo project: pass (0).
- TMPDIR at `<copy>/target/tmp/r3-in-project`: fail (101) at the new ancestor assertion, even though the directory is writable and otherwise valid for temporary files.
- TMPDIR beneath an ordinary `[package]` with no workspace table: also fail (101) at that assertion. Separately constructed the headerless excluded-package fixture beneath an ordinary package and obtained successful Cargo metadata (0). An ancestor Cargo.toml alone is therefore an unnecessarily broad reason to reject the location.
- TMPDIR at a symlink outside the project pointing into `<copy>/target/tmp/r3-in-project`: the ancestor assertion passes, but the headerless case fails (101) with the original "current package believes it's in a workspace when it's not" error. `root.ancestors()` checks the lexical path, while `owners()` canonicalizes the excluded manifest before querying Cargo. Cargo then sees the real repository workspace above it. A direct metadata query through the symlink succeeds; the canonicalization in the complete guard is the distinguishing step.

Thus the original default-target failure is fixed in the tested external TMPDIR, but a valid ambient TMPDIR can still make the test fail. The new assertion is both stronger than necessary for ordinary parent packages and insufficient for symlinked workspace parents. This is a test-fixture portability defect, with no identified runtime impact.

Suggested fix: select an isolated probe location with a fallback when the ambient TMPDIR is unsuitable, and assess the same canonical ancestry that `owners()` ultimately passes to Cargo. Merely asserting on the lexical ancestry, or merely canonicalizing before asserting, does not make the fixture work with a valid in-project TMPDIR. Keep the excluded package without its own workspace table, and retain the successful default-target validation.

Evidence: `review-evidence/r3/tmpdir-results.json`, `tmpdir-probes.py`, `tmpdir-{case}.log`, `ordinary-parent-results.json`, `ordinary-parent-headerless-metadata.log`, and `symlink-headerless-metadata.log`.

Validation and constraints

- The only build ran under `flock /work/scratch/cp/heavy.lock`; it waited for the other holder and then completed successfully. Compiler wrappers were disabled, and CARGO_TARGET_DIR was explicitly removed from the environment.
- Standalone uptime/df checks before the queued build showed load5/free 61.00/229G at 04:13:45 UTC; the last check before lock acquisition showed 54.17/218G at 04:20:39 UTC. The build log was created at 04:21:39 UTC. Both thresholds were satisfied in the pre-build checks.
- `cargo fmt --all -- --check`: pass (0); evidence `review-evidence/r3/fmt.log` and `fmt.exit`.
- All additional probes used the newly built executable and metadata only; no additional builds were needed. Review-copy fixtures and the dedicated 0700 TMPDIR were removed afterward.
- Frozen worktree remains clean at e5761f8c73b42cbc51002c633a6a591f06a85c51. No branch changes, commits, pushes, memory writes, or reads of the restricted directory contents. No other clear correctness defect found in this commit.

Verdict: CHANGES-REQUIRED — 0 blocker, 0 major, 1 minor, 0 nit.
