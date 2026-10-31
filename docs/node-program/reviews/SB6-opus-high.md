# SB6 — Opus high review (`dash-p2p`, whole series)

## Scope and pins

- **Target:** `node/sb6-dash-p2p` at **5516ee242**, reviewed detached at `/work/scratch/sb6-opus/wt`. The series is `418d4e881..5516ee242`, 67 commits. It covers `dash-p2p` (16.6k lines of source and in-crate tests), `dash-clock`, `dash-p2p-transport` `V2Transport::with_rand`, the `node-fuzz` `p2p_decode_{matrix,model}`/`p2p_gate_model` models, CI groups and the SB6 notes (D1–D42).
- **Test bases:**
  - `node/sb6-test-base-r1` at 32dce97f8: ABI 9 pinned, archive 6efa24f2.
  - `node/sb6-test-base-r2` at 14b7578cb: ABI 10 dev archive d694a47, `DASH_ORACLE_ALLOW_UNPINNED=1`.

  Each has its own detached worktree. Their `dash-p2p`, `dash-p2p-transport`, `dash-clock` and `node-fuzz/src/models` are byte-identical to the target (`git diff --stat` is empty).
- **References:**
  - Core **v24.0.0-rc.3**, from the `/work/scratch/p2a-fable/v24.0.0-rc.3/src` extract plus `git show v24.0.0-rc.3:` for the files it lacks.
  - SB0 rev6.1 §3, SB6 notes D1–D42 with the API ledger, DEC-178 and REVIEW-COMMON.
  - Sol r1–r4. Closed items (F1–F4, R3-M1, R3-m1, R3-n1/n2, R4-m1) are not reopened.
- **Method:** breadth-first. Five read-only sub-reviews (wire/handshake, conn/concurrency, dial/eviction, AddrMan/persistence, decode/tests) each read their area against Core and ran targeted mutations in private copies. I verified every finding below that is major or minor against the code (and Core where cited) before keeping it. Several were confirmed independently by two sub-reviews.

## Findings

### Major

#### M1 — `NetCore::discourage(PeerId)` silently does nothing once the sweep has removed the peer, so a misbehaving peer that closes at once escapes Core's discouragement

- **Location:** `dash-p2p/src/netcore.rs:654-657`
  ```rust
  pub fn discourage(&self, peer: PeerId) -> bool {
      let Some(p) = self.peer(peer) else { return false; };
  ```
  `peer()` looks the id up in the live table.
- **Core:**
  - `ThreadMessageHandler` holds a reference on every node in its snapshot. It runs `ProcessMessages` and then `SendMessages` back to back, with no `fDisconnect` test between them (net.cpp:3696-3712).
  - `SendMessages` starts with `MaybeDiscourageAndDisconnect`, which calls `m_banman->Discourage(pnode.addr)` (net_processing.cpp:5833-5870). The `CNode` and the `Peer` outlive `DisconnectNodes`'s removal until the reference count drops (net.cpp:2185-2205).

  So once a message has scored the peer past the threshold, Core always discourages its address, even if the socket thread has meanwhile dropped the node.
- **Failure scenario:**
  1. A peer sends a message worth `Misbehaving(100)` that is slow to process, such as an invalid block or a large bad `headers`, and closes its socket at once.
  2. The reader sees EOF and calls `close_socket(true)`. The sweep removes the peer with no linger (`other_side_disconnected`, `netcore.rs:1205`), while the processor is still inside `process_message`.
  3. The processor then runs `maybe_discourage_and_disconnect`. Both live P2a branches call `core.discourage(state.id)` (`node/p2a-core-leg` 498d42479 and `node/p2a-net` c8bf19b7f, `dashd-p2p/src/processing/misbehavior.rs:65`). The call finds no peer and returns `false`, and the address is never discouraged.
  4. An attacker can reconnect inbound at full slots and stay in the dial and relay pools, which Core denies.

  This is the misbehaviour half of focus item 1. A peer can tell the two apart by how its next connection is treated.
- **Evidence:** sub-review probe `probe_discourage_after_removal`: attach, remote close, wait until `peers()` is empty, then `discourage(id)` prints `returned=false discouraged=false`. I read the processor call site on both P2a branches. No D-row covers it.
- **Fix:** take the handle the processor holds (`discourage(&PeerHandle)`). Apply the noban, manual and `IsLocal` exemptions to it, discourage its address, and run `disconnect_subnet` whether or not the peer is still listed. Do the same for `disconnect` for symmetry. Add a test: remove the peer through a remote close while the handle is held, then `discourage` must still add the address to the filter. This changes an API that P2a-core codes against, so it belongs in SB6 before merge, with an API-ledger row.

#### M2 — the outbound dial loop, the DNS-seed wait and stop logic, and inbound `accept`/`eviction_candidates` have no behavioural test; seven Core-parity mutations survive the whole suite

