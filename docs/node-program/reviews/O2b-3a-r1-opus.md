# O2b-3a diff_chain Core half (CoreOnly): Opus-h review, round 1

Reviewed: rdc `node/o2b-3a` at **59a1b323e** (one commit on O2b-2 1484f7b74; `git diff 1484f7b74..59a1b323e`, 64 files, +4833). Design: O2b rev3.2. Plan: O2b-3 plan §2 and §9 (D1–D4). Core citations are to the pinned oracle source, Core 5c9e9117a2 (ABI 22; v24.0.0-rc.3 0364ed626e plus the shim). I used a detached worktree with a private 0700 target under `/work/scratch/o2b-3a-r1-opus`, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12` and `DASH_ORACLE_CACHE=/work/cache/oracle`. The worktree and target are deleted.

## Verdict: ACCEPT WITH NITS

The scoring is safe. The session's taint latch cuts every input at the first tie-decided step. A discarded input contributes nothing to the census. The replay check compares only untainted prefixes. The expectation model follows the pin's acceptance order, and none of my runs produced an unexpected reply (over 4,000 inputs, the stress runs included), a replay mismatch, a panic or a shim fault. The three scope rules are implemented as D2–D4 word them, and the tracked wording is generic.

Every finding below concerns coverage or code quality, not correctness. Three are worth doing before or soon after the merge:
- **F1:** rule 2's "consensus failure" predicate counts rejects that leave nothing in the index. That costs about 4% of inputs an in-scope `reconsiderblock`, more than the 3.6% that reach one.
- **F2:** the tie model misses that a block stored under a parent without data keeps sequence id 0. All three of my site-5 discards have this cause, so the "predicted safe, Core tainted" rate is at P4's 0.1% fix threshold.
- **F3:** `chain::compare` is never called or tested, although plan §2.1 has 3a exercise it.

## What I ran

| Run | Result |
|---|---|
| `cargo test -p node-l1 --features oracle` (26 test binaries; `l1_chain` S0+S1, `chain::*` unit tests, every other node-l1 oracle test) | all pass, EXIT 0 |
| `chain_sample S0 1000 7` and `chain_sample S1 1000 7` under `systemd-run --user` (the owner's smoke, my seed; release build of 59a1b323e) | EXIT 0 on both. 0 unexpected, 62 replays with 0 mismatches, 3 discards each (site 5, 0.3%), 8.5 / 8.8 inputs per CPU-s. No Core abort |
| The same S0 sample with census-only instrumentation (scratch patch, never committed; same op stream and totals) | the breakdowns used in F1, F4 and F6 |
| `chain_sample` instrumented, S0 1000 seed 11 | F5's count. 0 unexpected, 3 discards |
| Generator robustness: a scratch `chain_stress` example. Seed programs mutated libFuzzer-style (bit flips, boundary bytes, inserts, deletes, splices, 255-byte raw intents), plus random inputs of 16–2000 bytes and inputs over `MAX_INPUT`. Each input runs twice in one `Runner`, with `catch_unwind` around both. 2 processes × 420 s, about 14 CPU-min | 2039 inputs: **0 panics**, 0 unexpected, 0 replay mismatches, 0 shim faults, 3 discards |
| Tie traces (`chain_sample --trace` plus a debug print of the model's seq-0 levels) of the three discarded smoke inputs | F2 |

I also read the arithmetic in the panic-prone paths by hand. `deficit as u8` is bounded by `blocks_left ≤ 64`. Every coin is filtered to at least `MIN_COIN`, which keeps outputs positive. `mutate::COINS` guards the operators' coin indexing. `Run::untainted` always has the restore report. B1's fix holds.

## Faithfulness (scope 1)

The expectation model was checked against the pin, op by op:
- **`submitblock`:** the RPC's own duplicate and coinbase pre-checks come first (`rpc/mining.cpp:1030-1048`). `ProcessNewBlock` then runs `CheckBlock` first and never marks on its failure (`validation.cpp:5022-5034`). `AcceptBlockHeader` runs in this order (`:4738-4841`): known header, `CheckBlockHeader`, prev lookup, `bad-prevblk`, `bad-prevblk-chainlock`, `ContextualCheckBlockHeader`, the failed-ancestor walk, then `HasConflictingChainLock`, which indexes the new entry conflicting. `AcceptBlock`'s `CheckBlock`/`ContextualCheckBlock` failure marks the entry failed (`:4962-4968`). `ConnectBlock`'s ChainLock check (`:2562-2565`) runs only for a block that connects. `generate.rs:144-215` follows that order, including "a known header skipped the header-time lock check, so ConnectBlock reports it" (`generate.rs:207-208`).
- **ChainLock judgement:** `cl_conflict` (`generate.rs:109-115`) is `HasConflictingChainLock` against `bestChainLockBlockIndex` (`chainlock/chainlock.cpp:79-97`). `World::best_cl` follows the known-block lock: the record path (`chainlock.cpp:106-131`), late-header adoption (`:152-172`), and the shim's `best_cl` is `bestChainLock` (`oracle_chain.cpp:740-741`), filtered to indexed blocks in `world.rs:339-345`.
- **Enforcement and reconsider:** `enforce_marks` mirrors `EnforceBlock`'s sibling walk with `MarkConflictingBlock`'s disconnects (`validation.cpp:4158-4196`, `:4198-4240`). The reconsider model mirrors `ResetBlockFailureFlags` with its conflict-flag retention (`:4270-4345`). `resets_failures` is `!pindex->IsValid()` on a header-only lock (`:4192-4195`).
- **TestBlockValidity:** the ChainLock check comes first, then the header checks without PoW, then `CheckBlock`, `ContextualCheckBlock` and `ConnectBlock` (`generate.rs:238-258`). The proposal pre-checks match `rpc/mining.cpp:707-720`.

**Would a wrong expectation be caught?** It would only for ops whose expectation isn't `Any`. In the instrumented S0 1000:

| Op | Checked | `Any` |
|---|---|---|
| block | 10,238 | 2,124 (17%) |
| header | 5,630 | 1,016 (15%) |
| proposal | 190 | 1,128 (82%) |
| cl_record | 0 | 869 (100%) |
| restart | 0 | 1,485 (100%) |
| activate | 0 | 1,433 (100%) |

Because the world model folds in Core's own step (P2), a wrong `Any` can't corrupt later expectations; it just goes unchecked. 3b's two-sided comparison is the real check, so this is acceptable for CoreOnly, but see F6 and N3.

## Scoring safety (scope 2)

- **Taint:** `run.rs:277` reads the session latch (`core.rs:217`, `tainted_since`) right after the step, before `rows.see`, `w.observe` and `due_enforce`. A tainted restore is a shim fault (`run.rs:245-249`). The image's own build refuses a tainted save (`core.rs:195-197`).
- **Census:** `run.census.op` (`run.rs:274`) counts the tainted step itself. However, `Totals::input` merges an input's census only when `discard` is `None` (`census.rs:110-122`), so no tie-decided op reaches a scored row. `cl_enforce` is counted across discards by design (I38's per-enforce ratio).
- **Replay:** `replays` (`run.rs:381-390`) compares only the common untainted prefix when either run is tainted. That is right: whether a tie site fires can depend on which pairs the address-ordered set compares, so the taint position may legitimately differ between runs.
- **Rule 3's early end** (`Some(None)`, `run.rs:294`) ends the input `Done`, scored up to the last enforced state. That is correct: nothing past the unenforceable lock runs.
- **Tie model and restart deficit:** these are advisory and only cost discards. Core decides every discard. G-T1's own-invalidate exception (`ties.rs:40-65`) matches the load's reset of a single best-invalid entry. The G-T1 KILL variant (`ties.rs:69-77`) reads the durable entries, as a KILL load does.

I found no path that scores tie-decided or out-of-scope state.

## Scope rules 1–3 (scope 3)

The rules are implemented as worded. The commit message, D2–D4, the code comments and the census keys are generic and carry no pointer to anything private.

Avoided counters, S0 1000 seed 7, with what each costs:

| Counter | Count | Core behaviour it costs |
|---|---|---|
| `spork_not_admitted` | 1,486 | None until D34. It is just tag 9's 1/17 share of intents, about 6% of intents as no-ops. It doesn't starve anything |
| `reconsider_after_failure` | 1,263 | 1,198 (95%): the target doesn't undo the program's own invalidate (D3 as designed). 22: a real failure mark. **43: no index mark at all (F1)** |
| `cl_on_invalid` | 445 records + 53 enforcements | Records: 332 mutated blocks (correct: the quorum never signs them), 36 consensus-failed or conflict-marked, **24 the program's own invalidate (F4)**. Enforcements: 52 lock no longer presumed valid (mostly the program's own invalidate below it, F4), 1 `resets_failures` |
| `cl_unenforced` | 182 | The documented scheduling window (BL-212). D4 handles both the known-block and the late-header resolution (`world.rs:339-345`, `run.rs:291-295`). Rule 3 works at op granularity, and that is enough: no single op can make the locked block known and then connect an invalid descendant, because a descendant's data cannot be indexed before the locked block's header |
| `invalidate`, `precious`, `reconsider`, `cl_enforce`, `restart_tie` | 273 / 279 / 7 / 25 / 52 | G-T1/G-T2/G-T3 (the tie model) |

Beyond BL-212's window, two non-buggy behaviours are lost: the F1 reconsiders, and `EnforceBlock`'s reset of a locked block the operator had invalidated (F4).

**Are the rates reasonable?**
- `spork_not_admitted`: yes.
- `reconsider_after_failure`: it is mostly rule 2's own scope, but the coverage left is thin. `reconsiderblock` succeeds in 36 of 1,000 inputs here, and in 328 of 10,000 scored inputs in the owner's sample. F1 and F7 roughly double that within the rules.

## Findings

**F1. Minor: rule 2's predicate counts rejects that leave no index mark, and contradicts its own orphan exclusion.**
- `World::consensus_failure` is set by three paths:
  - any block reply except `prev-blk-not-found` and `bad-chainlock`/`bad-prevblk*` (`world.rs:391-396`, `own_failure` at `:564-566`);
  - any header reply except `Must submit previous header` (`:431-436`);
  - **every** non-chainlock `BlockChecked` invalid (`:460-462`). That third path includes `missing_prev` (the same orphan `observe_block_reply` excludes) and `invalid_prev`.
- In Core, these rejects create or mark no index entry:
  - a `CheckBlock` failure (`validation.cpp:5022-5034`);
  - header-stage rejects before `AddToBlockIndex` (`:4755-4785`);
  - orphans (`:4765-4767`).
- A `reconsiderblock` can't touch any of them. Instrumented S0 1000: 43 reconsiders that undo the program's own invalidate were dropped. The predicate was set by:

  | Source of the flag | Count |
  |---|---|
  | `missing_prev` BlockChecked | 21 |
  | `bad-txns-vout-*` / `bad-txnmrklroot` / `bad-cb-multiple` / no-coinbase (CheckBlock stage) | 9 |
  | header stage (`bad-version`, `time-too-old`, `bad-diffbits`, `high-hash`) | 7 |
  | `invalid_prev` | 4 |
  | `duplicate-invalid` | 2 |

  For comparison, `reconsider:ok` was 36.
- **Fix:** stop counting the inert classes: header-stage strings, `Stage::Pow`/`Stage::Check` operators, and `missing_prev` in `observe_checked`. Keep counting `duplicate-invalid` and `invalid_prev`. They say the node holds a failed entry that the program delivered data around, and the conservative reading of D3 should keep them. `ties::restart_deficit`'s own-invalidate exception reads the same flag (`ties.rs:47`), so it gains the same rows.

**F2. Minor: the tie model gives a unique sequence id to a block stored under a parent without data. Core leaves it at 0. This causes the site-5 discards.**
- `Block::store_data` sets `Seq::Arrived` whenever data is stored (`world.rs:106-111`).
- Core assigns `nSequenceId` only in `ReceivedBlockTransactions`' branch for a block whose parent has its chain's transactions (`validation.cpp:4412-4424`). Any other stored block keeps 0 until its parent's data arrives, or until a load. Core documents the same invariant at `:5848`.
- `ties::seq_zero_heights` therefore misses the pair, and G-T2 lets the op through.
- **Schedule (smoke input 606, S0, the shape of all three):**
  - a fork sent headers first, whose lowest block's data is never delivered;
  - the fork's next blocks' data delivered: stored, but with seq 0;
  - an `invalidate` whose `pindex->pprev` is one of those seq-0 blocks (here, a block made precious above it).
  `InvalidateBlock` compares every non-active entry with `pindex->pprev` before checking validity (`:4034-4040`). It meets a loaded block (seq 0) at the same height, here the earlier-invalidated main-chain block, and fires site 5. The model saw `Arrived` on the fork block and `risky_from` returned false.
- **Rate:** 3 in 1,000 here (0.3%), and probably most of the owner's 10 site-5 discards in 10k, which is exactly P4's 0.1% "fix before measuring" line.
- **Fix:** mark `Arrived` only when the block's parent has data all the way down to a loaded block. When a delivery completes that chain, mark the stored descendants it reaches `Arrived` too, as Core's queue walk does (`:4426-4432`).

**F3. Minor: `chain::compare` is dead and untested.**
- Nothing in node-l1 or node-fuzz calls `chain::compare::compare` (`compare.rs:67`). No test covers it, and `Skipped` is never read.
- Plan §2.1 says that in 3a "it's exercised Core-vs-Core and by injected differences". The O9a deltas are tested in `dash-statedigest/tests/compare.rs`; the planes over `Step` are not.
- Two things to settle before 3b relies on it:
  - Events are compared strictly in full (`compare.rs:85`), but the design says "Core-only events counted".
  - A records-plane `Verdict::Incomplete` passes silently (`:81`). That is fine for ops with no records, but it is undocumented.
- **Fix:** either add a test (a seed run's steps against themselves: equal; then one injected difference per plane: the field named), or drop the module until 3b.

**F4. Minor, the manager's call: rule 1 is narrower than worded, so `EnforceBlock`'s reset of a locked block is never reached.**
- D2's wording is "a block the quorum would have accepted". The implementation's `presumed_valid` (`world.rs:237-242`, used at `generate.rs:538` and `:287`) also refuses a block the program itself invalidated, but the quorum would have accepted that block.
- So a ChainLock that overrides the operator's `invalidateblock` is never generated: `EnforceBlock`'s `ResetBlockFailureFlags` on the locked block (`validation.cpp:4192-4195`). That costs 22–24 records and about 50 enforcements per 1,000 inputs.
- That reset is the one `reconsiderblock` does (default `ignore_chainlocks = false`, `validation.h:731`). Rule 2 already admits it for the program's own invalidate.
- **Suggestion:** admit a lock on, or above, an own-invalidated block exactly when rule 2 would admit a `reconsider` of it: same predicate, after F1. Add a census row such as `cl_overrides_invalidate`. If the manager prefers the strict reading, reword D2 to "presumed valid by the model, the program's own invalidations included", so the doc matches the code.

**F5. Minor, a question: rule 1 is broader than worded in the other direction.**
- A lock above the current best one, on a branch that does not contain it, is recorded and enforced.
- Shape: a fork indexed before a lock that sat on the active chain, so `EnforceBlock`'s walk marked nothing. Then `cl tip#k` on the fork above the lock's height.
- `presumed_valid` doesn't look at the existing lock, and Core accepts the lock on height alone (`chainlock.cpp:110-113`). Core then marks the earlier lock's chain conflicting.
- A quorum that signed the first lock never accepts a block that conflicts with it, so by D2's own wording this is outside the domain. 3b would hold the node to Core's answer in a case no network produces.
- **Rate:** 2 in 1,000 (seed 11).
- **Fix:** in `cl_record`, require the target's chain to contain `w.best_cl` when there is one (counted `avoided: cl_conflicts_lock`), or record a D-row that keeps it in scope on purpose.

