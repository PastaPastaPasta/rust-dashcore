# ST1: UTXO-churn storage-engine bake-off (RocksDB vs MDBX, LevelDB as reference)

| | |
|---|---|
| Status | Protocol **v4** + results (§9). v1 was reviewed by an independent Opus reviewer before any run (17 findings, folded in, §10). v3 added the durability and failure experiments asked for after the A1 design review (A1r-r1 F01–F03, F12; §6a). v4 applies A1 rev1 (PLAN §8.1) and the corrections found while running (§10.2). The round-3 review's corrections (fail-stop, the routing and size of OI-1, the statistics labels) are in §3.4, §6a, §7, §9.2, §9.4, §9.5 and §10.3 |
| Date | 2026-10-08 |
| Task | PLAN ST1 (M0), design step "D (bake-off protocol)" |
| Binding inputs | A1 §2.8 (engine decision, bake-off sketch, RocksDB configuration), §2.9 (`coins` CF schema), §3.4 (coins cache, `-dbcache` split), §3.5 (flush batching and durability), §3.8 (RSS/disk), §3.11 (B1 contract) |
| Code | `dash-coreimport` (blk/rev reader, trace extractor), `dashd-store` (engine trait, RocksDB and in-memory engines), `dashd-store-bakeoff` (replay harness, MDBX and LevelDB adapters, runner) |

## 1. Question and decision rule

A1 §2.8 chose **RocksDB** for `chainstate/` and asked for a time-boxed bake-off before the coins cache (CH2) lands. The bake-off answers one question: *does the engine choice cost a tracked metric on the coins workload?*

**Decision cells:** workload W-M (§2.2) at the pressure-matched sizes **P300** and **P4096**. Every other cell is informational.

