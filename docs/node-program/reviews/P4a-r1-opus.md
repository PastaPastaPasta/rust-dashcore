# P4a inbound policy review, round 1 (Opus-h, full range)

**Verdict: ACCEPT WITH NITS.** I reviewed `git diff a5d2a9c59..d19e91ef2` (`node/p4a-inbound-policy`) in a detached worktree at `/work/scratch/p4a-r1-opus/wt`, against Dash Core v24.0.0-rc.3 `0364ed626e`. I spent most of the effort on the scope the brief set: acceptance and eviction, bans, P2P-12, `-capturemessages`, the evidence and safety.
- I found no parity defect in the code P4a adds or exercises.
- The four findings are evidence and handoff gaps, all P3 or nit.
- One possible Core-side issue is in the private candidate file (item 5: see manager).
- I made no branch edits, commits or pushes. The worktree was restored clean after every mutant.

## Findings

1. **P3 (evidence): `inbound-bans` does not pin the ban-expiry boundary; only SB6's unit tests do.**
   - Core: `BanMan::IsBanned(addr)` and `IsBanned(subnet)` hold a ban while `current_time < nBanUntil` (banman.cpp:98-124). `SweepBanned` keeps an entry while `now <= nBanUntil` (banman.cpp:201). So at `now == nBanUntil` the address connects but `listbanned` still lists it, with `time_remaining` 0.
   - The node is right: `dash-p2p/src/ban.rs:90-92`.
   - The gap: the fixture bans 13.9.0.1 for 60 s at 1700000000 (`inbound-bans.txt:76`). Its only later probe is `time 1700000061` (`:80`, `capture.py:280`), one second past the boundary.
   - My mutant R4 (`now < e.ban_until` → `now <= e.ban_until` in `is_banned`) passes every P4a replay. It is killed only by `ban::tests::ban_semantics` and `sweep_keeps_until_after_expiry` (`ban.rs:274`, `:290`).
   - Schedule that would pin it against dashd: `setban 13.9.0.1 add 60 0` at T, then `time T+60`, `accept 9 from 13.9.0.1` (dashd accepts it), then `banlist` (dashd still lists the entry).
   - Suggest adding that probe to the design (or a D-row saying the boundary is unit-pinned only). No code change is needed.

2. **P3 (host handoff, D4): `-blocksonly` soft-sets `-whitelistrelay=0`, and nothing records that for the ConnMan-wiring row.**
   - Core: init.cpp:1058-1061 makes `-blocksonly=1` soft-set `-whitelistrelay=0`, before the implicit flags read it (net.cpp:1886-1891). init.cpp:1068-1070 adds `-whitelistforcerelay=1` → `-whitelistrelay=1`. That second one changes no flags, because `ForceRelay` carries the Relay bit (net_permissions.h:24-26).
   - Rust: `NetConfig` has independent `blocks_only` and `whitelist_relay` fields, with `whitelist_relay: true` by default (`dash-p2p/src/netcore.rs:130,136,216`). D4 hands the option parsing to the row that wires ConnMan into rdashd, but it lists no parameter interactions.
   - Schedule: `-blocksonly=1 -whitelist=11.2.0.0/16`, peer from 11.2.0.5.
     - Core: permissions `noban, mempool, download`. `IsBlockRelayOnly()` is true (net.cpp:554-560), so the peer's txs are ignored.
     - A host that sets only `cfg.blocks_only = true`: the node adds `relay` and treats the peer as tx-relaying.
   - Suggest one line in D4 (or the `whitelist_relay` doc) naming the interaction for the wiring row.