**F6. Minor: plan §2.4's operators are incomplete, with no D-row.**
- `OPERATORS` (`mutate.rs:59`) has no operator for:
  - sigops at the limit and limit + 1;
  - block size at the limit and limit + 1;
  - BIP30;
  - `bad-cbtx-payload`.
- All four are in plan §2.4 and design §3.4. Design §6.5's certification census needs every non-evo reject string reachable from `submitblock`/proposal.
- If `bad-cbtx-payload` is unreachable below the raised DIP3 (a type-5 transaction probably fails `bad-tx-type-dip3-inactive` first), say so in a D-row. Either way, list the rest as 3b or O2b-4 work.

**F7. Minor: reconsider and proposal coverage is thin within the rules.**
- **Proposals:** only about 1 in 7 reach TestBlockValidity with a checked expectation (190 of 1,369). Only 6 in 1,000 inputs were valid on-tip proposals.
  - The ref resolves to the tip only for `main@-0`, or when the built list happens to end there (`generate.rs:483-510`, `intent.rs:15-54`).
  - The mutate byte is nonzero 127 times in 128 (`intent.rs:187-195`).
- **Reconsiders:** 95% of reconsider intents are dropped because their ref misses the program's own invalidated subtree.
- **Fix:** the plan's R10 lever, a smarter retarget, without changing the rules:
  - map a reconsider ref into the own-invalidated subtree when there is one;
  - bias a proposal's `on` to the tip;
  - make a plain block the common case of the mutate byte.
