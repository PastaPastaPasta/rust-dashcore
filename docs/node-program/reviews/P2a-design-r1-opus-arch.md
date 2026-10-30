# P2a design review r1: architecture, interfaces, performance, testability, plan (Opus, reviewer 2 of 2, DEC-42)

| | |
|---|---|
| Design | `designs/P2a-outbound.md` reconciled r0 (2026-10-08), with DEC-45 and DEC-48 taken as decided |
| Focus | concurrency and interfaces (actor, inbox, AddrMan, BanList, ConnMan, `dashd-clock`, the CH1 contract, the P1/P2b/P3/P4/P5/overlay seams), performance against dashd, testability, the seven-phase plan, the 21 errata rows. Core equivalence and indistinguishability are reviewer 1's scope; I read Core only where an architecture, interface or test claim depends on it. |
| Checked against | Dash Core `v23.1.8` and `v24.0.0-rc.3` (`git show <tag>:…`; extracts in `/dev/shm/p2a-rev-b-tmp`, deleted after the review); `A1-architecture.md` §2.10, §2.10.2, §2.14, §3.1, §3.3, §3.9, §4.10; PLAN §4 rows P1, P2a, P2b, P3, CH1, ND1, RP0, RP1a, O11, O13a, PF0, B1, M2 exit, the scheduling model (§5); PRD NFR-FP-01..03, R-8; `reviews/PF0-r1-opus.md`; memory notes `pf0-counter-gate`, `dash-sha256-sh1`, `node-bench-harness`; node-bench at `node/bench-harness` (`nodebench/scenarios/ibd.py`, `common.py`); perfgate `workloads.toml` at `node/pf0-counter-gate`; rust-dashcore `dev` 67b307c6 `dash-spv/src/network/` |
| Mode | read-only; nothing run against the oracles; no dashd started |

## Summary

The core shape is right and I would not change it: one dedicated actor thread that ports `ThreadMessageHandler` line by line, Core's two receive queues, gates before reads with decoding at Core's read point, AddrMan as a mutex (the "small actors" in A1 were wrong), BanMan behind a sync trait, a shared node table, a separate `dashd-clock` crate, Core's `peers.dat` format, and no `getsporks` stub. Not reusing dash-spv's network manager is also correct: it is SPV-shaped (A1 §2.14 says the same). The one piece of dash-spv worth copying is a pattern, its bounded and ordered headers2 decompression (`dash-spv/src/network/manager.rs:37-120, 660-700`), which M2 below uses.

I found no blocker. I found ten majors in four groups:

- **Concurrency and robustness.**
  - **M1:** the wait-for graph is never stated, and three plausible implementation choices close a deadlock cycle.
  - **M2:** the speculative header job is unbounded, and pre-verack peers can trigger it. Its claimed benefit does not exist on the path that matters.
  - **M10:** zero-copy payloads pin read buffers outside the flood limit.
- **Interfaces.**
  - **M5:** the CH1 contract assumes lock-free index lookups that A1's index can't give, and it is missing the active chain and a lazy IBD answer.
  - **M6:** `NetHandler` takes a decoded message, which contradicts the design's own read-point rule and Core v24's handlers.
- **Performance evidence.**
  - **M3:** the 2× header claim depends on an X11 backend P2a doesn't control, and one of its gates can't be met.
  - **M4:** neither node-bench nor PF0 can currently measure the header path against dashd.
- **Tests and plan.**
  - **M7:** the +C probe can't detect anything at M2.
  - **M8:** the O1c oracle API can't compare multi-peer logic or RNG-dependent outcomes.
  - **M9:** P2a is effectively XL, and its phase acceptance depends on tasks that land after it or have no owner at M2.

Every fix is local: a contract sentence, a signature change, a PLAN edit or a narrower gate.

**Verdict: APPROVE-WITH-CHANGES.**

---

## Major findings

### M1 (major): the wait-for graph is unspecified, and three plausible choices close a deadlock cycle

**Where:**
- §2.2 l.115-117: the actor blocks on `recv`, and `PeerManagerHandle` calls "become commands drained at the top of the next pass".
- §2.4 l.287-296: `chain_event`, `misbehaving` and the other handle methods.
- §5.3 l.1306: the outbox is "per-node SPSC".
- A1 §2.10 events: ">10 pending blocks validation".
- A1 §2.14: "the writer runs a bounded send queue with backpressure".

**The edges.**
1. **Actor → V.** At M2 the actor blocks on `ChainCommand::ProcessHeaders`. From P3 on it also blocks on `ProcessBlock`.
2. **V → events.** V blocks on the dispatcher when more than 10 events are pending (A1 §3.1 `events` row).
3. **Events → actor.** The dispatcher calls subscribers that need ordering synchronously (A1 §2.10). PeerManager's `UpdatedBlockTip`/`BlockConnected` is `PeerManagerHandle::chain_event`, which enqueues a command for the actor. The design never says that queue is unbounded.

**Failure scenario.** An implementer picks a bounded crossbeam channel for actor commands, which is the natural default. During IBD at M3, V connects blocks and emits events, and the actor's queue fills while the actor is blocked in `ProcessBlock`. The dispatcher then blocks on the full queue, V blocks on the dispatcher, and the actor waits on V forever. The whole node stalls.

