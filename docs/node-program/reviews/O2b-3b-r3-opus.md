# O2b-3b round 3: Opus-h review of the T23 rework

- Branch `node/o2b-3b` at `6f57ab657`. Scope: `git show 27862ced6` and `git diff 74a08278f 6f57ab657` (node-l1 `chain/{diff,node,run}.rs`, `tests/l1_chain_diff.rs`, O2b-diff-chain.md §2.3 and §4.5).
- Built and run in a detached worktree (`/work/scratch/o2b-3b-r3-review/wt`) with its own reflink-seeded target and 12 jobs. Both have been removed.
- Baseline:
  - `cargo test --locked -p node-l1 --features sut`: 39 binaries, 267 passed, 0 failed, 4 ignored.
  - `l1_chain_diff`: 12 passed, 1 ignored.
  - lib `chain::diff`: 3 passed.
- Plants and mutants: one build, with each mutant behind `R3MUT=n` and two plant tests added to `l1_chain_diff`, run against the real V and the real Core.

## Verdict: CHANGES REQUIRED (small), F1 only

**Conditional:** if a5 merges no later than O2b-3b, this becomes ACCEPT WITH NITS. `node/ch3a-a5` (c0afbd91c) no longer has the stand-in text, so with it the refusal and F1 go away. See F6.

**What I checked against the implementer's claims:**
- **The rework does what it claims.**
  - `reorg_compare` and `with_undo` are gone, and no plane is excepted.
  - On a non-reorg run, V's `index` digest equals Core's.
  - A planted status, have-data or undo bit on the tip or its parent is `Diverged(post.index)`.
  - A connect-time rejection (`mutate cb_amount`) is the same on both sides.
  - R2-1 is moot: on a refused op, V's post is down and nothing of it is read.
  - R2-2 and R2-4 are fixed.
- **One leak remains (F1).** `before_the_stop` accepts a V that *left out* signals before its stop. A plant on the real `fork 1 2` reorg shows it: V without its `NotifyHeaderTip` is still `Refused(Disconnect)`, and so is a V that signalled and checked nothing at all.
- **Reachability today.** The leak cannot be reached through the Runner yet. Every real V step diverges on a gap plane at the input's first op (F2), so no input reaches a reorg. It is the M-1/R2-1 class, and the fix is about 20 lines plus a test.

## Findings

### F1 (should fix): the prefix rule refuses a V that dropped its last signals before the stop

- **Where:** `node-l1/src/chain/diff.rs:178-197` (`before_the_stop`). Doc claims: `:7-9`, `node.rs:44-49`, O2b-diff-chain.md:330.
- **Why it leaks:** `starts_with` only says V did no *more* than Core and did nothing out of order. It cannot tell "V stopped before it got there" from "V never emitted it". Any tail omission right before the stop therefore passes, and an empty V passes every time.
- **Shown on the real `fork 1 2` reorg** (real V and real Core steps):

  | V's step | Judgement |
  |---|---|
  | as is: sync `[AcceptedBlockHeader, NotifyHeaderTip]`, queued `[]`, checked `[]` | `Refused(Disconnect)` |
  | its `NotifyHeaderTip` dropped | **`Refused(Disconnect)`** |
  | events and checks both emptied | **`Refused(Disconnect)`** |
  | its two signals swapped | `Diverged(events)` |
  | an extra sync signal | `Diverged(events)` |

  Core's step is sync `[AcceptedBlockHeader, NotifyHeaderTip, BlockChecked×2, SynchronousUpdatedBlockTip]` plus queued `[BlockDisconnected, BlockConnected×2, UpdatedBlockTip]`. A V-only bug such as "no header-tip notification when the new best header triggers a reorg" is therefore filed as out of domain.
- **Fix:** Core's step tells where V's stop falls. Two cuts are exact, and a third is a count check:
  - **queued:** `node_queued == core_queued[..first BlockDisconnected]`. This is exact, because Core queues `BlockDisconnected` inside `DisconnectTip` (`validation.cpp:3326`), while a step's `BlockConnected`s are queued only after the step (`:3797-3800`). If Core shows only `left_chain`, keep the prefix rule or compare.
  - **sync:** `core_sync == node_sync ++ rest`, where `rest` is empty or starts with a signal Core fires only from the disconnecting step on. Those are `BlockChecked` (`:3432`), `SynchronousUpdatedBlockTip` (`:3821`, `:4036`, `:4141`) and any evo signal fired from `DisconnectBlock`; enumerate them from the pin. Before ABC, `AcceptedBlockHeader` (`:4724`), `NewPoWValidBlock` (`:4832`) and `NotifyHeaderTip` (`:4897`) all fire, so omitting any of them makes `rest` start wrong.
  - **counts:** V's `SynchronousUpdatedBlockTip` count must equal the `UpdatedBlockTip` count in Core's queued cut. V's valid `checked` count must equal the `BlockConnected` count in that cut. This catches a dropped signal from an earlier ABC iteration of the same op.
