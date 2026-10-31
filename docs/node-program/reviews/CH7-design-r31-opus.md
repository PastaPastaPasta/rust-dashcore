APPROVE-WITH-CHANGES

# CH7 design rev3.1 (642943a69): Opus-h r3.1 narrow confirmation

**Counts:** 0 Blocker, 0 Major, 3 Minor, 4 Nit.

**Scope.** I read the rev3→rev3.1 diff (2834a632b..642943a69), §10.2's r3 rows, DEC-527, and both r3 reviews. I checked the five focus items against Dash Core at 375c64d2317 and rust-dashcore at 497d3ca11. Keys follow the design's §0.1. Extra keys: `bs:` is `src/node/blockstorage.cpp`, `i:` is `src/init.cpp`, `c:` is `src/llmq/commitment.cpp`, `dsn:` is `src/dsnotificationinterface.cpp`.

**Result.**
- All 13 r3 findings are resolved in substance.
- Both of the designer's deviations are sound.
- §8's "accepts nothing Core rejects" column holds at every slice boundary, including CH7b-2 → CH7b-3.
- The three Minors:
  - The new §6.3 step 6 sits at the wrong point of Core's init order (m1).
  - Arm (a)'s declared preempted set misses `bad-qc-invalid` (m2).
  - The erratum's `startup_refusal(&self, …)` can't be called where the startup check runs (m3).
- None of them lets the node accept a block Core rejects.

---

## Minor

### m1. §6.3 step 6: Core calls `InitializeCurrentBlockTip` after init's `ActivateBestChain` and the mempool load, not straight after VerifyDB

- **Where:** §6.3 step 6; D16 ("At startup `dmn_tip` is `None` until the startup `InitializeCurrentBlockTip` command"); §11.2's ND1 bullet ("after `VerifyLoaded`, as Core's `init.cpp:2493`/`:2497` follow `:2130`").
- **Core:**
  - `i:2130` VerifyDB runs first.
  - `i:2228` registers `CDSNotificationInterface`.
  - `i:2479` starts the `loadblk` thread, which first runs `ThreadImport` to completion (`i:2482`). `ThreadImport` does the reindex and `-loadblock` imports and `LoadGenesisBlock` (`bs:941`). It then runs **`ActivateBestChain`** for each chainstate (`bs:966`), leaves `CImportingNow` (`bs:977`) and runs **`LoadMempool`** (`bs:978`).
  - Only then does init read the tip and `ibd` (`i:2485-2486`) and call `InitializeCurrentBlockTip` (`i:2493` or `i:2497` → `dsn:31-35`).
  - On a masternode, `i:2493` kicks only `CDSNotificationInterface`. The broadcast to every subscriber waits for `nodeman->Init` (`i:2569-2578`).
- **Problem:**
  1. The node's `VerifyLoaded` runs "before init's ABC" (`rs:chain/command.rs:194-196`). "After `VerifyLoaded`" therefore allows ND1 to send the command before init's ABC.
  2. In that case, during the first iteration of init's ABC (stored but unconnected blocks above the loaded tip, or `-loadblock`), the node's `dmn_tip` is the loaded tip. Core's `tipIndex` is still null there, so `GetListAtChainTip()` is empty (`dmn:944-951`), until that iteration's signal.
  3. Readers that diverge in that window:
     - the superblock arms (`pay:367`, `:424`). In practice the `sb_required` gate (MN sync unfinished) masks this, so no verdict differs. But RPC is up while `loadblk` runs, so the gate is not a guarantee on regtest.
     - once MP1b lands, `LoadMempool`'s `addUncheckedProTx` (`txm:647-699`), which Core runs before `InitializeCurrentBlockTip`.
  4. The `tip` and `ibd` the command carries must be the post-ABC values (`i:2485-2486`).
  5. D16's restart test ("a restart with no new block") can't tell the two orders apart.
- **Fix:**
  - §6.3 step 6 and the ND1 erratum: ND1 sends `InitializeCurrentBlockTip { ibd }` after init's ABC (Core's `ThreadImport`, `bs:966`) and, once MP1 exists, after the mempool load (`bs:978`), with the tip and `ibd` read at that point. Cite `i:2482-2497`.
  - Add a D16 test: restart with blocks stored above the loaded tip. The first ABC iteration's superblock-arm list argument is empty. After the command, `ChainView.mn_list` is the post-ABC tip's list.
  - Note the masternode split (`i:2493` vs `i:2578`) as AM1's obligation, since step 6 dispatches to every synchronous listener at once.

### m2. D3 arm (a): the declared preempted set omits Core's member-free `Verify` checks (`bad-qc-invalid`) and the quorum tx's own shared-collateral check

