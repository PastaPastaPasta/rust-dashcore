# MN0-1 review, round 2 (Sol)

Scope: `git diff 8a892fe0a..ca76c146f` on `node/mn0-1` (11 files, +204/-26), reviewed against Dash Core `v24.0.0-rc.4` (`49caf372bb`). The worktree had a pre-existing modified `Cargo.lock`; it was not changed by this review.

## Verification

- `cargo test -p dashd-mining`: 10/10 unit tests and doc tests passed.
- `cargo test -p dashd-node --features test-support --test mn0_rebuild`: 1/1 passed.
- `DASHVERSION=24.0.0-rc.4 python3 contrib/setup-dashd.py` found the cached rc.4 binary; `cargo test -p dashd-node --features test-support --test mn0_template_dashd -- --nocapture`: 4/4 passed, including the 431-block differential, the testnet height-1 nBits check, the block-version check, and the refused-template text check.
- `cargo build -p rdashd --release`: passed. `cargo tree -e features -p rdashd` shows only rdashd's normal `dashd-node` dependency; `dashd-mining` and `test-support` are absent from the release graph. `dashd-node`'s `dashd-mining = ... features = ["test-support"]` is a dev-dependency, and workspace resolver 2 keeps that feature out of a normal rdashd build. The hook is therefore not compiled into the product build; this remains true when a normal dashd-node dependency on dashd-mining is added, unless that normal dependency explicitly enables the feature.
- `git diff --check`: clean.

## Answers to the requested checks

### n1 and the V query path

`QueryDeployments<'a>` is `#[derive(Clone, Copy)]` over one `&Deployments<'a>` and its constructor only copies that reference. `ChainDeployments::view()` also constructs `Deployments` from four references. Neither operation allocates or takes a lock. The only cache lock on this path is the expected one inside `compute_block_version`/versionbits state evaluation, which can fill `VersionBitsCache` just as Core's CreateNewBlock does.

The tuple field is private, and Query exposes only `compute_block_version` plus the `DeploymentView` methods (`active_after`, `active_at`, `versionbits`). `VersionBitsCache::clear` is not reachable through a `ChainQuery`; the other Query fields expose chain/index/signature views, not `ChainDeployments`. I found no alternate Query route to clear the cache. n1 is closed.

### Hook

`set_between_turns` and the `BetweenTurns` type are both behind `#[cfg(feature = "test-support")]`. The feature is enabled only on dashd-node's dev-dependency for its two test binaries. A release build of rdashd passed, and its feature tree contains no dashd-mining/test-support node. This is safe under resolver 2, including the planned future normal dependency, because dev-dependency features are not part of the normal dependency graph.

### Tests

- **m1:** the testnet case catches the intended M13 (`bits = tip.bits`) and verifies the known rc.4 testnet values (`genesis = 0x1e0ffff0`, block 1 = `0x1e0fffff`). It is a fixed-vector assertion, however; it does not vary the candidate time or independently assert the `> 2 * target_spacing` min-difficulty condition. See finding n6 below for this remaining test-strength nit.
- **m2:** the hook is installed after an initial mined block. On the next call it connects that block after the query, the returned template must have height 2 and the new hash as `prev`, and the atomic hook count must be exactly 2 (one query/validity pass for each V turn). The test then compares the complete captured log lines to one exact `CreateNewBlock()` line and checks the reserved stats, so a stale return, an unbounded extra pass, or duplicate stats/log emission is caught. The test has its own binary as intended.
- **n5:** `Dashd::logged` reads the real rc.4 `regtest/debug.log`, removes only the timestamp prefix, and compares all 431 matching lines to the exact expected suffix. The file logger writes each line directly; this is not a synthetic tracing-only assertion.

### rc.4 cite spot checks

The re-pinned comments match rc.4 at the cited locations: `UpdateTime` 49-57; `nTime` 216; version computation and `-blockversion` 209-214; counters and log 259-261; coinbase/subsidy 263-279 and 271; header `nBits`/TBV 343-354; `GenerateBlock` outcome checks 148-153; `generateBlocks`' assembler 173; and `generateblock`'s no-mempool assembler 380. This is more than the requested five spot checks.

### Survivors

M15 is equivalent on regtest. Regtest has `fPowNoRetargeting`; every generated block and the block after the tip use the powLimit compact bits, so taking the next block's bits instead of the tip's bits gives the same subsidy input at every reachable height. The testnet early-chain subsidy cap likewise does not create a difference in the MN0-1 range.

M18 is correctly deferred to BL-467. Regtest's no-retargeting path ignores the candidate time and returns powLimit, so `now` versus the assembled block's final time cannot be distinguished cheaply there. Killing it requires a min-difficulty network with real-PoW history and a frozen clock behind MTP, as the backlog records.

## Findings

### n6 (minor): m1 pins the testnet result but does not exercise the min-difficulty predicate

- **Where:** `dashd-node/tests/mn0_template_dashd.rs:360-376`.
- **Scenario:** the test starts from the fixed testnet genesis and asserts the two known compact values. A mutant that returns the expected `0x1e0fffff` for this fixture, or a boundary mistake that is not exercised by this one timestamp, would pass. The test does kill M13 and proves the observed output, so this is a test-strength gap rather than a production mismatch.
- **Suggested fix:** add a nearby candidate-time case around `tip.time + 2 * target_spacing` (or expose/assert the calculated candidate time) and check both sides of the strict comparison. Keep the fixed-vector assertion for the M13 regression.

## Verdict

**APPROVE-WITH-NITS.**

There are no blockers or major findings. The round-1 n1, m1 (M13 coverage), m2 retry coverage, n2 cites, and n5 log assertion are addressed; n6 is the remaining minor test-strength nit.