**A second instance: the outbox.**
- §5.3 calls the outbox SPSC.
- §2.2 l.117 makes `push_message` thread-safe because overlay threads push into it. That makes it MPSC.
- A1 §2.14 calls it bounded with backpressure.

If `push_message` ever blocks, a peer that stops reading stalls the actor. Core's `PushMessage` never blocks: the queue is unbounded, and `fPauseSend` only stops the polling.

**A third instance: shutdown.** If V stops before the actor is interrupted, the actor stays blocked on a reply that never arrives.

**Fix.** Add a "Liveness" subsection to §2.2 with these invariants:
- (i) No thread ever waits on the actor. Every `PeerManagerHandle` method is non-blocking: either an unbounded queue, or, for tip events, a coalescing cell that accumulates up to 8 hashes per peer as Core's `m_blocks_for_headers_relay` does. `stats` is lock-free.
- (ii) `push_message` never blocks. It uses an unbounded MPSC with the byte counter, and fixes the "SPSC" wording plus the A1 §2.14 line through §3.15.
- (iii) A dropped reply channel means "interrupted". The actor exits the pass, and ND1 interrupts the actor before it stops V.
- (iv) A lock hierarchy: NodeTable → per-node {inbox, outbox, small mutexes} → wake mutex. AddrMan, BanList and the address cache are **leaf** locks with no I/O under them. It is checked by A1 §3.1's debug lock-order checker.
- (v) A `sim` test that blocks V, floods chain events and pushes from an overlay thread, and asserts progress.

### M2 (major): the speculative header job is unbounded, runs for peers Core would drop for free, and buys nothing on the IBD path it was added for

**Where:** §3.4.9 rules 2-3 (l.837-838), §3.1.3 l.497, §5.2 l.1298, §8 Q10 / DEC-48 Q10.

**Facts.**
- **Pre-verack peers are hashed.** The job is submitted "when the node's common version is already set", which is from the peer's `version` onwards. A peer that sent `version` but not `verack` and then floods `headers2` is dropped by gate 5 without a read in Core (`v24:np:4363-4366`). Under rdashd, each 8,000-header message costs ~8,000 X11 (~0.25 s of CPU at 30 µs each, §3.4.9 l.832) on the shared `prevalidate` pool before the gate discards the slot. The same holds for `LoadingBlocks()` and for `fDisconnect` peers. Rule 3 only discards the *result*.
- **Nothing bounds the job.**
  - There is no per-peer limit, no global semaphore and no early abort on count > limit. The +20 check is the actor's, "before the slot is consulted". Whether the job also stops is unstated.
  - Decoded headers take ~112 B against ≥ ~39 B on the wire, so the slots expand a peer's 5 MB flood allowance (`-maxreceivebuffer`) by up to ~2.9×, outside the `fPauseRecv` accounting.
  - Even for post-verack peers, Core serialises this hashing on one thread (msghand). rdashd lets one peer occupy the whole pool.
- **The pool is shared and FIFO.** `prevalidate` also runs S1/S2 block work and rescans (A1 §3.1). The claim "joining a running job is never slower than computing" is false when the job, or one of the 256-header `headers` sub-jobs, is still *queued* behind block prevalidation. The actor, and with it every peer, then waits on unrelated work.
- **The benefit.** `headers2` hashing is strictly sequential (`block.cpp:122`: each prev hash is `X11(previous)`), so the pool can't parallelise it. In IBD there is one sync peer (§3.4.2 l.786). The actor is idle when the batch completes, is woken at once, and joins a job that has just started. Latency is identical to hashing inline. The whole measured gain comes from hashing **once instead of three times**, and that needs no side job. dash-spv, which does parallelise headers2, bounds itself to 4 in flight with a semaphore and reorders results (`manager.rs:37-120`).

**Fix.**
- Phase E ships **hash-once inline**: the decoder returns hashes (§3.4.10 item 4), the actor builds `HeaderBatch`, and the CH1 contract is unchanged. Keep the side job out until PF1 shows the actor is busy when batches arrive. That is Fable's position, and the design's own numbers support it.
- If the side job is kept anyway:
  - submit only when `successfully_connected && !disconnect && !loading_blocks` and the count passes the limit (the job reads the count first);
  - allow at most one job per peer and a global byte semaphore;
  - count slot memory toward `fPauseRecv`;
  - the actor claims unstarted work with a CAS instead of waiting on a queued job;
  - drop the 256-way split, which serves only the rare `headers` path.

### M3 (major): the 2× header claim depends on an X11 backend P2a doesn't control, and the `headers` gate can't be met as written

**Where:** §5.1 row 1 (l.1290), §5.2 l.1298 ("P2a calls `dash-pow`'s `x11` entry point"), §5.4 l.1316 ("instructions per header ≤ Core's / 2"), phase E acceptance l.1439.

**Facts.** Measured user-space instructions per byte for an 80-byte header (memory note `dash-sha256-sh1`):

| Implementation | ins/byte | vs Core |
|---|---:|---:|
| Core | 2,120 | 1.0× |
| `rs-x11-hash` | 2,301 | 1.085× |
| base-sdk `dash-pow` @33fcc59 | ~4,950 | ~2.3× |

