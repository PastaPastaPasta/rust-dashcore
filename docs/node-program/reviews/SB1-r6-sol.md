# SB1 layer gates — Sol r6 (narrow)

Reviewed **488d715ee3f5d571c0ba3d2a47c96108734c149a**, branch `node/sb1-layer-gates`: 9dd93d22e, 985fc0eb5 and 488d715ee on the r5-reviewed a88fedbd5. All three commits verify **G**. Binding inputs: the supplied brief, REVIEW-COMMON.md, Sol r5, PLAN §7 and **DEC-181**. DEC-181 replaces the source/closure proof with registered-product canaries and explicitly makes the source scan advisory.

**Result: CHANGES-REQUIRED.** R5-1/2/3 are closed under DEC-181's registered-product scope. Three new major findings prevent the allocator proof from satisfying that decision: crate-name equality cannot establish allocator ownership; staticlib examples escape registration; and native malloc replacement escapes both the Rust canary and the package-name deny lists. There are two minor findings: the blocking scan contradicts DEC-181, and the registry misses the benchmark driver's combined profiling configuration.

Experiments use a clean detached worktree at `/work/scratch/sb1-r6-sol/wt`, with independent minimal fixtures outside it. Every copied gate helper/config matches this exact head byte for byte. Commands run at nice 10 with `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER`, mode-0700 disk `TMPDIR=/work/scratch/sb1-r6-sol/tmp`, and private target directories. Android uses NDK 29.0.14206865/API 24 and the supplied memory recipe. Rust is 1.98.1; the host is x86_64-unknown-linux-gnu. No reviewed-branch edits, commits, pushes, GitHub actions, private-material reads or process-pattern kills occurred. This report is uncommitted.

## R5 closure

**R5-1 — closed for registered Rust products.** The exact defaults-off-plus-allocator `dash-chain` fixture was repeated with the allocator in `#[path="../allocator.rs"]` outside `src/`, and with the attribute assembled by the original `r5-alloc-macro` implementation using `["global", "allocator"].join("_")`. On both fixtures the current source/sample helper still returns **0**, as expected for an early warning with known gaps. The product canary returns **1** and names **dash_chain on both host and Android**. Actual selected release products build on both targets; adding the product's allocator fails on both with the asserted `dash_chain` conflict. A cdylib-only product selecting the path fixture is also rejected and names dash_chain on both targets. Thus closure comes from testing the actual registered configuration, including its macro expansion and external modules, rather than widening the scan.

Evidence: `probes.py`, `more.py`, `logs/r5-{path,macro}-{old-helper,canary,product-*,conflict-*}.log`, `logs/cdylib-path-canary.log`, and the corresponding fixtures under `/work/scratch/sb1-r6-sol/`.

**R5-2 — closed for registered Rust products.** The original offline registry packages `r5-facade 99.0.0` and `r5-hidden-allocator 99.0.0` were reused, including their directory-source checksums. The exact separated-workspace control retains the original failure premise: the shared root's host and Android all-features closures contain dash-chain and the registry facade, but no hidden allocator; its helper returns **0**. A separately registered product enables the facade's allocator feature. Its canary returns **1**, naming **r5_hidden_allocator on both targets**. Its ordinary release products build on both, and the product-owned allocator controls fail on both with the asserted conflict. A same-workspace product-feature unification control is also rejected on both targets. Consumers outside the registry are expressly outside DEC-181's proof.

Evidence: `more.py`, `logs/r5-exact-root-{closure,helper}.log`, `logs/r5-exact-product-{canary,product-*,conflict-*}.log`, `logs/r5-facade-{tree,canary}.log`, and `fixtures/r5-exact-{root,product}/`.

