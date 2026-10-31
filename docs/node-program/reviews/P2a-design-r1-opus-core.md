# P2a outbound design: review r1 (Opus, Core-equivalence and indistinguishability)

| | |
|---|---|
| Design | `designs/P2a-outbound.md` (reconciled r0, 2026-10-08), with DEC-45 and DEC-48 |
| Reviewer | Opus, reviewer 1 of 2 (GPT is unavailable, DEC-42). Read-only. |
| Pins checked | v23.1.8 = `728f505583`, v24.0.0-rc.3 = `0364ed626e`. I extracted the sources myself with `git archive` to `/work/scratch/p2a-rev-a/<tag>/src` and confirmed they are byte-identical to `/work/scratch/p2a-outbound-opus/{v23,v24}` for net.cpp, net_processing.cpp and addrman.cpp. |
| Scratch | `/work/scratch/p2a-rev-a/` contains `addrman/` (bucket and ChaCha20 recomputation scripts) and `cmdtable/` (per-command handler splits at both pins, `notes.txt`). |
| Verdict | **APPROVE-WITH-CHANGES**: no blockers, 6 majors. Every major is a fix to the specification text or the test plan; none needs the architecture redone. |

## 0. Summary

The reconciled design is very good. I re-derived the handshake (§3.3.4–§3.3.7), the gates (§3.2.1), the actor loop and two-queue poller (§3.1), ping (§3.5), addr relay (§3.6), the ConnMan ladder and selection loop (§3.8.2), the seeds (§3.8.4, §3.8.6), linger and inactivity (§3.8.7), eviction and the stale tip (§3.9), misbehaviour (§3.10) and the AddrMan core (§3.7) from Core at both pins, and almost every byte, order and constant matches.

All four disputed facts the reconciler resolved are correct (§4 of this review). `headerssync` and presync are absent at both pins, and the design ports nothing from them (§6). The speculative header pre-hash (DEC-48 Q10) cannot change ordering, cross-peer order or who is penalised first. But it hashes eagerly in places where Core hashes nothing, which creates a CPU and latency asymmetry an attacker can trigger without doing any PoW (M-2).

The majors:
- **M-1:** the "full batch" test that decides whether to send more `getheaders` and whether to disconnect for insufficient work is underspecified, and the obvious reading of the text is wrong.
- **M-2:** the pre-hash is eager where Core is lazy.
- **M-3:** it is unspecified (and contradictory) whether `initialize_node` is synchronous. If it isn't, `version` leaves 0–100 ms late.
- **M-4:** the +C probe can't observe anything at M2.
- **M-5:** DEC-45 requires post-read citations, and most §3.11 rows have none.
- **M-6:** `diff_addrman`'s "exact equality" cannot hold after a `peers.dat` load.

## 1. Findings

### Majors

**M-1 (major): "more headers" and `may_have_more_headers` use the limit for the peer's bit 11, not the limit for the message type received.**
- **Where:** P2a-outbound.md:792 (step 3), :799 (step 10), :800 (step 11).
- **What the design says:** step 3 says `GetHeadersLimit` is "chosen by the message type received". Step 10 then says "Count == limit → maybe_send_getheaders" and step 11 says "with a short message", without naming which limit. The natural implementation reuses step 3's limit.
- **What Core does:** step 3 is right (`nCount > GetHeadersLimit(pfrom, msg_type == HEADERS2)`, v23:5247 / v24:5324). But `ProcessHeadersMessage` computes `uses_compressed = UsesCompressedHeaders(peer)` (bit 11 of the services the peer advertised) and uses `nCount == GetHeadersLimit(pfrom, uses_compressed)` both for the follow-up `getheaders` and for `UpdatePeerStateForReceivedHeaders(..., may_have_more_headers)` (v23:3386-3396 / v24:3370-3380).
- **Failure scenario A:** an outbound peer advertises bit 11, the common version is ≥ 70235, and it answers our `getheaders2` with a plain `headers` of 2000 entries. dashd sees 2000 ≠ 8000, so it does not ask for more. In IBD, with that peer's best header below `MinimumChainWork`, it logs `"Disconnecting outbound peer %d -- headers chain has insufficient work"` and disconnects. A port that uses step 3's limit sends `getheaders2` and keeps the peer.
- **Failure scenario B:** a peer without bit 11 sends `headers2` of 8000 at ≥ 70235. That passes the step 3 check. dashd treats it as short (limit 2000), so it doesn't ask for more and may disconnect; the port asks for more.
- **Why it matters:** both differences are deterministic, observable on the wire (an extra `getheaders` or a disconnect), and need no PoW (regtest/testnet header chains).
- **Fix:** step 10/11 must read "`count == GetHeadersLimit(common, UsesCompressedHeaders(peer))`". Add a `quirk_pq_*` entry and `diff_p2p_headers` cases for both mixed combinations.

