# DP3-04b storage pin and pool rotation: Sol review, round 3

**Verdict: CHANGES REQUIRED.** Reviewed platform `6273f1ca51..cb2988b285` and desktop `f100cce9..1b7842d1` in detached worktrees. The three round-2 schedules are closed, and the m4 numbering survived the merge. Two restore paths can still replace a healthy wallet with the wrong or unusable bytes.

## Findings

### High — Restore accepts a history that the next open refuses

The source and staged checks in `packages/rs-platform-wallet-storage/src/sqlite/backup.rs:291-293,372-374` call `assert_schema_history_well_formed`, but that check validates existing fork rows only and checks fork schema only for a *recorded* applied version (`src/sqlite/migrations.rs:707-716`). It also checks upstream `applied_on` and checksum syntax without comparing the upstream row to the embedded migration (`:665-696`). A backup can therefore pass both restore gates while `run_for_open` will reject it. With `--no-auto-backup`, the healthy destination has already been replaced.

Two CLI schedules on `cb2988b285`, with the source mutations checkpointed into the main file:

1. Migrate a valid `base.db`; copy it to `live.db` and `backup.db`; run `DELETE FROM dw_fork_schema_history WHERE version=1` on the backup, leaving `core_address_pool_retired` and the contact column intact. `restore --from backup.db --yes --no-auto-backup` exits **0** and replaces `live.db` with a file whose fork history is empty. `migrate` on that destination exits **1**: `table core_address_pool_retired already exists` when fork V1 tries to run.
2. Instead run `UPDATE refinery_schema_history SET checksum='0' WHERE version=1` on the backup. Restore again exits **0** and replaces the destination. `migrate` then exits **1**: `applied migration V1__initial is different than filesystem one V1__initial`.

The new row matcher closes the malformed *present-row* cases, but a missing applied row and a divergent upstream row remain. Before the rename, validate the staged file against the complete embedded histories and prove that its pending migrations can complete, for example on a disposable copy of the staged bytes. Add restore regressions that verify the destination survives both refusals.

### High — Restoring a WAL source silently drops its committed frames

`restore_from` validates the source through SQLite (`backup.rs:284-293`), then stages it with a raw copy of `src_backup` (`:349-355`). SQLite's source connection sees committed `-wal` frames, while the copied main file does not. The staged integrity and history checks can pass on that older snapshot, and the rename discards the destination and its sidecars (`:360-374,424-451`). This is a pre-existing path, but it is within this round's restore safety check.

CLI proof: a valid source database in WAL mode held one committed `wallets` row with `birth_height=111` in its uncheckpointed WAL; the healthy destination held one row with `birth_height=222`. The source connection read **111**, and a copy of only its main file read **no rows**. `restore --from source.db --yes --no-auto-backup` exited **0**; the destination then held **no wallet rows**. Stage a coherent SQLite snapshot with the backup API, or reject sources with uncheckpointed WAL frames before replacing the destination. Test a live WAL source whose committed state differs from its main file.

## Round-2 closures and other checks

- **Malformed fork checksum:** the exact `checksum='not_a_u64'` backup schedule now returns restore exit **3**, leaves the healthy destination byte-for-byte unchanged, and it reopens (`migrate` exit **0**).
- **Reconcile refusal:** the fork V1 row under `wrong_name` beside the exact old upstream V019 returns migrate exit **1**. The old V019 row and the conflicting fork row remain; the deletion no longer commits ahead of the refusal.
- **Reshaped fork table:** replacing `core_address_pool_retired` with `(wallet_id BLOB)` returns migrate exit **1** with `SchemaHistoryMalformed`; the replacement table remains for diagnosis. The table and index statement comparison closes this round-2 case.
- **Skipped CHECK constraint:** acceptable as a documented limit for this pin. D37 explicitly excludes the added contact column's CHECK; `SCHEMA.md` lists the compared attributes. The writer stores a `u32` as `i64`, and the reader uses checked `i64_to_u32` conversion (`schema/contacts.rs:244,405-413`), so a dropped CHECK does not silently turn an out-of-range reference into a payable account. A future upstream migration touching this column still needs the D36/BL-193 reconcile review.
- **Desktop follow-merge:** `f100cce9` had DP3-04b as item 56 at Contract-Version 29. The `0eda2ead` merge adds DP5-01 as item 56 and DP4-02 as item 57, moves DP3-04b to item **58**, and raises Contract-Version to **31**. `1b7842d1` retains that numbering and pins all eight platform dependencies to `cb2988b285`; the rust-dashcore pin and patch remain as before.

## Validation

Built the platform CLI at the reviewed head with `cargo build -p platform-wallet-storage --locked --bin platform-wallet-storage --features dpp/ed25519-dalek` (exit 0); the explicit DPP feature was needed by this package's default feature graph. Ran the five standalone CLI schedules above against that binary and inspected the resulting SQLite rows. Platform `git diff --check 6273f1ca51..cb2988b285` passed. No branch was edited, committed or pushed.