BS15 has not chosen the default yet. A1 §3.9 row 6 still assumes "X11 parity".

**Consequences per header.** Core does 3 X11 for `headers2` and 2 for `headers`. rdashd does 1, plus decode.

| Backend | `headers2` vs Core | `headers` vs Core |
|---|---|---|
| rs-x11-hash | ≈ 2.8× better | ≈ 1.84× better, which **fails** the "÷ 2" gate |
| dash-pow | ≈ 1.3× better, which **fails** the 2× target | ≈ 0.86×, i.e. **slower than dashd** (charter rule) |

**Fix.**
- Make BS15's decision (or `dash-pow` at ≤ Core instructions per byte) an explicit phase E dependency.
- Gate `headers_decode_hash_2000` at **≤ Core**. Gate `headers2_*` at ÷ 2 only if the X11 ratio allows it.
- Record the X11 ratio as its own counter, so a header-path regression and a backend regression can't hide behind each other.
- Add an erratum row against A1 §3.9 row 6.

### M4 (major): nothing in place can measure the header path against dashd at M2

**Where:** §5.1 row 1, §5.4, phase E and G acceptance (l.1439, l.1441), PLAN P2a BG ("`ibd-*` `header_sync_s` (window)").

**Problems.**
- **(a) node-bench.** `header_sync_s` is a by-product of the `ibd-*` scenarios.
  - `run_ibd`'s `is_done` requires `blocks >= target` and `initialblockdownload == false` (`nodebench/scenarios/ibd.py:100-104`). An M2 rdashd never connects a block, so the run never completes.
  - The sampler polls `getblockchaininfo` (`common.py:237-239`), which is an RP1a RPC (M3).
  - On `-seed`, dashd starts block download near the end of header sync, which competes with header processing on msghand. That makes the two runs unequal even when both complete.
- **(b) PF0.**
  - PF0 has no X11 workload and no header workload (`workloads.toml`; PF0 r1 m1).
  - Its node workloads are not CPU-gateable today: instructions are 5.7-47% noisy (PF0 r1 M5).
  - `p2p/headers_sync_replay` runs "through the actor" with a pool. Per `pf0-counter-gate`, idle pool workers spin and move instruction counts ~5% unless the process is pinned to one CPU with `RAYON_NUM_THREADS=1`.
- **(c) The Core counterpart.**
  - `src/bench/net_processing.cpp` exists at v24 only (`git ls-tree v23.1.8 src/bench` has no such file). The mainnet profile (`V23_1`) has no Core bench to compare against.
  - §5.4 compares Rust with a **stub** `ChainAccess` against Core's `ProcessMessage(HEADERS2)`, which runs real `AcceptBlockHeader`. That comparison can't support a ÷ 2 claim.
- **(d) `header-sync-regtest`.**
  - Regtest has `fDefaultConsistencyChecks = true` (`v24:chainparams.cpp:892`).
  - `ProcessNewBlockHeaders` calls `CheckBlockIndex()` after **every** header (`v24:validation.cpp:4737`). That is a full index walk, so 200,000 headers cost O(n²) on dashd.
  - Without `-checkblockindex=0` on both sides, and rdashd's equivalent, the workload measures the consistency walk, not P2P.

**Fix.**
- Add a node-bench **`headers-{mainnet,testnet}-replay`** scenario. It uses §6.6's scripted header server for both binaries, ends at "headers == target" via `getblockheader`/log, and needs no block download.
- Write the DEC-16 Core header bench at **both** pins with a mainnet `TestingSetup` and real acceptance. Compare it with Rust using CH1's real acceptance, or stub both at the same boundary.
- PF0 header benches run single-threaded with hashing inline.
- Pass `-checkblockindex=0` on regtest workloads.
- Until all this exists, phase E/G acceptance says "recorded", not "≤".

### M5 (major): the CH1 contract assumes index reads that A1's design can't provide, and leaves out the active chain and IBD semantics

**Where:** §2.4 `ChainAccess` l.231-247, §2.5.3 l.353-358, §3.4.2 l.774; A1 §2.10 l.966 and §2.10.2 l.1072.

**Gaps.**
- **(a) `lookup` and `locator` are called lock-free.**
  - A1's hash index is a plain `hashbrown::HashTable<BlockId>` that V mutates (A1 l.966). Concurrent reads during an insert or resize are unsound.
  - A1 §2.10.2 says locators "run as `ChainCommand::Query` on V".
  - Taken literally, every `inv` and `headers` lookup from the actor would queue behind V's work, which is worse than Core's `cs_main`. Reading a non-concurrent table lock-free is a data race.
- **(b) `active_contains` reads the active chain, which lives only in the throttled `ChainView`** (≤ every 100 ms in IBD). `TipFacts` is unthrottled but carries no active chain. P2b's `getheaders` serving walks `FindForkInGlobalIndex` + `ActiveChain().Next` (`v24:np:4784-4815`, the fork at `:4813-4815`), and block announcements use `ActiveChain().Contains`. Both need an active-chain view that is consistent with `TipFacts.tip`.
- **(c) `ibd: bool` is a snapshot.**
  - Core's `IsInitialBlockDownload()` is evaluated lazily against `GetTime()` and the tip time, and latches.
  - Suppose `setmocktime` moves the clock so that the tip becomes recent with no tip change. Core flips to not-IBD on the next call. `TipFacts.ibd` stays stale until the next publish.
  - That staleness reaches `MaybeSendAddr`, object requests and `GetDesirableServiceFlags`.
