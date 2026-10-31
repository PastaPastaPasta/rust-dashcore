# BL-547 r1 review (Opus)

- Branch `node/bl547`, head 2ece6d4ad, on base 30955743a.
- Frozen worktree: /work/home/agent/workspace/rdc-wt/bl547.
- Core reference: v24.0.0-rc.4.
- Everything below was settled by reading. Nothing was built or run.

## Verdict: ACCEPT-WITH-NITS

- **Counts:** 0 M, 2 m, 3 n.
- **Order:** the reaper's order matches Core's `loadblk`: `SetLoading(false)`, then `ibd`, then the DMN tip, then the listeners, then the reply.
- **Exactly once:** the DMN tip moves exactly once on every path the brief lists.
- **Liveness:** I found no hang or deadlock.
- **m-1:** one narrow window serves the command after a V panic, against D72's own contract.
- **m-2:** one cheap mutant the brief names survives.

## Findings

### m-1: Serve is sent before `stop_flush_at_exit`, so a panic there still gets init's command served

**Where:** `dashd-chainstate/src/chain/actor.rs:1459-1466`, with `:1525-1546`.

**The defect:**
- `run` sends `AfterStop::Serve { tip, listeners }` as soon as `serve()` returns.
- It does this before `stop_flush_at_exit`, on both the `None` (shutdown) arm and the `Some(f)` (AbortNode) arm.
- `stop_flush_at_exit` documents a panic in it as "V's panic (D62)": it calls `stop_serving(.., AbortCause::Panic)` (or `flush.abort(Panic)`), then `resume_unwind`.
- By then the reaper already holds `Serve` in `at_stop`'s one slot. A later `Drop` can't replace it.

**Failing sequence (shutdown during the import):**
1. Init's import thread is in ABC. `Chain::stop` sends `Shutdown`.
2. V's loop returns `None`, and `run` sends `Serve { tip: T, listeners: true }`.
3. `stop_flush_at_exit(false)` services W and waits for a job W started at an earlier site. That stop panics.
4. `stop_serving(Panic)` hands the lanes to the reaper, and V resumes the unwind.
5. ABC's reply disconnected at the stop. loadblk sends `SetLoading(false)` and `InitializeCurrentBlockTip`.
6. The reaper reads the cached `Serve`. It moves the DMN tip to T, runs the listeners (SyncListener → `MasternodeSync::updated_block_tip`), and replies `Ok`.

**What Core does:**
- That flush ran synchronously at its site, inside ThreadImport's ABC.
- An assert there ends the process before `ThreadImport` returns, so `InitializeCurrentBlockTip` never runs (`init.cpp:2458-2473`).

**Same on the AbortNode arm:**
- The sequence is AbortNode → `stop_serving` → `stop_flush_at_exit(true)` panics.
- The reaper (which took the lanes at the fault) is already unblocked by `Serve`. It runs the listeners at the same time as V's panic.

**Why this matters:**
- D72 says: "After a panic or one of Core's asserts, even one after a carried `AbortNode`, the command is still dropped".
- The test `the_reaper_drops_init_after_an_assert_past_an_abort` covers only an assert inside `serve`. It doesn't cover this window.
- The effect is in memory only (the DMN tip and mnsync in a process that is panicking), so it is m, not M.

**Fix:**
- Send `AfterStop` after the stop-flush match: after `stop_flush_at_exit` returns on either arm.
- The tip can't change in the flush stop, and W never waits on the reaper. So the reaper just waits a bounded time longer.
- A panic in `stop_flush_at_exit` then unwinds with the sender unsent, which reads as `Drop`.
- Optionally, plant a panic in the flush side's `stop` (the BL-271 test seams) and assert `Disconnected` with `dmn_moves` unchanged.

### m-2: "IBD from V's last published view" survives every test

**Where:**
- `dashd-chainstate/src/chain/actor.rs:933`
- `dashd-chainstate/src/chain/tests/dmn.rs:485-500` and `:529-563`
- `dashd-node/src/chain/tests.rs` `a_failed_startup_abc_aborts_the_node_again`

**The mutant:**

```rust
let snap = self.reader.snapshot();
let ibd = self.reader.is_initial_block_download(&snap, now, loading)
```

This replaces `is_initial_block_download_at(entry(tip).(work, time), ..)` with the same check on the published snapshot.

**Why it survives:**
- In `the_reaper_answers_init_after_an_abort_in_cores_order`, the reaper's tip is b1 (time day−120) and the published tip is b2 (time day−60).
- The rig clock is about genesis + 1 day, the max tip age is 6 h, and regtest's minimum work is 0. So both tips are recent, and both give `ibd = false` once `fImporting` clears.
- The latch isn't set beforehand either: importing has been true since the rig started.
- In the dashd-node test, the published tip is V's tip (genesis).
- So no test tells "V's tip" from "the view's tip" for `ibd`, and that is the point of D72's "ibd at V's tip".

