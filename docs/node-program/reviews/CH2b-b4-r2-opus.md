# CH2b b4 (M1 NodeFlush + M2 NodeBlockFiles): narrow Opus review, round 2

Target: `node/ch2b-b4` at `d1f4951a6` (detached worktree `/work/scratch/ch2b-b4-r2-opus/wt`). Fix range: `b994a05fe..d1f4951a6` (17 commits). Round 1: `reviews/CH2b-b4-r1-opus.md`. Core reference: `~/workspace/dash` v24 (`v24.0.0-rc.2-109-g15db4787500`).

**Verdict: ACCEPT WITH NITS.**

- All of round 1's findings are closed: M-1, m-1 to m-5, and n-1 to n-3.
- The fix commits add no Major or Minor defect.
- The CH1-node edits are test-only or behaviour-neutral for CH1's own paths.
- There are three new Nits, all about wording or a model edge. None blocks the merge.

## Round-1 findings

| Finding | Status | Evidence |
|---|---|---|
| **M-1** in-hold drain lost wakeup | **Closed** | See below. |
| **m-1** "System error" save path | **Closed** | See below. |
| **m-2** no-tip flush writes nothing | **Closed** | See below. |
| **m-3** PruneAndFlush contract | **Closed** | See below. |
| **m-4** BL-127's CRASH half | **Closed** | See below. |
| **m-5** unpinned pump, untested M2 arms | **Closed** (the watchdog variant was not done, and that is acceptable) | See below. |
| **n-1** `set_pump` with `set_bell` | **Closed** | `driver.rs:418`, `:435`: a `debug_assert!` each way, and both docs say a driver takes one or the other. CRASH and the watchdog harness use `set_pump` only; `NodeFlush` uses `set_bell` only. |
| **n-2** D62's Core-outcome claim | **Closed** | D62 (5) now cites ST3a R8, and BL-213/DEC-268 for NoSpace, and says outright that this is not a Core parity claim. |
| **n-3** D62 and D63 wording | **Closed** | D62 (1) now describes the `seq.rs:145-161` write failure precisely: two lines, matching neither Core outcome. D63 now names the reader built in `new`, its shared handle cache, and the in-process-reload caveat. |

### M-1: closed

**Primary fix:** `chain/abc.rs:350-354` re-checks `retry_pending()` after `answer_due` and before `park()`. This is the `backpressure_wait` shape (`flush/control.rs:268-278`).

**Backstop:** `flush/driver.rs:445-451` (the notify at `:449`). `wait_for_event` re-notifies V's wake if `next_event` parked on it. A permit it takes is passed on, so no later waiter loses one.

**The backstop cannot spin.** `parked` is set only on a real park, and the permit is re-set once per call, at exit. A belled wait therefore costs at most one spurious turn of its next waiter.

**Who parks on V's wake in production.** Only `drain_fanout` and the driver's own wait do (grep of `chain/` and `flush/`). V's idle loop selects on the bell's channel, not on the parker. Every parker is loop-shaped, so the extra permit is harmless.

**Regression test:** `node_stores::a_flush_in_the_in_hold_drain_leaves_the_drain_awake` (`node_stores.rs:1345`). It uses a Q = 1 subscriber, the synchronous policy, the bell, and W held at the gate. My runs:

| Code under test | Result |
|---|---|
| Baseline | 40/40 pass |
| Both fixes reverted | 3/3 fail at `:1398` ("V's drain parked after the release") |
| Drain re-check removed (backstop kept) | 10/10 pass |
| Backstop removed (re-check kept) | 10/10 pass |
| Round 1's m-5 mutant (`driver.rs:472` `pump();` removed) | 3/3 fail |

This matches D81's "fails with neither, passes with either" claim. As a result, neither fix is pinned on its own. That is acceptable for a belt-and-braces pair, because each one alone ends the hang. The backstop has no other consumer today.

**The remaining 200 ms sleep (`:1390`) does not weaken the test.** With both fixes reverted, V reaches the bug whenever it parks to take W's release. W is held until after `pumped` is asserted, so V is always in its wait loop by then.

### m-1: closed

- **The fix:** `store/blocks.rs:51-95` (`save_failed`) returns the exception's text, and `save_block` (`:142`) returns `Err(what)`.
  - V's `Err` arm (`chain/accept.rs:290`) builds `Fatal::new("System error: {msg}")`. This is Core's `AbortNode(state, "System error: " + e.what())` (`validation.cpp:4842`).
  - The adapter reports once, because the latch is tripped and `Fatal::new` is `UnlessLatched`.
- **Tests:**
  - `node_stores::a_failed_space_query_is_a_system_error` (adapter level);
  - `v_stops_on_a_system_error_reported_once`, which goes through V and asserts exactly one report.
- **Recorded differences:** D62 records the `io::Error` text (3), and the fact that V's `Err` arm stops V with no FAILED line and no reply (4). (4) is CH1-node's `Err` arm, which every `BlockFiles` shares. Core's state text is now matched where V reports it.

### m-2: closed

**Implementation:** `NodeFlush::run` (`flush/site.rs:166-184`) routes each no-tip call by kind:

