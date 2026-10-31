# dash-kv-sqlite

The SPV store's storage engine: `dash-kv`'s `KvEngine` on SQLite (SB0 §9.3, §9.4; SB10). The
crate docs (`src/lib.rs`) state the rules; this file maps them to the code and the tests.

## Layout

| File | What it holds |
|---|---|
| `lib.rs` | `SqliteEngine`, `SqliteConfig`, `SqliteSnapshot`: open and schema check, the writer, snapshots, the fail-stop latch |
| `pool.rs` | the capped read pool, its per-thread nested-take guard and bounded wait (Q8) |
| `scan.rs` | paged prefix scans (a live scan reads its first page when created) |
| `error.rs` | SQLite result codes to `ErrorKind` (`NOTADB` → `UnknownDatabase`, `CORRUPT` → `Corruption`, `FULL` → `NoSpace`, …) |
| `tests.rs` | unit tests, including the engine contract suite |
| `tests/sqlite_wal_prefix/` | the native power-loss regression: a recording VFS, power-loss images, the durability table, and the SPV workload (`spv.rs`) |

## Durability (SB0 §9.4)

| Call | SQLite | Syncs |
|---|---|---|
| `write(_, Buffered)` | `BEGIN IMMEDIATE … COMMIT` at `synchronous=NORMAL` | nothing |
| `write(_, Durable)` | the same at `FULL`; with a buffered commit pending it also bumps `_dash_kv.sync_seq`, so a commit that writes no page still writes a frame | the WAL, after its frames |
| `sync_wal` | that durable commit alone, when a buffered one is pending | the WAL once, or nothing |
| `flush_memtables` | the barrier, then a checkpoint | the WAL, then the database file |

`sqlite_wal_prefix` checks this table against a log of every write and sync. It also checks that
every power-loss image of every crash point recovers a prefix of the commits that keeps each
acknowledged durable one: a zeroed region at a commit boundary, a torn last frame, stale frames
after a WAL reset, and a lost `-shm`.

## Build

SQLite is bundled (`rusqlite` 0.38.0 / `libsqlite3-sys` 0.36.0, exact pins that move only with
Platform's, SB0 §9.3 (f)) on every target except iOS, which links the system library.
SQLite's worker threads are off (`threads=0`).

## Deviations from the plan (SB10 D-rows)

- The database is `<dir>/kv.sqlite`, and `<dir>/LOCK` is held while open.
- Snapshots and live scans are not `Send`.
- The contract runs `MODEL_SEEDS = 6` against SQLite.
- There is no `checkpoint(mode)` call yet.
- `sqlite_wal_prefix` has no automatic checkpoint at `NORMAL` and no injected fsync failure.
- A live read on a thread that already holds a pooled connection (a snapshot or a live scan,
  against `KvEngine::snapshot`'s rule, Q8) fails `Other`: at once when the thread holds every
  connection, else after `read_wait` (30 s). Through `LatchedEngine` either failure is fatal. A
  thread that holds none waits without a bound behind other threads' reads (Q8 ruling r5).