**Where it matters in production:**
- The view lags V's tip after a step carried past an AbortNode, or after an abandoned hold.
- Example: the non-public tip moved from a stale block to a recent one in a step whose release never published.
  - Core's `IsInitialBlockDownload()` reads `m_chain.Tip()` and says false (and latches).
  - The mutant says true.

**Fix:** in `importing_rig`'s abort variant, give the reaper's tip and the published tip different IBD answers. For example:
- make b2 older than the max tip age and b1 recent (carry the step forward rather than back); or
- set `-minimumchainwork` between their works.

Then assert `ibd` in the probe's call.

### n-1: The strong-signals mutant ("upgrade once and cache") survives

**Where:** `dashd-chainstate/src/chain/actor.rs:934`, `:1096`.

**The mutant:** upgrade the weak signals once at the first ICBT (or hold `ValidationSignals` strongly in `ReaperCx`). No test kills it.

**Its only effect is a cycle:** signals → a listener holding a `ChainHandle`/`ChainViewHandle` → the lanes stay open → the reaper never exits.

**Production impact:**
- In production `net.stop()` unregisters SyncListener before `Chain::stop`, so this would show only as a reaper thread (plus its listeners) leaking until the process exits.
- That holds for any listener with a handle that stays registered.

**Cheap kill:**
1. Register a listener that holds a `ChainHandle` clone and records its own `Drop`.
2. Stop V, serve ICBT on the reaper, then drop the rig's handles and signals.
3. Assert the listener dropped, or the reaper thread ended.

### n-2: A V that stopped on an SB4 contract failure still has init's command served at its non-public tip

**Where:**
- `dashd-chainstate/src/chain/actor.rs:1459`
- `dashd-chainstate/src/chain/seams.rs:112-117`: `Class::Contract` maps to `AbortCause::AbortNode`.

**The behaviour:**
- After a contract failure, `run` keeps nothing past V's last flush, because "SB4 refused a step V may have half taken" (BL-271 r1 m-2).
- But the reaper still moves the DMN tip to `self.tip()` and runs the listeners at that tip.

**Sequence:**
1. A connect step's SB4 call is refused.
2. V stops with `f.is_contract()`.
3. loadblk's ICBT reaches the reaper.
4. mnsync hears a tip whose state V has just declared untrusted.

**Assessment:**
- Core has no such failure, so no outcome is judged against Core.
- I'd either map contract failures to `AfterStop::Drop`, consistent with BL-271's distrust, or add one sentence to D72 saying they are served.

### n-3: D72 overstates two equivalences

**Where:** `docs/node-program/designs/CH7-notes.md:76`, and the probe in `dashd-chainstate/src/chain/tests/dmn.rs:466`.

**(a) "the outcome does not depend on whether V stopped before the command arrived":**
- This isn't strictly true.
- V's own handler retries `publish_view` (`scope_end`, actor.rs:2317). If the evo read now succeeds, it runs the listeners.
- The reaper uses `view_withheld` as it was at the stop and never retries.
- Both are D65-sanctioned refusals (no listener where Core runs one), so nothing is accepted that Core rejects. Only the sentence needs softening.

**(b) The re-entry test doesn't run the node's real listener:**
- The test that backs DEC-924's "no listener re-enters V" condition uses `InitProbe`, not `SyncListener`.
- `InitProbe` reads `ChainReader::entry`, `ChainViewHandle::view` and `signals.dmn_tip()`.
- `SyncListener` uses `ChainViewHandle::entry` and `::snapshot`. Both are `self.reader()` reads (view.rs:151-165), and `dashd-masternode` has no dependency on dashd-chainstate. So the claim holds by reading.
- No test runs `SyncListener` on the reaper. A one-line node test registering the real `SyncListener` in `a_failed_startup_abc_aborts_the_node_again` would close that.

## What I checked

### 1. Order against Core

