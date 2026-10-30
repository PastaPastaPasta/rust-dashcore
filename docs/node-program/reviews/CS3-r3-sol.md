# CS3 confirmation review, round 3 — Sol

Reviewed `node/cs3-checks` at `abdfc2f88867e07d32e8231e27283a4171da4063`, comparing the three commits `c03edc82`, `c4f1ffa3`, and `abdfc2f8` with r2's `7c333502`. Read r2 first, then r1 and `REVIEW-COMMON.md`. This review confirms CS3-S3, the re-pin selftest adjustment, and regressions in the affected paths. Settled check implementations and CS3-S1/S2 were not re-reviewed.

**No findings. CS3-S3 is closed, and the selftest adjustment preserves its purpose.**

## CS3-S3 — closed

At `dash-consensus/tests/support/checks.rs:255`, `Position::window_tail` runs the original generator for the full selected length. Only after generating and linking every header does it drain the same prefix from the headers and their cached hashes, then advance `first_height` by the dropped count (`:281`). It preserves the original RNG sequence, times, bits, hashes, and tip. Each generated header is hashed once on the Rust side.

The sole header-input parser passes the selected length unchanged to this function (`checks.rs:558`). `Position::window` delegates with an unlimited keep count (`:248`), preserving the other contextual window paths. There is no remaining CS3 path that passes the history cutoff as the generation length. KGW windows remain unshortened.

Core receives the retained bytes directly: `node-l1/src/checks.rs:99` copies `window.headers` into the oracle context with the adjusted first height, unchanged tip-derived flags, checkpoint setting, and clock. The shim constructs its indexes from those headers without regenerating them. Its initial-header handling permits the retained first header's original predecessor hash. Core's difficulty and MTP guards, and the header check at v24 `validation.cpp:4469`, remain consistent with r2's settled lookback assessment: 577 covers the non-KGW paths, while KGW retains its whole window.

I independently verified the following:

- Both committed window suites passed. A stricter scratch oracle probe counted **117 inputs, 75 cut windows, and 117 matching Core verdicts**, with no exclusions or unexpected oracle errors.
- A scratch test loaded the pre-clamp parser from `90bf0058`, independently of the new `whole()` helper. Across **2,808 variants** covering all eight window selectors and three offsets per network anchor, the whole window matched the original bytes, the kept window matched its suffix, and the candidate header, clock, checkpoint setting, and Rust verdict were preserved. **225 variants were cut.**
- The public r2 reproduction again has tip time `1602284198`, MTP `1602283307`, and candidate time `1602283898`. Rust and pinned Core accept it; Core also accepts its whole-window form.
- The new row, "a 4040-header window keeps its last 577 headers", passed the offline and live recorded-verdict tests. Restoring r2's complete `checks.rs` in scratch made `checks_recorded` and `recorded_rows_match_the_oracle` fail, while `rows_agree_with_core` passed. Restoring r3 made all three pass. The committed acceptance record catches the demonstrated differential-test blind spot.

## Re-pin selftest — confirmed

`contrib/node/selftest-checks.sh:436` now invokes `bump_to_parent` before checking the stamp. That helper rewrites the real root and nested pins, relocks the root and every nested workspace, and runs the full pin checker (`:206`). Consequently, an unrelated stale nested lockfile cannot substitute for the expected audit-stamp failure.

Both changed cases passed in the full selftest: the real parent re-pin failed with the old stamp and succeeded with the updated stamp. These cases exercise pin and stamp tooling without requiring current source code to compile against an older fork revision.

The stamp-validation block at `contrib/node/check-pure.sh:28` is byte-identical to r2's block, moved before the `--stamp` exit. Both entry paths execute that single block; there is no second predicate to drift. The full checker still tests the linked blst release and runs the dependency and lint bans, including both feature configurations (`:137`), and still rejects a stale stamp (`:144`). A scratch PATH probe made every Cargo invocation fail: `--stamp` nevertheless accepted a matching stamp and rejected stale and missing stamps, confirming its no-build behavior. The full selftest also passed its linked-blst mismatch and missing-stamp cases.

## Verification and limits

Tests ran in a detached worktree under `/work/scratch/cs3-review-r3-sol/`, with `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=12`. `TMPDIR=/dev/shm/cs3-review-r3-sol-tmp` was mode 0700. Cargo used the separate `target-r3` directory; the selftest used its own nested worktree and target. `DASH_EMBARGO_DIR` was unset throughout.

- Affected suites: `checks_header_windows`, `checks_recorded`, `l1_header_windows`, `l1_check_rows`, and `l1_checks`, using `cargo test -p dash-consensus -p node-l1 --features node-l1/oracle --locked --target-dir target-r3`: **10 passed, 0 failed**.
- The five censuses reproduced r2's check-verdict / decode-rejection / exclusion counts: transaction **2001/999/0**, block **2542/458/0**, contextual transaction **2641/354/5**, contextual block **1478/283/239**, and contextual header **738/0/62**.
- Three independent scratch tests: **3 passed, 0 failed**. The oracle archives were pinned, including v24 at `a9a483e51efdadeee37cf40ff8697da62b3cd012`, based on Core `v24.0.0-rc.3`.
- `contrib/node/selftest-checks.sh`: **101 cases passed, 0 unexpected failures**, exit 0.
- Workspace formatting, targeted Clippy on all five affected suites with `-D warnings`, shell syntax checks, and diff whitespace checks passed. The restored scratch source matched the reviewed tree.

This narrow confirmation did not repeat the full workspace, fuzz-smoke, complete oracle-diff, or real-private receipt gates. No private input was inspected; PLAN §7 handling was observed. Temporary tests, the detached worktree, build output, and TMPDIR were removed. Public verification logs remain under `/work/scratch/cs3-review-r3-sol/`. No review-owned dashd was started. The branch under review was untouched; this report is uncommitted. No commit, push, or GitHub comment was made.

**Sol gate satisfied: yes.**

**Verdict: APPROVE.**
