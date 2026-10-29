# P3: block download, stalling, the compact-block client, block-relay-only, IBD from the network

Status: **accepted, rev11** (Sol r11 ACCEPT WITH NITS, nits applied; from node/integration 657a28c28).
PLAN row P3 (PLAN.md:607), PRD FR-P2P-08 and behaviour 24. Crate: `dashd-p2p`. Decisions are
recorded as D-rows in [P3-notes.md](P3-notes.md).

**Citations.** Core is Dash Core v24.0.0-rc.3 (`~/workspace/dash`, tag `v24.0.0-rc.3`):

- `np` = `src/net_processing.cpp`, `nph` = `src/net_processing.h`;
- `be` = `src/blockencodings.cpp`, `v` = `src/validation.cpp`;
- `net` = `src/net.cpp`, `neth` = `src/net.h`;
- `tm` = `src/txmempool.cpp`, `init` = `src/init.cpp`.

Rust paths are relative to the repo at 657a28c28. "P2a-core" is the merged processor core
(`dashd-p2p/src/processing/`, train 14); its D-rows are cited as P2a-core Dnn. CH1-node's design
is `CH1-node.md`, and its lock rules are LO-n.

---

## 0. Summary

1. P3 implements P2a-core's `BlockSync` seam (`processing/seams.rs:242-294`) as `BlockDownload`. It
   holds Core's `mapBlocksInFlight`, each peer's `vBlocksInFlight`, `mapBlockSource`, the stall state,
   `m_block_stalling_timeout`, `m_peers_downloading_from` and `lNodesAnnouncingHeaderAndIDs`.
   It also adds the `block`, `cmpctblock` and `blocktxn` handlers, replacing the registry stubs
   (`registry.rs:146,157,160`).
2. Blocks go to V through a new `ChainAccess::process_block`, which is CH1-node's
   `ChainCommand::ProcessBlock { reply: BlockReply::Activated }`. The actor blocks on the reply, as
   Core's `msghand` blocks in `ProcessNewBlock` (np:3720), and it does not hold the peer's state
   lock while it waits.
3. **D65 option: the tail runs after delivery.** While it waits on V, the actor services V's
   queued `BlockChecked` signals in V's order. It drains them once more after the reply, then runs
   `ProcessBlock`'s tail (np:3717-3732). Core runs `BlockChecked` inline in `ProcessNewBlock`
   before the tail; this keeps that order. `block_checked` gets V's `ibd` argument. The signal
   queue is lossless and bounded. Over its bound, V waits only at Core's
   `LimitValidationInterfaceQueue` release points, never during the startup hold or before the
   actor's first drain. Those waits are an LO-9 exception ruled as DEC-229, amended by DEC-230
   for the pre-start queue (contract C5, §2.5, §3.8.1). **`NewPoWValidBlock` runs whole on V's
   thread, inside `AcceptBlock`, as in Core (D50).** It builds the compact block once, visits the
   connection table in Core's `ForEachNode` order, reads each peer's header state through a
   per-peer leaf cell, and pushes `cmpctblock` straight to that peer's send queue, before
   `SaveBlockToDisk` and before ABC. The actor holds a peer's header cell wherever Core's
   `msghand` holds `cs_main` for that peer, and never while it waits on V. `UpdatedBlockTip`
   keeps P2a-core's per-peer announcement cell, now a `RelayList`, and the head of
   `send_messages` reads the cell and its chain snapshot as one observation, as Core's section 7
   reads them under `m_block_inv_mutex` (DEC-236). Every drain applies only the entries its own
   chain snapshot covers. The actor enters a header cell only with a snapshot that shows every
   `NewPoWValidBlock` call already begun (the announce stamp), and it reads that stamp after its
   copy of `recent`, so the copy and the snapshot agree. And before an accept outside IBD,
   V waits until the actor has processed every queued `BlockChecked`. So the compact
   announcement, `BlockChecked`'s `sendcmpct` and section 7 keep V's order on every connection
   (C5′, C6, C7, C8, §3.6, D37, D44, D50, D55, D56, D58).
4. Download scheduling, the getdata condition, staller detection, the 2→64 s stalling timeout with
   0.85 decay, and the per-block timeout are ports of np:1477-1559, np:6752-6839 and np:2200-2207.
   They keep Core's statement order, log strings, and the shared `vGetData` batch with objects.
5. On the compact-block client side, the high-bandwidth peer list is a byte-exact port of
   `MaybeSetPeerAsAnnouncingHeaderAndIDs` (np:1360-1416), including the stale ids Core never removes.
   `PartiallyDownloadedBlock` is a port of be:55-223, including the libstdc++ bucket-size test
   (be:88-105). The `cmpctblock` handler follows np:5052-5293 and `blocktxn` follows np:3734-3816.
6. There is no mempool before MP1a and nothing feeds the extra-transactions ring before P5. The
   mempool source is a `CompactTxSource` trait whose M3 implementation is empty. P3 owns the ring
   and its `AddToCompactExtraTransactions` entry point; P5 and MP1a add Core's call sites. So every
   compact block requests all its non-prefilled transactions with `getblocktxn`, which is what a
   Dash Core node with an empty mempool **and** an empty extra ring sends. A peer that first relays
   a transaction (valid, orphan, or small and rejected) can tell the difference. That is an interim
   deviation registered for ALLOWED-DIFFS, and the compact-client UA-parity claim waits for P5 and
   MP1a (§3.5.5, D19).
7. Block-relay-only connections follow Core's rules: they download blocks like full outbound peers,
   take part in HB selection, and have no tx relay. P3 adds no rules of its own.
