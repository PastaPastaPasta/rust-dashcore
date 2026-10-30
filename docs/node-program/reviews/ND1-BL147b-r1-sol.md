# ND1 BL-147(b) review — round 1 (Sol)

Review target: `node/nd1-bl147b` at `50967d825` (base `cf5079316`), four files, +131/-15.

## Verdict

**CHANGES-REQUIRED**

The production fix is correct for rdashd's configured RocksDB 11.8.1 path and the Core refusal behavior remains intact. One major test gap remains: the committed IDENTITY guard does not verify the ordering invariant that makes the fix safe.

## Major finding

### M1 — the IDENTITY guard does not catch a future ordering regression

**Location:** `dashd-node/src/datadir.rs:480-503`, especially the test at `:485-503`.

`a_rocksdb_the_node_makes_has_its_identity_file` opens the database normally, then asserts that `IDENTITY` exists after `Open` has completed. It deliberately says at `:482-483` that it does not check the ordering. A RocksDB upgrade that writes `CURRENT`/`MANIFEST` first, writes `IDENTITY` later, and still leaves `IDENTITY` present after a successful open would pass this test while reintroducing the BL-147 failure window. A SIGKILL in that new interval would produce the exact `CURRENT + MANIFEST`, no `IDENTITY`, no `OPTIONS-*` shape that `is_leveldb` treats as Core and refuses forever.

The current source and independent probes prove the present ordering, but the regression is not pinned by the test suite. Add an automated ordering check: for example, a test-only file-system/Env hook that records the first durable rename/create of `IDENTITY` and `CURRENT`, or a checked-in kill/sync-point probe that kills at `SetCurrentFile`/the corresponding future hook and asserts the restart loads. Keep the existing eventual-presence assertion as a separate guard. The required invariant is `IDENTITY` durable before `CURRENT` becomes durable, not merely `IDENTITY` eventually exists.

This is **major** because the code's correctness depends on a storage-engine ordering guarantee and a dependency upgrade can silently restore the original permanent-startup failure.

## RocksDB source audit

Against vendored librocksdb-sys 0.19.0+11.8.1:

- `db/db_impl/db_impl_open.cc:329-389` calls `SetupDBId(... is_new_db=true)` before creating/syncing `MANIFEST-000001` and calling `SetCurrentFile`.
- `db/db_impl/db_impl_files.cc:1124-1172` calls `SetIdentityFile` when `write_identity_file` is enabled. The branch now sets it explicitly through the C API at `dashd-store/src/rocks.rs:202-207`.
- `file/filename.cc:469-500` writes `000000.dbtmp` with sync, renames it to `IDENTITY`, and fsyncs the DB directory. `NewDB` then syncs the manifest and makes `CURRENT` point to it.
- `db/db_impl/db_impl_open.cc:2787-2791` deletes an obsolete `IDENTITY` after successful recovery when `write_identity_file=false`; the source comments and `IdentityAcrossRestarts` test confirm that behavior.
- RocksDB's table writer uses `MakeTableFileName`, whose extension is `kRocksDbTFileExt` (`.sst`). `TableCache` only tries `Rocks2LevelTableFileName` (`.ldb`) as a read fallback when an `.sst` is missing; RocksDB does not create `.ldb` tables in this path.
- The normal rdashd creation/reopen/recovery and manifest-roll paths therefore retain `IDENTITY` before any state with `CURRENT` and `MANIFEST` can be observed without `OPTIONS`. WAL recovery and a manifest roll do not remove the durable identity.
- `DestroyDB` can of course leave arbitrary debris if an external process is killed while deleting files, and an external RocksDB opened with `write_identity_file=false` can remove `IDENTITY`; those are outside rdashd's configured path and fall into the already documented foreign-tool/D4 residue. If the product intends to defend against arbitrary third-party RocksDB mutation too, that needs a stronger provenance marker than this filename test.

