# P2a-net NodeAddressRelay wiring: Opus-h round 1

**Verdict: ACCEPT WITH NITS.** I reviewed `node/p2a-net-wire` at `e9ef589a9` against base `a5d2a9c59`, in a detached worktree with my own target. The Core reference is Dash Core `v24.0.0-rc.3` (`0364ed626e`); Core line numbers below are read from that tag. No branch file was edited.

The head is still `e9ef589a9`. The owner's doc-only commit for Sol's three Lows has not landed yet, so there was no delta to check, and those three Lows are still open (see O4).

The wiring (D29), the fc00 remap (D30) and the un-parking (D31/D32) are correct. D33's clock advance is sound for the schedule it fixes. But it makes `sim_dial` and `connman_sched` blind to one production close bug that the old harness caught and nothing else catches (O1). Its own code path also never runs deterministically in the gate (O2). Both are cheap to fix and neither blocks the merge.

## Findings

### O1 — Low: D33 hides a production close that lingers after a far-side EOF; mutant M2 survives every suite

- **Where:** `dashd-net/src/sim.rs:238-243` (advance to `linger_until` whenever one is set). The production code the mutant breaks is `dash-p2p/src/netcore.rs:1572-1578` (the reader's `Ok(0)` → `close_socket(true)`).
- **Core:** `src/net.cpp:2619-2627`. A graceful close sets `fOtherSideDisconnected = true; // avoid lingering`, so `DisconnectNodes` (`net.cpp:2121-2145`) removes the node on its next pass with no 100 ms linger.
- **Schedule:**
  1. A live, unmarked peer has flushed its queue (e.g. a V2 outbound whose key went out).
  2. The far side closes; the reader reads 0.
  3. With the bug, the node marks the peer without `other_side_disconnected`.
  4. The sweep sets `linger_until = steady + 100 ms` and removes the peer at the first pass after that. On an idle node that pass can be up to 600 ms later; Core removes it on the next pass.
  5. Meanwhile the grant or inbound slot stays held, and `dial_wait`/`ThreadOpenConnections` see the slot taken. The bytes on the wire don't change, but slot timing does.
- **Why D33 matters here:** before D33 the virtual clock never moved during `close`, so every close under this bug hung to `CLOSE_BUDGET`. The old harness failed 6 `sim_dial` tests plus `sched::scripted_runs_keep_the_rules` (mutant run A). With D33, `close` finds `linger_until` set, advances to it, and the peer is removed. The event log matches a correct run (`close` always leaves the clock at `+LINGER`), so the bug is invisible. The full `dash-p2p` suite (226 tests) and the full `dashd-net` suite (42) both pass under M2 (run B).
- **Fix (pick one or both):**
  - Add a `dash-p2p/tests/disconnect.rs` case: "their close of a live, flushed peer removes it on the next pass, never lingering". Attach, settle, `theirs.close()`, then assert `linger_until()` stays `None` and the peer is gone in less than `LINGER` of paused time. This tests the production rule where it lives.
  - Make the harness accept a linger only where one is legitimate. The race D33 handles always leaves the failed send's bytes queued: `send_data` does not `mark_bytes_sent` on an error (`dash-p2p/src/conn.rs:895-901`). A prototype of three lines in `SimExec::close` worked:
    - before `s.close()`: `marked_before = peer.disconnect_requested()`;
    - when `until` is `Some`: `assert!(marked_before || !peer.send_queue_empty(), "…lingers after a far-side close with nothing to send")`.

    It passes the base suites and my probe (run D-base). Under M2 it fails `sched` and 5 `sim_dial` tests (run D-m2), which restores the old detector without the hang.

### O2 — Low: D33's advance-to-linger path has no deterministic test; the gate exercises it only by chance

- **Where:** `dashd-net/src/sim.rs:238-243`; D33 (`docs/node-program/designs/P2a-net-notes.md:39`).
- **Core:** `net.cpp:1752-1759` (a send error sets only `fDisconnect`) and `:2121-2145` (a 100 ms steady linger).
- **Evidence:** I instrumented `close` to print whenever `linger_until` was set, then ran one full `dashd-net --lib` + `sim_dial` pass at load ~200. It printed **0** lingering closes in 20 tests. D33 itself records 3 in about 15 runs under gate conditions. So the code D33 adds almost never runs in tier 1. A regression in it (for example, losing the advance) would come back as the same 60 s flake that DEC-245 waives, not as a clean failure.
- **Schedule for a deterministic test:**
  1. Attach a manual peer whose `MemSocket` send budget is 0.
  2. Push one message, then `core.disconnect(id)`, our side, the way the processor drops a feeler after `version`.
  3. Wait until the sweep sets `linger_until`.
  4. Close the far end; `linger_watch` sees EOF before our `SHUT_WR` and waits for the deadline, as in Core's `only_poll` spin.
  5. Call `DialSim::close(id)`.
- **Result:** my scratch test `probe_lingering_peer_close` (in `/work/scratch/p2a-net-wire-r1-opus/probe_linger.rs`, deleted with the worktree):
  - passes at HEAD, moving the clock exactly 100 ms;
  - fails at 60 s with the pre-D33 `sim.rs`;
  - fails at 60 s under M1;
  - fails at once under M3 with "lingers past 100ms".

  It takes about 0.5 s. I recommend adding it as a `sim` unit test or a `dashd-net/tests` case. It also covers the local-mark linger, which `sim_dial` never reaches because it has no processor.

### O3 — Low: Sol's floors finding. Enough path evidence for the three un-parked targets; none for `diff_p2p_multi_relay`

- **Where:** `node-fuzz/targets.toml:446-450` (multi_relay floor), `:520,532,544`; `node-l1/tests/p2p_addr.rs:33-65,273-300`; D31/D32.
- **Core:** `net_processing.cpp:4460-4463` (relay only for ≤ 10 fresh, routable addresses from a non-`getaddr` source) and `:2686-2734` (`RelayAddress`).
- **addr/addrv2/getaddr:** the two deterministic tests run over the **same** inputs: `inputs()` is seeded by index, so `inputs(313)[..64] == inputs(64)`.
  - `scripts_reach_core_address_paths` proves Core relays in ≥ 16 of those 64 scripts per target and answers `getaddr` in ≥ 32.
  - `address_targets_agree` proves the node agrees on every `addr`/`addrv2` frame, queue and draw on the same 64.

  Together they witness the node's `relay_address` / `MaybeSendAddr` / `cached_addresses` paths on generator output. The generator gives any byte string the same bias, so a certified corpus that never relays would need a generator regression, and that test fails in tier 1. **Recommendation:** no certify-time witness for these three. Change D31 to say that `min_certify_execs` is throughput evidence only, and that path exercise is witnessed by those two tests, which must stay in the gate.
- **multi_relay:** this one is not covered. Its scripts are raw bytes (`node-l1/src/p2p.rs:338-372`, `s.bytes(255)`). For Core to relay, an entry needs a timestamp within 10 min of T0 (about 2⁻²² per random entry) on a routable address from a handshaked peer, plus a relay-enabled second peer. D32 says the same: "random bytes rarely make a timestamp within 10 min of T0". No test asserts that its inputs ever reach `RelayAddress` or a `getaddr` reply. Its 59,000-exec floor certifies throughput on scripts that almost never relay.
  - **Schedule:** the certify window runs 59,000+ executions that handshake and then send `addr` payloads with random timestamps. Core drops every entry at `:4460` or never relays it. The corpus goes flat and the target certifies with relay unexercised, unless libFuzzer's comparison tracing happens to find the freshness bound.
  - **Recommendation:** before multi_relay is un-parked at BS10, give it a witness. Either:
    - a `scripts_reach`-style assertion over its own inputs (likely needing a structured `addr` payload in P2a-core's `session_script` Relay vocabulary, which is P2a-core's call); or
    - a certify-time replay of the final corpus through Core's leg that requires > 0 relayed frames and > 0 `getaddr` replies, recorded in the certificate.

  Add this as a precondition in D32.

### O4 — Info: Sol's three Lows are still open at `e9ef589a9`

`node-l1/src/p2p_node.rs:14` still says "this branch's sessions install [`addr_send_timer`]". D33 (`P2a-net-notes.md:39`) still says only `run_for` moves the steady clock; `wait`'s sleep plans (`sim.rs:272-277`), `connman_sched` op 5 (`sched.rs:222`) and the new `close` also move it. The floors wording is covered by O3. I agree with all three as Lows.

## Scope answers

### 1. Is D33 sound? (schedule level)

The schedule D33 fixes is reachable in Core:

1. The far end closes.
2. Our writer's next `send()` gets an error before our reader's `recv()` returns 0. On TCP this happens when the close sends an RST, or when it races a send that our side already queued.
3. Core sets only `fDisconnect` (`net.cpp:1757-1758`).
4. `DisconnectNodes` stamps `nDisconnectLingerTime = now + 100ms`. The queue is not empty, so there is no `SHUT_WR`, and Core spins on the readable socket until the deadline (`:2121-2145`, and the `only_poll` path).
5. Core removes the node 100 ms later in real time.

dash-p2p reproduces this exactly: the reader's `linger_watch` peeks EOF and `linger_readable` makes the sweep spin to the deadline (`netcore.rs:1627-1672, 1263-1286`). But the sweep's deadline is on the **virtual** steady clock, and nothing in the old `close` moved that clock, so the peer could never leave. Advancing to `until` models the real time that passes in Core during the linger.

The extra `+LINGER` on non-racy closes inserts 100 ms of serial time into the dial loop's timeline. In Core the linger runs in parallel with `ThreadOpenConnections`. A 100 ms delay before the next dial step is still a schedule Core can reach (the OS can delay its thread that long), and `connman_sched` checks only structural rules (grants, ladders, netgroups, duplicates), none of them timing. That buys reproducibility, which a fuzz model target needs.

Can a production bug now pass?

| Bug | Mutant | `sim_dial` / `connman_sched` at HEAD | Other suites | vs pre-D33 |
| --- | --- | --- | --- | --- |
| A lingering peer never expires | M1 | pass (no close lingers on a normal run) | `dash-p2p/tests/disconnect.rs` hangs (killed at 900 s); my probe fails at 60 s | same: the old harness also saw it only on the rare race |
| A far-side EOF lingers | M2 | **pass (masked)** | all pass | **regression**: the old harness failed 7 tests (O1) |
| Linger 1 s | M3 | pass (no linger reached) | `send.rs::linger_until_is_the_sweeps_deadline` fails; my probe fails at once | the D33 assert catches it whenever a linger happens (M4) |
| EOF lingers **and** 1 s | M4 | `sched` + 6 `sim_dial` fail on D33's `until <= end` assert | `send.rs` fails | the assert works |
| No linger | M5 | pass (masked by the fixed advance) | `disconnect.rs` 3 fail, `send.rs` 1 fails | same |

So D33 is sound for the race it fixes. Its one blind spot is a linger where Core has none, and O1's guard or unit test closes it.

### 2. Is the D30 remap confined and complete?

It is confined: the remap lives in `Entry::encode_v2` (`node-l1/src/p2p_addr.rs:292-299`), a private harness encoder. Production crates are untouched; the only non-harness edits are `node-l1`'s dependency on `dashd-net` and the factory.

It is complete for the three targets, because every BIP155 entry they emit goes through `encode_v2`. Checked paths:

- `addr_payload`, including the fresh-repeat rewrite. Its `2a01` rule cannot produce `fc`.
- `relayable_payload`: classes 0 and 2 only, so routable IPv4 or `2a01::/16`.
- `v2_addr`: class 1 goes through `v1_addr`; class 7 is a known id at the wrong length with any first byte. The remap applies there too, and both legs reject the entry anyway.
- `seed_addrman`: v1 IPv4 only.

Truncation only shortens a payload, so it cannot reintroduce a byte. Both legs receive the same `Step::Deliver` payload, so the remap cannot make the node and Core see different bytes.

No other path can carry ID-2 `fc` to the node:

- v1 entries have no network id. Core and base-sdk both read an `fc00` v1 address as IPv6; class 3's `[0xfc,0]` agrees in every run.
- Neither leg can reflect an `fc`/`fd` IPv6 address back out. Unroutable entries never enter AddrMan, and `RelayAddress` is only called for `IsRoutable()` addresses (`net_processing.cpp:4460`).

The remap can turn a random `fc87:d87e:eb43:…` into the TorV2 OnionCat prefix. That lands in class 4, which is already covered and agrees on both legs, so it is a domain shift, not a divergence. The coverage loss (no BIP155 IPv6 in `fc00::/8` is ever compared) is documented, and the `base_sdk_still_reads_fc00_as_cjdns` tripwire is correct.

### 3. Does the wiring match Core's RelayAddress/MaybeSendAddr?

Yes. `node_address_relay` (`p2p_addr.rs:975-977`) wraps the `chain` that `p2p_node.rs:840-845` passes: the leg's own `SimChain::validating` over the leg's clock. Every call into the relay passes the leg's `NetCore`. So:

- **Time:** `core.clock().node_now()` is the sim's `VirtualClock` mock time. That is Core's `GetTime()` in `RelayAddress` (`:2702`), `MaybeSendAddr`'s `current_time`, and `GetAddresses`' `current_time` (`net.cpp:4275`).
- **SipHash relay key:** `core.deterministic_randomizer(RANDOMIZER_ID_ADDRESS_RELAY).write_u64(hash_addr).write_u64(time_addr)` matches `:2705-2707`. The leg's seeds are Core's `nSeed0/nSeed1`, drawn from the replayed `GetRandBytes` stream (`cfg.deterministic = false` + `ReplayStream`, P2a-core D60). The draw logs are compared every phase, so a wrong key or a wrong draw shows up as a divergence.
- **getaddr cache key:** `ConnectedThroughNetwork()` (`network()`, which includes the inbound-onion class), `addrBind` bytes, and the inbound-only port match `net.cpp:4267-4274`. The 21 h + `GetRandMillis(6 h)` expiry matches `net.cpp:4304`.
- **IBD:** `in_ibd` reads the leg's chain at the leg's mock time, which is the point of the factory signature.

### 4. Floors

See O3: the existing tests are enough for addr, addrv2 and getaddr; multi_relay needs a witness before it is un-parked.

### 5. Anything else wire-visible

Nothing else. The multi-session switch adds no address command to the handshake or headers vocabularies, and the relay's timer draws were already compared under the stand-in. One observation, not a finding: D31 notes that multi_relay's rate doubled (17.3 → 33.0 execs/CPU-s) without explanation, across a 7-train gap (`809251543` → `502478334`). A higher floor only makes certification stricter, but check the out-of-domain share before un-parking, so the rise doesn't turn out to be more inputs rejected early.

## Mutants

All mutants were applied to `dash-p2p/src/netcore.rs` (and, where noted, the base `sim.rs`) in my worktree, then reverted.

| Id | Change | `dash-p2p` disconnect / send | `dashd-net` lib (sched) / sim_dial | probe |
| --- | --- | --- | --- | --- |
| probe | log each `close` that sees `linger_until` | — | pass / pass, **0 lingering closes** | — |
| M1 | `if steady < until` → `if true` (never expire) | **hang** (900 s timeout) / pass | pass / pass | **fail** (60 s) |
| M2 | reader `Ok(0)` → `close_socket(false)` (EOF lingers) | pass / pass; full `dash-p2p` 226 pass | pass / pass; full `dashd-net` 42 pass | pass |
| M2 + pre-D33 `sim.rs` | (run A) | — | **fail** `sched` / **fail** 6 (`not finalized within 60s`) | — |
| M3 | `LINGER` 1 s | pass / **fail** `linger_until_is_the_sweeps_deadline` | pass / pass | **fail** (`lingers past 100ms`) |
| M4 | M2 + M3 | pass / **fail** | **fail** / **fail** 6 (`lingers past 100ms`) | — |
| M5 | `LINGER` 0 | **fail** 3 / **fail** 1 | pass / pass | pass |
| pre-D33 `sim.rs` | (run C) | — | — | **fail** (60 s) |
| O1 guard | 3-line assert in `close` | — | pass / pass (base); **fail** / **fail** 5 (M2) | pass |

## What I ran

All runs used `RUSTC_WRAPPER= CARGO_BUILD_JOBS=12 CARGO_TARGET_DIR=/work/scratch/p2a-net-wire-r1-opus/target`, at a load average of 135–200.

- `cargo test -p dashd-net`: **pass** (41: unit 9, fixed_seeds 2, loopback 8, relay 6, runtime 5, sim_dial 11).
- `cargo test -p node-l1 --features oracle --test p2p_addr`: **3 passed, 1 ignored** (`multi_relay_fc00_agrees`, by design), in 246 s.
- The mutant and probe runs above: `/work/scratch/p2a-net-wire-r1-opus/{mutants,followups}.sh`. All logs are under that directory, which is deleted after this review.
- No baseline failures, so there was nothing to rerun alone. Every failure in the mutant table is caused by a mutant and reproduces deterministically, apart from the hang timings.
- I did not rerun the tier-1 gate or the fuzzctl smoke; Sol's report and the owner's gate cover them.
