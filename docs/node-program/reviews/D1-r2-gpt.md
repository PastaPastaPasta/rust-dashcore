# D1 round 2 — independent review

**Verdict: CHANGES-REQUIRED**

Reviewed `dw/agentbox-setup` at `f699e3a38ea4b9c86539bae34ca3b66bb27fdc8f` against `main` at `1b080d3be91b1ce99bed976f44a8301a16038062`, including the five follow-up commits and the round-1 report. The checkout was clean before and after review. No branch source was modified and no commit was made. Reproduction programs and fixtures are outside the checkout, in `/tmp/d1-r2-evidence`.

## Findings

### 1. Major, blocking — symlink checks do not make permission tightening race-safe

**Locations:** `rust/crates/dw-engine/src/fsutil.rs:84` and `:90`; `Sources/PlatformServices/PrivateFileSystem.swift:82`, `:86` and `:93`. Newly created directories also receive path-based chmod at Rust `fsutil.rs:106` and Swift `PrivateFileSystem.swift:32`.

**Scenario:** A group-writable ancestor lets another group member replace the checked entry between metadata inspection and chmod. Both implementations inspect the original inode, then change permissions by pathname, following a replacement symlink. Rust uses neither `O_NOFOLLOW` nor descriptor-based permissions; Swift's `setAttributes(...ofItemAtPath:)` has the same problem. Skipping an already-present symlink only handles the non-concurrent case. This is reachable during upgrade from permissive layouts and through a user-chosen unsafe ancestor: the engine performs its chmods before storage eventually refuses that ancestor.

**Evidence:** Compiled the actual Rust `fsutil.rs` into an external harness (only the tracing macro was stubbed) and compiled the actual Swift `PrivateFileSystem.swift` in `dwd-linux-swift`. A deterministic `LD_PRELOAD` hook replaced an inspected 0775 directory with a symlink immediately before the underlying chmod. Both calls succeeded and changed the unrelated symlink target from 0775 to **0700**. The hook models a possible interleaving; exploitation requires write access to the containing directory. This is not a claimed arbitrary-user/root privilege escalation.

**Fix:** Open the item without following its final symlink, obtain its mode from that descriptor, and use `fchmod`/`File::set_permissions` on the same descriptor. Anchor traversal to validated directory descriptors (`openat` with suitable no-follow checks) where ancestors can be replaced too. Apply this to post-mkdir adjustment and owned-file creation, not just the existing-directory branch. Do not close the Rust file descriptor at `fsutil.rs:74` and then rediscover the file by path. Add a deterministic replacement-race regression; the current static symlink tests cannot catch this.

### 2. Major — a legacy 0775 `backups` parent still prevents migration backups

**Location:** `rust/crates/dw-engine/src/session.rs:238`; helper behavior at `rust/crates/dw-engine/src/fsutil.rs:56`.

**Scenario:** An existing installation has `<network>/backups` at 0775. Startup calls `create_owned_dir` only for `<network>/backups/auto`. `create_private_dir` deliberately preserves existing ancestors, and `tighten_owned` changes only its argument. Consequently `backups` stays 0775. When platform-wallet-storage needs a pre-migration backup, its ancestor check refuses `backups`, preventing the network from opening. Fresh installs pass because both missing components are created 0700. An app-level automatic wallet backup may tighten `backups` later, but it cannot repair a failed open.

**Evidence:** Built the current `dwcli`, opened a fresh root, set `regtest/backups` to 0775, and made the wallet database report a pending migration by removing its latest refinery history row. Reopen exited 1 with `database ancestor .../regtest/backups is group/other-writable (mode 0775) and not sticky`; the directory remained 0775. This fixture exercises the pre-migration backup gate, not the eventual validity of reapplying that migration. The pinned dependency's `ensure_dir` in `packages/rs-platform-wallet-storage/src/sqlite/persister.rs:2274` likewise tightens an existing `auto` directory but checks and rejects its unchanged parent.

**Fix:** Tighten the app-owned `backups` directory explicitly before creating/tightening `backups/auto` and before opening storage, using the race-safe helper from finding 1. Add an existing-layout test with 0775 `backups`, including a storage operation that actually needs its automatic backup. The current regression only makes the network directory and `app.sqlite` permissive.

### 3. Major — several exports remain readable by other users and writable by the group

