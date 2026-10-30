# CH3a a7-2 (L2 driver) and the a7-1 r1 fix round: independent review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `node/ch3a-a7` at **53fa97779**: twelve commits on 63d401c79, all signed (`%G?` = G). The fix round is `63d401c79..4a25b480f` (ea37964d4 and 53cb119a5, f7a00e43c, 011bac334, 4a25b480f). Slice a7-2 is `4a25b480f..53fa97779` (b6dfe4ad4, a7227efc7, dae8977c2, 7244670b3, ecf1f3d61, c3149b4f8, 53fa97779). The restack was checked with `git range-diff 63d401c79..node/ch3a-a7-2-pre-restack 4a25b480f..c3149b4f8`. |
| Against | Dash Core `v24.0.0-rc.3` (`v` = `validation.cpp`): `shutdown.cpp:27-37` `AbortNode`; `v:2239-2243`; `v:1809-1870` `GetBlockSubsidyHelper`; `v:1884-1889` `GetBlockSubsidy`; `v:2542-2571` `fScriptChecks`; `v:3270-3304` `DisconnectTip`; `v:3428-3457` `ConnectTip`; `v:3601-3680` `ActivateBestChainStep`; `v:3724-3855` `ActivateBestChain`; `v:3889` `InvalidateBlock`; `v:4896-4901`; `v:5439`, `:5463` (`-loadblock` frame bounds); `init.cpp:314` `PrepareShutdown` and its two flushes, `:379-385` and `:449-455`; `chain.cpp:147-160` `GetBlockProofEquivalentTime`; `validation.h:77-83`; `net_processing.cpp:1210-1216`; `chainparams.cpp:230-231`, `:394`, `:402`, `:439-440`; `consensus/consensus.h:10-15`; `rpc/blockchain.cpp:2010-2018`. O8's patch is read by function name. |
| Method | I built in a detached worktree, `/work/scratch/ch3a-a7-review/r2/wt`, with a target reflink-seeded from the implementer's and every `.rs`/`Cargo.toml` touched. At load 30–360 I ran `cargo test --locked` for `dash-conformance` (**188 passed, 0 failed**, over four invocations because the FT replays outran a 10-minute call; `digest_replay_core` ran in a systemd unit: 3/0 in 391 s, with the testnet test skipping loudly), and for `dashd-chainstate` and `dash-statedigest` (**816 passed, 0 failed**). In release I ran `digest_replay_core` and `digest_l2` with `CH3A_TESTNET_WINDOW=/work/scratch/ch3a-a7/testnet`, reading the window only. Hand mutants were env-gated in a second worktree (`A7MUT=n`, script `/work/scratch/ch3a-a7-review/scripts/a72_mutants.py`, logs in `/work/scratch/ch3a-a7-review/r2/mutlogs/`). I read the gate run directory and the bench_inputs output. I did not start dashd, made no network or oracle calls, and ran no PF0. |
| Verdict | **ACCEPT WITH NITS.** There is no Blocker or Major. One Minor: D137's declared residual is narrower than the code's, as the manager suspected. Five Nits. Every r1 finding's fix passes. Of 15 mutants (counting M3's three forms), 14 are killed. The survivor, M12, is a test gap on the preload's refusal checks. |

## 1. The manager's two notes

### Note 1: D137's residual against Core's `AbortNode` → `StartShutdown` → `Shutdown` order

**Core's order after a failed record.**

1. Inside `AttemptScope::Guard`, `Fail` sets `g_failed` and calls `AbortNode(msg)` (`shutdown.cpp:27-37`). That sets the misc warning, logs `*** msg`, calls `InitError`, stores `EXIT_FAILURE` and calls `StartShutdown()`. It does nothing else: no latch, and no store is closed.
2. The attempt runs on with its own outcome:
   - **`C`:** `ConnectTip` runs `view.Flush()` and `dbTx->Commit()` (`v:3446-3448`), then the IF_NEEDED `FlushStateToDisk` (`v:3457`), which writes only if the cache is critical. Then `UpdateTip`.
   - **`D`:** `DisconnectTip` runs `view.Flush()` and the commit (`v:3285-3287`), then IF_NEEDED (`v:3304`).
   - **`R`:** `InvalidBlockFound` marks the block `FAILED_VALID` (`v:3434-3435`).
