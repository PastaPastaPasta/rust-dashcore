# CH1-node A-1b slice: review round 2 (Opus-h, narrow)

- **Head:** `599a5c5df` on `node/ch1-node-a1b`. The r1 fixes are `1b57f269a`. The later commits follow-merge T21 (D68). None of them touches `actor.rs`, `signals.rs`, `tests/queue_hooks.rs` or `tests/signal_wait_model.rs` after `1b57f269a` (`git diff 1b57f269a 599a5c5df --stat` on those paths is empty).
- **Round 1:** `reviews/CH1-a1b-r1-opus.md` (CHANGES: B1, N1–N9).
- **Ran:** everything on a `git archive` copy of `599a5c5df` under `/work/scratch/ch1-a1b-r2-review/src`, with its own reflink-seeded `target/` and 12 jobs.
  - `cargo test -p dashd-chainstate --lib chain::`: **299 passed, 0 failed**. The owner's 289 used a narrower filter.
  - `--test signal_wait_model`: **3 passed** (32.9 s).
  - 8 hand mutants (table below). The model's MM9 was also run standalone with `rustc --test`.
- **Not re-checked:** the `gfast-ch1-a1b4` gate result, and N9's PF0 measurement (outside this round's scope).

## Verdict: ACCEPT WITH NITS

- Every r1 finding is closed as claimed.
- Each of the six named mutants (M1, M16, M2, M5, M26, M9), and the N1 guard, is killed by the test the owner names, at the assertion that should catch it.
- The r1 diff adds no defect in the wait/wake order. Dropping idle mode does not change behaviour, and makes the wait check `done` between commands.
- Four nits remain. Three are contract or record text, and one of those is a wrong reason in the text P3 S6b codes against. None blocks.

## 1. The r1 findings

| r1 | Claim | Checked |
|---|---|---|
| B1 | `a_wait_serves_held_commands_first` (`queue_hooks.rs:364`) kills M1, M16, M2 | **Closed.** All three are killed (table). The test is deterministic: `serve` takes `held` before any lane select (`actor.rs:902`), and `logged` and `stored` are both `Query`, on the same lane. |
| N1 | a wait after an `AbortNode` serves nothing | **Closed.** `actor.rs:892-894` returns early. `fail` (`:954`) keeps the first fatal. `a_failed_wait_serves_nothing_more` kills the early-return removal (`(Ok(()), 2)`: `done` is polled and the queued query is served). The hook doc says "return it, and do not wait again" (`signals.rs:108-110`). |
| N2/N3/N8 | contract text | **Present** in the trait docs (`signals.rs:104-110`, `:132-133`, `:235-239`), D67 r1 (notes `:210-211`, `:216`) and §8.4 (`CH1-node.md:1021`, `:1043-1044`). Precision: see §3, nits R2-1 to R2-3. |
| N4 | flag before list | **Closed.** In `signals.rs:458-462` (store order at `:461-462`), the Relaxed store is sequenced before ArcSwap's release store. A V that acquires the new list in `each` then reads `registered == true`. Read-read coherence keeps it `true` for that V's later loads, until an `unregister`. The `unregister` order is unchanged, and its stale `true` costs one empty iteration. |
| N5 | model gains init's ABC, `Headers`, MM1/MM2/MM9/MM10 controls | **Closed.** `each_mutant_of_v_fails_its_property` runs each mutant across the sweep and asserts the first failure's property. MM10 deadlocks on `Heading` (no drain while the actor awaits `Headers`). MM9's D67 record is wrong about which property fails (nit R2-4). |
| N6 | M5, nesting, M26, M9 tests | **Closed.** M5, M26, M9 (V's and `Blocking`'s) are all killed (table). `an_accept_met_at_the_queue_limit_is_deferred_whole` covers R3 at the barrier wait. M3 is killed there too, deterministically, by its last assertion: under M3 the accept runs inside the wait with the hook skipped (`in_wait`), so `accept_calls` stays 0. |
| N7 | idle mode dropped | **Closed.** See §2. M6 is gone. M21 is still equivalent: `serve` checks `stop` before it serves anything. |
| N9 | perf measurement | Not in this round's scope. |

