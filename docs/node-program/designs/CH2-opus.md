# CH2 (Opus): the coins cache, the coins table and the durability and recovery protocol

| | |
|---|---|
| Status | **Revision 0, independent design (D2, Opus half)**. PLAN §4 row CH2, design step D2, review class **STATE**. The manager reconciles it with the Fable design; no code until approval |
| Date | 2026-10-09 |
| Author | CH2 design agent (Opus 5.5 xhigh) |
| Task | `dashd-chainstate::{coins, flush, recovery}` and `dashd-store`: the coins cache (`PackedCoin`, freeze-and-flush under one memory budget), the coins table, **A1 §3.5's durability and recovery protocol** (file → B1 → coin sub-batches → Bk; coins-only `ReplayBlocks`; errors and disk full; subscribers' locators), and the multi-CF bake-off workload. FR-CHS-03, FR-CHS-04, FR-STO-03, US-NODE-04 |
| Binding inputs | A1 §2.8–§2.11, §3.4–§3.5 and its errata section; **ST3a rev5 (a24baa12, DEC-74), whose §5.6 is normative here**, with its errata E1–E22 (§12 there); DEC-43, DEC-59, DEC-62, DEC-63, DEC-64, DEC-66, DEC-69; ST1's merged `dashd-store` (583d0fbc); ST2's block files (`node/st2-block-files` @ 21c98a71); CS0's `dash-consensus` (`CoreBytes`); CS5a's coin codec and `CoinsView` (in progress, `rdc-wt/cs5a-coins`, read as an interface only); PLAN rows CH2, CS5a/b, CH1, CH3a/b, CH4, CH5, CH7a/b, CH8, IX1, ND1, ST3b, PF0/PF1; memory notes `q10-durable-prune-lock-lag`, `rocksdb-wal-nonprefix-recovery`, `rocksdb-macos-fullfsync-and-prune-hole`, `st1-storage-bakeoff` |
| Core | `~/workspace/dash`, read with `git show` at **v23.1.8** and **v24.0.0-rc.3**. Citations are `path:v23 / :v24`, paths under `src/`. Evidence extracts with full line maps: `/work/scratch/ch2-opus/core-coins.md`, `/work/scratch/ch2-opus/core-flush.md`; the memory-accounting probe compiled against each pin's headers: `/work/scratch/ch2-opus/tmp/sz.cpp` |
| Code read | `dashd-store` on `node/integration` 01cf7b7e (`engine.rs`, `rocks.rs`, `fail_stop.rs`, `schema.rs`, `table.rs`); `dashd-store/src/flatfile/` on `node/st2-block-files` 21c98a71; `dash-consensus/src/coins/` (CS5a, uncommitted); `rocksdb` 0.25.0 / `librocksdb-sys` 0.19.0+11.8.1 |

---

## 0. Summary

