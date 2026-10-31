# P3 S1 review: Opus (high), round 2 (narrow, r1 test fixes)

**Branch:** `node/p3-s1`, `147f09b89..a2958d750` (6a4147184, a2958d750): 5 files, +224/−13
**Scope:** the r1 F1/F2 fixes, the `Bells` change, D74–D75. Earlier code is not re-reviewed.
**Core:** Dash Core v24.0.0-rc.3 (`np` = net_processing.cpp, `v` = validation.cpp)

## Verdict: ACCEPT-NITS

r1 F1 and F2 are fixed.

- **No clock.** Neither probe uses a clock or a timeout. Both kill M2 and M3 deterministically: 8/8 each, on the full binary and on the probes alone, with every failing run finished in under 1 s.
- **No hang on the head.** Pinned to 2 CPUs with 4 busy hogs:
  - the probes passed 300/300 (slowest run 0.49 s);
  - the full `sim_p3_s1` binary passed 50/50.
- **`Bells` is sound.** It has no lost-wakeup or spurious-park path. In production the change costs two bool stores and one `notify_all` with no waiter per park.
- **The F2 line is Core's.** It matches v:4438 exactly: category, text and `ToString` form. It comes before np:5366's NET line.

One Low finding is left. Probe A does not order the other origin's push after the actor is asleep. So a nearby mutant, "drain once on entry to the wait instead of after each ring", passes A in 3 of 8 runs. A one-line fix makes it 16/16 (F1).

I recommend applying F1 on this branch before merge. The rest are nits.

No possible Dash Core security weakness was found.

## Findings

### F1 (Low): probe A pushes X before the actor is parked, so a drain-on-entry mutant passes it 3/8

**Where:** `dashd-p2p/tests/sim_p3_s1.rs:597-603` (`WakeProbe::wait_quiescent`)

**Mutant M6 (mine):** in `wait_block` (`blocks.rs:124-134`), call `self.drain_sync_signals()` once before the loop and drop the drain after `wait_doorbell`. It is a plausible regression: someone "simplifies" the wait so it drains once on entry.

**Schedule:**
1. The actor sends `ProcessBlock(B)`. Under M6 it then drains once, polls, and parks.
2. V takes the command. In `wait_quiescent`, the probe pushes X and calls `republish` at once.

If V's push and `republish` land before step 1's drain, that drain takes X. C5′ is then quiescent and A passes. Otherwise X is never drained and A reports the deadlock. That race decides M6: caught 5/8 on the full binary and 5/8 on the probes alone.

The test's doc says "Only the actor's drain inside its block wait can release it". That holds for M2, which has no drain in the wait at all. It does not hold for a drain that runs only on entry.

**Fix (verified):** make the push wait until the actor is asleep in its block wait. Its entry is then behind it, and only a drain after a ring can take X:

```rust
if self.other_origin.swap(false, Ordering::SeqCst) {
    self.shared.signals().bells.wait_parked();
    self.shared.block_checked(BlockHash::from_lendian([9; 32]), None, false);
    self.chain.republish();
}
```

This adds no hang risk. V took P's command, so the actor is already inside `wait_block`, and on the head it parks once its loop has run.

| Run | Result |
|---|---|
| M6 with the fix | caught 16/16 unloaded and 16/16 under the hogs recipe |
| Head with the fix, probes alone, unloaded | 50/50 |
| Head with the fix, probes alone, 2 CPUs + 4 hogs | 200/200 (slowest 0.74 s) |

M2 and M3 are unaffected: the extra wait comes before anything they change.

### F2 (Nit): D74's "No run hung" doesn't hold for the whole suite under M3

**Where:** `docs/node-program/designs/P3-S1-notes.md` D74 (Evidence); the test is `sim_p3_s1.rs:343`

Under M3, one of my 8 full-binary runs hung in the older `getpeerinfo_does_not_block_while_the_actor_waits_on_process_block`, not in a probe. It is a natural lost wakeup: that test has no reply hook. My M7 (below) did the same, 1/8.

- **On the head:** the test is fine, 50/50 under the hogs.
- **The probes:** they never hung.

So the claim is true of the probes but not of the suite. Either say "no probe hung" in D74, or give that test the same `set_reply_hook` check. The second is optional; the probes already pin the waker.

