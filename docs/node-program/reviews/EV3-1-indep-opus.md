# EV3-1 (payments): independent consensus review (Opus)

| | |
|---|---|
| Branch | `node/ev3-payments` at e1e1f52fc (base 4d85bfd4b): eb1610ad3, d41cdf261, c6ba6389c, e1e1f52fc |
| Core | v24.0.0-rc.3 (da567d7dba): `masternode/payments.cpp`, `governance/superblock.cpp` (v24's home of `CSuperblock::IsValid` / `GetPaymentsLimit`), `evo/providertx.h`, `evo/providertx_util.cpp`, `evo/deterministicmns.h`, `evo/chainhelper.cpp`, `validation.cpp:2841-2865` |
| Scope | `dash_evo::payments::{find_unmatched_masternode_payment, superblock_valid, valid_superblock}`; `dash_dml::payments::{block_tx_outs, masternode_payee_valid}`; node-l1 `diff_block_value_payee`; quirk-14 vectors; EV3-notes D1–D25 against DEC-276 |
| Ran | Seeded target `/work/scratch/ev3-1-review` (reflink of `/work/scratch/ev3/target`, source copy of the worktree at e1e1f52fc). Baseline green: dash-evo `payments` 16, `quirk_14_…` 1, dash-dml `payments` 7, node-l1 `l1_payments` 12 (`--features oracle`, plain pin, concurrency 1). No live oracle was read. Scratch deleted afterwards |
| Verdict | **ACCEPT-WITH-NITS** (no B items) |

## 1. Accept/reject parity, path by path

Each port line was read against Core at the tag.

- **`FindUnmatchedMasternodePayment`** (`pay:24-52`) → `dash-evo/src/payments.rs` `find_unmatched_masternode_payment`. Non-strict means existence in all of `actual`. Strict means a greedy first-unconsumed match. Both give the same index as Core. `TxOut ==` is value plus script bytes (D7).
- **`GetBlockTxOuts`** (`pay:131-199`) → `dash-dml/src/payments.rs:94-182`. The steps run in Core's order:
  1. `masternodeReward` comes from subsidy plus fees.
  2. In EvoReward, `PlatformShare(GetMasternodePayment(subsidy only))` is pushed **unconditionally** (a 0 value too) as `OP_RETURN` (0x6a), then `assert(MoneyRange)` becomes `PayeeError`.
  3. `GetCounts().total() == 0` returns true. `total` includes banned masternodes and undefined types (`types.rs:441-460`).
  4. With no `GetMNPayee`, it returns false and keeps the pushed outputs (D5, matching `pay:157-160` and what `FillBlockPayments` inserts).
  5. The operator part is taken only when `nOperatorReward != 0 && scriptOperatorPayout` is non-empty, from the reward **after** the platform share.
  6. `IsShared()` (shares non-empty, independent of version) splits with `SplitAmountByShares` and `RewardScript()`, falling back to the refund script.
  7. Otherwise `GetOwnerPayouts` applies: version ≥ 3 (`int` compare, as Core's `int nVersion`) uses `payouts` (empty pays nothing), and anything older uses `LegacyPayoutAsList`, which pays the whole reward to `scriptPayout`.
  8. Outputs of 0 or less are skipped, and the operator output comes last.
- **`IsTransactionValid`** (`pay:227-260`) → `masternode_payee_valid`. `!computed` accepts (F6, Core's fallback). Otherwise the unmatched check runs with V24's strict flag. The DIP0003 gate stays in CH3a's `block_payee_valid` (`height >= DIP0003EnforcementHeight`, same as `deploymentstatus.h:66-69`).
- **`CSuperblock::IsValid`** (`sb:248-337`) → `superblock_valid` (`dash-evo/src/payments.rs:572-620`):
  - the height check;
  - `nOutputs - nPayments < 0`;
  - `total > GetPaymentsLimit`;
  - `GetValueOut > reward + total`;
  - the ordered scan, restarting at the previous match before V24 (`max(nVoutIndex, 0)`) and just past it from V24 (`nVoutIndex + 1`).

  Each step matches Core. The order of the false-returning checks does not affect the boolean. `GetValueOut`'s throw becomes a documented panic that `CheckBlock` makes unreachable.
- **`IsValidSuperblock`** (`sb:622-631`) → `valid_superblock`: no best superblock (`sb_payments` returns `None`) is invalid.
- **Reject strings and classes.** `bad-cb-amount` and `bad-cb-payee` are both `BLOCK_RESULT_UNSET` (`val:2849, 2864`). **None of them is BLOCK_CONSENSUS.** node-l1 checks the class (`rej.result == BLOCK_RESULT_UNSET`), and production `serial.rs` uses `BlockValidationResult::Unset`. L1 compares Core's exact `strError`, including `invalid superblock detected at height %d`.
- **Era boundaries.** EV3's code only branches on `era == EvoReward` and the `strict` and `is_v24` flags. The era and V24 come from the caller (`MnRewardEra::after`, `SuperblockCheck::for_block`), and CH7a will wire them. In node-l1, era and V24 are derived from heights and the seeded V24 state, then compared field by field with Core's record (`era`, `strict`, `check`).

**Arithmetic.** CAmount is `i64` throughout. Divisions truncate toward zero, as in C++. The order is `x * n / d`, as Core's. `ProRataFloor` is the low 64 bits of the u128 product divided as unsigned, matching `arith_uint256`. A zero share total is `Err`, where Core throws. The superblock payment sum and `reward + total` wrap where Core's behaviour is undefined, which is unreachable within governance object sizes (D6). I found no parity defect.

## 2. Test discrimination: mutants

Each mutant was applied to the copy, run against the four suites, and the file restored (verified with `diff`). Results are listed as evo-unit / quirk14 / dml-unit / l1:

| Mutant | Change | Result | Killed by |
|---|---|---|---|
| A | `outputs.len() < payments.len()` → `<=` | FAIL / pass / pass / **pass** | only `superblock_is_valid` (`valid(vec![miner(100), x], &twice, false)`) |
| B | payments-limit check removed | FAIL / pass / pass / FAIL | unit + L1 public seed `[3,27,0,0,0,14,0,0]` (mainnet trigger with a duplicated payment exceeds the limit) |
| C | `counts().total()` → `enabled()` | pass / pass / FAIL / FAIL | dml unit + L1 `banned` |
| E | pre-V24 restart `j` → `j + 1` | FAIL / FAIL / pass / FAIL | all three |
| F | operator-script condition dropped | pass / pass / FAIL / FAIL | dml unit + L1 |
| G | `reward + total` → `reward + 2·total` | FAIL / pass / pass / FAIL | unit + L1 |

Every mutant dies. A has no Core-differential witness (N3).

Untested boundaries, as Core differential evidence, are in N1.

## 3. Interface

- `GovernanceView` errors propagate unchanged as `PaymentsError::Governance` (value check) or `G::Error` (payee check). Production `serial.rs` maps them to `Fatal`. `valid_superblock` turns `None` into invalid only as Core's "no best superblock". The contract (`payments.rs:102-110`) keeps "cannot answer" on `Err`, so a GV1b view that lacks data has no way to produce a verdict except by returning `None` wrongly. GV1b's tests must pin that.
- `PayeeError` (Core's asserts and throws) is never a verdict. node-l1 maps it to a divergence ("node fault").
- node-l1's recorded view: `NotRecorded` and probe failures are divergences. `Refused` (Core refuses the payee state on the probe) is `OutOfDomain`, a skip and not a fabricated verdict. That is acceptable.
- Production stays fail-closed. Both `Ev3Branch` arms still refuse in `serial.rs:695-697, 749-751`, and `no_evo::mn_payment_valid` is `Fatal` (D23, wired by CH7a).

## 4. Findings

### N1: The payments fixture lacks the Classic era, pre-V20 superblocks and every era boundary, but the notes claim them

- **Where:** `docs/node-program/designs/EV3-notes.md:182-186` (§4.1 L0) and `node-l1/src/payments.rs:5-10`. `dash-oracle-sys/tests/data/payments-v24.0.json.gz` has **9 samples**:
  - mainnet 100,000, 332,320, 332,420, 2,077,000, 2,077,001 ×2;
  - testnet 1,500,000, 1,500,001 ×2.
- **What is missing:** §4.1 (and O2a-GV's PLAN row) describe "the superblock window from 614,820, every 576th block from DIP3". None of these exist in the fixture:
  - any block in the **Classic era with the payee check active** (mainnet 1,047,200–1,987,775: about 940k blocks under the reallocation schedule);
  - any **pre-V20 superblock** (`GetPaymentsLimit` with `fV20Active = false`);
  - mainnet EvoReward (from 2,128,896);
  - any sample at or adjacent to the DIP0003-enforcement, V20, MN_RR or superblock-start heights.

  The 165 quirk-14 vectors replay only `IsBlockValueValid`, at 5 heights, and never the payee check.
- **Why it is a nit and not a B:** EV3's composition does not depend on the era. The Classic schedule (`GetMasternodePayment`, quirk 12) and `GetPaymentsLimit` (quirk 15) have CS2's Core vectors (`dash-consensus/tests/core_vectors.rs`, `oracle_corpus.rs`). CH7a's L2 samples "masternode payments" over 1,028,160→V20.
- **Failure scenario:** a defect in how CH7a feeds the era or rules into these functions at the V20 or MN_RR switch, or a Classic-era payout-shape difference, would first show up at CH7a's L2, not in EV3's gate.
- **Fix:** correct §4.1 and the node-l1 module doc to the fixture as it is. Then either regenerate the fixture through O2a-GV's generator with mainnet samples at:
  - 1,047,200 (and 1,047,199);
  - one block in the reallocation middle;
  - 1,977,332 (the last pre-V20 superblock: 614,820 + 82·16,616);
  - 1,987,775/1,987,776;
  - 2,128,895/2,128,896;

  or open a BL row that names these heights as CH7a L2 must-sample points.

### N2: `sb_triggered` is called at non-superblock heights, but its contract only defines superblock heights

- **Where:** `dash-evo/src/payments.rs:510` (`superblock_payee_valid`), trait doc `:96-100`.
- **The gap:** Core's `IsSuperblockTriggered` returns false at `sb:581-583`, **before** the lock and before `UpdateSentinelVariables`, at any height that is not a superblock height. The port asks the view at every height at or above `nSuperblockStartBlock` (once synced, governance loaded and `check != NoCheck`). The trait documents the answer and the side effects only "at a superblock height". This is CH3a code, but D3 hands this contract to GV1b.
- **Failure scenario:** a GV1b view that matches triggers by event height and runs the sentinel update without Core's height guard could mutate trigger state (cached funding, the deletion mark) at ordinary blocks. Core never does that, and later superblock answers would then diverge.
- **Fix:** guard the call with `is_superblock_height(height)` in `superblock_payee_valid`, which is equivalent to Core and removes the burden from the view. Or state in the trait doc that the answer is false, with no side effect, off superblock heights, and have GV1b test it.

### N3: The `nOutputs - nPayments < 0` boundary has only one native witness

- **Where:** `dash-evo/src/payments.rs:589`. Mutant A (`<=`) survives quirk 14, the dml tests and every L1 test, including 256 proptest cases and all public seeds. Only `superblock_is_valid`'s pre-V24 output-reuse assertion kills it.
- **Failure scenario:** a later refactor of the unit test (for example adding a miner output to that case) would let `<=` through. Before V24 that rejects a coinbase that Core accepts, one whose outputs are exactly the trigger's payments with one output reused for two payments.
- **Fix:** add a focused unit assertion with equal lengths in both modes. For example, `valid(vec![x, y], &[x, y], v24)` is true when the reward is 0 or the payments cover the value. Optionally add an L1 crafted case on the testnet superblock that trims the coinbase to its payments under F6 (`ban_payee` plus a dropped miner output).

### N4: Stale or inconsistent documentation

- `EV3-notes.md:7`: the status still reads "Phase 1 checkpoint (no code)".
- §1.2 and §2 show `block_tx_outs(...) -> Option<Vec<TxOut>>` and `masternode_payee_valid(..., branch: MasternodePayeeArgs, ...)`. The landed code is `Result<BlockTxOuts, PayeeError>` and `(coinbase, list, rules, &BlockReward, strict, params)`.
- `node-l1/src/payments.rs:3` says "ABI 18" while the notes header says the pin is ABI 28. State which is the entry's introduction ABI and which is the pin.
- The brief's `governance/classes.cpp` is `governance/superblock.cpp` at the tag. The notes already use `sb`, so this is only a reminder for anyone citing the old path.
- **Fix:** update the header, the sketches and the ABI wording.

## 5. Asked checks, answered

1. **Parity:** identical on every path read: the eras (as inputs), the platform `OP_RETURN` (unconditional, first), empty versus all-unpayable lists (F6), operator/owner/shared splits, superblock trigger present/absent/invalid/no-best, the payments-limit bound, duplicate and order handling before and after V24, and the reject strings and classes.
2. **Arithmetic:** identical (`i64`, truncating division in Core's order, u128 for `ProRataFloor`, wrapping only where Core has undefined behaviour).
3. **Discrimination:** 6/6 mutants killed. The boundary gaps are N1 and N3.
4. **Interface:** failures are errors, never verdicts (§3). N2 is the one contract hole.

No Dash Core security issue was found.

**Verdict: ACCEPT-WITH-NITS.**

---
Reviewer: Claude Opus 5.5 (independent), 2026-10-20.