- **(d) Section 6 writes into CH1.** "`best_header = tip` if unset" (l.774) is a write to CH1's state. CH1 should guarantee `best_header` is non-null after load, as Core's `LoadBlockIndex` does, and the line should become an assertion.
- **(e) Payload change.** A1's `ProcessHeaders { headers: Vec<BlockHeader>, … }` becomes `HeaderBatch { headers, hashes }`. That is an A1 change with no erratum row.
- **(f) Latency under a slow chainstate.**
  - Core's `ActivateBestChain` releases `cs_main` between steps, so msghand's `ProcessNewBlockHeaders` can interleave with an RPC- or CL-triggered reorg.
  - A FIFO V thread that runs `activate_best_chain` to completion holds the actor, and every peer, for the whole reorg.

**Fix.**
- Specify a concurrent append-only index. For example: open addressing with atomic `u32` slots, grown by RCU, and entries never deleted. Alternatively an `RwLock` taken per insert.
- State that `GetLocator` reads only immutable `pprev`/`pskip`, so it is lock-free by construction, and add the A1 §2.10.2 erratum.
- Add an unthrottled active-chain view published with `TipFacts` (an `imbl::Vector` or a height→`BlockId` seqlock array).
- Replace `ibd` with `ChainAccess::is_initial_block_download(now)` using CH1's latch.
- Require V to check its command queue between ABC steps.
- Send all of this to the CH1 designer now. CH1 is at wk 8.5 on the critical path.

### M6 (major): the `NetHandler` signature hands overlays a decoded message, which contradicts the read-point rule and Core v24's handlers

**Where:** §2.4 l.270-277 (`process_message(…, msg: &DecodedMsg) -> MessageProcessingResult`), §3.2.2, §3.2.4 l.609.

**What Core does.**
- Core's `NetHandler::ProcessMessage(CNode&, const std::string&, CDataStream& vRecv)` returns `void` (`v24:net_processing.h:122-145`).
- v24 handlers act **before** reading and on the **raw size**. For example, `NetQuorum` QGETDATA (`v24:llmq/net_quorum.cpp:79-92`):
  1. +10 "not a verified masternode or a qwatch connection" and return, with nothing read;
  2. then +100 if `vRecv.size() > GetSerializeSize(request)`;
  3. only then `vRecv >> request`.
- They also call `PeerMisbehaving`, `PeerRelayInv` and `PeerConsumeObjectRequest` part-way through (`net_governance.cpp:73-267`, `net_instantsend.cpp:67-420`).

**Why the design's signature fails.** With a pre-decoded `DecodedMsg`, the dispatcher decodes first. A malformed QGETDATA from an unverified peer then becomes a decode error rather than Core's +10. The raw-length check can't be expressed at all. §3.2.2 makes exactly this argument for P2a's own handlers.

**Fix.**
- Pass `msg: &RawPayload` and give `HandlerCx` a `read::<T>()` that builds `ProtoCtx` at the call and maps errors to Core's `Throw`.
- Keep `MessageProcessingResult` as an optional return for v23-style handlers.
- P2a's own §3.11 stubs use the same signature.
- This seam is shared by P5a, SP0, SP1, P6a/b, P7, AM1 and LQ5, so it has to be fixed before any of them starts.

### M7 (major): the +C probe can't distinguish anything at M2

**Where:** §3.12 +C l.1152, §6.1 matrix l.1330, phase E acceptance.

**Fact.** `getheaders` is served from the **active chain**: `FindForkInGlobalIndex(locator)`, then `ActiveChain().Next` (`v24:np:4784-4815`, the fork at `:4813-4815`). On mainnet and testnet, below minimum chain work, the reply is empty (`:4786-4793`). An M2 node never connects a block, so B's reply never contains A's headers, whatever the processing order, in either binary. "A sends headers, B immediately `getheaders`; B's reply includes them" can't fail, so it is not evidence for +C.

**Fix.** Use probes whose answer depends on the best header:
- **(1) inv probe.** A sends `headers` ending in H. In the same TCP write window, B sends `inv(MSG_BLOCK, H)`.
  - If A's batch was processed first, H is already had, so B gets no `getheaders`, subject to B's sync state; set it up so the inv path is armed.
  - Otherwise B gets a `getheaders`.
- **(2) headers probe.** B sends a one-header `headers` whose prev is H.
  - Processed first: it connects and is accepted silently.
  - Otherwise: it is unconnecting, so B gets a `getheaders` and `nUnconnectingHeaders` goes up.

Run both against both pins, and keep A1 §4.10's block/tx form of +C for P3/P5.

### M8 (major): the O1c session oracle can't compare multi-peer logic or RNG-dependent outcomes

**Where:** §6.2 l.1340-1351 and l.1368; DEC-48 Q1.