8. Verification:
   - L0 units;
   - `sim` ports of the three FTs, step for step (P3's own acceptance at M3);
   - an oracle session differential `diff_p2p_multi_blocks`, which needs a new O1c block-step entry
     point;
   - a `be` differential fuzz target over a new oracle entry point (E4). It and the compact steps
     of `diff_p2p_multi_blocks` are required before S3 and S4 report READY (D21);
   - captured dashd transcripts for getdata batching and stall timing;
   - a public-testnet IBD harness to height 6,500, below CH3's readiness guard at DIP3 (7,000);
   - the unmodified Python `p2p_block_sync` and `p2p_ibd_stalling`, as late-closing acceptance once
     O11, P2b and RP1a land (`p2p_compactblocks` closes in P4's row).
9. There are six slices (S1–S6). S6 composes `dashd-p2p` into `rdashd` (manager ruling on OQ1).

---

## 1. Scope and non-goals

### 1.1 In scope (PLAN P3, FR-P2P-08, behaviour 24)

| Area | Core | P3 deliverable |
|---|---|---|
| In-flight state | `QueuedBlock` np:227-232; `CNodeState` fields np:487-490; `mapBlocksInFlight` np:1075-1076; `m_peers_downloading_from` np:1112; `IsBlockRequested`/`IsBlockRequestedFromOutbound`/`RemoveBlockRequest`/`BlockRequested` np:1271-1358; `FinalizeNode`'s part np:1804-1820 | `processing/download.rs` `BlockDownload` |
| Scheduling | `FindNextBlocksToDownload` np:1477-1559; section 12 np:6820-6839 | `BlockSync::getdata_section` |
| Stalling and timeouts | section 9 np:6752-6781; decay in `BlockConnected` np:2200-2207 | `BlockSync::stall_section`, `Shared::block_connected` |
| Direct fetch | `HeadersDirectFetchBlocks` np:3202-3257 | `BlockSync::headers_direct_fetch` |
| `block` | np:5348-5387, `ProcessBlock` np:3717-3732 | `processing/blocks.rs` |
| Compact client | `cmpctblock` np:5052-5293, `blocktxn` np:5295-5307, `ProcessCompactBlockTxns` np:3734-3816, `PartiallyDownloadedBlock` be:55-223 | `processing/blocks.rs`, `processing/compact.rs` |
| HB selection | `MaybeSetPeerAsAnnouncingHeaderAndIDs` np:1360-1416 from `BlockChecked` np:2314-2343 | `BlockDownload::block_checked` |
| Announcing compact blocks | `NewPoWValidBlock` np:2228-2268; the cmpctblock branch of section 7 (P2a-core's `push_compact_announcement`) | `BlockDownload` (D3) |
| `BlockChecked` | np:2314-2343 | `BlockDownload::block_checked` |
| IBD from the network | ProcessBlock through V | `ChainAccess::process_block`, the two-phase handler (§2.6) |
| `rdashd` wiring | — | S6 (manager ruling on OQ1) |

### 1.2 Not in scope

- **P2a-core owns:**
  - headers and headers2 sync, `MaybeSendGetHeaders`, block availability (`ProcessBlockAvailability`
    and `UpdateBlockAvailability`, `sync.rs`);
  - the sync start and `sync_blocks_and_headers_from_peer` (np:6341-6396, `sync.rs:363-406`);
  - the headers-sync timeout (np:6783-6813, `sync.rs:542-575`);
  - `ConsiderEviction` (np:6817), `TipMayBeStale` (np:1418), and the `inv` block path (np:4507-4611);
  - `SENDCMPCT` intake (np:4273-4288) and `sendcmpct(0, 1)` at VERACK (np:4215-4222).

  P3 reads their state through `SyncCx`. Brief: "including the headers-based timeout". The headers
  timeout is P2a-core's code. P3 owns the block-download timeout, whose deadline is
  header-spacing-based (`nPowTargetSpacing × (1 + 0.5·N)`, np:6773-6781), and it checks that the two
  sit in Core's order (§3.3).
- **P4 owns serving:** `getdata MSG_CMPCT_BLOCK` (np:2865-2881 inside `ProcessGetBlockData`), the
  `GETBLOCKTXN` handler (np:4704), and `MSG_BLOCK`/`MSG_FILTERED_BLOCK` serving.
- **P5 and MP1a own** transaction relay and the two call sites of `AddToCompactExtraTransactions`:
  an orphan accepted into the orphanage (np:4997-4999), and a rejected transaction whose
  `RecursiveDynamicUsage` is under 100,000 (np:5016-5020). P3 owns the ring and the entry point
  (np:1922-1931, §2.3.4). These are asks A-6b and A-7, and the orphan-then-compact sequence becomes
  a P5 acceptance row (§10).
- **MP1a owns** the mempool. P3 only defines the `CompactTxSource` read trait (§2.3.3).
- **`BlockDisconnected` has no P3 effect.** Its body (np:2210-2221) resets P5's
  recently-confirmed filter, which is P2a-core D69's follow-up.
- **V owns** the validity of a block. P3 never decides it. `IsBlockMutated` is a pure check called
  before V (np:5365) and inside `FillBlock` (be:211-213).
- **Not P3:** `-blocksonly` init interactions (ND1), `getblockfrompeer` (RPC; Core v24 has
  `FetchBlock` np:2057-2089, so P3 exposes `fetch_block` for RP1x and does not implement the RPC), and
  the assumeutxo background chainstate (Core v24 Dash has none).

### 1.3 Acceptance mapping

| PLAN acceptance | Evidence (§6) | When |
|---|---|---|
| IBD of testnet from public peers | §6.7 harness. Real chainstate to height 6,500 at M3 (CH3's readiness guard refuses DIP3 at 7,000 until CH7a), then to the testnet tip after CH7a | M3 run in S6; full-tip run late-closing |
| FT `p2p_ibd_stalling` | `sim` port (S2), then the Python FT on rdashd | S2; Python late-closing |
| FT `p2p_compactblocks` (client) | `sim` ports of the client subtests (S3/S4) are P3's evidence. The unmodified Python file is one sequential `run_test` that mixes P4's server subtests in (§6.6), so it closes in **P4's** row (manager ruling on OQ2, D18) | S3–S4; the Python file with P4 |
| FT `p2p_block_sync` | 3-node `sim` port (S2), then the Python FT | S2; Python late-closing |
| PRD behaviour 24 | §6.2 L6 rows plus the differential (§6.4) | S2–S4 |
| BG `ibd-testnet-seed` (window) | rdashd runs node-bench's workload argv (§6.8); the manager runs the window at H ≤ 6,500 once S6 has a binary | S6 makes it runnable |
| UA-indistinguishability | Download, stall, timeouts and getdata batching: transcripts (§6.3) and `diff_p2p_multi_blocks` (§6.4). Compact negotiation and reconstruction: the sim ports, E4's `diff_cmpct_initdata` and the compact steps of `diff_p2p_multi_blocks`, all required (D21). Accept-to-announcement timing: the real-V probe (§6.3 item 6) for P2P and RPC origins, idle, script-heavy and under disk pressure, which keeps the high-bandwidth `cmpctblock` ahead of the write and of `ConnectTip` and reports the distributions for NFR-FP-02 (D50, D52). The compact-client claim completes only after MP1a and P5 (§3.5.5, D19) | S2; S3–S4; the probe's order in S4, its numbers in S6; compact parity after P5 and MP1a |

The late-closing pattern follows CH3a D55: a slice is READY on its own evidence, and the row closes
when the dependent harness lands (OQ2).

---

## 2. Data model and public API

### 2.1 Ownership map

```
             ┌────────────────────── PeerManager actor (one thread, P2a-core) ──────────────────────┐
 wire ──►    │ process_messages(peer)                         send_messages(peer)                    │
 RawMsg      │   block / cmpctblock / blocktxn  (P3, blocks.rs)   s7  push_compact_announcement (P3)   │
             │     phase 1 under PeerState lock ─► BlockStep      s9  stall_section            (P3)   │
             │     phase 2 unlocked: process_block ─► wait ◄─┐    s10 headers timeout   (P2a-core)    │
             │        services SyncSignals while waiting     │    s11 ConsiderEviction  (P2a-core)    │
             │     phase 3 relock: ProcessBlock tail         │    s12 getdata_section          (P3)   │
             │                                               │                                        │
             │ BlockDownload (P3, actor-private; &mut via PeerLogic)                                  │
             │   in_flight: HashMap<BlockHash, SmallVec<[InFlight;3]>>  peers: HashMap<PeerId,PeerDl> │
             │   block_source, hb_list: VecDeque<PeerId>, stalling_timeout: Arc<AtomicI64>          │
             └──────────────────────────────────────────────┬─────────────────────────────────────────┘
                              ChainAccess::process_block    │   Shared::block_checked (leaf queue + doorbell)
                              (ChainCommand::ProcessBlock,  ▼   Shared::new_pow_valid_block (V's thread, D50)
                               BlockReply::Activated)      V (CH1-node actor) ── synchronous signals inline (LO-5)
                                                             └─ queued: UpdatedBlockTip, BlockConnected (signals thread)
```

`BlockDownload` is owned by the actor. Only `&mut self` hooks reach it, so it takes no lock, as
Core's state is all `GUARDED_BY(cs_main)`. Other threads see two mirrors:

- each peer's in-flight heights, kept in a leaf cell for `getpeerinfo` (§2.7);
- the stalling timeout, an `AtomicI64`, which the signals thread decays.

### 2.2 `BlockDownload` (`processing/download.rs`)

```rust
/// Core's block-download state (np:227-232, 487-490, 982, 1075-1076, 1109, 1112), actor-private
/// here. Core guards it with `cs_main`, except `vExtraTxnForCompact` (`g_msgproc_mutex`, np:1129)
/// and `m_most_recent_*` (`m_most_recent_block_mutex`, np:1038-1041), which the port keeps in
/// `Shared::recent` (§2.5).
pub struct BlockDownload {
    /// `mapBlocksInFlight`: per hash, the requesting peers in insertion order (a C++ multimap keeps
    /// equal keys in insertion order, so `lower_bound(h)->second.first` is element 0, np:1554).
    in_flight: HashMap<BlockHash, SmallVec<[InFlight; 3]>>,
    /// Per-peer `vBlocksInFlight` + `m_downloading_since` + `m_stalling_since`; created on the
    /// first request and removed in `finalize` (np:1783). Core's `CNodeState` exists from
    /// `InitializeNode` (np:1745); every reader here treats an absent entry as an empty one.
    peers: HashMap<PeerId, PeerDownload>,
    /// `mapBlockSource`: hash → (peer, `true` if the full block came by `block`; `false` for a
    /// compact reconstruction, so `BlockChecked` punishes with `via_compact = !second`, np:2324).
    block_source: HashMap<BlockHash, (PeerId, bool)>,
    /// `lNodesAnnouncingHeaderAndIDs` (np:1109). Never pruned in `finalize` (Core quirk Q4).
    hb_list: VecDeque<PeerId>,
    /// `m_peers_downloading_from` (np:1112).
    peers_downloading_from: i32,
    /// `mapBlocksInFlight.size()`: the multimap's entry count (Q10), kept as a counter.
    in_flight_entries: usize,
    /// `m_block_stalling_timeout` (np:982), seconds; shared with `Shared::block_connected`.
    stalling_timeout: Arc<AtomicI64>,
    /// `m_highest_fast_announce` and `m_most_recent_*` (np:1038-1044) live in `Shared::recent`
    /// (§2.5), because `NewPoWValidBlock` writes them on V's thread (D50).
    /// Section 7's cmpctblock and, from P4, getdata serving read them there.
    recent: Arc<Mutex<RecentBlock>>,
    /// `vExtraTxnForCompact` (np:1129-1131): P3's container; the call sites are P5's and MP1a's.
    extra_txn: ExtraTxns,
    /// The mempool for reconstruction (MP1a; `NoMempool` until then).
    tx_source: Arc<dyn CompactTxSource>,
    /// `m_ignore_incoming_txs` (`-blocksonly`, np:851; init:1657).
    ignore_incoming_txs: bool,
}

struct InFlight { peer: PeerId, seq: u64 }          // `seq` identifies the peer's list entry

struct PeerDownload {
    inbound: bool,                                   // `m_is_inbound`, fixed at connect
    blocks: VecDeque<QueuedBlock>,                   // `vBlocksInFlight`, front = oldest
    downloading_since_us: i64,                       // `m_downloading_since`
    stalling_since_us: i64,                          // `m_stalling_since` (0 = not stalling)
    heights: Arc<Mutex<Vec<i32>>>,                   // stats mirror (§2.7), leaf lock
}

struct QueuedBlock {                                 // np:227-232
    seq: u64,
    id: BlockId,                                     // `pindex`
    hash: BlockHash,
    height: i32,
    partial: Option<Box<PartiallyDownloadedBlock>>,  // `partialBlock`
}
```

A list-entry identity is needed because Core erases `vBlocksInFlight` entries by iterator
(np:1312) and `mapBlocksInFlight` entries by iterator (np:1320, np:1811). `seq` is a per-`BlockDownload` counter. Erasing by `seq` from a `VecDeque` of at
most 16 entries is a linear scan, which is cheaper than Core's `std::list` node allocation.

**Methods.** These are Core names in snake case, with the same arguments:

- `is_block_requested(&BlockHash)` (np:1271)
- `is_block_requested_from_outbound(&BlockHash)` (np:1276)
- `remove_block_request(&BlockHash, Option<PeerId>, now_us)` (np:1287)
- `block_requested(peer, &EntryFacts, want_partial) -> (bool, Option<&mut QueuedBlock>)` (np:1325)
- `find_next_blocks_to_download(cx, count, out, staller)` (np:1477)
- `maybe_set_peer_as_announcing_header_and_ids(cx_or_core, peer)` (np:1360)

`remove_block_request` takes `now_us` because Core reads `GetTime<microseconds>()` inside it
(np:1310). Here the time comes from `dash-clock`'s mockable node clock (P2a-core passes it to every
hook).

**`BlockSync` implementation.** This fills in the seam methods P2a-core defined (§3); its trait
shape stays as it is, except for the additions in §2.5.

### 2.3 Compact blocks (`processing/compact.rs`)

#### 2.3.1 `PartiallyDownloadedBlock` (be:55-223)

```rust
pub struct PartiallyDownloadedBlock {
    header: Option<BlockHeader>,                     // `header`, None = `IsNull()`
    txn_available: Vec<Option<Arc<Transaction>>>,
    prefilled_count: usize, mempool_count: usize, extra_count: usize,
}
pub enum ReadStatus { Ok, Invalid, Failed }          // `READ_STATUS_*` (blockencodings.h:83-87)

impl PartiallyDownloadedBlock {
    pub fn init_data(&mut self, cmpct: &CmpctBlock, keys: ShortIdKeys,
                     pool: &dyn CompactTxSource, extra: &ExtraTxns) -> ReadStatus;   // be:55-174
    pub fn is_tx_available(&self, i: usize) -> bool;                                // be:177-183
    pub fn fill_block(&mut self, missing: Vec<Arc<Transaction>>) -> (ReadStatus, Option<Block>); // be:185-223
}
```

`ShortIdKeys` is `dash_p2p_core::msg::compact::CmpctBlock::short_id_keys()`, the base-sdk pin
8382b76: SHA256 of the header and nonce gives `k0` and `k1` (be:37-45). The short ID is
`short_id_with_keys(k, &instance_hash)`, which is SipHash-2-4 masked to 48 bits (be:48-51). Short
IDs hash `GetInstanceHash()` (be:33). That is the txid for every type except asset unlock v2, whose
instance hash includes the signature (A1 §2.14).

#### 2.3.2 The libstdc++ bucket test (be:88-105)

Core builds `std::unordered_map<uint64_t, uint16_t> shorttxids(n)` and returns `READ_STATUS_FAILED`
when, after inserting short ID `i`, the bucket of `i` holds more than 12 elements. The result depends
on libstdc++'s bucket count and on `std::hash<uint64_t>`, which is the identity:

- `bucket_count = _Prime_rehash_policy::_M_next_bkt(n)`. That is the smallest prime ≥ n from
  libstdc++'s `__prime_list`, except that n ≤ 13 uses the small fast table (`hashtable_c++0x.cc`).
- `bucket(x) = x % bucket_count`.
- With `max_load_factor` 1.0, inserting n elements into n or more buckets never rehashes.

`ShortIdIndex` emulates this. It holds a `Vec<u32>` of per-bucket counts at `bucket_count` and a
`HashMap<u64, u16>` for the lookup. It reports `Failed` at the same insertion as Core, and also on a
duplicate short ID (be:107-110). A duplicate key does not bump its bucket's count, as re-assigning
an existing key in Core's map adds no element.

**Whose table (D7, D22).** The reference is the Linux release build, because macOS libc++ uses a
different policy. The v24.0.0-rc.3 Linux release `dashd` is built with GCC 13.3.0 (its `.comment`
section) and links libstdc++ statically (`ldd` lists none), so the release's policy is inside the
binary. S3 takes the table from there. It reads all 305 entries of `__prime_list` (304 primes
plus the repeated sentinel that `_M_next_bkt` relies on as `__last_prime`; 0x988 bytes on LP64) and
`__fast_bkt` from the release binary's read-only data, and `_M_next_bkt` follows GCC 13.3.0's
`hashtable_c++0x.cc`: n = 0 gives 1 bucket, n ≤ 13 uses the small table, and larger n takes the
`lower_bound` from `__prime_list[6]`. At design time the release's two tables are byte-identical to
those in this box's GCC 15.2 `libstdc++.so.6.0.35`. The GCC 15 probe is diagnostic only: it shows
whether the toolchains agree and never defines the table. The table is checked into `compact.rs`
with its extraction script.

The emulation is held to the release in three ways:

- **Exhaustive sweep.** For every n in 0..=65,535 (Core's decoder rejects more than 65,535,
  blockencodings.h:120-122), the emulated `bucket_count(n)`, after construction and after n
  distinct inserts, equals `bucket_count()` of a real `std::unordered_map<uint64_t, uint16_t>(n)`
  at the same two points, with no rehash. The reference is a probe built with GCC 13.3.0 and
  linked statically, in the pinned `gcc:13.3.0` container that E4's oracle also uses; the same
  probe built with this box's GCC 15 runs as a diagnostic (D31). Comparing with the extracted
  table alone would only test the `lower_bound` rule against itself.
- **Transition fixtures.** Take every n where `bucket_count` changes, including the small-table
  steps and the start at `__prime_list[6]`, and test n−1, n and n+1. For each n ≥ 13, craft one set
  of n short IDs with 13 in one bucket (`Failed` at the 13th insertion) and one with 12 (no
  `Failed`). These run black-box against the release regtest `dashd` through the transcript tool
  (§6.3, transcript 5). `Failed` makes Core send `getdata(MSG_BLOCK)` (np:5178-5183); otherwise it
  sends `getblocktxn` (np:5198-5202).
- **E4.** The same inputs, and fuzzed ones, run through `diff_cmpct_initdata` (§6.4), whose oracle
  uses the release's libstdc++ (D21).

CH3b's `DisconnectedBlockTransactions` emulation, in the CH3 design on `node/ch3-design` (not yet on
657a28c28), set the precedent.

#### 2.3.3 The mempool read trait

```rust
/// Core's `pool->vTxHashes` walk in `InitData` (be:112-139), under the pool's read lock.
pub trait CompactTxSource: Send + Sync {
    /// Calls `f(instance_hash, tx)` in `vTxHashes` order until it returns `false`. The order is
    /// insertion order with swap-remove (tm:475-476, tm:726-733); an asset unlock v2 whose
    /// instance hash changes is rekeyed in place (tm:871). The order decides which of two
    /// short-ID-equal transactions wins, so MP1a must keep it.
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&InstanceHash, &Arc<Transaction>) -> bool);
}
pub struct NoMempool;   // empty: every non-prefilled transaction is requested
```

The walk takes `&mut dyn FnMut` and stops early, as Core's loop does when
`mempool_count == shorttxids.size()` (be:136-137). MP1a implements the trait over its
`vTxHashes`-equivalent vector.

#### 2.3.4 The extra-transactions ring

P3 owns the ring and Core's entry point (manager ruling on Sol r1 M1, D19):

```rust
/// `vExtraTxnForCompact` + `vExtraTxnForCompactIt` (np:1129-1131).
pub struct ExtraTxns { /* D12 */ }
impl ExtraTxns {
    /// `-blockreconstructionextratxn`, default 100 (nph:43).
    pub fn new(max_extra_txn: i64) -> Self;
    /// `AddToCompactExtraTransactions` (np:1922-1931): nothing when `max_extra_txn <= 0`; keyed
    /// by `GetInstanceHash()`; once the ring is full, each add replaces the oldest entry.
    pub fn add(&mut self, tx: Arc<Transaction>);
    /// `InitData`'s walk over `extra_txn` (be:141-170), in Core's slot order.
    pub(crate) fn for_each(&self, f: impl FnMut(&InstanceHash, &Arc<Transaction>));
}
```

How the ring is built is D12: manager ruling (private). In every other respect it mirrors Core.
That includes the second-match test at be:157-162, which compares txids (`GetHash()`), not the
instance hashes the short IDs use, and which S3 exercises with real transactions (§6.1).

`BlockDownload::add_to_compact_extra_transactions(tx)` is the entry point P5 and MP1a call (A-6b,
A-7). In M3 nothing calls it, so the ring stays empty and the walk finds nothing. That is a Core
node that has accepted no orphan and rejected no small transaction. S3 tests the ring by injecting
transactions directly. The wire consequence of the empty ring is in §3.5.5.

### 2.4 `ChainAccess` additions (`processing/seams.rs`)

The mirror (P2a-core D3/D59) gains what P3 reads and does:

```rust
pub trait ChainAccess: Send + Sync {
    // … existing …
    /// `ProcessNewBlock(block, force, &new_block)` through V (CH1-node `ChainCommand::ProcessBlock
    /// { reply: BlockReply::Activated }`). Returns the reply the actor waits on. Sending can block
    /// on V's bounded command lane (`actor.rs:194-198`), as any `ChainCommand` send does.
    fn process_block(&self, req: BlockSubmit) -> Result<BlockWait, Interrupted>;
    /// The entry's mutable status as of `snap` (`nStatus`, `nTx`, `nChainTx != 0`).
    fn status(&self, snap: &ChainSnapshot, id: BlockId) -> Option<EntryStatus>;
    /// `ActiveChain().Contains(pindex)` at `snap`.
    fn in_active_chain(&self, snap: &ChainSnapshot, id: BlockId) -> bool;
    /// `ActiveChain()[h]` at `snap`.
    fn active_at(&self, snap: &ChainSnapshot, height: u32) -> Option<BlockId>;
    /// `LastCommonAncestor(a, b)` (chain.cpp:166); both are in the block tree, so it exists.
    fn last_common_ancestor(&self, a: BlockId, b: BlockId) -> BlockId;
    /// One `ChainView` load: its snapshot and the `view_seq` it was published as. Every drain,
    /// and so every message's snapshot (I0), takes both from this one load, never the snapshot
    /// from `snapshot()`, whose SB4 `TipCell` may be one store ahead of the `ChainView`
    /// (CH1-node.md:176; §3.6, D47).
    fn drain_snapshot(&self) -> (Arc<ChainSnapshot>, u64);
    /// Returns once V has published a `ChainView` with `view_seq ≥ seq`. Only an `Ahead` calls
    /// it (§3.6, D50, D56). It returns at once if the current `ChainView` already has that
    /// `view_seq`. Otherwise the host sends a class R no-op round trip, which DEC-229 (b) allows.
    /// By C7 (b), V serves it only after it has published since its last synchronous signal, so
    /// the reply implies the publication that ends the accept which set the stamp. A round trip
    /// that reaches V just after ABC's first loop head waits one ABC step, as Core's `msghand`
    /// can lose `cs_main` to that step (D57).
    fn wait_published(&self, seq: u64) -> Result<(), Interrupted>;
}

pub struct BlockSubmit {
    pub block: Arc<Block>, pub hash: BlockHash, pub force: bool, pub from: PeerId,
    /// `m_checked_merkle_root` (v:4326, v:4362): `is_block_mutated` has already checked this
    /// block's merkle root, so V's `CheckBlock` skips that pass (A-4b, D24).
    pub merkle_checked: bool,
}
pub struct BlockOutcome { pub new_block: bool }      // `*new_block` (v:4875, v:4835)
pub struct BlockWait(ActorReply<BlockResult>);       // CH1-node's Mutex+Condvar reply slot (reply.rs:253-360)

#[derive(Clone, Copy)]
pub struct EntryStatus {
    pub have_data: bool,          // BLOCK_HAVE_DATA
    pub validity: Validity,       // nStatus & BLOCK_VALID_MASK: Unknown|Reserved|Tree|Transactions|Chain|Scripts
    pub failed: bool,             // BLOCK_FAILED_MASK (IsValid() is false when set, chain.h:300-308)
    pub n_tx: u32,                // nTx
    pub have_txs_downloaded: bool,// nChainTx != 0 (HaveTxsDownloaded, chain.h:264)
}
impl EntryStatus { pub fn is_valid(&self, up_to: Validity) -> bool; }   // CBlockIndex::IsValid
```

**What `status` returns.** `EntryFacts` holds only the immutable fields. Status changes when V
accepts or connects a block. `status` returns the **last published** value: nothing versions
per-entry status by epoch, and the `snap` argument only ties the read to the hook's snapshot for
the active-chain reads. CH1-node publishes status at `scope_end`, before any reply
(CH1-node.md:664), so C2 already holds there. Last-published is safe for the same reason as every
P2a-core read. Status moves only when V runs. The net actor is V's only P2P caller and waits for its
own block, so a mid-hook change can come only from another origin (RPC, reindex) within the
publication lag, which is the R4 window CH1-node accepts for every published read (D14). The host
implementation over `ChainViewHandle` is an interface ask of CH1-node and CH3b (§10, A-1).

**Contracts on V** (interface asks, §10):

- **C1.** Every synchronous signal V emits while it serves a `ProcessBlock` (`NewPoWValidBlock`
  v:4832, `BlockChecked` v:3432/v:4891) reaches the listener before V sends that command's reply.
  CH1-node LO-5 already runs them inline.
- **C2.** The `Activated` reply goes out after the publication that carries the block's status and
  the new tip. A `status` or `snapshot` read after the reply then sees what Core's tail sees under
  `cs_main` (np:5284, `pindex->IsValid(BLOCK_VALID_TRANSACTIONS)`).
- **C3.** `BlockResult.new_block` is `AcceptBlock`'s `*fNewBlock` (v:4765, v:4835). It is set
  before `SaveBlockToDisk` (v:4837), so it stays true when the write then fails. It is false when
  `CheckBlock` fails (v:4873, v:4885-4888) or `AcceptBlock` returns before v:4835. CH1-node's
  `BlockResult::rejected` (`command.rs:85`) hard-codes `false`, which is right only for rejections
  before v:4835 (A-2).
- **C4.** V emits `NewPoWValidBlock` exactly under v:4831's guard (`!IsInitialBlockDownload() &&
  ActiveTip() == pindex->pprev`, emitted at v:4832), because the port draws the nonce when the
  signal arrives (Q12).
- **C5.** V hands `BlockChecked` to the listener, which pushes it on P3's bounded queue (§2.5)
  without blocking. (`NewPoWValidBlock` queues nothing; C8.) The queue is `PreStart` until the
  actor's first drain and `Live` after it. When a `Live` queue is over its bound, V waits for
  space only at release points:
  - Core's two `LimitValidationInterfaceQueue` sites (v:3760, v:3940), which are CH1-node's
    `limit_queue` calls (`abc.rs:731`, `failure.rs:149`);
  - V's idle point, before it starts the next command;
  - before each `AcceptBlock` that runs outside an ABC step (an import's per-block accept).

  V never waits for queue space in these cases:
  - inside an ABC step or an `AcceptBlock`, where Core holds `cs_main`;
  - while `holding` (DEC-196 (2)'s startup hold). V then holds every command except control,
    `ActivateBestChain`, `AttachCoins` and `SetLoading` (`actor.rs:902-911`,
    `command.rs:264-272`), so a `ProcessHeaders` the actor sent would never be served. V does not
    call the hook at all while holding. The overshoot is that ABC's `BlockChecked` entries, one
    per `ConnectTip`; the hold admits no `ProcessBlock`, so it adds no other entry (D26);
  - while the queue is `PreStart` (§2.5, D26);
  - at a C5 point reached inside a `wait_for_space` wait. Such a point returns at once, so the
    waits never nest (D29).

  These waits are an exception to LO-9, ruled as **DEC-229** and amended for `PreStart` by
  **DEC-230**: the net actor is a V-waitable task.
  It may await V only through a draining wait, or for replies that every release point serves;
  §3.8.1 states the rule, which binds later rows. At an ABC or invalidate release point the wait
  is CH1-node's `VWait`. At the idle point, and for an import accept outside a composite, it is a
  new `VWait` mode with no composite running (A-1b). Every mode services class R and M commands,
  takes the commands queued on both lanes, defers class C (`actor.rs:8-13`), and holds no lock
  that `read_block` or a `ChainAccess` read takes. The wait ends when the queue is within its
  bound, when it is closed, or on shutdown. CH1-node's `ServiceWait` must therefore accept a
  listener-supplied condition; today it takes only a signals `Barrier` (`signals.rs:175-178`).
  This is ask A-1b.
- **C5′. Before an accept, no signal is outstanding (DEC-240, D55).** Before V runs a `ProcessBlock`
  command's accept, or an import's per-block accept, while its own IBD latch is false, it waits
  until the queue holds no outstanding entry: none queued, and none drained but not yet
  processed. This is a C5 wait (DEC-229), at points C5 already lists: the idle point, an ABC
  release point that serves the command, and the point before an import's accept. It rings first,
  and it ends when the condition holds, when the queue is `PreStart` or closed, or on shutdown. A
  C5 wait never runs such an accept itself: it defers the command, as it defers class C, and V
  runs it after the wait, behind this same wait. In IBD, V emits no `NewPoWValidBlock` (v:4831),
  so it does not wait. By C7 (b), every outstanding entry is eligible at these points.
- **C6.** A queued `UpdatedBlockTip` reaches the signals thread only after the publication that
  carries its tip. CH1-node's release already orders it so: `publish_view`, then the signals'
  splice (`actor.rs` `Stage::Published` before `Stage::Spliced`). §3.6's ordering proof relies on
  it (A-1, D34, D44).
- **C7.** V's publications order its pushes (D37):
  - (a) the first publication after V pushes a synchronous signal reflects the state in which V
    emitted it. CH1-node publishes at more points than `scope_end` and `after_release`: it also
    stores a `ChainView` after SB4's `accept` mid-`ProcessHeaders`, and after `ClSigs` changes
    (CH1-node §6.2). The property P3 needs is narrower: no publication falls between a push and
    the end of the scope or hold that pushed it. The scopes that push (an accept, an ABC hold, an
    import's accept) satisfy it, because `accept_header` does not publish, so a push is always
    followed by its own scope's or hold's publication before any other (D49);
  - (b) whenever V reaches a C5 wait point, or serves any command, it has published since its
    last synchronous signal. At an ABC
    release point the previous hold's `after_release` has published (`abc.rs:703`, before the
    loop's `release_point` and `limit_queue`); at the idle point the last scope or release has.

  CH1-node must confirm (b) for `failure.rs:149` and for an import's per-block accept, and state
  C6 and C7 in its signal contract before S4 starts (A-1).
- **C8. `NewPoWValidBlock` runs inline, with V's own tree (D50).** V calls the listener's
  `new_pow_valid_block` at v:4832, inside the accept scope, and the call returns before
  `SaveBlockToDisk`. V passes the entry's `EntryFacts`, the stamp (C7 (a)), and a `TreeRead` over
  its in-scope block tree: lookup by hash, `ancestor(id, height)` and chain work, valid only
  during the call. Those are the reads Core's callback makes under `cs_main` (np:2253-2258), so a
  header that is new in this accept is found as Core finds it. Inside the call the listener takes
  P3's header cells, one at a time, and may wait for one that the actor holds. No holder of a
  header cell waits on V while it holds one (§3.6, §3.8.1), which is LO-5's condition for a lock V
  may wait behind. In turn, during the call V holds no lock that any `ChainAccess` read,
  `snapshot()`, `drain_snapshot()` or `read_block` takes, because a cell holder may call those
  (D57). CH1-node states C5′ and C8 in its signal contract before S4 starts (A-1, A-1b).

### 2.5 Signals and `Shared` (`processing/peerman.rs`)

The leaf queue `checked: Mutex<Vec<(BlockHash, Option<BlockValidationError>, bool)>>` becomes an
ordered queue of stamped `BlockChecked` signals, plus a doorbell. Each peer gains a header cell:

```rust
pub enum SyncSignal {
    /// `BlockChecked(block, state)` (np:2314; V emits at v:3432, v:4891), `ibd` V-evaluated.
    /// The only queued kind: `NewPoWValidBlock` finishes on V's thread (below, D50).
    BlockChecked { hash: BlockHash, invalid: Option<BlockValidationError>, ibd: bool },
}
/// Each queued signal carries a stamp: one more than the `view_seq` V last published (C7 (a)),
/// read on V's own thread, which is the first published `view_seq` whose state is consistent
/// with applying it (D37). V is the queue's only producer, so stamps never decrease in queue
/// order.
struct Entry { stamp: u64, signal: SyncSignal }
/// P2a-core's per-peer announcement cell (`peer.rs` `BlockInv`, Core's `m_block_inv_mutex`), a
/// leaf lock. `UpdatedBlockTip` appends on the signals thread; the head of `send_messages` moves
/// the list out (§3.6, DEC-236, D44).
pub struct BlockInv {
    /// `m_blocks_for_headers_relay`, compressed without changing section 7's result (D44):
    /// `Short` while it holds at most `MAX_BLOCKS_TO_ANNOUNCE` (8) hashes; past that, section 7
    /// only reverts to an inv of the last hash (np:6412-6414, np:6531-6550), so only the last is
    /// kept.
    pub headers_relay: RelayList,
    pub inv_relay: Vec<BlockHash>,                            // `m_blocks_for_inv_relay`, unchanged
}
pub enum RelayList { Short(ArrayVec<BlockHash, 8>), Overflow { last: BlockHash } }
/// The four `CNodeState` fields `NewPoWValidBlock` reads or writes (np:2253-2264), moved out of
/// `PeerState` into a per-peer leaf cell that the peer map's `PeerSlot` also holds, as it holds
/// `block_inv` (D50). It is the peer's share of `cs_main`: V's visit takes it on V's thread, and
/// the actor takes it where `msghand` holds `cs_main` for the peer (§3.6).
pub struct PeerHeaders {
    pub requested_hb_cmpctblocks: bool,                       // `m_requested_hb_cmpctblocks`
    pub best_known_block: Option<BlockId>,                    // `pindexBestKnownBlock`
    pub last_unknown_block: Option<BlockHash>,                // `hashLastUnknownBlock`
    pub best_header_sent: Option<BlockId>,                    // `pindexBestHeaderSent`
}
pub struct HeaderCell(Mutex<PeerHeaders>);
pub struct Ahead(pub u64);                                    // the announce stamp that was read
impl Shared {
    /// Every reader but V: lock `cell`, run `read` (the head of `send_messages` copies `recent`
    /// there; other callers pass a no-op), then read `announce_stamp`. The guard and `read`'s
    /// result if the stamp is ≤ `seq`, the `view_seq` of the caller's snapshot; else unlock,
    /// discard the result and return `Ahead`. The caller then releases every processor lock it
    /// holds, calls `ChainAccess::wait_published`, takes a new snapshot and retries (§3.6, D56,
    /// D58).
    pub fn lock_headers<'a, R>(&self, cell: &'a HeaderCell, seq: u64,
                               read: impl FnOnce(&Shared) -> R)
        -> Result<(MutexGuard<'a, PeerHeaders>, R), Ahead>;
}
impl Shared {
    pub fn block_checked(&self, hash, invalid, ibd);          // unchanged signature: pushes + rings
    /// new: all of np:2228-2268, on V's thread, inside the accept (C8, below)
    pub fn new_pow_valid_block(&self, facts: EntryFacts, block: &Arc<Block>,
                               tree: &dyn TreeRead, stamp: u64);
    pub fn block_connected(&self, now: i64);                  // + stall-timeout decay (§3.3.3)
    // `announce_stamp: AtomicU64`: the stamp of the latest `new_pow_valid_block` call that
    // passed the height test, stored by V before it touches `recent` or any cell (D56).
}
pub struct RecentBlock {                                      // `Shared::recent`, a leaf lock
    highest_fast_announce: i32,                               // `m_highest_fast_announce` (np:1044)
    hash: Option<BlockHash>,                                  // `m_most_recent_block_hash`
    block: Option<Arc<Block>>,                                // `m_most_recent_block`
    cmpct: Option<Arc<CmpctBlockBytes>>,                      // `m_most_recent_compact_block`
}
```

`Shared::block_checked` pushes, rings the doorbell and calls `self.wake()` as it does today
(`peerman.rs:160`), so an idle actor in its 100 ms wait wakes at once. The doorbell is a
`crossbeam_channel::bounded(1)` sender owned by `Shared`, whose `try_send` is dropped when the
doorbell is full. That keeps LO-5 (no lock V could wait behind) and P2a-core D66 (2) (a leaf; any
thread, including from inside a pass).

`drain_block_checked` becomes `drain_sync_signals`, which returns the drain's snapshot. It runs at
the same two places, the pass start and after each peer's `ProcessMessages` (`peerman.rs:392,414`),
and also at each message's start (I0), at the head of `send_messages` (§3.6, m-1), inside the
block wait, after a `ProcessHeaders` reply inside a message (§2.6, D46), and after an `Ahead`
wait (§3.6, D50). Only `BlockChecked` is queued. `NewPoWValidBlock` has finished on V before V's
accept publishes, so no announcement waits in the queue, behind a `BlockChecked` or for the actor
(D50).

**`NewPoWValidBlock` on V's thread (D50).** Core runs np:2228-2268 inline on the thread that runs
`AcceptBlock`, under `cs_main`, before `SaveBlockToDisk` (v:4832, v:4837). The port runs all of it
inside `Shared::new_pow_valid_block`, which V calls at the same point (C8):

1. Build `CmpctBlock::from_block(block, nonce)` with the nonce from `GetRand<uint64_t>()`
   (np:2229, Q12). Return if `height <= highest_fast_announce` (np:2234-2235); else set it
   (np:2236).
2. Store `stamp` in `announce_stamp` (D56). Then set `recent`'s hash, block and encoding
   (np:2243-2247), and release `recent`. A region reads the two in the reverse order (D58). The wire
   message is serialized at most once, for the first recipient, as np:2240's deferred `lazy_ser`
   is.
3. If the queue is closed (the actor has stopped), return. Core's shutdown has stopped connman by
   then, so its `ForEachNode` finds no node (init.cpp:348).
4. Visit a copy of the connection table in insertion order (`NetCore::peers()`,
   `netcore.rs:643-646`, Core's `m_nodes` order), testing `fully_connected` at each visit, as
   `ForEachNode` does (np:2249, net.h:1423-1440). For each such peer: clone its `HeaderCell` from
   the peer map and release the map; lock the cell; test `fully_connected` again and skip the
   peer if it no longer holds (np:2252, D57); run `process_block_availability` and the two
   `peer_has_header` tests against `tree` (np:2253-2258). If
   `requested_hb_cmpctblocks && !peer_has_header(B) && peer_has_header(prev)`, log `"%s sending header-and-ids %s to peer=%d"` (with `"PeerManager::NewPoWValidBlock"`), push
   the message with `NetCore::push`, which is safe from any thread and never blocks
   (`netcore.rs:659-670`), and set `best_header_sent = B` (np:2259-2264). Then unlock the cell.

`fully_connected` already excludes a peer marked for disconnect, as `NodeFullyConnected` includes
`!fDisconnect` (net.cpp:4852-4855). In Core the test and the callback body both run under
`cs_main`. In the port a cell wait can sit between them, and the actor's region may disconnect the
peer meanwhile (a stall, a download timeout, an old chain). So the visit tests again under the
cell, which is where np:2252's own `fDisconnect` return stands (D57, revising D49 (4)).

The lock order is V's state (Core's `cs_main`), then one header cell, then that peer's send queue
(Core's `cs_vSend`). V holds at most one cell at a time and takes no `PeerState`. The actor may
hold a cell, and V then waits for it. The actor never holds a cell while it waits on V (§3.6,
§3.8.1).

So section 7 and P4 find `recent` set before V replies and before `UpdatedBlockTip` is queued, as
in Core, whatever the block's origin (m-1). And every high-bandwidth peer whose header tests pass
holds `cmpctblock(B)` in its send queue before V writes B to disk and before B's ABC starts, as in
Core (§3.6, Timing).

**Before the first drain (D26).** The listener is registered where Core registers PeerManager,
before V's startup ABC and imports (init:2199, before ThreadImport at init:2455). The actor starts
with connman (init:2755). Both are ask A-12. Until the actor's first drain the queue is
`PreStart`. V never waits for it then, `NewPoWValidBlock` runs as always, and a `BlockChecked`
entry is discarded. The first drain switches the queue to `Live` under its leaf lock.

Discarding is exact. `block_source` is empty before the first drain, because only the actor's
handlers fill it, and every effect of `BlockChecked` needs a `mapBlockSource` hit
(np:2314-2343). `NewPoWValidBlock` queues nothing, so none of it is discarded. Before the first
drain its visit finds no fully connected peer, because `fSuccessfullyConnected` is set while the
actor processes `VERACK` (np:4257), which comes after a drain. It does what Core's callback does.

Registering at actor start instead, and dropping everything before it, does **not** match Core,
so the proof that the manager asked for fails. `BlockChecked` would be dropped exactly, but
`NewPoWValidBlock`'s update of `recent` and `highest_fast_announce` would be lost, and Core can run
it before connman starts. ThreadImport holds
`CImportingNow` for its whole block phase (blockstorage.cpp:893-960), so `LoadingBlocks()` makes
`IsInitialBlockDownload()` true (v:1979). But the latch test comes first (v:1973), so once IBD
has latched false, a `-loadblock` block that extends the tip passes v:4831's guard. Its
`NewPoWValidBlock` then sets `m_highest_fast_announce` and the most-recent block before any peer
exists, and a later announcement or getdata(CMPCT) sees them. Whether any pre-import caller
latches IBD, in Core or in rdashd, is not shown here.

**DEC-230** ratifies `PreStart` as an amendment to DEC-229: V never waits on it,
`NewPoWValidBlock` runs at once, and it discards only entries the first drain would provably
ignore, which are `BlockChecked` entries with no `mapBlockSource` entry. (rev4–rev9 also
discarded `NewPoWValidBlock`'s per-peer entries while no peer was fully connected; nothing of it is
queued now, D50.) `UpdatedBlockTip` writes P2a-core's cells, not the queue, and finds no fully
connected peer then, so it appends nothing (§3.6). Sol r2 checked the discard against v24:
`mapBlockSource`'s only insertion sites are the P2P handlers (np:3804, np:5271, np:5383), and none
runs before the first drain.

**DEC-230's guard.** If a later change adds a way to insert into `block_source` before the first
drain, the discard must go and `PreStart` falls back to an unbounded queue that the first drain
processes. S1 enforces this mechanically:
- `SignalQueue::push` in `PreStart` asserts, in every build, that `block_source` is empty. It
  reads a counter that the actor's insertion paths bump, so the check is O(1). The assertion
  message names DEC-230;
- an S1 test drives each `block_source` insertion site and shows that none is reachable before
  the first drain (D36, D50).


**The bound (C5, D20, D26, D29).** Core runs both callbacks inline, so it never holds a backlog.
The port's queue is lossless once `Live`, and bounded:

```rust
pub struct SignalQueue { inner: Mutex<Inner> }       // leaf: VecDeque<Entry>, cost, mode, closed
pub const SIGNAL_QUEUE_MAX_ENTRIES: usize = 1_024;
pub const SIGNAL_QUEUE_MAX_BYTES: usize = 8 << 20;   // four 2 MB blocks (consensus.h:10-14)
enum Mode { PreStart, Live }
impl SignalQueue {
    /// V's thread only. Never blocks; discards in PreStart or once closed.
    fn push(&self, stamp: u64, s: SyncSignal);
    /// The queued prefix stamped ≤ `cutoff`, the `view_seq` that the caller's
    /// `drain_snapshot()` returned with its snapshot (D37, D47); later entries stay queued.
    /// Switches `PreStart` to `Live`. `Drained` owns the taken entries and holds no
    /// `MutexGuard`: each entry's cost is released under a fresh leaf lock when the actor has
    /// processed it (its item drops), not at `drain`, so V's push never waits behind section 7 or
    /// per-peer work (LO-5, D49). Coming back within the bound wakes a waiting V (m-3).
    fn drain(&self, cutoff: u64) -> Drained<'_>;
    fn close(&self);                     // the actor has stopped (drop guard, below)
    /// C5: V's release-point wait, through the host listener. First, whenever the queue is
    /// `Live` and non-empty, even within the bound, it rings the doorbell (which ends the block
    /// wait) and calls `wake()` (which ends the idle wait), so the actor applies what V's last
    /// publication made eligible without waiting for a later ring (D45). Then returns at
    /// once while the queue is within both bounds, `PreStart` or closed, and when V is already
    /// inside a `wait_for_space` (an `in_space_wait` flag only V's thread touches: waits never
    /// nest).
    pub fn wait_for_space(&self, wait: &mut dyn ServiceWait) -> Result<(), WaitError>;
}
```

- **Cost.** An entry costs `size_of::<SyncSignal>()` plus its owned heap, which is the error's
  `reason` and `debug` strings.
- **Push never waits.** V pushes from inside a failed `AcceptBlock` or an ABC step, where Core
  holds `cs_main` and no release-point wait is possible.
- **V waits at C5's release points.** `wait_for_space` returns at once while `entries ≤
  SIGNAL_QUEUE_MAX_ENTRIES` and `bytes ≤ SIGNAL_QUEUE_MAX_BYTES`, so in normal operation it costs
  one leaf lock, plus, when entries are queued, one `try_send` on the doorbell and one `wake()`,
  both leaf operations (D45). Before it parks, it rings the doorbell again, in case an earlier
  ring was coalesced.
- **Space is released per processed entry (m-3).** A batch the actor is still working through
  stays counted, so V cannot refill the queue to the bound behind it. The peak is the maximum
  below, not twice it.
- **Hard maximum (MJ-2).** At every release point, and inside every wait, V runs each queued
  class M command, so each queued `ProcessBlock`'s accept (command.rs:221; actor.rs:741,
  1114-1125). An accept queues at most one failure `BlockChecked` (v:4891); its
  `NewPoWValidBlock` queues nothing (D50). Let Q be the
  number of `ProcessBlock` commands that can be queued at V at once. Each sender awaits its
  reply, and the `Activated` reply follows the deferred ABC, so each sender has at most one
  queued. So Q is at most the actor (1), plus the RPC workers (`-rpcthreads`, default 4,
  httpserver.h:12), plus the import (1): 6 by default. A-1b asks CH1-node to confirm that no
  sender sends again before its reply (after an `Accepted` reply, say) and that no `ProcessBlock`
  uses the marked lane. Between two v:3760 calls, one outer ABC iteration can run several steps
  after `fInvalidFound` (the inner do-while, v:3768-3800), each connecting until it exceeds the
  old tip's work (v:3665-3667). Call that iteration's `ConnectTip` count C. The maximum is then

  `SIGNAL_QUEUE_MAX_ENTRIES` entries and `SIGNAL_QUEUE_MAX_BYTES`
  \+ Q entries
  \+ C `BlockChecked` entries (a few hundred bytes each with their strings).

  `UpdatedBlockTip` adds no entry. Its appends go to the per-peer cells, each one `RelayList` of
  at most 8 hashes however many callbacks append to it, so `MarkConflictingBlock`'s one callback
  per competing sibling (v:4140-4142) needs no bound here (§5.1, DEC-236, D44).

  During the startup hold V does not wait at all, and C is that ABC's connect count. With the
  defaults this is at most 8 MiB + 6 entries + C entries. Core itself
  keeps every block of a step in its `ConnectTrace` (v:3349-3352, v:3511), and its queued
  `BlockConnected` signals keep them too, so the C term is far below what the same work already
  holds in Core.
- **Order and C1.** Entries leave in push order. V's pushes for a command come before its reply,
  and waiting never reorders, so C1 holds.
- **Close, through a drop guard (m-6).** The actor thread owns a guard whose drop calls `close`,
  so the queue closes on every exit, including a panic or an early return, and before any
  teardown step that can wait on V. A parked V then resumes at once. S6's shutdown stops the actor
  before V stops serving. Core's shutdown stops connman first (init.cpp:348), after which no peer
  is left to act on a signal, so dropping later pushes gives Core's outcome.

§3.8.1 shows that V's wait always ends.

The host listener (S6) implements CH1-node's `ValidationInterface` (`signals.rs:40-96`):

- `block_checked` → `Shared::block_checked(block.hash(), state.invalid(), ibd)`;
- `new_pow_valid_block` → `Shared::new_pow_valid_block(facts, block, tree, stamp)`, with the
  facts and the `TreeRead` from V's in-scope index (C8, A-1) and the stamp read on V's thread;
- C5's release-point hook (A-1b) → `SignalQueue::wait_for_space`; V does not call it while
  `holding`;
- queued `updated_block_tip` → `Shared::updated_block_tip` (P2a-core), unchanged in shape. It
  sets the IBD flag, and outside IBD visits the connection table in insertion order and appends
  to each fully connected `CanRelay` peer's `block_inv` cell under the cell's leaf lock, reading
  each recipient at its visit, as Core's `ForEachNode` does (np:2296-2305). The append is
  `RelayList`'s, and the callback never waits (LO-9, §3.6, DEC-236, D44);
- queued `block_connected` → `Shared::block_connected(now)`, which also rings the doorbell, so
  entries made eligible by the publication before it are drained promptly (§3.6, D37).

### 2.6 The two-phase block handlers (`processing/blocks.rs`, `logic.rs`)

`process_messages` holds the peer's `PeerState` lock for the whole message (`logic.rs:351-380`).
Today `process_headers` waits on V under that lock. That is safe because V never calls back into a
peer state. P3 cannot keep the lock while it waits for a block:

- **Self-deadlock.** `block_checked` for this same peer runs `BlockCheckedCx::with_peer`
  (`seams.rs:230-236`), which locks this peer's state.
- **RPC latency.** `getpeerinfo` would block for a whole ABC. Core takes `cs_main` per ABC step and
  releases it between steps (v:3753-3763), and `msghand` holds no per-peer lock during `ProcessNewBlock`.

So the three handlers return a step to the caller:

```rust
pub(super) enum BlockStep {
    Done,
    /// Hand `submit` to V, then run `tail` with the outcome.
    Process { submit: BlockSubmit, tail: Tail },
    /// A `ProcessHeaders` reply has been accepted inside the message: unlock, drain, relock,
    /// and continue on the drained snapshot (I0, D46).
    Redrain(AfterHeaders),
}
pub(super) enum AfterHeaders {
    /// P2a-core's `process_headers_message` after `ProcessHeaders`: the "more" request,
    /// `UpdatePeerStateForReceivedHeaders` and `HeadersDirectFetchBlocks` (np:3370-3383).
    Headers { last: BlockId, received_new_header: bool, n: usize },
    /// `cmpctblock` steps 6–13 and what follows them (np:5092-5293).
    Cmpct { cmpct: CmpctBlock, received_new_header: bool },
}
pub(super) enum Tail {
    /// np:3717-3732 alone (`block`, `blocktxn` via `ProcessCompactBlockTxns`).
    ProcessBlock,
    /// np:3717-3732, then np:5284-5286: `RemoveBlockRequest(hash, nullopt)` if the entry is
    /// `IsValid(BLOCK_VALID_TRANSACTIONS)` (the reconstructed-in-place `cmpctblock` path).
    CompactReconstructed { id: BlockId },
}
```

`process_messages` changes like this. Every other message keeps today's path, under the lock
throughout; the new code is only the `Redrain` and `Process` arms:

```
let mut step = self.process_message(peer, &mut st, &msg, &x);   // phase 1, under the lock; x: I0's drained snapshot
loop { match step {
    Ok(BlockStep::Redrain(after)) => {                      // a V round trip inside the message (D46)
        drop(st);
        let x2 = self.drain_sync_signals();                 // drain_snapshot(), then the drain (D47)
        st = state.lock();
        step = self.after_headers(peer, &mut st, after, &x2);   // may return Process, or Redrain again
    }
    Ok(BlockStep::Process { submit, tail }) => {
        let hash = submit.hash;
        drop(st);
        let mut wait = chain.process_block(submit)?;        // ChainCommand::ProcessBlock{Activated}
        let waker = shared.doorbell_waker();                // a Waker whose wake() rings the doorbell
        let outcome = loop {                                // phase 2, unlocked
            match Pin::new(&mut wait.0).poll(&mut Context::from_waker(&waker)) {   // registers the waker first
                Poll::Ready(r) => break r.map_err(|_| Interrupted)?,               // WaitError → Interrupted
                Poll::Pending => { shared.doorbell.recv(); self.drain_sync_signals(); }
            }
        };
        let x3 = self.drain_sync_signals();                 // C1: everything V emitted before replying
        let mut st = state.lock();                          // phase 3
        self.block_tail(peer, &mut st, &hash, outcome, tail, &x3);
        // then today's post-message checks (interrupt, pending getdata, `more`), unchanged
        break;
    }
    other => { /* today's Ok/Err handling, unchanged */ break }
} }
```

**The header cell (D50).** Wherever this loop locks `st`, it then takes the peer's header cell
with `lock_headers` for the snapshot it just drained (x, x2 or x3). It drops the cell before
`chain.process_block` and, inside a handler, before each `ProcessHeaders` send. An `Ahead`
releases `st` and the cell, waits with `wait_published`, drains again and retries. Nothing of the
step has run by then (§3.6). Serving queued `getdata` (P4) runs outside the cell: it reads none
of the four fields, and Core serves blocks outside `cs_main` (np:2741, np:2817-2821).

CH1-node's reply is `ActorReply<T>`, a `Mutex` + `Condvar` slot that implements `Future`
(`reply.rs:253-360`), not a channel. So the actor polls it with a `Waker` that rings the same
doorbell the signal producers ring. Every pass polls before it blocks, so a reply or a signal that
lands between the poll and `recv` has already rung the doorbell, and no wakeup is lost. The
doorbell is a `crossbeam_channel::bounded(1)` whose sender lives in `Shared`, so it is never
disconnected and cannot spin (memory note crossbeam-disconnected-receiver-spin).

S1 may use `ActorReply::try_recv` (reply.rs) plus the doorbell instead of a hand-built `Waker`.
That is an allowed simplification, as long as `try_recv` runs after each ring and before each
`recv`. The poll form is also correct, because `close` stores the value before it wakes
(reply.rs:287-298) (D33).

The wait has no timeout, because Core's `ProcessNewBlock` call has none, and `Shared::interrupt`
does not end it. In Core, `msghand` sits in `ProcessNewBlock` until ABC returns, and ABC breaks on
`ShutdownRequested()` after each step (v:3841). The port has the same shutdown latency.
S6 keeps V serving until the net actor has joined, which matches Core's shutdown stopping connman
(init.cpp:348) before it flushes the chainstate (init.cpp:383). A reply V drops (`WaitError::Disconnected`, V gone)
becomes `Interrupted`, and the handler returns without a tail.

**Why this order is exact.** In Core, inside one `ProcessMessage`:

1. the handler's pre-`ProcessBlock` code runs (phase 1 here);
2. `ProcessNewBlock` runs `NewPoWValidBlock` and `BlockChecked` inline on `msghand` (v:4832, v:4891,
   v:3432); here `NewPoWValidBlock` runs inline at the same point of `AcceptBlock`, on V's thread,
   and `BlockChecked` runs in the wait, on the actor, in V's order;
3. the tail runs (phase 3);
4. the source's `SendMessages` runs in the same pass.

A signal for another origin's block (RPC `submitblock`, a reindex) that V emits during this wait is
also serviced in the wait. In Core it would run on the RPC thread under `cs_main`, interleaved at
an ABC step boundary. Both are "between two `cs_main` sections of this `ProcessNewBlock`", so the
order is again Core's.

**Is the unlocked phase-2 window observable?** No. Between phase 1 and phase 3 nothing else in the
actor runs for this peer: the actor is single-threaded and is blocked here. Other threads take this
peer's state only to read stats or to queue into leaf cells (P2a-core D66). V's visit may take this
peer's header cell, as Core's callback reads this peer's `CNodeState` while `msghand` waits in
`ProcessNewBlock`. Core's `cs_main` is released at the same points inside `ProcessNewBlock`.

**A V round trip inside a message (I0, D46).** The `headers` handler and `cmpctblock`'s step 5
await `ProcessHeaders` under the peer's lock, as today (DEC-229 (b)). V can serve it after other
origins' scopes, so the reply's snapshot can show a tip whose `BlockChecked` entries are still
queued (Opus r8 M2). Today P2a-core hands that snapshot to
`UpdatePeerStateForReceivedHeaders` and to `SyncCx` for `headers_direct_fetch` (`sync.rs:245`,
`sync.rs:248`). In the port, an accepted reply returns `Redrain`: the actor drops the lock,
drains, relocks, and the continuation reads that drain's snapshot X′. It takes from the reply only
its own facts, `last_accepted` and the invalid state; an invalid or interrupted reply is handled
under the lock as today, and the message ends. This is Core's own release point.
`ProcessHeadersMessage` releases `cs_main` after `ProcessNewBlockHeaders` (np:3362), and
`UpdatePeerStateForReceivedHeaders` takes it again (np:3270). `cmpctblock` takes it again for
steps 7–13. Another origin's accept and its callbacks can run in that gap, and the next section
sees them. Core takes `cs_main` separately for `UpdatePeerStateForReceivedHeaders` and
`HeadersDirectFetchBlocks` (np:3380-3383); one drain before both is Core's schedule with nothing
run between them. The unlocked window is phase 2's: only the drain touches peer states in it,
which is Core's callbacks between two `cs_main` sections. The P2a-core change is ask A-13.

### 2.7 Stats mirror

`NodeStateStats::heights_in_flight` (`stats.rs:17-18`, `vHeightInFlight`, np:1886-1889) is filled
from `PeerDownload::heights`. That is a leaf `Mutex<Vec<i32>>` which `block_requested` and
`remove_block_request` keep in `vBlocksInFlight` order. `PeerState` gets an
`Arc<Mutex<Vec<i32>>>` handle to it when the peer's `PeerDownload` is created, and
`stats::node_state_stats` copies it out under the state lock. The eviction closures keep using
`BlockSync::blocks_in_flight` (`eviction.rs:161,220`).

---

## 3. Core equivalence

### 3.1 Functions replaced

| Core (rc.3) | Rust | Notes |
|---|---|---|
| `IsBlockRequested` np:1271 | `BlockDownload::is_block_requested` | `in_flight.contains_key` |
| `IsBlockRequestedFromOutbound` np:1276-1285 | `…::is_block_requested_from_outbound` | `Assert(State(id))`: every in-flight peer has a `PeerDownload` (debug assert) |
| `RemoveBlockRequest` np:1287-1323 | `…::remove_block_request` | `Assume(count ≤ 3)` is a debug assert |
| `BlockRequested` np:1325-1358 | `…::block_requested` | returns `false` plus the existing entry if this peer already has it |
| `MaybeSetPeerAsAnnouncingHeaderAndIDs` np:1360-1416 | `…::maybe_set_peer_as_announcing_header_and_ids` | §3.5.2 |
| `CanDirectFetch` np:1428-1431 | `can_direct_fetch(snap, now)` | `tip.time > now − 20·spacing`, `NodeClock` (O17 `UngatedRules.time_source`) |
| `PeerHasHeader` np:1433-1440 | `peer_has_header(st, chain, id)` | reads the header cell's `best_known_block` and `best_header_sent` (D50) |
| `FindNextBlocksToDownload` np:1477-1559 | `…::find_next_blocks_to_download` | §3.2 |
| `FinalizeNode` (in-flight part) np:1804-1820 | `BlockSync::finalize` | §3.2.4 |
| `MaybePunishNodeForBlock` np:1969-2020 | `SyncCx::maybe_punish_node_for_block` (P2a-core) | used, not reimplemented |
| `BlockConnected` (stall decay) np:2200-2207 | `Shared::block_connected` | §3.3.3 |
| `NewPoWValidBlock` np:2228-2268 | `Shared::new_pow_valid_block`, on V's thread (D50) | §2.5, §3.6 |
| `BlockChecked` np:2314-2343 | `BlockDownload::block_checked` | §3.7 |
| `HeadersDirectFetchBlocks` np:3202-3257 | `BlockSync::headers_direct_fetch` | §3.2.3 |
| `ProcessBlock` np:3717-3732 | `PeerLogic::block_tail` | §3.7 |
| `ProcessCompactBlockTxns` np:3734-3816 | `PeerLogic::process_compact_block_txns` | §3.5.4 |
| `CMPCTBLOCK` np:5052-5293 | `PeerLogic::on_cmpctblock` | §3.5.3 |
| `BLOCKTXN` np:5295-5307 | `PeerLogic::on_blocktxn` | §3.5.4 |
| `BLOCK` np:5348-5387 | `PeerLogic::on_block` | §3.4 |
| section 9 np:6752-6781 | `BlockSync::stall_section` | §3.3 |
| section 12, the block part np:6820-6839 | `BlockSync::getdata_section` | §3.2.2 |
| `IsBlockMutated` v:4434-4456 | `dash_consensus::is_block_mutated` (new `pub fn`, D9) | wraps CS3's `check_merkle_root` (`dash-consensus/src/block.rs:40`) and the 64-byte rule |
| `PartiallyDownloadedBlock` be:55-223 | `compact::PartiallyDownloadedBlock` | §2.3 |
| `FetchBlock` np:2057-2089 | `Shared::fetch_block` → actor command | for RP1x's `getblockfrompeer`; P3 ships the processor half (D10) |

### 3.2 Block download

#### 3.2.1 `FindNextBlocksToDownload` (np:1477-1559)

This is a line-by-line port. Every read goes through the hook's `snap`:

1. If `count == 0`, return (np:1479-1480).
2. Run `ProcessBlockAvailability(peer)` (np:1487). This calls P2a-core's
   `sync::process_block_availability` through `SyncCx`.
3. Return if `best_known_block` is none, or if its `chain_work` is below the tip's or below
   `MinimumChainWork()` (np:1489-1492). Both reads come from `snap` and `chain.min_chain_work()`.
4. If `last_common_block` is none, set it to `active_at(min(best_known.height, tip.height))`
   (np:1494-1498). Then set it to `last_common_ancestor(last_common, best_known)`, and return if that
   equals `best_known` (np:1502-1504).
5. Walk the chain (np:1506-1557):
   - `nWindowEnd = last_common.height + 1024`;
   - `nMaxHeight = min(best_known.height, nWindowEnd + 1)`;
   - chunks of `min(nMaxHeight − walk, max(count − out.len(), 128))`, rebuilt backwards with
     `ancestor`. For each entry, in Core's order:
     - if `!status.is_valid(Tree)`, return (np:1531-1534);
     - if `have_data || in_active_chain`, set `last_common = id` when `have_txs_downloaded`
       (np:1535-1537);
     - else, if the block is not requested: when `height > nWindowEnd`, set `staller = waitingfor`
       if `out` is empty and `waitingfor != peer`, then return (np:1538-1547); otherwise push it,
       and return when `out.len() == count` (np:1548-1551);
     - else, if `waitingfor == −1`, set `waitingfor = in_flight[hash][0].peer` (np:1552-1555).

`last_common_block` is P2a-core's `PeerState` field. P2a-core D65 says P3 writes it through `SyncCx`.

#### 3.2.2 Section 12, block requests (np:6820-6839)

```
if serves_blocks(peer) && peer.can_relay()
   && ((sync_from_peer && !is_limited_peer(peer)) || !chain.is_initial_block_download(snap, now))
   && dl.blocks.len() < 16 {
    find_next_blocks_to_download(cx, 16 - dl.blocks.len(), &mut v, &mut staller);
    for id in v { batch.push_block(Inventory::Block(hash)); block_requested(peer, id, false);
                  debug!("Requesting block {} ({}) peer={}", hash, height, peer) }
    if dl.blocks.is_empty() && staller != -1 && peers[staller].stalling_since_us == 0 {
        peers[staller].stalling_since_us = now_us; debug!("Stall started peer={}", staller) }
}
```

- `CanServeBlocks` = services have `NODE_NETWORK | NODE_NETWORK_LIMITED` (np:1190-1193).
- `IsLimitedPeer` = `NODE_NETWORK_LIMITED` without `NODE_NETWORK` (np:1203-1207).
- `CanRelay` = `!m_masternode_connection || m_masternode_iqr_connection` (neth:1083), from
  `PeerHandle::can_relay`.
- `sync_from_peer` is P2a-core's section-6 value, already a `getdata_section` argument (`logic.rs:797,818`).
- The `ibd` read here is the published view, which is Core's `IsInitialBlockDownload()` under
  `cs_main` at this point. Only `BlockChecked`'s argument must be V's (PLAN P3).
- Blocks go into the same `GetDataBatch` as objects, before them (`batch.push_block`). Core pushes
  `vGetData` at `MAX_GETDATA_SZ` (1000, np:115) and at the end (np:6894-6897). P2a-core's batch
  already does both (`seams.rs:302-366`). At most 16 block entries are added per peer per pass, so
  a block-only `getdata` never splits, and the object loop that follows splits it where Core does.
- The staller is another peer (`waitingfor != peer`, np:1542). Its `PeerDownload` exists, because
  it has a block in flight. Writing its `stalling_since_us` is actor-private state, so no other
  peer's lock is taken.
- `"Requesting block %s (%d) peer=%d"` and `"Stall started peer=%d"` are `BCLog::NET` lines in
  Core's format (P2a-core's `tracing` target `net`).

#### 3.2.3 `HeadersDirectFetchBlocks` (np:3202-3257)

This is called from P2a-core's headers path after `pindexLast` (`sync.rs:249`). It runs in the
continuation after the post-reply drain, on that drain's snapshot X′, never on `ProcessHeaders`'s
reply snapshot (§2.6, I0, D46). Core's code:

```
if CanDirectFetch() && last.is_valid(Tree) && tip.chain_work <= last.chain_work:          // np:3209
    walk = last; v = []
    while walk && !in_active_chain(walk) && v.len() <= 16:                               // np:3213
        if !have_data(walk) && !is_block_requested(walk): v.push(walk)                    // np:3214-3218
        walk = walk.prev
    if !in_active_chain(walk): debug!("Large reorg, won't direct fetch to %s (%d)")       // np:3225-3228
    else:
        for id in v.rev(): if dl.blocks.len() >= 16 break;                                // np:3232-3236
            v_get.push(MSG_BLOCK); block_requested; debug!("Requesting block %s from  peer=%d")
        if v_get.len() > 1: debug!("Downloading blocks toward %s (%d) via headers direct fetch")
        if !v_get.empty():
            if !ignore_incoming_txs && provides_cmpctblocks && v_get.len() == 1
               && in_flight_entries == 1 && last.prev.is_valid(Chain): v_get[0] = MSG_CMPCT_BLOCK   // np:3247-3255
            push getdata(v_get)                                                           // np:3256
```

The port follows the exact conditions and strings in np:3202-3257, including the double space in
`"from  peer=%d"` (np:3238). `mapBlocksInFlight.size() == 1` is the multimap's element count, so
`in_flight_entries` is the counter `BlockDownload` keeps for it (Q10). Since `BlockRequested` has just
run, it is 1 only when this request is the only one in flight node-wide.
`m_ignore_incoming_txs` is `-blocksonly`. Section 12's batch is not used: this `getdata` is
pushed at once from the headers handler, as Core does.

#### 3.2.4 `finalize` (np:1804-1820)

For each of the peer's queued blocks, Core erases the matching `in_flight` entry
(`range.first->second.first == nodeid`). Then it applies
`m_peers_downloading_from -= !vBlocksInFlight.empty()` and asserts `≥ 0` (np:1819-1820). On the
last peer it asserts `mapBlocksInFlight.empty()` and `m_peers_downloading_from == 0`
(np:1826-1830); those are debug asserts here. `block_source` is not cleaned, and neither is
`hb_list` (Q4). Finalizing the peer that found the window blocked does not reset the staller's
`m_stalling_since`, matching Core. Only a `RemoveBlockRequest` of one of the staller's own requests
clears it (np:1318), so the staller is still disconnected at its deadline otherwise.

### 3.3 Stalling and timeouts (section 9, np:6752-6781)

P2a-core calls `stall_section` after the inv section and before its own headers timeout
(`logic.rs:801-808`). That is Core's order: stall (np:6752), block timeout (np:6773), headers
timeout (np:6783), `ConsiderEviction` (np:6817), block getdata (np:6820).

#### 3.3.1 The staller

```
let t = stalling_timeout.load();                                    // seconds
if dl.stalling_since_us != 0 && dl.stalling_since_us < now_us - t * 1_000_000 {
    info!("Peer={}{} is stalling block download, disconnecting", id, ip_suffix);
    peer.request_disconnect();
    let n = min(2*t, 64);
    if t != n && stalling_timeout.compare_exchange(t, n).is_ok() {
        debug!("Increased stalling timeout temporarily to {} seconds", n) }
    return true;                                                    // SendMessages returns
}
```

`ip_suffix` is `" peeraddr=%s"` under `-logips`, as P2a-core already renders it. The CAS is Core's
`compare_exchange_strong` on the atomic (np:6763), so a racing decay from the signals thread wins
or loses as it does in Core.

#### 3.3.2 Block download timeout (np:6773-6781)

```
if let Some(front) = dl.blocks.front() {
    let others = peers_downloading_from - 1;
    // Core's order: seconds × double, to µs, then added (BASE 1, PER_PEER 0.5)
    if (now_us as f64) > dl.downloading_since_us as f64 + (spacing as f64 * (1.0 + 0.5 * others as f64)) * 1e6 {
        info!("Timeout downloading block {} from peer={}{}, disconnecting", front.hash, id, ip_suffix);
        peer.request_disconnect(); return true; }
}
```

Core computes the deadline in `std::chrono::microseconds` from
`seconds{spacing} * (double)`. The product is a `duration<double, …>` converted by `operator>`
comparison, with no truncation before the compare (np:6776). The port computes it as `f64`
microseconds, in Core's operation order, and compares with `(now_us as f64) > deadline`. That
matches Core's double compare exactly for every `others ≤ 2^20` and every spacing, not only
Dash's 150 s (D33). `downloading_since` moves forward
(`max(old, now)`) only when the front request completes (np:1309-1310), and is set when the list
goes from 0 to 1 (np:1348-1352).

#### 3.3.3 Decay (np:2200-2207)

`Shared::block_connected(now)` already sets `m_last_tip_update` (`peerman.rs:165`). P3 adds:

```
let t = timeout.load();  debug_assert!(t >= 2);
if t != 2 { let n = max((t as f64 * 0.85) as i64, 2);           // duration_cast<seconds> truncates
            if timeout.compare_exchange(t, n).is_ok() { debug!("Decreased stalling timeout to {} seconds", n) } }
```

`std::chrono::duration_cast<seconds>(seconds × 0.85)` truncates toward zero. So 64→54→45→38→32→27→
22→18→15→12→10→8→6→5→4→3→2. Core runs this on the scheduler thread for each `BlockConnected`, and
so does the port (a queued signal, CH1-node §8.2). That is why the timeout is an atomic.

### 3.4 `block` (np:5348-5387)

Phase 1, under the lock:

1. If `chain.loading_blocks()`, log `"Unexpected block message received from peer %d"` and return
   (np:5351-5354).
2. Decode, then log `"received block %s peer=%d"` (np:5359).
3. If `is_block_mutated(&block)`: log `"Received mutated block from peer=%d"`,
   `misbehaving(100, "mutated block")`, `remove_block_request(hash, Some(peer))`, then `Done`
   (np:5365-5370).
4. Otherwise (np:5372-5385): `force = is_block_requested(hash)`, then `remove_block_request(hash, Some(peer))`,
   then `block_source.insert(hash, (peer, true))`. Core uses `emplace`, which never overwrites, so
   the port uses `entry().or_insert` (Q5). Return `Process { force, tail: ProcessBlock }`.

Phase 3 is `block_tail` (§3.7).

The header's X11 hash may come from the reader's pre-hash side cache (A1 §2.14) when present.
Decoding is the full `dash_p2p_core` block decode, and its per-transaction hashing runs on the
`prevalidate` pool (A1 §3.2 S1) inside V's accept, not here.

### 3.5 Compact blocks, client side

#### 3.5.1 Negotiation

- At VERACK, P2a-core sends `sendcmpct(false, 1)` to a `CanRelay` peer (np:4215-4222).
- Intake (np:4273-4288) sets `provides_cmpctblocks` and `requested_hb_cmpctblocks`, and
  `m_bip152_highbandwidth_from`, for version 1 only. P2a-core does this too; P3 only reads it.

Both already ship in P2a-core. P3's part is HB selection and the requests.

#### 3.5.2 HB selection (np:1360-1416)

This runs from `block_checked` when the block is valid, `!ibd`, and
`in_flight.count(hash) == in_flight.size()` (np:2335-2339). The `ibd` is V's argument. The
condition reads "the only blocks in flight are this one's".

```
if ignore_incoming_txs { return }
let Some(st) = source state, provides_cmpctblocks else { return }
let mut outbound_hb = 0;
for (i, id) in hb_list.iter().enumerate() {
    if id == source { hb_list.remove(i); hb_list.push_back(source); return }
    if exists(id) && !inbound(id) { outbound_hb += 1 }      // State(*it) null for a gone peer: not counted
}
if source_inbound && hb_list.len() >= 3 && outbound_hb == 1
   && exists(front) && !inbound(front) { hb_list.swap(0, 1) }
// ForNode(source): push sendcmpct(true,1); peer.set_bip152_highbandwidth_to(true); hb_list.push_back(source)
if hb_list.len() > 3 { if let Some(p) = core.peer(front) { push sendcmpct(false,1); p.set_bip152_highbandwidth_to(false) }
                       hb_list.pop_front() }
```

- `ForNode` silently skips a peer that is gone or `fDisconnect`ed. The two-argument `ForNode` uses
  `FullyConnectedOnly` (neth:1344-1372), which is `NodeFullyConnected` (net.cpp:4852-4855), and P2a-core's `core.peer(id)` plus `fully_connected`
  give the same filter.
- The push for the source happens inside `ForNode`, so a disconnected source is not added to the
  list.
- `exists`/`inbound` follow `CNodeState`'s lifetime, which ends at `FinalizeNode`: presence in the
  processor's peer map plus the inbound flag kept in `PeerDownload`/`PeerState`, as
  `eviction.rs:160` does. `PeerHandle` presence would diverge while a peer has left SB6's table but
  is not finalized yet. A finalized peer is `State == nullptr` in Core, so it is "not counted" and
  the swap's guard fails.

`hb_list` keeps finalized ids (Q4). They count toward the 3, and the `ForNode` on such a stale
front does nothing but `pop_front` still runs (np:1405-1415). `test_highbandwidth_mode_states_via_getpeerinfo`
and a dedicated L0 test pin this.

#### 3.5.3 `cmpctblock` (np:5052-5293)

Steps 1–5 run under the lock. After an accepted step 5 the actor drains and relocks, and steps
6–13 run under the lock again (D46). The branches and their order are Core's:

1. If `loading_blocks`, log `"Unexpected cmpctblock message received from peer %d"` and return
   (np:5055-5058).
2. Decode. A decode failure is SB6's `Throw` path, as for every message.
3. If `prev` is unknown: outside IBD, `MaybeSendGetHeaders(getheaders|getheaders2, locator(best_header))`;
   then return (np:5069-5075). `UsesCompressedHeaders` selects `getheaders2`. This is P2a-core's
   `maybe_send_getheaders`.
4. `received_new_header = lookup(hash).is_none()`.
5. `process_headers([header], via_compact = true)`. If the result is invalid,
   `maybe_punish_node_for_block(state, via_compact = true, "invalid header via cmpctblock")` and
   return (np:5085-5090). An interrupted call returns, as on `headers` (`sync.rs:211-214`).
   Otherwise the handler returns `Redrain(Cmpct)`: the actor drops the lock, drains, relocks, and
   steps 6–13 read that drain's snapshot X′ (§2.6, D46).
6. If `received_new_header`, log `LogInfo("Saw new cmpctblock header hash=%s peer=%d")`
   (np:5092-5095).
7. `update_block_availability(peer, hash)`. Set `last_block_announcement = now_s` if
   `received_new_header && work > tip.work` (np:5110-5120).
8. If the block's status has `have_data`, return (np:5122).
9. Compute the in-flight facts: `already_in_flight`, `first_in_flight` (np:5130), and
   `requested_block_from_this_peer` (np:5124-5138).
10. If `work <= tip.work || n_tx != 0`: if requested from this peer, `getdata(MSG_BLOCK)`; return
    (np:5140-5149).
11. If `!already_in_flight && !can_direct_fetch()`, return (np:5153-5155).
12. If `height <= tip.height + 2` (np:5159):
    - `(already_in_flight < 3 && dl.blocks.len() < 16) || requested_block_from_this_peer` gives the
      **tracked** branch:
      - `block_requested(peer, id, want_partial = true)`. When it returns `false` with an existing
        `partial`, log `"Peer sent us compact block we were already syncing!"` and return;
        otherwise create the `partial` (np:5160-5171).
      - `init_data`:
        - `Invalid` → `remove_block_request(hash, Some(peer))`, `misbehaving(100, "invalid compact block")`;
        - `Failed` → if `first_in_flight`, `getdata(MSG_BLOCK)`, else `remove_block_request`.

        Return in both cases (np:5174-5189).
      - Collect the indexes with `!is_tx_available`:
        - empty → `fProcessBLOCKTXN` with empty `txn`;
        - else if `first_in_flight` → `getblocktxn`;
        - else if `bip152_highbandwidth_to && (!inbound || is_block_requested_from_outbound(hash) || already_in_flight < 2)`
          → `getblocktxn`;
        - else `remove_block_request` (np:5191-5218).
    - otherwise the **untracked** branch: a temporary `init_data`, and `fill_block` with no missing
      transactions. On `Ok` the block is reconstructed (np:5219-5236).
13. Otherwise (`height > tip + 2`): if requested from this peer, `getdata(MSG_BLOCK)` and return;
    else `fRevertToHeaderProcessing` (np:5237-5248).

After the lock section:

- `fProcessBLOCKTXN` → `process_compact_block_txns(peer, BlockTxn { hash, txn: [] })` (§3.5.4).
- `fRevertToHeaderProcessing` → P2a-core's `process_headers_message(peer, [header], via_compact = true)`
  (np:5257-5264). Today that function hard-codes `via_compact = false` (`sync.rs:190-224`), and
  `MaybePunishNodeForBlock` branches on it (np:1979-1995), so S3 adds the parameter.
- On reconstruction: `block_source.entry(hash).or_insert((peer, false))`, then
  `Process { force: true, tail: CompactReconstructed { id } }` (np:5266-5290).

Core holds `cs_main` across steps 7–13 but **not** across step 5, so another origin's accept and
its callbacks can run between `ProcessNewBlockHeaders` and the second section, which then sees
them. rev8.1 said that the gap could not be observed. It can: V may serve step 5 after another
origin's scope, and a step 7–13 reading the reply's snapshot would see a tip whose callbacks are
still queued, so step 7's work comparison or step 12's `bip152_highbandwidth_to` test could
disagree with every Core schedule (Opus r8 M2). Steps 1–4 read the message-start snapshot X, and
steps 6–13 read X′, taken through the drain after the reply, which is that gap (D46).

`getblocktxn` uses base-sdk's `GetBlockTxn` with differentially encoded increasing indexes
(blockencodings.h:47-77). The `getdata` pushes in steps 10, 12 and 13 are immediate one-entry pushes,
as in Core. They do not go into section 12's batch.

#### 3.5.4 `blocktxn` and `ProcessCompactBlockTxns` (np:3734-3816, np:5295-5307)

1. If `loading_blocks`, log `"Unexpected blocktxn message received from peer %d"` and return.
2. Decode `BlockTxn`, then compute the in-flight facts, where `requested_block_from_this_peer` needs a
   `partial` (np:3742-3756).
3. If it was not requested, log `"Peer %d sent us block transactions for block we weren't expecting"`
   and return.
4. If `partial.header` is none (an earlier attempt failed):
   `remove_block_request(hash, Some(peer))`,
   `misbehaving(100, "previous compact block reconstruction attempt failed")`, log
   `"Peer %d sent compact block transactions multiple times"`, and return (np:3765-3773).
5. `fill_block(txn)`:
   - `Invalid` → `remove_block_request`,
     `misbehaving(100, "invalid compact block/non-matching block transactions")`;
   - `Failed` → if `first_in_flight`, push `getdata(MSG_BLOCK)` **without returning** (np:3780-3794;
     the block is not processed, and `partial.header` is already null, Q7), else
     `remove_block_request` and log `"Peer %d sent us a compact block but it failed to reconstruct, waiting on first download to complete"`;
   - `Ok` → `remove_block_request(hash, Some(peer))`, `block_source.entry(hash).or_insert((peer, false))`,
     and `Process { force: true, tail: ProcessBlock }`.

`fill_block`'s two `Invalid` returns differ (Q8):

- a short `missing` returns before `header.SetNull()`, so the entry keeps its header (be:196-197);
- a long `missing` returns after it (be:204-208).

The port keeps both orders.

#### 3.5.5 Before the mempool and the extra ring are fed

Until MP1a, `tx_source` is `NoMempool`. Until P5 and MP1a add Core's call sites, nothing calls
`add_to_compact_extra_transactions`, so the ring is empty. Every `init_data` therefore finds only
the prefilled transactions (the coinbase, which Core's encoder always prefills at index 0,
be:21-35), and each compact block costs one `getblocktxn` round trip for all the others.

**What this matches, and what it does not (D5, D19).** It is byte-identical to a Dash Core node
whose mempool **and** extra ring are both empty, such as one that has just started. It is not
identical to a Core node that the same peer has fed. Core keeps transactions a peer relays even
when its mempool does not take them:

- a valid transaction enters Core's mempool (MP1a, P5);
- an orphan enters the orphanage and the extra ring (np:4997-4999);
- a rejected transaction whose `RecursiveDynamicUsage` is under 100,000 enters the extra ring
  (np:5016-5020).

Suppose a peer sends such a transaction and then a compact block containing it. Core's
`getblocktxn` omits that transaction's index; rdashd's lists it. The peer can see this at once, not
only statistically. It is an interim deviation, entered in ALLOWED-DIFFS through §10's PLAN delta.
It falls outside the PRD's three allowed classes, so it carries closing owners (P5 and MP1a) and
must be gone before REL-5.6. Until then the compact-client UA-parity claim is not made. The
orphan-then-compact sequence is a P5 acceptance row (§10), because only P5 can produce it. S3
covers the ring by direct injection (§6.1).

**Latency (m5).** The cost is one more RTT per compact block. Bulk download never uses compact
blocks: section 12 requests `MSG_BLOCK` (np:6827), and HB selection is off while
`IsInitialBlockDownload()` is true (np:2336), so peers do not announce them unsolicited. Direct
fetch is different. `CanDirectFetch` tests only the tip's time (np:1428-1431), not IBD, so
`HeadersDirectFetchBlocks` can swap in `MSG_CMPCT_BLOCK` (np:3247-3255) while IBD is still true.
This happens, for example, when the tip is recent but its chain work is below `nMinimumChainWork`
(v:1984). Such a block costs the extra round trip too. S2-e and S3 cover that case (§6.2).

The `p2p_compactblocks` subtests that need mempool transactions close with that file in P4's row
(§6.6).

### 3.6 Announcing compact blocks (D3)

P2a-core put `push_compact_announcement` (section 7's cmpctblock branch, np:6474-6496) on the
`BlockSync` seam (`sync.rs:517`). P3 therefore owns:

- **Section 7's branch.** P2a-core has already built `vHeaders`, tested
  `vHeaders.size() == 1 && requested_hb_cmpctblocks` (np:6474), logged
  `"SendMessages sending header-and-ids %s to peer=%d"` and set `best_header_sent` (np:6496,
  `sync.rs:470-473`) before it calls the hook (`sync.rs:517`). P3's hook only pushes. It sends the
  cached `m_most_recent_compact_block` if `m_most_recent_block_hash` is this block (np:6481-6486),
  else it reads the block from disk and encodes a fresh `CBlockHeaderAndShortTxIDs`, which draws its
  own nonce (np:6490-6494). Logging again or setting `best_header_sent` again would duplicate the
  line (D11).

  Reading from disk is `ReadBlockFromDisk` (np:6491). The port reads through a new
  `ChainAccess::read_block(id)`, from the LRU or with `pread` (A1 §2.14, A-3). A failure is Core's
  `assert(ret)` (np:6492). The port aborts the same way, and P4's O17 P2P-17 decision covers the
  prune race.
- **`NewPoWValidBlock` (np:2228-2268) runs whole on V's thread (D50).** §2.5 gives its steps. V
  calls it inside `AcceptBlock`, before the write (C8), and it pushes `cmpctblock` straight to each
  qualifying peer's send queue, so the actor no longer owns that send. It keeps reading `recent`,
  which the same call sets. Section 7's `read_block` runs under the peer's state lock; it reads
  block files only and takes no processor lock, which meets P2a-core D66 (1), and A-3 keeps it
  clear of every lock V holds inside the call.

  **V reads its own tree (C8; supersedes D28).** The visit's chain reads are
  `process_block_availability` (np:1442), which resolves `hashLastUnknownBlock` by a lookup by hash
  and Core's chain-work comparison, and `peer_has_header` (np:1433-1440), which takes the ancestor
  of the peer's best-known or best-sent entry at a height. Both run against C8's `TreeRead`, V's
  in-scope tree, so a header that is new in this accept (an RPC `submitblock`, an unsolicited
  `block`) is found as Core finds it under `cs_main`. rev4–rev9 carried the block's facts to the
  actor and resolved them as a one-entry overlay on a published snapshot; nothing needs that now.
  S4 tests a `submitblock` of a block whose header is new, with a peer that announced it by `inv`:
  that peer gets no `cmpctblock`, as in Core.

  **The header cell: each peer's share of `cs_main` (D50).** Core's callback and every `msghand`
  section that reads or writes these four fields hold `cs_main`. So each sees the other's effects
  entirely or not at all, and sees them together with the chain state they name. The port gets
  both properties from the cell and the announce stamp:
  - **Exclusion.** The actor holds peer P's header cell wherever Core's `msghand` holds `cs_main`
    while it handles P: for P's message handling, and for sections 6–12 of P's `send_messages`,
    which Core runs under one `cs_main` (np:6338-6839). A handler that Core runs in several
    `cs_main` sections is held as one region here, which is still one of Core's schedules; Q9
    records the longer wait it can cost V's visit. It drops the cell where Core's handler holds
    no `cs_main` either: inside a handler before each `ProcessHeaders` send
    (`ProcessNewBlockHeaders` takes `cs_main` itself, np:3362), and before `process_block`
    (np:3720). `getdata` serving is outside the cell (§2.6). V's visit of P then either runs
    before the actor's region, so its `cmpctblock` precedes the region's frames on P's send
    queue, or waits for the region and follows them.
  - **No wait on V under a cell.** The actor drops the cell before every `ChainAccess` call that
    sends to V, and an `Ahead` releases everything first. Inside a region it takes only P's state
    (already held), leaf cells, `recent`, published reads and `read_block` (A-3). The actor holds
    at most one header cell, as V does. So V's wait for a cell ends when one actor region ends
    (§3.8.1). S1 asserts this in every build: a `ChainAccess` call that sends to V panics if its
    thread holds a header-cell guard (a thread-local count).
  - **Visibility (the announce stamp, D56).** V stores its accept's stamp, the `view_seq` of the
    publication that ends the accept (C7 (a)), in `announce_stamp` before it touches `recent` or any
    cell. A region takes its peer's cell with `lock_headers(seq)` for its current drain snapshot: it
    locks the cell, takes the head's copy of `recent` if it is the head, then reads the stamp.
    `Ahead` means that a call has begun that the snapshot does not show. The actor then releases P's
    state and the cell, calls `wait_published`, drains again and retries the region on the new
    snapshot. Nothing of the region has run then, because the cell is taken at a region's start,
    right after a drain: at a message's start, at a `Redrain`'s relock, in phase 3 after the block
    wait, and at the head of `send_messages`. That is Core's `msghand` waiting on `cs_main` until
    `AcceptBlock` releases it, and then seeing the accept's chain state together with its callback's
    effects. If the stamp is ≤ seq, no call that began before the read is unpublished, and a call
    that begins later waits for this cell before it visits P: V's store precedes its lock of any
    cell, and the region's lock precedes its read. So the region is wholly before or wholly after
    every visit of P, with the matching chain state, whether or not the visit writes. A call that
    begins between the lock and the read gives a spurious `Ahead`, which only orders the region
    after it. Of what V writes before it takes a cell (the stamp, `highest_fast_announce` and
    `recent`), sections 6–12 read only `recent`, and the region reads it and the stamp in the
    reverse order, so nothing V writes early escapes the check (D58).
  - **`recent` (D56, D58).** The head copies `recent` (three `Arc`s and a hash) inside
    `lock_headers`, after it locks the cell and before it reads the stamp, and section 7's
    cmpctblock branch uses the copy. V stores the stamp before it writes `recent`, and both
    sides take `recent`'s mutex. So if the copy shows a call's `recent`, the stamp read after it
    shows that call's stamp and returns `Ahead`: the copy and the snapshot can never disagree.
    If the read passes, the copy predates every call that the snapshot does not show, and such a
    call waits for the cell before it visits P. Either way the region pairs S with the `recent` of
    S, as Core's section 7 reads `m_most_recent_block_hash` under `cs_main` (np:6481-6486),
    which `NewPoWValidBlock` holds too. The proof needs only the mutex's ordering and the
    atomic's coherence. P4's `getdata(MSG_CMPCT_BLOCK)` serving reads `recent` live, as Core's
    `ProcessGetBlockData` reads it without `cs_main` (np:2741).
  - **Other readers.** `getpeerinfo`'s `sync_height` (`stats.rs:67`) takes the cell with
    `lock_headers` for the `view_seq` of the snapshot `resolve_heights` uses. On `Ahead` the RPC
    thread releases the peer's state, calls `wait_published`, and retries with a fresh published
    snapshot (it has no queue to drain), as Core's `getpeerinfo` waits on `cs_main`.
    `consider_eviction` (`eviction.rs:94-111`) runs in section 11, inside the head's region. A-13
    lists every reader. The lock order is `PeerState`, then the header cell, everywhere.

  **Ordering against section 7 (F1, D34; manager rulings: keep Core's order, DEC-236).** In Core
  both effects run in V's order. `NewPoWValidBlock` runs synchronously in `AcceptBlock`, under
  `cs_main`, and sends `cmpctblock(B)` and sets `pindexBestHeaderSent`. The queued
  `UpdatedBlockTip` appends to `m_blocks_for_headers_relay` later, on the scheduler thread, under
  each peer's `m_block_inv_mutex` and without `cs_main` (np:2296-2305). Section 7 runs under
  `cs_main` and takes `m_block_inv_mutex` (np:6409), so it sees each effect entirely or not at
  all. rev3 let section 7 see headers whose compact announcement was still queued. Sol r2 F1's
  case shows that this changes the wire: B is RPC-submitted with a stored descendant D, and the
  peer gets `headers(B,D)`, or `inv(D)` before `cmpctblock(B)`. Sol r3 found a second crossing in
  rev4: an entry pushed after the snapshot but applied before section 7 (S → N → H). rev10 runs
  the callback where Core runs it, so no announcement is ever queued to cross. The appends stay in
  Core's per-peer cell, which the head of `send_messages` reads with S as one observation, and the
  header cell orders the callback against section 7 (D37, D44, D50):

  1. **The announcement cell stays per peer (DEC-236).** `Shared::updated_block_tip`, on the
     signals thread, keeps P2a-core's loop (`peerman.rs:129-149`), a port of np:2296-2305. It
     visits a copy of the connection table in insertion order (`netcore.rs:643-646`, Core's
     `m_nodes` order), tests `fully_connected && can_relay` at each visit, as Core's
     `ForEachNode` tests each node it reaches, and appends to that peer's `block_inv` cell under
     the cell's leaf lock. The cell's list is a `RelayList` (§2.5). The callback never waits.
  2. **V's signals are stamped.** Every queued `BlockChecked` carries one more than the
     `view_seq` V last published (C7 (a), §2.5), and so does `announce_stamp` for each
     `NewPoWValidBlock` call. V is the only producer of both, so stamps never decrease.
  3. **Every drain is one observation.** A drain takes `(X, seq) = drain_snapshot()`, one
     `ChainView` load (D47). It then applies the queued prefix stamped ≤ `seq`, in order, against
     X; later entries stay queued for the next drain. The drain points are:
     - the pass start;
     - the start of each message, and the end of a V round trip inside one (point 5);
     - after each peer's `ProcessMessages` (`peerman.rs:414`);
     - inside the block wait;
     - the head of `send_messages` (point 4);
     - after an `Ahead` wait.
  4. **The head of `send_messages` (np:6409).** In order:
     1. lock the peer's `block_inv` cell;
     2. take `(S, seq) = drain_snapshot()`, the snapshot sections 6–12 use;
     3. move the `RelayList` out of the cell, then unlock it;
     4. drain to `seq`;
     5. lock the peer's state, then its header cell with `lock_headers(seq)`, copying `recent`
        inside it before the stamp read (D58). On `Ahead`, discard the copy, release both,
        `wait_published`, take a new `(S, seq)` through a drain, and retry this step, keeping the
        moved list;
     6. run the sections, section 7 on S with the moved list, then release the cell and the state.

     `drain_snapshot` is an `ArcSwap` load and takes no lock, so the `block_inv` cell stays a
     leaf. Today's code takes S at section 6 (`logic.rs:797`) and the list inside section 7
     (`sync.rs:423`). S1 moves both to the head. An append after the unlock waits for the peer's
     next round, as Core's scheduler waits on `m_block_inv_mutex` behind a section 7 in progress.
     Keeping the moved list across an `Ahead` retry is Core-reachable. Every tip the list names is
     in the first S and so in the new one, and an append that lands after the unlock is a later
     append of the scheduler, which Core's queued `UpdatedBlockTip` can also be behind when
     section 7 takes the mutex.
  5. **(I0) The actor acts on drained snapshots (D44, D46).** A message's handling reads one
     snapshot (P2a-core R8, `logic.rs:460`). S1 takes it through a drain at the message's start,
     before the peer's state is locked, so no message acts on a publication whose entries are
     still queued, or on a header cell ahead of its snapshot. **A V round trip inside a message**
     ends the same way. A handler that awaits V partway through continues on a snapshot taken
     through a drain after the reply, never on the reply's own snapshot, which can show another
     origin's publication whose entries are still queued. The handlers are `ProcessHeaders` in
     the `headers` handler, and `cmpctblock`'s step 5 with its header-processing fallback (§2.6's
     `Redrain`, A-13). The reply contributes only its own facts: `last_accepted` and the invalid
     state. The one exception is `getdata` serving's per-item snapshot (`logic.rs:727`), taken
     under the peer's state lock. It only serves data, which P4 owns, and reads no header cell.

  **Mechanism.** Stamps order V's queued entries and V's `NewPoWValidBlock` calls against the
  actor's snapshots. The header cell orders V's visit against the actor's regions, as `cs_main`
  does. C5′ orders each accept after every `BlockChecked` before it, as Core's inline
  `BlockChecked` is. The
  `block_inv` cell orders the signals thread's append against the head's snapshot, as
  `m_block_inv_mutex` does. V never waits behind the actor while the actor waits on V: the only
  lock both take is a header cell, and the actor never holds one across a V wait. DEC-229 and
  DEC-230 gain no edge, nothing new runs while `holding`, and the signals thread keeps LO-9's
  no-wait marker.

  **Four invariants.**
  - **(I1) Nothing applied is newer than S.** Every applied entry is stamped ≤ the `view_seq` of
    its drain's snapshot, and the actor's drain snapshots only grow, so ≤ S's. By C7 (a), S then
    reflects the state each applied entry was emitted in.
  - **(I2) Nothing S reflects is pending.** Suppose S reflects the hold or scope that pushed an
    entry E. Then E's stamp is ≤ the first publication after E, which is ≤ S's `view_seq`. Both
    are `ChainView` `view_seq`s, because S and its cutoff come from one load (D47). V's earlier
    pushes have smaller stamps, so the whole prefix up to E is eligible, and the head's drain
    applies E before section 7.
  - **(I3) The moved list and S are one observation (DEC-236).** If the moved list holds U(T), U
    appended before the head locked the cell, so after T's publication (C6). S, loaded under that
    lock, therefore shows T. T's accept, and so its visit of this peer, ended before that
    publication, so by (I4) section 7 reads the visit's writes. An append after the unlock goes to
    the peer's next round, even if S already shows its tip. That is Core's scheduler blocked
    behind section 7 (np:6409).
  - **(I4) A held header cell agrees with its snapshot (D50, D56).** While the actor holds P's
    header cell after `lock_headers(seq)`, every `NewPoWValidBlock` call that began before its
    stamp read is published in S, so S shows every block the cell names and the region's copy of
    `recent`, which was taken before that read (D58); and every call that begins later waits for
    the cell before it visits P. So the region falls wholly
    before or wholly after each visit of P, with the chain state of that order.
  - **(I5) No accept overtakes a `BlockChecked` (C5′, D55).** When an accept outside IBD begins,
    the actor has processed every `BlockChecked` V pushed before it, including its `sendcmpct`
    pushes. So on every connection the frames of `BlockChecked(B)` precede those of a later
    accept's visit, as in Core, where `BlockChecked(B)` runs inside `ConnectTip(B)` and the next
    `NewPoWValidBlock` needs B as the tip (v:3432, v:4831).

  rev6–rev8.1 carried the appends through the queue as `HeadersRelay` entries, with coalescing,
  recipient lists, `retire`, and a relay slot that made the signals thread wait on the actor
  (D38–D43, DEC-233). DEC-236 withdrew all of them (D44). rev4–rev9 queued `NewPoWValidBlock`'s
  per-peer half for the actor (D27); rev10 withdrew it (D50).

  **Proofs against Core's `cs_main` schedules.** In Core, `NewPoWValidBlock` runs inside
  `AcceptBlock`'s `cs_main` section. The scheduler thread runs `UpdatedBlockTip` some time after the
  connect, and section 7 runs under `cs_main` and `m_block_inv_mutex` (np:6409). So section 7 sees
  a chain state together with every callback emitted up to it, plus the appends that completed
  before it took the mutex. (I1)–(I5) give exactly that: S, every `BlockChecked` and every visit S
  reflects, and the appends that completed before the head locked the `block_inv` cell, each of
  whose tips S shows.
  - **Reorg crossing (Sol r3).** Tip A is active, and A's append is in the peer's cell. An HB peer
    knows the fork parent and branch B, but not A. The head takes S (A active) and the peer's
    header cell. V then reorganizes to B and accepts C on B; C's visit of the peer waits for the
    head's region. Section 7 sends `cmpctblock(A)` from S and sets `best_header_sent = A`. When the
    head releases the cell, the visit finds that the peer has B, C's parent, and lacks C, so it
    sends `cmpctblock(C)` and sets `best_header_sent = C`. That is Core's schedule in which section
    7 ran before the reorg. If C's call begins before the head reads the stamp, the head runs
    section 7 on a snapshot that shows C's accept: S itself, or after `Ahead` the snapshot it
    waited for. Section 7 then finds A off the active chain and takes the inv fallback, after
    `cmpctblock(C)`. That is Core's schedule in which section 7 ran after C's callback. `cmpctblock(C)` before
    `cmpctblock(A)`, or `best_header_sent` moving back from C to A, cannot occur.
  - **RPC-block crossing (Sol r2).** B is RPC-submitted while a descendant D is stored. B's visit
    runs inside B's accept, before B's publication, and U(B, D) appends only after the publication
    that shows D (C6), so after the visit. If the moved list holds U, S shows D, and by (I4)
    section 7 reads `best_header_sent = B` from the cell. With headers preferred the peer gets
    `cmpctblock(B)` then `cmpctblock(D)`; with neither, `cmpctblock(B)` then `inv(D)`, as in Core.
    If the visit waits for the head's region, B's accept has not published, so S does not show B,
    the list holds neither, and section 7 announces neither. The visit then sends
    `cmpctblock(B)`, and D follows in a later round. The older-header case also holds. If section
    7 holds a pending P and B extends P, the cell gives section 7 `best_header_sent = B` whenever S
    shows B (I4), so it skips P as Core does.
  - **A read-only visit (Opus r10 M2, D56).** Tip B; HB peer P has best-known A. RPC accepts C
    on B, and its visit finds `PeerHasHeader(B)` false, so it pushes nothing. P's region for
    `inv(B, X)` then starts. It reads a stamp newer than its snapshot, so it waits and runs on a
    snapshot showing C: it sets best-known B and sends `getheaders` with a locator from C. That is
    Core's accept-first schedule. If the region holds P's cell first, the visit waits; the region
    sends `getheaders` with a locator from B, and the visit then finds `PeerHasHeader(B)` and
    pushes `cmpctblock(C)`. That is Core's region-first schedule. rev10's first draft stamped only
    written cells, so the region could run after the visit on the old snapshot, a mix of the two.
  - **A call between the cell and the cache copy (Sol r10 F1, D58).** Tip T is `recent`; HB
    peer P knows A, T's parent, has not started sync, and has U(T) moved. P's head takes S at T
    and locks P's cell. RPC accepts C on T: V stores C's stamp, sets `recent = C`, and waits for
    P's cell. If V's store comes after the head's stamp read, the head copied `recent = T`
    before it, and the region runs on S: section 6 sends `getheaders` with a locator from A,
    section 7 sends the cached `cmpctblock(T)` with no new nonce, and the visit then sends
    `cmpctblock(C)`. That is Core's region-first schedule. If the store comes before the read,
    the read returns `Ahead` whatever the copy showed; the head discards it and runs on a
    snapshot showing C: a locator from T, `cmpctblock(T)` encoded from disk with one nonce draw,
    and the visit, which ran in the wait, skipped P because P did not know T. That is Core's
    accept-first schedule. rev10 read the stamp before the copy, so the region could pair S with
    `recent = C`: a locator from A and a fresh encoding of T, then `cmpctblock(C)`, which no
    `cs_main` schedule gives.
  - **`BlockChecked` before a later accept (Opus r10 M1, D55).** A stored B with source P is
    connected by another origin (`reconsiderblock`, `ClEnforce`), and `BlockChecked(B)` is queued.
    An RPC `submitblock D` on B then reaches V before the actor drains. By C5′, V waits until the
    actor has processed `BlockChecked(B)`, so P gets HB selection's `sendcmpct(1)` before
    `cmpctblock(D)`, as in Core.
  - **P2P-block crossing.** For the actor's own block, V's visit runs inside the accept while the
    actor waits for the reply and holds no cell, so each HB peer gets `cmpctblock(B)` before V
    writes B. The wait's drains apply `BlockChecked` entries as V publishes them. The drain after
    the reply takes a snapshot after C2's publication, so by C7 (a) it applies everything V pushed
    before the reply (C1). The tail and that peer's `send_messages` follow on the same thread, with
    S at least as new. That is Core's `msghand` order: callbacks inline, then the tail, then
    `SendMessages`.
  - **A relay racing a full connection (Sol r4).** V has published A, and in B's hold it pushes
    `BlockChecked(B)`. Peer P, which knows A's parent but not A, completes `VERACK`, then Q's head
    takes S showing A. rev5 queued U(A) behind `BlockChecked(B)`. So Q got nothing at S, while P,
    connected by the time U(A) was applied, later got only `inv(B)`. Now no append queues:
    - Q gets `headers(A)` at S, or its preference's form, exactly when U(A) appended to Q's cell
      before Q's head; otherwise it gets A in its next round, with B.
    - P gets A exactly when U(A) visits P after its `VERACK`.
    - Q getting nothing at S while P gets `inv(B)` alone needs U(A) to visit P before the
      `VERACK` and Q after the head, so P must precede Q in the table. Core reaches that outcome
      under the same order: its scheduler visits P, `msghand` runs P's `VERACK` and enters Q's
      section 7, and the scheduler then blocks on Q's `m_block_inv_mutex`. Under the other order
      neither reaches it (D44).
  - **A peer reaching full connection.** `fSuccessfullyConnected` is set only while the actor
    handles `VERACK` (np:4257). Each callback reads its recipients as Core's `ForEachNode` does, at
    each visit:
    - **`NewPoWValidBlock`** runs inside V's scope, in Core and in the port. Its visit tests
      `fully_connected` at each peer as it reaches it. A peer whose `VERACK` the actor completes
      while the visit is between two peers is seen by exactly the later visits. If the visit
      reaches the peer while the actor is still in that `VERACK` region, it sees the flag and
      waits for the cell, which is Core's visit after that `VERACK`. If it reaches the peer before
      the flag is set, it skips it, which is Core's visit before. D39 (3)'s recipients read at the
      applying drain are superseded (D50).
    - **`UpdatedBlockTip`** runs on Core's scheduler thread, outside `cs_main`. `ForEachNode`
      tests each node as it reaches it. The port's callback is the same loop over the same order,
      so a `VERACK` that the actor completes while the callback is between two visits is seen by
      exactly the visits after it. In Core, `msghand` can likewise run it while the scheduler is
      between two nodes. A peer finalized before its visit has no cell. One finalized after it
      loses its cell with its state; Core's append to it would go to a socket that is closing.

  **Liveness.** A `BlockChecked` entry becomes eligible once V publishes the hold that pushed it.
  After that publication, the actor is woken by `wait_for_space`'s ring and `wake()` at V's next C5
  point, which fire whenever the queue is non-empty, so they end both the block wait and the idle
  wait (D45). The actor is also woken by the listener's `block_connected` ring, by the reply for
  its own block, and otherwise by the next pass, which is at most the 100 ms idle wait.
  `NewPoWValidBlock` needs no wake, because it finishes on V; its visit waits at most one actor
  region for each peer whose cell is held. An `Ahead` wait returns at once if the accept has
  already published, and otherwise ends at V's next serve point after the accept, ABC's first loop
  head or the idle point (C7 (b)). A C5′ wait ends at the actor's next drain point, at most one
  region away. An append reaches section 7 at the
  peer's next `send_messages`, and `updated_block_tip` wakes the actor as it does today. C7 (b)
  keeps V's space wait from depending on an entry the actor may not yet apply (§3.8.1).

  So every frame that section 7 and V's visit send to one peer, their order, every
  `best_header_sent`, and the draw log follow a Core-reachable order.

  **Timing (Q9, F1).** Core pushes `cmpctblock(B)` from inside `AcceptBlock` (v:4832), before
  `SaveBlockToDisk` (v:4837) and before `ActivateBestChain` (v:4899), so an HB peer gets it before
  Core validates B's scripts: BIP152's high-bandwidth relay. The port pushes it at the same point,
  on V's thread, before V writes B and before B's ABC starts (D50). rev9 instead applied it on the
  actor after a ring at ABC's first loop head. Sol r9 F1 showed that a ring orders nothing: V can
  run through `ConnectTip(B)` before the woken actor runs, so the announcement could trail storage
  and script checks.

  Three timing differences remain. All are recorded as Q9, and §6.3's probe measures them (D52):
  - **A held cell.** Core's `AcceptBlock` waits for `cs_main` before it starts, if `msghand` holds
    it. The port's visit waits mid-callback, for the one peer whose region the actor is in, after
    it has pushed to the peers before that peer in the table. Per connection the frames and their
    order are Core's. Across connections, a peer earlier in the table can get `cmpctblock(B)` up
    to one actor region before the frames of a later peer's region, which Core would have sent
    first. A region contains no wait on V, so it lasts one message's handling or one
    `send_messages`; a handler that Core splits into several `cs_main` sections is one region
    here.
  - **`BlockChecked` stays queued.** Its effects need `block_source` and the HB list, which are
    actor-private. In Core it runs inline after `ConnectBlock` (v:3432), so its `sendcmpct(1)`
    already follows validation there. The port applies it after the hold's step publishes, at
    the actor's next drain: the rest of that ABC step plus one doorbell wake.
  - **C5′'s wait.** An accept that finds a `BlockChecked` outstanding waits for the actor's next
    drain, up to one region. Core's accept never waits for that, because `BlockChecked` ran inline.
    It arises only when another origin's connect is followed at once by an accept.

  **Parity claim (restated).** Announcement frames, their order against section 7, their bytes,
  `best_header_sent` and the D60 draw log match a Core-reachable order on every connection. The
  residuals are Q9's three timing differences.

**Encoder.** `CmpctBlock::from_block` follows be:21-35: prefill only the coinbase at index 0, short
IDs of every other transaction's instance hash, with keys from SHA256(header ‖ nonce). base-sdk has
the message type and `short_id_with_keys`. P3 adds the constructor in `compact.rs`, not in base-sdk
(D4).

The nonce comes from `GetRand<uint64_t>()` (be:22), a self-seeding `FastRandomContext`. Like the
rejects-filter tweak (P2a-core D69), that is a `(false, 32)` call in the D60 draw log.
`NewPoWValidBlock` draws it before the height check, because it builds the
`CBlockHeaderAndShortTxIDs` first (np:2229 runs before np:2234). Section 7's disk path draws once
per encode. The port draws at the same points, or `diff_p2p_multi_blocks` would show a missing or
extra draw (Q12).

**Serving** `getdata(MSG_CMPCT_BLOCK)` and `getblocktxn` is P4's. It reads `Shared::recent`.

### 3.7 The `BlockChecked` ordering contract (D65 option, D1)

**Core** (np:3717-3732 inside the handlers; np:2314-2343):

```
handler: [pre: RemoveBlockRequest(hash, from), mapBlockSource.emplace]          (np:5376-5384)
ProcessNewBlock:  CheckBlock; AcceptBlock{ NewPoWValidBlock (v:4832) }; on failure BlockChecked (v:4891)
                  ActivateBestChain{ per ConnectTip: BlockChecked (v:3432) }    — all inline
tail:  if new_block { m_last_block_time = now; RemoveBlockRequest(hash, nullopt) }
       else         { mapBlockSource.erase(hash) }                              (np:3717-3732)
```

**`BlockChecked`** (np:2314-2343):

```
it = mapBlockSource.find(hash)
if state invalid: if it found && State(it.peer): MaybePunishNodeForBlock(it.peer, state, !it.second)
else if state valid && !ibd && mapBlocksInFlight.count(hash) == mapBlocksInFlight.size():
     if it found: MaybeSetPeerAsAnnouncingHeaderAndIDs(it.peer)
if it found: mapBlockSource.erase(it)
```

**Option chosen: the tail runs only after delivery (D1).** In §2.6 the wait services the signal
queue, a final drain follows the reply, and then the tail runs. So:

- **Invalid branch.** At `BlockChecked`, `block_source` still holds this message's entry. The tail's
  `erase` for a non-new block runs after it, as in Core. No source is lost (the D65 hazard is gone).
- **Valid branch.** `in_flight` at `BlockChecked` is as phase 1 left it: after the handler's
  `remove_block_request(hash, Some(peer))` and before the tail's `remove_block_request(hash, None)`.
  That is exactly Core's state at v:3432. The `count == size` test therefore reads the same values,
  not merely the same truth value.
- **Later connects.** A block stored out of order is connected in a later ABC, triggered by another
  message. Its `BlockChecked` then finds its `block_source` entry, which persisted because that
  block's own tail saw `new_block = true` and did not erase it. HB selection credits the original
  source, as in Core.
- **`ibd`.** `ibd` is the `SyncSignal`'s V-evaluated flag (CH1-A8, `signals.rs:41-43`), never
  `snap`'s (P2a-core D65, test `block_checked_hands_the_seam_the_signals_ibd_not_the_published_view`).

The other D65 option was rejected: erasing in `block_checked`, then dropping entries V never
signals. It would need a second rule to clean up `block_source` for non-new blocks that V never
signals (`CheckBlock` passes, then `AcceptBlock` returns early on `fAlreadyHave`, v:4804).
That rule would diverge from Core's map contents and gain nothing (D1).

**The tail** (`block_tail`, phase 3, on the C1 drain's snapshot `x3`, D47):

```
if outcome.new_block { peer.set_last_block_time(now_s); remove_block_request(hash, None) }   // conn.rs:641-697 setter
else { block_source.remove(&hash) }
if let Tail::CompactReconstructed { id } = tail {
    if chain.status(x3, id).is_some_and(|s| s.is_valid(Validity::Transactions)) {
        remove_block_request(hash, None) } }                                                  // np:5284-5286
```

`m_last_block_time` is the `CNode` field that eviction reads (P4/SB6). The `CompactReconstructed`
read happens after C2's publication. A `ProcessBlock` that returns `Interrupted` runs no tail, and
the actor is stopping.

### 3.8 Back-pressure

Core's model: `msghand` blocks in `ProcessNewBlock` for the block's whole ABC (v:4899-4906). During
that time no peer's messages are processed, and the socket thread fills each peer's
its receive queues up to the flood size. Dash computes `fPauseRecv` over the quorum and normal
process queues together (`m_msg_quorum_queue_size + m_msg_process_queue_size > m_recv_flood_size`,
net.cpp:4803, 4833, 4847), and `PollMessage` (net.cpp:4806) favours the quorum queue. SB6's receive
queues must keep both rules; P3 relies on them and does not change them. The port keeps this model:

- **One outstanding `ProcessBlock` from P2P.** The actor waits, and the reply is `Activated`
  (CH1-node §2.2, every Core caller). It never pipelines blocks to V. Pipelining would change which
  blocks are requested next: section 12 runs only after this peer's `ProcessMessages` returns, so
  Core's request schedule depends on blocks being connected first.
- **Receive side.** SB6's per-peer receive queue applies `fPauseRecv` at `-maxreceivebuffer`, so the
  reader tasks stop reading the socket, as Core's socket thread does.
- **Store side.** The 1024 window (np:168) bounds how far ahead of the connected tip blocks can be
  stored, and V stores them (CH1-node / CH2 block files) as Core does (`AcceptBlock` writes
  out-of-order requested blocks, v:4804-4837). `MIN_BLOCKS_TO_KEEP = 288` (validation.h:88) only
  limits unrequested blocks (v:4794, v:4806-4808).
- **V's side.** `ChainCommand` lanes are bounded (CH1-node §2, `actor.rs:194-198`), and the net actor is an unmarked
  caller (LO-9), so it may wait on `Activated`, through the draining wait DEC-229 requires. The host listener is marked and only queues (§2.5),
  so cycle B (CH1-node:1086) cannot form.
- **Window base.** The 1024 window is measured from each peer's `pindexLastCommonBlock`
  (np:1511), which advances as blocks finish downloading in order (`HaveTxsDownloaded`), not from
  the connected tip.
- **Cost of a long wait.** A missing block that unlocks many stored ones makes one ABC connect them
  all, so the actor waits for that whole ABC. Pings go unanswered meanwhile, exactly as in Core. That
  is intended parity: a peer sees the same silence, and the 20-minute ping timeout (np:6146-6156,
  `TIMEOUT_INTERVAL` neth:72) is far above any testnet ABC.
- **V's side, signals.** The `SyncSignal` queue is bounded, and V waits for it only at release
  points, never while `holding`, before the actor's first drain, or inside another such wait (C5,
  §2.5, §3.8.1).

#### 3.8.1 Why V's wait for the signal queue always ends (C5, DEC-229)

V waits for queue space only at C5's release points, only while the queue is `Live`, never while
`holding`, and never inside another such wait (§2.4). So the question is whether the net actor
always reaches a drain while V waits there.

**The rule (DEC-229, binding; DEC-230 for pre-start).** LO-9 says that nothing V waits on ever waits on V. C5 adds one
edge: V waits on the net actor's drain. DEC-229 makes the net actor a V-waitable task under this
rule:

> The net actor awaits V in only two ways:
>
> - (a) through a **draining wait**, which drains the `SyncSignal` queue on every doorbell ring
>   while it polls the reply (§2.6); or
> - (b) for the reply to a command that **every release point serves**: a class R or M command
>   whose reply is complete when the command returns (R/M-complete, CH1-node §8.3).
>
> The startup hold is excepted from (b), because V's release points then hold class M commands.
> C5 covers it: V never waits for queue space while `holding`. Any actor wait on a reply that
> needs class C work must be a draining wait. That includes `Activated`, an ATMP outcome that
> waits for a tip change, and a ChainLock or LLMQ handler that awaits a reorg.

The rule binds every later row that adds an actor handler that awaits V: P5's ATMP waits, the
LLMQ and ChainLock message handlers, and P4's serving if it ever awaits V. Each such row cites
DEC-229 and either shows its reply is R/M-complete or uses the draining wait (the shape of
`process_block`'s loop, §2.6).

**The instances at P3's merge.** These are the actor's waits once P3 lands, each with the clause
that covers it:

| The actor waits on | Clause | Why the wait ends while V waits for space |
|---|---|---|
| `ProcessBlock { Activated }` (§2.6) | (a) | The wait itself drains on every doorbell ring. V's pushes ring it, and `wait_for_space` rings again before it parks. Each processed entry releases space and wakes V (m-3). |
| `ProcessHeaders`, under the peer lock, with no drain: the `headers` handler, the `cmpctblock` header step (np:5085-5090) and `fRevertToHeaderProcessing` (np:5257-5264) | (b) | `ProcessHeaders` is R/M-complete (CH1-node §8.3). Every wait mode services class R and M commands; that is CH1-node's positive control P-2. The reply arrives, and the actor drains right after it (`Redrain`, D46), or at the message's end and the head of `send_messages`. During the hold V does not wait (C5). |
| A send on V's bounded lane when it is full (`actor.rs:194-198`) | a send, not a reply | Every wait mode takes the commands queued on both lanes, runs the R/M ones and defers the class C ones (`actor.rs:8-13`; the idle mode, A-1b). The lane empties and the send completes. During the hold, held commands go to `held` with backpressure off (`actor.rs:917`), and V does not wait. |
| Section 7's `read_block` (A-3), under the peer lock and the header cell, possibly while V's visit waits for that cell | no V | It reads the block LRU or the block files and awaits no V reply. A-3 asks CH1-node to guarantee that `read_block` takes no lock W holds while W waits on V's bell service, and none V holds in any C5 wait, so the read completes in I/O time. |
| `wait_published` after an `Ahead` (§3.6, D50, D56) | (b) | It returns at once if the publication has happened; otherwise a class R no-op round trip, R/M-complete. Every wait mode serves class R, and the actor holds no lock while it waits. During the startup hold no accept runs, so no `Ahead` arises; one already pending would end with the hold, and V waits on nothing of the actor's meanwhile (C5). |
| P2a-core's leaf cells, `recent`, and a header cell that V's visit holds for one peer | no V | Leaf operations; V's visit holds a header cell only for one peer's tests and push, and waits on nothing the actor holds while it does. |
| The idle 100 ms wait | no V | `wake()` ends it, and the pass starts with a drain. |

Every actor wait on V is either a draining wait or finishes during V's own wait, and every other
actor wait involves no V. So no cycle can form.

**V's wait for a header cell (D50).** V's visit may wait for a header cell that the actor, or an
RPC thread reading stats, holds. That is not a C5 wait and adds no DEC-229 edge, because no holder
of a header cell waits on V while it holds one. The actor drops the cell before every
`ChainAccess` call that sends to V; an `Ahead` releases every lock before `wait_published`; and
inside a region the actor takes only that peer's state, leaf locks, published reads and
`read_block`, which A-3 keeps clear of every lock V holds in the call. The `ProcessHeaders` wait
that DEC-229 (b) keeps under `PeerState` holds no header cell, and V never takes a `PeerState`. So
V's wait ends when one region ends: LO-5's case of a lock V may wait behind (C8). A thread that
held a header cell while it waited on V would close a cycle, so S1 asserts in every build that no
header-cell guard is alive on a thread whose `ChainAccess` call sends to V.

**C5′'s wait (D55).** It is a C5 wait at C5's own points, so the argument above applies: every
actor wait on V either drains or is served by V's wait modes, so the actor reaches a drain while V
waits. A C5 wait defers a `ProcessBlock` accept instead of running it, so the no-nesting rule is
never needed for it. The deferral blocks no actor wait: the actor's own `ProcessBlock` is a
draining wait (a), and DEC-229 (b) covers only R/M-complete replies, which the wait still serves.
C5′ is DEC-240, the manager's amendment to DEC-229 and C5 (D55).

**The cutoff cannot stall V (D37).** A drain applies only entries its snapshot covers (§3.6). By
C7 (b), V has published since its last push whenever it reaches a C5 wait point. Before parking,
`wait_for_space` rings the doorbell, so the actor's next drain takes a snapshot that covers every
queued entry and can release all their space. V is the queue's only producer, so nothing else
can hold space while V waits (D44).

The other origins:

- **Startup.** Before the actor's first drain the queue is `PreStart` (DEC-230), and V never
  waits for it (§2.5). During the startup hold V never waits (C5). Neither can block V's startup ABC or an
  import, whatever ND1's init waits for (A-12).
- **Reorg.** Disconnect steps emit no P3 signal (`BlockDisconnected` has no P3 effect, §1.2). Each
  outer ABC iteration adds at most §2.5's C term, and the next iteration starts at v:3760.
- **RPC** (`submitblock`, `reconsiderblock`, `preciousblock`, `invalidateblock`). These run on V,
  as P2P blocks do, and reach the same release points; `invalidateblock`'s loop has v:3940. The RPC
  thread waits on V. V's wait does not depend on it. Each RPC worker adds at most one accept to Q
  (§2.5).
- **ChainLock enforcement** (`ClEnforce`, class C). It runs `InvalidateBlock` and ABC, so it
  reaches v:3940 and v:3760 like the RPC paths, and runs deferred, never inside a wait. It is the
  likeliest source of a reorg storm on Dash; each of its outer ABC iterations adds at most the C
  term.
- **Reindex and `-loadblock`.** While `LoadingBlocks()` is true, IBD is true (v:1979) unless it
  had already latched false (v:1973), so V normally emits no `NewPoWValidBlock` (v:4831). Each
  import block reaches a C5 point before its `AcceptBlock`, and the import's ABC reaches v:3760 at
  each iteration. Before the first drain, `PreStart` applies.
- **Listeners on the `ValidationSignals` thread** (P2a-core's `updated_block_tip` and
  `block_connected`). Both only append to leaf cells and ring. Neither waits, so LO-9's no-wait
  marker on the thread holds unchanged (DEC-236 withdrew DEC-233's relay wait, §3.8.2, D44).
  `limit_queue` waits on the thread as before.
- **Shutdown.** The actor's exit closes the queue through its drop guard, on every exit path,
  which ends any wait at once (§2.5). When V stops, the actor's waits become `Interrupted`.

V waits here only when the actor lags by more than the bound. Core would have run those signals
inline before it continued, so no Core-visible order changes. S1 tests this, including the hold,
pre-start and N-sender cases (§6.1), and A-1b adds the same cases to CH1-node's lock-and-wait
model.

#### 3.8.2 Withdrawn: the signals thread's relay wait (DEC-236, D44)

rev7–rev8.1 carried `UpdatedBlockTip`'s appends in the signal queue and capped them with a relay
slot that the signals thread waited for (DEC-233, D41, D43). DEC-236 supersedes DEC-233. The
appends stay in P2a-core's per-peer cells (§3.6), so there is no `HeadersRelay` entry, relay slot,
reservation token or S → A wait edge. The signals thread keeps LO-9's marker and never waits on
the net actor. The wait-for graph is §3.8.1's: V → A (C5) and V → S (`limit_queue`, `sync`) are
the only edges into the listeners. The actor never waits on the signals thread, and the signals
thread waits on no task (only leaf locks), so no wallet, ZMQ or later P5 listener queues behind a pending
announcement.

### 3.9 Block-relay-only connections

P2a-net's connection types come from SB6's `ConnectionType` (`BLOCK_RELAY`), with
`Peer::is_block_only_conn()`. Core gives `block-relay-only` peers no special download rule:

- `CanServeBlocks`, `CanRelay` and `IsLimitedPeer` are the same, and `fPreferredDownload` is
  `(outbound || noban) && !AddrFetch && CanServeBlocks` (np:4109), so it is true for a
  block-relay-only peer that serves blocks. So block requests, stalling and timeouts apply as for
  `outbound-full-relay`.
- `MaybeSetPeerAsAnnouncingHeaderAndIDs` does not test the connection type, so a block-relay-only
  peer can be an HB peer.
- No tx relay (`m_tx_relay` stays without relay, np:446; `RejectIncomingTxs` np:6279). The compact
  reconstruction does not depend on the source, so this changes nothing for P3.
- Eviction uses `m_last_block_time`, which P3's tail sets (np:3722; read by eviction at np:6023-6044, P2a-core).
- `sendcmpct(false, 1)` at VERACK goes to every `CanRelay` peer, block-relay-only included
  (np:4214-4222).

P3 therefore adds no type-specific code. `sim` scenario S2-g checks that a block-relay-only outbound
peer is asked for blocks, can stall and be disconnected, and gets `sendcmpct(true)` (D8).

### 3.10 R4 quirks preserved

| # | Quirk | Core |
|---|---|---|
| Q1 | The getdata condition puts the IBD short-circuit after `sync && !limited`, so outside IBD every `CanServeBlocks && CanRelay` peer gets requests, limited peers included | np:6822 |
| Q2 | The staller is chosen only when this peer got **no** blocks this round and the waited-for peer is not itself | np:1542-1545 |
| Q3 | Doubling and decay CAS on an atomic, not under `cs_main`. The decay truncates seconds | np:6762-6765, np:2200-2207 |
| Q4 | `lNodesAnnouncingHeaderAndIDs` keeps finalized ids. They count toward 3; a stale front is popped silently | np:1376-1415, absent from `FinalizeNode` np:1783-1845 |
| Q5 | `mapBlockSource.emplace` never overwrites the first source of a hash | np:5383, np:3804, np:5271 |
| Q6 | Reserved (D12) | — |
| Q7 | `blocktxn` `Failed` on the first in-flight sends `getdata(BLOCK)` and keeps the in-flight entry with a null header, so the next `blocktxn` scores 100 | np:3780-3794, np:3765-3773 |
| Q8 | `FillBlock`'s short-missing `Invalid` returns before `header.SetNull()` | be:196-208 |
| Q9 | `NewPoWValidBlock` runs on V inside `AcceptBlock`, as in Core, but its visit of a peer can wait for the actor's region for that peer, after it has pushed to the peers before it in the table; Core's `AcceptBlock` instead waits for `cs_main` before it starts. A region is a whole handler where Core may take `cs_main` in pieces. Per connection the frames and their order are Core's; across connections a later peer's region frames can trail an earlier peer's `cmpctblock` by one region. `BlockChecked` is applied at the actor's next drain after its hold's step publishes, the rest of that step plus one doorbell wake after Core's inline call. An accept that finds a `BlockChecked` outstanding waits for the actor's next drain (C5′). A region's `Ahead` retries are unbounded: back-to-back tip-extending accepts can keep a peer retrying without a successful region, so progress assumes a gap between accepts; Core's `cs_main` is not a fair lock either and gives no bound (D59). Never reordered against section 7 or `BlockChecked` (D34, D37, D44, D46, D50, D55, D56, D58); measured by §6.3's probe (D52) | np:2228, np:2249, np:2314, v:3432, v:4832; §3.6 |
| Q10 | `mapBlocksInFlight.size()` in `HeadersDirectFetchBlocks` and `BlockChecked` is the entry count of a multimap | np:3251, np:2336 |
| Q11 | The block-download timeout is compared in floating point | np:6776 |
| Q12 | `NewPoWValidBlock`'s nonce draw happens before the height check | np:2229, np:2234 |
| Q13 | `cmpctblock` with unknown `prev` during IBD is ignored silently (no getheaders, no score) | np:5069-5075 |

### 3.11 Log and misbehaviour strings

Misbehaviour reasons, which `getpeerinfo` and the debug log show:

- `"mutated block"` (100)
- `"invalid compact block"` (100)
- `"previous compact block reconstruction attempt failed"` (100)
- `"invalid compact block/non-matching block transactions"` (100)
- the `MaybePunishNodeForBlock` messages with `"invalid header via cmpctblock"` (P2a-core's table,
  np:1969-2020).

Log lines, all copied byte for byte from the cited Core lines and checked by the transcript tests
where asserted:

- `NET`: `"received block …"`, `"Received mutated block …"`, `"Requesting block %s (%d) peer=%d"`,
  `"Stall started peer=%d"`, `"Increased stalling timeout temporarily to %d seconds"`,
  `"Decreased stalling timeout to %d seconds"`, `"Peer sent us compact block we were already syncing!"`,
  `"Peer %d sent us block transactions for block we weren't expecting"`,
  `"Peer %d sent compact block transactions multiple times"`, the two `"Unexpected … message"` lines,
  `"Large reorg, won't direct fetch …"`, `"Downloading blocks toward …"`, `"%s sending header-and-ids …"`.
- `VALIDATION`: `"Block mutated: %s"` (v:4434-4440 `IsBlockMutated`, the state as
  `ValidationState::ToString`), logged between the `NET` `"received block …"` and
  `"Received mutated block …"` lines (np:5348-5366; S1 D74–D76).
- `CMPCTBLOCK`: be:172, be:216-221.
- Unconditional (`LogPrintf`/`LogInfo`): `"Peer=%d%s is stalling block download, disconnecting"`,
  `"Timeout downloading block %s from peer=%d%s, disconnecting"`, `"Saw new cmpctblock header hash=%s peer=%d"`.

`p2p_ibd_stalling` asserts `"Stall started peer=%d"` (debug), `"Decreased stalling timeout to 2 seconds"`
and the disconnects through `assert_debug_log`. ND1's asserted-log list (O11) gets these strings.

---

## 4. Persistence and crash

P3 keeps no persistent state. Core keeps every structure in §2.2 in memory, and they reset on
restart (`CNodeState` is per connection; `m_block_stalling_timeout` starts at 2 s, np:982).

Crash safety of blocks is V's (CH1-node/CH2):

- a block accepted before a crash is in the block files and the index, with `BLOCK_HAVE_DATA`, so
  after restart `FindNextBlocksToDownload` skips it;
- a block in flight but not received is simply requested again.

A crash between V's accept and P3's tail loses only `m_last_block_time` and in-flight entries,
which are per-connection and gone anyway.

One thing to check: CH2's "files durable before index" ordering means a `have_data` entry after
restart always has its bytes (CH2 §3.5). That is V's, not P3's. The restart IBD case is in §6.7 (b).

---

## 5. Performance

### 5.1 Budget and metrics

IBD from the network is network-bound plus V-bound. P3's own cost per block has four parts:

- the decode, which is pre-hashed and pool-parallel per A1 §3.2 and not the actor's;
- the mutation check: one merkle pass. Core does not repeat it: `IsBlockMutated` sets
  `m_checked_merkle_root` (v:4326, v:4362), and `CheckBlock` then skips the merkle check. The port
  carries the same fact to V in `BlockSubmit::merkle_checked` (§2.4, A-4b). Until V honours it, V
  repeats the pass. The second pass counts against the budget below, and it is a measured gate
  (D24);
- a few hash-map operations;
- the getdata batch.

**Budget: one counter on both sides (D30, D35).** Wall time on this box is noise, so every gate
counts the PMU event `instructions:u` on both sides, over the same workload boundary.

- **The workload.** One block-receive transcript, replayed into a process on each side: the
  node leg (the sim node, or real V at S6) and the Core leg (the O1c oracle session, with Core's
  real `PeerManager` and chainstate). Blocks come from regtest, with mainnet-like transaction
  counts: those of mainnet heights 2,000,000–2,010,000, read once from the mainnet reference
  oracle (`getblock`, a pure read, concurrency 1) and checked in. The node is out of IBD, and the
  transcript has one high-bandwidth compact peer plus plain peers, so `NewPoWValidBlock` runs.
- **The boundary.** Each leg runs two passes over the same blocks from the same starting state:
  - **A:** every block delivered as a `block` message through the P2P path, with the P3 listener
    registered (Core: `PeerManager` registered);
  - **B:** the same blocks submitted straight to the chain, with no listener (node:
    `ChainCommand::ProcessBlock`; Core: `ChainstateManager::ProcessNewBlock` through new entry
    point E6, with `PeerManager` unregistered from validation signals).

  P3's share per block is (A − B) / blocks. Both passes count `instructions:u` process-wide, user
  space only, over the replay loop and nothing else. The harness opens one counter per thread of
  the process (`perf_event_open`, `exclude_kernel`, reading every thread in `/proc/self/task`
  before and after), and the code is the same on both legs. Process-wide counting covers
  `NewPoWValidBlock` on both sides. Core's synchronous callback runs inside A and not B. The
  node's runs on V's thread, likewise inside A and not B (D50).
- **S1 gate (sim node).** The node's A − B ≤ the Core leg's A − B, per block. The Core leg needs
  E1 and E6 (A-5). Until they land, S1 reports the node's A − B and names the comparison as a
  PENDING row, as D21 does for the differentials. S6's real-V comparison is PENDING in the same
  way, and S6 is READY with that row named. So S1, S2 and S6 land first (R10). P3's row stays
  open, late-closing, until E1 and E6 land and both comparisons pass (D48).
- **S6 gate (real V).** The same comparison with real V in the node leg. Testnet blocks up to
  6,500 are almost all coinbase-only, so the testnet run reports its numbers and gates nothing.
- **The merkle-pass gate (real V, kept).** With real V, the node leg runs B twice: once with
  `merkle_checked` honoured, once ignored. The difference is V's second pass. S6 is READY only if
  V honours `merkle_checked` (A-4b), or that difference is ≤ 2% of the Core leg's B per block.
  Both are `instructions:u` on the same blocks.
- **Attribution only.** callgrind `Ir`, on the oracle session built from rc.3 with symbols, splits
  Core's share by function (`PeerManagerImpl::ProcessMessage`, `NewPoWValidBlock`,
  `ChainstateManager::ProcessNewBlock`). It is labelled "attribution (callgrind Ir)", appears
  only beside a gate result, and is never itself a gate.

**Memory.** P3's worst-case resident state:

- in-flight entries: at most 16 per peer (`MAX_BLOCKS_IN_TRANSIT_PER_PEER`, np:152), each with at
  most one `PartiallyDownloadedBlock`, and at most 3 per block (`MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK`,
  nph:49);
- the extra ring: `-blockreconstructionextratxn` transactions (100 by default, nph:43), empty in
  M3;
- `Shared::recent`: one block and its compact encoding;
- the signal queue, at §2.5's maximum: 1,024 entries and 8 MiB, plus Q entries, plus one outer
  ABC iteration's `BlockChecked` entries (the startup ABC's during the hold);
- each peer's header cell: four fields, moved out of `PeerState` (D50), and one shared
  `announce_stamp` (D56);
- each peer's `block_inv` cell: one `RelayList` of at most 8 hashes, however many
  `UpdatedBlockTip` callbacks append to it, freed with the peer's state (DEC-236, D44).

Space is released per processed entry, so the batch the actor is working through is
  inside the bound, not on top of it (m-3). In normal operation the queue holds one or two
  entries.

Core has no queue term, because it holds no backlog. ND1's memory accounting adds it with this
maximum (A-9).

Metrics, exported through ND1's counters when it has them:

- `p3_blocks_requested`, `p3_blocks_received{via=block|cmpct|blocktxn}`;
- `p3_stalls`, `p3_block_timeouts`, `p3_process_block_wait_us` (histogram);
- `p3_cmpct_reconstructed{from=prefilled|pool|extra|blocktxn}`, `p3_getblocktxn_indexes`.

### 5.2 Route to 2×

The 2× target on IBD (PRD NFR, A1 §3.2) cannot come from P3. Core's 16-per-peer, 1024-window
scheduler is kept for fingerprint parity (A1 line 94), and the actor waits for each block's ABC as
`msghand` does. The levers are all V-side: S1's within-block parallel prevalidation, S2's coins
prefetch, and the script pool.

P3's job is to add no serial work to the actor:

- decoding runs off the actor (A1 §2.14: the reader pre-hashes the header; per-transaction work
  runs on the pool during V's accept);
- `is_block_mutated`'s merkle pass is the one serial hash pass P3 adds. It reuses the transaction
  hashes the decode cached, so it costs `n` SHA256d on 64-byte nodes;
- `ShortIdIndex` is O(n) with no rehash.

### 5.3 PF0

**`perfgate bench_inputs`.** `dashd-p2p` enters the `rdashd` closure only in S6, when `rdashd` links
it. S1–S5 change no bench input. S6 changes `rdashd`'s closure: expect "bench CHANGED" for the rdashd
rows, and PF0 re-anchors (memory pf0-reanchor-after-bench-change-merge).

**Workloads.** P3 adds no PF0 workload. `ibd-testnet-seed` is a window row (§6.8). The counter
check P3 runs itself is the sim replay of 2,000 blocks under `perf stat -e instructions:u`
(`benches/` style, not a gate), compared before and after each slice.

---

## 6. Verification

### 6.1 L0 (units, in `download.rs` and `compact.rs`)

- **In-flight accounting.** `block_requested`/`remove_block_request` keep `peers_downloading_from`,
  `downloading_since` (set on 0→1; advanced only when the front completes) and `stalling_since`
  (reset on any removal) as Core does. There is a proptest against a direct transliteration model
  over random request, remove and finalize sequences.
- **`find_next_blocks_to_download`** over hand-built trees: window edge at 1024/1025, staller choice,
  `have_txs_downloaded` advancing `last_common`, `!VALID_TREE` stop, chunking with `max(count−n, 128)`.
- **Stalling.** Doubling to 64; the decay step for every t in 2..=64 against Core's
  `duration_cast` result, because doubling from decayed values reaches values off the 64 chain,
  such as 27→54 and 5→10 (D33); CAS losers.
- **Block timeout.** The float compare at the boundary (`others = 0, 1, 7`).
- **HB list.** Every branch of np:1360-1416, including the stale-front pop (Q4) and the
  inbound/outbound swap.
- **`PartiallyDownloadedBlock`.** Every `ReadStatus` path of be:55-223, the bucket > 12 rule at the
  exact insertion (a crafted short-ID set that collides mod the libstdc++ bucket count), duplicate
  short IDs, mempool duplicate reset, extra-ring hash compare, and both
  `Invalid` orders (Q8).
- **Encoder.** `from_block` against Core-produced `cmpctblock` bytes for fixed nonces, from the
  oracle (§6.4) or dashd fixtures.
- **Extra ring, by direct injection (S3, D19).** `add` with `max_extra_txn` ≤ 0 (a no-op), 1 and
  100; once full, each add replaces the oldest entry; `init_data` fills a slot from the ring by
  short ID. The second-match test at be:157-162 runs with real transactions: two with different
  txids and one short ID (the slot is cleared, and `mempool_count` and `extra_count` both drop),
  and the same transaction in the mempool and the ring (nothing changes). It compares `GetHash()`,
  the txid, not the instance hash the short ID uses.
- **Bucket table (S3, D22).** The exhaustive sweep for n = 0..=65,535 and the transition sets of
  §2.3.2.
- **Signal queue (S1, D20, D26, D29).** A test hook stalls the actor before its drains. Pushes
  then exceed both bounds. A thread
  standing in for V parks in `wait_for_space` at a release point and services a `ProcessHeaders`
  the stalled actor sends meanwhile. The actor's wait completes, it drains, and processing the
  entries wakes V. Entries come out in push order, and `close` releases a parked V. Then the same
  with the actor stalled in each wait of §3.8.1's table, and these cases:
  - **Hold plus full queue.** V is in the startup hold (the shape of CH1-node's
    `commands_wait_for_startups_abc_after_a_tied_fallback`), connects more than 1,024 blocks, and
    has a `ProcessHeaders` held. V never waits, the overshoot equals the connect count, and after
    the hold the actor's command is served and the queue drains.
  - **Pre-start.** The listener is registered before the actor starts. V runs a startup ABC and a
    tip-extending import past the bound, with IBD latched false, and never waits. `recent` and
    `highest_fast_announce` equal those of a run with the actor live, and the first drain switches
    the queue to `Live`.
  - **N queued RPC `ProcessBlock`s.** N = 1, 4 and 16 submitter threads each send a sibling block
    at tip+1 while V waits for space. The queue's peak stays within §2.5's maximum for Q = N + 1,
    and every sender gets its reply.
  - **No nesting.** An accept served inside a `wait_for_space` wait reaches its C5 point and
    returns at once.
  - **Per-entry release.** V stays parked after `drain` until processed entries bring the cost
    within the bound.
  - **10,000 marks (D44).** With the actor stalled, `SimChain` runs an `EnforceBlock` that marks
    10,000 competing siblings, each emitting `UpdatedBlockTip(tip, nullptr)`, with `BlockChecked`
    entries interleaved. **Sol r5's schedule:** V waits in `limit_queue`, and one RPC sender
    completes a class M `ProcessHeaders` scope between every two callbacks. Then:
    - the signals thread never waits, and V leaves `limit_queue` as the callbacks run;
    - the queue holds only V's entries, within §2.5's maximum;
    - each peer's cell holds one `RelayList` of at most 8 hashes.

    Peers complete `VERACK`, are marked for disconnect, and are finalized between callbacks. A
    finalized peer's cell is gone, and no append reaches it. After the actor resumes, every
    peer's section 7 matches a model that appends Core's raw vectors in Core's visit order.
  - **The cell and S (DEC-236, D44).** T's accept has run, with a seam standing for its visit,
    first unpublished and then published. Barriers place U(T)'s append to a peer's cell:
    - (i) before the head locks the cell: S shows T, the seam's stamp and header-cell write are
      visible to the head, then section 7 uses the list;
    - (ii) while the head holds the lock: the append waits on the leaf lock and lands in the
      next round;
    - (iii) after the unlock and before section 7: the next round.

    Each peer's frames match Core's schedule with the append before or after section 7's
    `m_block_inv_mutex`. **Per-visit recipients:** a peer that completes `VERACK` while the
    callback is between two visits gets the append exactly when it comes after the callback's
    position in the table, for both table orders. This is the Sol r4 and D43 schedules, run
    under the cell.
  - **Two cells (m-1, D47).** `SimChain` stores SB4's `TipCell`, then the `ChainView`, with a
    hook between them. A drain in that window takes its snapshot and its cutoff from the
    `ChainView`, so it neither shows the hold's new tip nor applies that hold's `BlockChecked`; a
    drain after both stores does both. A source P's `getheaders` handled in the window is
    answered from the old tip. No drain point calls `snapshot()`, which the test asserts through
    a counting `ChainAccess`.
  - **Ring after publication (M1, D45).** `wait_for_space` with a `Live`, non-empty queue within
    its bounds rings the doorbell, calls `wake()` and returns at once; with an empty queue it
    does neither. An actor in its block wait with a `BlockChecked` queued and published wakes on
    the ring alone. An idle actor under a mock clock that never advances, with a `BlockChecked`
    pushed (its own `wake()` drained it while ineligible) and then published, applies it on the
    C5 point's `wake()`, with no timeout.
  - **Header cell (D50, D56, D57).** `lock_headers(seq)` returns `Ahead` exactly when
    `announce_stamp > seq` at its stamp read, after the lock and the head's copy. At each region
    start (a message, a `Redrain` relock, phase 3, the head of `send_messages`, and `getpeerinfo`),
    a seam stores a stamp the region's snapshot does not show. The region releases `PeerState` and
    the cell, calls `wait_published`, drains, and runs on the new snapshot with nothing of it run
    twice; `wait_published` returns at once when the publication has already happened. The head
    keeps its moved list across the retry, and its copy of `recent` is the one from its successful
    attempt. A coverage seam pauses the head between its cell lock and its stamp read while a
    stand-in for V stores a stamp and sets `recent` and then waits on the cell; the head returns
    `Ahead` and never runs a section with that copy. rev10's order passes this seam too; S4-x (10)'s
    after-read case is the regression test for Sol r10 F1 (D58, D59). A thread standing in for V
    visits peers while the actor holds one peer's cell: it pushes to the earlier peers first, waits
    on that cell only, and resumes when the region ends. A visit that only reads a cell, followed by
    that peer's region, makes the region wait (Opus r10 M2). A peer marked for disconnect while the
    visit waits for its cell gets no `cmpctblock`. A test that sends to V while it holds a
    header-cell guard panics on the thread-local assertion.
  - **C5′ (D55).** With `BlockChecked(B)` queued and the actor in a region, an RPC accept waits,
    deferred inside a space wait as well, until the actor has processed the entry; in IBD it does
    not wait; `PreStart` and a closed queue end the wait.
  - **Message snapshots and round trips (I0, D46).** A publication lands between a drain and a
    message's start. A recording seam shows every entry stamped ≤ the message's `view_seq`
    applied before the handler runs. The same holds after a `ProcessHeaders` reply in the
    `headers` handler and in `cmpctblock`'s step 5: the continuation reads the post-reply drain's
    snapshot, never the reply's.
    - **Headers racing an RPC block (D46, D51).** The node is out of IBD with tip A, and
      outbound Q's best-known block is A. RPC `submitblock B` lands relative to Q's
      message-start drain, V serving Q's `ProcessHeaders(B)`, and the continuation's lock of Q's
      header cell after its post-reply redrain. The variants split on whether B's
      `NewPoWValidBlock` call begins before that lock reads the stamp:
      - it begins before, and B is connected by the time the continuation runs: the continuation
        sees tip B with B's entries applied first, and `m_last_block_announcement` stays unset;
      - it begins before, and B is accepted but not yet connected (V serving at ABC's first
        release point, `abc.rs:730`): the continuation sees B stored and tip A, and the
        announcement time is set;
      - in both, if B publishes after the redrain's snapshot, the lock returns `Ahead` and the
        continuation runs on the snapshot it waited for;
      - it begins after: the announcement time is set from X′ without B, and the continuation
        makes B Q's best-known block.

      Each is a Core schedule with B's accept, or its connect, in or after the gap.

    S2 adds direct fetch racing another origin's `BlockChecked` (S2-h). S4 adds the HB
    `cmpctblock(B)` to Q and `cmpctblock`'s steps 7–13 (S4-x (7)).
  - **Cutoff (D37).** `drain(cutoff)` takes exactly the queued prefix stamped ≤ cutoff, in order.
    A later-stamped entry pushed after the snapshot stays queued, and the next drain after V
    publishes applies it. V parked at each C5 point is released by the actor's next drain.
- **`RelayList` (S1, D44).** A proptest over random append sequences, and random chain states and
  peer states, against a direct port of section 7 over the uncompressed vector: same frames, same
  `best_header_sent`, same inv queue.
- **`merkle_checked` (S1, D24).** Every `block`, `cmpctblock` and `blocktxn` submit that ran
  `is_block_mutated` sets it, and `SimChain` skips its merkle pass when it is set.

### 6.2 L1: `sim` scenarios (P3's M3 acceptance for the FTs)

`dashd-p2p`'s `sim` harness (P2a §6.7, `src/sim/`) gets block support:

- `SimChain::process_block`, a port of the parts of `ProcessNewBlock`, `AcceptBlock` (v:4759-4860)
  and ABC's step (v:3634-3670) that P3 observes:
  - `CheckBlock` reduced to the merkle and mutation checks; a failure emits `BlockChecked` (v:4891),
    with `new_block = false`;
  - `AcceptBlock`'s early returns, with no signal and `new_block = false`: header unknown,
    `fAlreadyHave` (v:4804), unrequested with `nTx != 0`, less work, too far ahead or below minimum
    chain work (v:4805-4814);
  - `*fNewBlock = true` at v:4835, then a stored block, even out of order (blocks 2–1024 of
    `p2p_ibd_stalling` while block 1 is withheld);
  - `nChainTx` propagation when a gap fills, which drives `HaveTxsDownloaded` and `last_common`;
  - `NewPoWValidBlock` only under v:4831's guard (C4), called inline inside the accept with a
    `TreeRead` over `SimChain`'s in-scope tree and the stamp (C8);
  - connecting in Core's steps (batches of up to 32, a step returning once it exceeds the old
    tip's work, v:3634, v:3665-3667), with one `BlockChecked` per connected block (v:3432) and a
    queued `BlockConnected` per block delivered to `Shared::block_connected` (the decay);
  - every synchronous signal before the reply (C1);
  - `merkle_checked` honoured, as A-4b asks of V, so that S1 can measure both passes;
  - C5's release points: `wait_for_space` at each outer iteration's start and before each
    command, with the startup hold, `PreStart`, the no-nesting rule and the ring (D45);
  - publication in CH1-node's order: SB4's `TipCell`, then the `ChainView`, with a test hook
    between them (D47);
  - test pauses inside the accept right after the `NewPoWValidBlock` call and before the write,
    and between an accept and its ABC's first `ConnectTip` (D45, D50).
- `SimChain::status`, `in_active_chain`, `active_at` and `last_common_ancestor`.
- A `SimPeer` that answers `getdata` with `block`/`cmpctblock`, withholds chosen blocks, and
  answers `getblocktxn`.

The FT ports are step-for-step translations of the Python, with the same constants, mocktime steps
and assertions:

| Scenario | Ports | Checks |
|---|---|---|
| S2-a `ibd_stalling` | `p2p_ibd_stalling.py` whole (1025 blocks; 4 outbound stallers via the sim's `addconnection` analogue; mocktime +1, then +3, +3, +3, +2, +7, +2 s as the test steps it) | no disconnect while the window is full (Core :82-100); stall at 1025 (:102-117); doubled to 4 and 8 s (:119-143); decay back to 2 s after the withheld block, with the log line (:145-150); all blocks connect (:151-152); bytes per `block` message from the sim's `bytesrecv_per_msg` analogue |
| S2-b `block_sync` | `p2p_block_sync.py` (node0→node1→node2, 3 sim nodes, node2 in IBD syncing from an inbound peer only) | node1 and node2 reach node0's tip; the getdata condition's `sync && !limited` branch with an inbound sync peer |
| S2-c timeouts | not an FT: block download timeout with N = 0, 1, 2 other downloading peers | disconnect at `spacing·(1+0.5N)` + 1 µs, not before |
| S2-d in-flight cap | 16 per peer, 1024 window, `getdata` batching with objects | one `getdata` per pass per peer; block entries before object entries |
| S2-e direct fetch | headers within 20·spacing: the `MSG_CMPCT_BLOCK` swap for one block with one in flight. Also with IBD true (tip recent by mocktime, chain work below `nMinimumChainWork`): the swap still happens, and with `NoMempool` the `cmpctblock` gets a `getblocktxn` for every non-prefilled index (completed in S3) | np:3247-3255, np:1428-1431, v:1984 |
| S2-f D65 | invalid block from peer A, not new (already stored invalid): A is punished before the tail erases; valid block makes A HB when nothing else is in flight; a block connected later by another peer's block credits its original source | §3.7 |
| S2-g block-relay-only | §3.9 | §3.9 |
| S2-h direct fetch racing `BlockChecked` (M2, D46) | not an FT. B, stored from peer R, is connected by another origin's ABC between Q's message-start drain and V serving Q's `ProcessHeaders` for B's descendant C | `BlockChecked(B)` is applied before `headers_direct_fetch` puts C in flight, so at that `BlockChecked` the in-flight map is as Core's (`count(hash) == size()` reads Core's values) and direct fetch walks from X′'s tip B |
| S3-a… | `p2p_compactblocks` client subtests: `getblocktxn_requests` (coinbase-only blocks; the mempool variants are late), `incorrect_blocktxn_response`, `reconstruction_stalling_peer`, `reconstruction_parallel_reconstruction`, `invalid_tx_in_compactblock`, `invalid_cmpctblock_message`, `multiple_blocktxn_response` | per subtest, the Python's assertions |
| S3-r `cmpctblock` racing a connect (M2, D46) | not an FT. Another origin connects T between Q's message-start drain and V serving `cmpctblock`'s step 5 | steps 7–13 read X′: step 7's and step 10's work comparisons, step 11's `can_direct_fetch` and step 12's `tip.height + 2` use T, with T's `BlockChecked` applied first, as a model running Core's second section after T's connect |
| S4-a… | `highbandwidth_mode_states_via_getpeerinfo`, `sendcmpct` (client and announcement parts), `end_to_end_block_relay` (announcement via `NewPoWValidBlock`) | HB list states, `bip152_hb_to/from` in stats, cmpctblock announcement to HB peers |
| S4-x ordering matrix (F1, D34, D37, D44, D46, D50, D51, D55, D56, D58) | not an FT; every case with an HB compact peer, crossed with each header preference: `sendheaders`, `sendheaders2`, neither. **(1) Stored descendant (Sol r2).** A valid descendant D of B is stored, and B arrives by RPC. B's visit of the peer runs before the head takes the header cell, or waits for the head's region; B's publication lands before S or after it; and U appends before the head locks the `block_inv` cell, while it holds it, or after it unlocks. **(2) Reorg crossing (Sol r3).** Tip A, with A's append in the cell. The peer knows the fork parent and branch B, not A. The head takes S and the header cell, then V reorganizes to B and accepts C, whose visit waits for the head; and the variants where C's visit takes the cell before the head, with C's publication before S or after it (`Ahead`). **(3) P2P block.** A peer's `block` is accepted and connected while the actor waits for the reply. **(4) Late full connection.** A peer completes `VERACK` and a high-bandwidth `sendcmpct` before V's visit reaches it, while the visit waits on its cell, or after the visit has passed it, for both table orders. **(5) A relay racing a full connection (Sol r4).** V has published A; in B's hold it pushes `BlockChecked(B)`. U(A)'s callback runs while peer P, which knows A's parent but not A, completes `VERACK` and Q's head takes S showing A; V then publishes B, and U(B) appends before or after Q's next head. Variants: U(A) visiting P before and after P's `VERACK`, and reaching Q before and after Q's head, for both table orders. **(6) Accept then connect (F1).** `SimChain` pauses right after the `NewPoWValidBlock` call, before the write, and again before ABC's first `ConnectTip`, for a P2P block and an RPC one, with the actor idle, in its block wait, or inside another peer's message. **(7) Headers racing an RPC block (M2, F2).** Outbound HB peer Q at best-known A sends `headers(B)`, and in a variant `cmpctblock(B)`, while RPC `submitblock B` is accepted, and then connected or not, with B's `NewPoWValidBlock` call beginning: between Q's message-start drain and V serving Q's `ProcessHeaders`; after the reply and before the continuation locks Q's header cell, with B's publication before the redrain's snapshot or after it; or after that lock. **(8) `BlockChecked` before a later accept (Opus r10 M1).** A stored B with source P, an HB-requesting peer, is connected by another origin (`reconsiderblock`; ChainLock enforcement), and `BlockChecked(B)` is queued; RPC `submitblock D` on B reaches V before the actor drains, with the actor idle, in a region, or in its block wait. **(9) A read-only visit (Opus r10 M2).** Tip B; HB peer P at best-known A. RPC accepts C; P's `inv(B, X)` region, `getheaders` handling with few unconnecting headers, and a stale-tip section 6 each run before or after the visit; and a section 7 for T whose region holds P's cell when C's call begins. **(10) A call between the cell and the cache copy (Sol r10 F1).** Tip T is `recent`; HB peer P knows A, T's parent, has not started sync, and has U(T) pending. P's head pauses after it locks P's cell, both before and after its stamp read, while RPC accepts C on T, which stores its stamp, sets `recent = C`, and waits on P's cell. In the after-read case a barrier holds P after its stamp read, at the point where rev10 copied `recent`, until V has finished writing `recent = C` and is waiting on P's cell (the regression case, D59) | (1) headers preferred: `cmpctblock(B)` then `cmpctblock(D)`; neither: `cmpctblock(B)` then `inv(D)`; never `headers(B,D)`, `headers2(B,D)`, or `inv(D)` before `cmpctblock(B)`. (2) `cmpctblock(A)` then `cmpctblock(C)`, with `best_header_sent` A then C; in the variants, `cmpctblock(C)`, then section 7's inv fallback for A per Core; never `cmpctblock(C)` then `cmpctblock(A)`, and `best_header_sent` never moves back. (3) each HB peer holds `cmpctblock(B)` before V writes B, and before that pass's section 7 announces it. (4) the peer gets `cmpctblock(B)` exactly when its `VERACK` and `sendcmpct(1)` were handled before V's visit read it, as with Core's `ForEachNode` (D50). (5) Q gets A at S exactly when U(A) appended to Q's cell before Q's head, else with B in its next round; P gets A exactly when U(A) visited P after its `VERACK`; Q nothing at S together with P `inv(B)` alone only when P precedes Q in the table, as in Core (D44). (6) at the first pause, every HB peer whose header tests pass already holds `cmpctblock(B)`; the call waited for any region in progress. (7) the call begins before the continuation's lock and B is connected when it runs: Q gets `cmpctblock(B)` and no announcement time; accepted, not connected: Q gets `cmpctblock(B)` and the announcement time is set; B published after the redrain's snapshot: `Ahead`, then one of those two; the call begins after the lock: the announcement time is set first, and B's visit skips Q; never neither; steps 7–13 of the `cmpctblock` variant read X′. (8) P gets `sendcmpct(1)` (and the evicted HB peer `sendcmpct(0)`) before `cmpctblock(D)`; never after. (9) after the visit: the region runs on a snapshot showing C (a `getheaders` locator from C, no `cmpctblock(C)`); before it: the region's frames (locator from B), then `cmpctblock(C)`; never the locator from B without `cmpctblock(C)`; section 7 sends T's cached compact block with no draw, then `cmpctblock(C)`. (10) C's stamp after the head's read: a locator from A, T's cached compact block with no nonce draw, then `cmpctblock(C)`; before it: `Ahead`, then a locator from T, `cmpctblock(T)` encoded from disk with one draw, and no `cmpctblock(C)`; the locator, the draw log and C's announcement match Core's for the same order; never a locator from A with a fresh encoding of T. The after-read case fails on rev10's order, giving a locator from A, a fresh nonce for T, then `cmpctblock(C)`. Each case also runs in `diff_p2p_multi_blocks` with Core's frames as the expectation |

Each scenario's file names its Python source and the Core lines it ports, as P2a-core's `sim_sync`
does.

### 6.3 Transcripts against dashd

P2a-core's capture tool (`contrib/node/p2a-transcripts/capture.py`) drives a regtest dashd v24 with
scripted peers and records frames with timings. P3 extends it with six scenarios:

1. 1,100 regtest blocks, one peer, to record the `getdata` sizes and order;
2. the same with two peers and one withholding, to record `"Stall started"` and the disconnect at
   2 s;
3. a header announcement to show the `MSG_CMPCT_BLOCK` swap;
4. HB selection after a valid block outside IBD;
5. the bucket-table transition sets of §2.3.2, sent as `cmpctblock` to the release regtest `dashd`
   from a fresh peer each, recording `getdata(MSG_BLOCK)` or `getblocktxn`. The script refuses to
   run unless that binary's sha256 is
   `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`
   (`~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd`), and the fixture records it (D31);
6. **the accept → HB `cmpctblock` timing probe (F1, D52).** This is a live differential, not a
   replay. The same scripted peers run against that sha256-pinned `dashd` and against rdashd with
   real V, both on regtest with `-dip3params` putting DIP3 above the run's last height (rdashd's
   CH3 guard refuses DIP3, and regtest activates it at 432, chainparams.cpp:818, 1148) and
   DIP0001 active from height 1 (chainparams.cpp:817), so the 2 MB limit applies. Each cell starts
   from the same funded regtest chain. Peer H is a high-bandwidth compact peer at tip A. B arrives
   as a `block` from a source peer (P2P origin), or through `submitblock` (RPC origin). Per block,
   the probe records the time from B's last byte, or the RPC call, to the first byte of
   `cmpctblock(B)` at H, and from there to HB selection's `sendcmpct(1)` where one follows. It
   runs three loads: idle; a script-heavy block at the 2 MB limit; and disk pressure from a
   background `fsync` writer on the data volume. Each cell has 200 blocks. For Q9's
   cross-connection skew, a second HB peer H2 sits later in the table than a peer L that keeps the
   actor busy with `getheaders`, and the probe reports H2's delay behind H. A third run has
   another origin connect a block just before each accept, for C5′'s wait.
   - **Gate.** In every rdashd sample, the push of `cmpctblock(B)` to H's send queue precedes V's
     write of B and its first `ConnectTip`, from rdashd's trace points. The design guarantees
     this by construction (C8), so the gate is a regression guard.
   - **Reported, gating nothing in P3.** p50, p90 and p99 per cell on both sides, a two-sample
     Kolmogorov–Smirnov statistic per cell, the skew, and C5′'s waits. They go to NFR-FP-02's
     classifier, whose owner judges them. The `instructions:u` count of `new_pow_valid_block` is
     reported beside Core's `NewPoWValidBlock` from the O1c session's callgrind attribution
     (§5.1), as attribution only. The box's load makes wall time noisy, so the numbers come from a
     quiet host.

   S4 runs the gate against `SimChain`. S6 runs the probe with real V, and S4-v carries it if S4
   lands second (D48).

Fixtures 1–5 go into `dashd-p2p/tests/fixtures/`, and `tests/dashd_transcripts.rs` replays them
against the sim node, comparing frames and the relative timing at mocktime granularity.

### 6.4 Oracle differential (O1c sessions)

`diff_p2p_multi_blocks`: a new multi-peer session script that delivers `headers`, `block`,
`cmpctblock` and `blocktxn` built from a regtest chain the oracle mines. It compares, per step:

- frames (`getdata`, `getblocktxn`, `sendcmpct`, `cmpctblock` announcements);
- state (in-flight per peer, `m_stalling_since`, the stalling timeout, the HB list, misbehaviour);
- the draw log (the `NewPoWValidBlock` nonce, D60).

In Core's session leg the stalling-timeout decay runs on the scheduler thread (`BlockConnected` is
queued). So a step that compares `m_block_stalling_timeout` first syncs the validation queue,
`SyncWithValidationInterfaceQueue` (entry point E5 below).

**New oracle entry points (flagged).** The O1c session runs Core's `PeerManager` over a
`TestingSetup` chain. P3 needs:

- (E1) a session step that mines n regtest blocks on Core's side and returns their bytes, so both
  legs see the same blocks;
- (E2) the session's `ProcessNewBlock` connected to the real chainstate. It already is inside
  Core's leg; the node leg uses `SimChain`, whose block validity must agree with Core's for the
  script's blocks. The script only produces valid, mutated, bad-merkle and duplicate-tx blocks,
  which `SimChain` judges like Core.
- (E3) the in-flight state, `mapBlockSource`, `lNodesAnnouncingHeaderAndIDs` and
  `m_block_stalling_timeout` exported in the session's state dump.
- (E5) a session step that runs `SyncWithValidationInterfaceQueue` before a state comparison.
- (E6) a session step that submits a block straight to `ChainstateManager::ProcessNewBlock` with
  the session's `PeerManager` unregistered from validation signals. It is §5.1's B pass; it gates
  S1's instruction budget, not S3/S4 READY (D35).

These are asks to the O1c owner. The O-row owner adds the ABI; P3 consumes it. The script's
`targets.toml` entry is owned by `dashd-p2p`, as D58 does for the other multi targets.

`diff_cmpct_initdata`: a new single-call differential fuzz target, oracle entry point (E4)
`oracle_partially_downloaded_block(cmpct, pool_txs, extra_txs, blocktxn) → (status, block_bytes)`.
It drives Core's `PartiallyDownloadedBlock::InitData`/`FillBlock` with a mock pool that has a
`vTxHashes` order. Compared fields: status and block bytes. This is where the libstdc++ bucket rule
is held to Core's real `unordered_map` (D7).

**Toolchain (D21, D22).** The oracle must behave like the release's libstdc++: GCC 13.3.0, linked
statically like the release `dashd` (§2.3.2). If S3's sweep and transition fixtures show that this
box's GCC 15 agrees with the release, a GCC 15 oracle is acceptable. If they show a difference, the
oracle is built with GCC 13.3.0 in a pinned toolchain container. The GCC 15 probe never stands in for
the release.

**Required, not optional (D21).** `diff_cmpct_initdata` and the compact steps of
`diff_p2p_multi_blocks` (`cmpctblock`, `blocktxn` and `getblocktxn` frames, the HB list and
announcements) are part of S3's and S4's acceptance. Neither slice reports READY until both run
green at their floors (§6.5), and they cannot be parked to get there. The manager takes E1–E5 to the
oracle owners as a PLAN delta (A-5). The block-download steps of `diff_p2p_multi_blocks` gate nothing
before their entry points land: S2 is READY on its sim ports and transcripts, with that row PENDING.

**PENDING rows (manager ruling on OQ3).** Until its entry point lands, a target is registered in
`node-fuzz/targets.toml` with `smoke = 0`, `nightly = 0` and the first comment line
`# PENDING (P3 D21): needs oracle E<n> (A-5)`, as P2a-core D62's parked rows are. Every READY report
names each PENDING row and the entry point it waits for. Nothing is skipped silently.

### 6.5 Fuzz

- `cmpct_reconstruct` (single, no oracle): arbitrary `cmpctblock`/`blocktxn` sequences against
  `PartiallyDownloadedBlock` with invariants: no panic, `Ok` ⇒ merkle root matches, `Invalid` ⇒ a
  score of 100 when the handler applies.
- `block_download_model`: the §6.1 proptest model as a libFuzzer target.
- **Floors (D31).** Every new target (`cmpct_reconstruct`, `block_download_model`,
  `diff_cmpct_initdata`, `diff_p2p_multi_blocks`) has:
  - DEC-184's per-PR smoke: 5 minutes with 0 divergences and 0 crashes. This is what "green at
    its floor" means for S3 and S4 READY;
  - a `min_certify_execs` in its `targets.toml` row, set by D58's method: about half the execs of
    a 3,600 CPU-s window at the rate measured when the row lands, with that rate recorded;
  - certification by plateau on O3's farm (DEC-184/186), not per PR.

  They are registered through O3's `targets.toml` with owner `dashd-p2p`.

### 6.6 Functional tests (Python, unmodified)

These run through O11's runner against rdashd once their dependencies land:

- `p2p_block_sync` needs P2b (inbound accept, so node1 can accept node0) and RP1a (`generate`).
  A1 Appendix D places it at M3.
- `p2p_ibd_stalling` needs P2a-net's `addconnection` (`open_one_try`), RP1a (`setmocktime`,
  `getblock`), `getpeerinfo`'s `bytesrecv_per_msg` (P2a-core stats plus SB6) and the
  `-dip3params=2000:2000` argument (regtest DIP3 at 2,000, so its 1,025 blocks stay pre-DIP3).
  A1 Appendix D places it at M5.
