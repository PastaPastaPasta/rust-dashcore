> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# C — Block subsidy, masternode/treasury payments, superblocks, sporks, credit pool

Source of truth: Dash Core `develop` @ `15db478750` (`git describe`: `v24.0.0-rc.2-109-g15db478750`).
Mainnet currently runs **v23.1.x**, and V24 is `NEVER_ACTIVE` on mainnet in this tree. Differences from `v23.1.8` are called out where they matter.
Every rule below was read in the code; file:line refers to `/work/home/agent/workspace/dash` at that HEAD unless stated.
Heights are **block heights** (`h`). `nPrevHeight = h-1`.

---

## 0. TL;DR for the Rust design

1. **Subsidy** is a pure function of `(prev.nBits, prev.height, network, V20-active-at-h)`. It includes IEEE-754 double math and must use binary64 with no FMA contraction (§1). Size: **M** (the formula is small; proving it bit-exact over all history is the work).
2. **The MN payee/amount** is deterministic given the full deterministic MN list (DML) at `pindexPrev`. It is enforced from DIP3 enforcement height onward (§2, §3). Size: **L** (needs the full `CDeterministicMNState` machine, not SML).
3. **Superblock payee validation is NOT deterministic.** It depends on P2P governance objects and votes, on `mn_sync.IsSynced()`, on the best ChainLock height, on wall-clock time (trigger expiry) and on `-disablegovernance`. A node replaying from disk enforces **only an upper bound** at superblock heights. In the 12.0 "old budget" windows it enforces **no bound at all** (§4). ChainLocks override governance-based rejections (§4.7). Size: deterministic part **S**; full governance **XL** and optional.
4. **Sporks:** on mainnet they are hard-coded to "on" (`spork.cpp:173-181`). They are never read by deterministic block-validity code. They only affect ChainLock/IS-dependent behavior (§5). Size: **S**.
5. **Credit pool:** each CbTx (v3+) commits the balance, and that balance is recomputed exactly. It is coupled to the subsidy through the platform share (§6). Size: **M**.

---

## 1. Block subsidy (`src/validation.cpp`)

### 1.1 Code map
- `ConvertBitsToDouble(nBits)`: `validation.cpp:1785-1802`
- `GetBlockSubsidyHelper(nPrevBits, nPrevHeight, params, fV20Active)` returns the pair `{subsidy_after_treasury, treasury_part}`: `validation.cpp:1810-1871`
- `GetSuperblockSubsidyInner` returns `.second` (`:1873-1877`). `GetBlockSubsidyInner` returns `.first` (`:1879-1883`).
- `GetBlockSubsidy(pindex)` (`:1885-1890`):
  - genesis returns `GenesisBlock().vtx[0]->GetValueOut()`. This is never used for consensus: ConnectBlock skips genesis at `:2535-2541`.
  - otherwise it returns `GetBlockSubsidyInner(pindex->pprev->nBits, pindex->pprev->nHeight, params, DeploymentActiveAt(*pindex, V20))`.
  - **The difficulty is the PREVIOUS block's nBits and the height is the PREVIOUS height.** The comment at `:1804-1808` acknowledges this. The miner does the same (`node/miner.cpp:255-256`).

### 1.2 ConvertBitsToDouble (`validation.cpp:1785-1802`)
```
nShift = (nBits >> 24) & 0xff
dDiff  = 65535.0 / (double)(nBits & 0x00ffffff)
while nShift < 29: dDiff *= 256.0; nShift++
while nShift > 29: dDiff /= 256.0; nShift--
```
Multiplying or dividing by 256 is exact (a power of two), so the only rounding is the single division. A mantissa of 0 would give +inf. It cannot happen on a valid chain because nBits must match the difficulty algorithm.

### 1.3 The formula (`validation.cpp:1810-1871`)
1. **dDiff**
   - Mainnet only, when `nPrevHeight <= 4500`, the buggy form is used: `dDiff = 65535.0 / (nPrevBits & 0x00ffffff)` with **no exponent shift** (`:1815-1817`). The check uses `Params().NetworkIDString() == MAIN`, a global and not consensusParams.
   - Otherwise `dDiff = ConvertBitsToDouble(nPrevBits)` (`:1818-1820`).
2. **Base (whole DASH, `CAmount nSubsidyBase`)**
   - `force_fixed = fV20Active || (devnet && nPrevHeight >= nHighSubsidyBlocks)` (`:1822-1823`). If set, `base = 5` (`:1824-1831`).
   - else if `nPrevHeight < 5465`: `base = trunc(1111.0 / pow(dDiff+1.0, 2.0))`, clamped to [1, 500] (`:1832-1837`).
   - else if `nPrevHeight < 17000 || (dDiff <= 75 && nPrevHeight < 24000)`: `base = trunc(11111.0 / pow((dDiff+51.0)/6.0, 2.0))`, clamped to [25, 500] (`:1838-1843`).
   - else: `base = trunc(2222222.0 / pow((dDiff+2600.0)/9.0, 2.0))`, clamped to [5, 25] (`:1844-1850`).
   - **Truncation before clamping.** The double is implicitly converted to int64 (truncation toward zero) and then compared with the integer clamps. Reachable maxima: formula 1 is at most 1111 (hits the 500 cap); formula 2 is at most 153 (the 500 cap is dead); formula 3 is at most 26 (hits the 25 cap). None can overflow int64, so Rust's saturating `as i64` gives the same result.
   - Thresholds (my derivation):
     - formula 3 gives > 5 only while dDiff <= ~2877.2, and gives 25 while dDiff <= ~83.3
     - formula 2 gives >= 25 while dDiff <= ~75.5, which explains the `dDiff <= 75` era test
3. `nSubsidy = base * COIN` (`:1852`).
4. **Yearly decline** (`:1854-1857`): `for (int i = interval; i <= nPrevHeight; i += interval) nSubsidy -= nSubsidy/14;`
   - So `k = floor(nPrevHeight / interval)` integer steps of `s -= s/14`. Do **not** use a closed form.
   - The interval is `nSubsidyHalvingInterval`: 210240 on main/test/devnet and 150 on regtest (`chainparams.cpp:162,362,536,768`).
   - The first decline hits block **210241**, not 210240.
