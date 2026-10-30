# CH7: evo and LLMQ integration in the chainstate (CH7a + CH7b)

| | |
|---|---|
| **Status** | **APPROVED (DEC-531)** at rev3.2 6fb66c8ab; §11.2's design-doc errata are applied. rev3.2 (the Opus-h r3.1 findings fixed, DEC-529; rev3.1 fixed the Sol r3 and Opus-h r3 findings, DEC-527; rev3 fixed Opus-h r2 and Sol r2, DEC-522, plus CH3's three CH3b-planning inputs; dispositions in §10.2) (PLAN: "D (CH7a + b)"; D O-h). Task CH7-D (DEC-515). rev0 d0f5c1abe, rev1 8ce6690a0, rev1.1 07d691958 (DEC-517, DEC-518), rev2 6a3fab586 (DEC-520), rev3 2834a632b (DEC-522), rev3.1 642943a69 (DEC-527) |
| **Date** | 2026-10-27 |
| **Rows** | PLAN CH7a (line 577) and CH7b (line 578); consumers SB19 (764), AM1 (699), GV1b (697), RP2a (629), MP1a (633), MN1 (637), QM1a (700), LK1a (705), O15 (669), IX1 (638), CH6b (576) |
| **Base** | rust-dashcore `node/integration` 497d3ca11 |
| **Binding inputs** | EV addendum FINAL rev6 (DEC-122, as amended by DEC-111/113/117/119), in particular §2.3–§2.6 and §4.3: this design implements them and does not re-design them. SB0 rev6.1 (DEC-119). CH3 rev2.1 §2.17 and §3 with CH3a-notes D19, D33, D36, D40, D47, D54, D56, D57, D61, D63, D70, D71, D74, D75, D83, D135, D158, D167. CH2 §2.4.4, §2.10, D17. A1 §2.9, §2.10, §2.10.1, §2.10.2. DEC-82 (v24 only) |
| **Reference** | Dash Core v24.0 at the program's pin **375c64d2317** (the rc.4 tag 0e7382df80d plus test-oracle hooks; §0.1) |

---

## 0. Conventions and summary

### 0.1 Citation keys

All Core lines are at **375c64d2317**. That commit carries test-oracle hooks (`statedigest::*`, `ORACLE_*`, `Tap*`, `Fx(…)`) in `validation.cpp`, `evo/specialtxman.cpp`, `evo/deterministicmns.cpp`, `evo/chainhelper.cpp`, `llmq/blockprocessor.cpp`, `llmq/snapshot.cpp`, `llmq/utils.cpp` and others (the inventory is `git diff 0e7382df80d 375c64d2317`). Hook lines are never cited as Core behaviour; where a hook wraps a Core expression, the expression is what is cited. `evo/evodb.*`, `evo/cbtx.cpp`, `evo/creditpool.*`, `evo/mnhftx.*`, `evo/assetlocktx.cpp`, `node/chainstate.cpp`, `llmq/commitment.cpp` are identical to the tag.

| Key | File |
|---|---|
| `v:N` | `src/validation.cpp` |
| `stm:N` | `src/evo/specialtxman.cpp` |
| `dmn:N` / `dmnh:N` | `src/evo/deterministicmns.cpp` / `.h` |
| `bp:N` | `src/llmq/blockprocessor.cpp` |
| `snap:N` | `src/llmq/snapshot.cpp` |
| `lu:N` | `src/llmq/utils.cpp` |
| `cbtx:N`, `sml:N` | `src/evo/cbtx.cpp`, `src/evo/simplifiedmns.cpp` |
| `cp:N`, `mnhf:N`, `al:N` | `src/evo/creditpool.cpp`, `src/evo/mnhftx.cpp`, `src/evo/assetlocktx.cpp` |
| `ch:N`, `pay:N` | `src/evo/chainhelper.cpp`, `src/masternode/payments.cpp` |
| `edb:N` / `edbh:N` | `src/evo/evodb.cpp` / `.h` |
| `cs:N`, `cl:N`, `clh:N` | `src/node/chainstate.cpp`, `src/chainlock/chainlock.cpp`, `src/chainlock/handler.cpp` |
| `rs:path:N` | rust-dashcore at 497d3ca11 |
| `ADD §x` | `designs/EV-addendum-SB0.md` |

### 0.2 Summary

1. **Two modules in `dashd-chainstate`, one implementation of `EvoHooks`.** CH7a adds `dashd_chainstate::evo` (§2.1): `NodeEvo`, the production `EvoHooks` that replaces `NoEvo`; a RocksDB `EvoStore` over the seven column families that already exist (`rs:dashd-chainstate/src/store/schema.rs:7`); `EvoDelta` as the B1 `StateDelta`; the evo layer and `EvoReadView`; and the effect applier. CH7b adds `dashd_chainstate::llmq` (§2.2): the process-wide `MembersFacade` with the attempt handle, the gate and the capability types of ADD §2.6, the runtime snapshot rows, the materializer and scan runtime, the `QcHashCache` and mineable pool hosts, the runtime V command, and the read-only mnxcheck surface of ADD §4.3. The pure algorithms stay where they are (`dash-evo`, `dash-dml`); CH7 only composes them in Core's order.
2. **Core's order, step for step.** `ConnectBlock`'s evo steps already have hook points (CH3 §3.1, H1–H11). §3 fills H4 with `ProcessSpecialTxsInBlock` in Core's order (`stm:881-1085`): CbTx parse, credit-pool indexes, the per-transaction loop, the credit-pool diff, the quorum block processor, the DML build and write, the two CbTx roots and the CbTx ChainLock, MNHF, then the V19 scheme switch; and H9 with the reverse undo (`stm:1087-1120`). Core's gates are kept exactly: `fCheckCbTxMerkleRoots` (the `-assumevalid` flag, `v:2716`), `fJustCheck` (`stm:997`, `bp:458-460`, `mnhf:210-262`), DIP3/V20 activation, and the exception mapping (D5).
3. **State classes follow Core's transaction boundary.** Everything Core writes into the evodb transaction is `EvoDelta`, merged on success and written in B1 with `meta.evo_tip` (D6). Everything Core writes outside it, or keeps in process memory, is an effect or a cache (§3.8, §4.4). This makes the mined-commitment rows transactional, and `EFFECT_CFS` and DEC-202 keep only `llmq_snapshot` (**Q1**, DEC-517).
4. **No global BLS scheme.** Each attempt gets `bls_ctx_for_block` (the scheme Core's `ScopedBLSLegacyScheme` enters, `v:2542-2543`). The scheme Core would leave in its global after a connect is published as `ChainView.bls` (A1 §2.10.2) only when the connect commits (`v:2970`); a check-only or failed attempt changes nothing (D9).
5. **A live `MnList` for the node** (§5): V publishes `Arc<MnList>` at Core's DMN tip in `ChainView` (D16), calls `ValidationInterface::notify_masternode_list_changed` synchronously for Core-equivalent listeners under Core's condition (`v:2972-2976`, D15), and serves pinned historical reads through `EvoReadView` and dispatched commands. SB19, AM1 and GV1b read through these, never through the store.
6. **CH7b follows ADD §2.6 literally**: one exclusive gate per attempt, a closed `RawRows` allow-list, every other external route dispatched whole to V under `OnV`, Core's `snapshotCacheCs` sections, cases 1–7, the compile-fail suite, `peek`. The only new design work is the runtime V command that dispatches a scoped closure (§4.6), the per-attempt context binding of the facade (§4.2.3), and the cache hosts (§4.4).
7. **Staged readiness.** CH7a lifts G1 in stages: first for evo without quorum answers, then (with CH7b) for LLMQ. Every member-free check of H4 is live from CH7a-2, and a well-formed block whose processing needs quorum members, a quorum object or a ChainLock verify is refused by a readiness capability until the CH7b slice that provides it: members and quorums by G1q at H4's entry until CH7b-2, the ChainLock verify at Core's own position until CH7b-3 (D2, D3, **Q3**). At no slice boundary does the node accept a block Core rejects (§8). This lets CH7a merge before CH7b, as PLAN's chain requires (`CH7b` deps `CH7a`).
8. **Slices** (§8): CH7a-1…5 and CH7b-1…5. **SB19's implementation is unblocked by CH7a-3 (the live list) plus CH7b-4** (`peek`, `ShadowRows`, the read-only `LlmqSource` and `llmq_S` reader, on CH7b-1's runtime `OnV` command). Its acceptance on testnet also needs CH7b-2, the slice that removes G1q, because almost every testnet block past DIP3 carries a commitment that needs members, and CH7b-3, which lifts the last readiness refusal (step 10's ChainLock verify), because testnet blocks past the first ChainLocks carry a CbTx ChainLock (§8.3, **Q10**).
9. **Open questions** (§10): DEC-517 ruled Q1–Q13 as recommended; DEC-518 ruled Q14 (D5 keeps CH3's AbortNode class). §10.1 and §10.2 record the Sol r1 (DEC-520) and r2 (DEC-522) dispositions.

---

## 1. Scope and non-goals

### 1.1 What the rows ask for

| PLAN text (CH7a, line 577) | Where |
|---|---|
| special-tx orchestration | §3.2 (H4), §3.3 (H9) |
| CbTx MN root and credit pool | §3.2 steps 1–3, 5, 8; §3.5.2 |
| payments/value via `ChainHelper` (NoCheck) | §3.6 |
| MNHF | §3.2 step 11; §3.5.3 (with the EHF deployments) |
| BLS context (`for_block`) and undo | §3.4 (D9) |
| `EvoDelta` in the B1 batch | §6.1 |
| the effect channel (A1 §2.10.1) and the views (§2.10.2) | §3.8, §5 |
| O17 CONS-09 | §3.4 (v24 only, DEC-82; **Q4**) |
| the `write evodb cache to disk` log pair | §6.1 (D20) |

| PLAN text (CH7b, line 578) | Where |
|---|---|
| commitments in blocks (eager BLS decode, null rules), quorum root | §4.1 |
| rotation snapshots as effects | §4.2, §4.3 |
| CL enforcement + `ChainHelper` CL/IS plumbing | §4.5 |
| `ReplayChainHelper` (failed attempts too) | §4.5.3 |
| `LlmqSource` and facade map `MemberError` per ADD §2.3 | §4.2.4 |
| attempt handle and overlay, gate, `release_attempt(EffectsApplied)` | §4.2 |
| `VContext`/`OnV`/`OnValidation`, whole-route V dispatch except `RawRows`, `begin_attempt` only with `VContext` | §4.6 |
| the precompute loop | §4.3 |
| read-only `peek`, `LlmqSource` and `llmq_S` row reader for mnxcheck (SB19) | §4.7 |

PRD: FR-CON-18, -19, -21, -22, -27, -28, -29, -32; FR-CHS-03, -04, -05; FR-EVO-01…09; FR-LLMQ-01, -02, -04, -05. §9 maps each acceptance clause to a test.

### 1.2 Non-goals

- **No v23.1.8 profile** (DEC-82). The v23.1.8 BLS-flag leak (A1 §2.25 row 26) and `Effect::BlsSchemeLeak` are out; PLAN's CH7a text still names them (**Q4**). CH7c is dropped.
- **No assumeutxo** (A1 NG-6). The pin's dual-chainstate support (`EvoDbIdentity::SNAPSHOT`, `edbh:36-39`; the `BlockDataUnavailableError` branch of `GetListForBlockInternal`, `dmn:868-884`; `EraseMinedCommitmentIfUnreferenced`'s other-chainstate branch, `bp:63-79`; `RecordBackgroundMNListHash`, `v:3216-3225`) is not modelled. With one chainstate each takes Core's single-chainstate branch.
- **Not the node roles.** The ChainLock handler and its schedule (LK1a), InstantSend (LK1b), DKG (DK1), the quorum manager beyond what validation needs (QM1a), signing (SG1), the active masternode (AM1), governance state (GV1b), MN sync and the mncache meta store (SP1), mining (MN1) and RPCs (RP2a) are consumers. CH7 gives them typed seams (§4.5, §5) and the effect appliers they own plug into (§3.8).
- **No evodb repair or migration of Core-written stores** (`RecalculateAndRepairDiffs`, `MigrateLegacyDiffs`, `ForceSignalDBUpdate`'s upgrade role; `cs:281`, `cs:286`): this node never writes Core's legacy formats (**Q6**). CH6b imports Core datadirs.
- **No new P2P behaviour.** CH7 serves no new message. `mnlistdiff`/`qrinfo` serving stays P6b's, through §4.6's dispatch. mnxcheck's peer requests stay SB19's (DEC-515).

---

## 2. Layout and public API

### 2.1 `dashd_chainstate::evo` (CH7a)

```text
dashd-chainstate/src/evo/
  mod.rs          NodeEvo (impl EvoHooks), NodeEvoAttempt, the evo read-only parts of ConnectCx
  store.rs        RocksEvoStore: EvoStore + DmlReader + MinedCommitmentReader over the evo CFs (read side)
  layer.rs        EvoLayer: the active and frozen merged indexes (EvoIndex) over the DB (A1 §2.10.2; D22), EvoWindow impl
  codec.rs        EvoDelta ⇄ CF rows (keys of A1 §2.9; values via dash_dml::codec)
  view.rs         EvoReadView { at, active, frozen, snapshot } (A1 §2.10.2; D22) and the LlmqSource/DmlReader adapters over it
  special.rs      process_special_txs (H4) and undo_special_txs (H9): Core's order (§3.2, §3.3)
  effects.rs      EffectApplier (exhaustive), presumed/independent classification, EffectLedger use (§3.8)
  caches.rs       ListCache (concurrent LRU, Core's MAX_CACHED_LISTS 64), SmlRootCache host, credit-pool/MNHF caches (§3.7)
  startup.rs      evo open/verify at startup, the below-DIP3 consistency check (D21, Q7)
```

The new dependency edge is `dashd-chainstate → dash-dml` (today the crate uses `dash_evo` only). It is legal under SB1's layers: `dashd-chainstate`'s layer is allowed to use `dash-dml`'s (`rs:contrib/node/layers.toml:42`, `:74`, `:92`), and SB1's L-gates check it.

```rust
/// The production EvoHooks (CH3 §3.1). One per chainstate, owned by the connector beside the facade (CH3a D56).
pub struct NodeEvo {
    store: Arc<RocksEvoStore>,             // read side over the CFs; V is the only writer (A1 §2.1 rule 4)
    layer: EvoLayer,                       // active + frozen merged indexes (D22)
    pending: Option<PendingTx>,            // the open evo transaction of the Active target (D1): begin_tx opens, commit_tx moves, rollback_tx clears
    caches: EvoCaches,                     // §3.7: ListCache, SmlRootCache, credit-pool and MNHF LRUs
    appliers: EffectAppliers,              // §3.8: one applier per EffectKind, all required at construction
    publish: Arc<dyn TipPublisher>,        // ChainView.mn_list / .bls and ChainEvent::MnListChanged (§5)
    params: Arc<ChainParams>,
}
impl EvoHooks for NodeEvo {
    const COMPLETE: bool = true;                          // G1 lifted in two stages (D3)
    type Facade = MembersFacade;                          // CH7b (§4.2); CH7a-1..2 use the stub of D2
    type Attempt<'v> = NodeEvoAttempt<'v>;
    type ListUpdates = MnListUpdates;                     // Core's MNListUpdates (old, new, diff)
    /* H1–H11 as CH3 §3.1; bodies in §3 */
}
pub struct NodeEvoAttempt<'v> {
    handle: AttemptHandle<'v>,             // CH7b's: the exclusive gate, the overlay, the facade (ADD §2.6)
    log: Vec<Effect>,                      // the attempt's one ordered effect log (EffectSink)
    bls: BlsCtx,                           // H1's answer; fixed for the attempt (D9)
    origin: AttemptOrigin,                 // Tip, Proposal, VerifyDb, Replay (rs:chain/seams.rs:457)
}
pub struct MnListUpdates { pub old: Arc<MnList>, pub new: Arc<MnList>, pub diff: Arc<MnListDiff> }
/// One open evo transaction (Core's `CurTransaction`): owned by the target, not the attempt, because
/// `finish_attempt` consumes the attempt before `commit_tx` runs (rs:connect/connector.rs:320-342).
pub struct PendingTx {
    delta: EvoDelta,                       // H4 (connect) or H9 (disconnect, tombstones only) fills it; H6 stages its evo_tip (D1)
    post_scheme: Option<BlsCtx>,           // set by step 12 / H9 step 1; published by commit_tx only after H1's commit (D9)
}
```

`EvoTxn` (`rs:dashd-chainstate/src/chain/seams.rs:655`, empty today, "CH7a gives it its methods") becomes the scratch target's evo half:

```rust
pub trait EvoTxn {
    /// The scratch index a VerifyDB attempt reads through, over the active layer (VerifyDB's never-committed
    /// transaction, D36): the same merged structure as the layer (D22).
    fn index(&self) -> &EvoIndex;
    /// The open entry: `begin_tx` opens it; `commit_tx` merges it into the scratch index, in order (level 3's
    /// disconnects, then level 4's connects); `rollback_tx` clears it (D1).
    fn pending(&mut self) -> &mut Option<PendingTx>;
}
```

### 2.2 `dashd_chainstate::llmq` (CH7b)

```text
dashd-chainstate/src/llmq/
  facade.rs       MembersFacade, AttemptHandle, ExternalToken, EffectsApplied (ADD §2.6), peek (ADD §4.3)
  capability.rs   VContext::on_v, OnV, the sealed OnValidation (ADD §2.6); lives in chain/actor's loop module
  rows.rs         RuntimeRows (llmq_snapshot CF + the snapshot cache, Core's sections), AttemptRows, RawRows, ShadowRows
  members.rs      the facade's QuorumMemberSource and QuorumPrecompute impls (LQ1 §3.7.4 + ADD §2.6)
  source.rs       NodeLlmqSource: LlmqSource<MnList> over EvoReadView/the attempt; the read-only variant for mnxcheck
  quorum.rs       the materializer (GetQuorum), the quorum-object cache, ScanQuorums and its cache (DEC-60)
  blocks.rs       the block half of LQ2 (§4.1): QcCtx binding, BlsCheckQueue, the QcHashCache host
  mineable.rs     MineablePool host (dash_dml::blockprocessor::pool) and its effect appliers (Q5)
  helper.rs       ChainHelper extensions: cl_best, the CbTx ChainLock verify, ReplayChainHelper (§4.5)
```

The facade lives in `dashd-chainstate`, not in a new `dashd-quorums` crate: PLAN puts CH7b there, `EvoHooks::Facade` already names it, and QM1a (`dashd-quorums`) extends it through `OnValidation`-taking APIs (**Q9**).

### 2.3 Neighbours and exchanged types

| Neighbour | Direction | Types |
|---|---|---|
| CH3a/CH3b connector (`rs:connect/connector.rs`, `serial.rs`, `disconnect.rs`) | calls CH7 | `EvoHooks` H1–H11, `AttemptOrigin`, `ViewTarget`/`EvoTarget`, `BlockAt`, `SpecialTxRules`, `ChainHelper` |
| CH2 flush (`rs:flush/*`) | calls CH7 | `StateDelta` (= `EvoDelta`), `EvoWindow` (= `EvoLayer`), `EffectLedger::record_presumed`, `meta.evo_tip` |
| CH1-node V loop (`rs:chain/actor.rs`) | hosts CH7b | `VContext` (exists, `:173`), `ChainCommand` (gains `OnV` and `VerifyDb`, §4.6), `ChainView` (gains `mn_list`, `bls`) |
| EV1 (`dash_dml::specialtx`) | called | the dispatcher `check_special_tx_inner`, `BlockPayloads`, `SpecialTxCtx` (EV1-2…5; **Q2**) |
| EV2a/EV2b (`dash_dml`) | called | `resolve_list`, `build_new_list_from_block`, `process_block`, `undo_block`, `DmlDelta`, `MnListChange` |
| EV3 (`dash_evo::payments`, `dash_dml::{payments, creditpool, mnhf}`) | called | `block_value_valid`, `block_payee_valid`, `masternode_payee_valid`, `connect_records`, `check_block_diff`, `MnhfView` |
| EV4-serve (`dash_dml::sml`, `mnlistdiff`) | called | `SmlRootCache`, `merkle_root_mn_list_full`, `project_sml`, `MnListDiffSource` |
| LQ1a/LQ1b (`dash_evo::llmq`) | called | `LlmqCtx`, `LlmqChain`, `members_gate`, `members_route`, `compute_*`, `QuorumMemberSource`, `QuorumPrecompute`, `SnapshotRows`, `members_uncached` |
| LQ2 (`dash_dml::blockprocessor`, `dash_evo::llmq::commitment`) | called | `process_block`, `undo_block`, `quorum_root`, `QcHashCache`, `MineablePool`, `check_llmq_commitment` |
| LQ3/SB8 (`dash_evo::locks`) | called | `verify_chainlock`, `QuorumScanView` |
| BS11a/BS12 (base-sdk pkc) | called | lazy/eager BLS types, the serial and batch verifiers |
| SB19, AM1, GV1b, RP2a, MN1, MP1a, QM1a, LK1a/LK1b, SP1, IX1, O15 | consume | `ChainView.mn_list`, `ChainEvent::MnListChanged`, `ChainCommand::OnV`, `EvoReadView`, the effect-applier slots (§3.8), `ChainHelper` back ends (§4.5) |

---

## 3. CH7a: evo integration, in Core's order

### 3.1 `ConnectBlock`'s evo steps and the hooks

CH3a already runs `ConnectBlock`'s non-evo steps in Core's order and calls the hooks at Core's sites (CH3 §3.1; `rs:connect/serial.rs:622+`). CH7a gives each hook its body. Core's sequence (`v:2521-2990`, Core body `ConnectBlockImpl`; the wrapper at `v:2495-2522` is a hook):

| # | Core | Line | Hook | CH7a body |
|---|---|---|---|---|
| 1 | `ScopedBLSLegacyScheme guard{!V19 active at pindex}` | `v:2542-2543` | H1 `bls_ctx` | `bls_ctx_for_block(params, at.height)`, kept in the attempt (D9) |
| 2 | `CheckBlock`; `HasConflictingChainLock` → `bad-chainlock` (`BLOCK_CHAINLOCK`) | `v:2557-2571` | — (CH3a; `ChainHelper::cl_conflict`) | unchanged |
| 3 | DIP3 active at pindex and `!VerifyBestBlock(pprev)` → `AbortNode(EvoDbInconsistencyMessage())` | `v:2577-2583` | H2 `verify_best_block` | `layer.best_block() == Some(prev)`, an `evo_tip` at genesis read as absent (D7) |
| 4 | genesis: `SetBestBlock` unless `fJustCheck`, return | `v:2588-2592` | — | no evo processing; B1 writes `meta.evo_tip` for genesis like any tip, and H2 reads that value as absent, because Core writes no `b_b4` for genesis: a chain with DIP3 active at height 1 fails `VerifyBestBlock` at block 1 in both (`v:2577-2583`, `edb:120-126`; D7) |
| 5 | `-assumevalid` → `fScriptChecks` | `v:2594-2623` | — (CH3a) | passed to H4 as `check_cbtx_merkle_roots` (`v:2716`) |
| 6 | `special_tx_rules = GetSpecialTxRules(pprev)` | `v:2711`, `v:6960-6966` | — (CH3a, D40) | `SpecialTxRules { v24, evo_shares }` |
| 7 | **`ProcessSpecialTxsInBlock`**, before the UTXO loop, over the pre-block view | `v:2713-2721` | **H4** | §3.2 |
| 8 | UTXO loop; `CheckSharedCollateralSpends` when V24 active at pindex | `v:2740-2821`, `:2761-2769` | H8 | EV1's `check_shared_collateral_spends` (`stm:1901-1917`) |
| 9 | `control.Wait()` | `v:2831-2834` | — | unchanged |
| 10 | IS conflicts outside IBD (`HasChainLock`, `ConflictingISLockIfAny`, `RemoveConflictingISLockByTx`, `conflict-tx-lock`) | `v:2852-2872` | — (CH3a; `ChainHelper`) | the IS back end is LK1b's (§4.5) |
| 11 | `IsBlockValueValid` → `bad-cb-amount` (`BLOCK_RESULT_UNSET`) | `v:2899-2903` | — (CH3a + EV3) | unchanged; superblock arms per §3.6 |
| 12 | `IsBlockPayeeValid` → `bad-cb-payee` (`BLOCK_RESULT_UNSET`) | `v:2912-2918` | **H5** (the masternode arm) | §3.6 |
| 13 | `if (fJustCheck) return true` | `v:2936-2937` | — | the attempt ends; its delta is dropped (D8) |
| 14 | undo write, `RaiseValidity` | `v:2942-2956` | — (CH3a) | unchanged |
| 15 | `SetBestBlock`; **`m_evoDb.WriteBestBlock(hash)`** | `v:2966-2967` | **H6** | stages the delta's `evo_tip` = the block (D1, D7) |
| 16 | `bls_scheme_guard.Commit()` | `v:2970` | **H1 commit** | marks the attempt's BLS scheme for publication (D9) |
| 17 | `NotifyMasternodeListChanged(false, old, diff)` if active chainstate and the diff has changes | `v:2972-2976` | **H7** | synchronous, inside the attempt, before the commit (§5.2, D15) |

The evo transaction: Core opens it in `ConnectTip` (`v:3532`), runs `ConnectBlock`, flushes the coins view, commits the transaction into the root transaction (memory), then calls `FlushStateToDisk` (`v:3532`, `:3535`, `:3550`, `:3552`, `:3561`). CH3 maps this to H10 (`begin_tx`/`commit_tx`/`rollback_tx`) and the commit after `finish_attempt` (CH3 §3.2, D33). CH7a's H10:
- `begin_tx` opens the target's pending slot (`NodeEvo.pending` for the Active target, the scratch `EvoTxn`'s open entry for VerifyDB) with an empty `PendingTx`. It opens nothing durable; the active layer is its parent (D6).
- `commit_tx` moves the slot's delta, a connect's or a disconnect's, into the active index (Core's `CommitCurTransaction`, `edb:80-87`) and hands its `post_scheme` to the publication (D9).
- `rollback_tx` clears the slot (Core's committer destructor, `edb:17-21`, `RollbackCurTransaction`, `edb:89-96`).

**D1. One `EvoDelta` per attempt, in a pending slot the target owns.** The connector consumes the attempt in `finish_attempt` before it calls `commit_tx`, which takes only the target (`rs:connect/connector.rs:320-342`; `rs:connect/evo.rs:294`, `:301`). So the open transaction lives in the target's slot (`PendingTx`, §2.1), not in the attempt:
- **`begin_attempt`** (`rs:connect/evo.rs:165`) asserts the slot is empty, and returns `Fatal` otherwise. So no delta or scheme survives into the next attempt, also after a path on which `begin_tx` never ran (a failed coins lock or scratch overlay begins none, `rs:connect/connector.rs:303-308`). The attempt keeps only its log, handle, BLS context and origin.
- **`begin_tx`** opens the slot with an empty delta.
- **H4** (connect) records the block's hash and height at entry and fills the delta (§3.2). **H9** (disconnect) records the block's hash, height and `prev`, and appends only the mined-commitment tombstones (§4.1.4); Core's disconnect writes no other evo row (D6).
- **H6** stages the delta's `evo_tip`: the block on a connect, `pprev` on a disconnect (Core's `WriteBestBlock`, `v:2967`, `v:2389`).
- **`commit_tx`** moves it into the active index only after an outcome the connector commits: a `Connected` connect, or a disconnect the connector commits (`rs:connect/connector.rs:265-346`, `:399-453`). **`rollback_tx`** clears it on every other path on which `begin_tx` ran: a failed connect or disconnect, a check-only attempt, a `SystemError`, a `Fatal` and a failed commit (the connector calls it for each `begin_tx` not committed, `rs:connect/evo.rs:296-301`). On an unwind neither runs; the latch trips and the hooks are not used again (CH3a D71). VerifyDB's attempts use the scratch `EvoTxn`'s slot instead (§6.3).