**Problems.**
- **(a) One peer per session.** The API is `oracle_peer_session_new(…, int conn_type, …)` with no way to add peers. Much of P2a's logic is cross-peer:
  - sync-peer selection (`n_sync_started`, `n_preferred_download`);
  - protection limited to 4;
  - `RelayAddress` choosing the top n over peers;
  - `EvictExtraOutboundPeers`;
  - +C.

  None of it can be compared.
- **(b) Unseedable randomness.**
  - The `addr` handler shuffles with an **OS-seeded** `FastRandomContext()` (`v24:np:4429`). `g_mock_deterministic_tests` does not reach it; it only affects `GetRand` (`v24:random.cpp:639-643`).
  - So both the order of `addrman.Add` arguments and *which* addresses get rate-limited are random in Core. §6.2 compares "addrman calls (SetServices/Good/Add with arguments)" exactly, so `diff_p2p_addr` will diverge at random.
  - Ping nonces (`GetRand`), connman SipHash seeds (`RelayAddress`, the address cache, `nLocalHostNonce`) and the 32-byte challenge have the same problem.
- **(c) Static scripts.** `deliver(cmd, payload)` can't express "reply to the nonce this side sent", which `diff_p2p_pong` needs.
- **(d) Process-global state.** Core's mocktime, logger callback and `gArgs` are process-global, so sessions must be one per process at a time. That is unstated, and it matters for fuzz throughput.

**Fix.**
- Add `oracle_peer_session_add_peer(conn_type, services, version) -> id` and per-peer deliver and outcome calls.
- A DEC-16 patch seeds the handler's `FastRandomContext` and `GetRand` from a session seed and logs each draw. Rust takes the draws through its RNG seam, so the streams match by construction.
- Define per-field normalisation for what stays random.
- Add capture and substitute to the script format.
- Document one session per process.
- Size O1c at **M**, not S-M.

### M9 (major): P2a is effectively XL, and its phase acceptance depends on tasks that land after it or have no M2 owner

**Where:** §7 l.1429-1443; PLAN P2a (L, "D2"; model durations l.384: L = 3 weeks), O13a (dep P2b), RP1a (M3), O11 (dep ND1, RP0), M2 exit 3.

**(a) Size.** Seven phases:
- a full AddrMan port with a 24 CPU-h differential;
- full ConnMan (ladder, anchors, seeds, linger, v1 retry);
- the actor and 74-row policy;
- headers sync and eviction;
- addr relay;
- about 20 `diff_p2p_*` targets at ≥ 24 CPU-h each;
- PF0 benches.

That is two L tasks of work. The model's single L (3 weeks + 2 design + 1 review) puts P2a → P2b (2.5) → O13a (2.5) inside wk 18 with about 1.5 weeks of slack. A realistic 6-7 weeks of implementation consumes that slack and moves M2.

**(b) Acceptance depends on tasks that come later or have no owner.**
- **O13a.** Phase D and G acceptance requires "O13a outbound transcripts … diff-empty". PLAN makes O13a depend on P2b, which depends on P2a.
- **Functional tests.** Phase D and E acceptance lists FTs. They need O11 (which depends on ND1 and RP0) and RPCs with no M2 owner:
  - `setmocktime`, `mockscheduler`, `getblockchaininfo` and `syncwithvalidationinterfacequeue` are RP1a, at **M3**;
  - `getpeerinfo`, `getnetworkinfo`, `addnode`, `addconnection`, `disconnectnode`, `setnetworkactive`, `getconnectioncount` and `ping` appear in no PLAN row;
  - M2 exit 3 already needs `getpeerinfo` and `getblockchaininfo` at M2;
  - Dash's `bump_mocktime` calls `setmocktime` **and** `mockscheduler` on every node (`v24:test_framework.py:883-900`), and `p2p_addr_relay` (8 calls), `p2p_addrv2_relay` and `p2p_blocksonly` use it.
- **O1c.** It gates B and C, but has no PLAN row or date yet.
- **Live sync.** Phase E's acceptance includes the live mainnet/testnet sync (M2 exit 1). That ties a P2a merge to ND1, RP and CH1 being complete.

**Fix.**
- Split into **P2a-core** (A, C, E: identity, actor, gates, handshake, headers) and **P2a-net** (B, D, F: AddrMan, ConnMan, addr relay). Both can run in parallel after A, as §7 already allows. Re-run the M2 model.
- Add PLAN rows:
  - **O1c** (M, scheduled with phase A);
  - an **RP-net** task at M2 for the network RPCs and the `addconnection` back end (§8 Q5);
  - move `setmocktime`, `mockscheduler`, `getblockchaininfo`, `getchaintips` and `syncwithvalidationinterfacequeue` into RP0 or another M2 task.
- Split O13a into **O13a-out** (depends on P2a-D) and **O13a-in** (depends on P2b).
- Gate each phase merge on `sim`, oracle and fuzz evidence. Move the FT, O13a-live and live-sync evidence to the M2 exit checklist.

### M10 (major): zero-copy `Bytes` payloads pin whole read buffers, which the flood limit doesn't count

**Where:** §5.3 Inbox row (l.1305, "`Bytes` payloads (zero copy from the read buffer)"), §3.1.3 (`fPauseRecv` on message bytes), §3.8.7 (64 KiB reads).