5. **Devnet high subsidy:** `if (nPrevHeight < nHighSubsidyBlocks) { assert(isDevnet); nSubsidy *= nHighSubsidyFactor; }` (`:1859-1862`). The defaults are 0 and 1 (`consensus/params.h:191-193`); `-highsubsidyblocks` and `-highsubsidyfactor` apply to devnet only (`chainparams.cpp:691-695,1215-1219`).
6. **Treasury:** `if (nPrevHeight > nBudgetPaymentsStartBlock) part = nSubsidy / (fV20Active ? 5 : 10);` and the function returns `{nSubsidy - part, part}` (`:1863-1870`).
   - This is a **strict `>` on the previous height**, so the treasury cut starts at block `nBudgetPaymentsStartBlock + 2`. Mainnet: 328010.
   - `IsBlockValueValid` switches to budget rules at `h >= nBudgetPaymentsStartBlock` (328008). Blocks 328008 and 328009 are in the budget era but still get the full subsidy.
   - Fees are **not** part of the treasury computation.

### 1.4 Floating-point gotchas
- **pow(x, 2.0):**
  - GCC and Clang fold `pow(x,2.0)` to `x*x` (exact, correctly rounded), and glibc `pow` returns the correctly rounded square in practice.
  - In Rust, write `let t = d + 2600.0; let q = t / 9.0; 2222222.0 / (q*q)`. Do not use a libm-dependent `powf`, although LLVM folds `powf(2.0)` to `fmul` as well.
  - There is no multiply-add pattern, so FMA contraction cannot occur. Make sure `-C target-feature` and the code do not use `mul_add`.
- Each operation is a binary64 IEEE op on x86-64 SSE2 and aarch64. Historic 32-bit x87 builds are not reproducible in principle; mainnet history evidently agrees with SSE2 semantics.
- Mainnet coinbase checks are `<=` (§4), so an error that makes Rust's subsidy *larger* is not detected by replay. Only a *smaller* subsidy breaks sync.
  - **Recommendation:** differential-test against Dash Core's `getblockstats h '["subsidy"]'` (`rpc/blockchain.cpp:2428` calls `GetBlockSubsidy`) for every mainnet and testnet height.
  - Unit vectors: `src/test/subsidy_tests.cpp:14-101`, mainnet. Examples (`nPrevBits` → `nPrevHeight` → subsidy):
    - `0x1c4a47c4` → 4249 → 500 DASH
    - same → 4249 with V20 → 5
    - `0x1c4a47c4` → 4501 → 56
    - `0x1c29ec00` → 5464 → 21
    - same → 5465 → 122
    - `0x1c08ba34` → 17588 → 61
    - `0x1b10cf42` → 99999 → 5
    - `0x1b10d50b` → 210240 → 4.64285715
    - same → 420480 → 3.88010205 (10% treasury) or 3.44897960 (V20)

### 1.5 Era table: mainnet (`chainparams.cpp:158-300`)
| Block h | Rule |
|---|---|
| 1 … 4501 | buggy dDiff (no shift) + formula 1, clamped [1, 500] |
| 4502 … 5465 | correct dDiff, formula 1 |
| 5466 … 17000 | formula 2, clamped [25, 500] |
| 17001 … 24000 | formula 2 if dDiff <= 75, else formula 3 |
| >= 24001 | formula 3, clamped [5, 25]. By block 100000 the base was already 5 (unit-test vector) |
| >= 210241 | first ×(1 − 1/14) step; one more step every 210240 blocks |
| >= 328010 | 10% treasury cut (`nBudgetPaymentsStartBlock` = 328008, strict `>` on prev height) |
| >= 1987776 (V20) | base forced to 5 (no difficulty dependence); treasury 20% |

Net-of-treasury subsidy with base 5 (a "gross" column is `nSubsidy` before the treasury cut; computed with the exact integer loop):

| k | blocks | gross | net, 10% | net, 20% (V20+) |
|---|---|---|---|---|
| 0 | 1-210240 | 5.00000000 | (4.5 after 328010; not in this range) | – |
| 1 | 210241-420480 | 4.64285715 | 4.17857144 | – |
| 2 | 420481-630720 | 4.31122450 | 3.88010205 | – |
| 3 | 630721-840960 | 4.00327990 | 3.60295191 | – |
| 4 | 840961-1051200 | 3.71733134 | 3.34559821 | – |
| 5 | 1051201-1261440 | 3.45180768 | 3.10662692 | – |
| 6 | 1261441-1471680 | 3.20524999 | 2.88472500 | – |
| 7 | 1471681-1681920 | 2.97630357 | 2.67867322 | – |
| 8 | 1681921-1892160 | 2.76371046 | 2.48733942 | – |
| 9 | 1892161-2102400 | 2.56630257 | 2.30967232 (to 1987775) | 2.05304206 (from 1987776) |
| 10 | 2102401-2312640 | 2.38299525 | – | 1.90639620 |
| 11 | 2312641-2522880 | 2.21278131 | – | 1.77022505 |
| 12 | 2522881-2733120 | 2.05472551 | – | 1.64378041 |

Pre-V20 mainnet blocks still use the difficulty-dependent base. The table assumes base 5. Whether every block after ~2014 was exactly base 5 depends on per-block dDiff > ~2877, which I could **not verify without chain data**.
- Example: the error string in `src/test/masternode_payments_tests.cpp:131` (from a real IBD failure, commit `2addaf83f6`) gives the limit at h=332320 as 5.08031847.
- If that were base 5 (net 4.17857144), it would imply 0.90174703 DASH in fees. If base 6 (net 5.01428573), it would imply 0.06603274 DASH in fees.
- So treat the base as **per-block**.

