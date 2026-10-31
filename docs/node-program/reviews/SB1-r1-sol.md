# SB1 round 1 — Sol implementation review

Reviewed `node/sb1-layer-gates` at **6e16045a54d48081aa19ddc376739d38582b7331**, the 11-commit change from `571e6a94`. Binding inputs: PLAN row SB1; SB0 §2.3 L1–L9, APPROVED rev6.1/DEC-119 with errata; DEC-81; `briefs/REVIEW-COMMON.md`; and the supplied `sb1-layer-gates-facts.md` memory note. File:line references below identify the reviewed head unless stated otherwise.

**Result: CHANGES-REQUIRED.** The ordinary planted cases pass their selftests, but additional resolved-graph and compiled-code probes demonstrate real false passes. Seven major findings and one minor follow. These are gate defects; no existing production layer violation or consensus vulnerability is asserted.

**M1 — major — Layer checks delegate to external packages that are never checked, and miss unlisted external node crates.**

Refs: `contrib/node/check-layers.py:217`, `:226`, `:233`, `:365`; `contrib/node/layers.toml:28`.

`check_direction` stops at every allowed layered package by returning `skip`, but its outer loop checks only workspace members. Classification by package name therefore does not finish the implementer's external-copy fix: an external copy of an *allowed* package is trusted to answer for its graph, although no check visits that copy as a root.

Two detached plants both returned **0** from `python3 contrib/node/check-layers.py`:

- `dash-spv (V) → dash-sha256 v99.0.0 (outside path copy, P) → [build] dash-args (N)`. The renamed dependency key was `sha_external`; `cargo tree -e normal,build --target all` independently shows the complete path. This is V→N through a dependency's build dependency. L6's normal-only graph does not rescue it.
- `dash-spv (V) → dash-sha256 v99.0.0 (outside path copy, P) → dash-dml v99.0.0 (outside path copy, Pn)`. This is the expressly forbidden V→Pn reachability.

There is a second classification hole: `dash-spv → dashd-sb1-unlisted`, a normal dependency outside the workspace, passes **both** the layer check and `check-mobile.sh --no-size`. SB0 classifies every `dashd-*` crate as N and L6 bans any `dashd-*`; the implementation instead recognizes only names explicitly present in `layers.toml`. L1 checks workspace membership and cannot catch this outside package.

B also has no independent enforcement: `[allowed]` has no B row, SDK packages are not assigned B, and neither direction nor ban checks root at SDK packages. Consequently a B→W edge under a V consumer, or a B-only bincode path under a V/product consumer, has no B check. This is a missing replacement for R2's direct wrapper restriction on a base-SDK package depending on W. I inspected this coverage gap; the additional B cases were not planted in the pinned fork.

Suggested fix: classify all resolved package IDs, including node-name patterns and the pinned SDK source, and check each classified package's own graph. Preserve actual IDs when visiting copies. Only delegate at a package ID that is guaranteed to receive the relevant rule check; otherwise continue the walk. Apply the `dashd-*` prohibition independently in L6. Add the two allowed-copy cases and the external unlisted-node case to selftests. The allowed-copy bug is source-independent: path, git and registry packages all take the same `skip` branch; the live plants used paths.

Evidence: `external-allowed-p-build-to-n.log`, `external-allowed-p-tree.log`, `external-allowed-p-normal-to-pn.log`, `unlisted-external-node-graph.log`, `unlisted-external-node-mobile.log` in the review log directory below.

**M2 — major — The hot-path W check misses workspace feature unification.**

Refs: `contrib/node/check-layers.py:307`, `:333`, `:235`.

The general graph includes features enabled elsewhere, but stops at allowed N members. The dedicated hot-path graph then resolves only the hot-path package, losing those features. The comment assigning unified features to the other crate does not enforce the requirement that the hot path itself reaches no W.

Plant: `dashd-p2p (N, hot path) → dashd-review-bridge (N)`. The bridge's `legacy` feature enables `dash-coreimport/wallet`; coreimport's new `wallet` feature enables `key-wallet`. A separate T member enables `bridge/legacy`. All feature declarations satisfy the `allow_w` two-graph test. The layer check returns **0**. However,

```sh
cargo tree --locked -p dashd-p2p -p sb1-review-driver \
  --all-features -e normal,build --target all
```

shows `dashd-p2p → dashd-review-bridge → dash-coreimport → key-wallet → …`. The hot path reaches W in an ordinary jointly selected workspace build.

