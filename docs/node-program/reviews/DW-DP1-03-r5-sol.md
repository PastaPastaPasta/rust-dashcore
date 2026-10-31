# DW DP1-03 — narrow confirmation review, round 5

Reviewed `dashwallet-desktop` branch `dw/dp1-03-names` at **`2b077b43464bd5adb891d882e861367d0eccb561`**, against r4 head `0ae1be56fd184f081f4b0f1ea8dba4b3a749e08e`. Read `DW-DP1-03-r4-sol.md`, `REVIEW-COMMON.md`, and binding **DEC-129**; applied the code-review-validator workflow within the requested scope. No settled registration, fee, grant, transport, UI, or live-network areas were re-reviewed. The implementation worktree remained clean and unmodified. Experiments ran in a detached scratch checkout at the reviewed head, changing tests only. No commits, pushes, or GitHub comments.

## Findings

### R5-1 — major: fallback ownership rows are copied from the old snapshot, so departed or reassigned names still qualify

**File:line:** `rust/crates/dw-engine/src/platform/recovery.rs:320`, `:330`, `:403`, `:443`, `:449`.

The fallback checks `o.row_owned`, but that vector was extracted from the library's marketplace working set when `OwnedIdentity` was saved. It is part of the same old snapshot as `o.names`; it is not ownership evidence read from the current persisted rows for this call. Only preferences are read from SQLite before fallback. A later committed departure or identity reassignment does not revoke this old positive evidence.

**Concrete failure, reproduced:** Give the identity `Al1ce` and an `Owned` marketplace row for normalized `alice`; call the real `identities()` to save the snapshot. Replace the live and persisted marketplace row with `Transferred { to: other_identity }`. The real `main_name()` is now `None`, and selecting `alice` is refused as `invalid_argument`. Hold the ordinary wallet-manager write lock and call `identities()`: it returns **`names = ["Al1ce"], main_name = Some("Al1ce")`**, using the snapshot's old `Owned` evidence. The probe read the committed row back through the production persister and verified `Transferred` before the summary call.

A second probe replaces that same document's persisted row with an `Owned` row for another identity and removes the old owner's label using the library's confirmed-departure primitive, `ManagedIdentity::remove_dpns_name`. The live main name is `None`; fallback again returns **`["Al1ce"] / Some("Al1ce")`**. A fresh other-identity row is correctly rejected by the existing identity filter; the stale snapshot is what bypasses that protection.

**Suggested fix:** Default-deny using current persisted ownership evidence scoped to the wallet, identity, normalized DPNS label, and `Owned` status. Do not treat the snapshot's `row_owned` vector as a fresh ownership fact. If that evidence cannot be obtained while the manager is locked, hide the label for that call, as DEC-129 allows. No per-label suppression memory or change to the ruling is needed. Add departure and reassignment regressions through the actual summary fallback.

### R5-2 — major: a retained pick authorizes a provisional name that is no longer owned

**File:line:** `rust/crates/dw-engine/src/platform/recovery.rs:327`, `:330`; pick validation and retention at `rust/crates/dw-engine/src/platform/names.rs:288`, `:1444`, `:1447`, `:1456`.

The public pick writer correctly validates ownership when the pick is made. That is insufficient for the new fallback: the selection rule deliberately retains a pick after ownership is lost, while `picked(label)` checks only normalized equality. Matching a saved provisional label therefore promotes the retained preference into current ownership evidence. The main-name resolver cannot repair this, because its supposedly owned input already contains the label accepted by that shortcut.

**Concrete failure, reproduced through real pick and refusal APIs:** Start with owned `Al1ce`; select `alice` through `DashPay::set_main_name`, which stores the owned spelling `Al1ce`. Remove the owned label using the library's departure primitive: `main_name()` becomes `None`, but the pick remains, as intended. An interrupted later retry leaves provisional `Al1ce` with `pending_name = alice`. Call `identities()` to save this raw provisional snapshot; its returned names and main name are correctly empty, and a new attempt to pick `alice` is refused. Retry registration against the scripted `Locked` response through the real `DashPay::register`/`forget` path. Before that call returns, the live name list is empty, pending is empty, refused contains `alice`, and `main_name()` is `None`. With the manager write lock held, fallback nevertheless returns **`names = ["Al1ce"], main_name = Some("Al1ce")`**, solely because the retained pick matches.

