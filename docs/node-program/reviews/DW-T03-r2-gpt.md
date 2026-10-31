APPROVE-WITH-NITS

1. **M1 — CLOSED: swiftly is versioned and hash-checked before execution.**

   `ci/github/linux-swiftly.sh:13–27` pins swiftly 1.1.1, rejects unsupported architectures, checks with `sha256sum -c`, and only then extracts and executes the archive. The generated privileged post-install script runs after verification (`:28–32`). Independent downloads reproduce both pins:

   ```text
   https://download.swift.org/swiftly/linux/swiftly-1.1.1-x86_64.tar.gz
   SHA256 dc5f94308b33455530f4150b412527596a33c8525a7d59b025b598520e92d121
   https://download.swift.org/swiftly/linux/swiftly-1.1.1-aarch64.tar.gz
   SHA256 e664899868b2dea9a1e7a7fbad0521c24aceeca14bbcbb22ba5cda1f62c37ed4
   ```

   Both adjacent `.sig` files return GPG `GOODSIG` and `VALIDSIG` for **Swift Automatic Signing Key #4**, fingerprint **E813 C892 820A 6FA1 3755 B268 F167 DF1A CF9C E069**. I imported the key independently from `https://swift.org/keys/all-keys.asc` into an isolated keyring and cross-checked its full fingerprint against `https://www.swift.org/install/linux/tarball/`. This verifies identity beyond the key's display name. GPG's unknown-owner-trust warning is expected in the fresh keyring.

   CI 37828155660 logs `swiftly.tar.gz: OK` at 18:59:09 UTC, `Verifying toolchain signature...` at 18:59:28, and a successful Swift install step. **The installer hash is checked at runtime; the installer signature is checked when establishing/reviewing the pin, not on every CI run.** This satisfies r1's required fix, which did not require both runtime checks.

2. **M2 — CLOSED: Windows App Runtime is pinned and verified.**

   `.github/workflows/ci.yml:334–354` names the installed package version, uses the resolved official URL, compares `Get-FileHash` with the pin, requires Authenticode `Valid` and signer subject beginning `CN=Microsoft Corporation,`, then checks the installer's exit code. Verification failures throw before execution.

   ```text
   https://download.microsoft.com/download/4f5a8b87-f017-485d-8cf7-c16e82671dbe/WindowsAppRuntimeInstall-x64.exe
   SHA256 7418dc8dc18cfee1cdc1b087a326ea6fd66e2e3a5e1c76d49fa948737a83a45e
   ```

   The independently downloaded 64,195,640-byte file matches. `osslsigncode 2.13` also independently verifies its PE digest, Microsoft Corporation signer, certificate chains, timestamp signature and CRLs, using Microsoft's published Root Certificate Authority 2011 and 2010 certificates:

   ```text
   https://www.microsoft.com/pki/certs/MicRooCerAut2011_2011_03_22.crt
   https://www.microsoft.com/pki/certs/MicRooCerAut_2010-06-23.crt
   ```

   The authenticated timestamp is 2025-01-08 18:47:29 UTC, within the now-expired signing certificate's validity period. No verification bypass was used. Ubuntu's general CA bundle initially lacked these Microsoft roots; supplying them completed verification.

   The Windows runner logs `verified: SHA-256 7418DC…A45E, signed by CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US` at 19:50:48 UTC, then `Microsoft.WindowsAppRuntime.1.5 5001.373.1736.0` at 19:50:58. Both guards actually passed in CI.

