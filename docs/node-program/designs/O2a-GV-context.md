# O2a-GV: the payments and credit-pool contexts of the oracle (ABI 18)

The context format of EV3's two oracle prerequisites (PLAN row O2a-GV, DEC-70): `oracle_block_value_payee` and
`oracle_credit_pool_step`. They are reviewed with EV3's brief. Both are v24-only (DEC-82). Each takes a block on top of
an O2a chain context (`oracle.h`, "chain contexts", format 2) plus a second, entry-specific context. The implementation
decisions are in `O2a-GV-notes.md`. The authoritative byte formats are the comments in Core's `src/test/oracle/oracle.h`
on branch `node/o2a-gv-oracle-v24.0`.

Line numbers cite Dash Core v24.0.0-rc.3. None of the cited files changed between rc.3 and the oracle pin (only the
oracle seams did).

## What v24 reads

ConnectBlock (`validation.cpp:2838-2865`) checks payments with three inputs that come from outside the block and the
chain:

1. **The superblock check level** (`SuperBlockCheckType`). It is `NoCheck` when
   `CChainstateHelper::IsSuperblockValidationRequired()` is false (`evo/chainhelper.cpp:41-52`): the block is at or below
   the best ChainLock, or the node is not masternode-synced (`CMasternodeSync::IsSynced()`). Otherwise the level is
   `DisallowDuplicates` once V24 is active after the parent, else `AllowDuplicates`. NoCheck carries two leniencies.
   - R4 F3 and MNO-12: `IsOldBudgetBlockValueValid()` accepts any coinbase value inside an old-budget window
     (`masternode/payments.cpp:276-287`).
   - Superblock payees are not checked (`:357`, `:416`).
2. **The governance state**: `governance::SuperblockManager` (`governance/superblock.h:132-187`), with its loaded flag
   (`IsValid()`, set from `CGovernanceManager::LoadCache()`) and its trigger objects. `IsSuperblockTriggered()` and
   `GetBestSuperblock()` count each trigger's funding votes against the masternode list at the chain tip
   (`GetListAtChainTip()`). The votes are weighted by masternode type. The threshold is
   `max(nGovernanceMinQuorum, weighted count / 10)` (`governance/object.cpp:964-997`).
3. **The fees** of the block (`nFees`).

Everything else is Core's chain state, which the O2a chain context already holds:

- the parent's masternode list (`GetListForBlock(pindexPrev)` and the payee);
- the deployments: V20 and MN_RR, which are buried and decide the reward era, and V24, a version-bits deployment that
  the context seeds, which decides `strict_multiplicity` and the superblock duplicate rule. On mainnet V24 is
  `NEVER_ACTIVE` at the pin (`chainparams.cpp:213`), so versionbits returns FAILED before it reads the seeded state:
  v24's rules are reachable on testnet, devnets and regtest only (D10);
- the parent's `nBits` and height, for `GetBlockSubsidy()`.

**v24 reads no spork on this path.** v23.1's `SPORK_9_SUPERBLOCKS_ENABLED` gate is gone. The PLAN row's "spork values"
item applied to the v23.1 pin, which DEC-82 dropped. SP0's spork semantics therefore do not enter this context (D3).

## `oracle_block_value_payee`

```
int oracle_block_value_payee(block, ctx, pay_ctx, out, *out_len, oracle_validation* state)
```

- `block`: the block to check (`CBlock`, network serialization). It must extend the chain context's tip and have a
  coinbase. ConnectBlock reads only `vtx[0]` here.
- `ctx`: an O2a chain context whose tip is the block's parent, with the parent's masternode list (`mn_list`) once
  DIP0003 is active at the parent. V24's state comes from its `deployments`.
