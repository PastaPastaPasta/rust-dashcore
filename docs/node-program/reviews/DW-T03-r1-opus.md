# DW-T03 review r1 (Opus): GitHub Actions CI for dashwallet-desktop

**Verdict: CHANGES-REQUIRED.** The fixes are small. The workflows are sound and I found nothing exploitable. Three things block the merge:

- M1, M2: two downloaded installers that float to "latest" and are never hash-checked.
- M3: the branch has never been built against current `main`.

Squash the history (§3) while rebasing.

Reviewed: `dw/t-03-ci` @ `66e251c` (27 commits over `77b3336`, the merge-base with `origin/main`; `94026e2` is the first branch commit). Clean worktree, read-only `gh`. Last green run: [37818742792](https://github.com/PastaPastaPasta/dashwallet-desktop/actions/runs/37818742792).

Tools:
- `actionlint` 1.7.12: clean.
- `zizmor` (offline, regular and auditor persona): only `artipacked` (L5) and `template-injection` on `matrix.os` (L6).

## 1. Verified OK (no action)

- **Token and triggers.**
  - All four workflows set top-level `permissions: contents: read`, and no job widens it.
  - The repo default workflow permission is also `read`.
  - No workflow uses `pull_request_target`, `workflow_run` or `issue_comment`. Fork PRs get `pull_request` with a read-only token.
  - The repo is **public**. Fork approval is required for first-time contributors only.
- **Secrets.** There are no `secrets.*` references and no `GITHUB_TOKEN` use beyond checkout.
- **Script injection.**
  - No `${{ github.event.* }}`, `github.head_ref` or branch name is expanded inside `run:`.
  - Free-form dispatch inputs (`gate.yml` `args`, `tauri-selftest.yml` `selftest_args`, `theme`) reach the shell only through `env:`.
  - Those inputs are split with `read -ra`, which does no globbing and no `eval`.
  - `GITHUB_REF_NAME` is used as a shell variable, not as an expression.
- **Action pins.** I checked all 8 distinct pins with `gh api repos/<o>/<r>/git/ref/tags/<tag>`, dereferencing annotated tags. Every SHA equals its commented tag:
  - checkout v7.0.1 `3d3c42e5…`
  - cache v6.1.0 `55cc8345…`
  - upload-artifact v7.0.2 `cf430e03…`
  - setup-node v7.1.0 `949feb24…`
  - Swatinem/rust-cache v2.9.2 `6323deb1…`
  - astral-sh/setup-uv v10.2.0 `c18668ad…`
  - compnerd/gha-setup-swift v0.5.0 `bbae8ce8…`
  - pnpm/action-setup v6.1.0 `ea17c68d…`
- **Cache poisoning.**
  - PR and fork runs can only save caches under `refs/pull/N/merge`. Pushes to `dw/**` save under their own branch. Neither is visible to `main`, and `main` restores only its own caches (including through the `swiftpm-*-` restore-keys).
  - The Dash Core tarball cache (`nightly.yml:63-66`) is re-hashed against the pinned SHA-256 on every use (`regtest/scripts/fetch-dashcore.sh:64-74`), so a bad cache entry fails closed.
  - swiftly verifies the toolchain's GPG signature ("Verifying toolchain signature..." in the run log), and rustup checks its component hashes.
- **Blocking jobs really block.**
  - `continue-on-error` appears only on the documented known gaps: the `windows-swift` job (`ci.yml:271`) and the macOS SwiftCrossUI launch step (`ci.yml:202`). Known gaps 1 and 3 in `docs/ci.md` cover them.
  - Every `|| true` in a blocking path is diagnostics or cleanup. The lldb capture at `ci.yml:206-209` is followed by an explicit `exit 1`.
  - All `run:` steps use bash `-eo pipefail`, set by `defaults.run.shell: bash` in every workflow.
- **Schedule.** `17 3 * * *` is valid (03:17 UTC daily, off the :00 peak).
- **gate.yml / tauri-selftest.yml dispatch on arbitrary refs.**
  - Dispatch needs write access.
  - The job runs the chosen ref's own workflow file and code, which a writer could push anyway.
  - The token is read-only and there are no secrets.
  - `candidate` and `os` are `choice` inputs, which GitHub validates on dispatch.
  - Running any ref therefore cannot reach beyond the ephemeral runner. A dispatch on `main` would save into `main`'s cache scope, but only a writer can do that.
- **Matrix and cache keys.**
  - rust-cache adds the job id and `rustc -vV` (host triple), so the gate/selftest OS matrix gets separate entries.
  - The SwiftPM keys hash `Package.resolved`.
  - The nightly Dash Core key hashes the fetch script, which holds the version and SHA-256 values.
- **Artifacts.** Screenshots and app logs come from `--demo`/`--fixture` data. The rest is regtest logs (regtest keys only), runner crash reports and JUnit XML. No step dumps the environment, and no uploaded path includes `.git`.
- **`build-core.sh`.**
  - The colour strip (`scripts/build-core.sh:118-121`) handles the `-lc<ESC>[0m` failure recorded in the memory note.
  - The MSVC import-library merge picks the target-arch crate (`*_x86_64_msvc-*`).
  - The module map skips merged libraries and `/defaultlib:` entries.
  - The vdir is wiped first, so the `[[ -f "$vdir/$lib_name" ]] ||` fallback is correct.
- **Rust changes** (`dw-compat/src/walletdat.rs`, `dw-engine/src/backup.rs`, `dw-testutil/src/lib.rs`): correct and minimal. The UNC branch of `windows_uri_path` is only unit-tested as a string transform; no real `\\server\share` open was tested. That's fine for now.

## 2. Findings

### Medium (required before merge)

**M1. swiftly is fetched as "latest" with no hash, and its generated script runs as root.** `ci/github/linux-swiftly.sh:13,18-19`

- `curl …/swiftly/linux/swiftly-$(uname -m).tar.gz` is the floating latest swiftly. Nothing checks its hash or signature.
- That binary then writes `post-install.sh`, which runs with `sudo bash`.
- The toolchain it installs is GPG-verified, but the installer itself isn't. It's the one executable in CI that is both unpinned and unverified. It runs in `ci.yml` swift-linux and in `gate.yml` for swiftcrossui on Linux.

*Fix:*
- Download a versioned build: `https://download.swift.org/swiftly/linux/swiftly-1.1.1-$(uname -m).tar.gz` (exists; `.sig` alongside).
- Check it against a per-arch SHA-256 kept in the script (`sha256sum -c`). Alternatively, `gpg --verify` the `.sig` against swift.org's published keys.
- Take the swiftly version as a second argument or a constant next to the Swift version.

**M2. Windows App Runtime: floating "latest" installer, no hash, no signature check.** `ci.yml:318-326`

- `https://aka.ms/windowsappsdk/1.5/latest/windowsappruntimeinstall-x64.exe` currently redirects to `https://download.microsoft.com/download/4f5a8b87-f017-485d-8cf7-c16e82671dbe/WindowsAppRuntimeInstall-x64.exe`.
- The job runs whatever is behind that redirect.
- The job is non-blocking, so CI health isn't at risk. But the brief requires installers pinned by version and hash. A servicing bump would also silently change what the G-03 WinUIBackend measurement runs on.

*Fix:*
- Pin the resolved `download.microsoft.com` URL (name the 1.5.x build in a comment).
- Check it with `(Get-FileHash $installer -Algorithm SHA256).Hash -eq '<sha>'`.
- Also require `(Get-AuthenticodeSignature $installer).Status -eq 'Valid'` with a Microsoft signer before running it.

**M3. Never built against current `main`; rebase and get a green run first.**

- `origin/main` is 44 commits past the branch base `77b3336`: +8,549/-492 lines across 51 Rust files, including `dw-engine/src/backup.rs` +173. The Windows build, test and clippy jobs and the Linux/macOS Swift jobs have never compiled that code.
- `git merge-tree` shows no textual conflict.
- New Unix-only code I spot-checked is gated: `tests/umask_002.rs` is `#![cfg(unix)]`. Still, only a run can show whether the Windows clippy `-D warnings` and the Windows tests pass on the combined tree.

*Fix:* do the §3 rebuild on `origin/main`, push, and merge only after CI is green on that head. Expect `windows-swift` to stay red because of the G-03 stack overflow.

### Low

**L1. protoc zip has no hash check.** `ci/github/install-protoc.sh:18-19`

The version is pinned (29.3). protobuf publishes no checksum file.

*Fix:*
- Keep a `case` table of SHA-256 per asset (linux-x86_64, linux-aarch_64, osx-aarch_64, osx-x86_64, win64), computed once.
- Check with `sha256sum`, `shasum -a 256` or `certutil` before unzipping.
- `ci/linux/Dockerfile` has the same gap, but that predates this branch.

**L2. The Windows Swift installer isn't verified.** `compnerd/gha-setup-swift` (`ci.yml:292-295`, `gate.yml`)

The version is pinned (6.3.3-RELEASE), but the action downloads the `.exe` with `WebClient.DownloadFile` and checks neither a hash nor a signature (run log).

*Fix:* accept and document this in docs/ci.md, or download and verify the installer yourself and point the action at it. Low because swift.org over HTTPS and an exact version.

**L3. `dash_hash` installs from a mutable tag archive with no hash.** `nightly.yml:73`

*Fix:* use a commit-archive URL with a hash, `dash_hash @ https://github.com/dashpay/dash_hash/archive/<commit>.tar.gz#sha256=<…>`, or a requirements file under `--require-hashes`. The regtest image builds `dash_hash` too (`regtest/Dockerfile`); check that it's pinned the same way.

**L4. Unpinned tool versions.**

- `astral-sh/setup-uv` has no `version:`, so it installs the latest uv (`nightly.yml:62`).
- `brew install xcodegen` is unversioned (`ci.yml:186`).

*Fix:* add `with: version: "<x.y.z>"` to setup-uv. xcodegen is fine as it is; Homebrew bottles are hash-pinned by the formula.

**L5. Checkout leaves the token in `.git/config` while third-party code runs.** All 10 `actions/checkout` steps (zizmor `artipacked`)

- Checkout persists `GITHUB_TOKEN` in `.git/config`.
- `pnpm install` lifecycle scripts, uv/pip installs and gate scripts then run in that checkout.
- The token is read-only on a public repo, so the exposure is negligible.
- No step needs git auth after checkout.

*Fix:* add `with: persist-credentials: false` to every checkout.

**L6. `${{ matrix.os }}` expanded inside `run:`.** `tauri-selftest.yml:98,110,118` (zizmor `template-injection`)

This isn't exploitable: `inputs.os` is a validated `choice`. It is the only place the workflows break their own rule of passing inputs through `env:`.

*Fix:* use `$RUNNER_OS` (`macOS`/`Windows`), or add `MATRIX_OS: ${{ matrix.os }}` to the job `env` and use `$MATRIX_OS`.

**L7. "Runs on `main` are never cancelled" isn't quite true.** `ci.yml:30-32`, `docs/ci.md:15-16`

- With one group per ref, GitHub keeps at most one *pending* run per group.
- So on three quick pushes to `main`, the middle run is cancelled while queued, even with `cancel-in-progress: false`.

*Fix:* give each `main` run its own group, `group: ci-${{ github.ref == 'refs/heads/main' && github.sha || github.ref }}`. Alternatively, reword the doc to "a running CI on `main` is not cancelled; a queued one can be superseded".

**L8. The nightly can't fire yet and will be red from day one.** `nightly.yml:10-12`; docs gaps 2 and 7

- `schedule` runs only from the default branch, and the default branch is `m3/r2-governance`, which has no `nightly.yml`. **pasta needs to set the default branch to `main`** (admin).
- Once it does fire, `crossui-linux` is known-red: 4 AT-SPI checks failed in [37753227825](https://github.com/PastaPastaPasta/dashwallet-desktop/actions/runs/37753227825). A nightly that is red every day stops getting read.

*Fix:* mark `crossui-linux` `continue-on-error: true` until gap 7 is fixed, or say in docs/ci.md that a red nightly means "check the regtest job first".

**L9. docs/ci.md understates the Tauri report check.** `docs/ci.md:135-139` vs `tauri-selftest.yml:124-126`

The workflow also fails a checkpoint report (`.final == false`) and a report with no `security.navigation` probe (commit 417ffed). The doc doesn't say so.

*Fix:* add those two conditions to item 1.

**L10. gate.yml comment says Playwright browsers are installed.** `gate.yml:92`

Nothing installs them. `docs/ci.md:120-121` correctly says the gate script fetches them.

*Fix:* drop "Playwright browsers" from the comment.

**L11. Lint job's clippy has no `--locked`.** `ci.yml:66`

Every other cargo command uses it. The rust-linux job catches lockfile drift anyway, so this is only for consistency.

*Fix:* add `--locked`.

**L12. windows-swift runs `swift test` after a failed core bundle.** `ci.yml:296-310`

- If `build-core.sh` (`:296-297`) fails, `swift build (headless)` is skipped because it has no `if:`.
- `swift test` (`:305-306`, `if: !cancelled()`) still runs against nothing and adds a misleading second failure.

*Fix:* give the headless build an `id:` and gate the test on `steps.<id>.outcome == 'success'`.

### Info

- **I1. Duplicate runs.** A PR from a `dw/*` branch runs CI twice, about 2×150 runner-minutes on macOS and Windows. It's documented (`docs/ci.md:18-19`) and free on a public repo; acceptable.
- **I2. Floating runner images.** `macos-latest`, `windows-latest` and their default Xcode/MSVC float. That's the usual trade-off; `sw_vers`/`xcodebuild -version` are logged (`ci.yml:151-156`), which is enough for triage.
- **I3. Artifact retention.** Artifacts keep the 90-day default. You could add `retention-days: 14` on the screenshot uploads; there's nothing sensitive in them, so this is only about storage.
- **I4. Stale header comment.** `ci.yml:10-11` still says "The Swift app is not built on Windows yet". `dash-wallet.exe` now builds in the non-blocking job; it's the *launch* that fails (gap 1). Reword when touching the file.

## 3. Squash plan

The four dead ends and the fixup are mixed commits, so dropping them with `rebase -i` would lose code that has to stay:

| Commit | Kept in the final tree | Gone from the final tree |
|---|---|---|
| `6909173` | lldb backtrace (`ci.yml:204-209`) | lld link, `--cfg windows_raw_dylib` |
| `b8c0701` | largest-window capture, `DWD_SHOT_SETTLE` | explicit libc `-L` dirs |
| `1a0df8f` | move from `swift:6.3.3-noble` container to runner, deletion of `linux-swift-setup.sh` | `vm.max_map_count` step |
| `1bcdc5b` | nothing (net zero with `30f2004`) | `-lc` autolink diagnostics |
| `5bcdc6d` (fixup) | everything; it belongs inside `408a6fc` | — |

So rebuild the branch **by path** on `origin/main` from the `77b3336..66e251c` diff. Don't reorder the 27 commits. Use six topic commits, each self-contained, in dependency order. The Windows CI jobs need C, so C goes before D:

| # | Commit | Paths | Absorbs |
|---|---|---|---|
| A | `fix: compile the Rust workspace for Windows` | `rust/` (3 files) | 408a6fc, 5bcdc6d |
| B | `fix(build-core): strip colour codes from rustc's native-static-libs line` | `scripts/build-core.sh` (that hunk only) | 3438da8 |
| C | `build(core): build the Rust core bundle for x86_64-pc-windows-msvc` | rest of `scripts/build-core.sh` | 059b178, 30ebbed, f2ebbfa, aa522ea, build-core part of 417ffed |
| D | `ci: add GitHub Actions CI for Linux, macOS and Windows` | `.github/workflows/ci.yml`, `ci/` | 94026e2, 89915c6, 1cc7a9f, 33c5d2e, 2cb2349, c1b5c5a, the surviving parts of 6909173, b8c0701 and 1a0df8f, 1bcdc5b+30f2004 (net zero), ci.yml part of ff67d99, e17221c, cfe3d6b |
| E | `ci: add the nightly, UI-gate and Tauri selftest workflows` | `nightly.yml`, `gate.yml`, `tauri-selftest.yml` | 46d867c, 4552b75, rest of ff67d99 and 417ffed |
| F | `docs: describe the CI workflows, artifacts and known gaps` | `docs/ci.md`, `README.md` | c02e245, a4d244f, 66e251c, doc parts of the rest |

B stays a separate commit because it's a real bug fix to a script developers run locally, and the `-lc` story is worth a bisectable commit. Fold the M1/M2 and Low fixes into D, E and F, or add them as one `ci: pin and verify downloaded installers; review fixes` commit on top.

I ran this recipe in a scratch worktree on `origin/main`. All six patches applied cleanly, and the resulting tree is **byte-identical** to `git merge-tree --write-tree origin/main 66e251c`. (That trial commit was made unsigned and thrown away; for the real one, commit normally so the agent key signs it.)

```sh
git switch -c dw/t-03-ci-squash origin/main
B=77b3336 T=66e251c
mk() { msg=$1; shift; git diff $B $T -- "$@" | git apply --3way --index && git commit -m "$msg"; }
mk "fix: compile the Rust workspace for Windows" rust/
git cherry-pick 3438da8          # B (applies cleanly on main; keep its message)
mk "build(core): build the Rust core bundle for x86_64-pc-windows-msvc" scripts/build-core.sh
mk "ci: add GitHub Actions CI for Linux, macOS and Windows" .github/workflows/ci.yml ci/
mk "ci: add the nightly, UI-gate and Tauri selftest workflows" \
   .github/workflows/nightly.yml .github/workflows/gate.yml .github/workflows/tauri-selftest.yml
mk "docs: describe the CI workflows, artifacts and known gaps" docs/ci.md README.md
git diff "$(git merge-tree --write-tree origin/main $T)" HEAD --stat   # must be empty
# then apply the M1/M2/L fixes, force-push to dw/t-03-ci (--force-with-lease=dw/t-03-ci:66e251c),
# wait for green CI on the new head (M3), and merge.
```

Write real commit bodies when committing. D's body should carry the debugging history so it isn't lost: the `-lc<ESC>[0m` hunt and the `vm.max_map_count`, lld and container theories that were ruled out. Point to `docs/ci.md` for details.

## 4. Docs vs workflows

`docs/ci.md` matches the workflows except for L7 (cancellation wording), L9 (Tauri report conditions), L10 (the gate.yml comment, the code-side counterpart) and I4.

Things I checked that do match:
- jobs, runners and steps per table row
- `--exclude dw-app` on Windows
- the free-disk scope
- the `DWD_SHOT_SETTLE` value
- artifact names
- the gate env contract (`GATE_CANDIDATE`/`GATE_OS`/`GATE_OUT`, args split without globbing)
- the known-gap run links (37769597095, 37796510022, 37804634973, 37811923871, 37753227825)
- the "registered on GitHub" claim (all four workflows are active)
- the "no secrets, read-only token" claim
