# SB6 — Opus re-check (fix round r4, `dash-p2p`)

## Scope and pins

- **Target:** `node/sb6-dash-p2p` at **3f991ee8d**, reviewed detached at `/work/scratch/sb6-recheck/wt`.
  - The range is `5516ee242..3f991ee8d`: 37 commits, all signed (`%G?` = G).
  - It touches 46 files, +4,339/−265. Most of that is tests.
- **Prior review:** `reviews/SB6-opus-high.md`, which found M1, M2, m1–m12 and n1–n14.
- **D-rows:** D43–D58 and the API-ledger rows 055aebd32, d1f3f1218, 926a8cc4b, 8bf567f2c and 2455c06a4 in `docs/node-program/designs/SB6-dash-p2p-notes.md`.
- **Core references:**
  - v24.0.0-rc.3 from `/work/scratch/p2a-fable/v24.0.0-rc.3/src`, with `git show v24.0.0-rc.3:` in `~/workspace/dash` for files the extract lacks (util/asmap.cpp, llmq/net_dkg.cpp, init.cpp).
  - O1c at rdc `c083e5aa5`, `dash-oracle-sys/tests/peer_session.rs:835-881`.
- **Method:**
  - I read each fix against Core.
  - Every finding the brief names was checked for closure by mechanism, not by its test alone: I re-applied **18 mutants** (M2 ×8, m12 ×7, and M1/m1/m5 ×3) and confirmed a behavioural test kills each one.
  - I regenerated the banlist goldens with dashd rc.3.
  - One reviewer probe covered the sweep cadence.

## Verdict on each item