- **Test:** add a planted test for the `NotifyHeaderTip` drop and the empty V. Both must be `Diverged(events)`.
- **Docs:** fix O2b-diff-chain.md:330, which says "having signalled and checked what Core's step did before it". That is true only after this fix.

### F2 (minor, the r2 label point carried): gap planes mask every later plane, so Diff mode reports only gaps

- **Where:** `compare.rs:72-107` (order), `diff.rs:14-22` ("Those divergences are the gaps', not findings").
- **What happens:** on a real V step, `compare` stops at the first of these:
  - `records` or `attempts`, on any op where Core has records;
  - `post.utxo`, on every other op.

  So everything after them is never looked at:
  - on a connect: events, tip, best_header, index, ibd, best_cl, sporks, tips and deployments;
  - on any op: index, flushes, ibd, best_cl, sporks, tips and deployments.
- **Shown:**
  - The `extend main 1` connect with a planted tip have-data, undo or valid-level bit, or a parent undo bit: the real node step is `Diverged(records)`, the same as unplanted. With Core as the node it is `Diverged(post.index)`.
  - The `time 3` op with planted `tips` or `index`: `Diverged(post.utxo)`, the same as unplanted.
  - From a restore, every Runner input ends at its first op with a gap label. That is also why the disconnect refusal (F1) cannot be reached through the Runner today.
- **Why minor:** the verdict is never wrong; it is always `Diverged`. But the label names a plane the module doc tells readers to discard.
- **Suggested:** when the first difference is a gap plane (`records`, `attempts`, `sparse records`, `post.utxo`, `post.flushes`), keep comparing. Report the first non-gap plane if there is one. Either name it in `Divergence` (for example `post.index (after records)`) or report the gap and add a `masked` list. This excepts nothing.
- **Test:** plant an index bit on a real V connect and expect `post.index` to be named.

### F3 (nit, tests): mutants of my own that survive

Each was run on the whole `l1_chain_diff` suite and the lib `chain::diff` tests.

| Mutant | Result | Why |
|---|---|---|
| R3MUT=1: drop the **sync** half of the events prefix (`diff.rs:191`) | **survives** | `a_disconnect_on_one_side_only_is_a_divergence`'s `other_signals` plant pops a sync signal (still a prefix) and pushes a *queued* one, so only the queued half fails. No test plants a sync-only difference before the stop. F1's test kills it. |
| R3MUT=3: any logged line counts as the stop (`node.rs:769`) | **survives** | The load's logger does see other lines (`init message: Verifying blocks…` at every load), so the matcher's specificity matters. No test has V stop for another reason, so nothing pins it. |
| R3MUT=6: any `*** ` abort line counts as the stop | **survives** | As above: a different Fatal in an op where Core disconnected would be refused under this mutant. |
| R3MUT=7: read the flag without V's stop | survives | Equivalent today: V logs the text only on its way to stopping (D55: replies are released after the sink). |
| R3MUT=2: drop the queued half | killed | `other_signals` |
| R3MUT=4: Core's disconnect = `BlockDisconnected` only | killed | lib `either_half_shows_core_s_disconnect` |
| R3MUT=5: checked prefix by length only | killed | `other_checks` |

**Suggested:** a unit test of the line predicate. Factor `fn is_disconnect_stand_in(line)` and check it against the real stop line and against another `*** <Fatal>` line and an init message. Better still, export the message as a constant from `dashd-chainstate` (`connector.rs:212`) instead of copying the literal at `node.rs:405`. Today the literal can drift without any check except `a_disconnect_on_both_sides_is_refused` failing, which is at least loud.

### F4 (nit, optional): on a refused op nothing V did before its stop is compared beyond events and checks

- **Where:** `diff.rs:178-180` ("Its reply and its post are the stop's").
- **What is lost:** V's header acceptance before ABC (best header, the new entry's status) is out of the comparison. A planted V post is `Refused(Disconnect)`, which is trivially so, since V's post is down.
- **One plane can be compared:** `best_header`. V's reader still reads after the stop (the removed `active_tip` relied on this). Core's `m_best_header` moves after a disconnect only through `InvalidChainFound` (`validation.cpp:2043-2044`) or `:3951-3953`, and either one shows as an invalid `BlockChecked` after V's prefix.
- **Suggested:** compare V's `best_header` to Core's `post.best_header` when Core's remaining checks are all valid. Otherwise say in the doc that the refusal concedes the op's post entirely. Which chain V was reorging to cannot be known before a5 either.