**Failure scenario.**
1. A peer stops reading from us.
2. Our send queue passes 1 MB through a few `getheaders` replies (P2b), and `fPauseSend` is set.
3. The actor then stops polling that peer (§3.1.2 step 6).
4. The peer trickles one small message (`ping`, 32 B on the wire) per TCP segment.
5. Each socket read's allocation (up to 64 KiB) stays alive through the one `Bytes` slice taken from it.
6. `fPauseRecv` counts ~32 B per message, so it trips only after ~5 MB / 32 B ≈ 160 k messages. By then they pin up to ~10 GB.

Core copies each message into its own `CDataStream`, so it has no such amplification.

**Fix:** copy every payload below a threshold (for example 4 KiB) out of the read buffer, or charge the retained capacity to the flood counters. Add the `sim` probe above.

---

## Minor findings

- **m1. The `BlockSync` seam is incomplete** (§2.4 l.249-255).
  - The `inv` block path needs `IsBlockRequested(hash)` (`v24:np:4537`), which the trait doesn't have.
  - Core runs section 9 (stalling, which may return), then sections 10-11 (`ConsiderEviction`, which can set `fDisconnect` without returning), then section 12 (getdata for blocks), which runs even after that `fDisconnect` (`v24:np:6752-6838`). One `send_section` can't sit at both positions.
  - Add `is_block_requested`, `stall_section`, `getdata_section` and a `block_checked` hook now, so P3 doesn't have to change the trait.
- **m2. The P1 factory takes a concrete `TcpStream`** (§2.4 l.322-326). `sim`'s in-memory transports and P8's proxied streams can't use it. Take a stream trait (`AsyncRead + AsyncWrite + Send`).
- **m3. `RawMsg.speculative` is a P2a field on P1's type** (l.226-227). Wrap `RawMsg` in an inbox-side struct.
- **m4. Actor timers and `mockscheduler`.** The actor's timers (§3.1.6: stale tip every 45 s; `ReattemptInitialBroadcast` later) must sit on ND1's scheduler time base so `mockscheduler` (`MockForward`) moves them. Otherwise every `bump_mocktime` in the FTs fires them in dashd and not in rdashd.
- **m5. Stats freshness** (§5.1 l.1294). "Snapshots refreshed by the actor" leaves the refresh point open.
  - FTs read `getpeerinfo` right after `sync_with_ping`: `addr_processed` and `addr_rate_limited` in `p2p_addr_relay`.
  - Refresh after each `process_messages` and before any send in that pass, or use per-field atomics as Core does.
  - Refreshing every peer every pass also undercuts the idle-pass CPU budget. Use dirty flags.
- **m6. ConnMan on tokio contradicts A1 §3.1 "nothing on the `net` runtime blocks"** (§2.2, §3.8.1).
  - `Select` (up to 100 tries), `GetAddr`, `Check()` with `-checkaddrman`, and `DumpAddresses` all hold the AddrMan mutex. Core writes `peers.dat` while holding `cs`, and here that would happen on a tokio worker.
  - Run `opencon`, `addcon` and `dnsseed` as OS threads, as Core does, or under `spawn_blocking`. Serialise into memory under the lock and write outside it.
- **m7. Determinism in `sim`.** The actor (an OS thread waiting on a `Condvar` against real steady time) and the ConnMan loops can't run under a virtual clock. tokio's paused time doesn't cover OS threads.
  - Expose step functions: `MessageLoop::run_one_pass(now)` and `opencon_step(state, view, now, rng) -> Action`. The threads become thin drivers.
  - Without that, `connman_sched` and the KS timer tests aren't deterministic.
- **m8. Six cited tests don't exist at either pin:**
  - FTs `feature_anchors_db`, `p2p_seednode`, `p2p_outbound_eviction` and `p2p_headers_sync_with_minchainwork`;
  - unit-test file `asmap_tests` (the asmap cases live in `addrman_tests.cpp` and `fuzz/asmap*.cpp`).

  Phase D and E acceptance cites `p2p_seednode` and `p2p_outbound_eviction`. Replace them with `feature_config_args` (`-seednode`) and `denialofservice_tests` `outbound_slow_chain_eviction`.
- **m9. §3.4.3 steps 10-11 (l.799-800) say "Count == limit" without saying which limit.**
  - Core uses `GetHeadersLimit(pfrom, UsesCompressedHeaders(peer))`, which follows the **peer's bit 11** (`v24:np:3370-3380`).
  - Step 3 (`:5324`) follows the **received message type**.
  - A bit-11 peer at common ≥ 70235 that sends 2,000 `headers` gets no "more" request and has `may_have_more_headers == false`.
  - State this, and add a `diff_p2p_headers` case. Flagged for reviewer 1.
