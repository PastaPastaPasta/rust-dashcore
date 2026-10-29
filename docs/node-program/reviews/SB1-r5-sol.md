# SB1 layer gates — Sol r5 (narrow)

Reviewed branch `node/sb1-layer-gates` at supplied head **a88fedbd5cc2fa2fefb014a305f5cc106f72d24b** (c97e66aa4 + a88fedbd5 on 4ca7de016). Binding inputs: REVIEW-COMMON.md, Sol r1–r4, SB0 §2.3, DEC-170 and DEC-176 on integration. DEC-176 supersedes allocator feature sampling with a source-level ban over each S/pure root's all-features closure; the link probe is a sanity check, not the proof.

**Result: CHANGES-REQUIRED.** The exact R4-1 plant is closed. Both submitted residual classes have fresh false-pass evidence, and the exception validator accepts a DEC citation naming a different package and version. The branch-local decision-authority protections work.

Scope: R4-1 closure, a fresh source-scan bypass, the two stated residuals, and allocator allow-list authority. Previously closed items are not reopened without regression evidence. PLAN §7 uses codenames only; no private material is read.

Experiments are confined to `/work/scratch/sb1-r5-sol/`. Tests run with nice 10, `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER`, and mode-700 disk `TMPDIR=/work/scratch/sb1-r5-sol/tmp`, overriding REVIEW-COMMON's older RAM-backed TMPDIR advice with the task's explicit constraint. The reviewed branch is read-only. This report remains uncommitted.

## R4-1 closure

**Closed for the exact r4 plant.** S `dash-chain` has `default=["std"]`, `std=[]`, `allocator=[]`, and `#[cfg(all(feature="allocator", not(feature="std")))] #[global_allocator]` in `src/lib.rs`. Its A consumer selects defaults off plus `allocator`. On the new head the layer gate returns 0, but full `check-shared.sh` returns **1**, specifically reporting `global_allocator in its all-features closure: dash-chain v0.0.0: …/src/lib.rs:2` on **host and Android**. All six sampled linked consumers succeed; the new source scan is what closes this plant.

The selected actual product builds in release on both targets. Giving that product its own allocator fails on both with `conflicts with global allocator in: dash_chain`, so the plant remains non-vacuous. Evidence: `/work/scratch/sb1-r5-sol/probes.py`, `results.json`, and `logs/r4-{layer-gate,shared-gate,product-host,product-aarch64-linux-android,conflict-host,conflict-aarch64-linux-android}.log`.

## Confirmed source/closure gaps

**R5-1 — major — external source inputs and assembled macro attributes can escape both the source ban and the sampled link backstop.**

Refs: `contrib/node/link-probe.py:134`, `:169`; `docs/node-program/BASE-SDK-FORK.md:64`.

Move the exact R4 allocator body to `dash-chain/allocator.rs`, leave `src/lib.rs` with `#[path="../allocator.rs"] mod allocator; pub fn touch() {}`, and retain defaults off plus `allocator` in the A consumer. There is no build script, so the lib target contributes only `dash-chain/src/` to the scanner. The `#[path]` module is valid Rust source inside the package root, outside the chosen scan directory; the target directory exclusion is not involved.

The full layer gate, standalone allocator helper (host + Android), and **full `check-shared.sh` all return 0**. All/default/no-default builds omit the allocator, while the real consumer's defaults-off-plus-allocator feature subset includes it. That consumer builds in release on both targets; giving it its own allocator fails on both with `conflicts with global allocator in: dash_chain`. Thus the stated external-source residual is a concrete false pass, not cured by normal linkage. The old Android-only external-path selftest is narrower: its allocator is active in a sampled configuration.

