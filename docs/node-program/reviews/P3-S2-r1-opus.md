# P3 S2 review: Opus-h round 1

**Head:** `node/p3-s2` at `d714bac05`. The range is `1e16158ef..d714bac05`. The code is at `dcedde7b7`, and the rest of the range is docs.
**Core reference:** `0364ed626e` (`v24.0.0-rc.3`). `np` means `src/net_processing.cpp` at that commit. It was extracted with `git show`, because `~/workspace/dash` HEAD is `15db478750`, not the pin. The line numbers in the brief match the pin.
**Prior rounds:** Sol r1 found F1 (clock origins) and N1 (log order) and raised one nit. Sol r2 returned ACCEPT. I did not repeat their audit or their mutations. This round covers the brief's six focus areas and runs 16 new mutants.

## Findings

### T1 (Minor, test gap): no test covers direct fetch of an equal-work header

**Port:** `dashd-p2p/src/processing/download.rs:597` (`cx.snap.tip.tip_work <= last.chain_work`). **Core:** np:3209 (`m_chainman.ActiveChain().Tip()->nChainWork <= last_header.nChainWork`).

The code is correct. Its tie case has no test. If `<=` becomes `<`, all of `download.rs`'s units, `sim_blocks`, and both P3 transcripts still pass (mutant M8 below).

The tie is visible on the wire. Suppose a peer announces a sibling A′ of the tip A, with the same work. Core then sends `getdata(A′)` from the headers handler at once, and a node using `<` sends nothing. In the existing scenarios, section 12 would mask the difference: `UpdateBlockAvailability` takes an equal-work best-known block (`>=`), and `FindNextBlocksToDownload` then also schedules A′. So the test must use a peer that section 12 never asks. A peer without `NODE_NETWORK` and without `NODE_NETWORK_LIMITED` works, because Core's direct fetch does not read `CanServeBlocks`.

**Fix:** add a sim case to `sim_blocks.rs`, for example S2-e's. I ran this probe. It passes on the head and kills M8 at the A′ assertion:

```rust
#[test]
fn probe_equal_work_sibling_is_direct_fetched() {
    let genesis = params_genesis();
    let mocktime = i64::from(genesis.time) + 10;
    let a = SimChain::build_blocks(&genesis, 1, 1, 0);
    let a2 = SimChain::build_blocks(&genesis, 1, 1, 7);
    let mut all = a.clone();
    all.extend(a2.iter().cloned());
    let sim = node_with(mocktime, Box::new(NoObjectRequests));
    sim.chain.set_ibd(false);
    let mut t = Net::new(sim, &all, mocktime);
    let p = t.connect(1, None, 0x4); // inbound, NODE_BLOOM only: not CanServeBlocks
    t.send(p, "headers", headers_msg(&a));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[p].getdata_requests, hashes(&a), "direct fetch of A");
    assert_eq!(t.tip_height(), 1);
    t.peers[p].getdata_requests.clear();
    t.peers[p].mute = true;
    t.send(p, "headers", headers_msg(&a2));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[p].getdata_requests, hashes(&a2), "equal work: Core fetches A'");
}
```

The case also pins a second Core behaviour: direct fetch serves a peer that section 12 would never ask.

### T2 (Nit, test gap): no test covers `CanDirectFetch`'s strict boundary

**Port:** `dashd-p2p/src/processing/eviction.rs:296` (`tip_time_us > now_us - spacing·20·10⁶`). **Core:** np:1430.

S2 moves this function and gives it a second caller, `headers_direct_fetch`. S2-e's "old tip" case is `sim_blocks.rs:664`. It places the clock at `genesis.time + 20·spacing + 1`, where `>` and `>=` agree, so the `>`→`>=` mutant passes every test (M15).

**Fix:** test exactly at `+20·spacing` (no direct fetch) and at `+20·spacing − 1` (direct fetch), with an inbound non-serving peer so that section 12 stays silent. I ran this probe. It passes on the head and kills M15 at `dt = 3000`, where the mutant logs `Requesting block … from  peer=0`.

### N3 (Nit, contract text): direct fetch handles an unpublished status differently from section 12

**Port:** `download.rs:406` and `:594`, compared with `:483`. **Contract:** `seams.rs:175`.

`schedule` treats a `None` status as the end of the walk (D84). `direct_fetch_walk` reads `None` as "no data", so it would request the block. `headers_direct_fetch` reads `None` for `last` as "not `VALID_TREE`", so it would skip direct fetch without logging anything. The `ChainAccess::status` doc mentions only section 12's handling.