Suggested fix: check each hot-path root's full closure in the already resolved workspace graph, without stopping at N or `allow_w`. Retain the package-local on/off graphs for the separate feature-boundary requirement. Add a positive-control case with features unified onto an unlayered helper and a failing hot-path case through the allowed boundary.

Evidence: `hot-path-workspace-unification.log`, `hot-path-unified-tree.log`. The unlayered-helper control is correctly rejected in `unlayered-workspace-unification-control.log`.

**M3 — major — L4/L5 allow additional OS-thread entry points and a conventional pool constructor.**

Refs: `contrib/node/pure-clippy.toml:85`, `contrib/node/shared-clippy.toml:18`, `:28`; `contrib/node/check-shared.sh:52`.

This compiled library body passes **both** `check-pure.sh` in a P crate and `check-shared.sh` in an S `dash-chain` crate:

```rust
pub fn launch<'scope, 'env>(scope: &'scope std::thread::Scope<'scope, 'env>) {
    drop(scope.spawn(|| ()));
}
```

`Scope::spawn` starts an OS thread. Neither ban list contains it, and the pure type list does not ban `Scope` or `ScopedJoinHandle`. Having the host create the scope does not make this library's thread spawn satisfy L4 or L5.

An S crate with `threadpool = "1"` and `threadpool::ThreadPool::new(2)` also passes `check-shared.sh`; the constructor creates its own workers. The pure dependency gate rejects this crate, but S has neither that dependency ban nor the constructor/type ban.

Suggested fix: add `std::thread::Scope::spawn` and the supported pool constructors/types to the proper ban lists. Include dependency-level thread/pool hygiene for S, retaining the explicitly audited blst exception. Add regressions for both entry points; the existing direct `thread::spawn`, Builder and rayon cases do not cover them.

Evidence: `scoped-thread-pure.log`, `scoped-thread-shared.log`, `threadpool-shared.log`, all exit **0** with their respective success messages.

**M4 — major — Target-specific impure/shared bodies are never linted.**

Ref: `contrib/node/clippy-lib.sh:86`.

The common lint runner tests two feature configurations on the Linux host, without a mobile target. This body passes both feature runs in a P crate and, separately, in an S `dash-chain` crate:

```rust
#[cfg(target_os = "android")]
pub fn launch() {
    drop(std::thread::spawn(|| ()));
}
```

Both gates return **0**. `cargo build --locked -p <planted-crate> --target aarch64-linux-android` also succeeds, proving the violating body is valid on the supported target. Cargo-deny and L6 cannot catch this: the body uses only std and adds no banned dependency. The same blind spot applies to target-specific std filesystem, clock, socket and process calls.

Suggested fix: add a defined target matrix to purity/shared lint enforcement, with Android on agentbox and Apple-target enforcement on the Mac path, or an equivalent fail-closed check for bodies the host run does not compile. Include an Android-only thread-spawn regression. The documentation's existing “on Linux” limitation describes the gap but does not enforce the mobile purity/hygiene requirements.

Evidence: `android-only-thread-pure.log`, `android-only-thread-shared.log`, and both corresponding `*-build.log` files, all exit **0**.

**M5 — major — L7 measures against a mutable same-PR baseline, allowing cumulative unruled growth.**

Refs: `contrib/node/check-layers.py:401`, `:408`, `:418`; `contrib/node/mobile-size.txt:3`.

The size arithmetic is correct for one invocation, but `--record` replaces the reference used by subsequent invocations. There is no comparison to the baseline at the PR base. Without a ruling, the detached probe performed:

```text
base                                12,615,752 B
size 12,820,552 release --record     exit 0  (+200 KiB)
size 13,025,352 release --record     exit 0  (+another 200 KiB)
size 13,025,352 release              exit 0  (delta 0)
```

The final record has `ruling = -`, despite **409,600 B** growth in the same uncommitted change, above the original **252,315 B** allowance. These are ordinary documented baseline updates during development, not a hand-edited baseline. A direct +300 KiB comparison against the original record correctly fails.

Suggested fix: bind the check and record operation to an immutable PR-base/integration reference. Compare the built artifact and any proposed new record against that prior record; a new same-PR record must not reset the allowance. A ruling must cover the total step and profile change relative to that reference. Add a repeated-record regression.

