# BS3 independent review — round 1

**Verdict: CHANGES-REQUIRED.** One blocker and two major findings, plus a broken integration self-test. The ordinary direct-dependency checks work, and the fold is mechanically correct in both merge orders, but R1 and R2 have demonstrated bypasses.

Reviewed all seven commits of `node/bs3-base-sdk-fork` at `10f2702d1e8b2a3852a898756a354b42b5188b4c` against `node/integration` at `b399cd00` using the three-dot diff, and fold commit `2afdb0488b4ffe735e5ed82342c30c49715e27cf`. Context: PLAN §4.1 and A1 §§1.1, 1.5–1.7 and 2.1. Used the code-review-validator workflow. Reviewed worktrees and the actual base-sdk checkout were left unchanged; no commits were made. All planted violations, builds and merge experiments ran in separate scratch clones under `/tmp/bs3-review.Tb4ezx/`.

## Findings

### 1. Blocker — the pin check validates the lockfile instead of the source Cargo actually builds

**Location:** `contrib/node/base_sdk_pins.py:179`, particularly lines 180–196; `crypto/tests/base_sdk_pin.rs:41` has the same lockfile-only limitation.

**Failure scenario:** Add a Cargo path override for a copy of `dash-num` with the same package version and dependencies:

```toml
# .cargo/config.toml in the scratch rust-dashcore clone
paths = ["/tmp/bs3-review.Tb4ezx/override/pkgs/num"]
```

The override copy was extracted from the pinned fork, so Cargo accepts its dependency graph. Cargo leaves the committed lockfile's `git+file://…?rev=…#…` entry intact. `check-base-sdk-pins.sh` exits **0**, reporting that every package resolves to the pinned fork. However, `cargo metadata --locked` reports the actual `dash-num` package with `source: null` and `manifest_path` under the override directory. The checker discards that metadata and examines only `Cargo.lock`.

For executable proof, I added `compile_error!("BS3_REVIEW_UNPINNED_CODE_EXECUTED");` to the override's `src/lib.rs`. The pin check still passed, while `cargo check -p dashcore --lib --locked` compiled the override and failed at that sentinel. This is an uncommitted, unpinned source used by a normal locked build, without changing any dependency declaration or lockfile entry. A committed repository Cargo configuration can introduce this bypass; it is not limited to a developer intentionally changing global settings.

**Fix:** Parse the successful full metadata result and validate the resolved SDK packages' actual `source` against the expected fork source, in addition to checking the lockfile. Include all features so optional SDK edges are checked as well. Reject SDK packages resolved from paths/configuration overrides. Add a path-override self-test that leaves `Cargo.lock` unchanged and proves the guard rejects the actual alternate build source. Retain the manifest checks and lockfile checks; they catch different errors.

**Evidence:** `pin-config-path-override.log`, `config-override-metadata.json`, `override-pin-check.log`, and `override-build.log` in the scratch directory.

### 2. Major — R2 permits a node crate to reach dashcore through an allowed intermediary

**Location:** `deny.toml:54` as changed by fold commit `2afdb048`; the completeness claim at `docs/node-program/BASE-SDK-FORK.md:50` is incorrect.

**Failure scenario:** In a scratch copy of the folded tree, add workspace library `dashd-review` with:

```toml
[dependencies]
dash-conformance = { path = "../conformance" }
```

`cargo deny check bans` exits **0** (`bans ok`), while `cargo tree -p dashd-review` includes `dashd-review → dash-conformance → dashcore`. `dash-conformance` is a real library, so this dependency can import its public code and APIs. It is an allowed direct parent of dashcore, and there is no rule on incoming dependencies to dash-conformance. This admits the dashcore stack to an otherwise forbidden `dashd-*` crate. Extending the wrappers with future R2 exceptions such as dashd-wallet or dash-testkit repeats the same problem unless their incoming edges are constrained.

The usual build-dependency, renamed/workspace-inherited build-dependency, inactive-target build-dependency and optional-feature cases **were rejected**. The problem is the graph boundary, not Cargo's handling of those tables/features.

**Fix:** Enforce R2 using reachability over the all-features metadata graph, checking each restricted node crate against the banned stack and applying exceptions to the consuming crate. Alternatively, close every permitted intermediary's incoming edges with additional cargo-deny rules and an explicit policy for exception crates. Add a self-test for a forbidden node crate depending on dash-conformance, and tests as the other exceptions become libraries. Correct the guide's assertion that every transitive path necessarily starts with a banned edge.

