# SB4 — `dash-chain` core (CH1's shared half): design

| | |
|---|---|
| Row | PLAN §4 SB4 (`PLAN.md:747`); SB0 §13.1 (`SB0-shared-base.md:1570`) |
| Status | **rev7.1, for manager verification and approval** (Sol r7 on rev6+rev7: APPROVE-WITH-CHANGES, gate yes; rev7.1 folds in its R7-m1 and DEC-147). rev0 8ff4b56b → Sol r1 → rev1 5f03a841 (§20) → Sol r2 → rev2 26e8b115 (§21) → Sol r3 APPROVE-WITH-CHANGES (24098412) → Opus high APPROVE-WITH-CHANGES (`reviews/SB4-design-opus-high.md`) → rev3 36266b40 (§22) → Sol r4 REWORK (`reviews/SB4-design-r4-sol.md`) → rev4 e3d2d9ad (§23) → Sol r5 REWORK (`reviews/SB4-design-r5-sol.md`) → rev5 45d9e640 (§24) → Sol r6 REWORK (`reviews/SB4-design-r6-sol.md`) → rev6 d6e461fc (§25) → `sb4-core-scenarios` vectors (`node/sb4-core-scenarios` @ 42737ddf) → rev7 6a625933 (§26) → harness Sol r1 (`reviews/SB4-core-scenarios-r1-sol.md`) and Sol r7 APPROVE-WITH-CHANGES, gate yes (`reviews/SB4-design-r7-sol.md`) → rev7.1 (§27). DEC-123, DEC-127, DEC-128, DEC-132, DEC-136, DEC-140, DEC-144, DEC-145, DEC-146 and DEC-147 applied |
| Date | 2026-10-09 |
| Tier | D O-h; impl O-h; rev Sol → O-h. Slack 1.5 weeks against M2 (PLAN §8.3 row 3) |
| Depends | CS0 (merged); `i:CS2` (merged), `i:CS3` (`node/cs3-checks` @ 7c333502, in review), `i:BS5` (merged) |
| Feeds | SB5 (feeds, residency), CH1-node (V's `ChainCommand`), CH2b (`IndexWrite`, `IndexAccess`), SB6/P2a-node (`ChainAccess` read side), SB11 (`AnchoredAncestry`), SB12/SB14/SB15 (SPV driver) |
| Inputs | SB0 rev6.1 §4, §4.1–§4.4, §4.6–§4.7, §6, §9, §13 (DEC-112/115/116/119); CH2 (DEC-103) §2.6, §2.7, §2.9.1, §4.3, §10.1; A1 §2.9, §2.10; P2a §2.5.3 (R1–R8) and `ChainAccess` (`P2a-outbound.md:284-312`, `:439-454`); CS3 `dash-consensus::{header, ctx}`; CS2 `dash-consensus::pow`; BS5 `dash-params` |
| Core pin | "v24" means **v24.0.0-rc.3** (`git show v24.0.0-rc.3:<path>` in `~/workspace/dash`). Every line cite below is at that tag |

## 0. Summary

`dash-chain` is one header index for both products: the node runs it with `window = None` from genesis, and SPV runs it anchored and windowed. SB4 builds the core:
- an append-only chunked arena with immutable entry fields, a seqlocked status/data group and a concurrent hash index (R1/R2);
- `accept`, a line-by-line port of `AcceptBlockHeader`/`ProcessNewBlockHeaders` that calls CS3's rule functions through a rule-sized `RuleContext`;
- `best_header` with exactly Core's update sites, and SPV's `fork_target` computed over a leaf set;
- the ChainLock floor, the conflict check at acceptance and `EnforceBlock`/`MarkConflictingBlock`, run against an explicit enforcement view (`TipHost`: V's real chain in the node, a virtual cursor in SPV), and Core's SPORK_19 lifecycle including the off → on reset (§7.5);
- the tip writers, the `TipCell` core (tip lock, `seq`, published snapshot) and `ChainReadSnapshot` (R3/R8, `tip_seq`), publishing per step in the node and once per committed header step in SPV (§9.1);
- `IndexAccess`, the locator, `chain_tips`, the IBD latch, the `BlockIndexRecord` codec, `take_dirty` and `load`;
- the anchor pack, the candidate and fallback functions, and `CheckpointView`.

SB5 adds the feeds, `open`/reopen, the cold LRU and the §4.4 property suite on top.

## 1. Scope

### 1.1 Split between SB4, SB5 and CH1-node

| Item | SB4 | SB5 | CH1-node |
|---|---|---|---|
| Arena, entries, seqlock, hash index | all | — | writes the block-data group through SB4's API |
| `Residency::Permanent` (node) | all | — | — |
| `Residency::Evicting` (SPV window) | eviction after commit, chunk release, cold resolution of ids and hashes through `HeaderStore`, skip-id ancestry over records, the cold-status overlay and the store-backed children lookup (§3.7; DEC-123 Q1) | `open`/reopen, the cold LRU, snapshot `at_height` below its chunks (SB0 §4.4), the windowed ≡ unwindowed property suite | — |
| `accept` with every Core header rule | all | — | issues `ProcessHeaders` on V |
| `best_header` rules | acceptance and load sites; the walk-back and invalid-chain hooks | — | calls the hooks at `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound` |
| SPV `fork_target` | all (leaf set, eligibility walk) | recomputed at `open` from `HeaderStore::leaves`; the checkpoint cache rebuilt at `open` (§11.4, O-m7) | never |
| ChainLock | floor, late-header floor update, conflict at acceptance, `enforce_block` + `mark_conflicting_block` index logic, the SPORK_19 state transition and reset (§7.5) | restores `cl_best`, `cl_floor` and the observed spork state at `open` | the `ChainCommand`, `DisconnectTip`, candidates, `m_best_invalid`; calling `check_cl_active_state` and `enforce_best_chainlock` at the handler's sites |
| SPORK_19 state | the `SporkView` seam and its use (§7.5) | — | the node's source is SP0's spork manager; SPV's is the shared spork core (D29) |
| Tip writers, `TipCell` | tip lock, `next_seq`, snapshot publish (`TipCell` core) | feeds, pending, `register`/`subscribe_at`/`deregister`, wakes, `StatusEvent` feed | the only caller in the node (V) |
| `ChainReadSnapshot`, R1–R8 | all | `at_height` below the chunks via ancestry | `ChainView` (+ `mn_list`, `best_chainlock`, `ibd`, `mtp`, `bls`, `epoch`) on top |
| `IndexAccess` | all | cold variant hardening | — |
| `BlockIndexRecord` codec, `take_dirty`, `load` | all | — | puts the writes in B1 (P10); calls `load` in recovery step 4 |
| Anchor pack, candidate, fallback, `CheckpointView` | all (functions, pack checks) | — | — (SB15 runs the initial-sync driver) |
| `HeaderStore` trait + `MemHeaderStore` test double | all | — | — (SB13 writes the SQLite one) |

### 1.2 Not in SB4

The feeds (F1, F3–F6, F10, F11), `subscribe_at`, the SQLite store, the MN pipeline and coverage (SB11/SB15), block data and `nSequenceId` assignment (`validation.cpp:4288`), the candidate set and `ActivateBestChain` (CH1-node), `AdjustedTime` (SB6), spork message intake, verification and storage (D29), and every network concern. The `sb4-core-scenarios` oracle harness is a separate parallel task (DEC-123 Q3; §15.1).

## 2. Crate placement

- **`dash-chain`** (new, class S, SB0 §2.3), `#![forbid(unsafe_code)]`. Dependencies:
  - `dash-consensus` (CS0/CS2/CS3): rule functions, `BlockId`, `HeaderCtx`, `ChainAncestry`, `DeploymentView`, `CheckpointView`, `ConsensusCtx`, `BlsCtx`, `UngatedRules`, `BlockValidationError`.
  - `dash-params` (BS5): `ChainParams`, checkpoints, buried heights.
  - `dash-primitives`, `dash-num` (`Arith256`, `CompactTarget`).
  - `dash-clock` (D1).
  - `hashbrown`, `parking_lot`, `arc-swap`, `rayon` (for the injected `&ThreadPool` only), `thiserror`, `tracing`.
- **L5** (SB0 §2.3) holds by construction: no threads, no pools built, no statics with network parameters, no `abort`/`exit`. `load` runs its parallel decode in the host's `&rayon::ThreadPool` through `pool.install`.
- **L6** (mobile): every dependency above is already in the SPV graph or is pure Rust.
- **`dash-clock`** (new, class S, D1): `pub trait Clock: Send + Sync { fn node_now(&self) -> NodeSeconds; fn steady(&self) -> Instant; }`, `SystemClock`, and `MockClock` (`set_mocktime`). SB6 adds the peer-offset `AdjustedTime` service.
- **`dash-consensus` addition** (D2): `pow::block_proof(bits: CompactTarget) -> Arith256`, a port of `GetBlockProof` (`chain.cpp:132-145`), with vectors from `dash-oracle-sys::block_proof`.

## 3. Data model

### 3.1 Ids and work

- `BlockId(u32)` is `dash_consensus::BlockId` (`ctx.rs:14`). In the node it is the arena index for the process lifetime and is never persisted (SB0 §4.1.1). In SPV it is stable and persisted (SB0 §4.4).
- `Work = dash_num::Arith256`. `chain_work(e) = chain_work(prev) + block_proof(bits)` (`blockstorage.cpp:110`).

### 3.2 Status

`Status(u32)` has Core's `nStatus` layout bit for bit (`chain.h:81-135`):

| Name | Value | Note |
|---|---|---|
| `VALID_TREE` .. `VALID_SCRIPTS` | 2 .. 5 | validity levels; `VALID_MASK` = 7 |
| `HAVE_DATA`, `HAVE_UNDO` | 8, 16 | written by CH1-node only |
| `FAILED_VALID`, `FAILED_CHILD` | 32, 64 | `FAILED_MASK` |
| `CONFLICT_CHAINLOCK` | 128 | `chain.h:118` |
| `ASSUMED_VALID` | 256 | reserved (A1 §2.9), never set |

`Status::is_valid(up_to)` is Core's `IsValid` (`chain.h:300-308`): false when any FAILED bit is set, otherwise `(s & VALID_MASK) >= up_to`. `CONFLICT_CHAINLOCK` does not affect it. SPV's `CONTEXT` is a flag of its record (`HeaderRecord::kind`), never a status bit (SB0 §4.1.1, n1).

### 3.3 Entry

```rust
pub(crate) struct Entry {
    imm: OnceLock<EntryImm>,   // written once by the writer before the id is published (R2)
    seq: AtomicU32,            // seqlock over the mutable group (even = stable)
    status: AtomicU32,         // Status
    n_tx: AtomicU32,           // Core nTx; CH1-node
    file: AtomicI32,           // Core nFile; -1 = none
    data_pos: AtomicU32,       // Core nDataPos
    undo_pos: AtomicU32,       // Core nUndoPos
}
pub(crate) struct EntryImm {
    hash: BlockHash,
    header: Header,            // 80 B: version, prev hash, merkle root, time, bits, nonce
    prev: Option<BlockId>,
    skip: Option<BlockId>,     // Core pskip (BuildSkip, chain.cpp:126-130)
    height: u32,
    chain_work: Work,          // absolute (SB0 §4.3)
    mtp: i64,                  // GetMedianTimePast at insert; MTP_UNKNOWN for the lowest ten pack entries (§8.3)
    time_max: u32,             // Core nTimeMax (blockstorage.cpp:109)
    context: bool,             // SPV anchor-pack context entry; always false in the node
}
```

- About 200 B per entry including the `OnceLock` state and chunk overhead (estimate; PF1/SB17 measure). Core's `CBlockIndex` plus its map node is about 200 B too.
- **Immutable after publish** (R2): `hash`, `header` (so `time`, `bits`, `version`), `prev`, `skip`, `height`, `chain_work`, `mtp`, `time_max`. `ChainAccess::locator`/`ancestor` and the rule functions read only these, lock-free.
- **Mutable group**: `status`, `n_tx`, `file`, `data_pos`, `undo_pos`, under the per-entry seqlock (§13.2). Only the single writer writes them.
- `mtp` and `time_max` are stored, not recomputed: Core recomputes MTP on demand, and storing it costs 8 B to make every MTP read O(1). They are pure functions of the ancestry, so this changes no answer.

### 3.4 Arena

- **Directory**: two levels over `u32` ids. L1 has 4,096 slots, L2 has 1,024 slots, and each chunk holds 1,024 entries (4,096 × 1,024 × 1,024 = 2³²).
- **Residency** is a type parameter:
  - `Permanent` (node): `OnceLock<Box<Chunk>>` slots; a chunk is never released. Readers borrow `&Entry` for `&self`'s lifetime.
  - `Evicting` (SPV): `ArcSwapOption<Chunk>` slots; readers hold a guard. A chunk is released when all of its entries are evicted (SB0 §4.4). An evicted id resolves through `HeaderStore::by_id`.
- Ids are assigned densely by the writer (`next_id`). In SPV `next_id` starts at `meta.next_id` (SB5).
- The cold branch in `Permanent` is a never-taken `if` (SB0 §4.4).

### 3.5 Hash index

`HashIndex = parking_lot::RwLock<hashbrown::HashTable<BlockId>>`, hashing through the arena (A1 §2.10). It is append-only:
- the writer takes the write lock once per insert, and readers take the read lock per lookup (R1);
- it is presized at `load` to the record count, and grows by doubling otherwise;
- in `Evicting`, an evicted entry's hash leaves the resident index, and lookups fall through to `HeaderStore::by_hash` (SB0 §4.4).

An `RwLock` per insert is the R1 option P2a lists. A lock-free table is not needed at 2–3 inserts per lookup burst; PF0 `chain-index-lookup` checks this.

### 3.6 `ActiveChain`

- A persistent radix vector of `BlockId` by height: 1,024-id leaves (4 KiB), 64-way inner nodes, depth growing as needed. Three levels cover 4.19 × 10⁶ heights.
- **Push and pop** copy one path with `Arc::make_mut`. `TipState.snapshot` and the `ArcSwap` both hold the published root, so the path is always shared and every push or pop copies it: about 5 KiB per step (n4). A retained snapshot keeps its own root.
- **Windowed** (SPV): leaves below `tip − w` are dropped from new roots, so a retained snapshot pins at most ⌈w/1,024⌉ + 1 leaves (SB0 §4.4).
- `ActiveChainView` is a cloned root plus `base` (lowest resident height) and `tip`. It provides `height()`, `tip()`, `at(h)`, `contains(id)`, `next(id)`, `find_fork(id)` (`chain.cpp:61-70`) and `fork_in_global_index(locator, lookup: &dyn IndexAccess)` (`validation.cpp:182-197`: each locator hash through `LookupBlockIndex`, then Core's `Genesis()` fallback; SPV's fallback is the anchor, §10.1) (n5).
- `ActiveChainView::at` answers resident heights only. The writer's ChainLock enforcement never uses it for membership; it uses the `TipHost` view, which resolves below-window heights by ancestry (§7.3, Sol r1 M2).
- In SPV the infallible readers (`contains`, `fork_in_global_index`, `IndexAccess::*`) answer only for resident entries: an evicted id reads as unknown (D-C4-12). `try_fork_in_global_index` is the fallible form that reads cold entries, and an entry below the view's base counts when it is the tip's ancestor at that height (D-R2-A-4). A wallet's rewind below the window (SB0 erratum E3) must call it, because at w < 60 the infallible form would fall back to genesis.

### 3.7 Children, leaves and cold statuses (Sol r1 M4)

Core keeps `m_prev_block_index`, a `prev hash → child` multimap of the whole index (`blockstorage.cpp:123-126`, rebuilt at load `:282-287`). Its only readers in SB4's scope are `EnforceBlock`'s sibling scan (`validation.cpp:4059-4064`), `ResetBlockFailureFlags` (`:4204-4215`) and `CheckBlockIndex`. The two residencies store it differently:

| | `Permanent` (node) | `Evicting` (SPV) |
|---|---|---|
| Children | `links: Vec<Links { first_child: u32, next_sibling: u32 }>` indexed by id, writer-only, 8 B per entry; `children(p)` walks the sibling list | **no in-memory map**. `children(p)` = `HeaderStore::children(&hash(p))` (committed records, answered from the `prev` index SB13 adds per DEC-123 Q7) ∪ the step's uncommitted new entries whose prev is `p` (`pending_new`, at most one step's headers). Called only from `EnforceBlock`'s walk, once per off-chain block on the locked path |
| Leaves (§6.2) | `leaves: BTreeSet<(Work, Reverse<BlockId>)>`, every leaf | resident leaves only, plus `max_cold_leaf_work`: the largest work of any leaf that left the window, refreshed to the exact value whenever `HeaderStore::leaves()` is read. A leaf is moved out when its entry is evicted |
| Status changes in a step | written to the entry under its seqlock, at Core's sites | **every** status change after insertion, resident or evicted, goes to `overlay: HashMap<BlockId, Mark { status, applied }>`, writer-owned and status-only (below; O-m8, D-A13-1) |

**Status overlay (SPV; Sol r1 M4, Opus O-m8).** `EnforceBlock` can mark an entry that has left the window: in Sol r1's cold A-21 variant (w = 10), B81 and C81 are evicted when the tip is B110. It also marks resident entries that are still on the published active chain (B82…B110), and readers must not see those marks before the step is committed and published, because Core's readers see statuses and tip under one `cs_main`.
- **Write.** Every status change of an SPV step to an already-inserted entry is a mark in the overlay: the new status only, with `applied` false (a new entry's initial status is set at insertion, §5.4, as before). No record is copied or loaded, so an evicted id is marked without a cold read (D-A13-1, SB5-A13). The overlay's keys are SPV's dirty set for status changes. Nothing touches the seqlocked status or the database before §9.1's stages 3 and 4.
- **Read precedence.** The writer (eligibility walk, acceptance steps 1/4/5, `VirtualTip`): overlay, then resident entry, then the cold LRU (SB5), then `HeaderStore`. Readers (`IndexAccess::status`, `ChainReader::entry`): resident entry, then the LRU, then the store; they never see the overlay.
- **Capture.** The step's WriteSet takes the overlay's marks as status rows (`overlay_marks`, SB5 F6), beside the new entries' records from the accept results (D-A13-1).
- **Apply.** In stage 4, inside the tip-lock critical section and before the snapshot store, overlay entries for resident ids are written to their seqlocked groups, and the cold LRU drops the evicted ids. A reader can see the new statuses a few instructions before the new snapshot, never before the commit. A cold status read between stages 3 and 4 can reach the committed store row one step early; no SPV reader combines a cold status with a snapshot (D36).
- **Commit.** An overlay entry is `Mark { status, applied }`; stage 4 sets `applied` when it writes the status to the resident group or drops the id from the LRU (D-R2-C-1). `records_committed()` (stage 5) drains the dirty set and removes the applied marks. It **keeps** a mark that no stage 4 applied, and the next stage 4 applies it with that step's snapshot: applying it in `records_committed` would show readers a status with no snapshot (D36). A failed commit trips the fail-stop latch and the client stops (P6), so there is no discard path; the overlay dies with the process, and readers never saw its contents.
- **Size.** At most the statuses changed in one step: the marked siblings and the blocks a `MarkConflictingBlock` disconnects.

### 3.8 Memory bounds (Sol r1 M4)

Estimates at ~200 B per arena entry (§3.3; PF1/SB17 measure). B is the number of headers a header step accepts before it commits. SB14's driver commits after each `headers` message, so B ≤ 2,000.

**SPV, mainnet, w = 10,000.** All figures in this section are **estimates** from the per-entry size above, not measurements. They are the same for a 2.55-million-header history synced from genesis and for an anchored client: nothing grows with chain length.

| Structure | Bound | Mainnet figure |
|---|---|---|
| Arena | entries with height ≥ tip − w, plus ≤ B not yet evicted; chunk granularity 1,024 | ≤ 13 chunks × 1,024 × 200 B ≈ 2.7 MB |
| Hash index | resident entries; hashbrown reclaims tombstones on in-place rehash, so capacity stays ≤ 2 × live | ≤ 32,768 slots × 5 B ≈ 160 KB |
| Children | `pending_new` only | ≤ B × 4 B = 8 KB |
| Leaves | resident leaves + one `Work` | one per resident branch; typical < 1 KB, worst (w + B) × 50 B ≈ 600 KB |
| Overlay | one step's cold status changes; cleared at commit | d × 200 B for a lock that disconnects d cold blocks |
| `RuleContext` cache | R ids; owned cold `HeaderCtx` copies only for ids below the window | 25 × 4 B in the DGW era; 16 KB of ids + ≤ 400 KB of copies in a KGW-era context at w < 4,033 |
| `ActiveChain` | ⌈(w + B)/1,024⌉ + 1 leaves | ≈ 56 KB, plus what retained snapshots pin (≤ 56 KB each) |
| `failed_blocks` | always empty in SPV (§7.4) | 0 |
| Cold LRU (SB5) | 1,024 records | ≈ 200 KB |
| **Total** | | **≈ 3.5 MB (estimate)**, flat; A-31 measures it |

Test (A-31): accept H-M from genesis at w = 10,000 under a counting allocator in the test binary (L5 forbids a global allocator only in the crate). Live bytes after the last step stay within 1.25 × the bound above, with no upward trend over the last 2 million headers.

**Erratum (DEC-174, measured).** A-31 measures the SPV total at **3,037,456 B** after H-M's last step, with a peak of 3,288,584 B over the last 2 million headers and a fitted growth of 20,292 B over them: inside the 4.375 MB gate and below the 3.5 MB estimate. Three things the table assumed had to be made true first. Only the first is DEC-174's; with it alone, A-31 measured 3,995,664 B (the ~4.0 MB DEC-174 accepted) and 781,932 B of growth, which the other two removed:
- **`ActiveChain` is windowed by the writer** (DEC-174 (1)). After every committed step, right after eviction, stage 4 raises its base to `max(floor, tip − w)`, the height eviction keeps, and drops the whole leaves below the base's leaf (up to 1,023 heights below the base stay). SB5's segmented activation calls the same function per released segment. A disconnect below the base re-extends the chain one height at a time, from the plan's legs read by ancestry, before it pops (DEC-174 (2)). The base only rises at stage 4, so after a disconnect below the window it stays at the fork, above `tip − w`, until the tip is `w` past it; the heights between were evicted while the old tip stood, and readers resolve them by ancestry as before. `FindForkInGlobalIndex`'s fallback stays the floor's id, which the chain keeps after its leaf leaves (§10.1). Unwindowed, the chain held 10.3 MB at H-M's height.
- **The dirty set's bits span the ids marked since the last `take`**, not the chain. A bit per id up to the largest held 262 KB (524 KB allocated) at H-M's height, and grew by 4 B per 32 headers.
- **An `Evicting` arena's second-level table holds one pointer per chunk slot.** The slot enum was sized by `Permanent`'s inline chunk, 160 B, so each 2^20 ids left a 160 KB table behind. Now each leaves 8 KiB, never freed: two of them are 16 KB of the 20 KB fitted growth. The node's tables keep their chunks inline.

**Node, mainnet (2,551,766 entries).** This is Core-sized by design, because Core keeps every `CBlockIndex`:
- arena ≈ 510 MB;
- hash index presized to 4,194,304 slots ≈ 21 MB;
- `links` ≈ 20 MB;
- `ActiveChain` ≈ 10 MB.

Core's `BlockMap` node plus its `m_prev_block_index` node come to roughly 300 B per entry, about 765 MB (estimate). The node total of ≈ 560 MB is also an estimate. Neither bounds an arbitrary fork population; SB0's residency term (window plus stale entries in it) is the bound, and A-31 is the fork-free measurement control.

## 4. Public API

Signatures are normative for the implementation; names may change in review. `S: HeaderStore` is `NoStore` in the node.

```rust
// ---- configuration -------------------------------------------------------------------------
pub enum ChainMode { Node, Spv }
pub struct ChainConfig {
    pub mode: ChainMode,
    pub window: Option<u32>,                 // Node: None (enforced). SPV: Some(w), w >= L_est + H_keep (host check, SB0 §4.4)
    pub checkpoints_enabled: bool,           // Core -checkpoints (default true)
    pub consistency_checks: bool,            // Core -checkblockindex; default params.default_consistency_checks
    pub min_chain_work: Option<Work>,        // Core -minimumchainwork; None = params.consensus.minimum_chain_work
    pub max_tip_age: Duration,               // Core -maxtipage; DEFAULT_MAX_TIP_AGE = 6 h (kernel/chainstatemanager_opts.h:18)
    pub rules: &'static UngatedRules,        // V24_0 (TimeSource::NodeClock) or V23_1 (AdjustedTime, D8)
}
pub struct Services {
    pub clock: Arc<dyn Clock>,
    pub adjusted: Option<Arc<dyn AdjustedTime>>,   // SB6; required only by a V23_1 profile (D8)
    pub sporks: Arc<dyn SporkView>,               // SPORK_19 state (§7.5): SP0's manager (node), the shared spork core (SPV)
    pub fail: Arc<dyn FailStop>,                  // AbortSink::read_failed until dash-kv lands (D5)
}
pub trait SporkView: Send + Sync {
    /// Core `Chainlocks::IsEnabled()` = `IsSporkActive(SPORK_19)` (chainlock/chainlock.cpp:33; spork.cpp:163-196),
    /// evaluated live, against the source's adjusted time.
    fn chainlocks_enabled(&self) -> bool;
}

// ---- the writer ----------------------------------------------------------------------------
pub struct HeaderChain<S: HeaderStore = NoStore, R: Residency = Permanent> { /* SB0 §4.1 fields */ }

impl<S: HeaderStore, R: Residency> HeaderChain<S, R> {
    // construction
    pub fn new_genesis(params: Arc<ChainParams>, cfg: ChainConfig, svc: Services, store: S)
        -> Result<Self, ChainError>;                                   // the index part of Core LoadGenesisBlock
    pub fn new_anchored(params: Arc<ChainParams>, cfg: ChainConfig, svc: Services, store: S,
                        pack: &AnchorPack) -> Result<Self, ChainError>; // SPV Candidate start (§11)
    pub fn load<I>(params: Arc<ChainParams>, cfg: ChainConfig, svc: Services,
                   records: I, pool: &rayon::ThreadPool) -> Result<Self, LoadError>
        where I: Iterator<Item = Result<(BlockHash, Vec<u8>), LoadError>>;  // node; §10.3
    // SPV reopen (`open`) is SB5's.

    // acceptance (Core ProcessNewBlockHeaders)
    pub fn accept(&mut self, batch: HeaderBatch, from: Option<PeerId>) -> Result<HeadersResult, ChainError>;
    // Core AcceptBlockHeader alone (CH1-node's AcceptBlock, validation.cpp:4598-4727): no per-header
    // CheckBlockIndex and no publication; the caller publishes (O-m9)
    pub fn accept_header(&mut self, header: &Header, hash: &BlockHash) -> Result<AcceptOne, ChainError>;
    pub fn publish_status(&mut self);                                  // node: epoch + 1, same tip_seq (§9.1)

    // ChainLocks (Core UpdateBestChainlock + EnforceBlock/MarkConflictingBlock, index side)
    pub fn apply_chainlock<H: TipHost<S, R>>(&mut self, cl: ChainLockRef, host: &mut H)
        -> Result<ClOutcome, ApplyError<H::Error>>;
    pub fn enforce_best_chainlock<H: TipHost<S, R>>(&mut self, host: &mut H)
        -> Result<Option<EnforceReport>, ApplyError<H::Error>>;        // Core EnforceBestChainLock's EnforceBlock half;
                                                                       // None when the observed state is off (chainlock/handler.cpp:275-277)
    pub fn check_cl_active_state(&mut self) -> ClActiveState;          // Core CheckActiveState (chainlock/handler.cpp:209-222), §7.5
    pub fn cl_floor(&self) -> Option<BlockId>;
    pub fn cl_enabled_observed(&self) -> bool;                         // the handler's cached isEnabled

    // tip writers: one family, one private step (D4)
    pub fn init_active_tip(&mut self, tip: BlockId) -> Result<(), ChainError>;  // Core LoadChainTip; seq stays 0
    pub fn connect_tip(&mut self, id: BlockId) -> Result<TipStep, ChainError>;  // node: Core ConnectTip boundary
    pub fn disconnect_tip(&mut self) -> Result<TipStep, ChainError>;            // node: Core DisconnectTip boundary
    pub fn tip_plan(&self, to: BlockId) -> Result<TipPlan, ChainError>;         // unbounded: node, tests (D47)
    pub fn set_active_tip(&mut self, to: BlockId) -> Result<TipChange, ChainError>; // composite. Node: publishes per step.
                                                                                     // SPV: stage 4 of §9.1's recipe: runs the
                                                                                     // pending plan, refuses a target that
                                                                                     // conflicts with the floor (DEC-136), publishes once
    pub fn virtual_tip(&self) -> VirtualTip;                                    // SPV: one per step (§9.1 stage 1, O-m4)

    // best header (Core m_best_header) and V's hooks
    pub fn best_header(&self) -> BlockId;                                // never null after construction (R5)
    pub fn best_header_walk_back(&mut self, marked: BlockId);            // validation.cpp:3951, :3969, :4089, :4107
    pub fn best_header_invalid_chain(&mut self, invalid: BlockId);       // validation.cpp:2043-2044
    pub fn fork_target(&self, vt: &VirtualTip) -> Result<ForkTarget, ChainError>; // SPV (§6.2); ChainError::Config in the node

    // status and block data (CH1-node)
    pub fn modify(&mut self, id: BlockId, f: impl FnOnce(&mut MutableGroup));   // seqlocked write; does NOT mark dirty
    pub fn mark_dirty(&mut self, id: BlockId);                                  // at Core's m_dirty_blockindex sites (D15)
    pub fn failed_blocks(&mut self) -> &mut FailedBlocks;                       // Core m_failed_blocks (ordered by id, D9)
    pub fn reset_failure_flags(&mut self, id: BlockId, ignore_chainlocks: bool) -> ResetReport; // index half of :4152-4220 (§7.4)

    // persistence
    pub fn take_dirty(&mut self) -> Vec<IndexWrite>;                     // node: CH2 B1 (P10)
    pub fn take_dirty_records(&mut self) -> Vec<HeaderRecord>;           // SPV: the header step's WriteSet, incl. the overlay (§3.7)
    pub fn cl_state(&self) -> ClPersisted;                               // SPV: CL rows + observed spork state for the WriteSet (§7.5)
    pub fn records_committed(&mut self);                                 // SPV: §9.1 stage 5: drop the applied marks (unapplied ones stay, D-R2-C-1), invalidate LRU

    // readers
    pub fn reader(&self) -> ChainReader;                                 // Clone + Send + Sync handle for other threads
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot>;
    pub fn complete(&self) -> Option<&dyn ChainAncestry>;                // Some only for Genesis + window None
    pub fn anchored(&self) -> &dyn AnchoredAncestry;                     // every configuration
    pub fn chain_tips(&self) -> Result<Vec<ChainTip>, ChainError>;       // getchaintips over the header index
    pub fn check_index(&self) -> Result<(), IndexInconsistency>;         // header subset of CheckBlockIndex
}

// ---- batches and results (P2a R7) ----------------------------------------------------------
pub struct HeaderBatch { pub headers: Vec<Header>, pub hashes: Vec<BlockHash> }  // hashes[i] = X11(headers[i])
pub struct HeadersResult {
    pub last_accepted: Option<BlockId>,       // Core pindexLast: the last header accepted or already known (:4742)
    pub stop: Option<HeaderStop>,             // why the batch stopped early, if it did
    pub floor_moved: Option<BlockId>,         // a late header became the CL floor (chainlock/chainlock.cpp:152-172): enforce next
    pub snap: Arc<ChainReadSnapshot>,         // published before the reply (R3)
}
pub enum AcceptOne { Accepted(BlockId), Known(BlockId), Invalid(HeaderReject), BelowAnchor, ContextSkipped }
pub enum HeaderStop {
    Invalid { index: usize, reject: HeaderReject },  // HeaderReject = dash-consensus BlockValidationError
    BelowAnchor { index: usize },                     // SPV: ignored, no blame (SB0 §4.1.1)
}

// ---- ChainLock -----------------------------------------------------------------------------
pub struct ChainLockRef { pub height: u32, pub hash: BlockHash }   // VerifiedChainLock (SB8) wraps it later (D11)
pub enum ClOutcome {
    Stale,                          // height <= best CL height (chainlock/chainlock.cpp:110-113)
    Pending,                        // header unknown: best CL recorded, floor unchanged (:129-131)
    HeightMismatch,                 // (:119-125)
    Floor { id: BlockId, enforced: Option<EnforceReport> },  // None while the observed state is off (§7.5)
}
pub struct EnforceReport { pub marked: Vec<BlockId>, pub disconnected: Vec<BlockId>, pub reset: Option<ResetReport> }
                                // `disconnected` is node-only: SPV leaves it empty and the step's list is `VirtualTip::disconnected` (D47)
pub struct ResetReport { pub reconsidered: Vec<BlockId>, pub promoted: Vec<BlockId> } // promoted: FAILED_CHILD -> +FAILED_VALID
pub struct ForkTarget { pub id: BlockId, pub floor: FloorRelation }
pub enum FloorRelation { NoFloor, Unenforced, Descends, Trails }  // Unenforced: observed off; Trails: id is a strict
                                                                   // ancestor of the floor (DEC-136, Core's trailing tip)
/// Port of `UpdatedBlockTip`'s guards (chainlock/handler.cpp:187-189): false when `new == fork` or `ibd`.
/// `fork: None` is Core's `nullptr` (InvalidateBlock, MarkConflictingBlock), which always passes the first guard.
pub fn updated_block_tip_schedules(new: BlockId, fork: Option<BlockId>, ibd: bool) -> bool;
pub enum ClActiveState { Unchanged, TurnedOff, TurnedOnReset }   // TurnedOnReset = Core ResetChainlock ran
pub struct ClPersisted { pub best: Option<ChainLockRef>, pub floor: Option<BlockId>, pub enabled_observed: bool }

/// The chain as Core's enforcement code sees it: `m_chain.Tip()`, `m_chain.Contains()` and `DisconnectTip`
/// (validation.cpp:4057, :4087, :4096-4110). Every read in §7.2/§7.3 goes through it (Sol r1 M2).
pub trait TipHost<S: HeaderStore, R: Residency> {
    type Error;
    fn tip(&self, chain: &HeaderChain<S, R>) -> BlockId;
    fn contains(&self, chain: &HeaderChain<S, R>, id: BlockId) -> Result<bool, ChainError>;
    fn disconnect_tip(&mut self, chain: &mut HeaderChain<S, R>) -> Result<(), Self::Error>;
    /// Called once at the end of every `mark_conflicting_block`, after its status writes (Core :4121) and
    /// before `enforce_block` moves to the next sibling (O-M2, DEC-136). The node does Core's :4122-4148 here,
    /// in Core's order (§7.3); `self.tip(chain)` is the tip Core passes to `UpdatedBlockTip(tip, nullptr)`.
    fn conflicting_marked(&mut self, chain: &mut HeaderChain<S, R>, marked: BlockId, was_in_chain: bool)
        -> Result<(), Self::Error>;
}
/// SPV: one per step (O-m4). A cursor starting at the active tip. `contains(x)` = height(x) <= height(cursor)
/// and ancestor(cursor, height(x)) == x, over arena, overlay and cold records; `disconnect_tip` sets
/// cursor = prev(cursor) and publishes nothing; `conflicting_marked` records the cursor as the tip of one
/// `UpdatedBlockTip(tip, nullptr)` notification. The real tip moves at stage 4 of §9.1's recipe (§7.3).
pub struct VirtualTip { cursor: BlockId, disconnected: Vec<BlockId>, notified_tips: Vec<BlockId> }
impl VirtualTip { pub fn cursor(&self) -> BlockId; pub fn notified_tips(&self) -> &[BlockId]; }
// Node: V implements TipHost. `tip`/`contains` read the writer's ActiveChain (complete in the node);
// `disconnect_tip` is V's DisconnectTip, which ends in `chain.disconnect_tip()`: inside `enforce_block`
// that step is staged under the tip mutex `enforce_block` holds throughout, and published once when it
// returns (§9.1, O-m3, R4-M2). Each disconnect's event goes to the feeds at its step (D43). Host
// callbacks, CH2's IF_NEEDED check included, run with that mutex held (DEC-144; contract §13.4, D42).

// ---- tip changes ---------------------------------------------------------------------------
pub enum TipStep { Connected { block: BlockRef, seq: u64 }, Disconnected { block: BlockRef, seq: u64 } }
pub struct TipChange { pub disconnected: Vec<BlockRef>, pub connected: Vec<BlockRef>, pub last_seq: u64,
                       pub snap: Arc<ChainReadSnapshot> }   // the snapshot published last (SPV: the step's only one)
pub struct TipPlan { pub fork: BlockRef, pub disconnect: Vec<BlockRef>, pub connect: Vec<BlockRef> }
pub struct BlockRef { pub id: BlockId, pub hash: BlockHash, pub height: u32 }   // CH2's BlockRef

// ---- readers -------------------------------------------------------------------------------
#[derive(Clone)] pub struct ChainReader { /* Arc<Shared> */ }
impl ChainReader {
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot>;                   // lock-free (ArcSwap)
    pub fn lookup(&self, h: &BlockHash) -> Option<BlockId>;             // R1
    pub fn entry(&self, id: BlockId) -> Option<EntryView>;              // immutables + one seqlocked status read (R2)
    pub fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;
    pub fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash>;       // Core GetLocator; None -> empty under V24_0 (P2a G2)
    pub fn is_initial_block_download(&self, snap: &ChainReadSnapshot, now: NodeSeconds, loading_blocks: bool) -> bool; // R4
    pub fn min_chain_work(&self) -> Work;
}
impl IndexAccess for ChainReader { /* CH2 §2.11 */ }
pub trait IndexAccess: Send + Sync {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId>;
    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId>;
    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId>;          // Core LastCommonAncestor (chain.cpp:166-181)
    fn height(&self, id: BlockId) -> Option<u32>;
    fn have_data_pos(&self, id: BlockId) -> Option<FlatFilePos>;       // None in SPV
    fn undo_pos(&self, id: BlockId) -> Option<FlatFilePos>;            // None in SPV
    fn status(&self, id: BlockId) -> Option<Status>;
}

pub struct ChainReadSnapshot {                // P2a R3/R8 + SB0 §4.1 + CH2 P7
    pub tip: TipFacts,
    pub tip_seq: u64,                          // F2: seq of the last tip step; 0 after init_active_tip
    pub active: ActiveChainView,
    pub bls: BlsCtx,                           // bls_ctx_for_block(params, tip_height + 1): BlsCtx::for_tip (P2a g2)
    pub cl_floor: Option<BlockId>,             // bestChainLockBlockIndex at publication
    pub cl_enabled_observed: bool,             // the handler's isEnabled at publication
    pub epoch: u64,                            // +1 on every publish
}
pub struct TipFacts { pub tip: BlockId, pub tip_work: Work, pub tip_time: u32, pub tip_height: u32,
                      pub tip_hash: BlockHash, pub best_header: BlockId, pub best_header_time: u32,
                      pub best_header_work: Work, pub best_header_height: u32 }

// ---- ancestry for anchored clients (SB0 §4.1.1) --------------------------------------------
pub trait AnchoredAncestry: Sync {
    fn floor(&self) -> u32;
    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError>;
    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError>;
    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError>;
}
pub enum AncestryError { BelowFloor { height: u32 }, Storage(StoreError) }

// ---- SPV cold tier seam (SB0 §4.1; implemented by SB13, MemHeaderStore in SB4) -------------
pub trait HeaderStore: Send + Sync {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError>;
    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError>;
    fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError>;
    fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError>;
    fn leaves(&self) -> Result<Vec<HeaderRecord>, StoreError>;   // DEC-123 Q7: records with no accepted child
    fn children(&self, prev: &BlockHash) -> Result<Vec<HeaderRecord>, StoreError>; // same `prev` index (§3.7)
}
pub struct HeaderRecord { pub id: BlockId, pub height: u32, pub header: Header, pub skip: Option<BlockId>,
                          pub status: Status, pub chain_work: Work, pub time_max: u32, pub kind: RecordKind }
                          // time_max: Core nTimeMax, needed when the record is a cold prev (§5.4, O-m5, D35)
pub enum RecordKind { Accepted, Context }

// ---- persistence ---------------------------------------------------------------------------
pub struct IndexWrite { pub hash: BlockHash, pub bytes: Vec<u8> }   // CH2 §2.7's type, owned here (D16)
pub struct BlockIndexRecord { pub height: u32, pub status: Status, pub n_tx: u32,
                              pub file: Option<i32>, pub data_pos: Option<u32>, pub undo_pos: Option<u32>,
                              pub header: Header }
impl BlockIndexRecord {
    pub fn encode(&self, out: &mut Vec<u8>);
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError>;
}

// ---- anchor and checkpoints ----------------------------------------------------------------
pub enum Anchor { Genesis, Checkpoint { height: u32, hash: BlockHash, chain_work: Work, window: Box<[Header; 25]> } }
pub struct AnchorPack { pub height: u32, pub hash: BlockHash, pub chain_work: Work, pub headers: [Header; 25] } // [0] = anchor - 24
pub fn anchor_candidate(packs: &[AnchorPack], params: &ChainParams, birth: Option<u32>,
                        now: NodeSeconds, history_blocks: u32) -> Anchor;     // SB0 §4.3.1 step 1
pub fn anchor_fallback(packs: &[AnchorPack], params: &ChainParams, current: &Anchor,
                       miss: CoverageMiss) -> Anchor;                          // SB0 §4.3.1 step 2
pub enum CoverageMiss { LowestUncovered(u32), UnknownBase }

// ---- errors (§14) --------------------------------------------------------------------------
pub enum ChainError { Storage(StoreError), BelowAnchor, ConflictsWithFloor { to: BlockId, floor: BlockId },
                      UnknownBlock(BlockId), Internal(&'static str), Config(ConfigError), Anchor(AnchorError) }
pub enum ApplyError<E> { Chain(ChainError), Host(E) }
pub enum LoadError { Read(StoreError), Decode { hash: BlockHash, err: CodecError }, HighHash { hash: BlockHash },
                     NonContiguous { missing_height: u32 }, MissingParent { hash: BlockHash, prev: BlockHash },
                     GenesisMismatch, Empty }   // GenesisMismatch, Empty: declared differences (D21, n3)
```

`ChainAccess` (P2a, `P2a-outbound.md:284-298`) is **not** implemented on `ChainReader`, which has no `loading_blocks` flag (O-m9). Its implementers wrap a `ChainReader`: CH1-node's `ChainView` in the node (it owns the import and reindex state behind Core's `loading_blocks`), and SB14's SPV adapter, whose `loading_blocks()` is false because SPV imports no blocks. Their infallible `entry`/`ancestor` answer `None` after a latch trip (D7). `process_headers` stays a `ChainCommand` to V (SB0 §13 P2a row).

## 5. Acceptance (Core `AcceptBlockHeader`, `ProcessNewBlockHeaders`)

### 5.1 Batch

`accept(batch)` ports `ProcessNewBlockHeaders` (`validation.cpp:4729-4756`):
1. `debug_assert_eq!(batch.hashes[i], x11(&batch.headers[i]))` for every i under `debug_assertions` only (`ASSERT_IF_DEBUG`, `:4602`; R7).
2. For each header in order: `accept_one`, then `check_index()` when `consistency_checks` (`:4737`). Stop at the first failure (`:4739-4741`).
3. `last_accepted` is set only after a header is accepted or found known (`:4742-4744`). A header ignored as a known context entry (§5.3) leaves it unchanged.
4. **Node:** when `best_header` or the floor changed, publish a status snapshot (epoch + 1, same `tip_seq`) before returning, and return it in `snap` (R3). **SPV:** publish nothing; the step publishes once at stage 4 of §9.1's recipe, and `snap` is the snapshot published before the step. `NotifyHeaderTip` and the progress log are the host's (`:4747-4754`).

`ChainError` aborts the batch only for a storage failure (the latch has tripped) or an internal error. A rule failure is `HeaderStop::Invalid`, never `Err`.

### 5.2 One header, in Core's order

| # | Step | Core | Notes |
|---|---|---|---|
| 0 | `hash == params.genesis_hash`: skip steps 1–9, go to 10 | `:4606` | node: the genesis entry exists, so step 10 returns it. SPV anchored: genesis is below the anchor → `BelowAnchor` (D13) |
| 1 | known hash: FAILED → `duplicate` (`CachedInvalid`); CONFLICT → `duplicate` (`ChainLock`); else return it as accepted | `:4605-4621` | SPV: a known **context** entry is skipped (D13) |
| 2 | `check_block_header(header, &hp, true)`: `high-hash`, `devnet-genesis` | `:4623-4626`; CS3 `header.rs` (`validation.cpp:4307`) | |
| 3 | prev lookup: missing → `prev-blk-not-found` (`MissingPrev`) | `:4629-4636` | SPV: the hash index, then `HeaderStore::by_hash`; a context parent → `BelowAnchor` |
| 4 | prev FAILED → `bad-prevblk` (`InvalidPrev`) | `:4638-4641` | |
| 5 | prev CONFLICT → `bad-prevblk-chainlock` (`ChainLock`) | `:4643-4647` | |
| 6 | build `RuleContext` for prev (§8); `contextual_check_block_header(header, &ctx, &hp)`: `bad-diffbits`, `bad-fork-prior-to-checkpoint`, `time-too-old`, `time-too-new`, `bad-version(…)` | `:4649-4652`; CS3 (`validation.cpp:4469-4520`) | building the context is the fallible step (`Storage`); the rules are infallible inside it |
| 7 | `m_failed_blocks` loop, only when `!prev.is_valid(VALID_SCRIPTS)`: for the first failed `f` (id order, D9) with `ancestor(prev, f.height) == f`, set `FAILED_CHILD` on prev … f's child, mark each dirty, → `bad-prevblk` | `:4673-4691` | SPV: `failed_blocks` is always empty, so the loop never runs |
| 8 | conflict check `has_conflicting_chainlock(prev.height + 1, hash)`, gated by the **live** `sporks.chainlocks_enabled()` (§7.5): add the header with status `CONFLICT_CHAINLOCK` (step 10 with that status), → `bad-chainlock` (`ChainLock`). Step 11 does **not** run for this header: Core returns at `:4698`, before `AcceptedBlockHeader` (`:4724`) (n1) | `:4693-4699`; `chainlock/chainlock.cpp:73-96` | §7.1 |
| 9 | (end of the non-genesis block) | `:4700` | |
| 10 | `add_to_block_index(header, hash, status = VALID_TREE)` | `:4701`; `blockstorage.cpp:85-128` | §5.4 |
| 11 | `accepted_block_header(id)`: the late-header floor update | `:4724`; `chainlock/chainlock.cpp:152-172` | §7.1; sets `floor_moved` |

The `ConsensusCtx` for step 6 is built in one place (`header_ctx_for(prev)`):
- `params`, `height = prev.height + 1`, `prev = Some(prev)`, `chain = &rule_context`;
- `deployments = &HeaderDeployments` (D14), `checkpoints = &self.checkpoints` (§11.4);
- `bls = bls_ctx_for_block(params, height)`, `median_time_past = prev.mtp`;
- `header_check_time` = `clock.node_now()` for `TimeSource::NodeClock` (v24; SB0 §4.2, DEC-104 M1), or `adjusted.now()` for `AdjustedTime` (V23_1 only; D8);
- `rules = cfg.rules`.

### 5.3 SPV-only paths (D13)

| Case | Answer | Why |
|---|---|---|
| new header whose parent is a context entry | `HeaderStop::BelowAnchor`, no record, no blame | a branch forking below the anchor (SB0 §4.1.1) |
| known context entry | skipped: the batch continues, `last_accepted` unchanged | the peer is resending the pack's headers; Core has no context entries |
| the genesis header on an anchored chain | `BelowAnchor` | Core's genesis short-cut would insert an unparented entry |
| parent deeper than the pack (unknown) | `prev-blk-not-found` | Core's answer for an unknown parent |

### 5.4 `add_to_block_index`

Port of `BlockManager::AddToBlockIndex` (`blockstorage.cpp:85-128`):
- `debug_assert!(!status.has(FAILED_MASK))` (`:88`). A known hash returns the existing id (`:91-94`).
- `prev`, `height = prev.height + 1`, `skip = build_skip(...)` (`:103-108`; `chain.cpp:84-130`). `time_max = max(prev.time_max, time)` (`:109`). `chain_work = prev.chain_work + block_proof(bits)` (`:110`). `mtp` from the 11 ancestors (`chain.h` `GetMedianTimePast`).
- **Cold prev (SPV; O-m5, D35).** A fork deeper than w has an evicted parent. `prev.time_max` comes from the parent's `HeaderRecord`, which stores it (SB13's schema gains the column). `prev.mtp` is not stored: `header_ctx_for` computes it from the `RuleContext`, which always holds prev and its ten ancestors (R ≥ 11, §8.1); the new entry's own `mtp` is computed the same way.
- **Best header** (`:111-119`): if `status & VALID_MASK`, raise validity and set `best_header = id` when `best_header.chain_work < chain_work` (strict, so first seen wins a tie). Otherwise raise to `VALID_TREE` and OR in the status: a `CONFLICT_CHAINLOCK` entry never moves `best_header`.
- **Dirty** (`:121`), in every mode.
- `m_prev_block_index` (`:123-126`): node, link `id` into `prev`'s sibling list (`links`); SPV, push `id` onto `pending_new` (§3.7, D10).
- **Leaf set** update (§6.2): remove `prev` from `leaves` (a no-op when `prev` is not a resident leaf), insert `id`. In SPV, a cold `prev` that was a leaf is not tracked individually; `max_cold_leaf_work` stays an upper bound (§3.7).
- **Checkpoint cache**: if `(height, hash)` is in the checkpoint list, `highest_checkpoint = max(...)` (§11.4).
- **Publish order**: `imm.set`, the status store, then the hash-index insert (§13.1).
- `nSequenceId` is 0 for a header-only entry (`:100`) and needs no field: CH1-node owns sequence ids.

