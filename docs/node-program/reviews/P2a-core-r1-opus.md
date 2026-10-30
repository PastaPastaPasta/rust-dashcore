# P2a-core — Opus high review, round 1

- Candidate: `node/p2a-core-leg` @ `a8ae94adb`, base `cc940c818` (integration).
- Scope: `dashd-p2p/`, `node-l1/src/p2p*.rs`, `node-l1/tests/p2p*.rs`, node-fuzz registrations, `docs/node-program/designs/P2a-core-notes.md`.
- Reference: Dash Core v24.0.0-rc.3. `o:` = the oracle tree `~/workspace/dash-wt/o1c-v24.0/src` (the notes' citation base); `tag:` = the pristine tag (`ProcessMessage` runs 11 lines behind the oracle).
- Not re-raised: R1-B-1 (DEC-186 certification, deferred), `diff_p2p_multi_relay` (parked), and Sol r1/r2 items already closed. The D60 doc nit from Sol r2 is still open in the candidate and is not repeated here.

## Findings

| # | Severity | Summary |
|---|---|---|
| F2 | Medium | `BlockSync` seam narrowed from the design (no `PeerState`/Cx, no `block_checked`); no D-row |
| F10 | Medium | 17/28 Core-boundary mutants survive; no test for the `pause_send` backpressure path or `finalize`'s bookkeeping |
| F1 | Low | the lock contracts (relay map→state, stats state→chain, D24 chain→map) admit a cycle; the real chain's lock-free reads are all that prevent it |
| F3 | Low | `ssc` labelled Exact, but Core reads the payload at M2 (logs differ) |
| F4 | Low | `process_headers` mirror has no shutdown outcome; D23's `debug_assert` would panic on a host `WaitError` |
| F5–F9 | Nit | `np:` line bases; stub comment reasons; D6 wording for getheaders/clsig/isdlock; `Connected` uncompared; production RNG unpinned |

Numbering is in discovery order.

### F1 — Low — the written lock contracts admit a three-thread cycle; only the real chain's lock-free reads prevent it

Location: `dashd-p2p/src/processing/seams.rs:369-373` (`AddressRelay::relay` "may lock any other peer's state in `peers`"), `seams.rs:111-112` (`ChainAccess` reads are "lock-free or a short read"), `dashd-p2p/src/processing/stats.rs:38-44` (`node_state_stats` calls `chain.entry` inside `Shared::with_state`, i.e. holding the peer's state lock), `dashd-p2p/src/processing/peerman.rs:95-110` (`Shared::updated_block_tip` locks the `PeerMap`), notes D16 and D24.

Each edge is allowed by what is written:

1. The actor in `relay` (P2a-net's `NodeAddressRelay`, `dashd-net/src/relay.rs:185-216`) holds the `PeerMap` lock and then locks each candidate's `slot.state`. Edge **map → state(X)**.
2. An RPC `getpeerinfo` → `node_state_stats` holds state(X) and calls `ChainAccess::entry`, which the trait allows to be "a short read", i.e. a lock. Edge **state(X) → chain index lock**.
3. D24: the chain side "may hold its index lock" while calling `Shared::updated_block_tip`, which locks the `PeerMap`. Edge **chain index lock → map**.

Scenario: a `ChainAccess` that takes its index lock in `entry`, and calls `updated_block_tip` under the same lock. If an `addr` relay, a `getpeerinfo` and a tip connection coincide, all three threads hang.

Today the cycle cannot close. CH1-node's `ChainViewHandle::entry` is lock-free (seqlocked, `dashd-chainstate/src/chain/view.rs:128-131`), and `UpdatedBlockTip` comes from the queued `ValidationSignals` thread, not from under the index lock. But nothing in P2a-core's contracts requires either. Core has no such cycle because `GetNodeStateStats` and `UpdatedBlockTip` both run under `cs_main`, and `m_peer_mutex` is never held while `cs_main` is taken.

Fix (cheapest first):
- In `node_state_stats`, copy the `BlockId`s out under the state lock and resolve heights after releasing it. This removes edge 2 outright.
- Or tighten the wording: `ChainAccess` reads must never take a lock that the chain holds while calling into the processor. State the lock order in D16 and on `PeerMap` (map → state → leaf cells; a non-actor thread never calls out while holding a state).
- Optionally, have `relay` clone the target `Arc`s and drop the map before locking states.

### F2 — Medium — the `BlockSync` seam lost the design's per-peer context and `block_checked`; no D-row records it

Location: `dashd-p2p/src/processing/seams.rs:140-164` against `docs/node-program/designs/P2a-outbound.md:315-320, 418`.

The design gives every block-sync hook `cx: &mut Cx` ("the peer's PeerState, the node handle, push_message, misbehaving, ChainAccess, addrman, the clock, the RNG", line 418) and a `block_checked(cx, hash, state)` hook. The candidate's `stall_section(peer, now_us)`, `getdata_section(batch, sync_from_peer, now_us)`, `headers_direct_fetch(core, peer, last)` and `push_compact_announcement(core, peer, block)` get only a `PeerId` or `NetCore`/`PeerHandle`; `GetDataBatch` (`seams.rs:180-227`) exposes only `core()` and `peer()`. No hook gets `&mut PeerState`, no `block_checked` exists anywhere in the crate or the notes, and no D-row mentions the change.

Scenario: P3 implements Core's `FindNextBlocksToDownload` in `getdata_section`. Core reads and writes `CNodeState::pindexLastCommonBlock` and reads `pindexBestKnownBlock` (o:`FindNextBlocksToDownload`, and the block-download section of `SendMessages`); the stall section reads and writes `m_stalling_since`. In the candidate `best_known_block` and `last_common_block` live in `PeerState` and are updated by `process_block_availability`/`update_block_availability` in `sync.rs`, so P3 can neither read them nor write `last_common_block`. Mirroring them in P3 needs a hook on every availability update, and none exists. `BlockChecked` → `MaybePunishNodeForBlock` (o:`PeerManagerImpl::BlockChecked`) has no landing place either.

Fix: either restore the design's shape (pass `&mut PeerState`, or a `SyncCx` with the state, the handle and `misbehaving`, to each `BlockSync` hook, and add `block_checked`), or record the narrowing as a D-row with the plan for P3: which `PeerState` fields P3 owns, how it learns availability updates, and where `BlockChecked` lands. The PLAN rows for P3 and CH1-node should name the seam change.

### F3 — Low — `ssc` is labelled Exact but Core reads its payload at M2

Location: `dashd-p2p/src/processing/registry.rs:179` (`exact("ssc", "SP1")`), `dashd-p2p/src/processing/stubs.rs:147-153` (the silent arm and its comment), `dashd-p2p/tests/sim_m2.rs:270`.

Core: `SyncManager::ProcessMessage` returns only when `IsSynced()`. At M2 it isn't synced, so it runs `vRecv >> nItemID >> nCount` (o:`node/sync_manager.cpp:336-342`). SB6's own policy row agrees: `.pre(&[PreRead::MnSyncFinished]).reads(ReadPlan::Whole)` (`dash-p2p/src/policy.rs:588`).

Scenario: the 5-byte `ssc` payload in `sim_m2.rs::silent_commands_neither_score_nor_reply` makes Core throw in the read. `ProcessMessages` logs "ProcessMessages(ssc, 5 bytes): Exception 'DataStream::read(): end of data…' caught" (o:5951-5952). The stub returns `Ok(())`, and the test asserts that. With 8 or more bytes Core logs "SYNCSTATUSCOUNT -- got inventory count…" and the stub logs nothing. The difference is in logs only (no score, no disconnect, no reply), but it breaks the module's contract (decode where Core reads), the Exact label and SB6's pre-read row.

Fix: add an `"ssc"` arm that reads the two `i32`s through `cx.read` and logs. Drop `ssc` from the silent list in that test and add a short-payload test expecting `Err`. Correct the comment.

### F4 — Low — `process_headers` has no shutdown path; D23's `debug_assert` turns a shutdown into a panic in debug builds

Location: `dashd-p2p/src/processing/seams.rs:135` (`fn process_headers(&self, batch, from) -> HeadersResult`), `dashd-p2p/src/processing/sync.rs:211-227`, notes D23 and D59.

D59 says the real `process_headers` returns `Result<_, WaitError>` and that P2a-node will map `WaitError` "to the host's shutdown path". The mirror's signature has no way to say "shutting down". A host newtype can only return a `HeadersResult` with neither `last` nor `invalid`, which hits `debug_assert!(false, …)` at `sync.rs:225`. In a debug build (every test and sim run) that panics, so the thread takes the D19 path and sets `panicked`, and the shutdown is reported as a crash. Core has no equivalent because `ProcessNewBlockHeaders` doesn't wait.

Fix: give the mirror an explicit outcome (`Result<HeadersResult, Interrupted>` or a `HeadersResult::Interrupted`) and have the headers branch return quietly. Keep D23's assert for the real contract break.

### F5 — Nit — `np:` citations use two line bases

The `np:` citations in `registry.rs`, `stubs.rs` and `logic.rs` follow the pristine tag: GETBLOCKS np:4636 (oracle 4647), GETHEADERS np:4760 (oracle 4771), MEMPOOL np:5426 (oracle 5437), "Unknown command" np:5828 (oracle 5839). The notes' header (`P2a-core-notes.md:3`) names the oracle tree as the base, and D61 cites np:454, an oracle line. Pick one base and say so in the header.

### F6 — Nit — `stubs.rs:147-150` gives the wrong reason for several silent rows

The behaviour is right; only the reasons are wrong:
- The sig-share rows are silent because a non-masternode has no shares manager (`llmq/net_signing.cpp:64`), not because the node is unsynced.
- `dsa`/`dsi`/`dss` are silent because the CoinJoin server handler exists only on masternodes (`init.cpp:2253`).
- `dsf`/`dsc`/`dssu` are silent because there is no wallet, so no client manager (`coinjoin/walletman.cpp:234`).
- Only `dsq`, governance, `mnauth` and `platformban` return unsynced before the read.

### F7 — Nit — D6 should say what the `getheaders`/`getheaders2` deviation is and where `clsig`/`isdlock` exactness comes from

- **getheaders/getheaders2:** at M2 Core *replies* (o:4786-4802). On testnet and mainnet (tip work below `nMinimumChainWork`, peer without Download permission) it sends an empty `headers` or `headers2`. The command follows the peer's `NODE_HEADERS_COMPRESSED` bit (`UsesCompressedHeaders`), not the request. On regtest and devnet any non-null locator gets an empty reply of the requested type, and a null locator with `hashStop` = genesis gets `[genesis]`. The row correctly says "deviation → P2b", but P2b needs the services-bit quirk written down.
- **clsig/isdlock "exact off mainnet":** this holds only because no spork is ever stored at M2 (the `spork` row is SP0 and spork `inv` is P5a, both deviations). On mainnet `GetSporkValue` hard-codes 0 for every spork (o:`spork.cpp:173-181`). Note the dependency on the row so that SP0 re-checks it.

### F8 — Nit — the session leg doesn't verify `finalize`'s AddrMan `Connected`

`logic.rs:331-337` ports `FinalizeNode`'s `m_addrman.Connected` condition (successfully connected, score 0, not block-relay, not inbound). The session leg skips the call log, and `p2p_node.rs:15` documents that ("SB6's AddrMan reports no calls"). The oracle's `Call::Connected` exists (`dash-oracle-sys/src/peer_session.rs:212`), so once SB6's AddrMan reports calls, the comparison can be switched on for this row. Also, `Connected` is stamped with the time of the next drain, not the time of `FinalizeNode`. That is harmless (it's within one pass), but worth a word in D12.

### F9 — Nit — the production RNG is not pinned to a non-deterministic source

`FastRandom::lazy` over the core's source matches Core's `FastRandomContext{}` (D60). Nothing stops a host from building `NetCore` with a seeded `StreamRand` outside tests (`NetConfig::with_rand_source` is public, `dash-p2p/src/netcore.rs:249`). A `#[cfg(test)]`-or-feature gate, or a debug log at construction naming the source, would make that mistake visible.

### F10 — Medium — 17 of 28 Core-boundary mutants survive both suites; the backpressure path and `finalize`'s bookkeeping have no test

Each mutant changes one line that I verified as Core-exact. None of the code is wrong, but nothing pins it (full table under Checks). The ones that matter most:

- **`pause_send` backpressure (M13, M24, M25):** dropping `ProcessMessages`' `fPauseSend` return before the poll (`logic.rs:364`), erasing the next getdata entry while paused (`logic.rs:719`), or counting a paused peer as more work (`peerman.rs:347`) all go unnoticed. That is Core's whole send-buffer flow control (o:`ProcessMessages`, `ProcessGetData`, `ThreadMessageHandler` `fMoreWork`). No sim test sets `pause_send`.
- **`finalize` bookkeeping (M4, M22):** if `outbound_with_protect` isn't decremented (`logic.rs:328`, `FinalizeNode`'s `m_outbound_peers_with_protect_from_disconnect`), the counter leaks. After four protected outbound peers disconnect, no peer is ever protected again (`sync.rs:307`). If the `score == 0` gate on `addrman.connected` is dropped, misbehaving peers get `Connected`. Both survive, and the session leg doesn't compare the call log either (F8).
- **Boundaries:**
  - M11: protect when best-known work `>=` tip work (`sync.rs:308`).
  - M17: `ConsiderEviction` catch-up `>=` (`eviction.rs:103`).
  - M18: `nCount <= MAX_BLOCKS_TO_ANNOUNCE` (`sync.rs:198`).
  - M10: unconnecting score on every multiple of 10, not every count ≥ 10.
  - M27: onion protection `<=` (`eviction.rs:190`).
  - M28: last-announce work strictly `>` (`sync.rs:293`).
  - M19: the headers-announce revert on `size() > 1` (`sync.rs:421`).
  - M23: trickle `>=` (`sync.rs:584`).
  - M6: masternode outbound half interval.
  - M15: `UpdatedBlockTip` skips non-relay peers.
  - M12: the pre-sync inv latch only before sync.
  - M21: interrupt check between peers.

Scenario: any later edit to these lines (P3, P5a and P2b all touch `sync.rs` and `logic.rs`) can silently break Core equivalence. The session leg only exercises the few paths its generators reach. M5 and M7 are caught only by node-l1, and the other 17 by nothing.

Fix: add sim tests at the boundaries. Use one peer with `pause_send` forced, plus a getdata batch and a queued message. For `finalize`, disconnect protected and misbehaving peers and assert the counter and the `Connected` calls via a recording AddrMan or the counters. Add equality cases for M11/M17/M18/M27/M28. Use 11 and 20 unconnecting batches for M10, a block-relay peer for M15, and an interrupt raised by the first peer's handler for M21. Re-run the table; the target is no survivors except M23, which is equivalent on the sim clock unless a test hits the exact microsecond.

## Checks

| Check | Result |
|---|---|
| `cargo test --locked -p dashd-p2p --all-features` | pass (105 tests, incl. `sim_m2`, `sim_sync`, `dashd_transcripts`) |
| `cargo test --locked -p node-l1 --all-features --test p2p_node --test p2p` | pass (1 + 13) |
| `cargo clippy --locked -p dashd-p2p -p node-l1 --all-features --all-targets -- -D warnings` | clean |
| `cargo fmt --check` (both crates) | clean |
| `python3 -I node-fuzz/fuzzctl.py check` | ok |

Build: detached checkout at `a8ae94adb`, `CARGO_BUILD_JOBS=8 nice -n 10`, own TMPDIR (mode 700). Box load 35–160. No timing failures in the suites; the only rerun was M7's mutant kill, done alone with and without the mutation.

### Exactness verified line by line against Core (no finding)

- **ThreadMessageHandler port** (`peerman.rs`): per-pass snapshot, `FastRandomContext{}` shuffle (lazy seed, ≥2 nodes), 100 ms masternode throttle, `fMoreWork` excluding `pause_send` peers, interrupt checks before each peer, `wait_while_for(!woken, 100ms)` clearing the flag (`fMsgProcWake`). A wake between the pass and the wait is kept by the flag, so no wake is lost.
- **D19:** `catch_unwind` → interrupt → `panicked`; `stop` joins, then runs the interrupt and stop handlers each under `catch_unwind`. Tested in `sim_m2.rs::a_panic_stops_the_thread_and_still_stops_the_handlers`.
- **D12:** `Shared::finalize` moves the slot out under the map lock and the actor drains it at the start of the next pass. The counters read the final state on the actor thread, so there's no race with an in-flight handler.
- **D16:** the misbehaviour cell is a leaf. `Shared::misbehaving` takes the map briefly, clones the cell's `Arc`, releases the map, then locks the cell.
- **ProcessMessages/ProcessMessage order:** `fDisconnect`, the getdata queue first, `pause_send`, the poll, the message, `fMoreWork`. Exceptions are logged and processing continues.
- **ProcessGetData:** the block-first rule, the `pause_send` break, unknown types erased and counted for `notfound`, the `notfound` limit `MAX_INV_SZ`.
- **SendMessages sections 1–13** and the pre-handshake return. The snapshot is taken after section 5 (D33).
- **headers/headers2:** hash-once, `windows(2)` continuity, unconnecting batches (a `getheaders` for each, +20 whenever the count reaches a multiple of `MAX_UNCONNECTING_HEADERS`), `UpdatePeerStateForReceivedHeaders`, last-announce work.
- **Block-inv getheaders** and the pre-sync latch, `MaybeSendGetHeaders`' throttle (D30), sync start and the timeout math (`HEADERS_DOWNLOAD_TIMEOUT_BASE + PER_HEADER * (now - tip time) / spacing`), the headers-timeout section's "others < 1" rule.
- **Eviction:** `ConsiderEviction`, `EvictExtraOutboundPeers` (block-relay youngest with `MINIMUM_CONNECT_TIME`, outbound worst-announcing with the protect rules), `CheckForStaleTipAndEvictPeers`, `TipMayBeStale` (3 × spacing), `CanDirectFetch`. `core.peers()` is a Vec in insertion (id) order, which matches `ForEachNode`.
- **UpdatedBlockTip** (fully connected, `can_relay`, the reversed `vHashes`), **GetNodeStateStats** fields, **misbehaviour** scores, the threshold (≥ 100) and `MaybeDiscourageAndDisconnect` (take-and-clear, NoBan, manual, local).
- **Trickle timer** (inbound 5 s average, outbound 2 s, masternode outbound half).
- **The 74-row registry** against `allNetMessageTypes` (same order and strings). Every Exact row's M2 outcome was checked against the oracle handler chain (`o:5779-5836` and each `m_handlers` entry). The one mismatch is F3. D6's 15 + 5 deviations match the code, and no deviation is silent.

### Harness soundness (no finding beyond F8)

- **D60 ReplayStream:** a misaligned phase falls back to `StreamRand` and shows up as different nonces and timers rather than hiding. The remaining blind spot (validator m1: two consumers swapping (false, 32) draws) only covers values with no compared effect. Today those are the bloom tweaks (D61, fixed) and the trickle timer values. The trickle timer is covered by the M5/M6 mutants below.
- **D62 SendTimer:** the stand-in can't hide relay behaviour. `next_local_addr_send`, `addrs_to_send`, `getaddr_recvd` and the draw log are all compared.
- **D55** (`check_block_index` after each step), **D56** (the empty input builds the 8001-header chain; `MAX_LONG_RUNS = 2`, `MAX_LARGE_INVENTORIES = 2` for 118..=50000) and **D57:** none of them masks a Core-visible field. `left_out` is limited to object-only getdata (D46), and the stop at Core's first block getdata or INACTIVITY tick (D43/D44) is where P3 begins.

### Interfaces

- **D3/D59 chain mirror:** fine for P2a-node, with F4. The D59 mapping plan is complete apart from the shutdown outcome.
- **AddrRelayFactory/AddressRelay:** works for P2a-net as written (see F1 for the lock-order wording).
- **BlockSync:** F2.
- **ObjectRequests:** sufficient for P5a. It only needs inv/getdata batching and per-peer finalize, and it has both.
- **CH1-node:** `updated_block_tip` arrives from the queued signals thread, and the reads are lock-free, so F1 can't close against the real chain. The contract text should still say so (F1).

### Mutants (scratch copy, reverted after each)

`dashd-p2p` = `cargo test --locked -p dashd-p2p --all-features`. `node-l1` = the `p2p` + `p2p_node` tests, run only on the `dashd-p2p` survivors. I reran M7's kill alone with and without the mutation: it fails mutated and passes unmutated.

| Mutant | Change | dashd-p2p | node-l1 |
|---|---|---|---|
| M1 | masternode throttle off | killed (`masternode_connections_are_throttled_to_100ms`) | — |
| M2 | `wait` keeps the wake flag | killed (`a_wake_is_consumed_by_the_next_wait`) | — |
| M3 | `should_discourage` not cleared | killed | — |
| M4 | `finalize` doesn't decrement `outbound_with_protect` | survived | survived |
| M5 | outbound trickle average 2 → 3 s | survived | killed (`header_chain_and_invalid_headers`) |
| M6 | masternode outbound trickle not halved | survived | survived |
| M7 | headers timeout `others < 1` → `< 0` | survived | killed (`single_command_targets_agree_on_scripts`) |
| M8 | block-relay eviction `>=` → `>` connect time | killed | — |
| M9 | unknown getdata type not erased | killed | — |
| M10 | unconnecting score `% 10 == 0` → `>= 10` | survived | survived |
| M11 | protect `>=` → `>` tip work | survived | survived |
| M12 | inv latch set even after sync started | survived | survived |
| M13 | no `pause_send` return before the poll | survived | survived |
| M14 | D19 `panicked` not set | killed | — |
| M15 | `UpdatedBlockTip` ignores `can_relay` | survived | survived |
| M16 | discouragement threshold `>=` → `>` | killed | — |
| M17 | `ConsiderEviction` catch-up `>=` → `>` | survived | survived |
| M18 | few-unconnecting `<=` → `<` | survived | survived |
| M19 | announce revert `> 1` → `> 2` | survived | survived |
| M20 | stale tip 3 × spacing → 2 × | killed | — |
| M21 | no interrupt check between peers | survived | survived |
| M22 | `addrman.connected` ignores the score | survived | survived |
| M23 | trickle `>=` → `>` | survived | survived |
| M24 | getdata erase ignores `pause_send` | survived | survived |
| M25 | `fMoreWork` counts paused peers | survived | survived |
| M26 | sync timeout `(behind / spacing)` integer order | killed | — |
| M27 | onion protection `<=` → `<` | survived | survived |
| M28 | last-announce work `>` → `>=` | survived | survived |

Totals: 9 killed by `dashd-p2p`, 2 more by `node-l1`, and 17 survive both (F10).

## Verdict

**CHANGES-REQUIRED.**

I found no runtime defect against Core beyond F3, which affects logs only. The actor loop is live (no lost wake, no lock-order inversion with the real chain), and the ProcessMessages, ProcessGetData, SendMessages, headers, eviction and misbehaviour ports are exact.

The round is still needed for two reasons:
- **F2:** the block-sync seam that P3 must build on differs from the design, and no D-row records the change. Either restore `&mut PeerState`/`block_checked` or record the narrowing with P3's plan.
- **F10:** most of the Core-boundary lines and the whole backpressure path are unpinned by tests.

Also fix F3 (one stub arm and a test) in the same round. F1 and F4 are small contract fixes, and F5–F9 are optional nits. A narrow re-check (the F2 decision, F3, the F10 mutant table re-run) should be enough to reach APPROVE.
