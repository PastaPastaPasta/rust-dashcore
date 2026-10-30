APPROVE-WITH-CHANGES

# CH7 design rev3 (2834a632b): Opus-h r3 narrow confirmation

**Counts:** 0 Blocker, 0 Major, 5 Minor, 6 Nit.

**Scope.** I read the rev2→rev3 diff (6a3fab586..2834a632b) and the resulting design. I checked the six focus items against Dash Core at the pin, 375c64d2317, and against rust-dashcore at the design's base, 497d3ca11. Keys are as in the design's §0.1, plus `txm:` for `src/txmempool.cpp` and `dsn:` for `src/dsnotificationinterface.cpp`.

**Result.**
- My r2 M1 and M3 are resolved in substance.
- M2 is resolved for every origin except startup, where two passages still disagree (m1).
- The CE4-17 adoption in D16 matches the pin. It leaves MP1b without a way to read the list it is told to read (m4).
- The CH3 errata are minimal and consistent. `startup_refusal` lacks the inputs G1q needs (m3).
- The interim CbTx-ChainLock divergence is documented too narrowly and is untested (m2).
- No slice boundary accepts a block Core rejects.

---

## Minor

### m1. Startup: §6.3 step 4 contradicts D16, and the node's `InitializeCurrentBlockTip` dispatch doesn't set `dmn_tip` (M2 residue)

- **Where:** §6.3 step 4; D16 (the "signal" bullet and the "At startup" sentence); §5.1.
- **Core:**
  - `GetListAtChainTip()` returns the empty list while `tipIndex` is null (`dmn:944-951`).
  - At startup, `tipIndex` is first set by `CDSNotificationInterface::InitializeCurrentBlockTip`, which calls `SynchronousUpdatedBlockTip(tip, nullptr, ibd)` (`dsn:31-35`). Because the fork is `nullptr`, `pindexNew != pindexFork` always holds and `UpdatedBlockTip` runs (`dsn:47-53`, `dmn:822-827`).
  - Init reaches this on both branches: `init.cpp:2493` (masternode) and `:2497` (`GetMainSignals().InitializeCurrentBlockTip`). Both run after `VerifyLoadedChainstate` (`init.cpp:2130`).
  - Nothing else sets `tipIndex` at startup. The only other `UpdatedMNListTip` caller is `v:434`.
- **Problem:**
  1. §6.3 step 4 publishes the startup `ChainView` with "`mn_list` from `resolve_list(tip)`" before step 5 (VerifyDB). D16 and §3.6 say `dmn_tip` is `None` and `mn_list` is empty until after VerifyDB, which is what lets startup VerifyDB's superblock arms see Core's empty list. The two passages give opposite answers.
  2. D16 ties `dmn_tip` to "where it already fires `synchronous_updated_block_tip` (`rs:chain/signals.rs:65`)". In the node, `initialize_current_block_tip` is a separate listener method with its own dispatch (`rs:chain/signals.rs:69-71`, `:525-526`). It does not go through `sync_updated_block_tip` (`:521-522`, whose only caller at the base is `rs:chain/abc.rs:324`). Read literally, `dmn_tip` stays `None` after a restart until the first ABC iteration that signals. That can be minutes on mainnet and indefinitely on an idle regtest.
  3. Meanwhile every `GetListAtChainTip` consumer reads an empty list where Core has the tip's list: GV1b's vote and trigger checks, AM1's own-entry lookup, and the superblock arms once `sb_required` can be true. With GV1b in, a regtest superblock mined right after a restart would fail `IsSuperblockTriggered(empty, h)`. The node would then reject a superblock Core accepts (`pay:367-371`).
  4. `signals.rs:65` is the listener's default no-op, not the firing site.