## 6. Best header and SPV's fork target

### 6.1 `best_header` (Core `m_best_header`, SB0 §4.1, DEC-112 O1)

Updated only at Core's sites:

| Site | Core | `dash-chain` |
|---|---|---|
| acceptance, strict greater work, VALID_MASK status only | `blockstorage.cpp:111-115` | §5.4 |
| `InvalidateBlock`: the block being invalidated, or each disconnected old tip, **is** `best_header` → its prev | `validation.cpp:3951-3953`, `:3969-3971` | CH1-node calls `best_header_walk_back` |
| `MarkConflictingBlock`: the marked block, or each disconnected old tip, is `best_header` → prev | `:4089-4091`, `:4107-4109` | `mark_conflicting_block` calls it itself (§7.3), in both modes |
| `InvalidChainFound`: invalid is an ancestor-or-self of `best_header` → active tip | `:2043-2045` | CH1-node calls `best_header_invalid_chain` |
| load recompute over `IsValid(VALID_TREE)` entries with `CBlockIndexWorkComparator` | `:5359-5360`; `blockstorage.cpp:31-49` | §10.3 |
| fresh start: genesis | `LoadGenesisBlock` → `AddToBlockIndex` | `new_genesis` |
| anchored start: the anchor | — (SPV) | `new_anchored`: the anchor is an accepted `VALID_TREE` entry |

- `best_header_walk_back(marked)`: `if marked == best_header { best_header = prev(best_header) }`. Core's `m_best_invalid` assignment on the same lines (`:3952`, `:3970`) is CH1-node's.
- `best_header_invalid_chain(invalid)`: `if ancestor(best_header, height(invalid)) == Some(invalid) { best_header = active_tip }`.
- **Nothing else moves it** (rev7; the vectors are in `dash-chain/tests/data/core-scenarios/` on `node/sb4-core-scenarios` @ 42737ddf). Core's three quirks, which `best_header` reproduces:
  1. A walk-back fires only when the marked or invalidated entry, or a disconnected old tip, **is** `best_header`. Marking any other entry leaves it, even an ancestor whose mark makes `best_header` ineligible (`a17-conflict-then-reload`: B96 marked CONFLICT, `best_header` stays B110).
  2. Connects never move it, so after an enforcement reorg it stays at the fork point (`a26-ii-enforcement-reorg`: the tip returns to A1000 and `best_header` stays A990).
  3. `reset_failure_flags` (Core's `reconsiderblock`) does not restore it (`a18-invalidate-walkback`: after `reconsiderblock` A99 the tip is A100 again and `best_header` stays A98, still after `reconsiderblock` B105 makes the heavier B110 eligible).

  It moves forward again only when a header with strictly more work than itself is accepted (`blockstorage.cpp:111-115`), or at the next load.
- **Comparator at load**: more work wins; then the lower `nSequenceId`, which is 0 for every entry right after `LoadBlockIndexGuts` (no assignment site runs before `:5359`); then the lower address. The arena id stands in for the address (D9). SB0 §4.1 adopted this substitution. It is an **observable allowed difference** (Sol r1 m1): heap address order is not allocation order, so after a reload with equal-work tips Core's choice is not reproducible, and the choice shows in `best_header` and in the locator's first hash. D9 lists every effect, and oracle comparisons normalize exactly those. **These ties are not Core-determinable** (rev7): the harness reports them as `address_tie` (`a19-first-seen-tie`: C101 and D101 after `reload`), so no vector asserts a winner, and SB4's tests check only that the chosen entry is one of the tied ones and that `locator[0]` is its hash.
- **R5**: `best_header` is set by every constructor and is never null.
- Every change sets `status_changed`. The node publishes a status snapshot (epoch + 1) when `accept` or the hook returns; SPV publishes at the step's end (§9.1).

### 6.2 SPV `fork_target` (SB0 §4.1, DEC-115 R6-M1, DEC-136)

The most-work eligible accepted header, first seen (lower id) winning a tie. It is computed against the step's single `VirtualTip` (O-m4), after the step's enforcement has run, so "the chain" below means the cursor's chain: the published active chain cut at the cursor. Eligible means:
- **(a)** no FAILED or CONFLICT entry from the candidate down to the cursor's chain. This is Core's `FindMostWorkChain` walk (`validation.cpp:3533-3575`, read-only) against `m_chain` as Core's enforcement left it. Entries that enforcement marked on the published active chain lie above the cursor, so the walk checks them (Opus O-M1's second point). (a) applies whatever the spork state, because Core's candidate walk reads the marks unconditionally (`:3544-3547`).
- **(b′)** the candidate **does not conflict with `cl_floor`**: it is the floor, an ancestor of it (`ancestor(floor, height(c)) == c`), or a descendant of it (`ancestor(c, floor.height) == floor`). This applies when there is a floor and `cl_enabled_observed` (§7.5). (b′) is the header-level form of `ConnectBlock`'s `HasConflictingChainLock` check (`validation.cpp:2516-2518`), which is what stops Core from connecting a chain that conflicts with the floor. It replaces SB0's rev6.1 (b), "descends from the floor" (DEC-136; SB0 erratum E2). Whenever an eligible floor descendant exists it has more work than every ancestor of the floor, so (b′) selects what (b) selected. When none exists, Core's tip trails the floor (the node's R7-O1 behaviour), and so does SPV's.

**Computation over leaves (D19).** Leaves are the accepted entries with no accepted child, ordered by `(Work, Reverse<BlockId>)` and updated in `add_to_block_index`. Their number is the number of branches. In SPV only resident leaves are held, plus `max_cold_leaf_work` (§3.7). `fork_target(vt)`:
1. Walk the resident leaves from most work down. Stop when a leaf's work is below the best candidate found so far. If `max_cold_leaf_work` ≥ that candidate's work (or there is none), also read `HeaderStore::leaves()`, evaluate the cold leaves the same way, and refresh `max_cold_leaf_work` to the exact maximum. On mainnet this read never happens in practice: a leaf more than w below the tip would need more work than the tip.
2. For leaf L, walk from L towards the cursor's chain (`find_fork` against `vt`). Let X be the lowest entry on that path with FAILED or CONFLICT. The leaf's candidate C is L if there is no X, else `prev(X)` if `prev(X)` is not on the cursor's chain, else none.
3. Apply (b′) to C: at most two `ancestor` lookups over skip pointers. If C fails (b′), its candidate becomes `lca(C, floor)`. That entry is an ancestor of the floor, so it satisfies (b′), and it lies on C's checked path or on the cursor's chain, so it satisfies (a).
4. The cursor itself is always a candidate (its path is empty, so it satisfies (a)); under (b′) it is replaced by `lca(cursor, floor)` when it conflicts.

The result carries `FloorRelation` (`NoFloor`, `Unenforced` while observed off, `Descends`, or `Trails` when the target is a strict ancestor of the floor) for SB14/SB15's logs and diagnostics. A cold read error trips the latch and returns `ChainError::Storage` (O-m9).