### 1.6 Era table: testnet (`chainparams.cpp:358-500`)
- There is no buggy-dDiff era (that is MAIN only).
- Formula eras use the same nPrevHeight thresholds: 5465, 17000/24000.
- Testnet difficulty is low, and min-difficulty blocks are allowed (`fPowAllowMinDifficultyBlocks=true`). A min-difficulty block has `nBits=powLimit` (0x1e0fffff → dDiff ≈ 0.000244), giving base 25 in formula 3.
- **So pre-V20 testnet subsidies vary block by block between 5 and 25.**
- Treasury from h >= 4102 (`nBudgetPaymentsStartBlock`=4100).
- V20 at h >= 905100: base fixed at 5, treasury 20%.
- Decline interval 210240.

### 1.7 Regtest / devnet
- **Regtest:** interval 150, budget start 1000, superblock start 1500, cycle 20 (`chainparams.cpp:768-780`). `V20Height = DIP0003Height = 432` and `MN_RRHeight = V20Height` (`:801-802`). These can be overridden by `-testactivationheight` (`:1021-1042`) and `-budgetparams` (`:972-976,1139-1150`).
- **Devnet:** V20Height=2 (`:567`); nHighSubsidy* from args.
- **Gotcha:** `rust-dashcore/dash-network/src/lib.rs:91-98` returns V20 = 0 for regtest and devnet. That is wrong for consensus purposes (Core: regtest 432, devnet 2).

---

## 2. Masternode payment amount (`src/masternode/payments.cpp`)

### 2.1 GetMasternodePayment(h, blockValue, params, era) (`payments.cpp:62-128`)
All arithmetic is int64. **Each increment is truncated separately** (`blockValue/20` and `/40`), so the "50%" level is *not* `blockValue/2`.
- `ret = blockValue/5` (20%).
- Add `blockValue/20` if `h > MNPI`, `h > MNPI+P`, `h > MNPI+2P`.
- Add `blockValue/40` if `h > MNPI+3P` … `h > MNPI+7P`, and `h > MNPI+9P`. Period **8 is skipped**; that is the historical quirk (`:71-79`).
  - Mainnet: MNPI=158000, P=17280. Thresholds: 158000, 175280, 192560, 209840, 227120, 244400, 261680, 278960, **313520** (each with strict `>`).
  - Testnet: 4030 + 10·{0..7, 9}, i.e. 50% from h >= 4121.
- If `h < BRRHeight`, return ret (`:81-84`).
- `nReallocStart = BRR - BRR % cycle + cycle` (`:86-88`). If `h < nReallocStart`, return ret (`:90-93`).
- If `era != Classic`, return `blockValue * 3 / 4` (`:95-100`). The **CreditPool** era (V20 active, MN_RR not) uses 75% too. The comment: treasury is 20%, so 75% of 80% = 60% of the gross subsidy.
- Otherwise use the BRR table (`:103-123`): `{513,526,533,540,546,552,557,562,567,572,577,582,585,588,591,594,597,599,600}`.
  - Index `min((h - nReallocStart)/(3*cycle), 18)`. Return `blockValue * per / 1000` (`:125-127`).
  - **Mainnet:** BRR 1374912 (1374912 % 16616 = 12400) gives realloc start **1379128**, period 49848 blocks.
    - Period starts: 1379128 (51.3%), 1428976, 1478824, 1528672, 1578520, 1628368, 1678216, 1728064, 1777912, 1827760, 1877608, 1927456, 1977304 (58.5%), …
    - V20 (1987776) cuts in during period index 12 = 58.5%.
    - Blocks 1374912..1379127 stay at 50%.
  - **Testnet:** BRR 387500 gives start 387504, period 72; 60% from 388800. V20 905100 switches to 75%.
- **Era selection:** `GetMnRewardEraAfter(pindexPrev)` (`validation.cpp:5893-5899`):
  - Classic if V20 is not active for h
  - CreditPool if V20 is active but MN_RR is not
  - EvoReward if MN_RR is active
  - Both are buried heights, `DeploymentActiveAfter`, i.e. `h >= Height` (`deploymentstatus.h:14-18`).

### 2.2 Fees and platform share (`payments.cpp:131-150`)
- `masternodeReward = GetMasternodePayment(h, blockSubsidy + feeReward, era)`. **MNs get the same percentage of fees.** `blockSubsidy` is already net of treasury. `feeReward` = sum of all non-coinbase fees (`validation.cpp:2716,2830`), including AssetUnlock `getFee()` (`consensus/tx_verify.cpp:165-167`).
- **EvoReward era:**
  - `platformReward = PlatformShare(GetMasternodePayment(h, blockSubsidy, era))`, where `PlatformShare(x) = x*375/1000` with `assert(MoneyRange)` (`payments.cpp:54-60`).
  - The share is computed on the **subsidy-only** MN reward, so fees are excluded.
  - `masternodeReward -= platformReward`. An expected output `CTxOut(platformReward, CScript() << OP_RETURN)` (scriptPubKey = single byte `0x6a`) is pushed **unconditionally, even when the amount is 0** (`:141-151`).
- **The "CreditPool" era has no platform share** (fix `3d67e7daab`, v23.0.0: it also requires V20). On mainnet V20 < MN_RR, so there is no historical impact.
- Worked mainnet example, k=11, V20 + MN_RR, fees F=0:
  - net subsidy 177022505
  - MN 132766878
  - platform 49787579
  - MN-to-owner/operator 82979299
  - miner max 44255627
  - Compare the unit-test vectors `src/test/block_reward_reallocation_tests.cpp:167-175` (k=14, V20 without MN_RR: gross 177167660, net 141734128, MN 106300596).

### 2.3 Operator and owner split (`payments.cpp:162-193`)
- `operatorReward = masternodeReward * nOperatorReward / 10000`, but only if `nOperatorReward != 0 && scriptOperatorPayout != empty`. `nOperatorReward` is the uint16 basis points from ProRegTx, stored on `CDeterministicMN`. If the operator script is empty, the operator share stays with the owner. Then `masternodeReward -= operatorReward` (`:162-168`).
- **Owner, v24 features (not active on mainnet):**
  - Shared-collateral MNs: `SplitAmountByShares`, sequential floor via a 128-bit intermediate, remainder to the last share. See `evo/providertx_util.cpp:88-117`, paying `share.RewardScript()` (`:170-178`).
  - Otherwise `GetOwnerPayouts(state)`: a legacy single `scriptPayout`, or the ProTx v3 (`ExtAddr`) multi-payout list. Payouts are `(reward*payout.reward)/10000`, with the last one getting the remainder (`:179-190`).
  - An output is added only if the amount is > 0. The operator output is appended last (`:191-193`).