This is not a request to make suppression markers govern fallback. Under DEC-129 the missing condition is that the stored pick must still be a pick **of an owned name**. The latest saved snapshot in this reproduction contains a provisional retry, not the originally owned name.

**Suggested fix:** Preserve the stored preference, but qualify its use as fallback evidence against current persisted ownership. A historical valid selection alone cannot establish that the label is owned now. If ownership cannot be positively established, hide it for this read. Add the owned-pick → departure → provisional retry → definitive refusal regression; the new pure summary test supplies an arbitrary matching pick and never exercises loss of ownership.

## R4 disposition under DEC-129

- **R4-1 closed.** The five-statement assembly is deleted. `AppDb::wallet_dp_prefs` prepares one SELECT, binds the wallet, and fully collects every `(identity, key, value)` row while holding the connection mutex. Both sync and async readers build `MainNamePrefs` from that one result. The committed `review_r4_split_read_during_contention` now checks the post-settlement state, not the old scheduled split-read boundary; its passing on the old head would not prove this fix. The source audit and independent statement/snapshot probes below supply that proof.
- **R4-2's original two-contender reproduction is closed.** The committed regression runs the real `conclude_others`/registration settlement and requires both summary fields to remain empty with the manager locked. It passed on this head. Neither contender in that case has a marketplace row or stored pick, so the new default-deny filter rejects both independently of the single contested-name marker. No per-label suppression memory was added.
- **DEC-129's broader fallback evidence requirement is not satisfied.** R5-1 and R5-2 show that the two positive-evidence shortcuts can still assert ownership after it is disproven. Closing the original reproductions does not close these explicitly requested evidence checks.

## One SELECT, SQLite snapshots, and reader audit

`wallet_dp_prefs` is at `rust/crates/dw-appdb/src/lib.rs:698–704`. It has one prepared statement:

```sql
SELECT identity, key, value
FROM dp_prefs
WHERE wallet_id = ?1
ORDER BY identity, key
```

`query_map` is lazy, but `collect` completes inside the method while both the statement and `MutexGuard<Connection>` are alive. No iterator escapes, no second SELECT contributes preferences, and SQL/decoding errors propagate.

Production opens one `AppDb` with `Connection::open`, requests WAL, and shares its `Arc` between sync calls and the blocking-pool helper (`dw-appdb/src/lib.rs:156–189`; `dw-engine/src/session.rs:225–234`; `dw-engine/src/coins.rs:212–220`). Rusqlite's default `SQLITE_OPEN_NO_MUTEX` is protected by the application mutex; it does not weaken SQLite's statement isolation. The reviewed code enables neither shared cache nor `read_uncommitted`. The in-memory test connection uses SQLite's MEMORY journal, despite the WAL request.

A single fully stepped SELECT has one read snapshot in the modes in use. On the shared production connection, the mutex additionally prevents a preference write between returned rows. A separate private-cache WAL connection may commit during the read, but the active statement retains its snapshot. With rollback journaling, the active read transaction holds its read lock until the statement finishes, preventing an intervening writer commit.

Independent review-only tests attached SQLite's `sqlite3_trace_v2` to the real `AppDb` connection:

- **MEMORY, WAL, and DELETE:** verified the actual journal mode, `read_uncommitted = 0`, and autocommit state; a call returned all five seeded preference rows for the requested wallet, excluded another wallet, and emitted exactly **one SELECT / five row events**.
- **Concurrent WAL commit:** paused the real reader at its first `SQLITE_ROW`; a second private connection committed an update of all five preferences before the reader continued. That call returned all five old values; its next call returned all five new values. No mixed generation occurred.

Repository-wide reader search found these production paths only:

1. `identity_summaries` → `wallet_name_prefs` → `read_name_prefs` → `wallet_dp_prefs`.
2. `NetworkSession::name_prefs` → `read_name_prefs` → `wallet_dp_prefs`, used by DP1-03's async main-name, selection, and registration preference readers.

`MainNamePrefs::KEYS` is gone. The old per-key `dp_prefs` API remains, but its only call in the repository is a storage unit-test closure; no production reader assembles preferences with it. Backup row export is separate from these preference read models.

