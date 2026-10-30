# O2b-3b round 2: Opus-h confirmation of the M-1 fix

- Branch `node/o2b-3b` at `d46b3e4fd`. Scope: `git diff 9b45a5655 d46b3e4fd`.
- Built and run in a detached worktree (`/work/scratch/o2b-3b-r2-review/wt`, own reflink-seeded target, 12 jobs). Both have been removed.
- Baseline: `l1_chain_diff` passed 10, with 1 ignored. The lib `diff::` tests passed 3.

## Verdict: CHANGES REQUIRED (small)

M-1's core is fixed:
- Core's step always runs.
- A disconnect or queue count on one side only is compared, never refused.
- The drain normalisation is exact.

One fix is still needed. A both-sided reorg is refused without comparing three post fields that V reads and that agree with Core on a real reorg. So a node-only difference in them is still filed as out of domain, which is M-1's class. The fix is one line plus a planted test; it is verified below.

## Findings

### R2-1 (should fix): `same_reorg` lets node-only `deployments`, `best_cl` and `sporks` differences through as `Refused(Disconnect)`

- **Where:** `node-l1/src/chain/diff.rs:162-172` (the doc at :164).
- **What it compares:** `same_reorg` compares reply, events, down, tip, best_header, ibd and tips.
- **What it skips:**
  - `flush_delta` and `persisted`. These come *before* `utxo` in `compare`'s order, so "what follows them" does not cover them.
  - `best_cl`, `sporks` and `deployments`.
- **Why the second group matters:** V really reads all three (`node.rs` `post()`), and none is explained by the stand-in connector. `ibd` and `tips` also come after `utxo`, and they are compared, so "what follows them" is not the rule the code applies either.
- **Shown:** I planted a node-only difference on the real `fork 1 2` reorg step, Core's step left as it was:
  - `post.deployments = {"planted": true}` gives `Refused(Disconnect)`.
  - `post.best_cl = Some(None)` with `post.sporks = Some(vec![])` gives `Refused(Disconnect)`.

  A V-only bug in reorg-sensitive state, such as a versionbits cache not reset on disconnect showing in `getdeploymentinfo`, would be out of domain where Core shows nothing.
- **Fix, verified:** add `flush_delta`, `persisted`, `best_cl`, `sporks` and `deployments` to the tuple.
  - On the real fork all five already equal Core's. Only `flushes` differs (V: `None`).
  - With that change all 11 tests still pass, including `a_disconnect_on_both_sides_is_refused`, which uses the real V against the real Core.
  - Both plants become `Diverged`.
  - Add a planted test for at least `deployments` and one other field.
- **Optional:** `index` can be narrowed too. On the real fork, `Some(index_digest(&with_undo(&V entries))) == core.post.index` holds after the reorg. If `with_undo` (`tests/l1_chain_diff.rs:57`) moves into the runner, the index plane is compared with only the undo bit excepted. Today a node-only index difference behind a reorg (statuses of non-tip entries, have-data bits) is still `Refused`.
- **Label:** when `same_reorg` fails, `judge` returns `compare`'s first field. On a real V step that is `records`, `attempts` or `post.utxo`, all stand-in planes that the module doc calls "not findings". A genuine reorg divergence is therefore labelled as an artefact. Consider returning the field `same_reorg` found.

### R2-2 (nit / doc): with the real V, `QueueDrain` can never be judged, and no op is `Same`

- **Where:** `diff.rs:158`, `node.rs:20-23`, `run.rs:407`.
- **Why:** the QueueDrain arm needs `Judgement::Same`. A real V step has no records and no `utxo`, so `compare` always stops at `records`, `attempts` or `post.utxo`.
- **Shown:** probe on `extend main 6 headers hold / deliver b1 rev`:
  - the long connect is `Diverged(records)`;
  - every other op is `Diverged(post.utxo)`.

  `a_long_connect_on_both_sides_is_refused_as_a_queue_drain` (`tests/l1_chain_diff.rs:337`) reaches `Refused(QueueDrain)` only with Core's step standing in for the node's.
- **Why only a nit:** this fails closed, and it matches the brief ("the whole step compares equal").
- **Fix:** say in the docs that the queue-drain refusal, and with it the drain normalisation on a real step, cannot fire until a7 (records) and a10 (utxo). Today `node.rs:22-23` reads as if it does.

### R2-3 (nit): test gaps that the mutants expose

These mutants of `same_reorg` survive every test, lib and integration:
- dropping `core.reply == node.reply`;
- dropping `down`;
- dropping `best_header`, `ibd` and `tips` together.

`a_disconnect_on_one_side_only_is_a_divergence` (`:460`) plants only `events` and `post.tip`.

Add plants for:
- reply;
- best_header or tips;
- the R2-1 fields.

Killed as claimed:
- `(true, _) => Disconnect`, by the lib `a_disconnect_on_one_side_is_compared`. The integration tests alone do not kill it, because `same_reorg`'s events and tip checks catch the one-sided case anyway. The both-sides requirement is defence in depth.
- QueueDrain without `Same`, by `a_long_connect_that_queues_differently_is_a_divergence`.

### R2-4 (nit): `diff.rs:10` is 114 columns; `comment_width` is 100

## Question 2: is the drain-order normalisation exact? Yes

- **Why it is exact:**
  - `drained_at_end` (`diff.rs:175`) is a stable partition of Core's events into synchronous then queued, applied to Core's side only.
  - V's listener always builds sync then queued (`node.rs` `finish`), using the same five names as `QUEUED`. I checked them against the listener's `queue()` callers and against the pin's `validationinterface.cpp` `ENQUEUE_AND_LOG_EVENT` set, intersected with the shim's `EventLog`.
  - The shim runs the queue inline only at sync points (`OracleSetInlineValidationQueue`).
  - So without a mid-op drain, Core's order already is sync then queued.
  - The normalisation therefore erases only the sync/queued interleaving, which V's listener cannot express on any op. The order within each class is kept, and counts are compared.
  - Two orders that differ inside a class still diverge: the swap plant in `a_long_connect_that_queues_differently_is_a_divergence` is pinned.
- **Equivalent mutant:** normalising always (`if true`) survives all tests. That is expected, since it is the identity without a drain. It is an optional simplification that would also remove the dependence on the logged count.
- **Unlogged queued callbacks:** these (mempool, IS, governance) could in principle make Core drain mid-op with ≤10 logged. That would give a false *divergence* (fail-closed), never a false refusal. It is moot at the pin anyway: the shim's `LoadChainstate` passes no mempool (`options.mempool` unset), and there are no IS or governance ops.

## Question 1, the rest

- The `Disconnect` facts are effectively symmetric even though either half counts:
  - a signalled-only difference shows in `events`;
  - a left-chain-only difference shows in `tip`.
- Records, utxo and attempts cannot carry a node-only difference today, because V has none.
- The leak is the fields in R2-1, plus `index` (optional narrowing).

## Also checked, no issue

- `run.rs` census: a refused op is counted only as unsupported; Same and Diverged ops are counted as ops; verifydb is counted only on Same.
- `listen` now checks the result of register and sync.
- `image_step` refuses an image that disconnected.
- `Facts::core`: when the model does not know Core's tip, the disconnect is not shown, so the op is compared (fail-closed).
