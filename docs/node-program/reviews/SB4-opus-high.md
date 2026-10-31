# SB4 `dash-chain` core: Opus-high review

**Scope.** Branch `node/sb4-dash-chain` at **69745378a73ba29f5dad48e73bb5b2b41664eb0a**. It is 64 commits over its merge-base 32f828c80 with `node/integration`.

The branch was reviewed against:
- `designs/SB4-dash-chain.md` rev7.1, especially §10.2 and §15;
- PLAN SB4/CH1;
- DEC-147/167/172/174 and the deviation registers in `/work/scratch/sb4-dash-chain/checkpoint-*.md`: D-C3-\*, D-C4-\*, D-C6-\*, D-C9b-\*, D-C10-\* and D-R1-\*;
- Core v24.0.0-rc.3.

The prior rounds, `SB4-r1-sol` (R1-M1, R1-m1) and `SB4-r2-sol` (APPROVE), were not reopened.

**Method.** The work was done in a detached scratch worktree, with three Opus sub-reviews whose claims I verified before including them:
- (A) acceptance, `load` and the codec against Core;
- (B) ChainLock enforcement, reset and SPV `fork_target`;
- (C) concurrency, eviction, `ActiveChain` and `Dirty`.

I also ran 16 of my own mutants and two probes. The sub-agents ran 32 more mutants between them.

## Findings

### Major

**MJ-1: SPV `locator` stops at the window. It has neither Core's doubling entries below `tip − w` nor the anchor terminus that the design requires (§10.1 n6, D41, SB0 E3 / DEC-140). No D-row registers this.**
- **Location:** `dash-chain/src/reader.rs:221-232` (`locator_entries`), with `ChainReader::ancestor` (`reader.rs`, which walks resident links only and returns `None` below the floor).
- **Spec:**
  - §10.1: "`locator(Some(id))` is `LocatorEntries` + `GetLocator` … ten single steps, then doubling, then genesis; **in SPV, the anchor**, which is the lowest accepted entry and lies on every chain the client accepts (n6)."
  - The §15.2 E3 row requires "`locator(Some(id))` with Core's `LocatorEntries` density and the SPV anchor terminus" and names `wallet_locator_lost_disabled_suffix` as its SB4-side test.
  - D41 makes the anchor "the SPV locator's terminus".
  - Core: `chain.cpp:32-54`. The `while` loop runs to height 0 and pushes genesis.
- **Evidence:** I added the probe `opus_probe_spv_locator_terminus` in my scratch copy, `spv_tests.rs`.
  - Setup: anchored regtest at ANCHOR 40,000 (floor 39,976), w = 10, 200 headers committed in steps of 50.
  - `locator(tip)` returns heights `[40200 … 40190]`: 11 entries, all single steps.
  - Core-density heights at or above the floor would add 40189, 40187, 40183, 40175, 40159, 40127 and 40063, then the anchor or floor as terminus.
  - `try_ancestor(tip, floor)` resolves fine (`Some(BlockId(0))`), so the cold path exists and the reader simply doesn't use it.
  - Sub-review C reproduced this on its own. Even without eviction the locator ends at 40,063 with no terminus, because the next Core height lies below the floor and nothing appends the anchor.