| Item | Status | Mechanism verified |
|---|---|---|
| **M1** (D46) | **Closed** | `discourage(&PeerHandle)` (`netcore.rs:693-717`) applies `noban` → manual → `IsLocal` to the handle, then `banlist.discourage(addr)` and `disconnect_subnet`, all without a table lookup. This is Core's `MaybeDiscourageAndDisconnect` order (net_processing.cpp:5845-5866). `disconnect(PeerId)` still finds no removed peer, which matches `ForNode` (net.cpp:4603-4610). Mutant P (re-adding the live-table lookup) is killed by `discourage_reaches_a_peer_the_sweep_removed` and `discourage_exemptions_hold_after_removal`. The API row is d1f3f1218; the P2a call site `misbehavior.rs:65` migrates on re-merge, per STATUS. |
| **M2** | **Closed** | All 8 mutants re-applied, each killed by a behavioural test (table below). |
| **m1** (D56) | **Closed** | `resolve_dest` flips to CJDNS (`netcore.rs:979`), and `lookup_numeric` and `accept` flip `addr` and `addr_bind`. `to_socket_addr` maps CJDNS to IPv6 (netaddress.cpp:868-893). `open` records `Attempt` for every valid address before the Tor/I2P no-route return, as Core's no-proxy `ConnectDirectly` then `Attempt` does (net.cpp:474-492). Mutant Q (no flip) is killed by `named_cjdns_target_is_cjdns` and `open_attempts_cjdns_and_unsupported_networks`. |
| **m2** (D47) | **Closed** | `v2_transport` is gone from the workspace (`git grep` is empty). `NetConfig::new(params, local_services)` has no default for services. |
| **m3** (D57) | **Closed, Core-literal** | Core's `LookupIntern` (netbase.cpp:147-169) runs `ContainsNoNUL`, then `SetSpecial`, then `dns_lookup_function(name, fAllowLookup)`, so an **invalid** `.onion`/`.i2p` name does reach getaddrinfo in Core. That confirms the implementer's order. `SetTor`/`SetI2P`/`DecodeBase32` match netaddress.cpp:209-290 and strencodings.cpp:194-230 line by line: case-sensitive `.onion`, ASCII-case-insensitive `.b32.i2p`, the 1/3/4/6 padding strip, `ConvertBits<5,8,false>` with `max_acc = 2¹²−1`, and the version-3 and checksum tests. `default_port_for` = `GetDefaultPort(string)` (I2P → 0). `split_host_port` = `SplitHostPort` with `ParseUInt16`. |
| **m4** (D44) | **Closed** | `abort_on_panic` (`catch_unwind` → log `"net" raised an exception` → `abort`) wraps the sweep (`netcore.rs:610`) and both I/O tasks. `task_panic.rs` runs each case in a child process and asserts `SIGABRT` plus the panic text. A callback on the host's own task unwinds to the host. |
| **m5** (D43) | **Mechanism closed; residual misdescribed.** See **R-m1**. | A mark no longer wakes the sweep (mutant R, re-adding the wake, is killed by `a_mark_waits_for_the_next_pass`). A remote EOF after our `SHUT_WR` ends the linger. A readable lingering socket models `only_poll`/`HasUnpausedReceivableNode` (net.cpp:2376-2415, 2453-2470). The residual, though, is a **busy-node** difference, not an idle-node one. |
| **m6** (D51) | **Closed** | `ReadError::{TooMany, Oversized, Throw}` gives `(20, "headers message size = N")` and `(100, "oversized qgetdata")`. `headers_limit` = `GetHeadersLimit`. base-sdk's `read_count_at_most` runs `decode_size` (the `MAX_SIZE` throw) before the limit, so a count over 0x02000000 still throws "size too large", as Core's `ReadCompactSize` does. Mutant N (`>=` → `>` at 70235) is killed twice. |
| **m7** | **Closed** | The `DkgIntake`/`qwatch` docs match `NetDKGStub` (net_dkg.cpp:938-944, init.cpp:2235) and `NetDKG::ProcessMessage` (:492-508: SPORK_17, then `qwatch` → +10 for a non-masternode). |
| **m8** (D53) | **Closed, independently confirmed** | I regenerated both goldens with `dashd` v24.0.0-rc.3 in private regtest datadirs, using the README recipe (`banlist-regen.sh`). The output is byte-identical: sha256 `272b9136…` and `b6df734c…`. The writer no longer depends on `serde_json` features. |
| **m9** (D54) | **Closed** | A peers load `Err` → `unwrap_or(None)` is Core's `DbNotFoundError` branch (addrdb.cpp:125-128, :204-208). A bans `Err` → recreate. A failed backup → `InitError::RenamePeers`, with Core's exact text (addrdb.cpp:219-221) and the io error as `source()`. D54's mid-read-EIO deviation is documented. |
| **m10** (D55) | **Closed** | `sanity_check_asmap` is a literal port of util/asmap.cpp:135-195, covering jump-into-instruction, RETURN after DEFAULT, padding ≤ 7 and zero, unreachable code, intersecting jumps, one incomplete MATCH per run, and `bits` underflow. `tests/asmap.rs` covers Core's `asmap.raw`, every truncation and a trailing zero byte. |
| **m11** (D45) | **Closed** | The `self.cfg.v1_retry &&` guard sits before `should_reconnect_v1` (`netcore.rs:1306`). `a_v2_close_before_any_byte_queues_a_v1_retry_unless_off` covers both settings. |
| **m12** | **Closed** | The implementer's per-mutant table is not in the D-row ledger or anywhere on disk I could find (the notes file, `/work/scratch/sb6/logs`, T3 search), so I re-ran seven of the original m12 mutants. All seven are killed (table below). The gate now has a `fuzz-unit` step (18 tests), and `p2p_gate_model` has a deterministic `#[test]` (a1160baa9). |
| **O1c draw note** (887a4cabf, D58) | **Closed** | See below. |
| **Nits n1–n14** | Closed or recorded | n1 `CNetAddr()` `::`. n2 sticky `Finished` (mutant H killed). n3 D48. n4 D49. n5 feeler log after its sleep. n6 `is_transient` on send. n7 no I/O after `interrupt`. n8 `TCP_NODELAY` keeps the socket. n9 doc plus D50. n10/n11 D52. n12 `wrapping_add`. n13 the `i32` range refuses the file. n14 `dump_bans` at every shutdown. |

