# CH1-node N7b-2 — Opus-h round 1

Scope: `f1c695664` and `3b55ebbea`, plus the `node-fuzz` conflict resolution in `288c2d996`, reviewed in a detached worktree at `288c2d996`. CH2b b3 is judged only where V's use of it matters. I did not repeat Sol r1's trace. This review looks for what that trace could miss.

## Verdict

**ACCEPT WITH NITS.** V's code is correct. I found no V wait that should answer a `FlushDue` and doesn't, and none that answers where it must not. No wait sits between a connector's commit and its tip step. The two minors below are about a contract that N7b-2 makes load-bearing and about a hand-off that N7b-2 left unanswered. Neither needs a code change in V.

## Findings

### m-1 (minor, contract and test gap): V's drain park has no timeout, so N7b-2 makes "W's `Released` rings V's bell" a liveness requirement, and nothing on V's side tests it

`dashd-chainstate/src/chain/abc.rs:263-265` parks with `self.wake.park()`, which has no timeout (`dash-chain/src/notify.rs:77`). After 3b55ebbea, D57 relies on b3's D24 for one case: a site takes `FlushDue` into its decision (`dmax_due`, `flush/control.rs:425`) while a job is in flight. The hub's flag is then clear, the policy keeps the cause, and only `answer_due`'s `poll` at a later wait freezes it (`flush/control.rs:370-376`, `:213-222`). That poll runs only if something wakes V after the release. The only candidate is W's `DriverEvent::Released` (`flush/driver.rs:36`), which reaches V's bell only through adapter item 1 (D47).

b3's own evidence does not cover V's park. The threaded watchdog drives `FlushControl::backpressure_wait` with `park_timeout(1 ms)`, and its docs say "W's release rings no bell on the seam … so V's backpressure park polls W's events every millisecond" (`subscriber/watchdog_tests.rs:12-14`, `:515-518`; CH2b D25). V's drain does not go through `backpressure_wait` (D57 already says so for X15) and has no poll. The model W does not cover this either: `ModelW::release` (`chain/tests/flush.rs:157-160`) rings nothing, and no N7b-2 test raises a `FlushDue` while a job is in flight. The D24 amendment is the reason for 3b55ebbea, and it has no V-side test.