| Call | Routed to |
|---|---|
| `at_site`, `flush` | `flush_without_tip(mode)` |
| `stop` | `stop_without_tip` |
| `prune_and_flush` | `prune_and_flush_without_tip` |
| everything else | `service_without_tip` |

**Policy side:**
- `FlushPolicy::decide` with `has_best_block: false` (`policy.rs:227-235`) yields `IndexOnly { signal: false }` for ALWAYS, a kept cause, or a due PERIODIC write.
- It never yields a `Freeze` there, so the `unreachable!` in `control.rs:744-749` holds. After a `WaitThenFreeze`, the loop re-decides with nothing in flight and lands on `IndexOnly`.

This matches Core at `Height() == -1` (`validation.cpp:3046-3074`): `FlushBlockFile` and `WriteBlockIndexDB` run, and the coins write and `ChainStateFlushed` are skipped (`:3076`).

**The prune half:** `NoTipPrune::check_due` (`control.rs:872-878`) takes the armed check under the "find files to prune" timer and returns false. This matches `:3012-3015` plus `FindFilesToPrune`'s `chain_tip_height < 0` return (`blockstorage.cpp:191-193`). That covers Core's `AcceptBlock` NONE call too (`validation.cpp:4852-4854`; V's `flush_now(None)`, `chain/accept.rs:295`). `is_ibd: true` matches `IsInitialBlockDownload()` with a null tip.

**Docs and tests:** D55 is rewritten with the Core citations and the consequence (genesis entry and blk00000 info persisted at an early shutdown). Five `control::tests` cover it, and `513bd5856` kills the `==` mutant.

**One judgement call, recorded in D55:** at genesis's ConnectTip, Core's CoinsTip does have a best block, so a critical cache there would flush coins. D55 argues IF_NEEDED cannot be critical with one block and an empty mempool. I agree.

### m-3: closed

**The new entry:** `FlushSite::prune_and_flush` → `FlushControl::prune_and_flush` (`control.rs:282-300`, fn at `:290`) runs these steps in order:
1. waits out the job in flight;
2. calls `PruneHost::arm`, which is ST2's `rearm_check_for_pruning` (Core's `m_check_for_pruning = true`, `validation.cpp:3142`);
3. runs the NONE flush, with its machine marked `unlink_before_release` (`machine.rs:407`, `tail` at `:992`);
4. calls `stop`, which waits for that release.

So the files are unlinked when the call returns, as Core's synchronous `FlushStateToDisk` leaves them (`:3069-3074`) before init's `ShutdownRequested` check (`init.cpp:2359`).

**Edge handling:** `unlink_first` is reset whether `flush` succeeds or fails (`control.rs:296-298`). MAJ-1's ordering is fixed by `827c91c5d`.

**Failure contract:** D57 now states it. Every `Err` out of `with_cx` is `Fatal::reported` (`site.rs:206-208`), so M3 logs Core's `PruneAndFlush: failed to flush state` line and goes on to init's next check. That matches Core's log-and-continue (`validation.cpp:3143-3145`).

**Tests:**
- `node_stores::prune_and_flush_arms_the_check_and_returns_with_the_files_unlinked` passed 20/20 in my repeats.
- The no-tip twin is in `control::tests`.

### m-4: closed

**The model:** `crash/chain.rs` `blk_files_present` is called from `recover` after the index and file-info load and before the prune scan, which is Core's `LoadBlockIndexDB` order (`blockstorage.cpp:378-390`).
- It opens every `HAVE_DATA` entry's blk file in ascending file order and stops at the first failure.
- An I/O error is a fatal read in `PATH_LOAD_INDEX`, sanctioned only there (`stores.rs` `sanctioned_after`).
- A missing file is a sanctioned refusal only when `lost_to_failed_dir_sync` holds and E's records pass R1 with the loss entitled. Otherwise it is an abort named with the reason, which R0 flags.

**R1 is tightened:** `check_records` entitles a lost blk file only with `refusing`, and a lost rev file as before. That closes the entitlement gap round 1 named.

**Planted control and tests:**
- `NoBlkChk` is the planted control.
- `crash_planted::a_blk_file_lost_to_a_failed_dir_sync_refuses_the_restart` builds the two-failed-dir-sync world and pins the sanction both ways.
- `a_lost_block_file_gives_cores_open_lines` now expects the load's refusal (R0, unwarranted).
- I ran the six `lost` tests: 6/6 pass.

**CRASH's step 10** is now the node's own call (`8b73363a9`).

### m-5: closed

- The pump mutant is killed by the M-1 regression test: 3/3 fail in my run, as listed under M-1. That test is the bell-path subscriber test round 1 asked for.
- The untested M2 arms are covered:
  - the System-error arm: `a_failed_space_query_is_a_system_error` and `v_stops_on_a_system_error_reported_once`;
  - the rollover flush failure: `a_failed_flush_at_a_rollover_is_cores_abort`;
  - `Corrupt`: the `blocks.rs` unit test `a_corrupt_save_is_an_abort_node_with_its_text`.
