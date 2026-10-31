# O2b-3b (diff_chain Diff mode) — Opus-h review, round 1

**Head:** `ea06a31d2` (`node/o2b-3b`), scope `01d837e4d..ea06a31d2`: `4fa734eb5` (D8), `72ffef991` (shape),
`ea06a31d2` (NodeSide + Mode::Diff). Core read at the oracle pin `77dbec824` (v24.0.0-rc.3 + the shim; rc.3 is its
ancestor, and the shim does not touch `rpc/mining.cpp`, `rpc/blockchain.cpp`, `core_read.cpp` or `univalue`). Line
numbers below are at the pin unless they say otherwise.

## Verdict

**ACCEPT WITH CHANGES: 0 Blocker, 1 Major, 6 Minor, 6 Nit.**

The shape port matches Core byte for byte for every branch it covers. The event plane, the D8 check and the NodeSut
scaffolding are sound. Diff mode cannot report "equal" today. Every live served op diverges at its first plane the
stand-ins cannot fill: `records` for a BLOCK, and `post.utxo` for everything else. So every Diff-mode input ends at its
first live op (probe below). The one path that turns a V/Core difference into "not a divergence" is the pair of
refusals that run *after* the op (M-1). They decide on V alone and skip Core's step. That is cheap to tighten now, and
it must be fixed before a7/a10 remove the stand-in divergences that mask it today. All five design-text corrections
(a)–(e) are correct (§6).

Verification run in a detached worktree at `ea06a31d2` with a seeded target. `l1_chain_diff` passed 8/8 with 1 ignored,
and `l1_chain_params` passed 5/5. A scratch probe and 17 mutants were run (tables below). The scratch was deleted
afterwards.

**Probe: Diff mode's first divergence** (`Runner::with_mode(.., Mode::Diff)`, scratch test, not committed):

| program | end |
|---|---|
| `extend main 1` | diverged `records` after op 1 |
| `time 3` | diverged `post.utxo` after op 1 |
| `extend main 1 headers` | diverged `post.utxo` after op 1 |
| `activate` | diverged `post.utxo` after op 1 |
| `cl_enforce` | diverged `post.utxo` after op 1 |
| `extend main 3 headers hold` + `deliver b1 rev` | diverged `post.utxo` after op 1 |

## Major

**M-1. The after-the-op refusals (`disconnect`, `queue-drain`) are decided on V alone, and Core's step never runs.**
`node-l1/src/chain/run.rs:381-389` runs the node's step first and returns `End::Unsupported` before
`self.core.step` (`:390`). `NodeSide::step` (`node-l1/src/chain/node.rs:535-542`) refuses on V's tip or V's
`BlockDisconnected`, and on V's queued count. So an op where **V** reorgs and Core does not (a chain-selection,
work-comparison or validity difference) becomes "out of domain, `unsupported-disconnect`", not a divergence. The same
goes for an op where V queues more than 10 signals and Core queues 10 or fewer (V connected more blocks than Core). A
V stop after a disconnect is also reported as `disconnect`, not as a stop. The declared limit is "V's disconnect is a
stand-in, so ops that disconnect are refused", and that limit stands. But as written, a V-only disconnect is the very
bug class the stand-in limit must not swallow.

Today it is unreachable in practice, because every input diverges at op 1 (probe). Once a7 (records) and a10 (utxo)
land, it becomes the only route by which a real difference ends an input as `OutOfDomain`.

Fix (no comparator relaxation needed):
1. Run Core's step for an op refused after running.
2. Refuse only if Core's step shows the same thing: a `BlockDisconnected` in Core's events, or Core's tip off its
   pre-op chain for `disconnect`; Core's queued-kind events (`UpdatedBlockTip`, `BlockConnected`,
   `BlockDisconnected`, `ChainStateFlushed`, `NotifyChainLock`) above the bound for `queue-drain`.
3. Otherwise, compare as usual. The events/tip plane then reports the difference.

A test is needed for each branch. The "V-only" side can be planted by injecting a Core step without the disconnect
into the decision function.

## Minor

