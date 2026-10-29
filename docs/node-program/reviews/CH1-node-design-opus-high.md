# CH1-node design — Opus high (sequential, after Sol r2)

Reviewed `designs/CH1-node.md` at **rev2 20ccc613** on `node/integration`. The working-tree file is identical to that commit. The prior rounds are Sol r1 (REWORK) and Sol r2 (APPROVE-WITH-CHANGES; R2-M1/M2 closed in rev2 and verified by the manager). The relevant rulings are DEC-147, DEC-152, DEC-153 and DEC-155. I followed `briefs/REVIEW-COMMON.md`. PLAN §7 handling: codenames only; I had no private access and found nothing that needs one.

Core citations are to **v24.0.0-rc.3**. I extracted them with `git show` into `/work/scratch/ch1-node-design-opus-high/core/`. Abbreviations follow the design's: `v` = `validation.cpp`, `bs` = `node/blockstorage.cpp`, `h` = `chainlock/handler.cpp`, `cl` = `chainlock/chainlock.cpp`, `np` = `net_processing.cpp`, `rpc` = `rpc/blockchain.cpp`, `is` = `instantsend/net_instantsend.cpp`. CH1 line numbers refer to rev2. SB4 code refers to `node/sb4-dash-chain` at **C2 6ef0a8e75**.

This is a design review, so no CH1 code exists to run. The evidence is a line-by-line Core comparison, the SB4 C2 source, the `sb4-core-scenarios` branch (`harness.cpp`, 40 scenario files), and the counterpart designs (SB4 rev7.1, SB5 with a7297d13, CH2, A1, P2a).

## Verdict

**APPROVE-WITH-CHANGES.**

The ported control flow is faithful. The five riskiest paths match Core statement by statement (§A below), and the r1/r2 closures hold. Three majors need design text before implementation starts. None changes the architecture (DEC-152/153, the holds and the command classes stay):

- **OH-M1:** a notification can become observable before the snapshot it announces.
- **OH-M2:** the lock rules miss deadlocks through task and actor waits, one of which CH1 itself introduced by narrowing the in-hold service loop.
- **OH-M3:** the row's acceptance (N9) depends on harness work that has no PLAN row, owner or estimate, and N9's own estimate is low.

## A. Core fidelity: the five riskiest paths, line by line

| Path | Core | CH1 rev2 | Result |
|---|---|---|---|
| `FindMostWorkChain` + `PruneBlockIndexCandidates` | `v:3519-3593` | §4.3 `:365-389`, §4.8 `:539` | **Match.** The failed/conflict/missing branches mark the *descendants* `f` using flags computed on `t` (`v:3544-3563`). `m_best_invalid` is updated from `pindexNew` (`v:3549-3551`). `t` is erased after the walk (`v:3565`). The walk stops at `m_chain.Contains` on the private chain (A2). |
| Candidate ordering | `bs:32-49`, `TryAddBlockIndexCandidate` `v:4233-4263`, `ReceivedBlockTransactions` `v:4266-4305` | §3.1 `CandKey`, §3.3, §4.8, §4.9 `:605-618` | **Match**, with D1/D2 declared. I confirmed D2's duplicate-visit case is reachable in Core: in pruned mode, two candidates sharing a data-missing ancestor path make `FindMostWorkChain` insert `(prev(f), f)` twice (`v:3559-3561`). Core then mutates a member's `nSequenceId` in place (`v:4286`), so replacement semantics is a sound definition. |
| `ActivateBestChain` / `ActivateBestChainStep` / `ConnectTip` / `DisconnectTip` | `v:3601-3684`, `v:3724-3856`, `v:3395-3513`, `v:3249-3342` | §4.1-§4.5 | **Match:**<br>• the inner-loop predicate (`v:3812`, the full comparator);<br>• the early `return true` without `CheckBlockIndex`/PERIODIC (`v:3813`);<br>• `InvalidChainFound(vpindexToConnect.front())` = the batch top (`v:3650`);<br>• `BlockChecked` before the failure check (`v:3432`);<br>• IF_NEEDED before `SetTip` (`v:3304`, `v:3457`);<br>• `BlockDisconnected` after `UpdateTip` (`v:3320`).<br>Two nits on null-tip edges (OH-n1). |
| `InvalidateBlock` | `v:3889-4046`, `rpc:2000-2023` | §4.6 | **Match:**<br>• the multimap scope and its filter (`v:3916-3933`);<br>• the hooks at `v:3951` and `v:3969`;<br>• the unconditional `insert(pprev)` (`v:3982`);<br>• the verbatim `v:3983` condition;<br>• `lower_bound` plus the comparator re-add (`v:3991-3999`);<br>• the final scope's full scan and `m_failed_blocks` (`v:4016-4032`);<br>• genesis returning `false` with a valid state, so the RPC still runs ABC (`rpc:2017`). |
| `PreciousBlock` + `ResetBlockFailureFlags` (node half) | `v:3858-3887`, `v:4152-4231` | §4.7 | **Match:**<br>• erase before the key changes;<br>• the `INT32_MIN` guard;<br>• the conflict/data filter on re-insert;<br>• `PruneBlockIndexCandidates` only on insert.<br>The reconsider post-processing (best_invalid reset, promoted erase, then re-add via `TryAdd`) gives Core's result: a promoted block now has `FAILED_VALID` and fails `IsValid(TRANSACTIONS)`, so the erase/add order cannot differ. |