- `pay_ctx`: the payments context (format 1, Core's serializers):

  | field | type | meaning |
  |---|---|---|
  | `format` | `uint8_t` | 1 |
  | `fee_reward` | `int64_t` | the block's fees (ConnectBlock's `nFees`): `0 .. MAX_MONEY` |
  | `synced` | `uint8_t` | `CMasternodeSync::IsSynced()` |
  | `best_chainlock_height` | `int32_t` | `Chainlocks::GetBestChainLockHeight()`, `-1` when there is none |
  | `governance_loaded` | `uint8_t` | `SuperblockManager::IsValid()`: the governance cache is loaded |
  | `triggers` | `std::vector<trigger>` | the trigger objects the SuperblockManager holds |

  A `trigger` is `std::vector<unsigned char> object` followed by `std::vector<vote> votes`.
  - `object` is a `CGovernanceObject` in network serialization (`Governance::Object`: `hashParent`, `revision`, `time`,
    `collateralHash`, `vchData`, `type`, `masternodeOutpoint`, `vchSig`), read whole. The trigger's JSON payment list
    is in `vchData`.
  - A `vote` is a `COutPoint` (the voting masternode's collateral), then `uint8_t signal` (`vote_signal_enum_t` 1-4),
    then `uint8_t outcome` (`vote_outcome_enum_t` 1-3). Each (collateral, signal) pair appears at most once, as in
    `CGovernanceObject::mapCurrentMNVotes`.

  The shim adds each object with `SuperblockManager::AddTrigger()`, as `CGovernanceManager` does. AddTrigger parses
  the payment list with Core's `CSuperblock` parser, so an object it refuses is not held, exactly as in a node. Votes
  are counted by Core against the parent's masternode list, which is the list at the chain tip of a synced node
  connecting at its tip (D4). Trigger objects are not validated as governance objects: their signatures and collateral
  are not checked. The payments code never reads them (D5).

The shim computes the block's subsidy (`GetBlockSubsidy(pindex)`), the era (`GetMnRewardEraAfter`), V24
(`GetSpecialTxRules`) and the check level (the two lines of IsSuperblockValidationRequired). It then runs Core's
`IsBlockValueValid`, `IsBlockPayeeValid`, `GetBlockTxOuts`, `IsSuperblockTriggered` and `GetSuperblockPayments` on one
`CMNPaymentsProcessor` and `SuperblockManager` built for the call.

**Verdict.** The verdict is ConnectBlock's.
- `ORACLE_OK`: both checks pass.
- `ORACLE_REJECT` with `bad-cb-amount` when IsBlockValueValid fails.
- `ORACLE_REJECT` with `bad-cb-payee` when only IsBlockPayeeValid fails.

Both rejections are `BLOCK_RESULT_UNSET`, with no debug message. ConnectBlock stops at `bad-cb-amount`
(`validation.cpp:2846-2849`), and so does the shim: when IsBlockValueValid fails, the payee check, the payee reward
refusals and the outputs do not run (D19).

**Output record** (Core's serializers):

| field | type | Core |
|---|---|---|
| `check` | `uint8_t` | the `SuperBlockCheckType` ConnectBlock passes (0 NoCheck, 1 AllowDuplicates, 2 DisallowDuplicates) |
| `era` | `uint8_t` | `MnRewardEra` (0 Classic, 1 CreditPool, 2 EvoReward) |
| `strict` | `uint8_t` | `special_tx_rules.v24`, the `strict_multiplicity` argument |
| `block_subsidy` | `int64_t` | `GetBlockSubsidy(pindex)` |
| `value_valid` | `uint8_t` | `IsBlockValueValid()` |
| `value_error` | `std::string` | its `strErrorRet` (the superblock and old-budget strings) |
| `payee_valid` | `uint8_t` | `IsBlockPayeeValid()`; 0 when `value_valid` is 0 (not run) |
| `outputs_computed` | `uint8_t` | whether the four fields below were computed |
| `txouts_ok` | `uint8_t` | `GetBlockTxOuts()`'s return: false when the list has masternodes but no payee (all PoSe-banned, R4 F6) |
| `masternode` | `std::vector<CTxOut>` | its outputs: the platform share (EvoReward era), then the owner payouts or share splits, then the operator reward |
| `triggered` | `uint8_t` | `IsSuperblockTriggered(tip list, height)` |
| `superblock` | `std::vector<CTxOut>` | `GetSuperblockPayments()`: the best trigger's payments when triggered, else empty |

`GetBlockTxOuts` and the superblock outputs are what `FillBlockPayments()` would add to a template. They are reported
whatever the check level, so a differential test compares the expected outputs on every block whose value passes. They
are not computed (`outputs_computed` 0; `txouts_ok` 0, both vectors empty, `triggered` 0) when the value check fails
(D19), or when DIP0003 is active at the tip and the context holds no list, so that only Core's own reads can need the
list (D16).

**Refused** (`ORACLE_E_ARG`). The shim refuses inputs on which Core's arithmetic would be undefined behaviour, and states
no chain produces (D6):
- a block whose first transaction is not a coinbase, or whose output values are outside the money range (CheckBlock
  and CheckTransaction reject them first);
- a fee outside `0 .. MAX_MONEY`;
- when DIP0003 is active at the tip, a parent-list payee with an operator reward above 10000, owner payouts above
  `MAX_REWARD` in total, or share amounts outside the money range or summing to 0; or one whose masternode reward times
  its operator reward (with an operator payout script), or the reward left after it times an owner payout's reward
  (all but the last), overflows `int64`;
- a trigger whose event height plus `min(576, nSuperblockCycle)` overflows `int` (`CSuperblock::IsExpired()` of a
  Valid trigger), or whose payments, or payments plus the block reward, overflow `int64`;
- repeated (collateral, signal) votes, a signal or outcome out of range, two objects with one hash, and context flags
  other than 0 or 1.

As for every context, a list read the context lacks is `ORACLE_E_CONTEXT`.

## `oracle_credit_pool_step`

```
int oracle_credit_pool_step(block, ctx, step_ctx, out, *out_len, oracle_validation* state)
```

This is one block of the credit pool, as a node runs it.
1. While connecting the block, `CSpecialTxProcessor::CheckCreditPoolDiffForBlock()` (`evo/specialtxman.cpp:1058-1089`)
   checks the block's asset locks and unlocks against the pool after the parent. It also checks the CbTx's
   `creditPoolBalance`.
2. `CCreditPoolManager::GetCreditPool(block)` (`evo/creditpool.cpp:250-269`) then builds the pool after the block from
   the parent's. ConnectBlock of the next block calls it.

Step 2 reads the block and its window-start block (height − `CreditPoolPeriodBlocks()`) from disk, through
`GetCreditDataFromBlock()`.

- `block`: the block (network serialization). It extends the context's tip, and its transactions match its header
  (merkle root, not mutated).
- `ctx`: an O2a chain context whose tip is the parent.
- `step_ctx` (format 1):

  | field | type | meaning |
  |---|---|---|
  | `format` | `uint8_t` | 1 |
  | `prev` | `CCreditPool` | the pool after the parent (`GetCreditPool(tip)`): `locked`, `currentLimit`, `latelyUnlocked`, `indexes` (a `CRangesSet`), Core's encoding; empty unless V20 is active at the parent |
  | `blocks` | `std::vector<CBlock>` | the node's disk: blocks of context headers that Core may read besides `block`, which is always on it (each one matching its header) |

  The shim seeds the parent's pool into a fresh `CCreditPoolManager`'s cache, as a node holds it. It serves
  `node::ReadBlockFromDisk()` from `block` and `blocks` through a `--wrap` seam (D8). A read of any other block fails the
  call with `ORACLE_E_CONTEXT`. When the check accepts and V20 is active at the block, `blocks` must hold the
  window-start block when DIP0003 is active there, whether or not Core reads it: `GetCreditPool(block)` reads it
  through `GetCreditDataFromBlock()`, which caches per-block data process-wide, so without this rule the refusal would
  depend on earlier calls (D9). A rejecting check needs no window block: the diff reads only `prev`, from the cache.

**Output record**:

| field | type | Core |
|---|---|---|
| `checked` | `uint8_t` | CheckCreditPoolDiffForBlock ran: the coinbase is a CbTx (`TRANSACTION_COINBASE`) |
| `has_expected` | `uint8_t` | the diff exists: DIP0008 and V20 are active at the block, and `GetCreditPoolDiffForBlock()` accepted its transactions |
| `expected` | `int64_t` | its `GetTotalLocked()`: the `creditPoolBalance` the CbTx must carry |
| `has_pool` | `uint8_t` | the pool was built (only on `ORACLE_OK`) |
| `pool` | `CCreditPool` | `GetCreditPool(block)`: `locked` is `getcreditpoolinfo`'s `balance`, `currentLimit` its `currentlimit` and `latelyUnlocked` its `window.unlocked` |

**Verdict.**
- `ORACLE_OK`: the check accepted the block, or did not run, and the pool was built.
- `ORACLE_REJECT` with `*state`: CheckCreditPoolDiffForBlock's reasons (`bad-cbtx-assetlocked-amount`, the
  `failed-creditpool-*` reasons, `failed-getcreditpooldiff`, `failed-checkcreditpooldiff`). The pool is not built after
  a rejection.
- `ORACLE_E_EXCEPTION`: `GetCreditPool()` threw (for example `failed-getcreditpool-index-duplicated`, or a negative
  limit). A node meets that exception at the next block, whose `ProcessSpecialTxsInBlock()` reads the pool first
  (`evo/specialtxman.cpp:870`), as `failed-procspectxsinblock` (`:1017`).

**Not run here.** The other steps of ProcessSpecialTxsInBlock: `CheckCbTx`, each asset unlock's `CheckSpecialTxInner`
(`oracle_check_special_tx_block` runs it) and the quorum block processor. The step runs as with script checks on;
under assumevalid a node skips the CbTx checks, credit pool included (R4 F2).

**Refused** (`ORACLE_E_ARG`):
- a block whose transactions do not match its header;
- a coinbase of type `TRANSACTION_COINBASE` whose payload `GetTxPayload` does not read (ConnectBlock has rejected it
  with `bad-cbtx-payload` before);
- a non-empty `prev` when V20 is not active at the parent, or a `prev` amount outside the money range;
- disk blocks without a coinbase, with an asset unlock whose payload does not decode, that are not context headers',
  repeat, or do not match their header (D15);
- asset locks or unlocks of one block totalling above `MAX_MONEY`, and a disk block's CbTx balance outside the money
  range.

Core's per-block credit data cache is process-wide and keyed by block hash, so a block must be the one its header
commits to (D8).

## How EV3 uses them

EV3's `dash_evo::payments` compares `is_block_value_valid` and `is_block_payee_valid`, its `SuperBlockCheck` level, and
the expected masternode and superblock outputs with `oracle_block_value_payee`, under the same check level.
`dash_dml::creditpool` compares the pool after each block, and the CbTx balance check, with `oracle_credit_pool_step`
(the L1 `diff_credit_pool` target). The Rust bindings are `Oracle::block_value_payee` (`PaymentsContext`, `Trigger`,
`GovVote`) and `Oracle::credit_pool_step` (`CreditPool`, `StepContext`) in `dash-oracle-sys`.