Under `just_check` Core writes evo rows into a transaction it never commits (TBV `v:5098-5099`; under `fJustCheck` most writes are skipped anyway: `stm:997`, `bp:458-460`, `mnhf:210-262`). The node never builds the rows Core skips, and drops the rest with the attempt: nothing in a check-only delta can reach the layer, because `commit_tx` runs only after a committed `Connected` (CH3a D75).

**D8. A check-only attempt keeps Core's side effects, not its rows.** `just_check` (TBV `v:5117`; CH3a D16) runs H1–H5 and stops at `v:2936-2937`: no H6, no H1 commit, no H7. What Core does outside the doomed transaction still happens, because Core does it too: the precompute loop and any members miss compute and write rotation snapshots (`bp:234`, `lu:463`; ADD §2.5), the DML build reads `prev`'s list through `GetListForBlockInternal`, which caches it (`stm:989-996`, `dmn:829-942`), and the step-12 log line prints. The attempt's effect log is therefore applied on every outcome, as for a full connect (CH3 §3.2), and only its `EvoDelta` and its `post_scheme` are dropped. VerifyDB level 4 is not `just_check` (`v:5310`); its attempts are full connects into a scratch `EvoTxn` (§6.3).

### 3.2 H4: `ProcessSpecialTxsInBlock` (`stm:881-1085`)

Signature (CH3 §3.1; `rs:connect/evo.rs:210`):

```rust
fn process_special_txs(&mut self, attempt: &mut NodeEvoAttempt<'_>, evo: &mut EvoTarget<'_>, block: &Block, at: &BlockAt,
    rules: SpecialTxRules, view: &dyn CoinsView<Error = CoinsReadError>, subsidy: Amount,
    just_check: bool, check_cbtx_merkle_roots: bool)
    -> Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>;   // SpecialTxFailure: D2
```

`view` is the pre-block view, without this block's outputs (`v:2713`; quirk 26; EV2:636). The body is Core's, step by step. Each step names the Core lines, the Rust call, and the reject strings (all `BLOCK_CONSENSUS` unless stated).

| Step | Core | Rust | Gate | Rejects |
|---|---|---|---|---|
| 0 | `BlockPayloads` (Core parses each payload where it needs it) | EV1-2's `BlockPayloads::decode(block)` once (EV1:969, EV2:559) | always | a parse failure is reported by the step that would have parsed it in Core (EV1 §3.2) |
| 1 | CbTx parse and `CheckCbTx(cbtx, pprev, rules.v24)` | EV1's `check_cbtx` | `check_cbtx_merkle_roots` and `vtx[0].nType == COINBASE` (`stm:899-917`) | `bad-cbtx-invalid`, `bad-cbtx-payload`, the tx reason re-wrapped (`stm:910`); debug "Special Transaction check failed (tx hash %s) %s" |
| 2 | missing CbTx after DIP3 | — | `check_cbtx_merkle_roots` and DIP3 active at pindex (`stm:918-923`) | `bad-cbtx-version` |
| 3 | credit-pool indexes from `GetCreditPool(pprev)` | `pool_after(store, prev)` (EV3) through the attempt's read view | V20 active at pindex (`stm:930-935`) | none here; a store fault is `Fatal` (D5) |
| 4 | per tx in block order: `CheckSharedCollateralTemplateOutputs` (if `rules.v24`, every tx incl. the coinbase); skip a coinbase-type `vtx[0]`; `CheckSpecialTxInner(chain, tx, pprev, rules, view, indexes, check_sigs = check_cbtx_merkle_roots)` | EV1-3…5's dispatcher `check_special_tx_inner` with `SpecialTxCtx { prev, rules, coins: view, indexes, check_sigs, chain: Some(attempt chain), members: &handle, quorums: &handle }` (EV1:138-158, :235) | always (`stm:937-960`) | `bad-shared-collateral-create`; each type's reason re-wrapped (`stm:957`); `failed-check-special-tx` for Core's inner catch (`stm:292-295`); `bad-tx-type-check` |
| 5 | `CheckCreditPoolDiffForBlock` | EV3's `check_block_diff(block, cbtx.credit_pool_balance, prev_pool, platform_reward, dip8, v20)` | only if step 1 parsed a CbTx (`stm:966-972`), and DIP8 and V20 active at pindex (`stm:1127-1128`) | `bad-cbtx-assetlocked-amount`; `failed-checkcreditpooldiff` (rc.4's `GetTotalLocked` overflow, `cp:297-305`); the pool's `failed-creditpool-*` reasons re-wrapped (`cp:411`) |
| 6 | quorum `ProcessBlock(block, pindex, fJustCheck, fBLSChecks = check_cbtx_merkle_roots)` | LQ2's `process_block` through `connect_commitments` (§4.1.2): from CH7a-2 over `NoMembers`, so the count, duplicate, premature and null rules are live (D2); from CH7b-2 over the production facade | always (`stm:979-982`) | LQ2's `bad-qc-*` |
| 7 | DML: `BuildNewListFromBlock(block, pprev, rules.v24, view, debugLogs = true)`; `SetBlockHash`; `if (!fJustCheck) m_dmnman.ProcessBlock(...)` | `resolve_list(prev)` → `build_new_list_from_block(block, prev, &prev_list, view, &handle, &rules, &mut log, &mut trace)` → if `!just_check`: `process_block(block, at, &prev_list, built.list, &params, first_dip3, &mut log)` | DIP3 active at pindex (`stm:989-1001`) | EV2b's reasons (`bad-protx-*`, `bad-qc-payload`, `bad-qc-commitment-type`, `bad-qc-quorum-hash`, `bad-shared-collateral-spend`); `failed-dmn-block` (`dmn:757-760`); `bad-dip3-enf-block` (`dmn:776-783`) |
| 8 | `CalcCbTxMerkleRootMNList(mn_list.to_sml())` ≠ CbTx → `bad-cbtx-mnmerkleroot` | `SmlRootCache::merkle_root(&list)` (EV4-serve; one per chainstate, §3.7) → `check_mn_list_root` | step 1 parsed a CbTx (`stm:1008-1053`) | `bad-cbtx-mnmerkleroot`, `mutated-calc-cb-mnmerkleroot`, `mutated-cached-calc-cb-mnmerkleroot`, `failed-calc-cb-mnmerkleroot` (`sml:102-146`) |
| 9 | `CalcCbTxMerkleRootQuorums` if `cbTx.nVersion >= MERKLE_ROOT_QUORUMS` | from CH7a-2: LQ2's `quorum_root(&llmq, &mined, &mut qc_cache, block, prev)` → `check_quorums_root` | as step 8 | `bad-cbtx-quorummerkleroot`, `commitment-not-found`, `bad-qc-payload-calc-cbtx-quorummerkleroot`, `bad-qc-commitment-type-calc-cbtx-quorummerkleroot`, `excess-quorums-calc-cbtx-quorummerkleroot`, `mutated-calc-cbtx-quorummerkleroot` (`cbtx:65-156`) |
| 10 | `CheckCbTxBestChainlock` | `check_cbtx_best_chainlock` (§4.5.2): from CH7a-2 every arm before `VerifyChainLock` (`stm:174-217`, `:233-235`; `cl_best` answers none until LK1a, which `stm:176` and `:211` handle); the verify (`stm:226`) is D2's refusal until CH7b-3 | as step 8; version ≥ `CLSIG_AND_BALANCE` (`stm:160-162`) | `bad-cbtx-null-clsig`, `bad-cbtx-older-clsig`, `bad-cbtx-cldiff`, `bad-cbtx-cldiff-ancestor`, `bad-cbtx-invalid-clsig` (`stm:156-239`) |
| 11 | MNHF `ProcessBlock(block, pindex, fJustCheck)` | `MnhfView::process_block(block, id, just_check)` | always (`stm:1055-1067`); its own V20 gate (`mnhf:364-383`) | `bad-mnhf-duplicates-in-block`, `bad-mnhf-duplicate`, `bad-mnhf-non-mn-fork`, `failed-proc-mnhf-inblock` (`mnhf:190-262`) |
| 12 | V19 active **after** pindex and the global is legacy → switch to basic, log "CSpecialTxProcessor::%s -- bls_legacy_scheme=%d" | sets the attempt's `post_scheme` (D9); the log line is emitted here, as Core's, for every attempt that switches (also `just_check`: Core logs and then the guard reverts) | always (`stm:1069-1074`) | none |
| 13 | catch: `EvoDbInconsistencyError` → `state.Error`; other `std::exception` → log "CSpecialTxProcessor::%s -- FAILURE! %s", `failed-procspectxsinblock` | D5 | — (`stm:1075-1082`) | `failed-procspectxsinblock` |

The bench lines (`stm:927`, `:964`, `:976`, `:986`, `:1005`, and the CbTx ones) are `BCLog::BENCHMARK` category logs. **D19.** CH7a emits each of them under the node's `bench` category with Core's text and Core's running totals (the statics at `stm:890-895` become fields of `NodeEvo`, reset only at process start, as Core's are), so a log diff against Core lines up. They are diagnostics: no test compares their timings.

**Order notes Core's code imposes.** Each is kept, and each has a test (§7.2):
- Script checks finish **after** every evo step (step 9 of §3.1 runs after H4). A block with both an invalid special transaction and an invalid script reports the special transaction.
- The MN-list root is computed from the list **after** step 7 has written it into the transaction, and the quorum root after step 6 has written the mined rows (`stm:1008-1053`). Both are in the attempt's delta, so a root failure leaves nothing outside the attempt (D1). `quorum_root` reads `GetQcHashes(pprev)`, which seeks below this block's height, so the block's own rows do not double-count (`cbtx:65-156`).
- Steps 5 (pool diff) and 6 (quorum processor) come **after** the per-tx loop: a block with a bad asset unlock and a bad QC reports the unlock.
- Step 7's QC branch calls `GetAllQuorumMembers` with `reset_cache = false` (`stm:746-748` inside `ApplyTxToList`), which fills the members caches; through the attempt handle this is a facade call that may store rows (ADD §2.6 route C).
- Step 11 (MNHF) runs **after** the roots: a block with a bad root and a duplicate MNHF signal reports the root.

**D2. Before CH7b-2, H4 runs on a stub facade that refuses through a real channel.** CH7a has no `MembersFacade`. Its `Facade` is `NoMembers`, which implements `DmlMemberSource`, `QuorumPrecompute` and EV1-5's `QuorumLookup`:
- **`precompute`** runs Core's loop (§4.3) and refuses only where the loop calls `GetAllQuorumMembers`: a rotation cycle base (§4.3's predicate). On every other block it returns `Ok` having done nothing, as Core's loop does (`bp:35-43`), so null-commitment blocks connect.
- **Members and quorum calls** refuse.
- **The refusal** is a new `dash-dml` variant, `StoreError::NotBuilt(&'static str)`, carrying "quorum validation (CH7b)". EV1, EV2b and LQ2 propagate it unchanged as the members source's error (inside `MemberError::View`, then `QcBlockError::Fault` or EV2b's store arm), never as `Exception`, `failed-procspectxsinblock` or a reject. Each crate gets a test that an injected `NotBuilt` comes back unchanged.
- **H4 maps it** at its boundary, before D5's fault mapping, to `SpecialTxFailure::NotReady(capability)`. H4's inner error widens from `BlockValidationError` to `SpecialTxFailure` (below), a CH3 §3.1 erratum (§11.2); Q14's "no `EvoHooks` change" concerned `NoReason` only. The connector turns `NotReady` into its existing readiness refusal, `refuse(refusal_text(params, at, capability), just_check)` (`rs:connect/serial.rs:484-491`): the reported `SystemError` on the tip path, and the proposal's unreported error under `just_check` (CH3a D121). It is never a verdict.
- **H9** needs no such channel: a block G1q or the stub refused was never connected, so a `NotBuilt` in H9 is `Fatal`.
- **Step 10's verify.** Step 10 receives its verify through its own seam, `ClVerifier` (in `dashd-chainstate::evo`, beside `check_cbtx_best_chainlock`; §4.5.2), not through the members or quorum facade, so CH7b-2's facade swap does not serve it. From CH7a-2 to CH7b-2 the seam is `NotBuiltClVerifier`, whose `VerifyChainLock` (`stm:226`) returns `StoreError::NotBuilt("ChainLock verification (CH7b)")` through the same channel, at Core's own position. Its capability differs from G1q's and `NoMembers`'s "quorum validation (CH7b)", so a refusal in an L2 log says which gate fired. Because the stub sits at Core's position, every step and arm before it runs first, with Core's rejects and side effects, and under `-assumevalid` step 10 does not run (`stm:899-917`, `:1008-1053`). From CH7b-2 the production facade replaces `NoMembers`; CH7b-3 replaces `NotBuiltClVerifier` with the real verifier (§4.5.2).

**The seam.** CH3's `connect` module defines the enum beside `EvoHooks`, and every H4 signature uses it:

```rust
// dashd_chainstate::connect, beside EvoHooks (rs:connect/evo.rs)
pub enum SpecialTxFailure {
    Invalid(BlockValidationError), // Core's state.Invalid: a verdict
    NotReady(&'static str),        // a readiness refusal carrying its capability (D2, D3); never a verdict
}

// EvoHooks::process_special_txs (§3.2; rs:connect/evo.rs:210)
    -> Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>;
```

