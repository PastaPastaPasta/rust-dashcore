# CH7 design rev2 (6a3fab586): Opus-h review (r2, Core-equivalence and architecture)

**Verdict: APPROVE-WITH-CHANGES.** 0 Blocker, 3 Major, 7 Minor, 8 Nit.

**Scope.** I reviewed `docs/node-program/designs/CH7-evo-llmq-chainstate.md` at 6a3fab586. Core references are at the pin, 375c64d2317. Rust references are at the design's base, 497d3ca11, which is an ancestor of the branch. Core paths use the design's §0.1 keys (`v:` validation.cpp, `stm:` evo/specialtxman.cpp, `bp:` llmq/blockprocessor.cpp, and so on). `D:N` means a line of the design document.

**Summary.** Sol's r1 items are fixed as DEC-520 asked. The rest of the design is close, but three things need changing:
- The slice plan lifts G1 at CH7a-4, but it schedules H4's member-free consensus checks for later slices: step 6's count and null rules, step 9's quorum root, and step 10's null-signature arm. Between those slices the node accepts blocks that Core rejects.
- §3.6 names the wrong masternode list for the superblock arm, which contradicts the design's own D16.
- §7.1 never specifies how per-block reads go through the delta layers, so connect cost is unbounded relative to Core.

---

## Findings

### Major

**M1. CH7a-4 lifts G1, but H4's member-free checks only arrive in CH7b-2 and CH7b-3. Until then the node accepts blocks Core rejects.**

- **Where:** §8.1 CH7a-4 (D:796), §8.2 CH7b-2 and CH7b-3 (D:806-807), §3.2 steps 6, 9 and 10 (D:244, D:247-248), D3 (D:264-273).
- **Core, for checks that need no quorum member or object:**
  - Step 6: `bp:245-264` checks the commitment count against `GetNumCommitmentsRequired` (`bad-qc-not-allowed`, `bad-qc-missing`). That reads only the chain and the mined index (`bp:650-670`, `:694-717`).
  - Step 6 also has the per-block duplicate checks (`bad-qc-dup`, `bp:271-276` and `bp:632-637`), `bad-qc-premature` (`bp:643-645`), and `ProcessCommitment`'s null path: `bad-qc-block` and `bad-qc-invalid-null` (`bp:393-420`).
  - Step 9: `CalcCbTxMerkleRootQuorums` (`stm:1027-1035`). Before the first non-null QC this is the root over an empty set, and it still has to equal the CbTx field.
  - Step 10: `CheckCbTxBestChainlock`'s null-signature arm. It rejects `bad-cbtx-cldiff` when the signature is null and `bestCLHeightDiff != 0` (`stm:233-235`), and `bad-cbtx-null-clsig` when the parent's coinbase CL is non-null (`stm:185-198`). Neither path reads a quorum.
- **Problem:**
  - G1q (D:270) refuses only the blocks that need members or a quorum object.
  - CH7b-2 is the slice that brings `connect_commitments` ("null rules"), the quorum root and the `QcHashCache`. CH7b-3 brings `check_cbtx_best_chainlock`. Nothing in CH7a-2 or CH7a-4 runs LQ2's `process_block`, `quorum_root` or the null arm of step 10.
  - So from CH7a-4 until CH7b-2/3 the node connects blocks that Core rejects: a mining-window block that omits the required null commitment, a block with a wrong `merkleRootQuorums`, or a null-CL CbTx with a non-zero height diff.
  - D:273's claim "CH7a-4 can … match dashd on them" holds only for valid chains. CH7a-5's L2 runs only valid chains, so it won't catch this.
  - The pieces already exist at the base: `dash_dml::blockprocessor::{process_block, undo_block, quorum_root}` (`rs:dash-dml/src/blockprocessor.rs:354`, `:502`, `:706`). Their `QcCtx` takes the members source and the precompute as trait objects (`:60-75`), so a refusing stub plugs straight in.