- Pre-v24, everything reduces to `[platform OP_RETURN?] + owner(scriptPayout) + operator(scriptOperatorPayout)?`.

---

## 3. Payee selection and validation

### 3.1 Expected payee: `CDeterministicMNList::GetMNPayee(pindexPrev)` (`evo/deterministicmns.cpp:158-194`)
- The list is `GetListForBlock(pindexPrev)` (`payments.cpp:152`), i.e. the list *after* the previous block. `nHeight` inside the function is that list's height (= prev height).
- If `mnMap` is empty, it returns nullptr.
- **EvoNode consecutive payments:** if V19 is active and MN_RR is not (both `DeploymentActiveAfter(pindexPrev)`), scan valid (non-banned) MNs for the one with `nLastPaidHeight == nHeight`. If it is `MnType::Evo` and `nConsecutivePayments < 4` (`dmn_types::Evo.voting_weight`, `dmn_types.h:37-39`), pay it again (`:167-184`).
  - Mainnet window: h in [1899072, 2128895].
- Otherwise pick the valid MN minimizing `CompareByLastPaid` (`:132-151`):
  - The key is `CompareByLastPaid_GetHeight`: `nPoSeRevivedHeight` if `!= -1 && > nLastPaidHeight`; else `nRegisteredHeight` if `nLastPaidHeight == 0`; else `nLastPaidHeight`.
  - Ties go to the smaller `proTxHash` using `uint256::operator<`, which is **memcmp over the internal little-endian bytes** (`uint256.h:55-59`), **not** the displayed hex order.
- "Valid" means `!IsBanned()`, i.e. `nPoSeBanHeight == -1` (`deterministicmns.h:301-324`, `dmnstate.h:146-149`). **Unconfirmed MNs (no `confirmedHash` yet) can be paid.**
- **State update after the block** (`evo/specialtxman.cpp:326,758-796`):
  - The payee computed from `prevList` gets `nLastPaidHeight = h`, even if the MN was removed or changed in this block. It is "paid one last time" if still present in `newList`.
  - If Evo and MN_RR is not active, `++nConsecutivePayments`.
  - All other Evo MNs with nonzero `nConsecutivePayments` are reset to 0. After MN_RR, the payee is reset too.
  - **This happens whatever the coinbase actually paid.**
- Payout scripts and operator reward come from `prevList`. A ProUpRegTx/ProUpServTx in the same block does not affect this block's payment.

### 3.2 IsTransactionValid (`payments.cpp:227-260`)
- `if (!DeploymentDIP0003Enforced(h))` → **return true**. `h >= DIP0003EnforcementHeight` is required (`deploymentstatus.h:65-69`): mainnet 1047200, testnet 7300, regtest 500.
  - **Pre-DIP3-enforcement MN payments, including all legacy-MN-era (2014-2019) payments, are not validated at all.** The legacy code was removed in #2600 (`f95aae2b30`).
  - Between DIP0003Height (1028160) and enforcement, the list exists but payees are unchecked.
  - The DIP3 enforcement block hash is pinned (`deterministicmns.cpp:772-778`, `"bad-dip3-enf-block"`).
- `GetBlockTxOuts` returns false only when the list is non-empty but **no valid (non-banned) MN exists**. In that case `IsTransactionValid` **returns true, i.e. accept anything** (`:237-240`). That includes the platform OP_RETURN, which is then not enforced while the credit pool still adds `platformReward` (§6).
- If the list is empty (`GetCounts().total()==0`), it returns true with only the platform OP_RETURN expected (`:153-156`).
- **Matching** (`FindUnmatchedMasternodePayment`, `:24-52`):
  - Pre-V24 (`strict_multiplicity=false`): every expected `CTxOut` (value **and** script equal) must exist *somewhere* in the coinbase. One output may satisfy several identical expected outputs.
  - From V24 (`is_v24_active`, passed from `validation.cpp:2660,2859`): greedy distinct-output (multiset) matching.
  - Output **order is irrelevant**, and extra outputs are allowed.

### 3.3 IsBlockPayeeValid (`payments.cpp:392-442`)
1. If `IsTransactionValid` fails, return false. ConnectBlock then returns `"bad-cb-payee"`.
2. If `!m_superblocks.IsValid()` (governance cache not loaded, e.g. `-disablegovernance`), return true.
3. If `h < nSuperblockStartBlock`, return true ("accepting any payee").
4. If `check_superblock == NoCheck`, return true.
5. If a superblock is triggered (§4), it must pass `IsValidSuperblock(..., blockSubsidy+feeReward, is_v24)`; otherwise return false.

---

## 4. Block value, superblocks and governance (deterministic vs. not)

### 4.1 ConnectBlock call sites (`src/validation.cpp`)
Order matters for which reject reason appears first:
- `:2617-2623`: if `h == nSuperblockStartBlock` and the hash is not `nSuperblockStartHash`, reject `"bad-sb-start"` (BLOCK_CONSENSUS). Mainnet pins 614820 = `0000000000020cb27c7ef164d21003d5d20cdca2f54dd9a9ca6d45f4d47f8aa3`; the test networks use a null hash.
- `:2659`: `blockSubsidy = GetBlockSubsidy(pindex)`. `:2660`: `is_v24_active = DeploymentActiveAfter(pprev, V24)`.
- `:2664`: `ProcessSpecialTxsInBlock(..., blockSubsidy, fJustCheck, fScriptChecks /*=fCheckCbTxMerkleRoots*/, ...)`. This contains the CbTx checks and the credit-pool balance check (§6).
  - **Under assumevalid, `fScriptChecks=false` (`:2543-2569`), so all CbTx checks are skipped**: the credit pool balance, MNList/quorum merkle roots and CL sig.
  - MN payee checks below are **not** skipped.
