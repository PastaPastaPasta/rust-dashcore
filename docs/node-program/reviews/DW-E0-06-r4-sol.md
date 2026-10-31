# DW E0-06 — narrow confirmation review, round 4

Reviewed `dw/e0-06-changeset-tap` at **ead9d8b2ba2ab849f4de28ef779fc3b35139d5eb**, comparing **7120df693d28c2d96fc12b7192a528b0f613bfd1** through commits **e182141** and **ead9d8b**. Read the r3 Sol review and binding **DEC-135 / DEC-139**, with DEC-139 superseding DEC-135's unrestricted bring-up advance. Scope: the three r3 findings, latch power-loss durability and checkpoint refusal, recovery-marker consumption, and commit-error propagation. Settled classification, positive-provenance policy, notification reconciliation, debounce, deduplication and phase-ledger decisions are not reopened.

**Verdict: REQUIRES CHANGES.**

**Sol gate satisfied: no.**

The supplied gate reproduces **640 passed / 0 failed / 3 ignored**, with fmt and Clippy clean. **R3-2 is closed.** The ordinary R3-1 retry/read-error cases and R3-3 stopped-discovery replacement now pass. Two remaining paths prevent closing R3-1 and R3-3: a reopen trusts an unsynced latch left by a refused checkpoint, and a stop before the Platform pass consumes the discovery marker needed by the replacement bring-up.

## Findings

### R4-1 — major, required: reopening after a refused checkpoint bypasses latch durability

**Locations:** desktop `rust/crates/dw-engine/src/platform/provenance.rs:65`, `:69`, `:97`, `:109`; `rust/crates/dw-appdb/src/lib.rs:698`.

`raise_setting()` commits the latch under `synchronous=NORMAL`. If `checkpoint_full()` returns busy, the current `fallback_in_use()` correctly returns `Err` and does not set its durable cache. The committed latch nevertheless exists in the WAL. `Provenance::open()` treats **any readable existing row** as both `fallback_used = true` and `latch_durable = true`. The next `fallback_in_use()` then returns `Ok` without checkpointing. Reading a committed WAL row does not establish power-loss durability.

A real-session probe reproduces this through actual engine shutdown/reopen, without modifying production code:

1. Open a regtest session, restore a wallet, and fully checkpoint app.sqlite.
2. Keep an independent app.sqlite reader in `BEGIN; SELECT * FROM settings_kv`, pinning the pre-latch database snapshot.
3. Call the session's `fallback_in_use()`: the latch commit succeeds, its full checkpoint cannot finish, and the call returns `Err`. A copy of **app.sqlite alone**, without `-wal`, has no latch.
4. Shut down the engine and reopen the same network directory, keeping that reader open. No successful checkpoint intervenes.
5. The reopened session's `fallback_in_use()` returns **`Ok`**. A fresh database-file-only copy still has no latch.

The syscall trace establishes the missing durability step, rather than inferring it solely from the file copy. Between the probe's start/end markers, SQLite writes and syncs a new **32-byte WAL header**, then writes the latch's frame headers and two 4096-byte pages. There is **no subsequent fsync/fdatasync of app.sqlite or its WAL**, through the busy checkpoint, shutdown, reopen and successful fallback admission. The header sync precedes the latch frames and cannot make them durable. Other wallet/vault files do not establish durability for this WAL.

A power loss at that point is allowed to discard the latch frames even though the reopened session has permitted a fallback fetch. This violates DEC-135 (1) and DEC-139 (1′), and preserves the original provenance-loss class. The reader is the very independent-reader condition the new checkpoint refusal handles; this is not a corrupt-schema or invented storage-mode case.

**Fix:** do not initialize `latch_durable` merely from latch presence. Presence remains conservative evidence for `fallback_used`; the first fallback admission in each new Provenance instance must establish durability, including for a row left by an earlier failed checkpoint. For example, initialize the durable cache false and run the existing keep-first-write plus full checkpoint before setting it. Add the busy-checkpoint → real close/reopen, reader still held → fallback-refused regression, then release the reader and assert success and a latch in the main file.

