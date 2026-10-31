# P2a-net BL-203: self-announcement against Core (design note)

Status: proposal for the manager's ruling. No code yet. Branch `node/p2a-net-bl203` from T20
`555794315`. Core references are to the v24 oracle pin `77dbec82427`.

## The gap

The `diff_p2p_{addr,addrv2,getaddr}` sessions start at `T0` = regtest genesis time + 24 h
(`node-l1/src/p2p_addr.rs:59`) on a genesis-only chain. The tip is older than Core's 6 h
`max_tip_age` (`kernel/chainstatemanager_opts.h:18`), so both legs stay in IBD for the whole
script. `MaybeSendAddr`'s self-announcement block (`net_processing.cpp:6203-6221`) never runs, and
`m_next_local_addr_send` compares only as 0 (P2a-net D29).

A second gap stays hidden behind the first. Even out of IBD, Core would announce nothing in
today's scripts. With no local addresses (`mapLocalHost` is empty in a session: nothing calls
`AddLocal`), `GetLocalAddrForPeer` (`net.cpp:252-281`) can only advertise the address the peer
says it sees us at. That needs `IsPeerAddrLocalGood` (`net.cpp:245-250`):
- `fDiscover`;
- a routable peer address;
- a routable `addrMe`;
- `addrMe`'s network reachable.

The sim's `version` payload sends `addrMe` as `::` port 0 (`dashd-p2p/src/sim/mod.rs:645-667`),
which is unroutable. So BL-203 has to change two things: leave IBD, and send a routable `addrMe`.

## 1. Leaving IBD on both legs

**What Core latches on.** `Chainstate::IsInitialBlockDownload` (`validation.cpp:1972-1995`) returns
false once the `m_cached_finished_ibd` latch is set. Each call that finds the latch unset checks:
- not `LoadingBlocks()`;
- a tip exists;
- the tip's chain work is at least `MinimumChainWork()` (0 on regtest, `chainparams.cpp:865`);
- the tip time is no older than `NodeClock::now() - max_tip_age`.

When all of these pass, the call logs "Leaving InitialBlockDownload", stores the latch and returns
false. It stays false even if the tip ages again later. The node's `SimChain` mirrors this
(`dashd-p2p/src/sim/chain.rs:1284-1300`, the `inner.ibd` latch).

**What happens on the way out.** Inside `IsInitialBlockDownload`, nothing but that log line.
Everything else is a caller reading the result. One process-wide state changes only on a new
tip, not on the latch:
- `SetServiceFlagsIBDCache(!fInitialDownload)` runs in `UpdatedBlockTip` (`net_processing.cpp:2287`).
- So a session that leaves IBD without connecting a block keeps
  `g_initial_block_download_completed` false. The session sets it false at start
  (`oracle_session.cpp`, "A previous session may have left IBD"), and no `UpdatedBlockTip` reaches
  PeerManager for fixture blocks.
- `GetDesirableServiceFlags` therefore still asks for `NODE_NETWORK`.
- A real dashd restarted on a recent tip does exactly this until its next block. The node does
  too: `set_ibd_complete` is called only from the `UpdatedBlockTip` equivalent
  (`dashd-p2p/src/processing/peerman.rs:291`).
- No state is skipped, then. Both routes below leave IBD through Core's own latch, so nothing is
  forced.

**What changes with IBD, whatever the route.** `SendMessages` starts headers sync with every peer,
not just the first, when `m_best_header->Time() > NodeClock::now() - max_tip_age`
(`np:6379-6381`). On a genesis-only chain the best header is the tip, so that condition holds
exactly when the tip-age test of `IsInitialBlockDownload` does. A session out of IBD therefore
also sends `getheaders` to every full peer. That is Core's real behaviour for a node on a recent
tip. P2a-core's sync code is compared on it already, and it adds frames to every script of the
new target.

**Route A: the tip inside the window.** A new `T0` within 6 h of genesis (or a fixture block
stamped near `T0`), with default options. Natural, but:
- every generated timestamp moves relative to the three existing targets, so their corpora and
  D30 regressions don't carry over;
- the fan-out condition goes false again for peers that start syncing after `T0` + 6 h, while the
  IBD latch stays false. A script then mixes the two states, depending on its clock steps.

