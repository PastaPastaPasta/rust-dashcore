# SQLite reopen fixture

An SPV store's database as an older SQLite left it, read back by `tests/sqlite_reopen.rs`
(`an_older_sqlite_store_reopens`).

| File | SHA-256 |
|---|---|
| `kv.sqlite` | `3e3c7e9cd4d189c874df8495d98456d9576973cf98a0ee968228df9320a3708d` |
| `kv.sqlite-wal` | `8f6ea368d0a8edfd5ac4c41341b30dc8ed3f9ad38c583568942f82e108b533cc` |
| `expected.txt` | `8a55c3379df548d516229ced8726aeb27361d9926a65ffea11bacad96040f13e` |

- **Written by:** `dash-kv-sqlite` on rusqlite 0.38.0 / libsqlite3-sys 0.36.0, bundled SQLite
  3.51.1 (the database header's version number is 3051001, which the test asserts); page size
  4096, 21 pages. The WAL holds 16 frames: 3 of the last commit, then 13 stale frames (a commit
  frame among them) from before the WAL restarted, under the old salts.
- **Generator:** `write_the_fixture` in `tests/sqlite_reopen.rs`, at rust-dashcore
  `d09c82ba75a00f0b73f77570789114db460ae5e3` with that test added, rustc 1.98.1, Linux x86_64,
  2026-11-09.
- **Command:**
  `SQLITE_REOPEN_OUT=<dir> cargo test -p dash-spv --test sqlite_reopen -- --ignored write_the_fixture`,
  then the three files copied from `<dir>` into this directory. `kv.sqlite` and `expected.txt`
  come out byte for byte; the WAL's salts are random on each restart, so its bytes (salts and
  checksums) and its SHA-256 differ on every run.
- **Contents:** headers 0..48, filter headers, filters (one rewritten), blocks (one rewritten),
  metadata (one key rewritten) and the last target height (rewritten) through
  `DiskStorageManager`, then `truncate_above(40)` and `stop`; rows through the engine in each
  column family that path leaves empty (puts, an overwrite, a delete, a range delete); a last
  session with a commit rewriting a row of every column family (same length), a passive
  checkpoint and a small commit, its files copied while it was open.
- **`expected.txt`:** per column family, in schema order, its row count and the SHA-256 of its
  rows (each key and value prefixed with its length as a big-endian u32, in key order), then the
  SHA-256 of those lines.

Regenerate it only when the store's format changes on purpose; a library bump must pass against
this copy as it is.
