# CH3a a6 (TestBlockValidity): independent consensus review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `node/ch3a-a6` at 17041008f, the a6 delta `git diff 26c1127cc 17041008f` (dcdbd19bc, 522caa12f, 84b0f5216, the follow-merge 1d4f84eb2, 17041008f). |
| Against | Dash Core `v24.0.0-rc.3`: `validation.cpp` (`v`) :4923-4974 `TestBlockValidity`, :4469-4522 `ContextualCheckBlockHeader`, :4530-4590 `ContextualCheckBlock`, :4365-4430 `CheckBlock`, :2490-2890 `ConnectBlock` (CheckBlock :2505-2514, CHAINLOCK :2516-2519, EVODB :2525-2531, genesis :2536-2540, assumevalid :2542-2571, BIP30 :2590-2612, H4 :2661-2678, fCacheResults :2750-2752, IS :2799-2818, payments :2841-2862, early return :2883-2884), :6785-6795 `IsBIP30Repeat`/`Unspendable`; `rpc/mining.cpp` :532-548 `BIP22ValidationResult`, :697-724 proposal mode. |
| Context | CH3 §2.14, §2.17, §3.2; CH3a notes D116–D126 on the branch; the implementer's self-review claims in the brief. |
| Method | Read line by line against Core (read with `git show v24.0.0-rc.3:…`; the dash checkout was not moved). Detached worktree in /work/scratch/ch3a-a6-review with a target reflink-seeded from the implementer's. The brief's command fails as written (`--features test-support` exists in neither crate: dash-chain has `test-util`), so I ran `cargo test --locked -p dashd-chainstate -p dash-chain`: dash-chain 414/414 (1 ignored), dashd-chainstate lib 618/618, its integration tests and both crates' doc-tests all green, at load 50–130. `contrib/node/ch3a-connect-census.py` against an rc.3 `git archive`: 0 failures. 15 env-gated hand mutants in a second worktree, each run against `chain::tests::connector` (56 tests), the survivors against the whole lib. `git show --remerge-diff 1d4f84eb2` for the merge. No clippy, no PF0 run. |
| Verdict | **ACCEPT WITH NITS.** No Blocker or Major. Two Minors (one test gap on the 17041008f logic, one design note for RP/MN0) and four Nits. Every implementer claim in the brief holds. |

## 1. Branch-for-branch parity

Everything below matches rc.3.

- **`TestBlockValidity` order** (`connect/connector.rs:244-347` against `v:4923-4974`):
  1. `V::test_block_validity` (`chain/proposal.rs:27-71`) answers `NotBestPrev` for a block whose parent is not the tip, or with no tip, and never reaches the connector. That is GBT's `inconclusive-not-best-prevblk` (`rpc/mining.cpp:717-720`), and it makes `assert(pindexPrev == Tip())` (`v:4934`) hold. The connector asserts it again on the locked coins as a `Fatal`.
  2. The ChainLock check `has_conflicting_chainlock(prev.h + 1, hash)` on V's ChainLock state (`ConnectCx::chainlocks`, not the helper) gives `INVALID_PREV` `bad-chainlock` with `LogPrintf`'s "ERROR: TestBlockValidity: conflicting with chainlock" at INFO (`connector.rs:290-295` against `v:4937-4940`). It comes before `begin_tx` (`v:4950`). `begin_attempt` and the coins lock run before it. `viewNew` reads nothing, so that reorder is not observable (D122).
  3. `begin_tx`, then the three pre-checks in Core's order, each with Core's `LogError` text (`connector.rs:117-138` against `v:4952-4967`):
     - `check_header_at` is `ContextualCheckBlockHeader` at the tip.
     - `CheckBlock` uses the caller's PoW and merkle flags.
     - `ContextualCheckBlock` uses V's `BlockChecks`.
  4. `connect_at` runs with `Finish::JustCheck`. On every path `finish_attempt` runs, then `end_attempt` (only if `ConnectBlock` ran), then `rollback_tx` (whenever `begin_tx` ran). Nothing commits.
