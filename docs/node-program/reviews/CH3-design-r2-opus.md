# CH3 design review r2 (narrow): Opus

**Reviewed:** `git diff b5ede63b5 c5a271265` on `node/ch3-design`, `docs/node-program/designs/CH3-chainstate.md` (rev1 → rev2). The branch was not edited.

**Against:**
- the Core v24.0.0-rc.3 extract at `/work/scratch/ch3-design/core/src`;
- both r1 reviews and the manager's rev2 rulings (`~/workspace/rdc-wt/integration/docs/node-program/reviews/`);
- PLAN.md and `tools/cpm.py` on the branch, for the §11 deltas.

**Scope:** the brief's three items. This review does not re-open settled r1 points, and it reports only new Blocker or Major problems that the rev2 delta introduced. A short list of sub-Major observations follows the findings; none of them affects the verdict.

## Summary

rev2 applies almost every ruling as ruled, and most of its new Core claims check out against the extract (listed under "Verified"). Two new problems need a fix before the manager rules:

| # | Severity | Problem |
|---|---|---|
| M1 | Major | G1's coinbase predicate (`nVersion ≥ 3 && nType == TRANSACTION_COINBASE`) is narrower than Core's (`vtx[0]->nType == TRANSACTION_COINBASE`, any version). Below DIP3, a coinbase with `nVersion` 1 or 2 and `nType` 5 gets `bad-cbtx-payload` from Core, but G1 lets it through and `NoEvo` accepts it. D32's "with G1 holding, `NoEvo` is exact" is false for that shape, and §7.3 drops it from L3's census. |
| M2 | Major | §11's PLAN CH3a delta adds `i:ND1, i:RP1a, i:P2b` to CH3a's deps. PLAN has `RP1a ← CH3b ← CH3a`, so this creates a cycle. Applied to `tools/cpm.py`, the model dies with `RecursionError`. "For slice a11 only" cannot be expressed in PLAN, which has no slice rows. |

Both fixes are bounded and textual, so the manager can rule them in without another round (fix text below).

---

## Findings

### M1. G1 and D32 miss a coinbase shape Core rejects below DIP3: `nType == 5` with `nVersion < 3` (§2.17, D31, D32, §7.3)

**What rev2 says:**
- G1 refuses a block when "its coinbase is CbTx-typed (`nVersion ≥ 3 && nType == TRANSACTION_COINBASE`)".
- D32/§2.17: "with G1 holding, `NoEvo` is exact".
- §7.3: "A CbTx-typed coinbase below DIP3 is a G1 refusal: it is outside the non-evo census."

**What Core does:**
1. `nType` is parsed from the upper 16 bits of the 32-bit version **whatever `nVersion` is**. The extra payload is read only when `nVersion ≥ 3 && nType != 0` (`primitives/transaction.h:357-363`). So a coinbase serialized with version `0x00050002` has `nVersion = 2`, `nType = 5` and an empty `vExtraPayload`.
2. Nothing earlier rejects it below DIP3:
   - `CheckTransaction` only relaxes `bad-cb-length` for `nType == 5` (`consensus/tx_check.cpp:62-67`).
   - `CheckBlock`'s asset-unlock mutation check finds no payload and passes (`validation.cpp:4351-4359`).
   - `ContextualCheckTransaction` and `ContextualCheckBlock` check types only under DIP0003 (`validation.cpp:356-378`, `:4589-4593`).
3. `ConnectBlock` passes `fScriptChecks` as `fCheckCbTxMerkleRoots` (`validation.cpp:2663`). `ProcessSpecialTxsInBlock` enters its CbTx branch on `fCheckCbTxMerkleRoots && block.vtx[0]->nType == TRANSACTION_COINBASE`, with **no version or DIP3 test** (`evo/specialtxman.cpp:816`).
4. In that branch, `GetTxPayload<CCbTx>` (`evo/specialtx.h:30-34`) decodes the empty payload, the decode throws, and the result is `nullopt`, so Core returns `BLOCK_CONSENSUS`, `bad-cbtx-payload` (`specialtxman.cpp:820-832`). That is a verdict; no evo state is involved.
5. The loop then skips `vtx[0]` on the same predicate, `i == 0 && nType == TRANSACTION_COINBASE` (`:865`).