**m-1. Events heard between ops are discarded silently.** `NodeSide::step` starts with
`self.loaded.listener.take()` (`node.rs:523`), and `listen` does the same after load (`:773-774`). Anything V signals
after the previous op's fence → sync is dropped unchecked: a late queued signal, a deferred task that ran after the
barrier, or work a future hub listener posts back into V. `finish` (`:616-625`) is one fence and one sync, not a
fixpoint (the design's §5.1 quiescence check). Nothing reaches this today: NodeSut has no hub listener but this one
(`grep register(` finds none in `dashd-node`/`dashd-chainstate` src) and no timer. It is still a silent pass. At op
start, treat a non-empty `Heard` as a divergence or a harness fault, not as something to discard. Keep the
post-load `take()` (load events are legitimately not compared).

**m-2. No test sees the signal sync (mutant M5 survives).** Removing `sync_signals()` from `finish`
(`node.rs:623`) leaves `l1_chain_diff` green. The signals thread delivers before `take()` in practice, so the queued
half of the event plane is guarded by timing only. Add a test-support delay on V's signals thread (or a listener
that sleeps in `updated_block_tip`), so that a missing sync fails `a_multi_block_connect_lists_its_events_as_core_does`
deterministically.

**m-3. A D8 failure shows as a bare `shim` divergence in the fuzz path.** `fuzz_one_in` maps the session start's
error with `map_err(|_| shim())` (`run.rs:713`). `check`'s per-leaf report is then lost, and every input of the run
diverges as `shim` with no reason. O2b §2.5 says a mismatch "aborts the leg (a harness error, not a finding)". Print
the `ShimFault` once (stderr), or panic on a `node:` fault at session start. Tests already see the text through
`unwrap`.

**m-4. D8's declared coverage is incomplete.** `core_shape` renders every `ConsensusParams` field. These
`ChainParams` fields are not compared, because Core's `ChainParamsJson` does not print them:
- `spork` / `spork_signers` (min keys and addresses: the SPORK op's reply and `post.sporks` depend on them);
- `credit_pool_period_blocks`, `assumeutxo` and `chain_tx_data` (consensus-relevant once evo or assumeutxo apply);
- network/policy fields.

The module doc (`params.rs:15-16`) names only the spork signers and `ext`. List every uncompared field, and say where
each is covered instead. Spork signers are covered by the SPORK reply and `post.sporks`, which Diff mode does not
reach until utxo lands. Only `l1_chain_diff`'s direct asserts hold them today.

**m-5. The queue-drain bound is conservative by one block, and its boundary test pins the conservative value.**
Core drains at an ABC iteration's start only with more than 10 callbacks pending (`validation.cpp:3826-3831`, called
at `:3870`). An extend connects one block per iteration (`ActivateBestChainStep` returns once the tip beats the old
one, `:3771-3774`) and queues two: `BlockConnected` at `:3909` and `UpdatedBlockTip` at `:3932`. So the first drain
is at the 7th iteration's start (12 pending), and a 6-block op is listed undrained. That makes the 5-block test right
and the rule sound, but `> 12` would also be sound for extend-only ops. Mutant M3a (bound 11) survives. M3b (12)
dies only on `a_long_connect_is_refused_as_a_queue_drain`, which asserts the conservative choice, not a Core fact.
Nothing shows that Core actually drains at 7 blocks (that the refusal is needed). Add a Core-only test: 7 held blocks
delivered child-first, so that queued events sit before the 7th `SynchronousUpdatedBlockTip` in Core's list. Keep
`> 10` (the doc explains it), or make the bound per iteration.

**m-6. Each half of the disconnect test is untested on its own.** Mutants M2a (structural check only) and M2b
(`BlockDisconnected` only) both survive. Only removing both (M2) fails `a_disconnect_is_not_served`. The structural
half (`left_the_chain`, `node.rs:553-559`) is the one that catches a V that leaves its chain without delivering
`BlockDisconnected` (for example, a stop mid-reorg), and no test reaches it. If M-1's fix reshapes this, test both
signals there.

## Nit

**n-1.** `listen` ignores `register_listener`'s `bool` and `sync_signals`'s result (`node.rs:772-773`). A
`false` there means the event plane is silently deaf. Return `Err`.