- The decoder is part of the corpus encoding, so do it now, before O2b-4 builds a corpus.

## Nits

- **N1.** `fuzz_one` turns an unexpected reply into `OutOfDomain` (`run.rs:356`). It is counted, but neither logged nor saved. Plan §2.5 says "counted and logged". Under `run.sh` the only trace is a stats counter, with no reproducer. A one-line `report()` with the reason would do, or a sink record under a non-failing field.
- **N2.** `examples/chain_sample.rs:96-102` re-implements `fuzz_one`'s replay schedule with a literal 16. Use `REPLAY_EVERY` (make it `pub`), or share the loop.
- **N3.** The expectations for `CL_RECORD` (`generate.rs:563`) and `RESTART` (`:607`) are `Any`, but both are cheap to predict:
  - `accepted` iff the height is above Core's best recorded lock height, which needs tracking the unknown-block lock height too;
  - a refused load iff the tip's time is more than 2 h past mocktime.
  These replies currently go unchecked in CoreOnly.
- **N4.** `UNENFORCED` is `("cl_unenforced", "cl_unenforced")` (`generate.rs:267`). `enforce`'s two-name parameter exists only for the `ClEnforce` intent's split. A single name plus a `bool` (or two call sites) reads more simply.
- **N5.** Unit-test value:
  - The `ties` tests are good: each pins one rule on a synthetic tree. Add F2's case (data stored under a header-only parent is seq 0).
  - `l1_chain`'s operator loop checks Core's strings only through `End::Done`, that is, through the expectation. That is enough, but the comment says the strings "are checked against Core". Say "the expectation check is the assertion".