- **Classes.** TestBlockValidity has one reject of its own, `bad-chainlock`/`INVALID_PREV`. A test pins it against `ConnectBlock`'s `CHAINLOCK` for the same block on the tip path (`a_conflicting_chainlock_is_invalid_prev_in_a_proposal_and_chainlock_at_connect`; mutant M1 is killed).
  - The pre-checks' classes come from shared CS3 functions that existing rows pin:
    - CCBH: `bad-diffbits`/`time-too-old`/`bad-version` are INVALID_HEADER, `bad-fork-prior-to-checkpoint` is CHECKPOINT, `time-too-new` is TIME_FUTURE.
    - CheckBlock: `high-hash` is INVALID_HEADER; `bad-txnmrklroot`/`bad-txns-duplicate` are MUTATED; the rest are CONSENSUS.
    - CCB: all CONSENSUS.
  - The directed test pins one row per check, class included (`assert_verdict` compares `result()`).
  - `ConnectBlock`'s classes are the a1–a8 code, unchanged.
  - I found no class that is wrong, and none that nothing pins.
- **`ConnectBlock(fJustCheck)` on the dummy** (`connect/serial.rs:441-853`):
  - Inner `CheckBlock` with PoW and merkle off (`:467-476`, `v:2505`). `checked = pow && merkle` (D118) is equivalent: `fChecked` is set only with both on (`v:4424-4425`), and the inner check is then a subset of the outer one (M16 survives as an equivalent mutant).
  - The `CORRUPT_BLOCK` AbortNode cannot fire. The port's `check_block` gives MUTATED only through `check_merkle_root` under `check_merkle` (`dash-consensus/src/block.rs:127-129`), as Core's (`v:4382-4384`).
  - CHAINLOCK at `:479-482`: `at.prev.is_some()` stands in for `pprev && phashBlock`, and the dummy has both.
  - G1, then EVODB's `AbortNode` through `active_after(at.prev, DIP0003)`, which is `DeploymentActiveAt(*pindex)` (`deploymentstatus.h`).
  - Assumevalid: `id: None` gives `script_checks = true` (`:523-526`). `v:2551-2552` compare `CBlockIndex*`, and the dummy is in no chain.
  - BIP30: `IsBIP30Repeat` is `return false` on Dash (`v:6785-6789`), so `!at.id.is_some_and(is_bip30_repeat)` is the same as before, and `bip30_probed` walks `prev`, as `pindex->pprev->GetAncestor(BIP34Height)` does.
  - Flags: `flags_after(prev)` matches `lock_time_flags`/`block_script_flags` at the block on every network at each buried boundary ±1 (`the_flags_after_the_parent_are_the_flags_at_the_block`). The DERSIG/CLTV/CSV/BIP147 order is kept.
  - H4 gets `fJustCheck` (`:584`, `v:2663`).
  - `fCacheResults = fJustCheck` is passed as both store flags (`:686-694`, `v:2750-2752`).
  - The early return comes right after the payee check (`:846-853`, `v:2883-2884`): no undo, no `VALID_SCRIPTS`, no H6/H1-commit/H7.
