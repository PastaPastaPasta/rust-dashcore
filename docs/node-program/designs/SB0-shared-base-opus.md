# SB0: the shared base for dash-spv and rdashd (Opus design)

- **Status:** design proposal, Opus half of the SB0 dual design (DEC-81). Written without reading the Fable half. Not committed (DEC-17).
- **Date:** 2026-10-09.
- **Code baseline:**
  - `node/integration` @186cae03, which is dev 67b307c6 plus node crates. Paths without a prefix are relative to that tree.
  - PR #902 head a0e64364, read with `gh` and a local worktree only.
  - EV0 branch `node/ev0-evo-interfaces` @3a5cb631.
  - CH2 rev2 @76391f21 (`CH2:` line numbers refer to that revision).
- **Inputs:**
  - DEC-81…DEC-94, PLAN revision 2026-10-09;
  - A1, P2a, CH2 rev2 (§2.5.1, §2.9.1, §10.1), ST1, ST3a, EV2, LQ1, CS0;
  - ZocoLini's PRs #902, #1081, #921, #375, #1104.
- **Experiments:** in `/work/scratch/sb0-opus/`. §3.1 and Appendix A describe the method.
- **Citation keys:** `A1:`, `P2a:`, `CH2:`, `ST1:`, `ST3a:`, `EV2:`, `LQ1:`, `CS0:` are line numbers in those designs. `#902 x.rs:N` is a line in the PR head.

---

## 0. The recommendation on one page

**Shape: one kernel, two drivers.**
- The shared base is a set of crates that both products link:
  - a pure consensus layer;
  - a small shared runtime layer (network core, header chain, MN/quorum/lock state, storage contract and a mobile storage engine).
- Each product brings its own *driver* on top:
  - dash-spv brings an SPV sync driver: #902's request broker and its sync managers.
  - rdashd brings the Core-exact `PeerManager` and the validation thread V (P2a, CH1-node, CH2…).
- The shared layer owns everything that must be *one*:
  - sockets, transport, handshake, peers, addresses, bans;
  - header acceptance and fork choice (ChainLock-aware);
  - SML lists, quorum membership, ChainLock and InstantSend verification;
  - the storage contract and the fail-stop latch.
- Scheduling policy is a plug-in. Core's round-robin message handler is wrong for a phone, and #902's any-peer broker is wrong for a node. Forcing either onto the other product would break one of DEC-81's binding constraints: Core parity, never slower, or mobile.

**Decisions, numbered for reference:**

1. **Validation moves down, not up.** The shared kernel applies Core's header, CbTx-root, merkle and ChainLock rules in both modes, so dash-spv adopts the node's pure consensus code (`dash-consensus`, `dash-evo`), not the other way round. This is the first upstreamable stage. Current SPV-mode validation coverage: see PLAN §7 handling (CANDIDATE-SPV).
2. **New shared crates:**

   | Crate | What it is | Replaces |
   |---|---|---|
   | `dash-net` | network core | `dash-spv/src/network/` and P2a's ConnMan/AddrMan/BanMan |
   | `dash-chain` | header chain and chain events | the shared half of CH1 |
   | `dash-mnstate` | SML/quorum/CL/IS state with two producers | dash-spv's `sml_engine` and its CL/IS managers |
   | `dash-filters` | Dash BIP158, the second half of CV5 | |
   | `dash-kv` | the `KvEngine` contract and latch, split out of `dashd-store` | |
   | `dash-kv-sqlite` | the mobile and SPV engine | |
   | `dash-clock` | clock | P2a's `dashd-clock` |

   `dash-evo` splits into a shared part (SML, LQ1 algorithms, lock verification) and a node-only `dash-dml` (EV2).
3. **R2 is replaced by layer rules** that a script checks against `layers.toml`:
   - Pure and shared crates never reach `dashcore`, `dash-spv`, `dashd-*` or engines.
   - The mobile closure of `dash-spv-ffi` (Android and iOS targets) never reaches `rocksdb`, jemalloc, `dash-dml`, `dashd-*` or `clap`.
   - A stripped-size gate on `libdash_spv_ffi.so` (aarch64-linux-android) runs per PR.
   - Shared crates never call `process::abort`, never install globals and never spawn OS threads themselves.
4. **Storage, by role (DEC-43 re-ruled):**
   - The node's chainstate, indexes and subscriber DBs stay on **RocksDB** (DEC-43 confirmed for the node, CH2 `RocksTuning` unchanged).
   - The shared base and every SPV store (mobile and rdashd `-spv`) use **SQLite** behind the same `KvEngine` contract. SQLite is the system library on Apple, so it costs no binary there. It is already linked by Platform's Android SDK. It recovers to a prefix, which is stronger than DEC-62's subset. It honours `F_FULLFSYNC`.
   - Block files stay node-only flat files.
   - Evidence: §3.1 and §4. RocksDB adds 6.9–7.7 MB (+54–61 %) to today's 12.6 MB stripped Android `.so`, and its default configuration reserves 56 MiB of engine memory. SQLite adds nothing on iOS or inside Platform's Android SDK, which both already have it, and 1.1–1.8 MB to a standalone Android build. On the SPV workload SQLite needs about 4 MiB of memory; a RocksDB tuned for mobile needs 17–21 MiB and still reads more slowly (§3.1).