- **Tests:** the only locator test is node-side (`chain.rs:3622`). `wallet_locator_lost_disabled_suffix` does not exist on the branch.
- **Failure scenarios:**
  1. **getheaders (SB14's ChainAccess).**
     - Situation: a peer whose best chain forked from ours more than about w below our tip.
     - The peer sees no common entry in the locator. Core's `FindForkInGlobalIndex` then falls back to genesis (`validation.cpp:182-197`) and serves headers from height 1.
     - An anchored client refuses them (no known prev, below anchor), so the client cannot follow that reorg through that peer.
     - With an anchor terminus, the peer would answer from the anchor instead.
  2. **Wallet recovery (E3 / D41).**
     - A record's stored locator only covers the w blocks below the record's block. In the E3 scenario the reopened chain forks deeper than that, which is §15.2's A100/B110/fork 50 case whenever w < 60.
     - `fork_in_global_index` then finds no entry and falls back to `floor_id()`, the anchor, instead of Core's answer (37).
     - That is safe, since it only rescans more, but it is not the approved mechanism. It also means the anchor fallback is the only thing guarding recovery, and that fallback isn't tested in SPV either.
- **Impact:** with production w = 10,000 on mainnet, scenario 1 needs a reorg deeper than 10,000 blocks. It is reachable on test chains and with small windows, and it is a reviewed and approved design rule (n6, DEC-140) that the code silently drops. That is why this is major rather than minor.
- **Fix (small):**
  - In `locator_entries`, walk with the cold-capable ancestry (`try_ancestor`, tripping the latch on a store error). Stop at the first height below the floor.
  - In SPV, end with the floor entry's hash (the anchor) unless it is already last. In the node the walk already reaches genesis.
  - Add an SPV test at w = 10 that compares the heights with `walk::locator_heights` filtered to the floor, plus the terminus.
  - Add `wallet_locator_lost_disabled_suffix`: the SB4 half over `MemHeaderStore`, asserting rewind height 37 and the all-unknown → anchor control.
  - If the manager prefers to defer this, it needs a D-row and a DEC, because DEC-140 made the anchor terminus binding.

### Minor

**mi-1: §15.1/§15.2 rows ship with a named half missing, and no D-row registers the gap. This is the second miss in this class (after D-C3-7's promise), so a mechanism is proposed.**
- **Missing halves:**
  - **A-1 rule level.** `rules_cs3_header_rows_genesis`/`_anchored` (CS3 recorded rows replayed over `RuleContext`) do not exist. `checks_recorded`/`cs3` appear nowhere in `dash-chain`. D-C3-7 promised it "lands with C4"; it did not. The A-1a/A-2 acceptance replays do exist.
  - **A-8 oracle half.** `Oracle::next_work_required` on the 4,033 synthetic headers is open, per D-C9b-4 ("That is open"). It is an open item, not an accepted deviation.
  - **A-13.** The "cargo public-api diff checked in" half is absent. Only the `compile_fail` doctests exist in `anchor.rs`.
  - **A-37.** `spv_status_coherent_with_snapshot`, the stage-hook check that readers never see the new tip with old statuses, does not exist. Only the empty-step half does (`spv_quiet_timer_step_moves_nothing`, the A-37 tail in `cl_tests.rs:615`). Sub-review C's mutant M4b (overlay applied after `publish_locked`) survives the whole suite.
  - **A-27.** "Random trees **with cold leaves**" isn't met. `prop_ft_leaves_vs_brute_force` (`cl_tests.rs:741-788`) runs over `NoStore`, so it never reaches `cold_leaves()`, the `max_cold_leaf_work` gate or SPV `children()`. Sub-review B wrote a cold generator: 3,000 cases, 902 with cold leaves, 0 counterexamples. So this is a test gap, not a bug.
  - **E3.** `wallet_locator_lost_disabled_suffix`; see MJ-1.
- **Unreplayed Core-judged vectors.** These sit in `tests/data/core-scenarios/` and nothing reads them. `replay()` asserts `network == "regtest"` (`scenario_tests.rs`), and D-C3-8 lists only four vectors:
  - `a1a-testnet-min-difficulty`, `a1a-devnet-min-difficulty-window(-edge)` and `a1a-mainnet-stale-branch-chainlock`. A-1a says "regtest, devnet, testnet, mainnet". Devnet's 11-entry window and testnet's min-difficulty walk-back are judged by Core here and not replayed anywhere; A-4 only covers testnet heights in the nightly run.
  - `a38-deep-fork-time-too-old`, A-38's harness half. `spv_accept_deep_fork_cold_prev` compares windowed against unwindowed, not against Core.
  - `a19-first-seen-tie`, A-19's acceptance half.
  - `smoke-reload`.
  - `pnbh-testnet`, which is empty and referenced nowhere. `pnbh-mainnet-dgw` is read only by `benches/chain.rs`.
  - `a18-*` belong to CH1-node per D-C6-7. That's fine.
- **Failure scenario:** a regression in devnet/testnet difficulty dispatch, or in A-38's cold-prev MTP, passes CI even though Core's verdict for it sits in the repo.
- **Fix (mechanism):**
  1. A test that lists `core-scenarios/*.json` and fails unless each vector is replayed, or listed in a `NOT_REPLAYED: &[(name, owner/D-row)]` table. This mirrors the existing `SKIPPED` assertion.
  2. Extend `replay()` to the non-regtest networks via `params(NetworkKind::…)`, or register each one with a reason.
  3. Have the design-row ledger at the gate grep for every §15.1 test name, or for a D-row that defers it, so a missing half is visible before review. Then add A-1's rule-level replay, A-37's stage-hook test and the cold A-27 generator (sub-review B's `agentB/probe_cold.rs` is ready to lift).

**mi-2: `records_committed` clears the SPV overlay even if stage 4 never applied it. The resident status then falls behind the committed record.**
- **Location:** `chain.rs:2046-2054`.
- **Scenario:** a step marks CONFLICT (the overlay holds it) and stage 3 commits status 130. Then stage 4 returns `Err`. This can happen through:
  - `refuse_floor_conflict` (`chain.rs:1648`);
  - a stale `pending_plan` (`:1655`);
  - a cold `plan_end` read failure (`:1659`);
  - an abandoned hold (`:2382`) whose marks wait for "the next stage 4".

  If the driver then runs stage 5, the overlay is dropped. Sub-review C's probe: writer and readers read `Status(2)`, the store reads `Status(130)`.
- **Effect:** after that, `writer_status`, enforcement and `fork_target` read the stale resident status until eviction.
- **Test helper:** `spv_tests::step` itself calls `records_committed` without stage 4 when the tip doesn't move. That's harmless only because those steps mark nothing.
- **Fix:** set an `overlay_applied` flag in stage 4 and `Hold::release`. Then `records_committed` either refuses, or applies the resident groups itself when the flag isn't set.

**mi-3: node-only mutators are `pub` and unguarded in SPV, and misuse turns peer input into a panic.**
- **Location:** `chain.rs:2004` (`failed_blocks()`), `:1999` (`mark_dirty`) and `:2012` (`take_dirty`).
- **Scenario:** an SPV host inserts an id into `failed_blocks()`, and the entry is later evicted. Any peer header whose parent isn't `VALID_SCRIPTS` then reaches `mark_failed_descent`, which panics at `chain.rs:1266` (`expect("resident")`). `take_dirty` likewise panics on an evicted id, or steals SB5's per-step written set (D-C4-11).
- **Why minor:** nothing in-crate does this, but these are public API on a type that SB5 drives in SPV mode.
- **Fix:** `require_mode(ChainMode::Node)` (or `pub(crate)` plus a node accessor) on all three.

**mi-4: surviving mutants show untested Core steps and invariants.** All of these were verified as non-equivalent. The ported code is correct in every case; the risk is a future regression.
- **B, `enforce.rs:255`.** This is Core's first `m_best_header` walk-back (`validation.cpp:4089-4091`, which I checked against rc.3). Deleting it passes all 157 tests and all 25 `cl` vectors. The later :4107 walk-back masks it whenever `b` is the active tip.
  - Witness (the orphan race): tip a5; b6 then l6 at equal work, so the best header stays on the first-seen b6; a ChainLock on l6 marks b6; Core moves the best header to a5. Sub-review B's probes (`agentB/probe_tests.rs`, node and SPV) kill the mutant.
- **K, `enforce.rs:275-276`.** `conflicting_marked` before `set_status(b)` survives. CH1-node's candidate re-add (`validation.cpp:4129-4137`) depends on this order, as `host.rs:31-34` documents. A test host should assert `writer_status(b)` has CONFLICT inside the hook.
- **M16, `chain.rs:1651-1653`.** Dropping the "plan starts at the current tip" half survives. In release builds `step`'s check is a `debug_assert`, so a plan made stale by an intervening hold would corrupt the active chain. Needs a test that moves the tip through a hold between `tip_plan` and `set_active_tip`.
- **F, `enforce.rs:440`.** The cold-leaf tie at `>=` has no witness.
- **M6 / M13, `active.rs:191` and `arena.rs:153`.** Leaves never dropped, and chunks never released. Only the nightly A-31 would notice. Cheap lib assertions would catch them: leaves dropped when the base crosses a leaf, and the slot `None` after release in `eviction_resolves_cold`.

**mi-5: the loom model isn't run anywhere.** `dash-chain/Cargo.toml` says "CI only", but no workflow, `contrib/` script or SB4 gate runs `--cfg loom`. Sub-review C ran it by hand and it passes; it exercises the production `SeqGroup`. Wire it into the gate or CI, or drop the claim.

### Nits

- **n-1, `chain.rs:1050-1053`, `:1082-1084`.** If `accept` errors mid-batch, it loses the ids already inserted (needed by CH1's `AcceptedBlockHeader` listeners) and skips the status publish, even though `best_header` may have moved. Every error path is a latched storage error or an `Internal` that Core asserts on, so document "any `accept` error is fatal", or return the partial result.
- **n-2, `shared.rs:96-119`.** The IBD max-tip-age check uses `now` truncated to whole seconds, while Core uses `GetTime()` (`validation.cpp:1987`). The latch can trip up to 1 s earlier.
- **n-3, `derive`/`derive_spv`.** `height + 1` is unchecked against a corrupt store record with height `u32::MAX`: it panics in debug and wraps in release. `cold_parent` checks parent and child heights but not overflow. Use `checked_add` and a corruption error.
- **n-4.** SPV `set_active_tip` has no `catch_unwind`/latch around `run_plan` + `stage4_effects`, unlike the node composite. A panic there leaves the tip lock poisoned but the latch untripped.
- **n-5.** §13.1 still says eviction never takes the index write lock under the tip lock. The code does take it, as D-A11-3 accepts, and there is no deadlock. Update the design text, or cite D-A11-3 there.
- **n-6, `enforce.rs:48-53`.** `ClOutcome::Floor { enforced: None }` is documented as "observed off". It can also mean the second `check_cl_active_state` reset the floor (live spork flipped mid-call). The state is Core's (`handler.cpp:168-176`); only the doc is wrong.
- **n-7 (simplification), `derive` (`chain.rs:589`) and `derive_spv` (`:629`).** They duplicate the entry derivation and differ only in how they read prev (resident `expect` vs cold `node()`). One body over a `node`-style closure would remove about 30 lines and the risk of the two MTP loops drifting. M6/M7 needed separate mutants for exactly this reason.

## Confirmations

All of the following were checked against Core v24.0.0-rc.3 source.

- **Acceptance (`header_verdict`, `accept`)** follows `ProcessNewBlockHeaders`/`AcceptBlockHeader` (`validation.cpp:4590-4736`) step for step:
  - a known duplicate checks FAILED before CONFLICT;
  - the genesis short-cut re-fires `AcceptedBlockHeader`;
  - then `CheckBlockHeader`, the prev-missing/prev-invalid checks, and the contextual check;
  - the step-7 `m_failed_blocks` loop is gated on `!IsValid(BLOCK_VALID_SCRIPTS)`, iterates in id order (D9), and marks and dirties FAILED_CHILD;
  - step 8 inserts with `VALID_TREE|CONFLICT_CHAINLOCK`, with no signal and `best_header` untouched (`blockstorage.cpp:111-119`).
  - The late-header floor update compares hash and height (`chainlock.cpp:152-172`).
  - Reject classes, reasons and debug strings match. My mutants M2–M5, M8, M11 and M14 were killed.
- **Insertion and best header:** strictly more work, a valid status only, and Core's arithmetic for chain work. **Load:** the best-header tie goes to the lowest id, which is order-independent and equals Core's comparator under D9 (my M1 and M12 were killed). FAILED_CHILD propagation is marked dirty (M9 killed), and inconsistent heights are refused (M16 killed). Load can't panic, overflow or allocate by height on disk bytes, and cycles are refused (sub-review A).
- **Rule context:** `read_envelope` covers BTC retarget plus the min-difficulty walk, KGW `PastBlocksMax + 1`, DGW 25 and the minimum 11 (M10 and M13 killed). Node and SPV MTP spans are 11 (M6 and M7 killed).
- **Enforcement:** `EnforceBlock`, `MarkConflictingBlock` and `ResetBlockFailureFlags` (`validation.cpp:4048-4220`), the ChainLock state machine (`chainlock.cpp:73-172`, `handler.cpp:136-222`) and SPV `fork_target` (a) ∧ (b′) all match Core and §6.2/§7. Sub-review B killed 14 of its 19 mutants; the survivors are under mi-4, plus one equivalent and one replaced.
- **Concurrency:** the seqlock is race-free (the stress test catches a torn read). Arena publication and eviction under racing readers are safe, because ids are never reused. Tip lock, hold and the node composite refuse re-entry and keep `epoch`/`tip_seq` in step. `Dirty`'s `lo` splice and 64-word cap never lose an id. The `ActiveChain` windowing (`set_base`, `drop_leaves`, `extend_down`, DEC-174) has no boundary off-by-one, and `find_fork`/`next`/`contains`/`fork_in_global_index` match Core.
- **D-row confinement:**
  - D-C4-12 (the infallible readers are resident-only) is confined to the readers it names, except `locator`, which falls outside the D-row (MJ-1).
  - D-C3-7's C4 promise and A-8's oracle half are open; see mi-1.
  - D-C6-7 (`a18-*` to CH1-node) and D-C9b-4 (A-5…A-7 via A-4 and the tolerance-era test) hold as stated.
  - DEC-167's fixed-point replay (`replay_cl_vectors`, 25 vectors) and DEC-172's perturbed replay pass.
- **Panics:** production `expect`/`panic` sites are resident-only invariants, the D-C4-11 caller bug, or reachable only through the API misuse in mi-3. None is reachable from peer or disk bytes on the supported paths. The crate is `#![forbid(unsafe_code)]`.
- **Memory:** index growth is bounded by PoW, as in Core. SPV residency is bounded by the window. Overlay and `Dirty` shrink at stage 5.

## Validation run

Environment: `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, `TMPDIR=/work/scratch/sb4-opus/tmp` (0700) and per-worktree target dirs.

- **Clean head:** `cargo test -p dash-chain --all-features` at 69745378a.
  - Lib: 157 passed, 2 ignored (`real_tolerance_era_contexts_match_complete_ancestry` and `locator_heights_match_core_on_mainnet`; both need H-M).
  - `core_codec` 1 ignored, `full_chain` 6 ignored and `spv_memory` 1 ignored (nightly data).
  - Doc-tests: 1 + 5 passed.
  - Exit 0.
- **My mutants** (`SB4_MUT`, full lib suite each). All 16 were killed; the baseline M0 passed.

  | # | Mutation | Killed by |
  |---|---|---|
  | M1 | best header `<=` | `replay_cl_vectors`, `best_header_is_first_seen_on_a_tie` |
  | M2 | CONFLICT duplicate before FAILED | `accept_pnbh_index_state`, `replay_a28_pnbh_index_state` |
  | M3 | step-7 gate always runs | `the_failed_blocks_loop_is_gated_and_ordered` |
  | M4 | late header compares hash only | `a_late_header_moves_the_floor` |
  | M5 | no conflict check at floor height | 3 `accept_tests` |
  | M6 | node MTP span 10 | `replay_a1a_pnbh_rules_genesis`, `index_fields_follow_core_on_two_branches` |
  | M7 | SPV MTP span 10 | `replay_a2_pnbh_rules_anchored`, `eviction_resolves_cold`, `spv_accept_deep_fork_cold_prev` |
  | M8 | `accept` doesn't publish | `a_batch_extends_the_index_and_publishes_once`, `a_late_header_moves_the_floor` |
  | M9 | load FAILED_CHILD not dirty | `load_propagates_failed_child_and_marks_it_dirty` |
  | M10 | KGW envelope +1 dropped | 3 `rules::tests` |
  | M11 | failed descent not dirty | `failed_blocks_mark_the_descent_lazily` |
  | M12 | load tie to highest id | `load_breaks_a_best_header_tie_by_load_id` |
  | M13 | BTC min-difficulty envelope ignored | `read_envelope_follows_the_dispatch`, `a_resent_cold_genesis_is_accepted` |
  | M14 | conflicting insert as VALID_TREE | 4 tests (incl. `replay_cl_vectors`, a1a, a28) |
  | M15 | SPV children omit uncommitted | `spv_children_join_the_store_and_the_step` |
  | M16 | load height-mismatch check off | `load_refuses_an_inconsistent_index` |

- **Sub-review mutants:**
  - B: 19 run, 14 killed; B, F and K survived (mi-4); Q is equivalent; I did not compile and was replaced by S.
  - C: 13 run; M4b, M6, M13 and M16 survived (mi-1 and mi-4); M12 is equivalent.
- **Probes** (all in scratch copies, then reverted):
  - `opus_probe_spv_locator_terminus` fails on HEAD (MJ-1).
  - `agentc_probe_spv_locator_reaches_the_anchor` fails on HEAD (MJ-1).
  - `agentc_probe_records_committed_without_stage4_drops_marks` reproduces mi-2.
  - `probe_4089_offchain_best_header_sibling{,_spv}` passes on HEAD and fails with :4089 deleted (mi-4).
  - `probe_cold_ft_vs_brute` found 0 counterexamples in 3,000 cases (mi-1, A-27).
- **Loom:** `RUSTFLAGS="--cfg loom"`, run by hand by sub-review C. Passes.
- **Not run:** the dashd integration suites (`dash-spv`/`-ffi`). SB4 doesn't touch those crates. The nightly H-M/H-T tests weren't run either, since that data isn't fetched in the scratch worktree; R1/R2 and D-C10-8 record them passing at the final code.

No security note.

**Verdict:** CHANGES-REQUIRED