**M-2 (major): the speculative pre-hash, and the inline path when it is switched off, hash eagerly where Core does not, which gives an attacker a CPU and latency asymmetry for free.**
- **Where:** §3.4.3 step 4 (:793) and §3.4.9 (:830-843).
- **What Core hashes, per message:**
  - In `headers`, decoding hashes nothing (v24:5329-5333).
  - In `headers2`, `Uncompress` hashes the previous header only when the prev-hash flag is set (`block.cpp:122`).
  - The connect check needs no hash (v24:3337).
  - A non-connecting batch of more than 8 headers gets +10 and returns without hashing anything (v24:3339-3347).
  - A non-connecting batch of ≤ 8 hashes `headers.back()`, plus `headers[0]` only when the `getheaders` log fires (v24:3146-3157).
  - Continuity and acceptance hashes come only after the batch connects.
- **What the design does:** rdashd decodes and hashes every header at the read point, either in the `prevalidate` job or inline, before the connect check. The job is also submitted for messages that Core drops without hashing: before `verack` (common version set, gate 5 drops it), `LoadingBlocks`, and oversized counts (the job decodes the whole payload).
- **Failure scenario (fingerprint and DoS):**
  1. A peer sends `headers` containing 2000 arbitrary 80-byte headers with no PoW and an unknown first prev, followed by `ping` in the same segment.
  2. dashd pays about 0 X11 and answers `pong` at once.
  3. rdashd pays 2000 X11, about 60 ms on B1's figure: on the pool with the job on, or on the actor (inline, or the "unfinished → compute inline/join" path, :837). The `pong` is delayed.
  4. The `headers2` equivalent with the prev flags cleared costs Core nothing as well.
- **Why the design's defence does not apply:** this is *higher* latency, which NFR-FP-03 does not allow ("may be lower"), and it is a classifier feature (NFR-FP-02, NFR-PERF-05, PRD R-8). On the DoS side, Core's +10 per message gives 10 such messages per connection, and each one is free for the sender.
- **The happy path also shifts the latency profile:**
  - Core spends 3 X11 per `headers2` header and 2 per `headers` header, so the `headers` → follow-up `getheaders` gap per header is about 1.5:1 between the two formats.
  - With the job, rdashd's gap is about 1:1 and doesn't depend on the compression flags.
  - That ratio stays the same across hardware, so it is a fingerprint feature. §3.4.9 dismisses it as "only latency is lower" without the NFR-FP-02 analysis.
- **Fix:**
  - (a) Make the inline path lazy, hashing exactly where Core does: decode with no hashing, take the prev hashes `headers2` needs, run the connect check, then hash for continuity and acceptance.
  - (b) Submit the job only when the node is successfully connected, the count is ≤ the limit, and `headers[0].prev` is already in CH1's arena (an append-only, lock-free lookup). Otherwise skip it.
  - (c) Say exactly what "join" means (wait for the job, or run in parallel), and never hash twice on the actor.
  - (d) Count the slot's decoded size against `fPauseRecv`. Decoded headers plus hashes are about 3× the compressed payload.
  - (e) Make "on by default" depend on O13b's classifier showing no gain from per-format latency features (or on the timing being normalised), not only on PF0.