Evidence: `size-first-record.log`, `size-second-record.log`, `size-total-400k.log`, `size-ratcheted-record.txt`; failing control `size-300k-control.log`.

**M6 — major — The global-allocator gate misses conditional and expanded attributes.**

Ref: `contrib/node/check-shared.sh:47`.

The grep matches only a source line beginning with `#[global_allocator`. The following S `dash-chain` source, with `std = []` as a crate feature, passes `check-shared.sh`:

```rust
#[cfg_attr(feature = "std", global_allocator)]
static ALLOC: std::alloc::System = std::alloc::System;
```

The all-features run actually installs the global allocator. The no-default-features run reports only an unused-static warning, and the gate exits **0**. Macro expansion and attribute placement on the same line as other tokens create similar holes in the textual check. Direct method/type clippy bans do not cover allocator attributes.

Suggested fix: enforce the attribute on parsed/expanded Rust, or use a conservative source check that rejects both the direct attribute and `cfg_attr`/macro forms with regression coverage. Do not treat an unsuccessful/no-match grep as proof of the absence of an expanded allocator.

Evidence: `cfg-attr-global-allocator.log`. The ordinary `#[global_allocator]` plant is correctly rejected by the existing selftest.

**M7 — major — Baseline keys silently grandfather additional violation routes.**

Refs: `contrib/node/check-layers.py:126`, `:249`, `:273`, `:373`.

Violations are deduplicated by `(rule, root → endpoint)`, losing dependency edge kind, source identity, target condition and all additional paths. A baseline for the original direct normal `dash-spv → dashd-sb1-review` violation passes. Adding a distinct path `dash-spv → sb1-extra-route → [build] dashd-sb1-review` also passes without changing that entry. Cargo tree confirms the extra build edge; the check still prints only the original shortest route. Thus the ratchet rejects new endpoint pairs but does not reject new offending edges sharing an endpoint.

The same representation is used for the current L6 entries: another clap/tracing-appender route or package version beneath the same product and target inherits the existing exemption. Removal of one formerly exempt route also is not stale while another route remains. This is narrower than the promise that unlisted new violations fail and resolved listed violations require removal.

Suggested fix: define and record a stable violation identity that includes the actual forbidden edge/package source and edge context, or record the full set of grandfathered offending edges under each root. Reject additions to that set and removals of its listed elements. Merely preserving one shortest path cannot detect an added parallel route. Add a same-endpoint normal/build and second-source regression.

Evidence: `endpoint-baseline-initial.log`, `endpoint-baseline-additional-build-route.log`, `endpoint-baseline-tree.log`.

**m1 — minor — Size comparisons do not bind the Android build environment.**

Refs: `contrib/mobile/android-env.sh:11`, `:13`, `:20`; `contrib/node/mobile-size.txt:6`.

The record's comment names NDK 29.0.14206865/API 24, but the recipe accepts any `ANDROID_NDK_HOME`, otherwise chooses the newest installed NDK, and accepts any `ANDROID_API`. No machine-readable record verifies these against the measured baseline. Upgrading the installed NDK can therefore change the measured artifact independently of a PR, potentially obscuring or manufacturing a size change. Rust itself is pinned by `rust-toolchain.toml`.

Suggested fix: record and validate NDK, API and relevant build-profile inputs, requiring an explicit baseline migration when those inputs change. My same-environment rebuild/strip is reproducible; I did not perform a second toolchain build on the loaded box.

**Verification and limits**

