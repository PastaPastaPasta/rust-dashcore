# P2a: outbound P2P core (`dashd-p2p`) — reconciled design

| | |
|---|---|
| Status | **Revision 3** (DEC-56 after `reviews/P2a-design-r3-gpt.md`: the `send_messages` fast path is removed; rev2 adopted G1–G7 of `reviews/P2a-design-r2-gpt.md`; rev1 followed `reviews/P2a-design-r1-opus-core.md`/`-arch.md` and DEC-51; dispositions in §10). Reconciled r0 merged `P2a-outbound-fable.md` (880 lines) and `P2a-outbound-opus.md` (1,461 lines); every disagreement was resolved against Dash Core source at both pins, not by preference (§9) |
| Date | 2026-10-08 |
| Task | `dashd-p2p` outbound core: ConnMan (outbound, feelers, anchors, addnode, seeds), handshake + `fingerprint.rs` from the release profile (`Identity`, `P2pRules`), the wake-driven actor loop (shuffle, MN-only throttle), the per-command decode policy and its gates, awaiting observable messages, ping, AddrMan, headers/headers2 sync. Seams for P1 (transport) and P2b (inbound) |
| PRD | FR-P2P-05, -06 (outbound half), -07, -08 (headers part), -16, -19; NFR-FP-01 (behaviours 1–7, 11, 25 outbound, +C), NFR-FP-03 (the timers P2a owns); FR-CHS-02/06 as a consumer |
| Core pins | v23.1.8 = `728f505583` (mainnet profile `V23_1`), v24.0.0-rc.3 = `0364ed626e` (testnet/devnet/regtest profile `V24_0`). Cites are `v23:file:line / v24:file:line`, or `file:line` when identical at both pins; "(=)" means the function body is the same at both pins apart from renames (`Misbehaving(NodeId,…)` → `Misbehaving(Peer&,…)`, `fImporting||fReindex` → `LoadingBlocks()`). Full trees: `/work/scratch/p2a-outbound-opus/{v23,v24}/src`; per-function extracts: `/work/scratch/p2a-fable/<tag>/src` |
| Inputs | A1 §2.14, §2.25, §3.1, §4.10, Appendix A/E; PLAN §4.5 (P1, P2a, P2b, P3, P4, P5, P5a, P6a/b, P7, P8), §4.7 (O13a/b), M2 exit; O17 §4.2 (`Identity`, `P2pRules`), `O17-pin-delta/p2p-init.md` P2P-01…-39; **BS4a `DDE-SWEEP.md` §4 at `node/bs4a-decode-domain` 00aab98c, read as the manager directed after `reviews/BS4a-r2-opus.md`: everything before the read (transport rows, gates, read shapes, decode-failure catching and scoring, trailing bytes) is normative; the "After the read" column is informative only, and this design re-derives every post-read outcome it owns from Core at both pins (§3.2.2, §3.11)**; PRD §4.2; DEC-04, -05, -11, -16, -24 D3, -28, -30, -37, **-45, -48, -51**; memory note `p2a-core-pin-diff` |
| Scratch | `/work/scratch/p2a-rec/` (this reconcile), plus both designers' scratch dirs |

**Changes in rev1** (DEC-51; one line per item, the dispositions are in §10):
1. **No speculative or side-job header hashing** (DEC-51a, core M-2, arch M2). Headers are hashed once, inline on the actor, never earlier than Core hashes them: `headers2` prev hashes where `Uncompress` needs them, everything else only after the connect check, nothing for pre-verack, non-connecting or oversized messages (§3.4.9). The `HeaderBatch { headers, hashes }` contract with CH1 stays.
2. **The full-batch test uses the peer's bit 11** (`GetHeadersLimit(common, UsesCompressedHeaders(peer))`), not the limit of the message type received, for the "more headers" request and `may_have_more_headers` (core M-1, arch m9; PQ-24; §3.4.3 steps 10–11).
3. **`initialize_node` is synchronous on the caller's thread**, so `version` leaves at connect time, as Core's `InitializeNode` does (core M-3; §2.2, §3.8.5).
4. **The +C probe is observable at M2**: a connecting one-header `headers` batch and a block-inv probe, measured statistically under the shuffle (core M-4, arch M7; §3.12, §6.1).
5. **Every §3.11 row cites Core at both pins** (core M-5, DEC-45); the `cmpctblock`, filter, `qfcommit` and `spork` rows are corrected (core m-1, m-2, m-3, n-7).
6. **`diff_addrman` and O1c corrected** (core M-6, arch M8): `vRandom` after a pruning `peers.dat` load is library-order dependent and is compared modulo order; O1c gets multi-peer sessions, a seeded RNG patch, capture/substitute for nonce echoes, one session per process, size M.
7. **Concurrency** (arch M1, M10, m6, m7; DEC-51f, h): the thread wait-for graph and liveness invariants, a non-blocking unbounded outbox, the explicit shutdown order, payloads copied out of read buffers like Core, ConnMan loops on OS threads with sans-io step functions (§2.2.1).
8. **Header-sync performance restated per X11 backend** (arch M3, M4; DEC-51b, c): XH1 is a dependency of the headers phase; the 2× claim holds only with a Core-class backend; PF0 gains X11 and header-acceptance workloads; a headers-only side-by-side node-bench scenario is defined (§5).
9. **The CH1 contract is stated as requirements on CH1** (arch M5; DEC-51d): a concurrent append-only index, lock-free locators over immutable fields, an unthrottled active-chain view, an IBD latch recomputed against mockable time, `best_header` never null (no P2a writes), V checks its queue between ABC steps (§2.5.3).
10. **Overlay `NetHandler`s receive raw payloads** and decode at Core's read point through `HandlerCx::read` (arch M6; DEC-51g; §2.4).
11. **The plan is split into P2a-core and P2a-net** with separate phases and acceptance, merge gates on `sim`/oracle/fuzz evidence only, and the RP0 M2 slice (`getpeerinfo`, `addconnection`, `setmocktime`, `mockscheduler`, `getblockchaininfo`, `getchaintips`, `syncwithvalidationinterfacequeue`) named (arch M9; DEC-51e; §7).
12. **Six cited tests that do not exist at either pin are removed** (core m-6, arch m8); **errata row 21** now records lower header latency as a classifier feature, and **seven errata rows** are added (arch §3.15 rulings; DEC-51j).
13. All remaining minors and nits from both reviews (§10).

**Changes in rev3** (DEC-56; the r3 closure check `reviews/P2a-design-r3-gpt.md` confirmed G1–G4, G6, G7 closed and G5 still open): the `send_messages` fast path of §5.1 is **deleted**. Every `SendMessages` section (1–13, §3.1.4) runs on every pass for every peer, as Core does; the rev2 predicate still suppressed block-download initiation (section 12) and tx-trickle state updates (section 8) on "empty" passes. Any future skip optimisation needs PF0 evidence that the loop pass matters, plus a twin-compared property test, and is its own reviewed change. Edited: the header, §0 item 5 of the rev2 list, §5.1, §7.2 phase E, §10.2 m16, §10.3 G5.

