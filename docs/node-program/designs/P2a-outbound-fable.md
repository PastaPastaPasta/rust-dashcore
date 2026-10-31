# P2a: outbound P2P core (`dashd-p2p`) — independent design (Fable)

| | |
|---|---|
| Task | PLAN P2a (W5), M2, size L, class P2P, design D2 (this is the Fable half; the manager reconciles with the Opus half) |
| Crate | `dashd-p2p` (runtime), with the `dash-p2p-transport` seam (P1) and the `dashd-chainstate` seam (CH1, interface dependency) |
| PRD | FR-P2P-05, -06, -07, -08 (headers part), -16; NFR-FP-01 (behaviours 1–7, 11, 25 outbound, +C); FR-CHS-02/06 as a consumer |
| Core pins | v23.1.8 = `728f505583` (mainnet profile), v24.0.0-rc.3 = `0364ed626e` (testnet/devnet/regtest profile). Every cite is `file:line` at **v24 / v23** unless one pin is named. Extracted sources: `/work/scratch/p2a-fable/{v24.0.0-rc.3,v23.1.8}/src` |
| Inputs | A1 §2.14, §2.25, §3.1, §4.10; O17 §4.2 (`Identity`, `P2pRules`) and packet `p2p-init` (P2P-01…-39); BS4a `DDE-SWEEP.md` §4; base-sdk `p2p_core` on `node/bsdk-bs4a-decode-domain`; dash-spv `src/network/`; R4 §1.15, §2.7, F §5 |
| Scratch | `/work/scratch/p2a-fable/` |

---

## 0. Summary

`dashd-p2p` is a line-for-line port of the outbound half of Core's `net.cpp` + `net_processing.cpp`, organised as A1 §2.14 prescribes: a tokio `ConnMan` (connection opening, seeds, addnode, anchors, feelers, inactivity, linger), a single-threaded `PeerManager` actor that reproduces `ThreadMessageHandler`/`ProcessMessages`/`SendMessages` **including Dash's quorum-priority receive queue and the 100 ms masternode send throttle**, a Core-faithful `AddrMan` (same bucket hashing, same `peers.dat` V4 format for import, same `GetChance`/`IsTerrible` arithmetic), and `release.rs` (`Identity`, `P2pRules`) read at every call site O17 keys. The two pins differ in **nothing** P2a sends or accepts except the four `Identity` values (P2P-01), the clock behind three header-sync decisions (P2P-16), inv type 34's known-ness (P2P-04), the `notfound` cap (P2P-10) and the bind-failure policy (P2P-33, consumed from `InitRules`); every other P2a function diffs clean between the tags (§4.0). Messages are decoded in the actor after Core's state gates through base-sdk's `decode_with_policy`, and `headers`/`headers2` is the only message P2a awaits (one `ChainCommand::ProcessHeaders` per message; the reply carries the index facts Core reads under `cs_main`, so no TOCTOU). The design enumerates 60+ observable behaviours with Core cites and the Rust rule for each (§4), lists the M2 gaps that O13a must mask (no sync chatter, no object requests; §1.3), and specifies the seams P1, P2b, P3, P5/P5a, SP0/SP1, P7 and RP1a plug into (§3.6). Verification is O13a transcripts per profile, addrman bucket-math parity against a new `oracle_addrman_*` family, `diff_p2p_*` at handler level for the M2 command set, four model fuzzers and the ported `p2p_*`/`feature_anchors_db`/`p2p_dns_seeds` functional tests (§7). Eight phases (§8), each PR-sized.

---

## 1. Scope and non-goals

### 1.1 In scope (PLAN row P2a)

| Area | Core | This design |
|---|---|---|
| ConnMan, outbound | `net.cpp` `ThreadOpenConnections` 2917/2923, `ThreadDNSAddressSeed` 2680/2686, `ThreadOpenAddedConnections` 3368/3374, `ProcessAddrFetch` 2806/2812, `PerformReconnections` 4980/4939, `ConnectNode` 372/371, `OpenNetworkConnection` 3586/3592, `DisconnectNodes` 2067/2073, `InactivityCheck` 2298/2304, `Start` 3991/3985, `StopNodes` 4187 | §4.1–4.5, §4.14 |
| Handshake and `fingerprint.rs` | `PushNodeVersion` 1561/1538; `ProcessMessage` VERSION 3935/3906, VERACK 4187/4156, the pre-verack set 4261–4362/4216–4317; `init.cpp` service bits 1125–1245, 1371, 2354 (v23 1077–1197, 1387, 2314); `strSubVersion` 1828–1843 | §3.1, §4.6 |
| Actor loop, gates, decode policy | `ThreadMessageHandler` 3676/3682; `PollMessage`/`MarkReceivedMsgsForProcessing` 4773–4850/4732–4810; `ProcessMessages` 5872/5728; `ProcessMessage` gates 4175–4385/4146–4340; BS4a §4 table | §3.3, §4.7, §4.15 |
| Awaiting observable messages | headers via `ProcessNewBlockHeaders` 3322/3338 | §4.11 |
| Ping | `MaybeSendPing` 6148/6002; PING 5456/5379; PONG 5474/5397 | §4.9 |
| AddrMan, addr relay, getaddr (request side) | `addrman.cpp` (identical at both pins except `GetEntries`), `netgroup.cpp:18-80`, ADDR/ADDRV2 4388/4343, `MaybeSendAddr` 6186/6040, `RelayAddress` 2686/2665, `SetupAddressRelay` 6283/6137, `GetAddresses` 4254–4309/4241–4296 | §3.4, §4.10, §5 |
| headers/headers2 sync | SendMessages sync start 6342–6395/6196–6249; HEADERS 5309/5232; `ProcessHeadersMessage` and helpers 3138–3387/3154–3403; `ConsiderEviction` 5949/5803; `EvictExtraOutboundPeers` 6007/5861; `CheckForStaleTipAndEvictPeers` 6121/5975 | §4.11 |
| Misbehaviour (the part P2a's handlers raise) | `Misbehaving` 1933/1953, `MaybeDiscourageAndDisconnect` 5833/5689, `MaybePunishNodeForBlock` 1969/1992 | §4.13 |
| Peer stats for `getpeerinfo` | `CopyStats` 596/595, `GetNodeStateStats` 1877/1897 | §3.6.7 |

### 1.2 Non-goals (owned elsewhere; this design only fixes the seam)

- **P1** `dash-p2p-transport`: V1/V2 framing, BIP324 handshake, short IDs, v1 detection. P2a consumes the `Transport` trait (§3.6.1) and reproduces only the *reader's* outcome table (§4.7.1).
- **P2b** inbound: listeners, `CreateNodeFromAcceptedSocket`, the responder handshake, `getheaders` serving, the one-answer `getaddr` cache. P2a leaves the hooks (§3.6.2).
- **P3** block download, direct fetch, stalling, compact blocks. P2a ports the header-side state (`pindexBestKnownBlock`, `hashLastUnknownBlock`, `m_last_block_announcement`) and leaves `HeadersDirectFetchBlocks` as a no-op hook that P3 fills (§4.11.6).
- **P4** inbound full: eviction, banman, permissions, serving, `-capturemessages`. P2a uses a `BanView` seam for the outbound ban/discourage checks (§3.6.5).
- **P5 / P5a** tx relay, both object-request trackers, trickle, orphans, the overlay `NetHandler` registry. P2a defines the `RequestTracker` and `NetHandler` seams and ships a `NoRequests` tracker (§3.6.3).
- **SP0 / SP1** sporks and masternode sync (`getsporks`, `govsync`, `mempool`, `full-sync` disconnects, netfulfilled). P2a provides the `on_send_tick` and scheduler hooks (§3.6.4).
- **P7 / AM1** masternode connections, MNAUTH signing and verification, probes, `qwatch`, `qsendrecsigs`. P2a reserves the connection flags, the challenge plumbing and the MN send throttle (§3.6.6).
- **P8** proxies, Tor, I2P, CJDNS, asmap, `-onlynet`. P2a keeps `NetGroupManager::get_group` asmap-capable with an empty asmap and a `Proxy` seam that is `None` in M2.
- **ND1** init order, `-releaseprofile`, `InitRules` (P2P-33/-34). P2a exposes `ConnMan::start(opts)` and reads `InitRules.bind_failure_fatal` through the options.

### 1.3 What an M2 rdashd does **not** send, and what O13a must mask

Core's post-handshake chatter that comes from components outside P2a: `getsporks` (SP0/SP1, `node/sync_manager.cpp:206-209` v24, `:208-211` v23), `govsync` (SP1), `mempool` (SP1), `mnauth` (AM1), `qwatch` (P7), `getdata` for tx/overlay invs (P5), tx `inv` trickle (P5). An M2 rdashd emits none of these, and it never requests objects. O13a's normaliser therefore compares the **handshake window** (behaviours 1–5, 9, 10) and must drop those commands from the dashd side until their owners land; O13b removes the mask. This is the single biggest fingerprint gap at M2 and is listed as open question Q1 (a tiny `getsporks` stub).

---

## 2. Crate setup and module layout

```
dashd-p2p/
  Cargo.toml          # deps: dash-p2p-transport (P1), dash-p2p-core (base-sdk, via node/base-sdk pin), dash-consensus (CS0: BlockHeader, BlockHash, Release), dashd-chainstate (ChainCommand, ChainView; interface dep), dash-params (BS5), tokio, bytes, arc-swap, rand_chacha, tracing
  src/lib.rs
  src/release.rs      # Identity, P2pRules, fingerprint::Fingerprint (§3.1)
  src/types.rs        # PeerId, ConnectionType, ServiceFlags re-export, Address (CAddress), NetAddr
  src/clock.rs        # NodeClock (mockable), SteadyClock, Poisson/uniform draws (§4.17.4)
  src/net/            # ConnMan (Core net.cpp)
    mod.rs            # ConnMan handle, Options, start/stop
    conn.rs           # one connection: reader task, writer task, send queue, linger
    outbound.rs       # ThreadOpenConnections port (slot selection, feelers, anchors, extra peers, fixed seeds)
    dnsseed.rs        # ThreadDNSAddressSeed port
    addnode.rs        # ThreadOpenAddedConnections + GetAddedNodeInfo
    addrfetch.rs      # AddAddrFetch / ProcessAddrFetch / -seednode
    reconnect.rs      # PerformReconnections (v1 fallback)
    inactivity.rs     # InactivityCheck sweep (1 s) and DisconnectNodes (linger, SHUT_WR)
    local.rs          # mapLocalHost, AddLocal/SeenLocal/IsLocal, GetLocalAddrForPeer
    stats.rs          # per-connection counters (CNodeStats)
    inbound.rs        # P2b fills (accept, eviction hook)
  src/addrman/
    mod.rs            # AddrMan actor API (Add/Good/Attempt/Connected/SetServices/Select/SelectTriedCollision/ResolveCollisions/GetAddr/Size)
    impl_.rs          # AddrManImpl port (tables, mapInfo, vRandom, collisions)
    bucket.rs         # GetTriedBucket/GetNewBucket/GetBucketPosition/IsTerrible/GetChance
    netgroup.rs       # NetGroupManager::GetGroup (+asmap hook), CalculateKeyedNetGroup
    file.rs           # peers.dat: own format + Core V4_MULTIPORT import; SerializeFileDB wrapper
    anchors.rs        # anchors.dat read (and delete) / dump
  src/peerman/        # PeerManager actor (Core net_processing.cpp)
    mod.rs            # PeerManager handle, spawn, wake
    actor.rs          # ThreadMessageHandler loop (shuffle, MN throttle, 100 ms wait)
    peer.rs           # Peer (+TxRelay slot) and NodeState unions
    queue.rs          # per-peer receive queues: quorum-priority + normal, PollMessage ratio (§4.7.2)
    gates.rs          # ProcessMessages: getdata-first, orphan slot, pause, poll; ProcessMessage state gates
    policy.rs         # per-command decode policy table (§4.15)
    handshake.rs      # version/verack/sendheaders(2)/sendcmpct/sendaddrv2/sendtxrcncl handlers
    ping.rs           # MaybeSendPing, ping/pong handlers
    addr.rs           # addr/addrv2/getaddr(request side), MaybeSendAddr, RelayAddress, token bucket
    headers.rs        # sync start, HEADERS/HEADERS2, ProcessHeadersMessage, unconnecting, timeouts
    inv.rs            # INV (block part + unknown-type path), NOTFOUND (cap), hand-off to RequestTracker
    eviction.rs       # ConsiderEviction, EvictExtraOutboundPeers, CheckForStaleTipAndEvictPeers, TipMayBeStale
    send.rs           # SendMessages in Core's order
    misbehavior.rs    # Misbehaving, MaybeDiscourageAndDisconnect, MaybePunishNodeForBlock(header part)
    handlers.rs       # NetHandler registry + "known command, no handler" behaviour (§3.6.3)
    tracker.rs        # RequestTracker trait + NoRequests (P5 fills Legacy/TxRequest)
    chain.rs          # ChainCommand client: ProcessHeaders await, best_header mirror, ChainEvent subscription
    stats.rs          # CNodeStateStats
  benches/            # addrman_ops, actor_loop, headers_sync_replay (§6)
  tests/              # unit + model tests; functional-test glue lives in dash-testkit
```

Conventions (A1 Appendix E): a `// Core: net_processing.cpp:NNNN FunctionName` anchor per ported function, pinned to **both** tags where they differ; `#![forbid(unsafe_code)]`; no `std::time` outside `clock.rs`; no `HashMap` iteration on any path whose order a peer can observe (the per-peer shuffle uses `Vec` + Fisher–Yates; the `m_peer_map` iteration in `RelayAddress` is order-insensitive by construction, a max over SipHash keys).

---

## 3. Data model and public API

### 3.1 `release.rs`: `Identity`, `P2pRules`, `Fingerprint`

Both tables follow O17 §4.2 exactly; P2a owns the `dashd_p2p::release` module O17 names as their home.

```rust
// dashd_p2p::release  — Core: version.h:14,23 (both pins); configure.ac:3-6; O17 P2P-01..-04, -10
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub protocol_version: i32,        // 70240 | 70242  (i32: Core's `int nVersion`, BS4a `ProtocolVersion` is i32)
    pub min_mn_proto_version: i32,    // 70240 | 70242  (read by DK1/LQ4a, not P2a)
    pub ua_version: &'static str,     // "23.1.8" | "24.0.0"
    pub client_version: i32,          // 230108 | 240000 (getnetworkinfo.version, RP1a)
}
impl Identity {
    pub const V23_1: Identity = Identity { protocol_version: 70240, min_mn_proto_version: 70240, ua_version: "23.1.8", client_version: 230108 };
    pub const V24_0: Identity = Identity { protocol_version: 70242, min_mn_proto_version: 70242, ua_version: "24.0.0", client_version: 240000 };
    pub fn of(r: Release) -> &'static Identity { match r { Release::V23_1 => &Self::V23_1, Release::V24_0 => &Self::V24_0 } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum ObjectDownload { Legacy, TxRequest }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum QrInfoRequest { Unbounded, Capped { max_base_blocks: usize } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum BlockRead { UnderMainLock, Unlocked }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P2pRules {
    pub object_download: ObjectDownload,        // P2P-05..-09, -11 (P5)
    pub notfound_limit: usize,                  // 116 | 50_000 (P2P-10)  — read by P2a's NOTFOUND handler
    pub unsolicited_scoring: bool,              // P2P-12 (P5a)
    pub tx_inv_before_verack: bool,             // P2P-15 (P5)
    pub msg_asset_unlock: bool,                 // P2P-04 — read by P2a's INV handler (`is_known_inv_type`)
    pub mnauth_inbound_cap: Option<usize>,      // P2P-18 (P7)
    pub request_gov_invs_when_disabled: bool,   // P2P-21 (GV1b)
    pub qrinfo_request: QrInfoRequest,          // DKG-12 (P6b)
    pub mnlist_data_unavailable_score: u32,     // DKG-13 (P6b)
    pub block_read_vs_prune: BlockRead,         // P2P-17 (P4)
}
impl P2pRules { pub const V23_1: P2pRules = …; pub const V24_0: P2pRules = …; pub fn of(r: Release) -> &'static P2pRules }

/// Every P2P-visible constant of this node, assembled once at init from the profile and the args.
/// Core: PushNodeVersion net_processing.cpp:1561/1538; init.cpp:1125-1245,1371,2354 / 1077-1197,1387,2314; init.cpp:1828-1843 (strSubVersion)
pub struct Fingerprint {
    pub identity: &'static Identity,
    pub rules: &'static P2pRules,
    pub time_source: TimeSource,            // UngatedRules.time_source (CS0): Adjusted (V23_1) | Local (V24_0); P2P-16
    pub local_services: ServiceFlags,       // computed as §4.6.1
    pub user_agent: UserAgent,              // "/Dash Core:<ua_version>(<comments>)/" or the honest UA (DEC-04)
    pub push_version: Option<i32>,          // `-pushversion` (non-mainnet only), Core: net_processing.cpp:1577-1580 / 1554-1557
    pub v2_transport: bool,                 // -v2transport (default true) ⇒ NODE_P2P_V2
    pub listen: bool, pub discover: bool,
}
```

`Fingerprint::user_agent` is `FormatSubVersion("Dash Core", <ua_version>, comments)` (`clientversion.cpp:68-83`, same at both pins): `/` name `:` version, then `(c1; c2)` if any comment, then `/`. Comments are `devnet.<name>` first (only on devnets), then each `-uacomment` in order (`init.cpp:1828-1840`). Length > 256 is an InitError with Core's text (`init.cpp:1841-1843`). `-honestua` substitutes `/rdashd:<cargo version>/` with the same comment rules; `-uacomment` works unchanged (DEC-04).

### 3.2 ConnMan types

```rust
// Core: node/connection_types.h (identical at both pins); strings from connection_types.cpp:11-22
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionType { Inbound, OutboundFullRelay, Manual, Feeler, BlockRelay, AddrFetch }
impl ConnectionType {
    pub fn as_str(self) -> &'static str;             // "inbound" | "outbound-full-relay" | "manual" | "feeler" | "block-relay-only" | "addr-fetch"
    pub fn is_outbound_or_block_relay(self) -> bool;   // net.h:886
    pub fn is_manual_or_full_outbound(self) -> bool;   // net.h:909
    pub fn expects_services(self) -> bool;             // net.h:941: OutboundFullRelay | BlockRelay | AddrFetch
}

pub struct Options {                                   // Core: CConnman::Options net.h:1222-1255 (v23 same fields minus m_masternode_connections)
    pub local_services: ServiceFlags,
    pub max_connections: i32,                          // nMaxConnections after the fd clamp (ND1 computes; init.cpp:1296-1314)
    pub max_outbound_full_relay: i32,                  // min(8, max_connections)
    pub max_outbound_block_relay: i32,                 // min(2, max_connections - full_relay)
    pub max_outbound_onion: i32,                       // min(2, max_connections/2) (P8 uses)
    pub max_addnode: i32, pub max_feeler: i32,         // 8, 1
    pub send_buffer_max: usize, pub recv_flood_size: usize, // 1000*maxsendbuffer, 1000*maxreceivebuffer
    pub peer_connect_timeout: Duration,                // -peertimeout, default 60 s
    pub connect_timeout: Duration,                     // -timeout, default 5000 ms (netbase.h:25)
    pub seed_nodes: Vec<String>, pub added_nodes: Vec<String>,
    pub use_addrman_outgoing: bool, pub specified_outgoing: Vec<String>,   // -connect semantics (init.cpp:2722-2727)
    pub dnsseed: bool, pub fixedseeds: bool, pub forcednsseed: bool,
    pub bind_failure_fatal: bool,                      // InitRules (P2P-33); P2b's InitBinds reads it
    pub masternode_connections: bool,                  // v24 gates the mncon task; v23 always runs it (unobservable, O17 #7659)
    pub active_masternode: bool,
    pub binds: Vec<Bind>, pub whitebinds: …, pub whitelist: …, pub onion_binds: …, pub bind_on_any: bool, // P2b
}

pub struct ConnMan { /* handle: Arc<Inner> */ }
impl ConnMan {
    pub async fn start(opts: Options, deps: ConnManDeps) -> Result<ConnMan, InitError>;  // Core: Start 3991/3985
    pub fn stop(&self);                                                                   // Interrupt + StopThreads + StopNodes (dump peers, anchors)
    pub fn open_network_connection(&self, addr: Address, count_failure: bool, grant: Option<SlotGrant>, dest: Option<&str>, kind: ConnectionType, v2: bool, mn: MasternodeConn, probe: bool); // 3586/3592
    pub fn disconnect_node(&self, sel: NodeSelector) -> bool;                             // 4578-4615
    pub fn add_node(&self, p: AddedNodeParams) -> bool; pub fn remove_added_node(&self, s: &str) -> bool; pub fn added_node_info(&self, include_connected: bool) -> Vec<AddedNodeInfo>;
    pub fn node_count(&self, dir: ConnectionDirection) -> usize; pub fn node_stats(&self) -> Vec<NodeStats>;
    pub fn set_try_new_outbound_peer(&self, b: bool); pub fn try_new_outbound_peer(&self) -> bool;    // 2827-2836
    pub fn start_extra_block_relay_peers(&self);                                                     // 2838
    pub fn extra_full_outbound_count(&self) -> i32; pub fn extra_block_relay_count(&self) -> i32;   // 2850, 2868
    pub fn local_services(&self) -> ServiceFlags; pub fn network_active(&self) -> bool; pub fn use_addrman_outgoing(&self) -> bool;
    pub fn get_addresses(&self, max: usize, pct: usize, net: Option<Network>, filtered: bool) -> Vec<Address>; // 4254 (v23 has no `filtered`; P2a passes true)
    pub fn get_addresses_cached(&self, requestor: &PeerCtx, max: usize, pct: usize) -> Vec<Address>; // 4265/4252 (P2b uses; the cache lives here)
    pub fn deterministic_randomizer(&self, id: u64) -> SipHasher;                                   // 4968
    pub fn wake_message_handler(&self);                                                              // 2671
    // P7 seam (implemented later, declared now): add_pending_masternode, set_masternode_quorum_nodes, set_quorum_relay_members, add_pending_probe_connections, try_mark_verified, is_masternode_quorum_node, …
}
pub struct ConnManDeps { pub addrman: AddrMan, pub banman: Arc<dyn BanView>, pub peerman: PeerManagerHandle, pub netgroup: Arc<NetGroupManager>, pub params: &'static ChainParams, pub fp: Arc<Fingerprint>, pub clock: Clock, pub mn_list: Arc<dyn MnListView>, pub mn_sync: Arc<dyn MnSyncView> }
```

A connection is `Conn { id: PeerId, addr: Address, addr_bind, addr_name: String, dest: String, kind, v2: bool, nonce: u64, keyed_netgroup: u64, connected_at: Seconds, transport: Box<dyn Transport>, send: SendQueue, recv: RecvQueues, flags: ConnFlags, stats: ConnStats, mn: MnFlags }`. `ConnFlags` holds Core's atomics: `successfully_connected`, `disconnect`, `other_side_disconnected`, `pause_recv`, `pause_send`, `has_all_wanted_services`, `relays_txs`, `version: i32`, `common_version: i32`, `time_offset`, `first_message_at`, `first_message_is_mnauth`; `MnFlags` holds `masternode_connection`, `masternode_probe_connection`, `masternode_iqr_connection`, `qwatch`, `verified_pro_reg_tx`, `verified_pubkey_hash`, `sent_mnauth_challenge`, `received_mnauth_challenge`, `version_services`, `version_relay_txs` (the last two are v24's DKG-01 inputs, `net.h:820-823`; harmless under V23_1).

