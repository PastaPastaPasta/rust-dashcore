# EV3: payments, credit pool and MNHF signals: implementation notes

> **PLAN §7 handling applies to this document;** see ~/private/embargo.

| | |
|---|---|
| Status | EV3-1 (payments) READY on `node/ev3-payments` (independent review ACCEPT-WITH-NITS, nits and r2 B1 applied); EV3-2 (credit pool) on `node/ev3-credit-pool`, under review; EV3-3 (MNHF) on `node/ev3-mnhf`, stacked on EV3-2 |
| Date | 2026-10-20 |
| Task | PLAN row EV3: payments (`GetBlockTxOuts`, payee and value validation, superblock hooks), credit-pool eras, MNHF signals; O17 CONS-10 and MNO-12 |
| Branch | `node/ev3-payments` from `node/integration` 4d85bfd4b (train 19); worktree `~/workspace/rdc-wt/ev3`; scratch `/work/scratch/ev3` |
| Profile | v24 only (DEC-82). The PLAN row's "per profile" collapses to v24 behaviour, as for EV1 (EV1 §8 A-6) and EV2b (D1) |
| Core verified at | `v24.0.0-rc.3` (da567d7dba) in `~/workspace/dash`. Every `file:line` below is at that tag, paths under `src/`. `pay` = `masternode/payments.cpp`, `sb` = `governance/superblock.cpp`, `cp` = `evo/creditpool.cpp`, `cph` = `evo/creditpool.h`, `rs` = `util/ranges_set.cpp`, `rsh` = `util/ranges_set.h`, `stm` = `evo/specialtxman.cpp`, `mh` = `evo/mnhftx.cpp`, `mhh` = `evo/mnhftx.h`, `val` = `validation.cpp` |
| Oracle | pin 77dbec82427cfa91d420825235ca0f7abf5c984e, ABI 28 (`/work/cache/oracle/77dbec82…/{plain,fuzz}`): `oracle_block_value_payee` (formats 1 and 2), `oracle_credit_pool_step`, `oracle_mnhf_step` (formats 1 and 2, BL-211). ABI 28 is the pin's; `oracle_block_value_payee` and `oracle_credit_pool_step` were introduced at ABI 18 (O2a-GV) |
| Inputs | O2a-GV-context.md, O2a-GV-notes.md, O2a-MNHF-notes.md, O2a-GOV-notes.md D10; EV-addendum-SB0.md §1.5; EV1-specialtx.md (D7, D8, §2.3 `UnlockIndexes`); EV2b-notes.md; CH3a slice a1 (`dash-evo/src/payments.rs`, `dashd-chainstate/src/connect/serial.rs:681-765`); O17-release-profile.md (CONS-10, MNO-12); quirks 14, 15, 31; PRD FR-CON-21, -22, -26, -27, FR-EVO-09 |

---

## 0. Summary

1. **What exists.** CH3a a1 landed every governance-free branch of `IsBlockValueValid`,
   `IsOldBudgetBlockValueValid` and `IsBlockPayeeValid` in `dash_evo::payments`, with the
   governance- and list-dependent branches returned as `Ev3Branch` seams that `ConnectBlock` refuses
   today. `dash_consensus::payments` already has `GetMasternodePayment`, `PlatformShare`, the operator
   reward, owner payouts and `SplitAmountByShares`. `dash_consensus::deployments` has
   `GetSignalsStage` over an `EhfSignalSource` (CS4). `dash_dml::{creditpool, mnhf}` hold only EV0's
   record types. `dash_dml::payee` has `GetMNPayee` (EV2b).
2. **What EV3 adds.** The two `Ev3Branch` resolutions (masternode payee, superblock payments); the
   credit pool (ranges set, diff, construction in three limit eras, CbTx balance check, the
   per-block data the window reads); and the MNHF manager (`ProcessBlock`, `UndoBlock`,
   `GetForBlock` over store rows and Core's cache, `IsValidMNActivation`, the node's
   `EhfSignalSource`).
3. **Four slices** in Core's call order: payments, then the credit pool, then MNHF, then the O17
   rows and quirk closure (§3). Each slice is diffed against its oracle entry at ABI 28 (§4).
4. **Two departures from the PLAN row's paths** need the manager's ruling. The payee-reading half
   of payments lives in `dash-dml` (D2). EV3 resolves the superblock hook but does not own
   governance state (D3).

---

## 1. Core call map

### 1.1 ConnectBlock order (the caller is CH7a; EV3 provides each step)

| Step | Core | EV3 function | Called by |
|---|---|---|---|
| Pool before the block, as EV1's index set | `stm:846-851` (`GetCreditPool(pindex->pprev)` when V20 is active at the block, inside the `try` at `stm:805`) | `creditpool::pool_after(prev)` → `&RangesSet` as `UnlockIndexes` | CH7a, passing it to EV1's `SpecialTxCtx` |
| Credit-pool diff and CbTx balance | `stm:882-888` → `CheckCreditPoolDiffForBlock` `stm:1036-1067` → `GetCreditPoolDiffForBlock` `cp:376-405` | `creditpool::check_block_diff` | CH7a, only when the CbTx was read (`fCheckCbTxMerkleRoots`, `stm:816`) |
| MNHF signals | `stm:973-976` → `CMNHFManager::ProcessBlock` `mh:210-262` | `mnhf::MnhfManager::process_block` | CH7a, after the CbTx roots |
| Value check | `val:2841-2850` → `IsBlockValueValid` `pay:313-388` | CH3a's `block_value_valid`; EV3 resolves `Ev3Branch::ValidSuperblock` | CH3a / CH7a |
| Payee check | `val:2859-2865` → `IsBlockPayeeValid` `pay:392-441` → `IsTransactionValid` `pay:227-260` | CH3a's `block_payee_valid`; EV3 resolves `Ev3Branch::MasternodePayee` and `ValidSuperblock` | CH3a / CH7a |
| Undo | `stm:1015` → `CMNHFManager::UndoBlock` `mh:264-283` | `mnhf::MnhfManager::undo_block` | CH7a |
| Pool after the block | built lazily by the next reader (`cp:250-269`) | built eagerly at connect (D8) | EV3, inside the step above |