I also checked the following, and they match:
- **`AcceptBlock`/`ProcessNewBlock`** (`v:4759-4906`): the unrequested gates, `NewPoWValidBlock`, `BlockChecked` on failure, and no `NotifyHeaderTip` on failure.
- **`ProcessNewBlockHeaders`** (`v:4729-4756`).
- **ChainLock record/enforce** (`h:133-183`, `h:270-321`, `cl:106-171`):
  - the captured pair;
  - HeightMismatch updating `best` without relay;
  - the late-header transition.
- **`EnforceBlock`/`MarkConflictingBlock`** order (`v:4049-4148`), against §5.4.
- **`ResolveBlockConflicts`** (`is:609-645`): one `hasTxForLock` for all blocks, reset without ABC, ABC at the end.
- **The load rules** (`bs:290-331`, `v:5313-5371`) and **`LoadChainTip`** (`v:4992-5018`).

## Findings

### OH-M1 — major: queued signals, UI and `TipWatch` notifications fire inside a hold, before the snapshot that contains their tip is published

**Where:**
- `CH1-node.md:169`, `:301` and `:307-310`: the in-hold enqueue and the inline UI notify.
- `:420`, `:640`: `update_tip` → `TipWatch` under the hold.
- `:942-943`: the signals thread runs concurrently with the hold.
- `:796`: the reader rule.

In Core, every queued validation callback, `uiInterface.NotifyBlockTip` and `g_best_block` is emitted under `cs_main` (`v:3797-3826`, `v:3212-3216`). Every Core reader that would act on such a notification and then read chain state takes `cs_main`, so it blocks until the scope ends and sees the post-scope tip.

CH1 keeps the emission under the hold but lets the consumers read the lock-free published snapshot. That snapshot is stored only at `release_tip` (`:172`). The signals thread runs concurrently (`:192`, `:942`), so a consumer can act on tip X and then read a snapshot without X. **Core never shows that ordering**, and it is visible on P2P and RPC.

**Concrete case 1 (P2P, `p2p_sendheaders`).** A peer prefers headers. Block B extends the tip in an ABC hold, and V enqueues `UpdatedBlockTip(B)` at `:308`. Before `release_tip`, the signals thread runs P2P's listener (Core `np:2273-2308`: it pushes B to `m_blocks_for_headers_relay` and wakes msghand; it takes no `cs_main`). msghand's `SendMessages` then reads `ActiveChain()[h(B)]` from the published snapshot (A1 `:1359`, "Headers: from ChainView and the arena only"). That snapshot is still pre-hold, so the check sees `!= B`, takes the "Bail out if we reorged away" branch (`np:6425-6427`) and sets `fRevertToInv`. We send `inv` where Core sends `headers`. In Core, `SendMessages` holds `cs_main` at `np:6338`, so it waits for the scope and sees B.

**Concrete case 2 (RPC).** The client calls `waitforblockheight(h+1)`, which `RPCNotifyBlockChange` wakes through `ui.notify_block_tip` at `:309` (`rpc:300-305`, `:431`). It then calls `getblockcount`, served from the `ChainView` fast path (A1 `:1955`). The answer is `h`. In Core, `getblockcount` blocks on `cs_main` and returns `h+1`. FTs do exactly this after `waitforblockheight` and `-blocknotify`. The same race applies to ZMQ `hashblock` followed by `getbestblockhash`.