### 3.3 PeerManager actor

```rust
pub struct PeerManagerHandle { tx: mpsc::UnboundedSender<Event>, wake: Arc<Notify> }
pub enum Event {
    NodeInitialized(ConnHandle),                 // InitializeNode 1745/1779 — the actor owns Peer+NodeState from here
    NodeFinalized(PeerId),                       // FinalizeNode 1783/1816
    MsgReady(PeerId),                            // reader queued ≥1 complete message (WakeMessageHandler 2671)
    SendableAgain(PeerId),                       // fPauseSend cleared by the writer (the loop re-checks fMoreWork)
    Chain(ChainEvent),                           // UpdatedBlockTip, BlockConnected, BlockDisconnected, NewPoWValidBlock, BlockChecked (A1 §2.10.2 bus)
    Scheduled(Tick),                             // CheckForStaleTipAndEvictPeers every 45 s (StartScheduledTasks 2143/2164)
    Rpc(RpcCall),                                // ping (m_ping_queued), getpeerinfo state stats, …
}

/// Core: struct Peer net_processing.cpp:240-409 (v23 ≈ same, no m_recent_object_requests); struct CNodeState :414-532
pub struct Peer {
    id: PeerId, our_services: ServiceFlags, their_services: ServiceFlags,
    misbehavior_score: i32, should_discourage: bool,
    blocks_for_inv_relay: Vec<BlockHash>, blocks_for_headers_relay: Vec<BlockHash>,
    starting_height: i32, ping_nonce_sent: u64, ping_start: Micros, ping_queued: bool, wants_recsigs: bool,
    tx_relay: Option<TxRelay>,                      // SetTxRelay() at VERSION per Core's rule; P5 fills its contents
    inv_relay: InvRelay,                            // GetInvRelay(): always present (vInventoryOtherToSend etc.)
    addrs_to_send: Vec<Address>, addr_known: Option<RollingBloom /*5000, 0.001*/>, addr_relay_enabled: bool, getaddr_sent: bool,
    next_addr_send: Micros, next_local_addr_send: Micros, wants_addrv2: bool, wants_dsq: WantsDsq,
    getaddr_recvd: bool, getsporks_recvd: bool, getsporks_last_response: Vec<Hash256>,
    addr_token_bucket: f64, addr_token_timestamp: Micros, addr_rate_limited: u64, addr_processed: u64,
    inv_triggered_getheaders_before_sync: bool, getdata_requests: VecDeque<Inv>, last_getheaders_timestamp: Option<NodeInstant>,
}
pub struct NodeState {
    best_known_block: Option<BlockId>, last_unknown_block: Option<BlockHash>, last_common_block: Option<BlockId>, best_header_sent: Option<BlockId>,
    unconnecting_headers: i32, sync_started: bool, headers_sync_timeout: Micros, stalling_since: Micros, blocks_in_flight: Vec<QueuedBlock>,
    downloading_since: Micros, preferred_download: bool, prefer_headers: bool, prefer_headers_compressed: bool,
    requested_hb_cmpctblocks: bool, provides_cmpctblocks: bool, chain_sync: ChainSyncTimeoutState, last_block_announcement: i64, is_inbound: bool,
    recently_announced_invs: RollingBloom, /* v24 only */ recent_object_requests: LruCache<Hash256, RequestedObject /*256*/>,
}
pub struct RawMsg { pub cmd: CommandString, pub payload: Bytes, pub recv_time: Micros, pub raw_size: u32 /* payload + 24 (V1) or the V2 frame size */ }
```

The actor also owns: `node_states: BTreeMap<PeerId, (Peer, NodeState)>` (PeerId order is creation order, as Core's `std::map<NodeId>`), `n_sync_started`, `num_preferred_download_peers`, `peers_downloading_from`, `outbound_peers_with_protect_from_disconnect`, `last_tip_update`, `stale_tip_check_time`, `initial_sync_finished`, `last_block_inv_triggering_headers_sync`, `best_height` (from `UpdatedBlockTip`), `best_header` mirror (§4.11.1), `map_blocks_in_flight` (P3), `tracker: Box<dyn RequestTracker>`, `handlers: Vec<Box<dyn NetHandler>>`, `rng: FastRandomContext`.

### 3.4 AddrMan

```rust
// Core: addrman.h/.cpp, addrman_impl.h (byte-identical logic at both pins; v24 adds GetEntries for the hidden getrawaddrman RPC)
pub struct AddrMan(Arc<Mutex<AddrManImpl>>);        // one mutex, as Core's `cs`; every public fn is one critical section with Check() on both sides
impl AddrMan {
    pub fn new(netgroup: Arc<NetGroupManager>, deterministic: bool, consistency_check_ratio: i32) -> AddrMan;
    pub fn add(&self, addrs: &[Address], source: &NetAddr, time_penalty: Seconds) -> bool;      // Add_ 739/.. ; penalty 2 h from `addr`, 0 from seeds
    pub fn good(&self, addr: &Service, time: NodeSeconds) -> bool;                               // Good_(test_before_evict = true)
    pub fn attempt(&self, addr: &Service, count_failure: bool, time: NodeSeconds);              // Attempt_
    pub fn resolve_collisions(&self);  pub fn select_tried_collision(&self) -> Option<(Address, NodeSeconds)>;
    pub fn select(&self, new_only: bool, network: Option<Network>) -> Option<(Address, NodeSeconds)>;
    pub fn get_addr(&self, max_addresses: usize, max_pct: usize, network: Option<Network>, filtered: bool) -> Vec<Address>;
    pub fn connected(&self, addr: &Service, time: NodeSeconds);  pub fn set_services(&self, addr: &Service, s: ServiceFlags);
    pub fn size(&self, net: Option<Network>, in_new: Option<bool>) -> usize;
    pub fn serialize_core(&self) -> Vec<u8>;  pub fn deserialize_core(bytes: &[u8], …) -> Result<AddrMan, AddrManFileError>;   // V4_MULTIPORT, import and export
    pub fn entries(&self, from_tried: bool) -> Vec<(AddrInfo, AddressPosition)>;                 // v24 getrawaddrman (RP1a); harmless under V23_1
}
```

An `AddrInfo` is Core's: `Address { services, time, addr, port }`, `source: NetAddr`, `last_success`, `last_try`, `last_count_attempt`, `attempts`, `ref_count`, `in_tried`, `random_pos`. Tables are `[[i32; 64]; 256]` (tried) and `[[i32; 64]; 1024]` (new), ids `i32`, `map_info: BTreeMap<i32, AddrInfo>`, `map_addr: HashMap<Service, i32>` (never iterated), `v_random: Vec<i32>`, `tried_collisions: BTreeSet<i32>` (Core iterates a `std::set<int>` in order in `ResolveCollisions_` and indexes it by `std::advance` in `SelectTriedCollision_`; a `BTreeSet` reproduces both). `n_key` is a 32-byte random key (deterministic mode: `uint256{1}`). The RNG is a `FastRandomContext` port (ChaCha20-based, `randbits`/`randrange`/`randbool`/`rand256` with Core's rejection sampling), so a deterministic-mode `AddrMan` reproduces Core's `addrman_tests` byte-for-byte.

### 3.5 NetGroupManager