- `p2p_compactblocks` is one sequential `run_test` (`p2p_compactblocks.py:909-966`). P4's server
  subtests run between the client ones: `test_getblocktxn_handler` is third, and
  `test_compactblocks_not_at_tip` uses `getdata(MSG_CMPCT_BLOCK)`. Its blocks come from
  `getblocktemplate` (`build_block_on_tip`), and its MiniWallet subtests need the wallet-less tx
  RPCs plus MP1a and P5. PLAN has P4 depending on P3, so the file cannot pass while P3 is open. A1
  Appendix D places it at M5. The file closes in P4's row (manager ruling on OQ2).

The sim ports (§6.2) are P3's FT evidence, and P3 closes them. `p2p_block_sync` and
`p2p_ibd_stalling` close in P3's row when their dependencies land. `p2p_compactblocks` moves to P4's
row and closes there when the whole file passes, after P5, MP1a and `getblocktemplate` (RP-tier
mining RPCs) (manager ruling on OQ2, D18).

### 6.7 Testnet IBD from public peers

The harness is `contrib/node/p3-ibd/run.sh`, run under
`systemd-run --user --unit=p3-ibd-<run> --same-dir --collect`:

- datadir `/work/scratch/p3/ibd/<run>`, mode 0700;
- `rdashd -testnet -datadir=… -listen=0 -stopatheight=<H> -debug=net -logips=0 -dbcache=1024`,
  with `H = 6,500` by default (below);
