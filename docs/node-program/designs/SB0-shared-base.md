# SB0: the shared base for dash-spv and rdashd (reconciled design)

| | |
|---|---|
| **Status** | **rev6**: Sol r6's narrow review (APPROVE-WITH-CHANGES, `reviews/SB0-design-r6-sol.md`) answered per DEC-115 and DEC-116. **rev5** (1f75dbb6): Opus r6 (APPROVE-WITH-CHANGES, `reviews/SB0-design-r6-opus.md`) answered per DEC-112, which approves SB0 subject to rev5. The finding map for both is §16.1. rev4 (7d40936c, with the DEC-109 addition 53091a32) answered Sol r4 R4-M1 (§16.2), rev3 (d790b55e) Sol r3 per DEC-107 (§16.3), rev2 (4db3f499) Sol r2 per DEC-105 (§16.4), and rev1 (d07a7d92) Sol r1 per DEC-104 (§16.5). rev0 (cce49b68) was the reconciled design (DEC-81) of the Fable leg (`SB0-shared-base-fable.md`) and the Opus leg (`SB0-shared-base-opus.md`). Next: a narrow Sol r7 confirm, then the Opus confirm (DEC-112), then approval |
| **Date** | 2026-10-09 (rev0 to rev6) |
| **Code baseline** | `node/integration` @0f6751f0 (rev0's reads); rev1's additional reads at @337e713d, rev2's at @d6d1f41a, rev3's at @a1f45a07, rev4's at @88916441, rev5's at @447bc629. Paths without a prefix are relative to that tree. Its dash-spv/dash-spv-ffi differ from dev 67b307c6 in 12 files (mostly tests and benches, plus `chain/chain_work.rs` and a few FFI lines); every line cited here was read on integration. Also: PR #902 head a0e64364 (read-only, `gh pr diff`); EV0 branch `node/ev0-evo-interfaces` @3a5cb631; Dash Core v24.0.0-rc.3 (`~/workspace/dash`); Platform @6499c680c6 (rev5 also reads its `rs-sdk`, `rs-drive-abci` and `dashmate`) |
| **Design inputs** | DEC-81…DEC-116; Opus r6 (`reviews/SB0-design-r6-opus.md`); Sol r6 (`reviews/SB0-design-r6-sol.md`); EV addendum fa09d8d0 (`designs/EV-addendum-SB0.md`, adopted by DEC-109); CH2 rev5 640d37f9, approved by DEC-103 (§2.9.1 F1–F11, §10.1 P1–P10); PLAN revision 2026-10-09 (rows CH2a/b/c); A1, P2a, ST1, ST3a, EV2, LQ1; PRD |
| **Evidence** | `/work/scratch/sb0-fable/sizes/`, `/work/scratch/sb0-opus/` (sizes, probes), `/work/scratch/sb0-reconcile/` (#902 diff). Summary in Appendix A. rev1's CPM overlay is `docs/node-program/tools/cpm_sb0_rev1.py`; rev5's, which adds SB12a, is `/work/scratch/sb0-reconcile/rev5/cpm_sb0_rev5.py` (§13.4) |
| **Citation keys** | `A1:`, `P2a:`, `CH2:`, `LQ1:` are line numbers in those designs. `v24 x.cpp:N` is a line in Dash Core v24.0.0-rc.3. `#902 x.rs` is a file in the PR head. **[F]** and **[O]** name the leg an idea came from |

> **PLAN §7.** This document states what the shared kernel checks, as design properties. For current SPV-mode validation coverage, see PLAN §7 handling (CANDIDATE-SPV). Nothing below describes the validation coverage of shipped dash-spv.

---

## 0. Summary

**Shape.** One shared base that both products link. It has five parts:
- a pure consensus layer;
- one network core;
- one header chain;
- one set of MN-list, quorum and lock algorithms with one SPV state machine;
- one storage contract.

Above it sit two processors: the node's Core-exact `PeerManager` with the validation thread V, and dash-spv's request router with its sync managers. dash-spv is built *from*: its network, header and engine modules move into the shared crates, and its managers, wallet glue and FFI stay where they are.

The seven calls:

| # | Call | Decision | Side taken |
|---|---|---|---|
| 1 | **Storage** | **SQLite** (`dash-kv-sqlite`, under the shared `dash-kv` contract) for every SPV store: mobile, desktop and rdashd `-spv`. **RocksDB stays node-only** (DEC-43 confirmed for the node). Block and undo files stay node flat files. In full mode the block index lives only in the chainstate engine and is written in B1 (P10). Full mode never opens the SPV store: switching `-spv` to full re-syncs headers from genesis over P2P (DEC-104 M5). Fallback if the maintainers refuse SQLite: keep the segment files and add a persisted hash index plus Fable's crash model (SB-ST) | **[O]**, with **[F]**'s fallback |
| 2 | **Crate set** | `dash-p2p-transport`, **`dash-p2p`** (shared network core; node processor in `dashd-p2p`), **`dash-chain`**, **`dash-mnlist`**, `dash-kv`, `dash-kv-sqlite`, `dash-clock`; `dash-evo` split into shared `dash-evo` and node-only `dash-dml`; `dash-filters`. Names avoid `dash-net`, which reads as the existing `dash-network` crate | merged |
| 3 | **Scheduling** | **One network core, two processors**: shared connections, transport, handshake, gates, ping, addresses, bans and dial steps; Core's actor in the node; a tokio router in SPV that keeps #902's broker and adds per-peer queues, attempt caps, `notfound` handling and misbehaviour → discourage. #902's facade is **extended** (bounded subscriptions, `cancel`, terminal `RequestFailed`, request vs attempt ids; DEC-104 M7). The core's `push` is unconditional and returns Core's pause state (M2). No SPV hooks inside the node actor, and no OS threads in shared crates | **[O]**; **[F]**'s `PeerSelector` becomes SPV dial policy |
| 4 | **Header store** | `HeaderChain` (CH1's arena and acceptance minus V) with a **resident window** (w = 10,000 on mobile, all entries on the node) and a SQLite cold tier that keeps **every accepted header as a full record keyed by hash, with a stable id and branch status**; the active height index is a projection (§4.4, §9.5). The **anchor carries absolute `chain_work`** and **ships the 24 headers below each checkpoint**, so every header rule applies from the anchor on (post-DGW anchors). A genesis-origin chain reads the pre-DGW eras through a context sized to the selected rule (§4.1.1). The anchor is the newest checkpoint whose forward history holds everything the **actually active** quorums need (bases, work blocks, diff targets). It is chosen **once, during initial sync, while the store holds headers only**, falling back to an earlier checkpoint, ultimately genesis. Once established it never moves: a later miss costs liveness only and raises an operator diagnostic. There is no backfill (§4.3.1). Anchored clients read ancestry through an explicit partial interface, and a retained snapshot resolves cold heights from its own tip (§4.4); the node's `ChainAncestry` contract is unchanged. `best_header` follows Core's `m_best_header` update rules in both products, and SPV's ChainLock-aware fork-choice target is a separate value (§4.1) | **[O]** structure, **[F]** chain work, the window fix is new; M3/M4 per DEC-104, F1–F3 per DEC-105, M1–M3 per DEC-107; O1 per DEC-112 |
| 5 | **MN pipeline** | The shared pure algorithms and verification live in `dash-evo`: the SML core, LQ1 generic over list views, and CL/IS verification. `dash-mnlist` is the shared SPV state machine (P2P producer and trust chain). **SPV trusts commitment-verified quorum public keys only and claims no exact membership**; exact membership is node-only (DEC-104 M8). To sign-check, SPV selects Core's quorum from the authenticated scan first and then requires that quorum to be `Verified`, never re-selecting (DEC-105 F4). The scan is the one at the lock's own sign height: SPV keeps the authenticated set for each selection start block within a fixed recent-lock horizon H_keep (320 blocks on mainnet), and anything older is unavailable (§5.2.1, DEC-107 M4). MN lists and quorum rows are retained for H_keep plus a Platform-lookup window, and a Platform lookup older than that is fetched on demand (§5.3, DEC-112 O3). The node keeps its authoritative state in node crates and calls the same functions. **mnxcheck**: the node runs `dash-mnlist` in shadow mode off V, on by default except on mainnet | synthesis |
| 6 | **R2 replacement** | One checkable list, **L1–L9** (§2.3). It adds a **V layer** for dash-spv as a sibling of N, so neither depends on the other and rdashd composes them | merged, contradiction fixed |
| 7 | **Plan** | Rows **SB1–SB19**, **SB12a** (the dashcore ↔ base-sdk bridge, DEC-112 O2) and **SB18b** (`-spv` on `dashd-wallet`, M6) (+ contingent SB-ST). CV5 is superseded by SB9 + SB16 (product: SB18). P2a, CH1, CH2b, EV0/EV2/EV4, LQ1, LK1a, IX1, ND1, O13a and O16 change. **CPM rerun (§13.4): M2 exit wk 18.0 → 18.5, REL11 wk 30.0 unchanged**, with SB4/SB5 and SB6 coded against stubs (interface edges); without that overlap M2 is wk 21.5 and REL11 wk 31.0. rev5's SB12a moves no milestone. **Unhold EV0/EV2a/LQ1a on a short EV addendum at approval**: EV0 has 0.5 week of slack on the critical chain | §13 |

**What neither leg caught** (detail in §14.2):
- CH2's rev5 producer obligations F10 (`subscribe_at` under the tip lock), F11 (deregistration finality) and P10 (block index in B1), plus `tip_seq` in the snapshot;
- the anchor-window hole and the fetch gap;
- the `dash-net` name clash;
- Opus's layer contradiction (`dashd-node → dash-spv`);
- the EV0 critical-path cost;
- review routing for the SPV kernel-rule stage under PLAN §7;
- blst's pool vs "no threads in shared crates";
- `opt-level = "s"` vs hashing speed;
- NFR-CMP-04's #803, #799 and #1077;
- iOS check feasibility on Linux;
- two baseline discrepancies;
- the engine being crate-private;
- Fable's "the SML has every field" claim.

---

## 1. Baseline facts (verified)

### 1.1 dash-spv today

| Fact | Where |
|---|---|
| `DashSpvClient<W: WalletInterface, N: NetworkManager, S: StorageManager>`; the `SyncManagers` alias pins the `Persistent*Storage` types; the engine is typed on `PersistentBlockHeaderStorage` | `dash-spv/src/client/core.rs:108`, `:27-34`, `:115` |
| The MN engine is crate-private (`mod sml_engine;`). It is reachable outside the crate only through `test-utils` (`test_utils/mod.rs:34`). The FFI reaches quorums through `DashSpvClient::get_quorum_at_height` | `dash-spv/src/lib.rs:66-74`; `dash-spv-ffi/src/platform_integration.rs:42-91` |
| `MasternodeListEngine<H: BlockHeaderStorage>` with `masternode_lists_around_height` and `rotated_quorums_of_cycle`; the `sml_engine` code is ≈ 3,032 lines | `dash-spv/src/sml_engine.rs:57`, `:191`, `:206` |
| Size of the three areas that move: `network/` 6,329 lines; `storage/` 4,473 lines | `dash-spv/src/` |
| Segment store: 10,000 headers × 112 B per segment, 50,000 filter headers × 32 B, 2,000 filters, 1,000 blocks; `MAX_ACTIVE_SEGMENTS = 2`; writes go to a temp file, then `sync_all`, then rename | `storage/segments.rs:42`, `:50-51`, `:68-69`, `:77`, `:98`; `storage/io.rs:24-80` |
| `open` reads every stored header (`get_items(start..end+1)`) to rebuild `HeaderHashIndex`: O(chain), ≈ 285 MB transient from genesis (2.55 M × 112 B). The index costs 4 B per header in groups of 4,096 | `storage/block_headers.rs:116-131`; `storage/header_hash_index.rs:6-18` |
| Its docs say a crash between `truncate_above` and `persist` may reopen at the pre-truncation tip | `storage/block_headers.rs:100-101` |
| Storage version 2; the MN engine is rebuilt at load by replaying the stored message log | `storage/migrator/mod.rs:15`; `storage/masternode.rs:160-161` |
| A fork is handled by stop, truncate, restart | `client/lifecycle.rs:257` (`handle_fork`) |
| Devnet sets four process-global LLMQ overrides (`OnceLock`s in dashcore) | `client/devnet.rs:89-105`; `dash/src/sml/llmq_type/mod.rs:233`, `:266`, `:269` |
| A 100 ms tick per manager | `sync/sync_manager.rs:251` |
| `Checkpoint` carries height, hash, `prev_blockhash`, time, target, merkle root, **`chain_work`**, MN-list name, protocol version and nonce. Mainnet has 51 checkpoints, every 50,000 blocks from 0 to 2,500,000 | `chain/checkpoints.rs:16-36`, `:170` |
| Default features ship `clap` (also in the FFI), `tokio` "full", `tracing-subscriber` with `env-filter`, `tracing-appender`; dashcore with `bls`, `quorum_validation`, `message_verification`; key-wallet-manager with `parallel-filters` | `dash-spv/Cargo.toml:13`, `:17`, `:21`, `:24`, `:39-40`; `dash-spv-ffi/Cargo.toml:27` |
| 43 exported C functions: 41 `dash_spv_ffi_*` plus `ffi_dash_spv_get_quorum_public_key` and `ffi_dash_spv_get_platform_activation_height`. This explains the legs' 41 vs 43 | `dash-spv-ffi/src/*.rs`; `platform_integration.rs` |
| Callback tables `FFISyncEventCallbacks`, `FFINetworkEventCallbacks`, `FFIWalletEventCallbacks`. `on_chain_truncated` is #1104's, not merged | `dash-spv-ffi/src/callbacks.rs:268`, `:529`, `:988` |

### 1.2 In-flight PRs (read-only)

| PR | State (2026-10-09) | Relevance |
|---|---|---|
| **#902** (ZocoLini) network rewrite | Draft, conflicting, changes requested; +7,354/−10,939 in 78 files; last commit 2026-09-10. Its `NetworkManager` trait has 11 methods: `start`, `stop`, `send`, `send_to`, `broadcast`, `dispatch_local`, `request_answered`, `subscribe`, `events`, `tip`, `connected_count`. It adds a keyed request registry, priority classes and a bandwidth controller. The author deferred reputation handling and peer persistence "for a next PR" (PR conversation, 2026-08-20) | SB16 is shaped as that follow-up (§10.3) |
| #803 (xdustinface) | Draft, conflicting; block locator plus staged fork detection; touches `chain/difficulty.rs` | NFR-CMP-04 (DGW). SB12 and SB14 coordinate with it |
| #799 | Draft; MN-list merkle root in `dash/src/sml` | NFR-CMP-04. Coordinate with SB7 |
| #1077 (QuantumExplorer) | Open, changes requested, conflicting; MN-list diffs against the block coinbase | NFR-CMP-04. Coordinate with SB7/SB11 |
| #1104 | Open; wallet `truncate_above`, FFI `on_chain_truncated` | Used by SB14's reorg path |
| #1081, #921, #375, #264 | Open | #1081 (MN replay) is made unnecessary by SB11's persisted state; #921 (anchor lowering) is not adopted (DEC-104 M3, §4.3); #375 (dense checkpoints) supplies sync anchors; #264 is P1's input |

### 1.3 Node side and consumers

- `dashd-store` is the engine contract plus RocksDB plus flat files:
  - `src/{codec,engine,error,fail_stop,memory,model,rocks,schema,table}.rs` and `flatfile/`;
  - `RocksConfig::default` is a 24 MiB cache, a 16 MiB write buffer × 2, 32 MiB total and 10-bit partitioned bloom filters (`rocks.rs:48-63`);
  - `flatfile/fs.rs:10-11` uses `std::os::unix` unconditionally and `:246` calls `posix_fallocate`;
  - `KvBatch` (`engine.rs:61`) has put and delete only, and the per-engine latch `FailStop` is `pub(crate)` (`fail_stop.rs:33`);
  - CH2a (implementing) adds `delete_range`, `GlobalFailStop` and `EngineTuning`/`RocksTuning`.
- `dash-consensus`:
  - `ChainAncestry` (`src/ctx.rs:46-58`) over `HeaderCtx` (`:18`). Its contract (`:35-45`) is infallible: every issued `BlockId` stays valid for the arena's lifetime and every entry's ancestors down to genesis exist; `pow::tests::broken_chain_panics_in_debug` (`src/pow/mod.rs:192`) treats a missing ancestor as a contract violation;
  - `release.rs:50-59` `TimeSource`: v23.1.8 `AdjustedTime`, v24 `NodeClock` (mockable local time, no peer offset), selected by `UngatedRules::V24_0` (`:96-99`);
  - `get_next_work_required(chain: &dyn ChainAncestry, last, new_time, p)` (`src/pow/mod.rs:30`);
  - `check_proof_of_work` (`:77`).
- **EV0** (unmerged) has `dash-evo/src/{bls,creditpool,ctx,dml/,effect,llmq/{members,mod,snapshot},mnhf,store/,testkit}`, depending on `dash-consensus`, `dash-primitives` and `imbl =7.0.2`.
  - It defines `QuorumMembers = Arc<[Arc<Dmn>]>` and `QuorumMemberSource::members(.., effects) -> Result<QuorumMembers, MemberError>`.
  - It uses `crate::dml::Dmn` and `crate::store::StoreError`; `effect.rs:23` names `crate::dml::ProTxHash`.
- **Platform** already links SQLite:
  - `rs-platform-wallet-storage` has rusqlite 0.38 `bundled` behind `sqlite`;
  - `rs-platform-wallet` has rusqlite behind `shielded`;
  - `rs-unified-sdk-jni` defaults to `shielded`;
  - dashwallet-desktop enables platform-wallet-storage `["sqlite","secrets"]` (`rust/Cargo.toml:24`), and its `dw-appdb` uses rusqlite and refinery.

---

## 2. Layering

### 2.1 Layers

| Code | Layer | Contents | Mobile |
|---|---|---|---|
| **B** | base-sdk | `dash-primitives`, `dash-p2p-core`, `dash-params`, `dash-pkc`, `dash-pow`, `dash-types`, `dash-script` (fork `node/base-sdk`) | yes |
| **P** | pure, shared | `dash-consensus`, `dash-x11`, `dash-sha256`, `dash-evo`, `dash-filters` | yes |
| **Pn** | pure, node-only | `dash-dml`, `dash-llmq`, `dash-governance`, `dash-coinjoin` | **no** |
| **S** | shared runtime | `dash-p2p-transport`, `dash-p2p`, `dash-chain`, `dash-mnlist`, `dash-kv`, `dash-kv-sqlite`, `dash-clock` | yes |
| **V** | SPV client | `dash-spv` (library) | yes |
| **N** | node runtime | every `dashd-*` crate, `dash-coreimport` | **no** |
| **W** | legacy wallet and type stack | `dashcore`, `dashcore_hashes`, `dashcore-crypto`, `dash-network`, `dash-network-seeds`, `key-wallet`, `key-wallet-manager`, `dashcore-rpc*` | yes |
| **A** | products | `dash-spv-ffi`, `key-wallet-ffi`, `rdashd`, `rdash-cli`, `rdash-wallet`, `dash-spv-cli` (SB3) | FFI only |
| **T** | tests and tools | `dash-conformance`, `dash-testkit`, `node-l1`, `node-sink`, `node-fuzz`, `node-bench`, `dash-oracle-sys`, `test-vectors`, `dash-spv-bench`, `masternode-seeds-fetcher` | no |

**V is a sibling of N, not above it.** rdashd's `-spv` mode is composed in A: `rdashd` builds dash-spv's client with host services from `dashd-node`. `dashd-node` never depends on dash-spv, and dash-spv never depends on a node crate. This fixes the Opus leg's L2/L3 contradiction (its L2 says nothing in N may depend on A, yet `dashd-node` builds dash-spv's client).

### 2.2 Crate map

| Crate | Layer | Owns | Comes from |
|---|---|---|---|
| `dash-consensus` | P | `Header` (hashed once at decode, DEC-51a; `Header::new` its only constructor, so the hash is always its own bytes' X11; SB12a D17 removed `from_parts_hashed`, DEC-224), `Tx`, `Block`, header/tx/block rules, DGW/KGW, merkle, `ChainAncestry`, `CheckpointView` | merged |
| `dash-evo` | P | `ctx`, `effect`, `bls` (BS12 serial verify), `sml` (SML entry and hash, SML list, `merkleRootMNList`/`merkleRootQuorums`, CbTx partial-merkle check, coinbase-CL predicate), `llmq` (LQ1 algorithms generic over `MnListView`; `QuorumMemberSource`, generic `QuorumMembers<E>`, `LlmqChain`, `MinedCommitmentReader`), `locks` (CL/IS request id, sign hash, signer selection, verification over a `QuorumScanView`) | EV0 (shared half) + EV4 core + LQ1 + the verification part of LK1 + `dash/src/sml` verification |
| `dash-dml` | Pn | EV2 `MnList`/`Dmn`, build/process/undo, payee, PoSe, collateral, codecs, `EvoStore`/`resolve_list`, credit pool, MNHF, `impl MnListView for MnList`, `MnList::project_sml` (EV4-serve) | EV0 (node half) + EV2 + EV4-serve |
| `dash-filters` | P | BIP158 GCS codec, the Dash element set, filter-header chain checks | `dash/src/bip158.rs` (dashcore keeps adapter functions with the old signatures, O2) + A1's filter half of `dashd-indexes` |
| `dash-p2p-transport` | S | V1/V2 framing, BIP324 handshake, short IDs | P1 |
| `dash-p2p` | S | the network core (§3.1): connection table, per-peer reader/writer tasks and bounded queues, handshake and gate functions, decode policy, ping, addr intake, AddrMan, BanList, sans-io dial steps, DNS and fixed seeds; `inbound` feature | dash-spv `network/` + #902 peer/codec + P2a §2.3/§3.2–3.9 |
| `dash-chain` | S | `HeaderChain` (§4): arena, hash index, acceptance, best header (Core's `m_best_header` rules) and SPV's fork-choice target (O1), CL floor, anchor, window, `ChainReadSnapshot` (+`tip_seq`), `IndexAccess`, `AnchoredAncestry` (partial ancestry for anchored clients), `BlockIndexRecord` codec and `take_dirty`; `TipCell`/`FeedHub` (§6); `HeaderStore` trait | CH1's shared half + P2a's `ChainAccess` read side + dash-spv `chain/` |
| `dash-mnlist` | S | SPV MN state machine (§5.3): SML lists, quorum set with status, rotation snapshots from qrinfo, best/pending CL, ISLock cache, the trust chain, persistence rows, shadow-mode API. The type keeps the name `MasternodeListEngine` | dash-spv `sml_engine*`, core logic of `sync/{masternodes,chainlock,instantsend}`, `storage/masternode.rs` |
| `dash-kv` | S | `KvEngine`/`KvRead`/`KvBatch` (+`delete_range`), `Commit`, `FailStop`, `GlobalFailStop`, `GuardedEngine`, `AbortSink`, `DbSchema`, `Table`, codecs, `MemoryEngine`, ST3a `faulty` (test feature) | `dashd-store/src/{engine,fail_stop,schema,table,codec,memory,error}.rs` + CH2a's latch types |
| `dash-kv-sqlite` | S | SQLite engine under the `KvEngine` contract (§9.4) | new |
| `dash-clock` | S | `NodeClock` (system time, or mock time under `setmocktime`), steady time, and the peer-offset `AdjustedTime` service. `AdjustedTime` is never a v24 consensus input (§4.2) | P2a's `dashd-clock` |
| `dashd-store` | N | `RocksEngine` + `RocksTuning`, `flatfile` | shrinks |
| `dashd-p2p` | N | `PeerManager` processor (P2a actor loop, `send_messages`, headers sync, misbehaviour scores), block download (P3), relay, serving (P2b/P4/P6b), the ConnMan threads that drive `dash-p2p`'s dial steps | P2a–P6 |
| `dashd-chainstate` | N | V, `ChainCommand`, block-data status on the shared arena, connect/disconnect, CH2 coins/flush/recovery/`SubscriberHub` | CH1-node + CH2 + CH3… |
| other `dashd-*` | N | as A1:570-584. `dashd-locks` and `dashd-quorums` call `dash-evo::locks`/`llmq` | A1 |
| `dash-spv` | V | SPV driver: `SpvRouter`, sync managers as adapters over `dash-chain`/`dash-mnlist`, `SpvStore` schema on `dash-kv-sqlite`, wallet glue, mempool tracker, checkpoint data, `bridge` (every dashcore ↔ base-sdk conversion, SB12a) | existing, re-layered |
| `dash-spv-ffi` | A | C ABI; event translation layer (§10.1) | existing |
| `rdashd` | A | full mode via `dashd-node`; `-spv` mode composes dash-spv (§2.4) | A1 |

Splits worth their cost:
- **`dash-evo` / `dash-dml`** [O]. EV0 is unmerged, so the split costs no migration. `imbl` and the DML leave the mobile closure, and L6 can name a crate instead of a feature.
- **`dash-kv` / `dashd-store`** [O]. The contract becomes shared while RocksDB and flatfile stay node-only. The move is mechanical and runs after ST3a and CH2a merge (SB10).

Everything else is a feature at most: `dash-p2p/inbound` (off in dash-spv), dash-spv `log-file` and `env-filter` (SB3; the CLIs are the `dash-spv-cli` crate, not a feature).

### 2.3 The rules that replace R2 (one checkable list)

R2 (A1:147, also A1:448, 492, 628) is withdrawn together with its enforcement: the `deny.toml:48-80` entries whose reason names "A1 R2", `contrib/node/r2_reach.py` and `check-r2.sh`. Every workspace crate must be listed in `contrib/node/layers.toml` with exactly one layer. An unlisted crate fails, as in `r2_reach.py`'s first check.

| Rule | Statement | Enforced by |
|---|---|---|
| **L1 declared** | Every workspace crate has exactly one layer in `layers.toml` | `contrib/node/check-layers.py` over `cargo metadata --all-features` |
| **L2 direction** | Normal and build edges, direct and transitive, point only to allowed layers: B → B. P → B, P. Pn → B, P, Pn. S → B, P, S. V → B, P, S, W. N → B, P, Pn, S, N (W only per L3). W → B, P, W (W → W is the existing wallet stack, e.g. `key-wallet-manager → key-wallet → dashcore`; W → B, P are compatibility re-exports). A → anything but T. T → anything. **Nothing in B, P, Pn or S depends on W, V, N, A or T. N never depends on V, and V never on N or Pn** | `check-layers.py` (closure and reachability, as `r2_reach.py:58-87`) |
| **L3 W boundary** | B, P, Pn and S never reach W. N reaches W only through `allow_w = ["dashd-wallet", "dash-coreimport/wallet"]`. The feature-qualified entry is checked on two graphs: `cargo metadata` with `dash-coreimport`'s `wallet` feature off must show no N → W edge from it, and with the feature on, its W edges must all be ones that the feature adds. The hot path of N (`dashd-chainstate`, `dashd-p2p`, `dashd-mempool`, `dashd-indexes`) names no W type and reaches no W crate | `layers.toml` `allow_w`; `check-layers.py` |
| **L4 purity** | P and Pn: A1 X-1 (no I/O, threads, clocks, globals, HashMap iteration) and DEC-79's blst pooled-call bans | `check-pure.sh`, `pure-deny.toml`, `pure-clippy.toml` (add `dash-dml`, `dash-filters`) |
| **L5 shared hygiene** | S crates:<br>• never call `std::process::{abort, exit}`;<br>• never spawn OS threads (`std::thread::spawn`, `Builder`);<br>• never build a runtime or pool (`tokio::runtime::{Builder, Runtime}`, `rayon::ThreadPoolBuilder`) and never call blocking-offload or resolver entry points (`tokio::task::spawn_blocking`, `tokio::task::block_in_place`, `tokio::net::lookup_host`, `std::net::ToSocketAddrs`);<br>• never set `#[global_allocator]`;<br>• hold no network parameters in statics;<br>• never make blst pooled calls (DEC-79's list; BS12's serial verify instead; the audit stamp stays);<br>• only `dash-p2p` uses tokio, through an injected `Handle`.<br>Blocking storage work and DNS go through host-owned, bounded services injected as parameters (`BlockingIo`, `Resolver`; §7). Fatal storage errors go through `dash-kv`'s latch and the host's `AbortSink`.<br>**V (dash-spv)** also never makes blst pooled calls and runs rayon work only inside the injected pool, and dash-spv-ffi builds blst with `no-threads` (§7, DEC-112 O5) | `shared-clippy.toml` (`disallowed_methods`/`disallowed_types`) via `check-shared.sh`; `spv-clippy.toml` (DEC-79's blst bans for V); the exercised-path thread-count test on every path (§7) |
| **L6 mobile closure** | The default-feature normal closure of `dash-spv-ffi` and `key-wallet-ffi`, for `aarch64-linux-android` and `aarch64-apple-ios`, contains none of:<br>• storage: `rocksdb`, `librocksdb-sys`, `leveldb-sys`, `libmdbx`;<br>• allocators: `tikv-jemallocator`, `tikv-jemalloc-sys`, `mimalloc`;<br>• any Pn crate or any `dashd-*` crate;<br>• `clap`, `tracing-appender`.<br>`libsqlite3-sys` is allowed and pinned to Platform's rusqlite line (`links = "sqlite3"`) | `contrib/node/mobile-deny.toml` + `check-mobile.sh` (`cargo deny --target … check bans`) |
| **L7 size gate** | Stripped `libdash_spv_ffi.so` (aarch64-linux-android, `profile.mobile`) may grow by at most min(2 %, 256 KiB) per PR without a recorded ruling. Baseline in `contrib/node/mobile-size.txt` | `check-mobile.sh` with `contrib/mobile/android-env.sh` (the NDK recipe) |
| **L8 one codec below the products** | B, P, Pn, S and N decode protocol bytes only with base-sdk / `dash-consensus`. `bincode` is banned in B, P, Pn, S and N | `layers.toml` bans |
| **L9 one owner per command** | Each processor's registry maps every P2P command to exactly one handler or an explicit `Ignore` | unit tests in `dashd-p2p` (P2a's 74 rows) and `dash-spv` |

R2's six reasons (A1 digest §A.4.3) are kept as follows:

| R2 reason | Now |
|---|---|
| Wrong shapes for consensus | L2, L3, L8 |
| Platform kept off the critical path | L3 (W is below no consensus or shared code) |
| Mobile stability | L5, L6, L7: stronger than R2 because measured |
| #902 conflicts | staging (§10), not a ban |
| dash-spv's network is SPV-shaped | the processor split (§3) |
| Codec boundary | L8 |

**Installing the gates (SB1) vs closing today's violations (SB3).**
- SB1 installs every rule with a named, temporary baseline-violation list, `contrib/node/layers-baseline.toml`. Each entry gives the rule, the edge or crate, and the row that closes it. Today's entries are dash-spv's default `clap` and `tracing-appender`, and dash-spv-ffi's `clap` (L6, closed by SB3), plus anything else the first run finds.
- The list is a ratchet:
  - a violation that is not on the list fails;
  - a listed violation that no longer occurs also fails until its entry is removed.
- SB3 empties the list for L6.
- SB1's planted failures must each fail their rule:
  - direct and transitive normal and build edges N → V, V → N and V → Pn;
  - an unlisted wrapper crate;
  - a transitive W reachable from a hot-path N crate.
- A legitimate W → W chain (`key-wallet-manager → key-wallet → dashcore`) must pass.

**iOS check.** Building `aarch64-apple-ios` on Linux is likely infeasible: C build scripts (blst, SQLite, `rs-x11-hash`) need Apple SDK headers. L6 uses cargo-deny's target-aware graph, which needs no build. The real iOS build and the size number run on pasta's Mac in the QA phase (DEC-84/89; Q6).

### 2.4 Where SPV mode stops and full validation plugs in

| Concern | Shared (both modes) | SPV mode | Full mode adds |
|---|---|---|---|
| Peers | `dash-p2p` core | `SpvRouter`, outbound only, 3–8 peers, services `NODE_HEADERS_COMPRESSED`, `fRelay` = mempool tracking | `PeerManager`, `inbound`, Core's slots (8 + 2 + feelers + addnode) |
| Headers | `HeaderChain::accept` (every header rule, CL floor, chain work, min chain work) | active chain = the fork-choice target's chain (§4.1); header-only status | V sets `HAVE_DATA`/`VALID_TRANSACTIONS`/`VALID_SCRIPTS` on the same entries; active chain = V's connected chain |
| Blocks | `dash-consensus` decode and merkle | wallet-matched blocks, checked against their header's merkle root | P3 download, `CheckBlock`…`ConnectBlock`, block files |
| Filters | `dash-filters` | cfheaders checked link by link against the header chain; each cfilter against its cfheader; matching | IX1 builds, P6b serves |
| MN list | `dash-evo::sml`, `dash-mnlist` | P2P producer (§5.2) | DML projection (`dash-dml`, CH7a, EV4) is authoritative; `dash-mnlist` shadow (mnxcheck) |
| Quorums | LQ1 algorithms, commitment verification | members over the SML view; rotation from qrinfo | mined commitments (CH7b), `llmq_S`; DKG |
| ChainLocks | `dash-evo::locks` verification; CL floor in `HeaderChain` | verify, apply floor | `dashd-locks`: relay, `EnforceBestChainLock` on V, signing. `apply_chainlock` reaches `HeaderChain` as a `ChainCommand` to V, never as a call from the locks thread (m9) |
| InstantSend | verification | wallet notification | `dashd-locks` + `dashd-mempool` conflicts |
| Mempool | none | BIP37 or fetch-all tracker | `dashd-mempool` |
| Storage | `dash-kv` contract and latch | `dash-kv-sqlite` | RocksDB chainstate and subscriber DBs, flat files |
| Time | `dash-clock`; the v24 header check reads `NodeClock` only (§4.2) | `NodeClock` = system time; `AdjustedTime` only for peer-facing uses | `NodeClock` with `-mocktime`/`setmocktime`; `AdjustedTime` for peer-facing uses (version `nTime`, `getnetworkinfo` `timeoffset`, the clock warning) |

**rdashd `-spv`** (SB18):
- `rdashd` builds dash-spv's client with:
  - a `SpvStore` on `dash-kv-sqlite` in `<datadir>/spv/`;
  - `SpvRouter` on `dash-p2p`;
  - the key-wallet wallet (`WalletInterface` via key-wallet-manager) at M5. `dashd-wallet` follows at M6 as SB18b, an adapter in `rdashd` within L3's `allow_w` (Q4, DEC-112 O6);
  - host services from `dashd-node` (clock, shutdown, `AbortSink`, logging).
- It exposes the RPC subset that SPV data can answer.
- It never constructs V, the coins cache, `SubscriberHub` or `dashd-indexes`. `-txindex`, `-coinstatsindex`, `-blockfilterindex` and the other indexes with `-spv` are InitErrors (P8).
- It is the code path mobile runs, which makes DEC-81 testable on Linux CI.
- Full mode never opens `<datadir>/spv/`. Switching a datadir from `-spv` to full starts a normal header sync from genesis (§4.7).

---

## 3. One network: `dash-p2p` core, two processors

### 3.1 What is one

One `NetCore` per process and network [O]. It owns:
- the connection table (Core's `m_nodes`; P2a §2.2's `NodeTable`), with per-peer receive queues bounded by `-maxreceivebuffer` (`fPauseRecv`) and per-peer send queues under Core's **soft** `-maxsendbuffer` limit, which sets `fPauseSend` and never rejects a message (below);
- the per-peer reader and writer tasks, spawned on the host's tokio `Handle`;
- framing and transport (P1; V2 by default, V1 fallback);
- handshake functions, pre-handshake gates and the decode policy (P2a §3.2–3.3), with `LocalIdentity` as a parameter;
- ping and inactivity timeouts, and addr intake (P2a §3.5–3.6);
- AddrMan (P2a §3.7, Core bucket math) and `BanList` (ban plus discourage, Core semantics), persisted through a `NetStore` trait;
- the dial steps `opencon_step`/`addcon_step`/`dnsseed_step` as **sans-io** functions returning a `DialPlan`;
- DNS and fixed seeds;
- with `inbound`: the listener, inbound limits and `AttemptToEvictConnection`.

```rust
pub struct RawMsg { pub cmd: Command, pub payload: Bytes, pub recv_us: i64 }
impl NetCore {
    pub fn new(cfg: NetConfig, rt: tokio::runtime::Handle, store: Arc<dyn NetStore>,
               clock: Arc<dyn Clock>, wake: Arc<dyn Fn() + Send + Sync>) -> Arc<Self>;
    pub fn peers(&self) -> PeerSnapshot;                          // copy-on-change
    pub fn pop(&self, p: &PeerHandle) -> Option<RawMsg>;         // never blocks
    pub fn push(&self, p: &PeerHandle, m: OutMsg) -> PauseState; // unconditional: always enqueues, never blocks;
                                                                  // returns fPauseSend after charging the message
    pub fn disconnect(&self, p: PeerId, why: Why);
    pub fn discourage(&self, p: PeerId, why: &'static str);
    pub fn ban(&self, net: SubNet, until: Option<i64>);
    pub fn dial_tick(&self, now: i64, policy: &dyn DialPolicy) -> DialPlan;
}
pub mod handshake { pub fn version_for(..); pub fn on_version(..) -> Step; pub fn on_verack(..) -> Step; }
pub mod gates     { pub fn admit(state: &PeerState, cmd: &Command) -> Gate; }
```

`NetCore` moves raw frames. Each processor decodes a payload once, after the gates, into its own type (decode once; [F]'s rule 2, DEC-51a). It creates no thread (L5): the node's `dashd-p2p` runs P2a's opencon, addcon and dnsseed OS threads and calls `dial_tick` from them. dash-spv drives it from a tokio interval. DNS seeding resolves through the host's injected `Resolver` (L5).

**Send contract (Core's `PushMessage`, DEC-104 M2).**
- `push` is Core's `CConnman::PushMessage` (`v24 net.cpp:4878-4890`):
  - it charges the message to the peer's send size;
  - it sets `fPauseSend` when that size exceeds `-maxsendbuffer`;
  - it **always appends** the message.

  There is no admission bound, no rejection and no waiting. The returned `PauseState` is the flag after the push.
- **Charging** follows P2a §2.2.1 (`P2a-outbound.md:164`) and P2a's `NodeHandle::push_message` (`:275-276`): retained memory, not queue payload. The transport's charge for a partially sent allocation (V1 or V2) is released only when the transport releases that buffer. Pause therefore clears at the same point as in Core's accounting.
- **Overlay pushes** from other threads (relay, LLMQ, CL/IS) call the same unconditional `push`.
- **Pause checks** sit where Core has them. The node processor reads `fPauseSend` at Core's caller sites, which P2a already places:
  - no poll while paused (`P2a-outbound.md:575`);
  - `fMoreWork |= more && !fPauseSend` (`:548`);
  - `ProcessGetData`'s serving loop.

  `NetCore` never decides on the processor's behalf.
- **SPV.** `SpvRouter` keeps any hard admission budget *in its own outbox*, before it calls `push`. It sends only requests from its `RequestLedger` (per-peer in-flight cap) and handshake/ping traffic, and it never serves data, so its outbound bytes are bounded by the ledger. The core's push contract is identical for both processors.
- **Tests (SB6 acceptance):**
  - a handler that emits several messages and crosses the limit mid-sequence: every message is enqueued in order, and pause is set from the crossing push on;
  - concurrent overlay pushes from several threads: no loss, and exact charge accounting;
  - partial V1 and V2 writes: pause clears only when the transport releases the partially sent buffer, at Core's byte count (P2a's boundary tests, `:1117`, run against `NetCore`).

### 3.2 Two processors

| | Node: `dashd-p2p::PeerManager` | SPV: `dash-spv::SpvRouter` |
|---|---|---|
| Thread | one OS thread, `fMsgProcWake` condvar (P2a §3.1) | one tokio task, `Notify` wake |
| Order | Core's `ThreadMessageHandler`: shuffle, one message per peer per pass, two receive queues with the quorum priority, MN-only throttle, Core's `send_messages` sections | per-peer FIFO drain into **bounded** subscriptions (§3.2.1); order kept per peer; a full subscription stops that peer's drain, so backpressure reaches the peer's receive queue and TCP |
| Requests | Core's per-peer state (`mapBlocksInFlight`, getheaders throttle, P3) | **`RequestLedger`**: #902's `RequestKey` registry (deduplication) made per peer, with a logical **`RequestId`** per request and an **`AttemptId`** per send to one peer, plus:<br>• an attempt cap of 3, then a terminal `RequestFailed{id}` to the owning subscription;<br>• explicit `cancel(id)`;<br>• `notfound` ends the attempt and re-routes the request once.<br>#902's strict-priority classes and bandwidth controller are kept |
| Handshake | `handshake::*` in Core order | the same functions |
| Misbehaviour | Core's scores (P2a §3.10) → `discourage` | an invalid header, cfheader, cfilter, merkle proof, mnlistdiff, qrinfo, CL or IS → `discourage`; a peer that doesn't answer → `disconnect` |
| Registry (L9) | 74 commands at M2, exhaustive | exhaustive. SPV handles `headers`, `headers2`, `inv`, `cfheaders`, `cfilter`, `block`, `merkleblock`, `tx`, `mnlistdiff`, `qrinfo`, `clsig`, `isdlock`, `notfound`, `addr`/`addrv2`, `sendheaders`/`sendheaders2`, `ping`/`pong`. Everything else is `Ignore`, logged once per command |
| Dial policy | Core's `AddrMan::Select`, feelers, anchors, block-relay-only | **`SpvDialPolicy`** [F's `PeerSelector`]: #902's latency-ranked supervisor over AddrMan, DNS seeds, fixed seeds and `dash-network-seeds`' masternode seed files; pinned peers for `restrict_to_configured_peers`; required services `COMPACT_FILTERS` (`BLOOM` with mempool tracking) |
| Serving | `inbound` + P2b/P4/P6b | none |

Only `dash-spv` depends on `SpvRouter`. It implements #902's `NetworkManager` facade, **extended** as in §3.2.1, so `DashSpvClient` and the managers keep their seam. `SpvRouter` config keeps [F]'s `LightOptions` fields:
- `max_outbound` (`ClientConfig.max_peers`);
- `relay_txs` (= mempool tracking);
- `persist_addrman` (true on desktop and `-spv`, an SQLite table; false on mobile by default);
- seed policy;
- pinned peers.

#### 3.2.1 #902's facade, extended (DEC-104 M7)

At #902's head the trait has 11 methods, and `subscribe(&self, kinds: &[MessageType]) -> UnboundedReceiver<Inbound>` creates an unbounded channel per subscription (`#902 network/mod.rs`, trait at diff line 7089; `manager.rs` `subscribe`). Kept verbatim, that adds an unbounded stage after the bounded per-peer inboxes. SB16 therefore **extends** the facade rather than keeping it verbatim:

```rust
// kept as in #902: start, stop, send_to, broadcast, dispatch_local, events, tip, connected_count
async fn subscribe(&self, kinds: &[MessageType], capacity: NonZeroUsize) -> Subscription; // was UnboundedReceiver<Inbound>
async fn send(&self, msg: NetworkMessage);                // untracked messages, as #902
async fn request_answered(&self, id: RequestId);          // was RequestKey
async fn cancel(&self, id: RequestId);                    // new
impl Subscription {
    pub async fn recv(&mut self) -> Option<Inbound>;
    pub async fn request(&self, msg: NetworkMessage, class: Priority) -> RequestId; // tracked; this subscription owns it
}
pub enum Inbound {
    Message { peer: PeerId, msg: NetworkMessage, request: Option<(RequestId, AttemptId)> },
    RequestFailed { id: RequestId, key: RequestKey, attempts: u8, last: FailReason },  // terminal; to the owner only
}
```

- **Identity.**
  - `RequestKey` stays #902's deduplication key.
  - `RequestId` names one logical request, for its owner's lifetime.
  - `AttemptId` names one send of it to one peer.
  - A retry after a timeout, `notfound` or disconnect is a new attempt under the same `RequestId`, sent to another peer.
- **Completion.**
  - The first matching response completes the request and is tagged with `(RequestId, AttemptId)`.
  - After the third failed attempt, the owner receives exactly one `RequestFailed`, and no one else does.
- **Cancel.**
  - `cancel(id)` removes the ledger entry and its in-flight attempts.
  - A late response to a cancelled or completed request is delivered untagged (`request: None`) to the subscriptions for its type, as an unsolicited message. It never completes or revives a request.
- **Fanout and backpressure.**
  - A message goes to every subscription whose kinds include its type. No message is ever dropped.
  - If a target subscription is full, the router parks *that peer's* drain with the message at its head until the target frees space, while other peers continue. Per-peer order holds.
  - A parked peer's receive queue then fills, sets `fPauseRecv` and stops the reader, which is TCP backpressure.
  - A subscription that stays full longer than `stall_report` emits one `NetworkEvent::SubscriberStalled { kinds }`.
  - SB16 sets the per-type capacities so that Σ capacity × maximum message size stays within the SPV peer-queue budget (§8.2).
- **Kept from #902:**
  - deduplication by `RequestKey`;
  - the strict-priority classes;
  - the bandwidth controller;
  - the mock's behaviour, apart from the extension.
- **Consumer migration** (SB16's PR carries it as an NFR-CMP-01 consumer note):
  - dash-spv's managers take `Subscription` instead of `UnboundedReceiver`, pass a capacity, issue tracked requests through `Subscription::request`, and replace their own retry timers with `RequestFailed` handling;
  - `DashSpvClient<W, N, S>` keeps its bounds;
  - `dash-spv-ffi/src/client.rs` and `dash-spv-bench` change only the constructor configuration;
  - downstream constructors (platform-wallet) are listed in the note.
- **Test migration.**
  - #902's mock gains `cancel` and bounded subscriptions.
  - Tests that assert unbounded buffering are rewritten to assert backpressure.
  - The deduplication, priority, controller and mock tests move `git mv`-style, so blame survives.
  - SB16's PR lists every changed test with its reason.
  - Coordination with #902's author goes through pasta (Q1).
- **Tests (SB16 acceptance):**
  - a stalled subscriber: the peer's drain parks, `fPauseRecv` is observed, nothing is lost, and the stall event fires once;
  - multi-topic fanout with one full target: both targets get the message, in order, once space frees;
  - three failed attempts on three peers: exactly one `RequestFailed`, to the owner;
  - late responses after `cancel`: delivered untagged, with no completion and no retry.

### 3.3 Why two processors and not a Light profile inside Core's actor

- **No SPV hooks inside the node actor.** Fable's Light profile adds an `OutboxScheduler` and a `RequestTracker` to Core's `send_messages` and the dispatcher. Those are observable scheduling changes in the node's processor, which P2a made Core-exact (P2a §3.12, PQ quirks).
- **No OS threads in shared crates.** A shared actor needs a dedicated thread with Core's wake cadence. A phone needs neither (L5).
- **#902's broker stays whole** in its scheduling (deduplication, priority classes, bandwidth controller); its subscription interface is extended (§3.2.1). It gets its speed from any-peer scheduling, which a node must not use (P2a §3.4: per-peer locators and judgement).
- **The node does not wait for #902.** `dashd-p2p` sits on SB6 from the start. The Fable leg gated its SB1 on #902 merging; that puts an external, stalled PR on the M2 path.

DEC-81's "one network loop" is honoured as one implementation of connections, transport, handshake, gates, peers, addresses and bans. One place fixes a protocol bug. DEC-112 takes that reading (Q3, a manager call that pasta may override).

### 3.4 Backpressure and memory

- A receive queue over its flood size sets `fPauseRecv` and stops the reader, which applies TCP backpressure.
- A send queue over its limit sets `fPauseSend`. The push is still accepted (§3.1), and processors stop polling or serving that peer at Core's sites.
- SPV defaults: 1 MiB receive and 256 KiB send per peer, about 8 peers.
  - A receive queue admits a whole message of up to `MAX_PROTOCOL_MESSAGE_LENGTH` = 3 MiB (`v24 net.h:84`) before `fPauseRecv` stops the reader.
  - A peer's worst case is therefore ≈ 1 MiB + 3 MiB + the send budget, ≈ 34 MiB across 8 peers (m5).
- SPV memory is bounded per peer: receive by `fPauseRecv`, send by the `RequestLedger`'s in-flight cap, and subscriptions by their capacities (§3.2.1). There is no unbounded channel anywhere (pending measurement in SB17).
- AddrMan is in memory, about 0.5 MB when seeded from DNS, fixed and masternode seeds.

---

## 4. One header chain: `dash-chain`

### 4.1 `HeaderChain`

```rust
pub struct HeaderChain<S: HeaderStore> {
    arena: Arena,                    // chunks of entries; immutable prev/skip/height/hash; seqlocked status word (CH1 R1/R2)
    index: HashIndex,                // hash -> BlockId for resident entries, concurrent (R1)
    best_header: BlockId,            // Core's m_best_header: its update sites and load recompute, exactly (O1); never null after load (R5)
    fork_target: Option<BlockId>,    // SPV only: the most-work eligible header (O1, R6-M1; below). None in the node
    active: ActiveChain,             // height -> BlockId. SPV: the fork target's chain, resident for the window (§4.4). Node: V's connected chain
    anchor: Anchor,                  // §4.3
    window: Option<u32>,             // §4.4. SPV: Some(10_000). Node: None
    cl_floor: Option<BlockId>,       // Core's bestChainLockBlockIndex
    params: Arc<ChainParams>, clock: Arc<dyn Clock>, store: S, dirty: DirtySet,
    tip: TipCell<ChainEvent>,        // §6: the tip lock, seq, feeds, the published snapshot
}
impl<S: HeaderStore> HeaderChain<S> {
    pub fn accept(&mut self, b: HeaderBatch, from: Option<PeerId>) -> Result<HeadersResult, ChainError>; // Core ProcessNewBlockHeaders
    pub fn apply_chainlock(&mut self, cl: VerifiedChainLock) -> Result<ClOutcome, ChainError>;          // floor; Core's EnforceBlock marking.
                                                                                                         // Node: a ChainCommand on V (m9)
    pub fn set_active_tip(&mut self, to: BlockId) -> Result<TipChange, ChainError>;                     // the only tip writer (P2); evicts (§4.4)
    pub fn best_header_walk_back(&mut self, marked: BlockId);    // Core's walk-back sites: if marked is best_header, its prev (O1)
    pub fn best_header_invalid_chain(&mut self, invalid: BlockId); // InvalidChainFound: invalid is best_header's ancestor -> active tip
    pub fn fork_target(&self) -> Option<BlockId>;                // SPV: read only by dash-spv's header task, for set_active_tip
    pub fn snapshot(&self) -> Arc<ChainReadSnapshot>;   // { tip, tip_seq, best_header, active, bls, epoch }, one state
    pub fn take_dirty(&mut self) -> Vec<IndexWrite>;    // node: written by CH2 in B1 (P10). SPV: the header step's write set (§9.5)
    pub fn load(records: impl Iterator<Item = BlockIndexRecord>, ..) -> Result<Self, LoadError>; // node; PoW vs stored hashes
    pub fn open(store: S, anchor: Anchor, window: u32, ..) -> Result<Self, ChainError>;          // SPV; O(window), §4.4
    pub fn complete(&self) -> Option<&dyn ChainAncestry>;     // Some only for Anchor::Genesis + window None (the node)
    pub fn anchored(&self) -> &dyn AnchoredAncestry;          // every configuration
}
pub trait HeaderStore {                                       // SPV cold tier (§9.5); the node's is never called
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError>;
    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError>;
    fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError>; // live projection: live queries only (§4.4)
    fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError>; // every branch; used by open
}
pub struct HeaderRecord { pub id: BlockId, pub height: u32, pub header: Header /* 80 B, carries prev */,
                          pub skip: Option<BlockId> /* Core's pskip */, pub status: Status, pub chain_work: Work }
```

The arena entry carries a block-data slot (`data_pos`, `undo_pos`, the data status bits). Only CH1-node writes it, under the seqlock. In SPV it stays empty, and `IndexAccess::{have_data_pos, undo_pos}` return `None`. `IndexAccess` (`lookup`, `ancestor`, `lca`, `height`, `have_data_pos`, `undo_pos`, `status`), the `BlockIndexRecord` codec and `take_dirty` are what CH2 §2.11 asks of "CH1, or SB0's shared base" (CH2:1397).

**Best header and SPV's fork-choice target (DEC-112 O1).** `best_header` is Core's `m_best_header` in both products. It is path-dependent, not a maximum over a predicate:
- **On acceptance** it moves only for a header added with a `BLOCK_VALID_MASK` status, and only on strictly greater work, so the first-seen header wins a tie (`v24 node/blockstorage.cpp:111-115`). A header stored with `BLOCK_CONFLICT_CHAINLOCK` does not move it (`:116-119`; `validation.cpp:4693-4698`).
- **Walk-backs.** It steps back one `prev` when the block being marked failed or conflicting, or the tip being disconnected, **is** `best_header` (`validation.cpp:3951-3953`, `:3969-3971`, `:4089-4090`, `:4107-4108`). `InvalidChainFound` sets it to the active tip when the invalid block is its ancestor (`:2043-2044`).
  - These are `best_header_walk_back` and `best_header_invalid_chain`, called exactly where Core updates the field.
  - In the node, V calls them in `InvalidateBlock`, `MarkConflictingBlock` and their disconnect loops, and in `InvalidChainFound` (CH1-node).
  - In SPV, `apply_chainlock` calls the walk-back for each block it marks. SPV connects no blocks, so it has no `InvalidChainFound` site.
- **A new ChainLock** follows `EnforceBlock`. It marks the sibling roots of the locked path that are not on the active chain (`:4047-4077`), plus the blocks it disconnects (`:4112-4113`: "this may not be all descendants"). If the locked block is already active, nothing is marked and `best_header` stays.
- **At `load`** it is recomputed over every entry that is `IsValid(BLOCK_VALID_TREE)`, conflicting entries included, with Core's `CBlockIndexWorkComparator` (`:5359-5360`; `node/blockstorage.cpp:32-49`). The comparator prefers more work, then the lower sequence id. Core's `nSequenceId` is 0 for a header-only entry (`blockstorage.cpp:100`) and is assigned when block data arrives (`validation.cpp:4288`).
  - Core's last tie-break is the entry's address, i.e. allocation order. The shared code uses the arena id there.
  - SB4's restart vector has no equal-work tie, so no oracle comparison depends on that choice.
- **Readers.** The node's header-sync start, its `getheaders` locator (`P2a-outbound.md:869-878`, `:919`) and `getblockchaininfo.headers` read `best_header`, as Core's do.

**SPV's fork-choice target** is a separate value: the most-work **eligible** accepted header, with the first-seen header winning a tie (DEC-115 R6-M1). A header is eligible only if both hold:
- **(a) No entry on its path is failed or `BLOCK_CONFLICT_CHAINLOCK`.** This is Core's `FindMostWorkChain` walk (`v24 validation.cpp:3533-3575`): from the candidate down to the active chain, a failed or conflicting entry rejects the candidate. The candidate's own flag is not enough, because `EnforceBlock` marks only sibling roots and the blocks it disconnects, "not all descendants" (`:4112-4113`).
- **(b) It descends from the `cl_floor` block, when there is one**: its ancestor at the floor's height is the floor block. This also covers a lock on a block that is already active, where Core marks nothing. *Superseded by erratum E2 (§17, DEC-136): (b′), "does not conflict with the floor".*

How the rules apply:
- **Choosing.** The header task picks the most-work eligible header. An ineligible candidate leaves the candidate set, as in Core (`:3550-3570`); a header accepted later on top of it is checked on its own.
- **Writes.** The walk only reads statuses. It writes none, so SPV's status bits stay those that acceptance and `EnforceBlock` give.
- **Cost.** (b) costs one ancestor lookup over skip pointers, and (a) stops at the active chain.
- **Cold reads.** Where the path leaves the window, the walk reads cold records (§4.4). A read error trips the latch, as any cold read does.
- **Restart.** `open` recomputes the target by the same rules, so a restart picks the same tip.
- **Every published SPV tip respects the floor.** `apply_chainlock` and the `set_active_tip(fork_target)` that follows it are one header step. **In SPV mode only**, `set_active_tip` refuses a target that does not descend from `cl_floor`, with a debug assertion and a `ChainError`. *Erratum E2 (§17): it refuses a target that conflicts with `cl_floor`, and a published tip may trail the floor.* The node's V also writes through `set_active_tip`, and in Core the node's tip legitimately trails a ChainLock: a lock recorded from a header whose block hasn't arrived yet, and every intermediate tip while `EnforceBlock` disconnects back to the fork point. So the check is off when the node writes (rev6.1, Opus r7 R7-O1).
- **Sol r6's witness.** Branches A, B and C share height 80 and end at A100, B110 (active) and C105 (stale). A ChainLock on A100 marks B81 and C81 and disconnects B, while C82…C105 stay unflagged. C105 fails (a), since C81 is conflicting, and fails (b) as well, so the target is A100.
- Only dash-spv's header task reads `fork_target`, to call `set_active_tip`.
- The node never computes it (`None`); V's candidate set chooses the node's tip.

#### 4.1.1 Ancestry: complete for the node, partial for anchored clients (DEC-104 M4, DEC-105 F2)

- **Node.** `dash-consensus::ChainAncestry` (`ctx.rs:35-58`) is **unchanged**. It is infallible, issued ids stay valid for the arena's lifetime, and every ancestor down to genesis exists. `HeaderChain::complete()` returns it only for `Anchor::Genesis` with `window = None`, which is the node's configuration: every entry is resident and none is ever evicted. In this configuration `BlockId` is the arena index for the process lifetime. It is never persisted, because records are keyed by hash in B1 and ids are reassigned at `load`.
- **Anchored or windowed clients (SPV).** They read ancestry through an explicit partial interface:

  ```rust
  pub trait AnchoredAncestry: Sync {
      fn floor(&self) -> u32;                         // anchor height − 24 (the pack's lowest); 0 for genesis
      fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError>;
      fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError>; // None above id's height
      fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError>;
  }
  pub enum AncestryError { BelowFloor { height: u32 }, Storage(StoreError) /* latch already tripped */ }
  ```

  `BelowFloor` is an ordinary answer: the history is not held. `Storage` means the latch has tripped (§4.4).
- **Header rules on an anchored or windowed chain (DEC-105 F2).** `dash-consensus`'s rule functions keep their `&dyn ChainAncestry` signatures and are called unchanged. For each header, `accept` first builds a `RuleContext` that holds every entry the selected rule can read, then passes it to the rule functions.
  - **Lookback R.** `dash-chain` computes R from `PowParams` and the next height h, following Core's era dispatch (`v24 pow.cpp:170-212`). R is never below MTP's 11. Nothing is hard-coded; the mainnet column is documentation.
    - R is the rule's whole **read envelope**: every entry the unchanged function reads, not only the entries that contribute to its result (DEC-107 M1).

    | Era (next height h) | What Core reads | R on mainnet |
    |---|---|---|
    | DGW: h ≥ `nPowDGWHeight` (34,140) | the parent and 23 predecessors (`pow.cpp:84-111`) | 25 (the 25th is a spare) |
    | KGW: `nPowKGWHeight` (15,200) ≤ h < DGW | up to `PastBlocksMax` = 7 · `nPowTargetTimespan` / spacing contributing entries from the parent, plus the predecessor that the exit guard reads before it tests the bound (`:35-36`, `:64-65`; `dash-consensus/src/pow/kgw.rs:34-37`, `:64-68`). Where the walk stops depends on timestamps (`:15-79`) | 4,033 (4,032 contributing + the guard read) |
    | BTC, h < KGW and h a multiple of the interval | the ancestor at parent − (interval − 1) (`:162-167`), with interval = `DifficultyAdjustmentInterval()` = `nPowTargetTimespan` / spacing (`consensus/params.h:184`) | 576 |
    | BTC, other heights with `fPowAllowMinDifficultyBlocks` | the walk back to the last interval boundary (`:142-157`) | ≤ 576 (not on mainnet) |
    | BTC, other heights; devnet's `nMinimumDifficultyBlocks` prefix | the parent only | 11 (MTP) |

  - **Building the context is the fallible step.** It covers the parent's branch from height `parent.height + 1 − R` (or 0) up to the parent. Resident entries are borrowed from the arena in place, with no copy. Entries below the resident window are loaded through `AnchoredAncestry` from the cold records (§4.4). `Storage` trips the latch, and the batch fails with `ChainError::Storage`.
    - **`HeaderCtx.mtp` is filled lazily** (m10). A context entry's `HeaderCtx` (`dash-consensus/src/ctx.rs:17-34`) carries `mtp` and `chain_work`. For the lowest ten entries of an anchor pack, `mtp` would need headers below the pack, and no rule in the envelope reads it: MTP reads only the parent's. The context computes `mtp` only where the envelope reads it, and a debug assertion fires on any other read.
  - **What each configuration reads.**
    - After a **post-DGW checkpoint anchor**, every next height is in the DGW era, so R = 25. At anchor + 1 the context is exactly the anchor pack (§4.3). The 25-header pack stays the checkpoint format.
    - A **genesis-origin SPV chain** (the windowed fallback to genesis, §4.3) has floor 0 and reads the BTC and KGW eras with their real R. The node is not affected: it calls the rules with its complete arena (`complete()`). With w = 10,000 ≥ 4,033, an in-order sync keeps every context resident. Only a stale pre-DGW branch extended after its entries were evicted loads cold records.
    - `BelowFloor` arises only for a header whose parent is a context entry, i.e. a branch that forks below the anchor. An anchored client trusts the checkpoint and never follows such a branch. The header is ignored without blame (`ChainError::BelowAnchor`) and gets no record.
  - **Inside the rules, the context is infallible.** It answers every `header`, `ancestor` and `prev` read within its range, so the partiality is never observable. A read outside the range would mean the R table is wrong: it panics in debug, like `broken_chain_panics_in_debug`. In release it rejects the batch with an internal error and does not blame the peer. SB4's gate exercises every row of the table, including a maximum-length KGW walk whose guard read is resident and, separately, cold.
- **LQ1 and `dash-mnlist`** read quorum bases and work blocks through `LlmqChain`, a view with an associated error (§5.1, m3; EV addendum §2.2). The node implements it with `ChainAncestry` (`Infallible`) and SPV with `AnchoredAncestry`.
  - In SPV, `BelowFloor` is an ordinary answer: the commitments that need that history are `Unavailable`. During initial sync it fails coverage (§4.3.1 step 2); afterwards it raises step 4's diagnostic, and the anchor does not move.
  - Failed DKG rounds can keep a quorum active whose history is arbitrarily deep (§4.3.1), so nothing in SPV assumes `BelowFloor` is unreachable.
- **Context vs accepted entries.**
  - The anchor itself is an accepted entry (`VALID_TREE`, trusted as checkpoint data).
  - The 24 headers below it are **context only**: they get ids and records with status `CONTEXT` so that `AnchoredAncestry` can return them, but they are never in `active`, never `best_header`, never a diff target or work block, and never reported as chain entries.
  - `CONTEXT` is a flag of the SPV record only. It never takes a bit in the shared status word, whose layout is Core's `nStatus` (`BLOCK_CONFLICT_CHAINLOCK` = 128, `v24 chain.h:118`) as the node's `BlockIndexRecord` codec persists it (n1).

### 4.2 Acceptance

Both products apply the same rules, from `dash-consensus` (CS0, CS2, CS3), with Core's reject strings:
- X11 PoW against the header's own target;
- `get_next_work_required` (DGW/KGW) against `pprev`;
- median time past;
- the future-time limit against **`NodeClock`**, i.e. mockable local time without any peer offset: `block.Time() > NodeClock::now() + MAX_FUTURE_BLOCK_TIME` → `time-too-new` (`v24 validation.cpp:4509-4510`). `dash-consensus` selects it as `TimeSource::NodeClock` for v24 (`release.rs:50-59`, `:96-99`; DEC-104 M1). `AdjustedTime` is never an input to this check. Tests (SB4): candidates timestamped around now + 7,200 s, with a median peer offset of +3,600 s, −3,600 s and none, and under `setmocktime`. The verdict follows `NodeClock` alone, in both products;
- version gates by height;
- Core's checkpoint rule, `bad-fork-prior-to-checkpoint` (`v24 validation.cpp:4492-4500`), over a `CheckpointView`. The node's view is Core's chainparams list (39 mainnet entries; `-checkpoints`), never dash-spv's 51 anchor checkpoints (n2);
- minimum chain work for IBD latching (FR-CHS-06), which needs absolute work (§4.3);
- the ChainLock rules of Core's `ChainstateManager::AcceptBlockHeader` (`v24 validation.cpp:4598`):

| Core behaviour | Where |
|---|---|
| A header already indexed with `BLOCK_CONFLICT_CHAINLOCK` → `duplicate` | `v24 validation.cpp:4616-4619` |
| Parent flagged → `bad-prevblk-chainlock` | `:4643-4646` |
| A header conflicting with the best ChainLock (`HasConflictingChainLock`, `chainlock/chainlock.cpp:73-95`, against `bestChainLockBlockIndex`) is added to the index with the flag and refused as `bad-chainlock` | `:4693-4698` |
| A new ChainLock marks the sibling roots of its path that are not on the active chain (`EnforceBlock`, `:4047-4077`) and the blocks it disconnects; `best_header` moves only by Core's walk-backs (§4.1, O1); SPV's tip follows §4.1's eligibility, which also holds when nothing is marked (R6-M1) | `:4113-4122` |
| `ConnectBlock` checks again (node only) | `:2516-2518` |

`cl_floor` is the best ChainLock whose block is in the index. A ChainLock for an unknown header waits in `dash-mnlist`'s pending set and does not move the floor. `ContextualCheckBlockHeader` (`v24 validation.cpp:4469`) is one function in `dash-consensus` (CS3), called with the node's complete `HeaderChain` or with an SPV `RuleContext` as the `ChainAncestry` (§4.1.1).

### 4.3 The anchor (corrected)

```rust
pub enum Anchor {
    Genesis,
    Checkpoint { height: u32, hash: BlockHash, chain_work: Work /* absolute, from checkpoint data */,
                 window: [Header; 25] /* anchor header and the 24 below it, hash-linked */ },
}
```

- **Absolute chain work** [F]. The checkpoint data already carries it (`chain/checkpoints.rs:30`). Minimum chain work and `getblockchaininfo`'s `chainwork` need absolute work.
  - The Opus leg's "relative work suffices, every candidate shares the anchor" holds for fork choice only.
  - That leg also lists min-chain-work among its acceptance rules, which contradicts it.
- **The window ships with the checkpoint.** DGW reads the 24 headers below a header and MTP reads 11, so the first 24 headers above an anchor have no inputs.
  - **Fable's rule** relaxed those 24 to continuity plus own-target PoW, pinned by "the next sync anchor's hash when one exists". When the anchor is the latest checkpoint, which is every new wallet, no next anchor exists. An eclipsing peer could then seed easy targets, and DGW would carry them forward.
  - **Opus's rule** fetches the 24 below with a `getheaders` "whose locator is the checkpoint 25 blocks back". It cannot work: with checkpoints 50,000 blocks apart, the client knows no hash 25 below the anchor, and a locator of `prev_blockhash` returns headers *from* the anchor.
  - **Resolution:** the checkpoint data carries the anchor header and the 24 headers below it, 80 B each (≈ 2 KB per checkpoint, ≈ 100 KB for mainnet's 51).
    - At construction, `dash-chain` checks that the anchor header hashes to the checkpoint hash and that the 24 hash-link to it. They are as trusted as the checkpoint itself, and **every rule applies from anchor + 1**.
    - The checkpoint generator (#375's tool, or `masternode-seeds-fetcher`'s dashd access) emits them.
    - **Fallback** for a checkpoint without a window: anchor at the nearest lower checkpoint that has one (or genesis), and download the gap through the full rules.
  - **Post-DGW checkpoint anchors only.** Core's DGW reads the parent and 23 predecessors (`pow.cpp:84-111`) and MTP reads 11, so 25 headers cover every rule from anchor + 1. A checkpoint below the network's DGW height, where BTC or KGW retargeting reads up to 576 or 4,033 entries, is never an anchor.
    - A client with no eligible checkpoint anchors at genesis. It syncs the pre-DGW eras through §4.1.1's rule-sized `RuleContext`, windowed or not (DEC-105 F2).
    - On mainnet every checkpoint dash-spv bundles above genesis (50,000, 100,000, …) is post-DGW (DGW at 34,140).
- **Sync anchors** [F]. #375's dense checkpoints pin segment ends for parallel download (§4.5). They never let a header skip a rule.

#### 4.3.1 Which checkpoint: the history the active quorums need (DEC-104 M3, DEC-105 F1, DEC-107)

The MN pipeline reads history the header pack does not hold. Each piece of it must be an **accepted entry at or above the anchor**; context headers do not count (§4.1.1). The anchor is chosen so that this history lies above it.
- The choice is made **once, during initial sync, while the store holds headers only** (step 2). Once established, the anchor never moves (step 3).
- There is **no backfill** below an anchor, no `lower_anchor` and no runtime re-anchor, and #921's anchor lowering is not adopted.

**The active set is counted in mined commitments.** For each LLMQ type, Core's active set at a block is `GetMinedAndActiveCommitmentsUntilBlock` (`v24 llmq/blockprocessor.cpp:818-832`):
- **non-rotated:** the last A = `signingActiveQuorumCount` *mined* commitments (`GetMinedCommitmentsUntilBlock`, `:687-729`);
- **rotated (DIP24):** the last mined commitment of each quorum index (`GetLastMinedCommitmentsPerQuorumIndexUntilBlock`, `:731-794`).

A failed DKG round mines a null commitment, which returns before any mined record is written (`:369-378`). Neither scan has an age bound. A quorum therefore stays active, however old, until enough later rounds succeed (LQ1 §3.9, `LQ1-quorum-members.md:845`).

SPV authenticates this same set:
- an `mnlistdiff`'s quorum list (`v24 evo/smldiff.cpp:34-35`) and the CbTx `merkleRootQuorums` (`evo/cbtx.cpp:65-156`, through `GetQcHashes`, `blockprocessor.cpp:470-472`) are both built from it;
- for each index's *actual* last commitment, `qrinfo` carries the snapshots and diffs three cycles below it (`v24 llmq/snapshot.cpp:195-229`).

**Required history.** For an authenticated active set T, `required(T)` collects the following for each commitment in T of a type SPV uses (§5.2 step 5):
- its base block (the `quorumHash`);
- its work blocks, which are also the diff targets that deliver the lists LQ1 reads (`WORK_DIFF_DEPTH` = 8, `v24 llmq/snapshot.h:40`):
  - non-rotated: base − 8 (`utils.cpp:715-720`);
  - rotated, with cycle start s = base − `quorumIndex`: s − k·C − 8 for k = 0…3. These are the new quarter's list and those of the three previous quarters (`utils.cpp:491`, `:700-701`). qrinfo's `extraShare` (H − 4C) is the k = 3 term of a previous-cycle commitment that is still active.

**Regular-DKG sizing estimate L_est.** When every DKG round succeeds, the deepest element of required(T) for a lock checked at the tip lies L below the tip. L has three parts:
- `SIGN_HEIGHT_OFFSET` = 8 (`v24 llmq/quorumsman.h:210`): a lock at the tip is checked against the active set at tip − 8;
- the age of the newest base: up to one interval plus the mining window, because a commitment can be mined as late as base + `dkgMiningWindowEnd` (`v24 llmq/blockprocessor.cpp:311-323`);
- the reach below that base:
  - **rotated:** the k = 3 work block of the previous cycle, which is still active while the new cycle mines;
  - **non-rotated:** the work block of the oldest of the A active quorums.

With C = `dkgInterval`, W = `dkgMiningWindowEnd` and A = `signingActiveQuorumCount`, this gives:
- **rotated:** L = 5C + W + 16;
- **non-rotated:** L = A·C + W + 16.

**L is an estimate for regular DKG, not a maximum.**
- Each failed round pushes the surviving commitment one interval deeper, without limit: for one index (rotated), or for the type (non-rotated).
- Sol r2's witness:
  - anchor 2,500,000 and tip 2,501,506, so tip − anchor = L_est;
  - one rotated index was last mined at cycle base 2,499,840;
  - its k = 3 work block, 2,498,968, is 2,538 below the tip, and both the block and the base are below the anchor.
- L_est, with the recent-lock horizon H_keep (§5.2.1), picks the first candidate and sizes the resident window. Nothing relies on it as a bound.
- **Clipped at genesis (Sol r4 R4-M1).** On a chain shorter than L_est, an element of required(T) can fall at a negative height. Such an element does not exist:
  - Core treats that previous cycle as absent (`v24 llmq/utils.cpp:491-494`), and LQ1 follows it;
  - the element is dropped from required(T), so it is never requested and never counts as uncovered.

The figures below take C, W and A from the LLMQ parameters in `v24 llmq/params.h`, mapped to each network's CL, IS and Platform types by its chainparams:

| Network | Type → L (C, W, A) | **L_est** (regular DKG) | Not verified by SPV (`Skipped`) |
|---|---|---|---|
| mainnet | CL `llmq_400_60` → 1,196 (288, 28, 4); IS `llmq_60_75` (rotated) → **1,506** (288, 50); Platform `llmq_100_67` → 610 (24, 18, 24) | **1,506** | MNHF `llmq_400_85` (would be 2,368) |
| testnet | CL `llmq_50_60` → 610; IS `llmq_60_75` (rotated) → **1,506**; Platform `llmq_25_67` → 610 | **1,506** | — (MNHF is `llmq_50_60`) |
| devnet | CL `llmq_devnet` → 130 (24, 18, 4); IS `llmq_devnet_dip0024` (rotated) → **276** (48, 20); Platform `llmq_devnet_platform` → 130 | **276** | — |
| regtest | CL `llmq_test` → 82 (24, 18, 2); IS `llmq_test_dip0024` (rotated) → **156** (24, 20); Platform `llmq_test_platform` → 82 | **156** | — |

- On every network the rotated IS type sets L_est. Under regular DKG the non-rotated types stay below it.
- Commitments of types SPV does not use are `Skipped` (§5.2). They are authenticated by `merkleRootQuorums` and are never verified or selected. They are not in required(T), so their history does not move the anchor (mainnet MNHF's would be 2,368 under regular DKG).
  - The exclusion is by configured use, not by type number.
  - A type that also fills a used role is verified and counted: testnet's MNHF type is `llmq_50_60`, which is its CL type.
- `dash-mnlist` computes L_est from `LlmqConfig` (dash-params) at construction. Nothing is hard-coded, and the table is documentation.
- The resident window (w = 10,000) is sized above L_est + H_keep (§5.2.1), so regular-DKG reads stay resident (§4.4). Deeper reads go to the cold tier and stay correct.

**The rule.**
1. **Candidate.** The newest post-DGW checkpoint c that:
   - has an anchor pack;
   - is at or below the wallet's birth height, when one is set. The birth height is `start_from_height` when set; otherwise it is the wallet's `earliest_required_height`, floored at the network's `hd_wallet_sync_floor`, as `resolve_start_height` does today (`client/lifecycle.rs:100-113`; m3);
   - has `c.time ≤ NodeClock::now() − 2 · (L_est + H_keep) · target_spacing`. On mainnet that is 2 × 1,826 × 150 s ≈ 6.3 days. The factor 2 absorbs block-time variance and leaves room for a few failed rounds.

   Networks without a checkpoint above genesis (devnet, regtest), and clients with no candidate, anchor at genesis. There the floor is 0 and every existing element is covered; coverage targets and lock domains are clipped at genesis and A_q (§5.2.1).
2. **Initial sync: headers only, then coverage.**
   - The anchor is chosen only while `meta.anchor_state = Candidate` (§9.5).
     - In that state the store holds header rows, `meta` and peer data (`peers`, `bans`) only: no MN rows, no subscriber data or locators, and no lock rows.
     - The MN pipeline and the subscribers have not started, and no ChainLock has been verified or applied.
   - Header sync runs from c to the peers' best header.
   - The MN pipeline then fetches, in memory only, the authenticated active sets at both ends of the horizon's **authenticated domain** [max(tip − H_keep, A_q), tip − 8]. That domain is clipped at genesis and at quorum-root activation A_q (§5.2.1).
     - It fetches a full `mnlistdiff` (null base) to the domain's newest block, the tip's selection start (tip − 8).
     - It fetches a full `mnlistdiff` to the domain's oldest block. When that is the same block, there is one request.
     - It runs §5.2 steps 1–4 on them: target accepted, CbTx proof, `merkleRootMNList`, `merkleRootQuorums`.
     - A target below c is itself uncovered; above a genesis anchor, no target is.
     - Nothing is committed, and no commitment is verified yet.

     Coverage needs no `qrinfo`: required(T) follows from each commitment's base, `quorumIndex` and fixed offsets. The pipeline requests `qrinfo` after `Established`, for step 5.
   - **MN sync off** (`ClientConfig::enable_masternodes = false`, `without_masternodes()`, `client/config.rs:188`; FFI `dash_spv_ffi_config_set_masternode_sync_enabled`): coverage is vacuous. The store becomes `Established` at the candidate, and CL and IS verification get `NoQuorum` (m3).
   - **An empty domain passes.** When tip − 8 < A_q, which includes every tip below 8, no selection start in the horizon can carry an authenticated quorum (§5.2.1). In that case:
     - nothing is fetched, coverage passes trivially, and the store becomes `Established`;
     - locks get `NoQuorum` or are unavailable until the first commitment;
     - the first commitment then arrives through the ordinary pipeline, and step 4 checks its history.
   - `MasternodeListEngine::coverage` (§5.3) resolves required(T) for both sets, for every used type:
     - each base by hash in `hdr`;
     - each work block by height on its base's branch (`AnchoredAncestry::ancestor`).

     An element is **covered** when it resolves to an accepted entry at or above c. An element at a negative height is dropped (clipped at genesis, above). An unknown base hash, a context entry or `BelowFloor` means the element lies below c. A base is an ancestor of an accepted diff target, so once headers have synced to the tip, an unknown base can only lie below c.
   - The sets between the two ends need no deeper history: mined order is monotone in base height, so every later set's history lies at or above the oldest set's.
   - **Pass.** One transaction sets `meta.anchor_state = Established`. Only then do the MN pipeline and the subscribers start.
   - **Fail: fall back.**
     - The new candidate is the newest checkpoint with a pack at or below the lowest uncovered height, when that height is known (a work block below an accepted base). For an unknown base it is the checkpoint before c. Ultimately it is genesis.
     - The store holds no MN, lock or subscriber state, so falling back clears the header tables and restarts header sync from the new candidate.
     - A crash anywhere in this state reopens as `Candidate` and resumes the same step.
   - **Cost.**
     - A timely candidate under regular DKG needs no fallback.
     - Each fallback adds one forward header sync from its candidate to the tip, and a base of unknown height steps back one checkpoint per attempt.
     - The worst case is therefore one header sync per checkpoint tried, ending with a full sync from genesis.
3. **Established: the anchor never moves.**
   - Nothing in a running client clears the header store, the MN tables, the lock rows or a locator. Each changes only through its own ordinary writes (§9.5). An applied ChainLock and its floor (§4.2) therefore persist until a newer verified ChainLock supersedes them. *Erratum E1 (§17): or until a SPORK_19 off → on reset clears them.*
   - Re-anchoring an established wallet is a user-initiated resync with a fresh store, outside SB0.
4. **Later coverage misses cost liveness only.**
   - Coverage runs again whenever a set in the horizon gains a commitment, including the first one on a young chain.
     - A newly mined commitment's history lies above what is already covered.
     - In practice only a reorg can miss: on a branch where an index's newer round failed, an older commitment becomes active again.
   - On a miss:
     - the commitments that need the missing history are `Unavailable`. A lock whose Core-selected quorum is among them is reported unavailable and is never checked against another quorum (§5.2 step 6);
     - the client surfaces an **operator-visible diagnostic**: a log line and an additive FFI event (§10.1). Both name the type, the quorum hash, the anchor and, when known, the missing height;
     - nothing else changes: no table is cleared, and the anchor stays.

**Tests.**
- **Candidate (SB4), on captured and synthetic header chains:**
  - tip at exactly the newest checkpoint, and at newest + 100: the time rule picks the previous checkpoint;
  - no API moves an `Established` anchor.
- **Coverage (SB11), with constructed active sets over regtest captures and a mainnet-shaped fixture:**
  - regular DKG (control): covered once the tip is L_est + H_keep above c;
  - **repeated null rounds** of one rotated index, as in Sol r2's witness: uncovered at 2,500,000, covered at 2,450,000;
  - a **very old surviving index** whose base predates the anchor (an unknown hash): uncovered with no known height. The fallback steps back one checkpoint at a time, and to genesis when the index predates every checkpoint;
  - **non-rotated, A successes with null gaps:** the A-th last mined commitment lies deeper than A·C + W + 16 and is required;
  - a commitment active only in the horizon's oldest set (replaced before the tip) is counted;
  - a `Skipped` type's deep history does not count; a used role that shares that type number does;
  - Sol r1's witness (anchor 2,500,000, tip + 100, work blocks 2,499,832 and 2,498,104): uncovered. The previous anchor, 2,450,000, holds both blocks as accepted entries.
  - **Bootstrap from genesis (Sol r4 R4-M1):** §5.2.1's young-chain cases; no negative or pre-activation target is ever requested.
- **End to end (SB15), on regtest with test checkpoints and on devnet:**
  - initial-sync fallback after forced DKG failures. Verification and the subscribers start only once coverage holds, and no MN, lock or subscriber row is written before that;
  - **regtest from genesis with default activation (Sol r4 R4-M1):**
    - the client establishes at tip 20 with no MN request, and at tip 450 with requests to 432 and 442 only;
    - the wallet and header subscribers run before any quorum exists;
    - locks report `NoQuorum` until the first commitment is mined, then verify;
    - the chain grows past A_q + H_keep, and the domain's oldest end leaves A_q and slides with the tip;
  - a skewed clock that admits the newest checkpoint: coverage fails, and initial sync falls back to the previous one;
  - a crash in the `Candidate` state, before and after a fallback: the client reopens as `Candidate` with no MN, lock or subscriber state and completes;
  - a sync that crosses a rotation-cycle boundary while coverage is being checked: no lock is verified before it holds;
  - established: a reorg restores an older commitment whose history is below the anchor. It becomes `Unavailable`, its locks report unavailable and the diagnostic fires. The anchor, the lock rows and every subscriber locator are unchanged.

### 4.4 Resident window and cold tier

The cold tier keeps **every accepted header of every branch** as a full record keyed by hash: id, height, the 80-byte header (which carries `prev`), skip id, branch status and absolute chain work. The active height index is a projection of it (§9.5, DEC-104 M4). No branch is ever dropped. Every stored header passed PoW and the contextual rules, so stale branches grow only as fast as valid work allows.

- **Stable ids (SPV).**
  - `BlockId(u32)`, the width of `dash-consensus`'s `BlockId` (`ctx.rs:14`) that `RuleContext`, `AnchoredAncestry` and `LlmqChain` pass on (m1), is assigned from `meta.next_id` when a header is first accepted (or seeded as anchor-pack context) and persisted in its record.
  - An id that was ever durable is never reused, so it resolves for the store's lifetime, across eviction and restarts. 4.29 × 10⁹ never-reused ids are unreachable. *E7 (§17):* ids issued in a Buffered suffix lost at power loss may be issued again after restart; no durable state names them.
  - The node's ids are arena indices (§4.1.1).
- **Eviction.**
  - With `window = Some(w)`, an entry of *any* branch whose height is below `tip.height − w` leaves the arena.
  - Eviction runs in `set_active_tip`, after the header step's `WriteSet` has committed, never inside `accept` (m2). Every evicted entry's record is therefore already in `hdr`, and eviction writes nothing. A `w` below a batch (SB5 tests one below KGW's reach) cannot evict an entry whose record is not yet committed.
  - A chunk is released when all of its entries are evicted.
- **Resolution.**
  - An id goes to its resident slot, or else to `hdr_id` → `hdr`. A hash goes to the resident index, or else to `hdr`.
  - Cold records are re-materialized into a bounded LRU (default 1,024 entries), never back into the arena. Each is an indexed SQLite read (5–6 µs measured, Appendix A).
  - A stale branch that forks below the floor and is extended later resolves its ancestry through those records. It is never resolved through the active projection, because height is not a branch identity.
  - `ancestor` over cold records follows each record's persisted skip id (Core's `pskip`, `v24 chain.cpp:84-130`), assigned at acceptance like the arena's. A deep ancestor therefore costs O(log n) record reads, as Core's `GetAncestor` does.
- **Reopen.**
  - Read `meta`: the anchor, `next_id` and the tip id. *E5 (§17):* `best_header` is recomputed, as Core's load does, over the stored entries with `IsValid(BLOCK_VALID_TREE)`; `meta`'s best-header id is a diagnostic only.
  - Load every record with height ≥ `tip.height − w`, across all branches, through `hdr_h`; then load the resident part of `active`.
  - This is O(window + stale entries in the window), with the same ids as before the restart.
- **Storage errors.**
  - A failed cold read trips the latch (`AbortSink::read_failed`, DEC-78), and the operation returns `ChainError::Storage` (`AncestryError::Storage`). The SPV client stops (P6).
  - A missing record for an issued id, or an `active` row naming an unknown id, is corruption. The latch trips and the client stops. The next start discards the store and syncs a fresh one from the same anchor (§10.1, behaviour row). This is corruption recovery before anything runs, not a transition inside a running store (§4.3.1 step 3).
- **Bounded residency and retained snapshots (DEC-105 F3).**
  - The SPV `ActiveChain` is a chunked copy-on-write vector of the last w heights (1,024 ids per chunk).
  - **Live queries** read heights below it from `HeaderStore::active_at`, the live projection. These are the header task, peer locators and reads of the current chain.
  - **A `ChainReadSnapshot`** holds `Arc`s to its `ActiveChain` chunks and its own tip id, never arena entries.
    - Its `at_height(h)` answers resident heights from those chunks.
    - Below them it resolves `ancestor(lowest, h)` through `AnchoredAncestry` over the immutable `hdr` records, where `lowest` is its own lowest resident entry and lies on its tip's branch.
    - It never reads `active_at`, which a later reorg rewrites (§4.6).

    `{tip, tip_seq, at_height}` therefore describe one chain state at every height, as CH2's coherent `ChainSnapshot` requires (`CH2-coins-flush.md:1313-1314`, F8 `:1342`).
  - **Cost.** A reader that walks a range below its chunks resolves the top once and follows `prev` downward in batches of 1,024, so n heights cost about n record reads. A point query costs O(log n) through skip ids.
  - A lagging reader pins at most ⌈w/1,024⌉ + 1 chunks (≈ 80 KB at w = 10,000). It resolves evicted ids through the cold tier and never delays eviction.
  - The resident hash index covers resident entries only.
- **Sizing.**
  - `w` defaults to 10,000, about 1.2 MB of entries (estimate; pending measurement in SB17).
  - That is above L_est + H_keep (1,826 on mainnet, §4.3.1, §5.2.1) and above KGW's 4,033-entry read envelope (§4.1.1), so regular-DKG reads, the recent-lock horizon and a genesis-origin sync's header contexts stay resident.
  - Construction rejects a w below L_est + H_keep as a configuration error. This is a sizing check; correctness does not depend on it.
  - Deeper reads go to the cold tier and stay correct. They come from failed DKG rounds, reorgs deeper than w (with or without a ChainLock) and stale pre-DGW branches.
- **No whole-chain index.** The SPV hash index is the `hdr` table, so no in-memory hash index for the whole chain exists on mobile. Fable's leg kept #1095's `HeaderHashIndex` (≈ 10–13 MiB) as the cold index; SQLite makes it unnecessary. Startup is O(window), not O(chain).
- **The node** uses `window = None`: every entry is resident (Core keeps every `CBlockIndex`), and the cold branch is a predictable never-taken `if`.
- **Tests (SB5 acceptance).** Windowed and unwindowed chains give equal answers (property test) for `lookup`, `ancestor`, `lca`, `status`, best header and active tip, over:
  - several stale branches, including one that forks below the floor and is extended in batches until it wins by work (Sol's A9,000/B9,000 case);
  - a restart in the middle of that extension (same ids after reopen);
  - lagging readers holding snapshots across eviction;
  - **a lagging reader holding a snapshot across a reorg deeper than w** that forks below its lowest resident height. Each resident and cold `at_height` answer is checked to be an ancestor of the snapshot's tip, while live queries answer the new branch (DEC-105 F3);
  - a reorg deeper than 10,000 blocks with no ChainLock (regtest PoW);
  - an injected cold-read error (latch tripped, `ChainError::Storage`, nothing published).

### 4.5 Download policy belongs to the processor

| | Node (P2a §3.4, P3) | SPV (dash-spv) |
|---|---|---|
| Headers | Core's per-peer `getheaders` with locator, throttle, unconnecting counter, `headers2` | checkpoint-segmented parallel download (`HeadersPipeline`, 8 segments). Segments are buffered and **accepted in order** through `accept`; the buffer is bounded at 8 × segment size. A segment whose end hash does not match its sync anchor is discarded and refetched from another peer |
| Then | block download → V → `set_active_tip` | `set_active_tip(fork_target)` after the header step commits (§4.1); cfheaders, cfilters and matched blocks follow as feed subscribers (§6) |

Fable's Q3 (keep segmented download on mobile) is therefore answered yes. In-order acceptance means segmenting changes throughput, not rules.

### 4.6 Reorgs without restart

- `accept` and `apply_chainlock` return `TipChange { disconnected, connected }`. In SPV, `set_active_tip` publishes one `Disconnected` per header leaving and one `Connected` per header joining (F1 order), after the header step's transaction has committed (§9.5).
- The SPV subscribers rewind by the disconnected list against their own durable locators. The wallet receives #1104's `truncate_above(fork_height)` once per reorg, and FFI `on_chain_truncated` fires from there once #1104 lands. #1104 is not on integration; SB14 needs it merged or carries it (m7).
- The wallet keeps its own progress, which is authoritative. At open, a reorg the wallet missed while offline is detected from its synced block hash and rewound with one `truncate_above` (§9.5, O4). *Erratum E3 (§17): the rewind point comes from the wallet's stored block locator.*
- `handle_fork`'s stop–truncate–restart (`lifecycle.rs:257`) is removed (SB14).
- A reorg rewrites the `active` rows for the changed heights in the header step's transaction. The records of the losing branch stay in `hdr` with their status (§4.4). A retained snapshot is unaffected: it resolves every height from its own tip (§4.4).

### 4.7 Persistence, P10, and `-spv` → full

- **Node.** V calls `take_dirty()`, and CH2's B1 writes the records into the chainstate engine **in the same batch as `meta`, the marker and evo** (P10). Recovery loads `HeaderChain::load` from those records before V starts (CH2 §4.3 steps 4–6). The node never opens an SPV header store.
- **SPV.** The header step's write set commits in one SQLite transaction before the tip change is published. It holds the `take_dirty()` rows, the `active` projection change and, when the step applied a ChainLock, the CL rows. Each subscriber, `dash-mnlist` included, commits its own data with its locator (§9.5).
- **`-spv` → full: no import** (DEC-104 M5; answers F's §4.1 reason 5).
  - Starting rdashd in full mode over a datadir that was used with `-spv` is an ordinary first start: a Core-style header sync **from genesis over P2P** (Core's `getheaders`, P2a §3.4), then block download through P3. Header sync is cheap.
  - Full mode **never opens** `<datadir>/spv/`, so P10 holds trivially and the stores never mix. An anchored SPV store has no genesis prefix, so it could not seed a full index anyway.
  - The SPV store stays on disk and the operator deletes it. rdashd logs its path once at startup. Checking that the directory exists does not open the store.
  - Test (SB18): a full start over an `-spv` datadir syncs from genesis, and a file-access audit (`strace -e openat` in the test harness) shows no open under `spv/`.

### 4.8 Filters

`dash-filters` (P) is the GCS codec, the Dash element set (special-transaction elements) and the filter-header chain check.
- **SPV:** each cfheader must extend the previous one and agree with the header chain at its height; each cfilter must hash to its cfheader before it is matched.
- **Node:** IX1 builds filters with it, and P6b serves them. One implementation produces and checks.

---

## 5. One MN-list, quorum and lock pipeline

### 5.1 The shared vocabulary is the SML

- Every SML field is a function of a DML entry. The reverse is impossible: `mnlistdiff` carries no collateral, owner key, payout or PoSe state [O].
- LQ1 works over both through `MnListView`:
  - `impl for MnList` in `dash-dml` (exact);
  - `impl for SmlList` in `dash-evo::sml`. Here, per LQ1 §2.2.4 (LQ1:247), `collateral()` returns the null outpoint and `try_add` checks only proTxHash, service and operator key.

  Fable's "the SML has every field" was wrong on collateral and owner key: `try_add` can under-reject on the SML view.
- **What SPV trusts (DEC-104 M8): commitment-verified quorum public keys, and nothing else.**
  - A quorum is `Verified` when two things hold:
    - its commitment is in a quorum set whose root equals an accepted block's CbTx `merkleRootQuorums` (§5.2 step 4);
    - the commitment verifies as Core's `CFinalCommitment::Verify` verifies it with signature checks:
      - the structural checks;
      - `BuildCommitmentHash` over type, quorum hash, valid-member bits, quorum public key and vvec hash (`v24 llmq/commitment.cpp:312-321`);
      - `quorumSig` against the committed quorum public key (`:96-107`);
      - `membersSig` against the aggregate of the operator keys at the **signer** positions of SPV's reconstructed member list (`:76-93`).
  - ChainLock, ISLock and recovered-signature verification, and the FFI quorum-key lookup, use only `Verified` keys.
- **What SPV does not claim: exact membership.**
  - `membersSig` covers only the signers, and the commitment hash does not commit member identities. A reconstruction that differs from Core's only at non-signer positions therefore verifies identically: Sol's roster `[A,B,C]` with signer bits `[1,1,0]` reconstructed as `[A,B,D]`.
  - `membersSig` is a secure aggregate over the **sorted** signer keys (`v24 dashbls/src/schemes.cpp:231-253`), so it binds the multiset of signing keys, not their positions (DEC-105 f1).
    - A reconstruction whose **signing-key multiset** differs from Core's fails `membersSig` under the BLS security assumptions, and the quorum stays `Unverified`.
    - One that only permutes signer positions, `[A,B,C]` with bits `[1,1,0]` reconstructed as `[B,A,C]`, gives identical inputs and the same verdict. SPV infers no signer identity or order from a success.
  - An `Unverified` quorum costs liveness and never safety, because selection runs before the status check (§5.2 step 6, DEC-105 F4). A lock whose Core-selected quorum is `Unverified` is reported unavailable, and no other quorum is ever substituted for it.
  - SPV's reconstructed member list is therefore an internal input to the `membersSig` check. `dash-mnlist` exposes no member-list API, and no SPV or FFI surface reports members.
- **Exact membership is node-only.** That covers DIP24 member lists, `quorum info` members and `llmq_S`, computed by LQ1 over `impl MnListView for MnList` (`dash-dml`). mnxcheck (§5.4) compares the shadow engine's reconstructed members with the node's exact members, which is how the SML adapter's behaviour on the reachable domain is observed.
- **EV0's types become generic, with store-independent errors** (m3):
  - `QuorumMembers<E> = Arc<[E]>` for `E: MnEntryView`;
  - `LlmqChain`, `LlmqSource` and `SnapshotRows` each get an associated `type Error`. `MnListView` stays infallible, because both lists are materialized values and every fallible read happens in the view that produces them (DEC-109 (3));
  - `MemberError<E> = Missing(BlockId) | View(E) | UnknownLlmqType(u8)`, keeping EV0's variant for Core's throw;
  - the mined-commitment reader (`MinedCommitmentReader`) moves from `store` to shared `dash-evo::llmq`, with an associated error. Only holders of complete ancestry implement it; SPV builds its scan from the authenticated set (§5.2 step 6);
  - nothing in `dash-evo` names `StoreError`;
  - `ProTxHash` moves to `dash-evo`.

  The products map the errors differently:
  - **SPV:** `Missing` becomes `Unavailable`, so the list is requested over P2P, and the quorum stays `Unverified` until it arrives.
  - **Node:** `Missing` for an established connected block is a fatal invariant failure under LQ1's D4 contract (`LQ1-quorum-members.md:267-281`, `:482-492`). The latch trips through `AbortSink`, and `View(StoreError)` likewise trips the latch. Neither path turns into a network request.
  - The EV addendum (fa09d8d0, DEC-109) covers every signature and adds a missing-connected-block-list test in both products.

### 5.2 The SPV trust chain (kernel properties of `dash-mnlist`)

1. The diff's target block is an accepted header in `HeaderChain`.
2. The CbTx partial-merkle proof reproduces that header's merkle root, with the coinbase at index 0.
3. Base list plus diff gives an `SmlList` whose root (proTxHash memcmp order, Core's per-entry `CalcHash`) equals the CbTx `merkleRootMNList`.
4. The quorum set after the diff hashes to `merkleRootQuorums`.
5. For each new commitment **of a type SPV uses** (the CL, IS and Platform types from `LlmqConfig`), Core's `CFinalCommitment::Verify` with signature checks runs, using an explicit `BlsCtx` (legacy scheme before V19):
   - the members at the signer positions come from LQ1 over the SML view, with rotation from qrinfo's snapshots through `SnapshotRows`;
   - success makes the quorum `Verified`, and failure leaves it `Unverified`.

   Commitments of other types (mainnet MNHF `llmq_400_85`) are `Skipped`, by configured use (§4.3.1): they are authenticated by step 4 and never verified or selected.
6. **Selection first, then status (DEC-105 F4).** For a CL, an ISLock or a recovered signature:
   1. **Scan.** `dash-mnlist` builds Core's scan at the selection start block (sign height − `SIGN_HEIGHT_OFFSET`) from the **authenticated** active set at that block (steps 1–4, §4.3.1), held or fetched within the recent-lock horizon (§5.2.1). Every commitment of the type is in it, whatever its local status (`Verified`, `Unverified` or `Unavailable`), in Core's order and count (LQ1 §3.8). Local status never removes an entry: Core's scan does not drop entries, and it returns blank when it cannot materialize one (`v24 llmq/quorumsman.cpp:253-268`).
   2. **Select.** LQ1's `select_from_scan` (LQ1 §3.9, quirk 30) picks the Core-defined commitment with the selection hash and height rules unchanged:
      - non-rotated, the lowest score over the whole scan (`v24 llmq/quorumsman.cpp:757-773`);
      - rotated, the first entry at the signer's index (`:733-756`).
   3. **Require `Verified`.** Only **that exact commitment** is then checked. If it is `Verified`, the signature is verified against its key. Otherwise the lock is reported **unavailable** (`LockReject::QuorumUnavailable`), and the client waits for the missing input or fails.

   `dash-mnlist` never re-selects over a filtered set and never substitutes another `Verified` commitment, including an older one at the same rotated index. A lock whose selection start block is below the horizon is unavailable without a fetch (§5.2.1).
7. A CL is verified, then `HeaderChain::apply_chainlock` runs. A CL for an unknown header waits in `pending_cl`.

After step 4 the quorum set is as trustworthy as the header chain: PoW, chain work and ChainLocks. Step 5 adds Core's signature checks for the keys SPV uses. SPV claims nothing about member identities beyond the signing-key multiset that `membersSig` accepted (§5.1).
- With step 6, a wrong reconstruction can cost liveness, but it never authorizes a quorum Core would not select.
- Steps 1 and 5 read only accepted entries at or above the anchor. A required base or work block below it makes the commitments that need it `Unavailable` and raises §4.3.1 step 4's diagnostic. It is never fetched below the anchor, and the anchor does not move.

#### 5.2.1 Recent-lock horizon H_keep (DEC-107)

Core selects a lock's quorum at the lock's **own sign height**, not at the verifier's tip:
- a ChainLock for height H selects at H − 8 (`v24 chainlock/clsig.cpp:253-269`, `llmq/quorumsman.cpp:711-721`, `:849-854`);
- an ISLock signs at the tip, unless the tip has passed its cycle's end. Then it signs at `cycleHeight + dkgInterval − 1` (`v24 instantsend/net_instantsend.cpp:136-152`; v23 `:106-122`).

The active set can change between a lock's signing and its delivery.
- **Sol r3's case:**
  - a mainnet CL-type commitment for cycle base 2,500,128 is mined at 2,500,148 and replaces the oldest of the four active CL quorums;
  - a valid ChainLock for 2,500,155 selects from the set at 2,500,147, from before the mine, and its scoring winner can be the replaced quorum;
  - it arrives when the tip is 2,500,156, whose own selection start is 2,500,148.
- A client that held only its newest set would report this lock, one block late, unavailable. `dash-mnlist` therefore keeps the authenticated set for each selection start block in a fixed horizon. The horizon is measured from local chain progress, never from a height a peer supplies.

**Definition.**

H_keep = C_max + `SIGN_HEIGHT_OFFSET` + M, measured from the header tip to the selection start block:
- **C_max** is the largest `dkgInterval` among the CL and IS types SPV uses, from `LlmqConfig`. An ISLock of the previous cycle that is delivered anywhere in the current cycle signs up to C_max below the tip.
- **`SIGN_HEIGHT_OFFSET`** = 8 (`v24 llmq/quorumsman.h:210`): the selection start lies 8 below the sign height.
- **M** = ⌈3,600 s / `target_spacing`⌉ is a relay-delay margin of one hour of blocks. A ChainLock delayed by d blocks needs the set d + 8 below the tip.

A lock is **in the horizon** when its selection start block s satisfies tip − H_keep ≤ s, with s on the active header chain.

**Domain on a short or young chain (Sol r4 R4-M1).** The horizon is clipped at genesis, and at the height where a quorum set can first be authenticated.
- **A_q** is the first height whose CbTx carries `merkleRootQuorums`: max(`DIP0003Height`, `DIP0008Height`).
  - Both are buried, height-based deployments, read from dash-params with its overrides (`v24 deploymentstatus.h:34-37`; `node/miner.cpp:262-295`; `evo/cbtx.cpp:35-37`).
  - A_q is 2 on devnet and 432 on default regtest (`chainparams.cpp:576-579`, `:818-822`; overridable through `-dip3params` and `-testactivationheight=dip0008@h`).
- **Classified without a fetch, before the horizon check:**
  - **s < 0** (sign height below 8): Core has no start block (`llmq/quorumsman.cpp:711-721`, `:727-729`). The lock gets `LockReject::NoQuorum`, Core's `VerifyRecSigStatus::NoQuorum` (`:784-793`).
  - **s < `DIP0003Height`**: no commitment can be mined before DIP3 (`llmq/blockprocessor.cpp:218-221`, `:599-601`). The active set is canonically empty, and the lock gets `NoQuorum`.
  - **`DIP0003Height` ≤ s < A_q**: commitments can exist, but no CbTx root authenticates them. The lock is unavailable (`QuorumUnavailable`).
    - On mainnet and testnet this range is history (1,028,160–1,088,640 and 7,000–78,800), far below any current tip's horizon.
    - On devnet and on default regtest it is empty.
- **The authenticated domain** is [max(tip − H_keep, A_q), tip − 8]. Retention, fetches and coverage use only this range.
  - It is empty while tip − 8 < A_q, which includes every tip below 8. Coverage then passes trivially (§4.3.1 step 2).
  - It grows with the chain. Its depth is min(H_keep, tip − A_q), and it reaches H_keep once tip ≥ A_q + H_keep. From then on it slides with the tip.
  - An authenticated empty set (no commitment mined yet) is an ordinary set. Coverage passes, and Core's selection over an empty scan finds nothing (`llmq/quorumsman.cpp:734-737`, `:758-761`), so the lock gets `NoQuorum`.
- **Early rotated cycles.** Core serves a `qrinfo` only when the cycle blocks three cycles back exist and have snapshots (`ConstructCycle`, `v24 llmq/snapshot.cpp:35-65`, used at `:175-186`).
  - Until then, an early rotated commitment cannot complete step 5, and its locks are unavailable. This costs liveness only.
  - It verifies once a peer can serve the `qrinfo`.

| Network | CL type (C) | IS type (C) | C_max | M | **H_keep** |
|---|---|---|---|---|---|
| mainnet | `llmq_400_60` (288) | `llmq_60_75` (288) | 288 | 24 | **320** |
| testnet | `llmq_50_60` (24) | `llmq_60_75` (288) | 288 | 24 | **320** |
| devnet | `llmq_devnet` (24) | `llmq_devnet_dip0024` (48) | 48 | 24 | **80** |
| regtest | `llmq_test` (24) | `llmq_test_dip0024` (24) | 24 | 24 | **56** |

`dash-mnlist` computes H_keep from `LlmqConfig` and the network's spacing. The table is documentation.

**Rule.**
1. **Retain.** Every set the engine authenticates stays in `recent` (§5.3), keyed by its target `BlockId`, until the target falls below tip − H_keep.
   - The engine authenticates through §5.2 steps 1–4, for each diff or `qrinfo` target as headers advance.
   - dash-spv's MN sync already diffs to every new tip (`dash-spv/src/sync/masternodes/sync_manager.rs`), so in steady state each selection start block in the horizon is a held target.
   - Entries reference `quorums` and copy no commitments.
2. **Fetch.** A lock whose s lies in the authenticated domain and is not a held target waits in `pending_cl` or the IS cache while the engine fetches the set:
   - it sends one `getmnlistd` to s, from the nearest held list below s (a null base when none is held), then runs steps 1–5 and adds the result to `recent`;
   - a rotated commitment that the engine has not yet seen also needs the `qrinfo` for its cycle;
   - fetches are deduplicated by s, so there is at most one per block in the horizon, however many locks arrive;
   - each fetch has a per-request timeout and a bounded peer count, both configured in dash-spv. If it fails, the lock is **unavailable** (`LockReject::QuorumUnavailable`).
3. **Select once, then require `Verified`.** §5.2 step 6 runs unchanged on the set at s.
4. **Outside the horizon.** A lock with s < tip − H_keep is **unavailable** (`LockReject::OutsideHorizon`):
   - no fetch, no resync, no re-anchor;
   - the lock is never checked against a quorum from another set.
5. **Coverage.** The authenticated domain's oldest set is included in §4.3.1's coverage check: at initial sync, and again when a set in the horizon gains a commitment.
   - Mined order is monotone in base height, so every set between that one and the tip's needs no deeper history.
   - The candidate rule (§4.3.1 step 1) uses L_est + H_keep for the same reason.
6. **Unaffected.**
   - A lookup by quorum hash is not a selection, and the horizon does not apply to it. This covers Platform's quorum lookup (§5.5) and a non-rotated IS type, whose `cycleHash` names its quorum directly (`net_instantsend.cpp:149-152`).
   - What such a lookup can find is set by §5.3's retention. For the served Platform type, anything older is fetched on demand (DEC-112 O3, DEC-116).

**Tests.**
- **SB8, on synthetic scans:**
  - Sol r3's case: the set at 2,500,147 against the set at 2,500,148, with a constructed scoring winner. The selection on each set matches Core's.
- **SB11, on regtest captures and a mainnet-shaped fixture:**
  - Sol r3's case end to end:
    - the ChainLock for 2,500,155 is delivered at tip 2,500,156;
    - it verifies against the replaced quorum from the retained set;
    - with that set dropped, it verifies after one fetch.
  - A delayed ChainLock immediately before and immediately after a commitment mine.
  - An ISLock signed in the last block of a cycle and delivered once the tip has passed `cycleHeight + C`: it signs at `cycleHeight + C − 1` and verifies from the held set.
  - A fresh start on the new side of each boundary: the earlier set is fetched once and verified.
  - Outside the horizon (s = tip − H_keep − 1): unavailable, with no request sent.
  - A fetch that fails on every allowed peer: unavailable.
  - A thousand locks with the same s: one fetch.
- **SB11, bootstrap from genesis (Sol r4 R4-M1).** Run with default regtest parameters (A_q 432, H_keep 56) and with devnet parameters (A_q 2, H_keep 80).
  - Tips 0, 7 and 8: no request, `Established`, and a ChainLock gets `NoQuorum`.
  - Tip 20 (the regtest control): an empty domain, `Established`, no request.
  - Tips A_q + 7 and A_q + 8: an empty domain, then the single block A_q with one request.
  - Tip 450 (the regtest control): requests to 432 and 442 only, never to 394. Both targets carry CbTx roots.
  - Tips A_q + H_keep − 1, A_q + H_keep and A_q + H_keep + 1: the oldest end stays at A_q, meets tip − H_keep, then slides.
  - The first commitment mined: before it, locks get `NoQuorum`. It is then authenticated and verified through steps 1–5 at the next tip, coverage re-runs and passes, and the locks that select it verify.
  - Regtest with `-testactivationheight=dip0008@h` above DIP3: a selection start in [`DIP0003Height`, A_q) is unavailable, with no request.
  - Devnet's first rotated cycles: required elements at negative heights are dropped and never requested. A commitment whose `qrinfo` cannot be served yet leaves its locks unavailable, and verifies once the `qrinfo` can be served.

### 5.3 `dash-mnlist`

```rust
pub struct MasternodeListEngine {      // name kept for maintainer continuity; crate-private use in dash-spv today
    lists: SmlLists,                   // sparse by BlockId: diff targets and qrinfo heights; retained per "Retention" below (O3)
    quorums: QuorumSet,                // (llmq_type, quorum_hash) -> { commitment, mined height and order, replaced-at height,
                                       //   status: Verified|Unverified|Unavailable|Skipped }; no member lists exposed; retained below
    recent: RecentSets,                // §5.2.1: authenticated active set per held target BlockId within tip − H_keep; references into `quorums`
    rotation: SnapshotRows,            // from qrinfo
    best_cl: Option<VerifiedChainLock>, pending_cl: PendingLocks, islocks: IsLockCache,
    mode: Mode,                        // Client | Shadow
}
impl MasternodeListEngine {
    pub fn apply_diff(&mut self, d: &MnListDiff, at: BlockId, chain: &dyn AnchoredAncestry, cfg: &LlmqConfig) -> Result<MnChange, MnReject>;
    pub fn apply_qrinfo(&mut self, q: &QrInfo, chain: &dyn AnchoredAncestry, cfg: &LlmqConfig) -> Result<MnChange, MnReject>;
    pub fn coverage(&self, chain: &dyn AnchoredAncestry, cfg: &LlmqConfig) -> Result<Coverage, MnReject>; // §4.3.1: Covered | Below { lowest: Option<u32> }; the authenticated domain's two ends, and an empty domain is Covered
    pub fn set_at(&self, s: BlockId, chain: &dyn AnchoredAncestry, cfg: &LlmqConfig) -> SetAt;    // §5.2.1: Held | Fetch { base, target } | NoQuorum | NoRoot | OutsideHorizon
    pub fn verify_chainlock(&self, cl: &ChainLockSig, cfg: &LlmqConfig) -> Result<VerifiedChainLock, LockReject>; // §5.2 step 6: select, then require Verified
    pub fn verify_islock(&self, is: &IsLock, cfg: &LlmqConfig) -> Result<VerifiedIsLock, LockReject>;
    pub fn lookup_quorum(&self, ty: LlmqType, hash: &QuorumHash, h: u32, cfg: &LlmqConfig) -> Lookup; // Platform: Found | Fetch | Unavailable | NotFound (DEC-444, DEC-447)
    pub fn take_dirty(&mut self) -> MnWrites;          // its subscriber write set, with its locator (§9.5)
    pub fn snapshot(&self) -> Arc<MnSnapshot>;
}
```

`LlmqConfig` carries LLMQ parameters from `dash-params` per client: an owned `Arc<ChainParams>` plus the used types. This retires the devnet `OnceLock`s from dash-spv's path (§1.1). For each LQ1 call, `dash-mnlist` builds LQ1's per-call `LlmqCtx` from it (DEC-109 (5), EV addendum §2.2).

**Retention (DEC-112 O3).** Today's engine prunes (`apply_and_prune`, `sml_engine.rs:701-710`), and Platform's lookups fall back to the stored message log (`client/queries.rs:73-105`), which §9.5 drops. Both state sets are bounded here:
- **Lists.** A list stays, in memory and in `mn_lists`, while one of these needs it:
  - a `recent` set (§5.2.1, within H_keep);
  - an `Unverified` commitment, or a pending fetch;
  - `latest_masternode_list()`, which returns the tip list.

  Every other list is dropped.
- **Quorum rows.** Each commitment of a used type has a row {key, status, mined height, replaced-at height} in `mn_quorums`.
  - The row stays while the commitment is active at the tip, or was replaced at a height ≥ tip − R, where R = max(H_keep, 2·W_P) (table below).
  - In memory the engine keeps the commitments active in the authenticated domain, plus an LRU of rows read for lookups.
  - On mainnet that is about 60 active commitments and about 50 replaced within R.
- **Pruning** is part of `dash-mnlist`'s own `WriteSet`, committed with its locator. It deletes rows that fall outside these bounds and never clears a table (§4.3.1 step 3).

**Platform's lookup.** `get_quorum_at_height(h, type, hash)` serves `ffi_dash_spv_get_quorum_public_key` (`dash-spv-ffi/src/platform_integration.rs:42-91`) and `rs-platform-wallet`'s `get_quorum_public_key` (`spv/runtime.rs:340-359`). It answers through `lookup_quorum`:
- **Found** when the commitment is `Verified`, mined at or before h, and not replaced at or before h − W_P.
  - Today's lookup has no replacement bound: any held list at or before h that contains the quorum answers (`queries.rs:73-105`). The bound narrows that.
  - The definition, W_P included, is proposed here and agreed with Platform's owner through pasta (Q8). The consumer note carries it (§10.1).
  - **One predicate for every source** (R6-M3). A row, an LRU entry and a fetch all answer this predicate for (type, hash, h). Which one answers changes only the cost.
- **Older than the rows: fetch on demand, for the served types only** (DEC-115 R6-M3, DEC-116). This applies when no row decides the lookup, because the commitment was replaced before tip − R and pruned, or the engine never held it.
  - **Served types**, which are each network's Platform type (the lifetime table below).
    - The engine fetches authenticated null-base diffs to **two targets, h − W_P and h**, through §5.2.1's fetch path, with its deduplication, timeouts and bounded peer count. Each is authenticated by §5.2 steps 1–4.
    - The commitment found is verified with step 5's inputs: the `getmnlistd` to its work block for a non-rotated type, or its cycle's `qrinfo` for a rotated one.
  - **Why two targets.** A null-base diff lists the commitments **active at its target**, not every commitment mined before it (`v24 evo/smldiff.cpp:31-64`; `llmq/blockprocessor.cpp:818-832`).
    - The commitment is found when it is `Verified` and active at either target. Under the lifetime invariant below, that is exactly the predicate.
    - Active at h − W_P means mined at or before h − W_P and not replaced by then.
    - A commitment mined in (h − W_P, h] cannot be replaced by h, so it is active at h.
    - Anything active at h was not replaced at or before h − W_P.
  - **Sol r6's witness.** A commitment replaced at r is found at h = r + W_P − 1, through the diff to r − 1. It is not found at h = r + W_P, where neither target holds it.
  - **A target below the anchor** cannot be authenticated. Then only the diff to h is fetched: a commitment it shows is found, and one it does not show is `Unavailable`, as is h below the anchor (DEC-444, SB11 D50 (f)).
  - **Caching.** The fetched answer goes into the LRU keyed by (type, hash, h), not into `recent`. A failed or unauthenticated fetch returns `Unavailable` (DEC-444); `NotFound` means only that the authenticated list does not hold the quorum.
  - **Other types** return not-found after pruning, with no request. This is an allowed difference from today's lookup, and no consumer depends on it (DEC-116).
- **h above the header tip** returns `Unavailable` until headers reach h (DEC-444).
- **Precedence** is `Found` > `Fetch` > `Unavailable` > `NotFound`. SB11's D50 lists every `Unavailable` case (DEC-447).
- The horizon does not apply: a lookup by hash is not a selection (§5.2.1 rule 6).

**The Platform-lookup window W_P** bounds how far a proof's `core_chain_locked_height` lags the tip when it reaches the client.
- rs-sdk rejects a response whose signed Platform block time is more than T_fresh = 31 min old (`DEFAULT_METADATA_TIME_TOLERANCE_MS`, Platform `packages/rs-sdk/src/sdk.rs:84`, checked at `:375-377`; set by `SdkBuilder::new`).
- That block's core height is the best ChainLock its proposer knew (`packages/rs-drive-abci/src/abci/handler/prepare_proposal.rs:84-95`, `:126-128`).
- Platform proposes a block at least every T_empty = 3 min (Tenderdash `createEmptyBlocksInterval`, `packages/dashmate/configs/defaults/getBaseConfigFactory.js:422`; no network config overrides it).
- So W_P = ⌈(T_fresh + T_empty) / spacing⌉ + M, where M is H_keep's one-hour margin (§5.2.1), here for ChainLock latency. `dash-mnlist` computes W_P from the network's spacing (devnet's is configurable); the table is documentation.

| Network | Spacing | ⌈34 min / spacing⌉ | M | **W_P** | H_keep | **R** = max(H_keep, 2·W_P) |
|---|---|---|---|---|---|---|
| mainnet | 150 s (`v24 chainparams.cpp:200`) | 14 | 24 | **38** | 320 | **320** |
| testnet | 150 s (`:409`) | 14 | 24 | **38** | 320 | **320** |
| devnet | 150 s by default (`:591`; `-powtargetspacing` override `:762-764`) | 14 | 24 | **38** | 80 | **80** |
| regtest (Platform's local network) | 150 s (`:834`) | 14 | 24 | **38** | 56 | **76** |

- An SDK that disables the freshness check, or a Platform halt, can produce older heights. Those lookups take the on-demand fetch.

**The lifetime invariant (DEC-115, DEC-116).** The two targets work only if no served commitment can be mined and replaced between them. For each network's served type, this must hold:

  interval × cycles active − mining-window width > W_P.

- **Cycles active** is `signingActiveQuorumCount` for a non-rotated type. For a rotated type it is 1, because Core keeps the last mined commitment of each quorum index (`v24 llmq/blockprocessor.cpp:818-832`); that count is the number of indexes.
- **The window width** is subtracted because a successor can be mined earlier in its window than its predecessor was (`:311-323`).
- **A params test (SB11) asserts the invariant** for every network's served type.
- **Config load refuses a configuration only when its served type breaks it.** Examples are a devnet with a short `-powtargetspacing`, or a `-llmqplatform` type that lives too briefly (`chainparams.cpp:762-764`, `:1308-1310`). Stock regtest and devnet configurations stay valid.
- **The verdict column covers the served types only.** The other rows, the rotated ones included, are kept for information.

W_P = 38 on every network (150 s spacing). Parameters: `v24 llmq/params.h:138-511`; types and roles per network: `chainparams.cpp:281-289`, `:483-492`, `:666-677`, `:949-957`.

| Network | Type (role) | Rotated | Interval × cycles active | Window width | Minimum lifetime | Verdict (served types) |
|---|---|---|---|---|---|---|
| mainnet | **100_67 (Platform)** | no | 24 × 24 = 576 | 8 | **568** | **served: holds** |
| mainnet | 50_60 | no | 24 × 24 = 576 | 8 | 568 | not served |
| mainnet | 60_75 (IS) | yes | 288 × 1 = 288 | 8 | 280 | not served |
| mainnet | 400_60 (CL) | no | 288 × 4 = 1,152 | 8 | 1,144 | not served |
| mainnet | 400_85 (MNHF) | no | 576 × 4 = 2,304 | 28 | 2,276 | not served |
| testnet | **25_67 (Platform)** | no | 24 × 24 = 576 | 8 | **568** | **served: holds** |
| testnet | 50_60 (CL, MNHF), 60_75 (IS), 400_60, 400_85, 100_67 | as on mainnet | as on mainnet | as on mainnet | as on mainnet | not served |
| devnet | **devnet_platform (Platform)** | no | 24 × 4 = 96 | 8 | **88** | **served: holds** |
| devnet | devnet (CL, MNHF) | no | 24 × 4 = 96 | 8 | 88 | not served |
| devnet | devnet_dip0024 (IS) | yes | 48 × 1 = 48 | 8 | 40 | not served |
| devnet | 50_60, 60_75, 400_60, 400_85, 100_67 | as on mainnet | as on mainnet | as on mainnet | as on mainnet | not served |
| regtest | **test_platform (Platform)** | no | 24 × 2 = 48 | 8 | **40** | **served: holds** (margin 2) |
| regtest | test (CL, MNHF), test_instantsend, test_v17 | no | 24 × 2 = 48 | 8 | 40 | not served |
| regtest | test_dip0024 (IS; rotation from height 1, `:825-826`) | yes | 24 × 1 = 24 | 8 | 16 | not served (below W_P) |

**Tests.** SB11 covers retention, each lookup case, the two-target fetch after pruning and a restart, and the params test. SB15 runs the lookup end to end after a restart, with and without network. SB17 measures the RSS that retention bounds (§13.1).

### 5.4 The node: authoritative state in node crates, shared functions, shadow check

- **Authoritative state:** the node's MN state is EV2's DML in `dash-dml` (CH7a), LQ1's caches and CH7b's quorum state, and `dashd-locks`' CL/IS state. Those crates call the shared functions directly:
  - `dash-evo::sml` roots in CH7a/EV4's CbTx checks;
  - `dash-evo::llmq` over `impl MnListView for MnList`;
  - `dash-evo::locks` in `dashd-locks`.

  One implementation verifies in both products.
- **Rejected [O]:** a node `MnState` fed by a projection input. It would duplicate EV2/CH7b's caches (memory and a second source of truth) and put a shared state machine on V's path.
- **Rejected [F]:** a `dml` binding inside `dash-mnlist`. It would pull `dash-dml` into a shared crate.
- **mnxcheck [O].** The node runs a `dash-mnlist` engine in `Mode::Shadow` on its own task, never on V:
  - it requests `getmnlistd` for the tip every 16 blocks, and `getqrinfo` on each new rotation cycle, from its peers through `dashd-p2p`;
  - it applies them with steps 1–5;
  - it compares the SML root, the quorum set and the members with its own projection (`MnList::project_sml`, EV4-serve) at the same block.

  A mismatch logs `mnxcheck` and counts a metric. It never feeds consensus. On by default on regtest, devnet, testnet, CI and L2 runs; opt-in on mainnet (`-mnxcheck`). This is the executable proof that the two producers agree, and it replaces LQ1 §6.4's inverted fixture test.

### 5.5 Compatibility

- `dashcore::sml` and `dashcore::network::NetworkMessage` stay public; Platform uses them. They become data shapes filled from `dash-evo::sml` through `dash-spv::bridge`, with the verification code moved, until CV3/CV4 retire them.
- `dash/src/sml`'s verification code (`merkle_roots.rs`, `quorum_entry/validation.rs`, `verify_message.rs`, `scores_for_quorum.rs`) moves to `dash-evo` (SB7), and `dash/src/bip158.rs` moves to `dash-filters` (SB9).
  - **dashcore keeps adapter functions with the old signatures**, which convert to base-sdk types and call the moved code (DEC-112 O2).
  - A plain `pub use` is impossible, because the moved code takes base-sdk and `dash-consensus` types. Platform calls these paths through `dashcore::sml` (`rs-platform-wallet/src/masternode/list.rs:18-19`, `spv/runtime.rs:11`).
- The FFI's quorum lookup keeps its shape (`platform_integration.rs`). Its semantics are §5.3's, with a consumer note (O3).
- **The bridge (DEC-112 O2).** L2 keeps P and S off W, so `dash-chain`, `dash-mnlist`, `dash-evo` and `dash-filters` take base-sdk and `dash-consensus` types. dash-spv's managers, key-wallet-manager's `WalletInterface`, the FFI, the Rust consumers and #902's facade (`NetworkMessage`, §3.2.1) speak dashcore types. From SB12 on, every header, block, cfheader, cfilter, mnlistdiff, qrinfo, CL and ISLock crosses between them.
  - `dash-spv::bridge` owns every conversion (V, where W is allowed; SB12a). Nothing else in dash-spv converts.
  - It fills the dashcore shapes that Platform-facing queries return: `latest_masternode_list() -> dashcore::sml::masternode_list::MasternodeList` (`client/queries.rs:53-56`; Platform `spv/runtime.rs:507-513`) and `get_quorum_at_height -> QualifiedQuorumEntry` (`queries.rs:73`).
  - **Each header is hashed once.** dash-spv already hashes every header it receives, and `dash_consensus::Header::new` always runs X11 again (`dash-consensus/src/primitives/block.rs:26-27`). The bridge therefore builds the header with `Header::from_parts_hashed(raw, hash)`, which takes a hash computed locally from the same 80 bytes and re-checks it under a debug assertion. *(Superseded 10-16 by SB12a D17 / DEC-224 as amended: `from_parts_hashed` was removed; `Header::new` is the only constructor, the bridge hashes each received header inside `dash-consensus` once, and dash-spv takes the header and hash back.)* Once SB16's router decodes with base-sdk, the dashcore value derives from the base-sdk one instead.
  - DEC-51a gets a one-line note: a header is still hashed once, by its first decoder.
  - Hashing twice would cost ≈ +1 s per 50,000 headers, against SB12/SB14's "`spv-headers-*` not slower" gate.
  - CV3 and CV4 later collapse the bridge into the identity, and CV4 deletes it (§13.2).

---

## 6. Chain events and CH2's requirements

### 6.1 `TipCell` and `FeedHub`

*Erratum E6 (§17): the slot, reader and wake below are E6's (DEC-152, DEC-155), replacing rev6.1's lock-free slot (`ArrayQueue` + `staged` + `SegQueue`), `SubWaker` and `front`/`pop_control`. SB5 §3–§4 is the detailed design.*

```rust
pub struct TipCell<E> { state: Mutex<TipState<E>>, producer_wake: ProducerWaker }
                                    // the tip lock: held to change the tip and assign seq; never across a wait,
                                    // except `finish()` in subscribe_at (DEC-96: the subscriber's durable handoff commit)
struct TipState<E> { tip: BlockRef, next_seq: u64, snapshot: Arc<ChainReadSnapshot>,
                     feeds: Slab<FeedSlot<E>>, pending: Pending<E> /* (seq, event, captured target FeedIds) */ }
pub struct FeedId { index: u32, gen: u32 }   // generational: a reused slab slot gets a new gen; stale ids are no-ops
pub struct FeedSlot<E> {
    gen: u32,
    live: Option<u64>,                       // None = registered, inactive (no fanout, never a target); Some(seq_synced) once live
    inner: Mutex<SlotInner<E>>,              // E6: one leaf mutex; every critical section O(1), no allocation or free inside
    needs: Needs,
}
struct SlotInner<E> {
    queue: VecDeque<(u64, E)>,               // reserved at Q by `register`; never reallocates
    used: u32,                               // queued + the reader's staged items; push fails (`Full`) when used == Q
    ctl_order: ArrayVec<ControlKind, 8>,     // F4, out of band: one outstanding message per kind (DEC-155),
    ctl_payload: [Option<ControlMsg>; 8],    // a same-kind send replaces the payload; cleared only when next() returns it
    outstanding: u8,
    waker: Option<Notify>,                   // F5: notification only (E6)
}
pub enum Notify { Task(Waker) /* wake_by_ref */, Thread(Arc<Parker>) /* condvar permit */ }
impl<E: Clone> TipCell<E> {
    pub fn change_tip(&self, step: TipStep, ev: E) -> Publish;   // under the lock: move tip, seq = next_seq++, publish snapshot
                                                                  // (tip_seq); capture the live targets; try-push; Full(ids) -> pending
    pub fn retry_pending(&self) -> Publish;                       // the tip writer, on its producer wake
    pub fn register(&self, needs: Needs) -> Result<FeedId, Refused>;          // P8; the slot starts inactive
    pub fn subscribe_at(&self, id: FeedId, p: &BlockRef,                       // F10: under the tip lock; makes the slot live
                        finish: &mut dyn FnMut() -> Result<(), SubscriberError>) -> Result<Handoff, SubscriberError>;
    pub fn deregister(&self, id: FeedId);                         // F11: under the lock; strips id from pending; wakes the producer
    pub fn send_control(&self, id: FeedId, c: ControlMsg) -> SendControl; // F4 producer part: out of band; per-kind
                                                                  // coalescing (DEC-155); wakes (F5)
}
pub struct FeedReader { slot: FeedId, staging: VecDeque<(u64, ChainEvent)>, spare: VecDeque<(u64, ChainEvent)>, .. }
impl SubscriberFeed for FeedReader {          // CH2's trait, moved to dash-chain; E6 replaces front/pop_control with next()
    fn next(&mut self) -> Next<'_>;           // ONE slot section: takes the oldest outstanding control (consuming it), and
                                              // swaps queued data into staging; returns Control, else peeks an Event, else Empty
    fn pop(&mut self) -> Option<ChainEvent>;  // frees space: wakes the producer if this slot is a pending target
    fn remove_round_trip(&mut self, b: &BlockHash) -> Option<(u64, u64)>; // compression (CH2 §4.4.1 rule 3) on staging,
                                              // after a data-only intake; frees space likewise
    fn set_waker(&mut self, w: Notify);       // F5; the old waker is dropped after the section
}
```

- **Two event streams.**
  - `TipCell<ChainEvent>` carries only `Connected` and `Disconnected` with `seq`. That is CH2's exact F1 stream, and `dash-chain` owns `seq`.
  - `HeaderAccepted` and `ChainLocked` are `StatusEvent`s on a separate bounded status feed with coalescing (latest best header, latest CL). They never appear in the F1 stream.
- `ControlMsg` is opaque to `dash-chain`. CH2 defines `Control` (`FlushCommit`, `RewindRequest`, `ChainStateFlushed`) and keeps the mirror and the flush barrier in its `SubscriberHub`. *E6 (§17), DEC-155:* each message carries a `ControlKind`; at most one message per kind is outstanding per slot, and a same-kind send replaces the queued payload, which is sound because a control tells the subscriber to re-read the hub mirror (CH2 §4.4.1 rule 1).
- **Inactive until handoff.**
  - `register` creates an inactive slot: no fanout, and never a target.
  - `subscribe_at` makes it live at `seq_synced = next_seq`, under the tip lock.
  - `change_tip` captures its target set (the live slots at that moment) together with the event in `pending`. A slot that becomes live later is never a target of an already-assigned event; that is CH2's X28 pending-at-handoff case.
- **Capacity.**
  - A feed is bounded by item count.
  - `remove_round_trip` must remove a later `Disconnected(b)` from the middle of the feed (CH2:1818-1821). The reader therefore moves items into its own staging deque and does the compression there, outside the slot lock.
  - Staged items count against the capacity through `used`, so queue plus staging never exceeds the bound. *E6 (§17):* the reader takes items by swapping the slot's queue against a spare buffer it owns (a header swap, no element moves), so no slot section is O(Q).
- **Producer wakes.** The tip writer never makes another tip change while `pending` is non-empty. *E6 (§17):* the producer's only blocking is on the slots' leaf mutexes, whose sections are O(1); every wake is notification-only (`Notify`) and runs no subscriber code. That is CH2 rule 5's wait in V's service loop, or the SPV header task awaiting `producer_wake`. *E6a (§17):* inside a hold the holder waits **with** the tip lock held: V's in-hold service wait (DEC-153) and the SPV header writer's between sub-steps (SB5 §4.8). Across a hold it may also block on its own I/O and on lock chains whose holders never wait for the tip mutex. The wait ends on either of two wakes:
  - **space available:** `pop` or `remove_round_trip` frees space on a slot that is a pending target;
  - **deregistration:** `deregister` removes a pending target.

  Either wake leads to `retry_pending`. Without the space wake, a producer that parked on a full queue could miss a subscriber that pops the last item and then parks.
- In the node, `SubscriberHub` (CH2b) wraps `TipCell<ChainEvent>`. In SPV, the driver owns it, and its subscribers are the cfheaders, filters, blocks and wallet adapters and `dash-mnlist`. Each subscriber except the wallet keeps a durable locator, so a restart resumes and a reorg rewinds per subscriber. The wallet's progress is the wallet's own (§9.5, DEC-112 O4).
- Chain-ordered data moves off dash-spv's broadcast bus onto these bounded feeds. The `SyncEvent` bus stays for progress, through an adapter first (SB14), so managers change one at a time.
- **Tests (SB5 acceptance)**, ports of CH2's scenarios to the real producer:
  - X28: pending at handoff, with a slot registered while an event is pending, which must not receive it;
  - F11 with a pending fanout, including slot reuse: a stale `FeedId` generation is a no-op;
  - compression and staging held within capacity;
  - full queue with a space wake: the producer parks, the subscriber pops the last item and parks, and the producer must resume;
  - a deregistration wake for a pending target;
  - `HeaderAccepted` and `ChainLocked` never appear in the F1 stream.

### 6.2 F1–F11 (CH2 §2.9.1)

| F | Owner (CH2 rev5) | How it is met |
|---|---|---|
| F1 ordering from the cut | producer | only the tip writer calls `change_tip`, in tip-change order; queues never drop; pending pushes complete in order before the next change |
| F2 `seq` at the tip change | producer | `seq = next_seq++` inside `change_tip`, under the lock, before any push attempt. `ChainReadSnapshot.tip_seq` is published in the same critical section, and `FlushCommit.tip_seq` reads it. *Erratum E4 (§17): `FlushCommit`'s `(new, tip_seq)` is the pair V captures into `FreezePacket` under its guard, not the published snapshot's.* |
| F3 bounded data queues | producer | queue + staging bound data only (§6.1, capacity). `Full` returns to the tip writer, which waits in its service loop without holding the lock, until a space or deregistration wake; inside a hold it waits with the lock held (E6a, §17). In the fanout section the producer's only blocking is E6's O(1) leaf sections (§17) |
| F4 control out of band | producer (out of band) + CH2 (mirror) | `send_control` bypasses the data bound, and the subscriber reads control before data; the mirror is CH2's |
| F5 wake on enqueue | producer | every data push and every control message wakes the slot's `Notify` (`Parker` for driver threads, `Waker` for tokio tasks; notification only). *Erratum E6 (§17): `Notify` replaces `SubWaker`.* In the other direction, freeing space on a pending target and deregistering one call `producer_wake` |
| F6 weak fairness | producer | node: CH2's driver threads; SPV: tokio tasks. A parked subscriber costs no thread |
| F7 flush barrier | CH2 | `ChainStateFlushed` and `T_ack` are sent by CH2 after B1 returns; `TipCell` takes no part |
| F8 data and ancestry | producer (snapshot, ancestry) + CH2 (prune locks) | the snapshot `{tip, tip_seq, at_height}` is one state. Resident heights come from its own chunks; in SPV, lower heights come by ancestry from its own tip over immutable records, never from the live `active` projection (§4.4, DEC-105 F3). `IndexAccess` is lock-free over immutable entries, and `window = None` in the node keeps every delivered block's entry resident. Block/undo bytes and prune locks are CH2's |
| F9 `best_block()` | CH2 | `TipCell` reports no per-subscriber progress |
| F10 catch-up-to-live | producer | the slot is inactive from `register` until the handoff. `subscribe_at` takes the tip lock and checks `p == tip` (otherwise `Moved`, and nothing goes live). It runs `finish()` under the lock (the DEC-96 exception) and makes the slot live at `seq_synced = next_seq`. No tip change can fall between the check and the first event. An event already pending at the cut captured its targets before the slot went live, so the slot never receives it (X28) |
| F11 deregistration is final | producer | `deregister` takes the tip lock, removes the slot, strips it from `pending` and wakes the producer. Nothing is enqueued afterwards, and no wait targets it. A reused slab slot gets a new generation, so a stale `FeedId` is a no-op |

Neither leg met F10 or F11: both wrote against CH2 rev2's F1–F9. Opus's `FeedHub` also had no tip lock, so `subscribe_at` could not be linearized. CH2's §4.4.2 argument needs no change: every producer row holds.

### 6.3 P1–P10 (CH2 §10.1)

| P | Requirement | How SB0 meets it |
|---|---|---|
| P1 | F1–F11 | §6.2 |
| P2 | one tip writer (V) servicing `FlushDue` in every wait | `HeaderChain::set_active_tip` is the only active-tip mutator, and in the node it runs only on V (CH1-node). `change_tip` never blocks except on leaf mutexes whose every critical section is O(1), and calls no code except notification-only wakes; every other wait is V's service loop. *Erratum E6 (§17).* |
| P3 | flush order, rewind gate, `T_ack` after B1 | node-only, in CH2b's `FlushMachine`. `dash-chain` contributes `take_dirty()` records that CH2 writes inside B1 |
| P4 | prune locks from locators | node-only. `dash-chain` answers `lca`/`ancestor` |
| P5 | a `KvEngine` meeting §2.5.1; each subscriber on its own engine | `KvEngine` moves to `dash-kv` unchanged. Node subscribers keep RocksDB through `RocksTuning`. `dash-kv-sqlite` meets the contract too: recovery to a prefix includes DEC-62's subset semantics, and `snapshot()` pins its read view before returning (§9.4) |
| P6 | one node-wide fail-stop latch | `GlobalFailStop`, `GuardedEngine` and `AbortSink` move to `dash-kv` with CH2a's semantics. The node creates one per process. dash-spv creates one per client, and its `AbortSink` stops the client and emits the FFI error event (L5) |
| P7 | coherent snapshot; atomic handoff | `ChainReadSnapshot` gains `tip_seq` and is published under the tip lock; F10 as above |
| P8 | refuse undo-needing subscribers without undo | `register(Needs { block, undo })` returns `Refused::NoBlockData`/`NoUndo` in SPV; rdashd `-spv` turns every index option into an InitError |
| P9 | mempool coin access as V commands | not shared: SB0 shares no mempool |
| P10 | block index in B1, same batch as meta, marker and evo | §4.7. The node's index lives only in the chainstate engine. The SPV store is never opened in full mode, and no import exists (DEC-104 M5) |

---

## 7. Runtime model

| Context | Node (rdashd full) | SPV (mobile, rdashd `-spv`) |
|---|---|---|
| Sockets | `dash-p2p` tasks on the `net` tokio runtime | `dash-p2p` tasks on the client's runtime. On mobile, dash-spv-ffi sets `worker_threads` = 2 and `max_blocking_threads` = 2 explicitly (O5) |
| Processor | `PeerManager` OS thread | `SpvRouter` task |
| Dialing | ConnMan OS threads (in `dashd-p2p`) call `dial_tick` | tokio interval calls `dial_tick` |
| Header chain owner | V (OS thread) | the SPV header task |
| MN state | node crates on V; mnxcheck task | `dash-mnlist` on the masternodes task |
| CPU pools | rayon `prevalidate`/`script`/`bls` | one injected rayon pool, default `min(2, cores)`: X11 batches, filter checks, BLS batches. dash-spv's validation and key-wallet-manager's matching run inside it through `pool.install`, so rayon's global pool is never built (O5) |
| BLS | BS12's serial verify inside pure and shared code; parallelism only through the host's pool | the same; **blst's own pool never starts**: dash-spv-ffi builds blst with `no-threads`, and `spv-clippy.toml` bans the pooled calls in V (O5) |
| Storage | W (db-writer) + RocksDB background threads | SQLite calls run on the host's bounded `BlockingIo` service, invoked by dash-spv's store writer (§9.5). On mobile that is the client runtime's blocking pool with `max_blocking_threads = 2`, built by dash-spv-ffi. SQLite itself starts no threads. No shared crate offloads or spawns (L5) |
| DNS | `dashd-p2p`'s dnsseed thread, through `Resolver` | the host's `Resolver` (the same bounded pool) |
| Time | `NodeClock` with mocktime; `AdjustedTime` only for peer-facing uses | `NodeClock` = system time; `AdjustedTime` only for peer-facing uses (§4.2) |
| Wakes (E6, §17) | V and CH2b's driver threads park on a `Parker` | subscriber tasks wake through their tokio `Waker`; adapter threads, if any, through a `Parker`. No wake runs subscriber code |
| Allocator | jemalloc in `rdashd`'s `main.rs` only | system allocator |
| Fatal storage error | `GlobalFailStop` → `AbortSink::abort_node` (ND1) | `GlobalFailStop` → client stops, FFI error event, store left consistent |
| Network parameters | `dash-params` per node | `dash-params` per client (no statics) |

Shared crates take as parameters a tokio `Handle` (`dash-p2p` only), an `Arc<dyn Clock>`, a `&rayon::ThreadPool`, an `Arc<dyn BlockingIo>`, an `Arc<dyn Resolver>`, a `GlobalFailStop` and an `Arc<dyn AbortSink>`. They create none of them.

**Where SPV's threads come from (DEC-112 O5).** L5 covers S crates only, and three thread sources sit outside them: `cargo tree -p dash-spv-ffi --target aarch64-linux-android -e normal` contains `rayon`, `rayon-core` and `blst`. Each is closed where it arises:
- **rayon's global pool** (num_cpus threads). It is reached from dash-spv's `validation/header.rs:22` and `validation/filter.rs:75`, and from key-wallet-manager's matching (`matching.rs:71`), which dash-spv's `parallel-filters` enables (`dash-spv/Cargo.toml:17`).
  - All of this work runs inside the host's injected pool through `pool.install`. No path builds or touches the global pool.
- **blst's pool.** dashcore-crypto's `verify_secure_aggregate` calls `dash_pkc::bls::BlsSignature::secure_verify_aggregates` (`crypto/src/bls.rs:405-442`), which is on DEC-79's banned list (`contrib/node/pure-clippy.toml:120`). dashcore's `quorum_validation` and `message_verification` features keep it reachable from dash-spv until SB15.
  - dash-spv-ffi builds blst with `no-threads` through a dependency of its own: target-specific for Android and iOS, and an opt-in feature for the Linux test. blst then runs every pooled call inline, on every path.
  - DEC-79 rejected `no-threads` for the node, because feature unification would make the node's blst serial. The feature therefore lives only in dash-spv-ffi's manifest, and `check-mobile.sh` asserts that no N package's resolved graph enables it.
  - `spv-clippy.toml` gives V DEC-79's pooled-call bans. DEC-79's audit also runs over the dashcore-crypto and key-wallet paths reachable from dash-spv-ffi, and its stamp stays.
  - dash-spv drops dashcore's `quorum_validation` and `message_verification` features once SB15 lands.
- **tokio's defaults.** dash-spv-ffi sets `worker_threads` only when it is non-zero, and `max_blocking_threads` stays at tokio's 512 (`dash-spv-ffi/src/client.rs:48-51`). It now sets both explicitly.

**Thread-count test (m2), on every path.**
- It runs on exercised paths, because the presence of a dependency alone proves nothing.
- During an SPV sync driven through dash-spv-ffi with the mobile runtime configuration, the test counts the threads in `/proc/self/task`. The sync covers headers, filters with wallet matching, the MN pipeline, a reorg and DNS seeding.
- It asserts that no more than the runtime's workers, plus `max_blocking_threads`, plus the measured platform baseline, ever exist.
- It runs in SB13 on Linux, over the **old MN path** (dashcore's verifier, with blst `no-threads`). It runs again in each later row that changes a path: SB14, SB15 (the `dash-mnlist` path), SB16 and SB17 (on the emulator).

---

## 8. Mobile budget

### 8.1 Measurements (aarch64-linux-android, stripped; Appendix A)

| Item | Δ | Source |
|---|---|---|
| Baseline `libdash_spv_ffi.so` | dev 12,551,712 B; integration 12,622,128 B | [F] A; [O] v0 |
| `dash-consensus` (PoW, DGW/KGW, X11, merkle referenced) | +0.18 MB | [F] I − I0 |
| SQLite, default features | +1.82 MB standalone (measured). **0 on iOS** (system library) only while no crate in the iOS graph enables rusqlite `bundled`. `rs-platform-wallet-storage` enables it, and `links = "sqlite3"` unifies the graph, so the per-target features of §9.3 (f) are required (m6). **0 inside Platform's Android SDK and dashwallet-desktop** (both link it already) while the rusqlite major stays in lockstep (§9.3 (f), n4). Both are expected from the dependency graphs, **pending measurement** (combined-SDK build in SB17; iOS on the Mac, Q6) | [O] v4 − v0 |
| SQLite, unused features compiled out | +1.13 MB standalone | [O] v5 − v0 |
| redb | +0.64–0.68 MB | [O] v3; [F] H |
| RocksDB, minimal | +6.86 MB | [O] v2 |
| RocksDB as `dashd-store` builds it | +7.52–7.70 MB | [F] C; [O] v1 |
| Mobile release profile, raw (fat LTO, 1 CGU, `opt-level = "s"` everywhere) | −3.74 MB (to 8.81 MB), measured. The **amended** profile (§8.2 overrides) is **pending measurement** in SB2 | [F] LTO |

Two baseline differences, explained:
- Fable built on dev and Opus on integration (base-sdk fork pins): +70 KB.
- Fable's I0 probe adds ≈ 47 KB of probe exports (row B − row A).

### 8.2 Budget

- **Code.** Each SB PR reports its L7 delta. The shared base's expected net is about neutral: it adds `dash-consensus`, `dash-filters`, `dash-evo` and `dash-p2p`, and removes dash-spv's `network/`, `sml_engine`, `validation/` and the segment store.
- **SQLite** is budgeted separately, as one recorded L7 ruling in SB13. Standalone builds use the v5 flags.
- **The mobile profile** [F] starts from the raw −3.74 MB measurement.
  - Per-package overrides keep `opt-level = 3` on the hashing paths that are actually linked, because `"s"` slows hashing:
    - `dash-pow`, whose eleven hash implementations sit behind `dash-consensus::hash::x11` (`dash-consensus/src/hash.rs:77-81`), until BS15's backend switch;
    - `dash-x11` (after the switch);
    - `dash-sha256`;
    - `dashcore_hashes`;
    - `blst`;
    - the C backends' compiler flags (`-O3` through the `cc` build) wherever `rs-x11-hash`, blst or SQLite are built.
  - SB2 derives the list from the linked symbols and measures the **amended** profile: its stripped Android size and the `spv-headers-*` instruction counts under QEMU.
  - Until then, the profile's size saving and hashing speed are **pending measurement**, not claims.
  - arm64 is the default mobile target (Q5, DEC-112). The x86_64 simulator is dev-only, and the L6 and L7 targets are unchanged.
- **Memory** (mobile, steady; **estimates pending measurement** with the production schema in SB17):

  | Item | Size |
  |---|---|
  | Header window | ≈ 1.2 MB |
  | SQLite page cache | 4 MiB per connection (+3.8 MiB RSS measured): the writer and at most one read connection on mobile (§9.4, m5) |
  | AddrMan | ≈ 0.5 MB |
  | Peer queues | ≈ 4.3 MiB × 8 ≈ 34 MiB worst case: 1 MiB receive, one 3 MiB message admitted before `fPauseRecv`, 256 KiB send (§3.4, m5) |
  | Subscriptions | Σ capacity × maximum message size, set in SB16 within the peer-queue budget (§3.2.1) |
  | Snapshots | ≤ ⌈w/1,024⌉ + 1 `ActiveChain` chunks per lagging reader (≈ 80 KB) (§4.4) |
  | SML lists | ≈ 0.8 MB per distinct list (≈ 4,000 entries), `Arc`-shared. Bounded by §5.3's retention: the tip list, plus the lists that `recent`, `Unverified` commitments and pending fetches need (O3) |
  | Quorum rows | the commitments active in the authenticated domain, plus an LRU; the rest stay in `mn_quorums` within R (§5.3, O3) |

  No whole-chain index is resident: ≈ 10–13 MiB less than the segment store's hash index. Opening the store reads no whole-chain data.

### 8.3 Features

| Crate | Features |
|---|---|
| `dash-p2p` | `inbound` (off for dash-spv) |
| `dash-spv` | `default` = the SPV client on SQLite; `log-file` (tracing-appender, chrono: file logging with rotation) and `env-filter` (`RUST_LOG`), both off by default. There is no `cli` feature: clap lives only in the `dash-spv-cli` crate (the `dash-spv` and `dash-spv-ffi` binaries), which turns the two on (SB3) |
| `dash-spv-ffi` | `log-file`, `env-filter`: pass-throughs to dash-spv, off by default (`dash_spv_ffi_init_logging` with a log directory returns `RuntimeError` without `log-file`) |
| `dash-evo` | none; node-only code is in `dash-dml` |
| `dash-consensus` | used with default features off where the script module isn't needed; `--gc-sections` drops the rest (row I) |

---

## 9. Storage

### 9.1 By role

| Role | Engine |
|---|---|
| Node chainstate (coins, block index, `mn_*`, `llmq_*`, credit pool, MNHF) | **RocksDB** through `RocksTuning` (DEC-43, CH2) |
| Node subscriber DBs (txindex, blockfilterindex, coinstats…) | **RocksDB** |
| Node block and undo files | **flat files** (`dashd-store::flatfile`, ST2; DEC-84 portability is ST2's follow-up) |
| Node `peers.dat`, `banlist`, `anchors.dat` | Core formats (P2a §4) |
| SPV store, mobile, desktop and rdashd `-spv` (headers, hash index, cfheaders, filters, matched blocks, MN state, CL, locators, peers, bans) | **SQLite** (`dash-kv-sqlite`) |

### 9.2 Evidence (Appendix A)

1. **Size.** RocksDB adds 6.86–7.70 MB (+54–61 %) to a 12.6 MB library. SQLite adds 1.13–1.82 MB standalone, and nothing on iOS, inside Platform's Android SDK or in dashwallet-desktop.
2. **Memory.** On 2.55 M rows: SQLite +3.8 MiB at 5–6 µs per random read; RocksDB with mobile settings +17 MiB at 42–47 µs, or +21 MiB at ≈ 10 µs with all file descriptors; the node configuration +41–72 MiB at 7–9 µs. Fable's RssAnon probe (1.6 / 9.5 / 6.9 MiB at small scale) agrees that memory alone would not reject RocksDB; size, toolchain and durability decide.
3. **Durability.** SQLite in WAL mode recovers to a prefix of committed transactions, stronger than DEC-62's subset semantics, so ST3a's `FaultyEngine` checks an SQLite-backed protocol conservatively. SQLite honours `fullfsync` on Apple; the pinned `librocksdb-sys` never defines `HAVE_FULLFSYNC` (BL-32). The segment files' temp + `sync_all` + rename protects one segment, not a cross-store step.
4. **Atomicity.** One database, per-component atomic `WriteSet`s, WAL-ordered (§9.5). Each component's rows commit together with its locator, and a later commit is durable only together with every earlier one (n3). The segment design has no multi-store commit.
5. **Startup.** O(window) with an indexed table, instead of O(chain) reads to rebuild an in-memory index.
6. **Portability (DEC-84).** C only, no C++ or bindgen; builds on every Core platform.
7. **Already in the product.** Platform's SDKs and dashwallet-desktop link rusqlite 0.38.

Fable's reason against SQLite ("one database for `--spv` and full") falls away under DEC-104 M5: full mode re-syncs headers from genesis and never reads the SPV store (§4.7). Its reason against a second engine on mobile does not apply: SQLite is already there.

### 9.3 Proposed ruling (DEC-43 re-checked under DEC-85)

> **DEC-43 (re-checked under DEC-85/SB0).**
> - (a) The node's chainstate, EvoDB column families and subscriber DBs use RocksDB through `RocksTuning` (CH2 §2.5.1), unchanged; block and undo files stay flat files.
> - (b) Every SPV store, mobile, desktop and rdashd `-spv`, uses SQLite through `dash-kv-sqlite`. RocksDB never enters the mobile closure (L6).
> - (c) Both engines implement the `KvEngine` contract (DEC-62 subset semantics or stronger), and no pure or shared crate names an engine.
> - (d) In full mode the block index is written only in the chainstate engine, in B1 (P10).
> - (e) The DEC-43 low-memory cell still gates M3, and CH2a proceeds whole.
> - (f) `dash-kv-sqlite` pins rusqlite to Platform's line (0.38), and bumps move both. It selects rusqlite per target: `bundled` on Android and desktop, the system SQLite on iOS. Because `links = "sqlite3"` unifies the graph, a combined iOS build must not enable `bundled` anywhere; `rs-platform-wallet-storage` does today, so Platform needs the same per-target choice (m6).

### 9.4 `dash-kv-sqlite`: the contract in SQLite terms [O]

| `KvEngine` | SQLite |
|---|---|
| column family | one `WITHOUT ROWID` table `(k BLOB PRIMARY KEY, v BLOB NOT NULL)`; BLOB order is memcmp |
| `KvBatch` | an op list: `put`, `delete`, `delete_range` |
| `write(b, Buffered)` | `BEGIN IMMEDIATE … COMMIT`, WAL, `synchronous=NORMAL`: atomic; survives a process kill; a power loss may lose a suffix |
| `write(b, Durable)` | the same with `synchronous=FULL` (+ `fullfsync=1` on Apple). It covers every earlier commit (sequential WAL) |
| `sync_wal()` | a Durable commit of `meta.sync_seq += 1` if any Buffered commit is newer; else a no-op |
| `delete_range(cf, a, b)` | `DELETE … WHERE k >= ?a AND k < ?b`: O(rows); `EngineTuning::unbounded_delete_span` is the journal's `d_max` |
| `snapshot()` | Taken on a **dedicated read connection**. The engine owns a capped pool of them (1 on mobile, 2 elsewhere), and a `Snapshot` owns its connection until drop, when `ROLLBACK` returns it to the pool. When every connection is taken, `snapshot()` waits for one and never opens more, since each connection carries a 4 MiB page cache (m5). A caller never holds two snapshots at once (a debug assertion), so the wait cannot deadlock.<br>`snapshot()` runs `BEGIN DEFERRED` and then **reads the `meta` schema row before returning**. SQLite fixes a deferred read transaction's WAL end mark at its first read, not at `BEGIN`, so this read is what pins the view as of the call (`dashd-store/src/engine.rs:114-115`; DEC-104 M6). Later commits are invisible.<br>`snapshot()` is infallible in the contract (`engine.rs:115`). An error from opening the connection, from `BEGIN` or from the pinning read therefore trips the latch (`AbortSink::read_failed`, DEC-78) and returns a **poisoned** snapshot, every read of which returns the latched error. An unpinned view is never served. `SQLITE_BUSY_RECOVERY` is retried a bounded number of times, then trips the latch |
| reads | cached prepared statements; range scans `ORDER BY k` |
| fail-stop | `SQLITE_FULL`, `SQLITE_IOERR*`, `SQLITE_CORRUPT`, `SQLITE_NOTADB` trip the latch; read failures call `AbortSink::read_failed` (DEC-78).<br>**iOS host requirement (m6):** the data directory's protection class is `NSFileProtectionCompleteUntilFirstUserAuthentication` or weaker. Under `NSFileProtectionComplete`, a sync while the device is locked fails with `SQLITE_IOERR` or `SQLITE_CANTOPEN`, which trips the latch |
| threads | none; checkpoints inline (`wal_checkpoint(PASSIVE)` at `after_bk` or on the owner's schedule) |
| memory | `cache_size=-4096`, `mmap_size=0`, `temp_store=MEMORY`, `wal_autocheckpoint=1000` |
| schema | `DbSchema` → tables; the `meta.schema` row and version check as `dashd-store/src/schema.rs` |
| writer | one write connection per engine; commits are serialized by its owner (the store writer, §9.5) |

**Engine contract tests (SB10; `dash-kv`'s contract suite, run against both engines):**
- **The interleaving:** write A; take `snapshot()`; the writer commits B; the snapshot reads A; the writer commits C; the snapshot still reads A.
- **A negative control:** without the pinning read, a bare `BEGIN DEFERRED` returns B. This documents why the read is there.
- **Pool behaviour:** more concurrent snapshots than the cap. The extra callers wait, and no connection beyond the cap is opened (m5).
- **Error injection:** the pinning read fails, the latch trips, and every read of the returned snapshot fails.

### 9.5 The SPV store (dash-spv, storage version 3)

| Table | Contents |
|---|---|
| `meta` | schema, anchor, `anchor_state` (`Candidate` or `Established`, §4.3.1), `next_id`, best-header id (a diagnostic; recomputed at open, E5 §17), tip id, `sync_seq` |
| `hdr` | hash → id ‖ height ‖ 80-byte header ‖ skip id ‖ status ‖ absolute chain work: **every accepted header of every branch**, plus the anchor pack as `CONTEXT` (§4.4) |
| `hdr_id` | id → hash |
| `hdr_h` | (height, id) → (): all branches by height, for `open` |
| `active` | height → id: the live active chain, a **projection** rewritten for the changed heights in the header step's transaction. Live queries only; snapshots never read it (§4.4) |
| `cfhdr`, `cfilter` | height → 32 B; height → bytes |
| `blocks` | wallet-matched blocks |
| `mn_lists`, `mn_quorums`, `mn_snapshots`, `locks` | verified MN state with status, retained per §5.3 (O3); best and pending CL |
| `locators` | per-subscriber durable locator; none for the wallet (O4) |
| `peers`, `bans` | AddrMan and BanList (`NetStore`) |

**Transaction assembly (the SB13 brief makes this explicit).**
- **One writer.** A single store-writer task owns the write connection. Every other task submits a `WriteSet`, and each `WriteSet` is exactly one `BEGIN IMMEDIATE … COMMIT`. Nothing else writes the store. *E8 (§17):* the SPV header step **leases** that connection for its stage 3 (one message out, the connection back) and runs `BEGIN IMMEDIATE`, its row batches, its tail and `COMMIT` on the header task's thread before returning it. Other `WriteSet`s queue meanwhile, as behind one long `WriteSet`. Cold reads use a separate read connection.
- **The header step's `WriteSet`** is assembled by the header task (*E8 (§17):* streamed through the lease in batches of ≤ W rows, not built as one vector; SB5 §3.7). It holds, in a fixed order:
  1. `HeaderChain::take_dirty()` (`hdr`, `hdr_id`, `hdr_h`);
  2. the `active` changes;
  3. the `locks` rows (best and pending CL) when the step applied a ChainLock, because a ChainLock changes header status and the floor;
  4. `meta`.

  It commits **before** `set_active_tip` publishes the step's events (§4.6), so no subscriber ever sees a block whose header row is not committed.
- **Each subscriber's `WriteSet`** (cfheaders, filters, matched blocks, `dash-mnlist`'s diff and qrinfo rows) holds its data and **its own locator**. A subscriber's write set names only blocks whose header step has committed, and WAL order makes the earlier commit durable whenever the later one is.
- **Durability.** Commits are Buffered. A Durable commit runs at each checkpoint boundary, at sync-complete and on `stop`, and since the WAL is sequential it covers every earlier commit.
- **Recovery is blind:**
  - open and `sync_wal`;
  - read the locators;
  - resume each subscriber from its own locator. A locator that names an unknown hash rewinds that subscriber to the anchor, resetting its data and its locator together in one `WriteSet`;
  - resume the wallet from its own progress (below).

  A crash can lose only a suffix of Buffered steps, which are downloaded again.
- **Wallet progress (DEC-112 O4).** The wallet's progress lives with the wallet: `WalletInterface::{synced_height, wallet_synced_height, update_wallet_synced_height, last_processed_height}` (`key-wallet-manager/src/wallet_interface.rs:131-171`). The host persists it in its own database: Platform's `platform-wallet-storage`, or dashwallet-desktop's `dw-appdb`. The two databases have no atomic commit, so:
  - **The wallet's synced heights are authoritative.** The SPV store keeps no wallet locator.
  - **Resume point.** Each wallet's feed resumes at min(`wallet_synced_height`, the durable SPV tip).
  - **Synced block hash.** An additive `WalletInterface` method (#854-style, with a default) returns the block hash at the wallet's synced height. An additive sibling of `update_wallet_synced_height` hands the wallet that hash when its checkpoint advances. *Erratum E3 (§17): it hands and returns a block locator, whose first entry is that hash.*
  - **Check at open.** The SPV store keeps every accepted header of every branch by hash (§4.4), so it checks the wallet's hash once:
    - on the active chain: nothing to do;
    - on a stale branch: one `truncate_above` at the height where that branch leaves the active chain, then resume from there;
    - unknown to the store, because the SPV store lost a Buffered suffix the wallet saw (DEC-115 R6-M2).
      - *Superseded by erratum E3 (§17, DEC-140): rewind to the first entry of the wallet's stored locator that is on the restored active chain, else the anchor; the floor is not a recovery bound. The rule, the argument and the witness below are kept for the record.*
      - **The rule.** No fork point is inferred from heights. The client calls `truncate_above(H)` once and rescans from H. H is the durable ChainLock floor's height capped at min(the wallet's synced height, the durable SPV tip). When the durable state holds no floor, H is the anchor height. If the wallet's synced height is at or below the floor's height and its hash is unknown, the wallet didn't follow this store (a restored or added wallet), so H is the anchor height (rev6.1, Opus r7 R7-m1).
      - **No lost ancestry is fetched.** The client never re-fetches the lost branch's headers to find the real fork point.
      - **The cost** is a rare rescan after power loss. ChainLocks keep the floor within a few blocks of the tip. With MN sync off there is no floor, and the rescan starts at the anchor.
      - **Why H is on both chains.** Let F be the durable floor, at height f.
        - F and its ancestors are in the durable state. The durable tip was published with F in force, so it descends from F (§4.1, R6-M1).
        - The wallet's block was on a tip published in the lost suffix. Every published tip descends from the floor in force at publication (R6-M1), and that floor is F or a later floor.
        - A later floor descends from F under ChainLock safety: no two verified ChainLocks conflict. Every ChainLock check rests on that assumption, and Core does not re-check it when a higher lock arrives (`v24 chainlock/chainlock.cpp:106-129`).
        - So both chains contain F, and they agree at every height up to f. A wallet block at or below f would be an ancestor of F and known to the store, so an unknown block lies above f, and H = f in practice. The caps only guard the arithmetic. *Erratum E2 (§17): published tips may trail the floor, so the chains agree up to min(f, both tips' heights) and the caps carry the argument; the rule is unchanged.*
        - Without a durable floor, the anchor is on every chain the client accepts, because a branch that forks below it is refused (`ChainError::BelowAnchor`, §4.1.1).
      - **Sol r6's witness.** The durable state is A100 with the floor at A70. A Buffered reorg at 80 is published as B110, and the wallet durably reaches B110. After power loss, the client calls `truncate_above(70)`, never 100: keeping B81…B100's effects would confirm a B90 payment falsely or miss one in A90;
    - an implementation without the method returns `None` and cannot be checked. The client logs that once, and the consumer note asks implementers to add it; SB14 adds it to key-wallet-manager.
  - **Ordering.** The wallet may advance its synced height past a block only after that block's header step has committed, which commit-before-publish (§4.6) already gives. Buffered-versus-Durable skew between the two databases is absorbed by the min rule and the hash check. *Erratum E3 (§17, DEC-144): The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. The skew is absorbed by the locator rewind.*
  - `truncate_above` is #1104's (§4.6, m7).
- **Anchor state (§4.3.1, DEC-107).**
  - While `anchor_state = Candidate`, the store holds header rows, `meta` and peer data only; no MN, lock or subscriber row is written. A fallback clears the header tables and writes the new candidate in `meta`, in one Durable transaction.
  - The Durable transaction that sets `Established` precedes every MN, lock and subscriber write.
  - After it, nothing rewrites `meta.anchor`, and nothing clears `hdr`, the MN tables, `locks` or a locator. Each subscriber's data and locator change only through its own `WriteSet`.
- With MN state persisted as verified rows, the message-log replay (`storage/masternode.rs:160-161`) and #1081's need for it go away. Platform's lookups, which fall back to that log today (`client/queries.rs:73-105`), read §5.3's retained rows or fetch on demand (O3).

**Migrator v3** (after `CURRENT_VERSION = 2`) streams segment files into tables with no O(chain) memory and drops the MN message log, since one qrinfo and a few diffs rebuild that state. A failed migration leaves v2 intact and falls back to a fresh sync from the anchor. A migrated store is `Established` at its v2 anchor; a coverage miss there is handled like any later miss (§4.3.1 step 4). Wallet data is not dash-spv's and is untouched.

**Verification:**
- an SPV-store `Sut` in ST3a's harness (SB10), with crash points between a header step's commit and a subscriber's commit;
- a native `sqlite_wal_prefix.rs` regression beside `rocks_wal_nonprefix.rs`;
- ST3b's device harness on ext4/XFS;
- Apple durability on pasta's Mac (BL-32 style).

### 9.6 Fallback if SQLite is refused (Q2)

Keep the segment files and add:
- a persisted hash index, so open is O(window);
- **SB-ST**, Fable's crash model: a per-segment checksum trailer, temp + rename, truncate-to-intact on load, and ST3a's `FaultyFlatFiles` as a second `Sut`;
- an atomic step marker covering header, MN and locator writes.

This beats redb, which has no column-family batch with CH2's durability modes and wrote 2.55 M rows in 22 s into a 1,029 MiB file in the probe.

---

## 10. Migration and coordination

### 10.1 Compatibility rules [O]

| Surface | Rule |
|---|---|
| C ABI (43 functions) | **Baseline:** integration's 43 functions plus #1104's `on_chain_truncated` in `FFISyncEventCallbacks`. #1104 is the one allowed layout change. If it has not landed when SB17 starts, the baseline is pre-#1104 and #1104 is recorded as that exception.<br>**Frozen** against that baseline: no signature, layout or enum-value change.<br>**New capability is additive**: new functions, or new callback tables that begin with `size: usize` and `version: u32`. The library reads only the fields that lie within the caller's `size`, and treats absent ones as null callbacks.<br>**Tests:** `ffi-c-tests/header-tests/dash-spv.c` compiles in every stage. A C caller built against the **baseline header** runs against the new library (old table layout, smaller `size`). Compiling the current headers alone does not show compatibility |
| FFI events | A translation layer (`dash-spv-ffi/src/events.rs`, SB17) maps internal events onto the frozen callback tables, so internal enums can change without an ABI change. #1104's `on_chain_truncated` is the last layout change to the sync table |
| Config | `FFIClientConfig` is opaque, so `ClientConfig` can change behind the setters. `FFIValidationMode` stays accepted; the shared kernel's checks are not optional, and the modes relax only redundancy such as multi-peer cfheader comparison |
| Rust API | Changes only where #902 changes it, where SB16 extends #902's facade (§3.2.1), and in storage construction (`SpvStore`). `DashSpvClient` queries and `EventHandler` keep their shapes, and `WalletInterface` gains only additive methods with defaults (§9.5).<br>**Consumers** (m4): `platform-wallet`, including its direct uses of `DiskStorageManager`, `BlockHeaderStorage::{get_tip, get_header}` (`spv/runtime.rs:194-197`, `:648-682`), `PeerNetworkManager` and `CheckpointManager`; dashwallet-desktop (`dw-engine`, `dw-ffi`, `dw-p2p`: `ClientConfig`, `DevnetConfig`, `SyncProgress`, `BlockHeadersProgress`, `FilterHeadersProgress`, `MasternodesProgress`, `EventHandler`, `NetworkEvent`); rs-dapi (`dash_spv::Hash`); `masternode-seeds-fetcher`.<br>**Consumer notes** (NFR-CMP-01), one per breaking PR, including: `DevnetConfig` when the devnet `OnceLock`s retire (§5.3); `SyncProgress` for the `Candidate` and coverage phases (§4.3.1); `get_quorum_at_height`'s semantics (§5.3); the `WalletInterface` additions (§9.5); and `SpvStore` replacing the storage types above (SB13) |
| Data directory | Migrator v3 (§9.5) |
| Behaviour | A stored chain that fails the kernel's rules is discarded: the client logs the rule and re-syncs from the anchor |

### 10.2 Stages

Each stage is one upstream-shaped PR series on `node/integration`, written to dev's conventions and green in the `spv` and `ffi` CI groups plus the dashd suites. Per DEC-87 they stay local until pasta decides otherwise (Q2).

| Stage | Rows | Change | Touches `network/`? |
|---|---|---|---|
| 0 | SB1, SB2, SB3 | gates, mobile profile, dash-spv hygiene | no |
| A | SB12 | the shared kernel's header, block and filter rules on dash-spv's existing paths, from `dash-consensus` and `dash-filters`; anchor windows in the checkpoint data. Scope per PLAN §7 handling | no |
| B | SB13 → SB14 → SB15 | SQLite store + migrator v3; dash-spv on `dash-chain` (SyncEvent adapter first, then feeds; reorgs without restart); MN on `dash-mnlist` | no |
| C | SB16 | `SpvRouter` on `dash-p2p` behind #902's facade; the old network behind `legacy-net` for one release [F] | **yes**: needs #902 merged or Q1 answered |
| D | SB17, SB18 | FFI event translation and mobile cells; rdashd `-spv` | no |

Nothing before stage C edits `dash-spv/src/network/` (A1 §2.24's isolation rule, kept until then).

### 10.3 #902 coordination

- SB0 never comments on, pushes to or rebases #902.
- Stages 0, A and B do not touch `network/`. Their conflicts with #902 are in the sync managers, which #902 already conflicts with on dev.
- The node never waits for #902: `dashd-p2p` builds on SB6.
- **SB16 is shaped as #902's announced follow-up**, and it **extends** #902's facade rather than keeping it verbatim (§3.2.1; DEC-104 M7).
  - Kept: `RequestKey` deduplication, the classes, the bandwidth controller and the mock's behaviour. Their tests move `git mv`-style so blame survives.
  - Changed: subscriptions become bounded handles, and `cancel`, a terminal `RequestFailed`, and `RequestId`/`AttemptId` are added. The consumer and test migration is documented (§3.2.1).
  - It supplies the reputation handling and peer persistence the author deferred, plus per-peer queues, attempt caps and `notfound` handling.
  - Coordination with #902's author goes through pasta. If #902 is still open when SB16 is ready, the choice is Q1.
- **#902 also reaches stage B** (m8). It changes 26 files under `dash-spv/src/sync`: the managers that SB14 and SB15 rewrite into feed subscribers.
  - Absorbing it after SB14 would be unsized work on the SPV track (≈ +M).
  - Q1 is therefore needed before SB14 (≈ week 16). If #902 lands first, integration rebases onto it before SB14 starts.
- ZocoLini's #1136–#1138 (DEC-87) are untouched.

### 10.4 Other in-flight work (NFR-CMP-04)

| PR | Overlap | Handling |
|---|---|---|
| #803 | difficulty and fork detection in dash-spv `chain/` | SB12 and SB14 reuse its locator and fork-detection shape where it lands first, and rebase onto it if it merges |
| #799, #1077 | MN-list merkle root and CbTx checks in `dash/src/sml` | SB7 moves that code into `dash-evo::sml`. Whichever lands first, the other rebases. The maintainers see SB7's draft before it lands on integration |
| #1104 | wallet `truncate_above` | SB14 consumes it, or carries it if it has not merged (m7) |
| #1081 | MN message replay | superseded by persisted verified state (SB13/SB15); no conflict before then |
| #375, #921 | checkpoints, anchor lowering | #375's generator emits the anchor windows (§4.3); #921's anchor lowering is not adopted (no backfill, DEC-104 M3) |

---

## 11. Performance

### 11.1 The node is never slower than dashd

| Hot path | Why the shared base stays off it | Check |
|---|---|---|
| Message processing | the node processor pulls per-peer queues as Core's `vProcessMsg`; no pump task, no broadcast hop, no SPV hooks | `diff_p2p_*`, PF0 `process_messages` |
| Header acceptance | `HeaderChain<S>` monomorphised; `window = None` folds the cold branch; hash once (DEC-51a) | PF0 `chain_accept_headers` vs Core's `ProcessNewBlockHeaders` |
| Tip change | `change_tip`: one uncontended lock, one seq, try-pushes | PF0 `feed_publish` counters |
| Block validation, coins, flush | untouched (CH2, CH3) | CH2's DEC-43 cell |
| MN state | node crates as designed; mnxcheck on its own task | EV2 benches |
| Storage | RocksDB unchanged; SQLite never in full mode's chainstate | L2: `dash-kv-sqlite` not in `dashd-chainstate`'s closure |

### 11.2 SPV is not slower than dash-spv today

- `spv-headers-*` (A1:2008; scenario list A1:593) keeps `header_sync_s` and `peak_rss_mib`.
- SB17 adds `spv-filters-*`, `spv-startup` (open-to-ready with 2.55 M stored headers) and `spv-size` (L7).
- Each SB12–SB16 PR reports them against its parent.

### 11.3 Risks

| Risk | Mitigation |
|---|---|
| X11 on aarch64: `dash-x11` has only a portable path, and `rs-x11-hash` (C) may be faster on phones | BS15 measures aarch64 (QEMU instruction counts with the `dash-sha256` recipe, plus the emulator) before switching the mobile default |
| DGW/MTP per-header cost in SPV | integer work over a 24-entry resident window; small next to X11; measured in `spv-headers-*`. A genesis-origin sync reads the pre-DGW eras over a resident, borrowed context of up to 576 (BTC) or 4,033 (KGW, with its exit-guard read) entries, as Core does (§4.1.1) |
| SQLite write rate on phone flash | Buffered `NORMAL` per step, Durable only at boundaries; the emulator measures no flash, so the write-rate cell is a real-device cell under Q6, run by pasta (m6) |
| `dash-p2p` at 125 peers | per-peer queues, no global send lock, copy-on-change snapshot; sim at 125 peers in SB6 |
| Two processors drift | shared handshake, gates and decode policy are the same functions; L9 registries tested in both; O13a transcripts for the node, SB16's transcript set for SPV |
| mnxcheck bandwidth | one mnlistdiff per 16 blocks and one qrinfo per cycle, off on mainnet by default |
| Deep reorg below the window | correct without a ChainLock: every branch's records stay in the cold tier and resolve by id (§4.4); slower only; gated by SB5's >10,000-block test |
| Anchor too shallow for the active quorums (a recent checkpoint, or failed DKG rounds keeping an old quorum active) | initial sync checks coverage over the actual required history, including the recent-lock horizon, while the store holds headers only. It falls back to an earlier checkpoint, ultimately genesis, before anything else starts, and each fallback costs one more forward header sync. Once established, the anchor never moves. A later miss (a reorg that restores a deeper requirement) makes those quorums `Unavailable` and raises an operator diagnostic. Their locks are unavailable and never checked against another quorum. The remedy is a user-initiated resync (§4.3.1) |

---

## 12. Rework list

### 12.1 Per design

| Design | Changes | Stays |
|---|---|---|
| **A1** | See §12.3 | D1, D8, D44, D49 |
| **P2a** (rev3) | Splits:<br>• **SB6** takes gates and decode policy (§3.2), handshake (§3.3) with `LocalIdentity` as a parameter, ping (§3.5), addr intake (§3.6), AddrMan (§3.7), ConnMan's `*_step` functions (§3.8, sans-io) and the inbound parts of §3.9 behind `inbound`;<br>• **P2a-node** keeps the actor loop (§3.1), `send_messages`, PQ quirks, both receive queues, headers sync (§3.4), misbehaviour scores (§3.10), the 74-row registry, and the ConnMan threads that call the steps.<br>Also: P2a:121 "dash-spv `src/network/*` not reused" is reversed (SB16); the `NodeTable` is `dash-p2p`'s; P2a's unconditional `push_message` and its retained-memory charge (§2.2.1) become `NetCore::push` (§3.1), unchanged in semantics; the `ChainAccess` reads come from `dash-chain`'s snapshot, while `process_headers` still goes to V; `dashd-clock` → `dash-clock`; shutdown order is the host's; "sole loop" means "the node's only processor" | verification §6, performance §5, M2 transcripts |
| **CH1** (row only) | Splits:<br>• **SB4/SB5** (`dash-chain`): arena, index, acceptance, best header with Core's update rules and their hooks, SPV's fork-choice target (O1), snapshot, `IndexAccess`, `AnchoredAncestry`, rule-sized `RuleContext`, record codec, load with PoW check, anchor candidate rule and initial-sync fallback, window and cold records, `TipCell`;<br>• **CH1-node** (`dashd-chainstate`): `ChainCommand` actor on V, including `apply_chainlock` from `dashd-locks` (m9), the best-header hooks at Core's `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound` sites (O1), block-data status and positions on the shared entries, `ChainView` (+`mn_list`, `best_chainlock`, `ibd`, `mtp`, `bls`, `epoch`), the index records in B1, R6.<br>SB4 writes the short design CH1 never had | the acceptance tests of the CH1 row |
| **CH2a** | Proceeds whole. Keep `GlobalFailStop`/`AbortSink` in a module with no RocksDB imports (`fail_stop.rs`) so SB10's move is mechanical | everything |
| **CH2b** | `i:SB5`. Its `SubscriberHub` wraps `dash-chain::TipCell`; `Control` stays CH2's type, sent through `send_control`; the latch types live in `dash-kv`; `register` takes `Needs` (P8). b1/b2 can start against stubs of SB5's API | F1–F11, P1–P10, the protocol, the model and the CRASH suite |
| **CH2c, O2a-CO** | unchanged | — |
| **ST2** | unchanged (node-only); DEC-84 portability (`flatfile/fs.rs:10-11`, `:246`) is ST2's follow-up | — |
| **ST3a** | unchanged. `FaultyEngine` and `World` move with `dash-kv` after merge (SB10). SB10 adds the SPV-store `Sut` and `sqlite_wal_prefix.rs`. `rocks_wal_nonprefix.rs` stays in `dashd-store` | — |
| **EV0** | The split: shared `dash-evo` (`ctx`, `effect`, `bls`, `llmq` traits, `sml` skeleton, `locks` skeleton) and node-only `dash-dml` (`dml`, `store`, `creditpool`, `mnhf`), except that `MinedCommitmentReader` moves from `store` to shared `llmq`. `QuorumMembers<E> = Arc<[E]>`; associated `Error` types on `LlmqChain`, `LlmqSource` and `SnapshotRows`, with `MnListView` infallible; `MemberError<E>` (`Missing`, `View(E)` or `UnknownLlmqType`) with no `StoreError` in `dash-evo`; the node keeps LQ1's D4 fail-stop and SPV maps `Missing` to `Unavailable` (§5.1, m3); `ProTxHash` in `dash-evo`. `imbl` moves with `dash-dml` | types-first, in-memory store, `EvoDelta`, `Effect` |
| **EV2a/b** | Crate `dash-dml`. EV2a adds `impl MnListView for MnList`. `MnList::project_sml` goes to EV4-serve (DEC-109 (2)). §2.12's "dash-spv: none direct" now reads: EV4-serve's projection feeds mnxcheck | the DML model, CHAMP order, payee queue, store |
| **EV4** | Splits:<br>• **EV4-core** → SB7 (M2–M3): SML entry and hash, `merkleRootMNList`/`merkleRootQuorums`, CbTx partial-merkle check, coinbase-CL predicate (LQ1 §3.3.1);<br>• **EV4-serve** (M4, node, `dash-dml`): `MnList::project_sml` (Core `CDeterministicMN::to_sml_entry`, `v24 evo/deterministicmns.cpp:43-46`; list build `:255-259`), incremental root cache, mnlistdiff builder in CHAMP order (quirk 41), evodb importer | FR-EVO-07/08 acceptance |
| **LQ1** (rev4) | Generic member source with associated error types (D4 preserved on the node). The SML adapter (LQ1 §2.2.4) becomes production **for SPV's `membersSig` input only**: SPV trusts commitment-verified keys and claims no exact membership (§5.1, DEC-104 M8). Exact membership stays node-only. §6.4's inverted fixture test is replaced by direct use plus mnxcheck. LQ1b-F's dash-spv test PR becomes part of SB15; the 11 rotated captures become shared vectors | algorithms, predicates, `log2`, effects, persistence contract |
| **LK1a** | CL/IS verification comes from `dash-evo::locks` (SB8); the node role is unchanged | Core order, relay, enforcement |
| **IX1** | blockfilterindex builds with `dash-filters` (SB9) | everything else |
| **ND1** | Host services for shared crates (clock, `AbortSink`, shutdown order), `-spv` InitErrors (P8). No dash-spv dependency (L2) | init order, mocktime, logging |
| **O13a** | node transcripts unchanged; SPV handshake transcripts belong to SB16 | — |
| **O16** | also runs the dash-spv suites *as* rdashd `-spv` (SB18), besides against rdashd | — |
| **BS15** | requires an aarch64 measurement before `dash-x11` becomes the mobile default (§11.3) | — |

### 12.2 Code that moves

| Code | From | To | Row |
|---|---|---|---|
| `engine.rs`, `fail_stop.rs`, `schema.rs`, `table.rs`, `codec.rs`, `memory.rs`, `error.rs`; ST3a `faulty/`; CH2a's latch types | `dashd-store` | `dash-kv` | SB10 (after ST3a and CH2a merge) |
| `rocks.rs`, `flatfile/` | `dashd-store` | stay | — |
| `dash/src/bip158.rs` | dashcore | `dash-filters`; dashcore keeps adapter functions with the old signatures (O2) | SB9 |
| `dash/src/sml` verification (`merkle_roots.rs`, `quorum_entry/validation.rs`, `verify_message.rs`, `scores_for_quorum.rs`) | dashcore | `dash-evo::{sml,llmq,locks}`; dashcore keeps the data shapes and adapter functions with the old signatures (O2) | SB7, SB8 |
| `dash-spv/src/chain/` (checkpoints, chain work), header acceptance in `sync/block_headers` | dash-spv | `dash-chain` (checkpoint data stays in dash-spv as `CheckpointView`) | SB4, SB14 |
| `dash-spv/src/sml_engine*`, core logic of `sync/{masternodes,chainlock,instantsend}`, `storage/masternode.rs` | dash-spv | `dash-mnlist` | SB11, SB15 |
| `dash-spv/src/storage/*` | dash-spv | `SpvStore` on `dash-kv-sqlite`; segment code removed after migrator v3 | SB13 |
| `dash-spv/src/network/*` | dash-spv | `dash-p2p` + `dash-spv::router` | SB16 |
| `dash-spv/src/validation/` | dash-spv | `dash-consensus`/`dash-evo`/`dash-filters` calls | SB12 |
| dashcore ↔ base-sdk conversions | dash-spv (new) | `dash-spv::bridge` (§5.5) | SB12a |
| `src/main.rs` (clap) | dash-spv | `dash-spv-cli` | SB3 |
| R2 enforcement (`deny.toml:48-80`, `r2_reach.py`, `check-r2.sh`) | `contrib/node` | removed; L1–L9 tooling | SB1 |

### 12.3 A1 edits

| A1 place | Edit |
|---|---|
| §0, §1.2 | one stack; P2P raw frames in `dash-p2p`; the SML shared in `dash-evo` |
| R2 (A1:147, 448, 492, 628), §1.1 (A1:124) | replaced by L1–L9 (§2.3). M1's exit criterion becomes "`check-layers.py`, `check-shared.sh` and `check-mobile.sh` pass" |
| §1.8 (A1:503) | CV1–CV4 keep scope; CV5 → SB9 + SB16 (+SB18) |
| §2.1 rule 8 (A1:527) | dropped; replaced by L6, L7 and §10.2's stage discipline |
| §2.5 (A1:685) | algorithms shared in `dash-evo`, node state in `dash-dml` |
| §2.8 (A1:831) | storage by role (§9.1) |
| §2.10 (A1:962) | header acceptance in `dash-chain`, owned by V in the node |
| §2.14 (A1:1213) | `dash-p2p` core + processors |
| §2.16 (A1:1419) | one BIP158 implementation (`dash-filters`) |
| §2.23 (A1:1592) | rewritten as §12.2 |
| §2.24 (A1:1610) | the isolation rule holds until stage C |
| §3.1 (A1:1685) | shared crates are host-driven (§7) |
| §3.8 (A1:1960) | jemalloc only in `rdashd`'s `main.rs` |
| §3.9 (A1:1980) | SPV rows: L7 size, `spv-*` peak RSS, startup, header and filter sync |
| §4 | dash-spv suites run against rdashd (serving) and as rdashd `-spv` |

---

## 13. Plan rows

Sizes: S = 1, M = 1.5, L = 3 weeks. Tiers per DEC-92. "§" refers to this document.

### 13.1 New rows (PLAN §4 format)

| ID | Task | MS | Crates/repo | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| SB1 | **Layer, purity and mobile gates** (L1–L9 tooling): `layers.toml` + `check-layers.py` (two feature graphs for `allow_w`), `shared-clippy.toml` + `check-shared.sh`, `mobile-deny.toml` + `check-mobile.sh`, size gate with `contrib/mobile/android-env.sh`; the ratcheted `layers-baseline.toml` (§2.3); removes R2's enforcement; CI groups | M1 | `contrib/node`, `contrib/mobile`, CI | — | S | OTH | — (SB0 §2.3) | **impl S-h; rev Sol** | DEC-81; NFR-CMP-03 | every rule green on integration against the baseline list; each planted failure of §2.3 fails its rule (direct and transitive normal/build N→V, V→N, V→Pn; an unlisted wrapper crate; hot-path W reachability; `thread::spawn` or `spawn_blocking` in `dash-chain`; +300 KiB); the W→W chain passes; a stale baseline entry fails · BG: L7 baseline recorded |
| SB2 | **Mobile release profile**: `[profile.mobile]` (fat LTO, 1 CGU, `opt-level = "s"`; `opt-level = 3` on the linked hashing paths: `dash-pow` until BS15's switch, `dash-x11`, `dash-sha256`, `dashcore_hashes`, `blst`, and the C backends' `-O3`); L7 baseline re-recorded | M1 | workspace `Cargo.toml`, `dash-spv-ffi`, `key-wallet-ffi` | SB1 | S | OTH | — | **impl S-h; rev Sol** | NFR-CMP-03 | builds for aarch64-android; the amended profile's stripped size is measured and recorded (target ≤ 9.0 MB) · BG: `spv-headers-*` instruction counts (QEMU aarch64) not higher than the release profile |
| SB3 | **dash-spv hygiene**: `dash-spv-cli` (clap out of the library), `log-file` feature, `env-filter` optional, `tokio` features trimmed; closes the L6 entries of `layers-baseline.toml` | M1–M2 | dash-spv, dash-spv-ffi | SB1 | S | OTH | — | **impl S-h; rev Sol** | NFR-CMP-01, -03 | L6 green for dash-spv-ffi defaults with no L6 baseline entry left; `spv`/`ffi` groups and dashd suites green · BG: L7 delta reported |
| SB4 | **`dash-chain` core** (CH1's shared half): arena, concurrent index, `accept` with every Core header rule incl. CL conflict and the v24 `NodeClock` time check, best header with Core's update rules and V's hooks, SPV's fork-choice target (§4.1, DEC-112 O1), `ChainReadSnapshot` R1–R8 + `tip_seq`, `IndexAccess`, `AnchoredAncestry` + the rule-sized `RuleContext` (§4.1.1), `BlockIndexRecord` codec, `take_dirty`, `load` with PoW check, anchor (absolute work, shipped pack, post-DGW checkpoints, candidate rule and the fallback candidate, §4.3.1 steps 1–2), `CheckpointView` (the node's is Core's chainparams list, n2) | M2 | dash-chain | CS0, i:CS2, i:CS3, i:BS5 | L | STATE | **D** (SB4 brief from SB0 §4) | **D O-h; impl O-h; rev Sol→O-h** | FR-CHS-01, -02, -06; FR-CON-17, -18 | Core L0 header vectors reject with Core's strings from genesis and from an anchor; `time-too-new` with ±3,600 s peer offsets and `setmocktime` (§4.2); full mainnet/testnet headers from oracle files, from genesis, windowed and unwindowed: chain work and `getchaintips` equal; **genesis-origin contexts (§4.1.1, DEC-105 F2)**: the BTC retarget at next height 576 (ancestor 0), the KGW boundary (a read beyond 25 entries; then the KGW → DGW switch at 34,140) and testnet's min-difficulty walk-back, each unwindowed, windowed at w = 10,000 and windowed at a test w below the KGW reach (cold loads), with equal verdicts; **the maximum-length KGW walk (§4.1.1, DEC-107 M1)**: parent 19,999 at uniform 150 s spacing (times 1,000,000 + 150·h, bits `0x1e010000`), 4,032 contributing entries down to 15,968 and the exit guard's read of 15,967, with the guard read resident and, at a test w, cold; the target equals complete ancestry's, and a 4,032-entry context panics in debug (negative control); the post-DGW 25-entry anchor control; a branch forking below the anchor (`BelowAnchor`); the candidate cases of §4.3.1, and no API moves an `Established` anchor; R1–R8 sim tests (P2a §2.5.3); seqlock coherence stress; **the best-header vectors (§4.1, DEC-112 O1)**, each comparing `getblockchaininfo.headers` and the first `getheaders` locator entry with the node oracle: a ChainLock on the active a1000 while header-only b1005 has more work (Core keeps b1005), and a higher-work header stored with `CONFLICT` followed by a restart (the load recompute picks it); the `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound` walk-backs; a first-seen tie; SPV's `fork_target` in the same cases (a1000 after the lock, never a failed or conflicting header); **fork-target eligibility (§4.1, DEC-115 R6-M1)**: Sol r6's three branches from height 80 (A100, B110 active, C105 stale) with a lock on A100 give A100, never C105; a lock on the already-active block, with nothing marked, keeps a heavier branch forking below it from becoming the target; a property test that every published SPV tip descends from `cl_floor`; the node's checkpoint rule over Core's list (n2) · BG: PF0 `chain_accept_headers` ≤ Core's `ProcessNewBlockHeaders`; node test: a ChainLock on a header-only block, and an enforcement reorg, leave the node tip trailing the floor with no `ChainError`, matched against the oracle (R7-O1) |
| SB5 | **`dash-chain` feeds and residency**: `TipCell` (F1–F3, F4 producer part, F5 + producer wakes, F6, F8 producer part, F10 with inactive registration, F11 with generational ids), the `SubscriberFeed` reader with staging, `FeedSource::subscribe_at`, `Needs`/`Refused` (P8), `ChainEvent` and a separate `StatusEvent` feed, resident window, cold records with stable ids and skip ids, snapshot heights by ancestry from the snapshot's own tip (§4.4), `HeaderStore` | M2–M3 | dash-chain | SB4 | M | STATE | (SB4 D) | **impl O-h; rev Sol→O-h** | FR-CHS-02; DEC-81 | CH2 §4.4's feed properties (X17–X24 analogues) plus §6.1's ports: X28, F11 with pending fanout and slot reuse, compression within capacity, space and deregistration wakes; `subscribe_at` linearization stress (T35 analogue); windowed ≡ unwindowed over §4.4's scenarios (stale branches below the floor, restart, lagging readers, >10,000-block reorg without a CL, cold-read error); **a lagging reader holding a snapshot across a reorg deeper than w (DEC-105 F3)**: its resident and cold heights all lie on the snapshot tip's branch while live queries answer the new one; **eviction after commit (m2)**: with w below a batch, no entry leaves the window before its record commits, and no cold read trips the latch on a healthy store; **R6-M1 across restart and cold records**: the three-branch case reopened after the lock, and again with a w below the branches' depth so that the eligibility walk reads cold records, gives A100 · BG: PF0 `feed_publish` |
| SB6 | **`dash-p2p` core** (replaces P2a's network half): connection table, per-peer tasks and queues, unconditional `push` with Core's retained-memory charge and pause state (§3.1), P1, handshake functions, gates, decode policy, ping, addr intake, AddrMan, BanList, sans-io dial steps, seeds through `Resolver`, `NetStore`, `inbound` | M2 | dash-p2p | P1, O17 | L | P2P | — (P2a §2.2–2.3, §3.2–3.9; SB0 §3) | **impl O-h; rev Sol→O-h** | FR-P2P-03…07 | `diff_addrman`; handshake transcripts through a minimal driver; §3.1's send tests (a handler crossing `-maxsendbuffer`, concurrent overlay pushes, partial V1/V2 writes at Core's byte count); sim at 125 peers; gate and decoder fuzz; builds for aarch64-android under L5/L6 · BG: — |
| SB7 | **Shared SML core** in `dash-evo` (EV4-core): SML entry and hash, roots, CbTx partial-merkle check, coinbase-CL predicate; `dash/src/sml` verification moved, with dashcore adapter functions at the old signatures (§5.5, DEC-112 O2) | M2–M3 | dash-evo, dash | EV0 | M | CON | — (EV4 row; SB0 §5) | **impl O-h; rev Sol→O-h** | FR-EVO-07; FR-CON-29 | L0 `evo_simplifiedmns_tests`; CbTx MN root equal over all history; dashcore's existing sml tests through the adapters · BG: crit incremental vs full SML root |
| SB8 | **Shared lock and commitment verification**: `dash-evo::locks` (request id, sign hash, signer selection over the full authenticated `QuorumScanView` with each entry's status, then the `Verified` check on the selected commitment only (§5.2 step 6), BS12 serial BLS); Core's `CFinalCommitment::Verify` with signatures; `impl MnListView for SmlList` as SPV's `membersSig` input (§5.1) | M3 | dash-evo | SB7, LQ1a, BS12 | M | LLMQ | — (LQ1 §2.2.4; SB0 §5) | **impl O-h; rev Sol→O-h** | FR-CL-01; FR-LLMQ-02 | L0 CL/IS vectors; commitment verification vectors; LQ1 fixtures under both views; the §5.1 mutation gate: a changed signing-key multiset → `Unverified`; **a signer permutation** and a non-signer-only change → verdict unchanged, and no member list exposed (API test) (DEC-105 f1); missing-property (collateral, owner key) and rotation (wrong snapshot row) cases; **the §5.2 step 6 selection gate (DEC-105 F4)**: non-rotated, an `Unverified` Core winner → unavailable (never the best-scoring `Verified` one), and an `Unverified` non-winner → the winner verifies as in Core; rotated, the newest commitment at the signer's index `Unverified` with an older `Verified` one at that index → unavailable; locks verify only against the selected commitment's key; **Sol r3's case (§5.2.1, DEC-107 M4)**: the sets at 2,500,147 and 2,500,148 around a CL-type mine at 2,500,148, with a constructed scoring winner that the mine replaces, select as Core does on each set · BG: — |
| SB9 | **`dash-filters`**: GCS codec, Dash element set, filter-header chain check; `dash/src/bip158.rs` moved, with dashcore adapter functions at the old signatures (O2; CV5's filter half) | M3 | dash-filters, dash | CS0 | M | P2P | — | **impl S-h; rev Sol** | FR-IDX-03; US-LCS-01 | L0 `blockfilters.json`; oracle filters for ProTx/asset-lock blocks; dashcore's bip158 tests through the adapters · BG: — |
| SB10 | **`dash-kv` split + `dash-kv-sqlite`**: move the contract and latch types; SQLite engine per §9.4 (pinned snapshots, read pool, one writer); SPV-store `Sut` in ST3a; `sqlite_wal_prefix.rs` | M3 | dash-kv, dash-kv-sqlite, dashd-store | ST3a, CH2a | M | STATE | — (SB0 §9.4) | **impl O-h; rev Sol→O-h** | FR-STO-03; NFR-CMP-03 | the engine contract suite on both engines, incl. §9.4's snapshot interleaving and its negative control; ST3a explore/sweep on the SQLite `Sut`; the prefix regression; `dashd-store` tests unchanged after the move · BG: — |
| SB11 | **`dash-mnlist`**: engine per §5.3, trust chain steps 1–7 with `Skipped` types (by configured use), L_est from `LlmqConfig`, `coverage` of the tip's and the horizon's oldest authenticated sets at initial sync and after each new commitment, with the later-miss diagnostic (§4.3.1), the recent-lock horizon H_keep with retained and fetched sets, clipped to its authenticated domain at genesis and A_q (§5.2.1), retention of lists and quorum rows with `lookup_quorum`, W_P, the two-target fetch for the served types and the lifetime invariant with its params test and config refusal (§5.3, DEC-112 O3, DEC-115, DEC-116), the canonical scan and select-then-`Verified` (§5.2 step 6), ancestry through `AnchoredAncestry`, `take_dirty` rows, shadow mode API, `LlmqConfig` per client, with LQ1's `LlmqCtx` built per call | M4 | dash-mnlist | SB5, SB8, SB10, LQ1b | L | LLMQ | — (SB0 §5) | **impl O-h; rev Sol→O-h** | FR-LLMQ-01, -02, -05; DEC-81 | the 11 rotated captures; regtest-mn and testnet captures through steps 1–7; a mutated diff/qrinfo/CL is rejected at its step; a below-floor read is `Unavailable`; **§4.3.1's coverage cases (DEC-105 F1)**: repeated null rounds of one rotated index (Sol r2's witness), a very old surviving index with an unknown base, non-rotated A successes with null gaps, each against its regular-DKG control; a `Skipped` type's history not counted; a later miss (a reorg that restores an older commitment): `Unavailable` and the diagnostic, with the anchor and every row unchanged; **the H_keep gates (§5.2.1, DEC-107 M4)**: Sol r3's 2,500,148 / 2,500,155 case from a retained set and after one fetch, a delayed CL immediately before and after a mine, an ISLock across a cycle boundary, a fresh start on the new side of each boundary, outside the horizon unavailable with no request, a failed fetch unavailable, one fetch for many locks at the same block; **bootstrap from genesis (Sol r4 R4-M1)**, with regtest and devnet parameters: tips 0, 7, 8 and 20 with no request, A_q + 7 / A_q + 8, tip 450 requesting 432 and 442 only, A_q + H_keep ± 1, the first commitment mined, a DIP8-after-DIP3 regtest gap, and devnet's first rotated cycles; **retention and Platform's lookup (§5.3, DEC-112 O3)**: a list dropped once nothing needs it and the tip list kept; quorum rows pruned at tip − R inside `dash-mnlist`'s `WriteSet`, never by clearing a table; **after pruning and a restart (R6-M3)**, for a served non-rotated commitment replaced at r with every verification input available: h = r + W_P − 1 fetches the diffs to h − W_P and h and is found, and h = r + W_P is not found; the same answers from a row before pruning and from the LRU; two lookups sharing a target fetch it once; a lower target below the anchor; a non-served type after pruning returns not-found with no request (DEC-116); the params test over every network's served type in the minimum-lifetime form, a devnet configuration that breaks it refused at load, and stock regtest and devnet loading; a failed fetch and h above the tip return `Unavailable` (DEC-444); a need below the anchor follows SB11 D50; no lookup ever returns another quorum's key · BG: — |
| SB12a | **`dash-spv::bridge`** (§5.5, DEC-112 O2): the dashcore ↔ base-sdk and `dash-consensus` conversions for headers, blocks, cfheaders, cfilters, mnlistdiff, qrinfo, CLs and ISLocks; `Header::from_parts_hashed` *(Superseded 10-16 by SB12a D17 / DEC-224 as amended: `from_parts_hashed` was removed; `Header::new` is the only constructor, the bridge hashes each received header inside `dash-consensus` once, and dash-spv takes the header and hash back.)*, so each header is hashed once (debug-asserted; DEC-51a note); the dashcore shapes behind Platform's queries (`latest_masternode_list`, `get_quorum_at_height`) | M3 | dash-spv, dash-consensus | CS0, SB7, SB9 | S | CON | — (SB0 §5.5) | **impl O-h; rev Sol→O-h** | NFR-CMP-01; DEC-81 | round trips over captured mainnet and testnet messages of each kind, byte-equal; no header hashed twice (a counting X11 wrapper over a 2,000-header batch and a `headers2` batch); a mismatched hash fails the debug assertion; Platform's two queries return the same values as before over the same captures · BG: `spv-headers-*` not slower |
| SB12 | **dash-spv kernel rules on existing paths**: header, block and filter rules from `dash-consensus`/`dash-filters`, through `dash-spv::bridge` (SB12a); anchor windows in checkpoint data. Scope per PLAN §7 handling (CANDIDATE-SPV) | M3 | dash-spv | SB4, SB9, SB12a | M | CON | — | **impl O-h; review routing per PLAN §7 handling** | FR-CHS-02, -06; DEC-81 | Core rule vectors through dash-spv's headers path; dashd suites · BG: `spv-headers-*` not slower |
| SB13 | **dash-spv SQLite store + migrator v3** (§9.5): schema with full header records and the `active` projection, the single store writer and `WriteSet` assembly, with no wallet locator (§9.5, DEC-112 O4); the thread sources of §7 (DEC-112 O5): validation and wallet matching under `pool.install`, blst `no-threads` in dash-spv-ffi and its `check-mobile.sh` check, the FFI runtime's explicit `worker_threads` and `max_blocking_threads`, `spv-clippy.toml` | M4 | dash-spv, dash-spv-ffi, key-wallet-manager, `contrib/mobile` | SB10 | M | STATE | — | **impl O-h; rev Sol→O-h** | FR-STO-03; NFR-CMP-01 | migration of v2 stores (mainnet-sized fixture) with flat memory; crash points via the SPV `Sut` (between a header step and a subscriber commit), and in each database independently with a faulty wallet store (O4); the store half of R6-M2's witness: after power loss the durable floor and tip are A70 and A100, and no B record survives; the thread-count test (§7) over the old MN path, with dashcore's verifier and blst `no-threads` (O5); a planted `no-threads` in an N package's graph fails `check-mobile.sh`; dashd suites · BG: `spv-startup` |
| SB14 | **dash-spv on `dash-chain`**: headers manager as driver, managers as feed subscribers behind a SyncEvent adapter, reorgs without restart, `handle_fork` removed; the wallet's resume point and synced-hash check, with the additive `WalletInterface` methods in key-wallet-manager (§9.5, DEC-112 O4) | M4 | dash-spv, key-wallet-manager | SB5, SB12, SB13, #1104 merged or carried (m7) | L | STATE | — | **impl O-h; rev Sol→O-h** | FR-CHS-02; DEC-81 | fork tests without restart; `dashd_sync` restart/disconnect suites; FFI `dashd_sync`; **wallet progress (§9.5, O4)**: the wallet's synced hash on the active chain (nothing to do), on a stale branch (one `truncate_above` at the fork) and unknown after a lost Buffered suffix; **the Buffered-reorg witness (§9.5, DEC-115 R6-M2)**: durable A100 with the floor at A70, a Buffered reorg at 80 published as B110, the wallet durably at B110, power loss: `truncate_above(70)`, never 100, and with no durable floor `truncate_above(anchor)`; next to it, the straight-extension control (A101…A110 lost, the wallet at A110) takes the same rule, and its rescanned wallet equals one synced straight; a `WalletInterface` without the hash method logs once; crash points in each database independently; the thread-count test (§7) · BG: `spv-headers-*`, `spv-filters-*` not slower |
| SB15 | **dash-spv MN on `dash-mnlist`**: sync managers as adapters; persisted verified state; the initial-sync driver (headers-only `Candidate`, coverage, fallback, `Established`; §4.3.1 step 2, §9.5) and the horizon fetch path (§5.2.1); Platform's queries through the bridge with §5.3's lookup (DEC-112 O3); MN sync off (m3); dashcore's `quorum_validation` and `message_verification` features dropped (O5); LQ1b-F's in-crate test | M4 | dash-spv | SB11, SB14 | M | LLMQ | — | **impl O-h; rev Sol→O-h** | FR-LLMQ-02; DEC-81 | `dashd_masternode` suite; replay-free restart; the 11 rotated captures; anchor selection end to end on regtest and devnet (§4.3.1); **initial-sync fallback end to end (DEC-105 F1, DEC-107)**: forced DKG failures at startup, with no MN, lock or subscriber row written and no lock verified before coverage holds; a crash in `Candidate` before and after a fallback reopens with no such state and completes; **a later miss**: the reorg that restores a commitment below the anchor leaves the anchor, the lock rows and every subscriber locator unchanged, reports its locks unavailable and raises the diagnostic (log line and FFI event); §5.2.1's delayed-lock cases against regtest dashd; **regtest from genesis (Sol r4 R4-M1)**: `Established` at tip 20 with no MN request and at tip 450 with requests to 432 and 442 only, subscribers running before any quorum, `NoQuorum` until the first commitment and verification after it, and the domain sliding once the chain passes A_q + H_keep; **Platform's lookup end to end (§5.3, DEC-112 O3)**, for a quorum rotated out about a day earlier (≈ 576 blocks on mainnet parameters, beyond R) and after a restart: with network, the two-target fetch and found; on regtest's Platform type after pruning and a restart, h = r + W_P − 1 found and h = r + W_P not found (R6-M3); with no network, `Unavailable` (DEC-444) and never another quorum's key; a lookup within W_P of a replacement answered from the rows with no network; **MN sync off (m3)**: `Established` at the candidate with CL and IS `NoQuorum`, and `start_from_height` used as the birth height above the sync floor; the thread-count test on the `dash-mnlist` path (§7) · BG: — |
| SB16 | **`SpvRouter` on `dash-p2p`** behind #902's facade, **extended** (§3.2.1; CV5's network half): `RequestLedger` with `RequestId`/`AttemptId`, bounded subscriptions, `cancel`, terminal `RequestFailed`, misbehaviour → discourage, AddrMan/bans in the store, `SpvDialPolicy`, L9 registry; `legacy-net` for one release | M4–M5 | dash-spv | SB6, SB14, #902 merged or Q1 | L | P2P | — (SB0 §3) | **impl O-h; rev Sol→O-h** | FR-P2P-05, -07; NFR-CMP-04 | dashd suites on both `N`; #902's tests migrated per §3.2.1; stalled subscriber, multi-topic fanout, retries then terminal failure, late responses after `cancel`; SPV handshake transcripts; peer-churn sim; the thread-count test (§7) · BG: `spv-*` cells not slower |
| SB17 | **FFI event translation + mobile cells**: `events.rs`; `spv-filters-*`, `spv-startup`, `spv-size`; emulator idle-RSS smoke (`agentbox-api36`); the measurements §8 labels pending; Mac iOS build and durability check in QA | M5 | dash-spv-ffi, node-bench | SB14, SB15, SB16 | M | OTH | — | **impl S-h; rev Sol** | NFR-CMP-01; REL-5.7 | `ffi-c-tests` header compile; a baseline-header C caller against the new library (§10.1); callback ABI unchanged; emulator smoke and thread count (§7); idle and synced RSS within §8.2's bounds under §5.3's retention (O3); the real-device SQLite write-rate cell, under Q6 and run by pasta (m6) · BG: the new cells recorded |
| SB18 | **rdashd `-spv`**: composition in `rdashd` with key-wallet (`WalletInterface` via key-wallet-manager; Q4, DEC-112 O6), RPC subset, P8 InitErrors, `spv` bench cell; full mode never opens `spv/` (no import, §4.7) | M5 | rdashd, dashd-node | SB15, SB16, ND1 | M | STATE | — | **impl O-m; rev Sol→O-h** | DEC-81; US-LCS-07 | dashd suites as rdashd `-spv` (O16); `-spv -txindex` refused; a full start over an `-spv` datadir syncs headers from genesis with no open under `spv/` · BG: rdashd `-spv` ≤ dash-spv binary on the same wallet and peers |
| SB18b | **`dashd-wallet` for `-spv`** (DEC-112 O6): an adapter in `rdashd` that puts `dashd-wallet` behind `WalletInterface` within L3's `allow_w`, replacing SB18's key-wallet for RPC parity | M6 | rdashd | SB18, WL2a | S | STATE | — | **impl O-m; rev Sol→O-h** | DEC-81; US-LCS-07 | SB18's suites as rdashd `-spv` with `dashd-wallet`; the wallet RPCs in WL2a's scope against the oracle · BG: — |
| SB19 | **mnxcheck node wiring**: shadow engine task, `getmnlistd`/`getqrinfo` cadence, comparison with `project_sml`, metric | M4–M5 | dashd-quorums or dashd-node | SB11, EV4-serve | S | LLMQ | — | **impl O-m; rev Sol** | FR-EVO-07/08 | on by default in regtest/devnet/testnet CI; a planted projection bug is reported · BG: — |
| SB-ST | **Contingent** (only if SQLite is refused): segment crash model, persisted hash index, step marker (§9.6) | M3 | dash-spv | ST3a | S | STATE | — | **impl S-h; rev Sol→O-h** | FR-STO-03 | ST3a explore/sweep on the segment `Sut` · BG: — |

### 13.2 Changed rows

| Row | Change |
|---|---|
| **CV5** | Superseded by SB9 + SB16 (product: SB18) |
| CV3 | Its network and LLMQ wrappers start from `dash-spv::bridge` (SB12a), which exists from M3. Moving dashcore's types onto base-sdk shrinks the bridge's conversions toward the identity (DEC-112 O2). Size unchanged |
| CV4 | Its "dash-spv P2P on `P2pMsg`" part is **not** subsumed by SB16: SB16's facade still names `NetworkMessage` (§3.2.1). CV4 makes the bridge the identity, as its row says, and deletes `dash-spv::bridge`; the hash-newtype work stays (DEC-112 O2) |
| **P2a** | Becomes **P2a-node** (the node processor in `dashd-p2p`). Size L → M, design D2 kept. Deps `P1, O17, i:SB6, i:CH1`: it codes against SB6's stubbed `NetCore` API and merges after SB6. Unhold once that API is stubbed |
| P2b | Uses SB6's `inbound`; deps + SB6 |
| **CH1** | Becomes **CH1-node** (`dashd-chainstate`). Size M, design D kept. Deps `ST1, SB4, i:SB5`: it codes against SB5's stubbed `TipCell` API and merges after SB5 |
| CH2 (CH2b after DEC-103) | `i:SB5`; unhold b1/b2 against SB5 stubs; latch types from `dash-kv` |
| EV0 | Gains the split, generic `QuorumMembers<E>` and store-independent view errors (§12.1); **unhold on the EV addendum (fa09d8d0, DEC-109) at SB0 approval** |
| EV2a/b | Crate `dash-dml`; EV2a adds `impl MnListView for MnList`; unhold with EV0 |
| EV4 | Becomes **EV4-serve** (M4): deps EV2b, BS8, SB7; adds `MnList::project_sml` (DEC-109 (2)) |
| LQ1a/b | Unhold; LQ1b-F → SB15. LQ1b now gates SB11 |
| LK1a | Verification from SB8 (deps + SB8) |
| IX1 | Builds with SB9 (deps + SB9) |
| ND1 | Host services (clock, `AbortSink`, `BlockingIo`, `Resolver`, shutdown order) + `-spv` InitErrors; no dash-spv dependency |
| O13a | Unchanged for the node (still held behind P2b); SPV transcripts are SB16's |
| O16 | Also runs the suites as rdashd `-spv` (with SB18) |
| BS15 | Needs an aarch64 measurement before the mobile default switches; `dash-pow` leaves SB2's opt-level list at the switch |
| P6b | Serves cfilters built with SB9 |
| M2 exit | Adds: SB1 green; SB4, SB5 and SB6 merged; `dash-chain` and `dash-p2p` build for aarch64-android within L5/L6 |
| M5 | The "SPV smoke" comes from SB18 + O16; `M5_exit` depends on SB18. SB18 uses key-wallet (Q4, DEC-112 O6), so M5 does not wait for WL2a; `dashd-wallet` for `-spv` is SB18b at M6 |

### 13.3 Order

The order follows the edges of §13.1 and §13.2. An edge `i:X` means the row codes against X's stubbed API and merges no earlier than X + 0.5 week.

- **Continue now, unchanged:**
  - CS series;
  - CH2a (keep the latch types RocksDB-free);
  - ST3a to merge;
  - O2a, O3, PF0;
  - P1;
  - BSX/BSX-PR;
  - desktop.
- **On approval:**
  - SB1 first (it guards the rest), then SB2 and SB3;
  - in parallel:
    - SB4: its design, then its implementation against CS2/CS3's signatures (`i:CS2`, `i:CS3`; merge after CS3);
    - SB6;
    - the EV addendum, which unholds EV0 → EV2a → LQ1a on the critical chain;
  - SB9 needs only CS0.
- **Then:**
  - SB5 after SB4;
  - CH1-node after SB4, merging after SB5;
  - CH2b's unhold against SB5 stubs;
  - P2a-node after P1, merging after SB6; then P2b after P2a-node and SB6;
  - SB7 after EV0, then SB8 after SB7, LQ1a and BS12;
  - SB10 when ST3a and CH2a merge;
  - SB12a after SB7 and SB9, then SB12 after SB4, SB9 and SB12a.
- **M4:**
  - SB13 after SB10;
  - SB11 after SB5, SB8, SB10 and **LQ1b**;
  - SB14 after SB5, SB12 and SB13, with #1104 merged or carried (m7) and Q1 answered (m8);
  - SB15 after SB11 and SB14;
  - SB19 after SB11 and EV4-serve;
  - SB16 after SB6 and SB14, plus #902 or Q1.
- **M5:** SB17 after SB14, **SB15** and **SB16**; SB18 after **SB15**, SB16 and ND1.
- **M6:** SB18b after SB18 and WL2a.

**Reviews (DEC-92):** Sol, then Opus high, in sequence, for the STATE/CON/LLMQ rows and the P2P rows tagged critical (SB6, SB16). Sol only for SB1–SB3, SB9, SB17 and SB19. SB12's routing follows PLAN §7 handling.

### 13.4 Milestone effect (CPM rerun, DEC-104 M9)

**Method.**
- `docs/node-program/tools/cpm_sb0_rev1.py` overlays §13.1/§13.2's rows and edges on the committed `cpm.py` (76ad33ea), which is unchanged, and prints three runs:
  - **A** applies only SB0 rev1's rows to the committed table, as Sol r1's sensitivity check did;
  - **B** also applies DEC-103's CH2 → CH2a/b/c split, with CH2's consumers on CH2b and SB10 on CH2a;
  - **C** is A with SB4's CS2/CS3 and CH1-node's SB5 as merge edges, i.e. no coding against stubs.
- The model's conventions hold:
  - S/M/L = 1/1.5/3 weeks;
  - review +1 week (+0.5 for OTH);
  - design steps from week 1, which assumes SB0 is approved by then, as for every design step in the model;
  - #902/Q1 is external and not modelled.
- rev2 (DEC-105) changes no row size or edge, so the overlay and its numbers stand.
- rev3 (DEC-107) changes no row size or edge either, so the numbers are unchanged: M2 18.5, REL11 30.0.
  - Removing the runtime re-anchor drops SB15's rebuild state machine. That is the work Sol r3's stress run sized by growing SB15 from M to L. SB15 stays M.
  - SB11 stays L: the H_keep horizon (§5.2.1) takes the place of the re-anchor requests.
  - No slack is freed in the model, since its sizes were never raised. What rev3 removes is the risk behind the stress run.
- rev5 (DEC-112) adds one row, **SB12a** (S, CON; deps CS0, SB7, SB9), and SB12's edge to it. The overlay is `/work/scratch/sb0-reconcile/rev5/cpm_sb0_rev5.py`: rev1's, plus SB12a and a stress run. Every other row keeps its size.
  - SB12a finishes at 11.5 in every run. SB12 moves from 11.0 to 14.0 in runs A and B, and from 13.5 to 14.0 in run C.
  - SB14 does not move: 20.5 in A (limited by SB13), 18.0 in B (SB12 and SB13 now tie) and 23.0 in C.
  - **Each run's milestone dates are unchanged** (R6-m1). M2 / REL11 / M5 are **18.5 / 30.0 / 41.5** in runs A and B and in the stress run, and **21.5 / 31.0 / 42.5** in run C. The table's other cells are unchanged.
  - The O3, O4 and O5 work lands in SB11, SB13, SB14 and SB15 at their current sizes. A stress run with SB13 and SB15 at L moves SB14 to 22.0 and ends the SPV rows by 28.5 (SB15 26.0, SB16 26.0, SB17 28.0, SB18 28.5). M2, REL11 and M5 do not move.
  - SB18b (M6) and #1104 are outside the model: WL2a is not in it, and #1104 is external, like #902.

| Finish (week) | Baseline | **A (rev1)** | B (+ CH2 split) | C (merge edges) |
|---|---|---|---|---|
| **M2 exit** (max of CH1-node, P2a-node, P2b, O13a, ND1, SB1, SB4, SB5, SB6) | 18.0 | **18.5** | 18.5 | 21.5 |
| SB4 / SB5 | — | 8.5 / 11.0 | 8.5 / 11.0 | 11.0 / 13.5 |
| SB12a / SB12 (rev5; rev1's SB12: 11.0, 11.0, 13.5) | — | 11.5 / 14.0 | 11.5 / 14.0 | 11.5 / 14.0 |
| CH1 → CH1-node | 8.5 | 11.5 | 11.5 | 16.0 |
| SB6 / P2a → P2a-node | — / 13.0 | 13.0 / 13.5 | 13.0 / 13.5 | 13.0 / 16.5 |
| P2b / O13a | 15.5 / 18.0 | 16.0 / 18.5 | 16.0 / 18.5 | 19.0 / 21.5 |
| ND1 | 11.0 | 14.0 | 14.0 | 18.5 |
| CH3b | 19.0 | 20.0 | 21.0 | 24.5 |
| CH7b | 28.0 | 28.0 | 28.0 | 29.0 |
| **REL11** | 30.0 | **30.0** | 30.0 | 31.0 |
| SB11 / SB14 / SB16 | — | 21.5 / 20.5 / 24.5 | 21.5 / 18.0 / 22.0 | 21.5 / 23.0 / 27.0 |
| SB17 / SB18 | — | 26.5 / 27.0 | 26.0 / 26.5 | 29.0 / 29.5 |
| M4_soak / M5_exit / RL4 | 34.5 / 41.5 / 49.3 | 34.5 / 41.5 / 49.3 | 34.5 / 41.5 / 49.3 | 35.5 / 42.5 / 50.3 |

**Reading run A.** Two quantities are kept apart below (DEC-105 f2):
- **merge slack** is how much later a row could merge without moving M2's exit;
- **coding float** is how much earlier a row's coding completes than the earliest merge its interface edges allow.

The numbers:
- **M2 exit** moves **+0.5 week**, still limited by O13a. Its chain is P1 (9.0) → **SB6** (13.0) → P2a-node (13.5) → P2b (16.0) → O13a (18.5).
  - SB6 and P2a-node both have **zero merge slack**.
  - P2a-node's coding completes 2.0 weeks before its SB6 merge constraint. That is coding float, not slack.
- The header split is **not** M2's limit. Merge slack against M2:
  - SB4 and SB5: 1.5 weeks each;
  - CH1-node: 1.5. P2a-node merges at 13.5 and needs CH1-node + 0.5, so CH1-node's latest merge is 13.0, against its 11.5.
  - CH1-node's coding float is 0.5: coding completes at 11.0, and its earliest merge is 11.5, after SB5 + 0.5. rev2 quoted 2.0 here, which is CH1-node's coding slack against its latest M2-safe merge (13.0), not its coding float (DEC-107 m1).

  That holds only because CH1-node codes against SB5's stubs and SB4 against CS2/CS3's signatures. Run C shows the cost without that overlap: M2 at 21.5 and REL11 at 31.0, the latter limited by the header chain through CH3b → CH7a.
- **REL11** stays at 30.0, limited by the EV chain (BS3 → … → EV0 → EV2a → LQ1a → LQ1b → LQ2 → LQ3 → CH7b). EV0 has 0.5 week of slack, and EV2a and LQ1a have 0, so **EV0/EV2a/LQ1a must unhold at SB0 approval**.
- SB7 has 8.0 weeks of slack and SB8 16.0. Neither is on that chain: they depend on EV0's types, not the reverse.
- The SPV product rows (SB11–SB18) end by week 27.0 at the stated sizes, with slack against M5 (41.5). rev5's SB12a leaves that unchanged. Under Sol r3's stress run (SB11 + 3.0 weeks, SB15 → L) they end by 31.0 (SB11 24.5, SB15 28.5, SB18 31.0), still with slack against M5; REL11 and M2 do not move.
- **Sol r1's sensitivity** (CH1 15.5, ND1 18.0, P2b 18.0, REL11 30.5) used rev0's edges with merge dependencies. Run C is its rev1 analogue, which adds CS3 → SB4 and the other missing edges.

---

## 14. Reconciliation log

### 14.1 Divergences

| # | Divergence | Fable | Opus | Taken | Why |
|---|---|---|---|---|---|
| 1 | Mobile/SPV storage | keep segment files; RocksDB for rdashd `--spv` | SQLite under `dash-kv` | **Opus** | Size: 0 on iOS, in Platform's Android SDK and on desktop; +1.1–1.8 MB standalone. Memory: +3.8 MiB, with no whole-chain hash index. Atomic multi-table steps, prefix recovery, fullfsync, O(window) open. Fable's same-DB argument falls away: full mode re-syncs headers from genesis and never opens the SPV store (§4.7, DEC-104 M5; rev0's import is withdrawn), so P10 holds trivially. Fable's crash model is kept as the fallback |
| 2 | rdashd `--spv` store | `dashd-store` (RocksDB) | SQLite | **Opus** | One SPV code path for mobile and `-spv` makes DEC-81 testable on Linux CI, and the SPV schema stays out of `CHAINSTATE_SCHEMA` |
| 3 | Network shape | one loop with `Profile::{Full, Light}`, SPV hooks inside Core's actor | network core + two processors | **Opus** | No SPV hooks in the node's Core-exact scheduler, no OS threads in shared crates, #902's broker intact, the node independent of #902. Fable's `PeerSelector` and `LightOptions` are kept as SPV policy. The DEC-81 reading is Q3 |
| 4 | Network crate name | `dash-p2p` | `dash-net` | **Fable's name, Opus's scope** | `dash-net` is easily confused with the existing `dash-network` (the Network enum crate); `dash-p2p`/`dashd-p2p` mirrors Core's net/net_processing split |
| 5 | Header store | `Residency::Windowed` + #1095's in-memory hash index; `ChainStore` trait | window + indexed SQLite `HeaderStore` | **Opus** | No resident whole-chain index (≈ 10–13 MiB); O(window) open. rev1 adds full branch records, stable ids and the `active` projection (§4.4, DEC-104 M4) |
| 6 | Anchor chain work | absolute | relative | **Fable** | Min chain work and RPC need absolute work, and the data exists |
| 7 | Anchor window | relaxed rule for 24 headers | fetch 24 below with a locator 25 back | **neither** | Fable's rule is unpinned at the newest checkpoint. Opus's fetch is impossible with 50,000-block spacing. The 25 headers ship with each checkpoint (§4.3; post-DGW anchors only, confirmed by Sol r1). The pack does not cover the MN pipeline's history; coverage of the actually active quorums' history chooses the checkpoint (§4.3.1, DEC-105 F1). A genesis origin gets a rule-sized context (§4.1.1, F2) |
| 8 | MN pipeline in the node | `dash-mnlist` with a `dml` binding (`BlockSource`) | `MnState` with a projection producer | **neither, synthesis** | Node state stays in node crates and calls the shared functions; `dash-mnlist` is SPV-only plus shadow. This avoids a second state holder on V and keeps `dash-dml` out of S |
| 9 | Cross-check | `-consensusxcheck` SML equality | mnxcheck shadow | **Opus** | Concrete cadence, default-on outside mainnet, never feeds consensus |
| 10 | SML fields | "the SML has every field the scoring reads" | SML lacks collateral and owner key; fail-closed | **Opus on the fields; "fail-closed" withdrawn in rev1** | LQ1:247: `collateral()` is null; `try_add` only under-rejects. rev0 inferred from BLS success that membership is exact, which is wrong because `membersSig` covers signers only (Sol r1 M8). SPV trusts commitment-verified keys and claims no exact membership; exact membership is node-only, with mnxcheck as the shadow (§5.1, DEC-104 M8) |
| 11 | `dash-evo` split | one `dash-evo` with features | `dash-evo` + `dash-dml` | **Opus** | EV0 is unmerged; L6 can ban a crate |
| 12 | Engine facade | keep `MasternodeListEngine` public shape for FFI and desktop | replace | **Opus's design, Fable's name** | The engine is crate-private (`lib.rs:74`) and the FFI goes through `DashSpvClient`, so no public facade is needed. The name stays for maintainer continuity |
| 13 | R2 replacement | L1–L4 (layers, pure, mobile closure, hot-path types) + size gate | L1–L8 | **merged, L1–L9** | Opus's list, plus a V layer and Pn (fixing `dashd-node → dash-spv`), Fable's hot-path rule as L3, and Fable's mobile profile |
| 14 | Mobile LTO profile | `opt-level = "s"` | not proposed | **Fable, amended** | −3.74 MB is free, but `"s"` slows hashing; hashing crates keep `opt-level = 3` |
| 15 | Migration staging | 5 stages, #902 first (stage 0 waits) | A/B/C/D, #902 only for C | **Opus** | Stages 0–B and the node proceed without #902. Fable's `legacy-net` safety switch is kept for SB16, which extends #902's facade (§3.2.1, DEC-104 M7) |
| 16 | CV5 placement | SB4 = rdashd `--spv` at M2–M3 | SB7 + SB10 at M3–M4 | **Opus, plus SB18** | `-spv` needs the router and store first; M2 is already the busiest milestone |
| 17 | P1–P9 tables | F1–F9 | F1–F9 | **extended to F11, P10** | CH2 rev5 (DEC-103) added F10, F11 and P10 |
| 18 | `Control` channel | a `dash-chain` type | stays in CH2's hub | **Opus, with a generic slot** | `dash-chain` carries opaque `ControlMsg` out of band (F4); the types and mirror stay CH2's |
| 19 | Fail-stop latch | stays in `dashd-store` | moves to `dash-kv` | **Opus** | dash-spv needs the same latch semantics per client (P6, L5) |
| 20 | Kernel-rule stage review | not separated | "rev Sol" | **routing per PLAN §7 handling** | That stage's review routing is decided under PLAN §7 |

### 14.2 What neither leg caught

1. **F10, F11 and P10** (CH2 rev5). Both legs wrote against rev2's F1–F9. Neither `FeedHub` design had a tip lock, so `subscribe_at` could not be linearized and deregistration was unspecified. Fable's rdashd `--spv` on `dashd-store` would also invite the separate header store P10 warns about.
2. **`tip_seq` in `ChainSnapshot`** (CH2 F8/P7): absent from both snapshot types.
3. **The anchor window**: Fable's hole at the newest checkpoint, Opus's impossible fetch (§4.3).
4. **The `dash-net` / `dash-network` name clash.**
5. **Opus's layer contradiction**: N may not depend on A, but `dashd-node` builds dash-spv's client. Fixed by the V layer and composition in `rdashd`.
6. **The EV0 hold costs the critical path**: EV0 has 0.5 week of slack on BS3 → … → CH7b. Unhold on the EV addendum at approval.
7. **Review routing for the kernel-rule stage** belongs to PLAN §7 handling, not a default tier.
8. **blst's pool vs "no threads in shared crates"**: DEC-79 bans pooled calls only in pure crates, and Fable deferred it to CV3. L5 extends the bans to S, and BS12's serial verify provides the path.
9. **`opt-level = "s"` vs hashing speed.**
10. **NFR-CMP-04's #803, #799 and #1077**: overlapping in-flight work neither leg named (§10.4).
11. **iOS check feasibility**: Opus planned an iOS `cargo check` on Linux, which C build scripts likely prevent; cargo-deny's target graph needs no build.
12. **Baseline discrepancies**: 41 vs 43 FFI exports (two non-`dash_spv_ffi_` names) and 12.55 vs 12.62 MB (dev vs integration, plus probe overhead).
13. **The engine is crate-private**, so the public-facade constraint Fable designed around does not exist.
14. **CH2a's latch placement**: CH2a is implementing `GlobalFailStop` now; keeping it RocksDB-free makes SB10 a pure move.

---

## 15. Open questions

### 15.1 For pasta (product or external): decided and open (DEC-112)

DEC-112 records manager calls on Q3, Q4, Q5 and Q7, which pasta may override; they are reversible build and configuration choices. Q1, Q2 and Q6 stay with pasta, each with the deadline below. Q8 is new in rev5 (O3).

| # | Question | Decision, or recommendation and deadline |
|---|---|---|
| Q1 | **#902's path.** SB16 needs #902's facade and extends it (§3.2.1: bounded subscriptions, `cancel`, terminal `RequestFailed`, request vs attempt ids). Does #902 land first, with SB16 as its follow-up, or does SB16 carry the facade, credited to #902, if #902 stays open? Either way, the extension should be agreed with #902's author through pasta (DEC-104 M7). Needs a human contact; agents do not comment | **Open with pasta; needed before SB14, ≈ week 16** (m8), not only before SB16. Recommendation: land first; raise the extension with the author early; carry it once SB14 is done if still open. If #902 lands first, integration rebases onto it before SB14 (§10.3) |
| Q2 | **Upstream shape.** Stages A–C make dash-spv depend on node-program crates and on SQLite. Pre-clear that shape with the maintainers, and decide whether to offer stages early under a narrow DEC-87 relaxation. If SQLite is refused, the fallback is segments + SB-ST. The base-sdk fork's `[patch]` (`Cargo.toml:56-62`, `file://` URLs) reaches dash-spv's consumers once SB12 makes dash-spv depend on `dash-consensus`, so BSX must upstream the fork before stages A–C can upstream (n5) | **Open with pasta; needed by ≈ week 9–10, before SB10 and SB12 start.** Recommendation: pre-clear before SB12; offer stages 0–B early |
| Q3 | **DEC-81's "one network loop".** Is one network core with two processors (shared decisions, separate schedulers) the intended reading? | **Yes** (DEC-112, manager call): one shared network core with two processors satisfies DEC-81's "one network loop"; SB6 and P2a-node proceed on that basis (§3.3) |
| Q4 | **rdashd `-spv` scope.** Outbound only and no serving by default (`-listen=1` opt-in later)? Which wallet: Core-compatible `dashd-wallet` or key-wallet? | **No inbound by default; key-wallet** (`WalletInterface` via key-wallet-manager) at M5 in SB18 (DEC-112, manager call). `dashd-wallet` follows at M6 as SB18b, because it is WL2a's (O6; §2.4, §13) |
| Q5 | **Mobile ABIs.** arm64 only, or also armv7/x86? | **arm64 is the default mobile target**; the x86_64 simulator is dev-only (DEC-112, manager call; §8.2) |
| Q6 | **iOS and Mac checks.** When does the Mac run `build-ios.sh` for dash-spv-ffi, the size number and the fullfsync check? | **Open with pasta; gates only SB17's iOS cells** (iOS build, size, fullfsync), plus m6's real-device write-rate cell. Recommendation: first QA-phase Mac access; L6 covers the closure until then |
| Q7 | **Hybrid rdashd** (serve wallet, CL and IS from the SPV pipeline during IBD) | **No** (DEC-112, manager call); revisit after M5 |
| Q8 | **Platform's quorum lookup** (O3). `get_quorum_at_height`'s new semantics (§5.3: `Verified`, mined at or before h, not replaced at or before h − W_P) and the window W_P (38 blocks on every network, from rs-sdk's 31-minute freshness tolerance and Tenderdash's 3-minute empty-block interval). Older lookups are fetched on demand. Since DEC-444 a lookup can also answer `Unavailable` (could not decide, for example a failed fetch), distinct from not-found. Both need agreement with Platform's owner, through pasta | **Open with pasta; needed before SB11 starts, ≈ week 17.5.** Recommendation: §5.3 as written |

### 15.2 For the manager

| # | Question | Recommendation |
|---|---|---|
| M1 | Unhold EV0/EV2a/LQ1a on a short EV addendum (the split, generic `QuorumMembers<E>`, store-independent view errors with D4 kept on the node, `project_sml` signature) at SB0 approval | Yes; it protects the critical chain |
| M2 | Adopt §13's rows, SB12a and SB18b included, with run A's interface edges into PLAN and the committed `cpm.py` (M2 exit wk 18.5, REL11 wk 30.0; §13.4). If SB4 or CH1-node cannot code against stubs, plan for run C (wk 21.5 / 31.0) | Adopt run A; dispatch SB6 promptly, since it now has zero slack on M2 |
| M3 | SB12's scope, review routing and upstream timing | See PLAN §7 handling (CANDIDATE-SPV) |
| M4 | Who writes SB4's short design (D O-h) and the EV addendum, and in what order | SB4 design and the EV addendum in parallel, right after SB1 |

---

## 16. Revision changes

### 16.1 rev6 and rev5 changes (Sol r6 → DEC-115, DEC-116; Opus r6 → DEC-112)

**rev6 changes (Sol r6 → DEC-115, DEC-116).** Sol r6's narrow review accepted rev5's other changes: O1's node half, O2–O6, bounded R and the wallet checkpoint's authority. It raised three majors and one minor. rev6 changes nothing else.

| Finding | Resolution | Where |
|---|---|---|
| **R6-M1** `fork_target` checks the candidate's own flag, not its path or the floor | rev5's `best_header` and its Core update sites stay. A header is eligible for `fork_target` only if no entry on its path to the active chain is failed or `BLOCK_CONFLICT_CHAINLOCK` (Core's `FindMostWorkChain` walk), and it descends from the `cl_floor` block when there is one. The second rule also covers a lock on an already-active block. The most-work eligible header wins, and every published SPV tip respects the floor | §4.1 (struct, "SPV's fork-choice target"); §4.2 (ChainLock row); SB4, SB5; Appendix B |
| **R6-M2** an unknown wallet hash does not make the durable SPV tip a safe rewind point | No fork point is inferred from heights, and lost branch ancestry is not fetched again. The client does one `truncate_above(H)` and a rescan. H is the durable floor's height capped at min(wallet height, durable SPV tip), or the anchor height. §9.5 argues that H is on the retained chain | §9.5; SB13, SB14 |
| **R6-M3** a diff to h cannot show a commitment replaced in (h − W_P, h] | After pruning, a served type's lookup fetches null-base diffs to h − W_P and h. Rows, the LRU and fetches answer by one predicate. DEC-116 scopes the lifetime invariant to each network's Platform type, in its minimum-lifetime form, with a params test and a config-load refusal; other types return not-found after pruning. §5.3 carries the lifetime table | §5.2.1 rule 6; §5.3; SB11, SB15 |
| **R6-m1** "every run" gave run A's dates to run C | Each run's dates are quoted: 18.5 / 30.0 / 41.5 for A, B and the stress run, and 21.5 / 31.0 / 42.5 for C | §13.4 |

The preservation map below now covers these three. The CL floor binds SPV's tip through `fork_target`'s eligibility. An unknown wallet hash rewinds once to H. A pruned lookup of a served type fetches two targets.

**rev5 changes (Opus r6 → DEC-112).** Opus r6 approved with changes: 6 majors, 10 minors and 5 nits. DEC-112 rules on all of them and on Q1–Q7; rev5 closes each as Opus proposed, with the rulings' choices where DEC-112 makes one (O3's bounded retention, O5's test on every path, O6's key-wallet). rev5 changes nothing else.

**Sol-settled sections touched**, each because a finding forces it. The property Sol settled is unchanged in each:
- §3.4 (m5): the peer-queue bound names the 3 MiB message term. DEC-104 M2's send contract is unchanged.
- §4.1 and §4.2's ChainLock row (O1): rev1's best-header predicate is replaced by Core's update rules, and SPV's fork choice moves to `fork_target`. Acceptance and `TipChange` are unchanged. The CL floor binds SPV's tip through `fork_target`'s eligibility: no failed or conflicting entry on the path, and descent from the floor (rev6, R6-M1).
- §4.1.1 (m10, n1): `mtp` is filled lazily, and `CONTEXT` is an SPV-record flag. F2's rule-sized context and DEC-107 M1's envelope are unchanged.
- §4.3.1 steps 1–2 (m3): the birth height and MN-sync-off inputs are named. DEC-107's anchor rule, coverage and fallback are unchanged.
- §4.4 (m1, m2): the id is u32, and eviction runs after the header step commits. F3's ancestry reads and the cold tier are unchanged.
- §4.5's Then row (O1): `set_active_tip(fork_target)`.
- §4.6 (m7, O4): #1104 is merged or carried, and the wallet's offline-reorg check is added. The rewind order is unchanged.
- §5.2.1 rule 6 and §5.3 (O3): retention and Platform's lookup. H_keep, coverage, the canonical scan and select-then-`Verified` are unchanged. After pruning, only the served Platform type is fetched, at h − W_P and h, under the lifetime invariant (rev6, R6-M3, DEC-116).
- §6.1 (O4): the wallet adapter keeps no locator. The feed contract is unchanged.
- §7 (O5): the thread sources and the test's paths. The runtime model is unchanged.
- §9.4 (m5, m6): the snapshot pool waits at its cap, plus an iOS host note. M6's pinning read is unchanged.
- §9.5 (O4): the wallet has no locator in the SPV store. Every other subscriber's locator and recovery is unchanged. An unknown wallet hash rewinds once to H (the durable floor, or the anchor), never to a fork inferred from heights (rev6, R6-M2).
- §13.4 (O2): SB12a is added to the overlay. The method and every earlier number stand.

| Finding | Resolution | Where |
|---|---|---|
| **O1** `best_header` is a predicate, not Core's path-dependent `m_best_header` | `best_header` follows Core's update sites: strict-greater for `BLOCK_VALID_MASK` headers on acceptance (first seen wins a tie; a `CONFLICT` header does not move it); walk-back hooks that V calls at `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound`; `EnforceBlock`'s marking, which marks nothing when the locked block is already active; and the load recompute over `VALID_TREE` entries, conflicting ones included, with Core's comparator (the arena id stands in for the address tie-break). SPV's ChainLock-aware fork choice is a separate `fork_target` (`None` in the node), read only by dash-spv's header task. SB4 gains Opus's two node vectors | §2.2 (`dash-chain`); §2.4 (Headers, ChainLocks); §4.1 (struct, hooks, "Best header and SPV's fork-choice target"); §4.2 (ChainLock row); §4.5 (Then); §12.1 (CH1); SB4; Appendix B |
| **O2** dash-spv needs a dashcore ↔ base-sdk bridge before CV3/CV4; the "re-exports" are impossible | New row **SB12a** `dash-spv::bridge` (S, CON; deps CS0, SB7, SB9; SB12 depends on it). Each header is hashed once: `Header::from_parts_hashed` with a debug assertion, and a DEC-51a note. dashcore keeps adapter functions with the old signatures for bip158 and sml. CV3/CV4 collapse the bridge into the identity, and CV4's `P2pMsg` part is not subsumed by SB16. CPM rerun: no milestone moves (§13.4) | §0 row 7; §2.2 (`dash-consensus`, `dash-filters`, `dash-spv`); §5.5; §12.2; SB7, SB9, SB12a, SB12; §13.2 (CV3, CV4); §13.3; §13.4 |
| **O3** MN list and quorum retention is unspecified; Platform's lookup loses its message-log fallback | Lists are kept only while `recent`, an `Unverified` commitment or a pending fetch needs them, plus the tip list. Quorum rows {key, status, mined, replaced-at} are kept while active or replaced at or after tip − R, with R = max(H_keep, 2·W_P), and pruned in `dash-mnlist`'s own `WriteSet`. Platform's lookup is found when `Verified`, mined at or before h and not replaced at or before h − W_P. W_P = ⌈(31 + 3 min) / spacing⌉ + 24 = 38 on every network, with its sources. Anything older is fetched on demand (`getmnlistd`, plus the work block's `getmnlistd` or `qrinfo`); a failure is `Unavailable` (DEC-444). The semantics go to Platform's owner as Q8 | §0 row 5; §5.2.1 rule 6; §5.3 (struct, `lookup_quorum`, Retention, Platform's lookup, W_P table); §5.5; §8.2 (SML lists, Quorum rows); §9.5 (`mn_*` row, message log); §10.1; SB11, SB15, SB17; §15 Q8; Appendix B |
| **O4** the wallet's progress lives in the host's database | The wallet's synced heights are authoritative, and the SPV store keeps no wallet locator. The resume point is min(`wallet_synced_height`, durable SPV tip). An additive `WalletInterface` method returns the synced block hash. At open, a hash on a stale branch or unknown to the store triggers one `truncate_above` and a rescan; an implementation without the method logs once. The wallet advances only past committed header steps | §4.6; §6.1; §9.5 (`locators` row, `WriteSet`s, recovery, "Wallet progress"); §10.1; SB13, SB14 |
| **O5** the thread claims are not enforced where threads come from | Validation and wallet matching run under the host pool's `pool.install`; the global pool is never built. blst `no-threads` is set in dash-spv-ffi only (Android and iOS by target; an opt-in feature for the Linux test), and `check-mobile.sh` keeps it out of every N graph (DEC-79). `spv-clippy.toml` bans the pooled calls in V. dashcore's `quorum_validation`/`message_verification` features go after SB15. The FFI sets `worker_threads` and `max_blocking_threads` to 2. The thread-count test runs in SB13 over the old MN path, then in SB14, SB15, SB16 and SB17 | §2.3 (L5); §7 (rows, "Where SPV's threads come from", the test); SB13–SB17 |
| **O6** Q4's `dashd-wallet` is WL2a (M6), but SB18 exits M5 | SB18 uses key-wallet at M5. New row **SB18b** (M6; S, STATE; deps SB18, WL2a) adapts `dashd-wallet` in `rdashd`. It is outside the CPM model, which has no WL2a | §2.4; §13.1 (SB18, SB18b); §13.2 (M5); §13.3; §15 Q4 |
| **m1** `BlockId` width | `BlockId(u32)`, as in `dash-consensus` | §4.4 |
| **m2** eviction can precede the step's commit | Eviction runs in `set_active_tip` after the header step commits, keyed on `tip.height − w`; reopen loads entries at or above `tip.height − w` | §4.1 (`set_active_tip`); §4.4; SB5 |
| **m3** the anchor rule ignores two config paths | `start_from_height` is the birth height, honouring the network's sync floor. With MN sync off, coverage is vacuous: `Established` at the candidate, and CL/IS get `NoQuorum` | §4.3.1 steps 1–2; SB15 |
| **m4** the consumer list is incomplete | dashwallet-desktop, rs-dapi and Platform's direct storage and network uses are listed, with a consumer note per change | §10.1 |
| **m5** the memory window understates two items | Peer queues ≈ 4.3 MiB × 8 ≈ 34 MiB (a 3 MiB message is admitted before `fPauseRecv`). The snapshot pool is capped and `snapshot()` waits | §3.4; §8.2; §9.4 |
| **m6** SQLite on iOS and real flash are asserted | Per-target rusqlite features (system SQLite on iOS); the iOS `CompleteUntilFirstUserAuthentication` host requirement; the write-rate cell moves to a real device under Q6 | §8.1; §9.3 (f); §9.4 (fail-stop); §11.3; SB17 |
| **m7** #1104 is not on integration | SB14 needs #1104 merged or carries it | §4.6; §10.4; SB14; §13.3 |
| **m8** #902 reaches stage B | Q1 is needed before SB14 (≈ week 16); if #902 lands first, integration rebases before SB14 | §10.3; §13.3; §15 Q1 |
| **m9** the node-side ChainLock writer | In the node, `apply_chainlock` is a `ChainCommand` from `dashd-locks` to V | §2.4; §4.1; §12.1 (CH1) |
| **m10** `mtp` below the anchor pack | `HeaderCtx.mtp` is filled lazily, only where the envelope reads it, with a debug assertion | §4.1.1 |
| **n1** `CONTEXT` in the shared status word | An SPV-record flag only; never a bit in Core's `nStatus` layout | §4.1.1 |
| **n2** the node's `CheckpointView` | Core's chainparams list (39 mainnet entries; `-checkpoints`), never dash-spv's 51 anchor checkpoints; the rule is `bad-fork-prior-to-checkpoint` | §4.2; SB4 |
| **n3** §9.2 #4 misstates the commit | "One database, per-component atomic `WriteSet`s, WAL-ordered" | §9.2 |
| **n4** the rusqlite major | §8.1 cross-references §9.3 (f) | §8.1 |
| **n5** the base-sdk fork `[patch]` | Added to Q2 | §15 Q2 |
| **Q decisions** (DEC-112) | Q3 yes, Q4 no inbound and key-wallet, Q5 arm64 default, Q7 no: manager calls that pasta may override. Q1 before SB14 (≈ week 16), Q2 by ≈ week 9–10 (before SB10/SB12), Q6 only for SB17's iOS cells: with pasta. Q8 is new | §3.3; §8.2; §15.1 |

Not taken: **S1** (cut `legacy-net`), **S2** (one sparse id = slot table; m1's u32 fix is taken), and **S3**'s single connection and naming of the snapshot callers. m5 takes S3's cap and wait. S4 asks for no change.

### 16.2 rev4 changes (Sol r4 R4-M1)

Sol r4 closed every r3 finding and raised one major. It listed no minors. rev4 also takes DEC-109's (5) erratum and aligns SB0 with the EV addendum's §7 departures (fa09d8d0); it changes nothing else.

| Finding | Resolution | Where |
|---|---|---|
| **R4-M1** initial coverage has no rule for a short or pre-quorum genesis chain | L_est and H_keep are clipped at genesis. A required element at a negative height does not exist; Core treats that cycle as absent, and the element is dropped. Coverage targets are the ends of the **authenticated domain** [max(tip − H_keep, A_q), tip − 8], where A_q = max(`DIP0003Height`, `DIP0008Height`) is the first height whose CbTx carries `merkleRootQuorums`. A selection start that is negative or below DIP3 gets `NoQuorum`, because the set is canonically empty, as in Core. One in [DIP3, A_q) is unavailable without a fetch. An empty domain (tip − 8 < A_q, including every tip below 8) and an authenticated empty set both pass coverage trivially. The store becomes `Established`, the MN pipeline and subscribers start, and locks get `NoQuorum` until the first commitment, which then goes through the ordinary pipeline. The domain grows to depth min(H_keep, tip − A_q) and then slides. Coverage needs no `qrinfo`, and an early rotated cycle whose `qrinfo` Core cannot serve yet costs liveness only. Fallback below a non-genesis anchor and DEC-107's later-miss policy are unchanged | §4.3.1 (L_est clipping; step 2 targets and empty domain; step 4; tests); §5.2.1 (domain on a short or young chain; rules 2 and 5; SB11 bootstrap tests); §5.3 (`coverage`, `set_at`); SB11, SB15 |
| **DEC-109 (5)** the per-client name clashes with LQ1's `LlmqCtx` | SB11's per-client value is `LlmqConfig`, an owned `Arc<ChainParams>` plus the used types. `dash-mnlist` builds LQ1's per-call `LlmqCtx` from it | §4.3.1 (L_est); §5.2 step 5; §5.2.1 (C_max, H_keep); §5.3 (signatures, note); SB11 |
| **EV addendum §7** departures from SB0's text | `MnListView` stays infallible; the associated errors sit on `LlmqChain`, `LlmqSource` and `SnapshotRows` (DEC-109 (3)). `MemberError<E>` keeps `UnknownLlmqType(u8)`. `MinedCommitmentReader` moves from `store` to shared `dash-evo::llmq`. `MnList::project_sml` moves from EV2a to EV4-serve in `dash-dml` (DEC-109 (2)). No size, edge or CPM change | §2.2 (`dash-evo`, `dash-dml`); §4.1.1 (LQ1 bullet); §5.1; §12.1 (EV0, EV2a/b, EV4); §13.2 (EV0, EV2a/b, EV4) |

### 16.3 rev3 changes (Sol r3 → DEC-107)

DEC-107 removes the runtime re-anchor, since three of Sol r3's four majors came from it.
- The anchor is chosen once, during initial sync, and never moves.
- A later coverage miss costs liveness only and raises an operator-visible diagnostic.
- Re-anchoring an established wallet is a user-initiated resync, outside SB0.
- The CPM numbers do not change (§13.4).

| Finding | Resolution | Where |
|---|---|---|
| **M1** KGW's exit guard reads past the context | R is the rule's whole read envelope, not its contributing window. KGW's R is `PastBlocksMax + 1` = 4,033, clipped at genesis, and includes the loop-exit predecessor. The pure functions are unchanged. SB4 adds the maximum-length walk at parent 19,999 with uniform spacing (guard read 15,967), resident and cold, with a 4,032-entry negative control | §4.1.1; §4.3; §4.4 Sizing; §11.3; SB4; Appendix B |
| **M2** the rebuild breaks the MN locator/state contract | Removed with the runtime re-anchor. The anchor is chosen only during initial sync, while the store holds no MN, lock or subscriber state (`anchor_state = Candidate`). The MN pipeline and the subscribers start after `Established`. No transaction in a running store clears header, MN, lock or locator rows, so no locator can name missing state. The controlled rebuild and the locator-preservation rules are deleted | §0 row 4; §4.1.1 (LQ1 bullet); §4.3.1 steps 2–3; §5.2 closing; §9.5 (`meta`; anchor state replaces the re-anchor bullet); §12.1 CH1; SB4, SB15 |
| **M3** clearing the lock rows erases ChainLock finality | Removed: an established anchor never moves and `locks` is never cleared, so an applied ChainLock and its floor persist. Corruption recovery still discards the store at startup, before anything runs (§10.1), and is not a runtime transition | §4.3.1 step 3; §4.4 (storage errors); §9.5 |
| **M4** the old-set lock rule needs a horizon | H_keep = C_max + 8 + ⌈3,600 s / spacing⌉, measured to the selection start block: 320 on mainnet and testnet, 80 on devnet, 56 on regtest. Sets within it are retained, or fetched once per block with a bounded retry and then reported unavailable. Older locks are unavailable with no fetch. Select once, then require `Verified`, is unchanged. Coverage includes the horizon's oldest set, and the candidate rule uses L_est + H_keep. Sol r3's 2,500,148 / 2,500,155 case gates SB8 and SB11 | §0 row 5; §4.3.1 steps 1–2; §4.4 Sizing; §5.2 step 6; §5.2.1; §5.3 (`recent`, `set_at`); SB8, SB11, SB15; Appendix B |
| **m1** CH1-node's coding float | 0.5: earliest merge 11.5 minus coding completion 11.0. rev2's 2.0 was coding slack against the latest M2-safe merge, 13.0. Merge slack stays 1.5; P2a-node's float stays 2.0 | §13.4 |
| **m2** repeated re-anchor cost | The runtime re-anchor is gone. The initial-sync fallback clears only header tables, and its cost is stated as one forward header sync per checkpoint tried, ending at genesis at worst | §4.3.1 step 2 (Cost); §11.3 |

### 16.4 rev2 changes (Sol r2 → DEC-105)

rev3 removes F1's runtime re-anchor and steady-state recovery, and corrects F2's KGW R to 4,033 (§16.3).

| Finding | Resolution | Where |
|---|---|---|
| **F1** active quorum age has no bound in DKG intervals | Core's active set counts *mined* commitments, and a null round writes no mined record, so failed rounds keep old quorums active. Startup **coverage** resolves the actual required history of the authenticated active set: bases, work blocks (k = 0…3 for rotation) and diff targets. An uncovered element re-anchors at an earlier checkpoint, ultimately genesis, through a controlled rebuild and forward sync, with no backfill. **Steady-state recovery**: the affected commitments are `Unavailable` and their locks unavailable, then the client re-anchors; a stale-sign-height lock does not re-anchor. L_max becomes **L_est**, a regular-DKG sizing estimate, and `BelowFloor` is an ordinary answer. `Skipped` applies by configured use | §0 row 4; §4.1.1 (LQ1 bullet); §4.3.1; §4.4 Sizing; §5.2 closing; §5.3 `coverage`; §9.5 re-anchor; §11.3; SB11, SB15 |
| **F2** the 25-entry adapter cannot validate a genesis origin | `ContextWindow` becomes `RuleContext`. Its size is the selected rule's real lookback R, from `PowParams`: DGW 25, KGW `PastBlocksMax` 4,032, BTC the interval (576). It is built fallibly, borrowing resident entries and loading cold ones, before the unchanged pure functions run. The 25-header pack stays the post-DGW checkpoint format; the node's complete ancestry is unchanged | §0 row 4; §4.1.1; §4.2; §4.3; §11.3; SB4 |
| **F3** a retained snapshot read the live cold projection | Below its chunks, a snapshot resolves heights by ancestry from its own tip over the immutable `hdr` records. Persisted skip ids make that O(log n) per point query. `active_at` serves live queries only | §4.1 (`HeaderStore`, `HeaderRecord`); §4.4; §4.6; §6.2 F8; §9.5; SB5 |
| **F4** selection must precede the status gate | The scan is built from the authenticated active set regardless of local status, in Core's order and count, and selection runs unchanged. Only the selected commitment must be `Verified`; otherwise the lock is unavailable. No re-selection and no substitution | §0 row 5; §5.1; §5.2 step 6; §5.3; SB8, SB11 |
| **f1** the signer-position claim | Narrowed to a changed signing-key multiset, since dashbls sorts the keys. A permutation keeps the verdict and proves no order. SB8 adds the permutation case | §5.1; SB8 |
| **f2** coding float vs merge slack | §13.4 labels both. Merge slack: SB4/SB5 1.5, CH1-node 1.5, SB6/P2a-node 0. Coding float: P2a-node 2.0, CH1-node 2.0. M2 18.5 and REL11 30.0 are unchanged | §13.4 |

**Unchanged in rev2:**
- the DEC-104 simplifications: no SPV→full import, anchors by forward sync without backfill, commitment-verified keys without exact SPV membership, and #902's extended facade;
- every r1 finding Sol r2 closed: M1, M2, M5, M6, M7, M9 and m1–m5;
- the plan's sizes and edges, so the CPM overlay and its numbers stand.

### 16.5 rev1 changes (Sol r1 → DEC-104)

rev2's F1–F3 refine the M3 and M4 rows below (§16.4).

| Finding | Resolution | Where |
|---|---|---|
| **M1** wrong clock for v24 | The header time check uses `NodeClock` (mockable local time, no peer offset), as `release.rs` selects for v24. `AdjustedTime` serves only peer-facing uses. Tests cover ±3,600 s offsets and `setmocktime` | §4.2; §2.2 `dash-clock`; §2.4 and §7 Time rows; SB4 |
| **M2** push became admission | `push` is unconditional and returns `PauseState`, with Core's retained-memory charge (P2a §2.2.1), transport release on partial writes, and pause checks at Core's sites. Any hard budget is the SPV router's own | §3.1 (send contract); §3.4; §12.1 P2a; SB6 |
| **M3** pack cannot bootstrap the MN pipeline | Anchor = the newest post-DGW checkpoint whose forward history covers L_max, else the previous one, confirmed before the MN pipeline starts. L formulas and per-network numbers (mainnet/testnet 1,506, devnet 276, regtest 156). No backfill; #921 not adopted | §4.3, §4.3.1; §5.2; §1.2; §10.4; §11.3; SB4, SB15 |
| **M4** cold schema could not answer branch ancestry | Full records for every branch keyed by hash, with stable persisted ids and status; `active` as a projection; eviction, resolution, reopen and storage-error rules; bounded `ActiveChain`/snapshots; `AnchoredAncestry` beside the unchanged `ChainAncestry`, bridged by a 25-entry `ContextWindow` | §4.1, §4.1.1, §4.4, §9.5; SB4, SB5, SB13 |
| **M5** SPV → full replay lacked the prefix | **No import.** Full mode syncs headers from genesis and never opens `spv/` | §0 row 1; §2.4; §4.7; §6.3 P10; §9.2; §14.1 #1; SB18 |
| **M6** `BEGIN DEFERRED` does not pin | `snapshot()` reads the `meta` row before returning, on a pooled dedicated read connection. A failure trips the latch and yields a poisoned snapshot, because the contract's `snapshot()` is infallible. The interleaving and a negative control are in the engine contract suite | §9.4; §6.3 P5; SB10 |
| **M7** verbatim facade cannot bound topics | #902's facade is extended: bounded `Subscription`s with park-the-peer backpressure, `cancel`, terminal `RequestFailed`, `RequestId`/`AttemptId`, and consumer and test migration; coordination through Q1 | §3.2, §3.2.1; §3.3; §10.1; §10.3; §15 Q1; SB16 |
| **M8** BLS does not prove exact membership | SPV trusts commitment-verified quorum keys only (Core's `CFinalCommitment::Verify`) and exposes no member lists. Exact membership is node-only, with mnxcheck as the shadow. SB8's gate tests signer vs non-signer mutations | §0 row 5; §5.1, §5.2, §5.3; §12.1 LQ1; §14.1 #10; SB8 |
| **M9** missing edges, understated M2 | Edges `i:CS2`/`i:CS3` → SB4, `i:SB5` → CH1-node, LQ1b → SB11, SB15/SB16 → SB17, SB15 → SB18; order prose fixed. CPM rerun: **M2 exit 18.0 → 18.5, REL11 30.0** (run A); 21.5 / 31.0 without stub overlap (run C) | §13.1–§13.4; §0 row 7; §15.2 M2; `tools/cpm_sb0_rev1.py` |
| **m1** W → W and the waiver | B → B, W → B, P, W; a two-graph `allow_w` check; the ratcheted baseline list (SB1 installs, SB3 closes); planted failures plus a legitimate W → W pass | §2.3; SB1, SB3 |
| **m2** threads beyond `std::thread` | L5 bans runtime and pool builders, `spawn_blocking`/`block_in_place` and resolver calls. `BlockingIo` and `Resolver` are host services. An exercised-path thread-count test | §2.3 L5; §3.1; §7; SB13, SB17 |
| **m3** fatal store errors across LQ1 seams | Associated error types on every view; SPV `Unavailable` vs node D4 fail-stop; covered in the EV addendum | §5.1; §12.1 EV0, LQ1; §15.2 M1 |
| **m4** `dash-pow` missing, claims unmeasured | `dash-pow` and the C backends' flags are on the opt-level 3 list. The amended profile, memory, iOS and combined-SDK figures are labelled pending measurement | §8.1, §8.2; SB2, SB17 |
| **m5** feed mechanics | Inactive registration until `subscribe_at`, captured pending targets, generational `FeedId`, a staging reader within capacity (CH2's `SubscriberFeed`), space and deregistration producer wakes, separate tip and status streams, the DEC-96 exception; CH2's scenarios ported | §6.1, §6.2; SB5 |
| Sol: ABI baseline | Baseline = integration + #1104, with `size`/`version` negotiation and an old-layout caller test | §10.1; SB17 |
| Sol: SB13 transaction assembly | One store writer; the header step's `WriteSet` commits before publication; each subscriber commits its data with its locator | §9.5; §4.7; SB13 |

**Settled by Sol r1, not re-argued:**
- the 25-header pack (post-DGW anchors only);
- SQLite WAL durability and prefix recovery;
- F2, F10, F11 and P10 as contracts;
- L2's two-way independence of N and V.

---

## 17. Errata

Corrections adopted after approval (DEC-119). Each names the decision that adopted it and the text it amends; the amended passages carry a pointer here.

**E1 — the floor also ends at a SPORK_19 off → on reset** (DEC-123 Q2, DEC-127 Q-r1-1; SB4 §7.5). §4.3.1's "Established" item (step 3) says an applied ChainLock and its floor persist until a newer verified ChainLock supersedes them. Core has a second exit: `CheckActiveState`'s off → on transition calls `ResetChainlock` (`v24 chainlock/handler.cpp:209-222`, `chainlock/chainlock.cpp:98-104`), which clears the best ChainLock and the floor. SPV follows it, writes the cleared rows and the observed flag in that step's `WriteSet`, and persists the observed flag (SB4 D30), so a restart with the spork on does not reset. `BLOCK_CONFLICT_CHAINLOCK` marks survive the reset.

**E2 — SPV's floor rule is "does not conflict with", and the tip may trail the floor** (DEC-136, Opus high on SB4 O-M1; SB4 §6.2, D37). §4.1's eligibility rule (b) and the SPV `set_active_tip` refusal used "descends from `cl_floor`". With E1's reset, marks kept from before the reset can mark the new floor's own path. Example (corrected in SB4 rev7 by the Core-judged harness: a lock whose block is already on the active chain makes no marking walk. This connected-A fixture supplies the witness. A headers-only A branch keeps a mark too, SB4 A-29 (x′)): fork at 80, A81…A95 connected and active, B81…B110 headers; a lock on B100 marks A81…A95 and disconnects them; an observed off → on; a lock on A90 marks B81, and A81…A95 keep CONFLICT. Then no header descends from the floor and satisfies (a), while Core's tip trails the floor at 80 (SB4 A-29 (x)). The amended rules:
- **(b′)** a candidate is eligible under the floor if it does not conflict with it: it is the floor, an ancestor of the floor, or a descendant of the floor. This is the header-level form of `ConnectBlock`'s `HasConflictingChainLock` (`v24 validation.cpp:2516-2518`). Whenever an eligible floor descendant exists, it has more work than every ancestor of the floor, so (b′) selects what (b) selected.
- **(a)** is evaluated against the post-enforcement view: the step's enforcement cursor, after `EnforceBlock`'s disconnects, not the published active chain.
- **`set_active_tip`** in SPV mode refuses a target that conflicts with `cl_floor` while ChainLocks are observed enabled. **Every published SPV tip never conflicts with the floor**; it may trail it, as Core's node tip does.
- **Wallet recovery (§9.5).** The argument "the durable tip descends from F", "both chains contain F" now reads: both the durable tip and every lost published tip do not conflict with F (or with a later floor, which descends from F), so both chains contain F's ancestors and agree at every height up to min(f, the durable SPV tip, the wallet's synced height). The rule H = the floor's height capped at min(the wallet's synced height, the durable SPV tip) is unchanged, and the caps are now load-bearing rather than arithmetic guards. Sol r6's witnesses are unaffected: C105 conflicts with A100 and stays ineligible. *This wallet bullet is superseded by E3: its premise, that every lost published tip respected a floor descending from F, fails when the lost suffix observed the spork off.*

**E3 — wallet recovery rewinds by the wallet's block locator, as Core's wallet does** (DEC-140, Sol r4 R4-M1 on SB4 rev3; supersedes DEC-115 R6-M2's unknown-hash rule and E2's wallet bullet; SB4 D41).
- **Why.** A Buffered suffix lost at power loss can include an observed SPORK_19 off. While off, neither (b′) nor the refusal applies (§4.1, E2), so that suffix can publish a branch that conflicts with the durable floor. Sol r4's witness: the durable state is A100 with floor A70, observed enabled. A lost suffix observes the spork off, accepts B51…B110 (fork at A50) and publishes B110. The wallet durably reaches B110 with a payment in B60. After power loss the rule above gives H = min(70, 110, 100) = 70 and keeps B51…B70's effects, but the restored chain has A51…A70. The caps bound heights; they do not prove common ancestry.
- **Core's mechanism.** `CWallet` stores a best-block `CBlockLocator`: `chainStateFlushed` → `WalletBatch::WriteBestBlock` (`v24 wallet/wallet.cpp:624-633`, `wallet/walletdb.cpp:187-191`, fed by `ChainStateFlushed(m_chain.GetLocator())`, `validation.cpp:3114`), and the scan-progress locator of the last scanned block (`wallet/wallet.cpp:2053-2061`). At load, `AttachChain` rescans from `findLocatorFork(locator)` (`wallet/wallet.cpp:3526-3537`), which is `FindForkInGlobalIndex`: the first locator entry contained in the active chain, else genesis (`node/interfaces.cpp:1418-1425`, `validation.cpp:182-197`).
- **Where the locator lives.** In the key-wallet sync record that the host persists with `wallet_synced_height`, i.e. the per-wallet record in Platform's `platform-wallet-storage` or dashwallet-desktop's `dw-appdb`. It is not in the SPV store, which still keeps no wallet locator (O4). `WalletInterface` gains `wallet_synced_locator(wallet_id) -> Option<Vec<BlockHash>>` and `update_wallet_synced_checkpoint(wallet_id, height, locator)`, both additive with defaults. They replace this section's synced-hash pair, because the locator's first entry is the synced block's hash. SB14 implements them in key-wallet-manager.
- **Shape and size bound.** The locator is `HeaderChain::locator(Some(id))` for the record's block, computed on a published snapshot whose active chain holds that block. That is Core's `LocatorEntries` (`chain.cpp:32-49`): the block, then single steps until 11 entries, then doubling steps, ending at genesis, or in SPV at the anchor (SB4 §10.1). It holds at most 11 + ⌈log2 h⌉ + 1 hashes: 33 at today's mainnet height (about 1 KiB), 43 for any `u32` height.
- **Write ordering.**
  1. The header steps for every block up to the record's block commit in the SPV store (commit-before-publish, §4.6). So every locator entry was a committed header when the locator was taken, and its entries below the anchor collapse into the anchor, which is Established and Durable.
  2. The wallet's effects (including any reorg truncation), its synced height and its locator commit in one wallet-DB transaction. (DEC-144, Sol r5 R5-M1.) There is no separate effects commit, so a crash leaves either the old effects with the old checkpoint, or the new effects with the new checkpoint. It never leaves effects newer than their checkpoint. Core reaches the same end differently: it writes the best-block locator separately and, at load, `CWallet::LoadToWallet` revalidates every loaded confirmed or conflicted transaction against the active chain, resetting one whose block left it to inactive (`v24 wallet/wallet.cpp:1142-1167`). The single transaction is the simpler equivalent for SPV, which has no per-transaction block lookup at load.
  3. Buffered-versus-Durable skew between the two databases is harmless: an SPV suffix lost after step 2 shows up as unknown locator entries.
- **The rule at open.** If the record's first entry (the synced hash) is on the restored active chain, there is nothing to do. Otherwise the client takes L = the first locator entry present on the restored **active** chain (a known entry on a stale branch does not count, as in Core), or the anchor if there is none. It calls `truncate_above(height(L))` once and rescans from `height(L) + 1`. Every locator entry is an ancestor of the wallet's block, and L is on the restored chain, so every wallet effect kept at or below `height(L)` belongs to the restored chain. The floor, the ChainLock state and the spork history play no part. A known stale-branch hash goes through the same rule, as in Core. A wallet that returns no locator (`None`) is handled as before E3.
- **Cost.** Because of the doubling spacing, L can lie below the true fork point. In Sol r4's witness, `locator(B110)` = B110…B100, B99, B97, B93, B85, B69, 37, genesis. The first entry on the restored chain is height 37, below the fork at 50, so the client calls `truncate_above(37)` and rescans 38 onward. B60's effects are dropped. Core's wallet rescans the same extra span.
- **Tests.**
  - SB4 §15.2's `wallet_locator_lost_disabled_suffix` uses Sol r4's witness, with the suffix committed Buffered but not Durable: the rewind lands at 37, and B60's effects are dropped. The two controls are a first entry that is on the chain and an all-unknown locator (anchor).
  - `wallet_effects_checkpoint_one_txn` covers Sol r5's cut: effects committed before their checkpoint, then power loss. The single transaction makes that cut impossible by construction. The test asserts it: a recording wallet-DB layer fails on any commit that writes effects without the synced height and locator. Crash cuts just before and just after the one commit, including the reorg to B110, recover to A100 with no B effects, or to the B110 checkpoint and then the rewind to 37.
  - SB14 adds the wallet half: the key-wallet-manager record, `truncate_above` and the rescan.

**E4 — `FlushCommit`'s tip and sequence come from the freeze** (DEC-146, Sol r6 R6-m1 on SB4 rev5; SB4 §9.2, D45). F2 said `FlushCommit.tip_seq` reads the published `ChainReadSnapshot.tip_seq`. Inside the node's `enforce_block` hold (DEC-140), the published snapshot stays at the pre-enforcement tip, while DEC-144's per-disconnect `IF_NEEDED` check can freeze at an intermediate tip: B109 at `seq` q + 1 while the snapshot still says q. The amended rule: at each freeze V captures into `FreezePacket`, under the tip guard it already holds, the private tip as `new` and the `seq` of the event that made it the tip as `tip_seq`. W takes both only from the packet, and `FlushCommit { new, tip_seq }` repeats them. Outside a hold the pair equals the published snapshot's, so nothing else changes. W never takes the tip mutex to obtain it (SB4 §13.4 H1).

**E5 — `open` recomputes `best_header`, as Core's load does** (DEC-151 OQ6, DEC-157; SB5 §8 step 7, D17). Drafted in SB5 rev1 Appendix A and applied in SB5 rev3. §4.4's reopen step read the best-header id from `meta`. Core never persists `m_best_header`: `ChainstateManager::LoadBlockIndex` recomputes it over every entry with `IsValid(BLOCK_VALID_TREE)` using `CBlockIndexWorkComparator` (`v24 validation.cpp:5337-5360`; v23.1.8 `:5035-5036`; comparator `node/blockstorage.cpp:31-49`). The amended rule: `open` computes `best_header` as the maximum over the eligible entries, with a lower id winning a work tie (SB4 D9). The candidates are each stored leaf, or for a FAILED leaf its highest non-FAILED ancestor. CONFLICT is not a FAILED bit (`chain.h:116-118`, `:300-307`), so a CONFLICT-marked leaf stays eligible here, though not for `fork_target`. `meta.best_header` stays as a diagnostic. After a reopen, `best_header` can therefore move forward to a heavier stored leaf that a walk-back had left behind, which is exactly Core's behaviour after a restart.

**E6 — the producer may block only on O(1) leaf mutexes; wakes are notification-only** (DEC-152, Sol r1 R1-M2 on SB5 rev0; DEC-155 for controls; SB5 §4.3–§4.5, D6, D25, D29). Drafted in SB5 rev1 Appendix A and applied in SB5 rev2 (Sol r2 R2-m1).
- **§6.3 P2.** "`change_tip` never blocks, so every wait is V's service loop" reads: "`change_tip` never blocks except on leaf mutexes whose every critical section is O(1), and calls no code except notification-only wakes; every other wait is V's service loop." Core has the same contract: validation callbacks are enqueued under `cs_main` through `SingleThreadedSchedulerClient::AddToProcessQueue`, which takes the leaf `m_callbacks_mutex` for one `emplace_back` (`v24 scheduler.cpp:176-183`), and the scheduler thread holds it only for O(1) pops (`:145-155`).
- **§6.1's sketch.**
  - The `ArrayQueue` + `staged` + `SegQueue` slot is replaced by one leaf `Mutex<SlotInner>`. Its queue is reserved at Q. The reader takes events by swapping the queue against a spare buffer it owns, so no section is O(Q). No section allocates or frees.
  - Control is stored in the slot, one outstanding message per `ControlKind`. `send_control` replaces a same-kind payload in place, and the flag clears only when `next()` returns the message, so a slot holds at most 8 unconsumed messages (DEC-155 (1)).
  - `SubWaker` (a `Fn()` or a `Waker`) is replaced by `Notify`: `Task(Waker)`, used through `wake_by_ref`, or `Thread(Arc<Parker>)`, a condvar permit. There is no caller closure, so no wake runs I/O, waits for a response or handles inline.
  - `front` and `pop_control` are replaced by `next()`, which selects control before data in one slot critical section (SB5 D24; rev6.1's two calls let data overtake an earlier control).
- **§6.2.** F3's row adds "the producer's only blocking is E6's O(1) leaf sections". F5's row reads `Notify` for `SubWaker`.
- **§7.** A Wakes row: driver threads wake through a `Parker`, tokio tasks through their `Waker`; no wake runs subscriber code.
- **Not changed:** F1–F11's semantics and the tip lock's scope (SB4 §9.1).

**E6a — waits inside a hold** (DEC-157, Opus high m4 on SB5 rev2; SB5 §4.2, §4.5, §4.8). Applied in SB5 rev3. E6's O(1) rule applies to the fanout section and to every path a subscriber's progress needs. The holder of the tip mutex may block on (i) O(1) leaf mutexes, (ii) its own I/O and lock chains whose holders never wait for the tip mutex (the cold cache → store; SB4's index locks; V's block I/O), and (iii) its in-hold service wait: V's (DEC-153; CH1-node LO-9) and the SPV header writer's between sub-steps (SB5 §4.8; SPV-LO9, SB5 §4.9). §6.1's "Producer wakes" and §6.2's F3 row carry pointers here.

**E7 — durable ids are never reused** (Sol r1 confirmation on SB5 §5.1, D16; DEC-157). Applied in SB5 rev3. §4.4's "Ids are never reused" reads: an id that was ever durable is never reused. Ids issued in a Buffered suffix lost at power loss may be issued again after restart. No durable state names them: subscriber locators and the wallet's locator store hashes, and `locks` and `meta` commit with their records.

**E8 — the SPV header step leases the write connection** (DEC-157, Opus high m7 on SB5 rev2; SB5 §3.7, SB5-S1). Applied in SB5 rev3. §9.5's one-writer model keeps one write connection, owned by the store-writer task. The SPV header step leases it for stage 3: one message out, the connection back; `BEGIN IMMEDIATE`, the `HeaderStepSink::rows` batches (≤ W rows each), the O(1) tail and `COMMIT` run synchronously on the header task's thread; then the connection is returned. Other `WriteSet`s queue meanwhile, exactly as behind one long `WriteSet`. Cold reads during stage 3 use a separate read connection (SB5 §3.7's read-after-commit contract). The step's rows stream instead of forming one `take_dirty()` vector (SB5 D31). Commit-before-publish (§4.6) is unchanged.

## Appendix A. Evidence

**Size** (aarch64-linux-android, NDK 29, API 24, `llvm-strip`; recipe in the legs' appendices):

| Row | Contents | Stripped bytes |
|---|---|---|
| F-A | `libdash_spv_ffi.so`, dev 67b307c6 | 12,551,712 |
| F-B | probe, dash-spv-ffi only | 12,598,640 |
| F-C | + RocksDB (lz4, zstd) | 20,069,712 |
| F-D | + RocksDB, no compression | 19,331,832 |
| F-E | RocksDB alone | 7,790,976 |
| F-G | redb alone | 958,720 |
| F-H | + redb | 13,230,400 |
| F-I0 | integration probe | 12,669,096 |
| F-I | I0 + `dash-consensus` | 12,850,568 |
| F-LTO | fat LTO, 1 CGU, `opt-level = "s"` | 8,811,664 |
| O-v0 | integration `libdash_spv_ffi.so` | 12,622,128 |
| O-v1 | + `dashd-store` RocksDB | 20,319,224 |
| O-v2 | + RocksDB minimal | 19,477,816 |
| O-v3 | + redb | 13,258,048 |
| O-v4 | + SQLite (rusqlite bundled) | 14,437,176 |
| O-v5 | + SQLite, features trimmed | 13,755,184 |

Sources: `/work/scratch/sb0-fable/sizes/summary.txt`, `/work/scratch/sb0-opus/sizes/results.txt`.

**Storage probe** (x86_64, 2.55 M header-sized rows; `/work/scratch/sb0-opus/probe-results*.txt`):

| Engine | Write | Random get | RSS above base |
|---|---|---|---|
| SQLite (WAL) | ≈ 6 s | 5–6 µs | +3.8 MiB |
| flat file | 0.6–0.8 s | ≈ 1 µs | +0.3 MiB |
| RocksDB, mobile settings | — | 42–47 µs | +17 MiB |
| RocksDB, all file descriptors | — | ≈ 10 µs | +21 MiB |
| RocksDB, node settings | — | 7–9 µs | +41–72 MiB |
| redb | 22 s | — | (1,029 MiB file) |

**RocksDB RssAnon at small scale** (`/work/scratch/sb0-fable/sizes/rss.txt`): 0.9 MiB at start; 1.6 after open; 9.5 after 200k writes and a flush; 6.9 after reads.

**#902**: diff at `/work/scratch/sb0-reconcile/pr902.diff`; `NetworkManager` extract in `m902.rs`.

## Appendix B. Core and design anchors

| Topic | Anchor |
|---|---|
| Header acceptance | `v24 validation.cpp:4598` (`AcceptBlockHeader`), `:4469` (`ContextualCheckBlockHeader`), `:4616-4619`, `:4643-4646`, `:4693-4698`, `:4113-4122`, `:2516-2518`; `chainlock/chainlock.cpp:73-95` |
| Best header (DEC-112 O1) | `v24 node/blockstorage.cpp:111-115` (strict-greater for `BLOCK_VALID_MASK`), `:116-119` (a conflicting header does not move it), `:32-49` (`CBlockIndexWorkComparator`), `:100` (header-only `nSequenceId` 0); `validation.cpp:3951-3953`, `:3969-3971`, `:4089-4090`, `:4107-4108` (walk-backs), `:2043-2044` (`InvalidChainFound`), `:4047-4077`, `:4112-4113` (`EnforceBlock`), `:4288` (sequence id on block data), `:5359-5360` (load recompute), `:3533-3575` (`FindMostWorkChain`'s ancestry walk, for SPV's target, R6-M1); `P2a-outbound.md:869-878`, `:919` (readers) |
| Platform lookup window (DEC-112 O3) | Platform at 6499c680: `packages/rs-sdk/src/sdk.rs:84` (`DEFAULT_METADATA_TIME_TOLERANCE_MS`, 31 min), `:375-377` (the check); `packages/rs-drive-abci/src/abci/handler/prepare_proposal.rs:84-95`, `:126-128` (core height = best known CL); `packages/dashmate/configs/defaults/getBaseConfigFactory.js:422` (`createEmptyBlocksInterval` 3 min); `packages/rs-platform-wallet/src/spv/runtime.rs:340-359` (`get_quorum_public_key`). dash-spv: `client/queries.rs:73-105`, `dash-spv-ffi/src/platform_integration.rs:42-91` |
| Quorum lifetime (DEC-115, DEC-116) | `v24 llmq/blockprocessor.cpp:818-832` (active set: last mined per index when rotated, else the last `signingActiveQuorumCount`), `:311-323` (mining window); `evo/smldiff.cpp:31-64` (a diff lists the commitments active at its target); `llmq/params.h:138-511`; `chainparams.cpp:281-289`, `:483-492`, `:666-677`, `:949-957` |
| Header time check (M1) | `v24 validation.cpp:4509-4510` (`NodeClock::now()` + `MAX_FUTURE_BLOCK_TIME` → `time-too-new`); `dash-consensus/src/release.rs:50-59`, `:96-99` |
| Send contract (M2) | `v24 net.cpp:4878-4890` (`PushMessage`: charge, `fPauseSend`, append); `P2a-outbound.md:164`, `:275-276`, `:548`, `:575`, `:1117` |
| DGW lookback | `v24 pow.cpp:84-111` (parent + 23 predecessors) |
| Pre-DGW header contexts (F2) | `v24 pow.cpp:170-212` (era dispatch), `:15-79` (KGW, `PastBlocksMax`), `:134-168` (`GetNextWorkRequiredBTC`; min-difficulty walk-back `:142-157`, interval ancestor `:162-167`); `consensus/params.h:184` (`DifficultyAdjustmentInterval` = 86,400 / 150 = 576); `dash-consensus/src/pow/btc.rs:42-51`, `src/pow/kgw.rs` |
| KGW exit guard (DEC-107 M1) | `v24 pow.cpp:35-36` (the loop condition reads the entry's height before the `PastBlocksMax` test), `:64-65` (predecessor advance); `dash-consensus/src/pow/kgw.rs:34-37`, `:64-68` |
| Lock sign heights (DEC-107 M4) | `v24 chainlock/clsig.cpp:253-269` (a CL verifies at its own height), `llmq/quorumsman.cpp:711-721` (`SelectQuorumForSigningStartBlock`), `:849-854` (`VerifyRecoveredSig`); `instantsend/net_instantsend.cpp:136-152` (ISLock `nSignHeight`; a non-rotated type by `cycleHash`, `:149-152`); v23 `net_instantsend.cpp:106-122` |
| MN lookback estimate (M3) | `v24 llmq/blockprocessor.cpp:311-323` (mining window), `llmq/quorumsman.h:210` (`SIGN_HEIGHT_OFFSET`), `llmq/snapshot.h:40` (`WORK_DIFF_DEPTH`), `llmq/utils.cpp:491`, `:700-701`, `:715-720`; `llmq/params.h` |
| Active set by mined commitments (F1) | `v24 llmq/blockprocessor.cpp:369-378` (a null commitment returns before any mined record), `:687-729` (last A mined), `:731-794` (last mined per index), `:818-832` (`GetMinedAndActiveCommitmentsUntilBlock`), `:470-472` (`GetQcHashes`); `evo/smldiff.cpp:34-35`; `evo/cbtx.cpp:65-156`; `llmq/snapshot.cpp:195-229` (qrinfo history for each actual last commitment); `LQ1-quorum-members.md:845` |
| Quorum selection (F4) | `v24 llmq/quorumsman.cpp:724-775` (`SelectQuorumForSigning`: rotated `:733-756`, non-rotated `:757-773`), `:253-268` (blank scan on a missing quorum); LQ1 §3.8–§3.9 (`select_from_scan`, quirk 30) |
| Commitment verification (M8, f1) | `v24 llmq/commitment.cpp:76-93` (`membersSig` over signers), `:96-107` (`quorumSig`), `:312-321` (`BuildCommitmentHash`); `dashbls/src/schemes.cpp:231-253` (`VerifySecure` sorts the serialized keys) |
| Snapshot coherence (F3) | `CH2-coins-flush.md:1313-1314` (`ChainSnapshot`), F8 `:1342`; `v24 chain.cpp:84-130` (`GetSkipHeight`, `GetAncestor`, `BuildSkip`) |
| Complete ancestry contract (M4) | `dash-consensus/src/ctx.rs:35-58`; `src/pow/mod.rs:192` |
| Engine snapshot contract (M6) | `dashd-store/src/engine.rs:114-115` |
| SML projection | `v24 evo/deterministicmns.cpp:43-46`, `:255-259` |
| CH2 feed contract | CH2 §2.9.1 (F1–F11, CH2:1335-1345), §10.1 (P1–P10, CH2:2452-2461), producer interface CH2:1397 |
| LQ1 SML adapter | LQ1 §2.2.4 (LQ1:240-251) |
| P2a runtime and modules | P2a §2.2 (P2a:127-169), §2.3 (P2a:171-210) |
| R2 and its enforcement | A1:147; `deny.toml:48-80`; `contrib/node/r2_reach.py`, `check-r2.sh` |
