# BL-271 r1 review (Opus)

Branch `node/nd1-bl271` at c5421b04a (base bfec24a81, 11 commits). Core reference: v24.0.0-rc.4
49caf372bb, read from the local checkout `~/workspace/dash` (`git show 49caf372bb:<path>`), so
the Core citations below are verified against the pin.

**Verdict: ACCEPT-WITH-NITS** (0 M / 2 m / 3 n)

**CH1-node sign-off (db4cba2a2, 0a122433c, 310d74903, ced823383): acceptable as CH1-node code.**
- Actor boundary: V still owns its flush side for its whole life. It only leaves V by value in
  `VExit`, after `serve` has returned and `stop_flush_at_exit` has drained W (D45 order kept).
  It leaves only when the latch is open and the stop was clean or an `AbortNode`. On a panic, an
  assert (D62a) or a latched failure it still drops on V's thread (`actor.rs:1222`), so Sol r1
  N2's "W's panic reaches V's joiner" holds. No V-side path can run a flush after `VExit` is
  built, so there is never a second writer.
- API shape: `VExit::force_flush(deliver)` is the one new public entry. `VParts::devnet_pending()`
  is a read-only getter over a `pub(crate)` flag. `FlushHost` stays `pub(crate)`, and the
  private fields (`flush`, `reader`, `signals`, `clock`, `loading`, `abort`) don't widen the
  surface. `deliver: bool` is a little opaque at call sites (n-1), but that's not a blocker.
- Coupling: dashd-chainstate gains no dependency on dashd-node, and no dashd-node type appears in
  it. The doc comments cite ND1 decisions (2b-D3, DEC-613) but nothing in the code depends on them.
- The race fix (0a122433c) is right: on a latched failure, `drop(flush)` joins W before
  `force_flush` returns, so W's `AbortNode` lands in `NodeAbort.reported` before ND1 reads
  `report(n)`. `catch_unwind` around the drop keeps a W panic from escaping into ND1's shutdown,
  matching how V's join swallows it.

## Findings

### m-1: mutant "fresh devnet on the regular path" isn't equivalent

`dashd-node/src/steps.rs:306-312`. Core: `node/chainstate.cpp:210` (`LoadGenesisBlock`, whose
devnet step `validation.cpp:5408-5416` writes the devnet genesis block) runs before
`InitCoinsDB` at `:233`.

If a fresh devnet takes the regular path, V's devnet step (`accept.rs:211` `load_devnet_genesis`,
an `AcceptBlock` that writes block 1 to `blk00000.dat`) moves to after the coins open and
recovery. On the success path the durable state and restart height are the same, which is why
the kill matrix and the restart tests can't tell the two apart. They differ when the step fails.

- **Failing input:** a fresh `-devnet=nd1` start where the block-file write of the devnet genesis
  block fails (ENOSPC or EIO on `blocks/`, or a `FlatFs` write fault in `NodeSut`). `AcceptBlock`
  raises `AbortNode`.
  - Head and Core: the failure comes before the coins exist, so `CanFlushToDisk` is false and
    there's no shutdown flush (`sut.flushes() == 0`).
  - Mutant: `self.chainstate` is already `Some` (`steps.rs:317`), so shutdown runs both flushes.
    That's `flushes() == 2`, plus any WAL-sync failure lines if the fault latched.
- **Second observable:** ND1-D6's startup page-fault ordering (PF0 `node-startup-genesis`),
  which the doc at `steps.rs:235` cites as the reason for the split.
- **Fix:** add one test that injects a block-file write fault at the devnet step of a fresh
  devnet. Assert init's failure, `flushes() == 0` and no `ForceFlushStateToDisk` line. That kills
  this mutant. Or record it as killed only by PF0, and stop calling it equivalent.
  `chain/tests.rs:516` (`an_abort_in_the_devnet_step_comes_before_init_s_error`) already has
  the fault (`NoRoomAtOne` block files), but it calls `Chain::start` directly. It never goes
  through `ChainSteps`, so it can't see where the step falls relative to the coins open. The fix
  is to run that seam through `NodeSut`.

The other two arguments I accept (see "Equivalent mutants" below).

### m-2: SB4 contract failures now get a shutdown flush on V's state; undocumented

`dashd-chainstate/src/chain/actor.rs:1214-1217`, with `seams.rs:107-111`.

`open` is true for any `Fatal` whose `cause()` is `AbortCause::AbortNode`, and that includes
`Class::Contract` (a `ChainError` from SB4, e.g. `self.hold.connect_tip(id)?` at `abc.rs:676`).
Before BL-271, a contract failure stopped V and nothing past V's last flush became durable.
After it, shutdown flush #1 freezes V's private tip and coins cache after SB4 refused a step.
`seams.rs:68-70` describes SB4's state at that point as "never carried on past".

- **Scenario:** `connect_block` commits block N+1 into the coins cache, which sets the cache's
  best block to N+1. Then `hold.connect_tip` returns a `ChainError`.
  - Head: the shutdown flush writes coins at N+1 (`CoinsCache::freeze` takes the cache's best
    block, so O6 keeps coins and best block consistent) and SB4's dirty index.
  - Restart: comes back at N+1, by `LoadChainTip` on the coins' best block.
  - Base: came back at V's last flush.