- `:2716-2720`: `nFees` accumulation, rejecting `"bad-txns-accumulated-fee-outofrange"`.
- `:2800-2817`: InstantSend conflict check, only when `!IsInitialBlockDownload()` (spork-gated, §5).
- `:2830`: `feeReward = nFees`.
- `:2840-2842`: `check_superblock` is:
  - `NoCheck` if `!IsSuperblockValidationRequired(pindex)`
  - else `DisallowDuplicates` if V24
  - else `AllowDuplicates`
- `IsSuperblockValidationRequired` (`evo/chainhelper.cpp:41-53`) returns false if `bestChainLockHeight >= pindex->nHeight` (a **height-only** comparison) or if `!mn_sync.IsSynced()`.
- `:2845-2849`: if `!IsBlockValueValid(chain, block, pprev, blockSubsidy + feeReward, ...)`, reject `"bad-cb-amount"` with **`BLOCK_RESULT_UNSET`**. The peer is not punished (`net_processing.cpp:1970-1971`), but the block *is* marked invalid.
- `:2858-2863`: `IsBlockPayeeValid(chain, coinbase, pprev, blockSubsidy, feeReward, era, is_v24_active, check_superblock)`, rejecting `"bad-cb-payee"` (`BLOCK_RESULT_UNSET`).
- Dash has **no** Bitcoin-style unconditional `GetValueOut() > blockReward` check. `IsBlockValueValid` replaces it.
- `v23.1.8` difference: `check_superblock = bestCLHeight < pindex->nHeight` (bool), with `IsSynced`/`IsValid` tested inside payments.cpp. The effect is the same, apart from the old-budget detail in §4.2.

### 4.2 IsBlockValueValid (`payments.cpp:313-390`). `V = coinbase.GetValueOut()`, `R = subsidy+fees`
1. `h < nBudgetPaymentsStartBlock` → `V <= R` (`:320-326`).
2. `h < nSuperblockStartBlock` → `IsOldBudgetBlockValueValid` (`:262-301`):
   - If `h % nBudgetPaymentsCycleBlocks < nBudgetPaymentsWindowBlocks` (mainnet: cycle 16616, window 100):
     - **`NoCheck` → accept with NO bound at all** (`:280-287`).
     - Otherwise `V <= R`.
   - Outside the window, `V <= R`.
   - Mainnet windows lie in [328008, 614819]: h ∈ [16616·k, 16616·k+99] for k=20…37. The first is 332320; the last is 614792..614819 (truncated by SB start).
   - Real overpaying blocks exist, e.g. **h=332320 paid 1181.08031847 DASH vs a limit of 5.08031847** (old 12.0 budget). See commit `2addaf83f6` and `src/test/masternode_payments_tests.cpp:131`. That commit fixed an IBD regression in v24 rc1/rc2 introduced by `95d5dcc2c2`.
   - In `v23.1.8` the gate was `!m_mn_sync.IsSynced()` rather than NoCheck.
3. SB era (`h >= nSuperblockStartBlock`):
   - `max = R + CSuperblock::GetPaymentsLimit(h)` (`:334`).
   - If `!IsValidBlockHeight(h)` → `V <= R` (`:339-346`).
   - Else if `V > max` → false ("exceeded superblock max value", `:348-353`).
   - If `!m_superblocks.IsValid()` → true (`:355-360`). If `NoCheck` → true (`:362`).
   - If not triggered → `V <= R` (`:368-376`).
   - If triggered → `IsValidSuperblock(..., R, is_v24)`; on failure, "invalid superblock detected" (`:378-387`).

### 4.3 Superblock heights and limit (`governance/superblock.cpp`)
- **`IsValidBlockHeight(h)`** is `h >= nSuperblockStartBlock && h % nSuperblockCycle == 0` (`:119-124`).
  - Mainnet: start 614820, cycle 16616. The **first SB is at 631408** (614820 % 16616 = 28).
  - Testnet: start 4200, cycle 24, first SB 4200. Regtest: start 1500, cycle 20.
- **`GetPaymentsLimit(h)`** (`:145-163`):
  - Returns 0 at non-SB heights.
  - `fV20 = h >= V20Height`.
  - `nBits = fPowAllowMinDifficultyBlocks ? compact(powLimit) : 1`. Mainnet uses 1, which forces formula 3 → base 5. Testnet, regtest and devnet use powLimit → base 25 pre-V20.
  - `limit = GetSuperblockSubsidyInner(nBits, h-1, fV20) * nSuperblockCycle`.
  - It uses the decline step `k` of the SB height itself, not the actual per-block treasury accrued over the cycle.
- Mainnet limits (exact):

| first SB at | limit (DASH) |
|---|---|
| 631408 | 6651.84988184 |
| 847416 | 6176.71768808 |
| 1063424 | 5735.52350816 |
| 1262816 | 5325.84323384 |
| 1478824 | 4945.42589560 |
| 1694832 | 4592.18120064 |
| 1894224 | 4264.16823400 |
| **1993920 (first SB after V20)** | **8528.33663416** |
| 2110232 | 7919.16981480 |
| 2326240 | 7353.51481616 |
| 2525632 | 6828.26378160 |

### 4.4 Which trigger counts (`governance/superblock.cpp`, `object.cpp`)
- **Triggered:** `IsSuperblockTriggered(tip_mn_list, h)` (`superblock.cpp:578-620`). It is true if **any** known trigger with `event_block_height == h` has `fCachedFunding` after `UpdateSentinelVariables(tip_mn_list)`.
- **Funding condition:** absolute-yes FUNDING votes >= `max(nGovernanceMinQuorum, weighted_valid_MN_count/10)` (`object.cpp:964-997`).
  - Absolute yes = yes − no.
  - Votes are weighted 4 for Evo and 1 for regular, using the **tip** list (`object.cpp:880-895`).
  - `nGovernanceMinQuorum`: mainnet 10, testnet and regtest 1.
