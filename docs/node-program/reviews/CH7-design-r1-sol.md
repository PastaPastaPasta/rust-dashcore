REWORK

## Findings

1. **Major — H7 is placed after the commit, contrary to Core and approved CH3 D47.**
   - **Section:** §3.8 (effect application), §5.2 D15, and the H7 decision register.
   - **Core:** `src/validation.cpp:2972-2976` (connect) and `src/validation.cpp:2391-2395` (disconnect); the outer transaction is committed later at `src/validation.cpp:3532-3552`.
   - **Problem:** The design says the tip-path `list_changed` callback runs “after the commit” (`CH7-evo-llmq-chainstate.md:396`, `:634-637`). Core calls the synchronous MN-list listeners after `WriteBestBlock` but before the coins flush/EvoDB transaction commit and before the chain tip is published. Approved CH3 D47 (`CH3-chainstate.md:507-509`, `:1206`) explicitly requires that position because `govman.CheckAndRemove()` reads the pre-block coins view. The design supplies no listener proof that would permit moving it.
   - **Fix:** Specify H7 as a synchronous callback at Core’s in-attempt position: after H6’s staged best-block write, before `view.Flush`/EvoDB commit and before tip publication, for both connect and disconnect. If a later architecture cannot provide that position, enumerate every listener and prove its reads are independent, then amend CH3 D47 and the acceptance tests.

2. **Major — D7 contradicts the approved B1 protocol about when `meta.evo_tip` is durable.**
   - **Section:** §3.5.1 D6/D7 and §6.1 B1, especially `CH7-evo-llmq-chainstate.md:675` and `:1826-1833`.
   - **Core:** Core writes the best-block marker in the connect/disconnect transaction (`src/validation.cpp:2965-2967`, `:2387-2389`), while its full-flush EvoDB commit is after the coins flush (`src/validation.cpp:3160-3168`).
   - **Problem:** D7 says `meta.evo_tip` is written “in the same batch as the coins tip,” but the specified B1 protocol puts `meta.evo_tip`, `head_blocks`, and the Evo rows in B1, while `meta.tip` is written in B1 only for the small case and otherwise in the final coin batch Bk. Those statements describe different durable prefixes. An implementer cannot tell whether `evo_tip` belongs in B1 (the required all-Evo-before-coins ordering) or must wait for Bk, and recovery behavior after a crash immediately after B1 depends on that choice.
   - **Fix:** Make D7 agree with the approved A1/CH2 sequence: put `meta.evo_tip` with the Evo rows and `head_blocks` in B1; put `meta.tip` in B1 only when all coins fit, otherwise in Bk. State the marker/recovery invariant for the B1-only prefix and remove the claim that `evo_tip` is in the same batch as the coin tip.

3. **Major — D20 does not define a realizable log position while preserving B1 ordering.**
   - **Section:** §6.1 D20 (`CH7-evo-llmq-chainstate.md:676`) versus the B1/B2…Bk sequence (`:1826-1833`).
   - **Core:** `LOG_TIME_SECONDS("write evodb cache to disk")` wraps `CommitRootTransaction` only after `CoinsTip().Flush()` at `src/validation.cpp:3160-3168`.
   - **Problem:** The design says to emit that Core log pair around `write_b1` “after the coins rows are put,” but B1 must contain all Evo state before the coin sub-batches. In a multi-batch flush, `write_b1` cannot both be the pre-coin durable write and occur after the coin writes have been committed. The ambiguity changes log timing, failure reporting, and the crash test’s observable prefix.
   - **Fix:** Choose and state one contract. Either log around the actual pre-coin B1 commit and explicitly document the diagnostic timing difference from Core (updating CH2 D17/PLAN acceptance), or keep Core’s post-coin log position by introducing a separate post-Bk diagnostic finalization while leaving `write_b1` before B2…Bk. Do not move Evo rows after the coin batches.