### Mutants re-applied by hand (`tools/mutate.py`; each test run with `--exact`, sources restored and diffed afterwards)

| # | Mutation | Named test | Result |
|---|---|---|---|
| M1 | `serve` drops the held-first block | `a_wait_serves_held_commands_first` | **killed**: `["lane"]` ≠ `["held", "lane"]` (`queue_hooks.rs:397`) |
| M16 | held commands dispatched with `at_release = false` | same | **killed**: V panics "V's waits never nest" (`actor.rs:1535`), then `stored` is `None` (`:396`) |
| M2 | `idle_point` ignores `held` | same | **killed**: "no idle point while commands are held", 2 ≠ before (`:394`) |
| M5 | `limit_queue` runs the hook before the barrier wait | `c5s_hook_follows_the_queue_limit_at_both_sites` | **killed**: a pending count > 10 (`:510`) |
| M26 | C8 drops `!is_ibd()` | `no_accept_wait_in_ibd` | **killed**: `[PowValid(BlockId(1))]` (`:297`) |
| M9 (V) | `Until::received` returns `Ok` on a gone `wake` | `a_listeners_closed_wake_ends_vs_wait` | **killed**: `[Ok(())]` (`:593`) |
| M9 (`Blocking`) | `Blocking::wait_until` returns `Ok` on a gone `wake` | `a_closed_wake_ends_wait_until` | **killed**: `Ok(())` (`:549`) |
| N1m | no early return on `fatal` | `a_failed_wait_serves_nothing_more` | **killed**: `(Ok(()), 2)` (`:614`) |

M16 is caught through a `debug_assert`, so only test builds catch it. That is fine for a test.

## 2. The r1 diff (`1b57f269a`): no new defect

- **One-at-a-time serving (N7).** Composite mode used to call `release_point()` after each served command. That drained a snapshot of both lanes, and `held` too, with `at_release = true`, checking `stop` between commands. Now every iteration of `serve` (`actor.rs:895-951`) does the same, one command at a time:
  1. `until.over()`;
  2. `stop`;
  3. `held` first;
  4. one select over the waiter's receiver, the bell and the live lanes.

  The new form is strictly finer:
  - **`done` is rechecked after every command.** A flood of commands can no longer hold a wait open past its condition.
  - **The bell is answered between commands, not only between drains.**
  - **`release_point`'s `debug_assert!(in_composite)` is no longer reached from an idle-point wait.** That assert was the only reason idle mode existed.

  The commands served, their order (held, then lanes) and their `at_release = true` are unchanged. Class C and outside-IBD `ProcessBlock` are still deferred, and the main loop runs them before the next idle point (`actor.rs:1160-1162`).
- **The idle point and `held`.** `idle_point` (`:1508-1513`) runs only with `held` empty. `held` grows only while `holding` is set (`dispatch`, `:1209`), and then `queue_hook` skips anyway. So the idle-point wait cannot take from `held`, and R2's "the idle point never takes from held" holds by construction.
- **N1's early return** comes before `over()`. A re-wait after a fatal therefore polls nothing and serves nothing. `wait_with` returns that fatal whatever the hook returned. With the early return, `fail`'s `get_or_insert` cannot actually meet a second fatal inside one `VWait`. That makes it belt-and-braces, which is harmless.
- **Wake order is unchanged.** `Cond` checks `done()` before the first select and after every ring. A disconnected `wake` maps through `received`, which checks `done()` again. There is no lost wakeup on V's side, and no spin: the first disconnected receipt returns.

## 3. Contract text for P3 S6b

