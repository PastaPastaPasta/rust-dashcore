# CH2: the coins cache, the coins table, and the durability and recovery protocol (reconciled)

| | |
|---|---|
| Status | **rev5, amended by DEC-108, DEC-131, erratum E-b1 and DEC-388 (§2.3: the presize is a ceiling, CH2c D29).** E-b1 records CH2b b1's inputs from SB0, SB4, SB5, CH1-node and DEC-164 (see "Erratum E-b1"). DEC-108 and DEC-131: a1's measured margins replace rev5's estimates, §2.3 adds the boundary rebuild, and DEC-131 declares §2.4.2's `add` deviation and narrows §6.7's load band. **rev5** answered Opus's r6 pass ( `reviews/CH2-design-r6-opus.md`, under **DEC-102**: no Core-flush emulation; `have_in_cache` answers from our own layers, and the native ledger is the only counter; see "Changes in rev5"). rev4 (answered Sol's r4 closure, `reviews/CH2-design-r4-sol.md`, under **DEC-99**: two named counters, the native ledger for physical flushing and the Core-model meter for cache membership; see "Changes in rev4"). rev3 (answered Sol's r3 closure, `reviews/CH2-design-r3-sol.md`, under **DEC-95**: the coins budget is checked at Core's points and rule O is gone; see "Changes in rev3"). rev2 (answered the r2 closures, GPT REWORK and Opus APPROVE-WITH-CHANGES staged, and the manager's D12/D13 rulings; see "Changes in rev2"; step 1 landed separately as `0c7abf97`). The rev2 addendum aligns it with **DEC-81** (SB0: CH2b held, the subscriber interface independent of its event source), **DEC-82** (v24 only) and **DEC-85** (engine parameters isolated for SB0's storage re-check). rev1 answered the r1 reviews and DEC-78. rev0 was the reconciled D2 design. It reconciles `designs/CH2-fable.md` (Fable 5.1) and `designs/CH2-opus.md` (Opus 5.5) under the manager's nine rulings of 2026-10-09. §8 is the reconciliation log. No code until approval |
| Date | 2026-10-09 (rev0–rev5); 2026-10-10 (E-b1) |
| Task | PLAN row CH2, **split into CH2a / CH2b / CH2c** (ruling 1; row texts in §7.4). Review class STATE |
| Crates | `dashd-chainstate::{coins, flush, recovery, subscriber, store}`; `dashd-store` (small additions); `dashd-store-bakeoff` (W-MC); `node-fuzz`, `node-l1` targets |
| Binding inputs | ST3a rev5 (a24baa12, **DEC-74**). **§5.6 is normative**: §4.7 maps each of its items to this design. Also §2.4.4, §5.5, §6.2, and errata E1–E22 (§12 there). DEC-43, DEC-59, DEC-62, DEC-63, DEC-64, DEC-66, DEC-69, DEC-74. A1 §2.8–§2.11 and §3.4–§3.5 with their errata. The PLAN CH2 row |
| Code read | `dashd-store` on `node/integration` (b23527de): `engine.rs`, `fail_stop.rs`, `rocks.rs`, `flatfile/blocks.rs`, `flatfile/fs.rs`; ST2 merged at ebc17787. CS5a on `node/cs5a-coins` (f739d2c7 at rev0; **17014d4b** at rev1, which adopts the fallible `CoinsView`, DEC-77 Q1): `coins/{coin,view,undo,compress}.rs`. rev1 also re-read Core's `index/{base,coinstatsindex,blockfilterindex}.cpp`, `shutdown.cpp`, `dbwrapper.h` and `consensus/consensus.h` |
| Core pin | `~/workspace/dash`, read with `git show` at **v24.0.0-rc.3**, the sole parity reference (**DEC-82**). Cited as `path:line` under `src/`. rev0–rev2 cited and re-checked both v23.1.8 and v24; the addendum keeps only the v24 lines (the history sections above §0 and §8 keep their original pairs). Fixes to the inputs' line numbers are in §8 |

---

## Erratum E-b1: CH2b b1's inputs (2026-10-10)

CH2b b1 starts after these:
- SB0 (DEC-119);
- SB4 rev7.1;
- SB5 rev3 (DEC-159);
- CH1-node rev3.1 (DEC-158);
- DEC-164;
- the manager's b1 rulings Q1–Q6 (2026-10-10).

This erratum records what they change here. It changes no rule of §4.1–§4.4.1's protocol. Where a section disagrees with it, the erratum wins, and the affected sections are marked "(E-b1.n)".

**E-b1.1 Types come from `dash-chain`.**
- SB4's `dash-chain` defines `BlockRef { id, hash, height }`, `IndexAccess { lookup, ancestor, lca, height, have_data_pos, undo_pos, status }`, `IndexWrite { hash, bytes }`, `Status` (`HAVE_DATA`, `HAVE_UNDO`) and `FlatFilePos`, in `tip.rs`, `reader.rs`, `record.rs` and `status.rs`. `IndexWrite` is owned there (SB4 D16).
- `BlockId` is `dash-consensus`'s.
- CH2 imports these types and defines none of them.
- The `BlockIndexRecord` codec, `HeaderChain::take_dirty()` and `HeaderChain::load` are SB4's. CH2 writes the records' bytes as given, in B1 and in the index-only write.
- §2.7's `BlockRef` sketch and §2.11's "CH1, or SB0's shared base" row read accordingly: the event producer is `dash-chain`'s `TipCell` (SB5), and V's side of the block index is CH1-node's.

**E-b1.2 The freeze follows the step** (CH1-node §7.1 and its Q3; ruling Q3). V's API is split into two calls:
- **`at_site(mode, ..) -> Result<PendingFreeze, Abort>`** runs §3.3's decision at Core's call site, including any CACHE_CRITICAL or EVO_CRITICAL wait for the in-flight F's release. It never freezes. It captures the pre-step locator for `ChainStateFlushed` (E-b1.4).
- **`PendingFreeze::freeze_after_step(tip: BlockRef, tip_seq: u64)`** runs right after the tip step, under the same guard.
  - It freezes with `new = tip` (V's private tip after the step) and `tip_seq` = that step's `seq`.
  - A `FlushDue` serviced between the two calls is latched into the `PendingFreeze` and frozen with the same post-step pair.
  - Before W gets the packet, V checks `new.hash == coins.best_block()`, and evo's best block once CH7a tracks one. A mismatch is `AbortNode`.
  - Under `-dbcrashratio` (D14), the synchronous wait is inside `freeze_after_step`.
- **Freezes outside the two IF_NEEDED sites** (PERIODIC, `Flush`, `FlushDue` at idle) capture `(private tip, last assigned seq)`, which then match the coins.

CH1-node N7 defines its `FlushSite` trait over this API. A follow-up after N7 does the rest; it is the PLAN CH2b row's CH1-node item, not part of b1:
- wire the trait;
- re-run CH1-node §9.5's freeze-labelling tests with the real W;
- run SB4's A-40/A-41 with the real W.

§3.3's table and §4.2's "V at the decision" read with this split: the decision happens at the site, and the freeze after the step.

**E-b1.3 Recovery is two calls around CH1-node's step 4** (CH1-node §7.3). `open_chainstate` becomes:
1. **`ChainstateOpen::open(engine, files, ..)`** runs §4.3 steps 0–3:
   - the open-time `sync_wal`;
   - the decision keys;
   - resuming `wiping` (BL-72): it returns `Wiped`, and the node continues with CH5's `-reindex-chainstate`;
   - `BlockFiles`.
2. **CH1-node runs step 4** (`HeaderChain::load`, then `NodeIndex::load`). Step 7's startup scan (`ScanAndUnlinkAlreadyPrunedFiles`) moves inside step 4, between the index load and the candidate rules, where CH1-node §7.3 leaves its slot; ND1 calls it there when CH8 lands. The scan only unlinks files already marked pruned, so running it before step 6's prune locks is safe, as step 6's own note says.
3. **`Opened::recover(index: &dyn IndexAccess, ..)`** runs steps 5, 6, 8, 9 and 10.
   - Steps 6 and 10 have no subscribers in b1 (E-b1.6).
   - `Recovery::step` stays the seam's step machine over steps 5–9.

Step 11 (`load_chain_tip`, VerifyDB, the first `ActivateBestChain`) belongs to CH1-node and ND1.

**E-b1.4 `ChainStateFlushed`** (ruling Q4). Two signals have this name, and b1 keeps them apart.
- **Core's validation signal** (the wallet's `WriteBestBlock`):
  - It goes through a `FlushSignals` sink port. The port has a null implementation until CH1-node/ND1's `ValidationSignals` plugs in (CH1-node §8.2).
  - It carries the locator `at_site` captured: the pre-step tip's, like Core's `m_chain.GetLocator()` before `SetTip` (`validation.cpp:3114`).
  - **V hands it to the port when it observes W's release of that flush** (`FlushDone`), so B1 has already returned Durable (**DEC-166**, which reverses this erratum's first text). Core writes the coins synchronously before it signals (`validation.cpp:3092-3114`), and the wallet's saved locator must never run ahead of durable state. Under a hold the signal goes through CH1-node's staging, like any signal V emits there. This replaces CH1-node §7.1's staging of the signal at `freeze_after_step`.
  - **The cost: the locator lags, and never leads** (D16, §3.9). Our signal can reach the queue after later `BlockConnected` events, so the wallet's locator can lag what the wallet has processed. A restart then rescans a few more blocks from `findLocatorFork`.
  - **The locator never names a block above Core's.** It is the flush's `new` or an ancestor of it, with one exception, which is Core's own: a flush frozen at the `DisconnectTip` site names the disconnected block d, `new`'s child. Core's flush there writes the coins at `parent(d)` and signals `m_chain.GetLocator()` while `m_chain`'s tip is still d, because `SetTip` comes after (`validation.cpp:3303-3316`).
  - **CRASH-suite assertion** (b1 commit 6): the locator handed to the port names `new` or an ancestor of it, or, for a disconnect-site freeze only, `new`'s child d. It is handed only after B1 of that flush has returned Durable.
  - The ALWAYS shortcut (§4.2: active empty and the tip equal to `meta.tip`) hands it over after its index-only write returns.
- **The subscriber control event** of §4.2 step 5 (`ChainStateFlushed { locator(new) }`, sent with `FlushCommit`) is b3's. It goes through W's channel (H2 in E-b1.5).

**E-b1.5 SB4 §13.4's H1–H3, H6 and H7 are CH2's rules.** SB4 requires CH2b to state them. They add to §4.2's concurrency rules and to §4.4.1.
- **H1, W's lock order.** From the freeze to the release (steps 1–11, the rewind gate and the prune wait included), W takes only `BlockFiles` and the stores' own locks. It never takes the tip mutex, the feed or hub lock, or any lock V may hold during enforcement.
- **H2, a W-owned channel.**
  - W writes step 5's `T_ack`, `FlushCommit { flush, new, tip_seq, to }` and step 3b's rewind gate into a channel W owns.
  - The captured Synced set `to` and the gate's target set come from the registry snapshot that the hub republishes, lock-free, on every `register`, `subscribe_at` and `deregister`. Both are filtered on `closing`.
  - Subscribers read the commit and the gate from W's channel and ack to W directly.
  - V drains the channel after the hold and enqueues the control mirrors in F7's order. The queued copy stays a no-op (§4.4.1 rule 1).
- **H3, V's waits under the hold.** While V waits under the guard for W's release, it holds only the guard and its `&mut HeaderChain`. It services W's `PruneClears` (b2), whose status writes need no tip lock. An automatic prune selection may stay armed (§3.3).
- **H6, subscriber progress.** A live subscriber's path from event to ack takes no tip mutex, and it waits on nothing V does under the hold (b3).
- **H7, closing retires obligations at once.**
  - W's wait predicate for a captured set T is `∀ s ∈ T: acked(s) ∨ rewound(s) ∨ closing(s)`.
  - W registers its waker, then re-checks the predicate before each park and on every wake.
  - Captures and gates filter on `closing` after they read the snapshot.
  - An ack or rewind report from a closing slot is ignored.
  - The slot's prune lock stays at its last durable locator until deregistration completes: b2's prune bound and b3's predicate.

b1 has no subscribers. Of these rules only H1 and H3's release wait have code there, and H2's channel exists and carries only `T_ack`. A-40 and A-41 run against the real W in E-b1.2's follow-up.

**E-b1.6 b1's hub has no registry** (ruling Q2). b1 is subscriber-free, so `SubscriberHub` is a CH2-owned facade with an empty registry:
- `publish_durable` records `T_ack`;
- the capture sets and gate targets are empty, so steps 3b and 6 never wait;
- `predicted_locks` is empty.

b3 replaces its internals with SB5's `TipCell` (`register(Needs)`, `send_control`; SB0 §12.1's CH2b row). The API stays CH2's.

**E-b1.7 DEC-164 covers every wait, not only engine writes.** DEC-164 says no storage write waits without observing the background error. CH2b extends that to every wait in W, V and the drivers.
- **The rule.** A wait is a loop:
  1. Register a wake handle with the latch.
  2. Check the latch and the wait's predicate.
  3. Park only if neither holds.

  `GlobalFailStop::trip` stores the failure, then wakes every registered handle, so no wake is lost (the same order as H7's predicate). The rule covers:
  - `ThreadDriver`'s `Wait(..)` (acks, rewinds, `PruneClears`);
  - V's CRITICAL, ALWAYS, backpressure and `-dbcrashratio` waits;
  - manual prune's wait for step 12;
  - V's idle loop.
- **`GlobalFailStop` (§2.10) gains the wake registry.**
  - `check()` returns the latched failure as `Fatal`.
  - `wait_tripped(timeout)` parks until a failure is latched or the timeout passes, and reports which.
  - Both land in b1 with `ThreadDriver`, their first consumer (BL-72).
  - The handle is a condvar permit shaped like SB5's `Parker`. b3 moves to SB5's `Notify` when the hub wraps `TipCell`. The latch types move to `dash-kv` with SB10.
- **No wait inside a call that cannot see the latch.**
  - Engine writes use DEC-164's no-wait write.
  - `sync_wal`, `fdatasync`, `fsync` and `flush_memtables` (with `allow_write_stall`) are I/O that returns or fails (§4.4.2's first assumption).
  - The `BlockFiles` mutex is held only for I/O (rule 4).
- **No new tuning (BL-75).** CH2b changes no `RocksConfig`: no write-controller delay, no slowdown below a stop, and at most 3 write buffers per CF. A lever that needs any of these needs a new no-wait design first.

**E-b1.8 BL-72's carry-forward in b1.**
- **Tuning.** `FlushMachine` takes its batch bound S and its accounting (R, `after_bk`) from the `EngineTuning` it was given, never from `RocksTuning`'s constants. The custom-tuning test gains its `FlushMachine` legs and asserts the real step-11 `after_bk`. The driver legs (journal budget, deletion span) are b3's.
- **Wipe.** Recovery resumes `wiping` before it reads any marker (E-b1.3).
- **Cursors after the post-Bk memtable flush** (CH2a a3 Opus high O2).
  - A copy-point flush (§2.6) runs step 11 in this order: Bk → `after_bk` → take the `GuardedSnapshot` → drop the `Arc` → `FlushDone`.
  - So the RPC's cursor never pins the memtables Bk filled. A cursor that did would keep writers refused as stalls (DEC-164) for the whole scan. The scan would never end if the scanner waited on V while V waited on W.
  - Other flushes keep step 11's order, and `after_bk` runs after `FlushDone`.

**E-b1.9 b1 merges before its CRASH suite** (Opus high m4, 2026-10-11). b1's last commit, the ST3a CRASH suite ("commit 6"), waits for ST3a's CP3.4, and b1 merges without it. Each item of §7.2's b1 acceptance is in the merge or owned by commit 6, whose merge completes b1. The one exception is D14's regtest run, which moves to CH5. Nothing in the row is dropped.

| §7.2 b1 item | In the merge | Owned by commit 6, and why |
|---|---|---|
| the trigger-table unit test, with `CoinsBoundary`'s mapping onto Core's modes (O-R6-m5) | `flush::policy`'s tests (`the_coin_side_maps_onto_cores_modes`, the timers, in flight, index-only, step 0) | — |
| the `-dbcrashratio` synchronous-freeze smoke (D14) | at unit level: `dbcrashratio_needs_the_synchronous_freeze` rolls the crash inside a flush. The block whose submission fails has a durable marker and index entry with the hook, and neither without it | — (the regtest run moves to CH5's `feature_dbcrash` line, §7.4: b1 has no node to run it against) |
| the copy-point snapshot test (§2.6, O-R6-m2) | `a_copy_point_snapshots_the_flushed_tip`, `a_copy_point_runs_the_hook_before_the_release` | — |
| `flush-split`'s barrier count | the split test's step order (the pre-Bk `sync_wal`), and the PF0 row's gated syscalls | — |
| PF0 counters baselined | the six rows of `contrib/node/perfgate/workloads.toml` | — |
| ST3a's S1, S3, S4, S4b, S7 (CH2 half), S8, S9, S12; C1, C2, C4–C7, C9, C11's CH2 half and C12; the T and X lists; the production `RefExtractor`, `CoinsSut` and `ChainSut` | — | they run on ST3a's harness as of CP3.4 (DEC-185), through the SUTs commit 6 adds. S1, S4 and S4b run as `subscribers: None` variants, and commit 6 checks X6, X8, X13, X14, X16a, X16b and X21 itself (ST3a's answer, 2026-10-11) |
| FA7 | — | **Landed in commit 6** (CP6.2). It asserts over `ChainSut`'s marks on the CRASH model's forward runs: no verdict, event, status change or reply between a read fault and `Abort` (§6.4). b1 alone has no forward run to assert it on. Its units cover the trip and the abort (`GuardedEngine`'s read path; a3's `corrupt_coin_aborts`) |
| the `coins_replay_blind` smoke | — | **Landed in commit 6** (CP6.3). Its windows come from `ChainSut`'s mini connect (connects, reorgs, connect-then-disconnect, §6.5), and its oracle is `RefExtractor`'s reference. b1's recovery tests replay fixed windows |
| PR-gate budget measured | — | **Landed in commit 6** (CP6.3): 31.6 CPU-s at 699256754. It is the CRASH suite's run time |

b1 also gains two power-loss tests the row does not name (Opus high m1). After a flush's release, one-batch and split, a power loss keeps `meta.tip = new` with no marker (`a_released_flush_survives_a_power_loss`). After recovery, it keeps the converged tip, and a failed pre-final barrier leaves the marker (`a_finished_recovery_survives_a_power_loss_and_finalizes_only_after_the_barrier`). The CRASH suite's C-cases cover the same states exhaustively.

**Out of b1** (agreed 2026-10-10):
- BL-72's journal and deletion-span legs: b3.
- O7, `-reindex-chainstate` wiping the InstantSend and LLMQ stores: CH5.
- The `delete_range` reclamation caveat: CH5.
- BL-75's measurements: CH2c.

**E-b1.10 b3's items** (CH2b b3, branch node/ch2b-b3; D-rows in `CH2b-b3-notes.md`). Every b3 item above landed:

| Item | Where |
|---|---|
| E-b1.4: the subscriber control `ChainStateFlushed { locator(new) }` with `FlushCommit` | W sends both at step 5 through `ChainFeeds::send_control` (D2, D3, D7); the driver's `on_control` commits on it, capped as Core's `BaseIndex::ChainStateFlushed` |
| E-b1.5: H6, no tip mutex on a live subscriber's event-to-ack path | `a_live_subscriber_acks_while_v_holds_the_tip` |
| E-b1.5: the slot's prune lock held at its durable locator until deregistration | `a_closing_slot_holds_its_prune_lock_until_deregistered`; step 0's predicted and step 7's actual locks (FA1, T16, T30, T32) |
| E-b1.6: the hub over SB5's `TipCell` (`register(Needs)`, `send_control`) | `SubscriberHub::with_feeds` over a `FeedTable` (D4) |
| E-b1.7: SB5's `Notify` instead of the condvar permit | W's wake is a `Notify::Task` on SB5's close hook; V's wait for W runs the pending fanout and takes the producer's wake (D23) |
| E-b1.8 (BL-72): the journal-budget and deletion-span legs | `DriverConfig::from_tuning`, asserted in `tuning.rs`'s custom-tuning test |
| §4.4.3's model ported | `tests/subscriber_liveness_model.rs`, rev3's 120 configurations (D1; the PR runs a 20-configuration subset) |
| the threaded watchdog test | `subscriber/watchdog_tests.rs`, 10⁴ schedules per shape, and X15 (D25). It found D24's and D27's liveness gaps |
| the subscriber-bearing CRASH scenarios moved out of b1/b2 (Opus F11) | `tests/crash_subscriber.rs` (D20, D21; S11c with the driver's prune refusal, D31) |
| `chainstate_crash` and `coins_replay_blind` | node-fuzz model targets (D28) |

---

## Changes in rev5

Rev5 answers Opus's r6 pass (`reviews/CH2-design-r6-opus.md`, CHANGES-REQUIRED) under **DEC-102**, which drops the Core-flush emulation. DEC-95, DEC-96, DEC-99 and DEC-100 stay. The handoff (F10) and the liveness model are unchanged, so the model was not rerun.

**DEC-102 in one paragraph.** `have_in_cache` answers from CH2's own in-memory layers, active then F, and never reads prefetch or the DB, as Core's `HaveCoinInCache` reads only `cacheCoins`. It is `&self`, infallible and O(1). It equals Core's answer whenever no flush separates the coin entering the cache from the query. Otherwise the ATMP reason and the P2P orphan-parent request may differ: that is **D12**, reinstated as a declared allowed difference and listed for compat ALLOWED-DIFFS. `CoreCacheShadow`, `CoreCacheMeter`, the `CoreModelUsage`/`NativeUsage` newtypes, any `CoreFlushModel` and `core_meter_vectors` are removed with their acceptance rows and §7.4 text. **The native ledger is the only counter.** The history sections (rev2–rev4) keep their wording as history.

**Disposition of every r6 finding**

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **O-R6-M1** `feature_dbcrash` cannot pass with asynchronous flushes | major | **Fixed** with a declared test hook, **D14**: under `-dbcrashratio` > 0 every freeze is synchronous, and V waits for `FlushDone` at the call site. A crash then happens only inside the flush of the block being submitted, after its B1 made the index durable, so recovery lands on the test's `expected_tip` as Core's does. The test's lines are cited (`feature_dbcrash.py:62-64, 71-74, 76-103, 105-123, 147-151`). There is a b1 smoke that fails with the hook off and passes with it on, and CH5's acceptance line runs the test in this mode | §3.3; §3.9 D14; §4.6; §6.6; §7.2 b1; §7.4 CH5 |
| **O-R6-M2** shadow epochs need Core's own flush model | major | **Moot under DEC-102:** no Core flush points are modelled. Its statsd note stays, as a fact: rdashd has no `-statshost` ALWAYS source in CH2's scope | §2.4.5; §3.3 |
| **O-R6-M3** meter order in a block's `BatchWrite` is not determined | major | **Moot under DEC-102:** the meter is removed. The fact that Core's flush point is itself salted is kept as a reason in §2.5 and §2.4.5 | §2.4.5; §2.5 |
| **O-R6-M4** `have_in_cache` was fallible and read the DB | major | **Resolved by DEC-102:** it reads active and F only, and is `&self`, infallible and O(1). a2 counts zero DB and prefetch reads on every path | §2.4.1; §2.4.5; §7.1 a2 |
| **O-R6-m1** the shadow's event sources were incomplete | minor | **Moot as a shadow hook:** membership follows our own layers, whatever filled them. The fetch-site table is kept, with the owner of each Core `CoinsTip()` reader, because a non-caching port makes D12 more frequent. The owners get a §7.4 clause | §2.4.5; §7.4 |
| **O-R6-m2** copy-point snapshots could be overtaken by the next flush | minor | **Fixed:** W takes a `GuardedSnapshot` between Bk and `FlushDone` and returns it with the ALWAYS reply. The RPC reads `meta.tip` from it and asserts no marker. b1 holds a pending cause across a copy point | §2.6; §7.2 b1 |
| **O-R6-m3** does the child cache its inputs? | minor | **Fixed** to §2.5's rule: `have_inputs` caches in active only, and the child holds no copies. §2.4.2 and §3.2 are corrected, and a2 checks it | §2.4.2; §3.2; §7.1 a2 |
| **O-R6-m4** "no shard grows" is false at small B; the floor margin is thin | minor | **Fixed:** growth events are counted per budget and gated at zero only at B_DEC43. At small B the trigger is stated as pure density cost. The margins are restated (below) | §2.3; §5.5; §5.6; §7.1 a1 |
| **O-R6-m5** a2 relied on CH2b's `FlushPolicy` | minor | **Moved, not stubbed.** a2 tests `core_size_arith` and a new CH2a function, `CoinsCache::boundary_state` (THRESHOLD, LARGE, CRITICAL on native Σ). The mapping onto Core's modes moves to b1's `FlushPolicy` tests | §2.4.1; §2.5; §3.3; §7.1 a2; §7.2 b1 |
| **O-R6-m6** a grown F plus a fresh presize stalls V twice | minor | **Fixed:** the new active is presized to `min(f·(B − R − B/16), B − R − B/16 − F.allocated_bytes())` and can still grow. a1 reports the stall count both ways | §2.3; §7.1 a1 |
| **O-R6-m7** §10.1 missed P10 and deregistration, and mixed in CH2 duties | minor | **Fixed:** P10 (the block index persists in CH2's B1, in the batch with `meta`, the marker and evo; recovery loads `IndexAccess` from it) and **F11** (deregistration is final). Each F row is labelled *producer* or *CH2*. P9 is marked "if SB0 shares the mempool" | §2.9.1; §10.1 |
| **O-R6-m8** shard routing must avoid hashbrown's bits | minor | **Fixed** as a requirement: bits disjoint from h1's low bits and h2's top 7 (`sharded.rs:94` routes on 25..56). Covered by a unit test and by a1's PMU line | §2.3; §7.1 a1 |
| **n1** `Usage` had no field for the shadow's sets | nit | **Moot:** there are no sets | §2.4.5 |
| **n2** `LongRef`'s arena variant; `held_out` atomicity | nit | **Fixed:** `LongRef` is `Box<[u8]>` only, and `held_out` is a shared `AtomicU64` | §2.2; §2.5 |
| **n3** `&dyn EngineTuning` is not writable | nit | **Fixed:** `T: EngineTuning` throughout | §2.5.1 |
| **n4** F10 listed before F9 | nit | **Fixed** | §2.9.1 |
| **n5** the wipe was covered only by a unit test | nit | **Fixed:** new C12 sweeps `wipe_chainstate` and its resume with both crash kinds and nested crashes. It runs in b1 | §6.4; §7.2 b1 |
| **n6** rebuild latency | nit | **Fixed:** a1 reports per-block rebuild time at p99 against metric #5 | §2.3; §7.1 a1 |
| **Sol G-R5-n1** "20,000 entries grows no shard" was stated as certain | nit | **Fixed:** "ordinarily". A shard exceeds its margin with ≈ 1.9 % probability per such block (union bound), and the table then grows, which is permitted | §2.3 |
| **Sol G-R5-n2** headroom before CRITICAL | nit | **Fixed:** the actual table is 109.5 MiB against the 110.4453125 MiB cap, so headroom is ≈ 17.7 MiB − prefetch (B/16 plus rounding slack), not "at most B/16 − prefetch" | §2.3 |
| **"Things CH2a must get right"** | list | **Folded into §7.1,** revised: items 1–4 and 10 are gone with DEC-102 and replaced by the `have_in_cache` rule. The rest are kept, with the presize rule, `boundary_state` and the floor re-run added | §7.1 |

**The fallback's margins, restated** (a1 kept the fallback; a1's head is `57785c8b`; PF1; DEC-108):
- **Floor at 4 MiB:** **1.043**, re-run with rev4's trigger and churn rebuild and m6's presize in place. rev5 expected ≈ 1.025–1.035 (1.044–1.054 without the trigger, less about 1/56). The rebuild up to the trigger raised the margin, and m6 lowered it again at small B.
- **DEC-43:** **1.216× Core's resident coins** (2,676,754 against 2,200,586, Core's count at a P2PKH coin's 115.67 B; at Core's least bucket share, ≈ 112.05 B a coin, still ≥ 1.178×); 1.174× as the sweep's geometric mean.
- **The fallback alone does not reach the 2× target.** It lowers misses; the rest of the 2× must come from CH4, CS6 and the payee queue (§5.6).

---

## Changes in rev4

Rev4 answers Sol's r4 closure (`reviews/CH2-design-r4-sol.md`, CHANGES-REQUIRED) under **DEC-99**, which clarifies DEC-95. DEC-95, DEC-96 and the bindings listed for rev3 stay. The handoff (F10) and the liveness model are unchanged, so the model was not rerun: §4.4.1 now states the rule the model already implements (`liveness_model_v3.py:303`), and Sol's r4 reruns of it stand.

**Two counters (DEC-99).** CH2 keeps two memory numbers. Each has its own type, and every consumer and test names the one it uses:

| Counter | What it is | Its only consumers | Tested by |
|---|---|---|---|
| **native Σ** (`NativeUsage`, from `Usage::total()`) | CH2's own allocations (capacity, not occupancy): active, F, prefetch, overlay, undo copies still alive (`held_out`), plus the engine reserve R | the physical flush policy (`CoinsBudget::size_state` → `CACHE_LARGE`, `CACHE_CRITICAL`, V's wait), the "Cache size … exceeds" log line, the peak evidence | §6.7's tagging allocator, which proves the native ledger exact and nothing about Core; a2's witnesses; the CH2c and DEC-43 memory gates |
| **Core-model usage** (`CoreModelUsage`, from `CoreCacheMeter`) | Linux Core's `DynamicMemoryUsage()` for the map Core would hold since **Core's** last flush | the shadow's Core flush epochs only (`CoreCacheShadow::core_size_state` → `on_core_flush`), which decide `have_in_cache` | a2's `core_meter_vectors` against a pinned-Core probe, after every op; c3's L1 against O2a-CO's per-op `DynamicMemoryUsage` (DEC-94 Q3) |

Both pass their own counter to one pure function, `core_size_arith`, which is Core's `GetCoinsCacheSizeState` arithmetic. **Our physical flush points are not Core's, and the design no longer claims they are.** On the same blocks, native density can make our LARGE or CRITICAL fire later than Core's, and F, prefetch, R or layout growth can make them fire earlier. Sol's witness, a 20,000-output block at `-dbcache=4`, gives native LARGE against Core's CRITICAL. Flush timing is neither consensus nor RPC-visible (D1, D11). The one Core-visible effect of Core's flush points, `HaveCoinInCache`, follows the meter.

**Disposition of every r4 finding**

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **G-R4-M1** the threshold function was compared on CH2's bytes, not on Core's accounted usage | major | **Fixed per DEC-99.** The two counters are newtypes (`NativeUsage`, `CoreModelUsage`) over one shared `core_size_arith`. Every claim that a physical flush decision equals Core's is removed (§0, §2.5, §3.3, §4.2, §6.7, a2, A-1, D1, D11). a2's boundary table now tests only the arithmetic and says so. a2 adds **the meter-against-Core acceptance test**, `core_meter_vectors`. Its vectors come from a pinned-Core probe: a real `CCoinsViewCache` over a stub base at v24.0.0-rc.3 with Linux libstdc++. They cover the constructor's first pool chunk, chunk and free-list accounting, bucket growth from the unreserved empty map, script capacity per source path, and `ReallocateCache` at a flush, and the test checks the meter after every op. c3 repeats the check on L1 streams against O2a-CO. Sol's 20,000-output block becomes an a2 case: the physical decision follows native Σ (LARGE), while the shadow's epoch follows the meter (CRITICAL). DEC-99's native-peak gate is in §5.5, and §10 Q1 asks the manager to confirm how it is read | §0; §2.4.5; §2.5; §3.3; §3.9; §4.2; §5.5; §6.7; §7.1 a2; A-1; §10 |
| **G-R4-m1** the sequence rule contradicted F10 | minor | **Fixed.** §4.4.1 rule 1 uses F10's cut: `seq_synced` is the next seq at the handoff, the driver sets `last_seq = seq_synced − 1` and clears `skipped`, and an event still pending at the cut is never delivered. A `FlushCommit` for that event is case A. §4.4's "ignored" paragraph now points to §2.9.1: events from a dropped or compressed lineage are ignored, and a strict-ancestor parent is `FeedOrder`, which trips the latch. The X28 convention is gone from the operative text; X28 remains the plant of it | §4.4; §4.4.1 |
| **G-R4-m2** a borrowed input became an owned undo coin by a "move" | minor | **Fixed.** The child holds no pointer or offset into a parent layer: it resolves by key, and borrows end at the next `&mut` call, so parent rehashes are safe. `spend` moves only bytes the child owns. For any other coin it copies: inline coins by value, long coins into a new box of exactly their length. The parent is unchanged until `commit`. A long undo coin owns a `LedgerBox`, counted in `Usage::held_out` from creation until it is dropped, wherever that happens, so `tagged == native Σ − R` at every op boundary. `held_out == 0` at Core's check points. The DB path's "Vec to Box without a copy" now says that conversion may reallocate. a2 adds the long-input abort witness and the undo round trip | §2.2; §2.4.2; §2.5; §6.7; §7.1 a2 |
| **G-R4-m3** the growth cost was a mean, and "never" was false | minor | **Fixed.** Worst cases replace the mean. One doubling allocates up to 4.5625 MiB (a 32,768-bucket shard) while 2.28125 MiB lives, and the j-th repeat costs 2^(j−1) times that. With F in flight the shards cross together and the layer's table doubles (≈ +110.4 MiB kept). A maximal block near the trigger does the same in one block, against ≈ 15.3 MiB for the arena. The fallback now also freezes at a block boundary when a shard of active is within 1/64 of its buckets of its growth load. Below that trigger, a shard whose room is used up by hashbrown's DELETED tombstones is rebuilt at its own size instead of doubling. With no F in flight, a block adding up to ≈ 20,000 entries then grows no shard. The cost is ≈ 1.8 % density, now in the estimate (DEC-43 ≈ 2.64 M; ratio ≈ 0.823). The "never" and "stops the layer" claims are removed. The Core comparison uses Sol's libstdc++ bucket counts, which a2's meter vectors confirm at the pin | §0; §2.3; §2.5; §5.2; §7.1 a1 |
| **G-R4-n1** rule-O remnant, ceilings, missing hook | nit | **Fixed.** D11 is rewritten with no T. §5.2's and §5.3's ceilings are now estimates with measured overshoot, and Core's shutdown cache is no longer stated as ≤ B − 10 MiB. `SubscriberData::max_entry_bytes()` is declared: what entry bytes include, that admission bounds the retained journal, and that a computed but unadmitted entry is transient memory visible in the process evidence | §2.9; §3.9; §5.2; §5.3 |

The rev3 rows below that quote "≈ 3.45 MiB per shard" and "≈ 34 MiB" for Core are history; §2.3 has the corrected figures.

---

## Changes in rev3

Rev3 answers Sol's r3 closure (`reviews/CH2-design-r3-sol.md`, CHANGES-REQUIRED) under **DEC-95**. DEC-95 has CH2 follow Core's semantics: the coins cache may grow during a block, and it is checked only at Core's `FlushStateToDisk` points with Core's LARGE and CRITICAL arithmetic. Rule O is removed. DEC-78, DEC-80 (except Q4), DEC-81, DEC-82, DEC-85, DEC-92 and DEC-94 (except Q1) stay binding.

**The liveness model (rev3).** `/work/scratch/ch2-rev3/liveness_model_v3.py` extends rev2's model with the two-step catch-up handoff (G-R3-M4) and a new invariant: no Synced subscriber ever ignores an event that Core would rewind for. Results (`run-*.txt` there):
- **Grid:** 120 configurations, 19,367,358 states, all pass.
- **Directed:** rev2's 8 checks, all pass unchanged.
- **Handoff coverage:** 983,348 transitions in which the re-check failed because V's tip moved between catch-up and `subscribe_at`, and 160,320 handoffs taken with an event pending emission, over its 60 configurations.
- **Plants:** X17, X18, X19 and X22 are caught as before. **X27** (no live-tip re-check: Sol's producer) and **X28** (the pending event delivered across the cut) are caught. X20, X23 and X24 are caught on the injected states (directed). rev3 did not re-sweep the grid for them; rev2's sweep found them unreachable there.

**Disposition of every r3 finding**

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **G-R3-M1** the allocation list and `T_max` disagree | major | **Moot under DEC-95:** no `T(op)` or `T_max` is claimed, so there is no proof to repair. Two facts it found still matter for exact accounting, and both are fixed: a long coin's store holds the whole serialized coin, not `len − 34`; and ownership of DB-read buffers, restored coins and freed capacity is now specified. Growth transients are counted in Σ and compared with Core's (arena 20 MiB / shard 3.45 MiB / Core ≈ 34 MiB at DEC-43 sizes) | §2.2; §2.3; §2.5 |
| **G-R3-M2** streaming admission and the one-retry argument | major | **Moot under DEC-95:** streaming admission, abort-and-retry and the oversized path are removed. Nothing checks the budget inside an op | §2.5; §4.2 |
| **G-R3-M3** the presized fallback cannot complete an oversized op | major | **Fixed:** every layout grows past its presize. A fallback shard that fills inside an op doubles (≈ 3.45 MiB transient at DEC-43, 1/64 of the layer); long coins have no byte cap; the arena already grew. The layout gate runs both layouts block by block under the same policy, with growth in Σ and peak Σ reported, so neither wins by hiding a transient. The two witnesses run on the kept layout: a 50,000-output block and a long-input connect with its disconnect, at `-dbcache=4` (B = 1.75 MiB), txindex off | §0; §2.3; §7.1 a1, a2; A-3 |
| **G-R3-M4** no atomic catch-up-to-live handoff in F1–F9 / P7 | major | **Fixed:** new **F10** and `FeedSource::subscribe_at`. It is linearized with V's tip changes and does three things atomically: check `p == V's tip` (else `Moved`), commit, and cut at the next `seq`. The pending event at the cut is **excluded**. `ChainSnapshot` is coherent with a `tip_seq`. F1 is now "exactly the events from the cut". Non-applying events are specified: blocks descending from a dropped connect are ignored, and a block whose parent (or itself) is a strict ancestor of p is `FeedOrder`, which trips the latch. The model has the two-step handoff and the invariant; X27 and X28 are caught; C8(j). P7 rewritten for SB0 | §2.9.1; §4.4; §4.4.3; §6.4; §10.1 P7 |
| **G-R3-m1** byte admission; inexact-only exemption | minor | **Fixed:** inexact-only subscribers keep **no journal** (O(1) progress), so they can never post `FlushDue(DMax)`, whatever the engine. Exact subscribers compute the entry, then admit iff it fits; otherwise the subscriber is at the limit. An entry larger than the budget is admitted alone into an empty journal, which bounds the journal by `max(budget, largest entry)`. Each `SubscriberData` declares `max_entry_bytes()`. C8(k) | §2.9 items 2, 6; §5.9; §10 Q3 |
| **G-R3-m2** the reserve hardcodes RocksDB | minor | **Fixed:** `CoinsBudget` takes `EngineTuning::batch_reserve(S)`. The Rocks number lives only in `RocksTuning`, with its batch-peak argument (preallocated rep, `rocks.rs:605`; `size_bytes` exact for it, `:484`). `MemoryTuning` is 0. Tests assert `after_bk`, not a Rocks flush. a3 adds a custom-tuning test | §2.5; §2.5.1; §6.7; §7.1 a3 |
| **G-R3-n1** stale sketches and text | nit | **Fixed:** §2.6 signatures over `GuardedRead`/`GuardedSnapshot` returning `CoinsReadError`; §4.2 step 5 says `tip_seq`; §3.1 names `with_high_water`/`stage_disconnected`; §5.2 and §5.7 follow the full-budget gate; the v23 `AbortError` clause is gone from §3.6; §2.11 MP1 says bookkeeping at `:963-974` and the `Uncache` calls at `:1744, 1775`; F3 states atomic versus independent fanout and what the model explores | §2.6; §3.1; §3.6; §4.2; §5.2; §5.7; §2.9.1 F3 |
| Sol's spot check: `LookupOne`'s early return | note | **Fixed:** CH2 deletes height rows that Core never deletes, so IX1's lookups fall back to the archive when the height row is absent. C8(c) covers it | §2.9 item 4; §2.11 IX1 |
| Sol's note: Core's flush count is an estimate | note | **Stated as an estimate**; DEC-94 Q2 gates stall and bytes, and M3 measures the count | §5.9 |

**r3 PARTIAL items, closed or moot**

| Item | Now |
|---|---|
| G-R1-M1 (completion under a ceiling) | **Moot** (DEC-95: no ceiling); completion is by growth (G-R3-M3); the release race stays closed |
| G-R2-M1 (long input coins) | **Moot** for the bound; whole long-coin bytes are counted exactly (G-R3-M1 item 1) |
| Opus F1 (mid-block budget) | **Moot** (DEC-95); peak gated as evidence (§5.5) |
| O-R2-M4 (forced-flush cliff) | **Closed:** no journal for inexact-only subscribers (G-R3-m1); Core's count labelled an estimate |
| O-R2-M5 (fallback cliff) | **Closed:** the presize is not a cap (G-R3-M3); the gate runs under one policy for both layouts |
| O-R2-m1 (growth missing from T) | **Moot** (no T); growth is in Σ |
| O-R2-m10 (journal memory) | **Closed:** byte admission and the oversized-entry rule (G-R3-m1) |
| SubscriberFeed F1–F9 / P1–P9 | **Closed:** F10 and P7 (G-R3-M4) |
| EngineTuning isolation | **Closed:** the reserve goes through `EngineTuning` (G-R3-m2) |
| O-R2-n6 (fixed-table baseline) | **Moot** for a bound; capacity, not occupancy, is in Σ |

Rejected: none.

---

## Changes in rev2

Rev2 answers the two r2 closure reviews:
- `reviews/CH2-design-r2-gpt.md`: REWORK, with one new major (here **G-R2-M1**) and one minor (**G-R2-m1**), plus notes.
- `reviews/CH2-design-r2-opus.md`: APPROVE-WITH-CHANGES, staged. It has six new majors (**O-R2-M1…M6**), three partial closures (F1, F4, F5), minors **O-R2-m1…m10** and nits n1–n7.

It also applies the manager's rulings on D12 and D13. DEC-78 and DEC-80 stay binding.

It lands in two commits:
1. **Step 1, `0c7abf97`**: O-R2-M5 alone, in §2.3 and §7.1 a1.
2. **This commit**: everything else.

**The liveness model (rev2).** `/work/scratch/ch2-rev2/liveness_model_v2.py` is a clean rewrite, now an abstraction with stated limits (§4.4.3):
- stepwise emission (a tip change gets its `seq` at once, its event is pending until every queue has room, and V may freeze meanwhile);
- the queue bound asserted in every state;
- an `invalidateblock` reorg mode;
- the sequence-number case test;
- wake on enqueue and the empty flush;
- the v24 capped eager commit.

Results (`run-*.txt` there):
- **Grid:** 120 configurations, **21,656,118 states**, all pass.
- **v23-timing subset** (no eager commit): 40 configurations, 6,764,561 states, all pass.
- **Directed:** GPT's exact B1 schedule, Opus's R2-M6 configuration, and GPT's injected parked state, each with its planted counterpart failing as expected.
- **Planted in the grid:** X17, X18, X19, X22 are all caught.
- **Opus's own `stepwise_seq.py`** (60 configurations, 4,615,988 states) is re-run: all pass, X19 caught.
- **Before the empty-flush rule** the rev2 grid (90 configurations, 10,501,100 states) also passed.

**Disposition of every finding**

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| **G-R2-M1** rule O's `T(op)`/`T_max` ignore long input coins | major | **Fixed.** `T(op)` enumerates every allocation: child and active entries, output long bytes exact, input long bytes counted once (the child references the parent's bytes), growth reserved at the boundary, shadow sets, scratch. Connects use **streaming admission** with at most one abort-and-retry; disconnects are exact from undo. **`T_max` ≈ 475.8 MiB (arena) / 471.3 MiB (fallback)**, derived from consensus limits and asserted by a unit test; rev1 said 47.7 / 90.8 MiB. That changes DEC-80 Q4's number (§10 Q1). Core needs ≈ 941 MiB for the same block | §2.5 |
| **O-R2-m1** / F1 partial: layer growth missing from T | minor | **Fixed:** `reserve(n_in + n_out)` at the op's start; growth is item (e) of T; the presized fallback never grows | §2.3, §2.5 |
| **O-R2-M5** fallback cliff; gate sample points | major | **Fixed in step 1** (`0c7abf97`): a presized, never-rehashing fallback; the gate decides on budget bytes per resident coin at the DEC-43 budget and over a 4 MiB–1 GiB sweep, with a floor at Core's resident count. rev2 aligns §0, §5.2 and A-3 | §2.3, §7.1 a1 |
| **O-R2-M1** rows invisible while `best_block()` reports them | major | **Fixed:** height and inexact rows go into one Buffered batch per block, and `best_block()` advances after it returns (Core's `CustomAppend` → `SetBestBlockIndex`); keyed rows are served by `SubscriberDriver::read`, journal → DB. FT-shaped C8(g) | §2.9 items 1–2; §6.4 |
| **O-R2-M2** blockfilter position moves backward | major | **Fixed:** a process-wide monotone high-water applied to **every** state move; archive rows written only after `sync_files`; at open, `max(durable, file length)`. C8(c) reads archived filters back after a reorg above L and new connects | §2.9 items 3–4 |
| **O-R2-M3** row classes cannot express IX2 | major | **Fixed:** a **keyed** class written only in locator batches and journal-held above L, plus `stage_disconnected` (rev0's path, restored; Core's `CustomRewind`/`RewindBatch`). Disconnects above L pop the journal; rewinds below L apply undo rows computed from block and undo files. C8(h) is an IX2-shaped subscriber; the three FTs are listed | §2.9 items 1, 4, 5c, 8 |
| **O-R2-M4** `D_max = 1,024` dominates flushes in the DEC-43 cell | major | **Fixed:** `D_max` is per subscriber. It is **unbounded for inexact-only subscribers** (txindex; `llmq/` as LK1b declares it) and journal-byte-sized otherwise. Arithmetic in §5.9: Core flushes ≈ 2 times over 1→453,371 (ST1's Core-model arm) and ≈ 10–15 over the pre-DIP3 window; `D_max = 1,024` would add ≈ 1,004; rev2 adds 0. The proxy now carries a txindex-shaped subscriber and reports flushes by cause. Our own threshold freezes are ≈ 2× Core's count at f = 1/2 (§10 Q2) | §2.9 item 6; §5.5; §5.9 |
| **O-R2-M6** freeze before `Connected(new)` is emitted | major | **Fixed:** Opus's rule. `seq` is assigned at the tip change; case B iff `tip_seq ≥ seq_synced ∧ tip_seq > last_seq ∧ tip_seq ∉ skipped`; the queued copy is always a no-op; Lemma 2 restated; `tip_seq` in `FreezePacket` (n2). Ported to the model with stepwise emission; planted X22 is caught | §4.4.1 rule 1; §4.4.2 |
| **G-R2-m1** model queue bound for reorgs | minor | **Fixed:** one disconnect per step, emission only with room, Q asserted; §4.4.3 now states the model's limits (AG EF done, fairness, what is abstracted) | §4.4.3 |
| GPT r2 notes: fairness, coin-empty changes, round trip, parked + later disconnect | note | **Fixed:** the fairness assumption is stated; **wake on enqueue**; the **empty flush** for a pending `FlushDue`; the injected state is a directed regression (X23, X20 + X24) | §4.4.1 rules 3, 5; §4.4.2 |
| **O-R2-m7** Lemma 2's freeze condition (coin-free blocks) | minor | **Fixed:** freeze iff tip ≠ last frozen tip or active is DIRTY, else an empty flush on `FlushDue`; no ALWAYS shortcut while `FlushDue` is pending | §4.4.1 rule 5 |
| **O-R2-m8** model fidelity | minor | **Fixed** (stepwise model) | §4.4.3 |
| **O-R2-m6** archives lost on a crash | minor | **Fixed:** archive rows written at the disconnect (Buffered, after `sync_files`) | §2.9 item 4 |
| **O-R2-m10** journal memory | minor | **Fixed:** bounded in bytes (32 MiB default); keyed rows, archive sources and state only; reported by `engine-memory` and W-MC | §2.9 item 2 |
| **O-R2-m9** `GuardedEngine` snapshots and cursor | minor | **Fixed:** `type Snapshot = GuardedSnapshot`, `CoinsTable` typed over guarded handles and returning `CoinsReadError`, `corrupt` is `pub(crate)`; `corrupt_coin_aborts` adds the cursor and snapshot paths | §2.6, §2.10, §7.1 a3 |
| **O-R2-m2** / F4 partial: CH2c row and W-MC rule still said RssAnon + RssFile | minor | **Fixed:** both gate on `rss_anon_peak` under the common cap; RssFile and `memory.peak` are reported only | §5.8; §7.4 CH2c |
| **O-R2-m3** FA7's window | minor | **Fixed:** "from the fault until the epoch ends" | §6.4 |
| **O-R2-m4** D12 rationale wrong | minor | **Superseded by the ruling:** D12 withdrawn. A **Core-membership shadow** (§2.4.5) answers `HaveCoinInCache` exactly as Core does given Core's flush points, which a `CoreCacheMeter` reproduces; verified by O2a-CO; `mempool_accept.py`'s case is in a2 | §2.4.5; §3.9; §7.1 a2 |
| **O-R2-m5** D13 wording | minor | **Superseded by the ruling:** the capped eager commit at `LCA(pprev, commit_cap)` (v24 at the disconnect; v23 at the next connect). It only raises L, so prune locks never fall below their rev1 value and the Q10 lag cannot return. D13 withdrawn | §2.9 item 5b; §3.9 |
| F5 partial / n1: CH3a acceptance | minor | **Fixed** in §7.4's CH3a edit | §7.4 |
| n2 `tip_seq` in `FreezePacket` | nit | **Fixed** | §2.7 |
| n3 reads at open | nit | **Fixed:** `decode_state` may read the height row at L | §2.9 |
| n4 per-height prefix scan | nit | **Fixed:** `DurableBelow::height_rows` | §2.9 |
| n5 rewind commit deferral | nit | **Fixed:** at the next connect, `RewindRequest` or `FlushCommit` | §2.9 item 5c |
| n6 `RULE_O` cause; prefetch in the overshoot | nit | **Fixed** | §2.5; §2.7 |
| n7 D7 trailing bytes | nit | **Fixed:** `from_db` requires the decode to consume the value; a value with trailing bytes is Corruption too (Core's `Read` reads it as present) | §3.9 D7 |

Rejected: none.

### rev2 addendum: DEC-81, DEC-82, DEC-85 (pasta's direction of 2026-10-09)

| Decision | What changed | Where |
|---|---|---|
| **DEC-82** v24 only | Every Core cite is at v24.0.0-rc.3 only (132 paired cites and 4 special cases converted; history sections and §8 keep theirs). No rules struct: `IndexRules`, `InitRules`, `ChainstateRules` and "per profile" are gone. §3.8 is now the list of v24 behaviours CH2 implements unconditionally. The v23 lazy-commit variant of item 5b, IX2's legacy disconnect path, the v23 HaveCoin escape, exit status 0, the v23 EvoDB text, the v23 mempool term, the v23-timing model subset and the v23.1 fuzz share are dropped. D12's shadow was already cited at v24 only (`validation.cpp:970-979`, `mempool_accept.py:107`) | header; §0; §2.4.4; §2.5; §2.9 items 1, 4, 5b, 8; §3.1, §3.5.1, §3.6, §3.8; §4.4.3; §6.4 C8; §6.5; §7.1 a3; §7.3 c3; §7.4 |
| **DEC-81** SB0 | §2.9.1 states what a subscriber needs from its event source as `SubscriberFeed` with guarantees F1–F9 (ordering, `seq`, bounds, control events, wake, fairness, the flush barrier, data and ancestry, `best_block()` ownership); the driver no longer names CH1's bus. §10.1 lists what SB0 must preserve (P1–P9). CH2b is held until SB0 is approved | §0 item 8; §2.9, §2.9.1; §2.11 CH1 row; §4.4; §7.2; §7.4; §10 Q7, §10.1 |
| **DEC-85** DEC-43 re-check | Engine-specific parameters sit behind `EngineTuning` (§2.5.1): E's RocksDB mapping, R's native batch reserve, the post-Bk hook, the WAL settings, the journal budget behind `D_max`, the blind-delete span, memory diagnostics. The protocol uses only the `KvEngine` contract. The 0-DMAX result is engine-independent. A-2 is marked provisional | §0 item 5; §2.5, §2.5.1; §2.9 items 5c, 6; §4.2 step 11; §5.9; §9 A-2; §10 Q6 |
| DEC-84 platforms | Q4's recommendation now names every platform | §10 Q4 |

The liveness model is unchanged (no rule changed), so the rev2 results stand; only the v23-timing subset is retired.

---

## Changes in rev1

Rev1 answers both r1 reviews:
- `reviews/CH2-design-r1-gpt.md`: REWORK, with blocker B1, majors M1–M4 and minors m1–m3. Evidence is in `/work/scratch/ch2-gpt/`.
- `reviews/CH2-design-r1-opus.md`: APPROVE-WITH-CHANGES, with majors F1–F4 and F9–F12, minors F5–F8 and F13–F17, and nits N1–N8.

DEC-77 is binding and nothing here reopens it. The two places where rev1 needs a restatement of a DEC-77 item are §10 Q2 and Q3.

**The liveness model.** The new deadlock-freedom argument (§4.4) is backed by an explicit-state model checker: `/work/scratch/ch2-rev1/liveness_model.py`, with results in `model-results.txt`.
- **What it models:** V, W, one shared events thread or one thread per subscriber, Synced and Syncing mandatory subscribers, bounded queues with backpressure, `D_max`, freezes only when mandated, and reorgs of depth 0–2.
- **What it checks:**
  - safety in every reachable state: R11a, the data bound, and L on p's path;
  - AG EF done: from every reachable state, completion stays reachable.
- **What it found:** it caught GPT's exact B1 schedule and Opus's F9 schedule. It also found three further holes that neither review reported: X19 (§4.4 rule 4), X20 (rule 3) and a case-A gap (rule 1). All three are fixed below and planted as regressions.
- **The grid** (§4.4, 90 configurations, ≈ 1.78 M states, about 3 CPU-min) passes under rev1. CH2b b3 ports it as a Rust test.

**Disposition of every finding**

| Finding | Severity | Disposition | Where |
|---|---|---|---|
| GPT **B1** the out-of-band gate never completes an already-reached flush | blocker | **Fixed.** An outstanding `FlushCommit` is now *serviced* out of band: case A commits and acks at once; case B drains toward `new` with checkpoints only on that leg; off-path connects are dropped. GPT's schedule is a directed regression; X18 is the planted form | §4.4 rule 1; X18 |
| Opus **F9** a cycle through an in-flight F's step 6 on a shared events thread | major | **Fixed.** A subscriber never blocks a thread: at its limit without a commit it compresses its queue, then **parks** (option (a)). Opus's schedule is a directed regression; X17 is the planted form. One thread per subscriber is not required | §4.4 rules 2–3; X17 |
| *(model)* a backward B1 (a flush at a fork below a durable locator, e.g. after `invalidateblock`) makes that locator "ahead of G" (R11a) | new | **Fixed** with a **rewind gate** before such a B1 (X19). ST3a scenario erratum proposed (§10 Q1) | §4.2 step 3b; §4.4 rule 4 |
| *(model)* subscribers park on a connect their own queue later undoes; V has nothing to freeze, so nothing wakes them | new | **Fixed:** net-path compression before parking (X20) | §4.4 rule 3 |
| *(model)* `new` never delivered to a subscriber that turned Synced after `new` was disconnected; rev0's drop rule then dropped its whole branch | new | **Fixed:** case B only when `Connected(new)` is actually queued (`FlushCommit.connected_seq`) | §4.4 rule 1 |
| GPT **M1** / Opus **F1** no legal block-completion path under the hard ceiling | major | **Fixed** with the explicit overshoot rule O. Every cache-growing V operation declares a bound `T(op)` first; V freezes or waits at the boundary; an op larger than B is admitted with accounted overshoot ≤ `T_max`. The asserted invariant is restated. The release race is fixed: W drops its `Arc` before `FlushDone`, and V asserts it holds the last reference | §2.5; §4.2 step 11; §6.7; §10 Q4 |
| GPT **M2** WBM charging does not prove a hard engine bound | major | **Accepted.** E is now a *configured target* with a derived soft ceiling: non-strict LRU, `allow_stall = true`, no L0 pinning. Measured by a new `engine-memory` workload; the binding evidence is the DEC-43 `rss_anon` gate. GPT's native witness is cited | §2.5; §5.7; A-2 |
| Opus **F4** DEC-43 proxy memory criterion biased; regime unspecified | major | **Fixed.** The proxy, W-MC and the M3 cell run under ST1's cgroup cap (anon + page cache), gate time under the cap and memory on `rss_anon_peak`; `memory.peak` and RssFile are reported only. M3 exit text updated | §5.5; §7.4 |
| GPT **M3** no locator-target state contract (coinstatsindex's accumulator) | major | **Fixed.** `SubscriberData` declares row classes (height, archive, state, inexact) and a pure `State`. The driver keeps a journal of `(rows, state)` for `(L, p]`, so the state at any commit target X is exact. State keys are written only in locator batches | §2.9; §4.4 |
| Opus **F10** blind-delete-only rewind cannot keep Core's stale-block copies | major | **Fixed.** Rewinds stage archive rows (Core's `CopyHeightIndexToHashIndex`): rows above L come from the journal, rows ≤ h(L) from `DurableBelow`, which permits only barrier-covered reads. Item 13 holds. The two FTs are listed | §2.9; §4.4; §6.4 C8c |
| Opus **F2** O17 MPIX-11 has no home; a literal v24 port breaks R11a | major | **Fixed.** `SubscriberDriver::best_block()` follows `IndexRules.rewind_on_disconnect` (in-memory); durable rewinds happen only below L. The difference is a declared deviation; MPIX-11 is added to §3.8 | §2.9; §3.8; §3.9 D13; §6.4 C8b |
| GPT **M4** / Opus **F12** legacy undo: wrong branch; writes into hard-linked source rev files | major | **Fixed.** Resolution is by the spending block's **ancestry**; ambiguity or unavailability is explicit. Affected rev files are **rewritten into rdashd-owned inodes** (temp file, sync, rename over the link, dir sync); the source is never written. Unresolvable stale-branch blocks lose `HAVE_UNDO`. Fixture added | §3.5.2; §7.4 CH6a/CH5 text; §10 Q5 |
| Opus **F3** X16 undetectable | major | **Fixed.** The plant sites are named (X16a above the guard, X16b inside it). New forward assertion **FA7** (no verdict, event, status change or reply between a read fault and `Abort`) | §6.4 |
| Opus **F11** CH2b phase order vs acceptance | major | **Fixed.** b1 = flush + recovery, subscriber-free (ST3a `subscribers: None` variants, confirmed with ST3a's owner); b2 = prune, subscriber-free; b3 = subscribers. Subscriber-bearing scenarios move to b3 | §7.2 |
| GPT **m1** cache membership ≠ Core across a hidden freeze | minor | **Accepted.** Membership is defined (first-hit resolution; `uncache` touches active only). D12 is declared, and `diff_coins_ops` compares membership only in aligned mode | §2.4.1; §3.9; §6.2 |
| GPT **m2** / Opus **N4** startup order uses ancestry before the index | minor | **Fixed:** the startup order is reordered | §4.3 |
| GPT **m3** VARINT not numeric-order-preserving | minor | **Fixed:** "Core's serialized-key order", plus a 16383/16512 vector | §2.2; §6.1 |
| Opus **F5** T12, C11 and the connect halves of T20/T21 test stubs | minor | **Accepted:** moved to CH3a's acceptance | §6.4; §7.4 CH3a edit |
| Opus **F6** the user-visible text on a flush error | minor | **Fixed:** `*** ` log, generic user message, misc warning in RPC `warnings`, per-profile exit | §3.6; D10 |
| Opus **F7** `have_in_cache` union | minor | **Fixed:** first hit, unspent | §2.4.1; §3.1 |
| Opus **F8** D7 point reads; **DEC-78** (CS5a finding) | minor / ruling | **Fixed per DEC-78.** The abort is kept and declared as D7: Core's `CDBWrapper::Read` catches the decode failure (`dbwrapper.h:314-316 / :324-326`) and reads a corrupt coin as missing, which can reach `bad-txns-inputs-missingorspent`; ours trips the latch and aborts through `GuardedEngine::corrupt`. Added the test `corrupt_coin_aborts`, planted X21, and FA7. CS5a's `CoinsView` stays generic over `type Error`, so only `GuardedEngine` constructs one | §2.2; §2.4.4; §2.10; §3.9 D7; §7.1 a3 |
| Opus **F13** fallback `dirty_order` | minor | **Fixed:** dropped; shard iteration (salted, fixed under the seam) | §2.3 |
| Opus **F14** manual prune must be synchronous; deferral must keep the check armed | minor | **Fixed** | §3.3; §4.2 |
| Opus **F15** shutdown order and deregistration | minor | **Fixed:** `deregister`, Core's two-flush order for ND1, shortcut = "no DIRTY entry" | §2.9; §4.2 |
| Opus **F16** layout gate latency is wall-clock | minor | **Fixed:** PMU instructions and LLC misses per hit, plus a paired sign test. This restates DEC-77 Q5's measurement, not its threshold (§10 Q2) | §2.3 |
| Opus **F17** `StableBytes` Stacked-Borrows UB | minor | **Fixed:** `NonNull<[u8]>` from `Box::into_raw`, freed in `Drop`; Miri under Stacked and Tree Borrows | §2.4.2 |
| Opus **N1** convergence wording | nit | **Fixed**; `coins_replay_blind` generates connect-then-disconnect windows | §4.3; §6.5 |
| Opus **N2** `m_synced` lines | nit | **Fixed:** rev0's "correction" reverted; the assignment is at `index/base.cpp:78 / :90` (re-checked) | §2.9; §3.1; §4.4; §8 row 44 |
| Opus **N3** S from B(t) | nit | **Fixed:** S comes from the static coins share | §2.5 |
| Opus **N5** wipe resume order | nit | **Fixed:** the resume runs before the marker checks | §4.3 |
| Opus **N6** idle wake | nit | **Fixed** | §4.2 |
| Opus **N7** overlay counted in B | nit | **Accepted:** D11 | §3.9 |
| Opus **N8** IX1 note on copy timing | nit | **Accepted** | §2.11 IX1 row |
| GPT audit: u32 lock arithmetic must clamp like Core | note | **Fixed:** i64 with `max(1, …)` (`validation.cpp:2791 / :2999`) | §4.5 |
| GPT: `flush-split` must count protocol barriers, not OS fsyncs | note | **Fixed** | §5.7 |
| GPT: `max_total_wal_size` is a trigger, not a bound | note | **Fixed** | §2.5 |
| GPT: "we hold more recent coins at any time" overstated | note | **Fixed** | §5.2 |
| GPT: reorg benchmarks need a different branch; a prefetch completion after release; F-owned spent/recreated keys | note | **Added** | §5.8; §7.1 a2 |
| GPT: CS5a branch ahead of the design's recorded revision; Opus: CS5a already fallible at 17014d4b | note | **Recorded:** DEC-77 Q1 done | §2.11 CS5a row |

Rejected: none. Two findings are accepted as declared deviations rather than fixed, because exact Core equivalence would cost the design its asynchronous flush or its blind rewind:
- m1, cache membership across a hidden freeze;
- F2's durable half: v24 commits the locator at `pprev` on every disconnect, and we do not.

Each is justified in §3.9.

---

## 0. The design on one page

1. **CH2 is split** (ruling 1):
   - **CH2a**, cache and store: the in-memory cache with its layers, the freeze and the prefetch fence, the overlay, the budget, the schema and the engine memory split. CH3a builds on it.
   - **CH2b**, flush, recovery and subscribers: ST3a §5.6's protocol on the real components, with the CRASH acceptance.
   - **CH2c**, performance evidence: PF0 wall-clock workloads, the multi-CF bake-off W-MC, the DEC-43 proxy gate, and L1 against a small O2a follow-up (ruling 7).
2. **The cache is Core's `CCoinsViewCache`, layered.**
   - V (the validation thread) owns an **active** layer. A flush **freezes** it in O(1) into an immutable layer F, which W (the db-writer thread) writes while V keeps connecting.
   - Reads go overlay → active → F → prefetch → database.
   - **F over the database answers exactly as Core's database would right after a flush at F's tip.** The active layer starts empty at the freeze, so every `CCoinsViewCache` rule applies to it unchanged (§3.2).
3. **Values are Core's `Coin` bytes.** CS5a's `coin_bytes` encodes them, and they are kept inline up to 34 bytes. Every P2PKH and P2SH coin is inline at any height and amount. A flush copies the stored bytes with a memcpy, with no re-serialization and no XOR.
4. **Layout is a hypothesis** (ruling 5).
   - The primary layout is an arena of 72-byte slots behind a `hashbrown::HashTable<u32>` index. The estimate is ≈ 80 B per P2PKH coin against Core's measured 115.67 B. That figure is not claimed until PF1 measures it.
   - The fallback (rev2) is 64 hashbrown shards with the same slot, **presized** to a layer's normal size. ≈ 83 B per entry at full load; it holds ≈ 2.64 M coins at the DEC-43 budget against Core's 2.20 M (estimate, §2.3, with rev4's trigger margin). **rev3 (G-R3-M3):** the presize is not a limit; a shard that fills inside an op doubles, so every valid block completes on either layout. **rev4 (G-R4-m3):** the fallback also freezes at a block boundary when a shard nears its growth load. That keeps ordinary blocks from growing shards while no F is in flight; the worst-case growth cost is stated in §2.3.
   - A PF1 gate in CH2a picks one layout on budget bytes per **resident** coin at the DEC-43 budget and over a 4 MiB–1 GiB sweep, with a floor at Core's resident count, and deletes the other.
5. **One coins budget, Core's `nTotalSpace` exactly** (ruling 4): `B(t) = coins share + max(maxmempool − mempool usage, 0)`.
   - **Checked at Core's call sites, on our own counter** (rev3, **DEC-95**; rev4, **DEC-99**). The **native ledger** Σ = active + F + prefetch + overlay + undo copies + R (R the engine's sub-batch reserve, from `EngineTuning`) is compared with B(t) using Core's own `GetCoinsCacheSizeState` arithmetic. The comparison runs only at Core's `FlushStateToDisk` call sites (after each block, after ATMP, and in Core's flush modes), with Core's LARGE headroom. A block may grow the cache past B in between, as it does in Core. rev1–rev2's rule O (`T(op)`, `T_max`, streaming admission, abort-and-retry, the oversized path) is removed.
   - **Native Σ is not Core's `DynamicMemoryUsage`**, so our flush points are not Core's (DEC-99; D1, D11). It is the only counter: rev4's Core-model meter is removed (rev5, **DEC-102**).
   - Native accounting is exact and asserted with a tagging allocator. The **peak** is compared with dashd's as evidence (CH2c, the DEC-43 cell): never higher, and no native block-boundary peak above B on the gated workloads (DEC-99).
   - The engine's memory is separate: **E = Core's block-tree and chainstate DB shares + 64 MiB (Core's fixed EvoDB cache)**. That is one non-strict RocksDB LRU with memtables charged to it (stalling WBM), buffered reads, and memtables flushed after each Bk. E is a configured target with a derived soft ceiling; the binding evidence is DEC-43's `rss_anon` gate. This is an A1 §3.4 erratum (§9 A-2).
   - **Every engine-specific number sits behind one `EngineTuning`** (§2.5.1; DEC-85). DEC-43's RocksDB choice is provisional while SB0 re-checks storage for mobile; the cache, the budget and the flush protocol depend only on the `KvEngine` contract, so SB0's ruling swaps `RocksTuning` without touching them.
6. **The flush is ST3a's order, implemented once.** `FlushMachine<E, F, D>` is a synchronous step machine:
   - prune dry run → space → files barrier → [`sync_wal` if a presumed effect is pending] → [**rewind gate** if a durable locator lies strictly above `new`] → **B1** (Durable) → `ChainStateFlushed` → synced mandatory subscribers commit → prune selection `D ∩ eligible` → coin sub-batches (Buffered) → `sync_wal` → **Bk/Bp** (Durable) → release → unlink.
   - Production steps it on W. The ST3a seam steps it on the test thread.
7. **Recovery is coins-only and blind.**
   - It runs `sync_wal` at open, then checks the marker.
   - It undoes `old → fork` and replays `fork → new` from block and undo files with puts and erases. It reads only the six `meta` decision keys, once, before its first write, and never a key the replay writes. Then it runs `sync_wal` and writes a Durable final batch.
   - **Legacy zero-height undo** can reach rdashd only through imported Core rev files written by Dash ≤ v0.12.2.1. The **importer normalises** those records at import, by the spending block's ancestry, into rdashd-owned rewritten rev files (never the hard-linked source). Recovery treats a zero height as corruption (ruling 2; §3.5.2).
8. **Subscribers:** `SubscriberHub` is the W side. `SubscriberDriver<D: SubscriberData>` is the only place locator rules live:
   - classes as Core's `m_synced`;
   - out-of-band servicing of a `FlushCommit` (case A commits and acks at once; case B drains toward `new`, checkpointing on that leg);
   - submit-before-sync, with the p and s tips;
   - the capped cadence;
   - the Durable rewind commit, with Core's stale-block archive copies;
   - the rewind gate before a backward B1;
   - parking, never blocking a thread;
   - blind recovery.

   A **state contract** (rev2 row classes: height and inexact rows written per block and visible when `best_block()` reports them; keyed rows (IX2) journal-held and read through the driver; state and archive rows; a monotone resource high-water; a byte-bounded journal; a per-subscriber `D_max`, unbounded for txindex) makes the state at any commit target exact. MPIX-11 follows v24: `best_block()` moves to `pprev` at each disconnect, plus a **capped eager commit** at `LCA(pprev, T_ack)` (D13 ruling). Indexes, `llmq/` and wallets implement only their data computation.

   **The event source is an interface** (DEC-81): the driver consumes a `SubscriberFeed` whose guarantees F1–F10 are stated in §2.9.1 (per-subscriber ordering consistent with V's tip changes, `seq` at the tip change, out-of-band control events, wake on enqueue, fairness, the flush barrier, and since rev3 the atomic catch-up-to-live handoff). CH1's bus is one producer; SB0's shared base may be another. **CH2b's implementation is held until SB0 is approved**, and §10.1 lists what SB0 must preserve.

   **rev3 (G-R3-M4):** a Syncing subscriber turns Synced through one **catch-up-to-live handoff** (F10) that is linearized with V's tip changes: it re-checks that its catch-up reached V's live tip, commits, and starts its feed at the next `seq`, so no tip change falls in between. Journals admit entries by bytes, and inexact-only subscribers (txindex) keep none.

   The threaded system is deadlock-free by the argument of §4.4.2, under stated fairness assumptions. The case test is by **sequence number**, a parked subscriber wakes on any enqueue, and every `FlushDue` gets a flush (empty if need be). An explicit-state abstraction checks it over 120 configurations with stepwise emission and, since rev3, the two-step handoff (§4.4.3). Mempool coin access runs as commands on V (ruling 9).
9. **Errors:** one node-wide fail-stop latch spans every engine and the block files (ruling 3).
   - `CoinsView` is **fallible** (`type Error`; CS5a 17014d4b), so a read error can never become a verdict.
   - `have_in_cache` answers from our own in-memory layers, active then F, never the DB, as Core's `HaveCoinInCache` reads only its map (§2.4.5; rev5, **DEC-102**). It is infallible and O(1), and equals Core's answer whenever no flush separates the coin entering the cache from the query. Across differing flush points the reject reason and the orphan-parent request may differ: declared D12, an allowed difference. A stored coin that fails to decode also aborts, where Core reads it as missing (DEC-78, declared D7).
   - Production read errors end the process as Core's `std::abort()` does. Write errors abort with Core's user-visible strings and v24's exit status 1.
10. **Gates** (ruling 6):
    - CH2a and CH2b merge on PF0 counters against the previous head.
    - CH2c merges on the **DEC-43 proxy**: coins replay against ST1's LevelDB Core-model arm, paired, under a common cgroup cap (anon + page cache), gating time and `rss_anon_peak`, plus counters.
    - The real DEC-43 cell (rdashd vs dashd) runs at the **M3 exit**.

---

## 1. Scope and non-goals

### 1.1 The PLAN row, item by item, and where each lands after the split

| PLAN CH2 text | Sub-task | Section |
|---|---|---|
| Coins cache, `PackedCoin` | CH2a | §2.2–§2.4, §3.2 |
| freeze-and-flush under one memory budget | CH2a (freeze, budget), CH2b (the flush) | §2.5, §4.2, §5.2 |
| coins table | CH2a | §2.6 |
| A1 §3.5 protocol: file → B1 → coin sub-batches → Bk | CH2b | §2.7, §4.1–§4.2 |
| coins-only `ReplayBlocks` | CH2b | §2.8, §3.5, §4.3 |
| errors and disk full | CH2a (latch), CH2b (flush paths) | §2.10, §4.6 |
| subscribers' locators | CH2b | §2.9, §4.4 |
| the multi-CF bake-off workload | CH2c | §5.8 |
| CRASH at model level: every sub-batch boundary, the first flush, a reorg in the window, interrupted recovery | CH2b | §6.4 |
| `coins_cache_model` | CH2a (no crash), CH2b (crash kinds) | §6.5 |
| budget invariant asserted with a counting allocator | CH2a | §6.7 |
| BG: `reindex-chainstate` counters; `reindex_time_s`, `peak_rss_mib` not worse than the previous head | CH2a/b proxies; CH3a once it can run | §5.7 (ruling 6) |
| BG: DEC-43 low-memory cell not slower than dashd | CH2c proxy; M3 exit for the real cell | §5.5 (ruling 6) |

### 1.2 PRD requirements

- **FR-CHS-03.** A write-back cache sized by `-dbcache` with batched atomic flushes is CH2a and CH2b. Per-block undo is CH3a's, written through ST2.
- **FR-CHS-04.** Atomic consistency across UTXO, MN list, LLMQ indexes and index markers. B1 commits evo at `new` with the marker, and locators follow §4.4.
- **FR-STO-02 / FR-STO-05.** A versioned chainstate schema and explicit codecs (§2.6).
- **FR-STO-03.** Atomic commits, the fsync policy of §4.1, and ST3a's harness over the real code (§6.4).
- **FR-STO-04.** The prune mechanism (§4.5); CH8 owns the policy.
- **US-NODE-04 / NFR-OPS-01.** No reindex after a crash or power loss. `feature_dbcrash` passes (CH5 runs it). A clean `stop` is no slower than dashd's.
- **NFR-PERF-01/03, NFR-RES-01/02:** §5.

### 1.3 Non-goals (owned elsewhere)

- **CH3a:** `connect_block` and `disconnect_block`, undo construction, BIP30/BIP34, assumevalid. CH3a calls CH2a's overlay in Core's order, and ports `ApplyTxInUndo`'s live path, including `AccessByTxid`.
- **CH3b:** `activate_best_chain` and the `FlushStateToDisk` call sites. CH2b provides `FlushPolicy::decide` and `ChainCommand::Flush`.
- **CH4:** the S2 prefetch producer. CH2a provides `PrefetchMap`, `admit_prefetch` and the fence.
- **CH7a:** the evo deltas and the presumed/independent classification of effects. CH2b defines the `StateDelta` slot and the `EffectLedger`.
- **CH8:** `-prune` configuration, `pruneblockchain`, the incompatibility errors and the FTs. CH2b implements the protocol's prune slots (§4.5), because the CRASH model needs them inside the driver.
- **IX1/IX2, LK1b, WL:** subscribers' data writers implement `SubscriberData`.
- **CH5:** `-reindex(-chainstate)`, `-loadblock`, `verifychain`, and running `feature_dbcrash`. CH2 provides `wipe_chainstate`, `-dbcrashratio` and `open_chainstate`.
- **CH5 / CH6a import paths:** normalising legacy undo (§3.5.2; PLAN text in §7.4).
- **CS5a/CS5b:** codecs, `CoinsView` (amended here, §2.4.4), MuHash and `hash_serialized_2`.
- **ST3b:** the device and VM harnesses. CH2b provides its hooks (§6.4).
- **NG-6:** assumeutxo. Core's `Sync()` and v24's `EvoDbIdentity` plumbing are not ported.
- **Windows.** macOS durability rests on BL-32.

---

## 2. Data model and public API

### 2.1 Placement

```text
dashd-chainstate/src/
  coins/key.rs        OutPointKey, db-key codec (txid ‖ VARINT(vout))                          CH2a
  coins/packed.rs     PackedCoin (inline ≤ 34 B, else long)                                    CH2a
  coins/layer.rs      CoinLayer: ArenaLayer (primary) | ShardedLayer (fallback), one API        CH2a
  coins/cache.rs      CoinsCache<E>, FrozenLayer, fetch/add/spend/uncache, freeze/release       CH2a
  coins/overlay.rs    BlockOverlay<'c, E> (Core's child view), StableBytes                      CH2a
  coins/prefetch.rs   PrefetchMap, PrefetchBatch, the generation fence                          CH2a
  coins/budget.rs     CoinsBudget, Usage, CoinsCacheSizeState                                    CH2a
  coins/table.rs      CoinsTable (CF `coins`), cursor, multi_get, estimate_size                  CH2a
  coins/reference.rs  RefCoinsCache: line-by-line CCoinsViewCache over MemoryCoins (test twin)   CH2a
  caches.rs           CacheSizes: CalculateCacheSizes and the Cache configuration lines          CH2a
  tuning.rs           EngineTuning, MemoryTuning, RocksTuning, EngineMemory                      CH2a
  store/schema.rs     CHAINSTATE_SCHEMA v1                                                       CH2a
  store/meta.rs       Meta codecs, wipe_chainstate                                               CH2a
  store/guarded.rs    GlobalFailStop, GuardedEngine<E>, GuardedSnapshot, GuardedRead,
                      GuardedFiles<F>, AbortSink, RecordingSink                                  CH2a
  store/abort.rs      CoreAbortSink (the production sink)                                        CH2a
  flush/{packet, policy, machine, driver, observer, prune, effects}.rs                           CH2b
  recovery/{mod, replay}.rs                                                                      CH2b
  subscriber/{hub, driver, keeper}.rs                                                            CH2b
  seam.rs             Salt, Clock, the ST3a Sut adapters (feature `seam`, test builds only)      CH2b
dashd-store/src/
  engine.rs           + KvBatch::delete_range (BL-38)                                            CH2a
  rocks.rs            + WriteBufferManager charged to the block cache; manual_wal_flush asserted  CH2a
  flatfile/blocks.rs  + side-effect-free prune planning (plan_files_to_prune[_manual], eligible)  CH2b
dashd-store-bakeoff/  W-MC (multi-CF workload, §5.8)                                             CH2c
node-fuzz/fuzz_targets/{coins_cache_model, coins_overlay_diff, packed_coin_roundtrip,
                        chainstate_crash, coins_replay_blind}.rs                                 CH2a/b
node-l1/              diff_coins_ops (against O2a-CO's oracle_coins_ops)                         CH2c
```

These rules carry over from A1 Appendix E and ST3a §5.6:
- no `async`;
- no `HashMap` with `RandomState` in determinism-linted paths;
- `CoreBytes`/`CoreUndoBytes` for stored blocks and undo;
- everything generic over `E: KvEngine` and `F: FlatFs`, monomorphised.

`dashd-chainstate` is CH1's crate (M2). CH1 has not started, so CH2 codes against the narrow `IndexAccess` trait (§2.11). CH1 implements it, and a test arena stands in until then.

### 2.2 Keys, `PackedCoin`, slots

```rust
/// txid ‖ vout (LE): Core's COutPoint in memory.
#[derive(Clone, Copy, PartialEq, Eq, Hash)] #[repr(transparent)]
pub struct OutPointKey(pub [u8; 36]);
impl OutPointKey {
    pub fn new(op: &OutPoint) -> Self;
    pub fn outpoint(&self) -> OutPoint;
    /// The `coins` CF key: txid ‖ VARINT(vout). This is Core's `CoinEntry` without its 'C' prefix
    /// (txdb.cpp:62-68; A1 §2.9). CF order is Core's serialized-key order (raw bytes of
    /// txid ‖ VARINT(vout)), not numeric vout order: VARINT(16383) = fe7f sorts after VARINT(16512) = 808000.
    pub fn db_key(&self, buf: &mut [u8; 41]) -> usize;
}

/// Core's `Coin` serialization (coins.h:60-66):
/// VARINT(h·2+cb) ‖ VARINT(CompressAmount(v)) ‖ ScriptCompression(spk).
/// The cache value and the database value are the same bytes.
pub struct PackedCoin(Repr);
enum Repr { Inline { len: u8, bytes: [u8; 34] }, Long(LongRef) }   // LongRef: Box<[u8]> (rev5: a1 kept the fallback, so the arena's
                                                                   // offset+len variant is gone, O-R6-n2);
                                                                   // it holds the WHOLE serialized coin (rev3, G-R3-M1 item 1).
                                                                   // A long PackedCoin handed OUT of the cache (spend's undo coin)
                                                                   // owns a LedgerBox, counted in Usage::held_out until dropped (rev4, G-R4-m2);
                                                                   // held_out is a shared AtomicU64: a LedgerBox drops on any thread (O-R6-n2)
impl PackedCoin {
    pub fn from_output(out: &TxOut, height: u32, coinbase: bool) -> Result<PackedCoin, CoinsError>; // CS5a coin_bytes; Err only on value −1
    pub fn from_db(bytes: &[u8]) -> Result<PackedCoin, DecodeFailure>;  // validated by CoinRef::decode; CoinsTable turns a failure into
                                                                      // GuardedEngine::corrupt(..) -> CoinsReadError (latch, abort; DEC-78, D7)
    pub fn as_bytes(&self) -> &[u8];
    pub fn as_ref(&self) -> CoinRef<'_>;   // CS5a borrowed decode; infallible for bytes we produced or validated
}
```

**Inline bound: 34 B, from Opus.** Fable's 30 B is too small; see §8 row 3.
- A P2PKH or P2SH coin is the height-code VARINT, then the amount VARINT, then a 21-byte compressed script.
- The height code is ≤ 4 B below height 2^27.
- `CompressAmount` of any value ≤ `MAX_MONEY` is below 2^55, so its VARINT is ≤ 8 B.
- That is at most 33 B, so **every** P2PKH/P2SH coin is inline.
- Compressed P2PK is 1 + 32 B of script (38–46 B in all), so it is long, as are bare multisig and non-standard scripts.

**The slot** (72 B, `#[repr(C, align(8))]`):

`key: OutPointKey (36) ‖ flags: u8 ‖ len: u8 (0xFF = long) ‖ coin: [u8; 34]`

The flags are `DIRTY = 1`, `FRESH = 2` and `SPENT = 4`. SPENT marks a tombstone that carries no coin bytes.

**Round trip.** `CoinRef::decode(encode(c)) == c` for every coin `add` can store:
- `IsUnspendable` outputs never enter (quirk 33; `coins.cpp:72`);
- uncompressed P2PK is compressed only when `IsFullyValid` (`compressor.cpp:19-84`);
- amounts are in range, because `CheckTransaction` runs first.

The `packed_coin_roundtrip` fuzz target pins this.

**Encode cost.** We encode on `add`; Core serializes at flush time, and only the coins that survive. One encode is about 50 ns; `coins_micro` measures it.

### 2.3 The layer: primary and fallback (ruling 5)

```rust
/// One API, two layouts. CH2a's layout gate (§7.1) keeps one and deletes the other; there is no runtime switch.
pub(crate) trait CoinLayerOps {
    fn get(&self, k: &OutPointKey) -> Option<SlotRef<'_>>;          // flags + coin bytes, borrowed
    fn upsert(&mut self, k: &OutPointKey, flags: Flags, coin: CoinSrc<'_>);   // rev3: infallible, grows as needed (DEC-95)
    fn remove(&mut self, k: &OutPointKey);
    fn iter_dirty(&self) -> impl Iterator<Item = (OutPointKey, Flags, &[u8])>;   // the flush order
    fn allocated_bytes(&self) -> u64;                                         // exact capacity (not occupancy), from allocation sizes
    fn len(&self) -> usize; fn dirty_bytes(&self) -> u64;
}

/// Primary (Opus): append-only arena of 72-byte slots plus a slot-id index.
pub(crate) struct ArenaLayer {
    chunks: Vec<Box<[Slot; 3_640]>>,       // 262,080 B per chunk, never moved (Core's PoolResource chunk: pool.h:187)
    free: Vec<u32>,                         // slots freed by a FRESH spend or an uncache, reused first
    index: hashbrown::HashTable<u32>,       // hashes through the arena (A1 §2.10's block-index idiom)
    long: LongCoins,                        // chunked byte arena for coins > 34 B
    hasher: SaltedFx,                       // random salt; FIXED under the seam (ST3a §5.6 item 3)
    live: u32, dirty: u32, dirty_bytes: u64,
}

/// Fallback (rev2, O-R2-M5): 64 hashbrown shards with the same 72-byte slot (the key plus a 36-byte value),
/// **presized** when the layer is created; a shard that fills inside an op doubles (rev3, G-R3-M3). Shard s gets a power-of-two bucket count;
/// the counts come from a binary decomposition of the layer's capacity, and a key is routed to a shard in
/// proportion to bucket counts. Long coins are boxed.
pub(crate) struct ShardedLayer {
    shards: [hashbrown::HashMap<OutPointKey, SlotValue, SaltedFx>; 64],   // with_capacity_and_hasher at creation; grows past it
    route: [u64; 64],                                                    // cumulative bucket counts; hash → shard
    cap_bytes: u64,                                                       // the presize; not a limit (rev3)
}   // iter_dirty: shard 0..63, table order
```

**Primary (hypothesis, PF1 confirms).**
- Each entry costs 72 B in the arena plus 5 B per index bucket at a load factor of 7/16–7/8. That is 77.7–83.4 B, **≈ 80 B on average**.
- Core counts **115.67 B** per P2PKH coin: a 104.05 B pool node plus 8–16 B of buckets (`memusage.h:187-203`, `PoolAllocator`; Opus's probe, memory note `dashd-coins-cache-memory-facts`). Compressed P2PK is ≈ 128 B here against ≈ 180 B in Core.
- The arena grows one chunk at a time, as Core's pool does, and only the 5-byte index rehashes.
- **Iteration is arena order** (insertion order plus deterministic free-slot reuse). So no store call's order or sub-batch composition depends on the salt.
- Slots never move while the layer lives, which makes borrowed reads trivially sound.

**Fallback: presized, growing only past the presize (rev2, O-R2-M5; rev3, G-R3-M3).** rev1's fallback was 64 *equally* loaded shards that double on demand. Uniform hashing fills all 64 within about 1 % of each other, so they reach 7/8 load together. The layer then stops where the unsharded table stops: ≈ 922k coins per layer, ≈ 1.84 M in active + F, against Core's 2.20 M (Opus `shard_cliff.py`). rev2 removes growth from the fallback instead:
- **Capacity.** At creation (the start, or a freeze) the layer gets `cap = f · (B(t) − R − B(t)/16)`: half of the budget left after the sub-batch reserve and the prefetch cap, with f = 1/2. Two layers at their presize therefore fit B(t). The presize is what a layer normally reaches before the THRESHOLD freeze at a block boundary (§2.5, with the shard trigger below); it is a sizing choice, not a capacity.
- **The presize is a ceiling (CH2c D29, DEC-388).** A layer no longer allocates its presize at creation. The presize fixes the decomposition and the routing (`ShardSizes::for_bytes`, unchanged for the layer's life) and each shard's *ceiling*. Each shard starts at its ceiling's `2^-d`, with `d = min(k − 8, 3)`: the smallest shard at 256 buckets, but never below 1/8 of its ceiling, so at most three doublings reach it (CH2c D30, DEC-388's CPU budget, accepted by DEC-397(a); ≈ 14.6 MB of tables at DEC-43 against a 115 MB ceiling). Below its ceiling it doubles as any hash table does. At its ceiling every rule below applies unchanged: the shard trigger, the rebuild at size below the trigger, DEC-108's boundary rebuild, and growth past the ceiling. The budget charges each shard at least its ceiling (`Usage::unbacked`), so THRESHOLD, LARGE, CRITICAL and the next presize read exactly what they read for a presized layer, and the freezes and flushes come at the same blocks. Only RssAnon changes. Tables of 64 KiB or more come from a page allocator that maps them and unmaps them on free (`layer/pages.rs`), counted in whole pages; the rounding, under one page a shard, is part of the table overhead.
- **Sizing.** `cap / 73 B` buckets are split into 64 power-of-two shard sizes 2^k and 2^(k+1): s shards of 2^(k+1) and 64 − s of 2^k, chosen so their sum is within 2^k of the target, a rounding loss under 1/64. A key's shard is the bucket-weighted slot of its salted hash, so expected loads are equal fractions of each shard's capacity.
- **Routing bits (a requirement; rev5, O-R6-m8).** Routing must use hash bits that hashbrown does not: not the low bits it takes the bucket index from, and not the top 7 bits it takes the h2 tag from. If routing used the top bits (the natural `(h as u128 * total) >> 64`), every key in a shard would share nearly the same h2, the SIMD tag filter would stop filtering, and each probe would compare several 36-byte keys. The branch routes on bits 25..56 (`sharded.rs:94` on `node/ch2a-coins`). a1's PMU line (instructions and LLC misses per hit) covers it, and a unit test asserts that the h2 tags within one shard are spread over all 128 values.
- **Growth past the presize** (rev3, G-R3-M3; rev2's "no growth" rule is withdrawn). Nothing inside an op checks the presize. A shard that reaches 7/8 load doubles, as hashbrown does, and long coins are boxed as they come with no byte cap. So **any valid block completes** on a presized layer, at any B: the `-dbcache=4` witness (B = 1.75 MiB, presize 712,704 B ≈ 8,500 entries) takes a 50,000-output block by growing every shard about three times. The THRESHOLD freeze and the native CRITICAL check then run at the block boundary (§2.5).
- **The shard trigger** (rev4, G-R4-m3). rev3 said normal blocks never grow a shard because the THRESHOLD freeze comes first. Under rev3's policy that was false. The presize is allocated at creation (≈ 110.4 MiB at DEC-43), which is below f·B (126.375 MiB), and filling it adds nothing to native Σ. So `active ≥ f·B` could be reached only by growing shards. rev4 adds a fallback-only THRESHOLD condition, decided at the same block boundaries: **freeze when any shard of active holds at least its growth load minus 1/64 of its buckets**, that is 55/64 of its buckets against hashbrown's 7/8 = 56/64.
  - Keys reach a shard in proportion to its buckets. At DEC-43 (below), a block that adds n entries to active (outputs plus DB-fetched inputs) puts n/96 into each 16,384-bucket shard and n/48 into each 32,768-bucket one in expectation, against margins of 256 and 512 entries.
  - **No F in flight:** at DEC-43, a block adding up to ≈ 20,000 entries **ordinarily** grows no shard: under uniform routing the expected share plus 3σ stays within the margin (≈ 251.4 < 256 and ≈ 477.3 < 512). It is not a guarantee. A small shard exceeds its margin with probability ≈ 0.00058, ≈ 1.9 % for any of the 32 (union bound, Sol G-R5-n1), and the table then grows, which is permitted. The safe block size scales with B: ≈ 20,000 × B / 252.75 MiB, so ≈ 5,000 entries at 64 MiB and ≈ 320 at 4 MiB, where the margin is 4 or 8 entries. **So the trigger prevents ordinary growth only near DEC-43; at small B it is pure density cost** (rev5, O-R6-m4). a1 counts growth events per budget and gates zero only at B_DEC43 on the mainnet trace.
  - **F in flight, or a larger block:** shards grow, at the cost below.
  - The trigger costs density: the fullest shard freezes the layer up to 1/56 (≈ 1.8 %) earlier. It is not a limit; within a block the layer keeps taking entries and grows if it must.
  - **Tombstone churn.** On remove, hashbrown leaves a DELETED control byte that still uses up insert room. When the room runs out, hashbrown rehashes in place only at a live load of 7/16 or less, and above that it doubles. Active removes entries on every FRESH spend and every uncache, so mainnet churn alone would double shards at low occupancy and defeat the trigger. The fallback therefore checks before an insert: if a shard's room is gone and its **live** load is below its trigger, it rebuilds the shard at the same bucket count with the tombstones dropped. That is a new table of the same size, filled and then swapped, so the transient is one shard's table (≤ 2.28125 MiB at DEC-43), counted in native Σ. The rebuild restores at least 1/64 of the shard's buckets of room. A shard doubles only at or above its trigger. A rebuild is O(shard) on V inside a block; near the trigger that is at most about one rebuild per shard per block (≤ 64 × ≈ 1 ms at DEC-43 sizes). a1 reports **per-block rebuild time at p99**, against connect-latency metric #5, as well as frequency (O-R6-n6).
  - **The boundary rebuild** (DEC-108, from a1's measurement). The room a rebuild leaves does not last: removes leave DELETED buckets that use it up again while the shard stays below its trigger. So when a block takes a shard to its trigger, the shard can have room for as little as one insert, and it doubles inside that block with no F in flight (a1 saw it at 76.99 MiB and below, over 10 passes of freezes on the mainnet trace; none at B_DEC43). **At every block boundary with no F in flight**, the cache therefore also rebuilds, at its size, each shard of active whose live entries plus DELETED buckets have reached its trigger while its live entries have not. After that every shard below its trigger has room for more than 1/64 of its buckets, so a block must put more than that many inserts into fresh buckets of one shard to double it, as the estimates above assume. It is the same rebuild as the in-op one, run at a boundary: one shard's table is the transient, counted in native Σ, and its time is reported with the in-op rebuilds. It changes no entry and no THRESHOLD decision (it does not change a shard's live count). CH2a implements it in a2 (`CoinsCache::boundary_rebuild`), with a churn test at a low budget (76.99 MiB) showing that no shard grows with no F in flight.
- **Growth cost, worst case** (rev4, G-R4-m3). rev3's "≈ 3.45 MiB per shard" was a mean over 64 equal shards, which the binary decomposition does not produce. At DEC-43, `cap/73` = 1,586,442 buckets rounds down to 32 shards of 16,384 buckets and 32 of 32,768, at 73 B per bucket (before small alignment and control-tail bytes):

  | Doubling | Old table | Allocated while the old one lives | Kept after |
  |---|---:|---:|---:|
  | a 16,384-bucket shard | 1.140625 MiB | 2.28125 MiB | +1.140625 MiB |
  | a 32,768-bucket shard | 2.28125 MiB | **4.5625 MiB** | +2.28125 MiB |
  | the j-th doubling of the same shard | × 2^(j−1) | × 2^(j−1) | × 2^(j−1) |

  - **One op:** shards double one at a time, so the transient above the kept growth is one shard's new table: at most 4.5625 MiB at DEC-43 until some shard has already doubled. At small B one block can double a shard several times; the `-dbcache=4` witness doubles each shard about three times.
  - **With F in flight, the shards cross together.** Uniform hashing fills them within about 1 % of each other. Once active passes its trigger with F in flight, every shard doubles within a few blocks, and kept native Σ rises by up to the layer's whole table, 109.5 MiB at DEC-43 (the decomposition's actual table, below the 110.4453125 MiB cap). Two presized layers plus R and the prefetch cap approximately fill B by construction, so native CRITICAL fires after roughly 17.7 MiB − prefetch of growth: B/16 plus each table's 0.9453125 MiB of rounding slack (Sol G-R5-n2). V then waits for release. Under a slow W the fallback therefore stalls about where rev2's estimate put it, with both layers at their presize.
  - **After a grown layer freezes, the new active is presized to what is left** (rev5, O-R6-m6). A fixed presize on top of a frozen layer that has doubled would push native Σ past B at once, so V would wait for that whole F straight after waiting for the previous one. The new active therefore gets `cap = min(f·(B − R − B/16), B − R − B/16 − F.allocated_bytes())` (never below one 256-bucket-per-shard floor). It can still grow (DEC-95). a1 reports the stall count with and without this rule.
  - **The worst single block:** a 2 MB block of ≈ 222,000 minimum-size (9-byte) outputs, arriving when active is just below its trigger. It doubles every shard once (≤ 4,630 entries per shard fit after one doubling), so ≈ 109.5 MiB is kept and at most 4.5625 MiB of transient sits on top. If no F was in flight, that doubled layer freezes at the boundary. The new active is presized to what is left (above), so V does not stall on the presize. It stalls only if the next blocks grow active past B before W has written F. The arena takes the same block as ≈ 15.3 MiB of chunks plus at most one index doubling. Core takes it as ≈ 22 MiB of pool nodes plus at most one bucket rehash. This is the fallback's real worst case against the arena. a1 runs it on both layouts and reports it beside density. It is not gated: DEC-95 makes the peak evidence, and the DEC-43 gate measures the mainnet workload (§5.5).
  - **For comparison:**
    - The arena's index doubling at DEC-43 sizes (2^21 → 2^22 buckets of 5 B) allocates 20 MiB while the old 10 MiB lives, and 640 MiB over 320 MiB at `-dbcache=16384` (2^26 → 2^27).
    - Core's bucket array (8 B per bucket) starts from Core's unreserved empty map and follows Linux libstdc++'s prime policy (Sol's r4 probe). The rehash in the DEC-43 range goes 1,447,153 → 2,938,679 buckets, allocating 22.42 MiB while 11.04 MiB lives. The one near 16 GiB goes 101,473,717 → 206,062,531 buckets, allocating 1,572.13 MiB over 774.18 MiB.
    - These Core figures are Sol's r4 probe values, reported for comparison; no CH2 test reproduces Core's number (rev5, DEC-102).
  - Every transient of ours is native Σ, counted to the byte (§6.7), and appears in the peak evidence (§2.5).
- **Cost.** At full load, 73 B / (7/8) = 83.4 B per inline entry. Add the rounding loss, the trigger margin (≈ 1.8 %), and the fill noise of the fullest shard, which reaches its trigger first (1–2 % at DEC-43 sizes, simulated).
- **RSS.** The control bytes are written at creation; slot pages are touched only as entries arrive. RSS therefore follows occupancy, while the budget accounting is conservative and counts the full allocation from creation.
- **A known cost.** If B(t) grows mid-layer (the mempool drains), the layer reaches its presize earlier than it needs to and grows a shard or two; the next freeze sizes a new layer. If B(t) shrinks, Core's check at the next block boundary decides (§2.5).
- `iter_dirty` walks shard 0 to 63 in each shard's table order, with no side list (Opus F13). That order depends on the salt: random in production, fixed under the seam and in bench builds, so PF0 counters stay reproducible.
- It needs no `unsafe`.

**Resident capacity** (`/work/scratch/ch2-rev2/layout_capacity.py` and `.txt`; inline coins; rev2's estimate under rule O's accounting, including the arena's index-doubling overlap, kept for scale; a1 replaces it with measurements under DEC-95's policy) is coins in active + F at the moment V must wait:

| B (MiB) | Core: `max(0.9B, B − 10 MiB)` / 115.67 B | arena | presized fallback | rev1's equal shards |
|---|---|---|---|---|
| 4.00 | 32,635 | 43,008 | 33,920 | — |
| 25.40 | 207,218 | 344,064 | 244,634 | — |
| 111.43 | 919,491 | 1,376,256 | 1,097,240 | — |
| **252.75 (DEC-43)** | **2,200,586** | **3,211,264** | **2,692,224** | ≈ 1,843,916 |
| 488.88 | 4,341,151 | 6,422,528 | 5,461,380 | — |
| 1,024.00 | 9,192,151 | 12,845,056 | 11,736,096 | — |

Both layouts clear Core at every one of the 17 sweep points; the full table is in the `.txt`. Budget bytes per resident coin, as a geometric mean over the sweep: arena 89.4, presized 105.3, a ratio of 0.850. At DEC-43 the ratio is 0.838. These are estimates, not the gate's numbers. **rev4's shard trigger** lowers the presized column by about 1/56 (DEC-43: ≈ 2,644,000), which moves the DEC-43 ratio to ≈ 0.823 and the geometric mean to ≈ 0.835. Both layouts still clear Core.

**a1's measurements replace these estimates** (rev5, O-R6-m4; DEC-108; PF1 on `node/ch2a-coins` at `57785c8b`, where the fallback won the gate). With rev4's trigger and churn rebuild and m6's presize in place, the fallback holds **2,676,754 coins at DEC-43, 1.216× Core's 2,200,586**, and 1.174× as the sweep's geometric mean. Its floor margin is narrowest at 4 MiB, **1.043**. **The fallback alone does not reach the 2× target.** Density gives ≈ 1.22× resident coins, not 2× throughput, and the rest of the 2× must come from the other levers (§5.6).

The unsharded `HashMap` of A1 §3.4 and rev1's equal-shard map are **not** acceptable fallbacks. Both stop at ≈ 917–922k coins per layer at the DEC-43 B, ≈ 1.84 M in total, against Core's 2.20 M.

**Layout gate** (CH2a phase a1, §7.1; rev2 metric, O-R2-M5). rev1 sampled bytes per coin at two fixed counts, 1 M and 6.4 M. Those are snapshots at arbitrary points in each layout's growth cycle: the arena was 46.1 % below the equal-shard fallback at 1 M and 17.9 % below at 6.4 M. The gate now measures at the point that matters, **when the budget is full**:
- **Harness** (rev3: DEC-95's policy, the same for both layouts). `coins_layer_density` drives the real `CoinLayerOps` implementations **block by block**: per-block create and spend counts and coin lengths come from the **mainnet** trace (ST1's `mainnet-453371`; the W-MF trace once it exists). At each block boundary it applies §2.5's policy exactly: the THRESHOLD freeze with no F in flight (including the fallback's shard trigger, rev4), and the CRITICAL check on **native** Σ = allocated capacity + the reserve, checked by the tagging allocator. The measurement point is the first block boundary at which V would have to wait (CRITICAL with F in flight).
- **Fairness** (G-R3-M3). Neither layout excludes its own growth: the arena's index doublings and the fallback's shard doublings are in native Σ whenever they happen, and the fallback is charged its full presize from creation. Both are also run through the two completion witnesses of §7.1 a2. The gate reports each layout's peak native Σ over the sweep beside its density, so a layout cannot win density by moving memory into transients. It also reports each layout's worst single block (§2.3, rev4), run on both layouts.
- **The budgets swept.** **B_DEC43 = 252.75 MiB**, plus 16 log-uniform values from 4 MiB to 1 GiB. 1 GiB holds more than the 6.4 M-coin mainnet set, so the sweep covers mainnet coin counts.
- **The metric.** `r(layout, B) = B / N_res(layout, B)` budget bytes per resident coin, where `N_res` is the coins resident in active + F at that moment.
- **Density: the arena is kept iff** both:
  - `r(arena, B_DEC43) ≤ 0.8 · r(fallback, B_DEC43)`;
  - the geometric mean over the sweep of `r(arena, B) / r(fallback, B)` is ≤ 0.8.
  This is DEC-77 Q5's 20 % threshold, unchanged; only the measurement point changes, as DEC-80 Q2 did for latency.
- **Latency.** Hit latency ≤ 1.1× the fallback's at B_DEC43, as PMU `instructions:u` and `LLC-load-misses` per `get` (each ≤ 1.1×; memory note `agentbox-perf-counters`), confirmed by a paired wall-clock sign test over 5 rotations in a reserved window (DEC-80 Q2).
- Otherwise the fallback ships.
- **Floor for the winner.** Whichever layout wins must satisfy `N_res(winner, B) ≥ N_core(B) = max(0.9·B, B − 10 MiB) / 115.67 B` at **every** sweep point, Core's resident coins at its flush threshold at equal B. If it does not, a1 fails and goes back to the manager; it does not ship.
- **Reporting.** `iter_dirty` throughput and the flush memcpy rate are recorded, not gated. The PF1 note records every number before any density claim appears in a report (A1 §3.4).
- **Prediction, not decision.** From the estimate above (with rev4's trigger, DEC-43 ratio ≈ 0.823; geometric mean ≈ 0.835), the presized fallback is likely to ship, and both layouts clear the floor. Whichever ships, a2's completion witnesses run on it.

### 2.4 `CoinsCache`, `BlockOverlay`, and the fallible `CoinsView`

#### 2.4.1 The cache

```rust
pub struct CoinsCache<E: KvEngine> {
    active: CoinLayer,
    frozen: Option<Arc<FrozenLayer>>,   // at most one (A1 §3.4); V and W hold it
    prefetch: PrefetchMap,               // positive DB results with fetch_gen; CH4 fills it
    released_gen: u64,
    best: Option<BlockRef>,              // Core's hashBlock for (active ∪ F ∪ DB)
    table: CoinsTable, db: Arc<GuardedEngine<E>>,
    budget: CoinsBudget, latch: GlobalFailStop,
}
pub struct FrozenLayer { coins: CoinLayer, gen: u64, tip: BlockRef }       // immutable after freeze

impl<E: KvEngine> CoinsCache<E> {
    // ---- Core's CCoinsViewCache over base := (F over DB) ----
    pub fn fetch(&mut self, op: &OutPointKey) -> Result<Option<CoinRef<'_>>, CoinsReadError>; // FetchCoin+GetCoin
    pub fn have_in_cache(&self, op: &OutPointKey) -> bool;      // HaveCoinInCache: active then F, unspent; never the DB (§2.4.5, DEC-102)
    pub fn uncache(&mut self, op: &OutPointKey);                // Uncache: an active entry with flags == ∅ only (F is immutable)
    pub fn best_block(&self) -> Option<BlockRef>;
    // ---- budget, freeze, release ----
    pub fn usage(&self) -> Usage;                                 // the native ledger, the only counter (DEC-99, DEC-102)
    pub fn size_state(&self, mempool_usage: u64) -> CoinsCacheSizeState;   // core_size_arith on native Σ
    /// rev5 (O-R6-m5): the coin-side half of the flush decision, in CH2a so a2 can test it without CH2b's FlushPolicy.
    /// At a block boundary: THRESHOLD (active ≥ f·B, or a fallback shard at its trigger) and native CRITICAL/LARGE.
    pub fn boundary_state(&self, mempool_usage: u64, flush_in_flight: bool) -> CoinsBoundary;
    pub fn freeze(&mut self, mempool_usage: u64) -> Arc<FrozenLayer>;  // O(1): mem::take(active); panics if F is Some; F's tip is the best block (DEC-142, O6)
    pub fn release(&mut self, gen: u64);                          // drop F; released_gen += 1; prefetch.clear()
    pub fn admit_prefetch(&mut self, batch: PrefetchBatch) -> usize;  // fence + headroom (§2.5)
    pub fn resize(&mut self, coins_share: u64, db_resized: bool) -> Resized; // ResizeCoinsCaches: grow → IF_NEEDED, shrink or DB-only → ALWAYS
}
```

**Read path** (from Opus, with the equivalence argument of §3.2):
- overlay child → active → F → prefetch → DB.
- A **tombstone** (`SPENT`) in active or F is authoritative: the coin does not exist.
- A DB or prefetch hit is copied into active as clean (flags ∅), as Core's `FetchCoin` emplaces it (`coins.cpp:44-59`).
- An **F hit is not copied**, because F lives until release.
- `spend` of a coin that lives only in F writes a DIRTY, non-FRESH tombstone into active.

**Cache membership** (rev5, **DEC-102**; §2.4.5). `have_in_cache` resolves op first-hit over active then F and answers "present and unspent". It never reads prefetch or the DB. rev2–rev4's Core-membership shadow is removed, and D12 is back as a declared allowed difference. `uncache` removes only a clean active entry (F is immutable).

**`-consensusxcheck`** (A1 §3.4 item 5): every `fetch` is also resolved without prefetch (active → F → a direct DB read), and any difference aborts. CH4 extends this to its producer.

#### 2.4.2 The block overlay (Core's child view)

Core's child view is `CCoinsViewCache view(&CoinsTip())`: connect at `validation.cpp:3430`, disconnect at `:3279`. Our mutations take `&mut self` and our reads take `&self`, alternating as Core's connect loop does, with no `RefCell`:

```rust
pub struct BlockOverlay<'c, E: KvEngine> {
    tip: &'c mut CoinsCache<E>,
    child: CoinLayer,          // small; same slot; insertion-ordered for the merge
    scratch: StableBytes,      // append-only Box<[u8]> store for DB hits read through &self; addresses never move
}
pub enum CoinSrc<'a> { Output { out: &'a TxOut, height: u32, coinbase: bool }, Bytes(CoinRef<'a>) }   // connect / undo restore
impl<'c, E: KvEngine> BlockOverlay<'c, E> {
    pub fn new(tip: &'c mut CoinsCache<E>) -> Self;
    /// Core's HaveInputs (coins.cpp:303-313): fetches every input, caching it in the TIP (active) as clean, as Core's
    /// parent FetchCoin does. The child keeps NO copy (rev5, O-R6-m3; §2.5 "Ownership"): reads resolve child → active
    /// → F → prefetch → DB. CH3a calls it first, as CheckTxInputs does, so the &self checks then hit memory.
    pub fn have_inputs(&mut self, tx: &Tx) -> Result<bool, CoinsReadError>;
    pub fn add(&mut self, op: &OutPointKey, coin: CoinSrc<'_>, possible_overwrite: bool) -> Result<(), CoinsError>;  // AddCoin
    /// SpendCoin with moveout (undo). Moves only child-owned bytes; a coin the child does not own is COPIED, and the
    /// parent is untouched until commit (rev4, G-R4-m2; §2.5 "Ownership"). A long result owns a LedgerBox.
    pub fn spend(&mut self, op: &OutPointKey) -> Result<Option<PackedCoin>, CoinsError>;
    /// SetBestBlock(best), then BatchWrite(child → tip, erase = true), coins.cpp:178-252. The block's best block is an
    /// argument, as every Core flush of a block's view sets it first (validation.cpp:2365, :2538, :2912), so a commit
    /// cannot leave the tip's unset (DEC-142, O4).
    pub fn commit(self, best: BlockRef) -> Result<(), CoinsError>;
    // Dropping the overlay discards it (a failed connect).
}
impl<E: KvEngine> dash_consensus::coins::CoinsView for BlockOverlay<'_, E> {
    type Error = CoinsReadError;                                   // §2.4.4
    fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, CoinsReadError>;  // child → active → F → prefetch → DB (into scratch)
    fn have_coin(&self, op: &OutPoint) -> Result<bool, CoinsReadError>;            // no caching on a miss (BIP30 output probes)
    fn best_block(&self) -> Option<BlockHash>;                     // the tip's until the commit (ConnectBlock reads it first, :2523)
}
```

- `scratch` holds only DB hits that a `&self` read could not cache, such as `access_by_txid` on CH3a's legacy path. Its bytes are counted in the budget as overlay usage and freed when the overlay drops.
- `StableBytes` is about 40 lines of `unsafe`: an `UnsafeCell<Vec<NonNull<[u8]>>>`, push-only behind `&self`.
  - Each value is `Box::into_raw`'d once, and returned borrows derive from that raw pointer. Growing the `Vec` moves only `NonNull`s, never a `Box`, so no uniqueness is re-asserted over a borrowed allocation.
  - `Drop` frees each value with `Box::from_raw`.
  - Miri runs it under Stacked Borrows and Tree Borrows.
  - rev0's `Vec<Box<[u8]>>` would have been Stacked-Borrows UB on reallocation (Opus F17).
- **Why the overlay starts empty:** special-transaction processing, which runs before the UTXO loop, sees the `pprev` state (quirk 26).
- **Differences from Core's child** (`the_overlays_documented_differences`):
  - **Declared deviation (DEC-131).** With no child entry, `add(op, _, possible_overwrite = false)` consults the tip's `have_in_cache` (active, then F; no DB read) and returns `OverwriteUnspent` if the coin is unspent there. Core's child knows only what it has fetched: it adds the coin FRESH, and its `BatchWrite` throws `FreshMisapplied` (`coins.cpp:217-223`) against the same cached coin. Both are a `logic_error` (`Fatal`, §2.10), raised at different ops, and neither is reachable on a valid chain (§3.2). `coins_overlay_diff` exempts that class.
  - Spending a child tombstone returns no coin, where Core returns true with a spent coin. The undo gets nothing either way.

#### 2.4.3 `PrefetchMap` (the consumer side of CH4's S2)

```rust
pub struct PrefetchBatch { pub fetch_gen: u64, pub epoch: u64, pub entries: Vec<(OutPointKey, PackedCoin)> }   // positive results only
```

The fence is A1 §3.4's, verbatim:
1. Results carry the `released_gen` current when their snapshot was taken.
   - **The order (DEC-142, O5):** the producer reads `fetch_gen` **before** it takes its snapshot, or V assigns it when it dispatches the request. The other order can stamp a snapshot taken before W's write of F with the generation after the release; F no longer shadows the keys it changed, so the batch would bring back a coin F spent. The batch also carries `epoch`, `GuardedEngine::write_epoch()` read after `fetch_gen` and before the snapshot, and `admit_prefetch` debug-asserts that a batch of the current generation has `epoch` ≥ the epoch at the release. The release reads that epoch before it advances the generation, so a producer that sees the new generation always reads an epoch at or after it. CH2a's test is `the_fence_reads_the_generation_before_the_snapshot`; X32 skips the assert, so a debug build shows the resurrection too. **CH4** keeps the order in its S2 producer (a3's `GuardedSnapshot` can record the epoch itself) and adds a deterministic interleaving test of a producer thread against W and V.
2. A result is admitted only if `fetch_gen == released_gen` and the budget keeps headroom (`prefetch ≤ B/16`). Otherwise it is dropped, which costs one DB read later.
3. Absence is never cached.
4. `release` and any `disconnect_block` clear the map.

CH2a tests this with a synthetic producer in `coins_cache_model`; CH4 ships the rayon producer.

#### 2.4.4 Read errors and the fallible `CoinsView` (ruling 3)

**What Core does.** `CCoinsViewErrorCatcher` catches a `runtime_error` from a coins read, runs its callbacks, logs and then calls `std::abort()` (`coins.cpp:356-379`):
- the callback shows "Error reading from database, shutting down." (`init.cpp:2089`);
- the log line is "Error reading from database: %s".

The process ends **before any verdict is formed**. `HaveCoin` is routed through the catcher too (bitcoin#26331), so every coins read aborts. RL2 documents it (CONS-22).

**Choice: a fallible trait** (DEC-77 Q1). CS5a adopted it at 17014d4b (`dash-consensus/src/coins/view.rs` on `node/cs5a-coins`). The trait is generic over `type Error`, with **no concrete error type in the pure crate** (DEC-78). Only CH2's `GuardedEngine` constructs one (§2.10). The adopted shape:

```rust
// dash-consensus::coins::view (CS5a, amended)
pub trait CoinsView {
    /// A storage failure. Never a "missing coin". In-memory views use `core::convert::Infallible`.
    type Error: core::fmt::Debug;
    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error>;
    fn have_coin(&self, outpoint: &OutPoint) -> Result<bool, Self::Error> { Ok(self.coin(outpoint)?.is_some()) }
    fn best_block(&self) -> Option<BlockHash>;     // held in memory by every implementation
}
impl CoinsView for MemoryCoins { type Error = core::convert::Infallible; /* … */ }
pub fn access_by_txid<'v, V: CoinsView + ?Sized>(view: &'v V, txid: &TxHash) -> Result<Option<CoinRef<'v>>, V::Error>;
```

CS5b's consumers become `fn check_tx_inputs<V: CoinsView>(…) -> Result<Amount, ViewOr<V::Error, TxReject>>`, and likewise for sigops with coins and for sequence locks. `ViewOr::View` is never mapped to a reject. With `Infallible`, pure callers pay nothing.

**Why fallible over "infallible + latch check before every verdict":**
- The latch rule would have to cover every place a lookup result leaves V: rejects, *successes* (a false "absent" passes BIP30), overlay merges, `ChainEvent`s, peer actions and RPC replies. That is review-enforced and easy to miss in CS5b's pure code.
- The `Result` makes the type system enforce it.
- CS5a is unmerged, so the cost is a signature change now.

**The rule CH3a and MP1 follow** (and that the latch still backs):
1. A `CoinsView::Error` (or `CoinsReadError` from `fetch`/`have_inputs`) propagates as `ConnectError::Fatal` / `MempoolError::Fatal`. It is never converted into a validation result, `DISCONNECT_FAILED`, `FAILED_VALID` or a reject string.
2. `CoinsReadError` can be constructed only by `GuardedEngine`: by its read path on an engine error, or by `GuardedEngine::corrupt` when a stored coin fails to decode (DEC-78). Both **trip the node-wide latch first** (§2.10). So every later write, barrier or flat-file mutation is refused, whatever the caller does.
3. **Production:** after tripping, `AbortSink::read_failed` logs Core's two strings and calls `std::process::abort()`, exactly as Core does, with no shutdown flush. **Seam and tests:** the sink records `Mark::Abort` and returns, and rule 1 carries the error out of V's operation.
4. A CH4 prefetch read error is reported the same way. A failed prefetch is never silently dropped, because Core aborts on any coins read error.

#### 2.4.5 Cache membership: `have_in_cache` (rev5, DEC-102; replaces rev2–rev4's Core-membership shadow)

**Why it matters.** Core's `HaveCoinInCache` (`coins.cpp:167-170`) picks two Core-visible outcomes, though no consensus or block outcome:
- **ATMP's verdict.** When an input is missing, Core checks whether any output of the transaction itself is in its cache. If one is, the reply is `txn-already-known` (`TX_CONFLICT`); otherwise it is `bad-txns-inputs-missingorspent` (`TX_MISSING_INPUTS`) (`validation.cpp:970-979`). `mempool_accept.py` asserts `txn-already-known` right after `generate` (`:107`). The package path asks the same question (`:1591-1592`).
- **The orphan path.** Only `TX_MISSING_INPUTS` sends a P2P transaction to the orphan pool and requests its parents (`net_processing.cpp:4963`).
- **Uncache bookkeeping.** ATMP records which prevouts were not already cached (`coins_to_uncache`, `validation.cpp:963-964`) and uncaches them if the transaction is rejected.

**The rule (DEC-102).** `have_in_cache(op)` resolves op first-hit over **active, then F** and returns true iff the entry found is present and unspent. An active tombstone hides F. It **never** reads prefetch or the DB, just as Core's `HaveCoinInCache` reads only `cacheCoins`. So it is `&self`, infallible and O(1): at most two hash lookups and no I/O.
- **Connects and fetches add members**, as in Core: a block's outputs and restored coins enter active, and a caching `fetch` that misses active copies its coin into active as clean (§2.4.1).
- **Freezes do not change membership:** F is still searched.
- **A release drops F's entries,** as Core's flush empties its map (`coins.cpp:254-265`). An entry that was also fetched into active since the freeze stays a member.
- **`uncache` removes only a clean active entry,** as Core's `Uncache` does (`coins.cpp:284-297`). F is immutable, so an F entry stays a member until release. Core's map would keep a clean entry fetched before its flush only until that flush, so this is again only a difference of flush points.

**When it equals Core.** It equals Core's answer whenever neither a Core flush nor one of our releases falls between the coin entering the cache and the query. That covers `mempool_accept.py`'s case, every same-block query, and every query between two releases that Core also makes between two flushes. L1's aligned mode checks it per op (§6.2).

**D12, the allowed difference.** Our flush points are not Core's (D1, D11), and Core's own are not reproducible across runs (its child-cache walk is salted). Across a release Core has not made, or a flush we have not made, the answer can differ. ATMP's reason then differs (`txn-already-known` against `bad-txns-inputs-missingorspent`), and for a P2P transaction that decides whether it goes to the orphan path and requests its parents. Both outcomes are rejections. No consensus result, block, chainstate or mempool content depends on the difference. It is declared as D12 (§3.9) and listed for compat ALLOWED-DIFFS (§7.4).
- **Why not emulate Core** (rev2–rev4's shadow and Core-model meter). Opus r6 showed that exact emulation needs Core's own 24 h clock, prune and evo state at every call site (O-R6-M2), and the order of Core's salted child-cache walk, which is not determined (O-R6-M3). It would also need a DB read inside a `&self` answer (O-R6-M4). DEC-102 drops it. r6 M2 and M3 are moot, and M4 is resolved, because nothing reads the DB.

**Fetch sites.** Under DEC-102 nothing has to be hooked: membership follows our own layers, whatever filled them. A Core `CoinsTip()` reader ported **without** caching only makes D12 more frequent: an output it would have cached is not a member. So each site is ported as a caching `CoinsCache::fetch` (or `uncache`) V command (ruling 9), and the owner adds that clause (§7.4). The v24 sites, from O-R6-m1:

| Core site (v24.0.0-rc.3) | What it does to `CoinsTip()` | Owner |
|---|---|---|
| `ConnectBlock` inputs via `HaveInputs`/`UpdateCoins`; `DisconnectBlock` restores | fetch + spend; restore | CH3a (through §2.4.2's overlay) |
| ATMP prevouts through `CCoinsViewMemPool` (`validation.cpp:932`), `coins_to_uncache` (`:963-964`), the outputs loop (`:970-979`), the package path (`:1591-1592`) | fetch; `HaveCoinInCache` | MP1a, MP1b |
| `LimitMempoolSize` Uncache (`validation.cpp:404`); ATMP's reject Uncache (`:1744, 1775`) | uncache | MP1a, MP1b |
| `gettxout`; `gettxoutproof` (`rpc/txoutproof.cpp:77`); `verifychain` level ≥ 3 (`rpc/blockchain.cpp:1440`, a child over the tip) | fetch | RP1a, RP1b |
| REST `getutxos` (`rest.cpp:917`) | fetch | ZQ1 |
| `BroadcastTransaction`'s `AccessCoin` of each output (`node/transaction.cpp:51`) | fetch | RP3a |
| `TestBlockValidity`'s child over the tip (`viewNew` in `validation.cpp`), from `getblocktemplate` and `generate` via `CreateNewBlock` | fetch (child reads cache in the tip) | MN0, MN1 |
| `GetUTXOCoin`, `GetUTXOHeight`, `GetUTXOConfirmations` (`validation.cpp:326-347`), used by `rpc/evo.cpp:1584, 1605`, `rpc/masternode.cpp:679`, `init.cpp:2491` | fetch | CH7a, RP2a, RP2b |
| `evo/providertx_service.cpp:128` | fetch | RP2a, RP2b |
| InstantSend `CheckSpecialTx` on the tip (`instantsend/signing.cpp:215`) | fetch | LK1b, LK1c |
| CoinJoin's `CCoinsViewMemPool` over the tip (`coinjoin/coinjoin.cpp:374, 477`) | fetch | CJ1 |
| wallet `findCoins` (`node/coin.cpp:17`) | fetch | WL2a |

A child view over the tip (CH3a's overlay, `TestBlockValidity`, `verifychain`) caches what it fetches in active, not in the child (§2.4.2, §2.5 "Ownership").

**Memory.** Nothing beyond the layers: no sets, no meter. r6's n1 (`Usage` had no field for the shadow's sets) is moot.

**Tests (a2, §7.1).**
- `mempool_accept.py`'s case: mine a block and query `have_in_cache` for its outputs, which is true (active). Force an own-cause freeze and query with F in flight, which is still true (F). After release it is false, and ATMP answers `bad-txns-inputs-missingorspent`. That is the D12 case, asserted as such.
- A fetched coin is a member. After `uncache` of that clean entry it is not. A DIRTY entry and an F entry are not removed by `uncache`.
- An active tombstone over a live F entry gives false.
- A prefetch-only coin and a DB-only coin give false.
- No path of `have_in_cache` reads the DB or prefetch: a counting `CoinsView` stub and the prefetch map's hit counter stay at zero over the whole case list.
### 2.5 The budget and the memory split (ruling 4)

```rust
pub struct CacheSizes { pub block_tree_db: u64, pub tx_index: u64, pub address_index: u64, pub timestamp_index: u64,
                        pub spent_index: u64, pub filter_index: u64, pub coins_db: u64, pub coins: u64 }
impl CacheSizes { pub fn calculate(dbcache_mib: i64, args: &IndexArgs, n_filter: usize) -> Self; }   // v24 (DEC-82)

pub struct CoinsBudget { coins_share: u64, max_mempool: u64, f: Ratio /* 1/2 */,
                         batch_reserve: u64 /* EngineTuning::batch_reserve(S), rev3 (G-R3-m2) */ }
impl CoinsBudget {
    // a3 as built: the node's constructor takes R from the tuning; `new` takes the numbers (tests).
    pub fn for_engine<T: EngineTuning>(sizes: &CacheSizes, max_mempool: u64, f: Ratio, s: u64, tuning: &T) -> Self;
    pub fn new(coins_share: u64, max_mempool: u64, f: Ratio, batch_reserve: u64) -> Self;
    pub fn total(&self, mempool_usage: u64) -> u64 { self.coins_share + self.max_mempool.saturating_sub(mempool_usage) }  // B(t)
    pub fn reserve(&self, flush_in_flight: bool) -> u64 { if flush_in_flight { self.batch_reserve } else { 0 } }        // R
    /// The physical flush policy's state: core_size_arith(native Σ, B(t)) (rev3, DEC-95; rev4, DEC-99).
    /// Core's arithmetic on CH2's own counter: NOT Core's flag, which is computed on a different number.
    pub fn size_state(&self, sigma: u64, mempool_usage: u64) -> CoinsCacheSizeState;
}
/// Core's GetCoinsCacheSizeState arithmetic (validation.cpp:2945-2966), in i64. Pure; CH2a (rev5, O-R6-m5).
/// rev5 (DEC-102): native Σ is the only counter, so rev4's NativeUsage/CoreModelUsage newtypes are gone.
pub fn core_size_arith(usage: u64, total_space: u64) -> CoinsCacheSizeState;
pub struct Usage { pub active: u64, pub frozen: u64, pub prefetch: u64, pub overlay: u64,
                   pub held_out: u64 /* live LedgerBox undo coins (a shared AtomicU64 read), rev4 */, pub reserve: u64 }
impl Usage { pub fn total(&self) -> u64; }                  // native Σ
pub enum CoinsCacheSizeState { Ok, Large, Critical }       // Core's three states, over native Σ
/// rev5 (O-R6-m5): what CoinsCache::boundary_state reports; b1's FlushPolicy maps it onto Core's modes and its other causes.
pub enum CoinsBoundary { Ok, Threshold, Large, Critical }  // Threshold only with no F in flight

pub struct EngineMemory { /* RocksTuning's (§2.5.1) */ pub cache: u64 /* E/2 */, pub wbm: u64 /* (block_tree_db + coins_db)/2 */, pub coins_write_buffer: u64 /* S */,
                          pub coins_write_buffers: i32 /* 3 */, pub other_write_buffer: u64 /* 4 MiB */ }
impl EngineMemory { pub fn for_chainstate(c: &CacheSizes, s: u64) -> Self; }   // E = block_tree_db + coins_db + 64 MiB
```

**`CacheSizes::calculate`** is Core's `CalculateCacheSizes`, line for line (`node/caches.cpp:15-42`):
1. Clamp `-dbcache` to [4, 16384] MiB.
2. Block tree: `min(total/8, 2 MiB)`.
3. Txindex: `min(rem/8, 1 GiB)` if `-txindex`.
4. Address (256 MiB cap), timestamp and spent shares (`node/caches.cpp:25-30`).
5. Filter indexes.
6. `coins_db = min(rem/2, rem/4 + 8 MiB, 8 MiB)`; the rest is `coins`.

rdashd prints Core's `Cache configuration:` lines (`init.cpp:1986-2023`), and node-bench sums them.
- At `-dbcache=300` with txindex on: block tree 2, txindex 37.25, coins DB 8, **coins 252.75 MiB**.
- With txindex off: coins 290 MiB.

(Fable's design used 290 MiB for the default; Dash's node-bench cell runs with txindex, §8 row 11.)

**The coins budget** `B(t)` is Core's `nTotalSpace` **exactly** (`validation.cpp:2952-2953`). Our trigger reads the mempool's own maximum, as Core's `GetCoinsCacheSizeState` does.

**The physical flush policy: native Σ, at Core's call sites, with Core's arithmetic** (rev3, **DEC-95**; rev4, **DEC-99**; replaces rev1–rev2's rule O). Core's `nTotalSpace` is a flush *threshold*. `GetCoinsCacheSizeState` (`validation.cpp:2945-2966`) is evaluated only inside `FlushStateToDisk` (`:3036-3046`), at its call sites after a block, after ATMP and in the other modes of §3.3. During a block Core's cache grows unchecked. CH2 checks at the same call sites, in the same modes and with the same arithmetic, **on its own counter**:
- **Removed:** rule O's per-op bound `T(op)`, `T_max`, streaming admission, abort-and-retry, the oversized path and the `RULE_O` cause. DEC-80 Q4 and DEC-94 Q1 are superseded. An op allocates what it needs, and every layout grows past its presize (§2.3), so a valid block always completes.
- **The arithmetic.** `core_size_arith` is Core's function, in Core's i64 arithmetic, on a usage it is given:

  ```
  B     = coins_share + max(max_mempool − mempool_usage, 0)       // nTotalSpace (:2952-2953)
  large = max((9 · B) / 10, B − 10 MiB)                           // MAX_BLOCK_COINSDB_USAGE_BYTES (:2956-2958)
  CRITICAL iff usage > B;  else LARGE iff usage > large;  else OK  // (:2960-2965)
  ```

  `CoinsBudget::size_state` passes it **native Σ**. `FlushPolicy::decide` uses the result at Core's call sites and modes: `CACHE_CRITICAL` = IF_NEEDED ∧ CRITICAL after every connect and disconnect (`:3304, 3457`), and `CACHE_LARGE` = PERIODIC ∧ ≥ LARGE after each `ActivateBestChain` step and after ATMP. On a native CRITICAL it logs Core's "Cache size (%s) exceeds total space (%s)" with native Σ (a debug-log line; its timing is D1's).
- **These are not Core's flush points** (DEC-99). Native Σ is not Core's `DynamicMemoryUsage` and is not meant to be. On the same blocks the two numbers differ for several reasons:
  - our slots are 72 B, against Core's ≈ 104 B pool node;
  - our index or shard tables grow by their own policy, not Core's prime-sized buckets;
  - our scripts are packed;
  - our active layer empties at our freezes, not at Core's flushes;
  - native Σ also counts F, prefetch, R and layout growth.

  So our LARGE and CRITICAL can fire **later** than Core's (density) or **earlier** (F, prefetch, R, growth). Sol's r4 witness, checked against Core's pool and `memusage` with a pinned probe: one block leaves 20,000 inline P2PKH outputs on an empty tip at `-dbcache=4`. The arena's native Σ is 1,736,320 B (six 3,640-slot chunks and a 32,768-bucket index), above LARGE = 1,651,507 and below B = 1,835,008. Core's map is 2,263,712 B, above B. Native says LARGE, so there is no IF_NEEDED flag (THRESHOLD still freezes). Core says CRITICAL. Flush timing is neither consensus nor RPC-visible (D1, D11), so DEC-99 allows this. Core's own flush point is not even fixed: its child-cache walk is randomly salted, so the block at which Core reaches LARGE or CRITICAL can move between two Core runs (O-R6-M3).
- **Nothing in CH2 models Core's flush points** (rev5, **DEC-102**). rev4's Core-model meter and membership shadow are removed. The one Core-visible effect of Core's flush timing, `HaveCoinInCache`, now answers from our own layers and is declared D12 where it differs (§2.4.5).
- **Where native Σ is read.** Only at those call sites, all of which are op boundaries. The overlay has merged or been dropped, and `held_out` is 0 there (the block's undo is written or dropped; below), so native Σ = active + F + prefetch + R. The THRESHOLD freeze (active ≥ f·B, or the fallback's shard trigger, §2.3; §3.3) is decided at the same points. **No freeze happens mid-block** (unchanged).
- **With F in flight**, native CRITICAL makes V wait for release, then freeze (§3.3). LARGE waits for release (pending).
- **Mid-block growth and the peak.** Between two checks native Σ grows by what one op allocates:
  - child entries;
  - active's merged entries;
  - the whole long coin bytes of outputs and of fetched or restored inputs;
  - long undo copies (`held_out`);
  - layout growth (§2.3);
  - `scratch`.

  Consensus limits bound all of it: a block of ≤ 2 MB, inputs of ≥ 41 B and outputs of ≥ 9 B, stored scripts of ≤ 10,000 B (`script/script.h:534-537`). Core's cache is bounded by the same limits, and it holds each fetched input twice (parent and child, `coins.cpp:44-59`). **The peak is evidence, not an invariant** (DEC-95, DEC-96 Q2, DEC-99):
  - a2 records native Σ's peak for the witnesses of §7.1.
  - CH2c and the DEC-43 cell gate three things (§5.5):
    - `rss_anon_peak` must not be higher than dashd's;
    - **peak coins usage** must not be higher than Core's: the maximum of native Σ over block boundaries, against the maximum of Core's per-block `cache=%.1fMiB` in `UpdateTip` (`validation.cpp:3178`). This compares our counter with Core's logged number on purpose: it asks whether we use more memory, not whether we flush at Core's points;
    - **native block-boundary peak ≤ B** on the gated workloads (DEC-99), read per **DEC-100**: a CH2c / DEC-43 gate on the proxy and cell runs only, at block boundaries; native CRITICAL never fires there. The `-dbcache=4` witnesses are exempt.
  - Mid-block native peaks are reported from the tagging allocator, not gated. Core's `DynamicMemoryUsage` omits its child view, so `rss_anon_peak` under the common cap is the gating comparison for transients.
- **Ownership, so that native Σ is exact** (G-R3-M1 item 3; rev4, G-R4-m2):
  - **DB hits.** A DB read arrives as the engine's owned `Vec<u8>`. `KvRead` promises an owned `Vec`, not `capacity == len`. The read runs inside the `coins` tag scope, so the `Vec` is tagged from its allocation. An inline coin is copied into a slot. A long coin is either copied into the arena's long store, or becomes the fallback's `Box<[u8]>` by `into_boxed_slice`, which reallocates when `capacity ≠ len`; the overlap is then the `Vec`'s capacity plus `len`. Either way the `Vec` is gone before the op returns, so its bytes are in the mid-op peak and never in native Σ at an op boundary. (`RocksEngine`'s point path uses `to_vec`, which gives `capacity == len`.)
  - **Fetched inputs.** A fetched input is cached once, in active, as clean (Core's `FetchCoin` caches it in the parent, `coins.cpp:44-59`; Core's child also takes a copy, ours does not). **The child holds no pointer or offset into a parent layer.** Each read resolves the key through child → active → F → prefetch → DB and returns a borrow tied to `&self`. Mutations take `&mut self`, so no borrow survives one. A fallback shard rehash or an arena index rehash of active, for example while a later input is fetched, therefore moves nothing the child holds.
  - **`spend` moves only child-owned bytes.**
    - If the child owns the coin (created in this block, restored by this disconnect, or already copied into the child), its bytes move into the returned `PackedCoin`. A fallback `Box` moves. Arena bytes cannot leave the arena, so they are copied out, and the child's arena is freed when the overlay drops.
    - If the child does not own the coin (it lives in active, F, prefetch or `scratch`), `spend` **copies** it: an inline coin by value (34 bytes, no heap), a long coin into a new `Box<[u8]>` of exactly its length.
    - Either way the child then writes a DIRTY tombstone in its own layer (§3.2's flags), and **the parent's entry is unchanged until `commit`**. If a later check fails and the overlay is dropped, the parent still returns the original unspent coin.
  - **Undo copies stay on the ledger.** A long `PackedCoin` returned by `spend` owns a `LedgerBox`. A copied payload is allocated under the `coins` tag; a moved one carries its tag from the child, and its bytes move from the child's count to `held_out`. The box adds its allocation size to `Usage::held_out` when created and subtracts it when dropped, on whatever thread and in whichever component drops it: CH3a's undo record, or the overlay's error path. So `tagged(coins) == native Σ − R` holds at every op boundary, undo included. **At Core's call sites `held_out == 0`**, because CH3a writes or drops the block's undo before the block returns, as Core's `ConnectBlock` writes undo (`validation.cpp:2888`) before `ConnectTip`'s flush check. §6.7 asserts it. Inline undo coins live by value in CH3a's records, outside B, as Core's `CBlockUndo` is outside its cache.
  - **Restored coins** (disconnect) are borrowed from the undo record (CH3a's memory, outside B) and copied once into the child, which then owns them.
  - At the merge, child entries move into active: a `Box` move in the fallback, a copy in the arena, where the child's arena is freed when the overlay drops.
  - **A long coin's store holds the whole serialized coin**, not `len − 34` (G-R3-M1 item 1; rev2's figure was wrong). The slot's inline area is unused for it.
  - a2's ownership witnesses (§7.1): a long input fetched into a clean parent entry, read through the child, spent into an undo copy, then a failed later check, with the overlay and undo dropped. The parent must return the original coin byte for byte, `held_out` must return to 0, and the tagged count must equal native Σ − R after every op. The same case with a commit, then a disconnect from that undo, must restore an identical coin.
  - Freed slots and long bytes go to free lists. Index, shard and chunk capacity is kept until the layer drops at release. Native Σ counts capacity, not occupancy.

- **Release accounting** (GPT M1). W drops its `Arc<FrozenLayer>` **before** it sends `FlushDone`. V's `release` asserts `Arc::strong_count == 1` and drops it, so the memory is freed before V's accounting forgets it. Nothing else may hold F: copy-point readers use a DB snapshot after a forced flush, and mempool and RPC coin reads are V commands (ruling 9).
- **R = `EngineTuning::batch_reserve(S)`** while a flush is in flight, else 0 (rev3, G-R3-m2: `CoinsBudget` takes the selected engine's value; no Rocks constant outside `RocksTuning`). It is the engine memory a sub-batch holds outside the tagging allocator:
  - **`RocksTuning`: S + 64 KiB.** `RocksEngine::batch_with_capacity` preallocates the C++ `WriteBatch` rep (`dashd-store/src/rocks.rs:605`, ST1 D3). A sub-batch closes once `size_bytes() > S`, and one record is < 64 KiB, so the rep never reallocates. `RocksBatch::size_bytes` is the rep's size (`rocks.rs:484`), exact for the rep, unlike the trait's documented lower bound. `size_bytes ≤ S + max_record` is asserted per sub-batch.
  - **`MemoryTuning`: 0.** `MemoryBatch` is Rust memory outside the `coins` tag, in test engines only.
  - The trait's default `batch_with_capacity` (which falls back to `batch()`) is not enough for an engine that ships: its `EngineTuning` must state the batch's peak, and a3 tests it.
- **S = min(`-dbbatchsize`, max(coins_share/8, 64 KiB))** is static. It is derived from the coins share, not from B(t), so the `coins` CF's `write_buffer_size` can equal it (Opus N3). That is Core's 16 MiB at any realistic share. For `feature_dbcrash` (`-dbcache=4`, `-dbbatchsize=200000`, `feature_dbcrash.py:57`) only the number of partial batches changes.
- **Freeze** when F is none, at a block boundary, and either `active ≥ f·B(t)` (f = 0.5; PF3 tunes it) or, for the fallback, a shard of active has reached its trigger (§2.3, rev4). With F in flight, active grows until the native CRITICAL check after a block makes V wait for release, the only I/O stall on V.
- **Native accounting is exact arithmetic** on the cache's own allocation sizes: `HashTable::allocation_size`, shard tables, long-coin boxes, `LedgerBox`es, the prefetch and overlay maps, `StableBytes`. In bench and test builds the tagging allocator checks it to the byte (§6.7, DEC-77). That proves the native ledger exact; nothing in CH2 claims Core's number (DEC-102). There is no production allocator wrapper.
- **D11** (§3.9): the physical policy reads native Σ, where Core reads `CoinsTip().DynamicMemoryUsage()` (`validation.cpp:2951`). Our flushes can come earlier or later than Core's; that is flush timing only (D1).

**The engine budget E: a configured target with a derived soft ceiling** (A1 §3.4 erratum A-2; rev1 for GPT M2). Everything below up to "Why this split" is `RocksTuning`'s (§2.5.1): E's formula is engine-neutral, its mapping onto caches and buffers is not. The 74 MiB split is unchanged:

`E = block_tree_db + coins_db + 64 MiB`

At `-dbcache=300` that is 2 + 8 + 64 = **74 MiB**: Core's block-tree, chainstate and EvoDB caches (`evo/evodb.cpp:38`). Core splits each LevelDB cache n/2 for blocks and n/4 per write buffer, "up to two write buffers … simultaneously" (`dbwrapper.cpp:113-118`).

**rev0's claim that "memtables and blocks together never exceed E" is withdrawn.** RocksDB's `WriteBufferManager` flushes and stalls at a *soft* threshold. A default LRU cache can exceed its capacity by pinned or referenced entries. GPT's native witness (RocksDB 11.8.1, `/work/scratch/ch2-gpt/wbm-probe.txt`) shows a 74 MiB cache at 80 MiB usage, with 40 MiB of memtable charge and `ShouldStall=1`. LevelDB's cache and its two write buffers are equally soft, so Core's 74 MiB is a configured share too.

**The shipped split (CH2c D27, D31) replaces the capacities below:** a block cache of E/2 (37 MiB at `-dbcache=300`), and a write buffer manager and `max_total_wal_size` of (block_tree_db + coins_db)/2 (5 MiB), charged to that cache. D27's 5 MiB cache re-read the coins table on every miss (BL-373); E/2 holds the replay's working set as E did. The rest of this section stands.

The configuration:
- **One LRU cache of capacity E with `strict_capacity_limit = false`.** A strict cache fails inserts and reads with `MemoryLimit`, which our fail-stop would turn into an abort.
- **A `WriteBufferManager` of E/2, charged to that cache, with `allow_stall = true`.** Mutable memtables flush near 7/8 of E/2. Writers stall above E/2. This includes V's single-row effect writes, whose stall clears through RocksDB's own flush threads, never ours; a stalled write waits in `RocksEngine::write`'s loop (DEC-164, below).
- **No write waits inside RocksDB (DEC-164; a3 fix r2, Opus O1).** RocksDB wakes a writer stalled on the manager only when memory is freed (`write_buffer_manager.cc:92-99, :141`). A flush that fails, for example on a full disk, frees nothing. Such a writer would wait for ever, the latch would never trip and the node would hang. LevelDB re-checks `bg_error_` each time its writer wakes (`leveldb/db/db_impl.cc:1322-1356`), and a background error wakes every waiter (`:650-654`).
  - **The mechanism.** `RocksEngine::write` writes with `no_slowdown`, so RocksDB refuses a write that would wait with `Incomplete("Write stall")`. That covers both a manager stall and a write-controller stop (`db_impl_write.cc:2173-2178, :2865-2870`). On that refusal it runs `check_writable`, the check every write and barrier runs first: it ends the wait with the refusal if another write or barrier has latched a failure, so nothing commits after it, and runs `check_background`, which trips the latch and returns the error if one is set. Otherwise it sleeps (1 ms, doubling to 64 ms) and retries the same batch.
  - **The batch is built through RocksDB's C API**, so a retry does not copy it: rust-rocksdb's `write_opt` takes the batch by value.
  - **The stalled writer still waits until memory is freed**, so `allow_stall` still bounds memtable memory. It waits in our loop, which sees a background error within one pause.
  - **No write-controller delays, in any `RocksEngine`, since every write is retried so.** A refused write is charged the delay again: `WriteController::GetDelay` empties the credit and moves the refill time (`write_controller.cc:51-80`). So a `no_slowdown` retry faster than the delay would never pass. The profile therefore sets:
    - `level0_slowdown_writes_trigger = level0_stop_writes_trigger = 36`;
    - `soft_pending_compaction_bytes_limit = 0`, which RocksDB raises to the hard limit (`column_family.cc:362`);
    - at most 3 write buffers per column family; the memtable delay needs more than 3 (`column_family.cc:1027`). `cf_options` asserts this.
    - RocksDB's `default` column family keeps its own triggers, but it is always empty.
  - **Stops remain**: the memtable count, 36 L0 files and the hard pending-compaction limit. A stopped write waits in the same loop. The OPTIONS test asserts the triggers.
  - **`flush_memtables` flushes with `allow_write_stall`.** Without it, RocksDB first waits until the flush would not stall writes, and that wait outlasts a soft background error, such as compactions that cannot run on a full disk (`db_impl_compaction_flush.cc:3098-3170`, `error_handler.h:77-83`). The flush itself ends when it is done or a background error stops the database (`:3183-3250`). `sync_wal` has no such wait.
  - **With CH2b's flush protocol (§4.1):**
    - A Bj or Bk that meets a stall waits in `RocksEngine::write`'s loop, as it waited in RocksDB before. RocksDB's flush and compaction threads clear the stall; W does not have to.
    - A background error ends the wait within one pause (≤ 64 ms): the write returns the latched error, and CH2b maps it to `AbortNode` (§2.10) as it maps any failed write. W must not retry a failed write itself.
    - `flush_memtables` no longer queues behind a stalled writer. A manual flush enters RocksDB's write queue to switch memtables (`db_impl_compaction_flush.cc:2744`), where a stalled leader used to hold it (`db_impl_write.cc:2917-2929`). A refused write leaves that queue between attempts.
    - RocksDB's stall statistics (`stall-micros`) no longer count these waits; each refused attempt adds one to `write-buffer-manager-limit-stops` instead. CH2b and CH2c time the stall around `write()`, the source of §5's V-stall and W-stall figures. The polling adds up to 64 ms to a stall, which CH2c measures. `dashd-store-bakeoff`'s stall figures no longer compare with ST1's recorded runs.
    - L0 can reach 36 files before writes stop, where the defaults would slow writes from 20. CH2c's W-MC and `engine-memory` workloads report the L0 count, and PF3 may tune the trigger.
- **Pinning.** `cache_index_and_filter_blocks = true`, partitioned filters, `pin_top_level_index_and_filter = true`, and **`pin_l0_filter_and_index_blocks_in_cache = false`** for the chainstate profile. ST1's default pins L0 (`dashd-store/src/rocks.rs:401`), which would grow the pinned set with the number of L0 files.
- **Write buffers.** The `coins` CF uses `write_buffer_size = S`, 3 buffers and merge-1; the other CFs use 4 MiB.
- **Reads** are buffered `pread`: no mmap, no direct I/O (lever L9).
- **`flush_memtables()` after every Bk** (W, off V's path; 0.16–0.82 s, ST1 D2). It returns the memtable charge to blocks between flushes.
- **`max_total_wal_size = E`** is a flush *trigger* (GPT), not a bound on startup replay; the `chainstate-open` workload measures replay. **`manual_wal_flush = false`** is set and asserted (item 12).

**The derived soft ceiling**, for diagnosis:

`engine ≲ max(E, P + M + I) + X`

- P is pinned top-level partitions, O(#SSTs) and small.
- M is memtables: ≤ E/2 plus the one write that crosses the threshold (≤ max(S, |B1|)), since WBM charge tracks arena memory to its 256 KiB reservation unit.
- I is blocks pinned by live iterators and snapshot reads (copy-point scans).
- X is RocksDB memory outside the cache: table readers, WAL and compaction buffers.

**What gates it:** CH2c's `engine-memory` workload (§5.7) reports `block-cache-usage`, `block-cache-pinned-usage`, `cur-size-all-mem-tables`, `size-all-mem-tables`, `estimate-table-readers-mem` and RssAnon. It covers large B1/evo batches, all CFs, a held full scan during flushes, stalled flushes (cgroup `io.max`) and compactions. **The binding evidence that the whole process stays at or below dashd is DEC-43's `rss_anon` gate** (§5.5), the CH2c proxy, and the M3 cell.

**Why this split gives the DEC-43 cell its best chance** (ruling 4, unchanged):
- The coins share is Core's to the byte, so any density gain is pure gain.
- RocksDB gets the same 74 MiB LevelDB gets, without shrinking B.
- The charged memtables return their memory to blocks after each Bk.

**Other memory** stays outside `-dbcache`, as in Core:
- the block-index arena (CH1);
- the in-flight evo delta, bounded by Core's 64 MiB EvoDB-critical trigger (§3.3);
- the dirty index records of a freeze;
- the subscribers' journals (byte-bounded, §2.9 item 2);
- index DBs, which get Core's per-index shares under their own engines.

#### 2.5.1 Engine-specific parameters: `EngineTuning` (rev2 addendum; DEC-85)

DEC-43's RocksDB choice stands **provisionally**: SB0 re-checks storage against DEC-81's mobile constraints (binary size, memory, iOS and Android builds). So every number and hook that depends on the engine lives in one place that SB0's ruling can swap. The rest of CH2 sees the engine only through ST1's `KvEngine` contract.

```rust
/// Every engine-specific choice CH2 makes. CoinsCache, CoinsBudget, FlushMachine, Recovery and SubscriberDriver are
/// generic over `T: EngineTuning` (rev5, O-R6-n3: the associated `type E` rules out a bare `&dyn EngineTuning`)
/// and never read an engine's own configuration type.
pub trait EngineTuning: Send + Sync {
    type E: KvEngine;
    /// The engine target E from Core's shares (engine-neutral: block_tree_db + coins_db + 64 MiB), and how the engine
    /// spends it. RocksTuning: one non-strict LRU (E/2 since CH2c D31), WBM of (block_tree_db + coins_db)/2 (CH2c D27) charged to it with allow_stall, no L0 pinning, buffered pread.
    fn open_chainstate(&self, path: &Path, sizes: &CacheSizes, sub_batch: u64) -> Result<(Self::E, SchemaState), StoreError>;
    /// R: memory a sub-batch of `s` bytes holds outside the tagging allocator (RocksTuning: s + 64 KiB, ST1 D3).
    /// CoinsBudget takes this value (rev3, G-R3-m2); no other code knows an engine's batch cost.
    fn batch_reserve(&self, sub_batch: u64) -> u64;
    /// W, after each Bk, off V's path (RocksTuning: flush_memtables, returning the memtable charge to blocks).
    /// Through the guard, so a failure trips the latch and a tripped latch refuses it (a3 review).
    fn after_bk<K: KvEngine>(&self, db: &GuardedEngine<K>) -> Result<(), StoreError>;
    /// Subscriber journal budget in bytes; exact subscribers admit entries against it (§2.9 item 2). Inexact-only
    /// subscribers keep no journal.
    fn journal_budget(&self) -> u64;
    /// The blind-delete span when d_max is None (RocksTuning: 2^24 blocks; one range tombstone costs the same at any width).
    fn unbounded_delete_span(&self) -> u32;
    /// Diagnostics for the engine-memory workload and W-MC (RocksTuning: the six RocksDB properties of §2.5).
    fn memory_report(&self, db: &GuardedEngine<Self::E>) -> EngineMemoryReport;
}
pub struct RocksTuning { /* EngineMemory, RocksConfig */ }   // CH2a a3
pub struct MemoryTuning;                                     // MemoryEngine and ST3a's FaultyEngine: R = 0, after_bk no-op
```

| Parameter | `RocksTuning` (today) | Engine-neutral part (stays if the engine changes) |
|---|---|---|
| E | `block_tree_db + coins_db + 64 MiB`; one non-strict LRU of E/2 (CH2c D31), memtables charged through a stalling WBM of (block_tree_db + coins_db)/2 (CH2c D27); per-CF write buffers (`coins` = S, 3 buffers; others 4 MiB); no L0 pinning; buffered `pread` | E's formula: the engine gets Core's LevelDB shares plus Core's fixed EvoDB cache, and the DEC-43 `rss_anon` gate binds the whole process |
| R | `S + 64 KiB` for the C++ `WriteBatch` | R is whatever native memory a sub-batch holds; reserved arithmetically |
| post-Bk hook | `flush_memtables()` | step 11 calls `after_bk`; tests assert the `after_bk` call, not a Rocks-specific flush (rev3, G-R3-m2). Copy points may still call `KvEngine::flush_memtables`, a generic operation |
| WAL | `max_total_wal_size = E` (a trigger); `manual_wal_flush = false` set and asserted | ST3a item 12 in the engine's own terms: a Durable commit syncs everything before it |
| journal budget | 32 MiB per exact subscriber | **whether** a subscriber has a journal at all (inexact-only subscribers keep none, so they never post `FlushDue(DMax)`), and the admission rule (§2.9 item 2) |
| blind-delete span | 2^24 (range tombstones) | the blind delete covers every height a journal can hold |
| ST1 requests | `delete_range` (BL-38), WBM config, `manual_wal_flush` | `delete_range` is a `KvEngine` operation every engine provides |

**What the protocol needs from any engine** (the `KvEngine` contract CH2b uses, and nothing more): Buffered and Durable batch commits; `sync_wal`; `delete_range`; snapshots and `multi_get`; a cursor in key order; and DEC-62's recovery contract (subset semantics, or stronger). ST3a's `FaultyEngine` already checks the protocol against exactly that contract, so a swapped engine is re-verified by rerunning CH2b's CRASH suite on it. The D_max-driven flush count (§5.9) depends only on the neutral column: a txindex-shaped subscriber keeps no journal and posts no DMAX flushes under any engine. a3 adds a **custom-tuning test**: a test `EngineTuning` whose reserve and journal budget differ from both shipped ones, with `CoinsBudget`, `FlushMachine` and the driver shown to use its values (G-R3-m2).

### 2.6 Tables, schema, `meta`

```rust
pub static CHAINSTATE_SCHEMA: DbSchema = DbSchema { name: "chainstate", version: 1, cfs: &[
    CfSpec::new("meta", Ordered), CfSpec::new("block_index", PointLookup), CfSpec::new("block_files", Ordered),
    CfSpec::new("coins", PointLookup), CfSpec::new("mn_diff", Compressed), CfSpec::new("mn_snapshot", Compressed),
    CfSpec::new("credit_pool", Compressed), CfSpec::new("mnhf", Ordered), CfSpec::new("llmq_mined", Compressed),
    CfSpec::new("llmq_mined_h", Ordered), CfSpec::new("llmq_snapshot", Compressed) ] };   // all A1 §2.9 CFs from v1

pub struct CoinsTable { cf: Cf }
impl CoinsTable {   // rev3 (n1): over guarded handles only (O-R2-m9)
    pub fn get(&self, db: &impl GuardedRead, k: &OutPointKey) -> Result<Option<PackedCoin>, CoinsReadError>;
    pub fn multi_get(&self, db: &impl GuardedRead, ks: &[OutPointKey]) -> Result<Vec<Option<PackedCoin>>, CoinsReadError>;  // CH4
    pub fn put(&self, b: &mut impl KvBatch, k: &OutPointKey, v: &[u8]);
    pub fn erase(&self, b: &mut impl KvBatch, k: &OutPointKey);
    pub fn cursor<'a, E: KvEngine>(&self, snap: &'a GuardedSnapshot<'_, E>) -> impl Iterator<Item = Result<(OutPointKey, PackedCoin), CoinsReadError>> + 'a;
    pub fn estimate_size<E: KvEngine>(&self, db: &GuardedEngine<E>) -> Result<u64, CoinsReadError>;   // gettxoutsetinfo.disk_size
}
/// `GuardedRead` is the guarded engine or a `GuardedSnapshot`: every read trips the latch on an engine error and routes
/// a decode failure to `corrupt` (§2.10). a3 as built: a trait both implement, not an enum.
pub trait GuardedRead { type E: KvEngine; fn engine(&self) -> &GuardedEngine<Self::E>;
                        fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, CoinsReadError>;
                        fn multi_read(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, CoinsReadError>; }
impl CoinsTable {
}
pub struct Meta { pub tip: Option<BlockHash>, pub head_blocks: Option<(BlockHash, Option<BlockHash>)>,
                  pub evo_tip: Option<BlockHash>, pub pruned_files: bool, pub last_file: u32, pub reindexing: bool,
                  pub wiping: bool }
pub fn wipe_chainstate<E: KvEngine>(engine: &GuardedEngine<E>) -> Result<(), StoreError>;    // CH5's -reindex-chainstate
```

| CF / key | Value | Written by |
|---|---|---|
| `coins` / `txid ‖ VARINT(vout)` | Core's `Coin` bytes, **no XOR** (Core XORs values, `dbwrapper.h:116`; DEC-11 own format) | coin sub-batches, B1 (one-batch), Bk, recovery |
| `meta` / `tip` | `BlockHash` (32 B) | B1 (one-batch), Bk, recovery's final batch; **absent during a transition** |
| `meta` / `head_blocks` | `new ‖ old` (64 B; `old` all-zero when null, as Core's null `uint256`, `txdb.cpp:130-139`) | B1 (split); erased by Bk or recovery's final batch |
| `meta` / `evo_tip` | `BlockHash` | B1 (CH7a's delta) |
| `meta` / `pruned_files`, `last_file` (BE u32, ST2's `BlockFilesState`), `reindexing`, `schema` | as A1 §2.9 | Bk/Bp; B1; CH5's `-reindex`; ST1 |
| `meta` / `wiping` | one byte `1`; erased when cleared, as the other flags | `wipe_chainstate` |

- **Guarded snapshots** (rev2, O-R2-m9). `GuardedEngine::Snapshot` is `GuardedSnapshot`, whose reads and iterators trip the latch on an engine error and route decode failures to `corrupt`. `CoinsTable` is typed over guarded handles and returns `CoinsReadError`, so the copy-point cursor and CH4's snapshot `multi_get` cannot bypass the latch. `corrupt` is `pub(crate)`.
- **The cursor and point reads** decode keys and values. A malformed record goes through `GuardedEngine::corrupt` (latch, then abort). Core's cursor stops silently there (`txdb.cpp:226-274`), and Core's point read returns "no coin" (`dbwrapper.h:324-326`). Both are reachable only on a corrupt DB and are declared as D7 (DEC-78).
- **Copy points.** `gettxoutsetinfo`, `scantxoutset` and `dumptxoutset` call `ForceFlushStateToDisk` (`rpc/blockchain.cpp:1238, 2771, 3173`). Ours force an ALWAYS flush, then iterate an engine snapshot taken after Bk returned.
  - **The snapshot is atomic with respect to the next flush** (rev5, O-R6-m2). Core holds `cs_main` across the flush and the cursor's creation (`dumptxoutset`, `:3165-3180`), and reads `GetBestBlock()` under it right after the flush (`gettxoutsetinfo`, `:1238-1246`). Ours: the ALWAYS command's reply carries a `GuardedSnapshot` that **W takes between Bk's return and `FlushDone`**. No later B1 can precede it, because W runs one flush at a time and V's next freeze waits for release. The RPC reports `meta.tip` read from that snapshot and asserts that the snapshot holds no marker (`head_blocks`).
  - Without this, a pending cause (CRITICAL, DMAX, EMPTY, PRUNE) could freeze straight after the copy point, and W's next split B1 (marker on, `meta.tip` erased) could land before the RPC thread took its snapshot.
  - b1 tests it: a pending cause held across a copy point must not change the reply. On regtest, ALWAYS also runs `flush_memtables` and `sync_wal` (A1 §2.9 live-copy rule), so `estimate_size` is the SST size. The FT bound `6400 < disk_size < 64000` (`rpc_blockchain.py:372-374`) is a CH2a unit test on a 200-block regtest DB.
- **`wipe_chainstate`:**
  1. a Durable `meta.wiping = 1` with the erase of `tip`, `evo_tip` and `head_blocks`;
  2. `delete_range` over `coins` and the evo CFs (BL-38);
  3. a Durable clear of `wiping`.

  A crash in between resumes the wipe. `block_index` and `block_files` are kept. The marker is its own key, not `reindexing`: that is Core's `-reindex` flag (`txdb.cpp:188-193`, CH5's), and a `-reindex` that wipes the chainstate must find it still set (a3 review, M1).
- **Schema.** All CFs exist from v1, so CH7a needs no migration. Before the first release a schema bump refuses older DBs; FR-STO-02 applies from the first release.

### 2.7 The flush API (CH2b)

**E-b1.1, E-b1.2:** `BlockRef`, `IndexWrite` and `IndexAccess` are `dash-chain`'s. V's decision and its freeze are split into `at_site` → `PendingFreeze::freeze_after_step`.

```rust
pub struct BlockRef { pub id: BlockId, pub hash: BlockHash, pub height: u32 }

/// What V hands W at a freeze. Built synchronously on V at a block boundary; immutable afterwards.
pub struct FreezePacket<D: StateDelta> {
    pub flush_id: u64, pub gen: u64, pub new: BlockRef, pub old: Option<BlockRef>,   // old = the previous durable tip
    pub coins: Arc<FrozenLayer>,
    pub delta: D,                                  // CH7a's EvoDelta of F's window, in block order; `()` until CH7a
    pub index: Vec<IndexWrite>,                    // CH1: every dirty block_index record at the freeze (hash, bytes)
    pub presumed_effects_seq: u64,                 // EffectLedger at the freeze (§4.1)
    pub tip_seq: u64,                              // rev2 (n2): seq of the event that made `new` V's tip (§4.4.1 rule 1)
    pub causes: FlushCauses, pub mode: FlushStateMode,
    pub prune: Option<PruneDecision>,              // step 0's retained dry run
    pub dbcrashratio: u32,
}
pub trait StateDelta: Send + 'static {
    fn write_b1(&self, batch: &mut impl KvBatch, cfs: &ChainstateCfs);   // evo CFs + evo_tip, block order
    fn bytes(&self) -> u64;                                               // Core's m_evoDb.GetMemoryUsage analogue
}
pub struct EffectLedger { seq: AtomicU64 }   // V bumps after a presumed effect's write returns; CH7a classifies
impl EffectLedger { pub fn record_presumed(&self) -> u64; pub fn current(&self) -> u64; }

pub enum FlushStateMode { None, IfNeeded, Periodic, Always }         // Core's FlushStateMode
bitflags! { pub struct FlushCauses: u16 { const ALWAYS; const CACHE_LARGE; const CACHE_CRITICAL; const EVO_CRITICAL;
    const PERIODIC_FLUSH; const PRUNE; const THRESHOLD; const DMAX; const LEFT_IBD; const SCRIPTED;
    const EMPTY; } }   // the empty flush for a pending FlushDue (§4.4.1 rule 5); rev3 removes rev2's RULE_O (DEC-95)
pub enum FlushAction { Nothing, IndexOnly, Freeze { causes: FlushCauses, prune: Option<PruneDecision>, wait_done: bool },
                       WaitThenFreeze { causes: FlushCauses }, Pending { causes: FlushCauses } }
pub struct FlushPolicy { last_write: Option<Instant>, last_flush: Option<Instant>, left_ibd_done: bool, pending: FlushCauses }
impl FlushPolicy { pub fn decide(&mut self, mode: FlushStateMode, ctx: &mut PolicyCtx<'_>) -> FlushAction; }
pub struct PolicyCtx<'a> { pub now: Instant, pub usage: Usage, pub b_t: u64, pub evo_bytes: u64, pub in_flight: bool,
    pub prune: &'a mut dyn PruneHost, pub hub: &'a SubscriberHub, pub idx: &'a dyn IndexAccess, pub new: BlockRef,
    pub manual_prune_height: Option<u32>, pub dmax_due: bool, pub left_ibd: bool, pub is_ibd: bool }

pub struct FlushMachine<E: KvEngine, F: FlatFs, D: StateDelta> { /* packet, step, d1, selection, cleared, cursor, report */ }
pub enum FlushStep { Space, Files, PresumedBarrier, RewindGate, B1, Durable, Subscribers, PruneSelect,
                     SubBatch(u32), PreBkBarrier, Bk, Release, Unlink, Done }
pub enum Step { Progress(FlushStep), Wait(WaitFor), Done(FlushReport) }
pub enum WaitFor { Rewound { flush: u64, pending: Vec<SubscriberId> }, Acks { flush: u64, pending: Vec<SubscriberId> },
                   PruneClears { files: Vec<u32> } }
pub enum Delivery { Ack { flush: u64, from: SubscriberId, locator: BlockRef }, Cleared { records: Vec<IndexWrite> } }
impl<E: KvEngine, F: FlatFs, D: StateDelta> FlushMachine<E, F, D> {
    pub fn new(packet: FreezePacket<D>, cfg: &FlushConfig) -> Self;
    pub fn step(&mut self, env: &mut FlushEnv<'_, E, F>) -> Result<Step, Abort>;   // one store-call group per step
    pub fn deliver(&mut self, d: Delivery);
}
pub struct IndexOnlyWrite { pub index: Vec<IndexWrite> }      // Core's fPeriodicWrite: files barrier + one Durable batch
pub struct FlushEnv<'a, E: KvEngine, F: FlatFs> { pub engine: &'a GuardedEngine<E>, pub files: &'a GuardedFiles<F>,
    pub hub: &'a SubscriberHub, pub obs: &'a dyn FlushObserver, pub cfg: &'a FlushConfig }

pub trait FlushDriver { fn submit(&self, m: FlushMachine</*…*/>); fn poll(&self) -> Option<DriverEvent>; }
pub struct ThreadDriver;   // production: W loops step(); Wait(..) blocks on W's channel; also wakes on the latch
pub struct InlineDriver;   // the seam (ST3a §5.6 item 2): the caller steps W, V and subscribers on one thread

pub trait PruneHost {      // over ST2's BlockFiles; CH8 adds configuration
    fn check_due(&mut self) -> bool;                                    // take_check_for_pruning(); manual heights separate
    fn dry_run(&self, new: &BlockRef, bound: u32, manual: Option<u32>, is_ibd: bool) -> PruneDryRun;   // NO side effects
    fn eligible(&self, file: u32, bound: u32) -> bool;                  // nHeightLast ≤ bound ∧ not the current file
    fn apply(&mut self, files: &[u32]);                                 // prune_one_block_file each
    fn rearm(&mut self);
}
pub struct PruneDryRun { pub files: Vec<u32>, pub met_target: bool }
pub struct PruneDecision { pub d: Vec<u32>, pub met_target: bool, pub automatic: bool, pub manual: Option<u32>,
                           pub predicted: Vec<(&'static str, u32)> }
```

**An ST2 addition (CH2b, reviewed by ST2's owner).** ST2's `find_files_to_prune[_manual]` call `prune_one_block_file` on each file they select (`flatfile/blocks.rs:580-660`), so they cannot serve as the side-effect-free dry run that ST3a §5.6 item 7 requires. CH2b therefore splits each one into:
- `plan_files_to_prune[_manual](&self, …) -> PruneDryRun` (pure, including `met_target`), and
- the existing apply loop, kept as the composition of the two, so ST2's tests stay green.

Neither input design caught this (§8 row 33).

**The observer** (ST3a §5.6 item 4). Production code does not depend on `dashd-store::faulty`. CH2 emits its own marks, and the seam adapter forwards them to `World::mark`.

```rust
pub trait FlushObserver: Send + Sync { fn mark(&self, m: ObsMark); }
pub enum ObsMark { FlushBegin { flush: u64, old: Option<BlockHash>, new: BlockHash }, Durable { flush: u64 },
    Locator { store: &'static str, tip: BlockHash, kind: LocatorKind }, PruneDecision { d: Vec<u32>, met_target: bool,
    flush_for_prune: bool, causes: FlushCauses }, PruneSelected { files: Vec<u32>, rearmed: bool },
    SubscriberState { store: &'static str, class: SubscriberClass }, Log(String), BestEffortBegin, BestEffortEnd,
    RecoveryBegin, RecoveryEnd, StoreFailed { op: &'static str }, Abort { message: String } }
```

`Connected { applied }`, `ConnectBegin` and `ConnectEnd` are CH3a's marks, emitted through the same observer.

### 2.8 The recovery API (CH2b)

**E-b1.3:** `open_chainstate` is two calls around CH1-node's step 4: `ChainstateOpen::open` (steps 0–3) and `Opened::recover` (steps 5–10).

```rust
pub struct RecoveredChainstate { pub tip: BlockRef, pub replayed: Vec<BlockHash>, pub meta: Meta }
pub fn open_chainstate<E: KvEngine, F: FlatFs>(engine: &GuardedEngine<E>, files: &GuardedFiles<F>,
    index: &dyn IndexAccess, hub: &SubscriberHub, obs: &dyn FlushObserver, cfg: &RecoveryConfig,
) -> Result<RecoveredChainstate, InitError>;
pub struct Recovery<'a, E: KvEngine, F: FlatFs> { /* step machine */ }
pub enum RecoveryStep { Barrier, Checked(MarkerState), Undo(BlockRef), Forward(BlockRef), SubBatch(u32), PreFinal, Final, Done }
impl<'a, E: KvEngine, F: FlatFs> Recovery<'a, E, F> {
    pub fn step(&mut self) -> Result<RecoveryStep, InitError>;   // the seam drives it; open_chainstate loops it
    pub fn replayed(&self) -> &[BlockHash];                       // ST3a R6(b): old → fork → new
}
pub enum InitError { UnableToReplay { detail: &'static str }, EvoInconsistent, Corruption(String), Store(StoreError) }
```

### 2.9 Subscribers (CH2b; rewritten in rev2)

rev2 rewrites this section. The rev1 shape stays (driver-owned locator rules, a journal, blind deletes above L, reads only below L). rev2 adds:
- **visibility** of rows at the moment `best_block()` advances (O-R2-M1);
- a **monotone resource high-water mark** (O-R2-M2);
- a **keyed** row class with a disconnect staging path, for IX2 (O-R2-M3);
- archives written at the disconnect (O-R2-m6);
- a journal bounded in bytes and a **per-subscriber `D_max`** (O-R2-M4, m10);
- the **sequence-number** case test (O-R2-M6);
- the **capped eager commit** on disconnect (manager's D13 ruling).

The rev2 addendum (DEC-81) states what the driver needs from its event source as a `SubscriberFeed` (§2.9.1), so it no longer depends on CH1's concrete bus. DEC-82 removes every per-profile switch: subscribers behave as v24's indexes do.

```rust
pub struct SubscriberId(u16);
pub enum SubscriberClass { Synced, Syncing }                     // Core's m_synced (index/base.cpp:90, 169)
pub enum LocatorKind { CommitStep, Checkpoint, Cadence, Rewind, EagerRewind }
pub struct BlockNeeds { pub block: bool, pub undo: bool }        // coinstatsindex and IX2: undo

/// Every ChainEvent carries `seq`, assigned by V at the tip change, before any backpressure wait (§4.4.1 rule 1).
pub struct FlushCommit { pub flush: u64, pub new: BlockRef, pub tip_seq: u64, pub to: Vec<SubscriberId> }
pub struct RewindRequest { pub flush: u64, pub to: BlockRef, pub targets: Vec<SubscriberId> }

/// W's side (rev1 API, unchanged except publish_durable's tip_seq).
pub struct SubscriberHub { /* Mutex<HubState> */ }
impl SubscriberHub {
    pub fn register(&self, name: &'static str, needs: BlockNeeds, mandatory: bool, start_height: u32) -> SubscriberHandle;
    pub fn deregister(&self, id: SubscriberId);
    pub fn t_ack(&self) -> Option<BlockRef>;
    pub fn commit_cap(&self) -> Option<BlockRef>;               // T_ack, lowered to LCA(T_ack, new) from step 3b to step 5 (b3 D33)
    pub fn durable_locator(&self, id: SubscriberId) -> Option<BlockRef>;
    pub fn open_rewind_gate(&self, flush: u64, to: &BlockRef) -> Vec<SubscriberId>;
    pub fn publish_durable(&self, new: &BlockRef, tip_seq: u64) -> Vec<SubscriberId>;
    pub fn outstanding(&self, id: SubscriberId) -> Option<Outstanding>;
    pub fn ack(&self, id: SubscriberId, flush: u64, locator: BlockRef);
    pub fn predicted_locks(&self, new: &BlockRef, idx: &dyn IndexAccess) -> Vec<(&'static str, i64)>;
    pub fn actual_locks(&self, new: &BlockRef, idx: &dyn IndexAccess) -> Vec<(&'static str, i64)>;
    pub fn on_disconnect_tip(&self, height: u32) -> Vec<(&'static str, u32)>;
}

/// Row classes, declared once. The driver writes every row; the data object only computes them.
pub struct RowSchema {
    /// Keys start with BE height. One row per block on the indexed chain, written per block (Buffered) as processed,
    /// so the row is visible when best_block() reports the block. Blind-deleted by range above L.
    pub height_cfs: &'static [Cf],
    /// Hash-keyed copies for blocks that left the indexed chain (Core's CopyHeightIndexToHashIndex). Written at the
    /// disconnect, after `sync_files`. A monotone set: never deleted, values a function of the block and its ancestry.
    pub archive_cfs: &'static [Cf],
    /// Fixed keys, written ONLY in locator batches (coinstatsindex DB_MUHASH; blockfilterindex DB_FILTER_POS).
    pub state_keys: &'static [(Cf, &'static [u8])],
    /// rev2 (IX2): keys that are not height-prefixed, mutated per block and undone on disconnect (address history and
    /// unspent, spent, timestamp). Written ONLY in locator batches; rows for (L, p] live in the journal and are served
    /// through `SubscriberDriver::read`.
    pub keyed_cfs: &'static [Cf],
    /// DEC-77 Q6 (txindex): idempotent overwrites, never deleted. Written per block (Buffered) as processed.
    pub inexact_cfs: &'static [Cf],
    /// The subscriber's own flat files (blockfilterindex fltr?????.dat), with a monotone write position.
    pub has_files: bool,
}
pub enum Row { Put(Cf, Vec<u8>, Vec<u8>), Delete(Cf, Vec<u8>) }   // keyed rows may delete; the other classes only put
pub struct BlockRows { pub block: BlockRef, pub rows: Vec<Row> }  // one block's height, keyed and inexact rows

pub trait SubscriberData {
    type E: KvEngine;
    /// Core's locator-committed state (coinstatsindex: MuHash accumulator and totals; blockfilterindex: next file
    /// position), plus any resource high-water mark. `()` for none.
    type State: Clone + Send;
    fn name(&self) -> &'static str;
    fn needs(&self) -> BlockNeeds;
    fn start_height(&self) -> u32;
    fn schema(&self) -> &'static RowSchema;
    fn initial_state(&self) -> Self::State;
    /// Pure: b's rows and the state at b, from the state at b's parent (Core's CustomAppend / WriteBlock).
    fn connect(&self, b: &ConnectedBlock<'_>, at_parent: &Self::State) -> Result<(BlockRows, Self::State), SubscriberError>;
    /// rev2 (O-R2-M3): rev0's stage_disconnected, restored. The rows that undo b's keyed rows and the state at b's parent,
    /// computed from b, its undo, the state at b and reads through `below` (barrier-covered rows at heights <= h(L)
    /// only). This is Core's CustomRewind / RewindBatch / ReverseBlock (addressindex.cpp:144-161, 280-395 v24;
    /// coinstatsindex.cpp:403). Never reads a key above L.
    fn stage_disconnected(&self, b: &DisconnectedBlock<'_>, at_b: &Self::State, below: &DurableBelow<'_, Self::E>)
        -> Result<(Vec<Row>, Self::State), SubscriberError>;
    /// Core's CopyHeightIndexToHashIndex (coinstatsindex.cpp:236-262; blockfilterindex.cpp:313-339).
    fn archive(&self, height_rows: &[Row]) -> Vec<Row>;
    /// Resource high-water marks are monotone for the whole process (O-R2-M2): any state the driver moves to keeps
    /// `latest`'s high-water. Default: `at.clone()` (no resources).
    fn with_high_water(&self, at: &Self::State, latest: &Self::State) -> Self::State { at.clone() }
    fn encode_state(&self, s: &Self::State) -> Vec<Row>;           // keys ⊆ schema().state_keys
    fn decode_state(&self, rows: &[(Cf, Vec<u8>, Option<Vec<u8>>)], at_l: &DurableBelow<'_, Self::E>)
        -> Result<Self::State, SubscriberError>;                     // n3: may read the height row at L (CustomInit)
    fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<(Cf, KeyRange)>;   // blind delete ranges
    /// rev4 (G-R4-n1): a bound on one journal entry's bytes for any consensus-valid block (a 2 MB block's rows).
    /// Entry bytes = the owned capacity of its keyed rows, archive sources and State, including container capacity
    /// (Vec/VecDeque slack), not just their lengths. Inexact-only subscribers keep no journal and return 0.
    fn max_entry_bytes(&self) -> u64;
    fn sync_files(&mut self) -> Result<(), SubscriberError> { Ok(()) }
    fn file_len(&self) -> Result<u64, SubscriberError> { Ok(0) }  // open: high-water floor (rule 3 below)
}

/// Barrier-covered reads only: rows at heights <= h(L). n4: a per-height prefix scan for archiving (F, L].
pub struct DurableBelow<'a, E: KvEngine> { engine: &'a GuardedEngine<E>, max_height: u32, schema: &'static RowSchema }
impl<'a, E: KvEngine> DurableBelow<'a, E> {
    pub fn height_row(&self, cf: Cf, height: u32, key_rest: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError>;
    pub fn height_rows(&self, cf: Cf, height: u32) -> Result<Vec<(Vec<u8>, Vec<u8>)>, SubscriberError>;   // prefix scan
}

pub struct SubscriberDriver<D: SubscriberData> {
    data: D, engine: Arc<GuardedEngine<D::E>>, handle: SubscriberHandle,
    durable: Option<BlockRef>, durable_state: D::State,     // L and the state at L
    journal: VecDeque<JournalEntry<D>>,                     // (L, p]: per block, keyed rows (if any), archive sources, state
    journal_bytes: u64, journal_budget: u64,                // m10: bounded in bytes (EngineTuning::journal_budget); rev3: byte
                                                             // admission, empty for inexact-only subscribers (item 2)
    latest_high_water: D::State,                            // O-R2-M2: monotone, process-wide
    d_max: Option<u32>,                                      // O-R2-M4: per subscriber; None = unbounded (rule 6 below)
    reported_best: Option<BlockRef>,                         // best_block() (MPIX-11)
    class: SubscriberClass, parked: bool, cadence: Duration,
    last_seq: u64, seq_synced: u64, skipped: BTreeSet<u64>, // §4.4.1 rule 1
}
struct JournalEntry<D: SubscriberData> { block: BlockRef, keyed: Vec<Row>, archive_src: Vec<Row>, state: D::State }
pub enum SubStep { Progress, Parked, Idle }
impl<D: SubscriberData> SubscriberDriver<D> {
    pub fn open(data: D, engine: Arc<GuardedEngine<D::E>>, hub: &SubscriberHub, g: &BlockRef, idx: &dyn IndexAccess,
                blocks: &dyn BlockSource) -> Result<(Self, CatchUp), SubscriberError>;
    pub fn step(&mut self, feed: &mut dyn SubscriberFeed, hub: &SubscriberHub, idx: &dyn IndexAccess,
                blocks: &dyn BlockSource) -> Result<SubStep, SubscriberError>;
    /// Syncing catch-up from a coherent `ChainSnapshot`; at the snapshot's tip it calls `FeedSource::subscribe_at`
    /// (F10, rev3): `SyncProgress::Live(feed)` on success, else it takes a fresh snapshot and continues.
    pub fn sync_step(&mut self, source: &dyn FeedSource, hub: &SubscriberHub, blocks: &dyn BlockSource)
                -> Result<SyncProgress, SubscriberError>;
    pub fn best_block(&self) -> Option<BlockRef>;          // MPIX-11; never ahead of the rows a query can read
    /// rev2 (O-R2-M1): THE read path for every index lookup (IX1, IX2). Keyed CFs read journal → DB; the other classes
    /// are already in the DB when best_block() reports their block.
    pub fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError>;
    pub fn iter_prefix(&self, cf: Cf, prefix: &[u8]) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), SubscriberError>> + '_;
    fn commit_locator(&mut self, x: BlockRef, kind: LocatorKind) -> Result<(), SubscriberError>;   // private
    fn rewind_commit(&mut self, fork: BlockRef, blocks: &dyn BlockSource) -> Result<(), SubscriberError>;  // private
}
```

**The locator-target state contract** (normative for every `SubscriberData`; rev1's items, revised):

1. **Writing per class.**
   - **Height and inexact rows** go into one Buffered batch per processed block, written as the block is processed. **`best_block()` advances only after that batch returns**, as Core's `CustomAppend` writes before `SetBestBlockIndex` (`index/base.cpp:294-300`). So a query sees every row of every block `best_block()` reports. `rpc_getblockfilter.py:37-41` (tip filters right after mining) and `getrawtransaction` with txindex need no flush. s = p always, so "submit through X" is trivially satisfied.
   - **Keyed rows** are written only in locator batches. Until then they live in the journal, and `read`/`iter_prefix` merge journal → DB (newest journal entry first). `best_block()` advances when the journal entry is pushed.
   - **State keys** are written only in locator batches, as rev1.
   - **Archive rows** are written at the disconnect (item 4).
2. **The journal** holds, for each block in `(L, p]` on p's branch: its keyed rows, the archive sources (its height rows, needed only if it is later disconnected), and the state after it.
   - **Inexact-only subscribers keep no journal** (rev3, G-R3-m1). A subscriber with only inexact rows (txindex; `llmq/` as LK1b declares it) has nothing to undo, archive or hold state for, so its progress is O(1): p, L and `best_block()`. LCA and path questions go to `IndexAccess`. rev2 kept a `BlockRef` per block, which Sol measured at ≥ 35.3 MiB over 1,028,159 blocks, past the 32 MiB budget.
   - **Exact subscribers admit by bytes** (G-R3-m1). `connect` is pure, so the driver computes b's entry before admitting it. The entry is admitted iff `journal_bytes + entry_bytes ≤ journal_budget` and, when `d_max` is set, `h(b) ≤ h(L) + d_max`. Otherwise the subscriber is **at the limit** (§4.4.1 rule 3: checkpoint in case B, else compress, else park), and the computed entry is discarded and recomputed later.
   - **An entry larger than the budget** is admitted only into an **empty** journal (p = L), and then alone. A checkpoint at p empties the journal first, so an oversized entry always completes. The journal is therefore bounded by `max(journal_budget, the largest single entry)`. Each `SubscriberData` declares `max_entry_bytes()`, a worst-entry bound from consensus limits (a 2 MB block's rows), and `engine-memory` reports it.
   - **What is counted** (rev4, G-R4-n1). `entry_bytes` is owned capacity: the rows' keys and values, the archive sources and the `State`, with their containers' capacity, not just their lengths. Admission bounds the **retained** journal only. An entry that is computed and then not admitted is transient memory until it is discarded. It does not count against the journal budget, but it is real process memory: it shows in `rss_anon_peak` and in `engine-memory`'s per-subscriber peak, which also reports the largest discarded entry.
   - The admission rule never consults the coins budget.
   - It is outside the coins budget and inside the subscriber's own memory, which `engine-memory` and W-MC report (m10).
3. **Monotone resources** (O-R2-M2). `latest_high_water` is the maximum of every resource high-water mark written so far in the process: for blockfilterindex, one past the last byte of the last filter written (Core's `m_next_filter_pos` only advances: `+=` at `blockfilterindex.cpp:309`, set otherwise only at init, `:160`).
   - **Every** state the driver moves to is passed through `with_high_water(at, latest)`: the state after a disconnect above L, the rewind commit's state at F, the gate's rewind, and every committed state. So no write ever reuses bytes a height or archive row points at. That matches Core, whose `CustomRewind` writes the *current* position (`:356`).
   - At open, the high-water is `max(durable state's, data.file_len())`. Bytes past the durable mark are never overwritten either; no durable row references them unless synced (item 4).
4. **A disconnect of p = b with `h(b) > h(L)`:**
   - pop b's journal entry;
   - the new state is `with_high_water(prev, latest)`;
   - keyed rows of b were never written, so the journal pop undoes them;
   - b's height row stays in the DB, where Core also leaves it: lookups check the stored hash, as Core's `LookupOne` does (`blockfilterindex.cpp:362-378`). The next connect at that height overwrites it, and recovery blind-deletes it.
   - **Lookups must try the archive when the height row is absent** (rev3, Sol's spot check). Core returns "not found" if the height row is missing (`:366-368`), because Core never deletes height rows. CH2 does delete them (recovery's blind delete, the rewind commit), so a stale block above the current height can have an archive row and no height row. IX1's lookups read the height row, then on absence **or** a hash mismatch read the hash-keyed archive. C8(c) reads stale filters back after a blind delete.
   - Archive rows `archive(b's height rows)`, taken from the journal, are written **at once**, in a Buffered batch **after `sync_files()`**. So an archive row never references unsynced filter bytes, and it survives a process kill as Core's does (O-R2-m6).
   - The **capped eager commit** follows (item 5b).
5. **Commits.**
   - **(a) A commit at X** (commit step, checkpoint, cadence, case A, eager) with X on p's path and `h(L) ≤ h(X)`, in order:
     1. all per-block batches through p have returned;
     2. `sync_files()`;
     3. `sync_wal()`;
     4. one Durable batch: the keyed rows of journal entries ≤ X, in order, `encode_state(with_high_water(state_at(X), latest))`, and the locator X;
     5. journal entries ≤ X are dropped once it returns.
   - **(b) The capped eager commit** (manager's ruling on D13; §3.9 has no D13 any more):
     - After each disconnect of a block on the indexed chain above L, commit at `X = LCA(pprev, commit_cap)` iff X strictly descends from L. This is Core's `BlockDisconnected` → `Rewind` → `Commit` (`index/base.cpp:308-330`, `:238-259`), capped so that X is never ahead of G (R11a).
     - `getindexinfo` after a crash matches Core whenever Core's own locator (`pprev` or the fork) was not ahead of its durable chainstate. Otherwise Core's locator is ahead of its chainstate and Core is not synced either.
     - **Prune locks (DEC-64, Q10):** the eager commit only ever **raises** L (X strictly descends from L), so every lock `height(LCA(L, new)) − PRUNE_LOCK_BUFFER − 1` is ≥ its value without it. It cannot reintroduce the Q10 lag. Its cost is one Durable commit per disconnected block above L; reorgs are rare. An `invalidateblock` of depth k costs k fsyncs, which Core spends as k unsynced writes.
   - **(c) The rewind commit** (F a strict ancestor of L), one Durable batch, made at the first of: the next connect, a `RewindRequest`, or a `FlushCommit` (n5). G has not moved meanwhile, so the deferral is safe.
     1. `stage_disconnected` for each block of `(F, L]`, top down. It reads block and undo files (on L's path, which R11b keeps readable) and rows ≤ h(L) via `DurableBelow`.
     2. Archive rows for `(F, L]`, from `height_rows` (n4).
     3. `sync_files()`, then `sync_wal()`.
     4. The batch: blind `height_ranges(h(F), h(L) + D_range)` deletes, the keyed undo rows, the archive rows, `encode_state(with_high_water(state_at(F), latest))`, and the locator F.
     5. The journal is discarded.
     - `D_range` = `d_max`, or `EngineTuning::unbounded_delete_span()` when unbounded (§2.5.1).
6. **`D_max` per subscriber** (O-R2-M4; rev3, G-R3-m1).
   - A subscriber whose schema has **no height, keyed or state rows**, only inexact rows (txindex; `llmq/` as LK1b declares it), has **`d_max = None` and no journal** (item 2). It can never be at the limit, so it never posts `FlushDue(DMax)`.
   - Exact subscribers are limited by journal bytes (`EngineTuning::journal_budget`, 32 MiB under `RocksTuning`). They may also set a block-count `d_max`, which the model and the blind-delete span use.
   - The blind range delete covers any `d_max`. A finite `D_max` exists only to bound the journal and ST3a's model.
   - §5.9 has the DEC-43 arithmetic.
7. **Recovery (`open`):**
   1. Read the locator L and the state keys once, before the first write. `decode_state` may read the height row at L, as Core's coinstatsindex `CustomInit → LookUpOne` does (`coinstatsindex.cpp:350-389` v24).
   2. Blind-delete `height_ranges(h(L), h(L) + D_range)`.
   3. Set the high-water floor (item 3).
   4. Catch up L → `LCA(L, G)` → G. The disconnect leg is item 5c; the connect leg is item 1.

   After step 2 the store equals the reference at L for height, keyed and state rows. Keyed rows above L were never written; height rows above L are gone. Archive and inexact rows are monotone sets with correct values (§10 Q5's R11c erratum). That is R11c(i).
8. **IX2.** Address, spent and timestamp indexes are keyed subscribers. Their `stage_disconnected` mirrors `AddressIndex::CustomRewind` and the spent and timestamp rewinds. The FTs that call `invalidateblock` must return Core's answers: `feature_addressindex.py:221-224`, `feature_spentindex.py:152` and `feature_timestampindex.py:56-57`.

**MPIX-11**: `best_block()` moves to `pprev` on each disconnect, as v24's eager rewind does. Durably, item 5b follows Core, capped at `commit_cap`.

**Deregistration** (rev1, unchanged). `commit_locator` and `rewind_commit` stay private. `debug_assert!(is_ancestor_or_eq(x, s))` holds before every `sync_wal`.

#### 2.9.1 What a subscriber needs from its event source (`SubscriberFeed`; rev2 addendum, DEC-81)

DEC-81 makes SPV and the node one stack; SB0 designs the shared base, and CH1's `ChainEvent` bus may be replaced by it. CH2b's driver therefore depends on the guarantees below and on two query traits, never on a producer type. CH1's bus is one producer that meets them.

```rust
/// CH2's own types; a producer constructs them.
pub enum ChainEvent { Connected { block: BlockRef, seq: u64 }, Disconnected { block: BlockRef, seq: u64 } }
pub enum Control { FlushCommit(FlushCommit), RewindRequest(RewindRequest), ChainStateFlushed { flush: u64, new: BlockRef } }

/// One per Synced subscriber. Never blocks the caller.
pub trait SubscriberFeed: Send {
    fn front(&self) -> Option<&ChainEvent>;
    fn pop(&mut self) -> Option<ChainEvent>;
    /// Compression (§4.4.1 rule 3): remove a head Connected(b) and a later Disconnected(b); returns both seqs.
    fn remove_round_trip(&mut self, b: &BlockHash) -> Option<(u64, u64)>;
    /// The parked subscriber's waker: called on every enqueue to this feed and on every Control addressed to it (F5).
    fn set_waker(&mut self, w: SubWaker);
}
/// Read-only chain queries from subscriber threads, without V's lock (F8): `IndexAccess` (§2.11) and, for Syncing
/// catch-up, a COHERENT snapshot: tip(), tip_seq() and at_height() describe one state of V's active chain (rev3).
pub trait ChainSnapshot { fn tip(&self) -> BlockRef; fn tip_seq(&self) -> u64; fn at_height(&self, h: u32) -> Option<BlockRef>; }

/// The producer side a Syncing driver uses (rev3, G-R3-M4; F10).
pub trait FeedSource: Send + Sync {
    fn snapshot(&self) -> Box<dyn ChainSnapshot + '_>;
    /// The catch-up-to-live handoff. Runs under the lock V holds while it assigns a `seq` and changes its tip, so it is
    /// linearized with every tip change. If V's tip is `p`: calls `finish()` (the driver's final commit at
    /// LCA(p, commit_cap); Core's Commit under cs_main), installs this subscriber's feed starting at
    /// `seq_synced` = the next seq V will assign, and returns it. Otherwise installs nothing and returns `Moved`.
    fn subscribe_at(&self, id: SubscriberId, p: &BlockRef, finish: &mut dyn FnMut() -> Result<(), SubscriberError>)
        -> Result<Handoff, SubscriberError>;
}
pub enum Handoff { Live { feed: Box<dyn SubscriberFeed>, seq_synced: u64 }, Moved }
```

**The feed's guarantees** (§4.4.2's argument and the model assume these; rev3 adds F10 and sharpens F1, F2, F3 and F8):

Each row says who must provide it (rev5, O-R6-m7): **producer** rows are what SB0's event source must implement; **CH2** rows are obligations of CH2's own hub and W, listed so the argument is complete.

| # | Guarantee | Why the subscriber needs it |
|---|---|---|
| F1 | *producer.* **Ordering from the cut.** Per subscriber, the feed delivers **exactly** the tip changes with `seq ≥ seq_synced` (F10), in order, with no loss and no duplication. The first applies to V's tip at the cut, which equals p. Every `Disconnected(b)` is of the then-current tip, and every `Connected(b)` extends the previous one. Different subscribers may lag each other | the journal and p's path (§2.9 items 2, 4); Core's `ValidationInterface` order |
| F2 | *producer.* **`seq` at the tip change.** A global, strictly increasing `seq` is assigned when V changes the tip, **before** any backpressure wait to emit it. `FlushCommit.tip_seq` is the `seq` of the change that made `new` the tip. `seq_synced` is fixed by the handoff (F10) | the case test of §4.4.1 rule 1 (O-R2-M6) |
| F3 | *producer.* **Bounded per-subscriber queues** of Q data events; the bound applies only to data. A full queue makes V wait **in its service loop** (rule 5), never with `FlushDue` unserviced. V's wait ends when the event is in every target queue. The producer may enqueue to subscribers with room first (independent fanout) or to all at once when all have room (atomic fanout, what the model explores); independent fanout only delivers earlier, and the b3 threaded test covers it (n1) | Lemma 3 |
| F4 | *producer (out of band); CH2 (the hub's mirror).* **Control events out of band.** `FlushCommit`, `RewindRequest` and `ChainStateFlushed` bypass the bound, are mirrored in the hub, and are visible to the subscriber before any queued data | rule 1 reads the commit from the hub; Lemma 1 |
| F5 | *producer.* **Wake on enqueue.** A parked subscriber is woken by any data event enqueued to it and by any control event addressed to it | rule 3, X23 |
| F6 | *producer.* **Weak fairness.** Whatever threads serve subscribers (one shared thread or one each), a runnable subscriber is eventually stepped, and a parked one costs no thread | §4.4.2's assumption; X17 |
| F7 | *CH2 (W publishes after B1; the hub captures the set).* **The flush barrier.** `ChainStateFlushed(new)` and `T_ack` are published only after B1 returns durably; the Synced set a `FlushCommit` targets is captured at step 5 under the hub's lock | R11a; ST3a item 6 |
| F8 | *producer (snapshot, ancestry); CH2 (prune locks, R11b).* **Data and ancestry.** A Syncing driver's `ChainSnapshot` is coherent (one state of V's chain, with its `tip_seq`). The block (and its undo, if `BlockNeeds.undo`) of every delivered event is readable when delivered and stays readable along the locator path until the subscriber commits past it (R11b, prune locks); `IndexAccess::{ancestor, lca, height}` answer for every delivered block from subscriber threads | `connect`, `stage_disconnected`, the rewind commit, LCA tests |
| F9 | *CH2 (the driver).* **`best_block()` is the driver's, not the feed's.** No producer reports a subscriber's progress; RPC (`getindexinfo`, index lookups) reads `SubscriberDriver::best_block()`, which advances only after the block's rows are readable (item 1) and moves to `pprev` on a disconnect | O-R2-M1 visibility; MPIX-11 |
| F10 | *producer.* **The catch-up-to-live handoff** (rev3, G-R3-M4). `subscribe_at` is linearized with V's tip changes: no tip change can fall between its check `p == V's tip` and the feed's first event. Its cut is `seq_synced` = the next seq V will assign. **An event already assigned but still pending emission at the cut (seq < seq_synced) is not delivered** to this subscriber: its change is already in p. If V's tip has moved, nothing is installed, and the driver continues catch-up from a fresh snapshot, rewinding off a stale branch as Core's `ThreadSync` does (`index/base.cpp:172-176`). The final commit runs inside the operation. This is Core's `NextSyncBlock` test, final `Commit` and `m_synced = true` under `cs_main` (`index/base.cpp:163-170`) | Sol's G-R3-M4 race: a snapshot ending at A3, a reorg to B3 before the transition, then a feed from the new cut leaves p on A3 for ever |
| F11 | *producer.* **Deregistration is final** (rev5, O-R6-m7). Once `deregister` returns, the producer enqueues nothing more to that subscriber, and V's backpressure never waits on its queue | §4.2's shutdown order (subscribers deregister, then the final flush runs without them) |

**Events that do not apply to p** (rev3; Sol's note on `b ≠ p`). Under F1 and F10, a Synced driver sees `Connected(b)` with `parent(b) ≠ p`, or `Disconnected(b)` with `b ≠ p`, only for blocks descending from a connect it dropped (case B's off-path drop) or compressed. It ignores those, as Core's warning branch does. It never sees one whose parent (for a connect) or which itself (for a disconnect) is a **strict ancestor of p**. Those are Core's post-handoff backlog cases: ignore if the block does not connect to an ancestor of the best block, else `Rewind` then append (`index/base.cpp:281-292`; disconnects `:319-329`). The cut excludes them, so the driver treats one as a producer contract violation: `SubscriberError::FeedOrder` trips the latch. The model asserts that it never happens (§4.4.3).

A producer that cannot give one of its rows (F1–F11) must say so in SB0, and CH2's argument has to be redone for it. §10.1 adds the storage and flush-side items SB0 must keep.

### 2.10 Errors, the latch, abort

```rust
#[derive(thiserror::Error, Debug)] pub enum CoinsError {
    #[error("Attempted to overwrite an unspent coin (when possible_overwrite is false)")] OverwriteUnspent,   // coins.cpp:82
    #[error("FRESH flag misapplied to coin that exists in parent cache")] FreshMisapplied,                    // coins.cpp:222
    #[error("coin value out of range")] ValueRange,
    #[error(transparent)] Read(#[from] CoinsReadError),
    #[error("database corrupted: {0}")] Corruption(String), }
pub struct CoinsReadError(StoreError);          // constructible only by GuardedEngine (latch already tripped)

/// One per node (in production, one per process), shared by every engine and the block files. A test World gets its own.
#[derive(Clone)] pub struct GlobalFailStop(Arc<OnceLock<Failure>>);
pub struct Failure { pub kind: ErrorKind, pub site: &'static str, pub message: String }
impl GlobalFailStop { pub fn trip(&self, f: Failure) -> bool; pub fn tripped(&self) -> Option<&Failure>;
                      pub fn check(&self) -> Result<(), Fatal>; pub fn wait_tripped(&self, timeout: Duration) -> bool; }
pub struct GuardedEngine<E> { inner: E, latch: GlobalFailStop, abort: Arc<dyn AbortSink>, name: &'static str }
pub struct GuardedSnapshot<'a, E: KvEngine + 'a> { engine: &'a GuardedEngine<E>, inner: E::Snapshot<'a>, epoch: u64 }  // rev2, O-R2-m9; a3: the write epoch it was taken at (§2.4 O5)
impl<E: KvEngine> KvEngine for GuardedEngine<E> { /* type Snapshot<'a> = GuardedSnapshot<'a, E>;
                                                     writes and barriers: check the latch, refuse ReadOnly, trip on error;
                                                     reads: trip + abort.read_failed() on error */ }
impl<E: KvEngine> GuardedEngine<E> {
    /// DEC-78: a stored value that fails to decode. Trips the latch (kind Corruption), calls
    /// abort.read_failed("corrupt coin record <cf/key>: <detail>") and returns the error. Core reads it as missing.
    pub(crate) fn corrupt(&self, cf: Cf, key: &[u8], detail: &str) -> CoinsReadError;
}
pub struct GuardedFiles<F: FlatFs> { inner: Mutex<BlockFiles<F>>, latch: GlobalFailStop }   // a3: no sink; a failed mutation returns its error and the caller (CH2b, CH5) calls Core's AbortNode
pub trait AbortSink: Send + Sync {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>);   // ND1's AbortNode: shutdown, exit status 1 (v24)
    fn read_failed(&self, what: &str);                              // production: Core's two strings, then process::abort()
}
```

- `OverwriteUnspent` and `FreshMisapplied` are Core's `logic_error`s. A thrown `logic_error` escapes `ConnectBlock` and terminates dashd, so ours are `Fatal` → abort. They are never a block verdict and never a panic (DEC-58).
- **E-b1.7:** `GlobalFailStop` gains a wake registry. `check`/`wait_tripped` land with b1's `ThreadDriver`, and every W, V and driver wait registers with the latch before it parks.
- ST1 already latches each engine on its own (`fail_stop.rs`), including RocksDB background errors that RocksDB would auto-resume after ENOSPC (ST1 §9.5 D4). The global latch extends that across stores, which is ST3a R8 across the chainstate, the subscribers and the flat files.

### 2.11 Neighbours

| Task | Dir. | Types exchanged | Requests |
|---|---|---|---|
| **ST1** `dashd-store` (RocksDB parts behind `EngineTuning`, §2.5.1) | up | `KvEngine`, `KvRead`, `KvBatch::size_bytes`, `batch_with_capacity`, `Commit`, `sync_wal`, `flush_memtables`, `snapshot`, `multi_get`, `approximate_size`, `ErrorKind`, `StoreError` | CH2a implements in `dashd-store`: (a) `KvBatch::delete_range` (BL-38, DEC-69 Q16); (b) `RocksConfig.write_buffer_manager: Option<WbmConfig { bytes, charge_to_cache }>`, per-CF write-buffer sizes; (c) set and assert `manual_wal_flush = false`. ST1's owner reviews |
| **ST2** block files | up | `BlockFiles<F>`, `sync_and_take_dirty_infos() -> DirtyFiles`, `take_check_for_pruning`, `prune_one_block_file`, `unlink_pruned_files`, `scan_and_unlink_already_pruned_files`, `calculate_current_usage`, `has_failed`, `BlockFileReader::{read_block, read_undo}`, `FlatFs::available_space`, `MIN_BLOCKS_TO_KEEP`, `Release` | CH2b adds `plan_files_to_prune[_manual]` (pure) and `eligible` (§2.7). CH6a adds `rewrite_undo_file` (§3.5.2). Later levers: a two-phase files barrier (L6); BL-33's streaming undo reader before replay reads large rev records |
| **ST3a** | up (test) | `World`, `FaultyEngine`, `FaultyFlatFiles`, `Sut`, `check_coins_profile`, `check_chainstate`, `RefExtractor`, `Scope`, `Mark`, `explore`/`sweep`/`minimize` | CH2b implements `Sut` twice and the production `RefExtractor` (§6.4) |
| **CS0** | up | `Block`, `Tx`, `TxOut`, `OutPoint`, `Tx::txid(rules)` (v24's stable asset-unlock txid), `CoreBytes` (v24 rules only after RP-V24, DEC-82) | — |
| **CS5a** | up | `coin_bytes`, `CoinRef<'a>` (borrowed decode), `CoinRef::decode_undo` (zero height = no metadata), `TxUndo`/`BlockUndo`/`decode_block_undo`, `is_unspendable`, `MemoryCoins`, `MAX_OUTPUTS_PER_BLOCK`, `access_by_txid` | Fallible `CoinsView` **adopted at 17014d4b** (DEC-77 Q1), generic over `type Error` with no concrete error in the pure crate (DEC-78); `CoreWrite` over a fixed `&mut [u8]` so the inline encode does not allocate |
| **CS5b** | peer | `check_tx_inputs`, sigops with coins, sequence locks over `V: CoinsView` | propagate `V::Error` as `ViewOr::View` |
| **CH1, or SB0's shared base** (DEC-81; since DEC-119, `dash-chain` (SB4, SB5) and CH1-node, E-b1.1) | up (interface) | `IndexAccess { lookup, ancestor, lca, height, have_data_pos, undo_pos, status }`, `take_dirty_index_records() -> Vec<IndexWrite>`, the `BlockIndexRecord` codec, a coherent `ChainSnapshot`, a **`FeedSource` with the atomic `subscribe_at`**, and a **`SubscriberFeed` producer meeting F1–F10** (§2.9.1). CH1's `ChainEvent` bus with one queue per subscriber is one such producer | CH2b is **held until SB0 is approved**; then add **i:** on whichever component SB0 assigns the producer (CH1 today) |
| **CH3a** | down | `BlockOverlay::{new, have_inputs, add, spend, commit}`, `CoinsView` impl, `CoinsReadError` → `Fatal`, `Connected { applied }` marks; the budget is checked after the block (§2.5, DEC-95) | the latch rule of §2.4.4; CH3a's live `ApplyTxInUndo` port keeps Core's legacy path; T12, C11's connect half and T20/T21's connect halves are CH3a's acceptance (Opus F5) |
| **CH3b** | down | `FlushPolicy::decide` at Core's call sites (§3.3), `ChainCommand::Flush(mode)`, V's service loop (§4.2) | — |
| **CH4** | down | `PrefetchBatch`, `admit_prefetch`, `CoinsTable::multi_get` on `E::Snapshot`, W's append queue drained at step 2 | xcheck per lookup |
| **CH5** | down | `wipe_chainstate`, `-dbcrashratio`, `open_chainstate` | runs `feature_dbcrash`, `feature_reindex`; legacy-undo normalisation in its regtest auto-import (§3.5.2) |
| **CH6a** (and CH6b) | down | ST2 `rewrite_undo_file` | legacy-undo normalisation at import by ancestry, into rewritten rdashd-owned rev files (§3.5.2; §7.4 row text) |
| **CH7a** | down | `StateDelta` (`EvoDelta`), `EffectLedger::record_presumed`, effect classification | effects stay Buffered single-row batches (A1 §3.5.7) |
| **CH8** | down | `PruneHost`, `PruneDecision`, locks from `SubscriberHub` | configuration, RPC, FTs; turns pruning on |
| **IX1/IX2, LK1b, WL** | down | `SubscriberData` (rev2 row classes incl. keyed, `State`, `connect`/`stage_disconnected`/`archive`/`with_high_water`), `SubscriberDriver` (`best_block`, `read`/`iter_prefix`), `SubscriberHub::{register, deregister}` | DEC-77 Q6 (txindex inexact); stale-block retention for blockfilterindex and coinstatsindex via `archive`, with lookups falling back to the archive when the height row is absent (§2.9 item 4, rev3) (`rpc_getblockfilter.py:43-47`, `feature_coinstatsindex.py:287-290` become CH2-dependent FTs); N8: v24's eager rewind changes when stale-block copies become visible, not the answers |
| **MP1** | down | ATMP's coins phase as `ChainCommand`s on V (ruling 9): `fetch`, `have_in_cache`, `uncache`; the PERIODIC check after ATMP (§2.5); `B(t)` reads mempool usage | Core holds `cs_main` across ATMP; `coins_to_uncache` bookkeeping at `validation.cpp:963-974`, the `Uncache` calls at `:1744, 1775` (n1) |
| **ND1 / RL2** | down | `AbortSink`, `CacheSizes` log lines, the shutdown flush skipped after an abort, exit status 1 after `AbortNode`, the shutdown order (§4.2) | RL2 documents CONS-22, the read-error abort and DEC-78's corrupt-coin abort |
| **O2a-CO** (new) | test | `oracle_coins_ops` | CH2c's last phase only (ruling 7; §7.4) |
| **PF0/PF1/PF3** | down | §5.7 workloads, the layout microbench, `f` tuning | — |

---

## 3. Core-equivalence

### 3.1 What CH2 replaces (v24.0.0-rc.3, DEC-82)

| Core | v24.0.0-rc.3 | Rust | Notes |
|---|---|---|---|
| `Coin`, `TxOutCompression`, VARINT | `coins.h:60-66`; `compressor.cpp:19-84, 149, 168` | CS5a `CoinRef`; `PackedCoin` | the cache value is Core's serialization |
| `CCoinsCacheEntry` DIRTY/FRESH, the legal states | `coins.h:97-102, 117-127` (both) | `Flags` (+ SPENT = tombstone) | §3.2 |
| `FetchCoin`, `GetCoin` | `coins.cpp:44-59, 61-68` (both) | `CoinsCache::fetch`, `BlockOverlay::coin` | F hits not copied (§3.2) |
| `AddCoin`, `AddCoins` | `coins.cpp:70-108, 118-127` | `BlockOverlay::add`; the loop is CH3a's | unspendable skipped (`:72`) |
| `SpendCoin` | `coins.cpp:129-150` | `BlockOverlay::spend` | FRESH erased, else tombstone |
| `AccessCoin`, `HaveCoin`, `HaveCoinInCache` | `coins.cpp:153-170` | `coin`, `have_coin`, `have_in_cache` | `have_in_cache`: active then F, never the DB (§2.4.5, DEC-102); equal to Core's unless a flush separates (D12) |
| `GetBestBlock`/`SetBestBlock` | `coins.cpp:172-180` | `best_block` | — |
| `BatchWrite` (cache → cache) | `coins.cpp:182-252` | `BlockOverlay::commit` | `erase = true` |
| `Flush` / `Sync` | `coins.cpp:254-282` | freeze + `FlushMachine`; `Sync` not ported (NG-6) | our freeze keeps serving F |
| `Uncache` | `coins.cpp:284-297` | `uncache` | clean entries only |
| `GetCacheSize`, `HaveInputs`, `ReallocateCache`, `SanityCheck` | `coins.cpp:299-301, 303-313, 315-323, 325-340` | `len`, `have_inputs`, dropping a layer, `sanity_check` (debug, xcheck) | allocated bytes, as Core counts pool chunks |
| `AccessByTxid` | `coins.cpp:344-353` | CS5a `access_by_txid` (CH3a's live path) | not used by recovery (§3.5.2) |
| `CCoinsViewErrorCatcher` | `coins.cpp:356-379` | `GuardedEngine` reads + `AbortSink::read_failed` | `std::abort()`; `HaveCoin` included (CONS-22) |
| `DynamicMemoryUsage` (pool) | `memusage.h:187-203`; `support/allocators/pool.h:187` (both) | `Usage` | 256 KiB chunks |
| `CCoinsViewDB::{GetBestBlock, GetHeadBlocks, BatchWrite, EstimateSize, Cursor}` | `txdb.cpp:98-111, 113-174, 176, 226-274` | `Meta`, the flush's sub-batches, `estimate_size`, `cursor` | marker moved into B1 (A1 §3.5.4) |
| `CBlockTreeDB::WriteBatchSync` | `txdb.cpp:276-286` | B1 and the index-only write | — |
| `GetCoinsCacheSizeState` | `validation.cpp:2945-2967` | `size_state` over `B(t)` | thresholds verbatim |
| `FlushStateToDisk`, `ForceFlushStateToDisk`, `PruneAndFlush` | `validation.cpp:2969-3120, 3122-3128, 3141-3148` | `FlushPolicy` + `FlushMachine` | §3.3, §3.4 |
| `ReplayBlocks`, `RollforwardBlock`, `ApplyTxInUndo` | `validation.cpp:5222-5305, 5180-5220, 2252-2278` | `Recovery` | §3.5 |
| `ResizeCoinsCaches` | `validation.cpp:5866` | `CoinsCache::resize` | grow → IF_NEEDED, shrink → ALWAYS (`:5889`, `:5892`); a change to the coins database's size alone → ALWAYS too (`Resized::DbOnly`, DEC-142 O9; b1 maps it) |
| `CalculateCacheSizes`, `Cache configuration` lines | `node/caches.cpp:15-42`; `init.cpp:1986-2023` | `CacheSizes::calculate` | with v24's index shares (§3.8) |
| `FlushBlockFile`, `FlushUndoFile`, `WriteBlockIndexDB`, `UnlinkPrunedFiles` | `node/blockstorage.cpp:585-605, 577-583, 333-352, 618-629` | ST2 barrier + B1; unlink after Bk | † (§3.4) |
| `BaseIndex::{Commit, CommitInternal/CustomCommit, ChainStateFlushed, Rewind, BlockDisconnected}`, `m_synced`, `SYNC_LOCATOR_WRITE_INTERVAL` | `index/base.cpp:218-236, 225, 332-369, 238-259, 308-330, 90, 169, 29, 202-207` | `SubscriberDriver`, `SubscriberHub` | the cadence is capped (T33); `BlockDisconnected` is v24 only (MPIX-11, §2.9) |
| `CoinStatsIndex`/`BlockFilterIndex` `CommitInternal`/`CustomCommit`, `Rewind`/`CustomRewind`, `CopyHeightIndexToHashIndex`, `ReverseBlock` | `index/coinstatsindex.cpp:394-399, 264-301, 236-262, 403`; `index/blockfilterindex.cpp:165-186, 341-359, 313-339` | `SubscriberData::{encode_state, with_high_water, archive, stage_disconnected}` (contract in §2.9; implementations are IX1's) | the state keys only in locator batches |
| `-dbbatchsize`, `-dbcrashratio` | `txdb.cpp:117-118, 156-163` | `FlushConfig` | §3.6 |
| `CheckDiskSpace` | `validation.cpp:3050, 3088`; floor 50 MiB `util/fs_helpers.cpp:109` | flush step 1 | — |

### 3.2 Cache semantics

**The equivalence argument** (Opus §3.2, adopted):
1. Freezing at tip `new` hands W every change since the previous durable tip `old`. F maps each key to its value at `new`, a tombstone, or a clean copy of the DB value.
2. While F is being written, the DB holds the state at `old`, plus B1, plus a subset of F's sub-batches. Every key in such a sub-batch is in F, and F is consulted first. **So "F over the DB" answers every lookup exactly as a DB at `new` would**: Core's DB right after a full flush at `new`.
3. Core's `Flush()` leaves its cache empty (`coins.cpp:254-265`). Our active layer starts empty at the freeze.
4. So active is Core's `CoinsTip()` right after a flush at `new`, over a base whose `GetCoin` returns only unspent coins, as `CCoinsViewDB` does. **Every `CCoinsViewCache` rule applies to active unchanged.** In particular no active entry is ever "spent, FRESH, not DIRTY": that state needs a parent that returns spent entries (`coins.cpp:52-56`).

This is an equivalence of **values and errors** (every lookup, every `logic_error`). Cache membership is not part of it: `have_in_cache` answers from active then F (§2.4.5, DEC-102), and differs from Core's only across differing flush points (D12).

Active therefore holds Core's four reachable tip states: unspent clean, unspent DIRTY, unspent DIRTY|FRESH, and spent DIRTY (the tombstone).

**Fable's alternative is rejected** (§8 row 5). It copies F's tombstone into active as `SPENT|FRESH`, which models F as a parent *cache*. That reaches the same DB outcome but adds a fifth state to active, and it contradicts point 4.

**The two defensive checks consult active, then F.** Core's cache holds everything since Core's last flush; ours since the last *durable* flush is active ∪ F.
- `add(op, _, possible_overwrite = false)` with `op` unspent in active **or** F → `OverwriteUnspent` (`coins.cpp:80-83`).
- `commit`: a child FRESH entry whose key is unspent in active **or** F → `FreshMisapplied` (`coins.cpp:217-223`).

Both are unreachable on valid chains:
- Non-coinbase outputs are added with `possible_overwrite = false` only after BIP30's `HaveCoin` check, or under BIP34 (quirks 7, 10).
- Coinbase outputs are added with `true` (`AddCoins`, `coins.cpp:118-127`).

**FRESH marking consults active only.** With `possible_overwrite = false`, `fresh = !(active entry is DIRTY)`. Where F holds a tombstone and active has nothing, we mark FRESH where Core's cache, still holding the tombstone, would not. The DB outcome is identical:
- if the coin is spent again, active drops it, and F's flush erased the key;
- otherwise active's put lands after F's erase, because flushes are strictly sequential.

The twin compares DB contents and lookups, not flags (D2).

**`commit(child)`**, for each DIRTY child entry in insertion order. Non-DIRTY entries are skipped (`coins.cpp:187-189`).

| Active entry | Child | Result | Core |
|---|---|---|---|
| none | FRESH and spent | dropped | `:191-214` |
| none | otherwise | check F for the FRESH case; insert DIRTY, plus FRESH if the child is FRESH | `:191-214` |
| unspent | FRESH | `FreshMisapplied` | `:217-223` |
| FRESH (unspent) | spent | entry removed | `:225-229` |
| any other | any other | coin := child's; flags \|= DIRTY (a FRESH bit is kept); SPENT follows the child | `:230-247` |

The overlay's own `add` marks FRESH without consulting the parent, exactly as Core's child does. Its `have_inputs` caches each input in the tip (active) only; the child keeps no copy and resolves reads through active (rev5, O-R6-m3; §2.5 "Ownership"). Core's child also copies the coin; ours does not, which changes memory, not values.

### 3.3 `FlushStateToDisk`: the decision and its triggers

`FlushPolicy::decide` runs on V at Core's call sites. That is the decision only: the freeze it decides follows the tip step (E-b1.2).
- `IF_NEEDED` after `DisconnectTip` and `ConnectTip` (`validation.cpp:3304, 3457`);
- `PERIODIC` at the end of each `ActivateBestChain` step (`:3851`) and after ATMP (`:1748, 1780`);
- `NONE` after `AcceptBlock` (`:4855`) and from `PruneAndFlush` (`:3141-3148`);
- `ALWAYS` from `ForceFlushStateToDisk` (shutdown, the RPC copy points of §2.6) and on a cache shrink. Core's `PeriodicStats` also calls it with `-statshost` (`init.cpp:921-928`); rdashd has no statsd client in CH2's scope, so that source does not exist here, and ND1 refuses `-statshost` until a task adds one (O-R6-M2's note).
- **Under `-dbcrashratio` > 0, every freeze is synchronous** (rev5, O-R6-M1, DEC-102; D14). See §4.6.

| Core flag (`validation.cpp:3036-3046`, identical) | Our cause | No F in flight | F in flight |
|---|---|---|---|
| `mode == ALWAYS` | `ALWAYS` | freeze; wait for completion | wait for release, then freeze and wait |
| `fCacheLarge` = PERIODIC ∧ Core's usage > max(0.9·B, B − 10 MiB) | `CACHE_LARGE`: the same mode and arithmetic on **native Σ** (DEC-99) | freeze | pending until release |
| `fCacheCritical` = IF_NEEDED ∧ Core's usage > B | `CACHE_CRITICAL`: the same mode and arithmetic on **native Σ** (DEC-99) | freeze | **V waits** for release, then freezes; if native Σ is still above B(t), V waits for that release too (DEC-407, CH2c D32) |
| `fEvoDbCacheCritical` = IF_NEEDED ∧ EvoDB memory ≥ `64 << 20` | `EVO_CRITICAL` (`StateDelta::bytes()` of active's window ≥ 64 MiB) | freeze | V waits, then freezes |
| `fPeriodicFlush` = PERIODIC ∧ now > last_flush + 24 h (`:106`) | `PERIODIC_FLUSH` | freeze | pending |
| `fFlushForPrune` (`:3020`) | `PRUNE` iff the dry run D ≠ ∅ (E20) | freeze with D | **automatic:** the decision is deferred and the check **stays armed** (Core clears it only when it selects, `:3017`), so it is decided afresh after release. **Manual** (`pruneblockchain`): `WaitThenFreeze` + `wait_done` (below) |
| `fPeriodicWrite` = PERIODIC ∧ now > last_write + 1 h (`:104`) | `IndexOnly` | index-only write | skipped (F's B1 covers the records dirty at its freeze; the next check covers the rest) |
| — (ours, A1 §3.4) | `THRESHOLD`: active ≥ f·B(t), or (fallback) a shard of active at its trigger (§2.3, rev4) | freeze | — (the limit rule applies) |
| — (ours, ST3a §5.6 item 6) | `DMAX`: a subscriber at its limit posted `FlushDue` | freeze | pending; F's B1 already advances `T_ack` |
| — (A1 §3.4) | `LEFT_IBD` | freeze once | pending |
| — (tests) | `SCRIPTED` | freeze | pending |

- **Timers.** `last_write` and `last_flush` are set to now on the first call, as Core avoids writing immediately after startup (`:3028-3034`). They advance at B1's return and at the freeze respectively. Neither pin randomises them, and neither do we.
- **DEC-95, DEC-99.** `CACHE_LARGE` and `CACHE_CRITICAL` apply Core's arithmetic (`core_size_arith`) to **native Σ** (`CoinsBudget::size_state`), only in these modes and at these call sites; nothing checks the budget inside an op (§2.5). **They are not Core's `fCacheLarge` and `fCacheCritical`**, because Core computes those on a different number, so they need not fire where Core's do (§2.5, "These are not Core's flush points"). Nothing evaluates Core's own flags (rev5, DEC-102). a2 tests the arithmetic with a boundary table on `core_size_arith` and the coin-side decision through `CoinsCache::boundary_state`; b1 tests the mapping onto modes in `FlushPolicy`. No test asserts that a physical decision equals Core's.
- **Σ ≤ B(t) after a freeze (DEC-407, CH2c D32).** Any freeze that leaves native Σ above B(t) waits for its release, whatever its cause: a layer that doubled with F in flight, frozen beside a new active at its floor, can otherwise end a block boundary above B(t). Below about 88 KiB of B at f = ½ (more with a reserve or a smaller f) the tables' minimum, 8 buckets in each of 64 shards, does that alone, so there every coins freeze is synchronous.
- **Flush timing differs (D1, D11).** We freeze at f·B or at the fallback's shard trigger, usually before native LARGE, so native LARGE and CRITICAL normally fire only while F is in flight. Because native Σ is not Core's usage, a flush need not land where Core's would. This is unobservable: the copy points force a flush, and only debug-log timing differs.
- **Disk space** (step 1, before any write):
  - `CheckDiskSpace(blocks/)`, the 50 MiB floor, for a full or index-only write;
  - `CheckDiskSpace(datadir, 48·2·2·entries(F))` for a full one. `entries(F)` counts every entry, clean included, as Core's `GetCacheSize()` does.
  - Either failure aborts with "Disk space is too low!" (`:3050-3051, 3088-3089`).
- **v24's periodic low-space check** ("Shutting down due to lack of disk space!", `init.cpp:1587` v24; CONS-22) belongs to ND1.
- **Manual prune is synchronous** (Opus F14). Core's `PruneBlockFilesManual` runs `FlushStateToDisk(NONE, height)` and returns only after the selection and unlink (`validation.cpp:4977-4980`). `pruneblockchain` then reads `GetFirstStoredBlock` for its reply (`rpc/blockchain.cpp:1128-1130`), and `feature_pruning` asserts it. Ours is `ChainCommand::Flush { mode: None, manual_prune_height }`:
  - if F is in flight, it waits for release (service loop);
  - it then decides (step 0), freezes with D, and **replies after step 12** (unlink and `sync_dir`) returns;
  - with D = ∅ and no other cause, it replies at once.

### 3.4 Order of operations and the declared † deviations

Core's order (`validation.cpp:2969-3120`):
1. prune selection;
2. `CheckDiskSpace(blocks)`;
3. `FlushBlockFile` (current pair only, `void` on failure);
4. `WriteBlockIndexDB` (sync);
5. `UnlinkPrunedFiles` (no directory sync);
6. `CheckDiskSpace(datadir)`;
7. `CoinsTip().Flush()`: the first batch writes H and erases B, then partial batches, then the final batch erases H and writes B, all unsynced (`txdb.cpp:113-174`);
8. `CommitRootTransaction` (unsynced, **after** coins: `:3092→3097`);
9. `ChainStateFlushed` (`:3114`).

V holds `cs_main` throughout.

| † | Ours | Core | Visible when |
|---|---|---|---|
| 1 | every blk/rev file written since its last sync is synced, plus the directory | the current pair only (`blockstorage.cpp:585-605`) | power loss with lagging undo (T6) |
| 2 | evo and the marker commit atomically with the index in B1 | coins, then EvoDB, separately | a kill between them: "Found EvoDB inconsistency" (T4, T24) |
| 3 | prune dry run at step 0; selection `D ∩ eligible` after the subscriber commits; clears in Bk/Bp; unlink after Bk | selection against the in-memory tip before the index write; unlink before coins | a crash after the index write in prune mode (T5, T13, T15, T16, T19, T30) |
| 4 | a failed file sync, write, `sync_wal` or batch aborts the flush and the node | `FlushBlockFile` returns `void` and the flush continues (`blockstorage.cpp:600` v24) | fsync failure (T9) |
| 5 | `ChainStateFlushed` after B1 (E16) | after EvoDB | timing only |
| 6 | B1, Bk, Bp, the index-only write and recovery's final batch are Durable behind `sync_wal` where a scope requires it | only the index write is synced | 1–3 extra WAL fsyncs per full flush (ST1 D2: 8–88 ms) |
| 7 | subscriber locators never ahead of the chainstate's durable tip | `ThreadSync` commits its progress (`index/base.cpp:202-207`) | power loss |
| 8 | both disk-space checks before any write | the datadir check after the index write | ordering only |

None of these changes a healthy node's RPC answers, the log lines an FT asserts, or P2P behaviour. Two of them close Core's own process-kill holes (ST3a §7.2); reporting those upstream is pasta's call (DEC-59, DEC-62).

### 3.5 `ReplayBlocks`

#### 3.5.1 Mapping

| Core (`validation.cpp:5222-5305`) | Ours (§4.3) |
|---|---|
| no heads → consistent; a size other than 2 → "ReplayBlocks(): unknown inconsistent state" (`:5232`) | no marker, or an empty one → the step-4a check; one hash, or three or more → the same text |
| a `head_blocks` value that does not decode: `CDBWrapper::Read` returns false (`dbwrapper.h:322-326`), so `GetHeadBlocks` finds no heads (`txdb.cpp:105-111`) and the chainstate is consistent (`:5230`) | a length that is no whole number of 32-byte hashes fails the open with Corruption ("Error opening block database"): **quirk 44, DEC-180**, a registered deviation on a corrupt store only. Core's reading can skip a replay the node needs |
| `pindexNew` unknown → "…reorganization to unknown block requested"; `pindexOld` unknown → "…reorganization from unknown block requested" (`:5244, 5251`) | the same texts; `new` must also have `HAVE_DATA` (I1) |
| EvoDB best block must equal `old` when DIP3 is active at `old`: "ReplayBlocks(DASH): " + `EvoDbInconsistencyMessage()` (`:5259`, `:1923-1929`) | `meta.evo_tip == new` (B1 wrote it); a failure logs Core's text |
| roll back with the full `DisconnectBlock`; UNCLEAN tolerated; FAILED → "ReplayBlocks(): DisconnectBlock failed at %d, hash=%s" (`:5277`) | blind undo of coins only. Structural failures (read, checksum, size mismatch, **a zero-height entry**, §3.5.2) log that text |
| `RollforwardBlock`: `ProcessSpecialTxsInBlock` (`:5196-5199`), `SpendCoin` per input, `AddCoins(…, check = true)` (`:5216`) | blind: erase inputs, put spendable outputs. Evo is untouched. IX2's indexes are subscribers, recovered through their own locators |
| when `old` is null, `pindexFork = nullptr`, so forward replay starts at height 1 (`:5289`) | the same; genesis outputs are never in the UTXO set |
| `cache.Flush()` unsynced; EvoDB committed afterwards (`node/chainstate.cpp:268`) | sub-batches, `sync_wal`, Durable final batch |
| any failure → "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." (`node/chainstate.cpp:252`) | the same init error |
| logs "Replaying blocks", "Rolling back %s (%i)", "Rolling forward %s (%i)"; progress "Replaying blocks…" (`:5236-5294`) | the same |
| `-dbcrashratio` applies through `BatchWrite`'s partial batches; `feature_dbcrash` asserts `crashed_on_restart > 0` (`feature_dbcrash.py:280`) | applies after each recovery sub-batch |

#### 3.5.2 Legacy (zero-height) undo: reachability, and the importer's job (ruling 2; rev1: GPT M4, Opus F12)

**The fact.** Before the per-txout model, an undo entry carried the height and coinbase flag only for the last spent output of a transaction. Every other entry stores `nHeight = 0`. `TxInUndoFormatter` still reads that form (`undo.h:22-49`). Core's `ApplyTxInUndo` resolves it from another unspent output of the same txid **in the current disconnect view**, so the answer follows the branch being disconnected, or the disconnect fails (`validation.cpp:2258-2269`).

**When Dash switched.** Dash adopted the per-txout model in **v0.12.2.2** (December 2017): `83ec12f1a0` is an ancestor of v0.12.2.2 and not of v0.12.2.1 (Opus r1 checked this with `merge-base --is-ancestor`).

**Reachability** (unchanged from rev0 and confirmed by both reviewers):
- rdashd never writes a zero-height entry.
- A full `-reindex` rewrites undo.
- Imported Core rev files from a datadir first synced by ≤ v0.12.2.1 keep legacy entries through `-reindex-chainstate`, which keeps the undo positions.
- So a marker transition whose disconnect leg crosses the legacy era can read them. An operator `invalidateblock` on an imported mainnet or testnet datadir is enough.

**The importer normalises at import** (CH6a; CH5's regtest auto-import and CH6b reuse the code). Recovery stays blind.

1. **Discovery.** While it reads and verifies every adopted rev record, the importer records each zero-height entry as (spending block S, tx index i, input index j, prevout txid, n). Candidates are keyed by txid **and** S, never by txid alone. The same normal transaction can be mined at A100 and B110 on two branches whose blocks and undo are both in the adopted files (GPT M4).
2. **Resolution by ancestry.**
   - One forward pass over the adopted blocks records every occurrence `(block, height, coinbase?)` of each candidate txid. Memory is bounded by the number of candidates.
   - For each entry, the importer picks the occurrence whose block is an ancestor of S, or S itself at an earlier transaction index (an in-block spend), using the adopted block index.
   - Exactly one occurrence ⇒ height and coinbase flag. This always holds on a consistent chain: Dash has no duplicate txids on one chain (`fEnforceBIP30 = true` in v24 `DisconnectBlock`, `validation.cpp:2321-2322`).
   - Zero occurrences (an ancestor's data missing) or more than one ⇒ **unresolvable**.
3. **Unresolvable.**
   - If S is on the imported active chain, **the import is refused**: "Core undo data for block <hash> lacks metadata that its ancestry cannot supply; import without the source's undo data with -reindex". The source datadir is then inconsistent with its own chain.
   - If S is on a stale branch, the importer **clears `HAVE_UNDO` and the undo position** for S in the import's index batch and lists S in the report.
   - A stale block's undo is needed only to disconnect it after it becomes active again. Its reconnection writes fresh undo then, because Core writes undo only when the position is null. After a full `-reindex`, Core has no undo for stale blocks at all.
4. **Never writing a source inode** (Opus F12). Our index mirrors Core's, where a block's undo lives in the rev file with the block's `nFile` (`FindUndoPos(…, pindex->GetBlockPos().nFile, …)`). So a normalised record must go into rev file `nFile`, and CH6a hard-links sealed source files. The importer therefore **rewrites every affected rev file into a new rdashd-owned inode**:
   1. Write `revNNNNN.dat.tmp` in rdashd's `blocks/` with every record of the source file in order, legacy ones normalised to the current format: per entry `VARINT(code) ‖ 0x00 ‖ TxOutCompression`, the 0x00 dummy that Core writes for every height > 0 (`undo.h:26-33`). Each record gets a new checksum (`undo_checksum`, ST2).
   2. Truncate or finalize per ST2's BL-29 rule; `sync_data`.
   3. `rename` over the hard link, replacing the directory entry only, then `sync_dir`.
   4. Only then commit the import's index batch with every undo position of that file updated (I1).

   The source inode is never opened for writing, so CH6a's "source files unchanged" and ST3a's R10 (LinkRule) hold. The blk file stays hard-linked. ST2 gains `BlockFiles::rewrite_undo_file(file, records) -> Vec<FlatFilePos>` for this; CH6a owns it, and ST2's owner reviews.
5. **Byte identity.** A normalised record equals the `CBlockUndo` that current Core's `ConnectBlock` writes for that block on its own branch. For active-chain blocks, that is what current Core has after a full `-reindex`. rev0 claimed this for every block; for stale blocks it does not apply (Opus F12), and those are cleared or normalised as in steps 2–3.
6. **The fixture** (DEC-77 Q3's gate, extended):
   - a regtest chain with the same normal transaction mined at A100 and at B110;
   - zero-height undo for descendants of both on their branches, with heights zeroed from real undo;
   - a stale block whose ancestry is withheld, which must be cleared;
   - an active-chain variant with a withheld ancestor, which must refuse.

   Each branch is made active in turn (`invalidateblock`). A crash during the deep disconnect's flush follows, then recovery, which must read only normalised records. The source files' hashes must be unchanged after import, activation, mining and restart.

**Recovery's rule** (unchanged). A zero-height entry is corruption. Recovery logs "ReplayBlocks(): DisconnectBlock failed at %d, hash=%s" and fails with "Unable to replay blocks…". It never calls `access_by_txid` and never reads a `coins` key.

CH3a's live `disconnect_block` keeps Core's `ApplyTxInUndo`, reads included. It is not recovery, so ST3a's per-image bound does not apply. A debug assertion records that, after normalisation, the legacy path is never taken.

### 3.6 Strings and logs

| String | Core (v24) | Where in CH2 |
|---|---|---|
| "Disk space is too low!" (log and user) | `validation.cpp:3051, 3089` | step 1 |
| "Flushing block file to disk failed. This is likely the result of an I/O error." / "Flushing undo file to disk failed. …" | `node/blockstorage.cpp:600, 581` | step 2 (ST2's error, mapped by file kind) |
| "System error while flushing: %s" through `AbortNode` | `validation.cpp:3117`; `AbortNode` `shutdown.cpp:27-35` | **every** engine error in a flush step. What the user sees (Opus F6): the log line `*** System error while flushing: <engine text>` (`shutdown.cpp:30`); the **generic** user message "A fatal internal error occurred, see debug.log for details" (`:32`), via `InitError` + `EXIT_FAILURE` (`:34-35`); and `SetMiscWarning(msg)` (`:29`), so the text appears in `getblockchaininfo`/`getnetworkinfo` `warnings` until shutdown. "Disk space is too low!" passes its own user message |
| "Failed to write to block index database", "Failed to write to coin database", "Failed to commit EvoDB" | `:3066, 3093, 3098` | **not emitted.** Core's DB writes throw from `HandleError` before these can return false (`dbwrapper.cpp:261-268`: "Fatal LevelDB error: …"), so what is logged and put in `warnings` is "System error while flushing: …" (the user dialog is the generic message above). Ours matches, with the engine's own inner text (§8 row 22; D10) |
| `HandleError`'s two lines: "Fatal LevelDB error: <status>", then "You can use -debug=leveldb to get more complete diagnostic messages" | `dbwrapper.cpp:261-268` | logged at the failing engine call, before any timer line the error unwinds through and before `AbortNode`; none for block-file failures, the disk-space checks, or a call the latch refused (an earlier failure already reported). The first line carries the engine's text (D10). Added by BL-485 (DEC-832) |
| "Error reading from database: %s" + "Error reading from database, shutting down." | `coins.cpp:364`; `init.cpp:2089` | `AbortSink::read_failed`, then `abort()` |
| "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate." | `node/chainstate.cpp:252` | `open_chainstate` |
| "Found EvoDB inconsistency, you must reindex to continue" | `validation.cpp:1928` | the no-marker corruption check |
| "Simulating a crash. Goodbye." then `_Exit(0)` | `txdb.cpp:159-160` | after each partial sub-batch, in flushes and in recovery; never after B1 or a final batch |
| coindb: "Writing partial batch of %.2f MiB", "Writing final batch of %.2f MiB", "Committed %u changed transaction outputs (out of %u) to coin database..." | `txdb.cpp:153, 170, 172` | W's coin phase; `count` counts every entry of F, clean included, as Core's loop does |
| "Cache size (%s) exceeds total space (%s)" | `validation.cpp:2961` | when native Σ > B(t) at each decision that finds CRITICAL (DEC-99; debug-log timing, D1). V also decides at its own polls and again after a CRITICAL wait, so the line can repeat where Core's logs once |
| "FlushStateToDisk: write evodb cache to disk started" / "… completed (%.2fs)" | `validation.cpp:3096` | **not emitted until CH7a** (D17), which emits it around B1's durable write, before the coin batches (CH7 D20) |
| "`<name>` limited pruning to height %d" / "`<name>` prune lock moved back to %d" | `:3006, 3299` | step 0 (predicted locks) / `SubscriberHub::on_disconnect_tip` (log-only emulation) |
| "Cache configuration:", "* Using %.1f MiB for …" | `init.cpp:1986-2023` | ND1 prints `CacheSizes` |
| "[%s] resized coinsdb cache to %.1f MiB", "[%s] resized coinstip cache to %.1f MiB" | v24 startup only, via `MaybeRebalanceCaches` (`node/chainstate.cpp:293`; lines at `validation.cpp:5879-5881`) | printed at startup, log only (§3.8) |
| `AbortNode` exit status | 1 | ND1 |

### 3.7 R4 quirks touched

- **33:** unspendable outputs never enter the UTXO set; the cache and replay both skip them. The `hash_serialized_2` VARINT bug belongs to CS5b/IX1; CH2's cursor yields Core's key order for it.
- **28:** a missing rotation snapshot silently changes member selection (`llmq/utils.cpp:496-503` at v24). That is why snapshot effects are **presumed** and covered by the pre-B1 barrier (§4.1).
- **26:** special transactions never see same-block outputs, because the overlay starts empty.
- **10, 7:** BIP34/BIP30 make the two defensive checks unreachable. v24's `DisconnectBlock` has `fEnforceBIP30 = true` ("dash blockchain doesn't have any violations", `validation.cpp:2321-2322` v24).

### 3.8 v24 only (DEC-82)

rev0–rev2 kept a v23.1.8 column here, selected by `IndexRules`, `InitRules` and `ChainstateRules`. DEC-82 drops the v23.1 profile: CH2 consults no rules struct, and RP-V24 removes the types. The behaviours that used to differ are now v24's, unconditionally:

| Behaviour (v24) | Core | Where in CH2 |
|---|---|---|
| Address, timestamp and spent index shares are carved out before the coins share, with their `Cache configuration` lines | `node/caches.cpp:25-30`; `init.cpp:1992-1998` | `CacheSizes::calculate` |
| Startup sizing: 20 % first, then `MaybeRebalanceCaches` | `node/chainstate.cpp:225, 293` | **log-only**: rdashd sizes once and prints the two "resized" lines; the IF_NEEDED flush is a no-op at startup |
| A coins read error in `HaveCoin` aborts | CONS-22 | §2.4.4 |
| `AbortNode` exit status 1 | P2P-31 | ND1 |
| Index rewind on disconnect: `BlockDisconnected` → `Rewind` → `Commit` | `index/base.cpp:308-330`, `:238-259` | `best_block()` moves to `pprev`; the capped eager commit (§2.9 item 5b) |
| Replay's EvoDB-mismatch text is `EvoDbInconsistencyMessage()` | `validation.cpp:5259`, `:1923-1929` | §3.5.1 |
| `GetCoinsCacheSizeState` reads the mempool's maximum | `validation.cpp:2952-2953` | `CoinsBudget::total` |
| `RollforwardBlock` writes no address, spent or timestamp index rows | `validation.cpp:5180-5220` | IX2's subscribers |
| rev-file finalize rule (BL-29), startup prune scan (CONS-28) | `node/blockstorage.cpp:412` | ST2 |

The model's v23-timing subset (rev2, 40 configurations, all pass) is dropped with the profile; it tested the lazy commit, which no longer exists.

### 3.9 Declared deviations (none observable to peers or RPC, except D12, an allowed difference under DEC-102)

| # | Deviation | Why it is unobservable |
|---|---|---|
| D1 | flush timing and frequency (f·B or the shard trigger, asynchronous, on native Σ: D11) | copy points force a flush; only debug-log timing differs |
| D2 | FRESH can be set where Core's cache would hold a tombstone | the DB outcome is identical; the twin compares contents |
| D3 | the defensive checks consult F | unreachable on valid chains |
| D4 | all of §3.4's † | visible only after a crash or a disk fault; strictly more durable (US-NODE-04) |
| D5 | replay is coins-only; evo is committed at `new` in B1 | the recovered state is identical |
| D6 | no XOR obfuscation | own format (DEC-11) |
| D7 | **a stored coin that fails to decode aborts** (`GuardedEngine::corrupt`: latch, Core's read-error log and user strings, `abort()`). Core's `CDBWrapper::Read` catches the decode failure and returns false (`dbwrapper.h:324-326`), so it treats the coin as **missing** and can reach a verdict such as `bad-txns-inputs-missingorspent`; Core's `Read` also has no end-of-stream check, so a value with **trailing bytes** reads as a present coin; ours requires the decode to consume the whole value and treats trailing bytes as Corruption too (n7). Core's cursor stops silently (`txdb.cpp:226-274`). Kept per **DEC-78**: fail-safe on local corruption | only on a corrupted coins DB, where Core may reject a valid block or transaction and we stop. Tests: §7.1 a3 (`corrupt_coin_aborts`), planted X21, FA7 |
| D8 | unlinked pruned files are directory-synced | durability only |
| D9 | imported legacy undo is normalised; recovery refuses a zero-height entry | identical outcome on every consistent chain; more robust on an inconsistent one |
| D10 | the "System error while flushing" inner text is the engine's, not LevelDB's ("Fatal LevelDB error: …") | operator-visible only under a DB fault (CONS-22 class I): in debug.log and in RPC `warnings` through `SetMiscWarning` (Opus F6) |
| D11 | **the physical flush policy reads CH2's native ledger Σ** (its own allocations for active, F, prefetch and live undo copies, its layout growth, and R) where Core reads `CoinsTip().DynamicMemoryUsage()` (`validation.cpp:2951`). Core's arithmetic, modes and call sites are kept (DEC-95, DEC-99; rev4 rewrites this row) | flush timing only (D1): on the same blocks our LARGE or CRITICAL can fire earlier than Core's (F, prefetch, R, growth) or later (density). Its one Core-visible effect is D12 |
| D12 | **`have_in_cache` answers from our layers (active then F), not from Core's flush epochs** (rev5, **DEC-102**; rev2's shadow is removed). It equals Core's `HaveCoinInCache` whenever no flush (Core's) or release (ours) separates the coin entering the cache from the query | **an allowed difference, not unobservable:** across differing flush points, ATMP's reason for a transaction with a missing input can be `txn-already-known` where Core says `bad-txns-inputs-missingorspent`, or the reverse, and for a P2P transaction that decides whether it goes to the orphan path and its parents are requested (`net_processing.cpp:4963`). Both reject; no consensus, block or mempool content depends on it. Listed for compat ALLOWED-DIFFS (§7.4) |
| ~~D13~~ | **withdrawn in rev2** (the manager's ruling): the capped eager commit at `LCA(pprev, T_ack)` (§2.9 item 5b) makes `getindexinfo` after a crash match Core's whenever Core's own locator was not ahead of its chainstate | — |
| D14 | **test hook: under `-dbcrashratio` > 0 every freeze is synchronous** (rev5, O-R6-M1, DEC-102). V waits for `FlushDone` at the freeze's call site, so no block is connected while a flush that can roll the crash is in flight (§4.6) | `-dbcrashratio` is a debug-only option (Core: `txdb.cpp:118`). Production flushes stay asynchronous; ST3a's C6 and C9 remain the evidence for the V-ahead crash states |
| D15 | **an engine error during a coins cursor scan, or from `estimate_size`, aborts** (a3 fix r2, Opus O3). `CoinsTable`'s cursor and `GuardedSnapshot::scan` route it through the latch and `read_failed` (D7's path). Core's `CCoinsViewErrorCatcher` wraps only `GetCoin`/`HaveCoin` (`coins.cpp:355-379`). A LevelDB iterator error makes `Valid()` false, so `CCoinsViewDBCursor::Next` ends the scan (`txdb.cpp:265-274`), and `ComputeUTXOStats` returns success with the partial totals (`kernel/coinstats.cpp:125-143`). Core's `EstimateSize` cannot fail (`dbwrapper.h:394-407`). Kept per **DEC-78**: fail-safe on local corruption, like D7 | only on a damaged coins DB (a checksum mismatch or an I/O error mid-scan). There, Core's `gettxoutsetinfo` returns wrong totals and ours stops the node from the RPC. `estimate_size` fails only on an engine fault |
| D16 | **Core's `ChainStateFlushed` validation signal lags, and never leads** (E-b1.4, **DEC-166**). V hands it to the signals port when it observes W's release of the flush, after B1 has returned Durable, with the locator `at_site` captured (the pre-step tip's). Core signals inside its synchronous `FlushStateToDisk`, before the step's `BlockDisconnected`/`BlockConnected` (`validation.cpp:3114`). Ours can reach the queue after later `BlockConnected` events | the wallet's saved locator can lag what the wallet has processed, so a restart rescans a few more blocks from `findLocatorFork`. It never names a block above the one Core's would: `new` or an ancestor of it, or, at a disconnect site, the disconnected block, as in Core. The CRASH suite asserts this |
| D17 | **no "write evodb cache to disk" timer lines yet** (Opus high m3). Core times its EvoDB commit with `LOG_TIME_SECONDS("write evodb cache to disk")` in every full flush at a best block, after the coins write (`validation.cpp:3095-3100`). Always on, it prints "FlushStateToDisk: write evodb cache to disk started" and "… completed (%.2fs)". b1's window has no evo state (`NoEvo`; the `StateDelta` is `()`), so there is no commit to time, and a line would time nothing. **Deferred to the EV flush integration:** CH7a's `EvoDelta`, which B1 writes (§2.7, §2.11), emits the pair around that write (§7.4's CH7a edit). Core prints it after the coins' timer pair. Ours will come with B1, before the coins' sub-batches (D5) | log only: until CH7a, debug.log lacks these two lines on a full flush, and after it their order differs. No peer, RPC or durability effect |

---

## 4. Persistence, crash consistency and recovery

The protocol is ST3a §6.2's toy order (A1 §3.5 plus errata E1–E22) on real components. §4.7 maps each item of ST3a §5.6 to this design.

### 4.1 Barriers, finalizers and scopes

| Commit | Writer | Kind | Finalizes (ST3a §2.4.4) | Required before it |
|---|---|---|---|---|
| effect row (rotation snapshot, …) | V | Buffered, single row | — | — |
| **B1** | W | **Durable** | presumed effects that returned before the freeze; the previous chainstate finalizer (`PreviousFinalizer`) | files barrier (I1); `sync_wal` iff `packet.presumed_effects_seq > covered_seq` |
| coin sub-batch Bj | W | Buffered | — | B1 returned |
| **Bk** | W | **Durable** | this flush's sub-batches (`SinceFlushStart`) | every Bj returned, then a `sync_wal` started and returned |
| **Bp** (one-batch flush that prunes) | W | **Durable** | — (B1 carried the coins) | the second files barrier |
| index-only write | W | Durable | — | files barrier |
| recovery sub-batch | recovery | Buffered | — | the open-time `sync_wal` |
| **recovery final** | recovery | **Durable** | recovery's sub-batches | `sync_wal` |
| subscriber data | subscriber | Buffered | — | — |
| **locator** (commit step, checkpoint, cadence, rewind) | subscriber | **Durable**, all four (as the toy; ST3a §2.4.4 allows Buffered forward locators, kept as PF3 lever L10) | the subscriber's data through X, all **submitted**; cross-store: X not ahead of G | submit staged through X and wait; `sync_wal`; then the locator; B1 returned (commit step, checkpoint) or the cap at `T_ack` (cadence) |
| wipe flag set / clear | CH5 via CH2 | Durable | — | — |

- **The presumed-effect ledger.** V writes a presumed effect synchronously and bumps `EffectLedger.seq` once the write has returned. The packet captures the seq at the freeze, so every effect for blocks up to `new` was written before the freeze, on V's thread. W's pre-B1 `sync_wal` starts after the freeze, so it covers them ("returned before the barrier started", §2.4.1). W records `covered_seq` only after the barrier returns. Effects written after the freeze belong to blocks above `new`; the next B1 presumes them.
- **The previous finalizer.** Flushes are strictly sequential (one F). Bk or Bp returns Durable before W reports the flush done, and recovery's final batch returns before V starts. So B1's `PreviousFinalizer` scope is always covered at no cost, and T34 cannot arise.
- **WAL order** is the engine's serialization order, never call order (item 11). Every rule above is stated as "returned before the barrier started" (E18).

### 4.2 The flush, step by step

**V at the decision** (§3.3), at a block boundary:
- **Step 0, the prune decision.** If `PruneHost::check_due()` or a manual height says a check is due:
  1. Run `dry_run` against `new` under `SubscriberHub::predicted_locks`. Synced subscribers count at `new`, syncing ones at `height(LCA(durable, new))`, and a subscriber with no locator at its start height.
  2. Retain D and `met_target`. `flush_for_prune ⇔ D ≠ ∅`, as `fFlushForPrune` (`:3020`).
  3. Log "limited pruning to height X" from the predicted locks (`:2992-3006`).
  4. Clear the automatic check (`:3017`).
  5. Emit `PruneDecision`.
  A full flush happens iff `flush_for_prune` or another cause is due (FA1). An empty D never suppresses another cause.
- **Freeze:** `F = mem::take(active)`; take the window's `StateDelta` (CH7a); take every dirty `block_index` record (CH1); capture the effect seq; set `old` to the durable tip and `new` to V's tip.
  - The `HAVE_DATA`/`HAVE_UNDO` bits in those records were set after their appends returned, so step 2's barrier covers their bytes (I1).
  - Building the packet costs one pointer swap plus the index records (≈ 150 B per block in the window).

**W**, one `FlushMachine::step` per row:

| Step | Store calls | Mark / log | On error |
|---|---|---|---|
| begin | — | `FlushBegin { old, new }` | — |
| 1 Space | `available_space(blocks/) ≥ 50 MiB`; for a full flush `available_space(datadir) ≥ 50 MiB + 192·entries(F)` | — | abort "Disk space is too low!" before any write |
| 2 Files | lock `BlockFiles`; drain W's append queue through `new` (CH4; empty before CH4); `sync_and_take_dirty_infos()` → `d1` (`fdatasync` of every file written; `fsync(blocks/)` if one was created, truncated or removed); unlock | bench "write block and undo data to disk" | the flat-file string; no retry (fsyncgate) |
| 3 Presumed barrier | `sync_wal()` iff `seq > covered_seq` | — | "System error while flushing: …" |
| 3b Rewind gate (rev1) | only if some registered mandatory subscriber's durable L strictly descends from `new` (V froze at a fork below it, e.g. after `invalidateblock`; L may be on the old durable chain or on a stale branch): `hub.open_rewind_gate(new)` (the commit cap becomes `new`; `RewindRequest` to every registered mandatory subscriber whose durable L strictly descends from `new`); `Wait(Rewound)` until none does (§4.4.1 rule 4) | `SubscriberState` | a subscriber error trips the latch |
| 4 **B1** | one `batch_with_capacity` batch: index records, `d1`'s infos, `meta.last_file`, `delta.write_b1`, `meta.evo_tip = new`; then **one-batch** (all of F's coin ops, `meta.tip = new`, no marker) when `F.dirty_bytes + rest ≤ S`, **else split** (`meta.head_blocks = [new, old]`, erase `meta.tip`); `write(Durable)` | bench "write block index to disk" | as step 3 |
| 5 Durable | `hub.publish_durable(new, tip_seq)`: `T_ack := new` (closing any rewind gate); under the hub lock, capture the **registered** mandatory subscribers that are **Synced now**; enqueue `ChainStateFlushed { locator(new) }` and `FlushCommit { flush, new, tip_seq, to }` as control events (outside the queue bound, mirrored in the hub). These are the subscriber events of b3, sent through W's channel (H2). Core's validation signal is V's (E-b1.4) | `Durable { flush }` | — |
| 6 Subscribers | `Wait(Acks)` until every subscriber captured at step 5 has acked; each services the commit out of band (§4.4.1 rule 1). One that turns Synced later is not awaited; it makes its own capped commit as it turns (S11s, T35). In split mode W may write step-8 sub-batches while it waits (interleave, §8 row 30) | `Locator` (from the subscribers) | a subscriber error trips the latch, and W wakes and stops |
| 7 Prune select | only if `flush_for_prune`: actual bound `min(new − K, manual, min over mandatory subscribers of height(LCA(durable locator, new)) − PRUNE_LOCK_BUFFER − 1)` (`:122`), computed in i64 and clamped to ≥ 1 as Core does (`validation.cpp:2999`); selection `D ∩ eligible(actual)`, a subset of D by construction (FA2, T39); `Wait(PruneClears)`: V clears `HAVE_DATA`/`HAVE_UNDO` and positions in memory and returns the cleared records; `apply(selection)`; re-arm iff the target is unmet while D had met it (T40) | `PruneSelected { files, rearmed }` | — |
| 8 Sub-batches (split) | over F's DIRTY entries in layer order: tombstone → `erase`, else `put(key, bytes)`; close a sub-batch when `size_bytes() > S` (Core's `SizeEstimate() > batch_size`, `txdb.cpp:152`); write Buffered; roll `-dbcrashratio`. The last partial chunk is kept for Bk | "Writing partial batch of %.2f MiB" | as step 3; `_Exit(0)` on the roll |
| 9 Pre-Bk barrier (split) | `sync_wal()`, started after every Bj returned | — | as step 3 |
| 10 **Bk / Bp** | **Bk** (split): the last coins; the **second** `sync_and_take_dirty_infos()` → `d2` (pre-prune resets plus any synced V-ahead appends); the cleared records; `meta.pruned_files` if anything was ever pruned; `meta.tip = new`; erase `head_blocks`; `write(Durable)`. **Bp** (one-batch with prune): clears, `d2`, `pruned_files`. Neither (one-batch, no prune): nothing | "Writing final batch…", "Committed %u changed transaction outputs (out of %u)…"; bench "write coins cache to disk (%d coins, %.2fkB)" | as step 3 |
| 11 Release | **drop W's `Arc<FrozenLayer>` first**, then send `FlushDone { gen }` to V (V asserts it holds the last reference, §2.5); `EngineTuning::after_bk` (`flush_memtables()` under `RocksTuning`, §2.5.1) | — | as step 3 |
| 12 Unlink | `unlink_pruned_files(selection)` in a `BestEffort` region; `sync_dir(blocks/)` | `BestEffortBegin/End`; bench "unlink pruned files" | logged; left for the startup scan (CONS-28) |
| — Index-only (its own machine) | Space(blocks/) → files barrier → one Durable batch of index records, file infos and `last_file` | — | as above |

**V's side.**
- **Release.** At its next poll after `FlushDone`, V asserts `Arc::strong_count == 1`, drops F, increments `released_gen`, clears `prefetch` and, if a freeze is pending, freezes at once.
- **Native Σ is checked only at Core's call sites** (§2.5, DEC-95, DEC-99): after every connect and disconnect (IF_NEEDED), after ATMP and each `ActivateBestChain` step (PERIODIC), and in ALWAYS. Never inside an op; rev2's rule O is removed. These decisions use our counter and need not match Core's; nothing models Core's (DEC-102).
- **Under `-dbcrashratio` > 0, V waits for each flush it starts** (D14, §4.6), as it does for ALWAYS. Otherwise flushes stay asynchronous.
- **Every V wait is a service loop.** This covers the CRITICAL wait for release, the ALWAYS wait and the event-queue backpressure wait. (V's wait for the `BlockFiles` mutex is a plain lock held by W only for I/O, and W needs nothing from V while it holds it; see §4.4.) In each loop V answers:
  - W's `PruneClears` requests;
  - subscribers' `FlushDue(DMax)`, **by freezing** if no F is in flight (V is at a block boundary);
  - read-only `ChainCommand::Query`s.
  Mutating commands are queued until the wait ends. Every wait also wakes on the latch and on shutdown. **V's idle loop at the tip also wakes on `FlushDue`** (Opus N6).
- **ALWAYS** (shutdown, copy points): wait for any in-flight F, then freeze the rest and wait, so the DB is at V's tip on return. If active **has no DIRTY entry** and the tip equals `meta.tip`, it runs the files barrier, an index-only write and `ChainStateFlushed`, as Core's empty-cache full flush does. Clean prefetch copies do not count (Opus F15). On regtest it then runs `flush_memtables` and `sync_wal` (A1 §2.9).
- **Shutdown order** (ND1, mirroring Core; Opus F15):
  1. an ALWAYS flush with the subscribers running (`init.cpp:383`);
  2. drain the event queues (Core's `FlushBackgroundCallbacks`, `:390`);
  3. stop the subscribers: each makes its final commit and calls `SubscriberHub::deregister` (Core's index `Stop()`s, `:421-441`);
  4. a second ALWAYS flush (`:453`). It captures no subscriber, because all are deregistered, so step 6 waits on nothing and the gate targets nobody.
- **Manual prune** replies after step 12 (§3.3).

**Concurrency rules** (normative):
1. V is the only mutator of active, the overlay, prefetch, the block index and effect rows.
2. W is the only writer of B1, Bj, Bk/Bp and index-only batches. Recovery runs before V starts.
3. F is immutable after the freeze.
4. `BlockFiles` sits behind a mutex. V appends; W's barrier holds the mutex for `sync_and_take_dirty_infos`, so V's appends wait during that `fdatasync`. W needs nothing from V while it holds the mutex. ST2's two-phase barrier is lever L6.
5. W never counts V's concurrent writes as covered. Coverage is always by an explicit returned-before-started order (E18).
6. Under the seam (`InlineDriver`): every store call runs on the test thread, the salt is fixed, CH4's producer is off, and parallel sub-batch writes (L4) are off.
7. SB4 §13.4's H1–H3, H6 and H7 hold (E-b1.5). Every wait in W, V and the drivers registers with the latch before it parks (E-b1.7).

### 4.3 Recovery, step by step (`open_chainstate`; rev1 reorders it, GPT m2, Opus N4/N5)

| # | Action | Notes |
|---|---|---|
| 0 | open the engine through `GuardedEngine`; **`sync_wal`** before any write (E21) | every subscriber engine does the same in its own open |
| 1 | read the **decision keys** once: `meta.{tip, head_blocks, evo_tip, last_file, pruned_files, reindexing, wiping}` | the only keys recovery reads that it may later write (`tip`, `head_blocks`), read before its first write, as in ST3a's toy. No replay op reads a key |
| 2 | **`wiping` set ⇒ resume `wipe_chainstate`** (steps 2–3 of §2.6) and stop: the node continues with CH5's `-reindex-chainstate` | before any marker check, so an interrupted wipe never reads as `Corruption` (N5) |
| 3 | open `BlockFiles` from the durable `block_files` and `meta.last_file`, never from file lengths | ST2 |
| 4 | load the block index (CH1-node, between `open` and `recover`, E-b1.3; PoW against the stored hash) | — |
| 5 | **G** := the marker's `new`, else `meta.tip`. Checks: **no marker:** `tip` absent ⇒ the chainstate must be empty but for the effect CFs (`llmq_mined`, `llmq_mined_h`, `llmq_snapshot`), else `Corruption` (**DEC-202**: no durable tip is a logically empty store; effect rows a kill before the first flush left are kept, as above a tip, and the connects from height 1 rewrite them; with a marker whose `old` is null recovery leaves them too); present ⇒ `evo_tip == tip`, else "Found EvoDB inconsistency, you must reindex to continue". **Marker `[new, old]`:** `new` in the index with `HAVE_DATA`; `old` null or in the index; `evo_tip == new`; any failure logs Core's specific text (§3.5.1) and returns "Unable to replay blocks…" | — |
| 6 | **register prune locks** from every mandatory subscriber's durable locator (read once from its store), evaluated at `height(LCA(locator, G))`; no locator ⇒ its start height | before anything in the epoch can prune: the startup scan below only unlinks files already marked pruned; activation flushes and R12's flush come later (ST3a §5.5 step 0, T28) |
| 7 | run the startup scan if `meta.pruned_files` (a `BestEffort` region) | CONS-28 |
| 8 | **marker present:** `RecoveryBegin`; "Replaying blocks"; `fork = LCA(old, new)`, or none when `old` is null (then the forward leg starts at height 1). **8a undo leg**, `old` down to `fork + 1`: read the block and undo (checksum checked); check `vtxundo.size() + 1 == vtx.size()` and each tx's `vprevout` count; **any zero-height entry ⇒ corruption** (§3.5.2). Per tx in reverse, `erase` each spendable output's key and `put` each restored input coin in reverse input order; "Rolling back %s (%i)". **8b forward leg**, `fork + 1` up to `new`: per tx in order, if not coinbase `erase` every input, then `put` every spendable output as `coin_bytes(out, height, coinbase)` keyed by `Tx::txid(rules(height))`; "Rolling forward %s (%i)". **8c** ops go into sub-batches of S, each written Buffered with the marker still present and followed by the `-dbcrashratio` roll; `replayed.push(b)` | blind; an interruption leaves the marker, so recovery re-runs (I4) |
| 9 | `sync_wal`; final Durable batch: `meta.tip = new`, erase `head_blocks`; `RecoveryEnd` | E4, E21; nothing else is written; the index is never touched (R5) |
| 10 | `T_ack :=` G. Each subscriber's `SubscriberDriver::open` (§2.9 item 6): blind rewind of `(L, L + D_max]`, then catch-up L → `LCA(L, G)` → G | §4.4 |
| 11 | hand the tip to CH3b: `LoadChainTip` (`validation.cpp:4992`), `VerifyDB`, `activate_best_chain` through the ordinary `connect_block` (I3) | — |

**Why replay converges (I2, I4; restated for Opus N1).** Let K be the keys the net path `old → fork → new` touches. For **every** key k, F's value for k is k's value at `new`. Keys F touched that are off the net path (a coin created by a block connected and then disconnected inside the window, or an input restored the same way) have equal values at `old` and `new`, so F's write and the DB's value at `old` agree.

Before step 8 the DB is the state at `old`, plus B1, plus any subset of this flush's Bj, plus any subset of an earlier interrupted recovery's sub-batches. Every surviving write therefore holds k's value at `new`, or k's unchanged value at `old` for keys outside K. Replay writes every key in K, and its **last** write to each is that key's value at `new`; the op order is Core's disconnect and connect order, and the later op wins within a batch. So whatever subset survived, the state after step 9 is the state at `new`, and a re-run repeats the same ops.

No replay op reads a key, so ST3a's per-image bound stays polynomial (item 13). `coins_replay_blind` generates connect-then-disconnect windows to exercise the off-path case.

### 4.4 Subscribers in the threaded system, and why it cannot deadlock (ruling 8; rewritten in rev1, revised in rev2)

ST3a §5.6 item 6 defines the rules over a single-threaded toy. In the node the toy's queue is a `SubscriberFeed` (§2.9.1): ordered `ChainEvent`s per subscriber, whichever component produces them (CH1's bus, or SB0's shared base; DEC-81). The argument below uses only F1–F10.

**Sequence numbers** (rev2, O-R2-M6). Every event carries a global `seq` that V assigns **at the tip change, before any backpressure wait** for the event's emission. `FreezePacket` carries `tip_seq`, the `seq` of the event that made `new` V's tip (n2). `FlushCommit` carries it too.

`SubscriberDriver` implements every rule, and §2.9 has the state contract. Classes are unchanged:
- Synced iff the locator is the tip at init (`index/base.cpp:90`);
- Synced through the atomic catch-up-to-live handoff, F10 (`:163-170`; rev3, G-R3-M4);
- never inferred from "has processed `new`" (T35).

A queued `Connected(b)` that does not connect to p, or a `Disconnected(b)` with b ≠ p, is handled as §2.9.1's "Events that do not apply to p" says (rev4, G-R4-m1):
- **Ignored** when b descends from a connect this subscriber dropped (case B's off-path drop) or compressed. This is the lineage F1 and F10 allow, and Core's synced index ignores it the same way (`index/base.cpp:281-287`).
- **Fatal** when the connect's parent, or the disconnected block itself, is a **strict ancestor of p**. The cut excludes such events, so one arriving is a producer contract violation: `SubscriberError::FeedOrder` trips the latch.

#### 4.4.1 The rules

1. **Servicing an outstanding `FlushCommit { new, tip_seq }`** (GPT B1; O-R2-M6). A subscriber checks for one before every event and on every wake. The commit is read from the hub; **the queued copy is always a no-op when dequeued, before or after the ack**.
   - **The case test is by sequence number:**
     - **Case B** iff `tip_seq ≥ seq_synced ∧ tip_seq > last_seq ∧ tip_seq ∉ skipped`. That is, the tip event is still ahead of this subscriber: queued, or **still pending emission in V's backpressure wait**.
     - `seq_synced` is **F10's cut**: the next `seq` V will assign, fixed by `subscribe_at` under the tip lock (rev4, G-R4-m1; rev3's "the pending event's `seq`" was the X28 convention and is withdrawn). An event already assigned but still pending emission at the handoff has `seq < seq_synced`. It is never delivered to this subscriber, because its change is already in p.
     - **At the handoff** the driver sets `last_seq = seq_synced − 1` and clears `skipped`, so the case test can never wait for a `seq` outside the subscription. `liveness_model_v3.py:303` does the same.
     - `last_seq` is the `seq` of the last event processed, dropped or ignored.
     - `skipped` holds the `seq`s that compression removed ahead of `last_seq`.
     - A `FlushCommit` whose `tip_seq` is below the cut is therefore case A (`tip_seq ≤ last_seq`): its tip change was already in p when the subscriber turned Synced.
     - rev1's test, "`Connected(new)` is in the queue", misread a pending emission as "already processed". Opus's stepwise variant reaches that deadlock (planted X22).
   - **Case A** (the tip event was processed, or lies before this subscriber's cut and so is already in p): let `X = LCA(p, new)`.
     - If X is on L's path at or above L, commit at X (a no-op if X = L) and ack at once.
     - If X is strictly below L, process the queue first; its rewind commit lowers L.
     - With no disconnect queued or pending, ack at L: L is on a branch other than `new`'s.
   - **Case B: drain toward `new`.**
     - A queued `Connected(b)` with b not an ancestor of `new` is dropped.
     - Disconnects are processed.
     - At the limit, a connect on the leg toward `new` makes a **checkpoint at p** (an ancestor of `new` = `T_ack`).
     - When the tip event is processed, case A applies.
2. **Checkpoints** happen only inside case B, on the leg toward `new`.
3. **At the limit with no outstanding commit** (Opus F9). The limit is `d_max` blocks or the journal budget (§2.9 item 6).
   - **Compress first:** a head `Connected(b)` with a matching `Disconnected(b)` later in the queue is skipped together with it. `last_seq` takes the connect's `seq`, and the disconnect's `seq` goes into `skipped`.
   - If the head survives, the subscriber **parks**: it posts `FlushDue(DMax)` and returns `SubStep::Parked`, and its events thread serves others.
   - **It wakes on a `FlushCommit`, a `RewindRequest`, or any new event enqueued to it** (rev2, GPT r2), so a later matching disconnect is compressed.
   - A subscriber handler never blocks on another component.
4. **The rewind gate** before a backward B1 (rev1, unchanged): it targets every registered mandatory subscriber whose durable L strictly descends from `new`.
   - Synced targets drain toward `new`. Their disconnects are queued or pending, never parked behind a limit.
   - Syncing targets rewind at once.
5. **V's side.**
   - Every V wait (CRITICAL, ALWAYS, backpressure) is a service loop. It answers `PruneClears`, release, read-only queries and `FlushDue`.
   - **A pending `FlushDue` always produces a flush once no F is in flight** (rev2, O-R2-m7). It is a normal freeze if V's tip ≠ the last frozen tip or active has DIRTY entries. Otherwise it is an **empty flush**: B1 carries no coin ops, then issues `FlushCommit` as usual. This covers coin-free blocks (an unspendable-only coinbase, quirk 33), which change the tip but not active. It also covers a connect–disconnect round trip back to the last frozen tip.
   - The ALWAYS shortcut of §4.2 (index-only write, no `FlushCommit`) is never taken while a `FlushDue` is pending.
   - V's idle loop also wakes on `FlushDue` (rev1, Opus N6).
   - Control events (`FlushCommit`, `RewindRequest`, `ChainStateFlushed`) bypass the queue bound and are mirrored in the hub.
6. **The capped eager commit** (§2.9 item 5b) runs on the subscriber's own thread with its own engine, like a checkpoint. It adds no wait edge.

The remaining subscriber rules are unchanged:
- Syncing: a capped cadence at `LCA(progress, commit_cap)`;
- Durable rewind commits (T36), deferred to the next connect, request or commit (n5);
- recovery (§2.9 item 7);
- non-mandatory subscribers never awaited.

#### 4.4.2 Why the system cannot deadlock

**Assumptions** (stated, as GPT r2 asked):
- every I/O call returns or fails, and a failure trips the latch and wakes every waiter;
- the feed meets F1–F10 (§2.9.1); in particular the dispatcher is **weakly fair** (F6): a runnable subscriber is eventually served;
- V's loop is weakly fair across its enabled actions.

**Wait-for edges** (rev1's table, unchanged):
- V → W: CRITICAL, ALWAYS;
- V → a Syncing subscriber's final commit, while `subscribe_at` holds the tip lock (rev3, F10): one Durable write on that subscriber's own engine, which returns or fails;
- V → a subscriber's queue space: backpressure;
- V → W's I/O: the files mutex;
- W → gate targets;
- W → captured subscribers: acks;
- W → V: `PruneClears`;
- subscriber → its own engine;
- Syncing thread → the next `T_ack`.

A parked subscriber waits on nothing: it holds no thread. The handoff edge ends at I/O, and the subscriber holds nothing V needs while it runs, so it adds no cycle.

**Lemma 1 (W progresses)** (rev1's argument, with rule 1's case test).
- Steps 1–4 are W's own I/O.
- **Gate targets** consume their disconnects, which are queued or pending. Pending ones are emitted as soon as the queue drains, and the target is draining it. Off-path connects ahead of them are dropped. A parked target is woken by the request.
- **Captured subscribers** never park, because they have an outstanding commit. Case A acks in one step. Case B consumes events toward the tip event. That event is queued, or pending and emitted once this subscriber's queue has room, which the subscriber itself creates. Each connect on the leg passes the limit or checkpoints at p, which raises L. So the subscriber reaches the tip event, then acks.
- No captured subscriber waits on another subscriber, because handlers never block (rule 3).

**Lemma 2 (a parked subscriber is eventually woken; restated in rev2).** Let S park at a head `Connected(b)` with no outstanding commit. It posted `FlushDue`. By rule 5:
- if no F is in flight, V flushes, emptily if need be, and B1 issues S a `FlushCommit`;
- if F is in flight, Lemma 1 ends it, then the same happens.

S wakes with an outstanding commit and proceeds by Lemma 1's argument. In case B it drops b if b is off the path to `new`, else it checkpoints. In case A it commits or acks.

rev1's Lemma 2 claimed "b, or a later change, is in active", which coin-free blocks and round trips falsify (O-R2-m7, GPT r2). rev2 no longer needs that claim: the empty flush makes every `FlushDue` produce a `FlushCommit`.

**Lemma 3 (V progresses).**
- A wait on W ends by Lemma 1.
- A backpressure wait on S ends because S consumes, or S is parked and woken by Lemma 2.
- The files mutex is held only for I/O.

**Theorem.** Under the assumptions, every wait-for chain ends at a component's own I/O or at V servicing `FlushDue`, so no cycle exists. Every enabled step is eventually taken, and each step consumes an event, completes a flush stage, or advances V. Under weak fairness the system therefore reaches quiescence after V's last change.

#### 4.4.3 The model check (an abstraction with stated limits)

`/work/scratch/ch2-rev2/liveness_model_v2.py` is an **explicit-state abstraction** of §4.4.1's rules. It is not a proof of the threaded implementation; CH2b b3 ports it to `dashd-chainstate/tests/subscriber_liveness_model.rs`. Results are in `run-*.txt` and §"Changes in rev2".

- **What it models:**
  - **V**, one tip change per step: a connect, or **one disconnect at a time** of a scripted reorg of depth 0–2. Each change gets its `seq` at once, and its event stays **pending** until every Synced queue has room. The **queue bound Q is asserted in every state** (GPT r2 m1).
  - **V may freeze while an event is pending**, as rule 5's service loop allows. It freezes only when mandated (`FlushDue`, a full active window, the end), including the empty flush.
  - **A reorg mode with no alternative chain** (`invalidateblock`).
  - **W:** the rewind gate, B1 with `tip_seq`, acks, Bk, release.
  - **Subscribers:** Synced on one shared events thread or one thread each; Syncing on their own thread. Rules 1–6, including the capped eager commit.
  - **The handoff (rev3, G-R3-M4)** in two steps. `Y.ready` happens when catch-up reaches the tip it saw. `Y.subscribe` is atomic with V's tip state: it re-checks `p == V's tip` (else back to Syncing), commits at `LCA(p, cap)`, and installs the cut at the next `seq`, excluding a pending event. V may connect, disconnect, freeze or have an event pending between the two steps, which is Sol's race.
- **What it checks**, in every reachable state:
  - R11a;
  - the data bound `p ≤ h(L) + D_max`;
  - L on p's path;
  - every Synced data queue ≤ Q;
  - **rev3:** no Synced subscriber ignores a `Connected(b)` whose parent is a strict ancestor of p, or a `Disconnected(b)` with b a strict ancestor of p (F10's exclusion; §2.9.1);
  - **AG EF done:** from every reachable state, completion stays reachable. Completion here is the strict form: a final flush with every L = tip. The injected `invalidateblock` regressions use quiescence instead (§4.4.2), because a locator may legitimately lag the tip until the next flush.
  - AG EF done finds deadlocks and fixed points. It does **not** exclude an infinite run that keeps skipping an enabled step; §4.4.2's fairness assumption covers that.
- **What it abstracts:** block contents, rows and state, I/O and its failures, pruning, shutdown, real thread scheduling, the per-block costs of the empty flush, **memory and journal bytes** (the admission rule of §2.9 item 2 maps to the abstract limit, which is never reached at p = L), and **independent fanout** (it emits to all Synced queues at once, F3). The ST3a toy, `ChainSut`, C8(j, k) and the b3 threaded watchdog test cover those.
- **Grid:** subscribers ∈ {Synced+Synced, Synced+Syncing, Syncing+Synced} × {shared thread, a thread each} × `(D_max, Q, active window)` ∈ {(1,1,1), (1,1,2), (2,1,2), (2,2,3), (1,2,3)} × reorg ∈ {none, depth 1, depth 2, depth-2 `invalidateblock`}: **rev3: 120 configurations, 19,367,358 states, all pass**, with the two-step handoff and the new invariant (`/work/scratch/ch2-rev3/liveness_model_v3.py`, `run-grid-*.txt`). rev2's model gave 21,656,118 states on the same grid. (rev2 also ran a v23-timing subset without the eager commit; DEC-82 drops it, §3.8.)
- **Handoff coverage** (rev3) over the Syncing-and-reorg part of the grid: 983,348 transitions in which the re-check failed because V's tip moved between catch-up and `subscribe_at`, and 160,320 handoffs taken with an event pending emission, over its 60 configurations. Both races Sol named are reachable in the correct model, and it completes through them.
- **Directed regressions:**
  - GPT's exact B1 schedule;
  - Opus's R2-M6 configuration;
  - GPT's injected parked state: a parked subscriber, a later matching disconnect still pending, and no freezable work.
  - All 8 rerun unchanged on the rev3 model and pass.
- **Planted regressions:**

| ID | Planted | Expected |
|---|---|---|
| X17 | block the events thread at the limit (Opus F9) | caught by the grid |
| X18 | no case-A ack (GPT B1) | caught |
| X19 | backward B1 without the rewind gate | caught |
| X22 | case A by queue membership (O-R2-M6) | caught |
| X20 | no compression **and** no empty flush | caught on the injected state |
| X23 | no wake on enqueue | caught on the injected state |
| X24 | no empty flush | caught on the injected state paired with X20 |
| X27 | the handoff without the live-tip re-check (Sol's G-R3-M4 producer) | caught by the grid (Syncing configurations) |
| X28 | the pending event delivered across the cut (rev2's handoff) | caught by the grid (the new invariant) |

  X20, X23 and X24 alone are **not reachable failures in the grid**, because rules 1 and 5 subsume them there. They stay as defence in depth, and the threaded test injects their states.

### 4.5 Prune slots (mechanism here; policy in CH8)

- **Step 0 (V)** and **step 7 (W)** are as in §4.2, over ST2's selectors through `PruneHost`, with the pure planning functions that CH2b adds (§2.7).
- The final selection never re-runs the automatic selector (T39). Re-arming follows E20 (T40). A decision deferred because F is in flight keeps the automatic check armed (§3.3).
- **Lock arithmetic** is signed. `height(LCA(…)) − PRUNE_LOCK_BUFFER − 1` and `new − K` are computed in i64, and the final bound is clamped as Core's `last_prune = std::max(1, std::min(last_prune, lock_height))` (`validation.cpp:2999`). A null or start-height locator near genesis never underflows a u32 (GPT audit, item 7).
- V clears the in-memory index entries at step 7. The durable index still says `HAVE_DATA` until Bk, and the files exist until after Bk, so a crash in between is consistent (R5's entitled change (c)).
- **Moveback log** (ST3a item 7; Fable): `SubscriberHub` keeps each mandatory subscriber's emulated in-memory lock height, which is its processed height, lowered by movebacks and reset when it processes a block. On `DisconnectTip` of block h, for each subscriber whose emulated height exceeds `h − 1`, it logs "`<name>` prune lock moved back to `h − 1`" (`validation.cpp:3295-3299`). Our real lock does not move.
- CH2b tests the mechanism with ST2's manual selector on S5-shaped scenarios. CH8 adds `-prune`, the RPC, the incompatibilities and the six `feature_index_prune` assertions.

### 4.6 Errors and disk full (I5)

- **The latch** (§2.10). The first error from a write, barrier, `fdatasync`, `fsync`, flat-file write or read trips it, emits `StoreFailed`, and calls `AbortSink`. After that every mutating call in every store is refused (ST3a R8 across stores). W stops at the step that failed, and every waiter wakes: each one registered with the latch before it parked (E-b1.7).
- **Kinds.** Engine `NoSpace` and `Io` both map to "System error while flushing: …". A flat-file sync failure maps to the block-file or undo-file string. A failed `fsync` is never retried (fsyncgate; ST3a §2.2.5). Unlike Core, the flush stops after a failed block-file sync (†4).
- **Reads.** A coins read error, or a stored coin that fails to decode (DEC-78, D7), trips the latch and aborts the process (§2.4.4). A read error during recovery ends it with an init error and no mutation after the fault (ST3a §4.4).
- **Shutdown after an abort** skips the shutdown flush (ND1, A1 §3.5.6). The next start recovers from the last completed barrier.
- **`-dbcrashratio=N`:** `_Exit(0)` with probability 1/N after each partial coin sub-batch, in flushes and in recovery. Never after B1 or a final batch, as in Core. The RNG is seeded per process, as Core's `FastRandomContext` is, and fixed under the seam.
- **With `-dbcrashratio` > 0 every freeze is synchronous** (rev5, O-R6-M1; DEC-102; D14). At the freeze's call site V waits for `FlushDone` in its service loop, as it does for ALWAYS, before it replies or connects anything else. Why `feature_dbcrash` needs it:
  - The test leaves its nodes unconnected (`test/functional/feature_dbcrash.py:71-74`) and delivers blocks only with `submitblock` (`:105-123`). When a `submitblock` fails because the node died, it restarts the node and waits up to 120 s for `waitforblock(expected_tip)`, where `expected_tip` is the block whose submission failed (`:76-103`, `:147-151`). Its nodes run `-dbcrashratio=8/16/24` with `-dbcache=4/8/16` (`:62-64`).
  - Core passes because its crash point is synchronous with the tip: the roll happens inside the `FlushStateToDisk` of the block being submitted (`txdb.cpp:152-163`), after `WriteBlockIndexDB` has made every accepted block's index durable (`validation.cpp:3065` before `:3092`). So `ReplayBlocks` lands on `expected_tip`.
  - Asynchronously, V would freeze at X, return, and connect X+1 while W rolls the crash on X's sub-batches. `submitblock(X+2)` then fails; recovery brings coins to X, X+1 and X+2 have no durable index entry, no peer re-sends them, and `waitforblock(X+2)` times out (O-R6-M1's scenario).
  - Synchronously, a crash can happen only inside the flush of the block being submitted, whose B1 has already made the index durable through `new` = that block. So recovery lands on `expected_tip`, as in Core.
  - Tests: a b1 regtest smoke reproduces O-R6-M1's scenario with the hook off (it must fail) and on (it must pass), and CH5's acceptance runs `feature_dbcrash` in this mode (§7.4). **E-b1.9:** b1's smoke is unit-level, and the regtest run is CH5's.

### 4.7 The ST3a §5.6 contract, item by item

| # | ST3a §5.6 item | Where in this design |
|---|---|---|
| 1 | Generic stores over `E: KvEngine`, `F: FlatFs`, monomorphised | §2.1; `CoinsCache<E>`, `FlushMachine<E, F, D>`, `Recovery<E, F>`, `SubscriberDriver<D>` over `D::E`, `GuardedEngine<E>`, `GuardedFiles<F>` |
| 2 | Synchronous seams: flush steps, startup recovery, subscriber recovery, V-ahead | §2.7 `FlushMachine::step` + `InlineDriver`; §2.8 `Recovery::step`; §2.9 `SubscriberDriver::{open, step, sync_step}` (out-of-band servicing is inside `step`, so the seam drives it); V-ahead = the seam connects through `BlockOverlay` between W steps (§6.4) |
| 3 | Determinism: fixed salt under the seam; no store call on worker threads | §2.3 (arena iteration salt-free; fallback salt fixed), §4.2 concurrency rule 6, §2.4.3 (CH4 producer off) |
| 4 | Marks: `FlushBegin`, `Durable`, `Locator`, `PruneSelected`, `Connected { applied }`, `BestEffort`, Connect/Recovery regions, `StoreFailed`, `Abort`, `PruneDecision`, `SubscriberState`, `Log` | §2.7 `FlushObserver`/`ObsMark`; `Connected { applied }` from CH3a through the same observer; §4.2 table |
| 5 | Barrier rule with scopes; Bk, Bp and recovery final Durable; B1 presumes the previous Bk/Bp; `sync_wal` at open; W does not count V's writes | §4.1 table and ledger; §4.2 steps 3, 9, 10; §4.3 steps 0 and 9; §4.2 concurrency rule 5; `RefExtractor::finalizes` (§6.4) |
| 6 | Subscribers: `m_synced` classes; synced commits at `LCA(p, new)` after the path drain; capped cadence; Durable rewind; per-block or batched data (declared); backpressure, checkpoint, submit-before-sync with p and s; blind recovery; declared needs; requirements only from the locator's path to G; the sanctioned refusal | §2.9 (`SubscriberDriver`, the rev2 state contract: row classes incl. keyed, per-block writes visible with `best_block()`, journal, `DurableBelow`, monotone high-water, `stage_disconnected`, the capped eager commit; per-block data declared), §4.4.1 (the sequence-number case test, checkpoint on the leg to `new`, compression, parking with wake on enqueue, the empty flush, the rewind gate), §4.4.2–§4.4.3 (deadlock freedom, model-checked), §6.4 (R11 against the production driver; ST3a errata §10 Q1) |
| 7 | Prune decision (dry run under predicted locks, `flush_for_prune ⇔ D ≠ ∅`), final selection `D ∩ eligible(actual)`, re-arm, E12 order, the lock at the LCA, loaded at open, moveback emulation, CH8 cross-check | §4.2 step 0 and step 7, §4.3 step 6, §4.5 (signed and clamped arithmetic), §2.7 (ST2 pure planning) |
| 8 | Check profiles | §6.4: `CoinsSut` → `check_coins_profile` (`coins_cache_model`); `ChainSut` → `check_chainstate` (`chainstate_crash`) |
| 9 | Digest scope: `state_digest` excludes `meta`, `block_index`, `block_files`, `coins` and effect CFs | §6.4 `ChainSut` views over `CHAINSTATE_SCHEMA` (§2.6) |
| 10 | `RefExtractor`: standing cross-store requirements; G = the durable marker's `new`, else `meta.tip` | §6.4 production extractor |
| 11 | WAL order is the engine's, never call order | §4.1 last bullet; §4.2 concurrency rule 5 |
| 12 | `manual_wal_flush = false` | §2.5 (set and asserted), §2.11 ST1 request (c) |
| 13 | Blind replay and rewind | §4.3 steps 1 and 8 (decision keys only, no replay reads), §3.5.2 (no legacy read; the importer normalises), §2.9 `height_ranges` + `delete_range` (blind above L; `DurableBelow` reads only barrier-covered rows ≤ h(L)); planted X8 |

**I1–I5 on the real code.**
- **I1:** B1's index records were snapshotted at the freeze after their appends returned, and B1 follows step 2's barrier. Bk's `d2` comes from its own barrier, and the index-only write runs behind its own.
- **I2:** §4.3.
- **I3:** replay touches only the marker's transition, whose blocks were connected with an `applied` mark before the freeze. Everything above it is CH3b's ordinary activation.
- **I4:** §4.3.
- **I5:** §4.6.

---

## 5. Performance

### 5.1 Metrics CH2 moves

| Metric (A1 §3.9) | How CH2 affects it | Budget (never slower) |
|---|---|---|
| #1/#2 IBD, #3 reindex and `reindex-chainstate` | coin misses on V; flush stalls on V; write volume and compaction | ≤ dashd at equal `-dbcache`, `-par` |
| #5 connect latency at the tip | post-flush misses (Core empties its cache at every flush); a stall at the tip | ≤ dashd p50/p95 |
| #9 startup | open + WAL replay (bounded by `max_total_wal_size` and post-Bk memtable flushes); recovery replay after a crash | ≤ dashd; our replay is lighter (no special-tx re-execution) |
| #10 RSS | B (allocator-checked) + E (74 MiB) + transients | ≤ dashd peak and steady (NFR-RES-01) |
| #11 disk | `coins` and evo CFs in RocksDB | ≤ dashd per component (ST1: same size, writes less) |
| #12 CPU per block | encode on add, decode on read, merge, the flush memcpy | PF0 instructions |
| shutdown (US-NODE-04) | the final ALWAYS flush | `shutdown_s` ≤ dashd |
| flush latency (ST1 OI-7) | V's stall per flush; RocksDB stall counters on W | V stall ≤ dashd's (its whole flush); W's stall p99 recorded |

### 5.2 Memory and density (arithmetic; PF1 measures before any claim, ruling 5)

At `-dbcache=300` with txindex on (B as computed in §2.5):

| | Core (v24) | rdashd, arena (hypothesis) | rdashd, fallback |
|---|---|---|---|
| bytes per cached P2PKH coin | **115.67** (measured probe) | ≈ 80 | ≈ 83.4 at the presize, plus ≈ 1–2 % rounding and fill loss (rev2's presized fallback, §2.3; n1) |
| DEC-43 cell, B = 252.75 MiB | usually flushes at LARGE: max(0.9B, B − 10 MiB) = 242.75 MiB ≈ **2.20 M coins**, then empties; a block can carry it past B before the check | **estimate, not a ceiling** (rev4): a layer is frozen near f·B ≈ 126 MiB ≈ 1.66 M; active + F near B − R ≈ 236.7 MiB ≈ **3.10 M** where V would wait. A block may overshoot either value by its own growth (§2.5), and a2/a1 measure the overshoot | presized: ≈ **2.64 M** where V would wait (estimate, §2.3, with rev4's trigger margin); the same overshoot rule, with the worst case of §2.3 |
| with the allowance (empty mempool) | B = 252.75 + (300 MiB − mempool usage) | proportionally more | proportionally more |
| engine memory | 2 MiB block tree + 8 MiB chainstate + 64 MiB EvoDB LevelDB caches, + mmapped SSTs (RssFile) | E = 74 MiB, one LRU with memtables charged; SSTs in the page cache via `pread` (not in RSS) | same |

- **Reads usually favour us, not always** (GPT). Core's cache ramps 0 → about 0.9B and then empties at every flush. Ours typically holds active plus F, between about f·B and B − R, while V keeps connecting. But a release drops F's clean coins, so right after a release that follows little V progress our cache can hold fewer coins than Core's would. Lever L8 targets that, and PF3 measures hit rates on the traces rather than claiming the advantage.
- **Writes favour Core at f = 0.5.** Our FRESH window is one active layer, about half of Core's, so more coins reach the DB as a put and a later delete. That cost is on W and in compaction, off V's path. PF3 measures puts and deletes per block against the Core model and tunes f; L3 lets f approach 0.8.

### 5.3 V's critical path

- **Freeze:** a pointer swap plus the dirty-index list.
- **CRITICAL stall:** only when active reaches its limit before W releases F. With f = 0.5, W must write F's dirty set before V adds an equal amount.
  - RocksDB inserts Buffered batches at well over 10⁵ keys/s (ST1 D2: 256 MiB of random coin puts in 25 s on a contended disk).
  - V's coin creation in `reindex-chainstate` is a few thousand per second (hypothesis; PF1 measures).
  - So the stall should be rare. Core's stall is the whole flush: files, index, coins and EvoDB.
- **Misses:** a synchronous RocksDB `get` until CH4's prefetch lands. RocksDB's miss path costs about 1.3× LevelDB's CPU (ST1 OI-2). Density, the ramp advantage and pinned partitioned filters cut the miss count; CH2c's workloads count both.
- **Shutdown:** in-flight F + active, against Core's single tip cache; both are flushed synchronously. Neither has a ceiling at shutdown (rev4, G-R4-n1): each is wherever the last checks and the last block left it. Core's is typically below its LARGE threshold because PERIODIC flushes at LARGE, but a block can carry it past B. Ours is typically well below active + F's sizing point, because W has been flushing all along. `coins-shutdown-flush-p300` measures both.

### 5.4 W's throughput

- Serialization is a memcpy of stored bytes, with no re-serialization and no XOR (Core does both).
- Durable fsyncs per split flush: B1, the pre-Bk `sync_wal` and Bk, plus one if a presumed effect is pending, plus about 2 per mandatory subscriber, plus the post-Bk memtable flush.
- At 3–88 ms each (ST1 D1/D2), against flushes minutes apart, that is noise.
- Memtable stalls slow W, not V (ST1 OI-7). W-MC records RocksDB's stall counters.

### 5.5 The DEC-43 cell: the proxy at CH2c's merge, the real cell at the M3 exit (ruling 6; rev1 regime for Opus F4)

- **Configuration** (both sides): `-dbcache=300`, no allowance (full mempool or `-maxmempool` at its floor), txindex as node-bench sets it.
- **Expected:** equal B, E equal to Core's three caches, V never blocked by a flush, and resident coins ≈ **1.22×** Core's at DEC-43 (the fallback a1 kept: 2,676,754 against 2,200,586, PF1 at `57785c8b`; rev5, O-R6-m4; DEC-108).
- **Risks:**
  - RocksDB's miss CPU (OI-2);
  - **double caching under real memory pressure (OI-1)**, which the regime below now measures;
  - background compaction CPU (`max_background_jobs` from A1's compute budget).
- **The regime** (Opus F4). ST1 already learned this (`ST1-bakeoff.md` §1, §3.4, §5). The proxy, W-MC and the M3 cell all run each arm in its own cgroup with **`MemoryMax` covering anonymous memory and page cache together** and `MemorySwapMax=0`.
  - The cap is the same for both arms: Core's coins share B + E (74 MiB) + the harness's or node's fixed overhead + a page-cache allowance.
  - The allowance follows ST1's rule: 0.5 × the workload's peak live set × 75 B, scaled with the trace (ST1 §3.4–§5).
  - Under the cap, LevelDB's mmapped SSTs, RocksDB's page-cache reads and both engines' own caches compete for the same memory, so a page-cache-leaning E cannot win on time by reading from free memory.
- **What gates:**
  - **time:** total time and V-stall time, ≤ the Core model's (sign test over 5 paired rotations, every pair), measured **under the cap**;
  - **memory:**
    - **`rss_anon_peak` ≤ the Core model's.**
    - (rev3, DEC-95) **Peak coins usage ≤ the Core model's.** This compares the maximum of **native Σ** over block boundaries with the maximum of the **Core model's own `DynamicMemoryUsage`** at its block boundaries (in the M3 cell, dashd's `UpdateTip` `cache=` value, `validation.cpp:3178`). It deliberately compares two different counters, because it asks whether we hold more memory (DEC-96 Q2).
    - (rev4, DEC-99; DEC-100) **Native block-boundary peak ≤ B(t):** on these workloads (the proxy and the cell runs) no block boundary has native Σ above B, so native CRITICAL never fires. Mid-block peaks are reported; the `-dbcache=4` witnesses are exempt.
    - Together these are the evidence that replaces rule O's invariant. Mid-block native peaks are reported, not gated. RssAnon counts both arms' heaps and caches equally. RssFile (LevelDB's mmapped pages) and page cache (our `pread`) are excluded from both. rev0's "RssAnon + RssFile" was biased toward RocksDB, because our SST pages appear in neither.
  - `memory.peak` (cgroup, anon + page cache), RssFile and PF0 counters are **reported**, not gated.
  - An uncapped run is informational.
- **The proxy, CH2c's merge gate:** `coins-replay-{testnet,mainnet}-dec43` against **ST1's LevelDB Core-model arm** on the same traces, under the regime above:
  - the Core-model arm uses Core's coins share, an 8 MiB chainstate DB, a 2 MiB block-index DB and a separate 64 MiB LevelDB for the evo stand-in, with Core's synchronous `BatchWrite` order;
  - it runs in a reserved window (ST1 saw within-cell spreads up to 620 %);
  - **rev2 (O-R2-M4):** both arms carry a **txindex-shaped subscriber** (8 + 12 B per transaction, inexact class, `d_max = None` on our side), and both report flushes **by cause**. The gate also requires **0 DMAX flushes**; totals, flushed bytes and V-stall are reported (§5.9).
- **The real cell, at the M3 exit:** node-bench's DEC-43 cell, rdashd against dashd, `reindex-chainstate` over the pre-DIP3 window.
  - Both nodes run under the same cgroup cap: B1b's `--memory-cap` variant, sized as above with the node's measured fixed overhead.
  - It gates `reindex_time_s` and **`peak_rss_anon_mib`**, not VmHWM. VmHWM includes dashd's mmapped SST pages, which would favour rdashd for the wrong reason.
  - The proposed M3 exit 6 text is in §7.4.

### 5.6 The route to 2× (each lever measured by PF0/PF3; none is a claim)

| Lever | Removes | When |
|---|---|---|
| L1 arena density | misses and flush count at equal `-dbcache` | CH2a, if the layout gate keeps it |
| L2 asynchronous freeze-and-flush | dashd's flush stall on V | CH2b |
| L3 segmented F: partition F at the freeze into k sub-layers by hash prefix, release each once written | F held to Bk, so f can rise to ~0.8: bigger FRESH windows, fewer writes | PF3 |
| L4 parallel sub-batch commits (disjoint keys; all returned before the pre-Bk barrier; off under the seam) | W latency on many-core hosts | PF3 |
| L5 SST ingestion for large flushes (durable on return, before the pre-Bk barrier) | memtable and WAL cost | experiment; needs an ST1 API |
| L6 ST2's two-phase files barrier | V's appends waiting on W's `fdatasync` | follow-up |
| L7 CH4's S2 prefetch (the fence is here) | miss stalls on V | CH4 |
| L8 warm retention: after release, keep F's clean unspent entries as a read-only layer while the budget allows | post-flush misses at the tip (#5) | PF3 |
| L9 direct I/O with a larger block cache taken from B | double caching on memory-capped hosts | PF3, only if the M3 cell or PF5 needs it |
| L10 Buffered forward locators (allowed by ST3a §2.4.4 rule 4) | 1 fsync per subscriber per flush | PF3 |
| L11 tombstone-only side map for spent-not-FRESH entries (Fable L1) | half the cost of tombstones | PF3 |

**Amdahl.** PF1 measures dashd's coins share of `reindex-chainstate`: `Connect transactions`, which includes fetches, plus the `-debug=bench` flush lines. L1, L2 and L7 can remove at most that share. The rest of the 2× must come from CH4, CS6 and the payee queue (A1 §3.2). **The kept fallback alone does not reach the 2× target** (rev5, O-R6-m4): it holds 1.216× Core's resident coins at DEC-43, which lowers the miss count but cannot carry 2× throughput. L1 contributes a fraction of the coins share; CH2's levers together are a part of the 2×, not all of it.

### 5.7 PF0 workloads, by sub-task

| Workload | Drives | Gated on | Sub-task |
|---|---|---|---|
| `crit:dashd-chainstate coins_micro` (+ iai) | `fetch`/`add`/`spend`/`commit`, `freeze`, `iter_dirty`, `PackedCoin` encode/decode, index rehash | instructions, allocations | CH2a |
| `coins_layer_density` (PF1) | each layout block by block under §2.5's policy over the 17-point budget sweep, mainnet coin lengths (rev2 gate; rev3 policy) | `N_res` and `r(layout, B)` at the first native CRITICAL wait, peak native Σ, the worst single block's growth (rev4), PMU `instructions:u` and `LLC-load-misses` per hit at B_DEC43 (the layout gate, §2.3; n1) | CH2a |
| `coins-replay-{testnet-435990, mainnet-453371}-{p300, p4096, dec43}` | ST1's traces through the real `CoinsCache` + `FlushMachine` + `RocksEngine`; `InlineDriver` for counters | instructions, allocations, syscalls, I/O bytes, page faults, peak RSS | CH2b (counters), CH2c (wall time, `ThreadDriver`) |
| `flush-split` | one 64 MiB F written as 16 MiB sub-batches | bytes written, syscalls; **protocol barriers asserted exactly** from `FlushObserver` marks (files barrier, B1 Durable, pre-Bk `sync_wal`, Bk Durable, the post-Bk `flush_memtables` call). OS fsync counts are reported, not asserted: SST, manifest and multi-CF flush syncs vary with compaction (GPT) | CH2b |
| `coins-recovery-mainnet` | recovery of a fixed crash image: a 2,000-block split transition stopped after B1 | the counters, plus startup share | CH2b |
| `coins-shutdown-flush-p300` | one ALWAYS flush of a full B | the counters (shutdown proxy) | CH2b |
| `chainstate-open` | open + WAL replay of a fixed DB, including one left by a large B1 and a stalled memtable flush (`max_total_wal_size` is a trigger, not a bound) | the counters | CH2b |
| `engine-memory` (rev1, GPT M2) | the chainstate engine under large B1/evo batches, all CFs, a held full `coins` scan during flushes, stalled flushes (cgroup `io.max`) and compactions | **reported**: `block-cache-usage`, `block-cache-pinned-usage`, `cur-size-all-mem-tables`, `size-all-mem-tables`, `estimate-table-readers-mem`, RssAnon, against §2.5's soft ceiling | CH2c |
| `reindex-chainstate-{pre-DIP3, DIP3–V19, post-V20, tip−50k}` at `-dbcache=300` and `-dbcache=8` | the node, once CH3a lands | counters from then on | CH3a's BG (moved there) |

- **CH2a and CH2b BG:** the rows above against the previous head. A regression above max(noise, 0.5 %) blocks (PF0).
- **CH2c BG:** the DEC-43 proxy under its regime (§5.5).
- **Milestone windows:** `reindex_time_s`, `peak_rss_anon_mib` (§5.5), `shutdown_s`, the DEC-43 cell, and V-stall histograms against dashd's `-debug=bench` flush lines.

### 5.8 The multi-CF bake-off workload W-MC (A1 §2.8; CH2c)

This merges Fable's W-X with Opus's W-MC. It extends `dashd-store-bakeoff`, runs before CH2c merges, and runs again at W-MF.

- **Coins:** ST1's traces (testnet-435990, mainnet-453371; the full mainnet trace when the oracle reaches the tip), replayed through CH2's real cache and flush.
- **Other CFs in the same `chainstate/`, in B1:**
  - `block_index` records (~150 B per block, rewritten on status changes) and `block_files` infos;
  - `mn_diff` per block after DIP3, sized from the oracle's `protx diff` at that height (bytes, not content);
  - `mn_snapshot` and `credit_pool` every 576 blocks, `llmq_mined`/`llmq_mined_h` per mined commitment, `mnhf`;
  - rotation-snapshot effect rows as single Buffered batches.
  Sizes are parametrised from the oracles' evodb key-prefix volumes and recorded with the results.
- **A subscriber DB:** a txindex-shaped RocksDB (8 + 12 B per tx) driven by a `SubscriberDriver` with commit steps, checkpoints and the cadence, so its fsyncs compete as in production.
- **Reorg bursts:** every 2,000 blocks, disconnect k ∈ {1, 6, 30} blocks with the trace's undo (coin restores, `llmq_mined` erases), then connect a **genuinely different branch** of k + 1 synthetic blocks that spend some of the restored coins and create new ones, and finally return to the trace (GPT). Every 50,000 blocks a depth-3 reorg with evo erases. One reorg per run lands on a flush boundary, so a backward B1 exercises the rewind gate.
- **Readers:** 4 snapshot point readers (`gettxout`-shaped), one `iter_prefix` over `mn_diff`, and a full `coins` scan (`gettxoutsetinfo`-shaped) every 50,000 blocks, all while flushes run.
- **Arms:**
  - RocksDB with §2.5's split;
  - **the Core model:** LevelDB chainstate (8 MiB), a separate block-index LevelDB (2 MiB) and EvoDB LevelDB (64 MiB), with Core's synchronous `BatchWrite` (marker in the first batch, all unsynced, EvoDB after coins).
  - MDBX stays available in the harness but is not a decision arm (DEC-43 (a)).
- **Cells:** p300 (allowance on), the DEC-43 cell, p4096. Background compaction is on.
- **Metrics:**
  - per-CF peak and final disk, and disk peak during compaction;
  - write amplification;
  - RocksDB stall counters (`stall-micros`, `is-write-stopped`);
  - W's flush duration and V's stall;
  - miss-read p99;
  - RssAnon and RssFile;
  - scanner time and its effect on V;
  - the durability costs under the real protocol (B1, pre-Bk, Bk, post-Bk memtable flush).
- **Rule:** under §5.5's common cgroup cap, RocksDB ≤ the Core model on time (sign test over 5 pairs), on disk per component, and on **`rss_anon_peak`** (rev2, O-R2-m2; RssFile and `memory.peak` reported, not gated). A miss triggers the tuning items (E split, memtable count, the post-Bk flush, L5, L9), not an engine switch (DEC-43 (a)). The PLAN risk row's MDBX path is reconsidered only if a tracked metric still fails after tuning. Results go to `designs/ST1-bakeoff.md` §11.

### 5.9 `D_max` and the DEC-43 cell's flush count (rev2; O-R2-M4)

**Core's flush count over the pre-DIP3 window** at the DEC-43 settings (`-dbcache=300`, txindex on, no allowance, so B = 252.75 MiB and Core flushes at LARGE = 242.75 MiB):
- **Measured, first part.** ST1's LevelDB Core-model arm at literal 300 (coins budget 244 MiB) replayed mainnet 1 → 453,371 with **2 flushes**, the final flush included: `harness.replay.flushes = 2` in `/work/bench-store/results/wm-l300/leveldb-r0-micro.json`.
- **Estimated, the rest.** The trace's churn is 8.21 M creates + 7.20 M spends over 1,760,111 transactions, 8.75 cache events per transaction. The chain then grows to 13,325,307 transactions at 1,028,159 (`getchaintxstats` on the mainnet oracle), 6.6× more transactions in the remaining 574,788 blocks. Scaling the measured ≈ 1 size-triggered flush by the churn gives **≈ 7–13 more**. So Core makes **≈ 10–15 flushes in total, one every ≈ 70–100 k blocks** after the early chain. This is an **estimate** from a Core-model harness cell, not a measured v24 count (Sol r3); DEC-94 Q2 gates stall and bytes, and M3 measures the count. M3's real cell counts Core's `-debug=coindb` "Writing final batch" lines exactly.
- **With rev1's `D_max = 1,024`**, txindex is a mandatory subscriber, so a forced flush would fire every ≤ 1,024 blocks: **≈ 1,004 flushes**, about 100× Core's.
- **With rev2:**
  - txindex has only inexact rows, so it keeps **no journal** (rev3, G-R3-m1) and can never post `FlushDue(DMax)`. rev2 said "`d_max = None`", which removed the block limit but not the byte limit: one `BlockRef` per block is ≥ 35.3 MiB over the chain (Sol);
  - `llmq/` receives no rows before DIP3, and LK1b declares its classes;
  - so the DEC-43 cell has **0 DMAX flushes**.
- **What is left.** Our flushes in that cell are THRESHOLD freezes at f·B. Each writes about half of what one of Core's does, so there are **≈ 2× Core's count (≈ 20–30)** at f = 1/2. W writes them asynchronously and V does not stall for them. "No more flushes than Core" in total would need f ≈ 0.96, which removes the room active needs while F is in flight. §10 Q2 asks for a ruling.
- **Engine independence** (DEC-85). The 0 comes from the row classes, not from RocksDB: a subscriber with only inexact rows has no journal under any `EngineTuning`. Only the journal budget of the other classes is engine-tuned (§2.5.1).
- **Evidence.**
  - The DEC-43 proxy (§5.5) runs with a **txindex-shaped subscriber** and reports flushes **by cause** for both arms.
  - Its gate requires **0 DMAX flushes**.
  - Total flushes, flushed bytes and V-stall time are reported against the Core-model arm.

---

## 6. Verification

### 6.1 L0: Core's vectors (CH2a)

`src/test/coins_tests.cpp` is read at v24.

| Core test | Port |
|---|---|
| `ccoins_access`, `ccoins_spend`, `ccoins_add`, `ccoins_write` (`:649-1071`): parent and child states ABSENT/SPENT/VALUE × DIRTY/FRESH → value, flags or `FAIL` | **(a)** verbatim, with "parent" = active and F empty: every row must match, `FAIL` = our `CoinsError`. **(b)** each row again with the parent's state placed in **F**: values and `FAIL` match Core; flags may differ only where D2 says so, with an equal DB outcome. Plus a cursor-order vector: vouts 16383 and 16512 of one txid iterate in Core's serialized-key order (16512 first; GPT m3) |
| `coins_cache_simulation_test`, `updatecoins_simulation_test` (`:278, 308`) | ported with freezes, synthetic W completions and releases at random points; every lookup and the final state equal a `MemoryCoins` reference; `sanity_check` after every step |
| `ccoins_flush_behavior` (`:1073`) | the erase-flush half (Core's `Sync()` has no production caller) |
| `coins_resource_is_used` (`:1087`) | replaced by the layer allocation tests (§6.7) |
| `ccoins_serialization`, compressor vectors | CS5a |

Core's fuzz targets `src/test/fuzz/coins_view.cpp` and `coinscache_sim.cpp` seed `coins_overlay_diff`.

### 6.2 L1 (ruling 7)

- **`oracle_coins_ops`** is a new entry point and a small O2a follow-up, **O2a-CO** (§7.4).
  - Input: an op script over Core's stack. That stack is `CCoinsViewDB` (an in-memory LevelDB, `fMemory = true`, with `-dbbatchsize` as given) ← a `CCoinsViewCache` tip ← a child view per "block".
  - Ops: `Begin`, `Add(op, coin, possible_overwrite)`, `Spend`, `Get`, `Have`, `HaveInCache`, `Access`, `Commit`, `Drop`, `Uncache`, `FlushTip(hash)`, `Dump`.
  - Output: per-op results, including the `logic_error` text, plus DB contents and `GetBestBlock`/`GetHeadBlocks` after each `FlushTip`.
- **`diff_coins_ops`** (node-l1, CH2c's last phase) drives the same script through `BlockOverlay` + `CoinsCache` in two modes (GPT m1):
  - **hidden:** freezes and W steps at random points, then ALWAYS at each `FlushTip`. Values, errors (`logic_error` text) and dumps must be equal per op, per pin. `HaveInCache` is **not** compared here (rev5, DEC-102: D12 allows it to differ across differing flush points), and `DynamicMemoryUsage` is not compared anywhere.
  - **aligned:** freezes only at `FlushTip`, run to release before the next op. Here **every** per-op result, membership included, must be equal: after release our layers are empty, as Core's cache is after `Flush()`, so no flush separates differently on the two sides. This is D12's "equal" case, checked.
- Fable's "no new entry point" stance is kept for everything **before** CH2c's last phase: CH2a and CH2b rest on the L0 ports, the `RefCoinsCache` twin and ST3a.
- `diff_codec_primitives` (CS5a) covers coin bytes. O2b's `diff_chain` covers coins inside chain operations, including restarts (M3).
- **The trigger table of §3.3** is a CH2b unit test against a transcription of Core's conditions.

### 6.3 L2: replay windows

- **Trace replays** (`coins-replay-*` doubles as L2): the final `coins` CF equals a `MemoryCoins` rebuilt from the trace. At every Bk the DB equals the twin's snapshot at `new`. In xcheck mode every lookup is compared.
- **With CH3a:** the M3 exit's mainnet 0 → 1,028,159 replay compares the UTXO MuHash per block (CS5b) against the oracle with `-consensusxcheck` on. At every flush boundary and after every recovery, the digest of active ∪ F ∪ DB at `new`, and of the DB alone after Bk, equals the oracle's. `regtest-40000` runs `ChainSut` in capture mode with sampling (M3 exit 2, model level). The `-dbcache=8` window makes every flush a split flush.

### 6.4 CRASH: ST3a's model over CH2's real code (CH2b)

Two SUTs implement ST3a's `Sut`:
- **`CoinsSut`** (coins profile): `CoinsCache` + `FlushMachine` + `Recovery` over one `FaultyEngine`, with no files and no evo. `coins_cache_model` uses it.
- **`ChainSut`** (chainstate profile):
  - the real cache, flush, recovery and subscriber driver;
  - `BlockFiles<FaultyFlatFiles>` (`-fastprune`);
  - the chainstate `FaultyEngine` with `CHAINSTATE_SCHEMA`;
  - a stub `StateDelta` writing `mn_diff`, and one presumed-effect class writing `llmq_snapshot` rows that a later block's stub fold reads;
  - a test subscriber (`SubscriberDriver` over its own `FaultyEngine`, Synced or Syncing);
  - synthetic blocks encoded with CS0, undo with CS5a, and a mini coins connect (no scripts) that emits `Connected { applied }`;
  - `IndexAccess` from CH1, or the test arena.

  Its `RefExtractor` is ST3a §6.2's extractor with the same `finalizes` scopes:
  - index entries require their bytes; a durable marker requires its replay path; a locator requires its path to G plus `DurableTipAtLeast`; data records require nothing;
  - `finalizes`: Bk → `SinceFlushStart`; recovery's final batch → its sub-batches; B1 → `Keys` over the presumed-effect CFs plus `PreviousFinalizer`; every locator → `SincePreviousLocator` plus the cross-store barrier.

**Scenarios.** Exhaustive within ST3a's computed bounds, both crash kinds, zero truncated images asserted. ST3a's S1–S14b run against `ChainSut` with ST3a's preconditions. The C-families name the PLAN acceptance items:

| # | Scenario | Covers |
|---|---|---|
| C1 | first flush (`old` null), one-batch and split (S3, S3p) | **the first flush** |
| C2 | a split flush with 6 sub-batches, a crash at every event: each Bj `Append`, the pre-Bk barrier, Bk's `Append` and `Barrier` (S4, S4b) | **every sub-batch boundary** |
| C3 | (a) V disconnects below `new` while F is in flight; (b) the next marker has `fork ≠ old`; (c) a reorg crossing `old` (S2, S11r) | **a reorg in the window** |
| C4 | a nested crash at every recovery event: depth 1 both kinds, depth 2 sampled (R7a, R7b; S12) | **interrupted recovery** |
| C5 | one-batch flushes, an index-only write, a presumed effect pending before B1 (S1, R12 continuation) | §4.1 scopes |
| C6 | V-ahead: `HAVE_DATA` blocks above `new` appended during the barrier; `d2` in Bk (S5c, S6 shapes) | I1 |
| C7 | ENOSPC and EIO at every store call of W and recovery, `Applied` and `NotApplied`; read faults in recovery and in `fetch` (S8, S9) | I5, §2.4.4 |
| C8 | subscribers. **(a)** Synced backlog 0/2, `D_max` small with S11d's crash points (a)–(e) for b = 1, 2, the rewind commit, Syncing with the capped cadence (S11, S11s, S11q, S11d, S11c, S13). **(b) MPIX-11**: `invalidateblock`, query `best_block()`, crash, recover. **(c) Stale-block retention**: a blockfilter-shaped subscriber (flat file + monotone position + archive) and a coinstats-shaped one; a reorg below and above L; the archive rows of every disconnected block present with Core's values after recovery. **(d) A coinstats-shaped fixed-key accumulator** (GPT M3): capped and checkpoint commits behind p, rewind commits, a crash at every event; R11c(i) on the state key. **(e) The rewind gate**: S2b (§10 Q1), a backward B1 with Synced and Syncing subscribers whose L lies above `new`; R11a in every image. **(f) Shutdown**: deregistration, then the second ALWAYS flush captures no one. **(g) Visibility** (rev2, O-R2-M1): connect, then query at the tip with no flush, for every class (`rpc_getblockfilter.py:37-41`'s shape). **(h) An IX2-shaped keyed subscriber** (rev2, O-R2-M3): address history and unspent rows, a reorg above and below L, a crash at every event, answers equal to Core's after `invalidateblock`. **(c) also** reads every archived filter back through its position after a reorg above L followed by new connects (O-R2-M2). **(i) The capped eager commit**: `getindexinfo` after a crash equals the reference's. **(j) The catch-up-to-live handoff** (rev3, G-R3-M4): a reorg between the snapshot's end and `subscribe_at` (it returns `Moved`, and catch-up rewinds off the stale branch), a pending emission at the cut (the event is not delivered), and a producer that skips the re-check (X27) or delivers across the cut (X28) caught. **(k) Journal admission** (rev3, G-R3-m1): variable-size entries, an entry larger than the budget at an empty journal (admitted alone, then a checkpoint), one arriving at a non-empty journal (checkpoint first), and a txindex-shaped subscriber keeping no journal across 10⁵ blocks. **(c) also** looks up stale filters whose height row was blind-deleted (the archive fallback, §2.9 item 4) | §4.4, §2.9 |
| C9 | the `-dbcrashratio` points: ProcessKill after every partial sub-batch, in flushes and in recovery | US-NODE-04 |
| C10 | prune: S5, S5b, S5c, S14, S14b with ST2's selectors, subscriber-free and with a Synced subscriber (N9 equality); Bp; `prune_selection_protocol` | §4.5 |
| C11 | **CH2 half only:** a freeze or marker never names an unapplied tip; the invalid-connect half moves to CH3a (Opus F5) | I3 |
| C12 | `wipe_chainstate` and its resume (§2.6; §4.3 step 2): ProcessKill and PowerLoss at every store event of the wipe and of the resumed wipe, including nested crashes during the resume (rev5, O-R6-n5) | `-reindex(-chainstate)` durability |

**Planted bugs** (`cfg(feature = "planted-bugs")`, test builds only; the `cargo tree` guard of ST3a §3.1 extends to this feature). Each must be reported and has a checked-in repro.
- **ST3a's T1–T41, re-planted against the production code**, except:
  - T23a/b, which belong to the ST2/CH6 adopter;
  - T12 and the connect halves of T20/T21, which exercise `ChainSut`'s stub connect and move to CH3a's acceptance (Opus F5).
- **Opus's CH2-specific additions**, which have no T counterpart:

| ID | Bug | Caught by |
|---|---|---|
| X6 | no `sync_wal` at open | R7b |
| X8 | replay reads coins keys before writing them (including a "legacy-undo" read) | the per-image `Work` bound check fails |
| X9 | `add` ignores F's unspent coin | L0 (b) rows; `diff_coins_ops` |
| X10 | a prefetch result admitted with a stale `fetch_gen` | `coins_cache_model` xcheck (a resurrected coin); with xcheck off, the view (DEC-142 O2) |
| X13 | V releases F before Bk returns | `coins_cache_model`: the tagging allocator (tagged ≠ Σ − R), and with the meter off a lookup mismatch (DEC-142 O2); R3 |
| X14 | W treats effects written after the freeze as covered by the pre-B1 barrier | D1(f), R12 (a T29 variant) |
| X15 | V's backpressure wait does not service `FlushDue` (new, ruling 8) | §4.4's threaded test hangs under its watchdog |
| X16a | **above the guard:** `BlockOverlay::coin` / `CoinsCache::fetch` map `Err(CoinsReadError)` to `Ok(None)`. The latch is already tripped, so nothing harmful becomes durable (Opus F3) | **FA7** (forward) |
| X16b | **inside the guard:** `GuardedEngine`'s read path swallows the engine error **without tripping**, returning `Ok(None)` | R6(d)/(e) after reopen (a durable `FAILED_VALID` for a valid block) and FA7 |
| X17 | a subscriber at its limit blocks the events thread (rev0's rule) | `subscriber_liveness_model` (Opus F9 cycle); the threaded watchdog test |
| X18 | no case-A ack: the subscriber waits for the queued `FlushCommit` (rev0's rule) | the model (GPT B1 fixed point) |
| X19 | backward B1 without the rewind gate | the model (R11a); S2b (R11a, D1(b)) |
| X20 | parking without queue compression | the model (no wake) |
| X22 | case A decided by queue membership (rev1's test; O-R2-M6) | `subscriber_liveness_model` (stepwise emission) |
| X23 | no wake on enqueue | the model's injected parked state (GPT r2) |
| X24 | no empty flush on `FlushDue` (with X20) | the injected parked state |
| X25 | the blockfilter high-water moves backward on a disconnect (O-R2-M2) | C8(c) read-back |
| X26 | `best_block()` advances before the block's rows return (O-R2-M1) | C8(g) |
| X27 | the catch-up handoff installs the feed without re-checking `p == V's tip` (G-R3-M4) | `subscriber_liveness_model`; C8(j) |
| X28 | the event pending at the cut is delivered to the new subscriber | `subscriber_liveness_model` (the F10 invariant); C8(j) |
| X21 | DEC-78 reversed: a stored coin that fails to decode reads as missing | `corrupt_coin_aborts` (§7.1 a3) and FA7 (no `bad-txns-inputs-missingorspent` verdict) |
| X29 | `CoinsCache::merge` drops the child's best block (Sol's a1+a2 r1 S1 mutation) | `coins_cache_model`'s best-block checks; the one-block commit regression (§7.1 a2) |
| X30 | `BlockOverlay::have_inputs` resolves an input without caching it in active (Opus high's a1+a2 MB mutation; DEC-142 O1) | `coins_cache_model` and `coins_overlay_diff`: the tip's entry, checked against the twin after every `have_inputs` and fetch |
| X31 | `CoinsCache::fetch` caches a database or prefetch hit as DIRTY (Opus high's MI mutation; DEC-142 O1) | the same: the tip's entry |
| X32 | `CoinsCache::admit_prefetch` skips its epoch check (DEC-142 O5) | `the_fence_reads_the_generation_before_the_snapshot`: the debug assert, unplanted; planted, the resurrected coin |

**FA7, a forward assertion** (rev1, Opus F3), checked on every forward run with an injected read fault, a corrupt stored coin, or a decode failure:
- the operation that observes it returns `Fatal`;
- **from the fault until the epoch ends** (rev2, O-R2-m3: under the seam the sink records `Abort` when the latch trips, so "until `Abort`" was empty), `ChainSut` records **no** `Connected { applied }` mark, block-status change (in memory or durable), `ChainEvent`, mempool verdict, peer-visible action or RPC reply. `ChainSut` emits `Verdict`, `Event` and `StatusChange` marks for this.

FA7 catches X16a and X21. X16b is also caught at reopen by R6(d)/(e).

**PR-gate budget:** ≤ 60 CPU-s (ST3a's class); `subscriber_liveness_model` adds ≈ 3 CPU-min in release mode, so it runs nightly with a 20-configuration PR subset. The estimate is ≈ 40 CPU-s: ST3a's toy runs 43.8 CPU-s, `ChainSut` has ~1.5× the events per flush, and ~0.6× the scenarios, because the S6/S10 adopter cases stay in ST3a. Phase b1 measures it. The nightly (`Extended`, depth 2, 2·10⁵ sweep cases) shares ST3a's approved 1.5 CPU-h.

**ST3b hooks:**
- `-dbcrashratio`;
- a `flush` debug category that logs every barrier with the flush id, so `dm-log-writes` marks align to protocol steps;
- `-dbbatchsize` and a tiny `-dbcache` to force split flushes on regtest.

### 6.5 Fuzz targets (the 5-minute smoke per PR; ≥ 24 CPU-h on new targets at merge, all at v24: DEC-59 Q6's v23.1 share is dropped by DEC-82)

| Target | Drives | Oracle | Sub-task |
|---|---|---|---|
| `coins_cache_model` | add/spend/fetch/have/uncache, overlay begin/commit/drop, freeze, W steps, release, prefetch admits with stale and fresh gens, disconnect via undo; CH2b adds crash kinds | the `RefCoinsCache` twin (a line-by-line `CCoinsViewCache` over `MemoryCoins`) for every lookup; the budget invariant; R0, R2–R4, R7, R8, R13 | CH2a (no crash), CH2b |
| `coins_overlay_diff` | random parent/child flag states and op sequences (seeded from Core's two targets) | the twin; `logic_error` parity | CH2a |
| `packed_coin_roundtrip` | arbitrary `TxOut` → `coin_bytes` → `CoinRef` → re-encode; the 34-byte inline boundary; long coins | CS5a's corpus shared | CH2a |
| `chainstate_crash` | ST3a `sweep` over `ChainSut` (§6.4 families, random workloads, faults) | `check_chainstate` | CH2b |
| `coins_replay_blind` | random transitions (connects, reorgs, and connect-then-disconnect windows inside one flush, Opus N1) with random subsets of pending batches, then recovery, interrupted at every batch | a reference recomputed from the block list; idempotence | CH2b |

### 6.6 Functional tests and their owners

| FT | Owner | CH2's part |
|---|---|---|
| `feature_dbcrash` (EXTENDED; utxo hash after crash restarts; `crashed_on_restart > 0`) | CH5 | `-dbcrashratio` in flushes **and** recovery; recovery; **the synchronous-freeze hook (D14, §4.6)** |
| `feature_reindex` | CH5 | `wipe_chainstate` |
| `rpc_blockchain` `gettxoutsetinfo.disk_size` bounds | RP1a | `estimate_size` (pre-checked by a CH2a unit test) |
| `feature_index_prune`, `feature_pruning`, `feature_fastprune` | CH8 | §4.5 slots, the moveback lines |
| `feature_coinstatsindex`, `feature_utxo_set_hash`, `rpc_dumptxoutset` | IX1 / CS5b / RP1a | cursor key order; the forced flush and snapshot iteration |
| mempool tests that depend on `Uncache` | MP1 | `uncache`, `have_in_cache` |
| the F01 restart case; DIFF-RPC `gettxoutsetinfo` after kills on a regtest twin (Fable) | CH3b (M3) | recovery |

### 6.7 The native ledger under a tagging allocator (CH2a)

This section tests **native Σ**, the only counter (DEC-99, DEC-102). The tagging allocator proves that CH2's own ledger is exact; nothing in CH2 claims Core's number.

- **Feature `alloc-tag`** (bench and test builds only). A global allocator wrapper records `(tag, size)` in a 16-byte header. The cache sets the tag `coins` with scoped guards around its own allocation sites: chunks, index growth, long-coin chunks, `LedgerBox` undo copies, DB reads on the fetch path, the prefetch and overlay maps, `StableBytes`. Frees subtract by header on whatever thread and in whatever component frees them (W drops F; CH3a drops undo).
- **Assertions** (rev3, DEC-95; rev4, DEC-99):
  - after every op in `coins_cache_model`, and at sampled points in `coins-replay`: `tagged(coins) == usage().total() − R`, exactly, with `held_out` included (G-R4-m2), and a `LedgerBox` moved from the child counted once;
  - **native Σ is checked only at Core's call sites:** every `size_state` call happens at an op boundary (no overlay open) with `held_out == 0`, and no freeze starts with an overlay open. Each decision equals `core_size_arith(native Σ, B)`. No assertion compares it with Core's flag;
  - **the peak is recorded, not bounded:** the maximum tagged usage inside each op is logged per op, and per block in `coins-replay`, for the a2 witnesses and CH2c's peak evidence;
  - after `release`, the freed bytes are visible to the allocator before V's usage drops (W's `Arc` was gone first);
  - no `coins`-tagged allocation outside the declared sites;
  - per sub-batch, `KvBatch::size_bytes ≤ S + max_record`, and R is `EngineTuning::batch_reserve(S)` (no constant);
  - layout: `size_of::<Slot>() == 72`, `align_of::<Slot>() == 8`, `3,640 × 72 ≤ 262,144`;
  - after warm-up, every index (or shard) load factor is at most 7/8, and at least 7/16 while blocks only add (DEC-131). Under churn only the upper bound is asserted: removes lower a doubled shard's load and tables never shrink (DEC-95, "layers grow"). `boundary_rebuild` rebuilds a shard at its size and never shrinks it.
- `cargo tree -e features,no-dev` on the runtime binaries shows none of `alloc-tag`, `faulty`, `seam` or `planted-bugs`.

---

## 7. Implementation plan

Each phase is one reviewable PR stacked on the previous one. Class STATE: two reviewers plus `code-review` at high effort.

### 7.1 CH2a: cache and store (≈ 9–11 agent-days)

| Phase | Content | Acceptance |
|---|---|---|
| **a1 Layout** | `OutPointKey`, `PackedCoin`, `Slot`, both `CoinLayerOps` impls (the arena, and the **presized** 64-shard fallback of §2.3 rev2), `alloc-tag`, `coins_layer_density` over the budget sweep with the mainnet coin-length histogram; **the layout gate** (§2.3) decides and the loser is deleted in this PR. **Status (DEC-108):** done; a1's head is `57785c8b`. The **fallback** won and the arena is deleted. The gate was re-run with rev4's shard trigger, its churn rebuild and m6's presize in place | layout tests (incl. the fallback's shard decomposition: total within 2^k of target; routing proportional to bucket counts; **routing on hash bits disjoint from hashbrown's low h1 bits and top-7 h2 bits, with h2 tags spread over all 128 values within a shard** (rev5, O-R6-m8; §2.3); shard growth events **counted per budget on the mainnet-shaped sweep, gated at zero only at B_DEC43 while no F is in flight** (rev5, O-R6-m4; at small B the trigger's margin is 4–8 entries and growth is expected); shards **do** grow past the presize on the witnesses and with F in flight, asserted by the tagging allocator (rev3, G-R3-M3); the worst-single-block case of §2.3, with kept growth and transient reported; **per-block rebuild time at p99** beside its frequency, against connect-latency metric #5 (rev5, O-R6-n6); **the stall count with and without the presize-to-what-is-left rule** (rev5, O-R6-m6)); `packed_coin_roundtrip` smoke; PF1 note with `N_res`, `r(layout, B)` and peak native Σ at every sweep point, under §2.5's policy (rev3), the DEC-43 ratio (1.216× at `57785c8b`: 2,676,754 against 2,200,586) and the sweep's geometric-mean ratio, PMU instructions and LLC misses per hit at B_DEC43 (covers the routing bits), and the paired sign test; **the floor `N_res ≥ N_core` at every sweep point, re-run with the trigger** (measured: 1.043 at 4 MiB; DEC-108); the gate's decision recorded |
| **a2 Cache** | `CoinsCache` (active + F + prefetch + DB miss path), `fetch`/`have_in_cache`/`uncache`/`resize`, `freeze`/`release` (in-memory), `boundary_state` (rev5, O-R6-m5), `core_size_arith` (pure), `PrefetchMap` + fence, `BlockOverlay` (`have_inputs`, `add`, `spend`, `commit`, `CoinsView` impl, `StableBytes` under Miri), `RefCoinsCache` twin, `sanity_check`, xcheck; the boundary rebuild (DEC-108) | L0 (a) and (b) rows; **the boundary rebuild** (DEC-108): a churn test at 76.99 MiB in which no shard grows while no F is in flight; both simulation ports with freezes; `coins_cache_model` v0 (no crash: freeze/release/prefetch/reorg interleavings) and `coins_overlay_diff` smoke; budget assertions (§6.7) for every f ∈ {0.25, 0.5, 0.75}; **threshold-arithmetic cases** (rev3, DEC-95; rev5 keeps only CH2a's half, O-R6-m5): `core_size_arith(usage, B)` for usage ∈ {large − 1, large, large + 1, B − 1, B, B + 1} in i64, with and without a mempool allowance, at B = 1.75 MiB and B_DEC43, equals Core's arithmetic (`validation.cpp:2945-2966`); `CoinsCache::boundary_state` returns THRESHOLD at f·B or a shard at its trigger (only with no F in flight), and LARGE/CRITICAL from `core_size_arith` on **native Σ**. **No `FlushPolicy` here:** the mapping of `CoinsBoundary` onto `CACHE_LARGE`/`CACHE_CRITICAL` in Core's modes (`:3036-3046`) moves to b1 (CH2b), which consumes this function; nothing is stubbed. **Sol's divergence case** (20,000 inline P2PKH outputs on an empty tip at `-dbcache=4`): `boundary_state` follows native Σ on the kept layout, recorded; no assertion compares it with Core's flag. Also: a mempool allowance that shrinks between ops; release ordering. **Completion witnesses on the layout a1 kept** (G-R3-M3), at `-dbcache=4`, txindex off, no allowance (B = 1.75 MiB): (i) a 50,000-output block, (ii) a long-input connect (inputs spending 9,577-byte consensus-valid scripts, as many as fit 2 MB) and its disconnect restoring them, (iii) both again at B_DEC43 with F in flight. Each completes, and the tagging allocator equals **native Σ** − R after every op. `boundary_state` after the block is recorded. The native peak is recorded beside Core's for the same block (Sol's probe value, a reported number only). (iv) Fallback shard growth mid-block at B_DEC43, counted to the byte. **Ownership witnesses** (rev4, G-R4-m2; §2.5): long-input spend, then a failed check and overlay abort, then a parent read (original bytes, `held_out` back to 0); the same with commit, then disconnect from the undo (an identical coin), each with a shard rehash triggered between the input's fetch and its spend; **the child holds no input copy** after `have_inputs` (rev5, O-R6-m3); prefetch cases: a completion arriving after release, F-owned spent and recreated keys (GPT); **cache membership (DEC-102, §2.4.5's list):** `mempool_accept.py`'s case with an own-cause freeze and F in flight (true) and after release (false, the D12 case), uncache of a clean entry and not of a DIRTY or F entry, an active tombstone over F, prefetch-only and DB-only coins, and zero DB and prefetch reads on every `have_in_cache` path; `StableBytes` under Miri (Stacked and Tree Borrows); X9, X10, X13, X29, X30, X31 and X32 caught; `coins_micro` baseline |
| **a3 Store** | `CHAINSTATE_SCHEMA` v1, `Meta` codecs (layout tests), `CoinsTable` (cursor, `estimate_size`), `CacheSizes` with Core's log lines, `EngineTuning` with `RocksTuning` (`EngineMemory` + `RocksConfig`: WBM charged to the LRU, per-CF buffers, `manual_wal_flush` asserted) and `MemoryTuning`, `KvBatch::delete_range` (BL-38), `GlobalFailStop`/`GuardedEngine`/`GuardedFiles`/`AbortSink`, `wipe_chainstate`; **CS5a's fallible `CoinsView` adopted** **Status:** implemented on `node/ch2a-coins` (`6db927fd`..`c1b13f15`: four commits, then five from the series review; then the fixes for Sol r1). As built, against the sketches above: the wipe marks itself with its own `meta.wiping`, not `reindexing` (§2.6); `after_bk` and `memory_report` take the `GuardedEngine` (§2.5.1); `GuardedRead` is a trait; `GuardedSnapshot` records the write epoch it was taken at; `GuardedFiles` has no sink; `CoinsBudget::for_engine` takes R from the tuning. The custom-tuning acceptance splits: `CoinsBudget::for_engine` and step 11's `after_bk` (one call per release of a written F, asserted in the coins model) are a3's; its use by `FlushMachine` and the driver, and the journal budget, are CH2b's. Deferred: `GlobalFailStop::check`/`wait_tripped` to b1. Not in the `OPTIONS` file (RocksDB does not write them): the cache's strictness, its capacity and the WBM's size and `allow_stall`. The test reads the capacity and the WBM's size from the engine, and the WBM's charge from the cache's usage; `dashd-store`'s tests check the other two by behaviour, a charge kept beyond the cache's capacity (a strict cache opened through RocksDB's C API keeps it within) and a no-wait write refused as a stall (it goes through with `allow_stall` off). `CacheSizes` is checked against Core's grid for each pointer width (the 32-bit grid comes from Core's `node/caches.cpp` built for i386), on every build. After Sol r1 (S1, S4): `read_failed` aborts even if its log panics, and a snapshot's `multi_get` is checked against a later write. After Opus high (fix r2):
- **DEC-164 (O1):** `RocksEngine::write` never waits inside RocksDB (§2.5). A stalled write is refused, checks the background error and retries. When the flush that would free its memory fails, it returns the latched error within one pause. `a_stalled_write_fails_when_its_flush_fails` covers this, with a read-only database directory and a writable `wal_dir`. A stalled write also ends when another write or barrier latches a failure. No `RocksEngine` has a write-controller delay, and `flush_memtables` does not wait for a stall to clear (tested with compactions held at a gate).
- **O3:** an engine error in a cursor scan or in `estimate_size` aborts (D15).
- **O4:** `CoreAbortSink`'s log must write through before it returns.
- **O5:** the wipe test's keys include both ends of the range.
- **O6:** `CacheSizes::calculate` and `log_lines` take the filter set from `IndexArgs`. `log_lines`'s doc names Core's `-maxmempool` check, which comes before the UTXO line.

For CH5: Core wipes by destroying the database (`dbwrapper.cpp:144-146`, `should_wipe` at `node/chainstate.cpp:233-236`), and ours range-deletes beside the kept `block_index`, so the wiped SSTs stay on disk until compaction. | `CacheSizes` equals Core's numbers for a grid of `-dbcache`, `-txindex`, filter and index flags; `coins_cache_model` runs over `MemoryTuning` and `RocksTuning`; the **custom-tuning test** (rev3, G-R3-m2: a test `EngineTuning` with its own reserve and journal budget, used by `CoinsBudget`, `FlushMachine` and the driver; step 11's `after_bk` call asserted); the `disk_size` unit test; `dashd-store` tests incl. `delete_range` on both engines; the chainstate `RocksConfig` (non-strict cache, `allow_stall`, no L0 pinning, DEC-164's stop-only triggers) asserted from the saved `OPTIONS` file, and the stalled write's wait and its failure on a background error by behaviour; **`corrupt_coin_aborts`** (DEC-78): a value that fails `CoinRef::decode` under a coin key, on `RocksEngine` and `MemoryEngine`, read through `fetch`, `BlockOverlay::coin`, `have_coin`, the cursor, a `GuardedSnapshot` point read and `multi_get` (rev2, O-R2-m9), and with trailing bytes (n7), must trip the latch, call `read_failed` once and return `CoinsReadError` (kind `Corruption`), never `None`; X21 caught by it; `wipe_chainstate` crash-resume unit test |

Dependencies: ST1 (merged), CS5a with the amended trait, CS0.

**Things CH2a must get right** (rev5; Opus r6's list for `node/ch2a-coins`, revised under DEC-102, which removes its items 1–4 and 10):

1. **`have_in_cache`** is a first-hit lookup over active then F: present and unspent, `&self`, infallible, no DB or prefetch read (§2.4.5). Core's `CoinsTip()` fetch and uncache sites are ported as caching `fetch` / `uncache` V commands by the owners in §2.4.5's table.
2. **The child holds no input copies.** Reads resolve child → active → F → prefetch → DB, `have_inputs` caches inputs in active only, and `spend` copies anything the child does not own (O-R6-m3; §2.4.2, §2.5).
3. **Accounting at op boundaries.** `held_out` is a shared `AtomicU64` (a `LedgerBox` drops on any thread). A `LedgerBox` moves its bytes from the child's count to `held_out` without double counting. `tagged == native Σ − R` holds at every op boundary (§6.7). Native Σ is the only counter (DEC-99, DEC-102).
4. **Shard routing** uses hash bits disjoint from hashbrown's low h1 bits and top-7 h2 bits, as `sharded.rs:94` does now (O-R6-m8; §2.3).
5. **Shard trigger and rebuild.** The trigger is 55/64 of a shard's buckets, checked at block boundaries only. A same-size rebuild runs when `len == capacity` and the load is below the trigger, and it is counted in native Σ. At each block boundary with no F in flight, a shard whose live entries plus DELETED buckets have reached its trigger while its live entries have not is rebuilt too (DEC-108). Rebuild time per block is reported at p99 (O-R6-n6). "No growth" is gated at B_DEC43 only (O-R6-m4).
6. **Presize after a grown freeze:** `min(f·(B − R − B/16), B − R − B/16 − F.allocated_bytes())`, with the stall count reported both ways (O-R6-m6; §2.3).
7. **Fallible reads.** Every read path goes through `GuardedRead` / `GuardedSnapshot` and trips the latch before returning `CoinsReadError`. A decode failure or trailing bytes go to `corrupt` (DEC-78, D7; §2.4.4).
8. **`boundary_state` and `core_size_arith` live in CH2a** and are tested there. `FlushPolicy`'s mode mapping is b1's (O-R6-m5).
9. **The floor is re-run with the trigger** before merge (done: 1.043 at 4 MiB at `57785c8b`; DEC-108; §2.3).

### 7.2 CH2b: flush, recovery and subscribers (≈ 17–21 agent-days; re-phased in rev1, Opus F11)

**Held until SB0 is approved** (DEC-81). b1 and b2 do not touch the event source, but they sit on the storage contract that SB0's storage ruling (DEC-85) may change, and b3 sits on the feed. The design here is ready; implementation starts on SB0's approval, with whatever producer and engine it names (§2.9.1, §2.5.1, §10.1).

Every CRASH check runs after `Sut::open`, which is recovery, so flush and recovery land together. Subscriber-bearing scenarios wait for the driver. ST3a's owner confirms that each planted T listed for b1 and b2 is still caught in the `subscribers: None` variant before b1 starts. Any T that is not moves to b3.

| Phase | Content | Acceptance |
|---|---|---|
| **b1 Flush + recovery, subscriber-free** (E-b1.9: merged before its CRASH suite) | `FreezePacket`, `StateDelta`, `EffectLedger`, `FlushPolicy` (§3.3, including manual prune's synchronous path, and Core's guard that flushes the coins only at a best block, `validation.cpp:3078`: `CoinsCache::freeze` panics without one, as at a shutdown during reindex), `FlushMachine` steps 1–5 and 8–12 (one-batch and split; step 6 is a no-op with no subscribers), the index-only write, `ThreadDriver`/`InlineDriver`, `FlushObserver`, errors and `-dbcrashratio`, `Recovery` and `open_chainstate` (§4.3 order), the production `RefExtractor`, `CoinsSut` and `ChainSut` | ST3a's S1, S3, S4, S4b, S7 (CH2 half), S8, S9, S12 in `subscribers: None` variants; C1, C2, C4, C5, C6, C7, C9 and C11's CH2 half, exhaustive, both kinds; T1–T4, T6–T9, T14 (R12), T17, T18, the CH2 halves of T20/T21 (a freeze or marker at an unapplied tip), T24, T26, T27, T29, T31, T34, X6, X8, X13, X14, X16a, X16b, X21 caught; FA7; the trigger-table unit test, including the mapping of `CoinsBoundary` onto Core's modes moved from a2 (rev5, O-R6-m5); the `-dbcrashratio` synchronous-freeze smoke (D14, §4.6: O-R6-M1's scenario fails with the hook off and passes with it on); the copy-point snapshot test (§2.6, O-R6-m2); C12; `flush-split`'s barrier count; `coins_replay_blind` smoke; PR-gate budget measured; PF0 counters baselined |
| **b2 Prune slots, subscriber-free** | `PruneHost`; ST2's pure `plan_files_to_prune[_manual]` + `eligible` (ST2 owner reviews); step 0 and step 7; Bp; clears via V; unlink; re-arm; deferral keeps the check armed; i64-clamped bounds (§4.5) | S5, S5b, S5c, S14b; `prune_selection_protocol`; T5, T11, T13, T15, T19, T25, T39, T40 caught; FA2 |
| **b3 Subscribers** | `SubscriberHub` (registration, deregistration, `commit_cap`, rewind gate), `SubscriberDriver` with the rev2 §2.9 state contract (row classes incl. keyed, per-block visibility, `read`/`iter_prefix`, a byte-bounded journal, per-subscriber `D_max`, `DurableBelow` with prefix scans, archives at the disconnect, the monotone high-water, `stage_disconnected`, the capped eager commit), §4.4.1's rules (the sequence-number case test, compression, parking with wake on enqueue, the empty flush, the rewind gate, V's service loops and idle wake), MPIX-11 `best_block`, the moveback emulation, step 6 and step 3b in `FlushMachine`; **rev3:** `FeedSource::subscribe_at` (F10) with a test producer, `SubscriberError::FeedOrder`, byte admission with the oversized-entry rule, no journal for inexact-only subscribers, the archive fallback for lookups; the liveness model (rev3) ported to Rust; the threaded watchdog test | ST3a's S1, S2, S4, S4b with a synced subscriber, S3p, S11, S11r, S11s, S11q, S11d, S11c, S13, S14, N9; C8 (a–k); T10, T16, T22, T28, T30, T32, T33, T35–T38, T41, X15, X17–X20, X22–X28 caught; FA1, FA3–FA6; `subscriber_liveness_model` (the §4.4.3 grid and directed schedules); the 10⁴-schedule threaded test with a shared and with per-subscriber events threads; `chainstate_crash` and `coins_replay_blind` at the merge fuzz budget |

T12, C11's connect half, and the connect halves of T20/T21 move to CH3a's acceptance. There they rerun with `ChainSut`'s mini connect replaced by the real `connect_block` (Opus F5).

### 7.3 CH2c: performance evidence and L1 (≈ 7–9 agent-days)

| Phase | Content | Acceptance |
|---|---|---|
| **c1 Wall clock and tuning** | the `ThreadDriver` workloads of §5.7 in reserved windows; f ∈ {0.4, 0.5, 0.6, 0.7}; the post-Bk memtable flush on/off; E's WBM fraction | a tuning note in PF3's log; defaults updated only on a sign-test win |
| **c2 W-MC and the DEC-43 proxy** | the W-MC extension and report (§5.8); `coins-replay-*-dec43` against ST1's LevelDB Core-model arm | the W-MC rule; **the DEC-43 proxy passes** (ruling 6); results in `designs/ST1-bakeoff.md` §11 |
| **c3 L1** (after O2a-CO merges) | `diff_coins_ops` against `oracle_coins_ops` | ≥ 24 CPU-h clean at v24; X9 found by the target |

Dependencies: CH2b, PF0/B1b, ST1's bakeoff harness; O2a-CO for c3 only (ruling 7).

**Total ≈ 33–41 agent-days** (rev1 adds the state contract, the gate and the model port). That exceeds v1's single-row L, which is why the work is split.

### 7.4 Proposed PLAN row texts

Columns: ID | content | milestone | crates | deps | size | class | design | effort | PRD | acceptance · BG.

> | CH2a | **Coins cache and store:** `PackedCoin` (Core's `Coin` bytes, 34-byte inline), the layer (arena + `HashTable<u32>`; sharded-hashbrown fallback; the PF1 layout gate keeps one), `CoinsCache` (active → frozen → prefetch → DB; `freeze`/`release`; the prefetch fence), `BlockOverlay` + the fallible `CoinsView`, one coins budget = Core's `nTotalSpace` (tagging allocator), `CacheSizes` (v24), `CHAINSTATE_SCHEMA` v1 and `meta`, the engine budget E behind `EngineTuning` (A1 §3.4 erratum; RocksDB provisional, DEC-85), `GlobalFailStop`/`GuardedEngine`, `wipe_chainstate`, `KvBatch::delete_range` (BL-38) | M3 | dashd-chainstate::coins, dashd-store | ST1, CS5a (fallible `CoinsView`), CS0 | M | STATE | **D2** (designs/CH2-coins-flush.md) | O-xh | FR-CHS-03, FR-STO-02/05, NFR-RES-01 | Core `coins_tests` state tables (as tip and as frozen parent) and both simulation tests; `coins_cache_model` (no crash) and `coins_overlay_diff` smoke; `packed_coin_roundtrip`; the native ledger exact under the tagging allocator and checked at Core's call sites with Core's arithmetic (DEC-95, DEC-99); `have_in_cache` from active then F, never the DB (DEC-102); the completion and ownership witnesses on the kept layout; the floor re-run with the shard trigger; `CacheSizes` equal Core's on a grid; layout gate recorded · BG: PF0 `coins_micro` not worse than the previous head |

> | CH2b | **Flush, recovery and subscribers:** the A1 §3.5 / ST3a §5.6 protocol (`FreezePacket`, `FlushMachine`: prune dry run → space → files → [presumed barrier] → B1 → `ChainStateFlushed` → synced subscriber commits → `D ∩ eligible` → coin sub-batches → `sync_wal` → Bk/Bp → release → unlink; one-batch and split; index-only write; `-dbcrashratio`; effect ledger), coins-only blind `ReplayBlocks` (`open_chainstate`), `SubscriberHub` + `SubscriberDriver` (classes, out-of-band commit servicing, path drain and `D_max` checkpoint, submit-before-sync, capped cadence, Durable rewind with stale-block archives, the rewind gate, parking, blind recovery, the subscriber state contract and the `SubscriberFeed` guarantees F1–F10 with the atomic catch-up handoff, MPIX-11), prune slots (ST2's side-effect-free selector), errors and disk full with Core's strings, model-checked threaded deadlock freedom | M3 | dashd-chainstate::{flush, recovery, subscriber}, dashd-store | CH2a, ST2, ST3a, **SB0** (held until approved, DEC-81), i:the `SubscriberFeed` producer SB0 names (CH1 today) | L | STATE | **D2** (same design) | O-xh | FR-CHS-04, FR-STO-03/04, US-NODE-04 | CRASH model level on every sub-batch boundary, the first flush, a reorg in the window and interrupted recovery (C1–C11 exhaustive, both kinds); T1–T41 (less T23a/b, T12 and the connect halves of T20/T21) and X6–X28 caught; FA1–FA7; `subscriber_liveness_model` (§4.4.3) and the threaded watchdog test; `chainstate_crash` and `coins_replay_blind` at the merge fuzz budget · BG: PF0 counters (`coins-replay-*`, `flush-split`, `coins-recovery-mainnet`, `coins-shutdown-flush-p300`, `chainstate-open`) not worse than the previous head |

> | CH2c | **Coins performance evidence and L1:** PF0 wall-clock workloads (`ThreadDriver`), `f` and `RocksTuning` (memtable, E) tuning, the multi-CF bake-off workload W-MC (A1 §2.8; vs ST1's LevelDB Core-model arm), the DEC-43 proxy; L1 `diff_coins_ops` (last phase) | M3 | dashd-chainstate, dashd-store-bakeoff, node-l1 | CH2b, PF0, i:O2a-CO | M | OTH (perf) + CON (L1) | — (CH2 design §5, §6.2) | O-h | NFR-PERF-01/03, NFR-RES-01/02, NFR-CEQ-04 | the W-MC rule (§5.8) with results in ST1-bakeoff §11; `diff_coins_ops` ≥ 24 CPU-h clean · BG: **the DEC-43 proxy**: `coins-replay-*-dec43` vs ST1's Core-model arm at Core's cache numbers, 5 paired rotations under a common cgroup cap (anon + page cache), total time and V-stall ≤ (sign test) and `rss_anon_peak` ≤ and peak coins usage ≤ (native Σ at block boundaries vs the Core model's `DynamicMemoryUsage`, DEC-95/96) and native block-boundary peak ≤ B (DEC-99), with a txindex-shaped subscriber and **0 DMAX flushes** (RssFile, `memory.peak` and flush counts by cause reported); the real DEC-43 cell runs at the M3 exit |

> | O2a-CO | Oracle follow-up for CH2c (ruling 7; `HaveCoinInCache` per op, compared only in L1's aligned mode; rev5 drops rev2's `DynamicMemoryUsage()` per op, DEC-102, superseding DEC-94 Q3's meter use): `oracle_coins_ops`, an op script over Core's `CCoinsViewDB` (in-memory LevelDB, given `-dbbatchsize`) ← `CCoinsViewCache` tip ← a child view per block; per-op results including the `logic_error` text, DB contents and `GetBestBlock`/`GetHeadBlocks` after each `FlushTip` | M3 | Core `node/oracle-shim-*` branches; dash-oracle-sys | O2a | S | CON (adapter logic) | — | O-h | NFR-CEQ-04 | a smoke differential per op at the v24 pin; Core's `coins_tests` state tables reproduced through the entry point · BG: — |

**Edits to existing rows:**
- **CH2** is replaced by CH2a/CH2b/CH2c.
- **CH3a** deps `CH2` → `CH2a, i:CH2b` (develops against CH2a; its L2 replay merges after CH2b). CH3a's BG adds the `reindex-chainstate` window counters at `-dbcache=300` and `-dbcache=8` (moved from CH2). **CH3a's acceptance adds** (rev2, F5/n1): "ST3a T12 (`HAVE_UNDO` before the undo write returns), C11's connect half (an accepted block whose real connect fails, persisted before connection, ends `FAILED_VALID` after restart) and the connect halves of T20/T21, rerun with `ChainSut`'s stub connect replaced by `connect_block` (CH2 §6.4)".
- **CH4** deps `CH3a, PF1` + `CH2a`.
- **ST3b** and **CH5** deps `CH2` → `CH2b`.
- **CH2b** is held until SB0 is approved (DEC-81); it is listed with the work SB0 can change.
- **CH5** adds: "regtest Core-datadir auto-import reuses CH6a's legacy-undo normalisation (CH2 §3.5.2)", and (rev5, O-R6-M1) "`feature_dbcrash` runs with CH2's synchronous-freeze hook under `-dbcrashratio` (CH2 D14, §4.6)", and (DEC-142 O4) "`verifychain`'s view: Core's VerifyDB keeps one `CCoinsViewCache` over `CoinsTip()` across many blocks without flushing it and asserts `coins.GetBestBlock() == pindex` after each `DisconnectBlock` (`validation.cpp:5112`; level 4 likewise, through `:2523`/`:2912`). A `BlockOverlay` has no best block of its own (its `commit` takes it), so CH5 builds that view itself, or adds an overlay best block it can set without committing".
- **compat ALLOWED-DIFFS** adds (rev5, DEC-102): "D12: `HaveCoinInCache`-dependent ATMP reason (`txn-already-known` vs `bad-txns-inputs-missingorspent`) and the P2P orphan-parent request may differ from Core's across differing coins-cache flush points; equal when no flush separates the coin's caching from the query (CH2 §2.4.5)".
- **CH7a** adds (CH2b b1, Opus high m3): "emit Core's `LOG_TIME_SECONDS("write evodb cache to disk")` pair (`validation.cpp:3096`) around B1's durable write, which carries the EvoDelta (Core's text; printed before the coin batches, where Core prints it after the coins flush)" (CH2 D17; CH7 D20).
- **MP1a, MP1b, RP1a, RP1b, RP2a, RP2b, RP3a, ZQ1, MN0, MN1, CH7a, LK1b, LK1c, CJ1, WL2a** each add (rev5): "port Core's `CoinsTip()` `FetchCoin`/`Uncache` sites as caching `CoinsCache::fetch`/`uncache` V commands (CH2 §2.4.5's table)".
- **CH6a** adds: "adopted Core rev files: entries without undo metadata (zero height, written by Dash ≤ v0.12.2.1) are resolved by the spending block's ancestry and normalised. Each affected rev file is **rewritten into a new rdashd-owned inode** (temp file, sync, rename over the hard link, dir sync, then the index batch with the new undo positions); the source inode is never written. Unresolvable on the active chain ⇒ the import is refused; on a stale branch ⇒ `HAVE_UNDO` is cleared. Listed in the verification report; gated on the two-branch fixture (CH2 §3.5.2)". CH6b inherits it.
- **M3 exit 6** adds: "the DEC-43 low-memory side-by-side cell (`-dbcache=300`, no mempool allowance, `reindex-chainstate` over the pre-DIP3 window): both nodes under the same cgroup `MemoryMax` covering anon + page cache: `reindex_time_s` and `peak_rss_anon_mib` not worse than dashd's (DEC-43 (c); CH2 ruling 6; §5.5)".
- **IX1** adds: "`rpc_getblockfilter` stale-chain filters and `feature_coinstatsindex` reorged-block lookups through `SubscriberData::archive` (CH2 §2.9)".
- **§8 design table:** `CH2 | D2 (Fable + Opus) | …` → `CH2a/b | D2 reconciled (designs/CH2-coins-flush.md)`.

---

## 8. Reconciliation log

F = Fable's design; O = Opus's design; R = a manager ruling.

| # | Item | Fable | Opus | Choice | Why |
|---|---|---|---|---|---|
| 1 | Task shape | one row, phases A–G | split CH2a/b/c (Q5) | **split** (R1), freeze and prefetch fence moved into CH2a | the freeze is in-memory; CH3a then depends on CH2a's full cache API |
| 2 | Layout | hashbrown `HashMap<OutPointKey, Entry>`, inline slots (A1) | arena of 72-B slots + `HashTable<u32>` | **arena primary as a hypothesis; 64-shard hashbrown fallback; a PF1 gate picks one** (R5) | the unsharded map has power-of-two cliffs (≈ 1.83 M coins at the DEC-43 B, below Core's 2.20 M); sharding averages Core's 115.7 B on its own; the arena's ≈ 80 B waits for PF1 |
| 3 | Inline size | 30 B, "every P2PKH/P2SH inline" | 34 B | **34 B** | 4 B height code + up to 8 B amount + 21 B script = 33 B; 30 B spills large non-round amounts at heights above 2^21 |
| 4 | Iteration order | salted (fixed under the seam) | arena order, salt-free | **arena order; the fallback salted, fixed in seam and bench builds** | reproducible PF0 counters and sub-batch composition |
| 5 | F hit on read | a spent F entry copied into active as `SPENT\|FRESH` | F hits not copied; F over DB = DB at `new` | **Opus** | keeps active to Core's four tip states; same DB outcome; a simpler proof (§3.2) |
| 6 | Defensive checks | not discussed | consult active ∪ F | **Opus** | Core's cache spans everything since its last flush |
| 7 | Overlay API | `BlockOverlay<'a>` over `&CoinsCache` | `CoinsOverlay` + `OverlayView` with `RefCell` | **merged:** `BlockOverlay<'c, E>` with `&mut` mutations, `&self` `CoinsView` reads, `have_inputs` first (Core's `HaveInputs` order), `StableBytes` for `&self` DB hits | sound borrowed `CoinRef<'_>` for both layouts without `RefCell` |
| 8 | Read errors | `AbortNode` | latch + infallible trait + `failed()` check (Q4) | **fallible `CoinsView` (`type Error`) + node-wide latch + Core's `std::abort()`** (R3) | both pins `std::abort()` before any verdict (`coins.cpp:367 / :369`); the type system enforces "never a verdict"; CS5a is unmerged |
| 9 | Coins budget | `T = nTotalSpace`, memtables in flight counted inside it | `B(t) = nTotalSpace`; batch reserve R inside; engine memory in E | **Opus's B(t) and R** (R4) | memtables are C++ engine memory a Rust allocator cannot tag; ruling 4 fixes the coins budget at `nTotalSpace` |
| 10 | Engine memory | block cache `clamp(dbcache/16, 16, 64)` + 2×16 MiB memtables from the coins share; `-dbdirectio` knob | E = block-tree + coins-DB shares + 64 MiB, one LRU with WBM-charged memtables | **Opus's E and WBM + Fable's post-Bk `flush_memtables` and memtable shape (`write_buffer_size = S`, 3 buffers); direct I/O becomes lever L9, not a knob** (R4) | the same 74 MiB Core spends, without shrinking B; the WBM charge plus empty memtables gives the memory to blocks most of the time; this is A1 erratum A-2 |
| 11 | Default coins share | 290 MiB (txindex off) | 252.75 MiB (txindex on) | **per `CacheSizes` with the actual flags** | Dash's node-bench cell runs with txindex; memory note `dashd-coins-cache-memory-facts` |
| 12 | Counting vs tagging allocator | counting, compared at freeze/release | tagging, exact after every op | **tagging (test and bench) + exact arithmetic in production** | catches misplaced allocations and runs per op |
| 13 | Sub-batch size | `-dbbatchsize` | `S = min(-dbbatchsize, max(B/8, 64 KiB))` | **Opus** | keeps R small for tiny caches; only the partial-batch count changes |
| 14 | Prefetch cap | none | `prefetch ≤ B/16` | **Opus** | budget-honest admission |
| 15 | "Dash undo always carries height" | yes | no; imported rev files can hold legacy entries | **Opus's fact, verified** | per-txout landed in Dash v0.12.2.2 (`83ec12f1a0`, Dec 2017) |
| 16 | Legacy undo in recovery | `DISCONNECT_FAILED` path kept | one read-based exception (Q13) | **neither: the importer normalises at import; recovery refuses a zero height** (R2) | reachable only via imported rev files; recovery must never read a key it rewrites |
| 17 | Recovery reads | "no key read before written" | the same plus the legacy exception | **the decision keys (`meta`, 5) are read once before the first write; no replay op reads** | precise statement of what ST3a's toy also does |
| 18 | Recovery API | `Recovery::run` | `open_chainstate` + `Recovery::step` | **Opus** | steppable for the seam (item 2) |
| 19 | Flush state machine names | `FlushDriver` = machine; `FlushPlan` | `FlushMachine` + `FlushDriver` trait (`ThreadDriver`/`InlineDriver`); `FreezePacket` | **Opus** | separates protocol from threading |
| 20 | Presumed effects | `presumed_effect_pending: bool` | an `EffectLedger` seq captured at the freeze | **Opus** | exact "returned before the barrier started" coverage, excluding post-freeze effects |
| 21 | Error strings | the three `Failed to …` strings emitted; exit 1 | not emitted (Core throws first); exit per profile | **Opus, verified** (`dbwrapper.cpp:260-267 / :261-268`; P2P-31) | Core's user-visible text is "System error while flushing" |
| 22 | Inner error text | — | — | **engine text, declared (D10)** | LevelDB's "Fatal LevelDB error" cannot be matched by RocksDB |
| 23 | Subscriber side | `SubscriberDriver<D: SubscriberData>` owns every rule | `SubscriberHub` + `LocatorKeeper` library | **merged:** Opus's hub (W side) + Fable's driver/data-trait (subscriber side) with Opus's p/s/L keeper inside; `commit_locator` private | one implementation of every rule; subscribers cannot bypass it |
| 24 | Moveback log emulation | CH2 | CH8 | **CH2b** (in `SubscriberHub`) | ST3a item 7 lists it; the hub holds the processed heights |
| 25 | Who is awaited at step 6 | synced mandatory | the Synced set captured under the hub lock at step 5 | **Opus** | race-free with S11s's syncing → synced transition |
| 26 | Prune clears | `prune_one_block_file` on W | V clears the index (W → V request) | **Opus** | V is the index's only mutator |
| 27 | ST2 selectors | used directly for the dry run | used directly | **new: pure `plan_files_to_prune[_manual]` + `eligible`** | ST2's selectors call `prune_one_block_file` themselves (`flatfile/blocks.rs:580-660`), so they are not side-effect-free |
| 28 | Error latch | engine latches + `BlockFiles::has_failed` | `GlobalFailStop` + `GuardedEngine` | **Opus**, plus `GuardedFiles` and wake-on-trip for every wait | R3; ST3a R8 across stores |
| 29 | `ChainStateFlushed` / events | the events thread | a dispatcher with control events outside the bound | **Opus**; the argument covers a shared or per-subscriber thread | CH1 owns the bus threading |
| 30 | Step 6 / step 8 interleave | sequential | allowed (Q11) | **allowed, both orders explored by the seam** | sub-batches are in no locator's scope and Bk still follows the selection |
| 31 | Deadlock argument | none (Q8 only) | §4.4 edge list | **Opus, kept** (R8), checked against the drain checkpoint and submit-before-sync, with a planted X15 and a threaded test | — |
| 32 | Mempool coins | not specified | V commands (Q12) | **adopted** (R9) | Core holds `cs_main` across ATMP; the cache stays single-owner |
| 33 | `LeftIbd` trigger | absent | kept (Q9) | **kept** | A1 §3.4; one unobservable flush makes the node durable when it starts serving |
| 34 | v24 rebalance lines | — | log-only under V24_0 (Q10) | **Opus** | log parity at no cost; §10 Q8 for an O17 field |
| 35 | Schema | small additions | all A1 CFs from v1; `wipe_chainstate` | **Opus** | no CH7a migration |
| 36 | Planted bugs | T1–T41 re-planted as a feature | 14 X-bugs as `cfg(test)` knobs | **union:** T1–T41 (less T23a/b) + X6, X8, X9, X10, X13, X14 + new X15, X16, behind `planted-bugs` | X1–X5, X7, X11, X12 duplicate T3, T4, T18, T26, T27, T1, T41, T38 |
| 37 | PR-gate CRASH budget | ≈ 60 CPU-s + overhead | ≈ 25 CPU-s | **≤ 60 budget, ≈ 40 estimated, measured in b1** | ST3a's 43.8 × 1.5 events × 0.6 scenarios |
| 38 | Fuzz targets | 4 | 4 | **5** (the union; `flush_protocol_sweep` = `chainstate_crash`, `recovery_replay` = `coins_replay_blind`) | — |
| 39 | Bake-off workload | W-X (evo sized from `protx diff`, depth-3 reorgs, 4 readers) | W-MC (real CH2 code, subscriber DB, Core-model arms, scanner) | **W-MC with W-X's evo sizing, reorgs and readers merged in** | Opus's arms make it a DEC-43-relevant comparison |
| 40 | DEC-43 gate | Phase G runs the real cell before merge | proxy at merge, real cell at M3 (Q1) | **Opus** (R6); the proxy is CH2c's gate | `connect_block` does not exist before CH3a |
| 41 | L1 | no new entry point | `oracle_coins_ops` | **`oracle_coins_ops` as O2a-CO; only CH2c c3 depends on it** (R7) | — |
| 42 | `Sync` semantics / warm retention | L4 | L8 | **one lever, L8** | duplicate |
| 43 | CH1 dependency | stub index | `IndexAccess` + i:CH1 (Q6) | **Opus** | CH1 has not started |
| 44 | Core lines | `ChainStateFlushed` `:2905 / :3114`; `m_synced` `:78 / :90` | `:2903 / :3111`; `:78 / :90` | **`:2905 / :3114`; `m_synced` at `:78 / :90`** (rev1: rev0's `:77 / :89` was wrong; Opus N2) | re-checked at both pins |
| 45 | `RollforwardBlock` range | `:4796-4853 / :5207-5216` (partial) | `:4772-4877 / :5180-5220` | **Opus's ranges**; v23's legacy index writes noted (§3.8) | — |
| 46 | EvoDB inconsistency string | `:2004 / :2290` (use sites) | `:2001-2006 / :1923-1929` | **`:2004 / :1928`** (the string; v24 uses it at `:2290`, `:2529`) | — |
| 47 | `access_by_txid` in recovery | via CS5a | via a read exception | **not used by recovery**; CH3a's live path only | R2 |

---

## 9. Proposed A1 errata (with the BL-26 batch)

| # | A1 location | Proposed |
|---|---|---|
| A-1 | §3.4 "One memory budget" | B(t) is Core's `nTotalSpace` exactly (the coins share plus the unused mempool allowance; `validation.cpp:2952-2953`). **One counter (DEC-95, DEC-99, DEC-102).** The **native ledger** Σ = active + F + prefetch + R (CH2's own allocations; exact, asserted by a tagging allocator in test and bench builds) drives the flush policy. It is checked only at `FlushStateToDisk`'s call sites (after each block, after ATMP, in Core's flush modes) with `GetCoinsCacheSizeState`'s arithmetic (CRITICAL above B, LARGE above max(0.9B, B − 10 MiB)). These flush points are **not** Core's, and flush timing is not observable. `HaveCoinInCache` answers from the in-memory layers (active then F), not from a model of Core's flush epochs; across differing flush points its answer may differ (an allowed difference). Under `-dbcrashratio` every freeze is synchronous. No freeze happens mid-block; a block may grow the cache past B as in Core, and every layout grows so a valid block completes. R is the engine's sub-batch reserve from `EngineTuning` (`S + 64 KiB` under RocksDB; static `S = min(-dbbatchsize, max(coins_share/8, 64 KiB))`). The peak is gated as evidence against dashd's (CH2c, DEC-43: `rss_anon_peak`, native block-boundary peak against Core's `cache=`, and native block-boundary peak ≤ B), not as an invariant |
| **A-2** | §3.4 `-dbcache` split table; §2.8 RocksDB configuration (**the ruling-4 erratum**) | **Provisional (DEC-85):** SB0 re-checks RocksDB for mobile. If the engine changes, the RocksDB clauses below become the new engine's `EngineTuning` (CH2 §2.5.1), and the coins share and E's formula stay. Delete the "8 % block cache + 64 MiB memtables out of `-dbcache`" split. **The coins share is Core's `CalculateCacheSizes` remainder (v24). The `chainstate/` engine gets E = block-tree share + coins-DB share + 64 MiB (Core's fixed EvoDB cache, `evo/evodb.cpp:38`), as one non-strict LRU block cache with every CF's memtables charged to it through a stalling `WriteBufferManager` of E/2, and no L0 pinning.** E is a **configured target**, not a hard bound: the soft ceiling is `≲ max(E, P + M + I) + X` (CH2 §2.5), and the evidence is the DEC-43 `rss_anon` gate. The `coins` CF uses `write_buffer_size = S` and 3 buffers; the others 4 MiB. Buffered `pread`, no mmap, no direct I/O by default (lever L9); `flush_memtables` after each Bk; `max_total_wal_size = E` (a flush trigger); `manual_wal_flush = false` asserted. Index DBs keep Core's per-index shares |
| A-3 | §3.4 `CoinsCache` sketch and density paragraph | The layout is an arena of 72-byte slots plus a `HashTable<u32>` index (≈ 80 B per coin, a **hypothesis** until PF1), with a **presized** 64-shard hashbrown fallback (≈ 83 B per entry at the presize; it freezes at a block boundary when a shard nears its growth load, and a shard that fills inside an op doubles, so a valid block always completes); a PF1 gate keeps one on budget bytes per resident coin at the DEC-43 budget and over a sweep, with a floor at Core's resident count. Inline coins are up to 34 B |
| A-4 | §3.4 read path | An F hit is not copied into active; DB and prefetch hits are copied as clean; tombstones are authoritative; the defensive checks consult active ∪ F |
| A-5 | §3.5.4 | ST3a's order with E12–E22; steps 6 and 8 may interleave; `-dbcrashratio` also applies in recovery; the three `Failed to …` strings are not emitted (Core throws first) |
| A-6 | §3.5.5 steps 1–4 | Startup order: open and `sync_wal`; read the six `meta` decision keys once (resume an interrupted wipe first); open `BlockFiles`; load the index; determine G; register prune locks; startup scan; replay. Recovery reads only the decision keys, never a key the replay writes. A zero-height undo entry is corruption ("Unable to replay blocks…"). Imported legacy undo is normalised by the importer (CH5, CH6a), by ancestry, into rewritten rdashd-owned rev files |
| A-7 | §3.5.7 subscribers | The real-system mapping of CH2 §4.4: control events outside the queue bound; every V wait is a service loop that can freeze; the deadlock-freedom argument |
| A-8 | §3.5.6 | Coins read errors end the process as Core's `std::abort()` does; `CoinsView` is fallible and a read error is never a verdict. A stored coin that fails to decode also aborts (DEC-78), where Core reads it as missing |
| A-9 | §3.5.7 subscribers | The subscriber state contract (§2.9: row classes, journal, state keys only in locator batches, `DurableBelow` reads ≤ h(L), archives on rewind), out-of-band commit servicing, parking, and the rewind gate before a backward B1 (§4.4) |

---

## 10. Open questions for the manager (rev5)

rev2's Q1–Q7 were decided in DEC-94, and DEC-95 supersedes its Q1 (no `T_max`). rev3's Q1–Q3 were decided as recommended in DEC-96: the handoff commit runs under the tip lock, peak coins usage is the block-boundary maximum plus `rss_anon_peak`, and one oversized journal entry is admitted alone. rev4's Q1 was decided in DEC-100 as recommended. **rev5 raises none**: DEC-102 settled the only open choice. rev4's question is kept below for reference.

| # | Question | Recommendation |
|---|---|---|
| ~~Q1 (rev4)~~ | **Decided (DEC-100).** **How to read DEC-99's gate "native peak ≤ B".** DEC-95 lets a block grow the cache past B, and native CRITICAL is by definition native Σ > B at a block boundary. So read as a hard invariant on every peak, the gate would forbid what DEC-95 allows, and it would fail on the a2 witnesses. | **Read it as a CH2c / DEC-43 evidence gate on the gated workloads**: no block boundary of the proxy or cell runs has native Σ above B(t), so native CRITICAL never fires there (§5.5). Mid-block native peaks are reported only (DEC-96 Q2), and the witnesses are exempt. |

### 10.1 What SB0 must preserve for CH2's subscribers to stay correct (rev2 addendum, DEC-81)

Proposed as an input to SB0's brief. Each item is something §4.4.2's argument, ST3a §5.6 or the state contract uses; if SB0 drops one, CH2b's argument has to be redone for that change.

| # | Must hold | Used by |
|---|---|---|
| P1 | **The feed guarantees F1–F11** (§2.9.1; the *producer* rows are SB0's to implement, the *CH2* rows are CH2's): per-subscriber order consistent with V's tip changes from the subscriber's cut; `seq` assigned at the tip change before backpressure; bounded data queues whose backpressure is a V service-loop wait; control events out of band and mirrored; wake on enqueue; weak fairness; the flush barrier; block, undo and ancestry readable from subscriber threads; `best_block()` owned by the driver | §4.4, Lemmas 1–3, X17–X24 |
| P2 | **One writer of the chain tip (V) that services `FlushDue`** in every wait, including the empty flush when nothing is dirty | Lemma 2, rule 5 |
| P3 | **The flush order** B1 → `ChainStateFlushed` → synced subscriber commits → prune selection `D ∩ eligible` → Bk/Bp, with the rewind gate before a backward B1 and `T_ack` published only after B1 | ST3a §5.6 items 4–8; R11a; DEC-64 prune locks |
| P4 | **Prune locks from subscriber locators**: block and undo files along each mandatory subscriber's path from L stay on disk until its commit passes them | R11b; F8; `stage_disconnected` and the rewind commit |
| P5 | **A storage engine meeting the `KvEngine` contract** of §2.5.1: Buffered and Durable commits, `sync_wal`, `delete_range`, snapshots, key-ordered cursors, and DEC-62's subset-semantics recovery (or stronger). Each subscriber commits to its own engine on its own thread | blind recovery (§2.9 item 7); no wait edge into W |
| P6 | **One node-wide fail-stop latch** spanning the chainstate engine, every subscriber engine and the block files | ruling 3; §2.10 |
| P7 | **Subscriber classes and the atomic catch-up-to-live handoff** (rev3, G-R3-M4; F10): Syncing catches up from a **coherent** `ChainSnapshot` (tip, `tip_seq`, heights from one chain state). It turns Synced only through `FeedSource::subscribe_at`, which is linearized with V's tip changes, taken under the lock V holds to assign a `seq` and change the tip. Inside it: the check `p == V's tip` (else `Moved` and nothing installed), the final capped commit, and the cut `seq_synced` = the next seq. The event pending at the cut is excluded. No tip change may fall between the check and the first delivered event. This is Core's `NextSyncBlock` test, `Commit` and `m_synced = true` under `cs_main` (`index/base.cpp:163-170`) | §4.4 classes; T35 |
| P8 | **Undo where a subscriber needs it.** A subscriber with `BlockNeeds.undo` (coinstatsindex, IX2) cannot run on a node that keeps no undo, as an SPV mode would not. SB0 must refuse its registration there, not feed it blocks without undo | `connect`, `stage_disconnected` |
| P9 | **Mempool coin access as V commands** (ruling 9), so the coins cache stays single-owner. A node-internal rule: it binds SB0 only **if SB0 shares the mempool** (rev5, O-R6-m7) | §2.4.1; §2.4.5 |
| P10 | **The block index persists in CH2's B1** (rev5, O-R6-m7): block-index records and file infos are written by B1 in the chainstate engine, **in the same batch as `meta`, the marker and evo** (I1; †2), and recovery loads `IndexAccess` from that durable state before V starts (§4.3 steps 4–6). If SB0 moved headers or the block index to a separate store (an SPV header store invites it), I1 and †2 would fail and recovery's checks on `new` would have no atomic basis | I1; †2; §4.3 |
