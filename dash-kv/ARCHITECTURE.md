# dash-kv

The storage engine contract shared by the Rust Dash node and dash-spv (SB0 §2.2, §9; SB10). The
crate is shared (L4/L5): no async runtime, no threads of its own, no allocator, and no engine
dependency. The engines live elsewhere: RocksDB in `dashd-store` (the node), SQLite in
`dash-kv-sqlite` (the SPV store).

## Modules

| Module | What it is | From |
|---|---|---|
| `engine` | `KvEngine`, `KvRead`, `KvBatch`, `Cf`, `Commit::{Buffered, Durable}` | `dashd-store` (SB10 C1, unchanged) |
| `schema`, `codec`, `table`, `error` | `DbSchema` with its `meta` record, typed `Table`s over explicit codecs, `StoreError` | `dashd-store` (C1) |
| `memory` | `MemoryEngine`, a `BTreeMap` per column family | `dashd-store` (C1) |
| `fail_stop` | the per-engine latch an engine implementation follows | `dashd-store` (C1) |
| `latch` | `GlobalFailStop` and `AbortSink`, the latch every store of a process shares (BL-109) | `dashd-chainstate` (C2) |
| `latched` | `LatchedEngine`: an engine of a shared-runtime store behind the global latch | new (C6) |
| `blocking` | `BlockingIo`, `run_blocking`, `InlineIo`: the host's bounded blocking service | new (C3) |
| `fs` | `FlatFs`, `OpenFailed`: the filesystem seam block files go through | `dashd-store` (C1) |
| `model`, `contract` (feature `model`) | the `kv_model` differential driver; the suite every engine runs (`engine_contract_tests!`) | C1, new (C4) |
| `faulty` (feature `faulty`) | ST3a's power-loss model, its explorer and checks; `micro` (MicroLog) and `spv` (the SPV store in miniature) | `dashd-store` (C1), `spv` new (C9) |

`dashd-store` and `dashd-chainstate` re-export what moved, so their users did not change.

## The contract

- A batch is atomic across the column families of one database. `Commit::Buffered` survives a
  process kill; a power loss may lose a suffix of buffered commits (SQLite) or a subset of them
  (RocksDB, DEC-62). `Commit::Durable` survives with every earlier commit, and `sync_wal` is an
  empty durable commit.
- A failed write or barrier latches the engine: every later write is `ReadOnly` until reopen;
  reads still work.
- A snapshot reads one fixed view. While a thread holds a snapshot or a live iterator of an engine
  it reads that engine only through it (SB10 Q8): SQLite's capped read pool fails the live read
  with `Other` instead of hanging, at once when the thread holds every connection, else after
  `read_wait`. A thread that holds no connection waits without a bound behind other threads'
  reads and never fails for it. Through `LatchedEngine` that failure is fatal, like any read
  error.
- Snapshots need not be `Send` (SQLite's hold a pooled connection on the taking thread).
- The contract suite runs on `MemoryEngine`, `RocksEngine` and `SqliteEngine`, including §9.4's
  interleaving (a snapshot keeps its view across later commits).

## Power-loss models (feature `faulty`)

- `FaultyEngine` recovers, per engine, `Recovery::Subset` (any subset of the pending batches,
  each atomic: RocksDB, DEC-62) or `Recovery::Prefix` (a prefix: SQLite's WAL, C8). A Durable
  return or a `sync_wal` pins what came before it either way.
- `faulty::spv` is SB0 §9.5's SPV store in miniature: one SPV database with headers, the active
  projection and two subscribers, and a wallet database of its own; checks S1–S5. Its planted
  bugs B1–B7 are witnessed on prefix engines. On subset engines the correct store breaks S1,
  which is why the SPV store needs SQLite's prefix recovery. `dash-kv-sqlite`'s
  `sqlite_wal_prefix` runs the same workload on the real engine under recorded power-loss images.
- The nightly (`contrib/node/st3a-nightly.sh`) adds `spv_sweep`, the seeded sweep at 2 × 10⁵
  cases; the PR gate runs 5,000.