3. **M3 — CLOSED: rebuilt on main and validated at the exact head.**

   The merge-base with `origin/main` is `ffb4f99567b19211c6c26f82f86a6e011cdde18d`; exactly seven commits follow it. GitHub's commit API reports `verified: true`, reason `valid`, for all seven signatures.

   `gh run view` confirms both supplied runs use **5a6e914216b08ea205723d171102671533d4616e**. [CI 37828155660](https://github.com/PastaPastaPasta/dashwallet-desktop/actions/runs/37828155660) succeeds: lint, Linux Rust, Linux Swift/UI T0, macOS, and **Windows (engine)** all pass. Windows executes workspace build, workspace tests excluding the documented `dw-app` gap, and clippy with `--all-targets --locked -- -D warnings`. The migration immutability test passes in Linux, Windows and macOS logs. Linux Swift reports 547 passing tests; macOS Swift reports 656.

   Green overall does not mean every diagnostic job passed. Windows Swift has the documented three portability failures out of 529 tests and app exit `-1073741571` (`0xC00000FD`, stack overflow). Its core bundle, headless build, WinUI app build and runtime installation pass. [Nightly 37835379322](https://github.com/PastaPastaPasta/dashwallet-desktop/actions/runs/37835379322) is a successful **manual dispatch**, with every regtest suite passing and the non-blocking AT-SPI job failing three hard checks. These documented exceptions do not invalidate M3's blocking-job evidence.

4. **L1 — PARTIAL: workflow protoc downloads are checked; the pre-existing Docker gap remains.**

   `ci/github/install-protoc.sh:18–38` supplies all five hashes, rejects unknown version/asset combinations, and checks before extraction, with macOS `shasum` fallback and Windows path handling. Independent official v29.3 downloads match:

   ```text
   linux-x86_64   3e866620c5be27664f3d2fa2d656b5f3e09b5152b42f1bedbf427b333e90021a
   linux-aarch_64 6427349140e01f06e049e707a58709a4f221ae73ab9a0425bc4a00c8d0e1ab32
   osx-aarch_64   2b8a3403cd097f95f3ba656e14b76c732b6b26d7f183330b11e36ef2bc028765
   osx-x86_64     9a788036d8f9854f7b03c305df4777cf0e54e5b081e25bf15252da87e0e90875
   win64         57ea59e9f551ad8d71ffaa9b5cfbe0ca1f4e720972a1db7ec2d12ab44bff9383
   ```

   URL pattern: `https://github.com/protocolbuffers/protobuf/releases/download/v29.3/protoc-29.3-<asset>.zip`. Install steps pass on all three CI operating systems and nightly regtest. However, `ci/linux/Dockerfile:10–12` still downloads/unzips protoc without hashing. This predates T-03; it does not reopen a medium blocker. Reuse the checked installer or add Linux hashes there in follow-up, and qualify the broad download claim in `docs/ci.md:21`.

5. **L2 — CLOSED by r1's explicitly permitted documentation alternative.**

   `docs/ci.md:31` expressly accepts that the versioned Windows Swift installer is neither hash- nor signature-checked. The pinned action's actual `action.yml:217–231` confirms `WebClient.DownloadFile` without verification. `ci.yml:307–310` and `gate.yml:114–118` specify 6.3.3 exactly; CI installs it successfully. This closes the request to fix **or document**, while leaving an accepted supply-chain exception.

6. **L3 — CLOSED: dash_hash is content-pinned in both environments.**

   `nightly.yml:77–81` checks commit archive `7af387315d9064ec3a751545fee0524d997c8867` before installation. `regtest/Dockerfile:12–20` already checks the tag archive before building its wheel. Dereferencing the annotated 1.4.0 tag via `gh api` resolves to the same commit. Independent downloads reproduce:

   ```text
   commit archive 7153d3341e3a6b1a2a1365dbdf9ff6315fa7c6c67b60506e91ffd93383a601d5
   tag archive    2490feb0bb186a03956a140bb077d196fa4e2b0c22c5df0517c5f1bbd6cc7e5a
   ```

   Both log `OK` in nightly and install 1.4.0. Different archive root names explain the different hashes. Although the Docker tag reference remains mutable, its fixed content hash makes unexpected movement fail closed.

7. **L4 — CLOSED: uv pinned; xcodegen remains an accepted float.**

   `nightly.yml:64–66` pins uv 0.12.23; the log says `Successfully installed uv version 0.12.23`. `ci.yml:197` retains installed xcodegen/Homebrew's current formula, which r1 explicitly allowed. The macOS build passes; `docs/ci.md:30` accurately describes the uv pin.

8. **L5 — CLOSED: all checkouts disable credential persistence.**

   Parsed YAML confirms all **10** checkout steps set boolean `persist-credentials: false`, including `ci.yml:56–58`, `nightly.yml:46–48`, `gate.yml:66–68` and `tauri-selftest.yml:67–69`. CI/nightly checkouts pass, and zizmor no longer reports `artipacked`. Gate/Tauri closure here is source evidence; those workflows are not exercised by the supplied runs.

9. **L6 — CLOSED: script inputs go through the environment.**

   `tauri-selftest.yml:56–58` defines `MATRIX_OS`; `:102,114,122` use it. No `run:` body in any of the four workflows contains `${{ ... }}`. Free-form dispatch arguments enter `env`, are split with `read -ra`, and are passed as quoted array elements; no `eval` or shell re-parsing occurs. Matrix selectors use validated choice inputs. Source/static-analysis closure; Tauri execution remains untested.

10. **L7 — PARTIAL: distinct main commits are protected; wording overstates uniqueness.**

    `ci.yml:33–35` implements r1's recommended SHA-based main group and disables cancellation there. Distinct main pushes no longer share a pending slot. Two manual runs of **the same main commit** still share a group, however; a later pending run can supersede an earlier pending one. Thus `docs/ci.md:16–17` and `ci.yml:31–32` overclaim that every main run has its own group. Say “each distinct main commit has its own group,” or use `github.run_id` in main's group for the literal every-run guarantee. This is a low documentation edge case; no supplied run tests main concurrency.

11. **L8 — PARTIAL: nightly failure handling fixed; schedule still needs an admin change.**

    `nightly.yml:129–133` makes AT-SPI non-blocking; `docs/ci.md:100–104,225–231` explains the status and asks to restore blocking behavior when reliable. Current nightly regtest passes; AT-SPI fails onboarding Overview and two transaction-detail checks. The historical four-failure account is explicitly attributed to an earlier run and need not equal today's three.

    The repository API still returns default branch **`m3/r2-governance`**. The daily schedule is therefore not operational, as `docs/ci.md:193–196` warns. The supplied nightly is `workflow_dispatch`, not a scheduled-run proof. Pasta still needs to switch the default branch to main once the workflows are there; documenting that dependency does not complete it.

12. **L9 — CLOSED: selftest report conditions are documented.**

    `docs/ci.md:148–153` now includes checkpoint `final: false`, missing `security.navigation`, failed checks and `NOT REFUSED` probes, matching `tauri-selftest.yml:118–130`. The predicate deliberately tests `.final != false`, not `.final == true`, consistent with the doc's explicit condition. End-to-end execution remains untested and disclosed in known gap 8.

13. **L10 — CLOSED: browser setup comment corrected.**

    `gate.yml:94` now says the gate script fetches its own browsers, matching `docs/ci.md:133–134`. The workflow installs Node/pnpm and packages, and does not claim to install Playwright browsers. Source-only closure; no gate run is supplied.

14. **L11 — CLOSED: lint clippy uses --locked.**

    `ci.yml:71` includes `--locked`; the current lint job passes that exact command. The shortened documentation table omits this flag, but actual lockfile enforcement is correct.

15. **L12 — CLOSED: Windows Swift tests require a successful headless build.**

    `ci.yml:313–322` assigns `id: headless` and checks `!cancelled() && steps.headless.outcome == 'success'`. A failed or skipped build cannot launch the tests. Current CI passes the build, so tests correctly run and expose the three documented issues. Failure/skip handling was checked in source; no deliberately failed build was dispatched.

16. **Security recheck — permissions, injection, credentials and cache boundaries pass.**

    All four workflows specify only `contents: read`, with no job override. Repository settings report default workflow permission `read` and `can_approve_pull_request_reviews: false`. There are no workflow secret references, privileged `pull_request_target`/`workflow_run`/`issue_comment` triggers, or event/branch expressions embedded in scripts. `GITHUB_REF_NAME` is shell data. No dispatch input is evaluated as code. All checkouts disable persisted credentials.

    Cache keys separate SwiftPM by OS/resolved dependencies, Rust by job/toolchain/dependencies, gate/Tauri by additional keys, and Dash Core by OS/fetch-script content. GitHub cache scope prevents fork PR merge refs and `dw/**` pushes from planting entries consumed by main. More precisely than r1, a branch can also restore the **default branch's** caches: currently `m3/r2-governance`, later main if the setting changes. No privileged PR path can populate that scope. Dispatch on trusted refs requires repository write access. Dash Core tarballs are re-hashed before extraction on every use (`regtest/scripts/fetch-dashcore.sh:64–74`; nightly logs the verified hash). Swift installer caching defaults to false. No new cache-poisoning path found.

17. **Direct action pins pass; NEW LOW: nested Swift-action references remain tag-based.**

    All **36 direct `uses:` references** contain full 40-hex SHAs. Independent `gh api` tag resolution, including annotated-tag dereferencing, confirms all eight match their comments:

    ```text
    actions/checkout v7.0.1         3d3c42e5aac5ba805825da76410c181273ba90b1
    actions/cache v6.1.0            55cc8345863c7cc4c66a329aec7e433d2d1c52a9
    actions/upload-artifact v7.0.2  cf430e030ddbb5b0abf93d22962f4752f3646cd9
    actions/setup-node v7.1.0       949feb2413d6458794dcd2491c4babbbce0c15c1
    Swatinem/rust-cache v2.9.2      6323deb102c322ba6fcbdcafc7e3dddab59af2b6
    astral-sh/setup-uv v10.2.0      c18668ad3cf93ea998bef934396af7bb5c839dc7
    compnerd/gha-setup-swift v0.5.0 bbae8ce86bac5a3449f3992b3d88bc4ca25f2057
    pnpm/action-setup v6.1.0        ea17c68df8912ef543352723c149a84f56e3d413
    ```

    The Swift action is composite. Its [pinned action.yml](https://github.com/compnerd/gha-setup-swift/blob/bbae8ce86bac5a3449f3992b3d88bc4ca25f2057/action.yml#L190) uses `actions/cache/restore@v6`, `actions/cache/save@v6` and `actions/upload-artifact@v4` (`:190,252,303`). The Windows setup log actually downloads the nested cache tag as `55cc8345…` and artifact tag as `ea165f8d65b6e75b540449e92b4886f43607fa02`. Cache steps default to disabled; nested artifact upload is an installer-failure path. These official-action tag references are a low residual supply-chain exception, but **a transitive “every action is SHA-pinned” claim is false**.

    Scope `docs/ci.md:15` to direct workflow actions and document the exception, or use a reviewed vendored/forked wrapper with full nested pins. A strict policy extending to transitive actions requires the latter before claiming full compliance. The action also contains mutable-main SDK module downloads, but their option defaults to false and is not enabled here.

18. **0efc310 — correct and minimal test-only fix.**

    The diff changes only `rust/crates/dw-appdb/src/migration_tests.rs:170–177`: make the vector mutable and sort before comparing the unchanged `(version, name, checksum)` tuples. Tuple ordering compares version first, so `sort_unstable()` correctly orders uniquely versioned migrations. Missing, extra, duplicate, renamed or changed migrations still fail the exact expected-vector assertion. Production code and migration files are untouched.

    Refinery 0.9.2's `find_migration_files` uses unsorted `WalkDir` enumeration; its embedding macro preserves that order. Production `verify_migrations` independently sorts before applying migrations. The old test confused enumeration order with immutability; the fix removes that dependency without weakening the invariant. Existing fresh-database, upgrade, re-run and schema-equivalence tests still cover execution. `cargo test -p dw-appdb --locked` built locally in the fresh worktree and passed **26 tests**. The immutability test also passes in all three platform CI logs.

19. **Docs/workflow consistency — mostly accurate; low corrections remain.**

    Triggers, runners, job composition, exclusions, artifact names, permissions, checkout settings, installer guards, Swift suite selection, gate argument contract, selftest conditions, non-blocking jobs and default-branch dependency match. The runtime package version matches the install log. All four workflows are active according to GitHub's API.

    Beyond concurrency and pinning qualifications above, `docs/ci.md:165–166` still says neither UI is built on Windows and WinUIBackend has never been built, contradicting `ci.yml:327–331`, its own later paragraph, and the successful current app build. `ci.yml:276–279` and `scripts/build-core.sh:57` retain similar stale wording. Say that WinUI builds but launch fails. Introduce the Windows Swift verification exception and existing Docker protoc gap alongside the doc's blanket verification claim. Add `--locked` to shortened table commands for exact parity. These are nits, not blocking-job defects.

20. **Round-1 informational items.**

    **I1 — CLOSED/accepted:** duplicate push/PR runs remain documented (`docs/ci.md:18–19`). **I2 — CLOSED/accepted:** hosted runner/Xcode/MSVC versions float; macOS versions are logged (`ci.yml:162–167`). **I3 — CLOSED/accepted:** artifact retention remains the 90-day default; shortening it is optional. **I4 — PARTIAL:** the top header now correctly describes Windows builds (`ci.yml:10–12`), but stale job/script/doc wording remains as detailed above.

21. **Validation, evidence and limits.**

    Reviewed on 2026-10-08 in fresh detached worktree `/work/scratch/dw-t03-closure/wt`, at the requested head, against r1 and the real `ffb4f99..HEAD` diff. `TMPDIR=/dev/shm/dw-t03-closure-tmp` was mode 700 and removed after review. GitHub access used read-only `gh run view` and `gh api`; no push, merge, dispatch or GitHub message was made. No commits or signing-configuration changes were made.

    Local checks: appdb build/tests (26 pass), `cargo fmt --all --check`, `bash -n` for CI shell helpers/build-core, `git diff --check`, actionlint 1.7.12 (clean), zizmor 1.22.0 offline (no regular findings). Auditor persona reports four low advisories: two computed runner mappings and absent concurrency limits in the manual workflows; these intentional structures/resource choices are not injection vulnerabilities. Parsed YAML checks cover all direct action pins, checkouts, permissions and run bodies. Independent hash, GPG, official fingerprint, Authenticode and action-tag checks are detailed above. Logs and verification records are retained under `/work/scratch/dw-t03-closure/`.

    Windows/macOS execution evidence comes from the exact-head CI logs; no new jobs were run locally on this Linux host. Gate/Tauri end-to-end execution and an actual scheduled nightly are not established by the supplied runs; their documented limitations remain explicit.

Summary: M1–M3 are closed. Nine low requests are closed, including L2's permitted documentation alternative; L1, L7 and L8 remain partial, with no new medium/high blocker. The migration-order fix is correct and minimal. T-03 is suitable to merge with the documentation and transitive-action-pin nits; enabling nightly scheduling still requires pasta to change the default branch to main.
