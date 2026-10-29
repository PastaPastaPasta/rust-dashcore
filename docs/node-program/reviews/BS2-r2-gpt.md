# BS2 focused independent review — GPT round 2

Reviewed `node/bs2-conformance-harness` at **`829e8ee6a4b7856008bba0f37ffa0bcc64f1bf59`**, in `/work/home/agent/workspace/rdc-wt/bs2-conformance-harness`, against `node/integration` (`9e14991d`). Followed `briefs/REVIEW-COMMON.md` and `PROCESS.md`. This round checks the round-one fixes and BS3 fold, rather than repeating the full original review.

No source or tracked branch files were changed; no commits, pushes or GitHub comments were made. Scratch probes and evidence are in `/work/scratch/bs2-r2-gpt/`. The deployed `dash-conformance-refresh.timer`, its service, deployed binary and output directories were not touched. The workspace tests use their own temporary datadirs and allocated ports.

## Finding

### 1. BLOCKER — a reorg between checkpoint-boundary reads permits a mixed-chain resumed gate to pass

**Location:** `conformance/src/run.rs:336`, `:354`, `:375`, `:394`; `conformance/src/main.rs:352`.

The boundary hash batch is treated as a chain snapshot, but its individual `getblockhash` answers need not come from the same chain. Checkpoints are invalidated against those answers only once, before work starts. Each new chunk starts with `prev = None`, so its first block is not linked to the preceding chunk's retained boundary. There is no final snapshot/canonical-chain check before the gate.

**Concrete reproduction:** Using the existing mock-RPC test infrastructure in a scratch-only probe:

1. Check heights 0–9 of chain A, with chunk size 10 and `--fail-on-mismatch`: exit **0**.
2. Resume through height 19. During the initial boundary query, return A's hash at height 9, then reorg from height 1 to chain B before answering height 19. All subsequent requests consistently serve B. B has an injected `getblock 1.time` mismatch at height 3, following the same fault model as the committed gate tests.
3. The old `rpc:0` checkpoint matches the first boundary answer and is reused. The driver checks only B's heights 10–19. Their internal linkage is correct, and their final hash matches B's preflight height-19 answer.
4. The resumed invocation exits **0**, with `gate: 2 units in scope, 0 not covered, 0 failing mismatches`. It made only 20 `getblock` calls, covering the new ten-block chunk. Its reported 20-block coverage combines two incompatible chains.
5. A fresh invocation over the same B chain exits **1**, recording `header.field:time:both` at height 3.

This does not require an oracle to return fabricated hashes or inconsistent block contents: a real reorg between individually executed batch calls suffices. The altered time field makes the missed revalidation observable. Even without that injected disagreement, the successful resumed gate does not describe any one chain.

**Evidence:** `rpc_probe.rs`, test `resumed_mixed_chain_false_pass`; `reorg-probe.log`, `reorg-resume.log`, `reorg-fresh.log`, and the `reorg-out/` and `reorg-fresh/` reports. Rebuild the probe with `bash /work/scratch/bs2-r2-gpt/compile-rpc-probe.sh`, then run it with `TMPDIR=/work/scratch/bs2-r2-gpt /work/scratch/bs2-r2-gpt/rpc_probe resumed_mixed_chain_false_pass --nocapture`.

**Suggested fix:** Establish a coherent chain snapshot for all scope boundaries, retained checkpoints and newly checked chunks. At minimum, bracket boundary collection and the final gate with a pinned block hash at a fixed height, revalidate the scope boundaries before accepting success, and reject/retry when the pin or any required boundary changes. Verify linkage across chunk boundaries as well as within chunks. Ordinary tip growth should remain distinguishable from a reorg. Add the between-boundary reorg regression above, including a reused early checkpoint; the existing within-unit reorg test does not exercise this path.

## Round-one closure assessment

| Round-one finding | Assessment | Evidence |
|---|---|---|
| 1. Resuming a recorded failing gate returns success | **Closed for retained findings on a stable chain.** The gate counts results across runs in its requested scope. The separate reorg false pass above still blocks using it as a sound gate. | Binary RPC regression and blk integration test retain failure on resume; `Reporter::gate` uses recorded results, not only new counters. |
| 2. Checkpoint code, policy and canonical-chain keying | **Partially closed; reorg snapshot coherence remains open.** Code, resolved dependency revisions, executable/features and policy are keyed; stable pre-run reorgs invalidate affected chunks. | Independent source-identity and changed-code driver probes; committed changed-policy, changed-oracle-version, ordinary-reorg and same-size blk replacement tests. See blocker above for a reorg during boundary collection. |
| 3. Count-based summary erasure | **Closed.** Replacement uses actual coverage rather than block count; disjoint results and newly failing partial rechecks survive. The CLI now checks whole selected chunks/files. | Independent Reporter probe retained 5 findings after a clean equal-sized disjoint run, then 6 after a failing narrower recheck, with a one-line output cap and `unaccounted = 0`. A clean covering recheck cleared them. Committed report tests also pass. |
| 4. Silent derivable-field/subtree exclusion | **Closed for the reported omissions.** Exact-path classification replaces blanket address/payout/share exclusions. Endpoints and owner/voting/payout destinations are rendered; unmodelled share/script fields produce missing-field findings. Unknown oracle objects/leaves are failing `harness-gap` records, including empty unknown objects. Decode-failure suppression is explicit. | All 879 independently mutated non-exempt scalar leaves were caught on clean `testnet-7000`, `testnet-7090` and `regtest-feature_protx_version-201` fixtures. The 46 explicit chain/index leaves were excluded from that sweep. Committed share, address, deprecated-service, script-rendering and unknown-object regressions pass. |
| 5. Terminal nonzero blk suffix silently discarded | **Closed.** Both scanner paths report short/nonzero terminal data; live-file tails also fail coverage. | Scanner tests and `COVERAGE_CHECKS`/gate inspection. Stored-zero preallocation remains exempt. |
| 6. Incorrect blk-mode v24 AssetUnlock txid reference | **Closed for the reported v24 rule.** The independent reference zeroes the trailing 132 bytes only for the matching transaction version/type, payload size and payload-version predicates. | Both txid-rule tests pass across the fixtures, including RPC/blk agreement. Predicates and zeroing checked against Core rc.3 `src/primitives/transaction.h:382` and `src/primitives/transaction.cpp:82`. |
| 7. Inverted/empty RPC range passes | **Closed.** Explicit inversions and requests above the tip are rejected before output creation. | Binary range tests pass; implied-tip range validation inspected. |
| 8. Output cap does not bound mismatch memory | **Closed for the reported allocation paths.** Blk checking reads/reports individual blocks, bounds worker count to 8 by default, and limits transaction repro hex to 16 KiB. Tail repair reads backward in 64-KiB chunks. | Allocation-path inspection and tests. This was not an OOM/load measurement; memory still scales with worker count and individual block size. |

