# D1 round 3 — focused verification

**Verdict: APPROVE**

Reviewed `dw/agentbox-setup` at `b64139fbe2b8ed2ca909e16d0e1eb9c13a4f5237`, focusing on commits `c98aa55..b64139f` and the blocker/two majors in `D1-r2-gpt.md`. **All three findings are closed. No blocker or major remains within this review's scope.** No tracked branch files were modified and no commit was made. Independent harnesses, mutations and logs are under `/work/scratch/d1-r3/`.

## 1. Permission-tightening TOCTOU — closed

**Rust:** `rust/crates/dw-fs/src/lib.rs:222–255, 308–353, 397–439`.
**Swift:** `Sources/PlatformServices/PrivateFileSystem.swift:216–318`.

Both implementations anchor traversal at `/`, open each directory relative to the retained parent descriptor, reject kernel symlink following with `O_DIRECTORY | O_NOFOLLOW`, and obtain ownership/mode from `fstat`. Permission changes address the opened inode. On Linux the walk uses `O_PATH`; before `fchmod`, it opens **`"."` relative to that descriptor**, with `O_RDONLY | O_DIRECTORY`. It never rediscovers the directory through its pathname or `/proc/self/fd`.

I independently confirmed that `fchmod` on an `O_PATH` fd fails with `EBADF`. After renaming the opened directory and replacing its old pathname with a symlink, reopening `"."` returned the same inode; chmod changed the moved original to 0700 and left the outside target at 0775. The actual Rust and Swift swap tests and additional ancestor-swap probes behaved the same way, including traversal of subsequent components through the retained directory.

Additional adversarial checks passed:

- Symlinks at every owned level (`net`, `backups`, `auto`) leave their targets' existing modes unchanged; symlink cycles terminate with an error. Chosen-part symlinks are resolved deliberately, with each target component walked under the same trust rules. Owned-part symlinks suppress tightening at and below the target.
- A deterministic swap **after successful `mkdirat`, before the subsequent open** left the unrelated symlink target unchanged in both implementations. The newly created original remained 0700. The mkdir/open boundary hook was added only to scratch copies; filesystem logic was unchanged. An `EEXIST` mkdir race retries the normal no-follow open without setting the `created` flag; subsequent descriptors and ownership/parent checks govern any mode change.
- `/proc/self/fd/N` links in the owned part did not change the pointed-to directory. In the chosen part, the textual target was resolved through the normal component walk before tightening an owned child. No magic-link-based reopen is used for chmod.
- Nonsticky group-writable ancestors prevented tightening. Sticky parents allowed tightening of current-user entries, while an owned entry changed to uid 65534 was refused and remained unchanged. Existing file hard links were left unchanged, and opened-file replacement affected the original fd, not the replacement target.

The separate `statat`/`readlinkat` symlink inspection does not reopen the old vulnerability: an untrusted-replaceable parent taints the walk and prevents chmod; under a trusted nonwritable/sticky parent an untrusted user cannot replace the trusted entry. Readlink targets are subsequently walked component by component.

I tested three scratch mutations: reopen by pathname, remove directory `O_NOFOLLOW`, and bypass the unsafe-item guard. Each failed the regressions (respectively 2, 4 and 3 failures), whereas the unmutated scratch harness passed all 16 tests. These checks exercise the defenses rather than only asserting final modes on an ordinary path.

## 2. Legacy 0775 backup parent — closed

`rust/crates/dw-engine/src/session.rs:236–253` computes storage's actual automatic-backup path, verifies it is below the network directory, and prepares the complete owned chain **before `SqlitePersister::open`**. The wrapper in `fsutil.rs` includes the network directory, so all of `<network>`, `backups` and `backups/auto` are tightened rather than only the final component.

The executed `data_roots_open_under_umask_002` regression creates a wallet, makes both backup directories 0775, reopens the engine, asserts both are 0700, removes the wallet through the authorized engine API, and requires an actual storage automatic-backup file. It passed. This exercises the backup ancestor gate that failed in round 2. The synthetic refinery-history migration fixture was not rerun; code ordering establishes that migration-time storage opening receives the same prepared chain.

## 3. Export umask modes — closed

The CSV callers now use `ExportFile.write`, PSBT uses `PrivateFileSystem.writeFile(replacing: false)`, and both macOS save helpers use the same private writer. Exclusive writes request 0600 with `O_EXCL | O_NOFOLLOW`, then `fchmod` the same fd **before writing any contents**. Replacement writes create a UUID-named private staging file in the destination directory and rename it after writing/fsync. Rust diagnostic ZIP export now uses exclusive creation with `.mode(0o600)` before writing.

Executed under umask 002: Rust diagnostic-export tests, Swift CSV-style replacement and PSBT-style exclusive writes, and the Swift umask subprocess regression. Modes were 0600. The real PSBT view-model regression passed and preserved refusal to overwrite an existing file. Swift tests also verified private atomic replacement, contents and temporary-file cleanup. macOS callers were checked by source inspection; no macOS execution is claimed.

## Regression validation

- `umask 002; TMPDIR=/work/scratch/d1-r3 CARGO_BUILD_JOBS=12 cargo test --locked -p dw-fs -p dw-engine -p dw-vault -p dw-desktop -p dw-testutil -p dw-console -p dwcli -p dw-ffi`: **passed, 279 tests, 0 failures, 1 ignored; doc-tests passed**. Log: `rust-tests-rerun.log`.
- `scripts/linux-docker-test.sh`: **passed, 539 Swift tests in 68 suites**; Rust core build and committed-binding comparison also passed. Includes root swaps, umask-child engine opening, private writes, settings, PSBT save and real-engine regressions. The child-only umask test is skipped in the parent as intended; its launching parent test passed. Log: `swift-linux-tests.log`.
- Additional Rust harness: **16 passed**; additional Swift ancestor-swap, post-mkdir-swap, proc-fd and umask export probes: **passed**. Logs: `adversarial-tests.log`, `swift-adversarial.log`, `opath-proof.log`, `mutations.log`.
- `cargo fmt --all --check`, targeted all-target Clippy with `-D warnings` for `dw-fs`, `dw-engine`, `dw-vault`, `dw-desktop`, and `git diff --check f699e3a..HEAD`: **passed**.

The first Rust invocation correctly refused my initially 0775 scratch ancestor and stopped in the console integration test. I corrected that scratch directory to 0700 and reran the complete command successfully; this was a harness setup failure, not a branch regression. Build output remained checkout-local for host Rust and in the script's existing isolated Docker volumes.

This approval is limited to the requested follow-up findings and regression checks. Bind mounts, arbitrary hostile same-uid/root processes, upstream dependency-wide filesystem auditing, and native macOS/Windows execution were outside scope. Final tracked checkout status is clean at the reviewed commit.