**Route B: `-maxtipage` on both legs (proposed).** `T0` stays at genesis + 24 h, and the session
passes `-maxtipage=<span>` on both legs:
- Core gets it through `oracle_peer_session_new`'s `args`. The chainstate takes `-maxtipage` from
  them as init.cpp does (`oracle.h:1410-1412`; `node/chainstatemanager_args.cpp:35`).
- The node leg sets its validator's `max_tip_age` to the same value.
- `<span>` is a fixed value longer than any script can run (proposed: 30 days), so the tip-age
  term is false at every call and the latch sets at the first `IsInitialBlockDownload` call on
  each leg.

Why B is faithful:
- `-maxtipage` is a Core startup option an operator can set, read by the same function. The
  latch, the log line and the later callers are Core's own.
- The only difference from today's sessions is the tip-age answer, uniform for the whole
  script: out of IBD and fanning out headers sync, as above. The IBD-gated paths in the address
  vocabulary change on both legs together: `MaybeSendAddr` (`np:6204`), the block-download
  condition in `SendMessages` (`np:6833`, with nothing to download) and the low-work outbound
  disconnect check in headers processing (`np:3300`, which needs headers, and the address
  vocabulary sends none).

Any divergence a non-IBD session finds outside address relay goes to P2a-core's row, not here.

## 2. What is compared

All of this runs under the session's draw log: every `GetRand` and `FastRandomContext` on the
calling thread draws from `rng_seed`'s stream (`oracle.h:1356-1372`).

- **The self-announcement frame.** The first pass after a peer's address relay is set up and IBD
  is left. While `m_next_local_addr_send` is 0, the `addr`/`addrv2` message carries
  `CAddress{local, m_our_services, Now}` from `GetLocalAddrForPeer`.
  - Inbound: the full `addrMe`.
  - Outbound: `addrMe`'s IP with `GetListenPort()` (`net.cpp:262-272`; regtest default port, no
    `-port`).
  - It goes out in the same pass, since `m_next_addr_send` starts at 0.
  - The frames are already compared byte for byte by `address_targets_agree`'s machinery.
- **The timer.** `m_next_local_addr_send = GetExponentialRand(now, 24h)` (`np:6220`;
  `random.cpp:822-826`, one `GetRand(1 << 48)`). It is drawn before the same pass's
  `m_next_addr_send` draw, and it is drawn even when no address is advertised. Compared through
  the outcome's `next_local_addr_send_us` (already decoded, `peer_session.rs:123`) and through the
  draw order itself (a misordered draw moves every later value).
- **Re-announcement.** When the timer fires again, `m_addr_known->reset()` (`np:6212-6214`) draws
  the rolling filter's new tweak (`GetRand<unsigned int>`), and the self-address goes out again
  despite the filter. The node draws the same (`dashd-net/src/relay.rs:275-301`). Reaching it needs
  a script to advance mock time past the first draw (mean 24 h), so the new mode adds clock steps
  of 1–5 days. Under route B those steps cannot re-enter IBD (the latch holds anyway).
- **`AdvertiseLocal`.** It does not exist at the v24 pin (`git grep AdvertiseLocal` finds nothing
  under `src/`). Its job is done by `GetLocalAddrForPeer` inside `MaybeSendAddr`, which this
  compares. Nothing else of BL-203's text is left uncompared.