**Locations:** `Sources/CrossUI/TransactionsScreen.swift:195`; `Sources/CrossUI/ToolScreens.swift:119`; `Sources/WalletFeatures/PSBT/PSBTViewModel.swift:247`; `rust/crates/dw-desktop/src/logs.rs:101`. Equivalent macOS save paths: `Sources/PlatformServicesMac/MacDesktop.swift:49` and `Sources/PlatformServicesMac/MacPanels.swift:68`.

**Scenario:** CSV exports disclose wallet addresses, contacts and transaction history; PSBT exports disclose transaction and wallet metadata; diagnostic ZIPs include app logs and data-root details. These files are written to user-selected destinations outside the 0700 network directory. Their creation sites still rely on the umask. A normal traversable Documents/export directory therefore exposes their contents to other local users and, under umask 002, allows group modification. The README's new blanket assertion that exports are 0600 is incorrect.

**Evidence:** Executed the same Foundation `.atomic` and `.withoutOverwriting` writes in the Linux Swift image under umask 002: both new files were **0664**. The Rust log exporter uses `create_new(true)` without `.mode(0o600)`, so it also creates 0664. Neither path subsequently restricts the file. The engine's secret/key export and `.dwbackup` writer is different and correctly uses mode 0600 at creation.

**Fix:** Give every wallet export owner-only permissions before its first content write. Use a descriptor opened with 0600 and exclusive creation where overwrite refusal is required; for atomic replacement, stage a private file and rename it. Preserve existing overwrite/cancel semantics. Add umask 002 assertions on CSV, PSBT and log-export destinations. macOS uses the same unqualified Foundation writes; their exact modes were not executed on a Mac.

### 4. Minor — settings are restricted only after their contents have been published

**Location:** `Sources/WalletRuntime/State/SettingsStore.swift:281` and `:282`.

**Scenario:** A user-chosen 0755 data root is accepted by storage (it is not group/other-writable), but others can traverse it. A new atomic settings write publishes a 0664 file under umask 002, then chmods it afterward. There is a readability window, and a crash or chmod failure leaves that mode indefinitely. For the usual 0700 default root the enclosing permissions protect the file, so this is less severe than exports to arbitrary destinations.

**Fix:** Set 0600 on the atomic staging descriptor before writing/renaming, and reuse that private atomic writer for settings. Checking only the final mode in a test misses the exposure window.

### 5. Minor, pre-existing gap — the vault still changes symlink targets and creates missing parents with the umask's mode

**Location:** `rust/crates/dw-vault/src/file.rs:199`, `:203` and `:217`.

**Scenario:** The vault retains its separate `create_dir_all` followed by pathname chmod. A pre-existing `vault` symlink causes chmod of the user's target directory, contradicting the stated symlink policy. Calling the public vault API on a path with missing parents creates those parents 0775 under umask 002 and restricts only the final directory. Its predictable `vault.dwv.tmp` is opened with create/truncate rather than exclusive/no-follow creation; `.mode(0600)` does not repair a pre-existing permissive temporary file or reject a symlink.

**Evidence:** An external harness calling the current public `Vault::open` and `Vault::create`, with `MemoryOsStore` and the test KDF, changed a symlink target directory from 0755 to 0700 without a race. Under umask 002, a fresh nested vault path produced parent modes **0775, 0775** and final vault mode **0700**. The engine normally creates a 0700 network parent first, which prevents the nested-parent gap from reproducing ordinary desktop startup failure; this is an existing API/security-policy gap, not a new branch regression.

**Fix:** Unify the vault with the safe directory policy, avoid chmod following symlinks, and use a private, exclusive staging file for atomic writes. Test the public API's missing-parent and existing-symlink behavior. Do not confuse this application vault with platform-wallet-storage's separate secrets-file implementation, which already uses no-follow descriptor operations in important paths.

## Creation-site audit and macOS reasoning

