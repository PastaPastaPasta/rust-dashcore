# SB1 layer gates — Sol r4 (narrow)

Reviewed branch `node/sb1-layer-gates`, supplied head **4ca7de016** (r3 fixes cde0f803c + 4ca7de016; not rebased). Binding inputs: REVIEW-COMMON.md, Sol r1–r3 reports, SB0 §2.3 and DEC-170 on integration. Scope: mechanism closure for R3-1/R3-2/R3-3, one fresh bypass attempt each, residual dispositions, and fixture-test non-vacuity.

The original R3-1 and R3-3 mechanisms are closed. The original R3-2 default-feature case is closed, but the linked-product feature coverage still has a confirmed major gap (R4-1). No previously settled r1–r3 finding is reopened.

| Item | Disposition |
|---|---|
| R3-1 — reference / decision authority | Closed. Qualified integration ref is used for both; environment redirect, colliding tag and linked-worktree attempts fail. |
| R3-2 — allocator feature selection | Original default-only case closed; broader mechanism incomplete, R4-1. |
| R3-3 — external name-marked pure | Closed. Actual resolved package names, folded for case and separator, are checked independently of layer. Renamed `[patch]` plant fails. |
| Pure name from B source | Acceptable fail-closed behavior; no required change. |
| Feature subsets with defaults disabled | Required fix, R4-1. |
| New moved-constant fixture cases | Non-vacuous; positive and negative controls pass, and deliberate setup/comparison faults are detected. |

All experiments use nice 10, eight Cargo jobs, empty RUSTC_WRAPPER, and mode-700 disk TMPDIR `/work/scratch/sb1-r4-sol/tmp`. The branch under review is read-only. PLAN §7 handling uses codenames only; no private material is read. This report stays uncommitted.

**R4-1 — major — allocator coverage still omits a feature configuration a real product can select.**

`contrib/node/link-probe.py:94` covers all/default/no-default features only. Fresh S `dash-chain` plant: `default = ["std"]`, `std = []`, `allocator = []`, and `#[cfg(all(feature = "allocator", not(feature = "std")))] #[global_allocator] static A: std::alloc::System = std::alloc::System;`. A workspace A consumer selects `dash-chain` with `default-features = false, features = ["allocator"]`. Both the full layer gate and **full check-shared.sh return 0**. The product builds in release on host and Android. Adding its own allocator then fails on both with `conflicts with global allocator in: dash_chain`, proving that the shared library installed the allocator in a normally linked product.

This is the reported feature-subset residual, now demonstrated on a concrete consumer; it is required, not acceptable as an undocumented coverage limit. The original R3-2 default-only plant is closed: the updated helper rejects it on both targets, specifically in the default-feature probe. The general feature-selection mechanism remains incomplete. Suggested fix: define the supported product feature configurations, derive their resolved library feature sets, and run allocator consumers for those sets, including defaults disabled plus selected features. If every exposed combination is supported, provide exhaustive or otherwise fail-closed coverage; three samples cannot establish that prohibition. Keep the linked-consumer approach.

Evidence: `/work/scratch/sb1-r4-sol/logs/allocator-subset-{layer-gate,shared-gate,product-host,product-aarch64-linux-android,conflict-host,conflict-aarch64-linux-android}.log`, `allocator-r3-default-closure.log`, and `probes.py`. No production crate violation is asserted.

**R3-1 closure — L7’s reference and decision authority.**

Refs: `contrib/node/check-layers.py:632`, `:650`, `:690`. The checker does not read `L7_BASE_REF`; reference history and authoritative decision rows both use `refs/heads/node/integration`. Local decision rows cannot replace an existing integration row. The extracted regression cases confirm rewritten DEC-170 is rejected even with the old environment redirect.

Fresh probe: in a signed throwaway repository, integration holds the bootstrap constant and DEC-170, while a feature history records +200 KiB. Put a signed tag named `node/integration` at that feature record, add another feature commit, and create a detached linked worktree. The +400 KiB measurement fails in both worktrees, using **12,615,752 B**, growth **409,600 B**, allowance **252,315 B**; setting `L7_BASE_REF=HEAD` does not change either result. Removing only the fixture’s qualified integration branch, leaving the colliding tag, fails with `no merge base with refs/heads/node/integration`. No fallback accepts the tag or branch-local record. Fixture commits verify **G** signatures. Evidence: `l7-probes.py`, `l7-results.json`, `logs/l7-tag-worktree-{fixture,linked}.log`, `logs/l7-env-ignored-{fixture,linked}.log`, `logs/l7-tag-only-fails-closed.log`, `logs/l7-signatures.log`. These tests do not mutate the real integration ref.