- no `-connect`: P2a-net's DNS seeds, fixed seeds and addrman reach public testnet peers. The box
  allows outbound connections to the public internet. Inbound is impossible here, and IBD does not
  need it.

Pure reads against public peers: it never uses /work/chains or /work/bench, does not listen, and
runs one instance at a time.

Each run records:

- the height reached and the stop, parsed from debug.log's `UpdateTip` lines as node-bench's
  `stopped_endpoint` does, because RP1a (behind CH3b) is likely absent at S6;
- the block hash at H, compared with a value pinned in the harness. The pinned value is read once
  from the testnet reference dashd oracle (`getblockhash H`, a pure read, concurrency 1) and
  committed with the oracle's version;
- the count of `"Requesting block"` lines per peer, and stalls and timeouts, from the same log;
- `getpeerinfo` snapshots every 30 s only if the RPC exists;
- the wall time.

**Why H = 6,500.** CH3's readiness guard (CH3 §2.17, `GuardScope::Live`) refuses to connect a
testnet block this build cannot judge, so connecting block 7,000 (DIP0003) aborts the node until
CH7a. The guard fires at connect, so blocks downloaded beyond 7,000 are harmless. But
`-stopatheight` overshoots: ABC calls `StartShutdown` at H (v:3830) and checks shutdown only after
a step (v:3841), so every `ProcessNewBlock` that reaches ABC before the message thread stops
connects at least one more step. A step that extends the tip, including one that fills a gap from
the stored window, connects one block and returns (v:3665-3667). Only a reorg step connects more,
in batches of up to 32 (v:3634). node-bench records the overshoot as `overshoot_blocks`
(`workloads.py:340`), and a non-zero exit fails the run (`:336`). A 500-block margin is far more
than the few late `ProcessNewBlock` calls a stopping node handles. The pass condition is "stopped
with tip ≥ H, hash at H matches, exit 0".

