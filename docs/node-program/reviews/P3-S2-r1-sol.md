# P3 S2 review — Sol round 1

**Head:** `0e97ada2c` against accepted S1 head `1e16158ef`; Core reference `0364ed626e` (`v24.0.0-rc.3`).  
**Verdict: CHANGES REQUIRED**

## Findings

### F1 — Block download timestamps are sampled before the bookkeeping operation

**Port:** `dashd-p2p/src/processing/logic.rs:997,1037`, `dashd-p2p/src/processing/download.rs:582,320`, and `dashd-p2p/src/processing/blocks.rs:88-103`. **Core:** `src/net_processing.cpp:1310,1324-1351,6773-6779,6825-6829`.

Section 12 passes the `now_us` sampled at the beginning of `send_messages` into `block_requested`. Core samples `GetTime<microseconds>()` inside `BlockRequested`, when it enqueues the first block. Likewise, the Rust `block` handler passes the handler context's time from before `read_block` and `block_mutation` into `remove_block_request`; Core samples the clock inside `RemoveBlockRequest` after its block decoding and mutation check. Section 9 then uses these earlier Rust timestamps as the timeout origin. Its floating-point comparison is otherwise faithful.

Concrete wire-visible schedule, with one downloader and a 150 s block interval: the pass begins at `t=0`, section 6 or the chain walk takes 250 ms, and section 12 requests block A at `t=0.250 s`. Rust records `m_downloading_since=0`; Core records `0.250 s`. On a pass at `t=150.100 s`, Rust disconnects A's peer and Core does not (Core's deadline is `150.250 s`). The same discrepancy occurs when an oldest `block` message begins at `t=10 s` but its decode and mutation check finish at `t=10.200 s`: Rust resets the next block's timeout origin to 10 s; Core resets it to 10.200 s. A later pass between those deadlines disconnects only in Rust. The existing timeout tests advance time only *after* bookkeeping, so they cannot catch this.

Read the clock at the request and removal operations, while retaining the pass-start `now_us` for Core's section 9 stall and timeout checks. Add a test whose clock advances within section 12 and within block decoding.

### N1 — The S2 stall test does not check log order

**Port test:** `dashd-p2p/tests/sim_blocks.rs:351-356`; **port code:** `dashd-p2p/src/processing/download.rs:535-537`; **Core:** `src/net_processing.cpp:6758-6765`.

At the `+3 s` stall, Core logs the disconnect line before raising and logging the timeout. The test asserts only that both lines occur. I moved `raise(seen)` before the disconnect line in a temporary mutation. `s2a_ibd_stalling` still passed, although the visible log order was reversed. The transcript replay compares frames and connection closure, so it does not cover log order either. Assert the positions of the two captured lines in the sim test.

## Fidelity and contract audit

- `FindNextBlocksToDownload` preserves Core's best-known and minimum-work gates, last-common bootstrap and reorg correction, 128-entry chunks, validity stop, downloaded/active-chain advancement, 1024+1 window, first in-flight staller, and the `waitingfor != peer` rule (`download.rs:420-509`; Core :1477-1558). The first requester for a hash is kept in insertion order. The in-flight removal, per-peer count, and outbound-request query match Core (`download.rs:210-328`; Core :1271-1358).
- Stall evaluation uses the strict comparison, checks stall before block timeout, logs before the successful strong CAS, doubles only through 64 s, and decays with truncation toward zero. The atomic is shared with `Shared::block_connected`, which performs no actor wait or nested lock (`download.rs:59-94,355-385,527-545`; `peerman.rs:259-265`; Core :2199-2207, :6752-6781). A racing raise and decay can each win or lose the CAS exactly as in Core. The section 9 floating-point expression and strict `>` are faithful, subject to F1's origin time.
- Direct fetch walks newest to oldest until the active chain or 17 candidates, requests oldest first, caps the peer at 16 in flight, and uses `MSG_BLOCK` at S2 (`download.rs:393-411,589-631`; Core :3202-3258). The lone `MSG_CMPCT_BLOCK` substitution is explicitly assigned to S3.
- D78's defaulted `BlockSync::bind_stalling_timeout` does not change existing S1 implementations; `PeerLogic::new` binds production `BlockDownload` to the signal's atomic once. D84's `status=None` stops the current section 12 walk; after the missing status is published, the next `SendMessages` pass retries. It does not clear best-known or last-common state, so I found no permanent skipped request. The actor's periodic passes provide that retry. `BlockCheckedCx::with_peer` remains a separate actor path and does not acquire the new atomic through a peer lock.
- D81's one-publication `SimChain` activation omits Core's 32-block ABC step boundaries. That limits interleaving coverage but does not change the S2 production scheduler; the current S2 cases observe the per-block signals and final published view. D82's scripted one-node legs exercise the section 12 limited/inbound branches; D83's invalid-connect branch covers the stated source-credit case; D85's two transcripts exercise S2's real `BlockDownload` through `Seams` and `Hooks::downloads_blocks() = true`. These deviations are acceptable for S2, with the ABC steps and valid HB branch still owed by S4 as documented.
- The two fixtures contain only loopback IP metadata and hex message payloads (plus the `closed` marker). I found no filesystem path, private key, or non-loopback IP in them.

## Validation and mutations

All commands used the detached review worktree and its private `/work/scratch/p3-s2-r1-sol/target`. Box load was high; no wall-clock failure was attributed to the implementation.

| Check | Result |
|---|---|
| `cargo test -p dashd-p2p` | **Passed:** 201 tests across all package targets; includes `sim_blocks` and both P3 transcript replays. |
| `cargo clippy -p dashd-p2p --all-targets -- -D warnings` | **Passed.** |
| Stall `<` → `<=` | **Caught** by strict-boundary unit. |
| Doubling cap 64 → 128 | **Caught** by timeout sequence unit. |
| Decay truncation → rounding | **Detected as nontermination:** the unit printed “running for over 60 seconds”; at 3 s, rounding returns 3 and its unbounded `while let Some` loop cannot finish. I stopped that test process by PID. The test would be clearer with a bounded iteration count. |
| Block timeout `>` → `>=` | **Caught** by floating-point boundary unit. |
| Direct-fetch per-peer `>= 16` → `> 16` | **Caught** by `s2e_headers_direct_fetch_requests_msg_block`. |
| Section 12 count 16 → 17 | **Caught** by `s2d_in_flight_cap_and_getdata_batching`. |
| Omit scheduler active-chain check | **Caught** by the pruned active-block unit. |
| Change one hex nibble in a recorded block `getdata` | **Caught** by `blocks_1100` transcript replay. |
| Move timeout raise before stall disconnect log | **Survived** `s2a_ibd_stalling`; N1. |

Every mutation was temporary. The detached Rust worktree was clean after restoration. No live oracle, chain data, or transcript capture was used.