**R3-3 closure — pure rules follow the resolved name through aliases and patches.**

Ref: `contrib/node/check-layers.py:433`. Declare `dashd-r4-pure` as N, pure and pending. Resolve outside package **Dashd_R4_Pure v99.0.0** under dependency key `alias_pure` in `dash-rpc-spec`, using a crates.io declaration redirected through `[patch.crates-io]` to an outside path package. Its release library builds and contains a forbidden filesystem call. Cargo tree verifies the package is actually reached. L1 rejects that package specifically as **N marked pure from outside the workspace**. The dependency rename, case/separator difference, and patch do not bypass the name rule.

Classifying the same resolved source as B via `[b].sources` still rejects it specifically as **B marked pure**. This is the base-SDK-name residual: acceptable as is. DEC-170 makes the pure flag name-keyed with no B exemption; when the checker cannot lint an outside package, rejection preserves that rule. `BASE-SDK-FORK.md:62` already says outside pure names fail “whatever its layer,” so the behavior is described broadly. Explicitly naming B would be optional clarification, not a gate requirement. The B control uses a synthetic source classified through the same mechanism, not a modified SDK fork. Evidence: `logs/pure-renamed-patch-{gate,tree,build}.log`, `logs/pure-base-sdk-name-gate.log`, and `probes.py`.

**Fixture selftest non-vacuity.**

Refs: `contrib/node/selftest-checks.sh:51`, `:831`. The negative fixture must return nonzero **and** match the specific moved-constant diagnostic; an unrelated setup failure is insufficient. The positive fixture must return zero. Its setup operations and final checker invocation form one `&&` chain, so failed copying, initialization, checkout or signing cannot skip the checker and return success. The fixture puts the original `[size]` and integration decision rows in a signed base commit with no size record, then changes the NDK to 30.0.0 and commits the new decision with the change. The old ruling rejects the movement; the new ruling accepts it. Command substitution confines the fixture’s `cd`, so the caller’s reset still acts on the detached test worktree.

I extracted the L7 section and assertion/reset helpers from the exact reviewed head without changing their case bodies: **30 cases passed, zero FAIL, exit 0**. This is the count independently established here, not the implementer’s reported 32-case selection. Both new fixture cases are included. Two targeted mutation controls then tested those exact fixture assertions:

- Insert a failing operation before the fixture copy: **both assertions fail**, including the negative case, rather than accepting setup failure as the expected gate rejection.
- Disable the base-vs-working constant comparison in the copied checker: the negative assertion **fails** because the required comparison diagnostic disappears; the valid new-ruling control still passes.

The control harnesses assert the expected failure counts and exit 0 only when those mutations are caught. Evidence: `selftest-l7.sh`, `fixture-setup-failure.sh`, `fixture-disabled-comparison.sh`, and their matching files under `logs/`. The fixture checker mutations are only in throwaway repositories; the implementation branch is unchanged.

**Verification and limits.**

The allocator plant passed the full layer and shared gates, including all six release allocator consumers, cargo-deny and the four host/Android clippy runs. The selected actual-product configuration was built in release on both targets; the two allocator-conflict controls failed for the asserted reason. The default-only closure check rejects on both targets. Clean L6 passes for both mobile products on Android and iOS graph targets. Clean L1/L2/L3/L8 also passes; see `logs/clean-layers.log` and `logs/clean-mobile.log`. I did not repeat the full 269-case selftest, full workspace build/test gate, BL-20 runs, Apple builds/lints, integration overlay, or Android size measurement. The implementer’s full-gate/test counts and artifact measurement remain reported evidence. Existing accepted platform and denylist limits are unchanged.

All logs, scripts and result manifests are under `/work/scratch/sb1-r4-sol/`. The detached build worktree and empty TMPDIR were removed after testing; recreate both before rerunning `probes.py` (worktree at reviewed head, TMPDIR mode 700). Cargo runs use a private offline Cargo home sharing registry/git data, eight jobs, empty wrapper, nice 10, and disk TMPDIR. Each worktree owns its target directory; no shared CARGO_TARGET_DIR is set. No reviewed-branch edit or commit, push, GitHub action, private-material read or process-pattern kill occurred. Scratch fixture commits are signed normally. The implementation branch remains clean at **4ca7de016e34800ff542e59a4c155976547333bf**. The report remains uncommitted.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