- **Location:**
  - `dash-p2p/src/dial.rs:458-810`: `tick_opencon`, `maybe_add_fixed_seeds`, `pick_outbound`.
  - `dial.rs:879-982`: `tick_dnsseed`.
  - `inbound.rs:235-270`: `eviction_candidates`.
  - `inbound.rs:294-388`: `accept`.
- **Core:**
  - net.cpp:2917-3292, `ThreadOpenConnections`.
  - net.cpp:2680-2794, `ThreadDNSAddressSeed`.
  - net.cpp:1775-1820, `GetEvictionCandidates`.
  - net.cpp:1875-2010, `CreateNodeFromAcceptedSocket`.
- **Scenario:** "which address is dialled, when, in what order" and "which inbound is evicted from a live table" are the observables the product requirement names, and they are pinned only by review.
  - The in-crate `dial_tick` callers cover only `-connect=0`/`-dnsseed=0` → `Done` and the addcon grant release. Nothing drives `OpenCon` or `DnsSeed` past `Start`.
  - Nothing calls `accept` or `eviction_candidates`.
  - `tests/eviction.rs` and `inbound_core_tests.rs` test only the pure `select_node_to_evict`/`protect_by_ratio` on hand-built candidates, and do that well.

  A regression in the ladder, the 100-try skip rules, fixed-seed timing, DNS stop, masternode eviction protection or the discouraged-near-full refusal would ship unseen. The sub-review found every rule right today; the problem is that nothing holds them there.
- **Evidence:** I confirmed the coverage claim with `grep -rn "dial_tick\|DialLoop::OpenCon\|\.accept(\|eviction_candidates" dash-p2p/{src,tests}`. A single mutant build ran the whole `dash-p2p --all-features` suite, with the same counts as the clean run (lib 105, addr_ping 10, bans 3, eviction 1, handshake 17, rand_source 9, send 10, shutdown 3, sim 1, stdsort 1). **All seven mutants pass** (`/work/scratch/sb6-opus/logs/mut-dial.log`):

  | Mutant | Location | Core rule |
  |---|---|---|
  | netgroup-diversity `continue` deleted | `dial.rs:744-746` | net.cpp:3207 |
  | `tries < 30` → `< 3` in the 10-minute recency skip | `dial.rs:759` | net.cpp:3230 |
  | DNS stop at ≥ 2 peers → ≥ 3 | `dial.rs:951` | net.cpp:2734 |
  | fixed seeds after 60 s → 6 s | `dial.rs:608` | net.cpp:2982 |
  | verified-masternode eviction protection deleted | `inbound.rs:248-250` | net.cpp:1795-1797 |
  | discouraged refused at `inbound+1 >= max` → `inbound >= max` | `inbound.rs:345` | net.cpp:1942 |
  | `is_local` forced false in eviction candidates | `inbound.rs:264` | net.cpp:1814 |

- **Fix:** tests only; no code change is implied.
  - **`tick_opencon`:** drive it against a seeded deterministic AddrMan, with `MockClock` and `MemSocket` peers, and assert the `DialPlan` sequence:
    - the ladder rows in order, with regtest's `allow_multiple_addresses_from_group`;
    - each skip rule of the 100-try loop: netgroup, 10-minute/`tries < 30`, bad port/`tries < 50`, addnode entry, masternode;
    - fixed seeds at `start + 60 s`, and at once under `-dnsseed=0` with no addnode or seednode;
    - `-connect`'s `min(pass, 10) × 500 ms` back-off.
  - **`tick_dnsseed`:** use a fake resolver to cover the 11 s steps, the stop at two handshaken full-relay non-probe peers, batches of 3 and `-forcednsseed`.
  - **`accept`:** cover the inbound limit with eviction, the discouraged threshold, verified masternodes not counted, `refuse_unsynced` after eviction and the implicit whitelist flags.
  - **`eviction_candidates`:** cover `active_masternode`'s protections.

  Each new test must kill the mutant listed above for its rule.

### Minor

#### m1 — CJDNS is half supported once a host makes it reachable: named targets skip `MaybeFlipIPv6toCJDNS`, and a CJDNS address can't be dialled or attempted

- **Location:**
  - `netcore.rs:918-940` (`resolve_dest`) builds `Service::new(r, port)` with no flip. `dial.rs:384-390` (`lookup_numeric`) and `local.rs:204` do flip.
  - `netcore.rs:876` with `netaddr.rs:541-550`: `to_socket_addr()` is `None` for `Network::Cjdns`, so `open` fails with `OpenError::Connect` before `addrman.attempt` (`netcore.rs:883`).