Neither branch is reachable at this head. `HeadersResult::snap` is "published before the reply" (`seams.rs:113`, R3). X′ is drained after the reply. Every header on the walk is `last` or one of its ancestors, so each was indexed and published by the time X′ was taken.

Add one sentence to `direct_fetch_walk` or to the `status` doc that says this branch relies on R3. Otherwise a later host adapter that relaxes R3 would make direct fetch fail silently on a path D84 never considered.

No code change is needed.

## Audit by focus area (beyond Sol r1/r2)

1. **`FindNextBlocksToDownload` and in-flight state (np:1271-1358, 1477-1558).**
   - **Reorgs and `pindexLastCommonBlock`.**
     - The bootstrap guess is followed by `LastCommonAncestor` (`download.rs:454-462`).
     - Any active-chain entry at a height at or above the fork point gives the same LCA. So Core's `min(best.height, tip.height)` is only an optimisation, and replacing it with `tip.height` cannot be observed. This is mutant M12, an equivalent mutant.
     - `nWindowEnd` is computed once from the first last-common block, as in Core. Updates inside the loop do not move the window.
     - Only `HaveTxsDownloaded` blocks advance last-common.
   - **The 1024 window.** The walk reads `+1` past the window, and a mutant without it fails three suites (M10). The chunk arithmetic matches D77.
   - **One hash, several peers.**
     - The `Vec<InFlight>` per hash keeps multimap insertion order through removals (`Vec::remove` is order-preserving), so `[0]` is `lower_bound`'s peer. M2 overwrites `waitingfor` with the last peer, and it is caught.
     - At S2 a hash has at most one requester. Section 12 and direct fetch both skip `IsBlockRequested` hashes, and only S3's compact paths add a second.
     - `finalize` erases only the departing peer's entries and leaves the other peers' `m_stalling_since` alone (np:1804-1830).
     - `start_stall`'s `expect` (`download.rs:383`) holds: the staller comes from `in_flight`, every in-flight peer has a `PeerDownload`, and `block_requested` between the walk and the tail removes only the requesting peer's entries.
2. **Stalling (np:2199-2207, 6748-6781).**
   - `section9` loads the atomic once. It uses that value for both the comparison and the CAS's expected value. Both CASes are `compare_exchange` (strong).
   - A decay that runs between the load and the raise makes the raise lose, as in Core.
   - Core's `BlockConnected` (np:2162-2208) has no early return before the decay. Its Dash-only `ProcessCoinbaseChainLock` prefix is not P3's.
   - The `m_stalling_since` lifecycle matches Core:
     - It is set only by section 12's tail, at the pass-start `current_time` (np:6321, 6834).
     - It is cleared by every `RemoveBlockRequest` of one of the peer's own entries, including the `nullopt` removal when another peer's block connects.
     - It is not cleared by `finalize` of the peer that detected the stall.
     - M3 (no reset) and M6 (restart each pass) are caught; M6 fails three suites.
   - The transcript replay's `mine` uses the signal-free `set_tip` and calls `block_connected` once, so no block is decayed twice.
   - Note: `Shared::block_connected` has no production caller yet. The only `block_connected` call outside `sim` is `dashd-chainstate`'s listener. It was the same at S1 for `m_last_tip_update`, and the host wiring is S6's.
3. **Block download timeout (np:6773-6779).**
   - In libstdc++, `seconds × double` is `duration<double>`. Adding it to `microseconds` gives `duration<double, micro>`, a ×10⁶ conversion with no division, and `>` compares in double. The port's `since as f64 + spacing as f64 * factor * 1e6` performs the same operations in the same order.
   - Every operand is an integer number of µs below 2⁵³: the factor is a multiple of 0.5, multiplied by 10⁶. So no rounding can happen at real epoch times.
   - Only peers with at least one block in flight are counted: the counter goes up on a 0→1 transition and down on 1→0, or at finalize. A peer that is disconnecting but not yet finalized still counts, as in Core. S2-c's simultaneous disconnect at `deadline + 1 µs` is consistent with that.
   - M4 (`others = peers_downloading_from`) fails the unit and two sim cases. M5 (`m_downloading_since` reset on every request) fails the unit and the proptest. M9 (front update on any position) fails the proptest.
