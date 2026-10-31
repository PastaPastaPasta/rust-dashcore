APPROVE-WITH-CHANGES

## Findings

### Major

1. **H4's public signature still contradicts the new readiness error channel.**
   - **Design:** §3.2, lines 238–241; compare D2 at lines 272–278 and D5 at lines 307–311, plus the CH3 erratum at line 1038.
   - **Core:** `src/evo/specialtxman.cpp:979-982` is the H4 quorum-processing boundary whose failure must be propagated; the surrounding H4 operation is `:881-1085`.
   - **Problem:** The H4 pseudocode still declares `Result<Result<MnListUpdates, BlockValidationError>, Fatal>`, while rev3 says `NoMembers` must become `SpecialTxFailure::NotReady` and ordinary rejects must become `SpecialTxFailure::Invalid(BlockValidationError)`. As written, the signature cannot express the documented `NotBuilt → NotReady → refuse` path, and an implementer can follow the stale type instead of the erratum.
   - **Fix:** Change the H4 signature to `Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>`, define the enum at the CH3 seam, and update the `EvoHooks` trait, connector mapping, and all H4 callers/tests to use the two variants. Keep `startup_refusal(&self, block, at) -> Option<String>` as the separate G1q startup API.

### Minor

1. **The deep-window PF0 assertion needs an aggregation rule.**
   - **Design:** §7.1, lines 802–806, especially the `connect-evo-deep-window` row at line 803.
   - **Core:** `src/validation.cpp:3883-3928` supplies the repeated block-connect loop being measured.
   - **Problem:** “the per-block instruction count over the last 1,000 blocks must be within 5% of the first 1,000's” does not say whether the comparison is total instructions divided by blocks, a median, a mean, or every block. Different interpretations produce materially different pass/fail results, especially with cache and branch variation.
   - **Fix:** Retain the 5% tolerance as a reasonable bounded-growth smoke criterion, but specify the statistic (for example, total `instructions:u` per block for each 1,000-block window, with a stated repetition/selection rule) and state that this is an asymptotic-growth guard rather than the normal Core-parity PF0 gate.

## Checked and correct

- All six r1 findings and the r2 dispositions are substantively resolved: H7 is at Core's in-attempt position; B1/D7 and D20 agree; rotated tombstones carry both keys; G1q is decode-first and assumevalid-aware; the pending transaction hand-off is explicit; and `NoMembers` has a real `NotReady` channel.
- D3's per-slice table matches §8/§9: CH7a-2 runs member-free QC/root/ChainLock checks, CH7b-2 leaves only the valid-signature CbTx ChainLock readiness arm, and CH7b-3 removes G1q.
- D16/D22 are consistent with the B1 protocol and D7: merged indexes provide bounded point/range reads and O(1) pinned views; `meta.evo_tip` is in B1 with evo rows and recovery replays coins only after a B1-only crash.
- (a) The fail-closed valid-signature CbTx ChainLock refusal is visible to implementers in D3 (lines 296–303) and to testers in §7.2 (line 819), including the three Core `bad-cbtx-*` rejects that cannot run until CH7b-3.
- (b) The 5% tolerance is acceptable for the stated deep-window growth guard; the minor finding only asks for a deterministic aggregation definition.
- (c) The `has_mempool()` gate matches Core rc.4's `MaybeUpdateMempoolForReorg` (`src/validation.cpp:422-435`): V moves `dmn_tip` before each add=true drain only when a real mempool exists, while `NullMempool` preserves the no-mempool behavior.
- The CH3 §3.1 errata explicitly provide `startup_refusal`; the remaining defect is the stale H4 signature shown earlier in the same design.