- **Core:** it has no contract failure. The nearest analogue is `ConnectTip` failing at its
  IF_NEEDED flush between `view.Flush()` and `SetTip` (`validation.cpp` `ConnectTip`). After
  that, Core's shutdown flush also writes `CoinsTip` at the new block. So the outcome is
  defensible and probably consistent, but it's a new durable-state behavior on a bug path, and
  it has no test or decision behind it. The implementer's listed residue ("no ND1 test injects
  a V AbortNode without tripping the latch") covers the `AbortNode` class but not this one.
- **Fix:** pick one of these.
  - (a) Record in the `VExit` doc and D62 that `Class::Contract` keeps the flush side, and why
    that's safe (O6, Core's ConnectTip analogue).
  - (b) Treat it like D62a: `open` only for `Class::AbortNode` (one condition at
    `actor.rs:1216`), so SB4-inconsistent state is never persisted.

  I lean (b), because it's the conservative choice for storage durability. Either way, add a
  dashd-chainstate test: a `ChainError` at the tip step, then `force_flush`, then assert the
  durable tip.

### n-1: `force_flush(deliver: bool)` is opaque at call sites

`dashd-chainstate/src/chain/actor.rs:269`, `steps.rs:492` and `:520`. `force_flush(true)` and
`force_flush(false)` don't say what the bool means. A two-variant enum, or two methods
(`first_shutdown_flush` / `second_shutdown_flush`), would make Core's order self-documenting.
Optional.

### n-2: `reports` snapshot attributes any concurrent AbortNode to the flush

`dashd-node/src/steps.rs:570-583`. `report(n)` takes the first `AbortNode` recorded since the
snapshot. During flush #1 the signals thread is still alive (`flush_background_callbacks` runs
after it, `:493-495`). A listener that calls `AbortNode` in that window would get its message
used as the flush's `state.ToString()`. No such listener exists in ND1 today, and Core's
`state.ToString()` is always the flush's own message. Optional fix: have `VExit::force_flush`
return the reporting message, or note the assumption in the comment.

### n-3: `NodeAbort.reported` keeps every message for the process lifetime

`dashd-node/src/abort.rs:21-22`. The list exists only to recover one message by index. It's
bounded in practice (AbortNode stops the node), so this is cosmetic. A `(count, Vec)` capped at
a few entries, or recording only after a `begin_flush` mark, would make the purpose explicit.
Optional.

## Core parity of shutdown (focus 1)

Checked against `init.cpp:353-458` at 49caf372bb:

- **Order:** Core joins `m_load_block` (`:354-355`), then `FlushBackgroundCallbacks` (`:360`),
  flush #1 per `CanFlushToDisk` chainstate (`:379-385`), `FlushBackgroundCallbacks` (`:389`), the
  unregisters and the Dash managers' resets (sporks, netfulfilled, metaman), the index stops,
  then flush #2 and `ResetCoinsViews` (`:449-456`). `ChainSteps::shutdown` (`steps.rs:478-523`)
  follows the same order: `chain.stop()` (V and `loadblk` joined), drain, `force_flush(true)`,
  `flush_background_callbacks`, sporks, net caches, `force_flush(false)`, then `exit = None`
  (`ResetCoinsViews`). Matches.
- **Signals:** Core's `ChainStateFlushed` is sent only when `full_flush_completed`
  (`validation.cpp:3108-3112`). Flush #2's lands in a queue that never runs again
  (`UnregisterBackgroundSignalScheduler`), so dropping it (2b-D3) is right.
  `the_first_shutdown_flush_stores_the_tip_and_signals_it` checks delivery before flush #2.
- **Failure line:** `state.ToString()` after `AbortNode(state, msg)` is `msg`
  (`validation.cpp:2245-2249` → `state.Error(msg)`). The line
  `ForceFlushStateToDisk: failed to flush state (<msg>)` (`:3128-3133`) matches
  `steps.rs:580-581`, and the AbortNode line comes before it, as in Core.
- **After an AbortNode:** Core's shutdown flushes still run (`shutdown.cpp` `StartShutdown` only
  requests a stop). Head keeps the flush side when the latch is clear, and the restart comes
  back at the stopped tip (`an_aborted_node_comes_back_at_the_tip_it_stopped_on`, plus the
  chainstate twin with a real V `Fatal`). Matches DEC-241.
- **After an assert or panic:** the flush side drops and nothing more is written. Core's process
  has ended. Matches D62a. The WAL-sync lines ND1 still prints on the latched store there are
  pre-existing (the same as base), not new.
- **Failed loads:** attach and verify failures keep `VExit` (`steps.rs:374`, `:396`), and
  shutdown's second `chain.stop()` returns `None` without overwriting it (`chain.rs:340-346`,
  `join.take()?`). Core runs both flushes once `InitCoinsCache` has run; head does the same once
  `self.chainstate` is `Some`. Matches.

## Concurrency and ownership (focus 2)