4. **Headers direct fetch (np:3202-3258).**
   - The condition order is Core's. The walk's conjuncts are pure, so their order cannot be observed (D80).
   - The walk runs oldest-first: M14 (newest-first) is caught by S2-e and S2-h.
   - It runs on X′, after S1's two-phase `ProcessBlock` tail (D1/D2). It holds the peer's lock only, and its only side effect is `NetCore::push`, which never blocks (`dash-p2p/src/netcore.rs:664-670`).
   - The `IsValid(TREE)` operand on `last` is effectively unreachable. Core returns at np:3363-3366 for a duplicate-invalid header, so M13 (the operand forced true) passes every test, and that is expected. Only a race in the np:3362→3383 gap could fail `last` there.
   - Direct fetch does not read `CanServeBlocks` or `CanRelay`, in either Core or the port. T1's probe relies on this.
5. **Lock order and liveness.**
   - No S2 path waits on V. `status`, `entry`, `ancestor`, `in_active_chain`, `active_at`, `last_common_ancestor`, `min_chain_work` and `is_initial_block_download` are published reads.
   - `NetCore::push` and `disconnect` never block.
   - `HeightsCell` is a leaf. The actor locks another peer's cell while it holds this peer's state; `stats.rs:54` locks state, then its own cell. No cycle.
   - `StallingTimeout` is lock-free.
6. **Test quality.**
   - The L0 units are discriminating. Each one either pins a Core boundary exactly or compares the bookkeeping against an independently written model (the proptest's `Model`).
   - Four behaviours are caught only by L0 units, and no sim scenario or transcript catches them: M1, M2, M3 and M5. That is acceptable, because each unit states the Core rule it pins.
   - The gaps are T1 and T2 above.

## Mutations

Each mutant was applied in my detached worktree, run against `--lib processing::download`, `--test sim_blocks` and `--test dashd_p3_transcripts`, then restored. None overlaps Sol's r1/r2 set.

| # | Mutation | Result |
|---|---|---|
| M1 | Staller may be the peer itself (drop `waiting_for != Some(st.id)`) | **Caught:** unit `the_window_edge_names_the_first_in_flight_peer_as_the_staller` |
| M2 | `waitingfor` = every in-flight block, so the last one wins (drop `is_none()` guard) | **Caught:** same unit |
| M3 | `RemoveBlockRequest` keeps `m_stalling_since` | **Caught:** proptest and `the_stall_clock_starts_once…` |
| M4 | Timeout `others = peers_downloading_from` (no `- 1`) | **Caught:** unit, `s2c_block_download_timeout…`, `s2c_timeout_origins…` |
| M5 | `m_downloading_since` set on every request | **Caught:** unit and proptest |
| M6 | Stall clock restarted every pass (no `== 0` check) | **Caught:** unit, `s2a_ibd_stalling`, `blocks_stall` transcript |
| M7 | No decay in `Shared::block_connected` | **Caught:** `s2a_ibd_stalling` (line 402) |
| M8 | Direct fetch `tip_work < last.chain_work` | **Survived:** T1. Killed by the T1 probe |
| M9 | `m_downloading_since` advanced on any removal, not only the front | **Caught:** proptest |
| M10 | Window without `+1` | **Caught:** unit, S2-a, S2-g, `blocks_stall` |
| M11 | Section 12 ignores the limited/IBD gate (`\|\| true`) | **Caught:** `s2b_block_sync_from_inbound_peers_in_ibd` |
| M12 | Bootstrap from the tip height, not `min(best, tip)` | **Survived:** equivalent (LCA corrects; see audit 1) |
| M13 | Direct fetch ignores `last.IsValid(TREE)` | **Survived:** unreachable operand (audit 4) |
| M14 | Direct fetch requests newest first | **Caught:** S2-e, S2-h |
| M15 | `CanDirectFetch` `>`→`>=` | **Survived the suite:** T2. Killed by the T2 probe |
| — | T1/T2 probes on the unmutated head | Both pass |

## Validation

These ran in `/work/scratch/p3-s2-r1-opus/wt` (detached at `d714bac05`), with a private `CARGO_TARGET_DIR` reflink-seeded from the S1 worktree and a 0700 `TMPDIR`. Box load was 34–45 during the run.

| Check | Result |
|---|---|
| `cargo test -p dashd-p2p` | **Passed:** 202 tests (lib 50, inbound transcripts 7, P3 transcripts 2, dashd transcripts 11, sim_blocks 9, sim_m2 44, sim_p3_s1 17, sim_serve 11, sim_sync 51) |
| `cargo clippy -p dashd-p2p --all-targets -- -D warnings` | **Passed** |
| Worktree after mutations and probes | Clean (`git status` empty; a proptest-regressions file a mutant wrote was removed) |

I did not touch any live oracle, chain data, gate slot or other worktree. I found no security issue in Dash Core.

VERDICT: ACCEPT-NITS
