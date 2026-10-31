# D1 round 1 — independent review

**Verdict: CHANGES-REQUIRED**

Reviewed `dw/agentbox-setup` at `44469adb504a6fea0093ce72251ff7c76badd07b` against `main` at `05161d92741246d8bb4cda08441997ce11367f07`, using `git diff main...HEAD` and all nine commits from `git log main..HEAD`. No branch files were edited and no commit was made.

## Findings

### 1. Major — the desktop app still creates a data root that wallet storage refuses under umask 002

**Locations:** `Sources/PlatformServicesDesktop/DesktopDataDirectory.swift:74`; callers at `Sources/DashWalletCross/DashWalletCrossApp.swift:248` and `:272`; equivalent macOS creation at `Sources/MacUI/App/MacAppComposition.swift:82`. The new assurance in `README.md:149` overlooks these product paths.

**Failure scenario:** A Linux user launches the live app with umask 002 and a missing default data root, or supplies a missing `--datadir`. `DesktopDataDirectory.prepare()` calls Foundation's `createDirectory` without permission attributes, creating the root as 0775. The engine subsequently creates `<root>/<network>` as 0700, but `create_private_dir()` deliberately leaves existing ancestors unchanged (`rust/crates/dw-engine/src/fsutil.rs:10`). The pinned platform-wallet-storage implementation checks every ancestor, so opening the wallet database fails on the app-created root. Newly created `.local/share` parents can cause the same refusal. An explicit missing macOS data root has the same unqualified Foundation creation path under a permissive umask; normal macOS umask 022 avoids this particular failure.

**Evidence:** Ran the actual `DesktopDataDirectory` enum in the existing Linux Swift Docker image, with a private 0700 parent and umask 002: `DesktopDataDirectory.prepare mode: 775`. Independently opened an equivalent root using the checkout's built `dwcli`: exit 1 with `database ancestor .../app-created-root is group/other-writable (mode 0775) and not sticky`. Its `regtest` child was already 0700. Changing only the root to 0700 made the same command succeed with exit 0.

This is a pre-existing product bug exposed by the branch's investigation, rather than a new production-code regression. It is a major finding under the requested review scope: the test helper and runner umask make tests pass while real desktop users still encounter the same refusal. The chooser's `FileSystemDataDirectoryInspector.create()` already requests 0700, but the default and explicit launch paths bypass that protection.

**Fix:** Create application-owned missing roots and missing parents with owner-only permissions in all desktop preparation paths, preferably through one consistent helper. Preserve existing user-owned directory permissions and report unsafe existing ancestors clearly; do not weaken storage validation or silently chmod arbitrary user directories. Add a subprocess regression exercising actual desktop preparation followed by engine network opening under umask 002, with existing safe ancestors and missing intermediate parents. Correct the README's assurance to describe the product behavior accurately.

### 2. Minor — relative `DWD_DEPS_DIR` produces an unusable artifact path

**Location:** `scripts/build-core.sh:68`, consumed at `:95`, `:108` and `:117`.

**Failure scenario:** With `CARGO_TARGET_DIR` unset, invoke `DWD_DEPS_DIR=../deps scripts/build-core.sh` from the repo root. The script exports `../deps/target`, then runs Cargo after changing directory to `rust/`; Cargo resolves the relative target against that directory. The outer script resolves `out_dir` against its original working directory and reports `libdashwallet_core.a not found` after compilation. Invocation from elsewhere has the same mismatch. The new variable is documented without an absolute-path restriction.

**Evidence:** Ran an unchanged copy of the build script in an isolated fixture with mocked Cargo and rustc. With `DWD_DEPS_DIR=deps`, Cargo produced `checkout/rust/deps/target/debug/libdashwallet_core.a`, while the script searched the caller's `deps/target/debug/libdashwallet_core.a` and exited 1.

**Fix:** Resolve the configured dependency directory to an absolute path with a documented reference directory before exporting the Cargo target, or reject relative values explicitly and document the requirement. Normalize an explicit relative `CARGO_TARGET_DIR` consistently as well; that equivalent issue predates this branch.

## Checks and review observations

- **macOS build path:** With the normal dev account (`HOME=/Users/pasta`), no overrides, and the existing deps directory, the default remains `/Users/pasta/workspace/dashwallet-desktop-deps/target`; absent that directory, it remains checkout-local `rust/target`. Explicit nonempty `CARGO_TARGET_DIR` still takes precedence. The new `DWD_DEPS_DIR` override and the use of the current user's home are intentional configurability changes, so behavior is equivalent for the established dev-Mac setup, rather than literally identical for every account/environment. Build and artifact generation logic otherwise stays unchanged.
- **macOS Docker behavior:** Mocked Darwin execution confirms the exact existing volume names, `dwd-linux-swift-target` and `dwd-linux-swiftpm`, and default jobs 8 even when host `CARGO_BUILD_JOBS=12`. `DWD_LINUX_JOBS` still overrides that count. The default Darwin branch requires neither hash command. Explicit volume tags are new opt-in behavior.
- **Linux isolation:** Mocked Linux execution with two checkout paths produces different 12-hex suffixes for both target and SwiftPM volumes. Default host Rust output stays under each checkout's `rust/target`. Explicit shared `DWD_DEPS_DIR`, `CARGO_TARGET_DIR`, or identical/empty volume tags can override isolation and must be chosen accordingly. Shared Cargo registry/git caches are separate from compilation output. Same-checkout concurrent runs still share scratch directories, as before; this change supplies checkout isolation, not run isolation.
- **Shell correctness:** Modified Bash scripts pass `bash -n`; the POSIX functional runner passes `sh -n`; `git diff --check main...HEAD` passes. Added expansions are quoted, the sourced helper uses caller-provided `set -euo pipefail`, and hash failures propagate. Both container-copy paths exclude `rust/target`, `.derived`, and `.claude`, preserving nested `.build`/`.swiftpm` exclusions. Scratch-root chmod is deliberate, and newly created functional-run directories inherit umask 077. Existing shared config/cache concurrency was not introduced by this branch.
- **Test-only boundary:** All consuming `dw-testutil` dependencies are under `[dev-dependencies]`; every changed Rust call is inside a test module or integration-test file. Normal Cargo dependency trees for `dw-ffi` and `dwcli` contain no `dw-testutil`. The helper requests 0700 at creation and does not change the process umask; the isolated helper test is the only process-umask mutation.
- **Documentation:** Build precedence, Linux volume names/jobs, tar exclusions, and corrected dwcli paths match the code. Historical regtest/CrossUI timing and diagnosis claims were read but their full runs were not independently repeated. The materially inaccurate permission assurance is covered by finding 1.

## Validation limits

No macOS host is available; macOS conclusions come from comparison with `main`, control-flow analysis, and mocked Darwin execution. The Linux Foundation behavior and storage refusal were executed locally. No full Docker Swift suite, live GUI, or regtest network suite was rerun for this review.

Rust validation: `umask 002; cargo test --locked -p dw-testutil -p dw-engine -p dw-console -p dwcli -p dw-ffi` **passed**, exit 0: 176 tests passed, none failed or ignored, and doc-tests passed. The helper test also passed when executed directly under umask 002. All build output used this checkout's `rust/target`; the branch remains clean.
