# BL-512 r1 — Opus review (narrow)

- Branch `node/bl512`, head 2227e77a0 (fix ba5b1d0fa + notes 2227e77a0), base 0e6138849.
- Reviewed 2026-10-29T13:00Z.
- **Verdict: ACCEPT-WITH-NITS.** 0 M, 2 m, 3 n.

## Summary

The fix is correct and well placed.
- The temporary file is synced after `write_all` and before `rename_over`, at `dash-args/src/settings.rs:368`.
- The directory holding `settings.json` is synced only once the rename has succeeded, at `dash-args/src/args.rs:730-732`.
- Both production write paths go through it.
- Contents, error texts and refusals are unchanged from Core.

The three mutants named in the brief die, and so do two more.

Two minor items:
- a cheap mutant on *which* directory gets synced survives;
- a failed file sync is swallowed silently, where Core's own commit-then-rename pattern gives up on the rename.

## Focus 1: ordering and coverage

- **Write paths.** The only production caller is `init.rs:164`, which calls `ArgsManager::write_settings_file`. That goes to `write_settings_file_synced` with `OsSync` (`args.rs:694-700`). The public `settings::write_settings` also routes through `OsSync` (`settings.rs:325-331`), and nothing else in the workspace calls it. No other code writes `settings.json`; `grep` across the workspace found only tests and the `dash-coreimport` readers. The `backup=true` path (`settings.json.bak.tmp` → `.bak`) takes the same code, so it is covered as well.
- **Which directory.** `settings_path` returns `abs_path_join(datadir, -settings)` (`args.rs:645`), and `abs_path_join` asserts an absolute base (`paths.rs:382`). So `path.parent()` is always a non-empty absolute directory:
  - the datadir for the default path;
  - `<datadir>/sub` for `-settings=sub/x.json`;
  - the named directory for an absolute `-settings`;
  - `<datadir>/..` (kernel-resolved) for `../x.json`.
  
  When `settings.json` is a symlink, `rename(2)` replaces the link entry in that same lexical parent, so the directory synced is the right one. Symlinked directory components are followed by `open`, which is also correct. The temporary file always shares that directory, because it is `path` + `.tmp`, so one directory sync persists both the rename and the removal of the temporary entry.
- **Order.** The file sync happens after `write_all` and before the function returns to the rename. The directory sync happens after `rename_over` returns true and is skipped when the rename fails. Both orders are right.

## Focus 2: "best effort"

- **What is swallowed.**
  - `OsSync::file` discards every `sync_all` error, EIO included (`settings.rs:309-311`).
  - `OsSync::dir` discards both an `open` failure and every `sync_all` error (`settings.rs:313-321`).
  - Nothing reaches a log, a result or an error text.
- **Directory sync.** Swallowing errors here matches Core's `DirectoryCommit` exactly (`util/fs_helpers.cpp:153-162` @ v24.0.0-rc.4, which ignores both the `fopen` and the `fsync` result). A lost directory sync is also benign: after a crash the old, valid `settings.json` remains. Fine.
- **File sync.** Swallowing errors here does *not* match Core's own commit-before-rename idiom. See m-2.
- **Error texts and refusals.** None changed. `write_settings_synced` still returns `true` after a successful open, with the same open error. `write_settings_file_synced` keeps the panic text and the "Failed renaming settings file %s to %s\n" text (`args.rs:713`, `args.rs:722-726`). These match `util/settings.cpp:142-149` and `util/system.cpp:486-503`.
- **Platform behaviour.**
  - Rust's `File::sync_all` is `fsync` on Linux (stronger than Core's `fdatasync`), `F_FULLFSYNC` on macOS (the same as `FileCommit`), and `FlushFileBuffers` on Windows (the same).
  - `File::create` opens for writing, so `FlushFileBuffers` is allowed.

## Focus 3: tests and mutants