- **Fix:**
  1. Move the member-free parts into CH7a-2, so they are live when CH7a-4 lifts G1:
     - step 6 through LQ2's `process_block`/`undo_block`, with `NoMembers` as the `DmlMemberSource` and as the `QuorumPrecompute`. The stub's precompute must run Core's loop and refuse only where the loop would call `GetAllQuorumMembers` (see m1);
     - step 9 through `quorum_root` and one `QcHashCache`;
     - step 10's arms before `VerifyChainLock` (`stm:174-217`, `:233-235`), with `cl_best` answering "none" until LK1a, which `stm:176` and `:211` handle. Only the `VerifyChainLock` call (`stm:226`) is a stub refusal.
  2. Restate CH7b-2 as "the production facade, mined rows and `erased_mined`, the quorum object". Restate CH7b-3 as "the CL verify, `CbTxClCell`, `ReplayChainHelper`".
  3. State G1q's predicate for each slice. At CH7a-4 it is D3's full list. At CH7b-2, where the production facade replaces `NoMembers`, only the arm for a valid CbTx CL signature remains. At CH7b-3 there is no G1q.
  4. Add §7.2 L0 rows for the window: an omitted required null QC (`bad-qc-missing`), an extra null QC (`bad-qc-not-allowed`), a bad `merkleRootQuorums`, and a null-CL CbTx with diff ≠ 0. Each must get Core's reject at CH7a-4.

**M2. §3.6 gives the superblock arm the parent's list. Core reads the DMN tip's list, which is D16's rule. The definition of `ChainView.mn_list` is also inconsistent across the design.**

- **Where:** §3.6 (D:359), the §5.1 struct comment (D:643), D16 (D:652-657), and the §5.4 MP1a row (D:683).
- **Core:**
  - `IsBlockValueValid` and `IsBlockPayeeValid` read `m_dmnman.GetListAtChainTip()` (`masternode/payments.cpp:367`, `:424`).
  - That function returns the list at `tipIndex` (`dmn:944-951`).
  - `tipIndex` moves only in `UpdatedBlockTip` (`dmn:822-827`), which `SynchronousUpdatedBlockTip` calls when `pindexNew != pindexFork` (`dsnotificationinterface.cpp:47-53`). That happens once per `ActivateBestChain` step, after the step's last connect (`v:3928-3937`).
  - At startup `tipIndex` is null until `InitializeCurrentBlockTip` (`init.cpp:2493`), which runs after `VerifyLoadedChainstate` (`init.cpp:2130`). Until then the list is empty.
- **Problem:**
  - §3.6 says "During a connect the tip is the parent, so this is the same list on the tip path". That is false for the second and later blocks of a multi-block step, such as a reorg step or a burst while synced. There `GetListAtChainTip` is the list at the step's starting tip. After a reorg's disconnects it is even the old fork's tip.
  - The superblock arm's vote counting (`IsSuperblockTriggered(tip_mn_list, h)`, `IsValidSuperblock(…, tip_mn_list, …)`) depends on that list, so the verdict can differ from Core's.
  - The design's statements also disagree with each other:
    - §5.1 says `mn_list` is "the list at snap's tip";
    - D16 says it is the list at `dmn_tip`, which lags;
    - §5.4 tells MP1a to use `ChainView.mn_list` for `CheckSpecialTx` at `Tip()`, but Core's mempool checks read `GetListForBlock(Tip())` through `pindexPrev`.
  - The VerifyDB remark in §3.6 ("the tip at VerifyDB's start") is also inexact. At startup Core's list is empty, and only the gates (`sb_required` false, governance not valid) keep the arm from being reached.
- **Fix:**
  - Give `ChainView` an explicit `dmn_tip: Option<BlockId>`, and define `mn_list` as the list at `dmn_tip` (empty while it is `None`). Fix the §5.1 comment.
  - In §3.6, pass V's current `dmn_tip` list, not the parent's, to both superblock arms on every origin.
  - In §5.4, have MP1a's `CheckSpecialTx` read `EvoReadView` at the chain tip, and keep `ChainView.mn_list` for `GetListAtChainTip` callers: GV1b, AM1 and the governance vote checks.
  - Add an L0 test: a two-block reorg step whose second block is at a superblock height with `sb_required` true. The arm must see the old tip's list.