Labels use the pinned DPNS `convert_to_homograph_safe_chars`: lowercase plus homograph folding. `same_name` applies it to both pick and label, and marketplace comparisons apply it to the displayed label against the row's normalized label. The probes deliberately compare `Al1ce` with `alice`. Current marketplace extraction requires the same identity and `Owned` status. These comparisons are correct; the failures concern stale evidence and an unqualified retained pick.

## DP1-05 restore-test adjustment

**No hidden restore regression found.** `assert_shown` waits for the same complete result previously asserted immediately, with a 20-second bound and a final equality assertion (`recovery_tests.rs:576–585`, `:644`, `:652`, `:656`). The test still requires all three recovered names, the acquisition-based main name `alice`, the explicit `tmp-alice-1` pick, and restoration of `alice` after clearing the pick. It retains exactly-one bring-up and names-pass assertions.

The fixture's `zed` has no marketplace row and is never picked. Therefore the required full result, including `zed`, needs a live read; a filtered fallback result cannot accidentally satisfy the wait. Waiting for that live read is consistent with DEC-129's accepted brief hiding while a sync pass holds the manager lock. The adjusted r3 refused-name test likewise now expects the unrelated real name without a row to hide during fallback and return after lock release; its refused label remains absent.

## Validation

On the unchanged reviewed head, under `rust/`:

- `cargo fmt --check`: **exit 0**.
- `cargo clippy --workspace --all-targets -- -D warnings`: **exit 0**.
- `cargo test --workspace --no-fail-fast`: **exit 0; 649 passed, 0 failed, 4 ignored**, including doc tests. The two committed r4 regressions, `an_older_snapshot_shows_only_evidenced_names`, the adjusted refused-name test, and the adjusted restore test all passed.

In the detached scratch checkout, with production source unchanged:

- `cargo test -p dw-appdb review_r5_ -- --nocapture`: **exit 0; 2 passed**. These are the SQLite trace and intervening-commit probes described above.
- Final corrected `cargo test -p dw-engine review_r5_ -- --nocapture`: **exit 101; 1 passed, 3 failed**. All three failures occurred at the final fallback assertions, each returning `["Al1ce"] / Some("Al1ce")` instead of empty names / `None`:
  - `review_r5_stale_owned_marketplace_row` — committed row is `Transferred`; live API rejects ownership.
  - `review_r5_marketplace_row_now_belongs_to_other_identity` — committed row now belongs to another identity; original owner's live label is removed.
  - `review_r5_stored_pick_does_not_own_refused_retry` — retained valid historical pick, subsequent provisional retry, real definitive refusal.
  - `review_r5_other_identity_row_is_not_evidence_when_current` — **passed**; another identity's fresh row does not authorize a provisional name on either path.

Two probe setup corrections preceded that final run: use the public persistence trait on the session's actual store rather than private `WalletPersister` methods, and use `remove_dpns_name` rather than an empty fetch, because the pinned library merges fetches for managed identities. Those preliminary compile/setup failures are not implementation findings. The final probes use the production persistence backend and existing library mutation APIs, with no production pause hook or implementation patch.

Validation used `umask 022`, `TMPDIR=/dev/shm/dp1-03-r5`, and `CARGO_BUILD_JOBS=12`. Successful gate and final probe runs used `RUSTC_WRAPPER=`. The scratch checkout had its own reflink-copied `rust/target`; no target directory was shared. The temporary directory was restricted to mode 0700 before cleanup. No shared cache service, other agent, or Dash Core process was stopped.

The scratch worktree, its test additions and build output, logs, and this review's `/dev/shm` directory were removed after validation. This uncommitted report is the only retained review artifact.

## Open questions / limits

None requiring a decision. DEC-129 was applied as binding and was not reopened. The departure probes reproduce persisted/live state changes with the production store and library setters; they do not submit a public-network transfer. Settled areas remain outside this confirmation review.

## Verdict

**CHANGES-REQUIRED.** The one-statement preference fix and the original two-contender case are confirmed, and the restore-test adjustment is acceptable. The older-snapshot path still treats stale marketplace evidence or a retained, no-longer-owned pick as sufficient ownership evidence.

**Sol gate satisfied: no.**