**R5-3 — closed.** The reviewed `allow_list()` rejects the original `bls@0.3.1` citation to the actual DEC-79 row on `refs/heads/node/integration`. Distinct-name, distinct-version and combined-prefix controls are rejected; an exact `blst@0.3.17` token is accepted. The old `[allocator] allow={}` key is rejected. Records require exactly package/version/dec. The qualified integration authority lookup remains unchanged. The real DEC-79 control uses the actual integration ref; synthetic prefix/exact parser controls inject only decision-row strings into the harness, without creating commits or changing any authority ref. These tests exercise the exact reviewed function, not a reimplementation.

Evidence: `probes.py`, `logs/allow-{real-authority,bls-real,name-prefix,version-prefix,both-prefix,exact,old-table}.log`.

## New findings

### R6-1 — major — a dependency sharing the product's rustc crate name is accepted as its own allocator

Locations: `contrib/node/check-products.py:138`, `:188`, `:214`.

The `own` verdict compares only the crate name printed by rustc with the product unit's `--crate-name`. The registry's collision protection covers only a bin and **its own package's** library. Different packages can legitimately use the same library/bin crate name; Cargo's metadata disambiguates the compiled units, but this diagnostic comparison discards that identity.

Reproduction: registered bin package **r6-product** has no allocator and declares `allocator="own"`. Its dependency is distinct package **r6-dep**, with `[lib] name="r6_product"` and an allocator. The dependency is imported as `alias-dep`; the product calls `alias_dep::touch()`. The exact gate returns **0** on host **and Android**, printing **"its own allocator only"**. The only allocator is in r6-dep. Without the `own` declaration, the canary rejects both targets and names r6_product. The real product builds, and adding an actual product allocator fails with `conflicts with global allocator in: r6_product`, confirming that the dependency owns the existing allocator.

This is a false pass through ordinary package/library naming, with no compiler-argument or source-scan trick. It generalizes the same-name ambiguity addressed by C1.

**Fix:** establish ownership using compiled-unit identity. If relying on rustc's name-only diagnostic, first fail closed when any other linked Rust unit in an `own` product's transitive closure has the product's actual crate name, including custom `[lib] name` values and dependency aliases. Apply that protection to libraries as well as bins. A comparison with the package's own lib alone is insufficient.

Evidence: `fixtures/own-name/`, `logs/own-name-{canary,real-build,unwaived-canary,real-conflict}.log`, `probes.py`, `more.py`.

### R6-2 — major — workspace staticlib examples escape the mandatory registry

Location: `contrib/node/check-products.py:110`.

The inventory intersects `target["kind"]` with cdylib/staticlib/bin and never checks `target["crate_types"]`. Cargo reports an example library as **kind=["example"], crate_types=["staticlib"]**. That is a workspace staticlib artifact, but the inventory does not see it.

Reproduction: workspace package r6-example has an ordinary library and `[[example]] name="shipped", crate-type=["staticlib"]`. The example installs a Rust allocator and exposes an exported C function. With **no product rows**, `check-products.py --registry` returns **0**. `cargo build --locked --release -p r6-example --example shipped` succeeds and produces `target/release/examples/libshipped.a`. No canary is required. A registry row cannot currently remedy this target, because resolution uses the same incomplete inventory and the dispatcher supports only `--lib`/`--bin`.

**Fix:** inventory artifact types from `crate_types` as well as Cargo target kind. Support an example target selector and its canary, or reject unsupported workspace cdylib/staticlib targets explicitly. Add staticlib and cdylib example controls. Any exclusion for other example/bench/test executables must distinguish ordinary harnesses from registered products explicitly; target kind alone cannot identify a shipped library artifact.

Evidence: `fixtures/example-static/`, `logs/example-static-{metadata,registry,build}.log`, `probes.py`. Ordinary unregistered lib cdylib/staticlib/bin targets and dangling rows correctly fail in `extras.py`; this finding concerns the missed Cargo target category.

### R6-3 — major — package-name bans do not close native allocator interposition

Locations: `contrib/node/shared-deny.toml:24`, `contrib/node/pure-deny.toml:52`, `contrib/node/check-products.py:204`; claimed fallback at `docs/node-program/BASE-SDK-FORK.md:65`.

