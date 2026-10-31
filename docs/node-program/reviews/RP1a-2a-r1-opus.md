# RP1a-2a review, round 1 (Opus)

Scope: `b0306ac01` (feature) and `519819a40` (own-review fixes) on `node/rp1a-2a` at 519819a40.
Core reference: v24.0.0-rc.4 49caf372bb, read from `~/workspace/dash` (`git show 49caf372bb:…`):
`src/rpc/blockchain.cpp:77-85, 300-441`, `src/rpc/server.cpp:307-336`,
`src/init.cpp:557-568, 986-987, 2429-2438, 2761-2770`.

**Verdict: ACCEPT-WITH-NITS.** 0 M, 3 m, 5 n.

The port matches Core on everything I checked: argument parsing, the `{hash, height}` reply,
which block is returned on timeout and on change, the wake at shutdown, signal order, and the
init tip set. I found no lost-wakeup window and no lock cycle. All three m findings are test gaps,
and each has a mutant that survives today's tests; I confirmed four of them by running them. None
of them is a code defect. The n findings are hardening and doc fixes.

## Findings

### m-1: nothing tests the restart path that init's tip set exists for

- Port: `dashd-node/src/node.rs:400` (`self.rpc_chain.notify_loaded_tip()`),
  `dashd-node/src/rpc.rs:260`.
- Core: `init.cpp:2761-2769`. Core's comment gives the reason for the call: without it the RPC's
  view stays at height 0 and hash 0, so `waitforblockheight` answers wrongly.
- Scenario: the node restarts on a datadir whose tip is at height N. Loading the chain doesn't
  move the tip, so V sends no `NotifyBlockTip`. If the init call is missing, `latestblock` stays
  `{000…0, 0}`. Then `waitforblockheight [1, 1]` answers `{"hash":"000…0","height":0}`, and
  `waitforblockheight [1]` (no timeout) hangs until the next block.
- Gap: every test that reaches this code starts from a fresh datadir: `rpc_diff`, `hooks`, and
  func-compat's `feature_shutdown` and `interface_rpc`. There, V's genesis connect already
  notifies genesis. The node test `the_waits_hear_the_tips_v_connects` calls
  `slot.notify_loaded_tip()` itself, so it covers `ChainSlot`, not `Node`.
- Mutant that survives (by reasoning, not run): delete `node.rs:400`. I confirmed that V only
  notifies when blocks connect: `dashd-chainstate/src/chain/abc.rs:941-950` pushes `ui` only
  `if blocks_connected && fork != new_tip`, as Core `validation.cpp` ActivateBestChain does. So on
  a restart with no new blocks, the init call is the only thing that sets `latestblock`.
- Fix: add a restart case. Either a `dashd-node` test that loads a chain with a tip above genesis
  through the `Node` steps and calls `waitforblockheight [h, 1]` at once, or an `rdashd`
  test that mines or submits a block, restarts, and calls `waitforblockheight [1, 1]` right
  after warmup.

### m-2: the timed-wait branch is never run while blocks arrive

- Port: `dashd-rpc/src/blockchain/wait.rs:92-108`.
- Core: `blockchain.cpp:339`, `:384` and `:431` use `wait_for(lock, ms, pred)`, which is
  `wait_until(now + ms, pred)`: one absolute deadline, and the predicate re-checked on every wake.
- Gap: every unit-test or node-test wait that a notification ends has timeout 0 (the `None`
  branch). Every timed wait (`[20]`, `[0, 1]`, `[5, 1]`, and all `rpc_diff` cases) either holds at
  once or expires with nothing arriving. So the `Some(deadline)` branch's re-check and its
  remaining-time arithmetic never run.
- Mutants that survive. I ran all three; each passes the 5 existing wait tests:
  1. **Deadline resets on every wake.** In the `Some` arm, do
     `wait_timeout(state, full_timeout)` and `break` only on `timed_out()`. A
     `waitforblockheight [100, 100]` with a non-matching block every 5 ms then never returns.
     Nothing in `rpc_diff` notices either, because its timed cases see no traffic.
  2. **Seconds, not milliseconds.** `wait.rs:94` with `Duration::from_secs`. The `[20]` case
     still passes, just 20 s slower.
  3. **A timed wait ignores notifications.** It sleeps until the deadline and then returns the
     latest block. The result is still right; only the timing is wrong.
- Fix: add two tests. I checked both in the scratch copy: they pass on 519819a40, and each kills
  its mutants.
  - `waitforblockheight [100, 100]` while the poke sends non-matching blocks every 5 ms. Assert
    it returns within 2 s. This kills mutants 1 and 2.
  - `waitforblockheight [3, 10000]` while the poke climbs to height 3. Assert it returns
    `block(3, 3)` within 2 s. This kills mutant 3.
  - The probe bodies are in `/work/scratch/rp1a-2a-rv-opus/dashd-rpc/src/blockchain/wait/tests.rs`
    (`rv_*`).

### m-3: `waitfornewblock`'s "same height, other hash" case is untested