I ran an independent reduced gdb matrix on a reflinked copy of the head binary: 5/5 kills at `rocksdb::DBImpl::CreateColumnFamilyImpl` loaded on restart, and 5/5 kills at `rocksdb::DBImpl::WriteOptionsFile` loaded on restart. The implementer's 20-run matrix is consistent with this.

## LevelDB/Core audit

Against Dash Core v24.0.0-rc.4 bundled LevelDB:

- `leveldb/db/db_impl.cc:182-212` writes and syncs the manifest, then `CURRENT`; `Recover` at `:290-317` treats `CURRENT` as the creation commit and reopens a half-made DB with `create_if_missing`.
- `leveldb/db/filename.cc` recognizes both `.ldb` and historical `.sst` table names, and there are no `IDENTITY` references in bundled LevelDB. Thus a fresh Core DB with `CURRENT + MANIFEST` but no table is still recognized by the fallback “neither IDENTITY nor OPTIONS” arm; a Core DB with old `.sst` names is also recognized because the fallback arm does not require `.ldb`.
- `-reindex` does not create a new provenance shape that defeats this check; Core still owns the same LevelDB directories and either has the normal table/no-table shape or the stronger `evodb`/`llmq` signs.
- A copied RocksDB file is not a Core database. With `IDENTITY` or `OPTIONS-*` and no `.ldb`, the new probe deliberately passes it onward to RocksDB, where schema/foreign-database checks reject it. That is separate from protecting a real Core DB.
- A real v24 Core chainstate with a table remains refused even if a stray `IDENTITY` is added, because the `*.ldb` arm wins. I ran `a_data_directory_dashd_made_is_refused_untouched` against the installed dashd v24.0.0-rc.4; it passed with status 0.

The accepted D4 residue is real and narrow: a bare Core chainstate with no `.ldb` yet (or only old `.sst`), no `evodb`/`llmq`, and an externally planted RocksDB `IDENTITY` can pass the datadir probe. Init then writes `debug.log`, `settings.json`, and `blocks/` before a later refusal (the probe stops at `sporks.dat`). This is weaker than Core's behavior on its own foreign datadir, but it requires the third-party RocksDB mutation described in DEC-605/DEC-609 and was explicitly accepted there. The `.ldb` arm removes the same risk once a real Core table exists.

## Lock/TOCTOU audit

`refuse_core_dir` still performs the first read-only probe, and for a LevelDB sign obtains `lock_existing(dir, ".lock")` before re-probing at `dashd-node/src/datadir.rs:330-341`. The second decision is therefore under the directory lock, with no check-to-open race for a genuine LevelDB sign. `evodb`/`llmq` remain the certain signs checked before the LevelDB lock path. The subsequent `app_init_main` acquires rdashd's real lock before store open. The added process test confirms `evodb` wins even while another dashd-style lock is held.

## Tests and validation

All requested relevant checks passed on `50967d825`:

- `cargo fmt --all -- --check`
- `cargo clippy -p dashd-node -p dashd-store --all-targets -- -D warnings`; the feature/all-features clippy passes were also green in the implementer's `check.sh` evidence.
- `cargo test -p dashd-node --features test-support datadir::`: 10 passed, 1 ignored helper.
- `cargo test -p rdashd --test features --test process`: 11 + 11 passed.
- The focused Core-v24 differential refusal test: 1 passed.
- `cargo test -p dashd-store`: 70 unit tests passed, 8 engine-contract tests passed, 2 fail-stop tests passed, 1 feature-scope test passed, 2 WAL tests passed; 3 expected ignored tests.
- `git diff --check`: clean.

The committed mutants are meaningful and killed: `id`, `ldb`, `order`, and `wif` fail the relevant unit/process tests. They do not include the ordering regression described in M1, which is why the guard gap remains.

No other blocker or major correctness issue was found in the current RocksDB 11.8.1 implementation. The refusal text and exit code for a genuine Core datadir remain unchanged and are covered by the v24 comparison test.
