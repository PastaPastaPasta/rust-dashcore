# CH1-node A-1b slice (DEC-273, DEC-278): review round 1 (Opus)

- **Range:** `909087861..cec322c33` on `node/ch1-node-a1b`. Commits: aa1a3babc (§8.4 text), f8f00b12a (D67 signatures), 17606cb20 (implementation), cec322c33 (simplifier).
- **Read:** CH1-node.md §8.4, CH1-node-notes D66/D67 (R1–R7), P3 D111 at 9e305f41c, and DECISIONS DEC-273/DEC-278.
- **Core:** v24.0.0-rc.3 (`da567d7db`). I read `validation.cpp` 3716-3770 (`LimitValidationInterfaceQueue`, ABC loop head, `m_chainstate_mutex` at 3737-3738), 3930-3945 (`InvalidateBlock` loop head), 4324-4460 (`CheckMerkleRoot`, `CheckBlock`, `IsBlockMutated`), 4824-4850 (`AcceptBlock` → `NewPoWValidBlock`), 4875-4905 (`ProcessNewBlock`), and `validationinterface.cpp` 255-270.
- **Ran:** everything on a `git archive` copy under `/work/scratch/ch1-a1b-review`.
  - `cargo test -p dashd-chainstate --lib chain::`: 293 passed.
  - `--test signal_wait_model`: 2 passed, in 24 s.
  - 14 crate mutants and 7 model mutants (tables at the end).

## Verdict: CHANGES

There is one blocking item: R2 has no test (B1). It is cheap to fix.

The core mechanism is sound:
- the shared `VWait` loop;
- C5′'s whole-command deferral and its rerun through `handle`;
- the idle-point skip;
- R4's order;
- the C8 stamp;
- C8's site against Core.

I found no deadlock or lost wakeup in V's code. The nits are mostly contract text P3 S6b will code against (N2, N3), a robustness guard (N1), and model and test coverage (N5, N6).

## Blocking

### B1. R2 (held commands served first inside a `VWait`) has no test. M1 and M16 both survive.

- **Where:** `actor.rs:897-904`, `serve`'s held-first block, and `actor.rs:1508-1513`, `idle_point`'s held skip.
- **What is untested:** R2 is one of the seven refinements P3 accepted (D111 R2). It is the only thing that keeps D38's order once commands wait inside the new waits. No test stages a held command and then makes V wait.
  - `no_hook_while_commands_wait_for_startups_abc` covers `holding`. It does not cover `held` non-empty with `holding` false.
- **Mutants:**
  - **M1** removes the held-first block. Every test passes. Held commands then wait behind lane commands until the wait ends. That is exactly the overtaking R2 exists to prevent: "a lane command would overtake an older held one".
  - **M16** dispatches held commands with `at_release = false` inside a wait. Every test passes.
    - A held class C command (`InvalidateBlock`, `ClEnforce`, an RPC `ActivateBestChain`) would then run `composite` inside the wait.
    - Its `limit_queue` would reach `wait_with`'s `debug_assert!(!self.in_wait)` and panic in debug builds.
    - In release builds the waits would nest, which §8.4 forbids.
  - **M2** (the idle point ignores `held`) also survives. Its harm is smaller: the idle wait then takes from `held`, against R2's "the idle mode never takes from held". The order is still kept, because `serve` takes held first.
- **Reachable:** yes, though rarely. The startup hold (DEC-196 (2)) has to end with commands held. `handle(ActivateBestChain)` clears `holding` only after the composite returns. Then `next()` takes `held[0]`. If that is a `ProcessBlock` outside IBD, `before_accept` opens an idle-mode `VWait`, and `serve`'s top takes the rest of `held` from there.
- **Fix:** add one directed test. With `stage_held` or the restart rig under a startup hold, stage these held commands:
  - (a) a `ProcessBlock` outside IBD, with the `Gates` accept latch closed;
  - (b) a query X;
  - (c) an `InvalidateBlock` or `ActivateBestChain`.

  While (a) waits, send a lane query Y. Then assert:
  - X is served before Y;
  - (c)'s reply arrives only after the latch opens;
  - nothing aborts.

  M1 fails the first assertion. M16 fails the last two, through the debug panic. Add the R2 idle-point skip (M2) to D67's mutant record: either as equivalent, with the reason above, or killed by the same test with a release-hook counter.

## Nits