- **Fix:**
  - §6.3 step 4 publishes `dmn_tip = None` and the empty `mn_list` (with `bls` and `credit_pool` as stated).
  - D16 lists the node's `initialize_current_block_tip` dispatch (`signals.rs:525`) as a `dmn_tip` setter, with Core's guard (`new != fork`, where fork is `None`, so it sets whenever there is a tip). Like the signal, it updates `dmn_tip` before the listeners run.
  - Cite `Signals::sync_updated_block_tip` (`signals.rs:521-522`, called at `abc.rs:324` and at CH3b's invalidate and conflict sites) rather than `:65`.
  - Test: restart with no new block. Startup VerifyDB sees the empty list, and after init `ChainView.mn_list` equals the tip's list.

### m2. G1q pre-empts more of Core's member-free rejects than D3 says, and the interim difference has no test

- **Where:** D3 arm (d) ("also refuses before the member-free rejects of a valid signature (`bad-cbtx-cldiff`, `bad-cbtx-older-clsig`, `bad-cbtx-cldiff-ancestor`)"), arms (a)–(c), §7.2's D3 tests, and the §8 "accepts nothing" column.
- **Core order:** G1q runs at H4's entry, so it refuses before anything that Core reaches ahead of the quorum-dependent call:
  - **Arm (d):** Core's `VerifyChainLock` is the last call of step 10 (`stm:226`). Everything before it comes first in Core:
    - steps 1–9: the CbTx, special-tx and ProTx rejects, `bad-qc-*`, the DML, and `bad-cbtx-mnmerkleroot` and `bad-cbtx-quorummerkleroot` (`stm:899-1053`);
    - step 10's member-free arms: `bad-cbtx-older-clsig` (`stm:199-201`), the range check `bad-cbtx-cldiff` (`stm:207-209`), and `bad-cbtx-cldiff-ancestor` (`stm:218-222`).
  - **Arm (b):** the MNHF signal's payload checks come before `GetQuorum`: `bad-mnhf-version`, `-nbit-out-of-bounds`, `-type`, `-payload`, `-non-ehf` and `-quorum-hash` (`mnhf:97-135`, the call at `:148`). The same holds for the asset unlock's member-free checks and the rejects of every earlier transaction in step 4's loop.
  - **Arm (a):** step 1 and the step-4 rejects of transactions before the QC run before `CheckLLMQCommitment` reaches `Verify` (`commitment.cpp:301`).
  - **Arm (c):** the precompute runs inside step 6 (`bp:234`), so all of steps 1–5 come first.
- **Problem:**
  - This doesn't breach DEC-522 (1): a refusal is never an acceptance.
  - But on the tip path a refusal is `refuse` → `abort_node` (`rs:connect/serial.rs:484-491`; CH3 §2.17). Where Core marks the block invalid and continues, the node stops.
  - D3 names three strings for (d) and nothing for (a)–(c), so O2b `diff_chain` vectors and L2 runs that carry such blocks will show differences no text accounts for.
  - §7.2 has no row that pins this behaviour. It also has no row asserting Core's reject once the arm is lifted ("turned around" is mentioned only generically).
- **Fix:**
  1. **Arm (d): drop it from G1q.** Step 10's verify is already D2's refusal until CH7b-3 (D2's last bullet; the CH7b-2 row says "D2's channel backs"). With the arm gone:
     - the pre-empted set for the CbTx ChainLock is empty, and only the verify's own verdict is refused;
     - steps 1–9 run in Core's order, and their side effects happen as in Core. For example, step 7's `ResetPlatformBan` (`dmn:737-752`), which Core performs before step 10 whatever step 10 decides;
     - m3's assumevalid problem goes away;
     - G1q is then empty from CH7b-2, and D3's table loses its CH7b-2 row.
  2. **Arms (a)–(c): keep them** as defence in depth. A `NotBuilt` mis-mapped by a port would otherwise become a reject such as `bad-mnhf-missing-quorum` (`mnhf:148`) or `bad-assetunlock-quorum-internal-error`. But:
     - state the pre-empted set ("every reject Core reaches before the first members, quorum or precompute call") as a declared interim difference, in the same register CH3 §2.17's refusal text cites;
     - add L0 rows: an invalid ProRegTx before a well-formed non-null QC gives the readiness refusal through CH7b-1 and Core's `bad-protx-*` from CH7b-2;
     - if (d) is kept anyway, add a valid-signature CbTx with `bestCLHeightDiff >= height`: the refusal through CH7b-2 and `bad-cbtx-cldiff` from CH7b-3.

### m3. `startup_refusal` lacks G1q's inputs, and with arm (d) it can refuse to restart a datadir the same binary built

- **Where:** D3 "Startup"; §11.2 CH3 §3.1 erratum.
- **Code and Core:**
  - `g1q(block, &payloads, prev, &ctx, check_cbtx_merkle_roots, arms)` needs the deployments (for arm (c)'s `IsQuorumRotationEnabled` and the types enabled at `pprev`) and the assumevalid flag.
  - `startup_refusal(&self, block, at) -> Option<String>` takes neither. CH3a's load path builds the deployments itself (`ChainDeployments::new`, `rs:chain/load.rs:385-386`).
  - For recovery's replayed blocks, Core's `RollforwardBlock` passes `fCheckCbTxMerkleRoots = false` (`v:5330-5370`; the design's §4.1.2), so arm (d) is off for them in Core's own terms.
  - The tip was connected under whatever `fScriptChecks` held then (`v:2594-2623`). That isn't recomputable at startup: it depends on the best header and the proof-equivalent time.
- **Problem:**
  - If the startup check evaluates arm (d) with the flag `true`, a CH7b-2 node that synced past V20 under default `-assumevalid` can't restart. Arm (d) was off when each block connected, and almost every post-V20 tip carries a CbTx ChainLock.
  - The arm isn't needed at startup either. CH3 §2.17's reason for the startup check is the undo and extension this build cannot do, and H9 needs no quorum answer for a CbTx ChainLock.
  - Also, the erratum must change `check_startup_readiness`'s early return on `H::COMPLETE` (`load.rs:382-384`), not only add the method.
  - `Option<String>` departs from CH3's pattern, where the hook returns a capability (`ChainHelper::interim_refusal -> Option<&'static str>`; `NotReady(&'static str)`) and the caller formats it with `refusal_text`.
- **Fix:**
  - Define `startup_refusal(&self, block, at, deployments: &dyn DeploymentView) -> Option<&'static str>` (provided, default `None`). It evaluates arms (a)–(c) only; arm (d) is off at startup, or gone per m2. The load path formats the result with `refusal_text` and calls it when `COMPLETE` is true.
  - Test: at CH7b-2, a datadir synced with `-assumevalid` whose tip has a valid CbTx ChainLock restarts.

### m4. MP1a, MP1b and MN1 have no route to the `dmn_tip` list where Core reads `GetListAtChainTip`, and the drain move may never be published

- **Where:** D16's reorg-update bullet, §5.4 (MP1a and MN1 rows), §11.2 (the CH3 seams and MP1b bullets).
- **Core:**
  - Mempool code reads `GetListAtChainTip()`, which is `tipIndex`, the lagging DMN tip:
    - `addUncheckedProTx` (`txm:647`, `:660`, `:668`, `:699`), reached by the re-add that follows `v:433-434`;
    - `removeProTxSpentCollateralConflicts` (`txm:1069`) and `removeProTxConflicts` (`txm:1250`), reached from `removeForBlock` (`txm:1321-1344`), which `ConnectTip` calls at `v:3572`, inside an ABC step while `tipIndex` still lags;
    - `GetKeyChangeTarget` (`txm:1139`).
  - `FillBlockPayments` reads it for the template's superblock arm (`pay:451-455`).
  - `CheckSpecialTx` reads `GetListForBlock(Tip())` through `pindexPrev` (`stm:300-305`), which is what §5.4 gives MP1a.
- **Problem:**
  1. §5.4's MP1a row has only "`EvoReadView` at the chain tip". The ProTx conflict maps need the `dmn_tip` list too. MN1's row likewise lacks the list for `FillBlockPayments`.
  2. §11.2's MP1b bullet says "Wherever Core's re-add reads `GetListAtChainTip`, MP1b reads the list at `dmn_tip`". That is too narrow, because `removeForBlock` reads it as well. It also has no channel: D16 gives the sink no evo handle, and `ChainView.mn_list` "is published with the next `ChainView`", i.e. after the step. So inside `maybe_update_for_reorg` and `remove_for_block`, MP1b can't see the list V has just moved to.
  3. Publication: in a step that only disconnects, with a mempool, V moves `dmn_tip` at the drain after the last commit's publication. No `SynchronousUpdatedBlockTip` follows (`pindexFork == pindexNewTip`, `v:3926-3937`). So `ChainView.mn_list` can stay on the pre-drain list while V's own `dmn_tip` has moved. Under `cs_main`, Core's readers would see the moved one.
- **Fix:**
  - Fix the seam once, together with `has_mempool`: the `MempoolSink` calls V makes inside a step whose Core counterparts read `tipIndex` (`maybe_update_for_reorg`, `remove_for_block`) receive V's current `dmn_tip` list (`&Arc<MnList>`). A V-written read cell would also work. `dmn_tip` keeps one writer either way.
  - §5.4 adds `ChainView.mn_list` to the MP1a row (the ProTx conflict maps) and the MN1 row (`FillBlockPayments`).
  - V publishes a `ChainView` whenever `dmn_tip` changes, before the step or command ends.
  - Widen the MP1b bullet to "every mempool call Core makes while `tipIndex` lags". Extend D16's with-mempool test to assert `ChainView.mn_list` after a disconnect-only step.

### m5. The `connect-evo-deep-window` row doesn't hold D22's bound as specified (M3 residue)

- **Where:** §7.1 (CH7a-5 rows), D22's "PF0's `connect-evo-deep-window` row (§7.1) holds the bound".
- **Problem:** As specified ("20,000 DIP3 blocks with null commitments only, connected with no flush: the per-block instruction count over the last 1,000 blocks must be within 5% of the first 1,000's"):
  1. **It compares different blocks.** Unless the chain is a uniform synthetic one, the transaction load of blocks 1–1,000 and 19,001–20,000 differs by more than 5%, so the row fails or passes for reasons unrelated to depth. The design doesn't name the chain.
  2. **It never exercises the range read M3 was about.** With null commitments only, the `mined` `OrdMap` is empty. The `llmq_mined_h` merge with tombstones (`GetMinedAndActiveCommitmentsUntilBlock`, `bp:731-773`) then merges an empty map with RocksDB. `resolve_list(prev)` hits the LRU, so by-hash reads of the window barely occur. A stack-of-deltas implementation with a fast empty-delta check could pass.
  3. **It doesn't check for flushes.** On a mainnet-sized list, roughly 35 `mn_snapshot` rows in 20,000 blocks (`dmn:723-733`) can approach `EVO_CRITICAL` (64 MiB), and the dbcache can force a flush. A mid-window flush resets the depth being measured.
  4. **The tolerance is too coarse.** A whole-block instruction count, including script checks, against a 5% tolerance can hide an O(window) walk over near-empty layers.
- **Fix:**
  - Measure the same 1,000 blocks after a shallow and after a deep unflushed window, or use a uniform regtest chain. Assert that no flush ran in the measured range.
  - Judge the evo layer's own functions by callgrind per-function counts (PF0's method), not the whole-block count.
  - Add an `EvoIndex` L0 or bench at CH7a-1 with a populated `mined` map and tombstones at 1k vs 100k entries.
  - Add a CH7b-3 deep-window variant across a mining window with non-null QCs, so the merged range read with tombstones is measured.

## Nit

**n1. D16's "once per ABC iteration that connected at least one block".**
- Core sets `blocks_connected` after every successful step (`v:3904`), and signals only when `pindexFork != pindexNewTip`, with `pindexFork = FindFork(starting_tip)` (`v:3926-3937`).
- An iteration can connect blocks and end at its starting tip: step 1 disconnects and meets an invalid block, then step 2 reconnects the old chain (the inner `while`, `v:3927`). No signal follows.
- Implementing the update at the signal site, as D16 does, is right. Reword it as the signal's own condition.

**n2. `rs:chain/failure.rs:51` is not a literal `true` call.** Its argument is `*disconnected <= 10 && ret`, Core's `v:4081`. Say "when the call's `add` argument is true" (not on the 11th disconnect, and not after a failed `DisconnectTip`). Ideally route all three sites through one V wrapper, `reorg_drain(pool, add)`.

**n3. The §7.2 L0 row "a duplicate null QC (`bad-qc-dup`)" holds only for a non-rotation type.** It comes from `GetCommitmentsFromBlock` (`bp:632-637`). The per-block duplicate check exempts null commitments (`bp:270-276`). Name the type, and add the rotation case: an excess null QC gives `bad-qc-not-allowed`.

**n4. CH2 doc and freeze check.**
- `StateDelta::write_b1`'s doc says "in block order" (`rs:flush/packet.rs:21`). Under D22, B1 writes the merged entries, so add the doc change to §11.2.
- D22 can cite `EvoWindow::best_block`'s freeze check (`rs:flush/control.rs:36`: "a freeze checks that it is `new`") as the guard that the frozen index's `evo_tip` equals the flush's `new`.

**n5. CH7b-2's content should name the chain-aware `ScanQuorums`** (route C) next to the quorum object. Arm (b) lifts at CH7b-2, and the asset-unlock signature check uses both `ScanQuorums(…, chain)` and `GetQuorum(…, chain)` (`al:165-172`; `bad-assetunlock-too-old-quorum`, `al:129`).

**n6. H2 under VerifyDB.** D7's H2 reads "the active, then the frozen index's `evo_tip`, else `meta.evo_tip`". For origin `VerifyDb` it must read the scratch `EvoTxn` index's `evo_tip` first, as Core's `VerifyBestBlock` in VerifyDB reads its open transaction (`v:5197`). Level 3's disconnects move it, and level 4's connects check against it.

---

## Checked and correct

### Focus 1: M1, the member-free checks from CH7a-2

- **Step 6 runs LQ2's `process_block` over `NoMembers`.**
  - `process_block` calls the precompute itself (`rs:dash-dml/src/blockprocessor.rs:367`), so §4.1.2's removal of the second call is right.
  - `NoMembers::precompute` refuses exactly where `bp:35-43` calls `GetAllQuorumMembers`, and arm (c) uses the same predicate.
  - `GetNumCommitmentsRequired` (`bp:245-264`, `:650-670`) and `GetCommitmentsFromBlock`'s `bad-qc-dup` and `bad-qc-premature` (`bp:608-648`) read no members.
  - The null branch is member-free on all three paths: `CheckLLMQCommitment` (`commitment.cpp:292-298`), `ProcessCommitment`, and `ApplyTxToList`, which skips null QCs (`stm:725-751`).
- **Step 9's quorum root and step 10's arms before the verify** are live: `stm:185-201`, `:205-209` and `:233-235`. With `cl_best` none, neither shortcut (`stm:176`, `:211`) can fire.
- **`QcBlockError::Fault(NodeMemberError)`** carries the `StoreError`, so §4.1.2's `is_not_built()` arm is implementable.
- **Every slice's "accepts nothing Core rejects" claim holds.** I found no window where the node accepts a block Core rejects. CH7b-2 lifts arms (a)–(c) in the same slice that brings the production facade, mined rows, `erased_mined` and the quorum object. The CH7a-4 L0 window rows are correct, apart from n3.

### Focus 2: M2, `dmn_tip`

- `mn_list` now has one definition across the tip path, TBV and VerifyDB.
  - VerifyDB never moves `tipIndex`: it has no `UpdatedBlockTip` or drain call.
  - Startup VerifyDB sees the empty list (`dmn:944-951`; `init.cpp:2130` comes before `:2493`).
  - Both superblock arms read the `dmn_tip` list (`pay:367`, `:424`).
  - MP1a's `CheckSpecialTx` through `EvoReadView` at the chain tip matches `stm:300-305`.
- The remaining gaps are m1 and m4.

### Focus 3: M3 and D22

- The structure fixes M3: persistent `imbl` maps; O(1) pinning; O(log n) point reads plus one DB read on a miss; an ordered map whose tombstones hide the rows below them; at most two in-memory indexes; B1 equivalent to writing the deltas in block order; and the same structure for VerifyDB's `EvoTxn`.
- `StateDelta: Send + 'static` is satisfiable with `imbl`'s `Arc` nodes.
- The remaining gaps are m5, n4 and n6.

### Focus 4: the CH3-sourced D16 change (CE4-17)

Verified at the pin and present in the rc.4 tag 0e7382df80d (not a hook):
- **The early return:** `if (!m_mempool) return;` (`v:426`).
- **The move:** `if (fAddToMempool && m_chain.Tip()) …UpdatedMNListTip(m_chain.Tip())` (`v:433-434`), which goes `UpdatedMNListTip` → `UpdatedBlockTip` → `tipIndex = pindex` (`stm:879`, `dmn:822-827`).
- **The add=true callers:**
  - `v:3789`: at the end of every step that disconnected, after the step's connects;
  - `v:4081`: `++disconnected <= 10 && ret`;
  - `v:4250`: once after `MarkConflictingBlock`'s loop, even when `pindex` was not in the chain.
- **The add=false callers:** `v:3728`, `:3772` and `:4228`.
- **The node's sites:** `rs:chain/abc.rs:529`, `failure.rs:51` and `locks.rs:463` correspond to these.
- **`NullMempool`** is Core without a mempool.
- **Order:** moving before the drain matches Core.
- **The outer loop:** `v:3869-3958` wraps an inner do-while of steps, signals at `v:3937` once per outer iteration, and `InvalidateBlock` (`:4152`) and `MarkConflictingBlock` (`:4265`) signal with fork `nullptr`.
- **MP1b's §11.2 obligation** is directionally right, apart from the gaps in m4.

### Focus 5: the CH3 §3.1 errata

- **`SpecialTxFailure` is the minimal change.** A `NotBuilt` reached inside H4 must become `refuse`, which is unreported under `just_check` (CH3a D121). Returning `Fatal` would stop the node on a proposal.
- **It is consistent with CH3 §2.17's refusal semantics:** a `SystemError`, no `FAILED_*`, `HAVE_DATA` kept, and reported on the tip path.
- **Q14/DEC-518 is unaffected.**
- **`startup_refusal` as a provided method is the right shape;** its inputs are m3.

### Focus 6: the interim divergence

- It is documented for three strings only and has no test; see m2.
- The assumevalid gate on arm (d) (Sol r2) is correct: `stm:899-917` and `:1008-1053`.

### Other r2 dispositions spot-checked and correct

- **m1:** the `NotBuilt` channel.
- **m3:** H9 under `bls_ctx_for_block(pindex)`. The tip-path disconnect of the last block before V19 prints "entered … =1", and VerifyDB's switch matches `stm:1091-1099`.
- **m4:** D15's listener rule and its registration after startup VerifyDB (`init.cpp:2228`).
- **m6:** `PendingTx` and the assertion at `begin_attempt`.
- **m7:** the `CbTxClCell` write points (`stm:176-183`, `:211-217`, `:230-232`).
- **n1, n2, n5 and n7.**