3. **P3 (scope gap, D6): `cleardiscouraged` has no node half.**
   - Core registers it as a hidden RPC (rpc/net.cpp:896-915, `:1262`). `BanMan::ClearDiscouraged` (banman.cpp:82-90) does two things:
     - it resets the filter, which draws a fresh `nTweak = GetRand<unsigned int>()` (common/bloom.cpp:385-391);
     - it sets `m_is_dirty`, so the next `DumpBanlist` rewrites `banlist.json`.
   - The PRD lists it as MUST (PRD.md:1292, the functional-test hooks). P4a's own framing is "bans and discouragement end to end", but D6 covers only setban, listbanned and clearbanned, and `BanList` has no way to clear the filter. SB6 D38 says "`cleardiscouraged` … not ported".
   - Schedule: a peer from 15.5.0.1 is discouraged (+100), then the operator runs `cleardiscouraged`.
     - Core accepts 15.5.0.1 into a nearly full table (`inbound-bans.txt`'s peer-13 case).
     - The node has no call to give it; and on the dump timer Core rewrites `banlist.json` while the node does not.
   - Suggest either `BanList::clear_discouraged` (fresh tweak through `RandSource`, dirty flag) plus `NetCore::clear_discouraged`, or a D6 sentence naming its owner (RP or SB6).

4. **Nit (D10): a failing capture write logs one `warn` per message, with no latch.**
   - `dashd-net/src/capture.rs:64-66` logs every failed `create_dir_all`, open or write.
   - With the datadir unwritable (permissions, or a full disk while logs go elsewhere), a peer sets the warn rate by sending ordinary messages. A `ping` costs it 8 bytes per line.
   - Core does not continue at all (D10), so this is the node's own policy. A warn-once latch per peer and direction, or a debug level after the first, would do.
   - The capture also does blocking `create_dir_all` + open + write on whatever thread calls `push` or `pop`. Core does this synchronously too, and it is an operator debug option, so I mention it only for the record.

5. **See manager**.
   - It is a Core-side observation reached while checking D17.
   - The branch reproduces Core exactly there, so nothing needs to change in P4a.

## Checks

### Inbound acceptance and eviction parity (D3, D11, D12)
- **`CreateNodeFromAcceptedSocket`** (net.cpp:1875-1971) against `NetCore::accept` (`dash-p2p/src/inbound.rs:316-400`), step by step:
  - the CJDNS flip of `addr` and `addr_bind` (net.cpp:1861-1867);
  - `AddWhitelistPermissionFlags` (net.cpp:548-552), ORing every matching subnet;
  - `Implicit`: ForceRelay if `-whitelistforcerelay` (default 0), Relay if `-whitelistrelay` (default 1, net.h:67-69), then Mempool and NoBan;
  - the inbound count, including peers already marked `fDisconnect`, and the verified-masternode count, which is not gated on `m_active_masternode`;
  - `fNetworkActive`;
  - banned unless NoBan;
  - discouraged refused when `nInbound + 1 >= nMaxInbound` unless NoBan, using the unsubtracted count;
  - the eviction loop on `nInbound - nVerifiedInboundMasternodes >= nMaxInbound`, with a local decrement;
  - the unsynced-masternode refusal after eviction;
  - `NODE_BLOOM` for BloomFilter;
  - `prefer_evict = discouraged`, even with NoBan.

  Every step matches.
- **Slot formula:** `nMaxInbound` uses Core's unclamped `m_max_outbound = full + block + feeler` (net.h:1263-1269). The node clamps it (`inbound.rs:340-343`, copied from the `semOutbound` clamp at net.cpp:4042). At `-maxconnections ≤ 10` that makes Core −1 and the node 0. I traced both: neither can admit an inbound peer from an empty table (`0 ≥ −1` and `0 ≥ 0` both reach "no candidate"). Divergence needs a verified inbound masternode to already be present, which neither side can admit. So the difference is unobservable, and not a finding.
- **`GetEvictionCandidates`** (net.cpp:1775-1820) against `eviction_candidates` (`inbound.rs:235-270`): the `fDisconnect` skip, and the active-masternode 1 s protection lifted by a first non-MNAUTH message. That first message is set after verack (np:4363-4371). A verified proRegTx is always protected. The fields match.
- **`SelectNodeToEvict`** and `ProtectEvictionCandidatesByRatio` (node/eviction.cpp:178-239, :105-176) against `inbound.rs:55-226`: every comparator, the protection order and counts (NoBan, outbound, netgroup 4, ping 8, tx 4, block-relay-only 8 with its predicate, block 4, then ratio), and the disadvantaged-network array order CJDNS, I2P, localhost, onion.
  - The `std::stable_sort` matches Rust's stable `sort_by_key`.
  - The early `break` comes before `n.count -= delta`.
  - `remove_if` on the tail keeps the survivors' order.
  - `prefer_evict` filtering, then largest netgroup with the youngest-group tie on `group[0].m_connected`.
- **Permissions** (net_permissions.cpp/.h against `dash-p2p/src/permissions.rs`, `conn.rs:83-103`):
  - the flag bits, with ForceRelay ⊇ Relay and NoBan ⊇ Download;
  - `HasFlag` as all-bits;
  - the first `@`; an offset always at `@+1` (I traced Core's `readen` arithmetic for `,@`, `,,@`, `a,@`);
  - empty entries; `all`; `ToStrings` order; the error texts.
- **`evict_model.py` is an independent oracle for the fixture design, not a re-derivation of Rust.**
  - It splices Core's own `src/node/eviction.cpp` into a g++/libstdc++ harness and runs `SelectNodeToEvict` under all 24 netgroup-key orders.
  - Run here on the extracted v24.0.0-rc.3 tree, it exits 0 and predicts 30, 29, 32, 28, 26: exactly dashd's evictions in `inbound-evict-groups`.
  - The Rust is then judged against dashd's transcript, not against the model.

### Bans (D2, D6, D17, D20, D23)
- **`setban`** (rpc/net.cpp:743-826) against `dashd_net::bans::set_ban` (`dashd-net/src/bans.rs:40-87`):
  - a `/` selects a subnet;
  - `LookupHost(.., false)` plus the CJDNS flip plus `IsValid`, or `LookupSubNet`;
  - `IsBanned(addr)` (covering) for a single address and `IsBanned(subnet)` (exact) for a subnet;
  - "already banned" before the absolute-in-the-past check;
  - `Ban` normalizes an offset ≤ 0 to `-bantime`, relative;
  - `m_banned[sub_net]` default-inserts (`entry().or_default()`) and keeps the longer ban;
  - it dumps only on change, then `DisconnectNode` by subnet whatever the permissions (net.cpp:4587-4604; `netcore.rs:739-756`);
  - `Unban` by `CSubNet(addr)`.
- **`listbanned`, `clearbanned` and the legacy warning:** `listbanned` sweeps first and keeps `CSubNet` order. `clearbanned` sets dirty and dumps an empty list. The legacy `banlist.dat` warning matches addrdb.cpp:153-157, including the order and the `LogPrintf` level.
- **Scope ids:** they are ignored by `NetAddr` Eq, Ord, Hash and `SubNet::matches` (netaddress.cpp:610-617). BTreeMap's `entry` keeps the first key as `std::map::operator[]` does, so `fe80::1%7` and then `fe80::1%9` keeps `%7` on both sides.
- **No path admits a banned peer or refuses an unbanned one:** inbound checks the flipped address. The discouragement filter keys on `GetAddrBytes`, which carries no scope. NoBan bypasses both checks, as in Core. Dials are SB6's and unchanged.

### P2P-12 (D7, D8)
- **The `solicit` module** against np:90-111, 1593-1631, 6876-6881 and 6925-6957, and `unordered_lru_cache.h`:
  - six types and the 5 s / 60 s intervals;
  - `UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE` 10;
  - the grace `≤ 2 × interval` of the *requested* type;
  - the type taken from the request;
  - the entry erased on every outcome, including the tracker-hit path;
  - truncation past 512 to the 256 most recently used, with `insert` refreshing the access counter. The `nth_element` and unique counters keep exactly 256, as the Rust sort plus threshold does.
- **The `qfcommit` arm** (`stubs.rs`) against llmq/blockprocessor.cpp:100-119 and np:5783-5790: decode first, with a throw unscored; then `CInv{MSG_QUORUM_FINAL_COMMITMENT, ::SerializeHash(qc)}`; +10 "unrequested quorum commitment" when UNREQUESTED. `now_us` is the node's mockable clock (`logic.rs:735`).
- **The canonical hash after decode:** I confirmed in bls/bls.h:100-131 that an invalid point re-serializes as zeros. `sim_m2`'s three-way test pins that the wire bytes, trailing bytes and canonical bytes all spend the request, and that the canonical bytes score 10 unrequested.
- **Ordering:** the gate runs on the single processing thread, with `PeerState` held mutably. Core runs CoinJoin's `processMessage` and `CMNAuth::ProcessMessage` first, and both ignore `qfcommit`. Pre-verack messages return before dispatch (np:4363-4366). I found no race with the M2 handlers.

### `-capturemessages` (D10, D16)
- **The record** matches `CaptureMessageToFile` (net.cpp:5022-5050): i64 LE µs from the mockable clock, the type NUL-padded to 12, u32 LE size, the payload, `ab` append.
- **The path** is `GetDataDirNet()/message_capture/<ToStringAddrPort, ':'→'_'>/msgs_{recv,sent}.dat`.
- **The sites** are `PushMessage` before queueing (net.cpp:4862-4864) and after `PollMessage` before `ProcessMessage` (np:5921-5923). `NetCore::push` and `pop` are the only callers of `push_msg` and `poll_message` in the three crates, so no path bypasses the hook.
- **No writes outside the datadir:** the directory names come from IPv4, IPv6 (`[..]`, `%N`), onion and I2P text, so they cannot hold `/` or `..`.
- **Symlinks** are followed exactly as Core's `create_directories` and `fopen` do. That needs local write access to the datadir.
- **Disk full** gives a torn record, as Core's `AutoFile` does, then item 4's warn.

### Safety (DEC-246, DEC-249)
- `#![deny(unsafe_code)]` in `dash-p2p/src/lib.rs:31`.
- The only `#[allow(unsafe_code)]` items are `lookup::numeric_host` (`lookup.rs:18-62`) and the `#[cfg(test)]` `if_nametoindex` (`:209-210`). `dashd-net` and `dashd-p2p` stay `forbid(unsafe_code)`.
- **I re-checked `numeric_host` against `WrappedGetAddrInfo` (netbase.cpp:44-95):**
  - `CString` refuses interior NUL. Core's `c_str()` would truncate there, but every caller checks `ContainsNoNUL` first, and so do `lookup` and `lookup_host`.
  - The zeroed hint is valid, and its fields are Core's.
  - On failure there is no free, and `res` is unset.
  - Each node is read only while the list lives.
  - Lengths are checked before `read_unaligned`.
  - `s_addr.to_ne_bytes()` gives the bytes in network order.
  - The IPv6 address goes through `from_legacy_ipv6` plus the scope id, as in `CNetAddr(in6_addr, scope)`.
  - The first IPv4 or IPv6 answer is taken, as Core's `maxSolutions` is 1.
  - `freeaddrinfo` runs exactly once. A panic in between could only leak.
  - `getaddrinfo` is MT-safe and makes no DNS query under `AI_NUMERICHOST`.
- No soundness issue.

### Evidence quality and fixtures
- **D21 mutants:** M1–M6 are reasonable and match their D-row descriptions.
- **The capture tool and fixtures carry no host-private data:**
  - The operator key is redacted as `<operator-key>`.
  - Addresses come from the private namespace (11.x–17.x, 127.0.0.1).
  - The proTxHashes are regtest values.
  - No `/work` or `/home` path appears in any fixture or `PROVENANCE.md`.
  - The tool's `P2A_CAPTURE_DIR` default `/work/scratch/p4a/capture` follows the P2b tool's convention and can be overridden.

### Runs
All runs used `CARGO_TARGET_DIR=/work/scratch/p4a-r1-opus/target` (0700), `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, at load 40–60. There were no wall-clock failures, and `sim_dial::connect_only` passed. I did not rerun the gate.

| Command | Result |
|---|---|
| `cargo test -p dash-p2p --features inbound` | pass (150 lib + all integration suites) |
| `cargo test -p dashd-net` | pass (11 lib, `dashd_inbound_policy` 3, `runtime` 5, `sim_dial` 11, and the rest) |
| `cargo test -p dashd-p2p --test dashd_inbound_transcripts --test sim_m2` | pass (13, 45) |
| `cargo test -p dashd-p2p --lib` (`solicit`'s unit tests live here; the brief's command does not run them) | pass (26) |
| `cargo test -p node-l1 --features oracle --test p2p_node` (oracle pin `5c9e9117a2`, from `/work/cache/oracle`; includes D9's `notfound_limit_is_v24s`) | pass (16) |
| `python3 evict_model.py <v24.0.0-rc.3 src>` | exit 0, predicts dashd's five evictions |

### Reviewer mutants
`/work/scratch/p4a-r1-opus/mutate.py` (kept with its logs under `logs/mutants/`; the target and worktree are deleted) applies one mutant, runs the three suites, then restores the file with `git checkout`. Each mutant below compiled, and each "killed" means a test failed.

| Id | Mutant (Core reference) | Result |
|---|---|---|
| R1 | Verified inbound masternodes counted towards the limit (net.cpp:1953) | **killed**: `inbound_evict_mn` (transcript) and `accept_does_not_count_verified_masternodes` |
| R2 | Netgroup tie goes to the oldest group (eviction.cpp:228) | **killed**: `inbound_evict_groups` (transcript) and `matches_core_eviction_trace` |
| R3 | No netgroup protection (eviction.cpp:188) | **killed**: `inbound_evict_21`, `inbound_evict_groups`, `inbound_evict_mn`, `peer_eviction_test`, `protections` |
| R4 | Ban holds at `now == banned_until` (banman.cpp:106) | **killed only by unit tests** (`ban_semantics`, `sweep_keeps_until_after_expiry`); every P4a replay passes (finding 1) |
| R5 | Discouraged refused only when completely full (net.cpp:1942) | **killed**: `inbound_bans` (transcript) and `accept_refuses_discouraged_when_nearly_full` |
| R6 | `qfcommit` gate treats every commitment as requested (np:5788) | **killed**: `inbound_qfcommit` (transcript) and `qfcommit_answers_its_request_by_the_reserialized_hash` |
| R7 | `setban` on an address checks the exact entry, not a covering ban (rpc/net.cpp:791) | **killed**: `setban_and_listbanned_are_cores` |
| R8 | Implicit whitelist flags never get Relay (net.cpp:1889) | **killed only by unit test** `accept_implicit_whitelist_flags`; Relay has no M2 effect, so no transcript can see it (D5 leaves relay to P5) |
| R9 | LRU truncation keeps 257 (unordered_lru_cache.h `truncate_if_needed`) | **killed**: `the_cache_truncates_to_the_most_recent_256_past_512` (dashd-p2p `--lib`) |
