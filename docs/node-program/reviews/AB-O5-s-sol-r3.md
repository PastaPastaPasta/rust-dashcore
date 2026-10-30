# AB-O5-s confirmation review — r3

Verdict: **REQUEST CHANGES**.

**Sol gate satisfied: no.**

Reviewed only `node/ab-o5-s` at `a415735db768874afecad3413e9b490139058cdd`, against the r2 head `1d259191b9b07e4f36c6d465909fe53aa769f63b`. The two new commits are `64d9762f` (inventory by target/package/features) and `a415735d` (separate same-package and other-package file selftests). Scope is the r2 inventory finding and regressions in its fix, under DEC-121's honest-developer drift model.

## F1-R2 disposition

**F1-R2 is closed.** Distinct Cargo targets sharing one source file remain distinct compiled occurrences. Each entry's presence, file/name and effective ignore status are checked in its own package/feature build, so another entry's enabled feature cannot supply the missing default test.

In independent dependency-free Cargo fixtures, the r2 checker exits 0 for both twin targets and the cross-entry feature leak; the r3 checker exits 1 for both. Separate executable listings establish 42 named tests in each twin binary, and quirk 13 absent in the default configuration but present with `not_default`.

## Finding

### F1-R3 — major: a cross-package test move can leave a duplicate that the checker silently accepts

`contrib/node/check-quirks.py:251` and `contrib/node/check-quirks.py:271`.

Selecting only `configs[config_of[qid]]` correctly prevents another configuration from proving presence. However, occurrences in other packages are subsequently examined only when their id is **unregistered**. A second compiled test for an already registered id in another checked package is silently discarded for validation and the ignored-count summary. The r2 checker rejected this ordinary duplicate.

Concrete honest-developer scenario: a developer copies a placeholder into its owning crate, updates the register's `file`, and forgets to remove the old placeholder. Both copies remain real, compiled tests. This is the move described by `conformance/tests/quirks.rs:5`; it requires no deliberate substitution or gaming.

A two-package Cargo fixture proves the regression: without the retained copy both checkers pass; with the same pending quirk 13 present in both packages, the r2 checker exits 1 with `found 2`, while r3 exits 0 with the unchanged 42/29 summary.

I reproduced the same failure in the detached copy of this exact arm:

1. Copy the existing `quirk_13_payee_tie_break` stub, unchanged, from `conformance/tests/quirks.rs` into `dash-consensus/tests/quirk_13_migration_copy.rs`.
2. Change only quirk 13's register `file` to that new path; retain the original conformance test.
3. Run the byte-identical r3 checker and the r2 checker against the mutated worktree.

The **r3 checker exits 0**, reporting **42 registered / 29 ignored**. The **r2 checker exits 1**, identifying the two source files and `found 2`. Independently collected Cargo inventories show quirk 13 in both `test quirk_13_migration_copy` (dash-consensus) and `test quirks` (dash-conformance), with the correct owner reason in both. The actual totals are **43 compiled quirk occurrences / 30 ignored**. This violates the one-test-per-quirk requirement and underreports the ignored count. The mutation was restored after the experiment.

**Suggested fix:** retain the entry's own feature-build validation, and also reject a registered id compiled in a different package among the collected inventories. Alternatively, enforce uniqueness of the owning target across inventories while deduplicating the same target/test intentionally rebuilt for multiple configurations. Add a selftest for a cross-package move with the original test accidentally retained. This needs no additional adversarial machinery or new build configurations.

## Validation and limits

- Direct `DASH_CORE_REPO=/work/home/agent/workspace/dash python3 -I contrib/node/check-quirks.py`: **PASS**, exit 0; **42 registered (38 R4/A1, 4 appended), 12 implemented, 1 private, 0 dropped, 29 ignored**. The optional v24 anchor existence checks also pass.
- All **31/31 O5 quirk selftests: PASS**, exit 0. I extracted their case bodies unchanged from `selftest-checks.sh`, using its exact `expect` and reset logic in the detached worktree. This includes both new r2 repro cases, both file-location cases, feature-enabled acceptance, cfg/cfg_attr exclusions, target exclusions, owner reasons, and dropped/unregistered tests. Unrelated sections of the node selftest script were not rerun.
- **10/10 independent checker assertions: PASS**: both old false greens now fail; repeated tests across intentional feature builds, an entry enabling its own feature, `tests/quirks/main.rs` with a nested module, and a required-features target with the feature declared by its entries all pass. Dropped and macro-generated unregistered tests seen only in another feature build are rejected.
- Cross-package duplicate: reproduced in both the two-package fixture and the actual arm; the old checker rejects it and the new checker accepts it. The fixture's clean two-package control passes with both versions.
- Python syntax, `bash -n contrib/node/selftest-checks.sh`, and `git diff --check 1d259191..a415735d`: **PASS**.

No false failure was found on the clean current workspace or the legitimate layouts tested. The declared absence of `--no-default-features` coverage is accepted for this review: no current entry requests such a configuration. The checker uses Cargo defaults plus explicitly requested features.

The supplied full-gate result (3,131 passed / one BL-20 failure, rerun 54/0) is reported evidence; I did not rerun the unrelated full workspace gate in this narrow round. No Rust implementation or behavioural test changed after r2, and the earlier register/Core-behaviour findings were outside this confirmation's scope.

All scratch, fixtures, build products, and the detached worktree under `/dev/shm/ab-o5-s-r3` were removed. Tests used a private mode-700 TMPDIR there and `CARGO_BUILD_JOBS=12`. The only change to the reviewed worktree is this uncommitted report. No commit, push, GitHub comment, or private-vector/report inspection was made.

Verdict: **REQUEST CHANGES**.

**Sol gate satisfied: no.**

blockers=0 majors=1 minors=0 nits=0
