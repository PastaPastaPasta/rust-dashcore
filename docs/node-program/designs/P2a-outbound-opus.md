# P2a: outbound P2P core (Opus design, D2 half)

| | |
|---|---|
| Status | **Draft r0 for the D2 reconcile** (PLAN §4.5 row P2a, design kind D2: Fable and Opus design independently, the manager reconciles). Written without reading the Fable half |
| Date | 2026-10-08 |
| Author | Opus 5.5 (designer, `briefs/DESIGNER.md`) |
| Task | `dashd-p2p` outbound core: ConnMan (outbound, feelers, anchors, addnode, seeds), the handshake and `fingerprint.rs` from the release profile (`Identity`, `P2pRules`), the wake-driven actor loop (shuffle, MN-only throttle), the per-command decode policy and its gates, awaiting observable messages, ping, addrman, headers/headers2 sync. Seams for P1 (transport) and P2b (inbound) |
| Binding inputs | A1 §2.14, §2.25, §3.1, §2.10–§2.10.2, Appendix A/E; PLAN §4.5 (P1, P2a, P2b, P3, P4, P5, P5a, P6a/b, P7, P8), §4.7 (O13a/b), M2 exit; O17 `O17-release-profile.md` §4.2, §6, §10 and `O17-pin-delta/p2p-init.md`; BS4a `DDE-SWEEP.md` (branch `node/bs4a-decode-domain` c5534013; base-sdk `node/bsdk-bs4a-decode-domain` f8a088f); PRD FR-P2P-05…08, -16, NFR-FP-01…04, §4.2 behaviours; DEC-03, -04, -05, -11, -24 D3, -37 |
| Core verified at | `v23.1.8` = 728f505583 (mainnet pin) and `v24.0.0-rc.3` = 0364ed626e (testnet/devnet/regtest pin), read with `git archive` into `/work/scratch/p2a-outbound-opus/v{23,24}/`. Line numbers below are `vNN:file:line`; "(=)" means the same logic at both pins |
| rust-dashcore verified at | `node/integration` 276685fc; #264 = `upstream/feat/bip324-v2-transport` 5c296fba; dash-spv `src/network/*` at `node/integration` |
| Scratch | `/work/scratch/p2a-outbound-opus/` (Core trees, merged O17 rows, and the fact sheets under `factsheets/`: `addrman.md`, `connman.md`, `np-addr-evict-misb.md`, `dash-overlay-seams.md`, `m2-command-outcomes.md`; supporting notes with per-line Core citations; the design cites Core directly wherever it states a behaviour) |

> **Reading guide.** §0 is the one-page design. §1 scope. §2 the crate, the runtime shape and the Rust API. §3 is Core equivalence, which is most of the document: the actor loop (§3.1), the gates and decode policy (§3.2), the handshake and fingerprint (§3.3), headers sync (§3.4), ping and timeouts (§3.5), addr relay (§3.6), AddrMan (§3.7), ConnMan (§3.8), outbound eviction (§3.9), misbehaviour (§3.10), the 74-command table for the M2 node (§3.11), every observable behaviour per profile (§3.12), the O17 rows (§3.13), quirks (§3.14) and A1 errata (§3.15). §4 persistence, §5 performance, §6 verification, §7 the implementation plan, §8 open questions.

---

## 0. Summary

1. **One actor, Core's loop.** `PeerManager` runs on a dedicated OS thread (`peerman`) and is a line-by-line port of `ThreadMessageHandler` + `ProcessMessages` + `ProcessMessage` + `SendMessages`. Each pass shuffles a snapshot of the node table, runs `process_messages` then `send_messages` per node, skips `send_messages` for masternode connections unless the one global 100 ms steady-clock throttle has elapsed, and sleeps until woken or 100 ms. Messages from one peer are polled from **two** queues, Core's quorum-priority queue and the normal queue, at Core's 100:1 ratio (a detail A1 misses: §3.1.3). The actor blocks only where `msghand` blocks: on `ChainCommand` replies for `headers`/`headers2` at M2, and later on blocks, transactions and the synchronous overlay messages.
2. **The logic is sans-io and deterministic.** `PeerLogic` holds every piece of `net_processing` state and takes its world through narrow traits (`Net`, `ChainAccess`, `AddrManApi`, `Clock`, `Rng`, plus the seams for P3, P4, P5 and the overlays). The same code runs under the real runtime and under `sim`, a single-threaded virtual-time harness used for transcript tests, sequence fuzzing and handler-level differential tests.
3. **Gates first, then decoding at Core's read point.** `process_message` applies Core's gates in Core's order (duplicate `version`, nothing before `version`, the five negotiation messages honoured before `verack`, the MNAUTH-probe first-message rule, the blocks-only violation list); each handler then calls base-sdk's decoder (`decode_with_policy` / `MsgDecode`, with a `ProtoCtx` built at that point) exactly where Core's handler reads, so side effects Core performs before a failing read still happen. A table in `dashd_p2p::policy` pins, for every one of the 74 commands, its gate class, its pre-read effects, the outcome of a decode failure, its count limits and penalties, whether trailing bytes are ignored, and its M2 behaviour (§3.2, §3.11).
4. **The fingerprint is data.** `Identity` (protocol 70240 / 70242, UA `23.1.8` / `24.0.0`, client version, MN minimum) and `P2pRules` come from O17's statics; `fingerprint.rs` builds the 137-byte default `version`, the local services (0x1C05 / 0x1C45 and pruned variants) and the fixed post-`version` and post-`verack` orders. Every send-side version gate follows from `Identity.protocol_version` through the negotiated common version.
5. **Headers sync is Core's, with one X11 per header.** Initial-sync peer selection, the 2-minute `getheaders` spacing, `headers2` (8000) vs `headers` (2000) by the peer's bit 11 and common version, unconnecting headers (`MAX_UNCONNECTING_HEADERS` 10, +20), the continuity check (+20), min-chain-work disconnects, outbound protection, the 15 min + 1 ms/header timeout. Core hashes every header 2× (`headers`) or 3× (`headers2`) on `msghand`; we hash once, off the actor, in the reader task, and hand CH1 a `HeaderBatch` with the hashes. That is the route to 2× on `header_sync_s`.
6. **ConnMan and AddrMan are faithful ports.** Outbound full-relay (8), block-relay-only (2) with anchors, feelers every 2 min (Poisson), the extra full-relay peer on a stale tip and the extra block-relay peer every 5 min, `-addnode` on its own semaphore, `-connect`, `-seednode`/ADDR_FETCH, DNS seeds (always `x1.<seed>`, one seed per network, ≤ 32 results, 11 s / 5 min) and fixed seeds equal to `chainparamsseeds.h`. AddrMan is Core's bucket math with `nKey`, asmap, Core's `FastRandomContext` stream and Core's `peers.dat` format, so parity is proven by running the same operation stream through Core's AddrMan and ours in deterministic mode and comparing every answer and the full canonical state after every step (§6.3).
7. **Seams.** P1 delivers `RawMsg` frames and takes `OutMsg`s; P2b plugs in at `accept` and reuses the shared `version`/`verack` handler (both branches are written here); CH1 answers `process_headers` and publishes unthrottled `TipFacts`; P3, P4, P5 and the overlays plug in through `BlockSync`, `BanList`, `ObjectRequests`/`TxRelay` and a Core-shaped `NetHandler` registry.
8. **Per profile.** P2a reads `Identity.*`, `P2pRules.msg_asset_unlock` (inv type 34 is unknown end to end under `V23_1`), `P2pRules.notfound_limit` (the +20 size check: 116 / 50,000), `UngatedRules.time_source` (sync start, headers timeout) and provides `InitRules.bind_failure_fatal`. Every other handshake, headers, addr, ping and ConnMan behaviour is byte- and timing-identical between the pins (verified hunk by hunk, §3.12).
9. **Verification.** O13a transcripts per profile (outbound v1 and v2, against both dashd pins); `diff_addrman` (every answer and the canonical state equal after each operation under the shared deterministic RNG stream, plus `peers.dat` exchanged both ways); handler-level `diff_p2p_<cmd>` for the M2 command set against a new stateful oracle entry built from Core's own `process_message` fuzz harness (§6.2, a new O1c task); the Core unit vectors (`addrman_tests`, `net_tests`, `netbase_tests`, `denialofservice_tests`) and the functional tests of §6.5.
10. **Open questions** (§8): where `NodeClock` and the time-offset filter live; Core's `peers.dat` format; AddrMan as a mutex rather than an actor; the O1c oracle task; the O13a transcript window at M2; asmap and `InitBinds` pulled into P2a; P2P quirk numbering.

---

## 1. Scope

### 1.1 The PLAN row

| PLAN P2a deliverable | Where in this design |
|---|---|
| ConnMan: outbound, feelers, anchors, addnode, seeds | §2.4, §3.8, §4 |
| Handshake + `fingerprint.rs` from the release profile (`Identity`, `P2pRules`) | §2.3, §3.3 |
| Wake-driven actor loop (shuffle, MN-only throttle) | §2.2, §3.1 |
| Per-command decode policy and the gates | §3.2, §3.11 |
| Awaiting observable messages | §3.1.5 |
| Ping | §3.5 |
| AddrMan | §3.7, §4 |
| headers/headers2 sync | §3.4 |
| Acceptance: O13a transcripts identical per profile; addrman fuzz + bucket-math parity; behaviours 1–7, 11, 25 (outbound), +C; handler-level `diff_p2p_*` for the M2 command set; BG `header_sync_s` (window) and header-sync counters | §6, §5 |

PRD requirements: FR-P2P-05 (handshake), FR-P2P-06 (connection manager, outbound half), FR-P2P-07 (addresses), FR-P2P-08 (headers part), FR-P2P-16 (identity); NFR-FP-01 for behaviours 1–7, 11, 25 (outbound) and +C; NFR-FP-03 for the timers P2a owns (ping, feelers, extra peers, DNS-seed timing, addr broadcast).

### 1.2 In scope