### N1. `VWait::serve` keeps serving after a fatal, and a second fatal replaces the first.
- **Where:** `actor.rs:889-896`, and the three `self.fatal = Some(f)` sites at `actor.rs:900`, `:932` and `:953`.
- **How it happens:** after a served command goes `Fatal`, `serve` returns `Err(Disconnected)`. Nothing stops the listener from calling `wait`/`wait_until` again on the same `VWait`. That could be a retry loop, or a second wait in the same hook. `ValidationSignals::release_point` also hands the same `wait` to the next listener if the first one swallowed the error.
- **What goes wrong:** V then serves more commands after an `AbortNode`, and the second `Fatal` overwrites the first.
  - `stop_serving` branches on `f.cause()` (D62: only `AbortNode` runs `stop_flush_at_exit(true)`).
  - So an `Assert` cause replaced by an `AbortNode` one changes V's stop path.
  - The report also names the wrong failure.
- **What already limits it:** `wait_with` returns `wait.fatal` whatever the hook returned, so V does stop afterwards. The exposure is the serving in between.
- **Fix:** add `if self.fatal.is_some() { return Err(WaitError::Disconnected); }` at the top of `serve`, and keep the first fatal (`self.fatal.get_or_insert(f)`). Also state on `release_point`/`before_accept` that a listener must return the wait's `Err` and must not wait again after it.

### N2. The contract text does not say that a listener's other callbacks are re-entered from inside its own hook.
- **Where:** `signals.rs:99-115` (hook docs), `CH1-node.md:1043` ("What it serves"), and D67's `VWait::wait_until` bullets.
- **What happens:** inside `release_point`/`before_accept`, V serves class M commands on V's own thread. Their inline signals run on the same stack as the listener's hook, before `wait_until` returns:
  - `ProcessHeaders` → `accepted_block_header` and `notify_header_tip`;
  - an IBD `ProcessBlock`'s rejection → `block_checked` (`accept.rs:156`);
  - `notify_header_tip` (`accept.rs:166`).
- **The hazard:** a listener that holds a non-reentrant lock across `wait_until` (for example its queue mutex, kept for `done`) and takes the same lock in `block_checked` deadlocks V's thread against itself.
- **Fix:** one sentence in §8.4 and the trait docs:
  - V may call any of the listener's inline callbacks while it waits in a hook;
  - the listener must hold no lock across `wait`/`wait_until`;
  - `done` must take and release its own locks.
- **Worth stating as a positive:** `NewPoWValidBlock` is never re-entered this way. Outside IBD a `ProcessBlock` met in a wait is deferred whole (R3), and in IBD C8 does not fire.