- Port: `wait.rs:170` (`latest != start`, comparing both fields).
- Core: `blockchain.cpp:339-341`, `latestblock.height != block.height || latestblock.hash !=
  block.hash`.
- Gap: the unit tests always pair `hash(n)` with height `n`. `rpc_diff`'s `waitfornewblock_c5`
  passes `[-1]`, so it only reads `latestblock` and never runs the predicate across the a5 → c5
  same-height switch.
- Mutant that survives, which I ran: `|start, latest| latest.height != start.height`. It
  passes all 5 tests. A `waitfornewblock` blocked at a5 would then miss `preciousblock c5`, a
  reorg to a sibling at the same height.
- Fix: in `waitfornewblock_returns_the_next_block`, or a sibling test, notify
  `(hash(9), 1)` after `(hash(1), 1)` and expect `block(9, 1)`. My probe
  `rv_waitfornewblock_sees_a_same_height_sibling` passes on base and kills the mutant.

### n-1: unit-test waits have no bound, so a regression hangs instead of failing

- `dashd-rpc/src/blockchain/wait/tests.rs:41-56` (`waits`) loops until the call finishes. The node
  test uses `bounded(...)`, but these don't.
- Mutants caught only by a hang (nextest or CI timeout), not a failure:
  - Drop `notify_all` in `stopped` (`wait.rs:79-82`). I ran this one:
    `a_stopped_server_ends_every_wait` hit my 60 s `timeout` (rc 124), and every other test
    passed.
  - Drop `running()` from the loop condition (`wait.rs:98`). Not run.
- Fix: stop poking after a bound (for example 2 s or 400 rounds) and `panic!` with the round
  count.

### n-2: the slot guard checks `is_running`, not "has started", and isn't atomic with `start`

- Port: `dashd-rpc/src/server.rs:297-309`.
- `on_started` after `interrupt()` passes the assert, because `is_running()` is false again, and
  the slot never runs. The doc says this is the case being refused. Also, `on_stopped` connected
  after start would still run at `stop`: Core lets `OnStopped` connect at any time, so the doc's
  "the slot would never run" is wrong for it. The check and the push sit in different critical
  sections from `start`'s `running.store` and its slot loop. No current caller races them,
  since init is single-threaded.
- This mirrors `append_command`'s existing guard (`server.rs:283`), so it is consistent.
- Fix (optional): keep a `started: bool` inside `Signals`, set by `start()` under the same lock.
  Refuse `on_started` once it is set, and allow `on_stopped` until `stop()`. Otherwise just
  correct the `on_stopped` doc.

### n-3: `RpcServer::stop`'s check is debug-only, but in release the same mistake deadlocks

- Port: `server.rs:325`. Core: `server.cpp:328` is `assert(!g_rpc_running)`, which is active in
  release builds.
- Scenario: a future caller calls `Servers::stop` without `interrupt`. In release builds, every
  wait with no timeout wakes, sees `running()`, and waits again. Then `http.stop()` joins the
  workers forever. Today `Node::shutdown` always interrupts first (`node.rs:448`), so this is
  latent.
- Fix: use `assert!` (it costs nothing) to match Core and turn a hang into a crash.

### n-4: the init tip can still overwrite a newer notified tip, as in Core

- Port: `dashd-node/src/rpc.rs:260-266` reads the published view, then `set_tip` writes it.
  Core: `init.cpp:2769` reads `ActiveTip()` under `cs_main` and then takes `cs_blockchange`.
- Scenario: V publishes and notifies T2 (`abc.rs` `after_release`: `publish_view`, then
  `notify_block_tip`) between `notify_loaded_tip`'s read (T1) and its write. `latestblock` then
  goes back to T1 until the next tip. Core has the same window: it notifies outside `cs_main`.
  The port is no worse, and the effect lasts one block.
- Fix: none needed for parity. Add one line to D20 saying the window is Core's, since the brief
  asked whether the tip can go backwards.

### n-5: D21's long-poll wake has no owner

- `docs/node-program/designs/RP1a-notes.md:61-65`. The note says that once `getblocktemplate`
  long poll lands, the stop slot must also wake `TipWatch` (Core `init.cpp:567`), "or a long poll
  hangs at shutdown". No BL row or owning slice is named. D7 and others name theirs.
- Fix: name the owner (the GBT / RP1a-2 slice) or a BL row.

## Checked and found matching Core

- **Arguments.** All three waits read `timeout` with `getInt<int>`, and null means 0. A timeout of
  0 waits with no deadline (`cond.wait`). A negative timeout checks the predicate once:
  `wait_for` with a negative duration is a past `wait_until`, and the port's zero-`left` break
  does the same. `blockhash` goes through `ParseHashV`, and `height` through `getInt<int>`.
  Arity and type errors come from the shared `RPCHelpMan` check (`server.rs:75`, Core
  `rpc/util.cpp:540-546`). `rpc_diff` covers short and non-hex hashes, a number for the hash,
  string and fraction heights, an extra argument, and a string timeout.