- **Normal Swift roots:** `DesktopDataDirectory.prepare`, `DesktopDataLocation.defaultDataRoot`, the chooser inspector, and the selected-root path in `MacAppComposition` now route through the private helper. Missing parents are requested 0700; default-root tightening logs, whereas an existing chosen directory is preserved. This fixes the ordinary round-1 Linux launch refusal.
- **Before the macOS root exists:** `Sources/PlatformServicesMac/MacDataLocation.swift:14` still calls `FileManager.url(...create: true)` to obtain/create Application Support, before the private helper runs. The API has no explicit 0700 attributes here. A pre-existing safe Application Support directory is fine; the mode Foundation chooses when that directory or `Library` is absent under umask 002 is **not established by the Linux tests**. Avoid that eager creation and let the private helper create missing components, or verify Apple's behavior with a macOS subprocess test. I do not claim a measured macOS 0775 result from this Linux host.
- **Engine:** Network creation, initial `app.sqlite` and session-marker creation request restrictive modes. SPV starts only after `spv` is prepared. Existing app-database sidecar permissions are not explicitly swept by the new helper, but a 0700 network parent protects those contents. Existing symlinked files remain problematic: `create_owned_file` opens through a symlink before its later skip, and app SQLite is not the wallet-storage symlink-rejection path. This is another reason to fix the descriptor handling in finding 1.
- **Backups/storage:** Engine wallet backups and key exports use `write_new_private` with exclusive 0600 creation. Pinned platform-wallet-storage database creation requests 0600; online backup staging uses a `NamedTempFile` and descriptor permissions before publication. Its backup machinery has independent ancestor validation; finding 2 is why protecting only the network root is insufficient. Some dependency chmod operations remain pathname-based; this review is not approval of all upstream filesystem race handling.
- **SPV:** The pinned dash-spv storage modules create internal directories with ordinary `create_dir_all` and write internal files/temporary files with ordinary create/open APIs. They therefore retain umask-derived modes. The 0700 network and SPV roots shield these public chain/peer caches in the normal app layout, as the README now explicitly describes. No additional startup refusal was found from those internal paths. The new regression does not start SPV.
- **Other temporaries:** Linux desktop-entry/mime configuration parents are now requested 0700 recursively. Their predictable `.tmp-dw` staging file remains an ordinary write, appropriate to public desktop configuration only under a trusted parent; it is not a private-wallet-file implementation. Unix instance fallback creation still does mkdir then pathname chmod; its lock file requests 0600, followed by ownership/privacy checks. These pre-existing transient/race limitations are not exercised by the new tests. Build-script `mktemp` files/directories are private. Test/demo-only creation was distinguished from production writes.

## Tests, portability and round-1 regressions

- `umask 002; cargo test --locked -p dw-engine -p dw-vault -p dw-desktop -p dw-testutil -p dw-console -p dwcli -p dw-ffi`: **passed**, 270 tests, 0 failed, 1 ignored; doc-tests passed. `cargo build --locked -p dwcli` also passed, using this checkout's own `rust/target`.
- Focused Docker Swift build/tests for `PrivateDataRootTests|SettingsStoreTests`: **passed**, 13 tests reported, with the child-only test skipped in the parent as intended. The umask parent test launched the child and required its `ok` result file. The existing Linux engine artifact's source stamp matches the current Rust sources.
- The Rust integration binary really sets umask 002 before engine creation and checks an ordinary temp-directory probe's group-write bit. It contains one test, so the process-global mutation is isolated. The Swift test starts its subprocess through `umask 002`, verifies `/proc/self/status` contains `Umask:\t0002`, opens the real engine, and requires a result file so an empty filter cannot pass. These are substantive regressions, not merely helper-mode tests. Their old-code negative runs were not independently repeated this round.
- Missing coverage: replacement races, legacy `backups` permissions with a required storage backup, export modes, public vault parent/symlink policy, settings publication mode, SPV start, and macOS pre-root creation. Passing existing tests does not establish these properties.
- Round-1 relative-path issue is **fixed**. Executed an unchanged build-script copy with mocked Cargo/rustc from a different caller directory, using relative `DWD_DEPS_DIR`, relative `CARGO_TARGET_DIR`, and a path containing spaces. All three completed and found the artifacts in the same absolute target directory Cargo used. Explicit target precedence and macOS defaults remain intact by code comparison.
- Per-checkout Linux Docker volume selection, Darwin's legacy shared volume names/default 8 jobs, override handling, tar exclusions and the test-only `dw-testutil` boundary remain intact. The five follow-up commits do not change their working behavior. Bash/POSIX syntax checks and `git diff --check main...HEAD` passed.

**Approval status: CHANGES-REQUIRED.** The ordinary Linux root failure and relative target failure are resolved, but safe tightening and complete storage/export permission handling are not.
