# CH2b b3 subscribers — Sol round 3

Reviewed `node/ch2b-b3` at `13cd6c373` in a detached scratch worktree, focusing on the eight r2-fix commits after `1cffd18f9` (excluding the train-14 merge), CH2 §4.2/§4.4/§7.2 b3, and D45–D50. The branch under review was not edited. I found no new Blocker or Major state defect.

## Findings and limits

### Minor (deferred) — `open` still has two blocking schedules outside D47's guard

`dashd-chainstate/src/flush/hub.rs:304-308, 366-375, 392-400`.

D47 closes the round-2 same-thread, opened-but-unstepped case: registration records an owner, and a window waiter rechecks outstanding gate and commit obligations. If a driver is handed to another thread but has not stepped there yet, that new thread can still block in `open` while W waits for the handed-off driver; the previous owner can instead get a false `WouldDeadlock`. The `HoldActive` park also has no owner check. Both gaps are documented in D47. A repository-wide call-site check found no production `SubscriberDriver::open` in `dashd-node` at this revision (`dashd-node/src/chainstate.rs:150` uses `SubscriberHub::new()`), so neither is a current node schedule. The eventual index adapter needs an explicit handoff/nonblocking open policy before it shares a driver thread with registration.

### Minor (accepted availability consequence) — the eager rewind can require old-branch files Core's `Init` does not

`dashd-chainstate/src/subscriber/driver.rs:366-367, 422-431, 1147-1172`.

For a mandatory data object that requests blocks or undo, a disabled index at L=9 can pass the active-chain prune check after G moves back to 6, yet `rewind_commit` needs old blocks 7–9. If those were pruned while the index was disabled, `open` now returns a `Blocks` error where Core's `Init` can start without an eager durable rewind. This is observable as an additional startup refusal. It is an acceptable consequence of D45's required stronger R11a guarantee, provided the future production adapter surfaces a clear rebuild/refusal path. Failure before the batch drops the registration and its prune lock; a guarded batch or WAL failure trips the fail-stop latch. I found no half-published hub locator or W wait left behind by this path.

### Nit (deferred) — D48's skip has no Core warning line

`dashd-chainstate/src/subscriber/driver.rs:824-829`.

The off-branch predicate and skip match `BaseIndex::ChainStateFlushed` (`index/base.cpp:357-363`), but Core logs a warning and this driver has no log sink. The locator result is correct; index logging is already a follow-up in D48.

## Round-2 disposition

| Item | Status at `13cd6c373` |
|---|---|
| B1 | **Closed.** The L=9/G=6 reopen now writes the LCA locator, state, undo and archives durably before `open` returns. The separate step-3b window test still shows that registration waits until release; the driver then repairs the locator. A failure returns an error and deregisters. |
| M2 | **Closed for the r2 A6/A7/B6/B7 backlog.** `Toward::Flushed` applies its queued connects and disconnects before the tip commit, and both stale archive rows are present. At the journal limit D49 deliberately drops later off-leg connects to let V's queue drain; this bounded-memory exception is observable and differs from Core's unbounded listener. |
| m3 | **Closed for the reported unstepped owner's window wait.** D47 refuses that `open` with `WouldDeadlock`, including a pending gate installed after the wait begins. The two broader scheduling gaps above remain for future wiring. |

## New-code and Core assessment

- **D45:** `register` and the prune lock precede the check and rewind. A W window that starts before registration excludes it until release; one starting after registration sees the old L in its gate. The rewind lowers `best_block`/the prune lock before reading old rows, synchronizes files and WAL, then puts rows, state and locator in one Durable batch. A successful open can delay W until the driver next steps, but it does not require V's tip mutex while W waits. Core's `Init` only changes the in-memory best block; D45's durable rewind and possible startup refusal are stricter and observable. They are acceptable for CH2's R11a rule.
- **D46/D49/D50:** Optional flush targets retain queue order until pressure reaches the journal limit. At the limit, a checkpoint at p makes room while p is on the target leg; off-leg connects are dropped so pending disconnects can be emitted. Controls bypass Q, and D50 clears `posted` on the answering `ChainStateFlushed` even when that flush writes no locator. The scripted pending-fanout and no-write-then-repark tests passed. The threaded watchdog currently constructs mandatory subscribers only, so optional liveness still lacks that independent scheduling test.
- **D47:** The owner/outstanding check has no window/state/table lock inversion in the paths reviewed. Its 20 ms recheck catches an obligation published after the wait begins. The current node has no subscriber driver adapter, as noted above.
- **D48:** A flushed tip off p's path now skips as Core does, including D37's handoff recheck. A successful commit remains at the flushed tip and capped; Core commits its current best block. The cap is an intentional CH2 guarantee and is observable when p is ahead of the flushed tip.

## Validation

- `CHAINSTATE_CASES=5000` `chainstate_crash_smoke` passed on fresh seeds `70301` and `70302`: each had 5,000 images, 5,000 branches, zero truncations and `found {}` (479.91 s and 460.29 s under box load).
- All 28 `subscriber::driver::tests` and all 14 `flush::hub::tests` passed. These include the r2 L=9/G=6 open and window methods, the A6/A7/B6/B7 archive backlog, D47's unstepped owner, D48's off-branch skip, D49's pending fanout, and D50's no-write `FlushDue` answer.
- Unplanted S11o passed (6 images, 13 branches, `found {}`); S2b passed for both synced and syncing subscribers (9 images, 20 branches each, `found {}`).
- With its fix removed, `NoOpenRwd` found R11a (and D1b) on S11o. `X19` and `T36` each found D1b on S2b. `T36-S11r.repro` still found R11c, and `checked_in_subscriber_repros_replay` passed its planted and unplanted checks.
- `git diff --check 1cffd18f9..13cd6c373 -- dashd-chainstate docs/node-program/designs/CH2b-b3-notes.md` passed. The optional node-fuzz smoke was not run.

VERDICT: ACCEPT