**rdashd under rev2:**
- G1 does not fire, because `nVersion` is 2.
- `NoEvo`'s ported gate is `CheckSpecialTxInner`, which returns true at once because `HasExtraPayloadField()` is false (`:224-225`). The `vtx[0]` skip gives the same answer under either predicate.
- So H4 returns `Ok(empty)` and the block connects.

**Scenario.** On regtest or devnet below DIP3 (devnet is `GuardScope::Declared`, but G1 applies on every network), a miner publishes a block whose coinbase has version `0x00050002`.
- Every dashd with script checks on rejects it with `bad-cbtx-payload`.
- rdashd connects it, writes its undo and serves it.
- That is a chain split. It is the acceptance-difference class that Sol B1 / ruling 1 requires to fail closed, and it sits in exactly the window that D32 declares exact.
- L3 would not catch it: §7.3 moves "CbTx-typed coinbase below DIP3" out of the non-evo census on the assumption that G1 covers it.

(Mainnet and testnet history are unaffected. A v24 `-assumevalid=0` reindex would reject such a block, so none exists before DIP3. L2 stays green, which is why only a directed test or L3 would show the gap.)

**Fix:**
1. **G1's coinbase predicate becomes Core's own:** `block.vtx[0].nType == TRANSACTION_COINBASE`, any `nVersion`, citing `specialtxman.cpp:816`, `:865` and `validation.cpp:4351`.
   - Alternative: keep G1 at `nVersion ≥ 3` and have `NoEvo` port the payload-free case exactly: with `fScriptChecks` on, `nType == 5 && nVersion < 3` gives `BLOCK_CONSENSUS`, `bad-cbtx-payload`, plus `v:2666`'s log line; with it off, accept. Either way, state the choice in D32.
   - Refusing is simpler. It is fail-closed in the one case where Core accepts (`fScriptChecks` off, so the branch is skipped). Porting is exact; the manager can pick either.
2. **`NoEvo`'s loop skip uses Core's predicate** (`i == 0 && nType == TRANSACTION_COINBASE`), not the doc's "CbTx-typed" definition. The two give the same answer today, but the code should match Core's line.
3. **Tests:**
   - §2.17 "G1 shapes" adds a pre-DIP3 coinbase with `nVersion` 1 and 2, `nType` 5, both with `-assumevalid=0` and with the skip taken.
   - §7.3 keeps that shape in the non-evo census if it is ported, or lists it as a G1 refusal test if it is refused.
4. **D32's sentence** "every other part of the function is … reached only through a special transaction or a CbTx coinbase" changes "CbTx coinbase" to "a coinbase whose `nType` is `TRANSACTION_COINBASE`, any version".

### M2. §11's CH3a dependency delta makes PLAN cyclic (§11 "PLAN CH3a", §7.5, §8.1 a11, D37)

**What rev2 says:**
- §11 PLAN CH3a: "deps add … for slice a11 only **i:ND1, i:RP1a, i:P2b** (RP1a depends on CH3b, so a11 lands last; D37)".
- §7.5: "CH3a's row closes when a11 is green … There is no cycle at slice level."

**PLAN as it stands:**
- RP1a's merge dep is `RP0, CH3b` (`PLAN.md:626`), and CH3b's is `CH3a` (`:571`).
- An `i:` dependency "constrain[s] only the merge" (`:417`).
- PLAN has no slice rows, so "for slice a11 only" has nowhere to live. Applied to the CH3a row, the edges are CH3a →(i) RP1a → CH3b → CH3a.

**Checked:**
- I copied `tools/cpm.py` to `/work/scratch/ch3-design-r2/cpmcheck/` and added `'i:ND1', 'i:RP1a', 'i:P2b'` to its CH3a row.
- The original runs (exit 0). The edited copy fails with `RecursionError: maximum recursion depth exceeded` in `ef()` (exit 1).
- Even read loosely, the delta is ambiguous on the schedule:
  - If "CH3a merged" means a11 merged, CH3b cannot merge before RP1a, which needs CH3b. That is the same cycle.
  - CH4, MN0 and CH7a, which have merge deps on CH3a, would also slide past RP1a.