### M2 and n2 mutants, re-applied (`/work/scratch/sb6-recheck/mut/`)

| Mutant | Site | Killed by |
|---|---|---|
| A: netgroup-diversity skip disabled | `dial.rs:753` | `dial::loop_tests::opencon_skips_connected_netgroups` |
| B: `tries < 30` → `< 3` | `dial.rs:768` | `opencon_selection_skip_rules` |
| C: DNS stop `>= 2` → `>= 3` | `dial.rs:960` | `dnsseed_stops_at_two_full_outbound_peers` |
| D: fixed seeds after 60 s → 6 s | `dial.rs:617` | `opencon_fixed_seeds` |
| E: verified-masternode eviction protection disabled | `inbound.rs:248` | `inbound::accept_tests::eviction_candidates_masternode_protections` |
| F: discouraged refusal `inbound+1 >= max` → `inbound >= max` | `inbound.rs:358` | `accept_refuses_discouraged_when_nearly_full` |
| G: `is_local` forced false | `inbound.rs:264` | `eviction_candidates_fields` |
| H (n2): `Finished` re-enters `Start` | `dial.rs:990` | `dnsseed_stops_at_two_full_outbound_peers` |

### m12 spot-check, re-applied

| Mutant | Site | Killed by |
|---|---|---|
| I: `fPauseSend` drops the transport term | `conn.rs:906` | `send::a_partly_sent_message_keeps_the_pause` |
| J: `remove_peer` keeps the grant | `netcore.rs:1238` | `grants::a_removed_peer_releases_its_grant_while_its_handle_is_held` |
| K: GetAddr filters by `network`, not `net_class` | `addrman/imp.rs:844` | `addrman::crafted_tests::getaddr_filters_by_net_class` |
| L: new-table overwrite drops `&& ref_count == 0` | `imp.rs:608` | `new_table_overwrite_needs_an_unreferenced_newcomer` |
| M: ResolveCollisions `> 60` → `>= 60` | `imp.rs:950` | `resolve_collisions_waits_more_than_60_seconds` |
| N: raised `headers2` limit from 70236 | `policy.rs:424` | `headers_limit_is_get_headers_limit`, `headers2_limit_by_version_on_both_readers` |
| O: `filteradd` handler score 100 → 20 | `policy.rs:524` | `policy_table::every_handler_catch_is_cores_score_and_reason` |

After every run `git status --porcelain` was empty.

### The O1c draw note (D58)

- **The salts.** In rc.3, `CServiceHash()` draws `m_salt_k0{GetRand<uint64_t>()}` and then `m_salt_k1{GetRand<uint64_t>()}` (netaddress.h:576-578).
  - In `AddrManImpl`, members initialise in declaration order: `insecure_rand`, then `nKey` (whose `rand256()` self-seeds through `GetRandHash`), then `mapInfo`, then `mapAddr` with its default `CServiceHash` (addrman_impl.h:158-193; addrman.cpp:111-116).
  - The salts therefore come after `nKey`, and they come whether or not the AddrMan is deterministic, since the deterministic flag only affects `insecure_rand`/`nKey`.
  - `Impl::new` now draws `get_rand(rand, u64::MAX)` twice after the key. `unserialize` builds its placeholder with `with_context`, which draws nothing. That is right, because Core's `Unserialize` reads into the existing impl.