- **Where:** D3, "Declared interim difference", the **(a)** bullet ("`commitment.cpp:240-300`; the members `Verify` is at `:301`") and the "for every arm" bullet ("the step-4 rejects of every transaction ahead of the first quorum-dependent one"); §7.2's interim-difference row.
- **Core:**
  - `CheckLLMQCommitment` calls `Verify(…, false)` at `c:301`. The members call inside `Verify` is at `c:177`.
  - Before that call, `Verify` returns false, which becomes `bad-qc-invalid` (`c:304`), for any of: a parentless base (`c:125-128`), the wrong `nVersion` (`c:130-137`), the wrong `quorumIndex` (`c:144-147`), bad sizes (`c:149-151`), too few valid members or signers (`c:153-160`), an invalid `quorumPublicKey` (`c:161-164`), a null `quorumVvecHash` (`c:165-168`), or an invalid `membersSig` or `quorumSig` (`c:169-176`). None of these needs members.
  - Under `rules.v24`, step 4 also runs `CheckSharedCollateralTemplateOutputs` on the quorum-dependent transaction itself before its `CheckSpecialTxInner` (`stm:940-946`). An asset unlock carries outputs, and `al:196-199` notes that this generic rule is what rejects one that pays the template.
- **Problem:** These rejects are preempted by arms (a) and (b) but not declared. This is the gap r3 m2 asked to close: O2b `diff_chain` vectors and CH7a-5's L2 would show a refusal that no text accounts for, for example a non-null QC with a wrong `nVersion` or fewer than `minSize` signers.
- **Fix:**
  - (a): add `bad-qc-invalid` from `Verify`'s member-free checks (`c:125-176`), and put the members call at `c:177`, inside the `c:301` call.
  - "For every arm": "the step-4 rejects of every transaction ahead of the first quorum-dependent one, and that transaction's own shared-collateral check (`stm:940-946`)".
  - Add one §7.2 row: a non-null QC with `nVersion` ≠ expected gives the readiness refusal through CH7b-1 and `bad-qc-invalid` from CH7b-2.

### m3. `startup_refusal(&self, …)` has no receiver at its call site

- **Where:** D3 "Startup"; §11.2 CH3 §3.1 erratum; §10.2 r3 m3 row.
- **Code:**
  - `check_startup_readiness<H: EvoHooks>(params, chain, files, blocks)` (`rs:chain/load.rs:376-381`) is generic over a type, not a value. Its one readiness input today is the associated const `H::COMPLETE` (`:382`).
  - The node calls it as `check_startup_readiness::<NodeEvoHooks>(…)` (`dashd-node/src/steps.rs:263`), before V starts (`start_chain`, `steps.rs:277`), so no `NodeEvo` instance exists yet.
- **Problem:** As written, the erratum's provided method `startup_refusal(&self, block, at, deployments)` either can't be called there, or forces ND1 to build `NodeEvo` (store, V handles) before V starts. That is a startup restructuring the design doesn't state.
- **Fix:**
  - Make it an associated function: `fn startup_refusal(block: &Block, at: &BlockAt, deployments: &dyn DeploymentView) -> Option<&'static str>` (default `None`). The design already says arms (a)–(c) are pure functions of the block, its parent and the deployments.
  - `check_startup_readiness` calls `H::startup_refusal(...)` when `H::COMPLETE`.
  - The deployments it passes come from `ChainDeployments::new(...).view()` (`load.rs:386-387`), whose `NoEhfSignals` is fine for arm (c). `IsQuorumTypeEnabled` reads only DIP0024, DIP0020, TESTDUMMY and heights, and no EHF deployment (`validation.cpp`, `ChainstateManager::IsQuorumTypeEnabled`). `IsQuorumRotationEnabled` reads DIP0024 (`llmq/options.cpp:50-62`).

## Nit

**n1. The H4 erratum names the wrong trait at the step layer.**
- The connector's inner type is declared on `AttemptSteps::process_special_txs` (`rs:connect/serial.rs:98`, `:114-123`). Its implementations are `HookSteps` (`rs:connect/connector.rs:610`) and `NoEvoSteps` (`rs:connect/bench.rs:51`).
- `bench.rs:64` is that `AttemptSteps` implementation, not an `EvoHooks` one.
- Name the trait declaration in §11.2 and D2's "Implementors and callers", and list `bench.rs:51/64` as an `AttemptSteps` implementation. The `EvoHooks` implementations are `NoEvo` (`evo.rs:336`), `Ch7aShape` (`tests/attempt.rs:111`) and `TestEvo` (`tests/connector.rs:268`).

**n2. "The stub helper's `VerifyChainLock`" has no named home.**
- D2 relies on the verify refusal surviving CH7b-2's facade swap. So the stub must not be served by the members or quorum facade. §4.5.2 lists no verify seam.
- Name it, for example a `ClVerifier` that step 10 receives, `NotBuiltClVerifier` until CH7b-3, returning `StoreError::NotBuilt("ChainLock verification (CH7b)")`.
- Its capability string should differ from G1q's "quorum validation (CH7b)", so that refusals in L2 logs say which gate fired.