**Fix (pick one, and state it in §11 and D37):**
- **(a) A separate PLAN row** for a11, e.g. `CH3a-FT`: deps `CH3a, ND1, RP1a, P2b`; acceptance the five FTs. M3 exit 5 depends on it. CH3a's own row keeps its rev1 deps and closes on a1–a10 and a12's own gate.
- **(b) The pattern CH3b already uses:** add no deps to CH3a. State that the five FTs are acceptance that closes after RP1a merges, as CH3b's four FTs already do. CH3b's PLAN row lists `rpc_invalidateblock` and the rest, which need RP1a, and it has no RP1a dep.

Either way, CH3b's merge dep on CH3a means a1–a10, which (a) gives for free.

---

## 1. Closure of the r1 findings and rulings (§12.1)

| Finding | Verdict | Notes |
|---|---|---|
| Sol B1 = Opus M1 (ruling 1) | **PARTIAL** | Closed as ruled: scope (G1 on every network, G2 mainnet/testnet), form, call sites (tip, TBV, ABC, invalidate, recovery, startup), refusal class, boundary and restart tests, and the named removal rows. Missing: G1's predicate does not cover every evo rule live below DIP3 (M1) |
| own (D32) | **PARTIAL** | The `bad-tx-type-dip3-inactive` port is right (`specialtxman.cpp:224-229`, loop `:864-875`, block reject text `:873-874`). The exactness claim fails for the `nType == 5`, `nVersion < 3` coinbase (M1) |
| Sol M1 (ruling 2) | CLOSED | The commit follows a successful `finish_attempt`; emission order and survival on invalid/`just_check` are kept; fault injection before the first effect, between effects and at release, with all four assertions |
| Sol M2 = Opus m9 (ruling 3) | CLOSED | Genesis start; the 45-min rule with a suffix check that claims no MuHash parity and gets its own D-row; counters seeded from golden(start − 1); segment equals full replay at first and last heights (a12). Citations right (`muhash.cpp:314-323`, `muhash.h:120-123`) |
| Sol M3 = Opus m2 (ruling 4) | CLOSED | Only `is_conflict` advances the cursor; the removal acts on the returned lock and consumes nothing; `[A, null]`, `[A, B, null]` and a failed attempt after a removal are tested. `chainhelper.cpp:79-85`, `:93-99` agree |
| Opus m1 | CLOSED | Closure = the lock plus its chained children (`instantsend.cpp:339-350`); the Z test is included |
| Opus M2 (ruling 5) | CLOSED | `ViewTarget` lands in a2; commit follows the target; H1, H6 and H10 take it; `ScratchView` is CH5's with CH2b; §7.2's claim is withdrawn; M3 exit 1 → CH5 is in §11 |
| Opus M3 (ruling 6) | **PARTIAL** | The assignment is exactly the ruling's; slices a11 and b2 are named. The PLAN expression of a11's deps is cyclic (M2) |
| Sol m1 = Opus m4 (ruling 7) | CLOSED | a8 → a3; a7 needs a3, a4, a5 and a8; a10 needs a4, a5 and a6 |
| Sol m2 (ruling 8) | CLOSED | V makes its own `flush_state(ALWAYS)` write and reads the record back before it parks; the durable record is asserted before `kill -9` |
| Sol scope note / Opus n7 | CLOSED | D25 |
| Opus m3 | CLOSED | H3 retired; `GetSpecialTxRules` (`v:6797-6803`) and `GetMnRewardEraAfter` are computed from deployments |
| Opus m5 | CLOSED | `control_inline` is a §11 CS6 delta; `v:2649` agrees |
| Opus m6 | CLOSED | The memo is ported; `v:4374-4375`, `:4424-4425` agree |
| Opus m7 | CLOSED | Probe first at `v:2174`, before `AccessCoin` at `v:2184`; a pair of §11 CS6 deltas |
| Opus m8 | CLOSED | Header preload, skip-count assertion, the decision table, `feature_assumevalid`. `defaultAssumeValid` heights match `chainparams.cpp:231`, `:440` |
| Opus m10 | CLOSED | Hint then stream; the error table. Checked against `UndoReadFromDisk` (`blockstorage.cpp:540-575`) |
| Opus m11 | CLOSED | Open by path, a better fix than my `nlink` proposal (D46's reason is right) |
| Opus m12 | CLOSED | D47; `dsnotificationinterface.cpp:90-96` agrees |
| Opus m13 | CLOSED | Reverse leg and three fuzz targets; `ReverseBlock` at `coinstatsindex.cpp:403` |
| Opus m14 | CLOSED | D49. The regtest budget and superblock parameters match `chainparams.cpp:801-806`, and v20/mn_rr@2000 is required by the asserts at `:964-966` |
| Opus n1–n6 | CLOSED | D18, D50, D51, D14, D52, §7.2 (testnet CSV 8,064 at `chainparams.cpp:392`) |

## 2. The owner's eight declared choices

1. **G1 leaves out DIP0008, V19, DIP0024 and BRR. Accept.** I grepped every use of these four on the validation path; none needs a check `NoEvo` must make below DIP3:
   - **DIP0008:** gates only the CbTx version rules (`evo/cbtx.cpp:35`) and the credit-pool diff (`specialtxman.cpp:1041`), both behind the CbTx branch.
   - **V19:** drives only the BLS scheme (`v:2490-2491`, `specialtxman.cpp:983`; H1's) and ProTx versions.
   - **DIP0024:** LLMQ type enablement only (`v:6672-6673`).
   - **BRR:** used only in the MN payment calculation (`masternode/payments.cpp:68`), which sits behind `DeploymentDIP0003Enforced` (`:230`).

   All four are active from height 1 on regtest (`chainparams.cpp:822-827`), so including them would refuse every regtest chain. The one gap in G1's set is M1's predicate, not these four.
2. **D32: `NoEvo` ports `bad-tx-type-dip3-inactive`. Accept**, with M1's fix. The gate, its position (step 15, before the UTXO loop), its exemption and the block-level reject text are Core's.
3. **A G2 refusal is an `AbortNode`; R10. Accept the `AbortNode`, but R10 must be corrected before the manager rules on it.** Say so in R10, or at minimum say that the stop is sticky.
   - **R10 understates the outage.** "Stops about monthly" with "a restart after SP1/GV1b land connects it" reads as one stop a month.
   - **A restart does not resume.** The refused block stays `HAVE_DATA`. At restart the tip is fresh, so IBD latches false at once (`validation.cpp:1970-1992`: no tip-age or chainwork reason) and the first ABC re-refuses before any ChainLock at ≥ h can be learnt. With LK1a, the CL for h arrives seconds after the block, but the node is already down.
   - **So each stop lasts until one of these:**
     - the tip is older than `-maxtipage` (default 24 h), when a restart re-enters IBD, passes G2 under Core's own not-synced regime and catches up;
     - SP1 and GV1b land.
   - An operator who sets a small `-maxtipage` keeps the node in that IBD regime permanently. That is Core-equivalent, so it is not a hole, but D31 should name it as the escape hatch.
   - **Option for the manager, not required:** Core's own structure allows a non-aborting refusal. `ActivateBestChainStep` returns false on a non-invalid `ConnectTip` failure without shutting down (`v:3656-3661`), and shutdown comes only from `AbortNode`. A G2 refusal that leaves the tip at h − 1 and retries on the CL (CH1-node's `EnforceBlock` → ABC path) would turn the monthly ~24 h outage into a seconds-long pause once LK1a is present. That needs a non-stopping outcome in CH1-node. I accept rev2's choice as ruled; this is a product trade-off for the manager.
4. **ND1 has no blanket mainnet refusal. Accept.** The G1 startup check and G1/G2 at every connect site cover the cases. A blanket refusal would block M3's pre-DIP3 IBD.
5. **Undo reads: fast path with fallback to the stream decode. Accept.** I checked the equivalence against `UndoReadFromDisk`:
   - Core hashes `prev ‖ decoded bytes` and reads the checksum right after the decoded bytes, never the size field.
   - Fast-path accept, fast-path mismatch, and a non-EOF decode error inside `size` are all final with Core's outcome and `what()`.
   - Every other case takes the stream path.
6. **Commit after effects, with the undo write before the effects. Accept.** Two checks:
   - On an undo-write `AbortNode` (step 22), `finish_attempt` still applies the step-18 removals, which Core had already executed. The end state is the same.
   - The one appliers' read I could check agrees. Core's `RemoveConflictingLock` reads `GetTipHeight()` (`instantsend.cpp:343`). Under D33 effects run before the tip moves, so both see `prev`'s height.
7. **CH3a's a11 closes last. Reject the PLAN form; accept the sequencing.** The intent (the five FTs after RP1a, a11 last) is right. The §11 dependency text is cyclic (M2); use M2's (a) or (b).
8. **`feature_llmq_is_cl_conflicts` gates CH7b. Accept.** It needs real locks, and those are evo (my r1 comparison note).

## 3. Consensus-order claims verified against the extract

- Step 5a (G1) sits after `CheckBlock` and the CL-conflict check and before H2's DIP0003 evo-best-block check (`v:2505-2531`). A block that `CheckBlock` rejects still gets its verdict.
- G2's predicate is exactly the synced-Core difference set:
  - `IsSuperblockValidationRequired` = `best_cl < h && mn_sync.IsSynced()` (`chainhelper.cpp:41-52`);
  - at non-superblock heights `IsBlockValueValid` returns before `check_superblock` matters (`payments.cpp:339-346`), and `IsSuperblockTriggered` is false (`superblock.cpp:578-583`);
  - old-budget heights use `check_superblock` (`payments.cpp:279-287`), so including them is right;
  - `gov_valid` (`m_superblocks.IsValid()`) matters only at superblock heights (`:357-361`, `:406-410`).
- G2 sits at step 19, after the IS loop (`v:2801-2819`), and before `check_superblock` is computed (`v:2841-2843`). Effects emitted before it are ones Core would also have executed.
- A refusal never punishes a peer: `BlockChecked` scores only `IsInvalid()` (`net_processing.cpp:2323-2326`).
- `bad-tx-type-dip3-inactive` comes before the type switch (`specialtxman.cpp:224-229`). `ContextualCheckTransaction` checks types only under DIP0003 (`v:356-378`).
- `CQuorumBlockProcessor::ProcessBlock` returns before DIP0003 (`llmq/blockprocessor.cpp:218-221`). Its pre-DIP3 write is an evo-DB marker, not a check.
- MNHF finds no signals without MNHF transactions (`evo/mnhftx.cpp:213-225`).
- The MN list is gated at `specialtxman.cpp:906` and the credit pool at `:847`.
- VerifyDB uses one `CCoinsViewCache` over the tip and a rolled-back evo transaction (`v:5046-5055`). Levels 3 and 4 go into that view (`v:5112`, `:5160`). This supports D36.
- `CheckInputScripts` probes the exec cache before any `AccessCoin` (`v:2174` vs `v:2184`). `fChecked` is set only with both checks on (`v:4424-4425`).

## Sub-Major observations (not counted toward the verdict)

- **The `ScratchView` sketch** (§2.1): `begin(&mut self) -> BlockOverlay<'_>` and `evo(&mut self)` cannot both be live, yet H4 needs the overlay and the evo transaction together. a2 should land a split-borrow shape, e.g. `fn split(&mut self) -> (BlockOverlay<'_>, &mut dyn EvoTxn)`, because a2 freezes the trait.
- **"A capable build connects it later"** (§2.17, R10) needs a CH7a note for G1. Core requires the evo best block to equal `prev` at DIP3 (`v:2527-2529`), and an empty EvoDB fails `VerifyBestBlock` (`evo/evodb.cpp:120-126`). `NoEvo` writes no evo state (H6 is a no-op), and Core also writes pre-DIP3 evo records (`blockprocessor.cpp:219`, the MNHF signal cache). CH7a must say how it opens a `NoEvo`-built datadir: initialize from B1, or require `-reindex-chainstate`. Its "turned around" G1 test should use such a datadir.
- **§2.10.2's open-failure row:** Core first logs `Unable to open file <path>` (`flatfile.cpp:45`) and then "OpenUndoFile failed". The I/O class also has "AutoFile::read: fread failed" (`streams.cpp:27`).
- **The G1 startup check** should not refuse under `-reindex` or `-reindex-chainstate`, which discard the tip (CH5's handling).
- **a3's memo test** should pass `checked = true` only for blocks that pass `CheckBlock` with both checks on. For a `CheckBlock`-invalid case the memo would skip the check that the directed test expects to fail.

---

**Counts:** Blocker 0, Major 2 (M1, M2). Closure: 2 PARTIAL rows map to M1, 1 PARTIAL row to M2, every other row CLOSED. Choices: 7 accepted (choice 3 conditional on the R10 correction), 1 rejected in its PLAN form (choice 7).

Neither Major reopens a settled r1 point, and both fixes are textual. The manager can rule them in (M1: G1 predicate, D32 wording, two tests; M2: separate FT row or acceptance-only wording) with no further review round.

VERDICT: CHANGES-REQUIRED