- **Best trigger:** `GetBestSuperblockInternal` (`:556-576`) takes the max absolute-yes count with a strict `>`. Ties go to the first entry in `std::map<uint256>` order. It requires a count > 0.
- **Trigger validity checks are minimal** (`object.cpp:746-765`). The object must parse and be signed by the operator BLS key of the MN at `masternodeOutpoint` in the tip list. **The payment contents are not checked against proposals at consensus level.** MNs only *vote* for triggers matching their own candidate (`governance/signing.cpp:38-117`), which uses `GetTime()` and so is clock-dependent.
- **`CSuperblock::IsValid`** (`:248-337`):
  1. `IsValidBlockHeight`.
  2. `vout.size() >= nPayments`.
  3. The sum of payments must be ≤ `GetPaymentsLimit(h)`.
  4. `V <= R + sum(payments)`.
  5. **Payments matched in order:**
     - Each payment is an exact `(script, amount)` match. The scan starts at `prevMatch` (pre-V24, **inclusive**, so a duplicate can reuse the same vout) or at `prevMatch+1` (V24) (`:295-334`).
     - SB payments may be interleaved with MN outputs, but must appear in non-decreasing vout order.
     - Payments use `GetScriptForDestination(DecodeDestination(addr))`, P2PKH or P2SH (`:194-216,415-432`).
     - Amounts are parsed by `ParsePaymentAmount` (`:22-64`, `ParseFixedPoint` with 8 decimals).
- The same coinbase output can satisfy both an MN-payment expectation and an SB payment (the two checks are independent).

### 4.5 When is governance actually enforced? (nondeterminism inventory)
Full superblock checks run **only if all of these hold**:
- (a) governance is enabled and the cache loaded (`SuperblockManager::IsValid()`, set by `CGovernanceManager::LoadCache` at `governance/governance.cpp:98-109`; init at `init.cpp:2249-2274`)
- (b) the node is `mn_sync.IsSynced()`
- (c) no ChainLock exists at height ≥ h

Even then, the result depends on which trigger objects and votes this node received over P2P. Triggers expire by height: ~1h unknown, 576 blocks if valid, one cycle if executed (`superblock.cpp:339-375`). Testnet also has a "too far in the future" rule.

Further sources of nondeterminism:
- **Governance sync completes on timeout** (`node/sync_manager.cpp:250-262`, `MASTERNODE_SYNC_TIMEOUT_SECONDS=30`, `masternode/sync.h:22`). "Synced" does not mean "has every trigger and vote".
- `-disablegovernance`, which `-prune` forces (`init.cpp:1086,1258`), leads to `IsValid()=false`, so only the bound check applies forever. A masternode cannot disable governance (`init.cpp:1411-1413`).
- **`-reindex` / `-reindex-chainstate`:** `fLoadCacheFiles=false` (`init.cpp:2249`), so `LoadCache(false)` stores an empty cache and sets `is_loaded=true`. The cache is "valid but empty". During reindex `IsSynced()` is false, so the result is `NoCheck` for every block.

**What a node replaying from disk enforces** (reindex, IBD, or `-disablegovernance`) is exactly this deterministic subset:
1. `V <= R` at every non-SB height, **except** old-budget windows in [nBudgetPaymentsStartBlock, nSuperblockStartBlock), where any value is accepted.
2. At SB heights, `V <= R + GetPaymentsLimit(h)`. **The SB payees are not checked**, so a miner could pay the whole budget to itself and replaying nodes would accept it.
3. The MN payment (§3) from DIP3 enforcement on, including the platform OP_RETURN.
4. The credit pool balance in the CbTx (V20+, unless assumevalid).
5. The `bad-sb-start` and `bad-dip3-enf-block` pinned hashes.

