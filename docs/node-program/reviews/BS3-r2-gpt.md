# BS3 independent review — round 2

**Verdict: CHANGES-REQUIRED. No blocker; two major findings.** The original path-override reproducer is fixed, the conformance intermediary is closed, the hosted/local CI boundary is now executable, and both selftests pass. Independent probes found another source-integrity bypass and another R2 intermediary bypass.

Reviewed `node/bs3-base-sdk-fork` at **d74f94a6a2a40c01e52b456732c20ae0f631824d**, including the four follow-up commits `b7197f1c`, `b892b816`, `0c3f7ebd`, and `d74f94a6`, and `node/bs3-fold-bs2` at **57fdade7a9ce0b014054f31a61160bf8a3679e96**. Compared the follow-up changes against the round-1 head `10f2702d`; inspected the new fold and the relevant A1 R1/R2 requirements. Used the code-review-validator workflow. All mutations, compilation, and merge experiments ran in independent scratch clones under **`/tmp/bs3-r2-review.Q55609/`**. The reviewed worktrees and actual fork were not modified. No commits, pushes, or GitHub messages were made.

## Findings

### 1. Major — Git replacement objects defeat the new clean-checkout verification

**Location:** `contrib/node/base_sdk_pins.py:250–256`, and the `git()` helper at line 58.

The guard checks the correct metadata source and checkout directory, but trusts Git's interpreted HEAD/tree and status. Git replacement objects can change that interpretation without changing the HEAD commit ID. This allows modified cached SDK code to pass the new integrity check.

**Reproducer:** In a private copy of Cargo's git checkout, append `compile_error!("BS3_R2_CACHE_REPLACEMENT");` to `pkgs/num/src/lib.rs`. Save the original HEAD tree ID, stage only the modified library file, write the resulting tree, and install a replacement for the original tree:

```bash
old_tree=$(git rev-parse 'HEAD^{tree}')
# Append the sentinel to pkgs/num/src/lib.rs here.
git add pkgs/num/src/lib.rs
new_tree=$(git write-tree)
git replace "$old_tree" "$new_tree"
```

This writes blobs/trees and a local replacement ref, **not a commit**, and moves no branch. HEAD remains `33fcc59e1868c803f9608e0e78ee8bddf3abcee7`. Ordinary `git status --porcelain --ignored` reports only `.cargo-ok`, which the guard explicitly permits. The index flags are ordinary flags, not assume-unchanged or skip-worktree. Metadata still reports the exact pinned git source and its normal manifest path under this private `$CARGO_HOME/git/checkouts`.

**Observed:** `check-base-sdk-pins.sh` exits **0 on both BS3 and the fold**. `cargo check --locked -p dash-num` then exits **101 at the sentinel in the modified checkout**. In contrast, `git --no-replace-objects status --porcelain --ignored` reports the staged library modification. The manifest, lockfile, actual fork, compiler, and Cargo configuration were unchanged.

This is narrower than round 1's blocker: it requires modification of the local Cargo checkout's Git metadata, so a normal committed Cargo config alone cannot distribute it. Nevertheless, clean-checkout verification is explicitly intended to reject modified cached SDK code, and here it certifies code that differs from the pinned commit. It uses Git's standard replacement feature, without a compiler wrapper or changed checker executable.

**Fix:** Disable replacement-object interpretation for provenance checks, for example with `git --no-replace-objects` or `GIT_NO_REPLACE_OBJECTS=1`. Check Git subprocess return codes and fail closed if an integrity query fails. Ensure the queried worktree is the actual checkout being compiled. Add a regression case that installs a replacement tree, leaves the lockfile unchanged, and requires the guard to reject it. Both the original metadata-source checks and the additional clean-cache checks should remain.

**Evidence:** `replacement-proof.py`, `replacement-clean.log`, `replacement-head.log`, `replacement-status.log`, `replacement-raw-status.log`, `replacement-bs3-pins.log`, `replacement-fold-pins.log`, and `replacement-build.log`. The private replacement checkout remains under `replacement-home/`; it does not share checkout files with the real Cargo home.

### 2. Major — R2 traversal treats excluded packages as a hole in the dependency boundary

**Location:** `contrib/node/r2_reach.py:48–49,88`; `deny.toml:7`.

The new checker removes every `[graph] exclude` package from traversal, matching cargo-deny's exclusion. The existing `dash-spv-bench` exclusion is reasonable for an internal binary's optional profilers, but becomes a route into the stack as soon as that package also supplies a library. The closure check does not cover it because it is absent from the wrappers lists.

**Reproducer:** Without editing `deny.toml`, any wrapper, or either guard, add:

```rust
// dash-spv-bench/src/lib.rs
pub use dashcore::Transaction;
```

Add workspace library `dashd-review`, assign it to the existing `node-runtime` CI group, and give it:

```toml
[dependencies]
dash-spv-bench = { path = "../dash-spv-bench" }
```

Its library actually consumes the re-export:

```rust
pub fn accepts_dashcore_transaction(_: dash_spv_bench::Transaction) {}
```

**Observed:** All of these exit **0**:

- `check-base-sdk-pins.sh`;
- `check-r2.sh`, reporting no forbidden reachability;
- `cargo deny check bans`, reporting `bans ok`;
- `python3 .github/scripts/ci_config.py verify-groups`;
- `cargo check --locked -p dashd-review --lib`.

`cargo tree -p dashd-review --depth 2` shows `dashd-review → dash-spv-bench → dashcore`, as well as paths to dash-spv and the wallet stack. Thus a forbidden node crate can compile with a real dashcore type. Only ordinary source/manifests and group assignment changed. Today's benchmark is binary-only, so this is a demonstrated hole in enforcement of future changes, not an assertion that the clean branch already contains a forbidden node dependency.