- **The counts.**
  - With no `peers.dat`, `LoadAddrman` draws 3 for the first AddrMan, 3 for the replacement in the `DbNotFoundError` branch, 1 for BanMan's `CRollingBloomFilter` tweak, and 2 for CConnman's `nSeed1`, `nSeed0`. That makes **9**. With a readable file it is 3 + 1 + 2 = **6**. The unsupported and inconsistent branches give 9 as well (addrdb.cpp:200-232).
  - `rand_source.rs` pins exactly these counts: `netcore_new_draws_in_inits_order`, `readable_peers_dat_keeps_the_constructors_context` and `unreadable_peers_dat_draws_only_for_cores_replacement`.
  - O1c's `setup_draws_in_inits_order` tiles the same stream. Its first five draws are the test context, ECC_Start 32, `StrongRandBytes` 32, 8 and the cache nonce 32. Then come ten seeded contexts of `[32, 8, 8, 32, 8, 8, 8, 8, 8, 8]`: AddrMan, AddrMan', peers.dat's name, BanMan, nSeed1, nSeed0. Without the store's temp-name draw, that is the dash-p2p side, 9 draws.
- **Host-side draws.** D58's attribution is correct: the ECC 32 + 32 + 8 and the script-cache nonce happen before `LoadAddrman`, in the host. It also correctly says a shared-stream harness must draw them, and the peers.dat temp name, first.

## New findings

### Minor

#### R-m1: D43 inverts the m5 residual. Ours now matches an idle Core but not a busy one, which is the mainnet norm.

- **Location:**
  - D43's last sentence ("Residual: … this moves only when an idle linger starts and ends within the 500 ms window").
  - `SWEEP_INTERVAL`'s doc (`netcore.rs:48-53`).
  - The sweep's wake sites. Reader close and error paths are at `netcore.rs:1570`, `:1591` and `:1602`. Linger readability is at `:1646` and `:1658`. A marked peer's flush is at `:1679` and `:1688`. Eviction is at `inbound.rs:285`, and `interrupt` at `netcore.rs:1121`.
  - An ordinary read on any peer, and a push, wake nothing.
- **Core:**
  - `ThreadSocketHandler` runs `SocketHandler` and then `DisconnectNodes` in a loop (net.cpp:2652-2667).
  - `SocketHandler` returns from `WaitManyInternal` on **any** socket's event (net.cpp:2423). That includes every peer's receive readiness and the wakeup pipe, which `PushMessage` writes on every push to an empty queue while the loop waits (net.cpp:4896-4904).
  - So on a node whose peers exchange traffic, passes run every few ms, and a mark's linger starts, with `SHUT_WR`, within ms.
- **Probe** (`tests/zz_probe_busy.rs`, run and deleted):
  1. Peers A and B are attached.
  2. `disconnect(A)`.
  3. Inject 4 bytes, a partial V1 header, on B. That is a receive event that closes nothing.
  4. Result: `linger_started_after_other_peers_read=false`. Core's loop would have run a pass.
- **Scenario:**
  - A peer that can repeatedly trigger a non-discouraging disconnect of itself (for example an obsolete `version`, which sets `fDisconnect` with no push, net_processing.cpp `on_version`) measures message → FIN.
  - Core on a connected mainnet node gives a few ms to tens of ms, set by other peers' traffic. Ours is U(0, 500 ms), mean ≈ 250 ms.
  - A few dozen trials separate the two.
  - On an idle node, such as regtest with one peer, ours now matches Core, and r3's wake-on-mark did not.
  - The fix moved the match from one regime to the other. The removal time is still matched wherever it is visible (unread or late bytes make the socket readable, and then the deadline applies).
  - This is timing-only and statistical, hence minor. But the brief asks whether "the idle-node residual" is acceptable, and the residual is not idle-node.
- **Fix (before merge, one of):**
  - **(a) Preferred: add Core's two remaining pass triggers.** Call `sweep_wake.notify_one()` after every successful read in `reader`, and on a push that wakes an idle writer (`NetCore::push` when `wake`). `Notify` already coalesces wakes, so the sweep runs at most once per scheduling round, which is Core's "a pass per readiness batch". The idle cadence (500 ms) is unchanged. Add a test that is this probe with the opposite assertion (B's read starts A's linger before `PASS`), plus a push variant.
  - **(b) Rewrite D43's residual** to say: "on a node with peer traffic Core passes on every receive event and every push to an idle queue, so a mark's FIN lands within ms; ours waits for the next 500 ms pass or a listed event". Then have the manager rule on accepting it, and register it in BACKLOG.

  Either way, correct D43, because the residual it describes is the opposite of the one that exists.