**n-2.** `block_rpc`'s doc (`shape.rs:192-196`) says the `-20` branch is unmodelled because V's other failures
follow an AbortNode. That holds for V. Core, however, has non-abort error states that reach `RPC_DATABASE_ERROR` (and
BIP22's `-25`) with the node still up: `evo/specialtxman.cpp:1018` and `:1086` catch an exception and set
`state.Error(e.what())`. Note this for a5, when INVALIDATE/RECONSIDER/PRECIOUS start using `block_rpc`.

> **Manager note (10-20):** n-2 is declined. At the pin, both cited sites run only after the node has already begun aborting, so they give no example of a `-20` with the node still serving. The `block_rpc` doc stands.

**n-3.** M7 (the reply double as the raw `f64`, not UniValue's 16-digit text) survives `l1_chain_diff`. For regtest's
constant `nBits` both parse to the same double, and the doc says so. Only `reply_doubles_are_univalue_text` guards
the port. That is fine, but a schedule with other `nBits` is guarded only by that unit test.

**n-4.** The restore step is not compared (`diff.rs:13-14`, `run.rs:357-359`), so an image-state difference surfaces
under op 1's name. Comparing the post fields of the node's post-build `SETTIME`-style read with Core's restore post
would localize it. Low value until a10.

**n-5.** `Diff::compare` drops `Skipped` (`diff.rs:112-114`), so D17's `verifydb` count is not in Diff mode's census.
None occur in the ops served today.

**n-6.** `Diff::start` builds the image (load-0), and `Runner::with_mode` → `reset` immediately rebuilds it
(load-1). That is one wasted 200-block replay per session.

## Mutants (each applied alone, then restored; `l1_chain_diff` + `--lib chain::`, plus `l1_chain` and the probe for M9–M11)

| mutant | result | killed by |
|---|---|---|
| M1 drop `has_txs` byte from `index_digest` | killed | 4 `l1_chain_diff` tests |
| M1b drop `CONFLICT_CHAINLOCK` from `INDEX_STATUS_MASK` | **survives** | (no entry has the bit; equivalent on these chains) |
| M1c drop `FAILED_MASK` from the mask | killed | `an_extend_only_program_is_the_same_on_both_sides` |
| M2 skip the disconnect check | killed | `a_disconnect_is_not_served` |
| M2a structural check only | **survives** | — (m-6) |
| M2b `BlockDisconnected` check only | **survives** | — (m-6) |
| M3a `QUEUE_LIMIT` 11 | **survives** | — (sound; m-5) |
| M3b `QUEUE_LIMIT` 12 | killed | `a_long_connect_is_refused_as_a_queue_drain` (pins the conservative value) |
| M3c `QUEUE_LIMIT` 1000 | killed | same |
| M4 queued events before sync | killed | 3 `l1_chain_diff` tests |
| M5 no `sync_signals` in `finish` | **survives** | — (m-2) |
| M6 drop `!new_block && accepted → duplicate` | killed (unit only) | `submitblock_reads_the_state_catcher` |
| M7 raw double in the reply | killed (unit only) | `reply_doubles_are_univalue_text` (n-3) |
| M8 BlockChecked `why` dropped | killed | `an_extend_only_program…`, unit test |
| M9 `compare.rs`: drop the `post.utxo` plane | survives `l1_chain_diff`; killed by `l1_chain` (injected differences) | probe's first divergence moves to `post.index` (the undo bit) |
| M10 `compare.rs`: drop the events plane | survives `l1_chain_diff`; killed by `l1_chain` | probe unchanged (stops before events) |
| M11 `compare.rs`: drop the `post.index` plane | survives `l1_chain_diff`; killed by `l1_chain` | probe unchanged |

The comparator's own planes are guarded by `l1_chain`'s injected-difference test, not by `l1_chain_diff` (which
compares fields directly). That is expected while Diff mode stops at op 1.

## 1. Shape port (`chain::shape`) — correct for every covered op

- `submit_block` matches `rpc/mining.cpp:1025-1064` step for step: decode → coinbase → lookup
  (`duplicate`/`duplicate-invalid`) → `ProcessNewBlock` → `!new_block && accepted` → catcher `found` → BIP22. The
  catcher is "the last `BlockChecked` of the hash during `ProcessNewBlock`". All of Core's `BlockChecked` fire there
  (synchronous, `validationinterface.cpp:280-284`), and the node's listener takes the last one of the op (`node.rs:173-175`).
  This is equivalent.
- `DecodeHexBlk` (`core_read.cpp:144-159`): `IsHex("")` is false, so empty input is a decode failure, and trailing
  bytes are allowed. Both are ported and tested.
- `submit_header` matches `:1082-1103`. The `IsError` branch is unreachable for headers (AcceptBlockHeader sets
  only invalid states).
- `proposal` matches `:697-725` (no checks before the mode branch, `:671-697`). It is cited and unused until a6.
- BIP22 (`:532-548`): `Error` → `-25 ToString()` (an error state has no debug text, so `ToString` is the reason);
  an empty reason → `rejected`. The codes (`rpc/protocol.h:41-46`) are -5, -22 and -25.
- Difficulty: `ConvertBitsToDouble` (`validation.cpp:1786-1804`) is ported exactly. UniValue's `setFloat` is
  `ostringstream << setprecision(16)` (`univalue.cpp:75-82`), which is `%.16g`. The port's exponent comes from the
  rounded `{:.15e}`, so a carry into the next decade switches notation as `%g` does. The fixed form uses `15 - exp`
  decimals with trailing zeros trimmed, and the exponent form uses a sign and two digits. Test vectors include
  `1e+16` and `1.25e-05`.
- BlockChecked: the `(hash, res, cls, why)` match the shim's `StateFields` (`oracle_chain.cpp:186-192`): `why` is
  the reject reason, never the debug text. The class name comes from `REJECT_CLASSES`, which is index-equal to the
  shim's `NAMES`.
- `getchaintips` order matches the shim's `Tips()` (`oracle_chain.cpp:691-704`): height descending, then the hash's
  hex string. `getdeploymentinfo` matches `SoftForkDescPushBack` (`rpc/blockchain.cpp:1445-1537`): `bit`,
  `statistics` and `signalling` only while signalling; `threshold`/`possible` not when LOCKED_IN; `height` only
  when next is ACTIVE.

## 2. Event ordering — correct

Core's queued signals are `UpdatedBlockTip`, `BlockConnected`, `BlockDisconnected`, `ChainStateFlushed` and
`NotifyChainLock` (`validationinterface.cpp:215-227`, `:254-278`, `:309-316`). The rest are synchronous (`:229-236`,
`:280-300`, `:349-352`). Under the inline queue, `MaybeScheduleProcessQueue` posts nothing (`scheduler.cpp`, the
shim's hook). `SyncWithValidationInterfaceQueue` then drains on the calling thread (`validationinterface.cpp`, the
shim's hook). The only drain points inside a served op are `LimitValidationInterfaceQueue` (`validation.cpp:3870`,
plus `:4050` in InvalidateBlock, which is unsupported). The other one is `FinishReport` (`oracle_chain.cpp:1046`).
The node keeps the synchronous and queued lists apart and joins them sync-then-queue (`node.rs:633-634`), which is
Core's list whenever no mid-op drain happened.

The read order fence → queue sync → post (`node.rs:616-626`) matches `FinishReport`'s sync → records → post. Core
has no deferred work, and V's deferred work runs before the fence query. The queue-drain refusal is sound: total
queued ≥ pending at any iteration start. Its tightness and the 5/6-block boundary are covered in m-5. The refusal's
reliance on V alone is M-1.

## 3. Can Diff report "equal" when they differ?

Not today. The comparator compares `Option`s strictly (`compare.rs:93-105`), and the node's `utxo` is `None` against
Core's `Some`. So every live served op diverges by `post.utxo` at the latest. Record gaps are mismatches in stream
mode (`dash-statedigest/src/compare.rs:447-457`), so BLOCK ops stop at `records`. The `index`/undo adjustment lives
only in the test (`l1_chain_diff.rs:51-66`), never in the comparator, and it is sound on these chains. It adds
`HAVE_UNDO` exactly where Core's ConnectBlock writes undo before raising to VALID_SCRIPTS (`validation.cpp:2935-2950`;
genesis returns early, `:2582`). It also asserts that V wrote none. A failed fence or signal sync makes the node down,
with a `V stopped` reply, so it diverges and is never refused. The one hole is M-1. A consequence worth stating in
`diff.rs`: until a7/a10, Diff mode compares only op 1 of each input, and the post fields after `utxo` (`index`
through `deployments`) are held only by `l1_chain_diff`'s direct asserts.

## 4. D8 parity — sound, fails loudly in tests

`check` compares Core's `oracle_chainparams_json(chain_args)` with rdashd's parse of the same strings, leaf by leaf
(the same leaf set is asserted). It then compares the running parameters with the compared ones, allowing exactly
`-fastprune`'s `pruneAfterHeight` (the only node option `CreateChainParams` reads, `chainparams.cpp:875`; every other
`args` read there is a chain option). The four tests exercise a one-sided option, a running-only option and the
`-fastprune` allowance. Gaps: m-3 (fuzz path loses the message) and m-4 (uncompared fields).