**n3. Superseded rows in §10.2.** The r2 rows "Opus M3 delta-layer reads" (`connect-evo-deep-window`), "Sol Major assumevalid" (arm (d) requires `check_cbtx_merkle_roots`) and "Opus m5 PF0 rows" (four rows) now contradict D3 and §7.1. Mark each "superseded by DEC-527 (r3 m2/m5 rows)".

**n4. VerifyDB level 4 and the verify stub.**
- D3 says "the startup check runs no H4". That's true, but VerifyDB at `-checklevel=4` reconnects through H4 with Core's `fScriptChecks` (`v:5179-5327`).
- Example: a CH7a-4…CH7b-2 datadir whose last blocks carry a valid CbTx ChainLock connected under `-assumevalid`, reopened with `-checklevel=4 -assumevalid=0`. It reaches the stub inside VerifyDB.
- State that `NotReady` under origin `VerifyDb` is a VerifyDB failure carrying the refusal text, with no verdict and no failure mark. Add it to D2's test.

---

## Checked and correct

### 1. The 13 r3 findings

- **Sol Major (H4's signature).**
  - §3.2, D2's seam block, D2's D-row and §11.2 all carry `Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>`.
  - The enum is defined beside `EvoHooks`.
  - The call site (`serial.rs:816-829`) keeps `Invalid`'s log line and `ConnectOutcome::Invalid`, and maps `NotReady` to `refuse(refusal_text(..), just_check)` (`serial.rs:484-491`), as G1 does at `:688`.
  - §4.1.2's LQ2 arm returns `Ok(Err(NotReady(e.capability())))`.
  - The step-layer naming is n1.
- **Sol Minor and Opus m5 (deep window).**
  - The statistic is fixed: total `instructions:u` over the window ÷ 1,000, median of 3 fresh runs, deep ≤ 1.05 × shallow. The blocks are identical in both configurations, and a no-flush assertion is included.
  - The guard is declared asymptotic, not PF0 parity.
  - `evo-index-depth` exercises the populated `mined` map with tombstones, and the CH7b-3 run covers the tombstoned `llmq_mined_h` range read (`bp:731-773`).
  - The PF0 counts (3, then 5) agree in §7.1, Q12 and §8.
- **m1 (startup `dmn_tip`).**
  - §6.3 step 4 now publishes `dmn_tip = None` with the empty list.
  - D16 cites `signals.rs:521-522` and `:525-526`, which I verified at the base. Both dispatchers exist, and nothing calls `initialize_current_block_tip`.
  - The restart test is added.
  - The ordering residue is m1 above.
- **m2 (preemption).**
  - Arm (d) is gone. The step 10 order now matches `stm:156-239`. With `cl_best` none and no `CbTxClCell`, neither the `:176` nor the `:211` shortcut can fire. Both shortcuts only ever return true, and on any input that reaches them the node's extra checks pass: `:176` requires `bestCLHeightDiff == 0` and a valid signature, so `:195-201` and `:207` pass. So the node runs `:185-224` as Core does and refuses only at `:226`.
  - The listed (b) strings and positions are right: `mnhf:97-135`, `GetQuorum` at `:146`; `al:185-219`, `ScanQuorums` at `al:123`.
  - So is (c): the precompute is `bp:234`, step 6's first call.
  - The arm (a) and shared-collateral gap is m2 above.
- **m3 (`startup_refusal` inputs).** The deployments are passed, the return is `Option<&'static str>`, the `load.rs:382-384` early return becomes the call, and only arms (a)–(c) are evaluated. The receiver problem is m3 above.
- **m4 (tip-list readers).**
  - `reorg_drain` gives one writer and publishes inside the step.
  - The `tip_list` argument covers `addUncheckedProTx` (`txm:647-699`) and `removeForBlock` (`txm:1321-1344` → `:1069`, `:1250`), the latter reached from `ConnectTip` (`v:3572`) while `tipIndex` lags.
  - The MP1a, MP1b and MN1 rows in §5.4 match `txm:1139` and `pay:451-455`.
  - The disconnect-only publication case (`v:3926-3937`) is closed.
- **n1–n6.**
  - D16 now states ABC's own condition (`v:3904`, `v:3926-3937`), including the iteration that reconnects to its start.
  - `reorg_drain`'s `add` argument follows `failure.rs:51` (`*disconnected <= 10 && ret`, `v:4081`).
  - `bad-qc-dup` is limited to non-rotation types (`bp:270-276`, `:632-637`), and the rotation excess case is `bad-qc-not-allowed` (`bp:254-256`).
  - The `packet.rs:21` erratum and the `control.rs:36` freeze check are cited.
  - CH7b-2 names the chain-aware `ScanQuorums` and `GetQuorum` (`al:165-174`).
  - H2 reads the scratch `EvoTxn`'s `evo_tip` first under VerifyDB (`v:5197`).

### 2. The two deviations

- **`NotReady(&'static str)` carries the capability.** This is the shape of CH3's existing refusals (`load.rs:411`, `refusal_text(params, &at, …)`), and it lets the startup check, the connector and `NotBuilt` share one formatter. It is sound.
- **`check_cbtx_merkle_roots` is dropped from `g1q`.** Arms (a)–(c) never read Core's flag:
  - `CheckLLMQCommitment` always calls `Verify(…, /*checkSigs=*/false)` (`c:301`), which calls `GetAllQuorumMembers` at `c:177` whatever the flag.
  - The precompute is unconditional (`bp:234`).
  - `CheckSpecialTxInner` passes `check_sigs` only to the ProTx checks. The MNHF and asset-unlock branches take no flag (`stm:283-290`), so `GetQuorum` (`mnhf:146`) and `ScanQuorums` (`al:123`) run under `-assumevalid`.
  - The flag gates only steps 1, 2, 9 and 10 (`stm:899-923`, `:1008-1053`) and the QC BLS batch (`bp:278-295`). The batch sits after step 4's members call, so it changes no arm.
  - Sol's r2 assumevalid finding concerned step 10. H4 now handles that at Core's own position, with the flag H4 already receives (`serial.rs:823`). Nothing is lost, and the startup check needs no assumevalid input.

### 3. §8 at every slice boundary

- **CH7a-1…3:** G1 refuses every evo-active block, unchanged.
- **CH7a-4, CH7a-5, CH7b-1:**
  - Arms (a)–(c) refuse at H4's entry, never accepting. The verify stub refuses at `stm:226`.
  - Every other H4 step is Core's member-free code: steps 6 and 9 over `NoMembers`, and step 10's `:185-224` and `:233-235`.
  - A malformed payload bypasses G1q to Core's reject.
- **CH7b-2:** G1q is removed together with the arrival of the production facade, the mined rows, the quorum object and route-C `ScanQuorums` and `GetQuorum`. Every member and quorum call then gets Core's answer. The only remaining refusal is the verify, independent of the facade (naming: n2).
- **CH7b-2 → CH7b-3:**
  - At CH7b-2, every block whose CbTx carries a valid `bestCLSignature` and that passes `:195-224` reaches `:226` and is refused. Core either rejects it there (`bad-cbtx-invalid-clsig`, `:228`) or continues to step 11, so a refusal never accepts what Core rejects.
  - Core's `:176` and `:211` shortcuts, which skip the verify, need a best ChainLock that the node's `cl_best` (CH7b-3) cannot report yet. So the node never skips the verify where Core would verify.
  - Under `-assumevalid`, both skip step 10.
  - CH7b-3 adds the verify together with `cl_best` and `CbTxClCell`, so Core's shortcuts and its cache writes (`:181`, `:215`, `:231`) arrive with the verify they bypass.
- **CH7b-4 and CH7b-5:** these are read-only, or add an effect target no verdict reads.

### 4. Step 6 against init, VerifyDB and H7

- The command runs after VerifyDB, which is right (`i:2130` before `i:2493`/`:2497`). So startup VerifyDB reads the empty list, as Core does, and moves no `dmn_tip`. VerifyDB has no `UpdatedBlockTip` or drain.
- The synchronous listeners are registered after VerifyDB and before the command, matching `i:2228` against `i:2479`.
- The fork-`None` guard means a tip always sets (`dsn:47-53`), and a null tip (fresh datadir) sets nothing, as `SynchronousUpdatedBlockTip(nullptr, nullptr)` returns early.
- The one defect is the position relative to init's ABC and the mempool load (m1).

### 5. Consistency of D2, D3, D5, D7, D16 and D22 with §11.2

- **D2, D3 and D5** agree on the two refusal sources: G1q arms (a)–(c) until CH7b-2, and the verify stub until CH7b-3. Both map to `NotReady`, mapped before the fault class.
- **D3's table, §0.2 items 7–8, Q3, §8 and §8.3** all say G1q goes at CH7b-2 and the verify lifts at CH7b-3. The PLAN content erratum matches §8.2.
- **D7's VerifyDB read order** agrees with D22: `EvoTxn` is a scratch index with the same structure, and `best_block` is the active index's `evo_tip`.
- **D16's setters** agree among themselves: the signal under `new != fork`, the startup command, and `reorg_drain` with `add` and `has_mempool`. They also agree with §3.6, §5.1's `ChainView` doc, §5.3, §5.4, §6.3 step 4 and the CH3 seams, MP1b and ND1 errata.
- No new contradictions, apart from the stale historical rows (n3) and the erratum details in m3 and n1.