**Failing schedule (if the production adapter rings for `PruneClears`, `Failed` and `Crashed` but not for `Released`, as b3's seam does):**
1. A job J is in flight.
2. Synced subscriber S reaches `D_max` and calls `post_flush_due()`: the flag is set and the bell rings.
3. V's ConnectTip for block k runs `at_site(IF_NEEDED)`. `decide` takes the flag (`dmax_due`), and J is in flight, so the cause is kept and the flag is clear.
4. `hold.connect_tip(k)` fans k out, and S's queue is full: `retry_pending` returns `Blocked`. `answer_due` polls, J is not yet released and the flag is clear, so nothing freezes. The park consumes step 2's stale permit, and the loop repeats and parks again.
5. W releases J. It is reported to the driver's own parker (`flush/driver.rs:290-297`), not V's bell.
6. V stays parked in the hold. S's `FlushDue` was taken in step 3, and S waits for a flush that only V's next `poll` would start. Neither side moves, and no latch trips: a permanent hang at the tip lock. This is the same hang D24 fixed in `backpressure_wait`, moved into V's drain.

Every V wait reached through the bell has the same dependency (serve's select, a release point, `VWait`). Only the drain one is a hard deadlock: at the others, the next command or block reaches a site that polls.

**Ask:**
- D57 (and D47 item 1, or the D13 adapter row) states that every `DriverEvent`, `Released` included, must ring V's `FlushBell`. Without it V's untimed waits cannot answer a cause kept under D24, and the watchdog's 1 ms poll is not evidence for V.
- Optionally, a model-W test: set `in_flight`, raise `FlushDue` so a decision takes it, then hold V in a drain, and have `release()` ring. Assert that the freeze lands at the stepped pair. That pins V's side of D24.

### m-2 (minor, hand-off for the manager): CH2b b3 D8 assigns "wiring `SubscriberHub::on_disconnect_tip` into V's DisconnectTip" to N7b-2; N7b-2 neither wires it nor records a deferral

CH2b-b3-notes D8 says: "Wiring the call into V's DisconnectTip log is CH1-node's (N7b-2)." `on_disconnect_tip` (`flush/hub.rs:905`) emulates Core's prune-lock moveback (`validation.cpp:3292-3301`, the BCLog::PRUNE line "%s prune lock moved back to %d"). It is the in-memory `height_first` that the T16 plant reads. V's `disconnect_tip` (`chain/abc.rs:600-601`) still says that the moveback is subsumed by CH2's LCA locks, as CH1-node §4.5 and Q12 say. The production caller is only CRASH's harness (`crash/chain.rs:680`). The two designs disagree, and D57–D59 don't mention it.

**Failing schedule:** on the node, a reorg that disconnects block h never lowers any subscriber's Core-side lock or logs the moveback. With `-debug=prune`, Core's line is missing, and T16 is unreachable through V.

Core's call sits between `DisconnectBlock`'s commit and the IF_NEEDED site, which is V's `disconnect_block` → `at_site` gap. V holds no hub, so the wiring needs a seam (for example a `FlushSite` hook with a no-op default, called with `pindexDelete->nHeight - 1` before `at_site`) or an adapter-side call. **Ask:** a D-row that either wires it, or rules it out of scope or into the adapter (D13), and corrects b3 D8 to match.

### n-1 (nit, test): the hold test can pass with the freeze made by step 2's decision rather than by (d)

In `a_flush_due_during_a_hold_is_answered_at_the_stepped_tip` (`chain/tests/flush.rs:1448-1480`), W calls `flush_due()` as soon as S reports the first step's event. V may still be between step 1's drain and step 2's `at_site`. In that schedule step 2's decision takes the due, and `after_step` freezes it at the same `(genesis, seq₂)` pair (`:289-296`). The drain's `answer_due` then records `Some(0)` but freezes nothing. The test still kills mutant D in both schedules: through the 120 s wait when V is already parked, and through `dues.first() == Some(Some(0))` otherwise. Even so, the freeze it attributes to (d) is sometimes the decision's.

**Fix:** in the `act` closure, first `w.wait_for(|m| !m.dues.is_empty())` (V's first drain answer, so V is parked in the drain), then `flush_due()`. That makes (d) the only path that can freeze it.

## What I checked and found sound

- **Wait coverage.** I enumerated every blocking point on V's thread: `Lanes::recv` (`actor.rs:106`), `VWait::wait` (`:707`), `release_point` (`:1105`), the in-hold park (`abc.rs:265`), and the `FlushSite` calls that block inside W (`at_site`'s `wait_release`, crash-ratio completion, `stop`). The only other wait loop on a fanout is SB5's `activate_segmented` (`dash-chain/src/chain/sb5/segmented.rs:121`), which is SPV-only (`require_mode(Spv)`) and unreachable from V. SB4's `enforce_block` makes no wait of its own: its fanout comes only from V's `TipHost::disconnect_tip` (`locks.rs:403-425`), whose drain runs under `Steps::Enforce`. The node's only fanout entry points are `feed::after_publish` and `feed::after_step` (`dash-chain/src/feed.rs:766-799`), and neither waits. `host.tip()` and `prune_check_height()` there are lock-free (`chain.rs:1998`, `:2365`), so `answer_due` under SB4's tip mutex cannot self-deadlock. At shutdown, `run`'s `service` and `stop` and `attach_coins` correctly keep `service` (D58). A restart has a tip before init's ABC (`load_chain_tip`); an idle-select answer there freezes at `(coins best block, last seq)`, which is consistent.
- **Ordering.** On both connect and disconnect, `answer_due` cannot run between the commit and the tip step. The only code in that gap is `at_site`, the mempool sinks and the step itself. A `FlushDue` raised there (for example by a subscriber reacting to the step's fanout) is taken by the next decision or the next wait, always at a consistent pair. On the disconnect path the freeze precedes InvalidateBlock's failure mark and SB4's conflict marks, as Core's IF_NEEDED in DisconnectTip does (`validation.cpp:3304` and `:4096-4123`). Under D24, a cause kept by a site is frozen at or after that site's post-step pair, never before its commit. This matches what Core flushes synchronously at the site.
- **Reentrancy.** `FlushBell::ring` is a `try_send` and a notify. Under crash ratio, `answer_due`'s wait reaches only `host.retry_pending()`, which is the hold's own retry. Nothing waits on V.
- **D57 notes.** Both are correct: b3's `answer_due` has no tip guard (`FlushCx::tip` is required, so the adapter must keep item 6's guard), and V's drain bypasses `backpressure_wait` and X15. See m-1 for what follows from the second.
- **D59.** I agree that m-D and m-F belong to `SubscriberHub`. V holds no hub, opens no subscriber and steps no driver, so neither is reachable through V's code.
- **Tests.** (a), (b), (c), the commit test, no-tip, stop and attach are deterministic. `rig.accept` returns only after a later query, so V is idle. (c) raises the due inside the command, before `limit_queue`, with no command in the lanes. The only timing in them is the 120 s failure bound.
- **Merge 288c2d996.** Merging the model, target and bin names of both parents gives exactly the merge's lists: 50 models, 76 targets, 78 bins, with no name lost. `coins_replay_blind`, `chainstate_crash` and `connman_sched` are each in `models.rs`, `targets.toml`, `Cargo.toml` and `fuzz_targets/`.

## What I ran

- `cargo test -p dashd-chainstate` at 288c2d996. `CARGO_TARGET_DIR` and `TMPDIR` were under `/work/scratch/ch1-n7b2-r1-opus`, the target reflink-seeded from the owner's and every source touched, with the default sccache wrapper. Result: **EXIT 0**. Lib 470 passed, in 2350 s at load ~260, including all seven N7b-2 tests. Integration and doc suites all passed, with 2 ignored.
- The owner's mutants, adapted to a second detached worktree and target. All 4 I ran were **CAUGHT**:
  - A-vwait, by the 120 s wait in `await_due_freezes`;
  - C-release, by `flush.rs:1410` (wrong pair);
  - D-drain, by the 120 s wait (V parked in the drain);
  - G-commit, by `flush.rs:1537` (frozen at the parent).

  I didn't run B, E, E2 or F.
- `node-fuzz` registrations compared by name across both merge parents and the merge.

Worktrees and targets deleted afterwards.