```rust
pub struct NetGroupManager { asmap: Vec<bool> /* empty until P8 */ }
impl NetGroupManager {
    pub fn get_group(&self, a: &NetAddr) -> Vec<u8>;   // netgroup.cpp:18-80 both pins (v23 has no ASMapHealthCheck): asmap ⇒ [NET_IPV6, asn LE 4 bytes]; local ⇒ [class]; internal ⇒ class + 10 bytes after the 6-byte prefix; unroutable ⇒ [class]; IPv4/mapped ⇒ [class, a, b]; Tor/I2P ⇒ 4 bits; CJDNS ⇒ 12 bits; he.net ⇒ /36; other IPv6 ⇒ /32, with Core's "push nBits then fill the rest of the byte with 1s" tail
    pub fn get_mapped_as(&self, a: &NetAddr) -> u32;   // 0 without asmap
    pub fn asmap_checksum(&self) -> Hash256;           // uint256() when empty (peers.dat field)
}
```

### 3.6 Seams

#### 3.6.1 P1: `dash-p2p-transport`

```rust
pub trait Transport: Send + 'static {                               // A1 Appendix A, generalised
    fn received_bytes(&mut self, buf: &mut &[u8]) -> Result<(), TransportFatal>;   // consumes from buf; Err ⇒ disconnect (V1: bad magic / size; V2: key or garbage violation)
    fn received_message_complete(&self) -> bool;
    fn take_received(&mut self, time: Micros) -> ReceivedMsg;                    // Core GetReceivedMessage 780/..: { cmd, payload: Bytes, raw_size, reject: Option<RejectReason /*Checksum|Command*/> }
    fn set_message_to_send(&mut self, msg: &SerializedMsg) -> bool;             // false while the V2 handshake is in progress
    fn bytes_to_send(&mut self, have_next: bool) -> (&[u8], bool /*more*/, &str /*msg type or ""*/);
    fn mark_bytes_sent(&mut self, n: usize);
    fn should_reconnect_v1(&self) -> bool;                                       // V2Transport 1647: initiator, RecvState::KEY, empty recv buffer, sent ≥ a v1 header's worth
    fn info(&self) -> TransportInfo { kind: Detecting|V1|V2, session_id: Option<[u8;32]> };
    fn set_peer_version(&mut self, v: i32);                                      // V2 short-ID send gate (≥70235; platformban ≥70240) — A1 §2.14
    fn send_memory_usage(&self) -> usize;
}
```

P2a never decodes a payload inside `Transport`; it classifies `reject` (§4.7.1) and queues `RawMsg`.

#### 3.6.2 P2b: inbound

`ConnMan` exposes `fn accept(&self, sock, permissions, addr_bind, inbound_onion)` as an unimplemented hook; `Conn::new_inbound` reuses `Conn`. The actor's VERSION handler already contains Core's inbound branches (`PushNodeVersion` after the peer's version, `CheckIncomingNonce`, `SeenLocal(addrMe)`, `fOtherMasternode` disconnect on a non-MN) because they are interleaved with the outbound code; P2b enables them by constructing inbound `Conn`s and adds the `getheaders`/`getaddr` serving branches and `MaybeSendAddr`'s inbound-only pieces. The `GetAddresses(requestor)` cache (21–27 h, keyed by network, local socket bytes and, for inbound, the bound port) lives in P2a's `ConnMan` so that P2b only calls it.

#### 3.6.3 P5 / P5a: request tracker and handlers

```rust
pub trait RequestTracker: Send {                                 // P5 provides Legacy and TxRequest; P2a ships NoRequests
    fn add_announcement(&mut self, from: &PeerCtx, inv: Inv, now: Micros);           // AddObjectAnnouncement 1635 / RequestObject 1680 (v23)
    fn requestable(&mut self, peer: PeerId, now: Micros, expired: &mut Vec<(PeerId, Inv)>) -> Vec<Inv>;
    fn requested(&mut self, peer: PeerId, inv: &Inv, expiry: Micros); fn received_response(&mut self, peer: PeerId, inv: &Inv);
    fn forget(&mut self, inv: &Inv); fn disconnected_peer(&mut self, peer: PeerId); fn count(&self, peer: PeerId) -> usize; fn size(&self) -> usize;
}
pub struct NoRequests;   // M2: records nothing, requests nothing (O13a masks `getdata`; §1.3)

pub trait NetHandler: Send + Sync + 'static { /* A1 §2.14 signature */ }
```

Core's dispatch tail (5765-5830 / 5621-5686): a command present in `getAllNetMessageTypes()` but with no registered handler is **silently ignored** (no log line); an unknown command logs `Unknown command "%s" from peer=%d` and is ignored. P2a reproduces both: `CommandString::is_known()` (base-sdk's 74-name table) decides which branch, and the `handlers` vector is empty at M2 except for the core handler.

#### 3.6.4 SP0 / SP1: sync chatter