## D36 (coverage stability), cause analysis only (DEC-250 not re-reviewed)

The plan names three causes for the systematic "twice" excess: the session starting inside the first input, restores from whatever state the previous input left, and the counter-based replay at 16 or more inputs. All three are plausible. In particular, a restore's teardown runs different flush and close paths depending on the state it tears down. It is also consistent that the ignorelist's effect stays within per-process noise, since none of these is thread timing.

One more contributor fits the 7-seed case, which has no replay: the Rust side is sancov-instrumented, and its state grows across inputs in one process.
- `Totals::census`'s `BTreeMap`s gain keys and split nodes as new reject strings and rows appear.
- `n == 1` / `STATS_EVERY` writes the stats file.
- The `RUNNER` thread-local initialises inside input 1.

These give new edges, and especially new hit-count features, in a second pass. That fits the +330 ft, much larger than the +15–20 cov. A cheap check: rerun the 7-seed once/twice comparison with `Totals::input` stubbed, or diff `-print_pcs=1` between the two passes to see whether the new PCs are Rust or Core.

## Tracked/private hygiene

Checked: the commit message, D2–D4, the comments in `generate.rs`, `ties.rs` and `world.rs`, the census keys, `targets.toml` and `PROVENANCE.md` are generic. They describe domain rules ("the quorum signs only a block it accepted", "reconsider after a consensus failure is outside 3a's scope", "Core schedules enforcement; the window between them is outside 3a's scope"). `diff_chain` routes private. The 15 public seed programs run strictly under the rules.