A second fresh control covers the assembled-token residual directly. Replace the external module with a normal proc-macro dependency, `r5-alloc-macro`, whose attribute function constructs the name using `["global", "allocator"].join("_")` and parses `format!("#[{attribute}] {item}")`. The S library applies `#[r5_alloc_macro::allocator]` to its allocator static under the same defaults-off-plus-allocator cfg. Neither source contains the banned token, even though the macro's source directory is scanned. The layer gate, allocator helper and **full shared gate again return 0**; actual selected products build on both targets, and their own allocator conflicts with `dash_chain` on both. Evidence: `probes.py` mode `macro`, `logs/macro-{layer-gate,helper,shared-gate,product-host,product-aarch64-linux-android,conflict-host,conflict-aarch64-linux-android}.log`, and `macro-dep/src/lib.rs` under `/work/scratch/sb1-r5-sol/`.

Suggested fix: make source discovery complete for supported library module/include inputs, or fail closed on references outside the proved source boundary. Also define an enforceable policy for generated/assembled allocator attributes; three linked feature samples cannot establish the remaining cases. This needs mechanism closure, rather than adding only this path plant. Evidence: `probes.py` mode `path`, `results.json`, `logs/path-{layer-gate,helper,shared-gate,diff,product-host,product-aarch64-linux-android,conflict-host,conflict-aarch64-linux-android}.log` under `/work/scratch/sb1-r5-sol/`.

**R5-2 — major — root all-features does not bound dependency features unified by a downstream product.**

Refs: `contrib/node/link-probe.py:112`, `:119`, `:169`; `docs/node-program/BASE-SDK-FORK.md:64`; DEC-176's superset claim.

A minimal S-shaped `dash-chain` normally depends on registry package `r5-facade 99.0.0`. That package's disabled `allocator` feature enables optional registry package `r5-hidden-allocator 99.0.0`, whose library contains an ordinary, unconditionally active allocator. Both host and Android root all-features closures contain only `dash-chain` and `r5-facade`; the allocator package is absent. **The exact reviewed allocator helper returns 0**, including all six links and both scans. A standalone downstream product depends on `dash-chain` and enables `r5-facade/allocator` directly; Cargo unifies that feature onto the facade beneath the shared crate. Its release products build on both targets; adding the host-owned allocator fails on both with `conflicts with global allocator in: r5_hidden_allocator`.

The registry is an offline alternate registry using Cargo's directory-source replacement, with per-file checksum manifests. Metadata verifies the facade's source is `registry+file://…`, and the product tree verifies the unified normal dependency. These are registry package IDs, not a path dependency or a `[patch]` standing in for one. The minimal fixture copies `link-probe.py` and `check-layers.py` unchanged; it does not run the whole workspace layer/shared gate.

Suggested fix: either establish and enforce the permitted downstream dependency-feature configurations, checking their union and linkage, or resolve/scan a conservative feature closure that also bounds permitted transitive feature activation, failing closed when it cannot. Keep the source ban for configuration-independent literal attributes. This does not reopen the accepted L2 limit for the actually resolved workspace graph: it assesses the new allocator-proof claim under DEC-176 and the residual explicitly submitted for this round. Root `--all-features` activates root/workspace features, not every dependency feature available to another consumer.

Evidence: `optional-probes.py`, `optional-results.json`, `logs/optional-{closure,helper,product-tree,product-host,product-aarch64-linux-android,conflict-host,conflict-aarch64-linux-android}.log`; fixture and product sources under `registry-fixture/`, `registry-product/`, `registry-vendor/`, all under `/work/scratch/sb1-r5-sol/`.

## Allow-list authority

**R5-3 — major — substring matching lets the branch borrow an exception for a different package and version.**

Ref: `contrib/node/link-probe.py:155`.

The lookup is correctly pinned to `refs/heads/node/integration`, but `name not in row or version not in row` checks substrings, not package/version identities. In the real scratch worktree, add an outside normal dependency **`bls 0.3.1`** with the R4 cfg-gated allocator body. Root `std` and `allocator` forward the corresponding dependency features. With the empty allow-list, the helper rejects the dependency's source on **both targets**. Set only `[allocator] allow = { "bls@0.3.1" = "DEC-79" }`. The actual integration DEC-79 row names **`blst 0.3.17`**, neither this package nor its version; the two substrings nevertheless match. **The layer gate, allocator helper and full shared gate then all return 0**. Selected release products build on both targets; adding the product's allocator fails on both with `conflicts with global allocator in: bls`.

