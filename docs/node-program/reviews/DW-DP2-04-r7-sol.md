# DW DP2-04 — independent Sol review, round 7

Scope: `dc5dcf55..4e6b9d21`, reviewed in a detached 0700 worktree. The implementer's worktree was read only. No GitHub, commit, rebase, push, or merge.

## Findings by severity

- **Low — document sidecar replacement after an open returns.** `rust/crates/dw-appdb/src/lib.rs:404-409` checks the second lock descriptor's identity once, then keeps that descriptor. `docs/contracts/m4-dashpay-engine.md:2168-2169` lists database rename/move/bind-mount after open, but omits deletion or replacement of `app.sqlite.lock`. A local probe opened A, removed a wallet, unlinked the sidecar, opened B at the same database path, and wrote the removed wallet through A: B cleared the tombstone and the late row appeared. This requires an external filesystem mutation; the app does not remove its lock file. It is the same accepted boundary as moving the database while open, but the stated residual should name the sidecar too.
- **No High or Medium finding in the fix range.** The round-6 ordinary-path alias failure is closed.

## Closure table

| Item | Judgment |
|---|---|
| Round-6 High: final symlink / SQLite `file:` URI gets a different sidecar | **Closed** for those forms. Both are refused before an alias can clear a tombstone. Mixed-case `FiLe:` is also refused. |
| Linux ordinary alternate names | **Closed.** Parent-directory canonicalization makes `/proc/self/fd/<dirfd>`, `/dev/fd/<dirfd>`, symlinked directory with `..`, and a trailing slash use the original database and lock name. Final `/proc/self/fd/<filefd>` is refused as a symlink. |
| Hard-link and check/use changes | **Closed within a stable filesystem.** Two and three links are refused. Replacing the database before SQLite opens or the lock file between its two opens is refused by the committed tests. |
| Round-5 lifecycle, detached-job, and import items | Unchanged from round 6; no contrary evidence in the focused engine tests. |
| NFS, long restore, skipped-insert diagnostics, DEC-210 (5) | Manager-accepted residuals; unchanged. |
| Windows path identity | Accepted documented gap: only the final symlink is refused; no hard-link count or file-index comparison. No Windows runtime validation here. |

The database/sidecar replacement and file bind-mount residuals require explicit changes to files or mounts outside the app. The reviewed engine has no normal path that moves or removes either file while an `AppDb` is alive. On Windows the engine uses the fixed `app.sqlite` basename; the documented alternate file-name forms require a separately supplied alias.

## Alias probes

| Form | Expected | Observed |
|---|---|---|
| Symlink to `app.sqlite` (round-6 probe) | Refuse before a second sidecar | Refused; no alias `-wal` or `-shm` created. |
| `file:` and `FILE:` URI names, plus `FiLe:` with an existing relative parent | Refuse any case | Refused by the prefix check. |
| `/proc/self/fd/<dirfd>/app.sqlite`; `/dev/fd/<dirfd>/app.sqlite` | Resolve parent, retain tombstone | Both opened through the original name; late row skipped. |
| `/proc/self/fd/<filefd>` | Refuse final symlink | Refused. |
| Symlinked directory followed by `..`; `app.sqlite/` | Resolve to original database and sidecar | Both opened; tombstone retained and late row skipped. |
| Add second and third hard links while A is open; remove them, then reopen original | Refuse while `nlink > 1`; retain tombstone after removal | Both aliases refused with the correct hard-link error; original reopened with A alive and kept the tombstone. |
| File bind mount, created inside an unprivileged user/mount namespace | Documented deliberate filesystem alias boundary | Same `(dev, ino)` and `nlink == 1` through two names. The second `AppDb::open` reached SQLite but returned `sqlite: disk I/O error` with A's WAL open. It did not demonstrate a late write on this host. The names imply different lock and `-wal`/`-shm` sidecars, so this is outside the stable-name guarantee. |
| Delete `app.sqlite.lock` after A returns, then open B | External mutation boundary | B acquired a new sidecar, cleared the tombstone, and A's late row appeared. |

For accepted ordinary aliases, `resolve_directory` passes the canonical database pathname to SQLite, so SQLite's `-wal` and `-shm` names also converge. A file-only bind mount does not make the neighboring sidecars coincide. A directory bind mount would map the lock and SQLite sidecars with the directory.

## TOCTOU and regressions

- `rust/crates/dw-appdb/src/lib.rs:357-389`: an existing database must retain its `(dev, ino)` across the precheck and SQLite open; SQLite refuses a final symlink. If the precheck sees no file, another direct `AppDb` opener may create it before SQLite opens. Both use the same sidecar, so the permissive `before == None` arm does not clear a tombstone while the other opener's shared lock exists. The engine precreates `app.sqlite` before `AppDb::open` (`rust/crates/dw-engine/src/session.rs:246-250`), so its normal first-run path has an existing file at that check.
- In a five-attempt *direct `AppDb`* simultaneous fresh-open probe, one attempt returned `migration: duplicate column name: digest`. Both heads use the same `init` migration sequence without serializing it (`rust/crates/dw-appdb/src/lib.rs:418-425`; unchanged from `dc5dcf55`). This is an existing startup race, not a tombstone bypass or a regression from this fix.
- A file swapped between SQLite's actual open and the postcheck can escape the `before == None` arm only with an external rename/unlink. That is within the accepted moving-open-database residual. Replacing the sidecar after the second identity check is the Low documentation item above.
- A normal fresh profile, an upgraded profile, and a clean reopen passed the appdb unit suite. `AppDb::open` calls `symlink_metadata` for the database and opens it only through SQLite; it does not open or close a separate database descriptor. `open_sole_file` creates a lock file with mode 0600, including under `umask 002`; existing lock files follow the engine's owned-file permission restriction.
- Alias refusals are `AppDbError::File` with the path and reason; session open maps them to `EngineError::Storage`. The error strings were readable in the probes.

## Reviewer mutations

Each mutation was applied alone to a saved source copy in the detached worktree. After its focused test, the source was restored with `cp -p`, then `touch`, and verified byte-for-byte with `cmp`.

| Mutation (distinct edge case) | Focused result |
|---|---|
| Skip parent canonicalization only for `/proc/self/fd` paths, retaining ordinary directory-symlink handling | **Killed:** the `/proc/self/fd/<dirfd>` alias failed at SQLite open instead of opening under the canonical name. |
| Reject only exactly `file:` and `FILE:`, allowing mixed-case `FiLe:` | **Killed:** the `FiLe:real/app.sqlite` probe opened when it had to be refused. |
| Permit exactly three hard links while still refusing two | **Killed:** the three-link probe got SQLite I/O error instead of the required hard-link refusal. |

## Validation and limits

- `cargo test --locked -p dw-fs -p dw-appdb --lib`: 12 + 95 passed.
- `cargo test --locked -p dw-engine --lib removal`: 25 passed. One timing-sensitive test took over 60 seconds under load but passed in the same run.
- Independent alias probe passed on the restored head; the sidecar-replacement and bind-mount probes produced the observations above. The temporary probe source was removed.
- The `dw-fs` sole-file test also passed under `umask 002`.
- `cargo fmt --all -- --check`, `git diff --check dc5dcf55..4e6b9d21`, and Linux clippy for `dw-fs`, `dw-appdb`, `dw-engine` with `--all-targets -- -D warnings`: passed.
- Windows-target clippy for the same three crates and all targets: passed (`x86_64-pc-windows-gnu`).
- No full gate, Windows runtime, or NFS test was run. The unprivileged bind mount probe was confined to a new mount namespace.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