**The tip:**
- The reaper's tip is V's `chain.private_tip()` (Core's `m_chain.Tip()`).
- It is read after `serve()` returns, which is after `run_carried_tails` (BL-535). So it includes every carried tail step.
- An abort during the final step:
  - If a tail fails with an AbortNode, the tip is the tail's result.
  - If a tail fails with an assert, the result is `AfterStop::Drop`, via `Fatal::cause`.
- An abort after the final step (an `Err` from `stop_flush_at_exit`) can't move the tip.
- Core races loadblk's ICBT against the other threads' ABC tails. rdashd always reports after the tails, which is one of Core's interleavings.

**IBD:**
- The reaper is the only writer of `LoadingFlags` once V has stopped (D220).
- It serves `SetLoading(false)` in lane order before the ICBT behind it. Both come from loadblk (unmarked), so both use the bounded lane.
- `loading.any()` is read at the call.
- The latch is the shared `ReaderState::finished_ibd` (`dash-chain/src/shared.rs:158`), so the reaper and V use the same one.
- The node keeps every arena entry (no cold eviction), so `reader.entry(tip)` gives the same `(chain_work, header.time)` as `facts_of`.

**The DMN tip, the listeners, and the shutdown:**
- The DMN tip moves before the listeners: `initialize_dmn_tip`, then `initialize_current_block_tip`. That matches `dsnotificationinterface.cpp:31-35` (`SynchronousUpdatedBlockTip` → `m_dmnman.UpdatedBlockTip`, then `UpdatedBlockTip`).
- The listeners run before the node's shutdown flushes:
  - `Chain::stop` joins the import thread, which waits on the reply, before `force_flush`.
  - That matches `init.cpp:355` before `:379-385`.
- The SyncListener unregister by `net.stop()` (where Core keeps `CDSNotificationInterface` until `:484`) is pre-existing, and D72 records it.
- Core lines I checked: `init.cpp` 2455-2474 and 340-360/378-390/480-488; `blockstorage.cpp` 878-962; `validationinterface.cpp:207-209`; `dsnotificationinterface.cpp:31-70`.

### 2. Exactly once

**Who takes the command:**
- `InitializeCurrentBlockTip` is class M (`command.rs:437-484`), so a release point never defers it.
- V's release points and serve-waits stop taking from the lanes once `self.stop` is set (`actor.rs:1300`, `:2081-2091`).
- So exactly one of V and the reaper takes the command.

**The base's double move:**
- The base moved the DMN tip twice in V's handler (its own move, plus `ValidationSignals::initialize_current_block_tip`).
- The split fixes that (M9).

**Each path:**

| Path | What happens | DMN tip moves |
|---|---|---|
| normal | V's handler | once |
| abort | reaper `Serve` | once |
| shutdown | reaper `Serve` | once |
| withheld view | moves, but no listeners, in both V and the reaper | once |
| panic inside `serve` | sender dropped on unwind, so `Drop` | 0 times |
| assert after a carried abort | `Fatal::cause` is Panic, so `Drop` | 0 times |

The one exception is m-1.

### 3. Liveness

**The reaper's wait for V:**
- The reaper blocks on `at_stop` only while V's loop is still ending (carried tails), or never.
- During that time V waits only on W and on the signals thread.
- The signals thread is no-wait marked (`signals.rs:381-392`). Its listeners post to the unbounded marked lane and never wait on a reply.
- So no cycle runs through the blocked reaper. Unmarked senders on the bounded lane can wait until V's loop ends, a bounded delay that ends.

**The other cases the brief lists:**
- A failed weak upgrade replies `Ok` (there are no listeners).
- A dropped reply sender is ignored.
- A listener that blocks makes Chain::stop wait, as Core's `m_load_block.join()` does.
- The reaper exits only when both lanes are disconnected and empty. loadblk holds a handle until its reply, so the reaper can't exit before serving it.

**What the reaper keeps alive:**
- Its new strong refs are the `ChainReader`, the clock and `NodeAbort` (Ui, Warnings, ShutdownState).
- None of them holds a `ChainHandle`, so there is no new cycle.

### 4. Re-entry

- The only real listener with an ICBT body is `SyncListener` (mnsync.rs:52-89), and it only reads. See n-3(b).
- V is unmarked too, so a listener that waited on V would deadlock on V just as it would on the reaper. Moving the call to the reaper doesn't change re-entry behaviour.

### 5. The dashd-node test

- `FailingFlushAfterOne` lets genesis connect. Then the next site fails with AbortNode.
- V stops at the fault (D213) and the lanes go to the reaper. ABC replies `Err`.
- loadblk aborts again ("Failed to connect best block"), then sends `SetLoading(false)` and ICBT.
- The production `after_import` → reaper path serves them.
- The listener records `aborts == 2` because loadblk's report comes before its send on the same thread.
- `tip` is compared with `VExit`'s non-public tip, and `ibd = false` holds at genesis + 60 s.
- So the test exercises the new path, and both claims hold.

### 6. Mutants

I read M1–M10 in `/work/scratch/bl547/mut-patch.py` and `mut-run.log`. All ten are killed.

| Extra mutant | Result | Why |
|---|---|---|
| The reaper replies before the listeners | Killed | `init.try_recv() == None` while the probe is parked (dmn.rs:543-546) |
| The DMN tip moves after the listeners | Killed | The probe sees `dmn` inside the call |
| `loading.any()` read before `at_stop.recv()` | Equivalent | The reaper is the only writer |
| IBD from the published view | Survives | m-2 |
| Strong or cached signals | Survives | n-1 |
| Serve sent after the flush stop | Not killed, and would be the fix | m-1 |

### Outside these findings

PLAN.md's "fold in the manager's n-1: assert `WIPED[0] == "coins"`" isn't in this commit. `store/meta.rs:30` is unchanged. This is not a correctness finding; I mention it only for the manager's tracking.