- **Not done:** the watchdog variant over `set_bell` and `wait_pumped`. With M-1's test in place, that variant would only add schedule breadth.
- **Accepted survivors:**
  - `set_pump` stubbed: it has no production caller, and the bell path is now pinned;
  - `NoTipPrune::dry_run` stubbed: unreachable, because `check_due` always returns false.

## CH1-node edits (behaviour-neutrality check)

- **`chain/abc.rs` drain re-check.** With CH1's own `NoFlush`, `answer_due` is a no-op and never parks. The only change is one extra `retry_pending()` before each park, and the loop already calls it on every wake. `Stage::Drain` and `at_stage` are `#[cfg(test)]`.
- **`chain/actor.rs`.** `prune_and_flush_at_site` is `#[cfg(test)]`. `Stage::Drain` lives in the `tests` module.
- **`FlushSite::prune_and_flush` (default `Ok(())`).** Production implementors are `NodeFlush`, which overrides it, and `NoFlush`, for which `Ok` is right: it has no flush side, as with `flush`. `ModelW` and `Latched` are test sites. Nothing is hidden today.
- **`PruneHost::arm` (default no-op).** `FilesPruneHost` overrides it. `NoPrune`'s no-op is right: it does not prune. `Switched` (test) forwards. See n-c for the two wrappers.
- **`FlushBell::park` → `FlushBell::wake()`.** A `pub(crate)` change whose only caller is `NodeFlush::attach`. No behaviour change.
- **Lock order.** `arm` takes the files mutex under the coins cell, the same order as `check_due` inside `decide`. I found no new inversion.

## New findings

### n-a (Nit): Core line drift in D57 and the PruneAndFlush comments

- **Where:**
  - D57's "`v:3146`" for the `failed to flush state` line;
  - "`validation.cpp:3141-3148`" in `chain/flush.rs:101` and `flush/control.rs:282`, and in D57.
- **Problem:** in the pinned Core, `Chainstate::PruneAndFlush` is `validation.cpp:3139-3146`, and its log line is `:3144`.
- **Fix:** correct the citations.

### n-b (Nit): CRASH skips the no-tip PruneAndFlush, so it leaves the check armed where the node clears it

- **Where:** `crash/chain.rs` `VState::prune_and_flush` (`self.active.is_empty()` returns early), and D57's "with no chain the check selects nothing".
- **Scenario:** a prune-mode restart that recovers no active tip, with the check armed by an earlier allocation.
  - The node's `prune_and_flush_without_tip` arms the check and then clears it, as Core does at `Height() == -1`.
  - CRASH leaves the check as it found it. Its first site with a tip then makes one dry run that the node does not make.
  - That dry run selects nothing at a low tip, so no state differs. Only the BENCHMARK line or mark differs.
- **Fix:** either record in D57 that CRASH keeps the check's prior state there, or clear it in CRASH's skip branch (`take_check_for_pruning`).

### n-c (Nit): the `PruneHost` wrappers don't forward `arm`

- **Where:** `flush/prune.rs:219` `TimedPrune` and `flush/control.rs:872` `NoTipPrune` both take the default no-op `arm`.
- **Why it doesn't matter today:** `prune_and_flush` calls `cx.prune.arm()` on the unwrapped host, and the wrappers live only inside `decide`.
- **Risk:** a future caller that arms through a wrapper would lose the arm silently. This is the "default that hides a missing implementation" shape the brief asks about.
- **Fix:** forward `arm` in both wrappers (one line each), or make `arm` required.

## Validation

**Setup:** my own target `/work/scratch/ch2b-b4-r2-opus/target`, with `RUSTC_WRAPPER=` (no sccache), `-j 8`, `umask 077`, and `TMPDIR` under the scratch dir. The box was at load 60–157.

**Baseline at `d1f4951a6`** (`cargo test -p dashd-chainstate --all-features --lib`, filtered):

| Run | Result |
|---|---|
| `node_stores` | 26 passed |
| `flush::control::tests` | 35 passed |
| `store::blocks` | 1 passed |
| `a_flush_in_the_in_hold_drain_leaves_the_drain_awake`, repeated | 40/40 |
| `prune_and_flush_arms_the_check_and_returns_with_the_files_unlinked`, repeated | 20/20 |
| `--test crash_planted -- lost` | 6 passed, including `a_blk_file_lost_to_a_failed_dir_sync_refuses_the_restart` |

**Hand mutants:** applied to a reflinked copy of the tree (`/work/scratch/ch2b-b4-r2-opus/mut`, with its own target), never the review worktree. The script is `mutants.sh`; the copy and its target are deleted, and `target/` (5.7 GB) is kept for reruns. Results are in the M-1 table above. In summary:
- both M-1 fixes reverted: killed 3/3;
- either fix reverted alone: survives 10/10 each, as D81 says;
- the pump removed: killed 3/3.

**Hygiene:**
- `git diff --check b994a05fe..d1f4951a6` is clean.
- The review worktree is clean at `d1f4951a6`.
- I made no commits and no pushes, killed no processes, and did not rerun GATE-FAST or the CRASH smoke. I took those from the owner's report: GATE-FAST r7 `GATE EXIT 0`, CRASH 39/69, smoke seeds 67/71.