- **No double or missing flush:** `exit.flush` is `take`n on a latched failure, so the second
  flush then goes through the WAL sync on the latched store (DEC-253 form, a listed residue).
  On a non-latched failure the flush side stays and flush #2 retries, as Core's does. With the
  production `NodeFlush`, every `Fatal` is `Fatal::reported`
  (`flush/site.rs:207-209`, `Report::Done`) and comes from a store failure that has tripped the
  latch. So the non-latched branch is reachable only with test flush sites.
- **No stale state:** `VExit` is built from V's fields after the last `stop_flush`, so its chain,
  index and coins are V's final state. ND1 never flushes through any other handle.
- **`kill`:** `chain.stop()`'s `VExit` drops at once, and `self.exit = None` drops one kept from
  a failed load. Dropping the flush side joins W, which has no job in flight after `stop`, so it
  writes nothing. Matches a SIGKILL (`a_killed_node_comes_back_at_v_s_last_flush`).
- **Latch check before the exit flush (`steps.rs:572`):** correct. A latched store fails Core's
  flush with the kept LevelDB error, which the WAL-sync path reproduces.

## Equivalent mutants (focus 3)

1. **Skip flush #2: agree, equivalent.** Between the two flushes V is stopped, and nothing
   dirties the coins, the index or the block files. The listeners, sporks and net caches don't.
   Flush #1 has cleared the prune check, and ALWAYS ignores the timers. Flush #2's signal is
   dropped anyway. The only branch where flush #2 does work is a retry after a non-latched flush
   #1 failure, and `NodeFlush` can't produce one (above). The WAL sync still runs, so the
   DEC-253 lines and `flushes()` stay the same.
2. **VExit's own AbortNode report: agree, equivalent for production.**
   `f.reported_by_v(latched)` is false for every `NodeFlush` failure (`Report::Done`), so the
   report is dead code with the production site. It's live only for a site returning
   `Fatal::new` or `Fatal::abort_node`. A test-site case
   (`connector.rs`/`flush.rs` test `FlushSite`s) would kill it if someone wants it covered. That's
   optional.
3. **Fresh devnet on the regular path: disagree.** See m-1 for the input that tells them apart.

## Tests (focus 4)

- Skipping flush #1 is caught by
  `the_shutdown_flushes_on_vs_exit_make_its_tip_durable` (chainstate),
  `a_restart_keeps_its_tip_through_the_node_stores`,
  `an_aborted_node_comes_back_at_the_tip_it_stopped_on`, the CONS-11 clean twin, and the kill
  matrix.
- Reordering (flush #1 after `flush_background_callbacks`) is caught by
  `the_first_shutdown_flush_stores_the_tip_and_signals_it` (the listener count at flush #2's
  probe).
- Running the flush on the wrong state (a fresh `VExit` or a stale one): the restart-height
  assertions catch a stale one. A flush through the old WAL-only stub is caught too, since base
  comes back at b1.
- The failure-line tests check text and order (`a_failed_write_in_the_first_shutdown_flush_…`)
  and the single report (`a_failed_shutdown_flush_on_vs_exit_is_reported_once`, `a_w_panic_…`).
- Gaps: m-1's devnet-step fault, and m-2's contract-failure state.

## Checked / ran

- Read the whole `git diff bfec24a81 c5421b04a`.
  - In dashd-chainstate: `actor.rs` exit path, `stop_serving` and `stop_flush_at_exit`;
    `flush.rs` (the `FlushSite` contract, `FlushHost::lend`/`tip`); `flush/site.rs`
    (`NodeFlush` abort and error mapping); `seams.rs` (`Fatal` classes); `abc.rs` `connect_tip`;
    `coins/cache.rs` `freeze`; `accept.rs` `load_devnet_genesis`.
  - In dashd-node: `steps.rs` load, shutdown, kill and `force_flush`; `chain.rs` `Chain::stop`;
    `abort.rs`.
- Core at 49caf372bb: `init.cpp:340-470`, `validation.cpp:2245-2249`, `:2975-3134`
  (FlushStateToDisk, ForceFlushStateToDisk), `:5380-5420` (LoadGenesisBlock and its devnet
  step), `node/chainstate.cpp:179-277` (load order).
- Implementer's kill-matrix summaries under `/work/scratch/nd1-bl271/km` (head 30 in every cell,
  base 1).
- Reflink copy `/work/scratch/bl271-rv-opus`, target seeded by reflink from the implementer's,
  with `CARGO_BUILD_JOBS=12` and TMPDIR under the copy.
  - `cargo test -p dashd-chainstate --lib -- shutdown_flush on_vs_exit`: **5 passed**.
  - `cargo test -p dashd-node --lib -- shutdown_flush aborted_node killed_node restart_keeps devnet
    stored_tip net_caches`: **6 passed**.
  - `cargo test -p dashd-node --features test-support --lib -- sut::tests`: **27 passed**, all
    the BL-271 `NodeSut` tests among them.
- I didn't run mutants: m-1's distinguishing input needs a new fault seam, so I've given it as a
  scenario rather than a run.