I ran these in a reflink copy, `/work/scratch/bl512-r1-opus`, with the script at `.mut/run.py`.
- Each mutant was restored from the worktree with a fresh mtime.
- Nothing ran in the live worktree, and no git command ran in the copy.
- `cargo test -p dash-args --lib` at the head: 23 passed, including `a_settings_write_syncs_the_file_then_the_directory`.

| Mutant | Result |
|---|---|
| A: no file sync (drop `sync.file`) | KILLED (`args.rs:1542`) |
| B: no directory sync | KILLED |
| C: directory sync before the rename | KILLED |
| D: file sync before `write_all` | KILLED (records 0 B) |
| F: `sync.dir(&path)` instead of the parent | KILLED |
| G: `path_tmp.parent()` instead of `path.parent()` | survives; equivalent (same directory) |
| **I: sync `state.get_data_dir(true)?` instead of `path.parent()`** | **SURVIVED** with `--lib a_settings_write`. See m-1. |
| E: `OsSync` bodies made no-ops | SURVIVED the full `cargo test -p dash-args`. Expected; D2 leaves this to the ST3b C6 rerun. See n-1. |
| H: `write_settings_file` wired to a no-op sync | SURVIVED the full `cargo test -p dash-args`. Same as E. See n-1. |

## Findings

### m-1: the test cannot tell "the settings file's directory" from "the datadir"

- **Where:** `dash-args/src/args.rs:1527-1549`, in the test; the code under test is `args.rs:730-732`.
- **Failing input:** the test writes the default `settings.json`, whose parent *is* the datadir (`<root>/regtest`). Mutant I syncs the datadir instead of `path.parent()` and passes. Run that mutant with `-settings=sub/alt.json`, or with an absolute `-settings=/elsewhere/s.json`: the rename lands in `<datadir>/sub` or `/elsewhere`, that directory is never synced, and a power cut can lose the rename. `SeenSyncs::dir` also hardcodes `"settings.json"` (`args.rs:1518-1519`), so it could not check a non-default name anyway.
- **Fix:** add a second write in the same test, or a sibling test, with `force_set_arg("-settings", "sub/alt.json")` after `create_dir(<datadir>/sub)`. Have `SeenSyncs` record the directory it is given and compare it against `path.parent()` of `get_settings_path(false, false)`, or take the expected file name as a field rather than hardcoding `settings.json`. Mutant I then dies.

### m-2: a failed file sync is swallowed, so the rename still happens and is then made durable

- **Where:** `dash-args/src/settings.rs:309-311`, and the rationale at `settings.rs:291-296` and notes D1.
- **Failing scenario:** on ext4 or xfs, `fsync` of `settings.json.tmp` returns EIO, for example after a transient writeback error. The pages are already marked clean (Linux has behaved this way since the 2018 "fsyncgate" fix), so the data may never reach disk.
  - The code goes on to `rename(2)`, and then the directory sync commits that rename durably.
  - After a power cut, `settings.json` can read as zeros, and the next start refuses with "does not contain valid JSON". That is BL-512's exact failure, now made durable.
  - Without the directory sync, the old file would at least have had a chance to survive.
- **Core comparison:** the justification "as Core ignores stream errors once the file is open" applies to `WriteSettings` itself (`util/settings.cpp:148-149`). It does not apply to the commit-then-rename idiom this fix borrows. In Core, `FileCommit` logs and returns false on any error except EINVAL (`util/fs_helpers.cpp:122-151`), and `SerializeFileDB` then does *not* rename (`addrdb.cpp:78-87`).
- **Fix:** keep every result and error text as Core's, but:
  - (a) Log the failure, as `FileCommit` does. A log line is not an error text, so "no message changes" still holds for results and refusals.
  - (b) Recommended: on a non-EINVAL file sync failure, skip `rename_over` *and* the directory sync, and still return `true`. The old, valid `settings.json` stays, the `.tmp` is left behind (Core leaves one too when the rename fails), and the init result is unchanged.
  
  If you prefer (a) alone, add a sentence to D1 saying that EIO on the file sync still renames, and why that is acceptable. To do (b), `SettingsSync::file` would need to return `bool`. The recorder in the test returns `true`, and adding a mutant "file sync fails → rename skipped" would make the new branch observable.