- **Side effects of a proposal.**
  - The tests pin that nothing changes:
    - index: dense ids, lookup;
    - block and rev files: `stored()`, rev bytes;
    - the tip's status;
    - coins: best block, the seed unspent, the new output absent;
    - evo: no `SetBestBlock`, rollback;
    - no signal.
  - What does change is Core's own side effects, which the port keeps on purpose:
    - the script-exec cache store (D126);
    - the versionbits cache at the tip (Core's `m_versionbitscache` is keyed by `pindexPrev`, a real index);
    - the attempt's effect log. `RemoveConflictingISLockByTx` (`v:2808-2810`) runs in Core's just-check too, and `finish_attempt` applies the log on every outcome (A1 §2.10.1).
  - `scope_end` republishes the status and the view with `view_seq + 1` (N2).
  - Mutants skipping the rollback (M8), lying about fJustCheck to H4 (M9), and storing exec entries on connect (M5) are all killed.
- **D121/D75 and 17041008f** (`connector.rs:325-347`, `serial.rs:347-352`):
  - A G1/G2/D63 refusal in a proposal skips the `AbortSink`, so the latch stays untripped and the reply is `BlockState::Error` (killed: M2).
  - Core's own EVODB `AbortNode` reports once and trips the latch (`an_evodb_inconsistency_aborts_a_proposal_too`).
  - After an unreported refusal, a failed `finish_attempt` is the `Fatal`, which V reports once at its stop (killed: M14, the fix reverted).
  - The tip path is unchanged: `refuse` reports, so the latch is tripped and D75 keeps the `SystemError`.
  - A later connect after a proposal's EVODB abort finds the latch tripped. Its store writes fail-stop as "reported", so nothing is reported twice. That leg, a reported AbortNode in a proposal followed by a failed apply, is untested (m-1).
- **`check_header_at`** (`dash-chain/src/chain.rs:1449-1477`):
  - It takes `&self`, so it inserts nothing. The test pins the epoch, `next_id`, best header and lookups.
  - It is `check_on` over the full arena at any resident `prev`. That is exactly the node accept path's CCBH, so it agrees with `ContextualCheckBlockHeader(…, pindexPrev)` at a non-tip parent too (all 39 CS3 rows, `rule_rows.rs:207-209`).
  - SPV gets `WrongMode`, and an unknown `prev` gets `UnknownBlock`.
  - The `check_on(…, now)` change keeps both existing callers on the chain's clock, read once per call as before. In SPV mode it is now read before the `RuleContext` refresh, which nothing observes.
- **The follow-merge 1d4f84eb2** (`--remerge-diff`). `actor.rs` keeps `verify_loaded` and `test_block_validity`, both complete. `command.rs` keeps both variants and both in `Class::M`, and the module comment merges the two sides' wording correctly: VerifyDb is now inside `VerifyLoaded`. `support.rs` keeps both the `store::blocks` and the `connect::connector` targets, and the doc lists both. Nothing from either side was lost.
- **PF0.**
  - `connect` builds `Finish::Index` and calls `connect_at`, one more call frame.
  - `just_check` is a runtime bool on a few cold branches and the two cache flags.
  - Buried lookups through `active_after(prev)` cost the same as `active_at(id)`. The V24/EvoShares versionbits lookups save a `prev(id)` lookup each.
  - `script_checks` and `enforce_bip30` gain an `Option` match.
  - I found no per-block work that Core does not do. I expect a flat-to-slightly-negative PF0, but I did not measure it.

## 2. Tests and census

The census passes, with `TestBlockValidity`'s range under `TESTED_LOGS`: every log line's fixed text must also be in the tests. The directed tests cover:
- the valid proposal changing nothing, then connecting;
- each pre-check's reject with its class and line, and the flag combinations (bad root only with merkle on, a missed target only with PoW on, both off valid);
- `ConnectBlock`'s own rejects inside a proposal;
- INVALID_PREV against CHAINLOCK;
- G1 and G2 refusals without a stop;
- refusal plus failed apply (both apply sites);
- the EVODB abort;
- the store-both cache;
- assumevalid against the tip path;
- `NotBestPrev` for a sibling, genesis and an orphan;
- the helper scope (D125);
- a storage fault and a hook panic inside the bracket.

The line-count assertions are safe under the parallel harness (cf. a5's M-1). Every exact-count line is a `TestBlockValidity: …` text that only `each_pre_check_rejects_with_cores_class_and_line` logs, and the shared ChainLock line is asserted with `>=`.

### Mutants

| # | Mutant | Result |
|---|---|---|
| M1 | proposal ChainLock reject as `CHAINLOCK` instead of `INVALID_PREV` (`connector.rs:293`) | killed (2 tests) |
| M2 | `refuse` reports in a proposal too (latch trip on a G1/G2 refusal, `serial.rs:348`) | killed (2) |
| M3 | assumevalid applied to the dummy (`None => false`, `serial.rs:525`) | killed (2) |
| M4 | `cache_sig_store` true on a tip connect (`serial.rs:692`) | **survives** the whole lib (618/618). D126 admits it is pinned by inspection only (m-2's note) |
| M5 | `cache_full_script_store` true on a tip connect | killed |
| M6 | 17041008f's arm without its latch guard (any `SystemError` + failed apply is `Err(f)`) | **survives** the whole lib (618/618) (m-1) |
| M7 | `CheckBlock` before CCBH (without its log line) | killed, but only by the missing log line: no test has a two-fault block, so the order itself is unpinned (N1) |
| M8 | skip `rollback_tx` | killed (6) |
| M9 | H4 told `fJustCheck = false` in a proposal | killed |
| M11 | inner `CheckBlock` with PoW and merkle on in just-check | killed |
| M12 | skip the CCBH pre-check | killed |
| M13 | `end_attempt` also when `ConnectBlock` never ran | killed (3) |
| M14 | 17041008f reverted (the old D75 drop after a refusal) | killed |
| M15 | proposal ChainLock check after `begin_tx` | killed (2) |
| M16 | `checked = false` always (D118) | survives: equivalent (the inner check is a subset of the outer) |

## 3. Findings

### Blocker / Major

None.

### Minor

- **m-1. The D75 half of 17041008f is unpinned, and its discriminator is read after `finish_attempt`.**
  - In `connector.rs:333-339`, the `(SystemError, Err(f))` arm returns `Err(f)` only if `self.latch.tripped().is_none()`. M6 removes that guard, which makes a reported `AbortNode` in a proposal plus a failed apply stop V and report `f` a second time. M6 survives every connector test: `an_evodb_inconsistency_aborts_a_proposal_too` never fails the apply, and `a_failed_application_after_a_refused_proposal_stops_the_node` covers only the refusal leg.
  - The guard reads the node-wide latch *after* `finish_attempt` and from any thread. A trip during `finish_attempt`, or by another component (the flush side) between `connect_at` and the check, is taken as "this SystemError was reported". The refusal is then kept and `f` is only logged.
    - That is safe today, because whoever tripped the latch reported, so the stop is still reported once.
    - But it is a different outcome from the one D121 describes ("the failed apply is the outcome"), and it depends on timing.
  - Fix:
    - Add the missing leg to `an_evodb_inconsistency_aborts_a_proposal_too`: with `fail_apply` set, assert the reply is `BlockState::Error(EVODB…)`, `sink.messages() == [EVODB…]` (one report), and the `finish_attempt` line logged.
    - Preferably, make the discriminator explicit rather than inferred from the shared latch. For example, `refuse` in just-check could set a flag on `SerialCore`, or return a distinct marker that the connector reads. At least snapshot the latch before `finish_attempt`.
- **m-2. GBT's duplicate lookup outside V's scope can give answers Core never gives (D123, design note for RP/MN0).**
  - Core runs GBT's duplicate lookup, `Tip()` read and `TestBlockValidity` under one `LOCK(cs_main)` (`rpc/mining.cpp:707-723`). D123 leaves the lookup to RP "through the reader" before the class M command.
  - Suppose the proposed block is accepted and connected between RP's lookup and V's command. rdashd then answers `inconclusive-not-best-prevblk` (V's tip is the block itself). Core, at any single instant, would answer `duplicate` (indexed, `VALID_SCRIPTS`) or the proposal's verdict.
  - `CreateNewBlock` has the same gap (`node/miner.cpp:193` takes `cs_main` before the template build and holds it through `TestBlockValidity` at `:339`): MN0 can get `NotBestPrev`, a spurious internal error, where Core cannot fail.
  - Nothing calls the command yet, so it costs nothing to fix now. Let the command answer the duplicate cases too, with a typed `Duplicate { valid_scripts, failed }` or by folding them into the reply enum, inside V's scope. Or record in D123 that RP/MN0 own this race and how they close it (retry on `NotBestPrev` when the tip moved).
  - Not a6's consensus code, so not a Major.
  - Separately, M4 survives (D126 says so). A P2PKH spend in `a_proposal_stores_script_results_and_a_connect_does_not` asserting the signature cache entry would pin `cache_sig_store` for a few lines. Optional; listed here rather than as its own Minor.

### Nits

- **N1. The pre-check order is unpinned.** Each `each_pre_check_rejects_with_cores_class_and_line` row has one fault, so swapping CCBH and `CheckBlock` (with its log line) survives. Add one two-fault row, for example `time-too-old` with a bad merkle root and merkle on, expecting CCBH's reason and only CCBH's line.
- **N2. "Nothing is different after a proposal" needs one qualifier in D122 and D126.** `scope_end` (`actor.rs:1284`) republishes the status snapshot and a `ChainView` with `view_seq + 1` (`actor.rs:1516-1519`). That is CH1-node §4.12's class M rule, and nothing consumes `view_seq` today. Say it, so a future longpoll or `waitfornewblock` keyed on view changes does not wake on proposals by surprise.
- **N3. The brief's test command is wrong.** `--features test-support` matches no package (`error: none of the selected packages contains this feature`; dash-chain has `test-util`). Fix the review template for a7.
- **N4. The `ContextualCheckBlock` log line has no block identity,** and neither do Core's other pre-check lines (Core's own text, kept verbatim). This is fine, but when a7 adds its L3 oracle, it should match these lines by order within a proposal, not by hash.

## 4. Possible security note

None from this slice.
