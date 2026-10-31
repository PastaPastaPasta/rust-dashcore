# CH2b b3 subscribers: Opus round 1 (independent, after Sol r3 ACCEPT)

- **Under review:** `node/ch2b-b3` at **13cd6c373**. The whole b3 slice is `657a28c28...13cd6c373 -- dashd-chainstate`; the r2 round is `1cffd18f9..13cd6c373`.
- **Spec:** CH2 §4.2/§4.4/§7.2 b3, CH2b-b3-notes (D37–D39 updated, D45–D50 new).
- **Core reference:** `index/base.cpp`, `validationinterface.cpp`.
- **Method:**
  - I read hub.rs, subscriber/driver.rs, flush/{machine,driver,control}.rs, and crash/{fa,subs,chain,workload}.rs.
  - Each r2 fix was run with its fix removed (scratch-only, env-gated mutations).
  - Soaks ran on 8 fresh seeds at the head and 8 more with two scratch fixes. An optional-subscriber threaded watchdog probe was also run.
  - The branch was not edited. All probes lived in a detached scratch worktree.

## Summary

The r2 fixes close B1, M2 and m3, and every new or changed plant is caught. Two soak failures appeared on fresh seeds that the owner's seeds (11/13/17/19/23/29) didn't hit:

1. **FA4 cadence (Major):**
   - Cause: the driver's Syncing cadence counter is not lowered when a stale-branch rewind drops `progress` below it.
   - Effect: the catch-up re-climbs past the cadence point without a cadence commit.
   - Seed 81703 fails at the head.
2. **FA6 checker false positive (Minor, harness):**
   - Cause: FA6's checkpoint bound ignores a rewind that lands during step 6.
   - The driver is correct here; the model is wrong.
   - Seed 81707 fails at the head.

Neither failure breaks durability or R11a. Both would fail the merge fuzz/soak budget, so both need a fix and a checked-in repro before merge. Everything else is Minor or Nit.

## Findings

### Major

**M-A: Syncing cadence counter is not lowered by a stale-branch rewind, so FA4 fails.**

Locations: `dashd-chainstate/src/subscriber/driver.rs:580-583` (stale path) and `:597-598` (cadence check).

The cadence point is unconditional. `cadence_from = next.height` is set even when `capped_commit` writes nothing because an in-flight flush's step-3b bound caps it below L.

The stale path calls `disconnect_tip()` (`:1002`) and leaves `cadence_from` above the new `progress`. The catch-up then re-climbs through heights the counter already "spent", and the next cadence commit is due `cadence` blocks above the stale high point.

The b3 code already handles this in the other two places that move `progress` down or reset the timer:
- D45's open rewind sets `cadence_from = fork.height` (`:430`).
- `tick` sets `cadence_from = progress` (`:488`).

The catch-up disconnect is the remaining path. Blame shows `:597-598` predate r2 (018343e53 / fac7b5030), so this is b3 logic and not a regression from r2.

Schedule (seed 81703, `CHAINSTATE_CASES=5000`, also saved as `fa4-81703.repro`):
```
config sub=1000 invalid=- effects=- presumed=- crash=- replay=- index=syncing:2:96
workload c6;f;f;r4:3;a2;s7;f;r3:6;r5:5;r6:5;D2;s6;f
expect FA4
```
With cadence 2:
1. The cadence point at block 6 fires while a reorg flush's step-3b bound caps the commit to 2 < L=4. The commit is a no-op, but `cadence_from := 6`.
2. The stale-branch catch-up rewinds `p` 6→3, with a rewind commit at L=3.
3. It re-climbs to 6 with no cadence commit; the next commit is not due until 8. FA4 flags the stuck cadence.

Is this observable versus Core?
- Core's cadence is wall-clock (`last_locator_write_time`) and Rewind doesn't reset it either. So Core can also re-climb after a reorg without a locator write.
- The model's cadence is block-count, and FA4 is our own spec. The practical effect is extra re-sync work after a crash in this window. There is no durability or R11a break.
- It is Major only because it makes the soak gate fail at about 1 seed in 16 × 5000.

