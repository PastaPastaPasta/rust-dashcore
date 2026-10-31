# P2a-net round 3 — Sol review

Reviewed `node/p2a-net` at `6899e6183` in a detached checkout at `/work/scratch/p2a-net-sol-r3/src`, concentrating on `3576187be..6899e6183`. Read `REVIEW-COMMON.md` and round 2 first. Reference: Dash Core **tag** `v24.0.0-rc.3`. The P2a-core session seam at `a8ae94adb` is a separate, unmerged leg; I inspected its API but did not treat it as part of this candidate. No candidate-branch edits, commits, pushes, or GitHub actions. The temporary audit instrumentation in the detached checkout was restored before handoff.

## Finding

### R3-M1 — major: the generator sends address work to peers Core has already removed, and the reachability test hides it

**Location:** `node-l1/src/p2p_addr.rs:504-547,573-596,720-733,747-796,801-824`; `node-l1/tests/p2p_addr.rs:75-126,134-163`; D25 at `docs/node-program/designs/P2a-net-notes.md:31`.

**Scenario:** Core v24's `SendMessages` disconnects an `ADDR_FETCH` peer more than `10 * AVG_ADDRESS_BROADCAST_INTERVAL` (300 seconds) after connection (`net_processing.cpp:6323-6326`), before `MaybeSendPing` and `MaybeSendAddr`. The generator moves its clock by 21–27 hours and by a final five minutes, but `Builder::advance_by` leaves an `ADDR_FETCH` peer `live()` and can queue its `pong` and later `addr`, `addrv2`, or `getaddr` deliveries. A second path chooses a manual or outbound full-relay seeder before checking whether a late `sendaddrv2` disconnected it: `handshake` marks it gone, then `seed_addrman` still sends its `addr`. Feelers likewise receive `sendaddrv2`/`verack` after Core ends them at `version`. These are skipped by the session leg, so they exercise no address handler.

I temporarily logged every delivery for which Core's preceding `peer_outcome` already said `removed`, using the existing 64 deterministic inputs per target, then restored the test file. Among the 192 scripts there were **177 such deliveries**: 80 to `ADDR_FETCH` peers (41 address commands, 39 pongs), 11 seeding `addr` commands to manual peers, 4 `addr` commands to outbound full-relay peers, and 82 handshake commands to feelers. The `ADDR_FETCH` skips occurred in 14/64 `addr`, 15/64 `addrv2`, and 2/64 `getaddr` scripts. The unmodified test still reports `scripts losing a peer 0` because it exempts every `ADDR_FETCH` peer and late-v2 peer, and ignores the result of `session.deliver` (`:99-100,120-126`). Its relay threshold therefore does not prove that all intended address messages reached live peers. This does not erase the 25/22/24 scripts in which Core demonstrably did relay, but it materially weakens D25's liveness and path-coverage claims.

**Fix:** track Core's 300-second `ADDR_FETCH` timeout in the generator, or avoid selecting that peer after a jump; ensure the chosen seeder is still live after handshake, retrying with another live outbound/manual peer where needed; stop scripting messages after an expected feeler disconnect. Make the Core-only reachability test assert successful delivery of every intended address command and distinguish each expected removal reason instead of exempting all `ADDR_FETCH` peers. Re-measure the relay and getaddr thresholds after these repairs.

### R3-m1 — minor: the claimed `MAX_ADDRV2_SIZE` boundary lacks the accepted edge

**Location:** `node-l1/src/p2p_addr.rs:316-349`; D25 at `docs/node-program/designs/P2a-net-notes.md:31`.

**Scenario:** the structured BIP155 generator produces 513 bytes for its over-limit case but never 511 or exactly 512 bytes. Known IDs use 4, 10, 16, or 32 bytes; unknown IDs use at most 39. Thus D25's “lengths around 512” reaches only the rejecting side of Core's `MAX_ADDRV2_SIZE` boundary.

**Fix:** add 511-, 512-, and 513-byte unknown-network entries so the accepted maximum and rejecting next byte are both exercised; keep D25's claim tied to those cases.

## Round 2 disposition and Core parity

R2-M1 is **partly closed**: the three targets now exist, use structured session scripts, are registered in `node-l1`, `node-fuzz/Cargo.toml`, and `targets.toml`, and reach real Core relay paths. The Core-only test observed relay sends in 25, 22, and 24 of 64 scripts for `addr`, `addrv2`, and `getaddr` (threshold 16); `getaddr` had 84 address sends classified as replies. Core's relevant rules match the generator's main choices: `ADDR`/`ADDRV2` intake shuffles, rate limits, timestamps, and relays only fresh routable messages of at most ten entries (`net_processing.cpp:4388-4475`); `GETADDR` answers inbound peers once (`:5389-5424`); `RelayAddress` chooses one or two enabled, compatible peers (`:2686-2734`); `MaybeSendAddr` gates on relay state and timers (`:6182-6252`). The new per-site RNG contexts in `dashd-net/src/relay.rs` match Core's context placement for relay, `PushAddress`, local advertisement and getaddr. The sampled no-loss threshold misses R3-M1.