**Related publication ordering:** at `provenance.rs:110`, `latch_durable` is published before `fallback_used`. A first caller can be descheduled between those two stores; another caller can take the durable fast path and return while `is_unverified()` still sees false. Publish the classification latch before exposing the successful durable fast path. The persisted latch is safe in that interleaving; the in-memory classification is not. This ordering observation is from the new source, not a claimed empirical race reproduction.

### R4-2 — medium, required; cosmetic effect: cancelling before the Platform pass loses the replacement recovery advance

**Locations:** desktop `rust/crates/dw-engine/src/platform/bringup.rs:468`, `:482`, `:495`, `:388`; `rust/crates/dw-engine/src/platform/recovery.rs:250`, `:702`.

The new marker survives the explicit admission, watch-only, prelude-timeout and proven-absence returns: they precede `take_discovered()`. It also survives a dropped Readmit while SPV is stopped. However, it is removed **before asynchronous key acquisition and before the Platform subsystem call**. `stop_spv()` can cancel the task in that interval. The marker is not restored; names work remains pending. On the next start, the identity is on file and the discovered marker is absent, so the replacement bring-up receives the plain-warm-start treatment and does not advance before replaying recovery events.

The real-session probe uses the existing `pause_after_keys` scheduling seam, public start/stop/discovery APIs, the real persister and the real journal:

1. Complete the restored wallet's initial no-identity bring-up; stop SPV.
2. Explicit discovery stores an identity and queues its recovery. The Readmit has no supervisor receiver.
3. Restart SPV. Pause after the new bring-up has consumed the discovered marker and acquired keys, **before `run_subsystems()`**.
4. Stop SPV. Confirm the mock Platform bring-up count remains **1**: no post-discovery Platform pass has run.
5. Persist a name marketplace row dated after that aborted attempt's boundary, but before the next start. Restart SPV again.
6. The replacement bring-up stores the name **unread**. The pending names pass subsequently raises the boundary beyond its authoritative timestamp, but cannot change the already-journaled row.

Observed output:

```text
attempted_boundary = 1791573569
name_time_ms       = 1791573570001
final_boundary     = 1791573572
journal            = [("username_registered", "between@900", false)]
```

This is a deferred recovery, not an ordinary warm start: no post-discovery Platform bring-up has run, and the recovery's names work is still queued. The engine stays open throughout; it is outside the declared app-close loss. The timestamp is authoritative and known, so it is not the accepted undated-event residual.

**Fix:** retain the existing in-memory recovery intent across an attempt cancelled before the Platform pass, so its replacement advances before replay. Consume it at actual pass admission, or use the still-pending recovery scheduling information (`names_due` or equivalent, as DEC-139 permits) without losing the existing protection against names work being drained ahead of bring-up. Keep the ordinary warm-start test. Add this stop-before-pass/restart order, asserting **read**. No persistent marker, phase ledger, broad warm-start advance, or retroactive reconciliation is requested.

## Confirmation of the mechanisms