**Equivalence to the predicate (a) ∧ (b′)** (not to Core's candidate set; Sol r1 m2). Every candidate produced above satisfies (a) ∧ (b′) (step 3). Conversely, let E satisfy (a) ∧ (b′), and let L be any leaf above E (one always exists). Every bad entry on L's path lies above E, so step 2's C is E or a descendant of E, unless `prev(X)` is on the cursor's chain, which can only happen when E is on the cursor's chain; then step 4's candidate is the cursor or `lca(cursor, floor)`, both E or above it. If C satisfies (b′), work(C) ≥ work(E). If C fails (b′), E is a common ancestor of C and the floor (E is the floor's ancestor, since a floor descendant's descendants satisfy (b′)), so `lca(C, floor)` is E or above it, and again its work ≥ work(E). Chain work strictly increases along a path (`block_proof` ≥ 1), so equal work means the same entry, and every maximum-work eligible entry is itself produced. Hence the maximum over the produced candidates equals the maximum over all accepted headers satisfying (a) ∧ (b′), ties included. The proof goes in the module docs; A-27 checks it against a brute-force scan.

**Opus O-M1's cases** (A-29 (ix) and (x); rev7 follows the Core-judged vectors, which overrule rev3's narrative of (ix)).
- **(ix)**, `a29-ix-reset-then-lower-lock`. Branches fork at 80; B81…B110 is active and A81…A95 are headers. B100 is already on the active chain, so its lock makes no marking walk (`validation.cpp:4057`): **A81 is never marked**. The spork goes off, a check observes it, and the spork goes on; the A90 lock's initial check resets `cl_best` and `cl_floor`, so the lower lock is accepted. Without the check while off (`a29-ix-no-observed-off`) the observed flag never leaves on, nothing resets, and the A90 lock is `Stale`. Enforcing A90 marks B81 and disconnects B110…B81 (B81…B110 CONFLICT, status 157), and `best_header` walks back to A80. Core's tip stays at A80, the fork point, under floor A90, because A81…A95 have no block data, not because of a mark. SPV's predicate is header-only: A95's walk meets no bad entry and A95 descends from A90, so the SPV target is A95 with `Descends`, the tip a node reaches once A81…A95 are downloaded. This is the data-dependent difference stated below, not a floor case.
- **(x)**, the kept-mark trailing case. `enforce_block` marks every sibling on the locked block's path below the point where that path meets the active chain, whether the sibling is active or not (`validation.cpp:4057-4064`, `:4120-4122`; rev7.1, Sol r7 R7-m1), so a sibling on a future floor's path can keep a mark. The connected-A fixture below supplies the kept-mark witness. Fork at 80; A81…A95 are connected and active; B81…B110 are headers only. The lock on B100 marks A81: `mark_conflicting_block(A81)` disconnects A95…A81 and marks A81…A95 CONFLICT. The tip goes to A80 (B has no data), and `best_header` stays B110 (no marked or disconnected entry is it). Off (observed) → on clears the floor and keeps the marks. The A90 lock marks B81 (a header, nothing disconnected). A90 is still `is_valid` (CONFLICT is not a FAILED bit, `chain.h:305`), so `reset_failure_flags` is not called, and A81…A95 keep CONFLICT. Every A leaf's walk stops at A81 and every B leaf's at B81, both with `prev` = A80 on the cursor's chain. The only candidate is the cursor, A80, an ancestor of A90, so the target is A80 with `Trails`, which is Core's tip after `EnforceBestChainLock`. Under rev2's (b) there was no target at all. The harness does not yet have this vector; §15 specifies it as `a29-x-kept-mark-trailing` (Sol's harness r1 and design r7 probes both reproduced these values).
- **(x′)**, the headers-only variant (rev7.1, Sol r7 R7-m1). It is the same as (x), except that A81…A95 are headers only and never active. The B100 lock marks A81 itself (status 130, no disconnect; A82…A95 stay 2). The tip is A80 throughout, and `best_header` is B110. After the observed off → on, the A90 lock marks B81 (130). A90 is header-only and so not `is_valid`, and the node calls `reset_failure_flags(A90, false)`, which keeps every CONFLICT (§7.4). A81 therefore stays 130. Both leaf walks stop at A81 and B81, and the SPV target is A80 with `Trails`, as in (x). §15 specifies it for the harness.

This is an equivalence to **SB0 §4.1's SPV predicate as amended by DEC-136**, not to Core's candidate set. Core's candidates (`setBlockIndexCandidates`) also require transaction validity and downloaded data along the path (`:3537-3547`, `:5348-5354`) and are ordered with `nSequenceId`; a heavier downloaded branch can still be tried until a later connect rejects it (`:2516-2518`). SPV has no block data, so SB0 adopted a header-only predicate, and SB4 implements it exactly.

**Cost.** In steady state the top leaf is the active tip: O(1). After a ChainLock, the walk covers the marked branches once. Cold entries in the walk are read through `HeaderStore` (SB0 §4.1 "cold reads"). A read error trips the latch.

**Core's candidate re-add** (`MarkConflictingBlock` `:4129-4137`, a full-index scan) has no counterpart in SPV: leaves are never removed for being ineligible, and each call re-evaluates them, so nothing needs re-adding. (The node's re-add is V's, in `conflicting_marked`, §7.3.)

**`set_active_tip(to)` in SPV mode** refuses a `to` that conflicts with `cl_floor`, while `cl_enabled_observed` (`ChainError::ConflictsWithFloor`, with `debug_assert!`). A target that trails the floor is allowed (DEC-136). The node never checks (R7-O1, SB0 §4.1).

## 7. ChainLocks

### 7.1 Floor and conflict check

