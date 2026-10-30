# SB0: the shared base for dash-spv and rdashd (Fable leg)

| | |
|---|---|
| Status | D2 independent design, Fable leg; the Opus leg is written in parallel and a reconcile follows. Review per DEC-92: Sol, then Opus high |
| Date | 2026-10-09 |
| Author | Fable 5.1 (SB0 Fable leg) |
| Task | DEC-81: one stack for SPV and the node. Decide the layering, the replacement of A1 rule R2, where SPV mode stops and full validation plugs in, one network loop, one header/sync flow, one MN-list/quorum/ChainLock pipeline, the mobile budget, the storage engine under DEC-85, the migration path for mobile consumers, the rework of P2a/CH1/CH2/ST2/ST3a/EV0/EV2/LQ1/A1, the plan revision including CV5's placement, and the performance rules |
| Binding inputs | CHARTER; DECISIONS DEC-81…DEC-94 (DEC-85 re-check of DEC-43; DEC-94 Q7 makes CH2 §10.1 P1–P9 an input); PLAN §0–§4 and the held set; A1 §1.1, §2.1–2.3, §2.5, §2.10, §2.14, §2.23, §2.24, §3.1; P2a §2.2–2.8; CH2 rev2 §2.5.1, §2.9, §2.9.1 (F1–F9), §4.4.1, §10.1 (P1–P9); EV2 §2.10–2.12; LQ1 §2.6–2.8; ST1 §9; PRD US-LCS-01…07, NFR-CMP-01…05, NFR-RES-01…05 |
| Code read | rust-dashcore `dev` at 67b307c6 (dash-spv, dash-spv-ffi, key-wallet-manager, dashcore `sml` and `network`); integration worktree (dash-consensus, dashd-store, deny.toml, `contrib/node/r2_reach.py`); PR #902 at a0e64364 (`gh pr diff 902`), #1081, #921, #375, #1104 |
| Evidence | `/work/scratch/sb0-fable/`: `sizes/summary.txt` (aarch64-linux-android stripped sizes, §4.2), `sizes/rss.txt` (the resident-memory output), and the per-row build logs (`sizes/*.log`, `android-size*.log`). The probe sources and the NDK env script were removed after the run; the NDK recipe is in agent memory (`android-cross-build-recipe`) |

> **EMBARGO-1/2/3/4.** Nothing here touches them; they are referenced by codename only, as the rules require.

## 0. The recommendation on one page

1. **Build from dash-spv, by extraction, not by replacement.** Four shared crates come out of dash-spv's `network/`, `chain/`, `storage/block_headers`, `validation/header` and `sml_engine` modules, and the node's P2a/CH1/EV2/LQ1 designs land *inside* those crates instead of beside them: `dash-p2p-transport` (P1, as planned), `dash-p2p` (the one network loop: P2a-core with a `Light` profile), `dash-chain` (the one header flow: block-index arena, header acceptance, best header, `ChainReadSnapshot`, the `ChainEvent` bus that is CH2's `SubscriberFeed` producer) and `dash-mnlist` (the one MN-list/quorum/CL/IS pipeline: `dash-evo` algorithms bound to two list representations and two sources). dash-spv keeps its managers, wallet integration, FFI and tests on top of them; rdashd's SPV mode **is dash-spv's managers** running on the node's storage (CV5 is placed here, at M2, no longer post-M7).
2. **Where SPV mode stops:** at `dash-chain`'s tip. In SPV mode the tip is the best header, blocks are fetched on demand for filter matches, and the subscribers are the filter, wallet and MN-diff managers. Full validation plugs in as one more subscriber-and-driver of the same chain: `dashd-chainstate` (CH1's block half plus CH2) consumes `HeaderAccepted`, fetches blocks through P3, connects them on V and moves the *validated* tip; the header tip and the validated tip are two fields of the same snapshot, as in Core (`m_best_header` and `m_chain`).
3. **One loop, two profiles.** `dash-p2p` runs Core's `PeerManager` actor for both. `Profile::Full` is P2a as designed (inbound serving arrives with P2b). `Profile::Light` is Core's own client shape (`fClient`, `-listen=0`, outbound only, few peers, no relay unless mempool tracking is on) plus an SPV **request broker** overlay, which is #902's scheduler (keyed dedup, strict priority, bandwidth-sized in-flight caps, timeout/requeue, latency-ranked peer supervisor) moved above the loop. #902's `NetworkManager` trait is the compatibility seam dash-spv's managers keep.
4. **One MN-list pipeline, two trust models, one algorithm.** LQ1's member selection, commitment verification and CL/IS verification are generic over the list representation; the node binds them to the full DML (`MnList`, EV2) and the SPV side binds them to the simplified list (SML, what `mnlistdiff` carries). The node computes lists from blocks and projects the SML it serves (EV4); the SPV side applies verified diffs. The projection equality SML(DML(h)) = SML(diffs to h) is the cross-check, run as a test and optionally live.
5. **Storage: DEC-43 stands for rdashd in both modes; RocksDB never enters the mobile build.** Measured on this box: today's arm64 `libdash_spv_ffi.so` is **12.55 MB stripped**; RocksDB with lz4+zstd is **7.79 MB** on its own (§4). Mobile keeps dash-spv's height-keyed segment store behind a shared `ChainStore` trait; the node implements the same trait over `dashd-store`'s column families, so rdashd `--spv` and full mode share one database and the SPV→full upgrade keeps the index.
6. **R2 is replaced by a layer table** (`contrib/node/layers.toml`, checked by a rewritten `r2_reach.py`): edges only go down the layers; `dashd-*` may depend on dash-spv (that is the SPV mode), dash-spv may depend only on shared and base crates; a **mobile closure ban** (no `dashd-*`, no storage engines, no node-only crates in `cargo tree -p dash-spv-ffi`) and an **arm64 size budget** gate replace the old "node never depends on dashcore" ban; the hot-path type rule survives as a `wrappers` list for `dashcore` (§2.3).
7. **CH2's P1–P9 are met** by `dash-chain` as the `SubscriberFeed` producer with V as the single tip writer in both profiles; node-only items (flush order, prune locks, subscriber engines, the latch, undo) stay in `dashd-chainstate`, and P8 becomes a capability check at registration (§2.7).
8. **Migration** is five staged, upstreamable PR series after #902 merges, each behind an existing seam (`NetworkManager`, `BlockHeaderStorage`/`StorageManager`, `MasternodeListEngine`), with the FFI's 41 exported functions, its callbacks and the config setters unchanged; storage layouts migrate under the existing storage-version system (#1084).
9. **Performance:** nothing SPV-shaped sits on the node's hot path. The actor, the arena, the snapshot and the per-command decode policy are P2a/CH1 as designed; the SPV adapter decodes only what SPV subscribers asked for; residency windowing (mobile's memory lever) is a branch taken only on an index miss below a boundary that is 0 on the node.
10. **Plan:** P2a-core, CH1 and EV0 resume now under this document's briefs (SB1–SB3); CH2b, EV2b, LQ1b resume when their one-page addenda land; CV5 becomes SB4 at M2; a mobile gate (SB5) joins CI. M2 exit moves by about three weeks; M5's SPV smoke becomes a by-product.

---

## 1. Current state of dash-spv, and what #902 changes

### 1.1 Shape

`DashSpvClient<W: WalletInterface, N: NetworkManager, S: StorageManager>` (`dash-spv/src/client/core.rs:38`) owns the config, the network, the storage, the wallet, an optional `MasternodeListEngine<PersistentBlockHeaderStorage>` and the `SyncCoordinator`. Eight sync managers, each a tokio task with a `tokio::select!` over its message receiver, the `SyncEvent` broadcast, `NetworkEvent`s and a 100 ms tick (`dash-spv/src/sync/sync_manager.rs:163-250`), coordinate through `SyncEvent` (`dash-spv/src/sync/events.rs:12-78`: `BlockHeadersStored`, `FilterHeadersStored`, `FiltersStored`, `BlocksNeeded`, `BlockProcessed`, `MasternodeStateUpdated`, `ChainLockReceived`, `InstantLockReceived`, `ForkDetected`, …) and per-manager `watch` progress channels. Sizes: `sync/filters/manager.rs` 4,056 lines, `sync/mempool/manager.rs` 2,532, `network/manager.rs` 2,313, `chain/checkpoints.rs` 1,330, `storage/segments.rs` 1,303, `sml_engine.rs` 1,850; 39k lines in the crate, 3.5k in `dash-spv-ffi`, 6.5k in `key-wallet-manager`. The last 25 commits are all dash-spv (Borja Castellano, Kevin Rombach, QuantumExplorer lead the history); this is live, maintained code, which is the reason to build from it.

