# MN0-2a round-2 review (Sol)

Target: `node/mn0-2a` at `a886f73ee` (previous head `18f787a73`). Review probes used the required reflink copy at `/work/scratch/mn0-2a-rv-sol2/`; the target worktree was not modified.

## Verdict: ACCEPT-WITH-NITS

The r1 fixes are complete and the implementation remains byte-for-byte equivalent to the committed Dash Core v24.0.0-rc.4 capture across all seven chains (1,043 blocks as reported by the fixture). The requested tests and clippy pass. One documentation nit remains.

## Round-1 findings

- **m1 — closed.** `contrib/node/mn0-captures/README.md:9-24,46-48` and `contrib/node/mn0-captures/capture_mn0_evo.py:49-88` now define the four additional chains (`brr-late`, `mnrr-early`, `legacy-bls`, `v24-before-v20`) and state the rc.4 SHA-256 provenance. `dashd-mining/tests/evo_dashd.rs:47-67` asserts seven chains and the reported `seen` counts. The M17 spot-check (mutating `platform_reward(..., height, ...)` to `height + 1`) is killed at `brr-late` h440.

- **m2 — closed.** `dashd-mining/src/evo.rs:57-64` now documents both `CalcCbTxBestChainlock` arms and the V19 threshold/any-height behavior. I checked it against rc.4 `src/node/miner.cpp:117-171` (the helper), `src/evo/cbtx.cpp:181-210` (parent-CbTx extraction), and `src/evo/specialtxman.cpp:161-188` (carried-CL and best-CL validation). It correctly says a parent non-null ChainLock is carried forward, or a best ChainLock at/above V19 is used; below V19 is treated as false.

- **m3 — closed.** `dashd-mining/src/evo.rs:168-175` fences non-empty mined `qc_hashes` with `NotReady("mined commitments", "MN0-2b")`; `:187-196` fences non-null pool commitments with `NotReady("mineable commitments", "MN0-2b")`. `dashd-mining/tests/evo_dashd.rs:205-271` exercises both routes for every fixture chain. M29 (disabling the mined fence) and M30 (disabling the pool fence) both fail those assertions in the spot-check.

## Finding

### N1 (nit): public `block` error documentation omits the new commitment fences

`dashd-mining/src/evo.rs:133-137` lists the `NotReady` cases as the non-empty list, carried ChainLock, and superblock trigger, but the implementation now also returns typed `NotReady` for mined commitments at `:168-175` and mineable commitments at `:187-196`. Add those two cases to the `# Errors` documentation so callers can discover the MN0-2b-owned readiness contract. This does not affect behavior or the differential result.

## Verification

- `cargo test -p dashd-mining --all-targets`: pass (12 unit tests and the seven-chain `evo_dashd` differential test).
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- Mutant spot-checks in the reflink copy: M17 killed at `brr-late` h440; M29 killed by the mined-commitment fence assertion; M30 killed by the mineable-commitment fence assertion.
- `git diff --check` is clean for the target range.

Finding count: 1.
