# BS15 r2 — Sol

Reviewed `node/bs15-x11-blockhash` at `b8bc91607898a0e5e3e38e5534057054e5a02825`, the two follow-up commits `39ebaeed` and `b8bc9160` over r1's `4cc009de`. Review date: 2026-10-09. Followed `REVIEW-COMMON.md` and read `BS15-r1-sol.md`. This is the requested narrow confirmation of the two r1 findings, including fresh packaging, test and dependency-policy checks.

All validation used a detached worktree at `/work/scratch/bs15-review-r2/wt`, `TMPDIR=/dev/shm/bs15-review-r2-tmp` (verified mode 0700), and `CARGO_BUILD_JOBS=8`. The branch under review was not modified.

## Findings and closure

**No open findings. Both r1 findings are closed.**

1. **r1 major — published differential-test compilation: CLOSED.** `dash-x11/Cargo.toml:25` now uses the registry-backed `bitcoin_hashes` workspace dependency. I independently ran `cargo package -p dash-x11` with verification enabled, extracted `target/package/dash-x11-0.45.0.crate` into a separate directory outside the workspace, then ran:

   ```text
   cargo test --manifest-path /work/scratch/bs15-review-r2/unpacked/dash-x11-0.45.0/Cargo.toml --no-run
   ```

   Both commands exited **0**. The normalized manifest retains `bitcoin_hashes` version `0.14.101`, with `default-features = false` and no path reference. The extracted source compiled its library test and all four integration-test executables: `core`, `props`, `tooling`, and `vectors`. In particular, the previously broken `tests/core.rs` now compiles. The README's Publishing section accurately describes the dependency and this verification procedure.

   **Digest parity confirmed.** `dash-x11/tests/core.rs:43` streams into `sha256::HashEngine`, finalizes with `sha256::Hash::from_engine`, and hex-encodes the raw byte array, preserving the previous digest direction. `core_fixture` passed under every implementation selected by the test harness on this CPU. All three X11 stream digests and all eleven stage-stream digests match the unchanged fixture, at seed `1479618865` and 2,000 cases per stream. `tests/data/core-fixture.json` has the same Git blob at r1 and r2: `5779bef0a6c2712aa6517f6ea5c04a56ebf660eb`. The header stream still hashes to `1037305c2a5c30799c52ff45f30c2cfda3593bb300a3089f08c9a28b5c0a92b1`.

2. **r1 minor — SPV startup detection: CLOSED.** `dash-spv/src/client/lifecycle.rs:50` calls `dash_x11::init()` directly, outside tracing macros and before configuration validation, checkpoint resolution or genesis initialization (line 64). Disabling INFO logging therefore cannot suppress initialization. The constructor logs the actual wrapper backend via `hash_x11::backend()` and emits a WARN when `Selection::fell_back()` is true, including the selection/failure details. The old `start_sync` hook is gone. `dash-spv/Cargo.toml:15` supplies the direct dependency needed by the explicit init call. The operator strict-mode/full-node work remains assigned to BL-35 as recorded in r1.

## Independent validation

Every requested command exited **0**:

- `cargo package -p dash-x11`: packaged and verified successfully.
- Unpacked-source `cargo test --no-run`: all five test executables compiled successfully.
- `cargo test -p dashcore_hashes -p dash-x11`: **50 passed, 0 failed** including doctests. Breakdown: dash-x11 lib 9, core 3, properties 7, tooling 1, vectors 3, doctests 1; dashcore_hashes lib 19 and doctests 7. `core_fixture` ran and passed. The conditional live-Core tests had no external harness configured; the hashes-level chain replay is feature-gated out of this exact default-feature command.
- `cargo test -p dash-spv --lib`: **625 passed, 0 failed, 2 ignored**. The ignored tests are `test_fork_rejection` and `test_dns_discovery_mainnet`.
- `contrib/node/check-r2.sh`: **R2 ok**, including the all-features dependency graph with the new SPV edge.
- `cargo deny check`: **advisories ok, bans ok, licenses ok, sources ok**. Nonfatal configured warnings concern duplicate versions, unused wrappers, a license not encountered, and unlisted Git sources; there were no errors.
- `contrib/node/check-pure.sh`: **pure crates: 3 crate(s) ok** (`dash-sha256`, `dash-consensus`, `dash-x11`). Dependency bans and both all-features/no-default-features clippy passes succeeded. The base-SDK and blst audit stamps matched. Unused-wrapper warnings for absent threadpool/blst dependencies were nonfatal.
- `git diff 4cc009de..HEAD --check`: passed; the detached checkout remained clean.

This round does not replace r1's full-chain replay/performance evidence or the manager's integration merge gates. The detached worktree, unpacked package/builds, scratch logs and private TMPDIR were removed after validation. This report is left uncommitted in the integration worktree.

**mergeable: yes**

**Verdict: APPROVE.**