**Concrete case 3 (inline listeners).** P2b's synchronous `BlockChecked` listener runs under the hold (LO-5). Core's version reads `IsInitialBlockDownload()` under `cs_main` (`MaybeSetPeerAsAnnouncingHeaderAndIDs` gating). If ours reads the published view, it evaluates IBD on the pre-hold tip.

**Fix:**
1. **Signals.** Inside a hold, V puts queued signals on a hold-local staged list, built with the same O(1) node pre-allocation. `release_tip` splices that list into the shared queue in one O(1) section, **after** the snapshot and `ChainView` stores. This is a Core schedule: the scheduler thread got no CPU until `cs_main` was released. Nothing waits for these callbacks inside a hold (LO-4), so no wait is added, and `limit_queue` runs only outside holds, where the staged list is empty.
2. **UI and `TipWatch`.** Fire `ui.notify_block_tip` and `TipWatch` after `publish_view` at the same release (and after the final scope's release in `InvalidateBlock` and `conflicting_marked`).
3. **Invariant.** State it normatively: *no notification of tip X, through any path, is observable before a published snapshot contains X.*
4. **Inline listeners.** They receive V-evaluated values, such as `ibd`, as arguments and never read the published view.
5. **F1-derived state.** F1 fanout must stay per step (DEC-153). Add a rule for RP0, P2b and index owners: externally visible state derived from F1 events (an index's synced height, an IS state) must not be served ahead of `getblockcount`. Either serve it behind a `tip_seq` fence (`published.tip_seq >= event.seq`) or through `Query`.
6. **Tests.** Add a §9.4 directed test (a listener reads `ChainAccess::snapshot()` inside its `UpdatedBlockTip` callback and must see the new tip), and add `p2p_sendheaders`, `interface_zmq` and `rpc_blockchain`'s `waitfor*` to §9.8.

Minor naming: `TipWatch` is described as Core's `g_best_block` for `waitfornewblock` (`:640`). `g_best_block` serves GBT long-poll; `waitfor*` uses `latestblock` through `uiInterface.NotifyBlockTip`. Both need the move.

### OH-M2 — major: LO-1…LO-8 do not cover waits through tasks and actors; CH1's in-hold narrowing creates one cycle

**Where:**
- `:159`, `D4 :1152`, `E-SB5-1 :1137`: class R/M work is not served inside holds.
- `:216-222`: LO-6, LO-8 and the "Why no cycle" argument.
- `:967`: the thread-local enforcement.

**Cycle A (introduced by CH1).** Before rev1, CH2 rule 5 and SB5 §3.3 let V's in-hold wait answer queries. CH1 removed that (D4, E-SB5-1, which is correct for Core fidelity). Since then, an F1 subscriber whose **pop/ack task** awaits *any* V reply deadlocks against V's per-step drain:
1. LK1b is an F1 subscriber (`:187`).
2. Its task sends an `OnV` quorum-selection route (route B, R/M) to verify an ISDLOCK, and awaits the reply.
3. V enters an ABC hold for a 2-block reorg. Disconnect 1 fills LK1b's slot (Q = 1, or any full Q), so disconnect 2's fanout is pending.
4. V's `drain_fanout` (`:418`) waits for LK1b to pop.
5. LK1b's task is blocked awaiting the `OnV` reply, which V serves only at the next release point.

No mutex is involved, so LO-1…LO-7 do not see it. LO-8 covers only the signals thread. SB4's H6 covers only the event-to-ack path. In Core the IS worker takes `cs_main` but is never waited on by validation (its callbacks are queued), so Core has no edge.

**Cycle B (transitive LO-8).**
1. A `ValidationSignals` listener (P2P's `UpdatedBlockTip`) awaits a reply from the P2a/P3 net actor. That is not a V reply, so the marker allows it.
2. The net actor is blocked awaiting `ProcessBlock { Activated }`, as msghand must (OH-m4).
3. V is in `limit_queue` with its barrier pending behind that listener.
4. The Activated reply is C-dependent and deferred.

The same shape appears if an F1 subscriber or W ever calls `signals.sync()`, or if a listener awaits a `Flush` (allowed by `:959`) whose completion needs the ack of a subscriber that runs on the signals thread.

**Fix:**
1. Add **LO-9**: an F1 subscriber's pop/ack task never awaits any `ChainCommand` reply (R, M or C), never runs on the signals thread, and never calls `signals.sync()`. Subscribers that need V use a separate task or post.
2. Restate **LO-8 over the wait-for closure**: neither the signals thread nor anything it awaits, transitively, may await a C-dependent V reply. Say which actors a listener may await at all; the simplest rule is "V R/M replies only; other actors by posting".
3. State that the thread-local marker is a backstop, not the proof. It does not follow spawned tasks or actor hops.
4. Add to §9.6's model, as negative controls: (a) a subscriber task awaiting a `Query` during a Q-full hold, and (b) a listener → net actor → `Activated` chain. Add positive controls with the rules applied.
5. Add H6's companion to SB4/SB5's H-list for node subscribers: "the pop path waits on nothing V does under the hold, *including V replies*".

### OH-M3 — major (feasibility): N9, the row's acceptance, depends on H1–H5, which have no PLAN row, owner or estimate; N9's 3.5 days is low

**Where:** `:1088` (N9 after "harness H1–H5"), `:1124`, Q10 `:1176`, `:1100`.

Neither PLAN nor STATUS has a row for the harness additions. `node/sb4-core-scenarios` (`harness.cpp`, 1,574 lines) has `--wrap` seams for `VerifyChainLock` and `EnforceBlock` only; there is **no `ConnectBlock`/`DisconnectBlock` seam**.

H1 is not optional for the "existing" replays either. T5 and §9.2's 39 replays include invalid-at-connect blocks (for example `cb-amount`, `harness.cpp:732`, used by `a18-invalid-chain-found`). `RecordedConnector` needs H1's transcript for those.

N9's content is also heavy for 3.5 days:
- 39 replays plus 17 new Core-judged scenarios;
- T1/T2 on full mainnet and testnet headers;
- the brute-force suite;
- the explicit-state lock-and-wait model with seven actors and the LO-8, growth and Q = 1 controls;
- loom;
- the release-point injection hook for the ChainLock schedules;
- nine PF0 lines.

CH2's comparable model work alone took longer.

**Fix:**
1. Add a PLAN row for H1–H5: `--wrap` of `Chainstate::ConnectBlock`/`DisconnectBlock` for the transcript, H3's dumps, and H4's generator. My estimate is about 4 agent-days plus a Sol review, on CH1's critical path.
2. Split N9:
   - **N9a**, with no H1–H5 dependency: models, loom, the property suites, R6/R8, PF0, and the 39 existing replays through a `ScenarioConnector`. That connector derives verdicts from the scenario's build ops (invalid-by-construction hashes) plus V's own `has_conflicting_chainlock` through `ConnectCx`, so the `v:2516` state dependence is computed rather than recorded. It must be cross-checked against H1 once H1 lands.
   - **N9b**: the 17 `ch1-*` scenarios and the H4 campaign.
3. Re-estimate N9a+b at about 6–7 days, which puts the critical path near 15 days. Re-run `tools/cpm_plan.py`.

### OH-m1 — minor: the IF_NEEDED full flush's `ChainStateFlushed` locator is the *pre-step* tip in Core

`v:3114` enqueues `ChainStateFlushed(m_chain.GetLocator())` at Core's site, before `SetTip`. That is the locator of the old tip (`d` on a disconnect, `prev(id)` on a connect), while the coins are already at the new state. D3 (`:1151`) says the split is "never" visible, but if CH2b builds the locator at `freeze_after_step` from the post-step tip, the wallet's best-block locator (`chainStateFlushed` → `WriteBestBlock`) differs.

**Fix:** `PendingFreeze` captures `locator(private_tip)` at `at_site`, and the signal is enqueued in `freeze_after_step` with that locator. That keeps Core's FIFO position (before `BlockDisconnected`, which `:445` enqueues after the step; before the batch's `BlockConnected`). Note it in D3 and E-CH2-1.

### OH-m2 — minor: `ClSigs::by_hash` differs from Core's `GetChainLockByHash` on the zero hash

`bestChainLockHash` defaults to `uint256{}`, and `ResetChainlock` resets it there (`cl:98-104`, `chainlock.h:55`). `GetChainLockByHash(0)` therefore returns true with a null `ChainLockSig` (height −1) whenever no lock is recorded (`cl:140-150`). `ProcessGetData` then pushes a `clsig` for `getdata(MSG_CLSIG, 0)` (`np:3041-3046`). CH1's `by_hash` (`:754-755`) returns `None`.

**Fix:** mirror `best_hash` as `Uint256::ZERO` when unset, with `best` as the null sig. Alternatively, declare the difference in §13 and `quirks.toml`, and confirm with a dde-probe raw-P2P regtest probe.

### OH-m3 — minor: `deferred_or_now(ClEnforce)` in `RecordChainLock` must never run enforcement before the reply

At `:679`, "now" would run a class C enforcement and its ABC inside a class M command and before the relay reply. That contradicts §0 item 6 ("relay is not delayed behind a reorg") and §2.4. **Fix:** say it is queued: next on `deferred` when idle, otherwise deferred behind the running composite. It always runs after `scope_end` and the reply.

### OH-m4 — minor: state which `BlockReply` P2P must use

Core's msghand runs `ProcessNewBlock` through ABC before it reads the peer's next message. With `Accepted`, a peer's `block` followed by `getheaders` would be answered without B (`:115-122`). **Fix:** §2.2/§8.3 should say normatively that the P2P block path uses `Activated`, and that `Accepted` exists only for contexts LO-8 forbids from awaiting `Activated`.

### OH-m5 — minor: read-error handling is inconsistent between §4.5/§4.6 and §7.3

§4.5 ports `DisconnectTip`'s read failure as `return Ok(false)` (`v:3259-3263`). `invalidate_block` then returns `false` with a **valid** state, so the RPC runs ABC and returns success (`rpc:2017-2023`). §7.3 (`:860`) says any read error is "an abort (fail-stop)". DEC-148 requires only that a read error is never treated as data loss; it does not require an abort.

**Fix:** §7.3 should say read errors follow Core's per-site handling:
- `ConnectTip` aborts;
- `ActivateBestChainStep` aborts after a failed disconnect;
- `InvalidateBlock` returns false;
- IS and `MarkConflictingBlock` assert;

and in every case nothing is cleared or marked invalid.

### OH-m6 — minor: SB4/SB5 asks and dependencies need cleanup before SB4 C5

None of A1–A13 exists at 6ef0a8e75, and SB4 rev7.1 has not adopted them. SB4 has already shipped the types A11 must change: `TipHost::tip -> BlockId` (`host.rs:24`) and `ChainReadSnapshot.tip: BlockId` (`snapshot.rs:31`). The "+1.5 SB4 days" (`:1093`) understates A11.

**Fix:**
1. Get a manager ruling (an SB4 erratum) on A1/A2/A5/A7/A8/A11/A12/A13 before C5 starts.
2. Defer A10 to the `TestBlockValidity` owner (CH3a/CH7a). CH1's rows only route it.
3. Rename the asks per source (`CH1-A1…`, `SB5-A…`): SB5 uses A11/A12 for different SB4 asks (StepBudget, product-agnostic `hold_tip`; SB5 `:483-491`), and its F3 row lists "SB4 A1, A11, A12" across both namespaces.
4. N3 is effectively gated on **SB4 C6**, not C5, because it needs SB5 F3, and F3 needs "C6 + SB4 A1, A11, A12" (SB5 `:1157`). Fix the "After" column and the CPM input.

### OH-m7 — minor: the brute-force property must be checked at quiescence

ABC caches `pindexMostWork` across outer iterations (`v:3773`, `v:3842`). A class M `ProcessBlock` served at a release point can add a better candidate that the running ABC ignores until its deferred tail runs. "After every completed ABC" (`:1019`) would then flag Core-correct states.

**Fix:** compare when `rx` and `deferred` are both empty, and also exempt `-stopatheight`/shutdown exits and system errors.

### OH-m8 — minor: `PruneOneBlockFile` erases every matching unlinked pair

`bs:147-155` erases all `(prev, id)` entries, duplicates included. §7.2 (`:837`) says "the `(prev, id)` pair". With D2's reachable duplicates, erasing one leaves a stale entry, and `CheckBlockIndex`'s unlinked rules (`v:5768-5803`) would then fail. State "all".

### OH-m9 — minor: crash and recovery test gaps

Add two crash cases:
- `genesis_then_crash`: genesis saved and connected, crash before the first B1. Reopen hits `needs_init` again.
- `prune_clear_in_hold_then_crash`: `PruneClears` serviced inside an ABC hold. The cleared records go to the next B1, not the one in flight.

Recovery steps 4 and 11 otherwise match `bs:290-331`, `v:5313-5371` and `v:4992-5018`. `ScanAndUnlinkAlreadyPrunedFiles` (`v:5320`, `bs:412-427`) is file-only, so it is CH8's; note it in §7.3 so it isn't lost.

### Nits

- **OH-n1.** §4.1 `:302`: with `starting_tip = None` (pre-genesis), Core leaves the inner loop once any tip exists (`!Tip() || (starting_tip && …)`). Write the predicate out instead of `key(starting_tip)`. §4.9 `:584`: `more_or_same` is `true` with a null tip (`v:4780`).
- **OH-n2.** §2.6 LO-3 says V "never waits on another thread while holding L3", but V can take L6 under L3 while W's barrier holds it. That wait is bounded by I/O because W holds L6 only for I/O (H1). Say so.
- **OH-n3.** `next_sequence_id += 1` should be `checked_add` / an abort. Core's `int32` overflow is UB (`v:4286`).
- **OH-n4.** R6 test (a) `:1026`: with the `rx.len()` sampling of `:157`, a command arriving just after the sample waits one more block. Say "at most one further block connect".
- **OH-n5.** `cl_on_updated_block_tip` runs inline at emission, while Core's handler is a *queued* callback (`h:185-207`). That is one of Core's schedules (the scheduler dispatched immediately) and is fine under DEC-147, but record it in §13.

## Answers to the brief's questions

1. **Fidelity.** See §A; the riskiest paths are faithful. The only differences I found are OH-m1, OH-m2 and OH-n1.
2. **Locks and commands:**
   - **Header answers during a reorg.** `ProcessHeaders` is class M and is answered at release points with a fresh snapshot, so this is fine. The `getheaders` and announcement paths that read snapshots are the OH-M1 problem.
   - **ChainLock record versus enforce.** Core-equivalent, given OH-m3.
   - **`ValidationSignals` against the tip feed.** The two are independent by design (DEC-153), but signals become visible before publication (OH-M1).
   - **Deadlocks outside LO-1…LO-8.** Yes: OH-M2.
3. **Crash and recovery:**
   - **Freeze labels.** Correct: `(private tip after the step, step seq)` and the every-build check. D3's "no write between the two points" holds for coins, evo and index records.
   - **Deferred `FlushDue`.** The latch is sound.
   - **Recovery steps 4 and 11.** They match Core.
   - **CRASH cases.** See OH-m1 for the signal locator and OH-m9 for the additions.
4. **Feasibility:**
   - **Estimates.** N0–N8 are plausible. N9 and the harness dependency are not (OH-M3).
   - **SB4 asks A1–A13.** Necessary except A10, but not yet adopted by SB4, and A11 collides with shipped types (OH-m6).
   - **SB5 asks.** B1–B3 are consistent with SB5 at a7297d13 (§4.7).
   - **N9 blocked on harness additions.** Reasonable only once the harness work has a PLAN row and N9 is split.
5. **Tests:**
   - **Harness replay.** Strong once H1 exists.
   - **Brute-force candidates.** Strong once OH-m7's quiescence rule is applied.
   - **loom.** Should include the staged-splice fix of OH-M1.
   - **Missing pieces:** the OH-M1 notification-ordering test, the OH-M2 negative controls, and the FT ports `p2p_sendheaders`, `interface_zmq` and `rpc_blockchain` `waitfor*`.

## Evidence and limits

- I read all of CH1 rev2, both Sol reviews, and DEC-147/148/152/153/155. I read the relevant parts of SB4 rev7.1 (H1–H7, §13.4), SB5 (§3.3, §4.7, the asks, F3), A1 §2.10 and P2a.
- I compared against the Core sources extracted under `/work/scratch/ch1-node-design-opus-high/core/`:
  - `validation.cpp` 2015-2089, 3185-3856, 3858-4148, 4152-4305, 4640-4906, 4988-5018, 5313-5371;
  - `blockstorage.cpp` 28-50, 130-160, 290-331, 412-427;
  - `chainlock/handler.cpp` 55-325 and `chainlock.cpp` 95-172;
  - `net_processing.cpp` 2273-2308, 3041-3046, 6300-6440;
  - `rpc/blockchain.cpp` 300-434 and 1995-2070;
  - `instantsend/net_instantsend.cpp` 598-645;
  - `node/chainstate.cpp` 440-465.
- I read SB4 C2 at 6ef0a8e75 (`chain.rs`, `host.rs`, `chainlock.rs`, `snapshot.rs`, `tip.rs`) and checked for each A1–A13 symbol; none is present. I read the `sb4-core-scenarios` branch's seams and scenario list.
- I ran nothing beyond source reads and greps. No dashd was started, no oracle was queried, and nothing under `/work/chains` or `/work/bench` was touched. Nothing is committed; this file is the only change.