**M-3 (major): it is unspecified and self-contradictory how `initialize_node` runs, and one reading delays `version` by 0–100 ms.**
- **Where:** :288, :449, :1011, :1138; §2.2 :117.
- **What the design says:** §2.2 says handle methods "become commands drained at the top of the next pass". `PeerManagerHandle::initialize_node` is such a handle method, and `drain_commands()` sits in the loop (:449). But §3.8.5 and behaviour 3 say it "pushes `version` before the node is visible to the loop".
- **What Core does:** `OpenNetworkConnection` calls `InitializeNode` synchronously on the `opencon` thread. That creates `CNodeState` and `Peer` under `cs_main` and `m_peer_mutex` and pushes `version` at once (v24:net.cpp:3648 → np:1745-1760). The `version` leaves microseconds after the TCP handshake.
- **Failure scenario:** if this is implemented as a drained command, the actor parks for up to 100 ms (§3.1.1) and handles it at the next pass, or later while it is blocked on `ProcessHeaders`. The gap from SYN-ACK to `version` becomes uniform in 0–100 ms, which a passive observer separates from dashd trivially. Inbound peers (P2b) whose first bytes are read before the command drains would have messages queued for a peer that has no `PeerState`.
- **Fix:** make `initialize_node` synchronous on the caller's thread:
  - insert into a shared peer map under one mutex (Core's `m_peer_map` / `m_peer_mutex`);
  - read `best_height` and `our_services` from atomics;
  - push `version` before appending to `NodeTable`.

  Give O13a/O13b a connect→`version` latency feature.

**M-4 (major): the +C probe can't observe anything at M2.**
- **Where:** :1152 (row +C), §6.1 :1330, §6.7.
- **What the design says:** "A sends headers, B immediately `getheaders`; B's reply includes them as dashd's does."
- **What Core does:** `getheaders` is served from the **active chain**: `FindForkInGlobalIndex(locator)` and `ActiveChain().Next(...)` (v24:4809-4815). On mainnet and testnet below `MinimumChainWork` the reply is empty whatever happened (v24:4785-4793). At M2 the active chain is genesis, so headers-only index entries never appear in B's reply, in either binary.
- **Also:** the per-pass shuffle means A's and B's messages queued in the same pass are processed in random order in both binaries. As written, the probe is not deterministic either.
- **Fix:** use an outcome that depends on the block index.
  1. A sends `headers` (valid regtest PoW).
  2. Wait for evidence that A's message has been processed (our follow-up `getheaders` to A, or a `pong` to a `ping` A sent after the headers).
  3. B sends a `headers` with ≤ 8 entries whose first prev is A's last header.

  dashd treats B's batch as connecting (no `getheaders` to B). If A's headers were not yet in the index it would take `HandleFewUnconnectingHeaders` and send B a `getheaders` (v24:3138-3165). Measure the race statistically under the shuffle.

**M-5 (major): DEC-45 requires cited post-read outcomes, and most §3.11 rows (:1096-1126) have none, including rows P2a itself implements as "exact" stubs.**
- **Uncited rows** (Core lines to add in parentheses):
  - `notfound` (v23:5598-5622 / v24:5744-5765)
  - `getheaders`: the empty reply below minimum chain work (v23:4735-4743 / v24:4785-4793)
  - `merkleblock`, `cfilter`, `cfheaders`, `cfcheckpt` (no branch, not handled in the extension dispatch)
  - `filterload`, `filteradd`, `filterclear` (v23:5452-5532 / v24:5529-5609)
  - `spork`, `getsporks`
  - `ssc` (`node/sync_manager.cpp` v23:334-347 / v24:330-343)
  - the `govsync`…`dsq` row
  - the `dsa`…`qsigshare` row
  - `qdata` (v23 `llmq/quorumsman.cpp:510-513` / v24 `llmq/net_quorum.cpp:204-208`)
  - `mnlistdiff`/`qrinfo` (v23:5571, 5595 / v24:5656, 5741)
  - `getmnlistd` (v23:5534-5551 / v24:5611-5636)
  - `getqrinfo` (v23:5575-5591 / v24:5660-5687)
  - `qfcommit`, `qsigrec`, `clsig` (v23:5666-5674 / v24:5794-5819), `isdlock`
  - unknown commands
- **Partly cited:** the `ProcessGetData` outcomes in `getdata` (v23:2889-3110); `cmpctblock` with a known prev; the first row, which cites sections instead of lines.
- **Fix:** add per-row citations at both pins. Mark `OwnerDerives` rows explicitly as uncited-by-design rather than leaving them blank. My verifier's per-command splits are in `/work/scratch/p2a-rev-a/cmdtable/`.

**M-6 (major): `diff_addrman`'s "exact equality after every operation" cannot hold across a `peers.dat` load.**
- **Where:** :1384, :1386 (§6.3), :1270.
- **What Core does:** `Unserialize` assigns ids in file order. Its prune loop then calls `Delete` (`SwapRandom` with the last element) while iterating the `std::unordered_map mapInfo` (v23:386-395 / v24 the same). So `vRandom` after a load depends on libstdc++'s hash-table iteration order.
- **Effect:** after any load that prunes an entry, `GetAddr`'s output (and with it inbound `getaddr` answers and the cache) differs between rdashd (ascending ids) and Core. The stated gate fails, or flakes, as soon as the serialisation-exchange tests feed a pruning file.
- **Fix:** have O1c's `oracle_addrman_canonical` export `vRandom`. After a load, either re-seed the Rust `vRandom` from it or compare modulo id renumbering and `vRandom` order, and compare `GetAddr` results as multisets until the next permuting call. State that this is the only non-reproducible state.

### Minors

- **m-1: `cmpctblock` row (:1105) is wrong for IBD.** In IBD, an unknown prev returns *silently*; the `getheaders` is sent only outside IBD (v23:4994-4997 / v24:5071-5074). The M2 result ("ignored") matches by accident, but P3 inherits the wrong rule.

- **m-2: §3.10.2 (:1066-1082) is incomplete.**
  - Missing scores: `qgetdata` +10, `qdata` +10, `mnlistdiff` +100, `qrinfo` +100. These are P2a stubs marked exact in §3.11.
  - Missing from "disconnects without a score":
    - `mempool` without NODE_BLOOM (v23:5356 / v24:5433)
    - `getcf*` with an unsupported filter (v23:3456 / v24:3440)
    - all three filter commands without NODE_BLOOM (v23:5454, 5485, 5517 / v24:5531, 5562, 5594)
  - The filter row (:1110) says only `filterclear` disconnects; all three do.

- **m-3: `qfcommit` (:1122) understates v23.**
  - At v23, a null commitment +100 (`llmq/blockprocessor.cpp:77-81`) and an unknown LLMQ type +100 (`:83-89`) come before the quorum-hash lookup.
  - A genesis quorum hash with a non-zero index scores +100 (`:111-116`).
  - `HasMinedCommitment` is only reached at `:126`.

- **m-4: the version step 6 citation is off (:697).** The inbound `fMasternode` rule is at v23:3971-3983 / v24:4000-4012. The cited range is mostly the self-connection check.

- **m-5: the `getheaders`/`getheaders2` locator carries a version word.** `CBlockLocator` serialises an `int nVersion` taken from the stream version, which is the common version via `CNetMsgMaker` (`primitives/block.h` v23:249-255 / v24:255-261). That word differs by profile against a 70242 peer: 70240 under `V23_1`, 70242 under `V24_0`. Neither §3.3 nor §3.4.4 says so. Add it to `fingerprint.rs`'s contract and the O13a fixture notes. O13a's byte comparison would catch it, but the encoder contract should say it.

- **m-6: the functional-test plan cites tests that don't exist.** These are absent at both pins:
  - `p2p_seednode`
  - `p2p_outbound_eviction` (`p2p_eviction` exists but covers inbound eviction)
  - `feature_anchors_db` (v23 has `feature_anchors.py`)
  - `p2p_headers_sync_with_minchainwork`, a Bitcoin presync test that should be dropped, not "where present"

  Affected lines: :1401, :1406, :1396, :1403. Outbound eviction needs an rdashd-authored FT or `sim` coverage beyond `denialofservice_tests`.

- **m-7: O13a's "frozen mocktime" window depends on when the sync tick's start time was set.**
  - Where: :1332, :23, :78.
  - `SyncManager::ProcessTick` keeps `static int64_t nTimeLastProcess = GetTime()`, set on the first tick (`node/sync_manager.cpp` v24:149).
  - Any `setmocktime`/`bump_mocktime` of ≥ 6 s after that (for example during fixture setup) makes the next tick fire once, and dashd then sends `getsporks` to every fully connected peer inside the window.
  - Fix: the harness should assert mocktime has not moved since node start, or the getsporks mask should apply from the start.

- **m-8: the `peers.dat` error-class order is not specified (:1272).**
  - `DeserializeDB` checks the magic, parses the whole payload, including `CheckAddrman` → `DbInconsistentError` and the compat check → `.bak`, and only then compares the trailing hash (`addrdb.cpp` v24:96-117).
  - So a file with a bad checksum that also fails the consistency check is *silently reset* in Core. A hash-first loader would make it fatal.
  - Fix: state the order.
  - Also state the read-side rules:
    - the format byte is never validated;
    - compat < 32 is fatal;
    - only the high bits of the version word are checked (`DISK_VERSION_IGNORE_MASK` 0x7FFFF);
    - an occupied restore slot falls back to the source bucket;
    - out-of-range bucket indexes are dropped silently.
  - And define the asmap checksum: `SHA256d(CompactSize(nbits) ‖ one byte per bit)` of the decoded `vector<bool>`, not of the file (`netgroup.cpp` v24:11-16).

- **m-9: the `GetChance` table can't represent negative `nAttempts` (:938).** `nAttempts` is read raw from disk and never validated by `CheckAddrman`. `pow(0.66, min(n, 8))` with n < 0 gives values above 1 (and `inf` near `INT_MIN`). A nine-entry table can't represent that. This is reachable only from a crafted local file. Use a glibc-bit-exact `pow` for n < 0, or register it as a quirk, and add the oracle cases.

- **m-10: smaller AddrMan text errors.**
  - :907: v24's `ASMapHealthCheck` *does* call `GetAddr(…, filtered=false)` (v24:net.cpp:5009-5020).
  - PQ-19 (:1227): `peers.dat` dumps don't depend on `vRandom`, because `Serialize` iterates `mapInfo`.
  - :1384: `-checkaddrman=1` draws nothing, since `randrange(1)` uses 0 bits; use ratios 1 and 2–3.
  - :947: the error codes run −1…−21.
  - :1271: the temporary file is `peers.XXXX` (`"%s.%04x"`, `addrdb.cpp` v24:60-61), not `peers.dat.XXXX`.
  - :1013 and :1009: `ConnectNode` aborts on the first invalid resolved address, and the v1 reconnect also carries `m_dest`.

- **m-11: the feeler step (:992 (c)) is ambiguous.**
  - Core: a feeler takes `SelectTriedCollision`. With no collision it takes `Select(new_only=true)`. With a collision whose address is already connected, it runs `Good` and takes `Select(true)`.
  - Non-feelers take `Select(false, preferred_net)` (v24:net.cpp:3178-3200).
  - The "otherwise" in the text can be read as sending a collision-free feeler to `Select(false, …)`.

- **m-12: row 25 (:1149) and PQ-12 (:1220) leave out regtest churn.** With accounting skipped, regtest opens up to 11 full-relay peers. `GetExtraFullOutboundCount()` is computed separately in `net.cpp` and still counts them, so `EvictExtraOutboundPeers` disconnects one non-protected extra every 45 s once it has been connected > 30 s (localhost peers share a network, so they aren't protected as "only on network"). The soak and O13a expectations should model this churn.

### Nits

- **n-1 (:1017, :1487):** the fixed-seed age is half-open, `(now − 2 w, now − 1 w]` (`rand_uniform_delay` with a negative range).
- **n-2 (:783):** Core scales the timeout as `1000 µs × secs / spacing`, multiplying before the integer division. `1 ms × (secs / spacing)` truncates differently, by less than 1 ms.
- **n-3 (:977):** `StopNodes` dumps `peers.dat` *before* `anchors.dat` (v24:net.cpp:4190-4199).
- **n-4 (:1159, new-6):** the "next pass" `getaddr` reply holds only if no earlier `addr`/`addrv2` from that inbound peer enabled relay. Otherwise `next_addr_send` is already Exp(30 s) out.
- **n-5 (:993):** `count_failures` is true on regtest when `-maxconnections ≤ 1`, because `min(n − 1, 2) = 0`.
- **n-6 (:1086):** `"peer=%d: %s"` logs on every non-punishing exit of `MaybePunishNodeForBlock` (v24:2013-2015), not only for the three results listed.
- **n-7 (:1111):** the `spork` +100 has no message at v23 and `"invalid spork received. peer=%d"` at v24 (v24:5703). This is a log-only pin difference that isn't marked.
- **n-8 (:621):** the CoinJoin wallet manager exists in every wallet-enabled non-masternode build, whether or not a wallet is loaded (v24:init.cpp:2184-2191). The M2 outcome is the same.

## 2. Observable-behaviour table (§3.12) and handshake

I checked each row against Core at both pins.

| Row | Result |
|---|---|
| 1 `version` | OK. The 137-byte layout, `your_services = addr.nServices` with `addr_you = CService()` when not routable, proxy or v1-compatible, zero `addr_from`, unadjusted mockable time, the nonce from `RANDOMIZER_ID_LOCALHOSTNONCE`, `-pushversion` non-mainnet only and advertised only (common uses `PROTOCOL_VERSION`), `tx_relay = !RejectIncomingTxs`, the challenge, and `fMasternode` set before `InitializeNode` (v24:np:1561-1593; net.cpp:3631-3649). `PushNodeVersion` is identical across pins apart from `Params()`. |
| 2 services | OK. 0x0C00 always, plus P2P_V2 (default on at both pins), plus BLOOM, plus NETWORK after index load (init.cpp v23:1077/1188/1387/2314; v24:1125/1236/1371/2354). |
| 3 who speaks first | OK on the Core side. See **M-3** for the timing. |
| 4 after the peer's `version` | OK: `sendaddrv2` (≥ 70223), [`sendtxrcncl`, gated at ≥ 70235 and off], `verack`, `getaddr` for every non-inbound connection except block-relay-only, including feelers and addr-fetch (v24:4051-4132). The feeler disconnect comes after `AddTimeData`. |
| 5 after `verack` | OK: `mnauth`, `sendheaders2`/`sendheaders` by `m_their_services` bit 11, `sendcmpct(0,1)` iff `CanRelay`, and `senddsq(1)` with `qwatch` inside `!RejectIncomingTxs` (v24:4187-4259). |
| 6 gates | OK: the order and texts of gates 0–10 (v24:3935-4386). |
| 7 ping and getheaders policy | OK: `MaybeSendPing` (v24:6148-6184; first ping at once because `m_ping_start` is 0; the 20-minute timeout only after `peertimeout`), `MaybeSendGetHeaders` on `NodeClock` at both pins (v23:3195-3211), the sync-start rule and its P2P-16 clock (v23:6225/6246 vs v24:6370/6391), and the inv trigger. The full-batch rule is not right: **M-1**. |
| 11, 12, 13, 14 | OK, including `notfound` 116/50,000 and the inv-34 `"tx"` class at v24 (`protocol.cpp` v24:305). |
| 15 addr relay | OK: the token bucket (v24:4416-4424), the per-address order with the token taken before the filters, `RelayAddress` (v24:2686-2734), the `PushAddress` overwrite, `MaybeSendAddr` timers, the `getaddr` cache (net.cpp v24:4265-4307). |
| 21 headers2 modes | OK (`block.cpp:57-134`; the reply leaves the first header uncompressed and doesn't record its version). |
| 25 connections | OK, apart from **m-12** (regtest churn): the ladder and selection loop (net.cpp v24:2947-3291), DNS seeding (2680-2794: `x1.`, 32, 3 to 7 days, `IsFullOutboundConn` count), fixed seeds, `-connect` cadence, `ProcessAddrFetch` try-grant. |
| 26 inactivity | OK (v24:2293-2331). |
| 27 devnet | OK. |
| +C | Ordering in Core is as stated, but the probe is vacuous: **M-4**. |
| +D, new-1 | OK (`ThreadMessageHandler` v24:3676-3721; `PollMessage` v24:4807-4850). |
| new-2, new-3 | Consistent with the code (linger at net.cpp:2122-2148; `SHUT_WR` only if the queue drains inside the 100 ms window). O13b owns the exact timing. |
| new-4, -5, -7, -8, -9, -10, -11 | OK. For new-11, the `≥ 30 s` / `> 30 s` asymmetry and the youngest-scan quirk are confirmed (v24:6012-6110). |
| new-6 | Needs the qualification in **n-4**. |

## 3. Speculative header pre-hashing (DEC-48 Q10)

- **Which peer is penalised first, and order across peers:** unchanged. Every score, disconnect and `ProcessHeaders` call stays on the actor in shuffle order. Enqueueing at read completion keeps the `PollMessage` order, the `fPauseRecv` computation and `recv_time` (v24:4773-4850). Rules 1–3 hold by construction.
- **Kill switch on or off:** the wire outcomes are identical. Decoding is not version-dependent for these two types, and the count check stays on the actor before the slot is consulted.
- **CPU-time side channel:** this one is real. The design front-loads hashing, both in the job and inline, before Core's connect check. That makes rdashd slower than Core on header paths that need no PoW, and it removes Core's 3:2 per-format cost ratio. See **M-2** for the fixes. The slot's memory sits outside the receive-flood budget (part of M-2).

## 4. The four disagreements the reconciler resolved

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 12 | Regtest and devnet skip the whole outbound-accounting block | **Correct** | `if (!Params().AllowMultipleAddressesFromGroup())` wraps every counter and the netgroup set (net.cpp v23:3034 / v24:3028). The flag is true for devnet and regtest (`chainparams.cpp` v23:658,857 / v24:689,895). With every counter at 0 the ladder always picks anchors or full-relay. `count_failures` is false (except n-5). |
| 13 | `ConnectNode` dials the last resolved address | **Correct** | The loop overwrites `addrConnect` for every shuffled result, aborting if any is already connected (net.cpp v24:404-423, identical at v23). |
| 14 | Fixed-seed ages U[1 w, 2 w] ago | **Correct** (half-open, n-1) | `rand_uniform_delay(Now() - one_week, -one_week)` (net.cpp v23:220 / v24:221), with services `GetDesirableServiceFlags(NODE_NONE)` = NODE_NETWORK. |
| 15 | The devnet `Misbehaving` message is v24-only | **Correct** | v23:4008,4010 call `Misbehaving(pfrom.GetId(), 100)` and `(…, 1)` with no message. v24:4037,4039 pass `"wrong-devnet-name"`. The score and disconnect are the same at both pins. |

## 5. The CommandPolicy table

All 74 commands are verified at both pins (`protocol.cpp:99-176`; the only pin difference is the v24 `MSG_ASSET_UNLOCK` → `"tx"` line). The blocks-only set (26) and the quorum-priority set (8) are exact.

About 60 post-read claims were sampled at both pins, well over the 20 asked for. They hold, apart from m-1 (`cmpctblock`), m-2 (filter rows and §3.10.2 gaps), m-3 (`qfcommit` at v23), m-4 (one citation off) and nits n-6 to n-8. Rows confirmed include:
- `version` steps 2–20 and `verack`;
- `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2`, `sendtxrcncl` (off path);
- `senddsq` (NONE/ALL/INV, 70234) and `qsendrecsigs`;
- `ping`, `pong`, `addr` (+20 at v23:4363 / v24:4408), and `getaddr` ignored on outbound;
- `inv`: 50,000 +20, unknown type, the `MSG_ISDLOCK` ≤ 70223 rule, `MSG_SPORK` only in IBD;
- `getdata` 50,000 +20 and `notfound` 116/50,000;
- `headers` limits, the "headers message size" text for `headers2`, unconnecting +20 every 10th and +10 above 8, non-continuous +20;
- `getheaders` locator above 101 disconnects, `getblocks`, `mempool`, `tx`/`dstx` returning before the read in IBD, `block` mutated +100, `blocktxn`, `getblocktxn`, `getcf*`;
- spork hard-coding on mainnet, `getsporks`, `ssc`, the silent overlay rows;
- the five DKG commands +10 (v23:5650-5659 / v24 `net_dkg.cpp:938-944`), the `qgetdata`/`qdata` +10 texts, the `mnlistdiff`/`qrinfo` +100 texts, `clsig`, `isdlock`;
- `MaybePunishNodeForBlock` (v23:1992-2040 / v24:1969-2016);
- the `Misbehaving` and `MaybeDiscourageAndDisconnect` order.

The §3.2.6 dispatch order a–k and the init registration order are right, and no command has two handlers at either pin. The citation gap is **M-5**.

## 6. AddrMan, RNG, `peers.dat`; headerssync

- **Bucket math:** correct as written. CompactSize prefixes are hashed, the port is big-endian and enters only the tried bucket and the positions, IPv4 is mapped as `::ffff:`, and `GetCheapHash` is `ReadLE64(SHA256d)`. The known answers 40, 786, 160, 236 and 795 were recomputed independently and match `addrman_tests.cpp` at both pins.
- **AddrMan constants and operations:** correct at both cited line ranges.
- **The only `addrman.cpp` differences between the pins** are the `filtered` flag and `GetEntries`.
- **RNG:** `FastRandomContext`, ChaCha20 (RFC 8439, 96-bit nonce) and `HashWriter` are identical across the pins. The first deterministic `rand64` is `0x903df1a0ade0b876`, recomputed. `randrange` uses `bit_width` and draws nothing for `randrange(1)`.
- **`peers.dat` write format:** correct: format 4, compat 36, `1024 ^ 2^30`, the `0x20035B60` version word, magic ‖ payload ‖ SHA256d. The load side needs m-8, M-6, m-9 and m-10.
- **headerssync:** there is no `src/headerssync.*` at either pin, and no `HeadersSyncState`, presync or redownload logic in `net_processing.{cpp,h}` or `validation.{cpp,h}`. The design ports none of it. The only leftover is the `p2p_headers_sync_with_minchainwork (where present)` FT entry, which should be removed (m-6). The minimum-chain-work disconnect the design keeps is Core's pre-presync `UpdatePeerStateForReceivedHeaders` rule (v24:3290-3306), which is correct.

## 7. Verdict

**APPROVE-WITH-CHANGES.** Fix M-1 to M-6 in the design text and test plan before phase C/E implementation starts. The minors can be fixed in the same edit. No re-review is needed beyond checking these items.