**Switch to MDBX** only if, in **both** decision cells, all of the following hold:
- **time:** the median of the paired ratios `t(MDBX) / t(RocksDB)` (pairs = the two engines' runs in the same rotation, §5) is **< 0.75**, and every paired ratio is < 0.85. `t` is `time_total_s` (§6): replay plus final flush plus compaction quiesce.
- **disk:** median `db_bytes` (after close, §6) ≤ 1.05 × RocksDB's.
- **memory:** median `rss_anon_peak` ≤ 1.05 × RocksDB's. File-backed memory (`RssFile`, cgroup `file`) is reported separately, as A1 §3.8 asks; it does not gate, because under the cap it is reclaimable page cache and fills to the cap for every engine.

**LevelDB** is dashd's engine and serves as the reference arm. It is optional under A1 rev1 (dashd in B1 is the real LevelDB baseline) and kept here because it is the cheapest arm to run. RocksDB slower than LevelDB beyond the run-to-run spread on any metric is reported as a finding with a configuration fix or an explanation. It does not change the engine choice by itself.

Anything else keeps RocksDB; ties keep RocksDB.

**The 25 % rule only picks between engines** (A1 rev1 §2.8, A1r-r1 F12). Whichever engine is kept must still pass every tracked metric against dashd under the charter's no-regression rule; if it does not, it is tuned or replaced. Here that means: a loss to LevelDB on any metric of §6 or §6a beyond the measured spread is listed in §9 as an open item with a configuration fix or an explanation. The open items pass or fail against dashd in DEC-43's low-memory side-by-side cell (§9.4).

**If W-MF overturns the provisional decision** (§2.2): CH2 is written against `KvEngine` only, so an overturn means adding an MDBX engine behind the trait (the bake-off adapter is the starting point) and re-running CH2's bench guard. The manager is told immediately, and CH2's design must not take a dependency on RocksDB-only features (`MultiGet` is behind `KvRead::multi_get`).

## 2. Workload: the real UTXO churn trace

### 2.1 Source

- Block and undo files of the reference oracles (ORACLE.md; Dash Core v24.0.0-rc.3): `/work/chains/mainnet/data/blocks/` and `/work/chains/testnet/data/testnet3/blocks/`. They are read **read-only**; the running dashd instances are never touched.
- The files are copied into `/work/bench-store/src/<net>/` (a frozen snapshot). Every block written by Core is complete in the kernel when its `AutoFile` closes, so the copy holds every block and undo record up to the oracle's connected height; the trailing record of the last file may be torn and the zero-filled preallocated tails are skipped (§2.3).
- **The canonical chain is anchored by the oracle**: the extractor asks the oracle (read-only RPC `getblockhash H`) for the hash at the chosen end height `H`, then walks `prev` links backwards through a header index built from the copied blk files. Stale blocks are never part of the trace.

### 2.2 Availability and workloads

On 2026-10-08 both oracles were still in IBD: mainnet at ~407k of 2.55M blocks (early 2016), testnet at ~393k of 1.57M. The full-chain trace does not exist yet and will not for days.

| ID | Trace | Role |
|---|---|---|
| **W-M** | mainnet, height 1 → `H_m` (the oracle's connected height at copy time, minus 100) | **decision** (provisional) |
| **W-T** | testnet, height 1 → `H_t` (same rule) | informational: post-DIP2 special transactions, testnet churn shape |
| **W-MF** | mainnet, height 1 → tip | **the final run**: the same scripts once the oracle reaches tip; it confirms or overturns the provisional decision |

### 2.3 Extraction (`dash-coreimport`)

For every block of the anchored chain in height order from height 1 (Core never connects genesis):

1. Read the block from its blk record (scanner with Core's `-reindex` semantics: magic resync, zero tails).
2. **Undo pairing.** Core writes a block's undo into `rev<nFile>` with the block's own file number, in connection order, and connects the canonical chain in height order. The extractor keeps one cursor per rev file and takes the next record that (a) passes `SHA256d(prev_hash ‖ undo bytes)`, (b) has one `CTxUndo` per non-coinbase transaction with the same input count per transaction, and (c) whose every coin equals the coin the extractor's UTXO set holds for that input (step 5). Records failing any check are undo data of stale blocks (a stale sibling has the same `prev_hash`, so the checksum alone cannot tell them apart) and are skipped and counted. If no record in the file matches, extraction aborts.
3. Parse transactions with a byte-slicing reader (Core's `UnserializeTransaction` layout including the DIP2 `vExtraPayload`). `txid = SHA256d` of the transaction's bytes as stored in the block file.
4. Emit events in Core's `ConnectBlock` order: per transaction, one **Spend(outpoint)** per input (none for the coinbase), then one **Create(outpoint, coin)** per output Core adds (`AddCoins` skips `IsUnspendable()`: `OP_RETURN`-prefixed or longer than 10,000 bytes).
5. **Coin bytes** are Core's `Coin` serialization `VARINT(height·2 + coinbase) ‖ VARINT(CompressAmount(value)) ‖ ScriptCompression(script)`, the exact bytes A1 §2.9 stores in `coins`. The self-check compares them with the undo record's raw bytes (`TxInUndoFormatter` minus the dummy version VARINT after the height code), **not** a decompress/recompress round trip.
6. **Self-check.** The extractor holds the whole UTXO set in memory; every Spend must find its coin and match the undo bytes. A Create on an existing outpoint aborts (Dash enforces BIP30 on every connected block). Limitation: coins still live at `H` are not checked against Core. Every compression path they use is checked through spent coins of the same kind; the script-kind histogram of checked coins is reported.

### 2.4 Trace file

`<net>-<H>.trace`, a versioned binary format owned by `dash-coreimport::trace`, stored on **tmpfs** (`/dev/shm/bench-store/`) so trace reads are neither disk I/O nor charged to a run's cgroup (tmpfs pages stay charged to the extractor):

- header: magic `DSTRACE1`, network magic, first and last height, the anchor hash;
- per block: `u32 height`, `u32 event count`, then events: `0x00 ‖ txid[32] ‖ u32 vout` (Spend) or `0x01 ‖ txid[32] ‖ u32 vout ‖ u16 len ‖ coin` (Create);
- trailer: `u32::MAX` sentinel, event totals.

`<net>-<H>.trace.json` holds the provenance: input files and sizes, anchor hash, the live-set curve (count and bytes every 1,000 blocks), the peak live set, skipped stale undo records, the script-kind histogram, extraction time.

### 2.5 The traces used (extracted 2026-10-08)

| | W-M | W-T |
|---|---|---|
| source | oracle mainnet blk/rev copy, 11 blk files, 453,479 blocks indexed | oracle testnet blk/rev copy, 15 blk files, 436,097 blocks indexed |
| anchor | height 453,371 `00000000000b433b5b41fc97d6395664f2c3b54bd535797b9c22f366ccf6c1df` | height 435,990 `0000009a291cedd6ac93a1122cd18a94a7a6c88ea9c337a72a6b1dac22661bd6` |
| transactions | 1,760,110 | 4,408,219 |
| spends (each checked byte for byte against Core's undo data) | 7,200,446 | 9,411,780 |
| creates / unspendable outputs skipped | 8,209,924 / 8,197 | 9,895,991 / 42,474 |
| peak live set | 1,187,399 coins (32.3 MB of coin bytes) at height 377,682 | 579,988 coins (16.0 MB) at height 426,350 |
| final live set | **1,009,478** coins | **484,211** coins |
| oracle `gettxoutsetinfo none <H>` `txouts` | **1,009,478** | **484,211** |
| stale undo records skipped / resync bytes | 0 / 0 | 0 / 0 |
| trace size / extraction time | 815 MB / 200 s | 1,008 MB / 209 s |

The final live set equals the oracle's UTXO count at the anchor height on both networks (coinstatsindex), an independent check of the extractor beyond the per-spend undo comparison. Spent-coin kinds checked: P2PKH, P2SH, P2PK compressed and (mainnet) uncompressed. Raw-script coins were created (41 on mainnet) but none was spent in the window, so that compression path is covered only by unit tests.

## 3. Replay model (identical for every engine)

### 3.1 Coins cache

- A `hashbrown` map `OutPointKey[36] → Entry { coin, DIRTY|FRESH }`, coins up to 33 bytes inline, longer ones on the heap. Core's FRESH semantics: a coin created and spent between two flushes never reaches the engine; spending a non-FRESH coin leaves a DIRTY tombstone that becomes a delete. As in Core's `AddCoins(possible_overwrite = fCoinbase)`, **coinbase outputs are never FRESH**, so a coinbase output created and spent between flushes still costs a delete.
- **Accounting** is per entry, like Core's node-based `DynamicMemoryUsage`: `ENTRY_COST = 80 B` (A1 §3.4's estimate for a P2PKH entry including control bytes and load factor) plus the heap bytes of long coins. The map is **pre-allocated once** for `budget / ENTRY_COST` entries so it never resizes (a resize would double memory and quantise the usable budget to powers of two). The table's real allocation is reported as `harness_table_bytes`.
- **Coins budget** (same for every engine): at literal sizes, A1's split `coins = dbcache − min(8 % · dbcache, 64 MiB) − 2 × 16 MiB` (A1's prefetch share is folded in because the replay does not prefetch). At pressure-matched sizes, §3.4.
- **Miss path**: a Spend whose key is not in the cache does a synchronous engine `get` (Core's `FetchCoin` on the validation thread). A miss for a key the engine does not have aborts the run (the trace is self-checked).
- No prefetching (A1's S2) and no parallel connect, so misses are fully exposed; this favours the engine with lower point-read latency (MDBX). Synchronous flushes (§3.2) favour the LSM engines. To expose both, each run also reports `time_pipelined_est_s` = Σ over flush intervals of `max(connect time of the interval, the flush that follows it)`, an estimate of A1's overlapped freeze-and-flush.

### 3.2 Flush

- **Trigger:** after a block, when the accounted size exceeds Core's LARGE threshold `max(0.9 · budget, budget − 10 MiB)` (`GetCoinsCacheSizeState`), and once at the end of the trace.
- **Synchronous** (Core's `Flush()`): the replay stops, writes every DIRTY entry **sorted by engine key**, then empties the cache. Sorting is harness work done identically for every engine; it is the best write shape for the B+tree and harmless for the LSMs. The stall of every flush is recorded.
- **Shape per engine (each engine's natural atomic unit):**
  - RocksDB and LevelDB: Core's head-blocks protocol (A1 rev1 §3.5.4): sub-batches of at most 16 MiB (`-dbbatchsize`), the first erases `meta.tip` and writes `meta.head_blocks`, the last writes `meta.tip` and erases `head_blocks`. A flush that fits in one sub-batch is one batch.
  - MDBX: **one write transaction per flush** (atomic, so no head-blocks protocol is needed). This is MDBX's native shape; committing many 16 MiB transactions in key order would rewrite the same leaf pages repeatedly.
- **Durability: every flush ends durable.** RocksDB uses A1 rev1 §3.5.4's commit kinds: the first sub-batch (the marker; in CH2 also the index and evo deltas) `Commit::Durable`, the coin sub-batches `Commit::Buffered`, the last `Commit::Durable` (two WAL fsyncs per flush; a one-batch flush has one). LevelDB: Core's unsynced coin batches, plus `sync=true` on the last one (LevelDB syncs only its current log file; logs rolled over during the flush stay unsynced, a weaker guarantee than RocksDB's). MDBX: the transaction commits under `MDBX_SAFE_NOSYNC`, then `mdbx_env_sync(force)`.
- **Quiesce and close:** after the final flush the run waits until the LSM engines have no pending or running memtable flush or compaction (RocksDB `mem-table-flush-pending`, `num-running-flushes`, `compaction-pending`, `num-running-compactions` all 0; LevelDB: `leveldb.stats` unchanged for 2 s), then closes the engine (LevelDB's close waits for a running compaction). Both are inside `time_total_s`, so compaction debt is paid there.

### 3.3 Keys and values

- Engine key: `txid[32] ‖ VARINT(vout)` (A1 §2.9; Core's `VARINT`). Value: the Core `Coin` bytes from the trace.
- The `meta` keyspace holds the tip and head-blocks markers: a column family in RocksDB, a named sub-database in MDBX, a one-byte key prefix in LevelDB (Core prefixes keys the same way).

### 3.4 Pressure matching

The prefixes have smaller UTXO sets than the full chain, so literal cache sizes would put them into an easier regime. Pressure is the fraction of the peak live set the coins cache can hold, in entries: `ρ = (coins_budget / ENTRY_COST) / peak_live_coins`.

- Full chain: **6,422,912** unspent outputs at height 2,551,683 (Blockchair API, 2026-10-08; replaced by W-MF's measured peak once it exists). At `dbcache=300`, `coins_budget` = 244 MiB → ρ₃₀₀ ≈ 0.50. At `dbcache=4096` the whole set fits (ρ₄₀₉₆ ≈ 8).
- **Caveat (round-2 review):** Core's flush threshold also lends the unused mempool allowance to the coins cache: `nTotalSpace = coins + max(maxmempool − mempool usage, 0)` (`GetCoinsCacheSizeState`). During IBD the mempool is empty, so dashd at the defaults (`-dbcache=300`, `-maxmempool=300`) flushes at ~0.9 × 544 MiB, not 0.9 × 244 MiB: its real ρ is ≈ 1.1, close to "everything fits". P300 therefore models a dbcache-300 node *without* that slack (or a low-memory configuration); a default dashd IBD sits between P300 and P4096. The harness, like A1 §3.4, does not model the slack.
- **P300** for a workload with peak live set `U`: `coins_budget = ρ₃₀₀ · U · ENTRY_COST`, rounded to 1 MiB. The engine budget `E` keeps its literal 300 MiB composition (24 MiB block cache, 2 × 16 MiB memtables).
- **P4096**: everything fits, as at 4 GiB on the full chain: `coins_budget = 1.25 · U · ENTRY_COST` (no flush before the end of the trace; a larger table would only allocate untouched memory). `E` keeps its literal 4 GiB composition (64 MiB block cache, 2 × 16 MiB memtables).
- The page-cache allowance is scaled the same way (§5).
- **Caveat (round-3 review): `E` is not scaled.** The coins budget and the page-cache allowance scale with the prefix, but `E` keeps its literal composition. At P300 that is 56 MiB, about 75 % of W-M's 75 MiB database, against about 10 % of the database at full-chain scale. A cell where the database only just fits in the memory left over sits at a cache cliff, which exaggerates how much the split of `E` (block cache against page cache) matters: OI-1 (§9.4) is the example. The W-MF rerun should scale `E` with the database or leave it uncapped.
- The results print `U`, `ρ`, both budgets and the allowance for every cell.

## 4. Engine configurations

Each engine gets its own best-practice configuration with the same memory budget `E` and the same durability. Versions are pinned and printed in every result; RocksDB's effective `OPTIONS-*` file is saved with each run. The harness and every engine use glibc `malloc`.

| | RocksDB (`dashd-store`, `rocksdb` crate) | LevelDB (`leveldb-sys`, Google LevelDB C API) | MDBX (`libmdbx` crate) |
|---|---|---|---|
| Configuration source | A1 §2.8 | Core `dbwrapper.cpp` `GetOptions` | reth/Erigon practice |
| Cache | shared LRU block cache `min(8 % · dbcache, 64 MiB)` (arm `rocksdb-bc8`: 8 MiB), `cache_index_and_filter_blocks`, L0 filter/index pinning | `block_cache = E/2` (unused for table data on 64-bit hosts, where tables are mmapped) | none of its own: OS page cache inside the cgroup cap |
| Write buffer | 2 × 16 MiB memtables (`db_write_buffer_size` caps all CFs together) | `write_buffer_size = E/4` | dirty pages until commit, at most `E` (`txn_dp_limit`), then spilled to the file |
| Point lookups | 10-bit whole-key bloom, hashed data-block index (`BinaryAndHash`, ratio 0.75), memtable whole-key bloom, **partitioned index and filters** (top level pinned, partitions at high priority; added in v4, §9.3; arm `rocksdb-nopf` is A1 rev1's whole-block configuration). Set explicitly: `optimize_for_point_lookup` would install its own block cache | 10-bit bloom | n/a |
| Compression | none on `coins` | none (Core) | n/a |
| Checksums | RocksDB defaults (`paranoid_checks`, verified reads) | `paranoid_checks = true`, `verify_checksums = true` on reads (Core) | none (MDBX has no page checksums) |
| Background | `level_compaction_dynamic_level_bytes`, `max_background_jobs = 2` (`max(2, 8 cores / 4)`), `bytes_per_sync = 1 MiB` | one compaction thread (fixed) | none |
| Other | WAL on, `atomic_flush`, `kPointInTimeRecovery`, `unordered_write = false` (A1 rev1) | — | `MDBX_NOSTICKYTHREADS`, `MDBX_NORDAHEAD`, `MDBX_SAFE_NOSYNC` + explicit sync, page size 4 KiB, growth step 16 MiB |

Notes:
- `two_write_queues` (A1 §2.8) is a `TransactionDB` option for two-phase commit and is not exposed by `rust-rocksdb`; it has no effect on plain `WriteBatch` writes and is omitted.
- `leveldb-sys` 2.0.9 builds Google LevelDB **1.22**, the release Core's vendored fork is based on; Core's changes (a Windows env, an fsync fix) do not touch the coins write or read path. Using it avoids vendoring Core's tree.
- RocksDB 11.8.1 (`librocksdb-sys` 0.19.0, `rocksdb` 0.25.0); libmdbx 0.14.4 (`libmdbx` crate 0.9.0).

## 5. Isolation and fairness on a shared box

agentbox is shared and busy (two oracles in IBD, B1's benchmark IBDs, other agents' builds and tests, all on the same virtual disk `sdb`). The protocol cannot make the box quiet; it makes runs comparable and measures the noise.

- **cgroup per run** (`sudo systemd-run --scope --uid=agent`):
  - `AllowedCPUs=24-31`, `AllowedMemoryNodes=1`: eight cores on NUMA node 1 (B1 prefers node 0, A1 §3.11). Other processes are not excluded from these cores (the oracles are unpinned), so CPU contention is recorded, not prevented.
  - `MemoryMax = harness_table_bytes + 16 B · max_entries (the flush sort index) + 64 MiB (harness overhead) + E + allowance`, where the **page-cache allowance** is `0.5 × U × 75 B` (half of the workload's peak live set at ~75 B per stored entry): the same ratio of page cache to database size for every cell. `MemorySwapMax=0` (agentbox has zram swap; swapping would hide pressure).
  - The cap bounds **anonymous memory and page cache together**, so every engine competes for the same total memory: RocksDB's and LevelDB's SST pages, MDBX's mapped pages and every engine's own caches are all inside it. A secondary **uncapped** run of W-M at P300 (`MemoryMax=infinity`) is reported for information.
  - `IOAccounting=yes`, `CPUAccounting=yes`: cgroup `io.stat` and `cpu.stat` per run.
- **No global cache drops** (they would disturb other agents). Every run starts from an empty DB directory on `/work/bench-store/db/` (XFS on `sdb`). The trace sits on tmpfs (§2.4).
- **Order:** the runs of a cell are interleaved by rotation, in rotated engine order (R-L-M, L-M-R, M-R-L, …), so drift in box load is spread across engines. Statistics pair runs by rotation (§7).
- **Repetitions:** 5 per decision cell, 3 per informational cell.
- **Noise record with every run**, written into the run's JSON: `/proc/loadavg` and `/proc/pressure/{cpu,io,memory}` before and after; the busy fraction of CPUs 24-31 and the steal time (from `/proc/stat` deltas); `sdb` utilisation and throughput (from `/proc/diskstats` deltas); the oracle services' `cpu.stat` and `io.stat` deltas; the top 5 CPU consumers at start.

## 6. Metrics (per run)

| Metric | Definition |
|---|---|
| `time_total_s` | **the decision metric**: wall time from the first event to the end of the final flush, plus the quiesce and the close (§3.2) |
| `time_replay_s`, `quiesce_s`, `close_s` | the parts |
| `connect_s`, `flush_s` | replay time outside / inside flushes |
| `time_pipelined_est_s` | §3.1 |
| `flush_stall_ms` p50 / p99 / max, count | duration of each synchronous flush |
| `miss_read_us` p50 / p99 / p99.9 / max, count | latency of every engine `get` on a cache miss (log-linear histogram) |
| `block_us` p50 / p99 / max | per-block replay time excluding flushes |
| `db_bytes` | allocated size (`st_blocks × 512`) of all files in the DB directory **after the engine is closed**; the rule uses this |
| `db_bytes_compacted` (LSMs), MDBX `live_bytes` | informational: after a full manual compaction; MDBX's pages in use, `(last_pgno + 1 − freelist) × page size` (MDBX has no in-place compaction) |
| `rss_anon_peak`, `rss_file_peak`, `vm_hwm` | maxima of `RssAnon` and `RssFile` from `/proc/self/status` (sampled every 50 ms), `VmHWM` |
| `engine_anon_peak` | `rss_anon_peak − harness_table_bytes`: the anonymous memory not owned by the coins cache |
| `cg_anon_peak`, `cg_file_peak`, `cg_mem_peak` | maxima of cgroup `memory.stat` `anon` / `file` (sampled) and `memory.peak` |
| `io_read_bytes`, `io_write_bytes`, `write_amp` | cgroup `io.stat`; `write_amp = io_write_bytes / logical bytes`, logical bytes = Σ(key + value) of puts + Σ key of deletes |
| `cpu_user_s`, `cpu_sys_s`, `cpu_per_event_ns`, `maj_faults` | `getrusage(RUSAGE_SELF)` (all threads, including the engine's); cgroup `cpu.stat` recorded alongside |

## 6a. Durability and failure experiments (added in v3, A1r-r1 F01–F03, F12)

A1 §3.5's flush ordering is being revised: Core's order is block/undo files written and fsynced, the directory committed, then the block index, then the coins (`validation.cpp` `FlushStateToDisk`, `flatfile.cpp` `FlatFileSeq::Flush`). The engine trait therefore exposes only primitives (an atomic multi-CF batch, a buffered or durable commit, explicit WAL-sync and memtable-flush barriers, snapshot reads) and no ordering. These experiments measure what each primitive costs per engine, so the revised protocol can be costed. They run in the same cgroup setup as §5 (literal 300 MiB budget), 3 repetitions, rotated engine order.

| ID | Experiment | Metrics |
|---|---|---|
| **D1** commit cost | 20,000 batches of 1 coin put, and 2,000 batches of 100 coin puts (fresh random outpoint keys, 30-byte coins), committed **buffered** and **durable** (sync per commit) into a DB pre-filled with W-M's live set at `H_m`; each phase capped at 2 minutes | per-commit latency p50/p99/max, commits/s, durable ÷ buffered ratio |
| **D2** barrier cost | after writing 16 MiB and 256 MiB of coin puts buffered: RocksDB `sync_wal` and `flush_memtables`; LevelDB a durable write of a one-key marker batch (LevelDB syncs its log on a sync write; it has no separate barrier); MDBX `env_sync(force)`. Reference: `fsync` of a freshly written 128 MiB flat file plus `fsync` of its directory (the block-file barrier in Core's order) | barrier latency; bytes written by the barrier (`io.stat`) |
| **D3** pending-batch memory | build, without committing, one batch the size of the largest flush of the W-M literal-300 replay (67 MB) and one of 256 MiB of logical data; then commit it (building capped at 2 minutes) | `RssAnon` delta per logical byte while pending and peak during the commit (memtable inserts, MDBX dirty pages); the harness cache is released first so the delta is the engine's |
| **D4** disk full | each engine on a 512 MiB XFS loop image (XFS's minimum size) with a ballast file on the same image (64 MiB in the bake-off runs; 128 MiB since the round-3 review, more than RocksDB's 64 MiB recovery reserve), writing 4 MiB batches buffered with a durable barrier every 8 batches until a write fails | the error returned and how it maps to `StoreError` (kind `NoSpace` expected); whether the failing batch is visible after reopen (must not be); whether the engine accepts writes after space is freed without a reopen (it must not: fail-stop, A1 rev1 §3.5.6). The retry waits 12 s after the ballast is deleted, because RocksDB re-checks free space every 5 s and resumes writes by itself once its reserve is free (§9.5); whether reopen recovers exactly the last durable commit (checked against the replay's own record of acknowledged commits) |

Out of scope here and owned by ST3a/ST3b (A1 rev1 §3.5.8): power-loss tests that lose or reorder unsynced writes. D4 covers only the error path the engine reports. D1–D3 run on the database a W-M literal-300 replay leaves behind (its live set at `H_m`), in a new cgroup scope with 2 GiB more headroom for D3's 256 MiB batch.

## 7. Statistics and reporting

- Per cell: median, min and max of every metric, and the spread `max/min − 1` of `time_total_s`.
- **Paired ratios:** in each rotation, `t(engine) / t(RocksDB)`. The paired ratios cancel load drift between rotations. Each comparison reports the median paired ratio and the range of the pairs. (The bake-off runs reported a bootstrap "95 % interval" of the median; with 2–5 pairs its 2.5 and 97.5 percentiles are just the smallest and largest pair, so §9 now calls it the range, round-3 review m7.) "Every pair on one side of 1" is a sign test, two-sided p = 0.0625 at 5 pairs (the smallest possible), and assumes independent pairs, which load drift on a shared box weakens. `run.py summary` now reports the range and that test. The decision rule (§1) uses the paired ratios.
- `cpu_per_event_ns` is the less noise-sensitive secondary for time; disagreement between it and wall time is called out.
- Results: `/work/bench-store/results/<cell>/<run>.json`, summarised in §9 with the harness commit, engine versions, trace provenance and the medians of the noise record.

**Time box.** Decision cells: 2 cells × 3 engines × 5 rotations = 30 runs. Informational: W-M literal 300/4096 and W-T P300/P4096, 4 cells × 3 engines × 3 = 36 runs, plus 3 uncapped runs. The pilot (one run per engine of W-M P300) sets the expected runtime; if the whole matrix would exceed ~8 h of wall time, the informational cells drop to 2 rotations and W-T to P300 only.

## 8. What the bake-off does not cover

- A1's asynchronous freeze-and-flush, prefetching (S2 `MultiGet`) and concurrent readers: they change how much engine latency the pipeline hides (estimated by `time_pipelined_est_s`), not which engine is cheaper. If MDBX ever wins the decision cells, the switch must first be confirmed with a sorted-`MultiGet` prefetch variant.
- The index DBs (`txindex`, `addressindex`, …): different access shapes; RocksDB's compression helps them and MDBX's page fill hurts them, so they would not move the decision towards MDBX.
- Crash behaviour: CH2's power-loss harness.

- A1r-r1 F12 also asks for a multi-CF flush/reorg workload with background compaction and concurrent readers. The coins CF dominates chainstate bytes and churn; the other CFs (block index, evo diffs) add write volume of a few percent. The replay reports `time_pipelined_est_s`, but concurrent readers and a full multi-CF chainstate workload need CH2's real flush pipeline and are listed as a CH2 bench-guard item in §9.

## 9. Results

Runs of 2026-10-08 on agentbox, harness at branch `node/st1-store-bakeoff` (the commit that adds this section), RocksDB 11.8.1 with `dashd-store`'s configuration (§4), LevelDB 1.22, libmdbx 0.14.4. Raw per-run JSON (metrics, noise record, effective RocksDB `OPTIONS`) is under `/work/bench-store/results/`; `run.py summary` and `run.py report` regenerate the tables below. The traces are archived in `/work/bench-store/traces/` (copy them back to `/dev/shm/bench-store/traces/` before rerunning).

### 9.1 Decision

**Keep RocksDB.** MDBX fails the switch rule in both decision cells on time and on disk.

| Decision cell | MDBX / RocksDB time, paired median (range of 5 pairs) | needed to switch | MDBX / RocksDB disk (file; live) | MDBX / RocksDB RssAnon |
|---|---|---|---|---|
| W-M P300 | **1.04** (0.68–1.63) | < 0.75 | **4.3×** (320 vs 75 MiB; 178 vs 50 MiB) | 0.70 (166 vs 238 MiB) |
| W-M P4096 | **2.03** (1.11–6.43) | < 0.75 | **6.7×** (448 vs 67 MiB; 195 vs 50 MiB) | 0.87 (302 vs 346 MiB) |

- MDBX has the best point reads, as A1 predicted (miss p50 2.0–3.1 µs against RocksDB's 8–10 µs), and the lowest CPU per event at P300 (0.74×). It loses on everything that writes: a random-key flush rewrites most leaf pages (842 MiB written per W-M replay against RocksDB's 437 MiB; 32× write amplification in D2), its file is 2.8–6.7× RocksDB's, and in the capped decision cells its flush stalls are the longest (p99 21–39 s).
- The decision holds for the informational cells (2–3 pairs each, so their ranges are indicative only): W-T P300 MDBX/RocksDB 2.04, W-T P4096 0.93, W-M literal 4096 0.66 (MDBX faster in a regime with almost no misses), uncapped W-M P300 0.73; MDBX's disk is 2.8–6.7× RocksDB's in every cell.
- **The rule only picks between engines** (§1, A1 rev1 §2.8). RocksDB must still pass every tracked metric against dashd, and against LevelDB it does not yet in the tightest cell (which is tighter than a default dashd IBD, §3.4): see §9.4. That is a configuration task for CH2's budget split, not a reason to switch engine: LevelDB itself is not a candidate (no column families, no atomic multi-CF batch, single compaction thread, maintenance-only upstream).
- **Provisional until W-MF** (§2.2). When the mainnet oracle reaches the tip, rerun with the same scripts: `dashd-store-bakeoff extract --net mainnet --blocks <copy> --anchor <tip-100 hash> --out /dev/shm/bench-store/traces/mainnet-<H>.trace`, then `run.py cells wm-p300,wm-p4096 --rotations 5 --engines rocksdb,mdbx,leveldb`. The disk gap (3.6–6.7×) is the margin that would have to close for MDBX, which a larger tree only widens (page fill does not improve with size).

### 9.2 Measurement quality

The box was saturated during every run: load average 67–119, CPUs 24–31 96–100 % busy (other processes are not excluded from them), `sdb` at 60–99 % utilisation from the oracles' IBD and other agents. Same-configuration wall times spread up to 620 % within a cell (e.g. RocksDB-bc8 at W-M P300: 74–315 s); only a few engine/cell combinations stayed under 15 %. Three consequences:
- Wall-time comparisons use paired ratios and the range of the pairs (§7); most ranges are wide. A difference is called real only when every pair lies on the same side of 1, which at 5 pairs is a sign test with p = 0.0625, not a 95 % interval. With 2–3 pairs it means even less, so informational-cell ratios are indicative. In the decision cells all four arms ran in the same pass, rotation by rotation, except LevelDB's fifth W-M P300 rotation, which ran after an orchestrator restart.
- CPU per event (`getrusage` user + system time of the whole process / trace events) is far more stable (spread 3–20 %) and is reported next to every time ratio. Bytes read from disk per run (cgroup `io.stat`) are deterministic enough to diagnose memory pressure.
- Five runs were OOM-killed under the hard cap: all three engines' first W-T P300 rotation and one RocksDB W-M literal-4096 rotation, plus one MDBX micro replay. The replay failures are counted in the summary tables (`failed:`); the micro replay is not a summary cell. No decision-cell run failed. The kills came with only ~110 MiB anonymous memory against a 205–239 MiB cap: the cap also charges dirty and writeback page cache, which drains slowly on the saturated disk. W-T therefore uses a 64 MiB floor on the page-cache allowance (§5).

### 9.3 Configuration findings applied to `dashd-store`

- **Partitioned index and filters** (now the default). With A1 rev1's whole per-file index/filter blocks in a 24 MiB shared cache, uncapped W-M P300 cost 5,349 ns of CPU per event and miss p99 307 µs **with no disk reads**; partitioned index/filters cut that to 2,320 ns and 37 µs (CPU 0.43×, the stable metric; wall time 0.76× over 2 pairs, range includes 1; `rocksdb-nopf` in the uncapped table). Diagnostic runs before the harness fixes showed the same direction (115 → 29 s).
- **Zero-copy batches** (§10.2): the old batch encoding doubled every pending batch.
- **`atomic_flush`, point-in-time WAL recovery, paranoid checks** per A1 rev1.

### 9.4 Open items: RocksDB against LevelDB (dashd's engine), for CH2 and DEC-43's low-memory gate

| # | Cell | Finding | Evidence | Candidate fix (CH2 decides with node-bench against dashd) |
|---|---|---|---|---|
| OI-1 | W-M P300 (tight memory; tighter than a default dashd IBD, §3.4) | RocksDB slower than LevelDB in every pair: LevelDB/RocksDB 0.17–0.61, median 0.27, so **at least 1.6×**. The median's 3.7× is not the size to expect: the rest comes from disk contention (60–99 % utilisation inflates every extra read) and from an engine budget `E` that is not scaled to the prefix, which puts this cell at a cache cliff (§3.4). The mechanism, double caching under one memory cap, is real and does scale. Miss p99 2.65 ms vs 25 µs | RocksDB reads 410–791 MiB from disk per run, LevelDB 35–227 MiB. RssAnon 238 vs 192 MiB: under one total-memory cap, RocksDB's anonymous block cache and memtables leave too little page cache for the 75 MiB database. LevelDB on 64-bit mmaps its tables and does not put their blocks in its block cache, so its `E/2` cache stays empty and the memory goes to page cache. | Spend less of `-dbcache` on the block cache when the OS page cache holds the data (arm `rocksdb-bc8`: reads 321–535 MiB, miss p99 0.9 ms, paired time 0.60, range 0.45–3.13). DEC-43 (b): size the memtables like Core (≈ `nCoinDBCache/4`), then choose between direct I/O with a larger block cache (no double caching) and a small block cache that leans on page cache; count page cache in the A1 §3.4 split; re-measure at W-MF with `E` scaled. |
| OI-2 | W-M P300 | CPU per event 1.30× LevelDB's (LevelDB/RocksDB 0.77, range 0.67–0.80) | stable across 5 rotations | read path: block-cache shards, `optimize_filters_for_memory`, jemalloc (PF2); re-measure at W-MF |
| OI-3 | W-M P4096 (everything fits) | No significant time difference (LevelDB/RocksDB 0.77, range 0.46–1.82); CPU 0.89× (range 0.84–0.92) | — | track in CH2 |
| OI-4 | all cells | RocksDB writes **less** than LevelDB (437 vs 728 MiB at W-M P300; 337 vs 423 at P4096) and is the same size on disk (75 vs 81 MiB; 50 vs 49 compacted) | — | none (a RocksDB advantage) |
| OI-5 | W-T | No significant difference (LevelDB/RocksDB 1.31 at P300, 0.93 at P4096) | — | — |
| OI-6 | all cells (added after the round-3 review) | RocksDB's `RssAnon` is higher in every cell, with ranges that do not overlap: W-M P300 228–265 vs 189–202 MiB, P4096 337–348 vs 296–322, W-T P4096 221–222 vs 179–187 (§9.6) | RocksDB's block cache is anonymous memory; LevelDB mmaps its tables, so the same data shows up as `RssFile` (80–98 MiB at W-M P300 against RocksDB's 12). Total `RssAnon + RssFile` is lower for RocksDB (250 vs 281 MiB at W-M P300). `peak_rss_mib` and `idle_rss_mib` are charter metrics, so this counts against dashd | the same split decision as OI-1 (direct I/O, block-cache size); measure `peak_rss_mib` against dashd in the DEC-43 cell and at the defaults |
| OI-7 | W-M P300, uncapped W-M P300 (added after the round-3 review) | flush-stall p99 worse than LevelDB's: 12.5 s vs 2.7 s at P300, 40 vs 16 s uncapped; the ranges overlap (3.1–52.9 vs 2.0–36.2 s at P300), so this is not significant by §1's rule, but flush stall is the metric A1 asked for | likely memtable write stalls while a 16 MiB sub-batch flush meets 2 × 16 MiB memtables | CH2: memtable count and size for the flush shape; the harness now records RocksDB's stall statistics (`rocksdb.dbstats` and `cfstats` stall lines, `is-write-stopped`) for the W-MF rerun |

Per A1 rev1 §2.8 these are tuning items, not grounds to change the engine (DEC-43 (a)). They pass or fail against dashd in **DEC-43's low-memory side-by-side cell**: node-bench runs dashd and the Rust node with the same flags at `-dbcache=300`, with no mempool allowance lent to the coins cache (a full mempool, or `-maxmempool` set), and CH2's merge gate and the M3 exit compare the two (PLAN CH2 BG, DEC-43 (c)). The cell joins B1b's list, and node-bench gates it once B1c lands (DEC-31). Until then nothing else covers this regime. Every other planned dashd gate runs Core's defaults, where the empty mempool's 300 MiB goes to the coins cache (ρ ≈ 1.1, §3.4), and CH2's `reindex-chainstate` guard compares against the previous head, not against dashd. The regime is not exotic: a synced node with a full mempool, or any low-memory host, runs in it, and block-connect latency at the tip is where it would show.

### 9.5 Durability and failure costs (§6a)

D1–D3 ran on the database a W-M literal-300 replay leaves (its live set at height 453,371), RocksDB and LevelDB 2 rotations each, MDBX 1 (its second replay was OOM-killed; its first unbounded D1 run was stopped after 30 minutes and ~57 GB of writes, see below). Each D1 phase was later capped at 2 minutes; `n` is the number of commits measured.

**D1: commit latency** (p50, p99; one put of a 30-byte coin per commit, or 100)

| | 1 put buffered | 1 put durable | 100 puts buffered | 100 puts durable |
|---|---|---|---|---|
| RocksDB | 3.8–4.0 µs, 15 µs | 3.3–4.2 ms, 41–44 ms | 0.30–0.37 ms, 0.5–0.7 ms | 4.8–5.1 ms, 51–56 ms |
| LevelDB | 2.3–4.3 µs, 11–17 µs | 3.8–4.1 ms, 15–66 ms | 0.26–0.30 ms, 0.7–3.2 ms | 3.0 ms, 14 ms (one rotation stalled on the disk: 94 ms, 58 s) |
| MDBX | 133 µs, 8.1 ms | 8.0 ms, 407 ms | 12.6 ms, 2.1 s | 453 ms, 5.6 s |

A durable commit costs about 1,000× a buffered one on RocksDB and LevelDB: one WAL fsync on this virtual disk, 3–5 ms at p50. A1 rev1 §3.5.4's two Durable commits per flush therefore add ~10–100 ms per flush on this disk (the harness's first Durable sub-batch also carries up to 16 MiB of coins, which A1's B1 does not; D2's `sync_wal` after 16 MiB takes 8–88 ms). Flushes are minutes apart. MDBX's durable commit forces a steady sync that writes ~2.6 MB for a single put (57 GB written in the stopped unbounded run, ~4 commits/s). Its buffered commits go through the `libmdbx` crate's write-transaction thread and rewrite leaf pages, so they are also slow. D1 ran before the fail-stop latch (§9.5, D4): each write now also reads `rocksdb.background-errors` (one DB-mutex acquisition), about 170 ns, roughly 5 % of a one-put buffered commit and negligible for sub-batches and durable commits (measured on agentbox, 3 × 200,000 commits).

**D2: barrier latency after buffered writes**

| | after 16 MiB | after 256 MiB | written by the barrier |
|---|---|---|---|
| RocksDB `sync_wal` | 8–88 ms | 30–107 ms | 1.4–14 MiB |
| RocksDB `flush_memtables` | 0.16–0.23 s | 0.46–0.82 s | 10–65 MiB |
| LevelDB sync write | 4–888 ms | 0.13–2.7 s | 0.1–19 MiB |
| MDBX `env_sync` | 66 s | 6.8 s | 103–161 MiB |
| flat file, 128 MiB `fdatasync` / directory `fsync` (Core's block-file barrier) | 0.84–2.2 s / 30 µs–9 ms | | 128–172 MiB |

Writing 256 MiB of random coin puts in 4 MiB commits: RocksDB 25 s and 1.39 GB written (5.2×). MDBX managed 14 commits (59 MB) in the 2-minute cap and wrote 1.88 GB (32×).

**D3: memory of a pending batch** (A1r-r1 F12)

| | anonymous memory while pending, per logical byte | peak while committing, 67 MB batch (largest W-M literal-300 flush) | peak while committing, 256 MiB batch |
|---|---|---|---|
| RocksDB | 1.25–1.46 | +169–170 MiB | +571–576 MiB |
| LevelDB | 1.06–1.25 | +149–168 MiB | +574 MiB |
| MDBX | 0.41–0.78 | +50 MiB | +52 MiB (bounded by `txn_dp_limit` = `E`, the rest spilled) |

An LSM batch costs ~1.1–1.5× its logical size while pending and ~2.2–2.7× at its peak, when the memtable copy coexists with the batch. A1 §3.5.4's 16 MiB sub-batches bound this at ~45 MiB per sub-batch; the coins cache budget must reserve it (CH2's budget invariant). `KvBatch::size_bytes` is a lower bound on a batch's memory (a growing `WriteBatch` holds up to twice its size in capacity); CH2 creates sub-batches with `KvEngine::batch_with_capacity` so that the counting allocator sees what the budget assumes.

**D4: disk full** (512 MiB XFS loop image; 4 MiB buffered batches, a durable one every 8)

| | error | failing batch after reopen | torn batches | writes after freeing space, without reopen | after reopen |
|---|---|---|---|---|---|
| RocksDB, bake-off run (64 MiB ballast, retried at once) | `StoreError` kind `NoSpace` ("No space left on device" while appending an SST) | absent | 0 | refused, but only because the retry came too early (see the correction below) | every acknowledged batch present (47) |
| RocksDB, fail-stop engine (round-3 rerun: 128 MiB ballast, retried 12 s after freeing it, 232 MiB free) | kind `NoSpace` ("No space left on device" while appending to the WAL) | absent | 0 | **refused**, kind `ReadOnly`, quoting the first `NoSpace`; RocksDB's own LOG shows that RocksDB had resumed by itself | every acknowledged batch present (37, the last durable one at 32) |
| LevelDB (retried at once) | "IO error: … No space left on device" | absent | 0 | refused (LevelDB keeps a background error until it is reopened) | every acknowledged batch present (57) |
| MDBX (retried at once) | `EIO` ("Input/output error"), **not** ENOSPC | absent | 0 | accepted | every acknowledged batch present (22) |

**Correction (round-3 review, M1).** The bake-off run's "refused" for RocksDB was an artifact of timing, and v4 of this section drew the wrong conclusion from it ("the engine refuses further writes until reopened").
- RocksDB 11.8.1 treats a `NoSpace` from a WAL write, a flush or a MANIFEST write as a hard error. With a single DB path, `DB::Open` enables auto-recovery: the `SstFileManager` re-checks free space every 5 s and resumes writes once free space covers its reserve: the largest column family's write buffer, 64 MiB here, which comes from RocksDB's unconfigured `default` column family, not from `dashd-store`'s 16 MiB memtables (`db/error_handler.cc` `RecoverFromNoSpace`, `db_impl_open.cc`).
- D4 retried right after deleting a 64 MiB ballast: inside the first 5-second poll, and possibly below the reserve. The reviewer's probe on a 200 MiB tmpfs had a Durable write accepted 6.07 s after space was freed, with no reopen.
- So RocksDB on its own does not fail-stop, and A1 rev1 §3.5.6's premise that it "may go read-only" is false. A later commit could land after one whose outcome is unknown, from another thread or at shutdown: the hole in the commit sequence that invariant I5 forbids.
- `dashd-store` now enforces fail-stop itself. `RocksEngine` and `MemoryEngine` latch the first write, barrier or background error, including a background flush error RocksDB has already recovered from, and refuse every write and barrier that starts later with `ErrorKind::ReadOnly` until the engine is reopened (the `KvEngine` documentation is the contract). For a background error RocksDB did not stop for, the first error's kind is read back from RocksDB's LOG, so a compaction or flush that ran out of space still reports `NoSpace`. The tests are in `dashd-store/tests/fail_stop.rs`: a portable flush-error test, the reviewer's fill, free and wait-12-s scenario, and the recovered background error (the last two need a small dedicated filesystem; without the latch the write 12 s after freeing space is accepted).
- D4 now waits 12 s and uses a 128 MiB ballast. The second RocksDB row is that harness on the fail-stop engine: one run on a scratch image, not part of the matrix, and not rerun for the other engines.

With the latch, RocksDB meets A1 rev1 §3.5.6's fail-stop rule: the first error has kind `NoSpace`, and the engine refuses all further writes until it is reopened. MDBX would need its EIO disambiguated, and it stored 88 MB of batches where RocksDB stored 188 MB.

### 9.6 All cells

Each table gives median [min–max] over the runs. `time` is `time_total_s` (replay, final flush, quiesce and close). `compacted/live` is the LSM size after a full compaction, or MDBX's live pages. `written` is cgroup `io.stat` write bytes. Paired ratios are per rotation against `rocksdb`, the `dashd-store` default; `rocksdb-bc8` is the same configuration with an 8 MiB block cache, and `rocksdb-nopf` the A1 rev1 whole-block index/filters.

**wm-l4096** (median [min–max]; n runs; failed: {'rocksdb': 1})

| metric | leveldb | mdbx | rocksdb |
|---|---|---|---|
| n | 3 | 3 | 2 |
| time (s) | 26.8 [21.7–36.6] | 22.9 [21.9–24.8] | 44.6 [26.1–63.1] |
| CPU/event (ns) | 1379 [1342–1499] | 1323 [1313–1349] | 1464 [1392–1535] |
| flush p99 (ms) | 7635 [5653–18846] | 8302 [6838–9134] | 7539 [6972–8106] |
| miss p50 (µs) | 5.6 [5.5–6.5] | 2.2 [2.1–2.5] | 8.2 [7.6–8.8] |
| miss p99 (µs) | 21.8 [21.2–26.1] | 7.9 [7.5–9.7] | 35.8 [32.8–38.9] |
| size (MiB) | 82 [82–82] | 288 [288–288] | 101 [101–101] |
| compacted/live (MiB) | 49 [49–49] | 228 [228–228] | 50 [50–50] |
| RssAnon (MiB) | 318 [298–318] | 302 [302–302] | 371 [370–372] |
| RssFile (MiB) | 54 [50–68] | 110 [77–110] | 12 [12–12] |
| written (MiB) | 246 [244–247] | 273 [273–273] | 220 [220–220] |
| time spread | 69 % | 13 % | 142 % |
| CPUs 24-31 busy | 100 % | 99 % | 100 % |

- leveldb / rocksdb, 2 pairs by rotation: time 0.68 (range 0.34–1.03); CPU per event 0.98

- mdbx / rocksdb, 2 pairs by rotation: time 0.66 (range 0.36–0.95); CPU per event 0.91

**wm-p300** (median [min–max]; n runs; failed: none)

| metric | leveldb | mdbx | rocksdb | rocksdb-bc8 |
|---|---|---|---|---|
| n | 5 | 5 | 5 | 5 |
| time (s) | 42.3 [33.0–77.7] | 158.4 [107.0–211.9] | 127.1 [100.6–234.3] | 110.3 [74.3–314.8] |
| CPU/event (ns) | 2308 [2244–2322] | 2236 [2153–2314] | 3007 [2895–3457] | 2951 [2699–2997] |
| flush p99 (ms) | 2655 [1990–36183] | 39280 [11335–78413] | 12461 [3084–52929] | 4986 [2581–148368] |
| miss p50 (µs) | 6.1 [6.1–6.5] | 2.5 [2.4–2.6] | 9.5 [9.0–9.7] | 11.1 [9.5–11.6] |
| miss p99 (µs) | 24.8 [23.6–26.1] | 639 [508–2425] | 2654 [1114–5046] | 893 [590–2327] |
| size (MiB) | 81 [74–83] | 320 [320–320] | 75 [75–75] | 75 [75–91] |
| compacted/live (MiB) | 49 [49–49] | 178 [178–178] | 50 [50–50] | 50 [50–50] |
| RssAnon (MiB) | 192 [189–202] | 166 [166–166] | 238 [228–265] | 224 [219–228] |
| RssFile (MiB) | 89 [80–98] | 124 [123–124] | 12 [12–12] | 12 [12–12] |
| written (MiB) | 728 [695–763] | 842 [842–842] | 437 [437–438] | 436 [427–439] |
| time spread | 135 % | 98 % | 133 % | 323 % |
| CPUs 24-31 busy | 97 % | 98 % | 100 % | 99 % |

- leveldb / rocksdb, 5 pairs by rotation: time 0.27 (range 0.17–0.61); CPU per event 0.77

- mdbx / rocksdb, 5 pairs by rotation: time 1.04 (range 0.68–1.63); CPU per event 0.74

- rocksdb-bc8 / rocksdb, 5 pairs by rotation: time 0.60 (range 0.45–3.13); CPU per event 0.95

**wm-p300u** (median [min–max]; n runs; failed: none)

| metric | leveldb | mdbx | rocksdb | rocksdb-nopf |
|---|---|---|---|---|
| n | 2 | 2 | 2 | 2 |
| time (s) | 49.5 [40.2–58.8] | 55.4 [35.8–75.0] | 84.1 [67.9–100.2] | 102.9 [87.2–118.5] |
| CPU/event (ns) | 2222 [2121–2322] | 1603 [1577–1629] | 2320 [2269–2371] | 5349 [5016–5682] |
| flush p99 (ms) | 16051 [8508–23594] | 26139 [5253–47025] | 40157 [33181–47132] | 13177 [2388–23965] |
| miss p50 (µs) | 6.1 [5.6–6.6] | 2.0 [2.0–2.0] | 8.2 [7.5–8.8] | 7.0 [6.7–7.4] |
| miss p99 (µs) | 22.1 [20.2–24.1] | 4.6 [4.5–4.7] | 36.9 [35.3–38.4] | 307 [303–311] |
| size (MiB) | 74 [74–74] | 320 [320–320] | 75 [75–75] | 75 [75–75] |
| compacted/live (MiB) | 49 [49–49] | 178 [178–178] | 50 [50–50] | 50 [50–50] |
| RssAnon (MiB) | 194 [188–200] | 166 [166–166] | 254 [246–261] | 245 [244–245] |
| RssFile (MiB) | 91 [86–95] | 329 [329–329] | 12 [12–12] | 12 [12–12] |
| written (MiB) | 603 [595–611] | 842 [842–842] | 437 [437–437] | 437 [436–438] |
| time spread | 46 % | 110 % | 48 % | 36 % |
| CPUs 24-31 busy | 99 % | 99 % | 99 % | 100 % |

- leveldb / rocksdb, 2 pairs by rotation: time 0.59 (range 0.59–0.59); CPU per event 0.96

- mdbx / rocksdb, 2 pairs by rotation: time 0.73 (range 0.36–1.11); CPU per event 0.69

- rocksdb-nopf / rocksdb, 2 pairs by rotation: time 1.31 (range 0.87–1.75); CPU per event 2.31

**wm-p4096** (median [min–max]; n runs; failed: none)

| metric | leveldb | mdbx | rocksdb | rocksdb-bc8 |
|---|---|---|---|---|
| n | 5 | 5 | 5 | 5 |
| time (s) | 32.9 [25.7–121.5] | 135.6 [118.6–216.0] | 58.5 [33.6–121.8] | 32.8 [30.7–111.0] |
| CPU/event (ns) | 1776 [1579–1901] | 2143 [2067–2262] | 1986 [1841–2058] | 2052 [1940–2134] |
| flush p99 (ms) | 6486 [4624–41148] | 21523 [18406–26109] | 11860 [7433–76939] | 4910 [4692–60901] |
| miss p50 (µs) | 6.1 [4.8–6.2] | 3.1 [3.1–3.3] | 8.2 [7.7–9.0] | 10.9 [10.4–12.9] |
| miss p99 (µs) | 20.2 [16.1–21.5] | 3932 [1982–5767] | 156 [48.6–909] | 50.7 [49.7–56.8] |
| size (MiB) | 87 [83–105] | 448 [448–448] | 67 [67–67] | 67 [67–67] |
| compacted/live (MiB) | 49 [49–49] | 195 [195–195] | 50 [50–50] | 50 [50–50] |
| RssAnon (MiB) | 308 [296–322] | 302 [302–302] | 346 [337–348] | 315 [309–322] |
| RssFile (MiB) | 88 [85–130] | 130 [130–131] | 12 [12–12] | 12 [12–12] |
| written (MiB) | 423 [336–441] | 480 [480–480] | 337 [336–338] | 338 [337–339] |
| time spread | 372 % | 82 % | 263 % | 261 % |
| CPUs 24-31 busy | 99 % | 100 % | 96 % | 100 % |

- leveldb / rocksdb, 5 pairs by rotation: time 0.77 (range 0.46–1.82); CPU per event 0.89

- mdbx / rocksdb, 5 pairs by rotation: time 2.03 (range 1.11–6.43); CPU per event 1.06

- rocksdb-bc8 / rocksdb, 5 pairs by rotation: time 0.84 (range 0.53–0.96); CPU per event 1.04

**wt-p300** (median [min–max]; n runs; failed: {'rocksdb': 1, 'mdbx': 1, 'leveldb': 1})

| metric | leveldb | mdbx | rocksdb |
|---|---|---|---|
| n | 2 | 2 | 2 |
| time (s) | 34.0 [22.1–45.9] | 61.4 [59.8–63.1] | 36.1 [21.1–51.1] |
| CPU/event (ns) | 1116 [1067–1165] | 939 [928–950] | 1132 [1112–1151] |
| flush p99 (ms) | 11671 [927–22415] | 13861 [12196–15526] | 8944 [1218–16671] |
| miss p50 (µs) | 5.0 [4.5–5.4] | 1.9 [1.9–2.0] | 5.0 [4.9–5.1] |
| miss p99 (µs) | 22.9 [20.5–25.3] | 77.7 [7.9–147] | 25.5 [24.1–26.9] |
| size (MiB) | 56 [55–57] | 144 [144–144] | 52 [52–52] |
| compacted/live (MiB) | 30 [30–30] | 72 [72–72] | 31 [31–31] |
| RssAnon (MiB) | 113 [111–115] | 113 [113–113] | 150 [150–151] |
| RssFile (MiB) | 59 [54–64] | 136 [134–137] | 12 [12–12] |
| written (MiB) | 411 [410–412] | 543 [543–543] | 271 [270–271] |
| time spread | 108 % | 6 % | 143 % |
| CPUs 24-31 busy | 100 % | 100 % | 100 % |

- leveldb / rocksdb, 2 pairs by rotation: time 1.31 (range 0.43–2.18); CPU per event 0.99

- mdbx / rocksdb, 2 pairs by rotation: time 2.04 (range 1.23–2.84); CPU per event 0.83

**wt-p4096** (median [min–max]; n runs; failed: none)

| metric | leveldb | mdbx | rocksdb |
|---|---|---|---|
| n | 3 | 3 | 3 |
| time (s) | 18.0 [17.5–18.7] | 17.5 [16.7–22.2] | 18.8 [16.6–119.9] |
| CPU/event (ns) | 824 [793–875] | 766 [753–785] | 1000 [882–1057] |
| flush p99 (ms) | 2306 [2026–3075] | 3232 [2877–7062] | 2066 [1970–100530] |
| miss p50 (µs) | 4.7 [4.3–4.8] | 1.8 [1.8–1.9] | 7.2 [5.8–7.4] |
| miss p99 (µs) | 15.6 [14.6–18.4] | 4.9 [4.5–5.3] | 26.6 [23.0–39.9] |
| size (MiB) | 59 [58–59] | 192 [192–192] | 52 [52–52] |
| compacted/live (MiB) | 30 [30–30] | 87 [87–87] | 31 [31–31] |
| RssAnon (MiB) | 180 [179–187] | 203 [203–203] | 222 [221–222] |
| RssFile (MiB) | 46 [46–48] | 129 [128–130] | 12 [12–12] |
| written (MiB) | 221 [220–221] | 213 [213–213] | 185 [185–185] |
| time spread | 7 % | 33 % | 624 % |
| CPUs 24-31 busy | 100 % | 100 % | 100 % |

- leveldb / rocksdb, 3 pairs by rotation: time 0.93 (range 0.16–1.08); CPU per event 0.83

- mdbx / rocksdb, 3 pairs by rotation: time 0.93 (range 0.19–1.01); CPU per event 0.78

`wm-l300` holds only the replays that prepared the D1–D3 databases (§9.5); its replay numbers are in the per-run JSON.

## 10. Review of v1 and what changed

An independent reviewer (Opus) read v1 before any run. Blockers: undo pairing by checksum alone can take a stale sibling's undo (→ shape and content checks, §2.3); pressure matching broke at small sizes and kept a fixed page-cache headroom (→ §3.4, scaled allowance §5); the memory criterion was ill-defined under a hard cap (→ RssAnon gates, file-backed memory reported, §1); the decision cells were ambiguous (→ P300/P4096 of W-M, §1). Should-fix items adopted: MDBX's native one-transaction flush and sorted writes (§3.2), compaction quiesce inside the decision time (§3.2), the pipelined estimate (§3.1), explicit point-lookup options and Core's LevelDB checksums (§4), a pre-allocated cache map with per-entry accounting (§3.1), entry-count pressure (§3.4), noise records for the oracles and paired-ratio statistics with 5 rotations (§5, §7), trace on tmpfs and a defined write amplification (§2.4, §6), size after close and MDBX used pages (§6), the overturn rule and a time box (§1, §7). Nits adopted: raw undo-byte comparison and the BIP30 assertion (§2.3), torn and zero tails (§2.1), cgroup `cpu.stat` and one allocator (§4, §6).

### 10.2 Corrections made while running (v4)

The pilot and the diagnostic runs (their raw results are kept in `/work/bench-store/results-old/`, superseded) exposed harness and engine problems; an independent code review found more. Every decision-cell number in §9 comes from runs after all of these were fixed.

- **RocksDB batches were copied twice** (`dashd-store`): the batch kept its own WriteBatch encoding and the library copied it again, so a 16 MiB sub-batch held 32 MiB. Fixed in `dashd-store` (zero-copy batches); this was the production engine's bug, not only the harness's.
- **MDBX was OOM-killed** under the cap: a one-transaction flush keeps every dirty page in anonymous memory. Its dirty pages are now bounded by `E` (`txn_dp_limit`), spilling beyond that, the MDBX equivalent of the LSMs' 16 MiB sub-batches.
- **Coinbase outputs were FRESH** in the cache model; Core never marks them FRESH. Fixed (2.1M deletes in W-M instead of fewer).
- **Flush threshold**: now Core's `max(0.9 · budget, budget − 10 MiB)`.
- **RocksDB flush shape**: A1 rev1 §3.5.4's commit kinds instead of buffered batches plus one `sync_wal`.
- **Quiesce and close** are inside `time_total_s`; RocksDB's wait includes pending memtable flushes.
- **The memory cap** includes the flush sort index.
- **Failed runs** are counted per cell in the summary instead of silently dropped.
- **MDBX's reopen** in the same process failed (the environment was leaked); MDBX size is reported as file size and live pages.
- **RocksDB's whole-block index and filters** churned the small block cache (§9.3); partitioned index/filters are now `dashd-store`'s default.

### 10.3 Round-3 review and what changed

An independent Opus reviewer read the branch after the bake-off (`reviews/ST1-r3-opus.md`: changes required, 2 major and 10 minor findings, 9 nits). Manager decision DEC-43 settles the planning side of M2.

| Finding | Resolution |
|---|---|
| **M1** fail-stop not enforced: RocksDB resumes writes by itself after `ENOSPC` | Fixed. Both engines latch the first write, barrier or background error and refuse every later write and barrier (`ReadOnly`) until reopened; the `KvEngine` documentation states the contract. Tests in `dashd-store/tests/fail_stop.rs`, including the reviewer's scenario on a 200 MiB tmpfs. D4 waits 12 s with a 128 MiB ballast, and RocksDB's D4 was rerun once. §6a and §9.5 are corrected. |
| **M2** open items routed to a gate that does not test them; OI-1's size | Fixed in this document: §1 and §9.4 route the open items to DEC-43's low-memory side-by-side cell, and OI-1 is restated as at least 1.6× in every pair, with the rest put down to disk contention and an unscaled `E` at a cache cliff (caveat in §3.4). The PLAN change (CH2 BG) and the decision are DEC-43's. |
| m1 the blk scan resynchronises after a record past the end of the file; Core stops | Fixed: `scan_block_records` follows `LoadExternalBlockFile` at v23.1.8 and v24.0.0-rc.3 (the scan ends at such a record, reported as a torn tail), and its test encodes Core's rule. One difference remains, documented and pinned by a test: when Core loads a block during the scan it resumes at the end of the deserialized block, not the claimed end, so it scans the slack of a record whose claimed size exceeds its block. Core never writes such records, and matching it needs Core's runtime state (which parents are known); ST2 owns parity on corrupted inputs. Block reads ignore bytes after the block, as Core's `ReadBlockFromDisk` does. |
| m2 duplicates cover undo only; a stale header can hide rev records | Fixed. Block bytes come from the first copy that parses, hashes to the block and matches its merkle root (test: a damaged first copy of block 1000 gives the clean fixture's trace). Rev files are scanned for every magic candidate, each gated by its checksum. ST1's traces had 0 resync bytes and no damaged duplicates, so §9 is unaffected. |
| m3 every non-database directory is "not one of our databases" | Fixed: `CURRENT` decides. With it, RocksDB's own error is surfaced (`Corruption`, `Io`); without it, creation debris (`LOG`, `LOCK`, `IDENTITY`, `OPTIONS-*`, `MANIFEST-*`) is created over, table or log files are `Corruption`, and anything else is `UnknownDatabase` naming the file. A `lost+found` directory is ignored whatever it holds: it belongs to the filesystem, and mke2fs makes it unreadable to the node. Only numbered RocksDB file names count as database data (`debug.log` is foreign). |
| m4 the schema record lacks the database name | Fixed: the record is the version followed by the name, and both are checked on open. Records written before this change (bake-off scratch databases only) are refused, which is intended. |
| m5 `size_bytes` is not the batch's memory | Fixed in `dashd-store`: `size_bytes` is documented as a lower bound, and `KvEngine::batch_with_capacity` preallocates the buffer. Using it, and the counting-allocator budget assertion, are CH2's. |
| m6 `kv_model` is thin where it matters | Fixed: `Op::Reopen` for RocksDB (unit model check and fuzz target), a fresh database per fuzz input in a per-process directory (directories of killed processes are swept), corrected module docs, a file-set comparison in the refusal tests, and the error-path tests of M1. Fuzzed for 2 + 3 minutes on 4 workers with no crash. Injecting errors into the model is ST3a's `FaultyEngine`, which can build on `MemoryEngine::inject_failure`. |
| m7 the statistics are labelled more strongly than the data supports | Fixed: "95 % CI" is now "range" throughout §9; §7 and §9.2 say what a range supports; `run.py` reports the range and an exact sign test. The decision rests on disk use and is unaffected. |
| m8 missing open items (RssAnon, flush stall) | Fixed: OI-6 and OI-7 in §9.4; the harness records RocksDB's stall statistics. |
| m9 `wait_for_compactions` can hang | Fixed: it takes a timeout, and fails (and latches) on a background error. |
| m10 `MemoryEngine`'s durable write and barrier take the lock twice | Fixed: one write guard, which also holds the engine's fail-stop latch, so a write either precedes a failure or is refused. A power loss or `MemoryEngine::restart` clears the latch, as a reopen does, so ST3a can inject a failure, crash and restart. |
| n1 `EDQUOT` maps to `Io` | Fixed: a quota counts as disk full (`NoSpace`); the remedy is the same. |
| n2 rev files are rescanned on reload | Fixed: the candidates are parked with the cursor. |
| n3 reservations sized from untrusted counts | Fixed: capped at what the remaining bytes can hold. |
| n4 `compress_amount` panics on overflow in debug builds | Fixed: it wraps, as Core's `uint64_t` arithmetic does. |
| n5 the VARINT vectors cite a develop commit | Fixed: they cite `varints_bitpatterns` at both tags (identical there). |
| n6 A1 is stale (§2.8 bloom filters, §3.5.1 `atomic_flush`); `atomic_flush` and many CFs make tiny L0 files | For the manager: A1 is not edited here. `dashd-store` sets `atomic_flush` and uses partitioned index and filters by default (§9.3). The L0 effect of atomic flush across five or more chainstate CFs belongs in CH2's multi-CF workload. |
| n7 `fuzz` pulls RocksDB into every workspace build | Accepted, unchanged: the `kv_model` target has to link RocksDB, and sccache absorbs the C++ build. Moving node fuzz targets out of the workspace `fuzz` crate is a layout decision for the manager. |
| n8 the schema record is decoded in two places | Fixed: one `check_record` helper. |
| n9 a leaked `RocksBatch` keeps the database open | Fixed: debug builds assert on engine drop that no batch is alive. |

An independent review of this fix round (Opus code-review agent, approved with suggestions) led to the LOG-derived kind of background errors, the `lost+found` and file-name rules of m3, the documented scan difference of m1, a `MemoryEngine` latch that a restart clears, and the frozen schema-record layout.
