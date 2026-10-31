APPROVE — 0 blocker, 0 major, 0 minor, 0 nit

Narrow correctness r4 re-review of signed commit `f8b72b588cea25f441c3ec47f3b26507c4efa9d6` (`G`), the single commit in `e5761f8c7..f8b72b588`, on frozen `node/lq3b`. Read REVIEW-COMMON, the r3 review and reproduction scripts, the implementer's RESULTS and runs/h scripts, and the complete changed file. Reviewed and tested only the requested guard-harness delta; the rest of LQ3b's accepted review stands.

Independent validation used `/work/scratch/cp/reviews/lq3b-sol-copy2`. Compared all 22,045 tracked files against the frozen head: only `dash-evo/tests/feature_gates.rs` differed from r3, and copied that file from the frozen worktree before compiling. Evidence below is relative to this copy's `review-evidence/r4/`.

1. R3 minor — CLOSED.

Repeated the r3 environments with `CARGO_TARGET_DIR` explicitly unset and the default in-tree target. Each environment ran the entire command, not just the previously failing test:

```text
cargo test -p dash-evo --all-features --locked --no-fail-fast
```

- External, dedicated 0700 TMPDIR under `/dev/shm`: exit 0; 214 passed, 0 failed, 1 ignored.
- TMPDIR inside the review copy at `target/tmp/r4-sol-in-project`: exit 0; 214 passed, 0 failed, 1 ignored.
- TMPDIR outside the copy, symlinked into that in-project directory: exit 0; 214 passed, 0 failed, 1 ignored.
- TMPDIR below a valid ordinary `[package]`-only parent with `src/lib.rs`: exit 0; 214 passed, 0 failed, 1 ignored.

All four feature-gate tests pass in each run. `isolated()` checks canonical ancestry before selecting the base, so a symlink cannot bypass the repository workspace check. An ordinary package parent is accepted. The in-repository cases use the fallback instead of failing the old assertion. Probe creation uses the canonical selected base, matching the paths later handed to Cargo. The excluded `second` package still has no `[workspace]` table in the headerless case.

Evidence: `run-review.py`, `run-results.json`, the four environment logs and `.exit` files, and `guard-binary.txt`. The changed test was recompiled; the executable is `target/debug/deps/feature_gates-58a2a2895f717ebf` in this review copy.

2. Six header forms and default-target validation — CLOSED.

Independently repeated the real repository guard with a temporary second-owner package, separately enabling only `reload` and only `mined`, under each of:

- Plain `[workspace]`.
- Commented `[workspace] # independent tool`.
- Final `[workspace]` with no trailing newline.
- Top-level `workspace = {}`.
- Quoted `["workspace"]`.
- Dotted `workspace.resolver = "2"`.

All 12 metadata calls succeed (exit 0). All 12 real guard invocations reject the added owner (exit 101), naming `review-second-owner` and the corresponding gated feature. The clean real guard passes in every full-suite run. Removed the synthetic package afterward. The default-target run therefore still passes, and detection has not been weakened by the fixture-placement fix.

Evidence: `guard-probes.py`, `guard-probe-results.json`, and each `guard-{feature}-{form}.log` and corresponding metadata log.

3. New correctness defects and the stated limit — no finding; limit acceptable.

The only change to the actual ownership reader is extracting its existing read-and-parse operation into `table()`; it still panics on unreadable or invalid TOML and requires successful Cargo metadata for every discovered workspace and every otherwise-unread package. The new base selection affects synthetic fixtures, not repository discovery or feature-owner classification. There is no new skip or successful fallback from a metadata error.

Independently reproduced the stated limit with a TOML-valid parent package lacking targets. Its TMPDIR passes the syntactic isolation criterion, but Cargo rejects the headerless fixture's metadata with `failed searching for potential workspace` and `no targets specified`. The probe test exits 101 at `metadata()`'s success assertion (`dash-evo/tests/feature_gates.rs:47`), rather than passing with an owner omitted. The real repository ownership test still passes with that same ambient TMPDIR, showing that it does not use fixture placement to decide which repository packages to inspect. Thus the limit can cause an explicit harness failure under a Cargo-invalid ancestor, but does not conceal a second feature owner. Expanding this test-only isolation helper into a complete Cargo manifest validator is not necessary for this fix.

Evidence: `invalid-parent-probe.log` (expected exit 101), `invalid-parent-real-guard.log` (exit 0), and `run-results.json`.

Validation and constraints

- All four Cargo test runs executed under `flock /work/scratch/cp/heavy.lock`, with compiler wrappers disabled and `CARGO_BUILD_JOBS=12`.
- Ran `uptime` and `df -BG /work` in a separate command before each Cargo test invocation, after acquiring the lock. Five-minute load/free space at those checks: 68.22/211G, 67.60/210G, 67.01/209G, 67.09/209G. Each met load5 <100 and free space >=200G; the runner also checked these thresholds before invoking Cargo. Evidence: the four `*-preflight.log` files.
- `cargo fmt --all -- --check`: exit 0. `git diff --check e5761f8c7 f8b72b588`: exit 0.
- After validation, compared all 22,045 tracked files again: the review copy still matches the frozen head exactly (`final-source-check.json`).
- Scratch fixtures used a dedicated 0700 `/dev/shm` directory and the explicitly requested in-project TMPDIR. The tests themselves exercised their `/tmp` fallback; no build target or reviewer-created scratch tree was placed there. Temporary probe trees are removed on drop. Removed only the fixtures created by this review; see `cleanup.json`.
- Frozen worktree remains clean at `f8b72b588cea25f441c3ec47f3b26507c4efa9d6`. No commits, pushes, GitHub comments, memory writes, or reads of restricted directory contents.

Verdict: APPROVE — 0 blocker, 0 major, 0 minor, 0 nit.