- The crate `dashd-p2p` (A1 §2.2): its manifest, MIT licence with Dash Core's notice (DEC-07), `ARCHITECTURE.md`, the `release` module with **every** field of `Identity` and `P2pRules` as complete literals for both releases (O17 spec rule 4; later tasks read fields P2a does not).
- `fingerprint.rs`: the `version` message, local services, the user agent (`-uacomment`, the devnet comment, `-honestua`), `-pushversion`, the post-`version` and post-`verack` sequences.
- The node table (Core's `CNode` net-level fields), connection types, the per-node inbox (two queues, receive flood control) and outbox handle (send flood control).
- `ConnMan`, outbound side: `opencon`, `addcon`, `dnsseed`, `-connect`, `-seednode`/ADDR_FETCH, anchors, feelers, extra peers, `OpenNetworkConnection` over TCP (clearnet), the v2→v1 reconnect bookkeeping with P1, `DisconnectNodes` (linger, half-close), `InactivityCheck`, `-maxconnections` accounting, `setnetworkactive`, and `InitBinds` (bind and listen with `InitRules.bind_failure_fatal`; accepting is P2b).
- `AddrMan` (full port), `netgroup` with asmap interpretation, local-address bookkeeping (`mapLocalHost`, `SeenLocal`, `GetLocalAddrForPeer`), `peers.dat` and `anchors.dat`.
- The time-offset filter (`AddTimeData`) fed from outbound `version` messages.
- `PeerManager`: the actor, `PeerState` (Core's `Peer` ∪ `CNodeState`), `process_messages`, `process_message` (gates, policy, dispatch), `send_messages` in Core's order with the P3/P5 sections as seams.
- Handlers: `version`, `verack`, `sendheaders`, `sendheaders2`, `sendcmpct` (state only), `sendaddrv2`, `sendtxrcncl`, `ping`, `pong`, `addr`, `addrv2`, `getaddr`, `headers`, `headers2`, the block path of `inv`, the size checks of `getdata`/`notfound`, unknown commands, and the M2 stubs that make every other command Core-exact where that needs no later task's state: `EmptyServing` for `getdata`, `getblocks`, `mempool`, `blocktxn`, `getblocktxn`, `getcf*`, `qgetdata`, `qdata`, `mnlistdiff`, `qrinfo`, the DKG +10 fallback and the silent commands (§3.11).
- Headers sync, block announcements (headers/headers2/inv; the `cmpctblock` branch through P3's encoder), ping, addr relay, outbound eviction and stale-tip handling, the misbehaviour framework and the scores of P2a's commands.
- The `NetHandler` registry with per-release dispatch order, and `PeerManagerHandle` (the thread-safe API RPC and overlays use).
- The `sim` harness.

### 1.3 Non-goals (and their owners)

| Not P2a | Owner |
|---|---|
| V1/V2 framing, BIP324 handshake, short IDs, garbage, rekey, inbound v1 detection | P1 |
| Accepting inbound connections, inbound transcripts, `getheaders`/`getheaders2` serving, `getaddr` serving tests | P2b |
| Block download, stalling, compact blocks, direct fetch, `BlockChecked` punishment, `p2p_ibd_*` | P3 |
| Inbound eviction, BanMan persistence (`banlist.json`), whitelisting options, the rest of the score table, block/filter/bloom serving, `-maxuploadtarget`, `-capturemessages` | P4 |
| Tx relay, both object-request trackers, trickle, orphans, `mempool` replies | P5 |
| Overlay handlers (`clsig`, `spork`, govobj, CoinJoin, LLMQ), mnsync (`getsporks`, `govsync`), MN connections, MNAUTH, probes | P5a, P6a/b, SP0, SP1, P7, AM1 |
| SOCKS5, Tor, I2P, CJDNS, `-onlynet`, NAT-PMP/UPnP, `-discover` interface enumeration | P8 |
| Header acceptance, the block index, `m_best_header`, `NotifyHeaderTip` | CH1 |
| Option parsing, init order, logging sinks, `getpeerinfo`/`getnetworkinfo` RPC shapes | ND1, AR1, RP1a |
| The fingerprint lab and its probes | O13a/b (P2a ships the `sim` transcripts and fixtures they reuse) |

### 1.4 What the M2 node is

At M2 rdashd syncs headers only (no block download until P3, no mempool until MP1a, no overlays until M4). Its observable state is therefore that of a dashd in initial block download, before masternode sync, with an empty mempool. §3.11 lists, per command, what dashd does in that state and what P2a does; where they differ the row is an **M2 interim deviation**, owned and closed by a named later task. O13a's transcript window (§6.1) stops before the first message a later task owns.

---

## 2. Crate, runtime shape and API

### 2.1 Reuse

| Source | Decision | Why |
|---|---|---|
| base-sdk `dash-p2p-core` (BS4a/BS7) | **Use** for every payload type, `P2pMsg::decode_with_policy`, `ProtoCtx`, `CommandString`, `ServiceFlags`, `ProtocolVersion`, the headers2 codec | Rule DDE lives there; A1 §1.4 |
| #264 transport (`Transport`, V1/V2, `V2Session`, Dash short IDs) | **Use through P1** (`dash-p2p-transport`) | A1 §2.14; P2a sees only `RawMsg`/`OutMsg` |
| dash-spv `src/network/*` (`manager.rs`, `discovery.rs`, `addrv2.rs`, `handshake.rs`, `reputation.rs`) | **Not reused** | SPV-shaped by design: masternode IP lists as seeds (`discovery.rs:37-60`), a `HashMap` address store capped at 2000 (`addrv2.rs:18-27`), `relay=false`, services `HEADERS_COMPRESSED` only, `start_height` 0 (`handshake.rs:250-290`), reputation scores instead of Core's misbehaviour model, timeouts unlike Core's (`constants.rs`). A1 §2.1 rule 8 forbids touching it (#902) |
| `dash-network-seeds` | **Not used** | it is a masternode IP list; Core's fixed seeds are `chainparamsseeds.h` (§3.8.6) |
| dash-spv `test-utils` (`DashdTestContext`, `DashCoreNode`) via `dash-testkit` (O16) | **Use** for live interop tests against dashd | no code moves |
| `contrib/node/dde-probe` (BS4a) | **Use** as the raw-P2P probe base for `diff_p2p_*` corpus capture | already drives both dashd pins |
| Core's functional framework `P2PInterface` / `v2_p2p.py` | **Use** in O13a | handles v1 and v2 |

### 2.2 Runtime shape

```
             tokio "net" runtime (A1 §3.1)                                     dedicated OS threads
 ┌────────────────────────────────────────────────────────────┐        ┌──────────────────────────────────┐
 │ ConnMan tasks: opencon · addcon · dnsseed · disconnector     │        │ peerman: PeerManager actor        │
 │   (+ P2b: accept)  (+ P7: mncon)                             │        │   loop = ThreadMessageHandler      │
 │ per node: reader task (P1 transport → RawMsg, speculative    │ wake → │   PeerLogic (sans-io)              │
 │           decode+X11 for headers/headers2) → Inbox           │───────►│   blocks on ChainCommand replies   │
 │           writer task (P1 transport ← OutMsg)                │◄───────│   pushes OutMsg into Outbox        │
 │ scheduler: DumpAddresses 15 min, anchors at shutdown          │ NodeTbl│   timers: stale-tip check 45 s     │
 └────────────────────────────────────────────────────────────┘        └──────────────────────────────────┘
          ▲  NodeTable (shared, Core's m_nodes of Arc<NodeHandle>)  ▲             │ ChainCommand / TipFacts
          └── AddrMan: Mutex<AddrManImpl> (Core's m_mutex) ─────────┘             ▼
                                                                         validation thread (CH1)
```

- **The node table is shared, as Core's `m_nodes` is.** `NodeTable` is an `RwLock<Vec<Arc<NodeHandle>>>` in insertion order (`ForEachNode` order), and `NodeHandle` carries Core's `CNode` net-level fields as atomics or small mutexes, exactly where Core uses atomics or `cs_*` locks. Both ConnMan (counting slots, choosing addresses, disconnecting) and PeerManager (protocol flags) read it without message passing, so ConnMan's decisions see the same state Core's `ThreadOpenConnections` sees. Message passing here would create orderings dashd never produces.
- **AddrMan is a `Mutex<AddrManImpl>` called synchronously** (Core: `AddrManImpl::cs`), not an actor. Every caller in Core calls it synchronously and acts on the answer at once (`Select` then `OpenNetworkConnection`, `Good` inside the `version` handler). §8 Q3 records the deviation from A1's "small actor".
- **The actor is a plain thread with blocking waits.** It parks on a `Condvar` with `fMsgProcWake` semantics (§3.1.1); readers set the flag and notify. Awaiting a `ChainCommand` reply is a blocking `recv` on the actor thread, exactly as `msghand` blocks on `cs_main`.
- **Writers do framing and encryption.** The actor serializes the payload (it owns the negotiated version) and pushes `OutMsg { cmd, payload }`; P1's writer frames and, for v2, encrypts in send order. Send-buffer accounting (`fPauseSend`) is updated synchronously in `push_message`, before the writer runs, as in `CConnman::PushMessage` (`v24:net.cpp:4857-4900`, (=)).
- **Overlays push directly.** Core's overlay threads call `CConnman::PushMessage` and `PeerManagerInternal` methods from their own threads. `NodeTable::push_message(id, OutMsg)` is thread-safe for that reason; `PeerManagerHandle` methods that Core executes under `cs_main` from other threads become commands drained at the top of the next actor pass (one of the interleavings Core allows).

### 2.3 Modules

```
dashd-p2p/src/
  lib.rs            pub use; crate docs; the Core-file map (ARCHITECTURE.md mirrors it)
  release.rs        Identity, P2pRules (O17 §4.2), V23_1/V24_0 literals, of(Release)
  fingerprint.rs    LocalIdentity (UA, services, version fields), VersionPayload builder, handshake plans
  clock.rs          (until §8 Q1 is decided) NodeClock trait use, GetExponentialRand
  net/
    node.rs         NodeId, ConnectionType, NodeHandle (CNode), Inbox (two queues), OutboxHandle
    nodes.rs        NodeTable, NodesSnapshot (shuffle), connection counts by type and network
    connman.rs      ConnMan: start/stop, InitBinds, OpenNetworkConnection, DisconnectNodes, InactivityCheck
    opencon.rs      ThreadOpenConnections port (slots, anchors, feelers, extra peers, address choice)
    addcon.rs       ThreadOpenAddedConnections port; GetAddedNodeInfo
    seeds.rs        DNS seeds, fixed seeds (generated from chainparamsseeds.h), -seednode, -connect
    addrman/        mod.rs (AddrMan API), imp.rs (AddrManImpl, Core port), info.rs (AddrInfo, chance,
                    terrible), bucket.rs (bucket math), ser.rs (peers.dat format), check.rs (consistency)
    netaddr.rs      CNetAddr/CService/CAddress semantics over base-sdk's address types (routability, RFC
                    classes, GetNetwork, IsAddrV1Compatible, GetKey)
    netgroup.rs     NetGroupManager (GetGroup, asmap Interpret/GetMappedAS)
    local.rs        mapLocalHost, AddLocal/RemoveLocal, SeenLocal, GetLocal, GetLocalAddrForPeer, IsReachable
    addrdb.rs       peers.dat / anchors.dat read/write (magic, checksum, tmp+fsync+rename)
    permissions.rs  NetPermissionFlags (the type and the flags P2a's paths read; P4 adds the options)
    timedata.rs     the time-offset median filter (until §8 Q1)
  processing/
    peerman.rs      PeerManager actor thread, MessageLoop, command queue, timers
    logic.rs        PeerLogic: initialize_node, finalize_node, process_messages, send_messages
    peer.rs         PeerState (Peer ∪ CNodeState), ChainSyncTimeoutState
    policy.rs       CommandPolicy table (74 rows), Gate, PreRead, ReadPlan, ThrowOutcome, M2Behaviour
    dispatch.rs     process_message: gates → handler (which decodes at Core's read point); NetHandler registry; unknown commands
    handshake.rs    version, verack, sendheaders(2), sendcmpct, sendaddrv2, sendtxrcncl; push_node_version
    ping.rs         MaybeSendPing, ping, pong
    addr.rs         addr, addrv2, getaddr, PushAddress, RelayAddress, MaybeSendAddr, the addr cache front end
    headers.rs      headers, headers2, sync start, getheaders policy, unconnecting, continuity, timeouts
    inv.rs          inv (block path, blocks-only, unknown types), getdata/notfound size checks
    announce.rs     block announcements (SendMessages section)
    eviction.rs     ConsiderEviction, EvictExtraOutboundPeers, CheckForStaleTipAndEvictPeers
    misbehavior.rs  Misbehaving, MaybeDiscourageAndDisconnect, MaybePunishNodeForBlock, the score table
    seams.rs        ChainAccess, BlockSync, ObjectRequests, TxRelay, BanList, OrphanStep, Serving
    handle.rs       PeerManagerHandle, PeerStats snapshots
  sim/              (feature "sim", also used by dev-tests) virtual clock, in-memory transports, scripted peers
```

### 2.4 Public API (Rust signatures)

```rust
// ---- release.rs (O17 §4.2; P2a defines every field) ----
pub struct Identity {
    /// O17 P2P-01, P2P-03. v23.1.8 version.h:14 = 70240; v24 version.h:14 = 70242.
    pub protocol_version: i32,
    /// O17 P2P-02, DKG-01. v23 version.h:23 = 70240; v24 = 70242.
    pub min_mn_proto_version: i32,
    /// O17 P2P-01. "23.1.8" | "24.0.0" (FormatVersion(client_version); no rc suffix).
    pub ua_version: &'static str,
    /// O17 P2P-01. 230108 | 240000.
    pub client_version: u32,
}
pub struct P2pRules { /* O17 §4.2, all ten fields: object_download, notfound_limit, unsolicited_scoring,
                         tx_inv_before_verack, msg_asset_unlock, mnauth_inbound_cap,
                         request_gov_invs_when_disabled, qrinfo_request, mnlist_data_unavailable_score,
                         block_read_vs_prune */ }
impl Identity { pub const V23_1: Identity; pub const V24_0: Identity; pub fn of(r: Release) -> &'static Identity; }
impl P2pRules { pub const V23_1: P2pRules; pub const V24_0: P2pRules; pub fn of(r: Release) -> &'static P2pRules; }

// ---- fingerprint.rs ----
pub struct LocalIdentity {                 // built once by ND1 at init
    pub identity: &'static Identity,
    pub user_agent: UserAgent,              // "/Dash Core:<ua_version>[(<comments>)]/"; ≤ 256 (InitError text in §3.3.2)
    pub push_version: Option<i32>,          // -pushversion, non-mainnet only (v24:net_processing.cpp:1577-1580, (=))
    pub local_services: ServiceFlags,       // §3.3.3
}
impl LocalIdentity {
    pub fn new(identity: &'static Identity, params: &ChainParams, opts: &IdentityOptions) -> Result<Self, InitError>;
    pub(crate) fn version_payload(&self, v: &VersionInputs) -> Version;   // base-sdk p2p_core::Version
}

// ---- net/node.rs ----
pub type NodeId = i64;                                 // Core NodeId (int64_t)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionType { Inbound, OutboundFullRelay, Manual, Feeler, BlockRelay, AddrFetch }  // node/connection_types.h
pub struct NodeHandle { /* Core CNode, net level: id, addr (CAddress, services from addrman), addr_bind,
    addr_name, conn_type, inbound_onion, keyed_net_group, local_host_nonce, connected (mockable seconds),
    permissions, transport_info; atomics: disconnect, successfully_connected, pause_send, pause_recv,
    other_side_disconnected, version, common_version, relays_txs, version_services, version_relay_txs,
    masternode_connection, masternode_probe, masternode_iqr, last_send, last_recv, last_block_time,
    last_tx_time, min_ping_time, time_offset, bip152_hb_to/from, has_all_wanted_services; small mutexes:
    clean_subver, addr_local, mnauth challenges (sent/received), verified_protx; inbox; outbox */ }
impl NodeHandle {
    pub fn push_message(&self, msg: OutMsg);           // Core CConnman::PushMessage: thread-safe
    pub fn request_disconnect(&self);                 // fDisconnect = true
}
pub struct RawMsg { pub cmd: CommandString, pub payload: Bytes, pub recv_time: Micros, pub raw_size: usize,
                    pub speculative: Option<Arc<Speculative>> }   // P1 → P2a; §5.2
pub struct OutMsg { pub cmd: CommandString, pub payload: Bytes }   // P2a → P1

// ---- processing seams (seams.rs) ----
pub trait ChainAccess: Send {                          // CH1 implements; A1 §2.10, §2.10.2
    fn tip(&self) -> TipFacts;                         // unthrottled ArcSwap (§3.4.6)
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId>;
    fn entry(&self, id: BlockId) -> IndexEntryView;   // hash, height, prev, time, bits, chain_work, status
    fn ancestor(&self, id: BlockId, height: u32) -> BlockId;
    fn active_at(&self, height: u32) -> Option<BlockId>;
    fn locator(&self, id: BlockId) -> Vec<BlockHash>; // Core GetLocator (identical at both pins, O17 §3 note)
    fn loading_blocks(&self) -> bool;                  // LoadingBlocks() / fImporting||fReindex
    fn min_chain_work(&self) -> Arith256;
    fn process_headers(&self, batch: HeaderBatch, from: NodeId) -> HeadersReply;   // blocks; ChainCommand::ProcessHeaders
}
pub struct HeaderBatch { pub headers: Vec<BlockHeader>, pub hashes: Vec<BlockHash> }   // hashes[i] = X11(headers[i])
pub struct HeadersReply { pub last_accepted: Option<BlockId>, pub invalid: Option<BlockReject>, pub tip: TipFacts }
pub struct TipFacts { pub tip: BlockId, pub tip_work: Arith256, pub tip_time: u32, pub tip_height: u32,
                      pub best_header: BlockId, pub best_header_time: u32, pub ibd: bool, pub epoch: u64 }

pub trait BlockSync: Send {        // P3; M2 ships NoBlockSync (every method a no-op returning "nothing to do")
    fn headers_direct_fetch(&mut self, cx: &mut Cx, peer: NodeId, last: BlockId);
    fn send_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros) -> SendFlow;   // stall, block timeout, getdata(blocks)
    fn blocks_in_flight(&self, peer: NodeId) -> usize;
    fn finalize(&mut self, peer: NodeId);
}
pub trait ObjectRequests: Send {   // P5 builds Legacy or TxRequest per P2pRules.object_download; M2: NoObjectRequests
    fn add_announcement(&mut self, peer: NodeId, inv: Inv, preferred: bool, now: Micros);
    fn send_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros);               // getdata (non-blocks)
    fn finalize(&mut self, peer: NodeId);
}
pub trait TxRelay: Send { fn send_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros); /* trickle; P5 */ }
pub trait OrphanStep: Send { fn step(&mut self, cx: &mut Cx, peer: NodeId) -> OrphanStepResult; }  // P2P-13 semantics are P5's
pub trait BanList: Send + Sync {   // P4's BanMan implements; M2 ships DiscourageOnly (Core's 50,000 / 1e-6 rolling bloom)
    fn discourage(&self, addr: &NetAddr); fn is_discouraged(&self, addr: &NetAddr) -> bool; fn is_banned(&self, addr: &NetAddr) -> bool;
}

// ---- NetHandler (corrects A1 §2.14: mirrors Core's NetHandler, net_processing.h:90-113 v23 / :122-145 v24) ----
pub trait NetHandler: Send + 'static {
    fn commands(&self) -> &'static [CommandString];
    fn process_message(&mut self, cx: &mut HandlerCx<'_>, peer: NodeId, msg: &DecodedMsg);  // on the actor; may block
                                                                                           // only where Core does (§3.1.5)
    fn already_have(&self, inv: &Inv) -> bool { false }
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, peer: NodeId, inv: &Inv) -> bool { false }
    fn schedule(&mut self, _s: &Scheduler) {}       // Core NetHandler::Schedule
    fn start(&mut self) {} fn stop(&mut self) {} fn interrupt(&mut self) {}
}

// ---- PeerManager and its handle ----
pub struct PeerManagerConfig { pub identity: LocalIdentity, pub rules: &'static P2pRules,
                               pub consensus: &'static UngatedRules, pub params: Arc<ChainParams>,
                               pub opts: PeerManagerOptions /* -blocksonly, -peerbloomfilters, -txreconciliation,
                                                               -maxtipage, -capturemessages hook (P4), … */ }
impl PeerManager {
    pub fn spawn(cfg: PeerManagerConfig, nodes: Arc<NodeTable>, addrman: Arc<AddrMan>, chain: Box<dyn ChainAccess>,
                 seams: Seams, clock: Arc<dyn NodeClock>) -> (PeerManagerHandle, JoinHandle<()>);
}
pub struct PeerManagerHandle { /* Arc to the command queue + wake */ }
impl PeerManagerHandle {
    pub fn initialize_node(&self, node: Arc<NodeHandle>);  // ConnMan after connect/accept; outbound pushes `version`
    pub fn finalize_node(&self, id: NodeId);                // ConnMan after DeleteNode
    pub fn chain_event(&self, ev: ChainEventLite);          // UpdatedBlockTip (best height, IBD cache, announcements)
    pub fn send_pings(&self);                               // RPC `ping`: sets m_ping_queued for every peer
    pub fn stats(&self, id: NodeId) -> Option<PeerStats>;   // lock-free snapshot (getpeerinfo)
    pub fn misbehaving(&self, id: NodeId, howmuch: i32, msg: &str);  // PeerManagerInternal::PeerMisbehaving
    pub fn wake(&self);
    pub fn shutdown(&self);
}

pub struct ConnMan { /* … */ }
impl ConnMan {
    pub fn start(opts: ConnManOptions, nodes: Arc<NodeTable>, addrman: Arc<AddrMan>, peerman: PeerManagerHandle,
                 transport: Arc<dyn TransportFactory>, ban: Arc<dyn BanList>, clock: Arc<dyn NodeClock>)
                 -> Result<ConnMan, InitError>;            // InitBinds + thread start, Core CConnman::Start order
    pub fn add_node(&self, s: &str) -> bool;              // addnode add/remove/onetry back ends
    pub fn remove_added_node(&self, s: &str) -> bool;
    pub fn open_one_try(&self, dest: &str, conn_type: ConnectionType, v2: bool) -> bool;  // addnode onetry; addconnection (regtest)
    pub fn disconnect_node(&self, by: DisconnectBy) -> bool;
    pub fn set_network_active(&self, active: bool);
    pub fn counts(&self) -> ConnectionCounts;
    pub fn stop(self);                                    // DumpAnchors, DumpAddresses, Core's shutdown order
}

// ---- P1 seam ----
pub trait TransportFactory: Send + Sync {
    /// Starts a connection's transport on an already connected stream. `v2` is decided by ConnMan
    /// (automatic dials: addrman's services for the peer & ours & NODE_P2P_V2; manual, -connect, seed
    /// and MN dials: ours alone; §3.8.2 step 10, §3.8.5). Returns the reader and writer halves.
    fn outbound(&self, stream: TcpStream, v2: bool, node: Arc<NodeHandle>) -> (Box<dyn FrameReader>, Box<dyn FrameWriter>);
    fn inbound(&self, stream: TcpStream, node: Arc<NodeHandle>) -> (Box<dyn FrameReader>, Box<dyn FrameWriter>); // P2b
}
pub enum ReaderEvent { Messages(Vec<RawMsg>), Closed { other_side: bool }, Fatal(TransportFault), V2HandshakeFailedEarly }
```

The `Cx`/`HandlerCx` context gives a handler exactly what Core's `PeerManagerImpl` methods reach: the peer's `PeerState`, the node handle, `push_message`, `misbehaving`, the chain access, addrman, the clock, the RNG and the release groups. Handlers of later tasks receive the same context; nothing outside `processing/` can mutate `PeerState`.

### 2.5 Neighbours and the exact types exchanged

| Neighbour | Direction | Types | Contract |
|---|---|---|---|
| **P1** `dash-p2p-transport` | P1 → P2a | `RawMsg`, `ReaderEvent` | one `ReaderEvent::Messages` per socket read: every message completed by that read, in arrival order, all stamped with the read's time (Core `CNode::ReceiveMsgBytes`, `v24:net.cpp:649-692` (=)). A frame with a bad checksum or invalid command is dropped inside P1 and never surfaces; bad magic, oversize or a V2 failure surfaces as `Fatal` and ConnMan closes the socket (FR-P2P-03) |
| | P2a → P1 | `OutMsg`; `NodeHandle.common_version` | the writer encodes short IDs only once the common version allows it (A1 §2.14); the actor stores `common_version` before it pushes `sendaddrv2`/`verack`, so the writer sees it for every post-`version` message |
| | P1 → ConnMan | `ReaderEvent::V2HandshakeFailedEarly` | ConnMan records a v1 reconnect, as Core's `m_reconnections` (§3.8.5) |
| **P2b** inbound | P2b → P2a | `ConnectionType::Inbound` `NodeHandle`s via `initialize_node` | P2a's `version` handler already implements the inbound branches (shy `version`, self-connection nonce, inbound `fMasternode`, `SeenLocal`); P2b adds accept, eviction-free admission and serving, and owns their tests |
| **CH1** | P2a → CH1 | `HeaderBatch`, `NodeId` | `process_headers` is `ChainCommand::ProcessHeaders`; CH1 validates `hashes[i] == X11(headers[i])` only in debug builds (Core `ASSERT_IF_DEBUG`, `v24:validation.cpp:4602`) |
| | CH1 → P2a | `HeadersReply`, `TipFacts`, `ChainEventLite::UpdatedBlockTip{new, fork, ibd}` | `TipFacts` must be published **before** the reply and never throttled (§3.4.6; amends A1 §2.10.2's 100 ms IBD throttle for this field set) |
| **P3** | P2a ↔ P3 | `BlockSync`, `CompactBlockSource` | P3 replaces `NoBlockSync`; the announcement code calls `CompactBlockSource::cmpctblock_for(tip)` for the HB branch |
| **P4** | P2a ↔ P4 | `BanList`, `NetPermissionFlags`, score-table extensions | P4 replaces `DiscourageOnly` with BanMan |
| **P5** | P2a ↔ P5 | `ObjectRequests`, `TxRelay`, `OrphanStep` | P5 builds one tracker per `P2pRules.object_download` (O17 §4.4) |
| **P5a/SP0/SP1/P6/P7** | overlays → P2a | `NetHandler`, `PeerManagerHandle`, `NodeTable::push_message` | registered by ND1 in Core's per-release order (§3.2.6) |
| **ND1** | ND1 → P2a | `PeerManagerConfig`, `ConnManOptions`, `LocalIdentity` | init step 13 (A1 §2.18) |
| **RP1a** | RPC → P2a | `PeerManagerHandle::stats`, `ConnMan::counts`, addrman queries | `getpeerinfo`, `getnetworkinfo`, `getconnectioncount`, `getaddednodeinfo`, `getnodeaddresses`, `addnode`, `disconnectnode`, `setnetworkactive`, `ping` back ends |
| **LQ4a/DK1** | P2a → them | `NodeHandle.version_services`, `.version_relay_txs` (O17 DKG-01) | recorded in the `version` handler as v24 does (`v24:net_processing.cpp:4060-4061`); v23 does not record them, but the fields are internal, so recording them under both profiles is unobservable |

### 2.6 Hooks for the overlays (what later tasks plug into)

Core's overlays reach the peer manager through the VERSION/VERACK handlers, `PeerManagerInternal` and `CConnman`, from the message thread and from their own threads. P2a provides each of these now, with a no-op or Core-equivalent default for the M2 node, so P5a, P6a/b, P7, SP0, SP1, AM1 and DK1 only fill them in:

| Hook | Called | Provides | Default at M2 | Owner that fills it |
|---|---|---|---|---|
| version tail out/in | building our `version`; parsing theirs (§3.3.4–§3.3.5) | the 32-byte challenge, `fMasternode`; the inbound MN rule | implemented (both directions) | AM1/P7 read the challenges |
| `verack` slot 1 | first push of the `verack` handler | `mnauth` | none (not a masternode) | AM1 |
| `verack` slot 2 | after `senddsq` | `qwatch` (watching and the peer is a quorum node) | none (`-watchquorums` unsupported until DK1) | DK1/P7 |
| first post-`verack` message | before the blocks-only gate | probe rule (non-`mnauth` first message → disconnect) | implemented | P7 sets the probe flag |
| `NetHandler` registry | extension dispatch (§3.2.6) | per-command handlers, `already_have`, `process_getdata`, `schedule` | the DKG +10 fallback; silence otherwise | every overlay task |
| `MessageProcessingResult` applier | after a handler returns | misbehaviour → erase request → relay txs → relay invs | implemented (relay to nobody's queue is a no-op until P5) | — |
| `PeerManagerHandle` services | any thread | `misbehaving`, `relay_inv` (every peer, **no** `CanRelay` filter, peers mid-handshake included), `relay_inv_filtered`, `relay_dsq` (by `wants_dsq`), `relay_recsig` (by `wants_recsigs`; proactive → direct push), `push_inventory`, `ask_peers_for_tx`, `requested_object_count`, `is_banned`, object-request erase/consume/forget | `misbehaving`, `relay_inv`, `push_inventory`, `is_banned` implemented; the object-request ones forward to `ObjectRequests` (no-op) | P5, P5a, P6a, SG1a |
| `NodeTable` services | any thread | `push_message` (per-peer FIFO), the fully-connected snapshot **in connection order, unshuffled** (the sync tick depends on it), all node ids, per-node MN fields (`verified_protx`, `verified_pubkey`, `iqr`, `probe`, `qwatch`, `first_msg_is_mnauth`) | implemented | SP1, P7, AM1, LQ5 |
| `CanRelay()` | `verack`, every send pass, relay helpers, the sync tick | `!masternode_connection || masternode_iqr` | implemented | P7 sets the flags |
| connection-count change | socket task | 0 ↔ non-zero transitions (mnsync's non-forced reset) | event emitted, nobody listens | SP1 |
| quorum connection sets | ConnMan | `set_quorum_nodes`, `set_quorum_relay_members` (pushes `qsendrecsigs` to verified members and sets `iqr`), `is_quorum_node` | stored, unused | P7, LQ5 |
| `EvictExtraOutboundPeers` exemption | 45 s task | never evict `masternode_connection` peers | implemented | — |
| scheduler + validation-event executor | init | one serial executor for timers and queued validation events (Core's `scheduler` thread) | provided by ND1; P2a's timers run on the actor (§3.1.6) | ND1, all overlays |

`getsporks`, `mempool` and `govsync` are **not** sent by the peer manager at either pin: the only sender is `SyncManager`'s tick on the scheduler (every 1 s, acting every ≥ 6 s of mockable time while masternode sync is unfinished; `node/sync_manager.cpp:142-332/138-328`), so they belong to SP0/SP1 with the hooks above. The first `getsporks` reaches a new peer U(0, 6] s after its `verack` and `govsync` U(6, 12] s after, not "0.5–2 s" and "≈ 8 s" (A1 §2.14; §3.15).

### 2.7 Options P2a implements

Every Core option below is registered at both pins (`init.cpp` `AddArg`, v23 lines 557-775 / v24 606-828) with the same default (`net.h:102-122/120-142`, `net_processing.h:46-47/44-45`, `netbase.h:29-32/25-28`, `timedata.h:16`, `addrman.h:41`, `node/txreconciliation.h:15`; `DEFAULT_MAX_TIP_AGE` moved from `validation.h:82` to `kernel/chainstatemanager_opts.h:18`, still 6 h). `-honestua` is ours (§8 Q10). ND1 parses; P2a consumes through `ConnManOptions`/`PeerManagerOptions`/`IdentityOptions`. `-pushversion` is ignored on mainnet at both pins (`net_processing.cpp:1557/1578`).

| Option | Default | Used by |
|---|---|---|
| `-maxconnections` | 125 (reduced to fit descriptors) | §3.8.8 |
| `-connect`, `-addnode`, `-seednode` | — | §3.8.2, §3.8.5 |
| `-dnsseed`, `-forcednsseed`, `-fixedseeds` | 1, 0, 1 | §3.8.4, §3.8.6 |
| `-listen`, `-bind` | 1 (soft-off with `-connect`), — | §3.8.9 (accepting: P2b) |
| `-externalip`, `-allowprivatenet` | —, 0 | §3.7.4, §3.7.3 |
| `-networkactive` | 1 | §3.8.5 |
| `-v2transport` | 1 | §3.3.3, §3.8.2 |
| `-peertimeout`, `-timeout` | 60 s, 5000 ms | §3.8.7, §3.8.5 |
| `-maxsendbuffer`, `-maxreceivebuffer` | 1000, 5000 (× 1000 bytes) | §3.1.3, §3.8.7 |
| `-maxtimeadjustment` | 4200 s | §3.3.8 |
| `-maxtipage` | 6 h | §3.4.2 |
| `-blocksonly` | 0 | gate 7, §3.3.4 relay flag |
| `-peerbloomfilters`, `-peerblockfilters` | 1, 0 | §3.3.3 |
| `-uacomment`, `-honestua` (rdashd only), `-pushversion` (non-mainnet) | — | §3.3.2, §3.3.4 |
| `-checkaddrman` (debug) | 0 | §3.7.2 |
| `-asmap` | — | §3.7.3 (interpreter); loading UX P8 (§8 Q6) |
| `-txreconciliation` | 0 | §3.3.5 step 13 (off path only; FR-P2P-17) |
| `-proxy`, `-onion`, `-i2psam`, `-onlynet`, `-discover`, `-upnp`, `-natpmp`, `-whitebind`, `-whitelist`, `-maxuploadtarget`, `-capturemessages` | — | not P2a (P8, P4); InitError until their owner lands (§8 Q12) |

---

## 3. Core equivalence

Every subsection names the Core functions it replaces, gives the order of operations, and states the per-profile behaviour. "(=)" after a pair of line numbers means the logic is the same at both pins (checked by diffing the function bodies; differences that are only renames, `Misbehaving(NodeId, …)` → `Misbehaving(Peer&, …)` or `fImporting||fReindex` → `LoadingBlocks()` are not called out).

### 3.1 The actor loop (Core `ThreadMessageHandler`, `ProcessMessages`, `PollMessage`)

#### 3.1.1 The loop

Core: `v23:net.cpp:3682-3727` / `v24:net.cpp:3676-3721`.

```rust
// processing/peerman.rs — one OS thread, holds no lock across iterations except its own state.
loop {
    if interrupted { return }
    let mut more_work = false;
    // One global steady-clock throttle for masternode connections (v24 SteadyClock; v23 SystemClock in ms,
    // same 100 ms value; O17 notes #7283 — differs only under wall-clock jumps, not modelled).
    let skip_mn_send = steady_now - last_mn_send < 100ms;
    if !skip_mn_send { last_mn_send = steady_now }
    self.drain_commands();                                  // §2.2: PeerManagerHandle commands, finalize_node, chain events
    let snap = nodes.snapshot_shuffled(&mut rng);           // NodesSnapshot(AllNodes, shuffle=true): copy of m_nodes, then Shuffle
    for node in snap {
        if node.disconnect() { continue }
        let more_node = self.logic.process_messages(&node); // §3.1.2
        more_work |= more_node && !node.pause_send();
        if interrupted { return }
        if !skip_mn_send || !node.masternode_connection() { self.logic.send_messages(&node) }   // §3.1.4
        if interrupted { return }
    }
    // WAIT_LOCK(mutexMsgProc); if !fMoreWork wait_until(now + 100 ms, fMsgProcWake); fMsgProcWake = false
    let mut g = wake.lock();
    if !more_work { wake.cond.wait_until(&mut g, steady_now + 100ms, |w| w.flag) }
    g.flag = false;
    self.run_due_timers();                                  // §3.1.6
}
```

- The shuffle is over a **snapshot**, so a node added during the pass is first seen on the next pass, and a node removed during the pass is still visited (its `disconnect` flag skips it), as in Core (`v24:net.cpp:4943-4959` (=)).
- `fMsgProcWake` is reset after the wait **whether or not** the thread waited, so a wake that arrives during a busy pass is consumed without an extra pass. Reproduced as written.
- The RNG is a `FastRandomContext`-equivalent ChaCha20 stream seeded from OS randomness; in `sim` it is seeded explicitly. The order is not observable directly, only statistically through cross-peer interleavings, which +C and +D probe.

#### 3.1.2 `process_messages(node)` (Core `ProcessMessages`)

Core: `v23:net_processing.cpp:5728-5801` / `v24:5872-5947`. The two pins differ only in the orphan step, which is P2P-13 (`PolicyRules.orphan_processing`, P5).

1. Look up the peer; none → return false.
2. If the peer has pending `getdata` requests, run `ProcessGetData` (the `Serving` seam; P4/P5; empty at M2).
3. The orphan step through `OrphanStep` (P5). v23: `has_more_orphans = ProcessOrphanTx(peer)` (`v23:5745`), and if true return true after the disconnect check (`:5751`). v24: `processed_orphan = ProcessOrphanTx(peer)` (`v24:5886`), and if true return true after the disconnect check (`:5891`). Both are behind one seam call that returns whether to return `true`; the disconnect check sits between the call and that return at both pins, so one call site serves both. At M2 the seam is `NoOrphans` (false).
4. If `node.disconnect` → return false.
5. If `getdata` requests are still pending → return true ("maintains the order of responses").
6. If `pause_send` → return false (the send buffer is full; do not read).
7. `PollMessage` (§3.1.3); none → return false.
8. `-capturemessages` hook (P4), then `msg.SetVersion(common_version)`.
9. `process_message(node, msg)` (§3.2) inside Core's exception boundary: a decode error or a handler `Err(Throw)` is logged `"ProcessMessages(%s, %u bytes): Exception '%s' (%s) caught"` and costs nothing (`v23:5795` / `v24:5941`). Rust has no exceptions; every handler returns `Result<(), Throw>` and the boundary reproduces the log line with Core's `what()` text where a test asserts it (§3.2.4).
10. After a successful call: pending `getdata` → `more = true`; v24 only: an orphan ready for this peer → `more = true` (`v24:5939`, P5's seam).
11. Return `more` (the `PollMessage` "more" bit, possibly raised by step 10).

#### 3.1.3 Two receive queues (Core `MarkReceivedMsgsForProcessing`, `PollMessage`)

Core: `v23:net.cpp:4732-4814` / `v24:net.cpp:4773-4855` (=); the classification `IsQuorumPriorityMessage` is `v23:net.h:2012-2029` / `v24:net.h:2053-2070` (=).

A1 §2.14 says the reader queues messages "in arrival order". Core does not process them in arrival order:
- every completed message is classified on arrival: `qsigshare`, `qbsigs`, `qsigsinv`, `qgetsigs`, `qsigsesann`, `qsigrec`, `clsig` and `isdlock` go to the **quorum queue**, everything else to the **normal queue**;
- `PollMessage` takes from the quorum queue first, unless the normal queue is non-empty and 100 quorum messages have been taken since the last normal one (`QUORUM_TO_NORMAL_RATIO`); a quorum message taken while the normal queue is empty does not count toward the ratio; taking a normal message resets the count;
- "more" is true if either queue is non-empty after the pop;
- `fPauseRecv = quorum_bytes + normal_bytes > m_recv_flood_size` is recomputed after every classification batch and every pop. The flood size is `-maxreceivebuffer` × 1000 bytes (default 5000 → 5 MB).

Observable consequence: a peer that writes `ping` then `clsig` in one segment gets the `pong` **after** the CLSIG has been processed. The Rust `Inbox` has the two queues, the counter and the byte totals behind one mutex, and the reader task performs the classification on `RawMsg.cmd` before it wakes the actor. Messages are stamped with the time of the socket read that completed them; all messages completed by one read are moved into the queues together, then the actor is woken once (`v24:net.cpp:2611-2617`).

Receive flow control: when `pause_recv` is set the reader stops reading the socket (Core's socket handler skips `RECV` events for that node), and it resumes when a pop clears it; the actor signals the reader through a `Notify` on the transition.

#### 3.1.4 `send_messages(node)` (Core `SendMessages`)

Core: `v23:net_processing.cpp:6154-6738` / `v24:6300-6899`. Order of sections (the order is observable: it is the order of messages a peer receives in one pass):

| # | Section | Core (v23 / v24) | Owner |
|---|---|---|---|
| 1 | `MaybeDiscourageAndDisconnect`; if it acted → return | `:6168` / `:6312` | P2a (§3.10) |
| 2 | not `fSuccessfullyConnected` or `fDisconnect` → return | `:6171` / `:6315` | P2a |
| 3 | ADDR_FETCH older than `10 × AVG_ADDRESS_BROADCAST_INTERVAL` (300 s) → disconnect "addrfetch connection timeout; disconnecting peer=%d" | `:6179-6183` / `:6323-6327` | P2a |
| 4 | `MaybeSendPing` (may disconnect on ping timeout → return) | `:6185` / `:6329` | P2a (§3.5) |
| 5 | `MaybeSendAddr` | `:6190` / `:6334` | P2a (§3.6) |
| 6 | start block sync: the initial `getheaders`/`getheaders2` | `:6197-6250` / `:6342-6396` | P2a (§3.4.2) |
| 7 | block announcements: `cmpctblock` (one header to an HB peer) / `headers2` / `headers` / fall back to `inv` | `:6254-6407` / `:6399-6552` | P2a (§3.4.8); the `cmpctblock` encoder is P3's |
| 8 | `inv`: queued block invs, then the tx trickle and `mempool` reply (P5 `TxRelay`), then `vInventoryOtherToSend` (non-tx invs: immediate path, ISDLOCK only if `m_relay_txs && !IsBlockRelayOnly`), one `inv` flush | `:6410-6592` / `:6555-6750` | P2a for blocks and the other-inv queue; P5 for tx |
| 9 | stalling detection, block download timeout | `:6594-6622` / `:6752-6781` | P3 (`BlockSync::send_section`) |
| 10 | headers sync timeout (15 min + 1 ms per expected header; noban peers are reset instead of disconnected) | `:6624-6656` / `:6782-6813` | P2a (§3.4.5) |
| 11 | `ConsiderEviction(GetTime())` | `:6659` / `:6817` | P2a (§3.9) |
| 12 | `getdata` for blocks | `:6662-6682` / `:6820-6838` | P3 |
| 13 | `getdata` for objects (the object-request tracker) | `:6684-6762` / `:6842-6897` | P5 (`ObjectRequests::send_section`) |

Sections 9, 12 and 13 sit behind the seams at exactly these positions, so P3 and P5 cannot reorder them. In v24, section 13 runs after `cs_main` is released and section 12's `vGetData` is flushed together with section 13's (`v24:6894-6897`); in v23 both happen inside the `cs_main` block (`v23:6758-6762`). The resulting message order is the same (one `getdata` with blocks first, then objects), so one implementation serves both.

#### 3.1.5 Awaiting observable work

The actor awaits, before taking its next message, the result of every message whose effects a later message could observe (A1 §2.14, M-5). For P2a at M2 that set is:

| Message | What is awaited | Why (Core) |
|---|---|---|
| `headers`, `headers2` | `ChainAccess::process_headers` → `HeadersReply` with fresh `TipFacts` | `ProcessNewBlockHeaders` runs synchronously in `msghand` (`v24:net_processing.cpp:3362`, (=)); a `getheaders` from any peer processed next must see the new headers |
| `version` (outbound) | `AddrMan::set_services`, `AddrMan::good` (synchronous mutex calls) | `v24:3961, 4149` (=) |
| `addr`, `addrv2` | `AddrMan::add` (synchronous) | `v23:4429` / `v24:4474` |

The P3/P5/overlay additions (blocks, transactions, the synchronous overlay messages of A1 §2.14) use the same pattern: the handler calls the blocking seam and the loop does not advance. Nothing in P2a spawns work whose completion a later message could observe.

#### 3.1.6 Timers the actor owns

| Timer | Period | Core |
|---|---|---|
| `CheckForStaleTipAndEvictPeers` | every `EXTRA_PEER_CHECK_INTERVAL` = 45 s | scheduled on the scheduler thread, `v23:net_processing.cpp:2171` / `v24:2150` (=); it takes `cs_main` and touches peer state, so it runs on the actor at the first loop boundary after it is due (one of the interleavings Core allows) |
| `ReattemptInitialBroadcast` | 10–15 min, uniform | `v24:1762-1781`; P5 (unbroadcast txs) |

The loop's wait is shortened to the next due timer, so a timer never fires more than one loop pass late.

### 3.2 Gates and the per-command decode policy (Core `ProcessMessage` prologue and epilogue)

#### 3.2.1 Gate order

Core runs these checks in this order on every message, before any handler reads the payload (`v23:net_processing.cpp:3889-4338` / `v24:3918-4386`; the logic is identical at both pins):

| # | Gate | Outcome | Core (v23 / v24) |
|---|---|---|---|
| 0 | log `"received: %s (%u bytes) peer=%d"` | — | `:3898` / `:3927` |
| 1 | `version` while `nVersion != 0` | log `"redundant version message from peer=%d"`, return **before reading** | `:3906-3910` / `:3935-3939` |
| 2 | any other message while `nVersion == 0` | log `"non-version message before version handshake. Message \"%s\" from peer=%d"`, return, nothing read | `:4147-4151` / `:4178-4182` |
| 3 | `verack` | handled (§3.3.6); a second one logs `"ignoring redundant verack message from peer=%d"` | `:4156-4214` / `:4187-4259` |
| 4 | `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2`, `sendtxrcncl` | handled **before or after** `verack` (§3.3.7) | `:4216-4316` / `:4261-4361` |
| 5 | anything else before `verack` | log `"Unsupported message \"%s\" prior to verack from peer=%d"`, return, no score | `:4318-4321` / `:4363-4366` |
| 6 | first message after `verack` | record `nTimeFirstMessageReceived`, `fFirstMessageIsMNAUTH`; an MN probe whose first message is not `mnauth` is disconnected | `:4323-4334` / `:4368-4379` |
| 7 | `RejectIncomingTxs(peer) && NetMessageViolatesBlocksOnly(cmd)` | log `"%s sent in violation of protocol peer=%d"`, disconnect | `:4337-4341` / `:4382-4386` |
| 8 | the command's own branch (per-command reads, limits and outcome) | §3.2.3, §3.11 | |
| 9 | a command in `getAllNetMessageTypes()` with no branch | the extension dispatch (§3.2.6); silent if nobody handles it | `:5627-5681` / `:5769-5825` |
| 10 | a command not in the list | log `"Unknown command \"%s\" from peer=%d"`, ignore | `:5684` / `:5828` |

`NetMessageViolatesBlocksOnly` is the set `dsa dsc dsf dsq dss dssu dstx dsi getqrinfo platformban qbsigs qcomplaint qcontrib qdata qgetdata qgetsigs qjustify qpcommit qsendrecsigs qsigrec qsigsesann qsigshare qsigsinv qrinfo qwatch tx` (`protocol.cpp:185-212` at both pins, identical). `clsig`, `isdlock`, `spork`, `govobj`, `govobjvote` are not in it. `RejectIncomingTxs` is true for block-relay-only and feeler connections and in `-blocksonly` mode without the `relay` permission (`v23:6127-6135` / `v24:6273-6281`).

In Rust, `dispatch::process_message` is this prologue verbatim, followed by a `match` on the command that calls the handler, then the epilogue (rows 9–10). The gate state (`version`, `successfully_connected`, the first-message fields) lives in `NodeHandle` atomics because ConnMan's inactivity check reads `successfully_connected` too.

#### 3.2.2 Decode at Core's read point

Core has no separate decode step: each branch reads from the stream where its code says `vRecv >> …`, and side effects that precede the read happen even when the read later throws. The Rust handlers therefore call the base-sdk decoder **at Core's read point**, not before the handler. The table in `policy.rs` is the specification and the test oracle for that placement; the dispatcher does not decode generically.

```rust
pub struct CommandPolicy {
    pub cmd: CommandString,
    pub gate: Gate,                  // AnyTimeAfterVersion | BeforeOrAfterVerack | PostVerack (the default)
    pub pre_read: &'static [PreRead],// effects Core performs before its first read (e.g. HeadersClearGetheadersTimestamp)
    pub read: ReadPlan,              // Nothing | Whole(type) | Head{…}Tail{…} (version) | CountThenItems{limit} | Custom
    pub on_throw: ThrowOutcome,      // CaughtByProcessMessages (log, no score) | Handler{score, msg} | Disconnect
    pub trailing: Trailing,          // Ignored for every P2P command (DDE-SWEEP §4)
    pub limits: &'static [Limit],    // e.g. inv/getdata > 50,000 → +20 "inv message size = %u"; checked after the read
    pub blocks_only_violation: bool, // NetMessageViolatesBlocksOnly
    pub quorum_priority: bool,       // IsQuorumPriorityMessage (§3.1.3)
    pub owner: Owner,                // P2a | P2b | P3 | P4 | P5 | P5a | P6a | P6b | SP0 | SP1 | P7 | AM1 | DK1 | …
    pub m2: M2Behaviour,             // §3.11
}
pub static POLICY: [CommandPolicy; 74] = [ /* one row per command, from DDE-SWEEP §4 and §3.11 */ ];
```

A unit test asserts that the table has exactly Core's 74 commands (`allNetMessageTypes`, `protocol.cpp:99-176` at both pins), that `blocks_only_violation` and `quorum_priority` equal Core's two sets, and that every row's `owner` handler is registered once its task has merged.

#### 3.2.3 The M2 command set (P2a's handlers)

| Command | Pre-read effects (in order) | Read | Throw | Limits and outcomes | Core (v23 / v24) |
|---|---|---|---|---|---|
| `version` | gate 1 | head: i32 version, u64 services (`CustomUintFormatter<8>`), i64 time, 8 skipped, `CService` addrMe; then side effects (§3.3.5 steps 3–5); then each optional field only if bytes remain | caught, logged; a throw in the tail happens **after** `SetServices`, the services check and the obsolete-version check, which base-sdk exposes as `P2pDecodeError::VersionTail { head }` | §3.3.5 | `:3906-4145` / `:3935-4176` |
| `verack` | — | nothing | — | §3.3.6 | `:4156` / `:4187` |
| `sendheaders`, `sendheaders2` | — | nothing | — | set `fPreferHeaders` / `fPreferHeadersCompressed` | `:4216-4226` / `:4261-4271` |
| `sendcmpct` | — | `CoreBool` hb, u64 version | caught | version ≠ 1 → ignore; else set provides/HB flags | `:4228-4243` / `:4273-4288` |
| `sendaddrv2` | — | nothing | — | common version < 70223 → ignore; after `verack` → disconnect; else `m_wants_addrv2` | `:4247-4260` / `:4292-4305` |
| `sendtxrcncl` | txreconciliation disabled (the default) → log and return **without reading** | u32, u64 (only when enabled) | caught | the four disconnect cases | `:4265-4316` / `:4310-4361` |
| `senddsq` | — | `CoreBool` | caught | sets `m_wants_dsq` NONE / ALL (< 70234) / INV | `:4440-4452` / `:4485-4497` |
| `qsendrecsigs` | — | `CoreBool` | caught | sets `m_wants_recsigs` | `:4455-4460` / `:4500-4505` |
| `ping` | — | u64 nonce | caught | reply `pong(nonce)` | `:5379-5395` / `:5456-5472` |
| `pong` | — | u64 if ≥ 8 bytes remain, else "Short payload" | never throws | §3.5 | `:5397-5450` / `:5474-5527` |
| `addr`, `addrv2` | — | `vector<CAddress>` (v1 or BIP155) | caught | relay not set up → log `"ignoring %s message from %s peer=%d"`; > 1000 → +20 `"%s message size = %u"`; §3.6 | `:4343-4437` / `:4388-4482` |
| `getaddr` | — | nothing | — | §3.6.4 | `:5312-5347` / `:5389-5424` |
| `inv` | — | `vector<CInv>` | caught | > 50,000 → +20 `"inv message size = %u"`; §3.4.7 | `:4462-4566` / `:4507-4611` |
| `getdata` | — | `vector<CInv>` | caught | > 50,000 → +20 `"getdata message size = %u"`; the rest is P4/P5 (at M2: queued for `ProcessGetData`, which serves nothing; §3.11) | `:4568-4576` / `:4613-4621` |
| `notfound` | — | `vector<CInv>` | caught | > `P2pRules.notfound_limit` → +20 `"notfound message size = %u"` (116 / 50,000; O17 P2P-10); the rest is P5 | `:5598-5620` / `:5744-5765` |
| `headers`, `headers2` | `LoadingBlocks()` → log `"Unexpected headers message received from peer %d"`, return; then **clear `m_last_getheaders_timestamp`** | count (`ReadCompactSize`, range-checked at `MAX_SIZE`); > limit → +20 before any header is read; then headers (+ ignored tx count) or compressed headers with a **per-message** version cache | caught (after the timestamp was cleared) | §3.4.3 | `:5232-5269` / `:5309-5346` |
| `getheaders`, `getheaders2` | — | locator, stop hash | caught | locator > 101 → disconnect; serving is P2b | `:4710-4808` / `:4760-4858` |
| unknown | — | nothing | — | log `"Unknown command"` | `:5684` / `:5828` |

The `headers` row is the case that motivates reading at Core's point: a `headers2` message whose count is valid but whose third compressed header is truncated still clears the peer's last-`getheaders` timestamp, so the next `getheaders` to that peer can go out at once. A decode-first dispatcher would lose that effect.

#### 3.2.4 The exception boundary

Core's `ProcessMessages` catches every exception a handler lets escape and logs `"ProcessMessages(%s, %u bytes): Exception '%s' (%s) caught"` (`v23:5795` / `v24:5941`), with no score and no disconnect. Rust handlers return `Result<(), Throw>` where `Throw { what: Cow<'static, str>, kind: ThrowKind }`. The boundary formats the same line; `what` reproduces Core's text for the decoder failures (`"DataStream::read(): end of data"`, `v23:streams.h:268` / `v24:287`; `"ReadCompactSize(): size too large"` and `"non-canonical ReadCompactSize()"`, `v24:serialize.h:304-319`; the `LIMITED_STRING` `"String length limit exceeded"`, `v24:serialize.h:781`; libstdc++ appends `": iostream error"` to an `ios_base::failure`'s `what()`), mapped from `P2pDecodeError` by one function with a test per variant. Asserted log fragments (A1 §2.18) are checked by O11; the rest is best effort and not a fingerprint (a peer cannot read our log).

Handlers that catch their own exceptions in Core (governance votes +100, malformed `spork` +100, `filterload` pre-allocation) are owned by later tasks; their rows say `on_throw: Handler{…}` and the dispatcher passes the decode error to them instead of the boundary.

#### 3.2.5 `ProtoCtx`

Each read builds `ProtoCtx { version: node.common_version(), bls: tip.bls }` at the read point. The version is Core's `GetCommonVersion()` (`min(peer, Identity.protocol_version)`, set in the `version` handler); before `version` it is `INIT_PROTO_VERSION` 209, which matters only for the `version` message itself. `bls` is `BlsCtx::for_tip` from the `TipFacts` the actor last saw (A1 §1.4.1); none of P2a's commands has a BLS field, so P2a only plumbs it. A peer's `ProtoCtx` is never cached across messages: the common version is fixed after `version`, but the tip's BLS context is not.

#### 3.2.6 The extension dispatch and the `NetHandler` registry

A command in `allNetMessageTypes` that no inline branch of `ProcessMessage` handles goes to the Dash extension dispatch, which runs **after** every generic branch (`v23:5627-5681` / `v24:5769-5825`). Both pins have the `NetHandler` mechanism (`net_processing.h:90-113` / `:122-145`: `Start`, `Stop`, `Interrupt`, `Schedule`, `ProcessMessage`, `AlreadyHave`, `ProcessGetData`; registered with `AddExtraHandler`, iterated in registration order). They differ in which subsystems are handlers:

| Step | v23.1.8 | v24.0.0-rc.3 |
|---|---|---|
| a | CoinJoin wallet manager (if a wallet is loaded) | same |
| b | masternode: sig-share manager, then DKG manager | (moved into handlers) |
| c | observer (`-watchquorums`): DKG manager | (moved into handlers) |
| d | neither: `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit`/`qwatch` → +10 | (now `NetDKGStub`, same +10) |
| e | spork manager (`spork`, `getsporks`) | (inline branches earlier, `v24:5689-5736`) |
| f | MNAUTH | same |
| g | quorum block processor (`qfcommit`) | same (with P2P-12's solicitation callback) |
| h | quorum manager (`qgetdata`, `qdata`) | (now `NetQuorum`) |
| i | platform ban | same |
| j | `clsig`, then **return** (handlers never see it) | same, with P2P-12's gate |
| k | `m_handlers`: `NetInstantSend`, `NetSigning` (`qsigrec`), `CCoinJoinServer` (MN), `NetGovernance` (only when governance is enabled), `SyncManager` (`v23:init.cpp:2213-2262`) | `NetInstantSend`, `NetSigning`, `NetQuorum`, `NetDKG` or `NetDKGStub`, `CCoinJoinServer` (MN), `NetGovernance` (always), `SyncManager` (`v24:init.cpp:2208-2281`) |

A command not in the list is logged `"Unknown command"`; a listed command that nobody handles (e.g. `merkleblock`, `cfilter`, `dsc` without a CoinJoin client, sig-share messages on a non-masternode) is dropped **silently**. Each command is consumed by exactly one subsystem at either pin (`dsq` has two owners, which are mutually exclusive by role), so the step order is not observable; only who handles what, and the `MessageProcessingResult` application order (misbehaviour → erase the object request → relay transactions → relay invs; `v23:3798-3812` / `v24:3819-3837`), are.

The Rust registry is a map `CommandString → handler` filled by ND1 in each release's registration order (a double registration is a startup panic), plus P2a's per-release fallbacks for the M2 node: the +10 for the five DKG commands under both profiles (v23's inline rule, v24's stub), and silence for every other unowned listed command. The trait (§2.4) mirrors Core's `NetHandler`; A1's `on_send_tick`, `on_connected` and `on_disconnected` do not exist at either pin and are dropped (§3.15): overlay traffic comes from the overlays' own scheduler tasks (the sync tick, governance relay flush, sig-share loops), which push through `NodeTable::push_message` and `PeerManagerHandle` from their own threads, as Core's do. §2.6 lists every hook P2a exposes for them.

### 3.3 Handshake and `fingerprint.rs`

#### 3.3.1 Identity per profile

| Field | `V23_1` (mainnet; regtest with `-releaseprofile=23.1`) | `V24_0` (testnet, devnets, regtest) | Core |
|---|---|---|---|
| `version.nVersion` / `Identity.protocol_version` | 70240 | 70242 | `version.h:14` both (O17 P2P-01) |
| user agent | `/Dash Core:23.1.8/` | `/Dash Core:24.0.0/` | `FormatSubVersion(CLIENT_NAME, CLIENT_VERSION, comments)`, `clientversion.cpp:69-84` (=); `CLIENT_NAME` "Dash Core" |
| `client_version` | 230108 | 240000 | `FormatVersion(v) = "%d.%d.%d" % (v/10000, v/100%100, v%100)`, `clientversion.cpp:55-58` (=) |
| `min_mn_proto_version` | 70240 | 70242 | `version.h:23` (O17 P2P-02; read by DK1, carried here) |
| `INIT_PROTO_VERSION` | 209 | 209 | `version.h:17` (=) |
| `MIN_PEER_PROTO_VERSION` | 70221 | 70221 | `version.h:20` (=) |

`ua_version` is derived from `client_version` by Core's `FormatVersion`; a unit test asserts the two fields agree, so they cannot drift. Every send-side version gate (`sendaddrv2` ≥ 70223, the `headers2` 8000 limit ≥ 70235, short IDs ≥ 70235 / 70240, `dsa` flags ≥ 70241, `MSG_ASSET_UNLOCK` announcements ≥ 70242, DSQ as inv ≥ 70234) compares against the **common** version `min(peer, Identity.protocol_version)` and so follows the profile with no extra field (O17 P2P-03).

#### 3.3.2 User agent

`LocalIdentity::new` reproduces `init.cpp` (`v23:1790-1807` / `v24:1827-1844`, (=)):
1. comments start with `devnet.<name>` on a devnet;
2. each `-uacomment` must equal its `SanitizeString(…, SAFE_CHARS_UA_COMMENT)` (alphanumerics and ` .,;-_?@`), else InitError `"User Agent comment (%s) contains unsafe characters."`;
3. `"/Dash Core:" + FormatVersion(client_version) + ("(" + join(comments, "; ") + ")")? + "/"`;
4. longer than 256 (`MAX_SUBVERSION_LENGTH`, `net.h:88/86`) → InitError `"Total length of network version string (%i) exceeds maximum length (%i). Reduce the number or size of uacomments."`.

`-honestua` (DEC-04) replaces `Dash Core:<ua_version>` with `rdashd:<crate version>` and keeps the comments; it is the only switch that changes the wire identity, it is off by default, and it is logged at startup. The true identity is always in `-version`, the `debug.log` banner and `getimplementationinfo` (A1 §2.14).

#### 3.3.3 Local services

| Bit | Value | When | Core (v23 / v24 `init.cpp`) |
|---|---|---|---|
| `NODE_NETWORK_LIMITED` \| `NODE_HEADERS_COMPRESSED` | 0x0C00 | always (initial value) | `:1077` / `:1125` |
| `NODE_P2P_V2` | 0x1000 | `-v2transport` (default 1, `net.h:122/142`) | `:1187-1189` / `:1235-1237` |
| `NODE_COMPACT_FILTERS` | 0x40 | `-peerblockfilters` (default 0; soft-set to 1 on masternodes) with the basic filter index, else InitError `"Cannot set -peerblockfilters without -blockfilterindex."` | `:1192-1198` / `:1240-1246` |
| `NODE_BLOOM` | 0x04 | `-peerbloomfilters` (default 1, `net_processing.h:46/44`) | `:1386-1387` / `:1370-1371` |
| `NODE_NETWORK` | 0x01 | not pruned, set after the block index loads ("Setting NODE_NETWORK on non-prune mode") | `:2313-2314` / `:2353-2354` |

Defaults: 0x1C05 for a node, 0x1C45 for a masternode, 0x1C04 pruned, 0x0C05 with `-v2transport=0`. The O17 P2P-35 difference (v23 lets an explicit `-peerblockfilters=0` win on a masternode, so 0x1C05; v24 refuses to start) is `InitRules.mn_init`, read by ND1/AM1; P2a only consumes the resulting set. An outbound connection offers exactly these services (`InitializeNode(*pnode, nLocalServices)`, `v24:net.cpp:3648`); inbound connections may add `NODE_BLOOM` through the `bloomfilter` permission (`v24:net.cpp:1998`, P2b/P4).

`GetDesirableServiceFlags(s)` is `NODE_NETWORK_LIMITED` if `s` has it and the node has left IBD, else `NODE_NETWORK` (`protocol.cpp:252-257` (=)). The IBD half is a process-wide cache that `UpdatedBlockTip` sets (`SetServiceFlagsIBDCache(!fInitialDownload)`, `v24:net_processing.cpp:2276`); P2a keeps it in an `AtomicBool` that ConnMan and the `version` handler both read.

#### 3.3.4 Outbound: our `version` (Core `InitializeNode` → `PushNodeVersion`)

Core: `InitializeNode` `v23:1779-1793` / `v24:1745-1760`; `PushNodeVersion` `v23:1538-1572` / `v24:1561-1593` (=). As soon as ConnMan hands the actor a new outbound node, before any byte arrives, the actor pushes:

| Field | Bytes | Value |
|---|---|---|
| `nVersion` | 4 | `Identity.protocol_version`, or `-pushversion` on a non-mainnet network (the advertised field only; the common version still uses the profile value, `v24:4046`) |
| services | 8 | `m_our_services` (§3.3.3) |
| time | 8 | `GetTime()` seconds (mockable, **not** adjusted time, at both pins) |
| addr_recv | 26 | services = `addr.nServices` (what addrman recorded for the address, or `NODE_NETWORK` for seeds, `NODE_NONE` for names, §3.8); address = the peer's address if `IsRoutable() && !IsProxy() && IsAddrV1Compatible()`, else `[::]:0` |
| addr_from | 26 | services = `m_our_services`; address `[::]:0` |
| nonce | 8 | `nLocalHostNonce` = SipHash with the process key over `RANDOMIZER_ID_LOCALHOSTNONCE` ‖ node id (`net.cpp:124, 491` v23 / `:125, 492` v24) |
| user agent | 1+18 | §3.3.2 |
| start_height | 4 | `m_best_height`: set from the active height at init (`SetBestHeight`, `v23:init.cpp:2534` / `v24:2601`), then by every `UpdatedBlockTip` |
| relay | 1 | `!RejectIncomingTxs(node)`: 0 for block-relay-only and feeler connections and in `-blocksonly` |
| mnauth challenge | 32 | fresh `GetRandBytes` per connection, stored as the sent challenge |
| fMasternode | 1 | `node.m_masternode_connection` (set only on MN connections, P7) |

Framing uses `INIT_PROTO_VERSION` 209. The default fixture is 137 bytes (4+8+8+26+26+8+19+4+1+32+1); comments, `-honestua` and devnet names change it (A1 §2.14, F14). The log line is `"send version message: version %d, blocks=%d, txrelay=%d, peer=%d"` (with `them=%s` under `-logips`).

#### 3.3.5 Processing the peer's `version` (outbound and inbound branches)

The handler follows `v23:3906-4145` / `v24:3935-4176` step by step (both pins identical except that v24 also records `m_version_services`/`m_version_relay_txs`, `v24:4060-4061`, which are internal; §2.5 LQ4a row):

1. `nVersion != 0` → "redundant version message", return (gate 1).
2. Read `nVersion`, services, time (negative → 0), skip 8 bytes, read addrMe. A throw here leaves the peer pre-version, logged.
3. Outbound only: `addrman.set_services(node.addr, services)` (overwrite).
4. `ExpectServicesFromConn()` (full-relay, block-relay, addr-fetch) and `!HasAllDesirableServiceFlags(services)` → log `"peer=%d does not offer the expected services (%08x offered, %08x expected); disconnecting"`, disconnect.
5. `nVersion < 70221` → log `"peer=%d using obsolete version %i; disconnecting"`, disconnect (no reject message exists).
6. Optional fields, each only if bytes remain: skip 26 + nonce; subver (`LIMITED_STRING` 256, then `SanitizeString`); start height; relay (`CoreBool`); mnauth challenge (stored as received); `fMasternode` (`CoreBool`). Inbound with `fMasternode` sets `m_masternode_connection`; if we are not a masternode → log and disconnect (`v24:4003-4013`). A throw in any optional field ends the handler here; steps 3–5 have already happened (base-sdk `VersionTail`).
7. Inbound with a nonce equal to one of our outbound nonces → `"connected to self at %s, disconnecting"`. Inbound with a routable addrMe → `SeenLocal(addrMe)`.
8. Inbound: push our `version` now ("be shy", §3.3.4).
9. Devnet: subver must contain `devnet.<name>`, else log `"connected to wrong devnet. Reported version is %s, expected devnet name is %s"`, +100 outbound / +1 inbound (message `"wrong-devnet-name"` at v24, none at v23: log text only), disconnect.
10. `common = min(nVersion, Identity.protocol_version)`; store common version and `nVersion` (the transport reads the former, §2.5).
11. Push `sendaddrv2` if `common ≥ 70223`.
12. Record services, relay, addrMe (`SetAddrLocal`), subver, starting height. Create the tx-relay state unless block-relay-only or feeler, when `relay || our NODE_BLOOM` (P5 fills it; P2a creates the slot).
13. `sendtxrcncl` only if txreconciliation is enabled (off by default; FR-P2P-17 SHOULD) and the peer qualifies (`v24:4087-4101`).
14. Push `verack`.
15. `fPreferredDownload = (outbound || noban) && !addr-fetch && CanServeBlocks(peer)` and update the global count (`v24:4106-4111`).
16. Outbound: `SetupAddressRelay` (false only for block-relay-only); if true push `getaddr`, set `m_getaddr_sent`, add 1000 to the addr token bucket (`v24:4116-4132`). This includes feelers, manual and addr-fetch connections; A1's "getaddr on outbound full-relay only" is too narrow (§3.15).
17. Outbound: `addrman.good(node.addr)` (`v24:4149`).
18. Log `"receive version message: %s: version %d, blocks=%d, us=%s, txrelay=%d, peer=%d%s%s"`.
19. `nTimeOffset = nTime − GetTime()`, stored on the node (`getpeerinfo.timeoffset`); outbound only: `AddTimeData(node.addr, nTimeOffset)` (§3.3.8).
20. Feeler → log `"feeler connection completed peer=%d; disconnecting"`, disconnect.

#### 3.3.6 `verack`

Core: `v23:4156-4214` / `v24:4187-4259` (=, plus a v24 `Assume` that is not behaviour).

1. Already connected → `"ignoring redundant verack message from peer=%d"`.
2. Log `"New %s %s peer connected: version: %d, blocks=%d, peer=%d%s%s"` (unconditionally for outbound).
3. Masternode and not a probe: push `mnauth` (`CMNAuth::PushMNAUTH`; the slot is P2a's, the payload is AM1/P7's).
4. Push `sendheaders2` if the peer's version services have bit 11 (`NODE_HEADERS_COMPRESSED`), else `sendheaders` (`UsesCompressedHeaders`, `v23:1180` / `v24:1196`).
5. `CanRelay()` (`!m_masternode_connection || m_masternode_iqr_connection`, `net.h:1083` v24) → push `sendcmpct(false, 1)`.
6. `!RejectIncomingTxs` → push `senddsq(true)`; then, still inside that branch, if `qman.IsWatching()` and the peer is a masternode quorum node, push `qwatch` (`v23:4193-4201` / `v24:4224-4232`).
7. Txreconciliation bookkeeping (disabled by default).
8. `fSuccessfullyConnected = true`.

The next `send_messages` for the node, in the same pass, then sends `ping` (the first `MaybeSendPing` always fires because `m_ping_start` starts at 0, `v24:6169`), an `addr` if there is anything to send (§3.6.6), and the initial `getheaders2`/`getheaders` if the sync conditions hold (§3.4.2). So the complete outbound transcript of a non-masternode with default options, after the peer's `version` and `verack`, is:

```
→ version                                   (at connect)
                     ← version [← sendaddrv2] ← verack
→ sendaddrv2 (common ≥ 70223)  → verack  → getaddr (unless block-relay-only)
→ sendheaders2 | sendheaders  → sendcmpct(0,1) (unless a non-IQR MN connection)
→ senddsq(1) (unless block-relay-only/feeler/-blocksonly)  [→ qwatch (watching, quorum peer)]
→ ping  [→ addr|addrv2]  [→ getheaders2 | getheaders]
```

A1 §2.14 and §4.10 behaviour 5 put `qwatch` after `ping` and `getheaders`; at both pins it is sent in the `verack` handler right after `senddsq`, before them (§3.15). Feelers send `version`, then `sendaddrv2`, `verack`, `getaddr` and disconnect (the 100 ms linger flushes them, §3.8.7).

#### 3.3.7 The pre/post-`verack` control messages

| Message | Before `verack` | After `verack` | Core (v23 / v24) |
|---|---|---|---|
| `sendheaders` / `sendheaders2` | sets the preference | sets the preference | `:4216-4226` / `:4261-4271` |
| `sendcmpct` | read; version ≠ 1 ignored; sets `m_provides_cmpctblocks`, `m_requested_hb_cmpctblocks`, `m_bip152_highbandwidth_from` | same | `:4228-4243` / `:4273-4288` |
| `sendaddrv2` | common < 70223 → ignore; else `m_wants_addrv2` | log `"sendaddrv2 received after verack from peer=%d; disconnecting"`, disconnect (common ≥ 70223) | `:4247-4260` / `:4292-4305` |
| `sendtxrcncl` | txreconciliation off → debug log `"sendtxrcncl from peer=%d ignored, as our node does not have txreconciliation enabled"`, nothing read | same as before `verack` when off | `:4265-4316` / `:4310-4361` |

#### 3.3.8 Time offsets

`AddTimeData` (`timedata.cpp:46-111` (=); the files differ only in `GetAdjustedTime`'s return type): ignore a source already counted (keyed by `CNetAddr`, port ignored) or once 200 sources exist; push into a 200-sample median filter whose initial sample is 0; when the filter holds ≥ 5 samples and an odd count, set the offset to the median if `|median| ≤ -maxtimeadjustment` (default 4200 s), else to 0 and, once, raise the clock warning if no sample is non-zero and within ±5 min. The 200-sample quirk (the offset can never change again once the filter is full, because 200 is even) is reproduced (§3.14 PQ-6). Both pins feed it the same way; under `V23_1` its output is the consensus clock (`UngatedRules.time_source = AdjustedTime`, O17 CONS-04, P2P-16), under `V24_0` only `getnetworkinfo.timeoffset` and the warning read it.

### 3.4 Headers and headers2 sync

#### 3.4.1 State

| Per peer (`PeerState`) | Core field | Global (`PeerLogic`) | Core field |
|---|---|---|---|
| `sync_started` | `CNodeState::fSyncStarted` | `n_sync_started` | `nSyncStarted` |
| `headers_sync_timeout` (µs; `MAX` = disarmed) | `m_headers_sync_timeout` | `n_preferred_download` | `m_num_preferred_download_peers` |
| `last_getheaders` (NodeClock) | `Peer::m_last_getheaders_timestamp` | `last_block_inv_triggering_sync` | `m_last_block_inv_triggering_headers_sync` |
| `unconnecting_headers` | `nUnconnectingHeaders` | `outbound_protected` | `m_outbound_peers_with_protect_from_disconnect` |
| `best_known_block: Option<BlockId>` | `pindexBestKnownBlock` | `best_height` | `m_best_height` |
| `last_unknown_block: Option<BlockHash>` | `hashLastUnknownBlock` | `initial_sync_finished` | `m_initial_sync_finished` |
| `best_header_sent: Option<BlockId>` | `pindexBestHeaderSent` | `ibd_service_cache` | `g_initial_block_download_completed` |
| `inv_triggered_getheaders_before_sync` | `Peer::…` | | |
| `last_block_announcement` (s) | `m_last_block_announcement` | | |
| `prefer_headers`, `prefer_headers_compressed` | `fPreferHeaders`, `fPreferHeadersCompressed` | | |
| `chain_sync: ChainSyncTimeoutState` | `m_chain_sync` | | |
| `blocks_for_headers_relay`, `blocks_for_inv_relay` | `Peer::m_blocks_for_*` | | |

`best_known_block` and `best_header_sent` are `BlockId`s into CH1's arena, which never moves or frees entries (A1 §2.10), so the actor holds them like Core holds `CBlockIndex*`.

#### 3.4.2 Starting sync (SendMessages section 6)

Core: `v23:6197-6250` / `v24:6342-6396`.

```
if best_header is unset: best_header = tip                                    // m_chainman.m_best_header
sync_from_peer = preferred_download
              || (CanServeBlocks(peer) && !addr_fetch && (n_preferred_download == 0 || no block in flight anywhere))
if !sync_started && CanServeBlocks(peer) && !loading_blocks && node.CanRelay():
    if (n_sync_started == 0 && sync_from_peer) || best_header.time > now_sync − max_tip_age:
        start = best_header.prev or best_header
        if maybe_send_getheaders(peer, getheaders2 if peer bit 11 else getheaders, locator(start)):
            log "initial %s (%d) to peer=%d (startheight:%d)"
            sync_started = true; n_sync_started += 1
            headers_sync_timeout = now_us + 15 min + 1 ms × ((now_sync − best_header.time) / pow_target_spacing)
```

`CanServeBlocks(peer)` is `their_services & (NODE_NETWORK | NODE_NETWORK_LIMITED)` (`v24:1190-1193` (=)); `max_tip_age` is `-maxtipage`, default 6 h at both pins. **Profile difference** (O17 P2P-16, `UngatedRules.time_source`): `now_sync` is `GetAdjustedTime()` under `V23_1` (`v23:6225, 6246`) and `NodeClock::now()` under `V24_0` (`v24:6370, 6391`). `now_us` is the mockable `GetTime()` at both pins. The actor reads `time_source` once at construction and holds the matching closure; O17 lists P2b and P3 as the readers of P2P-16, but the sync-start rule and the headers timeout are this section, so P2a is a reader too (§3.13).

#### 3.4.3 Processing `headers`/`headers2` (Core `ProcessHeadersMessage`)

Core: the message branch `v23:5232-5269` / `v24:5309-5346`; `ProcessHeadersMessage` `v23:3338-3402` / `v24:3322-3386` (=).

1. `loading_blocks` → log, return (nothing read; the timestamp is kept).
2. `last_getheaders = 0` (any `headers`/`headers2`, even an empty or malformed one, answers our request).
3. Read the count (`ReadCompactSize`, which throws above `MAX_SIZE`); count > `GetHeadersLimit` → +20 `"headers message size = %u"`, return. `GetHeadersLimit` is 8000 for `headers2` when the common version is ≥ 70235, else 2000 (`v23:1194-1200` / `v24:1210-1216`); note it is chosen by **the message type received**, while the reply type we request is chosen by the peer's service bit.
4. Read the headers. `headers`: 80 bytes and a `ReadCompactSize` tx count (ignored) each. `headers2`: compressed headers, decompressed against the headers of **this message only** with a fresh version list (`last_unique_versions` is local to the branch, `v24:5336`), so the compression state never carries across messages (§3.4.9).
5. Empty → return ("stop asking this peer").
6. `headers[0].prev` not in the index: if count ≤ `MAX_BLOCKS_TO_ANNOUNCE` (8) → `HandleFewUnconnectingHeaders` (`unconnecting_headers += 1`; `maybe_send_getheaders(locator(best_header))`; `UpdateBlockAvailability(last hash)`; every 10th → +20 `"%d non-connecting headers"`), else +10 `"invalid header received"`; return.
7. Not continuous (`header.prev != hash(previous header)`) → +20 `"non-continuous headers sequence"`, return.
8. `received_new_header = lookup(last hash).is_none()`.
9. `process_headers(batch)` (awaited, §3.1.5). An invalid result → `MaybePunishNodeForBlock(state, via_compact_block=false, "invalid header received")` (§3.10.3), return. Headers accepted before the failing one stay accepted (Core's loop, `v24:validation.cpp:4733-4745`).
10. Count == limit (for the received type) → `maybe_send_getheaders(locator(last accepted))`, log `"more %s (%d) to end to peer=%d (startheight:%d)"`.
11. `UpdatePeerStateForReceivedHeaders` (`v23:3283-3336` / `v24:3267-3320`): reset `unconnecting_headers` (logging the reset); `UpdateBlockAvailability(last)`; a new header with more work than the tip sets `last_block_announcement = GetTime()`; in IBD with a short message (count < limit) and the peer's best known chain below minimum chain work → outbound-or-block-relay peer: `"Disconnecting outbound peer %d -- headers chain has insufficient work"`; an outbound full-relay peer whose best known work ≥ the tip's, while fewer than 4 are protected, becomes protected (`"Protecting outbound peer=%d from eviction"`).
12. `BlockSync::headers_direct_fetch(last)` (P3; a no-op at M2).

`via_compact_block` is false here; P3 calls step 9's punish path with true from the compact-block path.

#### 3.4.4 The `getheaders` policy

`MaybeSendGetHeaders` (`v23:3195-3211` / `v24:3179-3195` (=); `NodeClock::now()` at both pins): a `getheaders`/`getheaders2` goes out only if `NodeClock::now() − last_getheaders > HEADERS_RESPONSE_TIME` (2 min); it is sent with `hashStop = 0` and stamps `last_getheaders`. Every receipt of `headers`/`headers2` clears the stamp (§3.4.3 step 2). The callers are the initial sync (§3.4.2), "more headers" (step 10), unconnecting headers (step 6), the block-inv trigger (§3.4.7) and `ConsiderEviction` (§3.9.1). The message type is always chosen by the peer's bit 11 (`UsesCompressedHeaders`), never by the common version: a peer with bit 11 and a common version below 70235 gets `getheaders2` and answers with up to 2000 compressed headers.

The locator is Core's `GetLocator(pindex)`: the 10 most recent hashes, then exponentially spaced back to genesis, genesis always last (`chain.cpp`; O17 notes the v24 free function produces the identical locator). CH1 implements it over the arena (`ChainAccess::locator`).

#### 3.4.5 Headers sync timeout (SendMessages section 10)

Core: `v23:6624-6656` / `v24:6782-6813` (the clock comparison at `v23:6627` / `v24:6785`). If `sync_started` and the timeout is armed: when `best_header.time ≤ now_sync − max_tip_age` (still catching up), `now_us > headers_sync_timeout`, `n_sync_started == 1` and at least one **other** preferred-download peer exists: a non-noban peer is disconnected (`"Timeout downloading headers from peer=%d%s, disconnecting"`); a noban peer is logged (`"…from noban peer=%d%s, not disconnecting"`) and its sync state reset (`sync_started = false`, `n_sync_started -= 1`, timeout 0). Once caught up, the timeout is disarmed for good. `now_sync` follows `time_source` as in §3.4.2.

#### 3.4.6 What the actor needs from CH1, and when

- `process_headers` replies only after the headers are in the arena **and** `TipFacts` (best header, its time and work, tip work, IBD) is published. The actor reads `TipFacts` after the reply, so the next message (any peer) sees the new best header, as `msghand` does under `cs_main`.
- `TipFacts` is a small `ArcSwap` published on every best-header change and every tip change, **without** A1's 100 ms IBD throttle (which stays for the full `ChainView`, whose `MnList` and active-chain vector are expensive). Throttling it would make a `getheaders` locator lag the headers just accepted, which dashd never does.
- `lookup`, `entry`, `ancestor`, `active_at` and `locator` are lock-free reads of the arena and the active-chain vector. Core takes `cs_main` for each, without atomicity across them (e.g. `LookupBlockIndex` at `v24:3337` and `:3359` are separate critical sections), so lock-free reads give the same guarantees.

#### 3.4.7 The block path of `inv`

Core: `v23:4462-4566` / `v24:4507-4611`. After the size check, for each entry:
- **unknown type** → `"got inv of unknown type %d: %s peer=%d"`, skip. Whether type 34 is known is `P2pRules.msg_asset_unlock` (O17 P2P-04): under `V23_1` an `inv` of type 34 is skipped like any unknown type and is never a blocks-only violation; under `V24_0` it is a known `tx`-class type, so on a block-relay-only or feeler connection it disconnects (`"%s (%s) inv sent in violation of protocol, disconnecting peer=%d"`). The same field decides `IsKnownType` for `getdata` and `notfound` (P4/P5 read it there too);
- **block**: `UpdateBlockAvailability`; not already had, not loading, not requested → remember it as `best_block`;
- **anything else**: on a tx-rejecting connection, an inv whose command is in the blocks-only set disconnects (as above); then `AddKnownInv`; if not already had: an `MSG_ISDLOCK` inv on a tx-rejecting connection is skipped silently when the common version is ≤ 70223 and disconnects otherwise; else, outside IBD (or for `MSG_SPORK` even in IBD), `ObjectRequests::add_announcement` (P5; a no-op at M2, where the node is always in IBD). `AlreadyHave` for the overlay types goes through the `NetHandler` registry (§2.6).

After the loop, if `best_block` is set and (`sync_started` or (`!inv_triggered_getheaders_before_sync` and `best_block != last_block_inv_triggering_sync`)): `maybe_send_getheaders(locator(best_header))` (logged `"%s (%d) %s to peer=%d"`); if not yet syncing, mark the peer and remember the hash, so at most one extra peer starts syncing per new block.

#### 3.4.8 Block announcements (SendMessages section 7)

Core: `v23:6254-6406` / `v24:6399-6551` (=), plus `UpdatedBlockTip` (`v23:2280-2315` / `v24:2273-2308`), which (outside IBD) queues the up-to-8 new tip hashes, oldest first, on every `CanRelay()` peer and wakes the loop.

For a `CanRelay()` peer: revert to `inv` unless the peer asked for `headers` or `headers2` (or asked for HB compact blocks and only one block is queued), or more than 8 blocks are queued; `ProcessBlockAvailability`; walk the queue to find the first header the peer lacks whose parent it has (the devnet genesis counts as had), bailing out to `inv` on a reorg or a gap; then send, in priority order: one `cmpctblock` if exactly one header and the peer asked for HB (P3 supplies the encoder: the cached most-recent compact block or one built from disk); else `headers2` (compressed, **announcement mode**, §3.4.9) if the peer sent `sendheaders2`; else `headers` if it sent `sendheaders`; else `inv`. `inv` fallback announces only the last queued hash, and only if the peer lacks it. `best_header_sent` is updated on every headers-type send.

At M2 the node never connects blocks, so this section is idle; P2a implements and tests it in `sim` with a fake chain, and it becomes live at M3 with no change.

#### 3.4.9 The `headers2` codec contract (BS7)

The codec is BS7's (`dash-p2p-core::headers2`, `diff_headers2`); P2a depends on these properties, all from `primitives/block.cpp:57-134` (identical at both pins):

1. **Per-message state.** Decoding and encoding start every message with an empty version list.
2. **Two encoder modes.** The `getheaders2` reply leaves the first header uncompressed **and does not record its version** (`if (!v_headers.empty()) Compress(…)`, `v23:net_processing.cpp:4804` / `v24:4854`). The announcement path calls `Compress` on the first header too, which records its version (`Compress` with no previous header calls `SaveVersionAsMostRecent`, `block.cpp:59-63`). The same header list therefore encodes to different bytes in the two paths. BS7 must expose both (`encode_reply`, `encode_announce`); P2b uses the first, P2a's announcements the second (§3.14 PQ-3).
3. **Decoder.** The first header applies no compression flag; a later header with a compressed previous hash takes `X11(previous decompressed header)`; a version offset beyond the list leaves version 0 without an error; the version list is a 7-entry MRU that can hold duplicates (DDE-SWEEP §5, BS7 items).
4. **Hash output.** The decoder returns each header's X11 hash, since it computes the previous one anyway (§5.2).

#### 3.4.10 Why P2a owns header-sync performance

Core hashes every header 2× (`CheckHeadersAreContinuous` and `AcceptBlockHeader`'s `GetHash`) and every `headers2` header 3× (plus `Uncompress`'s previous-hash recomputation), all on `msghand`, and X11 costs ~30 µs per 80-byte header (B1 `Pow_X11_0080b` 383 ns/byte). §5 turns that into the 2× route.

### 3.5 Ping, pong and the inactivity timeouts

Core: `MaybeSendPing` `v23:6002-6038` / `v24:6148-6184`; `ping`/`pong` `v23:5379-5450` / `v24:5456-5527`; both identical between the pins. `PING_INTERVAL` 2 min (`v23:134` / `v24:148`), `TIMEOUT_INTERVAL` 20 min (`net.h:74/72`).

`MaybeSendPing(node, now)`, run in section 4 of every `send_messages` pass after `verack`:
1. If inactivity checks apply (`now > m_connected + -peertimeout`, §3.8.7), a ping is outstanding and `now > ping_start + 20 min` → log `"ping timeout: %fs peer=%d"`, disconnect, return.
2. Send if the RPC queued one (`ping_queued`), or none is outstanding and `now > ping_start + 2 min`. `ping_start` starts at 0, so the first pass after `verack` always pings.
3. Nonce: `GetRand<uint64_t>()` redrawn until non-zero; clear `ping_queued`, set `ping_start = now`, `ping_nonce_sent = nonce`, push `ping(nonce)`.

All times are the mockable `GetTime<µs>()`; the 2-minute cadence is therefore "2 min after the previous ping was **sent**, checked on every pass", whose distribution the KS test of NFR-FP-03 compares (the pass cadence is ≤ 100 ms plus processing).

`ping` handler: read the nonce (a short payload throws and is logged), reply `pong(nonce)` at once. `pong` handler (never throws):
- fewer than 8 bytes → finished, problem "Short payload";
- no ping outstanding → problem "Unsolicited pong without ping" (not finished);
- nonce matches → finished; `ping_time = msg.recv_time − ping_start`, which must be ≥ 0 (else "Timing mishap") and updates the node's last and minimum ping times (the minimum feeds P4's inbound eviction);
- nonce mismatch → "Nonce mismatch", and if the received nonce is 0 → finished, "Nonce zero";
- any problem → log `"pong peer=%d: %s, %x expected, %x received, %u bytes"`; finished → `ping_nonce_sent = 0`.

`recv_time` is the time stamped by P1's reader for the socket read that completed the message (§2.5), as Core's `CNetMessage::m_time` (`v24:net.cpp:652`). Measuring at the actor instead would add the queueing delay to `getpeerinfo`'s `pingtime`/`minping` and to the eviction ranking.

The socket-level inactivity rules (`InactivityCheck`: no message in the first `-peertimeout` seconds, 20 min without send or receive, handshake timeout) are ConnMan's (§3.8.7); P2a's actor implements only the ping timeout above.

### 3.6 Address relay

Core: `SetupAddressRelay` `v23:6137-6152` / `v24:6283-6298`; the `addr`/`addrv2` branch `v23:4343-4437` / `v24:4388-4482`; `getaddr` `v23:5312-5347` / `v24:5389-5424`; `RelayAddress` `v23:2665-2713` / `v24:2686-2734`; `PushAddress` `v23:1149-1162` / `v24:1165-1178`; `MaybeSendAddr` `v23:6040-6106` / `v24:6186-6252`; the cache `CConnman::GetAddresses(CNode&,…)` `v23:net.cpp:4252-4294` / `v24:4265-4307`. All identical between the pins (verified by diffing each body).

#### 3.6.1 Per-peer state

`addr_relay_enabled` (false), `addr_known: Option<RollingBloom>` (created with `(5000, 0.001)` when relay is first enabled), `addrs_to_send` (≤ 1000), `getaddr_sent`, `getaddr_recvd`, `wants_addrv2`, `next_addr_send` and `next_local_addr_send` (both 0), `addr_token_bucket: f64 = 1.0`, `addr_token_timestamp` = mockable time **when the peer state is created** (in `initialize_node`), and the two counters `addr_processed`, `addr_rate_limited` (reported by `getpeerinfo`).

#### 3.6.2 Who relays

`SetupAddressRelay(node)` returns false for block-relay-only connections and otherwise enables relay once (creating `addr_known`). It is called for every outbound connection in the `version` handler (§3.3.5 step 16), and for inbound peers on their first `addr`, `addrv2` or `getaddr`. An inbound peer that never sends one of those never receives addresses from us, not even our self-advertisement.

#### 3.6.3 Receiving `addr`/`addrv2`

In Core's order:
1. Read the vector (BIP155 for `addrv2`); a decode error is the generic throw (no score).
2. `!SetupAddressRelay` → log `"ignoring %s message from %s peer=%d"`, return. So an oversized `addr` from a block-relay-only peer is ignored without a score, while an inbound peer's oversized one scores **and** enables relay.
3. More than 1000 entries → +20 `"%s message size = %u"`, return.
4. Refill the token bucket if below 1000: `+ max(now − timestamp, 0) × 0.1`, capped at 1000; always update the timestamp.
5. `rate_limited = !permission(Addr)`; shuffle the vector with a fresh RNG.
6. For each address: bucket < 1 → if rate-limited count and skip, else continue without decrement; else take a token (**before** any filter); skip if neither `MayHaveUsefulAddressDB` nor `HasAllDesirableServiceFlags`; fix `nTime` ≤ 100,000,000 or > now + 10 min to now − 5 d; `AddAddressKnown`; skip (after remembering) banned or discouraged addresses; count as processed; relay with `RelayAddress` if `nTime > now − 10 min`, `!getaddr_sent`, the message had ≤ 10 entries and the address is routable; keep it if its network is reachable.
7. Log `"Received addr: %u addresses (%u processed, %u rate-limited) from peer=%d"`; `addrman.add(kept, source = peer.addr, penalty = 2 h)` (§3.7.2; the penalty is waived when the address equals the source IP, port ignored).
8. Fewer than 1000 entries received → `getaddr_sent = false`.
9. ADDR_FETCH connection and more than one entry received → `"addrfetch connection completed peer=%d; disconnecting"`.

#### 3.6.4 `getaddr`

Outbound → log `"Ignoring \"getaddr\" from %s connection. peer=%d"` (so a probe cannot read our addrman through an outbound link). Inbound: enable relay; a second `getaddr` → `"Ignoring repeated \"getaddr\". peer=%d"`; otherwise clear `addrs_to_send` and push every address of `GetAddresses(1000, 23%)`, uncached for peers with the `addr` permission and otherwise from the **address-response cache**, keyed by SipHash over (`RANDOMIZER_ID_ADDRCACHE` 0x1cf2e4ddd306dda9, the network the requestor connected through, our local bind address bytes, our local bind port for inbound) and refreshed after 21 h + uniform [0, 6 h). Banned and discouraged entries are removed from each fresh result. The reply leaves on the **next** `send_messages` pass, because this peer's `next_addr_send` is still 0. P2a implements the handler and the cache; inbound `getaddr` arrives only once P2b accepts connections, and P2b owns `p2p_getaddr_caching`.

#### 3.6.5 `RelayAddress`

```
if !reachable && !addr.IsRelayable(): return                       // IPv4, IPv6, Tor, I2P, CJDNS
h  = CServiceHash(0,0)(addr)                                       // SipHash(0,0) over u64 net, u64 port, address bytes
t  = (u64(now_s) wrapping_add h) / 86400                           // 24 h rotation
hs = SipHash(seed0, seed1).write(0x3cac0035b5866b90).write(h).write(t)
n  = (reachable || hs.finalize() & 1) ? 2 : 1
best = top-n of { SipHash(hs).write(u64 id).finalize() : peer in peers by id, relay enabled, id ≠ origin,
                  wants_addrv2 || addr.IsAddrV1Compatible() }       // strict ">"; includes peers mid-handshake
for each of best with a non-zero key: PushAddress(peer, addr)
```

`seed0`/`seed1` are ConnMan's per-process SipHash keys (the same ones the nonce and netgroup keys use). `PushAddress` drops invalid addresses, addresses in the peer's `addr_known` and non-v1 addresses for peers that did not send `sendaddrv2`; when `addrs_to_send` is full it overwrites a random entry.

#### 3.6.6 `MaybeSendAddr` (SendMessages section 5)

Nothing for peers without relay. If listening, out of IBD and `now > next_local_addr_send`: on every time but the first, reset `addr_known`; push our address from `GetLocalAddrForPeer` (§3.7.4) with our services and time now; reschedule `next_local_addr_send = GetExponentialRand(now, 24 h)`. Then, if `now > next_addr_send`: reschedule `GetExponentialRand(now, 30 s)`; drop queued addresses the peer already knows (and mark the rest known in the same pass); send `addrv2` if the peer asked for it, else `addr`; clear. Both timers start at 0, so a listening node out of IBD advertises itself in the first pass after `verack`. At M2 the node is always in IBD, so self-advertisement does not happen yet (as for dashd in IBD).

`GetExponentialRand(now, avg) = now + (−log1p(U × −2^−48)) × avg + 0.5 µs` with a 48-bit uniform `U` (`random.cpp:788-792/771-775`); P2a ports it exactly (f64 `ln_1p`), which is all the NFR-FP-03 KS test needs.

### 3.7 AddrMan, netgroup and local addresses

Core: `addrman.cpp`, `addrman_impl.h`, `netgroup.cpp`, `netaddress.cpp`, `util/asmap.cpp`, `addrdb.cpp`. Bucketing, selection and the `peers.dat` format are identical at both pins: the only behavioural changes in `addrman.cpp` are four lines in `GetAddr` (a `filtered` flag, default true at every existing caller) and the new `GetEntries` behind the hidden v24 `getrawaddrman` RPC. `addrman.cpp` line numbers below are `v23/v24`; v24 = v23 + 4 up to line 847.

#### 3.7.1 Port, not redesign

`net::addrman` is a line-by-line port of `AddrManImpl` with Core's names (`add_single`, `good_`, `attempt_`, `select_`, `get_addr_`, `resolve_collisions_`, `select_tried_collision_`, `make_tried`, `clear_new`, `delete`, `create`, `swap_random`, `check_addrman`). It takes `now` and an RNG from its caller instead of reading globals, which is the only structural change and what makes it fuzzable and oracle-comparable.

| Item | Core | Rust |
|---|---|---|
| Geometry | 1024 new × 64, 256 tried × 64; 8 tried buckets per group; 64 new buckets per source group; ≤ 8 references (`addrman_impl.h:27-34`, `addrman.cpp:26-44/30-48`) | the same constants; flat `[i32; 65536]` and `[i32; 16384]` tables, −1 = empty |
| Entries | `std::unordered_map<int, AddrInfo> mapInfo`; ids from `nIdCount++`, never reused | `Slab<AddrInfo>` indexed by id with the same allocation rule |
| Index | `unordered_map<CService,int>` keyed by IP **and port** (Dash multiport, `V4_MULTIPORT`; no `discriminatePorts` flag at either pin) | `hashbrown::HashMap<ServiceKey, i32>`, never iterated |
| Random order | `std::vector<int> vRandom`, swap-with-last removal; `GetAddr` permutes it permanently | identical `Vec<i32>` and operations |
| Collisions | ordered `std::set<int>` (≤ 10) | `BTreeSet<i32>` (iteration and `advance(randrange(size))` both need the order) |
| Network counts | `unordered_map<Network,{n_new,n_tried}>` keyed by `GetNetwork()` | `[Counts; 7]` by network id |
| Key | `nKey` 32 bytes; random, or `uint256{1}` in deterministic (test-only) mode | same; deterministic mode exists only for tests and the oracle comparison |

Bucket math (addrman fact sheet `factsheets/addrman.md` §2.5 in the scratch dir; reproduces Core's own vectors: tried bucket 40, new bucket 786, 160 distinct tried buckets, and 236/795 with `test/data/asmap.raw`):

```
cheap(x)                  = u64::from_le_bytes(SHA256d(x)[0..8])
tried_bucket(K, G)        = cheap(nKey ‖ len(G) ‖ G ‖ le64(cheap(nKey ‖ len(K) ‖ K) % 8)) % 256
new_bucket(G, S)          = cheap(nKey ‖ len(S) ‖ S ‖ le64(cheap(nKey ‖ len(G) ‖ G ‖ len(S) ‖ S) % 64)) % 1024
bucket_position(new, b, K)= cheap(nKey ‖ ('N' | 'K') ‖ le32(b) ‖ len(K) ‖ K) % 64
K = GetKey()   = 16-byte v1 address bytes (IPv4 as ::ffff:a.b.c.d) or raw 32-byte Tor/I2P bytes, ‖ port (big-endian)
G, S = GetGroup(addr), GetGroup(source)   (§3.7.3); len(·) is Core's CompactSize prefix (one byte here)
```

The port enters the tried bucket and the bucket positions but not the new bucket or the group. SHA-256d goes through the SH1 backend (DEC-28).

#### 3.7.2 Operations

All semantics are Core's `addrman.cpp` (`v23:1-1341` / `v24:1-1383`; summarised with line numbers in the addrman fact sheet §2.6–§2.15), which the port follows line by line. The points most likely to be ported wrongly, each with a named unit test:
- `IsTerrible` boundaries: tried within 1 min → never terrible; `nTime > now + 10 min`; `now − nTime > 30 d`; never succeeded and ≥ 3 attempts; last success > 7 d and ≥ 10 attempts (`addrman.cpp:67-90/71-94`).
- `GetChance` = `0.01` if tried < 10 min ago, times libm `pow(0.66, min(nAttempts, 8))` with `nAttempts` signed (a negative value from disk is kept) (`:92-105/96-109`).
- `AddSingle` (`:553-626/557-630`): unroutable → reject; the 2 h penalty is dropped when the address equals the source (`CNetAddr ==`, port ignored); an existing entry's time moves forward only by more than 1 h (advertised < 24 h ago) or 24 h; services are OR-ed; the early return compares the **unpenalized** time; tried or 8 references → stop; an extra reference is taken with probability 2^−refcount; the bucket uses the **caller's** source; an occupied slot is replaced if its occupant is terrible or has more references while the newcomer has none.
- `Good_` (`:628-684/632-688`): records `m_last_good` even for unknown addresses; never touches `nTime`; test-before-evict queues a collision (silently dropped when 10 are queued).
- `Attempt_` (`:698-716/702-720`): counts a failure only if the last counted attempt predates `m_last_good`.
- `Connected_` (`:849-866/877-894`): refreshes `nTime` only if it is more than 20 min stale; `SetServices_` overwrites.
- `Select_` (`:718-792/722-796`): `randbool` for new vs tried only when both are non-empty and `!new_only`; random bucket, random start position, linear probe; accept with `randbits(30) < chance_factor × GetChance() × 2^30` evaluated left to right in `f64`; `chance_factor *= 1.2` per rejection (multiplied, never `1.2^k`). The network filter uses `GetNetwork()`.
- `GetAddr_` (`:811-847/815-851`): `nNodes = max_pct × size / 100` (integer), capped by `max_addresses`; a partial Fisher–Yates over `vRandom` that persists; the network filter uses `GetNetClass()` (so 6to4 and Teredo count as IPv4 here but as IPv6 in `Select`); terrible entries skipped (v24: only when `filtered`, which every pre-existing caller passes).
- `ResolveCollisions_` and `SelectTriedCollision_` (`:895-984/923-1012`) with Core's 4 h, 60 s and 40 min windows; `SelectTriedCollision_` does not remove the chosen id.
- `Check()` with `-checkaddrman=<ratio>` (default 0 on every network, clamped to [0, 10^6]); a ratio above 1 draws from the RNG in every public call, so the port draws too.

The RNG is Core's `FastRandomContext`: ChaCha20 (RFC 8439, nonce 0, counter 0), a 64-bit bit buffer whose refill discards leftover bits, `randrange` by rejection with `bit_width(r−1)` bits and no draw for `randrange(1)`, `rand64`/`rand256` bypassing the buffer (addrman fact sheet §2.16). P2a carries this generator (`net::rng::FastRandom`) and uses it everywhere Core's code would; its first deterministic `rand64` is checked against `0x903df1a0ade0b876`.

#### 3.7.3 Netgroups, asmap and address predicates

`GetGroup` (`netgroup.cpp:17-78/18-79`): an asmap hit gives `[NET_IPV6, asn LE]`; otherwise `[GetNetClass()]` followed by: nothing for local or unroutable; 10 hash bytes for internal; the /16 of the linked IPv4 for IPv4, 6to4, Teredo (bit-inverted), NAT64 and SIIT; 4 bits for Tor and I2P; 12 for CJDNS; 36 for Hurricane Electric `2001:470::/36`; 32 for other IPv6, with the partial byte OR-ed with ones. `GetMappedAS` interprets Core's asmap bytecode (`util/asmap.cpp:88-131`, bits LSB-first per byte from the file, IP bits MSB-first, IPv4 as `::ffff:`) after `SanityCheckASMap`. The interpreter is 60 lines and a pure function, so P2a ports it (with Core's `asmap_tests` and the fuzz target `asmap`) because bucket parity needs it; P8 keeps `-asmap` loading UX, the health-check logging and `feature_asmap` (§8 Q6).

The routability predicates (`IsRFC1918` … `IsRFC7343`, `IsHeNet`, `IsLocal`, `IsValid`, `IsRoutable`, `GetNetwork`, `GetNetClass`, `HasLinkedIPv4`, `GetLinkedIPv4`, `IsAddrV1Compatible`, `IsRelayable`; `netaddress.cpp:313-785`, identical at both pins) are ported as methods on a `NetAddr` newtype over base-sdk's address bytes. Dash's `-allowprivatenet` (default 0) makes RFC 1918 addresses routable process-wide; it is a `NetConfig` value passed in, never a global.

#### 3.7.4 Local addresses (`mapLocalHost`)

Ported from `net.cpp:176-326/177-327` (`factsheets/connman.md` §9): scores `LOCAL_IF` 1, `LOCAL_BIND` 2, `LOCAL_MAPPED` 3, `LOCAL_MANUAL` 4; `AddLocal` rules (unroutable rejected while `RequireRoutableExternalIP`, which is false only on regtest; below `LOCAL_MANUAL` only with `-discover`; unreachable networks rejected; re-adding increments the score); `SeenLocal` bumps only known addresses, from inbound peers only; `GetLocal` picks the best `GetReachabilityFrom(peer)` then score, never crossing between privacy and clear networks, falling back to `0.0.0.0:<listen port>`; `GetLocalAddrForPeer` substitutes the peer-reported address when `IsPeerAddrLocalGood` holds and either ours is unroutable or `randbits(score > LOCAL_MANUAL ? 3 : 1) == 0`, keeping our listen port for outbound peers. The sources are `-externalip` and bound addresses (P2a), interface discovery `Discover()` (P8) and NAT mapping (P8).

### 3.8 ConnMan (outbound)

Core: `CConnman` in `net.cpp`. The outbound logic (`ThreadOpenConnections`, `ThreadDNSAddressSeed`, `ThreadOpenAddedConnections`, `OpenNetworkConnection`, `ConnectNode`, the socket and message handlers) is identical between the pins except for the items in §3.8.10; `diff net.cpp` is 303 lines in total. Line numbers are `v23/v24`.

#### 3.8.1 Tasks and start order

`ConnMan::start` follows `CConnman::Start` (`net.cpp:3985/3991`): `InitBinds` if listening (§3.8.9); queue every `-seednode` as an ADDR_FETCH; read `anchors.dat` if addrman outbound is in use (no `-connect`); create the outbound semaphore (11 = 8 + 2 + 1 by default, `min(m_max_outbound, nMaxConnections)`) and the addnode semaphore (8); then spawn, in Core's order:

| Task | Core thread | Started when | Cadence |
|---|---|---|---|
| `socket` (the disconnector and inactivity sweep; readers/writers are per-node P1 tasks) | `net` | always | wakes on any socket event or after 500 ms (Core's `SELECT_TIMEOUT_MILLISECONDS` with the wakeup pipe, `net.cpp:113-119/114-120`); inactivity sweep when > 1 s since the last; then `DisconnectNodes` |
| `dnsseed` | `dnsseed` | `-dnsseed` (default 1) | once (§3.8.4) |
| `addcon` | `addcon` | always | §3.8.5 |
| `opencon` | `opencon` | no `-connect`, or a non-empty `-connect` list | 500 ms sleep per iteration plus the blocking connect (§3.8.2) |
| `mncon` | `mncon` | v23: always; v24: masternode, `-watchquorums` or a loaded wallet (§3.8.10) | P7 |
| `msghand` | the `PeerManager` thread | always | §3.1 |

Scheduler jobs: `DumpAddresses` every 15 min (`DUMP_PEERS_INTERVAL`), `CheckForStaleTipAndEvictPeers` every 45 s (on the actor, §3.1.6), and, v24 with `-asmap` only, the ASMap health check (§3.8.10). Shutdown follows `StopNodes`: write `anchors.dat` (§4.2), dump `peers.dat`, disconnect everyone.

Sleeps use real time (Core's `interruptNet.sleep_for` is not mockable); every time *read* uses `NodeClock` (mockable), exactly as Core splits them. In `sim` both come from the virtual clock.

#### 3.8.2 `opencon` (Core `ThreadOpenConnections`, `net.cpp:2923-3298/2917-3292`)

**`-connect` mode** (`:2926-2950/2920-2944`): forever, dial every `-connect` address as MANUAL (v2 iff we advertise `NODE_P2P_V2`) without a semaphore grant, sleeping `min(pass, 10) × 500 ms` after each one and 500 ms after each pass, then `PerformReconnections`. `OpenNetworkConnection` skips names already connected, so each pass retries only the missing peers. `-connect` soft-sets `-dnsseed=0` and `-listen=0`, disables `-seednode`, and makes `mncon` return at once.

**Main loop, one iteration** (`:2953-3297/2947-3291`):
1. `ProcessAddrFetch`: pop one ADDR_FETCH destination; **try** a grant; none → the entry is dropped; else dial it (v2 iff we advertise it).
2. Sleep 500 ms; `PerformReconnections` (v1 retries, §3.8.5).
3. **Block** on an outbound grant.
4. Fixed seeds (§3.8.6).
5. Count outbound connections, **only when `!AllowMultipleAddressesFromGroup`**: that flag is false on mainnet and testnet and **true on devnet and regtest** (`chainparams.cpp:283,476,658,857/294,497,689,895`), so on regtest and devnets every counter stays 0, no netgroup set is built, and the ladder below always picks full-relay (or block-relay for anchors), bounded only by the 11 grants. Full-relay counts exclude masternode connections; the netgroup set includes manual, full-relay (masternode connections included) and block-relay connections; Tor/I2P/CJDNS count as privacy-network peers instead.
6. Collect the verified ProRegTx hashes of all nodes.
7. The connection-type ladder (first match wins, `:3101-3154/3095-3148`): anchors left and block-relay below 2 → BLOCK_RELAY (anchor); full-relay < 8 → OUTBOUND_FULL_RELAY; block-relay < 2 → BLOCK_RELAY; the stale-tip flag → OUTBOUND_FULL_RELAY; `now > next_extra_block_relay` and extra block-relay peers started → BLOCK_RELAY (reschedule Exp(5 min)); `now > next_feeler` → FEELER (reschedule Exp(2 min)); all 8 full-relay up, `now > next_extra_network_peer` and `MaybePickPreferredNetwork` finds a reachable network with no outbound manual/full-relay peer and addrman entries → OUTBOUND_FULL_RELAY restricted to it (reschedule Exp(5 min)); Dash only: fewer than `min(2, maxconnections/2)` onion full-relay peers and Tor reachable → OUTBOUND_FULL_RELAY onion-only; else release the grant and loop.
8. `addrman.resolve_collisions()`; snapshot the tip MN list (for step 9m).
9. Address selection (`:3161-3270/3155-3264`), `nTries` from 0, first decisive rule wins: (a) anchors: pop from the **back**; skip (not retried) if invalid, local, unreachable, lacking desirable services, or its netgroup is used; when they run out fall through to addrman with the same type; (b) `++nTries > 100` → give up this iteration; (c) feeler: `SelectTriedCollision`, and if that address is connected, `Good` it and take `Select(new_only)`; otherwise `Select(false, preferred_net)`; (d) not a feeler and netgroup already used → next (this precedes the validity check); (e) invalid → give up; (f) our own listen port on a local address → give up; (g) unreachable → next; (h) onion-only and not Tor → next; (i) tried within 10 min and `nTries < 30` → next; (j) not a feeler and lacking desirable services, or a feeler lacking `MayHaveUsefulAddressDB` → next; (k) `nTries < 50` and a bad port (1–1023 and Core's list, `netbase.cpp:781/801`) → next; (l) an `-addnode` target (checked only while fewer than 24 are configured) → next; (m) Dash: a masternode we are already connected to → next; else accept. There is no default-port preference and no "after 40 tries" relaxation at either pin.
10. Feelers sleep uniform [0, 1 s) first; `count_failures = (#ipv4/6 netgroups + #privacy peers) ≥ min(maxconnections − 1, 2)` (false on regtest/devnet); **v2 iff `addr.nServices & our services & NODE_P2P_V2`** (addrman's belief about the peer); `OpenNetworkConnection(addr, count_failures, grant, type, v2)`.

`next_feeler`, `next_extra_block_relay` and `next_extra_network_peer` start at `GetExponentialRand(start, …)`. Extra block-relay peers are enabled once, the first time `CanDirectFetch()` holds in `CheckForStaleTipAndEvictPeers` (§3.9.3); the stale-tip flag is set and cleared there too.

#### 3.8.3 Anchors

At most 2 (`MAX_BLOCK_RELAY_ONLY_ANCHORS`). Read at start only when addrman outbound is in use; the file is **deleted immediately after reading** (`addrdb.cpp:242/249`); truncated to 2. Tried first (ladder row 1), popped from the back, never retried. Written only on a clean shutdown with the current block-relay-only connections' addresses (`GetCurrentBlockRelayOnlyConns`, `net.cpp:3301/3295`, the `IsBlockRelayOnly()` predicate, which also matches any peer under `-blocksonly` without the `relay` permission), in node-table order, at most 2. Anchors never enter addrman.

#### 3.8.4 DNS seeds (`ThreadDNSAddressSeed`, `net.cpp:2686-2800/2680-2794`)

Shuffle the seeds; query all of them at once if `-forcednsseed` or addrman is empty; otherwise wait 11 s (addrman < 1000 entries) or 5 min (≥ 1000), in 11 s steps, and stop ("P2P peers available. Skipped DNS seeding.") as soon as two full-outbound, handshaken, non-probe connections exist (masternode connections count, block-relay-only do not). Batches are 3 seeds, but each network has one seed (`dnsseed.dash.org.` on mainnet; testnet's `testnet-seed.dashdot.io.` "only supports x9"; regtest's `dummySeed.invalid.`; none on devnets). Per seed: wait while the network is inactive; with a name proxy (P8) → ADDR_FETCH the bare name; else resolve `x1.<seed>` (always `x1`, since `GetDesirableServiceFlags(NODE_NONE)` is `NODE_NETWORK`), take ≤ 32 results, date each uniformly in (now − 7 d, now − 3 d], services `NODE_NETWORK`, and `addrman.add(results, source = "x1.<seed>" as an internal address)`; an empty answer → ADDR_FETCH the bare seed name. The resolver is the system resolver (`getaddrinfo` semantics through `tokio::net::lookup_host`), as Core uses `getaddrinfo`.

#### 3.8.5 `addcon`, reconnections and the dial path

`addcon` (`net.cpp:3374-3401/3368-3395`): block on an addnode grant; for each added node not connected (numeric entries match `IP:port` against peer addresses, names match the connection name; inbound peers count): dial as MANUAL with the entry's v2 flag (for `-addnode`, iff we advertise v2), sleep 500 ms, try the next grant; then `PerformReconnections`; sleep 60 s if anything was tried, else 2 s. Manual peers get no permissions and are never discouraged.

**v1 reconnection** (`ShouldReconnectV1`, `net.cpp:1653/1647`): an initiating v2 transport that is still waiting for the key, has received nothing and has sent at least 24 bytes is reconnected over v1 with the same address, type, grant and masternode flags, `count_failures = false`, on the next `opencon`/`addcon` pass. P1 reports the condition (`ReaderEvent::V2HandshakeFailedEarly`); ConnMan queues the retry in `DisconnectNodes`, as Core.

**`OpenNetworkConnection`** (`net.cpp:3592-3671/3586-3665`): return if interrupted or the network is inactive; for an address: return if banned, discouraged, or already connected (`FindNode(CService)`: full `IP:port`, ignoring nodes that are disconnecting), or if it is our own listen port on a local address; for a name: return if a node with that name exists; `ConnectNode`; attach the grant; set the masternode flags (P7); `PeerManagerHandle::initialize_node` (which pushes `version` before the node is visible to the message loop); append to the node table; count manual/full-relay connections per network.

**`ConnectNode`** (`net.cpp:371-516/372-517`): names are resolved (≤ 256 results, shuffled, the last one used, any already-connected result aborts); direct TCP connect with `-timeout` (default 5000 ms) and `TCP_NODELAY`, no keepalive or linger options; proxies, Tor and I2P are P8 (an `-proxy`/`-onion`/`-i2psam` setting is an InitError "not supported by rdashd yet" until P8, A1 §2.18); `addrman.attempt(addr, count_failures)` on every attempt to a valid address; node ids are a counter shared with inbound connections, from 0; the local nonce (§3.3.4); the connection name is the destination string or `addr:port`. The connect blocks the dialling task (opencon, addcon, mncon), as in Core.

#### 3.8.6 Fixed seeds

After the grant, if `-fixedseeds` (default 1) and some reachable network has an empty addrman: when 60 s have passed since `opencon` started, or immediately if there is no DNS seeding, no `-seednode` and no `-addnode`, add `chainparamsseeds.h`'s addresses for those networks with services `NODE_NETWORK`, a time uniform in (now − 14 d, now − 7 d], source `"fixedseeds"`; once per process (`net.cpp:2980-3018/2974-3012`). `chainparamsseeds.h` is identical at both pins: mainnet 170 IPv4 + 25 TorV3, testnet 19 IPv4, none on devnet and regtest. P2a generates `seeds.rs` from it with a script (`contrib/node/gen-fixed-seeds.py`, BIP155 bytes verbatim) and a test asserts the table equals the pinned header's bytes (A1 §2.14, FR-P2P-07).

#### 3.8.7 The socket task: inactivity, disconnect, flow control

- **Inactivity** (`InactivityCheck`, `net.cpp:2304-2337/2298-2331`), swept when more than 1 s has passed since the last sweep (steady clock at v24, system clock at v23): only once `connected + -peertimeout (60 s) < now`; no byte ever received or ever sent → `"socket no message in first %i seconds, %d %d peer=%d"`; nothing sent for 20 min → `"socket sending timeout: %is peer=%d"`; nothing received for 20 min → `"socket receive timeout: %is peer=%d"`; handshake not complete → `"version handshake timeout peer=%d"` or `"V2 handshake timeout peer=%d"` (P1 reports whether the transport is still detecting). `connected` is whole seconds, so a silent peer goes 60–62.5 s after connect, as with dashd.
- **Disconnect** (`DisconnectNodes`, `net.cpp:2073-2207/2067-2201`): when a node is marked, start a 100 ms linger; on the first pass at which the writer's queue is empty, `shutdown(SHUT_WR)`; on the first pass after the deadline, remove the node, queue a v1 retry if applicable, release the grant, close, decrement the per-network count, and tell the actor (`finalize_node`). A peer-initiated close or a transport error closes at once (no linger). Disconnecting nodes are no longer read but are still flushed. With the 500 ms idle cadence the close lands 100–600 ms after the mark; the socket task reproduces that cadence rather than closing at exactly 100 ms.
- **Send side.** Core never sends from the calling thread (`PushMessage` only queues and wakes the socket thread, `net.cpp:4816-4864/4857-4905`) and writes with `MSG_MORE` while more bytes follow, so a burst of messages (e.g. `sendaddrv2` + `verack` + `getaddr`) usually leaves as one TCP segment. **P1's writer must coalesce** every queued frame into one `writev` per wake-up, with `TCP_NODELAY` on, to reproduce the segment boundaries a capturing peer sees. `fPauseSend` is `queued bytes > -maxsendbuffer × 1000` (default 1 MB), updated in `push_message` and after each write.
- **Receive side.** One read of up to 64 KiB per node per socket-loop round (`net.cpp:2601/2595`); every message completed by that read is classified and handed to the actor together, then one wake (§3.1.3). Reading stops while `fPauseRecv` (`queued bytes > -maxreceivebuffer × 1000`, default 5 MB).

#### 3.8.8 Limits

| Item | Value | Core |
|---|---|---|
| `-maxconnections` | 125; reduced to fit file descriptors (`nMaxConnections = min(maxconn, fd_max − binds − 150 − 8 − 1)`); InitError below 150 descriptors | `init.cpp:1243-1262/1295-1314` |
| full-relay / block-relay / feeler | `min(8, maxconn)` / `min(2, maxconn − full)` / 1 | `init.cpp:2542-2544/2609-2611` |
| outbound grants | `min(full + block + feeler, maxconn)` = 11 | `net.cpp:4036/4042` |
| addnode grants | 8 (separate) | `net.cpp:4040/4046` |
| onion full-relay target (Dash) | `min(2, maxconn / 2)` | `init.cpp:2544/2611` |
| inbound | `maxconn − 11` = 114 (P2b/P4) | `net.cpp:1889/1883` |
| connections that take no outbound grant | `-connect`, addnode (own pool), masternode connections, v1 retries (reuse the old grant) | §3.8.5 |

#### 3.8.9 `InitBinds` and `bind_failure_fatal`

ConnMan binds and listens; P2b accepts. `InitRules.bind_failure_fatal` (O17 P2P-33) is read by ND1 and passed into `ConnManOptions`:
- `false` (`V23_1`, `net.cpp:3963-3983`): startup succeeds if any bind succeeds; a failed explicit `-bind`/`-whitebind` is reported but tolerated; onion binds are `BF_DONT_ADVERTISE` only, so their errors are silent; for the default binds a failure is reported only if both `::` and `0.0.0.0` fail;
- `true` (`V24_0`, `net.cpp:3957-3989`): any failed explicit `-bind`, `-whitebind` or onion bind (now `BF_REPORT_ERROR`), or a failed `0.0.0.0` default bind, makes `InitBinds` fail → "Failed to listen on any port. Use -listen=0 if you want this."; only `[::]` stays non-fatal.

Bound routable addresses are added to `mapLocalHost` as `LOCAL_BIND` unless `noban`/`DONT_ADVERTISE` (`net.cpp:3957/3951`). The onion-bind defaults (`InitRules.onion_bind`, P2P-34) are P8's.

#### 3.8.10 Pin differences in this area

| Difference | v23.1.8 | v24.0.0-rc.3 | Handling |
|---|---|---|---|
| linger, inactivity-sweep cadence, MN send throttle clock (#7283) | system clock, ms | steady clock | steady clock under both profiles: the values are equal and only a wall-clock step separates them, which no peer can trigger; declared in §3.15 as unmodelled (no O17 row exists; it was classified "not a delta") |
| `InitBinds` (#7300) | lenient | strict | `bind_failure_fatal` (above) |
| `mncon` start (#7659) | always | MN, `-watchquorums` or wallet | unobservable: without one of those nothing queues MN targets (P7 decides) |
| ASMap health check | absent | at start and every 24 h with `-asmap`: two unfiltered `GetAddr` passes (permuting `vRandom`, drawing randomness) and a log line | P8 owns `-asmap` loading; the check's log line is asserted by v24's `feature_asmap.py` (O17 §2, #7121). Recommend an `InitRules`/`P2pRules` flag so `V23_1` skips it (§8 Q9) |
| `PROTOCOL_VERSION` | 70240 | 70242 | `Identity` |

### 3.9 Outbound eviction and the stale tip

Core: `ConsiderEviction` `v23:5803-5859` / `v24:5949-6005`; `EvictExtraOutboundPeers` `v23:5861-5973` / `v24:6007-6119`; `CheckForStaleTipAndEvictPeers` `v23:5975-6000` / `v24:6121-6146`; `TipMayBeStale` `v23:1395-1403` / `v24:1418-1426`; ConnMan's counters `net.cpp:2833-2905/2827-2899`. Identical at both pins apart from the clock in `CanDirectFetch` (P2P-16).

#### 3.9.1 `ConsiderEviction` (SendMessages section 11)

For an unprotected outbound full-relay or block-relay-only peer **whose headers sync has started** (so pure masternode connections, which never start it, are exempt), with `now = GetTime()` seconds:
- its best known block has at least the tip's work → clear the timeout;
- else, no timeout yet, or it caught up to the work recorded at the last check → `timeout = now + 20 min`, record the tip, clear `sent_getheaders`;
- else, past the timeout: if `sent_getheaders` → `"Disconnecting outbound peer %d for old chain, best known block = %s"`, disconnect; otherwise `maybe_send_getheaders(locator(work_header.prev))` (`getheaders2` by bit 11), log `"sending %s to outbound peer=%d to verify chain work (current best known block:%s, benchmark blockhash: %s)"` whether or not the 2-minute throttle let it out, set `sent_getheaders`, `timeout = now + 2 min`.

Protection (`m_protect`) is granted only in `UpdatePeerStateForReceivedHeaders` (§3.4.3 step 11), to at most 4 outbound full-relay peers (masternode connections included), and released in `finalize_node`.

#### 3.9.2 `EvictExtraOutboundPeers(now)`

- **Extra block-relay-only** (`GetExtraBlockRelayCount() > 0`: handshaken block-relay-only peers above 2): scan nodes in table order keeping the youngest (highest id) and the previous youngest (`next` is only updated when a new youngest is found, as written); disconnect the youngest unless its last block time is newer than the second's, in which case the second; only if connected **≥ 30 s** and nothing in flight, logging `"disconnecting extra block-relay-only peer=%d (last block received at time %d)"`, else `"keeping block-relay-only peer=%d chosen for eviction (connect time: %d, blocks_in_flight: %d)"`.
- **Extra full-relay** (`GetExtraFullOutboundCount() > 0`: handshaken full-relay peers above 8, masternode and probe connections excluded): scan in table order, skipping the first `min(2, maxconnections/2)` Tor peers of **any** type (Dash), masternode connections, non-full-relay or disconnecting peers, protected peers, and the only manual-or-full-relay peer on its network; pick the oldest `last_block_announcement`, ties to the higher id; disconnect it only if connected **> 30 s** (strict, unlike the block-relay case) with nothing in flight (`"disconnecting extra outbound peer=%d (last block announcement received at time %d)"`), and then clear the stale-tip flag.

#### 3.9.3 `CheckForStaleTipAndEvictPeers` (every 45 s, §3.1.6)

`EvictExtraOutboundPeers(now)`; then, if `now > stale_tip_check_time`: when not loading blocks, the network is active, addrman outbound is in use and `TipMayBeStale()` (last tip update more than 3 × 150 s ago — lazily initialised to now on first use — and no block in flight), log `"Potential stale tip detected, will try using extra outbound peer (last tip update: %d seconds ago)"` and set ConnMan's flag, otherwise clear it if set; `stale_tip_check_time = now + 150 s` (so the effective cadence is 180 s). Finally, the first time `CanDirectFetch()` holds (tip time within 20 × 150 s of `now_sync`, §3.4.2's clock), enable extra block-relay peers (`"enabling extra block-relay-only peers"`). `last_tip_update` is set by `BlockConnected`, so at M2 (no blocks connected) the stale-tip logic fires every 180 s once the first 450 s pass, exactly as for a dashd that receives no blocks.

### 3.10 Misbehaviour

#### 3.10.1 The framework

`Misbehaving(peer, howmuch, msg)` (`v23:1953-1978` / `v24:1933-1955`, same effect): add to the score; when the score crosses 100 from below, set `should_discourage` and append `" DISCOURAGE THRESHOLD EXCEEDED"`; log `"Misbehaving: peer=%d (%d -> %d)%s%s"` with `": " + msg` when there is a message. The flag is raised once; scores keep accumulating without re-raising it. Scores never decay.

`MaybeDiscourageAndDisconnect` (`v23:5689-5726` / `v24:5833-5870`), the first step of every `send_messages` (so it acts in the same loop pass as the offending message, except on masternode connections, which get their pass at most every 100 ms): clear the flag **before** the exemptions; `noban` → `"Warning: not punishing noban peer %d!"`; manual → `"Warning: not punishing manually connected peer %d!"`; a local address → `"Warning: disconnecting but not discouraging %s peer %d!"` ("inbound onion" or "local") and disconnect; otherwise `"Disconnecting and discouraging peer %d!"`, `BanList::discourage(addr)` and disconnect **every** node with that IP, whatever the port (`DisconnectNode(CSubNet(addr))`, logging `"disconnect by subnet%s matched peer=%d; disconnecting"`).

P2a ships `DiscourageOnly`, an in-memory `BanList` with Core's discouragement filter (a rolling bloom of 50,000 entries at 10^−6, `banman.h`), consulted by `OpenNetworkConnection` and the `addr` handler; P4 replaces it with BanMan (bans, `banlist.json`, `listbanned`/`setban`).

#### 3.10.2 Scores in P2a's command set

| Trigger | Score | Message | Core (v23 / v24) |
|---|---|---|---|
| `addr`/`addrv2` > 1000 entries (relay enabled) | 20 | `"%s message size = %u"` | `:4363` / `:4408` |
| `inv` > 50,000 | 20 | `"inv message size = %u"` | `:4467` / `:4512` |
| `getdata` > 50,000 | 20 | `"getdata message size = %u"` | `:4574` / `:4618` |
| `notfound` > `P2pRules.notfound_limit` (116 / 50,000; O17 P2P-10) | 20 | `"notfound message size = %u"` | `:5602-5604` / `:5752-5754` |
| `headers`/`headers2` count > limit | 20 | `"headers message size = %u"` (also for `headers2`) | `:5248` / `:5325` |
| every 10th unconnecting `headers` (≤ 8 entries) | 20 | `"%d non-connecting headers"` | `:3179` / `:3163` |
| unconnecting `headers` with > 8 entries | 10 | `"invalid header received"` | `:3362` / `:3346` |
| non-continuous headers | 20 | `"non-continuous headers sequence"` | `:3369` / `:3353` |
| invalid header from CH1 | per §3.10.3 | `"invalid header received"` | `:3380` / `:3364` |
| devnet name mismatch | 100 outbound, 1 inbound | none at v23; `"wrong-devnet-name"` at v24 (log text only) | `:4008,4010` / `:4037,4039` |
| `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit`/`qwatch` at a node with no DKG role | 10 | none | `:5650-5659` / `net_dkg.cpp:938-944` |

Disconnects without a score: obsolete version, missing desirable services, inbound `fMasternode` at a non-masternode, self-connection, feeler completion, `sendaddrv2` after `verack`, the `sendtxrcncl` cases, a locator above 101, blocks-only violations (message or inv), the MN-probe first-message rule, ping timeout, insufficient-work headers chains in IBD, `ConsiderEviction`, ADDR_FETCH completion and timeout. Never scored: unknown commands, anything before `version` or before `verack`, decode failures.

#### 3.10.3 `MaybePunishNodeForBlock` (header results)

`v23:1992-2040` / `v24:1969-2019` (=): `BLOCK_CONSENSUS` and `BLOCK_MUTATED` → 100 unless via a compact block; `BLOCK_CACHED_INVALID` → 100 for outbound peers only, unless via a compact block; `BLOCK_INVALID_HEADER`, `BLOCK_CHECKPOINT`, `BLOCK_INVALID_PREV` → 100; `BLOCK_MISSING_PREV` and Dash's `BLOCK_CHAINLOCK` → 10; `BLOCK_RECENT_CONSENSUS_CHANGE`, `BLOCK_TIME_FUTURE` and unset → no score, log `"peer=%d: %s"`. CH1's `HeadersReply.invalid` carries CS0's `BlockValidationResult` and reject reason (`high-hash`, `bad-diffbits`, `time-too-old`, `time-too-new` (TIME_FUTURE; its clock is `UngatedRules.time_source`, O17 CONS-04), `bad-version(0x%08x)`, `bad-fork-prior-to-checkpoint`, `duplicate` (CACHED_INVALID or CHAINLOCK), `prev-blk-not-found`, `bad-prevblk`, `bad-prevblk-chainlock`, `bad-chainlock`, `devnet-genesis`), so the mapping is one `match` in `misbehavior.rs`, shared with P3.

### 3.11 The 74 commands at M2

The M2 node is a non-masternode, wallet-less node in IBD (tip at genesis, headers syncing, masternode sync at BLOCKCHAIN, empty mempool, no sporks stored). The table gives dashd's outcome in that state for a handshaken outbound full-relay peer (both pins unless marked), and what rdashd does at M2. **Exact** means byte- and score-identical; a **deviation** is an interim difference, listed in STATUS with the task that closes it. Block-relay-only peers first apply gate 7 (26 commands disconnect without decoding); before `version` and before `verack` every command gets the gate 2 / gate 5 log (§3.2.1). Each row becomes the `m2` field of `policy.rs` and a `sim` test.

Spork state that decides several rows: on **mainnet** every spork value is hard-coded (SPORK_21 = 1, all others 0 → active) whatever is received (`v23:spork.cpp:265-276` / `v24:171-181`), so ChainLocks and InstantSend are enabled; on testnet, devnets and regtest the defaults (4070908800 = off) apply until a spork arrives.

| Commands | dashd at M2 | rdashd at M2 | Status |
|---|---|---|---|
| `version`, `verack`, `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2`, `sendtxrcncl`, `senddsq`, `qsendrecsigs`, `ping`, `pong`, `addr`, `addrv2`, `getaddr`, `headers`, `headers2`, `inv` (block path; unknown types; type 34 per profile) | §3.2–§3.6 | P2a handlers | exact |
| `inv` (spork entries) | `MSG_SPORK` is the only object requested in IBD → a later `getdata(spork)` | not requested (`ObjectRequests` is a no-op) | deviation → SP0 + P5a (M4) |
| `getdata` | > 50,000 → +20; known non-block types we lack → one `notfound`; unknown block hash → silent; genesis (or a connected block) → `block`; unknown inv type → stops the batch silently (type 34: unknown under v23, `notfound` under v24) | P2a's `EmptyServing` default: the same, except genesis is not served | exact except `getdata(genesis)` → P4 (M5) |
| `notfound` | > 116 (v23) / > 50,000 (v24) → +20; else completes our requests (none at M2) | P2a | exact |
| `getheaders`, `getheaders2` | locator > 101 → disconnect; mainnet/testnet: tip below minimum chain work → **empty** `headers`/`headers2` by the peer's bit 11; regtest/devnet: our headers after the fork (empty at genesis) | P2b serving | exact (P2b, M2) |
| `getblocks` | locator > 101 → disconnect; tip at genesis → no reply | P2a stub (decode, the locator rule, no reply) | exact while the tip is genesis; P4 serves (M5) |
| `mempool` | NODE_BLOOM offered (default) → flag set, empty mempool → no `inv`; without NODE_BLOOM and no `noban` → disconnect | P2a stub (the gate and the flag) | exact |
| `tx`, `dstx` | first statement returns in IBD: silent | gate 7, then silent | exact |
| `block` | mutated → +100 `"mutated block"`; `CheckBlock` failure → +100; unknown prev → +10; otherwise the header is added and the block discarded (low work), `"ProcessNewBlock : ACCEPTED"` logged | ignored | deviation → P3 (M3) |
| `cmpctblock` | unknown prev → silent in IBD; known prev → header processed (+100/+10 on invalid, `"Saw new cmpctblock header"`), no fetch | ignored | deviation → P3 (M3) |
| `blocktxn` | `"Peer %d sent us block transactions for block we weren't expecting"`, no score | P2a stub (log) | exact |
| `getblocktxn` | unknown → log, no reply; genesis → `blocktxn` | P2a stub (log, no reply) | exact except genesis → P4 |
| `merkleblock`, `cfilter`, `cfheaders`, `cfcheckpt` | no handler at either pin: silent, never decoded | registry fallback: silent | exact |
| `getcfilters`, `getcfheaders`, `getcfcheckpt` | decoded, then `NODE_COMPACT_FILTERS` not offered → `"peer %d requested unsupported block filter type: %d"`, disconnect | P2a stub (BS7 decode, the log, disconnect) | exact |
| `filterload`, `filteradd`, `filterclear` | NODE_BLOOM offered: oversize/misformatted filter +100; `filteradd` without a filter or over 520 bytes +100; otherwise filter state changes | ignored | deviation → P4 (M5) |
| `spork` | stored, logged and relayed as `inv` when valid and new; +100 when invalid or malformed (v24 with messages) | ignored | deviation → SP0 (M4) |
| `getsporks` | nothing stored → no reply (v24 logs a repeat) | registry fallback: no reply | exact on the wire |
| `ssc` | log only (not synced) | silent | exact on the wire |
| `govsync`, `govobj`, `govobjvote`, `mnauth`, `platformban`, `dsq` | silent: not blockchain-synced | silent | exact |
| `dsa`, `dsi`, `dss` (masternode only), `dsf`, `dsc`, `dssu` (no mixing session), `qsigsesann`, `qsigsinv`, `qgetsigs`, `qbsigs`, `qsigshare` (masternode only) | silent | silent | exact |
| `qcontrib`, `qcomplaint`, `qjustify`, `qpcommit`, `qwatch` | +10, no message (v23 inline rule, v24 `NetDKGStub`) | registry fallback +10 | exact |
| `qgetdata` | +10 `"not a verified masternode or a qwatch connection"`, not decoded | P2a stub | exact |
| `qdata` | +10 `"not a verified masternode and -watchquorums is not enabled"`, not decoded | P2a stub | exact |
| `mnlistdiff`, `qrinfo` | +100 `"received not-requested mnlistdiff. peer=%d"` / `"received not-requested quorumrotationinfo. peer=%d"`, not decoded | P2a stub | exact |
| `getmnlistd` | +1 `"getmnlistdiff failed …"`, or an empty-list `mnlistdiff` for (null or genesis, genesis) | ignored | deviation → P6b (M5) |
| `getqrinfo` | v23: +1 (malformed: exception); v24: +1, +100 malformed or > 4096 bases, 0 for a header-only target | ignored | deviation → P6b (M5) |
| `qfcommit` | v23: content checks, usually 0 (unknown quorum hash); v24: always +10 `"unrequested quorum commitment"` | ignored | deviation → LQ2/P5a (M4) |
| `qsigrec` | +100 for an LLMQ type not on this network; otherwise silent | ignored | deviation → SG1a/P5a |
| `clsig` | testnet/devnet/regtest: silent (spork 19 off). Mainnet: +10 (v23 after a failed verification, v24 `"unrequested clsig"`) | ignored | exact off mainnet; mainnet deviation → LK1a/P5a (M4) |
| `isdlock` | testnet/devnet/regtest: silent (spork 2 off). Mainnet: structural checks +100, unknown cycle hash +1, else queued and dropped by the worker | ignored | exact off mainnet; mainnet deviation → LK1b (M5) |
| any string not in the list | `"Unknown command"` | the same | exact |

So on regtest and testnet, where M2's functional tests and O13a run, every command is exact except `inv`/`spork` handling of sporks, the BIP37 trio, `block`/`cmpctblock`, `getmnlistd`/`getqrinfo`, `qfcommit` and `qsigrec`, each owned by a scheduled task. P2a's stubs are tiny (no decoding beyond BS7's small types) and are deleted when their owner registers the real handler, which a test enforces (a command may not have both a stub and an owner).

### 3.12 Every observable behaviour, per profile

The charter rule: a peer spoofing a user agent should struggle to tell rdashd from dashd. This table lists every behaviour a remote peer (or a passive observer of our traffic) can see that involves P2a's code, with how the design reproduces it under each profile and what proves it. Numbers are PRD §4.2 / A1 §4.10; "new" rows are behaviours this design found that the checklist does not list. "=" in the profile column means both profiles behave identically (verified in the sections cited).

| # | Behaviour | Profile | Reproduced by | Proved by |
|---|---|---|---|---|
| 1 | `version` layout and values (137-byte default; services; time = local not adjusted; addr_recv rule and its services echo; zero addr_from; nonce; UA; start height; relay; challenge; `fMasternode`) | `nVersion` 70240 / 70242; UA `23.1.8` / `24.0.0`; rest = | `fingerprint.rs` from `Identity` (§3.3.4) | O13a v1 + v2 transcripts vs both pins; unit fixture of the 137 bytes per profile |
| 2 | service bits | = (0x1C05 / 0x1C45 / pruned; MN `peerblockfilters=0` difference is `InitRules.mn_init`) | §3.3.3 | O13a; unit test per option combination |
| 3 | who speaks first | = | outbound `initialize_node` pushes `version` before the node is in the loop | O13a |
| 4 | after the peer's `version`: `sendaddrv2` (≥ 70223), [`sendtxrcncl`], `verack`, `getaddr` (every outbound type except block-relay-only, **feelers included**); feelers then disconnect | = | §3.3.5 | O13a per connection type (full, block-relay, feeler, manual, addr-fetch) |
| 5 | after `verack`: [`mnauth`], `sendheaders2`/`sendheaders`, `sendcmpct(0,1)` iff `CanRelay`, `senddsq(1)` unless tx-rejecting, [`qwatch`], then on the same pass `ping`, [`addr`], [`getheaders2`/`getheaders`] | = | §3.3.6 | O13a |
| 6 | messages before `version` / non-allowed before `verack` ignored, logged, unscored | = | §3.2.1 gates 1, 2, 5 | `diff_p2p_*` pre-version and pre-verack sequences; FT `p2p_leak`, `p2p_timeouts` |
| 7 | immediate `ping`, every 2 min after the last send, 20 min timeout; `getheaders` policy (2 min spacing, type by bit 11); empty `headers`/`headers2` below min chain work (P2b serves) | = except the sync-start and headers-timeout clock (P2P-16) | §3.4, §3.5 | FT `p2p_ping`, `p2p_initial_headers_sync`; KS on ping cadence (O13b); `diff_p2p_headers*` |
| 11 | unknown commands ignored without score; `feefilter`, `wtxidrelay`, `reject`, `alert`, `getaddrv2` never sent and unknown | = | gate 10; none of them is in `POLICY` or ever constructed | `diff_p2p_unknown`; a grep test that the five command strings appear nowhere in `dashd-p2p` |
| 12 | misbehaviour model (P2a's part: framework and the scores of §3.10.2) | = except `notfound_limit` (P2P-10) | §3.10 | `diff_p2p_*`; FT `p2p_invalid_messages` (P2a's cases) |
| 13 | blocks-only violations (messages and invs), incl. inv type 34 | type 34: unknown under `V23_1`, a `tx`-class violation under `V24_0` (P2P-04) | gate 7, §3.4.7, `P2pRules.msg_asset_unlock` | `diff_p2p_inv` per profile; FT `p2p_blocksonly` (P2a's part) |
| 14 | size limits: `inv`/`getdata` 50,000, `addr` 1000, `headers` 2000 / 8000, `notfound` 116 / 50,000, locator 101 | `notfound` per profile | §3.10.2 | `diff_p2p_*` at the boundaries |
| 15 | address relay: token bucket (0.1/s, 1000, +1000 after our `getaddr`), relay to 1–2 peers per 24 h epoch, freshness and size rules, 2 h penalty, one cached `getaddr` answer per inbound connection (21–27 h cache, 23 %, 1000), `getaddr` ignored on outbound, `sendaddrv2` after `verack` disconnects, ADDR_FETCH completion and 300 s timeout | = | §3.6 | FT `p2p_addr_relay`, `p2p_addrv2_relay`, `p2p_addrfetch`, `p2p_getaddr_caching` (with P2b); `diff_addrman` |
| 21 (part) | `headers2` encoding: reply mode vs announcement mode, per-message version cache; 8000/2000 limits | = | §3.4.9 (BS7 codec, two modes) | `diff_headers2` (BS7) + `sim` fixtures of both modes against Core captures |
| 25 | connection behaviour: 8 full + 2 block-relay + anchors; feelers Exp(2 min) + U[0,1 s); extra block-relay Exp(5 min) once direct-fetch is possible; extra full-relay on a stale tip (450 s, checked every 180 s); extra-network peer Exp(5 min); Dash onion extras; ADDR_FETCH rules; DNS seeding (`x1.`, ≤ 32, 11 s / 5 min, skipped with 2 peers); fixed seeds at 60 s; linger 100 ms then close at the next socket-loop tick; `shutdown(SHUT_WR)` once flushed; **regtest/devnet: no outbound accounting, so no block-relay or feeler connections and up to 11 full-relay** | = | §3.8 | FT `p2p_add_connections`, `feature_anchors`, `p2p_dns_seeds`, `p2p_addrfetch`; KS on feeler/extra-peer timers (O13b) |
| 26 | inactivity: first message within 60 s (dropped 60–62.5 s after connect), 20 min send/receive, handshake timeout | = | §3.8.7 | FT `p2p_timeouts` |
| 27 | devnet: `devnet.<name>` in the UA; mismatch +100 outbound / +1 inbound | = (log text differs) | §3.3.2, §3.3.5 step 9 | FT `p2p_connect_to_devnet` |
| +C | cross-peer ordering: peer A's `headers` is fully processed (best header updated) before peer B's next message; same-peer reply order | = | §3.1.5, §3.4.6 (`TipFacts` before the reply) | O13a's +C probe for headers: A sends headers, B immediately `getheaders`; B's reply includes them, as dashd's does |
| new-1 | quorum-priority receive queue: `clsig`, `isdlock`, `qsig*` overtake earlier normal messages (100:1) | = | §3.1.3 | `sim` unit test; O13b probe "`ping` then `clsig` in one segment" (with LK1a) |
| new-2 | TCP segment boundaries: a burst of messages leaves as one segment (Core's `MSG_MORE`), `TCP_NODELAY` set | = | P1 writer coalescing (§3.8.7) | passive capture comparison (O13b n-grams over segments) |
| new-3 | linger/close timing: FIN when the queue drains, close 100–600 ms after the mark (the socket loop's 500 ms idle tick), RST if unread data | = | §3.8.7 | O13b disconnect-timing probe |
| new-4 | `addr_recv.services` echoes addrman's belief (`NODE_NETWORK` for seeds, `NODE_NONE` for names) | = | §3.3.4 | O13a fixture with a name-based `-connect` and a seed-learned address |
| new-5 | `getaddr` and `sendaddrv2` to feelers before the disconnect | = | §3.3.5 steps 11, 16, 20 | O13a feeler transcript |
| new-6 | `getaddr` reply on the **next** pass (no Poisson delay) for an inbound peer's first `getaddr` | = | §3.6.4 | FT `p2p_addr_relay` (P2b) |
| new-7 | a malformed `headers`/`headers2` still clears the 2-minute `getheaders` throttle | = | §3.2.3 pre-read effects | `diff_p2p_headers2` sequence case |
| new-8 | time-offset sampling from outbound peers only; the 200-sample freeze | = (consumed by consensus only under `V23_1`) | §3.3.8 | unit tests; `getpeerinfo.timeoffset` DIFF-RPC (RP1a) |
| new-9 | `qsendrecsigs` is never sent by a plain node; masternodes and `-watchquorums` nodes send it only to a verified quorum-relay member, right after its `mnauth` verifies or when the relay set changes | = | §2.6 hook (P7/AM1 fill it) | O13c |

**Unprompted traffic of the M2 node** (everything it sends that is not a direct reply), for the record O13a/O13b compare:

| Message | When | Timing |
|---|---|---|
| `version` | outbound connect | at once (queued before the node joins the loop) |
| `sendaddrv2`, [`sendtxrcncl`], `verack`, `getaddr` | the peer's `version` | same pass |
| `sendheaders2`/`sendheaders`, `sendcmpct`, `senddsq` | the peer's `verack` | same pass |
| `ping` | first pass after `verack`; then 2 min after each send while none is outstanding; RPC `ping` | pass cadence |
| `getheaders2`/`getheaders` | initial sync (one peer in IBD unless the best header is recent), "more" after a full batch, unconnecting headers, block inv, `ConsiderEviction` | ≥ 2 min apart per peer unless a `headers` reply clears the throttle |
| `addr`/`addrv2` | relayed addresses (to 1–2 peers per address); self-advertisement (not in IBD, so never at M2); `getaddr` replies (inbound, P2b) | Exp(30 s) flush per peer |
| `inv`/`headers`/`headers2`/`cmpctblock` | new tip (never at M2: no blocks connected) | next pass |

Replies the M2 node sends: `pong`, `addr`/`addrv2` (to inbound `getaddr`, with P2b), `headers`/`headers2` (P2b), and Core's `notfound` for a `getdata` it cannot serve (§3.11).

Behaviours 8, 9, 10, 16–20, 22–24, 28–31 and +A, +B, +D, +E belong to other tasks (SP0/SP1, P1, P1, P5, P4, P4, P4, P4, P6b, P5a, P3, P7/AM1, P6a, P6a, P6a; P5, P6b, P4/P5, every profile row); §2.6 lists the hooks they use, and §3.11 the M2 node's interim behaviour for their commands.

### 3.13 The O17 P2P rows

O17 has 50 rows of class P2P (43 `profile`, 3 `gated`, 4 `v24-all`; regenerated with `merge.py --decisions` into `/work/scratch/p2a-outbound-opus/rows.json`). P2a's relation to each:

| P2a implements (reads the field) | Rows |
|---|---|
| `Identity.{protocol_version, ua_version, client_version}` | P2P-01 |
| send-side version gates through the common version | P2P-03 (the gates P2a's messages have: `sendaddrv2`, headers2 limit; later tasks inherit the mechanism) |
| `P2pRules.notfound_limit` (the size check) | P2P-10 (P5 owns the per-entry handling) |
| `P2pRules.msg_asset_unlock` in `IsKnownType` for `inv`/`getdata`/`notfound` | P2P-04 (**P2a is a reader**; O17 lists P5, P6a, BS7, O13b, BS10, MP1a) |
| `UngatedRules.time_source` for sync start, headers timeout, `CanDirectFetch` | P2P-16 (**P2a is a reader**; O17 lists P2b, P3, ND1) |
| `InitRules.bind_failure_fatal` (provided as a ConnMan option) | P2P-33 (O17: P2a provides) |
| carries `Identity.min_mn_proto_version`, records `version_services`/`version_relay_txs` | P2P-02, DKG-01 (read by DK1/LQ4a) |

| P2a provides the seam, another task implements | Rows (owner) |
|---|---|
| `ObjectRequests` (one tracker per `P2pRules.object_download`) | P2P-05, -06, -07, -08, -09, -11 (P5, SP1, GV1b, LK1b) |
| `OrphanStep` | P2P-13 (P5) |
| `TxRelay` (the `PushInv` gate) | P2P-15 (P5) |
| `NetHandler` registry, `PeerConsumeGetDataResponse` | P2P-12, P2P-14, P2P-20, P2P-21, SIG-14/-15/-18, DKG-03/-05/-09..-13, MNO-03/-06/-07/-11, INTG-01 (P5, P5a, SP0, LK1a/b, GV1b, DK1, QM1b, P6a/b, CJ1, SG1a) |
| `Serving` (P4) | P2P-17 (P4) |
| `NetPermissionFlags`, verified-MN fields | P2P-18 (P7, AM1) |

The remaining P2P-class rows are validity, mempool and wallet behaviour that peers see only through other tasks' results: CONS-25, EVO-15, MPIX-01, -02, -12…-16 and WAL-31 (CH3b, EV1, MP1a/b, WL); the gated MPIX-17, -18 and WAL-38; and the `v24-all` CONS-26, DKG-04, SIG-11 and MPIX-03.

No other P2P row touches P2a's code. Two v24-only behaviours in P2a's area have no O17 row: the ASMap health check (logging plus RNG side effects, `-asmap` only; §3.15 proposes a row) and v24's `FinalizeNode` cleanup of a discouraged peer's pending recovered sigs, which is internal (no wire effect) and is SG1a's to reproduce behind the `finalize` hook.

### 3.14 Quirks (proposed P2P register entries)

R4's register (1–34, A1's 35–38, CS1's 39, CS2's 40) is consensus-only. P2a proposes these P2P quirks, provisionally numbered PQ-n until the manager assigns register numbers (§8 Q7). Each gets a named test (`quirk_pq_N_*`) and is replicated, not fixed.

| PQ | Quirk | Core |
|---|---|---|
| 1 | A `version` whose optional tail fails to decode has already applied `SetServices`, the services check and the obsolete-version check, but leaves the peer pre-version, so it may resend | §3.3.5 |
| 2 | Any `headers`/`headers2`, including a malformed one, clears the 2-minute `getheaders` throttle (unless blocks are loading) | `v23:5241` / `v24:5318` |
| 3 | `headers2` reply mode does not record the first header's version; announcement mode does; the decoder always does | §3.4.9 |
| 4 | The `headers2` oversize log says `headers message size` | `v23:5248` / `v24:5325` |
| 5 | The quorum-priority receive queue reorders a peer's messages (100:1) | §3.1.3 |
| 6 | The time-offset median freezes once 200 samples are held (an even count), and no source beyond the 200th is ever counted | `timedata.cpp:40, 50, 76` (both pins) |
| 7 | `GetAddr` filters by `GetNetClass()` while `Select` and the counts use `GetNetwork()` (6to4/Teredo are IPv4 in one, IPv6 in the other) | §3.7.2 |
| 8 | The self-announcement exemption from the 2 h penalty compares IPs only, ignoring the port, while addrman identity includes the port | `addrman.cpp:564-566/568-570` |
| 9 | A token from the addr bucket is spent before the service, ban and reachability filters; `getaddr`'s +1000 can push it above the 1000 cap | §3.6.3 |
| 10 | `ConsiderEviction` records `sent_getheaders` and extends the timeout even when the throttle suppressed the `getheaders` | §3.9.1 |
| 11 | Extra block-relay eviction uses `≥ 30 s`, extra full-relay `> 30 s`; the youngest/second-youngest scan does not update the second when an id falls between them | §3.9.2 |
| 12 | Regtest and devnet skip outbound accounting (`AllowMultipleAddressesFromGroup`): no feelers, no block-relay connections except anchors | §3.8.2 step 5 |
| 13 | Anchors are matched by `IsBlockRelayOnly()`, which includes any peer under `-blocksonly` without `relay` | §3.8.3 |
| 14 | `getaddr` goes to feelers (queued before their disconnect) | §3.3.5 |
| 15 | `pong` timing uses the socket read's timestamp, so queueing delay is excluded from `pingtime` | §3.5 |
| 16 | Discouragement disconnects every peer on the IP, whatever the port | §3.10.1 |
| 17 | A `peers.dat` that fails Core's consistency check is silently replaced by an empty addrman (Dash), while other read errors are fatal | §4.1 |

### 3.15 Errata to A1, the PLAN and O17

| Source | Says | Core at both pins | Proposed fix |
|---|---|---|---|
| A1 §2.14, §4.10 #5; PRD §4.2 #5 | `qwatch` after `ping` and `getheaders` | `qwatch` is pushed in the `verack` handler right after `senddsq` (`v23:4193-4201` / `v24:4224-4232`) | reorder |
| A1 §2.14 "Order after the peer's version"; PRD #4; R4 L6 | `getaddr` "on outbound full-relay connections only" | every non-inbound connection except block-relay-only, feelers included (`SetupAddressRelay`) | reword |
| A1 §2.14 reader bullet | messages queued "in arrival order" | two queues, quorum-priority first at 100:1 (§3.1.3) | add |
| A1 §2.14 `NetHandler` | `on_send_tick`, `on_connected`, `on_disconnected` | do not exist; overlays push from their own tasks (§3.2.6) | drop; adopt §2.4's trait |
| A1 §2.14 "Later", §4.10 #8; PRD #8 | `getsporks` 0.5–2 s after the handshake; `govsync` ≈ 8 s | `SyncManager` tick only: U(0, 6] s and U(6, 12] s; none once synced | correct (SP1's design) |
| A1 §2.14 seeds | "3 at a time" | true, but each network has exactly one seed; the prefix is always `x1.` | note |
| A1 §2.14 "Global helpers run as small actors: AddrMan, BanMan and ConnMan" | — | AddrMan as a synchronous mutex (§2.2, §8 Q3) | amend |
| A1 §2.10.2 | `ChainView` published at most every 100 ms in IBD | the actor needs the best header and tip facts unthrottled (§3.4.6) | add `TipFacts` |
| A1 §2.18, §2.2 | `NodeClock` in `dashd-node` | `dashd-chainstate` and `dashd-p2p` need it and cannot depend on `dashd-node` | §8 Q1 |
| O17 P2P-04 consumers | P5, P6a, BS7, O13b, BS10, MP1a | the `inv` handler's `IsKnownType` is P2a's | add P2a |
| O17 P2P-16 consumers | P2b, P3, ND1 | sync start and the headers timeout are P2a's SendMessages sections | add P2a |
| O17 (none) | — | v24's ASMap health check: two unfiltered `GetAddr` passes (RNG and `vRandom` side effects) and a log line asserted by v24's `feature_asmap.py`, `-asmap` only | add a row (keying `profile`, a flag in `P2pRules` or `InitRules`; §8 Q9) |
| O17 (none) | #7283 classified "not a delta" | linger, sweep cadence and MN throttle use the system clock at v23, the steady clock at v24; they differ only across wall-clock steps | keep "not a delta"; rdashd uses the steady clock under both profiles (declared in this design) |
| PLAN P2a acceptance "behaviours 1–7" | behaviour 7 includes "empty headers below minimum chain work" | that reply is `getheaders` serving (P2b) | P2a covers 7's ping and `getheaders` policy; P2b the empty reply |
| PLAN P8 | asmap | P2a needs the interpreter for bucket parity | §8 Q6 |

---

## 4. Persistence, crash consistency and recovery

P2a persists two files. Everything else it holds (peer state, the node table, the address-response cache, the discouragement filter until P4) is in memory and rebuilt at start, as in Core.

### 4.1 `peers.dat`

- **Format: Core's, `V4_MULTIPORT`** (format byte 4, compat byte 36, `nKey`, `nNew`, `nTried`, `1024 ^ 2^30`, the new entries, the tried entries, the 1024 bucket lists of indexes into the new entries, the asmap checksum), in Core's envelope (network magic ‖ payload ‖ SHA256d(magic ‖ payload)), `CAddress` in the disk form with the `0x20035B60` version word (fact sheet §4). FR-P2P-19 allows our own format if Core's is importable; writing Core's own is simpler, makes the two nodes' files interchangeable (a user can move a datadir's `peers.dat` either way), and lets the parity test exchange files (§6.3). DEC-11 is about chain state; §8 Q2 asks the manager to confirm.
- **Entry order.** Core writes entries in `std::unordered_map` iteration order, which depends on the C++ library, so byte equality with Core is neither possible nor needed; rdashd writes them in ascending id order. Files are compared semantically (§6.3).
- **Write**: every 15 min (`DumpAddresses`) and at shutdown: write `peers.dat.<4 random hex digits>`, `fsync`, rename over `peers.dat`, then `fsync` the directory (Core skips the last step; adding it is unobservable and closes the "renamed but not durable" window of A1 §3.5). A crash leaves the old or the new file, never a mix.
- **Read** (`LoadAddrman`, `addrdb.cpp:185-224/194-231`): missing → empty addrman, dump it; `CheckAddrman` failure (duplicates, zero key, any of Core's 21 codes) → **silently** replace with an empty addrman and dump it (Dash-specific; PQ-17); a compat version above ours → rename to `peers.dat.bak`, start empty (fatal if the rename fails); any other error (bad magic "Invalid network magic number", checksum mismatch, truncation, out-of-range counts, a bad `CAddress` version word, a bad BIP155 length) → InitError `"Invalid or corrupt peers.dat (%s). If you believe this is a bug, please report it to %s. As a workaround, you can move the file (%s) out of the way (rename, move, or delete) to have a new one created on the next start."` (`addrdb.cpp:220/227`; `feature_addrman.py` checks it with the bug-report URL taken from the test config, so O11 sets that to rdashd's). Bytes after the checksum are ignored. Loading follows Core's restore rule: bucket membership is restored only when the file has 1024 buckets and its asmap checksum equals ours, otherwise every new entry is re-bucketed by its stored source; tried entries whose slot is taken are dropped; entries left with no reference are deleted. `-checkaddrman` applies after load.

### 4.2 `anchors.dat`

Written only on a clean shutdown when addrman outbound is in use: up to 2 addresses of the current block-relay-only connections, `CompactSize(n) ‖ n × CAddress (disk, v2)` in the same envelope. Read once at start and **deleted immediately after reading** (any error yields an empty list). A crash therefore loses the anchors, as in Core: anchors exist to survive restarts, not crashes.

### 4.3 Import and downgrade

Because both files are Core's formats, IM1a's `peers.dat`/`anchors.dat` import is a copy. A datadir switched back to dashd reads rdashd's files unchanged.

## 5. Performance

### 5.1 Metrics P2a affects

| Metric (A1 §3.9) | node-bench scenario · metric | P2a's share | Budget | Route to 2× |
|---|---|---|---|---|
| IBD to tip, header part | `ibd-*-seed` · `header_sync_s` (gated), `ibd-*-net` · `header_sync_s` (information) | all of the per-message path except header acceptance (CH1) | ≤ dashd 23.1.8 (mainnet seed) and 24.0.x (testnet seed) in the window; PF0 counters ≤ dashd per header | §5.2: one X11 per header off the actor; zero-copy payloads; batch handoff to CH1 |
| Startup | `startup-*` · `startup_s` | `peers.dat`/`anchors.dat` load, ConnMan start | ≤ dashd | `peers.dat` decode into pre-sized tables; parallel address parsing is not needed at ≤ 65,536 + 16,384 entries (Core's caps) |
| Idle memory | `idle_rss_mib` | AddrMan (≤ 81,920 entries), node table, per-peer state | ≤ dashd | AddrMan entries in a slab (`Vec<AddrInfo>` indexed by id, as Core's `mapInfo` but contiguous); bucket tables as flat `[i32; 1024×64]` and `[i32; 256×64]` arrays (Core's layout) |
| CPU at 125 peers (P4's gate) | counters + window | the loop pass, `send_messages` on idle peers | the loop pass ≤ dashd's instructions per idle pass | a `send_messages` fast path that exits after sections 1–5 when nothing is queued and no timer is due (same observable output; Core walks every section) |
| RPC `getpeerinfo` | `rpc-*` · `rpc_getpeerinfo_ms` | stats snapshots | ≤ dashd | lock-free `PeerStats` snapshots, refreshed by the actor when they change |

`header_sync_s` on the `-net` scenarios is network-bound (round trips to public peers at Core's one-`getheaders`-at-a-time cadence, which the fingerprint keeps), so parity is the honest expectation there; the 2× target applies to `-seed`.

### 5.2 The header path

Core processes every `headers2` message on `msghand` as: read the compressed headers, recomputing `X11(previous)` for each compressed previous hash (`block.cpp:122`); `CheckHeadersAreContinuous` hashes each header again (`v24:3167-3177`); `ProcessNewBlockHeaders` hashes each a third time for `AcceptBlockHeader` (`v24:validation.cpp:4736`); `LookupBlockIndex(headers.back().GetHash())` once more per message. `headers` messages skip the first. At ~30 µs per X11 of 80 bytes (B1 `Pow_X11_0080b`), that is ~90 µs of hashing per `headers2` header on the single message thread, ~180 ms per 2000-header message.

rdashd:
1. **Speculative decode and hash in the reader.** When P1's reader completes a `headers`/`headers2` frame it decodes it with the peer's common version (known once `version` was processed; until then nothing is speculated) and computes each header's X11 once, then attaches `Speculative::Headers { headers, hashes, error }` to the `RawMsg`. It is a pure function of the payload bytes and the common version, never fails the connection, never scores and never reorders anything (A1 §2.14 allows exactly this). The actor uses it only at the read point of §3.2.3, after the gates and the pre-read effects; a speculative decode error is raised there as the same `Throw`. A `headers2` chain is inherently sequential (each header's previous hash is the X11 of the one before), so its hashing runs on the reader task, off the actor; a `headers` message's hashes are independent and are split across the `prevalidate` pool when the message has more than 256 headers.
2. **One hash per header end to end.** `CheckHeadersAreContinuous`, the "received new header" lookup and CH1's acceptance all take the precomputed hashes (`HeaderBatch.hashes`). CH1 checks `hashes[i] == X11(headers[i])` only under `debug_assertions`, as Core's `ASSERT_IF_DEBUG` (`v24:validation.cpp:4602`).
3. **Zero-copy payloads.** `RawMsg.payload` is a `Bytes` slice of the reader's buffer; base-sdk decodes headers in place into a pre-sized `Vec<BlockHeader>`.
4. **No pipelining.** The next `getheaders` still leaves only after `process_headers` returns, exactly where Core sends it (§3.4.3 step 10). Sending it earlier would be cheaper but is a timing fingerprint, and in IBD Core syncs headers from one peer at a time.

Expected effect on the `-seed` scenarios: per `headers2` header, the critical path loses two of three X11 computations and the remaining one moves off the actor. With CH1's acceptance (DGW over the last 24 headers, a hash-table insert, no per-header lock) at a few µs, rdashd's per-header critical path is one X11 plus acceptance against Core's three X11 plus acceptance, which is the 2× (and more for `headers2`) on CPU-bound header sync. The X11 backend is whichever of the C `rs-x11-hash` and `dash-pow` BS15 has made the default; P2a calls it through `dash-pow`'s `x11` entry point and inherits the faster one.

### 5.3 Other hot paths

| Path | Data structure / technique | Why |
|---|---|---|
| Actor pass | the snapshot is a reused `Vec<Arc<NodeHandle>>` (no allocation per pass); the shuffle is Fisher–Yates over a ChaCha20 stream | Core copies and ref-counts `m_nodes` every pass |
| Inbox | two `VecDeque<RawMsg>` and two byte counters under one `parking_lot::Mutex`, classification by a perfect-hash of the 8 quorum commands | Core's two `std::list`s |
| Outbox | per-node SPSC queue to P1's writer plus an atomic byte counter for `fPauseSend` | lock-free handoff; `PushMessage`'s accounting stays synchronous |
| AddrMan | slab of `AddrInfo` by id, `hashbrown::HashMap<ServiceKey, i32>` for `mapAddr` (never iterated, so its order cannot leak), flat bucket arrays, `BTreeSet<i32>` for tried collisions (Core's ordered `std::set`) | Core's semantics, contiguous memory |
| Bucket math | SHA-256d through the SH1 backend (DEC-28); the 18- or 34-byte key and the group bytes are hashed from stack buffers | no allocation per hash |
| addr relay | `RollingBloom` with Core's parameters (5000, 0.001) per peer, the per-network relay destinations from a SipHash randomizer | Core's |

### 5.4 Benches and counters (PF0)

| Bench / workload | Kind | Core counterpart | Gate |
|---|---|---|---|
| `addrman/add`, `addrman/select`, `addrman/select_almost_empty`, `addrman/select_by_network`, `addrman/getaddr`, `addrman/add_then_good` | criterion, same inputs as Core's benches | `bench/addrman.cpp` `AddrManAdd`, `AddrManSelect`, `AddrManSelectFromAlmostEmpty`, `AddrManSelectByNetwork`, `AddrManGetAddr`, `AddrManAddThenGood` (`v24:170-175`, present at both pins) | instructions ≤ Core's (milestone record); no regression per PR |
| `p2p/headers2_decode_hash_2000`, `p2p/headers_decode_hash_2000` | criterion | a DEC-16 local Core bench driving `ProcessMessage(HEADERS[2])` through the TestingSetup harness that `bench/net_processing.cpp` (`v24:1-56`) already uses | instructions per header ≤ Core's / 2 (the 2× claim, recorded) |
| `p2p/actor_idle_pass_125` | criterion over `sim` with 125 idle peers | none (Core has no equivalent; recorded for P4's CPU gate) | no regression per PR |
| `p2p/version_handshake` | criterion over `sim` | — | no regression |
| `header-sync-regtest` | PF0 workload: rdashd syncs 200,000 headers from a local dashd seed (both pins) with `-connect`, measured until `getblockchaininfo.headers` reaches the target | dashd syncing from the same seed | instructions ≤ dashd's; wall clock in the window |
| `ibd-*-seed` · `header_sync_s` | node-bench window | dashd 23.1.8 / 24.0.x | ≤ dashd (REL-1.3 path) |

---

## 6. Verification

P2a has no consensus code, so the layers are L0 (Core's unit vectors), L1 (differential against Core in-process), the L6 fingerprint probes (O13a now, O13b later), functional tests (L4) and live interop. Every test runs under **both** profiles unless it says otherwise: `V24_0` against v24.0.0-rc.3, and `V23_1` (regtest with `-releaseprofile=23.1`) against v23.1.8.

### 6.1 O13a: handshake transcripts per profile (M2 exit 2)

- **Harness.** Core's functional framework (`P2PInterface`, `v2_p2p.py`) drives both binaries. Outbound transcripts use `add_outbound_p2p_connection`, which calls the regtest `addconnection` RPC (`rpc/net.cpp:368/380`) with a connection type and v2 flag; the framework listens and records. P2a provides the back end (`ConnMan::open_one_try(addr, conn_type, v2)`, Core's `AddConnection` semantics: per-type limits counting unfinished connections, a try-grant); the regtest-only RPC is wired by RP0/RP1a at M2 (§8 Q8).
- **Matrix.** {v1, v2} × {outbound full-relay, block-relay-only, feeler, addr-fetch, manual (`addnode onetry`)} × {peer at 70240, 70242, 70235, 70222 (below `sendaddrv2`), with and without bit 11, with and without bit 12} × {`V23_1` vs v23.1.8, `V24_0` vs v24.0.0-rc.3}, plus devnet (name match and mismatch) and the masternode-flag variants once AM1 exists (excluded at M2, as the PLAN says).
- **Normalisation.** Nonces, challenges, ping nonces, v2 garbage and keys, and `version.time` (both nodes run with the same `-mocktime`); everything else is compared byte for byte, including message order, `version` length and `addr_recv` services.
- **Window.** From the TCP connect to the first `getheaders`/`getheaders2`, plus 10 s of silence afterwards with the mock clock frozen (so the sync tick, which needs mock time to advance 6 s, cannot fire under either binary). `getsporks`, `govsync` and `mempool` are SP1's and enter O13b's window when SP1 lands.
- **State parity.** Both nodes load the same regtest chain (the `regtest-40000` fixture or a fresh `generate`), so `start_height` and the sync-start decisions match.
- **CI form.** The dashd transcripts are captured once per pin and checked in as fixtures; CI replays them against `sim` (no dashd needed) on every P2P-touching PR, and the live dashd comparison runs nightly.

### 6.2 Handler-level `diff_p2p_*` (A1 §2.14; L1)

The per-command outcome includes state, so codec equality is not enough. P2a needs a **stateful in-process oracle** (new entry points; §8 Q4 proposes task O1c):

```c
// libdash_oracle, both pins; built from Core's TestingSetup + ConnmanTestMsg + PeerManager, exactly as
// v24 bench/net_processing.cpp and test/fuzz/process_message{,s}.cpp already do.
oracle_peer_session* oracle_peer_session_new(int pin, const char* chain, uint64_t our_services, int conn_type,
                                             int64_t mocktime, const uint8_t* chain_fixture, size_t len);
int  oracle_peer_session_deliver(oracle_peer_session*, const char* cmd, const uint8_t* payload, size_t len);
int  oracle_peer_session_send_pass(oracle_peer_session*, int64_t mocktime);
int  oracle_peer_session_outcome(oracle_peer_session*, uint8_t* out, size_t* len);
     // serialized: messages pushed (cmd + bytes, in order), Misbehaving deltas and messages (captured from a
     // log callback), fDisconnect, should_discourage, addrman calls (SetServices/Good/Add with arguments),
     // AddTimeData samples, and a snapshot of the peer fields this design names (§3.4.1, §3.6.1)
void oracle_peer_session_free(oracle_peer_session*);
```

The Rust side runs the identical script against `PeerLogic` in `sim` with a `ChainAccess` built from the same chain fixture (CH1's arena once it merges; a fixture-backed fake before that) and compares the outcomes.

| Target | Inputs | Seeds |
|---|---|---|
| `diff_p2p_version`, `_verack`, `_sendheaders`, `_sendheaders2`, `_sendcmpct`, `_sendaddrv2`, `_sendtxrcncl`, `_senddsq`, `_qsendrecsigs` | payloads + connection type + delivery before/after `version`/`verack` | BS4a's `decode_domain.json5` (`version` cut at every field, bools 02/ff, subver 256/257, negative time and version, broken tails) |
| `diff_p2p_ping`, `_pong` | payloads, outstanding/none/mismatched nonce, mocktime jumps past 20 min | `p2p_ping.py`'s cases |
| `diff_p2p_addr`, `_addrv2`, `_getaddr` | address vectors (v1 and BIP155, every network id, oversized), relay state, token bucket state | `p2p_addr_relay.py`/`p2p_addrv2_relay.py` payloads |
| `diff_p2p_inv`, `_getdata`, `_notfound` | inventories of every type incl. 34, sizes around 116, 50,000, 50,001; tx-rejecting connections | per profile |
| `diff_p2p_headers`, `_headers2` | chains extending the fixture, unconnecting, discontinuous, invalid (bad PoW, time-too-new, checkpoint fork), counts at 2000/2001/8000/8001 at common versions 70234/70235 | Core captures (`-capturemessages`), dde-probe's headers cases |
| `diff_p2p_unknown` | random command strings, listed-but-unhandled commands | — |
| `diff_p2p_m2_stubs` | every command §3.11 marks exact via a stub or fallback, well-formed and malformed, on full-relay and block-relay-only connections | §3.11 rows |
| `diff_p2p_sequence` | random message sequences (pre-version, pre-verack, duplicate `version`/`verack`, renegotiation attempts, send passes with mocktime steps) | O13b extends it with tip changes across V19 |

Budget: each target ≥ 24 CPU-h clean before P2a merges (A1 §4.13 P2P gate). Differences are triaged through the O3 runner (DEC-30 N5).

### 6.3 AddrMan parity (`diff_addrman`, bucket math)

- **Oracle entries** (also O1c): `oracle_addrman_new(pin, deterministic, asmap)`, `oracle_addrman_op(…)` for `Add` (vector, source, penalty), `Good`, `Attempt`, `Connected`, `SetServices`, `ResolveCollisions`, `Select(new_only, network)`, `SelectTriedCollision`, `GetAddr(max, pct, network, filtered)`, `Size(network, in_new)`, with a mock time per call; and `oracle_addrman_canonical(…)`: every new and tried cell's occupant key, every entry's fields (`nTime`, services, source, last try/success/count-attempt, attempts, refcount, `fInTried`, random position), `vRandom`, the collision set, the network counts, and `nKey`. v24 has `GetEntries` for the cells; for v23 the shim compiles the same walker against `AddrManImpl` as a DEC-16 local test patch.
- **Differential.** Both start in deterministic mode (`nKey = uint256{1}`, the zero-key ChaCha20 stream); a fuzzer generates operation streams over a small address universe (so collisions, multiport duplicates, terrible entries and 8-reference entries happen); after every operation the outputs (selected address, returned vectors) and the canonical state must be equal. Because the RNG streams are identical, this checks every draw, so `Select`'s probabilities, `GetAddr`'s shuffle and the 2^−refcount rule are compared exactly, not statistically. ≥ 24 CPU-h, plus the same with `-checkaddrman` ratios 1 and 3 (which change the draw stream) and with `test/data/asmap.raw`.
- **Serialization.** Each side serializes; each side loads the other's file; canonical states must be equal (the entry order in the file is the only allowed difference, §4.1). Corrupted files (truncation at every offset, bad magic, checksum, counts, version words, BIP155 lengths, duplicate entries) must produce the same outcome class (fatal message / silent reset / `.bak`).
- **L0.** Core's `addrman_tests.cpp` (22 cases at v23.1.8, 23 at v24 — the extra one is `getaddr_unfiltered`; transcribed; their known answers include tried bucket 40 and new bucket 786), `netbase_tests.cpp` and the netgroup/asmap cases, Core's fuzz targets `addrman` and `asmap` ported as non-differential fuzzers.

### 6.4 Other L0 vectors

Transcribed from both pins: `denialofservice_tests.cpp` (`outbound_slow_chain_eviction`, `stale_tip_peer_management`, `block_relay_only_eviction`, `peer_discouragement`), the `net_tests.cpp` cases on local addresses, `GetLocalAddrForPeer` and `version` construction, `net_peer_connection_tests.cpp` (`AddConnection` limits; present at both pins), the ChaCha20/`FastRandomContext` vectors (`random_tests`, `crypto_tests`), and the fixed-seed table equality (§3.8.6).

### 6.5 Functional tests (L4, through O11's runner, both pins)

| Test | Covers | With |
|---|---|---|
| `p2p_add_connections` | slot types, feeler behaviour | — |
| `feature_anchors` | anchors write/read/delete | — |
| `feature_addrman` | `peers.dat` load outcomes | — |
| `p2p_addr_relay`, `p2p_addrv2_relay` | §3.6 | P2b (inbound halves) |
| `p2p_addrfetch`, `p2p_dns_seeds` | ADDR_FETCH, DNS timing | — |
| `p2p_ping`, `p2p_timeouts`, `p2p_leak` | §3.5, gates, inactivity | P1 (transport), P2b (inbound) |
| `p2p_initial_headers_sync`, `p2p_dos_header_tree`, `p2p_sendheaders_compressed` (headers part) | §3.4 | CH1, P2b |
| `p2p_invalid_messages` (P2a's cases: redundant `version`, oversized `inv`/`getdata`/`headers`, non-continuous headers) | §3.2, §3.10 | P1 (framing cases) |
| `p2p_blocksonly` (block-relay-only part) | gate 7, §3.4.7 | P5 (tx part) |
| `p2p_connect_to_devnet` | devnet UA and mismatch | — |
| `rpc_net` (P2a's back ends) | `getpeerinfo` fields, `addnode`, `disconnectnode`, `setnetworkactive`, `getnodeaddresses`, `addpeeraddress` | RP1a |

`feature_minchainwork`, `p2p_sendheaders` and `p2p_compactblocks` need block download and join with P3.

### 6.6 `sim` regression suite (every PR, no dashd)

Golden transcripts per connection type and profile (from §6.1's fixtures); the §3.12 "new" behaviours (priority queue overtaking, feeler `getaddr`, `headers2` timestamp clearing, `addr_recv` services); timer distributions (ping cadence, feeler and extra-peer Poisson means, DNS seed delays) checked with a KS test against the analytic distribution, which O13b later repeats against dashd; stall-free behaviour under 125 simulated peers with backpressure.

### 6.7 Live

M2 exit 1: headers to the tip on mainnet (`V23_1`) and testnet (`V24_0`) from public peers and from the oracle over localhost, v1 and v2; `getchaintips`/`chainwork` equal the oracle's (CH1's check). A 24 h soak with 125-connection limits and the default outbound behaviour, comparing connection-type counts, feeler and extra-peer rates and DNS-seed timing with a dashd of the same pin on the same host and network.

### 6.8 Fuzz (non-differential)

`p2a_process_message` (any message in any state through `PeerLogic`: no panic, no unbounded growth, invariants on the global counters of §3.4.1 after `finalize_node`), `p2a_peers_dat` (no panic on any file), `p2a_inbox` (the two-queue poller against a model), all through the O3 runner.

---

## 7. Implementation plan

Seven phases, each one or two PR-sized reviews. Dependencies: P1 (transport) before phase D's live tests; CH1's interface (`ChainAccess`, `ChainCommand::ProcessHeaders`, `TipFacts`) before phase E merges (PLAN: `i:CH1`); O1c's entry points before the differential gates of phases B and C (§8 Q4). Phases B and C can run in parallel after A; F can run in parallel with E.

| Phase | Content | Acceptance (all under both profiles) |
|---|---|---|
| **A. Foundations** | crate `dashd-p2p` (MIT, lints, `ARCHITECTURE.md`, CI group `node-runtime`); `release.rs` with every `Identity`/`P2pRules` field as complete literals and the row-citing doc comments (O17 rules 3–4); `fingerprint.rs` (UA, services, `version` payload); `FastRandom` (Core's ChaCha20 stream semantics) and `GetExponentialRand`; `NetAddr` predicates, `netgroup`, asmap interpreter; the clock seam (§8 Q1) and `timedata` | 137-byte `version` fixtures per profile byte-equal to dashd captures (nonce/challenge masked); `FastRandom` first output `0x903df1a0ade0b876` and Core's `random_tests` vectors; netgroup and asmap vectors (`netbase_tests`, `addrman_tests` asmap cases); `check-release-refs.sh` passes (no `Release::` outside `release.rs`) |
| **B. AddrMan** | the full port (§3.7), `peers.dat`/`anchors.dat` (§4), `-checkaddrman`; benches | `addrman_tests` transcribed and green; `diff_addrman` ≥ 24 CPU-h clean (with ratios 1, 3 and asmap); serialization exchange both ways; `feature_addrman`-equivalent unit cases; PF0 addrman benches recorded against Core's `bench/addrman.cpp` |
| **C. Actor and handshake** | `NodeTable`/`NodeHandle`, `Inbox` (two queues) and outbox accounting; `PeerManager` thread and loop (§3.1); `PeerLogic` with `process_messages`/`send_messages` sections and all seams with M2 defaults; `policy.rs` (74 rows) and the gates; handlers `version`, `verack`, `send*`, `senddsq`, `qsendrecsigs`, `ping`, `pong`, unknown; the §3.11 stubs and `EmptyServing`; misbehaviour framework and `DiscourageOnly`; `NetHandler` registry with the M2 fallbacks; `PeerManagerHandle`; `sim` | `sim` transcripts equal the dashd fixtures for every connection type and profile; `diff_p2p_{version,verack,send*,ping,pong,unknown,m2_stubs,sequence}` ≥ 24 CPU-h; `policy.rs` table tests (74 commands, both sets); `p2a_process_message` and `p2a_inbox` fuzz clean |
| **D. ConnMan** | tasks `opencon`, `addcon`, `dnsseed`, `socket`; `OpenNetworkConnection`/`ConnectNode` (TCP, clearnet); anchors; feelers; extra peers; fixed seeds (generated table); `InitBinds` with `bind_failure_fatal`; inactivity and linger; P1 integration (reader classification, writer coalescing, v1 retry); `addconnection`/`addnode onetry` back ends | FT `p2p_add_connections`, `feature_anchors`, `p2p_dns_seeds`, `p2p_addrfetch`, `p2p_timeouts`, `p2p_leak` (outbound parts); O13a outbound transcripts v1 and v2 against both dashd pins diff-empty; fixed-seed table test |
| **E. Headers sync** | `headers`/`headers2` handlers with pre-read effects; sync start and timeout with `time_source`; unconnecting/continuity; `ConsiderEviction`, `EvictExtraOutboundPeers`, stale tip; block announcements (the `cmpctblock` branch behind P3's seam); `inv` block path with `msg_asset_unlock`; `getdata`/`notfound` size checks; speculative decode + X11 in the reader | FT `p2p_initial_headers_sync`, `p2p_dos_header_tree`, `p2p_invalid_messages` (P2a's cases), `p2p_blocksonly` (block-relay part); `diff_p2p_{headers,headers2,inv,getdata,notfound}` ≥ 24 CPU-h; live: mainnet and testnet headers to the tip from public peers and from the oracle (M2 exit 1, with CH1); PF0 header benches ≤ Core's instructions per header ÷ 2 (recorded) |
| **F. Address relay** | `addr`/`addrv2`/`getaddr`, the token bucket, `RelayAddress`, `PushAddress`, `MaybeSendAddr`, the address-response cache, local addresses and `-externalip` | FT `p2p_addr_relay`, `p2p_addrv2_relay` (outbound halves; inbound halves with P2b); `diff_p2p_{addr,addrv2,getaddr}` ≥ 24 CPU-h |
| **G. Evidence and hand-off** | O13a's full matrix (with P2b's inbound half); `header-sync-regtest` PF0 workload vs dashd; the `header_sync_s` window run; the 24 h soak (§6.7); quirk tests PQ-1…17; `ARCHITECTURE.md` (Core file map, deviations of §3.15) | M2 exit 2 for the outbound half; `header_sync_s` ≤ dashd in the window; counters recorded in STATUS |

Every phase keeps `cargo fmt`, `clippy -D warnings`, the workspace tests and the `node-runtime` CI group green, and ships its PF0 benches with the code that they measure (A1 Appendix E).

---

## 8. Open questions for the manager

| # | Question | Recommendation |
|---|---|---|
| Q1 | A1 puts `NodeClock` (and Core's `timedata`) in `dashd-node`, but `dashd-chainstate` (header `time-too-new` under `AdjustedTime`) and `dashd-p2p` (every timer, the offset samples) need it and cannot depend on `dashd-node`. Where does it live? | A tiny runtime crate `dashd-clock` below `dashd-chainstate`: `NodeClock` (mockable seconds/µs, steady clock), the time-offset filter with Core's quirks (PQ-6), `GetExponentialRand`, and the mock-scheduler hook. CH1 and P2a both need it in M2, so whichever starts first creates it. |
| Q2 | `peers.dat`/`anchors.dat` format: Core's or our own (DEC-11, FR-P2P-19)? | **Core's.** It is the natural port, makes the files interchangeable, gives IM1a nothing to do, and the parity test exchanges files (§4.1, §6.3). |
| Q3 | A1 says AddrMan runs as a small actor. | A synchronous `Mutex<AddrManImpl>`, as Core: every caller acts on the answer immediately, an actor would add reorderings and latency for no benefit. BanMan likewise (P4). ConnMan stays a set of tasks over the shared node table. |
| Q4 | Handler-level `diff_p2p_*` and `diff_addrman` need stateful oracle entries (`oracle_peer_session_*`, `oracle_addrman_*`; §6.2–§6.3) that O1/O1b do not have. | New task **O1c** (S–M, DEC-16 local patch on both pins, built from Core's own `TestingSetup`/`ConnmanTestMsg` harness and `AddrManImpl`; v23 needs a shim-local `GetEntries` walker). Schedule it alongside P2a phase A so phases B and C can gate on it. |
| Q5 | O13a at M2: dashd sends `getsporks` U(0, 6] s after `verack` while unsynced; the M2 node does not (SP1, M5). | Define O13a's window as §6.1 (to the first `getheaders` plus 10 s with the mock clock frozen) and list the M2 interim deviations of §3.11 in STATUS; O13b's window includes the sync chatter once SP1 lands. Do not add a stub `getsporks` sender to P2a: a sender without SP0's spork processing would be a different, longer-lived fingerprint. |
| Q6 | The PLAN puts asmap in P8, but bucket math needs the interpreter for parity. | P2a ports the interpreter (pure, ~60 lines, Core's tests and fuzz target); P8 keeps `-asmap` loading UX, the health check and `feature_asmap`. |
| Q7 | Quirk register numbers for PQ-1…17 (§3.14). | Register them as a P2P block after the consensus quirks (e.g. 41–57) once the reconcile fixes the list, so EV2/LQ1 designs in flight can claim numbers first. |
| Q8 | O13a and several FTs need the regtest-only `addconnection` RPC and `addnode … onetry` at M2. | P2a provides the back ends (phase D); RP0/RP1a wires the RPCs in M2 (they are thin). Confirm the owner. |
| Q9 | v24's ASMap health check (two unfiltered `GetAddr` passes and a log line asserted by v24's `feature_asmap.py`, `-asmap` only) has no O17 row. | Add an O17 row keyed `profile` with a field `P2pRules.asmap_health_check: bool` (`V23_1` false, `V24_0` true), implemented by P8. |
| Q10 | The exact `-honestua` user agent (DEC-04). | `/rdashd:<version>[(<comments>)]/`, comments kept; documented in the release notes; never the default. |
| Q11 | `InitBinds` (binding, `bind_failure_fatal`) is pulled into P2a because ConnMan's start does it; P2b accepts. | Keep it in P2a (phase D); P2b only adds the accept loop and admission. |
| Q12 | Before their owners land (P8, P4), how do the options in §2.7's last row (`-proxy`, `-onion`, `-i2psam`, `-onlynet`, `-discover`, `-upnp`, `-whitelist`, …) behave? | InitError "not supported by rdashd yet" (A1 §2.18's rule for unimplemented options); never silently ignored. |