3. `ActivateBestChainStep` never checks for shutdown (`v:3601-3680`). It returns after the first connect that betters the old tip. ABC's inner loop (`v:3811`) keeps stepping while the tip has less work than `starting_tip`, so a reorg in progress is carried through to a better tip.
   - After an `R`, `blocks_connected` is already `true` (`v:3788` is set after every successful step, invalid or not). So `v:3812`'s early return does not apply, and ABC reaches `if (ShutdownRequested()) break;` (`v:3841`) and the PERIODIC flush (`v:3851`).
   - **The port matches this.** `abc.rs:907-964` sets `blocks_connected` after `abc_step` whatever it found, checks `shutdown_requested()` at `:964` and flushes PERIODIC after the loop. So D137's "after an `R` at ABC's PERIODIC site (`v:3851`), once `ShutdownRequested()` has ended its loop (`v:3841`)" is right.
4. `Shutdown()` → `PrepareShutdown` (`init.cpp:314`) runs `ForceFlushStateToDisk()` for every chainstate with `CanFlushToDisk()`, twice (`:379-385`, `:449-455`). Everything Core did after the failed record is made durable: the block or blocks connected, the reorg, the `FAILED_VALID` mark, header and block-index changes. On restart Core resumes at its shutdown tip and redoes nothing.

**The port.** `AbortPath::abort_node` (`seams.rs:221-232`) trips the latch before calling the sink. In production that latch is the stores' latch, `self.engine.latch()` (`dashd-node/src/chainstate.rs:321`, `:418`). So every store write after the trip is refused: the next flush site, and both of ND1's shutdown flushes. V stops at its first latched flush site.

**Rulings on the manager's three questions.**

- **Is the residual exactly as stated, no wider? No: it is wider.**
  - D137 says "a restart redoes the block". Once the latch refuses every write, the restart actually resumes from the **last durable flush**. Everything after it is lost and redone:
    - every block connected since that flush, which in IBD can be a whole dbcache's worth, not one block;
    - every block-index change since then: headers accepted, `HAVE_DATA`/`HAVE_UNDO` positions, the `R`'s `FAILED_VALID`;
    - for a `D`, the disconnect and the rest of the reorg. Blocks whose data was stored but whose index entries were never flushed may have to be fetched again.
  - This is exactly a crash at the trip, so it is consistent with CH2b. It is not "the block".
