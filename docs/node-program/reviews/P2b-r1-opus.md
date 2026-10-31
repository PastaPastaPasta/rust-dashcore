# P2b inbound P2P — Opus r1 review (second reviewer, P2P)

Range `d00ec3062..35499e285` (6 commits) on `node/p2b-inbound`. Core: v24.0.0-rc.3 (`np:` = `src/net_processing.cpp`).

**Verdict: ACCEPT WITH NITS.** I found no behaviour a peer could use to tell this node from dashd, and no DoS or amplification that Core lacks. All findings are Low or nits. None blocks merge.

## What I checked against Core

1. **Serving (`serve.rs` vs np:4760-4855).**
   - Read, then locator > 101 disconnect, then `LoadingBlocks` ignore, then the min-work empty reply: same order as Core.
   - The empty reply's type follows the peer's bit 11 (`UsesCompressedHeaders`, np:1196-1199, np:4790), not the request. It is skipped with `Download`, and `NO_BAN` implies `Download` as in Core.
   - Null locator: `LookupBlockIndex(hashStop)`, then `BlockRequestAllowed`. Otherwise `FindForkInGlobalIndex` + `Next`.
   - v24's `FindForkInGlobalIndex` (validation.cpp:182-197) has no "locator entry descends from tip → return tip" clause. The port correctly lacks it too.
   - `collect` gives the same entries as Core's forward `Next` loop in every case:
     - a stop below the start never matches;
     - a stop off the active chain never matches;
     - a stop equal to the start sends one header;
     - a null-locator start off the active chain sends that start alone (`Next` is null; the stop check breaks first);
     - an exact-limit run ending at the tip.
   - Its `pindex` result gives the same `pindexBestHeaderSent` as Core in all those cases, including the unchanged state on the unknown-stop and min-work returns.
   - The limit uses `GetHeadersLimit(pfrom, msg_type_internal == HEADERS2)`, i.e. the request type and common version, with `>= 70235` (np:1210-1216).
   - The `headers2` reply keeps the first header out of the version list, as `CompressibleBlockHeader::Compress` does when `v_headers` is empty (np:4848-4853; primitives/block.cpp:57-93). base-sdk's `compress(record_first=false)` matches this, including Core's uint32 time-diff wrap.
   - Trailing bytes are ignored (`Trailing::Ignored`), as in Core.
2. **`BlockRequestAllowed` (np:2048-2055).** All three clauses read the best header, not the tip. `GetBlockProofEquivalentTime(best, pindex, best)` (chain.cpp:147-162): `r * arith_uint256(spacing)` wraps at 2^256, then `/ GetBlockProof`, then saturates at bits > 63. The port matches, and `proof_equivalent_time` pins the wrap.
3. **D6 snapshot vs live reads.** The active-chain set, tip and best header all come from one snapshot, and entries are immutable. So the only mixed read is a `valid_scripts` bit raised by a `set_tip` after the snapshot. Any reply that bit enables is the reply Core gives when it processes the message at that later `set_tip`, when the block is on the active chain. Invalidation only ever turns serving off. I found no interleaving that serves a header Core would never serve. I agree with D6 and with Sol.
4. **Eviction (SB6 `inbound.rs` vs node/eviction.cpp:16-239 and net.cpp:1775-1846, 1880-1960).** I compared every comparator line by line, plus the protection order (noban, outbound, 4 netgroup, 8 ping, 4 tx, 8 block-relay-only with its predicate, 4 block, ratio). Also checked:
   - `remove_if` keeps order;
   - the network list uses `stable_sort`;
   - "most connections, youngest group" iterates in vector order;
   - the masternode protection and the `nVerifiedInboundMasternodes` exclusion;
   - `m_min_ping_time` starts at max.

   **D3** is correct. For N ≤ 10, Core's limit is −1 and SB6's is 0. Both make `nInbound + 1 >= limit` and `nInbound - verifiedMN >= limit` true for every count, so both evict until nothing is left, then drop. **D4**'s ownership map matches net.cpp:1802-1817 and np:4059-4084.
5. **Versions and gates** are SB6's code; P2b supplies the evidence. `inbound-versions` covers:
   - obsolete 70220 (MIN_PEER_PROTO_VERSION is 70221, version.h:20);
   - 70222, 70234, 70235 and 70242;
   - `NODE_NONE`, `NODE_BLOOM`-only, the masternode flag;
   - a `version` cut after `addr_recv`;
   - a 257-byte user agent that throws, after which a later good `version` works.

   **D10**'s reasoning holds: with V2 on, `ProcessReceivedMaybeV1Bytes` treats a non-`version` first message as V2 garbage.
