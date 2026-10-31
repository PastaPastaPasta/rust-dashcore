# CH1-node N7b-2 — Opus-h round 2 (narrow)

Scope: `git diff 288c2d996..8ecfadf39` (c8c1736cd and 8ecfadf39: `chain/abc.rs`, `chain/flush.rs`, `chain/tests/flush.rs`, CH1-node.md §4.5 and Q12, notes D47/D57/D60/D61). I reviewed it in a detached worktree at `8ecfadf39`. Core reference: v24.0.0-rc.3 `0364ed626e`, `src/validation.cpp`.

## Verdict

**ACCEPT WITH NITS.** m-1, m-2 and n-1 from r1 are closed. The moveback runs at Core's place with Core's height, and is skipped on every path where Core skips it. Both new tests are deterministic, and each fails on the mutant it names; I ran those mutants myself. I found no regression. The one nit is a missing test, not a code change.

## r1 items

### m-1 → D60: closed

- **Contract.** D47 item 1 now says "every `DriverEvent` with `Released` included (D60)". D57 ends with the reason, and D60 states the reason and the hang. `FlushBell`'s doc (`chain/flush.rs:150-153`) already said "for every event it hands V". `DriverEvent` has four variants (`flush/driver.rs:34-44`): `Released`, `PruneClears`, `Failed` and `Crashed`. All four are covered by the wording. The FlushDue post rings too (`flush/hub.rs:201`).
- **Model W.** It now rings for every event it models: `release()` (`tests/flush.rs:162-169`, new), `send_under` → `ring()` (PruneClears), `fail_job` (Failed), `flush_due` and the armed requests. It has no `Crashed` path, which is fine, because no V test needs one. A W that ends drops the bell sender, and V's disconnected-receiver rule (D45) covers that.
- **No other untimed wait is missed.** `drain_fanout` (`abc.rs:262-268`) is V's only `wake.park()` (grep). Every other `answer_due` call is gated on `take_ring` (`actor.rs:114`, `:729`, `:1110`). So the ring on `Released` reaches all four of D45's waits.
- **Production.** The ring itself is the adapter's (ND1 2b and CH2b b3), and D60 records the hand-off. Nothing on this branch can enforce it. That is accepted as D47's contract.

### m-2 → D61: closed. Order and height match Core exactly

| Core `DisconnectTip` (rc.3) | V `disconnect_tip` (`abc.rs:582-622`) |
|---|---|
| `:3262-3265` `ReadBlockFromDisk` fails → `return false` | `:589-595` `read_block` `Err` → `Ok(false)`, before the moveback |
| `:3281-3284` `DisconnectBlock != OK` → `return false` | `:596-599` `Ok(false)` → `Ok(false)`, before the moveback; a `Fatal` propagates by `?`, before it too |
| `:3285-3287` `view.Flush`, `dbTx->Commit` | inside `connector.disconnect_block` (CH1-node §4.5) |
| `:3292-3301` `max_height_first = pindexDelete->nHeight - 1`, then lower and log | `:603` `move_prune_locks_back(e.height)`, where `e = entry(d)` and `d = tip()` = `pindexDelete`; the hub subtracts 1 (`flush/hub.rs:905-906`) |
| `:3304` `FlushStateToDisk(IF_NEEDED)` | `:604` `at_site(IfNeeded)` |

All three disconnect callers go through this function: ABC (`abc.rs:465`), `InvalidateBlock` (`failure.rs:42`) and SB4's `enforce_block` (`locks.rs:410`, Core's `MarkConflictingBlock` → `DisconnectTip`). So there is no second disconnect path that skips the call. The call is infallible and needs no host, so making it outside `with_flush` is right: it hands nothing to `FlushReleases`. The hub's `fetch_min(h−1) > h−1` filter is Core's `height_first <= max_height_first` → `continue`, inverted. The doc updates (§4.5 pseudo-code and prose, Q12, the trait doc) all agree with D61.

### n-1: closed

The hold test now waits for `!dues.is_empty()` before `flush_due()` (`tests/flush.rs:1483`). No ring comes before the hold, so the only answer that can satisfy the wait is the drain's (all non-drain answers are ring-gated). Together with the existing `dues.first() == Some(Some(0))`, the freeze can only come from wait (d).

## Determinism of the new tests