### F3 (Nit): `Bells::is_parked`/`wait_parked` are ungated public API with test-only callers

**Where:** `dashd-p2p/src/processing/signals.rs:152-164`

The only callers are in `tests/sim_p3_s1.rs`. In a production build, `wait_parked` is a public call that blocks forever unless the actor's wait parks.

The crate already has a `sim` feature (`Cargo.toml:21`, which gates `pub mod sim`), and the probes need it anyway. Put the two methods behind `#[cfg(feature = "sim")]` (or `cfg(any(test, feature = "sim"))`).

The `parked` bookkeeping in `wait_doorbell` can stay unconditional, because it is negligible.

### F4 (Nit): citations and the D71 cross-reference

- **The NET line is np:5366, not np:5367.** At v24.0.0-rc.3, np:5366 is `"Received mutated block from peer=%d\n"`. D75 cites np:5367; my r1 had the same off-by-one. The `blocks.rs:94` comment (v:4434-4440) and the test's v:4324 are right.
- **D71 still reads as before.** Its row still says the ring-alone block-wait case "moves to S4/S6". D74 corrects it, but a reader of D71 alone won't see that. Add "(narrowed by D74)" to D71's text.

### F5 (Nit): the F2 test does not pin the line's position relative to the NET line

**Where:** `sim_p3_s1.rs:548`

`Lines("validation", …)` records one target, so the test cannot see the order against the `net` line. By inspection the code's order is Core's: `blocks.rs:91-97` emits the `validation` line, then the `net` line, and both come after `received block`.

If the order matters to O11's log contract, record both targets and assert the ordered subsequence: `received block`, `Block mutated: …`, then `Received mutated block from peer=…`.

## Notes (no finding)

**`Bells` soundness (`signals.rs:105-164`).**
- **Ring against park.** `ring` sets `rung` under the mutex and then calls `notify_all`. `wait_doorbell` checks `rung` under the same mutex before it waits, and `wait_while` re-checks after spurious wakeups. So no ring is lost, as before.
- **Two kinds of waiter.** Each kind's predicate is re-checked under the lock:
  - the actor waits for `rung`;
  - the test's `wait_parked` waits for `parked && !rung`.

  `ring` notifying both, and a park notifying both, can only cause an extra re-check, never a missed one. In production, the park's `notify_all` has no waiter; `parking_lot`'s fast path is one atomic load.
- **`is_parked` after a ring.** `ring` makes `rung` true under the lock, so `parked && !rung` is false from the ring until the actor itself clears `rung`. If the actor was asleep, it then clears `parked` too. If it was running, it took the fast path and never parked. It can only become true again at the next park, which comes after one full loop: `wait_doorbell` → drain → poll → park.

  So "parked after a ring" really means "drained and polled since the ring". That is exactly what `DrainedOrStuck` and the reply hook rely on: the queue's `wait_until` rings (signals.rs:421/427) before calling `wait`.
- **No spurious park.** `parked` is only set inside the `!rung` branch, and is cleared on every exit from it.
- **Behaviour is unchanged.** The fast path (`rung` already set) clears it without waiting, as the old `wait_while` did.
- **Cost.** Per park: `parked = true`, one no-waiter `notify_all`, then `parked = false` on wake. The owner's "one bool and one `notify_all`" undercounts by one store. That is immaterial.

**The probes' own deadlock surface.**
- `WakeProbe::published` blocks V until the actor parks, so it is safe only for publications made while the actor is in `wait_block`.
- In `wake_case`, every publication after `set_listener` is V's, made while serving P's submit. The actor is then inside `wait_block`, because V has its command. No publication comes from the actor's thread.
- If the actor's pass panicked, V would stay in `wait_parked` on its detached thread, holding the chain. That is a leak, but the scoped join still returns the panic, so the test fails and does not hang.
- M2's path ends with no extra publication, so nothing waits on a park that cannot come:
  1. `DrainedOrStuck` returns `Interrupted`.
  2. `serve_block` returns `Err(Interrupted)` with no publication.
  3. The slot's waker rings.