4. **Major — H9’s erased-mined representation cannot identify the rotated inverse key.**
   - **Section:** §4.1.4, `CH7-evo-llmq-chainstate.md:446-453`, and the key layout at `:673`.
   - **Core:** `src/llmq/blockprocessor.cpp:579-590` erases the primary commitment and, for rotated quorums, calls `BuildInversedHeightKeyIndexed(type, pindex->nHeight, qc.quorumIndex)`.
   - **Problem:** H9 records only `erased_mined: Vec<(LlmqType, QuorumHash)>`. That pair is sufficient for the primary `llmq_mined` key, but it does not contain `quorumIndex`, which is part of the `llmq_mined_h` key (and the design’s own key description includes it). If the layer applies the tombstone later without first reading and retaining the old commitment, it can leave a stale inverse-height row. Such a row can affect historical lookup and quorum-root results after a reorg.
   - **Fix:** Make each erase a full tombstone containing the block height, quorum index, and both encoded keys (or specify an atomic read-before-delete of the primary record that recovers those fields, including missing/duplicate-row behavior). Add a test for a rotated commitment disconnect followed by reconnect and historical reads.

5. **Major — G1q’s pre-H4 decode path can hide Core’s malformed-payload rejects.**
   - **Section:** §3.2 step 0 and D3 (`CH7-evo-llmq-chainstate.md:233`, `:257-263`).
   - **Core:** CbTx payload failures are `bad-cbtx-payload` at `src/evo/specialtxman.cpp:899-915`; QC payload failures are `bad-qc-payload` at `src/llmq/blockprocessor.cpp:568-578`.
   - **Problem:** G1q is computed before H4 from payload types and QC nullness “decoded once.” The design does not say what happens when that decode fails. If the guard reports a readiness `SystemError`, a malformed QC/CbTx that Core would reject in H4 never reaches the Core reject string; if it treats the failure as “no quorum needed,” the guard and H4 may disagree about the same bytes.
   - **Fix:** Define G1q as a raw-type-only/tri-state predicate: any payload decode failure bypasses the readiness refusal and proceeds to H4, where the pinned Core reject is returned. Add malformed QC, CbTx, and ProTx cases to the CH7a guard tests; reserve `SystemError` for well-formed blocks that genuinely require CH7b capabilities.

6. **Minor — The disconnect delta lifecycle is not specified.**
   - **Section:** `NodeEvoAttempt` at `CH7-evo-llmq-chainstate.md:126-130` and H9 at `:453`.
   - **Core:** `UndoSpecialTxsInBlock` is a separate operation at `src/evo/specialtxman.cpp:1087-1120`, and disconnect writes the previous best block at `src/validation.cpp:2387-2389`.
   - **Problem:** `delta` is described as `None until H4 builds it`, but H9 never runs H4 and is nevertheless required to commit an EvoDelta containing mined erases and the disconnect `meta.evo_tip`. The design does not define who creates that delta, whether it is empty apart from erases, or how it is merged with the layer on a disconnect.
   - **Fix:** State that `begin_attempt` creates an origin-specific empty disconnect delta (block/height and previous tip), H9 appends only the mined tombstones, H6 records `meta.evo_tip = pprev`, and `commit_tx` pushes that delta on successful disconnect; failed/check-only paths must drop it.

## Checked and correct

- H4’s CbTx, credit-pool, transaction loop, quorum, DML, both roots, ChainLock, MNHF, and V19-BLS ordering follows `src/evo/specialtxman.cpp:899-1074`, including the listed gates and reject strings.
- H9’s BLS rollback, MNHF undo, DML undo, and quorum undo order follows `src/evo/specialtxman.cpp:1094-1110`; the review issue is the erased-key data, not the ordering.
- The decision that mined commitment rows are transactional and `llmq_snapshot` is the remaining effect CF is consistent with DEC-517, DEC-202, and the design’s §3.5/§6.2 classification.
- The `MnList` tip rule, disconnect-only historical behavior, and `EvoReadView` routes generally match Core’s `UpdatedMNListTip` behavior and the approved SB0/EV addendum boundaries.
- The CH7a/CH7b slice dependencies and §9 acceptance rows cover the substantive CH7a, CH7b, and SB19 PLAN obligations, including the staged G1q and the full-testnet gate.