`HeaderChain` holds Core's `Chainlocks` index state: `cl_best: Option<ChainLockRef>` (`bestChainLock`) and `cl_floor: Option<BlockId>` (`bestChainLockBlockIndex`).
- **`apply_chainlock(cl, host)`** ports the index side of `ProcessNewChainLock` (`chainlock/handler.cpp:133-183`) after signature verification (upstream, SB8/dashd-locks):
  1. `check_cl_active_state()` (`:136`; §7.5);
  2. `UpdateBestChainlock` (`chainlock/chainlock.cpp:106-132`): `cl.height <= cl_best.height` → `Stale`. Otherwise record `cl_best`. If the hash is known with a different height → `HeightMismatch`, floor unchanged (`:118-125`). If the hash is unknown → `Pending` (`:129-131`). Otherwise set `cl_floor`;
  3. with a known block, the scheduled `CheckActiveState` + `EnforceBestChainLock` (`:170-176`): `check_cl_active_state()` again, then `enforce_best_chainlock(host)`, which is `None` while the observed state is off. Returns `Floor { id, enforced }`.

  Recording does not depend on the spork state (§7.5). A CL reaches it while the live spork is off only through the node's `submitchainlock` RPC: the P2P intake drops a CLSIG unread (§7.5's intake row, rev7).
  - The node's `ChainCommand::ApplyChainLock` (CH1-node, m9) calls it on V, then runs `ActivateBestChain` when the tip does not contain the floor (`chainlock/handler.cpp:273-320`).
  - SPV calls it inside one step (§9.1's recipe).
  - A CL for an unknown header also waits in `dash-mnlist`'s pending set in SPV (SB0 §4.2). Recording `cl_best` here as well is what Core does, so a late header sets the floor even before the pending set re-delivers it.
- **Late header** (`AcceptedBlockHeader`, `chainlock/chainlock.cpp:152-172`): after step 10, if `hash == cl_best.hash` and `height == cl_best.height`, set `cl_floor = id` and report `floor_moved`. This is not gated by the spork. The node enforces it at the next `EnforceBestChainLock` (CH1-node schedules it as `chainlock/handler.cpp:63-72`, `:185-207` do). SPV runs `check_cl_active_state` + `enforce_best_chainlock` in the same header step.
- **`enforce_best_chainlock(host)`** = `EnforceBestChainLock`'s index half (`chainlock/handler.cpp:273-297`): `None` if `!cl_enabled_observed` (`:275-277`) or there is no floor (`:284-287`); else `enforce_block(cl_floor, host)`. The `ActivateBestChain` that follows (`:299-310`) is V's (node) or the tip change of §9.1's recipe (SPV).
- **`has_conflicting_chainlock(h, hash)`** (`chainlock/chainlock.cpp:73-96`): false if the **live** `!sporks.chainlocks_enabled()` (`:77-79`), or there is no floor, or `h > floor.height`; at the floor's height, `hash != floor.hash`; below it, `ancestor(floor, h).hash != hash`. Core asserts the ancestor exists. SPV reads it through `AnchoredAncestry`: `Storage` aborts the batch, and `BelowFloor` cannot happen because `h > prev.height ≥ anchor`.
- `HasChainLock` (`:49-71`, live-gated at `:53-55`) is provided as `has_chainlock(h, hash)` for CH1-node and RPC. `ConnectBlock`'s check (`validation.cpp:2516-2518`) is `HasConflictingChainLock`, i.e. `has_conflicting_chainlock`.

### 7.2 `enforce_block` (Core `EnforceBlock`, `validation.cpp:4048-4078`)

```text
walk = locked
while !host.contains(self, walk)?:               // :4057, the host's view (§7.3)
    for sib in children(prev(walk)) where sib != walk:   // §3.7: links (node), store + pending_new (SPV); id order (D9)
        mark_conflicting_block(sib, host)        // :4064; Core asserts success
    walk = prev(walk)
if !locked.is_valid(VALID_TRANSACTIONS):         // Core IsValid() default nUpTo (chain.h:300)
    reset_failure_flags(locked, false)           // :4075-4077; node only (D18)
```

In SPV the walk always ends: the anchor is on every chain the cursor can describe, because no disconnect goes below a sibling root's parent. Every enforcement in one SPV step uses the step's single `VirtualTip` (O-m4): a second enforcement in the same step (a received lock's, then a timer's or `floor_moved`'s) starts from the cursor the first one left, so nothing is disconnected or notified twice, as on Core's `m_chain`.

### 7.3 `mark_conflicting_block` (Core `:4080-4150`)

```text
walk_tip = host.tip(self)                                             // :4087
if b == best_header: best_header = prev(best_header)                 // :4089-4091
was_in_chain = false
while host.contains(self, b)?:                                        // :4096
    old_tip = host.tip(self); was_in_chain = true                     // :4097-4098
    host.disconnect_tip(self)?                                        // :4101; node: V's DisconnectTip; SPV: cursor = prev
    if old_tip == best_header: best_header = prev(best_header)       // :4107-4109
while was_in_chain and walk_tip != b:                                 // :4114-4118
    set_status(walk_tip, |= CONFLICT_CHAINLOCK); walk_tip = prev(walk_tip)
set_status(b, |= CONFLICT_CHAINLOCK)                                  // :4121
host.conflicting_marked(self, b, was_in_chain)?                       // :4122-4148, O-M2
```

`set_status` writes the entry under its seqlock in the node, and the overlay copy in SPV (§3.7).

**Sol r1's B110/A100 case.** B110 is the tip and `best_header`, and the fork is at 80. Marking B81 reads `old_tip` from the cursor as B110, B109, …, B81, so `best_header` walks back to block 80, which is exactly Core's sequence. Core's `best_header` stays at 80 until another header is accepted. That is Core's value, and SB4 reproduces it (A-28).

- **Not dirty** in the node: Core inserts none of these into `m_dirty_blockindex`, so the marks persist only if the entry is dirtied later for another reason (D15). Core-judged: `a17-conflict-then-reload`, B96's status 130 before `reload` and 2 after (rev7). In SPV every status change is dirty, so a restart reproduces the marks (SB0 §4.4: records carry branch status).
- **`conflicting_marked` (O-M2, DEC-136).** Core finishes each `MarkConflictingBlock` call, under `cs_main` and before `EnforceBlock` moves to the next sibling, with the work below. The node's V does it in `conflicting_marked`, in this order:
  1. erase the walked and marked entries from `setBlockIndexCandidates` (`:4116`, `:4122`) and `MaybeUpdateMempoolForReorg` (`:4126`);
  2. the candidate re-add scan against the **current** tip (`:4129-4137`);
  3. `ConflictingChainFound(marked)` (`:4139`): its log and `CheckForkWarningConditions`;
  4. `SynchronousUpdatedBlockTip` and `UpdatedBlockTip(m_chain.Tip(), nullptr, IsInitialBlockDownload())` (`:4140-4142`), with `host.tip()` as the tip;
  5. when `was_in_chain`, `uiInterface.NotifyBlockTip(…, marked.prev)` (`:4145-4148`).

  For a lock on A100 with B110 active (fork at 80) and a stale sibling C90 under A89, Core emits `UpdatedBlockTip(B110, nullptr)` for C90 (marked first, from the top of the path), then the disconnects of B110…B81, then `UpdatedBlockTip(80, nullptr)` for B81; ZMQ publishes `hashblock` for each (`zmq/zmqnotificationinterface.cpp:160-167`). The hook reproduces that order; a replay after `enforce_block` could not (A-36). `VirtualTip`'s implementation records the cursor, the tip argument of the same notification, for `updated_block_tip_schedules` (§7.5).
- **Node host.** V's `DisconnectTip` implementation calls `chain.disconnect_tip()` (one tip step, one `Disconnected` event with its `seq`) after its own coins work. Inside `enforce_block` those steps run under the tip mutex, which `enforce_block` holds from entry to return. Each event goes to the feeds at its step (D43), CH2's IF_NEEDED check runs in every `DisconnectTip` (DEC-144), and the snapshot is stored once before the release (§9.1, O-m3, R4-M2), because Core holds `cs_main` across the whole `EnforceBlock` (`validation.cpp:4053`). The tip trails the floor through every step, and no check refuses it (R7-O1).
- **SPV host (`VirtualTip`).** SPV connects no blocks, and its tip moves only at stage 4 of §9.1's recipe (SB0 §4.6).
  - `chain.virtual_tip()` starts a cursor at the active tip, once per step (O-m4). `tip()` is the cursor.
  - `contains(x)` is `height(x) <= height(cursor) && ancestor(cursor, height(x)) == x`, resolved over arena entries, the overlay and cold records with skip ids. That is O(log n) and correct below the window, where `ActiveChainView::at` has no answer.
  - `disconnect_tip` sets `cursor = prev(cursor)` and publishes nothing.

  Every read Core makes of `m_chain` in `:4048-4150` therefore sees the chain as Core's would after the same disconnects, so the marks and walk-backs are Core's. The real disconnects happen at stage 4 of §9.1's recipe, which publishes once.

### 7.4 `reset_failure_flags` (index half of `ResetBlockFailureFlags`, `validation.cpp:4152-4220`)

Port of Core's two loops with their exact conditions (O-m10). Both drive the dirty set, so the `pnbh`/`cl` record diffs depend on them.
1. **Descendants** (`:4177-4192`), a full-index scan as in Core: every entry `e` with `!e.is_valid(VALID_TRANSACTIONS)` (Core's `IsValid()` default `nUpTo`, `chain.h:300`) and `ancestor(e, height(id)) == id`, which includes `id` itself. For each: clear `FAILED_MASK`; clear `CONFLICT_CHAINLOCK` only when `ignore_chainlocks`; mark dirty; add to `reconsidered`; remove from `failed_blocks`. A header-only descendant is never `VALID_TRANSACTIONS`, so **every** header-only descendant is dirtied, even with no failure bit.
2. **Ancestors** (`:4194-4220`), from `id` down to genesis: every `p` with `p.status & (FAILED_MASK | CONFLICT_CHAINLOCK)`. For each: clear `FAILED_MASK`; clear `CONFLICT_CHAINLOCK` only when `ignore_chainlocks`; mark dirty; add to `reconsidered`; remove from `failed_blocks`; then for each child `c` of `p` (`links`) with `FAILED_CHILD`: set `FAILED_VALID` (keeping `FAILED_CHILD`), insert into `failed_blocks`, mark dirty, add to `promoted`. So with `ignore_chainlocks = false` an ancestor carrying only `CONFLICT` keeps it, but is still dirtied and reconsidered, and its `FAILED_CHILD` children are promoted.

It returns `ResetReport { reconsidered, promoted }`. The `!pindex` substitution of `m_best_invalid` (`:4155-4163`), `m_best_invalid = nullptr`, erasing `promoted` from the candidates (`:4215`) and the candidate admission pass after `:4220` are CH1-node's.

**SPV never calls it** (D18). SPV entries never carry FAILED bits (no block validation, `failed_blocks` empty), so Core's call changes no SPV status: loop 1 only dirties, and loop 2 keeps every CONFLICT when `ignore_chainlocks` is false. A debug assertion checks that no SPV entry has a FAILED bit.

### 7.5 SPORK_19 (DEC-123 Q2; Sol r1 M5)

SB4 models what Core does, and nothing stricter. Core keeps two notions of "ChainLocks enabled", and both are kept:
- **live**: `Chainlocks::IsEnabled()` = `IsSporkActive(SPORK_19)` (`chainlock/chainlock.cpp:33`), evaluated at each call. SB4 reads it as `services.sporks.chainlocks_enabled()`;
- **observed**: the handler's cached `std::atomic<bool> isEnabled{false}` (`chainlock/handler.h:55`), refreshed only by `CheckActiveState`. SB4 holds it as `cl_enabled_observed`.

**Core facts (v24.0.0-rc.3).**

| Fact | Core | `dash-chain` |
|---|---|---|
| Default value | `sporkDefs`: SPORK_19 = 4070908800, "OFF" (`spork.h:66-72`) | the source's concern (D29); SB4 sees only the boolean |
| Mainnet | `GetSporkValue` returns 0 for every spork but SPORK_21 when `!IsTestChain()` (`spork.cpp:171-181`): SPORK_19 is always active on mainnet, whatever is received | `StaticSporks::for_network` returns true on mainnet; SP0's manager the same |
| Test chains (testnet, devnet, regtest) | the latest received spork's value if any (`SporkValueIfActive`, `:22-30`, `:185-187`), else the default (`:189-191`) | from the source; default off |
| Activation | `IsSporkActive`: value < `GetAdjustedTime()` (`:163-169`) | the source evaluates against its adjusted time (`dash-clock`, SB6) |
| Signed messages | `IsValidSpork`: `nTimeSigned` ≤ adjusted time + 2 h and signed by `sporkPubKeyID` (`:86-102`); `ProcessSpork` keeps the latest `nTimeSigned` per id (`:104-125`); message hash and signature `:264-330` | intake, verification and storage are not SB4's (D29) |
| Persistence | `sporks.dat` (CFlatDB): loaded at init step 7a, before the chain (`init.cpp:1967-1972`; `LoadCache` `spork.cpp:52-60`, `CheckAndRemove` `:62-84`), stored by the destructor at clean shutdown (`:41-50`) | the source's; it must be loaded before `HeaderChain` is constructed |
| ChainLock state persistence | none: a restarted node has no best CL and `isEnabled == false` | node: none, as Core. SPV: the CL rows persist (SB0 §4.3.1, §9.5) with `enabled_observed` beside them (D30) |
| `CheckActiveState` | `old = isEnabled; isEnabled = IsEnabled(); if !old && isEnabled { ResetChainlock(); lastNotifyChainLockBlockIndex = nullptr }` (`chainlock/handler.cpp:209-222`) | `check_cl_active_state()`: same transition; the reset clears `cl_best` and `cl_floor` (`ResetChainlock`, `chainlock/chainlock.cpp:98-104`); the notify pointer is CH1-node's |
| Turning off (true → false) | nothing is cleared; `isEnabled` goes false at the next check | the same: `TurnedOff`, nothing cleared |

**Where each notion is read.**

| Read | Core | Gate |
|---|---|---|
| CLSIG intake (rev7) | P2P `CLSIG`: dropped unread unless `Chainlocks::IsEnabled()`, i.e. `IsSporkActive(SPORK_19)` (`net_processing.cpp:5794-5795`, `chainlock/chainlock.cpp:33`); `submitchainlock` RPC: calls `ProcessNewChainLock` with no spork check (`rpc/quorums.cpp:1350-1398`, call at `:1397`) | P2P: live, at the caller of `apply_chainlock` (the node's net-processing CLSIG handler; SPV's intake under SB14's driver). RPC: none, node only |
| `UpdateBestChainlock` (record best CL, set floor) | `chainlock/chainlock.cpp:106-132` | none once called; while the live spork is off only `submitchainlock` calls it (row above) |
| `AcceptedBlockHeader` (late header sets floor) | `:152-172` | none |
| `HasConflictingChainLock` (acceptance step 8, `ConnectBlock`) | `:77-79` | live |
| `HasChainLock` | `:53-55` | live |
| `EnforceBestChainLock` | `chainlock/handler.cpp:275-277` | observed |
| `ProcessCoinbaseChainLock` | `chainlock/handler.cpp:107` | live (CH1-node) |
| existing `CONFLICT_CHAINLOCK` marks: duplicate (`validation.cpp:4612-4619`), `bad-prevblk-chainlock` (`:4643-4646`), `FindMostWorkChain` (`:3544-3547`) | — | none: marks act whatever the spork |
| SPV fork-target eligibility (b′) and the `set_active_tip` floor refusal (§6.2) | SB0's stand-ins for `EnforceBlock` + `ActivateBestChain` and `ConnectBlock`'s conflict check | observed (the transient live/observed difference is D12's) |

**`check_cl_active_state` call sites.** Core calls `CheckActiveState` at four places, and SB4's hosts call the port at the same ones:

| Core | Node (CH1-node) | SPV (SB14's driver) |
|---|---|---|
| `Start`'s 5 s scheduler, then `EnforceBestChainLock` (`chainlock/handler.cpp:63-72`) | the same timer | the same timer; the enforcement runs as a ChainLock step (below) |
| `UpdatedBlockTip(pindexNew, pindexFork, fInitialDownload)`: returns if `pindexNew == pindexFork` (`:187-188`) or `fInitialDownload` (`:189`), else schedules check + enforce once (`:197-206`) | V's notifications, with Core's arguments from all three producers: `ActivateBestChain` (`validation.cpp:3818-3822`), `InvalidateBlock` (`:4034-4037`) and `MarkConflictingBlock` (`:4139-4142`, `(m_chain.Tip(), nullptr, IsInitialBlockDownload())`) | the same guards through `updated_block_tip_schedules(new, fork, ibd)`. Producers: each published step (`new` = final tip, `fork` = the plan's fork, `ibd` = the snapshot's latch) and each `MarkConflictingBlock` call in the step (`new` = the tip `VirtualTip` recorded in `conflicting_marked`, `fork = None`) |
| `ProcessNewChainLock` start (`:136`) | inside `apply_chainlock` | inside `apply_chainlock` |
| the enforcement scheduled after a CL with a known block (`:170-176`) | inside `apply_chainlock` | inside `apply_chainlock` |

A **ChainLock step** in SPV is a step with no headers (timer, received lock, scheduled check, reset only). It follows §9.1's step recipe.

**Lifecycle.**
- **Start.** Node: `cl_enabled_observed = false`, no CL (Core's restart). SPV: the persisted `enabled_observed`, `cl_best` and `cl_floor` (D30); a fresh store starts false with no CL.
- **First check with the spork on** (every mainnet start; a test chain whose source holds an active SPORK_19): false → true, so `ResetChainlock` runs. In the node it clears nothing, because nothing was recorded before the first check (`ProcessNewChainLock` checks first). In SPV it clears a floor only when the stored state was off.
- **On → off.** Nothing is cleared. While observed off: no enforcement, no SPV floor refusal or eligibility (b′). While live off: no conflict check at acceptance. A late header still moves `cl_floor` for a CL already recorded (`a29-ii-late-header-before-check`). A **new** CL is recorded while off only through the node's `submitchainlock` RPC (`a29-i-off-on-lower-height` step 6, `cl via: submitchainlock`). A P2P CLSIG is dropped unread (`a29-i-p2p-lock-on-off-on` step 7, `dropped: spork19-off`). Existing marks keep acting.
  - **SPV while it believes the spork is off** (rev7). SPV has no RPC path. While its live `SporkView::chainlocks_enabled()` is false, its intake drops a received CLSIG unread, as Core's P2P handler does: no signature verification, no `dash-mnlist` pending entry, no `apply_chainlock`. A CL recorded while off is therefore unreachable in SPV. Only a CL recorded while on, including one recorded on a still-unknown header, carries into an off period. This is consistent with DEC-123 Q2 as decided, i.e. Core's SPORK_19 semantics with no always-on SPV (rev0's `AlwaysOn` was withdrawn; D12, D31). On mainnet the spork is hard-wired on, so SPV never drops a CLSIG there; on test chains it drops one until a signed SPORK_19 is received.
- **Off → on.** At the first check after the change, `cl_best` and `cl_floor` are cleared and `ClActiveState::TurnedOnReset` is returned. The transition is the **observed** one: a toggle off and on with no check in between leaves the observed flag on, so nothing resets (`a29-ix-no-observed-off`). Every CL recorded while off (in the node, by `submitchainlock` only) is lost, including the floor established before the spork went off. **Marks stay**: `ResetChainlock` touches no index entry, so branches marked before the transition remain `CONFLICT_CHAINLOCK` until `reset_failure_flags(_, true)` (Core's `reconsiderblock`). The next CL of any height is accepted, because the cleared best CL has height −1 (`chainlock/chainlock.h:25`; `chainlock/chainlock.cpp:110-113`). A sibling on the future floor's path can keep a mark from the earlier lock, whether its branch was active and connected (A-29 (x)) or headers only (A-29 (x′)). Then no floor descendant is eligible and the tip trails the floor, in Core and in SPV (§6.2). A lock whose block is already on the active chain makes no marking walk (A-29 (ix)). SPV writes the cleared rows and `enabled_observed = true` in that step's WriteSet.
- **Live and observed disagree** between a spork change and the next check: nominally up to the 5 s timer, sooner when a lock or a qualifying `UpdatedBlockTip` refreshes it. SB4 reproduces the window: acceptance follows live, enforcement and SPV's floor follow observed. One transient difference remains (n7, D12): (b′) and the SPV refusal stand in for `ConnectBlock`'s live-gated check as well as the observed-gated enforcement, and follow the observed flag, so in that window SPV may keep a target Core would refuse to connect, or the reverse, until the next check.

**D29: spork sources.** `SporkView` is a seam; SB4 owns no spork state.
- **Node**: SP0's spork manager (`dashd-masternode::sporks`, PLAN:694), which ports `CSporkManager` and imports `sporks.dat`. Until SP0 lands, `StaticSporks::for_network(params, overrides)`: mainnet true; test chains the default (off) unless an override is configured. That matches a Core test-chain node that has received no spork.
- **SPV**: the same verification and value logic as a shared spork core, with intake over SB16's message routing. It is stored at receipt, not only at clean shutdown, because mobile processes are killed rather than shut down. This is a declared allowed difference: it changes when the value survives a crash, not what the value is. Until it lands, `StaticSporks` as above.
- **Tests**: `StaticSporks` with a settable value drives A-29.

**D30: SPV persists `enabled_observed`.** Core loses both the CL state and the observed flag at restart. SPV keeps the CL rows (SB0 §4.3.1 step 3, §9.5). Restoring them with a fresh `false` would make the first check reset them on every start with the spork on. That would undo SB0's persisted floor at every launch on mainnet, a difference from SB0 with no Core counterpart. Persisting the flag keeps the reset tied to an observed off → on transition, which is Core's trigger.

**D31: "default on" in DEC-123 Q2** is read as Core's behaviour: on for mainnet (hardened), and the received value with an off default on test chains. On testnet and devnet the network's signed SPORK_19 is active, so the effective state is on once a spork message has been received.

**SB0 erratum.** SB0 §4.3.1 says an applied ChainLock and its floor "persist until a newer verified ChainLock supersedes them" (`SB0-shared-base.md:648`). DEC-123 Q2 adds Core's second way out: the off → on reset. Accepted by DEC-127 (Q-r1-1) and recorded as SB0 erratum E1 (SB0 §17), next to DEC-136's E2 (§6.2).

## 8. `RuleContext` and `AnchoredAncestry` (SB0 §4.1.1)

### 8.1 Read envelope R

`fn read_envelope(pow: &PowParams, next_height: u32) -> u32`, following `GetNextWorkRequired`'s dispatch (`pow.cpp:170-212`):

| Era (next height h) | R | Core |
|---|---|---|
| `h >= dgw_height` | 25 | `pow.cpp:81-133` (parent + 23 predecessors; one spare) |
| `kgw_height <= h < dgw_height` | `PastBlocksMax + 1` = 4,033 on mainnet | `pow.cpp:15-79`; `dash-consensus/src/pow/kgw.rs:34-37`, `:64-68` |
| `h < kgw_height`, `h % interval == 0` | `interval` (576) | `pow.cpp:162-167` |
| `h < kgw_height`, else, `allow_min_difficulty_blocks` | `interval` (≤ 576) | `pow.cpp:142-157` |
| otherwise (incl. devnet's `nMinimumDifficultyBlocks` prefix) | 11 | MTP |

`R = max(R_era, 11)`. Values come from `PowParams` (CS2), never constants.

### 8.2 Construction

- `RuleContext::build(chain, prev) -> Result<RuleContext, ChainError>` collects the ids on `prev`'s branch from `max(prev.height + 1 − R, floor)` to `prev` into `path: Vec<BlockId>` indexed by height.
- Resident entries are borrowed. Entries below the window come from `HeaderStore::by_id` along `prev`, into `cold: SmallMap<BlockId, HeaderCtx>`. `StoreError` → `fail.read_failed()` and `ChainError::Storage`.
- **Incremental reuse** (D23, Sol r1 M3). The writer caches one context: `{ top: BlockId, lo: u32, path: VecDeque<BlockId>, cold: HashMap<BlockId, HeaderCtx>, residency_epoch: u64 }`. For each candidate:
  1. Compute `R = read_envelope(pow, prev.height + 1)` afresh, and the wanted lower bound `lo' = max(prev.height + 1 − R, floor)` with `saturating_sub` (no underflow near genesis; `floor` is 0 or the pack floor).
  2. **Same branch** when the cached `top` is `prev` or an ancestor of `prev` (checked as `ancestor(prev, height(top)) == top`, O(1) when `prev(prev) == top`). Then push the ids from `top` (exclusive) up to `prev` at the top.
  3. **Envelope grew** (`lo' < lo`, e.g. 575 → 576 where R goes 11 → 576, or 15,199 → 15,200 where R goes 11 → 4,033): fetch the missing older prefix `lo' .. lo − 1` by walking down from `path.front()` and push it at the bottom.
  4. **Envelope shrank or slid** (`lo' > lo`): pop from the bottom.
  5. **Branch change** (`top` not an ancestor of `prev`): rebuild from `prev`.
  6. **Eviction**: the cache stores ids, never borrows. Each `set_active_tip` that evicts bumps `residency_epoch`. An id the cache holds that was evicted since is re-read into `cold` the next time it is used. The cache is therefore never dangling. Ids are never reused in SPV (SB0 §4.4), so a stale id cannot alias another entry.

  Cost: O(1) amortized per in-order header, and O(R) at each growth, which happens once per retarget interval in the BTC era (576 reads every 576 headers) and once at KGW activation. A genesis-origin KGW-era sync therefore stays O(n). A branch change costs one O(R) rebuild, as rev0's did. A-5/A-6 drive consecutive accepts through each boundary with a warm cache (§15.1).
- `impl ChainAncestry for RuleContext`:
  - `header(id)` → the entry's `HeaderCtx`;
  - `ancestor(id, h)` → `path[h]` when `id` is on the path and `h ≤ height(id)`;
  - `prev(id)` → `path[height(id) − 1]`.
- **A read outside the path** sets an out-of-range flag and returns a fixed dummy. In debug it panics first (`broken_chain_panics_in_debug`). After the rule returns, `accept` checks the flag and returns `ChainError::Internal("rule read outside its envelope")`, with no blame and nothing inserted.
- **The node** passes `complete()` (the arena itself) as `ChainAncestry` instead. It has no window, the reads cannot fail, and no context is built.

### 8.3 Lazy `mtp` (m10)

The ten lowest pack entries have no `mtp`, which needs headers below the pack. They store `MTP_UNKNOWN = i64::MIN`. `HeaderCtx.mtp` is read by the rules only through `ConsensusCtx.median_time_past = prev.mtp`. A pack entry is a `prev` only when it is the anchor (index 24), whose `mtp` is known (entries 14–24). `header_ctx_for` therefore `debug_assert!(prev.mtp != MTP_UNKNOWN)`.

### 8.4 `AnchoredAncestry` on `HeaderChain`

- `floor()` = `anchor.height − 24`, or 0 for genesis.
- A request below `floor()` → `BelowFloor { height }`. A cold read error → `Storage` (the latch has tripped).
- `ancestor` follows skip pointers over arena entries, then over records' persisted skip ids (SB0 §4.4: O(log n) record reads).
- **The lowest held entry** (D-C4-1, for SB5 F5b). The lowest pack entry (height `floor()`) has `prev = None` and `skip = None`, like genesis. Its header keeps its real `prev_hash`, which resolves to nothing in the index. Every entry whose skip height is below `floor()` gets `skip = None`, which is `BuildSkip` over this ancestry. Walks then take `prev`, which every entry above `floor()` has. `floor() > 0` together with `height == floor()` tells it apart from genesis.
- **Fallible reader calls** (C4; SB5 §5.4). `ChainReader::{try_lookup, try_ancestor, try_lca, try_entry}` return `Result<Option<_>, ChainError>`. They read the resident entry, then the cold cache, then the store.
  - `Err(Storage)` happens only on a failed store read, and the latch has tripped by then.
  - `try_ancestor(id, h)` returns `Ok(None)` when `h` is above `id`'s height or below `floor()`. The 24 context entries are real ancestors, so heights from `floor()` up to the anchor answer `Some`. SB5 §5.3's cut at the anchor height is SB5's own.
  - `try_entry` returns `Ok(None)` for an id the chain never held. The infallible `ChainReader::ancestor` makes the same floor check first, returning `None`. So `walk::ancestor`'s `pprev` expect is reached only when `h ≥ floor()`, where it cannot fail: every entry it walks through is above the floor.

## 9. Tip, `seq` and the snapshot

### 9.1 The step

Publication follows Core's lock scopes (Sol r1 M1, Opus O-m3; D28). The node publishes every connect and disconnect step of `ActivateBestChain` and `InvalidateBlock`, which release `cs_main` between steps, so the tip trails a floor until activation completes (`chainlock/handler.cpp:299-309`). Inside `enforce_block` the node publishes once, when it returns, because Core holds `cs_main` across the whole `EnforceBlock` (`validation.cpp:4053`) and its readers never see a half-walked state. SPV publishes once per committed step, because SB0 §4.1's SPV invariant (as amended by DEC-136) is that no published tip conflicts with `cl_floor`, and a disconnect/connect sequence passes through tips that do.

All tip writers go through one private `step(dir, block)` (D4), which runs under the tip lock (`TipCell::state`):
1. update `active` (§3.6) and the tip;
2. `seq = next_seq; next_seq += 1` (F2);
3. SB5 adds: record the event with its `seq` for fanout.

**Node.** Each `connect_tip`/`disconnect_tip` is one critical section: take the lock, `step`, build `ChainReadSnapshot { tip_seq: seq, epoch: epoch + 1, .. }`, store it in `TipState.snapshot` and in the `ArcSwap` readers load (D6), fan the event out to live feeds or `pending` (SB5), release. `set_active_tip` in the node runs the plan as such steps.

**Node inside `enforce_block`** (O-m3, DEC-136; Sol r4 R4-M2, DEC-140). The whole enforcement is **one critical section of the tip mutex**, as Core's `EnforceBlock` holds `cs_main` from `validation.cpp:4053` to its end:
1. Without the lock, wait until `pending` is empty (CH2 rule 5, as before any tip change).
2. Take the tip mutex as an owned guard (`TipCell.state` is an `Arc<parking_lot::Mutex<TipState>>`, locked with `lock_arc`) and keep it in the writer-only slot `composite` until step 5.
3. Run the walk. Each V `DisconnectTip` keeps Core's order (`validation.cpp:3249-3316`):
   - the coins disconnect;
   - CH2's `IF_NEEDED` check at its existing site, before the tip moves, on **every** disconnect (DEC-144, Sol r5 R5-M2). A CACHE_CRITICAL or EVO_CRITICAL wait for the in-flight F's release, and the freeze that follows, happen here under the held guard, as Core's `FlushStateToDisk(IF_NEEDED)` runs inside `DisconnectTip` under `cs_main`. §13.4's lock-order contract makes the wait complete;
   - `chain.disconnect_tip()`, which runs `step` under the held guard (its own `seq`, its own F1 event) and hands the event at once to the live feeds or to `pending` (D43). Before the next step V waits under the guard for `pending` to drain (CH2 rule 5; §13.4 H4). No snapshot is stored.
4. On return, also on error, in the same critical section: store one snapshot (`tip_seq` = the last `seq`, the final `best_header`, marks, floor and observed flag). The snapshot store is the same composite as SPV's stage 4.
5. Release.

Consequences:
- **Handoffs see only the pre- or post-enforcement cut.** `subscribe_at` and its `finish()` (F10), `deregister` and `publish_status` take the same mutex. A slot can go live only at the pre-enforcement tip, before the first staged `seq`, or at the post-enforcement tip, after every staged event has captured its targets. In Sol r4's interleaving, a subscriber retained at B109 calls `subscribe_at(B109)` during an A100 enforcement that has staged the disconnect of B110. It blocks until step 5, then finds the tip at 80 and gets `Moved` (A-39).
- **Lock-free readers are unaffected.** They see the pre-enforcement snapshot until step 4: `ArcSwap` snapshots, `IndexAccess`, and seqlocked statuses under §13.2's per-entry contract.
- **Feeds** see one event per disconnect, at its step and in `seq` order (D43). Core does the same: its queued `BlockDisconnected` reaches listeners on the scheduler thread while `EnforceBlock` still holds `cs_main`. `conflicting_marked`'s notifications go out during the walk too; they carry tips but read no snapshot.
- **Callbacks run under the guard** (D42, §13.4). V's `DisconnectTip`, CH2's `IF_NEEDED` check and `conflicting_marked` must not call a mutex-taking `TipCell` API (`subscribe_at`, `deregister`, `publish_status`, `connect_tip`, a nested `enforce_block`). The only cross-thread waits allowed under the guard are the CRITICAL wait for W's release and the `pending` drain. Both depend only on W and subscribers, whose paths §13.4 keeps free of the tip mutex. Debug builds record the guard's owner thread and panic on a reentrant lock attempt instead of deadlocking.

**SPV.** `set_active_tip(to)` is the only SPV tip writer after construction (SB5's `activate_segmented` is its segmented form, D47). It is stage 4 of the step recipe below, and runs:
1. refuse a target that conflicts with the floor (§6.2, while `cl_enabled_observed`);
2. take the tip lock **once**;
3. apply the overlay's resident status changes to their seqlocked groups and drop its evicted ids from the cold LRU (§3.7, O-m8); run every disconnect tip-down, then every connect fork-up, through `step`. Each gets its own `seq` in F1 order; nothing is published between them;
4. build one snapshot with `tip_seq` = the last step's `seq` (or the old one if no step ran) and `epoch + 1`, carrying the final `best_header`, `cl_floor` and `cl_enabled_observed`; store it in `TipState.snapshot` and the `ArcSwap`;
5. SB5 adds: hand the step's events, in `seq` order, to the live feeds or to `pending` in the same critical section. A durable subscriber's `subscribe_at` (DEC-96) takes the same lock, so it sees either none or all of a step's events, and a snapshot whose `tip_seq` covers exactly the events delivered. The node's `enforce_block` composite gives the same guarantee by holding the lock throughout (R4-M2). SPV needs no such hold during enforcement, because its enforcement moves only the `VirtualTip` cursor and leaves `TipState` untouched until this stage;
6. evict (SB0 §4.4, D3) and release the lock: eviction runs in `stage4_effects` under the tip lock and takes the index write lock there (D-A11-3, §13.1).

`accept`, `apply_chainlock`, `enforce_best_chainlock` and `check_cl_active_state` publish nothing in SPV. Status changes (best header, floor, marks, spork state) become visible in the snapshot of step 4 even when no tip step ran. A reader therefore never sees an intermediate tip. Every published tip is the final `to`, and it does not conflict with that snapshot's `cl_floor` (it is the floor, an ancestor, or a descendant) whenever that snapshot's `cl_enabled_observed` is true (DEC-136). It may trail the floor, as Core's does. While the observed state is off, the floor is kept but neither eligibility (b′) nor the refusal applies (§7.5), so the tip may leave the floor's chain. A-23 asserts, for every published `snap`:

```rust
!snap.cl_enabled_observed || snap.cl_floor.is_none() || !conflicts(snap.tip, snap.cl_floor)
// conflicts(t, f) = ancestor(f, height(t)) != Some(t) && ancestor(t, height(f)) != Some(f)
```

**The step recipe (DEC-128; normative).** This is the only statement of a step's order in this design; every other section cites it. It applies to every step kind: a header batch, the 5 s timer, a received lock, a late header, and a step that only resets (SPORK_19 off → on).
1. **Compute.** The driver takes one `vt = chain.virtual_tip()` for the whole step (O-m4). The step's calls on the writer: `accept` (header batch, late header), `check_cl_active_state`, `apply_chainlock(cl, &mut vt)` (received lock), `enforce_best_chainlock(&mut vt)` (timer, `floor_moved`, a scheduled `UpdatedBlockTip` check), then `fork_target(&vt)` and, for the change's shape, `tip_target(target)`. **Stage 1 never builds the full `tip_plan`** (DEC-187, D47): its legs grow with the change, outside SPV's step bound of cap + M + L. In SPV the step's disconnect list is held only on `vt` (`VirtualTip::disconnected`); `EnforceReport.disconnected` is node-only and stays empty, so the list exists once (D47).
2. **Capture.** `take_dirty_records()` (resident changes and overlay replacements), `cl_state()` (CL rows and `enabled_observed`) and the active/meta change, sized from `tip_target` and never from a materialized plan, into one WriteSet.
3. **Commit** the WriteSet in one transaction (SB0 §9.5).
4. **The tip move**, SB5's `activate_segmented(target)` (SB5 §4.8; DEC-187). It sizes the change with `tip_target` and plans each sub-step with `tip_plan_bounded`, so the writer holds one sub-step's legs at a time. SB4's unsegmented form is `set_active_tip(target)`, which executes a `pending_plan` from `tip_plan` exactly (a different `to`, or no pending plan, is `ChainError::Internal`). Either form then publishes once, fans out and evicts (the SPV list above). Nothing mutates the chain between 1 and 4: the driver holds `&mut HeaderChain`, and the commit touches only the store.
5. **`records_committed()`**: drops the overlay's applied marks and invalidates the written ids in SB5's cold LRU. A mark that no stage 4 applied stays and the next stage 4 applies it with that step's snapshot (D-R2-C-1, §3.7); SB5's n8 emptiness check therefore counts a kept mark as work. The driver then replies with `TipChange.snap`.

**Empty steps (n8).** When stage 2 captures nothing (no dirty record, CL rows and observed flag unchanged, an empty plan), stages 3–5 are skipped: a quiet 5 s timer step neither commits nor publishes.

A failed commit trips the fail-stop latch (P6) at stage 3, so stages 4 and 5 never run: nothing is published, fanned out or evicted, and the store keeps the previous whole step. A crash anywhere before stage 3 completes leaves the previous whole step. A crash after it leaves the new whole step, with the floor, active change and observed flag in the same transaction as the status rows that justify them. Stage 4 runs after the commit, so an entry marked while resident is evicted only once its marked record is the committed one (Sol r2's B110 → A120 case, A-35).

**Node instance** (D34, DEC-132). The node has no per-step store commit: its dirty entries wait for CH2's B1 flush (P10), which, like Core's `FlushStateToDisk`, is not tied to a tip step. Stages 2, 3 and 5 are therefore empty for a node step, and the recipe reduces to compute → publication. Publication is per step in ABC and `InvalidateBlock` (`chainlock/handler.cpp:299-309`). An `enforce_block` is one step: its disconnect steps, each with its `IF_NEEDED` check and its event handoff, and its one snapshot store all happen under the single tip-mutex hold above (R4-M2, DEC-144). DEC-132 is unchanged, and there is still no per-step disk commit.

- `connect_tip(id)` requires `prev(id) == tip`. `disconnect_tip()` requires a tip above the floor of the store (anchor or genesis). Both are node-only entry points.
- `init_active_tip(id)` (Core `LoadChainTip`) builds the active chain and publishes `tip_seq = 0` with no event.
- `publish_status()` (node; public for CH1-node's `accept_header` path, O-m9) publishes a snapshot with the same tip and `tip_seq`, epoch + 1, after a best-header, floor or spork-state change. It also takes the tip lock, so epochs are totally ordered. In SPV the status changes ride on the step's single snapshot.

### 9.2 `TipCell` core (SB4) and SB5's extension

```rust
pub struct TipCell<E> { state: Mutex<TipState<E>>, published: ArcSwap<ChainReadSnapshot> }
struct TipState<E> { tip: BlockRef, next_seq: u64, snapshot: Arc<ChainReadSnapshot>, epoch: u64,
                     _feeds: PhantomData<E> /* SB5: feeds, pending, producer_wake */ }
```

SB0 §6.1 holds the tip lock across `subscribe_at`'s `finish()` (DEC-96). Readers therefore load `published` instead of taking the lock, so a durable handoff never stalls `ChainAccess::snapshot` on the P2a actor (D6). `published` is written only inside the critical section, so F2 still holds, and any reader sees a snapshot no older than the last completed step. **`FlushCommit`'s `(new, tip_seq)` comes from the freeze, not from the published snapshot** (rev6, Sol r6 R6-m1, DEC-146; SB0 erratum E4). At each freeze, V captures into CH2's `FreezePacket` the matching pair from `TipState`, under the guard it already holds: the private tip as `new`, and the `seq` of the event that made it the tip, which is the last assigned `seq`, as `tip_seq`. W takes both only from that packet, and `FlushCommit { new, tip_seq }` repeats them. Outside a hold the pair equals the published snapshot's. Inside `enforce_block`'s hold the published snapshot still shows the pre-enforcement tip at `seq` q, while an intermediate freeze captures, for example, B109 at q + 1. Copying the snapshot's `seq` would misclassify CH2's case A/B test, and W taking the mutex to read `TipState` would break H1. Which block `new` names at the `DisconnectTip` site, relative to that disconnect's tip step, stays CH2's existing rule; the pair only has to match.

### 9.3 R1–R8

| R | How SB4 meets it |
|---|---|
| R1 concurrent append-only hash index | §3.5; entries never deleted in the node |
| R2 immutable `prev`/`skip`/`hash`/`height`/`time`/`bits`/`chain_work`; status seqlocked | §3.3, §13.2 |
| R3 `TipFacts` + `ActiveChainView` in one snapshot, published on every tip and best-header change, before the reply, unthrottled | §9.1. Node: `accept` publishes before returning `HeadersResult.snap`. SPV: the step's single snapshot is published before the driver replies with `TipChange.snap`. A1's 100 ms throttle stays on CH1-node's full `ChainView` only |
| R4 lazy latched IBD | `ChainReader::is_initial_block_download` (§9.4) |
| R5 `best_header` non-null | §6.1 |
| R6 V services its queue between ABC steps | CH1-node. SB4's part is step granularity: `connect_tip`/`disconnect_tip` are single steps, and none holds a lock across them. The exception is `enforce_block`, which is not an ABC step and holds the tip mutex throughout, as Core holds `cs_main` across `EnforceBlock` (R4-M2) |
| R7 `HeaderBatch` + debug-only hash check + `HeadersResult` | §4, §5.1. `HeadersResult` gains `stop` and `floor_moved` (D17) |
| R8 one coherent retained snapshot | `Arc<ChainReadSnapshot>`; `ActiveChainView` holds its own root; never reads the live chain |

### 9.4 IBD latch

Port of `IsInitialBlockDownload` (`validation.cpp:1970-1993`), against one snapshot (R4): latched → false; `loading_blocks` → true; tip null → true (cannot happen after construction); `tip_work < min_chain_work` → true; `tip_time < now − max_tip_age` → true; otherwise set the latch (`AtomicBool`, Release) and return false. `now` is `NodeClock` (mockable), as in Core. The latch lives in `Shared`, one per chain.

## 10. Readers, locator, chain tips, persistence

### 10.1 `IndexAccess`, `ancestor`, `lca`, locator

- `ancestor` is `GetAncestor` over skip pointers (`chain.cpp:84-119`). `lca` is `LastCommonAncestor` (`chain.cpp:166-181`), returning `None` when the inputs share no resident or cold ancestry, which cannot happen in the node.
- `locator(Some(id))` is `LocatorEntries` + `GetLocator` (`chain.cpp:32-54`): ten single steps, then doubling, then genesis; in SPV, the anchor, which is the lowest accepted entry and lies on every chain the client accepts (n6). In SPV the walk keeps Core's density down to the anchor, reading cold ancestry below the window (D-C4-12), and appends the anchor unless it is already last (D-R2-A-1; DEC-140). `locator` stays infallible (a read failure trips the latch and answers empty, below); `try_locator` returns the `Result`. `locator(None)` is empty under V24_0 and the active tip's under V23_1 (`P2pRules.null_locator`, P2a G2).
- **SPV and the latch** (D7): a cold read inside an infallible `IndexAccess`/`ChainReader` method that fails trips the latch and answers `None`. The client is stopping (P6), so the answer is never acted on. Callers that must tell `BelowFloor` from absence use `AnchoredAncestry`.

### 10.2 `chain_tips()` (getchaintips over the header index)

Core (`rpc/blockchain.cpp:1827-1943`; statuses `:1914-1936`) builds `setOrphans` (entries not on the active chain) and `setPrevs`. The tips are the orphans that are no orphan's prev, plus the active tip. That set equals **leaves ∪ {active tip}**: a non-active entry's children are all non-active, so "no orphan has it as prev" means "no child".
- **Node**: iterate `leaves`, O(branches).
- **SPV**: resident leaves (§3.7) ∪ `HeaderStore::leaves()` (DEC-123 Q7), deduplicated by hash, with statuses read through the §3.7 precedence. A store error trips the latch and returns `ChainError::Storage`. Reading the store also refreshes `max_cold_leaf_work`.
- `branchlen = height − find_fork(tip).height`.
- `status` follows Core's order: `active` / `invalid` (FAILED) / `conflicting` (CONFLICT) / `headers-only` (`nChainTx == 0`) / `valid-fork` (`VALID_SCRIPTS`) / `valid-headers` (`VALID_TREE`) / `unknown`. `nChainTx` is CH1-node's, so the node passes it through a hook. In SPV every non-active tip is `headers-only` or `conflicting`.
- Order is `CompareBlocksByHeight` (`:1745-1757`: height descending, then address → id). The equal-height order is D9's observable difference; comparisons sort equal-height tips by hash.
- **Oracle comparison** (Sol r1 M6, Opus O-m1; D40): `status` is compared with block-data-dependent values projected away. `headers-only`, `valid-fork` and `valid-headers` all map to `header-valid`, because a header-only index cannot know what Core downloaded.
  - **Harness scenarios** (`pnbh`, `cl`, `invalidate`, `reload`) construct every FAILED and CONFLICT bit they hold, so there `active`, `invalid` and `conflicting` are compared exactly.
  - **A-4's live-oracle replay** accepts headers with no blocks and no locks, and a live node's `invalid` comes from block validation and its `conflicting` from its own ChainLock history. A-4 therefore also projects `invalid` and `conflicting` to `header-valid`, and SB4 must report every non-active tip as `headers-only`. `active` is compared exactly. Exactly comparable there: the tip set, heights, branch lengths, the active tip and its chain work.

### 10.3 `BlockIndexRecord` codec, `take_dirty`, `load`

- **Codec** (A1 §2.9 v1): key = hash (32 B). The value is `VARINT(height)`, `VARINT(status)`, `VARINT(n_tx)`; `VARINT(file)` if `HAVE_DATA|HAVE_UNDO`; `VARINT(data_pos)` if `HAVE_DATA`; `VARINT(undo_pos)` if `HAVE_UNDO`; then the 80-byte header. This is Core's `CDiskBlockIndex` field order (`chain.h:391-413`) without the client-version prefix and without the inline `hash`, which is the key. Core's VARINT (`serialize.h` `WriteVarInt`, `NONNEGATIVE_SIGNED` for height and file). No per-record version: `DbSchema` versions the column family (D16). The tests are round-trip proptests plus vectors cross-checked against Core's `CDiskBlockIndex` serialization with the client-version prefix and hash stripped (§15).
- **`take_dirty()`** drains `dirty: Vec<BlockId>` (deduplicated by a per-entry dirty bit) and encodes each record at take time, from one seqlocked read. CH2's B1 writes them (P10). **`take_dirty_records()`** (SPV) returns `HeaderRecord`s with `kind`.
- **`load`** ports `LoadBlockIndexGuts` (`txdb.cpp:300-344`), `BlockManager::LoadBlockIndex` (`blockstorage.cpp:268-331`) and the best-header part of `ChainstateManager::LoadBlockIndex` (`validation.cpp:5337-5361`):
  1. Read records in batches of 65,536 from the iterator and decode each batch in parallel in `pool.install` (`par_iter`). Check `check_proof_of_work(stored_hash, bits)` against the **stored** hash (the key), as Core does (`txdb.cpp:328-331`), with no X11. A failure → `LoadError::HighHash`.
  2. Assign ids in iteration order **on first mention**, hash then prev, as `insertBlockIndex(hash)` then `insertBlockIndex(hashPrev)` allocate (`:314-315`). This makes the arena-id tie-breaks (D9) mirror Core's allocation order.
  3. A prev hash mentioned but never defined → `LoadError::MissingParent` (D21). The null prev is genesis; it must match `params.genesis_hash`, else `GenesisMismatch`.
  4. Sort ids by height. A gap → `NonContiguous` (`blockstorage.cpp:297-300`). In height order compute `chain_work`, `time_max`, `mtp`, `skip`, set each `imm`, and propagate `FAILED_CHILD` to children of FAILED entries, marking them dirty (`:321-324`). `nChainTx` and `m_blocks_unlinked` are CH1-node's (`:305-320`).
  5. Recompute `best_header` over `is_valid(VALID_TREE)` entries with the comparator (§6.1), iterating in height order as `:5341-5361` does.
  6. Populate `leaves`, `links` (§3.7) and the checkpoint cache. The tip is set later by CH1-node through `init_active_tip` (Core `LoadChainTip`).
- **`new_genesis`** inserts the genesis entry with `VALID_TREE` and marks it dirty. CH1-node's `ReceivedBlockTransactions` raises it.

## 11. Anchor and checkpoints (SB0 §4.3, §4.3.1 steps 1–2)

### 11.1 Pack check (`new_anchored`)

`AnchorPack.headers[24]` must X11-hash to `hash`, and each `headers[i]` must hash to `headers[i + 1].prev_hash` (hash-linked). `height` must be at least `dgw_height + 24`, so every header from anchor + 1 is in the DGW era and R = 25 (SB0 §4.3, post-DGW only). `chain_work` is absolute. Context entry k (anchor − k) gets `chain_work − Σ_{j<k} block_proof(bits(anchor − j))`, derived downward from the trusted absolute work one subtraction at a time with **checked** subtraction (Sol r1 m3): each step compares first, and `work <= proof` → `AnchorError::Work`, before any context work is assigned. `Arith256`'s `Sub` wraps like Core's `arith_uint256`, so an unchecked chain would turn an underflow into a large positive work. A-32 covers a low-absolute-work pack that would underflow, and asserts `work(child) − work(parent) == proof(child)` for every pair. This verifies the supplied pack; it adds nothing to the live chain-work arithmetic, which stays Core's `chain_work(prev) + GetBlockProof` (`chain.cpp:132-145`). Ids: the 24 context entries first (lowest height first), then the anchor, from `next_id`. The anchor is `VALID_TREE`, accepted, the initial `best_header`, the active tip (`init_active_tip`) and the fork target. Every pack entry is dirty, so the first WriteSet holds the 25 records.

### 11.2 Candidate (step 1)

`anchor_candidate` picks the newest pack with `height ≥ dgw_height + 24`, `height ≤ birth` (when `birth` is set), and `time(anchor) ≤ now − 2 · history_blocks · pow_target_spacing`, where `history_blocks = L_est + H_keep` is passed in by the host (`dash-mnlist` computes it, SB11). No such pack → `Genesis`. The birth-height resolution itself stays in dash-spv (`resolve_start_height`, SB15).

### 11.3 Fallback (step 2)

`anchor_fallback(current, miss)`:
- `LowestUncovered(h)` → the newest pack at or below h;
- `UnknownBase` → the newest pack strictly below `current`;
- either → `Genesis` when none.

The function never returns a pack above `current`. No `HeaderChain` method changes `anchor`: it is set at construction and read-only afterwards. The driver clears the header store and constructs a new chain (SB15). That is the "no API moves an `Established` anchor" property, checked by a compile-fail doc test plus an API audit test (§15).

### 11.4 `CheckpointView` (DEC-112 n2)

In both products the list is Core's chainparams list, `params.checkpoints` (39 mainnet entries), never dash-spv's anchor packs (n2).
- `highest_checkpoint_in_index()` = Core `GetLastCheckpoint` (`blockstorage.cpp:429-441`): the highest-height checkpoint whose hash is in the index, or `None` when `!checkpoints_enabled`.
- It is cached and updated in `add_to_block_index` and `load` (§5.4). Entries never leave the index (SPV's leave the arena only, not the store), so the cache equals Core's reverse scan.
- **SPV reopen (O-m7).** SB5's `open` rebuilds the cache with one `HeaderStore::by_hash` per checkpoint, highest first, stopping at the first hit. Without it a genesis-anchored SPV chain (regtest, devnet, a testnet client without a pack) would reopen with the rule silently off.
- An anchored SPV chain may hold a checkpoint hash: the anchor or a pack context header can itself be a Core checkpoint. The rule is still inert there, because every new header is above the anchor and a fork below the anchor is `BelowAnchor`.

## 12. Deployments in the header path

`HeaderDeployments` implements `DeploymentView` for headers. `active_after(Some(prev), Buried(d))` = `prev.height + 1 >= params.buried_height(d)`. Versionbits deployments are unreachable from `contextual_check_block_header` (CS3 queries buried ones only); `active_after(.., VersionBits(_))` returns false with a `debug_assert!` (D14). Block-level deployments are CH1-node's `DeploymentView`.

## 13. Concurrency model

### 13.1 Who does what

| Actor | Holds | May call |
|---|---|---|
| **Writer**: V (node) or the SPV header task | `&mut HeaderChain` (exactly one) | everything in §4 |
| **Readers**: P2a actor, RPC, CH2 driver threads, SPV subscribers | `ChainReader` clones | `ChainReader`, `IndexAccess`, `AnchoredAncestry` via the reader |
| **Load workers** | borrowed `&[u8]` slices inside `pool.install` | decode + PoW check only; no shared state |

| Lock | Kind | Holder | Held across |
|---|---|---|---|
| hash index | `parking_lot::RwLock` | writer: write per insert; readers: read per lookup | nothing; it is a leaf lock |
| tip lock | `parking_lot::Mutex` (`TipCell::state`, an `Arc` so `enforce_block` can hold an owned guard) | writer in `step`/`publish_status`/`enforce_block`; SB5's `subscribe_at`/`deregister` | the step's critical section; SB5's `finish()` (DEC-96); the node's whole `enforce_block`, including V's `DisconnectTip` work, CH2's `IF_NEEDED` check with its CRITICAL wait for W's release, the `pending` drain and `conflicting_marked` (R4-M2, DEC-144, §13.4) |
| CH2b's flush writer W (freeze → release) | CH2b | W only | `BlockFiles` and the store's own locks; reads `closing` flags as atomics; **never** the tip mutex, the feed/hub lock, or any lock V may hold during enforcement (§13.4 H1, H7) |
| entry seqlock | per-entry `AtomicU32` | writer only | one field-group write |
| arena directory | `OnceLock` / `ArcSwapOption` per slot | writer initializes; readers load | nothing |

Lock order: the writer takes the index **write** lock to insert, which is outside the tip lock, and in SPV eviction, which is part of stage 4 and so runs **under** the tip lock (D-A11-3: stage 4 runs immediately before the snapshot store, and eviction joins it). The order is therefore tip lock, then index lock; no path takes them the other way round. Inside `enforce_block` the writer and V may take index **read** locks under the tip lock. Readers hold at most one lock, and the only index writer is the writer thread, so no cycle can form. Under the enforcement hold, V's only cross-thread waits are W's release and subscribers' pops (§13.4). Neither W's path from freeze to release (H1, H2) nor a subscriber's path from event to ack and pop (H4, H6) takes the tip mutex, the feed/hub lock, or a lock V holds while it waits (H3). A subscriber blocked in `deregister` on the tip mutex is no edge either: its `closing` flag, set before it blocks, has already retired it from V's `pending` wait (H4) and from every W wait (H7). So the wait-for graph has no edge back to V. There is no lock between the writer and the snapshot readers.

**Publication.** A new entry's `imm` is set and its status stored before its id is inserted into the hash index (Release by the `RwLock`). An id handed to a reader through a snapshot was published before the snapshot's `ArcSwap` store (Release). A reader that holds an id therefore sees its immutable fields.

### 13.2 Seqlock protocol

Writer (single):

```text
s = seq.load(Relaxed); seq.store(s + 1, Relaxed); fence(Release)
store fields (Relaxed)
seq.store(s + 2, Release)
```

Reader:

```text
loop { s1 = seq.load(Acquire); if s1 odd { spin_loop; continue }
       read fields (Relaxed); fence(Acquire)
       if seq.load(Relaxed) == s1 { return } }
```

This is the standard fence-based seqlock over atomics, so it is data-race-free in the Rust memory model. Tests: a stress test with tuple-consistency invariants under `--release`, and a `loom` model of one writer and two readers (feature `loom`, CI only).

### 13.3 Eviction (SPV)

Eviction is the last part of `set_active_tip`, stage 4 of §9.1's recipe (SB0 §4.4, m2). Every entry below `tip − w` leaves: its hash leaves the resident index under its write lock first, then the entry's evicted bit is set (D-C4-9: the window's edge does not fall on chunk boundaries, so eviction is per entry). A chunk slot is replaced with `None` when its last live entry has left. A reader holding a chunk guard keeps reading it, and a reader arriving later resolves the id cold. The writer keeps naming some entries after they leave: the best header and the ChainLock floor carry the facts the snapshot shows (D-C4-12), and tip plans read their cold legs from the store and step them with their refs. Leaves that leave the window fold into `max_cold_leaf_work` (§3.7), and the `RuleContext` cache's `residency_epoch` is bumped (§8.2). The overlay and `pending_new` are writer-only and take no lock.

### 13.4 Requirements under the node's enforcement hold (DEC-144; Sol r5 R5-M2)

Core runs `FlushStateToDisk(IF_NEEDED)` inside every `DisconnectTip` under `cs_main`, so SB4 keeps CH2's pressure check, and its CRITICAL wait, on every disconnect inside `enforce_block`'s hold (§9.1). That wait completes only if nothing it waits on needs the tip mutex. The requirements below are binding inputs for the named designs; CH2b's design must state H1–H3, H6 and H7 as its own and run A-40 and A-41 against the real W. Core's model is the same: a flush never waits on an index, and a stopping index simply unregisters (`BaseIndex::Stop`, `index/base.cpp:419-426`).

| # | Requirement | Owner | Test |
|---|---|---|---|
| H1 | **W's lock order.** On its path from freeze to release (CH2 §5's steps 1–11, the rewind gate and the prune wait included), W acquires neither the tip mutex nor the feed/hub lock nor any lock V may hold during enforcement. It takes only `BlockFiles` and the store's locks, which V never holds while it waits | CH2b | A-40, A-41 |
| H2 | **W's durable barrier goes through a W-owned channel.** W writes step 5's `T_ack`, `FlushCommit { flush, new, tip_seq, to }` (with `(new, tip_seq)` from the `FreezePacket`, §9.2) and step 3b's rewind gate into a channel W owns. The captured Synced set `to` and the gate's target set come from a registry snapshot that the hub republishes lock-free on every `register`, `subscribe_at` and `deregister`. Registration is stable during a hold, because those calls block on the tip mutex, but `closing` is not: every capture and every gate excludes slots whose `closing` flag is set, and a target that closes later is retired by H7. Subscribers read the commit and the gate from W's channel and ack to W directly. V drains the channel after the hold and enqueues the control mirrors (`FlushCommit`, `ChainStateFlushed`) into the feeds in F7's order; the queued copy stays a no-op (CH2 rule 1) | CH2b, with SB5's hub | A-40 |
| H3 | **V's waits under the hold.** While V waits for W's release under the guard, it holds only the guard and its exclusive `&mut HeaderChain`, and it services W's `PruneClears` requests (status writes need no tip lock). An automatic prune selection may also stay armed, as CH2 already allows | CH1-node, CH2b | A-40 |
| H4 | **Feed progress takes no tip mutex.** `pop`, `remove_round_trip`, `set_waker` and the space wake take no tip mutex. `deregister` first sets the slot's atomic `closing` flag (the retirement point, H7), wakes the producer and W, and only then takes the mutex. Under its guard, V drops a closing slot from `pending` and enqueues nothing more to it, so F11 still holds. The events it drops are not preserved: no W obligation can depend on them (H7) | SB5 | A-40 |
| H5 | **Events at their step.** Each disconnect's event reaches the live feeds or `pending` at its step, under the guard (D43). Subscribers can therefore reach any F's `new` (case B) and process any rewind without V releasing the mutex | SB4 | A-26, A-41 |
| H6 | **Subscriber progress.** A **live** (non-closing) subscriber's path from event to ack (process, commit its own data, read W's channel, ack) takes no tip mutex and waits on nothing V does under the hold. A closing subscriber has no such path and needs none (H7); it makes no further durable commit, so its durable locator does not advance | CH2b, SB5 subscribers | A-40 |
| H7 | **Closing retires a subscriber's flush obligations at once** (rev6, Sol r6 R6-M1, DEC-146; as Core never makes a flush wait on a stopping index). The successful `closing` store (`SeqCst`, monotone: never cleared, and a reused slab slot gets a new generation) is the single linearization point. At it, the slot leaves W's ack set and rewind-gate target set, for barriers and gates W captured **earlier**, including ones it is waiting on now, and for every capture or gate W creates **later** during the hold.<br>**The linearization rules:**<br>(a) W's wait predicate for a captured set T is `∀ s ∈ T: acked(s) ∨ rewound(s) ∨ closing(s)`. W registers its waker, then re-checks the predicate, before each park and on every wake, and deregistration wakes W after setting the flag. A target that closes while W waits is satisfied, with no lost wake-up.<br>(b) Step-5 capture and gate creation read the registry snapshot, then filter on `closing`. A flag set before the filter excludes the slot; a flag set after it is retired by (a). Either order gives the same obligation set.<br>(c) An ack or rewind report from a closing slot is ignored.<br>**What it keeps:** the subscriber's **prune lock** stays at its last durable locator until deregistration completes after the hold. Every prune selection, including those of freezes made during the hold, still includes that locator in `min over mandatory subscribers of height(LCA(L, new))`, so pruning never passes it and Core's `BaseIndex::Init`-style resume from `FindForkInGlobalIndex` (`index/base.cpp:72-84`) stays possible after a crash. The durable locator is not advanced (H6), and the lock is released only by F11's completion, after the hold | CH2b (W's predicate, capture, gates, prune bound), SB5 (flag, wakes) | A-40 |

## 14. Error model

| Error | Raised by | Meaning | Caller's action |
|---|---|---|---|
| `HeaderStop::Invalid` | `accept` | Core's `BlockValidationState` (result, reason, debug) | P2a/SPV: Core's misbehaviour mapping |
| `HeaderStop::BelowAnchor` | `accept` (SPV) | fork below the anchor, genesis, or a context parent | ignore, no blame |
| `ChainError::Storage` | any cold read (SPV) | the latch has tripped (`FailStop::read_failed`) | stop the client (P6); nothing published |
| `ChainError::Internal` | `RuleContext` envelope miss, invariant breaks | a bug; debug builds panic first | no blame; node: fail-stop through the host |
| `ChainError::ConflictsWithFloor` | `set_active_tip` (SPV) | target conflicts with `cl_floor` while observed (is neither the floor, an ancestor nor a descendant; DEC-136) | a bug in the driver; debug assert |
| `ChainError::UnknownBlock` | tip writers given an unknown id | a bug | — |
| `ChainError::Config` | constructors | `window` in node mode, V23_1 without `AdjustedTime`, … | refuse start |
| `ChainError::Anchor` | `new_anchored` | pack not hash-linked, pre-DGW, bad work | try the fallback pack |
| `LoadError::*` | `load` | corrupt or inconsistent index | CH2 recovery reports it; the node refuses to start (Core's `LoadBlockIndex` false) |
| `AncestryError::BelowFloor` | `AnchoredAncestry` | history not held | an ordinary answer (`Unavailable` upstream) |
| `ApplyError::Host(E)` | `apply_chainlock`/`enforce_best_chainlock` | V's `DisconnectTip` failed | node: Core asserts (`:4067`) → fail-stop |

Rule errors never become `Err`, and `Err` is never blame.

## 15. Test plan

Fixtures:
- **H-M/H-T**: `/work/scratch/cs2/headers/{mainnet,testnet}.bin`, raw headers in height order (mainnet 2,551,766). Copied under `dash-chain/tests/data/` as a fetch script plus SHA-256, not checked in.
- **Oracle RPC**: `/work/chains` v24 rc.3 (27998 mainnet, 37998 testnet) for `getblockheader.chainwork` and stale-branch headers only. A live node's `getchaintips` is not an oracle for a header-only index (Sol r1 M6); A-4 projects statuses, `invalid` and `conflicting` included (§10.2, O-m1).
- **CS3 rows are rule-level controls, not acceptance vectors** (Sol r1 M6). The recorded header rows (`node/cs3-checks` @ 7c333502, `tests/data/checks_recorded.json`) judge `ContextualCheckBlockHeader` over a constructed window, with no PoW and no index state. Five of the ten are masked by `high-hash` under full acceptance. They are replayed through `dash_consensus::contextual_check_block_header` with the window built by SB4's `RuleContext` (the CS3 harness's `Input::ContextualHeader` arm, `tests/support/checks.rs:603-608`, with SB4's context in place of the row's flat window). The oracle is the recorded verdict; a row added by SB4 is judged by O2a's **`dash_oracle_sys::Oracle::contextual_check_header`** (`dash-oracle-sys/src/context.rs:449`).
- **`sb4-core-scenarios`** (D22; built by a separate parallel task, DEC-123 Q3). It produces every **acceptance** verdict below.
  - **Build** (Sol r1 m4). Neither `build-oracle.sh` (tests disabled) nor the BS5 tool (`dash-tx` only) yields a linkable `RegTestingSetup`, and the O1 archive localizes Core's symbols. The task builds Core at the pinned shim commit with tests enabled, links the unlocalized server libraries plus `test/util` objects (`setup_common`, `mining`, `validation`), and records provenance: Core commit, shim commit, configure flags, SHA-256 of each archive. A smoke test runs first: one PNBH batch; through `cl`, the **fresh-enabled first lock** (observed false, no CL, SPORK_19 active: the first check resets the empty state, A100 is installed, and the scheduled check keeps it) and the **off → on lower-height lock** (height 100 recorded while off through `submitchainlock`, the only path that records while off, then spork enabled and a lock at 90 accepted because the initial check clears 100), each emitting best CL, floor, observed flag and statuses; one reload; one codec dump. The full vector set is promised only after it passes. No MN-chain fallback is needed: Core's `RegTestingSetup` already toggles SPORK_19 with the regtest spork key (`test/llmq_chainlock_tests.cpp:393-401`, `:462-464`; `test/util/setup_common.h:110-114`).
  - **Modes** and their per-step JSON:
    - `pnbh`: a pre-state script (accepted headers, `InvalidateBlock`/`MarkConflictingBlock` seeds, `m_failed_blocks` seeds), then `ProcessNewBlockHeaders` on a batch. Emits the result class, `GetRejectReason()`, `GetDebugMessage()`, the last accepted hash, the status of every touched entry, `m_best_header`, `GetLocator(m_best_header)[0]`, the projected `getchaintips` (§10.2), and the block-tree records written, as the diff of the block-tree DB around `ForceFlushStateToDisk`.
    - `cl` (receipt of a lock; DEC-128). Core's `ProcessNewChainLock` order (`chainlock/handler.cpp:133-183`) with **only** signature verification bypassed: `chainlock::VerifyChainLock` is replaced through the O2a `--wrap` seam to return `Valid`. The seam wraps the four-argument overload `(Consensus::Params const&, CQuorumManager const&, ChainLockSig const&, CBlockIndex const*)` called at `handler.cpp:156` (declared `chainlock/clsig.h:69-70`, defined in `chainlock/clsig.cpp:263-269`, so it is an external reference). The harness links the unlocalized Core objects, not the localized oracle archive (Sol r3). The steps run in Core's order: initial `CheckActiveState` (`:136`); the seen-hash and height pre-check (`:140-151`); `LookupBlockIndex` and `UpdateBestChainlock` (`:166-168`, `chainlock/chainlock.cpp:106-132`); then the scheduled `CheckActiveState` and `EnforceBestChainLock` (`:170-176`), which the harness waits for by draining the handler's scheduler. Spork toggles are signed with the regtest key. It emits, after every step: `bestChainLock`, `bestChainLockBlockIndex` (the floor), `isEnabled` (the observed flag), the statuses of touched entries, the tip trace and `m_best_header`.
      - **rev3: the `EnforceBlock` boundary** (Opus O-m2). The harness also wraps `Chainstate::EnforceBlock(BlockValidationState&, CBlockIndex const*)` through the same `--wrap` seam: it is called from `chainlock/handler.cpp:296` and defined in `validation.cpp`, so the reference crosses object files. The wrapper calls the real function and, on return and before `EnforceBestChainLock`'s `ActivateBestChain` (`:302`), emits the statuses of every entry, the tip and `m_best_header`, under `cs_main`. SB4's node rows compare at this boundary; the post-ABC emission stays for the floor, the tip trace and SPV's rows.
      - **rev3: the signal trace** (Opus O-M2). A `CValidationInterface` registered before the scenario records, in order, `SynchronousUpdatedBlockTip(new, fork, ibd)` (called synchronously under `cs_main`) and the queued `UpdatedBlockTip(new, fork, ibd)` and `BlockDisconnected(block)`. The queued signals run on one serial scheduler queue, so their relative order is Core's issue order (`validation.cpp:3326`, `:4141-4142`). Fork arguments are emitted as a hash or `null`. (rev7.1, DEC-147) The listener delivers Core's real callbacks under a controlled, recorded schedule, and each vector's provenance names that delivery policy; the recorded trace is one schedule's sample, judged under §15's comparison rule.
      - **`cl via`** (rev7): `p2p` (the default) applies `net_processing.cpp:5794-5795`'s live gate first, so while SPORK_19 is off the lock is reported `dropped: spork19-off` and nothing runs; `submitchainlock` enters `ProcessNewChainLock` as `rpc/quorums.cpp:1397` does, with no gate.
      - **New scenario `a29-x-kept-mark-trailing`** (rev7, A-29 (x)): build A1…A80 connected; A81…A95 connected (A active); B81…B110 from A80 as headers; spork on, `timer`; `cl` B100; spork off, `timer`; spork on; `cl` A90. Expected by reading Core, with the harness as judge: after B100, A81…A95 status 157, tip A80, `best_header` B110, floor B100; after A90, B81 status 130, B82…B110 2, A81…A95 still 157, tip A80, `best_header` B110, floor A90, observed true. It emits the `EnforceBlock` boundary for both locks.
      - **New scenario `a29-xp-kept-mark-headers-only`** (rev7.1, A-29 (x′), Sol r7 R7-m1): as `a29-x-kept-mark-trailing`, but A81…A95 are submitted as headers. Expected values: after B100, A81 is 130, A82…A95 are 2, the tip is A80, `best_header` is B110 and the floor is B100. After A90, A81 is still 130, B81 is 130 and B82…B110 are 2, the tip is A80, `best_header` is B110, the floor is A90 and observed is true.
    - `seed-cl`: a direct `Chainlocks::UpdateBestChainlock` call, named as a **seeding operation**. It builds pre-states only, and is never the oracle for receiving a lock.
    - `notify`: `UpdatedBlockTip` with chosen `(new, fork, ibd)` arguments, including `(tip, nullptr, false)`, followed by a scheduler drain; it emits `isEnabled`, the floor and statuses (A-29 (viii)).
    - `reload`: flush, restart the chainstate from the same datadir, emit the loaded `m_best_header`, statuses and tips. An equal-work, equal-`nSequenceId` tie is emitted as `address_tie` with the tied entries (rev7).
    - `invalidate`: `InvalidateBlock`/`ReconsiderBlock`, emitting the same fields.
    - `locator`, `diskindex` (codec vectors), `bench-pnbh` (§16).
  - **Networks.** Regtest is primary: headers are mined trivially, and `fPowNoRetargeting` (`chainparams.cpp:836`) makes `bad-diffbits` reachable with mined headers whose bits are harder than expected. Anchored SPV vectors mine past `dgw_height + 24` = 34,164 headers (seconds at regtest difficulty). `bad-fork-prior-to-checkpoint` uses a regtest checkpoint patched in through private access. Devnet covers `nMinimumDifficultyBlocks`. Testnet uses the H-T prefix plus mined min-difficulty candidates. Mainnet uses real headers only: valid batches, duplicates and `bad-chainlock` on real stale branches with a bypassed CL. Mainnet-only rules that need mainnet-difficulty mining (the 68,589 tolerance, KGW invalid targets) stay rule-level (A-1, A-6, A-8).
  - **Node scenarios keep non-floor branches data-less** unless a row says otherwise (Opus O-m2): Core's `FindMostWorkChain` writes `CONFLICT`/`FAILED_CHILD` on downloaded candidates (`validation.cpp:3548-3570`), which is CH1-node's work, not SB4's. A row that needs such a branch compares SB4 at the `EnforceBlock` boundary only.
  - **Comparison** (rev7.1, DEC-147, D46; DEC-167). The vectors are **state oracles**, asserted at Core's **fixed point**.
    - **Exact:** result class, reason, debug string, last accepted, statuses, tip, `best_header`, floor and records, except the D9 effects, which are normalized (§18 D9). No vector asserts an `address_tie` winner.
    - **Exact trace prefix:** for each lock, the signal trace up to and including its **first** enforcement: the `EnforceBlock` boundary and the disconnect/connect order of the `ActivateBestChain` that follows it. A lock already on the active chain emits no qualifying notification and has exactly one pass; a lock on an unknown header has none.
    - **Fixed point** (DEC-167, replacing DEC-147's idempotent repeats). SB4's `enforce_block` is Core's single pass exactly, with no fixpoint shortcut inside it. After the prefix, and before the next scripted operation, the test driver re-runs enforcement as dashd does (the rescheduled update and the timer) until a pass changes nothing: no status, tip, `best_header`, floor, record or dirty-set change and no disconnect or connect. The exact assertions above apply to that fixed point. A repeat is **not** always a no-op: Core's pass resets failure flags only when the locked block itself is flagged (`validation.cpp:4075`), so pass 1 can leave a branch that pass 2 reconnects. A state after an intermediate pass is a transient, recorded but not asserted, and neither the vector's pass count nor SB4's is asserted. A schedule whose fixed point differs is a finding.
- **`ScriptedV`** (test-support) replays node events from the harness's tip trace (`ConnectTip`/`DisconnectTip` order) through `connect_tip`/`disconnect_tip` and calls the CH1-node hooks where the trace shows them. It never manufactures an expected verdict. It compares under the comparison rule above (rev7.1, DEC-147; DEC-167). Up to and including each lock's first enforcement and its ABC, SB4's `disconnect_tip` requests **equal** the trace's `BlockDisconnected` sequence (n9), and its `conflicting_marked` calls, with `host.tip()` at each, equal the trace's `(tip, null)` `UpdatedBlockTip` entries in position (O-M2). After that prefix, `ScriptedV` skips the trace's later passes, drives its own passes until one changes nothing (DEC-167), and compares the state at that fixed point; it never asserts a hook count, or a state, between the prefix and the fixed point.
- **`StaticSporks`** with a settable value drives the SPORK_19 transitions on the Rust side, mirroring the harness's signed toggles.

### 15.1 SB4 row acceptance → tests

Each row names the oracle that produces its verdict. "Harness `m`" is `sb4-core-scenarios` in mode `m`; "—" means a property or a self-consistency check with no Core verdict.

| ID | Acceptance item (PLAN:747) | Test | Oracle |
|---|---|---|---|
| A-1 | Core L0 header vectors reject with Core's strings, from genesis | **rule level**: `rules_cs3_header_rows_genesis`: every CS3 contextual header row through `contextual_check_block_header` over SB4's `RuleContext` on a node chain (window inserted with `test_support::insert_unchecked`) | CS3 recorded verdicts; added rows: `Oracle::contextual_check_header` (O2a) |
| A-1a | … and through full acceptance | `accept_pnbh_vectors_genesis`: harness vectors replayed through `accept` on a node chain, comparing every emitted field | harness `pnbh` (regtest, devnet, testnet, mainnet as listed above) |
| A-2 | … and from an anchor | **rule level**: `rules_cs3_header_rows_anchored`: rows whose window is post-DGW, with the window as the pack and context, at w = 10,000 and w = 1,000. Pre-DGW rows are node-only (listed). **Acceptance**: `accept_pnbh_vectors_anchored`: the regtest vectors re-mined above an anchor at ≥ 34,164 | CS3 recorded verdicts; harness `pnbh` on the re-mined chain (new Core verdicts, never the pre-rebase ones) |
| A-3 | `time-too-new` with ±3,600 s peer offsets and `setmocktime` | `time_too_new_nodeclock`: candidates at now + 7,200 and + 7,201 under `MockClock`, with an `AdjustedTime` stub at +3,600, −3,600 and none that is never consulted under V24_0; then `setmocktime` moves the boundary | regtest dashd: `dde-probe` opens 5 raw peers whose `version.nTime` is skewed ±3,600 (enough samples for `AddTimeData`), then `submitheader` at the same offsets, with and without `setmocktime`; verdicts recorded as vectors |
| A-4 | full mainnet/testnet headers from oracle files, from genesis, windowed and unwindowed: chain work and `getchaintips` equal | `full_chain_{mainnet,testnet}_{node,spv_w10000,spv_w1000}` (`#[ignore]`, CI-nightly): accept H-M/H-T from genesis plus every oracle stale branch with height ≤ the dump tip, then `connect_tip` along the main chain | `getblockheader <tip>.chainwork`; `getchaintips` filtered to tips at or below the dump height, compared as sets of (hash, height, branchlen, projected status). The projection is §10.2's A-4 rule: every non-active status maps to `header-valid` (O-m1) |
| A-5 | BTC retarget at next height 576 (ancestor 0) | `genesis_ctx_btc_576`: **consecutive** accepts 560 … 600 with one warm cache (R grows 11 → 576 at 576 and shrinks after), on H-M, unwindowed, w = 10,000 and w = 300 (cold) | equal verdicts and targets vs `complete()`; H-M's real bits; and at 576 and the boundary heights, `complete()`'s target equals `dash_oracle_sys::Oracle::next_work_required` over the same consecutive raw headers (`dash-oracle-sys/src/lib.rs:714`, Opus O-m6) |
| A-6 | KGW boundary (a read beyond 25), the KGW → DGW switch at 34,140 | `genesis_ctx_kgw_boundary`: consecutive warm accepts 15,190 … 15,210 and 34,130 … 34,150 at the three windows | same, `Oracle::next_work_required` at every height of both ranges |
| A-6a | warm cache across a reorg and an eviction | `ctx_warm_reorg_evict`: a branch change mid-KGW (rebuild), an eviction between two accepts (`residency_epoch` bump, re-read into `cold`), and a near-genesis branch (`saturating_sub`) | `complete()` |
| A-7 | testnet's min-difficulty walk-back | `genesis_ctx_testnet_mindiff` on H-T at the three windows | same, with `Oracle::next_work_required` over H-T's consecutive headers at the walk-back heights |
| A-8 | maximum-length KGW walk: parent 19,999, times 1,000,000 + 150·h, bits `0x1e010000`; 4,032 contributing entries down to 15,968 and the guard read of 15,967, resident and cold | `kgw_max_walk_{resident,cold}`: an arena built with `test_support::insert_unchecked` (no PoW); `get_next_work_required` via `RuleContext` at w = 10,000 and w = 1,000 equals `complete()`'s | the target equals complete ancestry's, and `complete()`'s target equals `Oracle::next_work_required(chain, headers, first_height, new_header)` over the same 4,033 synthetic headers (Core's `GetNextWorkRequired`, O-m6) |
| A-9 | a 4,032-entry context panics in debug (negative control) | `kgw_short_context_panics` (`#[should_panic]`, debug) with R forced to 4,032; and in release, `ChainError::Internal` with nothing inserted | — |
| A-10 | the post-DGW 25-entry anchor control | `anchor_ctx_is_pack`: at anchor + 1 the context is exactly the pack; a 24-entry pack is rejected | DGW target equals H-M's real bits |
| A-11 | a branch forking below the anchor → `BelowAnchor` | `fork_below_anchor_ignored`; plus known-context skip and anchored genesis (§5.3) | — (SPV-only path, D13) |
| A-12 | the candidate cases of §4.3.1 | `anchor_candidate_cases`: tip at exactly the newest checkpoint and at newest + 100 (the time rule picks the previous one); birth-height cut; no pack → `Genesis`; pre-DGW pack never chosen; fallback `LowestUncovered`/`UnknownBase`/to `Genesis` | — |
| A-13 | no API moves an `Established` anchor | `anchor_immutable`: a compile-fail doc test that `anchor` has no setter, plus a public-API audit (`cargo public-api` diff checked in) | — |
| A-14 | R1–R8 sim tests (P2a §2.5.3) | `r1_lookup_during_insert`, `r2_immutables_stable`, `r3_snapshot_before_reply`, `r4_ibd_mocktime_flip`, `r5_best_header_nonnull`, `r6_single_steps`, `r7_hash_mismatch_panics_debug`, `r8_reorg_between_loads` (deterministic reorg between two loads; the held snapshot is one epoch) | — |
| A-15 | seqlock coherence stress | `seqlock_stress` (8 readers, 10⁷ writes, tuple invariant) + `loom_seqlock` | — |
| A-16 | best-header vector: CL on active a1000 while header-only b1005 has more work → Core keeps b1005 | `bh_cl_on_active_keeps_heavier_header` (node via `ScriptedV`); SPV: `fork_target` = a1000 | harness `cl`: `m_best_header` and `GetLocator(best)[0]` |
| A-17 | a CONFLICT mark is not persisted, and the load recompute picks the higher-work header (rev7) | `bh_conflict_then_reload` (node): A95 active, A96…A100 and B96…B110 headers, lock A100 → B96 CONFLICT (130), `best_header` B110 (B96 is not it); `take_dirty` → codec → `load` → B96 is 2 again (the mark was never dirty, D15), `best_header` B110, no CL, observed false. SPV keeps 130 (D15) | harness `reload`: `a17-conflict-then-reload` |
| A-18 | `InvalidateBlock`, `MarkConflictingBlock`, `InvalidChainFound` walk-backs, and none elsewhere | `bh_walkback_{invalidate,mark_conflicting,invalid_chain}` with `ScriptedV` calling the hooks where the harness trace shows Core's sites; rev7 `bh_reconsider_keeps`: after `reset_failure_flags` and the reconnects, `best_header` stays A98, also once B110 is eligible again (§6.1 quirk 3) | harness `invalidate` and `cl`: `a18-invalidate-walkback`, `a18-invalid-chain-found` |
| A-19 | a first-seen tie | `bh_first_seen_tie`: two equal-work headers; the first stays; after reload the lower id | harness `pnbh` (acceptance half); the reload half compares with D9's normalization (either tied tip accepted): Core reports `address_tie` for C101/D101 (`a19-first-seen-tie`), so no vector asserts a winner |
| A-20 | SPV `fork_target` in the same cases: a1000 after the lock, never a header that fails (a) or conflicts with the floor (b′) | `ft_same_cases` over A-16…A-19's inputs | harness `cl` statuses, then A-27's brute-force predicate |
| A-21 | Sol r6's three branches from 80 (A100, B110 active, C105 stale), lock on A100 → A100, never C105 | `ft_sol_r6_three_branches`: also asserts B81 and C81 CONFLICT, B82…B110 CONFLICT (disconnected), C82…C105 unflagged, and C105 failing both (a) and (b′); C is data-less (O-m2) | harness `cl`: Core's statuses at the `EnforceBlock` boundary |
| A-21a | the same case in SPV: Core's `best_header` walk-back B110 → 80 (Sol r1 M2) | `spv_b110_a100_walkback`: one `VirtualTip` for the step (O-m4); `best_header` after enforcement, every mark, and one published snapshot at A100. A variant runs the lock's enforcement and timer enforcements in the same step until a pass changes nothing, and compares that fixed point (DEC-167); the `conflicting_marked` calls and notifications of the later passes are not counted | harness `cl`: `m_best_header` and statuses after `EnforceBestChainLock` |
| A-21b | repeated enforcement with an active floor below the window | `spv_reenforce_floor_below_window`: w = 10, floor 30 below the tip, `enforce_best_chainlock` again (5 s timer step) marks nothing new and `contains` resolves through cold records | harness `cl`: a second `EnforceBestChainLock` changes nothing |
| A-22 | a lock on the already-active block marks nothing, and a heavier branch forking below it never becomes the target | `ft_lock_on_active_heavier_below` | harness `cl`: nothing marked; Core's `best_header` is the heavier one |
| A-23 | property: every **reader-visible** SPV snapshot satisfies §9.1's floor predicate (the tip never conflicts with the floor while observed; DEC-136), with the flag and floor read from the **same** snapshot (Sol r1 M1, Sol r2 R2-m1) | `prop_spv_tip_never_conflicts_floor` (proptest over random branch trees, CLs, spork toggles, including off → on with kept marks, and batches; §9.1's step recipe). A test hook observes **every** `ArcSwap` store, not only final targets, and asserts one store per committed step that changed something (n8). Fixed counterexamples: `spv_disabled_floor_allows_heavier` (A-22's floor on active A100 and an unmarked heavier B110; after observed off, the target is B110 and the snapshot, flag false and floor A100, passes) and A-29 (x)'s trailing tip A80 under floor A90, with (ix)'s target A95 descending from it | — (SB0 §4.1's SPV invariant as amended, SB0 erratum E2) |
| A-24 | the node's checkpoint rule over Core's list | `checkpoint_rule_core_list`: a fork below the highest checkpoint in the index → `bad-fork-prior-to-checkpoint`; `checkpoints_enabled = false` disables it; dash-spv's packs are never consulted | harness `pnbh` with a patched regtest checkpoint |
| A-25 | BG: PF0 `chain_accept_headers` ≤ Core's `ProcessNewBlockHeaders` | §16 | harness `bench-pnbh` |
| A-26 | BG: node test (R7-O1): a CL on a header-only block, and an enforcement reorg, leave the node tip trailing the floor with no `ChainError` | `r7o1_node_tip_trails_floor`: (i) lock on header-only a1001 while the tip is a1000: floor = a1001, tip a1000 at the fixed point (DEC-167: dashd's pass count here is timing-dependent); (ii) lock on a1000 while b1000 is active: inside `enforce_block`, `ScriptedV` disconnects b1000…b(fork+1); each disconnect gets its own `seq` and F1 event, and **one** snapshot is stored when `enforce_block` returns (tip = fork, trailing the floor; the store hook counts no store during the walk, O-m3). SB5's feed receives each event at its step, in `seq` order, while the published snapshot still shows b1000 (D43). A lock probe asserts that the tip mutex is held continuously from entry to the snapshot store (R4-M2), and a counter asserts one `IF_NEEDED` check per disconnect, before its tip step (DEC-144). Then ABC's connects of a publish one snapshot each, trailing the floor until a1000; no `ChainError` at any step; `best_header` stays a990, the fork point, through the reconnects (§6.1 quirk 2; `a26-ii-enforcement-reorg`) | harness `cl`: tip and floor at the `EnforceBlock` boundary and the trace through the first enforcement's ABC; later passes run to the fixed point (DEC-167) |
| A-27 | leaf-set `fork_target` equals SB0's predicate (Sol r1 m2) | `prop_ft_leaves_vs_brute_force`: random trees with cold leaves, bad suffixes (FAILED/CONFLICT entries above good ones), marks on the floor's own path, spork off/on, and one shared cursor after a random enforcement; the leaf answer (with the `lca` replacement) equals a brute-force scan over **all accepted records** with (a) ∧ (b′) relative to the cursor | — (equivalence to SB0's predicate as amended, not Core's candidate set, §6.2) |
| A-28 | PNBH index-state branches (Sol r1 M6) | `accept_pnbh_index_state`: a known FAILED duplicate (`duplicate`, `BLOCK_CACHED_INVALID`), a known CONFLICT duplicate (`duplicate`, `BLOCK_CHAINLOCK`), one carrying both (FAILED wins, `validation.cpp:4612-4619`), a header whose parent is both FAILED and CONFLICT (`bad-prevblk` first), an immediate invalid parent vs lazy `m_failed_blocks` ancestry (`FAILED_CHILD` marks and dirty set), `last_accepted` on a batch failing at index 3, unknown/height-mismatched/late locks (`Pending`, `HeightMismatch`, `floor_moved`) | harness `pnbh` and `cl` |
| A-29 | SPORK_19 transitions (Sol r1 M5) | `spork19_transitions`, node and SPV: (i) off → on resets `cl_best`/`cl_floor` and a post-reset lock at a **lower** height is accepted (node: the lock recorded while off arrives by `submitchainlock`, `a29-i-off-on-lower-height`; a P2P lock while off is dropped unread and the reset clears the floor set while on, `a29-i-p2p-lock-on-off-on`; SPV runs only the P2P form, rev7); (ii) a late header for a CL recorded while off (node: by `submitchainlock`), crossing the transition before and after the check (`a29-ii-late-header-before-check`, `-after-check`); SPV, which cannot record while off, runs it with a CL recorded while on and pending on its unknown header; (iii) on → off: no conflict check at acceptance, no enforcement, no SPV floor refusal, marks still act; (iv) restart: node starts unobserved with no CL; SPV restores `enabled_observed` and does not reset while the spork stays on; (v) pre-existing marks survive both transitions; (vi) mainnet ignores a received "off"; (vii) fresh-enabled first lock: observed false, no CL, spork on, a lock on A100 → floor A100 kept and observed true; (viii) a non-IBD `(tip, nullptr)` notification after `MarkConflictingBlock` schedules the check (`updated_block_tip_schedules` true; with `ibd` or `new == fork`, false); **(ix)** (Opus O-M1, DEC-136; rev7 per `a29-ix-reset-then-lower-lock`) fork at 80, B81…B110 active, A81…A95 headers: lock B100 (already on the active chain: no marking walk), spork off, a check, on, lock A90 (reset, accepted; B110…B81 disconnected and CONFLICT, status 157; A81…A95 unmarked): node tip A80 and `best_header` A80 under floor A90 (A81…A95 have no data), SPV target A95 with `FloorRelation::Descends`, no `ChainError`; without the check while off (`a29-ix-no-observed-off`) the A90 lock is `Stale` and nothing changes; **(x)** (rev7, the kept-mark trailing witness, §6.2) fork at 80, A81…A95 connected and active, B81…B110 headers: lock B100 (A95…A81 disconnected, A81…A95 CONFLICT, tip A80, `best_header` B110), off, a check, on, lock A90 (B81 CONFLICT; A90 `is_valid`, so no reset; A81…A95 keep CONFLICT): node tip A80 and SPV target A80 with `FloorRelation::Trails`, no `ChainError`; **(x′)** (rev7.1, Sol r7 R7-m1) as (x) with A81…A95 headers only: the B100 lock marks A81 (130) without a disconnect; after A90, A81 stays 130 (the node's `reset_failure_flags(A90, false)` keeps it), B81 is 130, node tip A80 and SPV target A80 with `Trails`. In (ix), (x) and (x′), `m_best_header`, statuses, best CL, floor and `isEnabled` equal Core's | harness `cl` with signed regtest toggles (receipt order of DEC-128; `cl via: submitchainlock` for a lock recorded while off, `cl via: p2p` for the dropped one; (i) and (vii) are the smoke-test vectors; (ix) is `a29-ix-reset-then-lower-lock` and `a29-ix-no-observed-off`; (x) is the new `a29-x-kept-mark-trailing` and (x′) the new `a29-xp-kept-mark-headers-only`, §15), `seed-cl` only for R2-M2's seed-order controls (`r2m2-*`), `notify` for (viii), plus `reload` for (iv)'s node half; (iv)'s SPV half and (vi) are D29/D30 checks with no Core counterpart beyond `spork.cpp:173-181` |
| A-30 | cold marking (Sol r1 M4) | `spv_cold_mark_b81_c81`: A-21 at w = 10, so B81 and C81 are evicted; enforcement marks them in the overlay, the eligibility walk sees the marks, both branches are then extended in the same step, the records commit, and a reopen (`MemHeaderStore`) reproduces statuses and `fork_target` | harness `cl` statuses (the same as A-21's) |
| A-31 | SPV memory bound (§3.8) | `spv_memory_flat_mainnet` (`#[ignore]`, nightly): H-M from genesis at w = 10,000 under a counting allocator; live bytes ≤ 1.25 × §3.8's bound and no trend over the last 2 million headers | — |
| A-32 | pack work underflow (Sol r1 m3) | `anchor_pack_work_checked`: a pack with low absolute work → `AnchorError::Work`; on a valid pack, `work(child) − work(parent) == proof(child)` for each pair | — |
| A-33 | commit failure on every step kind (Sol r2 R2-M1) | `spv_step_commit_failure_{batch,timer,received_lock,late_header,reset_only}`: `MemHeaderStore` fails stage 3. Each asserts: no `ArcSwap` store (hook count unchanged), no SB5 event handoff (`TipCell` fanout hook unchanged), no eviction (resident id set unchanged), the latch tripped, and the store byte-identical to before the step | — (§9.1's recipe) |
| A-34 | crash cuts on the empty-header path (Sol r2 R2-M1) | `spv_step_crash_cuts`: for the timer, received-lock, late-header and reset-only steps, stop the step after stages 1, 2, 3 and 4 (drop the chain; `MemHeaderStore`'s commit is all-or-nothing). In every cut the store holds either the old whole step or the new whole step: a stored floor, active change or `enabled_observed` is never present without the status rows written in the same step, and vice versa. SB5 adds the reopen half | — (SB0 §9.5's atomic step) |
| A-35 | Sol r2's resident-to-cold case | `spv_b110_a120_w10_resident_to_cold`: w = 10, B branch from 100 active to B110, lock on A120. B101 is resident before enforcement and is marked CONFLICT in the overlay; A120's eviction cutoff is 110. After a successful step the cold read of B101 returns CONFLICT from the committed record. With stage 3 failing (O-m8): B101 is still resident, its CONFLICT exists only in the writer's overlay, `IndexAccess::status(B101)` still reads the unmarked value, nothing was published, the store still holds the unmarked record, and the latch has tripped | harness `cl`: Core's statuses for the same chain |
| A-36 | per-call `MarkConflictingBlock` effects in Core's order (Opus O-M2) | `enforce_conflicting_marked_order` (node, `ScriptedV`): lock on A100 with B110 active (fork 80) and a stale C90 under A89. `conflicting_marked` is called for C90 with `host.tip()` = B110 and `was_in_chain` false, then the disconnects B110…B81, then for B81 with tip 80 and `was_in_chain` true. The hook sequence, each tip argument and the interleaved disconnects equal the trace through the first enforcement and its ABC; later passes run to the fixed point, where the state is compared (DEC-167). SPV: `VirtualTip` records the same two tips, and `updated_block_tip_schedules` is evaluated on each | harness `cl` signal trace (non-IBD): `SynchronousUpdatedBlockTip` and `UpdatedBlockTip(B110, null)`, then `BlockDisconnected` for B110…B81 (30 entries, tip down), then both signals with `(80, null)`, i.e. the first pass's prefix (DEC-147), state at the fixed point (DEC-167) |
| A-37 | SPV status coherence and empty steps (Opus O-m8, n8) | `spv_status_coherent_with_snapshot`: during a lock step (hooks between stages 1–2, 2–3 and 3–4), `IndexAccess::status` on B82…B110 reads unmarked while the published tip is B110; after stage 4 they read CONFLICT and the tip is A100. `spv_empty_timer_step_skips`: a timer step that changes nothing makes no `commit` call and no `ArcSwap` store | — (§3.7, §9.1) |
| A-38 | cold prev in SPV (Opus O-m5) | `spv_accept_deep_fork_cold_prev`: w = 10, no floor, a stale branch forking 30 below the tip with its parent evicted: `accept` reads `time_max` from the parent's record, computes `mtp` from the `RuleContext`, and the verdicts (including `time-too-old` at the MTP boundary) and the new entry's `time_max` equal the resident run's; `accept_header` (O-m9) gives the same results one header at a time | harness `pnbh` on the same regtest chain |
| A-39 | the node's `enforce_block` composite excludes handoffs (Sol r4 R4-M2, DEC-140) | `node_enforce_blocks_subscribe_at`: B110 active and published at `tip_seq` q, with a subscriber retained at B109 and still catching up. `enforce_block(A100)` runs with `ScriptedV`, and a hook pauses it after the staged disconnect of B110 (`TipState.tip` = B109, `next_seq` = q + 2). A second thread calls `subscribe_at(B109)`. It must block, with no `finish()` call and no slot going live, until the handoff, and then return `Moved` because the tip is 80. Variants: `subscribe_at(B110)` issued before `enforce_block` takes the mutex goes live at q + 1 and receives every staged event; `subscribe_at(80)` issued during the walk goes live after the handoff at the next `seq` and receives none of them; `deregister` of a pending target during the walk blocks likewise. A debug-build test asserts that a reentrant `publish_status` from a `ScriptedV` callback panics | — (Core's `cs_main` scope over `EnforceBlock`, `validation.cpp:4053`; SB0 F10) |
| A-40 | deadlock freedom of a CRITICAL wait under the hold (Sol r5 R5-M2, DEC-144; strengthened per Sol r6 R6-M1, DEC-146) | `enforce_critical_wait_completes` (node, `ScriptedV` with test-support's model flush writer, which obeys H1–H3 and H7). Sol r6's schedule: `enforce_block(A100)` takes the tip mutex at B110 (fork 80, published `seq` q) with F0 in flight. **S**, a **mandatory, Synced** subscriber with a capacity-one queue, holds the connect of B110 (q). A second mandatory subscriber M stays live. A third thread is blocked in `subscribe_at`. The disconnect of B110 (q + 1) becomes `pending` for S. A deregistration thread sets S's `closing` and blocks on the mutex; V drops q + 1 for S (H4). S's worker processes q and acks F0; F0 releases. At the next CRITICAL, F1 freezes at B109, and its `FlushCommit` carries **(B109, q + 1)** while the published snapshot still says q (R6-m1). F1's prune selection requests `PruneClears`. Native Σ then crosses B again, so a **second CRITICAL wait under the hold** waits for F1, and F2 freezes later in the walk. Two orderings run:<br>(i) S closes **before** F1's capture and gate creation: S is in neither set;<br>(ii) S closes **after** them, so F1's `to` and rewind gate (S's durable L = B110 strictly descends from B109) include S, and W, already waiting, is woken and treats S as satisfied (H7 (a)).<br>Asserts: enforcement and F1/F2 complete within a 10 s watchdog (a hang fails); **no W target, captured or gated, depends on an event removed by closing**, checked by a hook that fails if W's predicate ever waits on a slot whose `closing` is set or on a `seq` dropped from a closing slot; M's acks and rewind and `PruneClears` are served during the hold; S's durable locator stays B110 and its prune lock stays in F1's and F2's prune bound until deregistration completes; afterwards the drained channel enqueues the control mirrors of F0, F1 and F2 in order, `deregister` completes with nothing enqueued after it (F11) and releases the prune lock, and `subscribe_at` returns `Moved`. Negative controls: (1) the model W takes the feed/hub lock at step 5 (an H1/H2 violation), and the watchdog must report the deadlock; (2) the model W ignores `closing` (an H7 violation) in ordering (ii), and the watchdog must report the deadlock. CH2b repeats this test with the real W | — (Core's `FlushStateToDisk(IF_NEEDED)` inside `DisconnectTip` under `cs_main`, `validation.cpp:3303-3316`, `:4053`) |
| A-41 | Sol r5's deep coin-restoring enforcement: pressure checked at every boundary (R5-M2, DEC-144) | `enforce_deep_restoring_pressure` (node, `ScriptedV`, model coins counter and model W): a losing branch of 2,000 blocks, each restoring 1,000 distinct coins (72-byte slots, 72,000 B per disconnect); B = 8 MiB; a 1 MiB F in flight at entry; a hub operation (`subscribe_at`) blocked on the mutex; EVO pressure crossing 64 MiB before the final disconnect. It asserts: 2,000 `IF_NEEDED` checks, one per disconnect before its tip step; every intermediate freeze's `FlushCommit` `(new, tip_seq)` equals the private tip and last assigned `seq` captured under the guard, never the published snapshot's (R6-m1); the first CACHE_CRITICAL at disconnect 102, which waits for F's release and freezes; at every boundary Σ ≤ B + 72,000 B, never rev4's 145,048,576 B before a first check; the EVO_CRITICAL wait at its crossing; completion; every subscriber received all 2,000 events in `seq` order; one snapshot store; and recovery from a crash injected after any intermediate freeze replays from that F's durable cut (CH2's unchanged packet protocol) | — (Core's per-`DisconnectTip` `IF_NEEDED`; CH2 §3.3's call sites and modes) |

### 15.2 SB5 items that rest on SB4

| SB5 item | SB4 provides | SB4-side test |
|---|---|---|
| windowed ≡ unwindowed over §4.4's scenarios | `Evicting`, cold resolution, skip-id ancestry | A-4's `spv_w1000`; `cold_ancestor_skip_reads` (O(log n) record reads counted) |
| R6-M1 across restart and cold records (A100/B110/C105 reopened; and with w below the branches' depth) | `leaves()` seam, `fork_target` over cold reads, the overlay, every SPV status change dirty | A-30 (cold marks, extension, `MemHeaderStore` reopen); SB5 adds the SQLite reopen half |
| eviction after commit (DEC-112 m2) | eviction only inside `set_active_tip` (§9.1 stage 4) | `evict_only_after_set_active_tip`: `accept` with w below the batch never evicts; a failing `MemHeaderStore` read trips `FailStop` and publishes nothing; `records_committed()` invalidates the LRU ids it wrote |
| lagging reader across a reorg deeper than w | `ActiveChainView` roots pinned by snapshots | `snapshot_pins_own_leaves`; SB5 adds `at_height` below the chunks |
| wallet recovery by locator (SB0 erratum E3, Sol r4 R4-M1, DEC-140) | `locator(Some(id))` with Core's `LocatorEntries` density and the SPV anchor terminus (§10.1); `ActiveChainView::fork_in_global_index(locator, lookup)` (Core `FindForkInGlobalIndex`, §3.6) over the **restored** snapshot | `wallet_locator_lost_disabled_suffix` (`MemHeaderStore`): the durable step holds A100 with floor A70, observed enabled. A suffix committed Buffered but not Durable (publication follows commit) observes the spork off, accepts B51…B110 forking at A50 and publishes B110. The wallet's record carries `locator(B110)` = B110…B100, B99, B97, B93, B85, B69, then the shared 37 and genesis, plus a B60 payment. After power loss, the reopened chain is A100/A70, and `fork_in_global_index` returns height **37**: the first locator entry on the restored active chain, at or below the true fork 50 at Core's density. So the wallet `truncate_above(37)` drops B60's effects (and B51…B110's), then rescans from 38; the floor A70 plays no part. Controls: a locator whose first entry is on the restored chain returns it (nothing to rewind), and an all-unknown locator returns the anchor. SB14 adds the wallet half (key-wallet-manager record, `truncate_above`, rescan) |
| wallet effects and checkpoint are one transaction (SB0 erratum E3, Sol r5 R5-M1, DEC-144) | nothing beyond the row above: The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. | `wallet_effects_checkpoint_one_txn` (SB14, key-wallet-manager over a recording wallet-DB layer). Sol r5's cut is: durable SPV A100/A70 with the wallet checkpoint at A100 and `locator(A100)`; a committed Buffered suffix observes the spork off and publishes B110; the wallet commits its reorg truncation and B effects (B60's payment) **before** its checkpoint; power loss. That cut is impossible by construction. The test therefore asserts that every wallet-DB commit writing effects also writes the synced height and locator, and the layer fails the test on an effects-only commit. Crash cuts just before the one commit (wallet at A100, no B effects, nothing to rewind) and just after it (checkpoint B110, then the rewind to 37 drops B60) both recover without wrong-branch effects. Core's equivalent is `CWallet::LoadToWallet`'s revalidation (`wallet/wallet.cpp:1142-1167`) |
| the checkpoint rule after reopen (Opus O-m7) | the `highest_checkpoint` cache and §11.4's rebuild rule (one `by_hash` per checkpoint, highest first) | `checkpoint_cache_rebuilt`: a genesis-anchored regtest chain holding a patched checkpoint, reopened from `MemHeaderStore` with the rebuild, rejects a fork below it with `bad-fork-prior-to-checkpoint`; SB5's `open` calls the helper and adds the SQLite reopen half |

### 15.3 Other tests

- Codec: proptest round trip; vectors from Core `CDiskBlockIndex` (harness `--dump-diskindex`) with the version prefix and hash stripped.
- `block_proof`: `dash-oracle-sys::block_proof` over random and edge targets (zero, negative, overflow → 0).
- `load`: mainnet-sized synthetic records (H-M with random stale branches and statuses), shuffled iteration order; `best_header`, chain work, FAILED_CHILD propagation and the dirty set equal a sequential reference; `NonContiguous`, `MissingParent`, `HighHash`, `GenesisMismatch` negative cases.
- `check_index` (`-checkblockindex`, header subset of `validation.cpp:5607-5855`): on in every SB4 test, which is how Core's own tests run.
- `chain_tips` vs Core in every harness scenario, with the §10.2 projection and D9's equal-height normalization.
- `locator`: equals Core's `GetLocator` for random ids on H-M (harness `--locator`), and `locator(None)` per profile.

## 16. Performance plan (PF0)

Bench target `dash-chain/benches/chain.rs` (criterion), and the workload lines proposed for `contrib/node/perfgate/workloads.toml`:

```toml
chain-accept-headers     = { bench = "dash-chain/chain", id = "accept/mainnet-dgw-2000" }      # BG gate (A-25)
chain-accept-kgw-2000    = { bench = "dash-chain/chain", id = "accept/mainnet-kgw-2000" }
chain-accept-spv-2000    = { bench = "dash-chain/chain", id = "accept/spv-anchored-w10000-2000" }
chain-index-lookup       = { bench = "dash-chain/chain", id = "lookup/hit-miss-10k" }
chain-locator-tip        = { bench = "dash-chain/chain", id = "locator/mainnet-tip" }
chain-ancestor-random    = { bench = "dash-chain/chain", id = "ancestor/mainnet-random-1k" }
chain-tip-step-publish   = { bench = "dash-chain/chain", id = "tip/connect-publish-1k" }
chain-load-records-256k  = { bench = "dash-chain/chain", id = "load/records-256k" }
```

- **Like for like** (A-25). Each bench starts from an index preloaded with H-M up to height h−1 and accepts 2,000 headers from h (h = 2,000,000 for DGW, 20,000 for KGW). The timed region includes hashing the batch with `dash-x11`, because Core's `ProcessNewBlockHeaders` hashes inside (`:4736`) while `accept` receives hashes (R7). The Core side is the harness's `--bench-pnbh` mode on the same pre-state, counted with the PF0 counter method (instructions; memory: perf counters stable to ±0.04%). The gate is instructions(rust) ≤ instructions(Core).
- **Expected costs**: per header, one X11 (dominant), one index probe and insert, `RuleContext` O(1) amortized (§8.2), DGW over 24 resident entries, one `OnceLock` set. The arena and index allocations are amortized by chunking and presizing.
- **Risks**: base-sdk `dash-pow` X11 is ~2.3× Core's instructions, so the bench must use `dash-x11` (0.23× Core). If BS15's switch has not landed when SB4 merges, the gate runs with `dash-x11` called directly in the bench, and the line is noted.

## 17. Commit plan

PLAN's L is 3 weeks (15 days), with 1.5 weeks (7.5 days) of slack against M2. The plan front-loads the API that SB5 and CH1-node code against (PLAN §8.3 rows 10–11). The oracle harness is a separate task (DEC-123 Q3), and its days are not on SB4's path.

| # | Commit (conventional) | Content | Est. (days) | After | Unblocks |
|---|---|---|---|---|---|
| C0 | `feat(dash-chain): crate skeleton and public API stubs` | §4 signatures with `todo!()`, `dash-clock` minimal, `IndexWrite`, `ChainReadSnapshot`, `TipCell` core types, `SporkView`/`StaticSporks`, `TipHost`; layer entries (SB1's `layers.toml` if landed) | 1 | — | SB5 and CH1-node start coding against the API |
| C1a | `feat(dash-consensus): GetBlockProof port` | D2, `block_proof` vectors (§15.3) | 0.5 | C0 | — |
| C1b | `feat(dash-chain): Status and BlockIndexRecord codec` | codec, vectors (split from C1 per n2) | 1 | C0, C1a | CH2b's `IndexWrite` use |
| C2 | `feat(dash-chain): arena, seqlock entries, hash index, ActiveChain, links` | §3.1–§3.6, §3.7 node half, §13; stress + loom | 3 | C0 | — |
| C3 | `feat(dash-chain): accept with Core's header rules` | §5, §12, §11.4; A-1, A-1a, A-3, A-24, A-28 | 3 | C2 | P2a-node's `process_headers` |
| C4 | `feat(dash-chain): RuleContext cache, AnchoredAncestry, anchor pack, Evicting, overlay, MemHeaderStore` | §3.7 SPV half (overlay for every status change), §3.8, §5.4 cold prev, §8, §11.1; A-2, A-5…A-11, A-6a, A-32, A-38 | 4 | C3 | SB11's ancestry |
| C5 | `feat(dash-chain): tip writers, per-product publication, IBD latch, locator, chain_tips` | §9, §10.1–10.2, `check_index`; A-14 | 3 | C2 | SB5's feeds on `TipCell` |
| C6 | `feat(dash-chain): ChainLock floor, SPORK_19 lifecycle, enforcement and SPV fork target` | §6, §7, §9.1's node staging; A-16…A-23, A-21a/b, A-26, A-27, A-29 (i)–(ix), A-30, A-36, A-37, A-39, A-40, A-41 | 5 | C4, C5 | CH1-node `ApplyChainLock` |
| C7 | `feat(dash-chain): take_dirty and parallel load` | §10.3; A-17, load tests | 2 | C1b, C2 | CH2 recovery step 4 |
| C9a | `feat(dash-chain): anchor candidate and fallback` + bench scaffolding | §11.2–11.3, A-12, A-13, §16 workload lines and fixtures | 1.5 | C0 | SB15 |
| C9b | `test(dash-chain): full-chain, memory and PF0 gates` | A-4, A-25, A-31 run against the finished crate | 1 | C6, C7, C9a | the SB4 row |

**Arithmetic** (Sol r1 m5). All figures are **estimates**. The SB4 commits total **24.5 days**: rev0's 24, minus C8's 3 (now a separate task), plus rev1's additions with their tests (+1 C4 for the cache and overlay, +0.5 C5 for SPV publication, +0.5 C6 for SPORK_19 and `TipHost`, +0.5 for splitting C9 and adding A-31), plus rev3's +1 C6 for (b′) with the cursor-relative (a), `conflicting_marked`, the node's staged publication and one `VirtualTip` per step, with A-29 (ix), A-36 and A-37, plus rev5's +0.5 C6 for the per-step check under the hold, the model flush writer and A-40/A-41 (the total is now **25 days**). Splitting C1 (n2) and rev3's C4 changes (overlay for every SPV status change, `time_max`, A-38) add no time.
- **One implementer**: 25 days = 5 weeks, 2 weeks over L, which is more than the 1.5 weeks of slack before any review. That does not fit.
- **Two implementers (recommended)**. Implementer A runs C0 → C2 → C3 → C4 → C6 → C9b = **17 days** (3.4 weeks). Implementer B (S-h is enough) runs C1a, C1b, C9a, then C5 after C2 lands (day 4 → day 7), then C7 = 8 days, all finished before A starts C6 on day 11. The critical path is A's 17 days, which uses 2 of the 7.5 days of slack and leaves about 5.5 days for the sequential implementation review, fixes and the nightly gates. Review time is **not** included in any figure here.
- **CPM** (rev3, rerun at rev5). C6's estimate changed both times, so `tools/cpm_plan.py` was rerun. It models SB4 by size class (L); +1.5 days stays inside L, and the results are unchanged: SB4 finishes in week 8.5, SB5 11.0, CH1-node 11.5, M2 18.5 (critical path through P1/SB6/P2a-core/P2b/O13a, not SB4), M5 exit 39.5.
- If the harness slips, C6's oracle halves land as a follow-up commit before merge, and the SB4 row is not marked done without them. The same holds for C3's harness-dependent halves (A-1a, A-24; Opus review point 7).

## 18. Decisions

| # | Decision | Why / alternative |
|---|---|---|
| D1 | SB4 creates a minimal `dash-clock` (`Clock`, `SystemClock`, `MockClock`) | the crate does not exist yet, and `accept` needs `NodeClock` now. SB6 adds `AdjustedTime`; P2a's `dashd-clock` merges into it (SB0 §13 P2a row). Confirmed by DEC-123 Q4 |
| D2 | `block_proof` lives in `dash-consensus::pow` | pure rule code next to `check_proof_of_work`; CS2 has no `GetBlockProof` |
| D3 | SB4 implements `Residency::Evicting`, cold resolution through `HeaderStore` (with `MemHeaderStore`), the cold-status overlay and the store-backed children lookup; SB5 implements `open`, the LRU, snapshot heights below the chunks and the §4.4 property suite | DEC-123 Q1. SB4's own gate (A-4, A-5…A-8, A-30, the cold KGW walk) needs windowed and cold reads |
| D4 | Tips change only through one private `step`; public writers are `init_active_tip`, `connect_tip`, `disconnect_tip` (node, Core's ConnectTip/DisconnectTip boundaries) and `set_active_tip` (composite), plus `tip_plan` for SPV's WriteSet | SB0 P2's "only tip writer" holds at the step level; V needs per-step granularity for R6 and for `DisconnectTip` inside `MarkConflictingBlock` |
| D5 | `StoreError` and `FailStop` are `dash-chain` types until SB10's `dash-kv` lands, then become re-exports | S crates cannot use `dashd-store`'s `StoreError`, and `dash-kv` is M3 |
| D6 | Readers load the snapshot from an `ArcSwap` written inside the tip lock's critical section | SB0 holds the tip lock across `subscribe_at`'s `finish()` (DEC-96); a `Mutex` read would stall the P2a actor behind a durable commit. F2 is unchanged (§9.2) |
| D7 | In SPV, infallible reader methods (`IndexAccess`, `ChainReader`) answer `None` after a failed cold read, which has already tripped the latch | DEC-123 Q8; the client is stopping (P6); typed answers stay available through `AnchoredAncestry` |
| D8 | The time source follows `cfg.rules.time_source`; a V23_1 profile requires an injected `AdjustedTime`, else `ChainError::Config` | v24 is NodeClock-only (DEC-104 M1); the v23.1.8 profile (O17) must not silently use NodeClock |
| D9 | Core's pointer-address tie-breaks use the arena id, assigned at `load` in first-mention order. **An observable allowed difference** (Sol r1 m1; SB0 §4.1 adopted the substitution). Its effects, all node-side unless stated: (1) after a reload with equal-work, sequence-0 tips, `best_header` and `locator[0]` (both products, SPV through SB5's `open`; not Core-determinable: the harness reports `address_tie`, `a19-first-seen-tie`, and no vector asserts it); (2) `m_failed_blocks` iteration in acceptance step 7, when prev has two or more failed ancestors: which one bounds the `FAILED_CHILD` marks and the dirty set; (3) `getchaintips` order among equal-height tips; (4) (rev3, Opus O-M2) the order in which `EnforceBlock` marks the siblings at one level: Core iterates `m_prev_block_index`, an `std::unordered_multimap` (`node/blockstorage.h:59`) with unspecified order, and SB4 uses id order, which fixes the `conflicting_marked` tip arguments and so the `(tip, null)` notifications when a level holds both the active block and a stale sibling. Oracle comparisons normalize exactly these: (1) either tied tip is accepted, and the locator is checked against the chosen one; (2) result and reason exactly, and the marked set must be the path from prev down to the child of **some** failed ancestor; (3) equal-height tips are sorted by hash; (4) statuses compare exactly, and the signal trace must equal the one Core would emit for **some** order of each level's siblings (A-36's fixture has one sibling per level, so it compares exactly) | heap addresses are not allocation order, so no deterministic choice is Core's; the id is the closest deterministic proxy |
| D10 | Children (Core `m_prev_block_index`): node, a writer-only intrusive `links` list (8 B per entry); SPV, no map, `HeaderStore::children` ∪ `pending_new` (§3.7) | Sol r1 M4. A whole-chain map in SPV would grow with history (≈ 2.55 million entries on mainnet); the store's `prev` index (DEC-123 Q7) answers the only SPV reader, `EnforceBlock`'s walk |
| D11 | `apply_chainlock` takes `ChainLockRef { height, hash }`; SB8's `VerifiedChainLock` wraps it later | SB8 is M3; signature verification is upstream of the index |
| D12 | SPORK_19 is modelled as Core does in both products: the live `SporkView::chainlocks_enabled()` gates the conflict check and `has_chainlock`; the observed `cl_enabled_observed`, refreshed only by `check_cl_active_state`, gates enforcement and SPV's floor refusal and eligibility (b′); an observed off → on transition resets `cl_best` and `cl_floor` and keeps marks (§7.5). **Declared transient difference** (rev3, n7): (b′) and the SPV refusal also stand in for `ConnectBlock`'s live-gated `HasConflictingChainLock` (`chainlock/chainlock.cpp:77-79`), but follow the observed flag, so between a spork change and the next check SPV may keep a target Core would refuse to connect, or the reverse | DEC-123 Q2 (replaces rev0's SPV `AlwaysOn`); Sol r1 M5. One gate per predicate keeps a published snapshot self-consistent (A-23 reads the flag from the snapshot); the window closes at the next check |
| D13 | SPV-only acceptance paths of §5.3 (context parent / genesis → `BelowAnchor`; known context entry skipped) | SB0 §4.1.1; Core has no context entries |
| D14 | Header `DeploymentView` is buried-only; versionbits → false with a debug assert | CS3's header check queries buried deployments only |
| D15 | Dirtiness follows Core's `m_dirty_blockindex` sites exactly in the node (MarkConflictingBlock marks are **not** dirty); in SPV every status change is dirty, overlay changes included | node: Core semantics, so a restart reproduces Core's statuses (A-17's oracle, `a17-conflict-then-reload`: B96 130 → 2); SPV: SB0 §4.4 records carry branch status, and fork-target eligibility after reopen needs the marks (R6-M1 across restart) |
| D16 | `IndexWrite { hash, bytes }` and the record codec are owned by `dash-chain`; no per-record version (the column family is versioned by `DbSchema`) | CH2 §2.7 imports it; A1 §2.9 v1 |
| D17 | `HeadersResult` gains `stop: Option<HeaderStop>` (with `BelowAnchor`) and `floor_moved`; P2a's `invalid` becomes `stop`'s `Invalid` arm | DEC-123 Q6 |
| D18 | `reset_failure_flags` is the node's only; SPV never calls it, with a debug assertion that no SPV entry carries a FAILED bit | in SPV Core's call is a no-op that costs a full scan per CL |
| D19 | SPV `fork_target` is computed over resident leaves plus a store query when `max_cold_leaf_work` could win, with a candidate that conflicts with the floor replaced by `lca(C, floor)`; it equals **SB0's header-only predicate** as amended by DEC-136 ((a) ∧ (b′), (a) against the step's cursor), not Core's candidate set (§6.2) | Sol r1 m2; windowed-safe, O(1) in steady state; A-27 checks it |
| D20 | `HeaderStore` gains `leaves()` and `children(prev)`; SB13 indexes `prev` | DEC-123 Q7; `open` and the cold leaf query need `leaves()`, `EnforceBlock`'s cold walk needs `children` |
| D21 | A prev mentioned but never defined at load is `LoadError::MissingParent`. Also declared (rev3, n3): a second null-prev root or a root other than `params.genesis_hash` is `LoadError::GenesisMismatch`, and an empty index is `LoadError::Empty`. Core loads a second root and fails later at init's genesis check, and treats an empty index as a fresh datadir (`LoadGenesisBlock`); the caller handles `Empty` that way | DEC-123 Q5: a declared allowed difference, corrupt-DB only. Refusing at load gives the same outcome as Core's later init failure, earlier; returning `Empty` leaves writing genesis to the caller that owns the datadir |
| D22 | Acceptance verdicts come from `sb4-core-scenarios` (§15), built as a separate parallel task (Sonnet high) with test-support objects at the pinned shim commit; CS3 rows are rule-level controls judged by their recorded verdicts and O2a's `Oracle::contextual_check_header` | DEC-123 Q3; Sol r1 M6, m4. Core's `RegTestingSetup` already signs spork toggles, so no MN-chain fallback is needed |
| D23 | `RuleContext` reuse: R recomputed per header; the cached path grows its prefix, trims, or rebuilds on a branch change; ids plus owned cold copies, revalidated by `residency_epoch`; lower bound saturating and clipped to the floor (§8.2) | Sol r1 M3; O(1) amortized, O(R) once per growth; never dangling |
| D24 | `mtp` and `time_max` are stored immutable fields; pack entries 0–9 hold `MTP_UNKNOWN` with a debug assert on use | O(1) MTP; m10's laziness enforced where the only read happens |
| D25 | The hash index is an `RwLock<HashTable>` taken per insert | R1's second option; simpler than RCU growth; PF0 `chain-index-lookup` checks the cost |
| D26 | `history_blocks` (L_est + H_keep) is a host parameter of `anchor_candidate` | `dash-mnlist` computes L_est (SB11); `dash-chain` must not depend on LLMQ params |
| D27 | Core's checkpoint list is used by SPV as well as the node | n2 for the node; for SPV the rule is inert above every checkpoint and identical to Core's below |
| D28 | Publication follows Core's lock scopes: the node publishes every tip step of ABC and `InvalidateBlock` and every status change outside `enforce_block`; inside `enforce_block` it stages each disconnect (own `seq`, own F1 event) and stores **one** snapshot on return, handing each event to SB5 at its step (D43), all within **one** hold of the tip mutex from entry to return (rev3, Opus O-m3; rev4, Sol r4 R4-M2, DEC-140; rev5, DEC-144), so `subscribe_at` and `finish()` see only the pre- or post-enforcement cut. SPV publishes once per committed header or ChainLock step, inside one tip-lock critical section that also assigns the step's `seq`s and hands SB5 its events (§9.1). Both composites are the DEC-128 shape; feeds keep per-step events | Sol r1 M1. Core's node trails the floor between ABC steps (`chainlock/handler.cpp:299-309`), so per-step publication is Core's there; `EnforceBlock` holds `cs_main` throughout (`validation.cpp:4053`), so per-step snapshots inside it would show states Core never exposes (R8). SB0's SPV invariant forbids publishing an intermediate tip |
| D29 | Spork sources: node, SP0's manager (`StaticSporks` until it lands); SPV, a shared spork core with intake over SB16, persisted at receipt (declared difference: crash durability only); tests, `StaticSporks` (§7.5) | DEC-123 Q2 makes SPV track signed spork state; SB4 owns only the seam |
| D30 | SPV persists `enabled_observed` with the CL rows | without it, every SPV start with the spork on would reset SB0's persisted floor (§7.5) |
| D31 | DEC-123's "default on" means Core's behaviour: hardened on for mainnet (`spork.cpp:173-181`), received value with an off default on test chains | Sol r1 M5's network qualification; §19 Q-r1-3 asks for confirmation |
| D32 | Enforcement runs against `TipHost { tip, contains, disconnect_tip }`; SPV's `VirtualTip { cursor }` answers `contains` by ancestry over arena, overlay and cold records | Sol r1 M2: Core's walk reads `m_chain.Tip()` at each disconnect, and the floor's ancestry can lie below the window |
| D33 | Status changes to evicted SPV entries go to a writer-owned overlay, read before the LRU and the store, written in the step's WriteSet and dropped by `records_committed()` once applied by a stage 4; a mark no stage 4 applied is kept for the next one (§3.7, D-R2-C-1) | Sol r1 M4: `EnforceBlock` must mark B81/C81 after they left a 10-header window |
| D34 | §9.1's step recipe is the only statement of step order. For a node step, capture, commit and `records_committed` are empty: the node's dirty entries wait for CH2's B1 flush (P10), as Core's wait for `FlushStateToDisk` | DEC-128; Sol r2 R2-M1. A per-step node commit before publication would be stricter than Core and would change CH2 (§19 Q-r2-1) |
| D35 | SPV cold prev (rev3, Opus O-m5): `HeaderRecord` stores `time_max` (SB13's schema gains the column); `mtp` is not stored and is computed from the `RuleContext`, which already holds R ≥ 11 ancestors | `time_max` is a running maximum over all ancestors and cannot be recomputed from a bounded window; MTP needs only the 11 entries the context already loads, so a column would duplicate data |
| D36 | Every SPV status change after insertion, resident or evicted, goes through the writer's overlay and becomes reader-visible at stage 4 under the tip lock (rev3, Opus O-m8; extends D33). A cold status read between stages 3 and 4 may see the committed row one step early, and no SPV reader combines a cold status with a snapshot | Core's readers see statuses and tip under one `cs_main`; the alternative (declare SPV status reads incoherent) would leave B82…B110 reading CONFLICT under a published tip B110 |
| D37 | SB0 §4.1's SPV floor rule becomes "never conflicts with `cl_floor`" ((b′), DEC-136): eligibility, the `set_active_tip` refusal and A-23; the SPV tip may trail the floor; recorded as SB0 erratum E2 | Opus O-M1: under rev2's "descends from" a spork off → on reset with a kept mark leaves no eligible target, while Core's tip trails (A-29 (x); rev7: (ix)'s original shape marks nothing). (b′) is the header-level form of `HasConflictingChainLock` and adds nothing stricter than Core |
| D38 | `TipHost::conflicting_marked(chain, marked, was_in_chain)` runs at the end of each `MarkConflictingBlock`, before the next sibling; V does Core's candidate erase and re-add, `ConflictingChainFound`, the two `UpdatedBlockTip` signals with `host.tip()`, and `NotifyBlockTip` there; `VirtualTip` records the tip (rev3, Opus O-M2) | a replay after `enforce_block` cannot reproduce Core's order (ZMQ publishes B110 before the disconnects); adding the hook later would change a trait V implements |
| D39 | One `VirtualTip` per SPV step, from `chain.virtual_tip()`; every enforcement of the step and (a)'s cursor use it (rev3, Opus O-m4) | a fresh cursor per enforcement would re-disconnect entries Core's `m_chain` already lost and repeat notifications |
| D40 | Status oracles (rev3, Opus O-m1, O-m2): A-4 projects every non-active `getchaintips` status to `header-valid`; harness scenarios compare `invalid`/`conflicting` exactly; node rows compare statuses at the `EnforceBlock` boundary (harness `--wrap`), and node scenarios keep non-floor branches data-less unless a row says otherwise | a header-only replay cannot produce a live node's block-validation and ChainLock history; `FindMostWorkChain`'s propagation is CH1-node's, and copying Core's post-ABC statuses into `ScriptedV` would self-certify |
| D41 | SPV wallet recovery uses Core's mechanism (rev4, SB0 erratum E3, DEC-140). Each committed wallet sync record carries a `CBlockLocator`-shaped list, computed by `chain.locator(Some(id))` on the published snapshot that holds the record's block. On reopen, an unknown wallet hash rewinds to `fork_in_global_index(locator)` on the restored chain: the first entry present on its active chain, falling back to the anchor (the SPV locator's terminus). SB4 adds no API: `locator` and `fork_in_global_index` already exist (§3.6, §10.1). The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. (rev5, DEC-144) | Sol r4 R4-M1: a lost Buffered suffix with the spork observed off can publish a branch that conflicts with the durable floor, so no height derived from the floor is a common point. Core's wallet stores its best-block locator (`CWallet::chainStateFlushed` → `WalletBatch::WriteBestBlock`, `wallet/wallet.cpp:624-633`, `wallet/walletdb.cpp:187-191`) and on load rescans from `findLocatorFork` → `FindForkInGlobalIndex` (`wallet/wallet.cpp:3526-3537`, `node/interfaces.cpp:1418-1425`, `validation.cpp:182-197`). That needs no floor premise, because every locator entry is an ancestor of the wallet's block |
| D42 | **rev5 (DEC-144), replacing rev4's D42.** rev4 evaluated CH2's IF_NEEDED check once after the hold, and DEC-144 withdrew that deferral. CH2's IF_NEEDED check and its CRITICAL/EVO_CRITICAL wait run after **every** disconnect inside the `enforce_block` hold, at CH2's existing site before the tip step. The deadlock rev4 avoided is removed at its source by §13.4's **lock-order contract**: W's freeze → release path never takes the tip mutex, the feed/hub lock or a lock V holds (H1); W's durable barrier goes through a W-owned channel that V drains after the hold (H2); V's wait services `PruneClears` (H3); feed progress and subscriber progress take no tip mutex (H4, H6). Callbacks under the guard still call no mutex-taking `TipCell` API, and debug builds turn a reentrant lock attempt into a panic | Core runs `FlushStateToDisk(IF_NEEDED)` in every `DisconnectTip` under `cs_main` (`validation.cpp:3303-3316`, `:4053`). Sol r5's 2,000-block witness shows that rev4's deferral let native Σ reach 145,048,576 B before any check, more than 17 × B. CH2 `:818` covers counter differences, not skipped call sites. Tests: A-40 (deadlock freedom with F in flight), A-41 (pressure at every boundary) |
| D43 | Inside the node's `enforce_block` hold, each disconnect's F1 event is handed to the live feeds or `pending` **at its step** (rev5); only the snapshot store is deferred to the end. rev3/rev4 handed all events at the end | Needed for H5. An F frozen inside the hold has `new` at a disconnected tip, so its acks (case B) and rewind gate need that tip's events. With end-of-hold delivery, the next CRITICAL wait in the same hold (Sol r5's witness has many) would deadlock. This is also Core's behaviour: queued `BlockDisconnected` callbacks run on the scheduler thread during `EnforceBlock`. O-m3's concern was snapshot readers, who still see one store, and R4-M2's handoff exclusion is unchanged because `subscribe_at` still blocks on the hold |
| D44 | Closing retires a subscriber's flush obligations at the `closing` store (§13.4 H7; rev6, DEC-146): it leaves every captured and future ack set and rewind gate, W's predicate treats it as satisfied, its prune lock stays at its last durable locator until deregistration completes after the hold, and its durable locator does not advance | Sol r6 R6-M1: H4 dropped the event that a still-captured mandatory target needed, so V → W → missing event, with deregistration blocked on V. Core never makes a flush wait on an index; a stopping index unregisters (`index/base.cpp:419-426`) and resumes from `FindForkInGlobalIndex` (`:72-84`), which the kept prune lock preserves. Preserving events for a closing slot was the alternative; DEC-146 rejects it as unneeded |
| D45 | `FreezePacket` carries `(new, tip_seq)` captured by V under its guard from `TipState`, and `FlushCommit` repeats it (rev6, Sol r6 R6-m1; SB0 erratum E4) | inside the hold the published snapshot lags the private tip, so reading it would mislabel an intermediate freeze, and W reading `TipState` would take the mutex (H1) |
| D46 | Harness vectors are **state oracles** (rev7.1, DEC-147; harness Sol r1 R1-M1). Exact: statuses, tip, `best_header`, floor, records, and the trace up to and including each lock's first enforcement boundary with its ABC disconnect/connect order. DEC-167 replaces the idempotent-repeat rule: the state is asserted at the fixed point, where a further pass changes nothing; intermediate passes are transients. SB4's pass is Core's single pass, and the driver repeats it (§15) | dashd's pass count after a lock off the active chain depends on when the validation queue delivers the `(tip, null)` notification relative to `tryLockChainTipScheduled` (`chainlock/handler.cpp:197-206`). Sol measured 2, 4, 6, 2 and 2 boundaries for one script with natural delivery, all ending in the same state, and Core sets no bound. Asserting one timing sample would be stricter than Core (the DEC-95 rule) |
| D47 | SPV never builds the full `tip_plan` in a production step (DEC-187): stage 1 stops at `fork_target` and `tip_target`, and stage 4 is SB5's `activate_segmented`, which plans sub-steps with `tip_plan_bounded`. `tip_plan` stays public and unbounded for the node's `set_active_tip`, SB4's unsegmented SPV path and the tests and benches that use it as the reference plan. `EnforceReport.disconnected` is node-only; SPV's list is `VirtualTip::disconnected` | SB5 F8 measured Sol's full-plan witness at 1.44 MB and the doubled disconnect list on top of the marks bound. No debug assert in `tip_plan`: SB5's `full_chain`, `spv_memory` and `feeds` bench call it on SPV chains as the reference, so the bound is held where the step is driven (SB5's `activate_segmented`, SB14's driver) |

## 19. Open questions

**Resolved by DEC-123** (rev0's Q1–Q8): Q1 residency split → D3; Q2 SPORK_19 → D12, §7.5; Q3 harness as a separate task → D22; Q4 `dash-clock` in SB4 → D1; Q5 missing parent refuses to start → D21; Q6 `HeadersResult` → D17; Q7 `leaves()` and the `prev` index → D20; Q8 `None` after the latch → D7.

**Resolved by DEC-127** (rev1's Q-r1-1 to Q-r1-5): all five accepted. Q-r1-4 is staffed as the Rust core plus the separate `sb4-core-scenarios` row, with the CPM rerun; §17's split within the core is the implementer's to use.

**Resolved by DEC-132**: Q-r2-1 accepted; node stages 2, 3 and 5 are empty (D34).

**Resolved by DEC-136** (Opus high on 24098412): O-M1's (b′) is accepted as an SB0 §4.1 erratum (recorded in SB0 as E2, with Q-r1-1 as E1; D37); O-M2's `conflicting_marked` (D38) and O-m3's node composite (D28) are adopted. A narrow Sol confirmation of those three follows, then manager approval. rev3 raises no new question; its choices are D35–D40 and the amended D9, D12, D19, D21 and D28.

**Resolved by DEC-147** (harness Sol r1 R1-M1): vectors are state oracles with idempotent enforcement repeats allowed (§15, D46). Sol r7's R7-m1 is folded in (§27). rev7.1 raises no new question.

**rev7 (harness findings).** No question: the manager's rule is that a Core vector wins over the narrative, and every change in §26 follows a vector or Core's source. The one addition, A-29 (x), is specified for the harness to add.

**Resolved by DEC-145 and DEC-146** (Sol r6 on 45d9e640). DEC-145 accepts D43, per-step events under the hold. DEC-146 rules R6-M1: closing retires a subscriber's flush obligations at its linearization point and keeps its prune lock (H7, D44, A-40). It also asks for the R6-m1 fix: `FlushCommit` carries the freeze's captured tip and `seq` (§9.2, D45, SB0 E4). rev6 raises no new question.

**Resolved by DEC-144** (Sol r5 on e3d2d9ad). R5-M1, word for word: The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. (E3, §15.2.) R5-M2: D42's deferral is withdrawn; the IF_NEEDED check runs at every disconnect under the hold, with §13.4's lock-order contract for CH2b (D42, A-40, A-41). rev5 makes one consequential decision for Sol r6 to confirm: D43 hands each event to the feeds at its step inside the hold, which H5 needs.

**Resolved by DEC-140** (Sol r4 on 36266b40): R4-M1 becomes SB0 erratum E3, in which wallet recovery rewinds by the wallet's stored block locator (D41, §15.2), and R4-M2 makes the node's `enforce_block` hold the tip mutex for the whole composite (§9.1, D28, D42, A-39). rev4 raises no new question. One clarification for the manager: at Core's locator density the counterexample's rewind lands at height **37**, the first locator entry at or below the fork at 50, not at 50 itself. Core's wallet behaves the same way, and B60's effects are dropped either way.

**New in rev2.**
- **Q-r2-1 — the node instance of DEC-128** (D34). DEC-128 names node steps as well. **Decision**: the order binds the node too, with its per-step capture, commit and acknowledgement stages empty, because the node's index persistence is CH2's B1 flush and Core does not write the index per tip step. The manager may overrule, but a per-step node commit would reopen CH2 (DEC-103).

**rev1's questions, as decided then** (kept for the record):
1. **Q-r1-1 — SB0 erratum.** SB0 §4.3.1 (`SB0-shared-base.md:648`) says an applied ChainLock and its floor persist until a newer one supersedes them. With DEC-123 Q2, Core's off → on reset also clears them. **Decision**: SB4 follows DEC-123 and Core; the manager records the erratum against SB0.
2. **Q-r1-2 — SPV spork intake has no PLAN row.** D29 needs a shared spork core (verification, `ProcessSpork`, storage) usable by SP0 and SPV, plus intake over SB16. **Decision**: SB4 ships `StaticSporks` and the seam. Until the intake lands, SPV on testnet and devnet runs with ChainLocks observed off, like a Core test-chain node that has received no spork message. Proposed row: split SP0 into a shared S-class spork core and the node manager, with SPV intake under SB16.
3. **Q-r1-3 — D31.** Is "default on" meant as Core's network-specific behaviour (mainnet hardened, test chains off until a signed spork arrives)? **Decision**: yes, as D31 states; it is the only reading that adds no protocol difference.
4. **Q-r1-4 — schedule.** One implementer does not fit L + slack (23.5 days against 22.5, before review). **Decision**: recommend the two-implementer split of §17 (critical path 15.5 days) and a `cpm_plan.py` rerun with the harness as its own row.
5. **Q-r1-5 — D30 and D29's persistence timing** are SPV-only differences from Core's restart behaviour (Core loses CL state and stores sporks only at clean shutdown). **Decision**: keep both and declare them; each changes when a value survives a restart or a crash, never what Core's rules decide.

## 20. rev1 changes

Sol r1 (`reviews/SB4-design-r1-sol.md`) against rev0 8ff4b56b, and DEC-123.

| Finding | Fix | Sections |
|---|---|---|
| **M1** SPV composite publishes tips below or off the floor | Per-product publication (D28): the node publishes every step; SPV's `set_active_tip` closes the header step under one tip-lock critical section, assigns each disconnect/connect its `seq` in F1 order, hands SB5 the events, and publishes **one** snapshot at the final tip. `accept`/`apply_chainlock`/`enforce`/`check` publish nothing in SPV. `TipChange.snap` carries the snapshot for the reply. A-23 now observes every `ArcSwap` store | §0, §4, §5.1, §6.1, §9.1, §9.3 (R3), §15.1 A-23 |
| **M2** `VirtualTip` lacks Core's tip view | `TipHost { tip, contains, disconnect_tip }` (D32); the §7.2/§7.3 pseudocode reads `host.tip()`/`host.contains()` at Core's lines; `VirtualTip { cursor }` resolves `contains` by ancestry over arena, overlay and cold records. The B110/A100 walk-back reaches 80 as in Core | §4, §7.2, §7.3; A-21a, A-21b |
| **M3** cached `RuleContext` breaks when R grows | R recomputed per header; the cache grows its prefix, trims, slides, or rebuilds on a branch change; it holds ids and owned cold copies revalidated by `residency_epoch`; the lower bound saturates and is clipped to the floor (D23) | §3.8, §8.2; A-5, A-6 (warm consecutive accepts 560…600, 15,190…15,210, 34,130…34,150), A-6a |
| **M4** unbounded SPV children; undefined cold-status writes | Node keeps Core's map as `links`; SPV has no map and asks `HeaderStore::children` ∪ `pending_new` (D10, D20). Resident-only leaves plus `max_cold_leaf_work`. Cold-status overlay with read precedence, WriteSet inclusion, `records_committed()` and LRU invalidation (D33). Memory bounds with mainnet numbers (estimates): SPV ≈ 3.5 MB flat at w = 10,000; node ≈ 560 MB vs Core's ≈ 765 MB | §1.1, §3.7, §3.8, §5.4, §6.2, §10.2; A-30, A-31 |
| **M5** SPORK_19 lifecycle and network semantics | §7.5 models Core: live vs observed state, `check_cl_active_state` with the off → on reset (`chainlock/handler.cpp:209-222`, `chainlock/chainlock.cpp:98-104`), the four call sites, mainnet hardening and test-chain defaults (`spork.cpp:163-196`, `spork.h:66-72`), signed messages and `sporks.dat` persistence, restart behaviour, marks kept across transitions. SPV persists the observed flag (D30); spork sources D29; "default on" read as D31 | §1.1, §4, §6.2, §7.1, §7.5, §18 D12, D29–D31; A-29 |
| **M6** CS3 rows are not acceptance vectors | CS3 rows become rule-level controls (A-1, A-2) judged by their recorded verdicts and O2a's `Oracle::contextual_check_header`. Acceptance verdicts come from the extended `sb4-core-scenarios` harness (`pnbh`, `cl`, `reload`, `invalidate`, `locator`, `diskindex`, `bench-pnbh` modes; regtest/devnet/testnet/mainnet coverage stated per rule), with rebased vectors re-judged by Core. Every §15.1 row names its oracle. New rows for index-state branches and lock cases (A-28) and spork transitions (A-29). `getchaintips` compared with block-data status projected away | §10.2, §15, §15.1 (A-1, A-1a, A-2, A-28, A-29), §18 D22 |
| **m1** D9 proxy is observable | D9 declared an observable allowed difference; its three effects enumerated, with the normalization each comparison applies | §6.1, §10.2, §15, §18 D9 |
| **m2** leaf proof qualified | §6.2 proves equivalence to SB0's header-only predicate and says where Core's candidate set differs; A-27 is the randomized leaf-vs-all-records test with cold leaves and bad suffixes | §6.2, §18 D19; A-27 |
| **m3** checked pack-work subtraction | compare before each subtraction; underflow or zero → `AnchorError::Work`; A-32 | §11.1; A-32 |
| **m4** harness build inputs | the separate harness task builds Core with tests at the pinned shim commit, links unlocalized server libraries plus `test/util` objects, records provenance and passes a smoke test before the full vector set; no MN fallback | §15 fixtures, §18 D22 |
| **m5** schedule arithmetic | recomputed: 23.5 SB4 days without the harness; one implementer does not fit; a two-implementer split puts 15.5 days on the critical path; review time excluded; `cpm_plan.py` rerun requested | §17, §19 Q-r1-4 |
| DEC-123 Q1–Q8 | applied | §19 (resolved list), D1, D3, D7, D12, D17, D20, D21, D22 |

## 21. rev2 closure map

Sol r2 (`reviews/SB4-design-r2-sol.md`, c804aff0) against rev1 5f03a841; DEC-128 binding. M1–M6 and m1–m5 stay as Sol r2 confirmed them; only the sections below changed.

| Finding | Fix | Sections |
|---|---|---|
| **R2-M1** §7.5's ChainLock-step recipe published and evicted before commit | §9.1 now holds the single normative step recipe in DEC-128's order, with commit before `set_active_tip` (which runs the plan computed in stage 1), for header-batch, timer, received-lock, late-header and reset-only steps. §7.5 now cites it and gives no order; §3.7, §7.1, §7.3, §13.3, §15.2 and the §4 comments cite stages instead of restating an order. Node instance recorded as D34. New controls: A-33 (commit failure on every step kind: no publication, fanout or eviction, store unchanged), A-34 (crash cuts on the empty-header path: no floor, active change or observed flag without its status rows), A-35 (Sol's B110 → A120 at w = 10, B101 resident to cold) | §3.7, §4, §7.1, §7.3, §7.5, §9.1, §13.3, §15.1 A-33–A-35, §15.2, §18 D34, §19 Q-r2-1 |
| **R2-M2** the `cl` harness mode recorded the lock before Core's initial state check | `cl` follows `ProcessNewChainLock`'s order with only `chainlock::VerifyChainLock` bypassed (O2a `--wrap` seam): initial `CheckActiveState` → seen/height pre-check → `LookupBlockIndex` + `UpdateBestChainlock` → scheduled `CheckActiveState` + `EnforceBestChainLock`. Direct `UpdateBestChainlock` is the separately named `seed-cl` seeding operation. The smoke test and A-29 include the fresh-enabled first lock and the off → on lower-height lock, emitting best CL, floor, observed flag and statuses. (rev7: the lock recorded while off arrives by `cl via: submitchainlock`, the only Core path that records while off; the P2P form is dropped unread, §7.5.) Core cites now use the pinned paths `chainlock/handler.cpp`, `chainlock/chainlock.cpp`, `chainlock/handler.h`, `chainlock/chainlock.h` | §7, §15 (`cl`, `seed-cl`, `notify`, smoke test), §15.1 A-29 |
| **R2-m1** A-23 asserted descent unconditionally | A-23 asserts §9.1's floor predicate (observed off, or no floor, or the tip descends from the floor) over one snapshot; fixed counterexample `spv_disabled_floor_allows_heavier` (A-22's floor, heavier B110, observed off); §9.1's concluding sentence qualified | §9.1, §15.1 A-23 |
| **R2-m2** `UpdatedBlockTip`'s guard paraphrased | The exact guards `pindexNew == pindexFork` and `fInitialDownload` are ported as `updated_block_tip_schedules(new, fork, ibd)`, with Core's three producers (ActivateBestChain, InvalidateBlock, MarkConflictingBlock's `(tip, nullptr)`). SPV feeds it from each published step and each `MarkConflictingBlock` call (`EnforceReport.tip_notifications`). A-29 (viii) and the harness `notify` mode cover the non-IBD `(tip, nullptr)` control | §4, §7.5, §15, §15.1 A-29 |
| Sol r2 note: figures are estimates | §3.8's 3.5 MB, 560 MB and 765 MB figures and §20's M4 row are labelled estimates; A-31 is the fork-free measurement control | §3.8, §20 |

## 22. rev3 closure map

Opus high (`reviews/SB4-design-opus-high.md`) against 24098412, APPROVE-WITH-CHANGES; DEC-136 binding. Sections not named below are unchanged.

| Finding | Fix | Sections |
|---|---|---|
| **O-M1** no SPV target when the floor's own path carries a kept mark | (b) becomes (b′): the candidate does not conflict with the floor (the floor, an ancestor or a descendant), gated by the observed flag. (a) is evaluated against the step's single `VirtualTip` cursor, after enforcement. A leaf candidate that fails (b′) is replaced by `lca(C, floor)`, and the cursor is always a candidate. The equivalence proof covers the replacement. `fork_target` reports `FloorRelation` (`Trails` when the target is a strict ancestor of the floor). `set_active_tip` refuses only a target that conflicts (`ChainError::ConflictsWithFloor`). The A-23 predicate is `!conflicts(tip, floor)`. SB0 erratum E2 is recorded in SB0's new errata section with Q-r1-1 as E1. New case A-29 (ix): B100 lock, toggle off → on, A90 lock, tip 80 trailing | §4, §6.2, §7.5, §9.1, §14, §15.1 A-20, A-21, A-23, A-27, A-29 (ix), §18 D19, D37; SB0 errata |
| **O-M2** no per-`MarkConflictingBlock` hook | `TipHost::conflicting_marked(chain, marked, was_in_chain)` runs after each call's status writes and before the next sibling. V performs Core's five steps in order there (candidate erase and mempool, re-add scan, `ConflictingChainFound`, both `UpdatedBlockTip` signals with `host.tip()`, `NotifyBlockTip` when in chain). `VirtualTip` records the tip for `updated_block_tip_schedules`, replacing the count. Sibling order is D9 effect (4). Test: A-36. ScriptedV asserts the hook sequence and tips against the harness signal trace | §4, §7.3, §15 (`cl` trace, ScriptedV), §15.1 A-36, §18 D9, D38 |
| **O-m1** A-4 compared `invalid`/`conflicting` from a header-only replay | A-4 projects every non-active status to `header-valid` and SB4 reports non-active tips as `headers-only`. Harness scenarios compare `invalid`/`conflicting` exactly. The exactly comparable fields are named | §10.2, §15, §15.1 A-4, §18 D40 |
| **O-m2** node status oracles included `FindMostWorkChain`'s propagation | Harness `cl` also emits statuses, tip and `m_best_header` at the `EnforceBlock` boundary (`--wrap` on `Chainstate::EnforceBlock`, called from `chainlock/handler.cpp:296`). Node rows compare there, and node scenarios keep non-floor branches data-less unless a row says otherwise | §15, §15.1 A-21, A-26, §18 D40 |
| **O-m3** node per-step snapshots inside `enforce_block` | Disconnects inside `enforce_block` are staged (own `seq` and F1 event). One snapshot is stored on return and the staged events are handed to SB5 in `seq` order in the same critical section. ABC and `InvalidateBlock` stay per step. A-26 is amended | §4, §7.3, §9.1, §15.1 A-26, §18 D28 |
| **O-m4** one `VirtualTip` per step | `chain.virtual_tip()` once per step at stage 1; every enforcement and (a)'s cursor use it. A-21a's variant checks a second enforcement in the same step | §4, §6.2, §7.2, §7.3, §9.1, §15.1 A-21a, §18 D39 |
| **O-m5** cold prev lacked `mtp`/`time_max` | `HeaderRecord` gains `time_max`; `mtp` is computed from the `RuleContext`. Test: A-38 | §4, §5.4, §15.1 A-38, §18 D35 |
| **O-m6** A-5…A-8 judged SB4 against SB4 | `complete()`'s targets are also compared with `dash_oracle_sys::Oracle::next_work_required` over the same consecutive headers: A-8's synthetic chain and A-5…A-7's boundary heights | §15.1 A-5–A-8 |
| **O-m7** checkpoint cache after SPV reopen | SB5's `open` rebuilds `highest_checkpoint` (one `by_hash` per checkpoint). The "anchored above every checkpoint" sentence is corrected: a pack header may be a checkpoint, and the rule stays inert. A §15.2 row is added | §1.1, §11.4, §15.2 |
| **O-m8** SPV status writes visible before commit | Every SPV status change after insertion goes through the writer's overlay. It is applied to resident seqlocked groups at stage 4 under the tip lock, and the LRU drops the evicted ids there. A cold read may see the committed row one step early. A-35's failure arm is amended; A-37 is new | §3.7, §9.1, §15.1 A-35, A-37, §18 D36 |
| **O-m9** API gaps | `fork_target(&vt) -> Result<ForkTarget, ChainError>`; `ChainAccess` implementers named (CH1-node's `ChainView` with `loading_blocks`, SB14's SPV adapter); `accept_header(header, hash)` and a public `publish_status()` for CH1-node's `AcceptBlock` | §4, §9.1 |
| **O-m10** `ResetBlockFailureFlags` conditions | Both loops are stated verbatim (`!IsValid()` at `VALID_TRANSACTIONS`; ancestors carrying `FAILED_MASK` or `CONFLICT`; `FAILED_CHILD` children promoted), returning `ResetReport` | §4, §7.4 |
| **n1–n9** | n1 §5.2 step 8 (no `AcceptedBlockHeader` after a conflict insert); n2 C1 split into C1a/C1b; n3 `GenesisMismatch`/`Empty` declared with D21; n4 §3.6 reworded (path always copied); n5 `fork_in_global_index` takes a lookup with the anchor fallback; n6 the SPV locator terminus is the anchor; n7 the transient live/observed difference in D12; n8 empty steps skip stages 3–5; n9 ScriptedV asserts equality with the trace | §3.6, §4, §5.2, §9.1, §10.1, §15, §17, §18 D12, D21 |

**Schedule.** C6 rises from 3.5 to 4.5 days, so the total is 24.5 days and implementer A's path is 16.5 days. `tools/cpm_plan.py` was rerun and no milestone moves (§17).

**Harness changes for `sb4-core-scenarios`** (the task started on §15 at 24098412):
1. `cl` mode: a `--wrap` on `Chainstate::EnforceBlock` emits statuses of every entry, the tip and `m_best_header` at the `EnforceBlock` boundary, before `EnforceBestChainLock`'s `ActivateBestChain`.
2. `cl` mode: a registered `CValidationInterface` emits the ordered signal trace: `SynchronousUpdatedBlockTip`, queued `UpdatedBlockTip` with `(new, fork-or-null, ibd)`, and `BlockDisconnected`.
3. New `cl` case A-29 (ix): fork at 80, B81…B110 active, A81…A95 headers. Lock B100, signed toggle off then on, then lock A90. Emit statuses, `m_best_header`, best CL, floor, `isEnabled` and the tip (80).
4. New `cl` case A-36: lock A100, B110 active (fork 80), stale C90 under A89, non-IBD. Emit the signal trace and the boundary statuses.
5. New `pnbh` case A-38: a stale branch forking 30 below the tip on regtest, including a `time-too-old` header at the MTP boundary.
6. A-5–A-8 use `dash_oracle_sys::Oracle::next_work_required` (no harness mode).
7. Node scenarios keep non-floor branches data-less unless stated.
8. A-4's projection maps `invalid` and `conflicting` to `header-valid`. This is comparison-side only.

## 23. rev4 closure map

Sol r4 (`reviews/SB4-design-r4-sol.md`) against rev3 36266b40, REWORK; DEC-140 binding. It confirmed O-M1's (b′) selection, the leaf proof, the reset witness, O-M2's `conflicting_marked` order with A-36, and O-m3's snapshot and event staging. None of those changed; only the two items below did.

| Finding | Fix | Sections |
|---|---|---|
| **R4-M1** E2's unchanged wallet rule is unsafe across a lost spork-off suffix | SB0 erratum **E3**: each committed wallet sync record carries a Core-shaped block locator (`LocatorEntries` density: ≤ 11 + ⌈log2 h⌉ + 1 entries, 33 at mainnet heights, about 1 KiB). On reopen with an unknown hash, the client rewinds to the first locator entry present on the restored active chain, falling back to the anchor. The floor is no longer a recovery bound, and DEC-115's unknown-hash rule and E2's wallet bullet are superseded. Write order: header step committed, then: The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. (DEC-144 deleted rev4's "with or after", §24) Core's mechanism is cited. Sol's counterexample is a §15.2 row (rewind to 37, B60 dropped) | SB0 §9.5, §17 E3 (pointers at the recovery rule, the synced-hash item, the ordering item, §4.6's wallet line and E2); SB4 §15.2, §18 D41, §19 |
| **R4-M2** node staging let `subscribe_at` admit an unpublished intermediate tip | `enforce_block` holds the tip mutex as one owned guard from entry to return. All staged disconnects, the final snapshot store and the event handoff happen under it, so `subscribe_at`/`finish()`, `deregister` and `publish_status` see only the pre- or post-enforcement cut. Lock-free readers are unchanged. Callback contract and flush-check deferral are in D42 (the deferral is withdrawn in rev5, §24). The DEC-132 reduction, O-m3's text, A-26, the SPV handoff note, R6, and the lock table and lock order are updated. Sol's B109 interleaving is A-39 | §4, §7.3, §9.1, §9.3, §13.1, §15.1 A-26, A-39, §18 D28, D42, §19 |

**Schedule.** Unchanged. The node composite holds one guard instead of re-locking per disconnect, and A-39 is one test, both inside C6's 4.5 days. The wallet half of E3 is SB14's (key-wallet-manager). No estimate changed, so `tools/cpm_plan.py` was not rerun.

**Harness.** No `sb4-core-scenarios` mode or case is added or changed. A-39 is a Rust concurrency test, and the E3 row's locator is checked against Core's `LocatorEntries` density through the existing `locator` mode.

## 24. rev5 closure map

Sol r5 (`reviews/SB4-design-r5-sol.md`) against rev4 e3d2d9ad, REWORK; DEC-144 binding. It confirmed the locator mechanism, the rewind to 37, the size bound and seam, and the continuous guard with A-39. None of those changed; only the two items below did.

| Finding | Fix | Sections |
|---|---|---|
| **R5-M1** E3 allowed effects to commit before their checkpoint | rev4's "with or after" is deleted. The operative rule, word for word in SB0 E3, its §9.5 ordering pointer, D41, §15.2 and §23, is: The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. Core's equivalent, `CWallet::LoadToWallet`'s revalidation (`wallet/wallet.cpp:1142-1167`), is cited. Sol r5's cut is a §15.2 row (`wallet_effects_checkpoint_one_txn`) that asserts the single transaction plus crash cuts before and after it. The r4 fixture's suffix is now committed Buffered but not Durable | SB0 §9.5, §17 E3; SB4 §15.2, §18 D41, §19, §23 |
| **R5-M2** D42 deferred pressure checks across an unbounded disconnect leg | D42's deferral is withdrawn. CH2's IF_NEEDED check, with its CRITICAL/EVO_CRITICAL wait and freeze, runs after every disconnect inside the hold at its existing site, as in Core's `DisconnectTip`. New §13.4 holds the lock-order contract: W never takes the tip mutex, the feed/hub lock or a lock V holds (H1); the barrier goes through a W-owned channel that V drains after the hold (H2); V's wait services `PruneClears` (H3); feed and subscriber progress are mutex-free (H4, H6); events arrive at their step (H5, new D43). The lock table and lock order are updated. Tests: A-40 (CRITICAL wait under the hold with F in flight completes; a negative control detects a violating W) and A-41 (Sol's 2,000-block witness: a check at every boundary, Σ ≤ B + 72,000 B). A-26 is amended | §4, §7.3, §9.1, §13.1, §13.4, §15.1 A-26, A-40, A-41, §17, §18 D28, D42, D43, §19 |

**Schedule.** C6 rises from 4.5 to 5 days, so the total is 25 days and implementer A's path is 17 days. `tools/cpm_plan.py` was rerun: SB4 is still class L, so SB4 lands in week 8.5, SB5 11.0, CH1-node 11.5, M2 18.5 and M5 exit 39.5, all unchanged.

**Harness.** No `sb4-core-scenarios` mode or case is added or changed. A-40 and A-41 are Rust tests against a model flush writer, which CH2b repeats with the real W.

## 25. rev6 closure map

Sol r6 (`reviews/SB4-design-r6-sol.md`) against rev5 45d9e640, REWORK; DEC-146 binding. It confirmed DEC-144 (1) (one wallet transaction), DEC-144 (2) (the per-disconnect check under the hold), H1–H6 as necessary progress paths, A-41's arithmetic, and DEC-145 (D43, with Core's separate `cl-schdlr` and validation scheduler threads). None of those changed; only the items below did.

| Finding | Fix | Sections |
|---|---|---|
| **R6-M1** a closing mandatory subscriber stayed a W target after H4 dropped the event it needed | New **H7**. The `closing` store is the single linearization point that retires the slot from W's ack set and rewind gates, both those already captured (including the one W waits on) and every capture or gate created later during the hold. W's predicate counts `closing` as satisfied and is re-checked before each wait and on wake, with deregistration waking W. Capture and gate creation filter on `closing`, and either race order gives the same obligation set. The prune lock stays at the last durable locator until deregistration completes after the hold, and the durable locator does not advance. No event preservation is needed. H2, H4 and H6, the lock table and the lock-order argument are updated. A-40 is strengthened to Sol's schedule: a mandatory Synced S closes with q + 1 pending, closing comes before and after capture and gate creation, and a second CRITICAL wait under the hold. It asserts that no W target depends on a removed event, and keeps the violating-W control plus a new ignores-`closing` control | §13.1, §13.4 H2, H4, H6, H7, §15.1 A-40, §18 D44 |
| **R6-m1** `FlushCommit.tip_seq` pointed at the published snapshot | V captures `(new, tip_seq)` = the private tip and the last assigned `seq` into `FreezePacket` under its guard, and W uses only the packet. §9.2 is reworded, SB0's F2 row gets erratum E4, and A-40 (F1 = (B109, q + 1) while the snapshot says q) and A-41 assert the pairing | §9.2, §13.4 H2, §15.1 A-40, A-41, §18 D45; SB0 §6.2 F2, §17 E4 |
| **R6-n1** D41 lacked the sentence's final period | Period inserted | §18 D41 |

**Schedule.** Unchanged. H7 is SB5's flag and wake plus CH2b's predicate, and the A-40 strengthening fits C6's 5 days, so `tools/cpm_plan.py` was not rerun.

**Harness.** No `sb4-core-scenarios` mode or case is added or changed.

## 26. rev7 closure map

The `sb4-core-scenarios` harness (`node/sb4-core-scenarios` @ 42737ddf; 38 vectors in `dash-chain/tests/data/core-scenarios/`) judged rev6's narrative against Core v24.0.0-rc.3. The vectors win. rev6's §13.4 and §9.2 (R6-M1, R6-m1) are unchanged and go to Sol r7 together with this round. Historical closure rows in §20–§25 keep their text; this map supersedes them where they differ.

| Harness finding | Fix | Sections |
|---|---|---|
| **1. A-29 (ix) mechanism.** The B100 lock is on the active chain, so `EnforceBlock` marks nothing and A81 is never marked; the A90 lock marks B81…B110 and the tip ends at A80 under floor A90 | (ix) is rewritten to `a29-ix-reset-then-lower-lock`. The node tip trails at A80 because A81…A95 have no data. SPV's header-only target is **A95, `Descends`**, the tip a node reaches once they download (§6.2's data-dependent difference). `a29-ix-no-observed-off` is added: with no check while off, nothing resets and A90 is `Stale`. The kept-mark trailing case is still needed as the witness for (b′) over rev2's (b) (D37) and for A-23's fixed counterexample, so it becomes **A-29 (x)**. The connected-A fixture below supplies the kept-mark witness, specified for the harness as `a29-x-kept-mark-trailing` (§15). (rev7.1: an "only if active" restriction stood here and was withdrawn per Sol r7 R7-m1; see §27.) | §6.2, §7.5, §15, §15.1 A-23, A-29, §18 D37; SB0 §17 E2 example |
| **2. "Recorded while off"** is reachable only through `submitchainlock`, because the P2P CLSIG is dropped unread | §7.1 and §7.5 now carry a CLSIG intake row (P2P live-gated at `net_processing.cpp:5794-5795`; RPC ungated at `rpc/quorums.cpp:1397`). Rows A-29 (i), (ii) and R2-M2 name the `cl via` path. SPV has no RPC, so its intake drops a CLSIG unread while its live spork is off. That is consistent with DEC-123 Q2, which made SPV follow Core's SPORK_19 semantics and withdrew rev0's always-on proposal (D12, D31); always-on would be the rev0 premise, not the decided one | §7.1, §7.5, §15, §15.1 A-29, §21 R2-M2 |
| **3. `m_best_header` quirks** | §6.1 already ported the walk-back as "is `best_header`", and §7.3 already kept it at the fork. rev7 states all three as "nothing else moves it", each with its vector: walk-back only on identity (`a17-conflict-then-reload`), stays at the fork after an enforcement reorg (`a26-ii-enforcement-reorg`, A990 while the tip is A1000), not restored by `reconsiderblock` (`a18-invalidate-walkback`, A98). New test `bh_reconsider_keeps` in A-18; A-26 asserts the fork-point `best_header` | §6.1, §15.1 A-18, A-26 |
| **4. CONFLICT not persisted** | Already stated (§7.3, D15); the vector is cited. A-17 is rewritten from "stored with CONFLICT" to the vector's sequence: B96 130 → 2 across `reload`, `best_header` B110 | §7.3, §15.1 A-17, §18 D15 |
| **5. Equal-work ties after reload** | §6.1 and D9 state that these ties are not Core-determinable. `reload` emits `address_tie`, no vector asserts a winner, and SB4 checks only membership in the tied set (`a19-first-seen-tie`) | §6.1, §15, §15.1 A-19, §18 D9 |

**Harness case changes to forward.** One new scenario: **`a29-x-kept-mark-trailing`** (§15 lists the script and the expected values read from Core). There is no new mode: §15 now documents the `cl via` option and `reload`'s `address_tie` output, both already implemented at 42737ddf. No existing vector changes.

**Schedule.** Unchanged. (x) is one more case in C6's `spork19_transitions`, and `bh_reconsider_keeps` is a few lines in C4's best-header tests. The harness scenario is a JSON script in the harness row. `tools/cpm_plan.py` was not rerun.

## 27. rev7.1 closure map

rev7.1 folds in two items. DEC-147 (b685e157) rules the harness's Sol r1 R1-M1: dashd runs a timing-dependent number of enforcement passes after a lock on a branch off the active chain, so the vectors are state oracles. Sol r7 on rev6+rev7 (0ae6a19b, APPROVE-WITH-CHANGES, gate yes) closed every r6 finding, confirmed all of rev7's Core expectations, and left one minor, R7-m1.

| Finding | Fix | Sections |
|---|---|---|
| **R1-M1** (harness Sol r1) §15's comparison and `ScriptedV` asserted the exact trace and hook count, i.e. one sample of dashd's timing | §15's comparison is split into three parts. **Exact:** state. **Exact trace prefix:** through each lock's first enforcement boundary and its ABC disconnect/connect order. **Allowed repeats:** zero or more idempotent passes, after the prefix. `ScriptedV` compares the prefix, skips repeats and checks the idempotence of its own. The provenance names the delivery policy. A-21a's same-step variant no longer asserts "no notification", and A-26 (i)/(ii) and A-36 assert the first pass only. New D46 | §15 (signal trace, comparison, `ScriptedV`), §15.1 A-21a, A-26, A-36, §18 D46, §19 |
| **R7-m1** (Sol r7) rev7 said a kept mark needs the floor's branch to have been active; `EnforceBlock` marks every sibling on the locked path, active or not (`validation.cpp:4057-4064`, `:4120-4122`) | The "only if active" statements in §6.2 and §26 now read: "The connected-A fixture below supplies the kept-mark witness." §7.5 says a sibling on the future floor's path can keep a mark, the connected-A case included. The (ix) control now reads: a lock whose block is already on the active chain makes no marking walk. (x)'s script and values are unchanged. New **(x′)** is the headers-only variant, which Sol replayed with A81 at 130 and SPV trailing at A80; it is specified for the harness as `a29-xp-kept-mark-headers-only`. SB0 E2's parenthesis is reworded to match | §6.2, §7.5, §15, §15.1 A-29, §19, §26; SB0 §17 E2 |

**Evidence carried in.** Sol's R1-M2 probe ran rev7's `a29-x-kept-mark-trailing` script and reproduced §15's expected values (A81 stays 157, B81 130, B82…B110 2, tip A80, `best_header` B110, floor A90). The scenario still has to be committed to the harness branch, which is harness-side work under R1-M2.

**Schedule.** Unchanged; the change narrows assertions. `tools/cpm_plan.py` was not rerun. **Harness:** one new case from SB4, `a29-xp-kept-mark-headers-only` (A-29 (x′); the manager is requesting it). DEC-147's controlled delivery and provenance field are the harness's R1-M1 fix.
