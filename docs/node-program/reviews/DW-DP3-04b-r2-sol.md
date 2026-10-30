# DP3-04b security and correctness review (Sol, round 2)

**Verdict: CHANGES REQUIRED.** Reviewed the detached platform snapshot `f2f15b55..6273f1ca51` and desktop snapshot `223d5f4c..f100cce9`. The separate migration histories resolve the round-1 V019 collision for normal old-fork and later-upstream paths. Three guard failures remain.

## Findings

### High — Restore accepts a malformed fork history and replaces a healthy wallet

`packages/rs-platform-wallet-storage/src/sqlite/backup.rs:291-293,372-374` validates the upstream history before and after staging, but `assert_schema_history_well_formed` reads only `refinery_schema_history` (`src/sqlite/migrations.rs:538-572`). The fork guard checks only `MAX(version)` (`:454-475`). A fork V1 row with a nonnumeric checksum or invalid timestamp passes both restore gates. The restore commits it over the destination, then `SqlitePersister::open` fails when the fork runner parses the row (`src/sqlite/migrations.rs:223-249`). The allowed `--no-auto-backup` path can thereby discard the only healthy destination.

**Concrete schedule:** Create a valid database with `platform-wallet-storage --db live.db migrate`; copy it to `bad-backup.db`; run `UPDATE dw_fork_schema_history SET checksum='not_a_u64' WHERE version=1` on the backup; run `platform-wallet-storage --db live.db restore --from bad-backup.db --yes --no-auto-backup`. Restore exits 0 and changes `live.db` to the bad checksum. `platform-wallet-storage --db live.db migrate` then exits 1 with `checksum is not a valid u64`. I reproduced this on the requested head using files under `/work/scratch/dw-dp304b-r2-sol/probe-fork-restore/`.

**Fix:** Validate both histories, including the fork row shape and refinery-readable values, on the source and staged bytes before the replacement. Add a restore regression that leaves the destination byte-for-byte intact on malformed fork history.

### Medium — Reconcile deletes the exact old V019 row before rejecting a conflicting fork row

When both histories contain fork V1, `reconcile_old_fork_history` checks the existing fork row's checksum but not its name (`src/sqlite/migrations.rs:98-128`). A row with the correct fork V1 checksum and a different name passes this check. The function commits deletion of the exact old upstream V019 row; only afterward does refinery reject the fork row as divergent (`:173-178`). The failed open has changed the database and removed the original migration record without an open or a pre-migration backup.

**Concrete schedule:** Start from a valid database; change `dw_fork_schema_history` V1's `name` to `wrong_name` but retain its checksum; add the exact old upstream `(19, dashpay_external_rotation, 15375114024144232753)` row. `platform-wallet-storage --db duplicate.db migrate` exits 1 with `applied migration V1__wrong_name is different than filesystem one`; querying afterward shows the old V019 row has been deleted. Reproduced on the requested head.

**Fix:** Require both the expected fork name and checksum before deleting the old row. Preferably keep reconciliation and both migration runners in one transaction, so a later validation failure cannot commit a partial open.

### Medium — Fork schema check accepts a replaced table with the required name but unusable shape

`missing_fork_schema` checks that `core_address_pool_retired` is a table and that `contacts.external_account_reference` is a column (`src/sqlite/migrations.rs:42-49,354-389`). It does not check the retired table's required columns, key, or index. A later upstream migration can rebuild the table under the same name, remove its working schema, and pass the advertised guard; the two runners then commit (`:173-192`). A later retired-pool read expects `account_xpub`, `pool_type`, `retired_at`, and other missing columns (`src/sqlite/schema/core_pool.rs:802-815`). A simple rename of `contacts.external_account_reference` *is* caught; replacing the table under the same name is the hole.

**Concrete schedule:** On a current database, run `DROP TABLE core_address_pool_retired; CREATE TABLE core_address_pool_retired (wallet_id BLOB)` while leaving fork history V1. `platform-wallet-storage --db shape.db migrate` exits 0 (`applied: 0`), despite the unusable retired table. Reproduced on the requested head. The same SQL in a future upstream migration would be committed by the two-runner transaction.

**Fix:** Check the actual V1 table shape and essential constraints/indexes before commit, or perform a targeted compatibility query that proves the fork readers and writers' required columns and conflict key. Keep the D36 manual pin-bump rule as a separate requirement.

## Checks without findings

- The old V019 SQL is byte-for-byte the fork V1 SQL; the literal old checksum is asserted against refinery's V019 calculation (`migrations.rs:867-877`). Normal old-fork files and backups reconcile the exact row, preserve `applied_on`, and reopen with contact stamp and pool data intact (`tests/sqlite_dashpay_external_rotation.rs:623-769`). A different old checksum is refused untouched. Repeating reconciliation with no old row makes no further change.
- Upstream and fork runners retain refinery's `abort_divergent=true` and `abort_missing=true` defaults (refinery-core 0.9.2 `runner.rs:240-241`) and execute in one `IMMEDIATE` transaction (`migrations.rs:149-193`). An interruption or failure during either runner rolls their migration SQL and history back together. Reconciliation uses a separate committed transaction; a crash after that commit leaves a file this build can retry, but the prior V019 build cannot reopen it. That one-way downgrade is documented in `SCHEMA.md`; no automatic pre-migration backup is made when reconciliation is the only change (`persister.rs:367-395`).
- Newer upstream or fork history is refused before reconciliation on open, and on the restore's source and staged copy (`persister.rs:363-367`; `backup.rs:291-293,372-374`). The desktop maps only those two typed storage errors to `storage_too_new` (`dw-engine/src/error.rs:235-253`), the renderer omits `Try again` for that code (`apps/desktop/src/app/openFailure.ts:7-17`), and Swift FFI still receives `storage` (`dw-ffi/src/api/error.rs:67-73`). Other storage errors retain their existing retry and code.
- The desktop lockfile changes are 31 platform `source` replacements from `f2f15b55` to `6273f1ca51`; the eight platform dependencies in `rust/Cargo.toml` use the new commit. Rust-dashcore stays at `8fe0a381` with the `d3d50520a` fork `[patch]`.

## Validation

- `CARGO_TARGET_DIR=/work/scratch/dw-dp304b-r2-sol/target-platform CARGO_BUILD_JOBS=12 cargo test -p platform-wallet-storage --all-features --locked` — passed, exit 0. The platform unit and integration targets, including the old-fork open/restore cases and the ten migration scenarios with simulated later upstream V019–V021, passed; doc tests passed (5 passed, 1 ignored). No wall-clock failure required a rerun.
- `CARGO_TARGET_DIR=/work/scratch/dw-dp304b-r2-sol/target-desktop CARGO_BUILD_JOBS=12 cargo test -p dw-engine --locked --test session_lifecycle` — passed, 12 tests, 0 failed. This includes `a_wallet_file_from_a_newer_build_is_storage_too_new` for both histories. No wall-clock failure required a rerun.
- Three standalone CLI/SQLite probes reproduced the findings above. The probes used private files in `/work/scratch/dw-dp304b-r2-sol/probe-fork-restore/`; no branch was edited.
- `git diff --check 223d5f4c..f100cce9` passed. Platform `git diff --check f2f15b55..6273f1ca51` reports only an extra blank line at the end of `SCHEMA.md:902`, unrelated to the findings.