### N3. `wait_until`'s `wake` contract is too loose to rule out a lost wakeup.
- **Where:** `signals.rs:222-230`. The doc says only "`wake` rings when `done` may have changed".
- **Why V itself is fine:** V checks `done()` before every select (`over`) and after every ring (`received` → `continue`).
- **Where wakeups can still be lost:**
  - **A rendezvous channel.** `bounded(0)` with `try_send` loses a ring sent between V's `done()` check and its select.
  - **A shared receiver.** If another consumer reads the same receiver (crossbeam's receivers are MPMC), it can steal V's ring. P3 D111 plans to feed `wake` from the adapter's `on_notify`/`WaitEvent`, which may have other readers.
- **Fix:** state three requirements:
  - `wake` is buffered (capacity ≥ 1);
  - it is rung after the change `done` reads is visible;
  - V is its only consumer while it waits.

  Dropping its sender is how "closed" is signalled (Err unless `done()`).

### N4. `register` stores the flag after the list, which leaves a real window where hooks skip a listener that already receives signals.
- **Where:** `signals.rs:443-444`.
- **The window:** the registering thread can be descheduled between `listeners.store` and `registered.store(true)`. In that window V's `each` already delivers inline signals to the new listener, for example P3's `block_checked` queueing an entry. But `has_listeners()` still reads `false`. So a `ProcessBlock` skips `before_accept` and C8. That breaks P3's I5 for that accept, and P3 misses its `NewPoWValidBlock`.
- **How likely:** the window is narrow and needs P3's registration (ND1 step 12) to coincide with block processing.
- **Fix:** store `registered = true` before the list in `register`. A Relaxed store sequenced before ArcSwap's release store is then visible to any V that loaded the new list. Keep `unregister`'s order: a stale `true` costs one empty iteration.
- See Q6 for why Relaxed is otherwise fine.

### N5. The model checks the deferral, but not the serving, the PreStart exemption, the idle point or the hold.
- **Where:** `tests/signal_wait_model.rs`. Details in Q7. MM10, MM9, MM4 and MM11 all survive.
- **Fix:**
  - **Serving and the PreStart init edge.** Give the actor a step that sends a class M command (P3's `ProcessHeaders`) and awaits its reply without draining. Add an init step: the actor starts, and the queue leaves `PreStart`, only after V replies to init's ABC. With those, MM10 and MM9 must fail.
  - **Targeted negative controls.** Make MM1 (accept inside a wait outside IBD) and MM2 (no C5′ wait) explicit controls. They fail the I5 invariant, and they show the checker sees C5′, not just the generic cycle.
  - **Record the rest.** Note the idle point (MM4) and the hold exemption (MM11) as not liveness-bearing in this model, with their real reason: R2/D38 order for the hold, P3's bound between commands for the idle point.

### N6. Smaller test gaps (D67 lists the tests the slice adds).
- **R4's order is not pinned (M5 survives).** P3 D111 says the order does not matter for liveness, so a test that records barrier-then-hook order is enough: `callbacks_pending() > 10`, with a listener that logs whether the barrier was reached.
- **The closed-wake path is untested (M9 survives).** `wait_until` should return `Err(Disconnected)` once `wake`'s sender is gone with `done()` false, and `Ok` when `done()` is true. That path is how "closed" reaches V if P3 drops its sender. Add two unit tests, on `Blocking` and on V through a listener that drops its sender.
- **C8's IBD skip is not pinned (M26 survives the whole `chain::` suite).** The guard predates the slice, but §8.4 restates it. `no_accept_wait_in_ibd` can assert that no `Ev::PowValid` was recorded. The tip guard is already pinned by `blocks::new_pow_valid_block_only_on_the_tip` (M28 killed).
- **The nesting exemption is missing.** D67's test list names it, but `queue_hooks.rs` has none. A `ProcessBlock` reaching V inside the signals' barrier wait at `v:3760` should be deferred whole (R3's "that includes the signals' barrier wait"). Today only the idle-mode `before_accept` wait is tested (`an_accept_outside_ibd_waits_and_defers_the_next_one`).

### N7. The idle and composite modes behave the same apart from the `debug_assert` (M6 is equivalent).
- **Where:** `actor.rs:829-831`, `:940-946` and `:1537`.
- **Why they are the same:** in composite mode a served command is followed by `release_point()`. That polls the bell and dispatches everything already queued, all with `at_release = true`. The `serve` loop does the same one select at a time. It checks the bell, takes held first and checks `stop` and `over()` between commands.
- **The only differences:**
  - `release_point`'s `debug_assert!(in_composite)`, which idle mode exists to avoid;
  - composite mode keeps draining after `done()` has turned true.
- **Fix, either way:**
  - drop `idle` and the `release_point()` call from `serve`, so both modes serve one command per iteration (simpler, and `done` is checked after each one); or
  - keep both, and record M6 as equivalent in D67 with this reason.

### N8. When V aborts after the C8 call, the stamp names a view that is never published.
- **Where:** `accept.rs:299-305`, then `:309-325`.
- **How it happens:** `save_block`'s `System error`, `received_block_transactions` or `flush_now` can return `Err(Fatal)` after the listener got `stamp = view_seq + 1`. No `scope_end` follows, and V stops.
- **The risk:** a P3 wait for "the first view ≥ stamp" (C7 (a)) must also end on V's stop, or it hangs at shutdown.
- **Fix:** one sentence in §8.4's stamp bullet. D67 already says "every caller of `accept_block` then ends in one `scope_end`", which is true of the `Ok` paths only.

### N9. The perf claim holds, but the measurement is still owed, and P3's case costs more.
- **No listener:** the claim holds (see "Hot path").
- **Owed:** D67 says `perfgate measure --label` runs on the connect and feed rows before READY. I found no result on the branch.
- **With P3 registered:** P3 D111 asks for PF0 rows with its listener registered. That case costs more per serve iteration:
  - an `ArcSwap::load_full` (refcount increment and decrement);
  - a `VWait` and a virtual call per listener at the idle point;
  - per `ProcessBlock`, one more `is_ibd()` (a clock read) and the `before_accept` chain.

  It is probably noise against a connect, but it is the case PF0 will see.

## The implementer's questions

### Q1. The shared `VWait`/`Until` loop: disconnect against `done`, and the fatal path.

The order is correct.
- **Each iteration (`actor.rs:889-956`):**
  1. `until.over()`: the barrier's `poll`, or `done()`;
  2. V's `stop`;
  3. held first;
  4. with the lanes and the bell gone, a blocking `recv` on the waiter's receiver alone;
  5. otherwise a select over receiver, bell and live lanes.
- **Cond waits never report `Disconnected` while `done()` holds.** A disconnected `wake` is mapped through `received`, which runs `done()` before returning `Err`. A ring returns `None`, so the loop rechecks `over()`. So `done()` is checked:
  - before the first select;
  - after every ring;
  - after every served command.
- **The barrier path is the old behaviour.** It returns `Ok` on the token and `Disconnected` when the signals thread drops the sender.
- **`done()` before `stop`.** When both hold, the hook returns `Ok`, and V reads `stop` at its next check:
  - `queue_hook` skips at once;
  - `serve`'s loop head returns.
- **A `before_accept` that ends because of `stop` still runs its accept.** `wait_with` returns `Ok` on `Disconnected`. That matches Core: a `ProcessNewBlock` already started finishes `AcceptBlock`, and its ABC stops at `ShutdownRequested` (`v:3841`).
- **The fatal path.** A served command's `Fatal` and `answer_flush`'s are stored, and `Disconnected` is returned. `wait_with` (`actor.rs:1544-1548`) returns the fatal even if the listener swallowed the error, so V stops on every path:
  - `limit_queue`'s `?`;
  - `handle`'s `?`, which drops the `ProcessBlock` so its reply disconnects after `stop_serving`;
  - `serve`'s `Err(f) => Err(f)`.

  The gap is serving after the fatal if the hook waits again (N1).
- **No spin.** A disconnected `wake` is always ready, but the first such receipt returns.

### Q2. C5′: deferring the accept whole under `in_wait`, and coming back through `handle`.

Both are correct.
- **The deferral.** `handle` (`actor.rs:1236-1245`) defers before anything of the command runs: no `CheckBlock` and no `accept_header`. So the accept is one piece, and its reply waits with it.
- **The rerun.** `run_deferred` → `handle(cmd, false)` from the serve loop, where `in_wait` is false, and with `in_composite` false. It re-evaluates `has_listeners() && !is_ibd()` and runs `before_accept` in idle mode before the accept. M3 (no deferral) is killed.
- **The rerun cannot skip the hook.** The IBD latch never returns to true, so a deferred block that met C5′ meets it again. `holding` cannot be set by then.
- **One schedule worth knowing, also Core-reachable:**
  1. PB1, from sender A, is deferred at an ABC loop head's barrier wait.
  2. PB2, from sender B, arrives during the next step.
  3. PB2 is served at the next loop head's plain `release_point()`. Its `before_accept` there runs in composite mode, and PB2 is accepted inside the composite.
  4. PB1 is accepted only after the composite.

  This crosses senders only: R7/D111 give one outstanding `ProcessBlock` per sender. Core's unfair `cs_main` gives the same order (D111 R3 (b)). No change is needed.

### Q3. Skipping the idle point while commands are held: correct? Can it starve `next()`?

It is correct, and it cannot starve `next()`.
- **When the skip applies.** `idle_point` skips only when `held` is non-empty.
- **Why `next()` keeps moving.**
  - With `holding` false, `next()` pops `held` first (`actor.rs:1094-1099`), so every iteration shrinks `held` by one. The idle hook comes back once `held` is empty.
  - With `holding` true, `queue_hook` would skip anyway.
- **What each held command still meets.**
  - A held `ProcessBlock` outside IBD still meets `before_accept` (R2's second half).
  - A held class C command meets C5 at its own loop heads.
- **The bound still holds.** P3's queue bound needs no idle wait between held commands: what they push is bounded by the number held, plus the loop-head waits.
- **When the hook does wait, nothing starves:**
  - the wait serves both lanes and W's bell;
  - only class C work and outside-IBD `ProcessBlock`s are deferred, and they run, before the next idle point, as soon as it ends: `Ok(()) if !self.deferred.is_empty() || self.stop`.
- **The no-wait path is cheap.** With `done()` already true, the idle point costs one `VWait` and returns.
- B1 covers the missing test.

### Q4. At both `limit_queue` sites, is the barrier wait strictly before `release_point`?

Yes.
- **The order.** `limit_queue` (`actor.rs:1497-1504`) completes `wait_with(signals.limit_queue)` before `queue_hook(Release)` starts. Both run outside any hold.
- **Its two callers.** Both are loop heads: `abc.rs:871-872` (`v:3760`) and `failure.rs:148-149` (`v:3940`). There, V's own `release_point()` comes first, then the queue limit, then the hook.
- **When the hook is skipped and when it runs.**
  - If the barrier wait served a `Shutdown`, `stop` is set and the hook is skipped.
  - If the signals thread stopped (`Disconnected`), the hook still runs. P3's queue closure or V's `Shutdown` ends it.
- The order is not pinned by a test (M5 survives, N6). D111 R4 calls the order immaterial to liveness.

### Q5. Is the C8 stamp `view_seq + 1` exact?

Yes.
- **Who moves `view_seq`.** It changes only in `publish_view` (`actor.rs:1640-1641`). Only two places call that:
  - `scope_end` (`actor.rs:1631`);
  - `after_release` (`abc.rs:831`), which accept never reaches.
- **Nothing publishes between the call and the scope end.** After the call, `accept_block` runs `save_block`, `received_block_transactions`, `flush_now(None)` and `check_block_index`, and none of them publishes.
- **Every `Ok` path of `process_block_accept` ends in exactly one `scope_end`:**
  - the rejected path at `accept.rs:159`, including `save_block`'s `Ok(None)` after the call;
  - the accepted path at `accept.rs:165`.

  `load_devnet_genesis` is IBD, so C8 does not fire there.
- **Tested.** M24 (stamp = `view_seq`) is killed by `new_block_carries_the_stamp_and_the_in_scope_tree`.
- **The only inexact case** is an `AbortNode` after the call (N8).

### Q6. Relaxed `has_listeners`: a concurrent registration can miss a signal. Acceptable against Core?

Yes, with N4.
- **Core's behaviour.** Core's `MainSignalsImpl::Iterate` copies the list under `m_mutex` (`validationinterface.cpp`). A registration is seen if its lock section precedes the copy and missed otherwise. A truly concurrent registration may miss a signal in Core too.
- **A registration ordered before V's check is always seen.** For example: register, then send V a command. The relaxed store is sequenced before the release (the channel send). Once V acquires through the receive, coherence forces its load to see it.
- **Register and unregister are serialised by `edit`,** so `registered` cannot end in a state that contradicts the list.
- **A stale read either way is otherwise harmless:**
  - a stale `true` costs one empty iteration;
  - a stale `false` for a concurrent registration is Core's own race.
- **The one non-Core artefact** is the window in N4, where a listener already receives signals but the hooks skip it. Fix it by storing the flag first.

### Q7. Does `signal_wait_model.rs` faithfully model V's serve and defer behaviour? Is the negative control meaningful?

- **The deferral: yes.** It models:
  - the whole deferral inside every wait outside IBD (MM1, an accept inside a wait, fails I5);
  - the C5′ wait before an accept from the idle point and at a release point inside a tail (MM2 fails I5);
  - R4's loop-head wait (MM3 breaks the bound and the control).
- **The `Tail { accept: Some }` state matches** the composite-mode `before_accept` that a plain release point runs.
- **The serving: no.** No modeled agent depends on being served:
  - RPC senders block without being needed by anyone;
  - the actor's only wait drains.

  So MM10 (waits serve nothing) survives. In the real system V must serve, because the actor awaits class M replies (`ProcessHeaders`, D111 R2) without draining.
- **Three more design points are unconstrained:**
  - **PreStart (MM9 survives).** The model has no init ordering: the actor is always present, and its first drain makes the queue `Live`.
  - **The idle point (MM4 survives).** No property depends on it.
  - **The hold (MM11 survives, hooks during the hold).**
- **The negative control** (an actor that does not drain) is meaningful as a check that AG EF detects a real cycle: it fails at `ibd` = false and true. But it is generic: it would deadlock any design with a bounded wait. It says nothing about C5′ or R3. See N5 for targeted controls.
- **Smaller abstractions, both fine:**
  - the signals' own barrier wait is not modeled. P3 D111 R4 says the signals thread never needs the actor, so this is safe.
  - R2's held-first is not modeled: hold commands stay in the lane.

## Core ordering

- **C8's site matches `v:4830-4832`.**
  - **The guard.** `!IsInitialBlockDownload() && ActiveTip() == pindex->pprev`. V evaluates `is_ibd()` first, so the latch side effect happens as in Core, and adds `has_listeners()` last.
  - **The order around it.** The call is after `ContextualCheckBlock` (`v:4819-4826`). It is before `*fNewBlock = true`, `SaveBlockToDisk` and `ReceivedBlockTransactions` (`v:4834-4842`).
  - **What the listener sees.** The entry has no `HAVE_DATA`, as Core's `pindex` does at that point. V's tip is the parent, and the tip moves only in the ABC tail, after the accept's `scope_end` and the `Activated` wait. Core's `NewPoWValidBlock` is synchronous (`validationinterface.cpp:264-267`), as V's is.
- **`LimitValidationInterfaceQueue` (`v:3716-3722`)** sits at both of Core's sites: ABC's loop head at `v:3760`, inside `m_chainstate_mutex` and outside `cs_main`, and `InvalidateBlock`'s at `v:3940`. C5's hook after it has no Core counterpart. That addition is P3's (DEC-229).
- **A-4b matches Core.**
  - Core's `CheckMerkleRoot` returns early on `m_checked_merkle_root` (`v:4326`).
  - `CheckBlock` calls it only under `fCheckMerkleRoot` (`v:4384`), and sets `fChecked` only when both flags are true (`v:4424`).
  - V passes `merkle = false` to `dash_consensus::block::check_block`, which skips only `check_merkle_root` (`dash-consensus/src/block.rs:127-128`). It then gives `accept_block` and the tail `checked = true`, the same result as Core's `fChecked`.
  - `merkle_checked_skips_only_the_merkle_pass` discriminates the two cases (`Mutated` against `bad-cb-multiple`).
- **BL-236.**
  - `contains`/`at` read `view.snap.active`.
  - `contains` takes the entry's height from the live reader, which is resident-only. Node mode evicts nothing (D111 R6), so that is exact.
  - `contains_and_at_answer_at_the_view` covers both reads.

## Hot path with no listener

The claim holds: one relaxed load per hook site.
- **The idle point:** `held.is_empty()` and one load per serve iteration.
- **`handle`:** a discriminant compare and one load per command. `is_ibd()` is evaluated only after a `true` load.
- **The accept:** one load after the existing `is_ibd()` and tip compare.
- **`limit_queue`:** one load after the existing queue limit.
- **What the slice removes:** the old unconditional `new_pow_valid_block` iteration, an `ArcSwap` load per accept outside IBD.
- See N9 for the measurement still owed and the cost with P3 registered.

## Mutants

All runs used `chain::tests::{queue_hooks,actor,signals,failure,flush,verify}` on the branch head.

| # | Mutation | Result |
|---|---|---|
| M1 | `serve` does not take `held` first (R2) | **survived** (B1) |
| M2 | `idle_point` ignores `held` | survived (B1: near-equivalent) |
| M3 | no whole deferral inside a wait (R3) | killed: `an_accept_outside_ibd_waits_and_defers_the_next_one` |
| M5 | R4 order swapped | survived (N6) |
| M6 | composite-mode waits never call `release_point` | survived: equivalent (N7) |
| M9 | a gone `wake` ends `Ok` | survived (N6) |
| M16 | held commands dispatched with `at_release = false` in a wait | **survived** (B1) |
| M21 | `queue_hook` ignores `stop` | survived: equivalent, because `serve` returns at `stop` before serving |
| M23 | C5′ ignores IBD | killed (hang: `no_accept_wait_in_ibd`'s closed gate) |
| M24 | stamp = `view_seq` | killed: `new_block_carries_the_stamp_and_the_in_scope_tree` |
| M25 | `Cond` checks `done()` only after a ring | killed (hang: an open latch with no ring never ends) |
| M26 | C8 fires in IBD | **survived**, also against the whole `chain::` suite (N6) |
| M27 | no idle point | killed: `the_idle_point_serves_r_and_defers_c`, `no_hook_while_commands_wait_for_startups_abc` |
| M28 | C8 ignores `tip == e.prev` | killed: `blocks::new_pow_valid_block_only_on_the_tip` (existing test) |

Model mutants (`signal_wait_model.rs`, compiled standalone):

| # | Mutation | Result |
|---|---|---|
| MM1 | a block met in a wait is accepted outside IBD | killed (I5) |
| MM2 | no C5′ wait at the idle point | killed (I5) |
| MM3 | no C5 wait at the loop head | killed (bound; the control also fails) |
| MM4 | no idle-point wait | survived (N5) |
| MM9 | no PreStart exemption | survived (N5) |
| MM10 | waits serve nothing | survived (N5) |
| MM11 | C5 waits during the startup hold | survived (N5) |