**M3. §7.1 doesn't say how per-block reads go through the delta layers. As written, reads cost O(window), not Core's O(log n).**

- **Where:** §2.1 `layer.rs` and `view.rs` (D:98-100), D17 (D:671), §6.1 merging (D:703), the §7.1 table (D:738-748), and `EvoTxn::deltas() -> &[Arc<EvoDelta>]` (D:141).
- **Core:** `CEvoDB`'s current and root transactions are `CDBTransaction`s, which are map-backed. Point reads are O(log n), and iterators merge the maps with LevelDB (`GetCurTransaction().NewIteratorUniquePtr()`, `bp:736`, `:780`).
- **Problem:**
  - Every connect makes several evo reads through `EvoReadView`, each going attempt delta → active deltas → frozen deltas → RocksDB snapshot:
    - `resolve_list(prev)` walks the diff and snapshot rows;
    - `pool_after(prev)`, `GetSignalsStage(pprev)` and `HasMinedCommitment` per type;
    - `GetMinedAndActiveCommitmentsUntilBlock` range scans, per type, for every CbTx v2+ block (`bp:513-515`, `:731-773`).
  - The design describes the layer as a stack of per-block `EvoDelta`s. `EvoDelta` holds one block's rows (`rs:dash-dml/src/store/delta.rs:26-48`).
  - A window can hold up to 64 MiB of encoded rows (D18), which is tens of thousands of blocks in IBD. A linear walk per lookup makes IBD quadratic in the window, and a miss, the common case for a hash-keyed row, walks the whole stack before it reaches RocksDB.
  - The `llmq_mined_h` scans also need a merged iterator that applies M4's tombstones from later deltas over earlier rows and the DB.
  - None of the proposed PF0 rows has a deep unflushed window, so none would catch this.
- **Fix:**
  - Specify that the active and frozen layers keep an index merged across blocks: a persistent hash map per block-hash-keyed CF and an ordered map (with tombstones) for `llmq_mined`/`llmq_mined_h`. With `imbl` maps, pinning an `EvoReadView` is O(1). Range reads merge the ordered map with a RocksDB iterator.
  - State the bound: a point read is O(log n) in the window plus one DB read on a miss, and a range read costs O(k + log n).
  - Give VerifyDB's `EvoTxn` the same structure.
  - Add a PF0 row `connect-evo-deep-window`: N blocks connected with no flush, whose per-block instruction count must not grow with N.

### Minor

**m1. The `NoMembers` refusal has no error channel that reaches the readiness `SystemError`, and "every precompute call refuses" contradicts null-only blocks connecting.**

- **Where:** D2 (D:262), D3 (D:265), §7.2 D3 tests (D:761).
- **Code:**
  - `StoreError` has only `Read` and `Inconsistent` (`rs:dash-dml/src/store/mod.rs:42-57`).
  - A members error in LQ2 and EV1 becomes `QcBlockError::Fault` (`rs:dash-dml/src/specialtx/qc.rs:63`, `:102`), which §4.1.2 maps to `Fatal::abort_node` (D:441). §4.2.4 maps `Missing` and `View` the same way, with "Found EvoDB inconsistency, you must reindex".
  - Core's `PreComputeQuorumMembers` runs on every DIP3 block, but calls members only at rotation cycle bases (`bp:35-43`).
- **Problem:**
  - A stub refusal would come out as an AbortNode telling the user to reindex, not as D69's readiness text. The §7.2 assertion ("reaches the stub's refusal") couldn't pass as specified.
  - Read literally, "every … precompute … call is a readiness refusal" refuses every DIP3 block at step 6.