- **Is "nothing runs after the stop that Core's shutdown would not run" true? Yes.**
  - For `C`, `R` and `D`, V stops in the same ABC call, at or before the point where Core's own loop ends: Core may connect more (the rest of a reorg step, `v:3811`). The port's shutdown writes nothing, where Core flushes.
  - For a proposal, V runs until its next flush site. Any store write it attempts in between (a block's data in `AcceptBlock`) is refused by the latch where Core would perform it. So it is still a subset of Core, though it can log one more failure report than Core does.
- **Is a redone block consistent with CH2b in each case? Yes.**
  - **`C`:** the stores sit at a flush point, and recovery replays from it. That is CH2b's crash model.
  - **`R`:** the `FAILED_VALID` mark lived only in memory, so the restart revalidates the block. The connect-time verdict is a function of the chain and the block, so it marks the block again. (Mutated blocks are never marked, `v:3649`, so D138's case is unaffected.)
  - **`D`:** the restart is at the pre-disconnect flush point, and the reorg reruns when the better chain is found again.
  - **Proposal:** nothing is durable on either side.

**Where this comes from.** The residual is the connector `AbortPath`'s rule (CH3 D52, "trips the latch if untripped"), which every connector `AbortNode` shares. D137 says "as after any connector AbortNode", which is true. Note, though, that CH1-node D62 (DEC-241) argues the opposite for V's own `AbortNode`s: "Core's `AbortNode` only … requests shutdown, and both shutdown flushes run", so a non-store failure leaves the store open. A digest record failure is the clearest non-store failure there is, since the stores are healthy. The two rules disagree, and a7 did not create the disagreement. See m-1 for the wording fix and the question for the manager.

### Note 2: the M7 V20−1 claim

**The implementer is right, and r1's "only when `nPrevHeight > nBudgetPaymentsStartBlock`" was incomplete.**

- `GetBlockSubsidyHelper` sets `force_fixed_base_subsidy = fV20Active || (devnet && nPrevHeight >= nHighSubsidyBlocks)`, and with it `nSubsidyBase = 5` (`v:1822-1830`). Without it, the base comes from the difficulty era (`v:1831-1849`).
- The treasury part is `nSubsidy / (fV20Active ? 5 : 10)`, and only above `nBudgetPaymentsStartBlock` (`v:1865-1867`).
- So M7 (V20 taken at `pindex` instead of `pindex->pprev`'s child) changes block V20−1's subsidy in two independent ways:
  - the base, **whatever the budget start**, whenever the difficulty-era base is not already 5;
  - the treasury split, above the budget start.

The claim needs one qualifier: where the era base is already clamped to 5 (the GPU/ASIC era at high difficulty, as on mainnet at V20, or a devnet past `nHighSubsidyBlocks`), only the split differs, and then only above the budget start.

The rig (`the_c_before_v20_has_the_pre_v20_subsidy`: regtest, V20 at 6, `-budgetparams=240:1:1500`) exercises both ways at once: base 500 → 5, treasury 1/10 → 1/5. Re-applied, **M7 dies**: `stats[0]` is `400000000` against `45000000000` (4 against 450 DASH; `mutlogs/lib-7.log`). The test's doc states both mechanisms correctly.

## 2. The r1 findings: did each fix pass?

| r1 | Fix | Verdict | Evidence |
|---|---|---|---|
| m-1 | 53cb119a5 | **PASS** (code and test). The D137 wording is m-1 below. | `report_failed_record` (`connector.rs:139-143`) reports through `core.abort` and returns nothing, and every call site keeps `out`. `DigestLog::record` latches `failed` (`digest_log.rs:343-350`). `a_failed_record_write_fails_only_the_recording` covers C, R, D and the proposal. M16 (no report) is killed by 3 tests; M17 (no `g_failed`) is killed by it. The D121 interplay (the refusal stays the answer, and the write failure is reported) is sound. |
| m-2 | 4a25b480f | **PASS** | `abc.rs:650` (`SystemError → BlockState::Error`) and `:664` (the log line) are right, as are `v:3432`, `:3436` and `:4901`. BL-317 is in `node-program/BACKLOG.md:316`. Nothing else in the `Mutated` path diverges beyond those three, the `R` and the returned state. |
| m-3 / M3 | f7a00e43c | **PASS** | M3 on the shared mapping: killed by 2 tests. M3L (DigestLog's call site only): killed by `an_unclean_or_failed_disconnect_records_its_res_and_rewinds_nothing`. M3T (the tap only): killed by `the_tap_records_an_unclean_and_a_failed_disconnect`. |
| m-3 / M7 | f7a00e43c | **PASS** | §1, note 2. |
| N1 | 011bac334 | **PASS** | `derived` makes a missing row a `Fatal` (`digest_log.rs:184-186`); `a_c_without_versionbits_answers_fails_its_record`. |
| N2 | 011bac334 | **PASS** | Reproduced from my release build: 7,000 pairs, range (0, 6999), all 18 keys "equal 7000 of 7000". The module doc and D146 say "from height 0". |
| N3 | 53cb119a5, 4a25b480f | **PASS** | D137/D138 cite `Guard`, `AttemptScope::{Failed, Threw, Rejected}`, `Fail`/`WriteLine` and the patched `ConnectBlock`'s comment. D144 still cites patch line numbers, but it was outside N3. |
| N4 | 011bac334 | **PASS** | `digest_log.rs:155`. |

**The restack resolutions** (range-diff): **PASS.**

- The union of `failed` and `observer` is right.
- N1's literal gained `script_checks: Some(true)`.
- `observed()` uses `node_stop`.
- `an_observer_error_stops_v_at_that_rewind` now asserts the D137 order: the disconnect flushed (`coins_at == b1`), one report, the shutdown requested, the tip still at b2, and nothing after the `D`.
- The trait doc and D152 follow D137.
- One leftover is N1 below.
- **A mismatch in the reverse leg fails the run with its text.** `a_corrupted_golden_is_named_by_height_and_key` asserts the exact string.
- **The leg cannot report clean after an observer error.** Even with the kept text ignored (M14), the run fails, now with V's `AbortNode: ["reverse leg: at 50, …"]`, because `rig.stop()` returns V's stop.

## 3. Slice a7-2

### Streaming (D148, D149)

- **D21's refusals come before V starts.** `node_leg` → `load_corpus` → `frames::prepass` runs before `Rig::start` (`replay_core.rs:125-150`), and likewise in `replay.rs:155-194`. The hash is checked before the shape (`frames.rs:204-211`).
- **A pass that differs from the pre-pass is refused** by `finish_pass` (`frames.rs:231-245`): block count and SHA-256. M15 is killed by `a_pass_that_differs_from_the_pre_pass_is_refused`. The refusal comes after V has run, but the run fails and removes its output (`remove_partial`). That is acceptable for a frozen input.
- **The frame bound:** `MAX_FRAME` = 2,000,000 = `MaxBlockSize()` (`consensus.h:10-15`, `v:5439`, `:5463`). Refusing where Core resynchronises is right for a file Core wrote.
- **Is the memory test meaningful? Yes.**
  - `VmHWM` counts anonymous and file-backed resident pages. So a `Vec` of the input, a whole-file read, or an mmap walked end to end would each add at least about 47.5 MB, against a bound of about 5.9 MB.
  - The synthetic coinbases pay to `OP_RETURN`, so the coins set stays flat, and V's block files are on disk (`disk_block_files`). Nothing else grows with N.
  - The synthetic leg is `replay_window` over `Frames`. The node path shares `feed` with it and `replay_pass` with the null leg, which the `feature_block` child bounds at 1/4 of 1.09 GB. So tier 1 bounds every streamed stage.
  - Both tests passed in my run (3/0, 62 s).
- I did not rerun cargo-mutants on `frames.rs`. The `Sha256Reader::read → Ok(1)` timeout explanation holds by reading: `finish`'s `io::copy` never sees EOF.

### `blkdir.rs` (D150)

- `BlockDir::open` refuses any index issue except a cut record at the end of the newest file.
- Every header or block read must hash to its child's `prev` (or the tip's hash, which both libraries must agree on).
- Reads go through `read_record`, which checks the record header again.
- Memory is the best chain's `Record`s only.
- The testnet test cross-checks every height's (file, offset) against the pinned `located-0-6999.json`, and it passed in my run.
- **Duplicates:** `Chain::build` keeps the first record of a hash (pre-existing blk-mode code).
- **"Best chain" here is the highest chain**, with ties going to the earlier record. It is not Core's most-work chain (N3).

### D44 (D151, D153)

- **`script_checks` is the value `serial.rs` decides:**
  - `None` at entry (`:634`);
  - `Some(decision)` right after `self.script_checks(cx, id, h)` (`:712`);
  - read only by `record_connect`.
  - `script_checks()` matches `v:2542-2571`, including the ancestor, best-header and min-work conditions and `<=` 1,209,600 s.
  - It is additive. With `digest: None` nothing reads it, and the None path gains two stores per connect. bench_inputs agrees: only `dashd-chainstate` is CHANGED, so PF0's usual re-gate applies. dashd-node is untouched.
- **The preload:**
  - batches of 2,000, `MAX_HEADERS_UNCOMPRESSED_RESULT` (`validation.h:82`). A headers2 peer would send 8,000 (`net_processing.cpp:1213`), which changes nothing that is decided.
  - `stop.is_none()` and `last_accepted == lookup(last)` are the right checks. Nothing tests them (M12, N2).
- **The choices** match Core's parameters: testnet `defaultAssumeValid` and `nMinimumChainWork` at 1,400,000 (`chainparams.cpp:439-440`); mainnet at 2,487,500 (`:230-231`).
  - The "preloaded tip" fallback for regtest's null default is reasonable.
  - Lowering `-minimumchainwork` to the tip's work when the default is above it is a deliberate departure from a node's default. It is recorded with its reason in the manifest. Accepted.
- **Condition 5's boundary is pinned on both sides:**
  - B = 8,166 skips 1..101 and checks 102, at exactly 1,209,600 s;
  - B = 8,167 checks from 103.
  - M11 (`<` for `<=`) dies: the skips run to 102.
  - The arithmetic holds: regtest's proof is 2 per block, so 8,064 × 150 s = 1,209,600 s, and `chain.cpp:147-160`'s `r * spacing / GetBlockProof(tip)` is exact.
- **Testnet, reproduced from my build:**
  - on: 1,574,533 headers in 788 batches, all 6,999 blocks skipped, the `C`s byte-equal to the off run's, and the reverse leg to 0 with 6,999 comparisons;
  - off: reverse to 2,000, with 4,999 comparisons and a 3,232-coin scan.
  - `digest_l2` passed 6/0 in 465 s and `digest_replay_core` 3/0 in 121 s.

### D48 (D152)

- **One `invalidate_block` of L+1.** The ancestor is looked up from V's tip (`l2.rs:885-893`), which matches `rpc/blockchain.cpp:2010-2018` (`InvalidateBlock`, then ABC). The run then asserts V's tip is L.
- **The observer gets the parent's view:** height `h−1`, hash `block.prev_hash()`, and the accumulator after `reverse_block`. `GoldenCheck` looks up `want[to.height]` and compares the block, then `muhash`, `txouts` and `amount`.
  - M9 (`golden(h)`) dies on the block check.
  - M8 (the pre-rewind accumulator under the parent's label, i.e. golden(h)'s values) dies on `muhash`, in both the lib test and the corrupted-golden test.
- **The end check** asserts that no durable or frozen layer exists, then folds every live coin of the active layer into a fresh MuHash. M13 (no end check) is killed.
- **The separate reverse log:** its `H` has run `<run>-reverse`. The test asserts 110 `D` lines, all `ok`, in descending height.
- **The count checks** require tip − L disconnects, all `ok`, no `C` or `R`, and tip − L comparisons.

### D154, D155

- **The manifest** carries what D154 lists, including the CPU model, nproc, both load averages and the git dirty flag.
- **V time** is first `Step::Send` → last `Step::Reply`. It includes the digest sink's MuHash and the reader waits, and leaves out the index, the preload and the reverse leg.
  - That is the right figure for R11 as written ("45 min of **V time**" for the per-PR digest replay, which runs with the sink).
  - The gate's per-PR budget pays the index and the preload too (N4).

### D156–D158

- **D156:** the env gate is honoured. Tier 1's L2 coverage is as listed.
- **D157:** BL-315 is in the backlog (`BACKLOG.md:314`).
- **D158:** the coverage gap is stated accurately, and matches r1 §1.

## 4. Findings

### Blocker / Major

None.

### Minor

- **m-1. D137's declared residual understates its extent, and it is the connector `AbortPath`'s rule, which runs against DEC-241's.**
  - **The problem.** "So a restart redoes the block" (CH3a-notes D137) should say that a restart resumes from the last durable flush, as after a crash. Every block connected since then is redone, along with every block-index change, the `R`'s `FAILED_VALID` mark, and a `D`'s whole reorg. Core's `PrepareShutdown` flushes all of it (`init.cpp:379-385`, `:449-455`) and redoes nothing.
  - **What holds.** The residual is consistent with CH2b in all four cases (§1). "Nothing runs after the stop that Core's shutdown would not run" is true.
  - **Fix.** Reword the sentence and cite CH2b's crash model. Optionally, add a line that the latch is the stores' latch (`seams.rs:221-232`; `dashd-node/src/chainstate.rs:321`, `:418`).
  - **For the manager, not a7.** CH1-node D62 (DEC-241) leaves the store open after V's own non-store `AbortNode`, on the ground that Core's shutdown flushes. The connector's `AbortPath` trips the stores' latch on every `AbortNode`, including this one, where the stores are healthy. Whether connector `AbortNode`s that are not store failures (digest records, the D69 refusals, the EvoDB-inconsistency abort) should follow DEC-241 is a cross-chunk question. It could be a backlog row.

### Nits

- **N1. A stale module doc.** `digest_log.rs:17-20` still says the observer's error "is the attempt's `Fatal`, which stops V". Under D137/D152 it is a failed record: an `AbortNode`, and the attempt keeps its outcome. The trait doc (`:104-111`) is already right; make the module bullet match.
- **N2. M12 survives: the preload's refusal checks are untested** (`l2.rs:594-599`).
  - No test feeds a batch V stops or partly accepts.
  - The manifest's `preload.headers` counts headers **sent**, so a run whose preload was refused part-way would report the same count.
  - A run that dropped those checks would still pass every test (checked against `condition_5…` and `assumevalid_on…`).
  - Add a synthetic case whose batch V refuses, for example a header with a bad `prev` or bad PoW in batch 2, and assert the "stopped" error. Or record `last_accepted`'s height in the manifest.
- **N3. "Best chain" is the highest chain, not Core's most-work chain.** D150 and the `blkdir.rs` module doc say "best chain … to the highest block". `Chain::build` picks the greatest height, and on a tie the earliest record (`blkfile.rs:368`).
  - On testnet a long low-difficulty side branch can exist, and the "best chain" would then differ from Core's.
  - A golden compare would catch a wrong branch, and `--to`/anchor avoid it. A run without a golden would not.
  - Say it in D150, and name the anchor as the remedy.
- **N4. R11's figure should go beside the per-PR budget.** D155 is right for R11's literal "V time". The per-PR gate step also pays the index (about 120 s of CPU on testnet's 29 files) and, when on, the preload (124–142 s). When the lead rules R11 on mainnet, quote `total_wall_s` beside `v_time_s`.
- **N5. A correction of r1, recorded here.** r1's m-3 said M7 differs "only when `nPrevHeight > nBudgetPaymentsStartBlock`". That was wrong. V20 also fixes the base at 5 (`v:1822-1830`), so M7 differs at V20−1 whatever the budget start, unless the era base is already 5 (§1, note 2). The rig is right, and so is its doc.

## 5. Mutants

All are env-gated in one build: `scripts/a72_mutants.py`, plus `run_lib_mutants.sh` and `run_l2_mutants.sh`. The baseline runs are clean: the lib's digest filter 20/0, `frames` 8/0, and `a_corrupted_golden` 1/0.

| # | Mutant | Result |
|---|---|---|
| M3 | `D.res` swaps `unclean`/`failed` in the shared `disconnect_res` (both writers) | **killed** by `an_unclean_or_failed_disconnect_records_its_res_and_rewinds_nothing` and `the_tap_records_an_unclean_and_a_failed_disconnect` |
| M3L | the same, at `DigestLog`'s call site only | **killed** by `an_unclean_or_failed_…` |
| M3T | the same, at the tap's call site only | **killed** by `the_tap_records_…` |
| M7 | `subsidy_of`'s V20 flag as `active_after(at.id)` | **killed** by `the_c_before_v20_has_the_pre_v20_subsidy` (4 vs 450 DASH) |
| M8 | observer handed the accumulator **before** the rewind (golden(h)'s values under h−1's label) | **killed** by `a_rewind_observer_sees_each_parents_golden_keys` (lib) and `a_corrupted_golden_is_named_by_height_and_key` (`muhash` at 102) |
| M9 | `GoldenCheck` compares with golden(h) instead of golden(h−1) | **killed** by `a_corrupted_golden_…` (the block at 102) |
| M10 | `script_checks` observable inverted (`serial.rs:712`) | **killed** by `a_connect_attempt_carries_its_script_checks_decision` (lib) and `assumevalid_on_skips_every_block_and_logs_what_off_logs` |
| M11 | condition 5 `<` for `<=` (`dash-consensus/src/connect.rs:98`) | **killed** by `condition_5_ends_the_skips_two_weeks_below_the_best_header` (skips run to 102) |
| M12 | preload's `stop`/`last_accepted` checks dropped | **survives** `condition_5…` and `assumevalid_on…`, and by reading every test (N2) |
| M13 | end check skipped | **killed** by `a_corrupted_golden_…` (the golden(103) case) |
| M14 | the observer's kept mismatch text ignored | **killed** by `a_corrupted_golden_…`. The run still fails, with V's `AbortNode` text, so no clean report is possible |
| M15 | `finish_pass` accepts a differing pass | **killed** by `a_pass_that_differs_from_the_pre_pass_is_refused` |
| M16 | a failed record not reported (no `AbortNode`) | **killed** by 3 tests: `a_failed_record_write_fails_only_the_recording`, `a_log_that_missed_a_block_stops_v` and `an_observer_error_stops_v_at_that_rewind` |
| M17 | `DigestLog` keeps writing after a failure (no `g_failed`) | **killed** by `a_failed_record_write_fails_only_the_recording` |

## 6. Gate and evidence

- **Gate run** `/work/scratch/gate-fast/runs/1021-153507-gate-53fa97779-okX`: every step in `steps.tsv` exits 0, and the test step reports "tests passed 6681 failed 0". Matches the READY message.
- **bench_inputs** (`bench-inputs-a7-2.out`): `CHANGED: ['dashd-chainstate']`. Matches.
- **My counts** match the lead's: conformance 188/0, chainstate 699/0 plus statedigest 117/0 (816/0). I did not run the five-feature chainstate count (868) or the node suite (154).

## 7. Possible security note

None from this slice.