- **Core:**
  - net.cpp:411: `addrConnect = CAddress{MaybeFlipIPv6toCJDNS(r), NODE_NONE}`.
  - netaddress.cpp:881 `GetSockAddr` treats IPv6 and CJDNS alike, and net.cpp:488-492 records the `Attempt`.
- **Scenario:** a host sets `reachable_overlays = [Cjdns]` (Core's `-cjdnsreachable`; D27 offers it, and CJDNS is plain IPv6 TCP, so it needs no proxy).
  - `-addnode=[fc00::1]:9999` connects as an IPv6 `fc00::/8` address. That address is unroutable, so `GetLocal` scores our IPv4 address above our CJDNS one, and the processor advertises our **IPv4** address to a CJDNS peer. Core advertises the CJDNS address. The difference is visible on the wire, and it leaks our clearnet address.
  - `GetAddedNodeInfo` keys the entry as CJDNS (flipped) and never sees it connected, so addcon re-plans it every 2 s instead of every 60 s.
  - Every CJDNS address AddrMan selects fails without an `Attempt`, so `m_last_try` never moves and the same address is re-picked.
- **Evidence:** I read both paths (`netcore.rs:918-933` against net.cpp:403-415). Two sub-reviews found this independently (W-1, DIAL-2).
- **Fix:**
  - Flip in `resolve_dest`.
  - Map `Cjdns` to its IPv6 socket address in `to_socket_addr`, or `attempt` before the `None` return.
  - Have `inbound::accept`'s doc require the host to pass flipped `addr`/`addr_bind` (net.cpp:1864-1867), or flip them there.
  - Alternatively, record a D-row that CJDNS is unsupported and refuse it in `reachable_overlays`.

#### m2 — `NetConfig::v2_transport` is dead, and `NetConfig::new`'s "Core defaults" advertise `services = NODE_NETWORK` only

- **Location:** `netcore.rs:107-108` and `:195` declare `v2_transport` and default it to `true`, but nothing reads it. Every V2 decision reads `local_services & NODE_P2P_V2` (`netcore.rs:569`, `inbound.rs:364`, `dial.rs:442`, `:793`). `netcore.rs:76`/`:183` set `local_services: NODE_NETWORK` and call that "Core's defaults".
- **Core:** the defaults are `NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED | NODE_P2P_V2 | NODE_BLOOM`, plus `NODE_NETWORK` after load (init.cpp:1125, :1235-1236, :1370-1371, :2354). That is 0x1C05.
- **Scenario:** any host that keeps the documented defaults advertises `services=0x1` and never speaks V2, despite `v2_transport = true`. That identifies it in one `version` message. The node host is unaffected today, because P2a's `fingerprint::local_services` builds 0x1C05, but sims and a future SPV wiring are exposed.
- **Fix:** make `v2_transport` the single source by OR-ing `NODE_P2P_V2` in `NetCore::new`, or delete it. Fix the doc, or set the default to Core's pre-load value.

#### m3 — `.onion`/`.i2p` names go to the host's DNS resolver; Core never resolves them through DNS

- **Location:** `netcore.rs:918-923` sends every non-numeric host to `resolver.resolve`. `dial.rs:384-390` (`lookup_numeric`) has the same gap.
- **Core:** `LookupIntern` tries `CNetAddr::SetSpecial` (Tor v3 `.onion`, I2P `.b32.i2p`) before any DNS query (netbase.cpp:147-159, netaddress.cpp:209-224).
- **Scenario:**
  - `-addnode=<v3>.onion` or `-connect=<v3>.onion` sends the onion name to system DNS on every addcon or `-connect` pass. That is a privacy leak Core avoids (RFC 7686).
  - Addnode de-duplication and `GetAddedNodeInfo` matching also differ for such names.
  - This is configuration-driven, hence minor.
- **Fix:** parse special names into `NetAddr` before the resolver in both places, or refuse them without calling it. Nit in the same place: `split_host_port` (`netcore.rs:1364-1379`) accepts the malformed bracket forms `[::1]:abc` and `[::1]x:9999`, which Core's `SplitHostPort` rejects. Port it literally.

#### m4 — a panic in a host callback on the sweep task stops the sweep for good and leaks the peers it was removing, and `shutdown` hides it

- **Location:**
  - `netcore.rs:1225-1258`: the batch is taken out of the table first, then `remove_peer` calls `events.finalize` for each.
  - `netcore.rs:1103`: `let _ = task.await;` discards the JoinError, and `:1115` does the same for the I/O tasks.
- **Core:** an exception escaping `FinalizeNode` terminates the process. D30 covers only AddrMan's check panic.
- **Scenario:** under the default `panic = "unwind"`, a `finalize` that panics leaves the rest of its batch unclosed, unfinalized and still holding their grants. Every later mark and inactivity timeout is never swept, so slots fill, and `shutdown` returns normally.
- **Evidence:** sub-review probe `probe_sweep_panic_leaks`: outbound grants stuck at 10 of 11 after shutdown, a later-marked peer still listed 1.5 s on, and only one of the batch's two peers finalized.
- **Fix:** abort on a panic in the sweep or I/O tasks (`catch_unwind` then `abort`, which is Core's outcome), or `resume_unwind` in `shutdown`. At minimum, extend D30 and the `NetEvents` contract to require `panic = "abort"`.

#### m5 — disconnect timing: FIN within ms of the mark and close at exactly mark + 100 ms, contrary to the cited P2a cadence

- **Location:**
  - `netcore.rs:47` documents `SWEEP_INTERVAL` as "a close lands 100–600 ms after the mark".
  - `disconnect`/`discourage` wake the sweep at once (`netcore.rs:645`, `:674`).
  - The sweep sleeps exactly to the linger deadline (`netcore.rs:1409-1413`).
  - The reader stops at the mark, so a remote close during the linger is not seen (`netcore.rs:1438`).
- **Core:**
  - A message-handler mark doesn't wake the socket thread. `DisconnectNodes` acts on the next pass: a socket event, or the 500 ms POSIX select timeout (net.cpp:111-120, 2067-2201).
  - A remote close during the linger ends it early: `node_err_set` is still serviced, and EOF sets `fOtherSideDisconnected` (net.cpp:2522-2529, :2620-2627).
- **Evidence:** sub-review probe, baseline FIN at 2–3 ms after the mark and removal at 101–103 ms in every run.
- **Scenario:** on an idle node, a misbehaviour or eviction disconnect sends FIN within milliseconds, and an RST for unread data lands at exactly +100 ms. Core spreads both over 0–500 ms. This is a weak timing fingerprint that converges on a busy node, and no D-row records it.
- **Fix:** a D-row ("close at the linger deadline; Core's pass jitter not modelled") and a corrected comment. Or don't wake the sweep on marks, so passes follow the 500 ms cadence or socket events.

#### m6 — `policy::read` turns two of base-sdk's handler refusals into a `Throw` where Core scores

- **Location:** `policy.rs:900-903`. In the `None =>` arm, `Throw::from_decode` maps `TooManyEntries` and `HandlerPayloadTooLarge` to an iostream failure (`policy.rs:737-750`).
- **Core:**
  - `headers` count > 2000: `Misbehaving(20, "headers message size = N")` and return (net_processing.cpp:5323-5327).
  - `qgetdata` > 67 bytes: `PeerMisbehaving(100, "oversized qgetdata")` before the read (llmq/net_quorum.cpp:90-93).
- **Scenario:** a processor that reads through the documented boundary (`policy::read::<Headers>` or `<QuorumDataRequest>`) logs a caught exception and scores nothing, so 2001-header spam never accrues score. It is latent today, because P2a-core checks the `headers` count by hand and `qgetdata` is a stub. f1e6590d7 fixed exactly this class for `headers2` (`Headers2Error::TooMany`) and left these two.
- **Fix:** a typed handler-return error from `read`, as `read_headers2` has, or a `read_headers`. At minimum, document on `read` that callers pre-check these two and debug-assert it.

#### m7 — the `DkgIntake`/`qwatch` pre-read doc puts SPORK_17 first for a plain node

- **Location:** `policy.rs:84-87` and the `qwatch` row comment (`policy.rs:625`).
- **Core:** a plain node registers `NetDKGStub` (init.cpp:2235), which scores +10 for `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit`/`qwatch` with **no** SPORK_17 test (llmq/net_dkg.cpp:938-944). The SPORK_17 early return is `NetDKG`'s only (net_dkg.cpp:494), on masternodes and observers.
- **Scenario:** a processor that follows the column ignores DKG traffic on a plain node with SPORK_17 off (regtest or devnet before the spork), where Core scores +10.
- **Fix:** reword it: "a plain node scores +10 whatever SPORK_17 says; with `NetDKG`, SPORK_17 off ignores, …". Give `qwatch` the same note.

#### m8 — `banlist.json` is not Core's bytes, though D13 says it is, and its key order depends on feature unification

- **Location:** `ban.rs:184-203`. `serde_json::json!` plus `PrettyFormatter` with 4-space indent. `dash-p2p` takes `serde_json` without `preserve_order`.
- **Core:** `WriteSettings` writes `_warning_` first, then the values, with `write(4, 1)` (util/settings.cpp:127-150). Entry keys are written in the order `version, ban_created, banned_until, address` (net_types.cpp). UniValue writes an empty array as `[\n    ]`.
- **Evidence:**
  - `cargo tree -p dash-p2p -e features -i serde_json` has no `preserve_order`; `--workspace` has it, through rpc-client/rpc-json.
  - The sub-review's probe on a `-p dash-p2p` build printed alphabetical keys (`address, ban_created, banned_until, version`), and printed `"banned_nets": []` for an empty list, with no `_warning_`.
- **Scenario:**
  - The bytes on disk depend on which crates the binary unifies.
  - A datadir moved from dashd and diffed, or a byte-level conformance check, differs.
  - Both readers accept either form, so no ban is lost. Nothing differs on the wire.
- **Fix:** write UniValue's format by hand (`_warning_`, Core's key order, the empty-array form), and add a golden test from a dashd-written file. Or narrow D13's claim.

#### m9 — store errors at init are fatal where Core recreates the file

- **Location:** `netcore.rs:479` and `:517` (`store.load(..).map_err(InitError::Store)?`); `:500` (backup).
- **Core:** an unopenable `peers.dat` throws `DbNotFoundError` and gets a fresh AddrMan (addrdb.cpp:125-128, :204-208). A failed `banlist.json` read logs "Recreating the banlist database" (addrdb.cpp:153-178, banman.cpp:35-45). A failed `.bak` rename has its own text (addrdb.cpp:219-220).
- **Scenario:** an EACCES or EIO on `peers.dat` or `banlist.json` makes dashd start and recreate the file, while `NetCore::new` fails with `network store: …`. Confidence is medium, since it depends on how a host's `NetStore` maps an unopenable file.
- **Fix:**
  - Treat a peers load `Err` like `Ok(None)`.
  - Start bans empty and dirty on `Err`.
  - Map a backup failure to Core's text.
  - Or record the deviation and the host contract in a D-row.

#### m10 — `SanityCheckASMap` exists nowhere, though `NetGroupManager::new`'s doc relies on the host running it

- **Location:** `netgroup.rs:15-27`. The doc says "The host decodes and sanity-checks the file (Core's `DecodeAsmap` and `SanityCheckASMap`)". `decode_asmap`, documented as Core's `DecodeAsmap`, only unpacks bits, and no Rust `SanityCheckASMap` exists in the workspace (grep).
- **Core:** `DecodeAsmap` runs `SanityCheckASMap(bits, 128)` and returns `{}` on failure, and init then fails with "Could not parse asmap file" (util/asmap.cpp:197-222).
- **Scenario:** a truncated `-asmap` file stops dashd from starting. This node starts with silently different bucketing and netgroups: `Interpret` returns 0 instead of asserting.
- **Fix:** port `SanityCheckASMap` (about 60 lines) and have `decode_asmap` return an empty map or an error on failure.

#### m11 — the V1-retry queue grows without bound on a host that runs no dial loop

- **Location:** `netcore.rs:1241-1255` pushes a retry for every removed peer whose V2 handshake failed early. Only `tick_opencon`/`tick_addcon` and `take_reconnection` pop it, and the latter is documented as "for the `addcon` loop".
- **Scenario:** an SPV host, which per D16 runs no dial loop, dialling V2 to peers that drop the key exchange accumulates one `OpenRequest` per failure, possibly holding a grant, for the life of the process.
- **Fix:** document on D16 and `take_reconnection` that such hosts drain and `open` the queue. Alternatively, add a `NetConfig` switch that SPV turns off, or cap the queue.

#### m12 — further surviving mutants: send accounting, grant release, AddrMan and the decode table

Calibration runs, beyond M2's seven mutants:

| Mutant | Location | `dash-p2p` suite | Oracle / node-fuzz |
|---|---|---|---|
| `send_data`'s final `fPauseSend` drops the transport term (SB0 §3.1 requires this test) | `conn.rs:901` | survives | n/a |
| `remove_peer` doesn't take the grant while the processor holds the handle | `netcore.rs:1179` | survives | n/a |
| a refused `attach` doesn't take the grant | `netcore.rs:1032` | survives | n/a |
| linger never sends `SHUT_WR`: FIN moves from about 2 ms to about 101 ms | `netcore.rs:1208` | survives | n/a |
| GetAddr filters by `network` instead of `net_class` | `addrman/imp.rs:826` | survives (39 addrman tests) | **survives** r1 `l1_addrman` (pinned `diff_addrman`) |
| the new-table overwrite drops `&& ref_count == 0` | `imp.rs:590` | survives | **survives** `l1_addrman` |
| ResolveCollisions `> 60` → `>= 60` | `imp.rs:932` | survives | **survives** `l1_addrman` |
| Unserialize ref cap `>=` → `>` (crafted peers.dat) | `addrman/ser.rs:456` | survives | not run (no crafted-file leg) |
| Unserialize `entry_index >= 0` → `!= -1` | `ser.rs:423` | survives | not run |
| headers2 MRU not promoted | `policy.rs:1228` | survives | caught by node-fuzz matrix (`headers2_values_by_hand`, `_match_the_boundary`) |
| headers2 raised limit from 70236 instead of 70235 | `policy.rs:1015` | survives | **survives** node-fuzz lib tests |
| `getcfcheckpt` row reads nothing | `policy.rs:534` | survives | caught by matrix `rows_are_policy_rows_that_read` |
| gate order: blocks-only before the first-message stamp | `gates.rs` | survives | `p2p_gate_model` has no `#[test]`; fuzz only |
| `filteradd` handler score 100 → 20 | `policy.rs:509` | survives | survives (nothing reads scores) |

The SB6 gate doesn't compile node-fuzz's lib tests (`fuzz-unit.sh` runs them separately), so the two node-fuzz catches are not gate-enforced either.

- **Fix:**
  - Add the probes the sub-review wrote (pause held while the transport holds a partial; grant released with the handle held and on a refused attach; FIN before the linger ends).
  - Add AddrMan unit tests for each AddrMan mutant above, asserting Core's result on a crafted state or file.
  - Add a 70235 `headers2` case.
  - Assert every `Handler` row's `(score, msg)` in `throw_outcomes`.
  - Assert the count of `ReadPlan::Nothing` rows is 15.
  - Add a deterministic `#[test]` to `p2p_gate_model`.
  - Run node-fuzz's model lib tests in the gate.

### Nits

- **n1** `local.rs:290`: the `local_address` fallback is IPv4 `0.0.0.0`; Core uses `CNetAddr()` = IPv6 `::` (net.cpp:232-235). The API only, nothing on the wire.
- **n2** `dial.rs:885`: the DNS loop's `Done` isn't sticky. A driver that ticks again after "P2P peers available" restarts seeding, with a second shuffle draw. Add a `Finished` phase.
- **n3** No D-row records that `ThreadOpenMasternodeConnections` (net.cpp:3397-3585) is left out and belongs to P7/AM1. D38 only mentions it in passing.
- **n4** The outbound limits are Core's *member* values, not derived from `max_connections` (init.cpp:2609-2611). With `-maxconnections=9` alone, Core allows 1 block-relay peer and this allows 2. Document it as a host obligation or add a setter.
- **n5** `dial.rs:806` logs "Making feeler connection" before the feeler's sleep; Core logs after it (net.cpp:3271-3278). Debug log only.
- **n6** `conn.rs:892`: the send path treats `EMSGSIZE`/`EINPROGRESS` as fatal, but Core treats them as transient (net.cpp:1756). Use `is_transient` as the reader does.
- **n7** Between `interrupt()` and `shutdown`'s socket close, which spans the dump, writers keep flushing and readers keep reading. Core sends nothing after `interruptNet` (net.cpp:2446-2533).
- **n8** `sock.rs:53-62`: failing to set `TCP_NODELAY` fails the connection; Core logs and continues (net.cpp:1926-1930).
- **n9** `inbound::accept` isn't serialized. Document that the host calls it from one task.
- **n10** D37 lists `dstx` among the out-of-scope CoinJoin rows, but every Core node reads it outside IBD (net_processing.cpp:4861-4874). Note that it needs a Core-judged read when CJ1/P5 reads it.
- **n11** `Throw::from_decode`'s doc (`policy.rs:723-735`) says every other count bound is the bytes left. `getblocktxn`/`cmpctblock` 65535 bounds contradict it. The texts are log-only; fix the doc or list them in D37's "different text" set.
- **n12** `ban.rs:61`: `now + offset` overflows, which panics in debug, on an absurd `setban` time. Use `wrapping_add`, Core's de facto result.
- **n13** `ban.rs:215`: a banlist `version` outside `int` range drops one entry; Core's `getInt<int>` throws and recreates the whole list (net_types.cpp:61, addrdb.cpp:175-177).
- **n14** `shutdown` dumps bans only if `start_addresses` ran (`netcore.rs:1107`). Core's `~BanMan` always dumps (banman.cpp:23-26).

## Simplification opportunities

- Remove `NetConfig::v2_transport`, or make it the single source of `NODE_P2P_V2` (m2).
- `PingState::{last_us, min_us}` duplicate `Peer::{last_ping_us, min_ping_us}`; Core keeps them on `CNode` only.
- `addr::Intake::step` is an `Option<Step>` documented as always set; make it a plain `Step`.
- `AddrMan::deserialize` (test callers only since R3-M1), `BanList::serialize` (test-only) and `SubNet::netmask` (unused) can go behind `cfg(test)` or be deleted. Drop `deserialize` from D38.
- `ser.rs:141-203` and `netaddr.rs` `from_addrv2`/`to_addrv2` duplicate the embedded-IPv4, Tor and internal prefix handling.
- `CommandPolicy::{blocks_only_violation, quorum_priority}` columns duplicate `violates_blocks_only`/`is_quorum_priority`, kept equal only by a test. Keep one representation.
- Core-lenient reads are keyed in four places: `core_bool_offset`, `gov_limit`/`gov_rest`, the `headers2` arm and `read`'s `TypeId` map. One per-command `CoreRead` enum would hold them.
- A `const fn headers_limit(version, compressed)` would remove `headers2_count`'s `let … else unreachable!`, and serve m6's `read_headers`.
- `inbound.rs`:
  - The eviction group map only needs a count and the first member.
  - `erase_last_k` is O(k·n), where `split_off` plus `filter` would do.
  - `.min(cfg.max_connections)` (`:325`) can go to match `nMaxInbound` literally.
- `open()` calls `already_connected_to_address` and then `exists_service` with no await between them, so the second check can't fire. Keep it only with a comment saying why.
- `send_data` sets `fDisconnect` with a raw store that skips the reader wake. Use `mark_disconnect`.

## Confirmations

The sub-reviews checked each of the following against Core v24.0.0-rc.3 line by line. I re-checked the items marked †.

- **Handshake.**
  - `version_for` = `PushNodeVersion` (net_processing.cpp:1561-1593): every field, inbound `|NODE_BLOOM`, `addr_you` zeroing, the localhost nonce from SipHash, `fRelay`, and the MNAUTH challenge drawn at our `version` (D38).
  - `on_version` (:3935-4176): read points, the obsolete-version text, `LIMITED_STRING(256)`, `CheckIncomingNonce`, the devnet check, the push order `sendaddrv2` → host `SetTxRelay`/`sendtxrcncl` → `verack` → `getaddr`, `AddTimeData` outbound only, and the feeler disconnect last.
  - `on_verack` (:4187-4259): `mnauth` → `sendheaders2` (their `NODE_HEADERS_COMPRESSED`) → `sendcmpct(0,1)` → `senddsq`/`qwatch` → txrcncl.
- **Gates and the decode boundary.**
  - The gate order has no score before `version` or before `verack` (:4178-4385).
  - The 74 commands are in order, the blocks-only set has 26 entries and the quorum-priority set 8 (protocol.cpp:98-211, net.h:2053-2070).
  - More than 20 matrix rows were re-derived from serializers and handlers, including trailing bytes being ignored.
  - The 17 out-of-scope rows are base-sdk stubs with no verdict at the boundary.
  - The Core-lenient bools are complete for the read rows.
  - No payload-derived panic exists in `policy.rs` or the models.
- **Ping and addr intake.**
  - `PING_INTERVAL`/`TIMEOUT_INTERVAL`, the nonce from a fresh `GetRand` redrawn until non-zero, and the pong branches† (`ping.rs:100-150`).
  - The addr token bucket, `>1000` → +20 before the bucket, the time fixup, `AddAddressKnown` before the ban skip, the relay condition, the AddrFetch disconnect, `SetupAddressRelay`'s (5000, 0.001) filter, and `CRollingBloomFilter`.
- **Local addresses and time.** `GetLocal`/`GetReachabilityFrom`/`IsPeerAddrLocalGood`/`GetLocalAddrForPeer`, `AddLocal`/`SeenLocal`, and the `timedata.cpp` median filter.
- **Transport choice.** V1/V2 per connection type (net.cpp:1980, 2819, 2927, 3289); `ShouldReconnectV1` and the retry's captured fields; V2 key, entropy and garbage draws before `initialize`.
- **Send and receive.**
  - `push` = `PushMessage`: charge, pause upward only, always append. `send_data` = `SocketSendData`, including `MSG_MORE` and the stats.
  - `fPauseRecv` over both queues and the 100:1 ratio.
  - Per-peer memory is bounded: queued raw bytes ≤ `-maxreceivebuffer` + 64 KiB, and the transport allocates at most 256 KiB ahead of what has arrived.
  - No `as` truncation of peer lengths.
- **Concurrency.**
  - Lock order send → transport everywhere it nests; recv → stats; nodes.write → {io_tasks, network_counts}; dial → reconnections.
  - No guard is held across `.await`: parking_lot guards are `!Send` inside `Send` tasks.
  - `finalize` runs exactly once.
  - Shutdown joins the sweep before the dump† (`netcore.rs:1096-1123` vs net.cpp:4168-4230), and `Taken` restores on cancellation.
  - D39 grant release under `live_dial`.
- **Dial loops.** Every rule of the ladder, the 100-try loop, fixed seeds, `-connect`, addcon and DNS seeding is in Core's order. Every iteration sleeps, no grant leaks on error paths, and an empty AddrMan, all networks unreachable or `max_connections = 0` behave as Core does. (Untested, see M2.)
- **Eviction.** The comparators, `ProtectEvictionCandidatesByRatio`, `SelectNodeToEvict` and `GetEvictionCandidates` match. D36's libstdc++ port matches GCC 13 line by line, and a non-strict-weak comparator panics rather than going out of bounds.
- **AddrMan and persistence.**
  - The peers.dat and anchors.dat formats and every `LoadAddrman` branch (addrdb.cpp:204-231).
  - A crafted `nUBuckets = i32::MAX` reads to EOF with Core's text and no huge allocation (probe).
  - Select and collision draw order (D34). Collisions are capped at 10. `stdmap` matches libstdc++ iteration with `RandomState` lookups (no HashDoS).
  - Unusual networks from peers leave `check() == 0` (probe).
  - Anchors† = `IsBlockRelayOnly` (net.cpp:554, 3295, 4187-4200), D28.
  - `take_dump`'s atomic sweep and copy (F1) holds.
- **Clock and randomness.** `dash-clock`† is SB4's copy plus `node_now_micros` (D10). `FastRandom`, `siphash`, `netgroup`/asmap `Interpret` match. `V2Transport::with_rand`† draws in Core's order (net.cpp:1099-1101, key.cpp:160-166).

## Validation

Every command ran under `nice -n 10`, `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER` and `TMPDIR=/work/scratch/sb6-opus/tmp` (0700, on disk), with own target directories; seeded targets are reflinks of earlier reviewers' targets. Load was 70–100. rustc is from the workspace toolchain.

- **Exact head, `cargo test -p dash-p2p -p dash-clock --all-features -- --test-threads=4`:** **162 passed, 0 failed**. That is dash-p2p 160 (lib 105, addr_ping 10, bans 3, eviction 1, handshake 17, rand_source 9, send 10, shutdown 3, sim 1, stdsort 1) plus clock 2.
- **Exact head, builds and lints:** `cargo check -p dash-p2p --no-default-features` and `--features inbound` (production surfaces) build. `cargo clippy -p dash-p2p -p dash-clock --all-features --all-targets -- -D warnings` and `cargo fmt --check` are clean.
- **r1 (pinned ABI 9, `DASH_ORACLE_CACHE=/work/scratch/o1c/oracle-cache`), `node-l1 --features oracle`:**
  - `l1_addrman` 2/2 (seeds, random inputs);
  - `l1_overlay` 5/5 (every command a v24 target, input encodings, the BS8 corpus, O4 fixtures, mutations);
  - `l1_overlay_alloc` 1/1.
- **r2 (ABI 10 dev, unpinned), `node-l1 --features oracle`:**
  - `l1_addrman` 2/2, `l1_overlay_alloc` 1/1;
  - `p2p` 3/3 (`scripts_decode_as_before_the_session_types_grew`, `o1c_targets_replay_core`, `core_leg_runs_ticks_drains_and_controlled_peers`). R4-m1's Tick and Drain regression strength is being fixed separately and isn't re-reported.
- **Fuzz smokes on r1 (pinned fuzz archive 6efa24f2):** run with `node-fuzz/run.sh <t> --pin v24.0 --cpu-hours 0.05 --jobs 1` in a 0700 fixture dir (`DASH_EMBARGO_DIR=/work/scratch/sb6-opus/fuzz`); the private sink was not used.
  - `p2p_decode_model`: 867,087 executions, 0 divergences, 0 crashes, 0 sink failures, `status=pass`.
  - `p2p_gate_model`: 2,469,679 executions, 0/0/0, `status=pass`.

  These are smokes, not DEC-184/186 certification.
- **node-fuzz lib tests (`--lib p2p_decode`, r1 sources):** 10/10 at baseline. Mutation results are in m12.
- **Mutations:** 7 (dial/inbound) + 6 (conn) + 6 (decode, dash-p2p side) + 6 (AddrMan lib) + 3 node-fuzz re-runs + 3 oracle re-runs, all in private copies or my own scratch test-base worktree, which was reverted (`git status` clean). The tables are in M2 and m12.
- **Not done:** a workspace gate or dash-spv dashd tests (BL-20, reported elsewhere); a fuzz campaign or plateau certification; a P2a-core processor-versus-Core session comparison (D32: the node leg is still Core); macOS, Windows or Android builds.

Scratch: the review worktrees and targets under `/work/scratch/sb6-opus/` are deleted after this report. Sub-review notes are under `notes/`, and logs under `logs/`.

No Dash Core security issue was found.

## Verdict

M1 is a real Core-parity gap in misbehaviour handling, reachable as soon as P2a-core's processor (which calls `discourage(state.id)`) runs, and fixing it changes SB6's public API. M2 leaves the dial and accept paths, which carry the most indistinguishability weight, with no regression protection: seven parity mutants survive. Both are small to fix: an API and handle change plus tests, and tests only. The minors can follow in the same round or be registered as D-rows; m1, m2 and m6 are the ones a spoofing peer or a downstream processor would hit first.

**Verdict:** CHANGES-REQUIRED