### F5 (nit, doc): O2b-diff-chain.md:330 overstates the refusal

See F1. §2.3 (`:118`) is accurate: BL-271 is real (`dashd-node/src/sut.rs:839`, `:863`), and the replay-latch gap matches `node.rs:59-64`.

### F6 (integration note for the manager): a5 retires the refusal and breaks two tests on purpose

- **The judge needs no change:** on `node/ch3a-a5` (c0afbd91c) `connector.rs:206` implements `disconnect_block`, and the stand-in text is gone from `dashd-chainstate`. No text means no stop, which means no refusal, so the judge drops the refusal without any code change.
- **Two tests do need a change:**
  - `a_disconnect_on_both_sides_is_refused` asserts `at_disconnect` (`tests/l1_chain_diff.rs:444`), so it fails loudly. That is good.
  - `a_disconnect_on_one_side_only_is_a_divergence` (`:464`) depends on V's stop too.
- **Who acts:** whichever of O2b-3b and a5 merges second must rework them (and delete `DISCONNECT_STAND_IN`, the logger hook and `NodeFacts::at_disconnect`). If a5 lands in T23 with O2b-3b, put that in the train's merge-compile check.

## Brief questions

1. **Can a real V-only divergence be refused?** Yes, by tail omission before the stop (F1, planted on the real reorg). The other routes do not refuse:
   - **Stop text from elsewhere:** the only producer is `connector.rs:212`, and it reaches the per-load logger only through `NodeAbort` (`abort.rs:47`). V's `tracing` lines do not go to this logger. A different Fatal cannot match, but nothing pins this (F3).
   - **Vacuous prefixes:** yes. This is F1.
   - **Wrong side's events:** not a problem. Both splits use `QUEUED`, Core's side is the drained copy (order within a class unchanged), and swapped or extra V signals diverge.
   - **Stickiness:** it cannot swallow an op V ran. The Runner ends the input at the refusal (`run.rs:409-413`), and `NodeSide::step` refuses before running anything (`node.rs:543-545`). A restore resets both the side's and the load's flag (new `Loaded`).
   - **A connect V never attempted:** Core's later connects are conceded by design. Only V's work before the stop can be held, which is F1's fix.
2. **R2-1:** moot. No post field is read on a refused op (V is down), and the label is the plane `before_the_stop` found (`reply` or `events`). What R2-1 guarded now lives in F1 and F4.
3. **Index exactness:**
   - After `extend main 1`, V's own digest equals Core's `post.index`.
   - The image test asserts every connected entry has `HAVE_UNDO`, and that Core's digest differs without it.
   - Planted tip `HAVE_DATA`, `HAVE_UNDO` and valid-level flips, and a parent `HAVE_UNDO` flip, are `Diverged(post.index)` with Core as the node. On the real node step they are masked as `records` (F2).
4. **Records and utxo:** they mask every later plane (F2). The verdict is still `Diverged`, but the label is a gap.
5. **When a5 lands:** the refusal vanishes with no code change, and the stop-text tests fail loudly (F6). A changed text fails the same test. A text that matches too much is not pinned (F3).
6. **Docs:** §2.3 is right; §4.5 overstates (F5). The module docs match the code except `diff.rs:7-9` and `node.rs:44-49`, which inherit F1.
7. **Tests:** see F3. The implementer's listed mutants: I re-ran the queued, checked and Core-half ones, and they are killed.

## Also checked, no issue

- **Logging order:** V logs the stop before any reply disconnects. `stop_serving` (`actor.rs:1210-1236`) calls the sink before `ReleaseParked` drops (D55), so the read at `node.rs:558` after `finish` cannot race the flag.
- **The image build** refuses an image op that stopped at the disconnect (`node.rs:520`).
- **`refused`:** V's stop without Core's disconnect is compared (lib test, and the `no_reorg` plant gives `Diverged(reply)`). Core's disconnect without V's stop is compared (`Diverged(events)`).
- **Line width:** no line over 100 columns in the scoped files.
- **Observed, not a finding:** `fork 1 2`'s first op diverges in `events`. This is the declared replay-latch gap: Core re-notifies the unchanged best header after its load, and V does not. `judged()` does not judge earlier ops, so the reorg tests are unaffected.
