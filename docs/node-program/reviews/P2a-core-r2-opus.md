# P2a-core: Opus high review, round 2 (narrow re-check)

- Candidate: `node/p2a-core-leg` at `bcfea90ef`. Fix range `a8ae94adb..bcfea90ef` (16 commits, 15 files).
- Scope: closure of r1 F1–F10 (`reviews/P2a-core-r1-opus.md`), Sol r2's R2-n-1 (the D60 text), the new D68 commits (5d1f34d7f, bcfea90ef), and anything new in the diff. Unchanged code was not re-reviewed.
- Reference: Dash Core v24.0.0-rc.3. `tag:` means the release tag (extract `/work/scratch/p2a-fable/v24.0.0-rc.3/src`). `pin:` means the oracle pin `d4c16888b4` (`dash-oracle-sys/pins.txt` v24.0). `np:` is `net_processing.cpp`.

## Closure table

| Item | Ruling | Evidence |
|---|---|---|
| F1 lock-order D-row | CLOSED | D66 (`P2a-core-notes.md:72`) states the order: map, then state, then leaf cells. Only the actor waits on the map while it holds a state. It also states three contracts. The text appears on `ChainAccess` (`seams.rs:116-121`), `Shared::updated_block_tip` (`peerman.rs:124-127`) and `AddressRelay` (`seams.rs:496-498`). The RPC edge (state → chain) is removed in code: `stats::node_state_stats` (`stats.rs:43`) copies the two `BlockId`s out under the state lock, and `resolve_heights` (`stats.rs:71`, called at `peerman.rs:172`) reads the chain after the lock is released. A debug assert (`peerman.rs:135`) checks rule (2). `with_state` (`peerman.rs:230-234`) clones the slot and drops the map before it locks the state. |
| F2 BlockSync seam | PARTIAL | The design's shape is restored. `SyncCx` (`seams.rs:162`) carries `&mut PeerState`, the handle, `NetCore`, `ChainAccess` and the snapshot, plus `misbehaving` and `maybe_punish_node_for_block`. Every per-peer hook takes it (`seams.rs:251-267`), and there is a `block_checked(cx, hash, invalid)` (`seams.rs:274`). This matches `P2a-outbound.md:315-320, 418` (`Cx` = state, handle, push, misbehaving, `ChainAccess`, clock and RNG through the core; addrman is not needed by block sync). D65 records the contract for P3. Two residual gaps against CH1-node rev3.1 and Core remain: N1 and N2 below. |
| F3 `ssc` pre-sync read | CLOSED | The `"ssc"` arm (`stubs.rs:149-152`) reads base-sdk `Ssc { item_id: i32, count: i32 }` (`codec_p2p!`, base-sdk `dbe245f7` `p2p_core/src/msg/control.rs:55-62`) through `cx.read`. It logs Core's `SYNCSTATUSCOUNT -- got inventory count: nItemID=%d  nCount=%d  peer=%d` under `mnsync`, which matches tag `node/sync_manager.cpp:330-343`. A short payload maps to `ReadError::Throw` (`policy.rs:1057-1076`; only headers/headers2/qgetdata map to the other variants), so `HandlerCx::read`'s `unreachable!` cannot fire on peer input. The test is `sim_m2.rs:355` `ssc_is_read_until_the_masternode_sync_finishes`. The row stays `exact("ssc", "SP1")` (`registry.rs:179`), which is now accurate. |
| F4 shutdown from process_headers | CLOSED | `ChainAccess::process_headers` now returns `Result<HeadersResult, Interrupted>` (`seams.rs:145-156`). The headers branch returns quietly on `Err` (`sync.rs:211-214`). D23's `debug_assert!` (`sync.rs:228`) stays for the real contract break. D23 and D59 are updated. Test: `sim_sync.rs:420` `headers_while_the_chain_shuts_down_end_quietly` (no reply, no score, no panic, and state is untouched until the chain recovers). |
| F5 `np:` line base | CLOSED | The notes header (line 3) now says tag lines throughout. I spot-checked 24 re-cited ranges against the tag (WakeMessageHandler net.cpp:2671; np:546/314/446 bloom filters; FinalizeNode 1783-1856; ProcessGetData 2935-3111; 6870-6875; 6894-6897; 6581-6590; 6749-6750; 6555-6750; 6592-6609; NextInvToInbounds 1259-1269; GetExtra* net.cpp:2850-2880; 5329-5343; FormatSubVersion clientversion.cpp:68-83; GetNodeStateStats 1877-1920; MaybeSendAddr 6186; UpdatedBlockTip 2273-2308; 2185; ThreadMessageHandler net.cpp:3676; FinalizeNode's Connected 1848-1853; CJ server init.cpp:2238-2242; getheaders 4775-4791). All match. |
| F6 stub reasons | CLOSED | `stubs.rs:153-159`: sig shares (no shares manager), `dsa`/`dsi`/`dss` (no CJ server), `dsf`/`dsc`/`dssu` (no wallet), and only `dsq`, governance, `mnauth` and `platformban` as unsynced-before-read. |
| F7 D6 wording | CLOSED | D6 (`notes:12`) states the getheaders/getheaders2 reply rule, including the `UsesCompressedHeaders` services-bit choice and the regtest/devnet genesis case, and the clsig/isdlock dependency on no stored spork, with SP0 re-checking it. |
| F8 `Connected` uncompared | CLOSED (recorded) | D12 (`notes:18`) records the drain-time stamp and the ask to SB6 for an AddrMan call recorder. `sim_m2` `addrman_connected_follows_the_finalize_gates` pins Core's four gates (np:1848-1853); it kills M22. |
| F9 production RNG | CLOSED (recorded) | D67: `with_rand_source`/`deterministic` exist only under SB6's `test-utils`, reached only through `sim`. The `compile_error!` attempt (b4d72341e) was rightly withdrawn (02abbc4b3), because `sim` unifies into workspace builds. The accessor/log line is an ask to SB6. No `lib.rs`/`Cargo.toml` residue remains (both are absent from the diffstat). |
| F10 mutants | CLOSED | **28/28 killed**, plus the implementer's two extra rows (M21b, M29), also killed: 30/30. Table below. |
| R2-n-1 D60 text | CLOSED | D60 (`notes:66`) says "37 RNG byte calls … 36 `GetRandBytes` and one `GetStrongRandBytes`". It limits "(false, 32)" to the nine selected setup twins and the compared per-peer and per-pass draws. |
| D68 reachable overlays | CLOSED, with nit n-1 | Citations hold. tag `init.cpp:1862/1948/2750` and pin `init.cpp:1877/1963/2765` are each `g_reachable_nets.Remove(NET_CJDNS/NET_ONION/NET_I2P)`, and tag `netbase.h:143` is `m_reachable`'s all-networks default. The O1c session (pin `src/test/oracle/oracle_session.cpp:670-697`) builds `TestingSetup` and `CConnman::Options` itself and refuses `-onlynet/-proxy/-onion/-i2psam` ("not modelled"). Init's reachability code never runs, so every network is reachable in Core's leg. The node leg now sets `cfg.reachable_overlays = [Onion, I2p, Cjdns]` (`node-l1/src/p2p_node.rs:852`), with `only_net` empty, so SB6's `is_reachable` (`dash-p2p/src/netcore.rs:821-825`) is true for every network, as Core's is. Intake and relay match: tag np:4459-4466 (`reachable` → `RelayAddress`, `vAddrOk`) and np:2710 (`nRelayNodes`). The change is confined to the comparison leg; a production `NetConfig` keeps SB6's default. |
| New in the diff | N1, N2 (minor), n-1 (nit) | No regression, lock inversion or new panic on peer input. Details follow. |

## Findings

### N1: Minor: `BlockChecked` drops V's `ibd` argument, which CH1-node rev3.1 makes normative

Location: `dashd-p2p/src/processing/peerman.rs:156` (`Shared::block_checked(hash, invalid)`) and `seams.rs:274-281` (`BlockSync::block_checked(cx, hash, invalid)`). Against `designs/CH1-node.md:962` (`pub fn block_checked(&self, b, st, ibd: bool); // rev3: P2b's IBD gate, V-evaluated`), CH1-A8 (`:1184`) and test (d) `inline_listener_gets_ibd` (`:1070`: "the argument wins, and the listener never reads the view").

Core's valid branch reads `!m_chainman.ActiveChainstate().IsInitialBlockDownload()` under `cs_main` at the signal (tag np:2334-2336). CH1-node passes that value as an argument because the published view can still say "pre-hold" when V signals. The new seam has no slot for it. P3's `block_checked` can only call `chain.is_initial_block_download(snap, now)` through `BlockCheckedCx::with_peer`. That reads the published snapshot at drain time, which is later than V's inline call and is exactly the read CH1-node's test (d) forbids.

Scenario: a block that ends IBD is checked valid while the view is still pre-hold. Core runs `MaybeSetPeerAsAnnouncingHeaderAndIDs` for the source. P3 on this seam sees IBD still latched and skips it, so the high-bandwidth compact-block peer set diverges.

Fix: thread the flag through, as `Shared::block_checked(hash, invalid, ibd)` → queue → `BlockSync::block_checked(cx, hash, invalid, ibd)`. Name it in D65. It is one parameter, and `NoBlockSync` ignores it.

### N2: Minor: D65's timing claim omits `ProcessBlock`'s tail, so a literal P3 port loses invalid-block punishment

Location: D65 (`P2a-core-notes.md:71`, "The per-peer drain keeps both when P3's block handler waits on V … and V calls `Shared::block_checked` before it answers"), and the delivery point `peerman.rs:412`.

In Core, `BlockChecked` runs inline inside `ProcessNewBlock`, *before* the rest of `ProcessBlock` (tag np:3717-3732). That code then does `if (new_block) RemoveBlockRequest(hash, nullopt) else mapBlockSource.erase(hash)`. Here the signal is delivered after the whole `ProcessMessages` returns, which is after P3's port of that tail.

Scenario: a peer sends a block that fails `CheckBlock`. V calls `block_checked(hash, Some(invalid))`, which queues it, and answers. `new_block` is false. P3's literal `ProcessBlock` tail erases `mapBlockSource[hash]`. The drain then calls `block_checked`, finds no source, and does not call `MaybePunishNodeForBlock`. Core scores the peer (np:2321-2326) and discourages it in that pass. The same holds for the `cmpctblock` and `blocktxn` paths, which share `ProcessBlock` (np:3804, 5271).

I checked the valid branch. There, the deferred `mapBlocksInFlight.count(hash) == size()` gives the same truth value before and after the tail's `RemoveBlockRequest(hash, nullopt)`, so only the invalid branch differs. `a_block_checked_inside_process_messages_punishes_in_the_same_pass` can't see this, because its handler has no tail.

Fix (either):
- Have D65 require P3 to run `ProcessBlock`'s tail only after the source's `BlockChecked` has been delivered. A small `drain_block_checked` reachable from the handler right after V's reply would make that mechanical.
- Or have P3 do the `mapBlockSource` erase inside `block_checked`.

Name it in P3's PLAN row next to D65's existing note.

### n-1: Nit: D68 says `NetCore::accept`'s CJDNS flip "would not" follow Core

`dash-p2p/src/inbound.rs:315-318` flips through `maybe_flip_ipv6_to_cjdns`, which uses `is_reachable(Cjdns)` (`local.rs:192-198`). With D68's setting it flips exactly as Core's `AcceptConnection` does (tag `net.cpp:1864, 1867`). The point is moot because the leg never accepts, but the clause is backwards. Reword it to "would too; the session leg never accepts" or drop it.

## Diff-only regression check

- **Lock order.** `BlockCheckedCx::with_peer` (`seams.rs:230-236`) clones the state `Arc` under the map guard. That guard is a temporary of the `let`, so it is dropped before `chain.snapshot()` and `state.lock()`. The drain runs only on the actor with no lock held: at pass start (`peerman.rs:390`), and after `process_messages` has released the state (`peerman.rs:412`; `logic.rs:348-352` takes the lock locally). `Shared::block_checked` takes only the leaf `checked` lock and the wake lock, so it is safe from V's inline listener and from inside a pass (the M29 test does exactly that). `SyncCx` in sections 7, 9 and 12 reborrows the `&mut PeerState` the actor already holds; it adds no lock.
- **Panics on peer input.** No new `unwrap`, `expect` or indexing on peer data. `ssc` errors are `Throw` (F3). The `IN_PASS` assert can only fire on a host wiring error (`updated_block_tip` called from inside a pass), not on peer bytes. `PassGuard` restores the flag on unwind.
- **Semantics.**
  - `headers_direct_fetch` now gets V's post-acceptance `r.snap` (48140f16d). That matches Core's live-tip read in `HeadersDirectFetchBlocks`.
  - Section 9's and section 12's `SyncCx` use the section-5 snapshot (D33), unchanged.
  - The stats heights are resolved after the lock is released, so they can come from a later tip than the state copy. That is harmless for `getpeerinfo`.
- `git diff --check a8ae94adb..bcfea90ef` is clean.

## Checks (at `bcfea90ef`, detached worktree, private target seeded by reflink, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=8 nice -n 10`, TMPDIR in scratch, load 60–85)

| Check | Result |
|---|---|
| `cargo test --locked -p dashd-p2p --all-features` | pass: lib 20, dashd_transcripts 11, sim_m2 44, sim_sync 48 (123/0) |
| `cargo test --locked -p node-l1 --all-features --test p2p --test p2p_node` | pass: p2p 1/0, p2p_node 13/0 (115 s) |
| `cargo clippy --locked -p dashd-p2p -p node-l1 --all-features --all-targets -- -D warnings` | clean |
| `cargo fmt --check -p dashd-p2p -p node-l1` | clean |

No timeouts or reruns were needed.

## Mutants (`run28.py bcfea90ef`; worktree and TMPDIR re-pointed to this scratch dir, logic unchanged)

I checked each replacement string against my r1 intent; they all match. The baseline was OK. `dashd-p2p` killed every mutant, so node-l1 was never needed.

| Mutant | Result | Killing test |
|---|---|---|
| M1 mn throttle off | killed | masternode_connections_are_throttled_to_100ms |
| M2 wait keeps wake flag | killed | a_wake_is_consumed_by_the_next_wait |
| M3 should_discourage kept | killed | discouragement_acts_in_the_next_send_messages |
| M4 outbound_with_protect leak | killed | a_disconnected_protected_peer_frees_its_slot |
| M5 outbound trickle 3 s | killed | the_trickle_timer_draws_cores_average_per_connection |
| M6 mn trickle not halved | killed | the_trickle_timer_draws_cores_average_per_connection |
| M7 others < 0 | killed | a_stalled_sync_peer_is_kept_when_no_other_could_serve |
| M8 block-relay `>` connect time | killed | the_youngest_extra_block_relay_peer_goes_unless_it_gave_a_block |
| M9 unknown getdata not erased | killed | getdata_unknown_type_ends_the_batch_silently (+3) |
| M10 unconnecting `>= 10` | killed | few_unconnecting_headers_ask_for_the_gap_and_score_every_tenth |
| M11 protect `>` | killed | a_header_with_exactly_our_tips_work_protects_but_is_no_announcement |
| M12 inv latch after sync | killed | a_syncing_peers_inv_leaves_the_pre_sync_trigger_alone |
| M13 no pause_send return | killed | a_paused_peer_is_neither_polled_nor_more_work |
| M14 panicked not set | killed | a_panic_stops_the_thread_and_still_stops_the_handlers |
| M15 UpdatedBlockTip ignores can_relay | killed | updated_block_tip_skips_a_masternode_connection |
| M16 threshold `>` | killed | threshold_is_crossed_once |
| M17 ConsiderEviction `>` | killed | a_sync_peer_at_exactly_our_tips_work_is_never_asked_to_verify |
| M18 few-unconnecting `<` | killed | eight_unconnecting_headers_are_still_few |
| M19 revert `> 2` | killed | two_new_blocks_revert_a_high_bandwidth_peer_to_an_inv |
| M20 stale 2 × spacing | killed | the_stale_tip_check_asks_for_an_extra_outbound_peer |
| M21 no interrupt between peers | killed | an_interrupt_from_a_handler_skips_the_peers_send_messages |
| M22 Connected ignores score | killed | addrman_connected_follows_the_finalize_gates |
| M23 trickle `>` | killed | the_trickle_timer_draws_cores_average_per_connection |
| M24 erase ignores pause_send | killed | a_paused_peer_keeps_its_next_getdata_entry |
| M25 fMoreWork counts paused | killed | a_paused_peer_is_neither_polled_nor_more_work |
| M26 timeout integer order | killed | the_initial_getheaders2_goes_to_one_peer_from_the_best_header |
| M27 onion `<` | killed | onion_peers_up_to_the_onion_target_are_kept |
| M28 last-announce `>=` | killed | a_header_with_exactly_our_tips_work_protects_but_is_no_announcement |
| M21b (extra) no interrupt after SendMessages | killed | an_interrupt_from_send_messages_skips_the_next_peer |
| M29 (extra) BlockChecked drained only at pass start | killed | a_block_checked_inside_process_messages_punishes_in_the_same_pass |

Score: 28/28 killed on r1's table (30/30 with the extras). There are no survivors, so no equivalence rulings are needed. M23, which r1 expected to be equivalent on the sim clock, is killed by the trickle-timer test hitting the boundary.

Raw output: `/work/scratch/p2a-core-r2-opus/OUT.tsv`.

## Verdict

F1, F3–F10, R2-n-1 and D68 are closed, and the mutant table is fully killed. F2's seam shape is restored; what remains are N1 and N2. Both are contract gaps on the P3-facing `BlockChecked` path, not defects at M2, since `NoBlockSync` ignores the hook. They are cheap to fix: one parameter, and D65 text (with the P3 and CH1-node PLAN notes D65 already asks for). I recommend fixing both before merge, with a manager check of the diff and no further review round. n-1 is optional.

APPROVE-WITH-NITS
