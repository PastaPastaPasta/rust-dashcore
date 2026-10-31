# BL-512: a settings write is synced before and after its rename

## Problem

ST3b's C6 (a crash during the recovery of a crash) found it on ext4 (run 1793271984-4185261,
points 160 and 200, seed 20261029). At start rdashd writes `settings.json.tmp` and renames it over
`settings.json`, as Core does, with no sync. In the second crash's log the file's data block is
written (entry 374) and the rename commits (entries 398-399), but the transaction that turns the
file's extent from unwritten to written commits later. A power cut in between leaves, after
journal replay, a `settings.json` of the right size whose only extent is unwritten, so it reads as
zeros, and every later start stops at "Settings file … does not contain valid JSON". Core v24.0.0-rc.4
has the same window: `WriteSettings` writes through an `std::ofstream` and closes it
(`util/settings.cpp:142-149`), and `WriteSettingsFile` renames over (`util/system.cpp:495-499`),
with no `FileCommit` and no directory sync.

## D-rows

| # | Decision | Why |
|---|---|---|
| D1 | **A settings write syncs the written temporary file before the rename and the directory after it (DEC-725), where Core syncs neither** (`util/settings.cpp:142-149`, `util/system.cpp:495-499`). `settings::write_settings` syncs the file once written; `ArgsManager::write_settings_file` syncs the directory that holds `settings.json` once the rename succeeds. The file's contents, the order of the writes, every error text and every refusal stay Core's. **A failed file sync keeps the old file** (r1 m-2, DEC-736): it is logged (`fsync failed: …`) and the temporary file is not renamed, as Core's own commit-then-rename gives up on a failed `FileCommit` (`SerializeFileDB`, `addrdb.cpp:78-87`); the write still returns Core's `true`. EINVAL, a filesystem that cannot sync, counts as synced, as in `FileCommit` (`util/fs_helpers.cpp:140`). A failed directory sync is ignored, as in Core's `DirectoryCommit` (`fs_helpers.cpp:153-162`): the old, valid file is what a lost rename leaves. On Windows only the file is synced, since there is no directory handle to sync. | A durability strengthening, not a behaviour change: without the syncs a power cut can keep the rename and lose the contents, and the node then refuses to start until the file is removed by hand. Core's own `peers.dat` and `anchors.dat` writes already commit before their rename (`SerializeFileDB`); `banlist.json` does not, written in place by `util::WriteSettings` (`addrdb.cpp:140-143`). |
| D2 | **The syncs sit behind a seam, `settings::SettingsSync`** (`OsSync` on the real filesystem). The unit test `a_settings_write_syncs_the_file_then_the_directory` records each sync with what the filesystem holds at that moment: the file sync must come while the temporary file holds every byte written and before the rename, and the directory sync must name `settings.json`'s directory after the rename, with no temporary file left. `a_failed_file_sync_keeps_the_old_settings_file` injects EIO (no rename, the old contents kept, the failure logged, the result `true`) and EINVAL (renamed). The first test also writes `-settings=sub/alt.json`, whose directory is not the datadir's (r1 m-1). Eight mutants die (in a reflink copy): no file sync, no directory sync, the directory sync before the rename, the datadir synced instead of the file's directory, a rename after a failed sync, no log line, EINVAL taken as a failure, `false` after a failed sync. | A missing sync is invisible to every test that does not cut the power; the seam makes each one observable. That `OsSync` really syncs is left to ST3b's C6 rerun on the fixed binary (r1 n-1): on a binary with this fix, 4 C6 recoveries had no `settings.json` failure, where the same shard on the old binary failed on it in 10 of 13 C6 runs (ST3b-D17). |