- **Fix:**
  - Add a dedicated variant, for example `StoreError::NotBuilt(&'static str)` or a `MemberError` variant. EV1, EV2b and LQ2 must propagate it unchanged and never map it to `Exception` or a reject.
  - H4 maps it at its boundary to `SystemError(refusal_text(…, "quorum validation (CH7b)"))` before D5's fault mapping.
  - State that `NoMembers::precompute` runs Core's loop and refuses only on the members call.

**m2. D21's premise is false: `NoEvo` datadirs already carry `meta.evo_tip = tip`.**

- **Where:** D21 (D:721), Q7 (D:877).
- **Code:**
  - CH2's B1 writes `Meta::put_evo_tip(&mut b, &f.new.hash)` on every full flush, whatever the hooks (`rs:flush/machine.rs:942-943`; also `:714`, `:732`).
  - Recovery's no-marker path requires `evo_tip == tip`, or it fails with `EvoInconsistent` (`rs:recovery/replay.rs:412-414`).
  - CH3 §2.17 "Removal" ("H6 is a no-op … VerifyBestBlock fails on an empty EvoDB") predates that code.
- **Problem:** D21's trigger ("if `meta.evo_tip` is absent") never fires on a real `NoEvo` datadir. A datadir with no `evo_tip` and a tip already fails at recovery, before D21 runs.
- **Fix:**
  - Restate D21: a `NoEvo` datadir has `evo_tip = tip` from B1. Its tip is below every G1 deployment, so its empty evo CFs equal `NodeEvo`'s state there (D11; V20 is at or after DIP3).
  - The startup check keeps DEC-517 (7)'s condition as a consistency check: if DIP3 is inactive at the tip, every non-effect evo CF must be empty; otherwise `-reindex-chainstate`. Write nothing.
  - Add the CH3 §2.17 erratum to §11.2.

**m3. BLS: the pin's `DisconnectTip` guard means H9 step 1 never fires on the tip path, and the guard's log lines aren't modelled.**

- **Where:** §3.3 step 1 (D:290), D9 (D:300-312).
- **Core:**
  - `DisconnectTip` enters `ScopedBLSLegacyScheme{!V19 at pindexDelete}` before `DisconnectBlock` (`v:3376-3377`). So on the tip path `UndoSpecialTxsInBlock`'s condition, `!V19 at pindex && !legacy` (`stm:1094`), is always false. Its "CSpecialTxProcessor::%s -- bls_legacy_scheme=%d" line prints only in VerifyDB level 3, which calls `DisconnectBlock` with no per-block guard (`v:5263`).
  - The guards print "ScopedBLSLegacyScheme: entered bls_legacy_scheme=%d" and "… reverted …" (`v:161`, `v:168`) at `v:2542`, `v:3376`, `v:3509` and `v:5194`.
- **Problem:**
  - As written, H9 would print the undo line on tip-path disconnects across V19, and the node never prints Core's guard lines. D19 and D20 show the design aims for log-diff parity.
  - H9's `BlsCtx` (for QC decode in the undo's `GetCommitmentsFromBlock` and its precompute) isn't named.
- **Fix:**
  - State that H9 runs under `bls_ctx_for_block(pindex)`, which equals the scheme Core's guard enters.
  - Step 1's switch and its log fire only for origin `VerifyDb`, where the "current scheme" is VerifyDB's running scheme starting from `ChainView.bls`.
  - Emit the guard's entered and reverted lines where Core's guard would change the value. Alternatively, list these lines as an allowed log difference.

**m4. H7's synchronous subscribers run on V inside an attempt that holds the exclusive gate. The design gives no rule against re-entering V, and lists subscribers that can't follow one.**

- **Where:** D15 (D:661-667), §3.8 `NotifyApplier` (D:389, D:406), `NodeEvo.publish` (D:116).
- **Core:**
  - `CDSNotificationInterface` is the only listener (`dsnotificationinterface.cpp:90-96`; no other override in `src/`).
  - It is registered at `init.cpp:2228`, after `VerifyLoadedChainstate` (`:2130`). So Core's startup VerifyDB fires the MN-list signal with no Dash listener attached. A later `verifychain` RPC does reach it.