6. **Tests run here** on a clean `git archive` of 35499e285: `cargo test -p dashd-p2p --test sim_serve --test dashd_inbound_transcripts --test dashd_transcripts` gave 11 + 7 + 11 passed. I did not rerun the node-l1 O1c test; Sol reports it passing.

## Findings

1. **Low — a peer message can now panic on a broken `ChainAccess` contract.** `dashd-p2p/src/processing/serve.rs:79` and `serve.rs:172`.
   - `entry_at` and `active_run` call `expect` on `ancestor(snap.tip, h)` and on the `prev` walk. Any peer reaches these: an all-unknown locator falls back to height 0 (`find_fork_in_global_index`, `serve.rs:71`).
   - These are the processing layer's first panicking `ChainAccess` reads on a peer path; `sync.rs` only compares with `== Some`. `SimChain`, and a full node with floor 0, meet the invariant.
   - But `seams.rs:133` does not state it, and SB4's `AnchoredAncestry::ancestor` has a `BelowFloor` error. A host adapter that maps that error to `None` would let any peer kill the message-handler thread with `getheaders`. Core has no equivalent.
   - **Fix:** state on `ChainAccess::ancestor` and `entry` that they are total for heights `0..=snap.tip_height` on the snapshot tip and for `prev` of an active entry. Or return `Ok(())` without a reply instead of `expect`. Docs-only is enough for M2.
2. **Low (evidence) — eviction fixtures cannot vary netgroup or control ping.** `contrib/node/p2b-transcripts/capture.py:217-241` and `tests/fixtures/dashd-v24/inbound-evict*.txt`.
   - Every peer is 127.0.0.1, so all candidates share one keyed netgroup and are `m_is_local`. The netgroup protection and the "netgroup with most connections" choice are therefore only tested on ties, and the localhost ratio slot is always in play.
   - The 8-by-ping protection compares dashd's real wall-clock latencies with the sim's virtual pongs. The recorded choices (28, 27, 31, 32) agree, but nothing makes them agree by construction.
   - Combined with SB6's unit tests this is fine for M2. Mark it as a known gap in D12.
   - **Follow-up (optional):** capture from distinct non-local addresses (e.g. a dummy interface with 10.x/172.x addresses, using sudo on agentbox) so netgroup diversity reaches dashd.
3. **Nit — `inbound-evict-21` cannot be regenerated by the committed tool.** `capture.py:224` hard-codes `max_inbound = 29`. PROVENANCE says the 21-slot fixture came from an earlier tool revision. **Fix:** make `max_inbound` a parameter and add an `inbound-evict-21` job, so both fixtures come from the committed tool.
4. **Nit (evidence) — two version boundaries are covered only by unit tests.** Serving at exactly 70235 (the first version with the 8000 limit) and the handshake at exactly 70221 (the lowest accepted version) are not in a Core fixture. `inbound-versions` peer 2 is at 70235 but asks for no headers. `headers_limit` uses `>=` as Core does. **Fix (optional):** make `inbound-versions` peer 2 send one `getheaders2`, and add a 70221 peer.
5. **Nit — `BlockRequestAllowed`'s off-chain branch has no Core-generated evidence.** It is checked only in `sim_serve.rs:183-226`. dashd can produce a once-connected side block (`invalidateblock` X, mine, `reconsiderblock` X) and a headers-only one (`submitheader`). That would pin "served" against "ignored" before P3 makes the branch reachable. A follow-up is fine; it is unreachable at M2 (D6).

## On Sol's nit (D9, the 8000-header `getheaders2` edge)

**I agree it is a real evidence gap, and I agree it is Low. It should not block merge.** What already pins the edge:

- **The constant.** `headers2-8000` has dashd treating an 8000-entry `headers2` batch as full and asking for more. That is `GetHeadersLimit(pfrom, true) == 8000` at 70242, and np:4819 calls the same function when serving.
- **The branch.** `inbound-serve` has dashd sending all 2100 headers for `getheaders2` at 70242 (`fd3408`), byte-identical to the node's. That proves the raised-limit branch is taken and the compressed encoding matches over a long run.
- **The edge.** It is then a plain `--nLimit <= 0` loop, which `sim_serve.rs:128-143` asserts with a literal 8000.

One correction to D9's rationale: the DIP0003/CbTx obstacle applies only to the O1c route. A dashd capture mines its own CbTx coinbases, so the only cost is fixture size (about 1.4 MB of `chain` lines plus about 0.6 MB of reply). If the manager wants byte-exact Core evidence at the edge, add an 8001-block `inbound-serve-8000` dashd fixture as a follow-up.

Possible security note: none.