Runs:

- **(a) M3 run, S6.** `H = 6,500`, with real chainstate. Pass: the condition above, and no
  misbehaviour score given to an honest peer. "Honest" is judged mechanically (D31). The harness
  keeps the bytes of each message that drew a score. A score whose reason is one of §3.11's P3
  strings fails the run unless the testnet reference oracle shows the message invalid: the block
  it carries or reconstructs, or the transactions it supplies, differ from the oracle's copy of
  that block (`getblock <hash> 0`, a pure read, concurrency 1), or the oracle lacks that block.
  Scores with other reasons are reported without failing.
- **(b) Restart run.** Kill (SIGKILL) at about 3,000, restart, finish to 6,500. This checks that
  in-flight state is rebuilt and that §4 holds.
- **(c) Full-tip run, late-closing after CH7a.** `H` = testnet tip at run time minus 100.

A download-only run past 7,000 is not possible: blocks beyond the window are not requested until
the connect point advances. The scheduler at heights above 7,000 is covered by the sim scenarios and
transcripts instead.

### 6.8 BG row `ibd-testnet-seed`

node-bench's workload (`node-bench/nodebench/workloads.py:366-376`) runs the node with
`-connect=<4 local seeds>`, `-stopatheight`, `-dbcache`, `-par` and `-txindex=0` against local
dashd seeds cloned from /work/bench data, and checks the endpoint hash. It is a manager-run window
row. P3 never runs it, because it touches /work/bench.