The added bans correctly cover tikv-jemallocator, tikv-jemalloc-sys, jemallocator, jemalloc-sys, mimalloc and libmimalloc-sys **by package name**. They do not constrain native link inputs or allocator symbol definitions. Neither snmalloc-rs/snmalloc-sys nor rpmalloc/rpmalloc-sys is listed. A Rust wrapper installing `#[global_allocator]` remains detectable regardless of its package name; the gap is native override code without that Rust attribute. Merely extending the name list cannot cover renamed/vendored implementations.

Reproduction: ordinary dependency **r6-cmalloc** declares **links="mimalloc"**. Its build script compiles a local C archive defining `malloc`/`free`, links it through normal `rustc-link-lib`/`rustc-link-search`, and exposes a counter. The replacements forward to glibc after recording calls, so this is a deterministic interposition control rather than a replacement allocator algorithm. The registered product canary returns **0**. Both exact shared and pure cargo-deny configurations return **0** (`bans ok`), and the source/sample helper returns **0**. The actual release executable has defined **T malloc** and **T free** symbols and prints **override_calls=6 data=1**, proving that the linked override runs. There is no Rust allocator attribute to detect.

The `links` key is a Cargo collision identifier, not a ban by native-library identity. A differently named package with `links="mimalloc"` is accepted. A build script can also vendor the native override without that key. This confirms the claimed C fallback is incomplete for a registered product even though its Rust canary passes.

**Fix:** enforce allocator ownership for the actual product's native link inputs/symbol providers, with target-aware inspection or an audited native-input policy that fails closed on undeclared allocator replacements. Account for allocator-family functions and dynamic interposition, with explicit product-owned exceptions. Keep named crate bans as early checks; adding snmalloc/rpmalloc names alone does not close the vendored-build-script mechanism.

Evidence: `fixtures/native-malloc/`, `logs/native-malloc-{canary,shared-deny,pure-deny,helper,symbols,execute}.log`, `probes.py`. This native interposition experiment is host-only; no Android native-override run is claimed.

### R6-4 — minor — the blocking scan contradicts the binding advisory decision

Locations: `contrib/node/link-probe.py:10`, `:196`; `docs/node-program/BASE-SDK-FORK.md:64`; error propagation at `contrib/node/check-shared.sh:59` and the corresponding pure check.

**Ruling on item 4: blocking is not acceptable under DEC-181 as currently authorized.** Its item (4) says **"The source scan stays, as an advisory early warning, not the proof."** It does not leave advisory versus blocking open. The implementation and docs incorrectly claim that discretion exists.

Blocking is conservative for acceptance, but it creates failures beyond the registered-product contract. The scanner searches every line without parsing Rust: a library containing only `//! Documentation: global_allocator` and `pub fn touch(){}` fails the helper (**1**), while its registered product canary passes (**0**). A comment or an inactive allocator for an out-of-scope consumer should not require an allocator exception decision just to pass the product gate. The scan itself also retains known false negatives, as the R5-1 helper controls show; its useful role is warning independently of the proof.

**Fix:** report scan hits as warnings without setting the gate's failure status. Preserve hard failures for malformed/unauthorized exception records and for canary/build failures. If a stronger source policy is desired, it needs an authoritative decision superseding the advisory requirement, with the broader acceptance scope stated accurately.

Evidence: `fixtures/scan-comment/`, `logs/scan-comment-{canary,helper}.log`, `extras.py`, and DEC-181 in the supplied node-program decision log.

### R6-5 — minor — the existing benchmark driver supports a fourth feature configuration absent from the registry

Locations: `contrib/node/layers.toml:179`; `dash-spv-bench/run.sh:39`, `:274`.