**Changes in rev2** (GPT r2 review, all seven majors adopted; verified at both pins before adoption):
1. **G1 — hash-once keeps Core's prefix short-circuit and `Uncompress`'s exact work.** The continuity check is an incremental loop (compare, return on mismatch, then hash), so a mismatch at header k costs k−1 hashes as in Core; `HeaderBatch.hashes` is complete only after continuity succeeds; the zero-hash claims are corrected: `Uncompress`'s prev hashes happen before the connect check for every `headers2` message, and a ≤ 8 unconnecting batch hashes its last header (§3.4.3 steps 6–7, §3.4.9 rules 1–2); hash-call-count/order witnesses are test gates (§6.2, §7.2).
2. **G2 — the null locator is reachable through `ConsiderEviction`** (`m_work_header` = genesis tip → `pprev` null) and differs per profile: v23 substitutes the active tip (one entry), v24 sends an empty locator. `ChainAccess::locator` takes `Option<BlockId>`, the behaviour is keyed by a new `P2pRules.null_locator` field (O17 row proposed), §3.0 #10 and PQ-23 are reclassified reachable, and a differential session is added (§3.4.4, §3.9.1, §8 Q12).
3. **G3 — send backpressure is Core's retained-memory charge**: `m_send_memusage += GetMemoryUsage()` (object size + `MallocUsage(capacity)`) plus the transport's `GetSendMemoryUsage()`, transferred at handoff, with the pause state recomputed after every write; P1 exposes the transport charge including retained capacity (§2.2.1, §3.8.7, §5.3); boundary tests with stalled and partially draining V1/V2 writers.
4. **G4 — one coherent read snapshot from CH1** (new requirement R8): `ChainAccess::snapshot() -> Arc<ChainReadSnapshot { tip, best_header, active_chain, bls, epoch }>` for every Core critical section; `tip()`/`active_chain()` are gone; g2's `status` read semantics and BLS context are specified (§2.4, §2.5.3).
5. **G5 — the `send_messages` fast path was narrowed** in rev2; **rev3 removes it entirely** (DEC-56, see "Changes in rev3").
6. **G6 — `diff_addrman` after a pruning load uses the alignment option only** (translate ids by address key, install Core's `vRandom`, update `nRandomPos`, verify the RNG state); the multiset alternative is withdrawn; unaligned loads compare canonical invariants plus a separate sampling check; the serialization-exchange rule permits renumbering and the post-load order (§4.1, §6.3).
7. **G7 — phase C's merge gates no longer reach into E/F**: `diff_p2p_multi` is split into a C-owned handshake/gate/queue subset and a joint E/F header/relay target; C's transcripts end before the first E-owned send; M2 live handshake parity uses fresh genesis-only datadirs (the `regtest-40000` fixture stays an in-process `sim`/O1c fixture); the PLAN rows are recorded as pending manager work (§6.1, §7).
8. Minors g1–g4 and the audit notes: the `platformban`, `dsq`/CoinJoin and `getdata` anchors fixed and core M-5 marked mostly resolved; `entry().status` is a seqlocked read and `bls` lives in the snapshot; the peer-map lock joins the hierarchy and the shutdown sequence names interrupt, join and terminal `finalize_node` cleanup; scheduler time is only for scheduled callbacks (addr/feeler/extra-peer deadlines stay on node time) with `setmocktime`-only and `mockscheduler`-only tests; §3.6.4 carries the new-6 qualification; the 383 ns/byte quotation is removed; `ConnChainView` says where the verified-ProTx set comes from.

> **Reading guide.** §0 one page. §1 scope and the M2 node. §2 crate, runtime shape, API and seams (the CH1 contract is §2.5.3). §3 Core equivalence: the loop (§3.1), gates and policy (§3.2), handshake (§3.3), headers sync and the header-hash path (§3.4), ping (§3.5), addr relay (§3.6), AddrMan (§3.7), ConnMan (§3.8), eviction (§3.9), misbehaviour (§3.10), the 74 commands at M2 (§3.11), every observable behaviour per profile (§3.12), O17 rows (§3.13), quirks (§3.14), errata (§3.15). §4 persistence, §5 performance, §6 verification, §7 plan (P2a-core / P2a-net), §8 open questions, **§9 reconciliation log** (one line per disagreement), **§10 dispositions of the r1 and r2 reviews** (one line per finding).

---

## 0. Summary

1. **One actor, Core's loop.** `PeerManager` is one dedicated OS thread running a line-by-line port of `ThreadMessageHandler` → `ProcessMessages` → `ProcessMessage` → `SendMessages` (`v23:net.cpp:3682-3727 / v24:3676-3721`; `v23:net_processing.cpp:5728-5801 / v24:5872-5947`). Each pass shuffles a snapshot of the node table, processes at most one polled message per peer, then sends; masternode connections get `SendMessages` only when one global 100 ms steady-clock throttle has elapsed (and the throttle timestamp resets only when it fires); `fMoreWork |= more && !fPauseSend`; the loop waits ≤ 100 ms or until woken. Messages are polled from **two** per-peer queues, Core's quorum-priority queue and the normal queue, at Core's 100:1 ratio (`v23:net.h:2012-2029 / v24:2053-2070`; `PollMessage` `v23:net.cpp:4766-4810 / v24:4807-4850`). The actor blocks only where `msghand` blocks: on `ChainCommand::ProcessHeaders` for `headers`/`headers2` at M2.
2. **Gates first, decode at Core's read point, post-read outcomes from Core.** `process_message` runs Core's prologue in Core's order (duplicate `version`, pre-`version`, the five negotiation messages, the probe first-message rule, the blocks-only set), then each handler calls base-sdk's decoder exactly where Core's handler reads, so pre-read side effects survive a failing read. `policy.rs` has one row per command (74): the pre-read columns are BS4a §4's normative content; the post-read column is **re-derived from Core at both pins and cited** (§3.11), never copied from DDE-SWEEP §4.
3. **The fingerprint is data.** `Identity` (70240 / 70242; `/Dash Core:23.1.8/` / `/Dash Core:24.0.0/`; 230108 / 240000; MN min 70240 / 70242) and the ten `P2pRules` fields are O17 §4.2's complete literals. `fingerprint.rs` builds the 137-byte default `version`, the local services (0x1C05 / 0x1C45 / pruned), and the fixed post-`version` and post-`verack` orders. Every send-side version gate follows from the negotiated common version.
4. **Headers sync is Core's; headers are hashed once, inline, never earlier than Core hashes them.** Sync-peer selection, the 2-minute `getheaders` spacing, `headers2` (8000 at common ≥ 70235) vs `headers` (2000), unconnecting headers (+20 every 10th, +10 above 8), continuity (+20), the min-chain-work disconnect, outbound protection, the 15 min + 1 ms/header timeout, with the sync clock per `UngatedRules.time_source` (P2P-16). Core hashes every `headers` header twice and every `headers2` header three times on `msghand`; rdashd hashes each header once on the actor, at the points where Core first needs the hash (the `headers2` prev hashes inside the decoder, exactly as `Uncompress`; the last header of a ≤ 8 unconnecting batch; the rest in an incremental continuity loop that stops at the first mismatch, after the connect check), and hands CH1 a `HeaderBatch { headers, hashes }` only once continuity succeeded. On every path a peer can drive without PoW, the number and order of hashes equal Core's (DEC-51a; G1). The win is hash-once against Core's 2–3×; it is real only with a Core-class X11 backend (XH1, DEC-51b), and the shorter batch-to-`getheaders` gap is recorded as an O13b classifier feature (§3.4.9, §5.2).
5. **No `getsporks` stub at M2.** The only sender at either pin is `SyncManager::ProcessTick` (`v23:node/sync_manager.cpp:146-214 / v24:138-214`), whose 6 s gate is on the mockable clock, whose next actions on regtest (`mempool`, `govsync`, the `full-sync` disconnect) cannot be reproduced before SP1, and whose `spork` replies dashd stores and relays. O13a's M2 window ends at the first `getheaders` plus 10 s with mocktime frozen, so the tick cannot fire in either binary (§6.1).
6. **ConnMan and AddrMan are faithful ports; AddrMan is a mutex.** 8 full-relay + 2 block-relay (anchors first) + 1 feeler (Exp 2 min, U[0,1 s) sleep) + addnode (8, own semaphore); the stale-tip extra peer and the extra block-relay peer (Exp 5 min); `-connect`, `-seednode`/ADDR_FETCH, DNS seeds (`x1.<seed>`, ≤ 32, 11 s / 5 min, skipped at 2 handshaken full-outbound peers), fixed seeds at 60 s from `chainparamsseeds.h` with ages U[1 w, 2 w]. **Regtest and devnets skip the whole outbound-accounting block**, so no feelers or block-relay connections open there except anchors (`v23:net.cpp:3034 / v24:3028`, `chainparams.cpp:294,497,689,895`). AddrMan is a `Mutex<AddrManImpl>` as Core's `AddrManImpl::cs` (`addrman_impl.h:155`), with Core's bucket math, `nKey`, asmap interpreter, `FastRandomContext` stream and `peers.dat` V4_MULTIPORT format; parity is a deterministic operation-stream differential (§6.3).
7. **Seams.** P1 delivers `RawMsg`s and takes `OutMsg`s; P2b plugs in at `accept` and reuses the shared `version`/`verack` handler; CH1 answers `ChainCommand::ProcessHeaders` with `HeadersResult` and publishes unthrottled `TipFacts`; P3/P4/P5 and the overlays plug in through `BlockSync`, `BanList`, `ObjectRequests`/`TxRelay`/`OrphanStep` and a Core-shaped `NetHandler` registry. `NodeClock`, `SteadyClock`, the time-offset median filter and `GetExponentialRand` live in a new tiny crate `dashd-clock` below `dashd-chainstate`, owned by P2a-core phase A (§2.7). Overlay handlers receive raw payloads and decode at Core's read point (§2.4). The thread wait-for graph has no cycle: no thread ever waits on the actor, the outbox never blocks, and ND1 interrupts the actor before it stops validation (§2.2.1). The CH1 contract is a list of requirements on CH1 (§2.5.3).
8. **Per profile.** P2a reads `Identity.*`, `P2pRules.msg_asset_unlock`, `P2pRules.notfound_limit`, `P2pRules.null_locator` (new in rev2: the null-index locator `ConsiderEviction` builds on a genesis-only node is the active tip at v23 and empty at v24, §3.4.4), `UngatedRules.time_source`, and consumes `InitRules.bind_failure_fatal`. Everything else in P2a is byte- and timing-identical between the pins (§3.0).
9. **Verification.** O13a-out transcripts per profile and connection type (v1 and v2) against both dashd pins, with a +C probe that is observable at M2 (a connecting one-header batch and a block inv, §3.12); a new oracle task **O1c** (size M) with multi-peer `oracle_peer_session_*` sessions, a seeded-RNG patch and `oracle_addrman_*` entries (§6.2–6.3); handler-level `diff_p2p_<cmd>` for the M2 set; Core's unit vectors (`addrman_tests`, `net_tests`, `netbase_tests`, `denialofservice_tests`, `random_tests`); functional tests (§6.5); header-sync replay windows (§6.6); the `sim` suite on every PR.
10. **Plan and open questions** (§7, §8): the work is split into **P2a-core** (identity, actor, gates, handshake, headers; phases A, C, E) and **P2a-net** (AddrMan, ConnMan, addr relay; phases B, D, F), run in parallel after A (DEC-51e); XH1 gates the headers phase; the open questions are the register numbers, the O13a-out/-in split and the m14 regtest ladder switch.

---

## 1. Scope

### 1.1 The PLAN row

| PLAN P2a deliverable | Where |
|---|---|
| ConnMan: outbound, feelers, anchors, addnode, seeds | §2.4, §3.8, §4 |
| Handshake + `fingerprint.rs` (`Identity`, `P2pRules`) | §2.3, §3.3 |
| Wake-driven actor loop (shuffle, MN-only throttle) | §2.2, §3.1 |
| Per-command decode policy and the gates | §3.2, §3.11 |
| Awaiting observable messages | §3.1.5 |
| Ping | §3.5 |
| AddrMan | §3.7, §4 |
| headers/headers2 sync | §3.4 |
| Acceptance: O13a transcripts per profile; addrman fuzz + bucket-math parity; behaviours 1–7, 11, 25 (outbound), +C; handler-level `diff_p2p_*` for the M2 set; BG `header_sync_s` (window) and header-sync counters | §6, §5 |

### 1.1a The split (DEC-51e)

P2a is two tasks run in parallel after the shared phase A: **P2a-core** (identity, the actor and gates, the handshake, headers sync, eviction, misbehaviour, `getpeerinfo` state stats; phases A, C, E) and **P2a-net** (AddrMan, netgroup, local addresses, `peers.dat`/`anchors.dat`, ConnMan, addr relay, `NodeStats`; phases B, D, F). The crate stays one (`dashd-p2p`); the seams between the halves are `AddrMan`'s API (§3.7.1), `NodeTable`/`NodeHandle` (§2.4) and `ConnChainView` (§2.4). Each half merges on `sim`, oracle and fuzz evidence; functional tests, O13a-out and the live sync are M2 exit items (§7).

### 1.2 In scope

- Crate `dashd-p2p` (A1 §2.2): manifest, MIT with Dash Core's notice (DEC-07), `ARCHITECTURE.md`, `release.rs` with **every** field of `Identity` and `P2pRules` as complete literals for both releases (O17 rule 4).
- `fingerprint.rs`: our `version`, local services, the UA (`-uacomment`, the devnet comment, `-honestua`), `-pushversion`, the fixed post-`version`/post-`verack` sequences.
- The node table (Core's `CNode` net-level fields), connection types, the per-node inbox (two queues, receive flood control) and outbox accounting (send flood control).
- ConnMan, outbound: `opencon`, `addcon`, `dnsseed`, `-connect`, `-seednode`/ADDR_FETCH, anchors, feelers, extra peers, `OpenNetworkConnection`/`ConnectNode` over TCP (clearnet), the v2→v1 reconnect bookkeeping with P1, `DisconnectNodes` (linger, half-close), `InactivityCheck`, `-maxconnections` accounting, `setnetworkactive`, and `InitBinds` (bind/listen with `InitRules.bind_failure_fatal`; accepting is P2b).
- AddrMan (full port), `netgroup` with the asmap interpreter, local-address bookkeeping (`mapLocalHost`, `SeenLocal`, `GetLocalAddrForPeer`), `peers.dat`, `anchors.dat`.
- The time-offset filter (`AddTimeData`) fed from outbound `version` messages (in `dashd-clock`, §2.7).
- `PeerManager`: the actor, `PeerState` (Core's `Peer` ∪ `CNodeState`), `process_messages`, `process_message` (gates, policy, dispatch), `send_messages` in Core's order with the P3/P5 sections as seams.
- Handlers: `version`, `verack`, `sendheaders`, `sendheaders2`, `sendcmpct` (state only), `sendaddrv2`, `sendtxrcncl` (off path), `senddsq`, `qsendrecsigs`, `ping`, `pong`, `addr`, `addrv2`, `getaddr`, `headers`, `headers2`, the block path of `inv`, the size checks of `getdata`/`notfound`, unknown commands, and the M2 stubs that make every other command Core-exact where no later task's state is needed (§3.11).
- Headers sync, block announcements (the `cmpctblock` branch through P3's seam), ping, addr relay, outbound eviction and the stale tip, the misbehaviour framework and the scores of P2a's commands, `getpeerinfo` stats.
- The `NetHandler` registry with per-release dispatch order; `PeerManagerHandle`; the `sim` harness.

### 1.3 Non-goals (and their owners)

| Not P2a | Owner |
|---|---|
| V1/V2 framing, BIP324 handshake, short IDs, garbage, rekey, inbound v1 detection, command-byte validation (BS4a-r2 N-4) | P1 |
| Accepting inbound, inbound transcripts, `getheaders`/`getheaders2` serving (incl. the empty reply below minimum chain work), `getaddr` serving tests | P2b |
| Block download, stalling, compact blocks, direct fetch, `BlockChecked` punishment | P3 |
| Inbound eviction, BanMan persistence, permissions options, the rest of the score table, block/filter/bloom serving, `-maxuploadtarget`, `-capturemessages` | P4 |
| Tx relay, both object-request trackers, trickle, orphans, `mempool` replies | P5 |
| Overlay handlers (`clsig`, `spork`, govobj, CoinJoin, LLMQ), mnsync (`getsporks`, `govsync`, `mempool`, `full-sync`), MN connections, MNAUTH, probes, `qwatch` | P5a, P6a/b, SP0, SP1, P7, AM1, DK1 |
| SOCKS5, Tor, I2P, CJDNS, `-onlynet`, NAT, `-discover`, `-asmap` loading UX, the asmap health check | P8 |
| Header acceptance, the block index arena, `m_best_header`, `NotifyHeaderTip` (incl. the "Synchronizing blockheaders" log, `validation.cpp` v24:4752-4757) | CH1 |
| Option parsing, init order, logging sinks, RPC shapes | ND1, AR1, RP1a |
| The fingerprint lab | O13a/b (P2a ships `sim` transcripts and fixtures) |

### 1.4 What the M2 node is, and what O13a must mask

At M2 rdashd syncs headers only: a non-masternode, wallet-less node in IBD (tip at genesis, headers syncing, masternode sync at BLOCKCHAIN, empty mempool, no sporks). It never requests objects and never sends the mnsync chatter. §3.11 gives, per command, what dashd does in that state and what P2a does; each difference is an **M2 interim deviation** with the task that closes it. O13a's transcript window (§6.1) ends before the first message a later task owns, with mocktime frozen so the sync tick cannot fire in either binary (§3.0 item 8 of the reconciliation; `v23:node/sync_manager.cpp:161 / v24:157`).

---

## 2. Crate, runtime shape and API

### 2.1 Reuse

| Source | Decision | Why |
|---|---|---|
| base-sdk `dash-p2p-core` (BS4a/BS7) | **Use** for every payload type, `P2pMsg::decode_with_policy`, `split_v2`/`decode_v2_payload`, `ProtoCtx`, `CommandString`, `ServiceFlags`, `ProtocolVersion`, the headers2 codec | Rule DDE lives there (A1 §1.4) |
| #264 transport (`Transport`, V1/V2, `V2Session`, short IDs) | **Use through P1** (`dash-p2p-transport`) | A1 §2.14; P2a sees only `RawMsg`/`OutMsg` |
| dash-spv `src/network/*` | **Not reused** | SPV-shaped (MN IP lists as seeds, 2000-entry map store, `relay=false`, `start_height` 0, reputation instead of misbehaviour); A1 §2.1 rule 8 |
| `dash-network-seeds` | **Not used** | a masternode IP list; Core's fixed seeds are `chainparamsseeds.h` |
| dash-spv `test-utils` via `dash-testkit` (O16) | **Use** for live interop against dashd | — |
| `contrib/node/dde-probe` (BS4a) | **Use** as the raw-P2P probe base for `diff_p2p_*` corpus capture | drives both pins already |
| Core's functional framework `P2PInterface` / `v2_p2p.py` | **Use** in O13a | v1 and v2 |

### 2.2 Runtime shape

```
             tokio "net" runtime (A1 §3.1)                                      dedicated OS threads
 ┌──────────────────────────────────────────────────────────────┐        ┌──────────────────────────────────┐
 │ ConnMan socket task (sweep, disconnector)  (+ P2b: accept)      │        │ peerman: PeerManager actor        │
 │   (+ P7: mncon)                                                │        │   loop = ThreadMessageHandler      │
 │ per node: reader task (P1 transport → RawMsg, payload copied   │ wake → │   PeerLogic (sans-io)              │
 │   out of the read buffer) → Inbox (2 queues)                   │───────►│   blocks on ChainCommand replies   │
 │ per node: writer task (P1 transport ← OutMsg, unbounded MPSC)  │◄───────│   pushes OutMsg into Outbox        │
 │ OS threads (Core's): opencon · addcon · dnsseed (sans-io step  │ NodeTbl│   timers: stale-tip check 45 s on  │
 │   functions, thin thread drivers)                              │        │   the scheduler time base          │
 │ scheduler: DumpAddresses 15 min; anchors at shutdown           │        └──────────────────────────────────┘
 └──────────────────────────────────────────────────────────────┘                  │ ChainCommand / TipFacts
          ▲  NodeTable (shared: Core's m_nodes of Arc<NodeHandle>)  ▲                ▼
          └── AddrMan: Mutex<AddrManImpl> (Core's cs) ──────────────┘        validation thread (CH1)
```

- **The node table is shared, as Core's `m_nodes`.** `NodeTable` is an `RwLock<Vec<Arc<NodeHandle>>>` in insertion order (`ForEachNode` order); `NodeHandle` carries Core's `CNode` net-level fields as atomics or small mutexes where Core uses atomics or `cs_*`. ConnMan (slot counts, address choice, disconnects) and PeerManager (protocol flags) read it without message passing, so ConnMan sees the same state `ThreadOpenConnections` sees.
- **AddrMan is a `Mutex<AddrManImpl>` called synchronously** (Core `addrman_impl.h:155` `mutable Mutex cs`; every public method is `LOCK(cs)` + `Check()` both sides, `addrman.cpp` v24:1181-1260). Every Core caller acts on the answer at once: `Select` → `OpenNetworkConnection` (`v24:net.cpp:3155-3291`), `Good` inside the `version` handler (`v24:np:4149`), `Add` inside the `addr` handler (`v24:4474`). Both independent designs chose this (Fable §3.4, Opus §2.2); A1 §2.14's "small actors" line is an erratum (§3.15). BanMan (P4) likewise; ConnMan is a set of tasks over the shared table.
- **The actor is a plain thread with blocking waits.** It parks on a `Condvar` with `fMsgProcWake` semantics (§3.1.1); readers set the flag and notify. Awaiting a `ChainCommand` reply is a blocking `recv`, exactly as `msghand` blocks on `cs_main`. A dropped reply channel means "interrupted" (§2.2.1).
- **`initialize_node` is synchronous on the caller's thread** (core M-3). `OpenNetworkConnection` calls `InitializeNode` on the `opencon` thread, which creates `CNodeState` and `Peer` under `cs_main`/`m_peer_mutex` and pushes `version` at once (`v24:net.cpp:3648` → `np:1745-1760`), microseconds after the TCP handshake. The Rust `PeerManagerHandle::initialize_node` inserts the `PeerState` into a shared peer map under one mutex (Core's `m_peer_map`), reads `best_height` and `our_services` from atomics, pushes `version` for outbound nodes, and only then lets ConnMan append the node to `NodeTable`. It is never a drained command; a drained command would delay `version` by up to one 100 ms pass, which a passive observer can separate from dashd. O13a-out and O13b get a connect→`version` latency feature.
- **Payloads are copied, as Core copies each message into its own `CDataStream`** (arch M10; DEC-51h). The reader copies every completed payload out of the socket read buffer into its own allocation, so a 64 KiB read buffer is never pinned by a 32-byte `ping`, and `fPauseRecv` (counted on raw message size, as Core) bounds memory.
- **ConnMan's loops run on OS threads, as Core's `opencon`/`addcon`/`dnsseed` do** (arch m6, m7). They block on connects, on the outbound semaphore and on the AddrMan mutex (`Select` up to 100 tries, `GetAddr`, `Check()`), which A1 §3.1 forbids on the `net` runtime; `DumpAddresses` serialises under the lock and writes outside it. Each loop is a sans-io step function (`opencon_step(state, view, now, rng) -> Action`, `addcon_step`, `dnsseed_step`) with a thin thread driver, and the actor exposes `MessageLoop::run_one_pass(now)`, so `sim` drives everything from a virtual clock deterministically.
- **Writers do framing and encryption.** The actor serializes the payload (it owns the negotiated version) and pushes `OutMsg { cmd, payload }`; P1's writer frames and, for v2, encrypts in send order. `fPauseSend` accounting is updated synchronously in `push_message`, before the writer runs (`CConnman::PushMessage` `v23:net.cpp:4816-4864 / v24:4857-4905`), and it is **Core's retained-memory charge, not queued wire bytes** (G3; §2.2.1, §3.8.7).
- **Overlays push directly.** Core's overlay threads call `PushMessage` and `PeerManagerInternal` from their own threads; `NodeTable::push_message` is thread-safe for that reason, and ordering between a scheduler push and the actor's own pushes is whichever thread wins, as in Core (`cs_vSend`-serialised, not ordered relative to `SendMessages`). `PeerManagerHandle` methods Core runs under `cs_main` from other threads (`misbehaving`, `relay_inv`, `chain_event`, `finalize_node`, `send_pings`) become commands drained at the top of the next pass (an interleaving Core allows); `initialize_node` and `stats` are the exceptions (synchronous and lock-free respectively).

#### 2.2.1 Liveness: the thread wait-for graph (arch M1; DEC-51f)

Threads: `opencon`/`addcon`/`dnsseed`/`mncon` (OS), the `net` runtime (readers, writers, socket task, RPC), `peerman` (the actor), `validation` (V), `events` (the ordered `ChainEvent` dispatcher), overlay workers (later), `scheduler`.

| Edge | Who waits on whom | Bound |
|---|---|---|
| actor → V | `ChainCommand::ProcessHeaders` (M2), `ProcessBlock` (P3) | V always answers or drops the channel |
| V → events | V blocks when > 10 block events are pending (A1 §3.1) | the dispatcher never blocks on the actor (below) |
| events → actor | `PeerManagerHandle::chain_event` | **non-blocking**: `UpdatedBlockTip` is a coalescing cell (≤ 8 hashes per peer, as `m_blocks_for_headers_relay`), other events an unbounded queue |
| overlays/RPC → actor | every other handle method | **non-blocking**: unbounded command queue; `stats` lock-free |
| anyone → outbox | `NodeHandle::push_message` | **never blocks**: unbounded MPSC with the **send-memory charge** (not wire bytes): `send_memusage += charge(msg)` where `charge = 56 + MallocUsage(capacity)` (`CSerializedNetMsg::GetMemoryUsage` = `sizeof(*this)` + `memusage::DynamicUsage(data)`, `v23:net.cpp:135-141 / v24:136-142`; `MallocUsage(n) = ((n + 31) >> 4) << 4` on 64-bit, `memusage.h:51-58`; `sizeof(CSerializedNetMsg)` = 24 + 32 = 56 on 64-bit libstdc++, so an 8-byte `pong` costs 88, not its 32 V1 wire bytes), and `pause_send = send_memusage + transport.send_memory_usage() > -maxsendbuffer × 1000` (`v23:4837-4849 / v24:4878-4890`); at handoff the charge moves to the transport (`SocketSendData` `v23:1712-1720 / v24:1702-1709`) and the pause state is recomputed after every write from both terms (`v23:1771 / v24:1765`), so a V1 payload that is partly sent still counts in full until the transport releases it, and V2 charges its encoded buffer (`V1Transport::GetSendMemoryUsage` `v24:877-882`, `V2Transport::` `v24:1664-1670`). The `fPauseSend` flag only stops polling; nothing blocks (G3) |
| `initialize_node` → peer map | the short peer-map mutex (Core's `m_peer_mutex`) | taken only to insert or to obtain an owned `Arc<PeerState>` (Core's `PeerRef`, `GetPeerRef` `v24:np:1858`); the actor never holds it while waiting on V, so synchronous initialization never waits behind a blocked actor (g3) |
| writer → socket | blocking send | per connection; a stuck peer stalls only its writer |
| actor/ConnMan → AddrMan, BanList, address cache | leaf mutexes | no I/O and no callouts under them |

Invariants: (i) **no thread ever waits on the actor**; (ii) `push_message` never blocks; (iii) a dropped reply channel means "interrupted": the actor exits the pass; **shutdown sequence** (g3; Core `StopThreads` then `StopNodes`, `v23:net.cpp:4155-4195 / v24:4187-4210`): ND1 sets the interrupt flag and wakes the actor → cancels any in-flight `ChainCommand` wait by dropping the reply sender (V keeps running) → **joins** the actor thread → stops the ConnMan threads and readers → ConnMan's `StopNodes` dumps `peers.dat` and `anchors.dat`, then deletes every node and runs **terminal `finalize_node` cleanup synchronously on the stopping thread** (the actor is gone, so queued `finalize_node` commands are drained by this owner, as Core finalizes after joining `msghand`) → stops V → stops the dispatcher; (iv) lock hierarchy `NodeTable → peer map (short, insert/lookup only) → per-node {inbox, outbox, small mutexes} → wake mutex`, with AddrMan, BanList and the address cache as leaves, checked by A1 §3.1's debug lock-order checker; (v) `sim` tests: block V, flood chain events, push from an overlay thread and a stalled writer, assert progress; shut down while V is blocked and while finalizations are queued, assert the join completes and every node is finalized. The outbox is MPSC, not SPSC; A1 §2.14's "bounded send queue with backpressure" is an erratum (§3.15).

### 2.3 Modules

```
dashd-p2p/src/
  lib.rs            pub use; crate docs; the Core-file map (ARCHITECTURE.md mirrors it)
  release.rs        Identity, P2pRules (O17 §4.2), V23_1/V24_0 literals, of(Release)
  fingerprint.rs    LocalIdentity (UA, services), VersionPayload builder, handshake plans
  net/
    node.rs         NodeId, ConnectionType, NodeHandle (CNode), Inbox (two queues, copied payloads), OutboxHandle (unbounded MPSC)
    nodes.rs        NodeTable, NodesSnapshot (shuffle), counts by type and network
    connman.rs      ConnMan: start/stop, InitBinds, OpenNetworkConnection, DisconnectNodes, InactivityCheck, setnetworkactive
    opencon.rs      ThreadOpenConnections port as opencon_step() (slots, anchors, feelers, extra peers, address choice) + thread driver
    addcon.rs       ThreadOpenAddedConnections port; GetAddedNodeInfo
    seeds.rs        DNS seeds, fixed seeds (generated from chainparamsseeds.h), -seednode, -connect
    addrman/        mod.rs (AddrMan API), imp.rs (AddrManImpl), info.rs (AddrInfo, chance, terrible),
                    bucket.rs, ser.rs (peers.dat V4_MULTIPORT), check.rs (-checkaddrman), rng.rs (FastRandom)
    netaddr.rs      CNetAddr/CService/CAddress semantics over base-sdk's address types
    netgroup.rs     NetGroupManager (GetGroup, asmap Interpret/GetMappedAS, checksum)
    local.rs        mapLocalHost, AddLocal/RemoveLocal, SeenLocal, GetLocal, GetLocalAddrForPeer, IsReachable
    addrdb.rs       peers.dat / anchors.dat read/write (magic, checksum, tmp+fsync+rename)
    permissions.rs  NetPermissionFlags (type and the flags P2a reads; P4 adds the options)
    stats.rs        NodeStats (CNodeStats) and PeerStateStats (CNodeStateStats) for getpeerinfo
  processing/
    peerman.rs      PeerManager actor thread, MessageLoop::run_one_pass(now), command queue, timers on the scheduler time base
    logic.rs        PeerLogic: initialize_node, finalize_node, process_messages, send_messages
    peer.rs         PeerState (Peer ∪ CNodeState), ChainSyncTimeoutState
    policy.rs       CommandPolicy table (74 rows), Gate, PreRead, ReadPlan, ThrowOutcome, PostRead, M2Behaviour
    dispatch.rs     process_message: gates → handler (decodes at Core's read point); NetHandler registry; unknown
    handshake.rs    version, verack, sendheaders(2), sendcmpct, sendaddrv2, sendtxrcncl, senddsq, qsendrecsigs; push_node_version
    ping.rs         MaybeSendPing, ping, pong
    addr.rs         addr, addrv2, getaddr, PushAddress, RelayAddress, MaybeSendAddr, the addr cache front end
    headers.rs      headers, headers2, sync start, getheaders policy, unconnecting, continuity, timeouts, HeaderBatch
    inv.rs          inv (block path, blocks-only, unknown types), getdata/notfound size checks
    announce.rs     block announcements (SendMessages section 7)
    eviction.rs     ConsiderEviction, EvictExtraOutboundPeers, CheckForStaleTipAndEvictPeers
    misbehavior.rs  Misbehaving, MaybeDiscourageAndDisconnect, MaybePunishNodeForBlock, the score table
    seams.rs        ChainAccess, ConnChainView, BlockSync, ObjectRequests, TxRelay, OrphanStep, BanList, Serving
    handle.rs       PeerManagerHandle, PeerStats snapshots
  sim/              (feature "sim", used by dev-tests) virtual clock, in-memory transports, scripted peers
```

Conventions (A1 Appendix E): a `// Core: <file>:<v23 line>/<v24 line> <Function>` anchor per ported function; `#![forbid(unsafe_code)]`; no `std::time` outside `dashd-clock`; no `HashMap` iteration on any path whose order a peer can observe (the per-pass shuffle is Fisher–Yates over `Vec`; `RelayAddress` is a max over SipHash keys, order-insensitive; `mapAddr` is never iterated); `check-release-refs.sh` (O17 rule 2) passes.

### 2.4 Public API (Rust signatures)

```rust
// ---- release.rs (O17 §4.2; P2a defines every field; doc comments cite rows) ----
pub struct Identity {
    /// O17 P2P-01, P2P-03. version.h:14 = 70240 (v23.1.8) | 70242 (v24.0.0-rc.3). i32: Core's `int nVersion`.
    pub protocol_version: i32,
    /// O17 P2P-02, DKG-01. version.h:23 = 70240 | 70242 (read by DK1/LQ4a; carried here).
    pub min_mn_proto_version: i32,
    /// O17 P2P-01. "23.1.8" | "24.0.0" = FormatVersion(client_version); no rc suffix.
    pub ua_version: &'static str,
    /// O17 P2P-01. 230108 | 240000 (getnetworkinfo.version, RP1a).
    pub client_version: u32,
}
pub enum ObjectDownload { Legacy, TxRequest }
pub enum QrInfoRequest { Unbounded, Capped { max_base_blocks: usize } }
pub enum BlockRead { UnderMainLock, Unlocked }
pub struct P2pRules {                            // O17 §4.2, all ten fields, complete literals
    pub object_download: ObjectDownload,         // P2P-05..-09, -11 (P5)
    pub notfound_limit: usize,                   // 116 | 50_000 (P2P-10) — P2a's NOTFOUND size check
    pub unsolicited_scoring: bool,               // P2P-12 (P5a)
    pub tx_inv_before_verack: bool,              // P2P-15 (P5)
    pub msg_asset_unlock: bool,                  // P2P-04 — P2a's INV `is_known_type`
    pub mnauth_inbound_cap: Option<usize>,       // P2P-18 (P7)
    pub request_gov_invs_when_disabled: bool,    // P2P-21 (GV1b)
    pub qrinfo_request: QrInfoRequest,           // DKG-12 (P6b)
    pub mnlist_data_unavailable_score: u32,      // DKG-13 (P6b)
    pub block_read_vs_prune: BlockRead,          // P2P-17 (P4)
}
impl Identity { pub const V23_1: Identity; pub const V24_0: Identity; pub fn of(r: Release) -> &'static Identity; }
impl P2pRules { pub const V23_1: P2pRules; pub const V24_0: P2pRules; pub fn of(r: Release) -> &'static P2pRules; }

// ---- fingerprint.rs ----
pub struct LocalIdentity {                       // built once by ND1 at init
    pub identity: &'static Identity,
    pub user_agent: UserAgent,                   // "/Dash Core:<ua_version>[(<comments>)]/" or the -honestua form; ≤ 256
    pub push_version: Option<i32>,               // -pushversion, non-mainnet only (v23:np:1554-1557 / v24:1577-1580)
    pub local_services: ServiceFlags,            // §3.3.3
}
impl LocalIdentity {
    pub fn new(identity: &'static Identity, params: &ChainParams, opts: &IdentityOptions) -> Result<Self, InitError>;
    pub(crate) fn version_payload(&self, v: &VersionInputs) -> Version;   // base-sdk p2p_core::Version
}

// ---- net/node.rs ----
pub type NodeId = i64;                           // Core NodeId (int64_t), shared counter with inbound, from 0
pub enum ConnectionType { Inbound, OutboundFullRelay, Manual, Feeler, BlockRelay, AddrFetch }   // node/connection_types.h
impl ConnectionType {
    pub fn as_str(self) -> &'static str;         // connection_types.cpp:11-22
    pub fn is_outbound_or_block_relay(self) -> bool;   // net.h IsOutboundOrBlockRelayConn
    pub fn is_manual_or_full_outbound(self) -> bool;
    pub fn expects_services(self) -> bool;       // ExpectServicesFromConn: OutboundFullRelay | BlockRelay | AddrFetch
}
pub struct NodeHandle { /* CNode net level: id, addr (CAddress, services as addrman recorded), addr_bind, addr_name,
    conn_type, inbound_onion, keyed_net_group, local_host_nonce, connected (mockable seconds), permissions,
    transport_info; atomics: disconnect, successfully_connected, pause_send, pause_recv, other_side_disconnected,
    version, common_version, relays_txs, version_services, version_relay_txs (DKG-01), masternode_connection,
    masternode_probe, masternode_iqr, qwatch, last_send, last_recv, last_block_time, last_tx_time, min_ping_time,
    time_offset, bip152_hb_to/from, has_all_wanted_services, first_message_at, first_message_is_mnauth;
    small mutexes: clean_subver, addr_local, mnauth challenges (sent/received), verified_protx/pubkey; inbox; outbox */ }
impl NodeHandle {
    pub fn push_message(&self, msg: OutMsg);     // CConnman::PushMessage: thread-safe, never blocks; charges Core's send-memory usage
                                                 // (56 + MallocUsage(capacity)) and recomputes fPauseSend synchronously (§2.2.1, G3)
    pub fn request_disconnect(&self);            // fDisconnect = true
}
pub struct RawMsg { pub cmd: CommandString, pub payload: Vec<u8>, pub recv_time: Micros, pub raw_size: usize }  // P1 → P2a; the payload is
                    // an owned copy (Core copies into CDataStream; DEC-51h), so no read buffer outlives its messages
pub struct OutMsg { pub cmd: CommandString, pub payload: Bytes } // P2a → P1

// ---- seams.rs ----
pub trait ChainAccess: Send {                    // CH1 implements; the requirements on CH1 are §2.5.3 (DEC-51d)
    fn snapshot(&self) -> Arc<ChainReadSnapshot>;           // ONE coherent, retained snapshot per Core critical section (R8, G4):
                                                            // published before every ProcessHeaders reply, never throttled
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId>;  // concurrent append-only hash index (R1)
    fn entry(&self, id: BlockId) -> IndexEntryView;         // hash, height, prev, time, bits, chain_work immutable after insert;
                                                            // `status` is a seqlocked read (never cached by P2a; a failed or ChainLock-
                                                            // conflicting entry is visible on the next read) (R2, g2)
    fn ancestor(&self, id: BlockId, height: u32) -> BlockId; // over immutable prev/skip
    fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash>;   // Core GetLocator over immutable prev/skip; None = Core's null index:
                                                            // the active tip's locator under V23_1, empty under V24_0 (P2pRules.null_locator; G2)
    fn loading_blocks(&self) -> bool;                       // LoadingBlocks() / fImporting||fReindex
    fn min_chain_work(&self) -> Arith256;
    fn is_initial_block_download(&self, snap: &ChainReadSnapshot, now: NodeSeconds) -> bool;   // CH1's latch, evaluated against the
                                                            // snapshot's coherent tip/work/loading facts and mockable time before it latches (R4)
    fn process_headers(&self, batch: HeaderBatch, from: NodeId) -> HeadersResult;   // blocks; ChainCommand::ProcessHeaders
}
pub struct ChainReadSnapshot {                   // immutable once published; a reader keeps the Arc for the whole Core critical section
    pub tip: TipFacts, pub active_chain: ActiveChainView /* height → BlockId, contains(), next(), fork_in_global_index(locator) */,
    pub bls: BlsCtx /* BlsCtx::for_tip, the source of ProtoCtx.bls for every read (g2) */, pub epoch: u64,
}
pub trait ConnChainView: Send + Sync {           // what ConnMan reads from the chain side (arch m13): the tip MN list and the IBD service cache
    fn mn_list_at_tip(&self) -> Arc<MnList>; fn ibd_service_cache(&self) -> bool;
}                                                // the verified-ProTx set of §3.8.2 step 6 is not chain state: ConnMan collects it from
                                                 // NodeTable's per-node `verified_protx` fields under the table's read lock, as Core does from m_nodes
pub struct HeaderBatch { pub headers: Vec<BlockHeader>, pub hashes: Vec<BlockHash> }   // hashes[i] = X11(headers[i])
pub struct HeadersResult { pub last_accepted: Option<BlockId>, pub invalid: Option<HeaderReject>, pub snap: Arc<ChainReadSnapshot> }
pub struct HeaderReject { pub result: BlockValidationResult, pub reason: String, pub debug: String }  // CS0 types
pub struct TipFacts { pub tip: BlockId, pub tip_work: Arith256, pub tip_time: u32, pub tip_height: u32,
                      pub best_header: BlockId, pub best_header_time: u32, pub best_header_work: Arith256,
                      pub epoch: u64 }                     // no `ibd` field: IBD is a lazy, latched query over the snapshot (is_initial_block_download)

pub trait BlockSync: Send {        // P3; M2 ships NoBlockSync (arch m1: the trait sits at every Core position now)
    fn headers_direct_fetch(&mut self, cx: &mut Cx, peer: NodeId, last: BlockId);
    fn is_block_requested(&self, hash: &BlockHash) -> bool;                          // IsBlockRequested (v24:np:1271; inv path :4537)
    fn stall_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros) -> SendFlow; // section 9: stalling, block timeout (may return)
    fn getdata_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros);           // section 12: getdata(blocks), runs even after ConsiderEviction set fDisconnect
    fn block_checked(&mut self, cx: &mut Cx, hash: &BlockHash, state: &BlockValidationState);   // BlockChecked hook
    fn blocks_in_flight(&self, peer: NodeId) -> usize;
    fn any_in_flight(&self) -> bool;                                                 // mapBlocksInFlight.empty()
    fn finalize(&mut self, peer: NodeId);
}
pub trait ObjectRequests: Send {   // P5 builds Legacy or TxRequest per P2pRules.object_download; M2: NoObjectRequests
    fn add_announcement(&mut self, peer: NodeId, inv: Inv, preferred: bool, now: Micros);
    fn received_response(&mut self, peer: NodeId, inv: &Inv);                        // notfound
    fn send_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros);              // getdata (non-blocks), ≤ 1000 per message
    fn finalize(&mut self, peer: NodeId);
}
pub trait TxRelay: Send { fn send_section(&mut self, cx: &mut Cx, peer: NodeId, now: Micros); }   // trickle; P5
pub trait OrphanStep: Send { fn step(&mut self, cx: &mut Cx, peer: NodeId) -> bool; }           // P2P-13 is P5's
pub trait BanList: Send + Sync {   // P4's BanMan; M2 ships DiscourageOnly (rolling bloom 50,000 / 1e-6, banman.h:94)
    // Contract (arch m12): non-blocking, no I/O under its lock; P4's banlist.json dump happens outside the lock
    fn discourage(&self, addr: &NetAddr); fn is_discouraged(&self, addr: &NetAddr) -> bool; fn is_banned(&self, addr: &NetAddr) -> bool;
}
pub trait Serving: Send { fn process_getdata(&mut self, cx: &mut Cx, peer: NodeId); }           // P4/P5; M2: EmptyServing

// ---- NetHandler (mirrors Core's NetHandler, net_processing.h v23:90-113 / v24:122-145; corrects A1 §2.14) ----
// Core's signature is ProcessMessage(CNode&, const std::string&, CDataStream&) -> void: handlers act BEFORE reading and on the
// raw size (e.g. NetQuorum QGETDATA: +10 unverified and return with nothing read, then +100 if vRecv.size() > the fixed request
// size, only then `vRecv >> request`, v24:llmq/net_quorum.cpp:79-92), and call PeerMisbehaving/PeerRelayInv/PeerConsumeObjectRequest
// part-way through. So the handler gets the RAW payload and decodes at Core's read point through `cx.read::<T>()`, which builds
// ProtoCtx at the call and maps decode errors to Core's Throw (arch M6; DEC-51g). P2a's own §3.11 stubs use the same signature.
pub trait NetHandler: Send + 'static {
    fn commands(&self) -> &'static [CommandString];
    fn process_message(&mut self, cx: &mut HandlerCx<'_>, peer: NodeId, cmd: CommandString, payload: &RawPayload)
        -> Result<Option<MessageProcessingResult>, Throw>;   // Some(..) for v23-style handlers that return a result; None for v24-style
    // HandlerCx: fn read<T: MsgDecode>(&mut self, payload: &RawPayload) -> Result<T, Throw>; fn remaining(&self) -> usize;
    //            fn misbehaving(..); fn relay_inv(..); fn consume_object_request(..); fn push(..)
    fn already_have(&self, inv: &Inv) -> bool { false }
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, peer: NodeId, inv: &Inv) -> bool { false }
    fn schedule(&mut self, _s: &Scheduler) {}       // Core NetHandler::Schedule
    fn start(&mut self) {} fn stop(&mut self) {} fn interrupt(&mut self) {}
}

// ---- PeerManager and its handle ----
pub struct PeerManagerConfig { pub identity: LocalIdentity, pub rules: &'static P2pRules,
                               pub consensus: &'static UngatedRules, pub params: Arc<ChainParams>,
                               pub opts: PeerManagerOptions /* -blocksonly, -peerbloomfilters, -txreconciliation, -maxtipage, … */ }
impl PeerManager {
    pub fn spawn(cfg: PeerManagerConfig, nodes: Arc<NodeTable>, addrman: Arc<AddrMan>, chain: Box<dyn ChainAccess>,
                 seams: Seams, clock: Arc<dyn NodeClock>) -> (PeerManagerHandle, JoinHandle<()>);
}
impl PeerManagerHandle {
    pub fn initialize_node(&self, node: Arc<NodeHandle>);  // SYNCHRONOUS on the caller's thread (core M-3): creates PeerState under the
                                                           // peer-map mutex and pushes `version` for outbound nodes before the node is in NodeTable
    pub fn finalize_node(&self, id: NodeId);                // ConnMan after DeleteNode
    pub fn chain_event(&self, ev: ChainEventLite);          // UpdatedBlockTip (best height, IBD cache, announcements), BlockConnected
    pub fn send_pings(&self);                               // RPC `ping`
    pub fn stats(&self, id: NodeId) -> Option<PeerStateStats>;   // lock-free; per-field atomics for the FT-visible counters (addr_processed,
                                                                  // addr_rate_limited, misbehavior_score, sync/common height), the rest refreshed
                                                                  // after each process_messages via dirty flags (arch m5)
    pub fn misbehaving(&self, id: NodeId, howmuch: i32, msg: &str);
    pub fn fully_connected_peers(&self) -> Vec<Arc<NodeHandle>>;  // NodesSnapshot(FullyConnectedOnly), table order, unshuffled (SP1)
    pub fn wake(&self); pub fn shutdown(&self);
}

// ---- ConnMan ----
pub struct ConnManOptions { /* CConnman::Options net.h v23:1222-1255 / v24 (+ m_masternode_connections): local_services,
    max_connections (after the fd clamp), max_outbound_full_relay, max_outbound_block_relay, max_outbound_onion,
    max_addnode, max_feeler, send_buffer_max, recv_flood_size, peer_connect_timeout, connect_timeout, seed_nodes,
    added_nodes, use_addrman_outgoing, specified_outgoing, dnsseed, fixedseeds, forcednsseed, bind_failure_fatal
    (InitRules, P2P-33), masternode_connections (v24 gates mncon; unobservable), active_masternode, binds, … */ }
impl ConnMan {
    pub fn start(opts: ConnManOptions, nodes: Arc<NodeTable>, addrman: Arc<AddrMan>, peerman: PeerManagerHandle,
                 transport: Arc<dyn TransportFactory>, ban: Arc<dyn BanList>, clock: Arc<dyn NodeClock>)
                 -> Result<ConnMan, InitError>;            // InitBinds + task start, CConnman::Start order (v23:net.cpp:3985 / v24:3991)
    pub fn add_node(&self, p: AddedNodeParams) -> bool; pub fn remove_added_node(&self, s: &str) -> bool;
    pub fn added_node_info(&self, include_connected: bool) -> Vec<AddedNodeInfo>;
    pub fn open_one_try(&self, dest: &str, conn_type: ConnectionType, v2: bool) -> bool;  // addnode onetry; addconnection (regtest)
    pub fn disconnect_node(&self, by: DisconnectBy) -> bool;
    pub fn set_network_active(&self, active: bool);
    pub fn counts(&self) -> ConnectionCounts; pub fn node_stats(&self) -> Vec<NodeStats>;
    pub fn get_addresses(&self, max: usize, pct: usize, net: Option<Network>, filtered: bool) -> Vec<Address>;   // v23:4241 / v24:4254
    pub fn get_addresses_cached(&self, requestor: &NodeHandle, max: usize, pct: usize) -> Vec<Address>;         // the 21–27 h cache (P2b calls)
    pub fn deterministic_randomizer(&self, id: u64) -> SipHasher;                                           // v24:4968
    pub fn stop(self);                                    // DumpAnchors, DumpAddresses, StopNodes order
    // P7 seam, declared now: add_pending_masternode, set_masternode_quorum_nodes, set_quorum_relay_members, …
}

// ---- P1 seam ----
pub trait TransportFactory: Send + Sync {
    /// Starts a connection's transport on a connected stream. `v2` is decided by ConnMan (§3.8.2 step 10, §3.8.5).
    /// The stream is a trait object (arch m2) so `sim`'s in-memory pipes and P8's proxied streams fit.
    fn outbound(&self, stream: Box<dyn NetStream>, v2: bool, node: Arc<NodeHandle>) -> (Box<dyn FrameReader>, Box<dyn FrameWriter>);
    fn inbound(&self, stream: Box<dyn NetStream>, node: Arc<NodeHandle>) -> (Box<dyn FrameReader>, Box<dyn FrameWriter>);   // P2b
}
pub trait NetStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
pub enum ReaderEvent { Messages(Vec<RawMsg>), Closed { other_side: bool }, Fatal(TransportFault), V2HandshakeFailedEarly }
pub trait FrameWriter: Send {                   // P1; the two methods G3 needs besides framing:
    fn set_message_to_send(&mut self, msg: OutMsg) -> bool;   // false while a message is still being sent or the V2 handshake is incomplete
    fn send_memory_usage(&self) -> usize;                    // Core's transport charge: the retained message's GetMemoryUsage (V1) or the
                                                             // encoded buffer (V2), including capacity retained after a partial send
}
```

`Cx`/`HandlerCx` gives a handler exactly what Core's `PeerManagerImpl` methods reach: the peer's `PeerState`, the node handle, `push_message`, `misbehaving`, `ChainAccess`, addrman, the clock, the RNG and the release groups. Nothing outside `processing/` mutates `PeerState`.

### 2.5 Neighbours and the exact types exchanged

| Neighbour | Direction | Types | Contract |
|---|---|---|---|
| **P1** | P1 → P2a | `RawMsg`, `ReaderEvent` | one `ReaderEvent::Messages` per socket read: every message completed by that read, in arrival order, all stamped with the read's time (`CNode::ReceiveMsgBytes` `v23:net.cpp:648-691 / v24:649-692`); a bad checksum, or invalid command bytes (0x20–0x7e V1, 0x20–0x7f V2, non-zero after NUL), drops the message **inside P1** and never reaches the gates (BS4a-r2 N-4: Core never sets `nTimeFirstMessageReceived` for a dropped message); bad magic, oversize or a V2 failure surfaces as `Fatal` and ConnMan closes the socket at once |
| | P2a → P1 | `OutMsg`; `NodeHandle.common_version` | short IDs only once the common version allows (≥ 70235; `platformban` ≥ 70240); the actor stores `common_version` before it pushes `sendaddrv2`/`verack` |
| | P1 → ConnMan | `ReaderEvent::V2HandshakeFailedEarly` | the `ShouldReconnectV1` condition (`v23:net.cpp:1653 / v24:1647`): ConnMan queues a v1 retry in `DisconnectNodes` (§3.8.5) |
| **P2b** | P2b → P2a | `ConnectionType::Inbound` handles via `initialize_node` | the shared `version` handler already has the inbound branches (shy `version`, self-connection nonce, inbound `fMasternode`, `SeenLocal`); P2b adds accept, admission, serving, and owns their tests; the `GetAddresses(requestor)` cache lives in P2a's ConnMan |
| **CH1** | P2a → CH1 | `HeaderBatch`, `NodeId` | `ChainCommand::ProcessHeaders { batch, from, reply }` (A1 §2.10 line 978 keeps its name); CH1 checks `hashes[i] == X11(headers[i])` only under `debug_assertions`, as Core's `ASSERT_IF_DEBUG` (`v24:validation.cpp:4602`; v24's own `ProcessNewBlockHeaders` passes `header.GetHash()` into `AcceptBlockHeader`, `v24:validation.cpp:4736`) |
| | CH1 → P2a | `HeadersResult`, `Arc<ChainReadSnapshot>`, `ChainEventLite::UpdatedBlockTip{new, fork, ibd}`, `BlockConnected` | one coherent retained snapshot (tip, best header, active chain, BLS context, epoch) published **before** the reply and never throttled; the index is concurrent and append-only; IBD is a lazy latched query over the snapshot; `best_header` is never null; V checks its command queue between `activate_best_chain` steps (§2.5.3, requirements R1–R8; DEC-51d, G4) |
| **P3** | P2a ↔ P3 | `BlockSync`, `CompactBlockSource` | P3 replaces `NoBlockSync`; the announcement code calls `cmpctblock_for(tip)` for the HB branch |
| **P4** | P2a ↔ P4 | `BanList`, `NetPermissionFlags`, `Serving`, score-table extensions | P4 replaces `DiscourageOnly` and `EmptyServing` |
| **P5** | P2a ↔ P5 | `ObjectRequests`, `TxRelay`, `OrphanStep` | one tracker per `P2pRules.object_download` |
| **P5a/SP0/SP1/P6/P7/AM1** | overlays → P2a | `NetHandler`, `PeerManagerHandle`, `NodeTable::push_message` | registered by ND1 in Core's per-release order (§3.2.6); the sync tick uses `fully_connected_peers()` (table order) |
| **ND1** | ND1 → P2a | `PeerManagerConfig`, `ConnManOptions`, `LocalIdentity` | init step 13 (A1 §2.18) |
| **RP1a** | RPC → P2a | `PeerManagerHandle::stats`, `ConnMan::{counts,node_stats,added_node_info,…}`, addrman queries | `getpeerinfo`, `getnetworkinfo`, `getconnectioncount`, `getaddednodeinfo`, `getnodeaddresses`, `addpeeraddress`, `addnode`, `disconnectnode`, `setnetworkactive`, `ping`, regtest `addconnection` (§8 Q5) |
| **LQ4a/DK1** | P2a → them | `NodeHandle.version_services`, `.version_relay_txs` | recorded in the `version` handler as v24 does (`v24:np:4060-4061`); v23 does not record them, but the fields are internal, so recording under both profiles is unobservable |
| **dashd-clock** | both | `NodeClock`, `SteadyClock`, `TimeOffsets` (the median filter), `exponential_rand` | §2.7 |

#### 2.5.3 The CH1 contract, stated as requirements on CH1 (DEC-51d; arch M5)

Core reads the block index at four points while handling a headers message, each in its own `cs_main` scope: `LookupBlockIndex(headers[0].hashPrevBlock)` (`v23:np:3353 / v24:3337`), `LookupBlockIndex(headers.back().GetHash())` (`v23:3375 / v24:3359`), `ProcessNewBlockHeaders` (`v23:3378 / v24:3362`, one scope for the whole batch, `validation.cpp` v24:4729-4757), and the reads in `UpdatePeerStateForReceivedHeaders` (`v23:3283-3336 / v24:3267-3320`: tip work, `MinimumChainWork`, IBD). Nothing is atomic across them in Core. For the actor to read the index without queueing behind V (which would be worse than Core's `cs_main`) and without a data race (A1 §2.10's plain `hashbrown::HashTable` that V mutates is not readable concurrently), P2a requires of CH1 — and these go into the CH1 design brief now, since CH1 is on the critical path:

| # | Requirement on CH1 | Why |
|---|---|---|
| R1 | **A concurrent, append-only hash index** `hash → BlockId` (open addressing with atomic slots grown by RCU, or an `RwLock` taken per insert; entries never deleted), so `ChainAccess::lookup` is safe from the actor while V inserts | A1 §2.10 l.966's table is not concurrent; A1 §2.10.2's "locators via `ChainCommand::Query`" would queue every `inv`/`headers` lookup behind V (errata) |
| R2 | **Index entries immutable after insert** for `prev`, `skip`, `hash`, `height`, `time`, `bits`, `chain_work` (status may change); `ChainAccess::locator` and `ancestor` walk only those, so they are lock-free by construction | Core's `GetLocator` reads only `pprev`/`pskip` (`v23:chain.cpp:32-57`, `v24:chain.cpp:32-54`) |
| R3 | **`TipFacts` and an `ActiveChainView`** (height → `BlockId`, `contains`, `next`, `fork_in_global_index`; an `imbl::Vector` or a seqlocked array) published together inside one `ChainReadSnapshot` (R8), on every best-header and tip change, **without** A1 §2.10.2's 100 ms IBD throttle (which stays for the full `ChainView`); published **before** the `ProcessHeaders` reply | a throttled view makes a `getheaders` locator lag the headers just accepted; P2b's `getheaders` serving (`FindForkInGlobalIndex` + `ActiveChain().Next`, `v24:np:4784-4815`) and §3.4.8's `ActiveChain().Contains` need an active chain consistent with the tip |
| R4 | **`is_initial_block_download(snap, now)` as a lazy, latched query** evaluated against the mockable clock and one coherent set of tip/work/loading facts (the snapshot) **before it irreversibly sets the latch**, as Core's `IsInitialBlockDownload()` under `cs_main` (`v24:validation.cpp`: `m_cached_finished_ibd`, `LoadingBlocks`, tip work vs `MinimumChainWork`, tip time vs `max_tip_age`), not a snapshot field | a `setmocktime` that makes the tip recent flips Core on the next call; a snapshot would stay stale for `MaybeSendAddr`, `GetDesirableServiceFlags` and object requests |
| R5 | **`best_header` is non-null after load** (Core's `LoadBlockIndex` guarantees it); P2a never writes it: §3.4.2's `if best_header is unset: best_header = tip` is an assertion in P2a | Core writes `m_best_header` from net_processing (`v24:6342-6344`) only because its own load may leave it null; CH1 owns the field |
| R6 | **V checks its command queue between `activate_best_chain` steps**, so a long RPC- or CL-triggered reorg does not hold the actor (and every peer) for its whole duration; CH1 ships a queue-service/reorg-latency test (a 1,000-block reorg with `ProcessHeaders` commands arriving: each is answered within one ABC step) | Core's `ActivateBestChain` releases `cs_main` between steps, letting `ProcessNewBlockHeaders` interleave |
| R7 | **The `ProcessHeaders` payload is `HeaderBatch { headers, hashes }`** (an A1 §2.10 change, errata), complete only after the continuity check succeeded (§3.4.9); CH1 checks `hashes[i] == X11(headers[i])` only under `debug_assertions`, as Core's `ASSERT_IF_DEBUG` (`v24:validation.cpp:4602`); the reply `HeadersResult { last_accepted, invalid, snap }` carries Core's `pindexLast` (the last **accepted** header, `validation.cpp` v24:4742), the `BlockValidationState` when invalid, and the snapshot after acceptance | so `UpdatePeerStateForReceivedHeaders`'s reads are at least as consistent as Core's |
| R8 | **One coherent, retained read snapshot** (`ChainReadSnapshot { tip, best_header, active_chain, bls, epoch }`, an `Arc` the reader holds for the whole critical section) instead of independent `tip()`/`active_chain()` loads; every fork/next/contains operation and the IBD evaluation run against one snapshot; immutable index and locator walks stay outside it | P2b's fork search and successor walk run under one `cs_main` scope (`v23:np:4731-4809 / v24:4784-4859`) and announcement selection checks membership under the enclosing send scope (`v23:6193, 6277-6282 / v24:6338, 6422-6428`); two independent loads could mix epoch E's tip with E+1's chain during a reorg and truncate a reply or choose `headers` vs `inv` from facts that never coexisted (G4). CH1 ships a `sim` test with a deterministic reorg between two loads, asserting a reader sees one complete epoch |

With R1–R8, `ChainAccess::lookup` answers the two pre-acceptance lookups on the actor as Core does from `msghand` (a concurrent `submitheader` on V can interleave exactly as in Core between two `cs_main` scopes); torn reads *between* those scopes are Core's, torn reads *within* one scope are not, which is what R8 prevents. Locator null handling: `ConsiderEviction` passes a null index on a genesis-only node (§3.4.4, G2), and the two pins differ there; `locator(None)` reproduces the profile's behaviour through `P2pRules.null_locator`. The reply is the only `ChainCommand` P2a awaits at M2; P3 adds `ProcessBlock`.

### 2.6 Hooks for the overlays

| Hook | Called | Provides | Default at M2 | Owner |
|---|---|---|---|---|
| version tail out/in | building our `version`; parsing theirs | the 32-byte challenge, `fMasternode`; the inbound MN rule | implemented | AM1/P7 read the challenges |
| `verack` slot 1 | first push of the `verack` handler | `mnauth` (`CMNAuth::PushMNAUTH`, `v23:np:4175 / v24:4206`) | none | AM1 |
| `verack` slot 2 | after `senddsq`, inside `!RejectIncomingTxs` | `qwatch` (`IsWatching && IsMasternodeQuorumNode`, `v23:4193-4201 / v24:4224-4232`) | none | DK1/P7 |
| first post-`verack` message | before the blocks-only gate | probe rule | implemented | P7 sets the probe flag |
| `NetHandler` registry | extension dispatch (§3.2.6) | per-command handlers, `already_have`, `process_getdata`, `schedule` | the DKG +10 fallback; silence otherwise | every overlay task |
| `MessageProcessingResult` applier | after a handler returns | misbehaviour → erase request → relay txs → relay invs (`v23:3798-3812 / v24:3819-3837`) | implemented | — |
| `PeerManagerHandle` services | any thread | `misbehaving`, `relay_inv` (every peer, no `CanRelay` filter), `relay_inv_filtered`, `relay_dsq`, `relay_recsig`, `push_inventory`, `ask_peers_for_tx`, `requested_object_count`, `is_banned`, object-request erase/consume/forget | the object-request ones forward to `ObjectRequests` (no-op) | P5, P5a, P6a, SG1a |
| `NodeTable` services | any thread | `push_message` (per-peer FIFO), `fully_connected_peers` in **connection order, unshuffled** (the sync tick depends on it), per-node MN fields | implemented | SP1, P7, AM1, LQ5 |
| `CanRelay()` | `verack`, every send pass, relay helpers, the sync tick | `!masternode_connection || masternode_iqr` (`v24:net.h:1083`) | implemented | P7 |
| connection-count change | socket task | 0 ↔ non-zero transitions (mnsync's non-forced reset) | event emitted | SP1 |
| quorum connection sets | ConnMan | `set_quorum_nodes`, `set_quorum_relay_members`, `is_quorum_node` | stored, unused | P7, LQ5 |
| `EvictExtraOutboundPeers` exemption | 45 s task | never evict `masternode_connection` peers | implemented | — |
| scheduler + validation-event executor | init | one serial executor for timers and queued validation events | provided by ND1; P2a's timers run on the actor (§3.1.6) | ND1 |

`getsporks`, `mempool` and `govsync` are **not** sent by the peer manager at either pin; the only sender is `SyncManager::ProcessTick` on the scheduler (every 1 s, `v23:node/sync_manager.cpp:24-30 / v24:24-30`; acting at most every 6 s of mockable time, `v23:161 / v24:157`). The first `getsporks` reaches a new peer U(0, 6] s after its `verack`; `govsync` follows only after the BLOCKCHAIN asset finishes. They belong to SP0/SP1 (A1 §2.14 "0.5–2 s"/"≈ 8 s" is an erratum, §3.15).

### 2.7 `dashd-clock` (new tiny runtime crate; resolves Opus Q1)

A1 §2.18 puts `NodeClock` in `dashd-node`, but `dashd-chainstate` (header `time-too-new` under `TimeSource::AdjustedTime`, `v23:validation.cpp:4144`) and `dashd-p2p` (every timer; the offset samples, `v23:np:4136 / v24:4167`) both need it and cannot depend on `dashd-node`. Core keeps them as free-standing utilities (`util/time.h:19` `NodeClock`, `timedata.cpp:29-118` both pins), fed by net_processing and read by validation. `dashd-clock` holds: `NodeClock` (mockable seconds/µs: `-mocktime`, `setmocktime`), `SteadyClock`, `TimeOffsets` (the 200-sample median filter with Core's rules and the clock warning, §3.3.8), `exponential_rand(now, mean)` (`random.cpp` v23:788-792 / v24:771-775), and the mock-scheduler hook. `sim` substitutes a virtual clock. **Owner: P2a-core phase A** (arch m11); CH1 depends on it, and ND1's PLAN row (which still lists `NodeClock`/mocktime) is edited accordingly. **Four time bases, kept distinct** (g4): *scheduler time* only for Core's scheduled callbacks (the 45 s `CheckForStaleTipAndEvictPeers`, `DumpAddresses`, `ReattemptInitialBroadcast`), which `mockscheduler` (`MockForward`) advances as in dashd (arch m4); *node time* (mockable `GetTime`) for the `addr`/local-address deadlines (`current_time` in `MaybeSendAddr`, `v23:np:6040-6106 / v24:6186-6252`, esp. `:6209, 6215`), the ping, the sync-start and headers-timeout checks, and the feeler/extra-peer deadlines `opencon` reads, none of which `mockscheduler` moves; *adjusted time* for `time_source` consumers under `V23_1`; *steady time* for the linger, the sweep and the MN throttle. Dash's `bump_mocktime` calls both `setmocktime` and `mockscheduler` (`v24:test_framework.py:883-900`), so `sim` has separate `setmocktime`-only and `mockscheduler`-only cases that assert which deadlines move.

### 2.8 Options P2a implements

Every option below is registered at both pins with the same default (`init.cpp` AddArg v23:557-775 / v24:606-828; `net.h`, `net_processing.h`, `netbase.h`, `timedata.h:16`, `addrman.h:41`). `-honestua` is ours (DEC-04). ND1 parses; P2a consumes through `ConnManOptions`/`PeerManagerOptions`/`IdentityOptions`. Options not implemented before their owner lands fail at startup with "not supported by rdashd yet" (A1 §2.18's rule; never silently ignored): `-proxy`, `-onion`, `-i2psam`, `-onlynet`, `-discover`, `-upnp`, `-natpmp`, `-whitebind`, `-whitelist`, `-maxuploadtarget`, `-capturemessages`, `-asmap` (P8/P4).

| Option | Default | Used by |
|---|---|---|
| `-maxconnections` | 125 (clamped to descriptors) | §3.8.8 |
| `-connect`, `-addnode`, `-seednode` | — | §3.8.2, §3.8.5 |
| `-dnsseed`, `-forcednsseed`, `-fixedseeds` | 1, 0, 1 | §3.8.4, §3.8.6 |
| `-listen`, `-bind` | 1 (soft-off with `-connect`), — | §3.8.9 |
| `-externalip`, `-allowprivatenet` | —, 0 | §3.7.4, §3.7.3 |
| `-networkactive` | 1 | §3.8.7 |
| `-v2transport` | 1 | §3.3.3, §3.8.2 |
| `-peertimeout`, `-timeout` | 60 s, 5000 ms | §3.8.7, §3.8.5 |
| `-maxsendbuffer`, `-maxreceivebuffer` | 1000, 5000 (× 1000 bytes) | §3.1.3, §3.8.7 |
| `-maxtimeadjustment` | 4200 s | §3.3.8 |
| `-maxtipage` | 6 h | §3.4.2 |
| `-blocksonly` | 0 | gate 7, the `relay` flag |
| `-peerbloomfilters`, `-peerblockfilters` | 1, 0 | §3.3.3 |
| `-uacomment`, `-honestua`, `-pushversion` (non-mainnet) | — | §3.3.2, §3.3.4 |
| `-checkaddrman` (debug) | 0 | §3.7.2 |
| `-txreconciliation` (debug-only) | 0 | §3.3.7 (off path only) |
| `-mocktime` | — | `dashd-clock` |

---

## 3. Core equivalence

Every subsection names the Core functions it replaces, the order of operations and the per-profile behaviour.

### 3.0 What differs between the pins in P2a's scope

Every P2a function was diffed between the tags (both designers, independently; reproduced in `/work/scratch/p2a-fable` §12 and `/work/scratch/p2a-outbound-opus`). Behaviour-changing differences:

| # | Difference | Keyed | Where P2a reads it |
|---|---|---|---|
| 1 | `PROTOCOL_VERSION` 70240 → 70242, UA `23.1.8` → `24.0.0`, `CLIENT_VERSION` 230108 → 240000, `MIN_MASTERNODE_PROTO_VERSION` (`version.h:14,23`; `configure.ac:3-6`) | `Identity` (P2P-01, -02, -03) | `PushNodeVersion`, common version, UA, stats |
| 2 | Sync clock: `GetAdjustedTime()` (`v23:np:1407, 6225, 6246, 6627`) vs `NodeClock::now()` (`v24:1430, 6370, 6391, 6785`) in `CanDirectFetch`, the sync-start rule, the headers-timeout scaling and the stall rule | `UngatedRules.time_source` (P2P-16; **P2a is a reader**, O17 lists P2b/P3/ND1) | §3.4.2, §3.4.5, §3.9.3 |
| 3 | inv type 34 known at v24 from any peer (`v24:protocol.h:527,555-558`), unknown at v23 (`v23:protocol.h:526`) | `P2pRules.msg_asset_unlock` (P2P-04; **P2a is a reader**) | `inv` unknown-type branch; `getdata`/`notfound` `IsKnownType` (P4/P5 inherit) |
| 4 | `notfound` cap 116 (`v23:np:5602`) vs 50,000 (`v24:5752`) | `P2pRules.notfound_limit` (P2P-10) | NOTFOUND size check |
| 5 | `InitBinds`: any-success (`v23:net.cpp:3963-3983`) vs all-or-abort with `[::]` exempt (`v24:3957-3989`) | `InitRules.bind_failure_fatal` (P2P-33) | `ConnMan::start` option |
| 6 | `m_version_services`/`m_version_relay_txs` recorded (`v24:net.h:820-823`, `np:4060-4061`) | none (DKG-01 reads them; recording is unobservable) | `version` handler always records |
| 7 | devnet-mismatch `Misbehaving` carries the message `"wrong-devnet-name"` at v24 only (`v23:np:4008,4010 / v24:4037,4039`) | none (log text only; wire-identical) | log line |
| 8 | `SyncManager` 1 s tick, 6 s mockable gate: identical at both pins (`v23:node/sync_manager.cpp:146-166 / v24:138-166`) | — | not P2a; fixes O13a's window (§6.1) |
| 9 | ASMap health check at start and every 24 h with `-asmap` (`v24:net.cpp:4115-4116, 5009-5019`; absent at v23): a log line plus two unfiltered `GetAddr` passes (RNG and `vRandom` side effects) | no O17 row (#7121 classed "logging"); proposed `P2pRules.asmap_health_check` (§8 Q6) | P8 |
| 10 | `GetLocator(nullptr)`: v23 substitutes the active tip (`v23:chain.cpp:37-38`), v24 returns an empty locator (`v24:chain.cpp:35-36`). **Reachable** through `ConsiderEviction` on a genesis-only node: `m_work_header` = the active tip (genesis), and after `CHAIN_SYNC_TIMEOUT` the verify-chain-work `getheaders` is built from `m_work_header->pprev` = null (`v23:np:5822-5846 / v24:5968-5993`), so the request carries one locator hash under v23 and none under v24 (G2) | **`P2pRules.null_locator: NullLocator { ActiveTip \| Empty }`** (new field; O17 row proposed, §8 Q12) | `ChainAccess::locator(None)` (§3.4.4, §3.9.1) |
| 11 | linger, inactivity sweep cadence and MN throttle: `SystemClock` ms at v23, `SteadyClock` at v24, same values (#7283) | O17: "not a delta" | steady clock under both profiles (declared, §3.15) |
| 12 | `mncon` start gating (#7659), `TryMarkVerified` cap (P2P-18), `GetEntries`/`filtered` in addrman (hidden `getrawaddrman`) | P7 / unobservable / RP1a | — |

Everything else in §3 holds verbatim at both pins.

### 3.1 The actor loop (`ThreadMessageHandler`, `ProcessMessages`, `PollMessage`)

#### 3.1.1 The loop (`v23:net.cpp:3682-3727 / v24:3676-3721`)

```rust
// processing/peerman.rs — one OS thread; holds no lock across iterations except its own state.
loop {
    if interrupted { return }
    let mut more_work = false;
    // One global steady-clock throttle for masternode connections; the stamp resets only when the check passes
    let skip_mn_send = steady_now - last_mn_send < 100ms;
    if !skip_mn_send { last_mn_send = steady_now }
    self.drain_commands();                                  // PeerManagerHandle commands (unbounded), finalize_node, coalesced chain events; initialize_node is NOT here (synchronous, §2.2)
    let snap = nodes.snapshot_shuffled(&mut rng);           // NodesSnapshot(AllNodes, shuffle=true): a copy of m_nodes, then Shuffle
    for node in snap {
        if node.disconnect() { continue }
        let more_node = self.logic.process_messages(&node); // §3.1.2
        more_work |= more_node && !node.pause_send();       // fMoreWork |= (fMoreNodeWork && !pnode->fPauseSend)
        if interrupted { return }
        if !skip_mn_send || !node.masternode_connection() { self.logic.send_messages(&node) }   // §3.1.4
        if interrupted { return }
    }
    // WAIT_LOCK(mutexMsgProc); if !fMoreWork wait_until(now + 100 ms, fMsgProcWake); fMsgProcWake = false
    let mut g = wake.lock();
    if !more_work { wake.cond.wait_until(&mut g, steady_now + 100ms, |w| w.flag) }
    g.flag = false;
    self.run_due_timers();                                  // §3.1.6: scheduled callbacks only, on the scheduler time base (mockscheduler moves them; node-time deadlines live in send_messages)
}
// `MessageLoop::run_one_pass(now)` is the body above without the wait; the thread is a driver around it (sim determinism, arch m7)
```

- The shuffle is over a **snapshot**: a node added during the pass is first seen next pass; a node removed during the pass is still visited (its `disconnect` flag skips it) (`v24:net.cpp:4943-4959` (=)).
- `fMsgProcWake` is reset after the wait whether or not the thread waited, so a wake during a busy pass is consumed without an extra pass.
- Wake sources: message arrival (`WakeMessageHandler`, `v23:net.cpp:2616,2671`), `UpdatedBlockTip` (`v23:np:2318 / v24:2311`), the writer clearing `pause_send`, RPC pushes.
- Two details A1 §2.14's sketch omits: the `!fPauseSend` factor (a send-paused peer's backlog does not keep the loop spinning), and that the MN throttle timestamp resets only when the check passes (MN connections are serviced on the first pass at or after each 100 ms boundary, never more often).
- The RNG is a `FastRandomContext`-equivalent ChaCha20 stream seeded from OS randomness (explicit in `sim`). The order is observable only statistically (+C, +D).

#### 3.1.2 `process_messages(node)` (`v23:np:5728-5801 / v24:5872-5947`)

1. Look up the peer; none → `false`.
2. Pending `getdata` requests → `Serving::process_getdata` (P4/P5; `EmptyServing` at M2 serves nothing and answers `notfound` for known-type non-block entries, §3.11).
3. The orphan step through `OrphanStep` (P5; P2P-13): v23 `has_more_orphans` (`v23:5745`), v24 `processed_orphan` (`v24:5886`); both return `true` after the disconnect check when set. `NoOrphans` at M2.
4. `node.disconnect` → `false`.
5. `getdata` requests still pending → `true` ("maintains the order of responses").
6. `pause_send` → `false` (no message is polled while the send buffer is over `-maxsendbuffer`).
7. `PollMessage` (§3.1.3); none → `false`.
8. `-capturemessages` hook (P4); `msg.SetVersion(common_version)`.
9. `process_message` (§3.2) inside Core's exception boundary (§3.2.4): a decode error or a handler `Err(Throw)` logs `"ProcessMessages(%s, %u bytes): Exception '%s' (%s) caught"` (`v23:5795 / v24:5941`), no score.
10. After the call: pending `getdata` → `more = true`; v24 only: `m_orphanage.HaveTxToReconsider(peer)` → `more = true` (`v24:5939`, P5's seam).
11. Return `more`.

Only one message per peer per pass; fairness across peers comes from the shuffle.

#### 3.1.3 Two receive queues (`MarkReceivedMsgsForProcessing`, `PollMessage`: `v23:net.cpp:4732-4810 / v24:4773-4850` (=); classification `v23:net.h:2012-2029 / v24:2053-2070`)

A1 §2.14 says the reader queues messages "in arrival order". Core does not process them in arrival order:
- every completed message is classified on arrival: `qsigshare`, `qbsigs`, `qsigsinv`, `qgetsigs`, `qsigsesann`, `qsigrec`, `clsig`, `isdlock` go to the **quorum queue**, everything else to the normal queue;
- `PollMessage` takes from the quorum queue first, unless the normal queue is non-empty and 100 quorum messages have been taken since the last normal one (`QUORUM_TO_NORMAL_RATIO`); a quorum message taken while the normal queue is empty does not count; a normal message resets the counter;
- "more" is true if either queue is non-empty after the pop;
- `fPauseRecv = quorum_bytes + normal_bytes > m_recv_flood_size` is recomputed after every classification batch and every pop; the flood size is `-maxreceivebuffer` × 1000 (5 MB).

Observable: a peer that writes `ping` then `clsig` in one segment gets its `pong` after the CLSIG has been processed. The Rust `Inbox` has the two queues, the counter and the byte totals behind one mutex; the reader classifies on `RawMsg.cmd`; all messages completed by one read are moved together, then one wake (`v24:net.cpp:2611-2617`). While `pause_recv` is set the reader stops reading the socket (`SocketHandlerConnected` `v24:2488-2491`); a pop that clears it notifies the reader. Every queued payload is an owned copy (§2.2), so the flood counter bounds memory as Core's does.

#### 3.1.4 `send_messages(node)` (`v23:np:6154-6738 / v24:6300-6899`): the section order is the order a peer receives messages in one pass

| # | Section | v23 / v24 | Owner |
|---|---|---|---|
| 1 | `MaybeDiscourageAndDisconnect`; acted → return | `:6168` / `:6312` | P2a (§3.10) |
| 2 | not `fSuccessfullyConnected` or `fDisconnect` → return (nothing is sent before `verack` except what the handshake handlers push) | `:6171` / `:6315` | P2a |
| 3 | ADDR_FETCH older than 10 × 30 s → `"addrfetch connection timeout; disconnecting peer=%d"` | `:6179-6183` / `:6323-6327` | P2a |
| 4 | `MaybeSendPing` (may disconnect → return) | `:6185` / `:6329` | P2a (§3.5) |
| 5 | `MaybeSendAddr` | `:6190` / `:6334` | P2a (§3.6.6) |
| 6 | start headers sync: the initial `getheaders2`/`getheaders` | `:6197-6250` / `:6342-6396` | P2a (§3.4.2) |
| 7 | block announcements: `cmpctblock` / `headers2` / `headers` / `inv` | `:6254-6407` / `:6399-6552` | P2a (§3.4.8); P3's encoder |
| 8 | `inv`: queued block invs, tx trickle and `mempool` reply (P5), `vInventoryOtherToSend` (non-tx invs; ISDLOCK only if `m_relay_txs && !IsBlockRelayOnly`), one flush | `:6410-6592` / `:6555-6750` | P2a (blocks, other-inv queue); P5 (tx) |
| 9 | stalling detection, block-download timeout | `:6594-6622` / `:6752-6781` | P3 (`BlockSync::stall_section`; may return) |
| 10 | headers-sync timeout | `:6624-6656` / `:6782-6813` | P2a (§3.4.5) |
| 11 | `ConsiderEviction(GetTime())` | `:6659` / `:6817` | P2a (§3.9.1) |
| 12 | `getdata` for blocks (runs even after section 11 set `fDisconnect`) | `:6662-6682` / `:6820-6838` | P3 (`BlockSync::getdata_section`) |
| 13 | `getdata` for objects (≤ 1000 per message, `MAX_GETDATA_SZ`) | `:6684-6762` / `:6842-6897` | P5 (`ObjectRequests::send_section`) |

Sections 9, 12, 13 sit behind seams at exactly these positions. In v24 section 13 runs after `cs_main` is released and the `getdata`s are flushed together (`v24:6894-6897`); in v23 both happen inside the `cs_main` block (`v23:6758-6762`); the resulting message order is the same (blocks first, then objects), so one implementation serves both.

#### 3.1.5 Awaiting observable work (A1 M-5)

| Message | What is awaited | Core |
|---|---|---|
| `headers`, `headers2` | `ChainAccess::process_headers` → `HeadersResult` with fresh `TipFacts` | `ProcessNewBlockHeaders` runs synchronously in `msghand` (`v23:3378 / v24:3362`); any peer's next `getheaders` must see the new headers |
| `version` (outbound) | `AddrMan::set_services`, `AddrMan::good` (synchronous mutex calls) | `v23:3932, 4120 / v24:3961, 4149` |
| `addr`, `addrv2` | `AddrMan::add` (synchronous) | `v23:4429 / v24:4474` |

While the actor waits, every other peer waits, as behind `msghand`. Nothing in P2a spawns work whose completion a later message could observe.

#### 3.1.6 Timers the actor owns

| Timer | Period | Core |
|---|---|---|
| `CheckForStaleTipAndEvictPeers` | `EXTRA_PEER_CHECK_INTERVAL` 45 s | scheduler (`v23:np:2171 / v24:2150`); it takes `cs_main` and touches peer state, so it runs on the actor at the first loop boundary after it is due (an interleaving Core allows). Its due time is on the scheduler time base, so `mockscheduler` advances it as in dashd (arch m4) |
| `ReattemptInitialBroadcast` | 10–15 min | `v24:1762-1781`; P5 |

The wait is shortened to the next due timer, so a timer never fires more than one pass late.

### 3.2 Gates and the per-command decode policy (`ProcessMessage` prologue and epilogue)

#### 3.2.1 Gate order (`v23:np:3889-4341 / v24:3918-4386`, identical logic)

| # | Gate | Outcome | v23 / v24 |
|---|---|---|---|
| 0 | log `"received: %s (%u bytes) peer=%d"`; statsd `message.received.<cmd>` | — | `:3898` / `:3927` |
| 1 | `version` while `nVersion != 0` | `"redundant version message from peer=%d"`, return before reading | `:3906-3910` / `:3935-3939` |
| 2 | any other message while `nVersion == 0` | `"non-version message before version handshake. Message \"%s\" from peer=%d"`, return | `:4147-4151` / `:4178-4182` |
| 3 | `verack` | handled (§3.3.6); a repeat logs `"ignoring redundant verack message from peer=%d"` | `:4156-4214` / `:4187-4259` |
| 4 | `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2`, `sendtxrcncl` | handled before or after `verack` (§3.3.7) | `:4216-4316` / `:4261-4361` |
| 5 | anything else before `verack` | `"Unsupported message \"%s\" prior to verack from peer=%d"`, return, no score | `:4318-4321` / `:4363-4366` |
| 6 | first message after `verack` | record `nTimeFirstMessageReceived`, `fFirstMessageIsMNAUTH`; an MN probe whose first message is not `mnauth` is disconnected. Only messages that reach `ProcessMessage` count: a frame P1 dropped for invalid command bytes or a bad checksum never sets these (BS4a-r2 N-4) | `:4323-4334` / `:4368-4379` |
| 7 | `RejectIncomingTxs(peer) && NetMessageViolatesBlocksOnly(cmd)` | `"%s sent in violation of protocol peer=%d"`, disconnect, nothing read | `:4337-4341` / `:4382-4386` |
| 8 | the command's own branch | §3.2.3, §3.11 | |
| 9 | a listed command with no branch | the extension dispatch (§3.2.6); silent if nobody handles it | `:5627-5681` / `:5769-5825` |
| 10 | an unlisted command | `"Unknown command \"%s\" from peer=%d"`, ignore | `:5684` / `:5828` |

`NetMessageViolatesBlocksOnly` is the 26-command set `dsa dsc dsf dsq dss dssu dstx dsi getqrinfo platformban qbsigs qcomplaint qcontrib qdata qgetdata qgetsigs qjustify qpcommit qsendrecsigs qsigrec qsigsesann qsigshare qsigsinv qrinfo qwatch tx` (`protocol.cpp:185-212`, identical). `clsig`, `isdlock`, `spork`, `govobj`, `govobjvote` are not in it. `RejectIncomingTxs` holds for block-relay-only and feeler connections and under `-blocksonly` without the `relay` permission (`v23:6127-6135 / v24:6273-6281`). Gate state (`version`, `successfully_connected`, the first-message fields) lives in `NodeHandle` atomics because ConnMan's inactivity check reads `successfully_connected`.

#### 3.2.2 `policy.rs`: BS4a's pre-read columns are normative; post-read outcomes come from Core

Core has no separate decode step: each branch reads from the stream where its code says `vRecv >> …`, and side effects before the read happen even when the read later throws. Handlers call the base-sdk decoder **at Core's read point**. `policy.rs` is the specification and the test oracle for that placement:

```rust
pub struct CommandPolicy {
    pub cmd: CommandString,
    pub gate: Gate,                  // AnyTimeAfterVersion | BeforeOrAfterVerack | PostVerack (default)      — BS4a §4.0 (normative)
    pub pre_read: &'static [PreRead],// effects before Core's first read (e.g. HeadersClearGetheadersTimestamp) — BS4a "Before the read" (normative)
    pub read: ReadPlan,              // Nothing | Whole(type) | Head{…}Tail{…} | CountThenItems{limit} | Custom  — BS4a "Handler reads" (normative)
    pub on_throw: ThrowOutcome,      // CaughtByProcessMessages | Handler{score, msg} | Disconnect                — BS4a "Decode failure" (normative)
    pub trailing: Trailing,          // Ignored, except the two v24 rules (DKG whole-payload, qgetdata 67 bytes)  — BS4a (normative)
    pub blocks_only_violation: bool, // NetMessageViolatesBlocksOnly                                            — normative
    pub quorum_priority: bool,       // IsQuorumPriorityMessage                                                 — normative
    pub post_read: PostRead,         // the handler's outcome after a successful read: RE-DERIVED FROM CORE AT BOTH PINS
                                     // and cited per row (§3.11); DDE-SWEEP §4's "After the read" column is informative only
    pub owner: Owner,                // P2a | P2b | P3 | P4 | P5 | P5a | P6a | P6b | SP0 | SP1 | P7 | AM1 | DK1 | …
    pub m2: M2Behaviour,             // §3.11: Exact | Stub | Deviation { closes: Owner }
}
pub static POLICY: [CommandPolicy; 74];
```

Tests assert that the table has exactly Core's 74 commands (`allNetMessageTypes`, `protocol.cpp:99-176` at both pins), that `blocks_only_violation` and `quorum_priority` equal Core's two sets, that every pre-read column equals BS4a's `decode_domain.json5` corpus verdicts, and that every `post_read` of a row P2a owns is covered by a `diff_p2p_<cmd>` case. **Rows whose owner is a later task carry `post_read: OwnerDerives`**; P2a implements only the M2 interim stub of §3.11 for them. Rows of DDE-SWEEP §4 still under BS4a rework that P2a depends on: the `headers` row's minor note (desirable services depend on IBD, which P2a's `version` handler already models through `GetDesirableServiceFlags`), the gate-4 / N-4 fix (command-byte validation moves to P1's transport; P2a assumes dropped frames never reach the gates), and the DKG intake shapes (N-1(i), BS9a's; P2a's +10 fallback is unaffected).

#### 3.2.3 The M2 command set (P2a's handlers): pre-read (normative) and post-read (from Core)

| Command | Pre-read (BS4a §4) | Read | Throw | Post-read, in Core's order (cited) |
|---|---|---|---|---|
| `version` | gate 1 | head: i32 version, u64 services (`CustomUintFormatter<8>`), i64 time, 8 skipped, `CService` addrMe; then each optional group only while bytes remain | caught; a throw in the tail happens after `SetServices`, the services check and the obsolete-version check (`VersionTail { head }`) | §3.3.5 (`v23:3906-4145 / v24:3935-4176`) |
| `verack` | — | nothing | — | §3.3.6 |
| `sendheaders`, `sendheaders2` | gate 4 | nothing | — | `fPreferHeaders` / `fPreferHeadersCompressed` = true (`v23:4216-4226 / v24:4261-4271`) |
| `sendcmpct` | gate 4 | `CoreBool` hb, u64 version | caught | version ≠ 1 → ignore; else `m_provides_cmpctblocks = true`, `m_requested_hb_cmpctblocks = hb`, `m_bip152_highbandwidth_from = hb` (`v23:4228-4243 / v24:4273-4288`) |
| `sendaddrv2` | gate 4 | nothing | — | common < 70223 → ignore; after `verack` → `"sendaddrv2 received after verack from peer=%d; disconnecting"`, disconnect; else `m_wants_addrv2 = true` (`v23:4247-4260 / v24:4292-4305`) |
| `sendtxrcncl` | gate 4; txreconciliation off (default) → debug log, nothing read | u32, u64 (only when on) | caught | off path only in rdashd by default; the on path is BS7's row and P5's (`v23:4265-4316 / v24:4310-4361`) |
| `senddsq` | gate 5 | `CoreBool` | caught | `m_wants_dsq` = NONE / ALL (common < 70234) / INV (`v23:4440-4452 / v24:4485-4497`) |
| `qsendrecsigs` | gate 5, 7 | `CoreBool` | caught | `m_wants_recsigs = b` (`v23:4455-4460 / v24:4500-4505`) |
| `ping` | gate 5 | u64 nonce | caught (no `pong`) | push `pong(nonce)` at once (`v23:5379-5395 / v24:5456-5472`) |
| `pong` | gate 5 | u64 if ≥ 8 bytes remain | never throws | §3.5 (`v23:5397-5450 / v24:5474-5527`) |
| `addr`, `addrv2` | gate 5 | `vector<CAddress>` (v1 / BIP155) | caught | §3.6.3: relay not set up → `"ignoring %s message from %s peer=%d"`; > 1000 → +20 `"%s message size = %u"`; token bucket; per-address rules; `addrman.Add`; `getaddr_sent = false` if < 1000; ADDR_FETCH with > 1 → disconnect (`v23:4343-4437 / v24:4388-4482`) |
| `getaddr` | gate 5 | nothing | — | outbound → `"Ignoring \"getaddr\" from %s connection. peer=%d"`; inbound (P2b): §3.6.4 (`v23:5312-5347 / v24:5389-5424`) |
| `inv` | gate 5 | `vector<CInv>` | caught | > 50,000 → +20 `"inv message size = %u"` (`v23:4467 / v24:4512`); per entry §3.4.7 (`v23:4462-4566 / v24:4507-4611`) |
| `getdata` | gate 5 | `vector<CInv>` | caught | > 50,000 → +20 `"getdata message size = %u"` (`v23:4574 / v24:4618`); queued for `ProcessGetData` (`EmptyServing` at M2; §3.11) |
| `notfound` | gate 5 | `vector<CInv>` | caught | > `P2pRules.notfound_limit` → +20 `"notfound message size = %u"` (`v23:5602-5604 / v24:5752-5754`); else `ObjectRequests::received_response` per known-type entry (`v23:5606-5619 / v24:5756-5764`; `NoObjectRequests` at M2) |
| `headers`, `headers2` | gate 5; `LoadingBlocks()` → `"Unexpected headers message received from peer %d"`, return; then **clear `m_last_getheaders_timestamp`** | count (`ReadCompactSize`); > limit → +20 before any header is read; then 80-byte headers + ignored tx count, or compressed headers with a per-message version list | caught (after the clear) | §3.4.3 (`v23:5232-5269 / v24:5309-5346`; `ProcessHeadersMessage` `v23:3338-3402 / v24:3322-3386`) |
| `getheaders`, `getheaders2` | gate 5 | locator, stop hash | caught | locator > 101 → disconnect (`v23:4715-4719 / v24:4765-4769`); then `LoadingBlocks()` → ignore; serving is P2b |
| unknown | gate 2/5 first | nothing | — | `"Unknown command"` (`v23:5684 / v24:5828`) |

The `headers` row is the case that motivates reading at Core's point: a `headers2` whose count is valid but whose third compressed header is truncated still clears the peer's last-`getheaders` timestamp, so the next `getheaders` to that peer can go out at once.

#### 3.2.4 The exception boundary

Rust handlers return `Result<(), Throw>` where `Throw { what: Cow<'static, str>, kind }`. The boundary formats Core's line (`v23:5795 / v24:5941`); `what` reproduces Core's text for decoder failures (`"DataStream::read(): end of data"` `v23:streams.h:268 / v24:287`; `"ReadCompactSize(): size too large"`, `"non-canonical ReadCompactSize()"` `v24:serialize.h:304-319`; `"String length limit exceeded"` `v24:serialize.h:781`; libstdc++ appends `": iostream error"`), mapped from `P2pDecodeError` by one function with a test per variant. Asserted log fragments (A1 §2.18) are checked by O11; the rest is best effort (a peer cannot read our log). Handlers that catch their own exceptions in Core (`filterload`, `filteradd`, `govsync` filter, `govobjvote`, `spork`, `getqrinfo` at v24, sig-share and DKG intake) are later tasks' rows: `on_throw: Handler{…}` and the dispatcher hands them the decode error.

#### 3.2.5 `ProtoCtx`

Each read builds `ProtoCtx { version: node.common_version(), bls: snap.bls }` at the read point, from the `ChainReadSnapshot` the actor took for this message (R8; g2). The version is `min(peer, Identity.protocol_version)` after `version`, `INIT_PROTO_VERSION` 209 before (`version.h:17`). None of P2a's commands has a BLS field; P2a only plumbs `BlsCtx::for_tip`. A peer's `ProtoCtx` is never cached across messages.

#### 3.2.6 The extension dispatch and the `NetHandler` registry

A listed command no inline branch handles goes to the Dash extension dispatch after every generic branch (`v23:5627-5681 / v24:5769-5825`). Both pins have `NetHandler` (`Start`, `Stop`, `Interrupt`, `Schedule`, `ProcessMessage`, `AlreadyHave`, `ProcessGetData`; `AddExtraHandler`, registration order). They differ in which subsystems are handlers:

| Step | v23.1.8 | v24.0.0-rc.3 |
|---|---|---|
| a | CoinJoin wallet manager (every wallet-enabled non-masternode build, loaded wallet or not, `v24:init.cpp:2184-2191`) | same |
| b | masternode: sig-share manager, then DKG manager | (moved into handlers) |
| c | observer (`-watchquorums`): DKG manager | (moved into handlers) |
| d | neither: `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit`/`qwatch` → +10 (`v23:5650-5659`) | `NetDKGStub`, same +10 (`v24:llmq/net_dkg.cpp:938-944`) |
| e | spork manager (`spork`, `getsporks`) | inline branches (`v24:5689-5736`) |
| f | MNAUTH | same |
| g | quorum block processor (`qfcommit`) | same (with P2P-12's callback) |
| h | quorum manager (`qgetdata`, `qdata`) | `NetQuorum` |
| i | platform ban | same |
| j | `clsig`, then **return** | same, with P2P-12's gate |
| k | `m_handlers`: `NetInstantSend`, `NetSigning`, `CCoinJoinServer` (MN), `NetGovernance` (when enabled), `SyncManager` (`v23:init.cpp:2213-2262`) | `NetInstantSend`, `NetSigning`, `NetQuorum`, `NetDKG`/`NetDKGStub`, `CCoinJoinServer` (MN), `NetGovernance` (always), `SyncManager` (`v24:init.cpp:2208-2281`) |

Each command is consumed by exactly one subsystem at either pin, so only who handles what, and the `MessageProcessingResult` application order, are observable. The Rust registry is a map `CommandString → handler` filled by ND1 in each release's order (double registration panics at startup), plus P2a's per-release fallbacks for the M2 node: +10 for the five DKG commands under both profiles, silence for every other unowned listed command. The trait (§2.4) mirrors Core's; A1's `on_send_tick`/`on_connected`/`on_disconnected` do not exist at either pin and are dropped (§3.15).

### 3.3 Handshake and `fingerprint.rs`

#### 3.3.1 Identity per profile

| Field | `V23_1` | `V24_0` | Core |
|---|---|---|---|
| `version.nVersion` / `Identity.protocol_version` | 70240 | 70242 | `version.h:14` (P2P-01) |
| user agent | `/Dash Core:23.1.8/` | `/Dash Core:24.0.0/` | `FormatSubVersion(CLIENT_NAME, CLIENT_VERSION, comments)`, `clientversion.cpp:69-84` (=) |
| `client_version` | 230108 | 240000 | `FormatVersion` `clientversion.cpp:55-58` (=) |
| `min_mn_proto_version` | 70240 | 70242 | `version.h:23` (P2P-02) |
| `INIT_PROTO_VERSION` / `MIN_PEER_PROTO_VERSION` / `ADDRV2_PROTO_VERSION` / `INCREASE_MAX_HEADERS2_VERSION` | 209 / 70221 / 70223 / 70235 | same | `version.h:17,20,26,50` (=) |

A unit test asserts `ua_version == FormatVersion(client_version)`. **The locator carries a version word per profile** (core m-5): `CBlockLocator` serialises an `int nVersion` taken from the stream version (`primitives/block.h` v23:249-255 / v24:255-261), which `CNetMsgMaker` sets to the common version; against a 70242 peer every `getheaders`/`getheaders2`/`getblocks` we send starts with `70240` under `V23_1` and `70242` under `V24_0`. `fingerprint.rs`'s encoder contract states it and O13a's fixtures note it. Every send-side version gate (`sendaddrv2` ≥ 70223, the `headers2` 8000 limit ≥ 70235, short IDs ≥ 70235 / 70240, `dsa` flags ≥ 70241, `MSG_ASSET_UNLOCK` announcements ≥ 70242, DSQ as inv ≥ 70234) compares against the common version and so follows the profile with no extra field (P2P-03).

#### 3.3.2 User agent (`v23:init.cpp:1790-1807 / v24:1827-1844` (=))

1. comments start with `devnet.<name>` on a devnet;
2. each `-uacomment` must equal its `SanitizeString(…, SAFE_CHARS_UA_COMMENT)`, else InitError `"User Agent comment (%s) contains unsafe characters."`;
3. `"/Dash Core:" + FormatVersion(client_version) + ("(" + join(comments, "; ") + ")")? + "/"`;
4. longer than 256 (`MAX_SUBVERSION_LENGTH`, `net.h` v23:88 / v24:86) → InitError `"Total length of network version string (%i) exceeds maximum length (%i). Reduce the number or size of uacomments."`.

`-honestua` (DEC-04) replaces `Dash Core:<ua_version>` with `rdashd:<crate version>`, keeps the comment rules, is off by default and logged at startup; the true identity is in `-version`, the `debug.log` banner and `getimplementationinfo`.

#### 3.3.3 Local services (`init.cpp` v23 / v24)

| Bit | Value | When | v23 / v24 |
|---|---|---|---|
| `NODE_NETWORK_LIMITED` \| `NODE_HEADERS_COMPRESSED` | 0x0C00 | always | `:1077` / `:1125` |
| `NODE_P2P_V2` | 0x1000 | `-v2transport` (default 1) | `:1187-1189` / `:1235-1237` |
| `NODE_COMPACT_FILTERS` | 0x40 | `-peerblockfilters` (default 0; soft-set on masternodes) with the basic filter index, else InitError `"Cannot set -peerblockfilters without -blockfilterindex."` | `:1192-1198` / `:1240-1246` |
| `NODE_BLOOM` | 0x04 | `-peerbloomfilters` (default 1) | `:1386-1387` / `:1370-1371` |
| `NODE_NETWORK` | 0x01 | not pruned, after the block index loads | `:2313-2314` / `:2353-2354` |

Defaults: 0x1C05 node, 0x1C45 masternode, 0x1C04 pruned, 0x0C05 with `-v2transport=0`. O17 P2P-35 (v23 lets an MN's explicit `-peerblockfilters=0` win → 0x1C05; v24 refuses to start) is `InitRules.mn_init`; P2a consumes the result. `GetDesirableServiceFlags(s)` = `NODE_NETWORK_LIMITED` if `s` has it and the IBD cache is complete, else `NODE_NETWORK` (`protocol.cpp:252-257` (=)); the cache is set by `UpdatedBlockTip` (`v23:2283 / v24:2276`) and kept in an `AtomicBool` ConnMan and the `version` handler read.

#### 3.3.4 Our `version` (`InitializeNode` `v23:1779-1793 / v24:1745-1760`; `PushNodeVersion` `v23:1538-1572 / v24:1561-1593` (=))

Pushed by `initialize_node` for outbound nodes before any byte arrives (behaviour 3), framed at `INIT_PROTO_VERSION` 209:

| Field | Bytes | Value |
|---|---|---|
| `nVersion` | 4 | `Identity.protocol_version`, or `-pushversion` on a non-mainnet network (advertised only; the common version still uses the profile value) |
| services | 8 | `m_our_services` |
| time | 8 | `GetTime()` seconds: mockable, **not** adjusted, at both pins (`v24:1564`) |
| addr_recv | 26 | services = `addr.nServices` (addrman's belief: `NODE_NETWORK` for seeds, `NODE_NONE` for names); address = the peer's if `IsRoutable() && !IsProxy() && IsAddrV1Compatible()`, else `[::]:0` (`v24:1570`) |
| addr_from | 26 | our services; `[::]:0` |
| nonce | 8 | `nLocalHostNonce` = SipHash(process key, `RANDOMIZER_ID_LOCALHOSTNONCE` 0xd93e69e2bbfa5735 ‖ node id) (`net.cpp` v23:124,491 / v24:125,492) |
| user agent | 1+18 | §3.3.2 |
| start_height | 4 | `m_best_height`: set at init (`v23:init.cpp:2534 / v24:2601`) and by every `UpdatedBlockTip`, so a `version` built between a connect and a queued callback carries the pre-connect height |
| relay | 1 | `!RejectIncomingTxs(node)`: 0 for block-relay-only and feeler connections and under `-blocksonly` (`v24:1582`). At M2 rdashd has no mempool but still advertises `relay=1` as dashd would (Fable Q2; nothing in Core ties the flag to mempool state) |
| mnauth challenge | 32 | fresh `GetRandBytes`, stored as the sent challenge |
| fMasternode | 1 | `node.m_masternode_connection` (P7) |

The default fixture is 137 bytes (4+8+8+26+26+8+19+4+1+32+1). Log: `"send version message: version %d, blocks=%d, txrelay=%d, peer=%d"` (`them=%s` under `-logips`).

#### 3.3.5 Processing the peer's `version` (`v23:3906-4145 / v24:3935-4176`; v24 also records `m_version_services`/`m_version_relay_txs`, `v24:4060-4061`)

1. `nVersion != 0` → `"redundant version message"`, return (gate 1).
2. Read `nVersion`, services, time (negative → 0, `v23:3922-3924 / v24:3951-3953`), skip 8, read addrMe. A throw here leaves the peer pre-version, logged.
3. Outbound only: `addrman.set_services(node.addr, services)` (`v23:3932 / v24:3961`).
4. `ExpectServicesFromConn()` (full-relay, block-relay, addr-fetch; not manual or feeler) and `!HasAllDesirableServiceFlags(services)` → `"peer=%d does not offer the expected services (%08x offered, %08x expected); disconnecting"`, disconnect (`v23:3934-3939 / v24:3963-3968`).
5. `nVersion < 70221` → `"peer=%d using obsolete version %i; disconnecting"`, disconnect (signed; −1 disconnects) (`v23:3942-3947 / v24:3971-3976`).
6. Optional groups, each only if bytes remain: skip 26 + nonce; subver (`LIMITED_STRING` 256, then `SanitizeString`); start height; relay (`CoreBool`); mnauth challenge (stored); `fMasternode` (`CoreBool`). Inbound with `fMasternode` sets `m_masternode_connection`; if we are not a masternode → log and disconnect (`v23:3971-3983 / v24:4000-4012`). A throw in any group ends here; steps 3–5 have happened (PQ-1).
7. Inbound with a nonce equal to one of our outbound nonces → `"connected to self at %s, disconnecting"` (`v23:3987-3992 / v24:4016-4021`). Inbound with a routable addrMe → `SeenLocal(addrMe)`.
8. Inbound: `PushNodeVersion` now ("be shy") (`v23:4001 / v24:4030`).
9. Devnet: subver must contain `devnet.<name>`, else `"connected to wrong devnet. Reported version is %s, expected devnet name is %s"`, +100 outbound / +1 inbound (message `"wrong-devnet-name"` at v24 only), disconnect (`v23:4004-4014 / v24:4033-4043`).
10. `common = min(nVersion, Identity.protocol_version)`; store common and `nVersion` (`v23:4017-4019 / v24:4046-4048`).
11. Push `sendaddrv2` iff `common ≥ 70223` (`v23:4022-4028 / v24:4051-4057`).
12. Record services, `m_has_all_wanted_services`, relay, addrMe (`SetAddrLocal`), subver, starting height. Create the tx-relay slot unless block-relay-only or feeler, when `relay || our NODE_BLOOM` (`SetTxRelay`, `v23:4048 / v24:4079`; P5 fills it).
13. `sendtxrcncl` only if txreconciliation is on and the peer qualifies (`v24:4087-4101`; off by default).
14. Push `verack` (`v23:4074 / v24:4103`).
15. `fPreferredDownload = (outbound || noban) && !addr_fetch && CanServeBlocks(peer)`; update the global count (`v23:4077-4082 / v24:4106-4111`).
16. Outbound: `SetupAddressRelay` (false only for block-relay-only); if true push `getaddr`, `m_getaddr_sent = true`, `m_addr_token_bucket += 1000` (`v23:4087-4103 / v24:4116-4132`). **Feelers, manual and addr-fetch included** (PQ-14).
17. Outbound: `addrman.good(node.addr)` (`v23:4120 / v24:4149`), every outbound type including block-relay and feelers (PQ-18).
18. Log `"receive version message: %s: version %d, blocks=%d, us=%s, txrelay=%d, peer=%d%s%s"`.
19. `nTimeOffset = nTime − GetTime()`, stored on the node; outbound only: `AddTimeData(node.addr, nTimeOffset)` (`v23:4136 / v24:4167`; §3.3.8).
20. Feeler → `"feeler connection completed peer=%d; disconnecting"`, disconnect (`v23:4141-4145 / v24:4170-4174`). The queued `sendaddrv2`, `verack`, `getaddr` are flushed by the linger (§3.8.7).

#### 3.3.6 `verack` (`v23:4156-4214 / v24:4187-4259` (=))

1. Already connected → `"ignoring redundant verack message from peer=%d"`.
2. Log `"New %s %s peer connected: version: %d, blocks=%d, peer=%d%s%s"`.
3. Masternode and not a probe: push `mnauth` (AM1's payload; P2a's slot).
4. Push `sendheaders2` if the peer's version services have bit 11, else `sendheaders` (`UsesCompressedHeaders`, `v23:1180 / v24:1196`).
5. `CanRelay()` → push `sendcmpct(false, 1)`.
6. `!RejectIncomingTxs` → push `senddsq(true)`; then, inside the same branch, `qwatch` if watching and the peer is a quorum node (`v23:4193-4201 / v24:4224-4232`).
7. Txreconciliation bookkeeping (off).
8. `fSuccessfullyConnected = true`.

The next `send_messages` for the node, in the same pass, sends `ping` (`m_ping_start` starts at 0), an `addr` if anything is queued (§3.6.6), and the initial `getheaders2`/`getheaders` if the sync conditions hold (§3.4.2). The complete outbound transcript of a non-masternode with default options:

```
→ version                                   (at connect)
                     ← version [← sendaddrv2] ← verack
→ sendaddrv2 (common ≥ 70223)  → verack  → getaddr (unless block-relay-only)
→ sendheaders2 | sendheaders  → sendcmpct(0,1) (unless a non-IQR MN connection)
→ senddsq(1) (unless block-relay-only/feeler/-blocksonly)  [→ qwatch (watching, quorum peer)]
→ ping  [→ addr|addrv2]  [→ getheaders2 | getheaders]
```

A1 §2.14 / §4.10 #5 and PRD #5 put `qwatch` after `ping` and `getheaders`; at both pins it is sent in the `verack` handler right after `senddsq` (§3.15). Feelers: `version`, then `sendaddrv2`, `verack`, `getaddr`, disconnect.

#### 3.3.7 The pre/post-`verack` control messages

| Message | Before `verack` | After `verack` | v23 / v24 |
|---|---|---|---|
| `sendheaders` / `sendheaders2` | sets the preference | sets the preference | `:4216-4226` / `:4261-4271` |
| `sendcmpct` | read; version ≠ 1 ignored; sets the three flags | same | `:4228-4243` / `:4273-4288` |
| `sendaddrv2` | common < 70223 → ignore; else `m_wants_addrv2` | disconnect (common ≥ 70223) | `:4247-4260` / `:4292-4305` |
| `sendtxrcncl` | off → `"sendtxrcncl from peer=%d ignored, as our node does not have txreconciliation enabled"`, nothing read | same when off | `:4265-4316` / `:4310-4361` |

#### 3.3.8 Time offsets (`timedata.cpp:46-111` (=); only `GetAdjustedTime`'s return type differs)

Ignore a source already counted (keyed by `CNetAddr`, port ignored) or once 200 sources exist (`:50`); push into a 200-sample median filter whose initial sample is 0; with ≥ 5 samples and an odd count, set the offset to the median if `|median| ≤ -maxtimeadjustment` (4200 s), else 0 and, once, the clock warning if no sample is within ±5 min. The 200-sample freeze (200 is even, so the offset never changes again once the filter is full) is PQ-6. Under `V23_1` the offset feeds the consensus clock (`TimeSource::AdjustedTime`: `time-too-new`, the miner, the four sync decisions), under `V24_0` only `getpeerinfo.timeoffset`, `getnetworkinfo.timeoffset` and the warning. Lives in `dashd-clock` (§2.7).

### 3.4 Headers and headers2 sync

#### 3.4.1 State

| Per peer (`PeerState`) | Core | Global (`PeerLogic`) | Core |
|---|---|---|---|
| `sync_started` | `CNodeState::fSyncStarted` | `n_sync_started` | `nSyncStarted` |
| `headers_sync_timeout` (µs; `MAX` = disarmed) | `m_headers_sync_timeout` | `n_preferred_download` | `m_num_preferred_download_peers` |
| `last_getheaders` (NodeClock) | `Peer::m_last_getheaders_timestamp` | `last_block_inv_triggering_sync` | `m_last_block_inv_triggering_headers_sync` |
| `unconnecting_headers` | `nUnconnectingHeaders` | `outbound_protected` | `m_outbound_peers_with_protect_from_disconnect` |
| `best_known_block: Option<BlockId>` | `pindexBestKnownBlock` | `best_height` | `m_best_height` |
| `last_unknown_block: Option<BlockHash>` | `hashLastUnknownBlock` | `initial_sync_finished` | `m_initial_sync_finished` |
| `best_header_sent: Option<BlockId>` | `pindexBestHeaderSent` | `ibd_service_cache` | `g_initial_block_download_completed` |
| `inv_triggered_getheaders_before_sync` | `Peer::…` | `last_tip_update` | `m_last_tip_update` (lazily `GetTime()`) |
| `last_block_announcement` (s) | `m_last_block_announcement` | `stale_tip_check_time` | `m_stale_tip_check_time` |
| `prefer_headers`, `prefer_headers_compressed` | `fPreferHeaders`, `fPreferHeadersCompressed` | | |
| `chain_sync: ChainSyncTimeoutState` | `m_chain_sync` | | |
| `blocks_for_headers_relay`, `blocks_for_inv_relay` | `Peer::m_blocks_for_*` | | |

`m_node_states` is a `BTreeMap<NodeId, …>` (Core's `std::map<NodeId>`, creation order). `best_known_block` and `best_header_sent` are `BlockId`s into CH1's arena.

#### 3.4.2 Starting sync (section 6; `v23:6197-6250 / v24:6342-6396`)

```
assert best_header is set                                                     // CH1 requirement R5 (§2.5.3): never null after load; P2a does not write it
sync_from_peer = preferred_download
              || (CanServeBlocks(peer) && !addr_fetch && (n_preferred_download == 0 || !block_sync.any_in_flight()))
if !sync_started && CanServeBlocks(peer) && !loading_blocks && node.CanRelay():
    if (n_sync_started == 0 && sync_from_peer) || best_header.time > now_sync − max_tip_age:
        start = best_header.prev or best_header
        if maybe_send_getheaders(peer, getheaders2 if peer bit 11 else getheaders, locator(start)):
            log "initial %s (%d) to peer=%d (startheight:%d)"
            sync_started = true; n_sync_started += 1
            headers_sync_timeout = now_us + 15 min + (1000 µs × secs(now_sync − best_header.time)) / pow_target_spacing   // multiply before the integer division (core n-2; v24:6386-6390)
```

`CanServeBlocks(peer)` = `their_services & (NODE_NETWORK | NODE_NETWORK_LIMITED)` (`v24:1190-1193` (=)); `max_tip_age` = `-maxtipage`, 6 h. **Profile:** `now_sync` is `GetAdjustedTime()` under `V23_1` (`v23:6225, 6246`) and `NodeClock::now()` under `V24_0` (`v24:6370, 6391`); `now_us` is the mockable `GetTime()` at both. On a fresh node exactly one outbound peer gets the initial `getheaders2`; a second only when the best header is within 6 h of now or the first sync peer is gone.

#### 3.4.3 Processing `headers`/`headers2` (branch `v23:5232-5269 / v24:5309-5346`; `ProcessHeadersMessage` `v23:3338-3402 / v24:3322-3386` (=))

1. `loading_blocks` → log, return (nothing read; the timestamp is kept).
2. `last_getheaders = 0` (any `headers`/`headers2`, even empty or malformed, re-arms `MaybeSendGetHeaders`; PQ-2).
3. Read the count; count > `GetHeadersLimit(common, msg_type == headers2)` → +20 `"headers message size = %u"` (also for `headers2`; PQ-4), return. `GetHeadersLimit` is 8000 for compressed at common ≥ 70235, else 2000 (`v23:1194-1200 / v24:1210-1216`); **this size check is keyed by the message type received** (`v23:5247 / v24:5324`), while steps 10–11 are keyed by the peer's bit 11 (PQ-24).
4. Read the headers: `headers` = 80 bytes + an ignored tx count each, **no hashing**; `headers2` = compressed headers decompressed against this message's headers with a fresh version list (`last_unique_versions` is local to the branch, `v24:5336`), so compression state never crosses messages; the decoder computes `X11(previous)` only for a header whose prev-hash flag is set, exactly where Core's `Uncompress` does (`block.cpp:122`), and memoises it in the batch's hash slot (§3.4.9).
5. Empty → return ("stop asking this peer").
6. `lookup(headers[0].prev)` is none (a field compare, no hash, `v24:3337`): count ≤ 8 → `HandleFewUnconnectingHeaders` (`v23:3154-3181 / v24:3138-3165`: `unconnecting_headers += 1`; `maybe_send_getheaders(locator(best_header))` with `"received header %s: missing prev block %s, sending %s (%d) to end (peer=%d, nUnconnectingHeaders=%d)"`, which hashes `headers[0]` only when the log fires; `UpdateBlockAvailability(last hash)`, which hashes `headers.back()`; every 10th → +20 `"%d non-connecting headers"`); else +10 `"invalid header received"` (`v23:3362 / v24:3346`) **with no hash beyond those step 4's `Uncompress` already computed**; return.
7. Continuity, as an **incremental loop that stops at the first mismatch** (`CheckHeadersAreContinuous`, `v23:3183-3193 / v24:3167-3177`: for each header, first compare `header.prev` against the previous header's hash and return on mismatch, **then** hash this header, reusing a memoised `Uncompress` hash if there is one): a mismatch at position k costs k−1 hashes, as in Core (a mismatch at header 2 costs one). Mismatch → +20 `"non-continuous headers sequence"` (`v23:3369 / v24:3353`), return. Only when the loop completes is `HeaderBatch.hashes` complete (G1; R7).
8. `received_new_header = lookup(hashes.back()).is_none()` (`v23:3375 / v24:3359`).
9. `process_headers(batch)` (awaited). Invalid → `MaybePunishNodeForBlock(result, via_compact_block=false, "invalid header received")` (§3.10.3), return. Headers accepted before the failing one stay accepted. A non-invalid failure falls through with `last_accepted` as Core's `pindexLast` (`v23:3378-3383 / v24:3362-3367`); see §3.4.4 for the null case.
10. `count == GetHeadersLimit(common, UsesCompressedHeaders(peer))` — **the limit for the peer's advertised bit 11, not for the message type received** (`uses_compressed = UsesCompressedHeaders(peer)`, `v23:3386-3396 / v24:3370-3380`; core M-1, arch m9; PQ-24) → `maybe_send_getheaders(locator(last_accepted))`, `"more %s (%d) to end to peer=%d (startheight:%d)"`. So a bit-11 peer at common ≥ 70235 that answers `getheaders2` with a plain `headers` of 2000 gets no "more" request (2000 ≠ 8000), and a non-bit-11 peer that sends `headers2` of 8000 at ≥ 70235 passes step 3 but is treated as short here (limit 2000).
11. `UpdatePeerStateForReceivedHeaders` (`v23:3283-3336 / v24:3267-3320`) with `may_have_more_headers = (count == that same bit-11 limit)`: reset `unconnecting_headers` (logging `"peer=%d: resetting nUnconnectingHeaders (%d -> 0)"` if > 0); `UpdateBlockAvailability(last)`; a new header with more work than the tip sets `last_block_announcement = GetTime()`; in IBD with a short message (by the bit-11 limit) and the peer's best known chain below `MinimumChainWork` → outbound-or-block-relay peer: `"Disconnecting outbound peer %d -- headers chain has insufficient work"`, disconnect; an outbound full-relay peer whose best known work ≥ the tip's, while fewer than 4 are protected, becomes protected (`"Protecting outbound peer=%d from eviction"`).
12. `BlockSync::headers_direct_fetch(last)` (P3; no-op at M2).

#### 3.4.4 The `getheaders` policy and the locator

`MaybeSendGetHeaders` (`v23:3195-3211 / v24:3179-3195` (=), `NodeClock::now()` at both): a `getheaders`/`getheaders2` goes out only if `now − last_getheaders > HEADERS_RESPONSE_TIME` (2 min); it is sent with `hashStop = 0` and stamps `last_getheaders`; every receipt of `headers`/`headers2` clears the stamp. Callers: the initial sync, "more headers", unconnecting headers, the block-inv trigger (§3.4.7), `ConsiderEviction` (§3.9.1). The type is always chosen by the peer's bit 11, never by the common version: a peer with bit 11 and common < 70235 gets `getheaders2` and answers with up to 2000 compressed headers.

Locator: `ChainAccess::locator(Option<BlockId>)` = Core's `GetLocator` (`v23:chain.cpp:32-57` member, `v24:chain.cpp:32-54` free function; identical output for a non-null index: 10 single steps, then doubling, genesis last). **Null index** (G2): three callers cannot pass null (`m_best_header` is never null, R5; `pindexStart` is `best_header` or its parent; the "more headers" call needs `nCount == limit` with `pindexLast == nullptr`, which would dereference null at `v24:3380` in Core itself). **`ConsiderEviction` can**: it records the active tip as `m_work_header` (`v23:5826 / v24:5974`) and, after `CHAIN_SYNC_TIMEOUT`, builds the verify-chain-work request from `m_work_header->pprev` (`v23:5846 / v24:5992`), null when the tip is genesis. That needs no PoW: an outbound block-serving peer that completes the handshake, receives the initial `getheaders` and serves nothing useful (keeping the connection alive with pings; a sole sync peer never trips the headers-download timeout, which needs another preferred peer, §3.4.5) reaches it 20 min later on a fresh node. The pins differ: v23 `GetLocator(nullptr)` substitutes the active tip, so the request carries one hash (genesis); v24 `LocatorEntries(nullptr)` returns an empty vector, so the request carries none. `locator(None)` follows **`P2pRules.null_locator`** (`ActiveTip` under `V23_1`, `Empty` under `V24_0`; O17 row proposed, §8 Q12); the locator's version word is the common version under both (§3.3.1). The 2-minute second stage (`"Disconnecting outbound peer %d for old chain"`) follows unchanged. Differential session: a genesis-only chain, one outbound peer, advance past `CHAIN_SYNC_TIMEOUT`, compare the `getheaders`/`getheaders2` bytes (locator entries and version word) and the later disconnect under each profile (§6.2).

#### 3.4.5 Headers-sync timeout (section 10; `v23:6624-6656 / v24:6782-6813`)

If `sync_started` and the timeout is armed: when `best_header.time ≤ now_sync − max_tip_age` (still catching up), `now_us > headers_sync_timeout`, `n_sync_started == 1` and at least one **other** preferred-download peer exists: a non-noban peer is disconnected (`"Timeout downloading headers from peer=%d%s, disconnecting"`); a noban peer is logged and its sync state reset (`sync_started = false`, `n_sync_started -= 1`, timeout 0). Once caught up, the timeout is disarmed for good (`microseconds::max()`). `now_sync` per `time_source` (`v23:6627 / v24:6785`).

#### 3.4.6 What the actor needs from CH1, and when

§2.5.3 requirements R1–R7. In one sentence: `TipFacts` and the active-chain view before the reply, unthrottled; a concurrent append-only index for lookups; immutable `prev`/`skip` for ancestry and locators; a lazy IBD latch; `best_header` never null; the reply carries `last_accepted`, the invalid state and the tip snapshot.

#### 3.4.7 The block path of `inv` (`v23:4462-4566 / v24:4507-4611`)

After the size check, per entry:
- **unknown type** → `"got inv of unknown type %d: %s peer=%d"`, skip. Whether type 34 is known is `P2pRules.msg_asset_unlock` (P2P-04): under `V23_1` an `inv` 34 is skipped and is never a blocks-only violation; under `V24_0` it is a `tx`-class type (`GetCommand()` = "tx"), so on a tx-rejecting link it disconnects (`"%s (%s) inv sent in violation of protocol, disconnecting peer=%d"`). The same field decides `IsKnownType` for `getdata`/`notfound` (P4/P5);
- **block**: `UpdateBlockAvailability`; not already had, not loading, not `block_sync.is_block_requested(hash)` (`v24:4537`) → remember as `best_block`;
- **anything else**: on a tx-rejecting connection an inv whose command is in the blocks-only set disconnects; then `AddKnownInv`; if not already had: an `MSG_ISDLOCK` inv on a tx-rejecting connection is skipped silently at common ≤ 70223 and disconnects above (`v23:4502-4527 / v24:4547-4572`); else, outside IBD (or for `MSG_SPORK` even in IBD), `ObjectRequests::add_announcement` (no-op at M2). `AlreadyHave` for overlay types goes through the registry.

After the loop, if `best_block` is set and (`sync_started` or (`!inv_triggered_getheaders_before_sync` and `best_block != last_block_inv_triggering_sync`)): `maybe_send_getheaders(locator(best_header))` (`"%s (%d) %s to peer=%d"`, `v23:4550 / v24:4595`); if not yet syncing, mark the peer and remember the hash. This is the only `getheaders` path an M2 node has outside the initial sync and the unconnecting-headers path.

#### 3.4.8 Block announcements (section 7; `v23:6254-6406 / v24:6399-6551` (=); `UpdatedBlockTip` `v23:2280-2315 / v24:2273-2308`)

`UpdatedBlockTip` (outside IBD) queues the up-to-8 new tip hashes, oldest first, on every `CanRelay()` peer and wakes the loop. For a `CanRelay()` peer: revert to `inv` unless the peer asked for `headers`/`headers2` (or HB compact blocks with one block queued) or more than 8 are queued; `ProcessBlockAvailability`; walk the queue to the first header the peer lacks whose parent it has (the devnet genesis counts as had), bailing out to `inv` on a reorg or a gap; send, in priority: one `cmpctblock` (P3's encoder) if HB and exactly one header; else `headers2` (**announcement mode**, §3.4.10) if the peer sent `sendheaders2`; else `headers`; else `inv` of the last queued hash if the peer lacks it. `best_header_sent` is updated on every headers-type send. At M2 the node never connects blocks, so this section is idle; it is implemented and tested in `sim` with a fake chain.

#### 3.4.9 The header-hash path (rev1: hash once, inline, never earlier than Core; DEC-51a)

Core's hashing per `headers`/`headers2` message, in order (verified at both pins): `headers` decoding hashes nothing (`v24:5329-5333`); `headers2` `Uncompress` hashes the previous header only when a header's prev-hash flag is set (`block.cpp:122`); the connect check needs no hash (`v23:3353 / v24:3337`); a non-connecting batch of more than 8 gets +10 and returns without hashing (`v24:3339-3347`); a non-connecting batch of ≤ 8 hashes `headers.back()` and, only when the `getheaders` log fires, `headers[0]` (`v24:3146-3157`); `CheckHeadersAreContinuous` hashes every header (`v23:3183-3193 / v24:3167-3177`); `ProcessNewBlockHeaders` hashes every header again for `AcceptBlockHeader` (`v23:validation.cpp:4369 / v24:4736`; v24 computes `header.GetHash()` once per header in its loop and passes it in); `LookupBlockIndex(headers.back().GetHash())` once per message. Net: 2 X11 per `headers` header and 3 per `headers2` header on a connecting batch, 0 on a batch Core drops, all serial on `msghand`.

rdashd hashes each header **once**, inline on the actor, under these rules:

1. **Never earlier than Core, and never fewer either: the same hashes in the same order.** Decoding hashes nothing for `headers`. For `headers2` the decoder computes `X11(previous)` exactly when `Uncompress` would — for every header whose prev-hash flag is set, **before the connect check**, for every `headers2` message that passes the count check (`v23:5259-5267 / v24:5335-5345`; `block.cpp:120-123`) — and memoises it into the batch's hash slot (§3.4.10 item 4); so an unconnecting `headers2` batch larger than 8 has already cost up to `count − 1` hashes in both implementations, and that is not an exception to this rule but part of it. The connect check (§3.4.3 step 6) uses `headers[0].prev`, a field, and adds no hash. A batch that is pre-verack (gate 5), dropped by `LoadingBlocks` or oversized (step 3) is never decoded, so never hashed; a > 8 unconnecting batch adds nothing beyond `Uncompress`'s work; a ≤ 8 unconnecting batch hashes `headers.back()` and, when the log fires, `headers[0]`, as Core does. So on every path a peer can drive without PoW, rdashd's hash count equals Core's (core M-2's attack, 2000 arbitrary unconnecting plain headers followed by `ping`: 0 hashes in both; r0 hashed 2000).
2. **Hash once, incrementally, at the continuity check.** After the batch connects, step 7 runs Core's loop shape: compare, return on the first mismatch, then hash (G1). A mismatch at position k costs k−1 hashes in both implementations (2000 plain headers with genesis as the first prev and a wrong second prev, then `ping`: one hash in both; a "fill the vector, then compare" port would have cost 1,999 more and delayed every peer). Only when the loop completes is `HeaderBatch.hashes` complete; step 8's lookup and CH1's acceptance (R7) reuse the values. Core computes the same values 2–3 times on a connecting batch; the values are identical, so nothing observable changes except that the actor's critical section is shorter on **connecting** batches.
3. **No side job, no pool, no slot.** `headers2` hashing is sequential by construction (`block.cpp:122`), in IBD there is one sync peer and the actor is idle when a batch completes, so a side job could not shorten the critical path; it could only add work on paths Core skips, let one peer occupy the `prevalidate` pool, expand the receive-flood budget by ~3× (decoded headers against compressed bytes) and queue behind block prevalidation (arch M2). DEC-51a removes it; the `-debug` kill switch of r0 is gone with it.
4. **No pipelining.** The next `getheaders` leaves only after `process_headers` returns (§3.4.3 step 10), where Core sends it.

Witnesses (hash-call-count and order, not payload/outcome comparison, §6.2): mismatches at positions 2, the middle and the last; unconnecting `headers2` with compressed and with explicit prev hashes, above and at ≤ 8; a truncated compressed message; a connecting `headers2` batch (count = `Uncompress` hashes + 1). What a peer can see: nothing about order or misbehaviour (same actor, same order, same scores), and a shorter gap between a connecting batch and the next `getheaders` on a CPU-bound sync. That gap is lower latency, which NFR-FP-03 allows **only while NFR-FP-02 holds**; the per-format ratio (Core's `headers`:`headers2` cost ≈ 2:3, rdashd's 1:1) is hardware-independent, so it is recorded as an O13b classifier feature with PRD R-8 (errata row 21, §3.15), not dismissed. With a `dash-pow`-class backend the gap would not be lower at all (§5.2).

#### 3.4.10 The `headers2` codec contract (BS7)

The codec is BS7's (`dash-p2p-core::headers2`, `diff_headers2`); P2a depends on these properties (`primitives/block.cpp:57-134` (=)):
1. **Per-message state.** Every message starts with an empty version list.
2. **Two encoder modes.** The `getheaders2` reply leaves the first header uncompressed and does not record its version (`if (!v_headers.empty()) Compress(…)`, `v23:np:4804 / v24:4854`); the announcement path calls `Compress` on the first header too, which records its version (`block.cpp:59-63`). BS7 exposes both (`encode_reply` for P2b, `encode_announce` for §3.4.8) (PQ-3).
3. **Decoder.** The first header applies no compression flag; a later header with a compressed prev hash takes `X11(previous decompressed header)` (`block.cpp:122`); a version offset beyond the list leaves version 0 without an error; the list is a 7-entry MRU that can hold duplicates (DDE-SWEEP §5 BS7 items).
4. **Hash output.** The decoder exposes the prev hashes it had to compute (one per header whose prev-hash flag was set), and nothing else; the batch's remaining hashes are computed by the actor at the continuity check (§3.4.9 rule 2).

### 3.5 Ping, pong and the inactivity timeouts (`MaybeSendPing` `v23:6002-6038 / v24:6148-6184`; handlers `v23:5379-5450 / v24:5456-5527` (=); `PING_INTERVAL` 2 min `v24:148`, `TIMEOUT_INTERVAL` 20 min `net.h`)

`MaybeSendPing(node, now)` (section 4): if inactivity checks apply (`now > m_connected + -peertimeout`), a ping is outstanding and `now > ping_start + 20 min` → `"ping timeout: %fs peer=%d"`, disconnect, return. Send if the RPC queued one or none is outstanding and `now > ping_start + 2 min`; `ping_start` starts at 0, so the first pass after `verack` always pings. Nonce: `GetRand<u64>()` redrawn until non-zero; `ping_queued = false`, `ping_start = now`, `ping_nonce_sent = nonce`, push `ping`. All times are the mockable `GetTime<µs>()`.

`ping` handler: read the nonce (a short payload throws, logged, no `pong`), reply `pong(nonce)` at once (queued inside `process_message`, so it precedes anything `send_messages` adds in the same pass). `pong` (never throws): < 8 bytes → finished, `"Short payload"`; no ping outstanding → `"Unsolicited pong without ping"` (not finished); matching nonce → finished, `ping_time = msg.recv_time − ping_start` (≥ 0 else `"Timing mishap"`), updates last and minimum ping times; mismatch → `"Nonce mismatch"`, and nonce 0 → finished, `"Nonce zero"`; any problem → `"pong peer=%d: %s, %x expected, %x received, %u bytes"`; finished → `ping_nonce_sent = 0`. `recv_time` is P1's socket-read stamp (`CNetMessage::m_time`, `v24:net.cpp:652`), so queueing delay is excluded from `pingtime`/`minping` (PQ-15). The socket-level inactivity rules are ConnMan's (§3.8.7).

### 3.6 Address relay (`SetupAddressRelay` `v23:6137-6152 / v24:6283-6298`; `addr` branch `v23:4343-4437 / v24:4388-4482`; `getaddr` `v23:5312-5347 / v24:5389-5424`; `RelayAddress` `v23:2665-2713 / v24:2686-2734`; `PushAddress` `v23:1149-1162 / v24:1165-1178`; `MaybeSendAddr` `v23:6040-6106 / v24:6186-6252`; the cache `CConnman::GetAddresses(CNode&,…)` `v23:net.cpp:4252-4294 / v24:4265-4307`; all (=))

#### 3.6.1 Per-peer state

`addr_relay_enabled` (false), `addr_known: Option<RollingBloom>` ((5000, 0.001) when relay is first enabled), `addrs_to_send` (≤ 1000), `getaddr_sent`, `getaddr_recvd`, `wants_addrv2`, `next_addr_send` and `next_local_addr_send` (0), `addr_token_bucket: f64 = 1.0` and `addr_token_timestamp` = mockable time **when the peer state is created** (`v24:np:414-416`), `addr_processed`, `addr_rate_limited`.

#### 3.6.2 Who relays

`SetupAddressRelay` returns false for block-relay-only connections and otherwise enables relay once. Called for every outbound connection in the `version` handler (§3.3.5 step 16), and for inbound peers on their first `addr`, `addrv2` or `getaddr`. An inbound peer that never sends one never receives addresses from us.

#### 3.6.3 Receiving `addr`/`addrv2` (post-read, from Core)

1. Read the vector; a decode error is the generic throw.
2. `!SetupAddressRelay` → `"ignoring %s message from %s peer=%d"`, return (`v23:4350-4353 / v24:4395-4398`). An oversized `addr` from a block-relay-only peer is ignored without a score; an inbound peer's oversized one scores **and** enables relay.
3. > 1000 → +20 `"%s message size = %u"`, return (`v23:4361-4364 / v24:4406-4409`).
4. Refill the bucket if below 1000: `+ max(now − timestamp, 0) × 0.1`, capped at 1000; always stamp (`v24:4418-4424`).
5. `rate_limited = !permission(Addr)`; shuffle the vector with a fresh RNG.
6. Per address: bucket < 1 → if rate-limited count and skip, else continue without decrement; else take a token (**before** any filter; PQ-9); skip if neither `MayHaveUsefulAddressDB` nor `HasAllDesirableServiceFlags`; fix `nTime` ≤ 100,000,000 or > now + 10 min to now − 5 d; `AddAddressKnown`; skip (after remembering) banned or discouraged; count as processed; `RelayAddress` if `nTime > now − 10 min`, `!getaddr_sent`, the message had ≤ 10 entries and the address is routable; keep if reachable.
7. `"Received addr: %u addresses (%u processed, %u rate-limited) from peer=%d"`; `addrman.add(kept, source = peer.addr, penalty = 2 h)` (`v23:4429 / v24:4474`).
8. < 1000 received → `getaddr_sent = false` (`v23:4430 / v24:4475`).
9. ADDR_FETCH and > 1 entry → `"addrfetch connection completed peer=%d; disconnecting"` (`v23:4433-4436 / v24:4478-4481`).

#### 3.6.4 `getaddr`

Outbound → `"Ignoring \"getaddr\" from %s connection. peer=%d"`. Inbound: enable relay; a repeat → `"Ignoring repeated \"getaddr\". peer=%d"`; otherwise clear `addrs_to_send` and push every address of `GetAddresses(1000, 23 %)`, uncached for `addr`-permissioned peers and otherwise from the cache keyed by SipHash(`RANDOMIZER_ID_ADDRCACHE` 0x1cf2e4ddd306dda9, the requestor's network, our local bind bytes, the bound port for inbound), refreshed after 21 h + U[0, 6 h); banned and discouraged entries removed per fresh result. The reply leaves on the **next** `send_messages` pass when this peer's `next_addr_send` is still 0, i.e. when no earlier `addr`/`addrv2` from it had already enabled relay and started the Exp(30 s) schedule (core n-4; §3.12 new-6). P2a implements the handler and the cache; P2b owns `p2p_getaddr_caching`.

#### 3.6.5 `RelayAddress`

```
if !reachable && !addr.IsRelayable(): return
h  = CServiceHash(0,0)(addr); t = (u64(now_s) wrapping_add h) / 86400
hs = SipHash(seed0, seed1).write(0x3cac0035b5866b90).write(h).write(t)
n  = (reachable || hs.finalize() & 1) ? 2 : 1
best = top-n by SipHash(hs).write(u64 id) over { peers with relay enabled, id ≠ origin, wants_addrv2 || addr.IsAddrV1Compatible() }  (strict ">"; includes peers mid-handshake)
for each of best with a non-zero key: PushAddress(peer, addr)
```

`PushAddress` drops invalid addresses, addresses in `addr_known`, non-v1 addresses for peers without `sendaddrv2`; when `addrs_to_send` is full it overwrites a random entry.

#### 3.6.6 `MaybeSendAddr` (section 5)

Nothing for peers without relay. If `fListen`, out of IBD and `now > next_local_addr_send` (`v24:6193-6195`): on every time but the first, reset `addr_known`; push our address from `GetLocalAddrForPeer` with our services and time now; `next_local_addr_send = exponential_rand(now, 24 h)`. Then, if `now > next_addr_send`: reschedule `exponential_rand(now, 30 s)`; drop queued addresses the peer already knows (marking the rest known); send `addrv2` if asked else `addr`; clear. Both timers start at 0, so a listening node out of IBD advertises itself in the first pass after `verack`; at M2 the node is in IBD, so never (as dashd in IBD).

`exponential_rand(now, avg) = now + (−log1p(U × −2^−48)) × avg + 0.5 µs` with a 48-bit uniform `U` (`random.cpp` v23:788-792 / v24:771-775); ported exactly (f64 `ln_1p`), which is all NFR-FP-03's KS test needs.

### 3.7 AddrMan, netgroup and local addresses

Core: `addrman.cpp` (`v23:1-1341 / v24:1-1383`; v24 = v23 + 4 up to line 847), `addrman_impl.h`, `netgroup.cpp`, `netaddress.cpp`, `util/asmap.cpp`, `addrdb.cpp`. Bucketing, selection and the `peers.dat` format are identical at both pins; the only behavioural changes are four lines in `GetAddr` (a `filtered` flag, default true at every caller, `v24:815,845`) and `GetEntries` behind the hidden v24 `getrawaddrman` RPC.

#### 3.7.1 Port, not redesign

`net::addrman` is a line-by-line port of `AddrManImpl` with Core's names (`add_single`, `good_`, `attempt_`, `select_`, `get_addr_`, `resolve_collisions_`, `select_tried_collision_`, `make_tried`, `clear_new`, `delete`, `create`, `swap_random`, `check_addrman`). It takes `now` and an RNG from its caller instead of reading globals, which is the only structural change and what makes it fuzzable and oracle-comparable. The API is `AddrMan(Arc<Mutex<AddrManImpl>>)` with one critical section per public call and `Check()` on both sides (`v24:addrman.cpp:1181-1260`).

| Item | Core | Rust |
|---|---|---|
| Geometry | 1024 new × 64, 256 tried × 64; 8 tried buckets per group; 64 new buckets per source group; ≤ 8 references (`addrman_impl.h:27-34`, `addrman.cpp` v23:26-44 / v24:30-48) | the same constants; flat `[i32; 65536]` (256 KiB) and `[i32; 16384]` (64 KiB) tables, −1 = empty |
| Entries | `std::unordered_map<int, AddrInfo> mapInfo`; ids from `nIdCount++`, never reused | `Slab<AddrInfo>` by id, same allocation rule |
| Index | `unordered_map<CService,int>` keyed by IP **and port** (`V4_MULTIPORT`; no `discriminatePorts` at either pin) | `hashbrown::HashMap<ServiceKey, i32>`, never iterated |
| Random order | `std::vector<int> vRandom`, swap-with-last removal; `GetAddr` permutes it permanently (PQ-19); **after a `peers.dat` load that prunes entries its order depends on libstdc++'s `unordered_map` iteration** (§4.1, core M-6) | identical operations; the post-load order is the one non-reproducible state (§6.3) |
| Collisions | ordered `std::set<int>` (≤ 10) | `BTreeSet<i32>` (iteration order and `advance(randrange(size))` both need it) |
| Network counts | `unordered_map<Network,{n_new,n_tried}>` by `GetNetwork()` | `[Counts; 7]` by network id |
| Key | `nKey` 32 bytes; random, or `uint256{1}` in deterministic (test-only) mode | same |

Bucket math (reproduces Core's own vectors: tried bucket 40, new bucket 786, 160 distinct tried buckets, and 236/795 with `test/data/asmap.raw`):

```
cheap(x)                   = u64::from_le_bytes(SHA256d(x)[0..8])                       // HashWriter::GetCheapHash
tried_bucket(K, G)         = cheap(nKey ‖ len(G) ‖ G ‖ le64(cheap(nKey ‖ len(K) ‖ K) % 8)) % 256
new_bucket(G, S)           = cheap(nKey ‖ len(S) ‖ S ‖ le64(cheap(nKey ‖ len(G) ‖ G ‖ len(S) ‖ S) % 64)) % 1024
bucket_position(new, b, K) = cheap(nKey ‖ ('N' | 'K') ‖ le32(b) ‖ len(K) ‖ K) % 64
K = GetKey() = 16-byte v1 address bytes (IPv4 as ::ffff:a.b.c.d) or raw 32-byte Tor/I2P bytes, ‖ port (big-endian)   // netaddress.cpp:900-906
G, S = GetGroup(addr), GetGroup(source)   (§3.7.3); len(·) is a CompactSize prefix
```

The port enters the tried bucket and the positions but not the new bucket or the group. SHA-256d goes through the SH1 backend (DEC-28).

#### 3.7.2 Operations (each with a named unit test; `v23 / v24` lines)

- `IsTerrible` (`:67-90 / :71-94`): tried within 1 min → never terrible; `nTime > now + 10 min`; `now − nTime > 30 d` (`ADDRMAN_HORIZON`); never succeeded and ≥ 3 attempts (`ADDRMAN_RETRIES`); last success > 7 d and ≥ 10 attempts (`ADDRMAN_MAX_FAILURES`, `ADDRMAN_MIN_FAIL`).
- `GetChance` (`:92-105 / :96-109`): `0.01` if tried < 10 min ago, times libm `pow(0.66, min(nAttempts, 8))` with `nAttempts` signed: a negative value read raw from disk is kept (`CheckAddrman` never validates it), gives a chance above 1 and `inf` near `INT_MIN`; reachable only from a crafted local file (core m-9; PQ-25). **Floating point in a peer-visible path** (PQ-20): the Rust uses a compile-time table of the nine exact doubles `0.66^0..8` (verified against the oracle) for `0 ≤ n ≤ 8` and a glibc-bit-exact `pow` for `n < 0` (verified by `oracle_addrman_chance` cases at −1, −8, −1000 and `INT_MIN`), and mirrors Core's operand order; `Select_` compares `randbits(30) < chance_factor * chance * (1 << 30)` evaluated left to right in `f64` (`v24:788`), `chance_factor *= 1.2` per rejection (`:794`; multiplied, never `1.2^k`).
- `AddSingle` (`:553-626 / :557-630`): unroutable → reject; the 2 h penalty is dropped when `addr == source` (`CNetAddr ==`, port ignored; `v24:568`; PQ-8); an existing entry's time moves forward only by more than 1 h (advertised < 24 h ago) or 24 h; services OR-ed; the early return compares the **unpenalized** time; tried or 8 references → stop; an extra reference with probability 2^−refcount; the bucket uses the caller's source; an occupied slot is replaced if its occupant is terrible or has more references while the newcomer has none (`ClearNew`).
- `Good_` (`:628-684 / :632-688`): records `m_last_good` even for unknown addresses; never touches `nTime`; `nAttempts = 0`, `m_last_success = m_last_try = time`; already tried → false; test-before-evict queues a collision (dropped silently when 10 are queued); else `MakeTried` (removes every new reference, evicts the tried occupant back to new).
- `Attempt_` (`:698-716 / :702-720`): `m_last_try = time`; counts a failure only if `count_failure` and the last counted attempt predates `m_last_good`.
- `Connected_` (`:849-866 / :877-894`): refreshes `nTime` only if > 20 min stale; `SetServices_` overwrites. Called from `FinalizeNode` only for successfully connected, unscored, **non-block-relay outbound** peers (`v23:1870-1875 / v24:1848-1853`; PQ-18).
- `Select_` (`:718-792 / :722-796`): `randbool` for new vs tried only when both are non-empty and `!new_only`; random bucket, random start position, linear probe; the network filter uses `GetNetwork()`.
- `GetAddr_` (`:811-847 / :815-851`): `nNodes = max_pct × size / 100` (integer), capped by `max_addresses`; a partial Fisher–Yates over `vRandom` that persists; the network filter uses `GetNetClass()` (6to4 and Teredo are IPv4 here but IPv6 in `Select`; PQ-7); terrible entries skipped (v24: only when `filtered`).
- `ResolveCollisions_` / `SelectTriedCollision_` (`:895-984 / :923-1012`): `ADDRMAN_REPLACEMENT` 4 h, 60 s, `ADDRMAN_TEST_WINDOW` 40 min, exactly as written; `SelectTriedCollision_` does not remove the chosen id.
- `Check()` with `-checkaddrman=<ratio>` (default 0, clamped to [0, 10^6]); a ratio above 1 draws from the RNG in every public call (`randrange(1)` at ratio 1 draws nothing), so the port draws too; the error codes (−1…−21) are ported for the fuzzers and `addrman_tests`.

The RNG is Core's `FastRandomContext` (ChaCha20, RFC 8439, nonce 0, counter 0; a 64-bit bit buffer whose refill discards leftover bits; `randrange` by rejection with `bit_width(r−1)` bits and no draw for `randrange(1)`; `rand64`/`rand256` bypass the buffer). P2a carries it as `net::rng::FastRandom` (or reuses CS0/O4's port if one exists) and uses it everywhere Core would; its first deterministic `rand64` is checked against `0x903df1a0ade0b876` and Core's `random_tests` vectors.

#### 3.7.3 Netgroups, asmap and address predicates

`GetGroup` (`netgroup.cpp` v23:17-78 / v24:18-79): an asmap hit gives `[NET_IPV6, asn LE]`; otherwise `[GetNetClass()]` followed by nothing for local or unroutable; 10 hash bytes for internal; the /16 of the linked IPv4 for IPv4, 6to4, Teredo (bit-inverted), NAT64 and SIIT; 4 bits for Tor and I2P; 12 for CJDNS; 36 for Hurricane Electric `2001:470::/36`; 32 for other IPv6, with the partial byte OR-ed with ones. `GetMappedAS` interprets Core's asmap bytecode (`util/asmap.cpp:88-131`, LSB-first bits per byte, IP bits MSB-first, IPv4 as `::ffff:`) after `SanityCheckASMap`. The interpreter is ~60 pure lines; P2a ports it with the asmap cases of `addrman_tests.cpp` and Core's `fuzz/asmap*.cpp` targets (there is no `asmap_tests` file at either pin) because bucket parity needs it; P8 keeps `-asmap` loading UX, the health check (§3.0 #9) and `feature_asmap`. The asmap checksum field of `peers.dat` is `uint256()` while asmap is empty.

Routability predicates (`IsRFC1918` … `IsRFC7343`, `IsHeNet`, `IsLocal`, `IsValid`, `IsRoutable`, `GetNetwork`, `GetNetClass`, `HasLinkedIPv4`, `GetLinkedIPv4`, `IsAddrV1Compatible`, `IsRelayable`; `netaddress.cpp:313-785` (=)) are methods on a `NetAddr` newtype over base-sdk's address bytes. `-allowprivatenet` (default 0) makes RFC 1918 routable process-wide; it is a `NetConfig` value, never a global. BS10 owns the BIP155 field canonicalisation divergences (DDE-SWEEP §5).

#### 3.7.4 Local addresses (`net.cpp` v23:176-326 / v24:177-327)

Scores `LOCAL_IF` 1, `LOCAL_BIND` 2, `LOCAL_MAPPED` 3, `LOCAL_MANUAL` 4; `AddLocal` (unroutable rejected while `RequireRoutableExternalIP`, false only on regtest; below `LOCAL_MANUAL` only with `-discover`; unreachable networks rejected; re-adding bumps the score); `SeenLocal` bumps only known addresses, from inbound peers; `GetLocal` picks the best `GetReachabilityFrom(peer)` then score, never crossing privacy/clear networks, falling back to `0.0.0.0:<listen port>`; `GetLocalAddrForPeer` substitutes the peer-reported address when `IsPeerAddrLocalGood` and either ours is unroutable or `randbits(score > LOCAL_MANUAL ? 3 : 1) == 0`, keeping our listen port for outbound peers. Sources: `-externalip` and bound addresses (P2a); interface discovery and NAT mapping (P8). Agentbox has no routable address, so these paths are covered by unit tests and the functional tests' `-externalip`.

### 3.8 ConnMan (outbound)

Core: `CConnman` in `net.cpp`; the outbound logic is identical between the pins except §3.0's rows 5, 9, 11, 12. Lines are `v23 / v24`.

#### 3.8.1 Tasks and start order (`Start` `:3985 / :3991`)

`InitBinds` if listening (§3.8.9); queue every `-seednode` as an ADDR_FETCH; read `anchors.dat` if addrman outbound is in use; create the outbound semaphore (`min(8 + 2 + 1, nMaxConnections)` = 11) and the addnode semaphore (8); spawn, in Core's order:

| Task | Core thread | Started when | Cadence |
|---|---|---|---|
| `socket` (disconnector + inactivity sweep; readers/writers are per-node P1 tasks) | `net` | always | wakes on any socket event or after 500 ms (`SELECT_TIMEOUT_MILLISECONDS` + wakeup pipe, `net.cpp` v23:113-119 / v24:114-120); the sweep when > 1 s since the last; then `DisconnectNodes` |
| `dnsseed` | `dnsseed` | `-dnsseed` (default 1) | once (§3.8.4) |
| `addcon` | `addcon` | always | §3.8.5 |
| `opencon` | `opencon` | no `-connect`, or a non-empty `-connect` list (`-connect=0` alone starts none, `:4085-4090 / :4091-4096`) | 500 ms per iteration + the blocking connect (§3.8.2) |
| `mncon` | `mncon` | v23 always; v24 masternode, `-watchquorums` or a wallet (#7659, unobservable) | P7 |
| `msghand` | the `PeerManager` thread | always | §3.1 |

Scheduler: `DumpAddresses` every 15 min (`DUMP_PEERS_INTERVAL`), `CheckForStaleTipAndEvictPeers` every 45 s (on the actor), and, v24 with `-asmap` only, the ASMap health check (P8). Shutdown follows `StopNodes` (`:4174 / :4187`): dump `peers.dat` first, then `anchors.dat` from the current block-relay-only connections (`v24:net.cpp:4190-4199`; core n-3), then disconnect everyone; ND1 has already interrupted the actor (§2.2.1). Sleeps use real time (Core's `interruptNet.sleep_for` is not mockable); every time *read* uses `NodeClock`; `sim` virtualises both.

#### 3.8.2 `opencon` (`ThreadOpenConnections` `:2923-3298 / :2917-3292`)

**`-connect` mode** (`:2926-2950 / :2920-2944`): forever, dial every `-connect` address as MANUAL (v2 iff we advertise `NODE_P2P_V2`) without a grant, sleeping `min(pass, 10) × 500 ms` after each and 500 ms after each pass, then `PerformReconnections`. `OpenNetworkConnection` skips names already connected, so each pass retries only the missing peers. `-connect` soft-sets `-dnsseed=0` and `-listen=0`, disables `-seednode`, and makes `mncon` return at once.

**Main loop, one iteration** (`:2953-3297 / :2947-3291`):
1. `ProcessAddrFetch` (`:2812 / :2806`): pop one ADDR_FETCH destination; **try** a grant; none → dropped; else dial it (v2 iff we advertise it).
2. Sleep 500 ms; `PerformReconnections` (`:4939 / :4980`).
3. **Block** on an outbound grant.
4. Fixed seeds (§3.8.6).
5. **Only when `!AllowMultipleAddressesFromGroup()`** (`:3034 / :3028`; false on mainnet and testnet, **true on devnet and regtest**, `chainparams.cpp` v23:283,476,658,857 / v24:294,497,689,895): count `nOutboundFullRelay` (full-relay, excluding masternode connections), `nOutboundBlockRelay`, `nOutboundOnionRelay`, and build the netgroup set of MANUAL, OUTBOUND_FULL_RELAY and BLOCK_RELAY peers (Tor/I2P/CJDNS count as privacy-network peers instead; inbound, feeler and addr-fetch excluded). **On regtest and devnets every counter stays 0, no netgroup set is built, and the ladder below always picks full-relay (or block-relay for anchors), bounded only by the 11 grants** (PQ-12). Fable's reading ("only the netgroup rule is skipped") is wrong at both pins; Opus's is right.
6. Collect the verified ProRegTx hashes of connected nodes (`:3066-3074 / :3060-3068`).
7. The connection-type ladder (first match wins, `:3101-3154 / :3095-3148`): anchors left and block-relay below 2 → BLOCK_RELAY (anchor); full-relay < 8 → OUTBOUND_FULL_RELAY; block-relay < 2 → BLOCK_RELAY; the stale-tip flag → OUTBOUND_FULL_RELAY; `now > next_extra_block_relay` and extra block-relay peers started → BLOCK_RELAY (reschedule Exp(5 min)); `now > next_feeler` → FEELER (reschedule Exp(2 min)); all 8 full-relay up, `now > next_extra_network_peer` and `MaybePickPreferredNetwork` finds a reachable network with no outbound manual/full-relay peer and addrman entries → OUTBOUND_FULL_RELAY restricted to it (reschedule Exp(5 min)); Dash only: fewer than `min(2, maxconnections/2)` onion full-relay peers and Tor reachable → OUTBOUND_FULL_RELAY onion-only; else release the grant and loop.
8. `addrman.resolve_collisions()`; snapshot the tip MN list (`MnListView::at_tip()`, empty until EV/CH land, so step 9m never fires at M2).
9. Address selection (`:3161-3270 / :3155-3264`), `nTries` from 0, first decisive rule wins: (a) anchors: pop from the **back**; skip (not retried) if invalid, local, unreachable, lacking desirable services, or its netgroup is used; when they run out fall through to addrman with the same type; (b) `++nTries > 100` → give up this iteration; (c) a feeler takes `SelectTriedCollision`; with no collision it takes `Select(new_only = true)`; with a collision whose address is already connected it runs `Good` and takes `Select(true)`; a non-feeler takes `Select(false, preferred_net)` (`v24:net.cpp:3178-3200`; core m-11); (d) not a feeler and netgroup used → next (precedes validity); (e) invalid → give up; (f) our own listen port on a local address → give up; (g) unreachable → next; (h) onion-only and not Tor → next; (i) tried within 10 min and `nTries < 30` → next; (j) not a feeler and lacking desirable services, or a feeler lacking `MayHaveUsefulAddressDB` → next; (k) `nTries < 50` and a bad port (`IsBadPort`, `netbase.cpp` v23:781 / v24:801) → next; (l) an `-addnode` target (checked only while fewer than 24 are configured) → next; (m) Dash: a masternode we are already connected to → next; else accept. No default-port preference and no "after 40 tries" relaxation at either pin.
10. Feelers sleep U[0, 1 s) first (`FEELER_SLEEP_WINDOW`); `count_failures = (#ipv4/6 netgroups + #privacy peers) ≥ min(maxconnections − 1, 2)` (false on regtest/devnet, except with `-maxconnections ≤ 1`, where `min(n − 1, 2) = 0` makes it true; core n-5); **v2 iff `addr.nServices & our services & NODE_P2P_V2`**; `OpenNetworkConnection(addr, count_failures, grant, type, v2)`.

`next_feeler`, `next_extra_block_relay`, `next_extra_network_peer` start at `exponential_rand(start, …)`. Extra block-relay peers are enabled once, the first time `CanDirectFetch()` holds in `CheckForStaleTipAndEvictPeers` (§3.9.3); the stale-tip flag is set and cleared there too. Observable consequences on mainnet/testnet: the two block-relay slots fill after the eight full-relay slots (visible through `relay=0` in the `version`), one feeler every ~2 min that disconnects right after `version`, an extra block-relay peer every ~5 min once `CanDirectFetch()` first held, the netgroup diversity rule and the "100 tries then sleep" cadence. **Regtest churn** (core m-12): with the accounting skipped, regtest opens up to 11 full-relay peers, but `GetExtraFullOutboundCount()` is computed separately in `net.cpp` and still counts them, so `EvictExtraOutboundPeers` disconnects one unprotected extra every 45 s once it has been connected > 30 s (localhost peers share a network, so none is protected as "only peer on its network"); the soak and O13a-out expectations model this churn.

#### 3.8.3 Anchors

At most 2 (`MAX_BLOCK_RELAY_ONLY_ANCHORS`). Read at start only when addrman outbound is in use; **deleted immediately after reading**, even on success (`addrdb.cpp` `ReadAnchors`: `fs::remove`, v23:242 / v24:249); truncated to 2; tried first (ladder row 1), popped from the back, never retried; never enter addrman. Written only on a clean shutdown with the current block-relay-only connections' addresses (`GetCurrentBlockRelayOnlyConns`, `:3301 / :3295`; `IsBlockRelayOnly()` also matches any peer under `-blocksonly` without `relay`, PQ-13), in table order, ≤ 2.

#### 3.8.4 DNS seeds (`ThreadDNSAddressSeed` `:2686-2800 / :2680-2794`)

Shuffle the seeds; query all at once if `-forcednsseed` or addrman is empty; otherwise wait 11 s (addrman < 1000 entries) or 5 min (≥ 1000) in 11 s steps, stopping (`"P2P peers available. Skipped DNS seeding."`) as soon as two full-outbound, handshaken, non-probe connections exist (masternode connections count, block-relay-only do not; `v24:2731-2740`). Batches are 3 (`DNSSEEDS_TO_QUERY_AT_ONCE`), but each network has one seed (`dnsseed.dash.org.` mainnet, `chainparams.cpp` v23:250 / v24:259; `testnet-seed.dashdot.io.` testnet, v23:444 / v24:463; none on devnets; `dummySeed.invalid.` regtest). Per seed: wait while the network is inactive; with a name proxy (P8) → ADDR_FETCH the bare name; else resolve `x1.<seed>` (always `x1`: `GetDesirableServiceFlags(NODE_NONE)` = `NODE_NETWORK`), ≤ 32 results (`LookupHost(host, 32, true)`), each dated U[now − 7 d, now − 3 d] (`v24:2779`), services `NODE_NETWORK`, `addrman.add(results, source = internal("x1.<seed>"))`; an empty answer → ADDR_FETCH the bare seed name. Log lines reproduced verbatim (`"Loading addresses from DNS seed %s"`, `"%d addresses found from DNS seeds"`, `"Waiting %d seconds before querying DNS seeds."`). The resolver is the system resolver (`tokio::net::lookup_host`), as Core uses `getaddrinfo`.

#### 3.8.5 `addcon`, reconnections and the dial path

`addcon` (`:3374-3401 / :3368-3395`): block on an addnode grant; for each added node not connected (numeric entries match `IP:port`, names match the connection name; inbound peers count): dial as MANUAL with the entry's v2 flag (for `-addnode`, iff we advertise v2, `init.cpp` v24:1282-1288; RPC `addnode … v2transport` per entry), sleep 500 ms, next grant; then `PerformReconnections`; sleep 60 s if anything was tried, else 2 s. Manual peers get no permissions, are never discouraged (`MaybeDiscourageAndDisconnect`) and are exempt from `ExpectServicesFromConn`.

**v1 reconnection** (`ShouldReconnectV1`, `:1653 / :1647`): an initiating v2 transport still waiting for the key, having received nothing and sent at least 24 bytes, is reconnected over v1 with the same address, destination string (`m_dest`), type, grant and masternode flags, `count_failures = false`, on the next `opencon`/`addcon` pass. P1 reports the condition (`V2HandshakeFailedEarly`); ConnMan queues the retry in `DisconnectNodes` (`:2169-2182 / :2163-2176`). Observable: a v2-capable peer that gets our V2 key dump and closes sees a second TCP connection carrying a plain V1 `version` shortly after.

**`OpenNetworkConnection`** (`:3592-3671 / :3586-3665`): return if interrupted or the network is inactive; for an address: return if banned, discouraged (`BanList`) or already connected (`AlreadyConnectedToAddress`: full `IP:port`, ignoring disconnecting nodes), or if it is our listen port on a local address (`v23:3622 / v24:3616`); for a name: return if a node with that name exists; `ConnectNode`; attach the grant; set the MN flags (P7); `initialize_node` synchronously on this thread (creates the peer state and pushes `version`, §2.2; Core `v24:net.cpp:3648`); append to the table; count manual/full-relay per network.

**`ConnectNode`** (`:371-516 / :372-517`): names are resolved with `Lookup(dest, default_port, name_lookup && !proxy, 256)`, shuffled, **every** resolved address checked in order (the first invalid one aborts with `"Resolver returned invalid address"`, any already-connected one aborts with `"Not opening a connection to %s, already connected to %s"`), and **the last one** is dialled (`v24:404-423`; Fable's "first resolved address" is wrong); direct TCP connect with `-timeout` (5000 ms, `netbase.h` v23:25) and `TCP_NODELAY`; proxies, Tor and I2P are P8 (InitError until then); `addrman.attempt(addr, count_failures)` after every direct or SOCKS attempt that reached the target network (`v24:488-492`), skipped only when the proxy itself failed; node ids from a counter shared with inbound, from 0; the local nonce (§3.3.4); `keyed_netgroup = SipHash(RANDOMIZER_ID_NETGROUP 0x6c0edd8036ef4036).Write(GetGroup(addr))` (`v24:4973`); the transport chosen by `use_v2` (`MakeTransport` `:4683 / :4724`), both at `INIT_PROTO_VERSION`. The connect blocks the dialling task, as in Core.

#### 3.8.6 Fixed seeds (`:2980-3018 / :2974-3012`; `ConvertSeeds` `:208-225 / :207-224`)

After the grant, if `-fixedseeds` (default 1) and some reachable network has an empty addrman: when 60 s have passed since `opencon` started, or immediately if `-dnsseed=0` and no `-seednode` and no `-addnode`: add `chainparamsseeds.h`'s addresses for those networks with services `GetDesirableServiceFlags(NODE_NONE)` = `NODE_NETWORK` and a time **U(now − 2 w, now − 1 w]** (`rng.rand_uniform_delay(Now<NodeSeconds>() - one_week, -one_week)`, a half-open range with a negative delay, identical at both pins; Fable's "[now − 7 d, now)" is wrong), source `"fixedseeds"`; once per process. `chainparamsseeds.h` is identical at both pins: mainnet 170 IPv4 + 25 TorV3, testnet 19 IPv4, none on devnet and regtest. P2a generates `seeds.rs` from it (`contrib/node/gen-fixed-seeds.py`, BIP155 bytes verbatim) and a test asserts equality with the pinned header's bytes (FR-P2P-07).

#### 3.8.7 The socket task: inactivity, disconnect, flow control

- **Inactivity** (`InactivityCheck` `:2304-2337 / :2298-2331`), swept when > 1 s since the last sweep (steady clock at v24, system clock at v23, §3.0 #11): only once `connected + -peertimeout (60 s) < now` (`ShouldRunInactivityChecks`); no byte ever received or ever sent → `"socket no message in first %i seconds, %d %d peer=%d"`; nothing sent for 20 min → `"socket sending timeout: %is peer=%d"`; nothing received for 20 min → `"socket receive timeout: %is peer=%d"`; handshake incomplete → `"version handshake timeout peer=%d"` or `"V2 handshake timeout peer=%d"` (P1 reports whether the transport is still detecting). `connected` is whole seconds, so a silent peer goes 60–62.5 s after connect. `-peertimeout=999999999` disables it in tests.
- **`setnetworkactive false`** marks every connection `"Network not active, dropping peer=%d"` (`:2106-2114 / :2100-2108`), stops new outbound connections and makes the DNS task wait (`"Waiting for network to be reactivated before querying DNS seeds."`).
- **Disconnect** (`DisconnectNodes` `:2073-2207 / :2067-2201`): when a node is marked and **we** initiated (`!fOtherSideDisconnected`), start a 100 ms linger (`v23:2136 / v24:2130`); on the first pass at which the writer's queue is empty, `shutdown(SHUT_WR)` once; on the first pass after the deadline, remove the node (`"ThreadSocketHandler -- removing node: peer=%d …"`), queue a v1 retry if applicable, release the grant, close, decrement the per-network count, `finalize_node`. A peer-initiated close or a transport error closes at once (no linger). Disconnecting nodes are no longer read but are still flushed. With the 500 ms idle cadence the close lands 100–600 ms after the mark; the socket task reproduces that cadence rather than closing at exactly 100 ms. `FinalizeNode` (`:1816-1876 / :1783-1856`): `nSyncStarted--` if syncing, drop in-flight blocks, `ObjectRequests::finalize`, counters, `addrman.connected(addr)` for successful, unscored, non-block-relay outbound peers, `"Cleared nodestate for peer=%d"`.
- **Send side.** Core never sends from the calling thread (`PushMessage` queues and wakes the socket thread) and writes with `MSG_MORE` while more bytes follow, so a burst (e.g. `sendaddrv2` + `verack` + `getaddr`) usually leaves as one TCP segment. **P1's writer must coalesce** every queued frame into one `writev` per wake-up with `TCP_NODELAY`, to reproduce the segment boundaries a capturing peer sees. **Send accounting is retained memory, not wire bytes** (G3): `fPauseSend = m_send_memusage + transport.GetSendMemoryUsage() > -maxsendbuffer × 1000` (1 MB), where `m_send_memusage` is the sum of `CSerializedNetMsg::GetMemoryUsage()` = `sizeof(CSerializedNetMsg)` (56 on 64-bit libstdc++: a `std::vector` and a `std::string`) + `memusage::DynamicUsage(data)` = `MallocUsage(capacity)` = `((capacity + 31) >> 4) << 4` (`v23:net.cpp:135-141 / v24:136-142`; `memusage.h:51-58`) over the queued messages; the charge moves to the transport when `SocketSendData` hands a message over (`v23:1712-1720 / v24:1702-1709`), the transport's own charge is the retained message (V1, `v24:877-882`) or the encoded buffer (V2, `v24:1664-1670`) including capacity retained after a partial send, and the flag is recomputed from both terms after every write (`v23:1771 / v24:1765`). An 8-byte `pong` therefore costs 88, not 32; ignoring fixed buffers a 1 MB budget trips at ≈ 11,364 queued pongs, not ≈ 31,251, and a V1 payload that is half sent still counts in full until the transport releases it. rdashd defines the **logical** charge by that formula (never by Rust struct sizes), charges it in `push_message`, transfers it at `FrameWriter::set_message_to_send`, reads `FrameWriter::send_memory_usage()` for the transport term, and recomputes `pause_send` after every write; the receive side stays raw-wire-byte accounting (`fPauseRecv`, §3.1.3). Tests: stalled and partially draining V1 and V2 writers at the boundary (many small pongs; one large payload partly sent) compared with Core's pause/resume points. `send_bytes_per_msg[cmd] += n` per `send()` call excluding V2 handshake bytes (`SocketSendData` `v24:1734-1738`).
- **Receive side.** One read of up to 64 KiB per node per socket-loop round (`:2595 / :2601`); `last_recv` and `recv_bytes` on every chunk; every message completed by that read is classified and handed to the actor together, then one wake (§3.1.3); `recv_bytes_per_msg[cmd] += raw_size` for known commands, `*other*` for unknown and for rejected frames (`ReceiveMsgBytes` `:665-680 / :666-681`); reading stops while `fPauseRecv`.

#### 3.8.8 Limits

| Item | Value | Core |
|---|---|---|
| `-maxconnections` | 125; `nMaxConnections = min(maxconn, fd_max − binds − 150 − 8 − 1)`; InitError below 150 descriptors | `init.cpp` v23:1243-1262 / v24:1295-1314 |
| full-relay / block-relay / feeler | `min(8, maxconn)` / `min(2, maxconn − full)` / 1 | `init.cpp` v23:2542-2544 / v24:2609-2611 |
| outbound grants | `min(full + block + feeler, maxconn)` = 11 | `net.cpp` v23:4036 / v24:4042 |
| addnode grants | 8 (separate) | v23:4040 / v24:4046 |
| onion full-relay target (Dash) | `min(2, maxconn / 2)` | `init.cpp` v23:2544 / v24:2611 |
| inbound | `maxconn − 11` = 114 (P2b/P4) | `net.cpp` v23:1889 / v24:1883 |
| no outbound grant | `-connect`, addnode (own pool), masternode connections, v1 retries (reuse the old grant) | §3.8.5 |

#### 3.8.9 `InitBinds` and `bind_failure_fatal` (`:3963-3983 / :3957-3989`; O17 P2P-33)

ConnMan binds and listens; P2b accepts. `InitRules.bind_failure_fatal` arrives through `ConnManOptions`: `false` (`V23_1`): startup succeeds if any bind succeeds; a failed explicit `-bind`/`-whitebind` is reported but tolerated; onion binds are `BF_DONT_ADVERTISE` only (errors silent); for the default binds a failure is reported only if both `::` and `0.0.0.0` fail. `true` (`V24_0`): any failed explicit `-bind`, `-whitebind` or onion bind (now `BF_REPORT_ERROR`), or a failed `0.0.0.0` default bind → `"Failed to listen on any port. Use -listen=0 if you want this."`; only `[::]` stays non-fatal. Bound routable addresses are added to `mapLocalHost` as `LOCAL_BIND` unless `noban`/`DONT_ADVERTISE` (`:3957 / :3951`). `InitRules.onion_bind` (P2P-34) is P8's.

### 3.9 Outbound eviction and the stale tip (`ConsiderEviction` `v23:5803-5859 / v24:5949-6005`; `EvictExtraOutboundPeers` `v23:5861-5973 / v24:6007-6119`; `CheckForStaleTipAndEvictPeers` `v23:5975-6000 / v24:6121-6146`; `TipMayBeStale` `v23:1395-1403 / v24:1418-1426`; counters `net.cpp` v23:2833-2905 / v24:2827-2899; identical apart from the clock in `CanDirectFetch`)

#### 3.9.1 `ConsiderEviction` (section 11)

For an unprotected outbound full-relay or block-relay-only peer **whose headers sync has started** (pure masternode connections never start it), with `now = GetTime()` seconds: its best known block has ≥ the tip's work → clear the timeout; else, no timeout yet, or it caught up to the work recorded at the last check → `timeout = now + CHAIN_SYNC_TIMEOUT` (20 min), record the tip, clear `sent_getheaders`; else, past the timeout: if `sent_getheaders` → `"Disconnecting outbound peer %d for old chain, best known block = %s"`, disconnect; otherwise `maybe_send_getheaders(locator(work_header.prev))` (type by bit 11; `work_header.prev` is `None` on a genesis-only node, §3.4.4, G2), log `"sending %s to outbound peer=%d to verify chain work (current best known block:%s, benchmark blockhash: %s)"` whether or not the throttle let it out, set `sent_getheaders`, `timeout = now + HEADERS_RESPONSE_TIME` (PQ-10). Protection (`m_protect`) is granted only in `UpdatePeerStateForReceivedHeaders` to at most 4 outbound full-relay peers and released in `finalize_node`.

#### 3.9.2 `EvictExtraOutboundPeers(now)`

- **Extra block-relay-only** (`GetExtraBlockRelayCount() > 0`): scan in table order keeping the youngest (highest id) and the previous youngest (`next` is only updated when a new youngest is found, as written); disconnect the youngest unless its last block time is newer than the second's, then the second; only if connected **≥ 30 s** (`v24:6041`) and nothing in flight, logging `"disconnecting extra block-relay-only peer=%d (last block received at time %d)"`, else `"keeping block-relay-only peer=%d chosen for eviction (connect time: %d, blocks_in_flight: %d)"`.
- **Extra full-relay** (`GetExtraFullOutboundCount() > 0`: handshaken full-relay peers above 8, masternode and probe connections excluded): scan in table order, skipping the first `min(2, maxconnections/2)` Tor peers of **any** type (Dash), masternode connections, non-full-relay or disconnecting peers, protected peers, and the only manual-or-full-relay peer on its network; pick the oldest `last_block_announcement`, ties to the higher id; disconnect only if connected **> 30 s** (`v24:6099`, strict) with nothing in flight (`"disconnecting extra outbound peer=%d (last block announcement received at time %d)"`), then clear the stale-tip flag (PQ-11).

#### 3.9.3 `CheckForStaleTipAndEvictPeers` (every 45 s)

`EvictExtraOutboundPeers(now)`; then, if `now > stale_tip_check_time`: when not loading blocks, the network is active, addrman outbound is in use and `TipMayBeStale()` (`last_tip_update < GetTime() − 3 × 150 s`, lazily initialised to now, and nothing in flight), log `"Potential stale tip detected, will try using extra outbound peer (last tip update: %d seconds ago)"` and set ConnMan's flag, else clear it; `stale_tip_check_time = now + STALE_CHECK_INTERVAL` (150 s; effective cadence 180 s). Finally, the first time `CanDirectFetch()` holds (`tip.time > now_sync − 20 × 150 s`, the profile clock, `v23:1407 / v24:1430`), `StartExtraBlockRelayPeers()` (`"enabling extra block-relay-only peers"`). `last_tip_update` is set by `BlockConnected`, so at M2 (no blocks connected) the stale-tip logic fires every 180 s once the first 450 s pass and opens an extra full-relay peer that `EvictExtraOutboundPeers` later drops: exactly what a probe sees from a dashd that receives no blocks.

### 3.10 Misbehaviour

#### 3.10.1 The framework

`Misbehaving(peer, howmuch, msg)` (`v23:1953-1978 / v24:1933-1955`): add to the score; when it crosses 100 from below, set `m_should_discourage` and append `" DISCOURAGE THRESHOLD EXCEEDED"`; log `"Misbehaving: peer=%d (%d -> %d)%s%s"` with `": " + msg` when there is a message; statsd `misbehavior.banned` / `misbehavior.amount`. The flag is raised once; scores never decay.

`MaybeDiscourageAndDisconnect` (`v23:5689-5726 / v24:5833-5870`), the first step of every `send_messages` (so it acts in the same pass as the offending message, except on masternode connections, serviced at most every 100 ms): clear the flag **before** the exemptions; `noban` → `"Warning: not punishing noban peer %d!"`; manual → `"Warning: not punishing manually connected peer %d!"`; a local address → `"Warning: disconnecting but not discouraging %s peer %d!"` and disconnect; otherwise `"Disconnecting and discouraging peer %d!"`, `BanList::discourage(addr)` and `DisconnectNode(addr)`, which drops **every** connection from that IP whatever the port (`"disconnect by subnet%s matched peer=%d; disconnecting"`, PQ-16). M2 ships `DiscourageOnly` (rolling bloom 50,000 / 10^−6, `banman.h:94`), consulted by `OpenNetworkConnection` and the `addr` handler; P4 replaces it with BanMan.

#### 3.10.2 Scores in P2a's command set (re-derived from Core)

| Trigger | Score | Message | v23 / v24 |
|---|---|---|---|
| `addr`/`addrv2` > 1000 (relay enabled) | 20 | `"%s message size = %u"` | `:4363` / `:4408` |
| `inv` > 50,000 | 20 | `"inv message size = %u"` | `:4467` / `:4512` |
| `getdata` > 50,000 | 20 | `"getdata message size = %u"` | `:4574` / `:4618` |
| `notfound` > `notfound_limit` (116 / 50,000) | 20 | `"notfound message size = %u"` | `:5602-5604` / `:5752-5754` |
| `headers`/`headers2` count > limit | 20 | `"headers message size = %u"` | `:5248` / `:5325` |
| every 10th unconnecting `headers` (≤ 8 entries) | 20 | `"%d non-connecting headers"` | `:3179` / `:3163` |
| unconnecting `headers` with > 8 entries | 10 | `"invalid header received"` | `:3362` / `:3346` |
| non-continuous headers | 20 | `"non-continuous headers sequence"` | `:3369` / `:3353` |
| invalid header from CH1 | §3.10.3 | `"invalid header received"` | `:3380` / `:3364` |
| devnet name mismatch | 100 outbound, 1 inbound | none at v23; `"wrong-devnet-name"` at v24 | `:4008,4010` / `:4037,4039` |
| `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit`/`qwatch` at a node with no DKG role | 10 | none | `:5650-5659` / `net_dkg.cpp:938-944` |
| `qgetdata` at a non-masternode (P2a stub) | 10 | `"not a verified masternode or a qwatch connection"` | `llmq/quorumsman.cpp:420-422` / `llmq/net_quorum.cpp:80-83` |
| `qdata` at a non-masternode without `-watchquorums` (P2a stub) | 10 | `"not a verified masternode and -watchquorums is not enabled"` | `llmq/quorumsman.cpp:510-513` / `llmq/net_quorum.cpp:204-208` |
| unsolicited `mnlistdiff` / `qrinfo` (P2a stubs) | 100 | `"received not-requested mnlistdiff. peer=%d"` / `"received not-requested quorumrotationinfo. peer=%d"` | `:5571, 5595` / `:5656, 5741` |

Disconnects without a score: obsolete version, missing desirable services, inbound `fMasternode` at a non-masternode, self-connection, feeler completion, `sendaddrv2` after `verack`, the `sendtxrcncl` cases, a locator above 101, blocks-only violations (message or inv), the MN-probe first-message rule, ping timeout, insufficient-work chains in IBD, `ConsiderEviction`, ADDR_FETCH completion and timeout, inactivity, `mempool` without `NODE_BLOOM` and without the `mempool` permission or `noban` (`v23:5356 / v24:5433`), `getcfilters`/`getcfheaders`/`getcfcheckpt` with an unsupported filter type or no `-peerblockfilters` (`v23:3456 / v24:3440`), and all three of `filterload`/`filteradd`/`filterclear` without `NODE_BLOOM` (`v23:5454, 5485, 5517 / v24:5531, 5562, 5594`; core m-2). Never scored: unknown commands, anything before `version` or before `verack`, decode failures.

#### 3.10.3 `MaybePunishNodeForBlock` (`v23:1992-2040 / v24:1969-2019` (=))

`BLOCK_CONSENSUS`, `BLOCK_MUTATED` → 100 unless via a compact block; `BLOCK_CACHED_INVALID` → 100 for **outbound** peers only (`!m_is_inbound`), unless via a compact block; `BLOCK_INVALID_HEADER`, `BLOCK_CHECKPOINT`, `BLOCK_INVALID_PREV` → 100; `BLOCK_MISSING_PREV`, `BLOCK_CHAINLOCK` → 10; `BLOCK_RECENT_CONSENSUS_CHANGE`, `BLOCK_TIME_FUTURE`, unset → 0; `"peer=%d: %s"` is logged on **every** non-punishing exit, including the compact-block exemptions (`v24:2013-2015`; core n-6). CH1's `HeaderReject` carries CS0's `BlockValidationResult` and reject reason (`high-hash`, `bad-diffbits`, `time-too-old`, `time-too-new` (its clock is `time_source`), `bad-version(0x%08x)`, `bad-fork-prior-to-checkpoint`, `duplicate` (CACHED_INVALID or CHAINLOCK), `prev-blk-not-found`, `bad-prevblk`, `bad-prevblk-chainlock`, `bad-chainlock`, `devnet-genesis`), so the mapping is one `match` in `misbehavior.rs`, shared with P3.

### 3.11 The 74 commands at M2 (merged table)

The M2 node is a non-masternode, wallet-less node in IBD (tip at genesis, headers syncing, masternode sync at BLOCKCHAIN, empty mempool, no sporks). For each command: the normative pre-read behaviour is BS4a §4's (gate class, read shape, decode-failure outcome, trailing bytes); the **post-read** column below gives dashd's outcome in the M2 state, re-derived from Core at both pins (BS4a-r2 N-1's corrections included where they touch the M2 state), and what rdashd does. **Exact** means byte- and score-identical; **stub** means P2a implements the gate/decode and the M2-reachable outcome; **deviation** is an interim difference owned by a later task. Block-relay-only and feeler peers first apply gate 7 (26 commands disconnect without decoding); before `version`/`verack` every command gets the gate 2 / gate 5 log. Each row becomes `policy.rs`'s `m2` and `post_read` fields and a `sim` test. Rows owned by a later task are cited for the M2-reachable outcome only; their full post-read rule is `OwnerDerives`, re-derived and cited by the owner when it implements the handler (DEC-45), with the raw-payload `NetHandler` signature of §2.4.

Spork state that decides several rows: on **mainnet** every spork value is hard-coded (SPORK_21 = 1, all others 0 → active) whatever is received (`v23:spork.cpp:265-276 / v24:171-181`), so ChainLocks and InstantSend are enabled; on testnet, devnets and regtest the defaults (4070908800 = off) apply until a spork arrives.

| Commands | dashd at M2 (post-read, both pins unless marked; Core cites) | rdashd at M2 | Status |
|---|---|---|---|
| `version`, `verack`, `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2`, `sendtxrcncl` (off path), `senddsq`, `qsendrecsigs`, `ping`, `pong`, `addr`, `addrv2`, `getaddr`, `headers`, `headers2`, `inv` (block path; unknown types; type 34 per profile) | per-row line cites in §3.2.3 (`version` `v23:3906-4145 / v24:3935-4176`; `verack` `:4156-4214 / :4187-4259`; the five negotiation messages `:4216-4316 / :4261-4361`; `senddsq` `:4440-4452 / :4485-4497`; `qsendrecsigs` `:4455-4460 / :4500-4505`; `ping`/`pong` `:5379-5450 / :5456-5527`; `addr`/`addrv2` `:4343-4437 / :4388-4482`; `getaddr` `:5312-5347 / :5389-5424`; `headers`/`headers2` `:5232-5269 / :5309-5346` + `:3338-3402 / :3322-3386`; `inv` `:4462-4566 / :4507-4611`) | P2a handlers | exact |
| `inv` (spork entries) | `MSG_SPORK` is the only type requested in IBD (the inv branch, `v23:4462-4566 / v24:4507-4611`) → a later `getdata(spork)` | not requested (`ObjectRequests` no-op) | deviation → SP0 + P5a (M4) |
| `getdata` | > 50,000 → +20; the batch is served by `ProcessGetData` (`v23:2889-2984 / v24:2935-3030`; blocks via `ProcessGetBlockData` `v23:2715-2860 / v24:2736-2900`; txs via `FindTxForGetData` `v23:2861 / v24:2901`): known non-block types we lack → one `notfound`; an unknown block hash → silent; genesis (or a connected block) → `block`; an unknown inv type ends the known-type batch silently (type 34: unknown under v23, served/`notfound` under v24, P2P-04). The three no-score disconnects of `ProcessGetBlockData` (outbound-target historical/filtered block without `download`; a `NODE_NETWORK_LIMITED` node asked below its limit by a non-`noban` peer; v24 only, an unreadable block, `v23:2759-2775 / v24:2786-2827`) are unreachable at M2 (no target, not limited, no block data) | `EmptyServing`: the same, except genesis is not served | exact except `getdata(genesis)` → P4 (M5) |
| `notfound` | > 116 (v23) / > 50,000 (v24) → +20; else each known-type entry completes our request (none at M2) (`v23:5598-5622 / v24:5744-5765`) | P2a | exact |
| `getheaders`, `getheaders2` | locator > 101 → disconnect (`v23:4715-4719 / v24:4765-4769`); mainnet/testnet: tip below minimum chain work → **empty** `headers`/`headers2` by the peer's bit 11 (`v23:4735-4743 / v24:4785-4793`); regtest/devnet: our headers after the fork from the **active chain** (`FindForkInGlobalIndex` + `ActiveChain().Next`, `v24:4784-4815`; empty at genesis) | P2b serving | exact (P2b, M2) |
| `getblocks` | locator > 101 → disconnect (`v23:4597 / v24:4641`); tip at genesis → no reply | P2a stub (decode, the locator rule, no reply) | exact while the tip is genesis; P4 serves (M5) |
| `mempool` | NODE_BLOOM offered (default) → `m_send_mempool` set, empty mempool → no `inv`; without NODE_BLOOM and no `mempool` permission → disconnect unless `noban` (`v23:5349-5377 / v24:5426-5454`) | P2a stub (the gate and the flag) | exact |
| `tx`, `dstx` | IBD return before the read: silent (`v23:4815 / v24:4865`; `dstx`'s already-have/structure order differs by pin, BS4a-r2 N-1(c), unreachable at M2) | gate 7, then silent | exact |
| `block` | mutated → +100 `"mutated block"` (`v23:5288-5293 / v24:5365-5370`); then `ProcessNewBlock` + `BlockChecked` → `MaybePunishNodeForBlock(via_compact_block=false)` (N-1(g)); an unknown prev → +10 through that path; otherwise the header is added and the block discarded (low work) | ignored | deviation → P3 (M3) |
| `cmpctblock` | unknown prev: **in IBD silent**; outside IBD a `getheaders`, no score (`v23:4994-4997 / v24:5071-5074`; core m-1); known prev → header through `MaybePunishNodeForBlock(via_compact_block=true)` (consensus/mutated 0; invalid header/checkpoint/prev +100; chainlock +10; `v23:5008 / v24:5085`, N-1(d)), no fetch in IBD | ignored (matches: IBD at M2) | deviation → P3 (M3) |
| `blocktxn` | unrequested → `"Peer %d sent us block transactions for block we weren't expecting"`, no score (`ProcessCompactBlockTxns`, `v23:3738 / v24:3759`) | P2a stub (log) | exact |
| `getblocktxn` | unknown → log, no reply; genesis → `blocktxn`; the +100 index check applies only within `MAX_BLOCKTXN_DEPTH` 10 of the tip (`v23:4668-4705 / v24:4712-4749`, N-1(e)) | P2a stub (log, no reply) | exact except genesis → P4 |
| `merkleblock`, `cfilter`, `cfheaders`, `cfcheckpt` | no inline branch and no extension handler at either pin: silent, never decoded (listed in `protocol.cpp:99-176`; absent from `ProcessMessage` and from every `NetHandler::commands`, `v23:5627-5681 / v24:5769-5825`) | registry fallback: silent | exact |
| `getcfilters`, `getcfheaders`, `getcfcheckpt` | decoded, then `NODE_COMPACT_FILTERS` not offered → `"peer %d requested unsupported block filter type: %d"`, disconnect (`PrepareBlockFilterRequest`, `v23:3456 / v24:3440`) | P2a stub (BS7 decode, the log, disconnect) | exact |
| `filterload`, `filteradd`, `filterclear` | **all three** disconnect without `NODE_BLOOM` (`v23:5454, 5485, 5517 / v24:5531, 5562, 5594`; core m-2); with it (the default): oversize/misformatted filter +100 `"misformatted bloom filter"`; `filteradd` without a filter (for any peer with a `TxRelay`, fRelay=0 included, N-1 minor) or over 520 bytes +100; otherwise filter state changes (`v23:5452-5532 / v24:5529-5609`) | ignored | deviation → P4 (M5) |
| `spork` | stored, logged and relayed as `inv` when valid and new; +100 when invalid or malformed (no message at v23, `"invalid spork received. peer=%d"` at v24, a log-only pin difference; `v23:spork.cpp:140-145, 211-240 / v24:np:5689-5711`; core n-7) | ignored | deviation → SP0 (M4) |
| `getsporks` | nothing stored → no reply (`v23:spork.cpp:201-209 / v24:np:5713-5736`; v24 logs a repeat, P2P-14) | registry fallback: no reply | exact on the wire |
| `ssc` | log only (not synced) (`node/sync_manager.cpp` v23:334-347 / v24:330-343) | silent | exact on the wire |
| `govsync`, `govobj`, `govobjvote`, `mnauth`, `platformban`, `dsq` | silent: each handler returns before reading while the chain is not synced (`govsync`/`govobj`/`govobjvote`: `governance/net_governance.cpp` v23:63-70, 115-123, 217-225 / v24:64-71, 116-124, 218-226; `mnauth`: `evo/mnauth.cpp:104-108`; `platformban`: the pre-read `IsBlockchainSynced()` guard in the platform-ban handler `v23:np:3814-3824 / v24:3839-3849`; `dsq`: `CCoinJoinServer::ProcessMessage` returns unread before sync (`v23:coinjoin/server.cpp:52 / v24:54`) and the client manager likewise (`v23:coinjoin/client.cpp:172 / v24:66`)); the `track_request` limits, `govobjvote` penalties and `mnauth` cross-connection rules (N-1(a,b,l)) are unreachable at M2 | silent | exact |
| `dsa`, `dsi`, `dss` (masternode only), `dsf`, `dsc`, `dssu` (no mixing session; wallet builds only), `qsigsesann`, `qsigsinv`, `qgetsigs`, `qbsigs`, `qsigshare` (masternode only) | silent: `CCoinJoinServer::ProcessMessage` is registered only on masternodes and returns before reading while unsynced (`v23:coinjoin/server.cpp:50-52 / v24:52-54`); `CCoinJoinClientManager::ProcessMessage` (`v23:coinjoin/client.cpp:169-172 / v24:63-66`) and `CCoinJoinClientSession::ProcessMessage` (`v23:205 / v24:97-100`) return unsynced or without a session; sig-share messages are read only on masternodes (`llmq/signing_shares.cpp` v23:453-459 / `llmq/net_signing.cpp` v24:47-55) | silent | exact |
| `qcontrib`, `qcomplaint`, `qjustify`, `qpcommit`, `qwatch` | +10, no message (v23 inline rule `:5650-5659`; v24 `NetDKGStub` `net_dkg.cpp:938-944`); the pin-dependent intake shapes (N-1(i)) apply only on a node with a DKG role | registry fallback +10 | exact |
| `qgetdata` | +10 `"not a verified masternode or a qwatch connection"`, not decoded (`v23:quorumsman.cpp:420-422 / v24:net_quorum.cpp:80-83`); the v24 67-byte +100 and the rate-limit/type rules (N-1(j)) come after that gate | P2a stub | exact |
| `qdata` | +10 `"not a verified masternode and -watchquorums is not enabled"`, not decoded (`v23:llmq/quorumsman.cpp:510-513 / v24:llmq/net_quorum.cpp:204-208`) | P2a stub | exact |
| `mnlistdiff`, `qrinfo` | +100 `"received not-requested mnlistdiff. peer=%d"` / `"received not-requested quorumrotationinfo. peer=%d"`, not decoded (`v23:5571, 5595 / v24:5656, 5741`) | P2a stub | exact |
| `getmnlistd` | +1 `"getmnlistdiff failed …"` (v23) or, for the tip-at-genesis case, an empty-list `mnlistdiff` for (null or genesis, genesis); v24: 0 when block data is missing (DKG-13) (`v23:5534-5551 / v24:5611-5636`) | ignored | deviation → P6b (M5) |
| `getqrinfo` | v23: +1 (malformed: exception, unscored); v24: +1, +100 malformed or > 4096 bases (DKG-12), 0 for a header-only target (`v23:5575-5591 / v24:5660-5687`) | ignored | deviation → P6b (M5) |
| `qfcommit` | v23: a null commitment +100 (`llmq/blockprocessor.cpp:77-81`) and an unknown LLMQ type +100 (`:83-89`) **before** the quorum-hash lookup; a genesis quorum hash with a non-zero index +100 (`:111-116`); `HasMinedCommitment` only at `:126` → 0, as is a local minable commitment with ≥ signers (`:123-143`; N-1(k), core m-3). v24: +10 `"unrequested quorum commitment"` first (P2P-12, `blockprocessor.cpp:115-119`), then null/unknown-type +100 and the mined/minable returns (`:173-190`) | ignored | deviation → LQ2/P5a (M4) |
| `qsigrec` | +100 for an LLMQ type not on this network (`v23:llmq/net_signing.cpp:29-32 / v24:56-59`); otherwise silent | ignored | deviation → SG1a/P5a |
| `clsig` | testnet/devnet/regtest: silent (spork 19 off). Mainnet: +10 (v23 after a failed verification, `chainlock/handler.cpp:105-131`; v24 `"unrequested clsig"`, `np:5794-5819`, P2P-12) (`v23:np:5666-5674 / v24:5794-5819`) | ignored | exact off mainnet; mainnet deviation → LK1a/P5a (M4) |
| `isdlock` | testnet/devnet/regtest: silent (spork 2 off). Mainnet: structural +100, unknown cycle +1, else queued and dropped by the worker, with the asynchronous +20 (`instantsend/net_instantsend.cpp` v23:67-167 / v24:67-198; N-1 minor) | ignored | exact off mainnet; mainnet deviation → LK1b (M5) |
| any string not in the list | `"Unknown command"` (`v23:5684 / v24:5828`) | the same | exact |

So on regtest and testnet, where M2's functional tests and O13a run, every command is exact except `inv`/`spork` handling of sporks, the BIP37 trio, `block`/`cmpctblock`, `getmnlistd`/`getqrinfo`, `qfcommit` and `qsigrec`, each owned by a scheduled task. P2a's stubs are tiny (no decoding beyond BS7's small types) and are deleted when the owner registers the real handler; a test enforces that a command never has both a stub and an owner.

### 3.12 Every observable behaviour, per profile (merged enumeration)

The charter rule: a peer spoofing a user agent should find it very hard to tell rdashd from dashd. This table lists every behaviour a remote peer, or a passive observer of our traffic, can see that involves P2a's code, with how the design reproduces it under each profile and what proves it. Numbers are PRD §4.2 / A1 §4.10; "new" rows are behaviours the two designs found that the checklist does not list. "=" means both profiles behave identically (verified in the cited sections).

| # | Behaviour | Profile | Reproduced by | Proved by |
|---|---|---|---|---|
| 1 | `version` layout and values (137-byte default; services; time = local not adjusted; addr_recv rule and its services echo; zero addr_from; nonce; UA; start height; relay; challenge; `fMasternode`) | `nVersion` 70240 / 70242; UA `23.1.8` / `24.0.0`; rest = | §3.3.4 | O13a v1 + v2 transcripts vs both pins; the 137-byte fixture per profile (nonce/challenge masked) |
| 2 | service bits | = (0x1C05 / 0x1C45 / pruned; the MN `peerblockfilters=0` difference is `InitRules.mn_init`) | §3.3.3 | O13a; unit test per option combination |
| 3 | who speaks first, and when | = | outbound `initialize_node` is synchronous on the `opencon` thread and pushes `version` before the node is in the table (§2.2); inbound only after receiving one (P2b) | O13a-out; a connect→`version` latency feature in O13a-out and O13b (core M-3) |
| 4 | after the peer's `version`: `sendaddrv2` (≥ 70223), [`sendtxrcncl`], `verack`, `getaddr` (every outbound type except block-relay-only, **feelers and addr-fetch included**); feelers then disconnect | = | §3.3.5 | O13a per connection type (full, block-relay, feeler, manual, addr-fetch) |
| 5 | after `verack`: [`mnauth`], `sendheaders2`/`sendheaders`, `sendcmpct(0,1)` iff `CanRelay`, `senddsq(1)` unless tx-rejecting, [`qwatch`], then on the same pass `ping`, [`addr`], [`getheaders2`/`getheaders`] | = | §3.3.6 | O13a |
| 6 | messages before `version` / non-allowed before `verack` ignored, logged, unscored | = | §3.2.1 gates 1, 2, 5 | `diff_p2p_sequence`; FT `p2p_leak`, `p2p_timeouts` |
| 7 | immediate `ping`, every 2 min after the last send, 20 min timeout; `getheaders` policy (2 min spacing, type by bit 11, one sync peer in IBD, "more" after a full batch **by the bit-11 limit** (PQ-24), the block-inv trigger, unconnecting headers); empty `headers`/`headers2` below min chain work (P2b serves) | = except the sync-start and headers-timeout clock (P2P-16; a `sim` and O13a-out variant with ≥ 5 skewed-`nTime` outbound peers shows the rule moving under `V23_1` and not under `V24_0`, arch m15) | §3.4, §3.5 | FT `p2p_ping`, `p2p_initial_headers_sync`; KS on ping cadence (O13b); `diff_p2p_headers*` incl. both mixed bit-11/message-type cases; the L2 header-sync replay (§6.6) |
| 11 | unknown commands ignored without score; `feefilter`, `wtxidrelay`, `reject`, `alert`, `getaddrv2`, `sendpackages` never sent and unknown (absent from `protocol.cpp:18-94`) | = | gate 10; none is in `POLICY` or ever constructed | `diff_p2p_unknown`; a grep test that the strings appear nowhere in `dashd-p2p` |
| 12 | misbehaviour model (P2a's part: framework and §3.10.2's scores) | = except `notfound_limit` | §3.10 | `diff_p2p_*`; FT `p2p_invalid_messages` (P2a's cases) |
| 13 | blocks-only violations (messages and invs), incl. inv type 34 | type 34: unknown under `V23_1`, a `tx`-class violation under `V24_0` (P2P-04) | gate 7, §3.4.7 | `diff_p2p_inv` per profile; FT `p2p_blocksonly` (P2a's part) |
| 14 | size limits: `inv`/`getdata` 50,000, `addr` 1000, `headers` 2000 / 8000, `notfound` 116 / 50,000, locator 101 | `notfound` per profile | §3.10.2 | `diff_p2p_*` at the boundaries |
| 15 | address relay: token bucket (0.1/s, 1000, +1000 after our `getaddr`), relay to 1–2 peers per 24 h epoch, freshness and size rules, 2 h penalty, one cached `getaddr` answer per inbound connection (21–27 h, 23 %, 1000), `getaddr` ignored on outbound, `sendaddrv2` after `verack` disconnects, ADDR_FETCH completion and 300 s timeout | = | §3.6 | FT `p2p_addr_relay`, `p2p_addrv2_relay`, `p2p_addrfetch`, `p2p_getaddr_caching` (with P2b); `diff_addrman` |
| 21 (part) | `headers2` encoding: reply mode vs announcement mode, per-message version cache; 8000/2000 limits | = | §3.4.10 (BS7, two modes) | `diff_headers2` (BS7) + `sim` fixtures of both modes against Core captures |
| 25 | connection behaviour: 8 full + 2 block-relay + anchors; feelers Exp(2 min) + U[0,1 s); extra block-relay Exp(5 min) once direct fetch is possible; extra full-relay on a stale tip (450 s, checked every 180 s); extra-network peer Exp(5 min); Dash onion extras; ADDR_FETCH rules; DNS seeding (`x1.`, ≤ 32, 11 s / 5 min, skipped at 2 peers); fixed seeds at 60 s (ages U(1 w, 2 w]); linger 100 ms then close at the next socket-loop tick; `shutdown(SHUT_WR)` once flushed; **regtest/devnet: no outbound accounting, so no block-relay or feeler connections, up to 11 full-relay, and one unprotected extra evicted every 45 s after 30 s connected** (core m-12) | = | §3.8 | FT `p2p_add_connections`, `feature_anchors`, `p2p_dns_seeds`, `p2p_addrfetch`, `feature_config_args` (`-seednode`); KS on feeler/extra-peer timers (O13b); the 24 h soak (§6.7). The ladder itself cannot be diffed in the regtest lab (PQ-12): residual risk for O13b unless a DEC-16 hidden regtest switch runs the accounting block on both binaries (§8 Q3) |
| 26 | inactivity: first message within 60 s (dropped 60–62.5 s after connect), 20 min send/receive, handshake timeout | = | §3.8.7 | FT `p2p_timeouts` |
| 27 | devnet: `devnet.<name>` in the UA; mismatch +100 outbound / +1 inbound | = (log text differs) | §3.3.2, §3.3.5 step 9 | FT `p2p_connect_to_devnet` |
| +C | cross-peer ordering: peer A's `headers` is fully processed (in the index, best header updated) before peer B's next message; same-peer reply order | = | §3.1.5, §2.5.3 (`TipFacts` and the index before the reply) | **Two probes whose answer depends on the block index, not on the active chain** (core M-4, arch M7; the r0 `getheaders` probe was vacuous: `getheaders` is served from the active chain, `v24:np:4784-4815`, empty below min chain work, `:4785-4793`, and genesis-only at M2). (1) *headers probe*: A sends a valid-PoW regtest `headers` batch ending in H; after evidence that A's message was processed (our follow-up `getheaders` to A, or a `pong` to a `ping` A sent after the headers), B sends a one-header `headers` whose prev is H: processed in order it connects and is accepted silently; otherwise it is unconnecting and B gets a `getheaders` with `nUnconnectingHeaders` incremented (`v24:3138-3165`). (2) *inv probe*: A sends headers ending in H and, in the same TCP write window, B sends `inv(MSG_BLOCK, H)` with B's inv path armed: processed in order H is already had and B gets no `getheaders`; otherwise B does. Both run against both pins; the race under the per-pass shuffle is measured statistically (both binaries shuffle, so the distributions must match, not a single outcome). A1 §4.10's block/tx form of +C stays for P3/P5 |
| +D (part) | scheduling under load: backpressure (`fPauseSend` stops polling; `fMoreWork` ignores send-paused peers), the 100 ms throttle on MN connections only | = | §3.1 | `sim` under 125 peers with backpressure; O13b latency KS (lower latency allowed only while NFR-FP-02 holds; the header batch → `getheaders` gap and the per-format cost ratio are classifier features, §3.4.9) |
| new-1 | quorum-priority receive queue: `clsig`, `isdlock`, `qsig*` overtake earlier normal messages (100:1) | = | §3.1.3 | `sim` unit test; O13b probe "`ping` then `clsig` in one segment" (with LK1a) |
| new-2 | TCP segment boundaries: a burst of messages leaves as one segment (`MSG_MORE`), `TCP_NODELAY` set | = | P1 writer coalescing (§3.8.7) | passive capture comparison (O13b n-grams over segments) |
| new-3 | linger/close timing: FIN when the queue drains, close 100–600 ms after the mark (the 500 ms idle tick), RST if unread data | = | §3.8.7 | O13b disconnect-timing probe |
| new-4 | `addr_recv.services` echoes addrman's belief (`NODE_NETWORK` for seeds, `NODE_NONE` for names) | = | §3.3.4 | O13a fixture with a name-based `-connect` and a seed-learned address |
| new-5 | `getaddr` and `sendaddrv2` to feelers before the disconnect; `addrman.Good` for feelers and block-relay peers | = | §3.3.5 steps 11, 16, 17, 20 | O13a feeler transcript; `diff_addrman` |
| new-6 | `getaddr` reply on the **next** pass (no Poisson delay) for an inbound peer's first `getaddr`, provided no earlier `addr`/`addrv2` from that peer had already enabled relay (then `next_addr_send` is already Exp(30 s) out; core n-4) | = | §3.6.4 | FT `p2p_addr_relay` (P2b) |
| new-7 | a malformed or empty `headers`/`headers2` still clears the 2-minute `getheaders` throttle | = | §3.2.3 pre-read effects | `diff_p2p_headers2` sequence case |
| new-8 | time-offset sampling from outbound peers only; the 200-sample freeze; `version.nTime` unadjusted | = (consumed by consensus only under `V23_1`) | §3.3.8 | unit tests; `getpeerinfo.timeoffset` DIFF-RPC (RP1a) |
| new-9 | `qsendrecsigs` is never sent by a plain node | = | §2.6 hook (P7/AM1) | O13c |
| new-10 | the v1 reconnect after an early v2 close: a second TCP connection with a V1 `version`, same type and grant | = | §3.8.5 | FT `p2p_v2_transport` (P1) + O13a v2 matrix |
| new-11 | `ConsiderEviction`'s "verify chain work" `getheaders` and the 20 min old-chain disconnect; the extra-peer evictions (youngest block-relay ≥ 30 s; oldest-announcement full-relay > 30 s) | = | §3.9 | `denialofservice_tests` port (`outbound_slow_chain_eviction`, `stale_tip_peer_management`, `block_relay_only_eviction`) with `sim` equivalents of `ConnmanTestMsg`/`AddTestNode`; an rdashd-authored `sim` scenario for the extra-peer evictions (no Core FT covers outbound eviction: `p2p_eviction.py` is inbound-only) |
| new-12 | the full-batch rule keyed by the peer's bit 11 (PQ-24): a bit-11 peer answering `getheaders2` with 2000 plain `headers` gets no "more" request and may be disconnected for insufficient work in IBD; a non-bit-11 peer sending `headers2` of 8000 is treated as short | = | §3.4.3 steps 10–11 | `diff_p2p_headers`/`_headers2` mixed cases; `quirk_pq_24_*` |
| new-13 | the gap between a connecting header batch and the follow-up `getheaders` is shorter than dashd's on a CPU-bound sync, and its `headers`:`headers2` ratio is ~1:1 rather than ~2:3 | = | §3.4.9 (hash once) | an O13b classifier feature (PRD R-8; errata row 21); NFR-FP-02's AUC ≤ 0.55 is the acceptance, not a per-feature match |
| new-14 | `ConsiderEviction`'s verify-chain-work `getheaders` on a genesis-only node carries one locator hash (genesis) under `V23_1` and an empty locator under `V24_0`, 20 min after the handshake with a peer that serves no headers; the 2-minute disconnect stage follows under both | **profile** (`P2pRules.null_locator`, PQ-23) | §3.4.4, §3.9.1 | `diff_p2p_multi_headers`'s genesis-eviction session (locator bytes, version word, the later disconnect) under each profile; an O13a-out variant with mocktime advanced 20 min |
| new-15 | send backpressure trips on Core's retained-memory charge (88 bytes per queued 8-byte `pong`; a partly sent V1 payload counts in full), which decides when polling stops, the `pong` backlog and cross-peer work for a peer that stops reading | = | §3.8.7 (G3) | stalled/partially draining writer tests at the boundary vs Core (§6.7) |

**Unprompted traffic of the M2 node** (everything it sends that is not a direct reply), for O13a/O13b:

| Message | When | Timing |
|---|---|---|
| `version` | outbound connect | at once, on the connecting thread, before the node joins the table (§2.2) |
| `sendaddrv2`, [`sendtxrcncl`], `verack`, `getaddr` | the peer's `version` | same pass |
| `sendheaders2`/`sendheaders`, `sendcmpct`, `senddsq` | the peer's `verack` | same pass |
| `ping` | first pass after `verack`; then 2 min after each send while none is outstanding; RPC `ping` | pass cadence |
| `getheaders2`/`getheaders` | initial sync (one peer in IBD unless the best header is recent), "more" after a full batch, unconnecting headers, block inv, `ConsiderEviction` | ≥ 2 min apart per peer unless a `headers` reply clears the throttle |
| `addr`/`addrv2` | relayed addresses (to 1–2 peers per address); self-advertisement (not in IBD, so never at M2); `getaddr` replies (inbound, P2b) | Exp(30 s) flush per peer |
| `inv`/`headers`/`headers2`/`cmpctblock` | new tip (never at M2) | next pass |

Replies the M2 node sends: `pong`, `addr`/`addrv2` (to inbound `getaddr`, P2b), `headers`/`headers2` (P2b), `notfound` for a `getdata` it cannot serve (§3.11). **The mask O13a applies at M2** (§1.4): dashd additionally sends `getsporks` U(0, 6] s after `verack`, `mempool`/`govsync` after the BLOCKCHAIN asset finishes, `getdata(spork)` on spork invs, and the `full-sync` disconnect; none is P2a's. Behaviours 8, 9, 10, 16–20, 22–24, 28–31 and +A, +B, +E belong to other tasks; §2.6 lists the hooks they use and §3.11 the M2 node's interim behaviour for their commands.

### 3.13 The O17 P2P rows

| P2a implements (reads the field) | Rows |
|---|---|
| `Identity.{protocol_version, ua_version, client_version}` | P2P-01 |
| send-side version gates through the common version | P2P-03 (P2a's own gates: `sendaddrv2`, the headers2 limit; later tasks inherit the mechanism) |
| `P2pRules.notfound_limit` (the size check) | P2P-10 (P5 owns the per-entry handling) |
| `P2pRules.msg_asset_unlock` in `IsKnownType` for `inv`/`getdata`/`notfound` | P2P-04 (**P2a is a reader**; O17 lists P5, P6a, BS7, O13b, BS10, MP1a) |
| `UngatedRules.time_source` for sync start, the headers timeout, `CanDirectFetch` | P2P-16 (**P2a is a reader**; O17 lists P2b, P3, ND1) |
| `InitRules.bind_failure_fatal` (consumed as a ConnMan option) | P2P-33 |
| carries `Identity.min_mn_proto_version`; records `version_services`/`version_relay_txs` | P2P-02, DKG-01 (read by DK1/LQ4a) |

| P2a provides the seam, another task implements | Rows (owner) |
|---|---|
| `ObjectRequests` | P2P-05..-09, -11 (P5, SP1, GV1b, LK1b) |
| `OrphanStep` | P2P-13 (P5) |
| `TxRelay` (the `PushInv` gate) | P2P-15 (P5) |
| `NetHandler` registry, `PeerConsumeGetDataResponse` | P2P-12, -14, -20, -21, SIG-14/-15/-18, DKG-03/-05/-09..-13, MNO-03/-06/-07/-11, INTG-01 |
| `Serving` | P2P-17 (P4) |
| `NetPermissionFlags`, verified-MN fields | P2P-18 (P7, AM1) |

No other P2P row touches P2a's code. Two v24-only behaviours in P2a's area have no O17 row: the ASMap health check (§3.0 #9, §8 Q6) and v24's `FinalizeNode` cleanup of a discouraged peer's pending recovered sigs (internal; SG1a's, behind the `finalize` hook).

### 3.14 Quirks (deduplicated; proposed P2P register entries)

R4's register (1–34, A1's 35–38, CS1's 39, CS2's 40) is consensus-only. P2a proposes these P2P quirks, numbered PQ-n until the manager assigns register numbers (§8 Q4). Each gets a named test (`quirk_pq_N_*`) and is replicated, not fixed. Sources: Opus PQ-1…17 and Fable Q-P2a-1…10, merged; duplicates collapsed.

| PQ | Quirk | Core | Was |
|---|---|---|---|
| 1 | A `version` whose optional tail fails to decode has already applied `SetServices`, the services check and the obsolete-version check, but leaves the peer pre-version, so it may resend | §3.3.5 | Opus PQ-1 = Fable Q-3 |
| 2 | Any `headers`/`headers2`, including a malformed or empty one, clears the 2-minute `getheaders` throttle (unless blocks are loading) | `v23:5241 / v24:5318` | Opus PQ-2 = Fable Q-2 |
| 3 | `headers2` reply mode does not record the first header's version; announcement mode does; the decoder always does | §3.4.10 | Opus PQ-3 |
| 4 | The `headers2` oversize log says `headers message size` | `v23:5248 / v24:5325` | Opus PQ-4 |
| 5 | The quorum-priority receive queue reorders a peer's messages (100:1) | §3.1.3 | Opus PQ-5 = Fable Q-1 |
| 6 | The time-offset median freezes once 200 samples are held (an even count), and no source beyond the 200th is counted; `version.nTime` and `nTimeOffset` use the unadjusted clock, the offset feeds only the sync decisions under `V23_1` | `timedata.cpp:40,50,76` | Opus PQ-6 + Fable Q-7 |
| 7 | `GetAddr` filters by `GetNetClass()` while `Select` and the counts use `GetNetwork()` | §3.7.2 | Opus PQ-7 |
| 8 | The self-announcement exemption from the 2 h penalty compares IPs only, while addrman identity includes the port | `v24:addrman.cpp:568` | Opus PQ-8 |
| 9 | A token from the addr bucket is spent before the service, ban and reachability filters; `getaddr`'s +1000 can push it above the cap | §3.6.3 | Opus PQ-9 |
| 10 | `ConsiderEviction` records `sent_getheaders` and extends the timeout even when the throttle suppressed the `getheaders` | §3.9.1 | Opus PQ-10 |
| 11 | Extra block-relay eviction uses `≥ 30 s`, extra full-relay `> 30 s`; the youngest/second-youngest scan does not update the second when an id falls between them | `v24:6041, 6099` | Opus PQ-11 |
| 12 | Regtest and devnet skip the whole outbound accounting block (`AllowMultipleAddressesFromGroup`): no feelers, no block-relay connections except anchors, no netgroup rule, `count_failures` false (unless `-maxconnections ≤ 1`); up to 11 full-relay peers, of which `EvictExtraOutboundPeers` churns one unprotected extra every 45 s after 30 s connected | `v23:net.cpp:3034 / v24:3028`; `GetExtraFullOutboundCount` `v24:net.cpp:2850` | Opus PQ-12 (Fable's narrower reading corrected; core m-12, n-5) |
| 13 | Anchors are matched by `IsBlockRelayOnly()`, which includes any peer under `-blocksonly` without `relay` | §3.8.3 | Opus PQ-13 |
| 14 | `getaddr` (and `sendaddrv2`) go to feelers and addr-fetch connections, queued before the disconnect | §3.3.5 | Opus PQ-14 = Fable Q-4 |
| 15 | `pong` timing uses the socket read's timestamp, so queueing delay is excluded from `pingtime` | §3.5 | Opus PQ-15 |
| 16 | Discouragement disconnects every peer on the IP, whatever the port | §3.10.1 | Opus PQ-16 |
| 17 | A `peers.dat` that fails Core's consistency check is silently replaced by an empty addrman; an incompatible version is renamed `.bak`; other read errors are fatal | §4.1 | Opus PQ-17 |
| 18 | `addrman.Good()` on every outbound type incl. feelers and block-relay; `Connected()` only for successful, unscored, non-block-relay outbound peers | `v24:4149`; `v24:1848-1853` | Fable Q-5 |
| 19 | `GetAddr_` permanently reorders `vRandom`, so successive `getaddr` answers depend on earlier calls (and on the v24 asmap health check, which calls `GetAddr(…, filtered=false)` twice, `v24:net.cpp:5009-5020`); `peers.dat` dumps do **not** depend on it (`Serialize` iterates `mapInfo`), but a load that prunes entries leaves `vRandom` in library iteration order | `v24:addrman.cpp:815-851`; `:386-395` | Fable Q-6 (core m-10, M-6) |
| 20 | `GetChance`/`Select_` evaluate doubles (`pow(0.66, n)`, `× 1.2` per miss) in a peer-visible selection path | `v24:addrman.cpp:96-109, 788-794` | Fable Q-10 |
| 21 | `fMoreWork` ignores send-paused peers; the MN send-throttle stamp resets only when the throttle fires | `v24:net.cpp:3686-3690, 3704` | Fable Q-8 |
| 22 | The headers count limit is checked before any header is read, so an oversized `headers` with garbage after the count costs exactly +20 and nothing else | `v23:5246-5250 / v24:5323-5327` | Fable Q-9 |
| 23 | `GetLocator(nullptr)` differs between the pins (the active tip's locator at v23, an empty locator at v24) and **is reachable** through `ConsiderEviction`'s verify-chain-work request on a genesis-only node whose sync peer serves no headers for 20 min; the wire result differs per profile (G2) | `v23:chain.cpp:37-38 / v24:chain.cpp:35-36`; `v23:np:5846 / v24:5992` | new (reconcile; reclassified in rev2) |
| 24 | The `headers` size check is keyed by the message type received, but the "more headers" request and `may_have_more_headers` (the insufficient-work disconnect) are keyed by the peer's advertised bit 11; the two disagree for a bit-11 peer sending plain `headers` or a non-bit-11 peer sending `headers2` | `v23:5247 / v24:5324` vs `v23:3386-3396 / v24:3370-3380` | new (core M-1, arch m9) |
| 25 | `GetChance` keeps a negative `nAttempts` read from `peers.dat` (never validated by `CheckAddrman`), giving a chance above 1 or `inf`; reachable only from a crafted local file | `v24:addrman.cpp:96-109` | new (core m-9) |

Recommendation for the register: 1–5, 7–17, 19–20, 23–24 are peer-visible and should get numbers (23 is also a profile row); 6, 18, 21, 22, 25 are port anchors (tests, no register entry) unless the manager prefers to register all.

### 3.15 Errata to A1, the PRD, the PLAN and O17 (merged)

| Source | Says | Core at both pins | Proposed fix |
|---|---|---|---|
| A1 §2.14, §4.10 #5; PRD §4.2 #5 | `qwatch` after `ping` and `getheaders` | pushed in the `verack` handler right after `senddsq` (`v23:4193-4201 / v24:4224-4232`) | reorder |
| A1 §2.14 "Order after the peer's version", §4.10 #4; PRD #4; R4 L6 | `getaddr` "on outbound full-relay connections only" | every non-inbound connection except block-relay-only, feelers and addr-fetch included (`SetupAddressRelay`, `v24:4116-4132`) | reword |
| A1 §2.14 reader bullet | messages queued "in arrival order" | two queues, quorum-priority first at 100:1 (§3.1.3) | add |
| A1 §2.14 loop sketch | `more_work |= process_messages(peer)`; the MN timer "reset" after the pass | `fMoreWork |= (more && !fPauseSend)`; the throttle stamp resets only when the check passes (`v24:net.cpp:3686-3690, 3704`) | correct |
| A1 §2.14 `NetHandler` | `on_send_tick`, `on_connected`, `on_disconnected`; `process_message` "never blocks" | do not exist; overlays push from their own tasks; Core's handlers may block where Core does (§3.2.6) | drop; adopt §2.4's trait |
| A1 §2.14 "Later", §4.10 #8; PRD #8 | `getsporks` 0.5–2 s after the handshake; `govsync` ≈ 8 s | `SyncManager` tick only: U(0, 6] s after `verack`; `govsync` only after the BLOCKCHAIN asset; mockable 6 s gate (`v23:node/sync_manager.cpp:161 / v24:157`) | correct (SP1's design) |
| A1 §2.14 seeds | "3 at a time" | true, but each network has one seed; the prefix is always `x1.` | note |
| A1 §2.14 "Global helpers run as small actors: AddrMan, BanMan and ConnMan"; §3.1 | — | AddrMan as a synchronous mutex (§2.2); BanMan a sync trait; ConnMan tasks over the shared table | amend |
| A1 §2.10.2 | `ChainView` published at most every 100 ms in IBD | the actor needs the best header and tip facts unthrottled (§2.5.3) | add `TipFacts` |
| A1 §2.18, §2.2 | `NodeClock` in `dashd-node` | `dashd-chainstate` and `dashd-p2p` need it (§2.7) | new crate `dashd-clock` |
| A1 §4.10 #25 | "feelers every 2 min that disconnect after `version`" | right on mainnet/testnet; **no feelers on regtest/devnet** (PQ-12) | note (affects O13a's matrix: use `addconnection`) |
| A1 §4.10 #26 | "60 s for the first message" | the check fires after `peertimeout` when **either** direction has never carried a byte, or the handshake is incomplete (§3.8.7) | reword |
| A1 §4.10 #7; PLAN P2a acceptance "behaviours 1–7" | behaviour 7 includes "empty headers below minimum chain work" | that reply is `getheaders` serving (P2b) | P2a covers 7's ping and `getheaders` policy; P2b the empty reply |
| O17 P2P-04 consumers | P5, P6a, BS7, O13b, BS10, MP1a | the `inv` handler's `IsKnownType` is P2a's | add P2a |
| O17 P2P-16 consumers | P2b, P3, ND1 | sync start, the headers timeout and `CanDirectFetch`'s first use are P2a's | add P2a |
| O17 (none) | — | v24's ASMap health check (§3.0 #9) | add a row (§8 Q6) |
| O17 (none) | — | `GetLocator(nullptr)` (active tip at v23, empty at v24) is reachable on the wire through `ConsiderEviction` (§3.4.4, G2) | add a `profile` row keyed `P2pRules.null_locator` (§8 Q12) |
| O17 #7283 | "not a delta" | linger, sweep cadence and MN throttle use the system clock at v23, the steady clock at v24; they differ only across wall-clock steps | keep; rdashd uses the steady clock under both profiles (declared here) |
| PLAN P8 | asmap | P2a needs the interpreter for bucket parity | P2a ports the interpreter; P8 keeps the UX and the health check |
| PLAN (no O1b) | Fable's design cites "O1b/O2a" for new oracle entries | PLAN has O1, O2a, O2b; no O1b | the new entries go to a new task O1c (§8 Q1) |
| CS0 `Release { V23_1_8, V24_0 }` | — | O17 §4.2 names the lines `V23_1` / `V24_0` | align CS0's variant name with O17 (editorial) |
| PRD NFR-FP-03 ↔ §3.4.9 | "Processing latency may be lower than dashd's as long as NFR-FP-02 holds" | the clause is conditional: a shorter header-batch → `getheaders` gap, and its `headers`:`headers2` ratio, are timing features the sync peer sees directly | **add them to O13b's classifier features** (PRD R-8); NFR-FP-02's AUC ≤ 0.55 is the acceptance (arch m10; DEC-51j) |
| A1 §2.10 `ChainCommand::ProcessHeaders { headers: Vec<BlockHeader>, … }` | — | the payload is `HeaderBatch { headers, hashes }` (§2.5.3 R7) | amend (arch M5e) |
| A1 §2.10.2 "locators run as `ChainCommand::Query` on V"; §2.10 l.966 plain `hashbrown::HashTable` | — | locators walk immutable `prev`/`skip` lock-free; the hash index must be concurrent and append-only (§2.5.3 R1, R2) | amend (arch M5a) |
| A1 §2.14 "the writer runs a bounded send queue with backpressure equal to `-maxsendbuffer`" | — | Core's `PushMessage` never blocks (`v24:net.cpp:4857-4905`); the outbox is unbounded, `fPauseSend` only stops polling (§2.2.1) | amend (arch M1) |
| A1 §3.1 `net` row ("the ConnMan/AddrMan/BanMan actors") and "nothing on the `net` runtime blocks" | — | ConnMan's `opencon`/`addcon`/`dnsseed` loops block (connects, semaphores, the AddrMan mutex) and run on OS threads as Core's do (§2.2) | amend (arch m6) |
| A1 §4.10 +C | "peer A sends a block or tx and peer B immediately sends `getheaders`/`getdata`/`mempool`" | at M2 only a headers-only form is observable: the connecting one-header batch and the block-inv probes (§3.12 +C) | add the M2 form; keep the block/tx form for P3/P5 (arch M7) |
| A1 §3.9 row 6 "X11 parity" | — | measured user-space instructions per byte for an 80-byte header: Core 2,120; `rs-x11-hash` 2,301 (1.085×); base-sdk `dash-pow` @33fcc59 ~4,950 (~2.3×) (`dash-sha256-sh1`); parity needs XH1 (§5.2; DEC-51b) | amend (arch M3) |
| PLAN: O13a depends on P2b; the network and test RPCs (`getpeerinfo`, `getnetworkinfo`, `addnode`, `addconnection`, `disconnectnode`, `setnetworkactive`, `getconnectioncount`, `ping`, `setmocktime`, `mockscheduler`, `getblockchaininfo`, `getchaintips`, `syncwithvalidationinterfacequeue`) have no M2 owner; P2a is one L | — | O13a splits into O13a-out (after P2a-core/-net phase D) and O13a-in (after P2b); the RPCs form an early RP0 M2 slice; P2a is two tasks (§7; DEC-51e) | amend PLAN (arch M9, g) |

---

## 4. Persistence, crash consistency and recovery

P2a persists two files. Everything else (peer state, the node table, the address-response cache, the discouragement filter until P4) is in memory and rebuilt at start, as in Core.

### 4.1 `peers.dat`

- **Format: Core's `V4_MULTIPORT`** (format byte 4, compat byte 36, `nKey`, `nNew`, `nTried`, `1024 ^ 2^30`, the new entries, the tried entries, the 1024 bucket lists of indexes into the new entries, the asmap checksum), in Core's envelope (network magic ‖ payload ‖ SHA256d(magic ‖ payload)), `CAddress` in the disk form with the `0x20035B60` version word. FR-P2P-19 allows our own format if Core's is importable; both designs chose Core's: it is the natural port, makes the files interchangeable in both directions, gives IM1a nothing to do, and lets the parity test exchange files. DEC-11 is about chain state; §8 Q2 asks the manager to confirm.
- **Entry order.** Core writes entries in `std::unordered_map` iteration order (library-dependent), so byte equality with Core is neither possible nor needed; rdashd writes ascending id order. Files are compared semantically (§6.3).
- **Write**: every 15 min (`DumpAddresses`) and at shutdown: a temporary `peers.<4 random hex digits>` (`"%s.%04x"` of the prefix, `addrdb.cpp` v24:59-61), `fsync`, rename over `peers.dat`, then `fsync` the directory (Core skips the last step; unobservable, closes A1 §3.5's "renamed but not durable" window). A crash leaves the old or the new file.
- **Read** (`LoadAddrman`, `addrdb.cpp` v23:185-224 / v24:194-231, same logic): missing → empty addrman, dump it; `DbInconsistentError` (`CheckAddrman` failure) → **silently** replace with an empty addrman and dump it (`"Creating peers.dat because of invalid or corrupt file (%s)"`; PQ-17); `InvalidAddrManVersionError` (compat above ours) → rename to `peers.dat.bak`, start empty (fatal `"Failed to rename invalid peers.dat file. Please move or delete it and try again."` if the rename fails); any other error (bad magic `"Invalid network magic number"`, checksum, truncation, out-of-range counts, a bad `CAddress` version word, a bad BIP155 length) → InitError `"Invalid or corrupt peers.dat (%s). If you believe this is a bug, please report it to %s. As a workaround, you can move the file (%s) out of the way (rename, move, or delete) to have a new one created on the next start."` (`feature_addrman.py` checks it with the bug-report URL from the test config; O11 sets that to rdashd's). **Error-class order** (core m-8): `DeserializeDB` checks the magic first, then parses the **whole** payload — including `Unserialize`'s `CheckAddrman` (→ `DbInconsistentError`) and the compat check (→ `.bak`) — and only then compares the trailing hash (`addrdb.cpp` v24:96-117). So a file with a bad checksum that also fails the consistency check is silently reset in Core, not fatal; a hash-first loader would be wrong. Read-side rules: the format byte is never validated; compat < 32 is fatal; only the high bits of the `CAddress` version word are checked (`DISK_VERSION_IGNORE_MASK` 0x7FFFF); an occupied restore slot falls back to the source bucket; out-of-range bucket indexes are dropped silently; bytes after the checksum are ignored. The asmap checksum field is `SHA256d(CompactSize(nbits) ‖ one byte per bit)` of the decoded `vector<bool>` (`HashWriter{} << m_asmap`, `netgroup.cpp` v24:11-16), not a hash of the file. Restore rule: bucket membership is restored only when the file has 1024 buckets and its asmap checksum equals ours, otherwise every new entry is re-bucketed by its stored source; tried entries whose slot is taken are dropped; entries with no reference are deleted. **`vRandom` after a load is not reproducible** (core M-6): `Unserialize` assigns ids in file order, then its prune loop calls `Delete` (`SwapRandom` with the last element) while iterating `std::unordered_map mapInfo` (`v23:386-395 / v24` the same), so after any load that prunes an entry Core's `vRandom` depends on libstdc++'s iteration order; rdashd's is ascending-id order. `GetAddr` after such a load is a **partial** Fisher–Yates that stops at the sample size (`v23:811-846 / v24:815-850`), so two different permutations under identical draws select different **subsets**, not reorderings of one subset, and later draws never reconverge; the only sound differential after a pruning load is to align `vRandom` with Core's (§6.3, G6). `-checkaddrman` applies after load.

### 4.2 `anchors.dat`

Written only on a clean shutdown when addrman outbound is in use: ≤ 2 addresses of the current block-relay-only connections, `CompactSize(n) ‖ n × CAddress (disk, v2)` in the same envelope (`DumpAnchors`, `CLIENT_VERSION | ADDRV2_FORMAT`). Read once at start and **deleted immediately after reading** (any error yields an empty list; `ReadAnchors`, `fs::remove` even on success). A crash therefore loses the anchors, as in Core.

### 4.3 Import and downgrade

Both files are Core's formats, so IM1a's `peers.dat`/`anchors.dat` import is a copy, and a datadir switched back to dashd reads rdashd's files unchanged. `netfulfilled.dat` is SP1's, `banlist.json` P4's.

---

## 5. Performance

### 5.1 Metrics P2a affects

| Metric (A1 §3.9) | node-bench scenario · metric | P2a's share | Budget | Route to 2× |
|---|---|---|---|---|
| IBD to tip, header part | **`headers-{mainnet,testnet}-replay`** (new node-bench scenario, §5.4) · `header_sync_s`; `ibd-*-seed` after P3 | the per-message path except acceptance (CH1) | ≤ dashd 23.1.8 (mainnet) and 24.0.x (testnet) in the window; PF0 counters ≤ dashd per header **with a Core-class X11 backend (XH1)**; "recorded" until the measurement of §5.4 exists (DEC-51c) | §3.4.9: one X11 per header instead of 2–3, inline; batch handoff to CH1. On `-net` scenarios header sync is round-trip bound (mainnet ≈ 2.3 M headers / 8000 = 290 serial round trips, one `getheaders` at a time, which the fingerprint keeps), so parity is the honest expectation there; the 2× applies to the replay scenario and only per §5.2's backend table |
| Startup | `startup-*` · `startup_s` | `peers.dat`/`anchors.dat` load, ConnMan start | ≤ dashd | decode into pre-sized tables (≤ 65,536 + 16,384 entries) |
| Idle memory | `idle_rss_mib` | AddrMan (≤ 81,920 entries at ~100 B), node table, per-peer state | ≤ dashd | slab + flat arrays (Core's layout) |
| CPU at 125 peers (P4's gate) | counters + window | the loop pass, `send_messages` on idle peers | ≤ dashd's instructions per idle pass | **none: no fast path** (DEC-56). `send_messages` runs every section 1–13 of §3.1.4 on every pass for every peer, exactly as Core's `SendMessages` does; sections 6, 8, 10, 11 and 12 all change state or initiate work without any message queued to the peer (`ConsiderEviction` re-benchmarks before its deadline, `v23:np:5809-5830 / v24:5955-5976`; sync-start eligibility; tx-trickle timers; block-download initiation), so no "queue-empty" predicate is sound. Parity comes from the same loop shape, a futex wake and synchronous `fPauseSend`. A future skip optimisation needs PF0 evidence that the idle pass matters, a twin-compared property test over all state and later sends, and its own review |
| RPC `getpeerinfo` | `rpc-*` · `rpc_getpeerinfo_ms` | stats snapshots | ≤ dashd | per-field atomics for the FT-visible counters, dirty-flag refresh after each `process_messages` and before any send in that pass, so `getpeerinfo` right after `sync_with_ping` sees `addr_processed`/`addr_rate_limited` as dashd does (arch m5) |

### 5.2 The header path, per X11 backend (arch M3; DEC-51b)

Per connecting header, Core does 3 X11 (`headers2`) or 2 (`headers`) plus acceptance; rdashd does 1 plus decode plus acceptance. The outcome depends entirely on the X11 backend, which P2a does not control. Measured user-space instructions per byte for an 80-byte header (memory note `dash-sha256-sh1`; only instruction counts are used for gating):

| Backend | ins/byte | vs Core | `headers2` per header vs Core | `headers` per header vs Core |
|---|---:|---:|---|---|
| Core (dashd) | 2,120 | 1.0× | — | — |
| `rs-x11-hash` | 2,301 | 1.085× | ≈ 2.8× better | ≈ 1.84× better (fails a "÷ 2" gate) |
| base-sdk `dash-pow` @33fcc59 | ~4,950 | ~2.3× | ≈ 1.3× better (fails 2×) | ≈ 0.86×: **slower than dashd** (charter rule) |

Therefore: **XH1 (a Core-class X11 backend, DEC-51b) is a dependency of P2a-core's headers phase and of BS15's backend choice**; A1 §3.9 row 6's "X11 parity" is an erratum (§3.15). The gates are: `headers_decode_hash_2000` instructions per header ≤ Core's (never slower); `headers2_decode_hash_2000` ≤ Core's ÷ 2 only when the X11 ratio allows it; the X11 ratio recorded as its own counter so a header-path regression and a backend regression cannot hide behind each other. P2a calls X11 through `dash-pow`'s entry point and inherits whichever backend XH1/BS15 make the default. The design keeps Core's cadence everywhere a peer can see it (§3.4.9 rule 4).

### 5.3 Other hot paths

| Path | Data structure / technique | Why |
|---|---|---|
| Actor pass | a reused `Vec<Arc<NodeHandle>>` snapshot (no allocation per pass); Fisher–Yates over a ChaCha20 stream | Core copies and ref-counts `m_nodes` every pass |
| Inbox | two `VecDeque<RawMsg>` and two byte counters under one `parking_lot::Mutex`; classification by a perfect hash of the 8 quorum commands; **payloads copied out of the read buffer** (one allocation per message, as Core's `CDataStream`; DEC-51h) | Core's two `std::list`s; no read-buffer pinning outside the flood limit (arch M10) |
| Outbox | per-node **unbounded MPSC** queue to P1's writer (overlay threads push too) plus an atomic **send-memory charge** (`56 + MallocUsage(capacity)` per message, moved to the transport at handoff, plus the transport's own charge) for `fPauseSend`; never blocks | Core's `m_send_memusage` + `GetSendMemoryUsage()` (G3); liveness §2.2.1 |
| AddrMan | slab of `AddrInfo` by id, `hashbrown::HashMap<ServiceKey, i32>` (never iterated), flat bucket arrays, `BTreeSet<i32>` collisions | Core's semantics, contiguous memory |
| Bucket math | SHA-256d through the SH1 backend; keys and groups hashed from stack buffers | no allocation per hash |
| addr relay | `RollingBloom` (5000, 0.001) per peer; relay destinations from a SipHash randomizer | Core's |

### 5.4 Benches, counters and the M2 measurement (PF0; arch M4; DEC-51c)

Nothing in place today can measure the header path against dashd at M2: node-bench's `header_sync_s` is a by-product of `ibd-*` whose `is_done` needs `blocks >= target` and `initialblockdownload == false` (`nodebench/scenarios/ibd.py:100-104`) and polls `getblockchaininfo` (RP1a), so an M2 node never completes it; PF0 has no X11 or header workload and its node workloads are 5.7–47 % noisy; Core's `bench/net_processing.cpp` exists at v24 only; and regtest's `fDefaultConsistencyChecks = true` makes `ProcessNewBlockHeaders` run `CheckBlockIndex()` after every header (`v24:validation.cpp:4737`), an O(n²) index walk. The M2 measurement is therefore defined here and its pieces are tasks:

| Bench / workload | Kind | Core counterpart | Gate |
|---|---|---|---|
| **`headers-{mainnet,testnet}-replay`** (new node-bench scenario) | both binaries sync from §6.6's scripted header server (the oracle's header chain in 8000-header `headers2` and 2000-header `headers` batches); ends at "headers == target" via `getblockheader`/the log; no block download; `-checkblockindex=0` on regtest variants for both sides | dashd 23.1.8 / 24.0.x on the same script | `header_sync_s` ≤ dashd (REL-1.3), **recorded** until the scenario and XH1 exist |
| **PF0 `x11_80b`** and **`header_accept_2000`** (new PF0 workloads: X11 over 80-byte headers; CH1's real acceptance of a 2000-header batch) | perfgate counters, single-threaded, pinned (`RAYON_NUM_THREADS=1`) | a DEC-16 Core X11 bench and header-acceptance bench at **both** pins with a mainnet `TestingSetup` | the X11 ratio as its own counter (§5.2); acceptance instructions per header ≤ Core's |
| `p2p/headers2_decode_hash_2000`, `p2p/headers_decode_hash_2000`, `p2p/headers_sync_replay` (100 captured batches through `run_one_pass` with CH1's real acceptance, or both sides stubbed at the same boundary; instructions and allocations per header) | criterion, single-threaded, hashing inline | a DEC-16 Core bench at **both** pins driving `ProcessMessage(HEADERS[2])` with real `AcceptBlockHeader` (v24 has `bench/net_processing.cpp`; v23 gets the same bench as a local patch) | `headers_*` ≤ Core's instructions per header; `headers2_*` ≤ Core's ÷ 2 only when the X11 ratio allows (§5.2); the actor's own work ≤ 1/10 of acceptance |
| `addrman/add`, `/select`, `/select_almost_empty`, `/select_by_network`, `/getaddr`, `/add_then_good` | criterion, Core's inputs | `bench/addrman.cpp` (both pins) | instructions ≤ Core's (milestone record); no regression per PR |
| `p2p/actor_idle_pass_125`, `p2p/version_handshake` | criterion | none | no regression per PR |
| `header-sync-regtest` | PF0 workload: 200,000 headers from a local dashd seed (both pins) with `-connect` and `-checkblockindex=0` on both sides | dashd syncing from the same seed | instructions ≤ dashd's; wall clock in the window; recorded until the Core-side bench exists |

---

## 6. Verification

P2a has no consensus code, so the layers are L0 (Core's unit vectors), L1 (differential against Core in-process), L2 (replay windows), L4 (functional tests), L6 (the fingerprint probes: O13a now, O13b later), fuzz, `sim`, and live interop. Every test runs under **both** profiles unless it says otherwise: `V24_0` against v24.0.0-rc.3, `V23_1` (regtest `-releaseprofile=23.1`) against v23.1.8.

### 6.1 O13a-out: handshake transcripts per profile (M2 exit 2; O13a-in follows P2b)

- **Harness.** Core's functional framework (`P2PInterface`, `v2_p2p.py`) drives both binaries. Outbound transcripts use `add_outbound_p2p_connection`, which calls the regtest `addconnection` RPC (`rpc/net.cpp` v23:368 / v24:382) with a connection type and v2 flag; the framework listens and records. P2a provides the back end (`ConnMan::open_one_try`, Core's `AddConnection` semantics: per-type limits counting unfinished connections, a try-grant); RP0/RP1a wires the RPC at M2 (§8 Q5). Because regtest opens no feelers or block-relay connections by itself (PQ-12), `addconnection` is the only way to provoke those types, as it is for dashd's own tests.
- **Matrix.** {v1, v2} × {outbound full-relay, block-relay-only, feeler, addr-fetch, manual (`addnode onetry`)} × {peer at 70240, 70242, 70235, 70222 (below `sendaddrv2`), with and without bit 11, with and without bit 12} × {`V23_1` vs v23.1.8, `V24_0` vs v24.0.0-rc.3}, plus devnet (name match and mismatch), plus the two +C probes of §3.12 (the connecting one-header batch and the block inv, measured statistically), plus the P2P-16 variant with ≥ 5 skewed-`nTime` outbound peers (arch m15), plus the connect→`version` latency feature (core M-3). Masternode-flag variants come with AM1 (excluded at M2, as the PLAN says).
- **Normalisation.** Nonces, challenges, ping nonces, v2 garbage and keys, `version.time` (both nodes under the same `-mocktime`); everything else byte for byte, including message order, `version` length and `addr_recv` services.
- **Window.** From the TCP connect to the first `getheaders`/`getheaders2` (the first E-owned send), plus 10 s of silence with the mock clock frozen; phase C's transcript comparison ends at `senddsq` (§7.2), the full window is the joint E/F gate. The sync tick's 6 s gate is on `GetTime()` (`v23:node/sync_manager.cpp:161 / v24:157`), so with mocktime frozen it cannot fire in either binary — **provided mocktime has not moved by ≥ 6 s since the first tick** (`nTimeLastProcess` is a function-static set on the first call, `v24:149`; a `setmocktime`/`bump_mocktime` of ≥ 6 s during fixture setup makes the next tick fire once and dashd sends `getsporks` to every fully connected peer inside the window; core m-7). The harness asserts mocktime has not moved since node start, or applies the `getsporks` mask from the start; `getsporks`, `mempool`, `govsync` and the `full-sync` disconnect enter O13b's window when SP0/SP1 land. The M2 interim deviations of §3.11 are listed in STATUS.
- **State parity.** Both live binaries start from **fresh genesis-only datadirs** (an M2 rdashd connects no blocks and has no loader, so it cannot hold `regtest-40000`); `start_height` is 0 on both and the sync-start decisions match. The `regtest-40000` header fixture stays an in-process fixture for `sim` and O1c sessions (a fake chain), not evidence about a live M2 binary (G7).
- **CI form.** The dashd transcripts are captured once per pin and checked in as fixtures; CI replays them against `sim` on every P2P-touching PR; the live dashd comparison runs nightly.

### 6.2 Handler-level `diff_p2p_*` (A1 §2.14; L1) and the O1c oracle entries

Per-command outcomes include state, so codec equality is not enough. P2a needs a **stateful in-process oracle**. Neither O1 nor O2a has it, and PLAN has no O1b; both designs' proposals are merged into one new task, **O1c** (DEC-48 Q1; **size M**, arch M8), built as a DEC-16 local patch on both pins from Core's own `TestingSetup` + `ConnmanTestMsg` + `PeerManager` harness (`bench/net_processing.cpp` at v24, `test/fuzz/process_message{,s}.cpp` at both pins). Rev1 corrects its scope: **sessions hold many peers** (sync-peer selection, the 4-slot protection, `RelayAddress`'s top-n, `EvictExtraOutboundPeers` and +C are cross-peer logic), **randomness is seeded**, scripts can **capture and substitute** values, and **one session per process** (Core's mocktime, logger callback and `gArgs` are process-global):

```c
// libdash_oracle, both pins
oracle_peer_session* oracle_peer_session_new(int pin, const char* chain, uint64_t our_services, int64_t mocktime,
                                             uint64_t rng_seed, const uint8_t* chain_fixture, size_t len);
int  oracle_peer_session_add_peer(oracle_peer_session*, int conn_type, uint64_t services, int32_t version);  // -> peer id
int  oracle_peer_session_deliver(oracle_peer_session*, int peer, const char* cmd, const uint8_t* payload, size_t len);
     // the payload may contain capture/substitute markers: {ping_nonce_sent:<peer>}, {mnauth_challenge_sent:<peer>}, …,
     // filled from the session's recorded outbound messages (diff_p2p_pong needs "reply to the nonce this side sent")
int  oracle_peer_session_send_pass(oracle_peer_session*, int64_t mocktime);          // one SendMessages pass over every peer
int  oracle_peer_session_outcome(oracle_peer_session*, int peer, uint8_t* out, size_t* len);
     // serialized: messages pushed to that peer (cmd + bytes, in order — this covers Fable's `oracle_version_build`),
     // Misbehaving deltas and messages (log callback), fDisconnect, should_discourage, addrman calls
     // (SetServices/Good/Add with arguments), AddTimeData samples, the RNG draws made while handling it, and a snapshot
     // of the peer fields named in §3.4.1 and §3.6.1 plus the globals (nSyncStarted, protected count)
void oracle_peer_session_free(oracle_peer_session*);
// The DEC-16 patch seeds every FastRandomContext the handlers construct (the `addr` shuffle, v24:np:4429, is OS-seeded and
// not reached by g_mock_deterministic_tests, which only affects GetRand, v24:random.cpp:639-643), GetRand (ping nonces, the
// mnauth challenge) and ConnMan's SipHash seeds (RelayAddress, the address cache, nLocalHostNonce) from rng_seed, and logs each
// draw; the Rust side takes the same draws through its RNG seam, so the streams match by construction. What stays random on
// purpose is normalised per field.

// AddrMan (covers Fable's oracle_addrman_bucket / chance / replay and oracle_netgroup)
oracle_addrman* oracle_addrman_new(int pin, int deterministic, const uint8_t* asmap, size_t asmap_len, int check_ratio);
int  oracle_addrman_op(oracle_addrman*, const uint8_t* op, size_t len, int64_t mocktime, uint8_t* out, size_t* out_len);
     // ops: Add(vector, source, penalty), Good, Attempt, Connected, SetServices, ResolveCollisions,
     //      Select(new_only, network), SelectTriedCollision, GetAddr(max, pct, network, filtered), Size(network, in_new)
int  oracle_addrman_canonical(oracle_addrman*, uint8_t* out, size_t* len);
     // every new and tried cell's occupant key, every entry's fields (nTime, services, source, last try/success/
     // count-attempt, attempts, refcount, fInTried, random position), vRandom, the collision set, network counts, nKey
int  oracle_addrman_bucket(const uint8_t* nkey, const uint8_t* addr, size_t, const uint8_t* src, size_t, const uint8_t* asmap, size_t,
                           uint32_t* tried_bucket, uint32_t* new_bucket, uint32_t* pos_new, uint32_t* pos_tried);
int  oracle_addrman_chance(int64_t last_try_delta_s, int32_t attempts, uint64_t* double_bits);
int  oracle_netgroup(const uint8_t* addr, size_t, const uint8_t* asmap, size_t, uint8_t* out, size_t* len);
int  oracle_locator(const uint8_t* chain_fixture, size_t, uint32_t height, uint8_t* out, size_t* len);   // CH1 may own it
```

v24 has `GetEntries` for the cells; for v23 the shim compiles the same walker against `AddrManImpl`; `oracle_addrman_canonical` exports `vRandom`. The Rust side runs the identical script against `PeerLogic` in `sim` (one session, many scripted peers) with a `ChainAccess` built from the same chain fixture and compares the outcomes.

| Target | Inputs | Seeds |
|---|---|---|
| `diff_p2p_version`, `_verack`, `_sendheaders`, `_sendheaders2`, `_sendcmpct`, `_sendaddrv2`, `_sendtxrcncl`, `_senddsq`, `_qsendrecsigs` | payloads + connection type + delivery before/after `version`/`verack` | BS4a's `decode_domain.json5` (`version` cut at every field, bools 02/ff, subver 256/257, negative time and version, broken tails) |
| `diff_p2p_ping`, `_pong` | payloads, outstanding/none/mismatched nonce (the matching case uses the `{ping_nonce_sent}` substitution), mocktime jumps past 20 min | `p2p_ping.py` |
| `diff_p2p_addr`, `_addrv2`, `_getaddr` | address vectors (v1 and BIP155, every network id, oversized), relay state, bucket state | `p2p_addr_relay.py`, `p2p_addrv2_relay.py` |
| `diff_p2p_inv`, `_getdata`, `_notfound` | inventories of every type incl. 34, sizes around 116, 50,000, 50,001; tx-rejecting connections | per profile |
| `diff_p2p_headers`, `_headers2` | chains extending the fixture, unconnecting, discontinuous, invalid (bad PoW, time-too-new, checkpoint fork), counts at 2000/2001/8000/8001 at common 70234/70235; truncated third header (PQ-2); **both mixed cases of PQ-24** (a bit-11 peer sending 2000 plain `headers` at ≥ 70235; a non-bit-11 peer sending 8000 `headers2`), each with the peer's best known work below `MinimumChainWork` in IBD; **hash-call witnesses** (G1): the oracle's X11 collaborator counts and orders calls, and the Rust side's hash seam does the same, for mismatches at positions 2, middle and last, unconnecting `headers2` with compressed and explicit prev hashes above and at ≤ 8, a truncated compressed message, and a connecting batch | Core captures (`-capturemessages`), dde-probe's headers cases |
| `diff_p2p_multi_handshake` (phase C) | 2–10 peers through `version`/`verack`/gates/queues: duplicate and pre-verack sequences across peers, the quorum-priority interleaving, `fPauseSend` with the send-memory charge (G3 boundary cases: many small `pong`s, one large partly sent payload, V1 and V2), per-peer ordering up to `senddsq` | multi-peer sessions, no E/F behaviour |
| `diff_p2p_multi_headers` (joint E/F gate) | sync-peer selection with 2–10 peers, the 4-slot protection, `EvictExtraOutboundPeers`, the +C probes, the P2P-16 skew scenario, **the genesis-eviction session** (G2: one outbound peer serving nothing, mocktime past `CHAIN_SYNC_TIMEOUT`; compare locator bytes, the version word, the 2-min disconnect under each profile) | multi-peer sessions |
| `diff_p2p_multi_relay` (joint E/F gate) | `RelayAddress`'s top-n over 2–10 peers with the seeded shuffle | multi-peer sessions |
| `diff_p2p_unknown`, `diff_p2p_m2_stubs` | random command strings; every command §3.11 marks exact via a stub or fallback, well-formed and malformed, on full-relay and block-relay-only connections | §3.11 rows |
| `diff_p2p_sequence` | random message sequences (pre-version, pre-verack, duplicate `version`/`verack`, renegotiation attempts, send passes with mocktime steps) | O13b extends it with tip changes across V19 |

Budget: each target ≥ 24 CPU-h clean before P2a merges (A1 §4.13 G-P2P); differences triaged through the O3 runner (DEC-30 N5).

### 6.3 AddrMan parity (`diff_addrman`; PLAN's "bucket-math parity")

- **Differential.** Both sides start in deterministic mode (`nKey = uint256{1}`, the zero-key ChaCha20 stream); a fuzzer generates operation streams over a small address universe (collisions, multiport duplicates, terrible entries and 8-reference entries happen); after every operation the outputs (selected address, returned vectors) and the canonical state must be equal. Because the RNG streams are identical, this checks every draw: `Select`'s probabilities, `GetAddr`'s shuffle and the 2^−refcount rule are compared exactly **for operation streams that start from an empty addrman or from a non-pruning file**. **After a `peers.dat` load that prunes an entry, exact equality cannot hold** (core M-6; §4.1): Core's `vRandom` depends on libstdc++'s `unordered_map` iteration order, and because `GetAddr` is a partial shuffle, different permutations under equal draws select different subsets and never reconverge (G6), so there is no "compare as multisets" option. The differential uses **alignment only**: after such a load the runner takes `oracle_addrman_canonical`'s `vRandom` and entry table, translates ids by address key (`CService`), installs Core's `vRandom` order in the Rust addrman, rewrites every entry's `nRandomPos`, verifies that the two canonical states (buckets, entries, counts, collisions) are then identical and that the RNG streams are at the same position, and continues the stream exactly (draw by draw). For loads that are deliberately left unaligned (the serialization-exchange tests), the comparison is canonical bucket/entry invariants plus a separately defined sampling check (`GetAddr` over many seeds against the analytic uniform-sample distribution), with no claim of equal sample sets or of future draw-by-draw equality. Required case: a pruning file, then capped and network-filtered `GetAddr`, then `Select` and `Add`, aligned. This is the only non-reproducible state. ≥ 24 CPU-h per pin (the logic is pin-identical; the runner records both), plus `-checkaddrman` ratios 1 (draws nothing) and 2–3 (which change the draw stream) and `test/data/asmap.raw`.
- **Bucket math and chance.** `oracle_addrman_bucket`, `oracle_addrman_chance` (the nine doubles bit-exact) and `oracle_netgroup` over the address and asmap corpus.
- **Serialization.** Each side serializes; each side loads the other's file; canonical states equal modulo id renumbering and, for a pruning file, `vRandom` order (the two acknowledged differences; G6). Corrupted files (truncation at every offset, bad magic, checksum, counts, version words, BIP155 lengths, duplicates) produce the same outcome class (fatal / silent reset / `.bak`).
- **L0.** Core's `addrman_tests.cpp` (22 cases at v23.1.8, 23 at v24; `getaddr_unfiltered` is the extra one), transcribed, with their known answers (tried bucket 40, new bucket 786, the asmap cases with `asmap.raw`); `netbase_tests.cpp`; Core's `fuzz/addrman*.cpp` and `fuzz/asmap*.cpp` targets as non-differential fuzzers (there is no `asmap_tests` file; the asmap unit cases are inside `addrman_tests.cpp`).

### 6.4 Other L0 vectors

From both pins: `denialofservice_tests.cpp` (`outbound_slow_chain_eviction`, `stale_tip_peer_management`, `block_relay_only_eviction`, `peer_discouragement`), `net_tests.cpp` (local addresses, `GetLocalAddrForPeer`, `version` construction, `CalculateKeyedNetGroup`), `net_peer_connection_tests.cpp` (`AddConnection` limits), the ChaCha20/`FastRandomContext` vectors (`random_tests`, `crypto_tests`), the fixed-seed table equality (§3.8.6), and the `version` byte fixtures captured by BS4a's probe at both pins (137 bytes; nonce and challenge masked).

### 6.5 Functional tests (L4, through O11's runner, both pins)

| Test | Covers | With |
|---|---|---|
| `p2p_add_connections` | slot types, feeler behaviour (via `addconnection`) | — |
| `feature_anchors` (present at both pins; there is no `feature_anchors_db`) | anchors write/read/delete | — |
| `feature_addrman` | `peers.dat` load outcomes | — |
| `p2p_addr_relay`, `p2p_addrv2_relay` | §3.6 | P2b (inbound halves) |
| `p2p_addrfetch`, `feature_config_args` (`-seednode`; there is no `p2p_seednode`), `p2p_dns_seeds` | ADDR_FETCH, `-seednode`, DNS timing | — |
| `p2p_ping`, `p2p_timeouts`, `p2p_leak` | §3.5, gates, inactivity | P1 (transport), P2b (inbound) |
| `p2p_initial_headers_sync`, `p2p_dos_header_tree`, `p2p_sendheaders_compressed` (headers part) (`p2p_headers_sync_with_minchainwork` is a Bitcoin presync test absent at both pins and is dropped) | §3.4 | CH1, P2b |
| `p2p_invalid_messages` (P2a's cases: redundant `version`, oversized `inv`/`getdata`/`headers`, non-continuous headers) | §3.2, §3.10 | P1 (framing cases) |
| `p2p_blocksonly` (block-relay-only part) | gate 7, §3.4.7 | P5 (tx part) |
| `feature_maxtipage` | §3.4.2 | — |
| outbound eviction: no Core FT exists (`p2p_eviction.py` is inbound-only); covered by the `denialofservice_tests` port and an rdashd-authored `sim` scenario (§3.12 new-11) | §3.9 | — |
| `p2p_connect_to_devnet` | devnet UA and mismatch | — |
| `rpc_net` (P2a's back ends) | `getpeerinfo` fields, `addnode`, `disconnectnode`, `setnetworkactive`, `getnodeaddresses`, `addpeeraddress`, `getaddednodeinfo` | RP1a |

`feature_minchainwork`, `p2p_sendheaders` and `p2p_compactblocks` need block download and join with P3.

### 6.6 L2: replay windows

- **Header-sync replay, mainnet and testnet**: the oracle's header chain (per the `dashd-reference-oracles` recipe) is served by a scripted peer in 8000-header `headers2` batches and in 2000-header `headers` batches; the resulting `getheaders(2)` sequence (locators, timing of the follow-up relative to the batch) must equal a dashd of the same profile fed the same script, and the final `getchaintips`/`chainwork` must equal the oracle's (M2 exit 1; O9a's comparator).
- **Timing windows under mocktime**: 24 simulated hours with 8 scripted outbound peers; feeler, extra-block-relay, stale-tip and addr-broadcast event counts compared with dashd's by KS (NFR-FP-03; O13b owns the full suite, P2a ships the harness glue).

### 6.7 `sim` regression suite (every PR, no dashd) and live

`sim`: golden transcripts per connection type and profile (§6.1's fixtures); the §3.12 "new" behaviours (priority-queue overtaking, feeler `getaddr`, `headers2` timestamp clearing, `addr_recv` services, PQ-24's mixed cases, the synchronous `version` at connect); timer distributions (ping cadence, feeler and extra-peer Poisson means, DNS-seed delays) by KS against the analytic distribution, deterministic because the loop and the ConnMan steps are driven from the virtual clock (§2.2); the §2.2.1 liveness test; stall-free behaviour under 125 simulated peers with backpressure and a stalled writer; the §3.8.2 ladder under a simulated clock with a fake dialer (slot counts never exceed limits; netgroup rule; one feeler at a time; none on regtest; the regtest churn); a `sim` probe for the M10 scenario (a peer that stops reading and trickles 32-byte `ping`s: memory stays within the flood budget); the G3 send-boundary probes (a stalled V1 and V2 writer with many queued `pong`s, and one large payload partly sent) asserting `pause_send` flips at Core's charge, not at the wire-byte count; the g4 `setmocktime`-only and `mockscheduler`-only cases.

Live (M2 exit 1): headers to the tip on mainnet (`V23_1`) and testnet (`V24_0`) from public peers and from the oracle over localhost, v1 and v2; `getchaintips`/`chainwork` equal the oracle's. A 24 h soak with 125-connection limits and the default outbound behaviour, comparing connection-type counts, feeler and extra-peer rates and DNS-seed timing with a dashd of the same pin on the same host.

### 6.8 Fuzz (non-differential, through the O3 runner)

`p2a_process_message` (any message in any state through `PeerLogic`: no panic, no unbounded growth, the global counters of §3.4.1 return to zero after `finalize_node`), `p2a_peers_dat` (no panic on any file; Core's error classes), `p2a_inbox` (the two-queue poller against a model), `addrman_model` (random op sequences vs a `BTreeMap` model; `Check()` never fails; serialise→deserialise→serialise is a fixed point), `connman_sched` (the simulated-clock ladder invariants).

---

## 7. Implementation plan: P2a-core and P2a-net (DEC-51e; arch M9)

P2a is two tasks of size L run in parallel after a shared foundation phase; each phase is one or two PR-sized reviews. **Merge gates are `sim`, oracle (O1c) and fuzz evidence only**; functional tests, O13a-out and the live sync are **M2 exit items**, because they need tasks that land after or beside P2a: O11's runner (ND1, RP0), the RP0 M2 slice, O13a-out, CH1. Shared dependencies: P1's `TransportFactory`/`FrameReader`/`FrameWriter` before phase D's loopback tests (a V1 stub suffices for C); CH1's `ChainAccess`/`ChainCommand::ProcessHeaders`/`TipFacts`/`ActiveChainView` with requirements R1–R7 before phase E merges (PLAN `i:CH1`; a fixture-backed fake before that); BS7's headers2 codec before E (until BS7's divergences are fixed `diff_headers2` is red); **XH1 before E's performance gate** (DEC-51b); O1c's entry points before the differential gates of B and C; `dashd-clock` from phase A.

**PLAN edits this plan needs** (arch M9, g): rows for **O1c** (M, with phase A), **XH1** (Core-class X11, before E's gate), **O13a-out** (after P2a-core E and P2a-net D; O13a-in after P2b), and an **RP0 M2 slice** that wires `getpeerinfo`, `getnetworkinfo`, `getconnectioncount`, `addnode`, `addconnection`, `disconnectnode`, `setnetworkactive`, `ping`, `setmocktime`, `mockscheduler`, `getblockchaininfo`, `getchaintips` and `syncwithvalidationinterfacequeue` (Dash's `bump_mocktime` calls `setmocktime` **and** `mockscheduler`, `v24:test_framework.py:883-900`; `p2p_addr_relay`, `p2p_addrv2_relay` and `p2p_blocksonly` use it; M2 exit 3 already needs `getpeerinfo` and `getblockchaininfo`). P2a provides the back ends (`ConnMan::open_one_try`, `PeerManagerHandle::stats`, `send_pings`, …). The M2 model is re-run with two L tasks.

### 7.1 Shared phase

| Phase | Content | Merge gate |
|---|---|---|
| **A. Foundations** (P2a-core owns; P2a-net consumes) | crate `dashd-p2p` (MIT, lints, `ARCHITECTURE.md`, CI group `node-runtime`); `release.rs` with every `Identity`/`P2pRules` field as complete literals and row-citing doc comments; `fingerprint.rs` (incl. the locator version word); **`dashd-clock`** (owner: this phase); `FastRandom` and `exponential_rand`; `NetAddr` predicates, `netgroup`, the asmap interpreter; `NodeTable`/`NodeHandle` (atomics, inbox with copied payloads, unbounded MPSC outbox), `ConnChainView`; the `NetStream` trait | 137-byte `version` fixtures per profile byte-equal to dashd captures; `FastRandom` first output `0x903df1a0ade0b876` and `random_tests`; netgroup and asmap vectors (`addrman_tests` asmap cases); `check-release-refs.sh` passes; UA fixtures for both profiles, devnet and `-uacomment`, the 256-byte InitError; the `sim` M10 memory probe |

### 7.2 P2a-core (identity, actor, handshake, headers)

| Phase | Content | Merge gate (both profiles) |
|---|---|---|
| **C. Actor and handshake** | the `PeerManager` thread, `MessageLoop::run_one_pass(now)` and the loop (§3.1); the liveness invariants, the send-memory charge and the shutdown sequence (§2.2.1) and the synchronous `initialize_node`; `PeerLogic` with `process_messages`/`send_messages` sections and all seams with M2 defaults (`NoBlockSync` with the four section hooks, `NoObjectRequests`, `NoOrphans`, `EmptyServing`, `DiscourageOnly`); `policy.rs` (74 rows: BS4a's pre-read columns + the re-derived, cited post-read column) and the gates; handlers `version`, `verack`, `send*`, `senddsq`, `qsendrecsigs`, `ping`, `pong`, unknown; the §3.11 stubs over the raw-payload `NetHandler` signature; the misbehaviour framework; the registry with the M2 fallbacks; `PeerManagerHandle` with lock-free stats; `sim` | `sim` transcripts equal the dashd fixtures for every connection type and profile **up to and including `senddsq`** (the first E-owned send, `ping`/`getheaders`, is excluded from C's comparison; fixtures captured once by the O13a-out harness author from dashd, replayed in CI, no live rdashd comparison in C); `diff_p2p_{version,verack,send*,ping,pong,unknown,m2_stubs,sequence,multi_handshake}` ≥ 24 CPU-h (G7: nothing in C's gate depends on E or F); `policy.rs` table tests (74 commands, both sets, BS4a corpus verdicts); `p2a_process_message` and `p2a_inbox` fuzz clean; the §2.2.1 liveness `sim` test; the reader outcome table (§2.5 P1 row) on loopback |
| **E. Headers sync** | `headers`/`headers2` handlers with pre-read effects and the hash-once inline path (§3.4.9); `HeaderBatch`; the bit-11 full-batch rule (PQ-24); sync start and timeout with `time_source`; unconnecting/continuity; `ConsiderEviction`, `EvictExtraOutboundPeers`, stale tip; block announcements (the `cmpctblock` branch behind P3's seam); the `inv` block path with `msg_asset_unlock` and `is_block_requested`; `getdata`/`notfound` size checks; `getpeerinfo` state stats; the PF0 header benches and the node-bench `headers-*-replay` scenario glue | `diff_p2p_{headers,headers2,inv,getdata,notfound}` ≥ 24 CPU-h incl. PQ-24's mixed cases and the G1 hash-call witnesses; the `denialofservice_tests` port in `sim`; the +C probes in `sim`; the P2P-16 skew scenario in `sim`; the full-section `send_messages` twin test (every section runs on every pass, DEC-56); `headers_decode_hash_2000` instructions per header ≤ Core's and the X11 ratio recorded (XH1); the L2 header-sync replay against the fixture server. **M2 exit items, not merge gates:** FT `p2p_initial_headers_sync`, `p2p_dos_header_tree`, `p2p_invalid_messages` (P2a's cases), `p2p_blocksonly` (block-relay part), `feature_maxtipage`; the live mainnet/testnet sync (M2 exit 1, with CH1); `headers-*-replay` ≤ dashd |

### 7.3 P2a-net (AddrMan, ConnMan, addr relay)

| Phase | Content | Merge gate (both profiles) |
|---|---|---|
| **B. AddrMan** | the full port (§3.7), `peers.dat`/`anchors.dat` with §4.1's error-class order and read-side rules, `-checkaddrman`; benches | `addrman_tests` transcribed and green; `diff_addrman` ≥ 24 CPU-h clean per pin (ratios 1 and 2–3, asmap; pruning loads compared modulo `vRandom` order, §6.3); serialization exchange both ways; `feature_addrman`-equivalent unit cases incl. the bad-checksum-plus-inconsistent file; `addrman_model`/`p2a_peers_dat` fuzz smoke; PF0 addrman benches recorded against Core's; the negative-`nAttempts` oracle cases |
| **D. ConnMan** | OS threads `opencon`, `addcon`, `dnsseed` as step functions + drivers, the `socket` task; `OpenNetworkConnection` (synchronous `initialize_node`)/`ConnectNode` (TCP, clearnet); anchors; feelers; extra peers; fixed seeds (generated table); `InitBinds` with `bind_failure_fatal`; inactivity and linger; P1 integration (reader classification and payload copies, writer coalescing, v1 retry with `m_dest`); `open_one_try` for `addconnection`/`addnode onetry`; `setnetworkactive`; `NodeStats`; the shutdown order with ND1 | `connman_sched` fuzz; the `sim` ladder scenarios (slot limits, netgroup rule, one feeler, regtest churn, none on regtest) under the virtual clock; loopback: `-connect`, `-addnode`, `-seednode`, feelers via `open_one_try`, anchors round trip; the fixed-seed table test; the connect→`version` latency test. **M2 exit items:** FT `p2p_add_connections`, `feature_anchors`, `p2p_dns_seeds`, `feature_config_args`, `p2p_addrfetch`, `p2p_timeouts`, `p2p_leak` (outbound parts); O13a-out transcripts v1 and v2 against both dashd pins diff-empty |
| **F. Address relay** | `addr`/`addrv2`/`getaddr`, the token bucket, `RelayAddress`, `PushAddress`, `MaybeSendAddr`, the address-response cache, local addresses and `-externalip` | `diff_p2p_{addr,addrv2,getaddr}` ≥ 24 CPU-h with the seeded shuffle; rate-limited counts equal dashd's on the same script in `sim`. **M2 exit items:** FT `p2p_addr_relay`, `p2p_addrv2_relay` (outbound halves; inbound with P2b) |

### 7.3a Joint E/F gate (after P2a-core E and P2a-net F)

`diff_p2p_multi_headers` and `diff_p2p_multi_relay` ≥ 24 CPU-h each (sync-peer selection, protection, eviction, +C, the genesis-eviction session per profile, `RelayAddress`); the full §6.1 `sim` transcript through the first `getheaders` and the 10 s silence; the combined acceptance of both halves is this gate plus §7.4. **PLAN edits are pending manager work**, not completed dispositions: the PLAN at this revision still has one P2a row and no O1c/XH1/P2a-core/P2a-net/O13a-out rows; the split is scheduled only after those rows and the critical-path re-run exist, with the cross-half dependencies (A → B, C; C → E; B → D, F; E + F → §7.3a) written into them.

### 7.4 M2 exit evidence (shared; after both halves, with O13a-out, RP0's slice, O11, CH1, XH1)

O13a-out's full matrix; the functional tests listed as exit items above; the live mainnet (`V23_1`) and testnet (`V24_0`) header sync from public peers and the oracles (M2 exit 1); the `headers-{mainnet,testnet}-replay` window run and the PF0 X11/header counters recorded in STATUS; the 24 h soak; quirk tests PQ-1…25; `ARCHITECTURE.md` (Core file map, the §3.15 deviations); G-P2P reviews (two reviewers + `code-review`) per phase.

Every phase keeps `cargo fmt`, `clippy -D warnings`, the workspace tests and the `node-runtime` CI group green, and ships its PF0 benches with the code they measure.

---

## 8. Open questions for the manager (deduplicated; each with a recommendation)

| # | Question | Recommendation |
|---|---|---|
| Q1 | Handler-level `diff_p2p_*` and `diff_addrman` need stateful oracle entries (§6.2) that O1/O2a do not have; PLAN has no O1b. | **Decided (DEC-48 Q1, DEC-51e)**: O1c gets a PLAN row; rev1 sizes it **M** with the §6.2 scope (multi-peer sessions, the seeded-RNG patch, capture/substitute, one session per process). |
| Q2 | `peers.dat`/`anchors.dat` format: Core's or our own (DEC-11, FR-P2P-19)? | **Core's `V4_MULTIPORT`** (§4.1): both designs agree; interchangeable files, IM1a has nothing to do, the parity test exchanges files. |
| Q3 | A1 puts `NodeClock` and the time-offset filter in `dashd-node`; CH1 and P2a need them below it. | **Decided (DEC-48 Q3)**; rev1 names the owner: P2a-core phase A (§2.7); ND1's PLAN row drops `NodeClock`/mocktime. New sub-question: the outbound ladder cannot be diffed in the regtest lab (PQ-12, arch m14). *Recommend* recording it as O13b residual risk, covered by `sim` and the soak, rather than a DEC-16 hidden regtest switch on both binaries (which would be a third behaviour to keep in step); revisit if O13b's soak finds a ladder divergence. |
| Q4 | Quirk register numbers for PQ-1…23 (§3.14). | Register them as a P2P block after the consensus quirks once this reconcile fixes the list; EV2/LQ1 designs in flight claim numbers first. |
| Q5 | O13a and several FTs need the regtest-only `addconnection` RPC and `addnode … onetry` at M2. | P2a provides the back ends (phase D); RP0/RP1a wires the RPCs in M2. Confirm the owner. |
| Q6 | v24's ASMap health check (`-asmap` only; a log line asserted by v24's `feature_asmap.py`, plus `GetAddr` side effects) has no O17 row. | Add an O17 row keyed `profile` with `P2pRules.asmap_health_check: bool` (`V23_1` false, `V24_0` true), implemented by P8 with `-asmap`. Not P2a's. |
| Q7 | The exact `-honestua` user agent (DEC-04). | `/rdashd:<crate version>[(<comments>)]/`, comments kept, documented in the release notes, never the default, logged at startup. |
| Q8 | Options whose owners land later (`-proxy`, `-onion`, `-i2psam`, `-onlynet`, `-discover`, `-upnp`, `-natpmp`, `-whitebind`, `-whitelist`, `-maxuploadtarget`, `-capturemessages`, `-asmap`). | Confirm A1 §2.18's rule applies as written: InitError "not supported by rdashd yet", never silently ignored (no new decision needed). |
| Q9 | `relay=true` at M2 although the node has no mempool. | Keep it: Core's flag is `!RejectIncomingTxs` and nothing ties it to mempool state (`v24:1582`); a `relay=0` node would be a visible deviation and would also stop `senddsq`. `NoObjectRequests` ignores announcements until P5. |
| Q10 | The speculative header-hash job (§3.4.9): on by default, or off until PF1? | **Reversed by DEC-51a**: no speculative or side-job hashing; hash once, inline, never earlier than Core. Rev1 implements that (§3.4.9). |
| Q11 | CS0's `Release::V23_1_8` vs O17's `Release::V23_1`. | Editorial alignment to O17's name in the CS0 implementer's next PR. |
| Q12 | The reachable null locator (G2) differs per pin (one genesis hash at v23, an empty locator at v24) and O17 rule 2 forbids `release ==` at call sites. | Add an O17 `profile` row and a field **`P2pRules.null_locator: NullLocator { ActiveTip, Empty }`** (`V23_1` = `ActiveTip`, `V24_0` = `Empty`; evidence `v23:chain.cpp:37-38 / v24:chain.cpp:35-36`, reached from `v23:np:5846 / v24:5992`); P2a reads it in `ChainAccess::locator(None)`. |

---

## 9. Reconciliation log

One line per disagreement: Fable's position, Opus's position, the choice, and the Core evidence.

| # | Disagreement | Fable | Opus | Choice | Core evidence (file:line) |
|---|---|---|---|---|---|
| 1 | Header pre-hashing in the reader | not in P2a; revisit in P3 with PF1 data; keep the reader transport-only | hash once in the reader, hand CH1 a `HeaderBatch`; the route to 2× | r0 chose Opus's side job with an ordering rule; **rev1 (DEC-51a) reverses to Fable's position, strengthened**: hash once, inline, never earlier than Core hashes (the reviews showed the job hashed where Core does not, a free CPU/latency asymmetry, and bought nothing in IBD). The `HeaderBatch` contract stays (§3.4.9) | `MarkReceivedMsgsForProcessing` `v23:net.cpp:4732 / v24:4773` (queue at completion); `Uncompress` hashes `block.cpp:122`; `CheckHeadersAreContinuous` `v23:np:3183 / v24:3167`; v24 passes `header.GetHash()` into `AcceptBlockHeader` `v24:validation.cpp:4736`, `ASSERT_IF_DEBUG` `:4602`; `MaybeSendGetHeaders` after acceptance `v24:np:3374`; NFR-FP-03 allows lower latency; A1 §3.1 puts "X11 over header batches" on `prevalidate` |
| 2 | `getsporks` stub at M2 | a ~40-line stub reproducing the tick timing | no stub; mask until SP0/SP1 | **No stub (Opus)** | the only sender is `SyncManager::ProcessTick` `v23:node/sync_manager.cpp:210-214 / v24:206-213`; its 6 s gate is on mockable `GetTime()` `v23:161 / v24:157`, so O13a's frozen-mocktime window excludes it in both binaries; the same tick's next actions (`mempool` `v24:231-244`, `full-sync` disconnect `v24:200-204`, `govsync`) cannot be stubbed; dashd stores and relays `spork` replies |
| 3 | AddrMan and BanMan | `AddrMan(Arc<Mutex<AddrManImpl>>)` (§3.4), `BanView` sync trait | `Mutex<AddrManImpl>`, `BanList` sync trait | **Mutex (both agree)**; A1 §2.14/§3.1 "small actors" is an erratum | `addrman_impl.h:155` `mutable Mutex cs`; synchronous callers `v24:net.cpp:3155-3291`, `v24:np:4149, 4474` |
| 4 | The CH1 contract | `HeadersResult { prev_known, last_was_new, outcome, best_header, tip, tip_chain_work, minimum_chain_work, ibd, loading_blocks }` computed by CH1 in one reply | `ChainAccess` trait + `process_headers` → `HeadersReply { last_accepted, invalid, tip: TipFacts }`, unthrottled `TipFacts` | **Opus's shape with A1's names**: `ChainCommand::ProcessHeaders` → `HeadersResult { last_accepted, invalid, tip: TipFacts }`; the two lookups on the actor via `ChainAccess::lookup`; `TipFacts` unthrottled and published before the reply; rev1 restates it as requirements R1–R7 on CH1 (concurrent index, immutable `prev`/`skip`, active-chain view, lazy IBD, no P2a writes, ABC interleaving; §2.5.3, DEC-51d) | Core's four separate `cs_main` scopes `v23:3353, 3375, 3378, 3283 / v24:3337, 3359, 3362, 3267`; `*ppindex` set only after `accepted` `v24:validation.cpp:4742`; A1 §2.10 line 978 names `HeadersResult` |
| 5 | Clock placement | `clock.rs` inside `dashd-p2p` | new `dashd-clock` crate | **`dashd-clock` (Opus)**: CH1 and P2a both need it below `dashd-node` | `util/time.h:19` `NodeClock`; `timedata.cpp:29-118` fed by `v24:np:4167`, read by `v23:validation.cpp:4144` |
| 6 | Oracle entries | additions to "O1b/O2a": `oracle_addrman_bucket/chance/replay`, `oracle_netgroup`, `oracle_version_build`, an outcome-class extension of `oracle_p2p_decode` | new task O1c: `oracle_peer_session_*`, `oracle_addrman_new/op/canonical` | **One new task O1c (Opus's name; O1b does not exist in PLAN) with the union** (§6.2): the session outcome subsumes `oracle_version_build` and the outcome-class extension; Fable's bucket/chance/netgroup entries kept as cheap pure entries | `bench/net_processing.cpp`, `test/fuzz/process_message{,s}.cpp` at both pins; v24 `AddrMan::GetEntries` |
| 7 | Quirks | Q-P2a-1…10 | PQ-1…17 | **23 deduplicated entries** (§3.14): 5 pairs collapsed (Q-1=PQ-5, Q-2=PQ-2, Q-3=PQ-1, Q-4=PQ-14, Q-7⊂PQ-6); Fable's Q-5, Q-6, Q-8, Q-9, Q-10 added as PQ-18…22; PQ-23 new | per row |
| 8 | Errata | 5 items (§9 Q7, §10) | 13 items (§3.15) | **merged 21 rows** (§3.15), incl. the quorum queue, `fMoreWork`/throttle, `getaddr` on feelers/addr-fetch, `qwatch` position, PQ-12 for A1 #25, the P2P-04/-16 readers, O1b, CS0's variant name | per row |
| 9a | Unsupported options (Opus Q12) | — | InitError | **InitError**: already A1 §2.18's rule; no new decision | A1 §2.18 |
| 9b | `-honestua` (Opus Q10) | `/rdashd:<cargo version>/` with the same comment rules | `/rdashd:<version>[(<comments>)]/` | **same thing; adopted** | `clientversion.cpp:69-84` `FormatSubVersion` shape |
| 9c | ASMap health-check row (Opus Q9) | — | a `P2pRules` flag, implemented by P8 | **adopt as §8 Q6** | `v24:net.cpp:4115-4116, 5009-5019`; absent at v23; O17 #7121 |
| 9d | `relay=true` at M2 (Fable Q2) | keep | (implicit) keep | **keep** | `tx_relay = !RejectIncomingTxs(pnode)` `v24:np:1582` |
| 10 | Security note | flagged privately | — | not searched for, not discussed (manager's instruction) | — |
| 11 | `headerssync.cpp` / Bitcoin pre-sync | absent at both pins | (not stated) | **confirmed absent**; nothing in this design ports presync/redownload | no `src/headerssync.*` in either tree; no `HeadersSyncState`/`presync` in `net_processing.{cpp,h}` at either pin |
| 12 | Outbound accounting on regtest/devnet | only the netgroup rule is skipped | the whole counting block is skipped (no feelers/block-relay) | **Opus** (PQ-12); affects O13a's matrix | `if (!Params().AllowMultipleAddressesFromGroup())` wraps the whole count loop `v23:net.cpp:3034 / v24:3028`; `chainparams.cpp:294,497,689,895` |
| 13 | `ConnectNode` name resolution | the first resolved address is dialled | every resolved address is checked, the last is dialled | **Opus** | `v24:net.cpp:404-423` |
| 14 | Fixed-seed ages | U[now − 7 d, now) | U(now − 14 d, now − 7 d] | **Opus** (U[1 w, 2 w] ago, both pins) | `rand_uniform_delay(Now() - one_week, -one_week)` `v23:net.cpp:220 / v24:221` |
| 15 | devnet-mismatch message | `"wrong-devnet-name"` at both pins | v24 only | **Opus**; log text only | `v23:np:4008,4010` (no message) / `v24:4037,4039` |
| 16 | `peers.dat` load outcomes | load failures → `.bak` | silent reset / `.bak` / InitError by error class | **Opus** | `addrdb.cpp` `LoadAddrman` v23:185-224 / v24:194-231 |
| 17 | Post-read outcomes in the policy table | from BS4a §4 | from BS4a §4 (with its own corrections) | **re-derived from Core per the manager's decision**; BS4a §4 pre-read columns normative, "After the read" informative (§3.2.2, §3.11) | BS4a-r2 N-1 rows (a)–(n), N-4 |

---

## 10. Dispositions of the r1 reviews (one line per finding)

Reviews: `reviews/P2a-design-r1-opus-core.md` (core) and `reviews/P2a-design-r1-opus-arch.md` (arch); rulings in DEC-51. "Fixed" names the section that changed.

### 10.1 Core review

| Finding | Disposition |
|---|---|
| M-1 full-batch limit by the peer's bit 11 | **Fixed** §3.4.3 steps 3, 10–11 (cites `v23:3386-3396 / v24:3370-3380` vs `:5247 / :5324`); PQ-24; `diff_p2p_headers*` mixed cases; §3.12 new-12 |
| M-2 eager pre-hash where Core is lazy | **Fixed** per DEC-51a: no side job; hash once inline, never earlier than Core; rev2 (G1) adds the incremental continuity loop and corrects the zero-hash wording (`Uncompress`'s hashes precede the connect check; ≤ 8 unconnecting batches hash their last header) |
| M-3 `initialize_node` synchronous | **Fixed** §2.2 (synchronous on the caller's thread, `v24:net.cpp:3648` → `np:1745-1760`), §2.4 signature note, §3.8.5, §3.12 row 3 (connect→`version` latency feature) |
| M-4 +C probe vacuous at M2 | **Fixed** §3.12 +C: the connecting one-header `headers` probe and the block-inv probe, statistical under the shuffle; §6.1 |
| M-5 uncited §3.11 rows | **Mostly resolved** (per r2 g1): every row cites both pins for the M2-reachable outcome; rev2 fixed the `platformban`, `dsq`/CoinJoin and `getdata` anchors; `OwnerDerives` rows remain cited for the M2 outcome only and say so |
| M-6 `diff_addrman` exactness after a pruning load | **Fixed** §4.1 (`vRandom` after load is library-order dependent, `v23:386-395`), §6.3: rev2 (G6) keeps only the alignment option (install Core's `vRandom`, rewrite `nRandomPos`, verify the RNG position) and withdraws the multiset alternative; PQ-19 |
| m-1 `cmpctblock` in IBD | **Fixed** §3.11 (`v23:4994-4997 / v24:5071-5074`: silent in IBD) |
| m-2 §3.10.2 gaps; filter row | **Fixed** §3.10.2 (+ `qgetdata`, `qdata`, `mnlistdiff`/`qrinfo` scores; `mempool`, `getcf*`, all three filter commands in the disconnect list), §3.11 filter row |
| m-3 `qfcommit` at v23 | **Fixed** §3.11 (`blockprocessor.cpp:77-89, 111-116, 126`) |
| m-4 version step 6 cite | **Fixed** §3.3.5 (`v23:3971-3983 / v24:4000-4012`) |
| m-5 locator version word per profile | **Fixed** §3.3.1 (`primitives/block.h` v23:249-255 / v24:255-261), `fingerprint.rs` contract, O13a fixture note |
| m-6 non-existent FTs | **Fixed** §6.5, §3.12 rows 25 and new-11, §7: `p2p_seednode` → `feature_config_args`; `p2p_outbound_eviction` → `denialofservice_tests` + a `sim` scenario; `feature_anchors_db` → `feature_anchors`; `p2p_headers_sync_with_minchainwork` dropped |
| m-7 frozen-mocktime caveat | **Fixed** §6.1 (`nTimeLastProcess` function-static, `v24:149`; assert mocktime unmoved since start, or mask from the start) |
| m-8 `peers.dat` error-class order and read-side rules | **Fixed** §4.1 (magic → whole payload incl. `CheckAddrman` and compat → trailing hash, `addrdb.cpp` v24:96-117; format byte, compat < 32, `DISK_VERSION_IGNORE_MASK`, restore fallback, out-of-range indexes; the asmap checksum definition, `netgroup.cpp` v24:11-16) |
| m-9 negative `nAttempts` | **Fixed** §3.7.2 (bit-exact `pow` for n < 0, oracle cases), PQ-25 |
| m-10 AddrMan text errors | **Fixed**: §3.0/PQ-19 (`ASMapHealthCheck` calls `GetAddr(filtered=false)`; dumps iterate `mapInfo`), §6.3 (ratios 1 and 2–3), §3.7.2 (codes −1…−21), §4.1 (`peers.XXXX`), §3.8.5 (first invalid resolved address aborts; the v1 reconnect carries `m_dest`) |
| m-11 feeler step ambiguity | **Fixed** §3.8.2 step 9(c) (`v24:net.cpp:3178-3200`) |
| m-12 regtest churn | **Fixed** §3.8.2, §3.12 row 25, PQ-12 (`GetExtraFullOutboundCount` still counts the extras; one evicted every 45 s after 30 s) |
| n-1 fixed-seed range half-open | **Fixed** §3.8.6, §3.12 row 25: U(now − 2 w, now − 1 w] |
| n-2 timeout scaling order | **Fixed** §3.4.2 pseudo-code (multiply before the integer division) |
| n-3 `StopNodes` dump order | **Fixed** §3.8.1 (`peers.dat` before `anchors.dat`, `v24:net.cpp:4190-4199`) |
| n-4 new-6 qualification | **Fixed** §3.12 new-6 |
| n-5 `count_failures` with `-maxconnections ≤ 1` | **Fixed** §3.8.2 step 10, PQ-12 |
| n-6 `"peer=%d: %s"` on every non-punishing exit | **Fixed** §3.10.3 (`v24:2013-2015`) |
| n-7 `spork` +100 message pin difference | **Fixed** §3.11 `spork` row (log-only) |
| n-8 CoinJoin wallet manager presence | **Fixed** §3.2.6 row a (`v24:init.cpp:2184-2191`) |

### 10.2 Architecture review

| Finding | Disposition |
|---|---|
| M1 wait-for graph / deadlock cycles | **Fixed** §2.2.1: the edge table, invariants (i)–(v), unbounded non-blocking handle queue with a coalescing tip cell, unbounded MPSC outbox (`push_message` never blocks), dropped reply = interrupted, ND1's shutdown order (actor interrupted before V stops), lock hierarchy, the liveness `sim` test; §5.3 Outbox row; errata row for A1 §2.14's "bounded send queue" |
| M2 speculative job unbounded, pre-verack, no benefit | **Fixed** per DEC-51a: removed entirely (§3.4.9 rule 3 records why); the `-debug` kill switch is gone with it |
| M3 2× depends on the X11 backend | **Fixed** §5.2 (per-backend table), XH1 as a dependency of the headers phase (§7), gates `headers_* ≤ Core`, `headers2_* ≤ Core ÷ 2` only if the ratio allows, the X11 ratio as its own counter; errata row for A1 §3.9 row 6 |
| M4 nothing can measure the header path at M2 | **Fixed** §5.4: the `headers-{mainnet,testnet}-replay` node-bench scenario (no block download, `-checkblockindex=0`), PF0 `x11_80b` and `header_accept_2000` workloads, the DEC-16 Core header bench at both pins with real acceptance, single-threaded PF0 header benches, "recorded" until these exist |
| M5 CH1 contract gaps | **Fixed** §2.5.3 as requirements R1–R7 on CH1 (concurrent append-only index; immutable `prev`/`skip` for lock-free locators; unthrottled `ActiveChainView` with `TipFacts`; lazy latched IBD; `best_header` never null and never written by P2a; V checks its queue between ABC steps; the `HeaderBatch` payload); §2.4 `ChainAccess`; §3.4.2 assertion; two errata rows. To be sent to the CH1 brief now (DEC-51d) |
| M6 `NetHandler` takes a decoded message | **Fixed** §2.4: raw payload + `HandlerCx::read::<T>()` at Core's read point (`v24:llmq/net_quorum.cpp:79-92`), optional `MessageProcessingResult` for v23-style handlers; P2a's stubs use it; errata row 5 amended |
| M7 +C probe | **Fixed** as core M-4 (§3.12 +C, §6.1); A1 §4.10 +C errata row |
| M8 O1c scope | **Fixed** §6.2: `add_peer`, per-peer deliver/outcome, the seeded-RNG DEC-16 patch (the OS-seeded `addr` shuffle `v24:np:4429`, `GetRand`, SipHash seeds; draws logged), capture/substitute markers (`{ping_nonce_sent}`), one session per process, size M; `diff_p2p_multi` |
| M9 P2a is XL; acceptance depends on later tasks | **Fixed** §1.1a and §7: P2a-core (A, C, E) and P2a-net (B, D, F) in parallel; merge gates on `sim`/oracle/fuzz only; FTs, O13a-out and the live sync moved to the M2 exit list; PLAN rows for O1c, XH1, O13a-out/-in, the RP0 M2 slice; errata row (g) |
| M10 zero-copy payloads pin read buffers | **Fixed** per DEC-51h: payloads copied like Core (§2.2, §2.4 `RawMsg`, §5.3 Inbox row); the `sim` memory probe (§6.7, phase A) |
| m1 `BlockSync` incomplete | **Fixed** §2.4: `is_block_requested`, `stall_section`, `getdata_section`, `block_checked`; §3.4.7 |
| m2 `TcpStream` in the factory | **Fixed** §2.4: `Box<dyn NetStream>` |
| m3 `RawMsg.speculative` on P1's type | **Fixed**: the field is gone with the job |
| m4 actor timers and `mockscheduler` | **Fixed** §2.7, §3.1.1, §3.1.6: timers on ND1's scheduler time base |
| m5 stats freshness | **Fixed** §2.4 `stats`, §5.1 `getpeerinfo` row: per-field atomics for FT-visible counters, dirty-flag refresh after each `process_messages` |
| m6 ConnMan blocking on tokio | **Fixed** §2.2: `opencon`/`addcon`/`dnsseed` on OS threads as Core; `DumpAddresses` serialises under the lock and writes outside; errata row for A1 §3.1 |
| m7 determinism in `sim` | **Fixed** §2.2, §3.1.1: sans-io step functions (`run_one_pass(now)`, `opencon_step`, …) with thin thread drivers |
| m8 six non-existent tests | **Fixed** as core m-6 plus `asmap_tests` → the asmap cases of `addrman_tests.cpp` and `fuzz/asmap*.cpp` (§3.7.3, §6.3) |
| m9 steps 10–11 limit | **Fixed** as core M-1 |
| m10 NFR-FP-03 conditional; errata row 21 | **Fixed** §3.4.9, §3.12 new-13, §3.15 row 21: the header-batch → `getheaders` gap and the per-format ratio are O13b classifier features (PRD R-8) |
| m11 `dashd-clock` owner | **Fixed** §2.7, §7.1: P2a-core phase A; ND1's PLAN row edited |
| m12 `BanList` non-blocking | **Fixed** §2.4 contract comment |
| m13 ConnMan's chain inputs | **Fixed** §2.4 `ConnChainView` (tip MN list, verified-ProTx set, IBD service cache) |
| m14 the ladder cannot be diffed in the lab | **Recorded** as O13b residual risk with `sim` and the soak as cover (§3.12 row 25, §8 Q3); the DEC-16 regtest switch is left as the manager's option |
| m15 no P2P-16 end-to-end probe | **Fixed** §3.12 row 7, §6.1: a `sim` and O13a-out variant with ≥ 5 skewed-`nTime` outbound peers |
| m16 fast-path predicate | **Superseded** by DEC-56 (rev3): the fast path is removed; every section runs on every pass (§5.1) |
| n1 which backend the 383 ns/byte figure is | **Fixed** §5.2: B1's wall-time figure is not used for gating; the instruction counts per backend are |
| n2 kill-switch option name | **Moot**: the switch is gone with the job |
| n3 "criterion over `sim`" for the header bench | **Fixed** §5.4: single-threaded, hashing inline |
| Errata rulings 1–20 | **Accepted** as ruled; amendments 5 (raw-payload trait), 8 (A1 §3.1 `net` row), 9 (M5's additions), 19 (O1c row, size M, M8 scope) applied |
| Errata row 21 | **Changed** to the reviewer's position (DEC-51j) |
| Missing errata rows (a)–(g) | **Added** to §3.15: `HeaderBatch` payload; locators/concurrent index; the unbounded send queue; A1 §3.1 `net` row; +C's M2 form; A1 §3.9 row 6 X11 parity; PLAN's O13a dependency, RPC owners and the P2a size |

### 10.3 GPT r2 review

| Finding | Disposition |
|---|---|
| G1 prefix short-circuit; `Uncompress` exception | **Fixed** §3.4.3 steps 6–7 (incremental compare-then-hash loop, `CheckHeadersAreContinuous` `v23:3183-3193 / v24:3167-3177`), §3.4.9 rules 1–2 (the `Uncompress` hashes precede the connect check for every `headers2`, `v23:5259-5267 / v24:5335-5345`, `block.cpp:120-123`; ≤ 8 unconnecting batches hash their last header), §0 item 4, R7 (the batch is complete only after continuity), hash-call-count/order witnesses in `diff_p2p_headers*` and phase E's gate; §10.1 M-2 wording corrected |
| G2 reachable null locator per profile | **Fixed** §3.0 #10, PQ-23 (reachable), §3.4.4 (the `ConsiderEviction` path, `v23:np:5826, 5846 / v24:5974, 5992`; v23 active-tip locator vs v24 empty), §3.9.1, `ChainAccess::locator(Option<BlockId>)`, new `P2pRules.null_locator` with an O17 row (§8 Q12, §3.15), §3.12 new-14, the genesis-eviction differential session (§6.2) and an O13a-out variant |
| G3 send backpressure is retained memory | **Fixed** §2.2, §2.2.1 (the charge formula from `CSerializedNetMsg::GetMemoryUsage` `v23:135-141 / v24:136-142` and `memusage.h:51-58`; handoff `v23:1712-1720 / v24:1702-1709`; recompute after every write `v23:1771 / v24:1765`), §2.4 (`push_message`, `FrameWriter::send_memory_usage`), §3.8.7, §5.3, §3.12 new-15; boundary tests in §6.2 `multi_handshake` and §6.7 |
| G4 coherent read snapshot | **Fixed** §2.4 (`ChainAccess::snapshot() -> Arc<ChainReadSnapshot>`; `tip()`/`active_chain()` removed; `HeadersResult.snap`), §2.5 row, §2.5.3 **R8** with Core's single-scope evidence (`v23:np:4731-4809 / v24:4784-4859`; `v23:6193, 6277-6282 / v24:6338, 6422-6428`), R3/R4 restated over the snapshot, CH1's reorg-between-loads test |
| G5 fast path skips eviction transitions | rev2's narrowed predicate was **still open at the r3 closure check** (it suppressed block-download initiation and tx-trickle state updates on "empty" passes). **Fixed in rev3 per DEC-56**: the fast path is deleted from §5.1 and every reference; every `SendMessages` section runs on every pass, as Core; a future skip optimisation needs PF0 evidence, a twin-compared property test, and its own reviewed change |
| G6 multiset comparison unsound | **Fixed** §4.1, §6.3: alignment only (translate ids by address key, install Core's `vRandom`, rewrite `nRandomPos`, verify state and RNG position), unaligned loads compare canonical invariants plus a separate sampling check, the serialization-exchange rule permits renumbering and post-load order, the pruning-then-`GetAddr`-then-`Select`/`Add` case |
| G7 phase C gates reach into E/F | **Fixed** §6.1 (C's transcript ends at `senddsq`; fresh genesis-only datadirs for live M2 parity; `regtest-40000` is a `sim`/O1c fixture only), §6.2 (`diff_p2p_multi` split into `multi_handshake` for C and `multi_headers`/`multi_relay` for the joint E/F gate), §7.2 C's gate, new §7.3a joint gate; the PLAN rows are recorded as pending manager work with the cross-half dependencies |
| g1 anchors | **Fixed** §3.11: `platformban` guard `v23:3814-3824 / v24:3839-3849`; `dsq`/CoinJoin per-pin lines (`coinjoin/server.cpp` v23:50-52 / v24:52-54; `client.cpp` v23:169-172, 205 / v24:63-66, 97-100); `getdata`'s `ProcessGetData`/`ProcessGetBlockData`/`FindTxForGetData` ranges; §10.1 M-5 marked mostly resolved |
| g2 `status` and BLS context | **Fixed** §2.4: `entry().status` is a seqlocked read never cached by P2a; `bls` is a field of `ChainReadSnapshot` and the source of every `ProtoCtx.bls` (§3.2.5) |
| g3 peer-map lock; join and terminal cleanup | **Fixed** §2.2.1: the peer-map mutex in the hierarchy (short; `GetPeerRef` `v24:np:1858`), the actor never holds it while waiting on V; the shutdown sequence names interrupt → cancel the reply wait → **join** → stop ConnMan/readers → `StopNodes` dumps, deletes and runs terminal `finalize_node` synchronously (Core `v23:net.cpp:4155-4195 / v24:4187-4210`) → stop V → stop the dispatcher; `sim` tests for shutdown while V is blocked and with queued finalizations |
| g4 scheduler time overbroad | **Fixed** §2.7, §3.1.1: scheduler time only for scheduled callbacks; `addr`/local-address, ping, sync and feeler/extra-peer deadlines stay on node time (`MaybeSendAddr` `v24:6209, 6215`); `setmocktime`-only and `mockscheduler`-only cases |
| audit n-4 (§3.6.4) | **Fixed** §3.6.4 carries the new-6 qualification |
| audit m13 (verified-ProTx set) | **Fixed** §2.4: collected from `NodeTable`'s per-node fields, not `ConnChainView` |
| audit n1 (383 ns/byte) | **Fixed** §5.2: the quotation is removed; only instruction counts gate |
| audit: PLAN/A1/O17/brief edits | **Recorded** as pending manager work (§7.3a, §3.15); no completed-disposition claim is made for them |