- **Predicates.** `waitfornewblock` compares the height or the hash against the block at the
  call's start (Core `:339-341`). `waitforblock` compares the hash; `waitforblockheight` uses
  `>=`. Each also stops on `!IsRPCRunning()`. Every wait returns `latestblock` read under the
  lock once the wait ends, whether by change, timeout or stop (Core `:342`, `:387`, `:434`).
- **Reply.** `{hash, height}` in that order, with height as a number. Before any notification
  the default is the zero hash at height 0, as Core's static is.
- **Lost wakeups.** The predicate (including `running()`) is checked under `state` before every
  wait. `stopped()` takes `state` after `interrupt` has stored `running = false` (SeqCst), then
  calls `notify_all`. So a waiter either checks after the store or is already parked when the
  notify comes. That is tighter than Core, whose `RPCNotifyBlockChange(nullptr)` notifies without
  holding `cs_blockchange`. `notify_block_tip` writes under the lock and notifies after
  releasing it, which is fine. The `while` loop absorbs spurious wakeups.
- **Lock order.** The waiters hold only `BlockChange::state` and never touch the chain.
  `notify_block_tip` runs on the chain actor in `after_release`, holding only `state`. The
  start and stop slots take `state` under `signals`, and nothing takes `signals` while holding
  `state`. There is no cycle.
- **Shutdown order.** `Node::shutdown` interrupts first (`node.rs:448`). `Servers::stop` then
  runs `StopHTTPRPC`, cookie removal, `table.stop()` (which wakes the waits), and only then
  `http.stop()`, which joins the workers. This is Core's `init.cpp:329-332`, so a wait with no
  timeout cannot deadlock the join.
- **Worker use.** A wait with no timeout holds one HTTP worker until it is satisfied or the
  server stops, exactly as in Core's `-rpcthreads` model.
- **Signal wiring.**
  - `start` sets `running`, then fires the `Started` slots (Core `server.cpp:307-312`).
  - `stop` fires the `Stopped` slots after the cookie is deleted (`:325-336`).
  - `OnRPCStopped` disconnects the handler, then wakes the waits (`init.cpp:563-568`), minus
    `g_best_block_cv` (n-5).
  - The slots are registered in `command_table`, before `Servers::start` calls `table.start()`
    (Core `init.cpp:986-987` comes before `StartRPC`).
  - The sink calls `BlockChange` before `-blocknotify`, Core's slot order (RPC connects at 4a;
    `-blocknotify` at `init.cpp:2429-2438`).
- **Init tip.** `set_tip` ignores `connected`, as Core's direct `RPCNotifyBlockChange` call does.
  It is called just before `set_warmup_finished` (`init.cpp:2769-2770`). `height()` and
  `block_hash(height)` come from one `view()` snapshot, so the pair is consistent. Its race with
  a concurrent tip is Core's (n-4).
- **Invalidate and precious.** `InvalidateBlock`'s notify (`failure.rs:113`) and
  `preciousblock`'s ABC move `latestblock`. `rpc_diff` `waitforblockheight_a3` and
  `waitfornewblock_c5` check this against dashd.
- **519819a40.** The debug-assert in `stop` and the refusal of slots once running are both fine
  (see n-2 and n-3). The next-block test can no longer hang on a repeated block: from round 10
  every round's hash differs.
- **Security.** Nothing to raise.

## What I ran

- Read both commits in full, and the Core rc.4 sources listed at the top via `git show
  49caf372bb:` in `~/workspace/dash`.
- Made a reflink copy at `/work/scratch/rp1a-2a-rv-opus` (HEAD 519819a40, clean), with
  `TMPDIR=.rv-tmp` and `CARGO_BUILD_JOBS=12`. `cargo test -p dashd-rpc --lib blockchain::wait`
  passed 5/5 on base.
- Added mutants behind `RVMUT=n` in the copy's `wait.rs`, plus three `rv_*` probe tests. The
  probes pass 8/8 on base. I reran each test binary once per mutant:

  | mutant | 5 existing tests | probe that kills it |
  |---|---|---|
  | 2: `from_secs` | pass (the `[20]` case takes 20 s) | `rv_timed_wait_under_traffic_times_out` |
  | 3: height-only `waitfornewblock` | pass | `rv_waitfornewblock_sees_a_same_height_sibling` |
  | 4: no `notify_all` in `stopped` | `a_stopped_server_ends_every_wait` hangs (rc 124) | none (n-1) |
  | 5: deadline resets per wake | pass | `rv_timed_wait_under_traffic_times_out` |
  | 6: timed wait sleeps through notifies | pass | `rv_timed_wait_returns_early_on_match` |

  Mutant 1 (pass the full duration to `wait_timeout` but keep the absolute-deadline check) was
  too weak: it only overshoots by one timeout, so I replaced it with mutant 5.
- Did not run the dashd-backed tests (`rpc_diff`, `hooks`, func-compat) or tier 1. I relied on
  the implementer's results: tier 1 7472/0, DIFF-RPC 10/10, func-compat 8/0. The `node.rs:400`
  mutant (m-1) is reasoned from `abc.rs:941-950`, not run.
- The scratch copy is left in place for the manager.
