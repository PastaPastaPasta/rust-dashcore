# P2a-core, round 2 independent review

**Candidate:** `node/p2a-core-leg` at `a8ae94adb6a892fc768a0ac3fef79cb2a517af8c`, compared with round 1's `fd893ef3c` (10 commits, 14 files). I reviewed a detached checkout at `/work/scratch/p2a-core-sol-r2/src`; the candidate worktree was not changed. **Reference:** Dash Core `v24.0.0-rc.3`, with the pinned oracle hook at `66c4e647` for session draw observations.

## Findings

### R2-n-1 — Nit: D60 calls one strong setup draw an ordinary draw

**Location:** `docs/node-program/designs/P2a-core-notes.md:66` (D60, “37 `GetRandBytes` calls”).

**Failure scenario:** A reviewer reproducing the claimed setup count with the pinned oracle and `rng_seed = 7` sees 36 `Draw::RandBytes` calls and one `Draw::StrongRandBytes` call before the first peer, not 37 ordinary calls. This is documentation arithmetic; `ReplayStream` distinguishes strong from ordinary calls and the nine selected `NetCore::new` twins are ordinary calls. An independent scratch probe also observed two ordinary calls when attaching one inbound peer, as D61 says.

**Core cite:** `random.cpp:634-635` in v24 defines `GetRandBytes` and `GetStrongRandBytes` separately. The pinned oracle's `setup_common.cpp:247-250` includes the sanity check, and `dash-oracle-sys/tests/peer_session.rs:934-941` identifies its `StrongRandBytes` draw.

**Fix:** Say “37 RNG byte calls: 36 `GetRandBytes` and one `GetStrongRandBytes`.” Qualify the later “every Core draw here is `(false, 32)`” as referring to the nine selected setup twins and compared peer/pass draws, since the unfiltered setup log also contains the strong call and ordinary 8-byte calls.

## Checks and round 1 closure

- **R1-B-1 — deferred by manager, not closed.** I did not require a DEC-186 certificate at this head. The six core-gated targets remain registered and enabled for smoke/nightly; `diff_p2p_multi_relay` is parked with `smoke = 0`, `nightly = 0`. D56 records the slow-unit cause and its remaining warm-up limit, and D57 records the large-inventory cap. The fix adds per-phase draw comparison and a timer stand-in without changing the six targets' decoders or their certification floors. Script regression tests pass, but this round did not measure a 3600 CPU-second certification window. The required gated-head certificates remain work after train 10/FZ1 merges.
- **R1-M-1 — closed.** `sim_sync` uses mined regtest headers and `pass_with` to assert both shuffle orders for the connecting-header and block-`inv` probes. `p2p_node` sends the same structured inputs through Core and the node for 16 seeds, requires both orders and reaches the last step below P3's first block request. The observed branches match Core `net_processing.cpp:3322-3359,4531-4545,4582-4608`.
- **R1-M-2 — closed for the core-only gate; relay remains parked for the joint E/F gate.** `AddrRelayFactory` and `Session.addr_relay: Option<_>` have the pinned types and `None` installs `NoAddressRelay`; `Sim::with_chain_seams` builds the relay on the sim chain. `NetConfig::with_rand_source` installs the per-phase `ReplayStream`, whose fallback is `StreamRand::from_seed(seed)`; setup and each step compare draw-call shapes. `addr`/`addrv2`, the address queue, processed count and both send timers are unmasked and projected. The `getaddr` and fresh-address tests prove the send-timer stand-in differs when Core relays, consistent with Core `net_processing.cpp:2716-2734,4388-4482,6186-6252`. The parked target supplies no relay certification; P2a-net must install its relay and re-enable it at the joint gate. D60 discloses the remaining same-shape draw-order blind spot.
- **R1-m-1 — closed.** The Core-vs-node session compares whole passes with pending ping, address timer, trickle timer and headers timeout/restart work. The sim pass checks announcement and shared block/object `getdata` ordering with later-task seams. Together they exercise the section sequence of Core `net_processing.cpp:6300-6898`; no queue-empty fast path appeared in the fix.
- **R1-m-2 — closed.** The tests pin literal 999/1000/1001 counts, including a block-plus-object batch. In the detached checkout, mutating `MAX_GETDATA_SZ` from 1000 to 1001 made `getdata_batches_flush_at_1000_entries` fail at 1001 objects (`[1001]` versus `[1000, 1]`). I restored the file.
- **R1-n-1 — closed.** D6 says five inline deviations and lists five.
- **D60–D64 and regression check.** The pinned oracle setup produced 36 ordinary plus one strong byte draw; attaching an inbound peer produced two ordinary bloom-tweak draws. The two Core constructors are `CNodeState::m_recently_announced_invs` and always-built `Peer::TxRelay::m_tx_inventory_known_filter` (`net_processing.cpp:545-559,303-315,441-446,1745-1758`). Section 8's timer condition, shared inbound timer and outbound interval agree with Core `net_processing.cpp:1260-1268,6592-6609`. D63's two probes and D64's section tests passed. The fix range contains no unrelated source change or whitespace error.

**Validation:** `cargo test --locked -p dashd-p2p --all-features` passed (20 unit; integration groups 11, 38 and 35). `cargo test --locked -p node-l1 --all-features --test p2p_node` passed (13); the additional `--test p2p` multi-target replay passed (1). `cargo clippy --locked -p dashd-p2p -p node-l1 --all-targets --all-features -- -D warnings`, `python3 -I node-fuzz/fuzzctl.py check` (56 targets, 54 smoke and farm legs), and `git diff --check fd893ef3c..a8ae94adb` passed. The temporary draw-count probe and mutant were removed; the detached checkout was clean before deletion. All cargo commands used `CARGO_BUILD_JOBS=8 nice -n 10` and a private disk `TMPDIR` under the review scratch directory.

APPROVE-WITH-NITS