Fix: keep `cadence_from ≤ progress` whenever `progress` drops. `disconnect_tip` is the only place that lowers `progress` after `open` (`rewind_if_due` commits at the current `progress` and doesn't move it). So clamp either in `disconnect_tip`, which is harmless for Synced, or after the stale-path call in `sync_step`.

I tested the stale-path variant: `self.cadence_from = self.cadence_from.min(progress height)` after `disconnect_tip()`.
- It clears the 81703 repro.
- Seeds 81701–81706 are clean with it.
- With FA6 below also fixed: 8 more seeds are clean, crash_planted is 64/64, and crash_subscriber is 17/17.

Check `fa4-81703.repro` in as a regression. An alternative is to redefine FA4 as "a commit within `cadence` blocks above max(L, last cadence point) on the current leg". That would need a spec change and is weaker; I prefer the driver fix.

### Minor

**m-B: FA6 drain bound ignores a rewind landing during step 6 (checker false positive).**

Location: `dashd-chainstate/src/crash/fa.rs:117-118`.

`close` computes `F = lca(p_start, new)` from the subscriber's progress sampled when step 6 began. If V's queued disconnects reach the subscriber during step 6, the subscriber correctly rewinds below F (rewind commit). It then reconnects the same blocks and checkpoints at `D_max` intervals. Those checkpoints exceed `⌈(h(new) − F)/D_max⌉`, which here is 0.

Schedule (seed 81707, also saved as `fa6-81707.repro`):
```
config sub=- invalid=- effects=- presumed=- crash=- replay=- index=syncing:10:2
workload c8;f;s18;c1;a2;c4;f;i;D3;c3;f;r9:5;f
expect FA6
```
Flush 6 has new=13, and step 6 starts at p = L = new = 13. The subscriber then processes 13→10 disconnects (rewind commit at 10), reconnects 11–13, and checkpoints at 12 because D_max = 2. FA6 reports 1 > 0.

Fix: lower F to the lowest `LocatorKind::Rewind` locator marked during the step. The probe was a `low: Option<i64>` on `Step6`, with `f = f.min(low)`. It clears the repro, and the 8-seed fx2 batch is clean with it. Check the repro in.

This is independent of M-A: 81707 still fails with only the cadence fix.

**m-C: The soak generator never disables or invalidates, so D45, the 3b gate and registration windows are directed-only.**

Location: `dashd-chainstate/src/crash/workload.rs:21-58`.

`chainstate_crash` emits no `D`/`Dp{h}` (disable/re-register) and no `x` (invalidate) ops. So the soaks never reach:
- D45's open rewind;
- a registration inside a flush window (D35/D43/D47);
- a backward flush meeting a registered mandatory subscriber (the B1 shape).

Those paths are covered only by S11o/S11r/S2b and the unit tests. This is enough for this round, but B1 was found by hand, and the next bug in this area will be too.

Suggestion: in a follow-up, add a low-rate disable/re-register op (and `x`) to the generator, gated so existing seeds stay stable or are re-anchored.

**m-D: D45 silently relies on T_ack being known before a mandatory `open`.**

Location: `dashd-chainstate/src/subscriber/driver.rs:421-431`.

The rewind is gated on `let (Some(l), Some(g)) = (durable, me.hub.t_ack())`. If production wiring ever opens a mandatory subscriber before the hub has `recovered()`, then `t_ack()` is `None`. D45 is skipped silently and B1 reopens. Add a `debug_assert!` or error on "mandatory open before recovery", or document the ordering on `SubscriberHub::recovered`.

**m-E: D45's availability cost (agrees with Sol r3), with a correction to its framing.**

D45's `rewind_commit` reads old-branch block files. Without D45, the same read happens in the first `sync_step` catch-up (`disconnect_tip` → rewind commit), so the dependency is not new. What D45 changes is where a missing file surfaces:
- with D45: an `open` error, so the subscriber is never registered;
- without D45: a `sync_step` error that trips the latch.

The D45 outcome is gentler. The real gap versus Core is D17's one-entry locator: Core's `FindForkInGlobalIndex` over a multi-entry locator doesn't need the stale blocks to find the fork. I agree with deferring it.

**m-F: D47 holes (agrees with Sol r3), with one root and a recommended direction.**

Locations: `dashd-chainstate/src/flush/hub.rs:304` (`stepping_here`), `:322` (`owns_awaited`), `:366` (the `HoldActive` retry).

Both documented holes come from one fact: the hub cannot observe a driver moving to another thread before its first step. Until then, the D39 `stepper` and D47 `owner` records are stale.
- I mutated `owns_awaited` to always return false. Both D47 tests hang to the 30 s watchdog, so the reported case is closed.
- There is no production wiring yet that hands a registered driver across threads. So the holes are not reachable today.

When the events-thread wiring lands, prefer one of these:
- an explicit hand-off API, e.g. `hub.adopt(id)` called by the receiving thread before it blocks on anything;
- a documented rule that "a driver is stepped only by the thread that first stepped it".

Either beats more point guards.

### Nit

- **n-G** `dashd-chainstate/src/crash/subs.rs:407`: `set_on_locator` is installed after `SubscriberDriver::open` returns. D45's open-time `Rewind` write therefore never appears as a locator mark in the CRASH trace, and FA checks can't see it. It is harmless today (R11a is judged from the image), but install the callback before `open` or replay the mark, so a trace shows why L moved.
- **n-H** `dashd-chainstate/src/subscriber/driver.rs:824`: the D48 skip has no warning line. Core logs a warning that the locator block is not on the best chain and skips the write (index/base.cpp:357-363). I agree with deferring it; add a `tracing::warn!` when logging is wired.

## Round-2 items

### B1: CLOSED

- **Fix:** D45's mandatory-open rewind to LCA(L, G) is at `driver.rs:421-431`. It runs after the buffered blind delete of (L, L+span] and the prune check.
- **Evidence, rerun by me:**
  - The unit test (L=9 vs G=6) passes.
  - S11o is clean: 6 images, 13 branches.
  - With the NoOpenRwd plant, S11o fails R11a and D1b (images 5, branches 9).
  - `t36_on_s11r_replays_r11c` and `checked_in_subscriber_repros_replay` pass.
- **Regression checks:**
  - *Crash points:* the blind delete is `Commit::Buffered`. The rewind is one `Durable` batch after `sync_files`/`sync_wal`. A crash before it leaves the old L, and the next open redoes the rewind (idempotent). A crash after it leaves L = fork. Both satisfy R11a.
  - *3b window and prune lock:* candidates are taken from durable plus pending under the hub lock. A flush registered after the open rewind sees L = fork, and the gate targets it correctly. The open rewind moves L down only, so it never lifts a prune lock past G.
  - *Failed rewind:* `open` returns the error and drops the driver, and the drop deregisters it. V and W are not blocked. The only wait is the normal "gate waits until the owner steps it", which D45 doesn't extend.
- **Beyond Core:** D45 is stricter than R11a. It rewinds whenever L is not an ancestor-or-equal of G, which includes the sibling-branch case that R11a allows. That is harmless: it does the same work the catch-up would do immediately afterwards.
  - The in-memory best then equals what Core's `FindForkInGlobalIndex` gives.
  - Only the durable write is earlier than Core's. It is not observable through RPC (`getindexinfo` reports the in-memory best).
  - Acceptable.

### M2: CLOSED

- **Fix:** D46 removes the off-leg drop for `Toward::Flushed`. The A6/A7/B6/B7 backlog is applied and archived in queue order, as Core's single validation queue does.
- **Mutation (MUT_D46, drop off-leg for any `Toward`):** the r2 test fails.
- **Liveness (D46 + D49 + D50):** I tested whether an optional subscriber can stall V's back-pressure wait.
  - The commit cap is lowered only by an in-flight flush, and that flush's ChainStateFlushed control always follows. D50 clears `posted` on every ChainStateFlushed; MUT_D50 fails its test.
  - At the journal limit on a stale leg, D49 either checkpoints at p (p on the target's leg) or drops the stale connect. The subscriber therefore never parks holding a journal V is waiting on. MUT_D49 (Flushed drop arm disabled) fails its test.
  - Threaded probe: the watchdog sweeps (10,000 schedules × {per-subscriber, shared} events threads) were rerun with every subscriber optional, and with a mixed optional/mandatory set. All 4 passed with no hang.
- **Beyond Core:** D49's drop at the journal limit is a documented deviation. It is observable only for an optional journaled subscriber at the limit, and none exists in production (`SubscriberData::mandatory()` defaults to true). Acceptable.

### m3: CLOSED for the reported schedule; holes deferred (m-F)

- **Fix:** D47 records the owner at registration and moves it on step. In the window loop, `open` refuses with `WouldDeadlock` when `owns_awaited`.
- **Mutation (MUT_D47, skip `owns_awaited`):** both D47 tests hang to the watchdog.
- The two documented holes are not reachable in current wiring (m-F).

## D48 and D50

- **D48:** CORRECT, parity matches Core.
  - `on_flushed` (`driver.rs:824`) skips when the flushed tip is not ancestor-or-equal of p, which is Core's warn-and-skip.
  - The D37 handoff recheck uses the same rule.
  - The remaining difference is the commit point: Core commits its best block, and we commit the flushed tip, capped (D33). It is not observable outside the locator write timing.
  - MUT_D48 (commit at LCA when off-branch) fails its test. The log line is n-H.
- **D50:** CORRECT. MUT_D50 fails its test.

## Sol r3 deferrals

| Sol r3 item | My view |
|---|---|
| Minor: D47 holes | Agree to defer; one root, fix via a hand-off API when wiring lands (m-F) |
| Minor: D45 eager rewind needs old-branch files | Agree to defer; not new to D45, and D45 makes the failure gentler; root is D17 (m-E) |
| Nit: D48 no warning line | Agree (n-H) |

Sol r3 did not see M-A or m-B. Both come from fresh seeds.

## Guarantees beyond Core

| Item | Beyond Core | Observable? | Acceptable? |
|---|---|---|---|
| D45 durable open rewind | Core only moves the in-memory best at Init | No (RPC sees the same best); durable L is lower earlier | Yes |
| D45 rewinds sibling L too (stricter than R11a) | — | No | Yes |
| D49 drop at journal limit (optional) | Core has no journal limit | Only for optional journaled subscribers; none in production | Yes |
| D48 commit at flushed tip, not best | Core commits its best block | Locator timing only | Yes |
| Block-count cadence (vs wall-clock) | Model choice, pre-existing | Locator timing only | Yes, but see M-A |

No Dash Core security notes.

## Validation

All runs used my scratch worktree on 13cd6c373, with its own `CARGO_TARGET_DIR` and TMPDIR under /work/scratch/ch2b-b3-r1-opus.

- **Unit tests:** `subscriber::driver`, `flush::hub`, `flush::machine`, `flush::control`: 96 passed.
- **Plant witnesses, each failing with its fix removed:**
  - NoOpenRwd on S11o → R11a + D1b.
  - T36 on power_loss(S2b synced) → D1b, on the forward-run image.
  - X19 on S2b → D1b.
  - `t36_on_s11r_replays_r11c` passes, as does `checked_in_subscriber_repros_replay`.
  - Unplanted, S11o is clean (6 images / 13 branches), and S2b synced and syncing are clean (9/20).
- **r2 mutation probes (scratch, env-gated):** each r2 fix's test fails when the fix is removed.

  | Mutation | Result |
  |---|---|
  | D46 | test fails |
  | D47 | 2 tests hang → watchdog fail |
  | D48 | test fails |
  | D49 | test fails |
  | D50 | test fails |
- **Suites at the head:** `crash_subscriber` passes 17 (1 ignored).
- **`chainstate_crash_smoke`, CHAINSTATE_CASES=5000, at the head:**
  - Seeds 81701, 81702, 81704, 81705, 81706 and 81708 are clean, about 125–160 s each.
  - 81703 fails FA4 (M-A).
  - 81707 fails FA6 (m-B).
- **With both scratch fixes (FIX_CAD + FIX_FA6):**
  - Seeds 81707 and 81709–81715 are clean, 8 of 8, about 140–155 s each.
  - `crash_planted` passes 64 (4 ignored).
  - `crash_subscriber` passes 17 (1 ignored).
  - With FIX_CAD alone, seeds 81701–81706 are clean.
- **Optional-subscriber watchdog probe:**
  - The probe runs the two 10,000-schedule threaded watchdog sweeps, with `Node::open` using all-optional and mixed `ToyIndex` subscribers.
  - That makes 4 runs in total. A hang panics at the 60 s watchdog.
  - Throughput under box load is about 1.5–3 schedules/s.
  - Result: all 4 runs pass with no hang (10,000/10,000 schedules each, about 4,610 s each, EXIT 0).
- **Not run:** the fuzzctl smoke (it would need a fresh optimized build; the owner reported a pass). GATE-FAST was not rerun (the owner's gfast-ch2b-b3-r2c is EXIT 0).

Repro files, for the owner to check in:
- `/work/scratch/ch2b-b3-r1-opus/repro/fa4-81703.repro`
- `/work/scratch/ch2b-b3-r1-opus/repro/fa6-81707.repro`

Both are reproduced verbatim above.

## Required changes

1. **M-A:** clamp `cadence_from` to `progress` when `disconnect_tip` lowers it (in `disconnect_tip`, or after the stale-path call in `sync_step`). Check in `fa4-81703.repro`.
2. **m-B:** lower FA6's F to the lowest Rewind locator seen during step 6. Check in `fa6-81707.repro`.
3. Rerun `chainstate_crash_smoke` on at least two fresh seeds after both changes.

The following can follow in a later slice: m-C (generator ops), m-D (T_ack assert), m-F (hand-off API), n-G, n-H.

VERDICT: ACCEPT-WITH-CHANGES
