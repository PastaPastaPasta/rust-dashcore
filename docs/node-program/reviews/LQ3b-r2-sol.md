CHANGES-REQUIRED — 0 blocker, 0 major, 1 minor, 0 nit

Narrow correctness re-review of frozen `node/lq3b`, `89c6b40c4..3733246f9` (three signed `G` commits). Read the r1 report, REVIEW-COMMON, the complete delta, and implementer RESULTS/mutant evidence. The original minor and the two-type coverage note are closed. A new test-fixture defect needs fixing.

1. Original minor: CLOSED — workspace discovery uses TOML semantics.

`owners()` parses every discovered manifest and reads workspace roots through Cargo metadata. It also reads package manifests absent from those results, covering excluded packages such as dashd-store-bakeoff.

Independently repeated the exact r1 manifest in the review copy: a standalone `review-second-owner` depending on dash-evo with only `features = ["mined"]`, ending in `[workspace] # independent tool`. Its `cargo metadata --offline --no-deps` succeeds; the newly compiled `unverified_quorum_features_have_one_owner_each` fails with exit 101 and names `review-second-owner` as a second `mined` owner. Repeated with only `reload`: also rejected for that feature.

Also independently checked plain and commented headers, `[workspace]` as the final line without a newline, top-level `workspace = {}`, a quoted `["workspace"]` header, and a dotted `workspace.resolver = "2"` definition. Cargo accepts all six, and the compiled guard rejects the added owner in all six. Evidence: `lq3b-sol-copy2/review-evidence/guard-exact-results.json`, `guard-probe-results.json`, and their individual logs.

2. Two-type coverage note: CLOSED — the fixed-type key is killed in both orders.

The clean 165-test library suite passes, including both new differentials. The two-type case keeps a single production scan cache throughout each four-call sequence, shares the store block across TEST and TestPlatform, and requires Miss/Miss/HitSatisfied/HitSatisfied. The expected returned bases differ: TEST `[96, 72]`, platform `[96, 48]`.

Independently replaced both production cache operations' type arguments with `LlmqType::LlmqTest`, leaving the reference manager unchanged. A mechanical review-only order filter lets each original sequence run separately, so the first panic cannot hide the other order:

- TEST then platform: exit 101 on platform's first scan.
- Platform then TEST: exit 101 on TEST's first scan.

Both failures are second-call false cache hits and incorrect answers/events. Evidence: `lq3b-sol-copy2/review-evidence/fixed-type-key.patch`, `mutant-results.json`, and `mutant-test-first.log` / `mutant-platform-first.log`. All mutation edits were restored afterward.

3. New finding — Minor: the negative fixture fails with the normal in-repository target directory.

Location: `dash-evo/tests/feature_gates.rs:151` (fixture placement), together with the headerless `none` case at line 189.

With `CARGO_TARGET_DIR` unset, `CARGO_TARGET_TMPDIR` is `<repository>/target/tmp`. The test creates an inner workspace there which excludes `second`, then asks Cargo for that headerless package's metadata. Cargo discovers the enclosing repository workspace instead of treating the package as an independent root and refuses it:

```text
.../target/tmp/feature-gates/none/second/Cargo.toml:
error: current package believes it's in a workspace when it's not:
current:   .../target/tmp/feature-gates/none/second/Cargo.toml
workspace: .../lq3b-sol-copy2/Cargo.toml
```

Consequently ordinary `cargo test -p dash-evo --all-features --locked` fails in `a_second_owner_in_its_own_workspace_fails_the_guard` before checking this case's owners. This is reproduced with Cargo 1.98.1. The implementer's external target directory avoids the enclosing workspace and therefore misses the defect.

Independent isolation using the same compiled test: clean tree fails (101); temporarily adding `target/tmp/feature-gates/none/second` to the outer repository workspace's exclusions makes the test pass (0); restoring the manifest makes it fail again (101). The outer-manifest adjustment was solely a diagnostic in the review copy, and was restored. Evidence: `lq3b-sol-copy2/review-evidence/fixture-results.json`, `fixture-*.log`, and `fixture-outer-exclusion.patch`.

Suggested fix: place the synthetic workspace outside any enclosing Cargo workspace, using a temporary directory with cleanup, or otherwise isolate Cargo's ancestor workspace discovery. Keep coverage of an excluded package without its own workspace table. Validate with the default in-repository target directory and `CARGO_TARGET_DIR` unset.

No additional correctness defect found in the three commits. The runtime implementation is unchanged; the VerifiedQuorum documentation correction accurately describes its constructors. The start-block rotation test supplies a distinguishing activation boundary and independent score expectations.

Independent validation and constraints

- All probes/builds used `/work/scratch/cp/reviews/lq3b-sol-copy2`, with its own reflinked `target/`. Every build ran under `/work/scratch/cp/heavy.lock`; compiler wrappers/caches were disabled. The dedicated mode-0700 TMPDIR was removed afterward.
- Waited for resource recovery. Standalone uptime/df checks before build invocations showed load5/free: 83.97/234G (initial run queued on the lock), 57.01/248G (mutant), 52.77/247G (restored suite). A check while the first build was running showed 58.79/248G.
- `cargo test -p dash-evo --all-features --locked --no-fail-fast`, with reviewed source restored: **212 passed, 1 failed, 1 ignored**, exit 101. The sole failure is the new fixture defect above. Both new library tests pass without the mutant. Logs: `review-evidence/baseline.log` and `baseline-restored.log` inside the copy.
- `cargo fmt --all -- --check`: pass.
- This narrow round did not rerun node-l1's unchanged oracle suite; r1 independently passed its 30 tests. All specifically requested r2 probes were rerun after resource recovery.
- The frozen worktree remains clean at `3733246f9e67a04d197dc576ad255290e9c97b6d`. All reviewed files in the copy match it after restoring probes. Nothing was committed or pushed; restricted paths and memory were not read or written.

Verdict: CHANGES-REQUIRED — 0 blocker, 0 major, 1 minor, 0 nit.