- **The `addrMe` cases.** The generator draws each peer's `version` `addrMe` from:
  - routable IPv4 and IPv6, either matching or not matching the peer's network;
  - unroutable IPv4 and IPv6;
  - `::` port 0 (today's);
  - an unreachable network (onion via `addrv2` is out of `version`'s v1 encoding, so only the
    classes v1 can carry).

  Each peer is inbound or outbound and full-relay or block-relay. Block-only peers have no
  address relay, so they announce nothing and are a negative control.
- **Not compared (unchanged from D26):** the `mapLocalHost` branch of `GetLocal`, meaning local
  addresses from `-externalip`, `-discover` and binds, their reachability and score ranking, and
  the `randbits(1|3)` draw in `GetLocalAddrForPeer`. That draw happens only when a local address
  is routable, so with an empty `mapLocalHost` the `||` short-circuits and both legs draw nothing;
  the node short-circuits the same way (`dash-p2p/src/local.rs:320`). Covering it needs local
  addresses seeded into Core's session (section 3).

## 3. Oracle ABI

**No ABI change for BL-203 as proposed.**
- Route B uses the existing `args` of `oracle_peer_session_new`; `SessionConfig` gains a
  `max_tip_age` that dash-oracle-sys turns into `-maxtipage=…`, as it does `min_chain_work` today.
  That is a Rust-only change.
- `addrMe` is in the `version` bytes both legs receive, so it is a sim/generator change in
  `dashd-p2p::sim::PeerVersionMsg` (a new `addr_recv` field) and node-l1.

The pin stays `77dbec82427` (ABI 28).

**If you also want the `mapLocalHost` branch** (not proposed now; D26 left it out), it needs a
shim entry point:

    oracle_peer_session_add_local(oracle_peer_session*, const uint8_t* service, size_t len, int score)

That would be Core's `AddLocal(CService, nScore)` on the session. Core's `mapLocalHost` is
process-wide, so free would have to clear it, plus a matching `NetCore` seeding on the node leg.
On the number: DEC-238's 10-17 amendment says owners don't pick numbers and the manager assigns
the next one above max(installed, reserved). Installed is 28, so it would be ≥ 29. I would ask
you for it rather than take it myself.

## 4. Fuzz plan

**A new target, `diff_p2p_selfaddr`** (owner dashd-net, kind l1, pin v24.0, route private),
rather than a mode byte in the three existing targets. Their farmed corpora, D31 floors and D30
regressions stay valid as they are, and a corpus that leaves IBD stays separate from one that
doesn't.
- It reuses `p2p_addr`'s builder with a `Live` flavour: route B's `-maxtipage` on both legs, the
  drawn `addrMe` per peer, and the long clock steps.
- Its vocabulary is `addr`'s (relay, seeding, `getaddr`, `sendaddrv2`), so self-announcements mix
  with relayed addresses in the same queue, `MAX_ADDR_TO_SEND` trimming included.
- It is registered next to the others in `node-l1/src/lib.rs` and `node-fuzz/targets.toml`.

**Witness tests (gate, unskipped)** over `inputs(64)`, in `node-l1/tests/p2p_addr.rs`:
- `selfaddr_targets_agree`: both legs agree on every frame, queue and draw. It also carries the
  D30 remap and any regressions this work finds.
- `scripts_reach_core_self_announce`: Core self-advertises in at least 16 of 64 scripts, re-advertises after a filter reset in at least 4 of 64, and in an IBD control (the same scripts without `-maxtipage`) announces nothing and keeps `next_local_addr_send_us` at 0. That is what keeps certify honest about path exercise (the D31 lesson).
- The exact thresholds get set from a measured 64-input sweep and written down with the measurement.
- **Mutants that must fail:**
  - drop `!in_ibd` from the node's `maybe_send_addr`;
  - draw the local timer after `m_next_addr_send`;
  - skip the filter reset;
  - use `addrMe`'s port on an outbound peer.

**Seeds.** The 64 deterministic inputs, through the existing seed path. The three address legs'
farmed corpora may be merged in inside the private tree, since bytes map to scripts the same way
plus the new fields. Nothing leaves the private tree; findings get verdict-only reports and
replays through run.sh `--job`.

**Certification**, as the three address legs did:
- Start parked (smoke 0, nightly 0) until one short leg measures the rate. Then un-park with
  smoke 30 and nightly 1, and set `min_certify_execs` to half a measured 3,600 CPU-s window at the
  farm rate (D31/D35 method).
- Farm under DEC-184 at CPUWeight 30, with the target TMPDIR on the per-leg tmpfs. That is
  BL-237's run.sh option if it has landed, otherwise the D37/D38 campaign wrappers from a clean
  campaign worktree.
- Certify once under DEC-186.
- If the window ends `growing` or `failed`, DEC-252 applies: 0 divergences, 0 crashes, 0 sink
  failures and the floor met make it recorded uncertified with its totals. No further nights.
  Rework goes to BL-216's class.

**Size.** About S/M:
- `SessionConfig::max_tip_age`, `PeerVersionMsg::addr_recv`, the `Live` builder flavour, the new
  target, the two witness tests, the mutants and D-rows.
- Then one farm and certify round, about 1.5 CPU-h of farm plus one window.
- No Core change and no re-pin.