The source-identity probe ran the actual `build.rs` against a scratch root with read-only member symlinks. Changing only the lockfile's base-sdk source revision from `33fcc59e1868c803f9608e0e78ee8bddf3abcee7` to its existing parent `2d103909378386216bfdc5c3d153e385b53062f1` changed the code ID from `9bcbd8440dd6586e38bd9c27442df2e8` to `5d4102a10c03402a7abdbfd1d7264bda`. Separately, the public driver probe changed the code identity, resumed, fetched all ten blocks again and retained the newly observable mismatch. This verifies identity computation and checkpoint invalidation; it is not a claim that an alternative base-sdk revision's implementation was rebuilt and tested.

A gate under a deliberately weaker sampling policy certifies only that policy. Stronger-policy findings remain visible as `unverified`; they do not count as current-policy results. Unsampled transaction fields and the explicitly classified chain/index values remain outside field-equality coverage.

## BS3 fold and validation

The fold is present and correct: `conformance` is a workspace member, `dash-conformance` is assigned to `node-conformance`, its `dashcore` access is an R2 exception with its own deny entry, and both base-sdk dependencies use `workspace = true`. Root upstream/patch tables contain `dash-primitives` at the common pin, and the lockfile resolves the base-sdk dependency closure from the local fork.

| Check | Result |
|---|---|
| `cargo fmt --check` | **PASS** |
| `CARGO_BUILD_JOBS=12 cargo clippy --all-features --all-targets -- -D warnings` | **PASS** |
| `TMPDIR=/work/scratch/bs2-r2-gpt CARGO_BUILD_JOBS=12 cargo test -p dash-conformance --all-features` | **PASS**, 63 tests: 35 library, 1 blk, 13 fixture, 3 RPC, 2 txid-rule, 9 v24 finding tests |
| `contrib/node/check-base-sdk-pins.sh` | **PASS** |
| `cargo deny check` | **PASS**, advisories/bans/licenses/sources; expected unused-wrapper warnings |
| `contrib/node/check-r2.sh` | **PASS** |
| `contrib/node/check-pure.sh` | **PASS**, currently an empty node-pure group |
| `python3 .github/scripts/ci_config.py verify-groups` | **PASS**, all 20 crates assigned and matrix consistent |
| `TMPDIR=/work/scratch/bs2-r2-gpt CARGO_BUILD_JOBS=12 contrib/node/selftest-checks.sh` | **PASS**, including both the cargo-deny and R2 reachability violations through `dash-conformance` |
| `cargo test --workspace`, with the `contrib/setup-dashd.py` environment and private scratch/retention directories | **FAIL**, exit 101: the SPV `dashd_sync` suite had 31 passes and 4 failures; see below. Its 10-test masternode suite passed. |

Command logs are retained under `/work/scratch/bs2-r2-gpt/`. The independent field/code/summary probes are additional checks, not replacements for the committed suite. No full-history conformance pass or timer refresh was run. The BS3-specific node checks pass; they do not expose the RPC snapshot-coherence blocker.

The full `PROCESS.md` integration gate is **not established as green**. The four workspace failures were:

- `tests_transaction::test_coinbase_matures_without_later_wallet_activity` and `tests_transaction::test_coinbase_matures_when_a_later_block_touches_the_wallet`: `dash-spv/src/test_utils/node.rs:330`, block-generation RPC transport error `WouldBlock` / `Resource temporarily unavailable`.
- `tests_multi_wallet::test_runtime_add_during_initial_sync`: `dash-spv/tests/dashd_sync/tests_multi_wallet.rs:416`, the committed filter height reached only 5999 of the required 20000 within its 60-second window.
- `tests_multi_wallet::test_runtime_add_with_tip_advance_during_rescan`: the same block-generation RPC transport error.

Those SPV, wallet and wallet-manager sources have no diff against `node/integration`. The failure evidence is retained in `workspace-test.log` and `dashd-retain/`; I do not attribute these failures to the conformance fold without a causal reproduction. A sequential retry of the two coinbase cases also failed: the control case had the same block-generation RPC error, and the other case's private dashd failed to start within the test timeout. See `spv-coinbase-retry.log` and `dashd-retry/`. Thus the original failures cannot simply be labelled passing flakes; later workspace suites were not reached after the SPV failure.

**Verdict: CHANGES-REQUIRED.**