The driver accepts both `--flame` and `--memory-snapshot` together and accumulates **cpu-profile heap-profile** in FEATURES, then builds with both enabled. Registry rows cover default, cpu-profile alone and heap-profile alone, but no combined row. The real argument parser was run in isolation with both flags: it exits **0**, produces both feature names, and there are **0** matching registry rows. The clean registry check still passes. No Docker workload or combined production build was run for this control.

**Fix:** register the combined configuration with `allocator="own"`, or make the driver reject the combination if it is unsupported. Completeness requires an inventory of supported build configurations in addition to target-name coverage. No current allocator violation in this combined production configuration is asserted.

Evidence: `logs/bench-combined-config.log` and the exact driver/registry source.

## Faithfulness and scope assessment

For the tested ordinary Rust products, the wrapper preserves the source path, features, cfgs, edition, target, dependency artifacts and Cargo build-script environment. It removes the nonce before the real product compile, then changes output-related options/crate type for the rlib rebuild, keeping the same resolved `--extern` artifacts. Saved `unit.json` files confirm Android target/linker/dependency paths. The path, assembled-macro, downstream-feature, bin-root and cdylib controls demonstrate that these do not escape the rebuild.

A build-script-generated allocator selected by the product is rejected and names dash_chain on **both targets**. A Rust dylib dependency is rejected and names r6_dylib on the host; its clean counterpart passes. Clean thin- and fat-LTO products pass, and a literal bin-root allocator is rejected. `cfg(test)` is absent from a normal registered product unit; a cfg(test)-only allocator does not apply to that artifact, and its normal product control passes. Actual test-harness units are separate configurations and are not automatically registered. These tests find no feature/cfg/artifact mismatch in the ordinary rebuild; they do not establish reproducibility for arbitrary stateful proc macros or sources that change between the two compiler invocations.

The clean metadata has **24 bin targets**, the two FFI cdylib/staticlib targets, **17 examples**, **15 benches**, **134 explicit test targets**, and one proc-macro. The **11 registry rows expand to 30 canaries**, matching the submitted count. The proc-macro is a compiler-side unit rather than a shipped allocator owner; its generated product attributes are covered by the macro control. Ordinary example/bench/test harnesses do not automatically become shipped products. However, a staticlib example falls directly under DEC-181's mandatory artifact rule, as R6-2 demonstrates. Nested/excluded workspaces remain outside the documented registry boundary; no newly discovered shipped product in one of them is asserted. Apple builds remain the previously deferred Mac/QA checks; host/Android evidence is not an iOS allocator proof.

A faithful rebuild does not cure R6-1's ownership comparison or R6-3's native-symbol blind spot. Nor can successful canaries prove an omitted artifact/configuration. The proof is therefore not complete as claimed.

## Verification and limits

Independent clean checks pass: registry, L1/L2/L3/L8, and L6's four FFI product/platform closure graphs, retaining their existing baseline entries. Logs: `clean-registry.log`, `clean-layers.log`, `clean-mobile.log`. All fixture canaries, ordinary-product builds and asserted allocator-conflict controls described above were run independently. `results.json`, `extra-results.json` and `more-results.json` retain exit statuses and commands. The initial macro fixture setup was corrected to exclude its independent proc-macro workspace before running its successful controls; that harness setup failure is not attributed to the implementation.

The implementer's 30/30 production canaries, full gate, 21 DEC-181 selftests, BL-20 failures and isolated 3/3 dashd reruns remain **reported evidence**, not independently reproduced here. No full production workspace build/test, full selftest (which creates commits), Android size remeasurement, Apple build or dashd run is claimed. No production crate allocator violation is asserted. Previously closed unrelated layer/mobile/authority findings remain closed.

The detached review worktree is clean at the exact reviewed head. Scratch fixtures, scripts, logs and private target directories remain under `/work/scratch/sb1-r6-sol/` for reproduction; recreate its removed empty `tmp/` directory with mode 0700 before rerunning. The report is unstaged and uncommitted. PLAN §7 handling was observed, with no private material accessed.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
