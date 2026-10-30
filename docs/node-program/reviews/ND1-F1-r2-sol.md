# ND1 F1, Sol round 2 — narrow fix review

Reviewed only `4352535d9..6dd691262` in a private detached worktree at `6dd691262`.

## Findings

No new P1, P2, P3, or Nit findings in this commit.

## Round-1 findings

- **P2-1 — CLOSED** (`dashd-node/src/lib.rs:11-13`, `dashd-node/Cargo.toml:17-20`). The new `test-support` guard uses the existing `test-hooks` predicate. `cargo build -p rdashd --release --features dashd-node/test-support` exits 101 at the new `compile_error!`; the default release build succeeds. `cargo tree --workspace -e features -i dashd-node` shows only the default feature in the ordinary workspace graph.
- **P2-2 — CLOSED** (`dashd-node/src/sut.rs:176`, `:469-481`). The added `-prune=550` SUT test checks both production soft-sets and a loaded node. All eight SUT tests pass on the reviewed commit. Removing only the constructor's `init_parameter_interaction` call makes the new test fail with `Prune mode is incompatible with -txindex.` Restoring the call makes it pass again.

## Optimized-build audit

No existing build needs an exemption and none is broken by the guard. The gate's feature step (`/work/scratch/gate-fast/nightly.sh:53` and `/work/scratch/gate-feature-tests.py`) runs `cargo test -p dashd-node --features test-support` in debug. CI's all-feature tests/coverage (`.github/scripts/ci_config.py:194-196`), workspace check, and docs (`.github/workflows/rust.yml:59,73`) also use debug profiles. Perfgate's release binaries and benches (`contrib/node/perfgate/perfgate/build.py:572,602`) pass no test feature; the `rdashd` product row (`contrib/node/layers.toml:223-225`) declares none. The release loom, KGW, and ST3a scripts build `dash-chain`/`dashd-chainstate`, `dash-consensus`, and `dashd-store`, respectively; the separate `node-fuzz` and `cs2-diff` roots do not depend on `dashd-node`. The pre-push release all-feature Clippy path is covered by the guard's `not(clippy)` exception and passes; the all-feature `cargo doc` path passes.

## Validation

- `cargo build -p rdashd --release --features dashd-node/test-support`: expected failure at `dashd-node/src/lib.rs:13` (exit 101).
- `cargo build -p rdashd --release`: pass.
- `cargo test -p dashd-node --features test-support -- sut::`: 8 passed, 0 failed.
- Interaction-call deletion mutant: new exact test failed as expected (exit 101); after restore, it passed.
- `cargo doc -p dashd-node --all-features --no-deps`: pass.
- `cargo clippy -p dashd-node -p rdashd --release --all-features --all-targets -- -D warnings`: pass.
- `cargo fmt --all -- --check` and `git diff --check 4352535d9..6dd691262`: pass. The private worktree was clean after restoring the mutant.

All Cargo commands used `TMPDIR=/work/scratch/nd1-f1-r2-sol/tmp` and `CARGO_TARGET_DIR=/work/scratch/nd1-f1-r2-sol/target`.

VERDICT: ACCEPT
