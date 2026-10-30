# CH3: chainstate connect, disconnect, undo and reorg integration (CH3a + CH3b)

| | |
|---|---|
| Rows | PLAN **CH3a** (`connect_block`/`disconnect_block` non-evo, undo, assumevalid, BIP30/34, script flags, value bound, check queue) and **CH3b** (reorg integration, size M per CH1-node Q4) |
| Status | rev2.1, design. Self-reviewed; Opus code-review-validator r1 applied (rev1); manager Sol r1 and Opus r1 applied per the rev2 rulings (§12.1 closes every finding); Sol r2's one Major and Opus r2's two Majors applied (rev2.1, D54, D55), with Opus r2's observations |
| Branch | `node/ch3-design` from node/integration `0d0939b8a` |
| Interfaces read | CH1-node N7b-1 `7ae200d74` (on N7a `22ac83092`): `chain::{seams, abc, failure, locks, flush, load}`; CS6 `f1f32f12c` (`node/cs6-sigcache`): `checkqueue::{mod, script}`; integration: `coins::{overlay, cache}`, `store::guarded`, `recovery::replay`, `dashd-store::flatfile::blocks`, `dash-consensus::{block, tx::{inputs, locks}, subsidy, deployments, ctx, coins::{undo, utxo_hash, muhash}}` |
| Core pin | **v24.0.0-rc.3** (DEC-82), the oracle pin CH1-node and CH2 cite. `v:N` = `src/validation.cpp` line N; other files are named (functional tests are `test/functional/` at the tag). Every rule, order and string below cites it |
| Settled elsewhere, not re-decided | the V actor, holds, lock order, notifications and the ABC/invalidate/precious/CL control flow (CH1-node rev3.1, DEC-158); the coins cache, overlay, flush and recovery protocol (CH2, DEC-95/99/102/131/142); the undo byte format (ST2, CS5a); the attempt/effect contract (EV addendum §2.6, DEC-111/113/117); digest records (O8, O9a); v24 only (DEC-82) |

---

## 0. The design on one page