### 4.6 V20+ and later changes
- V20 doubles the treasury share (§1) and the SB limit (§4.3). Sentinel was eliminated in v20 (`ceb84d5b51`, #5525): MNs create and vote triggers themselves. That is not a consensus change in the validity rule.
- v21.0.0 (`41ab95dbf8`) added the "skip governance checks for chainlocked blocks" rule.
- v22.0.0: V20 buried (`1087489fd4`) and sporks hardened on mainnet (`9ed292a6e1`).
- v24 (develop):
  - SPORK_9 removed (`afcf3d148e`). Previously, when SPORK_9 was off, a synced node rejected any `V > R` at SB heights. That gate is now gone, with no consensus difference on mainnet because the spork was on (and hardened from v22).
  - Superblock logic extracted into `governance::SuperblockManager` (`95d5dcc2c2`).
  - V24 strict multiplicity for SB and MN outputs.
- There is no "superblock trigger quorum" consensus feature in v21-v24. The funding threshold is the same 10%-of-weighted-MNs rule.

### 4.7 Why governance rejections are "soft"
- `bad-cb-amount`/`bad-cb-payee` use `BLOCK_RESULT_UNSET`, so there is no ban. The comment says "Do not punish, the node might be missing governance data" (`validation.cpp:2846,2860`).
- `EnforceBlock` is called for chainlocked blocks. It calls `ResetBlockFailureFlags` on an invalid chainlocked block (`validation.cpp:4046-4075`, via `chainlock/handler.cpp:273-303`). On reconnect the block is chainlocked, so `NoCheck` applies and only bounds are checked.
- **ChainLocks are the final arbiter of superblock payee correctness on mainnet since DIP8 (h >= 1088640) and ChainLock deployment.**
- **Rust design:** implement §4.5 as the deterministic consensus core. Superblock payee checks can be an optional "tip policy" layer used only when governance is synced, and must yield to ChainLocks.

---

## 5. Sporks (`src/spork.h`, `src/spork.cpp`)

- Remaining sporks, all with default 4070908800 = OFF (`spork.h:35-73`):
  - `SPORK_2_INSTANTSEND_ENABLED`=10001
  - `SPORK_17_QUORUM_DKG_ENABLED`=10016
  - `SPORK_19_CHAINLOCKS_ENABLED`=10018
  - `SPORK_21_QUORUM_ALL_CONNECTED`=10020
  - `SPORK_23_QUORUM_POSE`=10022
- Removed: SPORK_3 and SPORK_9 (v24 `afcf3d148e`), SPORK_24 (`9a9d0d5b79`), and SPORK_8 payment enforcement plus others (#2600/#2607).
- **Mainnet hardening** (`spork.cpp:171-181`, since v22.0.0): `GetSporkValue` returns **0 for every spork except SPORK_21=1** when `!IsTestChain()`. `IsSporkActive` is `value < GetAdjustedTime()` (`:163-169`), so on mainnet every spork is "active" and spork messages are irrelevant.
- **Spork keys** (single key; multikey removed in v24 `4f3c876343`):
  - mainnet `Xgtyuk76vhuFW2iT7UAiHgNdWXCf3J34wh` (`chainparams.cpp:293`)
  - testnet `yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55` (`:487`)
  - devnet same as testnet (`:670`)
  - regtest `yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW` (`:868`)
  - `-sporkaddr` can override (`init.cpp:1746-1754`).
- **Where sporks touch block validity:**
  - `SPORK_2` gates the ConnectBlock `"conflict-tx-lock"` rejection, `BLOCK_CHAINLOCK` (`validation.cpp:2800-2817`). That rejection fires only when `!IsInitialBlockDownload()` and is further gated by `IsInstantSendEnabled() = !fReindex && !fImporting && spork2` (`instantsend/instantsend.cpp:510-513`, used by `GetConflictingLock` at `:425-440`). A chainlocked tx overrides the islock (`validation.cpp:2805-2809`).
  - `SPORK_19` gates ChainLock processing and signing (`chainlock/chainlock.cpp:33-35`, `handler.cpp:107,212`), and so indirectly `IsSuperblockValidationRequired` and conflict-chain enforcement. **The CbTx `bestCLSignature` verification does not read SPORK_19** (`evo/specialtxman.cpp:150-213`, `chainlock/clsig.cpp:253-275`).
  - `SPORK_17/21/23` affect only DKG networking and participation (`llmq/options.cpp:26-47`, `active/dkgsession.cpp:230,259`, `llmq/dkgsessionmgr.cpp:49-110`). PoSe punishment in the MN list comes from the **mined commitment's** `validMembers` bits, not from the spork (`HandleQuorumCommitment`, `evo/specialtxman.cpp:284-298`).
  - **No spork is read by subsidy, MN payment, credit pool or superblock-bound code.** There is no hard-coded historical spork behavior in block validation. Old blocks validated under spork-dependent rules (e.g. pre-DIP3 MN payment enforcement) are now simply unchecked.

---

## 6. MN_RR platform reward and credit pool (`src/evo/creditpool.{h,cpp}`)

### 6.1 Balance rule (CbTx v3+, V20+)
- `CheckCreditPoolDiffForBlock` (`specialtxman.cpp:1038-1068`) returns true unless DIP0008 **and** V20 are active at `pindex`. It then builds the diff and requires `cbTx.creditPoolBalance == GetTotalLocked()`, rejecting `"bad-cbtx-assetlocked-amount"`. Exceptions give `"failed-checkcreditpooldiff"`; `EvoDbInconsistencyError` gives `state.Error` and does not invalidate.
- It runs only if the CbTx was parsed, which requires `fCheckCbTxMerkleRoots` (= `fScriptChecks`) (`specialtxman.cpp:817-834,884-890`). **It is skipped under assumevalid** and also in `RollforwardBlock` (`validation.cpp:5196`).
- `GetTotalLocked = pool.locked + sessionLocked − sessionUnlocked + platformReward` (`creditpool.h:104-106`):
  - `pool.locked` = the previous block's CbTx `creditPoolBalance`.
  - `sessionLocked` = Σ over AssetLock txs of the **first** OP_RETURN output's value (`creditpool.cpp:291-305`). Errors: `"failed-creditpool-lock-payload"` / `"-lock-invalid"`.
  - `sessionUnlocked` = Σ over AssetUnlock txs of `fee + Σvout` (`:31-47,307-328`).
  - `platformReward` = `PlatformShare(GetMasternodePayment(h, blockSubsidy, EvoReward))` when MN_RR is active after pprev (`:279-289`). `blockSubsidy` = `GetBlockSubsidy(pindex)`, net of treasury, no fees. So it is **37.5% of 75% of the net subsidy**, i.e. 22.5% of the gross subsidy with V20's 20% treasury.
- The block's lock/unlock processing is in `GetCreditPoolDiffForBlock` (`:376-405`). It iterates `vtx[1..]`; any failure gives the tx's reject reason (BLOCK_CONSENSUS). Exceptions give `"failed-getcreditpooldiff"`.

### 6.2 Unlock limit (window = `nCreditPoolPeriodBlocks`: 576 on main/test/devnet, 100 on regtest; `chainparams.cpp:295,489,...,875`)
- `ConstructCreditPool(block_index=h, prev)` (`creditpool.cpp:167-241`) derives the pool *after* h, which is then used for block h+1.
- Inputs:
  - `blockData` = (CbTx balance, Σunlocked, unlock indexes) of block h (`:58-108`). Pre-DIP3 blocks give nullopt; pre-V20 pools are empty (`:116-118`).
  - `distant` = the same data for the ancestor at h−period. Zeros if missing or pre-DIP3; a pre-V20 CbTx v2 has a 0 balance.
  - `latelyUnlocked = prev.latelyUnlocked + blockData.unlocked − distant.unlocked`, a sliding sum over (h−period, h].
- Limit, by the deployment state **at h** (not h+1):
  - **V24 active at h:** `UnlockLimitV24(balance, balance_window_start) = min(max(0, max(start*20/100, 2000 DASH) − (start − balance)), balance)` (`creditpool.h:139-159`). v23.1.8 had a different V24 rule (`min(credit, 4000 − latelyUnlocked)`), so **V24 is in flux and not yet active on mainnet**.
  - **WITHDRAWALS active at h:** `min(balance, 2000 DASH)`. This is **per block**; `latelyUnlocked` is ignored (`31ca8a497a`, v22.0.0). Mainnet WithdrawalsHeight is 2201472, so the rule governs unlocks in blocks ≥ 2201473; testnet uses 1148500.
  - **Else (V20 to Withdrawals):**
    - Start from `limit = balance`.
    - If `limit + latelyUnlocked > 100 DASH`: `limit = max(100 DASH, balance/10) − latelyUnlocked`, clamped at ≥ 0.
    - Then `limit = min(limit, 1000 DASH − latelyUnlocked)` (`:208-221`).
  - A negative final limit throws, which the caller turns into `"failed-getcreditpooldiff"` (`:232-235`).
- Unlock checks:
  - `sessionUnlocked + toUnlock > currentLimit` → `"failed-creditpool-unlock-too-much"`.
  - An index already seen since genesis (`CRangesSet`) or within the block → `"failed-creditpool-unlock-duplicated-index"` (`:316-322`).
- Rust note: Core reconstructs the pool by re-reading historic blocks from disk (`ReadBlockFromDisk` in `GetCreditDataFromBlock`), with snapshots every 576 blocks in EvoDB. That makes pruning-hostile reads. A Rust node should store per-block `(balance, unlocked, indexes)` itself.
- **Quirk:** when the payee check is skipped (all MNs banned, §3.2), the pool is still credited `platformReward` even if the coinbase lacks the OP_RETURN burn.

---

## 7. Historical quirks to replicate (checklist)
1. Mainnet `nPrevHeight <= 4500`: dDiff without exponent shift (`validation.cpp:1815-1817`).
2. The subsidy uses **prev** block nBits and prev height. Declines start at block 210241; the treasury starts at 328010 (not 328008).
3. Each MN share increment is truncated separately; the 47.5%→50% step skips period 8 (threshold 313520). The BRR table and nReallocStart round to the next SB cycle (mainnet 1379128).
4. In old-budget windows (mainnet 332320+16616·k … +99, up to 614819), a non-checking node accepts **unbounded** coinbase value. Real blocks overpay, e.g. 332320 paid ~1176 DASH of budget.
5. Superblock start-hash pin at 614820 (`bad-sb-start`). The first actual SB height is 631408.
6. No MN payee validation before DIP3 enforcement (1047200, pinned hash).
7. Pre-V24 existence-only matching for MN outputs and inclusive-index matching for SB outputs (duplicates can share one vout).
8. Accept-any-payee when the DML has MNs but all are PoSe-banned.
9. Mainnet blocks ≤ 68589 accepted nBits within ±50% of DGW (`validation.cpp:4474-4486`). Those nBits feed later subsidy computations. This belongs to the PoW agent; check which `abs` overload is used there.
10. EvoNodes were paid 4 consecutive blocks during V19..MN_RR (mainnet 1899072..2128895).
11. `GetPaymentsLimit` ignores real difficulty (mainnet nBits=1 → base 5; testnet uses powLimit → base 25 pre-V20).

---

## 8. Difficulty estimates (Rust)
| Subsection | Est. | Notes |
|---|---|---|
| §1 subsidy | **M** | ~60 LOC. Effort goes into bit-exact float plus a full-history differential test against `getblockstats subsidy` |
| §2 MN share, platform share, operator/owner split | **S** (pre-v24) / **M** (with v24 multi-payout and shared collateral) | |
| §3 payee selection | **L** | Needs the full DML state machine (`nLastPaidHeight`, `nRegisteredHeight`, `nPoSeRevivedHeight`, `nPoSeBanHeight`, `nConsecutivePayments`, payout scripts, `nOperatorReward`), PoSe from commitments, memcmp proTxHash ordering. Mostly shared with the DML agent |
| §4 deterministic bounds (value, SB limit, budget windows, pinned hashes) | **S** | |
| §4 full governance (objects, votes, triggers, sync, expiry) | **XL** | P2P, BLS-signed objects, vote DB, wall clock. Recommend making it optional and non-blocking; rely on ChainLocks |
| §5 sporks | **S** | Mainnet hard-coded. Testnet needs spork message verification (ECDSA compact sig) only for IS/CL gating |
| §6 credit pool | **M** | Integer rules plus a sliding window. V24 rule still changing |

---

## 9. What rust-dashcore already has (HEAD `67b307c6`)
Reusable:
- Special-tx payload codecs in `dash/src/blockdata/transaction/special_transaction/`:
  - `coinbase.rs`: CbTx v1-v3.
  - `asset_lock.rs`, `asset_unlock/*`: fee is u32.
  - `provider_registration.rs`: `operator_reward: u16`, `script_payout`.
  - `provider_update_registrar.rs` (`script_payout`) and `provider_update_service.rs` (operator payout).
- BLS types, SML/quorum verification (`dash/src/sml/*`), ChainLock types (`dash/src/ephemerealdata/chain_lock.rs`), and the dash-spv SML engine (useful for CL verification).

Gaps and pitfalls:
- **No subsidy, MN-payment, superblock, governance, spork or credit-pool logic at all** (grep found none).
- `dash/src/blockdata/constants.rs:59` `SUBSIDY_HALVING_INTERVAL = 210_000` is a Bitcoin leftover and is **wrong** for Dash (210240). It is unused; do not reuse it.
- `CoinbasePayload` (`coinbase.rs:39-47`):
  - `asset_locked_amount: Option<u64>`, whereas Core's `creditPoolBalance` is int64 `CAmount`.
  - The field named `best_cl_height` is actually `bestCLHeightDiff`.
  - There is **no v4** (`MERKLE_ROOT_ASSETUNLOCKS`, V24) support.
- The SML `MasternodeListEntry` (`dash/src/sml/masternode_list_entry/mod.rs:135-144`) lacks all payment-ordering fields. A full DML is required.
- `dash/src/consensus/params.rs` is Bitcoin-shaped, with no Dash budget/SB/BRR/V20 params.
- `dash-network::v20_activation_height` is wrong for regtest and devnet (§1.7).
- No ProTx v3 (`ExtAddr`) multi-payout or shared-collateral payloads (v24).