`ProcessSpecialTxsInBlock` (`val:2663-2668`) runs before the payment checks (`val:2841`).
`fCheckCbTxMerkleRoots` is `fScriptChecks` (`val:2542`, `:2664`), so under assumevalid neither the
CbTx nor the credit-pool diff runs (R4 F2). `RollforwardBlock` passes `false` too (`val:5197-5200`).

### 1.2 Payments (`dash_evo::payments`, `dash_dml::payments`)

| Core | Lines | Port |
|---|---|---|
| `FindUnmatchedMasternodePayment` | `pay:24-52` | `dash_evo::payments::find_unmatched_masternode_payment(expected, actual, strict) -> Option<usize>` |
| `PlatformShare`, `GetMasternodePayment` | `pay:54-60`, `:62-129` | existing `dash_consensus::payments` |
| `GetBlockTxOuts` | `pay:131-199` | `dash_dml::payments::block_tx_outs(list, rules, &BlockReward, params) -> Result<BlockTxOuts, PayeeError>` (`BlockTxOuts { outputs, computed }`: `computed` is Core's return value, D5) |
| `GetOwnerPayouts`, `RewardScript`, `IsShared` | `evo/providertx.h:94`, `:80`; `evo/dmnstate.h:157` | `dash_dml` state accessors (EV2a types) |
| `IsTransactionValid` | `pay:227-260` | `dash_dml::payments::masternode_payee_valid` (resolves `Ev3Branch::MasternodePayee`) |
| `CSuperblock::IsValidBlockHeight` | `sb:119-` | existing `superblocks.is_superblock_height` |
| `CSuperblock::GetPaymentsLimit` | `sb:145-163` | existing `dash_consensus::subsidy::superblock_payments_limit` (CS2, quirk 15) |
| `CSuperblock::IsValid` | `sb:248-337` | `dash_evo::payments::superblock_valid(coinbase, height, payments, block_reward, is_v24, params)` |
| `SuperblockManager::IsValidSuperblock` | `sb:622-631` | `GovernanceView::sb_payments` → `superblock_valid`; `None` is invalid |
| `GetBestSuperblockInternal`, `IsSuperblockTriggered`, `GetSuperblockPayments` | `sb:556-576`, `:578-620`, `:633-666` | behind `GovernanceView` (D3; GV1a/GV1b implement it) |
| `FillBlockPayments` | `pay:444-474` | not EV3's: MN0 (the miner) calls `block_tx_outs` and `sb_payments` |
| `IsSuperblockValidationRequired` | `evo/chainhelper.cpp:41-52` | stays CH3a's `sb_required` helper |

### 1.3 Credit pool (`dash_dml::creditpool`)

| Core | Lines | Port |
|---|---|---|
| `CRangesSet` (`WrappedSuccessor`, `Add`, `Contains`, `Size`, `IsEmpty`, `RangeCount`) | `rs:13-16`, `:27-58`, `:103-111`, `:92-101`, `:87-90`; `rsh:31-48, 79` | `RangesSet` methods on EV0's `BTreeSet<(u64, u64)>`, whose tuple order is `Range::operator<` (`rsh:37-41`) |
| `CRangesSet::Remove` | `rs:60-85` | not ported: no caller outside tests at the pin (D11) |
| `CRangesSet` codec (`Serialize`, `UnserializeBounded`, `DEFAULT_MAX_RANGES`) | `rsh:53, 86-131` | `RangesSet::{encode, decode}` with the five decode failures (D11) |
| `CCreditPool` | `cph:39-58` | EV0's `CreditPool` (+ codec) |
| `GetDataFromUnlockTx` | `cp:31-47` | private helper, its two reasons |
| `GetCreditDataFromBlock` | `cp:58-108` | `credit_block_data(block, dip3_active_at) -> Result<Option<BlockCredit>, PoolError>` (the data and the block's sorted indexes), computed at connect; the data is stored (D9) |
| `ConstructCreditPool` | `cp:167-241` | `construct_pool(prev, block: Option<&BlockCredit>, window_start, eras) -> Result<CreditPool, PoolError>`; `connect_records(block, at, prev, window_start)` builds what a connect writes |
| `UnlockLimitV24`, `LimitAmount{Low,High,V22}`, `MaxUnlockDropPercentV24`, `MinUnlockDropV24` | `cph:130-159` | `const fn unlock_limit_v24` and constants |
| `GetFromCache`, `MaybeWriteSnapshot`, `AddToCache`, `GetCreditPool` | `cp:116-165`, `:250-269` | replaced by the store's per-block pool (D8), read by `pool_after(store, block, v20_active_at)`; `CreditPoolCacheSize`, `DISK_SNAPSHOT_PERIOD` are not modelled |
| `GetBalanceAt` | `cp:243-248` | `balance_at(store, block, dip3_active_at)` over `credit_data_at`, the stored per-block data (D9) |
| `CCreditPoolDiff` (ctor, `Lock`, `Unlock`, `ProcessLockUnlockTransaction(s)`, `GetTotalLocked`) | `cp:279-374`; `cph:71-115` | `CreditPoolDiff`, same names; the package form for MP1a |
| `GetCreditPoolDiffForBlock` | `cp:376-405` | `credit_pool_diff_for_block` |
| `CheckCreditPoolDiffForBlock` | `stm:1036-1067` | `check_block_diff` (the CbTx balance, `bad-cbtx-assetlocked-amount`) |
| `getcreditpoolinfo` data | `rpc/blockchain.cpp:1789-1823` | `credit_pool_info(store, at, window_start, window_blocks)`: the pool after `at` (or the stored exception) and the window start's height and balance; the RPC is the RP task's |
| `GetAssetUnlockFee` (`bad-txns-assetunlock-fee-outofrange`) | `evo/assetlocktx.cpp:255-267`, called from `consensus/tx_verify.cpp:166` | already ported by CS5b (`dash-consensus/src/tx/inputs.rs:141`); nothing to add (D22) |

### 1.4 MNHF (`dash_dml::mnhf`)

| Core | Lines | Port |
|---|---|---|
| `GetRequestId` | `mh:32-35` | EV1 (`CheckMNHFTx`); quirk 31's int64 bit |
| `GetSignalsStage` | `mh:61-92` | existing `Deployments::ehf_signals_stage` (CS4) |
| `extractEHFSignal`, `extractSignals` | `mh:176-188`, `:190-208` | private helpers over the cached payload decodes |
| `ProcessBlock` | `mh:210-262` | `MnhfView::process_block(block, id, just_check) -> Result<MnhfProcessed, MnhfError>` (the stage, the block's signals and the row to write; `MnhfError` is a verdict or a store fault, D21); the per-bit checks are `mnhf::process_signals` |
| `UndoBlock` | `mh:264-283` | `MnhfView::undo_block -> Result<bool, StoreError>`: Core's `assert` becomes a store fault (D15) |
| `GetForBlock`, `GetFromCache`, `AddToCache`, cache (size 1000) | `mh:285-383`; `mhh:104-107` | `MnhfView::get_for_block` over the store rows plus the cache `MnhfManager` holds (D16, D17) |
| `CChainParams::IsValidMNActivation` | `chainparams.cpp:109-130` | `mnhf::is_valid_mn_activation` (D18) |
| `AbstractEHFManager` for versionbits | `versionbits.h:113` | `impl EhfSignalSource for MnhfView` (D20) |
| `AddSignal`, `ForceSignalDBUpdate` | `mh:385-409` | not ported: a unit-test helper and a legacy-row migration (D17) |

---

## 2. Data model and API sketch

```rust
// dash_evo::payments (pure, beside CH3a's functions)
pub fn find_unmatched_masternode_payment(expected: &[TxOut], actual: &[TxOut], strict: bool) -> Option<usize>;
/// CSuperblock::IsValid. `payments` is the best trigger's list, in order (GovernanceView::sb_payments).
pub fn superblock_valid(coinbase: &Tx, height: u32, payments: &[TxOut], block_reward: Amount,
                        is_v24: bool, p: &PaymentsParams) -> Result<bool, SubsidyError>;

// dash_dml::payments (reads Dmn)
pub struct BlockReward { pub height: i32, pub block_subsidy: Amount, pub fee_reward: Amount, pub era: MnRewardEra }
pub struct BlockTxOuts { pub outputs: Vec<TxOut>, pub computed: bool }   // D5
pub fn block_tx_outs(list: &MnList, rules: PayeeRules, reward: &BlockReward,
                     p: &PaymentParams) -> Result<BlockTxOuts, PayeeError>;
pub fn masternode_payee_valid(coinbase: &Tx, list: &MnList, rules: PayeeRules, reward: &BlockReward,
                              strict_multiplicity: bool, p: &PaymentParams) -> Result<bool, PayeeError>;

// dash_dml::creditpool
pub struct CreditBlockData { pub balance: Amount, pub unlocked: Amount }   // D9, stored
pub struct BlockCredit { pub data: CreditBlockData, pub indexes: Vec<u64> }   // GetCreditDataFromBlock
pub enum PoolError { PrevLocked, PrevIndexes, IndexDuplicated, NegativeLimit(Amount),
                     UnlockData(&'static str) }   // Core's exceptions, Display = what() (D8)
pub type PoolState = Result<CreditPool, PoolError>;   // EvoDelta::credit_pool (D8, ask A-1)
pub struct PoolEras { pub v24: bool, pub withdrawals: bool }   // DeploymentActiveAt(block, …)
pub fn credit_block_data(block: &Block, dip3_active_at: bool) -> Result<Option<BlockCredit>, PoolError>;
pub fn construct_pool(prev: &CreditPool, block: Option<&BlockCredit>,
                      window_start: Option<&CreditBlockData>, eras: PoolEras) -> Result<CreditPool, PoolError>;
pub fn connect_records(block: &Block, at: ConnectAt, prev: &CreditPool,
                       window_start: Option<&CreditBlockData>) -> CreditRecords;   // data + pool state
pub fn platform_reward(mn_rr_active_after_prev: bool, height: i32, block_subsidy: Amount,
                       p: &PaymentParams) -> Result<Amount, PaymentError>;
pub struct CreditPoolDiff { /* pool, new_indexes, session_locked, session_unlocked, platform_reward */ }
pub fn check_block_diff(block: &Block, cbtx_balance: Amount, prev_pool: &CreditPool,
                        platform_reward: Amount, dip8_active_at: bool, v20_active_at: bool)
                        -> Result<Option<Amount>, BlockValidationError>;
pub fn pool_after(store: &dyn EvoStore, block: &BlockHash, v20_active_at: bool)
                  -> Result<PoolState, StoreError>;   // GetCreditPool over the store
// EV1's `UnlockIndexes` (EV1 §2.3) does not exist yet: `RangesSet::contains` is its seam (BL-253).

// dash_dml::mnhf
pub struct MnhfManager { /* Core's 1000-entry cache; process-wide (D16) */ }
pub struct MnhfView<'a> { manager, params, chain: &dyn ChainAncestry, store: &(dyn EvoStore + Sync) }
impl MnhfView<'_> {
    pub fn process_block(&self, block: &Block, id: BlockId, just_check: bool)
                         -> Result<MnhfProcessed, MnhfError>;  // { signals, row }; Invalid | Store
    pub fn undo_block(&self, block: &Block, id: BlockId) -> Result<bool, StoreError>;
    pub fn get_for_block(&self, id: BlockId) -> Result<EhfSignals, StoreError>;
    pub fn signals_stage(&self, prev: Option<BlockId>) -> Result<EhfSignals, StoreError>;  // GetSignalsStage
}
impl EhfSignalSource for MnhfView<'_> { /* GetForBlock; a store fault panics (CS4) */ }
pub fn is_valid_mn_activation(params: &ChainParams, bit: u8, time_past: i64) -> bool;
```

The signatures are a sketch for the review; the slices fix them. Every reject is CS0's
`ValidationError` with Core's reason string and `BLOCK_CONSENSUS`, except the payment checks, which
CH3a maps to `bad-cb-amount` / `bad-cb-payee` with `BLOCK_RESULT_UNSET` (`val:2849, 2864`).

---

## 3. Slice plan

Each slice is one PR-sized branch commit series on `node/ev3-payments`, reported READY separately:
tests, clippy `-D warnings`, fmt, GATE-FAST tier 1, `bench_inputs`, then code-review-validator and
code-simplifier.

| Slice | Content | Depends on | Evidence at READY |
|---|---|---|---|
| **EV3-1 payments** | `find_unmatched_masternode_payment`; `superblock_valid`; `dash_dml::payments::{block_tx_outs, masternode_payee_valid}`; the two `Ev3Branch` resolutions as functions CH7a can call (D23); MNO-12's v24 gate test against CH3a's `old_budget_block_value_valid`; quirk 14 un-ignored | EV2b (merged), O2a-GV | L0 over `payments-v24.0.json.gz` (every sample, both networks, both formats); crafted rejects per string; F3 and F6 cases; L1 `diff_block_value_payee` smoke + plateau (DEC-184); quirk 14 |
| **EV3-2 credit pool** | `RangesSet` ops and codec (CONS-10); `CreditBlockData`; `construct_pool` in three eras; `CreditPoolDiff` (block and package forms); `check_block_diff`; `balance_at`, `credit_pool_info`; `UnlockIndexes` impl; the store record change (D9, ask A-1) | EV3-1 (shares `PaymentParams` use), CH7a ask A-1 for the chainstate codec | L0 vs `oracle_credit_pool_step` over the fixture's credit steps; L1 `diff_credit_pool` plateau; live `getcreditpoolinfo` walk from 1,987,776 (§4.3); CONS-10 two-index witness |
| **EV3-3 MNHF** | `MnhfManager` (`process_block`, `undo_block`, `signals_for_block`, cache), `is_valid_mn_activation`, `EhfSignalSource` impl; dash-dml's MNHF vectors switched from the reference model to `dash_dml::mnhf` | EV3-2 (none in code; order only), BL-211 (merged T19) | L0 `mnhf_step_vectors` on the port; `mnhf-regtest-v24.0.json.gz` walk; L1 `diff_mnhf_step` plateau; quirk 31 |
| **EV3-4 O17 and closure** | quirk register rows 14, 15, 31 set; CONS-10 / MNO-12 traceability in the conformance register; FR-CON-21/-22/-26/-27, FR-EVO-09 trace table; any review carry-over | EV3-1…3 | `check-quirks.py` counts; register rows; final evidence table |

FR-CON-26 (the shared-collateral covenant) is EV1's code (EV1 D7) and CH3a's call site; EV3's
part is the trace row in EV3-4 pointing at EV1's tests.

---

## 4. Oracle and differential plan

### 4.1 `oracle_block_value_payee` (EV3-1)

- **Native side.** `PaymentsParams`, `SuperblockCheck::for_block(required, v24)`, CH3a's
  `block_value_valid` / `block_payee_valid`, EV3's two resolutions, and `block_tx_outs` over the
  context's parent list. The governance view is a **recorded view** built from the call's context
  and the oracle's record: `gov_valid` = `governance_loaded`; `sb_triggered(h)` = the record's
  `triggered`; `sb_payments(h)` = the record's `superblock` vector when non-empty (D3). This tests
  every rule EV3 owns given Core's trigger answer; trigger selection and tallies are GV1a's.
- **Compared.** The verdict (`bad-cb-amount` / `bad-cb-payee` / accept), `check`, `era`, `strict`,
  `block_subsidy`, `value_valid` and `value_error` (Core's exact `strError`), `payee_valid`,
  `txouts_ok` and the `masternode` outputs in order; not-computed fields must be not computed on
  both sides (O2a-GV D16, D19). `ORACLE_E_ARG` / `ORACLE_E_CONTEXT` count as out of domain.
- **L0.** Every sample of `dash-oracle-sys/tests/data/payments-v24.0.json.gz` in every node state
  (20 samples, listed in its PROVENANCE.md; the trigger states only where a sample has a trigger,
  which `first-superblock`, before DIP3, has not): mainnet before and in the old budgets, the
  superblock start (614,819/614,820) and the first superblock (631,408), DIP3 enforcement
  (1,047,199/1,047,200), the Classic era's reallocation schedule (1,700,000), the last
  superblock before V20 (1,977,304), V20 (1,987,775/1,987,776) and a superblock and a regular
  block in the CreditPool era (2,077,000/2,077,001); testnet MN_RR (1,066,899/1,066,900) and a
  superblock and a regular block in the EvoReward era (1,500,000/1,500,001); and the `banned`
  and `strict` variants of O2a-GV D13. Mainnet's MN_RR (2,128,896) is above the 2,090,000 cap
  (DEC-276), so mainnet's EvoReward era has no sample. Plus one crafted reject per string
  (`bad-cb-amount` with each `strError`, `bad-cb-payee` from a missing masternode output, a
  missing superblock output, and the V24 duplicate rule). The live sweep of every 576th block
  from DIP3 and every superblock from 614,820 is O2a-GV's `payments-check.py --plan` acceptance
  of the oracle entry, not an EV3 test.
- **L1 `diff_block_value_payee`.** A node-l1 module and node-fuzz target. Inputs start from a
  fixture sample and mutate: the coinbase outputs (drop, duplicate, reorder, ±1 value), fees, the
  flags (`synced`, `governance_loaded`, ChainLock height), V24 seeded on testnet contexts only
  (O2a-GV D10), the payee's operator reward and payout lists (including ExtAddr version-3 states
  if the O2a context codec takes them; ask A-6), share splits, and synthetic triggers through
  `Trigger::superblock`. The generator never draws the arithmetic the oracle refuses (O2a-GV D6).
- **Format 2** (O2a-GOV D10, governance scripts) is used only by the GV1b gate. EV3 runs the
  format-2 regtest sample once as a smoke test of the recorded view.

### 4.2 `oracle_credit_pool_step` (EV3-2)

- **Native side.** `check_block_diff` on the parent's pool (`prev`), then `construct_pool` with the
  block's `CreditBlockData` and the window start's data, read from the step context's `blocks`
  (the same blocks the oracle serves through its read seam, O2a-GV D8, D9).
- **Compared.** The verdict and reject reason; `checked`, `has_expected`, `expected`
  (`GetTotalLocked`); `has_pool` and the pool (`locked`, `currentLimit`, `latelyUnlocked`, and the
  indexes as canonical bytes). `ORACLE_E_EXCEPTION` must be `GetCreditPool: ` and the `PoolError`'s text (D8).
- **L0.** The credit steps in the payments fixture, plus crafted cases: an unlock above the limit,
  a duplicated index in the pool and inside the block, the lock without an `OP_RETURN` output, a
  CbTx balance off by one, a block whose pool construction throws (a negative pre-WITHDRAWALS limit
  under a crafted prev, which only an unchecked block reaches) and the empty-CbTx path.
- **L1 `diff_credit_pool`.** Mutates `prev` (all four fields; ranges near 0 and 2^64 − 1 for
  CONS-10), lock and unlock outputs and payload fees, indexes and version (2: V24's stable txid,
  which the reject's text names), the CbTx balance, the era (V24 seeded off mainnet; the real
  eras are the steps' own), the coinbase's type, and the block's transactions (removed, repeated, a crafted asset
  lock), with the merkle root recomputed. DEC-184 plateau, DEC-186 certify. As landed
  (`node-l1/src/credit_pool.rs`): the window start's block is not mutated (its header is in the
  context's chain, so an edit means relinking every header above it); the live walk (§4.3) covers
  the window over real blocks. The crafted negative pre-WITHDRAWALS limit is a unit test only:
  the oracle refuses a non-empty `prev` before V20, and mainnet's only pre-WITHDRAWALS step is
  V20's activation. The steps are mainnet's V20 activation, a testnet block with an asset lock
  and unlock (WITHDRAWALS), and testnet 905,800, soon after its V20, whose window start holds
  little, so V24's 2000 DASH floor and the clamp to the balance meet Core
  (`l1_credit_pool::v24_floor_and_clamp_soon_after_v20`, BL-260). The clamp to zero needs a
  large window start and a small balance, which no step has; the unit tests cover it.

### 4.3 Live `getcreditpoolinfo` (EV3-2 acceptance)

`dash-dml/examples/credit_walk.rs`, driven by `dash-oracle-sys/scripts/credit-walk.py`: from height
1,987,776 to the oracle's tip on mainnet, then testnet from its V20 height 905,100. Per height it
reads `getblockhash`, `getblock <hash> 0` and `getcreditpoolinfo <h>`, all pure reads (O2a-GV D12),
at concurrency 1, and compares `balance`, `currentlimit`, `window.height`, `window.balance` and
`window.unlocked` with the native chain of `construct_pool` steps. The walk starts from the empty
pool at V20's height, as O2a-GV D12 does. It runs as one `systemd-run --user --unit=ev3-cpwalk` (mainnet,
then testnet, DEC-276 A-4), resumes from the example's checkpoint file in `/work/scratch/ev3/walk`
(which holds the used indexes the RPC does not print), re-reads the cookie on reconnect, waits
while the node lags, and opens nothing under `/work/chains` but the RPC cookie. The example also
checks each mined block's CbTx balance with `check_block_diff`.

Result (2026-10-20): mainnet 1,987,776 to 2,090,000 matched 102,225 of 102,225 heights, testnet
905,100 to 1,575,839 matched 670,740 of 670,740, with no mismatch and no reject. Era coverage:
the mainnet comparison cap (2,090,000) is below mainnet's MN_RR (2,128,896) and WITHDRAWALS
(2,201,472), so mainnet covers the pre-WITHDRAWALS, pre-MN_RR era only; the WITHDRAWALS limit
and the platform reward are proven live on testnet alone (MN_RR from 1,066,900). V24 was active
on neither oracle, so its limit is covered by the unit tests and L1 only (BL-261).

### 4.4 `oracle_mnhf_step` (EV3-3)

- **L0.** `dash-dml/tests/mnhf_step_vectors.rs` swaps `reference::step` for the port; the crafted
  and regtest vectors stay (O2a-MNHF D11). The Core-mined chain (`mnhf-regtest-v24.0.json.gz`)
  is walked through `MnhfManager` with store rows: every block's verdict, `stage_after` and
  `getdeploymentinfo` EHF heights equal (O2a-MNHF D9).
- **L1 `diff_mnhf_step`.** node-l1 module and node-fuzz target over format 2: deployments
  (start, timeout, useEHF on regtest's bits), the V20 flag and V20 boundary heights, MTPs at and
  around window edges, the stage rows, the bits in block order (repeats, held bits, bit 28
  reuse), `fJustCheck`, and `seed_tip_cache` at `V20Height − 1`. Compared: the verdict,
  `signals`, `stage_after`, `undo_ok` and `stage_undone`. A signal bit ≥ 29 is out of domain
  (O2a-MNHF D5); the same bit in a transaction that carries no signal is compared. Several
  deployments on one bit are not in the domain: the input keeps each deployment's bit, so
  `GetSignalsStage`'s loop over every deployment on a bit (`mh:77-86`) and the first-match rule
  of `IsValidMNActivation` are tested on the port only (`activation_by_window_and_use_ehf`). No
  network shares a bit at rc.3, and `-vbparams` cannot move one.
- **Landed (EV3-3).** L0 runs the crafted and regtest vectors through `MnhfView` with the stage
  as the previous block's row. The mined chain is walked in `node-l1/tests/l1_mnhf.rs`
  (`feature_mnehf_chain_on_the_node`): one manager, each block's row written to a `MemEvoStore`,
  the stage after every block equal to `getdeploymentinfo`'s `ehf_height` for the three
  deployments, each proposal's `fJustCheck` verdict (Core's `bad-mnhf-invalid` comes from
  `CheckMNHFTx`, so `ProcessBlock` alone reports the repeated bit as
  `bad-mnhf-duplicates-in-block`), and the invalidated branch connecting on its fork point. L1's
  input is `[tip, v20, flags, (op, a, b)*]` (`node_l1::mnhf::case_from_bytes`): tips 12–40, V20
  from 2 or from `tip − 1 … tip + 2`, ops for a signal, a trailing byte, another type, a row
  change, a seeded tip entry below V20, and each window's start, timeout and `useEHF`, with
  times relative to the block's MTP. The trailing-byte op also gives transaction versions 1, 2,
  4 and −1 (no payload field below 3) and payload versions 0, 2 and 255 (`extractEHFSignal`
  reads any). The seeded entry is set as the oracle sets it (`MnhfView::seed_cache`), so a seeded
  bit no deployment signals is compared too. An oracle decode refusal is a divergence (the
  crafted blocks always decode); only context and argument refusals are out of domain. For a
  connected block the row `process_block` gives is written and read back by a fresh manager,
  which must give `stage_after`; the walk does the same for every height from V20. A wrong row
  (`AddToCache` giving an empty row) fails both. `crafted_steps_agree` holds a case per reject
  reason and check order, and the 940 public seeds (`node-fuzz/public-seeds/diff_mnhf_step`)
  give 832 accepts, 108 rejects and 10 out of domain.

### 4.5 Quirks, leniencies, O17

| Item | Test |
|---|---|
| Quirk 14 (old-budget windows unbounded under NoCheck; superblock payees checked only when synced and not ChainLocked) | `conformance/tests/quirks.rs::quirk_14_old_budget_and_superblock_checks`, on mainnet block 332,320 (FR-CON-22) and a ChainLocked superblock |
| Quirk 15 (`GetPaymentsLimit`'s `nBits`) | CS2's test stands; EV3 adds the superblock-max path through `superblock_valid` |
| Quirk 31 (unknown bits valid; any `llmqTypeMnhf` quorum signs; int64 bit in the request id; historic bits 10–12) | `quirk_31_mnhf_signals`: the unknown-bit and historic-bit parts through `process_block`; the request id and signer parts through EV1's `check_mnhf_tx` once EV1-5 lands (ask A-3). EV3-3 lands the bit parts as `dash_dml::mnhf::tests::unknown_and_historic_bits_on_mainnet_and_testnet`; the scaffold stays until EV1-5, when EV3-4 moves the assertions into the quirk test and sets the row |
| F3 (no superblock/old-budget enforcement unless synced and not ChainLocked; IBD value bounds only) | L0 cases at both check levels |
| F6 (no payable masternode: Core's payee-check fallback) | O2a-GV's `banned` variant (`l1_payments::no_payee_accepts_any_payee`) |
| CONS-10 (`Wrapped`) | two-index witness: a stored `[b, 0)` range and an unlock at `b + 1` → `failed-creditpool-unlock-duplicated-index`, and in EV1's `bad-assetunlock-duplicated-index` through `UnlockIndexes` |
| MNO-12 (`WhenSyncedAndNotChainLocked`) | an old-budget-window block overpaying under `SuperblockCheck::NoCheck` from a ChainLock is valid; the same block synced and not locked is `bad-cb-amount` |

---

## 5. Decisions (D-rows)

| D | Decision | Disposition |
|---|---|---|
| D1 | **v24 only.** No `ReleaseProfile` field: CONS-10 is v24's `Wrapped` membership (`rs:103-111`), MNO-12 v24's `WhenSyncedAndNotChainLocked` gate (CH3a's `SuperblockCheck`, `val:2841-2843`). The v23.1.8 variants and the importer's acceptance of overlapping ranges are not modelled (DEC-82). | Proposed |
| D2 | **Module homes.** Rules over `TxOut` lists (`FindUnmatchedMasternodePayment`, `CSuperblock::IsValid`) go in `dash_evo::payments` beside CH3a's functions. `GetBlockTxOuts` and `IsTransactionValid` read the payee `Dmn` (operator reward, payout lists, shares), so they go in `dash_dml::payments`, as the addendum moved EV1 and the records (§1.5). The alternative, a `PayeeView` trait in `dash-evo`, has no second implementer: SPV does not validate coinbases. The PLAN row says `dash-evo::payments`; this is a path erratum. | **Needs manager ruling** (ask A-0) |
| D3 | **Governance split.** EV3 resolves `Ev3Branch::ValidSuperblock` through `GovernanceView::sb_payments` and `superblock_valid`. Trigger storage, parsing, funding tallies and `UpdateSentinelVariables` stay behind `GovernanceView`, implemented by GV1a/GV1b. EV3's oracle diff uses a recorded view from the oracle's own record (§4.1); real trigger sets (O2a-GV D20) are GV1b's acceptance. Until GV1b, the node has no governance data, so `gov_valid` is false and Core's unsynced answers apply. `CSuperblock::IsValid` appears in GV1a's PLAN text; EV3 lands it and GV1a reuses it. | **Needs manager ruling** (ask A-0) |
| D4 | `GovernanceView::sb_payments`' `Option<Vec<TxOut>>` carries what `IsValid` reads: each payment's script and amount, in order. `IsValidBlockHeight` and `GetPaymentsLimit` are recomputed from the parameters. | Proposed |
| D5 | **No payee.** `block_tx_outs` returns `BlockTxOuts { outputs, computed }`, Core's out-parameter and return value: only the platform output, computed, when the list is empty (`pay:153-156`); and what Core pushed before returning false (the platform output in the EvoReward era), not computed, when no masternode is payable (`pay:157-160`; EV3-1 review M2). `masternode_payee_valid` then accepts (`pay:237-240`), which is F6. The platform output comes first in the EvoReward era (`pay:142-151`), before the empty-list return. | Proposed |
| D6 | **Arithmetic.** `masternodeReward * nOperatorReward`, the owner-payout products and the superblock payment sums are signed `int64` in Core, and overflow is undefined behaviour there. They wrap here, as CH3a and `dash_consensus::payments` already do. No chain reaches them, and the oracle refuses them (O2a-GV D6). `PlatformShare`'s `assert(MoneyRange)` (`pay:57-58`) and `GetBlockTxOuts`' `assert(MoneyRange(masternodeReward))` (`pay:147`) are node faults (Core aborts), never verdicts. | Proposed |
| D7 | `CTxOut ==` is value and script bytes; `TxOut`'s derived `PartialEq` is the same. | Proposed |
| D8 | **The pool after a block is built when the block connects.** Core builds it lazily at the next read (`cp:250-269`) and caches it. It is a function of the parent's pool, the block's data, the window start's data and the eras, so building it eagerly gives the same value. Where Core's construction throws (`cp:173-179`, `:191-193`, `:232-235`, the reads of `cp:76-78, 98-100`), the stored state is that error. Each child then fails where Core's first read throws: `failed-procspectxsinblock` from the read at `stm:848` inside the `try` at `stm:805` (V20 is buried, so it is active at every child). `getcreditpoolinfo` and the mempool get the error, as Core's callers do. On a checked chain construction cannot throw: the diff rejects duplicate indexes and over-limit unlocks first. Only blocks connected without the CbTx checks (assumevalid, `RollforwardBlock`) can reach it. | Proposed |
| D9 | **Per-block credit data is stored, not re-read from block files.** `CreditBlockData { balance, unlocked }` is written for every DIP0003-active block: what `GetCreditDataFromBlock` (`cp:58-108`) returns where it returns data (a non-empty special-version `vtx[0]` payload that decodes as a CbTx; the `nType` is not checked, `cp:80-91`), and zeros where it returns nothing, as both readers map nothing to 0 (`cp:197-205`, `:247`). Where it throws, nothing is written: no checked chain holds such a block (`bad-assetunlocktx-payload` runs first), and a later read of it is a store fault where Core would throw. The pool's window start (`cp:195-206`) and `GetBalanceAt` (`cp:243-248`, the RPC's window balance) read it. A missing record where Core would read the block is a store fault, never a verdict. A block read failure in Core becomes a consensus reject (`stm:993-995`); a local fault must not mark a block invalid (EV2b D8, DEC-122). The block's own unlock indexes are taken from the block being connected and are not stored. | Proposed; ask A-1 (CH7a: an `EvoDelta` field and its chainstate codec) |
| D10 | `GetCreditDataFromBlock` collects indexes in an `unordered_set` (`cp:53, 102`), so a repeated index inside one block counts once for `Add` but its amount twice in `unlocked`. Ported as a sorted, deduplicated list; order does not matter because any failed `Add` throws. | Proposed |
| D11 | **`RangesSet`.** `Add`, `Contains`, `Size`, `IsEmpty` and `RangeCount` are ported verbatim with `WrappedSuccessor`. `Remove` (`rs:60-85`) has no caller outside Core's tests and is not ported. The codec is v24's: compact-size count ≤ `MAX_SIZE` (0x02000000), then `(begin, end)` pairs, rejecting the five cases of `rsh:98-119`. The store and importers decode with it. | Proposed |
| D12 | `CreditPoolDiff` ports the package form (`cp:350-374`) with its index rollback for MP1a. Blocks use the single-transaction form (`cp:384-393`). An exception inside the per-transaction step is `failed-procassetlocksinblock` (`cp:344-347`); one around the whole diff is `failed-getcreditpooldiff` (`cp:400-403`); `CheckCreditPoolDiffForBlock`'s own catch is `failed-checkcreditpooldiff` (`stm:1061-1064`). The port lists where each reachable `std::exception` comes from, with one test each, as EV1 does for `failed-check-special-tx`. | Proposed |
| D13 | **Limit eras**, decided at the block being built (`cp:210-221`): V24 (`DeploymentActiveAt(block, V24)`, versionbits) → `UnlockLimitV24(balance, window_start_balance)`; else WITHDRAWALS (buried) → `min(balance, 2000 DASH)`; else the windowed rule with its two clamps in Core's order. `latelyUnlocked` is computed in every era (`cp:209`). The caller passes the eras; EV3 never derives deployments. | Proposed |
| D14 | `CCreditPoolDiff`'s platform reward is computed when MN_RR is active after the parent (`cp:285-288`), from `GetMasternodePayment(height, subsidy, EvoReward)` (subsidy only, no fees). | Proposed |
| D15 | **MNHF rows.** `process_block` writes the block's signals as a row for every block where V20 is active (`mh:368-369`), with the store's derived-write rule: a different row already stored for the hash is a store fault carrying Core's message (`mh:374-377`), never a verdict. Rows are keyed by hash and kept on disconnect. `UndoBlock`'s `assert` (`mh:279`) is a store fault (`StoreError::Inconsistent`). | Proposed |
| D16 | **MNHF cache.** Core's cache (1000 entries, `mhh:104-107`) is ported with Core's insert points (`mh:337-357, 379-382`), including the entry `AddToCache` keeps only in memory for a block where V20 is not active (O2a-MNHF D13, D16). Its lifetime is the node process; it is never persisted. The manager instance is process-wide, not per attempt (ask A-2). | Proposed |
| D17 | **No rebuild from blocks.** Core's `GetForBlock` walks back and re-runs `ProcessBlock` on blocks read from disk when a row is missing (`mh:289-319`). The node writes a row for every V20-active block it connects, so for a block the node connected, a missing row where V20 is active is a store fault. A block it never connected (one an RPC names on a side branch) has no row: that read is not a consensus path, and its caller must answer without reaching the versionbits path, which panics on a missing row (D20; `getdeploymentinfo <hash>`, BL-270). The legacy key (`mnhf_s`, read before MN_RR, `mh:353-359`) and `ForceSignalDBUpdate` matter only for a Core datadir import: FR-EVO-09's "legacy/new key distinction" belongs to the evodb importer (ask A-5). | Proposed |
| D18 | `is_valid_mn_activation` (`chainparams.cpp:109-130`) lives once, in `dash_dml::mnhf`. EV1-5's `check_mnhf_tx` imports it (EV1 §2.1 placed a copy in `specialtx/mnhf.rs`). The `assert(nBit < VERSIONBITS_NUM_BITS)` is a precondition: `IsTriviallyValid` rejects such bits first (O2a-MNHF D5). | Proposed; ask A-3 |
| D19 | **One signals type.** `dash_dml::mnhf::MnhfSignals` (EV0's `BTreeMap<u8, i32>`) becomes `dash_consensus::deployments::EhfSignals`, which is already Core's `std::map<uint8_t, int>` with `insert` keeping an existing entry (`mh:247`). The source then needs no conversion. Both are EV0/CS4 types with no persisted data yet. | Landed EV3-3: the store's `mnhf_signals` row is an `EhfSignals` |
| D20 | **The node's `EhfSignalSource`.** `MnhfView` (the process-wide `MnhfManager` with the params, chain and store it reads) implements `signals_for_block` as `GetForBlock`, and CH7a replaces `NoEhfSignals` in `dashd-chainstate/src/chain/deployments.rs` with it. The manager holds only the cache, so the view is built per call. CS4's contract stands: an unreadable row panics, because a fallback would poison the versionbits cache. | Landed EV3-3; ask A-2 |
| D21 | **Error classes.** Verdicts are Core's reasons (`BLOCK_CONSENSUS` for the credit pool and MNHF). Core's `EvoDbInconsistencyError` paths (`stm:989-992`, `mh:252-256`, `cp:395-399`) are `StoreError` faults. Store read errors propagate as faults. A missing record is a store fault, never a verdict: `process_block` reads the parent's stage through `MnhfView::signals_stage` (`Deployments::ehf_signals_stage_from`, a fallible read), so a missing row reaches the caller as `MnhfError::Store`; only the versionbits path (`EhfSignalSource`, D20) panics. Core's `catch (const std::exception&)` arms (`mh:257-260`, `failed-proc-mnhf-inblock`; `stm:993-995`, `failed-procspectxsinblock`) are deliberately not ported: store and read faults are `StoreError`, never a reject reason. | Landed EV3-2, EV3-3 |
| D22 | `bad-txns-assetunlock-fee-outofrange` is CS5b's and already ported. EV1 D8's assignment of it to EV3 is void. | Erratum for EV1 D8 |
| D23 | **Entry points for CH7a.** EV3 exposes plain functions for the two `Ev3Branch` arms: CH7a's `steps.mn_payment_valid(...)` (`serial.rs:715-731`) calls `masternode_payee_valid` with the parent's list and `PayeeRules` after the parent, and the `ValidSuperblock` arm calls `superblock_valid` over `gov.sb_payments(h)`. Each arm keeps CH3a's refusal until CH7a wires it. | Proposed |
| D24 | **RPC data.** `credit_pool_info(at)` returns the pool after `at`, the window length, and the window start's height (or none) and balance (`rpc/blockchain.cpp:1802-1820`). Formatting is the RPC task's. | Proposed |
| D25 | **Live-run scope.** The live walk compares the credit pool only. Payee outputs are compared against mined coinbases at the fixture's sampled heights (L0) and in L1. A native live payee walk would need every parent list (`protx listdiff`), which EV2b's replay windows already exercise. | Proposed |

---

## 6. Asks

| Ask | To | What |
|---|---|---|
| A-0 | manager | Rule on D2 (the `dash_dml::payments` path) and D3 (GV1a/GV1b own the governance state; EV3 owns `CSuperblock::IsValid`). Both edit PLAN text: EV3's module column and GV1a's "IsValid". |
| A-1 | CH7a (storage owner) | `EvoDelta` gains `credit_data: Option<CreditBlockData>`. `credit_pool` becomes the D8 state, the pool or Core's construction error, written for every V20-active block. These need the chainstate column and codec. EV3 lands the `dash-dml` side (`MemEvoStore`) in EV3-2; the `dashd-chainstate` codec is CH7a's, or EV3's if the manager prefers. |
| A-2 | CH7a | Wire: (a) the pool read before EV1's loop (`stm:846-851`) and `check_block_diff` (`stm:882-888`) in Core's order; (b) `MnhfManager` as a process-wide instance (D16), `process_block` after the CbTx roots (`stm:973`) with `MnhfProcessed::row` into the block's `EvoDelta::mnhf_signals`, and `undo_block` first in the undo (`stm:1015`), where `Ok(false)` fails the disconnect as Core's `false` does (`stm:1015-1016`), never dropped. The port inserts the cache entry before the store takes the row, where Core's `WriteDerived` runs first and aborts before the cache is touched (`mh:368-382`): this is safe only because the node stops on an MNHF store fault, as Core's `AbortNode` does, so the wiring must stop the node on that `StoreError` and never continue past it. EV1's `check_mnhf_tx` must run on every transaction before `process_block` (`stm:853-871` before `:973`): `process_block` rejects a bit at or above `VERSIONBITS_NUM_BITS` as `bad-mnhf-nbit-out-of-bounds` (`mh:99-101`), which keeps that order's verdict, but the version and signature checks are only `check_mnhf_tx`'s; `MnhfError::Store` is a fault, not a verdict; (c) `EhfSignalSource` in place of `NoEhfSignals` (D20); (d) the two payment arms (D23). |
| A-3 | EV1 | `is_valid_mn_activation` comes from `dash_dml::mnhf` (D18). EV3-2 provides `impl UnlockIndexes for RangesSet`. EV3 lands it against EV1's trait if EV1-5 has merged by then; otherwise it is `RangesSet::contains` and the impl lands with EV1-5. Quirk 31's signer and request-id parts are tested through EV1-5. |
| A-4 | manager | Confirm `getcreditpoolinfo <h>` is on the pure-read list for the live oracles (O2a-GV D12 argues it is; ORACLE.md's list does not name it). Expect the walk to run for several hours: about 430k mainnet blocks at concurrency 1. |
| A-5 | dash-coreimport owner (CH6) | The evodb importer reads `mnhf_s2` and, before MN_RR, the legacy `mnhf_s` (`mh:348-359`, FR-EVO-09), `cpm_S` snapshots and `CRangesSet` with v24's strict decoder. It must produce `CreditBlockData` for the window, which Core does not store, from the imported blocks. |
| A-6 | O2a-GV owner | Does the O2a chain context's `mn_list` encoding take version-3 (ExtAddr) states with payout lists? If not, the owner-payout overflow guard (O2a-GV D17) stays review-checked, and L1 covers multi-payout lists natively only. A context codec extension would be a new ABI under DEC-238. |

---

## 7. Possible follow-ups (not in EV3)

- `FillBlockPayments` (`pay:444-474`) for MN0's template.
- The real `GovernanceView` (GV1b) and its gate on format-2 governance scripts.
- `getcreditpoolinfo`, `getassetunlockstatuses` (RP), with `credit_pool_info` and `RangesSet::contains`.