`PeerManager` exposes `fn fully_connected_peers(&self) -> Vec<PeerCtx>` (Core's `NodesSnapshot(FullyConnectedOnly)`), `fn push(&self, peer: PeerId, msg: P2pMsg)` and a scheduler hook `fn schedule_every(&self, period, f)` on `dashd-node`'s scheduler. `SyncManager` (SP1) runs its 1 s scheduler job with the 6 s `MASTERNODE_SYNC_TICK_SECONDS` gate (`sync_manager.cpp:136-150`, `masternode/sync.h:22`) and pushes `getsporks`/`govsync`/`mempool` through `push`. Pushes from the scheduler thread must land in the peer's send queue **outside** the actor's `SendMessages`, exactly as Core's scheduler thread calls `PushMessage` concurrently with `msghand` (it is `cs_vSend`-serialised, not ordered relative to `SendMessages`); P2a's `SendQueue` is therefore an `Mutex<VecDeque>` that any thread may append to, and ordering between a scheduler push and the actor's own pushes is whichever thread wins, as in Core.

#### 3.6.5 P4: ban view

```rust
pub trait BanView: Send + Sync { fn is_banned(&self, a: &NetAddr) -> bool; fn is_discouraged(&self, a: &NetAddr) -> bool; fn discourage(&self, a: &NetAddr); }
```
P2a's outbound path calls it at `OpenNetworkConnection` (3608/3614) and in the ADDR handler (4446/4401); `MaybeDiscourageAndDisconnect` calls `discourage`. M2 ships `NoBans` (never banned; `discourage` records into a 50,000-entry rolling bloom so that behaviour 12 already holds for outbound peers).

#### 3.6.6 P7 / AM1: masternode connections

Reserved in P2a: `MnFlags`, `MasternodeConn`/`MasternodeProbeConn` parameters of `open_network_connection`, the MN send throttle (§4.7.3), `sent_mnauth_challenge` (set in `PushNodeVersion`), `received_mnauth_challenge` (set in the VERSION handler), the `mnauth` first-message rule for probes (4367-4378 / 4322-4333), the `CanRelay()` predicate (`!masternode_connection || masternode_iqr_connection`, `net.h:1083`), and the `mn_sync.is_blockchain_synced()` and `mn_list.at_tip()` views. The `mncon` task itself (3397/3403) is P7.

#### 3.6.7 RP1a: `getpeerinfo`

`ConnMan::node_stats()` returns `NodeStats` = Core's `CNodeStats` (`CopyStats` 596/595): `id, addr, addr_bind, network, last_send, last_recv, last_tx_time, last_block_time, connected, time_offset, addr_name, version, clean_subver, inbound, bip152_hb_to/from, send_bytes_per_msg, send_bytes, recv_bytes_per_msg, recv_bytes, transport_type, session_id, permissions, last_ping_time, min_ping_time, addr_local, verified_pro_reg_tx_hash, verified_pubkey_hash, masternode_connection, conn_type`. `PeerManager::node_state_stats(id)` returns `CNodeStateStats` (1877/1897): `misbehavior_score, sync_height, common_height, starting_height, ping_wait, heights_in_flight, relay_txs, addr_processed, addr_rate_limited, addr_relay_enabled, their_services`. Byte accounting: `recv_bytes_per_msg[cmd] += raw_size` for known commands, `*other*` for unknown and for rejected (bad-checksum) messages (`ReceiveMsgBytes` 666-681 / 665-680); `send_bytes_per_msg[cmd] += n` per `send()` call excluding V2 handshake bytes (`SocketSendData` 1734-1738). The functional framework asserts `pong` bytes ≥ 29 in `bytesrecv_per_msg` (A1 §4.8): 24 + 8 = 32 for V1.

#### 3.6.8 CH1: chain commands and views (interface dependency)

```rust
// dashd-chainstate (CH1) — requested additions, see Q3
pub enum ChainCommand { ProcessHeaders { headers: Vec<BlockHeader>, from: Option<PeerId>, reply: oneshot::Sender<HeadersResult> }, … }
pub struct HeadersResult {
    pub prev_known: bool,              // LookupBlockIndex(headers[0].hashPrevBlock) != nullptr, evaluated on the validation thread BEFORE acceptance (3334/3350)
    pub last_was_new: bool,            // LookupBlockIndex(headers.back().GetHash()) == nullptr before acceptance (3358/3374)
    pub outcome: Result<HeaderCtx /* pindexLast */, ValidationError /* BlockValidationResult + reason */>,  // ProcessNewBlockHeaders 3360/3376
    pub best_header: BlockId,          // m_chainman.m_best_header after the call
    pub tip_chain_work: Arith256, pub tip: BlockId, pub minimum_chain_work: Arith256, pub ibd: bool, pub loading_blocks: bool,
}
```

P2a also reads `ChainView` (`tip`, `best_header`, `height`, `ibd`, `index: Arc<BlockIndex>` for ancestry) and subscribes to `ChainEvent::{UpdatedBlockTip, BlockConnected, BlockDisconnected, NewPoWValidBlock, BlockChecked}`. Locators are built by `chain::locator(index, from: BlockId)` over the arena's immutable `prev`/`skip` links (`chain.cpp:32-54` v24 `LocatorEntries`, v23 `CChain::GetLocator`; identical output: step 1 doubling after 10 entries, genesis last). `ActiveChain().Contains(pindex)` checks (3238, 3246) use `ChainView.active` at the epoch of the awaited reply.

---

## 4. Core equivalence: the observable behaviour catalogue

### 4.0 What differs between the pins in P2a's scope

Every P2a function was diffed between the tags (`/work/scratch/p2a-fable`, §12 reproduces it). Behaviour-changing differences:

| # | Difference | Keyed | Where P2a reads it |
|---|---|---|---|
| 1 | `PROTOCOL_VERSION` 70240→70242, UA `23.1.8`→`24.0.0`, `CLIENT_VERSION` (`version.h:14`, `configure.ac:3-6`) | `Identity` (P2P-01) | `PushNodeVersion`, `greatest_common_version`, `Fingerprint::user_agent`, stats |
| 2 | `CanDirectFetch`, sync-start rule, headers-timeout scaling, initial-headers-sync stall: `GetAdjustedTime()` (v23 `:1407, 6225, 6246, 6627`) vs `NodeClock::now()` (v24 `:1430, 6370, 6391, 6785`) | `UngatedRules.time_source` (P2P-16, A1 #4) | §4.11.1, §4.11.5; `IsInitialBlockDownload` uses the **local** clock at both pins (`validation.cpp` v23 `:1704` `GetTime()`, v24 `:1987`) |
| 3 | inv type 34 known at v24 from any peer (`protocol.h:527,557`; `IsKnownType`), unknown at v23 (`:526`) | `P2pRules.msg_asset_unlock` (P2P-04) | INV handler's "got inv of unknown type" branch and the `getdata` batch cut (P4/P5) |
| 4 | `notfound` cap 116 (v23 `:5602`) vs 50,000 (v24 `:5752`) | `P2pRules.notfound_limit` (P2P-10) | NOTFOUND handler |
| 5 | `InitBinds`: any-success (v23 `net.cpp:3963-3983`) vs all-or-abort with `::` exempt (v24 `:3957-3989`) | `InitRules.bind_failure_fatal` (P2P-33) | `ConnMan::start` via `Options` (P2b implements the binds) |
| 6 | `m_version_services`/`m_version_relay_txs` recorded (v24 `net.h:820-823`, `net_processing.cpp:4143-4144`) | none needed (DKG-01 reads them; recording is unobservable) | VERSION handler always records |
| 7 | `TryMarkVerified` cap (P2P-18), `mncon` thread gating (#7659), ASMap health log, `GetEntries` | P7 / unobservable / RP1a | — |

Everything else in §4 holds verbatim at both pins; the v23 cite is given for each function.

### 4.1 Opening outbound connections (`ThreadOpenConnections` 2917/2923)

The `outbound` task is a faithful port; its pseudo-code is Core's control flow, and the observable rules are:

1. **`-connect` mode** (2923-2944 / 2929-2950): if `specified_outgoing` is non-empty, loop forever over the list opening `Manual` connections (v2 if we advertise `NODE_P2P_V2`), sleeping 500 ms × min(10, loop) between attempts, then 500 ms and `PerformReconnections`. No addrman, no seeds, no feelers. `-connect=0` alone (`specified_outgoing` empty, `use_addrman_outgoing` false) starts no `outbound` task at all (`Start` 4091-4096 / 4085-4090).
2. **Timers**: `next_feeler = exp(start, 2 min)`, `next_extra_block_relay = exp(start, 5 min)`, `next_extra_network_peer = exp(start, 5 min)` (`GetExponentialRand`, §4.17.4).
3. **Loop body** (2963-2969): `ProcessAddrFetch()`; sleep 500 ms; `PerformReconnections()`; **acquire an outbound slot** (`semOutbound`, size `min(8 + 2 + 1, max_connections)`); if interrupted, return.
4. **Fixed seeds** (2974-3010): only when `fixedseeds` is on and some reachable network has an empty addrman. Add them when (a) 60 s have passed since start (`start + 1min`), or (b) `-dnsseed=0` and no `-seednode` and no `-addnode` (immediately). Seeds come from `ConvertSeeds(Params().FixedSeeds())` (`net.cpp:207-224`): each BIP155 `CService` becomes a `CAddress` with services `GetDesirableServiceFlags(NODE_NONE)` (= `NODE_NETWORK`) and a time uniform in `[now − 7 d, now)`; entries whose network is not in the empty set are dropped; source is the internal address `"fixedseeds"`; `fixedseeds` is then cleared. The seed bytes come from `dash-params` (BS5) and must equal Core's `chainparamsseeds.h` at the pin (FR-P2P-07). Regtest has none; devnets have none.
5. **Slot accounting** (3015-3058): count `nOutboundFullRelay` (full-relay, **not** MN connections), `nOutboundBlockRelay`, `nOutboundOnionRelay`, and the netgroups of `Manual|OutboundFullRelay|BlockRelay` peers (inbound, feeler and addr-fetch excluded; Tor/I2P/CJDNS count as `outbound_privacy_network_peers` instead) **only when `!AllowMultipleAddressesFromGroup()`** (mainnet and testnet; devnet and regtest skip the netgroup rule entirely: `chainparams.cpp` 294/497 vs 689/895, v23 283/476 vs 658/857). Also collect the set of verified ProRegTx hashes of connected peers (3060-3068).
6. **Connection type decision**, in this order (3081-3146 / 3087-3152): anchors first (`BlockRelay`, while `nOutboundBlockRelay < max_block_relay` and anchors remain) → full-relay until `max_outbound_full_relay` → block-relay until `max_outbound_block_relay` → `try_new_outbound_peer` (stale tip; full-relay) → extra block-relay when `now > next_extra_block_relay && start_extra_block_relay_peers` (reschedule the timer) → feeler when `now > next_feeler` (reschedule; `fFeeler`) → network-specific extra when full-relay is saturated **and** `max_outbound_full_relay == 8` and `now > next_extra_network_peer` and `MaybePickPreferredNetwork` finds a reachable network with no outbound/manual peer and a non-empty addrman (reschedule) → onion-only when `nOutboundOnionRelay < max_outbound_onion` and onion is reachable → otherwise `continue` (release the slot and loop).
7. `addrman.resolve_collisions()` (3148), then the MN list at the tip (3150; P2a uses `MnListView::at_tip()`, which returns an empty list until EV/CH land, so the MN de-duplication filter at 3250-3253 never fires in M2).
8. **Candidate loop** (3152-3257), at most 100 tries: anchors pop from the back and are accepted if valid, not local, reachable, `HasAllDesirableServiceFlags`, and not in a used netgroup; otherwise `Select`: feelers try `SelectTriedCollision()` first, and if that address is already connected call `Good(addr)` and fall back to `Select(new_only=true)`; non-feelers `Select(false, preferred_net)`. Rejections, in Core's order: netgroup already used (non-feelers); invalid → break; our own listen address → break; unreachable; `onion_only && !IsTor`; tried within 10 min unless `nTries ≥ 30`; services (non-feeler needs all desirable; feeler needs `MayHaveUsefulAddressDB` = NETWORK or NETWORK_LIMITED); bad port unless `nTries ≥ 50` (`IsBadPort`, `netbase.cpp`); an `-addnode` address; a connected masternode's service address.
9. **Before connecting** (3259-3291): feelers sleep uniform `[0, 1 s)` (`FEELER_SLEEP_WINDOW`); `count_failures = (netgroups + privacy peers) ≥ min(max_connections − 1, 2)`; `use_v2 = addr.services & local_services & NODE_P2P_V2`; `OpenNetworkConnection(addr, count_failures, grant, None, kind, use_v2)`.

Observable consequences reproduced: a fresh node with an empty `peers.dat` dials its first peers from DNS (§4.3) within seconds and fixed seeds only after 60 s; the two block-relay slots fill after the eight full-relay slots (ordering visible to a probe that watches which services/relay flag the `version` carries: block-relay connections set `relay=false`, §4.6.2); one feeler every ~2 min (Poisson) that disconnects right after receiving `version` (§4.6.4); an extra block-relay peer every ~5 min once `CanDirectFetch()` first held (§4.11.5); the netgroup diversity rule and the "100 tries then sleep" cadence.

### 4.2 The connection attempt (`ConnectNode` 372/371, `OpenNetworkConnection` 3586/3592)

- `OpenNetworkConnection`: return if interrupted or network inactive; for an address: skip if banned or discouraged (`BanView`) or already connected to that `CService` (`AlreadyConnectedToAddress` compares `addr` as `CService`, i.e. ip:port) or it is our listen address; for a hostname: skip if a node with that `addr_name` exists. Log, `ConnectNode`, set the slot grant and MN flags, `InitializeNode` (which sends `version`, §4.6), insert into `m_nodes`, bump `network_conn_counts` for manual/full-relay.
- `ConnectNode`: hostname resolution with `Lookup(dest, default_port, name_lookup && !proxy, 256)`, shuffled; the first resolved address becomes `addrConnect` and **any** resolved address already connected aborts (415-425). The TCP connect uses `-timeout` (default 5000 ms; `netbase.cpp:587`, `netbase.h:25`). **`addrman.attempt(addr, count_failure)` is called after every direct or SOCKS attempt that reached the target network** (whether it succeeded or failed), skipped only when the proxy itself failed (488-492). The node id is the next `i64`; the local nonce is `SipHash(RANDOMIZER_ID_LOCALHOSTNONCE).Write(id)` with the per-process randomizer (`GetDeterministicRandomizer` 4968). `addr_bind` is the socket's local address. `keyed_netgroup = SipHash(RANDOMIZER_ID_NETGROUP).Write(GetGroup(addr))` (4973).
- The connection is created with the transport chosen by `use_v2` (`MakeTransport` 4724/4683): `V2Transport(initiating=true)` or `V1Transport`, both at `INIT_PROTO_VERSION` 209.

### 4.3 DNS seeds (`ThreadDNSAddressSeed` 2680/2686)

- Seeds: `Params().DNSSeeds()` shuffled. Mainnet: `dnsseed.dash.org.` (one seed; `chainparams.cpp:259` / `:250`); testnet: `testnet-seed.dashdot.io.` (`:463` / `:444`); devnet: none; regtest: `dummySeed.invalid.`.
- If `-forcednsseed` or `addrman.Size() == 0`: query all seeds at once (`seeds_right_now = seeds.size()`). Otherwise, before each batch of 3 (`DNSSEEDS_TO_QUERY_AT_ONCE`), wait `11 s` if `Size() < 1000` else `5 min` (`DNSSEEDS_DELAY_*`, `net.cpp:87-89`), waking every 11 s to stop entirely once ≥ 2 full-outbound peers (non-probe) have completed the handshake (`fSuccessfullyConnected`): log `P2P peers available. Skipped DNS seeding.` / `Finished DNS seeding.`
- Per seed: if a name proxy is set, `AddAddrFetch(seed)`; else resolve `x<hex services>.<seed>` with `GetDesirableServiceFlags(NODE_NONE)` = 1 → host `x1.dnsseed.dash.org.`, at most 32 results (`LookupHost(host, 32, true)`); each becomes a `CAddress` with services 1, the default port, and a time uniform in `[now − 7 d, now − 3 d)`; `addrman.add(vAdd, source = internal(host))`. An empty result falls back to `AddAddrFetch(seed)` (an `ADDR_FETCH` connection to the bare seed name).
- Log lines reproduced verbatim (`Loading addresses from DNS seed %s`, `%d addresses found from DNS seeds`, `Waiting %d seconds before querying DNS seeds.`).

### 4.4 `-addnode` (`ThreadOpenAddedConnections` 3368/3374) and `-seednode`

- Addnode: loop; take the `semAddnode` grant (8); `GetAddedNodeInfo(include_connected=false)` (unconnected entries only, resolved with `LookupNumeric` against the default port; a name entry matches by `addr_name`); open each as `Manual` with `use_v2 = local NODE_P2P_V2` (set at `Init` 1282-1288 for `-addnode`; RPC `addnode … v2transport` chooses per entry), sleeping 500 ms between; `PerformReconnections()`; then sleep 60 s if anything was tried, else 2 s. Manual peers are exempt from discouragement (`MaybeDiscourageAndDisconnect` 5851-5855) and from `ExpectServicesFromConn` (`net.h:941`).
- `-seednode` entries are queued as addr-fetches at `Start` (4020-4022) and dialled from `ProcessAddrFetch` (2806/2812) with a *try* grant on the outbound semaphore: an `ADDR_FETCH` connection that sends `version`, gets `getaddr` sent after the peer's version (§4.6.2), and disconnects after an `addr` with more than one entry (§4.10.1) or after `10 × 30 s = 5 min` (`SendMessages` 6323-6327 / 6177-6181).

### 4.5 Reconnection with v1 (`PerformReconnections` 4980/4939, `DisconnectNodes` 2163-2176 / 2169-2182)

When a disconnected connection's transport says `ShouldReconnectV1()` (initiator, still in `KEY` state with nothing received, and we sent at least a V1 header's worth), the connection is re-queued with the same grant, destination, type and MN flags, `use_v2 = false`, and `count_failure = false`. `PerformReconnections` runs from the `outbound` and `addnode` tasks' loops (every 500 ms / 60 s). Observable: a v2-capable peer that gets a V2 key dump from us and closes sees a second TCP connection carrying a plain V1 `version` shortly after.

### 4.6 Handshake

#### 4.6.1 Our service bits (`init.cpp` 1125-1245, 1371, 2354 / 1077-1197, 1387, 2314)

`NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED` (0x0C00) always; `| NODE_P2P_V2` (0x1000) if `-v2transport` (default true); `| NODE_COMPACT_FILTERS` (0x40) if `-peerblockfilters` (needs the basic filter index; InitError otherwise); `| NODE_BLOOM` (0x04) if `-peerbloomfilters` (default true); `| NODE_NETWORK` (0x01) when not pruning. Defaults: node `0x1C05`, masternode `0x1C45` (MN soft-sets `peerblockfilters=1`), pruned `0x1C04`. O17 P2P-35: under `V23_1` an explicit `peerblockfilters=0` on an MN is accepted and yields `0x1C05`; under `V24_0` it is an InitError. That is ND1's `InitRules.mn_init`; P2a just takes `local_services` from `Options`.

The **desirable** flags (`protocol.cpp:252-261`, both pins): `NODE_NETWORK_LIMITED` when the candidate has it and the IBD latch has completed (`SetServiceFlagsIBDCache(!fInitialDownload)` from `UpdatedBlockTip` 2275/2282), else `NODE_NETWORK`. `HasAllDesirableServiceFlags(s) = !(desirable(s) & !s)`; `MayHaveUsefulAddressDB(s) = s & (NETWORK|NETWORK_LIMITED)`.

#### 4.6.2 Our `version` (`PushNodeVersion` 1561-1593 / 1538-1572)

Sent from `InitializeNode` for outbound connections (1758-1760 / 1792-1794), i.e. **immediately at connect**, before any byte is received (behaviour 3). Serialised with `CNetMsgMaker(INIT_PROTO_VERSION = 209)`: `nVersion` (i32: `Identity.protocol_version`, or `-pushversion` on non-mainnet), `my_services` (u64), `nTime` (i64, `GetTime()` — the mockable node clock, **not** adjusted time), `addr_you` as pre-31402 `CAddress` = `your_services` (u64, the services recorded for the address we dialled) + `addr_you` (16 + 2, = the peer's address if routable, not a proxy and v1-compatible, else `CService()` = `::`/0), `addr_me` = `my_services` + `CService()`, `nonce` (u64 local nonce), `strSubVersion` (var-str), `nNodeStartingHeight` (i32 `m_best_height`, which is updated by `UpdatedBlockTip`, so a `version` built between a connect and the queued callback carries the pre-connect height, as in Core), `tx_relay` (bool = `!RejectIncomingTxs(pnode)`: false for block-relay and feeler connections and in `-blocksonly` without `relay` permission), `mnauthChallenge` (32 random bytes, also stored as `sent_mnauth_challenge`), `fMasternode` (bool = `m_masternode_connection`). Size: 119 + UA length, 137 with the stock 18-byte UA at either pin (behaviour 1). Log line: `send version message: version %d, blocks=%d, them=%s, txrelay=%d, peer=%d`.

#### 4.6.3 Receiving `version` (3935-4175 / 3906-4146)

In Core's exact order, each step with its outcome:

1. `nVersion != 0` → log `redundant version message from peer=%d`, return (no penalty; nothing parsed).
2. Decode the mandatory head: `nVersion` i32, `nServices` u64, `nTime` i64 (negative → 0), skip 8, `addrMe`. A decode failure here throws → caught in `ProcessMessages` → logged `Exception '…' caught`, no penalty, peer stays pre-version (BS4a verified).
3. Outbound only: `addrman.set_services(peer.addr, nServices)` (3961 / 3932).
4. `ExpectServicesFromConn() && !HasAllDesirableServiceFlags(nServices)` → log `peer=%d does not offer the expected services (%08x offered, %08x expected); disconnecting`, `fDisconnect`, return.
5. `nVersion < 70221` → log `peer=%d using obsolete version %i; disconnecting`, `fDisconnect`, return (signed compare: −1 disconnects).
6. Optional tail, each `if (!vRecv.empty())`: skip 26 + `nNonce`; `LIMITED_STRING(256)` subver → `SanitizeString`; `starting_height`; `fRelay` (`CoreBool`); `receivedMNAuthChallenge` (stored); `fOtherMasternode` (`CoreBool`; inbound only: set `m_masternode_connection`, and if we are not a masternode, `fDisconnect` + return). A truncated field throws (step 2's outcome) but **steps 3–5 have already applied** (BS4a `VersionTail`).
7. Inbound: self-connection nonce check → `connected to self at %s, disconnecting`; `SeenLocal(addrMe)` if routable; `PushNodeVersion` (behaviour 3, inbound half; P2b).
8. Devnet: `cleanSubVer` must contain `devnet.<name>` else `Misbehaving(100, "wrong-devnet-name")` for outbound, `(1, …)` inbound, log `connected to wrong devnet. Reported version is %s, expected devnet name is %s`, `fDisconnect` (behaviour 27).
9. `common = min(nVersion, PROTOCOL_VERSION)`; `SetCommonVersion`; `nVersion` stored. **Send `sendaddrv2` iff `common ≥ 70223`** (4051-4057 / 4022-4028), serialised at `common`.
10. Record `m_has_all_wanted_services`, `m_version_services`, `m_version_relay_txs` (v24), `their_services`, `addr_local = addrMe`, `cleanSubVer`, `starting_height`.
11. `SetTxRelay()` iff not block-relay, not feeler, and (`fRelay` or we offer `NODE_BLOOM`); `m_relay_txs = fRelay`; `m_relays_txs = fRelay`.
12. `sendtxrcncl` only if `-txreconciliation` (default off) — never in rdashd by default (behaviour 11).
13. **Send `verack`** (4103 / 4074).
14. `fPreferredDownload = (outbound || noban) && !addr_fetch && CanServeBlocks(their_services)`; `++num_preferred_download_peers`.
15. Outbound: `SetupAddressRelay` (false for block-relay) → **send `getaddr`**, `getaddr_sent = true`, `addr_token_bucket += 1000` (4119-4132 / 4090-4103). So full-relay, manual, **feeler and addr-fetch** connections all send `getaddr` here; block-relay never does (behaviour 4).
16. Outbound: `addrman.good(peer.addr)` (4149 / 4120) — for every outbound type including block-relay and feelers.
17. Log `receive version message: %s: version %d, blocks=%d, us=%s, txrelay=%d, peer=%d%s%s`.
18. `nTimeOffset = nTime − GetTime()`; outbound: `AddTimeData(addr, offset)` (`timedata.cpp:38-93`, identical logic at both pins; only the return type of `GetAdjustedTime` changed): one sample per distinct address, max 200, median over an odd count ≥ 5, applied iff `|median| ≤ -maxtimeadjustment` (70 min) else offset 0 and the one-time clock warning. Under `V23_1` this offset feeds §4.11's clock (P2P-16); under `V24_0` it only feeds `getpeerinfo.timeoffset` and the warning.
19. Feeler: log `feeler connection completed peer=%d; disconnecting`, `fDisconnect` (4170-4174 / 4141-4145). The queued `sendaddrv2`, `verack` and `getaddr` are still flushed by the 100 ms linger (§4.14), so a feeler's transcript is `version → (peer version) → sendaddrv2, verack, getaddr → FIN` (behaviours 4, 25).

#### 4.6.4 Receiving `verack` (4187-4259 / 4156-4214)

Gate: `nVersion == 0` → `non-version message before version handshake` log, ignore (applies to **every** command). Redundant verack → `ignoring redundant verack message`, ignore. Then, in order: the `New %s %s peer connected: version: %d, blocks=%d, peer=%d…` log (unconditional for outbound); `mnauth` if we are a masternode and this is not a probe (AM1); **`sendheaders2` if `their_services & NODE_HEADERS_COMPRESSED` else `sendheaders`** (`UsesCompressedHeaders` 1196/1180; exactly one of them); `sendcmpct(hb=false, version=1)` iff `CanRelay()`; if `!RejectIncomingTxs`: `senddsq(true)`, then `qwatch` iff watching quorums and the peer is a quorum node (P7); the txreconciliation cleanup (no-op); `fSuccessfullyConnected = true`. The `ping` and `getheaders(2)` of behaviour 5 come from the **next `SendMessages` pass** for this peer (§4.8), which in Core runs in the same `ThreadMessageHandler` iteration right after `ProcessMessages` returned, so they follow `senddsq` on the wire without any other message in between.

#### 4.6.5 Between `version` and `verack` (4261-4362 / 4216-4317)

Accepted before `fSuccessfullyConnected`: `sendheaders` (`fPreferHeaders = true`), `sendheaders2` (`fPreferHeadersCompressed = true`), `sendcmpct` (ignored unless `version == 1`; sets `m_provides_cmpctblocks`, `m_requested_hb_cmpctblocks`, `m_bip152_highbandwidth_from`), `sendaddrv2` (ignored below 70223; **after verack → `fDisconnect`**; else `m_wants_addrv2`), `sendtxrcncl` (with reconciliation off: logged and ignored, before or after verack). Anything else before verack: log `Unsupported message "%s" prior to verack from peer=%d`, ignore, no penalty (behaviour 6). These five handlers also work after verack with the same effects (except the two that disconnect).

#### 4.6.6 First message after the handshake (4367-4378 / 4322-4333)

`nTimeFirstMessageReceived` is set on the first post-verack message; `fFirstMessageIsMNAUTH` records whether it was `mnauth`; a probe whose first message is not `mnauth` is disconnected. Then the blocks-only violation check (§4.13).

### 4.7 The message loop

#### 4.7.1 Reader tasks (Core `ThreadSocketHandler` 2649/2655, `SocketRecvData` 2592, `ReceiveMsgBytes` 649/648)

Per connection, one tokio reader task reads up to 64 KiB per `recv`, sets `last_recv` and `recv_bytes` on every chunk, feeds `Transport::received_bytes`. Outcomes:

| Transport outcome | Core | Rust |
|---|---|---|
| V1: header fails to deserialise, wrong magic, `size > MAX_SIZE or > 3 MiB` (`readHeader` 708-746) | `-1` → `ReceivedBytes` false → `CloseSocketDisconnect` (no linger: the socket is closed at once, `fDisconnect` set) | `TransportFatal` → close immediately, mark `other_side_disconnected = false` but skip the linger (Core closes the socket in `CloseSocketDisconnect` 519, and `DisconnectNodes` later finds `m_sock` null) |
| V1: bad checksum (`GetReceivedMessage` 791-797), invalid command bytes (`:798-802`) | `reject_message`: the message is dropped, `*other*` byte accounting, connection kept | drop, account under `*other*`, keep |
| V2: handshake or packet violation | `ReceivedBytes` false → close | same as row 1 |
| complete message | `vRecvMsg.push_back`, per-command accounting, `MarkReceivedMsgsForProcessing`, `WakeMessageHandler` | queue `RawMsg` (payload as `Bytes`, zero-copy), account, classify (§4.7.2), `Event::MsgReady` |
| `recv` returns 0 / error | `fOtherSideDisconnected = true`, close | same |

Receive backpressure: `pause_recv = quorum_queue_bytes + normal_queue_bytes > recv_flood_size` (5 MB default) computed after every enqueue and dequeue (`MarkReceivedMsgsForProcessing` 4801 / `PollMessage` 4834, 4848); while paused the reader stops reading the socket (TCP flow control does the rest; `SocketHandlerConnected` 2488-2491). A paused connection keeps counting toward inactivity.

#### 4.7.2 Per-peer queues and `PollMessage` (4807-4850 / 4766-4810) — **Dash-specific, both pins**

Complete messages are split into a **quorum-priority queue** and a normal queue. `IsQuorumPriorityMessage` (`net.h:2053-2070`, same at v23): `qsigshare`, `qbsigs`, `qsigsinv`, `qgetsigs`, `qsigsesann`, `qsigrec`, `clsig`, `isdlock`. `PollMessage` takes from the priority queue first, up to 100 in a row while the normal queue is non-empty (`QUORUM_TO_NORMAL_RATIO`), then one normal message resets the counter; with an empty normal queue, priority messages drain without limit. The `more` flag is "either queue non-empty". **Observable:** a peer that sends `ping` then `clsig` gets the `clsig` processed first, so a `getdata`/`pong` interleaving differs from FIFO; the per-peer order is a fingerprint. P2a implements both queues and the ratio even though at M2 the priority commands have no handler (they are "known, no handler" → ignored, §3.6.3); the ordering still shows through `pong` latency and byte accounting, and P5a/LK1a inherit it.

#### 4.7.3 `ThreadMessageHandler` (3676-3721 / 3682-3727)

```
loop {
  more_work = false
  skip_mn_send = steady_now - last_mn_send < 100 ms   // if not skipped, last_mn_send = steady_now
  for conn in snapshot(all conns, shuffled):             // fresh Fisher–Yates each pass, our RNG
    if conn.disconnect { continue }
    more = process_messages(conn)                        // §4.7.4
    more_work |= more && !conn.pause_send                 // Core: fMoreWork |= (fMoreNodeWork && !pnode->fPauseSend)
    if !skip_mn_send || !conn.masternode_connection { send_messages(conn) }   // §4.8
  if !more_work { wait_until(steady_now + 100 ms, or Event::MsgReady/SendableAgain/wake) }
}
```

Two details A1 §2.14's sketch omits: the `!fPauseSend` factor (a send-paused peer's backlog does not keep the loop spinning), and that the MN throttle timestamp is only reset when the check *passes* (so MN connections are serviced on the first pass at or after each 100 ms boundary, never more often). Wake sources are message arrival, `UpdatedBlockTip` (2311/2318 `WakeMessageHandler`), the writer clearing `pause_send`, and RPC pushes. The actor runs on its own OS thread (A1 §3.1), blocking only on `ChainCommand::ProcessHeaders` replies.

#### 4.7.4 `ProcessMessages` (5872-5947 / 5728-5801)

1. If `getdata_requests` is non-empty → `ProcessGetData` (P2b/P4 serving; empty at M2).
2. `ProcessOrphanTx` (P5; `false` at M2). If `fDisconnect` → return false. If an orphan was processed → return true (skip the poll this pass).
3. If `getdata_requests` still non-empty → return true.
4. If `pause_send` → return false (**no message is polled while the send buffer is over `-maxsendbuffer`**).
5. `PollMessage` → none → false. `msg.SetVersion(common_version)`.
6. `-capturemessages` (P4). `ProcessMessage(cmd, payload, recv_time)` inside a catch-all: any decode or handler error → log `ProcessMessages(%s, %u bytes): Exception '%s' (%s) caught` / `Unknown exception caught`, **no penalty**, continue. Afterwards, `more |= getdata_requests non-empty`; v24 also `more |= orphanage.HaveTxToReconsider(peer)` (P5, profile-neutral here).
7. Return `more`.

Only one message per peer per pass (Core's semantics); fairness across peers comes from the shuffle.

#### 4.7.5 `ProcessMessage` prologue (3918-3935 / 3889-3906, then the gates of §4.6.3–4.6.6)

Log `received: %s (%u bytes) peer=%d`; statsd `message.received.<cmd>`; the VERSION branch; the `nVersion == 0` gate; the VERACK branch; the five pre-verack commands; the `Unsupported message … prior to verack` gate; first-message bookkeeping; the blocks-only violation check; then the per-command handlers. Decoding happens **inside each branch**, after its state gate, through `decode_with_policy(cmd, payload, &ProtoCtx { version: common_version, bls: BlsCtx::for_tip(view) })`; a `P2pDecodeError` is converted by the policy table (§4.15) into Core's outcome for that command.

### 4.8 `SendMessages` (6300-6899 / 6154-6765): emission order for a P2a node

Per peer, per pass, in this order; items marked (later) are owned by other tasks but their *position* is fixed here:

1. `MaybeDiscourageAndDisconnect` (§4.13) → return.
2. Return if not `fSuccessfullyConnected` or `fDisconnect`. **Nothing is sent before verack except what the handshake handlers push.**
3. `ADDR_FETCH` timeout: `connected + 10 × 30 s < now` → `addrfetch connection timeout; disconnecting`.
4. `MaybeSendPing` (§4.9); return if it disconnected.
5. `MaybeSendAddr` (§4.10.3).
6. Under `cs_main`: `m_best_header` default to tip; compute `sync_blocks_and_headers_from_peer` (preferred download, or `CanServeBlocks && !addr_fetch && (no preferred peers || nothing in flight)`); **initial `getheaders`/`getheaders2`** (§4.11.1).
7. Block announcements via `headers2`/`headers`/`cmpctblock`/`inv` for `blocks_for_headers_relay` (6397-6548): at M2 these vectors are filled by `UpdatedBlockTip` (2273/2280) when the tip changes outside IBD — on a headers-only node the tip never changes after start, so nothing is announced; the code path is ported now because `fRevertToInv` and `PeerHasHeader` depend only on NodeState. (P3 lights it up.)
8. Inventory: `blocks_for_inv_relay` → `inv` batches of ≤ 50,000; tx trickle and the `mempool` reply (P5); the non-tx `vInventoryOtherToSend` path (P5a); one final `inv` flush.
9. Stalling and block-download timeouts (P3); **headers-sync timeout** (§4.11.5); `ConsiderEviction` (§4.11.7).
10. `getdata` for blocks (P3).
11. `getdata` for objects from the `RequestTracker` (`NoRequests` at M2) in batches of ≤ 1000 (`MAX_GETDATA_SZ`).

### 4.9 Ping (`MaybeSendPing` 6148-6184 / 6002-6038; PING 5456 / 5379; PONG 5474 / 5397)

- Timeout: when inactivity checks are active for the peer (`connected + peertimeout < now`) and a ping is outstanding for more than 20 min (`TIMEOUT_INTERVAL`) → log `ping timeout: %fs peer=%d`, `fDisconnect` (behaviour 26).
- Send a `ping` when `m_ping_queued` (RPC `ping`) or when no ping is outstanding and `now > ping_start + 2 min`. **`ping_start` is 0 for a new peer, so the first `SendMessages` after verack always sends a `ping`** (behaviour 7, "immediate ping"). Nonce: a non-zero random u64. The ping goes out after `MaybeDiscourageAndDisconnect` and before `MaybeSendAddr` and `getheaders`, which fixes the post-verack order `[mnauth] → sendheaders2|sendheaders → [sendcmpct] → [senddsq] → [qwatch] → ping → [addr] → getheaders2|getheaders` (behaviour 5). `addr` appears only if there is something to send at that moment (normally not).
- PING handler: echo `pong(nonce)` immediately (the reply is queued inside `ProcessMessage`, so it precedes anything `SendMessages` adds in the same pass).
- PONG handler: `nAvail ≥ 8` → read nonce; matching → `PongReceived(ping_end − ping_start)` (`last_ping_time`, `min_ping_time`), finished; mismatch → `Nonce mismatch` (nonce 0 → `Nonce zero`, finished); none outstanding → `Unsolicited pong without ping`; `nAvail < 8` → `Short payload`, finished. Problems are logged (`pong peer=%d: %s, %x expected, %x received, %u bytes`), never scored. `ping_wait` for `getpeerinfo` is `now − ping_start` while a nonce is outstanding.

### 4.10 Addresses

#### 4.10.1 `addr` / `addrv2` (4388-4483 / 4343-4438)

Decode the vector (`addrv2` with the BIP155 stream flag; BS10 owns field canonicalisation, §4.15). Then: `SetupAddressRelay` false (block-relay-only peer) → log `ignoring %s message from %s peer=%d`, return. **Size > 1000 → `Misbehaving(20, "%s message size = %u")`, return** (the whole message is dropped, after decoding). Token bucket: refill `min(bucket + elapsed_s × 0.1, 1000)` only if below 1000, stamp `addr_token_timestamp`. Shuffle the vector (observable only statistically). For each address: if the bucket is below 1.0 and the peer lacks the `addr` permission → count as rate-limited and skip, else take one token; drop unless `MayHaveUsefulAddressDB || HasAllDesirableServiceFlags`; sanitise time (`≤ 100,000,000` or `> now + 10 min` → `now − 5 d`); `AddAddressKnown`; skip banned/discouraged; `++num_proc`; **relay to 1–2 peers iff `time > now − 10 min && !getaddr_sent && vAddr.size() ≤ 10 && routable`** (§4.10.2); keep if reachable. Then `addr_processed += num_proc`, `addr_rate_limited += num_rate_limit`, log `Received addr: %u addresses (%u processed, %u rate-limited) from peer=%d`, `addrman.add(ok, peer.addr, penalty = 2 h)`, `if size < 1000 { getaddr_sent = false }`, and **`ADDR_FETCH` with `size > 1` → `addrfetch connection completed peer=%d; disconnecting`** (behaviours 15, 25).

#### 4.10.2 `RelayAddress` (2686-2734 / 2665-2713)

`!reachable && !IsRelayable()` → return. `nRelayNodes = 2` if reachable or the SipHash(`RANDOMIZER_ID_ADDRESS_RELAY` 0x3cac0035b5866b90, `hash_addr`, `time_addr = (now_s + hash_addr) / 86400`) is odd, else 1. Over all peers with `addr_relay_enabled`, excluding the originator and addrv2-incompatible peers, keep the top-`nRelayNodes` by `SipHash(hasher).Write(peer_id)`; `PushAddress` to each. `PushAddress` (1165/1149): valid, not in `addr_known`, compatible; if `addrs_to_send.len() ≥ 1000` overwrite a random slot else push.

#### 4.10.3 `MaybeSendAddr` (6186-6271 / 6040-6125)

Only for `addr_relay_enabled` peers. **Self-announcement**: if listening and not in IBD and `next_local_addr_send < now`: reset `addr_known` if this is not the first time, `GetLocalAddrForPeer` (`net.cpp:250-271`: our best local address, or the peer's view of us (`addrLocal`) with probability 1/2, or 1/8 when our own score is above `LOCAL_MANUAL`), push it with `peer.m_our_services` and `now`; `next_local_addr_send = exp(now, 24 h)`. Then: return if `now ≤ next_addr_send`; `next_addr_send = exp(now, 30 s)`; remove already-known entries and add the rest to `addr_known`; if non-empty send one `addrv2` (if `wants_addrv2`) or `addr` with all of them (≤ 1000) and clear. So an outbound peer receives our batched `addr` at Poisson 30 s intervals, and our own address about daily (behaviour 15). At M2 `fListen` is true by default but P2b is not merged, so the self-announce is **gated on the listener being bound**; without a listener `GetLocalAddrForPeer` yields no routable address on agentbox anyway.

#### 4.10.4 `getaddr` **requests** we receive (5389-5424 / 5312-5347)

From an outbound peer: `Ignoring "getaddr" from %s connection. peer=%d`, ignored (behaviour 15). Inbound handling (one answer per connection, 23 % / 1000 from the 21–27 h cache) is P2b's branch of the same handler; P2a ports the handler with the inbound branch behind `is_inbound`.

#### 4.10.5 AddrMan arithmetic (`addrman.cpp`, identical at both pins; `GetEntries` aside)

- Buckets (`:38-55`): `tried = H(nKey ‖ key) % 8 → H(nKey ‖ group(addr) ‖ that) % 256`; `new = H(nKey ‖ group(addr) ‖ group(src)) % 64 → H(nKey ‖ group(src) ‖ that) % 1024`; `position = H(nKey ‖ 'N'|'K' ‖ bucket ‖ key) % 64`, where `H` is `HashWriter.GetCheapHash()` (the first 8 bytes of SHA256d, LE) and `key = GetAddrBytes() ‖ port BE` (`netaddress.cpp:900-906`).
- `IsTerrible` (`:57-80`): not within 1 min of a try; future > 10 min; older than 30 d; `attempts ≥ 3` with no success; `attempts ≥ 10` and no success in 7 d.
- `GetChance` (`:82-94`): `0.01` if tried within 10 min, `× 0.66^min(attempts, 8)`; selection accepts when `randbits(30) < chance_factor × chance × 2^30`, `chance_factor ×= 1.2` per miss (`Select_` 756-784). **Floating point in a Core path**: the nine `0.66^n` values and the `1.2^k` sequence are IEEE doubles computed with libm `pow`; P2a uses a compile-time table of the nine exact doubles (verified against the oracle, §7.2) and multiplies `chance_factor` by the literal `1.2` as Core does; the comparison is a product of doubles truncated to u64 by the `<` compare against an integer — the Rust expression mirrors Core's operand order so rounding is identical (quirk registration, §4.17.3).
- `AddSingle` (`:616-683`): routable only; `time_penalty = 0` when `addr == source`; an existing entry updates `nTime` only if `pinfo->nTime < addr.nTime − (online ? 1 h : 24 h) − penalty` (online = `now − addr.nTime < 24 h`), ORs services, and returns false when the new time is not newer, when in tried, when `nRefCount == 8`, or with probability `1 − 2^−nRefCount`; a new entry is created with `nTime − penalty`; the bucket slot is taken if empty, or if the occupant `IsTerrible()` or (`occupant.nRefCount > 1 && new.nRefCount == 0`), evicting it via `ClearNew`.
- `Good_` (`:685-724`): stamps `m_last_success`, `m_last_try`, `nAttempts = 0`; already tried → false; collision in tried with `test_before_evict` → remember in `m_tried_collisions` (≤ 10) and return false; else `MakeTried` (removes every new-table reference, evicts the tried occupant back to new).
- `Attempt_` (`:740-756`): `m_last_try = time`; `nAttempts++` only if `count_failure && m_last_count_attempt < m_last_good`.
- `Connected_` (`:883-894`): `nTime = time` if older than 20 min. Called from `FinalizeNode` only for successfully connected, non-misbehaving, **non-block-relay outbound** peers (1843-1848 / 1870-1875).
- `ResolveCollisions_` / `SelectTriedCollision_` (`:917-1012`): the 4 h `ADDRMAN_REPLACEMENT`, the 60 s and 40 min (`ADDRMAN_TEST_WINDOW`) rules, exactly as written.
- `GetAddr_` (`:815-851`): `nNodes = min(pct × size / 100, max)`; a partial Fisher–Yates over `vRandom` (which **permanently** reorders it, affecting later calls), skipping terrible entries when `filtered`.
- `Check()` with `-checkaddrman=N` (`:1053-1061`) and the error codes of `CheckAddrman` (−1…−18) are ported for the fuzzers and `addrman_tests`.

### 4.11 Headers synchronisation

#### 4.11.1 Starting a sync (`SendMessages` 6342-6395 / 6196-6249)

Preconditions: `!fSyncStarted && CanServeBlocks(their_services) && !LoadingBlocks() && CanRelay()`. Then, if `(nSyncStarted == 0 && sync_blocks_and_headers_from_peer) || best_header.time > now − max_tip_age (6 h)`: `pindexStart = best_header.pprev` if any; send `getheaders2` if `UsesCompressedHeaders` else `getheaders` with `locator(pindexStart)` and null `hashStop`, through `MaybeSendGetHeaders` (which refuses if one was sent to this peer within 2 min, `HEADERS_RESPONSE_TIME`); log `initial %s (%d) to peer=%d (startheight:%d)`; `fSyncStarted = true`; `m_headers_sync_timeout = now + 15 min + 1 ms × ((now − best_header.time) / 150 s)`; `nSyncStarted++`. **Clock:** `now` in the two `max_tip_age` tests and the timeout formula is `GetAdjustedTime()` under `V23_1` and the local clock under `V24_0` (P2P-16); `current_time` for the timeout base is the local clock at both pins.

So on a fresh node exactly one outbound peer gets the initial `getheaders2`; a second peer is asked only when our best header is within 6 h of now (near the tip) or the first sync peer is gone. The `headers` reply handler then keeps asking the same peer while it returns full batches (§4.11.3).

#### 4.11.2 `headers` / `headers2` arrival (5309-5346 / 5232-5269)

`LoadingBlocks()` → `Unexpected headers message received from peer %d`, ignore. **`m_last_getheaders_timestamp = {}` is reset by any headers message, solicited or not** (a quirk: an unsolicited empty `headers` re-arms our ability to send another `getheaders` immediately). Count via `ReadCompactSize`; `count > GetHeadersLimit(peer, compressed)` → `Misbehaving(20, "headers message size = %u")` with the limit 8000 for `headers2` at `common ≥ 70235` and 2000 otherwise (1210-1216 / 1194-1200); `headers`: 80 bytes + an ignored CompactSize tx count each; `headers2`: `CompressibleBlockHeader::Uncompress` with the MRU-7 version list (BS7's codec; §4.15). Then `ProcessHeadersMessage(headers, via_compact_block=false)`.

#### 4.11.3 `ProcessHeadersMessage` (3322-3386 / 3338-3402)

Empty → return. `prev_known`? No: `count ≤ 8` → `HandleFewUnconnectingHeaders` (3138-3165 / 3154-3181): `nUnconnectingHeaders++`, send `getheaders(2)` for `locator(best_header)` through `MaybeSendGetHeaders`, log `received header %s: missing prev block %s, sending %s (%d) to end (peer=%d, nUnconnectingHeaders=%d)`, `UpdateBlockAvailability(last.hash)` (sets `hashLastUnknownBlock`), and every 10th time `Misbehaving(20, "%d non-connecting headers")`; `count > 8` → `Misbehaving(10, "invalid header received")`. Connected: `CheckHeadersAreContinuous` else `Misbehaving(20, "non-continuous headers sequence")`. `last_was_new`; **`ProcessNewBlockHeaders` (awaited `ChainCommand`)**; on an invalid result → `MaybePunishNodeForBlock(state, via_compact_block=false, "invalid header received")` (§4.13) and return; a non-invalid failure falls through with `pindexLast` set as Core does. Then: if `count == limit` → `MaybeSendGetHeaders(locator(pindexLast))`, log `more %s (%d) to end to peer=%d (startheight:%d)`; `UpdatePeerStateForReceivedHeaders` (§4.11.4); `HeadersDirectFetchBlocks` (P3 hook).

The index facts Core reads under `cs_main` before and during this function (`prev_known`, `last_was_new`, `pindexLast`, `best_header`, the tip's work, `MinimumChainWork`, `IBD`) all come back in the one `HeadersResult`, so the actor's view is exactly as consistent as Core's single critical section. The actor blocks on the reply; every other peer waits, as behind `msghand`.

#### 4.11.4 `UpdatePeerStateForReceivedHeaders` (3267-3320 / 3283-3336)

Reset `nUnconnectingHeaders` (log `peer=%d: resetting nUnconnectingHeaders (%d -> 0)` if it was > 0); `UpdateBlockAvailability(last)`; `last_block_announcement = GetTime()` if the headers were new and `last.chain_work > tip.chain_work`. **In IBD, when the batch was not full and `pindexBestKnownBlock.nChainWork < MinimumChainWork`**: outbound or block-relay → `Disconnecting outbound peer %d -- headers chain has insufficient work`, `fDisconnect`. Protection: a full-relay outbound peer whose best known work ≥ our tip's work takes one of 4 `m_chain_sync.m_protect` slots (`Protecting outbound peer=%d from eviction`).

#### 4.11.5 Headers-sync timeout (`SendMessages` 6764-6793 / 6618-6647)

While `fSyncStarted` and the timeout is armed: if `best_header.time ≤ now − 6 h` (clock per profile) and `current_time > m_headers_sync_timeout` and we are the only syncing peer (`nSyncStarted == 1`) and there is another preferred-download peer: non-noban → `Timeout downloading headers from peer=%d…, disconnecting`; noban → log and reset `fSyncStarted`/`nSyncStarted`/timeout. Once `best_header.time > now − 6 h` the timeout is disarmed (`microseconds::max()`).

#### 4.11.6 `CanDirectFetch`, `TipMayBeStale`, `CheckForStaleTipAndEvictPeers` (1418-1431 / 1395-1408; 6121-6146 / 5975-6000)

`CanDirectFetch() = tip.time > now − 20 × 150 s` with the profile clock (P2P-16). `TipMayBeStale() = last_tip_update < GetTime() − 3 × 150 s && mapBlocksInFlight.empty()` (local clock; `last_tip_update` initialised lazily to `GetTime()`). Every 45 s (`EXTRA_PEER_CHECK_INTERVAL`, scheduler): `EvictExtraOutboundPeers(now)` (§4.11.7); every 150 s: if not loading, network active, addrman outgoing and the tip may be stale → `Potential stale tip detected, will try using extra outbound peer (last tip update: %d seconds ago)` and `SetTryNewOutboundPeer(true)`, else clear it; and once `CanDirectFetch()` first holds → `StartExtraBlockRelayPeers()`. A headers-only M2 node never updates `last_tip_update` (that is `BlockConnected`), so after 450 s it opens an extra full-relay peer and keeps it until `EvictExtraOutboundPeers` disconnects the worst one — exactly dashd's behaviour on a node that is not receiving blocks, which is what a probe sees from a stalled dashd too.

#### 4.11.7 `ConsiderEviction` and `EvictExtraOutboundPeers` (5949-6005 / 5803-5859; 6007-6119 / 5861-5973)

Ported verbatim: the `CHAIN_SYNC_TIMEOUT` 20 min state machine per unprotected outbound/block-relay peer with `fSyncStarted` (one `getheaders(2)` for `locator(work_header.pprev)` as a last chance, then `Disconnecting outbound peer %d for old chain, best known block = %s` after `HEADERS_RESPONSE_TIME`); the extra block-relay eviction (youngest by node id unless it delivered a block more recently than the next-youngest; only after 30 s connected and with nothing in flight); the extra full-relay eviction (least recent block announcement, ties to the higher id; MN connections, protected peers, the only peer of a network and up to `max_outbound_onion` Tor peers are skipped; after a disconnect `SetTryNewOutboundPeer(false)`).

#### 4.11.8 `inv` (block part) (4507-4611 / 4462-4566)

Decode; `size > 50,000` → `Misbehaving(20, "inv message size = %u")`. Per entry: **unknown type** (`!IsKnownType()`, where type 34 is known only under `V24_0`, P2P-04) → log `got inv of unknown type %d: %s peer=%d`, skip. `MSG_BLOCK`: `UpdateBlockAvailability`; if new, not loading and not requested → `best_block = hash`. Other types: blocks-only violation check (`tx`, `dstx` and the §4.13 list disconnect; `MSG_ISDLOCK` on a blocks-only link is ignored at `common ≤ 70223` and disconnects above), `AlreadyHave` (handlers), `AddKnownInv`, and — unless in IBD, except `MSG_SPORK` — `tracker.add_announcement` (`NoRequests` at M2). After the loop, if a new block hash was seen: when `fSyncStarted`, or when this peer has not yet triggered a pre-sync getheaders and the hash differs from `m_last_block_inv_triggering_headers_sync`, send `getheaders(2)` for `locator(best_header)` and record the hash. **This is the only `getheaders` path an M2 node has outside the initial sync and the unconnecting-headers path** (behaviour 7's "getheaders policy").

#### 4.11.9 Chain events the actor consumes (`UpdatedBlockTip` 2273/2280, `BlockConnected` 2162/2183, `BlockDisconnected` 2210/2217, `NewPoWValidBlock` 2228/2235, `BlockChecked` 2314/2321)

Delivered through A1's ordered `ChainEvent` bus (the equivalent of Core's queued validation-interface callbacks, so they are **asynchronous** relative to the message that caused them, as in Core):

- `UpdatedBlockTip { new, fork, ibd }`: `best_height = new.height`; `SetServiceFlagsIBDCache(!ibd)`; if `!ibd`, collect up to 8 hashes from `new` back to `fork` and append them to `blocks_for_headers_relay` of every `CanRelay()` peer; `wake_message_handler()`. (The announcement itself is §4.8 item 7.)
- `BlockConnected`: `last_tip_update = GetTime()`; the stalling-timeout decay, orphan and recent-confirmed bookkeeping are P3/P5.
- `BlockDisconnected`, `NewPoWValidBlock` (compact-block fast announce), `BlockChecked` (punish the block's source): P3; P2a registers the subscriptions and leaves the bodies as hooks so that the ordering of the subscriber list is fixed now.

### 4.12 `notfound` (5744-5766 / 5598-5620)

Decode; `size > P2pRules.notfound_limit` (116 | 50,000) → `Misbehaving(20, "notfound message size = %u")`; else each known-type entry → `tracker.received_response(peer, inv)` (`NoRequests` ignores). Under `V23_1` the legacy tracker's per-peer erase (P5) applies.

### 4.13 Misbehaviour and disconnect responses raised by P2a's handlers

| Trigger | Score / action | Core | Pin |
|---|---|---|---|
| `version` below 70221 | disconnect | 3971-3976 / 3942-3947 | both |
| outbound full-relay/block-relay/addr-fetch peer without desirable services | disconnect | 3964-3969 / 3935-3940 | both |
| devnet name mismatch | +100 outbound, +1 inbound, disconnect | 4034-4046 / 4005-4017 | both |
| inbound `fMasternode` on a non-MN | disconnect | 4012-4016 / 3983-3987 | both |
| `sendaddrv2` / `sendtxrcncl` (when enabled) after verack | disconnect | 4297-4301, 4315-4319 | both |
| blocks-only violation: `tx`, `dstx`, every `ds*`, `qgetdata`, `qdata`, `qsendrecsigs`, `qwatch`, every `q*` DKG/signing command, `getqrinfo`, `qrinfo`, `platformban` (`protocol.cpp:185-212`, identical at both pins) on a block-relay, feeler or `-blocksonly`-without-`relay` connection | disconnect | 4382-4386 / 4337-4341 | both |
| `addr`/`addrv2` > 1000 | +20 | 4406-4409 | both |
| `inv` > 50,000 | +20 | 4510-4513 | both |
| `notfound` > limit | +20 | 5752-5755 / 5602 | **profile** (116 / 50,000) |
| `headers(2)` > limit | +20 | 5324-5326 / 5247-5249 | both |
| non-connecting headers, every 10th | +20 | 3162-3164 / 3178-3180 | both |
| > 8 unconnecting headers in one message | +10 "invalid header received" | 3344 / 3360 | both |
| non-continuous headers | +20 | 3351 / 3367 | both |
| `ProcessNewBlockHeaders` invalid: `BLOCK_INVALID_HEADER`/`CHECKPOINT`/`INVALID_PREV` +100, `MISSING_PREV`/`CHAINLOCK` +10, `CONSENSUS`/`MUTATED` +100 (not via compact block), `CACHED_INVALID` +100 outbound only, `TIME_FUTURE`/`RECENT_CONSENSUS_CHANGE`/`UNSET` 0 | per class | 1969-2019 / 1992-2040 | both |
| insufficient-work headers chain in IBD (outbound) | disconnect | 3298-3308 / 3314-3324 | both |
| headers-sync timeout, chain-sync timeout, extra-peer eviction | disconnect | §4.11.5–4.11.7 | both |
| `getheaders(2)` locator > 101 | disconnect (P2b serves, P2a ports the check) | 4765-4769 / 4715-4719 | both |
| feeler after `version`; addr-fetch after `addr`>1 or 5 min; inactivity (§4.14) | disconnect | — | both |

`Misbehaving` (1933-1955 / 1953-1978): additive; crossing 100 sets `m_should_discourage` and logs ` DISCOURAGE THRESHOLD EXCEEDED`; the log line is `Misbehaving: peer=%d (%d -> %d)%s%s`. `MaybeDiscourageAndDisconnect` (5833-5870 / 5689-5726) runs at the top of every `SendMessages`: noban → `Warning: not punishing noban peer %d!`; manual → `Warning: not punishing manually connected peer %d!`; local address → disconnect without discouraging; else `Disconnecting and discouraging peer %d!`, `BanView::discourage(addr)` and `DisconnectNode(addr)` (every connection to that address). Scores are never decayed.

### 4.14 Inactivity, linger and half-close (`InactivityCheck` 2298-2333 / 2304-2339; `DisconnectNodes` 2067-2212 / 2073-2218)

- Every 1 s (steady clock) over all connections: skip until `connected + peertimeout (60 s) < now`; then disconnect if no bytes were ever received **or** never sent (`socket no message in first %i seconds, %d %d peer=%d`), if `last_send` or `last_recv` is older than 20 min (`socket sending timeout` / `socket receive timeout`), or if the handshake has not completed (`version handshake timeout peer=%d`, or `V2 handshake timeout` while the transport is still detecting). The times are the mockable node clock (`-peertimeout=999999999` disables it in tests).
- `setnetworkactive false` (RPC, RP1a) sets `network_active = false`: every connection is marked `fDisconnect` with `Network not active, dropping peer=%d` (2100-2108 / 2106-2114), no new outbound connection is opened (`OpenNetworkConnection` 3598-3600) and the DNS seed task waits (`Waiting for network to be reactivated before querying DNS seeds.`).
- Disconnect procedure: when **we** initiated (`!fOtherSideDisconnected`), start a 100 ms steady-clock linger; once the transport has nothing left to send, `shutdown(SHUT_WR)` once; after the linger, close. When the other side closed first, close at once. Then: log `ThreadSocketHandler -- removing node: peer=%d …`, queue a v1 reconnection if applicable (§4.5), release the slot grant, close the socket, decrement `network_conn_counts`, `FinalizeNode` (1783-1856 / 1816-1876: `nSyncStarted--` if syncing, drop in-flight blocks, `tracker.disconnected_peer`, counters, `addrman.connected(addr)` for successful, unscored, non-block-relay outbound peers, log `Cleared nodestate for peer=%d`).

### 4.15 Per-command decode policy (the M2 set)

The table pins, for every command P2a's handlers decode, what `decode_with_policy` reports and how the actor maps it to Core's outcome. It is BS4a §4's table restricted to M2 and made executable (`policy.rs`); `diff_p2p_<cmd>` compares the whole row (§7.2).

| Command | Decode (base-sdk type) | Decode failure | Trailing bytes | Limit / gate after decode |
|---|---|---|---|---|
| `version` | `Version` with `Option` tail, `VersionTail` on a cut inside the tail | log + ignore; **head decisions (services, obsolete version) still apply** when the head decoded | ignored | §4.6.3 |
| `verack`, `sendaddrv2`, `sendheaders`, `sendheaders2`, `getaddr`, `mempool`, `filterclear`, `getsporks`, `qwatch` | payload ignored | — | ignored | state gates only |
| `sendcmpct` | `bool` (`CoreBool`) + u64 | log + ignore | ignored | version ≠ 1 → ignore |
| `senddsq`, `qsendrecsigs` | `CoreBool` | log + ignore | ignored | — |
| `ping` | u64 | log + ignore (no pong) | ignored | — |
| `pong` | u64 if ≥ 8 bytes | `Short payload` (finishes the ping) | ignored | — |
| `addr` / `addrv2` | `Addr` / `AddrV2Msg` | log + ignore | ignored | > 1000 → +20 (after decode, after the relay-setup gate) |
| `inv`, `getdata`, `notfound` | `Inv` | log + ignore | ignored | > 50,000 / > 50,000 / > `notfound_limit` → +20 |
| `headers` | count then 80-byte headers + ignored CompactSize | log + ignore | ignored | count > 2000 → +20 **before** reading any header |
| `headers2` | count then `CompressibleBlockHeader`s (BS7 codec) | log + ignore | ignored | count > 8000 (≥ 70235) / 2000 → +20 before reading |
| `getheaders`, `getheaders2` | locator + stop hash | log + ignore | ignored | locator > 101 → disconnect |
| known command, no M2 handler (`tx`, `block`, `spork`, `clsig`, `isdlock`, `mnauth`, `ssc`, `dsq`, …) | **not decoded** | — | — | blocks-only violation check still applies; otherwise silently ignored (§3.6.3) |
| unknown command | `P2pMsg::Unknown` | — | — | `Unknown command "%s" from peer=%d`, ignored |

`ProtoCtx` is built per message from the connection's `common_version` and `BlsCtx::for_tip(ChainView)`; nothing in the M2 set depends on BLS.

### 4.16 Unknown and absent messages (behaviour 11)

Never sent: `feefilter`, `wtxidrelay`, `reject`, `alert`, `getaddrv2`, `sendtxrcncl` (off by default), `sendpackages`. All five are absent from base-sdk's 74-name table and therefore decode as `Unknown` → logged and ignored, as Core does (`protocol.cpp:18-94` has none of them).

### 4.17 Quirks and determinism

#### 4.17.1 Quirks registered by this design (new numbers after R4's 34 + CS1's 39 + CS2's 40; the manager assigns)

- **Q-P2a-1** quorum-priority receive queue with the 100:1 ratio (§4.7.2); both pins; changes per-peer processing order.
- **Q-P2a-2** `m_last_getheaders_timestamp` is cleared by any `headers`/`headers2`, solicited or not, empty or not (§4.11.2).
- **Q-P2a-3** a `version` whose head decodes but whose tail is cut applies the services/obsolete-version disconnects and otherwise leaves the peer pre-version, resending nothing (§4.6.3, BS4a verified at both pins).
- **Q-P2a-4** feelers and addr-fetch connections send `getaddr` before disconnecting (§4.6.3 step 15, 19).
- **Q-P2a-5** `addrman.good()` on every outbound type incl. feelers and block-relay; `connected()` only for non-block-relay outbound (§4.10.5).
- **Q-P2a-6** `GetAddr_` permanently reorders `vRandom`, so successive `getaddr` answers (and `peers.dat` dumps) depend on earlier calls (§4.10.5).
- **Q-P2a-7** `version.nTime` and `nTimeOffset` use the unadjusted mockable clock; the adjusted offset feeds only the three sync decisions under `V23_1` (P2P-16).
- **Q-P2a-8** `fMoreWork` ignores send-paused peers; MN send throttle resets only when it fires (§4.7.3).
- **Q-P2a-9** headers-count limit is checked before any header is read, so an oversized `headers` with garbage after the count still costs exactly +20 and nothing else (§4.15).
- **Q-P2a-10** `GetChance`/`Select_` floating point (§4.10.5): doubles in a peer-visible selection path.

#### 4.17.2 Randomness (allowed to differ, must match in distribution)

Per-process `FastRandomContext` (ChaCha20 over a fresh 256-bit seed; deterministic mode for tests), SipHash randomizers (`RANDOMIZER_ID_NETGROUP` 0x6c0edd8036ef4036, `_LOCALHOSTNONCE` 0xd93e69e2bbfa5735, `_ADDRCACHE` 0x1cf2e4ddd306dda9, `_ADDRESS_RELAY` 0x3cac0035b5866b90 — the same constants, so the *shape* of hashing is Core's). Poisson timers: `GetExponentialRand(now, mean) = now + round(−log1p(u48 × −2^−48) × mean)` (`random.cpp:771-775`); uniform delays via `randrange`. NFR-FP-03 checks these by KS test, not equality.

#### 4.17.3 Floating point

Three doubles live in P2a: `GetChance` (quirk Q-P2a-10, exact table + mirrored expression, verified by `oracle_addrman_chance`), the token bucket (`Ticks<SecondsDouble>(diff) × 0.1`, compared against 1.0 and 1000: mirrored expression; only the rate-limited counts are observable, and they are compared exactly in `p2p_addr_relay`), and `GetExponentialRand` (statistical only). None is consensus; all three are called out per FR-CON-15's spirit.

#### 4.17.4 Clocks

`NodeClock` (mockable: `-mocktime`, `setmocktime`) for `GetTime()`, `Now<NodeSeconds>()`, ping and inactivity; adjusted time = `NodeClock + offset` (`timedata.cpp`); `SteadyClock` for the linger, the 1 s inactivity sweep cadence and the MN throttle (v24 moved them to steady time with identical values, #7283; O17 "not a delta"); `time_received` is a microsecond stamp taken when the reader completes a message (Core: `ReceiveMsgBytes` 652).

### 4.18 The transcript of one outbound full-relay connection, and the behaviour cross-reference

What a probe acting as the **peer** sees from an M2 rdashd under the default (v24) profile, V1 transport, with the `V23_1` differences in brackets:

```
t0        → version   137 B: 70242 [70240], services 0x1c05, time=now, addr_you=<probe>, addr_me=0x1c05+::/0,
                      nonce, "/Dash Core:24.0.0/" ["/Dash Core:23.1.8/"], start_height, relay=1, challenge(32), fMasternode=0
t0+ε (we receive the probe's version with NODE_HEADERS_COMPRESSED)
          → sendaddrv2            (iff probe version ≥ 70223)
          → verack
          → getaddr               (outbound, not block-relay)
t1 (we receive verack; same ThreadMessageHandler pass)
          → sendheaders2          (or sendheaders without bit 11)
          → sendcmpct 00 01000000 00000000
          → senddsq 01
          → ping <nonce>          (SendMessages: first pass after verack)
          → getheaders2 <locator(best_header.prev)> 00…00   (iff this is the sync peer or best_header is within 6 h [adjusted clock])
… probe sends headers2 ×8000 → within the same handler: → getheaders2 <locator(last)>
… every ~2 min             → ping ; our pong replies arrive within the same pass as the probe's ping
… Poisson 30 s             → addr/addrv2 (only if we learned fresh addresses to relay)
… [O13a mask at M2: dashd additionally sends getsporks ≈0–6 s after verack, govsync ≈8 s later, mempool after sync, getdata for invs]
```

| PRD §4.2 behaviour | Where reproduced | Per profile |
|---|---|---|
| 1 `version` layout | §4.6.2 | `Identity` values only |
| 2 service bits | §4.6.1 | P2P-35 via `InitRules` (ND1) |
| 3 who speaks first | §4.6.2 (outbound at connect), §4.6.3 step 7 (inbound, P2b) | same |
| 4 after the peer's `version` | §4.6.3 steps 9, 13, 15, 19 | same |
| 5 after `verack` | §4.6.4 + §4.8 items 4, 6 | same |
| 6 pre-handshake messages | §4.6.4 gate, §4.6.5 | same |
| 7 ping, getheaders policy, empty headers below min work | §4.9, §4.11.1, §4.11.8 (empty-headers serving: P2b) | clock of §4.11.1 (P2P-16) |
| 11 unknown / absent messages | §4.16, §3.6.3 | same |
| 12 misbehaviour (outbound half) | §4.13 | `notfound_limit` |
| 14 size limits | §4.13, §4.15 | `notfound_limit` |
| 15 address relay (request side) | §4.10 | same |
| 25 connection behaviour | §4.1–4.5, §4.11.6 | same |
| 26 inactivity | §4.14, §4.9 | same |
| 27 devnet | §4.6.3 step 8 | same |
| +C cross-peer ordering | §4.7.3–4.7.4, §4.11.3 (awaited headers) | same |

---

## 5. Persistence, crash consistency and recovery

| File | Core | rdashd | Format and recovery |
|---|---|---|---|
| `peers.dat` | `DumpPeerAddresses` every 15 min (`DUMP_PEERS_INTERVAL`, scheduler) and at shutdown; `SerializeFileDB`: magic+network bytes, payload, SHA256d checksum, written to `peers.dat.<rand>.new` then renamed (`addrdb.cpp:55-90`); load failures rename the file to `peers.dat.bak` with Core's InitError text (`:205-229`) | same cadence; **own format** = Core's V4_MULTIPORT byte stream (DEC-11 allows our own; Core's is already a clean spec, needs no translation, and makes `-importcoredatadir` trivial), so import = load. The asmap checksum field is `uint256()` while asmap is empty (P8) | write temp + fsync + rename; a torn file fails the checksum → renamed `.bak`, start with an empty addrman (Core's behaviour) |
| `anchors.dat` | read at `Start` then **deleted** (`ReadAnchors` 239-250: `fs::remove` even on success); at most 2 used; dumped only at clean shutdown from the current block-relay-only connections (`StopNodes` 4193-4200) | identical; the file is written atomically like `peers.dat` | a crash leaves no file (it was deleted at start) → no anchors next start, as Core |
| `netfulfilled.dat` | SP1 | SP1 | — |
| `banlist.json` | P4 | P4 | — |

No other durable state. `FR-P2P-19`: Core's `peers.dat` and `anchors.dat` are importable because the format is the same; `-importcoredatadir` (CH6) copies them.

---

## 6. Performance

**Metrics affected** (A1 §3.9, PLAN BG for P2a): `ibd-*` `header_sync_s` (window), `startup_s` and `idle_rss_mib` (M2 exit 5), PF0 counters for header sync.

**Budget: never slower.** Header sync is bound by the peer's response time and by `ProcessNewBlockHeaders` (CH1); P2a adds one `getheaders2` round trip per 8000 headers, with the request issued inside the handler that processed the previous batch (as Core). Per-header P2P cost: decode (BS7's headers2 codec), one locator build per batch (≤ 32 hashes over the skip list), and one `ChainCommand` round trip per batch (one allocation per batch, the `Vec<BlockHeader>` moved, not copied). Target: P2a's own instructions per header ≤ 1/10 of CH1's acceptance cost; the actor must never be the critical path (measured by PF0's `headers_sync_replay`).

**Route to 2×** (where our own work is the bottleneck): none in P2a. Header sync wall time on mainnet (≈ 2.3 M headers / 8000 = 290 round trips) is network-latency bound; the only lever is CH1's acceptance throughput and X11 in the readers (A1 §2.14 allows pre-hashing headers off-actor as an unobservable optimisation; this design keeps it **off** in P2a and leaves it to CH1/P3 if PF1 shows acceptance is the bottleneck).

**Data structures.** `Bytes` payloads (zero copy from the read buffer to the actor); `VecDeque<RawMsg>` × 2 per peer; `Vec<Arc<Conn>>` snapshot per pass (one allocation per pass, like Core's `NodesSnapshot`); addrman tables as fixed arrays (`[[i32; 64]; 1024]` = 256 KiB + `[[i32; 64]; 256]` = 64 KiB), `BTreeMap` for `map_info` (Core's `std::map`), `HashMap` for `map_addr` (never iterated). Idle RSS: ≤ dashd's; the dominant term is addrman at ~100 B per entry (Core's `AddrInfo` is ~ 90 B + map node).

**Benches** (criterion, PF0-countable): `addrman_add_select` (10,000 adds + 1,000 selects, deterministic RNG), `actor_pass_125_peers` (one loop pass over 125 idle peers), `headers_sync_replay` (a captured dashd `headers2` transcript of 100 batches fed through the actor with a stub `ChainCommand` that replies instantly; counts instructions and allocations per header). Core-side comparison: `bench_dash` has no addrman bench; `AddrManAdd/AddrManSelect/AddrManGetAddr` exist in Core's `bench/addrman.cpp` at both pins and are used as the dashd baseline for the first bench.

---

## 7. Verification

### 7.1 L0: Core vectors and unit tests ported

- `src/test/addrman_tests.cpp` (both pins; identical): every case (`simple`, `ports`, `select`, `new_collisions`, `new_multiplicity`, `tried_collisions`, `getaddr`, `caddrinfo_get_tried_bucket`, `caddrinfo_get_new_bucket`, `caddrinfo_get_bucket_position`, `serialization` incl. the asmap re-bucketing cases, `selecttriedcollision`, `noevict`, `evictionworks`, `load_peers_dat_*`) through a deterministic `AddrMan`; the expected bucket numbers are Core's literal constants, which only match if `GetGroup`, `GetKey`, `HashWriter` and the `FastRandomContext` port are bit-exact. `asmap.raw` (R4 F §2, 59 bytes) is used by the asmap cases.
- `netbase_tests` / `net_tests` subsets that cover `GetGroup`, `CService::GetKey`, `IsRelayable`, `CalculateKeyedNetGroup`, `LocalAddress` logic.
- `denialofservice_tests`: `outbound_slow_chain_eviction`, `stale_tip_peer_management`, `peer_discouragement` (the outbound half), with the test's mocktime.
- `random_tests` (the `FastRandomContext` determinism vectors) — a port exists if CS0/O4 has not already provided one.
- The `version` byte fixtures: dashd's `version` captured by BS4a's probe at both pins (nonce and challenge normalised) and the 137-byte size.

### 7.2 L1: differential targets and new oracle entry points

Existing: `diff_p2p_<cmd>` for `version`, `verack`, `sendaddrv2`, `sendheaders`, `sendheaders2`, `sendcmpct`, `senddsq`, `qsendrecsigs`, `ping`, `pong`, `addr`, `addrv2`, `inv`, `getdata`, `notfound`, `headers`, `headers2`, `getheaders`, `getheaders2` — extended from codec equality to the **handler-level outcome** of §4.15 (`accepted | ignored_logged | score(n) | disconnect`) against `oracle_p2p_decode` plus a small result-class extension (Q4). `diff_headers2`, `diff_v1_framing` (BS7/P1).

New (flagged for O2a/O1b): `oracle_addrman_bucket(nKey, addr, src, asmap) -> (tried_bucket, new_bucket, pos_new, pos_tried)`, `oracle_addrman_chance(last_try_delta, attempts) -> double bits`, `oracle_netgroup(addr, asmap) -> bytes`, `oracle_addrman_replay(ops[]) -> serialized V4 state` (a deterministic `AddrMan` with `nKey = 1` driven by `Add/Good/Attempt/Select/Connected/ResolveCollisions` ops and the same `FastRandomContext` seed; compared as the serialised bytes = **bucket-math parity**, PLAN acceptance), `oracle_locator(heights…) -> hashes` (trivial; CH1 may own it), `oracle_version_build(fields) -> bytes` (the exact `PushNodeVersion` serialisation).

Budgets (NFR-CEQ-04): `diff_addrman_replay` ≥ 24 CPU-h per pin (the logic is pin-identical, but the runner records both); `diff_p2p_*` M2 set ≥ 24 CPU-h total.

### 7.3 L6: fingerprint (O13a)

Transcripts recorded from both binaries under identical regtest state and mocktime, V1 and V2, for each outbound connection type we can provoke (`-connect` manual, addrman full-relay, block-relay, feeler via mocktime, addr-fetch via `-seednode`), per profile (`-releaseprofile=23.1` vs v23.1.8 dashd; default vs v24 dashd). Compared after normalising nonces, challenges, timestamps and Poisson batches; the §1.3 mask applies at M2. Probes for behaviours 1–7, 11, 25 (outbound) and **+C**: peer A sends `headers` while peer B sends `ping`/`getheaders`; B's answer must reflect A's headers exactly when dashd's does (the actor's await makes this hold by construction; the probe proves it).

### 7.4 Fuzz (`node-fuzz`, A1 §4.12)

- `addrman_model`: random op sequences vs a `BTreeMap` model (membership, counts, `Check()` never fails, serialise→deserialise→serialise is a fixed point).
- `addrman_file`: `peers.dat` bytes (crash-free, Core's error classes).
- `actor_sequence`: random command/payload sequences across the state machine (pre-version, pre-verack, post-verack, renegotiation) → no panic, outcomes equal the policy table; seeded with BS4a's `decode_domain.json5` and dashd captures.
- `connman_sched`: a simulated clock drives `outbound`/`dnsseed`/`inactivity` with a fake dialer → invariants (slot counts never exceed limits; netgroup rule; one feeler at a time).

### 7.4a L2: replay windows

- **Header-sync replay, mainnet and testnet**: the oracle's header chain (dumped per the `dashd-reference-oracles` recipe) is served by a scripted peer in 8000-header `headers2` batches and in 2000-header `headers` batches; the resulting `getheaders(2)` sequence (locators, timing of the follow-up request relative to the batch) must equal a dashd of the same profile fed the same script, and the final `getchaintips`/`chainwork` must equal the oracle's (M2 exit 1; O9a's comparator).
- **Timing windows under mocktime**: 24 simulated hours with 8 scripted outbound peers; the feeler, extra-block-relay, stale-tip and addr-broadcast event counts are compared with dashd's by KS test (NFR-FP-03; O13b owns the full suite, P2a ships the harness glue).

### 7.5 Functional tests to port (both pins' trees; those present at the pin)

`p2p_leak` (no message before verack except `sendaddrv2`; the outbound half), `p2p_ping`, `p2p_timeouts`, `p2p_addr_relay`, `p2p_addrv2_relay`, `p2p_addrfetch`, `p2p_seednode`, `p2p_dns_seeds`, `p2p_add_connections`, `p2p_outbound_eviction`, `p2p_initial_headers_sync`, `p2p_sendheaders`/`p2p_sendheaders_compressed` (the sync half), `p2p_headers_sync_with_minchainwork` (if present), `feature_anchors_db`, `feature_maxtipage`, `feature_minchainwork` (sync side), `rpc_net` (`getpeerinfo`, `getnodeaddresses`, `addpeeraddress`, `getaddednodeinfo` — with RP0/RP1a). These need `dash-testkit` (O16) and the M2 `rdashd` (ND1).

---

## 8. Implementation plan

| Phase | Content | Acceptance |
|---|---|---|
| **A** `release.rs`, `types.rs`, `clock.rs`, `FastRandomContext`, SipHash randomizers | `Identity`/`P2pRules` tables with O17 row cites; `Fingerprint` assembly incl. UA rules and the 256-byte InitError; `random_tests` vectors | unit tests; UA fixtures for both profiles, devnet and `-uacomment` |
| **B** `addrman/` | `NetGroupManager::get_group`, bucket math, `AddrManImpl`, `peers.dat` V4 codec, anchors I/O | `addrman_tests` port green; `oracle_addrman_*` differential (new O1b entry points); `addrman_model` and `addrman_file` fuzz smoke; `addrman_add_select` bench recorded |
| **C** `net/conn.rs`, `inactivity.rs`, `local.rs`, `stats.rs` over the P1 `Transport` | reader/writer tasks, queues, backpressure, linger + `SHUT_WR`, byte accounting, `NodeStats` | loopback tests with a scripted peer: reader outcome table (§4.7.1), pause/unpause, linger timing, counters equal Core's `getpeerinfo` semantics |
| **D** `peerman/` core: actor loop, queues, gates, policy, handshake, ping, misbehaviour | §4.6–4.9, §4.13, §4.15; `NoRequests`; empty handler registry | scripted-peer transcripts equal dashd's handshake at both profiles (O13a v0 harness, outbound); `actor_sequence` fuzz; `diff_p2p_*` M2 set |
| **E** `net/outbound.rs`, `dnsseed.rs`, `addnode.rs`, `addrfetch.rs`, `reconnect.rs`, `ConnMan::start/stop` | §4.1–4.5, §5 | `connman_sched` fuzz; regtest: `-connect`, `-addnode`, `-seednode`, feelers under mocktime, anchors round trip; `feature_anchors_db`, `p2p_dns_seeds`, `p2p_seednode`, `p2p_addrfetch`, `p2p_add_connections` |
| **F** `peerman/addr.rs` | §4.10 (request side, relay, `MaybeSendAddr`, token bucket) | `p2p_addr_relay`, `p2p_addrv2_relay` (outbound parts); rate-limit counts equal dashd's on the same script |
| **G** `peerman/headers.rs`, `inv.rs`, `eviction.rs`, `chain.rs` | §4.11, §4.12; `HeadersResult` contract with CH1 | mainnet + testnet headers to the tip from the oracles over localhost and from public peers (M2 exit 1); `p2p_initial_headers_sync`, `feature_maxtipage`, `feature_minchainwork`, `p2p_outbound_eviction`, `denialofservice_tests` port; +C probes; `headers_sync_replay` bench with PF0 counters |
| **H** polish: `getpeerinfo` plumbing (RP1a), asserted log lines, `ARCHITECTURE.md`, code-simplifier and review rounds | — | `cargo clippy -D warnings`, gate green, review clean (P2P class: GPT xhigh + Opus + code-review skill) |

Phases A, B and C can start when P1's `Transport` trait is agreed (interface dependency; a loopback V1 transport stub is enough for C and D). D–G need CH1's `ChainCommand` skeleton (A1 §2.10) and BS7's headers2 codec; until then G runs against a stub chain.

---

## 9. Open questions for the manager (each with a recommendation)

1. **M2 sync chatter gap (§1.3).** An M2 rdashd never sends `getsporks`/`govsync`/`mempool`/`getdata`. *Recommend:* accept the O13a mask for M2 as PLAN already implies (behaviours 1–5, 9, 10), **plus** a 40-line `getsporks` stub in P2a that reproduces the `SyncManager` tick timing (1 s scheduler, 6 s gate, once per address via an in-memory netfulfilled) and decodes-and-drops `spork` replies, so the most visible post-handshake message is present from M2 and SP0 replaces the stub. Not a profile matter (identical at both pins).
2. **`-blocksonly`/relay flag at M2.** With no mempool, rdashd still advertises `relay=true` and will receive tx `inv`s it never requests. *Recommend:* keep `relay=true` (fingerprint-faithful); `NoRequests` ignores announcements; P5 fills.
3. **CH1 interface additions** (§3.6.8): `HeadersResult { prev_known, last_was_new, outcome, best_header, tip, tip_chain_work, minimum_chain_work, ibd, loading_blocks }`, and `ChainView.index` exposing `prev`/`skip`/`hash`/`height`/`chain_work` for locators and `PeerHasHeader`. *Recommend:* approve as the P2a↔CH1 contract now (CH1 is in design); it costs CH1 two boolean lookups it already performs.
4. **Oracle entry points** (§7.2): `oracle_addrman_bucket/chance/replay`, `oracle_netgroup`, `oracle_version_build`, and a result-class extension of `oracle_p2p_decode` returning `(outcome, score, disconnect)`. *Recommend:* add to O1b/O2a's list; `oracle_addrman_replay` is the one PLAN's "bucket-math parity" acceptance needs.
5. **`peers.dat` format** (§5). DEC-11 allows our own; *recommend* Core's V4_MULTIPORT verbatim (import = load, export = dump), since nothing is gained by diverging and `-importcoredatadir` and `addpeeraddress` tests become free.
6. **Quirk registration** of Q-P2a-1…10 (§4.17.1) in R4's register; *recommend* registering 1, 2, 3, 4, 6, 10 (peer-visible) and keeping 5, 7, 8, 9 as port anchors.
7. **A1 §2.14 errata**: the loop sketch should read `more_work |= more && !pause_send`; `qwatch` is sent inside the `!RejectIncomingTxs` block after `senddsq`; feelers and addr-fetch send `getaddr`; the quorum-priority queue is missing from A1; "`getaddr` on outbound full-relay only" should read "on every outbound type except block-relay". *Recommend:* fold into the A1 errata list.
8. **Pre-hashing headers in the readers** (A1 §2.14's allowed optimisation): *recommend* not in P2a (no measured need; keeps the reader transport-only, which simplifies P1's seam). Revisit in P3 with PF1 data.

---

## 10. Deviations from A1 and PLAN

- A1 §2.14 says the `PeerManager` loop is "one global 100 ms timer, MN connections only" — correct, with the two refinements of Q7 above.
- A1 §4.10 #4 "`getaddr` on outbound full-relay connections only": Core sends it on every outbound connection that is not block-relay-only (feelers and addr-fetch included). PRD §4.2 item 4 has the same wording; errata.
- A1 §4.10 #26 "60 s for the first message": the check fires only after `peertimeout` when **either** direction has never carried a byte, or the handshake is incomplete; a peer that sent `version` but never `verack` is dropped at 60 s by the handshake rule.
- PLAN P2a acceptance "behaviours 1–7, 11, 25 (outbound)": behaviours 7 (ping, getheaders policy) and 25 (feelers, extra peers, anchors, seeds) are fully in scope; behaviour 8 (sync chatter) is SP1's; Q1 proposes a stub.

---

## 11. Risks

- **CH1 slippage**: header sync is P2a's only awaited path; a stub chain lets D–F merge first, G waits (interface dependency, PLAN `i:CH1`).
- **BS7 headers2 codec divergence** (DDE-SWEEP §5: first-header compression flags, version offset past the cache): P2a's `headers2` handler inherits BS7's fixes; until then `diff_headers2` is red and the M2 mainnet sync must use `getheaders` (NODE_HEADERS_COMPRESSED cleared in a test build) — not acceptable for O13a, so BS7 is on P2a's critical path.
- **`FastRandomContext` fidelity**: `addrman_tests` depends on it bit-for-bit; one afternoon of porting, but any mistake invalidates phase B's evidence. Mitigation: port Core's `random_tests` first.
- **Agentbox has no routable address**: self-announce and `SeenLocal` paths cannot be exercised live; covered by unit tests and the functional tests' `-externalip`.

## 12. Reproducing the pin diff

```
mkdir -p /work/scratch/p2a-fable && cd ~/workspace/dash
for t in v23.1.8 v24.0.0-rc.3; do for f in src/net.cpp src/net.h src/net_processing.cpp src/addrman.cpp src/addrman_impl.h src/protocol.{h,cpp} src/netgroup.cpp src/timedata.cpp src/version.h src/init.cpp src/chainparams.cpp src/node/sync_manager.cpp src/evo/mnauth.cpp; do mkdir -p /work/scratch/p2a-fable/$t/$(dirname $f); git show $t:$f > /work/scratch/p2a-fable/$t/$f; done; done
cd /work/scratch/p2a-fable && diff v23.1.8/src/net.cpp v24.0.0-rc.3/src/net.cpp      # 195 lines: clocks, bind failures, mncon gating, TryMarkVerified, asmap log
diff v23.1.8/src/addrman.cpp v24.0.0-rc.3/src/addrman.cpp                              # GetEntries + `filtered` only
# per-function diffs of net_processing.cpp: the ranges in §1.1 (e.g. SendMessages v23 6154-6767 vs v24 6300-6901)
```