## 5. NodeSut scaffolding — minimal and safe until a10

`sut` is behind `test-support` (`dashd-node/src/lib.rs:30-31`, with a `compile_error!` without debug assertions at
`:12-13`). `signals()` is widened only to `cfg(any(test, feature = "test-support"))` (`dashd-node/src/chain.rs:318-322`).
`register_listener` adds a listener without unregistering it. The listener lives as long as the load, and each load
gets a fresh hub. `sync_signals` uses `Blocking`, and the hub refuses a no-wait-marked caller (`signals.rs:334-337`),
which the node treats as a stop. n-1 is the only nit.

## 6. Design-text corrections (a)–(e) — all correct

- **(a) async vs sync signals.** O2b §4.4's "Core's asynchronous signals run synchronously at their call sites" is
  wrong. Under the inline queue they are queued and run at the drain points of §2 above (op end; mid-ABC above 10
  pending). Correct.
- **(b) NotifyMasternodeListChanged is synchronous.** It iterates directly (`validationinterface.cpp:349-352`).
  Correct. It is not emitted at M3 below DIP3, so the node's lack of it shows no difference.
- **(c) restore is a replay on the node.** The shim's `Open()` calls `ResetNodeProcessState` (`oracle_chain.cpp:761`)
  → `OracleResetValidationStatics` (`oracle_process.cpp:191`, `validation.cpp:3237-3241`). That nulls the
  `NotifyHeaderTip` latch (`validation.cpp:3799-3824`), as a dashd process start would. The node's replay leaves V's
  latch at the image tip, so the `#[ignore]`d `fork 1 1` case is a real, declared gap. Correct.