**F2 against Core.**
- **Category.** v:4438 is `LogPrint(BCLog::VALIDATION, "Block mutated: %s\n", state.ToString())`. `debug!(target: "validation")` is the repo's mapping for `LogPrint(BCLog::VALIDATION, …)` (`dashd-node/src/logging/mod.rs:10`).
- **Text.** `ValidationError`'s `Display` (`dash-consensus/src/validation.rs:137-146`) is `ValidationState::ToString`'s invalid branch: `reason` alone, or `"reason, debug"`. That matches consensus/validation.h:105-116.
- **Which failure.** `check_merkle_root` returns the root mismatch before `bad-txns-duplicate`, as v:4333 and v:4343 are ordered. The test's block appends a duplicate transaction, which changes the root, so the first failure is `bad-txnmrklroot, hashMerkleRoot mismatch`.
- **`Tx64`.** It logs nothing, as Core's 64-byte branch returns without a line.
- **Isolation.** The new test is the only one in `sim_p3_s1` that reaches the callsite, so the interest-caching note holds.

**D74/D75.**
- **D74.** Accurate except F2's wording. My runs reproduce M2 8/8 on A and M3 8/8 on both. The "hold only the accept's ring" history explains why every publication holds V.
- **The D71 correction.** It is right that the ring-alone block-wait case is covered in S1 through C5′. The forms still owed to S4/S6 are those that need production release-point waits: the space wait and the region.
- **D75.** Accurate except the np line number (F4).
- **Untracked follow-up.** D75's follow-up, adding the line to §3.11 and O11, is recorded only in D75. `node/p3-design` (f82e7f9c1) has no "Block mutated" text yet. The manager should carry it to the design's next edit.

## Mutants and runs

All runs used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, a detached worktree at a2958d750, and a target under `/work/scratch/p3-s1-r2-opus` (0700), reflink-seeded from the owner's `rdc-wt/p3-s1/target/debug`. Each mutant was applied, built (`cargo test -p dashd-p2p --test sim_p3_s1 --lib --no-run`), its binary copied, and the worktree reset; `git status` was clean after each.

The "probes" column runs the two probe tests alone (filters `block_wait`, `the_reply_alone`). Every test binary run had a 120 s `timeout`; under the hogs recipe it was 300 s.

| # | Mutation | Full binary (8 runs) | Probes alone (8 runs) |
|---|---|---|---|
| — | head a2958d750 | 17/17 pass (lib 33/33) | — |
| M2 | delete the in-wait `self.drain_sync_signals()` (`blocks.rs:133`) | **fail 8/8**: A "the actor slept in its block wait with a signal undrained" | **fail 8/8** (A) |
| M3 | `DoorbellWaker::wake`/`wake_by_ref` no-ops (`signals.rs:173,177`) | A and B **fail 8/8** ("the reply was stored without waking"); in 1 of the 8 runs the binary then hung in the older getpeerinfo test (F2) | **fail 8/8** on A and B |
| M6 (mine, drain path) | drain once before the wait loop, not after each ring | fail **5/8**, on A only | fail **5/8** (F1) |
| M6 + F1 fix | as M6, with the probe change | — | **fail 16/16** (plus 16/16 under the hogs recipe) |
| M7 (mine, waker path) | after the first poll, re-poll with `Waker::noop()` (the waker is not re-registered) | A and B **fail 8/8**; in 1 of the 8 runs the binary then hung in the getpeerinfo test (F2) | **fail 8/8** on A and B |

**Load (BL-159 recipe):** each test binary was pinned with `taskset -c 30,31`, with 4 `while :; do :; done` hogs on the same 2 CPUs. The hogs were killed by PID through a trap; none were left over.

| Binary | Runs | Result | Slowest |
|---|---|---|---|
| head, probes alone | 300 | 300/300 | 0.49 s |
| head, full `sim_p3_s1` | 50 | 50/50 | 0.89 s |
| head + F1 fix, probes alone | 200 | 200/200 | 0.74 s |

**Not run:** the gate. I relied on the owner's `p3-s1-t5` EXIT 0 report, because the change is test-, sim- and doc-only apart from `Bells` and the one log line.

The scratch target, worktree and binaries were deleted after the runs. The logs are kept in `/work/scratch/p3-s1-r2-opus/logs`.

**Verdict: ACCEPT-NITS** (F1 Low, recommended before merge; F2–F5 nits)