- **m10. NFR-FP-03's latency clause is conditional:** "as long as NFR-FP-02 holds". A ~3× shorter delay between a `headers2` batch and the next `getheaders` is a timing feature the sync peer can see directly. Add it to O13b's classifier features, and cite R-8. Errata row 21 should say so instead of "none".
- **m11. `dashd-clock` ownership.** "Whichever of CH1 and P2a starts first creates it" (DEC-48 Q3) invites two parallel creations. Name one owner (P2a phase A) and edit ND1's PLAN row, which still lists `NodeClock`/mocktime.
- **m12. The BanList contract** (§2.4 l.264) should state that it is non-blocking with no I/O under its lock, because P4's `banlist.json` dump must not stall the actor's `addr` path.
- **m13. ConnMan's chain inputs.** ConnMan reads the tip MN list and the verified ProRegTx set (§3.8.2 steps 6 and 8) and the IBD service cache. None of these is in the seams table. Add a `ConnChainView` seam.
- **m14. The outbound ladder can't be tested deterministically.** Regtest and devnet skip the outbound accounting (PQ-12), so feelers, block-relay slots, anchors and the netgroup rule can't be diffed in the lab. Only the live 24 h soak covers them.
  - Either add a DEC-16 hidden regtest switch to both binaries that runs the accounting block, driven with a netns address universe,
  - or record this as residual risk for O13b.
- **m15. No end-to-end test of `time_source` (P2P-16) per profile.**
  - Add a `sim` and O13a variant with ≥ 5 outbound peers whose `version.nTime` is skewed.
  - Under `V23_1` the sync-start rule and the headers-timeout scaling move; under `V24_0` they don't.
- **m16. The `send_messages` fast path** (§5.1 l.1293, "exits after sections 1-5") must still run the time-driven sections 10 and 11 (headers timeout, `ConsiderEviction`). State the predicate, and add a property test that the fast path and the full path are equal.

## Nits

- n1. §3.4.9 l.832: say which implementation the "383 ns/byte" `Pow_X11_0080b` figure is.
- n2. §3.4.9 l.843: name the `-debug` kill-switch option and register it with ND1 (A1 §2.18 option table).
- n3. §5.4 calls the header bench "criterion over `sim`". After M2 that bench is single-threaded, so drop "over `sim`".

---

## Section answers

### 1. Concurrency and interfaces