- **(d) index equality needs undo writes.** `HAVE_UNDO` is in the digest mask (`oracle_chain.cpp:108-109`). It is
  set in ConnectBlock's undo write (`validation.cpp:2935-2938`) before `RaiseValidity(BLOCK_VALID_SCRIPTS)`
  (`:2946-2949`), and NullConnector writes none. Correct.
- **(e) V's `ClTrigger::TipScheduled`.** Core's `ChainlockHandler::UpdatedBlockTip` schedules `CheckActiveState();
  EnforceBestChainLock(); Cleanup()` (`chainlock/handler.cpp:225-245`). The shim registers no handler; its EventLog
  forwards only `AcceptedBlockHeader` to `Chainlocks` (`oracle_chain.cpp:212`, `:230-234`). V defers
  `ClEnforce{TipScheduled}` per tip change (`dashd-chainstate/src/chain/locks.rs:252-260`). Correct, and invisible
  while no lock can be recorded. One addition for the seam: V's task also runs `CheckActiveState` first
  (`locks.rs:276-280`). So V observes spork-19 flips at tip changes, where the shim observes them only at
  `CL_ENFORCE`/`CL_RECORD`. When CL_RECORD lands, an off→on flip between ops resets V's lock state earlier than
  Core's (`ResetChainlock`). The LK1a/a10 seam must suppress the whole task, not only the enforcement.