| Area | Today (`dev` 67b307c6) | Shared-base disposition (§2) |
|---|---|---|
| **Network** | `NetworkManager` trait (`network/mod.rs`): `message_receiver(&[MessageType])`, `request_sender()`, `send_message`, `broadcast`, `dispatch_local`, `request_qr_info`, `disconnect_peer`, `subscribe_network_events`; `PeerNetworkManager` with pool, handshake, reputation, latency, addrv2, DNS discovery, `storage/peers.rs`; a `DownloadCoordinator` per pipeline | **extracted and replaced** by `dash-p2p` (`Profile::Light`) behind #902's trait; #902's broker becomes the SPV request overlay |
| **Header sync** | `BlockHeadersManager` (`sync/block_headers/manager.rs:62-330`): `HeadersPipeline` downloads checkpoint-delimited segments in parallel (`pipeline.rs:60-130`), validates each segment's end against the checkpoint hash, stores in order; per-header validation in `BlockHeaderValidator` (`validation/header.rs`); current SPV-mode validation coverage: see PLAN §7 handling (CANDIDATE-SPV); forks: `handle_fork` stops every manager, truncates every store above the fork and restarts (`client/lifecycle.rs`); anchoring: `initialize_genesis_block` writes a version-0 checkpoint header at `start_from_height` (`client/lifecycle.rs:319-360`) | acceptance **moves into `dash-chain`** (Core's `AcceptBlockHeader` with the anchored-window rule, §2.5); the segment pipeline stays as the Light profile's `HeadersSource`; forks become `Disconnected`/`Connected` events and a store `truncate_above`, no restart |
| **Storage** | `PersistentStorage`/`StorageManager` (`storage/mod.rs`): per-store traits (`BlockHeaderStorage` `storage/block_headers.rs:44-110`, filter headers, filters, blocks, masternode messages, metadata), each with `truncate_above`; `DiskStorageManager` with a worker; height-keyed segment files (`storage/segments.rs`): `HashedBlockHeader` 112 B × 10,000, `FilterHeader` 32 B × 50,000, filters × 2,000, `HashedBlock` × 1,000; two resident segments per store (#1086), dirty-on-evict persistence (#1087), versioned layout (#1084); an in-memory hash→height index by hash-prefix groups (#1095) | **kept** as the mobile `ChainStore` backend (§4); the node implements the same trait over `dashd-store` |
| **MN lists, quorums** | `sml_engine.rs` (private module; `MasternodeListEngine<H: BlockHeaderStorage>`: `feed_qr_info`, `apply_diff`, `masternode_lists_around_height`, `rotated_quorums_of_cycle`, `missing_work_block_list_requests`, verified-cycle bookkeeping); algorithms in `dashcore::sml` (`masternode_list/{apply_diff,from_diff,merkle_roots,quorum_helpers,rotated_quorums_info,scores_for_quorum}`, `quorum_entry/validation.rs` BLS verification, `llmq_type` params); `MasternodesManager` fetches `qrinfo` first, then `mnlistdiff`s for the missing work-block lists, stores messages until the tip is reached (#1079) and replays them at load (#1073, #1091) | engine **re-based on `dash-mnlist`**: the same state machine over `dash-evo`'s generic algorithms, with the SML binding; public facade kept during migration (§2.6) |
| **ChainLocks / IS** | `ChainLockManager` (`sync/chainlock/manager.rs`): pending until the MN engine reaches the signing height (#1063), best CL in metadata, validated against the cycle quorums; `InstantSendManager` with a pending queue and TTL; `validation/{chainlock,instantlock}.rs` | **verification shared** (`dash-mnlist::verify_*`); the two consumers (SPV trust anchor; node enforcement in CH1/LK1) stay distinct, both through `dash-chain`'s CL hook |
| **Filters** | `FilterHeadersManager`, `FiltersManager` (BIP157 fetch, per-wallet matching with `key-wallet-manager` `parallel-filters`, lookahead batches, rescans), `BlocksManager` fetches matched blocks | **kept** in dash-spv; in rdashd full mode IX1 is the filter *producer* and the same managers can read locally (§2.5) |
| **Mempool** | wallet-side tracker with bloom `filterload`, broadcast with holdout/acceptance threshold | kept in dash-spv (SPV mode); `dashd-mempool` is the node's, unrelated |
| **FFI** | `dash-spv-ffi`: `cdylib`+`staticlib`+`rlib`, 41 exported `dash_spv_ffi_*` functions on arm64 (client lifecycle, config setters, broadcast, progress, wallet manager, logging, errors), `FFISyncEventCallbacks`/`FFIProgressCallback`/peer callbacks, `platform_integration.rs` (`get_platform_activation_height`, `get_quorum_public_key`); `InnerClient = DashSpvClient<…>` (`src/client.rs:17`) | **unchanged ABI** (§5.3) |

Dependencies that matter for the budget: dash-spv pulls `dashcore` with `serde, core-block-hash-use-x11, message_verification, bls, quorum_validation`, `key-wallet-manager` with `parallel-filters` (rayon), tokio `full`, `clap`, `tracing-subscriber`; the FFI closure is 163 crates and already contains `blst`, `secp256k1`, `rayon`, `tokio` (`cargo tree -p dash-spv-ffi`). `dash-consensus`'s closure is 48 crates, of which only `dash-params`, `dash-pow`, `dash-primitives`, `dash-script`, `dash-num`, `dash-sha256`, `ambassador`, `delegate`, `xxhash-rust` and `libm` are new to the mobile closure.

### 1.2 What #902 (`refactor/network-mod`, +7,354/−10,939, 78 files, last updated 2026-09-22) changes

- **Trait.** `NetworkManager` becomes eleven methods (`pr902 network/mod.rs:16-39`): `start`, `stop`, `send(msg)`, `send_to(addr, msg) -> bool`, `broadcast`, `dispatch_local`, `request_answered(RequestKey)`, `subscribe(&[MessageType]) -> UnboundedReceiver<Inbound>`, `events() -> broadcast::Receiver<NetworkEvent>`, `tip() -> u32`, `connected_count()`. `RequestSender` and the per-pipeline `DownloadCoordinator` are gone; pipelines "declare what they want" and the broker owns dedup, pacing, timeouts, retries and peer hot-swap.
- **Broker.** `RequestKey::{Headers(locator0), CfHeaders(stop), CFilters(start), MnListDiff(hash), Block(hash)}` (`manager.rs:269-283`, derived in `request_keys`), a registry `HashMap<RequestKey, ReqState>` with `queued`/`on_wire` states; `MsgClass::{Other, Blocks, CFilters, CfHeaders, Headers}` drained in strict priority (`manager.rs:341-366`); `PeerPlan` sizes each peer's cap by Little's law from its own service time, a link-share factor shrinks caps to the bandwidth-delay product (`manager.rs:896-930`); soft and hard timeouts requeue verbatim and drop the peer; a `Supervisor` fills below capacity, filters on service flags, ranks by handshake latency and swaps slow peers (`manager.rs:1524-1620`).
- **Inbound routing.** Topic subscription by `MessageType`; a single consumer receives the payload without a copy; `InboundMessage` carries the header hashes computed while decompressing `headers2` (`manager.rs:129-180`).
- **Removed.** Peer storage, reputation, the standalone dispatcher/handshake/pool modules, `tests/peer_test.rs`, `tests/test_handshake_logic.rs`; `sync/download_coordinator.rs`.
- **Also touched.** `dash/src/network/message.rs` (boxed large payloads landed separately as #1098), every sync manager and pipeline, `dash-spv-ffi/src/{client,callbacks}.rs`, examples, `masternode-seeds-fetcher`.

**Helps.** (a) The trait is exactly the seam the shared loop implements: an adapter can satisfy all eleven methods from a `PeerManagerHandle` plus the broker, and `DashSpvClient<W, N, S>` already accepts any `N`. (b) The broker is a good SPV request scheduler and is independent of connection management; it maps onto P2a's hooks as an overlay (§2.4). (c) The `MockNetworkManager` gives the dash-spv suites a network double, which the shared base inherits. (d) It deletes the three modules A1 §2.23 planned to replace anyway.

**Conflicts.** (a) The connection half (handshake, peer supervision, latency ranking, DNS discovery) is written again inside the broker; the shared loop supersedes it, so SB1 must land *after* #902 and re-home that half rather than merge against it. (b) The `Supervisor` picks peers by latency and swaps them; Core's `opencon` picks from addrman. The Light profile keeps #902's policy through a `PeerSelector` hook, the Full profile keeps Core's (§2.4). (c) `tip()` on the network trait is the best height peers advertised in `version` (`best_tip`, `manager.rs:651-653`), not our chain's; the adapter answers it from the `NodeTable`'s `start_height` maximum, as the broker does. (d) #902 sends `getheaders` with a locator the pipeline builds; in the shared design the Light profile's headers source still builds its own locators (per segment), while the Full profile uses Core's. (e) Anything SB-series changes under `dash-spv/src/network/` must wait for #902's merge; the shared crates themselves do not touch that directory, so the only contended file is the adapter module added after the merge.

### 1.3 The sibling PRs

- **#1081** (2 files): the engine's `validated_cycle_heights` seeded from replayed messages; a `dash-mnlist` concern, carried over unchanged.
- **#921** (sync backfill, 78 files, +6,036/−9,120): the managers' shape after it is the one described above; the shared base assumes it.
- **#375** (checkpoints rewritten into generated `dash-spv/checkpoints/*.checkpoints`, one FFI function removed): the anchor list `dash-chain` reads in Light profile (§2.5: sync anchors are distinct from Core's consensus checkpoints).
- **#1104** (wallet `truncate_above`, `WalletEvent::ChainTruncated`, FFI `on_chain_truncated`): the wallet-side contract the event-driven reorg of §2.5 preserves.

---

## 2. Layering

### 2.1 Crate map

```
layer 0  base          dashcore, dashcore_hashes, dash-network, key-wallet, key-wallet-manager, dashcore-rpc(-json),
                       base-sdk (dash-types, dash-pkc, dash-params, dash-pow, dash-primitives, dash-script), dash-x11, dash-sha256
layer 1  pure          dash-consensus, dash-evo (EV0/EV2/LQ1/LQ2), dash-llmq, dash-governance, dash-coinjoin   (A1 X-1 bans, unchanged)
layer 2  shared        dash-p2p-transport (P1)        V1/V2 framing, short IDs, #264's handshake state machine
                       dash-p2p (P2a-core + Light)   the one network loop: ConnMan, AddrMan, PeerManager actor, policy table, hooks
                       dash-chain (CH1's header half) block-index arena, header acceptance, best header, snapshots, ChainEvent bus,
                                                      IndexAccess/ChainSnapshot, ChainStore trait + segment backend, residency windowing
                       dash-mnlist (sml_engine, re-based) DiffSource/BlockSource over dash-evo's generic algorithms; SML binding;
                                                      CL/IS verification; the MasternodeListEngine facade
                       dash-clock (P2a §2.7, was dashd-clock)
layer 3  spv           dash-spv (managers, wallet integration, mempool tracker, HeadersSource::Segments, the #902 broker as overlay,
                                 NetworkManager adapter), dash-spv-bench, dash-network-seeds
layer 4  node runtime  dashd-store (ST1; RocksDB), dashd-chainstate (CH1 block half, CH2, CH3…, V), dashd-mempool, dashd-indexes,
                       dashd-locks, dashd-quorums, dashd-masternode, dashd-rpc, dashd-wallet, dash-coreimport
layer 5  apps          rdashd (dashd-node: Full and --spv modes; --spv wires dash-spv's managers onto the node store),
                       dash-spv-ffi, key-wallet-ffi, dashwallet-desktop
test     test          dash-conformance, dash-testkit (O16), node-l1, node-fuzz, node-bench, dash-oracle-sys, test-vectors
```

Naming follows rust-dashcore: library crates are `dash-*`; node-only runtime crates keep A1's `dashd-*` prefix so the mobile closure ban (§2.3) can be stated as a prefix. `dashd-p2p` and `dashd-clock` are renamed because they are no longer node-only; `dashd-chainstate` keeps its name and loses its header half to `dash-chain`.

**Ownership.** Layer 2 crates are owned jointly: the node program writes them (P2a/CH1/EV/LQ owners) and the dash-spv maintainers review the seams their managers consume (`NetworkManager`, `ChainStore`, the engine facade). dash-spv stays theirs. `dashd-*` and rdashd are the program's. This is the only workable split while #902-class work continues in the same tree (DEC-81 "coordinate, don't fork").

**Why extraction and not "node depends on dash-spv as is".** dash-spv's network, header and engine modules are the three places where the node needs Core's exact behaviour (handshake and message policy, header acceptance, list construction), and where the shared kernel gives both products the same rules (`nBits`, MTP, CL-consistent acceptance, the BLS scheme per quorum version). Writing the Core-exact version inside shared crates and pointing dash-spv at it serves both at once; writing it beside (A1 §2.23) would have left two header validators and two MN engines in one repository, with SPV-as-consumer as the only bridge.

### 2.2 Where SPV mode stops and full validation plugs in

```
                     dash-p2p (one loop, Profile::{Full, Light})
                        │ headers/headers2 (both profiles)          blocks: Full = P3 FindNextBlocksToDownload
                        ▼                                                   Light/--spv = broker getdata on filter match
          ┌──────────── dash-chain ────────────┐
          │ arena · accept_header (PoW, nBits, │   ChainReadSnapshot { best_header, tip, active_chain, bls, epoch }
          │ MTP, time, version, checkpoints,   │   SPV mode:  tip == best_header (advances on header acceptance)
          │ CL conflict) · best_header         │   Full mode: tip == validated tip (advances on ConnectTip on V)
          │ ChainEvent bus (SubscriberFeed)    │
          └──────────┬──────────────┬──────────┘
          subscribers│              │ driver
  ┌───────────────────┐    ┌─────────────────────────────────────┐
  │ dash-spv managers │    │ dashd-chainstate (Full only)        │
  │ filter headers,   │    │ accept_block, coins (CH2), connect/ │
  │ filters, blocks,  │    │ disconnect, ActivateBestChain on V, │
  │ wallet, MN diffs, │    │ evo/llmq state (CH7), subscribers   │
  │ CL/IS, mempool    │    │ hub (indexes), mempool, RPC         │
  └───────────────────┘    └─────────────────────────────────────┘
```

- **The boundary is the `tip` of `dash-chain`'s snapshot and who moves it.** In SPV mode the chain actor (V, one dedicated thread in both profiles, §2.7) moves `tip` whenever it accepts a header that extends the best chain, so `tip == best_header` and `ChainEvent::{Connected, Disconnected}` describe header-chain changes. In Full mode `dashd-chainstate` registers as the **tip driver**: header acceptance still moves `best_header` and emits `HeaderAccepted`, but `tip` moves only when `ActivateBestChain` connects a block, and the `Connected`/`Disconnected` events carry validated blocks. One snapshot type, one event type, one `seq` counter (F2), two drivers. This is Core's own split: `m_best_header` moves when a header is accepted into the index (`AddToBlockIndex`, reached from `AcceptBlockHeader`, `v24 validation.cpp:4598`), `m_chain` in `ConnectTip`.
- **rdashd `--spv`** = `Profile::Light` or `Full` networking (operator's choice; default Light with `-listen=0`), SPV tip driver, dash-spv's managers as subscribers, `ChainStore` on `dashd-store`. **rdashd full** = `Profile::Full`, `dashd-chainstate` as tip driver, P3 block download, CH2 subscribers (indexes), `dashd-mempool`. **Upgrade** `--spv` → full on the same datadir keeps the block index and the MN-diff store; CH5's `-reindex-chainstate` path builds coins from blocks it then downloads (the headers are already there; blocks are fetched by P3 from the index, as a pruned node re-downloading would).
- **Mobile** = `Profile::Light`, SPV tip driver, dash-spv's managers, segment `ChainStore`. Identical to rdashd `--spv` except for the storage backend and the absence of every layer-4 crate.
- **What SPV mode validates** (the same `accept_header` in every mode): PoW against the header's own target; `nBits` against `GetNextWorkRequired` (dash-consensus `pow`, DGW/KGW) once the anchored window allows it (§2.5); MTP time-too-old; time-too-new against `dash-clock`'s adjusted time; version-by-height rules; Core's consensus checkpoints (`dash-params`); `bad-fork-prior-to-checkpoint`; the ChainLock conflict hook. The shared kernel applies Core's header rules in both modes, so the node gets the one acceptance function it was going to write anyway. Current SPV-mode validation coverage: see PLAN §7 handling (CANDIDATE-SPV).

### 2.3 Dependency rules replacing R2

R2 said "node crates never depend on `dashcore`/`dash-spv`", enforced by `deny.toml`'s `[bans] deny` entries with `wrappers` (dashcore, dashcore-rpc, key-wallet*, dash-spv*, dash-conformance, dash-oracle-sys, node-l1) and by `contrib/node/r2_reach.py`'s closure check. It protected two things: the base-sdk type stack on the node's hot path, and the mobile build from node weight. DEC-81 removes the first as a *crate* rule (rdashd's SPV mode depends on dash-spv) but not as a hot-path rule, and makes the second the primary rule. The replacement is four checkable rules:

| Rule | Statement | Check |
|---|---|---|
| **L1 layers** | Every workspace crate declares a layer in `contrib/node/layers.toml` (0 base, 1 pure, 2 shared, 3 spv, 4 node, 5 app, test). A normal or build dependency edge may only point to the same or a lower layer; `test` may depend on anything. Exceptions are listed per edge with a reason (`dash-coreimport → key-wallet` for its `wallet` feature; `dash-conformance → dash-spv`) | `contrib/node/layers.py` (the rewrite of `r2_reach.py`) over `cargo metadata --all-features`; a missing layer entry fails; CI step in the `node-pure` and `spv` groups |
| **L2 pure** | Layer 1 keeps A1 X-1's bans (`contrib/node/pure-deny.toml`): no async runtime, sockets, storage, clocks, threads | unchanged `check-pure.sh` |
| **L3 mobile closure** | The dependency closure of `dash-spv-ffi` and `key-wallet-ffi` (default features, normal deps) contains no crate whose name starts with `dashd-`, no storage engine (`rocksdb`, `librocksdb-sys`, `rusqlite`, `libsqlite3-sys`, `sled`, `redb`, `heed`, `libmdbx`), no `jemalloc`/`mimalloc`, no `clap`, no `tracing-appender`; `dash-spv` the library may expose node-facing features but its **default** features must satisfy the same list | `contrib/node/mobile-deny.toml` + `check-mobile.sh` (cargo deny with the two FFI crates as roots); plus the **size gate** of §4.4 |
| **L4 hot-path types** | `dashcore` (and `key-wallet*`) may be a *direct* dependency only of: dash-spv, dash-spv-ffi, key-wallet-ffi, dash-mnlist's `sml-bridge` feature module, dashd-wallet, dashd-node (SPV-mode wiring), dash-coreimport (`wallet` feature), dash-conformance, dash-testkit, node-l1. `dash-p2p`, `dash-chain`, `dashd-chainstate`, `dashd-mempool`, `dashd-indexes`, `dash-consensus`, `dash-evo` never name a `dashcore` type | the existing `[bans] deny` entry for `dashcore` with the `wrappers` list edited to the set above; `r2_reach.py`'s reachability check is dropped (reachability is now expected) |

L4 is what keeps CS0's decode-once rule (A1 §1.3, DEC-51a) on the node: a block payload is decoded exactly once, by the consumer that asked for it, into that consumer's type (§2.4). The SPV adapter decodes into `dashcore::network::message::NetworkMessage` for dash-spv's managers; the chain decodes headers into `dash_consensus::primitives::Header`; the wallet seam converts hashes by reinterpretation (BS14's bridge, A1 §1.9) and blocks by re-decode only on the wallet-matched path, which is per matched block and never per connected block.

`deny.toml`'s current entries for `dash-spv`, `dash-spv-ffi`, `key-wallet-manager`, `dashcore-rpc*`, `node-l1`, `dash-conformance`, `dash-oracle-sys` are removed; L1 covers their intent.

### 2.4 One network loop

`dash-p2p` is P2a-core as designed (`P2a-outbound.md` §2.2: the shared `NodeTable`, `AddrMan` as a `Mutex<AddrManImpl>`, the actor as a plain thread with `fMsgProcWake` semantics, `initialize_node` synchronous, payloads copied, writers framing and encrypting, overlays pushing directly; §2.2.1's liveness graph; §2.3's modules; §2.4's API; §2.6's hooks) with these additions:

```rust
pub enum Profile { Full, Light }
pub struct LightOptions {
    pub max_outbound: usize,               // dash-spv ClientConfig.max_peers (default 8); Core: -maxconnections for outbound slots
    pub relay_txs: bool,                   // = enable_mempool_tracking; Core's fRelayTxes in `version` (v24:np:4060)
    pub persist_addrman: bool,             // peers.dat on desktop/rdashd --spv; false on mobile (no file)
    pub seeds: SeedPolicy,                 // DNS seeds + fixed seeds + dash-network-seeds' masternode seed files + configured peers
    pub headers_source: HeadersSource,     // CoreSequential | CheckpointSegments (dash-spv's pipeline; default on mobile)
    pub peer_selector: Arc<dyn PeerSelector>, // Core: AddrMan::Select; Light default: #902's latency-ranked supervisor
}
/// Light = Core's client shape: fClient (NODE_NETWORK not advertised; local services = NODE_HEADERS_COMPRESSED only, since a
/// Light node requests cfilters and never serves them), -listen=0, no inbound, -blocksonly unless relay_txs, no addr relay beyond getaddr.
```

| Concern | Full | Light |
|---|---|---|
| peer management | Core's `opencon`/`addcon`/`dnsseed`, anchors, feelers, 8 full-relay + 2 block-relay (P2a §3.8) | `opencon_step` with `max_outbound` slots, no feelers, no block-relay-only, no anchors; `PeerSelector` hook decides the candidate (`AddrMan::Select` or the ranked supervisor); `-connect`-style pinning for `restrict_to_configured_peers` |
| inbound serving | P2b (accept, getheaders serving), P4, P6b | none: no listener, `ConnMan::InitBinds` skipped; the policy table's serving rows return `Gate::Ignore` |
| dispatch policy | P2a's 74-row `CommandPolicy` table | the same table with a `light: bool` column: commands the Light profile neither sends nor accepts (`getblocks`, `mempool` served, `getcfilters` inbound, `govsync`, DKG, …) are `Gate::Ignore` and never decoded; Core's gates for the rest are unchanged |
| handshake | P2a §3.3 | identical code; `LocalIdentity` carries dash-spv's `user_agent` (DEC-83 honest UA either way) |
| headers | P2a §3.4 (`CoreSequential`: one sync peer, `getheaders2`, continuity, timeouts) | `CoreSequential` or `CheckpointSegments`: dash-spv's `HeadersPipeline` runs as a `NetHandler` that owns `headers`/`headers2` for the peers it drives, buffers segments, verifies each segment's end hash against its anchor, and submits in order through `ChainAccess::process_headers`; both sources end in the same acceptance |
| request scheduling | P3's block download; P5's object requests | the **broker** (#902): keyed dedup, classes, caps, timeouts, requeue; `NetHandler` for the response commands + an `OutboxScheduler` hook so `send_messages` for a Light node pops from the broker's class queues under the peer's cap |
| timers | stale-tip 45 s, pings, addr | pings and the headers timeout; stale-tip check only under `CoreSequential` |
| clock | `dash-clock` | the same (mockable in tests; iOS/Android system clock in production) |

The broker's `RequestKey` registry, `MsgClass` queues, `PeerPlan`, link-share and soft/hard timeouts move verbatim from #902's `manager.rs` into `dash-spv/src/network/broker.rs` (they are SPV policy, not loop mechanics); the loop gains two hooks: `OutboxScheduler::fill(peer, budget) -> Vec<OutMsg>` called from `send_messages` after Core's own sends, and `RequestTracker::{on_wire, answered, peer_lost}` called from the dispatcher and `finalize_node`. The `NetworkManager` adapter (`dash-spv/src/network/shared.rs`) implements #902's trait: `send`/`send_to` → broker enqueue or `NodeHandle::push_message`; `subscribe(kinds)` → a `NetHandler` registered for those commands that decodes the raw payload with `dashcore`'s codec and forwards `Inbound`; `events()` → connection-count and peer events from the socket task; `tip()` → the `NodeTable`'s best advertised `start_height`; `request_answered` → the tracker. Everything above the trait (`DashSpvClient`, the managers) is untouched.

**Inbound serving for the node vs none for mobile** is therefore a profile choice inside one `ConnMan`, not a second connection manager. The actor loop code is byte-identical between profiles; the per-pass shuffle, the 100 ms MN throttle and the wake semantics run on a phone exactly as on a server (the loop is idle most of the time with 8 peers).

### 2.5 One header/sync flow

```rust
// dash-chain (shared, sans-io except the store trait; A1 §2.10 + P2a §2.5.3 R1–R8)
pub struct Chain<S: ChainStore> { arena: Arena, index: ConcurrentHashIndex, snapshot: ArcSwap<ChainReadSnapshot>, bus: EventBus, store: S, anchor: Anchor, residency: Residency }
pub enum Anchor { Genesis, Checkpoint { height: u32, hash: BlockHash, chain_work: Arith256, header: Header } }
pub enum Residency { All, Windowed { hot_above: u32 } }     // Full: All. Light: hot_above = max(anchor, best CL − 2·DKG_INTERVAL)
pub trait ChainAccess { /* P2a §2.4: snapshot, lookup, entry, ancestor, locator, loading_blocks, min_chain_work, is_initial_block_download, process_headers */ }
pub trait IndexAccess { fn lookup(&self, h: &BlockHash) -> Option<BlockId>; fn ancestor(..); fn lca(..); fn height(..); fn have_data_pos(..); fn undo_pos(..); fn status(..) }   // CH2 §2.11
pub trait ChainSnapshot { fn tip(&self) -> BlockRef; fn at_height(&self, h: u32) -> Option<BlockRef>; }                                       // CH2 §2.9.1
pub enum TipDriver { Headers, External(Box<dyn FnMut(&HeaderAccepted) /* dashd-chainstate */>) }
pub trait ChainStore: Send + Sync {
    fn load(&self) -> Result<IndexLoad, StoreError>;            // Full: every BlockIndexRecord (hash-keyed); Light: the active chain by height
    fn persist(&self, w: &[IndexWrite]) -> Result<(), StoreError>;
    fn truncate_above(&self, height: u32) -> Result<(), StoreError>;   // Light only; Full stores branches and never truncates
    fn chainlock(&self) -> Result<Option<(u32, BlockHash)>, StoreError>; fn set_chainlock(..);
    // filter headers, filters, wallet-matched blocks, MN messages, metadata: the dash-spv per-store traits, re-homed here
}
```

- **Acceptance** is Core's `AcceptBlockHeader` (A1 CH1 row; `v24 validation.cpp:4598` `AcceptBlockHeader`, `:4469` `ContextualCheckBlockHeader`, the ChainLock conflict check at `:4693-4698` returning `bad-chainlock`), hashing once inline with `dash-x11` (DEC-51a; `par_iter` over a batch on the prevalidate pool in both profiles), with one addition for `Anchor::Checkpoint`: the first `DGW_PAST_BLOCKS` (24) headers above the anchor cannot be `nBits`-checked and the first 11 cannot be MTP-checked, so for those heights acceptance requires continuity, own-target PoW, and that the window's end hash equals the next sync anchor's hash when one exists; from anchor + 25 on every rule applies. This is the only place Light differs from Full in acceptance.
- **Checkpoints, two kinds.** Core's consensus checkpoints come from `dash-params` and are part of acceptance in both profiles. dash-spv's dense generated checkpoints (#375) are **sync anchors**: segment boundaries for `CheckpointSegments`, the start point for `start_from_height`/wallet birth, and the window-end pins above. They never reject a header the consensus rules accept, except inside the anchored window where they are the only rule.
- **Reorgs without restarts.** A header that makes a different branch the best (or a ChainLock that pins a branch) produces `Disconnected(b)` for each header leaving the active chain and `Connected(b)` for each joining it (F1 order). In Light mode `ChainStore::truncate_above(fork)` runs on V before the events are published (the store is height-keyed, so the stale tail must go first); the filter, block and MN subscribers truncate their own stores on `Disconnected`, and the wallet receives `truncate_above` (#1104) once per reorg with the fork height. FFI's `on_chain_truncated` fires from the same place it does today. `handle_fork`'s stop-truncate-restart is gone.
- **Residency.** `Residency::All` keeps every entry in the arena (Core keeps all 2.6M `CBlockIndex` in memory; A1 §3.8 plans 120–147 B per entry). `Residency::Windowed` keeps full entries above `hot_above` and, below it, only dash-spv's `HeaderHashIndex` (#1095; 4 B per header plus 4 MiB of group offsets) plus the sparse locator set; a `lookup` miss below the window reads the header from the store by height. Both branches and both profiles use the same arena code; on the node `hot_above = 0` and the cold path is unreachable. Numbers in §3.
- **Events** (`ChainEvent::{Connected, Disconnected}` with `seq`, plus `HeaderAccepted { id, is_best }`, `ChainLocked { height, hash }`): one bounded queue per subscriber (F3), `seq` assigned at the tip change before any wait (F2), control events out of band (F4), wakers (F5), round-robin over runnable subscribers on the `events` thread (F6). dash-spv's managers subscribe through a small driver that turns the stream into today's `SyncEvent`s (`BlockHeadersStored`, `ForkDetected`, `BlockHeaderSyncComplete`) so `sync/filter_headers`, `sync/filters`, `sync/blocks` and `sync/masternodes` do not change in the first migration step.
- **Filters** are a second-order flow on top of headers in both modes: the filter header chain is fetched (`getcfheaders`) in Light/`--spv` and computed by IX1 in Full. `FilterSource::{Peers, Local}` is the seam; dash-spv's `FilterHeadersManager` already validates the chain against the header chain, and that validator moves into `dash-chain::filters` so IX1's output is checked by the same code SPV trusts.

### 2.6 One MN-list / quorum / ChainLock / ISLock pipeline

The algorithms are `dash-evo`'s (EV2 `dml`, LQ1 `llmq`, LQ2 commitments, the CL/IS sign-hash and recovered-signature verification). The pipeline crate `dash-mnlist` binds them to two **list representations** and two **sources**:

| | Node (`BlockSource`) | SPV (`DiffSource`) |
|---|---|---|
| list type | `MnList` (DML, EV2 §2.2: `Dmn` with full state, immutable CHAMP order) | `SimplifiedMnList` (SML: `CSimplifiedMNListEntry` fields; the type `dashcore::sml::MasternodeList` is today, re-based on `dash-evo`'s `SmlEntry` under the `sml-bridge` feature until CV3) |
| how lists arise | `build_new_list_from_block` per connected block (EV2b), stored as `DmlDelta` in `EvoStore` (EV2 §2.10) | `mnlistdiff` applied to the base list (`apply_diff` over SML, today `dashcore::sml::masternode_list::apply_diff`), base from `qrinfo`'s four cycle lists or a stored list |
| trust | PoW chain + full validation | the diff's coinbase `CbTx` merkle-proven to the header, `merkleRootMNList`/`merkleRootQuorums` recomputed over the SML (`merkle_roots.rs`), quorum commitments' BLS signatures verified against members computed from the SML, ChainLocks as the fork anchor, Core checkpoints |
| members | LQ1 pure steps bound to `L = MnList, E = Arc<Dmn>` (LQ1 §2.6, CH7b's facade) | the **same pure steps** bound to `L = SimplifiedMnList, E = Arc<SmlEntry>`; `SnapshotRows` implemented from `qrinfo`'s `quorumSnapshotAtHMinus{C,2C,3C,4C}` instead of the stored rows; `MemberError::Store` maps to "missing work-block list, request it" (today's `missing_work_block_list_requests`) |
| commitments | LQ2 `verify` on the node's quorum index | the same `verify` over `qrinfo`/`mnlistdiff` `newQuorums` with members from above; the BLS scheme per quorum version (legacy or basic, as Core) |
| CL / IS | `verify_chainlock(cl, quorum)`, `verify_islock(is, quorum)` with `select_quorum_for_signing` over the node's scan cache | the same functions over the SPV scan (`rotated_quorums_of_cycle`, `masternode_lists_around_height`) |
| consumer | LK1 enforcement (`dashd-locks`), mempool conflicts, RPC | `ChainLockManager` (pending until lists reach the height, best CL persisted), `InstantSendManager` (wallet notification, TTL) |
| cross-check | `project_sml(MnList at h)` must equal the SML the serving path emits (EV4) and, in `-consensusxcheck`, the SML a `DiffSource` fed with our own served diffs reconstructs | dash-spv's suites against rdashd as the only peer (SPV-as-consumer, A1 §4.11) |

Requirements on LQ1 and EV2 that make this one pipeline: (i) LQ1's pure steps (§2.5 of LQ1) stay generic over the entry type with the accessor trait `MnEntry { pro_tx_hash, confirmed_hash, confirmed_hash_with_pro_tx, is_valid, mn_type, collateral_outpoint?, operator_key }`; the SML has every field the scoring and skip-list logic reads (`confirmedHash`, `isValid`, `nType`, `proRegTxHash`, `pubKeyOperator`), and LQ1 §2.6's provenance rule (members carry the entry they were taken from) holds in both bindings; (ii) `QuorumMemberSource` gets a second implementation, `SmlMemberSource`, in `dash-mnlist`, with `EffectSink` a no-op (the SPV never stores snapshots as effects; it receives them); (iii) EV2's `MnList` gains `project_sml(&self) -> SimplifiedMnList` (Core's `CDeterministicMN::to_sml_entry`, `v24 evo/deterministicmns.cpp:43-46`, and the list built from those entries at `:255-259`), the inverse direction never exists; (iv) EV4's `mnlistdiff` builder is the server side of the same SML codec `DiffSource` decodes. `dashcore::sml`'s types remain the public wire types for Platform consumers until CV3 converts them; the `sml-bridge` feature maps them to `dash-evo::sml` without copying (same field layout; `From` impls, a compile-time size assertion).

`MasternodeListEngine` keeps its public shape (`feed_qr_info`, `apply_diff`, `masternode_lists_around_height`, `rotated_quorums_of_cycle`, `missing_work_block_list_requests`, `height_of`, `validated_cycle_heights`) as a facade over `dash_mnlist::DiffEngine<C: IndexAccess>`; `dash-spv-ffi`'s `platform_integration.rs` and dashwallet-desktop keep compiling. The engine's persisted-message replay (#1073/#1091/#1081) becomes `ChainStore::{store_diff, store_qr_info, replay}`.

### 2.7 CH2's P1–P9, item by item

CH2 rev2 §10.1 (DEC-94 Q7) lists what the shared base must preserve. `dash-chain` is the `SubscriberFeed` producer in both profiles; `dashd-chainstate` keeps the node-only mechanics.

| # | Requirement | How SB0 meets it |
|---|---|---|
| P1 | F1–F9 | **F1** one tip writer (V) publishes `Connected`/`Disconnected` in tip-change order to every queue; **F2** `seq` is a V-local counter incremented at the tip change, before `publish` blocks on any full queue; `FlushCommit.tip_seq` is read from it; a subscriber turning Synced reads `seq_synced` under the hub lock; **F3** per-subscriber `ArrayQueue<Q>` for data events; a full queue makes V wait in its service loop (§4.4.1 rule 5 is V's loop in `dashd-chainstate`; in SPV mode the loop services only read queries, and the wait is the same wait); **F4** control events go through a separate unbounded channel and the hub mirror; `dash-chain` exposes `publish_control(id, Control)`; **F5** every enqueue and every control addressed to a feed calls its `SubWaker`; **F6** the `events` thread steps runnable subscribers round-robin and parks on a condvar; dash-spv's async subscribers are served through a waker that wakes a tokio task, which costs no thread while parked; **F7** `ChainStateFlushed` and `T_ack` are published by `dashd-chainstate` after B1 returns, through `publish_control`, never by `dash-chain`; **F8** `IndexAccess` is lock-free over immutable `prev`/`skip`/`height` (R1/R2); block and undo readability along the locator path is CH2's prune-lock rule, implemented by `dashd-chainstate` over ST2 files; `dash-chain` never deletes an entry; **F9** `best_block()` belongs to `SubscriberDriver`; `dash-chain` reports nothing per subscriber |
| P2 | one tip writer servicing `FlushDue` | V is one thread in both profiles, `dash-chain::Chain::run(cmds)`; `dashd-chainstate` installs its service loop (`FlushDue`, `PruneClears`, release, queries) as the V loop's `Service` hook; in SPV mode the hook is `NoFlush` and the empty-flush rule is vacuous (no coins) |
| P3 | flush order B1 → `ChainStateFlushed` → commits → prune selection → Bk/Bp, rewind gate, `T_ack` after B1 | unchanged, entirely inside `dashd-chainstate`; `dash-chain` only provides `IndexAccess`, the control channel and `take_dirty_index_records()` (the index records are part of B1 in Full mode; in SPV mode they are persisted by V through `ChainStore::persist` with `Commit::Buffered` and made durable by the node's periodic `sync_wal`) |
| P4 | prune locks from subscriber locators | unchanged (`SubscriberHub::{predicted,actual}_locks` over `IndexAccess`); SPV mode has no block files to prune |
| P5 | a `KvEngine` meeting §2.5.1's contract; each subscriber on its own engine and thread | `dashd-store` (RocksDB) in rdashd, both modes (§4); subscribers only exist in Full mode (P8), so the mobile segment store never has to meet the subscriber contract |
| P6 | one node-wide fail-stop latch | `GlobalFailStop` stays in `dashd-store`; `dash-chain` reports `StoreError` from `ChainStore` and V maps any error to the latch in rdashd; on mobile the same error reaches `ManagerError` → FFI `on_error` and the client stops (today's behaviour) |
| P7 | subscriber classes as Core's `m_synced`; Syncing catches up from a `ChainSnapshot` | `dash-chain` publishes `ChainSnapshot` (`tip`, `at_height`) as part of `ChainReadSnapshot` (R3/R8); the class machine is CH2b's |
| P8 | refuse subscribers needing undo on a node with no undo | `dash-chain` publishes `NodeCapabilities { blocks: BlockAvailability::{All, WalletMatched, None}, undo: bool }`; `SubscriberHub::register` returns `Err(Refused::NeedsUndo)` / `Err(Refused::NeedsBlocks)` when `needs` exceed the capabilities; rdashd `--spv -txindex` (or any index) fails at init with that error, never feeds blocks without undo |
| P9 | mempool coin access as V commands | Full mode only (`dashd-mempool` → `ChainCommand::{Fetch, HaveInCache, Uncache}`); SPV mode has no coins and no such commands; dash-spv's mempool tracker is a network subscriber and never addresses V |

The feed therefore gives F1–F9 in both modes; CH2's §4.4.2 argument needs no redo. Two things CH2 should note in its next revision: `ChainEvent` is constructed by `dash-chain` (it owns `seq`), and the `Control` channel is a `dash-chain` type CH2b publishes through.

---

## 3. The mobile budget

### 3.1 What the mobile build is

`dash-spv-ffi` (and `key-wallet-ffi`) for `aarch64-apple-ios`, `aarch64-apple-ios-sim` and `aarch64-linux-android` (plus `x86_64` simulators). The budget binds the **shared base's default features**: whatever `dash-spv` pulls in by default is what ships on phones. Baseline measured on this box (`/work/scratch/sb0-fable/sizes/summary.txt`, NDK r29 clang, API 24, workspace `release` profile = `panic = "unwind"`, no LTO, default codegen units):

| Artifact (aarch64-linux-android) | bytes | MB |
|---|---|---|
| `libdash_spv_ffi.so` today, unstripped | 20,006,944 | 20.0 |
| `libdash_spv_ffi.so` today, **stripped** | 12,551,712 | **12.55** |
| `libdash_spv_ffi.a` (staticlib, what iOS links) | 86,711,210 | 86.7 (link-time input; the app's final size is the stripped subset) |
| `libdash_spv_ffi.so` with `lto = "fat"`, `codegen-units = 1`, `opt-level = "s"` | 8,811,664 | 8.81 MB |

The last row is the free lever: it is a profile setting for the FFI crates' release builds, independent of SB0, and it is where the mobile size gate's baseline should be set (§3.4).

### 3.2 Cost of each shared-base choice

| Choice | Binary | Memory | Build/toolchain | Verdict |
|---|---|---|---|---|
| `dash-consensus` in the mobile closure (PoW incl. DGW/KGW `nBits`, X11, merkle; the script interpreter is never referenced from SPV code and is dropped by `--gc-sections`) | +0.18 MB measured (§4.2 row I, those four entry points referenced) | none | pure Rust; base-sdk crates already build for Android through the same recipe | **take** |
| `dash-x11` instead of `rs-x11-hash` for header hashing (XH1: 0.23× Core instructions; the C build script of `rs-x11-hash` needs `CARGO_NDK_SYSROOT_PATH`, the one Android build trap hit here) | pure Rust, ≈ the C code's size | none | removes a C dependency and cargo-ndk's env requirement | **take** (BS15 was already going to) |
| `dash-p2p` (actor, ConnMan, AddrMan, policy table, 74 rows of handlers, addrman serialization) | estimate 0.8–1.5 MB stripped (P2a's module list is ≈ 15–20k lines; comparable Rust code in dash-spv's `network/` is 5.7k lines today) | `NodeTable` 8 peers × (64 KiB read buffer + outbox) ≈ 1 MB; an in-memory AddrMan seeded from DNS/fixed/masternode seeds ≈ 0.5 MB; one actor thread (8 MiB virtual stack, resident ≈ 100 KiB) | tokio is already present | **take**; the profile gate keeps serving code out of Light builds only where it is a separate module (P2b/P4/P6b are node crates in layer 4, not features of `dash-p2p`) |
| `dash-chain` arena with `Residency::Windowed` | small | hot window ≈ 2 DKG intervals (1,152 entries × 128 B ≈ 150 KiB); cold index = #1095's `HeaderHashIndex` (one `u32` height per header packed by 4,096-value groups of the hash's first four bytes, plus 4 MiB of group offsets): ≈ 10 MB for a wallet born at height 1.0M (≈ 1.55M headers to the tip), ≈ 13 MiB for the whole mainnet chain (the commit's own figure, replacing a 150 MiB `HashMap`), ≈ 5 MB for a wallet born at 2.35M; two resident header segments 2.2 MB (#1086) | — | **take**; `Residency::All` would be 186 MB for the old wallet (A1's 120 B/entry), which is why windowing is mandatory on mobile |
| `dash-mnlist` on `dash-evo` (imbl CHAMP lists) | `imbl` ≈ 0.2–0.4 MB; `dash-evo` algorithms replace `dashcore::sml`'s, so net ≈ 0 | SML lists share `Arc` entries across heights (today) and CHAMP nodes (EV2); ≈ 4,000 entries × ≈ 200 B = 0.8 MB per distinct list, a few MB for the cycle window | — | **take** |
| BLS through `dash-pkc`/`blst` (already in the closure via `dash-pkc`) | 0 (already linked) | blst's pool spawns `cores` threads at first use (DEC-79): on a phone 4–8 threads, resident ≈ 1 MB | `blst` builds for Android/iOS (it is what the Platform SDK ships) | **take**; CV3 should plumb blst's `no-threads` for the mobile feature set (BL item, not SB0) |
| **RocksDB in the mobile build** | **+7.79 MB stripped on its own** (row E), +7.52 MB when linked with the FFI (row C), +6.78 MB without lz4/zstd (row D) | small at mobile scale: a 3-CF database with a 4 MiB cache and 4 MiB memtables costs 0.7 MiB RssAnon over process start at open, 9.5 MiB after 200k writes + flush, 6.9 MiB once reads settle (§4.2); memory is not the reason to reject | C++17 toolchain and `libclang` (bindgen) on every mobile CI runner; cargo-ndk sysroot plumbing; no iOS build verified on this box (no Xcode) | **reject** for mobile (§4) |
| `clap`, `tracing-subscriber` `env-filter`, `tracing-appender`, `serde_json` in dash-spv's default deps | ≈ 1–2 MB together (not measured; `tracing-subscriber` with `env-filter` pulls `regex-automata`) | — | — | out of SB0's scope but an easy win for the maintainers: the binary (`src/main.rs`) and the FFI should not share default features; a `cli` feature |

### 3.3 Feature flags and crate splits

- `dash-p2p`: features `light` (the Light profile, the `PeerSelector` hook, no serving tables) and `full` (default for rdashd). dash-spv depends with `default-features = false, features = ["light"]`; rdashd with both.
- `dash-chain`: no features; `Residency` is runtime. The `segments` backend is a module (`dash-chain::store::segments`, lifted from dash-spv's `storage/`), the `kv` backend lives in `dashd-chainstate` (it needs `dashd-store`).
- `dash-mnlist`: features `sml-bridge` (maps `dashcore::sml` wire types; default for dash-spv until CV3), `dml` (the node binding, pulls EV2's `MnList`; default for rdashd).
- `dash-spv`: `default = ["light-net", "segments"]`; `node-store` (the `StorageManager` over `KvEngine`; pulled by rdashd only); `cli` (clap, file logging) for the binary.
- `dash-consensus` is used by `dash-chain` with `default-features = false` (no script feature needed by headers); if the script module cannot be feature-gated without churn, `--gc-sections` already drops it (row I measures the actual delta).

### 3.4 The mobile gate (new CI step; §7 SB5)

1. `cargo ndk -t arm64-v8a build --release -p dash-spv-ffi` on every PR that touches layers 0–3 (recipe: the NDK env of §4.2, to be written as `contrib/mobile/android-env.sh`); `llvm-strip`; the stripped size must be ≤ the recorded baseline × 1.05, baseline re-recorded only by a PR that says why.
2. `check-mobile.sh` (rule L3).
3. An idle-RSS smoke on the Android emulator (`agentbox-api36`): start the client against a regtest dashd, sync 40,000 headers, read `RssAnon` of the process, compare against the recorded baseline ± 10 %. `dash-spv-bench`'s harness already measures sync; the emulator run is the only new piece.
4. iOS: `cargo build --release --target aarch64-apple-ios -p dash-spv-ffi` (key-wallet-ffi's `build-ios.sh` pattern) runs on pasta's Mac in the QA phase (DEC-84/89); until then the Android numbers stand in, and the design makes no choice whose iOS behaviour differs (no C++ dependency, no platform-specific storage API).

---

## 4. Storage: the recommendation against DEC-43

### 4.1 Decision

**DEC-43 stands for rdashd in both modes. The mobile build does not contain RocksDB.** The shared base's storage *contract* is one (`ChainStore`, §2.5, plus ST1's `KvEngine` for everything node-only); its *backends* are two: `dash-chain::store::segments` (dash-spv's height-keyed segment files, kept) for mobile and the desktop wallet, and `dashd-chainstate::store::kv` (the CH1/CH2 column families on `dashd-store`) for rdashd. Nothing in CH2 changes: `EngineTuning` (§2.5.1) stays RocksDB-tuned, `RocksTuning` stays isolated, `MemoryTuning` covers tests.

Why not one engine everywhere:

1. **RocksDB on mobile fails the size budget.** Rows E/C/D below: 7.79 MB stripped for RocksDB alone, +7.52 MB added to the FFI library. That is 60 % of today's 12.55 MB for a store whose access pattern on a phone is append-by-height and truncate-above, which the segment files already do in ≈ 1,300 lines.
2. **RocksDB on mobile fails the toolchain budget.** It needs a C++17 compiler per target, `libclang` for bindgen (or `bindgen-runtime` plus a `BINDGEN_EXTRA_CLANG_ARGS` sysroot per target, which is what `android-env.sh` does), ≈ 4 minutes of arm64 compile at 8 jobs, and an iOS build nobody on this program can run (DEC-84/89). Every one of those is a recurring cost on the maintainers' CI, for no feature.
3. **Memory is not the deciding argument, and the design says so.** The host probe (§4.2) measured 1.6 MiB RssAnon after opening a three-column-family database with mobile-sized settings (0.7 MiB over process start), 9.5 MiB after 200,000 writes and a flush, and 6.9 MiB once reads settle: a few MiB above the segment store's ≈ 5.4 MiB resident, not an order of magnitude. ST1 §9.4 OI-1/OI-6 (double caching, higher `RssAnon` in every cell) describe the node's 300 MiB regime and do not transfer to a store this small. The decision rests on reasons 1, 2, 4 and 5.
4. **A pure-Rust KV on both sides fails the node.** `redb` (row G: 958,720 bytes stripped, pure Rust, fits mobile) is a B-tree with a single writer and copy-on-write pages; its write amplification on random keys is MDBX's problem (ST1 §9.1: 4.3–6.7× disk, 2× time), and it has no column families or atomic multi-table batch with the durability modes CH2 needs. It would be a third engine for a mobile store that does not need a KV at all.
5. **One database for rdashd's two modes beats two.** `--spv` and full mode on one `dashd-store` instance share the block index, the MN-diff store and the CL marker, so the upgrade path (§2.2) is `-reindex-chainstate` over blocks the node then fetches, with no conversion step. A segment-backed `--spv` would need an import into RocksDB to go full.

What a future engine change would touch (the DEC-85 question, answered for the record): only `RocksTuning` and `dashd-store`'s engine module; CH2's protocol is re-verified by rerunning CH2b's CRASH suite on the new engine (CH2 §2.5.1), and `ChainStore`'s `kv` backend is engine-generic already.

### 4.2 Evidence

All builds: `aarch64-linux-android`, NDK 29.0.14206865, API 24, `llvm-strip`, workspace `release` profile unless stated. Rows B–H are a probe `cdylib` that links the named crates and exports one function per crate so nothing is dropped as unused; rows I use the integration worktree. NDK env: `CC_/CXX_/AR_aarch64_linux_android` and the linker set to the NDK's `aarch64-linux-android24-clang{,++}`/`llvm-ar`, `CARGO_NDK_SYSROOT_PATH=$NDK/sysroot` (`rs-x11-hash`'s build script needs it), and for RocksDB `LIBCLANG_PATH` plus `BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android="--sysroot=$NDK/sysroot --target=aarch64-linux-android24"`. Build logs per row are in `/work/scratch/sb0-fable/sizes/`.

| Row | Contents | unstripped | stripped | Δ vs. baseline |
|---|---|---|---|---|
| A | `libdash_spv_ffi.so`, `dev` 67b307c6 | 20,006,944 | 12,551,712 | baseline |
| B | probe: dash-spv-ffi only | 20,078,704 | 12,598,640 | +0.05 MB (probe overhead) |
| C | probe: dash-spv-ffi + rocksdb (`lz4`, `zstd`, as `dashd-store` configures it) | 30,979,088 | 20,069,712 | **+7.52 MB** |
| D | probe: dash-spv-ffi + rocksdb, no compression libs | 30,185,488 | 19,331,832 | +6.78 MB |
| E | probe: rocksdb (`lz4`, `zstd`) alone | 11,377,424 | 7,790,976 | RocksDB's own weight |
| F | probe: rocksdb alone, no compression libs | 10,583,656 | 7,053,016 | — |
| G | probe: `redb` 2 alone | 1,374,376 | 958,720 | the pure-Rust alternative's weight |
| H | probe: dash-spv-ffi + `redb` | 20,963,792 | 13,230,400 | +0.68 MB |
| I0 | probe: dash-spv-ffi from the integration worktree (base-sdk fork pins) | 20,126,968 | 12,669,096 | the integration baseline |
| I | I0 + `dash-consensus` (`check_proof_of_work`, `get_next_work_required`, `x11`, `compute_merkle_root` referenced; the rest dropped by `--gc-sections`) | 20,357,288 | 12,850,568 | **+0.18 MB**: what §2.5's acceptance costs |
| LTO | `libdash_spv_ffi.so`, `lto = "fat"`, `codegen-units = 1`, `opt-level = "s"` | 14,818,168 | 8,811,664 | −3.74 MB |

Resident memory of RocksDB with mobile-sized settings (x86_64 host: a dependency-free probe on the RocksDB C API linked against the integration worktree's debug-built `librocksdb-sys` 11.8.1 archive plus its lz4/zstd static libraries, because a fresh host compile of RocksDB did not finish on the loaded box; three column families, 4 MiB LRU block cache, 2 × 4 MiB memtables, partitioned index/filters with 10-bit bloom, lz4; 200,000 112-byte values written in 10,000-row batches and flushed, then 28,572 point reads; `RssAnon` and `VmRSS` from `/proc/self/status`, output in `sizes/rss.txt`):

| Point | RssAnon | VmRSS |
|---|---|---|
| process start | 0.9 MiB | 12.6 MiB |
| after `DB::open_cf` | 1.6 MiB | 15.6 MiB |
| after writes + flush | 9.5 MiB | 23.8 MiB |
| after reads | 6.9 MiB | 21.2 MiB |

Two control runs: with an 8 MiB cache and 8 MiB memtables the same workload reaches 18.4 MiB RssAnon after the flush and 11.6 MiB after reads; with only 20,000 rows at 4/4 it reaches 7.6 MiB and 4.6 MiB. `VmRSS` includes ≈ 12 MiB of file-backed pages from the probe's own 32 MB binary (unoptimised RocksDB objects), so `RssAnon` is the column to compare across engines; a release build's code pages are the §4.2 size rows, not this table.

For comparison, dash-spv's segment store holds two resident 10,000-header segments (2.2 MiB) and two 50,000-entry filter-header segments (3.2 MiB) and nothing else resident per store (#1086).

### 4.3 The segment store's obligations, and who meets them

Keeping the segment files is not free of obligations. Three of them, each a task row in §7:

- **Crash safety.** A segment is rewritten as a whole when evicted dirty (#1087, #1088 streams it). The shared base requires that a power loss mid-write never loses accepted headers below the segment being written. ST3a's `FaultyFlatFiles` model (adversarial unsynced pages) is pointed at `SegmentChainStore` (SB-ST): per-segment checksum trailer, write-to-temp-then-rename, and a load that truncates to the last intact segment (the SPV re-fetches the rest). This must hold before rdashd's desktop consumers rely on segments.
- **Branches.** The segment store persists the active chain only (§2.5). That is right for SPV (a ChainLocked history cannot reorg) and is why `Residency::Windowed` can read cold entries by height. The node's `kv` backend stores every entry.
- **Durability points.** `ChainStore::persist` is Buffered; the segment backend's "durable" is the rename. There is no `sync_wal`. CH2's subscribers are never on this backend (P5/P8), so the contract is enough.

### 4.4 Relation to CH2 §2.5.1

`EngineTuning` is unchanged. The one addition SB0 asks of CH2a a3: `RocksTuning::open_chainstate` must also open the **shared** column families `dash-chain`'s `kv` backend declares (`block_index` is already in `CHAINSTATE_SCHEMA`; add `chain_meta` for the anchor/CL marker, `filter_headers`, `filters`, `mn_messages`, `spv_blocks` under a schema version bump), so `--spv` and full mode open the same database with the same schema. The DEC-43 low-memory cell is unaffected; a new `--spv` cell (§7, SB4) compares rdashd `--spv` against dash-spv's own binary on the same wallet, the "`spv-headers-*` not slower" gate the CV5 row already carried.

---

## 5. Migration path

### 5.1 Principles

- **Every step lands behind a seam that already exists:** #902's `NetworkManager` trait and `DashSpvClient<W, N, S>`'s `N`; the per-store traits (`BlockHeaderStorage`, `FilterHeaderStorage`, …) and `StorageManager`'s `S`; the `MasternodeListEngine` facade. The managers, the wallet integration and the FFI are not rewritten.
- **Series, not a branch.** Each stage is one self-contained PR series on `node/integration`, written to `dev`'s conventions (conventional commits, `cargo fmt`/clippy `-D warnings`, the `typos` hook, the `spv`/`ffi` CI groups green), so it can be offered upstream the day DEC-87 allows (the stages give dash-spv the node's Core-exact header rules and delete duplicated code, which is the case to make to the maintainers). Until then integration rebases onto `dev` weekly and the stages stay rebased on the maintainers' work, never the other way round.
- **Nothing under `dash-spv/src/network/` until #902 merges** (A1 §2.24's rule, kept for exactly as long as #902 is open). The shared crates are new directories; the only dash-spv files the stages touch before the merge are none.
- **Behaviour first, then layout.** Each stage keeps the old implementation behind a feature for one release (`legacy-net`, `legacy-headers`) and runs both suites; the feature is removed in the following stage.

### 5.2 Stages

| Stage | Lands | Seam | dash-spv change | Tests that must stay green |
|---|---|---|---|---|
| 0 | #902 merges on `dev`; integration rebases (the #264 rebase pitfalls in memory apply to P1's lift: `Peer::connect`'s `TransportPreference`, single-accept mocks need `V1Only`, handshake leftover bytes) | — | theirs | `dashd_sync` 35, `dashd_masternode` 10, FFI `dashd_sync` |
| 1 | **P1** `dash-p2p-transport` (base-sdk framing, #264's state machine); dash-spv's `network/peer.rs` framing replaced by the transport | internal to `network/` | one file | the same, plus transport fuzz |
| 2 | **SB1** `dash-p2p` (P2a-core + Light + hooks); `dash-spv/src/network/shared.rs` implements #902's trait over `PeerManagerHandle` + broker; `dash-spv/src/network/broker.rs` is #902's scheduler, `git mv`-shaped so blame survives; `PeerNetworkManager` stays behind `legacy-net` | `NetworkManager` | adapter + move; `ClientConfig` unchanged (`max_peers` → `LightOptions.max_outbound`, `restrict_to_configured_peers` → pinned peers, `user_agent` → `LocalIdentity`) | the same on both `N`; M2's outbound transcripts for Full; a Light transcript set (handshake only, O13a's shrunken scope) |
| 3 | **SB2** `dash-chain` (= CH1-H) with the segment backend lifted from `storage/{block_headers,segments}.rs`; `BlockHeadersManager` becomes a driver (its `HeadersPipeline` is the Light `HeadersSource`); `handle_fork` replaced by events + `truncate_above`; `start_from_height` → `Anchor::Checkpoint` | `BlockHeaderStorage`, `SyncEvent` | `sync/block_headers/*` shrinks; `client/lifecycle.rs` loses `handle_fork`; a storage-version bump (#1084) adds the `chain.meta` record; existing stores migrate in place (the version-0 checkpoint header at the start height *is* the anchor) | the same; `test_disconnect_preserves_pipeline…`; the fork tests (#1102); new: `nBits`/MTP rejection vectors from Core (L0) in SPV mode |
| 4 | **SB3** `dash-mnlist` over `dash-evo` with the SML binding; `sml_engine.rs` becomes the facade | `MasternodeListEngine` API, `BlockHeaderStorage` → `IndexAccess` | `sml_engine.rs` shrinks to the facade; `sync/masternodes/*`, `sync/chainlock`, `sync/instantsend` unchanged; `dash-spv-ffi/src/platform_integration.rs` unchanged | `dashd_masternode` 10; the engine's replay tests (`storage/masternode.rs` tests); LQ1's fixtures under both bindings; the 11 regenerated rotated-quorum captures (LQ1b-F) |
| 5 | **SB4** rdashd `--spv`: `KvStorageManager<E: KvEngine>` implementing `StorageManager` in `dashd-chainstate::store::kv`; `dashd-node` constructs the loop and the chain, then `DashSpvClient` through the constructor that takes an external `N` (the one #902's mock tests use) with `S = KvStorageManager<RocksEngine>`; `--spv -txindex` refused per P8 | `StorageManager` | none | `dashd_sync` with `DASHD_IMPL=rdashd` (O16); the `spv-headers-*` cell (§7) |

### 5.3 FFI and library compatibility

| Surface | Status after the stages |
|---|---|
| The 41 exported `dash_spv_ffi_*` functions, `FFIDashSpvClient`, `FFIClientConfig`, every `config_*` setter, `FFISyncEventCallbacks`, `FFIProgressCallback`, peer callbacks, `platform_integration.rs` | unchanged; `set_start_from_height` sets the anchor; `set_masternode_sync_enabled` toggles the `dash-mnlist` subscriber; `set_user_agent` feeds `LocalIdentity`; `add_peer`/`set_restrict_to_configured_peers` map to pinned peers; `client_clear_storage` → `ChainStore::wipe` |
| `on_chain_truncated` (#1104) | fires from the reorg path of §2.5 with the fork height, as today |
| Storage directory | the segment files are the same files; `chain.meta` is added under a storage-version bump with an in-place upgrade; `clear_storage` semantics unchanged |
| `DashSpvClient<W, N, S>` (dashwallet-desktop, dash-spv-bench) | the type and the default aliases keep compiling; `N` defaults to the shared adapter after `legacy-net` is removed |
| `MasternodeListEngine` public methods | kept as the facade; `dashcore::sml` types remain in signatures until CV3 |
| Checkpoint files (#375) | read by `dash-chain` as sync anchors; unchanged format |
| Platform SDK consumers of `dashcore` | unaffected (L4 keeps `dashcore` the public type family; CV3/CV4 are still the conversion tasks, post-M4/M7) |

### 5.4 Coordination with #902 and the maintainers

1. Stage 0 waits for them; no SB PR is opened against `refactor/network-mod`, and nothing is commented or pushed there (the brief's rule).
2. The adapter and the broker move are the only dash-spv network changes in SB1; the adapter is a new file, the move keeps #902's tests (`a_requeued_request_still_de_duplicates`, `queue_drains_in_strict_priority_order`, …) verbatim.
3. Their post-#902 follow-ups to the managers are consumed by rebase; SB2's driver change to `sync/block_headers` is the one place a conflict is likely, so SB2's dash-spv commit is kept to the minimum (a driver file plus deletions) and the maintainers are shown it as a draft before it lands on integration.
4. ZocoLini's agent keeps #1136–#1138 (DEC-87); nothing here touches them.

---

## 6. Rework of the approved designs

| Design | What changes | What stays |
|---|---|---|
| **A1** | §1.1: R2 replaced by L1–L4 (§2.3); §2.1 item 8 reversed (the shared crates *are* dependencies of dash-spv; the FFI crates are untouched); §2.2/§2.3: `dashd-p2p` → `dash-p2p`, `dashd-clock` → `dash-clock`, `dashd-chainstate` split into `dash-chain` (shared) + `dashd-chainstate`; new `dash-mnlist`; a `node-shared` CI group (dash-p2p-transport, dash-p2p, dash-chain, dash-mnlist, dash-clock) and a `mobile` gate; §2.10: the header half, the index, snapshots, the event bus and the CL hook are `dash-chain`'s; `ChainCommand::ProcessHeaders` is answered there, `ProcessBlock` and the rest by `dashd-chainstate`; §2.14: the Light profile and the hooks of §2.4; "nothing changed under `dash-spv/src/network/`" becomes "until #902 merges"; §2.23: rewritten as §1.1's disposition column (reuse in place; replace nothing but the three extracted modules); §2.24: the R2 sentence → L1–L4 and the mobile gate; §3.1: a Light column (dash-spv's tokio runtime is `net`; `peerman` and `validation` are one thread each; no `db-writer`; `events` one thread; `prevalidate` is rayon as today); §4.11: SPV-as-consumer gains the SML projection cross-check; Appendix A: `ChainReadSnapshot` gains `tip` vs `best_header` semantics per mode, `NodeCapabilities`, `ChainStore` | the type stack (§1), the pure crates, the storage protocol, the verification architecture, the metric contract |
| **P2a** | crate name; `Profile`, `LightOptions`, `PeerSelector`, `OutboxScheduler`, `RequestTracker`, `HeadersSource` (§2.4); §2.5's neighbour table gains dash-spv (adapter) and `dash-chain` (replacing "CH1" as the implementer of §2.5.3 R1–R8, text unchanged); `dash-clock` delivered in phase A as planned; P2a-core/P2a-net split (DEC-56) stands; M2 exit transcripts unchanged for Full, plus Light handshake transcripts | the actor, ConnMan, AddrMan, policy table, gates, handshake, ping, addr, headers sync, eviction, misbehaviour, `sim` |
| **CH1** (brief not yet written) | split into **CH1-H** (`dash-chain`: arena, concurrent append-only index, `accept_header` with the anchored window, best header, `ChainReadSnapshot` R1–R8, `ChainEvent` bus meeting F1–F9 with the control channel, `IndexAccess`, `ChainSnapshot`, `ChainStore` + the segment backend lift, `Residency`, `Anchor`, the CL-conflict hook, PoW against stored hashes at load, `NodeCapabilities`) and **CH1-B** (`dashd-chainstate`: `ChainCommand` block path, `accept_block`, `ActivateBestChain`, the `TipDriver::External` registration, the `kv` backend, index records in B1); CH1-H is SB2 and gates CH2b | everything A1 §2.10 says about the block path |
| **CH2a/b/c** | CH2a: proceeds (DEC-94 Q6); a3 opens the shared CFs (§4.4). CH2b: held until SB2 lands; `ChainEvent`/`Control` become `dash-chain` types it publishes through; `SubscriberHub::register` takes `NodeCapabilities` (P8); `EngineTuning` unchanged. CH2c unchanged | F1–F9, P1–P9 (§2.7), the flush protocol, `RocksTuning` |
| **ST2 / ST3a** | ST2 unchanged (node only). ST3a unchanged; **SB-ST** adds the segment backend as a second `Sut` for `FaultyFlatFiles` (§4.3) | — |
| **EV0** | adds the `MnEntry` accessor trait LQ1's generics read, `MnList::project_sml` (signature only), and the `dash-evo::sml` type skeletons (`SimplifiedMnList`, `SmlEntry`, re-based on base-sdk types with `sml-bridge` `From` impls); `QuorumMemberSource`/`MemberError`/`Effect` unchanged | types-first, in-memory store, `EvoDelta` |
| **EV2a/b** | EV2a implements `project_sml` (Core `CDeterministicMN::to_sml_entry` and the SML build, `v24 evo/deterministicmns.cpp:43-46, 255-259`) and the SML `apply_diff` + merkle roots (lifted from `dashcore::sml::masternode_list::{apply_diff,merkle_roots}`); EV2b unchanged; EV4 builds `mnlistdiff` from `project_sml` | the DML model, CHAMP order, the payee queue, the store |
| **LQ1a/b** | the pure steps' generics are asserted by a second binding (`L = SimplifiedMnList`) in **LQ1c** (folded into SB3): `SmlMemberSource`, `SnapshotRows` from `qrinfo`, `EffectSink` no-op; the conformance suite runs both bindings on the same fixtures; LQ1b-F's dash-spv fixture files become shared vectors | member selection, modifier, skip lists, snapshots as effects, the scan cache, CH7b's facade |
| **merged node code** | `dash-consensus` becomes a dependency of `dash-chain` (its PoW, hashing, header codec; §4.2 row I is the cost); `dash-x11`/`dash-sha256` replace `rs-x11-hash` in dash-spv (BS15); `deny.toml` and `contrib/node/r2_reach.py` rewritten per §2.3; `.github/ci-groups.yml` gains `node-shared` and `mobile`; `dashd-store`, `dash-coreimport`, `node-l1`, `node-sink`, `conformance`, `test-vectors` do not move | — |

---

## 7. Plan revision

### 7.1 New and changed rows (PLAN §4 format: ID · scope · milestone · crates · deps · size · class · model per DEC-92 · acceptance)

| ID | Scope | M | Crates | Deps | Size | Class | Model | Acceptance / gate |
|---|---|---|---|---|---|---|---|---|
| **SB1** | `dash-p2p` Light profile, `PeerSelector`/`OutboxScheduler`/`RequestTracker`/`HeadersSource` hooks, the `NetworkManager` adapter and the broker move (§2.4, §5.2 stage 2); lands with P2a-core | M2 | dash-p2p, dash-spv | #902 merged, P1, P2a-core | M | P2P | impl O-h; rev Sol→O-h | dash-spv suites on `N = shared` and `legacy-net`; M2 outbound transcripts (Full) + Light handshake transcripts; no new dependency in the mobile closure |
| **SB2** (= CH1-H) | `dash-chain`: arena, concurrent index, `accept_header` (anchored window), best header, snapshots R1–R8, the bus (F1–F9 + control), `IndexAccess`/`ChainSnapshot`, `ChainStore` + segments lift, `Residency`, `Anchor`, CL hook, `NodeCapabilities`; dash-spv's headers driver | M2 | dash-chain, dash-spv | CS2, i:BS5, dash-x11 | L | STATE | D O-h (brief from this document); impl O-h; rev Sol→O-h | Core L0 header vectors reject in SPV mode (`nBits`, MTP, time, version, checkpoint); R1–R8 `sim` tests from P2a §2.5.3; F1–F9 tests from CH2 §4.4.3's model; fork tests without restart; PF0 counter gate on `accept_header` |
| **SB3** (incl. LQ1c) | `dash-mnlist`: `DiffEngine`, `SmlMemberSource`, `SnapshotRows` from `qrinfo`, CL/IS verification, the facade; `sml-bridge` | M3–M4 | dash-mnlist, dash-evo, dash-spv | EV0, EV2a, LQ1a/b | M | LLMQ | impl O-h; rev Sol→O-h | `dashd_masternode` 10; LQ1 fixtures under both bindings; the 11 rotated captures; replay tests; `project_sml` = diff-applied SML on regtest-mn and the testnet capture |
| **SB4** (= CV5) | rdashd `--spv`: `KvStorageManager<E>`, `dashd-node` wiring, P8 refusal, the `--spv` bench cell | M2–M3 | dashd-chainstate, dashd-node | SB1, SB2, ST1 | M | P2P/STATE | impl O-m; rev Sol | `dashd_sync` with `DASHD_IMPL=rdashd --spv`; `spv-headers-*` and a mainnet wallet-sync cell: rdashd `--spv` ≤ dash-spv binary on the same wallet, same peers (BG) |
| **SB5** | mobile gate: `contrib/mobile/android-env.sh`, cargo-ndk CI step, stripped-size budget, `check-mobile.sh` (L3), `layers.toml` + `layers.py` (L1), emulator idle-RSS smoke | M1 | contrib, CI | — | S | OTH | impl S-h; rev Sol | gate green on `dev` baseline; a planted `dashd-store` dependency in dash-spv fails L3 |
| **SB-LTO** | `[profile.mobile]` (fat LTO, 1 CGU, `opt-level = "s"`) for the FFI crates; the gate's baseline re-recorded | M1 | dash-spv-ffi, key-wallet-ffi | SB5 | S | OTH | impl S-h; rev Sol | size table row LTO reproduced in CI |
| **SB-ST** | segment-store crash model: checksum trailer, temp+rename, truncate-to-intact on load; ST3a `FaultyFlatFiles` as a second `Sut` | M3 | dash-chain | ST3a, SB2 | S | STATE | impl S-h; rev Sol→O-h | ST3a's explore/sweep on the segment `Sut`; a torn last segment never loses accepted headers below it |
| CV5 | **superseded by SB4** | — | — | — | — | — | — | — |
| P2a | crate renamed; +Light profile and hooks (SB1 is its second half); `dash-clock` in phase A | M2 | dash-p2p | +#902 merged | L (+2 wk) | P2P | unchanged | unchanged + SB1's |
| CH1 | split CH1-H (SB2) / CH1-B; CH1-B's brief depends on SB2's API | M2–M3 | dashd-chainstate | SB2 | L (+1 wk total) | STATE | unchanged | unchanged |
| CH2b | dep on SB2; `NodeCapabilities` at `register` | M3 | dashd-chainstate | +SB2 | unchanged | STATE | unchanged | unchanged |
| EV0 | + `MnEntry`, `project_sml` signature, `dash-evo::sml` skeletons | M1–M2 | dash-evo | — | S | CON | unchanged | + compile-only SML consumer |
| EV2a | + SML type, `apply_diff`, merkle roots, `project_sml` | M2–M3 | dash-evo::{dml,sml} | — | L | CON | unchanged | + `project_sml` round-trip against `dashcore::sml` on the testnet capture |
| LQ1a/b | generics asserted by LQ1c; fixtures shared | M3–M4 | dash-evo::llmq | — | unchanged | LLMQ | unchanged | unchanged |
| ND1 | + `--spv` init path, `NodeCapabilities`, refusal errors | M2 | dashd-node | +SB4 | +S | OTH | unchanged | unchanged |
| O13a | + Light handshake transcripts (protocol-parity rows only, DEC-83) | M2 | dash-conformance | SB1 | unchanged | P2P | unchanged | diff-empty per profile |
| O16 | `dash-testkit` runs the dash-spv suites against rdashd `--spv` and full | M2 | dash-testkit | SB4 | unchanged | OTH | unchanged | unchanged |
| IX1 | + `FilterSource::Local` for `--spv`-style consumers on a full node | M4–M5 | dashd-indexes | — | +S | STATE | unchanged | + the shared filter-header validator accepts IX1's chain |
| CV3/CV4 | unchanged (post-M4/M7); the `sml-bridge` makes them non-blocking for SB3 | | | | | | | |

### 7.2 Order, and what continues now

- **Continue now, unchanged:** CS-series, CH2a (DEC-94 Q6), ST3a, O2a/O3/PF0, base-sdk (BSX/BSX-PR), desktop, RP-V24, P1 (transport-only), **SB5** (can start today on Sonnet; it has no code dependency), **SB-LTO**.
- **Resume on approval of SB0 with a one-page addendum each:** EV0 (§6's three additions), P2a-core (SB1's hooks are additive; phase A can start before #902 merges because it does not touch dash-spv), SB2/CH1-H (the gating item; starts at once, it has no network dependency and uses the transport-less test harness).
- **Resume when their input lands:** SB1 (after #902 merges and P1), SB4 (after SB1 + SB2), CH2b (after SB2), SB3 (after EV0 + EV2a types; LQ1a in parallel), EV2b/LQ1b (their addenda; implementation after EV2a/LQ1a as planned), ND1 (after SB4's wiring is specified), O13a (after SB1).
- **Milestone effect:** M2's exit gains "rdashd `--spv` syncs a mainnet wallet through filters; the Light-profile dash-spv passes its suites on the shared loop; the mobile gate is green". M2 exit moves ≈ +3 weeks (P2a +2, CH1 split +1). M3–M8 are unchanged in content; M5's "SPV smoke" row is satisfied by SB4's cell. CV5's post-M7 slot is freed.
- **Reviews** per DEC-92: Sol first, then Opus high for SB1/SB2/SB3 (P2P/STATE/LLMQ critical); Sol only for SB4/SB5/SB-LTO.

---

## 8. Performance

**Rules that keep the node never slower.**
1. The node's hot paths are P2a's, CH1-B's and CH2's as designed; SB0 adds nothing on the block-connect path. The only new indirections are per accepted *header*: `TipDriver::External` (one call per `HeaderAccepted`) and the profile check in `accept_header`'s anchored-window branch, which is `anchor == Genesis` on the node and folds to a constant.
2. **Decode once.** The loop hands each payload to exactly one consumer: Core's handlers decode inside the gates (P2a's decode policy), the SPV adapter decodes only commands a subscriber asked for (`subscribe(kinds)`), and on a Full node with no SPV subscribers the adapter is not registered.
3. **No SPV abstraction on node data.** `ChainStore::segments` is never compiled into rdashd (feature `segments` off), and `KvStorageManager` is only constructed in `--spv`. The arena, the hash index and the snapshot are the same types in both profiles; `Residency::Windowed` is a branch on an index *miss* below `hot_above`, which is 0 on the node, so the branch is a predictable compare per miss and misses are rare (lookups are for hashes the node mostly has).
4. **The bus is CH2's bus.** Bounded per-subscriber queues; V waits only when a mandatory subscriber lags, as Core waits on `LimitValidationInterfaceQueue`; dash-spv's driver forwards and returns, so it never holds V.
5. **Gates:** PF0's counter gate on `dash-chain::accept_header` (instructions per header, compared with a build where `Profile`/`Residency` are compiled out; the delta must be noise); the M2 `headers-first` cells of node-bench unchanged; SB4's `--spv` cell against the dash-spv binary; the DEC-43 low-memory cell unchanged.

**Mobile performance, so the shared base does not regress dash-spv.**
- The shared kernel's `nBits`/MTP rules cost ≈ 1 µs per header on top of X11 (DGW is 24 reads of immutable entries); with `dash-x11` at 0.23× Core's instructions the batch is faster than today's `rs-x11-hash` path.
- The segment pipeline's parallel download is preserved as the Light `HeadersSource`; its throughput is unchanged (same code, submitting to `process_headers` instead of `store_headers`).
- The broker is #902's code; its scheduling behaviour on the shared loop is identical because the loop only asks it what to send next.
- Memory equals today's for old wallets when `Residency::Windowed` reuses #1095's `HeaderHashIndex` (the baseline is that structure plus two resident segments); it must not be larger (§3.4's RSS smoke).
- The large-wallet bottlenecks already on record (`drop_conflicted_transactions` full scans, the block re-download livelock, unbounded `rescan_committed_range`, BIP158 `match_any` re-sort, the ≈ 40 % false-positive rate at 345K scripts) are in dash-spv's filter and wallet code and are untouched by SB0; the shared base must not make `BlocksNeeded` slower (the broker's `Blocks` class keeps its priority).

**Risks.**
- The `peerman` actor is a plain OS thread with blocking waits inside a tokio-hosted mobile process: fine (dash-spv already hosts rayon threads), but the actor must never be driven from a tokio worker; the adapter uses channels only.
- A deep reorg in Light mode rewrites whole segments (≤ 1.1 MB per 10,000 headers); bounded and the same as today.
- `blst`'s thread pool on phones (DEC-79): unchanged from today's closure; CV3 plumbs `no-threads`.
- Two more threads on mobile (V, events): negligible; both park.
- RocksDB in `--spv`: header `IndexWrite` records and filter blobs are small sequential writes; the WAL sync cadence is the node's periodic `sync_wal` (no CH2 flush in `--spv`), so durability of the header chain is "last few seconds" and recovery re-fetches, as mobile does.

---

## 9. Open questions (only where undecidable here)

| # | Question | Recommendation |
|---|---|---|
| 1 | **Ownership and upstreaming of layer 2.** The program writes `dash-p2p`/`dash-chain`/`dash-mnlist`; the dash-spv maintainers own the managers above them. Does pasta want SB1–SB3 offered to `dev` as PRs before the program's upstreaming window (DEC-87), given they share the node's header rules with dash-spv and delete duplicated network and engine code, or kept local until the end? | Offer them early: they are maintainer-facing and the longer they wait the larger the rebase cost against #902-class work. DEC-87 would need a narrow relaxation for the three series |
| 2 | **rdashd `--spv` default networking profile.** Light (no inbound, 8 peers, no `peers.dat`) or Full networking with the SPV tip driver (serves headers, keeps addrman, more peers)? | Light by default, `-listen=1` opt-in; a desktop SPV node that serves headers is a product choice, not a correctness one |
| 3 | **Mobile `HeadersSource` default.** Keep `CheckpointSegments` (dash-spv's parallel pipeline) or move to Core's sequential sync once the shared loop exists? | Keep segments; it is the maintainers' feature and measurably faster on high-latency links; revisit with SB4's numbers |
| 4 | **dashwallet-desktop's store.** Segments (as mobile) or the node's kv backend (RocksDB, 7.8 MB, but one store for a future desktop full node)? | Segments now; it keeps desktop on the mobile closure and SB-ST makes segments crash-safe. Switch only if desktop grows a full-node mode |
| 5 | **iOS verification.** Nothing here can be built for iOS on agentbox; the design avoids every platform-specific choice, but the size and build claims are Android-measured until pasta's Mac runs `build-ios.sh` for dash-spv-ffi | Schedule it with the first QA-phase Mac access (DEC-84/89); until then SB5's gate is Android-only and says so |
