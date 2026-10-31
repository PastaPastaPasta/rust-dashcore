# CH2 (Fable): coins cache, coins table, and the durability and recovery protocol

| | |
|---|---|
| Status | design rev0, independent D2 design (the Opus design is reconciled with this one by the manager) |
| Date | 2026-10-08 |
| Task | PLAN row CH2 (M3): coins cache (`PackedCoin`, freeze-and-flush under one memory budget), the coins table, the A1 §3.5 durability and recovery protocol (file → B1 → coin sub-batches → Bk; coins-only `ReplayBlocks`; errors and disk full; subscribers' locators), the multi-CF bake-off workload |
| Crates | `dashd-chainstate::{coins, flush, recovery, subscriber}`; `dashd-store` (small additions) |
| Binding inputs | A1 §2.8–§2.11, §3.4–§3.5 and its errata; ST3a rev5 (DEC-74), **§5.6 normative**, §2.4.4, §5.5, §6.2, §12 errata E1–E22; DEC-43, DEC-59, DEC-62, DEC-63, DEC-64, DEC-66, DEC-69; ST1's merged `dashd-store` API; ST2's block-file API (`node/st2-block-files`); CS0's `CoreBytes`; the CS5a PLAN row |
| Core pins | v23.1.8 and v24.0.0-rc.3 (`~/workspace/dash`); cited as `file:line(v23) / :line(v24)` |

## 0. The design on one page

1. **One cache, three immutable-or-single-writer layers.** V (the validation thread) mutates one hashbrown `active` map of `OutPointKey → Entry`. A flush moves `active` to an immutable `frozen` layer behind an `Arc` in O(1) and starts an empty `active`. W (the db-writer thread) streams `frozen` into the `coins` column family through Core's `BatchWrite` rules. Reads go overlay → `active` → `frozen` → `prefetch` → DB. The Core `DIRTY`/`FRESH`/spent state machine is ported entry for entry (`coins.cpp:44-150`, same lines at both pins), and the five legal states are an invariant checked by a `SanityCheck` twin.
2. **`PackedCoin` is Core's `Coin` bytes.** A 40-byte entry: 36-byte key, 1 flag byte, 1 length byte and a 2-byte-aligned tail that holds up to 30 bytes of `VARINT(h·2+cb) ‖ CompressAmount ‖ ScriptCompression` inline, or a `Box<[u8]>` for the rest. Every P2PKH and P2SH coin is inline. The same bytes go to the DB with no re-serialization, and the `coins` CF is byte-diffable with a deobfuscated dashd chainstate. The codec is CS5a's; CH2 only adds the inline/heap container and borrowed decoding (§2.2, §8 Q2).
3. **One budget, measured.** `size(active) + size(frozen) + size(prefetch) + batch_in_flight + memtables_in_flight ≤ T`, where `T = B + max(maxmempool − mempool_usage, 0)` is Core's `nTotalSpace` (`validation.cpp:2744-2745 / :2952-2953`), not v1's bare coins share. Sizes are counted by a counting allocator in the bench build and by exact arithmetic in release. The freeze threshold is `f·T` with `f = 0.5`; V waits only when the new `active` reaches `T − size(frozen) − size(prefetch) − batch − memtables` (Core's CRITICAL). W calls `flush_memtables()` after Bk so the memtables return to empty between flushes, which keeps the coins share within ~6% of dashd's at `-dbcache=300` (§5.2).
4. **The flush is ST3a §5.6's protocol, implemented once, generically and synchronously.** `FlushDriver<E: KvEngine, F: FlatFs>` runs steps 0–12 of ST3a §6.2 as an explicit state machine whose `step()` is drivable from one thread (the ST3a seam) and from W in production. Order (DEC-64, E12): prune decision (dry run under the predicted lock) → files barrier → `sync_wal` if a presumed effect is pending → **B1 Durable** (dirty index, file infos, every evo delta, `evo_tip`, the marker, the erase of `tip`) → `ChainStateFlushed` → synced mandatory subscribers drain to `new` and commit locators behind their own `sync_wal` → prune selection `D ∩ eligible(actual)` → coin sub-batches Buffered → `sync_wal` → **Bk Durable** (last coins, second dirty-info take, prune clears, `pruned_files`, `tip = new`, marker erase) → release F → unlink. A one-batch flush is B1 alone (or B1 + Bp when it prunes).
5. **Recovery is coins-only and blind.** `meta.head_blocks = [new, old]` names the single transition. Recovery runs `sync_wal` at open, loads the locks and the index, applies the undo records from `old` down to the fork and the blocks up to `new` as erases and puts that never read the keys they touch (E22), writes Buffered sub-batches, then `sync_wal`, then one Durable batch with `tip = new` and the marker erased. Evo is already at `new` because B1 carried it. Nothing above `new` is replayed: `activate_best_chain` connects it through `connect_block` (I3).
6. **Subscribers share one driver.** `SubscriberDriver<E>` implements ST3a §5.6 item 6 for every mandatory subscriber (`llmq/`, each index) and the wallets: `Synced`/`Syncing` classes, the path-based drain, `D_max` backpressure with Durable checkpoints, capped cadence commits at `LCA(progress, T_ack)`, Durable blind rewind commits, submit-before-sync for locators, and blind rewind then catch-up at open. CH2 ships the driver and its model tests; the index, lock and wallet tasks plug their data writers in.
7. **Errors are fail-stop with Core's strings.** `"Disk space is too low!"`, `"Flushing block file to disk failed. This is likely the result of an I/O error."`, `"Failed to write to block index database"`, `"Failed to write to coin database"`, `"System error while flushing: …"`, and at startup `"Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate."` After the first store error nothing more is committed; the engine's own latch (ST1) and `BlockFiles::has_failed` back that up. `-dbcrashratio` calls `_exit` at sub-batch boundaries exactly where Core does (`txdb.cpp:147-154 / :156-163`).
8. **Core-equivalence, with four marked deviations (†)**, each visible only after a crash or a disk failure: every blk/rev file written since the last barrier is synced, not only the current pair; evo rides in B1 so coins and evo can never disagree; pruning's index clears ride in Bk and unlinking waits for Bk; a failed file sync aborts instead of continuing. Everything an RPC, a peer or a log reader can see on a healthy node is Core's: the flush triggers and modes, `gettxoutsetinfo`'s forced flush, the "Writing partial batch" and "Committed %u changed transaction outputs" lines, the replay log lines.
9. **Performance.** The affected metrics are #1 IBD, #3 reindex and `reindex-chainstate`, #10 memory (`peak_rss_mib`), #12 CPU per block, and flush-stall p99 (ST1 OI-7). Levers that dashd lacks: asynchronous flush (V never waits for a write unless the budget is exhausted), a cache that keeps serving across a flush (Core's `Flush()` empties `cacheCoins`, `coins.cpp:254-265`), denser entries, epoch-fenced prefetch (CH4), and a memtable that is flushed after every Bk. The DEC-43 cell (`-dbcache=300`, no mempool allowance) is the merge gate against dashd; the memory split of §5.2 is designed for it and measured there. PF0 workloads are named in §5.5.
10. **Verification.** L0: Core's `coins_tests.cpp` state tables and the cache-simulation test ported as a twin test; `feature_dbcrash` ported. L1: `diff_codec_primitives` is CS5a's; no new oracle entry point is needed for the cache or the protocol. L2: the coins digest at every flush boundary and after every recovery equals the oracle's MuHash. CRASH: the real `FlushDriver`, `Recovery` and `SubscriberDriver` are the `Sut` of ST3a's scenarios S1–S14b, so the 41 planted bugs are rejected by the production code, not by a toy. Fuzz: `coins_cache_model` (interleavings of spend, create, freeze, sub-batch completion, prefetch completion, reorg, crash) and `flush_protocol` (seeded sweeps over ST3a's `World`).

---

## 1. Scope and non-goals

### 1.1 In scope (PLAN row CH2)

| Deliverable | PRD | Where |
|---|---|---|
| `PackedCoin`, `OutPointKey`, `Entry`, the coins map and its memory accounting | FR-CHS-03 | §2.2, §2.4 |
| `CoinsCache`: Core's `CCoinsViewCache` semantics over three layers; `BlockOverlay` child map; freeze-and-flush; the prefetch map and its generation fence (the fence's producer is CH4) | FR-CHS-03; NFR-PERF-01/03 | §2.3, §2.5 |
| The coins table (CF `coins`) and `meta` keys `tip`, `head_blocks`, `evo_tip`, `pruned_files`, `last_file` | FR-STO-01/02 | §2.6 |
| `FlushDriver`: triggers and modes (Core's `FlushStateToDisk`), the ST3a §5.6 order, B1/sub-batches/Bk, the periodic index-only write, the prune decision and selection hook points (DEC-64), unlinking | FR-CHS-04, FR-STO-03 | §2.7, §4 |
| `Recovery`: coins-only `ReplayBlocks`, blind, idempotent, `sync_wal` at open, the corruption checks and Core's refusal strings | FR-CHS-04, US-NODE-04 | §2.8, §4.4 |
| `SubscriberDriver`: the locator contract for every mandatory subscriber and the wallets | FR-CHS-04, FR-STO-03 | §2.9, §4.5 |
| Errors and disk full: fail-stop, Core's strings, `-dbcrashratio` | US-NODE-04 | §3.6, §4.6 |
| The ST3a seam: generic over `E: KvEngine` and `F: FlatFs`, synchronous `step()`, marks, fixed salt, `replayed()` | FR-STO-03 | §2.10, §6.4 |
| The multi-CF bake-off workload W-X (A1 §2.8 "representative workload") | NFR-PERF-01, NFR-RES-02 | §5.6 |
| The DEC-43 memory split: memtables, block cache, page cache, direct I/O option | NFR-RES-01 | §5.2 |

### 1.2 Non-goals (owned elsewhere)

- `connect_block`/`disconnect_block`, undo construction, `UpdateCoins`'s callers, assumevalid: **CH3a**. CH2 provides `CoinsCache::{add_coin, spend_coin, fetch}` and `BlockOverlay`; CH3a calls them in Core's order.
- `activate_best_chain`, the reorg loop, `FlushStateToDisk(IF_NEEDED)` call sites in `ConnectTip`/`DisconnectTip`: **CH3b**. CH2 provides `FlushPolicy::decide` and `ChainCommand::Flush`.
- The S2 prefetch producer (rayon `MultiGet` ahead of V) and `-consensusxcheck`'s per-lookup comparison: **CH4**. CH2 provides the `PrefetchMap`, `released_gen` and the admission rule.
- Evo deltas, their persistence format and the classification of effects as presumed or independent: **CH7a**. CH2 takes an opaque `NonCoinDelta` (a prepared sub-batch) plus a `presumed_effect_pending: bool` and a `size_bytes` for the EvoDB-critical trigger.
- `-prune` configuration, the `pruneblockchain` RPC, prune locks from indexes, the six `feature_index_prune` assertions: **CH8**. CH2 implements the two hook points of DEC-64 (the dry run at step 0 and `D ∩ eligible(actual)` at step 7) over ST2's selectors, because the CRASH model needs them inside the protocol; CH8 wires configuration, the RPC and the FT.
- Index, `llmq/` and wallet data writers: **IX1/IX2, LK1b, WL**. They implement `SubscriberData` (§2.9) and reuse `SubscriberDriver`.
- `-reindex`, `-reindex-chainstate`, `-loadblock`, `VerifyDB`: **CH5**. CH2's recovery runs before `VerifyDB` and after the index load, as Core's `ReplayBlocks` does (`init.cpp:2066` / `node/chainstate.cpp:250-256`).
- The coin and undo codecs, `CoinsView`, MuHash and `hash_serialized_2`: **CS5a/CS5b**. §8 Q2 lists the API CH2 needs from CS5a.
- The device- and VM-level harness: **ST3b**. CH2's `Sut` gives it the production code.
- assumeutxo (NG-6): the `Sync()` path (`coins.cpp:267-282`) is not exposed; §3.2 notes that our flush already has `Sync` semantics.

---

## 2. Data model and public API

### 2.1 Placement

```text
dashd-chainstate/src/
  coins/mod.rs        CoinsCache, BlockOverlay, CoinsReadView, PrefetchMap, CoinsBudget, CoinsSizeState
  coins/entry.rs      OutPointKey, PackedCoin, Entry, EntryFlags (layout tests, counting-allocator tests)
  coins/layer.rs      ActiveLayer (hashbrown, SaltedFxBuild), FrozenLayer (Arc, immutable), iteration in freeze order
  coins/table.rs      CoinsTable (CF `coins`), MetaTable (CF `meta` keys), batch encoders
  flush/mod.rs        FlushPolicy (triggers, modes), FlushPlan, FlushDriver<E, F> (the state machine), FlushMarks
  flush/prune.rs      PruneDecision (dry run), PruneSelection (D ∩ eligible), the re-arm rule; PruneHooks trait
  recovery/mod.rs     Recovery<E, F>: open-time barrier, marker checks, coins-only replay, finalization
  subscriber/mod.rs   Subscriber, SubscriberData, SubscriberDriver<E>, Locator, LocatorKind, DMax
  seam.rs             Marks, Clock, Salt: the ST3a seam (feature `faulty` in tests only)
dashd-store/src/
  codec.rs            + VarintU32 key codec for `txid ‖ VARINT(vout)`; + `KvBatch::delete_range` (BL-38, DEC-69 Q16; used by SubscriberDriver's blind rewind)
  rocks.rs            + `RocksConfig::coins_profile(dbcache)` (the §5.2 split), + `flush_memtables` is already there
```

Rules carried from A1 Appendix E and ST3a §5.6:
- No `async`, no store call on a worker thread, no `HashMap` with `RandomState` in the determinism-linted paths (`SaltedFxBuild` takes its salt from `Salt`, which is random in production and fixed under the seam).
- `CoreBytes` for every stored block; `CoreUndoBytes` for every stored undo (ST2's types). The `coins` CF stores CS5a's `CoinBytes`.
- Everything is monomorphised over `E: KvEngine` and `F: FlatFs`; production instantiates `RocksEngine` and `OsFs`, the model `FaultyEngine` and `FaultyFlatFiles`.

### 2.2 Keys and values

```rust
/// Core's coins key without the 'C' prefix (A1 §2.9): txid (32, as stored, i.e. Core's internal byte order) ‖ VARINT(vout).
/// In memory the key is fixed-size; the VARINT is produced only when a DB key is encoded.
#[repr(C, align(4))]
pub struct OutPointKey { pub txid: [u8; 32], pub vout: u32 }          // 36 bytes

/// Core's `Coin` serialization: VARINT(height·2 + coinbase) ‖ VARINT(CompressAmount(value)) ‖ ScriptCompression(script).
/// Inline when ≤ 30 bytes (every P2PKH/P2SH coin: ≤ 4 + ≤ 6 + 21 = 31 at most; the height VARINT is ≤ 3 bytes below
/// height 2^20 and the amount VARINT ≤ 5 bytes below 10^13 duffs for amounts with ≥ 1 trailing zero, so the typical
/// coin is 25–29 bytes). Heap otherwise (P2PK 34 bytes, bare multisig, non-standard, raw scripts).
pub enum PackedCoin { Inline { len: u8, bytes: [u8; 30] }, Heap(Box<[u8]>) }   // 32 bytes, tag folded into `len` (len = 0xff ⇒ Heap)

bitflags! { pub struct EntryFlags: u8 { const DIRTY = 1; const FRESH = 2; const SPENT = 4; } }
pub struct Entry { pub coin: PackedCoin, pub flags: EntryFlags }                 // 33 → padded to 40 with the key? no: see below
```

**Layout decision.** `Entry` is stored as `(OutPointKey, PackedCoin, EntryFlags)` in one hashbrown slot of **72 bytes** (36 + 32 + 1, padded to 8): hashbrown's control byte adds 1 and its load factor 7/8 adds ~14%, so the steady cost is about **84 bytes per inline coin**, with no per-entry heap allocation. Core's node is `std::pair<const COutPoint, CCoinsCacheEntry>` with a `prevector<28>` script, a 4-byte flags and height word, a pool-allocated 8-byte next pointer and the bucket array: about 36 + 8 + 4 + 28 + 1 + 8 + 8 ≈ 93 bytes plus the pool's slack, so the PF1 expectation is 10–20% denser, **not claimed until PF1 measures both**. Two refinements are kept as PF3 levers, not design: (a) a `SPENT` entry needs no coin, so a tombstone-only map (`HashMap<OutPointKey, ()>`) for spent-not-FRESH entries would halve their cost; (b) an open-addressing table keyed by the first 8 bytes of the txid (the txid is uniformly random) could drop the control bytes. Neither changes any API.

`PackedCoin` **decodes on access** through CS5a's borrowed decoder (`CoinRef<'a> { height, coinbase, value, script: ScriptRef<'a> }`; §8 Q2). Script checks, `check_tx_inputs` and `UpdateCoins` read `CoinRef`; no `Coin` is materialised during connect. A coin is encoded exactly once, when `add_coin` is called with a `TxOut` reference and a height, through CS5a's `coin_bytes` encoder, and those bytes are what the DB stores (`coins.h:84` `DynamicMemoryUsage` → our `len`).

**Hashing.** `SaltedFxBuild` hashes the first 8 bytes of the txid XOR a 64-bit salt, then mixes in `vout` (the txid is the output of SHA256d, so this is Core's `SaltedOutpointHasher` in spirit with no SipHash). The salt is per-process random in production (`Salt::random()`), and the seam fixes it (`Salt::fixed(0)`, ST3a §5.6 item 3), which also makes iteration order deterministic for `replayed()` and digests.

### 2.3 `CoinsCache` and its layers

```rust
pub struct CoinsCache<E: KvEngine> {
    active:   ActiveLayer,                     // hashbrown::HashMap<OutPointKey, Entry, SaltedFxBuild>; V only
    frozen:   Option<Arc<FrozenLayer>>,        // immutable; W is writing it; generation `g`
    prefetch: PrefetchMap,                     // positive results only; fetch_gen-fenced (A1 §3.4)
    db:       CoinsTable<E>,                   // CF `coins`, reads through a snapshot or the engine
    best:     Option<BlockHash>,               // Core's hashBlock: the block the cache (active ∪ frozen ∪ DB) describes
    released_gen: u64,
    budget:   CoinsBudget,
}

pub struct BlockOverlay<'a> { parent: &'a CoinsCache<E>, child: ActiveLayer /* FRESH/DIRTY vs the parent */, best: BlockHash }

pub trait CoinsReadView { fn coin(&self, op: &OutPointKey) -> Result<Option<CoinRef<'_>>>; fn have(&self, op: &OutPointKey) -> Result<bool>; }
```

**Read path** (`fetch`): overlay child → `active` → `frozen` → `prefetch` → `db`. A `SPENT` entry anywhere above the DB is authoritative (Core's `FetchCoin` finds the spent entry in `cacheCoins` and returns it; `GetCoin` then reports `!IsSpent()` as false, `coins.cpp:61-68`). A DB hit is inserted into `active` as `flags = ∅` if unspent, exactly as `FetchCoin` emplaces the parent's coin; Core marks a *spent* parent entry `FRESH` (`coins.cpp:52-56`), which cannot happen for the DB layer (an erased key is absent) but does happen when the parent is `frozen`: a `SPENT` entry in `frozen` fetched into `active` becomes `SPENT | FRESH`, so that a later `add_coin` + `spend_coin` pair in `active` never reaches the DB (`coins.cpp:142-143`).

**Write path.** `add_coin(op, out, height, coinbase, possible_overwrite)` and `spend_coin(op) -> Option<PackedCoin>` are line-for-line ports of `coins.cpp:70-106` and `:129-150`, including the two `logic_error` conditions, which become `debug_assert!` plus an `Err(CoinsError::FreshMisapplied)`/`Overwrite` that CH3a turns into Core's `AbortNode` path (Core throws; a thrown `std::logic_error` escapes `ConnectBlock` and terminates the node, so an abort is the equivalent). `AddCoins` (`coins.cpp:118-127`) is CH3a's loop; CH2 exposes the per-output call with `possible_overwrite = coinbase` on the connect path and `= have_coin(op)` on the replay path (`AddCoins(…, check = true)`, used by `RollforwardBlock` `:4860 / :5213`). `IsUnspendable` outputs are skipped inside `add_coin` as Core does (`coins.cpp:72`; quirk 33's "unspendable outputs never in the UTXO set").

**Overlay merge** (`BlockOverlay::commit`) is Core's `CCoinsViewCache::BatchWrite` with `erase = true` (`coins.cpp:182-252`): the parent is `active`, the child is the block's map. The `FRESH`-misapplied check (`:217-223`) is a `debug_assert!` and an xcheck-mode error. A failed connect drops the overlay (`CCoinsViewCache view(&CoinsTip())` going out of scope).

**Freeze** (`freeze() -> Arc<FrozenLayer>`): V swaps `active` into a new `FrozenLayer` (an O(1) move of the hashbrown table) and allocates a fresh `active` with capacity derived from the previous one. `frozen` is `None` only when no flush is in flight. If a freeze is requested while `frozen` is `Some`, V **waits** on W (`CoinsCache::wait_released`): the only point where validation blocks on I/O, and the one Core blocks at every flush.

**Release** (`release(g)`): W, after Bk returned, hands the generation back; V drops `frozen`, increments `released_gen` and clears `prefetch` (A1 §3.4 fence rule 2). Reads between Bk and release still hit `frozen`, so no reader ever sees a half-written transition (A1 §3.5.7).

**`Sync` semantics for free.** Core's `Flush()` empties `cacheCoins` and reallocates the map (`coins.cpp:254-265`), so after every flush dashd's hit rate starts from zero; its `Sync()` (`:267-282`, used only by the assumeutxo path, out of scope under NG-6) keeps the entries. Our freeze keeps `active`'s new entries and keeps serving `frozen` until Bk; the PF3 lever "retain F as a clean, read-only warm layer after release until `active` needs the room" is §5.4's L4, not part of rev0.

**Disconnect.** `disconnect_block` (CH3a) applies undo records through the same `add_coin(possible_overwrite = !clean)`/`spend_coin` API in a `BlockOverlay`, as `ApplyTxInUndo` does (`validation.cpp:1966-1991 / :2252-2277`), and clears `prefetch` (fence rule 4). The `undo.nHeight == 0` metadata case returns `DISCONNECT_FAILED` through CS5a's undo decoder reporting a zero height; CH2 exposes `access_by_txid` (`AccessByTxid`, `coins.cpp` helper) for the alternate-output lookup.

### 2.4 `CoinsBudget` and `CoinsSizeState`

```rust
pub struct CoinsBudget {
    pub coins_share: usize,       // B: the -dbcache share for coins (§5.2 split)
    pub max_mempool: usize,       // -maxmempool, 0 when no mempool
    pub freeze_fraction: f32,     // f = 0.5 default; -dbfreezefraction (debug-only)
    pub batch_size: usize,        // -dbbatchsize, 16 MiB
    counts: BudgetCounters,       // active, frozen, prefetch, batch_in_flight, memtables_in_flight (bytes, exact)
}
pub enum CoinsSizeState { Ok, Large, Critical }     // Core's CoinsCacheSizeState

impl CoinsBudget {
    pub fn total_space(&self, mempool_usage: usize) -> usize { self.coins_share + self.max_mempool.saturating_sub(mempool_usage) }
    pub fn state(&self, mempool_usage: usize) -> CoinsSizeState;   // Core's thresholds: > total ⇒ Critical; > max(0.9·total, total − 10 MiB) ⇒ Large
    pub fn should_freeze(&self) -> bool;    // size(active) ≥ f·total − size(frozen) − size(prefetch) − batch − memtables, and frozen.is_none()
    pub fn must_wait(&self) -> bool;        // size(active)+size(frozen)+size(prefetch)+batch+memtables ≥ total, frozen.is_some()
}
```

**The invariant** (A1 §3.4, with two terms added):

`size(active) + size(frozen) + size(prefetch) + batch_in_flight + memtables_in_flight ≤ T`, `T = B + max(maxmempool − mempool_usage, 0)`.

- `batch_in_flight` is the in-memory size of the sub-batch W is building or committing: `KvEngine::batch_with_capacity(batch_size + max_record)` so that `KvBatch::size_bytes` is its real memory (ST1 §9.5: a growing `WriteBatch` holds up to twice its size; preallocation removes that).
- `memtables_in_flight` is the engine's memtable occupancy for the `coins` CF during a flush, reported by `RocksEngine::memtable_bytes()` (a `rocksdb.cur-size-all-mem-tables` property read; `MemoryEngine` reports 0). ST1 D3 measured an LSM batch at 1.1–1.5× its logical size while pending and 2.2–2.7× at its peak while the memtable copy coexists with the batch; both are inside the invariant because both terms are counted. After Bk, W calls `flush_memtables()` (ST1 D2: 0.16–0.82 s after 16–256 MiB) so the term returns to about zero between flushes; §5.2 explains why this is what makes the DEC-43 cell winnable.
- `size(active)` and `size(frozen)` are `table_capacity × slot_size + Σ heap(PackedCoin)`; `size(prefetch)` likewise. All three are maintained arithmetically on every insert, remove and grow, and a counting global allocator in the bench and test builds asserts, at every freeze and release, that the arithmetic equals the allocator's live bytes for those maps to within the allocator's chunk rounding (PLAN CH2 acceptance "budget invariant asserted with a counting allocator"). A layout test asserts `size_of::<(OutPointKey, Entry)>() == 72`, `align == 8`, and hashbrown's load factor.
- **Why `T` includes the mempool allowance (§8 Q1).** Core's trigger uses `nTotalSpace = coins + max(maxmempool − mempool_usage, 0)` (`validation.cpp:2742-2745 / :2950-2953`); at the defaults during IBD that is 300 + 300 MiB. A1 §3.4 bounded only the coins share. With the bare share, rdashd at `-dbcache=300` would flush about twice as often as dashd during IBD and lose the equal-flags comparisons that gate every milestone. The DEC-43 cell removes the allowance on both sides, so both regimes are measured. The memory bound stays Core's: Core's cache can reach `nTotalSpace` before a CRITICAL flush, and so can ours.
- **The freeze fraction.** `f = 0.5` (A1). Core freezes nothing and flushes at `LARGE` (90% of T, only at `PERIODIC` points) or `CRITICAL` (100%, at `IF_NEEDED`). With `f = 0.5` our first write starts earlier and the second half of T absorbs connects during the write; V waits only if the write takes longer than it takes V to fill the remaining half. PF3 tunes `f` on the churn trace; the model test asserts the invariant for every `f ∈ (0, 1)`.

### 2.5 `PrefetchMap` (the consumer side of CH4's S2)

```rust
pub struct PrefetchMap { map: HashMap<OutPointKey, PackedCoin, SaltedFxBuild>, gen: u64, bytes: usize }
impl CoinsCache<E> {
    pub fn snapshot_for_prefetch(&self) -> (E::Snapshot<'_>, u64 /* fetch_gen = released_gen */);
    pub fn admit_prefetch(&mut self, fetch_gen: u64, results: Vec<(OutPointKey, PackedCoin)>) -> usize;  // dropped unless fetch_gen == released_gen; positive only
}
```

The fence is A1 §3.4's, verbatim: results carry the `released_gen` at snapshot time; V admits only equal generations; release increments the generation and clears the map; absence is never cached; any disconnect clears the map. CH2 ships the map and the admission rule and tests them in `coins_cache_model` with a synthetic producer; CH4 ships the rayon producer and the `-consensusxcheck` comparison.

### 2.6 Tables

```rust
pub struct CoinsTable<E> { table: Table<OutPointDbKey, CoinBytes> }    // CF `coins`
pub struct MetaTable<E>  { … }                                            // CF `meta`
pub struct Meta { pub tip: Option<BlockHash>, pub head_blocks: Option<(BlockHash, Option<BlockHash>)>, pub evo_tip: Option<BlockHash>,
                  pub pruned_files: bool, pub last_file: u32, pub reindexing: bool }
```

| CF | Key | Value | Written by |
|---|---|---|---|
| `coins` | `txid[32] ‖ VARINT(vout)` (Core's `CoinEntry` without `DB_COIN`, `txdb.cpp:53-59 / :62-68`) | `CoinBytes` (Core's `Coin` bytes; CS5a) | the coin sub-batches, Bk, recovery |
| `meta` | `tip` | `BlockHash` | Bk (or B1 in a one-batch flush), recovery's final batch; **absent during a transition** |
| `meta` | `head_blocks` | `BlockHash new ‖ BlockHash old` (`old` = 32 zero bytes on the first flush, as Core's null `uint256`, `txdb.cpp:130 / :139`) | B1; erased by Bk or recovery's final batch |
| `meta` | `evo_tip` | `BlockHash` | B1 (CH7a's delta) |
| `meta` | `pruned_files`, `last_file`, `reindexing`, `schema` | as A1 §2.9 | Bk/Bp; B1; CH5; ST1 |

`gettxoutsetinfo`, `scantxoutset` and `dumptxoutset` first force a flush (`ChainCommand::Flush(Always)`) and then iterate an engine snapshot taken after Bk returned, as Core's `ForceFlushStateToDisk` then `Cursor()` does (`validation.cpp:2913 / :3122`, `txdb.cpp:217 / :226`). The iteration is in key order; `hash_serialized_2`'s precedence quirk (quirk 33) is CS5b's.

### 2.7 `FlushPolicy`, `FlushPlan`, `FlushDriver`

```rust
pub enum FlushMode { None, IfNeeded, Periodic, Always }          // Core's FlushStateMode (validation.h)
pub enum FlushCause { CacheLarge, CacheCritical, EvoCritical, PeriodicFlush, Always, Prune, DMax, Scripted }
pub struct FlushDecision { pub full: bool, pub index_only: bool, pub causes: Vec<FlushCause>, pub prune: Option<PruneDecision> }

impl FlushPolicy {
    /// Core's FlushStateToDisk trigger block, evaluated on V under its clock (validation.cpp:2776-2838 / :2984-3046).
    pub fn decide(&mut self, mode: FlushMode, manual_prune_height: Option<u32>, now: Instant, coins: &CoinsBudget,
                  mempool_usage: usize, evo_pending_bytes: usize, prune: &mut dyn PruneHooks, subscribers: &SubscriberSet) -> FlushDecision;
}

/// Everything W needs, captured by V at freeze time. V keeps connecting afterwards.
pub struct FlushPlan {
    pub flush_id: u64, pub old: Option<BlockHash>, pub new: BlockHash, pub new_height: u32,
    pub frozen: Arc<FrozenLayer>,                        // the coin delta old → new
    pub non_coin: NonCoinDelta,                          // CH7a: evo CF puts/erases, `evo_tip = new`, `bls_legacy`; plus dirty block_index records and `last_file` (CH1)
    pub presumed_effect_pending: bool,                   // CH7a (ST3a §2.4.4): barrier before B1
    pub prune: Option<PruneDecision>,                    // step 0's retained dry run D, met_target, flush_for_prune
    pub causes: Vec<FlushCause>, pub index_only: bool,
}

pub struct FlushDriver<E: KvEngine, F: FlatFs> { … }
pub enum FlushStep { Space, FilesBarrier, PresumedBarrier, B1, Flushed, SubscriberCommit, PruneSelect, SubBatch(u32), PreBkBarrier, Bk, Release, Unlink, Done }
impl<E, F> FlushDriver<E, F> {
    pub fn start(&mut self, plan: FlushPlan) -> Result<(), FlushError>;
    pub fn step(&mut self) -> Result<FlushStep, FlushError>;   // one protocol step; the seam drives this on one thread, W loops it
    pub fn run(&mut self) -> Result<FlushOutcome, FlushError> { loop { if self.step()? == Done { break } } … }
}
```

**Trigger equivalence** (`FlushPolicy::decide`, both pins identical; line pairs `v23 / v24`):

| Core | Ours |
|---|---|
| `fCacheLarge = PERIODIC && state ≥ LARGE` (`:2828 / :3036`) | `CacheLarge` iff `mode == Periodic && budget.state() ≥ Large` |
| `fCacheCritical = IF_NEEDED && state ≥ CRITICAL` (`:2830 / :3038`) | `CacheCritical` iff `mode == IfNeeded && state == Critical`; **plus** the freeze point `should_freeze()` at `f·T`, which has no Core counterpart and is invisible (it does not change which block is the tip of a durable state the operator can observe; it only starts the write earlier) |
| `fEvoDbCacheCritical = IF_NEEDED && evoDb.GetMemoryUsage() ≥ 64 MiB` (`:2832 / :3040`) | `EvoCritical` iff `mode == IfNeeded && evo_pending_bytes ≥ 64 MiB` (CH7a reports the pending deltas' size) |
| `fPeriodicWrite = PERIODIC && now > last_write + 1 h` (`:2834 / :3042`; `:97 / :104`) | `index_only` iff `mode == Periodic && now > last_write + 1 h` |
| `fPeriodicFlush = PERIODIC && now > last_flush + 24 h` (`:2836 / :3044`; `:99 / :106`) | `PeriodicFlush` |
| `fDoFullFlush = ALWAYS ∨ large ∨ critical ∨ evo ∨ periodicFlush ∨ fFlushForPrune` (`:2838 / :3046`) | `full` iff any of those causes, where `Prune` iff `D ≠ ∅` (DEC-64, ST3a E20), and the two new causes `DMax` (a subscriber at its limit, ST3a §5.6 item 6) and `Scripted` (tests) |
| `m_last_write`/`m_last_flush` initialised at first call ("avoid flushing immediately after startup", `:2820-2826 / :3028-3034`) | same |
| ATMP calls `FlushStateToDisk(PERIODIC)` after `Uncache` (`:1422, 1454 / :1748, 1780`) | `ChainCommand::Flush(Periodic)` from the mempool actor; `uncache(op)` erases a `flags == ∅` entry (`coins.cpp:284-296`) |
| `ConnectTip`/`DisconnectTip` call `IF_NEEDED` (`:3207, 3070 / :3457, 3304`) | CH3b calls `decide(IfNeeded)` at the same points |
| `ForceFlushStateToDisk` (`ALWAYS`), `PruneAndFlush` (`NONE` with `m_check_for_pruning` set) (`:2913, 2921 / :3122, 3141`) | `Flush(Always)`, `Flush(None)` with the prune check armed |
| `ResizeCoinsCaches`: grow ⇒ `IF_NEEDED`, shrink ⇒ `ALWAYS` (`:5502 / :5866`) | `CoinsCache::resize(new_share)` does the same |

**The flush, step by step** (DEC-64 / ST3a §6.2 and E12; W runs it; each step is one `step()`):

| # | `FlushStep` | What happens | Core (v23 / v24) | † |
|---|---|---|---|---|
| 0 | (on V, in `decide`) | **Prune decision** when a check is due (`m_check_for_pruning` from ST2's `take_check_for_pruning()`, or a manual height): a side-effect-free dry run under the **predicted post-commit lock** (synced subscribers at `new`, syncing ones at `height(LCA(durable, new))`); retain `D` and `met_target`; `flush_for_prune ⇔ D ≠ ∅`; log "`<name>` limited pruning to height X" from the predicted lock; clear the automatic check | `:2781-2814 / :2989-3022` selects **once**, against the in-memory tip | our dry run + final selection replace Core's single selection |
| 1 | `Space` | `CheckDiskSpace(blocks/, 50 MiB)` and, for a full flush, `CheckDiskSpace(datadir, 48·2·2·coins_count)` (`coins_count` = entries of F) **before anything is written** | `:2842, 2880 / :3050, 3088`; Core checks the second one after the index write | — (ordering only) |
| 2 | `FilesBarrier` | `BlockFiles::sync_and_take_dirty_infos()` (the **first** take): every blk/rev file written since the last barrier is `fdatasync`ed, the directory `fsync`ed if a file was created, truncated or removed; the returned `DirtyFiles` are pre-prune by construction | `FlushBlockFile` syncs the current pair only (`blockstorage.cpp:557-567 / :585-605`) | † |
| 3 | `PresumedBarrier` | `sync_wal()` iff `plan.presumed_effect_pending` (ST3a §2.4.4) | none | † (stricter) |
| 4 | `B1` | one `Commit::Durable` batch: every dirty `block_index` and `block_files` record, `last_file`, every non-coin delta of F in block order, `evo_tip = new`, `head_blocks = [new, old]`, erase `tip`. **One-batch flush:** if `Σ size(F's coin ops) ≤ batch_size`, the coin ops, `tip = new` and no marker go in B1 instead; a one-batch flush that prunes ends with **Bp** (step 9) | `WriteBatchSync` (`txdb.cpp:267-277 / :276-286`) then `BatchWrite` (`:104-165 / :113-174`) then `CommitRootTransaction` | † evo and marker atomic with the index |
| 5 | `Flushed` | emit `Mark::Durable` and `ChainEvent::ChainStateFlushed { locator(new) }` (DEC-64, E16: after B1, not after Bk) | `:2905 / :3114`, after the whole flush | — |
| 6 | `SubscriberCommit` | for every **synced** mandatory subscriber: wait for its drain through `Connected(new)` along p → `LCA(p, new)` → `new`, with `D_max` checkpoints; it then submits staged data through `LCA(p, new)`, waits, `sync_wal`, commits its locator (Durable) and acks (§2.9). Syncing subscribers are not waited for | `BaseIndex::ChainStateFlushed` queued behind `BlockConnected` (`index/base.cpp:290-327 / :332-369`) | † (Core's locator can be ahead of its chainstate's durable tip) |
| 7 | `PruneSelect` | iff `flush_for_prune`: actual bound `min(new − K, manual, min over mandatory subscribers of height(LCA(durable locator, new)) − PRUNE_LOCK_BUFFER − 1)`; selection `D ∩ eligible(actual)`; `prune_one_block_file` on each (in-memory clears); re-arm iff the target is unmet while D had met it; `Mark::PruneSelected` | selection before the index write, in-memory locks (`:2781-2814 / :2989-3022`; `blockstorage.cpp:202 / :198`) | † |
| 8 | `SubBatch(i)` | split flush only: coin sub-batches of ≤ `batch_size` each, `Commit::Buffered`, built with `batch_with_capacity`; puts for `DIRTY` unspent, erases for `DIRTY` spent, nothing for `FRESH ∧ spent` (Core's `BatchWrite`, `txdb.cpp:132-156 / :141-165`); the "Writing partial batch of %.2f MiB" log line; the `-dbcrashratio` `_exit` after each committed sub-batch | `:145-154 / :154-163` | — |
| 9 | `PreBkBarrier` | split flush only: `sync_wal()` (DEC-63 Q14) | none | † |
| 10 | `Bk` / `Bp` | `Commit::Durable`: the last coin ops, the **second** `sync_and_take_dirty_infos()` (reset infos after pruning plus any synced V-ahead appends), the prune clears, `pruned_files = true` if anything was ever pruned, `tip = new`, erase `head_blocks`; "Writing final batch", "Committed %u changed transaction outputs (out of %u)" | `txdb.cpp:158-165 / :167-174` (Buffered in Core) | † Durable |
| 11 | `Release` | hand F's generation back to V; W then `flush_memtables()` (budget term back to ~0) | `ReallocateCache` (`coins.cpp:315-323`) | — |
| 12 | `Unlink` | `unlink_pruned_files(selection)` in a best-effort region, then `sync_dir` | `UnlinkPrunedFiles` before the coins write (`:2862-2866 / :3070-3074`; `blockstorage.cpp:580-588 / :618-629`) | † |
| — | index-only write | step 2, then one Durable batch of dirty `block_index`, `block_files`, `last_file` | `fPeriodicWrite` (`:2840-2868 / :3048-3076`) | — |

**V and W concurrency.** V's writes between `FlushBegin` and Bk (block appends, index updates, effects, new `active` entries) are **not** in any finalizer's scope (ST3a E18): the second dirty-info take at step 10 covers only what ST2's barrier has synced, and B1's `PreviousFinalizer` scope is satisfied because the previous Bk returned, with its barrier, before this flush began. W never counts an in-flight V write as covered.

### 2.8 `Recovery`

```rust
pub struct Recovery<E: KvEngine, F: FlatFs> { … }
pub enum RecoveryOutcome { Consistent { tip: BlockHash }, Replayed { new: BlockHash, old: Option<BlockHash>, replayed: Vec<BlockHash> } }
impl<E, F> Recovery<E, F> {
    /// Runs at open, after the prune locks and the index are loaded and before VerifyDB and activation (ST3a §5.5 steps 0–5).
    pub fn run(engine: &E, files: &BlockFileReader<F>, index: &BlockIndex, marks: &mut dyn Marks) -> Result<RecoveryOutcome, RecoveryError>;
    pub fn replayed(&self) -> &[BlockHash];       // ST3a §5.2 `ChainstateView::replayed`
}
```

Steps (§4.4 has the invariants):
0. `engine.sync_wal()` before any write (E21): promotes a ProcessKill epoch's pending batches so no finalizer of the new epoch presumes an unbarriered batch.
1. Read `meta`. **No marker:** `tip` must be present and equal `evo_tip`; otherwise refuse with Core's "Found EvoDB inconsistency, you must reindex to continue" (`validation.cpp:2004 / :2290`; unreachable by construction, kept as a corruption check).
2. **Marker `[new, old]`:** `new` in the index with `HAVE_DATA`; `old` null or in the index; `evo_tip == new`. Any failure: "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." (`init.cpp:2066 / node/chainstate.cpp:251-252`). Log "Replaying blocks".
3. `fork = LCA(old, new)`. For `b` from `old` down to `fork + 1`: read `b`'s undo (checksummed) and block; log "Rolling back %s (%i)"; for each tx from last to first and each input: **put** the undo coin (height from the record, or `DISCONNECT_FAILED` when a zero-height record has no alternate output, as `ApplyTxInUndo` `:1966-1991 / :2252-2277`); for each output: **erase**. For `b` from `fork + 1` to `new`: read `b`; log "Rolling forward %s (%i)"; for each non-coinbase input: **erase**; for each spendable output: **put** (`RollforwardBlock` `:4796-4853 / :5207-5216`, `AddCoins(check = true)`). No key is read before it is written (E22): the model's branch count stays linear, and the result is identical because every operation is an overwrite or an erase.
4. Ops go into Buffered sub-batches of `batch_size` with the marker still present; then `sync_wal`; then one Durable batch with `tip = new` and `head_blocks` erased. Progress is Core's `ShowProgress("Replaying blocks…")`.
5. An interruption anywhere leaves the marker, and the next start re-runs steps 2–4 to the same result (I4).

What Core's `ReplayBlocks` does that we **do not**: `ProcessSpecialTxsInBlock` in `RollforwardBlock` (`:4787 / :5197`) and `DisconnectBlock`'s evo undo (`:4925 / :5275`), because B1 already committed evo at `new`; and the EvoDB best-block check against `old` (`:4910 / :5258`), replaced by the `evo_tip == new` check.

### 2.9 Subscribers

```rust
pub enum SubscriberClass { Synced, Syncing }                               // Core's m_synced (index/base.cpp:157 / :169)
pub enum LocatorKind { CommitStep, Checkpoint, Cadence, Rewind }
pub struct Locator { pub tip: Option<BlockHash> }                            // `locator` key in the subscriber's own `meta` CF

/// What an index, `llmq/` or a wallet implements: its data writes. The driver owns every locator rule.
pub trait SubscriberData {
    type E: KvEngine;
    fn name(&self) -> &'static str;
    fn needs_undo(&self) -> bool;                                            // coinstatsindex: true
    fn start_height(&self) -> u32;                                           // lock contribution with no locator (DEC-64)
    fn apply_connected(&mut self, block: &ConnectedBlock, batch: &mut <Self::E as KvEngine>::Batch) -> Result<()>;   // staged until submit
    fn apply_disconnected(&mut self, block: &DisconnectedBlock, batch: &mut <Self::E as KvEngine>::Batch) -> Result<()>;
    fn blind_rewind(&mut self, range: HeightRange, batch: &mut <Self::E as KvEngine>::Batch) -> Result<()>;   // delete_range over height-keyed CFs; never reads
}

pub struct SubscriberDriver<D: SubscriberData> {
    data: D, engine: Arc<D::E>, class: SubscriberClass,
    locator: Locator /* L, barrier-covered */, processed: Option<BlockHash> /* p */, submitted: Option<BlockHash> /* s */,
    d_max: u32 /* 1,024 default */, cadence: Duration /* 30 s, Core's SYNC_LOCATOR_WRITE_INTERVAL */, staged: Option<Batch>,
}
impl<D> SubscriberDriver<D> {
    pub fn open(..) -> Result<Self>;                          // reads L; blind rewind of (h(L), h(L) + d_max]; catch-up is scheduled by the class
    pub fn on_event(&mut self, ev: &ChainEvent) -> Result<()>; // Synced: queue-driven; applies backpressure at h(L) + d_max
    pub fn on_flush_point(&mut self, fp: &FlushPoint /* new, T_ack */) -> Result<LocatorAck>;   // step 6: path drain, checkpoints, commit at LCA(p, new)
    pub fn sync_thread_step(&mut self, chain: &ChainReadSnapshot) -> Result<()>;               // Syncing: ThreadSync with capped cadence commits
    pub fn prune_lock_height(&self, new: &BlockId, index: &BlockIndex) -> u32;                   // height(LCA(L, new)) − PRUNE_LOCK_BUFFER − 1, or start_height
    pub fn emulated_lock_height(&self) -> u32;                 // log-only moveback emulation (ST3a §5.6 item 7)
}
```

The rules are ST3a §5.6 item 6, implemented once:
- **Class** comes from the mode at init (`m_synced` set when the index's best block is the tip, `index/base.cpp:78 / :90`) or from a syncing thread reaching the tip with a capped commit (`:154-158 / :166-170`); never from an instantaneous test (T35).
- **Synced commit at step 6:** path-based drain p → `LCA(p, new)` → `new`, dropping queued events off the path; a **checkpoint** when the next connect would pass `h(L) + D_max`: submit staged data through p, wait, `sync_wal`, Durable locator at p (T37, T38, T41); then submit through `LCA(p, new)`, wait, `sync_wal`, Durable locator at `LCA(p, new)`, ack.
- **Syncing commit:** every `cadence`, submit staged data through the target, `sync_wal`, commit at `LCA(progress, T_ack)` (Buffered allowed, T33 forbids uncapped); at the limit, a capped commit, and a `FlushCause::DMax` request if that does not raise L.
- **Rewind commit:** before any disconnect below `h(L)`: `sync_wal`, then one Durable batch that blindly `delete_range`s `(h(F), h(L) + D_max]` and sets the locator to F (T36; BL-38's `delete_range`).
- **Two tips:** `p` (processed) and `s` (submitted); a locator at X requires X to be an ancestor of, or equal to, `s` when its `sync_wal` starts (T22, T41).
- **Open:** blind rewind of `(h(L), h(L) + D_max]`, then catch-up `(L, tip]` with block and undo data per `needs_undo`; the sanctioned refusal "…best block of the index goes beyond pruned data…" (`index/base.cpp:110 / :122`) when a block on L's path to the tip is pruned.
- **Backpressure:** no data above `h(L) + D_max`; the driver reports `blocked` to `SubscriberSet`, which `FlushPolicy::decide` reads as a `DMax` cause.

Who is mandatory: `llmq/` plus every enabled index (DEC-63 Q10a); wallets keep Core's startup check and are not in the prune lock.

### 2.10 The ST3a seam

```rust
pub trait Marks { fn mark(&mut self, m: Mark); }            // ST3a §3.2's Mark; production: a no-op or the trace logger
pub struct Salt(u64);                                        // SaltedFxBuild's salt; Salt::random() in production, Salt::fixed(k) under the seam
pub trait Clock { fn now(&self) -> Instant; }               // FlushPolicy's intervals; mockable (setmocktime)
```

`FlushDriver::step`, `Recovery::run`, `SubscriberDriver::{on_flush_point, sync_thread_step, open}` and V-ahead (`CoinsCache::freeze` + connects) are all plain synchronous functions, so ST3a's `Sut` for CH2 (`dashd-chainstate/tests/power_loss_chainstate.rs`) drives them on one thread, in any interleaving the scenario asks for, with `FaultyEngine`/`FaultyFlatFiles` substituted for the generics. The `RefExtractor` for the production schema is CH2's (`seam.rs`): index entries require their bytes; a durable marker requires its replay path; a locator requires its path to G and `DurableTipAtLeast`; `finalizes` returns `SinceFlushStart` for Bk, recovery's sub-batches for its final batch, `Keys` over the presumed-effect CFs plus `PreviousFinalizer` for B1, and `SincePreviousLocator` plus the cross-store barrier for every locator commit.

### 2.11 Neighbours

| Task | Direction | Types exchanged |
|---|---|---|
| **ST1** | upstream | `KvEngine`, `KvBatch`, `Commit`, `Table`, `KeyCodec`/`ValueCodec`, `RocksConfig`, `ErrorKind`, the fail-stop latch; **adds:** `KvBatch::delete_range` (BL-38), `VarintU32`-suffixed key codec, `RocksEngine::memtable_bytes()`, `RocksConfig::coins_profile` |
| **ST2** | upstream | `BlockFiles<F>` (`sync_and_take_dirty_infos`, `take_check_for_pruning`, `find_files_to_prune[_manual]`, `prune_one_block_file`, `unlink_pruned_files`, `calculate_current_usage`, `has_failed`), `BlockFileReader<F>` (`read_block`, `read_undo`), `DirtyFiles`, `BlockFileInfo`, `FlatFs`, `CoreBytes`, `CoreUndoBytes`, `MIN_BLOCKS_TO_KEEP`, `Release` |
| **ST3a** | upstream | `World`, `FaultyEngine`, `FaultyFlatFiles`, `Mark`, `RefExtractor`, `Sut`, `explore`/`sweep`/`minimize`; the §5.6 contract |
| **CS0** | upstream | `Block`, `Tx`, `TxRef`, `Header`, `BlockHash`, `CoreBytes`, `UngatedRules` (txids for keys: "anything keyed by txid takes the network's rules", CS0 §11) |
| **CS5a** | upstream | `coin_bytes` encoder, `CoinRef<'a>` borrowed decoder, `CoinsView`, the undo-record decoder yielding per-input coin bytes and heights; §8 Q2 |
| **CH1** | upstream | `BlockIndex` (arena, `BlockId`, LCA, `HAVE_DATA`/`HAVE_UNDO`, dirty-record take for B1), `ChainReadSnapshot`, `ChainEvent` bus |
| **CH3a** | downstream | `CoinsCache::{fetch, add_coin, spend_coin, access_by_txid, uncache}`, `BlockOverlay::{new, commit, drop}`, `CoinRef` |
| **CH3b** | downstream | `FlushPolicy::decide`, `ChainCommand::Flush(FlushMode)`, `CoinsCache::{freeze, wait_released}`, `FlushPlan` assembly, the `Prune` and `DMax` causes |
| **CH4** | downstream | `PrefetchMap`, `snapshot_for_prefetch`, `admit_prefetch`, `released_gen` |
| **CH5** | downstream | `Recovery::run` placement (after the index load, before `VerifyDB`); `-reindex-chainstate` wipes `coins` and `meta.{tip,head_blocks,evo_tip}` and the evo CFs, keeps `block_index`/`block_files` |
| **CH7a** | downstream | `NonCoinDelta` (prepared batch ops for the evo CFs + `evo_tip`), `presumed_effect_pending`, `evo_pending_bytes`; effects are their own Buffered single-row batches (A1 §3.5.7) |
| **CH8** | downstream | `PruneHooks` (the dry run, the actual-lock bound, `D ∩ eligible`, re-arm), `PruneDecision`, the manual height, `pruned_files`; the FT cross-check |
| **IX1/IX2, LK1b, WL** | downstream | `SubscriberData`, `SubscriberDriver`, `FlushPoint`, `LocatorAck` |
| **ND1/RL2** | downstream | `getmemoryinfo` buckets (§5.2), the abort path (`AbortNode` → shutdown, exit 1), the Core strings |
| **PF0/PF1/PF3** | downstream | the workloads and counters of §5.5; the counting allocator |

---

## 3. Core-equivalence

### 3.1 Function map

| Core (v23.1.8 / v24.0.0-rc.3) | Rust | Notes |
|---|---|---|
| `CCoinsViewCache::FetchCoin` (`coins.cpp:44-59`, both) | `CoinsCache::fetch` | layered; a `SPENT` parent entry becomes `SPENT∣FRESH` in the child |
| `GetCoin`, `HaveCoin`, `HaveCoinInCache`, `AccessCoin` (`:61-68, 152-169`) | `coin`, `have`, `have_in_cache`, `access` | `AccessCoin` returns `CoinRef` or the empty coin |
| `AddCoin` (`:70-106`), `AddCoins` (`:118-127`) | `add_coin`; the loop is CH3a's | `IsUnspendable` skip; `possible_overwrite` rules; the two `logic_error`s |
| `SpendCoin` (`:129-150`) | `spend_coin` | `FRESH` ⇒ erase, else `DIRTY` + clear |
| `GetBestBlock`/`SetBestBlock` (`:171-180`) | `best`/`set_best` | `None` falls through to the DB's `tip` |
| `BatchWrite` child→parent (`:182-252`) | `BlockOverlay::commit` | `erase = true` |
| `Flush` (`:254-265`) | `freeze` + the flush | our flush does not empty the cache |
| `Sync` (`:267-282`) | not exposed (NG-6) | our frozen layer already has its semantics |
| `Uncache` (`:284-296`) | `uncache` | only `flags == ∅` entries |
| `GetCacheSize`, `DynamicMemoryUsage` (`:40-42, 299-301`) | `len`, `budget.used()` | `cachedCoinsUsage` ⇒ Σ heap bytes; `DynamicUsage(cacheCoins)` ⇒ table bytes |
| `ReallocateCache` (`:315-323`) | `release` + a fresh `active` at freeze | — |
| `SanityCheck` (`:325-…`) | `sanity_check` (debug/xcheck) | the 5 legal states (`coins.h:94-102`) |
| `CoinsCacheSizeState`, `GetCoinsCacheSizeState` (`validation.cpp:2729-2759 / :2937-2967`) | `CoinsSizeState`, `CoinsBudget::state` | thresholds verbatim; the `"Cache size (%s) exceeds total space (%s)"` log line |
| `FlushStateToDisk` (`:2761-2911 / :2969-3120`) | `FlushPolicy::decide` + `FlushDriver` | §2.7 |
| `ForceFlushStateToDisk`, `PruneAndFlush` (`:2913-2928 / :3122-3148`) | `Flush(Always)`, `Flush(None)` | — |
| `CCoinsViewDB::BatchWrite` (`txdb.cpp:104-165 / :113-174`) | `SubBatch(i)` + `Bk` | marker first, sub-batches, `-dbcrashratio`, final batch |
| `GetBestBlock`, `GetHeadBlocks` (`:89-102 / :98-111`) | `MetaTable::{tip, head_blocks}` | — |
| `CoinEntry` key (`:53-59 / :62-68`) | `OutPointDbKey` | no `'C'` |
| `CBlockTreeDB::WriteBatchSync` (`:267-277 / :276-286`) | part of B1 and the index-only write | — |
| `CheckDiskSpace` (`util/system.cpp` / `util/fs_helpers.cpp`) | `check_disk_space(dir, extra)` via `FlatFs::available_space` | 50 MiB floor |
| `ReplayBlocks`, `RollforwardBlock` (`validation.cpp:4879-4954, 4772-4877 / :5222-5305, 5180-5220`) | `Recovery::run` | coins-only (§2.8) |
| `ApplyTxInUndo` (`:1966-1991 / :2252-2277`) | CH3a on the live path; `Recovery` replicates its put/overwrite result | `undo.nHeight == 0` ⇒ `DISCONNECT_FAILED` |
| `LoadChainTip` (`:4608 / :4992`) | CH3b/CH5 after `Recovery` | — |
| `ResizeCoinsCaches` (`:5502 / :5866`) | `CoinsCache::resize` | — |
| `CalculateCacheSizes` (`node/caches.cpp:12-33 / :15-42`) | `DbCacheSplit::compute` (§5.2) | same inputs; our buckets differ (RocksDB), the sum is the user's number |
| `CCoinsViewErrorCatcher` (`coins.cpp:355-363 / :355-380`) | read errors on the coins path are `StoreError` → `AbortNode` | both pins log `"Error reading from database: …"` and abort the process on a `GetCoin` read error; v24 also routes `HaveCoin` through the catcher (`:377-380`, bitcoin#26331, O17 CONS-22, `v24-all [I → RL2]`), where v23.1.8 lets the exception escape. rdashd aborts on any coins read error under both profiles; RL2 documents it |
| `feature_dbcrash.py` (`-dbcrashratio`, `-dbcache=4/8/16`, `-maxmempool=0`; `:53-69` v24) | the same flags | §6.5 |

### 3.2 Entry state machine

The five legal `(DIRTY, FRESH, spent)` combinations (`coins.h:94-102`) and every transition are ported as a table test and as the `sanity_check` twin. The model test (`coins_cache_model`) runs a reference `BTreeMap<OutPointKey, Option<Coin>>` plus a reference flag map through the same operations and compares after every step, including after freeze and after sub-batch completion (where the reference asks "would Core's DB now hold this coin?"). This is Core's `coins_cache_simulation_test` and `updatecoins_simulation_test` (`src/test/coins_tests.cpp`) generalised to our layers; the two Core tests themselves are ported verbatim as L0 (§6.1).

### 3.3 Order of operations and deviations

Core's flush order is files → index → prune unlink → coins → EvoDB → `ChainStateFlushed` (`validation.cpp:2840-2905 / :3048-3114`). Ours is files → B1 (index + evo + marker) → `ChainStateFlushed` → subscriber commits → prune selection → coins → Bk (tip + clears) → unlink. The visible differences are listed with their justification:

| † | Ours | Core | Visible when |
|---|---|---|---|
| 1 | every blk/rev file written since the last barrier is synced | the current pair only (`blockstorage.cpp:557-567 / :585-605`) | power loss with lagging undo (ST3a T6) |
| 2 | evo deltas and the marker commit atomically with the index in B1 | coins, then EvoDB, in separate DBs (`:2884→2889 / :3092→3097`) | a kill between them: "Found EvoDB inconsistency" (ST3a §7.2 window 2; T4, T24) |
| 3 | prune selection after the subscriber commits against the frozen tip; index clears in Bk; unlink after Bk | selection against the in-memory tip before the index write; unlink before coins (`:2862 / :3070`) | a crash after the index write: "Unable to replay blocks" in prune mode (ST3a §7.2 window 1; T5, T13, T15, T16, T19, T30) |
| 4 | a failed file sync, `write`, `sync_wal` or batch aborts the flush and the node | `FlushBlockFile` returns `void` and the flush continues (`blockstorage.cpp:600`) | fsync failure (T9) |
| 5 | `ChainStateFlushed` after B1 | after EvoDB | timing only; subscribers commit at a tip the marker already guarantees (E16) |
| 6 | Bk, Bp and recovery's final batch are Durable behind a `sync_wal`; B1 is Durable | the index write is synced, coins and EvoDB are not | one or two extra WAL fsyncs per full flush (ST1 D2: 8–88 ms) |
| 7 | subscriber locators are never ahead of the chainstate's durable tip | `ThreadSync` commits its progress (`index/base.cpp:187-192 / :202-207`) | power loss; Core tolerates its state by re-syncing |

None of these changes a healthy node's RPC answers, log lines a functional test asserts, or P2P behaviour. Two of them fix Core's own process-kill holes (ST3a §7.2); reporting those upstream is pasta's call (DEC-59, DEC-62).

### 3.4 Pruning hook points (DEC-64; the protocol's part of CH8)

CH2 implements the decision and selection logic as ST3a §6.2 steps 0 and 7 specify, over ST2's `find_files_to_prune[_manual]`: the dry run's `D` and `met_target` are retained in `PruneDecision`; the final selection is `D ∩ eligible(actual)` and never a fresh selector run (T39); re-arming follows E20 (T40); "limited pruning to height X" is logged from the predicted lock, and the moveback line "`<name>` prune lock moved back to X" is emulated log-only from each subscriber's emulated height on `DisconnectTip` (`validation.cpp:3061-3065 / :3295-3299`). CH8 provides the configuration (`-prune`, the target, `PruneAfterHeight`), the manual RPC, and the FT cross-check.

### 3.5 Quirks and pins

- **Quirk 33** (unspendable outputs never in the UTXO set): `add_coin` skips `IsUnspendable` scripts (`coins.cpp:72`); CS5a's encoder is never called for them; the coinstats part is CS5b's.
- **BIP30 overwrite:** on connect, a coinbase output may overwrite (`AddCoins(…, check=false)` ⇒ `overwrite = fCoinbase`, `coins.cpp:118-127`); on replay every output may (`check = true`). A non-coinbase overwrite of an unspent coin on connect is the `logic_error` ⇒ abort.
- **Old undo metadata:** a zero-height undo coin takes the alternate-output path or fails the disconnect (`:1966-1991 / :2252-2277`); Dash's undo files have always carried the height (DIP2-era serialization), so the path is reachable only with imported or corrupted data; it is kept because `feature_dbcrash`-style replay reads real undo records.
- **Per profile:** CONS-22 (read-error abort) is `v24-all` (§3.1); everything else in this task is identical at both pins (`coins.cpp` and the flush/replay code differ only in line numbers and in v24's `EvoDbIdentity`, which is assumeutxo plumbing, NG-6). ST2's `Release` governs the rev-finalize rule (BL-29) inside the files barrier. No new `ReleaseProfile` field is needed.

### 3.6 Error strings and abort behaviour (I5)

| Site | String | Ours |
|---|---|---|
| `CheckDiskSpace(blocks/)` fails (`:2842-2843 / :3050-3051`), or `CheckDiskSpace(datadir, 48·2·2·n)` (`:2880-2881 / :3088-3089`) | `"Disk space is too low!"` (log and user message) | both checks at step 1, same strings |
| `FlushBlockFile` failure (`blockstorage.cpp:553, 562 / :581, 600`) | `"Flushing block file to disk failed. This is likely the result of an I/O error."` | from ST2's `FlatFileError`; abort |
| `WriteBlockIndexDB` failure (`:2858 / :3066`) | `"Failed to write to block index database"` | B1 or the index-only write fails |
| `CoinsTip().Flush()` failure (`:2885 / :3093`) | `"Failed to write to coin database"` | a sub-batch or Bk fails |
| `CommitRootTransaction` failure (`:2890 / :3098`) | `"Failed to commit EvoDB"` | unreachable (evo is in B1); kept for a B1 failure whose cause is an evo-CF op, so the operator sees Core's text for an evo write |
| `std::runtime_error` from LevelDB (`:2908 / :3117`) | `"System error while flushing: <what>"` | any `StoreError` not mapped above, including `ErrorKind::NoSpace` after a write |
| startup | `"Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate."` | §2.8 step 2 |
| startup, no marker, `tip ≠ evo_tip` | `"Found EvoDB inconsistency, you must reindex to continue"` | corruption check |
| `-dbcrashratio` (`txdb.cpp:147-154 / :156-163`) | `"Simulating a crash. Goodbye."` then `_Exit(0)` | after each committed sub-batch with probability `1/ratio` (Core's `FastRandomContext`; the seam fixes the RNG) |

After any of these, nothing further is committed: `FlushDriver` enters `Failed`, the engine's latch refuses later writes (`ErrorKind::ReadOnly`), `BlockFiles::has_failed` is set, `AbortNode` starts shutdown with exit status 1 (`InitRules.abort_exit_failure`, ND1), and the shutdown flush is skipped. A failed `fdatasync` is never retried.

---

## 4. Persistence, crash consistency and recovery

### 4.1 Invariants

A1 §3.5.3's I1–I5, under ST3a's corrected assumptions:
- **I1** data before reference: the files barrier precedes B1 and the index-only write; `HAVE_UNDO` is set only after `write_undo` returns (ST2).
- **I2** every image recovers: with **subset semantics** (DEC-62) for Buffered batches between completed barriers. The protocol never presumes a Buffered batch survived unless a barrier that started after it returned has returned (§4.2).
- **I3** no validation bypass: recovery touches only the marker's transition; `activate_best_chain` validates everything above it.
- **I4** idempotent recovery: every recovery op is a put or an erase; the marker survives until the final Durable batch.
- **I5** fail-stop.

### 4.2 The barrier rule with scopes (ST3a §2.4.4, normative)

| Finalizer | Scope it presumes | Barrier before it | Commit |
|---|---|---|---|
| **Bk** | this flush's coin sub-batches (`SinceFlushStart`) | `sync_wal` at step 9 | Durable |
| **Bp** | nothing pending (B1 carried the coins); kept Durable so the unlink may follow | — | Durable |
| **recovery's final batch** | recovery's sub-batches | `sync_wal` | Durable |
| **B1** | every pending presumed effect (`Keys` over the presumed-effect CFs) and the previous chainstate finalizer (`PreviousFinalizer`) | `sync_wal` at step 3 iff a presumed effect is pending; the previous Bk/Bp returned Durable | Durable |
| **a locator** (commit step, checkpoint, cadence, rewind) | the subscriber's data through X (submitted, never staged) since its previous locator, plus the cross-store chainstate tip G that X is not ahead of | the subscriber's own `sync_wal` after its data batch returned; B1 returned (commit step/checkpoint) or `T_ack` (cadence) | checkpoint and rewind Durable; commit-step and cadence may be Buffered, counting `D_max` from the latest barrier-covered locator |

Concurrency: a barrier covers only writes that **returned before it started** (RocksDB's `SyncWAL` can finish before a concurrent write it raced with, `db_wal_test.cc:346-370`). W therefore sequences: wait for the batch to return → `sync_wal` → append the finalizer. The explicit `step()` ordering makes this structural, and ST3a's D1(f) checks it on every run.

### 4.3 Why subset semantics do not break the protocol

ST3a §2.4.2 reproduced RocksDB 11.8.1 recovering `{b1, b3, b4}` without `b2`. Under our protocol:
- a lost coin sub-batch with the marker still present (any crash before Bk returned) ⇒ recovery replays the transition from the files; replay is blind puts and erases, so the surviving subset is irrelevant;
- Bk cannot survive without its sub-batches, because the pre-Bk `sync_wal` returned before Bk was appended, and ST3a's native regression case 4 confirms RocksDB retains `bj` behind a completed `FlushWAL(true)`;
- the next B1 cannot survive without the previous Bk, because that Bk returned Durable before the next flush began (T34);
- a presumed effect cannot be lost under a surviving B1 (step 3);
- a locator cannot survive without its data (submit-before-sync), nor be ahead of G (commit after B1 returned; cadence capped at `T_ack`).

### 4.4 Recovery (ST3a §5.5 steps 0–5 as implemented)

1. `sync_wal` on every engine (chainstate and each subscriber) before any write.
2. Load prune locks from every mandatory subscriber's durable locator at `height(LCA(locator, recovered tip))`, before anything in the epoch can prune; a subscriber with no locator locks at its start height (T28).
3. Open `BlockFiles` from the durable `block_files` and `meta.last_file`, never from file lengths (ST2).
4. Run the startup scan if `meta.pruned_files` (ST2; Core v24 `blockstorage.cpp:412-417`, both profiles per ST3a §7.5).
5. Load the index (CH1).
6. `Recovery::run` (§2.8).
7. `LoadChainTip`, `VerifyDB`, activation (CH5/CH3b).
8. Each subscriber: blind rewind, then catch-up (§2.9).

Idempotence: steps 1–6 can be interrupted at any event and re-run; the model explores nested crashes (ST3a R7a/R7b).

### 4.5 Subscribers (the contract in one place)

ST3a §5.6 item 6 is implemented by `SubscriberDriver` (§2.9); the model proves it with S1, S2, S3, S3p, S11, S11r, S11s, S11q, S11d, S13 and the planted bugs T10, T22, T28, T30, T33, T35–T38, T41, now against the production driver.

### 4.6 Errors and disk full

- Flat-file ENOSPC/EIO ⇒ abort (§3.6). The pre-flight `CheckDiskSpace` makes the common case a clean refusal before anything is written.
- Store errors: `ErrorKind::NoSpace` from a `write` or `sync_wal` ⇒ "System error while flushing: …" ⇒ abort. RocksDB's own `NoSpace` auto-recovery (ST1 §9.5) never resumes writes because ST1's latch refuses them until reopen.
- After a repair and restart, recovery runs from the last completed barrier.
- `-dbcrashratio` exits at sub-batch boundaries; the model's `ProcessKill` at **every** event is the exhaustive form.

### 4.7 The CRASH matrix (PLAN CH2 acceptance)

| Case | Scenario (ST3a §6.2 IDs, driven against the production `Sut`) | Kinds |
|---|---|---|
| every sub-batch boundary | S4 (5 sub-batches), S4b (two split flushes) | ProcessKill at every event, PowerLoss with the lazy adversary |
| the first flush (`old` null) | S3, S3p | both |
| a reorg inside the flush window | S2 (backlog 2), S11r | both |
| interrupted recovery | S4 nested, S12 (ProcessKill → PowerLoss) | nested |
| prune windows | S5, S5b, S5c, S14, S14b | both |
| disk full, EIO | S8, S9 | faults per call |
| hardlinked file, adopt | S10, S10b | — |
| an invalid block persisted before connection | S7 (with CH3b's `Connected { applied }` marks) | both |
| subscriber shapes | S11, S11s, S11q, S11d, S11c, S13 | both |

The 41 planted bugs T1–T41 are re-planted as feature-gated mutations of the production driver (`cfg(feature = "planted-bugs")`, test builds only) so that each is caught by the real code's harness, not only by the toy. PR gate ≈ ST3a's 60 CPU-s budget plus the production driver's overhead (measured in Phase C; the DEC-63 debug `opt-level` overrides apply to `dashd-chainstate` too).

---

## 5. Performance

### 5.1 Metrics affected and the budget

| PRD metric | node-bench scenario · metric | Budget (never slower) | Route to 2× |
|---|---|---|---|
| #1 IBD | `ibd-mainnet-seed(-noav)`, `ibd-testnet-seed` · `ibd_time_s` | equal `-dbcache`, `-par`; V never blocks on a write unless the budget is exhausted | asynchronous flush + a cache that keeps serving + prefetch (CH4) remove the synchronous `FetchCoin` misses and the synchronous flush stalls that dashd pays on `cs_main` |
| #3 reindex | `reindex-chainstate-mainnet(-noav)`, `reindex-chainstate-testnet-noav`, `reindex-mainnet` · `reindex_time_s`, `blocks_per_s`, `cpu_ms_per_block` | the PLAN gate: not worse than the previous head on the window counters; the DEC-43 cell not slower than dashd | as #1, plus a parallel file scan (CH5) |
| #10 memory | `peak_rss_mib`, `steady_rss_mib`, `idle_rss_mib` | ≤ dashd at equal flags (NFR-RES-01) | the invariant bounds anonymous memory at T; denser entries; memtables returned after every Bk |
| #12 CPU per block | `cpu_ms_per_block` | not worse | no re-serialization at flush (the DB stores the packed bytes); no `Coin` materialisation on the connect path |
| flush stall p99 (ST1 OI-7) | `rocksdb.dbstats` stall lines; the W-X trace | p99 ≤ dashd's (ST1 measured 12.5 s vs 2.7 s at P300 with 2 × 16 MiB memtables) | memtables sized for the sub-batch shape (§5.2), `flush_memtables` after Bk so the next flush starts with empty memtables, `max_write_buffers = 3` with `min_write_buffer_number_to_merge = 1` |
| clean `stop` (US-NODE-04) | `shutdown_s` | not slower than dashd at equal `-dbcache` | the shutdown flush is a full flush of `active` + any in-flight frozen layer; the split keeps the final batch small |

### 5.2 The memory split (DEC-43 b)

dashd at `-dbcache=300` (`node/caches.cpp`, both pins; `dbwrapper.cpp:112-117 / :113-118`):

| Bucket | dashd | Notes |
|---|---|---|
| block tree DB | 2 MiB (`nMaxBlockDBCache`) | LevelDB: 1 MiB block cache + 0.5 MiB write buffer |
| coins DB | 8 MiB (`nMaxCoinsDBCache`) | LevelDB: 4 MiB block cache + 2 MiB write buffer; SSTs mmapped (page cache, not RSS-anon) |
| coins cache | **290 MiB** | `nTotalSpace` adds the unused mempool allowance |

rdashd (`DbCacheSplit::compute(dbcache, indexes)`; reported by `getmemoryinfo`):

| Bucket | rdashd at 300 MiB | Rule |
|---|---|---|
| RocksDB block cache (shared, all `chainstate/` CFs) | 16 MiB | `clamp(dbcache/16, 16 MiB, 64 MiB)`; partitioned filters and indexes pinned at high priority (ST1 §9.3); the bottom-level data is served from the OS page cache (buffered I/O; `-dbdirectio=1` switches to `O_DIRECT` with the whole engine share as block cache, for cgroup-capped hosts) |
| `chainstate/` memtables | 2 × 16 MiB **in flight only** | `write_buffer_size = batch_size`, `max_write_buffers = 3`, `db_write_buffer_size = 2 × batch_size`; **counted in the coins budget while occupied** and returned by `flush_memtables()` after Bk |
| non-coins CF memtables floor | 4 MiB | `block_index`, `meta`, evo CFs: 1 × 4 MiB each, capped by the DB total |
| coins share B | **280 MiB** | the remainder: `dbcache − block_cache − floor` |

So at `-dbcache=300` the steady-state coins capacity is 280 MiB against dashd's 290, and entries are expected denser (§2.2). During a flush the invariant holds the sum at T; dashd's in-flush peak is its full cache plus its 16 MiB `CDBBatch` plus the memtable, so ours is not higher. Index DBs keep Core's cache rules (`CalculateCacheSizes`'s per-index shares) with the same LevelDB-to-RocksDB mapping.

**Why this wins the DEC-43 cell, and what could lose it.** ST1 OI-1 showed RocksDB ≥ 1.6× slower than LevelDB under a *total-memory cap* because its anonymous block cache plus memtables left too little page cache for the database. The DEC-43 cell is a side-by-side RSS and time comparison, not a cgroup cap: a small anonymous block cache plus page-cache-served data minimises RSS-anon and keeps misses cheap. The risk is a host whose free memory is genuinely tight; `-dbdirectio` is the escape hatch, and node-bench's `--memory-cap` variant (B1b) measures that regime too. The hypothesis is tested, not assumed: Phase G runs the cell before the merge gate.

### 5.3 Data structures chosen for it

- hashbrown with a 72-byte slot, no per-entry allocation for inline coins; Fx-style hashing of a uniformly random key.
- `Arc<FrozenLayer>`: freeze is a pointer move; W iterates it without locking; V reads it lock-free (immutable).
- `batch_with_capacity(batch_size + max_record)`: no reallocation inside a sub-batch.
- Coin bytes stored as-is: the flush is a memcpy of key and value into the batch.

### 5.4 Levers kept for later (PF3), none in rev0

- L1 tombstone-only map for `SPENT` entries.
- L2 open-addressing table keyed by the txid's first 8 bytes.
- L3 adaptive `f` (freeze earlier when W is idle and the disk is fast).
- L4 retain F as a clean, read-only warm layer after release until `active` needs the room (Core's `Sync` semantics across flushes; more hits, same budget).
- L5 `MultiGet` for the DB layer of a block's inputs when prefetch missed (CH4).

### 5.5 PF0 counter workloads

Per-PR gate, deterministic counters (instructions:u, allocation count and bytes, syscalls, `/proc/<pid>/io` write bytes, page faults, peak RSS):
- `reindex-chainstate` over the four fixed 2,000-block windows (pre-DIP3, DIP3–V19, post-V20, tip − 50k), `-par=1`, at `-dbcache=300` **and** at `-dbcache=8` (the latter forces split flushes in every window).
- `coins-churn-trace`: ST1's W-M trace replayed through `CoinsCache` + `FlushDriver` over `MemoryEngine` and `RocksEngine` on `/dev/shm`, fixed window of 200k blocks, at coins shares 8 MiB and 244 MiB.
- `flush-split`: one frozen layer of 64 MiB written as 16 MiB sub-batches; counts bytes written and syscalls (the fsync count is asserted exactly: 1 pre-Bk + 1 Bk + files).
- `recovery-replay`: a marker transition of 2,000 blocks replayed coins-only.
- criterion/iai microbenches: `packed_coin_encode`, `packed_coin_decode_ref`, `coins_fetch_hit_active`, `coins_fetch_hit_frozen`, `coins_fetch_miss_db`, `overlay_commit_1k`, `subbatch_build_16mib`.

Against dashd at each milestone: the same window counters for dashd 23.1.8 / 24.0.x (`-dbcache=300`, `-par=1`); plus the DEC-43 wall-clock cell in a reserved window (CH2's merge gate and the M3 exit).

### 5.6 The multi-CF bake-off workload W-X (A1 §2.8's second trace)

Delivered before CH2 merges, in `dashd-store-bakeoff` (workspace-excluded), as `extract --net mainnet --with-evo` plus a `run.py cells wx-p300,wx-p4096`:
- **Coins** events as W-M/W-MF.
- **Evo CFs** per block: an `mn_diff` record sized from the oracle's `protx diff` at that height (bytes, not content), an `mn_snapshot` and `credit_pool` row every 576 blocks sized from the oracle, `llmq_mined`/`llmq_mined_h` puts per mined commitment, `llmq_snapshot` effect rows at cycle bases as single-row Buffered batches.
- **Reorg bursts:** every 50k blocks a depth-3 reorg (undo-driven coin restores, `llmq_mined` erases, `mn_diff` kept).
- **Concurrent readers:** 4 snapshot readers doing `gettxout`-shaped point reads and one `iter_prefix` over `mn_diff` while a flush runs.
- **Background compaction** on; measurements per CF: peak and final disk (`approximate_size` + directory bytes), flush-stall p99, miss-read p99, RSS-anon, and the ST1 §6a durability costs under the real protocol (B1 Durable, pre-Bk `sync_wal`, Bk Durable, `flush_memtables`).
- The decision rule is ST1's (RocksDB stays unless MDBX wins by > 25% on time without losing on disk or RSS); the engine must still pass every tracked metric.

---

## 6. Verification

### 6.1 L0: Core vectors and ported tests

- `src/test/coins_tests.cpp` (both pins): the `ccoins_access`, `ccoins_spend`, `ccoins_add` and `ccoins_write` state tables (the `CheckAccessCoin`/`CheckSpendCoins`/`CheckAddCoin`/`CheckWriteCoins` matrices over `{ABSENT, PRUNED, VALUE1..3}` × `{NO_ENTRY, DIRTY, FRESH}`), ported as table tests against `CoinsCache` and `BlockOverlay::commit`; the two `logic_error` cases become expected `Err`s.
- `coins_cache_simulation_test` and `updatecoins_simulation_test` ported with our layers inserted (a random freeze/release and a random flush completion are extra operations), plus the `SanityCheck` twin after every step.
- `feature_dbcrash.py` (`test/functional`, v24 `:53-69`): ported under CH5's reindex support with `-dbcrashratio` and `-dbcache=4/8/16`, `-maxmempool=0`; the recovered `hash_serialized_2` must equal node 3's.
- `feature_pruning`, `feature_index_prune`: CH8 (CH2's hook points are exercised by the model first).

### 6.2 L1: differential targets

- The coin codec is CS5a's `diff_codec_primitives`; CH2 adds no coin-level oracle entry point.
- The flush and recovery protocol has no pure Core function to shim (`FlushStateToDisk` is stateful and I/O-bound). Equivalence is shown by (a) the trigger table of §2.7 as a unit test against a transcription of Core's conditions, (b) DIFF-RPC: `gettxoutsetinfo` after forced flushes, kills and restarts on a regtest twin (dashd vs rdashd) over the same block sequence, (c) the `feature_dbcrash` port.
- **No new oracle entry point is needed.** If the manager wants one, `oracle_coins_cache_step` (a `CCoinsViewCache` over an in-memory `CCoinsView`, fed operations and asked for `DynamicMemoryUsage`, flags and contents) would let `coins_cache_model` be differential; it is cheap (O1b-style) and listed in §8 Q7.

### 6.3 L2: replay windows

- At every flush boundary and after every recovery in the L2 harness (CH3a/CH5), the coins digest of `active ∪ frozen ∪ DB` at `new` equals the oracle's `gettxoutsetinfo muhash` at that height (coinstatsindex); the digest of the DB alone after Bk equals it too.
- The `-dbcache=8` window runs make every window a split flush.

### 6.4 CRASH (model level)

`dashd-chainstate/tests/power_loss_chainstate.rs` implements ST3a's `Sut` over the production `FlushDriver`, `Recovery`, `SubscriberDriver` and `CoinsCache`, with the production schema and `RefExtractor`; it runs S1–S14b, N1–N9, FA1–FA6, W1–W5 and the re-planted T1–T41 (§4.7). `coins_cache_model` (node-fuzz) adds interleavings of spend, create, freeze, sub-batch completion, prefetch completion, eviction, reorg and crash kinds, checking `check_coins_profile` after reopen. The native RocksDB regression (`rocks_wal_nonprefix.rs`) stays in `dashd-store`.

### 6.5 Fuzz targets

| Target | What it fuzzes | Sink |
|---|---|---|
| `coins_cache_model` | operation interleavings vs the reference map; the budget invariant; prefetch fencing | node-fuzz runner (CS0/O3) |
| `flush_protocol_sweep` | seeded ST3a `sweep` over the production `Sut`: random flush shapes, subscribers, faults | same; 16 CPU-h on v24, 8 on v23.1 at merge (DEC-59 Q6 pattern) |
| `packed_coin_roundtrip` | arbitrary `TxOut` → `coin_bytes` → `CoinRef` → re-encode; inline/heap boundary | CS5a's corpus shared |
| `recovery_replay` | arbitrary marker transitions over synthetic blocks and undo, interrupted at every batch; idempotence | same |

### 6.6 Functional tests to port (with their owners)

`feature_dbcrash` (CH2 + CH5), `feature_reindex`/`feature_loadblock` (CH5), `feature_pruning`/`feature_index_prune`/`feature_fastprune` (CH8), `rpc_dumptxoutset` and `feature_utxo_set_hash` (CS5b/RP1a, exercising the forced flush + snapshot iteration), the F01 restart case (CH3b).

---

## 7. Implementation plan

| Phase | Content | Acceptance |
|---|---|---|
| **A** entries | `OutPointKey`, `PackedCoin`, `Entry`, `SaltedFxBuild`/`Salt`, the `VarintU32` key codec, `CoinBytes` adapters to CS5a (or a provisional local encoder/decoder behind the same signatures until CS5a merges, deleted at its merge) | layout tests; `packed_coin_roundtrip` fuzz; microbenches |
| **B** cache | `ActiveLayer`, `CoinsCache` single-layer (active + DB) with `fetch`/`add_coin`/`spend_coin`/`uncache`/`access_by_txid`, `BlockOverlay::commit`, `sanity_check`, `CoinsBudget` arithmetic + the counting-allocator assertion | the §6.1 state tables; the two Core simulation tests; `coins_cache_model` v0 (no crash) |
| **C** one-batch protocol | `CoinsTable`, `MetaTable`, `FlushPolicy::decide` (trigger table), `FlushDriver` with the one-batch path and the index-only write, `Recovery`, the `Marks` seam, the production `RefExtractor`, the `Sut` | S1, S3, S7 (with synthetic `Connected` marks), S8, S9 pass; T1, T2, T7, T8, T12, T14, T17, T20, T21, T31 caught |
| **D** split flush | sub-batches, pre-Bk barrier, Durable Bk, `-dbcrashratio`, Core's log lines, `batch_with_capacity`, `flush_memtables` after Bk | S4, S4b, S12 pass; T3, T4, T11, T18, T24, T26, T27, T34 caught; `rocks_wal_nonprefix` cases 3–4 reproduced with the production driver; `feature_dbcrash` port passes once CH5's reindex is available |
| **E** freeze-and-flush | `FrozenLayer`, `freeze`/`release`/`wait_released`, W as a thread with `FlushPlan` hand-off, `PrefetchMap` + fence, the full invariant with in-flight terms, `resize` | `coins_cache_model` with freeze/release/prefetch/reorg interleavings; the budget never exceeded under a counting allocator for every `f`; V-ahead scenarios (S5c shape) pass |
| **F** subscribers and prune hooks | `SubscriberDriver`, `SubscriberData`, `FlushPoint`/`LocatorAck`, `delete_range` (BL-38), the prune decision and selection hooks, the moveback emulation | S2, S3p, S5, S5b, S5c, S11, S11r, S11s, S11q, S11d, S11c, S13, S14, S14b, N9 pass; T5, T6, T9, T10, T13, T15, T16, T19, T22, T28, T29, T30, T32, T33, T35–T41 caught; FA1–FA6 hold; `prune_selection_protocol` passes |
| **G** performance | `DbCacheSplit`, `RocksConfig::coins_profile`, `-dbdirectio`, PF0 workloads and microbenches, the W-X bake-off trace and runs, the DEC-43 cell in a reserved window | PF0: window counters not worse than the previous head; `reindex_time_s` and `peak_rss_mib` not worse in the window; **the DEC-43 cell not slower than dashd**; W-X results recorded in `designs/ST1-bakeoff.md` §9 addendum |

Phases A–D need only ST1, ST2 (for the files barrier) and CS0; E needs CH1's index for LCA and dirty-record takes (a stub index suffices for the model); F needs ST3a merged; G needs node-bench B1b/B1c for the gated cell. Phases A–C can start on design approval.

---

## 8. Open questions for the manager

| # | Question | Recommendation |
|---|---|---|
| Q1 | **Mempool allowance in the budget.** A1 §3.4 bounds the coins share B alone; Core's trigger uses `B + max(maxmempool − mempool_usage, 0)`. Which is `T`? | Core's (`T` includes the allowance). It keeps flush cadence and peak memory equal to dashd's at equal flags, which the gating comparisons pin, and the DEC-43 cell measures the no-allowance regime on both sides. A1 §3.4 gets an erratum line. |
| Q2 | **CS5a API.** CH2 needs from `dash-consensus::coins`: (a) `coin_bytes(height, coinbase, &TxOut, &mut Vec<u8>)` or a `CoreWrite`-based encoder; (b) a **borrowed** decoder `CoinRef<'a>::decode(&'a [u8]) -> Result<CoinRef<'a>>` with `height`, `coinbase`, `value`, `script: &'a [u8]` or a decompressed-script view that avoids allocation for the special forms; (c) `CoinsView` with `fn coin(&self, op) -> Option<CoinRef<'_>>` or an owned `Coin` (the trait must be implementable over a layered cache that returns borrowed bytes); (d) the undo-record decoder yielding, per input, the coin bytes and its height code (zero-height detection); (e) `is_unspendable`. | Ask the CS5a owner to expose (a)–(e) now; CH2 Phase A carries a provisional local copy of `dash-coreimport/src/coin.rs`'s encoder behind the same signatures until CS5a merges, so neither task blocks the other. |
| Q3 | **Memtables as an in-flight budget term, and `flush_memtables` after every Bk.** This is what returns ~32 MiB to the coins share at `-dbcache=300`. Cost: one memtable flush per full flush (ST1 D2: 0.16–0.82 s, on W, asynchronous to V). | Approve. It is also a WAL-replay bound at startup (ST1: `flush_memtables` "bounds WAL replay"). |
| Q4 | **Block cache rule and `-dbdirectio`.** `clamp(dbcache/16, 16 MiB, 64 MiB)` buffered by default; `-dbdirectio=1` gives `O_DIRECT` with the engine share as the block cache. | Approve as the Phase G starting point; the DEC-43 cell and W-MF (with `E` scaled) decide the final numbers before CH2 merges; the knob stays. |
| Q5 | **CH2 implements the prune hook points** (dry run, `D ∩ eligible`, re-arm, moveback log) and CH8 wires configuration, the RPC and the FTs. | Approve; the CRASH model needs the logic inside the driver. CH8's brief gets a line. |
| Q6 | **`SubscriberDriver` as the single implementation** of ST3a's subscriber contract for `llmq/`, every index and the wallets. `D_max = 1,024` blocks default, per-subscriber override; cadence 30 s. | Approve; IX1/IX2/LK1b/WL briefs reference it and implement only `SubscriberData`. |
| Q7 | **An `oracle_coins_cache_step` entry point** (a `CCoinsViewCache` over an in-memory view, O1b-style) to make `coins_cache_model` differential against Core's flags and `DynamicMemoryUsage`. | Nice to have, not blocking: the state tables and simulation tests are ported from Core; add it to the O1b backlog. |
| Q8 | **`ChainStateFlushed` and the step-6 drain run on the events thread.** W posts `FlushPoint { new }` into the ordered `ChainEvent` bus and waits for the synced mandatory subscribers' `LocatorAck`s; the subscribers run their drains on the events thread (synced) or their own threads (syncing). | Approve; it is Core's queue order (`index/base.cpp:290-327 / :332-369`) and keeps every store call off worker pools (ST3a §5.6 item 3). |
| Q9 | **The `Failed to commit EvoDB` string** is unreachable under B1 but kept for a B1 failure caused by an evo-CF operation. | Keep (operator-visible parity is cheap); note in RL2's docs. |
| Q10 | **Planted bugs as feature-gated mutations of the production driver** (`feature = "planted-bugs"`, test builds only), so T1–T41 are caught against real code. | Approve; the `cargo tree` guard of ST3a extends to this feature. |
| Q11 | **Entry density claim.** §2.2's "10–20% denser than Core's node" is an estimate; PF1 measures both sides before any number appears in a report. | Record as a hypothesis in A1 §3.4's density paragraph; no change to the design either way. |

---

## 9. Summary

CH2 ports `CCoinsViewCache` entry for entry onto three layers (active, frozen, prefetch) over the `coins` column family, stores Core's own `Coin` bytes so a flush is a memcpy, bounds everything the cache holds at once by Core's `nTotalSpace` with the in-flight batch and memtable counted, and implements ST3a §5.6's durability protocol once, as a generic synchronous state machine that production runs on the db-writer thread and the power-loss model drives on one thread. Recovery is coins-only, blind and idempotent; subscribers share one driver that keeps their locators behind the chainstate's durable tip. Four marked deviations from Core's order are the ones that close Core's own crash holes, and none is visible on a healthy node. The DEC-43 low-memory cell against dashd is the merge gate, and the memory split is designed for it.