### n-1: the real syncs are proven only by the ST3b C6 rerun

- **Where:** `settings.rs:306-322` and `args.rs:699`.
- **Surviving mutants:** E (`OsSync` made no-ops) and H (`write_settings_file` wired to a no-op) both survive every `dash-args` test. D2 says so, which is fine. Make it a READY condition, though: READY should not be declared until the C6 double-crash rerun on the fixed binary passes the shard that failed 10 of 10 (run 1793271984-4185261, points 160 and 200), and the row should cite that rerun.
- **Optional cheap check:** a Linux-only `#[ignore]` test run under `strace -f -e trace=fsync,rename`, asserting the order fsync(tmp fd) → rename → fsync(dir fd). Without it, E and H stay reliant on C6.

### n-2: the notes' claim about Core's banlist write is wrong

- **Where:** `docs/node-program/designs/BL-512-notes.md`, D1 "Why", which says: "Core's own `peers.dat` and `banlist` writes already commit before their rename."
- **What Core does:**
  - `peers.dat` and anchors do commit before the rename, via `SerializeFileDB` (`addrdb.cpp:78-87`).
  - `banlist.json` does not: `CBanDB::Write` calls `util::WriteSettings(m_banlist_json, …)` directly on the final path, with no temporary file, no `FileCommit` and no rename (`addrdb.cpp:140-143`).
- **Fix:** say "`peers.dat` and `anchors.dat`". If rdashd's banlist store ports Core's direct write, it has the same power-cut window and arguably a worse one (a truncate in place). That may be worth its own backlog row; I did not trace the rdashd banlist writer.

### n-3: the fsync now runs under the args lock

- **Where:** `args.rs:712` (`self.lock()`), held across both syncs.
- **Scenario:** under heavy box load or a slow disk, a multi-second `fsync` blocks every concurrent `get_arg` caller. Core also writes under `cs_args`, but its write has no fsync.
- **Assessment:** today the only caller is `init.rs:164`, during startup, so this has no practical effect. The fix, if a runtime caller is ever added (for example a wallet load/unload that updates rw settings), is to snapshot `rw_settings` and release the lock before writing.

## Scope (args.rs, +64)

- **Code: 14 lines.** The doc sentence, the `write_settings_file` → `_synced` split, the `values` local (a line-length reflow only) and the directory sync block.
- **Test: 50 lines.** `SeenSyncs` and the test.
- Nothing is out of scope. The `path` parameter on `SettingsSync::file` exists only for the recorder (`OsSync` ignores it), which is acceptable for a seam.

## Core citations checked (v24.0.0-rc.4)

- `src/util/settings.cpp:142-149`: the `std::ofstream` open, write and close; no commit. This matches the notes.
- `src/util/system.cpp:486-503`: `WriteSettingsFile`, with `RenameOver` at :499 and the error text at :500. The notes' `495-499` covers the write and the rename. OK.
- `src/util/fs_helpers.cpp:122-151` (`FileCommit`) and `:153-162` (`DirectoryCommit`), for m-2.
- `src/addrdb.cpp:78-87` (`SerializeFileDB`) and `:140-143` (`CBanDB::Write`), for m-2 and n-2.

## What I ran

- `df -BG /work`: 267G free, which is at least 230G.
- `cp -a --reflink=always` of the worktree to `/work/scratch/bl512-r1-opus`, which stays in place, with `TMPDIR` set to `<copy>/.tmp`.
- `CARGO_BUILD_JOBS=8 nice -n 19 cargo test -p dash-args --lib`: 23/23 passed.
- Mutants A–I through `.mut/run.py`. A, B, C, D and F used the single test; E, G and H used the full `cargo test -p dash-args`; I used the single test. After the run, both sources in the copy are byte-identical to the worktree (`cmp`).