- **Problem:**
  - D15 names AM1, DK1, QM1a and SB19's shadow task as subscribers. SB19 and QM1a work through `OnV` dispatch. From inside an attempt that is a debug assertion, or a deadlock in release (ADD §2.6: "`dispatch` called on V is a debug assertion"; never wait on V while holding the gate).
  - The design also uses two mechanisms without saying which consumer uses which: `ChainEvent::MnListChanged` (async, A1 §2.10.2) in D:116, D:178 and D:389, and `ValidationInterface::notify_masternode_list_changed` (sync) in D15.
  - Delivering startup-VerifyDB notifications to subscribers differs from Core.
- **Fix:**
  - Rule: H7 and VerifyDB-effect subscribers run on V. They must not dispatch, take the gate or block on V, and they read only the arguments plus non-effect state.
  - Only Core-equivalent listeners subscribe synchronously: the CMNAuth equivalent and GV1b's `CheckAndRemove`. AM1, DK1, QM1a and SB19 consume the async `ChainEvent::MnListChanged` after the commit.
  - Register the synchronous listeners after startup VerifyDB, as Core does, or have the `NotifyApplier` drop notifications from startup VerifyDB.

**m5. The Q12 PF0 rows can't run where the design anchors them.**

- **Where:** §7.1 (D:750), §8.1 CH7a-5 (D:797).
- **Problem:** Under G1q, which keeps all of its arms until CH7b-2 and keeps the CL arm until CH7b-3 (M1), neither row can connect its window before CH7b-3:
  - `connect-evo-v20-window` (2,000 mainnet blocks after V20): every V20 window carries non-null QCs, asset unlocks and non-null CbTx CL signatures.
  - `connect-evo-dip24-cycle`: a cycle base is a precompute refusal.

  So the CH7a-5 re-anchor has no evo-active window to measure.
- **Fix:**
  - At CH7a-5, anchor `b1-evo-write`, `mnlist-tip-publish`, M3's deep-window row and a pre-quorum DIP3 window (mainnet from 1,028,160 up to the first non-null QC, or a regtest chain with null commitments only).
  - Add the V20 and DIP24 rows at CH7b-3, which is where Q12's second re-anchor already is.

**m6. The handoff of D1's delta to `commit_tx` is unspecified.**

- **Where:** §2.1 `NodeEvoAttempt.delta` (D:129), D1 (D:213-217).
- **Code:**
  - `finish_attempt(attempt)` consumes the attempt before the commit (`rs:connect/connector.rs:320-342`).
  - `commit_tx(&mut self, evo: &mut EvoTarget)` and `rollback_tx` take no attempt (`rs:connect/evo.rs:294`, `:301`).
  - An attempt whose `begin_tx` never ran gets no `rollback_tx` (`evo.rs:295-301`).
- **Problem:**
  - The delta lives in the attempt but has to reach `commit_tx`. If `finish_attempt` parks it in `NodeEvo`, it leaks into the next attempt on any path with no `begin_tx`.
  - The same applies to the BLS `post_scheme` that D9 publishes.