**Fix:** Allow an excluded internal tool to be exempt as a traversal **starting point**, but do not prune it from paths originating in other workspace members. Alternatively, reject any incoming dependency from a non-excluded member to an excluded package. Keep cargo-deny's profiler exclusion if needed, and add a selftest where an excluded package becomes a library consumed by a node crate. Removing the traversal's `names[dep] not in excluded` condition would make this reproducer reach the existing restricted stack entries.

**Evidence:** `r2-excluded-lib-tree.log` and `r2-excluded-lib-proof-{meta,pins,r2,bans,groups,build}.log`. The complete compiling fixture remains in `attack/`.

## Disposition of round-1 findings

| Round-1 finding | Round-2 result |
|---|---|
| Blocker: Cargo path override leaves the lockfile pinned but builds alternate code | **Original reproducer fixed.** An override copy with a compiler sentinel leaves Cargo.lock byte-identical; metadata reports path sources and the guard rejects them. Cargo independently compiles the sentinel, confirming the probe is real. The new major above concerns a different clean-cache integrity gap. |
| Major: `dashd-* → dash-conformance → dashcore` | **Exact path fixed; R2 boundary still incomplete.** Both cargo-deny and the reachability guard reject it, including renamed build-dependencies, optional dependencies and inactive-target build-dependencies. The excluded-benchmark path above still passes. |
| Major: hosted jobs cannot fetch the local fork | **Closed for the deliberately local-only phase.** Planning skips Cargo jobs, and the upstream PR guard fails instead of presenting all skipped Cargo jobs as approval. Moving the source and adapting the pin checker remain explicit prerequisites to publishing/upstream integration. |
| Moderate: two selftests fail after the BS2 fold | **Closed.** Standalone BS3 passes **36** cases; the fold passes **38**. The unpatched/unused probes select suitable packages from the current graph instead of assuming dash-primitives/dash-pow are absent. |

## CI and regression verification

- **actionlint 1.7.9:** all workflows pass on both scratch clones, including the reusable workflow outputs, caller conditions and dynamic matrix exclusion. ShellCheck is unavailable; actionlint ran with its ShellCheck integration disabled. This is workflow syntax/static validation, not a hosted run.
- Executed the exact `ci-plan.yml` planning step with the real local pin, an absent local fork URL, and a synthetic remote URL. Results were respectively `hosted=false`, `false`, and `true`; empty-group JSON was valid. The absent-local test needed no git source fetch. `verify-groups` also passes with the absent fork because its metadata query is `--no-deps`.
- Inspected every workflow that resolves the workspace. Security/MSRV/docs/pin/no_std/test callers, fuzzing, sanitizers and the seed updater are guarded by planning. Pre-commit skips clippy while retaining the checks that do not resolve dependencies; the permanently disabled integration job remains disabled. The test matrix excludes empty groups: four on BS3, three after adding folded conformance.
- Checked the upstream failure condition against PRs targeting **dev/main/master**, a node/integration PR, upstream pushes, workflow_dispatch, and a nonlocal pin. The upstream PR cases run the deliberately failing job; the local integration PR case does not. No branch-protection configuration or live GitHub execution was verified. The workflow's existing push trigger lists main/dev, so its master-push condition is currently unreachable; master **PRs** are covered as requested.
- Both baseline clones pass pin, R2, cargo-deny bans and group checks. Independent probes reject legacy `.cargo/config` path overrides, config aliases with `--config`, and source/alias override environment variables; benign build-job/network settings pass. The shipped tests cover ancestor/member/CARGO_HOME configs and environment overrides. Private checkout probes reject ordinary edits, assume-unchanged, skip-worktree, untracked files and ignored files, and pass again after restoring the checkout.
- Folded `cargo test --locked -p dash-conformance` passes **23 tests**: 9 unit, 5 fixtures, 9 v24 findings; its doc tests also pass. `cargo test --locked -p dashcore-crypto --test base_sdk_pin` passes. `cargo fmt --all --check` passes. Targeted strict clippy for conformance and crypto, all features/all targets, passes. Its first attempt hit `/tmp`'s quota; after removing only this review's disposable targets, the retry with `CARGO_PROFILE_DEV_DEBUG=0` completed successfully.
- Bash syntax, Python compilation and follow-up diff whitespace checks pass. Merging the exact current BS2/BS3 heads in **both orders**, without commits, then applying the fold patch reproduces tree **`128ff4c168b56777a9ff747c602267a40eecd1df`**, exactly the tree of `57fdade7`.

## Scope and assumptions

The local-only fork design and no-push policy remain accepted task constraints. A source guard does not prove the behavior of a hostile compiler wrapper or deliberately different command-line Cargo configuration; those boundaries are now disclosed. The replacement-tree finding concerns the checker explicitly added to verify cached source integrity, not those toolchain exceptions.

No full workspace test/clippy run, live dashd integration, live testnet oracle corpus, default full bump gates, publication, or hosted workflow was performed. Existing conformance findings are preserved; no oracle-equivalence claim beyond the fixture suites is made. A probe using `core.worktree` did **not** reproduce a bypass: Cargo restored that checkout before compiling, so it is not a finding.

**Approval status: CHANGES-REQUIRED.** Close the raw pinned-tree integrity gap and preserve R2 reachability through excluded packages. The original ordinary override, conformance, CI and fold-selftest fixes otherwise behaved as intended.