1. **One connector.** `CoreConnector` (dashd-chainstate `connect/`) implements CH1-node's `BlockConnector` seam. V owns it. It is a line-by-line port of Core's `ConnectBlock` (`v:2469-2935`), `DisconnectBlock` (`v:2282-2378`), `UpdateCoins` (`v:2091-2104`), `ApplyTxInUndo` (`v:2252-2278`), `GetBlockScriptFlags` (`v:2419-2448`) and `TestBlockValidity` (`v:4923-4974`), plus the parts of `ConnectTip`/`DisconnectTip` that sit inside the evo transaction (`v:3427-3449`, `v:3276-3288`). CH7a plugs evo in through an `EvoHooks` trait at ten named hook points, H1–H11 with H3 retired (§3, D40). It never gets a second connector.
2. **Order and strings are Core's.** §2.2 and §2.3 tabulate every step with its Core line, its result class and its reject string. The base slices port literally. Performance levers come later as separate slices, each with a differential test (D4, D22).
3. **Parallel and serial reporting are Core's** (D5). The connect body runs inside CS6's `CheckQueue::control` scope. A non-script failure returns its own reason and the queue verdict is discarded. A script failure surfaces as `block-validation-failed` after the loop when the queue has threads, and as the transaction's `mandatory-script-verify-flag-failed (<err>)` under `-par=1`. §2.4 gives the full precedence matrix.
4. **Undo is Core's `rev*.dat` record, byte for byte** (D11). It is serialized as a stream during the loop, written through ST2's `write_undo` under the node latch, and recorded with `HAVE_UNDO` only after the write returns (T12). The digest's `undo` field (`sha256d` of the record) proves parity per block.
5. **Coins.** Each attempt opens CH2's `BlockOverlay` over its **view target**: the active layer, or a scratch view that is never committed (VerifyDB levels 3–4; CH5 builds it; D36). Reads fall through child → target → F → prefetch → DB, and a read error is `Fatal`, never a verdict. The commit happens inside the connect (Core's `view.Flush()`, `v:3446`), **after the attempt's effects are applied** (D33), and before CH1-node's post-step freeze. The budget is checked only at Core's `FlushStateToDisk` sites (DEC-95).
6. **CS6.** A per-block `Vec<TxSlot>` is built before the control scope (Core's `txsdata`, `v:2648`). The exec cache is probed first, as Core probes it before touching coins. Only on a miss are the spent scripts copied into a per-block append-only arena (Core copies each `CTxOut` into its `CScriptCheck`). That needs two CS6 additions (D41, D43). The store policy: a tip connect consults the caches and does not fill them; `TestBlockValidity` fills both; MP1 fills as CS6 documents. One process-wide `ScriptCaches` (D9, D10).
7. **Evo seams** (§3). `VContext` lands with CH3a as V's affinity marker, and every connect, disconnect and proposal is bracketed by `EvoHooks::begin_attempt`/`finish_attempt`. The effect log is applied in order on every outcome. A `ChainHelper` trait carries exactly O8 §7.1's decision queries. CH3a ships the M3 production helper, an IBD helper for L2, and a recorded helper over O9a's `RecordedDecisions`.
8. **A readiness guard fails closed** (§2.17, D31, D32, D54). It refuses a block, never as an invalid verdict, in two cases:
   - **G1, until CH7a:** on every network, any block that an incomplete evo implementation cannot judge.
   - **G2, until SP1 and GV1b:** on mainnet and testnet, a superblock height that is not yet ChainLocked once the node has left IBD.

   On the tip path the refusal is an `AbortNode`; for `TestBlockValidity` it is an RPC error. Below DIP3, `NoEvo` ports the two evo checks Core runs there: the top-level CbTx check for a coinbase with raw type 5 below version 3 (`bad-cbtx-payload`, D54) and `bad-tx-type-dip3-inactive` (D32). With them and G1, it is exact.
9. **CH3b is integration.** CH1-node already ports ABC, the candidate set, invalidate/reconsider/precious, `EnforceBlock`/`MarkConflictingBlock` and FindMostWorkChain's dirty conflict marks. CH3b adds:
   - Core's kernel `DisconnectedBlockTransactions` with v24's `DynamicUsage(list + unordered_map)` logical budget (CONS-25, D25);
   - proof of conflict-flag persistence across restart (CONS-11, D26);
   - the real-block FT suites;
   - the reorg-across-flush crash test (D27) and the F01 restart case (D28).
10. **Tests.** The tests cover each acceptance column:
    - L2 replay of mainnet 0→1,028,159 and testnet 0→6,999 with `-assumevalid=0` against O8 goldens (`undo`, `muhash`, `txouts`, `amount`, `unspendable`, `bi`, `stats`, header and deployment fields);
      - the running MuHash starts at genesis (a golden hash cannot seed it), so the per-PR run is genesis→625,000 (D34);
      - a reverse leg disconnects back across CLTV, CSV and the superblock start (D48);
    - L3 non-evo mutation operators, run in-process against V, with dashd regtest as the oracle in its not-synced regime (D49);
    - the named FTs: CH3a's five (`feature_block` and four more), which close after RP1a merges, and CH3b's five, `feature_abortnode` closing after CH5 merges (D37, D55);
    - ChainSut's T12, C11 and T20/T21 connect halves;
    - `NodeSut`, the interface O2b's `diff_chain` drives;
    - three CH3 fuzz targets (D48).
11. **Slices.** CH3a has twelve (a1–a12), CH3b four (b1–b4). Each is gate-able alone (§8). a11 and a12 need rows that depend on CH3a (RP1a, O9b), so they are acceptance that closes after those rows merge, as CH3b's FTs already are; CH3a merges on a1–a10 (D55). There are 55 D-rows (§9) and no escalation (§9.1).

---

## 1. Scope

### 1.1 The rows and their acceptance (binding)

| Row | Acceptance (PLAN) | Where |
|---|---|---|
| CH3a | L2 replay of mainnet 0→1,028,159 and testnet pre-DIP3 (UTXO MuHash, `undo_hash`, block stats per block, `-assumevalid=0`); L3 non-evo operators at 100%. BG: `reindex-chainstate-*` counters vs dashd (pre-DIP3 window). Added by CH2 §7.4 (rev2, F5/n1): ST3a T12, C11's connect half and the connect halves of T20/T21, rerun with `connect_block`. **Added by this design (PLAN delta, §11):** the readiness guard and its boundary and restart tests (D31, D32, D54); per-PR L2 from genesis (D34). **Closing after later rows merge** (D55), as CH3b's FTs do: FTs `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation`, `feature_assumevalid`, unmodified, once RP1a merges (D37); L2 segment seeds once O9b merges (D34) | §7.1–§7.5, §2.17, §6.3, §8.1 a11–a12 |
| CH3b | Regtest reorg suites; FT `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `p2p_invalid_block`; the F01 restart case; the reorg-across-flush crash test. **Added (§11):** FT `feature_abortnode`, closing after CH5 merges; b2 runs its reorg half earlier as a regtest process test (D37, D46, D55). BG: none | §7.5–§7.7 |
| (CH5) | **Added (§11):** `ScratchView` and `verifychain 4` (D36) | §2.11 |
| (O9b) | **Added (§11):** snapshots carry Core's serialized `MuHash3072` state for CH3a's a12 (D34) | §2.15 |
| (CH7a) | **Added (§11):** FTs `feature_nulldummy` and `feature_bip68_sequence`, which cross DIP3 (D37); lifting G1 (D31) | §7.5, §2.17 |
| (O2b) | `diff_chain` depends on **i:CH3a** | §7.8 |

Testnet's pre-DIP3 window is 0→6,999 (`DIP0003Height = 7000`, `chainparams.cpp:394`). Mainnet's is 0→1,028,159 (`:185`).

PRD requirements (PLAN's column): CH3a carries FR-CON-19 (ConnectBlock order, §2.2), FR-CON-24 (assumevalid scope, §2.8) and FR-CHS-03 (undo and the coins cache, §2.10–§2.12; the evo half of disconnect is CH7a's). CH3b carries FR-CHS-02 (most-work selection with failed and conflict propagation, §4.2–§4.4), FR-CHS-05 (ChainLock enforcement, §4.5) and FR-CHS-07 (no reorg-depth limit beyond Core's, §7.5).

### 1.2 Brief items → sections

| Brief item | Section |
|---|---|
| 1. CH3a: order, BIP30/34, flags, value bound, undo, assumevalid, CS6, coins/flush epochs | §2 |
| 2. Evo/LLMQ seams and the attempt/effect contract | §3 |
| 3. CH3b: ABC/candidates, disconnect pool, invalidate/reconsider/precious, CL, crash, F01 | §4 |
| 4. Concurrency | §5 |
| 5. Performance | §6 |
| 6. Tests | §7 |
| 7. Slicing | §8 |
| 8. Open questions → D-rows | §9 |

### 1.3 Non-goals

- Evo contents: special transactions, the MN list, CbTx roots, credit pool, MNHF, commitments, rotation snapshots, BLS scheme handling. These are CH7a/CH7b; §3 gives their hook points only. The exceptions are the two pre-DIP3 checks `NoEvo` ports (D32, D54).
- The IBD pipeline (S1/S2 prefetch, W's queue): CH4. VerifyDB levels and `-reindex(-chainstate)`: CH5, including the scratch view that levels 3–4 run on (CH3a lands only the seam's `ViewTarget` parameter, D36). Pruning policy: CH8. Mempool contents and the mempool half of `MaybeUpdateMempoolForReorg`: MP1a/b. Governance and MN sync state: GV and MN rows (§2.7 and D18 state what CH3a assumes until they land).
- The candidate set, ABC, invalidate/reconsider/precious, `EnforceBlock`, `MarkConflictingBlock`, `LoadChainTip`: already ported by CH1-node (§4.1 maps each one). CH3b does not re-port them.

---

## 2. CH3a: the connector

### 2.1 Types and placement (indicative; slice a2 lands them compile-checked)

```rust
// dashd-chainstate/src/connect/mod.rs
pub struct CoreConnector<E: KvEngine, F: FlatFs, H: EvoHooks> {
    state: ChainstateCell<E>,            // D2: the coins cache (+ CH7a's evo window), shared with CH2b's FlushSite
    files: Arc<GuardedFiles<F>>,         // D13: undo write/read under the node latch
    queue: CheckQueue,                   // CS6; serial when -par=1 (script_check_threads)
    caches: Arc<ScriptCaches>,           // D10: one per process, shared with MP1
    helper: Box<dyn ChainHelper>,        // §3.3, D18
    hooks: H,                            // §3.1; NoEvo until CH7a
    digest: Option<Box<dyn DigestSink>>, // D17: O8 records; None in production unless -statedigestlog
    scratch: BlockScratch,               // §2.9, §6.3: arenas and buffers kept across blocks
    assume_valid: Option<BlockHash>,     // -assumevalid (ND1 parses; None = 0)
    abort: Arc<dyn AbortSink>,           // §2.1.1: V's abort path; reports every AbortNode raised inside the ported functions
}

pub enum AttemptOrigin { Tip, Proposal, VerifyDb, Replay }   // O8 §8.2; closes O9a D7 (D17); a digest tag only

/// Where an attempt's changes go (D36). Commit follows the target, never `AttemptOrigin`.
pub enum ViewTarget<'a> {
    Active,                              // tip connects, ABC, invalidate, recovery activation: commit into the active layer
    Scratch(&'a mut dyn ScratchView),    // VerifyDB levels 3–4: commit into the scratch view, never into active, F or prefetch
}
/// Built by CH5 with CH2b's owner (indicative; CH3a fixes only what the connector calls).
pub trait ScratchView {
    /// One split borrow (Opus r2): H4 needs the overlay and the evo transaction at once.
    /// The overlay's commit(best) merges into the scratch layer; the evo transaction is
    /// never committed (Core v:5047).
    fn split(&mut self) -> (BlockOverlay<'_>, &mut dyn EvoTxn);
}

/// The readiness guard's network scope (D31): ND1 derives it from the chain.
pub enum GuardScope { Live /* mainnet, testnet */, Declared /* regtest, devnet */ }

// CH1-node's seam, extended by CH3a (D3). `v` is V's affinity marker (§3.2, D19).
pub struct ConnectCx<'a> {
    pub params: &'a ChainParams,
    pub reader: &'a ChainReader,
    pub deployments: &'a dyn DeploymentView, // CS4 Deployments over the header chain (D21)
    pub best_header: BlockId,                // Core's m_best_header (assumevalid, v:2552)
    pub min_chain_work: Arith256,            // MinimumChainWork() (v:2553)
    pub is_ibd: bool,                        // V's IBD latch; read only by NodeHelper::ibd() (§3.3), never by connector code
    pub chainlocks: &'a dyn ChainLockView,   // V's ClState, for TestBlockValidity's unrecorded pre-check (§2.14)
    pub guard: GuardScope,                   // §2.17
    pub origin: AttemptOrigin,
}
pub trait BlockConnector: Send {
    /// `checked`: Core's `fChecked` memo (D42): true only for the object ProcessNewBlock's CheckBlock passed.
    fn connect_block(&mut self, cx: &ConnectCx<'_>, v: &mut VContext, target: ViewTarget<'_>, block: &Block,
                     checked: bool, id: BlockId, st: &mut BlockStatusWriter<'_>) -> Result<ConnectOutcome, Fatal>;
    fn disconnect_block(&mut self, cx: &ConnectCx<'_>, v: &mut VContext, target: ViewTarget<'_>, block: &Block,
                        id: BlockId) -> Result<DisconnectResult, Fatal>;      // D15: was bool
    /// Core TestBlockValidity (v:4923-4974) on the tip; `prev` must be the tip.
    fn test_block_validity(&mut self, cx: &ConnectCx<'_>, v: &mut VContext, block: &Block,
                           prev: BlockId, check_pow: bool, check_merkle: bool)
                           -> Result<ConnectOutcome, Fatal>;                    // D16
}
pub enum DisconnectResult { Ok, Unclean, Failed }                              // validation.h:405-409
// ConnectOutcome is CH1-node's: Connected | Invalid(BlockValidationError) | SystemError(String).
```

**Three failure kinds, never mixed** (CH2 §2.4.4 rule 1):

| Kind | Core | Ours | V's response (CH1-node) |
|---|---|---|---|
| Invalid | `state.Invalid(result, reason, debug)` | `Ok(ConnectOutcome::Invalid(e))` | `invalid_block_found` (`v:2077-2089`) on a tip connect, the batch-top `InvalidChainFound` (`v:3648-3650`) unless MUTATED |
| System error | `AbortNode(state, msg)`, i.e. `state.Error` | `Ok(ConnectOutcome::SystemError(msg))` with Core's text, already reported by the connector (§2.1.1). The readiness refusals (§2.17) are system errors with rdashd's own text | `block_checked` with the error state (`v:3432`; only `IsInvalid()` punishes a peer, `net_processing.cpp:2323-2326`), then ABC returns false and the node shuts down |
| Storage fault | `CCoinsViewErrorCatcher` → `std::abort()` (`coins.cpp:356-380`) | `Err(Fatal)` from any `CoinsReadError`/`CoinsError::Read`/latch refusal, an impossible-state assertion (§2.3 L7, §6.2 L1) | V stops through CH1-node's `Fatal` path; no verdict, **no `block_checked`** (Core aborts before it), nothing published (CH2 FA7). This needs a CH1-node change: today `connect_tip` emits `block_checked` for `Err(Fatal)` too (`abc.rs:538-547`; §11) |

#### 2.1.1 Who reports an `AbortNode` (exactly Core's count and text)

`GuardedFiles::mutate` trips the latch when its operation fails and leaves the report to its caller (`store/guarded.rs:455-470`). CH1-node's `stop_serving` calls V's `abort_node` only if `FlushSite::abort` says the latch had not already tripped (`actor.rs:886-895`, `flush.rs:72-78`). **One rule** (D52):
- **The connector reports every `AbortNode` raised inside the Core functions it ports** (`ConnectBlock`, `DisconnectBlock`, `WriteUndoDataForBlock`), and the readiness refusals on the tip path (§2.17). It reports through its `AbortSink` at Core's position, before any line rdashd logs after the call returns. The sink is V's abort path: it logs Core's text once per call and trips the node latch if it has not tripped.
- **V reports those raised in `ConnectTip`, `DisconnectTip` and ABC.**

A connector `SystemError` is therefore always already reported, and `stop_serving` finds the latch tripped and stays silent.

| Path | Core's `AbortNode` calls | Ours |
|---|---|---|
| `CheckBlock` MUTATED in `ConnectBlock` (`v:2510`) | 1 | the connector reports, then `SystemError`; V stays silent |
| `ConnectBlock`'s evo best-block check (`v:2530`) | 1 | the same |
| undo `FindUndoPos` out of space ("Disk space is too low!", `blockstorage.cpp:726-742`) | 1 | the same (`GuardedFiles` tripped the latch; the sink still logs) |
| undo write failure ("Failed to write undo data", `blockstorage.cpp:783-785`) | 1 | the same |
| undo write refused because another store tripped the latch first | 0 (no Core analogue) | `Err(Fatal)`; nobody reports again |
| readiness refusal G1/G2 on the tip path (§2.17) | — (rdashd only) | the connector reports rdashd's text, then `SystemError` |
| readiness refusal in `TestBlockValidity` | — | not reported; the RPC returns the error (§2.14) |
| `DisconnectBlock`'s evo check during ABC (`v:2290`, then "Failed to disconnect block; see debug.log for details" at `v:3621`) | 2 | the connector reports the first and returns `Failed`; CH1-node's ABC reports the second (`abc.rs:465`) |
| the same during `InvalidateBlock` (`v:2290`; `v:3965` only returns false) | 1 | the connector reports; CH1-node reports nothing (`failure.rs:42-53`) |

Slice a3 (connect and refusal rows) and a5 (disconnect rows) each add a fault-injection test that asserts the exact sequence of sink calls and texts per row.

### 2.2 `connect_block`, step by step

Core's numbering. "Result" is the `BlockValidationResult` and reject string. Everything not marked as a hook is CH3a's code.

| # | Core | What | Result / string | Ours |
|---|---|---|---|---|
| 1 | `v:2478-2480` | block hash equals the index hash; `parallel = scriptcheckqueue.HasThreads()` | assert | `debug_assert`; `self.queue.has_threads()` |
| 2 | `v:2490-2491` | BLS scheme guard: legacy iff V19 is not active at `pindex` | — | **hook H1** `bls_ctx` (CH7a); CH3a computes `bls_ctx_for_block(params, h)` and passes it in (A1 §1.4.1). Nothing is global |
| 3 | `v:2505-2514` | `CheckBlock(block, fCheckPOW = !fJustCheck, fCheckMerkleRoot = !fJustCheck)`, which returns at once if `block.fChecked` (`v:4374-4375`, set after a pass with both checks on, `v:4424-4425`) | CS3's error, as is; `MUTATED` → `AbortNode("Corrupt block found indicating potential hardware failure; shutting down")` | `dash_consensus::block::check_block(block, hp, !just_check, !just_check)`, skipped when `checked` (D42); MUTATED → the connector reports that text, then `SystemError` |
| 4 | `v:2516-2519` | `pprev && HasConflictingChainLock(h, hash)` | `CHAINLOCK`, `bad-chainlock` | `helper.cl_conflict(h, hash)` (O8 `cl_conflict`) |
| 5 | `v:2522-2523` | view's best block == prev hash | assert | `debug_assert_eq!(overlay.best_block(), prev)`; a mismatch in release builds is `Fatal` (a logic error, never a verdict) |
| 5a | — (rdashd only) | readiness guard **G1**: an incomplete `EvoHooks` and an evo rule live at the block | refusal: system error, never a verdict | §2.17 (D31, D32, D54) |
| 6 | `v:2525-2531` | DIP0003 active at `pindex` ⇒ evo best block == prev, else `AbortNode(EvoDbInconsistencyMessage())` | system error | **hook H2** `verify_best_block(prev)`; `NoEvo` answers true. With CH2's atomic B1 it cannot fail (A1 §2.10 step 4), but the check and its text stay |
| 7 | `v:2532` | `num_blocks_total++` | — | bench counter (`-debug=bench` averages) |
| 8 | `v:2536-2540` | genesis: `if !fJustCheck view.SetBestBlock(hash)`; return true | — | commit the empty overlay with `best = genesis`; **no undo, no `VALID_SCRIPTS`** (Core never raises genesis to it; CH1-node's `NullConnector` matches). The digest record has null `undo`/`mnl`/`qc` (O8 §5) |
| 9 | `v:2542-2571` | assumevalid decision → `fScriptChecks` | — | §2.8 |
| 10 | `v:2590-2612` | BIP30 | `CONSENSUS`, `bad-txns-BIP30` | §2.5 |
| 11 | `v:2617-2622` | superblock start hash at `nSuperblockStartBlock` (when the hash is set) | `CONSENSUS`, `bad-sb-start` | pure check on params; mainnet only (`chainparams.cpp:171-172`; testnet sets none, `:381`) |
| 12 | `v:2626-2632` | `nLockTimeFlags` (CSV active at `pindex`), script `flags` | — | §2.6 |
| 13 | `v:2641-2656` | `blockundo`, `txsdata(vtx.size())`, `CCheckQueueControl(fScriptChecks && parallel ? &queue : nullptr)`, counters, `blockundo.vtxundo.reserve`, `fDIP0001Active_context` | — | the scratch reset; `slots: Vec<TxSlot>` built here (§2.9); the `control` scope opens here and encloses steps 14–17. Core creates no control when `!fScriptChecks`. Ours then uses `CheckQueue::control_inline`, a CS6 addition (D41; today `control` always enters the pool's scope), so no rayon scope is entered below assumevalid. This is performance only: nothing is queued either way |
| 14 | `v:2658-2659` | `blockSubsidy = GetBlockSubsidy(pindex)`; `special_tx_rules = GetSpecialTxRules(pprev)` | — | `dash_consensus::subsidy::block_subsidy(prev_ctx, v20_at_block, p)`. `SpecialTxRules { v24, evo_shares }` is computed by CH3a from `cx.deployments.active_after(prev, …)` (`GetSpecialTxRules`, `v:6797-6803`), as is `MnRewardEra` from V20 and MN_RR (`GetMnRewardEraAfter`, `v:5897-5903`, used at step 20). Both are inputs to hooks H4, H5 and H8; neither is a hook (D40) |
| 15 | `v:2661-2678` | `ProcessSpecialTxsInBlock(… view, blockSubsidy, fJustCheck, fScriptChecks …)` **before** the UTXO loop; a failure logs "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block %s failed with %s" (`v:2666`); on success with `!fJustCheck`, `RecordBackgroundMNListHash` | its state, as is | **hook H4** `process_special_txs` over the overlay (empty, so it sees `pprev`'s state: CH2 §2.4.2 "why the overlay starts empty") and an `EffectSink`. `NoEvo` is not a no-op: below DIP3 it ports Core's two live checks in Core's order, the top-level CbTx check for a raw-type-5 coinbase below version 3 (`bad-cbtx-payload`, D54) and then `bad-tx-type-dip3-inactive` (D32; §2.17). `RecordBackgroundMNListHash` is assumeutxo-only; rdashd has no background chainstate (O8 §8.4) |
| 16 | `v:2687-2768` | the transaction loop | §2.3 | §2.3 |
| 17 | `v:2778-2781` | `control.Wait()` false | `CONSENSUS`, `block-validation-failed` | `(r, all_ok) = queue.control(...)`; `r` is `Ok` and `!all_ok` ⇒ `check_queue_failed()` (CS6) |
| 18 | `v:2801-2819` | IS conflicts, outside IBD only: `has_chainlock = HasChainLock(h, hash)`; for each tx with `HasLockInputs`: while `ConflictingISLockIfAny(tx)`: with a CL, `RemoveConflictingISLockByTx` (an effect); without, reject | `CHAINLOCK`, `conflict-tx-lock` | ported in CH3a (D20, D35): `helper.ibd()` decides, then `helper.cl_has`, then `helper.is_conflict(tx)` per iteration, which returns the lock. With a CL, `helper.remove_conflicting_is_lock(&lock, sink)` acts on **that** lock. Core's `RemoveConflictingISLockByTx` re-queries internally and gets the same lock (`evo/chainhelper.cpp:93-99`); O8 records only the loop condition. The call emits `Effect::RemoveConflictingIsLock` (A1 §2.10.1) and adds Core's removal closure to an attempt-local removed view: the lock and its chained descendants (`RemoveChainedInstantSendLocks`, `instantsend/instantsend.cpp:339-350`). `is_conflict` excludes that view, so the `while` loop advances exactly as Core's synchronous removal makes it (§3.3). One transaction can meet several locks, one per input. `has_lock_inputs` is `!coinbase && (!vin.empty() || asset_unlock)` (`instantsend/lock.cpp`) |
| 19 | `v:2831-2850` | `check_superblock` (`v:2841-2843`); `IsBlockValueValid(…, blockSubsidy + nFees, …)` | `RESULT_UNSET`, `bad-cb-amount` | readiness guard **G2** first (§2.17); then §2.7 |
| 20 | `v:2859-2865` | `IsBlockPayeeValid(…, blockSubsidy, nFees, era, v24, check_superblock)` | `RESULT_UNSET`, `bad-cb-payee` | §2.7; the MN-payment half is **hook H5** |
| 21 | `v:2883-2884` | `if fJustCheck return true` | — | drop the overlay; apply effects (§3.2); `Connected` |
| 22 | `v:2888-2890` | `WriteUndoDataForBlock`: only if the undo position is null; `FindUndoPos` out of space → `AbortNode("Disk space is too low!")`; a write failure → `AbortNode("Failed to write undo data")`; then `nUndoPos`, `HAVE_UNDO`, dirty (`blockstorage.cpp:773-803`) | system error | §2.10 |
| 23 | `v:2899-2902` | raise to `VALID_SCRIPTS` if below it, dirty | — | `st.raise_validity(VALID_SCRIPTS)` (CH1-node §4.8) |
| 24 | `v:2912-2913` | `view.SetBestBlock(hash)`; evo `WriteBestBlock(hash)` | — | the overlay's `commit(best)` (it sets the best block, CH2 DEC-142 O4) into the view target happens after the attempt's effects are applied (§2.13, D33, D36); **hook H6** `set_best_block` |
| 25 | `v:2916` | `bls_scheme_guard.Commit()` | — | hook H1 (CH7a) |
| 26 | `v:2918-2921` | active chainstate and a list diff with changes ⇒ `NotifyMasternodeListChanged(false, …)` | — | **hook H7** returns the list update; CH7a stages the signal (N1) under D47's ordering rule (§2.13) |

**Order inside `connect_block`** is the table's. The overlay's commit and the evo transaction's commit are Core's `view.Flush(); dbTx->Commit();` in `ConnectTip` (`v:3446-3448`). The connector runs them after step 26 **and after `finish_attempt` has applied the attempt's effects** (D33), into the view target (D36). In Core every effect has already executed inside `ConnectBlock` by then (§2.13).

### 2.3 The transaction loop (`v:2687-2768`), per transaction `i`

| # | Core | What | Result / string | Ours |
|---|---|---|---|---|
| L1 | `v:2692` | `nInputs += vin.size()` | — | counter (digest `stats.ins`, `-debug=bench`) |
| L2 | `v:2698-2703` | non-coinbase: `CheckTxInputs(tx, view, h, txfee)` (`consensus/tx_verify.cpp:163`) | `CONSENSUS`, the tx's reason and debug (`bad-txns-inputs-missingorspent`, `bad-txns-premature-spend-of-coinbase`, `bad-txns-inputvalues-outofrange`, `bad-txns-in-belowout`, `bad-txns-fee-outofrange`, …) | `overlay.have_inputs(tx)?` first, **except for an asset-unlock transaction**, for which `CheckTxInputs` returns before `HaveInputs` (`tx_verify.cpp:165-167`). The call exists only for Core's caching in the parent (`coins.cpp:303-313`); its `bool` is ignored, because CS5b's `check_tx_inputs(tx, &overlay, h)`, called next, checks presence itself and produces `bad-txns-inputs-missingorspent` (`dash-consensus/src/tx/inputs.rs:57-63`). `ViewOr::View` → `Fatal`; `ViewOr::Reject(e)` → `e.into_block(Consensus)` |
| L3 | `v:2708-2716` | V24 active at `pindex`: `CheckSharedCollateralSpends(tx, view)` | `CONSENSUS`, its reason | **hook H8** (EV2's rule, `EV2-dml.md` §3.5), called when CH3a's `SpecialTxRules::v24` holds (D40). Under `NoEvo` it is unreachable: V24 at the block is a G1 refusal (§2.17) |
| L4 | `v:2717-2721` | `nFees += txfee`; `!MoneyRange(nFees)` | `CONSENSUS`, `bad-txns-accumulated-fee-outofrange` | `Amount` checked add + range |
| L5 | `v:2726-2734` | `prevheights[j] = AccessCoin(prevout).nHeight`; `SequenceLocks(tx, nLockTimeFlags, prevheights, *pindex)` | `CONSENSUS`, `bad-txns-nonfinal` | CS5b `sequence_locks(tx, flags, &mut prev_heights, &ctx)`; `prev_heights` is a scratch `Vec<u32>` kept across transactions |
| L6 | `v:2740-2744` | `nSigOps += GetTransactionSigOpCount(tx, view, flags)`, all transactions, coinbase included; `> MaxBlockSigOps(fDIP0001Active_context)` | `CONSENSUS`, `bad-blk-sigops` | CS5b `transaction_sigop_count(tx, &overlay, flags)`; `max_block_sigops(dip1)` (`consensus/consensus.h:17`) |
| L7 | `v:2746-2761` | non-coinbase: `CheckInputScripts(tx, flags, fCacheResults = fJustCheck, fCacheResults, txsdata[i], parallel ? &vChecks : nullptr)` if `fScriptChecks`; then `control.Add(vChecks)` | serial: `CONSENSUS`, the tx's reason (`mandatory-script-verify-flag-failed (<err>)`) and debug message, plus the "ConnectBlock(): CheckInputScripts on %s failed with %s" log | CS6's probe-first pair (§2.9, D43): `exec_cache_probe(&slots[i], &opts)` as Core probes at `v:2174`, before its first `AccessCoin` (`v:2184`). On a hit, nothing is copied or queued. On a miss, the spent scripts are copied, then `check_input_scripts_unprobed(&slots[i], spent, &opts, Some(control))`. `Err(CheckInputsError::Script(f))` → `input_scripts_failed(f)`; `Err(CheckInputsError::Flags(_))` is Core's `IsValidFlagCombination` assert, so `Err(Fatal)`, never a verdict |
| L8 | `v:2763-2767` | `UpdateCoins(tx, view, i == 0 ? undoDummy : blockundo.vtxundo.back(), h)` | — | §2.3.1 |

#### 2.3.1 `UpdateCoins` (`v:2091-2104`) and `AddCoins` (`coins.cpp:118-128`)

- Non-coinbase: for each input in `vin` order, `SpendCoin(prevout, &undo)` (`coins.cpp:129-149`), asserted spent. Ours: `overlay.spend(key)?`. `None` is Core's failed assert, so it is `Fatal` (a logic error; `CheckTxInputs` just proved presence). The coin is appended to the streamed undo (§2.10.1).
- Then `AddCoins(view, tx, h)` with `check_for_overwrite = false`: each output is added with `possible_overwrite = is_coinbase` (`coins.cpp:118-128`). The unspendable skip is `AddCoin`'s (`coins.cpp:70-75`); CH2's `add` does it in `CoinSrc::pack`. Ours: `overlay.add(key, CoinSrc::Output { out, height, coinbase }, coinbase)?`.
- The coinbase's spends are none, and its `CTxUndo` is the dummy (`v:2763-2766`): no undo entry for transaction 0.

### 2.4 Failure precedence under the check queue (D5)

Core's `CCheckQueueControl` with threads queues the checks and reports only at `Wait` (`v:2778`). An early `return` from the loop destroys the control, and its destructor waits and ignores the verdict (`checkqueue.h`; CS6 `CheckQueue::control` mirrors this: the closure's `Err` wins, `all_ok` is discarded). Without threads (`-par=1`, or `script_check_threads` = 0), `CheckInputScripts` runs inline with `pvChecks == nullptr` and reports at the transaction.

| Defects in one block (tx order) | Threads (`parallel`) | `-par=1` |
|---|---|---|
| script failure in tx *i* only | `block-validation-failed` after the loop | `mandatory-script-verify-flag-failed (<err>)` at *i* |
| script failure in *i*, then any L2–L6 failure in *j* > *i* | *j*'s reason (`bad-txns-nonfinal`, `bad-blk-sigops`, …) | *i*'s script reason |
| L2–L6 failure in *j* < *i*, script failure in *i* | *j*'s reason | *j*'s reason |
| sigops over the limit at tx *i* whose own scripts also fail | `bad-blk-sigops` (L6 precedes L7 for the same tx) | `bad-blk-sigops` |
| script failure + `conflict-tx-lock` (step 18) | `block-validation-failed` (Wait precedes the IS loop) | the script reason |
| script failure + `bad-cb-amount`/`bad-cb-payee` | `block-validation-failed` | the script reason |
| script failure + hook H4 failure (special txs run before the loop) | H4's reason | H4's reason |
| script failure below assumevalid (`fScriptChecks` false) | none: scripts skipped; later checks decide | the same |

The result class is `CONSENSUS` in every script row. `fCacheResults` never matters to the verdict. A parallel run never inserts into the script-execution cache (CS6 D14), whatever the store flag. The matrix is slice a4's directed test (§7.1), run at `-par=1` and at `-par=4`. L3's multiple-failure operators pin it against dashd (A1 §4.7, "including script failures under `-par=1` vs parallel").

We do **not** make parallel mode report the specific script reason. That would be stricter than Core, and peers see `BLOCK_CONSENSUS` either way (the penalty is the same).

### 2.5 BIP30 and BIP34 (D6)

- **No exempt heights.** Dash's `IsBIP30Repeat` returns false: "Dash blockchain does not have any BIP30 violations" (`v:6785-6789`). Bitcoin's two exempt heights (91842/91880) do not exist here. The port is `const fn is_bip30_repeat(_: …) -> bool { false }`, kept as a named function so the quirk census can find it.
- **The BIP34 shortcut** (`v:2598-2601`): `pindexBIP34height = pprev->GetAncestor(BIP34Height)`. BIP30 is enforced unless that ancestor exists and its hash equals `BIP34Hash`.
  - Mainnet: height 951, `0x000001f35e…a8012` (`chainparams.cpp:178-179`), so blocks above 951 on the real chain skip the probe.
  - Testnet: 76 (`:387-388`).
  - Devnet: `BIP34Height = 1` and no hash (`:570`), so always enforced. Regtest likewise (the hash is null), as A1 §2.10 step 7 says.
- **No upper limit.** Dash's code has no `BIP34_IMPLIES_BIP30_LIMIT` (Bitcoin's height 1,983,702 re-enable): above the BIP34 block on the known chain the probe is skipped for good (`v:2598-2601`). The port has no such constant either.
- **The probe** (`v:2603-2612`): for every tx and every output index, `view.HaveCoin(COutPoint(txid, o))`; the first hit logs "ERROR: ConnectBlock(): tried to overwrite transaction" and rejects with `bad-txns-BIP30`. Ours uses `overlay.have_coin(&op)?`, which caches nothing on a miss (CH2 §2.4.2) and is an error-propagating read. The probe includes unspendable outputs and the coinbase, as Core's does.
- **BIP34 itself** (the coinbase height prefix) is `ContextualCheckBlock`'s (CS3 `contextual_check_block`, "the coinbase height prefix from BIP34 until DIP0003", quirk 10). `ConnectBlock` does not re-run it (`v:2494-2504`). CH3a re-runs it only inside `TestBlockValidity` (§2.14), where Core does.
- **Disconnect** uses `fEnforceBIP30 = true` (`v:2322`), so the coinbase "BIP30 exception" branch of the output check never fires (§2.12).

### 2.6 Script and lock-time flags

`GetBlockScriptFlags` (`v:2419-2448`) is a pure function of deployments **at** `pindex` (`DeploymentActiveAt`, not `…After`):

| Flag | Condition | Mainnet height |
|---|---|---|
| `P2SH` | always (`v:2425`) | 0 |
| `DERSIG` | `DEPLOYMENT_DERSIG` (BIP66) | 245,817 |
| `CHECKLOCKTIMEVERIFY` | `DEPLOYMENT_CLTV` (BIP65) | 619,382 |
| `CHECKSEQUENCEVERIFY` | `DEPLOYMENT_CSV` | 622,944 |
| `NULLDUMMY` | `DEPLOYMENT_BIP147` | 939,456 |

Heights are from `chainparams.cpp:180-183`. Every block flag is in `MANDATORY_SCRIPT_VERIFY_FLAGS` (`script/standard.h`), so a serial failure always classifies as `mandatory-…` (CS6's `input_scripts_failed`). `nLockTimeFlags = LOCKTIME_VERIFY_SEQUENCE` iff CSV is active at `pindex` (`v:2626-2629`).

Ours: `pub fn block_script_flags(d: &dyn DeploymentView, id: BlockId) -> ScriptFlags` and `lock_time_flags(d, id)` in `dash-consensus` (slice a1), table-tested at each boundary height ±1 on all four networks.

### 2.7 Subsidy, fees and the value bound (D7)

Dash does not use Bitcoin's `block.vtx[0]->GetValueOut() > blockReward` check (`bad-cb-amount` with `BLOCK_CONSENSUS`). The bound is `IsBlockValueValid(…, blockSubsidy + nFees, strError, check_superblock)` (`v:2846-2850`). A failure is `BLOCK_RESULT_UNSET` `bad-cb-amount`: not punished, still `FAILED_VALID`, cleared by `reconsiderblock` (FR-CON-32).

**`check_superblock`** (`v:2841-2843`): `NoCheck` unless `IsSuperblockValidationRequired(pindex)`. That function returns false when the best CL height ≥ h, or when MN sync is not finished (`evo/chainhelper.cpp:41-52`). Otherwise it is `DisallowDuplicates` under V24's special-tx rules, else `AllowDuplicates`.

**Where the code lives** (D7). PLAN gives payee/value validation to **EV3**, in `dash-evo::payments` (`PLAN.md:589`), with O2a-GV's `oracle_block_value_payee` as its oracle (`:650`). CH3a's L2 needs the bound at M3, before EV3. So CH3a writes the **governance-free branches** of both functions into EV3's module, `dash-evo::payments`, with EV3's owner reviewing. EV3 then adds `GetBlockTxOuts`, the MN payee check and the trigger branches to the same functions. That is a PLAN delta (§11). O17's MNO-12 (the old-budget window) has one behaviour under DEC-82, v24's, which is the one cited below.

**`IsBlockValueValid`** (`masternode/payments.cpp:313-387`), ported whole as `dash_evo::payments::block_value_valid` (slice a1). It reads governance only through three of the helper's answers (`gov_valid`, `sb_triggered`, `sb_payments`; O8 §7.1). In order:
1. h < `nBudgetPaymentsStartBlock` (mainnet 328,008): `value_out ≤ reward`, else the "old budgets are not activated yet" error (`:322-329`).
2. h < `nSuperblockStartBlock` (mainnet 614,820): `IsOldBudgetBlockValueValid` (`:262-299`).
   - In the budget window (`h % cycle < window`; mainnet 16,616 and 100), `NoCheck` accepts **any** value. That is A1's leniency; mainnet block 332,320 overpays about 1,176 DASH.
   - Otherwise `value_out ≤ reward`, with Core's two error texts.
3. From the superblock start, the max bound is `reward + GetPaymentsLimit(h)` (CS2 `superblock_payments_limit`).
   - Off superblock heights (`CSuperblock::IsValidBlockHeight`, `governance/superblock.cpp:119-124`), `value_out ≤ reward`.
   - On them: the max bound, then `!gov_valid` ⇒ true, then `NoCheck` ⇒ true, then the trigger/`IsValidSuperblock` branch (EV3 with GV1b).

`strError` texts go to the log only. The reject string is always `bad-cb-amount`.

**`IsBlockPayeeValid`** (`payments.cpp:392-435`) has two halves:
- `IsTransactionValid` returns true below `DIP0003EnforcementHeight` (mainnet 1,047,200; `payments.cpp:227-234`, "can't verify historical blocks here"). Above that height it is EV3's `GetBlockTxOuts` and payee check, reached through **hook H5** (CH7a wires it).
- The superblock half: `!gov_valid` ⇒ true; below the superblock start ⇒ true; `NoCheck` ⇒ true; else the trigger branch (EV3 with GV1b).

CH3a ports the control flow and both height gates. The governance-backed calls answer through the helper. **Acceptance:** a1's L0 tables cover every branch with scripted answers, and L3's coinbase-value operators check verdicts against dashd (§7.3). Once O2a-GV lands, `oracle_block_value_payee` runs on the same tables with the sync flag off and on and no governance (a CH3a acceptance item marked i:O2a-GV, §11).

**What M3 judges with.** Until the MN-sync row (SP1) and the governance runtime (GV1b) land, both at M5, the production helper answers `sb_required = false`, because MN sync is "not finished" (D18). So every block is judged as Core judges it before its MN sync completes, or during IBD and reindex, which is the regime L2 replays. This is not an approximation of Core's outcome *given that input*. The input itself is node-local state that Core also varies. The differences that remain, all on blocks whose best ChainLock is below them:
- a Core node whose MN sync has finished rejects an over-paying block in a 12.0 budget window (`IsOldBudgetBlockValueValid` needs no governance data, only a check type other than `NoCheck`);
- a Core node that has also loaded governance rejects a block above `reward` at an untriggered superblock height, and checks triggered superblocks' payees.

rdashd accepts both until then. Where they can arise:
- The first is confined to heights below the superblock start (mainnet 614,820, testnet 4,200): deep history on mainnet and testnet, so in practice regtest and devnet forks.
- The second is not. At a **live tip**, Core with MN sync finished checks every superblock height above its best ChainLock (`evo/chainhelper.cpp:41-52`): every 16,616 blocks on mainnet and every 24 on testnet (`chainparams.cpp:173`, `:382`), until that block is ChainLocked.

Replays still match because Core itself judges with `NoCheck` there: IBD and reindex never finish MN sync first, and the recorded FT chains carry Core's own `sb_required` answers (§3.3).

**The live-tip difference is refused, not just declared** (D31). On mainnet and testnet, once the node has left IBD, readiness guard G2 refuses every superblock height above the best ChainLock (§2.17), which is exactly the set where a synced Core could answer differently. At non-superblock heights the synced and unsynced verdicts coincide (`payments.cpp:313-387`, `:392-440`). Old-budget heights lie below the superblock start and are refused too, at no cost. What remains declared (D18) is the regtest and devnet regime, plus the decision-stream and log differences that `gov_valid = false` causes (D18). Both close with SP1 and GV1b.

**Subsidy.** `GetBlockSubsidy(pindex)` (`validation.cpp:1884`) → CS2's `block_subsidy(prev, v20_at_block, p)`; the genesis subsidy is CS2's `genesis_reward`.

### 2.8 Assumevalid: the decision and its scope (D8)

**Decision** (`v:2542-2571`). Script checks are skipped iff all of these hold:
1. an assumevalid hash is configured, and it is in the block index;
2. `assumevalid_block.GetAncestor(h) == pindex`;
3. `best_header.GetAncestor(h) == pindex`;
4. `best_header.nChainWork >= MinimumChainWork()`;
5. `GetBlockProofEquivalentTime(best_header, pindex, best_header, params) > 2 weeks` (`chain.cpp:147`).

When all hold, `fScriptChecks = (equivalent time ≤ 1209600)`, which is false.
- `-assumevalid=0` disables it, and `defaultAssumeValid` is per chain (mainnet `chainparams.cpp:231`).
- Slice a1 adds the missing pure function `block_proof_equivalent_time` to dash-consensus (`chain.cpp:147-160`; it uses `GetBlockProof` and `nPowTargetSpacing`).
- The best header is SB4/CH1-node's `m_best_header` analogue, passed in `ConnectCx::best_header`. `min_chain_work` is dash-chain's config (`-minimumchainwork`, `dash-chain/src/config.rs:36`).
- **Tests** (D44): a1's directed decision table flips each of conditions 1–5 alone and expects `fScriptChecks` on. §7.2's assumevalid-on L2 run preloads the header chain so that the skip is really taken, and asserts how many blocks skipped. CH3a's FTs include `feature_assumevalid` (§7.5).

**Scope (the m-1 set; A1 §2.10 step 6).** `fScriptChecks` does two things:
- It gates `CheckInputScripts` (L7). Nothing is queued, and the sigcache and script-exec cache are not consulted.
- It is passed to `ProcessSpecialTxsInBlock` as `fCheckCbTxMerkleRoots` (`v:2664`). That skips `CheckCbTx` entirely (presence, version, height, credit pool, MN-list and quorum roots, best CL), ProTx signatures and QC BLS signatures.

It still checks QC structure and `IsValid()`, the asset-unlock quorum signature (`assetlocktx.cpp:231`), the MNHF signature (`mnhftx.cpp:152`) and eager payload decoding.

CH3a passes `script_checks` to hook H4 as `check_cbtx_merkle_roots` and does nothing else with it. The m-1 list's evo half is CH7a's acceptance, and its quirk test is that `check_sigs` must not gate the AU and MNHF signatures. **Nothing else** depends on the decision: BIP30, sequence locks, sigops, fees and the value bound all still run, as in Core.

### 2.9 CS6 integration (D9, D10)

**Arena per block.** In Core:
- `txsdata` is a `std::vector<PrecomputedTransactionData>(vtx.size())`, sized once so queued checks can hold pointers into it (`v:2643-2648`);
- each `CScriptCheck` holds a **copy** of the spent `CTxOut`, because `UpdateCoins` spends the coin before the queue runs.

Ours:

```rust
// inside connect_block, before the control scope (indicative)
let slots: Vec<TxSlot<'_>> = block.txs().iter().map(TxSlot::new).collect();  // Core's txsdata; TxSigData built lazily (CS6 D24)
scratch.spent.reset();                                    // SpentScripts: a bumpalo::Bump kept across blocks (D51)
let run = |control| -> Result<Step, Fatal> {
    // ... steps 14–16; for tx i at L7, when script_checks:
    //   let Some(miss) = exec_cache_probe(&slots[i], &opts) else { continue };  // hit: soft-erased when !store; no copy (D43)
    //   spent_i.clear(); for each input j: spent_i.push(scratch.spent.alloc_slice_copy(overlay.coin(&prevout)?.script_bytes()));
    //   check_input_scripts_unprobed(miss, |j| spent_i[j], &opts, Some(control))   // the miss's slot, flags, entry (CH3a D130)
};
let (r, all_ok) = if script_checks { self.queue.control(run) } else { self.queue.control_inline(run) };  // D41
```

- `slots` lives outside the scope, so `&'s TxSlot<'s>` is valid for every queued `ScriptCheck<'s>`. The `TxSigData` is built at the first exec-cache miss (Core's `txdata.Init`, `v:2179`). A coinbase or an exec-cache hit builds none.
- **`SpentScripts`** holds raw `scriptPubKey` bytes. Compressed scripts are decompressed into it, because CH2's `PackedCoin` stores Core's compressed form. Each push returns `&'s [u8]` that is stable until `reset`.
  - **It is a `bumpalo::Bump`** (D51). `alloc_slice_copy(&self, …) -> &[u8]` is push-only, chunked and address-stable. `reset(&mut self)` runs only after the control scope ends, which the borrow checker enforces. `reset` keeps the largest chunk, so at steady state there is no allocation per input, and the crate adds no `unsafe` of ours.
  - If dependency review refuses the crate, the fallback is a bespoke chunked arena (CH2's `StableBytes` pattern extended to 256 KiB chunks, Miri under Stacked and Tree Borrows, CH2's owner reviewing).
- **Scripts are copied only on an exec-cache miss** (D43). Core probes the exec cache before it touches coins (`v:2174`, then `AccessCoin` at `v:2184`), so on a hit it does no coin lookup and no copy. CS6's `check_input_scripts` probes inside itself today. A copy made before calling it would add, on every hit, a lookup, a decompression and a copy per input; and a separate probe followed by the full call would soft-erase twice. So CS6 gains a pair (§11):
  - `exec_cache_probe(slot, opts) -> Option<ExecMiss>`, which soft-erases on a hit when `!cache_full_script_store`, as Core's `contains(…, erase)` does; the miss carries the slot, the flags and the hashed entry (CH3a D130);
  - `check_input_scripts_unprobed(miss, …)`, the same checks without the probe, inserting the miss's entry.

  Below assumevalid (`fScriptChecks` false) there is neither probe nor copy, as in Core.
- A spent coin created earlier **in the same block** (a chained spend) is still read through the overlay and copied. Borrowing it from `block` would save the copy but needs a second code path; it is left as a lever (§6.2 L5) only if PF0 shows chained spends matter.

**`ScriptOptions` for blocks:**
- `flags` = block flags (§2.6); `policy` = consensus-only (CS6's block policy);
- `caches = Some(&self.caches)`;
- `cache_sig_store = cache_full_script_store = just_check` (`v:2750`, CS6's docs).

So:

| Caller | `cache_sig_store` | `cache_full_script_store` | Effect |
|---|---|---|---|
| tip connect (`ConnectTip`, reorgs, IBD, reindex, replay, VerifyDB level 4) | false | false | consult both caches; an exec-cache hit is **soft-erased** (CS6 `contains(entry, erase = true)`); nothing inserted |
| `TestBlockValidity` (GBT proposal, `CreateNewBlock`) | true | true | consult and insert (serial runs only insert into the exec cache; CS6 D14) |
| MP1a ATMP (owned by MP1a; listed so the policy is in one place) | true under standard flags; then true/true under the tip's block flags (`CheckInputsFromMempoolAndCache`) | — | a mempool-validated tx hits the exec cache at its block's connect, which erases it |

**One `ScriptCaches` per process** (D10), sized from `-maxsigcachesize` as Core splits it (CS6's `InitSignatureCache`/`InitScriptExecutionCache`). It is an `Arc` shared by `CoreConnector` and MP1. The caches synchronize internally (a leaf lock, §5.2).

**Threads.** `-par` follows Core (`script_check_threads(par, cores)`, `MAX_SCRIPTCHECK_THREADS = 15`). ND1 builds the `CheckQueue` once and hands it to the connector. `-par=1` builds `CheckQueue::serial()`.

### 2.10 Undo: what we store and why (D11, D12)

**Core's record** (`undo.h`, `node/blockstorage.cpp`):
- `CBlockUndo { vtxundo }` holds one `CTxUndo { vprevout }` per non-coinbase transaction, with one `Coin` per input in `vin` order (`undo.h:53-70`).
- Each coin is serialized by `TxInUndoFormatter` (`undo.h:23-49`): `VARINT(nHeight*2 + fCoinBase)`, then a 0 dummy byte when `nHeight > 0`, then `TxOutCompression`.
- `UndoWriteToDisk` (`blockstorage.cpp:509-538`) appends `magic ‖ size ‖ CBlockUndo ‖ SHA256d(prev_hash ‖ CBlockUndo)` to `rev<nFile>.dat`, the same number as the block's `blk` file (`FindUndoPos(nFile)`, `:726-742`).
- `WriteUndoDataForBlock` writes only when the position is null, flushes and finalizes a lagging rev file when its last block's undo lands, and then sets `nUndoPos`, `BLOCK_HAVE_UNDO` and dirty (`:773-803`).

**What we store: exactly that** (ST2's `BlockFiles::write_undo`, `dashd-store/src/flatfile/blocks.rs:508-545`, which already mirrors the rev-file finalize rule), with Core's checksum (`undo_checksum`). There is no second undo representation. The reasons:
1. **Import and interop.** CH6a hard-links Core's sealed `rev*.dat` and normalizes legacy records into rdashd-owned files (CH2 §3.5.2). One format means import needs no transcoding.
2. **Recovery reads it.** CH2b's `ReplayBlocks` decodes `BlockUndoRef` from these records (`recovery/replay.rs`). `-reindex-chainstate` keeps undo positions as Core does.
3. **Parity is checkable per block.** O8's `undo` field is `sha256d(serialize(CBlockUndo))` (O8 §5.2). Equal bytes mean the same spent coins in the same order with the same heights and flags.
4. **No new durability domain.** The rev bytes are synced at CH2b's files barrier before B1 (CH2 §3.4 †1), and index records name them only after that barrier (I1).

Rejected alternatives: native undo rows in RocksDB (a second durability domain, transcoding at import, new crash states for ST3a to explore); undo inside the coins batch (it breaks Core's file → index → coins order, which the protocol is built on).

The **evo undo** is not in the rev record: in Core it is EvoDB's own diffs. It is CH7a's `EvoDelta` (A1 §2.10).

#### 2.10.1 The write path

- **Streaming.** CH3a builds no intermediate `BlockUndo`. `scratch.undo: Vec<u8>` (capacity kept across blocks) receives:
  - `CompactSize(vtx.len() − 1)` at step 13;
  - per non-coinbase tx, `CompactSize(vin.len())` at L8;
  - each spent coin's `CoinRef::write_undo` (CS5a), in `vin` order, as `spend` returns it.

  Because the counts are known before their entries, the bytes equal `serialize(CBlockUndo)`. Slice a3's test checks equality with CS5a's `BlockUndo` encoder on every L0 vector and on the replay.
- **Write.** After step 20, and only on a non-`just_check` attempt with a null undo position:
  1. `files.mutate(|f| f.write_undo(&scratch.undo, file, h, &prev_hash))`. Its `Err` maps to `SystemError`: "Disk space is too low!" for out-of-space, "Failed to write undo data" otherwise. The latch refuses a write after a trip (`Fatal`).
  2. **Then** `st.set_undo(file, pos)` (`HAVE_UNDO` + position + dirty).
  3. Then `st.raise_validity(VALID_SCRIPTS)`.

  Order (2) after (1) is **T12** ("`HAVE_UNDO` set before the undo write returns" is a planted bug the ChainSut connect half must catch; CH2 §6.4).
- **ST2 nit** (D13): `write_undo` takes `&CoreUndoBytes` (an `Arc<[u8]>`) and copies the record into a fresh `Vec`. Slice a3 adds `write_undo_slice(&[u8], …)`, which writes `[header, undo, checksum]` as one vectored write (the `undo.write(…, &[&…])` call already takes slices). This saves an allocation and a copy per block. ST2's owner reviews.
- **`undo_hash`** (`sha256d(undo)`) is computed only when a `DigestSink` is attached (D17). The checksum (`sha256d(prev ‖ undo)`) is always computed by `write_undo`.

#### 2.10.2 The read path (disconnect)

`UndoReadFromDisk` (`blockstorage.cpp:540-575`):
- It opens the rev file **by path for each read** (`OpenUndoFile`, `:649-652`).
- It decodes `CBlockUndo` **from `pos` through a hashing verifier**, seeded with the parent's hash. It **never reads the size field** in front of the record.
- It then reads the 32-byte checksum right after the decoded record and compares.

Each failure logs one `LogError` line and is `DISCONNECT_FAILED` (`v:2299-2302`), **not** an abort. The caller, `DisconnectTip` → ABC's `AbortNode(state, "Failed to disconnect block; see debug.log for details")` (`v:3612-3621`), aborts the node.

**Ours is Core-exact on corrupt files too** (D45, D46). ST2's `read_undo` today reads the size field first, bounds it and reads `size + 32` (`blocks.rs:866-944`). On an intact file that agrees with Core. On a damaged size field it does not: Core decodes the intact record and continues, while ST2 reports `Corrupt`. So ST2 gains `read_undo_record(pos, prev_hash, out)` (§11):
- **Fast path** (every intact read). Use the size field only as a hint: if it is within `MAX_UNDO_RECORD_SIZE` and the file, read `size + 32` and decode `BlockUndoRef` (CS5a). Accept when the decode consumed exactly `size` bytes and the checksum over `prev ‖ record` matches. Conclude "Checksum mismatch" when it consumed exactly `size` bytes and the checksum differs. A non-EOF decode error inside those bytes is also final: Core's decoder fails at the same offset on the same prefix.
- **Slow path** (any other case: size out of bounds, past EOF, decode hit EOF, or a decode shorter than `size`). Ignore the size field and decode from `pos` over a buffer that doubles on EOF up to the file's end. Then read the checksum at `pos + consumed`, exactly as Core's stream does. This runs only on corrupt files.
- **Open by path.** The read opens the rev file by path, never through `OsFs`'s handle cache (`flatfile/fs.rs:69-130`). A cached handle would keep reading an unlinked file, so `feature_abortnode`, which deletes `rev00000.dat` under a running node, could not fail as Core does (D46). Writes keep the cache. Undo reads happen only on disconnect, VerifyDB and recovery, so one `open` per read costs nothing measurable.

| Condition | Core's line (`UndoReadFromDisk: …`) | Ours |
|---|---|---|
| null undo position | "no undo data available" | the same, before any I/O |
| the open fails (missing or unlinked file) | "Unable to open file `<path>`" (`flatfile.cpp:45`), then "OpenUndoFile failed" | the same two lines (open by path) |
| decode or I/O exception | "Deserialize or I/O error - `<what>`" | the same prefix; `<what>` is Core's exception text for the class: "AutoFile::read: end of file" and "AutoFile::read: fread failed" (`streams.cpp:27`), "ReadCompactSize(): size too large" (`serialize.h:319`), "non-canonical ReadCompactSize()" (`:304-316`), "ReadVarInt(): size too large" (`:412-417`), plus the suffix libstdc++ appends, pinned from dashd's log by a5's corruption tests. A class with no Core analogue is a declared allowed difference (log text only; no FT asserts it) |
| checksum differs | "Checksum mismatch" | the same |

Each → `DisconnectResult::Failed`. None is a `CoinsReadError`: Core treats a rev-file failure as a failed disconnect.

### 2.11 Coins and CH2 flush epochs

- **One overlay per attempt, over its view target** (D36).
  - With `ViewTarget::Active`, `BlockOverlay::new(&mut cache)` (CH2 §2.4.2) borrows the active layer for the attempt.
  - With `ViewTarget::Scratch(s)`, `s.split()` gives an overlay over CH5's scratch view (and its evo transaction), which has its own best block and sits over active. Core's VerifyDB runs `DisconnectBlock` and `ConnectBlock` into one `CCoinsViewCache` over the tip and never flushes it (`v:5055`, `:5112`, `:5160`). `BlockOverlay` has no best block of its own and cannot stack (CH2 DEC-142), so the multi-block layer has to be a separate view, and the seam has to say which one an attempt writes.
  - **Commit follows the target, never `AttemptOrigin`.** The hooks (H1, H6, H10) get the same target, so CH7a can open a never-committed evo transaction for VerifyDB, as Core does at `v:5047`.
  - A frozen F may be in flight on W during the whole connect. The overlay reads F but never writes it, and `commit` merges into the target's layer only.
- **Reads** resolve child → active → F → prefetch → DB.
  - `have_inputs` caches each input in active, clean, as Core's parent `FetchCoin` does. The child keeps no copy.
  - Any read error is `CoinsReadError` → `Fatal` (CH2 §2.4.4 rules 1–3). It is never `bad-txns-inputs-missingorspent`, never `DisconnectResult::Failed`, never `FAILED_VALID`. FA7 checks this.
- **Commit point** (D33).
  - For a tip connect, `overlay.commit(BlockRef(id))` and the evo transaction's commit (H10) run inside `connect_block`, after step 26 **and after `finish_attempt` has applied every effect of the attempt** (§3.2). This is Core's `view.Flush(); dbTx->Commit();` (`v:3446-3448`), which runs after `ConnectBlock`, and every effect Core executes inside `ConnectBlock` has run by then.
  - If an effect fails, the result is `Fatal`, with no commit: the parent's coins and evo best block stay at `prev`, and no tip is published.
  - Both commits are in-memory merges. A failure there is a logic error (`Fatal`), and V's freeze never runs.
  - CH1-node's `at_site(IF_NEEDED)` then decides (`v:3457`), and `after_step` freezes `(id, q+1)` after `hold.connect_tip` (CH1-node D3, CH2 E-b1.2). So the freeze always sees the block applied, and V's check `new == coins.best_block() == evo.best_block()` holds.
  - **C11's CH2 half** (a freeze never names an unapplied tip) needs no CH3a change. Its connect half is CH3a's acceptance (§7.4).
- **Disconnect.** `BlockOverlay::new`, then `spend`/`add` per `ApplyTxInUndo`, then `finish_attempt`, then `commit(BlockRef(prev))` (`v:2365`). For `ViewTarget::Active`, the prefetch map is cleared at every disconnect (CH2 §2.4.3 rule 4: `cache.clear_prefetch()`), so CH4's results from before the disconnect cannot resurrect a coin. For `Scratch`, the prefetch belongs to active and is left alone. CH5 and CH2b show that the scratch layer shadows it: a coin spent or erased in the scratch is never read from F, the prefetch or the DB.
- **Budget.** Nothing in CH3a checks the coins budget. Mid-block growth is allowed (DEC-95). The checks are CH1-node's sites (`v:3304`, `v:3457`, `v:3851`).
- **A failed attempt** drops the overlay. Active is untouched, apart from the clean entries `have_inputs` cached, which is Core's behaviour (`HaveInputs` caches in the parent too). `have_in_cache` timing differences are DEC-102's allowed difference.
- **The DEC-131 deviation** (`add(possible_overwrite = false)` consults active and F) can raise `OverwriteUnspent` where Core's child would throw `FreshMisapplied` at its flush. Both are `logic_error` (`Fatal`) and unreachable on a valid chain. `UpdateCoins` with `possible_overwrite = coinbase` cannot raise it for a coinbase.

### 2.12 `disconnect_block` (`v:2282-2378`) (D15)

| # | Core | What | Ours |
|---|---|---|---|
| 1 | `v:2287-2292` | DIP0003 active at `pindex` ⇒ evo best block == `pindex`, else `AbortNode(EvoDbInconsistencyMessage())` and FAILED | hook H2; the connector's `AbortSink` reports `EvoDbInconsistencyMessage()` (§2.1.1), then `Failed`. Under an incomplete `EvoHooks`, a block where G1 holds (§2.17) also gives `Failed` with the connector's refusal text. That is unreachable while G1 and its startup check hold, but cheap |
| 2 | `v:2298-2307` | read undo ("DisconnectBlock(): failure reading undo data"); `vtxundo.size() + 1 != vtx.size()` → "DisconnectBlock(): block and undo data inconsistent" → FAILED | §2.10.2 |
| 3 | `v:2309-2313` | `UndoSpecialTxsInBlock` **before** the coins loop; failure → FAILED | **hook H9** |
| 4 | `v:2322` | `fEnforceBIP30 = true` | literal; `is_bip30_exception` is therefore always false |
| 5 | `v:2325-2362` | for each tx from last to first: each spendable output `SpendCoin(out, &coin)`; mismatch in presence, `out`, height or coinbase flag ⇒ `fClean = false`. Then for non-coinbase: `vprevout.size() != vin.size()` → FAILED ("transaction and undo data inconsistent"); inputs from last to first: `ApplyTxInUndo` | `overlay.spend` + compare via `CoinRef`; inputs reversed; legacy path below |
| 6 | `v:2252-2278` | `ApplyTxInUndo`: `HaveCoin(out)` ⇒ unclean; `nHeight == 0` (legacy) ⇒ take height and coinbase flag from `AccessByTxid(view, out.hash)`, else FAILED; `AddCoin(out, undo, possible_overwrite = !fClean)` | ported with CH2's `access_by_txid`. The legacy path stays live (CH2 §3.5.2: imports normalize; a debug assertion records that it is never taken after normalization). Recovery stays blind |
| 7 | `v:2365-2366` | `SetBestBlock(prev)`; evo `WriteBestBlock(prev)` | `commit(prev)` into the view target after `finish_attempt` (D33, D36); hook H6 |
| 8 | `v:2368-2371` | MN-list-changed notification | hook H7 |

The result is `Ok`, `Unclean` or `Failed`. CH1-node's `disconnect_tip` treats anything but `Ok` as Core's `DisconnectTip` does (`v:3281-3284`: `!= DISCONNECT_OK` ⇒ return false). VerifyDB (CH5) and `ReplayBlocks` (CH2b, coins-only) read `Unclean` themselves.

### 2.13 The tip glue: what runs inside the connector, and one declared reordering (D14)

Core's `ConnectTip` (`v:3427-3449`):
1. `dbTx = BeginTransaction`; `view(&CoinsTip())`;
2. `ConnectBlock`;
3. `BlockChecked(block, state)`;
4. on failure, `InvalidBlockFound` if invalid; return false;
5. `view.Flush()`; `dbTx->Commit()`.

CH1-node's `connect_tip` calls `connector.connect_block` and then emits `block_checked`. Inside `connect_block` the order is:
1. `begin_attempt` (the evo transaction opens, H10);
2. `ConnectBlock`;
3. `finish_attempt`: the attempt's effects are applied in emission order, as Core has executed them inside `ConnectBlock` by its step 5;
4. the commit (5).

So the commit precedes `BlockChecked` (3), the reverse of Core's order (D14).

**Declared, not observable.** Both happen inside V's hold, with no release point in between.
- `BlockChecked` is synchronous in Core (`validationinterface.cpp`, called through `m_internals->Iterate`). Its only listeners are net_processing's (`net_processing.cpp:2314-2342`: peer punishment and block-source bookkeeping) and `submitblock`'s state catcher (`rpc/mining.cpp:999`). They read the validation state, the peer map and the IBD flag, never the coins view or the evo store. No wallet, ZMQ, mempool or governance listener implements it.
- The alternative was to split the seam into `connect` and `commit` with `block_checked` between them. It was rejected because it adds a state ("checked, uncommitted") that has no observer.
- **One debug-only consequence:** with the commit inside `connect_block`, `-debug=bench`'s "Connect total" and "Flush" lines divide the time differently from Core's `ConnectTip` lines. Their format is Core's (D30); only the split differs.

**H7's MN-list signal needs its own rule** (D47; a CH7a requirement). Core calls `NotifyMasternodeListChanged` synchronously inside `ConnectBlock` (`v:2918-2921`), before `view.Flush()` and before the tip moves. Its listener (`CDSNotificationInterface`, `dsnotificationinterface.cpp:90-96`) runs `CMNAuth::NotifyMasternodeListChanged` and `govman.CheckAndRemove()` while `CoinsTip()` is still at `pprev`. CH7a stages the signal (N1), so it must either:
- fire it before the commit and before the tip moves, or
- prove that every listener reads neither the coins view nor the tip.

`DisconnectTip` (`v:3276-3288`) has no signal inside the transaction other than H7's (disconnect step 8, under the same rule), so it needs no other deviation.

### 2.14 `TestBlockValidity` (`v:4923-4974`) (D16)

CH1-node routes `ChainCommand::TestBlockValidity` (CH1-node §3.1) to the connector. Under V's hold, with `prev == tip`:
1. `HasConflictingChainLock(prev.h + 1, hash)` → `INVALID_PREV`, `bad-chainlock` (`v:4937-4939`). Note the class: `ConnectBlock`'s own check uses `CHAINLOCK` (`v:2518`). Core calls `chainlocks` directly here, not `m_chain_helper`, so it records no `A` entry; ours reads `ConnectCx::chainlocks` (V's `ClState`), not the `ChainHelper`.
2. A dummy index: hash = `block.GetHash()` (nonce included, O8 §4.3), height = prev + 1, `pprev` = tip. Ours: the connector's internals never take a `BlockId` directly. They take a `BlockAt { hash, height, prev: BlockId, id: Option<BlockId> }`, and every per-block question goes through `prev` (deployments at the block are CS4's `active_after(prev)`, ancestors are `prev`'s plus the block itself). A proposal has `id: None` and is never inserted. CH1-A10's `check_header_at(prev, &header, now)` is the non-inserting `ContextualCheckBlockHeader`.
   - **Assumevalid never applies to a proposal.** Core's dummy is never an ancestor of the assumevalid block or of the best header (`v:2551-2552` compare pointers), so `fScriptChecks` is true. With `id: None` our §2.8 test is false for the same reason.
3. `ContextualCheckBlockHeader`, `CheckBlock(fCheckPOW, fCheckMerkleRoot)`, `ContextualCheckBlock` (CS3), each with Core's `LogError` line.
4. `ConnectBlock(…, fJustCheck = true)`: steps 3–20, then return at step 21. There is no undo, no `VALID_SCRIPTS` and no commit; the overlay is dropped. CheckBlock re-runs inside with PoW and merkle off (`v:2505`); `checked` is always false here, since no caller passes a block whose CheckBlock ran with both checks on.
   - The readiness guard applies (step 5a's G1 and step 19's G2; §2.17). A refusal is a `SystemError` that the RPC returns (`getblocktemplate` proposal mode and `CreateNewBlock` already turn a `state.Error` into an RPC error). It is never an `AbortNode`: a proposal must not stop the node.
5. Effects are applied (§3.2) and the evo transaction is rolled back (hook H10).

**Digest scope.** O8 records attempts, and an attempt is one `ConnectBlock` call; `TestBlockValidity`'s pre-checks are outside it (`O8-statedigest-v1.md:38`, `:117`, `:273`). So the digest attempt and the decision-recorder scope open at step 4, not at the command: a proposal rejected at step 1 or 3 emits no `R` and no `A`, and one that reaches step 4 emits exactly one `R` with `org:"proposal"`. The EV attempt bracket (§3.2) still covers the whole call. Slice a6 tests all three cases.

GBT's template check (`CreateNewBlock` → `TestBlockValidity`, MN0) and `getblocktemplate` proposal mode (RP rows, L3's oracle) both use it. `generate*` therefore exercises the store-both cache policy (§2.9).

### 2.15 The digest tap and attempt origins (D17)

O9a D7 recorded that rdashd's records are all `org:"tip"` because `BlockConnector` receives no origin. CH3a closes it: `ConnectCx::origin` is set by the caller.
- CH1-node's `connect_tip`/`disconnect_tip` set `Tip`; `TestBlockValidity` sets `Proposal`.
- CH5's VerifyDB sets `VerifyDb`, and CH2b's `ReplayBlocks` sets `Replay` (it calls only the coins-only disconnect, so its `D` records come from recovery's own tap; CH2b adds the call).

A `DigestSink` (dash-statedigest's record types, O8 §12) receives, per attempt:
- **`C`** (a non-`just_check` success). Header fields (§5.1); `undo`; `bls`, `bur` and `dep` (CS4 answers for the child, O8 §5.2). `mnl`, `qc`, `cp` and `ehf` are filled by hook H11, so they are null in non-evo windows. The golden fields are `muhash`, `txouts`, `amount`, `unspendable`, `bi` and `stats` (§5.3), plus `fx` and `seq`.
- **`R`** (a failure or a `just_check` success): `res`, `cls`, `why`, `dbg`, `fx`.
- **`D`**: `res` and `fx`.

The decisions recorder (`A` records) is the helper's (§3.3).

**Golden fields are computed, not queried.** A pure `dash_consensus::coins::stats` module (slice a1) ports:
- `coinstatsindex`'s per-block accumulation: MuHash insert and remove via `utxo_hash::{insert_coin, remove_coin}`, `txouts`, `total_amount`, `total_unspendable_amount`, and `block_info` (`prevout_spent`, `coinbase`, `new_outputs_ex_coinbase`, `unspendable`, `genesis_block`, `unspendables_scripts`, `unclaimed_rewards` = subsidy + fees − coinbase value out);
- `getblockstats`' seven named fields.

Its inputs are the block, its undo and the subsidy and fees. It also has a reverse step, coinstatsindex's `ReverseBlock` (`index/coinstatsindex.cpp:403`: remove the block's created coins, re-insert the spent ones), which D48's reverse leg uses. IX's coinstatsindex later reuses the same pure functions, so the two cannot drift.

**Where the running state starts** (D34). The tap keeps the cumulative counters (`txouts`, `total_amount`, `total_unspendable_amount`) and a `MuHash3072` in memory.
- **A golden hash cannot seed the accumulator.** O8's golden `muhash` is the finalized value: `Finalize` divides the numerator by the denominator and hashes the 384-byte result (`crypto/muhash.cpp:314-323`). The accumulator's state is the `(numerator, denominator)` pair, 768 bytes, which Core serializes for coinstatsindex (`crypto/muhash.h:120-123`). Nothing recovers it from 32 bytes.
- **So every run that claims per-block MuHash parity starts at genesis**, from the empty set and zero counters. That includes the per-PR window (§7.2).
- **A segment start needs a segment seed.** That is either the starting UTXO set scanned into a fresh accumulator, or Core's serialized `MuHash3072` imported beside a UTXO snapshot. In both cases the finalized hash must equal golden(start − 1) before the first block, and the counters are seeded from golden(start − 1), which are verified values. Segment seeds are slice a12, which needs O9b's segment snapshots. Its test: a segment run equals a full replay at the segment's first and last heights.

### 2.16 Versionbits wiring and the unknown-bits warning (D21)

CH3a wires CS4's `Deployments` on V over the header chain (`dash-chain`'s `HeaderChain` as `ChainAncestry`, never a `RuleContext`, as CS4's docs require). It uses one `VersionBitsCache` and an `EhfSignalSource` that is `NoEhfSignals` until CH7a provides the MNHF source (no EHF deployment is active before V20). `ConnectCx::deployments` points at it. `UpdateTip`'s unknown-versionbits warning (CH1-node D30) is a port of `WarningBitsConditionChecker` (`v:2395-2417`) over the same cache, emitting Core's warning text through CH1-node's `update_tip`. Slice a8 lands both **before** a3, because a3's flags, rules and guard already need real deployment answers (D38).

### 2.17 The readiness guard (D31, D32, D54)

**Why it exists.** Two of CH3a's answers are exact only inside a regime:
- `NoEvo` is exact only where no evo rule is live.
- `NodeHelper`'s `sb_required = false` and `gov_valid = false` are exact only where a synced Core could not answer differently (§2.7).

A milestone plan does not stop an operator from pointing rdashd at mainnet peers. So the connector itself refuses, failing closed, every block outside those regimes.

**What a refusal is.** It is a readiness failure, never a verdict:
- It is a `ConnectOutcome::SystemError` with rdashd's text, e.g. `rdashd cannot validate block <hash> at height <h> on <chain>: <capability> is not built (ALLOWED-DIFFS D18)`.
- It is never `Invalid`: no `FAILED_*` bit, nothing cached, and no peer scored, since `BlockChecked` punishes only `IsInvalid()` (`net_processing.cpp:2323-2326`).
- G1 is raised at step 5a, G2 at step 19. Both come before step 22, so there is no undo, no `HAVE_UNDO` and no `VALID_SCRIPTS`.
- The block stays `HAVE_DATA`, so a capable build connects it later.
- Any effects emitted before G2 (only step 18's IS removals) are applied, as on every attempt outcome (A1 §2.10.1).
- On the tip path (ABC, invalidate, recovery activation), the connector reports it through its `AbortSink` (§2.1.1) and the node stops: Core's own fail-closed path. For `TestBlockValidity` it is an RPC error and the node keeps running (§2.14).

**G1: an incomplete `EvoHooks`** (lands in a3; CH7a lifts it).
- `EvoHooks::COMPLETE: bool` is false for `NoEvo`. CH7a sets it true only when every hook is implemented and its acceptance passes.
- At step 5a, with `!H::COMPLETE`, the block is refused **on every network** when any of these holds:
  - DIP0003, V20, MN_RR, V24 or EVO_SHARES is active at the block (CS4's `active_after(prev, …)`);
  - its coinbase is a special-version CbTx: `nVersion ≥ 3` (signed 16-bit) and raw `nType == TRANSACTION_COINBASE`. Below version 3 a raw type 5 carries no payload, and `NoEvo` judges it exactly (D54).
- **Why that set.** These are the conditions under which `ProcessSpecialTxsInBlock` or `IsBlockPayeeValid` read evo state or check evo data:
  - the CbTx branch (`evo/specialtxman.cpp:816-839`);
  - the V20 credit-pool indexes (`:847-851`);
  - the V24 shared-collateral rule (`:856-861`);
  - the MN list from DIP0003 (`:906`);
  - the payee check from DIP0003 enforcement (`masternode/payments.cpp:227-240`).

  V20 and MN_RR cannot precede DIP0003 on regtest (`chainparams.cpp:964` asserts it), and V24 and EVO_SHARES are EHF deployments, but `-vbparams` can move V24, so the set is checked as listed rather than inferred.
- **Deliberately not in it:** DIP0008, V19, DIP0024 and BRR. They are active from height 1 on regtest (`chainparams.cpp:822-827`), so including them would refuse every regtest chain. Below DIP3 they gate nothing `NoEvo` must check:
  - special transactions are rejected first, by the ported gate below;
  - the CbTx version rules that DIP0008 touches are G1-refused;
  - there are no quorums without masternodes.
- **Regtest consequence:** NodeSut, L3 and CH3a's FTs stay below DIP3 or raise it with `-dip3params` (the FTs already do; §7.5). Regtest FTs that cross DIP3 move to CH7a (D37).

**The pre-DIP3 checks `NoEvo` ports** (D32, D54). Every type test below reads the raw `u16` (`tx.tx_type().to_u16()`, as CS3's quirk 2 already does), never the typed `TransactionType::Coinbase`, which exists only at version ≥ 3. A version-2 transaction with raw type 5 decodes as `ClassicalWithNonStandardVersionTypeBytes(5)` (`dash/src/blockdata/transaction/mod.rs:621-635`).
- **The top-level CbTx check** (D54; Sol r2 M1). With `fCheckCbTxMerkleRoots` (H4's `check_cbtx_merkle_roots`), Core checks any `vtx[0]` whose raw `nType` is `TRANSACTION_COINBASE`, **at every version** (`evo/specialtxman.cpp:816-832`):
  - not a coinbase → `bad-cbtx-invalid` (`:819`; unreachable after `CheckBlock`'s `bad-cb-missing`, ported anyway);
  - a payload that does not decode as `CCbTx` → `bad-cbtx-payload` (`:831`).

  Both are `BLOCK_CONSENSUS` with no debug text, plus `v:2666`'s log line.
  - Below version 3 Core reads no extra payload (`primitives/transaction.h:362-364`), so `GetTxPayload` always fails (`evo/specialtx.h:18-35`). The outcome needs no evo state: `bad-cbtx-payload` with script checks on, no check at all below assumevalid. `NoEvo` ports exactly that.
  - At version ≥ 3 the payload can decode, and `CheckCbTx`, the credit-pool diff and the merkle roots follow. That coinbase is G1's.
- **The special-transaction gate** (D32). `CheckSpecialTxInner` rejects every transaction with an extra-payload field (`nVersion ≥ 3 && nType != 0`) while DIP0003 is not active after `pprev`: `TX_BAD_SPECIAL`, `bad-tx-type-dip3-inactive` (`evo/specialtxman.cpp:224-229`).
  - It runs inside `ProcessSpecialTxsInBlock`'s loop for every transaction except a `vtx[0]` whose raw `nType` is `TRANSACTION_COINBASE` (`:865-875`), so a version-2, raw-type-5 coinbase never reaches it. The block reject is `BLOCK_CONSENSUS`, with the debug text "Special Transaction check failed (tx hash %s) %s", plus `v:2666`'s log line.
  - `ContextualCheckTransaction` checks transaction types only under DIP0003 (`v:351-378`), so nothing earlier catches such a transaction.
- `NoEvo::process_special_txs` ports both in Core's order (the top-level check, then the loop) at step 15, before the UTXO loop, so §2.4's "H4 wins" row covers them.
- Every other part of the function is DIP0003- or V20-gated, or reached only through a special transaction or a decoded CbTx payload. Two examples:
  - `CQuorumBlockProcessor::ProcessBlock` returns true before DIP0003 (`llmq/blockprocessor.cpp:218-220`);
  - `CMNHFManager::ProcessBlock` finds no signals without MNHF transactions (`evo/mnhftx.cpp:213-225`).

  So with G1 holding, `NoEvo` is exact below DIP3. rev1 claimed this with neither check. rev2 claimed it with only the gate, and Sol r2 M1 found the version-2, raw-type-5 coinbase that the gate never sees.

**G2: the interim helper at a live tip** (lands in a3; SP1 and GV1b lift it).
- `ChainHelper::interim_refusal(h) -> Option<&'static str>` is an **unrecorded** method that defaults to `None`. `IbdHelper` and `RecordedHelper` keep `None`, so O8's decision stream is untouched.
- `NodeHelper` returns `Some` when all of these hold:
  - `cx.guard == Live` (mainnet, testnet);
  - `!is_ibd` (V's latch, as Core's);
  - `CSuperblock::IsValidBlockHeight(h)` (`governance/superblock.cpp:119-124`) or `h` is in an old-budget window;
  - `best_cl_height < h`;
  - SP1 or GV1b is absent.
- The connector asks it at step 19, before `sb_required`. The set is exactly where Core with a finished MN sync could answer differently: `IsSuperblockValidationRequired` (`evo/chainhelper.cpp:41-52`) and `payments.cpp:313-440`.
- During IBD and reindex, Core is in the same not-synced regime, so G2 lets them through. That is how mainnet history up to the live tip replays.
- Regtest and devnet keep D18's declared regime: `GuardScope::Declared`, Sol's "historical regime" scope.
- **Consequence:** after CH7a and before M5, a live mainnet node stops at the next superblock height that is not yet ChainLocked (about monthly). A testnet node stops within about an hour. A restart does not clear it: the same block is refused again at startup. Each stop lasts until the tip is older than `-maxtipage`, or until SP1 and GV1b land (R10). A live mainnet or testnet tip is an unsupported configuration before M5 anyway.

**Where the guard is checked.**
- `connect_block` on every path: tip connect, ABC, invalidate's reconnects, recovery's activation through `connect_block` (A1 §3.5.3 I3).
- `test_block_validity`.
- `disconnect_block` under `NoEvo` (G1 only; §2.12 row 1).
- **At startup**, in CH1-node's load path, after recovery's replay and the tip's load (Core's order, `node/chainstate.cpp:251` then `:272-277`; CH3a D70) and before ABC: an incomplete build refuses to start (InitError, the same text) when the active tip or any block in the recovery transition satisfies G1. This happens when a datadir written by a complete build is opened by an incomplete one. Its coins and evo state are past what this build can disconnect or extend. From CH7a-4, when `H::COMPLETE` is true, the check no longer returns early (`rs:chain/load.rs:382-384`): it calls the provided associated function `H::startup_refusal(block, at, deployments)` for the same blocks and formats its capability with `refusal_text` (CH7 D3). Under `-reindex` or `-reindex-chainstate` the check runs after CH5 has discarded the chainstate, so it sees genesis and passes; each block then meets G1 at its connect.

ND1's coarser outer layer (refuse mainnet and testnet outright without a complete build) is not added. It would block the pre-DIP3 IBD and replay that M3 relies on, and the startup check refuses exactly the datadirs this build cannot judge.

**Tests** (a3, CH1-node's in-process V harness with reopen; a10 reruns them as `NodeSut` ops):
- **G1 boundary.** Regtest with `-dip3params=20:20`, `NoEvo`: blocks 1–19 connect. Block 20 is refused: `SystemError`, one sink report, V stops, block 20 stays `HAVE_DATA` with no undo and no `FAILED_*`, and the tip is 19.
- **G1 restart.** Reopen the same stores. The startup check passes (tip 19). ABC refuses block 20 again, the same way.
- **G1 startup refusal.** A datadir extended past 20 by a test `EvoHooks` with `COMPLETE = true`, reopened with `NoEvo`, gives InitError.
- **G1 shapes.** A pre-DIP3 coinbase with version 3 and raw type 5 is refused. With `-vbparams` making V24 active below DIP3, the block is refused.
- **Version-1 and version-2 coinbases with raw type 5** (D54; Sol r2 M1, Opus r2 M1). The same pre-DIP3 height, an otherwise valid block, one case per version. G1 does not refuse them.
  - With script checks on, Core's outcome is `bad-cbtx-payload`, `BLOCK_CONSENSUS`, so `FAILED_VALID` (`evo/specialtxman.cpp:816-832`). Ours must be identical, and L3's operator checks it against dashd.
  - Below assumevalid (the skip taken, headers preloaded as in D44), Core runs no CbTx check (`:816` is gated on `fCheckCbTxMerkleRoots`) and the loop skips the coinbase (`:865`), so the block connects. Ours must connect it too.
- **D32 gate.** A pre-DIP3 block carrying one special transaction (each type, at index 0 and > 0) gives `bad-tx-type-dip3-inactive`, `BLOCK_CONSENSUS`, Core's debug and log texts. L3 checks it against dashd.
- **G2.** Under `GuardScope::Live` on regtest PoW (the scope is injected, so superblock heights below the raised DIP3 are reachable):
  - `!ibd`, a superblock height, no ChainLock → refused; then a restart at that height → refused again;
  - each of a ChainLock at or above `h`, `ibd`, a non-superblock height, and `Declared` → connects.
- **`TestBlockValidity`.** Under G1 and under G2 it gives an RPC error, no `AbortNode`, and the node keeps serving.

**Removal.** These rows (§11) remove the guard once their real inputs and tests land:
- **CH7a removes G1** by setting `COMPLETE = true`. Its acceptance turns the G1 tests around: the same blocks connect and match dashd.
  - **A datadir that `NoEvo` built** already carries `meta.evo_tip = tip`: CH2's B1 writes it on every full flush whatever the hooks (`rs:flush/machine.rs:943`), and recovery requires it. Its tip is below DIP3, where `NodeEvo` writes no evo row either, so it is the datadir `NodeEvo` would have written, and it opens under `NodeEvo` with no adoption step. CH7 keeps a consistency check (below DIP3 every non-effect evo CF must be empty, else Core's `EvoDbInconsistencyMessage` and `-reindex-chainstate`). Its turned-around G1 test uses such a datadir (CH7 D21).
- **SP1 and GV1b remove G2** when `NodeHelper` answers `sb_required` and `gov_valid` from real state. Their acceptance turns the G2 tests around against a synced dashd.

Until then, `compat/ALLOWED-DIFFS.md` D18 reads "mainnet/testnet live tip: refused (G2); regtest/devnet: declared".

---

## 3. Evo and LLMQ seams (CH7a, CH7b)

### 3.1 Hook points

`EvoHooks` is a trait with `NoEvo` as its CH3a implementation.
- Every method gets the attempt (§3.2) and, where Core passes it, the overlay as a `&dyn CoinsView` or `&mut` evo view.
- The view-taking hooks get the attempt's `ViewTarget` (D36).
- `const COMPLETE: bool` is false for `NoEvo` and drives guard G1 (§2.17). Beside it, the provided associated function `startup_refusal(block: &Block, at: &BlockAt, deployments: &dyn DeploymentView) -> Option<&'static str>` (default `None`, no receiver, like `COMPLETE`) serves the startup check when `COMPLETE` is true (CH7 D3).
- CH7a implements the trait, and CH7b supplies the LLMQ parts under CH7a's implementation. No hook changes CH3a's control flow.

| Hook | Core site | Called | `NoEvo` |
|---|---|---|---|
| H1 `bls_ctx` / scheme commit | `v:2490-2491`, `v:2916`; `ConnectTip`'s own guard `v:3405` and commit `v:3477`; disconnect `v:3272-3273`, `v:3320` | step 2, step 25; the tip glue (§2.13) | `bls_ctx_for_block`, no-op commit |
| H2 `verify_best_block(hash)` | `v:2525-2531`, `v:2287-2292` | step 6; disconnect 1 | true |
| ~~H3~~ | retired in rev2 (D40): `GetSpecialTxRules` (`v:6797-6803`) and `GetMnRewardEraAfter` (`v:5897-5903`) are deployment queries that read no evo state, so CH3a computes both from `cx.deployments` (step 14) | — | — |
| H4 `process_special_txs(block, id, rules: SpecialTxRules, view, subsidy, just_check, check_cbtx_merkle_roots, sink) -> Result<MnListUpdates, SpecialTxFailure>` (CH7 D2) | `v:2661-2678` | step 15 | below DIP3, the top-level CbTx check for a raw-type-5 coinbase below version 3 (`bad-cbtx-payload`, D54), then the gate `bad-tx-type-dip3-inactive` (D32); else `Ok(empty)` |
| H5 `mn_payment_valid(coinbase, prev, subsidy, fees, era: MnRewardEra, strict = rules.v24)` | `payments.cpp:227` `IsTransactionValid`, called at `:397` | step 20 | true (Core's answer below DIP3 enforcement; above it G1 has refused) |
| H6 `set_best_block(hash)` | `v:2913`, `v:2366` | step 24; disconnect 7 | no-op |
| H7 `list_changed(updates)` | `v:2918-2921`, `v:2368-2371` | step 26; disconnect 8; fired under D47's ordering rule (§2.13) | none |
| H8 `shared_collateral_spends(tx, view)` | `v:2708-2716` | L3, when `rules.v24` | unreachable (V24 is a G1 refusal) |
| H9 `undo_special_txs(block, id)` | `v:2309-2313` | disconnect 3 | Ok |
| H10 `begin_tx(target)` / `commit_tx` / `rollback_tx` | `v:3428`, `v:3448`; `v:3277`, `v:3287`; `TestBlockValidity`'s rollback; VerifyDB's never-committed transaction (`v:5047`) | around each attempt; the commit after `finish_attempt` (D33) | no-op |
| H11 `digest_fields(id) -> EvoDigest` | O8 §5.2 `mnl`, `qc`, `cp`, `ehf` | the `C` record | all null |

The hook names keep their rev1 numbers, so reviews and CH7a's notes can cite them unchanged.

**H4's inner error (CH7 D2).** H4 returns `Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>`, with `SpecialTxFailure { Invalid(BlockValidationError), NotReady(&'static str) }` defined beside `EvoHooks` (`rs:connect/evo.rs`). The step layer's trait `AttemptSteps` (`rs:connect/serial.rs:98`; `process_special_txs` at `:114-123`) takes the same inner type: `HookSteps` (`rs:connect/connector.rs:610`) passes it through, and `NoEvoSteps` (`rs:connect/bench.rs:51`) wraps its error in `Invalid`, as `NoEvo` and the test hooks do. At step 15 the connector keeps `Invalid`'s log line and verdict and maps `NotReady(cap)` to `refuse(refusal_text(cx.params, at, cap), just_check)` (`rs:connect/serial.rs:816-829`): reported on the tip path, unreported for a proposal, and, for origin `VerifyDb`, returned without `abort_node`, so VerifyDB fails with the refusal text.

### 3.2 The attempt and effect contract (EV addendum §2.6; A1 §2.10.1) (D19)

- **`VContext` lands with CH3a**, as the EV addendum specifies:
  - a zero-sized, `!Send`, `!Sync` marker;
  - created exactly once by V's command loop when the V thread starts, with its constructor private to CH1-node's `actor` module;
  - never handed out.

  CH3a threads `&mut VContext` into the three connector calls (D3). CH7b adds `VContext::on_v` and `MembersFacade::begin_attempt(&mut VContext, id)`. CH3a adds no capability beyond the marker, so CH7b's compile-fail suite (the addendum's six cases) needs no CH3a change. Slice a2 adds one compile-fail case of its own: constructing `VContext` outside V's loop module.
- **Bracket.** Every connector call opens an EV attempt and closes it on **every** outcome: success, invalid, system error, `just_check`, and an unwind (via a drop guard). The EV attempt is not O8's digest attempt: the digest attempt is one `ConnectBlock` call, so for `TestBlockValidity` it opens later (§2.14).

  ```rust
  let mut att = self.hooks.begin_attempt(v, AttemptId::next(), cx.origin, &target)?; // CH7a: opens the facade's AttemptHandle; NoEvo: an empty log
  let (out, staged) = self.connect_inner(cx, &mut att, &mut target, block, checked, id, st); // steps 1–26; effects go to att.sink(); nothing committed
  self.hooks.finish_attempt(att, &out)?;  // applies att's log in emission order, then release_attempt(EffectsApplied)
  if let (Ok(ConnectOutcome::Connected), Some(s)) = (&out, staged) {
      s.commit(&mut target);              // overlay.commit(best) + H10 commit_tx, into the target only (D33, D36)
  }
  out
  ```

  - **Commit after effects** (D33). The overlay and the evo transaction stay uncommitted until `finish_attempt` has returned `Ok`. A failure to apply an effect is A1's D4 → `abort_node` → `Fatal`, and nothing is committed. The parent's coins and evo best block stay at `prev`, and no tip is published.
    - Core's emission order is kept, and so is the rule that effects survive invalid and `just_check` attempts: `finish_attempt` runs on every outcome, and only the commit is conditional.
    - One order differs from Core, and nothing observes it. The step-22 undo write and `HAVE_UNDO` now precede the application of effects, whereas Core executes its step-18 removals before step 22. Both happen in one hold. On an effect failure, the undo position and `HAVE_UNDO` stay in memory only, because the latch blocks the flush. A restart finds the durable undo position null and rewrites the record. The orphan bytes left in the rev file are the same as Core's own crash outcome.
  - `finish_attempt` runs **before** CH1-node acts on the outcome (`invalid_block_found`, the freeze decision). This is A1's rule that effects are applied before the result is acted on, and the addendum's "before the next attempt's read view opens".
  - A guard dropped without `finish_attempt` trips the latch, as the addendum's unwind rule says.
  - **Fault-injection test** (a3, then a5 for disconnect). A failure is injected before the first effect, between effects k and k+1, and at `release_attempt`. For each:
    - the active coins' best block is still `prev` and every coin is unchanged;
    - the evo best block is still `prev` (a test `EvoHooks` with a store; CH7a reruns it with its own);
    - no `UpdateTip` or tip publication happened;
    - after a restart, recovery reaches the same state as an uncrashed run of the same blocks.
- **Effects CH3a itself emits:** only `RemoveConflictingIsLock` (step 18). Its applier belongs to the IS owner: it removes the lock and its chained descendants, as Core's `RemoveConflictingLock` does. At M3 the helper never reports a conflict, so it is never emitted. The rest of A1's table is CH7a/CH7b's (snapshot rows, mineable-commitment erase and re-insert, platform-ban resets, list-changed notifications).
- **Openers on V.** The openers are connect, disconnect (tip, reorg, invalidate, `MarkConflictingBlock`), `TestBlockValidity`, VerifyDB (CH5's `ChainCommand::VerifyDb`, the addendum's erratum), import and reindex. They all reach the connector from V's loop, so all have the context.

### 3.3 `ChainHelper`: the decision seam (D18)

```rust
pub trait ChainHelper: Send {           // O8 §7.1's queries, exactly; names follow O8's entries
    fn cl_conflict(&mut self, h: u32, hash: &BlockHash) -> bool;          // v:2516
    fn ibd(&mut self) -> bool;                                            // v:2801 (NodeHelper: ConnectCx::is_ibd; recorded)
    fn cl_has(&mut self, h: u32, hash: &BlockHash) -> bool;               // v:2803
    /// v:2806, the loop condition: the ONLY query that advances a recorded cursor (D35).
    /// Excludes every lock in this attempt's removed view (below).
    fn is_conflict(&mut self, tx: &Tx) -> Option<IsLockRef>;              // (islock hash, txid)
    /// v:2810. Acts on the lock is_conflict just returned (Core's inner re-query at
    /// chainhelper.cpp:93-99 returns the same one), emits Effect::RemoveConflictingIsLock, and
    /// adds Core's removal closure (the lock and its chained descendants,
    /// RemoveChainedInstantSendLocks, instantsend.cpp:339-350) to an attempt-local removed view,
    /// dropped when the attempt's effects are applied. Answers no query; consumes no recorded entry.
    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink);
    fn sb_required(&mut self, id: BlockId, h: u32) -> bool;               // v:2841 (chainhelper.cpp:41-52)
    fn gov_valid(&mut self) -> bool;                                      // payments.cpp:355, :404
    fn sb_triggered(&mut self, h: u32) -> bool;                           // superblock.cpp:578
    fn sb_payments(&mut self, h: u32) -> Option<Vec<(Amount, ScriptBuf)>>;  // superblock.cpp:622-629: the best
                                        // superblock's payment list; IsValidSuperblock over it is EV3's
    /// Not an O8 query and never recorded: readiness guard G2 (§2.17, D31). Default None.
    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> { None }
    // CH7b adds cl_best (specialtxman.cpp:151) and the evo queries
}
```

CH3a ships three implementations:
- **`NodeHelper`** (production, M3):
  - `cl_*` from CH1-node's `ClState` (LK1a feeds it; a node without LK1a has no CL);
  - `ibd` from `ConnectCx::is_ibd`, V's latch (the connector's own code never reads that field; in a `RecordedHelper` replay the recorded answer wins);
  - `is_conflict` → `None` until the IS owner lands. The IS owner's implementation then excludes the removed view; it holds the chained-lock index the closure needs;
  - `sb_required` = `best_cl_height < h && mn_synced`, with `mn_synced = false` until SP1 lands;
  - `gov_valid = false` until GV1b lands;
  - `interim_refusal` per G2 (§2.17).

  The resulting regime is §2.7's, refused at a live mainnet or testnet tip by G2. One nuance (n1): `gov_valid = false` is Core with `-disablegovernance` plus an unfinished sync, not a default dashd, whose governance is valid once `LoadCache` has run. Verdicts are identical on the `NoCheck` path, but production decision streams (the `gov_valid` answers) and the MNPAYMENTS/GOBJECT log lines differ from a default dashd's. D18 declares it.
- **`IbdHelper`**, for L2 mainnet/testnet replays: `ibd = true`, no CL, `sb_required = false`, `gov_valid` irrelevant. These are Core's answers during `-reindex-chainstate` of a node with no ChainLocks and an unfinished MN sync. They hold for the pre-DIP3 windows, where no CL exists. On the `NoCheck` path, which is the only one `IbdHelper` yields, the `gov_valid` answer does not change the verdict: `IsBlockValueValid` returns true at the `!IsValid()` check or the `NoCheck` check, after the same max bound.
- **`RecordedHelper`** over O9a's `RecordedDecisions` (keyed or strict; `final_connect`), for replays of O8's regtest FT recordings. A decision miss stops the replay (O8 §7.2).
  - **The cursor** (D35). Only `is_conflict` advances it. O8 records one `is_conflict` answer per `ConflictingISLockIfAny` call, i.e. per loop condition (`evo/chainhelper.cpp:79-85`), and nothing for the removal's inner re-query.
  - `remove_conflicting_is_lock` consumes nothing. It emits the effect for the lock the last `is_conflict` returned.
  - The removed view is not consulted, because the recorded answers already reflect Core's synchronous removal, chained children included.

**Directed tests for the IS loop** (a3, scripted helper; the recorded variants also run through `RecordedHelper` in strict mode). Each asserts the exact query sequence and effect sequence:
- **`[A, null]`:** a ChainLocked block whose transaction conflicts with lock A. Queries `ibd, cl_has, is_conflict→A, is_conflict→null`; effects `[Remove(A)]`; connects.
- **`[A, B, null]`:** two locks, one per input. Effects `[Remove(A), Remove(B)]` in Core's order.
- **A failed attempt after a removal:** `[A, null]`, then the block fails at step 19 (`bad-cb-amount`). The removal effect is still applied (effects survive), nothing is committed, and the next attempt's `is_conflict` no longer returns A.
- **Chained children** (Opus m1): T is locked by L; C spends `T:0` and an unrelated outpoint O and is locked by Lc, a chained child of L. A ChainLocked block holds X, which spends T's input, and Z, which spends O. X removes L, and the closure adds Lc. Z's `is_conflict` returns `null`: one removal effect, as in Core.
- **No ChainLock:** the `[A, null]` block without a ChainLock is `CHAINLOCK` `conflict-tx-lock`.
- **IBD:** neither `cl_has` nor `is_conflict` is asked.

The FT `feature_llmq_is_cl_conflicts` that Sol cites exercises this loop with real locks. It is evo, so it gates CH7b's replay, not CH3a's.

CH7b extends the trait with `cl_best` and the evo queries, and grows `RecordedHelper` into A1's `ReplayChainHelper` (failed attempts included). The PLAN text "CH7b: `ReplayChainHelper`" stays CH7b's; CH3a lands the non-evo subset it needs for its own L2 (§11).

---

## 4. CH3b: reorg integration

### 4.1 What CH1-node already ports (and CH3b only exercises)

| Core | CH1-node (N7b-1) | CH3b adds |
|---|---|---|
| `ActivateBestChain` (`v:3724-3856`), `ActivateBestChainStep` (`v:3601-3684`) | `abc.rs::{activate_best_chain, abc_step, abc_step_moves}`; disconnect pool per step; `MaybeUpdateMempoolForReorg(pool, false)` on system errors (`v:3616`, `v:3660`); 32-block batches; the batch-top `InvalidChainFound` | real-block runs (§7.5), the pool (§4.3) |
| `FindMostWorkChain` (`v:3519-3581`) | `abc.rs:296-350`, including `FAILED_CHILD` **and `CONFLICT_CHAINLOCK` marked dirty** (`v:3557`, `v:3561`) | CONS-11 restart proof (§4.5) |
| `PruneBlockIndexCandidates` (`v:3584-3593`), `TryAddBlockIndexCandidate` (`v:4233-4263`), `CBlockIndexWorkComparator` | `CandKey { work, Reverse(seq), Reverse(id) }`, one live key per candidate (CH1-node D1/D2/D9) | precious and tie FTs |
| `ConnectTip`/`DisconnectTip` (`v:3395-3513`, `v:3249-3342`) | `abc.rs:517-618`; prune-lock move-back subsumed by CH2's LCA bound (CH1-node §4.5) | the connector (§2.13) |
| `InvalidateBlock` (`v:3889-~4046`), incl. the per-iteration pool and `(++disconnected <= 10) && ret` (`v:3958-3965`) | `failure.rs::{invalidate_iteration, invalidate_final, invalidate_block}` | FT `rpc_invalidateblock` |
| `ResetBlockFailureFlags(pindex, ignore_chainlocks)` (`v:4152-4231`), `reconsiderblock` (`rpc/blockchain.cpp:2030-2064`) | `failure.rs::{reconsider_block, after_reset}` | FT; `ignore_chainlocks` RPC arg (RP row) |
| `PreciousBlock` (`v:3858-3887`) | `failure.rs::precious_block` | FT `rpc_preciousblock` |
| `EnforceBlock` (`v:4048-4078`), `MarkConflictingBlock` (`v:4080-4150`) | `locks.rs::{cl_enforce, enforce_best_chainlock, conflicting_marked}` | real-block CL scenarios with the connector |
| `LoadChainTip` and load-time candidates | `load.rs` | F01 (§4.6) |

### 4.2 Candidates with real blocks

The ordering is Core's comparator: more chainwork; then lower `nSequenceId` (arrival order, negative for precious); then the arena id standing in for the pointer (CH1-node D1/D9). With CH3a's connector, the CH3b regtest suites drive:
- equal-work forks arriving in both orders;
- `preciousblock` on the losing branch; repeated calls (Core's `nBlockReverseSequenceId` decrements); the counter resets only when the tip's chainwork exceeds `nLastPreciousChainwork`, i.e. the chain has been extended (`v:3868-3871`), so an equal-work reorg does **not** reset it (a test pins both);
- a candidate whose connect fails, with the batch-top marking.

`getchaintips` statuses (`active`, `valid-fork`, `valid-headers`, `headers-only`, `invalid`, `conflicting`) are the oracle: run on dashd and rdashd with the same submissions, and compared.

### 4.3 The disconnect pool (CONS-25) (D25)

**Core** (`kernel/disconnected_transactions.h`, v24): `DisconnectedBlockTransactions(max = MAX_DISCONNECTED_TX_POOL_SIZE * 1000 = 20,000,000)` (`:18`).
- `AddTransactionsFromBlock(vtx)` reserves the map for `size + vtx.size()`, appends the block's transactions **in reverse `vtx` order** at the list's back, adds `RecursiveDynamicUsage(tx)` for each, then calls `LimitMemoryUsage()` (`:91-100`).
- `LimitMemoryUsage()` pops from the front, which is the earliest added: the transactions of the blocks disconnected first, nearest the old tip ("the front of the list should be the most recently-confirmed transactions", `:33-37`). It pops while `DynamicMemoryUsage() > max` and returns the evicted ones in pop order (`:50-62`).
- The map's hasher is salted (`SaltedTxidHasher`), but `bucket_count` and the list order do not depend on hash values, so the accounting and every output order are deterministic.
- `DynamicMemoryUsage() = cachedInnerUsage + DynamicUsage(iters_by_txid) + DynamicUsage(queuedTx)` (`:80-82`).
- `removeForBlock` (`:103-118`) and `take()` (`:130-135`).

The caller removes the evicted transactions from the mempool recursively, with reason `REORG` (`v:3311-3313`).

**Ours.** Core's class is kernel code that does not depend on the mempool, so CH3b implements it in `dashd-chainstate::reorg::DisconnectedBlockTransactions` and makes it the production `DisconnectPool` (CH1-node seam). MP1b keeps the mempool half of `MaybeUpdateMempoolForReorg` (`v:420-…`) behind `MempoolSink::maybe_update_for_reorg` and adopts the pool as-is. This is a PLAN delta (§11): the row's "disconnect-pool accounting" moves from MP1b to CH3b.

**The accounting is a logical budget** (O17 CONS-25: v24 `ListAndMap`). It is computed by formula, not from Rust's allocations:
- `MallocUsage(n) = ((n + 31) >> 4) << 4` for n > 0 on 64-bit (`memusage.h:51-63`).
- `DynamicUsage(list<CTransactionRef>) = MallocUsage(sizeof(list_node)) · size` (`:162-165`), with `list_node` = {prev, next, shared_ptr} = 32 bytes.
- `DynamicUsage(unordered_map) = MallocUsage(sizeof(unordered_node<pair<const uint256, list::iterator>>)) · size + MallocUsage(8 · bucket_count)` (`:181-184`).
- `RecursiveDynamicUsage(CTransaction)` (`core_memusage.h:28-36`): the `vin` and `vout` vectors at their capacities, plus each `scriptSig` and `scriptPubKey`'s heap part (a prevector over 28 bytes). `vExtraPayload` is **not** counted, and neither is the `CTransaction` object or its `shared_ptr` control block.
- **`bucket_count`** follows libstdc++'s `_Prime_rehash_policy`. `reserve(n)` is `rehash(_M_bkt_for_elements(n))` (`bits/hashtable_policy.h`), and `rehash(c)` takes `max(_M_bkt_for_elements(size + 1), c)`, rounds it with `_M_next_bkt` (the small-size fast table, then the prime list), and rehashes whenever the result differs from the current count, **downwards included** (`bits/hashtable.h:2729-2741` in GCC 15's headers). Growth on insert happens when the load would exceed 1.0; `clear()` keeps buckets. `AddTransactionsFromBlock` calls `reserve(size + vtx.size())` on every call (`:93`), so after `LimitMemoryUsage` evicts entries the next add can **shrink** the table, which changes `DynamicUsage` and the next trim point. CH3b ports `rehash`, `_M_next_bkt`, `_M_need_rehash` and the prime table exactly (about 100 lines plus the table).
- **Capacities equal sizes** after Core's deserialization: `UnserializeVectorContents` reserves in 5 MB batches (`serialize.h:800-817`), and no `vin` or `vout` under a 2 MB block reaches a batch boundary. The port asserts this.

The reference is Core built with libstdc++ on x86_64 Linux (the oracle pins). Core's own trim point differs on libc++ (macOS) and MSVC. rdashd keeps the libstdc++ figure on every platform and declares the residual an allowed difference (`compat/ALLOWED-DIFFS.md`; DEC-102's pattern). That is not stricter than Core. The libstdc++ version is part of the reference: the vector file records the one the oracle shim links, and a different libstdc++ policy is the same allowed difference. **Vectors:** a Core-linked test (O2a-style, in the oracle shim branch) feeds generated blocks to a real `DisconnectedBlockTransactions` and records `(DynamicMemoryUsage, bucket_count, evicted)` after every call. The generated sequences include evict-then-add runs (a large block, then many small ones under a budget that evicts) so the shrink path is exercised. CH3b's port must match every row.

**The seam needs two methods.** CH1-node's `DisconnectPool` has only `add_from_block` and `remove_for_block` (`seams.rs:408-415`), and `MempoolSink::maybe_update_for_reorg` receives the pool boxed. MP1b cannot drain it without Core's `take()`. So the seam gains `take(&mut self) -> Vec<TxRef>` (`:130-135`) and `dynamic_memory_usage(&self) -> usize` (for the vectors and O15). Slice a2 lands both on the trait, with the `()` implementation returning empty and 0 (§11).

**Without a mempool** (until MP1), `NullMempool::new_disconnect_pool` returns a null pool, as Core skips the pool when `m_mempool` is null (`v:3308`). CH3b's tests use a recording `MempoolSink` that keeps the pool's outputs and the `remove_recursive`/`maybe_update_for_reorg(add)` calls, and compare them with Core's call sequence on the same reorgs (O15 owns the live comparison).

### 4.4 Invalidate, reconsider, precious; descendants

The control flow is CH1-node's port. CH3b's acceptance runs it with real blocks:
- **`invalidateblock`** of a block in the active chain:
  - disconnects one block at a time, each in its own pool, re-adding transactions only for the first 10 (`v:3958-3965`);
  - marks `FAILED_VALID` on the target and on every disconnected descendant (`v:3979`): the flip to `FAILED_CHILD` (`v:3983-3988`) compares the walk tip's parent with the block disconnected before it, which lies above it, so it never holds (CH1-node §4.6, kept verbatim);
  - re-adds candidates that are not worse than the new tip: `InvalidateBlock` itself does it per iteration from its `candidate_blocks_by_work` map (`v:3989-3997`), and again over the whole index at the end (`v:4021-4025`). `InvalidChainFound` (`v:2034-2056`) only updates `m_best_invalid` and logs.

  Invalidating a non-active block marks it and leaves its descendants for FindMostWorkChain to mark lazily, as Core does.
- **`reconsiderblock h [ignore_chainlocks]`** (`rpc/blockchain.cpp:2050-2059`): Core's two passes.
  1. The block and every descendant that is not fully valid lose their failure flags, and with `ignore_chainlocks` their `CONFLICT_CHAINLOCK` bit (`v:4177-4192`).
  2. Each ancestor carrying a failure or conflict bit loses its failure flags the same way. Each ancestor's direct children that carry `FAILED_CHILD` (siblings of the path) become `FAILED_VALID`, which is Core's quirk (`v:4194-4220`).

  Every reconsidered block is then re-admitted through `TryAddBlockIndexCandidate` (`v:4222-4230`), which skips `CONFLICT_CHAINLOCK` (`v:4241-4243`).
- **`preciousblock`**: §4.2.

Descendant handling is FindMostWorkChain's lazy marking plus load's `FAILED_CHILD` derivation (SB4). **`BLOCK_RESULT_UNSET`** failures (`bad-cb-amount`, `bad-cb-payee`) are `FAILED_VALID` like any other and are cleared by `reconsiderblock` (FR-CON-32). A test pins it.

### 4.5 ChainLock enforcement entry points and CONS-11 (D26)

| Entry | Core | Rust owner | CH3b's test |
|---|---|---|---|
| A new best CL arrives | `chainlock` handler → `EnforceBlock` + `ActivateBestChain` (`v:4048-4078`) | `ChainCommand::EnforceChainLock` (A1 §2.10) → CH1-node `cl_enforce` | a CL on a fork block: the active chain's conflicting blocks are disconnected and marked; ABC moves to the locked branch |
| A block conflicts with a CL at connect | `ConnectBlock` step 4 (`v:2516-2519`) | CH3a | an attempt on a conflicting block yields `CHAINLOCK` `bad-chainlock`, `FAILED_VALID` |
| A proposal conflicts | `TestBlockValidity` (`:4936-4939`) | CH3a | `INVALID_PREV` `bad-chainlock` |
| A candidate has a conflicting ancestor | FindMostWorkChain (`v:3545-3561`) | CH1-node | descendants marked `CONFLICT_CHAINLOCK` **and dirty** |
| Reconsider across a CL | `ResetBlockFailureFlags(…, ignore_chainlocks)` | CH1-node | `reconsiderblock h true` clears `CONFLICT_CHAINLOCK`; without the flag it stays |

**CONS-11** (O17: v24 persists FindMostWorkChain's conflict flags; `BLOCK_FAILED_CHILD` is re-derived at load anyway). CH1-node already sets the bit and the dirty mark at `v:3561`. `MarkConflictingBlock`'s own marks are **not** dirty in Core (`v:4115`, `v:4121`; SB4 D15). They reach disk only if a later write dirties the same entry, and CH1-node keeps that (CH1-node.md:864). CH3b proves both halves across a restart:
1. **Persisted.** A conflicting fork with descendants reached by FindMostWorkChain → flush → restart → `getchaintips` shows the descendants `conflicting` on both dashd and rdashd.
2. **Not persisted, as Core.** A block marked only by `MarkConflictingBlock` and never dirtied → flush → restart → rdashd's `getchaintips` equals dashd's after the same steps. The expected status is whatever dashd reports, not a fixed `valid-fork`/`valid-headers`: it depends on the chainwork order and on whether a ChainLock is learned again at load. The test also asserts that the entry's stored status lacks `CONFLICT_CHAINLOCK`.

The fixture is O14's (O17 lists CONS-11 under O14). CH3b's version runs in-process against `NodeSut` with Core's records as the expectation.

### 4.6 Crash safety: a reorg across a flush, and F01 (D27, D28)

**The protocol is CH2's.** A freeze labels `(id, q+1)` after a connect and `(parent(d), q+1)` after a disconnect (CH1-node D3). F is written by W while V continues. Recovery replays the `head_blocks` transition coins-only, then activates through ordinary `connect_block` (A1 §3.5.3 I3). CH2 §6.4 C3 already covers "a reorg in the window" at ChainSut level with a stub connect. CH3b reruns it with the real connector. The cases:
- **C3-real (ChainSut, in-process).** Branch A (k blocks above the fork) is active and F is frozen at A's tip. V disconnects to the fork and connects branch B (k+1 blocks, spending coins restored by the disconnect and creating new ones, with real scripts). Grid: k ∈ {1, 2, 6}, F in flight across the disconnect leg, the connect leg, or both; crashes of both kinds at every W event and every recovery event (depth 1 both kinds, depth 2 sampled). **Requirement:** after recovery and ABC, the UTXO set's MuHash, the tip, every block status and every index record equal an uncrashed run's; no `FAILED_*` on a valid block (R6(d)); no truncated image.
- **Node-level loop (regtest process, `-dbcrashratio`, CH2 §6.4 C9 hook).** A scripted reorg loop on two nodes with `-dbcache` small enough to force split flushes. Kill points are random and the node restarts each time. After every restart: `gettxoutsetinfo muhash` equals the reference node's at the same tip, and `getchaintips` agrees. This is the `feature_dbcrash` shape restricted to reorgs. CH5 owns `feature_dbcrash` itself.

**F01** (A1 §3.5.5 step 6, `A1-architecture.md:1858`; PLAN M3 exit 2): an accepted-but-script-invalid block persisted before its connection is rejected after a restart. Two levels:
- **CH3a, C11's connect half (ChainSut):**
  1. Accept B (`HAVE_DATA`, `VALID_TRANSACTIONS`) with an invalid signature.
  2. Make a durable index-only write before ABC connects B.
  3. Crash; recover.
  4. ABC connects B through `connect_block`, gets `block-validation-failed` or the serial reason, and marks `FAILED_VALID`.
  5. Restart again: `FAILED_VALID` is durable and the tip is B's parent.

  This also covers T20/T21's connect halves: B1 never names an unapplied transition, and an invalid block in the marker path is refused at recovery (R6(a, d)).
- **CH3b (regtest process)** (D39). `Flush` is a class-M command, which V serves only when it runs or at a release point (CH1-node §2.4). A hook that parks V and then waits for a `Flush` would deadlock. So **V makes the durable write itself, before it parks**:
  1. A V test hook sits at the release point between `AcceptBlock` and ABC for a submitted block (the kind CH1-node §9 adds for its ChainLock schedules, R1-M2). On V, it runs `flush_state(ALWAYS)`. That is CH2's documented ALWAYS wait on W, and it makes B's index record durable while the coins stay at B's parent.
  2. The hook reads B's record back through a read-only handle on the durable index store, not the cache. It sends the test `F01Ready { epoch, record }` and then parks V.
  3. **Before `kill -9`, the test asserts** that the record is durable, has `HAVE_DATA`, carries no `FAILED_*` bit, and that its epoch is the flush's.
  4. `kill -9`; restart without reindex.
  5. `getchaintips` shows B `invalid`, the tip is unchanged, and the debug log has Core's `ConnectBlock`/`InvalidChainFound` lines.

  The rejected alternative was a pause that keeps serving class-M commands. It adds a "parked but serving" V state to CH1-node for one test.

---

## 5. Concurrency

### 5.1 Who owns what

| Thread / actor | Owns | Takes | Waits on |
|---|---|---|---|
| **V** (CH1-node's command loop) | `HeaderChain` (`&mut`), `NodeBlockIndex`, `CoreConnector` (the overlay, the arenas, the `ChainstateCell` guard during an attempt), `VContext`, the candidate set | the `ChainstateCell` mutex (leaf), L6 (ST2's `BlockFiles` mutex, which `GuardedFiles` wraps) for `write_undo`/`read_undo`, L3 (the EV gate) exclusively (CH7b) | the check-queue scope (CS6 `control`), W only at CH2's documented waits (`CACHE_CRITICAL`, `ALWAYS`, manual prune) |
| **Check-queue workers** (CS6's rayon pool, `-par − 1` threads) | nothing; they borrow `ScriptCheck<'s>` data (`TxSlot`s, `SpentScripts`, `&SigCache`) for the scope | the sigcache's internal lock (leaf) | nothing |
| **W** (CH2b) | F, its batches | L6 at the files barrier (I/O only) | the store |
| RPC / P2P / dashd-locks | — | — | `ChainCommand` oneshot replies (`await`) |

### 5.2 Locks and order

CH1-node's L1–L8 and LO-1…LO-9 are unchanged (CH1-node §2.6). L6 already covers V's undo writes ("V's appends and undo writes; W's barrier (I/O only)"), so CH3a's undo I/O adds no edge. It adds two leaves to L8's class and one wait rule:
- **L-cell** (`ChainstateCell`'s mutex, D2). Taken only on V, and always with `try_lock().expect("L-cell is a V-only leaf")` (D50), so a violation panics in tests instead of hanging. In production the panic is V's, so it trips the latch. V may hold it across its waits on the check-queue scope and on W at CH2's documented waits (`CACHE_CRITICAL`, `ALWAYS`, manual prune), during which the `FlushSite` holds the coins borrowed from it (`FlushCx { coins: &mut CoinsCache }`). Neither the workers nor W ever take it, so neither wait can block on it.
- **L-sig** (CS6's cache locks). Workers and V hold it only inside one cache operation.
- **LO-9 and the check queue.** The check-queue scope is a wait of V on the pool. Pool workers take only L-sig, send no `ChainCommand`, await no actor reply and wait on nothing else, so LO-9 ("nothing V waits on ever waits on V") holds. CH3a adds the pool to LO-9's list of tasks V may wait on, and marks the workers with the no-wait marker. The pool belongs to the connector alone: MP1's stage-A script checks use their own `CheckQueue` or run inline, so ATMP never waits for V's block.

The EV addendum's order (V dispatch → L3 gate → L4 facade cache mutex → L5 snapshot-cache lock) is CH7b's. L-cell and L-sig are never held while taking L3.

### 5.3 Cancellation and failure

- A connect is not cancelled midway. Core is not either: `ShutdownRequested` is checked between ABC steps (`v:3841`), which CH1-node ports.
- The check queue finishes the block's queued checks before `control` returns, including on an early `Err` (CS6).
- A panic in a check is re-raised on V after the workers join (CS6). V's panic path trips the latch, as any V panic does.
- A `Fatal` inside the attempt runs `finish_attempt` first (effects applied, attempt released), then propagates. Nothing is committed: the commit follows a successful `finish_attempt` only (D33). If applying effects fails, the latch is already tripped, and the target still holds `prev`.
- A readiness refusal (§2.17) is not a cancellation: the attempt ends as any `SystemError` does, and on the tip path V stops.

### 5.4 How callers wait

All waiting is on `ChainCommand` replies (A1 §2.10; CH1-node §3):
- `submitblock`, `invalidateblock`, `reconsiderblock` and `preciousblock` await their command's reply, which is sent after the command's ABC finishes and its signals are staged and released (N1);
- GBT proposal and `CreateNewBlock` await `TestBlockValidity`;
- P2P block processing does not await connects: the peer actor learns the verdict through `BlockChecked` (CH1-node §8.2), and the RPC wait helpers (`waitfornewblock` etc.) watch `ChainView`;
- none of these holds a lock, a gate or a cache guard while waiting (EV addendum liveness rule; LO-9's `ActorReply` refuses a wait from a V-marked task).

---

## 6. Performance

### 6.1 The hot path and where the time goes

Core's per-input work in `ConnectBlock` is about six coin lookups per input:
1. `HaveInputs`;
2. `CheckTxInputs`' `AccessCoin`;
3. the `prevheights` `AccessCoin`;
4. `GetP2SHSigOpCount`'s `AccessCoin` (P2SH is always on);
5. `CheckInputScripts`' `AccessCoin`;
6. `SpendCoin`.

The rest per input is the script (or nothing below assumevalid), plus per-block `CheckBlock` hashing (merkle, already-cached txids), the undo serialization and its SHA256d checksum, and the coins commit.

Two Core shortcuts are ported, because without them a tip connect does more than dashd by construction:
- **The `fChecked` memo** (D42). A block that arrives through `ProcessNewBlock` and is connected from the same object skips `ConnectBlock`'s `CheckBlock`: the merkle tree (SHA256d per transaction on a box without SHA-NI) and per-transaction `CheckTransaction`. PF0's `connect-latency-tip` row compares like with like only with the memo.
- **Probe before copy** (D43). An exec-cache hit (the common case at the tip, where transactions are mempool-validated) costs one probe, with no coin lookup, decompression or copy.

With `-assumevalid` default (IBD), the coins work and the I/O behind it dominate. With `-assumevalid=0` (L2), secp256k1 dominates. CS6's evidence for a P2PKH input is 461.0 K instructions cold and 38.6 K warm (a sigcache hit). The verify alone is 421.6 K against dashd's 379 K, so CS6's own BG is not met yet (CS6 F1/F2; BL-160 tracks the gap). The `-noav` rows below inherit that gap until BL-160 closes (R9).

**What runs in parallel:** script checks (CS6, `-par`, Core's batching; the same as Core). Nothing else in CH3a, because `ConnectBlock`'s order is serial by definition. CH4's S2 prefetch runs beside V and is CH4's.

**What is cached:** coins (CH2), signatures and script executions (CS6), txids (`Tx::txid()` is cached in dash-consensus's primitives), deployment states (CS4's `VersionBitsCache`). Nothing new in CH3a.

### 6.2 Levers (each its own slice, each differential-tested, none claimed before PF0 measures; D22)

| # | Lever | Mechanism | Equivalence argument and test |
|---|---|---|---|
| L1 | **One resolve per input** | a per-tx `TxCoins` memo implementing `CoinsView`: `have_inputs` resolves each input once into a small vec (value, height, coinbase, script `&'s`); `check_tx_inputs`, `sequence_locks`, `transaction_sigop_count` and the script copy read it; `spend` is the second and last lookup. Six lookups become two. A memo miss for an input the resolve found present is a logic error, so `Err(Fatal)`, never a verdict; an absent input keeps CS5b's `bad-txns-inputs-missingorspent` path | the overlay is not mutated between the first read and `UpdateCoins` (only L8 mutates); a transaction's outpoints are distinct (CheckBlock → `bad-txns-inputs-duplicate`, run at step 3); `xcheck` mode compares every memo read with a direct overlay read; L2 and `diff_chain` run with it on |
| L2 | **No per-input allocation** | `SpentScripts` (a `Bump` whose largest chunk survives `reset`), the streamed undo `Vec`, the `prev_heights` and per-tx script-ref vecs kept across blocks; `slots` is the only per-block allocation, plus CS6's per-tx `Vec<ScriptCheck>` (Core allocates `vChecks` per tx too) | PF0 counting allocator: per-block allocations bounded by `txs + O(1)` in parallel mode and `O(1)` below assumevalid |
| L3 | **Vectored undo write** | ST2 `write_undo_slice` (§2.10.1) | byte-equal records (digest `undo`) |
| L4 | **`undo_hash` only with a sink** | §2.10.1 | — |
| L5 | chained-spend borrow | borrow in-block outputs' scripts from `block` instead of copying | only if PF0 shows it; same xcheck |

**Amdahl, honestly.** CH3a's levers remove lookups and allocations, which is part of the coins share of a dashd `-reindex-chainstate` profile (PF1 measures `Connect transactions`). The program's 2× target on `reindex-chainstate` (CH2 §5.6) needs CH4's prefetch and pipelining, CH2's layer and CS6's verify. CH3a's commitment is narrower and gateable:
- (a) **never slower**: instructions per block ≤ dashd's on the same workload, `-par` and `-dbcache`;
- (b) **the lookup and allocation counts** above.

### 6.3 PF0 rows (D22)

| Row | Workload | Gated on | Slice |
|---|---|---|---|
| `crit:dashd-chainstate connect_block_micro` (+ iai) | connect 8 fixed mainnet blocks against a warm in-memory cache: an empty coinbase block, a 500-input P2PKH block, a P2SH-multisig-heavy block, a pre-DIP1 1 MB block, a DIP1 2 MB block, a chained-spend block, two `bad-*` failures; each at assumevalid on/off, `-par=1` and `-par=4` | instructions:u, allocations | a3, a4, a9 |
| `crit:… disconnect_block_micro` | disconnect the same blocks | instructions, allocations | a5 |
| `reorg-depth-{1,6,30}` | regtest synthetic, in-process, real connector | instructions per block, allocations | b2 |
| **`reindex-chainstate-pre-DIP3-{p300, p8}`** and **`…-noav-{p300, p8}`** (BG) | rdashd `-reindex-chainstate -stopatheight=1028160` on the node-bench mainnet data copy (PF0-N's per-gate copy rule), `-assumevalid` default and `=0`, `-dbcache=300` and `=8` | instructions:u (total and V-thread), allocations, syscalls, I/O bytes, page faults; vs the same row run on dashd v24.0.0-rc.3 | a7 (rows land), a9 (levers) |
| `connect-latency-tip` | regtest: connect one 1 MB block of mempool-validated transactions (exec-cache hits) | instructions on V | a9 |
| `cpu_ms_per_block` (M3 exit 6) | derived from the reindex rows: total CPU time per connected block | ≤ dashd's (reported per row; wall-clock evidence only in reserved windows) | a7 |

- **Gate:** a regression above max(noise, 0.5%) against the previous head blocks (PF0). The dashd comparison is reported per row and must stay ≤ 1.0×: a breach is a CH3a bug, not noise.
- Wall-clock times are recorded at the M3 milestone window only (the box's load makes them noise otherwise).
- The rows that need a node are PENDING until PF0-N's out-of-process path admits them (PF0-N plan); they then name CH3a as owner.

---

## 7. Tests

### 7.1 L0 and L1 (per slice)

- **a1 (pure):** boundary tables for `block_script_flags`, `lock_time_flags`, `is_bip30_repeat`, `block_proof_equivalent_time` (vs Core's `chain.cpp` on synthetic indexes), `block_value_valid` and the old-budget branches (every height class on mainnet, testnet, regtest; each `strError` branch logged), and `stats` (vs the extractor's golden on 1,000 sampled mainnet blocks; `conformance/src/digest/extract.rs` already parses them).
- **a3–a5 (directed):**
  - Each reject string in §2.2–§2.3 and §2.12 has a minimal block that triggers it, first through `connect_block` and then through V. The census script greps `validation.cpp:2469-2935` and `:2282-2378` for `state.Invalid`, `AbortNode` and `LogError`, plus `bad-tx-type-dip3-inactive` (D32) and `bad-cbtx-payload` (D54), and fails if a string has no test.
  - The readiness guard's tests (§2.17), the commit-after-effects fault injection (§3.2), the IS-loop sequences (§3.3) and the §2.1.1 sink sequences.
  - The memo: a block connected with `checked = true` and the same block read from disk give identical outcomes on every directed case that passes `CheckBlock` with both checks on (D42). A `CheckBlock`-invalid case always runs with `checked = false`, as `ProcessNewBlock` never marks such a block.
- **a5 (undo reads):** a corruption matrix over a real rev file. It damages the size field (in bounds and out of bounds), truncates the record, flips a byte in the record and in the checksum, and unlinks the file under a cached handle. For each, it asserts the §2.10.2 row. dashd, given the same rev file and the same `invalidateblock`, is the oracle for the outcome and the `what()` text (D45, D46).
- **Fuzz targets** (D48; registered in node-fuzz smoke with the CH3 slices that own them):
  - `undo_decode` (a5): `read_undo_record`'s two paths over arbitrary bytes never panic, agree with each other, and round-trip whenever decode succeeds;
  - `connect_disconnect_roundtrip` (a5): random small blocks over a random UTXO set. Connect then disconnect restores the set byte for byte, with `Unclean` vs `Failed` classified as Core's rules say;
  - `disconnect_pool` (b1): operation sequences checked differentially against b1's Core-generated vectors.
- **L1:** O2a's contextual entry points (`oracle_check_tx_inputs`, sequence locks, sigops) already cover CS5b. CH3a's own differential is O2b (§7.8).

### 7.2 L2 replay (CH3a acceptance)

- **Input.** The BS2 oracle's blocks for mainnet 0→1,028,159 and testnet 0→6,999, read-only, fed through O9a-2's replay driver into V (`ProcessBlock`, reindex-equivalent, no P2P), with checkpoints on, `-par` at maximum and **`-assumevalid=0`**. The helper is `IbdHelper`.
- **Output.** rdashd's O8 digest (`C` records with `undo`, header fields, `bls`, `bur`, `dep`, and the golden fields from §2.15) is compared with the v24 oracle's goldens: `dash-conformance digest compare`, linear mode, `--from 1 --to 1028159`, with `--require` on `muhash undo stats bi txouts amount unspendable bls bur dep` (the deployment answers are in M3 exit 1).
- **Pass:** exit 0, every key covered (O9a D2), no unrecorded heights.
- **At the window's last block** (M3 exit 1): `hash_serialized_2` and the full `dumptxoutset` set-diff against the oracle's, and `verifychain 4 0`.
  - CH3a provides `hash_serialized_2` as a pure function in a1's `coins::stats` (Core's legacy per-txid stream, `ApplyHash(HashWriter&, …)` at `kernel/coinstats.cpp:72-88`, selected at `:163`, over the UTXO set in key order) and a `dump_utxo` iterator over the coins store, both on V.
  - RP1a's `gettxoutsetinfo` and `dumptxoutset` then wrap them.
  - **`verifychain 4 0` is CH5's, and M3 exit 1 depends on CH5 for it** (§11; D36). Levels 3–4 need the scratch view CH5 builds with CH2b's owner. CH3a's one-overlay-per-call connector cannot run them, so rev1's interim "levels 0–3 over CH3a's connector" is withdrawn. CH3a lands only the seam's `ViewTarget` parameter and the connector's side of it.
- **A second run with assumevalid on** must give the same digests, and it must really take the skip (D44). A reindex-style replay alone would never skip: its best header would be the window's last block, so the assumevalid block is not in the index, the chainwork is below `MinimumChainWork`, and the last two weeks fail condition 5.
  - So the driver first submits the oracle's **header chain to its tip** (headers only).
  - It then uses the chain's `defaultAssumeValid` (mainnet 2,487,500, testnet 1,400,000; `chainparams.cpp:231`, `:440`) when that header is in the preloaded chain, else `-assumevalid=<oracle tip>`. It keeps `-minimumchainwork` at its default when the preloaded chain meets it, else lowers it to the preloaded tip's work. Both choices are recorded in the run's manifest.
  - **It asserts the skip count**: every block of the window except genesis skips its scripts.
- **The reverse leg** (D48). After the forward run, the tap disconnects back through `disconnect_block` and compares, at each height `h`, the running MuHash, `txouts` and `amount` with golden(h − 1). It uses `ReverseBlock`, §2.15. This is the only test of `ApplyTxInUndo` on real mainnet-scale undo.
  - On the per-PR run it goes from 625,000 down to 614,000 (11,000 blocks, crossing CSV 622,944, CLTV 619,382 and the superblock start 614,820).
  - On the nightly run it takes the last 10,000 blocks.
- **Regtest FT recordings** (O8 §13.1): the non-evo FT chains replayed with `RecordedHelper`, strict mode, stream comparison `--org tip`, gated on O9a D6's clean pairs.
- **Testnet coverage note** (n6). The testnet window 0→6,999 crosses DERSIG (2,075) and CLTV (2,431) but not CSV, which activates at 8,064 on testnet (`chainparams.cpp:392`). CSV activation is covered by mainnet's window and by `feature_csv_activation` (§7.5).
- **Cost and the per-PR window** (D34).
  - The full mainnet run is about 1 h of V time at `-par` max on an unloaded box (dashd's reindex-chainstate over that window is about that long). It runs nightly and at merge.
  - **Per PR**, until segment seeds exist (a12, on O9b's snapshots), the run replays **from genesis through 625,000** with per-block MuHash, then the reverse leg. Early Dash blocks are small, so this is well under the full run, but a7 measures it.
  - **If its measured V time exceeds 45 min at gate load**, the per-PR check switches to a **suffix check**, and the switch is recorded as a new D-row:
    - start from the nightly run's chainstate at 599,999, kept as a gate artefact with that run's provenance;
    - seed the counters from golden(599,999);
    - replay 600,000→625,000 and compare every field except `muhash`, then run the reverse leg on counters only.

    That check claims no per-block MuHash parity; the nightly full run keeps it.

### 7.3 L3: non-evo mutation operators at 100% (D23)

- **Harness:** `dash-conformance l3`, in-process.
  - Templates come from dashd regtest (`generateblock … submit=false`, compressed activation heights).
  - Each mutation is applied, then submitted to rdashd's V (`ProcessBlock`, and `TestBlockValidity` for proposal mode) and to dashd (`submitblock` and `getblocktemplate {"mode":"proposal"}`) on a **disposable** datadir (A1 §4.7).
  - The comparison covers verdict, reject string, result class and surviving effects (none, for non-evo).
- **Both sides' regime** (D49). The value-bound operators are judged by `NodeHelper` with `sb_required = false`, so dashd must be in the same not-synced regime. Regtest's budget window (from 1,000) and superblock heights (from 1,500, cycle 20; `chainparams.cpp:801-806`) lie above the default DIP3 (432), where G1 refuses.
  - Both sides run with `-dip3params=2000:2000 -testactivationheight=v20@2000 -testactivationheight=mn_rr@2000`, `feature_block`'s settings.
  - dashd has no peers and is never sent `mnsync`.
  - Each case first asserts that dashd's `mnsync status` reports `IsSynced: false`. A case that finds dashd synced fails as a harness error, not a verdict difference.
  - With `GuardScope::Declared` (regtest), G2 does not refuse these heights.
- **Operators (non-evo subset of A1 §4.7):**
  - coinbase value ±1 at every value-bound branch (regular, budget window, superblock height off-trigger);
  - locktime and sequence at the MTP and height boundaries;
  - sigops at limit and limit + 1 (pre-/post-DIP1);
  - sizes at 1,000,000 / 2,000,000 ± 1;
  - the duplicate-tx merkle mutation (CVE-2012-2459 → MUTATED, never cached invalid);
  - header nBits ±1 and time edges (CH1-node's checks, here through a full block);
  - tx version and type, including a special-typed transaction below DIP3 (`bad-tx-type-dip3-inactive`, D32) and version-1 and version-2 coinbases with raw type 5 (`bad-cbtx-payload`, D54). A version-3, raw-type-5 coinbase below DIP3 is a G1 refusal: it is outside the non-evo census and moves to CH7a's evo operators;
  - missing, spent and duplicate-in-block inputs;
  - premature coinbase spend;
  - in < out, value range, accumulated fee range;
  - BIP30 duplicate txid (regtest enforces it);
  - invalid signature at a P2PKH input and inside P2SH;
  - NULLDUMMY, DERSIG, CLTV and CSV script failures at their activation boundary;
  - and the **multiple-failure blocks of §2.4**, run with rdashd and dashd both at `-par=1` and both at `-par=4`.
- **Census:** every `bad-*` string reachable from `ConnectBlock`'s non-evo steps and from `CheckBlock`/`ContextualCheckBlock` has at least one case.
- **Pass:** 100% identical over the census. Peer-penalty parity is the class (P2b maps it), checked end to end by `p2p_invalid_block` (§7.5).

### 7.4 ChainSut connect halves (CH3a acceptance, from CH2 §6.4)

CH2b's `ChainSut` with its mini connect replaced by `CoreConnector`, synthetic blocks with real scripts, and ST3a's explorer:
- **T12** (planted: `HAVE_UNDO` before `write_undo` returns) is caught (R1, D1(a)).
- **C11's connect half**: §4.6 F01, ChainSut level.
- **T20** (B1 before the transition's blocks were applied) and **T21** (an invalid block in the marker path), connect halves (R6(a, d), R3).
- **FA7** with the real connector: a read fault mid-connect yields `Fatal` and no verdict, status change, event, or reply.

### 7.5 Named FTs (CH3a, CH3b, CH7a) and CH3b's regtest suites

- **FT ownership** (D37). A1 Appendix D's M3 tier-1 row (PLAN M3 exit 5) lists eight FTs that test exactly this code and that no PLAN row owned. The two that cross DIP3 leave M3 exit 5 for CH7a's acceptance, and exit 5 points to CH7a (manager ruling). SW-FT runs the passlist, but it does not own the `ConnectBlock` behaviour that makes them pass. All run unmodified from Core's v24 tree and need ND1, RP1a (`submitblock`, `generate*`, `invalidateblock`) and P2b's block handling (CH1-node §9.8).

  | FT | Owner | Wiring slice | Why it fits there |
  |---|---|---|---|
  | `feature_block` | CH3a | a11 | Core's own end-to-end test of §2.2–§2.5: BIP30, CVE-2012-2459, sigops, sizes, maturity, the value bound, script failures, reorgs. `-dip3params=2000:2000` with v20/mn_rr at 2000 (`feature_block.py:91-97`) |
  | `feature_cltv`, `feature_dersig` | CH3a | a11 | flag activation and the `CheckInputScripts on … failed with …` log line (D30). Both run `-par=1` for the serial reason (D5; `feature_cltv.py:92`, `feature_dersig.py:54`). `-dip3params=9000:9000` |
  | `feature_csv_activation` | CH3a | a11 | sequence locks and CSV flags across activation, `-par=1` (`:103`). `-dip3params=2000:2000` |
  | `feature_assumevalid` | CH3a | a11 | §2.8's decision with real headers (D44). `-dip3params=9000:9000` |
  | `feature_abortnode` | CH3b | b2 | an unlinked `rev00000.dat`, a reorg, "Failed to disconnect block" (D46), then `assert_start_raises_init_error`, which needs CH5's startup VerifyDB. CH5 depends on CH3b, so the FT closes after CH5 merges; b2 runs the reorg half earlier as a regtest process test (D55) |
  | `feature_nulldummy`, `feature_bip68_sequence` | CH7a | CH7a's | they cross DIP3 (`-dip3params=105:105`; regtest's 432), so G1 refuses them before CH7a |
  | `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `p2p_invalid_block` | CH3b | b2 | as before |

  **Sequencing** (D55; Opus r2 M2, manager ruling). PLAN has RP1a ← CH3b ← CH3a, so RP1a cannot be any kind of CH3a dependency: an `i:` edge still constrains CH3a's merge (`PLAN.md:139`), and PLAN has no slice rows. The five FTs are therefore CH3a acceptance that closes after RP1a merges, worded as CH3b's own FTs already are (they also need RP1a, `PLAN.md:571`, `:626`). CH3a merges on a1–a10, and CH4, MN0, CH7a and CH3b wait for that merge only. The same pattern covers `feature_abortnode` (CH3b acceptance closing after CH5 merges, since CH5 ← CH3b) and a12 (CH3a acceptance closing after O9b merges, since O9b ← CH3a).
- **Regtest reorg suites (in-process, `NodeSut`):** depth 1–30 reorgs with spent and restored coins; one 300-block reorg that crosses several 32-block ABC batches, with blocks large enough that the disconnected transactions exceed the pool's 20,000,000-byte budget and force eviction (FR-CHS-07: no depth limit beyond Core's; the pool evicts but never refuses); equal-work races and precious; invalidate and reconsider at the tip, mid-chain and on a fork; `RESULT_UNSET` reconsider; CL enforcement onto a fork (§4.5); a reorg that runs into an invalid block mid-batch (the batch-top marking); and a reorg whose disconnect needs legacy undo (the CH2 §3.5.2 fixture, with the debug assertion off for this case). After each scenario, rdashd's tip, `getchaintips`, MuHash and block statuses equal dashd's on the same submissions.

### 7.6 CH3b: CONS-25 and CONS-11

The pool's accounting vectors (§4.3) and the recorded `MempoolSink` call sequence; the two CONS-11 restart cases (§4.5).

### 7.7 CH3b: crash

C3-real and the node-level `-dbcrashratio` reorg loop (§4.6); F01 at node level, with V's own durable write before it parks (D39).

### 7.8 The interface O2b's `diff_chain` drives (D24)

O2b compares a Core regtest chainstate linked into the shim with `dashd-chainstate` on generated operation sequences. CH3a provides the Rust side as **`NodeSut`**: production V and `CoreConnector` over an in-memory `KvEngine` and in-memory flat files, with a mock clock and a scripted `ChainHelper`.

```rust
// dashd-chainstate/src/sut.rs (test-support feature; the shim side is O2b's)
pub struct NodeSut { /* V thread, connector, MemEngine, MemFlatFs, ScriptedHelper, MockClock */ }
pub enum Op {
    Block(Vec<u8>), Header(Vec<u8>), Proposal(Vec<u8>),
    Invalidate(BlockHash), Reconsider(BlockHash, bool), Precious(BlockHash),
    EnforceChainLock(ClSig), Flush(FlushStateMode), Restart, SetTime(i64),
    Answer(HelperScript),              // the next attempts' helper answers (both sides get the same)
}
pub struct StepReport {
    pub reply: OpReply,                 // the ChainCommand's reply (BlockResult, RPC-shaped errors)
    pub records: Vec<statedigest::Record>,  // O8 C/R/D records of every attempt the op caused, with fx and org
    pub tip: Option<BlockRef>,
    pub tips: Vec<ChainTip>,            // getchaintips' rows
    pub utxo: UtxoDigest,               // muhash, txouts, amount
}
impl NodeSut {
    pub fn new(params: ChainParams, opts: SutOpts) -> Self;   // regtest params with -testactivationheight-style overrides
    pub fn step(&mut self, op: Op) -> StepReport;
}
```

- The comparison is O9a's comparator in **stream** mode with `org`, plus `tips` and `utxo` equality per step. O2b's planted bugs (a DML ordering bug, an effect omission) are evo and land at M4. The M3 non-evo set needs `diff_chain` ≥ 24 CPU-h clean (M3 exit 4).
- `Restart` drops V and reopens through CH2b's recovery over the same in-memory stores, after an optional injected crash image. This lets `diff_chain` generate restart and failure sequences.
- `NodeSut` runs with `GuardScope::Declared` and DIP3 raised above every height its generators reach. A generated sequence that crosses the raised DIP3 hits G1, and that is reported as a generator error, not a difference. a10 reruns §2.17's guard tests as `NodeSut` ops.

---

## 8. Implementation slicing

Sizes are agent-days at O-h. Each slice merges alone, with its own gate (program gate, fuzz smoke, PF0 rows where named) and the named reviewer focus. CH3a builds on CH1-node N7b-1 and CS6 (train 13) and on CH2a/CH2b on integration.

### 8.1 CH3a (L; about 22–27 agent-days, a11 and a12 included although they land after RP1a and O9b merge, D55)

Listed in merge order; the dependency sentence below the table is binding (D38).

| Slice | Commit(s) | Contents | Gate | Reviewer focus |
|---|---|---|---|---|
| **a1** | `feat(dash-consensus): connect-block pure rules`; `feat(dash-evo): payments, governance-free branches` | first commit: `block_script_flags`, `lock_time_flags`, `is_bip30_repeat`, `block_proof_equivalent_time`, `has_lock_inputs`, `coins::stats` incl. `hash_serialized_2` and the reverse step. Second: `payments::{block_value_valid, old_budget_block_value_valid, block_payee_valid}` over a `GovernanceView`, in EV3's module (D7) | L0 tables; the stats check vs 1,000 mainnet goldens; the assumevalid decision table, each condition flipped (D44); `oracle_block_value_payee` once O2a-GV lands | branch-for-branch parity with `payments.cpp`, `chain.cpp:147`; amounts; no float; EV3's owner co-reviews the payments commit |
| **a2** | `feat(dashd-chainstate): connector seam for CH3a` | §2.1 seam deltas (D3): `ConnectCx` fields (incl. `guard`), `AttemptOrigin`, `ViewTarget` + the `ScratchView` trait (D36), the `checked` argument (D42), `DisconnectResult`, `test_block_validity`, `VContext` marker in `actor`; `DisconnectPool::{take, dynamic_memory_usage}` (§4.3); `connect_tip` emits `block_checked` for `Ok(SystemError)` but not for `Err(Fatal)` (§2.1); `ChainHelper` (incl. `interim_refusal`) + `NodeHelper`/`IbdHelper`; `EvoHooks` (incl. `COMPLETE`) + `NoEvo`; the `AbortSink` as V's abort path (D52); `NullConnector` updated. No other behaviour change | CH1-node's suites unchanged, plus a test that a `Fatal` connect emits no `block_checked` | seam shape vs the EV addendum (§3.2); `VContext` constructor privacy (a compile-fail test); the target is never inferred from the origin |
| **a8** | `feat(dashd-chainstate): deployments on V, unknown-bits warning` | §2.16; lands before a3 (D38) | warning tests at `MinBIP9WarningHeight`; `active_after` answers at each boundary ±1 on all four networks | the cache is wired on `HeaderChain`, never `RuleContext` |
| **a3** | `feat(dashd-chainstate): ConnectBlock (serial)` | §2.2/§2.3 with `CheckQueue::serial()`, the `fChecked` memo (D42), assumevalid, BIP30, value/payee control flow, `SpecialTxRules`/`MnRewardEra` from deployments (D40), the IS loop (D35), streaming undo, `write_undo_slice` (ST2), T12 order, commit after effects (D33), the tip glue, the genesis case; **the readiness guard** G1/G2, its startup check in CH1-node's load path and `NoEvo`'s two pre-DIP3 checks (§2.17; D31, D32, D54) | directed reject tests; census; ChainSut T12/C11/T20/T21 (§7.4); the guard's boundary and restart tests; the effect-boundary fault injection (§3.2); the IS sequences (§3.3); the §2.1.1 sink sequences; `connect_block_micro` serial rows | order and strings vs `v:2469-2935`; Fatal vs Invalid vs SystemError vs refusal; T12; the guard's predicate vs `specialtxman.cpp`'s gates |
| **a4** | `feat(dashd-chainstate): ConnectBlock on the check queue` | CS6 `control` scope and `control_inline` (D41), `slots`, `SpentScripts` on `bumpalo` (D51), the probe-first pair (D43), the §2.4 matrix, the store policy, one `ScriptCaches` | the §2.4 matrix at `-par` 1/4; a hit-then-miss sequence proving one soft erase per hit; `connect_block_micro` parallel rows | lifetimes; precedence; no exec-cache insert from a parallel run; CS6's owner co-reviews the two CS6 additions |
| **a5** | `feat(dashd-chainstate): DisconnectBlock` | §2.12, `ApplyTxInUndo` with the legacy path, `read_undo_record` (ST2: hint then stream, open by path; D45, D46), `clear_prefetch` for `Active` only, `DisconnectResult`; the `Scratch` arm against a test double; fuzz `undo_decode`, `connect_disconnect_roundtrip` | directed (inconsistent undo, checksum, unclean); the corruption matrix against dashd (§7.1); the legacy fixture; a `Scratch` disconnect leaves active byte-identical; `disconnect_block_micro` | `fClean` semantics; reverse orders; `Failed` vs `Fatal`; the slow path equals Core's stream; ST2's owner co-reviews |
| **a6** | `feat(dashd-chainstate): TestBlockValidity` | §2.14 incl. the guard as an RPC error; `ChainCommand::TestBlockValidity` handler; CH1-A10's `check_header_at` | proposal-mode directed tests; the guard refuses a proposal without stopping the node; MN0 can build on it | `INVALID_PREV` vs `CHAINLOCK`; no index insertion; cache store-both |
| **a7** | `feat(dashd-chainstate): state digest tap and L2 replay` | `DigestSink`, origins, golden fields, MuHash from genesis (D34), `RecordedHelper`; wiring into O9a-2's driver (header preload for the assumevalid-on run, D44); the reverse leg (D48); the reindex-chainstate PF0 rows | **L2 mainnet + testnet pre-DIP3** (§7.2): nightly full; per PR genesis→625,000 + reverse leg, its V time measured against the 45-min rule (D34); the skip-count assertion; FT-recording replays | field definitions vs O8 §5; no golden-seeded accumulator; linear vs stream coverage |
| **a9** | `perf(dashd-chainstate): one resolve per input, allocation discipline` | §6.2 L1, L2 (L3 is already in a3) | `xcheck` on L2 and `diff_chain`; PF0 counters ≤ previous head; dashd ratio reported | equivalence argument for the memo; xcheck coverage |
| **a10** | `test(dash-conformance): L3 non-evo operators; NodeSut` | §7.3 harness, operators and dashd's not-synced regime (D49); §7.8 `NodeSut`; the guard tests as `NodeSut` ops | **L3 100%** | operator census; disposable oracle datadirs only; `mnsync status` asserted per case |
| **a11** | `test(dashd-node): CH3a's functional tests` (after RP1a merges, D55) | the five CH3a FTs on SW-FT's harness (D37); fixes for what they expose land in the owning slice's code | the five FTs green, unmodified | any FT-driven change is traced to a Core line |
| **a12** | `feat(dash-conformance): L2 segment seeds` (after O9b merges, D55) | segment seeds over O9b's snapshots: scan or import Core's serialized `MuHash3072` (D34) | a segment run equals a full replay at its first and last heights | provenance of the seed; no golden-hash seeding |

**Dependencies** (D38):
- a1 and a2 can proceed in parallel. a8 needs a2. a3 needs a1, a2 and a8.
- a4, a5 and a6 need a3 and are independent of each other.
- a7 needs a3, a4, a5 and a8: maximum `-par`, the reverse leg and the `dep` field.
- a9 needs a7, and a10 for its `diff_chain` xcheck.
- a10 needs a4, a5 and a6: NodeSut's `Invalidate`, `Reconsider`, `Precious` and `Restart` need disconnect.
- a11 needs a3–a6 and a8, ND1, RP1a and P2b. RP1a needs CH3b, so a11 lands after CH3a has merged and closes that acceptance late, as CH3b's FT wiring does (D55).
- a12 needs a7 and O9b's snapshots, and lands after O9b merges (D55).
- CH3a merges on a1–a10; a11 and a12 close its acceptance later (D55).

### 8.2 CH3b (M; about 8–10 agent-days)

| Slice | Contents | Gate | Reviewer focus |
|---|---|---|---|
| **b1** | `DisconnectedBlockTransactions` + the libstdc++ bucket policy + `RecursiveDynamicUsage`; the Core-linked vector generator (shim branch); production `DisconnectPool`; fuzz `disconnect_pool` (D48) | every vector row equal | the formula and the bucket policy, including the shrinking `rehash`; eviction order; `take()` semantics (the seam methods land in a2) |
| **b2** | `NodeSut` reorg suites (§7.5); FT wiring for CH3b's four named FTs once ND1, RP1a and P2b are in; `feature_abortnode`'s reorg half as a regtest process test: an unlinked `rev00000.dat`, a reorg, "Failed to disconnect block", the abort; the whole FT once CH5 merges, which closes that acceptance (D37, D46, D55) | suites green; FTs green | Core's precedence in invalidate/reconsider; the `++disconnected <= 10` rule; `getchaintips` statuses |
| **b3** | CL entry-point scenarios; CONS-11's two restart cases | green | that `MarkConflictingBlock`'s marks stay non-dirty |
| **b4** | C3-real; node-level `-dbcrashratio` reorg loop; F01 node-level with V's own durable write before it parks (D39) | green; nightly budget within ST3a's class; F01's pre-kill assertion | crash grid coverage; reference equality |

---

## 9. Decisions (D-rows)

| D | Decision | Why |
|---|---|---|
| D1 | One `CoreConnector`, owned by V, implements `BlockConnector`. CH7a extends it through `EvoHooks` (§3.1), never a second connector | Core's `ConnectBlock` is one function. Two connectors would split its order across crates, and the order is what parity depends on |
| D2 | The coins cache (and CH7a's evo window) live in a `ChainstateCell` (`Arc<Mutex<…>>`) shared by the connector, CH2b's production `FlushSite` (CH2b b1 implements CH1-node's seam, CH1-node.md:1163) and MP1's coins commands. The mutex is a V-only leaf, taken with `try_lock` (D50). If CH2b has already fixed an owner, CH3a borrows from it under the same leaf rule | `FlushControl` takes `FlushCx { coins: &mut CoinsCache, … }` (integration `flush/control.rs:88-110`), and CH1-node holds the connector and the flush site as separate `Send` objects. An uncontended single-thread mutex is the simplest safe sharing. It adds no lock-order edge because nothing else takes it |
| D3 | CH1-node's seam gains: `ConnectCx::{deployments, best_header, min_chain_work, is_ibd, chainlocks, guard, origin}`, a `&mut VContext` argument, a `ViewTarget` argument (D36), the `checked` memo flag (D42), `test_block_validity`, a `DisconnectResult` return (§2.1), `DisconnectPool::{take, dynamic_memory_usage}` (§4.3), and `block_checked` only for `Ok(SystemError)` (§2.1). The connector reports through an `AbortSink` that is V's abort path (D52) | CH1-node D29 reduced `ConnectCx` to what N7 needed and named CH3a/CH7a as the extenders. VerifyDB needs `Unclean` (D15) and the target (D36). O9a needs the origin (D17). The EV addendum needs V's context (D19). The guard needs its scope (D31). MP1b needs `take()`. Core's AbortNode counts need the connector to report what V cannot see |
| D4 | Base slices port Core literally: one Rust function per Core function, same order, same strings, same result classes. Optimizations come only in a9, behind a differential check | review against Core is line-by-line only when the code is. It keeps the CH2 lesson: no invariants of our own until parity is proven |
| D5 | Script-failure reporting follows Core's mode exactly: `block-validation-failed` with threads, the transaction's reason under `-par=1`; non-script failures win over pending checks (§2.4) | Core's behaviour; making parallel mode report specific reasons would be stricter than Core, with no peer-visible gain |
| D6 | BIP30 has no exempt heights (`IsBIP30Repeat` is false in Dash, `v:6785-6789`). BIP34's ancestor-hash shortcut is exact. Disconnect enforces BIP30 always | Core's code; Bitcoin's exemptions do not exist on Dash's chain |
| D7 | CH3a writes the governance-free branches of `IsBlockValueValid` and `IsBlockPayeeValid` into EV3's `dash-evo::payments`; EV3 adds `GetBlockTxOuts`, the MN payee and the trigger branches to the same functions. Governance and MN-sync answers come through the helper; the MN-payment half through hook H5. `oracle_block_value_payee` (O2a-GV) is CH3a's oracle once it lands | the value bound is consensus on the pre-DIP3 window (budget windows, superblock max bound) and L2 needs it at M3, before EV3; one module avoids two ports of one Core function, and PLAN's crate assignment stands |
| D8 | Assumevalid is ported exactly (`v:2542-2571`) with a new pure `block_proof_equivalent_time`. Its scope is CheckInputScripts plus the `fCheckCbTxMerkleRoots` pass-through to hook H4 | A1's m-1 list; the evo half is CH7a's acceptance |
| D9 | CS6 integration: `Vec<TxSlot>` per block before the control scope; the exec cache probed first, and spent scripts copied into a per-block `bumpalo::Bump` only on a miss (D43, D51); scratch buffers are kept across blocks | mirrors Core's `txsdata`, its probe before `AccessCoin` (`v:2174`, `v:2184`) and its `CTxOut` copy. A pre-pass over the whole block would read coins before Core does and change fault timing. rev1's "copy before the probe" rationale is withdrawn (Opus m7): on every hit it cost a lookup, a decompression and a copy per input |
| D10 | Store policy: tip connect consults and does not fill (exec hits soft-erased); `TestBlockValidity` fills both; MP1 per CS6. One process-wide `ScriptCaches` | `v:2750`; CS6's documented contract; one cache is Core's |
| D11 | Undo is Core's `rev*.dat` record byte for byte, written through ST2; there is no other undo store; `undo_hash` is digest-only | §2.10's four reasons; the format was already settled by ST2, CS5a and CH2b's recovery |
| D12 | The undo record is serialized as a stream during the loop. `set_undo` (`HAVE_UNDO`) follows `write_undo`'s return; `VALID_SCRIPTS` follows; the overlay commit is last | T12; Core's order (`blockstorage.cpp:783-799`, `v:2899-2912`); no intermediate `BlockUndo` allocation |
| D13 | The connector owns `Arc<GuardedFiles>` for undo I/O; CH1-node's `BlockFiles` seam is unchanged. ST2 gains `write_undo_slice` and `read_undo_record` (D45); undo reads open by path (D46) | the latch-guarded file handle already exists (`store/guarded.rs:438-490`); extending V's seam would route undo through code that does not need it |
| D14 | The overlay and evo commit happen inside `connect_block`, after `finish_attempt` (D33) and before CH1-node emits `BlockChecked`, the reverse of Core (`v:3432` vs `v:3446`). Declared, not observable | both run in one hold with no release; `BlockChecked`'s listeners read no coins or evo state. A split seam would add an unobservable state. The one trace is debug-only: `-debug=bench`'s Connect and Flush lines split the time differently from Core's, in Core's format (Opus n4) |
| D15 | `disconnect_block` returns `DisconnectResult`; `ApplyTxInUndo` keeps the legacy zero-height path (with a debug assertion); undo-read failures are `Failed`, coins-read failures are `Fatal` | Core's results; CH2 §3.5.2; CH2 §2.4.4 |
| D16 | `TestBlockValidity` and its command are CH3a's (§2.14), including CH1-A10's `check_header_at`. Its digest attempt opens at `ConnectBlock` (step 4); its ChainLock pre-check reads `ConnectCx::chainlocks`, unrecorded, as Core's direct `chainlocks` call is. The readiness guard applies; a refusal is an RPC error, never an `AbortNode` (D31) | it is `ConnectBlock(fJustCheck)`; MN0 (M3) needs it for `generate*`; CH1-node moved CH1-A10 to its owner; O8 excludes the pre-checks from attempts |
| D17 | `ConnectCx::origin` and a `DigestSink` tap emit O8 records; golden fields come from a pure `coins::stats` module that IX's coinstatsindex later reuses | closes O9a D7; L2 needs per-block MuHash and stats from rdashd itself (O8 §5.3: "written … by rdashd") |
| D18 | `ChainHelper` is exactly O8 §7.1's query set. CH3a ships `NodeHelper` (M3: no IS conflicts, `mn_synced = false`, `gov_valid = false`), `IbdHelper` and `RecordedHelper`; CH7b extends to `cl_best` and evo. Declared until SP1 (MN sync) and GV1b (governance) land at M5: rdashd judges as a Core node whose MN sync is unfinished. At a live mainnet or testnet tip, where a synced Core could answer differently, G2 refuses instead (D31). `gov_valid = false` is Core with `-disablegovernance` plus an unfinished sync, not a default dashd: verdicts agree on the `NoCheck` path, but decision streams and the MNPAYMENTS/GOBJECT log lines differ (Opus n1) | the decisions Core makes from node-local state are inputs, not consensus; recording them is how replay is exact. The interim regime is a state Core itself is in during IBD and after every start until sync completes |
| D19 | `VContext` lands with CH3a as a marker and is threaded into connector calls. Every connector call is an attempt bracketed by `begin_attempt`/`finish_attempt`, with effects applied in order on every outcome before V acts on the result, and the commit only after they are applied (D33) | EV addendum §2.6 (openers on V, release on every outcome); A1 §2.10.1. Landing the marker now means CH7a/CH7b reshape nothing |
| D20 | The IS-conflict loop (`v:2801-2819`) is ported in CH3a, emitting `Effect::RemoveConflictingIsLock`; `HasLockInputs` is ported as a pure function. The helper keeps an attempt-local removed view (the lock and its chained descendants; D35) that `is_conflict` excludes until the effect is applied (§3.3) | it is `ConnectBlock`'s control flow and decision-query order (O8 strict mode needs `ibd`, `cl_has`, `is_conflict` in Core's sequence); only its data source is the IS owner's |
| D21 | CS4's `Deployments` is wired on V over `HeaderChain` with `NoEhfSignals` until CH7a; the unknown-bits warning ports `WarningBitsConditionChecker` | CH1-node D30; CS4's requirement that the ancestry gives real MTPs |
| D22 | Performance levers L1–L5 are separate slices, each with an xcheck or byte-equality test; PF0 rows of §6.3; the dashd ratio ≤ 1.0 is reported and treated as a bug when breached. The 2× is not claimed for CH3a | Amdahl (§6.2); the CH2 lesson: gate observable counters, do not claim |
| D23 | L3 runs in-process against V, with dashd regtest as the oracle over RPC on disposable datadirs, in Core's not-synced regime (D49); peer-penalty parity comes from the result class plus `p2p_invalid_block` | rdashd's RPC and P2P are not CH3a's dependencies; proposal mode on live oracles leaks rows at cycle bases (A1 §4.7) |
| D24 | O2b drives `NodeSut` (§7.8): production V and connector over in-memory stores; per-step O8 records, tips and UTXO digest | O2b compares verdicts, digests and effects (PLAN), all of which already exist as O8 records; a second report format would need a second comparator |
| D25 | CH3b implements Core's kernel `DisconnectedBlockTransactions` with v24's logical budget, including libstdc++'s bucket policy, against Core-generated vectors; MP1b keeps the mempool half. Residual on non-libstdc++ platforms is an allowed difference | kernel code with no mempool dependency; CONS-25 asks for v24's accounting as a logical budget; Core's own figure is platform-dependent, so matching the reference platform is the most Core can define. Pool exactness never affects consensus (block validity, the UTXO set, the MN list). It decides only which disconnected transactions return to the mempool and which leave with reason REORG: a mempool-parity property (Opus n7, Sol's scope note) |
| D26 | CONS-11 is already implemented by CH1-node (`v:3561` dirty); CH3b proves persistence across restart and keeps `MarkConflictingBlock`'s marks non-dirty as Core does | O17's row text; SB4 D15; emulating less or more persistence than v24 would deliberately differ |
| D27 | The reorg-across-flush crash test is CH2 §6.4 C3 rerun with the real connector and real blocks, plus a node-level `-dbcrashratio` reorg loop; the reference is an uncrashed run | CH2's protocol already has the model; CH3b's job is to put real connects inside it |
| D28 | F01 is tested at two levels: ChainSut (CH3a, C11's connect half) and the regtest process with a V stage hook that makes the durable write itself before it parks, and `kill -9` (CH3b; D39) | PLAN puts it in CH3b; CH2 §7.4 moved the ChainSut half to CH3a |
| D29 | Before MP1 lands, CH3b's pool tests run on a recording `MempoolSink`; the production node uses `NullMempool`, which skips the pool as Core does without a mempool (`v:3308`) | Core's own behaviour with no mempool; O15 owns the live comparison |
| D30 | Error and log texts that FTs or recovery assert (`ConnectBlock`'s `LogError` lines, "Failed to write undo data", "Disk space is too low!", "Corrupt block found…", the undo-read texts with `what()` per class, D45) are byte-identical, and each `AbortNode` is reported exactly as often as Core reports it, by the party §2.1.1 names; `-debug=bench` lines keep Core's format | FTs use `assert_debug_log`; PF1 compares V stall lines against dashd's; the abort latch makes a double report or a silent abort easy to get wrong |
| D31 | **Readiness guard.** **G1**: `!EvoHooks::COMPLETE` and an evo rule live at the block (DIP0003, V20, MN_RR, V24 or EVO_SHARES active, or a special-version CbTx coinbase: `nVersion ≥ 3` and raw `nType == TRANSACTION_COINBASE`; below version 3 it is D54's), on every network; CH7a lifts it. **G2**: `NodeHelper::interim_refusal` under `GuardScope::Live` (mainnet, testnet), `!is_ibd`, a superblock or old-budget height, `best_cl_height < h`; SP1 and GV1b lift it. A refusal is a `SystemError`, never `Invalid`: an `AbortNode` on the tip path, an RPC error from `TestBlockValidity`, an InitError at startup when the tip or the recovery transition satisfies G1. It is checked on tip connects, ABC, invalidate, recovery activation, proposals and restart. Tests drive a block across each boundary and restart there (§2.17) | Sol B1 = Opus M1, ruling 1. A milestone plan does not stop an operator. `AbortNode` is Core's own fail-closed path, and no peer is scored because only `IsInvalid()` punishes (`net_processing.cpp:2323-2326`). ND1's coarser outer layer (refuse mainnet and testnet outright) is not adopted: it would block M3's pre-DIP3 IBD, and the startup check refuses exactly the datadirs this build cannot judge. G2 is `Live`-scoped because regtest and devnet keep D18's declared regime, as the ruling's "historical regime" allows. A G2 stop is sticky (R10); `-maxtipage` is the operator's escape hatch, since a tip older than it puts a restart back into Core's own IBD regime (`v:1970-1992`). A live mainnet or testnet tip is unsupported before M5 anyway. A refusal that does not stop the node is a follow-up for SP1 and GV1b (R10) |
| D32 | `NoEvo` ports `CheckSpecialTxInner`'s pre-DIP3 gate: `bad-tx-type-dip3-inactive`, `TX_BAD_SPECIAL` → `BLOCK_CONSENSUS`, at step 15 for every transaction except a `vtx[0]` with raw `nType == TRANSACTION_COINBASE` (`evo/specialtxman.cpp:224-229`, `:865-875`). G1 deliberately excludes DIP0008, V19, DIP0024 and BRR | own finding; neither r1 review raised it. rev1's "`NoEvo` is exact below DIP3" was false for a special-typed transaction, and an L3 tx-type operator would have shown it. **Corrected in rev2.1:** the gate alone does not make `NoEvo` exact below DIP3, because it never sees a version-2, raw-type-5 coinbase. With D54 and G1 it is (Sol r2 M1). The excluded deployments are active from height 1 on regtest (`chainparams.cpp:822-827`) and gate nothing `NoEvo` must check below DIP3 |
| D33 | **Commit after effects.** The overlay and the evo transaction stay uncommitted until `finish_attempt` returns `Ok`; only then are they committed into the view target. Core's emission order and "effects survive invalid and `just_check` attempts" are kept. a3 (a5 for disconnect) injects a fault before the first effect, between effects and at release, and asserts the old parent coins and evo best block, no tip, and recovery parity (§3.2). The step-22 undo write now precedes effect application: declared, unobservable | Sol M1, ruling 2. Core executes its effects inside `ConnectBlock`, before `view.Flush()` (`v:3446`). Committing first left the parent cache at the new block after an effect failure |
| D34 | **MuHash.** Every per-block MuHash claim starts at genesis. Until segment seeds exist, the per-PR L2 replays genesis→625,000 plus the reverse leg. Segment seeds (a12, on O9b's snapshots) are a scanned start set or Core's serialized 768-byte `MuHash3072`, with the counters seeded from verified golden values; a segment run must equal a full replay at its first and last heights. If a7 measures more than 45 min of V time at gate load, the per-PR check becomes a suffix check (from the nightly chainstate at 599,999, every field except `muhash`) that claims no per-block MuHash parity, and that switch gets its own D-row | Sol M2 = Opus m9, ruling 3. The golden is the finalized 32-byte hash (`crypto/muhash.cpp:314-323`); the accumulator is the `(numerator, denominator)` pair (`crypto/muhash.h:120-123`), which nothing recovers from 32 bytes |
| D35 | **IS loop.** `is_conflict` returns the lock and is the only query that advances a recorded cursor. `remove_conflicting_is_lock(&lock)` emits the effect, consumes nothing, and adds Core's removal closure (the lock and its chained descendants) to the attempt-local removed view. Tests: `[A, null]`, `[A, B, null]`, a failed attempt after a removal, chained children (§3.3) | Sol M3 = Opus m2, and Opus m1; ruling 4. O8 records one answer per loop condition (`evo/chainhelper.cpp:79-85`). `RemoveConflictingLock` also removes chained locks (`instantsend.cpp:339-350`) |
| D36 | **`ViewTarget::{Active, Scratch(&mut dyn ScratchView)}`** is a parameter of `connect_block` and `disconnect_block` from a2. Commit follows the target, never `AttemptOrigin`. H1, H6 and H10 get the same target. The prefetch is cleared for `Active` only. CH5 builds `ScratchView` with CH2b's owner; `verifychain 4` (M3 exit 1) is a CH5 dependency | Opus M2, ruling 5. Core's VerifyDB runs `DisconnectBlock` and `ConnectBlock` into one never-flushed `CCoinsViewCache` with a never-committed evo transaction (`v:5047`, `v:5055`). `BlockOverlay` cannot stack (DEC-142). With the parameter in place now, CH5 reshapes nothing |
| D37 | **FT ownership.** CH3a: `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation`, `feature_assumevalid` (slice a11; acceptance that closes after RP1a merges, D55). CH3b: `feature_abortnode` (b2 runs its reorg half as a process test; the FT closes after CH5 merges, D55). CH7a: `feature_nulldummy`, `feature_bip68_sequence` | Opus M3, ruling 6. A1 Appendix D's tier-1 FTs test exactly this code, and no row owned them. The two CH7a FTs cross DIP3, where G1 refuses |
| D38 | **Slice dependencies.** a8 lands before a3; a7 needs a3, a4, a5 and a8; a10 needs a4, a5 and a6; a11 is last (§8.1) | Sol m1 + Opus m4, ruling 7. a3's flags, rules and guard need real deployments. a7's gate runs at maximum `-par` with `dep` fields and the reverse leg. `NodeSut`'s invalidate, reconsider, precious and restart need disconnect |
| D39 | **F01 at node level.** V's test hook runs `flush_state(ALWAYS)` on V itself, reads B's durable record through a read-only handle, sends `F01Ready`, and only then parks. The test asserts the durable record before `kill -9` (§4.6) | Sol m2, ruling 8. `Flush` is class M (CH1-node §2.4), so a hook that parks V and waits for a `Flush` deadlocks. A "parked but serving" V would add a CH1-node state for one test |
| D40 | **H3 retired.** `SpecialTxRules` and `MnRewardEra` are computed from `cx.deployments` at step 14. The other hook numbers are unchanged | Opus m3. `GetSpecialTxRules` (`v:6797-6803`) and `GetMnRewardEraAfter` (`v:5897-5903`) read no evo state |
| D41 | CS6 gains `CheckQueue::control_inline`, used when `!fScriptChecks` (below assumevalid): no worker hand-off | Opus m5. Core passes `nullptr` to `CCheckQueueControl` when script checks are off (`v:2649`). The only inline mode CS6 has today is a serial queue fixed at construction. Performance only |
| D42 | **Port the `fChecked` memo.** `connect_block(checked)` is true only for the object that `ProcessNewBlock`'s `CheckBlock` passed with both checks on; `ConnectBlock`'s `CheckBlock` then returns at once | Opus m6. `v:4374-4375`, `v:4424-4425`. Without it a tip connect re-hashes the merkle tree, and PF0's `connect-latency-tip` row would compare unlike work |
| D43 | **Probe first.** CS6 gains `exec_cache_probe` (a soft erase on a hit when `!cache_full_script_store`) and `check_input_scripts_unprobed`. The connector probes, then copies scripts only on a miss | Opus m7. Core probes at `v:2174`, before `AccessCoin` at `v:2184`. A probe followed by CS6's full call would soft-erase twice |
| D44 | **Assumevalid-on L2 run.** It preloads the oracle's header chain; uses `defaultAssumeValid` when that header is present, else the oracle tip; keeps or lowers `-minimumchainwork`; records both choices; and asserts that every block except genesis skips its scripts. a1's decision table flips each condition alone | Opus m8. A reindex-style replay never satisfies conditions 1, 4 and 5 of `v:2542-2571`, so it would never skip |
| D45 | **Undo read.** ST2's `read_undo_record` uses the size field as a hint on the fast path and falls back to Core's stream decode from `pos` on any inconsistency. `what()` texts per exception class are pinned from dashd's log; a class with no Core analogue is a declared, log-only difference | Opus m10. Core never reads the size field (`blockstorage.cpp:540-575`), so a damaged size field with an intact record decodes in Core. Exactness costs nothing on intact files |
| D46 | Undo reads open the rev file by path for every read, never through `OsFs`'s handle cache. Writes keep the cache | Opus m11. Core opens by path per read (`blockstorage.cpp:550`, `:649-652`), and `feature_abortnode` unlinks `rev00000.dat` under a running node. Opus's alternative, a `nlink == 0` check on the cached handle, was not taken: it still reads a stale inode after a rename or replace, and undo reads are rare enough (disconnect, VerifyDB, recovery) that one `open` costs nothing measurable |
| D47 | **H7's ordering rule** (a CH7a requirement). CH7a fires the MN-list signal before the commit and before the tip moves, or proves that every listener reads neither the coins view nor the tip (§2.13) | Opus m12. Core calls `NotifyMasternodeListChanged` inside `ConnectBlock` (`v:2918-2921`). Its listener runs `govman.CheckAndRemove()` while `CoinsTip()` is still at `pprev` (`dsnotificationinterface.cpp:90-96`) |
| D48 | **Reverse leg and fuzz.** L2 disconnects back through `disconnect_block`, checking MuHash, `txouts` and `amount` against golden(h − 1): per PR 625,000→614,000, nightly the last 10,000 blocks. Fuzz targets: `undo_decode` and `connect_disconnect_roundtrip` (a5), `disconnect_pool` (b1) | Opus m13. This is the only test of `ApplyTxInUndo` on mainnet-scale undo. It uses coinstatsindex's `ReverseBlock` (`index/coinstatsindex.cpp:403`) through a1's pure stats module |
| D49 | **L3's dashd regime.** Regtest with `-dip3params=2000:2000 -testactivationheight=v20@2000 -testactivationheight=mn_rr@2000`, no peers, never sent `mnsync`. Every case asserts `mnsync status` `IsSynced: false` first | Opus m14. `NodeHelper`'s `sb_required = false` is the not-synced regime. Regtest's budget and superblock heights (from 1,000 and 1,500; `chainparams.cpp:801-806`) lie above the default DIP3 (432), where G1 refuses |
| D50 | L-cell is taken with `try_lock().expect("L-cell is a V-only leaf")` | Opus n2. A violation panics in tests instead of hanging. In production the panic is V's and trips the latch |
| D51 | `SpentScripts` is a `bumpalo::Bump`; a bespoke chunked arena is the fallback only if dependency review refuses the crate | Opus n3. No `unsafe` of ours, and `reset` keeps the largest chunk. R7 changes accordingly |
| D52 | **One `AbortNode` rule.** The connector reports every `AbortNode` raised inside the Core functions it ports (and tip-path refusals) through an `AbortSink` that is V's abort path. V reports those raised in `ConnectTip`, `DisconnectTip` and ABC (§2.1.1) | Opus n5. It keeps Core's log position (before rdashd's post-return lines) and Core's counts per path |
| D53 | **Wording.** n1 goes into D18, n4 into D14, n6 into §7.2 (testnet's window does not reach CSV), n7 into D25 | Opus n1, n4, n6, n7 |
| D54 | **The top-level CbTx check below version 3.** `NoEvo` ports `ProcessSpecialTxsInBlock`'s CbTx branch (`evo/specialtxman.cpp:816-832`) for a `vtx[0]` with raw `nType == TRANSACTION_COINBASE` and `nVersion < 3`. With `check_cbtx_merkle_roots` the result is `bad-cbtx-payload`, `BLOCK_CONSENSUS` (`bad-cbtx-invalid` if `vtx[0]` is not a coinbase); without it, no check. G1 keeps its special-version clause (`nVersion ≥ 3`). Every type test reads the raw `u16`. The version-2, raw-type-5 coinbase is in a3's G1 boundary tests and L3's operators, with Core's outcome both with script checks on and below assumevalid (§2.17) | Sol r2 M1; ruling rev2.1, which offered widening G1 to raw type 5 at every version or porting this check. **Port chosen.** Below version 3 Core reads no payload (`primitives/transaction.h:362-364`), so `GetTxPayload` always fails (`evo/specialtx.h:18-35`) and Core's outcome needs no evo state. The port is therefore exact both ways: `bad-cbtx-payload` with script checks on, a connect below assumevalid. A widened G1 would stop the node on a block Core marks invalid, and refuse one Core connects below assumevalid |
| D55 | **Late-closing acceptance** (Opus r2 M2, manager ruling). Three deliverables need rows that depend on their own row: CH3a's five FTs need RP1a (RP1a ← CH3b ← CH3a), a12 needs O9b (O9b ← CH3a), and `feature_abortnode`'s restart half needs CH5 (CH5 ← CH3b). Each is acceptance on its own row that closes after the later row merges, worded as CH3b's FTs already are, with no PLAN edge of either kind. CH3a merges on a1–a10, and its dependents wait for that merge only. `tools/cpm.py` gains only `i:O2a` on CH3a | rev2's `i:RP1a`, `i:O9b` and `i:CH5` deltas were cyclic. An `i:` edge still constrains the merge (`PLAN.md:139`), and PLAN has no slice rows. CH3b's FTs already need RP1a, which depends on CH3b (`PLAN.md:571`, `:626`), so the pattern is PLAN's own. It needs no new row and no M3 exit change. rev2.1's first cut (`35a562707`) used a new row CH3a-FT instead; the manager ruled for this pattern. A scratch `cpm.py` with `i:O2a` on CH3a exits 0 and prints the same dates as the unmodified file for every goal, CH3a, CH3b, CH5, O9b and RP1a included. Every row a late close waits on merges before M3's exit (wk 23): O9b at wk 17.5, CH5 and RP1a at wk 21.5 |

### 9.1 Escalations

None. No D-row fixes an irreversible product choice. The undo format was already settled (ST2, CH2). Every interim regime (D18, D29) and both readiness guards (D31) end when their owners land. The one product-visible consequence, a live mainnet node that stops about monthly between CH7a and M5 (R10), follows from the ruling's fail-closed requirement.

### 9.2 Embargo

No security weakness in Dash Core or Platform was identified in the material this design reads.

---

## 10. Risks

| # | Risk | Mitigation |
|---|---|---|
| R1 | CH1-node N7b-1 and CS6 are train-13 riders; their seams may move before CH3a starts | a2 is the only slice that changes CH1-node's seam (D3's fields, `ViewTarget`, `checked`, the `DisconnectPool` methods, the `block_checked`-on-`Fatal` fix), and a4 the only one that changes CS6's (D41, D43, CS6's owner co-reviewing); each follow-merges train 13 first and adapts there |
| R2 | CH2b's production `FlushSite` (b1) may pick a different owner for the coins cache than D2's cell | D2 defers to CH2b's owner under the same leaf rule; a2 checks CH2b's head before landing |
| R3 | L2's mainnet window needs per-block goldens for 0→1,028,159 (muhash, stats), and about 1 h of V time per run on a loaded box | goldens come from O8's extraction (O8 §9); the per-PR run is genesis→625,000 plus the reverse leg until a12's segment seeds land (D34); the full run is nightly via `systemd-run` |
| R4 | The libstdc++ bucket-policy port (D25) has subtle growth rules | the Core-linked vector generator checks every call; the trim point only matters for reorgs over about 20 MB, which CONS-25 notes cannot happen on mainnet in practice |
| R5 | "Never slower than dashd" on `reindex-chainstate` is not under CH3a's control alone (coins I/O, CH4) | CH3a gates instructions per block on the connect itself (micro rows) and reports the full-node ratio; CH4's row carries the pipeline |
| R6 | The D18 interim regime hides superblock-trigger logic from M3 tests | the trigger branch is CH7a/GV's acceptance; L2 cannot exercise it because Core uses `NoCheck` during reindex too. G2 refuses the live-tip cases where it would matter (D31), so the hidden logic cannot produce an acceptance difference in production |
| R7 | Dependency review may refuse `bumpalo` (D51) | the fallback is the bespoke chunked arena with CH2's `StableBytes` pattern and Miri under both borrow models in a4's gate |
| R8 | Regtest recordings with decisions exist only for FT chains (O8 §13.1) | non-evo FT replays use `RecordedHelper`; generated sequences go through O2b, where both sides get the same scripted answers |
| R9 | CS6's verify is 11% above dashd's instructions per P2PKH input (421.6 K vs 379 K; BL-160), so the `-noav` reindex rows cannot meet "never slower" through CH3a | the rows report the split (verify vs the rest, from the V-thread and worker counters) so CH3a's share is gated on its own; BL-160 owns the verify gap |
| R10 | Between CH7a and M5 (SP1 + GV1b), G2 stops a live mainnet node at the next superblock height that is not ChainLocked, about monthly, and a testnet node within about an hour (§2.17). **A restart does not clear it** (Opus r2): the tip is fresh, so the IBD latch clears at once (`v:1970-1992`), and the same block is refused again at startup, before any ChainLock at h can be learnt. Each stop lasts until the tip is older than `-maxtipage` (24 h by default), when a restart re-enters IBD and passes G2 in Core's own not-synced regime, or until SP1 and GV1b land | G2 stays an `AbortNode` (manager ruling): it is the ruling's fail-closed requirement. A live mainnet or testnet tip is an unsupported configuration before M5 anyway (ruling 1: refuse activation and serving at a live tip where superblock and payee decisions need missing state). The refused block stays `HAVE_DATA`. The `AbortNode` text names ALLOWED-DIFFS D18 and `-maxtipage`. M3's acceptance never runs at a live tip. **Follow-up for SP1 and GV1b** (Opus r2; not now): a refusal that does not stop the node. Core's ABC already returns false on a non-invalid `ConnectTip` failure without shutting down (`v:3656-3661`), so the tip could stay at h − 1 and ABC retry when the ChainLock arrives (CH1-node's `EnforceBlock` → ABC). It needs a non-stopping outcome in CH1-node, and the node would serve a stale tip meanwhile (§11 SP1, GV1b) |
| R11 | The per-PR genesis→625,000 replay may exceed 45 min of V time at gate load | a7 measures it on its own gate. Above 45 min the per-PR check becomes D34's suffix check, recorded as a D-row, and the nightly full run keeps per-block MuHash parity until a12 lands |

---

## 11. Deltas to other documents (for the manager's docs commit)

| Doc | Change |
|---|---|
| PLAN CH3a | deps add **i:CH2b** (already in CH2 §7.4), **i:CH1-node N7b-1**, **CS6** (with the two CS6 additions below), **i:O2a-GV** (O2a-GV depends only on O2a and i:SP0, so no cycle). No dependency on RP1a, O9b or CH5 (D55). Acceptance adds: CH2 §7.4's T12/C11/T20/T21 connect halves (already agreed); "the IS-conflict loop with the `[A, null]`, `[A, B, null]`, failed-attempt and chained-child sequences (D35), and `TestBlockValidity` (D16)"; "**the readiness guard: G1 and G2 boundary, restart and startup tests, and `NoEvo`'s two pre-DIP3 checks (D31, D32, D54)**"; "commit after effects, with fault injection at each effect boundary (D33)"; "L2 per PR: genesis→625,000 with per-block MuHash plus the reverse leg (D34, D48); the assumevalid-on run asserts its skip count (D44)"; "`oracle_block_value_payee` on the governance-free branches, sync flag off and on, no governance (D7)"; "the final-height `hash_serialized_2` and `dumptxoutset` set-diff (M3 exit 1)". `verifychain 4` leaves CH3a (see CH5). Crates add `dash-evo` (payments). BG rows named as §6.3. **Closing after later rows merge**, worded as CH3b's FTs are (D55): "FTs `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation`, `feature_assumevalid`, unmodified from v24 (D37); they run once ND1, RP1a and P2b merge" and "L2 segment seeds: a segment run equals a full replay at its first and last heights (D34); runs once O9b's snapshots merge". CH3a merges on slices a1–a10; a11 and a12 close the acceptance later |
| PLAN CH3b | acceptance adds "CONS-25: `DisconnectedBlockTransactions` with v24's logical budget against Core-generated vectors (D25)" and "**FT `feature_abortnode`** (D37, D46), which runs once CH5 merges (its restart half needs CH5's startup VerifyDB); its reorg half runs earlier as a regtest process test: an unlinked `rev00000.dat`, a reorg, 'Failed to disconnect block', the abort (D55)" |
| PLAN CH7a | acceptance adds "**FTs `feature_nulldummy` and `feature_bip68_sequence`** (D37)"; "**lifts G1**: sets `EvoHooks::COMPLETE` and turns §2.17's G1 tests around, so the same blocks connect and match dashd (D31)"; "H7's MN-list signal fires under D47's ordering rule"; "reruns D33's fault injection with its own evo store"; "opens a `NoEvo`-built datadir by initialising evo state from B1 or requiring `-reindex-chainstate`, and its turned-around G1 test uses one (§2.17)". H3 is retired: special-tx rules and the MN reward era come from deployments (D40) |
| PLAN SP1, GV1b | acceptance adds "**lifts G2**: `NodeHelper` answers `sb_required` and `gov_valid` from real state, and §2.17's G2 tests are turned around against a synced dashd (D31)". The row that lands second removes `interim_refusal`. **Follow-up** (R10, manager ruling on Opus r2): evaluate a G2 refusal that does not stop the node, holding the tip at h − 1 and retrying ABC on the ChainLock (`v:3656-3661`); it needs a non-stopping outcome in CH1-node |
| PLAN CH5 | adds "**`ScratchView`** (D36), built with CH2b's owner; VerifyDB levels 3–4 run through CH3a's connector with `ViewTarget::Scratch`"; "`verifychain 4 0` at M3 exit 1" |
| PLAN M3 exit 1 | `verifychain 4 0` depends on **CH5** (D36) |
| PLAN O9b | segment snapshots carry Core's serialized `MuHash3072` state (or the start set for a scan) beside the UTXO snapshot; consumer: CH3a a12, which lands after O9b merges and closes CH3a's segment-seed acceptance (D34, D55) |
| PLAN ND1 | derives `ConnectCx::guard` (`GuardScope`) from the selected chain (D31). No outer mainnet/testnet refusal (§2.17) |
| PLAN M3 exit 5 | covers non-evo paths only, with a pointer "evo-path FTs: see CH7a": `feature_nulldummy` and `feature_bip68_sequence` leave it for CH7a's acceptance (D37, manager ruling); A1 gains an erratum for its Appendix D M3 row |
| PLAN SW-FT | the M3 tier-1 passlist rows for six FTs of §7.5 name their owning rows, CH3a and CH3b; the two CH7a FTs are M4's (D37) |
| `tools/cpm.py` | `'i:O2a'` on CH3a (O2a-GV's parent row). The late-closing acceptance adds no edge (D55) |
| PLAN MP1b | "O17 CONS-25: disconnect-pool accounting" becomes "adopts CH3b's `DisconnectedBlockTransactions`; the mempool half of `MaybeUpdateMempoolForReorg`" |
| PLAN CH7b | "`ReplayChainHelper`" becomes "extends CH3a's `ChainHelper` and `RecordedHelper` to `cl_best` and the evo queries"; `feature_llmq_is_cl_conflicts` gates its replay of the IS loop (§3.3) |
| PLAN EV3 | "payee/value validation" becomes "extends CH3a's `dash-evo::payments` (the governance-free branches of `IsBlockValueValid`/`IsBlockPayeeValid`) with `GetBlockTxOuts`, the MN payee check and the trigger branches"; MNO-12 has v24's behaviour only (DEC-82) |
| PLAN O2a-GV | consumers add CH3a (D7); the governance-free cases (no governance context, sync flag off and on) are wanted first |
| CS6 | `CheckQueue::control_inline` (D41); `exec_cache_probe` (soft erase on a hit when `!cache_full_script_store`) and `check_input_scripts_unprobed` (D43). CS6's owner co-reviews a4 |
| ST2 | `write_undo_slice` (D13); `read_undo_record(pos, prev_hash, out)` with the hint-then-stream semantics and §2.10.2's error table (D45); undo reads open the rev file by path, never through `OsFs`'s handle cache (D46). ST2's owner co-reviews a5 |
| CH1-node | seam deltas of D3 (`ConnectCx::guard`, `ViewTarget`, `checked`) and `DisconnectPool::{take, dynamic_memory_usage}` (CH3a's a2 lands them); `connect_tip` emits `block_checked` for `Ok(SystemError)` only, never for `Err(Fatal)`, and its comment at `abc.rs:544` is corrected (a `Fatal` is a storage fault, where Core aborts before `BlockChecked`); `ChainCommand::TestBlockValidity`'s handler is CH3a's. **Added in rev2:** the `checked` memo travels on `ProcessNewBlock`'s block handle to `connect_tip` (D42); the G1 startup check in the load path, after recovery's replay and before ABC (D31; CH3a D70); the F01 V test hook (D39); the connector's `AbortSink` is V's abort path, which logs once per call and trips the latch if untripped (D52) |
| CH1-node (LO-9) | the check-queue pool joins LO-9's list of tasks V may wait on; its workers carry the no-wait marker (§5.2) |
| CH2b | recovery's `ReplayBlocks` passes `AttemptOrigin::Replay` and, when a `DigestSink` is attached, emits its `D` records through it (§2.15); co-designs CH5's `ScratchView` (D36) |
| O9a notes | D7's origin gap closes with CH3a a7 |
| RP1a | `gettxoutsetinfo hash_serialized_2` and `dumptxoutset` wrap CH3a's `hash_serialized_2` and `dump_utxo` (§7.2) |
| `compat/ALLOWED-DIFFS.md` | D14 (BlockChecked after commit; the `-debug=bench` split); **D18** reworded: "mainnet/testnet live tip: refused by G2 until SP1 + GV1b; regtest/devnet: the not-synced regime, declared; `gov_valid = false` changes decision streams and MNPAYMENTS/GOBJECT log lines, not verdicts"; D25 (pool accounting under another C++ library or libstdc++ version); D33 (the undo write precedes effect application); D45 (an undo-read exception class with no Core analogue: log text only) |

---

## 12. Change log

| Rev | Change |
|---|---|
| rev0 | First design: CH3a connector, evo seams, CH3b integration, D1–D30 (8c4c173c2) |
| rev1 | Opus code-review-validator r1 (APPROVE WITH CHANGES; 4 major, 7 minor, 2 nit groups), all applied. Majors: **(1)** §2.1.1 who reports each `AbortNode` (the connector's own `AbortSink` for the undo write and `DisconnectBlock`'s evo check; V for the rest), with Core's counts and a fault-injection test; **(2)** `TestBlockValidity`'s digest attempt opens at step 4 and its CL pre-check is unrecorded (O8 :38); **(3)** the IS loop's attempt-local removed set, so a deferred removal cannot loop forever; **(4)** the governance-free payments branches go into EV3's `dash-evo::payments`, with `oracle_block_value_payee` as the oracle (PLAN deltas for EV3 and O2a-GV). Minors: the live-tip scope of the D18 difference (§2.7); `DisconnectPool::{take, dynamic_memory_usage}`; libstdc++'s shrinking `rehash`; no `block_checked` on `Fatal`; `CheckInputsError::Flags` and memo misses are `Fatal`; `have_inputs` skipped for asset unlocks, its bool ignored; the L-cell rule; `is_ibd` read only by `NodeHelper`. Nits: candidate re-adding is `InvalidateBlock`'s; precious-counter reset rule; F01's A1 citation; H1's `ConnectTip` guard; `IbdHelper` wording; inline control below assumevalid; CONS-11 case 2 compares with dashd |
| rev2 | Manager Sol r1 (CHANGES-REQUIRED: B1, M1–M3, m1–m2) and Opus r1 (CHANGES-REQUIRED: M1–M3, m1–m14, n1–n7) applied per the rev2 rulings (`/work/scratch/ch3-design-r1/rev2-rulings.md`), plus one finding of our own (D32). New: the readiness guard G1/G2 with its startup check (§2.17); commit after effects (§3.2); MuHash from genesis and segment seeds (§2.15, a12); the recorded IS cursor and removal closure (§3.3); `ViewTarget`, with VerifyDB 3–4 moved to CH5 (§2.1, §2.11); FT ownership and slice a11 (§7.5); the `fChecked` memo; probe-first and `control_inline` (CS6 deltas); Core-exact undo reads (hint then stream, open by path); the reverse leg and three fuzz targets; L3's dashd regime; `bumpalo`; one `AbortNode` rule. D2, D3, D9, D13, D14, D16, D18–D20, D23, D25, D28 and D30 amended; D31–D53 added; R1, R3, R6 and R7 updated; R10 and R11 added; §8 re-sliced (a1–a12, b1–b4, about 22–27 + 8–10 agent-days). §12.1 closes every finding |
| rev2.1 | Sol r2 (CHANGES-REQUIRED: one new Major) and Opus r2 (CHANGES-REQUIRED: two new Majors), applied per the manager's ruling. Sol accepted every other r1 closure and all eight declared choices. Opus accepted seven, rejected the PLAN form of the eighth, and asked for an R10 correction. **Sol r2 M1 = Opus r2 M1:** G1 missed a pre-DIP3 coinbase with `nVersion` 1 or 2 and raw `nType = 5`, which Core checks at `evo/specialtxman.cpp:816` (`bad-cbtx-payload`) and which D32's gate never sees. `NoEvo` now ports that top-level check below version 3 (D54, the port chosen over widening G1). G1 keeps its version ≥ 3 clause, every type test reads the raw `u16`, the case joins a3's guard tests and L3's operators with Core's outcome both ways, and D32's exactness claim is corrected. Opus's two tests are in: version 1 and version 2, in §2.17 and in L3. **Opus r2 M2:** §11's `i:RP1a` on CH3a made PLAN cyclic, and so did `i:O9b` (a12) and CH3b's `i:CH5`. Per the manager's ruling each is acceptance that closes after the later row merges, worded as CH3b's FTs are, with no PLAN edge (D55); a scratch `cpm.py` completes with unchanged dates. The first cut (`35a562707`) had a new row CH3a-FT instead. Also R10 per the ruling (G2 stays an `AbortNode`; a restart refuses the same block again; each stop lasts until `-maxtipage` or SP1/GV1b; a live tip is unsupported before M5; the non-aborting refusal is a follow-up for SP1/GV1b), `ScratchView::split`, CH7a's `NoEvo`-datadir requirement, the startup check under reindex, the undo open-failure and `fread` texts, and the memo test's precondition. D31, D32 and D37 amended; D54 and D55 added (§12.2) |

### 12.1 r1 closure

Every finding of both r1 reviews, with where rev2 closes it. "=" marks the same finding raised by both reviews.

| Finding | Summary | Closed in | D-row |
|---|---|---|---|
| Sol B1 = Opus M1 | interim helpers and `NoEvo` accept blocks a synced Core rejects; nothing fails closed | §0 item 8, §2.1.1, §2.2 steps 5a and 19, §2.7, §2.14, §2.17, §3.1, §3.3, §5.3, §7.3, §7.8, §11 (CH7a, SP1/GV1b, ND1, CH1-node) | D31 (D16, D18 amended) |
| own | `NoEvo` was inexact below DIP3: a special-typed transaction gives `bad-tx-type-dip3-inactive` in Core | §2.17, §3.1 H4, §7.1, §7.3 | D32 (completed by D54 in rev2.1) |
| Sol r2 M1 (rev2.1) | G1 and D32 miss a pre-DIP3 coinbase with `nVersion = 2`, raw `nType = 5` (`bad-cbtx-payload` in Core) | §0 item 8, §2.2 step 15, §2.17, §3.1 H4, §7.1, §7.3 | D54 (D31, D32 amended) |
| Sol M1 | effects applied after the parent cache was committed | §2.2 step 24, §2.11, §2.12 row 7, §3.1 H10, §3.2, §5.3 | D33 (D14, D19 amended) |
| Sol M2 = Opus m9 | MuHash cannot be seeded from the golden hash | §2.15, §7.2, §8.1 a7 and a12, §11 (O9b), R3, R11 | D34 |
| Sol M3 = Opus m2 | `RecordedHelper` advances the cursor twice | §3.3 | D35 |
| Opus m1 | the removed set misses chained child locks | §3.2, §3.3 | D35 (D20 amended) |
| Opus M2 | the connector cannot run VerifyDB levels 3–4 | §1.3, §2.1, §2.11, §2.12, §3.1, §7.2, §11 (CH5, CH2b, M3 exit 1) | D36 (D3 amended) |
| Opus M3 | the M3 tier-1 FTs are in no row's acceptance | §1.1, §7.5, §8.1 a11, §8.2 b2, §11 (CH3a, CH3b, CH7a, CH5, SW-FT) | D37 |
| Sol m1 = Opus m4 | slice dependencies incomplete | §8.1 | D38 |
| Sol m2 | the F01 hook waits on the V it has paused | §4.6, §7.7, §8.2 b4, §11 (CH1-node) | D39 (D28 amended) |
| Sol scope note | pool exactness is a mempool-parity property | D25 | D25 amended |
| Opus m3 | H3 and `MnRewardEra` are deployment queries | §0 item 1, §2.2 step 14, §3.1, §11 (CH7a) | D40 |
| Opus m5 | inline control needs a CS6 API | §2.2 step 13, §2.9, §11 (CS6) | D41 |
| Opus m6 | `fChecked` memo not ported | §2.1, §2.2 step 3, §2.14, §6.1, §7.1, §11 (CH1-node) | D42 |
| Opus m7 | copying scripts before the exec-cache probe | §0 item 6, §2.9, §6.1, §11 (CS6) | D43 (D9 amended) |
| Opus m8 | the assumevalid-on L2 run never skips | §2.8, §7.2, §8.1 a1 and a7 | D44 |
| Opus m10 | the undo read trusts ST2's size header | §2.10.2, §7.1, §11 (ST2, ALLOWED-DIFFS) | D45 (D13, D30 amended) |
| Opus m11 | `feature_abortnode` cannot pass through the handle cache | §2.10.2, §7.5, §11 (ST2) | D46 |
| Opus m12 | H7's MN-list signal needs an ordering rule | §2.2 step 26, §2.13, §3.1, §11 (CH7a) | D47 |
| Opus m13 | no L2-scale disconnect differential, no fuzz targets | §2.15, §7.1, §7.2, §8.1 a5 and a7, §8.2 b1 | D48 |
| Opus m14 | L3's dashd regime unstated | §7.3 | D49 (D23 amended) |
| Opus n1 | `gov_valid = false` is `-disablegovernance`-like | §3.3, §11 (ALLOWED-DIFFS) | D18 amended, D53 |
| Opus n2 | `try_lock` for the V-only leaf | §5.2 | D50 (D2 amended) |
| Opus n3 | `bumpalo` instead of a bespoke arena | §2.9, §6.2, R7 | D51 |
| Opus n4 | `-debug=bench` split | §2.13 | D14 amended, D53 |
| Opus n5 | one `AbortNode` rule | §2.1, §2.1.1 | D52 (D3 amended) |
| Opus n6 | testnet's window does not reach CSV | §7.2 | D53 |
| Opus n7 | pool exactness never affects consensus | D25 | D25 amended, D53 |

### 12.2 r2 closure

| Finding | Summary | Closed in | D-row |
|---|---|---|---|
| Sol r2 M1 = Opus r2 M1 | G1 and D32 miss a pre-DIP3 coinbase with `nVersion` 1 or 2 and raw `nType = 5` (`bad-cbtx-payload` in Core). Opus's tests: both versions in §2.17, both regimes, and in L3's operators | §0 item 8, §2.2 step 15, §2.17, §3.1 H4, §7.1, §7.3 | D54 (D31, D32 amended) |
| Opus r2 M2 | the §11 `i:RP1a` delta on CH3a makes PLAN cyclic | §0, §1.1, §7.5, §8.1, §8.2 b2, §11 (CH3a, CH3b, CH5, O9b, SW-FT, `cpm.py`) | D55 (D37 amended) |
| own (r2) | the same cycle shape at `i:O9b` (a12) and CH3b's `i:CH5` | as above | D55 |
| Opus r2, choice 3 | R10 understates the outage: the stop is sticky | R10 and §2.17 per the ruling: G2 stays an `AbortNode`, a restart refuses again, `-maxtipage` or SP1/GV1b ends it, a live tip is unsupported before M5; the non-aborting refusal is a follow-up (§11 SP1, GV1b) | D31 amended |
| Opus r2 observation | `ScratchView`'s two `&mut self` methods cannot both be live | §2.1, §2.11 | D36 |
| Opus r2 observation | CH7a must open a `NoEvo`-built datadir | §2.17 Removal, §11 CH7a | D31 |
| Opus r2 observation | the open failure logs "Unable to open file" first; `fread failed` is a class | §2.10.2 | D45 |
| Opus r2 observation | the startup check under `-reindex(-chainstate)` | §2.17 | D31 |
| Opus r2 observation | the memo test passes `checked = true` only for `CheckBlock`-valid blocks | §7.1 | D42 |