- **Fix:**
  - Specify that `begin_tx` opens `NodeEvo.pending: Option<EvoDelta>` (or the scratch `EvoTxn`'s open entry). H4, H9 and H6 write there. `commit_tx` pushes it, and `rollback_tx` drops it.
  - `begin_attempt` asserts that `pending` is `None`. The attempt keeps only the log and the handle.

**m7. `CbTxClCell` "never on failure" isn't what Core does.**

- **Where:** §4.5.2 (D:587), §4.4 row (D:572).
- **Core:** The statics are written on step 10's success paths (`stm:179-182`, `:213-216`, `:230-232`), whatever happens after step 10. For example: a later MNHF reject at step 11, a `just_check` TBV (with the dummy index), or VerifyDB level 4.
- **Problem:**
  - The cell's content is observable when the parent's block data is unavailable: `GetNonNullCoinbaseChainlock` then reads `nullopt` (`cbtx:181-198`). This is BL-179's territory.
  - If "failure" means an attempt that fails or is check-only, the node would skip updates that Core makes.
- **Fix:** State that the cell is mutated in the attempt at step 10, non-transactionally. Core's updates happen in failed and check-only attempts too, and the only exclusion is step 10's own reject paths. Add a test: step 10 succeeds, step 11 rejects, and then a sibling block's step 10 sees the cell set.

### Nit

**n1. G1q's position and its startup check.**
- G1 sits before `VerifyBestBlock` and BIP30 (`rs:connect/serial.rs:684-689`). Place G1q at H4's entry, after step 0, so that Core's earlier `ConnectBlock` rejects come first.
- The startup check returns early on `H::COMPLETE` (`rs:chain/load.rs:382-384`), so G1q needs its own startup predicate, which reads block data for the QC/CbTx fields.

**n2. G1q's rotated-cycle arm should use Core's predicate.** For each type enabled at `pprev`, check `IsQuorumRotationEnabled(params, block)` (`useRotation` and DIP0024 active after the ancestor at `cycleBase − 1`, `llmq/options.cpp:50-62`) and `h % dkgInterval == 0`. This must be the same predicate the stub's precompute uses. The CbTx `bestCLSignature` validity must be decoded under H1's `BlsCtx`, because `IsValid` depends on the scheme.

**n3. Wrong D-row reference.** §3.8 (D:404) says "An applier error is D4"; it should be D5.

**n4. `impl StateDelta for EvoDelta` (D:695).** `EvoDelta` is one block's delta (`rs:dash-dml/src/store/delta.rs:26-48`), and the window is a sequence of them. It also lacks the `erased_mined` and staged `evo_tip` fields the design uses. Name the window type (for example `EvoWindowDelta`) and the field additions to `dash-dml`.

**n5. Genesis has no `b_b4` in Core.**
- Core returns for genesis without `WriteBestBlock` (`v:2588-2592`), so with regtest `-dip3params=1:N` its block 1 fails `VerifyBestBlock` and aborts (`v:2577-2583`; `evodb.cpp:120-126`).
- The node's B1 writes `evo_tip = genesis`, as §3.1 row 4 says.
- Either make H2 treat an `evo_tip` equal to genesis as absent, or record the difference in ALLOWED-DIFFS.

**n6. `MemPlatformBans` (D:405).** Platform-ban state is visible over RPC (`masternode/meta.cpp:50-51`, `is_platform_banned` and `platform_ban_height_updated`). RP2a's metaInfo comparisons therefore need SP1's store, and SP1 starts from empty when it replaces the stand-in.

**n7. §4.1.4's test and tombstone predicate (D:466-480).**
- Add a variant with no flush between the disconnect and the reconnect: the tombstone sits in the layer over a row already in RocksDB. This exercises `bad-qc-dup` and the `WriteDerived` identity check through tombstones (`bp:422-434`, `:473-476`).
- Also state that `rotation_index` uses the mining block, as Core's undo does (`bp:586`), while the write uses the base (`bp:462`). The two always agree, because a commitment is mined in its base's cycle (`bp:379`, `:436`).

**n8. §9.1 maps "`diff_chain` with evo ops" to CH7a-2 (D:833).** G1 still refuses evo-active blocks in the node until CH7a-4, so a chain-level diff can't pass at CH7a-2. Map it to CH7a-4, and keep the hook-level L0/L1 tests at CH7a-2.

---

## Checked and found correct

- **H4 order, gates and rejects** (§3.2 against `stm:881-1085`):
  - The CbTx parse is gated on `fCheckCbTxMerkleRoots` and `nType == COINBASE` (`bad-cbtx-invalid`, `bad-cbtx-payload`, the re-wrapped reason). `bad-cbtx-version` is gated on DIP3 at pindex.
  - The credit-pool indexes are gated on V20 at pindex.
  - The per-transaction loop: shared-collateral template checks on every transaction when `rules.v24`, the coinbase skip, `check_sigs = fCheckCbTxMerkleRoots`, and the chain-aware MNHF/AU variants.
  - The pool diff runs only with a parsed CbTx, then the DIP8 and V20 gates at `stm:1127-1128`.
  - Then: the quorum `ProcessBlock`; the DML build with `!fJustCheck` before `ProcessBlock`; the MN root, then the quorum root at version ≥ `MERKLE_ROOT_QUORUMS`, then the CbTx CL; MNHF; the V19 switch after the block. The `EvoDbInconsistencyError` → `state.Error` and catch-all → `failed-procspectxsinblock` mapping is right.
  - The "order notes" (D:256-260) are correct.
- **H9:** BLS, then MNHF, then DML, then quorums (`stm:1094-1110`).
  - The DML undo erases cache entries only; a missing diff gives no updates (`dmn:787-820`).
  - The quorum undo order (precompute with reset, then per non-null QC: erase, drop the hash cache, erase the mined-block cache, re-add to the mineable pool, `bp:562-606`).
  - Core has no credit-pool undo.
- **D2/D3's decode-first bypass is sound with respect to verdicts.** A bypassed block reaches either Core's reject at the step that would parse the bad payload, or a facade call that refuses. In Core's order, a facade-dependent step before the bad payload makes Core's own verdict depend on the quorum, as D2 says. Every non-null QC check reaches `GetAllQuorumMembers` only after member-free checks (`commitment.cpp:240-305`, `bp:410-456`). Null commitments need no members (`commitment.cpp:292-299`, `bp:410-420`). The step-10 signature arm is the only arm that verifies (`stm:205-232`). The remaining gaps are M1's missing member-free steps and m1's error channel, not the bypass itself.
- **D5 / DEC-518:** the three `bp:` sites (`bp:282-287`, `:441-446`, `:372-376`) return `false` with no state. LQ2's `NoReason` matches. Mapping it to `Fatal` follows the manager's ruling.
- **D7 and B1:**
  - B1 carries `write_b1` and `meta.evo_tip = new`. A split flush adds the marker and the erase of `meta.tip`; a one-batch flush writes `meta.tip` and erases the marker (`rs:flush/machine.rs:942-985`).
  - Recovery requires `evo_tip == new` under a marker (`replay.rs:357-362`), `evo_tip == tip` without one (`:412-414`), and treats an `evo_tip` with no tip as corruption (`:387-393`).
  - Dropping Core's evo rollforward is sound because B1 is atomic.
- **D20:** Core's `LOG_TIME_SECONDS("write evodb cache to disk")` wraps `CommitRootTransaction` after the coins flush in every full flush with a non-null best block, with "Failed to commit EvoDB" (`v:3146-3169`). The node logging around B1 is the DEC-520 choice.
- **D18:** the trigger is `fEvoDbCacheCritical = IF_NEEDED && GetMemoryUsage() >= 64<<20` (`v:3095`; `evodb.h:226-234`).
- **D15:** H7's position matches Core: after `SetBestBlock`/`WriteBestBlock` and `Commit()`, inside `ConnectBlock` (`v:2965-2976`), and after `WriteBestBlock(pprev)` in `DisconnectBlockImpl` (`v:2387-2395`), both before `view.Flush()` and `dbTx->Commit()` (`v:3550-3552`, `v:3389-3391`). The code agrees (`rs:connect/serial.rs:1145-1147`, then `connector.rs:320-342`). The only Core listener reads the coins view and the old DMN tip, as D15 says.
- **D16's update sites:** the end of an ABC step that connected at least one block, with `pindexFork != pindexNewTip` (`v:3928-3937`); `InvalidateBlock` (`v:4151-4153`); `MarkConflictingBlock` (`v:4264-4266`); startup `InitializeCurrentBlockTip`. A step that only disconnects keeps the old tip. M2 is about the readers, not this rule.
- **D9:** the scheme follows the committed tip, because `ConnectTip`'s guard is committed only after `SetTip` (`v:3509`, `:3581`), `DisconnectTip`'s only for the active chainstate (`v:3424`), and VerifyDB's outer guard restores it (`v:5194`, `:5293-5295`). This equals `bls_ctx_after(tip)`. Step 12's condition reduces to "the V19 activation block", a pure function.
- **M4 tombstones:** Core's undo erases `q_mc(type, quorumHash)` and, depending on `IsQuorumRotationEnabled(…, pindex)`, either `q_mcihi(type, quorumIndex, ¬h)` or `q_mcih(type, ¬h)` (`bp:579-590`). The write side uses the same families (`bp:479-483`). `MinedErase` carries every field both keys need. The LQ2 erratum (`undo_block` returns tombstones) is needed and is enough.
- **Q1:** with erasure on undo, the rows present are exactly the current chain's, so Core's non-ancestry `GetMinedCommitment` (`bp:719-728`) and the node's ancestry-aware reader agree. With mined rows removed from `EFFECT_CFS`, recovery's empty-store check (`replay.rs:395-410`) stays correct.
- **D8:** Core's side effects are reproduced under `fJustCheck`: the precompute and snapshot writes, the `GetListForBlockInternal` cache fill, and the step-12 log. Core skips the mineable erase, the qc-hash cache drop and the platform-ban reset (`bp:458-460`; `dmn:737-752` sits inside `ProcessBlock`, which `fJustCheck` skips).
- **§3.8's appliers:** each of the six variants of `rs:dash-evo/src/effect.rs` has an applier; snapshots are presumed and the rest independent. `ResetPlatformBan` is applied on every outcome, which matches Core's direct call in `DMN::ProcessBlock` before the roots (`dmn:737-752`).
- **D4 and D13:** the chain-aware `ScanQuorums` skips `scanQuorumsCache` (`quorumsman.cpp:205`, `:283`), and `BuildQuorumFromCommitment` fills `mapQuorumsCache` and writes contributions (`quorumsman.cpp:59-105`), which D13 assigns to QM1a.
- **D10/D11, the H5 payee list and `sb_required`:**
  - The H5 payee arm reads `GetBlockTxOuts(pindexPrev, …)` (`payments.cpp:228-261`).
  - `sb_required` is `ch:42-53`.
  - `check_superblock` is `NoCheck` unless required, else `DisallowDuplicates` iff V24 (`v:2894-2896`).
- **Slice independence (§8.1, D:799):** CH7a-1 to CH7a-3 can merge before CH3b, provided D16's `dmn_tip` update lives in the `synchronous_updated_block_tip` dispatch (`rs:chain/signals.rs:65`). Then CH3b's invalidate and `MarkConflictingBlock` call sites need no edits. CH7a-3's disconnect-only test needs CH3b's `invalidateblock` or a hook-level rig. CH7b-4 depends only on CH7b-1 and CH7a-3, so SB19's code path (§8.3) works as ruled in Q10.
- **Citations spot-checked at 375c64d2317:**
  - `v:2542-2543`, `:2577-2583`, `:2588-2592`, `:2713-2721`, `:2936-2937`, `:2966-2976`, `:2387-2395`, `:3376-3377`, `:3424`, `:3532-3561`, `:3095`, `:3164-3169`, `:5194`, `:5263`, `:5310`;
  - `stm:156-239`, `:241-298`, `:725-751`, `:881-1120`;
  - `bp:35-43`, `:223-306`, `:367-502`, `:562-606`, `:608-670`, `:694-773`;
  - `dmn:700-827`, `:944-951`; `mnhf:190-283`; `dsnotificationinterface.cpp:31-96`; `evodb.cpp:120-137`; `evodb.h:226-234`; `commitment.cpp:240-310`; `quorumsman.cpp:59-105`, `:171-297`; `payments.cpp:314-440`; `chainhelper.cpp:42-53`.

  All say what the design says, apart from the points above.