- **Connector.** The step layer's trait `AttemptSteps` (`rs:connect/serial.rs:98`; `process_special_txs` at `:114-123`) changes its inner type to `Result<(), SpecialTxFailure>`. Its implementations follow: `HookSteps` (`rs:connect/connector.rs:610`, `:623-645`) passes the enum through, and `NoEvoSteps` (`rs:connect/bench.rs:51`, `:64`) wraps `pre_dip3_special_txs`'s error in `Invalid`. At the call site (`rs:connect/serial.rs:816-829`), `Invalid(err)` keeps today's arm: the log line "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {} failed with {err}" and `Step::Stop(ConnectOutcome::Invalid(err))`. `NotReady(cap)` becomes `Step::Stop(self.refuse(refusal_text(cx.params, at, cap), just_check))`, as G1 refuses at `serial.rs:688`, with no "failed" line, because Core reached no verdict.
- **`EvoHooks` implementations.** `NoEvo` (`rs:connect/evo.rs:336`, H4 at `:386`) and the test hooks `Ch7aShape` (`rs:chain/tests/attempt.rs:111`, `:183`) and `TestEvo` (`rs:chain/tests/connector.rs:268`, `:350`) wrap their errors in `Invalid` and never return `NotReady`. `NodeEvo` returns `NotReady` from G1q (D3) and from the `NotBuilt` mapping (§4.1.2 shows the LQ2 arm).
- **Under VerifyDB.** VerifyDB at `-checklevel=4` reconnects through H4 with Core's `fScriptChecks` (`v:5179-5327`), so it can reach a refusal: for example a CH7a-4…CH7b-2 datadir whose last blocks carry a valid CbTx ChainLock connected under `-assumevalid`, reopened with `-checklevel=4 -assumevalid=0`, reaches the verify stub. For origin `VerifyDb`, `refuse` does not call `abort_node`: it returns the `SystemError` unreported, as for a proposal, and VerifyDB ends with a failure that carries the refusal text, not Core's "found unconnectable block" and `CORRUPTED_BLOCK_DB` (`v:5310-5312`), because the build cannot tell whether Core would accept the block. No block gets a verdict or a failure mark, and the scratch `EvoTxn` is dropped. At startup the load then fails with that text (CH5's `VerifyLoaded` result); a running node's `verifychain` returns false and logs it, and the node keeps running. CH5 carries the result variant (§11.2).
- **Tests** (§7.2): `NotReady` gives the reported refusal on the tip path, the unreported error for a proposal, and the VerifyDB failure above; `Invalid` keeps today's verdict and log line.

A block needs the facade exactly when Core would call `GetAllQuorumMembers`, `GetQuorum` or `ScanQuorums` for it: a non-null QC (steps 4, 6, 7), a rotation cycle-base height (step 6's precompute), an MNHF or asset-unlock signature (step 4), or a valid CbTx ChainLock signature (step 10's verify, `stm:226`). D3's guard refuses, at H4's entry, a well-formed block that needs one of the first three; the ChainLock verify is refused at its own position (above). Apart from step 10's verify, the stub is reached only in a block the guard let through because a payload does not decode, and only when Core's order needs a quorum answer before it parses that payload (for example a well-formed MNHF signal earlier in the block than a malformed QC): Core's verdict there depends on the quorum, so the refusal stands. A test covers that case.

**D3. G1 is lifted in stages; G1q holds until CH7b-2** (**Q3**). CH3 §2.17's guard G1 (`rs:connect/guard.rs:17-47`) refuses blocks at which DIP0003, V20, MN_RR, V24 or EVO_SHARES is active while `COMPLETE` is false. CH7a-4 sets `NodeEvo::COMPLETE`, so G1 is gone, and a new guard **G1q** refuses, per slice, the well-formed blocks whose processing needs an answer the slice cannot give yet:
- **Position.** G1q is one pure function, `evo::guard::g1q(block, &payloads, prev, &ctx, arms)`, evaluated at H4's entry after step 0. So every reject Core's `ConnectBlock` reaches before `ProcessSpecialTxsInBlock` (G1's position is before `VerifyBestBlock` and BIP30, `rs:connect/serial.rs:684-689`) comes first. Its refusal is D2's `NotReady("quorum validation (CH7b)")`, in D69's format (CH3a D69), with D2's reporting.
- **Startup.** The startup check (CH3a D70) returns early on `H::COMPLETE` (`rs:chain/load.rs:382-384`). CH7a-4 turns that early return into a call, for the tip and the replayed blocks, of a new provided associated function `EvoHooks::startup_refusal(block: &Block, at: &BlockAt, deployments: &dyn DeploymentView) -> Option<&'static str>` (default `None`), whose capability the check formats with `refusal_text`, as CH3's other refusals are (a CH3 erratum, §11.2). It takes no receiver, like `H::COMPLETE`: `check_startup_readiness<H: EvoHooks>` (`rs:chain/load.rs:376-381`) is generic over the type, and the node calls it as `check_startup_readiness::<NodeEvoHooks>` (`rs:dashd-node/src/steps.rs:263`) before V starts (`steps.rs:277`), when no `NodeEvo` exists. The check calls `H::startup_refusal(...)` when `H::COMPLETE`, passing `ChainDeployments::new(...).view()` (`load.rs:386-387`); its `NoEhfSignals` is enough, because arm (c) reads only DIP0024, DIP0020, TESTDUMMY and heights (`IsQuorumTypeEnabled`, `IsQuorumRotationEnabled` at `llmq/options.cpp:50-62`). `NodeEvoHooks` answers with G1q's arms (a)–(c), reading the blocks' data for the QC fields. Those arms are pure functions of the block, its parent and the deployments, with no assumevalid input, so a block the same binary connected never trips them at startup, and a datadir it built always restarts. Step 10's verify refusal is no startup arm: the startup check runs no H4 (VerifyDB at `-checklevel=4` does, and D2 states that outcome), and a tip connected under `-assumevalid`, whose CbTx ChainLock step 10 never checked, is Core's state too. With G1q removed at CH7b-2, `NodeEvoHooks` drops the override and the default `None` applies, so a datadir synced past V20 under `-assumevalid` restarts (§7.2).

| Slices | G1 | G1q's arms (each on a well-formed block) |
|---|---|---|
| CH7a-1…3 | refuses every evo-active block | — |
| CH7a-4, CH7a-5, CH7b-1 | lifted | (a) a non-null QC; (b) an MNHF signal or an asset unlock; (c) a rotation cycle base |
| CH7b-2 and later | lifted | none: G1q is removed with its tests turned around |

- **(a)** a transaction of raw type `TRANSACTION_QUORUM_COMMITMENT` whose commitment is non-null.
- **(b)** a transaction of raw type `TRANSACTION_MNHF_SIGNAL` or `TRANSACTION_ASSET_UNLOCK`.
- **(c)** for a type enabled at `pprev`, `IsQuorumRotationEnabled(params, block)` (`useRotation` and DIP0024 active after the ancestor at `cycleBase − 1`, `llmq/options.cpp:50-62`) and `h % dkgInterval == 0`: the predicate §4.3's loop and `NoMembers::precompute` use.
- **No CbTx ChainLock arm** (rev3.1, DEC-527). Step 10's verify is D2's refusal at Core's own position (`stm:226`), so G1q needs no arm for it. Steps 1–9 and step 10's member-free arms run first and give Core's rejects (`bad-cbtx-older-clsig` `stm:199-201`, `bad-cbtx-cldiff` `:207-209`, `bad-cbtx-cldiff-ancestor` `:218-222`, and the null-signature `bad-cbtx-cldiff` `:233-235`), and their side effects happen as in Core (for example step 7's `ResetPlatformBan`, `dmn:737-752`). Under `-assumevalid` Core skips step 10 (`stm:899-917`, `:1008-1053`), and so does H4.
- **Declared interim difference (arms (a)–(c), CH7a-4 to CH7b-1).** G1q refuses at H4's entry, so it preempts every reject Core reaches before its first members, quorum or precompute call in that block. Where Core marks such a block invalid and continues, the node refuses: on the tip path that stops it (`rs:connect/serial.rs:484-491`; CH3 §2.17), and a proposal gets the unreported error. It never accepts. The preempted rejects are:
  - for every arm: CbTx steps 1 and 2 (`stm:899-923`; a decode failure bypasses G1q, so `bad-cbtx-payload` is not among them), the step-4 rejects of every transaction ahead of the first quorum-dependent one (`stm:937-960`: `bad-shared-collateral-create`, each type's reason, `failed-check-special-tx`, `bad-tx-type-check`), and that transaction's own shared-collateral check, which runs under `rules.v24` before its `CheckSpecialTxInner` (`stm:940-946`; for an asset unlock it is the rule that rejects one paying the template, `al:196-199`);
  - **(a):** the non-null QC's own `bad-qc-commitment-type`, `bad-qc-version`, `bad-qc-height` and `bad-qc-quorum-hash` (`commitment.cpp:240-300`), and the `bad-qc-invalid` (`commitment.cpp:304`) that `Verify` (called at `:301`) returns from its member-free checks before its members call at `:177`: a parentless base (`:125-128`), the wrong `nVersion` (`:130-137`), the wrong `quorumIndex` (`:144-147`), bad sizes (`:149-151`), too few valid members or signers (`:153-160`), an invalid `quorumPublicKey` (`:161-164`), a null `quorumVvecHash` (`:165-168`), or an invalid `membersSig` or `quorumSig` (`:169-176`);
  - **(b):** an MNHF signal's `bad-mnhf-version`, `bad-mnhf-nbit-out-of-bounds`, `bad-mnhf-type`, `bad-mnhf-non-ehf` and `bad-mnhf-quorum-hash` (`mnhf:97-135`; `GetQuorum` at `:146`); an asset unlock's `bad-assetunlocktx-type`, `-have-input`, `-too-many-outs`, `-version`, `-version-2`, `bad-assetunlock-duplicated-index` and `bad-assetunlock-quorum-hash` (`al:185-219`; `ScanQuorums` at `:123` comes next);
  - **(c):** every reject of steps 1–5 (`stm:899-972`), because the precompute is step 6's first call (`bp:234`).

  After that first call Core's reject string depends on the answer, which the slice cannot give, so only the rejects before it are declared. The arms stay as defence in depth: without them a `NotBuilt` that a port maps wrongly would become a verdict, such as `bad-mnhf-missing-quorum` (`mnhf:148`) or `bad-assetunlock-quorum-internal-error` (`al:142`). O2b `diff_chain` vectors and CH7a-5's L2 runs read a refusal on such a block as this declared difference. CH7b-2 removes it, and §7.2 pins both sides.

G1q's arms read raw transaction types, with one tri-state input from step 0's `BlockPayloads`:
- **Decode first.** If any special payload in the block fails to decode (CbTx, QC, ProTx, MNHF signal, asset lock or unlock; step 0 reports it), G1q itself does not refuse. H4 runs and returns Core's reject at the step that would have parsed it (`bad-cbtx-payload`, `stm:899-915`; `bad-qc-payload`, `bp:608-648`; `bad-protx-payload`). So G1q refuses only blocks whose payloads all decode. H4 may still return D2's refusal in a bypassed block, when Core's order reaches a quorum-dependent operation before the malformed payload.
- **Otherwise it refuses** if one of the slice's arms holds.
- **Why not the raw type alone.** QC nullness is read from the decoded payload because the raw type cannot tell a null commitment, which needs no members (`VerifyNull`), from a non-null one. A raw-type-only rule would refuse every mining window's null commitments.

On regtest and devnet chains whose LLMQs are not active yet (null commitments only, no rotation, no CL) it never fires. Because every member-free check of H4 runs from CH7a-2 (steps 6 and 9, and step 10's arms before the verify), CH7a-4 connects those chains as dashd does and rejects their invalid blocks as dashd does. The guard tests (§7.2) include a malformed QC, a malformed CbTx and a malformed ProTx, each alone and each next to a well-formed non-null QC: every one gets Core's reject string, not `SystemError`. A block whose only quorum dependency is a valid CbTx ChainLock passes G1q and runs H4 to step 10, where the verify refuses until CH7b-3 (D2); with `-assumevalid` on, step 10 does not run, and the block connects.

**D4. Step 4's quorum lookups go through the attempt.** EV1-5's `QuorumLookup` for MNHF and asset-unlock signatures (`qman.GetQuorum(type, hash, chain)`, `mnhf:106-174`, `al:106-153`) and the `ScanQuorums(..., chain)` for an unlock's too-old check are route C (ADD §2.6): they use the attempt handle and its overlay. Core's chain-aware variants neither read nor write `scanQuorumsCache` (`quorumsman.cpp:205-247`, `:283`) but populate `mapQuorumsCache` through `GetQuorum` (`:59-105`). CH7b's materializer keeps both behaviours (§4.4).

**D5. Exception mapping, stated once** (DEC-50 (2), EV1:322, EV2:533-537, EV3 D21; DEC-518). Every failure inside H4 or H9 is one of four things:
- **A Core reject** (`BlockValidationError` with Core's reason and class): every `state.Invalid` Core reaches, including the strings Core produces from a caught exception. The ported functions return those reasons as values (`DmlBuildError::Exception` → `failed-procspectxsinblock`, EV2b; `failed-check-special-tx`, EV1; `failed-checkcreditpooldiff`, `failed-getcreditpooldiff`, EV3; `failed-proc-mnhf-inblock`, `failed-dmn-block`).
- **A store or invariant fault** (`StoreError`, `NodeMemberError::Store`, `QcBlockError::Fault`, a decode of our own row that fails; `QcBlockError::BlsUnsupported`, SB8's no-verdict domain, takes the same path as in rev0): `Fatal` through `abort_node` (A1 I5, LQ1 D4, ADD §2.3's node column). Core's own counterparts are `AbortNode` + `EvoDbInconsistencyError` → `state.Error` (`stm:1075-1077`, `dmn:717-733`, `cp:146-164`, `mnhf:364-383`, `bp:473-476`): the node stops and the block is not marked invalid. The node does the same.
- **A readiness refusal** (G1q, D3, or a `NotBuilt` from `NoMembers` or step 10's verify stub, D2): `SpecialTxFailure::NotReady`, mapped before the fault class.
- **A reason-less quorum failure** (LQ2's `QcBlockError::NoReason`, `rs:dash-dml/src/blockprocessor.rs:80-90`; Core's `return false` without a state at `bp:282-287`, `bp:372-376` and `bp:441-446`): H4 returns `Err(Fatal::new(..))`, the AbortNode class CH3 already has for hooks. A hook has no `ConnectOutcome`; the connector builds `SystemError` itself (`rs:connect/serial.rs:476-490`), and a hook's `Fatal` is the same class. The block is not marked failed, and V stops and the node shuts down. Core's ActivateBestChain returns false there without an abort (`v:3768-3773`, `v:3900-3903`). The test injects `NoReason` at each of the three places and asserts no failure mark and an abort.

No Rust code catches a panic to make a reject: a panic in a hook is an unwind, which CH3a D71 turns into a latch trip. Core's catch-all exists for exceptions that ported code returns as values (EV1 §3.1's census).

### 3.3 H9: `UndoSpecialTxsInBlock` (`stm:1087-1120`) and the disconnect

Core's `DisconnectBlockImpl` (`v:2305-2402`) runs, in order: `VerifyBestBlock(pindex)` (`v:2310-2315`, H2); undo-data reads; **`UndoSpecialTxsInBlock` before the UTXO undo** (`v:2332-2336`, H9); the UTXO undo; `SetBestBlock(pprev)` and `WriteBestBlock(pprev)` (`v:2388-2389`, H6); `NotifyMasternodeListChanged(true, …)` (`v:2391-2395`, H7). `DisconnectTip` opens the evo transaction (`v:3381`) and keeps the scheme only if the chainstate is active (`v:3424`).

H9's body, in Core's order:
1. **BLS:** H9 runs under `bls_ctx_for_block(pindex)` on every origin; that context decodes the block's QCs for the undo's `GetCommitmentsFromBlock` and its precompute. On the tip path it is the scheme `DisconnectTip`'s guard enters before `DisconnectBlock` (`v:3376-3377`), so Core's switch here (`!V19 at pindex && !legacy`, `stm:1091-1099`) never fires there. It fires only in VerifyDB level 3, whose `DisconnectBlock` has no per-block guard (`v:5263`): for origin `VerifyDb`, if V19 is not active at pindex and VerifyDB's running scheme (which starts at the committed scheme) is basic, H9 switches the running scheme to legacy, sets `post_scheme`, and logs "CSpecialTxProcessor::%s -- bls_legacy_scheme=%d". Level 3 descends from the tip, so the running scheme after the switch equals `bls_ctx_for_block(pindex)`.
2. **MNHF:** `MnhfView::undo_block(block, id)` (`mnhf:264-283`). Core writes and erases nothing and asserts each signal is known; `Ok(false)` fails the disconnect.
3. **DML:** `dash_dml::undo_block(&cur, &prev, &stored_diff)` (`dmn:787-820`). Core erases the block's entries from `mnListsCache`/`mnListDiffsCache` but **no evodb row**: `dmn_D4` and `dmn_S3` stay (re-connect hits `WriteDerived`'s identical-value path). The node keeps them too (D6).
4. **Quorums:** CH7b's block undo (§4.1.4): the precompute with `reset_cache = true` (`bp:566`), then per non-null QC a full tombstone of the mined rows appended to the disconnect delta, the qc-hash cache drop, the mined-block cache erase, and the mineable re-insert effect (`bp:573-601`).
5. On a caught exception Core restores the saved scheme and fails the disconnect (`stm:1112-1117`): here a store fault is `Fatal` (D5), and `Ok(None)` is Core's `false`.

There is **no credit-pool undo** in Core: the pool is derived per block hash. The node keeps the block's pool and credit-data rows (D6).

### 3.4 The BLS scheme (H1; O17 CONS-09 in v24)

**D9. The scheme is a per-attempt value; the published scheme changes only on commit.** Core has a process-wide `bls::bls_legacy_scheme`:
- `ConnectBlock` enters legacy iff V19 is not active at pindex (`v:2542-2543`; the guard's class `v:139-180`), so the block's own checks run under the parent's scheme.
- Step 12 of §3.2 switches to basic when V19 is active **after** pindex (`stm:1069-1074`).
- The guard reverts unless `Commit()` runs, which happens only after a full connect (`v:2936-2937`, `v:2970`). A failed connect, `fJustCheck`, TBV and VerifyDB (`v:5194`) therefore leave the global as it was.
- Mempool and P2P use whatever the global holds (no guard on the mempool path, `stm:300-305`; its callers `v:810-830`, `v:936-950`).

The node has no global (A1 §2.10.2; `rs:dash-evo/src/bls.rs`):
- H1 returns `bls_ctx_for_block(params, at.height)`: the scheme the guard enters. It is fixed for the attempt, and every BLS decode and verify inside it uses it. A commitment's own scheme is decided by its mining block's V19 status (SB11 D12, SB8 D3), which is the same value.
- Step 12 (and H9 step 1) set the attempt's `post_scheme`.
- H1's `commit_bls_scheme` marks `post_scheme` for publication. After the connect commits (CH3 §3.2), V publishes `ChainView.bls = post_scheme` with the new tip (§5.1). Mempool, P2P and RPC read `ChainView.bls`, which is `bls_ctx_after(tip)`. A check-only, failed or VerifyDB attempt never publishes.
- **The guard's log lines.** Core's `ScopedBLSLegacyScheme` prints "ScopedBLSLegacyScheme: entered bls_legacy_scheme=%d" when it changes the global and "ScopedBLSLegacyScheme: reverted bls_legacy_scheme=%d" when it restores it uncommitted (`v:155-170`); its instances are at `v:2542`, `v:3376`, `v:3509` and `v:5194`. The node prints each line where Core's guard would change the value. "entered" prints when an attempt's `bls_ctx_for_block` differs from the current scheme: the committed scheme, which `ChainView.bls` publishes, or VerifyDB's running scheme for origin `VerifyDb`. "reverted" prints when such an attempt ends uncommitted, and at VerifyDB's end when its running scheme differs from the committed one (`v:5194`). On the tip path a connect never prints, because the scheme it enters is `bls_ctx_after(prev)`, the committed one; a disconnect of the last block before V19 activation prints "entered … =1". After `Commit()`, Core's bare `ConnectTip` guard (`v:3509`) reverts only when a later step fails; in the node that is a `Fatal`, and the stopping node prints no line.
- **At startup** Core sets the scheme from V19 after the tip (`cs:429-434`) and re-asserts it after VerifyDB (`cs:456-461`). The node computes `bls_ctx_after(tip)` for the first `ChainView`. There is no persisted flag: A1 §2.9's `meta.bls_legacy` tag, which the code does not have (`rs:store/meta.rs`), is dropped (an A1 erratum, §11).

This is CONS-09's v24 side: the scheme's scope is the attempt, on every network. O17's v23.1.8 leak is not modelled (DEC-82, **Q4**).

### 3.5 Credit pool, MNHF, DML lists: what is stored and when

#### 3.5.1 Core's writes inside the transaction, per block

| Core row | Written | Line | Node row | Notes |
|---|---|---|---|---|
| `dmn_D4` (list diff) | every DIP3 block, `!fJustCheck` | `dmn:717-722` | `mn_diff` (EvoDelta.dml) | `WriteDerived`: identical value → no write; different → `AbortNode` |
| `dmn_S3` (list snapshot) | `h % 576 == 0` or the first DIP3 block | `dmn:723-733` | `mn_snapshot` (EvoDelta.dml) | same |
| `cpm_S` (credit pool) | when a pool is built at `h % 576 == 0` **and** a transaction is active | `cp:146-164` | `credit_pool` (EvoDelta.credit_pool) | **D10** |
| credit block data | never | — | `credit_pool` CF's data rows (EvoDelta.credit_data, BL-259) | **D10** |
| `mnhf_s2` (EHF signals) | every V20 block, `!fJustCheck` | `mnhf:364-383` | `mnhf` (EvoDelta.mnhf_signals) | `WriteDerived` |
| `q_mc`, `q_mcih` / `q_mcihi` (mined commitment and its index) | per non-null QC, `!fJustCheck` | `bp:473-483` | `llmq_mined`, `llmq_mined_h` (EvoDelta.mined_commitments) | **Q1** |
| `q_bbu2` (upgrade marker) | every block, also `fJustCheck` | `bp:229-232`, `:303`, `:603` | — | **D11** |
| `b_b4` (evo best block) | every non-genesis full connect and disconnect | `v:2967`, `v:2389` | `meta.evo_tip` (B1) | D7 |

**D6. The transactional class is Core's transaction boundary.** Every row in the table above except `q_bbu2` is an `EvoDelta` field. A failed or check-only attempt never commits it (D1). On disconnect, Core erases only the mined-commitment rows (`bp:573-601`); every other row stays, and the node keeps it (`dash_dml::store` already says list, pool and MNHF rows are never erased, `rs:dash-dml/src/store/mod.rs:17-19`). Because rows are keyed by block hash and never rewritten with different content (Core's `WriteDerived` makes a different value an `AbortNode`; `MemEvoStore::apply` checks the same, `rs:dash-dml/src/store/mem.rs:70-88`), a reconnect writes identical bytes.

**D10. Derived rows are written with the block that defines them.** The node computes block B's credit data and credit pool in B's own attempt (EV3's `connect_records`, `rs:dash-dml/src/creditpool.rs:601`) and stores both with B in B's `EvoDelta` (BL-259 gates CH7a's READY). B's MNHF signals are stored with B the same way (§3.5.3). The values are equal by construction: each is the same function of the same inputs as Core's (`GetCreditPool`, `cp:256-275`; `CMNHFManager::ProcessBlock`, `mnhf:210-262`), so `getblock`'s `cbTx`, the CbTx balance checks, the asset-unlock limits and `getassetunlockstatuses` read identical answers. Which rows exist on disk is the node's own layout (the store is ours, not Core's evodb; CH6b translates).

**D11. `q_bbu2` is not modelled.** It is Core's best-block marker for the quorum processor's DB upgrade (`DB_BEST_BLOCK_UPGRADE`, `bp:57-61`), read by nothing on v24's validation path. The node's schema has no such row (`rs:store/schema.rs:7`).

#### 3.5.2 Credit pool

The per-block calls are EV3's (`rs:dash-dml/src/creditpool.rs`), in Core's two places:
- **Step 3 (indexes).** `pool_after(prev)`. Core reads it at `stm:932` and again inside `GetCreditPoolDiffForBlock` (`cp:402`): both read the same cached value, so one read serves both.
- **Step 5 (diff).** `check_block_diff(...)`, with rc.4's checked sums (`cp:46-50`, `:297-305`, `:316-320`, `:336-339`).
- **This block's records.** `connect_records(block, at, &prev_pool, window_start)` gives `CreditRecords { data, pool }`; both go into the delta. `PoolState = Result<CreditPool, PoolError>` is stored with its error state (EV3 D8: a stored `PoolError` makes every child fail with `failed-procspectxsinblock` exactly where Core's first read throws). The codec is BL-259's.
- **Mempool** (`v:936-950`: `GetCreditPool(Tip()).indexes.Contains(index)` → `txn-already-known`) reads `ChainView`'s pool at the tip through `EvoReadView` (MP1a).

#### 3.5.3 MNHF and the EHF deployments

`MnhfView` (`rs:dash-dml/src/mnhf.rs:260`) is bound to the attempt's read view. Two consumers:
- **Step 11** (`process_block`) and H9 step 2 (`undo_block`).
- **Versionbits.** Core's `VersionBitsConditionChecker::SignalHeight` asks `GetSignalsStage(pindexPrev)` for EHF deployments (`versionbits.cpp:238-252`). `ChainDeployments.ehf` is `NoEhfSignals` today (`rs:chain/deployments.rs:30-37`, CH3a D83). CH7a replaces it with a node `EhfSignalSource` over `MnhfView::signals_stage` on V's read view (EV3 D20). The versionbits cache keys by block, so the source's answers are pure functions of the chain.

`ForceSignalDBUpdate(tip)` at startup (`cs:281`, `mnhf:392-409`) rebuilds missing `mnhf_s2` rows for a Core datadir upgraded from an older version. The node's rows are written by every V20 block (D6), so there is nothing to rebuild (**Q6**). The startup check (§6.3) verifies that the tip's row exists when V20 is active at the tip.

#### 3.5.4 DML lists

`resolve_list(at, chain, deployments, store, cache)` (`rs:dash-dml/src/store/resolve.rs:169`) is Core's `GetListForBlockInternal` (`dmn:829-942`): walk back through the cache, snapshots and diffs; apply forward. Core keeps mini-snapshots every 32 heights and a 64-list cache with cleanup (`dmn:839-841`, `:904-919`, `:978-1017`). These are caches (§3.7). The two branches the pin adds for dual chainstates (`dmn:868-884`) are NG-6; the "initial snapshot" branch (`dmn:885-891`) is reached only for a list below DIP3 activation, which the node answers with the empty list by `resolve_list`'s DIP3 check (`dmn:833-835`).

### 3.6 Payments and value (H5; `ChainHelper` `NoCheck`)

CH3a already runs `IsBlockValueValid` and `IsBlockPayeeValid` (EV3's `block_value_valid`, `block_payee_valid`; `rs:connect/serial.rs:1040-1117`). Two arms are CH7a's:
- **The masternode payee** (`Ev3Branch::MasternodePayee` → H5). H5 follows Core's `IsTransactionValid` (`pay:228-261`), with `strict_multiplicity = rules.v24`, its log lines, and the hook's "ERROR! Invalid masternode payment detected" line (CH3 `rs:connect/evo.rs:223-229`). It is `masternode_payee_valid(coinbase, &prev_list, rules, &reward, strict, &p)` (`rs:dash-dml/src/payments.rs:193`) over the attempt's `resolve_list(prev)`.
- **The superblock arms** (`IsBlockValueValid` and `IsBlockPayeeValid`). Core reads `GetListAtChainTip()` (`pay:367`, `:424`): the list at the DMN tip (D16). That tip moves only on D16's updates (once per `ActivateBestChain` iteration that connected a block and, with a mempool, at the end of each step that disconnected one), and at startup it is unset until `InitializeCurrentBlockTip` (`init.cpp:2493`, `:2497`; §6.3 step 6), which runs after VerifyDB (`init.cpp:2130`) and after init's block import, its `ActivateBestChain` and the mempool load (`init.cpp:2482`; `node/blockstorage.cpp:966`, `:978`), unless that `ActivateBestChain`'s signal sets it first. So it is not the block's parent for the second and later blocks of a step (a reorg step, or a burst while synced; the update with a mempool comes at the step's end, after its connects, `v:3789`), and without a mempool, after a reorg's disconnects, it is the old fork's tip. CH7a passes V's current `dmn_tip` list (the list `ChainView.mn_list` publishes; empty while `dmn_tip` is `None`) to both arms on every origin: the tip path, TBV and VerifyDB. At startup VerifyDB and the first iteration of init's `ActivateBestChain` therefore see the empty list, as Core does; the gates (`sb_required` false while MN sync is unfinished, governance not valid) keep the arm from being reached. Superblock validity itself stays GV1b's (CH3a D63, G2); CH7a only supplies the list. The test (§7.2): a two-block reorg step whose second block is at a superblock height with `sb_required` true sees the old tip's list.

`ChainHelper` (`rs:connect/helper.rs:27`) answers `sb_required` (`ch:42-53`: false if the best CL height ≥ h or MN sync is not finished), `gov_valid`, `sb_triggered` and `sb_payments` from SP1/GV1b's back ends; until they land, `NodeHelper`'s answers stand (CH3 §3.3), which is Core's `NoCheck` path. CH7b adds `cl_best` (§4.5).

### 3.7 Caches (process state CH7a hosts)

Core's caches are process state. A1 §2.10.1 leaves unmodelled those "whose contents are pure functions of deterministic keys". The node keeps each one Core has where it changes cost, never where it changes an answer:

| Core cache | Core | Node | Answer-neutral because |
|---|---|---|---|
| `mnListsCache` (64, cleanup), `mnListDiffsCache`, mini-snapshots every 32 | `dmnh:777-778`, `dmn:839-841`, `:904-919` | `LruListCache` (`MAX_CACHED_LISTS = 64`, `rs:dash-dml/src/store/resolve.rs:81`), concurrent, set to the tip on each tip change | keyed by block hash; a list is a function of the chain. Core keeps a failed block's list in its cache (`dmn:754-756` runs before the roots); the node does not, and no reader can name a failed block's list (an invalid block is never a parent or a tip) |
| per-list SML cache, CbTx MN-root statics | `dmnh:175-182`, `sml:110-114` | `SmlRootCache`, one per chainstate (EV4-serve-notes:61) | the root is a function of the list |
| `creditPoolCache` (1000) | `cp:122-171` | an LRU over `PoolState` by hash | stored per block (D10) |
| `mnhfCache` (1000) | `mnhf:364-383` | an LRU over `EhfSignals` by hash | stored per block (D6) |
| `CheckCbTxBestChainlock` statics | `stm:169-171` | CH7b, §4.5.2 | **not** answer-neutral: BL-179 |
| quorum-processor caches, members caches, snapshot cache, quorum-object and scan caches | `llmq/cache.h:26-80`, `snap:295-298`, `lu:661-664` | CH7b, §4.4 | some are observable (§4.4) |

### 3.8 The effect channel: application and appliers

The attempt's log is the one ordered `Vec<Effect>` every call in the attempt writes into (`rs:dash-evo/src/effect.rs:34`; A1 §2.10.1; ADD §2.6). CH3 §3.2 fixes when it is applied: `finish_attempt` on every outcome, before the commit and before CH1-node acts on the outcome (D33, D75).

**D12. CH7a applies the log; CH7b owns the handle it releases.** `NodeEvo::finish_attempt` iterates the log in emission order and calls each effect's applier, then calls `MembersFacade::release_attempt(handle, EffectsApplied)` (ADD §2.6: "CH7a applies the log … then calls `release_attempt`"; DEC-113). `EffectsApplied` is minted only by this loop (its constructor is visible only inside `evo::effects`). The appliers for LLMQ effects are CH7b's code, registered with CH7a's applier (LQ1 §3.7's "CH7b applies" means this: the code is CH7b's, the loop is CH7a's).

```rust
/// One applier per EffectKind. `NodeEvo::new` takes all of them: a missing owner is a construction error, not a runtime refusal.
pub struct EffectAppliers {
    pub snapshot: Box<dyn SnapshotApplier>,        // CH7b: RuntimeRows::write (Core's section, ADD §2.6 O1)
    pub is_lock: Box<dyn IsLockApplier>,           // LK1b's IS store; `RemoveConflictingIsLock`
    pub mineable: Box<dyn MineableApplier>,        // CH7b: the MineablePool host (Erase/Reinsert)
    pub platform_ban: Box<dyn PlatformBanApplier>, // SP1's mncache meta store (BL-425)
    pub notify: Box<dyn NotifyApplier>,            // ChainEvent::MnListChanged on VerifyDB level 4 (A1 §2.10.1 row 6)
}
fn apply(&mut self, e: &Effect) -> Result<(), Fatal> {
    match e {                                       // exhaustive: a new variant is a compile error here
        Effect::StoreQuorumSnapshot { llmq_type, cycle_base, snapshot } => self.snapshot.write(*llmq_type, cycle_base, snapshot),
        Effect::RemoveConflictingIsLock { lock_hash } => self.is_lock.remove(lock_hash),
        Effect::EraseMineableCommitment { llmq_type, quorum_hash, commitment_hash } => self.mineable.erase(*llmq_type, quorum_hash, commitment_hash),
        Effect::ReinsertMineableCommitment { commitment } => self.mineable.reinsert(commitment),
        Effect::ResetPlatformBan { pro_tx_hash, height } => self.platform_ban.reset(pro_tx_hash, *height),
        Effect::NotifyMnListChanged { at, undo } => self.notify.notify(at, *undo),
    }
}
```

- **Durability** (CH2 `EffectLedger`, `rs:flush/packet.rs:38-52`). `StoreQuorumSnapshot` is **presumed**: its applier writes the row through the engine (a `Commit::Buffered` single-row batch, A1 §3.5.7) and calls `record_presumed()`, so the next B1 syncs the WAL first (`rs:flush/machine.rs:539-548`). The others are **independent**: their owners persist them as Core does (the IS DB directly; the mineable pool not at all; mncache on its own schedule). This is the rule `dash-dml/tests/consumers/ch7a.rs:56-64` already models (only snapshots presumed).
- **Failure.** An applier error is D5's store fault: `Fatal` via `abort_node`, before release, so the gate never opens on rows that were not written (ADD §2.6). CH3a D75 keeps the outcome's kind.
- **Owners that have not landed** (**Q5**). The node wiring must pass every applier. Until LK1b, `NodeHelper::is_conflict` never reports a conflict (CH3 §3.3), so `RemoveConflictingIsLock` is never emitted and the wiring passes `UnreachableIsLock`, whose `remove` is `Fatal` (as `NoEvo` refuses today, CH3a D61). Until SP1 merges, `ResetPlatformBan` (emitted by every ProUpServTx in a real connect, `dmn:737-752`) goes to `MemPlatformBans`, an in-memory set CH7a provides, which SP1 replaces with its store. The local DKG reads platform-ban state (`active/dkgsession.cpp:285`) only from DK1 on. It is also visible over RPC (`masternode/meta.cpp:50-51`: `is_platform_banned`, `platform_ban_height_updated`), so RP2a's metaInfo comparisons need SP1's store. SP1 starts from empty when it replaces the stand-in.
- **`NotifyMnListChanged`** is the effect A1 §2.10.1 lists for VerifyDB: level 3's `DisconnectBlock` fires the signal with `undo = true` (`v:5263`, `v:2391-2395`), and level 4's `ConnectBlock(fJustCheck = false)` with `undo = false` (`v:5310`, `v:2972-2976`). On the tip path H7 calls the subscribers synchronously inside the attempt, before `finish_attempt` and the commit (§5.2, D15). VerifyDB attempts (origin `VerifyDb`) emit the effect instead at the same point; as the attempt's last effect it is applied after the others, so the synchronous listeners (D15) see the snapshot rows Core had written before its signal. D15's listener rule applies to them too.

---

## 4. CH7b: LLMQ integration

### 4.1 Commitments in blocks (§3.2 step 6; LQ2's block half)

#### 4.1.1 Core's `CQuorumBlockProcessor::ProcessBlock` (`bp:223-306`)

1. DIP3 not active at pindex: write `q_bbu2`, return (`bp:229-232`; D11).
2. **`PreComputeQuorumMembers(pindex, reset_cache = false)`** (`bp:234`, `bp:35-43`), for `fJustCheck` too (§4.3).
3. `GetCommitmentsFromBlock` (`bp:608-648`): `bad-qc-payload`, `bad-qc-commitment-type`, `bad-qc-dup` (a duplicate non-rotation type), `bad-qc-premature`.
4. For each LLMQ type enabled at `pprev` (the loop is skipped when the chain has no tip, i.e. `ReplayBlocks`, `bp:245-249`): `GetNumCommitmentsRequired` (`bp:650-670`) against the count in the block: `bad-qc-not-allowed`, `bad-qc-missing`; rotation types log "[ProcessBlock] h[%d] numCommitmentsRequired[%d] numCommitmentsInNewBlock[%d]" (`bp:245-264`). This is the null-commitment rule: inside the mining phase, before the quorum's non-null commitment is mined, exactly the required number of (possibly null) commitments; after it is mined, none.
5. Per-block duplicate non-null `(type, quorumHash)` → `bad-qc-dup` (`bp:271-276`).
6. If `fBLSChecks`: each non-null commitment's signatures are verified on the BLS check queue, `bad-qc-invalid` on failure (`bp:278-295`).
7. `ProcessCommitment` per commitment (`bp:367-502`), with the failure log "[ProcessBlock] failed h[%d] llmqType[%d] version[%d] quorumIndex[%d] quorumHash[%s]" (`bp:296-301`): `bad-qc-block` (null or mismatched quorum base, `bp:393-408`), `bad-qc-invalid-null` (`bp:410-420`), `bad-qc-dup` against the stored commitment (`bp:422-434`), `bad-qc-height` (`bp:436-439`), `bad-qc-invalid` (`qc.Verify(checksigs = false)`, which calls `GetAllQuorumMembers`, `bp:449-456`); **`if (fJustCheck) return true`** (`bp:458-460`); then the mined rows (`bp:473-483`), `DropQcHashesCache` (`bp:488`), the erase from the mined-block cache and from the mineable pool (`bp:490-496`), and the LLMQ log "%s -- processed commitment from block. type=%d, quorumIndex=%d, quorumHash=%s" (`bp:498`).
8. `q_bbu2` (`bp:303`).

At `bp:282-287`, `bp:372-376` and `bp:441-446` Core returns `false` without setting the validation state. LQ2 returns `QcBlockError::NoReason` at the same three places (`rs:dash-dml/src/blockprocessor.rs:316`, `:434`), and the node maps it as D5 states: no failure mark, and the node shuts down.

#### 4.1.2 The Rust call

```rust
// dashd_chainstate::llmq::blocks; from CH7a-2 `cap` is over `NoMembers`, from CH7b-2 over the production facade
pub(crate) fn connect_commitments(cap: &AttemptHandle<'_>, cx: &QcBinding<'_>, block: &Block, at: QcBlockAt,
    just_check: bool, bls_checks: bool, delta: &mut EvoDelta, log: &mut Vec<Effect>)
    -> Result<Result<(), SpecialTxFailure>, Fatal>
{
    // step 2 (§4.3) runs inside process_block, through QcCtx.precompute (rs:dash-dml/src/blockprocessor.rs:367)
    let mode = if just_check { QcMode::JustCheck } else { QcMode::Connect { bls_checks } };
    match dash_dml::blockprocessor::process_block(&cx.ctx(cap), block, at, cx.tip, mode, log) {  // steps 2–8
        Ok(records) => { delta.mined_commitments = records; Ok(Ok(())) }
        Err(QcBlockError::Invalid(e)) => Ok(Err(SpecialTxFailure::Invalid(e))),
        Err(QcBlockError::Fault(e)) if e.is_not_built() => Ok(Err(SpecialTxFailure::NotReady(e.capability()))), // D2
        Err(QcBlockError::NoReason(what)) => Err(Fatal::new(what)),            // D5 (DEC-518)
        Err(e @ (QcBlockError::BlsUnsupported | QcBlockError::Fault(_))) => Err(Fatal::abort_node(e)), // D5
    }
}
```

- **`QcBinding`** binds LQ2's `QcCtx` (`rs:dash-dml/src/blockprocessor.rs:60`) to the attempt: the `LlmqCtx` (the attempt's chain view, deployments and `BlsCtx`, §4.2.3), the attempt's `BlockLookup`, the mined-commitment reader over the attempt's read view (the layer plus this attempt's delta, so Core's `GetMinedCommitmentsUntilBlock` over the open transaction, `bp:731-773`, is reproduced), the facade as `&DmlMemberSource`, and the BLS check runner.
- **`tip`.** `Some(prev)` on the tip path, in TBV and in VerifyDB (each runs with a chain whose tip is the parent: `v:5083`, and VerifyDB's disconnect-then-reconnect at level 4); `None` only in `RollforwardBlock` replays, where Core's chain has no tip (`bp:245-249`) and also passes `fCheckCbTxMerkleRoots = false` (`v:5330-5370`). LQ2 D5's fail-stop on a non-null commitment in a replay above height 0 stays.
- **The BLS check runner** is `dashd_chainstate::checkqueue::BlsCheckQueue` (`rs:checkqueue/bls.rs`, unused today; LQ2 D11), sized as Core's `m_bls_queue` (`-parbls`; `bp:92-102`). It runs on its worker threads with the commitment data only; it never takes the attempt handle (ADD §2.6: work handed to other threads never takes it).
- **The mined rows** go into the delta (`EvoDelta.mined_commitments`), Core's transaction (**Q1**).
- **Effects.** `EraseMineableCommitment { llmq_type, quorum_hash, commitment_hash }` for each processed non-null commitment (`bp:490-496`), emitted only when `!just_check` (it follows `bp:458-460`), and the qc-hash cache drop (§4.4).

#### 4.1.3 Eager BLS decode and the null rules

Commitments are decoded eagerly with the mining block's scheme (LQ2; A1 §1.4.1; SB8 D3): `QcCtx.bls` is the attempt's `BlsCtx` (D9). `IsNull ⇔ !IsValid` and `VerifyNull` are LQ2's. Nothing here is new; CH7b only supplies the context.

#### 4.1.4 Undo (`bp:562-606`)

1. **`PreComputeQuorumMembers(pindex, reset_cache = true)`** (`bp:566`): §4.3's loop in `Undo` mode, which clears the type's two members caches and recomputes (re-storing the row as an effect).
2. `GetCommitmentsFromBlock` with a dummy state; a failure fails the disconnect (`bp:569`).
3. Per non-null commitment (`bp:573-601`): erase the mined row and its index row (with one chainstate, Core's `EraseMinedCommitmentIfUnreferenced` always erases, `bp:63-79`); drop the qc-hash caches; erase the mined-block cache entry; emit `ReinsertMineableCommitment { commitment }` (Core's `AddMineableCommitment`, `bp:884-911`).
4. `q_bbu2 = pprev` (D11).

Under **Q1** (DEC-517) the erase is a delta operation. Each erase is a full tombstone, built by H9 when it reads the commitment in step 2, so applying it never needs to read the old row:

```rust
pub struct MinedErase {
    pub llmq_type: LlmqType,
    pub quorum_hash: QuorumHash,
    pub mined_height: u32,               // pindex->nHeight: the disconnected block's height
    pub rotation_index: Option<i16>,     // Some(qc.quorumIndex) iff IsQuorumRotationEnabled(params, pindex), bp:579-590
    pub mined_key: Vec<u8>,              // llmq_mined: u8 type ‖ quorumHash (§6.1)
    pub inverse_key: Vec<u8>,            // llmq_mined_h: the indexed or plain family, by rotation_index (§6.1)
}
```

- `rotation_index` decides the family as `MinedCommitmentRecord.rotation_index` does when the row is written (`rs:dash-dml/src/store/delta.rs:76-83`), and the codec encodes both keys from these fields. So the tombstone names exactly the rows the connect wrote. `rotation_index` uses the mining block, as Core's undo does (`bp:586`), while the write uses the base (`bp:462`); the two always agree, because a commitment is mined in its base's cycle (`bp:379`, `:436`).
- H9's disconnect delta carries `erased_mined: Vec<MinedErase>` in `commitments_from_block`'s order. The layer applies it above the rows of the disconnected block's own delta, and B1 writes it as two deletes per tombstone (§6.1).
- LQ2's `undo_block` returns the tombstones; today it writes nothing (an LQ2 erratum, §11.2).
- An undone block's delta is not "popped": Core commits the disconnect as its own transaction, and so does the node.
- The test: a rotated commitment is connected, disconnected and reconnected (the same block, then a competing block mining it at another height), once with a flush between disconnect and reconnect and once without one (the tombstone then sits in the layer over a row already in RocksDB, which exercises `bad-qc-dup` and the `WriteDerived` identity check through tombstones, `bp:422-434`, `:473-476`). After each step `mined_until`, `nth_mined_by_index` and `quorum_root` at the tip and at the earlier heights equal Core's, and no `llmq_mined_h` row survives its disconnect.

### 4.2 The members facade, the attempt handle and the gate (ADD §2.6)

#### 4.2.1 Types

ADD §2.6's code block is the specification. CH7b's concrete types add only fields the addendum leaves as `/* … */`:

```rust
pub struct MembersFacade {
    gate: RwLock<()>,                           // writer-preferring (parking_lot); exclusive for an attempt (ADD §2.6)
    caches: Mutex<MembersCaches>,               // the facade's cache mutex (ADD §2.6 lock order, third)
    rows: RuntimeRows,                          // llmq_snapshot CF + the snapshot cache (§4.4); its lock is the fourth
    quorums: Mutex<QuorumCaches>,               // the quorum-object and scan caches (§4.4); taken only under the gate
    mineable: Mutex<MineablePool>,              // §4.4; Q5
    cbtx_cl: Mutex<CbTxClCell>,                 // Core's CheckCbTxBestChainlock statics (§4.5.2)
    params: Arc<ChainParams>,
    next_attempt: AtomicU64,
}
struct MembersCaches {
    by_base: PerTypeLru<BlockHash, QuorumMembers<Arc<Dmn>>>,         // Core's mapQuorumMembers (lu:661-662)
    by_cycle_index: PerTypeLru<(BlockHash, u32), QuorumMembers<Arc<Dmn>>>, // mapIndexedQuorumMembers (lu:663-664)
}
```

- **`PerTypeLru`** is Core's `PerLlmqTypeCache` (`llmq/cache.h:26-80`): one `unordered_lru_cache` per LLMQ type the chain registers, created on first use (`lu:679-681`, `:695-697`), each sized `keepOldConnections` (the default `Init`, `cache.h:45-50`), with `unordered_lru_cache`'s truncation and access counter (LQ1 §3.7.4). A type the chain does not register has no cache: reads miss and writes are dropped (`cache.h:20-24`).
- **`AttemptHandle<'v>`** is ADD §2.6's, plus the attempt's binding (§4.2.3): `{ facade, id, _gate: RwLockWriteGuard, _v_only, overlay: RefCell<BTreeMap<(LlmqType, BlockHash), QuorumSnapshot>>, bind: AttemptBinding<'v> }`.
- **`begin_attempt(&'v self, v: &'v mut VContext, id, bind) -> AttemptHandle<'v>`** takes the gate exclusively (waits for in-flight external routes); `release_attempt(h, EffectsApplied)` drops the overlay and the gate. A handle dropped without release trips the latch first (ADD §2.6; CH3a D71's guard covers the hooks' side).

#### 4.2.2 The members call (`GetAllQuorumMembers`, `lu:653-761`)

`impl QuorumMemberSource for AttemptHandle<'_>` (and for `OnV<'_>`, §4.6) runs Core's function exactly:
1. **Gate:** `members_gate` (genesis or type not enabled at the base's parent → `{}`, `lu:670-674`).
2. **Base cache:** with `reset` the type's base cache is cleared (`lu:682-683`); else a hit returns (`lu:684-686`).
3. **Rotated route** (`IsQuorumRotationEnabled(base)`, `lu:694`): `quorumIndex = h % dkgInterval`; `≥ signingActiveQuorumCount` → `{}` (`lu:707-711`); `reset` clears the indexed cache (`lu:720-722`); an indexed hit is copied into the base cache and returned (`lu:723-729`); a miss computes `compute_by_quarter_rotation` with `predicting = false` over `AttemptRows` (overlay first, then backing), which stores the cycle's row (`lu:463` → `snap:322-338`) as an overlay write plus an `Effect::StoreQuorumSnapshot`, and inserts **every** index of the cycle into the indexed cache (`lu:731-744`).
4. **Non-rotated route:** the work block is `base − 8` when V20 is active after the base, else the base (`lu:745-754`); `compute_quorum_members`.
5. Insert into the base cache (`lu:757-758`).

The order of steps 3's store and 3–5's cache inserts is Core's: the row is written (to the overlay) before any cache entry is published, which is what DEC-113's serialization protects (ADD §2.6).

#### 4.2.3 Binding the call to its attempt (the members trait has no context argument: EV2:517, LQ1:478)

`QuorumMemberSource::members(&self, t, base, effects)` takes no chain or BLS context (`rs:dash-evo/src/llmq/members.rs:32`). The context is the handle's:

```rust
pub struct AttemptBinding<'v> {
    pub chain: &'v dyn ChainAncestry,               // the attempt's header chain (the block's ancestry, also for a proposal's dummy index)
    pub deployments: &'v dyn DeploymentView,        // as the connector's ConnectCx
    pub bls: BlsCtx,                                // H1's (D9)
    pub evo: EvoReadView,                           // the layer plus this attempt's delta (lists, coinbase facts, mined rows)
}
```

`members` builds `LlmqCtx::new(params, &CompleteChain::new(bind.chain), bind.deployments, bind.bls)` and `NodeLlmqSource { view: &bind.evo }` per call. An `OnV` binds the same fields to the tip at dispatch time, with `ChainView.bls` (§4.6). One handle is bound to one attempt, so the context can never come from another block (LQ1's "`for_block(h)` in validation, `for_tip` elsewhere", LQ1:257).

#### 4.2.4 Error mapping (ADD §2.3)

The facade's `type Error = StoreError`. `MemberError::Missing(at)` and `View(e)` become `NodeMemberError::Store(…)` with Core's text "Found EvoDB inconsistency, you must reindex" for `Missing` (`rs:dash-dml/src/store/mod.rs:190`), then `Fatal::abort_node` (D5). `StoreError::NotBuilt` is the one exception: it is D2's readiness refusal, never a fault. `UnknownLlmqType(t)` is `failed-procspectxsinblock` via `DmlException::Members` (EV2 §2.11), unreachable after EV1/LQ2's type checks. The §2.4 node test (ADD §2.4): an injected absence through the production facade trips the latch and calls `abort_node` once, emits no P2P message, and marks no block invalid.

#### 4.2.5 `NodeLlmqSource` (`LlmqSource<MnList>`)

- `mn_list(at)`: `resolve_list(at, …)` over the bound read view; `Ok(None)` never happens for a connected block (the node holds every list it has connected), so a `None` is `Missing` and fatal (ADD §2.1: on the node "not held" cannot occur).
- `coinbase_facts(at)`: the work block's coinbase, read from the block files. A block-data-unavailable read is a normal-absence **value** (`cbtx:181-198`; the hash-form modifier, `lu:90`; LQ1 §3.7.3), never an error; ADD §4.3's `NotComparable` depends on it. Any other read error is a store fault.

### 4.3 The precompute loop (`bp:35-43`; ADD §2.5)

`impl QuorumPrecompute for AttemptHandle<'_>` is ADD §2.5's loop, line for line:

```rust
fn precompute(&self, block: BlockId, mode: PrecomputeMode, effects: &mut dyn EffectSink) -> Result<(), MemberError<StoreError>> {
    let ctx = self.llmq_ctx();
    for p in enabled_quorum_params(&ctx, prev_of(block)?)? {              // GetEnabledQuorumParams(pprev), eager
        if is_quorum_rotation_enabled(&ctx, p, block)? && height(block) % p.dkg_interval == 0 {
            self.members_with(p.ty, block, mode == PrecomputeMode::Undo, effects)?; // GetAllQuorumMembers(…, reset); discarded
        }
    }
    Ok(())
}
```

It runs at connect step 2 of §4.1.1 (`reset = false`) for every attempt, including `just_check`, TBV and VerifyDB, and at undo step 1 (`reset = true`). For a proposal, `block` is the dummy index's id: the attempt's chain view holds the proposal block's header at `prev + 1` for exactly this (CH3a's TBV attempt already builds it). Core keys the stored row by the dummy index's block hash, i.e. the template's hash (A1 §2.10.1 row 1: "one row per distinct template hash at a cycle-base height"); the node does the same.

### 4.4 Caches CH7b hosts, and which are observable

| Cache | Core | Node | Invalidation (Core's) | Observable? |
|---|---|---|---|---|
| members by base / by (cycle base, index) | `lu:661-664` | `MembersCaches` | only `reset_cache` (undo's precompute) clears the type (`lu:682-683`, `:720-722`); never on a failed connect | **yes**: a hit returns the cached roster after the work block's data is unavailable (ADD §4.3) |
| quorum snapshot cache (LRU 32) + `llmq_S` rows | `snap:290-338` | `RuntimeRows` (`Mutex<Lru32>` + the CF) | never | through interleavings only (ADD §2.6 case 7) |
| mined-block cache (positive only) | `bp:694-717`, `blockprocessor.h:68` | `QuorumCaches.mined_block` | erased per commitment in `ProcessCommitment` and undo | no (a positive hit is re-checked against the chain, `bp:694-717`) |
| qc-hash caches | `bp:504-560` | `QcHashCache` (`rs:dash-dml/src/blockprocessor.rs:551`), one per chainstate (LQ2 D22) | dropped whole by every processed commitment and undo | no; but dropped only when `!just_check` (LQ2 D22), as Core |
| mineable pool | `bp:884-911`, `blockprocessor.h:63-64` | `MineablePool` (`rs:…/blockprocessor/pool.rs:36`) | P2P intake adds; connect erases; undo re-adds | **yes**: GBT's commitments (MN1) and `qfcommit` relay |
| quorum objects, `ScanQuorums` cache | `quorumsman.h:88-89`, `quorumsman.cpp:59-105`, `:170-297` | `QuorumCaches.{objects, scans}` (DEC-60) | not touched by connect/undo; `GetQuorum` re-checks `HasQuorum` (`quorumsman.cpp:533-545`) | no |
| CbTx ChainLock statics | `stm:169-171` | `CbTxClCell` (§4.5.2) | written in the attempt at step 10 on its success paths, outside the evo transaction, also in attempts that fail later or are check-only; never on step 10's rejects or on undo | **yes** (BL-179) |

**D13. CH7b's quorum object is validation's quorum: commitment, members and mined block.** Core's `BuildQuorumFromCommitment` also writes contributions and queues warming (`quorumsman.cpp:72-105`, `WriteContributions` at `:88`). That is QM1a's (vvec and sk-share store) and has no effect on any validation answer; CH7b's materializer has a hook QM1a fills.

**D14. The mineable pool lives in the facade** (**Q5**). Core keeps it in the quorum block processor, beside the mined index. Validation erases from it (`bp:490-496`) and re-adds to it (`bp:600`) as effects; `qfcommit` intake adds to it (LQ2's `qfcommit_verdict`, route B); GBT reads it (MN1, route B). It is in-memory only, as in Core.

### 4.5 ChainLocks, InstantSend and the `ChainHelper` plumbing

#### 4.5.1 What is already there

CH3a/CH3b already ask the helper Core's per-block questions: `cl_conflict` (`v:2568-2571`, TBV `v:5086-5089`), `cl_has` and `is_conflict`/`remove_conflicting_is_lock` (`v:2852-2872`), `sb_required` (`ch:42-53`). CH3b runs CL enforcement's chainstate half: `EnforceBlock`/`MarkConflictingBlock` (`v:4164-4274`) as `ChainCommand::ClEnforce` and the `BLOCK_CONFLICT_CHAINLOCK` candidate rules (`v:3647-3689`, `:4381-4383`). Header acceptance's `bad-chainlock`/`bad-prevblk-chainlock`/`duplicate` (`v:4756-4848`) is CH1-node's. The ChainLock **handler** (`clh:69-362`: the 5 s schedule, `ProcessNewChainLock`, `ProcessCoinbaseChainLock`, `EnforceBestChainLock`) is LK1a's; the IS store is LK1b's.

#### 4.5.2 What CH7b adds

- **`ChainHelper::cl_best(&mut self) -> Option<BestChainLock>`**, Core's `GetBestChainLock()` (`stm:174`; `cl:37-41`), recorded like the other queries (O8 §7.1's `QueryBestChainLock`, which the pin's hook taps at `stm:175`). `NodeHelper` answers from CH1-node's `ClState` (LK1a feeds it; none before LK1a).
- **`check_cbtx_best_chainlock`** (`stm:156-239`), Core's function with its statics: `CbTxClCell { block: Option<BlockId>, cl: Option<(BlsSig, u32)> }`, process-wide in the facade, updated exactly where Core updates `cached_pindex`/`cached_chainlock` (the shortcut at `stm:176-183`, the success paths at `:211-217` and after verify, `:230-232`). It is written in the attempt at step 10, outside the evo transaction, as Core's statics are. So Core's updates in attempts that fail later (a step-11 reject), in check-only attempts (TBV) and in VerifyDB level 4 happen in the node too; only step 10's own rejects and undo leave it untouched (BL-179: always verifying would reject a block Core accepts). Its content is observable when the parent's block data is unavailable, because `GetNonNullCoinbaseChainlock` then reads `nullopt` (`cbtx:181-198`). The test: step 10 succeeds and step 11 rejects, then a sibling block's step 10 finds the cell set. Before CH7b-3 the cell is never written: the shortcut needs a best ChainLock (none before LK1a) and the verify is D2's refusal, so every read misses, which is Core's state while `cached_pindex` is unset. A proposal overwrites the cell with an entry that matches no block, because its block has no `BlockId`: Core's cache holds the address of TBV's stack-local dummy index (`v:5093-5096`), which no real `pprev` can equal. The verify reaches step 10 through the `ClVerifier` seam (D2): CH7b-3's `AttemptClVerifier` runs `dash_evo::locks::verify_chainlock` over the attempt's scan (route C, `ScanQuorums(..., chain)`), with the attempt's `BlsCtx`, and replaces CH7a-2's `NotBuiltClVerifier`. `GetNonNullCoinbaseChainlock(pprev)` reads the parent block (`cbtx:181-198`); a failed read is Core's `nullopt`.
- **IS plumbing.** The removed-view and chained-children rule is CH3's (§3.3); CH7b adds only the applier slot (§3.8) and the recorded decision for `is_rm` (O8 §6, CH3a D135).

#### 4.5.3 `ReplayChainHelper`

`RecordedHelper` (CH3 §3.3) grows into `ReplayChainHelper`: it answers `cl_best` and every other query from O9a's `RecordedDecisions` for **every** attempt the recording holds, failed and proposal attempts included (A1 §2.10.1, GPT F15). An attempt's queries are keyed by its O8 attempt sequence, so a failed attempt's answers never leak into the next. A decision miss stops the replay (O8 §7.2).

### 4.6 V dispatch and the capability types

- **`VContext::on_v(&mut self, &MembersFacade) -> OnV<'_>`** is added in V's loop module (`rs:chain/actor.rs`, where `VContext` is constructed, `:173-179`). `OnV` takes the shared gate as its first act (ADD §2.6 rev4).
- **`ChainCommand::OnV(OnVCommand)`** replaces the `#[cfg(test)] TestOnV` variant (`rs:chain/command.rs:140`):

  ```rust
  pub struct OnVCommand { run: Box<dyn for<'v> FnOnce(&OnV<'v>, &ChainQuery<'_>) + Send> }  // the reply channel is captured by the closure
  ```

  V runs the closure synchronously between attempts, with the `OnV` it mints for the call, and drops the `OnV` when the closure returns (ADD §2.6's "V command boundary"). The closure cannot return the capability: `for<'v>` makes it unnameable outside. Dispatching from V is a debug assertion. The queue is CH1-node's FIFO lane (`COMMAND_CAPACITY`, `rs:chain/actor.rs:187`).
- **`OnValidation`** (sealed): implemented for `OnV<'_>` and `AttemptHandle<'_>` only. Every runtime `QuorumMemberSource`, `QuorumPrecompute`, `QuorumMaterializer`, writable `SnapshotRows` and composite builder (scans, `GetQuorum`, the predictor, qrinfo, rotationinfo, mnlistdiff serving, mnxcheck) is constructed from `&C where C: OnValidation`.
- **`RawRows`** (`ExternalToken::rows()`): exactly `snapshot_for_block` and the `MinedCommitmentReader` getters, with Core's snapshot-cache section (ADD §2.6 allow-list).
- **VerifyDB.** `ChainCommand::VerifyLoaded` (startup) and CH5's `verifychain` path both run VerifyDB on V; levels 3 and 4 open attempts with origin `VerifyDb` and a scratch `EvoTxn` (§2.1) that is never committed (Core: `v:5197`; CH3a D36). The ADD erratum (`ChainCommand::VerifyDb`) is satisfied by these two.
- **Compile-fail suite** (trybuild, BL-65): ADD §2.6's six cases plus CH3a a2's `VContext` case.

### 4.7 The read-only surface for mnxcheck (ADD §4.3; SB19)

CH7b provides the primitives; SB19 composes the comparison.

```rust
impl MembersFacade {
    /// Core's lookup order without its side effects: the base cache, then (rotated) the indexed cache.
    /// A const lookup: no access-counter bump, no insert, no copy into the base cache (ADD §4.3 (2)).
    pub fn peek(&self, cap: &OnV<'_>, t: LlmqType, base: BlockId) -> Result<Peek, StoreError>;
}
pub enum Peek { Gated /* members_gate false or RotatedIndexOutOfRange */, Hit(QuorumMembers<Arc<Dmn>>), Miss }
/// The miss path's inputs (ADD §4.3 (3)): overlay-first rows whose backing reads `llmq_snapshot` bytes directly,
/// never the snapshot cache; writes stay in the overlay.
pub struct ShadowRows<'c> { cap: &'c OnV<'c>, overlay: BTreeMap<(LlmqType, BlockHash), QuorumSnapshot> }
/// A read-only LlmqSource that records whether any work-block read was BlockDataUnavailable (→ NotComparable).
pub struct ShadowSource<'c> { cap: &'c OnV<'c>, unavailable_read: Cell<bool> }
pub struct NoMemo;                                   // never the runtime SortedListMemo
```

`PerTypeLru` gets a `peek(&self, key) -> Option<&V>` that reads without touching the access counter (Core's `unordered_lru_cache::get` and `exists` both bump it, `unordered_lru_cache.h:61-76`). The acceptance test (PLAN: "`peek` leaves cache state and access order unchanged") compares both caches' full state, including counters, before and after.

---

## 5. The node's live `MnList` (replacing `NoEvo`)

SB19 (mnxcheck), AM1 (the active masternode), GV1b (governance), RP2a (`protx list/info`), MP1a (special-tx mempool checks), MN1 (templates) and QM1a all need the masternode list. Today the node has none: `NoEvo` keeps no state, and only `dash-coreimport` builds a list from Core's rows (`rs:dash-coreimport/src/evodb.rs:92`). CH7a gives the node three read paths, all fed by V.

### 5.1 The tip list in `ChainView`

`ChainView` (`rs:dashd-chainstate/src/chain/view.rs:71`; "`mn_list` and `ConnChainView::mn_list_at_tip` come with CH7a (D40)", `:6`) gains:

```rust
pub struct ChainView {
    /* existing: snap (SB4's ChainReadSnapshot, incl. bls), cl, mtp, tip_chain_tx, large_work_invalid_chain, view_seq */
    pub dmn_tip: Option<BlockId>,         // Core's CDeterministicMNManager::tipIndex (D16); None until the startup InitializeCurrentBlockTip (§6.3 step 6)
    pub mn_list: Arc<MnList>,             // Core's GetListAtChainTip(): the list at dmn_tip; empty while dmn_tip is None (D16)
    pub credit_pool: Option<Arc<PoolState>>, // GetCreditPool(Tip()) for the mempool (v:936-950); None below V20
}
```

- V stores them with the tip, in the same `ChainView` store that publishes the tip (CH1-node §6), so a reader never sees a tip with another tip's list.
- **`mn_list` has exactly one definition:** the list at `dmn_tip`, D16's lagging DMN tip, not the chain tip. Callers of Core's `GetListAtChainTip()` read it (the superblock arms §3.6, GV1b, AM1). Callers that read the list at the chain tip (`GetListForBlock(Tip())`, Core's mempool checks) use `EvoReadView` at the tip instead (§5.4).
- **`bls`** is already in SB4's snapshot. CH7a adds a test that it equals the last committed attempt's `post_scheme` (D9) at every tip, including across V19 in both directions.
- `MnList` is an `imbl` structure: publishing it is an `Arc` clone and shares every node with the previous list (EV2a).

**D16. `ChainView.mn_list` is the list at Core's DMN tip, which moves on Core's signal and, with a mempool, on Core's reorg update.** Core's `GetListAtChainTip()` is `GetListForBlockInternal(tipIndex)`, or the empty list while `tipIndex` is null (`dmn:944-951`). `tipIndex` is set only by `CDeterministicMNManager::UpdatedBlockTip` (`dmn:822-827`), which two callers reach:
- **`CDSNotificationInterface::SynchronousUpdatedBlockTip`**, when `pindexNew != pindexFork` (`dsnotificationinterface.cpp:47-53`). That signal fires:
  - from `ActivateBestChain` whenever its own condition holds: an iteration that connected a block (`blocks_connected`, `v:3904`) and ends with `FindFork(starting_tip) != pindexNewTip` (`v:3926-3937`). The outer loop (`v:3869-3958`) signals after its inner do-while, which can run several `ActivateBestChainStep`s, so it is not once per step and not once per block. An iteration that reconnects to its starting tip (one step disconnects and meets an invalid block, a later one reconnects the old chain, `v:3927`) signals nothing;
  - from `InvalidateBlock` (`v:4152`) and `MarkConflictingBlock` (`v:4265`), with the new tip;
  - at startup, from `InitializeCurrentBlockTip`, which calls `SynchronousUpdatedBlockTip(tip, nullptr, ibd)` (`dsnotificationinterface.cpp:31-35`), so with a tip it always sets. Init calls it on both branches (`init.cpp:2493`, `:2497`), after `VerifyLoadedChainstate` (`init.cpp:2130`).
- **`MaybeUpdateMempoolForReorg(disconnectpool, fAddToMempool)`**, through `CSpecialTxProcessor::UpdatedMNListTip(m_chain.Tip())` (`stm:879`), when the chainstate has a mempool and `fAddToMempool` is true (`v:426`, `v:433-434`; rc.4's CE4-17, absent at rc.3). Those calls are at the end of every `ActivateBestChainStep` that disconnected a block, after the step's connects and also in a step that only disconnects (`v:3789`); on `InvalidateBlock`'s first 10 disconnects (`v:4081`); and in `MarkConflictingBlock` (`v:4250`). The add=false calls (`v:3728`, `:3772`, `:4228`) do not move it.

Without a mempool, which is the oracle's configuration, a step that only disconnects leaves `tipIndex` at the old block, and the old block's list is still served (its rows and cache entry survive, D6). With a mempool, as on every real dashd, the step's end moves it to the new tip before the disconnected transactions are re-added. The node keeps a `dmn_tip` with exactly these updates, written only by V:
- **The signals.** V sets it, as an internal step before the subscribers run, in its two dispatchers of Core's tip signals, so every subscriber sees the new list:
  - `Signals::sync_updated_block_tip` (`rs:chain/signals.rs:521-522`; called at `rs:chain/abc.rs:324` and at CH3b's invalidate and conflict sites), under Core's guard `new != fork`;
  - `Signals::initialize_current_block_tip` (`rs:chain/signals.rs:525-526`), where the fork is `None`, so it sets whenever there is a tip. Nothing calls it at the base; CH7a-3 adds the startup command that does (§6.3 step 6).
- **The reorg update.** All three drain sites (`rs:chain/abc.rs:529`, `rs:chain/failure.rs:51`, `rs:chain/locks.rs:463`) call one V wrapper, `reorg_drain(pool, add)`. When the call's `add` argument is true (at `failure.rs:51` it is `*disconnected <= 10 && ret`, Core's `v:4081`: not on the 11th disconnect, nor after a failed `DisconnectTip`) and the sink has a mempool, the wrapper moves `dmn_tip` to the chain tip, publishes a `ChainView` with the new list, and only then calls the sink. The `MempoolSink` seam (`rs:chain/seams.rs:903`) gains `fn has_mempool(&self) -> bool`, false for `NullMempool` (`:928`): Core's `if (!m_mempool) return;`. The sink gets no evo handle, so `dmn_tip` keeps one writer and the move precedes the re-add, as in Core. Until MP1 the node is a Core without a mempool, which `NullMempool` models correctly; MP1b's obligation is in §11.2.
- **What the sink reads.** Core's mempool reads `GetListAtChainTip()` inside these calls, while `tipIndex` lags the chain tip: the re-add's `addUncheckedProTx` (`txm:647`, `:660`, `:668`, `:699`), and `removeForBlock`'s ProTx conflict removal (`txm:1321-1344` → `:1069`, `:1250`), which `ConnectTip` calls inside a step (`v:3572`). So the sink calls V makes inside a step pass V's current `dmn_tip` list: `maybe_update_for_reorg(pool, add, tip_list: &Arc<MnList>)` and `remove_for_block(vtx, height, tip_list)` (`rs:chain/abc.rs:671`). The argument is read from V's state at the call, so a move earlier in the same step is visible, as under `cs_main`.
- **Publication.** V publishes a `ChainView` whenever `dmn_tip` changes, before the step or command that moved it ends. A disconnect-only step with a mempool moves `dmn_tip` at its drain, after its last commit's publication, and no signal follows (`pindexFork == pindexNewTip`, `v:3926-3937`); `reorg_drain`'s publication carries that move, so `ChainView.mn_list` never trails V's `dmn_tip` once the step ends.

`ChainView.mn_list` is the list at `dmn_tip`: H7's `new` when that is the block, otherwise `resolve_list(dmn_tip)` (an LRU hit in practice). Below DIP3 the list is empty at the block's hash (`dmn:833-835`). At startup `dmn_tip` is `None` until the first of two events: the signal of an init `ActivateBestChain` iteration that connects a block, or the startup `InitializeCurrentBlockTip` command, which comes after init's block import, its `ActivateBestChain` and (with MP1) the mempool load (§6.3 step 6). Until then every reader gets the empty list, as in Core: startup VerifyDB, the superblock arms of init's first `ActivateBestChain` iteration, and the mempool load's ProTx checks (`txm:647-699`). The test pins both configurations. With `NullMempool`, a disconnect-only step leaves `ChainView.mn_list` on the old tip's list (the oracle's answer). With a test sink whose `has_mempool` is true, the same step moves `dmn_tip` to the new tip, as do `InvalidateBlock`'s first 10 disconnects (not the 11th) and `MarkConflictingBlock`; the sink records that `dmn_tip` had already moved when its add=true drain ran and that its `tip_list` argument is the moved list, and after the step `ChainView.mn_list` is the new tip's list (a real dashd's answer, not the oracle's). A multi-step iteration signals once, and one that reconnects to its starting tip signals nothing. A restart with no new block: startup VerifyDB sees the empty list, and after the command `ChainView.mn_list` equals the tip's list. A restart with blocks stored above the loaded tip: the superblock arms of init's first `ActivateBestChain` iteration get the empty list, and after the command `ChainView.mn_list` is the post-`ActivateBestChain` tip's list.

### 5.2 `MnListChanged`

**D15. The MN-list signal is synchronous and fires under Core's condition.** Core fires `NotifyMasternodeListChanged(undo, old_list, diff)` after a full connect (`v:2972-2976`) or disconnect (`v:2391-2395`), only for the active chainstate and only if the diff has changes (EV2:999's erratum), and the signal runs every subscriber synchronously (`validationinterface.cpp:349`). `uiInterface.NotifyMasternodeListChanged` is the GUI's.
- `ValidationInterface` (`rs:chain/signals.rs:41`) gains `fn notify_masternode_list_changed(&self, undo: bool, old: &Arc<MnList>, diff: &Arc<MnListDiff>) {}`.
- H7 (`list_changed(updates, undo)`) calls it synchronously **inside the attempt**, at Core's position (CH3a D47; CH3 §2.13). On a connect it runs after H6's staged best-block write and the H1 commit (`rs:connect/serial.rs:1145-1147`); on a disconnect after H6 (`rs:connect/disconnect.rs:194-197`). In both it runs before `finish_attempt`, the coins-view commit (`ov.commit`, Core's `view.Flush`), `commit_tx` (Core's evodb commit) and the tip publication (`rs:connect/connector.rs:265-346`, `:399-453`). So subscribers see Core's state: on a connect the coins view is still the parent's, which `govman.CheckAndRemove` reads, and the tip has not moved.
- **Who listens synchronously.** Only Core-equivalent listeners: the `CMNAuth::NotifyMasternodeListChanged` equivalent and GV1b's `govman.CheckAndRemove`, which together are Core's only listener, `CDSNotificationInterface::NotifyMasternodeListChanged` (`dsnotificationinterface.cpp:90-96`). AM1, DK1, QM1a and SB19 consume the asynchronous `ChainEvent::MnListChanged`, which V publishes with the tip after the commit (`NodeEvo.publish`, §5.1).
- **What a synchronous listener may do.** It runs on V, inside an attempt that holds the exclusive gate. It must not dispatch to V (`OnV` or any `ChainCommand`), take the gate, or wait on V or on any actor's reply (CH1-node LO-5, LO-9). Everything it needs comes as the callback's arguments (`undo`, `old`, `diff`). It must not read an effect target for this block (rotation snapshot rows, the mineable pool, the IS store, platform bans), because `finish_attempt` applies the effect log after H7; Core's listener reads none of them.
- **No CH3b change.** H7 sits at CH3a's sites (`rs:connect/disconnect.rs:194-197`, inside `DisconnectBlock`, before `DisconnectTip`'s pool add at `v:3412-3418`), so every CH3b disconnect path reaches it once G1 lifts: ABC, `InvalidateBlock`'s per-iteration pools and `EnforceBlock`'s marks. CH3b's suites run on `NoEvo` regtest, so CH7a-4's acceptance reruns CH3's reorg and ChainLock suites on a DIP3-active regtest chain (§8.1, §9.1).
- **Registration.** Synchronous listeners register after startup VerifyDB, as Core registers `CDSNotificationInterface` (`init.cpp:2228`) after `VerifyLoadedChainstate` (`init.cpp:2130`). So startup VerifyDB's notifications reach no listener, and a later `verifychain` reaches them.
- After H7 the attempt can fail only with a `Fatal` (an applier error or a store fault at the commit, D5), so a subscriber never sees a signal for a block the node then rejects while it keeps running.
- Under `just_check` there is no H7 (Core returns at `v:2936-2937`). VerifyDB's attempts emit `Effect::NotifyMnListChanged`, applied by the same call (§3.8): Core fires it from VerifyDB too (`v:5263`, `v:5310`).

### 5.3 Historical reads: `EvoReadView` and the dispatched routes

- **D17. Lists, pools, MNHF signals and mined commitments at a block** come from `EvoReadView { at, layers, snapshot }` (A1 §2.10.2), obtained through a `ChainCommand::Query` (`ChainQuery` gains `evo: &EvoLayer` and `fn evo_view(&self, at: BlockId) -> EvoReadView`). The view pins `at`, O(1) clones of the active and frozen indexes (D22) and a RocksDB snapshot, so it stays valid while V connects and flushes. Its rows are keyed by block hash. The one exception is the erased mined rows (Q1): a view pinned before a disconnect keeps the rows its snapshot and layers hold, which is what Core's reader sees under `cs_main` at the time of the read.
- **Anything that reads members, quorums or rotation snapshots** (RP2a's `quorum *` RPCs, P6b's `mnlistdiff`/`qrinfo` serving, QM1a, SB19) is a route-B command (§4.6), never an `EvoReadView` read: ADD §2.6's allow-list is closed.
- **The tip list for P2P and mempool work** is `ChainView.mn_list` (§5.1); inside V's mempool calls the sink receives the same list as an argument (D16). Core reads `GetListAtChainTip()` under `cs_main` for the same purposes.

### 5.4 What each consumer uses

| Consumer | Needs | Path |
|---|---|---|
| SB19 (mnxcheck) | the projection at a block, the facade's members | `ChainView.mn_list` + `project_sml` for the SML root at the tip; `EvoReadView` at a block; `peek`/`ShadowRows`/`ShadowSource` under one `OnV` command per comparison (§4.7) |
| AM1 | its own entry by operator key, list changes, platform-ban state | `ChainView.mn_list`, the async `ChainEvent::MnListChanged` (D15), SP1's meta store |
| GV1b | the tip list (votes, superblock payees `GetListAtChainTip`, `pay:367`, `:424`) | `ChainView.mn_list` |
| RP2a | `protx list/info/diff` at a block; `quorum list/info` | `EvoReadView`; route B for quorums |
| MP1a | special-tx checks against the tip (`CheckSpecialTx` with `pindexPrev = Tip()`, `stm:300-305`); the ProTx conflict maps and `GetKeyChangeTarget`, which read `GetListAtChainTip` (`txm:647`, `:660`, `:668`, `:699`, `:1139`) | `EvoReadView` at the chain tip (Core's `GetListForBlock(Tip())` through `pindexPrev`), `ChainView.{bls, credit_pool}`; `ChainView.mn_list` for the conflict maps and `GetKeyChangeTarget`; quorum lookups via route B |
| MP1b | the reorg re-add and `removeForBlock` (`txm:1321-1344` → `:1069`, `:1250`), which read `GetListAtChainTip` while `tipIndex` lags | the `tip_list` argument of `maybe_update_for_reorg` and `remove_for_block` (D16) |
| MN1 | the template's list and credit pool, the mineable pool; `FillBlockPayments`'s superblock arm (`GetListAtChainTip`, `pay:451-455`) | `EvoReadView` at the tip; `ChainView.mn_list` for `FillBlockPayments`; route B for the pool |

---

## 6. Persistence, crash consistency and recovery

### 6.1 B1

The flush machinery is CH2's and needs no new barrier (`rs:flush/machine.rs:942-943` already writes `delta.write_b1` and `meta.evo_tip`):

```rust
/// The evo layer's merged index (D22): every committed block's rows, merged, in persistent maps.
pub struct EvoIndex {
    by_hash: imbl::HashMap<(EvoCf, BlockHash), Arc<[u8]>>,   // mn_diff, mn_snapshot, credit_pool (+ credit data), mnhf
    mined: imbl::OrdMap<Vec<u8>, Option<Arc<[u8]>>>,          // llmq_mined and llmq_mined_h keys; None is a tombstone (§4.1.4)
    evo_tip: Option<BlockHash>,                               // the last committed delta's staged evo_tip (D7)
    bytes: u64,                                               // encoded key + value bytes (D18)
}
pub struct EvoWindowDelta(EvoIndex);                         // a frozen index: CH2's StateDelta
impl StateDelta for EvoWindowDelta {                         // rs:flush/packet.rs:20
    fn write_b1<B: KvBatch>(&self, batch: &mut B);           // codec.rs: a put per live entry, a delete per tombstone
    fn bytes(&self) -> u64;                                  // D18
}
impl EvoWindow<EvoWindowDelta> for EvoLayer { /* take: freeze the active index (an O(1) clone), start an empty one; bytes; best_block */ }
```

- **Keys** are A1 §2.9's (`A1-architecture.md:920-948`): `mn_diff`/`mn_snapshot`/`credit_pool`/`mnhf` by block hash; `llmq_mined` by `u8 type ‖ quorumHash`; `llmq_mined_h` by `u8 type ‖ i16 qIdx ‖ BE32(!minedHeight)`. Values are `dash_dml::codec`'s record envelopes (`rs:dash-dml/src/codec.rs:1004-1036`).
- **`EvoDelta`** stays dash-dml's one-block delta (`rs:dash-dml/src/store/delta.rs:26-48`). It gains two fields: `erased_mined: Vec<MinedErase>` (§4.1.4) and `evo_tip: Option<BlockHash>` (H6's staged value, D1) (§11.2).
- **D22. The layer is a merged index, not a stack of deltas** (Opus r2 M3). `commit_tx` encodes the pending delta's rows and merges them into the active `EvoIndex`. A row for an existing key replaces it (the values are equal by D6), and a tombstone replaces both mined entries with `None`. A flush's `take` moves the active index to the frozen one, which B1 writes, and starts an empty active index; CH2 freezes one window at a time, so there are at most two in-memory indexes whatever the window's size. `EvoWindow::best_block` reports the active index's `evo_tip`, and CH2's freeze check (`rs:flush/control.rs:36`, "a freeze checks that it is `new`") guards that the frozen index's `evo_tip` is the flush's `new`. Writing the frozen index is the same as writing the window's deltas in block order, with later rows and tombstones winning (CH2's `write_b1` doc changes to match, §11.2).
  - **The bound.** A read checks the attempt's pending delta (one block), then the active index, then the frozen one, then the DB snapshot. A point read costs O(log n) in the window's entries plus at most one RocksDB read on a miss. A range read over `llmq_mined_h` (`GetMinedAndActiveCommitmentsUntilBlock`, `bp:731-773`) merges the two ordered maps with a RocksDB iterator, the tombstones hiding the rows beneath them, in O(k + log n) for k results. Core's `CDBTransaction` maps give the same O(log n) (`bp:736`, `:780`).
  - Pinning an `EvoReadView` clones both indexes in O(1) and takes a RocksDB snapshot. VerifyDB's `EvoTxn` has the same structure: a scratch index over the active view.
  - Two growth guards hold the bound (§7.1): the `evo-index-depth` bench from CH7a-1 and the `evo-deep-window` run at CH7a-5 and CH7b-3. They guard asymptotic growth; they are not Core-parity PF0 rows.
- **D7. `meta.evo_tip` is Core's `b_b4`, and B1 writes it with the evo rows.** H6 stages it for every full connect and disconnect (`v:2967`, `v:2389`; D1). It becomes durable in the flush's B1, with every evo row of the frozen window and `head_blocks` (A1 §3.5.4; `rs:flush/machine.rs:942-943`). `meta.tip` is in B1 only when every coin fits there; otherwise B1 erases it and the last coins batch Bk writes it and erases `head_blocks` (`machine.rs:955-985`, `:709-710`). After a flush from `old` to `new` the durable state is one of two:
  - **B1 only** (a kill after B1, before Bk). The marker `head_blocks = (new, old)` is present, `meta.tip` is absent and `meta.evo_tip = new`. Recovery requires `evo_tip == new` (`rs:recovery/replay.rs:357-362`) and replays the coins alone from `old` to `new` (A1 §3.5.5; §6.3 step 2). The evo state is already at `new`.
  - **Complete** (after Bk, or a B1 that held every coin). There is no marker, and `meta.tip == meta.evo_tip` (`replay.rs:412-414`), which is Core's `VerifyBestBlock` at startup.

  Anything else is `EvoInconsistent`, or corruption when `evo_tip` is set without a tip (`:387-393`). H2 compares against the target's best block. For origin `VerifyDb` it reads the scratch `EvoTxn` index's `evo_tip` first, as Core's `VerifyBestBlock` inside VerifyDB reads its open transaction (`v:5197`): level 3's disconnects move it and level 4's connects check against it. Then it reads the active, then the frozen index's `evo_tip`, else `meta.evo_tip`. It reads an `evo_tip` at genesis as absent (§3.1 rows 3 and 4).
- **D20. The log pair.** Core times its evodb commit with `LOG_TIME_SECONDS("write evodb cache to disk")` after the coins write, in every full flush at a best block (`v:3164-3169`; "Failed to commit EvoDB" on failure). It prints "FlushStateToDisk: write evodb cache to disk started" and "… completed (%.2fs)" (CH2 D17, `logging/timer.h:38-47`). CH7a emits the same pair, with the same text, around B1's durable write (`engine.write(b, Commit::Durable)` or `write_final`, `rs:flush/machine.rs:955-985`) in every flush that has a best block: B1 is the write that carries the evo delta. Its position is the one difference from Core: Core prints the pair after the coins flush, and the node prints it before B2…Bk, because B1 must hold the evo state before any coin batch (A1 §3.5.4; CH2 D17 already notes the order). When every coin fits in B1, the pair also times the coins. A failure of the batch is CH2's existing B1 failure path. The PLAN and CH2 wording change is in §11.2.
- **D18. Size accounting.** Core's critical-size trigger is `m_evoDb.GetMemoryUsage() >= 64 MiB` in `IF_NEEDED` mode (`v:3095`; `edbh:226-234`). `EvoWindow::bytes` reports the encoded bytes of the frozen and active indexes, and `EVO_CRITICAL_BYTES = 64 << 20` (`rs:flush/policy.rs:19`) applies it. Core counts its own serialization of its own rows, so the trigger point can differ by framing; flush timing changes no consensus output, and PF0 measures the cadence.

### 6.2 Effect rows

`llmq_snapshot` rows are written by the snapshot applier as `Commit::Buffered` single-row batches, presumed effects with the WAL sync before the next B1 (§3.8). DEC-202 keeps them when there is no durable tip. Under **Q1** (DEC-517) `llmq_mined`/`llmq_mined_h` leave `EFFECT_CFS` (`rs:store/meta.rs:44`) and become transactional rows; `EFFECT_CFS` is then `["llmq_snapshot"]`, and DEC-202's text names only that CF.

### 6.3 Startup

In Core's order (`cs:142-296`, `cs:412-479`):
1. **Open.** The evo CFs exist from schema v1 (no migration, `rs:store/schema.rs:7`). `-reindex`/`-reindex-chainstate` wipe them with the coins (`WIPED`, `rs:store/meta.rs:31`; Core: `init.cpp:2065`).
2. **Recovery** (CH2) is coins-only: B1 puts the evo rows and `meta.evo_tip = new` in the batch that writes the marker, so evo is already at `new` whenever a marker exists (A1:1838, 1857-1862; CH2 D5). Core's `ReplayBlocks` re-runs `ProcessSpecialTxsInBlock` in `RollforwardBlock` (`v:5330-5370`) because its evodb and coins commit separately; here there is nothing for it to redo, and the evo state after recovery equals Core's after its rollforward. CH7a adds no evo replay. The checks stay CH2's: `evo_tip` must equal the replayed tip (`rs:recovery/replay.rs:357`, `:412`), and an `evo_tip` without a tip is corruption (`:387-393`). Core's "coins empty but evodb not" error (`cs:471-474`) is the second of these.
3. **D21. A datadir `NoEvo` built** (CH3 §2.17 "Removal"; **Q7**). It already carries `meta.evo_tip = tip`: CH2's B1 writes it on every full flush whatever the hooks (`rs:flush/machine.rs:943`, `:714`, `:732`), and recovery requires it (`rs:recovery/replay.rs:412-414`). Its tip is a block at which DIP3 is not active, because G1 refused every block at which a G1 deployment is active, and V20, MN_RR, V24 and EVO_SHARES all come at or after DIP3. Below DIP3 `NodeEvo` writes no evo row either (D11: `q_bbu2` is not modelled). So the datadir is the one `NodeEvo` would have written, and nothing on disk distinguishes them: there is no adoption step, and CH7a writes nothing. The startup check keeps DEC-517 (7)'s condition as a consistency check: if DIP3 is not active at the tip, every non-effect evo CF must be empty; otherwise startup fails with Core's `EvoDbInconsistencyMessage` (`v:1931-1937`) and "-reindex-chainstate" (Core's remedy). The turned-around G1 restart test (CH3 §2.17) uses such a datadir.
4. **The tip's `ChainView`**: `dmn_tip = None` and the empty `mn_list` (Core's `tipIndex` is null until init's `InitializeCurrentBlockTip`, `dmn:944-951`), `bls = bls_ctx_after(tip)` (D9), `credit_pool` from `pool_after(tip)`.
5. **VerifyDB** on V (§4.6), levels 3 and 4 as Core's (`v:5179-5327`): attempts with origin `VerifyDb`, a scratch `EvoTxn`, the effects applied (Core's level 3 re-adds mineable commitments and recomputes members; levels 3 and 4 both fire the MN-list signal, D15), nothing committed.
6. **The DMN tip** (D16), after init's block import. Core calls `InitializeCurrentBlockTip` only once `ThreadImport` has finished (`init.cpp:2482`): the reindex and `-loadblock` imports, `ActivateBestChain` for each chainstate (`node/blockstorage.cpp:966`) and `LoadMempool` (`:978`). Init then reads the tip and `ibd` (`init.cpp:2485-2486`) and calls it (`:2493` or `:2497` → `dsnotificationinterface.cpp:31-35`, fork `nullptr`). So the node's init sends a new `ChainCommand::InitializeCurrentBlockTip { ibd }` after `ChainCommand::VerifyLoaded` (`rs:chain/command.rs:194-196`), after init's `ActivateBestChain` and, once MP1 exists, after the mempool load, with the tip and `ibd` read at that point. V sets `dmn_tip` to its tip when there is one (a fresh datadir with no tip sets nothing, as `SynchronousUpdatedBlockTip(nullptr, nullptr)` returns early), publishes a `ChainView` with the tip's list (`resolve_list(tip)`), and then dispatches `Signals::initialize_current_block_tip` (`rs:chain/signals.rs:525-526`). Before the command every reader gets the empty list (D16), unless an init `ActivateBestChain` iteration that connected a block has already set `dmn_tip` through its signal. The synchronous listeners, registered after VerifyDB (D15), see the command, as Core's `CDSNotificationInterface` does (`init.cpp:2228`, before `:2479`). On a masternode Core kicks only `CDSNotificationInterface` here and broadcasts to every subscriber after `nodeman->Init` (`init.cpp:2569-2578`); the node's dispatch reaches every listener at once, and that split is AM1's obligation (§11.2). CH7a-3 adds the command and V's handler; the node's init sends it (§11.2).
7. **Not ported** (**Q6**): `ForceSignalDBUpdate`, `MigrateLegacyDiffs`, `RecalculateAndRepairDiffs`/`-forceevodbrepair` and `dmn_R1` (`cs:281`, `cs:286`, `init.cpp:2520-2560`). Each repairs or upgrades rows that older Core versions wrote; this node's rows are written only by this node. `-forceevodbrepair` is accepted and logs that there is nothing to repair.

### 6.4 Crash tests

CH3 §3.2's fault-injection test is rerun with `NodeEvo` (CH3 asks for this: "CH7a reruns it with its own"): a failure before the first effect, between effects, at `release_attempt`, at `begin_tx` and at `commit_tx`; each time the evo best block stays `prev`, no tip is published, and recovery after a restart equals an uncrashed run. ST3a's power-loss model gains the evo rows: a kill between a presumed snapshot write and the next B1 keeps the row (DEC-202), and a kill inside B1 keeps both coins and evo at the old tip.

---

## 7. Performance and verification

### 7.1 Where the cost goes, and what keeps it at Core's level

CH7a is the first row that does evo work in the node's connect. Every PF0 row that connects a block above DIP3 does work it did not do under `NoEvo` (G1 refused those blocks, so today no PF0 row connects one). The cost structure follows Core's:

| Work per block | Core | Node | Bound |
|---|---|---|---|
| DML build | `BuildNewListFromBlock` over a copy-on-write `immer` list | `build_new_list_from_block` over `imbl` (EV2a; shared structure, no full copy) | EV2a/EV2b's PF0 rows already measure it in isolation |
| `prev`'s list | `mnListsCache` (64) + snapshots every 576 blocks | `LruListCache` 64 + `resolve_list` (`rs:dash-dml/src/store/resolve.rs:169`) | same snapshot cadence (EV2a), the same miss pattern |
| MN-list root | `CalcCbTxMerkleRootMNList` with its static cache | `SmlRootCache` (`rs:dash-dml/src/sml.rs:67`), incremental per diff | EV4-serve |
| Quorum root | `CalcCbTxMerkleRootQuorums` + the qc-hash caches | LQ2's `quorum_root` + one `QcHashCache` per chainstate | LQ2 |
| QC signatures | queued on `m_bls_queue` (`-parbls`, `bp:92-102`) | `BlsCheckQueue` with Core's sizing (§4.1) | the same parallelism, no batching Core lacks |
| Members | `GetAllQuorumMembers` caches sized `keepOldConnections`, snapshot LRU 32 | `MembersCaches`/`PerTypeLru` with Core's sizes (§4.4) | ADD §2.6 |
| Evo writes | into the evodb transaction (memory), flushed in the full flush | `EvoDelta` in the layer, written in B1 | `EVO_CRITICAL` at Core's 64 MiB (D18) |

The only new lock on the per-block path is the attempt's exclusive gate. Its other holders are `OnV` commands, which take it shared and also run on V, so they never overlap an attempt; `RawRows` reads follow ADD §2.6's lock order. The `ChainView` publication adds one `Arc` clone per publication (D16).

**New PF0 rows** (in PF0's format):
- **At CH7a-5:** `b1-evo-write` (a B1 with a full `EVO_CRITICAL` window); `mnlist-tip-publish`; and `connect-evo-dip3-prequorum` (mainnet from 1,028,160 up to the first non-null QC, or a regtest chain with null commitments only).
- **At CH7b-3:** `connect-evo-v20-window` (a 2,000-block mainnet window after V20, connect only) and `connect-evo-dip24-cycle` (one rotation cycle base and its mining window, precompute included). Before CH7b-2 G1q refuses blocks in both windows (non-null QCs, asset unlocks, cycle bases), and until CH7b-3 step 10's verify refuses their CbTx ChainLocks (D2).

**Growth guards for D22's bound** (Opus-h r3 m5, Sol r3 Minor; DEC-527). They check that a deep unflushed window costs no more per block than a shallow one. That is an asymptotic-growth guard, not parity with Core, so they are not PF0 Core-parity rows, and Q12's re-anchor does not apply to them.
- **`evo-index-depth`** (an L0 bench, CH7a-1): `EvoIndex` point reads and `llmq_mined_h` range reads over a populated `mined` map with tombstones, at 1,000 and at 100,000 entries. Statistic: `instructions:u` per read over a fixed read set, median of 3 fresh-process runs. Pass: the 100,000-entry run is at most 2 × the 1,000-entry run. O(log n) predicts about 1.7 ×; a walk over the window would be about 100 ×.
- **`evo-deep-window`**: the same 1,000 measured blocks of a generated regtest chain whose blocks are uniform (each 1,000-block window holds the same transaction shapes and the same pattern of mining windows), connected in two configurations:
  - *shallow*: from a datadir flushed at the block just before the measured segment;
  - *deep*: from a datadir flushed at about block 1,000, connecting everything up to and through the segment with no flush, so the segment runs over a window of about 19,000 blocks.
  - **Statistic:** `perf stat -e instructions:u` around the measured 1,000-block window; the window's total divided by 1,000 is the per-block count. Each configuration runs 3 times in fresh processes, and the median is used. **Pass:** deep ≤ 1.05 × shallow. The 5% stays (DEC-527): both configurations connect identical blocks, so the statistic isolates the window's depth, and `evo-index-depth` catches small per-read growth directly.
  - **No flush:** the run fails if CH2's flush counter moved during the measured window in either configuration (`EVO_CRITICAL` and the dbcache are sized so that none triggers).
  - **Diagnostics:** on a failure, callgrind per-function counts of `EvoIndex::get`, `EvoIndex::range` and `resolve_list` (PF0's method) locate the growth.
  - **At CH7a-5** the chain has null commitments only, so the `mined` map stays empty. This precursor run guards the by-hash reads and the layer's per-block overhead; it does not exercise the mined range read.
  - **At CH7b-3** the full run uses a chain whose mining windows carry non-null QCs, with CbTx version 2 or later on every measured block, so every block runs step 9's `llmq_mined_h` range read (`bp:731-773`). Before the measured segment the deep run disconnects and reconnects about 100 blocks with commitments, so the window holds tombstones.

The "not worse than the previous head" rule cannot hold for the first evo-active rows, because the previous head did no evo work. Under **Q12** (DEC-517) they are re-anchored once, at CH7a-5 (its three rows) and at CH7b-3 (all five), and judged by the instruction ratio against Core v24 on the same windows (`perf stat -e instructions:u`, PF1's method).

### 7.2 Verification levels

**L0 (unit, per crate):**
- the H4 step table (§3.2): one test per row and gate. That includes `just_check` and `-assumevalid` (`check_cbtx_merkle_roots = false`, which still runs the AU and MNHF signatures: CH3's quirk test), each reject string, and each D5 class;
- the undo order (§3.3) and a connect/disconnect/reconnect round trip that leaves the layer and the caches as Core's (D6: DML rows stay, mined rows go);
- `EvoDelta` codec round trips (proptest) and a differential test of `RocksEvoStore` against `MemEvoStore` over random connect/disconnect/flush sequences;
- the window CH7a-4 opens (Opus r2 M1): an omitted required null QC (`bad-qc-missing`), an extra null QC (`bad-qc-not-allowed`), a duplicate null QC of a non-rotation type (`bad-qc-dup`, `bp:632-637`; the per-block duplicate check exempts null QCs, `bp:270-276`), an excess null QC of a rotation type (`bad-qc-not-allowed`, `bp:254-256`), a bad `merkleRootQuorums` (`bad-cbtx-quorummerkleroot`), and a null-CL CbTx with `bestCLHeightDiff != 0` (`bad-cbtx-cldiff`) or after a parent whose coinbase CL is non-null (`bad-cbtx-null-clsig`). Each gets Core's reject at CH7a-4, and the table test runs at every later slice;
- D2: an injected `NotBuilt` passes EV1, EV2b and LQ2 unchanged and ends in the readiness refusal (the reported `SystemError` on the tip path, unreported under `just_check`); a null-commitment block at a non-cycle-base height passes `NoMembers::precompute`; at the connector, a hook returning `SpecialTxFailure::NotReady` gives the reported refusal on the tip path (the node stops, the block keeps `HAVE_DATA` and gets no `FAILED_*` mark) and the unreported error for a proposal, and one returning `Invalid` keeps today's verdict and log line, for `NoEvo`, `NoEvoSteps` and the test hooks; at `-checklevel=4` with `-assumevalid=0`, a datadir whose last blocks carry a valid CbTx ChainLock connected under `-assumevalid` fails VerifyDB with the refusal text "ChainLock verification (CH7b)" through CH7b-2 (no verdict, no failure mark, the node not aborted; a runtime `verifychain` returns false and the node keeps running) and passes from CH7b-3; the two capability strings appear in the refusal text of their own gates;
- D5: an injected `QcBlockError::NoReason` at each of the three `bp:` places leaves the block without a failure mark and stops the node (DEC-518);
- D7: after a complete flush `meta.evo_tip` equals `meta.tip`; after a kill between B1 and Bk the marker is present, `meta.tip` is absent, `meta.evo_tip = new`, and recovery replays the coins only and ends at `new`; the fault-injection suite of §6.4;
- D1: a disconnect commits a delta holding only its tombstones and `evo_tip = pprev`; a failed, check-only, `SystemError` or `Fatal` attempt in either direction leaves the layer unchanged; the pending slot is empty at every `begin_attempt`, also after a failed coins lock (no `begin_tx`), a failed connect, a check-only attempt and a failed commit;
- D3 (G1q): a malformed QC, a malformed CbTx and a malformed ProTx, each alone and each with a well-formed non-null QC in the block, get Core's reject string; a well-formed block with a non-null QC, an MNHF signal, an asset unlock or a cycle-base height gets the readiness `SystemError` through CH7b-1; a block of null commitments only connects; a well-formed MNHF signal before a malformed QC reaches the stub's refusal (D2); each slice's arm set of D3's table;
- D3, no CbTx ChainLock arm (Opus-h r3 m2), at every slice from CH7a-4: a valid-signature CbTx with `bestCLHeightDiff >= height` gets `bad-cbtx-cldiff` (`stm:207-209`); a bad `merkleRootQuorums` next to a valid CbTx ChainLock gets `bad-cbtx-quorummerkleroot`; a valid signature in an otherwise valid block gets the verify stub's refusal (D2) through CH7b-2 and connects from CH7b-3; with `-assumevalid` on, that block connects at every slice, because step 10 does not run (Sol r2);
- D3, the declared interim difference: an invalid ProRegTx ahead of a well-formed non-null QC gives the readiness refusal through CH7b-1 and Core's `bad-protx-*` from CH7b-2; a non-null QC whose `nVersion` is not the expected one gives the readiness refusal through CH7b-1 and `bad-qc-invalid` from CH7b-2; likewise a `bad-mnhf-version` signal (arm (b)) and a step-1 CbTx reject in a cycle-base block (arm (c));
- D3, startup: at CH7a-4 a datadir the same binary built (regtest, null commitments only) restarts, and a tip carrying a non-null QC (written by the test) gets the startup refusal; at CH7b-2 a datadir synced past V20 with `-assumevalid` set (mainnet's default; on regtest set explicitly), whose tip carries a valid CbTx ChainLock, restarts;
- §4.1.4: the rotated-commitment disconnect and reconnect test with historical reads, with and without a flush in between;
- §4.5.2: step 10 succeeds and step 11 rejects, then a sibling block's step 10 finds `CbTxClCell` set (CH7b-3);
- D9: the BLS log lines: a tip-path disconnect of the last block before V19 activation prints Core's "entered" line; VerifyDB level 3 across V19 prints H9's switch line and VerifyDB's "reverted" line; a tip-path connect prints none;
- §3.6: a two-block reorg step whose second block is at a superblock height with `sb_required` true gives both superblock arms the old tip's list;
- §3.1 row 4: a regtest chain with DIP3 active at height 1 fails at block 1 with Core's `EvoDbInconsistencyMessage`;
- D8: a `just_check` attempt leaves the layer and `ChainView` unchanged but applies its effects;
- D9: `ChainView.bls` equals `bls_ctx_after(tip)` across V19 in both directions, with TBV, failed attempts and VerifyDB in between;
- D15/D16: the signal's condition (only on changes, never under `just_check`, both directions) and its position (a test subscriber sees the coins view at the parent and the old tip, for a connect and a disconnect); a synchronous listener that dispatches to V fails a debug assertion; startup VerifyDB's notifications reach no listener; with `NullMempool` the disconnect-only step leaves `mn_list` on the old tip; with a mempool sink, after that step `ChainView.mn_list` is the new tip's list and the sink's `tip_list` argument is the moved list; `reorg_drain` moves nothing when `add` is false (the 11th `InvalidateBlock` disconnect, a failed `DisconnectTip`); an iteration that reconnects to its starting tip signals nothing; a restart with no new block: startup VerifyDB sees the empty list and, after `InitializeCurrentBlockTip`, `ChainView.mn_list` equals the tip's list; a restart with blocks stored above the loaded tip: the superblock arms of init's first `ActivateBestChain` iteration get the empty list, and after the command `ChainView.mn_list` is the post-`ActivateBestChain` tip's list;
- D21: a `NoEvo`-built datadir opens unchanged; a below-DIP3 tip with a non-empty evo CF fails with "-reindex-chainstate";
- the exhaustive appliers: a new `Effect` variant without an applier fails to compile.

**L1 (conformance and oracles):**
- O2b `diff_chain` with evo ops (CH7a) and with QC transactions and rotation (CH7b), against the Core oracle at the pin;
- the H11 digest taps (`C.s` keys, `rs:conformance/tests/digest_replay_core.rs:61`);
- the quirk tests: 16 is un-ignored by CH7a-2 (owner); 13–15 and 24–25 run against `NodeEvo` as well as their owners' units; 26 and 27 when EV1-5 lands (they are EV1's scaffolds);
- ADD §2.6 cases 1–7 on the production handle (case 6 with real threads, plus 6-D), the compile-fail suite, the §2.4 latch test, and the snapshot-cache interleaving test (PLAN:31-34, O1);
- `peek` leaves the cache contents and the LRU order unchanged (ADD §4.3), checked by a cache-state digest before and after.

**L2 (chains):**
- mainnet 1,028,160 → the V20 windows and full testnet, comparing at sampled heights: the list (`protx list` form), the payees (`masternode payments` form), the credit pool and the deployment states against dashd v24 at the pin (the program's mainnet and testnet v24 reference oracles). Until RP2a provides the RPCs, a test binary prints the same JSON shapes from `EvoReadView`, and RP2a's later rows reuse the same comparison;
- segment replays across DIP24 and V19 (CH7b);
- the REL-1.1 campaign builds from CH7b's head.

**Fuzz** (O3 farm, DEC-184 plateau policy): the `EvoDelta` row decoders (our own rows, so a failed decode is `Fatal`, D5; the target checks that no input panics or allocates without bound), and an attempt-sequence target driving `NodeEvo` and the reference model with random connect/disconnect/`just_check`/failure orders, comparing layer, caches and effect logs.

**CH3's suites on evo chains.** CH7a-4 reruns CH3's reorg and ChainLock suites (CH3b's, written on `NoEvo` regtest) on a DIP3-active regtest chain, so every CH3b disconnect path runs H7, H9 and D16's updates (D15).

**FTs.** Core's `feature_nulldummy` and `feature_bip68_sequence` are CH7a's (CH3a D37; they cross DIP3, where G1 refuses today). They run once CH7a-4 lifts G1. The LLMQ FTs need masternode roles (non-goal); their recorded chains are replayed as `diff_chain` vectors instead (the program's Core functional-test chains).

---

## 8. Slice plan

### 8.1 CH7a

Each slice's last column states why, at its boundary, the node accepts no block Core rejects.

| Slice | Content | Merge deps | Unblocks | Accepts nothing Core rejects because |
|---|---|---|---|---|
| **CH7a-1** store and layer | `evo::{store, codec, layer, view}`: `RocksEvoStore` over the seven CFs, `EvoIndex`/`EvoWindowDelta: StateDelta`, `EvoLayer: EvoWindow` (D22), `EvoReadView`, D7/D18/D20 (the log pair), the `MemEvoStore` differential, the `evo-index-depth` bench (§7.1). No hook changes; `NoEvo` stays wired. | — | everything below | no hook changes; `NoEvo` and G1 unchanged |
| **CH7a-2** `NodeEvo` behind G1 | H1–H11 in Core's order (§3), D1–D6/D8–D11, caches (§3.7), the exhaustive appliers with stand-ins (§3.8), payments (H5), `NoMembers` and the `NotBuilt`/`NotReady` channel (D2). H4's member-free steps: LQ2's `process_block`/`undo_block` over `NoMembers` (count, duplicate, premature and null rules), `quorum_root` with one `QcHashCache`, and step 10's arms before `VerifyChainLock`. G1 still refuses evo-active blocks in the node; L0/L1 tests drive the hooks directly. Quirk 16 un-ignored. | CH7a-1, EV1-2 (the dispatcher); LQ2's code at the base | CH7a-3 | G1 still refuses every evo-active block |
| **CH7a-3** live list | `ChainView.{dmn_tip, mn_list, credit_pool}` with `reorg_drain`, `has_mempool` and the sink's `tip_list` (D16), the `InitializeCurrentBlockTip` command (§6.3 step 6), `notify_masternode_list_changed` (D15), `ChainQuery::evo_view` (D17), startup (§6.3, D21), VerifyDB origin ; `ChainQuery::evo_view` answers, in one V turn, the tip MN list, the tip credit pool and HasMinedCommitment on the active chain (MN0-2b's template source, DEC-612) | CH7a-2 | AM1 (`i:CH7a`), GV1b, SB19 (with CH7b-4) | G1 still refuses every evo-active block |
| **CH7a-4** lift G1 | `COMPLETE = true`; G1 → G1q (D3), `startup_refusal` in the startup check (D3), the G1 tests turned around, the restart test on a `NoEvo` datadir, CH3's reorg and ChainLock suites rerun on a DIP3-active regtest chain (D15), FTs `feature_nulldummy`/`feature_bip68_sequence` ; G1q's arms exported as a pure function of (block-or-template facts, parent, deployments), as `g1_rule` is, for MN0-2b's NotReady (DEC-612) | CH7a-3, **EV1-5** (**Q2**), **CH3b** (PLAN's `i:CH3b`: the disconnect and `ClEnforce` paths the evo undo runs in) | CH7a-5, the CH7b slices that need real chains, **MN0-2b** (DEC-612) | every H4 step that needs no quorum answer is live (since CH7a-2); G1q's arms (a)–(c) refuse, never accept, each well-formed block that needs members, a quorum or the precompute, preempting only the rejects D3 declares; step 10's verify refuses at Core's position (D2); a malformed payload gets Core's reject (D3) |
| **CH7a-5** L2 | the L2 runs to the first quorum block (G1q's limit until CH7b-2) plus the regtest chains with null commitments only; its three PF0 rows (§7.1, **Q12**) and the precursor `evo-deep-window` guard | CH7a-4 | CH7a's acceptance, finished by CH7b-3's L2 | no validation change |

CH7a-1 to CH7a-3 don't touch CH3b's files (`connect/disconnect.rs`, the reorg loop, `ClEnforce`); they only implement hooks and add store and view code. So they can merge before CH3b finishes. CH7a-4 merges after CH3b, under PLAN's interface-dependency rule (PLAN:489).

### 8.2 CH7b

| Slice | Content | Merge deps | Unblocks | Accepts nothing Core rejects because |
|---|---|---|---|---|
| **CH7b-1** facade and dispatch | the runtime `ChainCommand::OnV` (§4.6) replacing `TestOnV`, `OnValidation`, `RawRows`, `MembersFacade` with gate, caches and runtime snapshot rows (`snapshotCacheCs` sections), `AttemptHandle`, `release_attempt(EffectsApplied)`, `NodeLlmqSource`, the precompute loop (§4.3), ADD §2.6 cases 1–7, the compile-fail suite, the §2.4 latch test, the interleaving test | CH7a-2, LQ1b, BS11a (PLAN:497) | CH7b-2, CH7b-4 | the connector still uses `NoMembers`; G1q keeps arms (a)–(c); step 10's verify refuses at Core's position (D2) |
| **CH7b-2** production facade, mined rows and G1q removal | the production facade replaces `NoMembers` in the connector (`QcBinding`, eager decode, non-null commitments through `connect_commitments`, §4.1), the mined rows and `erased_mined` (**Q1**), the quorum object (D13), the chain-aware `ScanQuorums` with `GetQuorum` for the asset-unlock signature (route C, `al:165-172`; `bad-assetunlock-too-old-quorum`, `al:129`); G1q removed with its tests turned around | CH7b-1, LQ2 | CH7b-3 | G1q is gone; every member-dependent check is live except step 10's verify, which refuses at Core's position (D2), so nothing is accepted |
| **CH7b-3** CL verify | the `VerifyChainLock` call of step 10, `cl_best`, `CbTxClCell` (BL-179, **Q13**), `ReplayChainHelper`, the verify refusal's tests turned around, L2 across DIP24 and V19, the full-testnet L2 for both rows, the V20 and DIP24 PF0 rows | CH7b-2, LQ3, LK1a's `ClState` feed (an empty feed before LK1a is Core's "no best ChainLock" and is correct), CH2 prune-lock set, CH5 startup data check | REL-1.1, RP2a, MP1a, MN1, QM1a, CH6b, the M4 exit | every H4 step is Core's; no readiness refusal remains |
| **CH7b-4** mnxcheck surface | `peek`, `ShadowRows`, `ShadowSource`, `NoMemo`, `NotComparable(PrunedWorkBlock)` (§4.7), the read-only `llmq_S` reader, the cache-state digest test | CH7b-1 | **SB19** (with CH7a-3) | read-only surface; no validation change |
| **CH7b-5** mineable pool and intake | the facade's `MineablePool` host (D14) and its applier, replacing the stand-in, plus the `qfcommit` intake seam that QM1a/LQ3 call | CH7b-2 | MN1, QM1a | the pool is an effect target that no verdict reads (D14) |

### 8.3 SB19

SB19's code (the shadow task, the `getmnlistd`/`getqrinfo` cadence, the `project_sml` comparison and the metric) needs CH7a-3 and CH7b-4: the live list, `EvoReadView`, the runtime `OnV` command, `peek` and `ShadowRows`. It consumes the asynchronous `ChainEvent::MnListChanged` (D15). CH7b-4 depends only on CH7b-1, so this path does not wait for CH7b-2/3. SB19's acceptance, "on by default in regtest/devnet/testnet CI", needs a node that connects testnet past the first non-null commitment. That needs G1q gone (CH7b-2) and step 10's verify live (CH7b-3), because testnet blocks past the first ChainLocks carry a CbTx ChainLock. Ruling (**Q10**, DEC-517): start SB19 when CH7a-3 and CH7b-4 merge, test it on regtest chains with null commitments only and on recorded vectors, and close its acceptance after CH7b-3. SB19's peer-request D-row stays SB19's (DEC-515).

---

## 9. Acceptance map

### 9.1 CH7a (PLAN:577)

| PLAN text | Where | Slice |
|---|---|---|
| special-tx orchestration | §3.1–§3.3, D1–D5, D8 | CH7a-2 |
| CbTx MN root and credit pool | §3.2 steps 1–3, 5, 8; §3.5.2 | CH7a-2 |
| payments/value via `ChainHelper` (NoCheck) | §3.6 | CH7a-2 |
| MNHF | §3.2 step 11, §3.5 | CH7a-2 |
| BLS context (`for_block`) and undo | §3.4, D9; §3.3 | CH7a-2 |
| `EvoDelta` in the B1 batch | §6.1, D7, D18 | CH7a-1 |
| the effect channel (A1 §2.10.1) and the views (§2.10.2) | §3.8, D12; §5, D15–D17 | CH7a-2, CH7a-3 |
| O17 CONS-09 / `Effect::BlsSchemeLeak` / "per profile" | not built: DEC-82 removed the v23 profile; replaced by D9's scoped-scheme tests (**Q4**) | CH7a-2 |
| the `LOG_TIME_SECONDS` pair (CH2 D17) | §6.1, D20 | CH7a-1 |
| L2 mainnet 1,028,160→V20 windows + full testnet: `protx list`, `masternode payments`, credit pool, deployments | §7.2 L2 | CH7a-5 (to the first quorum block), CH7b-3 (full) |
| `diff_chain` with evo ops | §7.2 L1 (hook-level L0/L1 tests at CH7a-2) | CH7a-4 |
| effect parity for the §2.10.1 table | §7.2 L0/L1 (exhaustive appliers, reference-model comparison of effect logs) | CH7a-2 |
| …including the v23.1.8 BLS-flag leak under the v23 profile | dropped with the v23 profile (**Q4**) | — |
| quirks 13–16, 24–27 | §7.2 L1; 26 and 27 after EV1-5 | CH7a-2, CH7a-4 |
| CH3's reorg and ChainLock suites on a DIP3-active regtest chain (CH3 input) | D15, §7.2 | CH7a-4 |
| BG: `reindex-chainstate-mainnet` counters not worse than the previous head | §7.1 (**Q12**) | CH7a-5 |
| FR-CON-19, -21, -22, -27, -29, -32; FR-CHS-03/04; FR-EVO-01…09 | §3 (CON-19 order), §3.6 (payments), §6 (CHS-03/04 atomicity), EV2a/EV2b/EV4-serve units as composed in §3.2 | — |

### 9.2 CH7b (PLAN:578)

| PLAN text | Where | Slice |
|---|---|---|
| commitments in blocks (eager BLS decode, null rules) | §4.1 | null rules CH7a-2 (over `NoMembers`), non-null commitments CH7b-2 |
| quorum root | §4.1, §4.4 | CH7a-2 |
| rotation snapshots as effects | §4.3, §3.8, §6.2 | CH7b-1 |
| CL enforcement + `ChainHelper` CL/IS plumbing | §4.5 (enforcement itself is CH3b's `ClEnforce`; CL-conflict scenarios are LK1a's, F17) | CH7b-3 |
| `ReplayChainHelper` (failed attempts too) | §4.5.3 | CH7b-3 |
| `LlmqSource` and facade map `MemberError` per ADD §2.3 | §4.2 | CH7b-1 |
| attempt handle and overlay, gate, `release_attempt(EffectsApplied)` | §4.2 | CH7b-1 |
| `VContext`/`OnV`/`OnValidation`, whole-route V dispatch except `RawRows`; `begin_attempt` only with `VContext` | §4.6 | CH7b-1 |
| the precompute loop | §4.3 | CH7b-1 |
| read-only `peek`, `LlmqSource` and `llmq_S` reader for mnxcheck | §4.7 | CH7b-4 |
| `diff_chain` with QcTx and rotation | §7.2 L1 | CH7b-2 |
| segment replays across DIP24 and V19 | §7.2 L2 | CH7b-3 |
| REL-1.1 uses this build | §7.2 L2 | CH7b-3 |
| ADD §2.4 node latch test; §2.6 cases (1–6 in PLAN, 1–7 in rev6) on the production handle, case 6 with real threads + 6-D; compile-fail suite | §7.2 L1 | CH7b-1 |
| `peek` leaves cache state and access order unchanged | §7.2 L1 | CH7b-4 |
| BG: REL-1.3 counters | §7.1 | CH7b-3 |
| FR-CON-18, -28, -29, -32; FR-CHS-05; FR-LLMQ-01/02/04/05 | §4.1 (CON-18/28 QC rules), §4.5 (CHS-05's chainstate half), §4.2–§4.4 (LLMQ-02/05's chainstate half); LLMQ-04's relay rules and LLMQ-05's vvec/sk-share store are QM1a's | — |

---

## 10. Open questions and rulings

DEC-517 accepted the recommendations of Q1–Q13. Each row below records its ruling. DEC-518 ruled Q14.

1. **Mined-commitment rows are transactional, as in Core** (`bp:473-483`, `bp:573-601`). **Ruled:** they are `EvoDelta` rows, erased on undo (`erased_mined`, §4.1). `EFFECT_CFS` (`rs:store/meta.rs:44`) and DEC-202 keep only `llmq_snapshot`, and `dash-dml/src/store/mod.rs:17`'s doc is corrected (§11.2).
2. **EV1 in CH7a's deps.** **Ruled:** CH7a-2 depends on EV1-2 and CH7a-4 on EV1-5; PLAN's CH7a deps gain EV1.
3. **Staged G1 (D3).** **Ruled:** G1 is lifted at CH7a-4 and G1q ("quorum validation (CH7b)") holds until CH7b-2; rev3.1 (DEC-527) dropped its CbTx ChainLock arm, and step 10's verify refuses at Core's position until CH7b-3. Rev3 (DEC-522) states its arms per slice and moves the member-free checks into CH7a-2.
4. **CH7a PLAN text after DEC-82.** **Ruled:** the CONS-09 / `Effect::BlsSchemeLeak` / v23-profile wording is replaced by D9's scoped-scheme tests. The exact replacement text is in §11.2.
5. **Appliers whose owners haven't landed.** **Ruled:**
   - `UnreachableIsLock` until LK1b;
   - `MemPlatformBans` until SP1;
   - the mineable pool lives in the facade (CH7b-5, D14).
6. **Evodb repair and migration at startup.** **Ruled:** not ported. `-forceevodbrepair` is accepted and logs that there is nothing to repair (§6.3).
7. **Datadirs built under `NoEvo` (D21).** **Ruled:** `evo_tip = tip` only if every evo column family is empty and DIP3 is inactive at the tip. Otherwise startup fails with Core's message and `-reindex-chainstate`. Rev3 (DEC-522, Opus r2 m2): B1 already writes `evo_tip = tip` under `NoEvo`, so the condition stays as a consistency check and nothing is written.
8. **LQ1m.** **Ruled:** CH7b-1 runs the addendum's cases against both `reference::ModelAttempt` and the production handle; no new PLAN row.
9. **Crate of the runtime facade.** **Ruled:** `dashd-chainstate::llmq`.
10. **SB19.** **Ruled:** its PLAN row says "code after CH7a-3 and CH7b-4; acceptance after CH7b-3" (§8.3).
11. **ChainLock/InstantSend split.** **Ruled:**
    - CH7b adds `cl_best` and the CbTx ChainLock verify (§4.5.2);
    - LK1a/LK1b provide the back ends and the handler;
    - enforcement stays CH3b's.
12. **PF0 baseline for the first evo-active rows** (§7.1). **Ruled:** a one-time re-anchor at CH7a-5 (three rows) and at CH7b-3 (all five; DEC-522; DEC-527 made the deep-window run a growth guard, not a PF0 row), judged by the instruction ratio against Core v24 on the same windows. It is recorded as a DEC when those slices land.
13. **BL-179.** **Ruled:** `CbTxClCell` models Core's `cached_pindex`/`cached_chainlock` exactly (§4.5.2). BL-179 closes when CH7b-3, the slice that carries it, merges.
14. **The class of a reason-less quorum failure (D5).** **Ruled (DEC-518):** D5 keeps CH3's AbortNode class. H4 returns `Fatal`, so the block is not marked failed and the node shuts down. No `ConnectOutcome` variant and no `EvoHooks` change are added, and there is no CH3 erratum. Core's ActivateBestChain returns false there without an abort.

### 10.1 Sol r1 dispositions (DEC-520)

| Finding | Disposition | Where |
|---|---|---|
| M1 H7 after the commit | Fixed: H7 runs synchronously inside the attempt, after H6 (and the H1 commit on a connect), before `finish_attempt`, `ov.commit`, `commit_tx` and the tip publication, in both directions, as CH3 D47 and the code already have it; subscriber rule for effect targets; VerifyDB keeps the effect | §3.1 row 17, §3.8, D15, §7.2 |
| M2 D7 against B1 | Fixed: `meta.evo_tip` is in B1 with the evo rows and `head_blocks`; `meta.tip` is in B1 only when every coin fits, else in Bk; the B1-only invariant and its coins-only recovery are stated | D7, §7.2 |
| M3 D20 log position | Fixed: the pair is around B1's durable write, with Core's text; the timing difference from Core is stated; PLAN and CH2 wording in §11.2 | D20, §11.2 |
| M4 rotated inverse key | Fixed: each erase is a `MinedErase` tombstone (height, rotation index, both keys) built by H9; the rotated disconnect/reconnect test with historical reads | §3.3, §4.1.4, §7.2, §11.2 (LQ2) |
| M5 G1q decode | Fixed: any payload decode failure bypasses G1q and H4 returns Core's reject; G1q keys on raw types and reads only QC nullness and the CbTx ChainLock from decoded payloads; the stub's refusal is a `SystemError`-class answer; malformed QC, CbTx and ProTx tests | D2, D3, §7.2 |
| m1 disconnect delta | Fixed: `begin_attempt` creates a direction-specific empty delta, H9 appends tombstones, H6 stages `evo_tip = pprev`, `commit_tx` pushes it; failed and check-only paths drop it; VerifyDB stacks it in its `EvoTxn` (rev3 moves the delta into the target's pending slot, §10.2) | §2.1, D1, §7.2 |

### 10.2 r2, r3 and r3.1 dispositions (Opus-h r2, Sol r2 and CH3's inputs, DEC-522; Sol r3 and Opus-h r3, DEC-527; Opus-h r3.1, DEC-529)

| Finding | Disposition | Where |
|---|---|---|
| Opus M1 member-free checks after G1 lifts | Fixed (ruling 1): steps 6 and 9 and step 10's arms before the verify are CH7a-2's over `NoMembers`; CH7b-2 is the production facade, mined rows and the quorum object; CH7b-3 the CL verify, `CbTxClCell` and `ReplayChainHelper`; G1q's arms per slice; L0 rows for the window; each slice states why it accepts nothing Core rejects | §3.2, D3, §7.2, §8, §9 |
| Opus M2 superblock list | Fixed (ruling 2): both superblock arms get the `dmn_tip` list on every origin; `ChainView.dmn_tip` and one definition of `mn_list`; MP1a reads `EvoReadView` at the chain tip; reorg-step test | §3.6, §5.1, D16, §5.4, §7.2 |
| Opus M3 delta-layer reads | Fixed (ruling 3): D22 merged index (`imbl` maps, tombstones), O(log n) point and O(k + log n) range reads, O(1) pinning, the same for `EvoTxn`; PF0 `connect-evo-deep-window` (the PF0 row is superseded by DEC-527, see the r3 m5 row) | §2.1, §6.1, D17, D22, §7.1 |
| Sol Major assumevalid | Fixed (ruling 4): arm (d) requires `check_cbtx_merkle_roots`; Sol's test. Superseded by DEC-527 (see the r3 m2 row): arm (d) is gone, and step 10 does not run under `-assumevalid` | D3, §7.2 |
| Opus m4 = Sol Minor 1 H7 re-entry | Fixed (ruling 5): only Core-equivalent listeners are synchronous; AM1, DK1, QM1a and SB19 take the async event; no dispatch, gate or wait; arguments only; registration after startup VerifyDB | D15, §3.8, §5.4, §8.3 |
| Opus m6 = Sol Minor 2 delta handoff | Fixed (ruling 6): `PendingTx` slot owned by `NodeEvo`/the scratch `EvoTxn`; `begin_tx` opens, H4/H9/H6 fill, `commit_tx` moves, `rollback_tx` clears; empty at `begin_attempt`; `post_scheme` travels with it | §2.1, §3.1, D1, §7.2 |
| Opus m1 `NoMembers` channel | Fixed (ruling 7): `StoreError::NotBuilt` propagated unchanged; H4's `SpecialTxFailure::NotReady` into the connector's `refuse`; the stub's precompute refuses only on the members call | D2, D5, §4.1.2 |
| Opus m2 D21 premise | Fixed (ruling 8): a `NoEvo` datadir already has `evo_tip = tip` and equals `NodeEvo`'s state; nothing distinguishes them, so no adoption; the below-DIP3 condition stays as a consistency check; CH3 §2.17 erratum | §6.3, Q7, §11.2 |
| Opus m3 BLS on disconnect | Fixed: H9 runs under `bls_ctx_for_block(pindex)`; step 1's switch and log only for origin `VerifyDb`; the guard's entered/reverted lines | §3.3, D9, §7.2 |
| Opus m5 PF0 rows | Fixed: CH7a-5 anchors four rows that its G1q allows; the V20 and DIP24 rows move to CH7b-3. Superseded by DEC-527 (see the r3 m5 row): three rows at CH7a-5, five at CH7b-3 | §7.1, §8, Q12 |
| Opus m7 `CbTxClCell` | Fixed: written in the attempt at step 10, outside the transaction, also in later-failing and check-only attempts; test | §4.4, §4.5.2, §7.2 |
| Opus n1 G1q position, startup | Fixed: at H4's entry after step 0; `EvoHooks::startup_refusal` for the startup check | D3, §11.2 |
| Opus n2 rotation predicate, CL decode | Fixed: Core's `IsQuorumRotationEnabled`, shared with the precompute; `bestCLSignature` decoded under H1's `BlsCtx` | D3 |
| Opus n3 D4 → D5 | Fixed | §3.8 |
| Opus n4 window type, field additions | Fixed: `EvoWindowDelta`; `EvoDelta` gains `erased_mined` and `evo_tip` | §6.1, §11.2 |
| Opus n5 genesis `b_b4` | Fixed: H2 reads an `evo_tip` at genesis as absent, so DIP3-at-1 fails as in Core; test | §3.1 rows 3–4, D7, §7.2 |
| Opus n6 `MemPlatformBans` | Fixed: RPC visibility noted; RP2a needs SP1's store; SP1 starts empty | §3.8 |
| Opus n7 tombstone test | Fixed: no-flush variant; mining-block vs base agreement stated | §4.1.4 |
| Opus n8 `diff_chain` slice | Fixed: CH7a-4; hook-level tests stay at CH7a-2 | §9.1 |
| (found in rev3) double precompute | Fixed: §4.1.2 no longer calls the precompute before `process_block`, which already runs it (`rs:dash-dml/src/blockprocessor.rs:367`) | §4.1.2 |
| CH3 input 1: CE4-17 moves the DMN tip | Adopted (checked at the pin, `v:426`, `:433-434`, `:3789`, `:4081`, `:4250`): V moves `dmn_tip` before each add=true drain when `MempoolSink::has_mempool()`; MP1b's obligation; the test pins both configurations | D16, §3.6, §11.2 |
| CH3 input 2: "once per step" | Adopted (`v:3869-3958`, signal at `v:3937`): once per ABC iteration that connected a block | D16 |
| CH3 input 3: H7 needs no CH3b change | Adopted: stated in D15; CH7a-4 reruns CH3's reorg and ChainLock suites on a DIP3-active regtest chain | D15, §7.2, §8.1, §9.1 |
| Sol nit D3 wording | Fixed: G1q itself refuses only fully decoded blocks; H4 may still refuse in a bypassed block | D3 |
| r3 Sol Major: H4's signature | Fixed (DEC-527 1): `Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>` in §3.2 and D2; the enum defined at CH3's seam beside `EvoHooks`; the connector keeps `Invalid`'s verdict and log line and maps `NotReady` to `refuse`; `HookSteps` passes it through; `NoEvo`, the bench and the test hooks wrap `Invalid`; tests | §3.2, D2, §7.2, §11.2 |
| r3 Opus m2: interim preemption | Fixed (DEC-527 2): arm (d) dropped, and step 10's verify refuses at Core's position, so the member-free `bad-cbtx-*` rejects come first; arms (a)–(c) kept as defence in depth, with the rejects they preempt declared; G1q removed at CH7b-2; tests for both; §8's column rechecked | §1, D2, D3, D5, §7.1, §7.2, §8, §8.3, Q3, §11 |
| r3 Opus m3: `startup_refusal`'s inputs | Fixed (DEC-527 3): `startup_refusal(&self, block, at, deployments) -> Option<&'static str>` (the receiver dropped by DEC-529, see the r3.1 m3 row), arms (a)–(c) only; with (d) gone it needs no assumevalid input; the `load.rs:382-384` early return becomes the call; a datadir the same binary built restarts; tests at CH7a-4 and CH7b-2 | D3, §7.2, §11.2 |
| r3 Opus m1: startup `dmn_tip` | Fixed (DEC-527 4): §6.3 step 4 publishes `dmn_tip = None` and the empty list; new step 6 sends `ChainCommand::InitializeCurrentBlockTip` after `VerifyLoaded`, which sets `dmn_tip`, publishes and dispatches `Signals::initialize_current_block_tip`; D16 cites `signals.rs:521-522` and `:525-526`; restart test | §3.6, §5.1, D16, §6.3, §7.2, §8.1, §11.2 |
| r3 Opus m4: readers of the chain-tip list | Fixed (DEC-527 5): `reorg_drain` publishes the drain-time move before the step ends; the sink's `maybe_update_for_reorg` and `remove_for_block` receive the `dmn_tip` list; MP1a, MP1b and MN1 rows; MP1b's obligation widened | D16, §5.3, §5.4, §7.2, §11.2 |
| r3 Opus m5 and Sol Minor: deep window | Fixed (DEC-527 6): two growth guards, not PF0 rows: `evo-index-depth` (CH7a-1) and `evo-deep-window` (the same blocks shallow and deep, a no-flush check, total `instructions:u` per block over the 1,000-block window, median of 3 fresh runs, deep ≤ 1.05 × shallow, the tombstoned mined range at CH7b-3); 5% kept; PF0 rows become 3, then 5 | D22, §7.1, §8, Q12 |
| r3 Opus n1: signal condition | Fixed: ABC's own condition (`v:3904`, `v:3926-3937`); an iteration that reconnects to its starting tip signals nothing | D16, §7.2 |
| r3 Opus n2: the `add` argument | Fixed: "when the call's `add` argument is true"; one wrapper, `reorg_drain(pool, add)`, for the three sites | D16, §11.2 |
| r3 Opus n3: null QC duplicates | Fixed: `bad-qc-dup` for a non-rotation type (`bp:632-637`, `:270-276`); an excess rotation null QC gets `bad-qc-not-allowed` | §7.2 |
| r3 Opus n4: CH2 doc and freeze check | Fixed: `packet.rs:21` erratum; D22 cites `control.rs:36` | D22, §11.2 |
| r3 Opus n5: CH7b-2's `ScanQuorums` | Fixed | §8.2 |
| r3 Opus n6: H2 under VerifyDB | Fixed: the scratch `EvoTxn`'s `evo_tip` first (`v:5197`) | D7 |
| r3.1 Opus m1: step 6's place in init | Fixed (DEC-529 1): the command follows init's block import, its `ActivateBestChain` and (with MP1) the mempool load (`init.cpp:2482-2497`; `node/blockstorage.cpp:966`, `:978`), with the tip and `ibd` read then; the empty list is served before it, as in Core; restart test with blocks above the tip; the masternode split is AM1's | §3.6, D16, §6.3, §7.2, §11.2 |
| r3.1 Opus m2: arm (a)'s preempted set | Fixed (DEC-529 2): `bad-qc-invalid` from `Verify`'s member-free checks (`commitment.cpp:125-176`, members call at `:177`, result at `:304`), and the quorum-dependent transaction's own shared-collateral check (`stm:940-946`); a `nVersion` test row | D3, §7.2 |
| r3.1 Opus m3: `startup_refusal`'s receiver | Fixed (DEC-529 3): an associated function, `H::startup_refusal(block, at, deployments)`, like `H::COMPLETE`, callable from `check_startup_readiness::<NodeEvoHooks>` before V starts; deployments from `ChainDeployments::new(...).view()` | D3, §11.2 |
| r3.1 Opus n1: step-layer trait | Fixed (DEC-529 4): `AttemptSteps` (`serial.rs:98`, `:114-123`) with `HookSteps` and `NoEvoSteps` (`bench.rs:51`, `:64`); the `EvoHooks` implementations listed separately | D2, §11.2 |
| r3.1 Opus n2: the verify stub's seam | Fixed (DEC-529 5): `ClVerifier`, `NotBuiltClVerifier` until CH7b-3, capability "ChainLock verification (CH7b)" | D2, §4.5.2, §7.2, §11.2 |
| r3.1 Opus n3: superseded r2 rows | Fixed (DEC-529 6): three r2 rows marked superseded by DEC-527 | §10.2 |
| r3.1 Opus n4: `NotReady` under VerifyDB | Fixed (DEC-529 7): a VerifyDB failure carrying the refusal text, unreported, no verdict and no failure mark; test row | D2, §7.2, §11.2 |

---

## 11. Decision register and errata

### 11.1 D-rows

| D | Decision | § |
|---|---|---|
| D1 | One `EvoDelta` per attempt in the target's `PendingTx` slot: empty at `begin_attempt`, opened by `begin_tx`, filled by H4 or (tombstones only) H9, `evo_tip` staged by H6, moved by `commit_tx`, cleared by `rollback_tx` | 3.1 |
| D2 | Before CH7b-2 `NoMembers`, and until CH7b-3 step 10's `NotBuiltClVerifier`, refuse through `StoreError::NotBuilt` → `SpecialTxFailure::NotReady` (H4 returns `Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>`) → the connector's readiness refusal (under VerifyDB, a VerifyDB failure with no abort); its precompute refuses only on the members call | 3.2 |
| D3 | G1 lifted at CH7a-4; G1q at H4's entry with arms (a)–(c) until CH7b-2, the rejects they preempt declared; no CbTx ChainLock arm (step 10's verify refuses at Core's position, D2); `H::startup_refusal` is an associated function taking the deployments; a decode failure bypasses G1q to H4's Core reject | 3.2 |
| D4 | Step-4 quorum lookups go through the attempt | 3.2 |
| D5 | Exception mapping: Core reject / store fault → `Fatal` / readiness refusal / reason-less quorum failure → `Fatal` (no failure mark; the node shuts down) | 3.2 |
| D6 | Transactional class = Core's evodb transaction; DML/pool/MNHF rows never erased; mined rows erased on undo | 3.5 |
| D7 | `meta.evo_tip` is Core's `b_b4`, written in B1 with the evo rows and `head_blocks`; `meta.tip` in B1 or Bk; B1-only prefix recovers coins only; genesis read as absent by H2; under VerifyDB H2 reads the scratch `evo_tip` first | 6.1 |
| D8 | A check-only attempt applies its effects and drops its delta and scheme | 3.1 |
| D9 | BLS scheme per attempt; published on commit; no persisted `bls_legacy`; Core's guard log lines where its guard would change the value | 3.4 |
| D10 | Derived rows are written with their defining block | 3.5 |
| D11 | `q_bbu2` is not modelled | 3.5 |
| D12 | CH7a applies the effect log; CH7b owns the handle | 3.8 |
| D13 | Quorum object = commitment + members + mined block; contributions are QM1a's | 4.4 |
| D14 | The mineable pool lives in the facade | 4.4 |
| D15 | The MN-list signal is synchronous inside the attempt (after H6, before the commit), only on changes, also from VerifyDB; only Core-equivalent listeners, which never re-enter V and read no effect target; the rest take the async event; no CH3b change, CH3's suites rerun at CH7a-4 | 5.2 |
| D16 | `ChainView.mn_list` is the list at `ChainView.dmn_tip`, Core's DMN tip, which moves on `SynchronousUpdatedBlockTip` with `new != fork` (ABC's own condition), on the startup `InitializeCurrentBlockTip` command after VerifyDB, and, with a mempool, in `reorg_drain` before each drain whose `add` is true (CE4-17); every move is published before its step ends, and the mempool sink receives the list as an argument; it feeds the superblock arms, MP1 and MN1 | 5.1 |
| D17 | Historical evo reads go through a pinned `EvoReadView`; member/quorum reads are route B | 5.3 |
| D18 | `EVO_CRITICAL` uses the encoded bytes of the window | 6.1 |
| D19 | Core's bench lines and running totals, under `bench` | 3.2 |
| D20 | The "write evodb cache to disk" pair around B1's durable write; before the coin batches, unlike Core | 6.1 |
| D21 | A `NoEvo` datadir already equals `NodeEvo`'s; below DIP3 every non-effect evo CF must be empty, else reindex | 6.3 |
| D22 | The evo layer is a merged persistent index: O(log n) point reads plus one DB read, O(k + log n) range reads, O(1) pinning; held by two growth guards, not PF0 rows | 6.1 |

### 11.2 Errata to approved text (applied when this design is accepted)

Applied at approval (DEC-531): the A1, CH2, CH3, LQ2 and EV2 text items, and PLAN's (node-program c1fa05559, including BL-439 for the node's init and BL-440 for CH5's VerifyDB refusal). The code-doc items (`blockprocessor.rs:496-497`, `store/mod.rs:17`, `packet.rs:21`, `view.rs:6`) land with the slices that touch that code.

- **A1 §2.9/§2.10 and CH2 B1 text:** remove `meta.bls_legacy` / "`bls_legacy`" from B1's contents (D9: the scheme is derived from the deployment state at the block and is never persisted).
- **A1 §2.10.1:** the `Effect` enum in the text predates the code's six variants (`rs:dash-evo/src/effect.rs:34`: `StoreQuorumSnapshot`, `RemoveConflictingIsLock`, `EraseMineableCommitment`, `ReinsertMineableCommitment`, `ResetPlatformBan`, `NotifyMnListChanged`). The text is aligned with the code; the CONS-09 `BlsSchemeLeak` row is dropped (DEC-82, **Q4**).
- **PLAN CH7a** (**Q4**, **Q2**). Three replacements:
  - In the content column, replace "· O17 CONS-09: BLS scheme scope per profile, on mainnet too (`Effect::BlsSchemeLeak`) Emit Core's" with "· BLS scheme scoped per attempt (`bls_ctx_for_block`) and published as `ChainView.bls` only when a connect commits; no process-wide scheme and no persisted flag (CH7 D9) · Emit Core's".
  - In the acceptance column, replace "effect parity for the §2.10.1 table, including the v23.1.8 BLS-flag leak (A1 §2.25 row 26) under the v23 profile;" with "effect parity for the §2.10.1 table; the scoped-scheme tests of CH7 D9 (`ChainView.bls` equals `bls_ctx_after(tip)` across V19 in both directions, with TestBlockValidity, failed attempts and VerifyDB in between);".
  - Replace the deps cell "CH3a, EV3, EV4-serve (SB0 rev6.1), i:CH3b, i:O2b" with "CH3a, EV1 (EV1-2 for CH7a-2, EV1-5 for CH7a-4), EV3, EV4-serve (SB0 rev6.1), i:CH3b, i:O2b".
  - The BG baseline (**Q12**) is recorded as a DEC when CH7a-5 and CH7b-3 land.
- **PLAN CH7a and CH2 :2574 (D20):** "around the EvoDelta's write in B1" becomes "around B1's durable write, which carries the EvoDelta (Core's text; printed before the coin batches, where Core prints it after the coins flush)". CH2 D17's text already records the order difference and needs no change.
- **LQ2 D6 and `undo_block`'s doc (`rs:dash-dml/src/blockprocessor.rs:496-497`):** "nothing to erase in the append-only store" becomes "returns one `MinedErase` tombstone per non-null commitment, which H9 appends to the disconnect delta" (**Q1**, §4.1.4). CH7b-2 carries the code change.
- **CH3 §3.1 (`EvoHooks`):** H4's signature becomes `-> Result<Result<MnListUpdates, SpecialTxFailure>, Fatal>`, with `SpecialTxFailure { Invalid(BlockValidationError), NotReady(&'static str) }` defined beside `EvoHooks` (`rs:connect/evo.rs`). The step layer's `AttemptSteps::process_special_txs` (`rs:connect/serial.rs:98`, `:114-123`) takes the same inner type; `HookSteps` (`rs:connect/connector.rs:610`) passes it through and `NoEvoSteps` (`rs:connect/bench.rs:51`, `:64`) wraps `Invalid`. The connector keeps `Invalid`'s verdict and log line and maps `NotReady` to `refuse(refusal_text(..))` (`rs:connect/serial.rs:816-829`); for origin `VerifyDb`, `refuse` returns the `SystemError` without `abort_node`. The `EvoHooks` implementations `NoEvo` and the test hooks wrap `Invalid` (D2). A provided associated function `startup_refusal(block, at, deployments: &dyn DeploymentView) -> Option<&'static str>` (default `None`, no receiver, like `COMPLETE`) serves the startup check, and `check_startup_readiness`'s early return on `H::COMPLETE` (`rs:chain/load.rs:382-384`) becomes a call of `H::startup_refusal` per block, with `ChainDeployments::new(...).view()`, formatted with `refusal_text` (D3).
- **CH3 §2.17 "Removal"** ("H6 is a no-op … VerifyBestBlock fails on an empty EvoDB"): B1 writes `meta.evo_tip` under `NoEvo` too, so a `NoEvo` datadir opens under `NodeEvo` with no adoption step (D21).
- **`dash-dml`:** `StoreError::NotBuilt(&'static str)`, propagated unchanged by EV1, EV2b and LQ2 (D2); `EvoDelta` gains `erased_mined` and `evo_tip` (§6.1).
- **A1 §2.10.2 (`A1-architecture.md:1084`):** "the evo deltas of the active and frozen layers" becomes "O(1) clones of the active and frozen layers' merged indexes" (D22).
- **PLAN CH7b-2/CH7b-3 content:** CH7b-2 is "the production facade, the mined rows and `erased_mined`, the quorum object, the chain-aware `ScanQuorums`, G1q removal"; CH7b-3 is "the CL verify, `CbTxClCell`, `ReplayChainHelper`". The null rules and the quorum root move to CH7a (§8).
- **CH3 seams (`rs:chain/seams.rs:903`, `rs:chain/command.rs`):** `MempoolSink` gains `fn has_mempool(&self) -> bool` (false for `NullMempool`), and `maybe_update_for_reorg` and `remove_for_block` gain a `tip_list: &Arc<MnList>` argument, V's current `dmn_tip` list. The three drain sites call V's `reorg_drain(pool, add)`, which moves and publishes `dmn_tip` when `add` is true and the sink has a mempool (D16). `ChainCommand` gains `InitializeCurrentBlockTip { ibd }`, which V handles by setting `dmn_tip`, publishing, and dispatching `Signals::initialize_current_block_tip` (§6.3 step 6).
- **MP1b (CE4-17; CH3b D183):** when MP1b replaces `NullMempool`, its sink answers `has_mempool` true, so `reorg_drain` moves `dmn_tip` before every drain whose `add` is true (D16). MP1b never sets `dmn_tip` itself. In every mempool call Core makes while `tipIndex` lags (the re-add's `addUncheckedProTx` and `removeForBlock`'s ProTx conflict removal: `txm:647-699`, `:1069`, `:1250`, `:1321-1344`), MP1b reads the `tip_list` argument; elsewhere it reads `ChainView.mn_list` (§5.4). D16's with-mempool test then runs against MP1b's sink as well as the test sink.
- **ND1 (init):** the node's init sends `ChainCommand::InitializeCurrentBlockTip { ibd }` after `VerifyLoaded`, after init's `ActivateBestChain` and, once MP1 exists, after the mempool load, with the tip and `ibd` read at that point, as Core's `init.cpp:2482-2497` follows `ThreadImport` (`node/blockstorage.cpp:966`, `:978`) (§6.3 step 6).
- **AM1 (masternode init):** on a masternode Core's startup tip reaches only `CDSNotificationInterface` before `nodeman->Init`, and every other subscriber after it (`init.cpp:2493`, `:2569-2578`). The node's command dispatches to every listener at once, so AM1 defers its own startup-tip handling until its `Init` equivalent has run (§6.3 step 6).
- **CH5 (VerifyDB):** the VerifyDB result gains a refusal outcome carrying the text of a `SpecialTxFailure::NotReady` met at level 4; `VerifyLoaded` fails the load with it, and `verifychain` returns false (D2).
- **CH2 `StateDelta::write_b1` doc (`rs:flush/packet.rs:21`):** "Writes the window's evo records into B1, in block order" becomes "Writes the window's merged entries into B1 (CH7 D22): a put per live entry, a delete per tombstone".
- **PLAN SB19:** acceptance closes after CH7b-3 (**Q10**).
- **`EFFECT_CFS` and DEC-202:** only `llmq_snapshot` (**Q1**). `dash-dml/src/store/mod.rs:17`'s doc ("never erased") is corrected to "erased on undo" for the mined-commitment rows.
- **PLAN CH7b-3:** deps add "CH2 prune-lock set" and "CH5 startup data check" (DEC-518), as in §8.2.
- **EV2 :720/:726/:777/:997:** "v24 notifies always" is superseded by the :999 erratum (only on changes), which D15 follows.
- **`ChainView` doc (`rs:chain/view.rs:6`):** `mn_list_at_tip` is `ChainView.mn_list` under D16's tip rule.