- Clean head: `check-layers.py`, `check-pure.sh`, `check-mobile.sh --no-size`, and `ci_config.py verify-groups` all pass. The clean head has no S members, so shared-code enforcement was exercised with detached S plants.
- Ran **69 existing SB1-section selftests**, extracted without changing their case bodies from the layer-rules through CI-groups sections of `selftest-checks.sh`: **69 passed**, with no `FAIL`. This covers direct/transitive normal/build N→V, V→N and V→Pn, unlisted wrappers, hot-path W, the W→W positive case, allow_w on/off, package aliases including grovedb-bincode, mobile target selection, direct shared hygiene plants, the +300 KiB size comparison, and baseline validation. I did **not** run the full 194-case suite or independently establish the implementer's count of 77 SB1-related cases elsewhere in it.
- Separate stale-entry plants for **L1, L2, L3, L8 and L6** all fail with the intended stale-entry diagnostic. L4/L5 cannot be waived by the baseline schema; L7 uses its separate size record. The false pass in M7 concerns scope expansion, not the clean absent-endpoint stale-entry case.
- A normal/build target-qualified edge through an unlayered outside dependency is correctly rejected (`target-build-of-dependency-control.log`). General workspace feature unification through an unlayered helper is also rejected. M1/M2 identify the cases where the layered-package shortcut or separate hot-path resolution defeats those otherwise useful checks.
- An external macro expanding thread spawn, filesystem read, clock read and socket bind fails the P gate on the real std DefIds. The same macro's thread spawn fails the S gate. An external `pub use std::fs::read` re-export also fails. These tested macro/re-export routes are **not** false passes. A conflicting crate-local clippy `msrv` causes `write_conf` to fail and the caller to reject the crate, confirming that reported fix.
- Ran `check-mobile.sh` on the reviewed worktree, using its own existing target, `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and the requested disk TMPDIR. It rebuilds the **ELF64 AArch64 shared object** `target/aarch64-linux-android/release/libdash_spv_ffi.so`; stripped size is exactly **12,615,752 B**, delta **0**, allowance **252,315 B**. Two separate llvm-strip outputs have identical bytes and SHA-256 `bccc44cee5876e52ea4d303d3c0d12eec6b7413d41351083ccd8ac2ce7d03bf9`. This verifies the artifact and same-environment stripping, not reproducibility across NDKs or two clean builds. The +300 KiB controls exercise the numeric comparator, not a linked +300 KiB object plant.
- No full integration gate or dashd tests were run, as instructed. The reported four load flakes and full gate-copy result are **not independently confirmed** by this review. I did not re-gate a merge onto the subsequently moved integration head.
- All adversarial plants were confined to `/work/scratch/sb1-r1-sol/wt`, detached at the reviewed head, or its outside scratch dependencies. The selected selftests create and remove their own detached scratch worktree under the requested TMPDIR. Each custom probe restores its tracked inputs. No `CARGO_TARGET_DIR`, shared target, `/dev/shm`, live node, commit, push or GitHub action was used. The reviewed branch remains clean at the exact supplied head. PLAN §7 handling used codenames only; no excluded private material was opened.

Reproduction scripts, command exits and logs are retained under `/work/scratch/sb1-r1-sol/`: `probes.py`, `probes-extra.py`, `probes-targets.py`, `probes-final.py`, `selftest-sb1.sh`, the `results*.json` manifests and `logs/`. The mode-700 TMPDIR was empty and removed after the runs; recreate it before rerunning the scripts. The review report is uncommitted.

**Disposition of reported deviations and R2 removal**

`cargo tree -e normal` for L6 is appropriate: the binding rule concerns the shipped normal closure, while cargo-deny's build graph includes cbindgen's host-only clap. Target-specific Android/iOS graph selection works in the tested cases. Avoiding registry packages' unused optional dependencies is also reasonable for the actually resolved workspace graph; arbitrary downstream feature combinations are not thereby proven safe. Retain that coverage limitation explicitly.

`pure = true` and `pending` are useful, correctly tested extensions. The classifications of dash-args and dash-rpc-spec as N, and git-state as W, fit their ownership and prevent them from becoming shared-layer escape routes. Using `release` temporarily is acceptable because SB2 explicitly owns `profile.mobile`, the record labels the fallback, and the comparator rejects a profile mismatch; that does not excuse M5. Stopping N's bincode ban at W reached through a valid `allow_w` boundary is consistent with the legacy-wallet boundary: it permits the wallet's internal codec without permitting N's own protocol codec.

R2's removal is authorized by DEC-81/SB0, and the L-rule model intentionally permits new shared dependencies. The old `[graph] exclude` binary-only check is replaced adequately by L1 classification and L2's prohibition on lower-layer→T normal/build edges; the new graph still reads excluded tool edges. R2's own-dev-edge reachability restriction is **not retained**: an N crate dev-depending on V now passes, explicitly tested by this implementation. That is consistent with L2's specified normal/build scope and the test exemptions, but should be stated as an intentional loss of a test-only restriction, not described as identical R2 enforcement. More materially, the B direct-edge and external-package protections are incomplete as described in M1. The hot-path protection promised by the replacement is incomplete as described in M2.

L9's processor registries/tests and SB13's V pooled-call/no-threads extensions are owned by their named subsequent implementation rows; a green SB1 tooling run does not validate those future runtime paths.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