This is a synthetic `bls` package, not a change to blst, and no integration decision or reference was modified. An independent signed fixture also accepts `sb1-r5-allocator@99.0.0` against a row approving only `sb1-r5-allocator-extra 99.0.01`. Unrelated names and non-prefix mismatched versions correctly fail.

Suggested fix: validate the complete package name and complete version as a named pair in the authoritative decision, preferably through a structured exception record. Require exact identity and reject prefix/subsequence matches. Add distinct-name and distinct-version prefix controls. The linked samples cannot compensate for an unauthorized skip when the allocator is active only in the supported feature subset.

Evidence: `probes.py` mode `allow`, `results.json`, `logs/allow-dep-{no-waiver,authority-row,layer-gate,helper,shared-gate,product-host,product-aarch64-linux-android,conflict-host,conflict-aarch64-linux-android}.log`; `allow-probes.py`, `allow-results.json`, `logs/allow-prefix-mismatch-accepted.log`; all under `/work/scratch/sb1-r5-sol/`.

**Self-authorization by branch-local decision rows is closed.** In an independent signed fixture using the exact two reviewed Python files, an exact authoritative pair passes. A new local decision row, a locally rewritten existing row, `L7_BASE_REF=HEAD`, a signed local commit carrying that rewritten row, and a colliding signed `node/integration` tag all fail with the required integration-decision diagnostic. Removing only the fixture's qualified integration branch, leaving the tag and local rows, also fails closed. The fully qualified branch reference wins. Both fixture commits verify **G**. No real integration ref or reviewed-branch content changed. Evidence: `allow-probes.py`, `allow-results.json`, `allow-fixture/`, and the corresponding `logs/allow-*.log` files.

## Residual disposition

Neither submitted residual is acceptable as an allocator proof backed by the present link probe. The source-boundary/assembled-token cases evade the three sampled feature builds; the registry-feature case changes the dependency closure through ordinary downstream unification. Normal linkage reliably rejects an allocator **when that configuration is actually tested**, as every conflict control demonstrates. It does not make the sampled configurations exhaustive. DEC-176 explicitly says the link probe is not the proof, so the docs cannot transfer missing source/closure coverage to it. A mechanism change or an enforced supported-product/feature boundary is required before this gate can be satisfied.

No production crate violation is asserted. Previously closed L7, layer/name, mobile-platform and denylist items remain closed; the accepted L2 registry limitation is unchanged. R5-2 concerns the allocator proof, not a new general graph requirement.

## Verification and limits

Clean `check-layers.py` passes L1/L2/L3/L8. Clean `check-mobile.sh --no-size` passes all four L6 product/target graphs, retaining the existing SB3 baseline entries. Evidence: `logs/clean-layer-gate.log`, `logs/clean-mobile-gate.log`. The implementer's full gate, selftest total and two out-of-scope flakes (including BL-85) are reported evidence; this narrow review does not independently verify them. No full workspace build/test, full selftest, Apple build/lint, integration overlay, Android size remeasurement or dashd run is claimed. The targeted release products, six-link helpers and full shared gates above were run independently with assertions on both exit status and the allocator-conflict reason. Every actual-product control was built on host and Android, and every conflict control failed for the asserted allocator reason.

All scripts, results, logs and outside fixtures remain under `/work/scratch/sb1-r5-sol/`. The detached scratch worktree has been restored clean to the reviewed head and retains its own target directory. The empty TMPDIR was removed after tests; recreate `/work/scratch/sb1-r5-sol/tmp` with mode 700 before rerunning the scripts. The allow-list script recreates its independent signed fixture. Cargo runs use a private offline Cargo home reusing registry/git data, with no shared `CARGO_TARGET_DIR`. No reviewed-branch edit, commit, push, GitHub action, private-material read, live-node access or process-pattern kill occurred. Only the independent authority fixture contains new signed experiment commits. The implementation worktree remains clean at **a88fedbd5cc2fa2fefb014a305f5cc106f72d24b**; this integration report is untracked, unstaged and uncommitted.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
