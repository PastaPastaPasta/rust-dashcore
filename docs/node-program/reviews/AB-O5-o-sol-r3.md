# AB-O5, arm o — Sol confirmation review, round 3

Verdict: **CHANGES-REQUIRED**

Reviewed only `node/ab-o5-o` at `705e95ddc26d9c08f4293eb02a4713d82cc82633`, using the shared `AB-REVIEW.md`, `REVIEW-COMMON.md`, `AB-O5.md`, PLAN §4 O5 and `AB-O5-o-sol-r2.md`. The narrow confirmation range is `83188bfc..705e95dd`, one commit (`705e95dd`, `fix(contrib): match quirk tests to their own compiled target`). It changes the checker, four quirk selftests and the PROCESS description; no consensus implementation, vectors or register entries change.

All three specific r2 defects are fixed, and their exact reproductions are now rejected. The package-key identity bug is closed by retaining `target.src_path`; both minors are also closed. However, the broader claimed guarantee that the registered function compiled under its actual module path remains incomplete: a new same-target variant still passes. This is the sole r3 finding.

## Finding

1. **major — `contrib/node/check-quirks.py:173–186,365–378`: a same-target macro test can substitute for a registered function inside a disabled inline module.** `module_files()` associates the target's entire root file with the empty module prefix. The source parser records function names and their immediate attributes without the enclosing inline module or its conditional compilation. Matching consequently accepts a root-level macro test with the same leaf name even though the registered source function belongs to `hidden::` and is compiled out.

   Reproduction on the complete reviewed tree, without changing the register: replace `dash-consensus/tests/quirks.rs` with the following wrapper around its original contents:

   ```rust
   #[cfg(any())]
   mod hidden {
       // Entire original quirks.rs contents, including its three #[test] functions.
   }
   macro_rules! substitute {
       ($name:ident) => {
           #[test]
           fn $name() { assert_eq!(2 + 2, 4); }
       };
   }
   substitute!(quirk_04_checkdatasig);
   substitute!(quirk_05_num2bin_exceptions_and_unexecuted_failures);
   substitute!(quirk_07_ecdsa);
   ```

   `python3 -I contrib/node/check-quirks.py` incorrectly exits **0** with `42 quirks, 42 tests: 12 tested, 30 ignored (scaffold), 0 dropped (DEC-82)`. `cargo test --locked -p dash-consensus --test quirks -- --list` lists precisely those three root-level names, with no `hidden::` tests; executing that target reports **3 passed / 0 failed**. The original Core-vector assertions are absent from the binary. An independent minimal Cargo workspace reproduces the same false pass.

   **Suggested fix:** bind each parsed function to its full enclosing module path and conditional ancestors, and use that identity in the target match. Reject a registered function hidden behind a disabled/unsupported module configuration even if another compiled test shares its leaf name. Add the same-file disabled-inline-module plus root macro replacement as a negative selftest. Merely matching `target.src_path` and a file-level prefix is insufficient.

## R2 closure and validation

- **R2 major (cross-target substitution): closed.** Repeated the exact file-level `#![cfg(any())]` attack with all three same-named macro tests in a second `dash-consensus` target. The checker exits **1**, explicitly rejecting the source target for quirks 4, 5 and 7. The inventory now retains the compiler artifact's `target.src_path` rather than package identity.
- **R2 minor (other workspace packages): closed.** A macro-generated `quirk_99_unregistered` in `dash-sha256/tests/review_inventory.rs` is inventoried and rejected, exit **1**.
- **R2 minor (dropped entry): closed.** After dropping 33, removing its register test field and original scaffold, a macro-generated `quirk_33_hash_serialized_2_varint` is explicitly rejected as a compiled test for a dropped ID, exit **1**.
- **Clean compiled checker: PASS**, before mutation and after restoration, exit **0**: 42 quirks / 42 referenced tests / 12 tested / 30 ignored / 0 dropped. This exercises the new workspace-wide `cargo test --locked --workspace --no-run` inventory and Linux proc-macro loader handling.
- **Quirk selftests: PASS, 34/34 expected outcomes**, exit **0**. Ran the O5 section of `contrib/node/selftest-checks.sh` with its shared original setup/reset helpers in its own throwaway detached worktree. All four added cases pass, including the positive `#[path]` move. The unrelated sections of the full selftest script were not rerun.
- **Supplemental r2 probes: PASS, 12/12 expected outcomes** in an isolated minimal Cargo workspace: clean fixture; block/nested comments; file/module/feature suppression; cfg-attribute ignore; positive/disabled/duplicate path inclusion; literal/comment-marker handling; and a macro-generated unregistered test in an inventoried package.
- **New evasion: confirmed false pass** on the full tree and independently in a minimal workspace; its full-tree replacement target lists three tests and executes **3 passed / 0 failed**.
- **Static checks: PASS** for `cargo fmt --all --check`, Python syntax, shell syntax, and `git diff --check 83188bfc..705e95dd`. The unchanged Core anchor bounds also pass using `--source-only --core ~/workspace/dash`. There are no touched Rust crates requiring a new clippy run.


The review ran in detached scratch worktrees with `TMPDIR=/dev/shm/ab-review-o5-o-r3-tmp` (mode 700) and `CARGO_BUILD_JOBS=6`, using separate Cargo target directories. Consensus behavior and the fifteen Core spot-checks recorded in r2 were outside this narrow confirmation; only the unchanged anchor bounds were rechecked. macOS loader behavior was inspected in code but not executed on this Linux host. The implementer's claimed full-gate exit 0 / 3,129 passed was not independently rerun in r3.

The report is uncommitted. All review mutations were restored, and the detached review worktree, selftest worktree, synthetic fixture, logs and temporary directories were removed. The implementer's worktree was not modified; no commit, push or GitHub comment was made.

Verdict: **CHANGES-REQUIRED**

blockers=0 majors=1 minors=0 nits=0