**Evidence:** `deny-via-conformance.log` and `tree-via-conformance.log` in the scratch directory.

### 3. Major — hosted CI has no way to obtain the mandatory local fork

**Location:** `.github/workflows/rust.yml:61` and `:69`; `Cargo.toml:47`; `docs/node-program/PROCESS.md:28` and `BASE-SDK-FORK.md:5`.

**Failure scenario:** The new jobs run on `ubuntu-latest` and only check out rust-dashcore. That runner does not contain `/work/home/agent/workspace/base-sdk`, its `node/base-sdk` branch, or its private pinned commits/tags. The pin job necessarily fails. Existing hosted MSRV, docs and nonempty test jobs also resolve the patched workspace and fail before compiling. The Windows and macOS jobs have the same problem. Comments saying these jobs run in the local gate do not change `runs-on` or provision their source.

I reproduced the missing-repository behavior in a scratch folded tree by replacing the fork URL in both the manifest and lockfile with an absent local repository. Even `cargo check -p dash-network --locked`, for an existing crate that does not itself use base-sdk, failed loading the `dash-num` patch. This is workspace-wide resolution, not just a node-crate problem.

The mandated local `file://` design itself is an explicit A1 decision, so merely choosing that URL is not the finding. The failure is declaring hosted CI without the required source and without an executable provisioning strategy. The documented local gate and “nothing is pushed” policy contain the damage while node branches stay private; they do not make the GitHub jobs runnable or make an upstream `dev` merge safe.

**Fix:** Supply an explicit runner/provisioning policy. For example, run the node gate on a suitably isolated self-hosted runner that has the fork at the committed path, or materialize a Git bundle containing the pinned commit and required branch/tags at that path before any Cargo resolution. Cover existing jobs too, not just the two new ones. If this phase is intentionally local-only, document that boundary and the prerequisite changes before publishing/merging upstream; do not represent the hosted jobs as operational CI. Avoid removing the patch and silently validating upstream SDK code when the node needs fork changes.

**Evidence:** `missing-fork-build.log`; no hosted workflow was launched.

### 4. Moderate — the required self-test fails immediately after the documented BS2 fold

**Location:** `contrib/node/selftest-checks.sh:93` and `:97`; fold commit `2afdb048` changes the premises of both cases without adapting the test.

**Failure scenario:** The self-test passes on BS3 alone. On the clean folded tree it exits **1** with two failures:

```text
FAIL wanted "dash-primitives` comes from git\+https", got fail: a base SDK package used without a [patch] entry (resolves from GitHub)
FAIL wanted "patch for `dash-pow` is unused", got pass: an unused [patch] entry
2 selftest(s) failed
```

The first case inserts `dash-primitives` into workspace dependencies even though the fold already declares it, so it tests invalid duplicate TOML instead of an unpatched source. The second adds a dash-pow patch, but dash-primitives already uses dash-pow transitively: that patch is now used. Thus the prescribed integration gate fails in either merge order, and one supposed planted violation is no longer a violation. Future SDK use will produce the same fragility.

**Fix:** Construct cases from the current manifest/lock graph. To test an unpatched direct SDK dependency, remove a chosen existing patch while preserving a single valid declaration and ensure the chosen package actually resolves upstream. For the unused-patch case, choose an SDK package absent from the current dependency closure, or create a controlled unused fixture. Run the self-test on the folded tree as part of the fold's validation, not only on standalone BS3.

**Evidence:** `selftest.log` (standalone passes) and `fold-selftest.log` (two failures).

## Pin-check adversarial coverage

The shipped self-test passed every case on standalone BS3. Independent additions also demonstrated rejection of:

- A renamed SDK dependency in `build-dependencies`.
- A renamed SDK dependency in an inactive target-specific build-dependency table.
- A second SDK path source introduced through a non-workspace external helper library; this bypasses direct member-manifest scanning but is caught by the lockfile source check.
- Direct, renamed/workspace-inherited and inactive-target build-dependencies on dashcore, and an optional dashcore dependency. Each cargo-deny failure was specifically `error[banned]`, not a resolution failure.

The shipped cases additionally cover direct git/path pins, dependency aliases, target dependencies, mismatched upstream/fork revisions, unpatched and unused entries on standalone BS3, and edited lockfile sources. I found no bypass through these ordinary declarations. A Cargo dependency alias does not rename the underlying package in metadata or the lockfile. Arbitrarily republishing SDK implementation code under a different actual package name is outside the identity-based policy this checker can prove.

## Lockfile, portability and published consumers

- A committed lockfile plus an immutable git revision is appropriate for the node binary and makes worktrees independent of the fork's checkout and uncommitted changes. Tags preserve old pinned commits through later rebases. Neither lockfile commitment nor tags distribute the git objects to another machine.
- A clean source checkout on another machine needs the repository and pinned objects at the exact absolute file URL, or deliberate manifest/lockfile adaptation. Cloning into the guide's portable-looking `~/workspace/base-sdk` alone does not suffice if that home directory is elsewhere. Existing caches can let some builds work without the source repository, but are not reproducible provisioning; the pin guard also requires the actual repository. Hosted and ordinary non-agentbox source builds have no supplied bootstrap procedure.
- Keeping the changes on local `node/*` branches, with local integration gates and no pushes, is the current mitigation. Before an upstream `dev` merge, the fork must be distributed/provisioned or the local patch must be replaced by a reachable immutable source. Removing the patch requires regenerating the lockfile and verifying that all necessary fork changes are available from the replacement source. Taking an upstream lockfile without reconciling the manifest is not a mitigation.
- Dependency consumers do not inherit this workspace root's `[patch]` or its lockfile. Existing published crate consumers therefore do not automatically fetch this `file://` source. A consumer of rust-dashcore as a git dependency normally resolves its SDK declarations from upstream instead. Once a public crate needs fork-only APIs, downstream resolution and publication need separate reachable dependencies/versioned releases; this patch cannot supply them. The pre-existing git-only SDK declarations already complicate packaging and publication, independently of BS3. I did not publish any crate or claim that the branch is publication-ready.

## Fold correctness

Merged the exact BS2 and BS3 heads in both orders in two scratch clones, stopping before any merge commit, then applied the fold diff to the index. Both merges and both fold applications were clean. Both final trees were **`bd70803ac844488f56978744c46746958a3f27f3`**, exactly the tree of `2afdb048`.

The fold correctly inherits SDK declarations, adds dash-primitives to both root tables, updates the lockfile, assigns dash-conformance to the node-conformance group, and allows the intended direct conformance→dashcore dependency. The folded pin check, cargo-deny bans check and CI-group validation pass. `cargo test -p dash-conformance` passed **23 tests**: 9 unit, 5 fixture and 9 v24 finding tests. Those are saved testnet/regtest fixtures and do not need a live oracle. The fold's functional result is sound; findings 2 and 4 concern policy and integration validation around it.

## Bump script, guide and shell quality

- No destructive git operation on a shared branch was found. The script resolves the requested commit, verifies ancestry, computes the upstream merge base, and performs base-sdk gates in a unique detached temporary worktree. Its force removal targets only that temporary worktree. It neither checks out nor resets/rebases the shared fork, commits rust-dashcore, nor force-moves an existing tag. The self-test's checkout/clean operations similarly target its own temporary worktree.
- Exercised `bump-base-sdk.sh --no-gates` against independent scratch clones of both repositories. It successfully rewrote the local source, regenerated the lockfile, checked the pin, reused the existing retention tag and left the manifest/lockfile changes uncommitted. No actual shared fork refs were altered. I did not run the expensive default full workspace/base-sdk gates or create a new SDK revision solely for this review.
- `set -euo pipefail`, quoting of git paths/refs, and the temporary worktree approach are appropriate. All new shell scripts passed Bash syntax validation. The Python checker passed syntax compilation in scratch, and the branch diff passed `git diff --check`. ShellCheck was not installed, so no ShellCheck result is claimed. The self-test uses GNU `sed -i`, fitting agentbox but not an unmodified macOS shell environment.
- The guide clearly discloses that the bump script does **not** run the full testnet oracle corpus requested in A1 §1.6, and instead requires a manual baseline comparison because known findings exist. Its default gates also do not include cargo-deny, the no_std guard or live dashd integration. Treat its successful completion as a pin move plus the listed tests, not as completion of the full node integration gate; record the manual corpus comparison before accepting a changed pin. This is a disclosed acceptance deviation, not a destructive-operation bug.
- `base_sdk_pins.py set` depends on a specific single-line `git = ..., rev = ...` ordering even though `check` accepts semantically equivalent TOML formatting. A reordered/multiline table can make bump fail safely before writing. That is a maintenance limitation rather than a blocker; document the formatting requirement or use structural edits if the accepted layout is broadened.

**Approval status: CHANGES-REQUIRED.** Fix the actual-source pin bypass, the transitive R2 boundary, and the folded self-test. Resolve the hosted-runner/provisioning boundary before claiming live hosted CI or carrying this plumbing into upstream `dev`.