### Nits (BACKLOG unless noted)

- **R-n1, the TCP_NODELAY log text for accepted sockets (implementer's list).** BACKLOG. `TcpSocket::new` logs `CreateSockOS`'s text ("Unable to set TCP_NODELAY on a newly created socket, continuing anyway") for both directions. For accepted sockets Core logs `"connection from %s: unable to set TCP_NODELAY, continuing anyway"` (net.cpp:1927-1929). It is debug log only and invisible to peers.
- **R-n2, `parse_banlist` duplicate keys and `-0` (implementer's list).** BACKLOG.
  - **Duplicate keys.** UniValue keeps duplicate keys and `operator[]` returns the **first**. `ReadSettings` refuses a top-level duplicate key ("Found duplicate key …") and so recreates the file. `serde_json::Value` keeps the **last** and refuses nothing.
  - **`-0`.** `-0` may parse as a float in `serde_json`, so `as_i64` fails and the file is refused, where Core's `getInt` reads 0.
  - Both affect only a hand-edited local file; nothing is on the wire.
  - Seed both shapes into the `coreimport_banlist_json` corpus, so the differential pins Core's verdicts.
- **R-n3, getaddrinfo numeric forms such as `127.1`, `010.0.0.1` and `0x7f.1` (implementer's list).** BACKLOG.
  - **`open`.** `pre_lookup` uses Rust's `IpAddr` parser, which rejects `inet_aton` forms, so such names go to the host `Resolver`. With a getaddrinfo-backed resolver (the node's), the result equals Core's, including under `-dns=0` (`AI_NUMERICHOST` still accepts them).
  - **`lookup_numeric`.** It never calls the resolver, so `-addnode=127.1` is matched by connection name rather than by `ip:port` in `GetAddedNodeInfo` (net.cpp:3308-3366). An inbound connection from 127.0.0.1, or an outbound one opened by address, is then not counted as connected. This is config-driven and local.
  - **Fix.** Port `inet_aton`'s numeric forms into `pre_lookup`, or document the resolver contract.
- **R-n4 (BACKLOG).** If a lingering peer holds unread bytes and then closes after our `SHUT_WR`, Core's `EPOLLHUP` puts it in `node_err_set`, and `SocketRecvData` reads up to 64 KiB (net.cpp:2522-2529, 2595-2627). The peer is still removed at the deadline. Ours only peeks (`linger_watch`), so the final `close` sends RST where Core's may not. The far end has already closed both directions, so its application cannot see this; only a packet capture can.
- **R-n5 (BACKLOG).** While one lingering peer is readable, Core spins with a zero timeout, so a mark on *another* peer starts its linger within ms. Ours starts it at the readable peer's deadline (≤ 100 ms later). This is subsumed by R-m1 (a).
- **R-n6, doc.** D46's Core rationale is right. In D15 the `discourage(peer)` wording still reads as an id; say "handle (D46)".
- **R-n7, log text.** For an unopenable `banlist.json`, Core's text is `Cannot load banlist <path>: <path>. Please check permissions.` (from `ReadSettings`). Ours prints the io error. Log only.

## Regressions in the new code

I found none beyond R-m1.

- **`interrupt`.**
  - Readers, writers and `linger_watch` all return once `interrupted()`.
  - `interrupt` wakes every peer's reader and writer.
  - `attach_inner` refuses after it under the table lock, and the late-connect path releases the grant and finalizes.
  - Sockets stay open until `shutdown` closes them, as Core's `StopNodes` does after `interruptNet`. `dump_bans` now runs on every shutdown, after the I/O tasks join.
- **`NetSocket::poll_peek`.**
  - `TcpSocket` uses tokio's `poll_peek` on a 1-byte buffer. `MemSocket` returns `Ok(1)`/`Ok(0)`/`Pending` with a registered waker.
  - `linger_watch` never spins on a ready peek: after marking the peer readable it awaits `reader_wake`, which `close_socket` and the sweep's `SHUT_WR` notify.
  - `close_socket` notifies both tasks, so no reader outlives the removal holding the socket `Arc`. A `mark_disconnect` wakes a reader parked in the pause wait, so a paused peer also enters `linger_watch`.
- **`InitError::RenamePeers`.**
  - It is reached only on `is_unsupported_version` with a failed `backup`, which is Core's only fatal store branch.
  - The `Display` text is Core's untranslated string, and `source()` exposes the io error.
  - `CorruptPeers` is unchanged for every other read error.
- **`Cargo.toml`.** The `libc` dependency moved to `cfg(unix)` and `is_transient` to `#[cfg(unix)]`. On macOS that adds `EMSGSIZE`/`EINPROGRESS` as transient (Core's `WSAEMSGSIZE`/`WSAEINPROGRESS`, net.cpp:1756), which is right.

## Validation

- **Environment.** Every command ran under `nice -n 10`, `CARGO_BUILD_JOBS=8`, empty `RUSTC_WRAPPER` and `TMPDIR=/work/scratch/sb6-recheck/tmp` (0700), with its own target directory. Load was 23–40.
- **`cargo test -p dash-p2p --all-features -- --test-threads=4` at 3f991ee8d: 238 passed, 0 failed.** This matches the implementer's count. By binary:
  - lib 142, addr_ping 10, asmap 5, banlist_json 5, bans 3, disconnect 8, eviction 1;
  - gates 2, grants 2, handshake 17, headers2_mru 1, interrupt 2, policy_read 5, policy_table 3;
  - rand_source 10, send 11, shutdown 3, sim 1, stdsort 1, store_errors 3, task_panic 3.
- **`cargo fmt --check -p dash-p2p`:** clean.
- **Mutations.** 18 mutants, all killed (tables above). Logs are in `/work/scratch/sb6-recheck/mut/`, and the runner is `mutate*.py`.
- **Banlist goldens.** dashd rc.3 regeneration is byte-identical (`/work/scratch/sb6-recheck/banlist/`).
- **The implementer's gate.** I read `/work/scratch/sb6/logs/gate-3f991ee8d.log`:
  - `GATE EXIT 0`, with `tests passed 3775 failed 0`;
  - `fuzz-smoke` legs=27 pass=27, with the 8 public targets (kv_model, five coreimport targets, p2p_transport_model, p2p_gate_model) at 0 divergences and `rust_dashcore_dirty=False`;
  - `fuzz-unit` 18 passed;
  - clippy, deny, pins and quirks all green.
- **Not done:**
  - I did not re-run the workspace gate, clippy or the dashd suites.
  - There was no fuzz campaign.
  - I did not run the P2a-core or P2a-net re-merge against the new `discourage(&peer)` API.

Scratch: the target directory and the worktree are deleted after this report. The logs, probe scripts and regenerated goldens stay under `/work/scratch/sb6-recheck/`. I made no reads under `~/private`, and no Dash Core security issue was found.

## Verdict

Every prior finding is closed by mechanism: M1, M2, m1–m4 and m6–m12, the nits, and the O1c draw note. Each one I mutated is killed by a behavioural test, and m8 holds against a freshly dashd-written file. m3's Core-literal order is right.

The one open item is **R-m1**. m5's mechanism is fixed, but D43 describes the residual backwards: ours now differs from a *busy* Core, the usual case, not an idle one. That has to be corrected before merge, preferably with (a), the two missing pass triggers (about two lines plus a test), and otherwise by rewriting D43 and getting a manager ruling. The rest is BACKLOG nits. Option (a) needs only a one-test spot check, not another review round.

**Verdict:** APPROVE-WITH-NITS