The text is precise enough to code against, with the corrections below. The re-entry list is complete. The inline signals a served class R/M command can raise are only `accepted_block_header` (`accept.rs:64`), `notify_header_tip` (`:129`) and `block_checked` (`:156`). `abc.rs:644` is inside an ABC step, which is class C and never served in a wait, and V never calls `synchronous_updated_block_tip` or `initialize_current_block_tip` there.

## Nits

### R2-1. The reason given for "never re-entered" is wrong in IBD.

- **Where:** `signals.rs:107-108` ("V serves no `ProcessBlock` accept in the wait") and notes D67 r1 N2 (`CH1-node-notes.md:210`, "V serves no `ProcessBlock` accept in a wait (R3)").
- **What actually happens in IBD.** V does serve `ProcessBlock` accepts inside a C5 `release_point` wait. The C5′ test in `handle` is false, so the accept runs, with its tail deferred. The model says the same (`signal_wait_model.rs:237`, `Cmd::Block(c) if self.p.ibd`).
- **The conclusion still holds:**
  - **In IBD,** C8's guard re-reads `is_ibd()` (`accept.rs:298-299`). On the same tip, at a later clock, the latch cannot clear: it clears only when the tip or its work moves, which happens only in ABC, which is class C and deferred.
  - **Outside IBD,** the accept is deferred whole.
  - **`before_accept` is never entered in IBD,** and the latch never returns to true, so its wait sees no accepts at all.
- **Why it matters for P3.** P3 S6b may otherwise assume that while its `release_point` hook waits, no block is accepted. In IBD that wait can see headers inserted, `HAVE_DATA` set, and `block_checked` fire for a rejection.
- **Suggested text:** "outside IBD V serves no `ProcessBlock` accept in the wait (R3); in IBD it may, but C8 never fires in IBD, so `new_pow_valid_block` is never re-entered, and `before_accept` is never called in IBD."

### R2-2. "A bounded channel of capacity at least 1" excludes unbounded channels.

- **Where:** `signals.rs:235`, `CH1-node-notes.md:211`, `CH1-node.md:1044`.
- **The problem.** Read literally, the text excludes an unbounded channel. Every test uses one (`Latch`, `Closing`, `a_closed_wake_ends_wait_until`), and it meets the requirement.
- **Suggested text:** "unbounded, or bounded with capacity ≥ 1 (never a rendezvous channel)".

### R2-3 (optional). `done`'s lock under LO-5.

- **Where:** `signals.rs:106-107`, `CH1-node.md:1044`.
- **The gap.** The text says `done` "takes and releases its own" locks. `done` runs on V's thread before every select, so V waits behind whatever lock it takes. §8.4 states LO-5's condition for C8's header cells (`CH1-node.md:1034`) but not for `done`.
- **The rule that is missing.** No thread may hold `done`'s lock while it waits on V. An example is P3's actor holding its queue mutex while it awaits a `ProcessHeaders` reply.
- **Fix:** one clause, "`done`'s lock is one V may wait behind (LO-5): no holder waits on V".

### R2-4. D67's MM9 record names the wrong property.

- **Where:** `CH1-node-notes.md:215` says MM9 "breaks the bound at init 3 (at 2 it never passes the bound of 1)".
- **What the test asserts.** `"cannot complete"` (`signal_wait_model.rs:575`), and that is what happens.
- **What I ran.** The model standalone, with `Mutant::WaitOnPreStart` across the sweep. The first and only failing point is `(hold 2, init 3)`: "cannot complete from `Tail { s: INIT, left: 3, waiting: true, … }`".
  - The hold's two pushes leave `q = 2 > B` on a `PreStart` queue at init's first loop head.
  - The actor starts only after init replies, so nothing drains.
- **Fix the record:** "MM9 deadlocks at init's first loop head after the hold (no drainer before init replies)". Note also that `(hold 0, init 3)`, which would show it without the hold, is not in the sweep. That is optional.