The four Core drop mechanisms described in D25 are real: outbound peers need the expected block service during IBD (`net_processing.cpp:3963-3967`); the header-sync timeout can drop a sync peer when another preferred peer exists (`:6785-6806`); `ConsiderEviction` checks outbound chain work (`:5945-6004`); and ping timeout uses mock time (`:6143-6177`). The generator's preferred-peer restriction, genesis `inv`, one-message-per-pass sequencing, and pre-jump `pong` address those mechanisms in the sampled cases. D25 omitted the `ADDR_FETCH` timeout and the late-v2 seeder path described above.

D24 accurately names the P2a-core `AddrRelayFactory`/`Session.addr_relay` seam at `a8ae94adb`; it is not present in this candidate. D26's exclusions reflect what O1c's session can currently seed; the onion cache-key and permission-bypass behavior has separate `dashd-net` relay tests. D27's event-order argument is consistent with both legs adding outbound peers before the first send pass and comparing the `version` frame on that pass; it is a simulated pass bound, not a wall-clock latency measurement.

## D14 and certification status

D14 is honest about today's leg. `node-l1/src/p2p_node.rs:654-666` still builds `Seams::default()` without Core's draw stream, masks every outgoing `addr`/`addrv2` (`:377-384`), and omits `m_addrs_to_send`, `m_addr_processed`, and both next-send times from `PeerFields` (`:138-165`). The both-leg test prints `BothAccept` for some inputs and `Divergence(peer state)` for others; neither result certifies relay. The 64-input reachability evidence is Core-only. The P2a-core seam plus P2a-net's relay factory, draw replay, unmasked messages and state projections must land before a differential campaign is meaningful, including `diff_p2p_multi_relay`.

The new registry rows are **not parked mechanically**: `smoke = 30` and `nightly = 1` include them in normal smoke/farm selection (`fuzzctl.py:212-214,940-965,1111-1114`). They remain **uncertifiable**: this branch's `fuzzctl.py` only accepts `min_certify_execs` as a registry key and has no `certify` command yet (FZ1 pending), and the node leg cannot compare relay. The stated floors are arithmetically about half a 3600 CPU-second window at the recorded release rates: 10.0/s → 18,000; 7.6/s → about 13,680, rounded down to 13,500; 5.7/s → about 10,260, rounded down to 10,000. They must be measured again with the instrumented, wired build. Until then, keep these legs out of a required clean smoke/farm gate or label their results as pre-wiring diagnostics.

The reported scratch-wiring outcomes (64/64 multi-relay and addrv2, 63/64 addr, 62/64 getaddr) were not triaged here. Nothing in this range changes AddrMan intake or `GetAddr` selection code. `fe1d343b0` changes relay RNG context consumption, which can affect later draw alignment and reply-cache timing, but there is no direct new AddrMan count rule in this range. The off-by-one `addrs_to_send` observations therefore remain informational, not a finding assigned to this branch.

## Validation

- `cargo test --locked -p dashd-net --all-features`: all unit, fixed-seed, loopback, relay, and runtime tests passed; `sim_dial` passed 10/11 on the loaded-box run, with `connect_only` hitting the known 60-second finalization timeout. Its standalone `--test sim_dial connect_only -- --exact --nocapture` rerun passed in 0.08 seconds.
- `cargo test --locked -p node-l1 --all-features --test p2p_addr -- --nocapture`: 2/2 passed; Core-only reachability totals above. `cargo test --locked -p node-l1 --all-features --lib p2p_addr`: 2/2 passed.
- `cargo clippy --locked -p dashd-net -p node-l1 --all-features --all-targets -- -D warnings`: passed.
- `python3 -I node-fuzz/fuzzctl.py check`: passed (60 targets, 59 smoke legs, 59 farm legs). `git diff --check 3576187be..6899e6183`: clean.
- All Cargo commands used `TMPDIR=/work/scratch/p2a-net-sol-r3/tmp` (0700), `CARGO_BUILD_JOBS=8`, and `nice -n 10`. The audit was run only in the detached scratch checkout and its temporary edit was restored. No fuzz certification campaign was run.

Verdict: CHANGES-REQUIRED