| Item | Ruling |
|---|---|
| Single actor, two-queue poll | **Approve.** It is a faithful structure. It is fair under thousands of peers for the same reasons Core's loop is: one message per peer per pass, a fresh shuffle, `fMoreWork` ignoring send-paused peers. Per-pass cost is O(n), like Core's. The liveness invariants of M1 must be added. |
| AddrMan `Mutex` | **Approve.** It is a leaf lock with no callouts (Core's `AddrManImpl` calls nothing outside). Lock order is safe. m6 covers I/O and runtime placement. |
| BanMan sync trait | **Approve** with m12. |
| tokio ConnMan | **Approve-with-change** (m6, m7): OS threads with sans-io step functions are simpler and match Core. |
| `dashd-clock` | **Approve** (m11). |
| CH1 contract | **Changes** (M5). The field list for `UpdatePeerStateForReceivedHeaders`, the timeout and `ConsiderEviction` is otherwise complete: `last_accepted`, `HeaderReject {result, reason, debug}`, tip/best-header work and time, min chain work, the arena's `prev`/`ancestor`/`chain_work`. I checked them against `v24:np:3267-3386`. |
| P1 seam | m2, m3, M10. The per-read batch with a shared timestamp is right. |
| P2b seam | Fine. The shared `version` handler and the cache in ConnMan are good. P2b needs M5(b) for `getheaders` serving. |
| P3/P4/P5 seams | m1. `ObjectRequests`, `TxRelay`, `OrphanStep`, `Serving` and `BanList` are adequate for M2. |
| Overlays (`NetHandler`) | **M6.** |
| dash-spv reuse | **Agree** with not reusing it. Borrow the bounded, ordered headers2 pattern for M2. Keep `dash-testkit` for live interop. |

### 2. Performance

| Area | Assessment |
|---|---|
| Header-sync throughput | The 2× is real on `-seed` for `headers2` only with an rs-x11-class backend (≈ 2.6-2.8×). With `dash-pow` it is not (M3). On `-net` it is round-trip bound, so parity is the right expectation, as the design says. The gain comes from hashing once; moving the hash off the actor adds nothing in IBD (M2). |
| Per-message latency and CPU per peer | Parity by construction: Core's loop, a futex wake, synchronous `fPauseSend`. The fast path (m16) and the stats refresh (m5) are where rdashd could lose idle-pass instructions. |
| Memory | AddrMan's flat tables (~320 KiB) plus a slab is fine. The per-peer worst case equals Core's, except for M10 and M2's slot expansion. |
| Measurement | **Not set up** (M4). PF0 has no header or X11 workload and no CPU-gateable node workload. node-bench's metric can't be produced by an M2 node. The Core bench is missing at v23. |

### 3. Testability

| Item | Assessment |
|---|---|
| Sans-io `PeerLogic` + `sim` | Good. It needs step functions for the loop and ConnMan (m7). |
| O13a with frozen mocktime | Sound. The sync tick's `GetTime() - nTimeLastProcess < 6` gate (`v24:node/sync_manager.cpp:157`) can't fire. Inactivity uses mockable time too (`net.cpp:2302`). The +C probe must change (M7). The window covers behaviours 1-6. Behaviours 7, 15, 25 and 26 are compared with dashd only through FTs (assertions, not diffs), the analytic KS tests in `sim`, and the 24 h soak. |
| `diff_p2p_*` / O1c | Blocked on M8. |
| `diff_addrman` | **Good.** Deterministic mode with the zero-key stream makes every draw comparable. This is the strongest differential in the design. |
| Model fuzzers | Adequate. Add a property test that inline and speculative hashing give equal results under random completion order, if the side job is kept. |
| Ported tests | Fix the names (m8). `denialofservice_tests` needs `sim` equivalents of `ConnmanTestMsg` and `AddTestNode`. |

**What is missing for end-to-end indistinguishability per profile at M2:**
- a non-vacuous +C probe (M7);
- multi-peer and RNG-aligned differentials (M8);
- a P2P-16 profile probe (m15);
- a deterministic outbound-ladder lab (m14).

Mainnet-only data paths (fixed seeds, the DNS seed) are covered only by table equality and the live soak. That is acceptable if it is stated.

### 4. Plan

See **M9**. The dependencies inside §7 are otherwise right:
- A before B and C;
- P1 before D's live tests;
- CH1 before E merges;
- BS7's headers2 before E (it is critical);
- E and F in parallel.

**Missing on the M2 critical path:**
- O1c;
- RP-net and the test RPCs;
- O13a-out;
- the node-bench headers scenario (M4);
- BS15's backend decision (M3);
- the CH1 contract amendments (M5), which must reach CH1 before its implementation freezes.

**Oversized:** P2a as one L.

---

## Errata rulings (§3.15, 21 rows)

| # | Row | Ruling | Evidence or amendment |
|---|---|---|---|
| 1 | `qwatch` position | **Agree** | `v24:np:4224-4232`: inside `!RejectIncomingTxs`, right after `senddsq` |
| 2 | `getaddr` on every non-inbound except block-relay-only | **Agree** | `SetupAddressRelay` returns false only for `IsBlockOnlyConn()` (`v24:np:6283-6298`); push at `:4116-4132` |
| 3 | two receive queues | **Agree** | `v24:net.cpp:4787, 4813-4818` |
| 4 | `fMoreWork` and the throttle stamp | **Agree** | `v24:net.cpp:3686-3704` |
| 5 | `NetHandler` hooks | **Agree with amendment** | the three hooks don't exist (`v24:net_processing.h:122-145`); the replacement trait must take raw bytes (M6) |
| 6 | `getsporks`/`govsync` timing | **Agree** | `scheduleEvery(1 s)` (`v24:sync_manager.cpp:22-30`), the 6 s mockable gate (`:157`) |
| 7 | DNS "3 at a time" note | **Agree** | — |
| 8 | "small actors" → mutex / sync trait / tasks | **Agree with amendment** | also amend A1 §3.1's `net` row and its "nothing blocks" rule (m6) |
| 9 | `TipFacts` unthrottled | **Agree with amendment** | add the active-chain view, the lazy IBD, the concurrent index, the locator/Query erratum and the `HeaderBatch` payload (M5) |
| 10 | `dashd-clock` | **Agree** | and edit PLAN ND1 (m11) |
| 11 | no feelers on regtest/devnet | **Agree** | the `!AllowMultipleAddressesFromGroup()` block wraps all counting (`v24:net.cpp:3028`); counters stay 0, so the ladder always picks full-relay |
| 12 | `#26` reword | **Agree** | `InactivityCheck` (`v24:net.cpp:2298-2331`): either direction never used, or the handshake is incomplete |
| 13 | behaviour 7 split with P2b | **Agree** | — |
| 14 | P2P-04 reader | **Agree** | — |
| 15 | P2P-16 reader | **Agree** | — |
| 16 | the ASMap health-check row | **Agree** | DEC-48 Q6 |
| 17 | #7283: steady clock under both profiles | **Agree** | differs only across wall-clock steps; declare it in O17 as an unobservable deviation |
| 18 | P2a ports the asmap interpreter | **Agree** | — |
| 19 | O1b → O1c | **Agree with amendment** | needs a PLAN row, size M, and the M8 requirements |
| 20 | CS0 variant name | **Agree** | moot: done per DEC-48 Q11 |
| 21 | NFR-FP-03 "none; recorded" | **Disagree** | the clause is conditional on NFR-FP-02; add the classifier feature (m10) |

**Rows missing from §3.15:**
- (a) A1 §2.10's `ProcessHeaders` payload → `HeaderBatch` (M5e).
- (b) A1 §2.10.2 "locators via `ChainCommand::Query`" → lock-free walk over immutable fields, plus a concurrent hash index (M5a).
- (c) A1 §2.14 "the writer runs a bounded send queue with backpressure" → unbounded with `fPauseSend` (M1).
- (d) A1 §3.1 `net` row and "nothing blocks" (m6).
- (e) A1 §4.10 +C has no headers-only form at M2 (M7).
- (f) A1 §3.9 row 6 "X11 parity" against the measured backends (M3).
- (g) PLAN: O13a's dependency, the RP network and test RPCs at M2, the P2a size (M9).

---

## Verdict

**APPROVE-WITH-CHANGES.** I found no blocker. M1-M10 must be addressed in the design (r1), and M5 and M6 must be sent to CH1 and the overlay owners before their designs freeze. Minors can be fixed during implementation.