5. **One network loop:**
   - `dash-net` runs every connection: per-peer reader and writer tasks, bounded per-peer send and receive queues with Core's pause semantics, V1/V2 transport (P1), the shared handshake functions, AddrMan, BanList, dialing step functions and the optional listener.
   - A `Processor` pulls from the per-peer queues. The node's `PeerManager` (P2a, one OS thread) and dash-spv's `SpvRouter` (#902's broker semantics, fixed) are two processors over the same core.
   - Inbound is a node feature; mobile never enables it.
6. **One header flow:**
   - `dash-chain::HeaderChain::accept` runs the same rules for both products: PoW, DGW, MTP, future time, versions, checkpoints, chainwork best header, ChainLock-conflict rejection.
   - The arena is CH1's design with two additions: an anchored base (SPV starts at a checkpoint) and a bounded in-memory window (mobile keeps about 10k headers resident, not the ≈ 2.55 M of mainnet).
   - Download policy belongs to the processor: Core's per-peer `getheaders` in the node, checkpoint-segmented parallel download in SPV.
7. **One MN pipeline:**
   - `dash-mnstate` holds SML lists, quorums, rotation snapshots, the best ChainLock and InstantSend verification.
   - SPV feeds it from **verified P2P diffs**: CbTx partial-merkle proof → header merkle root; SML root → `merkleRootMNList`; quorum root → `merkleRootQuorums`; commitment BLS.
   - The node feeds it from **its own projection**: EV2 `MnList → SML` via EV4, plus mined commitments.
   - The node cross-checks by running the P2P producer as a shadow (always in CI and on test networks, opt-in on mainnet).
8. **CH2's P1–P9 are met.** `dash-chain` provides the generic feed (F1–F3, F5, F6, F8 ancestry). CH2's `SubscriberHub` in `dashd-chainstate` keeps F4, F7, F9 and the flush order. SPV mode refuses every block-needing subscriber (P8). §2.8 has the table.
9. **Migration without breaking mobile:**
   - The C ABI stays frozen: an event-translation layer replaces the 1:1 enum mirroring.
   - The Rust API changes only where #902 already changes it.
   - Upstreamable stages run in this order:
     - A: the shared consensus rules inside today's dash-spv;
     - B: shared header chain, MN state and SQLite store;
     - C: `SpvRouter` on `dash-net`, landed as #902's announced follow-up PR, never as a fork of it;
     - D: rdashd `-spv`.
10. **Plan:**
    - CV5 becomes the SB series (SB1–SB12, §7). CV3/CV4 are re-ordered behind it.
    - Continuing now: CS-series, CH2a (including a3 `RocksTuning`), ST3a, P1, O-tooling, BSX.
    - Unheld after SB0 with the edits in §6: EV0/EV2/LQ1 and CH2b.
    - P2a re-scoped as the node processor, after SB6.
    - CH1 splits into SB3 (shared) and CH1-node.

---

## 1. Current state

### 1.1 dash-spv today (`node/integration` @186cae03 = dev 67b307c6)

| Area | What exists | Where |
|---|---|---|
| Client | `DashSpvClient<W, N, S>`. The generics are partly fake: the `SyncManagers` alias pins `Persistent*Storage`, and the engine is typed on `PersistentBlockHeaderStorage` | `dash-spv/src/client/core.rs:27-34, 108-122` |
| Runtime | Everything runs on tokio multi-thread. There are 8 sync managers, one task each, plus a 100 ms coordinator tick. The `SyncEvent` broadcast bus has capacity 10000 and carries both chain-ordered data and progress | `sync/sync_coordinator.rs:30, 131-171`; `client/event_handler.rs`; `sync/sync_manager.rs` |
| Network | `PeerNetworkManager` (2313 lines): outbound only, v1 transport only (`Peer::connect` takes 3 args, no BIP324), `relay=false`, `start_height` 0. A reputation module exists. `GetHeaders` is ignored and nothing is served | `network/manager.rs:158-1980`; `network/peer.rs:79-122`; `network/handshake.rs:250-292`; `network/reputation.rs` |
| Header sync | Download is checkpoint-segmented and parallel (8 active segments, 30 s timeout). Current SPV-mode validation coverage: see PLAN §7 handling (CANDIDATE-SPV). | `sync/block_headers/pipeline.rs:20, 60-117`; `validation/header.rs` |
| Fork handling | `fork_height` detects a branch from a peer; `handle_fork` stops the managers, truncates storage and restarts | `sync/block_headers/manager.rs:185-201`; `client/lifecycle.rs:257-279`; `chain/chain_tip.rs` |
| Filters | BIP157/158. cfheaders come in batches of 2000 and are chained with `compute_filter_headers`. Filters are hash-checked against cfheaders with rayon; union-then-attribute matching; `match_any` still sorts per call | `sync/filter_headers/manager.rs:108-144`; `validation/filter.rs:40-115`; `sync/filters/manager.rs:884-1010`; `dash/src/bip158.rs:287, 330` |
| Blocks | Filter-matched blocks only, 8 per request. Unrequested blocks are dropped | `sync/blocks/sync_manager.rs:69-115`; `sync/blocks/manager.rs:128-164` |
| MN lists | `sml_engine` is private (since #1094). It keeps `BTreeMap<height, MasternodeList>` and verifies DIP-6 and DIP-24 quorum BLS. Rotation follows `dashcore::sml`; LQ1 is the Core-exact reference | `dash-spv/src/lib.rs:74`; `sml_engine.rs:57-62, 568-789`; `dash/src/sml/masternode_list/` |
| Quorum selection | `quorum_for_request` picks the quorum by minimum ordering hash | `dash/src/sml/masternode_list/quorum_helpers.rs` |
| ChainLocks | Verified against the cycle quorums, deferred until the lists reach the signing height (#1063). The best CL is kept as JSON metadata | `sync/chainlock/manager.rs` |
| InstantSend | Signature checked with quorum index from the request id; queue; handed to mempool and wallet | `sync/instantsend/manager.rs:136-246`; `validation/instantlock.rs:57-140` |
| Storage | Segmented flat files with 2 cached segments; temp file + `sync_all` + rename. JSON metadata. Background saves every 5 s. Opening reads every stored header to rebuild the hash index (`get_items(start..=tip)` materialises ≈ 2.55 M × 112 B ≈ 285 MB when headers start at genesis; mainnet height per the oracles on 2026-10-08). MN state is a raw message log replayed at startup. **No KV engine** | `storage/segments.rs:42-98`; `storage/io.rs`; `storage/mod.rs`; `storage/block_headers.rs:116-138`; `storage/masternode.rs`; `sync/masternodes/manager.rs` |
| Wallet seam | `WalletInterface` (key-wallet-manager), shared through `Arc<RwLock<W>>` with key-wallet-ffi | `key-wallet-manager/src/wallet_interface.rs:62-231`; `dash-spv-ffi/src/client.rs:382-397` |
| FFI | 43 `#[no_mangle]` functions; one tokio runtime per client. `FFI{Sync,Network,Wallet}EventCallbacks` mirror the Rust enums 1:1, so any event-enum change is an ABI change. `#[repr(C)] ClientConfig`. `clap` is a normal dependency of both the lib and the FFI crate. Devnet LLMQ overrides are process-global | `dash-spv-ffi/src/client.rs:40-110`; `callbacks.rs:268-285, 529-534, 988-996, 1079-1100`; `client/config.rs:3, 25-102`; `dash-spv-ffi/Cargo.toml:27`; `client/devnet.rs:91-105` |
| Size | 25,653 production LOC in `dash-spv/src` (network 4783, storage 3056, filters 2304). `dash/src/sml` is 3810. 172 transitive normal dependencies. dash-spv-ffi stripped on aarch64-android is **12.6 MB** (12,622,128 bytes, §3.1) | survey §10 |

### 1.2 What the shared base guarantees a mobile wallet

The shared kernel gives SPV mode the same validity rules as the node, as design properties of the shared crates:
- **Header chain** (`dash-chain`): Core's header rules (DGW/KGW `nBits`, MTP, future time, versions, checkpoints), best header by chainwork, and Core's ChainLock-conflict rule at header acceptance.
- **MN lists and quorums** (`dash-evo`, `dash-mnstate`): every diff proven to its header through the CbTx partial-merkle proof, the SML and quorum set checked against `merkleRootMNList`/`merkleRootQuorums`, commitment BLS, and only `Verified` quorums selectable for signing.
- **ChainLocks** (`dash-mnstate`, `dash-chain`): applied only to known headers; a lock for an unknown header waits until the header arrives.
- **Blocks and filters**: matched blocks checked against the header merkle root; each cfheaders batch anchored to the stored chain, plus a two-peer check at checkpoints.

Current SPV-mode validation coverage: see PLAN §7 handling (CANDIDATE-SPV).

### 1.3 PR #902: what it changes, where it helps and where it conflicts

**What it is.** A draft that was 93 commits behind dev as of 2026-10-09, with 24 conflicting files. The latest human review is CHANGES_REQUESTED, blockers 2 and 3 are still open by the author's choice, and there have been no pushes since 2026-09-10. It rewrites `dash-spv/src/network/` (15 files → 4) into a self-coordinating broker:

| Piece | What it does | Where |
|---|---|---|
| Facade trait | `NetworkManager` with `start`/`stop`/`send` (fire-and-forget, de-duplicated by `RequestKey`)/`send_to`/`broadcast`/`dispatch_local`/`request_answered`/`subscribe(&[MessageType])`/`events`/`tip`/`connected_count` | `#902 network/mod.rs:38-78` |
| Request registry | `RequestKey` registry `Queued → OnWire{peer,msg,last_progress}` | `#902 manager.rs:269-307` |
| Queues and router | 5 strict-priority queues; one router task doing serial `write_all` with a 5 s timeout; least-loaded routing against an adaptive per-peer cap | `#902 manager.rs:355-843` |
| Bandwidth control | BBR/Little's-law controller | `#902 manager.rs:988-1493` |
| Timeouts | 10 s; kick the peer and re-queue, with no attempt cap | `#902 manager.rs:1889-1996` |
| Supervisor | Picks peers by handshake latency and service time | `#902 manager.rs:1524-1760` |
| Peers | `PeerId{addr, connection}`; per-peer framed reader on a new `RawNetworkMessageCodec`; headers2 decompressed in parallel, in order | `#902 peer.rs:47, 633-797`; `dash/src/network/message.rs:677-760` |
| Inbound pump | One unbounded channel → one pump → fan-out over a **closed** set of 10 `MessageType`s. `notfound`, `reject`, `getdata`, `getheaders` and the rest are dropped | `#902 manager.rs:203, 1999-2195` |
| Deleted | Reputation and bans, `storage/peers.rs`, `DownloadCoordinator`, `disconnect_peer` | |
| Unchanged | FFI C ABI. The Rust `network` module goes private | |

**Where its direction helps the shared base.**
- The facade between sync managers and the network is small and declarative, and its `MockNetworkManager` honours the de-dup contract. That is the right interface for dash-spv's sync layer, and **SB0 keeps it as the SPV processor's API** (§2.5).
- `PeerId{addr, connection}` is Core's `NodeId` semantics; `dash-net` adopts it.
- The framed reader, the transport/consumer split, per-peer DIP-25 upgrade and ordered `headers2` decompression are all protocol-correct for both products.
- Measured per-peer capacity is a useful input to the SPV scheduler and optional for node block download.

**Where it conflicts with a node or with SB0, and what SB0 does about it.**

| #902 property | Problem | SB0 resolution |
|---|---|---|
| One router task with a serial `write_all` under a 5 s timeout | One slow peer stalls every send. Unusable at 125 peers | Per-peer writer task with a bounded byte queue (`dash-net`) |
| One unbounded `PeerEvent` channel feeding one pump | No backpressure, no per-peer fairness, cross-type order per peer lost | Per-peer bounded receive queue with pause; processors pull |
| Closed `MessageType` set; `notfound` dropped | A 10 s timeout and a peer kick on every `notfound`; the node needs exhaustive dispatch | Exhaustive raw-command registry; the SPV registry consumes `notfound` and cancels its keys |
| Retry = kick the peer and re-send, no attempt cap (review blocker 3) | Kick loops | Per-peer request ledger with attempt caps and explicit cancel |
| No misbehaviour or bans (review blocker 2, deferred "for a next PR") | Bad headers, filters, quorums and locks go unpunished | Shared `BanList` plus `NetCore::discourage`; SPV and node scoring rules |
| Discovery is seeds plus one-shot sequential DNS into an unbounded `Vec` | Weak bootstrap; no persistence or filtering | Shared AddrMan (P2a §3.7) with DNS and fixed seeds |
| `send_to` of `getheaders` charges an in-flight unit that no key releases (my reading of `#902 peer.rs:232-286`, `manager.rs:575-595`) | A healthy peer can get kicked | The ledger charges only keyed requests |

The C ABI is untouched, and the Rust API breaks are already accepted by #902's author (§5.1). SB0 adds no Rust API break beyond them.

**Coordination stance.**
- dev is the baseline that must keep working. #902 is a direction, not a dependency of stages A and B.
- Stage C (`SpvRouter` on `dash-net`) is shaped as exactly the "next PR" the author announced for misbehaviour and peer persistence. It reuses #902's facade and broker semantics, so whichever lands first, the other is a mechanical rebase.
- No SB0 work edits `#902`'s files before #902 merges or the maintainers ask for it (open question Q1).

### 1.4 The other open PRs

| PR | What it is | Relation to SB0 |
|---|---|---|
| #1081 (draft, one commit) | Seeds `validated_cycle_heights` from the replayed engine, so a restart avoids a QRInfo | Compatible. `dash-mnstate` persists the verified state, which makes the replay, and with it #1081's mechanism, unnecessary at stage B. Until then it is a good interim fix |
| #921 (stale) | An older #902 snapshot plus a header and filter-header *backfill* below the anchor | The idea survives as `HeaderChain::lower_anchor` (§2.6), to be designed when a product needs it |
| #375 (stale) | Binary checkpoint files | Orthogonal. SB0's anchored arena needs the 24 headers below an anchor, which it fetches and links back to the checkpoint hash, so checkpoint data does not grow |
| #1104 (open, mergeable, blocked) | Wallet `truncate_above` with a ChainLock floor; FFI `on_chain_truncated` (a layout change) | Defines how reorgs reach the wallet. SB0 keeps it as the wallet-facing rule. The CL floor becomes a header-chain rule too (§2.6), so the wallet floor never fires on a header chain SB0 accepted |

### 1.5 The node side today

- **Merged node code:**
  - `dash-consensus`: pure. Header type with one X11, `ChainAncestry`, `get_next_work_required`, `compute_merkle_root`.
  - `dashd-store`: the `KvEngine` contract, `RocksEngine`, `MemoryEngine`, flatfile.
  - Also `dash-coreimport`, `dash-x11`, `dash-sha256`, `node-sink`, `node-l1`.
- **Not merged:** EV0 (`dash-evo`), ST3a (`dashd-store::faulty`), CH2a WIP (`dashd-chainstate` coins layer).
- **No node crate depends on `dashcore` or `dash-spv`**, and vice versa. The two stacks share only base-sdk (via `dashcore-crypto → dash-pkc`) and `secp256k1`.
- **Platform gaps:**
  - `dashd-store` does not build on Apple or Windows: unconditional `std::os::unix` at `flatfile/fs.rs:10-11` and `libc::posix_fallocate` at `fs.rs:246`.
  - Its trait docs still claim prefix durability (`engine.rs:32, 101-102`).
  - `KvBatch` lacks `delete_range` (BL-38, `engine.rs:64-76`).
- **Statements DEC-81 overturns.** A1 and P2a assume two stacks and one loop. 22 places say so: A1:22-26, 126, 136, 139, 147, 413-416, 468, 503-515, 551-554, 572, 618, 760-764, 1223-1224, 1430, 1592-1608, 1616, 2086, 2527, D1/D8/D44/D49, 2949-2950; P2a:46, 92-105, 121, 145-150, 169, 728.
- **Places that conflict with mobile:**
  - the thread model (A1:1685-1712; P2a:150);
  - RocksDB (A1:833);
  - jemalloc (A1:1970);
  - header acceptance only inside V (A1:962-1140);
  - the full-node datadir (A1:886-961);
  - the only SPV performance commitment being "not slower" (A1:2008).

  §6 disposes of each.

---

## 2. Layering

### 2.1 Principles

**S-1. Share decisions, not schedules.** Anything that decides validity or identity is written once and linked by both products:
- header rules and fork choice;
- SML content and quorum membership;
- CL/IS signatures;
- the handshake;
- ban and discourage state;
- the storage contract.

Anything that decides *when* work happens is a plug-in owned by the product: message-processing order, request scheduling, threads, flush policy. Core's schedule is part of the node's parity and is useless on a phone. #902's schedule is right for a phone and wrong for a node.

**S-2. The consensus and shared layers speak base-sdk.**
- They use `dash-consensus`, `dash-evo` and base-sdk types (A1 R1, R3, R4 and R5 stay).
- `dashcore` stays at the product edge only: dash-spv's wallet glue, the FFI, dashd-wallet.
- R2's first reason (dashcore's shapes are wrong for consensus, A1:161) still holds, so it moves from "node crates" to "everything below the products".

**S-3. Mobile binds the shared layers. The node binds only itself.**
- A shared crate must build for aarch64-linux-android and aarch64-apple-ios, link no C++ engine, install no global allocator, spawn no OS threads of its own and never abort the process.
- A node crate may do all of these (RocksDB, jemalloc, V and W threads, `AbortNode`).

**S-4. Sans-io where state lives.**
- `dash-chain` and `dash-mnstate` are synchronous state machines owned by one writer and read through published `Arc` snapshots (CH1 R8, DEC-55).
- The owner is V in the node and the SPV driver's task in dash-spv.
- The only tokio-bound shared crate is `dash-net`, because sockets are tokio in both products already (A1:1228-1231; dash-spv).

**S-5. No SPV abstraction on the node's hot path.**
- Node processors and V call shared code through concrete generic types (monomorphised), never through `async_trait` or `dyn` per message or per header.
- §8 lists the checks.

### 2.2 Crate map

Layers:

| Code | Layer | Bound by |
|---|---|---|
| **B** | base-sdk | |
| **P** | pure | X-1 purity |
| **S** | shared runtime | S-3 |
| **N** | node runtime | |
| **W** | legacy wallet stack (`dashcore`, key-wallet*) | |
| **A** | products and binaries | |
| **T** | tests and tools | |

| Crate | Layer | Owns | Comes from | Mobile? |
|---|---|---|---|---|
| base-sdk `dash-primitives`, `dash-p2p-core`, `dash-params`, `dash-pkc`, `dash-pow`, `dash-num`, `dash-types`, `dash-script` | B | types, codecs, params, BLS/ECDSA | fork `node/base-sdk` (R1) | yes (already linked through `dashcore-crypto`) |
| `dash-consensus` | P | `Header` (one X11), `Tx`, `Block`, header/tx/block rules, DGW/KGW, merkle, `ChainAncestry`, `BlockId`, `CheckpointView` | merged | **yes** (new for mobile, §3) |
| `dash-x11`, `dash-sha256` | P | hashing back ends | merged | yes (aarch64 paths, DEC-61) |
| `dash-evo` (shared core) | P | `sml` (EV4 core: SML entry, entry hash, SML list with quorums, `merkleRootMNList`/`merkleRootQuorums`, CbTx partial-merkle check), `llmq` (LQ1 algorithms over `MnListView`), `locks` (CL/IS request id, sign hash, signer selection, verify), `bls`, `ctx`, `effect` | EV0 + LQ1 + EV4 core + LK1 verification part | **yes** |
| `dash-dml` | P | EV2 `MnList`/`Dmn`, build/process/undo, payee, PoSe, collateral, canonical codecs, `EvoStore`/`resolve_list`, credit pool, MNHF, `impl MnListView for MnList`, the `MnList → SML` projection | split out of EV0's `dash-evo::dml` | **no** (node only; `imbl`) |
| `dash-llmq`, `dash-governance`, `dash-coinjoin` | P | DKG, governance, CoinJoin | A1 | no |
| `dash-filters` | P | BIP158 GCS codec, Dash element set (special-tx elements, X11-keyed SipHash), filter-header chain | `dash/src/bip158.rs` + A1's `dashd-indexes::filter` (CV5 second half) | **yes** |
| `dash-p2p-transport` | S | V1/V2 framing, BIP324 handshake, short IDs (P1) | P1 | **yes** |
| `dash-clock` | S | mock, adjusted, steady and system time | P2a's `dashd-clock` (P2a:476-478) | yes |
| `dash-kv` | S | `KvEngine`/`KvRead`/`KvBatch` (+`delete_range`), `Commit`, `GlobalFailStop`, `GuardedEngine`, `AbortSink`, `DbSchema`, `Table`, codecs, `MemoryEngine`, `faulty` (ST3a, test feature) | `dashd-store/src/{engine,fail_stop,schema,table,codec,memory,error}.rs`; CH2 §2.10 latch types | **yes** |
| `dash-kv-sqlite` | S | SQLite engine under the `KvEngine` contract (§4.4) | new | **yes** |
| `dash-net` | S | connection table, per-peer reader/writer tasks and queues, handshake functions, gates, AddrMan, BanList, dialing step functions, DNS/fixed seeds, optional listener and inbound eviction (`inbound` feature) | dash-spv `network/` + #902 peer/codec + P2a §2.3/§3.6-3.8 | **yes** (`inbound` off) |
| `dash-chain` | S | `HeaderChain` (arena, hash index, acceptance, best header, CL floor, anchor, window), `ChainReadSnapshot`, `IndexAccess`, chain-event feeds | shared half of CH1 + P2a's `ChainAccess` read side | **yes** |
| `dash-mnstate` | S | SML lists, quorum set, rotation snapshots, best CL, pending CLs, ISLock cache, the P2P producer and its verification, the projection input, the shadow cross-check | dash-spv `sml_engine`, `sync/{masternodes,chainlock,instantsend}` core logic, `storage/masternode.rs` | **yes** |
| `dashd-store` | N | `RocksEngine` + `RocksTuning` hooks, flatfile block/undo files | merged (shrinks) | no |
| `dashd-chainstate` | N | V, `ChainCommand`, block-data status on the shared arena, connect/disconnect, CH2 coins/flush/recovery/`SubscriberHub` | CH1-node + CH2 + CH3… | no |
| `dashd-p2p` | N | `PeerManager` processor (P2a actor loop), Core misbehaviour scoring, serving, block download (P3), tx relay | P2a/P2b/P3/P4/P5 | no |
| `dashd-mempool`, `dashd-mining`, `dashd-masternode`, `dashd-quorums`, `dashd-locks`, `dashd-governance`, `dashd-coinjoin`, `dashd-indexes`, `dashd-wallet`, `dashd-rpc`, `dashd-node` | N | as A1:570-584 (`dashd-locks` now *uses* `dash-evo::locks` and `dash-mnstate` for verification and state) | A1 | no |
| `dashcore`, `key-wallet`, `key-wallet-manager`, `*-ffi` (wallet) | W | wallet, Platform compatibility types | existing | yes |
| `dash-spv` | A | the SPV driver: `SpvRouter` (processor), the 8 sync managers (now adapters over `dash-chain`/`dash-mnstate`), SPV store schema, wallet glue, mempool (BIP37/fetch-all) | existing, re-layered | yes |
| `dash-spv-ffi` | A | C ABI, event translation layer (§5.1) | existing | yes |
| `rdashd`, `rdash-cli`, `rdash-wallet` | A | binaries; `rdashd -spv` embeds dash-spv's client (§2.4) | A1 | no |
| `dash-conformance`, `dash-testkit`, `node-fuzz`, `node-bench`, `dash-oracle-sys`, `node-l1`, `node-sink` | T | as today | | no |

Two crate splits are worth their cost:
- **`dash-evo` / `dash-dml`.** Mobile never compiles EV2. The layer rule can name the forbidden crate instead of a feature, and EV0 is unmerged, so the split costs no migration.
- **`dash-kv` / `dashd-store`.** The contract becomes shared while RocksDB and flatfile stay node-only. Run it as a mechanical move *after* ST3a merges, so ST3a's branch (`dashd-store/src/faulty`, 9.4k LOC) does not conflict.

Everything else is a feature at most. `dash-net/inbound` (listener, inbound eviction, serving gates) is off in dash-spv.

### 2.3 Dependency rules that replace R2

R2 (A1:147) is withdrawn, together with its enforcement:
- `deny.toml:48-80`, the entries whose `reason` contains "A1 R2";
- `contrib/node/r2_reach.py` and `check-r2.sh`;
- A1:448, 492 and 628.

These rules replace it. All of them are checked in CI, and every workspace crate must be listed in `contrib/node/layers.toml` with exactly one layer. An unlisted crate fails the check, the same closure idea as `r2_reach.py`'s first check.

| Rule | Statement | Enforced by |
|---|---|---|
| **L1 purity** | P crates: unchanged X-1 (no I/O, threads, clocks, globals; no HashMap iteration) | `check-pure.sh`, `pure-deny.toml`, `pure-clippy.toml` (unchanged; `dash-dml`, `dash-filters` added to the list) |
| **L2 direction** | B ← P ← S ← N ← A, and W ← A. P may depend on B and lower P. S on B, P and lower S. N on B, P, S and N. A on anything. T on anything. **Nothing in B, P or S depends on W, N, A or T** (the successor of R2: `dashcore`/`key-wallet*`/`dash-spv` never reach the consensus or shared layers) | `check-layers.py` over `cargo metadata --all-features`: closure and reachability, like `r2_reach.py:58-87` |
| **L3 W exceptions** | Only these may reach W: `dash-spv`, `dash-spv-ffi`, `masternode-seeds-fetcher`, `dashd-wallet`, `rdash-wallet`, `dashd-node` and `rdashd` (through `dash-spv` for `-spv`, through `dashd-wallet` for the wallet), `dash-coreimport` (`wallet` feature), and T crates | `layers.toml` `allow_w = [...]` |
| **L4 mobile closure** | The normal-dependency closure of `dash-spv-ffi` and `key-wallet-ffi` with default features, for `aarch64-linux-android` and `aarch64-apple-ios`, contains none of: `rocksdb`, `librocksdb-sys`, `leveldb-sys`, `libmdbx`, `tikv-jemallocator`/`tikv-jemalloc-sys`, `dash-dml`, `dash-llmq`, `dash-governance`, `dash-coinjoin`, any `dashd-*`, `clap`, `tracing-appender` | `contrib/node/check-mobile.sh`: `cargo deny --manifest-path dash-spv-ffi/Cargo.toml --target aarch64-linux-android --target aarch64-apple-ios --exclude-dev check bans --config contrib/node/mobile-deny.toml` |
| **L5 size gate** | Stripped `libdash_spv_ffi.so` (aarch64-linux-android, release) may not grow by more than 2 % or 256 KiB per PR (the smaller) without a recorded ruling. The baseline is in `contrib/node/mobile-size.txt` | `check-mobile.sh` builds with the NDK recipe of App. A and compares |
| **L6 no abort, no globals** | S crates: clippy `disallowed_methods` bans `std::process::{abort, exit}`, `std::thread::spawn`/`Builder`, and setting a `#[global_allocator]`. Statics hold no network parameters (fixes `client/devnet.rs:91-105`). Fatal storage errors go through `dash-kv`'s `GlobalFailStop` and the host's `AbortSink` | `shared-clippy.toml` via `check-shared.sh` |
| **L7 one codec below the products** | B, P and S decode only with base-sdk/`dash-consensus`. `bincode` is banned in B, P, S and N (as A1:628) | `layers.toml` bans |
| **L8 one owner per command** | Each processor's registry maps every P2P command to exactly one handler or an explicit `Ignore`, checked by a unit test over the command list (P2a:714-726 for the node; new for SPV) | tests in `dashd-p2p` and `dash-spv` |

R2's six reasons (A1 digest §A.4.3) under these rules:

| R2 reason | What happens to it |
|---|---|
| 1. Wrong shapes for consensus | Kept, by L2 and L7 |
| 2. Platform kept off the critical path | Kept: W is below no consensus code |
| 3. Mobile stability | Becomes L4, L5 and L6, which is stronger than R2 because it is measured |
| 4. #902 conflicts | Handled by staging (§5) rather than by a ban |
| 5. dash-spv's network is SPV-shaped | Handled by the processor split (§2.5) |
| 6. Codec boundary | Kept, by L7 |

### 2.4 Where SPV mode stops and full validation plugs in

| Concern | Shared base (both modes) | SPV mode stops at | Full mode plugs in at |
|---|---|---|---|
| Peers | `dash-net`: connections, transport, handshake, AddrMan, BanList, dialing | `SpvRouter`; outbound only; `max_peers` 3–8; services from `LocalIdentity::spv()` (`NONE \| HEADERS_COMPRESSED`, `relay=false` unless mempool tracking) | `PeerManager` (P2a) and the `inbound` feature (P2b); 8+2+1 outbound slots; `NODE_NETWORK`… |
| Headers | `HeaderChain::accept` (all contextual header rules, CL floor, chainwork) | best header = the SPV active chain; header-only arena status | V adds `HAVE_DATA`/`VALID_TRANSACTIONS`/`VALID_SCRIPTS` on the same arena entries. Active chain = V's connected chain (CH1-node, CH3b) |
| Blocks | `dash_consensus::{decode_block, compute_merkle_root}` | filter-matched blocks only; merkle root vs header (new); wallet processing | P3 download, `CheckBlock`/`ContextualCheckBlock`/`ConnectBlock` (CS*, CH3), block files (ST2) |
| Filters | `dash-filters` (GCS, element set, filter-header chain) | cfheaders anchored per batch; filters checked and matched | `dashd-indexes` blockfilterindex builds; P6b serves |
| MN list | `dash-evo::sml`, `dash-mnstate` | P2P producer: verified mnlistdiff/qrinfo | projection producer: `dash-dml` per connected block (CH7a, EV4); shadow P2P producer cross-checks |
| Quorums | LQ1 algorithms; commitment BLS verify | members over the SML adapter; rotation from qrinfo snapshots | mined commitments (CH7b), `llmq_S` rows; DKG (`dashd-quorums`) |
| ChainLocks | `dash-evo::locks` verify; best CL; CL floor in `HeaderChain` | accept, verify, apply floor | `dashd-locks` Core-exact: relay, `EnforceBestChainLock` in V, signing |
| InstantSend | verify; ISLock cache | wallet and mempool notification | `dashd-locks` + `dashd-mempool` conflict handling |
| Mempool | none | dash-spv mempool manager (BIP37 or fetch-all) | `dashd-mempool` ATMP |
| Storage | `dash-kv` contract, latch | `dash-kv-sqlite`: headers, cfheaders, filters, matched blocks, MN state, meta | RocksDB chainstate (CH2), subscriber DBs, block files |
| Time | `dash-clock` | system + adjusted (header future-time rule) | `NodeClock` (mocktime, ND1) |

**rdashd `-spv`.**
- `dashd-node` builds dash-spv's client: SQLite store, `SpvRouter` and the wallet through `dashd-wallet`.
- It exposes the RPC subset that SPV data can answer (`getblockchaininfo` header-only fields, `getbestchainlock`, `quorum info` for known quorums, wallet RPCs).
- It never constructs V, the coins cache or `dashd-indexes`. P8 refuses those (§2.8).
- It is the same code path as mobile, which is what makes DEC-81's "one stack" testable on Linux CI.

### 2.5 One network loop: `dash-net`

**What is one.** One `NetCore` per process and network. It owns:
- the connection table (Core's `m_nodes`, P2a:145): `RwLock<Vec<Arc<PeerHandle>>>`, copied out as `PeerSnapshot`;
- the per-peer reader and writer tasks on the host's tokio runtime;
- framing and transport (P1, V2 by default with V1 fallback);
- the shared handshake functions, the pre-handshake gates and the decode policy (P2a §3.2–3.3);
- ping and inactivity timeouts;
- addr intake;
- AddrMan (P2a §3.7, Core bucket math, `peers.dat` in the node or a KV table in SPV);
- `BanList` (ban plus discourage, Core semantics, persistence through a `NetStore` trait);
- the dialing state machines (P2a's `opencon_step`/`addcon_step`/`dnsseed_step`, P2a:182);
- with `inbound`: listener, inbound limits and `AttemptToEvictConnection`.

```rust
pub struct PeerId { pub addr: PeerAddr, pub conn: u64 }          // #902 peer.rs:47; Core NodeId semantics
pub struct RawMsg { pub cmd: Command, pub payload: Bytes, pub recv_us: i64 }
pub struct PeerHandle {                                           // one per connection, immutable identity
    pub id: PeerId, pub kind: ConnKind, pub info: OnceLock<PeerInfo>,       // PeerInfo set by the handshake
    recv: Mutex<RecvQueue>,  pause_recv: AtomicBool,               // Core vProcessMsg + nProcessQueueSize/fPauseRecv
    send: Mutex<SendQueue>,  pause_send: AtomicBool,               // Core vSendMsg + nSendSize/fPauseSend
    /* writer task notify, disconnect flag, counters */
}
impl NetCore {
    pub fn new(cfg: NetConfig, rt: tokio::runtime::Handle, store: Arc<dyn NetStore>,
               clock: Arc<dyn Clock>, wake: Arc<dyn Fn() + Send + Sync>) -> Arc<Self>; // wake = WakeMessageHandler
    pub fn peers(&self) -> PeerSnapshot;
    pub fn pop(&self, p: &PeerHandle) -> Option<RawMsg>;         // never blocks
    pub fn push(&self, p: &PeerHandle, m: OutMsg);               // never blocks; the writer task drains
    pub fn disconnect(&self, p: PeerId, why: Why);
    pub fn discourage(&self, p: PeerId, why: &'static str);       // shared misbehaviour outcome
    pub fn ban(&self, net: SubNet, until: Option<i64>);
    pub fn dial_tick(&self, now: i64) -> DialPlan;                // sans-io; the host drives it on its own schedule
}
pub mod handshake { pub fn version_for(..); pub fn on_version(..) -> Step; pub fn on_verack(..) -> Step; }
pub mod gates     { pub fn admit(state: &PeerState, cmd: &Command) -> Gate; }  // pre-version/pre-verack/blocks-only
```

**What differs is the processor**, which pulls from the per-peer queues:

| | Node: `dashd-p2p::PeerManager` | SPV: `dash-spv::SpvRouter` |
|---|---|---|
| Thread | One OS thread, Condvar wake (`fMsgProcWake`, P2a:147) | One tokio task, `Notify` wake |
| Order | Core's `ThreadMessageHandler`: shuffle, one message per peer per pass, two receive queues with the 100:1 quorum priority, MN-only throttle, 13-section `send_messages` (P2a:535-610) | Per-peer FIFO drain into topic queues (#902's `subscribe`), cross-type order kept per peer (fixes #902's pump) |
| Requests | Core's per-peer state (`mapBlocksInFlight`, getheaders throttle, P3) | `RequestLedger`: #902's `RequestKey` registry made per-peer, attempt cap 3 then `RequestFailed{key}` to the owning manager, explicit `cancel(key)`, `notfound` cancels and re-routes once; strict-priority classes and the bandwidth controller from #902 |
| Handshake | `handshake::*` called from `ProcessMessage` in Core order | the same functions, called from the router's per-peer drain |
| Misbehaviour | Core's scores (P2a §3.10) → `discourage` | SPV rules: invalid header, cfheader, cfilter, merkle, mnlistdiff, qrinfo, CL or IS → `discourage`; non-answering → `disconnect` (no ban) |
| Registry | 74 commands at M2 (P2a:1188-1223), exhaustive | exhaustive. SPV consumes `headers`, `headers2`, `inv`, `cfheaders`, `cfilter`, `block`, `merkleblock`, `tx`, `mnlistdiff`, `qrinfo`, `clsig`, `isdlock`, `notfound`, `addr`/`addrv2`, `sendheaders`/`sendheaders2`, `ping`/`pong`. Everything else is `Ignore`, logged once per command |
| Dialing | `opencon` OS thread driving `dial_tick` (Core timing) | tokio interval driving `dial_tick` with SPV slot counts and the service requirement (`COMPACT_FILTERS`, `BLOOM` if mempool) |
| Serving | `inbound` + P2b/P4/P6b serving | none |

**Why not one processor.** Core's scheduler is part of the node's observable behaviour (P2a §3.12, PQ quirks) and costs a dedicated thread with Core's wake cadence, which is wrong on a phone. #902's any-peer broker gets its speed by giving up per-peer semantics that a node must keep (P2a §3.4: per-peer locators and per-peer judgement of responses). DEC-81's "one network loop" is honoured where it matters: one implementation of connections, peers, addresses, bans and protocol state machines, one place to fix a protocol bug, and one transport.

**Backpressure and memory.**
- Each peer's receive queue has Core's `-maxreceivebuffer` semantics. Over the flood size, the reader stops reading, which is TCP backpressure.
- The send queue has `-maxsendbuffer`. Over it, `pause_send` is set and processors skip that peer's sends, as Core does.
- SPV defaults are smaller (receive 1 MiB, send 256 KiB, about 8 peers), so SPV memory is bounded per peer, not by a global unbounded channel as in #902 (`#902 peer.rs:177`).

**What is not shared.** The node's addrman parity tooling (`diff_addrman`), feelers, anchors and block-relay-only slots are node policy (`dash-net` implements them; SPV never enables them).

### 2.6 One header/sync flow: `dash-chain`

`HeaderChain` is CH1's arena and acceptance, minus V:

```rust
pub struct HeaderChain<S: HeaderStore> {
    arena: Arena,                  // append-only chunks, immutable entries, seqlocked status word (CH1 R1/R2)
    index: HashIndex,              // hash -> BlockId, concurrent append-only (R1)
    best_header: BlockId,          // max chainwork among VALID_TREE headers not conflicting with `cl_floor` (R5)
    active: ActiveChain,           // height -> BlockId; SPV: best-header chain; node: V's connected chain
    anchor: Anchor,                // Genesis | Checkpoint { height, hash, below: [Header; 24] } (DGW/MTP window)
    window: Option<u32>,           // SPV: headers kept resident above the finality floor; node: None (all resident)
    cl_floor: Option<(u32, BlockHash)>,
    params: Arc<ChainParams>, clock: Arc<dyn Clock>, store: S, dirty: DirtySet,
    published: ArcSwap<ChainReadSnapshot>,   // CH1 R8 / DEC-55: { tip, best_header, active, bls, epoch }
}
impl<S: HeaderStore> HeaderChain<S> {
    pub fn accept(&mut self, b: HeaderBatch, from: Option<PeerId>) -> HeadersResult;  // Core ProcessNewBlockHeaders
    pub fn apply_chainlock(&mut self, cl: VerifiedChainLock) -> ClOutcome;            // floor; may move best_header
    pub fn set_active_tip(&mut self, to: BlockId) -> TipChange;                       // the only tip writer (P2)
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot>;
    pub fn take_dirty(&mut self) -> DirtyRecords;  // node: written in B1 by CH2; SPV: one SQLite transaction
}
pub trait HeaderStore { fn header(&self, id: BlockRef) -> Result<Option<Header>, StoreError>; /* cold tier, by height or hash */ }
```

**Acceptance rules** are the same for both products, all from `dash-consensus` (CS0 §8, CS2):
- X11 PoW against the header's own target;
- `get_next_work_required` (DGW/KGW) against `pprev`;
- MTP;
- the future-time limit from `dash-clock`;
- version gates;
- Core's checkpoint rule (no fork below the last checkpoint);
- minimum chain work for IBD latching (R4);
- the ChainLock-conflict hook, which is Core's header rule, not an SB0 addition. Core's `ChainstateManager::AcceptBlockHeader` applies it to headers (dash `15db478750`, src/validation.cpp):
  - a header whose parent is marked `BLOCK_CONFLICT_CHAINLOCK` is refused as `bad-prevblk-chainlock` (:4641-4645);
  - a header at height ≤ the best ChainLock's height that is not the locked block's ancestor at that height (`Chainlocks::HasConflictingChainLock`, src/chainlock/chainlock.cpp:73-95) goes into the index with `BLOCK_CONFLICT_CHAINLOCK` and is refused as `bad-chainlock` (:4691-4697);
  - an already-indexed header carrying the flag is refused as `duplicate` (:4614).

  When a new ChainLock arrives, Core walks back from the conflicting tip and marks the conflicting branch (:4113-4119). `ConnectBlock` checks again before connecting (:2517-2519).

  `cl_floor` is Core's `bestChainLockBlockIndex`: the best ChainLock whose block is already in the header index. A ChainLock for an unknown header is held and does not move the floor (§2.7, trust step 7). Both products get this from `dash-chain`.

The headers are hashed once, at decode (DEC-51a, `dash-consensus` `Header`).

**Two additions to CH1's design, both inert in the node:**

1. **Anchored base.**
   - SPV starts from a checkpoint (`CheckpointManager`, `chain/checkpoints.rs:61-124`).
   - DGW and MTP need the 24 headers below the anchor. `dash-chain` requests them with a `getheaders` whose locator is the checkpoint 25 blocks back, or from the previous checkpoint. They are accepted only if they hash-link to the anchor's hash, so they are trusted by the checkpoint exactly as the anchor is.
   - Chainwork comparisons above an anchor are relative. Every candidate shares the anchor, so absolute work is not needed.
   - `lower_anchor(new_cp)` (the #921 backfill idea) is reserved, not designed.
2. **Resident window.**
   - With `window = Some(w)`, entries more than `w` below `max(cl_floor, best_header − w)` are dropped from the arena. Chunks are released whole. `BlockId`s stay stable because the arena keeps a floor offset.
   - Reads below the floor (`ancestor`, `header`) go to `HeaderStore` (an indexed SQLite read).
   - LQ1 needs ancestors within about 4 DIP24 cycles (≈ 2.3k blocks), so a mobile `w` of 10,000 keeps every LLMQ read resident.
   - Without this, a mobile client from genesis would hold about 2.55 M entries × ~120 B ≈ 305 MB of arena. Today's dash-spv instead materialises ≈ 285 MB transiently at open (§1.1).

**Download policy belongs to the processor.**

| | Node (P2a §3.4, P3) | SPV (dash-spv) |
|---|---|---|
| Headers | Core's per-peer `getheaders` with locator, 2-minute throttle, unconnecting-headers counter, `headers2` | Checkpoint-segmented parallel download (`pipeline.rs`, 8 segments), buffered and accepted **in order** through `accept`. The buffer is bounded at 8 × the segment size and out-of-order segments wait. #902 fixed a stall where a testnet restore sat on 1,046,289 buffered headers because promotion ran only when a `headers` message arrived (`#902 block_headers/manager.rs:215-223`); with the bound, such a stall can never hold more than 8 segments |
| Then | Block download → V → `set_active_tip` | `set_active_tip(best_header)` after acceptance; cfheaders → cfilters → matched blocks follow the active chain as *subscribers* (§2.8) |

**Fork handling.**
- `accept` and `apply_chainlock` produce `TipChange { disconnected: Vec<BlockRef>, connected: Vec<BlockRef> }`. That is a proper reorg; it replaces `handle_fork`'s stop-truncate-restart (`lifecycle.rs:257-279`).
- SPV consumers rewind by the disconnected list. The wallet uses #1104's `truncate_above` at the fork height. #1104's ChainLock floor stays as a wallet-side assertion that can no longer fire for an accepted chain.

### 2.7 One MN-list/quorum/ChainLock/ISLock pipeline: `dash-evo` + `dash-mnstate`

**The shared vocabulary is the SML, not the DML.**
- Every SML field is a function of a DML entry (EV2 table, digest §EV2). The reverse is impossible: mnlistdiff carries no collateral, owner key, payout or PoSe state.
- So the shared list type is `dash-evo::sml::SmlList`: entries keyed by proTxHash, with the quorum set and an `Arc` structure-shared map.
- The node produces it by projection. SPV produces it from verified diffs.
- LQ1 works over both through `MnListView`:
  - `impl for dash_dml::MnList` in the node (exact);
  - `impl for SmlList` in SPV. It is exact except `try_add`'s collateral and owner-key rejections and the collateral tie-break (LQ1:247). Those can only under-reject, which makes SPV fail closed on a quorum: BLS verification fails and the quorum stays `Unverified`. They never make it accept a wrong one.

```rust
pub struct MnState {                                    // one writer; readers get Arc<MnSnapshot>
    lists: SmlLists,               // by BlockId: sparse (SPV: diff targets + qrinfo heights) or a window (node)
    quorums: QuorumSet,            // (llmq_type, quorum_hash) -> { commitment, status: Verified|Unverified|Skipped, base }
    rotation: SnapshotRows,        // (llmq_type, cycle base) -> QuorumSnapshot; qrinfo (SPV) or llmq_S (node); LQ1 §2.3
    best_cl: Option<VerifiedChainLock>, pending_cl: PendingLocks, islocks: IsLockCache,
    mode: Producer,                // P2p | Projection { shadow: Option<ShadowCfg> }
}
pub enum MnInput<'a> {
    Diff(&'a MnListDiff, BlockId), QrInfo(&'a QrInfo),                          // SPV producer
    Projection { at: BlockId, sml: Arc<SmlList>, mined: &'a [MinedCommitment] }, // node producer (CH7a/EV4/CH7b)
}
impl MnState {
    pub fn apply(&mut self, i: MnInput, chain: &dyn ChainAncestry, ctx: &LlmqCtx) -> Result<MnChange, MnReject>;
    pub fn verify_chainlock(&self, cl: &ChainLockSig, ctx: &LlmqCtx) -> Result<VerifiedChainLock, LockReject>;
    pub fn verify_islock(&self, is: &IsLock, ctx: &LlmqCtx) -> Result<VerifiedIsLock, LockReject>;
    pub fn snapshot(&self) -> Arc<MnSnapshot>;
}
```

**SPV trust chain:**
1. The diff's target block is an accepted header in `HeaderChain`.
2. The CbTx partial-merkle proof (`diff.merkle_hashes`, `merkle_flags`) reproduces that header's merkle root with the coinbase at index 0 (`compute_merkle_root` + BIP37 tree).
3. Base list + diff → `SmlList`. Its root (memcmp proTxHash order, Core `CalcHash` per entry; EV4 core) must equal the CbTx `merkleRootMNList`.
4. The quorum set after the diff must hash to `merkleRootQuorums`.
5. Each new commitment: LQ1 members over the SML adapter, rotation through `SnapshotRows` from qrinfo (LQ1's Core-exact rules), then `membersSig`/`quorumSig` BLS with an explicit `BlsCtx` (R4: legacy below V19, LQ1:1199).
6. Only `Verified` quorums are selectable for signing (`select_from_scan`, quirk 30).
7. CL: verify, then `HeaderChain::apply_chainlock`. A CL for an unknown header waits in `pending_cl` until the header arrives.

After step 4 the SML is exactly as trustworthy as the header chain: PoW plus chainwork plus ChainLocks.

**Node trust model.**
- The projection is authoritative. CbTx roots are consensus checks in CH7a/EV4, so the node never holds a list its own blocks disagree with.
- **Cross-check:**
  - `MnState` in `Projection { shadow: Some(..) }` also runs steps 1–5 on a peer's `mnlistdiff` for the tip every N blocks (default 16) and on each new rotation cycle's qrinfo. It compares SML root, quorum set and members, and on mismatch logs `mnxcheck` and counts a metric. It never feeds consensus.
  - On by default on regtest, devnet and testnet, and in every CI and L2 run. Opt-in on mainnet (`-mnxcheck`).
  - This replaces LQ1 §6.4's inverted fixture test (no longer forced once R2 is gone).
- `dashd-locks` keeps Core's processing order, relay and signing, but calls `MnState::verify_chainlock`/`verify_islock` on the projected state, so verification code is one.

**Persistence.**
- SPV: `MnState` commits its verified state (lists at retained heights, quorums with status, snapshots, best CL) in the same SQLite transaction as the header records it depends on. The raw-message replay (`storage/masternode.rs:160-214`) goes away, and with it #1081's need.
- Node: `MnState` holds only derived state rebuilt from EvoDB (`dash-dml` `resolve_list`, `llmq_*` CFs) at start. Nothing new is persisted.

### 2.8 Chain events, and how CH2's P1–P9 are met

`dash-chain` provides the feed primitive for both products:

```rust
pub struct FeedHub<E> { next_seq: u64, subs: Vec<FeedSlot<E>> }     // owned by the tip writer (V or the SPV driver)
pub struct Feed<E>    { q: ArrayQueue<(u64, E)>, waker: SubWaker }  // bounded data queue; SubWaker = Fn() or Waker
impl<E: Clone> FeedHub<E> {
    pub fn publish(&mut self, e: E) -> Publish;   // assigns seq first (F2), then try-pushes; returns Full(sub ids) and never waits
    pub fn register(&mut self, needs: Needs) -> Result<FeedId, Refused>;   // P8
}
```

- In the node, CH2's `SubscriberHub` (`dashd-chainstate/subscriber/hub.rs`) wraps `FeedHub<ChainEvent>` and adds the control plane (`FlushCommit`, `RewindRequest`, `ChainStateFlushed`), the mirrored hub state and the flush barrier. V's service loop handles `Full` (CH2 rule 5).
- In SPV, the driver owns a `FeedHub<HeaderEvent>`. Its subscribers are the cfheaders, filters and blocks managers, the wallet adapter and `dash-mnstate`. Each keeps a durable locator in the SQLite store, so a restart resumes and a reorg rewinds per subscriber.
- Chain-ordered data moves off the broadcast bus onto these bounded feeds. The `SyncEvent` bus stays for progress only.

| # | CH2 §10.1 requirement | How SB0 meets it |
|---|---|---|
| P1 | Feed guarantees F1–F9 | **F1:** only the tip writer calls `publish`, in tip-change order; queues never drop. **F2:** `publish` assigns `seq` before the push attempt, and `SubscriberHub` records `seq_synced` under its lock at the Syncing→Synced transition. **F3:** `ArrayQueue` bounds data only; `Full` returns to V, which waits in its service loop (CH2 rule 5); other subscribers' pushes are unaffected. **F4:** control events live in `SubscriberHub` (node-only), not in `FeedHub`; mirrored, out of band. **F5:** `SubWaker` fires on push and on control. **F6:** CH2's driver threads; a parked feed holds no thread. **F7:** `SubscriberHub` publishes `ChainStateFlushed`/`T_ack` after W reports B1 durable (CH2 §4.2); `FeedHub` takes no part in flushing. **F8:** ancestry through `dash-chain`'s `IndexAccess` on immutable arena entries, lock-free from any thread (`window = None` in the node, so every delivered block stays resident); block and undo bytes through `dashd-store` with prune locks (P4). **F9:** `best_block()` stays `SubscriberDriver`'s; `FeedHub` reports no progress |
| P2 | One tip writer (V) that services `FlushDue` in every wait | `HeaderChain::set_active_tip` is the only active-tip mutator, and in the node it is called only on V (CH1-node). `FeedHub::publish` never blocks, so every wait is V's own service loop, which CH2 defines to service `FlushDue`, including the empty flush |
| P3 | Flush order B1 → `ChainStateFlushed` → synced commits → prune selection → Bk/Bp; rewind gate before a backward B1; `T_ack` after B1 | Entirely in `dashd-chainstate`'s `FlushMachine` (CH2b, unchanged). `dash-chain` contributes only `take_dirty()` records, which CH2 writes **inside B1** (as CH1 planned). The shared crates never commit chainstate data themselves in the node |
| P4 | Prune locks from subscriber locators | Node-only. `dashd-store::flatfile` + CH2 prune selection. `dash-chain` answers `lca`/`ancestor` for locator heights |
| P5 | An engine meeting the `KvEngine` contract; each subscriber commits to its own engine on its own thread | `KvEngine` moves to `dash-kv` **unchanged** plus `delete_range` (BL-38). Node subscribers keep `RocksEngine` through `RocksTuning` (CH2 §2.5.1). `dash-kv-sqlite` also meets the contract (prefix ⊇ subset), so a test or SPV-side subscriber may use it. Engines are per subscriber, never shared |
| P6 | One node-wide fail-stop latch across chainstate, subscriber engines and block files | `GlobalFailStop`/`GuardedEngine`/`AbortSink` move to `dash-kv` as CH2 §2.10 defines them. The node creates one per process and passes it to every engine and to `GuardedFiles`. dash-spv creates one per client and its `AbortSink` stops the client and emits an FFI error event instead of `process::abort` (L6) |
| P7 | Subscriber classes and transitions as Core's `m_synced` | `SubscriberHub` (CH2b) unchanged. Its `ChainSnapshot` is `dash-chain`'s `ChainReadSnapshot` (`tip()`, `at_height()` over the published `ActiveChain`). The transition happens under the hub lock, atomically with `seq_synced` (F2) |
| P8 | Refuse an undo-needing subscriber where there is no undo | `FeedHub::register(Needs { block, undo })` returns `Refused::NoBlockData`/`NoUndo` in SPV mode. rdashd `-spv` never constructs `SubscriberHub` or `dashd-indexes`; `dashd-node` refuses `-txindex`, `-coinstatsindex` and the other indexes with `-spv` as an InitError. A pruned full node keeps undo inside its prune window, as Core does |
| P9 | Mempool coin access as V commands | Untouched: `dashd-mempool` is node-only and talks to V by `ChainCommand`. SPV's mempool manager never touches coins |

No F-guarantee is weakened, so CH2's §4.4.2 argument stands without changes. The one new fact CH2b must take in is placement: its hub wraps `dash-chain::FeedHub` instead of CH1's bus.

### 2.9 Runtime model, failure and clocks

| Context | Node (rdashd full) | SPV (mobile, rdashd `-spv`) |
|---|---|---|
| Sockets | `dash-net` tasks on the `net` tokio runtime (A1:1689) | `dash-net` tasks on the client's runtime (`dash-spv-ffi/src/client.rs:40-110`; 2 workers suggested on mobile) |
| Processor | `PeerManager` OS thread | `SpvRouter` task |
| Header chain owner | V (OS thread) | the SPV driver's header task |
| `MnState` owner | V (projection) + a shadow task | the masternodes task |
| CPU pools | rayon `prevalidate`/`script`/`bls` (A1:1692-1694) | one injected rayon pool, default `min(2, cores)`, used for X11 batches, filter checks and BLS batch verification |
| Storage | W (db-writer), RocksDB background threads | inline SQLite commits on the owning task (`spawn_blocking`); no background threads |
| Allocator | jemalloc, set in `rdashd`'s `main.rs` only | system allocator (L6) |
| Fatal storage error | `GlobalFailStop` → `AbortSink::abort_node` (ND1) | `GlobalFailStop` → client stops, `on_error` FFI event, storage left consistent |

Shared crates take what they need as parameters: a tokio `Handle` (`dash-net` only), an `Arc<dyn Clock>`, a `&rayon::ThreadPool` for batch CPU work, a `GlobalFailStop` and an `Arc<dyn AbortSink>`. They create none of these themselves.

### 2.10 Types and codecs

- **Shared layers** use `dash_consensus::{Header, Tx, Block}` and base-sdk `dash-p2p-core` payloads. `dash-net` moves raw frames (`RawMsg`), and processors decode after the gates (P2a:1253-1258), each with base-sdk.
- **dash-spv** keeps `dashcore` at its edges:
  - `WalletInterface::process_block_for_wallets` takes `dashcore::Block` (re-decoded from the verified `CoreBytes` of a matched block, a few per sync);
  - FFI types and Platform's `get_quorum_public_key` keep their shapes.

  Conversion uses the BS14 byte-identical bridge.
- **`dashcore::sml` and `dashcore::network::NetworkMessage`** stay public (Platform uses them). They become compatibility shapes filled from `dash-evo::sml`, until CV3/CV4 retire them. They no longer contain any engine logic.
- **This drops A1's "two codecs" (A1:413-416) for the protocol path.** BS2 equality tests remain for the W stack's public types.

---

## 3. Mobile budget

### 3.1 Measurements

**Binary size.** `dash-spv-ffi` at 186cae03 was built for `aarch64-linux-android` (NDK 29.0.14206865, API 24, release profile as committed, `llvm-strip`). Each probe variant adds one exported function that opens a store, batch-writes, reads, scans and deletes, so the linker keeps a representative part of the engine. Recipe: App. A.

| Variant | Stripped `.so` (bytes) | Change vs v0 |
|---|---|---|
| v0: `dash-spv-ffi` as committed | 12,622,128 (12.6 MB) | — |
| v4: + SQLite (`rusqlite` 0.38 `bundled`, default flags) | 14,437,176 | +1,815,048 (+1.82 MB, +14.4 %) |
| v5: + SQLite, `bundled` with unused features compiled out (`LIBSQLITE3_FLAGS`, App. A) | 13,755,184 | +1,133,056 (+1.13 MB, +9.0 %) |
| v3: + `redb` 2 | 13,258,048 | +635,920 (+0.64 MB, +5.0 %) |
| v2: + RocksDB 11.8.1, no compression codecs | 19,477,816 | **+6,855,688 (+6.86 MB, +54.3 %)** |
| v1: + `dashd-store`'s RocksDB (`lz4`, `zstd`) | 20,319,224 | **+7,697,096 (+7.70 MB, +61.0 %)** |

Unstripped, v0 is 20,088,472 bytes. The deltas above are what an app ships.

What a SQLite engine costs depends on where the library ends up:
- inside Platform's unified Android SDK: **0**, because that SDK already links the same `libsqlite3-sys` (below);
- in a standalone Android `dash-spv-ffi`: v4's delta, or v5's with the unused features compiled out;
- on iOS: **0** (system library).

Facts about what the mobile SDKs already link (checked with `cargo tree` on `~/workspace/platform`):
- **Android.** `rs-unified-sdk-jni` enables `shielded` by default (`packages/rs-unified-sdk-jni/Cargo.toml:33`). That pulls `rusqlite` 0.38 with `bundled` (`libsqlite3-sys` 0.36) through `grovedb-commitment-tree`. The shipped Android SDK therefore already contains SQLite, and a shared-base SQLite engine adds no new library there, provided it uses the same `libsqlite3-sys` (`links = "sqlite3"` allows only one per graph).
- **iOS.** `rs-unified-sdk-ffi` has `default = []` (`packages/rs-unified-sdk-ffi/Cargo.toml:17`), so it links SQLite only with `shielded`. iOS ships SQLite as a system library, and `rusqlite` without `bundled` links `/usr/lib/libsqlite3.dylib`, so the marginal size is 0. If an app also enables `shielded`, feature unification makes it the bundled copy, still one copy.
- **Both.** dash-spv (via `platform-wallet`) is in both unified SDKs. No component of either links RocksDB.

**Engine behaviour on an SPV-shaped workload.** `sb0-memprobe` writes headers (80 B) and filter headers (32 B) keyed by height in batches of 2,000 with a sync every 100,000. It then does 200,000 random point reads and a 50,000-row tail scan. x86_64 host, XFS on the box's virtual disk, shared with other agent threads (load average about 15–20 on 32 vCPUs during the runs), so the times are indicative only. RSS and disk are robust.

Two runs per engine, except one for the pinned variant. "RSS above base" subtracts the probe binary's own 12.4 MiB, measured after the run and a 2-second idle. "Disk" is the files' apparent size. Raw data is 2.55 M × 112 B = 272.4 MiB.

| Engine and settings | Write (s) | Random get (µs) | 50k tail scan (ms) | RSS above base (MiB) | Peak RSS (MiB) | Disk (MiB) | Files at rest |
|---|---|---|---|---|---|---|---|
| `flat`: two files indexed by height (no index, no atomicity) | 0.6–0.8 | 1.0 | 3.3–3.4 | +0.3–0.4 | 15.9–16.6 ¹ | 272 | 2 |
| `sqlite`: WAL, `synchronous=NORMAL`, 2 MB page cache | 5.8–6.2 | 5.3–5.9 | 8.7–9.4 | **+3.8–3.9** | 15.9–16.2 | 324 | 1 |
| `redb`: 4 MiB cache, `Eventual` | 21.5–22.2 | 5.5–6.6 | 10.0–10.7 | +6.1–6.2 | 20.1 | **1,029** (617 allocated) | 1 |
| `rocks-mobile`: 4 MiB cache, 8 MiB memtables, 1 job, 64 open files | 5.2–5.3 | **42–47** | 16–21 | +16.6–16.8 | 29.9–32.7 | 342 | 250 SSTs |
| … index and filter blocks pinned outside the cache | 5.2 | 46 | 17 | +17.3 | 29.8 | 342 | 250 SSTs |
| … unlimited open files | 5.2–6.6 | 10.3–10.6 | 13–15 | +20.6–20.9 | 32.2–32.3 | 342 | 246 SSTs, all open |
| `rocks-node`: `RocksConfig::default()` sizes (24 MiB cache, 32 MiB memtables, 8 jobs) | 3.9–4.5 | 7.4–9.5 | 9.1–10.7 | **+40.7–72.3** | 68.2–83.5 | 340 | 46 SSTs |

¹ The flat engine's scan reads the whole 4 MB tail into one buffer; the other engines stream it.

What the table shows:
- **SQLite holds the SPV workload in about 4 MiB above the process**, reads a random header in about 5–6 µs and stores it with 19 % overhead in one file.
- **RocksDB cannot be both small and fast on this workload.** Keys arrive in height order, so every memtable flush becomes an SST that is moved to L6 untouched (`Moved #4 files to level-6` in its LOG). Small memtables therefore mean many small files: about 250 at 8 MiB of memtables.
  - With a 64-file table cache, almost every random read reopens a table: 42–47 µs, 8× SQLite. Pinning index and filter blocks does not help, which confirms the table cache is the cause.
  - With every file open, reads drop to about 10 µs, still about 2× SQLite. That costs +21 MiB, 5× SQLite's footprint, and about 250 file descriptors, which is all of macOS's default soft limit of 256 (`launchctl limit maxfiles`).
  - The committed node configuration reads in 7–9 µs, still slower than SQLite here, at +41 to +72 MiB.
- **redb** matches SQLite on reads but is about 3.6× slower to write, and its file grew to 3.8× the data (2.3× in allocated blocks) without compaction. It is the pure-Rust fallback (§4.5), not the first choice.
- **The flat layout is fastest** but has no hash index, no atomicity across tables and no WAL, which the SPV store needs (§4.1).

### 3.2 Budget

These are targets for the B1 `spv-*` cells (§8), not measurements. Each row says what it costs to stay inside it.

| Resource | Budget for the shared base on mobile | Today (dash-spv) | Cost of the choices |
|---|---|---|---|
| Stripped `.so` (aarch64-android) | Shared-base code ≤ +1.5 MB over today's 12.6 MB. The storage engine is budgeted separately: 0 on iOS and inside Platform's Android SDK (both already have SQLite), and at most v5's delta in a standalone Android build | 12.6 MB | Adds `dash-consensus` + shared `dash-evo` + `dash-filters` + `dash-chain`/`dash-mnstate`/`dash-net`, plus SQLite where the host lacks it (§3.1). Removes `network/` (4.8k LOC), `sml_engine` (1.7k), segment storage (≈3k), `clap`, and possibly `rs-x11-hash` (C) if `dash-x11` replaces it. **RocksDB alone would take 6.9–7.7 MB, 4–5× the whole code budget, and no host already has it** |
| Resident headers | ≤ 2 MiB (window 10k × ~120 B) | 2 segments + compact hash index (≈ 14 MB at 2.55 M headers: 4 MB offsets + 4 B per header, `storage/header_hash_index.rs:6-60`) | Windowed arena, cold reads from SQLite (§2.6) |
| Startup | O(window), < 1 s from local storage | O(chain): reads every header (`block_headers.rs:116-138`) | Hash index persisted in the store, not rebuilt |
| MN state | ≤ 8 MiB | `BTreeMap` of lists with `Arc` sharing, pruned by `retention_window` | Same structure sharing; quorum members not retained, only verified keys |
| Filter pipeline | ≤ 32 MiB in flight | 3 × 5000-filter batches (`filters/manager.rs:30, 53`) | Bound the lookahead by bytes, not batches |
| Network buffers | ≤ 8 peers × (1 MiB receive + 256 KiB send) | Unbounded channels in #902 | Per-peer bounded queues with pause (§2.5) |
| Storage engine memory | ≤ 4 MiB page cache + WAL ≤ 4 MiB | none (flat files) | SQLite `cache_size=-4096`, `wal_autocheckpoint`. RocksDB's committed defaults reserve 56 MiB (`rocks.rs:48-63`). Even with a 4 MiB cache and 8 MiB of memtables it measured +17 MiB, and +21 MiB plus about 250 open files once its reads were usable (§3.1); SQLite measured +3.8 MiB |
| Threads | tokio 2 + rayon 2 + blocking as needed | tokio multi-thread + rayon global pool | No engine threads (RocksDB would add ≥ 2) |
| Peak RSS during first sync | ≤ 128 MiB | not measured (no harness) | New `spv-*` cells measure `peak_rss_mib` (A1:2008 already has the metric) |

### 3.3 Feature flags and crate splits

| Item | Mobile default | Node default | Why |
|---|---|---|---|
| `dash-net/inbound` | off | on | listener, eviction and serving gates are node policy |
| `dash-net/v2` (P1 BIP324) | on | on | protocol parity; small (ChaCha20-Poly1305, ElligatorSwift is already in secp256k1) |
| `dash-kv-sqlite/bundled` | Android, Linux, Windows: on. Apple: off (system lib) | on | 0 bytes on Apple |
| `dash-spv/cli` (clap, `main.rs`) | off; the bin moves to `dash-spv-cli` | — | L4 bans `clap` from the mobile closure (`dash-spv/Cargo.toml:21`, `dash-spv-ffi/Cargo.toml:27`) |
| `dash-spv/log-file` (`tracing-appender`, `chrono`) | off | — | file logging is an app concern; FFI forwards to a callback |
| X11 back end | `dash-x11` (pure Rust, `no_std`) | `dash-x11` (XH1) | removes the C `rs-x11-hash` from both; needs an aarch64 speed check (§8 R3) |
| `dash-dml`, `dash-llmq`, `dash-governance`, `dash-coinjoin` | absent (L4) | present | node consensus only |
| `dashd-store` (RocksDB, flatfile) | absent (L4) | present | node storage |

---

## 4. Storage recommendation (DEC-43 re-check under DEC-85)

### 4.1 Storage by role

| Role | Workload | What the protocol needs | Engine |
|---|---|---|---|
| Node chainstate (coins, block index, `mn_*`, `llmq_*`, `credit_pool`, `mnhf`) | UTXO churn: 16 MiB sub-batches, misses on the validation path, multi-CF atomic B1 | `KvEngine` contract + CH2 `RocksTuning` | **RocksDB** (unchanged, DEC-43/ST1) |
| Node subscriber DBs (txindex, blockfilterindex, coinstats, address/spent/timestamp) | append-mostly, blind range deletes up to 2^24 heights | contract + O(1) `delete_range` | **RocksDB** |
| Node block and undo files | append, prune by file | `FlatFs` (ST2) | **flat files** (`dashd-store::flatfile`, made portable per DEC-84) |
| Node `peers.dat`, `banlist`, `anchors.dat` | Core formats | none | flat files (P2a §4) |
| SPV store, mobile and rdashd `-spv` (headers, hash index, cfheaders, filters, matched blocks, MN state, CL, subscriber locators, peers, bans, request ledger snapshot) | append by height, point reads, small batches, one writer; must survive power loss consistently across tables | `KvEngine` contract | **SQLite** (`dash-kv-sqlite`) |

### 4.2 Evidence

1. **ST1 does not transfer to SPV.** It decided "on disk use alone" for one UTXO-churn workload on x86/XFS (DEC-43(a); ST1:482). It never measured size, ARM, small RAM, or header and MN workloads (ST1:200-204). The SPV workload has no churn: rows are written once by height and rarely deleted.
2. **Size.** §3.1, on today's 12.6 MB stripped Android `.so`: RocksDB adds 6.86 MB without compression codecs and 7.70 MB as `dashd-store` builds it (+54–61 %). SQLite adds 1.13–1.82 MB to a standalone build, and nothing inside Platform's Android SDK or on iOS. RocksDB would also be a second copy of a large C++ library in every mobile app. Nothing else in the mobile SDKs links it.
3. **Memory.** RocksDB's committed defaults reserve 24 MiB of block cache plus 32 MiB of memtables (`dashd-store/src/rocks.rs:48-63`). CH2's engine target is 74 MiB (CH2 §2.5). §3.1's probe gives the RSS on the same 2.55 M-header workload: SQLite +3.8 MiB at 5–6 µs per random read; RocksDB tuned to 4 MiB of cache and 8 MiB of memtables +17 MiB at 42–47 µs, or +21 MiB and about 250 open files at 10 µs; the committed node configuration +41 to +72 MiB at 7–9 µs.
4. **Apple durability.** `librocksdb-sys` 0.19.0 never defines `HAVE_FULLFSYNC`, so RocksDB's WAL syncs with `fdatasync` on iOS and macOS (BL-32; ST3a:336-343). SQLite honours `PRAGMA fullfsync` and `checkpoint_fullfsync`. On iOS this decides whether an acknowledged commit survives power loss.
5. **Recovery contract.** SQLite in WAL mode recovers to a prefix of committed transactions. That is stronger than DEC-62's subset semantics, so ST3a's `FaultyEngine` (which models subsets) checks a SQLite-backed protocol conservatively (ST3a:253-256).
6. **Portability (DEC-84).** SQLite builds on Linux, macOS, Windows, the BSDs, iOS and Android with no C++ or bindgen. `librocksdb-sys` needs a C++ toolchain and libclang for bindgen on every mobile CI image.
7. **No migration cost yet.** No production column family is merged (every `DbSchema` is a test schema; storage digest fact 7), and dash-spv has no KV engine. Choosing per role costs nothing now.
8. **The node keeps its evidence.** CH2's protocol depends only on the `KvEngine` contract (CH2 §2.5.1: "What the protocol needs from any engine"). `RocksTuning` is the only engine-specific part, and it stays.

### 4.3 Proposed ruling (DEC-43 as re-checked)

> **DEC-43 (re-checked under DEC-85/SB0).**
> - (a) The node's chainstate, EvoDB column families and subscriber DBs use RocksDB through `RocksTuning` (CH2 §2.5.1), unchanged.
> - (b) The shared base and every SPV store, mobile and rdashd `-spv`, use SQLite through `dash-kv-sqlite`.
> - (c) Both engines implement the `KvEngine` contract (DEC-62 subset semantics or stronger), and no pure or shared crate names an engine.
> - (d) The low-memory DEC-43 cell (CH2 §5.5) still gates M3.
> - (e) CH2a a3 proceeds whole (CH2 Q6).

### 4.4 `dash-kv-sqlite`: the contract in SQLite terms

| `KvEngine` | SQLite mapping |
|---|---|
| Column family | one `WITHOUT ROWID` table per CF: `(k BLOB PRIMARY KEY, v BLOB NOT NULL)`. BLOB comparison is memcmp, which is the contract's key order |
| `batch()`/`KvBatch` | an in-memory op list (`put`, `delete`, `delete_range`) |
| `write(b, Buffered)` | one `BEGIN IMMEDIATE … COMMIT` with `synchronous=NORMAL` in WAL mode: atomic and survives a process kill; a power loss may lose a *suffix* of Buffered commits |
| `write(b, Durable)` | the same transaction with `synchronous=FULL` (WAL fsync at commit, plus `fullfsync=1` on Apple). It covers every earlier commit because the WAL is sequential, which is stronger than DEC-63 Q14's rule |
| `sync_wal()` | if any Buffered commit happened since the last Durable one: a Durable commit of `meta.sync_seq += 1`; else a no-op |
| `flush_memtables()` / `after_bk` | no-op / `wal_checkpoint(PASSIVE)` |
| `delete_range(cf, a, b)` | `DELETE FROM cf WHERE k >= ?a AND k < ?b`: O(rows), so `EngineTuning::unbounded_delete_span` for SQLite is the journal's `d_max`, not 2^24 |
| `snapshot()` | a deferred read transaction on a second connection (WAL snapshot isolation) |
| `get`/`multi_get`/`iter_prefix` | cached prepared statements; range scans `ORDER BY k` |
| `approximate_size(cf)` | `dbstat` if compiled in, else `page_count × page_size` |
| Fail-stop | `SQLITE_FULL`, `SQLITE_IOERR*`, `SQLITE_CORRUPT` and `SQLITE_NOTADB` trip `GlobalFailStop`. Reads trip it and call `AbortSink::read_failed` (DEC-78 semantics) |
| Threads | none. Checkpoints run inline at `after_bk` or on the owner's schedule |
| Memory | `cache_size=-4096` (4 MiB) on mobile, `mmap_size=0`, `temp_store=MEMORY`, `wal_autocheckpoint=1000` pages |
| Schema | `DbSchema` → tables. `meta.schema` row as in `dashd-store/src/schema.rs:46-58`. The version check is shared code |

The SPV store schema (`dash-spv`, version 3 after today's `CURRENT_VERSION = 2`, `storage/migrator/mod.rs:15`):

| CF | Contents |
|---|---|
| `meta` | |
| `hdr_by_height` | height → hash ‖ 80-byte header |
| `hdr_index` | hash → height ‖ status ‖ chainwork-since-anchor |
| `cfhdr` | height → 32 B |
| `cfilter` | height → bytes |
| `blocks` | matched blocks |
| `mn_lists` | height → SML list record (or delta) |
| `mn_quorums` | |
| `mn_snapshots` | |
| `locks` | best CL, pending |
| `locators` | per-subscriber durable locator |
| `peers`, `bans` | |

Each sync step commits one Buffered transaction: headers + `hdr_index` + dependent MN rows + locators. A Durable commit happens at each checkpoint boundary, at `SyncComplete` and on `stop`.

Recovery is blind: open, `sync_wal`, read locators. Every subscriber resumes from its own locator. A crash can lose only a suffix of Buffered steps, which are re-downloaded.

**Verification.**
- ST3a's harness checks the SPV commit protocol on `FaultyEngine`, with a new `Sut` for the SPV store (SB2).
- A native regression `sqlite_wal_prefix.rs` mirrors `rocks_wal_nonprefix.rs` (ST3a:1014-1021).
- ST3b's device harness covers SQLite on ext4/XFS. Apple durability is checked on pasta's Mac (BL-32 style).
- An optional `SqliteTuning` lets CH2b's CRASH suite run on SQLite. That proves the protocol depends only on the contract (P5) and costs one test configuration.

### 4.5 Costs and risks of the ruling

| Cost or risk | Size | Mitigation |
|---|---|---|
| Two engines to verify | one more `KvEngine` impl (~1.5k LOC) and one more native regression | ST3a models the contract once. Per-engine tests are small |
| rdashd links both engines | ≈ 1 MB on desktop | irrelevant (no desktop size budget) |
| `libsqlite3-sys` `links` coupling with Platform and the desktop wallet | a version skew fails the build | `dash-kv-sqlite` takes `rusqlite = "0.38"` as Platform does, and BSX-style bumps move both. Recorded in `layers.toml` as a pinned shared dependency |
| SQLite write throughput on mobile flash under `FULL` | Durable commits are rare (checkpoint boundaries) | Buffered `NORMAL` for steps. Measure on a device in SB11 |
| SQLite's size in a standalone Android build | +1.82 MB with default flags (§3.1 v4), +1.13 MB with unused features compiled out (v5) | Budgeted separately from the code (§3.2). B3 records an L5 ruling for the one-time step. Standalone builds set the v5 flags. Platform's Android SDK and iOS pay 0 |
| O(rows) `delete_range` | SPV deletes only on reorg (tens of rows) | none needed |
| Upstream acceptance of a SQLite dependency in dash-spv | unknown | Q2. Fallback: `redb` behind the same contract (pure Rust; §3.1 measured), at the cost of a second library on Android |

---

## 5. Migration path without breaking mobile consumers

### 5.1 Compatibility rules

| Surface | Rule |
|---|---|
| **C ABI** (43 functions, `dash-spv-ffi/src/*.rs`) | Frozen: no signature, struct-layout or enum-value change. New capability is additive: new functions, or new callback tables carrying a leading `size` field. Compile-checked by `ffi-c-tests/header-tests/dash-spv.c` in every stage |
| **FFI events** | A new `dash-spv-ffi/src/events.rs` translation layer maps internal events onto the frozen `FFISyncEventCallbacks`, `FFINetworkEventCallbacks` and `FFIWalletEventCallbacks` (`callbacks.rs:268-285, 529-534, 988-996`). Internal enums (`SyncEvent`, `NetworkEvent`) are then free to change, which ends "any enum change is an ABI change". #1104's `on_chain_truncated` is the last layout change to that table. SB11 suggests the maintainers add a `size` field in the same release |
| **Config** | `FFIClientConfig` is opaque (`config.rs:27-32`; `cbindgen.toml:15-16`), so `ClientConfig` can change freely behind the 18 setters. New knobs get new setters. `FFIValidationMode` stays accepted, but the shared checks are not optional: `None`/`Basic` keep the checks that protect funds and only relax the two-peer cfheader comparison |
| **Rust API** | Changes only where #902 already changes it (network module private, `NetworkEvent` shape, `DashSpvClient.network: Arc<N>`) plus storage construction (`DiskStorageManager` → `SpvStore`). `DashSpvClient` query methods, `EventHandler` and `WalletInterface` keep their shapes. Every breaking PR carries a consumer-impact note (A1:2950 rule kept) for `platform-wallet` and `masternode-seeds-fetcher` |
| **Data directory** | Migrator v3 (`storage/migrator/`, after `CURRENT_VERSION = 2`): segment files → SQLite tables, streamed (no O(chain) memory). The MN message log is dropped: the state re-syncs from one QRInfo and a few diffs. Wallet data is not dash-spv's and is untouched. A failed migration leaves v2 intact and falls back to a fresh sync from the anchor |
| **Behaviour** | A stored chain that fails the shared kernel's rules is discarded: the client logs the rule and re-syncs from the last checkpoint. Current SPV-mode validation coverage: see PLAN §7 handling (CANDIDATE-SPV). |

### 5.2 Staged PRs

Each PR is upstream-shaped: one concern, mergeable on dev, passing the `spv` and `ffi` CI groups plus the dashd integration suites (`eval $(python3 contrib/setup-dashd.py)`; `dashd_sync`, `dashd_masternode`, `dash-spv-ffi --test dashd_sync`). Per DEC-87 they live on local branches until pasta decides to upstream.

| Stage | PR | Change | Needs | Touches #902's files? |
|---|---|---|---|---|
| A (shared consensus rules inside today's dash-spv) | **A1** | Header rules from `dash-consensus` and the anchor's 24-header window in the headers path; `chain_tip.rs`/`chain_work.rs` retired in favour of `dash-consensus`. Scope per PLAN §7 handling (CANDIDATE-SPV) | SB1 | `sync/block_headers/manager.rs` (a few lines) |
| | **A2** | The SML trust chain of §2.7 and ChainLock handling from the shared verification code. Scope per PLAN §7 handling (CANDIDATE-SPV) | SB4 | `sync/masternodes/*`, `sync/chainlock/manager.rs` (conflicts) |
| | **A3** | Block and filter-header rules of §2.4 from `dash-consensus` and `dash-filters`. Scope per PLAN §7 handling (CANDIDATE-SPV) | `dash-consensus` | `sync/blocks`, `sync/filter_headers` |
| | **A4** | Hygiene: `clap` to `dash-spv-cli`, `log-file` feature, per-client devnet parameters, progress-bus lag handling | none | `client/config.rs`, Cargo files |
| B (shared state, same network) | **B1** | `dash-chain::HeaderChain` drives header acceptance. The headers manager becomes an adapter, and cfheaders, filters, blocks and the wallet become `FeedHub` subscribers | SB3 | `sync/block_headers/*`, `sync/sync_coordinator.rs` |
| | **B2** | `dash-mnstate` replaces `sml_engine`. Rotation by LQ1, verified state persisted | SB4, SB5 | `sml_engine*`, `sync/{masternodes,chainlock,instantsend}` |
| | **B3** | SQLite store, migrator v3, `StorageManager` reshaped to per-role handles | SB2 | `storage/*` |
| C (network) | **C1** | `SpvRouter` on `dash-net` behind #902's `NetworkManager` facade: per-peer request ledger with attempt caps, `notfound`, misbehaviour → `discourage`, bans, AddrMan, V2 transport | SB6, **#902 merged or Q1 answered** | yes: it *is* #902's announced follow-up |
| | **C2** | `masternode-seeds-fetcher` uses `dash-net`'s probe API | C1 | `masternode-seeds-fetcher` |
| D (products) | **D1** | FFI event translation, additive functions | B1 | `dash-spv-ffi` |
| | **D2** | rdashd `-spv` | C1, ND1 | node crates only |

Order: A4 → A1 → A3 → A2 → B3 → B1 → B2 → C1 → C2 → D1 → D2.
- A1, A3 and A4 can start as soon as SB1 lands.
- Nothing before C1 edits `dash-spv/src/network/`. A1's isolation rule 8 survives in that narrowed form until C.

### 5.3 #902 coordination

- SB0 never comments on, pushes to or rebases #902 (DEC-87 and the brief).
- **Stages A and B are independent of #902.** They don't touch `network/`. Their conflicts with #902 are in the sync managers, and those are the same conflicts #902 already has with dev (24 conflicting files on 2026-10-09).
- **The node does not wait for #902.** `dashd-p2p`'s processor sits on `dash-net` (SB6) from the start. dash-spv keeps its dev network until C1.
- **C1 is designed as #902's follow-up.**
  - The author deferred misbehaviour and peer persistence "for a next PR" (#902 review state, 2026-08-20). C1 supplies them, plus the structural fixes for blockers 3 and 4 (attempt caps; per-peer writers).
  - It keeps #902's trait, `RequestKey`, priority classes, bandwidth controller and mock, so #902's sync-manager changes carry over unchanged.
- **If #902 is still stalled when C1 is ready** (SB9 done), the choice between waiting and carrying the facade inside C1 is Q1.

---

## 6. Rework of approved designs

### 6.1 Per design

**P2a (rev3, DEC-56).**
- **Stays:**
  - the actor loop (§3.1), as `dashd-p2p::PeerManager` (the node processor);
  - `send_messages`, the PQ quirks, the two receive queues;
  - headers sync §3.4 and the misbehaviour scores §3.10, in the node processor;
  - the 74-command registry (L8);
  - verification §6 (`diff_p2p_*`, `diff_addrman`, sim, fuzz) and performance §5.
- **Moves to `dash-net`, content unchanged:**
  - gates and decode policy §3.2;
  - handshake §3.3, as shared functions with `LocalIdentity` as a parameter;
  - ping §3.5, addr intake §3.6, AddrMan §3.7;
  - ConnMan §3.8: the `*_step` functions, driven by the node's OS threads as designed;
  - the inbound parts of §3.9, behind `inbound`.
- **Changes:**
  - P2a:121 "dash-spv `src/network/*` not reused" is reversed: dash-spv adopts `dash-net` in C1.
  - P2a:145-150's `NodeTable` is owned by `dash-net`.
  - `ChainAccess` (P2a:284-299): the reads are served by `dash-chain`'s `ChainReadSnapshot`; `process_headers` still goes to V, which calls `HeaderChain::accept`.
  - CH1 R1–R8 (P2a:445-452) are met by `dash-chain` (R1–R5, R7, R8) and CH1-node (R6).
  - `dashd-clock` becomes `dash-clock`.
  - The shutdown order (P2a:169) belongs to the host: ND1 or the FFI client.
  - The "sole loop" statements (P2a:46, 147, 728) now mean "the node's only processor".
- **Dropped:** nothing beyond DEC-82/83's removals.
- **Task:** P2a-net becomes SB6. P2a-core is the node processor on SB6.

**CH1 (PLAN row only, no design document).** It splits into:
- **SB3 `dash-chain`:** arena, hash index, header acceptance with CL hook and checkpoints, best header, `ChainReadSnapshot`, `FeedHub`, anchor, window, `HeaderStore`.
- **CH1-node** (`dashd-chainstate`): the `ChainCommand` actor on V; block-data status bits on the shared entries; `ChainView` (with `mn_list`, `best_chainlock`, `ibd`, `mtp`, `bls`, `epoch`; A1:1103-1109); writing dirty index records in B1; R6.

SB3 needs its own short design: CH1's never existed as a document.

**CH2a.** Stays whole, including a3's `RocksTuning` (§4.3(e)). Engine memory E is unchanged.

**CH2b.**
- **Stays:** the protocol, all of §4.4, the model check and the CRASH suite.
- **Changes:**
  - `SubscriberHub` wraps `dash-chain::FeedHub` (§2.8).
  - `GlobalFailStop`, `GuardedEngine` and `AbortSink` live in `dash-kv` (the code moves; the semantics don't).
  - `IndexAccess` and `ChainSnapshot` are provided by `dash-chain`.
  - Registration returns `Refused` in SPV mode (P8).
- **CH2 §10.1 P1–P9 are all met** (§2.8 table). No F-guarantee is weakened. CH2b can unhold once SB2's `dash-kv` and SB3's `FeedHub` interfaces are stubbed.

**ST2.** Stays node-only. DEC-85 makes Core-identical blk/rev optional. The DEC-84 portability defects (`flatfile/fs.rs:10-11, 246`) are an ST2 follow-up, not SB0's.

**ST3a.** Stays.
- `FaultyEngine` and `World` move with `dash-kv` after ST3a merges.
- SB2 adds an SPV-store `Sut` and `sqlite_wal_prefix.rs`.
- `rocks_wal_nonprefix.rs` stays in `dashd-store`.

**EV0** (unmerged).
- Split into `dash-evo` (shared): `ctx`, `effect`, `bls`, the `llmq` traits, `sml`, `locks`.
- And `dash-dml` (node-only): `dml`, `store` (`DmlReader`, `EvoStore`, `MemEvoStore`, `EvoDelta`), `creditpool`, `mnhf`.
- `QuorumMemberSource::members` returns `QuorumMembers<E> = Arc<[E]>` for `E: MnEntryView`. The node instance keeps `E = Arc<Dmn>`.
- `imbl` moves with `dash-dml` and leaves the mobile closure.

**EV2** (rev4, DEC-58).
- Stays as written, in `dash-dml`.
- §2.12's "dash-spv, desktop: none direct" row now reads: the node's projection feeds `dash-mnstate`, which dash-spv shares. `ColdDerived.sml_hash` slots are filled by the projection (EV4-core).

**EV4** (not designed). Split:
- **EV4-core**, early in SB4 (M2–M3): SML entry and entry hash, `merkleRootMNList`, `merkleRootQuorums`, the CbTx partial-merkle check, the coinbase-CL predicate (LQ1 §3.3.1).
- **EV4-serve**, M4 node: the mnlistdiff builder with CHAMP order (quirk 41), the evodb importer.

**LQ1** (rev4, DEC-60).
- **Stays:** every algorithm, the predicates, `log2`, the effects, the persistence contract.
- **Changes:**
  - The SML adapter (LQ1:247) becomes production, with fail-closed semantics (§2.7).
  - The member-source trait becomes generic.
  - §6.4's inverted fixture test is replaced by direct use plus the node's shadow cross-check.
  - LQ1b-F's "dash-spv in-crate test PR" becomes stage B2.

**A1.**

| A1 place | Edit |
|---|---|
| §0 (A1:22-26), §1.2 rows A1:126, 136, 139 | Rewrite for one stack. P2P is raw frames in `dash-net`; the SML is shared in `dash-evo` |
| R2 (A1:147, 448, 492, 628) | Replaced by L1–L8 (§2.3). The M1 exit criterion becomes "`check-layers.py` and `check-mobile.sh` pass" |
| §1.8 (A1:503-515) | CV1–CV4 keep their scope and stay non-blocking. CV5 becomes the SB series (SB7, SB10) and is required |
| §2.1 rule 8 (A1:551-554), A1:572, 618, 1616, 2949-2950 | Dropped. Replaced by L4, L5 and the stage discipline of §5.2 |
| §2.5 (A1:760-764) | Algorithms are shared, not lifted out |
| §2.8 (A1:831-885) | Storage by role (§4.3) |
| §2.10 (A1:962-1140) | Header acceptance in `dash-chain`, owned by V in the node |
| §2.14 (A1:1213-1407, 1223-1224) | `dash-net` + processors |
| §2.16 (A1:1430) | One BIP158 implementation (`dash-filters`) |
| §2.23 (A1:1592-1608) | Rewritten as §6.2 |
| §3.1 (A1:1685-1712) | Shared crates are host-driven (§2.9) |
| §3.8 (A1:1970) | jemalloc only in `rdashd`'s `main.rs` |
| §3.9 (A1:2008) | Adds SPV rows: L5 size, `spv-*` peak RSS, startup, header and filter sync time |
| §4 (A1:2086, 2527) | The dash-spv suites run both against rdashd (serving) and *as* rdashd `-spv` |
| D1, D8, D44 | Stand |
| D49 (testkit wraps in place) | Stands |

### 6.2 dash-spv reuse table (replaces A1 §2.23)

| dash-spv part | Disposition |
|---|---|
| `network/` (dev) | Replaced by `dash-net` + `SpvRouter` (C1). #902's facade, `PeerId`, codec, broker semantics and mock carry over |
| `sync/` managers, coordinator, pipelines | Kept as the SPV driver. They become adapters over `dash-chain` and `dash-mnstate` (B1, B2) |
| `validation/` | Replaced by `dash-consensus`/`dash-evo` calls (A1–A3) |
| `chain/checkpoints.rs` | Kept (SPV anchors), exposed as `CheckpointView`. `chain_tip.rs`/`chain_work.rs` deleted |
| `sml_engine*` | Replaced by `dash-mnstate` + LQ1 (B2) |
| `storage/` | Replaced by the SQLite store (B3) |
| `mempool/` | Kept (SPV-only) |
| `client/`, `test_utils/`, dashd suites | Kept. `test_utils` serves both products (`dash-testkit`, D49) |
| `dash-spv-ffi` | Kept. Event translation added (D1) |

### 6.3 Merged code that moves

| Code | From | To | When |
|---|---|---|---|
| `engine.rs`, `fail_stop.rs`, `schema.rs`, `table.rs`, `codec.rs`, `memory.rs`, `error.rs`; ST3a `faulty/` | `dashd-store` | `dash-kv` | SB2, after ST3a merges |
| `rocks.rs`, `flatfile/` | `dashd-store` | stay | — |
| `dash-consensus`, `dash-x11`, `dash-sha256` | stay | become mobile dependencies (A1) | — |
| `dash/src/bip158.rs` | `dashcore` | `dash-filters` (`dashcore` re-exports the old paths) | SB7 |
| `dash/src/sml` verification and engine-adjacent code (`merkle_roots.rs`, `quorum_entry/validation.rs`, `verify_message.rs`, `scores_for_quorum.rs`) | `dashcore` | `dash-evo::{sml,llmq,locks}`. `dashcore::sml` keeps data shapes as a compatibility façade | SB4 |
| `dash-spv/src/sml_engine*` | dash-spv | `dash-mnstate` | B2 |
| `dash-spv/src/network/*` (dev) | dash-spv | `dash-net` (+ `dash-spv::router`) | C1 |

---

## 7. Plan revision

### 7.1 New rows (the SB series; CV5 is placed here)

| ID | Task | When | Crates | Deps | Size | Tier (DEC-88) |
|---|---|---|---|---|---|---|
| SB1 | Layer rules L1–L8: `layers.toml`, `check-layers.py` (replaces the `deny.toml` R2 block, `r2_reach.py`, `check-r2.sh`), `mobile-deny.toml`, `check-mobile.sh` (Android NDK build, size baseline, iOS target `cargo check`), `shared-clippy.toml` | now | `contrib/node`, CI | — | S | impl S-h; rev Sol |
| SB2 | `dash-kv` split + `delete_range` (BL-38) + subset-contract docs (fixes `engine.rs:32, 101-102`) + `dash-kv-sqlite` + SPV-store `Sut` in ST3a + `sqlite_wal_prefix.rs` | M2 | `dash-kv`, `dash-kv-sqlite` | ST3a merged | M | impl O-h; rev Sol→O-h |
| SB3 | `dash-chain` (short design first: CH1's arena + anchor + window + `FeedHub`), then implementation; PF0 bench vs Core's `ProcessNewBlockHeaders` | M2 | `dash-chain` | CS0, CS2 | L | D O-h; impl O-h; rev Sol→O-h |
| SB4 | `dash-evo` split (EV0 → `dash-evo` + `dash-dml`); EV4-core; LQ1 SML adapter in production; CL/IS verification moved from `dash/src/sml` and `dash-spv/src/validation` | M2–M3 | `dash-evo`, `dash-dml` | EV0, LQ1a (algorithms) | L | impl O-h; rev Sol→O-h |
| SB5 | `dash-mnstate`: state, P2P producer with the full trust chain, SQLite persistence, projection input, shadow cross-check (`mnxcheck`) | M3 | `dash-mnstate` | SB2, SB3, SB4 | L | impl O-h; rev Sol→O-h |
| SB6 | `dash-net`: connection table, per-peer tasks and queues, P1, handshake and gates, AddrMan, BanList, dial steps, `inbound`; sim + fuzz (replaces P2a-net) | M2 | `dash-net` | P1 | L | impl O-h; rev Sol→O-h |
| SB7 | `dash-filters`: GCS, Dash element set, filter-header chain. `dash/src/bip158.rs` re-exports. The blockfilterindex (IX1) builds with it (half of CV5) | M3 | `dash-filters` | CS0 | M | impl S-h; rev Sol |
| SB8 | dash-spv stage A (A1–A4) | M2–M3 | `dash-spv`, `dash-spv-ffi` | SB1 (A2: SB4) | M | impl O-m; rev Sol |
| SB9 | dash-spv stage B (B1–B3, migrator v3) | M3–M4 | `dash-spv` | SB2, SB3, SB5 | L | impl O-h; rev Sol→O-h |
| SB10 | dash-spv stage C (`SpvRouter` on `dash-net`, C1–C2) (the other half of CV5) | M4 | `dash-spv` | SB6, Q1 | L | impl O-h; rev Sol→O-h |
| SB11 | FFI event translation (D1); B1 mobile cells (`spv-*` peak RSS, startup, size; Android emulator `agentbox-api36`); device durability check (Mac) | M4 | `dash-spv-ffi`, `node-bench` | SB9 | M | impl S-h; rev Sol |
| SB12 | rdashd `-spv` (D2): wiring, RPC subset, P8 InitErrors | M5 | `dashd-node` | SB10, ND1 | M | impl O-m; rev Sol |

### 7.2 Changed rows

| Row | Change |
|---|---|
| CV5 | Superseded: SB7 + SB10 |
| CV4 | Its "dash-spv P2P on `P2pMsg`" part is subsumed by SB10 (raw frames + base-sdk decode). The hash-newtype work stays |
| CV1–CV3 | Unchanged, post-M4/M7 |
| P2a | Becomes P2a-core (the node processor) on SB6. Unhold once SB6's `NetCore` API is stubbed |
| P2b | Depends on SB6 `inbound` |
| CH1 | Becomes CH1-node (deps SB3) |
| M2 exit | SB3 + SB6 + P2a-core + P2b + CH1-node |
| CH2b | Unhold. b1/b2 against `dash-kv` now; b3 on SB3's `FeedHub` |
| EV0 | Unhold with the SB4 split |
| EV2a/b | Unhold, crate `dash-dml` |
| EV4 | Now EV4-serve only |
| LQ1a/b | Unhold. LQ1b-F → SB4/B2 |
| LK1 | Its verification part moves to SB4 |
| ND1 | Gains host-owned shutdown for shared crates and the `-spv` InitErrors (with SB12) |
| IX1 | Builds blockfilters with SB7 |
| DEC-43 | Re-ruled per §4.3 |

### 7.3 Order, and what continues now versus what waits

- **Continue unchanged now:**
  - CS-series;
  - CH2a whole (a3 included);
  - ST3a (to merge, which unblocks SB2);
  - O2a, O3, PF0;
  - P1 (`dash-p2p-transport` is shared as designed);
  - BSX/BSX-PR;
  - desktop.
- **Start on approval:** SB1 first (one or two days, it guards everything else), then SB3, SB4, SB6 and SB7 in parallel. SB8's A1, A3 and A4 can follow SB1 directly.
- **Then:**
  - SB2 (when ST3a merges);
  - SB5 (needs SB2, SB3, SB4);
  - P2a-core and CH1-node (on the SB6/SB3 interfaces);
  - SB9 → SB10 → SB11 → SB12.
- **Still waiting:**
  - EV2b's heavy parts until EV2a, as before;
  - SB10 on Q1.

---

## 8. Performance

### 8.1 The node stays at least as fast as dashd

| Hot path | How the shared base stays off it, or out of its way | Check |
|---|---|---|
| Message intake and processing | The node processor pulls from a per-peer `Mutex<VecDeque>` exactly as Core's `vProcessMsg`. No tokio channel hop, pump task or per-type fan-out (#902's path) in node mode. Socket tasks are on tokio as A1 planned | `diff_p2p_*` + PF0 counters for `process_messages` |
| Header acceptance | `HeaderChain<S>` is monomorphised. `window = None` makes the cold-tier branch a never-taken predictable `if`. Hash once at decode (DEC-51a), X11 via XH1 | new `chain_accept_headers` PF0 bench vs Core's `ProcessNewBlockHeaders` (oracle harness) |
| Block validation, coins, flush | Untouched (CH2, CH3) | CH2's DEC-43 cell |
| MN state | The node's `MnState` takes the projection as an `Arc` and does O(changed) work per block. The shadow cross-check runs on its own task and never on V | EV2 benches + an `mnstate_apply` counter bench |
| CL/IS verification | Same blst calls as before, now in one place | LK1 benches |
| Storage | RocksDB unchanged. SQLite never appears in node full mode | layer check: `dash-kv-sqlite` is not in `dashd-chainstate`'s closure |

### 8.2 SPV must be no slower than dash-spv today

- **Existing guard.** `spv-headers-*` (A1:2008) keeps `header_sync_s` and `peak_rss_mib`.
- **New cells** (SB11):
  - `spv-filters-*`: time to a filter-synced tip, `peak_rss_mib`;
  - `spv-startup`: open-to-ready time with 2.55 M stored headers;
  - `spv-size`: the L5 number.
- **Each stage-A to stage-C PR** reports these cells against its parent.

### 8.3 Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | The shared kernel's DGW/MTP rules cost per-header work in SPV header sync | Integer arithmetic over a 24-header window, small next to X11. Measured in `spv-headers-*` |
| R2 | Windowed arena: a deep reorg below the window reads the store | Bounded by the CL floor. Mainnet reorgs below a CL are impossible by rule |
| R3 | X11 on aarch64: `dash-x11` has only a portable path for aarch64. `rs-x11-hash` (C) may be faster on phones | QEMU instruction counts (the `dash-sha256` recipe) plus an Android emulator run in SB11. Keep `rs-x11-hash` behind a feature until `dash-x11` is at least as fast on aarch64 |
| R4 | SQLite on phone flash | Buffered commits use `NORMAL`. Durable only at checkpoints. SB11 measures on the emulator; a real device is pasta's |
| R5 | First MN sync verifies every active commitment's BLS | Unchanged from today. Batched on the injected pool |
| R6 | `dash-net` at 125 peers | No global lock on the send path (per-peer queues); `PeerSnapshot` is copy-on-change. `diff_p2p` and sim at 125 peers in SB6 |
| R7 | Generic layering cost in V | PF0 counter gate per PR (perfgate, DEC-61 environment) |
| R8 | `mnxcheck` bandwidth | One mnlistdiff per 16 blocks (a few KB) plus one qrinfo per cycle |

---

## 9. Open questions

Each is a question only pasta or the maintainers can answer.

| # | Question | Recommendation |
|---|---|---|
| **Q1** | **#902's path.** SB10 needs #902's facade. Ask ZocoLini and the maintainers whether (a) #902 lands first and SB10 becomes its announced follow-up, or (b) SB10 carries the facade, credited to #902, if #902 stays stalled. This needs a human contact; agents may not comment | (a), with (b) as the fallback once SB9 is done |
| **Q2** | **Upstream acceptance.** Stages A–C make dash-spv depend on node-program crates (`dash-consensus`, `dash-evo`, `dash-chain`, `dash-mnstate`, `dash-net`, `dash-kv`) and on SQLite. DEC-87 keeps the work local, but its value depends on upstream accepting that shape. Pre-clear with QuantumExplorer and xdustinface before SB8? | Yes, before SB8. If SQLite is refused, use `redb` behind the same contract |
| **Q3** | Q3: see PLAN §7 handling. | — |
| **Q4** | **Hybrid rdashd.** Should a full node serve wallet, CL and IS state from the SPV pipeline during IBD and switch to its projection at the tip? Not Core behaviour | No for now. Revisit after M5 |
| **Q5** | **Mobile ABI set.** Is 32-bit Android (armv7) still supported? It doubles the size budget's ABI count and changes blst/X11 performance (DEC-54's CHAMP-order note doesn't matter to SPV) | arm64-only |

---

## Appendix A. Measurement method

Everything ran on agentbox on 2026-10-09, in `/work/scratch/sb0-opus/`, with `TMPDIR=/dev/shm/sb0-opus` and `CARGO_BUILD_JOBS=8`. Nothing touched `/work/chains` or `/work/bench`, and nothing was committed.

**A.1 Android size builds.** `build-android.sh <label> [features]` builds `cargo build --release --target aarch64-linux-android -p dash-spv-ffi --lib` in a detached worktree of `node/integration` at 186cae03. It uses the workspace's committed release profile (`opt-level` 3, no LTO, `panic = "unwind"`). The environment it sets:

| Variable | Value |
|---|---|
| NDK | `$ANDROID_HOME/ndk/29.0.14206865`, prebuilt `linux-x86_64` LLVM |
| `CC_/CXX_aarch64_linux_android`, `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` | `aarch64-linux-android24-clang{,++}` (API 24) |
| `AR_aarch64_linux_android` | `llvm-ar` |
| `CARGO_NDK_SYSROOT_PATH` | `$TC/sysroot`. `rs-x11-hash`'s `build.rs` panics without it |
| `BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android` | `--sysroot=$TC/sysroot --target=aarch64-linux-android24`, for `librocksdb-sys`'s bindgen |

It then strips with the NDK's `llvm-strip` and records both sizes. The probe variants add optional dependencies and one `extern "C"` function each to `dash-spv-ffi`. Each function opens a store, batch-writes 1,000 rows, deletes one, iterates, reads one, and (for `dashd-store`) flushes. That stops the linker from dropping the engine. Variants:

| Label | Feature | What it links |
|---|---|---|
| v0 | none | `dash-spv-ffi` as committed |
| v1 | `probe-node-store` | `dashd-store` with its committed `rocksdb` features (`lz4`, `zstd`, `bindgen-runtime`), through `RocksEngine` |
| v2 | `probe-rocksdb-min` | `rocksdb` 0.25 with no compression codecs, called directly: the smallest RocksDB a mobile build could ship |
| v3 | `probe-redb` | `redb` 2, pure Rust |
| v4 | `probe-sqlite` | `rusqlite` 0.38 `bundled` (`libsqlite3-sys` 0.36, the version the Android SDK already links) |
| v5 | `probe-sqlite` | as v4, in its own target directory, with `LIBSQLITE3_FLAGS` undefining FTS3, FTS3_PARENTHESIS, FTS5, RTREE, JSON1, STAT4, SOUNDEX, COLUMN_METADATA, DBSTAT_VTAB and LOAD_EXTENSION, and defining `SQLITE_OMIT_LOAD_EXTENSION`, `SQLITE_OMIT_JSON`, `SQLITE_OMIT_DEPRECATED` and `SQLITE_OMIT_SHARED_CACHE` |

v1 and v5 were built in their own target directories so that they could run alongside the others; that does not change the output. The deltas are against v0's stripped size. On iOS, SQLite links the system `libsqlite3.dylib` (no `bundled`), so its size cost there is 0 by construction; this was not built, because agentbox has no Apple toolchain.

**A.2 Engine probe.** `sb0-memprobe <engine> <dir> <n>` is a standalone crate (`memprobe/`) with `rocksdb` 0.25 (`lz4`, `zstd`), `rusqlite` 0.38 `bundled` and `redb` 2. It was built `--release` for x86_64 and run with `n = 2,550,000`, about mainnet's header count. Workload:
- write: an 80-byte header and a 32-byte filter header per height, big-endian height keys, 2,000 heights per batch, a sync every 100,000 heights and at the end;
- 200,000 uniformly random point reads of headers;
- a forward scan of the last 50,000 headers;
- a 2-second idle, then RSS from `/proc/self/statm`, peak RSS from `getrusage`, and on-disk bytes.

Engine settings:

| Engine | Settings |
|---|---|
| `rocks-node` | the sizes in `dashd-store`'s committed `RocksConfig::default()` (`rocks.rs:48-63`): 24 MiB LRU block cache, 16 MiB write buffer per CF, 32 MiB total, 8 background jobs (`available_parallelism / 4` on this 32-vCPU host), atomic flush, 10-bit bloom filters, index and filter blocks in cache. It leaves out partitioned filters and `bytes_per_sync`, which do not change the footprint at this size. Sync = `flush_wal(true)` |
| `rocks-mobile` | 4 MiB cache, 4 MiB write buffer per CF, 8 MiB total, 1 background job, 64 open files, otherwise as `rocks-node` |
| `rocks-mobile-pin` | as `rocks-mobile`, with index and filter blocks held by the table readers instead of the block cache |
| `rocks-mobile-allfd` | as `rocks-mobile`, with `max_open_files = -1` |
| `sqlite` | WAL mode, `synchronous = NORMAL`, default page cache (2 MB), one table per kind with `INTEGER PRIMARY KEY`. Sync = a passive WAL checkpoint |
| `redb` | 4 MiB cache, `Durability::Eventual` for batches, a default-durability empty commit as the sync |
| `flat` | two files, each record written with `pwrite` at `height × record size`; the dash-spv segment layout without its index. Sync = `fdatasync` |

**A.3 Caveats.**
- Load. Other agent threads shared agentbox (32 vCPUs): the load average was 25–90 during the builds and about 15–20 during the probe runs. Times can be off by a factor of two; RSS, peak RSS, disk bytes and binary sizes do not depend on load.
- Host, not phone. The probe ran on x86_64 Linux on XFS. A phone's flash, page cache and memory pressure differ. The numbers rank engines and size their caches; they do not predict phone latency. The B1 `spv-*` cells (§8.2) are where phone numbers come from.
- Synthetic records. The headers are pseudo-random bytes, so compression gains nothing. That is close to real headers (hashes and nonces) but understates RocksDB's advantage on compressible data such as MN-list entries.
- Two runs per engine (one for `rocks-mobile-pin`), so the ranges in §3.1 are min–max of two, not a variance. The raw lines are in `/work/scratch/sb0-opus/probe-results*.txt`.
- `rocks-node`'s RSS varied most (+41 vs +72 MiB) because it depends on how much of the memtables and cache had filled when RSS was sampled.

**A.4 Artefacts.** Kept in `/work/scratch/sb0-opus/`:
- `sizes/results.txt`, the six sizes;
- `probe-results*.txt`, the engine probe's raw lines.

The build script, the probe patch and the memprobe crate were removed after the run; A.1 and A.2 give enough to rebuild them.

Each variant's linked engine was checked in its stripped `.so`: the probe symbol is exported, RocksDB strings appear only in v1 and v2, FTS5 appears in v4 and not in v5. The `.so` files and target directories were then deleted to save disk. v1, v2 and v5 were built with `RUSTC_WRAPPER=` because the shared sccache server was queueing; the compiler and flags are the same.

## Appendix B. Where each overturned A1/P2a statement is handled

| Statement | Handled in |
|---|---|
| A1:22-26, 126 (two stacks) | §2.1 S-2, §6.1 A1 row 1 |
| A1:136 (`NetworkMessage` for dash-spv until CV4) | §2.10, §7.2 CV4 |
| A1:139 (dash-spv keeps its SML engine) | §2.7, §6.3 |
| A1:147, 448, 492, 628 (R2) | §2.3 |
| A1:413-416 (two codecs) | §2.10 |
| A1:468 (BS18 stays in `dash/`) | unchanged; nothing in SB0 contradicts it |
| A1:503-515 (CV track, CV5 optional) | §7.1, §7.2 |
| A1:551-554, 572, 618, 1616, 2949-2950 (isolation) | §2.3 L4/L5, §5.2 |
| A1:760-764 (algorithms lifted) | §6.1 EV0/LQ1 |
| A1:1223-1224, 1596 (dash-spv network not touched) | §2.5, §5.3 |
| A1:1430 (two BIP158 builders) | SB7 |
| A1:1592-1608 (§2.23) | §6.2 |
| A1:2086, 2527 (dash-spv as an external consumer only) | §6.1 A1 row §4, SB12 |
| D1, D8, D44, D49 | §6.1 (stand) |
| P2a:46, 145-150, 169, 728 (sole loop) | §2.5, §6.1 P2a |
| P2a:92-105 (non-goals give everything to node crates) | §2.2, §2.4 |
| P2a:121 (dash-spv network not reused) | §6.1 P2a |
| A1:1685-1712, P2a:150 (thread model vs mobile) | §2.9 |
| A1:833 (RocksDB) | §4 |
| A1:1970 (jemalloc) | §2.3 L6, §2.9 |
| A1:962-1140 (header acceptance only in V) | §2.6 |
| A1:886-961 (full-node datadir) | §4.1 (SPV store) |
| A1:2008 (only "not slower" for SPV) | §3.2, §8.2 |