P3's job is that rdashd accepts that argv and reaches the endpoint. Until CH7a the window row can
only run at `--height` ≤ 6,500, for the overshoot reason in §6.7. S6's harness runs the same argv
shape against **public** peers to that height to show it, and it runs `nodebench`'s
`--dry-run`/preflight (D113's method check) to show that no RPC method the workload calls is
missing. The manager schedules the window run once S6 has a binary.

---

## 7. Implementation plan

Each slice is one PR-sized commit series on `node/p3-block-download`, gated by GATE-FAST tier 1.
They follow-merge `node/p2a-net`'s READY-T15 head until train 15 lands.

| Slice | Content | Acceptance tests | Size |
|---|---|---|---|
| **S1** | `ChainAccess::{process_block, status, in_active_chain, active_at, last_common_ancestor, drain_snapshot}` on the mirror and `SimChain` (D47); the bounded `SignalQueue` (`PreStart`/`Live`, V's stamps, the prefix cutoff, per-entry release, `wait_for_space` with the no-nesting flag and the ring when non-empty) and the doorbell (C5, D26, D29, D37, D45); every drain point, each message's snapshot through a drain (I0) and the post-reply `Redrain` for `ProcessHeaders` (A-13, D46); the `block_inv` cell as a `RelayList` and the head of `send_messages` (cell, S, move, drain; DEC-236, D44); the header cell (`PeerHeaders` moved out of `PeerState` with every reader, `lock_headers` against `announce_stamp` with the head's `recent` copy before the stamp read (D58), `Ahead` → `wait_published` with its fast path → drain → retry at each region start, `getpeerinfo` included, the head's copy of `recent`) and the thread-local no-V-wait-under-a-cell assertion (A-13, D50, D56); C5′'s pre-accept wait in `SimChain` and the hook (D55); `BlockSubmit::merkle_checked`; the two-phase `process_messages`; `on_block` + `block_tail`; `dash_consensus::is_block_mutated`; `BlockDownload` skeleton with `in_flight`, `block_source`, `remove_block_request`, `block_requested`, `finalize`, `block_checked` (punish branch); stats mirror | L0 in-flight model; the signal-queue tests of §6.1, including the hold, pre-start, N-sender, no-nesting and per-entry cases; the 10,000-marks, cell-and-S, two-cell, ring, message-snapshot and round-trip, and cutoff tests, and the `RelayList` proptest, and the header-cell and C5′ tests, all on S1's own mechanisms, P2a-core's section 7 and `BlockChecked`, with a recording seam that stores the announce stamp and writes header cells where V's visit is S4's (D37, D44–D48, D50, D55, D56); `merkle_checked` honoured by `SimChain`; the (A − B) `instructions:u` gate on both legs with mainnet-like transaction counts (§5.1, D30, D35); DEC-230's guard (the `PreStart` assertion and the reachability test, D36); S2-f's invalid-branch case; the three P2a-core D65 tests updated to real `BlockDownload`; a test that `getpeerinfo` does not block during a `ProcessBlock` wait | M |
| **S2** | `find_next_blocks_to_download`, `getdata_section`, `stall_section`, block timeout, decay in `block_connected`, `headers_direct_fetch` (MSG_BLOCK only) in S1's post-reply continuation (D46), `is_block_requested_from_outbound` | L0 scheduler, stall and timeout units; sim S2-a…S2-h; transcripts 1–2 | L |
| **S3** | `compact.rs` (`PartiallyDownloadedBlock`, `ShortIdIndex` with the 305-entry release-extracted prime table, `CompactTxSource`/`NoMempool`, `ExtraTxns` and `add_to_compact_extra_transactions`); `on_cmpctblock` (steps 6–13 on the post-reply drain's snapshot, D46), `on_blocktxn`, `process_compact_block_txns`; the `MSG_CMPCT_BLOCK` swap in direct fetch | L0 compact units, including the ring by direct injection; the bucket sweep for n = 0..=65,535 against the GCC 13.3.0 static `unordered_map` probe; the transition sets against the sha256-pinned release `dashd` (transcript 5); sim S3-*, including S2-e's IBD case and S3-r (D46); `cmpct_reconstruct` smoke at its floor (§6.5); transcript 3. **READY requires `diff_cmpct_initdata` (E4) and the compact steps of `diff_p2p_multi_blocks` green at their floors (D21, D31); not READY with those rows PENDING** | L |
| **S4** | HB selection in `block_checked`; `NewPoWValidBlock` whole on V's thread: the encoding, `recent`, and the visit through header cells against C8's `TreeRead`, pushing with `NetCore::push` (D50); the encoder; `Shared::recent`; section 7's cmpctblock branch with `ChainAccess::read_block` | L0 HB list; sim S4-*, including S4-x's ten cases; the new-header `submitblock` with an inv-announcing peer (no `cmpctblock` to it, m-2); transcript 4; the timing probe's ordering gate on `SimChain` (§6.3 item 6, D52). **READY requires the HB and announcement steps of `diff_p2p_multi_blocks` green at their floors, including S4-x's ten cases for all three header preferences (D21, D31, D34, D37, D44–D46); not READY with those rows PENDING. S4-v, a subset of S4-x through real V that includes case (6), runs in whichever of S4 and S6 lands second (D48). **Entry condition:** C5′, C6, C7 and C8 are text in CH1-node's signal contract (A-1, A-1b)** | M |
| **S5** | the `diff_p2p_multi_blocks` script and `diff_cmpct_initdata`, registered as named PENDING rows until their entry points land (§6.4, D21); fuzz targets registered with their floors (§6.5); O11 passlist entries prepared | **S5 is READY with named PENDING rows** (D31): every target is registered, each target whose entry point exists is green at its floor, and the report names each PENDING row and the entry point it waits for | M |
| **S6** | `rdashd` composition (manager ruling on OQ1; ND1's owner reviews the init and shutdown placement): a host `ChainAccess` newtype over `ChainViewHandle` (P2a-core D59's item); the `ValidationInterface` listener with C5's hook, registered before V's startup ABC and imports (A-12); `NetCore` + `MessageLoop` thread (named `p3-net`) started with connman, stopped before V, with the drop guard that closes the signal queue on every exit (D32); the testnet harness | §6.7 (a) with the mechanical honesty check, and (b); `nodebench` preflight of `ibd-testnet-seed`'s argv; the (A − B) `instructions:u` gate with real V, and the real-V merkle-pass gate (§5.1, D24, D30, D35), the comparison a named PENDING row until E1 and E6 land (D48); through real V (dashd-chainstate test support), the two-cell window and the cell-and-S cases of §6.1, and S4-v if S4 has landed (D48); the timing probe with real V (§6.3 item 6, D52); a test that an actor panic with V parked closes the queue and V resumes; `perfgate bench_inputs` reported | M |

Order: S1 → S2 → S3 → S4. S5's scripts are written alongside S3, because S3 and S4 cannot report
READY without the compact differentials. S6 can start after S2, because blocks without compact
support are enough for IBD.

---

## 8. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| R1. C1, C2 and C5 not yet guaranteed by V (signals before reply; publication before reply; queue waits only at release points) | BlockChecked delivered after the tail (D65 hazard back), a stale `status` read in the `CompactReconstructed` tail, or an unbounded signal backlog | A-1, A-1b and A-2 to CH1-node/CH3b; S1 tests them against `SimChain` and a CH1-node integration test against the real actor (dashd-chainstate test support) |
| R2. No production `ChainAccess` implementation and no `rdashd` linkage yet (D59 filed it for "P2a-node", which has no PLAN row) | testnet IBD acceptance cannot run | P3's S6 (manager ruling on OQ1) |
| R3. CH3b is not started; CH3's readiness guard refuses DIP3 (7,000) on testnet until CH7a, and `-stopatheight` overshoots | real IBD evidence stops at 6,500; full-tip testnet evidence waits for CH7a | late-closing (c); the sim and transcripts carry heights above 7,000 |
| R4. libstdc++ bucket emulation wrong for some n | a compact block Core reconstructs is `Failed` here, or the reverse; on the wire that is an extra `getdata(BLOCK)` | prime table read from the release binary; n swept 0..=65,535; transition sets black-box against the release `dashd`; `diff_cmpct_initdata` against Core's real map with the release's libstdc++ (D21, D22) |
| R5. Waiting on `Activated` for a long ABC looks like a hang to operators | none on the wire (Core parity) | the `p3_process_block_wait_us` histogram only; no extra log line, because Core logs nothing there and asserted logs must match (D11) |
| R6. Timing of `NewPoWValidBlock` and `BlockChecked` (Q9) | a later peer's region frames can trail an earlier peer's `cmpctblock(B)` by one actor region; `sendcmpct(1)` after HB selection comes the rest of an ABC step plus one doorbell wake after Core's; an accept right after another origin's connect waits for one drain (C5′); per connection nothing reorders; under back-to-back tip-extending accepts a peer's `Ahead` retries have no bound, so its regions progress only in the gaps between accepts (D59) | accepted R4 deviation, recorded; the visit runs inline in `AcceptBlock`, before the write and ABC (D50); the announce stamp, read after the head's `recent` copy, keeps each region's reads consistent (D56, D58); C5′ keeps `BlockChecked` ahead of later accepts (D55); §6.3's probe gates the order and reports the distributions (D52); no handoff is added for the retries, since Core's unfair `cs_main` gives no bound either (D59) |
| R7. Python FTs depend on rows not yet built (O11, P2b, RP1a, MP1a, P5) | FT acceptance closes late | sim ports are P3's M3 evidence; OQ2 |
| R8. A Core security weakness found while porting | — | embargo rule (PLAN §7): write only to the private candidate file and say "possible security note: see manager" |
| R9. A deadlock between V's signal-queue wait and an actor wait | the node hangs | DEC-229's binding rule and §3.8.1's instances; C5 keeps V's waits at release points, and out of the hold, `PreStart` and nested waits; the S1 stall, hold, pre-start and N-sender tests; A-1b adds the cases to CH1-node's lock-and-wait model |
| R10. E4 or the session entry points land late | S3 and S4 cannot report READY | S1, S2 and S6 land first (IBD needs no compact blocks); S6 is READY with its (A − B) comparison a named PENDING row until E1 and E6 (D48); the PENDING rows name what each slice waits for (D21) |
| R11. A peer feeds transactions before a compact block | rdashd's `getblocktxn` differs from Core's until P5 and MP1a (§3.5.5) | the interim ALLOWED-DIFFS entry with closing owners; no compact-client parity claim before then (D19) |
| R12. A thread waits on V while it holds a header cell | V's visit waits on that cell: deadlock | the cell is dropped before every `ChainAccess` call that sends to V; `Ahead` releases every lock first; S1's thread-local assertion in every build; §3.8.1 (D50) |

---

## 9. Open questions (ruled by the manager, 2026-10-17; D25)

- **OQ1. Who composes `dashd-p2p` into `rdashd`?** **Ruled:** P3's S6 does, with the minimum set:
  host `ChainAccess` newtype, listener, actor thread, and init and shutdown hooks in ND1's order.
  ND1's owner reviews the init and shutdown placement. The broader P2a-node items (the sim onto
  `dash-chain`, the mirror types as re-exports) stay unowned.
- **OQ2. Late-closing FT acceptance.** **Ruled:** the Python `p2p_compactblocks` file closes in P4's
  row. P3 closes the sim ports and says so (§1.3, §6.6). The Python `p2p_block_sync` and
  `p2p_ibd_stalling` runs under O11, and the full-tip run after CH7a, are P3's late-closing items
  (CH3a D55 pattern).
- **OQ3. Oracle entry points E1–E5.** **Ruled:** the manager takes them to the oracle owners as a
  PLAN delta. Until they land, the targets are named PENDING rows, not silent skips (§6.4). E4 and
  the compact-handler differential are required for S3 and S4 READY (Sol r1 M3, D21).
- **OQ4. Interim compact-block behaviour without a mempool.** **Ruled:** keep Core's HB logic with
  the empty transaction source. Disabling HB selection would change `sendcmpct(1)` emission and
  `getpeerinfo`'s `bip152_hb_to`. The remaining interim deviation is §3.5.5's (D19).
- **OQ5. Section 7's announcement and `NewPoWValidBlock` in P3 rather than P4 (D3).** **Ruled:** P3.
  P4 keeps getdata(CMPCT)/getblocktxn serving and reads `Shared::recent` from P3.

---

## 10. Interface asks

| # | To | Ask |
|---|---|---|
| A-1 | CH1-node / CH3b | C1: synchronous signals for a `ProcessBlock` reach listeners before its reply. C2: the `Activated` reply follows the publication that carries the block's status. A status read through `ChainViewHandle` returning the last published value (`nStatus` validity, `HAVE_DATA`, failed, `nTx`, `nChainTx != 0`), plus `Contains`, `active_at` and `LastCommonAncestor`. C8: `new_pow_valid_block` runs at v:4832, inside the accept scope and before the write, and hands the listener the entry's `EntryFacts`, the stamp, and a `TreeRead` over V's in-scope block tree (lookup by hash, `ancestor(id, height)`, chain work), valid during the call; V may wait in the call for a P3 header cell, whose holders never wait on V while holding it (LO-5, D50), and during the call V holds no lock that a `ChainAccess` read, `snapshot()`, `drain_snapshot()` or `read_block` takes (D57). `wait_published(seq)`: return at once if the current `ChainView` has `view_seq ≥ seq`, else an R-class no-op round trip (an existing class R read will do) (D50, D57). C6: a queued `UpdatedBlockTip` reaches the signals thread only after the publication carrying its tip (today's `publish_view` then splice order). C7: (a) the first publication after a synchronous signal reflects the state V emitted it in: no publication falls between a push and the end of the scope or hold that pushed it (`accept_header` does not publish; D49); (b) at every C5 wait point, and at every point where V serves a command, V has published since its last synchronous signal; confirm (b) for `failure.rs:149` and an import's accept, and that the C5 hook runs at every ABC loop head, including the first after an accept (D45). C5′, C6, C7 and C8 are text in CH1-node's signal contract before S4 starts (D34, D37, D50, D55). `drain_snapshot()`: one `ChainView` load returning its snapshot and its `view_seq` (D47) |
| A-1b | CH1-node | C5 (§2.4) under **DEC-229** and **DEC-230**, which LO-9 should cite: (1) a `ValidationInterface` hook that V calls at its release points (`limit_queue`'s two sites, the idle point, before an import's `AcceptBlock`) and nowhere else, and never while `holding`; (2) a `ServiceWait` that accepts a listener-supplied condition; (3) the **idle-point mode**: a `VWait` with no composite running selects both lanes, the bell and the condition, dispatches each command with `at_release = true`, so class C goes to `deferred` even though no composite runs, and serves R and M. It does not call `release_point()` (whose `debug_assert!(in_composite)` is at actor.rs:1101) and takes nothing from `held`. After the wait V runs `deferred` in order, then its normal loop. An import accept outside a composite uses the same mode; (4) confirm that each `ProcessBlock` sender has at most one command queued (none sends again after an `Accepted` reply before its block's ABC tail has run, or list those that do), and that none uses the marked lane, as §2.5's maximum assumes; (5) the lock-and-wait model gains the hold-plus-full-queue, pre-start and N-sender cases; (6) withdrawn: DEC-236 supersedes DEC-233, no listener waits on the signals thread, and LO-9's marker stands unchanged (§3.8.2, D44); (7) **C5′** (DEC-240, D55): before a `ProcessBlock` or import accept outside IBD, the C5 hook also waits until no signal is outstanding, and a C5 wait defers such an accept instead of running it |
| A-2 | CH1-node | C3: `BlockResult.new_block` is `AcceptBlock`'s `*fNewBlock`, set at v:4835 before the write; `BlockResult::rejected` (`command.rs:85`) used only before v:4835. C4: `NewPoWValidBlock` only under v:4831's guard |
| A-3 | CH1-node | `read_block(id)` for section 7's cmpctblock, from the block LRU or `pread` (A1 §2.14). §3.8.1 relies on this property: `read_block` takes no lock that W holds while W waits on V's bell service, none that V holds in any C5 wait (D33), and none that V holds inside the `new_pow_valid_block` call, because section 7 may read a block while V waits for that peer's header cell (D50) |
| A-4 | CS3 owner (dash-consensus) | `pub fn is_block_mutated(&Block) -> bool` (v:4434-4456) over the existing `check_merkle_root`; additive (D9) |
| A-4b | CS3 / CH1-node | honour `BlockSubmit::merkle_checked`: `check_block` skips the merkle pass as Core's `m_checked_merkle_root` does (v:4326, v:4362). A measured S1/S6 gate until then (§5.1, D24) |
| A-5 | O1c / O2 owner (manager's PLAN delta) | E1–E5 (§6.4), and E6 for §5.1's B pass: a session step that submits a block straight to `ChainstateManager::ProcessNewBlock` with the session's `PeerManager` unregistered from validation signals (D35). E4 is required before S3 READY, with the release's libstdc++ (GCC 13.3.0, static) whenever GCC 15 differs (D21, D22) |
| A-6 | MP1a | implement `CompactTxSource` in `vTxHashes` order (tm:475, 726-733, 871) |
| A-6b | MP1a | Core's `RecursiveDynamicUsage` for a transaction, and the ATMP rejection outcome, which P5's np:5016-5020 site needs to call `add_to_compact_extra_transactions`. The usage follows Core's `memusage` model, including `MallocUsage`'s allocation rounding for glibc LP64 (memusage.h), because the 100,000 threshold (np:5018) depends on it exactly (D33) |
| A-7 | P5 | call `BlockDownload::add_to_compact_extra_transactions` at Core's two sites: an orphan accepted into the orphanage (np:4997-4999), and a rejected transaction whose `RecursiveDynamicUsage` is under 100,000 (np:5016-5020). P5's acceptance gains the orphan-then-compact row (below) |
| A-8 | P4 | serve `getdata(MSG_CMPCT_BLOCK)` and `getblocktxn` from `Shared::recent` and V's blocks; `MAX_CMPCTBLOCK_DEPTH`/`MAX_BLOCKTXN_DEPTH` are P4's |
| A-9 | ND1 | the asserted-log strings in §3.11; `-blockreconstructionextratxn` and `-blocksonly` passed to `BlockDownload`; the signal queue in the memory accounting at §2.5's maximum (1,024 entries and 8 MiB, plus Q entries, plus one outer ABC iteration's `BlockChecked` entries, with Q from `-rpcthreads`); each peer's `block_inv` cell is at most 8 hashes (D44), and its header cell four fields and a stamp (D50) |
| A-10 | P2a-core (in S3) | a `via_compact` parameter on `process_headers_message` (`sync.rs:190-224`) |
| A-11 | SB6 | keep Dash's joint quorum/process-queue `fPauseRecv` and `PollMessage`'s priority (net.cpp:4803-4847) |
| A-12 | ND1 | the order of P3's pieces against V's startup and import work (D26): register the P3 listener before V's startup ABC and imports, as Core registers PeerManager (init:2199) before ThreadImport (init:2455); start the net actor with connman (init:2755); at shutdown, stop the actor (its drop guard closes the queue) before V stops serving (init.cpp:348 before init.cpp:383). V never waits on a `PreStart` queue, so init may wait for V's startup work in any order |
| A-13 | P2a-core (in S1) | split `process_headers_message` after an accepted `ProcessHeaders` reply into a continuation that the actor runs after a drain (`BlockStep::Redrain`), and read that drain's snapshot in its two `&r.snap` uses (`sync.rs:245`, `sync.rs:248`); the reply contributes only `last_accepted` and the invalid state (I0, D46). `BlockCheckedCx::with_peer` takes the drain's snapshot instead of loading `snapshot()` per call (`seams.rs:233`, D47). `BlockInv::headers_relay` becomes a `RelayList`, and `PeerState::blocks_for_relay` (`peer.rs:131`) returns it; its callers (`node-l1/src/p2p_node.rs:1094`, `tests/sim_sync.rs:990`) adapt (D44). The four `CNodeState` fields `NewPoWValidBlock` touches (`requested_hb_cmpctblocks`, `best_known_block`, `last_unknown_block`, `best_header_sent`; `peer.rs:83`, `peer.rs:109-113`) move into a `HeaderCell` that `PeerSlot` also holds, and every reader takes it with `Shared::lock_headers`: `sync.rs:87-124`, `sync.rs:296-301`, section 7 (`sync.rs:423-507`), `logic.rs:555`, `eviction.rs:94-111`, `stats.rs:67`, and the digest and test readers (`node-l1/src/p2p_node.rs:1143`, `tests/sim_m2.rs:616,629`). The actor drops the cell before each V send (D50) |

**PLAN deltas** (for the manager):

- **compat ALLOWED-DIFFS adds** (interim, outside the PRD's classes 1–3; owners P5 and MP1a; must
  close before REL-5.6; D19): "P3 §3.5.5: until P5 and MP1a, rdashd keeps no transaction a peer
  relays, so a compact block containing a transaction the peer relayed first (valid, orphan, or
  rejected with `RecursiveDynamicUsage` under 100,000) gets a `getblocktxn` that lists its index,
  where Core's omits it (np:4997-4999, np:5016-5020)". O14 creates
  `compat/ALLOWED-DIFFS.md` (PRD §0.4); the entry is carried there, as CH2 D12's was.
- **P5 adds** an acceptance row: "orphan-then-compact: a peer sends a child before its parent, then
  a compact block with both; rdashd's `getblocktxn` requests the parent only, as Core's does
  (P3 D19)", plus the same for a small rejected transaction.
- **P4 adds** the unmodified Python `p2p_compactblocks` to its acceptance (manager ruling on OQ2).
- **P3's row** reads: compact-client UA parity after P5 and MP1a (D19); E4 and the compact
  differential required for S3/S4 READY (D21).