- **R3-1 ordinary write/refusal cases:** the setter returns errors, failed writes are retried, and an unreadable latch still classifies conservatively and is later written. The existing busy-writer, read-error/repair and persisted-identity close/reopen tests pass. R4-1 concerns the new distinction between a committed row and a successfully synced row.
- **Successful FULL checkpoint under NORMAL:** confirmed. Bundled SQLite **3.51.1** (`libsqlite3-sys 0.36.0`) gives NORMAL mode nonzero checkpoint sync flags. `walCheckpoint()` syncs the WAL before backfill and syncs the database after full backfill. `OP_Checkpoint` performs that work before yielding the pragma result; I/O failures are SQLite errors, and busy is the pragma's first column, which AppDb explicitly converts into `Err`. The successful latch test's trace shows WAL and database syncs, and its file-only copy contains the latch. A copy alone proves placement; SQLite source and the sync trace establish the durability operation. No physical power-cut experiment is claimed.
- **Busy checkpoint in the same instance:** refusal is correct; `?` precedes both cache stores, so another call retries rather than taking the success fast path. Busy need not imply that the WAL was synced: in the reproduced reader schedule, no safe backfill is possible and no latch-frame sync happens. Reopen must therefore re-establish durability.
- **R3-2 closed:** the numeric-max upsert runs under one explicit SQLite transaction and returns its stored value only after `tx.commit()?` succeeds. The cache uses `remember()` for both write completions and delayed read fills, preserving its maximum. The 900 → 2000 → stale-1000 test includes a stale cache fill and a dated 1500-second event; disk/cache stay 2000 and the event stays read.
- **RETURNING / commit errors:** `query_row()` drops/resets its statement while the explicit transaction still owns the write. `rusqlite::Transaction::commit()` then executes `COMMIT` and propagates its error; its default drop behavior rolls back remaining uncommitted work. An independent probe installs a rejecting `sqlite3_commit_hook` on real AppDb, reaches COMMIT after the upsert produced its RETURNING row, and confirms `raise_setting(2000)` returns `Err`, leaves the existing value **900**, and succeeds with **2000** after the hook is removed. This verifies the failed commit is not hidden by statement reset or by returning the captured row.
- **R3-3 ordinary replacement and warm-start paths:** both supplied regressions pass. The marker is inserted before Readmit and is separate from the names-due set, so an earlier names pass cannot consume it. An additional real-session admission-skip probe preserves the marker and produces a read name after readmission. The cancellation-before-pass case above remains uncovered by the supplied suite.

## Validation and limits

All scratch writes and build commands used **umask 022**, **TMPDIR=/dev/shm/e0-06-r4** (mode **0700**), and **CARGO_BUILD_JOBS=12**. Rust builds used **1.98.1**, the repository's toolchain. The implementer worktree was never edited and stayed at the requested head.

- Exact-head `cargo fmt --all -- --check`: **PASS**, exit 0.
- Exact-head `cargo clippy --workspace --all-targets -- -D warnings`: **PASS**, exit 0.
- Exact-head `cargo test --workspace --no-fail-fast`: **PASS**, exit 0, **640 / 0 / 3**. The supplied latch, boundary, stopped-SPV and warm-start regressions passed. The claimed mutation campaign was not independently rerun.
- Detached-head session probes: `cargo test -p dw-engine --lib review_probe_r4 -- --nocapture --test-threads=1`: **PASS**, **3 / 0 / 0**, 182 filtered out, **11.32 s**. These assertions reproduce R4-1/R4-2 and confirm admission-skip preservation. They were appended only in `/work/scratch/e0-06-r4-sol`, with that worktree's own copied target.
- `strace -f -yy -e trace=fsync,fdatasync,pwrite64,write` on the real-session latch-reopen probe: **PASS**, **1 / 0 / 0**, **5.89 s**; verifies no latch-frame sync before reopened fallback admission. A separate successful-latch trace also passed and showed the expected WAL/database syncs.
- Standalone AppDb/provenance probes: `cargo +1.98.1 test --offline --manifest-path /dev/shm/e0-06-r4/appdb-probe/Cargo.toml --lib review_r4 --test provenance_r4 -- --nocapture --test-threads=1`: **PASS**, **3 / 0 / 0** (one commit-hook test, two provenance tests). The AppDb production source is copied verbatim, with only the commit-hook test appended; provenance is included directly from the reviewed worktree. Confirms commit failure propagation, repeated checkpoint-busy refusal and successful retry after reader release, and the unsafe reopen fast path. The standalone crate has its own target and links the same rusqlite **0.38.0** / SQLite **3.51.1**.

No user input is required. E0-10b remains the future consumer of the fallible primitive; these probes evaluate admission and persistence, not a live provider or DAPI fetch. The session probes substitute only existing offline Platform/scheduling seams. No macOS UI or FFI generation is claimed. The power-loss conclusion assumes SQLite's VFS/storage honors sync, as SQLite's durability guarantee does.

## Cleanup

The temporary session source was restored to exact head ead9d8b and verified clean before the detached worktree and its own target were removed. The standalone harness, its own target, temporary databases, logs and traces were removed with `/dev/shm/e0-06-r4`. No probe/build process remains. The implementer worktree is clean at ead9d8b. This review is the only requested output, mode **0644**, and is **uncommitted**. No commits, pushes or GitHub comments.