**`a_flush_due_kept_for_the_job_in_flight_is_answered_after_its_release` (`:1609`).** The "≥3 answers" argument holds. `Parker` keeps one permit (`dash-chain/src/notify.rs:41-77`), and the idle `flush_due` ring leaves it set. Serve's select takes the channel slot, not the permit. Nothing else parks on `wake` before the hold, because only `drain_fanout` parks. The only feed is the gated subscriber's (`Rig` registers none). It pops nothing until `go`, and `go` comes after the freeze, so no producer-wake can add a second permit. The trace is: answer #2, then park (takes the idle permit), then answer #3, then park for good. After that, only `release()`'s ring can wake V. With the ring removed, the drain never answers again. The test then fails on `wait_for`'s `SOON` timeout instead of hanging, and V is left parked. That only matters for the mutant run. Ordering, not timing, decides the outcome. The test passed 30/30 runs at load ~150–220.

**`a_disconnect_moves_the_prune_locks_back_before_its_flush_site` (`:1651`).** It is single-threaded through V's own calls: `rig.accept` is synchronous. `n = sites.len()` at the call, plus the model's `open.is_none()` assert, pins the call to "after the previous step's `after_step`, before this step's `at_site`". `c == k+1` pins it after this disconnect's commit. `sites[n].at == the disconnected block` and `h == m.height` pin the height. It passed 30/30.

## Findings

### n-1 (nit, test gap): nothing pins that a failed `DisconnectBlock` does not move the locks back

- V: `dashd-chainstate/src/chain/abc.rs:596-603`. Core: `validation.cpp:3281-3284` returns before `:3292`.
- **Schedule:** in a two-block reorg where `DisconnectBlock` refuses `main[0]`, Core moves the locks back once (to h1) and then aborts. A V that called `move_prune_locks_back` before testing the result would move them back twice, to h0. It would also log a "%s prune lock moved back to 0" line that Core never prints, before the `AbortNode`.
- **Evidence:** the mutant `let ok = disconnect_block(..)?; move_prune_locks_back(h); if !ok { .. }` (with the original call removed) **survives** all 190 `chain::tests::` tests. The code today is correct; this is only a regression guard.
- **Ask (optional, at the next touch of `tests/flush.rs`; not merge-blocking):** reuse `RefusesDisconnect` (`tests/failure.rs:364`) with `ModelW` and a commit counter, and assert `movebacks == [(2, _, 1)]` plus the `AbortNode`.

### Observation for CH2b (outside N7b-2's diff, no action here)

`flush/hub.rs:901` and `crash/chain.rs:679` cite `validation.cpp:3290-3297`. That is v24.0.0-rc.1's numbering (the comment is at rc.1 `:3290`, rc.2 `:3291`, rc.3 `:3293`). At the pinned rc.3 the block is `:3292-3301`, as D61, `abc.rs:600` and `flush.rs:73` say. The fix is a cite refresh at CH2b's next touch (backlog).

## Mutants I ran (own worktree and target)

| Mutant | Test | Result |
|---|---|---|
| A: `ModelW::release` rings nothing | `a_flush_due_kept_…_after_its_release` | CAUGHT (W's `wait_for` timeout at `:238`) |
| B: moveback before the `!ok` check (once, original removed) | all `chain::tests::` | **SURVIVED** → n-1 |
| C: moveback after `after_step` and `drain_fanout` | `a_disconnect_moves_…` | CAUGHT (`:1674`) |
| D: moveback before `disconnect_block` | `a_disconnect_moves_…` | CAUGHT (`:1672`, commit count) |

I didn't rerun the owner's "removed" and "wrong height" mutants. The test's `[2, 1]` and `h == m.height` asserts kill them by construction.

## What I ran

- `cargo test -p dashd-chainstate --lib -- chain::`: 200 passed.
- `cargo test -p dashd-chainstate --lib`: 472 passed, 0 failed (654 s at load ~150–220).
- Each of the three changed tests 30× through the test binary: 30/30 each.
- Mutants A–D above (`/work/scratch/ch1-n7b2-r2-opus/mut.py`). The worktree was clean after the runs.
- I did not run the integration test binaries (`tests/*.rs`). The diff touches only the lib's `chain` module and its unit tests, and gfast on 8ecfadf39 is GATE EXIT 0.