1. **The cache is Core's `CCoinsViewCache`, split into two layers.** V (the validation thread) owns an **active** layer. A flush **freezes** it in O(1) into an immutable **frozen** layer F that W (the db-writer) writes while V keeps connecting. The read path is overlay → active → frozen → prefetch → database. The equivalence argument (§3.2): F over the database presents exactly the view "the database after Core flushed at F's tip", and the active layer starts empty at the freeze, so every operation on it is Core's `CCoinsViewCache` over that base. Core's two defensive `logic_error`s consult active and F, Core's "cache since the last flush".
2. **Density.** A layer is an append-only **arena of 72-byte slots** (key, flags, ≤ 34 bytes of Core's `Coin` serialization inline) behind a `hashbrown::HashTable<u32>` index. That is about **80 B per P2PKH coin against Core's measured 115.7 B** (PoolAllocator, both pins), so roughly 1.45× more coins fit in the same `-dbcache` (to be confirmed by PF1, A1 §3.4's rule). Iteration is arena order, so no store call depends on the hash salt. Memory grows smoothly in 256 KiB chunks, as Core's pool does.
3. **One memory budget.** `B(t) = coins share + max(maxmempool − mempool usage, 0)` is Core's `nTotalSpace` exactly. `active + frozen + prefetch + batch reserve ≤ B(t)` is enforced and asserted with a tagging allocator. The engine's memory (block cache with memtables charged to it) comes from Core's own DB shares plus Core's fixed 64 MiB EvoDB cache, not from the coins share. This is the DEC-43 (b) split. The `Cache configuration` log lines are Core's, per profile.
4. **The flush** is a synchronous, step-wise `FlushMachine` (ST3a §5.6 items 1–3), generic over `E: KvEngine` and `F: FlatFs`. It implements ST3a's order step for step: prune decision → space → files barrier → [presumed-effect barrier] → **B1** (Durable: index, file infos, evo, `evo_tip`, marker; or everything in a one-batch flush) → `ChainStateFlushed` → synced mandatory subscribers commit → prune selection → coin sub-batches (Buffered) → `sync_wal` → **Bk/Bp** (Durable) → release → unlink. Production runs it on W; the seam steps it on the test thread.
5. **Recovery** is coins-only and blind. It runs `sync_wal` at open, checks the marker, undoes old → fork and replays fork → new from block and undo files with puts and erases that never read the keys they write, then `sync_wal` and a Durable final batch. It is idempotent and produces `replayed()`. Evo is not touched, because B1 committed it at `new`.
6. **Subscribers.** CH2 ships the W side (`SubscriberHub`: classes as Core's `m_synced`, the step-6 commit wait, `T_ack`, prune locks) and a subscriber-side library (`LocatorKeeper<E>`). The library enforces §5.6 item 6 by construction: two tips p and s, submit-before-sync, `D_max` backpressure, the Durable checkpoint, the Durable rewind commit, and the blind rewind. IX1, CH7b and the `llmq/` owner use it instead of re-deriving the rules.
7. **Errors.** One process-wide fail-stop latch spans the chainstate engine, every subscriber engine and the block files. The first failure aborts with Core's strings; nothing commits after it.
8. **Performance.** V blocks on a flush only when a second freeze is needed while F is still being written (Core blocks V for the whole flush). There are fewer misses at equal `-dbcache` (density), and the flush writes Core's bytes with a memcpy (no re-serialization, no XOR). PF0 workloads: `coins_micro`, `coins-replay-{testnet,mainnet}`, `coins-recovery`, `coins-shutdown-flush`, then `reindex-chainstate` windows once CH3a lands. The DEC-43 cell is answered by a merge-time proxy and gated end to end at M3's exit (Q1).
9. **Verification.** Core's `coins_tests.cpp` tables (L0); a new oracle entry point `oracle_coins_ops` (L1); trace replays (L2); the ST3a model over CH2's **real** code with 11 scenario families, both crash kinds, nested crashes and 14 planted bugs (CRASH); 4 fuzz targets; FT owners named.
10. **Seven phases** (§7). I recommend splitting the task as CH2a/b/c (Q5).

---

## 1. Scope and non-goals

### 1.1 The PLAN row, item by item

| PLAN CH2 text | Where |
|---|---|
| Coins cache, `PackedCoin` | §2.2–§2.4, §3.2 |
| freeze-and-flush under one memory budget | §2.5, §4.2, §5.2 |
| coins table | §2.6 |
| A1 §3.5 protocol: file → B1 → coin sub-batches → Bk | §4.1–§4.2, §4.7 |
| coins-only `ReplayBlocks` | §2.8, §3.5, §4.3 |
| errors and disk full | §2.10, §4.6 |
| subscribers' locators | §2.9, §4.4 |
| the multi-CF bake-off workload | §5.8 |
| Acceptance: CRASH model level on every sub-batch boundary, the first flush, a reorg in the window, interrupted recovery | §6.4 (C1–C4 and more) |
| `coins_cache_model` | §6.5 |
| budget invariant asserted with a counting allocator | §6.7 |
| BG: `reindex-chainstate` counters; `reindex_time_s`, `peak_rss_mib` not worse than the previous head | §5.7 (proxies until CH3a; Q1) |
| BG: the DEC-43 low-memory side-by-side cell not slower than dashd | §5.5, Q1 |

### 1.2 PRD requirements

- **FR-CHS-03:** a write-back cache sized by `-dbcache`, batched atomic flushes, per-block undo. CH2 owns the first two; CH3a writes undo through ST2.
- **FR-CHS-04:** atomic consistency across the UTXO set, the MN list, the LLMQ indexes and the index markers. B1 commits evo and LLMQ CFs at `new` with the marker; subscriber locators follow §4.4.
- **FR-STO-03:** atomic commits, a documented fsync policy (§4.1), and the power-loss harness (ST3a, used in §6.4).
- **US-NODE-04 / NFR-OPS-01:** survives crashes and power loss without a reindex; `feature_dbcrash` passes (CH5 runs it with CH2's `-dbcrashratio`); clean `stop` no slower than dashd at the same `-dbcache` (§5.3).
- **FR-STO-02, FR-STO-05:** the chainstate schema is versioned (§2.6); explicit codecs only.
- **NFR-PERF-01/03, NFR-RES-01/02:** §5.

### 1.3 Non-goals

- **`connect_block` / `disconnect_block`** (CH3a), chain selection (CH3b), the S1/S2 pipeline and the prefetch producer (CH4). CH2 provides the overlay, the cache, the prefetch slot with its fence, and W's flush.
- **Evo state** (CH7a). CH2 defines the `StateDelta` slot B1 writes and the presumed-effect ledger; CH7a fills them and classifies effects.
- **Prune policy** (CH8): `-prune` parsing, `pruneblockchain`, index incompatibilities, the FTs. CH2 implements the protocol's prune slots (decision, `D ∩ eligible`, Bp, clears in Bk, unlink order) over ST2's selectors, off by default (§4.5).
- **Real subscribers** (IX1, CH7b, LK1b, wallets). CH2 ships the hub, the library and a test subscriber.
- **`-reindex(-chainstate)`, `-loadblock`, `verifychain`** (CH5). CH2 provides the wipe primitive (§2.6) and `-dbcrashratio`.
- **Core datadir import** (CH6a/b), assumeutxo (NG-6), the device and VM harnesses (ST3b), the `node-bench` cell itself (B1b/B1c).
- **Windows.** macOS durability rests on BL-32.

---
## 2. Data model and public API

### 2.1 Placement

```text
dashd-chainstate/src/
  coins/{mod, key, packed, layer, cache, overlay, prefetch, budget, table, reference}.rs
  flush/{mod, packet, machine, policy, subscribers, keeper, prune, observer, driver}.rs
  recovery/{mod, replay}.rs
  store/{schema, meta, guarded, sizes}.rs        # chainstate DbSchema v1, meta codecs, global fail-stop, CacheSizes
  tests/ chain_sut.rs (the ST3a Sut over real CH2 code), coins_tables.rs (Core's coins_tests ports)
dashd-store/src/rocks.rs                          # RocksConfig: write-buffer manager charged to the block cache; asserts
dashd-store-bakeoff/                              # the W-MC multi-CF workload (§5.8)
node-fuzz/fuzz_targets/{coins_cache_model, coins_overlay_diff, chainstate_crash, coins_replay_blind}.rs
```

`dashd-chainstate` is CH1's crate (M2). CH2 adds modules to it. If CH1 has not merged when CH2 starts, CH2 codes against the narrow `IndexAccess` trait of §2.11, which CH1 implements (Q6).

### 2.2 Keys, `PackedCoin`, slots

```rust
/// txid ‖ vout (LE). The in-memory key; Core's COutPoint layout.
#[derive(Clone, Copy, PartialEq, Eq)] #[repr(transparent)]
pub struct OutPointKey(pub [u8; 36]);
impl OutPointKey {
    pub fn new(op: &OutPoint) -> Self;
    pub fn outpoint(&self) -> OutPoint;
    /// The `coins` CF key: txid ‖ VARINT(vout), Core's CoinEntry without its 'C' prefix (A1 §2.9).
    /// Core's VARINT preserves numeric order, so CF order is (txid bytes, vout), Core's cursor order.
    pub fn db_key(&self, buf: &mut [u8; 41]) -> usize;
}

/// Core's `Coin` serialization (`coins.h:60-66`): VARINT(h·2+cb) ‖ VARINT(CompressAmount) ‖ ScriptCompression.
/// The same bytes are the cache's value and the database's value, so a flush is a memcpy.
pub struct PackedCoin(Repr);
enum Repr { Inline { len: u8, bytes: [u8; 34] }, Long(Box<[u8]>) }
impl PackedCoin {
    pub fn encode(coin: &Coin) -> Result<PackedCoin, CoinsError>;  // Err only for an out-of-range value (DEC-58: never panics)
    pub fn decode(&self) -> Coin;                                   // CS5a Coin::decode; infallible for bytes we encoded
    pub fn from_db(bytes: &[u8]) -> Result<PackedCoin, CoinsError>; // validated by a full decode (Corruption otherwise)
    pub fn as_bytes(&self) -> &[u8];
    pub fn height(&self) -> u32; pub fn is_coinbase(&self) -> bool; // read the first VARINT only
}
```

- **Inline length.** A P2PKH or P2SH coin is `VARINT(code)` (≤ 4 B up to height 135 M) + `VARINT(amount)` (≤ 9 B) + 21 B, so **at most 34 B: always inline**. Compressed P2PK is 33 B of script, so 38–46 B: long. Measured shapes: `core-coins.md` §2.
- **Round trip.** For every coin `AddCoin` can store, `decode(encode(c)) == c`.
  - `IsUnspendable` outputs (an `OP_RETURN` first byte, or more than `MAX_SCRIPT_SIZE` bytes) never enter the cache (`coins.cpp:70-76`, both pins; quirk 33).
  - P2PK with an uncompressed key is compressed only when `IsFullyValid`, and decompression recovers y exactly (`compressor.cpp:19-84`).
  - Amounts are within `[0, MAX_MONEY]`, because `CheckTransaction` runs first.
  - A property test and the L1 target (§6.2) pin this.
- **Encoding cost.** Core serializes at flush time, and only the coins that survive. We encode on `add`, so a coin created and spent within one window still pays one encode (about 50 ns; the `IsFullyValid` check only for uncompressed P2PK). `coins_micro` measures it (§5.7).

### 2.3 The layer: arena plus index

```rust
#[repr(C, align(8))]
struct Slot { key: OutPointKey /*36*/, flags: Flags /*1*/, len: u8 /*1; LONG = 0xFF*/, coin: [u8; 34] }   // 72 B
bitflags! { struct Flags: u8 { const DIRTY = 1; const FRESH = 2; const SPENT = 4; } }

pub(crate) struct CoinLayer {
    chunks: Vec<Box<[Slot; SLOTS_PER_CHUNK]>>,   // 3,640 slots = 262,080 B per chunk; never moved
    free: Vec<u32>,                               // slots freed by a FRESH spend or an uncache, reused first
    index: hashbrown::HashTable<u32>,             // slot ids; hashes through the arena (A1 §2.10's block-index idiom)
    long: LongCoins,                              // chunked byte arena for coins > 34 B; slot holds (offset u32, len u16)
    hasher: SaltedFx,                             // random salt; FIXED under the seam (ST3a §4.9)
    live: u32, dirty: u32, dirty_bytes: u64,      // dirty_bytes = Σ (db key + value + batch overhead): decides one-batch vs split
}
```

- **Per entry:** 72 B in the arena, plus 5 B per bucket of index (`u32` + control byte) at a load factor between 7/16 and 7/8. That is **77.7–83.4 B, about 80 B on average**.
  - Core counts 104.05 B of pool node and 8–16 B of buckets per coin: **115.67 B measured** for 1 M P2PKH coins, identical at both pins (`memusage.h:170-187 / :186-203`; probe `sz.cpp`).
  - Compressed P2PK costs about 128 B here against Core's about 180 B.
- **Why not A1 §3.4's `HashMap<OutPointKey, Entry>`.** hashbrown stores entries in its buckets, so memory doubles at each resize and an entry costs (72 + 1)/load, between 83 and 167 B. A budget at a power-of-two cliff either wastes up to half of B or overruns it during a rehash. The arena grows by one chunk at a time, as Core's `PoolResource` grows by 256 KiB chunks (`support/allocators/pool.h:185-187`), and only the small index rehashes (≤ 1.5 × 5 B per entry, transiently). This is an A1 refinement (Q3, errata A-3).
- **Accounting is allocated bytes:** chunks + index allocation + long chunks, exactly as Core counts pool chunks rather than live nodes (`memusage.h:170-187`). Freed slots are reused but not returned until the layer is dropped, as Core's pool returns memory only in `ReallocateCache` (`coins.cpp:315-323`).
- **Determinism.** Iteration (the flush's write order, `iter_dirty`) is arena order: insertion order plus deterministic free-slot reuse. **No store call depends on the hash salt.** The fixed seam salt only makes probe sequences reproducible.
- **Lookup:** probe the control bytes, then one compare in the arena. That is one cache miss, the same as hashbrown with inline entries.

### 2.4 `CoinsCache` and `CoinsOverlay`

```rust
pub struct CoinsCache<E: KvEngine> {
    active: CoinLayer,
    frozen: Option<Arc<FrozenLayer>>,      // at most one (A1 §3.4); V and W hold it
    prefetch: PrefetchMap,                  // positive DB results with fetch_gen; CH4 fills it
    released_gen: u64,
    table: CoinsTable, db: Arc<E>,
    budget: CoinsBudget, fail: GlobalFailStop,
}
pub struct FrozenLayer { coins: CoinLayer, gen: u64, tip: BlockRef }   // immutable after freeze

impl<E: KvEngine> CoinsCache<E> {
    // ---- Core's CCoinsViewCache over base := (frozen over DB) ----
    pub fn get(&mut self, op: &OutPoint) -> Result<Option<Coin>, CoinsError>;         // GetCoin; caches a DB or prefetch hit as clean
    pub fn have(&mut self, op: &OutPoint) -> Result<bool, CoinsError>;                // HaveCoin
    pub fn have_in_cache(&self, op: &OutPoint) -> bool;                                // HaveCoinInCache: active ∪ frozen
    pub fn add(&mut self, op: &OutPoint, coin: &Coin, possible_overwrite: bool) -> Result<(), CoinsError>;
    pub fn spend(&mut self, op: &OutPoint) -> Result<Option<Coin>, CoinsError>;       // SpendCoin with moveout
    pub fn uncache(&mut self, op: &OutPoint);                                          // Uncache: clean entries only
    pub fn merge(&mut self, child: CoinsOverlay) -> Result<(), CoinsError>;           // BatchWrite(child → tip, erase = true)
    // ---- budget, freeze, release ----
    pub fn usage(&self) -> Usage;                              // { active, frozen, prefetch, reserve, total }
    pub fn size_state(&self, mempool: MempoolUsage) -> CoinsCacheSizeState;  // Core's OK/LARGE/CRITICAL over B(t)
    pub fn freeze(&mut self, tip: BlockRef) -> Arc<FrozenLayer>;            // O(1): mem::take(active)
    pub fn release(&mut self, gen: u64);                       // drop F; released_gen += 1; prefetch.clear()
    pub fn admit_prefetch(&mut self, batch: PrefetchBatch);    // CH4; accepted iff batch.fetch_gen == released_gen and the
                                                               // invariant keeps headroom (prefetch ≤ B/16); otherwise dropped
}

/// Core's child view (`CCoinsViewCache view(&CoinsTip())`, validation.cpp:3189 / :3430 connect,
/// :3048 / :3279 disconnect). Insertion-ordered; reads fall through to the parent.
pub struct CoinsOverlay { layer: CoinLayer /* small; same slot type */ }
pub struct OverlayView<'a, E> { /* RefCell<&mut CoinsOverlay>, RefCell<&mut CoinsCache<E>> */ }
impl<E: KvEngine> dash_consensus::coins::CoinsView for OverlayView<'_, E> { /* CS5a's trait */ }
impl<'a, E> OverlayView<'a, E> {
    pub fn add(&self, op, coin, possible_overwrite) -> Result<()>; pub fn spend(&self, op) -> Result<Option<Coin>>;
    pub fn access(&self, op) -> Result<Option<Coin>>;
    pub fn failed(&self) -> Option<CoinsError>;   // see "read errors" below
}
```

**Read path:** overlay → active → frozen → prefetch → database (live `get` on V).
- A **tombstone** (SPENT) in active or frozen is authoritative: the coin does not exist.
- A database or prefetch hit is copied into active as a clean entry (flags 0), as Core's `FetchCoin` caches it (`coins.cpp:44-59`).
- A frozen hit is **not** copied, because F lives until release. `spend` of a coin that lives only in F writes a DIRTY, non-FRESH tombstone into active.

**Cross-check** (A1 §3.4 item 5). With `-consensusxcheck`, every `get` is also resolved without prefetch (active → F → a direct database read), and any difference aborts. CH4 extends the check to its prefetch producer.

**Read errors** (CONS-22). CS5a's `CoinsView::coin` is infallible, as Core's is.
- On an engine read error, the adapter trips the global fail-stop latch, emits the abort ("Error reading from database, shutting down.", `init.cpp:2026-2030 / :2087-2091`), records the error and returns `None`.
- **CH3a must check `OverlayView::failed()` before acting on any verdict.** A tripped latch turns every result into `ConnectError::Fatal`, never into `BLOCK_CONSENSUS`, so a disk error can never mark a block invalid. Core gets the same outcome by aborting the process (`coins.cpp:355-369 / :356-379`).
- At v24 Core also catches the error in `HaveCoin`, and so do we, for both profiles (CONS-22, `v24-all`).
- An alternative is a fallible `CoinsView` (Q4).

### 2.5 The budget and the memory split

```rust
pub struct CacheSizes { pub block_tree_db: u64, pub tx_index: u64, pub address_index: u64, pub timestamp_index: u64,
                        pub spent_index: u64, pub filter_index: u64, pub coins_db: u64, pub coins: u64 }
impl CacheSizes { pub fn calculate(dbcache_mib: i64, args: &IndexArgs, n_filter: usize, rules: &IndexRules) -> Self; }
pub struct CoinsBudget { coins_share: u64, max_mempool: u64, f: Ratio /* freeze fraction, 0.5 */, batch_reserve: u64 }
impl CoinsBudget { pub fn total(&self, mempool_usage: u64) -> u64 { self.coins_share + self.max_mempool.saturating_sub(mempool_usage) } }
```

- **`CacheSizes::calculate`** is Core's `CalculateCacheSizes` line for line (`node/caches.cpp:12-33 / :15-42`).
  - Clamp `-dbcache` to [4, 16384] MiB.
  - Block tree: `min(total/8, 2 MiB)`. Txindex: `min(rem/8, 1 GiB)`, if `-txindex` (Dash's default is on).
  - **v24 only:** address, timestamp and spent index shares (`IndexRules.dash_index_model`, O17 MPIX-08/10).
  - Filter indexes; then `coins_db = min(rem/2, rem/4 + 8 MiB, 8 MiB)`; `coins` is the rest.
  - rdashd prints Core's `Cache configuration:` block with the same lines and values per profile (`init.cpp:1946-1956 / :1986-2023`). node-bench sums those lines (`node-bench/README.md:282`).
- **The coins budget `B(t)`** is Core's `nTotalSpace`: `coins + max(maxmempool − mempool usage, 0)` (`validation.cpp:2737-2759 / :2945-2967`). v23 reads `-maxmempool` per call; v24 reads the mempool's own maximum. In a node with a mempool the value is the same, so we use v24's source for both profiles.
- **The invariant**, checked on every growth of active and asserted in tests (§6.7):

  `bytes(active) + bytes(F) + bytes(prefetch) + R ≤ B(t)`, where `R = S + 64 KiB` while a flush is in flight, else 0.

  `S = min(-dbbatchsize, max(B/8, 64 KiB))` is the effective sub-batch size used throughout §4. It equals Core's 16 MiB at every realistic B and only shrinks for tiny caches such as `feature_dbcrash`'s `-dbcache=4`. That changes only how many partial batches there are, which is not observable.

  - R is the sub-batch buffer, preallocated with `KvEngine::batch_with_capacity` so it never doubles (ST1 D3).
  - The memtable copy of a sub-batch is engine memory, inside E below.
  - A1 §3.4 counts the batch buffer inside B, and so does this design. Core does not count its `CDBBatch`.
- **Freeze and wait** (§3.3 has the full trigger table):
  - **Freeze** when `bytes(active) ≥ f·B(t)` and no F is in flight. `f` defaults to 0.5, as A1 says; PF3 tunes it within the invariant.
  - With F in flight, active may grow to `B(t) − bytes(F) − bytes(prefetch) − R`. At that limit **V waits** for W's release, then freezes at once. This is the only I/O stall (Core's CRITICAL).
  - If `B(t)` shrinks (the mempool fills) so that the invariant breaks with no F in flight, V freezes at the next block boundary.
- **The engine budget E** (DEC-43 (b); A1 erratum A-2):

  `E = block_tree_db + coins_db + 64 MiB`

  - Core's block-index and chainstate LevelDB shares come out of `-dbcache`.
  - Core's EvoDB LevelDB always gets a fixed 64 MiB cache outside `-dbcache` (`evo/evodb.cpp:35 / :38`), and our `chainstate/` holds the evo CFs.
  - At the default that is 2 + 8 + 64 = **74 MiB**, the same as Core's three LevelDB caches together. It does **not** reduce the coins share, unlike A1's split (8 % block cache plus 64 MiB of memtables taken out of `-dbcache`), which gave a smaller B than dashd's 252.75 MiB at the default.
- **RocksDB under E** (a `dashd-store` change, §2.11):
  - **One LRU block cache of E bytes.** Memtables are charged to it through `WriteBufferManager::new_write_buffer_manager_with_cache` (rocksdb 0.25 `db_options.rs:101`), capped at E/2. So index, filter and data blocks plus memtables never exceed E.
  - Reads are buffered `pread` (no mmap, no direct I/O): the SSTs lean on the page cache, as LevelDB's mmapped tables do (ST1 OI-1, OI-6).
  - `max_total_wal_size = 2 · E/2` bounds WAL replay at startup.
  - The evo CFs keep A1's compression.
- **Other memory** stays outside `-dbcache`, as in Core: the block-index arena (CH1), the in-flight evo delta (bounded by Core's 64 MiB EvoDB-critical trigger, §3.3), and the dirty index records of a freeze.

### 2.6 The coins table, the chainstate schema, `meta`

```rust
pub static CHAINSTATE_SCHEMA: DbSchema = DbSchema { name: "chainstate", version: 1, cfs: &[
    CfSpec::new("meta", Ordered), CfSpec::new("block_index", PointLookup), CfSpec::new("block_files", Ordered),
    CfSpec::new("coins", PointLookup), CfSpec::new("mn_diff", Compressed), CfSpec::new("mn_snapshot", Compressed),
    CfSpec::new("credit_pool", Compressed), CfSpec::new("mnhf", Ordered), CfSpec::new("llmq_mined", Compressed),
    CfSpec::new("llmq_mined_h", Ordered), CfSpec::new("llmq_snapshot", Compressed) ] };   // A1 §2.9, all CFs from v1

pub struct CoinsTable { cf: Cf }
impl CoinsTable {
    pub fn get(&self, db: &impl KvRead, k: &OutPointKey) -> Result<Option<PackedCoin>>;
    pub fn multi_get(&self, db: &impl KvRead, ks: &[OutPointKey]) -> Result<Vec<Option<PackedCoin>>>;   // CH4's S2
    pub fn put(&self, b: &mut impl KvBatch, k: &OutPointKey, v: &[u8]);  pub fn erase(&self, b: &mut impl KvBatch, k: &OutPointKey);
    pub fn cursor<'a>(&self, snap: &'a impl KvRead) -> impl Iterator<Item = Result<(OutPoint, Coin)>> + 'a;  // CCoinsViewDBCursor
    pub fn estimate_size(&self, db: &impl KvEngine) -> Result<u64>;   // CCoinsViewDB::EstimateSize → gettxoutsetinfo.disk_size
}
pub struct Meta { pub tip: Option<BlockHash>, pub head_blocks: Option<(BlockHash, Option<BlockHash>)>,
                  pub evo_tip: Option<BlockHash>, pub pruned_files: bool, pub last_file: u32, pub reindexing: bool }
```

- **Values.** `coins` holds Core's `Coin` bytes with no XOR obfuscation (A1 §2.9; Core XORs values only, `dbwrapper.h:96-123 / :100-127`). `meta` keys are A1 §2.9's tags.
  - `head_blocks` is 64 B, `new ‖ old`, with all-zero for a null `old`. A real `old` can be genesis but never zero.
  - `tip` and `evo_tip` are 32 B. `last_file` is a BE u32 (ST2's `BlockFilesState`). Flags are one byte.
  - Each codec has a layout test (FR-STO-05).
- **The cursor** decodes keys and values. A malformed record is `Corruption` and aborts. Core's cursor stops silently there (`txdb.cpp:194-265 / :203-274`); that is only reachable on a corrupt DB, so it is declared, not emulated.
- **Schema.** All A1 CFs exist from v1, so CH7a adds no migration before the first release. Pre-release schema bumps refuse older databases (FR-STO-02 applies from the first release).
- **Wipe for CH5.** `wipe_chainstate(engine)` resets the chainstate for `-reindex-chainstate`, which keeps `block_index` and `block_files`.
  1. A Durable `meta.reindexing = 1` and the erase of `tip`, `evo_tip` and `head_blocks`.
  2. `delete_range` over `coins` and the evo CFs (BL-38).
  3. A Durable clear of the flag.

  A crash between steps 1 and 3 resumes the wipe, because the flag is set.
- **`estimate_size`.** On regtest the copy points flush memtables before replying (A1 §2.9), so `estimate_size` is the SST size. The FT bound `6400 < disk_size < 64000` (`rpc_blockchain.py:324-326 / :372-374`) is a CH2 unit test on a 200-block regtest DB.

### 2.7 The flush API

```rust
pub struct BlockRef { pub id: BlockId, pub hash: BlockHash, pub height: u32 }

/// What V hands W at a freeze. Built synchronously on V; immutable afterwards.
pub struct FreezePacket<D: StateDelta> {
    pub flush_id: u64, pub gen: u64, pub new: BlockRef, pub old: Option<BlockRef>,   // old = the previous durable tip
    pub coins: Arc<FrozenLayer>,
    pub delta: D,                                   // CH7a's EvoDelta of F's window, in block order; () until CH7a
    pub index: Vec<IndexWrite>,                     // CH1: every dirty block_index record at the freeze (hash, bytes)
    pub presumed_effects_seq: u64,                  // the presumed-effect ledger at the freeze (§4.1)
    pub causes: FlushCauses, pub mode: FlushStateMode,
    pub prune: Option<PruneDecision>,               // step 0's retained dry run D, met_target, predicted locks (§4.5)
    pub dbcrashratio: u32,
}
pub trait StateDelta: Send + 'static {
    fn write_b1(&self, batch: &mut impl KvBatch, cfs: &ChainstateCfs);   // evo CFs, in block order
    fn bytes(&self) -> u64;                                               // Core's m_evoDb.GetMemoryUsage analogue
}

pub struct FlushMachine<E: KvEngine, F: FlatFs, D: StateDelta> { packet: FreezePacket<D>, step: FlushStep, /* … */ }
pub enum FlushStep { PruneDecided, Space, Files, PresumedBarrier, B1, Durable, Subscribers, PruneSelect,
                     SubBatch(u32), PreBkBarrier, Bk, Release, Unlink, Done }
pub enum Step { Progress, Wait(WaitFor), Done(FlushReport) }
pub enum WaitFor { Subscribers(Vec<SubscriberId>), V(VRequest) }
impl<E: KvEngine, F: FlatFs, D: StateDelta> FlushMachine<E, F, D> {
    pub fn step(&mut self, env: &mut FlushEnv<'_, E, F>) -> Result<Step, Abort>;   // one store-call group per step
}
pub struct FlushEnv<'a, E, F> { pub engine: &'a GuardedEngine<E>, pub files: &'a Mutex<BlockFiles<F>>,
    pub hub: &'a SubscriberHub, pub obs: &'a dyn FlushObserver, pub abort: &'a dyn AbortSink, pub cfg: &'a FlushConfig }

pub trait FlushDriver { fn submit(&self, m: FlushMachine<..>); fn poll_done(&self) -> Option<FlushReport>; }
pub struct ThreadDriver;  // production: W loops step() on the db-writer thread
pub struct InlineDriver;  // the seam (ST3a §5.6 item 2): the caller steps W, V and subscribers on one thread
pub struct IndexOnlyWrite { pub index: Vec<IndexWrite> }  // Core's fPeriodicWrite: files barrier + one Durable batch
```

**The observer** (ST3a §5.6 item 4). Production code does not depend on `dashd-store::faulty`. CH2 emits its own marks, and a test adapter forwards them to `World::mark`.

```rust
pub trait FlushObserver: Send + Sync { fn mark(&self, m: ObsMark); }
pub enum ObsMark { FlushBegin { flush: u64, old: Option<BlockHash>, new: BlockHash }, Durable { flush: u64 },
    Locator { store: &'static str, tip: BlockHash, kind: LocatorKind }, PruneDecision { .. }, PruneSelected { .. },
    SubscriberState { .. }, Log(String), BestEffortBegin, BestEffortEnd, RecoveryBegin, RecoveryEnd,
    StoreFailed { op: &'static str }, Abort { message: String } }
```

`Connected { applied }` is CH3a's mark. `ConnectBegin`/`ConnectEnd` come from CH3a's `connect_block` wrapper.

### 2.8 The recovery API

```rust
pub struct RecoveredChainstate { pub tip: BlockRef, pub replayed: Vec<BlockHash>, pub meta: Meta }
pub fn open_chainstate<E: KvEngine, F: FlatFs>(
    engine: &GuardedEngine<E>, files: &Mutex<BlockFiles<F>>, index: &dyn IndexAccess, hub: &SubscriberHub,
    obs: &dyn FlushObserver, cfg: &RecoveryConfig,
) -> Result<RecoveredChainstate, InitError>;
pub struct Recovery<'a, E, F> { /* state machine; step() for the seam */ }
impl Recovery<'_, E, F> { pub fn step(&mut self) -> Result<RecoveryStep, InitError>; pub fn replayed(&self) -> &[BlockHash]; }
```

`replayed()` is ST3a's R6(b) input: the blocks replayed coins-only, `old → fork → new`.

### 2.9 Subscribers

```rust
pub enum SubscriberClass { Synced, Syncing }            // Core's m_synced (index/base.cpp:78, 154-158 / :90, 166-170)
pub struct BlockNeeds { pub block: bool, pub undo: bool }
pub struct SubscriberHub { /* registry; T_ack; commit-request fan-out; acks; prune-lock view */ }
impl SubscriberHub {
    pub fn register(&self, name: &'static str, needs: BlockNeeds, mandatory: bool) -> SubscriberHandle;
    pub fn t_ack(&self) -> Option<BlockRef>;            // the tip of the last flush whose B1 returned
    pub fn predicted_locks(&self, new: &BlockRef, idx: &dyn IndexAccess) -> Vec<(&'static str, u32)>;  // step 0
    pub fn actual_locks(&self, new: &BlockRef, idx: &dyn IndexAccess) -> Vec<(&'static str, u32)>;     // step 7
}
/// Subscriber side: owns the subscriber's locator in its own engine and enforces ST3a §5.6 item 6.
pub struct LocatorKeeper<E: KvEngine> { engine: Arc<GuardedEngine<E>>, d_max: u32,
    durable: Option<BlockRef> /* L, barrier-covered */, submitted: Option<BlockRef> /* s */, processed: Option<BlockRef> /* p */,
    staged: E::Batch, class: SubscriberClass, handle: SubscriberHandle }
impl<E: KvEngine> LocatorKeeper<E> {
    pub fn may_process(&self, b: &BlockRef) -> Gate;                       // Proceed | AtLimit (h(b) > h(L) + D_max)
    pub fn stage(&mut self, b: &BlockRef, f: impl FnOnce(&mut E::Batch));   // record b's data; p := b
    pub fn submit(&mut self) -> Result<()>;                                 // write staged data (Buffered); s := p
    pub fn commit(&mut self, x: BlockRef, kind: LocatorKind) -> Result<()>; // submit through x → sync_wal → locator
    pub fn rewind_commit(&mut self, fork: BlockRef, range: impl Fn(u32) -> KeyRange) -> Result<()>;  // Durable, blind deletes
    pub fn recover(&mut self, g: &BlockRef, range: impl Fn(u32) -> KeyRange) -> Result<CatchUp>;    // blind rewind, then catch up
    pub fn on_flush_commit(&mut self, new: &BlockRef, idx: &dyn IndexAccess) -> Result<()>;          // commit at LCA(p, new); ack W
}
```

### 2.10 Errors, fail-stop, abort

```rust
#[derive(thiserror::Error, Debug)] pub enum CoinsError {
    #[error("Attempted to overwrite an unspent coin (when possible_overwrite is false)")] OverwriteUnspent,
    #[error("FRESH flag misapplied to coin that exists in parent cache")] FreshMisapplied,
    #[error("coin value out of range")] ValueRange,
    #[error(transparent)] Store(#[from] StoreError), #[error("database corrupted: {0}")] Corruption(String) }
pub struct GlobalFailStop(Arc<OnceLock<(ErrorKind, String)>>);        // one per process
pub struct GuardedEngine<E> { inner: E, latch: GlobalFailStop }        // checks before every write/barrier; latches on error
impl<E: KvEngine> KvEngine for GuardedEngine<E> { /* delegate, plus the global latch */ }
pub trait AbortSink: Send + Sync { fn abort(&self, log_msg: &str, user_msg: Option<&str>); }  // ND1: Core's AbortNode
```

- `OverwriteUnspent` and `FreshMisapplied` are Core's `logic_error`s (`coins.cpp:80-98, 217-223`). Both are unreachable given BIP30/BIP34 (§3.2). They surface as `Fatal` (an abort), never as a block verdict, and never as a panic (DEC-58).
- ND1's `AbortSink` applies `InitRules.abort_exit_failure` (O17 P2P-31: exit 0 under v23, 1 under v24).

### 2.11 Neighbours and the APIs CH2 needs

| Task | Dir. | Exchange | Status / request |
|---|---|---|---|
| **ST1** `dashd-store` | up | `KvEngine`, `KvBatch::size_bytes`, `batch_with_capacity`, `Commit`, `sync_wal`, `snapshot`, `multi_get`, `FailStop` | **Requests:** (a) `RocksConfig.write_buffer_manager: Option<WbmConfig { bytes, charge_to_cache }>` (CH2 implements it in `rocks.rs`, as DEC-43 gives CH2 the split); (b) set **and assert** `manual_wal_flush = false` (ST3a A-DUR, E17); (c) `KvBatch::delete_range` (BL-38, approved). CH2 also changes `RocksConfig::default()` to §2.5's split |
| **ST2** block files | up | `BlockFiles<F>`, `sync_and_take_dirty_infos() -> DirtyFiles`, `prune_one_block_file`, `find_files_to_prune{,_manual}`, `unlink_pruned_files`, `scan_and_unlink_already_pruned_files`, `BlockFileReader::{read_block, read_undo}`, `FlatFs` | Enough as is. **Later lever** (§5.6 L6): a two-phase barrier (`begin_sync()` takes the dirty set under the lock; `finish_sync()` runs `fdatasync` without it), so V's appends do not wait behind W's fsync. BL-33's streaming undo reader is needed before replay reads large rev records |
| **ST3a** | up (test) | `World`, `FaultyEngine`, `FaultyFlatFiles`, `Sut`, `check_coins_profile`, `check_chainstate`, `RefExtractor`, `Scope`, `Mark` | CH2 implements `Sut` twice (§6.4) and the chainstate `RefExtractor` |
| **CS0** | up | `Block` / `Tx` decoders, `Tx::txid(rules)` (v24 stable asset-unlock txid), `CoreBytes`, `Release`/`UngatedRules` | — |
| **CS5a** | up | `Coin`, `Coin::{write, decode, serialized_size, height, is_coinbase}`, `TxUndo`/`BlockUndo`/`decode_block_undo`, `CoinsView`, `MemoryCoins`, `access_by_txid`, `MAX_OUTPUTS_PER_BLOCK = 222 222` | **Requests:** (a) `CoinScript::is_unspendable()` (Core `IsUnspendable`, `script/script.h:534-537`); (b) `CoreWrite` for a fixed buffer (`&mut [u8]` cursor), so `PackedCoin::encode` does not allocate; (c) keep `CoinsView` infallible, or choose Q4's alternative |
| **CH1** | up / peer | `BlockId`, `BlockRef`, `IndexAccess { lookup, ancestor, lca, height, have_data_pos, undo_pos, status }`, `take_dirty_index_records() -> Vec<IndexWrite>`, the `BlockIndexRecord` codec, `ChainEvent` bus with control events (`FlushCommit` bypasses the queue bound) | Add **i:CH1** to the CH2 row (Q6) |
| **CH3a** | down | `CoinsOverlay`, `OverlayView` (implements `CoinsView`), `CoinsCache::merge`, `failed()` check, `-par`-independent | `connect_block` writes undo (CS5a `BlockUndo::core_bytes`) through ST2 |
| **CH3b** | down | `flush_policy(mode)` call sites (Core's IF_NEEDED after connect/disconnect, PERIODIC at loop end, NONE after accept, ALWAYS at shutdown/RPC) | §3.3 |
| **CH4** | down | `PrefetchBatch { fetch_gen, entries }`, `CoinsCache::admit_prefetch`, `CoinsTable::multi_get` on `E::Snapshot`; W's block/undo append queue drained before step 2 | xcheck mode compares every lookup with a direct read |
| **CH5** | down | `wipe_chainstate`, `-dbcrashratio`, `open_chainstate` | runs `feature_dbcrash`, `feature_reindex` |
| **CH7a** | down | `StateDelta` (`EvoDelta`), the presumed-effect ledger `EffectLedger::{record_presumed(seq), current()}`, the effect classification (presumed vs independent) | the effect batches stay Buffered single-row (A1 §3.5.7) |
| **CH8** | down | `PruneDecision`, `PruneHost::{dry_run, select, clear_files}`, locks from `SubscriberHub` | turns pruning on; FTs |
| **IX1, CH7b, LK1b** | down | `SubscriberHub::register`, `LocatorKeeper` | — |
| **ND1** | down | `AbortSink`, `CacheSizes` logs, shutdown flush skipped after an abort | `InitRules.abort_exit_failure` |
| **MP1** | down | `B(t)` reads mempool usage; ATMP's coin fetches and `uncache` run as V commands | Core holds `cs_main` across ATMP (`validation.cpp:1418-1445`) |
| **O2a / O2b** | test | `oracle_coins_ops` (new, §6.2); `diff_chain` | new entry point (Q7) |

---
## 3. Core-equivalence

### 3.1 What CH2 replaces (both pins)

| Core | v23.1.8 / v24.0.0-rc.3 | Rust | Notes |
|---|---|---|---|
| `Coin`, `TxOutCompression`, VARINT | `coins.h:31-87`; `compressor.{h,cpp}` (byte-identical) | CS5a `Coin`; `PackedCoin` | the cache value is Core's serialization |
| `CCoinsCacheEntry` DIRTY/FRESH | `coins.h:89-133` | `Flags` (+ SPENT, which marks a tombstone without coin bytes) | legal states §3.2 |
| `CCoinsViewCache::FetchCoin/GetCoin/HaveCoin/HaveCoinInCache/AccessCoin` | `coins.cpp:44-68, 151-170` | `CoinsCache::{get, have, have_in_cache}`, `OverlayView::access` | — |
| `AddCoin`, `AddCoins` | `coins.cpp:70-108, 118-127` | `add`, `OverlayView::add` (CH3a calls with Core's `overwrite` rule) | unspendable skipped (quirk 33) |
| `SpendCoin` | `coins.cpp:129-149` | `spend` | FRESH erased, else tombstone |
| `Uncache` | `coins.cpp:284-297` | `uncache` | clean entries only |
| `BatchWrite` (cache→cache) | `coins.cpp:182-252` | `CoinsCache::merge` | §3.2 |
| `Flush` / `Sync` | `coins.cpp:254-282` | freeze + `FlushMachine`; `Sync` has no production caller at either pin | — |
| `DynamicMemoryUsage`, `GetCacheSize`, `ReallocateCache` | `coins.cpp:40-42, 299-323`; `memusage.h:170-187 / :186-203` | `usage()`, `live`, dropping a layer | allocated bytes, not live entries, as Core |
| `CCoinsViewDB::{GetCoin, HaveCoin, GetBestBlock, GetHeadBlocks, BatchWrite, Cursor, EstimateSize}` | `txdb.cpp:81-170, 194-265 / :90-179, 203-274` (byte-identical ranges) | `CoinsTable`, `Meta`, the flush's sub-batches | marker moved into B1 (A1 §3.5.4) |
| `CCoinsViewErrorCatcher` | `coins.cpp:355-369 / :355-379` | the `OverlayView` latch (§2.4) | v24's `HaveCoin` catch for both profiles (CONS-22) |
| `FlushStateToDisk`, `ForceFlushStateToDisk`, `PruneAndFlush` | `validation.cpp:2761-2928 / :2969-3148` | `flush_policy` (§3.3) + `FlushMachine` | §3.3, §3.4 |
| `GetCoinsCacheSizeState` | `validation.cpp:2729-2759 / :2937-2967` | `size_state` over `B(t)` | the same thresholds |
| `CalculateCacheSizes` and the `Cache configuration` lines | `node/caches.cpp:12-33 / :15-42`; `init.cpp:1946-1956 / :1986-2023` | `CacheSizes::calculate` | per profile (§3.8) |
| `ReplayBlocks`, `RollforwardBlock`, `ApplyTxInUndo` (coins part) | `validation.cpp:4879-4954, 4772-4877, 1966-1992 / :5222-5305, :5180-5220, :2252-2278` | `Recovery` | §3.5 |
| `BlockManager::FlushBlockFile`, `WriteBlockIndexDB` / `WriteBatchSync`, `UnlinkPrunedFiles` | `node/blockstorage.cpp:328-347, 553-588 / :333-352, 581-629`; `txdb.cpp:267-277 / :276-286` | ST2 barrier + B1; unlink after Bk | † |
| `BaseIndex::{Commit, ChainStateFlushed, Rewind}`, `m_synced`, `SYNC_LOCATOR_WRITE_INTERVAL` | `index/base.cpp:24, 78, 154-158, 187-192, 224-240, 263-268, 290-327 / :29, 90, 166-170, 202-207, 238-259, 281-286, 332-369` | `LocatorKeeper`, `SubscriberHub` | the cadence is capped (T33) |
| `-dbbatchsize`, `-dbcrashratio` | `txdb.cpp:105-110, 147-154 / :114-119, 156-163` | `FlushConfig` | §3.6 |
| `CheckDiskSpace` | `validation.cpp:2840-2846, 2879-2882 / :3048-3054, 3087-3090`; floor 50 MiB `util/system.cpp:156 / util/fs_helpers.cpp:109` | flush step 1 | — |

### 3.2 Cache semantics

**The equivalence argument.**
1. Freezing at tip `new` hands W every change since the previous durable tip `old`. F maps each key to its value at `new`, a tombstone, or a clean copy of the database value.
2. While F is being written, the database holds the state at `old` plus a subset of F's sub-batches. Every key in such a sub-batch is also in F, and F is consulted first. **So "F over the database" answers every lookup exactly as a database at `new` would**: Core's database right after a full flush at `new`.
3. Core's `Flush()` leaves the cache empty (`coins.cpp:254-265`). Our active layer starts empty at the freeze.
4. So the active layer is Core's `CoinsTip()` right after a flush at `new`, over a base whose `GetCoin` returns only unspent coins, as `CCoinsViewDB` does. **Every rule of `CCoinsViewCache` then applies to active unchanged.** In particular, no active entry is ever "spent and FRESH but not DIRTY": that state needs a parent that returns spent entries (`coins.cpp:52-56`).

Active's states are Core's four reachable ones: unspent clean, unspent DIRTY, unspent DIRTY|FRESH, and spent DIRTY (a tombstone).

**The two defensive checks** are where "Core's cache" and "our active layer" can differ. Core's cache holds everything since Core's last flush; ours since the last *durable* flush is active ∪ F. The checks therefore consult **active, then F**. This matches Core whenever Core's flush points coincide with ours, and it is stricter otherwise:
- `add(op, coin, possible_overwrite = false)` with `op` unspent in active **or** F → `OverwriteUnspent` (`coins.cpp:80-98`).
- `merge`: a child FRESH entry whose key is unspent in active **or** F → `FreshMisapplied` (`coins.cpp:217-223`).

Both are unreachable on valid chains.
- Non-coinbase outputs are added with `possible_overwrite = false` only after BIP30's `HaveCoin` check has passed, or under BIP34, where a duplicate txid requires a SHA-256 collision (quirk 10; R4 §1.15 #7: BIP30 is always on for devnet and regtest).
- Coinbase outputs are added with `possible_overwrite = true` (`AddCoins`, `coins.cpp:118-127`).

So the checks affect only caller bugs, which they turn into an abort, never a panic.

**FRESH marking consults active only.** For `add` with `possible_overwrite = false`, `fresh = !(active entry is DIRTY)`. When F holds a tombstone for the key and active has nothing, we mark FRESH where Core, whose cache still holds the tombstone, would not. The database outcome is identical:
- if the coin is spent again, active drops it, and F's flush has already erased the key;
- otherwise active writes the put after F's erase. Flushes are strictly sequential (one F), so the order holds.

The reference twin compares database contents and lookups, never flags.

**`merge(child)`**, for each DIRTY child entry in insertion order. Non-DIRTY child entries are skipped (`coins.cpp:187-189`).

| Active entry | Child | Result | Core |
|---|---|---|---|
| none | FRESH and spent | dropped | `:191-214` |
| none | otherwise | check F (the FRESH case above); insert with DIRTY, plus FRESH if the child is FRESH | `:191-214` |
| unspent | FRESH | `FreshMisapplied` | `:217-223` |
| FRESH (unspent) | spent | entry removed | `:225-229` |
| any other | any other | coin := child's; flags \|= DIRTY (a FRESH bit is kept); SPENT follows the child | `:230-247` |

**`get` fall-through.** A database or prefetch hit enters active as clean, as Core's `FetchCoin` caches it (`coins.cpp:44-59`). A hit in F is returned without copying. `have_in_cache` (Core's `HaveCoinInCache`, which ATMP uses to decide what to uncache, `validation.cpp:844-855 / :963-974`) looks at active ∪ F.

**The overlay** is Core's child cache over `CoinsCache::get`.
- It starts empty for every connect and disconnect (`validation.cpp:3189, 3048 / :3430, 3279`), so special-transaction processing, which runs before the UTXO loop, sees the pprev state (quirk 26).
- Its `add` marks FRESH without consulting the parent, exactly as Core's child does.
- CH3a merges it on success and drops it on failure.

### 3.3 `FlushStateToDisk`: decision and triggers

`flush_policy(mode, state) -> FlushAction` is evaluated on V at Core's call sites:
- `IF_NEEDED` after `ConnectTip` and `DisconnectTip` (`validation.cpp:3207, 3070 / :3457, 3304`);
- `PERIODIC` at the end of each `ActivateBestChain` step (`:3550 / :3851`) and after ATMP (`:1422 / :1748`);
- `NONE` after `AcceptBlock` (`:4471 / :4855`) and from `PruneAndFlush`;
- `ALWAYS` from `ForceFlushStateToDisk` (shutdown; `gettxoutsetinfo`, `scantxoutset` and `dumptxoutset` at `rpc/blockchain.cpp:1230, 2586, 2790 / :1238, 2771, 3173`).

| Core flag (`validation.cpp:2828-2838 / :3036-3046`, identical at both pins) | Our cause | No F in flight | F in flight |
|---|---|---|---|
| `mode == ALWAYS` | `Always` | freeze; wait for completion | wait for F's release, then freeze and wait |
| `fCacheLarge` = PERIODIC ∧ Σ > max(0.9·B, B − 10 MiB) | `CacheLarge` | freeze | pending until release |
| `fCacheCritical` = IF_NEEDED ∧ Σ > B | `CacheCritical` | freeze | **V waits** for release, then freezes (the only stall) |
| `fEvoDbCacheCritical` = IF_NEEDED ∧ EvoDB memory ≥ `64 << 20` (a literal at both pins) | `EvoCritical` (active's `StateDelta::bytes()` ≥ 64 MiB) | freeze | V waits, then freezes |
| `fPeriodicFlush` = PERIODIC ∧ now > last_flush + 24 h | `Interval` | freeze | pending |
| `fFlushForPrune` (selection non-empty) | `Prune` (the dry run D ≠ ∅, E20) | freeze with D | pending; D recomputed at release |
| `fPeriodicWrite` = PERIODIC ∧ now > last_write + 1 h | `IndexOnly` | index-only write | skipped (B1 of F covers it) |
| — (ours, A1 §3.4) | `Threshold`: active ≥ f·B | freeze | — (the limit rule applies) |
| — (ours, ST3a §5.6 item 6) | `DMax`: a subscriber at its limit asks | freeze | pending |
| — (A1 §3.4) | `LeftIbd` | freeze once | pending |

- **Timers.** `m_last_write` and `m_last_flush` are set to now on the first call, as Core does ("Avoid writing/flushing immediately after startup", `:2819-2826 / :3027-3034`). They advance at B1's return and at the freeze respectively. Neither pin randomises them, and neither do we.
- **Flush timing differs from Core's.** We freeze at f·B, which is earlier than Core's ~B − 10 MiB, so Core's `CacheLarge`/`CacheCritical` fire only while F is in flight. This is unobservable to peers and RPC: `gettxoutsetinfo` and the other copy points force a flush, and the only other trace is `debug.log` (§3.9 D1).
- **Disk space** (step 1, before any write): `CheckDiskSpace(blocks/, 0)` (50 MiB) for a full or index-only write, and `CheckDiskSpace(datadir, 48·2·2·entries(F))` for a full one (`:2840-2846, 2879-2882 / :3048-3054, 3087-3090`). Either failure aborts with "Disk space is too low!".
- **v24's periodic low-space check** ("Shutting down due to lack of disk space!", `init.cpp:1584-1590` v24, CONS-22 `v24-all`) is ND1's.

### 3.4 Order of operations

| # | Core (`validation.cpp:2761-2911 / :2969-3120`, the same order at both pins) | Ours (§4.2) | † (visible only after a crash or a disk fault) |
|---|---|---|---|
| 1 | prune selection against the **in-memory** tip, `WriteFlag("prunedblockfiles")` unsynced | step 0 dry run; selection at step 7 against `new`; `pruned_files` in Bk/Bp | DEC-59 Q9, DEC-64 |
| 2 | `CheckDiskSpace` | step 1 | — |
| 3 | `FlushBlockFile()`: the current blk/rev pair only, `void` on failure (`blockstorage.cpp:553-567 / :581-605`) | step 2: every file written since its last sync, plus the directory; abort on failure | A1 †, T6, T9 |
| 4 | `WriteBlockIndexDB()`, `sync = true` (`txdb.cpp:267-277 / :276-286`) | **B1** Durable: index, file infos, **evo**, `evo_tip`, marker | evo moves before coins (T4, T24) |
| 5 | `UnlinkPrunedFiles`, no directory fsync | after Bk/Bp, then `sync_dir` | T5 |
| 6 | `CoinsTip().Flush()`: the first batch writes H and erases B, partial batches, the final batch erases H and writes B, **all unsynced** (`txdb.cpp:104-165 / :113-174`) | coin sub-batches (Buffered) → `sync_wal` → **Bk** Durable | E3, E21 |
| 7 | `CommitRootTransaction()`, unsynced, **after** coins (`:2884→2889 / :3092→3097`) | already in B1 | Core's coins→EvoDB hole cannot occur |
| 8 | `ChainStateFlushed(locator)` after a full flush (`:2903 / :3111`) | after B1 (E16), then the subscriber commit step | DEC-64 |
| — | V holds `cs_main` for all of it | V only freezes | timing only |

### 3.5 `ReplayBlocks`

| Core (`validation.cpp:4879-4954 / :5222-5305`) | Ours (§4.3) |
|---|---|
| No heads → consistent; a size other than 2 → "ReplayBlocks(): unknown inconsistent state" | no marker → step 2 corruption check; malformed `head_blocks` → the same text |
| `pindexNew` unknown → "…reorganization to unknown block requested"; `pindexOld` unknown → "…reorganization from unknown block requested" | the same texts. `new` must have `HAVE_DATA` (I1) |
| EvoDB best block must equal the old head when DIP3 is active at `old` (v23 "ReplayBlocks(DASH): Found EvoDB inconsistency" / v24 `EvoDbInconsistencyMessage()`) | `meta.evo_tip == new` (B1 wrote it). Failure logs the profile's text |
| roll back with the full `DisconnectBlock` (EvoDB undo, v23 index writes); UNCLEAN tolerated; FAILED → "ReplayBlocks(): DisconnectBlock failed at %d, hash=%s" | blind undo of coins only; structural failures (read, size mismatch) use the same texts |
| `RollforwardBlock`: `ProcessSpecialTxsInBlock` (no script or merkle checks), `SpendCoin` per input, `AddCoins(…, check = true)` | blind: erase inputs, put spendable outputs. Evo is untouched |
| `cache.Flush()`, unsynced; EvoDB committed later in `LoadChainstate` (`node/chainstate.cpp:182-184 / :268-270`) | sub-batches, `sync_wal`, Durable final batch |
| any failure → init error "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." (`init.cpp:2066` / `node/chainstate.cpp:251-252`) | the same |
| logs "Replaying blocks", "Rolling back %s (%i)", "Rolling forward %s (%i)"; progress "Replaying blocks…" | the same |
| `-dbcrashratio` applies through `BatchWrite` (`feature_dbcrash` asserts `crashed_on_restart > 0`) | applies after each recovery sub-batch |

**Legacy undo** (an entry with `nHeight == 0`, written before Core 0.15) takes its height and coinbase flag from `AccessByTxid` (`validation.cpp:1966-1992 / :2252-2278`).
- Our own rev files never contain it.
- An imported rev file kept through `-reindex-chainstate` could, and only a transition whose disconnect leg reaches pre-2017 blocks could meet it.
- Recovery then resolves it **with a read**. This is the one narrow exception to blind replay: every output of a transaction carries the same height and coinbase flag, and Dash has no duplicate txids (v24 `fEnforceBIP30 = true`, "dash blockchain doesn't have any violations", `validation.cpp:2321-2322`). So any surviving output gives the right answer, and none gives Core's `DISCONNECT_FAILED`.
- ST3a's bound is unaffected, because its scenarios never contain legacy undo.

### 3.6 Strings and logs

| String | Core | Where in CH2 |
|---|---|---|
| "Disk space is too low!" | `validation.cpp:2843, 2881 / :3051, 3089` | step 1 |
| "Flushing block file to disk failed. This is likely the result of an I/O error." / "Flushing undo file to disk failed. …" | `node/blockstorage.cpp:562, 553 / :600, 581` | step 2 (ST2 error mapped by file kind) |
| "System error while flushing: %s" | `validation.cpp:2908 / :3117` | any engine error in a flush step |
| "Failed to write to block index database" | `:2858 / :3066` | not emitted: Core's path throws first (`dbwrapper.cpp:260-268`), so its user-visible text is the "System error" one, and ours matches |
| "Error reading from database, shutting down." + "Error reading from database: %s" | `init.cpp:2026-2030 / :2087-2091`, `coins.cpp:355-369 / :356-379` | `OverlayView` latch |
| "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." | `init.cpp:2066 / node/chainstate.cpp:251-252` | `open_chainstate` |
| "Found EvoDB inconsistency, you must reindex to continue" | `validation.cpp:2001-2006 / :1923-1929` | the no-marker corruption check |
| "Simulating a crash. Goodbye." then `_Exit(0)` | `txdb.cpp:147-154 / :156-163` | after each partial coin sub-batch (flush and recovery); never after the final batch |
| coindb category: "Writing partial batch of %.2f MiB", "Writing final batch of %.2f MiB", "Committed %u changed transaction outputs (out of %u) to coin database..." | `txdb.cpp:144, 161, 163 / :153, 170, 172` | W's coin phase |
| bench category: "write coins cache to disk (%d coins, %.2fkB)", "write block and undo data to disk", "write block index to disk", "unlink pruned files" | `validation.cpp:2848-2866, 2871 / :3056-3074, 3079` | W, timed per step |
| "Cache size (%s) exceeds total space (%s)" | `:2753 / :2961` | when Σ > B(t) |
| "Cache configuration:", "* Using %.1f MiB for …" | `init.cpp:1946-1956 / :1986-2023` | ND1 prints `CacheSizes` |

### 3.7 R4 quirks touched

- **33:** unspendable outputs never enter the UTXO set (cache and replay both skip them). The `hash_serialized_2` VARINT bug is CS5b/IX1's; CH2's cursor yields Core's key order for it.
- **28:** a missing rotation snapshot silently changes member selection (`llmq/utils.cpp:496-503` at v24). This is why snapshot effects are **presumed** and covered by the pre-B1 barrier (§4.1).
- **26:** special transactions never see same-block outputs, because the overlay starts empty (§3.2).
- **10** and **7:** BIP34 and BIP30 make the two defensive checks unreachable (§3.2).

### 3.8 Per-profile behaviour

| Behaviour | v23.1 | v24.0 | Owner and mechanism |
|---|---|---|---|
| Cache split: address/timestamp/spent index shares before the coins share | none (indexes in the block-tree DB, 2 MiB) | carved out (`node/caches.cpp:25-30` v24) | `IndexRules.dash_index_model` (MPIX-08/10) → `CacheSizes::calculate` |
| `Cache configuration` lines for those indexes | absent | present (`init.cpp:1992-1998` v24) | the same rule |
| Startup sizing | sized once | 20 % first, then `MaybeRebalanceCaches` → `ResizeCoinsCaches` + an `IF_NEEDED` flush and two "resized … cache" log lines (`node/chainstate.cpp:225-293` v24) | **log-only**: rdashd sizes once and prints v24's two lines under V24_0 (Q10). The `IF_NEEDED` flush is a no-op at startup |
| Coins read error in `HaveCoin` | propagates | aborts | `v24-all` (CONS-22): abort under both |
| AbortNode exit status | 0 | 1 | `InitRules.abort_exit_failure` (P2P-31), ND1 |
| Replay's EvoDB-mismatch log text | "ReplayBlocks(DASH): Found EvoDB inconsistency" | `EvoDbInconsistencyMessage()` | logged per profile; the init error is the same text at both |
| `GetCoinsCacheSizeState` mempool term | `-maxmempool` arg | the mempool's maximum | the same value in a node with a mempool |
| rev-file finalize rule, startup prune scan | BL-29 / CONS-28 | | ST2's `Release` |
| Everything else in §3.1–§3.6 | identical | identical | — |

No new O17 row is needed. The rebalance log lines are not in O17; if the manager wants them modelled they become a `ChainstateRules.rebalance_log` field (Q10).

### 3.9 Declared deviations (none observable to peers or RPC)

| # | Deviation | Why it is unobservable |
|---|---|---|
| D1 | Flush timing and frequency (f·B, asynchronous) | copy points force a flush; only debug-log timing differs |
| D2 | FRESH can be set where Core's cache would hold a tombstone (§3.2) | the database outcome is identical; the twin compares contents |
| D3 | The defensive checks consult F | unreachable on valid chains |
| D4 | All of §3.4's † | visible only after a crash or a disk fault; strictly more durable (US-NODE-04) |
| D5 | Replay is coins-only; evo is committed at `new` in B1 | the recovered state is identical |
| D6 | No XOR obfuscation | own format (DEC-11) |
| D7 | The cursor reports `Corruption` where Core's stops silently | only on a corrupt DB |
| D8 | Unlinked pruned files are directory-synced | durability only |

---
## 4. Persistence, crash consistency and recovery

The protocol is ST3a §6.2's toy order (A1 §3.5 plus errata E1–E22) on real components. ST3a §5.6 is the contract; §4.7 maps each of its 13 items to this design.

### 4.1 Barriers, finalizers and scopes

| Commit | Writer | Kind | Finalizes (ST3a §2.4.4) | Required before it |
|---|---|---|---|---|
| effect row (rotation snapshot, …) | V | Buffered, single row | — | — |
| **B1** | W | **Durable** | presumed effects returned before the freeze; the previous chainstate finalizer | files barrier (I1); `sync_wal` iff `packet.presumed_effects_seq > covered_seq` |
| coin sub-batch Bj | W | Buffered | — | B1 returned |
| **Bk** | W | **Durable** | this flush's sub-batches | every Bj returned, then `sync_wal` started and returned |
| **Bp** (one-batch flush that prunes) | W | **Durable** | — (B1 is durable) | the second files barrier |
| index-only write | W | Durable | — | files barrier |
| recovery sub-batch | recovery | Buffered | — | the open-time `sync_wal` |
| **recovery final** | recovery | **Durable** | recovery's sub-batches | `sync_wal` |
| subscriber data | subscriber | Buffered | — | — |
| **locator** (commit step, checkpoint, cadence, rewind) | subscriber | **Durable** (all four; ST3a's toy does the same) | the subscriber's data through X, all **submitted**; cross-store: X not ahead of `T_ack` | submit staged data through X and wait for it, then `sync_wal`, then the locator |
| wipe flag set / clear | CH5 via CH2 | Durable | — | — |

- **The presumed-effect ledger** (CH7a classifies effects).
  - V writes a presumed effect synchronously and, once the write returns, bumps `EffectLedger.seq`.
  - The packet captures `seq` at the freeze. Every effect for blocks up to `new` was written before the freeze, on V's thread.
  - W's pre-B1 `sync_wal` starts after the freeze, so it covers them ("returned before the barrier started", ST3a §2.4.1). W records `covered_seq` only after the barrier returns.
  - Effects V writes after the freeze belong to blocks above `new`. B1 does not presume them; the next B1 does.
- **The previous finalizer.** Flushes are strictly sequential (one F). Bk or Bp is Durable and returns before W reports the flush done, and recovery's final batch returns before V starts. So B1's `PreviousFinalizer` scope is always covered, at no cost (T34 cannot arise).
- **WAL order** is the engine's serialization order, never call order (§5.6 item 11). Every rule above is stated as "returned before the barrier started".

### 4.2 The flush, step by step

**V at the decision** (§3.3). These steps run on V, at a block boundary:
- **Step 0:** if a prune check is due, run the dry run under the predicted post-commit locks (`SubscriberHub::predicted_locks`) and retain D and `met_target` (§4.5).
- **Freeze:**
  1. `F = mem::take(active)`.
  2. Take the evo delta of the window (CH7a).
  3. Take every dirty `block_index` record (CH1). The `HAVE_DATA`/`HAVE_UNDO` bits in them were set after their appends returned, so the step-2 barrier covers their bytes.
  4. Capture the effect seq.
  5. Set `old` to the durable tip and `new` to V's tip.

  Building the packet costs one pointer swap plus serializing the dirty index records: about 150 B per block in the window, so a few milliseconds for thousands of blocks.

**W**, one `FlushMachine::step` per row. Marks are ST3a's names.

| Step | Store calls | Mark / log | On error |
|---|---|---|---|
| begin | — | `FlushBegin { old, new }` | — |
| 1 Space | `available_space(blocks/)` ≥ 50 MiB; `available_space(datadir)` ≥ 50 MiB + 192·entries(F) | — | abort "Disk space is too low!" before any write |
| 2 Files | lock `BlockFiles`; drain W's append queue through `new` (CH4; empty before CH4); `sync_and_take_dirty_infos()` → `d1` (`fdatasync` of every file written, `fsync(blocks/)` if needed); unlock | bench "write block and undo data to disk" | abort with the flat-file string; no retry (fsyncgate) |
| 3 Presumed barrier | `sync_wal()` iff `packet.seq > covered_seq` | — | "System error while flushing: …" |
| 4 **B1** | one batch from `batch_with_capacity`: index records; `d1` infos into `block_files`; `meta.last_file`; `delta.write_b1`; `meta.evo_tip = new`; then **either** one-batch (all of F's coins, `meta.tip = new`, no marker; when `F.dirty_bytes + rest ≤ S`) **or** split (`meta.head_blocks = [new, old]`, erase `meta.tip`); `write(Durable)` | bench "write block index to disk" | as step 3 |
| 5 Durable | publish `T_ack = new`. Under the hub's lock, capture the set of mandatory subscribers that are **Synced now**. Enqueue the control events `ChainStateFlushed { locator(new) }` and `FlushCommit { flush, new, to: that set }` (not counted against the queue bound) | `Durable { flush }` | — |
| 6 Subscribers | wait for an ack from every subscriber in step 5's set. A subscriber that turns Synced later is not awaited: it makes its own capped commit as it turns, under the same lock (ST3a S11s; T35). In split mode W may write step-8 sub-batches while it waits (`interleave = true`; the seam explores both settings) | `SubscriberState`, `Locator` (emitted by subscribers) | a subscriber error aborts the node (global latch) |
| 7 Prune select | only if `flush_for_prune`: `D ∩ eligible(actual)`; ask V to clear the in-memory index entries of those files (V returns the cleared records); `prune_one_block_file` for each; re-arm per FA2 | `PruneSelected`; "limited pruning…" was logged at step 0 | — |
| 8 Sub-batches (split) | over F's DIRTY slots in arena order: tombstone → `delete(key)`, else `put(key, bytes)`. Close a sub-batch when `size_bytes() > S` (Core's `SizeEstimate() > batch_size`, `txdb.cpp:143-153 / :152-162`), write it Buffered, then roll `-dbcrashratio`. The last partial chunk is kept for Bk | coindb "Writing partial batch of %.2f MiB" | as step 3; `_Exit(0)` on the crash roll |
| 9 Pre-Bk barrier (split) | `sync_wal()`, started after every Bj returned | — | as step 3 |
| 10 **Bk / Bp** | Bk (split): the last coins; the second `sync_and_take_dirty_infos()` → `d2`; the prune clears; `meta.pruned_files` if any; `meta.tip = new`; erase `meta.head_blocks`; `write(Durable)`. Bp (one-batch with prune): clears, `d2`, `pruned_files`. Neither (one-batch, no prune): nothing | coindb "Writing final batch…", "Committed %u changed transaction outputs (out of %u)…"; bench "write coins cache to disk (%d coins, %.2fkB)" | as step 3 |
| 11 Release | send `FlushDone { gen }` to V; drop W's `Arc<FrozenLayer>`; unlink the pruned files in a `BestEffort` region; `sync_dir(blocks/)` | `BestEffortBegin/End`; bench "unlink pruned files" | an unlink error is logged and left for the startup scan (ST2, CONS-28) |
| 12 Index-only (separate machine) | Space(blocks/) → files barrier → one Durable batch of index records, file infos and `last_file` | — | as above |

**V's side of the flush.**
- **Release.** At its next poll after `FlushDone`, V drops F, increments `released_gen`, clears `prefetch` (A1 §3.4's fence) and, if a freeze is pending, freezes at once.
- **Every V wait is a service loop.** That covers the CRITICAL wait for release, the ALWAYS wait, and the event-queue backpressure wait. The loop answers W's requests (prune clears), `FlushDue` requests from subscribers, and read-only `ChainCommand::Query`s (V is at a block boundary, so its state is coherent). This is what makes the system deadlock-free (§4.4).
- **ALWAYS** (shutdown, `gettxoutsetinfo`, …): wait for any in-flight F, then freeze the rest and wait. On return, the database is at V's tip.
  - If active is empty and the tip equals `meta.tip`, it runs the files barrier, an index-only write and `ChainStateFlushed` (as Core's empty-cache full flush does).
  - On regtest it then also runs `flush_memtables` and `sync_wal` (A1 §2.9 live-copy rule).

**Concurrency rules** (normative):
1. V is the only mutator of active, the overlay, prefetch, the block index and effect rows.
2. W is the only writer of B1, Bj, Bk/Bp and index-only batches. Recovery runs before V starts.
3. F is immutable after the freeze.
4. `BlockFiles` sits behind a mutex. V appends blocks and undo. W's barrier holds the mutex for `sync_and_take_dirty_infos`, so V's appends wait during the `fdatasync` (Core blocks V for the whole flush; ST2's two-phase barrier is lever L6).
5. W never counts V's concurrent writes as covered. Coverage is always by explicit returned-before-started order (E18).
6. Under the seam (`InlineDriver`), every store call runs on the test thread, the salt is fixed, and parallel sub-batch writes (L4) are off (ST3a §4.9).

### 4.3 Recovery, step by step (`open_chainstate`)

| # | Action | Notes |
|---|---|---|
| 0 | open the engine through `GuardedEngine`; **`sync_wal`** before any write (E21) | every subscriber engine does the same in its own open |
| 1 | load prune locks from every mandatory subscriber's durable locator at `height(LCA(locator, recovered tip))` (no locator: its start height), before anything in the epoch can prune | ST3a §5.5 step 0, T28 |
| 2 | open `BlockFiles` from the durable `block_files` and `meta.last_file`, never from file lengths; run the startup scan if `meta.pruned_files` (a `BestEffort` region) | ST2; CONS-28 |
| 3 | load the block index (CH1; PoW against the stored hash) | — |
| 4a | **no marker:** if `meta.tip` is absent the chainstate must be empty (a fresh node), else `Corruption`. If present, `meta.evo_tip == meta.tip`, else "Found EvoDB inconsistency, you must reindex to continue". Done | unreachable by construction; a corruption check |
| 4b | **marker `[new, old]`:** `new` is in the index with `HAVE_DATA`; `old` is null or in the index; `evo_tip == new`. Any failure → "Unable to replay blocks…" (the init error) after logging Core's specific text (§3.5) | — |
| 5 | `RecoveryBegin`; log "Replaying blocks". `fork = LCA(old, new)`, or none when `old` is null, in which case forward replay starts at height 1 (genesis outputs are never in the UTXO set) | — |
| 5a | **undo leg**, blocks `old` down to `fork + 1`. Read the block and its undo (`BlockFileReader`, checksum checked); check that `vtxundo.size() + 1 == vtx.size()` and that every tx's `vprevout` count equals its inputs. Per tx in reverse: `erase` each spendable output's key; `put` each restored input coin, in reverse input order. A legacy entry is resolved by the read of §3.5. Log "Rolling back %s (%i)" | blind: no coin read |
| 5b | **forward leg**, `fork + 1` up to `new`. Per tx in order: if not coinbase, `erase` every input; `put` every spendable output as `Coin(out, height, coinbase)` keyed by `Tx::txid(rules(height))`. Log "Rolling forward %s (%i)" | blind |
| 5c | ops accumulate into sub-batches of S, each written Buffered with the marker still in place, each followed by the `-dbcrashratio` roll; `replayed.push(b)` | an interruption leaves the marker, so recovery re-runs (I4) |
| 6 | `sync_wal`; final Durable batch: `meta.tip = new`, erase `meta.head_blocks`; `RecoveryEnd` | E4, E21; nothing else is written (R5: the index is never touched) |
| 7 | `T_ack` := the recovered tip. Subscribers recover: blind rewind of `(L, L + D_max]`, then catch-up from L to G along `IndexAccess`'s path, with block and undo data | `LocatorKeeper::recover` |
| 8 | hand the tip to CH3b: `LoadChainTip`, `VerifyDB`, `activate_best_chain` through the ordinary `connect_block` (I3) | — |

**Why replay converges (I2, I4).**
- The database before step 5 is the state at `old`, plus B1, plus any subset of this flush's Bj (each atomic), plus any subset of an earlier interrupted recovery's sub-batches.
- Let K be the set of keys the transition `old → new` touches. **F only ever writes keys in K**: F's dirty entries are exactly the outpoints created, spent or restored by connects and disconnects in the window. Net-zero reorgs inside the window rewrite a key with its value at `new`.
- Replay writes every key in K (the undo leg's outputs and restored inputs, the forward leg's inputs and outputs).
- Replay's last write to each key is that key's value at `new`. Its op order is the order Core's disconnect and connect apply, and within a batch the later op wins (`WriteBatch` semantics).
- Keys outside K are untouched by F and by replay. So whatever subset survived, the state after step 6 is the state at `new`.
- Re-running recovery repeats the same ops, so the result converges.
- No step reads a key it writes, so ST3a's per-image bound stays polynomial (item 13). The one exception is the legacy-undo read of §3.5.

### 4.4 Subscribers in the real system

ST3a §5.6 item 6 defines the rules over a single-threaded toy. In the node, the toy's queue is the ordered `ChainEvent` stream. CH2 maps it as follows.

- **Classes.** Each mandatory subscriber (DEC-63: `llmq/` plus every enabled index) tells the hub its class at each transition, exactly where Core sets `m_synced`.
  - It starts Synced if its locator is at the tip at init (`index/base.cpp:78 / :90`).
  - It becomes Synced after `ThreadSync`'s final commit (`:154-158 / :166-170`).
  - The class is never inferred from "has it processed `new`" (T35).
- **Commit step.** W enqueues `FlushCommit { new }` after B1 returns. Events are delivered in order, so when a Synced subscriber reaches it, it has processed `Connected(new)`. It may also have processed more blocks that V queued before W's request (the DEC-69 case). It then:
  1. computes `X = LCA(p, new)` (block-index ancestry is immutable data, readable on any thread, A1 §2.10.2);
  2. runs `keeper.commit(X, CommitStep)`: submit staged data through X (in practice, through p), wait for the write to return, `sync_wal`, Durable locator;
  3. acks.

  Data in `(X, p]` lies inside the blind rewind range. A rewind commit has already lowered L to any fork below it, so `X ≥ h(L)` and `p ≤ h(L) + D_max`.
- **The `D_max` gate.** Before processing `Connected(b)` with `h(b) > h(L) + D_max`:
  - **Synced, with an outstanding `FlushCommit`** (issued to this subscriber and not yet acked, even if it is still behind the subscriber in the queue; its B1 has returned by construction): checkpoint, then continue. That is: submit through p, `sync_wal`, then a Durable locator at `LCA(p, T_ack)`, which is p on the connect leg up to `new` and `new` beyond it. Either way it is above L on a stable chain, because `L ≤` the previous `T_ack`, below `new`. After a reorg below L, the rewind commit has already lowered L to the fork. This is ST3a's step-6 checkpoint (T37, T38, T41).
  - **Synced, with no outstanding commit:** post `FlushDue(DMax)` to V and wait for a `FlushCommit`, still processing disconnects. Then checkpoint as above.
  - **Syncing:** make a capped commit at `LCA(progress, T_ack)`. If that does not raise L, post `FlushDue(DMax)` and retry after the next `T_ack`.
- **Rewind commit.** Before processing a disconnect below `h(L)` (fork F a strict ancestor of L), make one Durable batch that blindly deletes the data keys of `(h(F), h(L) + D_max]` (`delete_range`, BL-38) and sets the locator to F. This is Core's `BaseIndex::Rewind` (`index/base.cpp:224-240 / :238-259`); a Buffered one is T36.
- **Cadence** (Syncing). Every `SYNC_LOCATOR_WRITE_INTERVAL` (30 s, `index/base.cpp:24 / :29`), commit capped at `LCA(progress, T_ack)`. Core's uncapped commit is T33.
- **Recovery.** Run `LocatorKeeper::recover(G)`: delete `(L, L + D_max]` without reading, then re-apply `BlockConnected` along L → `LCA(L, G)` → G with block and undo data. A subscriber re-enabled past pruned data gets Core's refusal (`index/base.cpp:110 / :122`).
- **Non-mandatory subscribers** (wallets) receive `ChainStateFlushed` as Core's do and are never waited for.

**Why there is no deadlock** (the toy is single-threaded, so this argument is new here). The wait-for edges are:
- V → W (CRITICAL, ALWAYS);
- W → Synced subscribers (step 6) and W → V (prune clears);
- subscriber → dispatcher (events) and subscriber → V (`FlushDue`);
- dispatcher → subscribers;
- V → dispatcher (backpressure).

The edges are broken as follows:
1. Every V wait services W requests, `FlushDue` and read-only queries (§4.2). So no edge *into* V can block while V waits. A wallet that queries V from inside a callback cannot close a cycle.
2. Control events bypass the queue bound, so W never blocks on enqueueing.
3. A subscriber at its limit needs only `T_ack` to advance, which needs only W's B1. W reaches B1 without any subscriber, and a pending `FlushDue` makes V freeze at its next poll.

Hence every cycle contains a non-blocking edge. W's step-6 wait has no timeout (correctness); it logs a warning after 60 s.

### 4.5 Prune slots (mechanism here, policy in CH8)

- **Step 0 (V).** When ST2's `take_check_for_pruning()` or a manual height says a check is due, `PruneHost::dry_run` calls `find_files_to_prune` or `find_files_to_prune_manual` against `new` under `predicted_locks`.
  - It logs "limited pruning to height %d" from those locks (`validation.cpp:2784-2798 / :2992-3006`).
  - `flush_for_prune ⇔ D ≠ ∅` (`:2812 / :3020`). An automatic check is cleared here (`:2809 / :3017`).
- **Step 7 (W).** Select `D ∩ eligible(actual)`, with locks at `height(LCA(durable locator, new)) − PRUNE_LOCK_BUFFER − 1`.
  - The usage target is never re-applied (T39). Re-arm iff the target is still unmet while D had met it (T40).
  - V clears `HAVE_DATA`/`HAVE_UNDO` and the positions in memory, as Core does at selection. The durable index still says `HAVE_DATA` until Bk, and the files exist until after Bk, so a crash in between is consistent.
- **Bk/Bp** carry the cleared records, ST2's reset infos (`d2`) and `meta.pruned_files`. The unlink follows the return.
- **CH2 tests this mechanism** with ST2's manual selector on S5-shaped scenarios (§6.4 C10). CH8 adds `-prune`, the RPC, the incompatibility errors, the moveback-log emulation and the FTs.

### 4.6 Errors and disk full (I5)

- **One global latch.** `GlobalFailStop` is shared by `GuardedEngine` (chainstate and every subscriber engine) and wraps ST2's own `failed` flag.
  - The first error from a write, a barrier, `fdatasync`, `fsync` or a flat-file write latches it, emits `StoreFailed`, and calls `AbortSink::abort` with Core's string (§3.6).
  - After that, every mutating call in every store is refused (ST3a R8 across stores). W stops at the step that failed.
  - ST1 already latches per engine, including RocksDB's background errors, which RocksDB itself would auto-resume after ENOSPC (ST1 §9.5 D4). The global latch extends that to cross-store ordering.
- **Kinds.** Engine `NoSpace` and `Io` both map to "System error while flushing: …", as Core's runtime_error catch produces. A flat-file sync failure maps to the "Flushing block file…" or "Flushing undo file…" string.
  - A failed `fsync` is never retried (fsyncgate; ST3a §2.2.5).
  - Our flush stops after a failed block-file sync. Core continues writing the index, coins and EvoDB (†, `blockstorage.cpp:600` returns `void`).
- **Reads.** A coins read error trips the latch (§2.4). A read error during recovery ends it with an init error and no mutation after the fault (ST3a §4.4).
- **Shutdown after an abort** skips the shutdown flush (ND1, A1 §3.5.6). The next start recovers from the last completed barrier.
- **`-dbcrashratio=N`**: `_Exit(0)` with probability 1/N after each partial coin sub-batch, in flushes and in recovery. It is checked neither after B1 nor after the final batch, as in Core.

### 4.7 The ST3a §5.6 contract, item by item

| # | Item | Here |
|---|---|---|
| 1 | Generic over `E: KvEngine`, `F: FlatFs`, monomorphised | `FlushMachine<E, F, D>`, `Recovery<E, F>`, `CoinsCache<E>`, `LocatorKeeper<E>` |
| 2 | Synchronous seams: flush steps, startup recovery, subscriber recovery, V-ahead | `FlushMachine::step`, `Recovery::step`, `LocatorKeeper` methods, `InlineDriver`; V-ahead = the seam calls V's connect between W steps |
| 3 | Determinism: fixed salt under the seam; no store call on worker threads | §2.3 (iteration is salt-free), §4.2 rule 6 |
| 4 | Marks | `FlushObserver` (§2.7), adapter to `World::mark`; `Connected { applied }` from CH3a |
| 5 | The barrier rule with scopes; Bk, Bp and recovery final Durable; B1 includes the previous Bk/Bp; `sync_wal` at open; W does not count V's writes | §4.1, §4.2, §4.3 step 0; the chainstate `RefExtractor::finalizes` returns those scopes (§6.4) |
| 6 | Subscribers: `m_synced` classes, synced commits at `LCA(p, new)`, capped cadence, Durable rewind, backpressure and checkpoint, path drain, submit-before-sync, blind recovery, declared needs, the refusal | §2.9, §4.4 |
| 7 | Prune decision and locks | §4.5 |
| 8 | Check profiles | `coins_cache_model` → `check_coins_profile`; `chainstate_crash` → `check_chainstate` (§6.4–§6.5) |
| 9 | Digest scope: `state_digest` excludes meta, block_index, block_files, coins and effect CFs | the `ChainSut` views (§6.4) |
| 10 | `RefExtractor`: standing cross-store requirements; G = the durable marker's `new`, else `meta.tip` | §6.4 |
| 11 | WAL order is the engine's, never call order | §4.1 |
| 12 | `manual_wal_flush = false` | ST1 request (b) |
| 13 | Blind replay and rewind | §4.3, §4.4; the one legacy-undo read (§3.5) |

**I1–I5 on the real code.**
- **I1** (data before reference): B1's index records were snapshotted at the freeze, after their appends returned, and B1 follows step 2's barrier. Bk's `d2` comes from its own barrier. The index-only write runs behind its barrier.
- **I2** (every prefix, under subset semantics, recovers): §4.3.
- **I3** (no validation bypass): replay touches only the marker's transition, whose blocks were connected with an `applied` mark before the freeze. Everything above it is CH3b's ordinary activation.
- **I4** (idempotent recovery): §4.3.
- **I5** (fail-stop): §4.6.

---
## 5. Performance

### 5.1 Metrics CH2 moves

| Metric (A1 §3.9) | How CH2 affects it | Budget |
|---|---|---|
| #1/#2 IBD, #3 reindex and `reindex-chainstate` | coin misses on V; flush stalls on V; write volume and compaction competing for CPU and I/O | ≤ dashd at equal `-dbcache` and `-par`; the levers below are the 2× route |
| #5 connect latency at the tip | post-flush misses (Core empties its cache at every flush); a flush stall at the tip | ≤ dashd p50/p95 |
| #9 startup | open + WAL replay; recovery replay after a crash | ≤ dashd; replay is lighter than Core's (no special-tx re-execution) |
| #10 RSS | B (honest, allocator-checked) + E (74 MiB) + transient batch reserve | ≤ dashd peak and steady (NFR-RES-01) |
| #11 disk | `coins` and evo CFs in RocksDB against LevelDB; compaction peaks per CF | ≤ dashd per component (ST1: same size, writes less, OI-4) |
| #12 CPU per block | encode on add, decode on read, merge, flush memcpy | PF0 instructions |
| shutdown (US-NODE-04) | the final ALWAYS flush | `shutdown_s` ≤ dashd |
| **flush latency** | V's stall per flush | ≤ dashd's V stall (its whole flush) |

### 5.2 Memory, density and windows (arithmetic; PF1 measures before any claim, A1 §3.4)

| | Core (both pins) | rdashd |
|---|---|---|
| Bytes per cached P2PKH coin | **115.67** (measured probe, `memusage.h` pool formula) | **≈ 80** (72 slot + 5/load index; 77.7–83.4) |
| Bytes per compressed-P2PK coin | ≈ 180 | ≈ 128 |
| `-dbcache=300`, txindex on, empty mempool: B = 538.85 MiB | flushes at B − 10 MiB = 528.85 MiB ≈ **4.79 M coins**, then empties | active ≤ f·B ≈ 269 MiB; active + F ≤ B − R ≈ 522 MiB ≈ **6.8 M coins in memory** (mainnet's UTXO set: 6.42 M) |
| DEC-43 cell (no allowance): B = 252.75 MiB | flushes at 242.75 MiB ≈ 2.20 M coins | active ≤ 126 MiB ≈ 1.65 M; active + F ≤ 236 MiB ≈ **3.1 M** |
| Engine memory | 2 MiB block-index LevelDB + 8 MiB chainstate (4 cache + 2×2 write buffers) + 64 MiB EvoDB + mmapped SSTs (RssFile) | E = 74 MiB: one LRU with memtables charged to it; SSTs in the page cache through `pread` |

- **Reads favour us.** At any time we hold more recent coins than Core: Core's cache ramps 0 → 0.9B and empties, while ours stays between f·B and B − R.
- **Writes favour Core at f = 0.5.** A coin created and spent between two of Core's flushes never reaches disk (FRESH). Our FRESH window is one active layer (f·B, about half of Core's maximum window), so more coins reach the database as a put and later a delete.
  - That cost is on W and in compaction, off V's path.
  - PF3 measures the write volume on the traces (`coins-replay` reports puts and deletes per block against the LevelDB Core-model arm) and tunes f.
  - Lever L3 lets f approach 0.8.

### 5.3 V's critical path

- **Freeze:** a pointer swap plus building the dirty-index list (about 150 B per block in the window).
- **Stall (CRITICAL):** only when active reaches `B − |F| − R` before W releases F. With f = 0.5, W must write F's dirty set before V adds an equal number of entries.
  - RocksDB inserts Buffered batches at well over 10⁵ keys/s (ST1 D2: 256 MiB of random coin puts in 25 s on a contended disk).
  - V's coin creation in `reindex-chainstate` is a few thousand per second (hypothesis; PF1 measures).
  - So the stall should be rare, and V-visible flush latency about 0.
  - Core's V stall is the whole flush: block files, index, coins and EvoDB (§3.4).
- **Misses.** A miss is a synchronous RocksDB `get` until CH4's prefetch lands.
  - RocksDB's miss path costs about 1.3× LevelDB's CPU per event (ST1 OI-2), and its miss latency is higher in tight-memory cells (OI-1).
  - The density and window advantages (§5.2) cut the miss count; partitioned filters are on.
  - The `coins-replay` workloads count both (§5.7).
- **Shutdown:** in-flight F + active. That is ≤ B in the worst case, against Core's ≤ B − 10 MiB, both synchronous. Typically much less, because W has been flushing all along. `close()` does not wait for compactions.

### 5.4 W's throughput

- Serialization is a memcpy of stored bytes into the batch: no re-serialization, no XOR (Core does both, `dbwrapper.h:96-123 / :100-127`).
- Sub-batches go out in arena order. Durable fsyncs per flush: 2 (B1 and Bk), plus the pre-Bk `sync_wal`, plus 1 if a presumed effect is pending. Each subscriber adds 2. At 3–5 ms each (ST1 D1), against flushes minutes apart, that is noise.
- The memtable copy of a sub-batch is charged to E. A write stall (memtables full, flush pending) slows W, not V (ST1 OI-7). The W-MC workload records RocksDB's stall counters.

### 5.5 The DEC-43 low-memory cell

- **Configuration** (both sides): `-dbcache=300`, no allowance lent (full mempool or `-maxmempool` at its floor), txindex as node-bench sets it.
- **Expected:**
  - **equal B**;
  - our E (74 MiB) equal to Core's three LevelDB caches;
  - ≈ 1.45× the coins per MiB;
  - V never blocked for a flush.
- **The risks:**
  - RocksDB miss CPU (OI-2);
  - double caching under memory pressure (OI-1). The page-cache-leaning split above is DEC-43 (b)'s option 2; direct I/O would need a large block cache taken from B;
  - background compaction CPU. `max_background_jobs` comes from A1's compute budget.
- **Evidence:** CH2 cannot run `node-bench` end to end, because `connect_block` is CH3a. So:
  1. **At CH2's merge (proxy):** `coins-replay-*-dec43` against ST1's LevelDB Core-model arm on the same traces, with the same cache numbers (Core's coins share, its 8 MiB chainstate DB, a separate 64 MiB LevelDB for the evo stand-in), 5 rotations, paired. Pass when total time and V-stall time are ≤ the Core model (sign test, every pair) and RSS (anon + file) ≤.
  2. **At the M3 exit (the real cell):** node-bench's DEC-43 cell, rdashd against dashd, `reindex-chainstate` over the pre-DIP3 window, gated on `reindex_time_s` and `peak_rss_mib` (Q1).

### 5.6 The route to 2× (each lever measured by PF0/PF3; none is a claim yet)

| Lever | Removes | In base? |
|---|---|---|
| L1 arena density | misses and flush count at equal `-dbcache` | yes |
| L2 asynchronous freeze-and-flush | dashd's flush stall on V | yes |
| L3 **segmented F**: partition F at the freeze into k sub-layers by hash prefix, one per sub-batch; release each once written (V drops it and clears that segment's prefetch keys, gen per segment) | F's memory held to Bk, so f can rise to ~0.8: bigger FRESH windows, fewer writes | PF3 |
| L4 parallel sub-batch commits (disjoint keys; all returned before the pre-Bk barrier; off under the seam) | W latency on many-core hosts | PF3 |
| L5 SST ingestion for large flushes (`SstFileWriter` + `IngestExternalFile`, durable on return, before the pre-Bk barrier) | memtable and WAL cost, write stalls | experiment; needs an ST1 API |
| L6 ST2's two-phase files barrier | V's appends waiting on W's `fdatasync` | follow-up |
| L7 CH4's S2 prefetch (the fence is here) | miss stalls on V | CH4 |
| L8 warm retention: after release, keep F's clean unspent entries as a read-only layer below active while the budget allows (Bitcoin's later `Sync` idea) | post-flush misses at the tip (#5) | PF3 |

**Amdahl.** PF1 measures dashd's coins share of `reindex-chainstate`: the `Connect transactions` time, which includes fetches, plus the flush times from `-debug=bench`. L1, L2 and L7 can remove at most that share. The rest of the 2× must come from CH4, CS6 and the payee queue (A1 §3.2: no single-stage 2× claim).

### 5.7 PF0 workloads and the bench guard

| Workload | What it drives | Gated on |
|---|---|---|
| `crit:dashd-chainstate coins_micro` (+ `iai`) | `add`/`spend`/`get`/`merge`, `freeze`, `iter_dirty`, `PackedCoin::{encode, decode}`, the index rehash | instructions, allocations |
| `coins-replay-testnet-435990-{p300,p4096,dec43}`, `coins-replay-mainnet-453371-{p300,p4096,dec43}` | ST1's traces through the real `CoinsCache` + `FlushMachine` + `RocksEngine`, with Core's cache numbers. `InlineDriver` for counters, `ThreadDriver` for wall time | instructions, allocations, syscalls, I/O bytes, page faults, peak RSS (DEC-52: harness workloads are deterministic, so CPU-gated) |
| `coins-recovery-mainnet` | the recovery of a fixed crash image: a 2,000-block split transition stopped after B1 | the same, plus `startup` share |
| `coins-shutdown-flush-p300` | one ALWAYS flush of a full B | the same (shutdown proxy) |
| `chainstate-open` | open + WAL replay of a fixed DB | the same |
| `reindex-chainstate-{pre-DIP3, DIP3–V19, post-V20, tip−50k}` (A1 §3.11) | the node, once CH3a lands | counters from then on |

- **BG at CH2's merge:** the rows above against the previous head (PF0 rule: a regression above max(noise, 0.5 %) blocks), plus the DEC-43 proxy (§5.5). They replace the row's "`reindex-chainstate` counters", which cannot run before CH3a (Q1).
- **Milestone windows:** `reindex_time_s`, `peak_rss_mib`, `shutdown_s`, the DEC-43 cell, and V-stall histograms against dashd's `-debug=bench` flush lines.

### 5.8 The multi-CF bake-off workload (W-MC; A1 §2.8 "representative workload")

An extension of `dashd-store-bakeoff` (ST1's harness), run before CH2 merges and again at W-MF.

- **Coins:** ST1's traces (testnet-435990, mainnet-453371; the full mainnet trace when the oracle reaches the tip), replayed through CH2's real cache and flush, not the harness's own model.
- **Other CFs in the same `chainstate/` DB, in B1:**
  - `block_index` records (~150 B per block, rewritten on status changes);
  - `block_files` infos;
  - `mn_diff` per block after DIP3, `mn_snapshot` every 576 blocks, `llmq_mined` and `llmq_mined_h` per commitment, `credit_pool` and `mnhf`. Sizes are parametrised from the oracles' evodb key-prefix volumes, measured on a stopped copy with `dash-coreimport`, and recorded with the results;
  - rotation-snapshot effect rows as single Buffered batches.
- **A subscriber DB:** a txindex-shaped RocksDB (8 + 12 B per tx) driven by a `LocatorKeeper` with commit steps, checkpoints and the cadence, so its fsyncs compete as in production.
- **Reorg bursts:** every 2,000 blocks, disconnect k ∈ {1, 6, 30} blocks using the trace's undo, then reconnect.
- **Readers:**
  - concurrent point readers (gettxout-shaped) on V's thread;
  - a snapshot scanner that iterates the whole `coins` CF (`gettxoutsetinfo`-shaped) every 50,000 blocks while flushes continue.
- **Arms:**
  - RocksDB with §2.5's split;
  - **the Core model:** LevelDB chainstate (8 MiB), a separate block-index LevelDB (2 MiB) and a separate EvoDB LevelDB (64 MiB), with Core's synchronous `BatchWrite` (marker in the first batch, all unsynced, EvoDB after coins).
- **Cells:** p300 (allowance on), the DEC-43 cell, p4096.
- **Metrics:**
  - per-CF peak and final size, disk peak during compaction, write amplification;
  - RocksDB stall counters (`rocksdb.stall-micros`, `is-write-stopped`);
  - flush duration (W) and V-stall;
  - RssAnon and RssFile;
  - the scanner's run time and its effect on V.
- **Rule:** RocksDB ≤ the Core model on time (sign test over 5 pairs), on disk per component, and on RSS (anon + file). A miss triggers the tuning items (E split, memtable count, L5), not an engine switch (DEC-43 (a)). Results go to `designs/ST1-bakeoff.md` §11.

---
## 6. Verification

### 6.1 L0: Core's vectors

From `src/test/coins_tests.cpp`. Line numbers differ by +1 at v24, and the content is identical apart from that shift.

| Core test | Port |
|---|---|
| `ccoins_access`, `ccoins_spend`, `ccoins_add`, `ccoins_write` (`:648-1070 / :649-1071`): parent and child states ABSENT/SPENT/VALUE × DIRTY/FRESH → expected value, flags or `FAIL` (`logic_error`) | **(a)** verbatim, with "parent" = active and F empty: every row must match. **(b)** each row again with the parent's state placed in **F**: expected values come from §3.2. Values and `FAIL` match Core; flags may differ only where §3.2 D2 says so, with an equal database outcome |
| `coins_cache_simulation_test`, `updatecoins_simulation_test` (`:277, 307 / :278, 308`) | ported with freezes and releases at random points; the final state and every lookup equal a `MemoryCoins` reference |
| `ccoins_flush_behavior` (`:1072 / :1073`) | the erase-flush half (Core's `Sync()` has no production caller) |
| `coins_resource_is_used` | replaced by the arena's allocation tests (§6.7) |
| `ccoins_serialization`, compressor vectors | CS5a |

The two Core fuzz targets, `src/test/fuzz/coins_view.cpp` and `coinscache_sim.cpp`, become seeds for `coins_overlay_diff` (§6.5).

### 6.2 L1: differential against the oracle

- **New entry point `oracle_coins_ops`** (O2a family; owner Q7). Input: an op script over Core's stack, `CCoinsViewDB` (in-memory LevelDB, `fMemory = true`, `-dbbatchsize` as given) ← `CCoinsViewCache` tip ← a child view per "block". Ops:
  - `Begin`, `Add(op, coin, possible_overwrite)`, `Spend`, `Get`, `Have`, `HaveInCache`, `Access`;
  - `Commit` (child `Flush`), `Drop`, `Uncache`;
  - `FlushTip(hash)`, `Dump`.

  Output: per-op results, including the `logic_error` text, plus the database contents and `GetBestBlock`/`GetHeadBlocks` after each `FlushTip`.
- **Rust target `diff_coins_ops`** (node-l1) drives the same script through `CoinsOverlay` + `CoinsCache` with **hidden freezes and W steps at random points**, then ALWAYS at each `FlushTip`. Per-op results and dumps must be equal, per pin. The code is identical at both pins, and both still run.
- `diff_codec_primitives` (CS5a) covers coin bytes.
- O2b's `diff_chain` covers coins inside chain operations, including restarts (M3).

### 6.3 L2: replay windows

- **Trace replays** (`coins-replay-*` doubles as the L2 run): testnet-435990 and mainnet-453371, and the full mainnet trace when it exists.
  - The final `coins` CF must equal a `MemoryCoins` rebuilt from the trace.
  - At every Bk the database must equal the twin's snapshot at `new`.
  - In xcheck mode, every lookup is compared with the twin.
- **With CH3a:** the M3 exit's mainnet 0 → 1,028,159 replay compares UTXO MuHash per block (CS5b) against the oracle with `-consensusxcheck` on. `regtest-40000` runs CH2's `ChainSut` in capture mode with sampling (M3 exit 2, model level).

### 6.4 CRASH: ST3a's model over CH2's real code

Two SUTs implement ST3a's `Sut`:
- **`CoinsSut`** (coins profile): `CoinsCache` + `FlushMachine` + `Recovery` over one `FaultyEngine`, with no files and no evo. Used by `coins_cache_model`.
- **`ChainSut`** (chainstate profile):
  - the real cache, flush and recovery;
  - `BlockFiles<FaultyFlatFiles>` (`-fastprune`);
  - the chainstate `FaultyEngine` with `CHAINSTATE_SCHEMA`;
  - a stub `StateDelta` writing `mn_diff`, and one presumed-effect class writing `llmq_snapshot` rows that a later block's stub "evo fold" reads;
  - a test subscriber (`LocatorKeeper` over its own `FaultyEngine`) in Synced or Syncing mode;
  - synthetic blocks encoded with CS0, undo with CS5a, and a mini coins connect (no scripts) that emits `Connected { applied }` marks;
  - `IndexAccess` from CH1, or a test arena if CH1 has not merged.

  Its `RefExtractor` is ST3a §6.2's chainstate and subscriber extractor, with the same `finalizes` scopes.

**Scenarios.** Exhaustive within ST3a's computed bounds, both crash kinds, zero truncated images asserted.

| # | Scenario | Covers (PLAN acceptance) |
|---|---|---|
| C1 | first flush (`old` null), one-batch and split | **the first flush** |
| C2 | a split flush with 6 sub-batches: a crash after every event (each Bj `Append`, the pre-Bk barrier, Bk's `Append` and `Barrier`) | **every sub-batch boundary** |
| C3 | (a) V disconnects below `new` while F is in flight; (b) the next flush's marker has `fork ≠ old`; (c) a reorg crossing `old` | **a reorg in the window** |
| C4 | nested crash at every recovery event (depth 1, both kinds; depth 2 sampled): R7a, R7b | **interrupted recovery** |
| C5 | one-batch flushes, an index-only write, a presumed effect pending before B1 (R12 continuation reads it) | §4.1 scopes |
| C6 | V-ahead: `HAVE_DATA` blocks above `new` appended during the barrier; `d2` in Bk | I1 |
| C7 | ENOSPC and EIO at every store call of W and of recovery, `Applied` and `NotApplied` (R8) | I5 |
| C8 | subscriber: Synced with backlog 0 and 2 (the commit step), `D_max` small (a checkpoint, ST3a S11d's crash points (a)–(e)), the rewind commit, Syncing with a capped cadence | §4.4 |
| C9 | the `-dbcrashratio` points: ProcessKill after every partial sub-batch, in flushes and in recovery (Core's `feature_dbcrash` points) | US-NODE-04 |
| C10 | prune, S5 shape, with ST2's manual selector: subscriber-free, and with a Synced subscriber (N9's equality); Bp | §4.5 |
| C11 | an accepted block with a stub "invalid" connect persisted before connection (R6 (d)); CH3a re-runs it with real scripts | I3 (F01's index half) |

**Planted bugs.** These are `cfg(test)` knobs in the real code. Each must be reported, and each has a checked-in repro.

| ID | Bug | Caught by |
|---|---|---|
| X1 | marker written in Bk | R3 (T3) |
| X2 | evo delta and `evo_tip` in Bk | R0/R2 (T4) |
| X3 | Bk Buffered, no pre-Bk barrier | R3, D1(f) (T18) |
| X4 | Bk Durable, no pre-Bk barrier | R3, D1(f) (T26) |
| X5 | recovery's final batch without a barrier | R7a, D1(f) (T27) |
| X6 | no `sync_wal` at open | R7b |
| X7 | B1 before the files barrier | R1, D1(a) (T1) |
| X8 | replay reads keys before writing them | the per-image bound check (`Work`) fails, so the exhaustive search would be lost |
| X9 | `add` ignores F's unspent coin | L0 (b) rows, `diff_coins_ops` |
| X10 | a prefetch result admitted with a stale `fetch_gen` | `coins_cache_model` xcheck (a resurrected coin) |
| X11 | a locator committed without submitting staged data | R11c(i) (T41) |
| X12 | a Buffered checkpoint, resumed before a barrier | R11c(i) (T38) |
| X13 | V releases F before Bk returns (W still writing) | `coins_cache_model` lookup mismatch; R3 |
| X14 | W treats effects written after the freeze as covered by the pre-B1 barrier | D1(f), R12 (T29 variant) |

**PR-gate budget.** About 25 CPU-s in debug with DEC-63's overrides. This scales ST3a's calibration: ChainSut's events per flush are about 1.5× the toy's, and the scenario count is about 0.6×. Phase B of the implementation re-measures it. The nightly runs the second release, `Extended`, depth 2 and 2·10⁵ sweep cases within ST3a's approved 1.5 CPU-h, sharing that budget.

**ST3b hooks.**
- `-dbcrashratio`.
- A `flush` debug category that logs every barrier with the flush id, so `dm-log-writes` marks can be aligned to protocol steps.
- `-dbbatchsize` and a tiny `-dbcache` force split flushes on regtest.

### 6.5 Fuzz targets (NFR-CEQ-04: the 5-minute smoke plus ≥ 24 CPU-h on new targets at merge)

| Target | Drives | Oracle |
|---|---|---|
| `coins_cache_model` | ops: add/spend/get/have/uncache/overlay begin, commit, drop; freeze; W steps; release; prefetch admits with stale and fresh gens; disconnect via undo; crash at any event (coins profile) | the `RefCoinsCache` twin (a line-by-line port of `CCoinsViewCache` over `MemoryCoins`) for every lookup; R0, R2–R4, R7, R8, R13 |
| `coins_overlay_diff` | random parent/child flag states and op sequences (seeded from Core's two fuzz targets) | the twin; `logic_error` parity |
| `chainstate_crash` | ST3a `sweep` over `ChainSut` (§6.4 families, random workloads) | `check_chainstate` |
| `coins_replay_blind` | random transitions (connects, reorgs) and random subsets of pending batches, then recovery | a reference recomputed from the block list |

### 6.6 Functional tests and their owners

| FT | Owner | CH2's part |
|---|---|---|
| `feature_dbcrash` (EXTENDED at both pins; asserts the utxo hash after crash restarts and `crashed_on_restart > 0`) | CH5 | `-dbcrashratio` in flushes **and** in recovery; recovery itself |
| `feature_reindex` | CH5 | `wipe_chainstate` |
| `rpc_blockchain` `gettxoutsetinfo.disk_size` bounds | RP1a | `estimate_size` (a CH2 unit test pre-checks the bound) |
| `feature_index_prune`, `feature_pruning`, `feature_fastprune` | CH8 | §4.5 slots |
| `feature_coinstatsindex`, `feature_utxo_set_hash` | IX1 / CS5b | the cursor's key order |
| mempool tests that depend on `Uncache` | MP1 | `uncache`, `have_in_cache` |

### 6.7 The budget invariant under a counting allocator

- **Feature `alloc-tag`** (bench and test builds only). A global allocator wrapper records `(tag, size)` in a 16-byte header.
  - The cache sets the tag `coins` with scoped guards around its own allocation sites: chunks, index growth, long-coin chunks, the prefetch map, the batch buffer.
  - Frees subtract by header on whatever thread frees (W drops F).
- **Assertions:**
  - after every op in `coins_cache_model`, and at sampled points in `coins-replay`: `tagged(coins) == usage().total` exactly, and `≤ B(t)`;
  - no `coins`-tagged allocation outside the declared sites;
  - layout: `size_of::<Slot>() == 72`, `align_of::<Slot>() == 8`, `SLOTS_PER_CHUNK × 72 ≤ 262,144`;
  - after warm-up, the index load factor is in [7/16, 7/8].
- `cargo tree -e features,no-dev` on the runtime binaries shows neither `alloc-tag` nor `faulty` (ST3a §3.1's test).

---

## 7. Implementation plan

Each phase is one reviewable PR stacked on the previous one. Class STATE: two reviewers plus `code-review` at high effort.

| Phase | Content | Acceptance |
|---|---|---|
| **P1 Coins core** (5–6 d) | `OutPointKey`, `PackedCoin`, `Slot`, `CoinLayer`, `CoinsCache` with active only (F always empty), `CoinsOverlay`/`OverlayView`, `merge`, the miss path through `CoinsTable`, `RefCoinsCache` twin, `alloc-tag` | L0 tables (a) all rows; the simulation ports; `coins_overlay_diff` smoke; §6.7 assertions; `coins_micro` baseline |
| **P2 Store and budget** (3 d) | `CHAINSTATE_SCHEMA` v1, `Meta` codecs (layout tests), `CacheSizes` per profile with Core's log lines; `RocksConfig` write-buffer manager charged to the cache + the `manual_wal_flush` assert (`dashd-store`); `GuardedEngine`, `GlobalFailStop`; `wipe_chainstate` | `CacheSizes` equals Core's numbers for a grid of `-dbcache`, `-txindex`, filter and v24 index flags at both profiles; `dashd-store` tests; the `disk_size` unit test |
| **P3 Freeze and flush** (6–7 d) | `FreezePacket`, `FlushMachine` (steps 1–5, 8–12; one-batch and split), `flush_policy` (§3.3), release and the prefetch fence, `ThreadDriver`/`InlineDriver`, `FlushObserver`, errors and `-dbcrashratio`, the presumed-effect ledger | `CoinsSut` and `ChainSut` C1, C2, C5, C6, C7, C9 exhaustive; X1–X4, X7, X13, X14; L0 (b) rows; `coins-replay-*` green against `MemoryCoins` |
| **P4 Recovery** (4 d) | `Recovery` (open-time `sync_wal`, checks, blind undo and forward legs, final barrier), `replayed()`, legacy-undo read, `open_chainstate` | C1–C4 with recovery; X5, X6, X8; `coins_replay_blind` smoke; `coins-recovery-mainnet` baseline |
| **P5 Subscribers and prune slots** (5–6 d) | `SubscriberHub`, `LocatorKeeper` (two tips, gate, checkpoint, rewind, cadence, recovery), control events; step 6; prune step 0/7, Bp, clears, unlink via ST2 | C8, C10; X11, X12; FA1–FA6 asserted on `ChainSut` forward runs; deadlock test (§4.4) with a blocking non-mandatory subscriber that queries V |
| **P6 Perf and W-MC** (4–5 d) | PF0 workloads (§5.7), the W-MC extension and report, f tuning, the DEC-43 proxy | §5.5 proxy rule; W-MC rule; PF0 baselines recorded |
| **P7 L1** (2–3 d, after the O2a follow-up) | `diff_coins_ops` against `oracle_coins_ops` | ≥ 24 CPU-h clean (merge budget); X9 found |

**Total ≈ 29–35 agent-days.** That is above the row's L (2–4 agent-weeks), so I recommend the split in Q5.

---

## 8. Open questions for the manager

1. **Q1: the DEC-43 gate and the BG row cannot run at CH2's merge.** `connect_block` is CH3a's, so rdashd cannot run `reindex-chainstate` or node-bench yet. **Recommendation:** gate CH2's merge on the §5.5 proxy (`coins-replay-*-dec43` against ST1's LevelDB Core-model arm, sign test) and the §5.7 counters. Move the real DEC-43 cell and `reindex_time_s`/`peak_rss_mib` to the M3 exit and to CH3a/CH4's BG.
2. **Q2: the engine memory split (A1 erratum A-2).** E = Core's block-index and chainstate shares + Core's fixed 64 MiB EvoDB cache, as one LRU with memtables charged to it. The coins share stays Core's exactly. A1 §3.4 took a block cache and 64 MiB of memtables out of `-dbcache`, which left B ~20 % smaller than dashd's. **Recommendation:** approve; it is DEC-43 (b) with the evo cache Core spends anyway.
3. **Q3: the arena plus index instead of A1's `HashMap<OutPointKey, Entry>` (erratum A-3).** About 80 B per coin against 83–167 B, smooth growth, salt-free iteration order. **Recommendation:** approve. The density claim waits for PF1, per A1.
4. **Q4: read errors behind CS5a's infallible `CoinsView`.** **Recommendation:** keep the trait infallible. The adapter latches globally and returns `None`, and CH3a checks `failed()` before any verdict (a test plants a read error mid-block and asserts no `FAILED_VALID`). The alternative, `fn coin(&self) -> Result<Option<Coin>, ViewError>` in CS5a, is cleaner but touches every CS5b caller.
5. **Q5: split CH2.** The phases add up to about 6–7 agent-weeks. **Recommendation:**
   - **CH2a:** P1–P2, the coins cache and store (S/M);
   - **CH2b:** P3–P5, flush, recovery and subscribers (L, the STATE core);
   - **CH2c:** P6–P7, perf, W-MC and L1 (M).

   CH3a needs only CH2a's API plus CH2b's freeze to start (i:CH2b).
6. **Q6: the CH1 dependency.** CH2 needs `IndexAccess`, dirty-record hand-off and the record codec. **Recommendation:** add `i:CH1` to CH2's row. CH1 lands at M2, before CH2's implementation window, and the trait keeps CH2 testable with a test arena.
7. **Q7: `oracle_coins_ops`.** It is a new O2a-family entry point (in-memory `CCoinsViewDB` + tip + child views). **Recommendation:** an O2a follow-up task owned by the oracle-shim implementer, small (S). CH2c's P7 depends on it, and nothing else in CH2 does.
8. **Q8: `f` and lever L3.** **Recommendation:** ship f = 0.5 (A1); PF3 measures write volume against the Core model and decides between tuning f and building the segmented F (L3) before M5.
9. **Q9: the `LeftIbd` freeze trigger** (A1 §3.4, not Core). **Recommendation:** keep it. It costs one flush, it is unobservable, and it makes the node durable when it starts serving.
10. **Q10: v24's startup cache-rebalance log lines** ("resized coinsdb/coinstip cache"). **Recommendation:** print them under `V24_0` only, as log parity, without the 20 % → full resize dance. No O17 row unless the manager wants one (`ChainstateRules.rebalance_log`).
11. **Q11: the commit-step interleave.** Coin sub-batches may be written while W waits for synced subscribers (§4.2 step 6). The invariants allow it (sub-batches are in no locator's scope; Bk still follows the selection), and it shortens flushes when an index is slow. **Recommendation:** allow it. The seam explores both orders, so CH2's CRASH set covers whichever ships.
12. **Q12: ATMP's coin access.** **Recommendation:** MP1's coin fetches and `uncache` run as V commands (Core holds `cs_main` across ATMP), so the cache stays single-owner. Off-V readers use DB snapshots only after a forced flush, as `gettxoutsetinfo` does.
13. **Q13: legacy undo in recovery** (§3.5). **Recommendation:** accept the one read-based exception to blind replay. Its value is transaction-invariant, and the alternative is a forced reindex where Core recovers.

---

## Appendix A. Proposed A1 errata (with the BL-26 batch)

| # | A1 location | Proposed |
|---|---|---|
| A-1 | §3.4 "One memory budget" | `B(t)` is Core's `nTotalSpace` (the coins share plus the unused mempool allowance); `R = S + 64 KiB` while flushing, with S the effective sub-batch size |
| A-2 | §3.4 `-dbcache` split table; §2.8 RocksDB configuration | the coins share is Core's `CalculateCacheSizes` remainder (per profile); the engine budget E = block-tree share + coins-DB share + 64 MiB (Core's fixed EvoDB cache), one LRU with memtables charged through a write-buffer manager; buffered reads, no mmap and no direct I/O |
| A-3 | §3.4 `CoinsCache` sketch | an arena of 72-byte slots plus a `HashTable<u32>` index; iteration in arena order |
| A-4 | §3.4 read path | a hit in F is not copied into active; DB and prefetch hits are copied as clean; tombstones are authoritative |
| A-5 | §3.5.4 | ST3a's order with E12–E22; steps 6 and 8 may interleave (Q11); `-dbcrashratio` also applies in recovery |
| A-6 | §3.5.5 step 4 | the legacy-undo read exception (Q13) |
| A-7 | §3.5.7 subscribers | the real-system mapping of §4.4: control events outside the queue bound, and V's waits as service loops |
