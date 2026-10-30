> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# B — Block-level consensus, PoW, chain params, versionbits (Dash Core `develop` @ 15db478750)

Every rule below was checked against the source. Citations use the form `file:line`. Paths are relative to
`/work/home/agent/workspace/dash/` unless they start with `rust-dashcore/`. History comes from `git log -S` / `git show`.
Scope: block-level consensus outside special-tx and payment internals. Other agents own payments, evo, llmq, and mnhf;
here they appear only as hooks, with an ordering and a one-line description.

Difficulty legend for a Rust port: S = under 1 day, M = days, L = 1–2 weeks, XL = more than 2 weeks or high risk.

---

## 0. File index

| Area | File (lines) |
|---|---|
| Params struct and enums | `src/consensus/params.h:16-241` |
| Per-network params | `src/chainparams.cpp` (main 158-353, test 358-527, devnet 532-758, regtest 764-1003, arg overrides 1005-1357) |
| PoW | `src/pow.cpp:15-256`, `src/arith_uint256.{h,cpp}` |
| Header and block checks | `src/validation.cpp:4305-4594` (CheckBlockHeader/CheckMerkleRoot/CheckBlock/ContextualCheckBlockHeader/ContextualCheckBlock) |
| Accept path | `src/validation.cpp:4596-4905` (AcceptBlockHeader / AcceptBlock / ProcessNewBlock) |
| Connect | `src/validation.cpp:2420-2933` (GetBlockScriptFlags, ConnectBlock), `3393-3511` (ConnectTip), `3517-3579` (FindMostWorkChain) |
| Versionbits | `src/versionbits.{h,cpp}`, `src/deploymentstatus.h`, `src/deploymentinfo.cpp` |
| Sizes and sigops | `src/consensus/consensus.h:9-24`, `src/consensus/tx_verify.cpp:118-161`, `src/script/script.cpp:154-176` |
| X11 | `src/primitives/block.cpp:13-19`, `src/hash_x11.h:25-90`, `src/crypto/x11/*` |
| Merkle | `src/consensus/merkle.cpp:45-73` |
| TXID | `src/primitives/transaction.cpp:74-125`, `src/primitives/transaction.h:380-395` |

---

## 1. `Consensus::Params` (src/consensus/params.h)

### 1.1 Enums
- `BuriedDeployment : int16_t` (`params.h:22-39`). Values start at INT16_MIN: `DEPLOYMENT_HEIGHTINCB, DERSIG, CLTV, BIP147, CSV, DIP0001, DIP0003, DIP0008, DIP0020, DIP0024, BRR, V19, V20, MN_RR, WITHDRAWALS`. `ValidDeployment(dep)` is `dep <= DEPLOYMENT_WITHDRAWALS` (`:40`).
- `DeploymentPos : uint16_t` (`params.h:42-47`): `DEPLOYMENT_TESTDUMMY = 0`, `DEPLOYMENT_V24 = 1`, `MAX_VERSION_BITS_DEPLOYMENTS = 2`. **V24 is the only live BIP9 deployment.** There is no V23 entry: it was renamed to V24 (commit eb0e14c339, 2025-09-16).
- `DeploymentHeight(BuriedDeployment)` (`params.h:201-236`) maps each buried deployment to its height field. CSV, for example, maps to `CSVHeight`. The fallback return is `INT_MAX`.

### 1.2 `BIP9Deployment` (`params.h:53-92`)
`bit` (default 28), `nStartTime` (default NEVER_ACTIVE), `nTimeout` (default NEVER_ACTIVE), `min_activation_height` (0), `nWindowSize` (0 means use `nMinerConfirmationWindow`), `nThresholdStart` (0 means use `nRuleChangeActivationThreshold`), `nThresholdMin`, `nFalloffCoeff`, `useEHF` (false). Constants: `NO_TIMEOUT = INT64_MAX`, `ALWAYS_ACTIVE = -1`, `NEVER_ACTIVE = -2`.

### 1.3 `Params` fields (`params.h:97-199`)
`hashGenesisBlock`, `hashDevnetGenesisBlock`, `nSubsidyHalvingInterval`, `nMasternodePaymentsStartBlock`, `nMasternodePaymentsIncreaseBlock`, `nMasternodePaymentsIncreasePeriod`, `nInstantSendConfirmationsRequired`, `nInstantSendKeepLock`, `nBudgetPaymentsStartBlock`, `nBudgetPaymentsCycleBlocks`, `nBudgetPaymentsWindowBlocks`, `nSuperblockStartBlock`, `nSuperblockStartHash`, `nSuperblockCycle`, `nSuperblockMaturityWindow`, `nGovernanceMinQuorum`, `nGovernanceFilterElements`, `nMasternodeMinimumConfirmations`, `BIP34Height`, `BIP34Hash`, `BIP65Height`, `BIP66Height`, `BIP147Height`, `CSVHeight`, `DIP0001Height`, `DIP0003Height`, `DIP0003EnforcementHeight`, `DIP0003EnforcementHash`, `DIP0008Height`, `BRRHeight`, `DIP0020Height`, `DIP0024Height`, `DIP0024QuorumsHeight`, `V19Height`, `V20Height`, `MN_RRHeight`, `WithdrawalsHeight`, `MinBIP9WarningHeight`, `nRuleChangeActivationThreshold` (u32), `nMinerConfirmationWindow` (u32), `vDeployments[2]`, `powLimit`, `fPowAllowMinDifficultyBlocks`, `fPowNoRetargeting`, `nPowTargetSpacing` (i64), `nPowTargetTimespan` (i64), `nPowKGWHeight`, `nPowDGWHeight`, `nMinimumChainWork`, `defaultAssumeValid`, devnet-only `nMinimumDifficultyBlocks{0}`, `nHighSubsidyBlocks{0}`, `nHighSubsidyFactor{1}` (`:191-193`), `llmqs` (vector), `llmqTypeChainLocks`, `llmqTypeDIP0024InstantSend`, `llmqTypePlatform`, `llmqTypeMnhf`.
`DifficultyAdjustmentInterval() = nPowTargetTimespan / nPowTargetSpacing` (`:184`). That is 86400/150 = **576** on every network.

**Values that are consensus-relevant but live outside `Consensus::Params`:**
- `CChainParams::nCreditPoolPeriodBlocks`: 576 on main, test and devnet; 100 on regtest (`chainparams.cpp:295,489,672,870`). Read through the **global** `Params()` in `evo/creditpool.cpp:70,196`.
- `strSporkAddress` (`chainparams.cpp:293,487,670,868`). Sporks control chainlock and IS enablement (see §8).
- `TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT = 847000`, hard-coded in `validation.cpp:6666`.
- The network-ID-specific branches at `validation.cpp:1815` (main subsidy bug at h ≤ 4500), `:4474` (main PoW tolerance at h ≤ 68589), `:6676` (LLMQ_50_60 on testnet/devnet), `governance/superblock.cpp:366`, `llmq/utils.cpp:373`, and `evo/netinfo.cpp:69`.
- `MAX_STANDARD_TX_SIZE = 100000` (`policy/policy.h:27`). It is a **consensus** limit after DIP0001 (`validation.cpp:382`).

---

## 2. Per-network parameter tables (src/chainparams.cpp)

### 2.1 Activation heights (buried)
"BIP9 activation" means the height is aligned to a window boundary (4032, or 2016 for CSV). I verified the arithmetic.

| Param | main (`:178-197`) | testnet (`:378-397`) | devnet (`:552-570`) | regtest (`:784-804`) |
|---|---|---|---|---|
| BIP34Height / Hash | 951 / 000001f35e70…a8012 | 76 / 000008ebb1db…bcb6 | 1 / **unset (null)** | 1 / null |
| BIP65Height | 619382 | 2431 | 1 | 1 |
| BIP66Height | 245817 | 2075 | 1 | 1 |
| BIP147Height | 939456 (=233·4032) | 4300 | 1 | **0** |
| CSVHeight | 622944 (=309·2016) | 8064 | 1 | 1 |
| DIP0001Height | 782208 (=194·4032) | 5500 | 2 | 1 |
| DIP0003Height | 1028160 (=255·4032) | 7000 | 2 | 432 |
| DIP0003EnforcementHeight / Hash | 1047200 / 0000…2d1734…7f81 | 7300 / 00000055ebc0…2f8f | 2 / null | 500 / null |
| DIP0008Height | 1088640 | 78800 | 2 | 1 |
| BRRHeight | 1374912 | 387500 | 2 | 1 |
| DIP0020Height | 1516032 | 414100 | 2 | 1 |
| DIP0024Height | 1737792 | 769700 | 2 | 1 |
| DIP0024QuorumsHeight | 1738698 | 770730 | 2 | 1 |
| V19Height | 1899072 | 850100 | 2 | 1 |
| V20Height | 1987776 | 905100 | 2 | = DIP0003Height (432) |
| MN_RRHeight | 2128896 | 1066900 | 2 | = V20Height |
| WithdrawalsHeight | 2201472 | 1148500 | 2 | 600 |
| MinBIP9WarningHeight | 2201472+2016 | 1148500+2016 | 2+2016 | 0 |

Regtest asserts `V20Height >= DIP0003Height` and `MN_RRHeight >= V20Height` (`:928-930`).

### 2.2 PoW / BIP9 base params

| Param | main | testnet | devnet | regtest |
|---|---|---|---|---|
| powLimit | `00000fff…ff` (`:198`) | `00000fff…ff` (`:398`) | `7fff…ff` (`:571`) | `7fff…ff` (`:805`) |
| nPowTargetTimespan / Spacing | 86400 / 150 (`2.5*60` is a double converted to i64) | same | same; `-powtargetspacing` can override (`:657,1316-1333`) | same |
| fPowAllowMinDifficultyBlocks | false | true | true | true |
| fPowNoRetargeting | false | false | false | **true** |
| nPowKGWHeight / nPowDGWHeight | 15200 / 34140 | 4002 / 4002 (no KGW) | 4001 / 4001 | 15200 / 34140 (unused because NoRetargeting) |
| nRuleChangeActivationThreshold / nMinerConfirmationWindow | 1815 / 2016 | 1512 / 2016 | 1512 / 2016 | 108 / 144 |
| nMinimumChainWork | `…ba7225adc3153945d14d` (h 2487500) (`:221`) | `…036c8f738da818d2` (h 1400000) | 0 | 0 |
| defaultAssumeValid | `00000000000000119fe4…b492` (h 2487500) | `000000541a23…0239` (h 1400000) | 0 | 0 |

### 2.3 Payments / governance / IS params

| Param | main | testnet | devnet | regtest |
|---|---|---|---|---|
| nSubsidyHalvingInterval | 210240 | 210240 | 210240 | 150 |
| nMasternodePaymentsStart/Increase/Period | 100000 / 158000 / 17280 | 4010 / 4030 / 10 | 4010 / 4030 / 10 | 240 / 350 / 10 |
| nInstantSendConfirmationsRequired / KeepLock | 6 / 24 | 2 / 6 | 2 / 6 | 2 / 6 |
| nBudgetPaymentsStart/Cycle/Window | 328008 / 16616 / 100 | 4100 / 50 / 10 | 4100 / 50 / 10 | 1000 / 50 / 10 |
| nSuperblockStartBlock / Hash | 614820 / `0000000000020cb27c7e…8aa3` | 4200 / null | 4200 / null | 1500 / null |
| nSuperblockCycle / MaturityWindow | 16616 / 1662 | 24 / 8 | 24 / 8 | 20 / 10 |
| nGovernanceMinQuorum / FilterElements | 10 / 20000 | 1 / 500 | 1 / 500 | 1 / 100 |
| nMasternodeMinimumConfirmations | 15 | 1 | 1 | 1 |

Of these, `nInstantSendKeepLock`, `nInstantSendConfirmationsRequired`, `nGovernanceFilterElements` and `nMasternodePaymentsStartBlock` appear only in non-consensus code: `instantsend/*`, `governance.cpp`, and `rpc/mining.cpp` respectively. `nMasternodeMinimumConfirmations` is consensus, used for DMN confirmedHash at `evo/specialtxman.cpp:339`.

### 2.4 Live versionbits deployments (current code)
| | main (`:207-219`) | testnet (`:407-419`) | devnet (`:580-592`) | regtest (`:815-827`) |
|---|---|---|---|---|
| TESTDUMMY | bit 28, NEVER_ACTIVE | bit 28, NEVER_ACTIVE | bit 28, NEVER_ACTIVE | bit 28, start 0, NO_TIMEOUT |
| V24 bit | **13** | 13 | 13 | 13 |
| V24 start / timeout | NEVER_ACTIVE / NO_TIMEOUT | 1790208000 (2026-09-24) / NO_TIMEOUT | 1751328000 / NO_TIMEOUT | 0 / NO_TIMEOUT |
| V24 window / thrStart / thrMin / falloff / useEHF | 4032 / 3226 / 2420 / 5 / true | 100 / 80 / 60 / 5 / true | 120 / 96 / 72 / 5 / true | 250 / 200 / 150 / 5 / true |

V24 moved from bit 12 to bit 13 in commit 1be3ac299a (2026-09-23). The reason: **bit-12 MNHF signals are already mined on mainnet and testnet** (v23 shipped with bit 12 and NEVER_ACTIVE). Those signals stay in EHF state forever, so a Rust implementation must store unknown-bit signals too. `IsValidMNActivation` returns **true** for an unknown bit, with only a warning (`chainparams.cpp:109-130`).

### 2.5 LLMQ assignments (enum values in `llmq/params.h:19-46`)
| | main (`:272-280`) | testnet (`:465-474`) | devnet (`:639-656`) | regtest (`:913-926`) |
|---|---|---|---|---|
| llmqs | 50_60(1), 60_75(5), 400_60(2), 400_85(3), 100_67(4) | same + 25_67(6) | same as main + DEVNET(101), DEVNET_DIP0024(105), DEVNET_PLATFORM(107) | TEST(100), TEST_INSTANTSEND(104), TEST_V17(102), TEST_DIP0024(103), TEST_PLATFORM(106) |
| ChainLocks | 400_60 | 50_60 | DEVNET (`-llmqchainlocks`) | TEST |
| DIP0024 IS | 60_75 | 60_75 | DEVNET_DIP0024 (`-llmqinstantsenddip0024`) | TEST_DIP0024 (`-llmqtestinstantsenddip0024`) |
| Platform | 100_67 | 25_67 | DEVNET_PLATFORM (`-llmqplatform`) | TEST_PLATFORM |
| MNHF | 400_85 | 50_60 | DEVNET (`-llmqmnhf`) | TEST |

### 2.6 Genesis, network, misc
| | main | testnet | devnet | regtest |
|---|---|---|---|---|
| Genesis (nTime, nNonce, nBits, ver, reward) | 1390095618, 28917698, 0x1e0ffff0, 1, 50 DASH (`:240`) | 1390666206, 3861367235, 0x1e0ffff0, 1, 50 (`:435`) | 1417713337, 1096447, 0x207fffff, 1, 50 (`:609`) | same as devnet (`:847`) |
| Genesis hash | 00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6 | 00000bafbc94add76cb75e2ec92894837288a481e5c005f6563d91623bf8bc2c | 000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e | same |
| Merkle root (all nets) | e0028eb9648db56b1ac77cf090b99048a8007e2bb64b68f092c03c7f56a662c7 | | | |
| pchMessageStart | bf 0c 6b bd | ce e2 ca ff | e2 ca ff ce | fc c1 b7 dc |
| P2P port / RPC port | 9999 / 9998 | 19999 / 19998 | 19799 / 19798 | 19899 / 19898 (`chainparamsbase.cpp:42-48`) |
| nPruneAfterHeight | 100000 | 1000 | 1000 | 1000 (100 with `-fastprune`) |
| Checkpoints | 39 entries (heights 1500 … 2487500, `:297-339`) | 19 (255 … 1380000, `:491-513`) | {0: genesis, 1: devnet genesis} (`:674-679`) | {0} |

Main checkpoint heights: 1500, 4991, 9918, 16912, 23912, 35457, 45479, 55895, 68899, 74619, 75095, 88805, 107996, 137993, 167996, 207992, 312645, 407452, 523412, 523930, 750000, 888900, 967800, 1067570, 1167570, 1364585, 1450000, 1796500, 1850400, 1889000, 1969000, 2029000, 2109672, 2175051, 2216986, 2361500, 2421800, 2471728, 2487500.
Testnet checkpoint heights: 255, 261, 1999, 2999, 96090, 200000, 395750, 470000, 794950, 808000, 840000, 851000, 905100, 960000, 1069875, 1143608, 1189000, 1295700, 1380000.

**Genesis coinbase** (`chainparams.cpp:22-41,78-83`): tx v1. scriptSig = `CScript() << 486604799 << CScriptNum(4) << "Wired 09/Jan/2014 The Grand Experiment Goes Live: Overstock.com Is Now Accepting Bitcoins"`, which serializes to `04ffff001d 0104 4c59 <89 bytes>`. Output: 50 DASH to the P2PK `040184710f…b03a9 OP_CHECKSIG`. The genesis is never validated. It is inserted with `AddGenesisBlock` (`validation.cpp:5374-5384`), and ConnectBlock skips its transactions (`:2537-2541`), so its output never enters the UTXO set.

**Devnet genesis (block 1)** (`chainparams.cpp:43-65,85-107,614-615`): the block is **mined at startup on top of the regtest-style genesis**. Its fields:
- prevhash = genesis hash
- nTime = genesis.nTime + 1
- nBits = genesis nBits (0x207fffff)
- nVersion = 4
- nNonce = the first value whose X11 hash is ≤ target
- one coinbase, v1, scriptSig `CScript() << 1 << <devnet name bytes>`. Here `<< 1` encodes as **OP_1**. The name is `"devnet"` or `"devnet-<name>"` (`util/system.cpp:1051-1055`).
- one output: 50 DASH to `OP_RETURN`.

`CheckBlockHeader` requires every block whose prevhash is the genesis to hash to `hashDevnetGenesisBlock`, otherwise it rejects with `devnet-genesis` (`validation.cpp:4311-4317`). `LoadGenesisBlock` calls AcceptBlock on it (`validation.cpp:5405-5413`).
*Possible issue, unverified at runtime:* with default `-highsubsidyblocks=0`, devnet subsidy is fixed at 5 DASH from prevHeight 0 (`validation.cpp:1822-1831`). Block 1 pays 50 DASH, which looks like it would fail IsBlockValueValid (`bad-cb-amount`). Real devnets set `-highsubsidyblocks`. Confirm empirically before copying this behaviour.

### 2.7 Arg-driven overrides (test networks only)
- **Regtest** (`CRegTestParams` ctor `:843-845`, `:923-926`):
  - `-testactivationheight=name@h` (`:1005-1047`) accepts only `bip147, bip34, dersig, cltv, csv, brr, dip0001, dip0008, dip0024` (dip0024 sets both DIP0024Height and DIP0024QuorumsHeight), `v19, v20, mn_rr`. It does **not** accept dip0003, dip0020 or withdrawals.
  - `-vbparams=dep:start:end[:min_h[:window:thrStart[:thrMin:falloff:useEHF]]]` (`:1049-1108`). It allows 3, 4, 6 or 9 fields. A value of -1 means "keep".
  - `-dip3params=act:enf` (`:1110-1128`).
  - `-budgetparams=mn:budget:sb` (`:1130-1151`).
  - `-llmqtestparams`, `-llmqtestinstantsendparams`, `-llmqtestplatformparams` (size:threshold), and `-llmqtestinstantsenddip0024`.
- **Devnet**:
  - `-minimumdifficultyblocks`, `-highsubsidyblocks`, `-highsubsidyfactor` (`:1211-1220`)
  - `-llmqchainlocks` (must be non-rotating), `-llmqinstantsenddip0024` (must be rotating), `-llmqplatform`, `-llmqmnhf`
  - `-llmqdevnetparams`
  - `-powtargetspacing` (`:1316-1333`). This also changes the 4×spacing min-difficulty rule and DGW timespan.
  - `-devnet=<name>`. This changes the devnet genesis hash.

**Difficulty: S** (data entry plus arg parsing). The Rust implementation must reproduce `MaybeUpdateHeights` exactly, including the names it rejects.

---

## 3. Proof of work

### 3.1 Dispatch: `GetNextWorkRequired(pindexLast, pblock, params)` (`pow.cpp:170-211`)
The checks run in this order, with h = pindexLast->nHeight + 1:
1. If `pindexLast->nHeight < nMinimumDifficultyBlocks`, return powLimit compact. Only devnet sets this, so heights 1..N use the minimum (`:177`).
2. If `h < nPowKGWHeight`, use `GetNextWorkRequiredBTC` (`:181-183`).
3. If `fPowNoRetargeting`, return powLimit compact (`:187`). Regtest only. Commit 16418d3559 (2023) made this apply to the post-BTC algorithms.
4. If `fPowAllowMinDifficultyBlocks` (testnet and devnet):
   - if `pblock.time > last.time + 7200`, return powLimit (`:193`)
   - else if `pblock.time > last.time + 4*spacing` (600 s), return `min(powLimit, SetCompact(last.nBits) * 10)` (`:197-203`). **`*10` is `operator*=(uint32)`, which wraps modulo 2^256.** On devnet (powLimit ≈ 2^255) the wrap can make `bnNew > powLimit` false and return a garbage compact. Reproduce the wrapping exactly.
5. If `h < nPowDGWHeight`, use KGW; otherwise use DGW (`:206-210`).

Effective schedule per network:
- **main**: h < 15200 uses BTC; 15200 ≤ h < 34140 uses KGW; h ≥ 34140 uses DGW. Every h ≤ 68589 passes only the 50% tolerance check (§3.6), so **KGW and BTC are never checked exactly on mainnet**, and DGW is exact from 68590.
- **testnet**: h < 4002 uses BTC with the testnet min-difficulty rule; h ≥ 4002 uses DGW plus the min-difficulty rules.
- **devnet**: the first `nMinimumDifficultyBlocks` heights use powLimit; h < 4001 uses BTC; then DGW plus min-difficulty.
- **regtest**: h < 15200 uses BTC. Min-difficulty applies, but at interval boundaries `CalculateNextWorkRequired` returns last nBits because of NoRetargeting (`:216-217`). h ≥ 15200 uses powLimit. In practice every regtest block has nBits = 0x207fffff, which is the compact form of the regtest powLimit.
- **KGW is unreachable for exact checks on every network.** Testnet and devnet set KGW height = DGW height, regtest returns early, and mainnet KGW blocks fall under the tolerance rule.

### 3.2 BTC retarget (`pow.cpp:134-168, 213-237`)
- Not at a 576-block boundary (`(h) % 576 != 0`): with min-difficulty, `pblock.time > last.time + 2*spacing` (**300 s, not Bitcoin's 20 min**) returns powLimit. Otherwise walk back over blocks where `nHeight % 576 != 0 && nBits == powLimitCompact` and return the first other nBits. Without min-difficulty, return `last.nBits`.
- At a boundary: `first = GetAncestor(last.h - 575)`. `actual = last.time - first.time`, clamped to [timespan/4, timespan*4] = [21600, 345600]. Then `new = SetCompact(last.nBits) * actual / 86400`, capped at powLimit. Bitcoin's off-by-one interval is preserved.

### 3.3 KGW (`pow.cpp:15-79`). It uses floating point.
- `pastSecondsMin = uint64(86400 * 0.025) = 2160`, `pastSecondsMax = 604800`, `PastBlocksMin = 14`, `PastBlocksMax = 4032` (`:28-31`).
- If last is null, has height 0, or has height < 14, return powLimit (`:33`).
- Loop `i = 1..`, walking back from the last block while `nHeight > 0` (genesis excluded):
  - The difficulty "average" is an incremental arith_uint256 mean, divided by `i` with integer truncation and sign-safe ordering (`:39-47`; fix efca207c13).
  - `actual = last.time - reading.time` with a floor of 0. `ratio = double(target)/double(actual)`.
  - `EventHorizonDeviation = 1 + 0.7084 * pow(mass/28.2, -1.228)` (libm `pow`, `:56`).
  - Once `mass >= 14`, break when ratio ≤ 1/EHD or ratio ≥ EHD.
- Finally `new = avg * actual / target`, where `*=` takes a uint32-truncated int64 (overload resolution picks `operator*=(uint32_t)`), then cap at powLimit.
- Any float divergence only affects mainnet heights 15200..34139, which are under tolerance. Use `f64::powf`. **Risk: low.**

### 3.4 DGW v3 (`pow.cpp:81-132`). This is the main path and must be bit-exact.
- If last is null or `last.h < 24`, return powLimit.
- For k = 1..24 over blocks last, last-1, …, last-23:
  - k = 1: `avg = T1`
  - k > 1: `avg = (avg * k + Tk) / (k + 1)`
  - **Integer truncation happens at every step.** The comment calls it "not an average really". `avg * k` is `operator*(base_uint, uint32)` and wraps mod 2^256, which can overflow on devnet. The division is `operator/(uint64)`.
- `actual = last.time - (last-23).time`. This covers **23 intervals**, compared against `target = 24 * spacing = 3600`. The off-by-one is consensus-critical and makes observed spacing about 2.626 min (`:111-116`; doc commit 4d98bfc533).
- Clamp `actual` to [1200, 10800] using integer `/3` and `*3`. Then `new = avg * actual` (uint32 multiply, wrapping) `/ 3600`, cap at powLimit, and `GetCompact`.
- Timestamps are int64 and may be non-monotonic; negative `actual` gets clamped up to 1200.
- Commit 8bbcf62000 (2017) refactored DGW into its current form. I checked that it is equivalent to the 2014 loop.

### 3.5 arith_uint256 semantics to copy
These are the semantics to match in Rust with an exact wrapping U256 (`arith_uint256.{h,cpp}`):
- `SetCompact` (`arith_uint256.cpp:193-211`): `size = c>>24`, `word = c & 0x7fffff`. If size ≤ 3, use `word >> 8*(3-size)`; otherwise `word << 8*(size-3)`, where shifts of 256 or more yield 0. Negative means `word != 0 && (c & 0x800000)`. Overflow means `word != 0 && (size > 34 || (word > 0xff && size > 33) || (word > 0xffff && size > 32))`.
- `GetCompact` (`:213-234`): truncating mantissa. If bit 0x800000 is set, shift right by 8 and increment size.
- `*=(uint32)` and `*=(base_uint)` wrap (`:49-74`).
- `/=(uint64)` uses the fast path when the divisor ≤ u32::MAX, otherwise long division (`:77-117`). The results are identical; commit de1e56a45f was a performance change only.
- **Overload trap:** `bnNew *= int64` resolves to `operator*=(uint32_t)` (a standard conversion beats a user-defined one), so the value is **truncated to 32 bits**. Every call site clamps first (DGW at most 10800; BTC at most 345600), so this is harmless today. Note it anyway.
- powLimit history: mainnet and testnet used `00000fffff000…` until commit 9e98c856f2 (2018-03-19). No compact value lies strictly between the old and new limit, so the change had no effect.

### 3.6 Mainnet h ≤ 68589 tolerance (`validation.cpp:4474-4483`)
`n1 = ConvertBitsToDouble(block.nBits)`, `n2 = ConvertBitsToDouble(GetNextWorkRequired(...))`. The block is rejected with `bad-diffbits` if `abs(n1-n2) > n1*0.5`.
- `ConvertBitsToDouble` (`validation.cpp:1785-1803`): `0xffff / (nBits & 0x00ffffff)` (the sign bit is included in the mask), then ×256 or ÷256 until the shift reaches 29.
- The rule dates from the 2014 darkcoin 0.11 port (commit f8105535fe, "architecture issues with DGW v1 and v2"). The code at that time selected BTC, KGW (15200) and DGW (34140) by height, and only DGW v3 was implemented.
- `abs` resolves to `double abs(double)` in C++; I tested with g++ and `<stdlib.h>`. Do not truncate to an integer.
- The check is `NetworkIDString() == MAIN`, so it applies only to mainnet.
- Everything else uses exact equality: `bad-diffbits`, "incorrect proof of work at %d" (`:4485-4487`).

### 3.7 `CheckProofOfWork` (`pow.cpp:239-256`)
Reject if negative, zero, overflow, or `target > powLimit`, then reject if `UintToArith256(hash) > target`. The hash is interpreted **little-endian** (`ArithToUint256` / `UintToArith256` use LE32 limbs, `arith_uint256.cpp:236-247`). Also called when loading the block index (`txdb.cpp:328`) and reading blocks (`node/blockstorage.cpp:826`).
There is **no** `PermittedDifficultyTransition` and **no** headers-presync (Bitcoin #25717 is not backported). A grep for both comes back empty.

### 3.8 X11 and hashes
- `CBlockHeader::GetHash() = HashX11(80-byte header)` (`primitives/block.cpp:13-19`). The header serializes as nVersion i32, prev, merkle, nTime u32, nBits u32, nNonce u32.
- `HashX11` (`hash_x11.h:25-90`) chains 512-bit outputs through blake → bmw → groestl → skein → jh → keccak → luffa → cubehash → shavite → simd → echo. Each stage after the first hashes the previous **64-byte** output. The result is the **first 32 bytes** of the echo output (`uint512::trim256`, `uint256.h:145-150`). The SIMD dispatch variants (`crypto/x11/dispatch.cpp`, aesni, ssse3, arm) are optimizations only.
- Only the block header uses X11. TXIDs, the merkle tree, and everything else use SHA256d. **Exception (new in V24 code):** an asset-unlock tx (nVersion ≥ 3, nType = ASSET_UNLOCK, payload exactly 145 bytes, payload[0] ≥ 2) has `txid = SHA256d(serialization with the trailing 132 quorum-sig bytes zeroed)` (`primitives/transaction.cpp:74-95,115-125`; `transaction.h:47-53,380-395`). `GetInstanceHash()` returns the full-serialization hash, which the CbTx commits to through `merkleRootAssetUnlocks`. **The block merkle root uses the stable txid** (`consensus/merkle.cpp:65-73` calls `GetHash()`). The payload is predicated on the raw byte length and version, **not on activation**.
- Merkle (`consensus/merkle.cpp:45-62`): Bitcoin algorithm. An odd last entry is duplicated. The `mutated` flag is set when any adjacent pair at any level is equal (CVE-2012-2459).
- Chainwork: `GetBlockProof = ~T/(T+1) + 1`, or 0 if negative, overflow or zero (`chain.cpp:132-145`). Fork choice is most work, then lowest nSequenceId, then pointer address (`node/blockstorage.cpp:32-49`). Dash adds no PoW changes, but it **excludes chains with `BLOCK_CONFLICT_CHAINLOCK`** (`validation.cpp:3543-3559`; see §8).
- MTP: the median of 11 (`chain.h:281,283-295`).

**Difficulty: PoW plus params is M.** DGW, compact encoding and wrapping U256 are straightforward but need many test vectors. Validate every mainnet header from genesis. KGW and BTC are S because tolerance is generous. X11 is S–M: reuse an existing implementation such as `rs-x11-hash` or port sph and test against the genesis hashes.

---

## 4. Header and block validation pipeline (ordered, with reject strings)

### 4.1 `ProcessNewBlock` (`validation.cpp:4865-4905`)
`CheckBlock` → `AcceptBlock` → `ActivateBestChain`.

### 4.2 `CheckBlock(block, fCheckPOW=true, fCheckMerkleRoot=true)` (`:4364-4430`)
Context-free. Checks in order:
1. `CheckBlockHeader` (`:4305-4320`):
   - PoW → **`high-hash`** (BLOCK_INVALID_HEADER)
   - devnet-genesis rule → **`devnet-genesis`** (BLOCK_CONSENSUS)
2. `CheckMerkleRoot` (`:4322-4362`):
   - root mismatch → **`bad-txnmrklroot`** (BLOCK_MUTATED)
   - mutated → **`bad-txns-duplicate`** (BLOCK_MUTATED)
   - **Dash/V24:** if `vtx[0].nType == COINBASE` and the CbTx payload has `nVersion >= MERKLE_ROOT_ASSETUNLOCKS` and `merkleRootAssetUnlocks != CalcCbTxMerkleRootAssetUnlocks(block)` → **`bad-cbtx-assetunlockmerkleroot`** (BLOCK_MUTATED, so the block is not marked invalid; commit 62845d2cc9).
3. `vtx.empty() || vtx.size() > 2,000,000 || serialized size > 2,000,000` → **`bad-blk-length`**. This is the relaxed bound, `MaxBlockSize()` with its default of true (`:4391`).
4. `!vtx[0].IsCoinBase()` → **`bad-cb-missing`**. Any later coinbase → **`bad-cb-multiple`** (`:4395-4399`). `IsCoinBase` means `vin.size()==1 && prevout.IsNull()` (`transaction.h:308-311`).
5. `CheckTransaction` on each tx (`consensus/tx_check.cpp:14-75`), returning the tx reject reason. Reasons: `bad-txns-vin-empty` (empty vin is allowed for QC, MNHF and ASSET_UNLOCK), `bad-txns-vout-empty` (empty vout is allowed for QC and MNHF), `bad-txns-oversize` (> 1,000,000), `bad-txns-payload-oversize` (> 10000), `bad-txns-vout-negative`, `bad-txns-vout-toolarge`, `bad-txns-txouttotal-toolarge`, `bad-txns-inputs-duplicate`, `bad-cb-length` (coinbase scriptSig must be 2..100 bytes, or **1..100 if nType == CbTx**), `bad-txns-prevout-null`.
6. Legacy sigops > 40000 (relaxed `MaxBlockSigOps()`) → **`bad-blk-sigops`** (`:4413-4420`).

### 4.3 `AcceptBlockHeader` (`:4596-4724`)
- Known hash: return `duplicate` (BLOCK_CACHED_INVALID if failed), or `duplicate` (BLOCK_CHAINLOCK) if conflicting.
- Run `CheckBlockHeader`.
- Missing prev → `prev-blk-not-found`. Prev failed → `bad-prevblk`. Prev conflict-chainlock → **`bad-prevblk-chainlock`**.
- **`ContextualCheckBlockHeader`** (`:4467-4520`), in order:
  1. nBits: mainnet h ≤ 68589 uses the tolerance rule, otherwise exact → **`bad-diffbits`**.
  2. Checkpoints (default on, `-checkpoints`): if the last checkpoint present in the block index has height > h → **`bad-fork-prior-to-checkpoint`** (BLOCK_CHECKPOINT). Hash mismatch at a checkpoint height is *not* checked directly; `GetLastCheckpoint` only finds checkpoints already in the index (`node/blockstorage.cpp:429-441`).
  3. `time <= prev.MTP` → **`time-too-old`**.
  4. `time > NodeClock::now() + 7200` → **`time-too-new`** (BLOCK_TIME_FUTURE). This uses the **local clock**, not chain state.
  5. Version: `<2` after BIP34, `<3` after BIP66, `<4` after BIP65 (all `DeploymentActiveAfter(pindexPrev)`) → **`bad-version(0x%08x)`**.
- Descends from a block in `m_failed_blocks` → `bad-prevblk`.
- **Dash:** `HasConflictingChainLock(h, hash)` → added to the index with `BLOCK_CONFLICT_CHAINLOCK`, then **`bad-chainlock`** (`:4691-4697`).

### 4.4 `AcceptBlock` (`:4757-4863`)
After the header: `CheckBlock` again, then **`ContextualCheckBlock`** (`:4528-4594`), in order:
1. BIP113: if CSV is active after prev, `nLockTimeCutoff = prev.MTP`, otherwise `block.time` (`:4535-4543`).
2. `MaxBlockSize(DIP0001 active after prev)`, which is 1,000,000 or 2,000,000, applied to both tx count and serialized size → **`bad-blk-length`**.
3. For each tx:
   - `IsFinalTx(tx, h, cutoff)` → **`bad-txns-nonfinal`**
   - `ContextualCheckTransaction` (`:351-386`):
     - if DIP3 is active: special version (nVersion ≥ 3) with a type outside the 13 allowed (NORMAL, PRO_REG, PRO_UP_SERV, PRO_UP_REG, PRO_UP_REV, COINBASE, QC, MNHF, ASSET_LOCK, ASSET_UNLOCK, PROVIDER_DISSOLVE, PROVIDER_UPDATE_SHARE, PROVIDER_UPDATE_SHARED_REGISTRAR) → `bad-txns-type`
     - coinbase with a non-CbTx type → `bad-txns-cb-type`
     - non-special-version with nType ≠ 0 → `bad-txns-type`
     - if DIP0001 is active and the tx is larger than **100000** → `bad-txns-oversize`
   - accumulate legacy sigops.
4. Sigops > `MaxBlockSigOps(DIP1)` (20000 or 40000) → **`bad-blk-sigops`**.
5. BIP34 coinbase height: if `HEIGHTINCB` is active and **DIP3 is not active**, then `vin[0].scriptSig` must **start with** the bytes of `CScript() << h` → **`bad-cb-height`** (`:4578-4585`). `CScript() << n` gives OP_0 for 0, OP_1..OP_16 for 1..16 (`script.h:398-413`), and otherwise a minimal CScriptNum push. This is a prefix compare.
6. If DIP3 is active and `vtx[0].nType != TRANSACTION_COINBASE` → **`bad-cb-type`** (`:4587-4591`).

Failures other than `BLOCK_MUTATED` mark the index `BLOCK_FAILED_VALID`. If the block is unrequested, then less-work, too far ahead, or below MinimumChainWork means it is silently not processed (`:4803-4813`).

### 4.5 `ConnectBlock`
See §8. ConnectBlock does **not** re-run ContextualCheckBlock or ContextualCheckBlockHeader (`:2494-2505`). `-reindex-chainstate` skips both (`:4465,4526`).

### 4.6 `TestBlockValidity` (`:4921-4972`)
Used for mining and RPC. Checks `bad-chainlock` (BLOCK_INVALID_PREV) first, then CCBH, CheckBlock, CCB, and ConnectBlock with fJustCheck.

**Difficulty: M.** Reject-string parity matters for RPC (`submitblock`, `getblocktemplate` proposal) and P2P.

---

## 5. Block size and sigop limits over time
- Serialized size is the full non-witness serialization at PROTOCOL_VERSION. Dash has no segwit.
- `MAX_LEGACY_BLOCK_SIZE = 1,000,000`, `MAX_DIP0001_BLOCK_SIZE = 2,000,000`. `MaxBlockSigOps = size/50`, which gives 20,000 or 40,000 (`consensus/consensus.h:10-20`).
- The DIP0001 switch uses `DeploymentActiveAfter(pindexPrev, DIP0001)` in ContextualCheckBlock (`validation.cpp:4545`) and `DeploymentActiveAt(*pindex)` in ConnectBlock (`:2657`). Both are equivalent to h ≥ 782208 on mainnet.
- Sigops are counted three ways:
  - In CheckBlock and ContextualCheckBlock: `GetLegacySigOpCount` = inaccurate count over scriptSig + scriptPubKey (`tx_verify.cpp:118-130`). CHECKSIG/VERIFY count 1; CHECKMULTISIG/VERIFY count 20 (`script.cpp:154-176`). **OP_CHECKDATASIG (DIP0020) is not counted.**
  - In ConnectBlock: cumulative `GetTransactionSigOpCount`, which is legacy plus accurate P2SH counting for non-coinbase transactions, because P2SH is always on (`tx_verify.cpp:149-161`, `validation.cpp:2739-2743`) → `bad-blk-sigops`.
- Per-tx limits: 1 MB (CheckTransaction) always. After DIP0001, 100 kB (ContextualCheckTransaction). The extra payload is limited to 10000 bytes.

**Difficulty: S.**

---

## 6. Versionbits (Dash-modified BIP9)

### 6.1 State machine (`versionbits.cpp:33-138`)
Per deployment, with these Dash additions:
- `ALWAYS_ACTIVE` returns ACTIVE. `NEVER_ACTIVE` returns **FAILED** (`:42-49`).
- `Period = nWindowSize ? nWindowSize : nMinerConfirmationWindow` (`:255`).
- The state is computed at period boundaries `pindexPrev` with `(h+1) % period == 0`.
- **The walk-back stops and caches DEFINED** at null, at `pindexPrev.h < MinBIP9WarningHeight` (`:64-68`), or at `MTP < nStartTime || pindexPrev.h < masternodeStartHeight` (`:69-73`). **`MinBIP9WarningHeight` therefore acts as a consensus floor for every deployment.** On mainnet no BIP9 deployment can leave DEFINED before the period ending at or after 2203488.
- DEFINED → STARTED requires `MTP >= nStartTime && pindexPrev.h >= masternodeStartHeight` (`:95-98`).
- STARTED: count signalling blocks in the period. Condition: `(nVersion & 0xE0000000) == 0x20000000 && (nVersion & (1<<bit))` (`:268-271`).
  - `nAttempt = (pindexPrev.h - nStartHeight) / period`, where nStartHeight is the first STARTED height, found by walking cached STARTED entries (`:11-31`).
  - `count >= Threshold(nAttempt)` → LOCKED_IN. Else if `MTP >= nTimeout` → FAILED (`:101-119`). **LOCKED_IN is checked before timeout.**
- LOCKED_IN → ACTIVE when `pindexPrev.h + 1 >= min_activation_height` (`:121-126`).

### 6.2 Dynamic threshold (`versionbits.cpp:256-266`)
```
if thrStart == 0 -> nRuleChangeActivationThreshold
if thrMin == 0 || falloff == 0 -> thrStart
calc = thrStart - (int)(nAttempt*nAttempt*Period)/100/falloff   // int32 product, C truncating division, then i64
return max(thrMin, calc)
```
The product is computed in `int`. It overflows (undefined behaviour, wraps in practice) at nAttempt ≥ 730 for period 4032. Unreachable in practice, but the Rust implementation must not panic.

Mainnet schedule for attempts 0..10: 3226, 3218, 3194, 3154, 3097, 3025, 2936, 2831, 2710, 2573, 2420 (floor). Testnet (100/80/60): 80, 80, 80, 79, 77, 75, 73, 71, 68, 64, 60. Regtest (250/200/150): 200, 200, 198, 196, 192, 188, 182, 176, 168, 160, 150.

### 6.3 EHF (`useEHF`) (`versionbits.cpp:238-252`)
`SignalHeight(pindexPrev)` returns `0` for non-EHF deployments. For EHF deployments it returns the **mined height of the MNHF signal for `bit`** from `AbstractEHFManager::getInstance()->GetSignalsStage(pindexPrev)`, or `INT_MAX` if there is none.
- `GetSignalsStage` (`evo/mnhftx.cpp:61-92`) is empty before V20. It drops signals whose MTP at the signal block is earlier than the start time of the deployment that now uses that bit.
- **Subtle:** `SignalHeight` is evaluated once with the *query* `pindexPrev`, at line 38 before alignment, but the cache key is the period-boundary block. Either reproduce this caching and path behaviour exactly, or prove equivalence (I believe it is equivalent on a single chain).
- The global singleton `AbstractEHFManager::globalInstance` and `Params()` are used inside, so the result depends on global state.

### 6.4 Active-at queries (`deploymentstatus.h:14-50`)
- Buried: `DeploymentActiveAfter(prev, dep)` is `(prev ? prev.h+1 : 0) >= DeploymentHeight(dep)`. `DeploymentActiveAt(index)` is `index.h >= height`.
- BIP9: `State(prev) == ACTIVE`, through `ChainstateManager::m_versionbitscache` (`validation.h:1259-1267`).
- `DeploymentDIP0003Enforced(h)` is `h >= DIP0003EnforcementHeight` (`:66-69`).

### 6.5 Every mainnet deployment (all buried, BIP9 activation)
Values were read from chainparams before each burial commit.

| Deployment | bit | start → timeout | window / thrStart / thrMin / falloff / EHF | Activated (buried height) | Buried in |
|---|---|---|---|---|---|
| BIP34 / BIP66 / BIP65 | — (ISM era) | — | — | 951 / 245817 / 619382 | 4d651621ef (2018) |
| CSV | 0 | 1486252800 → 1517788800 | 2016 / 1916 (95%) | 622944 | 6ad9bdf722 (2019, bitcoin#16060) |
| DIP0001 | 1 | 1508025600 → 1539561600 | 4032 / 3226 | 782208 | d26b07a8f7 (2023) |
| BIP147 | 2 | 1524477600 → 1556013600 | 4032 / 3226 | 939456 | e3a97b0156 (2023) |
| DIP0003 | 3 | 1546300800 → 1577836800 | 4032 / 3226 | 1028160 (enforced 1047200) | e3a97b0156 |
| DIP0008 | 4 | 1557878400 → 1589500800 | 4032 / 3226 | 1088640 | 84573364f5 (2023) |
| REALLOC (BRR) | 5 | 1601510400 → 1633046400 | 4032 / 3226 / 2420 / 5 | 1374912 | e3a97b0156 (height hardened in 1aa50feb4b, 2022) |
| DIP0020 | 6 | 1625097600 → 1656633600 | 4032 / 3226 / 2420 / 5 | 1516032 | e3a97b0156 |
| DIP0024 | 7 | 1660521600 → 1692057600 | 4032 / 3226 / 2420 / 5 | 1737792 (first rotated quorum 1738698) | e3a97b0156 |
| V19 | 8 | 1682380800 → 1714003200 | 4032 / 3226 / 2420 / 5 | 1899072 | 5382d05b7e (2023) |
| V20 | 9 | 1700006400 → 1731628800 | 4032 / 3226 / 2420 / 5 | 1987776 | 1087489fd4 (2024) |
| MN_RR | 10 | 1704067200 → 1767225600 | 4032 / 3226 / 2420 / 5 / **EHF** | 2128896 | 0de3923b06 (2024) |
| WITHDRAWALS | 11 | 1728864000 → 1760400000 | 4032 / 3226 / 2420 / 5 / **EHF** | 2201472 | 77dd259004 (2025) |
| V23 (renamed V24) | 12 | NEVER_ACTIVE (start was set to 2025-07-01 in 98749a7111, then reverted in e4172cdf1d) | 4032 / 3226 / 2420 / 5 / EHF | never; **bit-12 MNHF signals exist on chain** | renamed eb0e14c339 |
| V24 | 13 | NEVER_ACTIVE on mainnet, testnet 2026-09-24 | as above | pending | — |

nRuleChangeActivationThreshold on mainnet was 1916 (95%) until at least May 2023. It is 1815 (90%) now. That value applies only to unknown-bit warnings and to deployments without `nThresholdStart`.

**Today the node decides historical activation from buried heights only** (§6.4). The BIP9 state machine is used only for V24 and TESTDUMMY. A Rust implementation therefore needs height tables plus the BIP9 machine for V24 and future deployments. Signalling data from historical blocks is never consulted.

**Difficulty: M** (state machine, caching, EHF coupling). Add **L** if the MNHF signal store is not built elsewhere.

---

## 7. Coinbase rules (non-payment)
- First tx must be coinbase, no other coinbase (§4.2).
- scriptSig length 2..100, or 1..100 for a CbTx (`tx_check.cpp:60-67`).
- BIP34 prefix height check applies only in [BIP34Height, DIP0003Height): mainnet [951, 1028160) (`validation.cpp:4578-4585`). After DIP3 the height sits in the CbTx payload and is checked in `CheckCbTx` (evo agent).
- After DIP3, coinbase `nType` must be `TRANSACTION_COINBASE` (5): `bad-cb-type` (`:4587-4591`), `bad-txns-cb-type` (`:374-375`). ProcessSpecialTxsInBlock also requires the payload: `bad-cbtx-version` if DIP3 is active and there is no CbTx payload, and `bad-cbtx-payload` or `bad-cbtx-invalid` (`evo/specialtxman.cpp:817-840`). **These run only when `fCheckCbTxMerkleRoots` (= fScriptChecks) is true** (§8).
- BIP30 (`validation.cpp:2591-2613`): `IsBIP30Repeat` is always false in Dash (`:6781-6791`). BIP30 is enforced unless `pprev->GetAncestor(BIP34Height)->hash == BIP34Hash`:
  - mainnet: heights ≤ 951
  - testnet: ≤ 76
  - **devnet and regtest: always**, because BIP34Hash is null
  - Violation → `bad-txns-BIP30`.
- Coinbase maturity is 100 (`consensus.h:24`), enforced in CheckTxInputs.

---

## 8. ConnectBlock and ConnectTip: Dash hooks in order (`validation.cpp:2470-2933`)
1. `ScopedBLSLegacyScheme` guard: legacy BLS if V19 is not active at pindex (`:2491-2492`). This is process-global state.
2. `CheckBlock(fCheckPOW = fCheckMerkle = !fJustCheck)`. A MUTATED result aborts the node (`:2506-2515`).
3. **`HasConflictingChainLock(h, hash)`** → `bad-chainlock` (BLOCK_CHAINLOCK) (`:2517-2520`).
4. EvoDB best-block consistency, or abort (`:2526-2532`).
5. Genesis is skipped (`:2537-2541`).
6. **assumevalid**: `fScriptChecks = false` if the block is an ancestor of both the assumevalid block and the best header, best header work ≥ MinimumChainWork, and the block is more than 2 weeks of equivalent work below the best header (`:2543-2572`).
7. BIP30 (`:2591-2613`).
8. **`bad-sb-start`**: at `nSuperblockStartBlock` the hash must equal `nSuperblockStartHash` when that is non-null (mainnet 614820) (`:2615-2623`).
9. Lock-time flags (CSV) and script flags. `GetBlockScriptFlags` (`:2420-2449`): **P2SH always on, with no BIP16 exception or time**. DERSIG at BIP66, CLTV at BIP65, CSV at CSVHeight, NULLDUMMY at BIP147. DIP0020 opcodes are **enabled from genesis** (flag removed in 61bc3002b0, 2024).
10. `GetBlockSubsidy` (`:1885-1890`). The **floating-point** subsidy formula for pre-V20 blocks (`pow()`, `ConvertBitsToDouble`, mainnet h ≤ 4500 bug path at `:1815-1817`) belongs to the payments agent, but it is a float-determinism hotspot.
11. `is_v24_active = DeploymentActiveAfter(pprev, V24)` (BIP9 state) (`:2660`).
12. **`ProcessSpecialTxsInBlock(..., fJustCheck, fCheckCbTxMerkleRoots = fScriptChecks, ...)`** (`:2664`; body in `evo/specialtxman.cpp:798-1001`). It runs **before** any input or script checks. Its order:
    1. CbTx payload / `CheckCbTx` (`bad-cbtx-*`)
    2. `bad-cbtx-version`
    3. credit pool indexes (V20)
    4. per-tx V24 `CheckSharedCollateralTemplateOutputs`, then `CheckSpecialTxInner` (ProTx, QC, MNHF, asset lock/unlock; `bad-tx-type-dip3-inactive` if DIP3 is not active)
    5. `CheckCreditPoolDiffForBlock`
    6. `qblockman.ProcessBlock` (quorum commitments)
    7. `BuildNewListFromBlock` and `dmnman.ProcessBlock` (includes **`bad-dip3-enf-block`**: at DIP0003EnforcementHeight the block hash must equal DIP0003EnforcementHash, `evo/deterministicmns.cpp:773-780`)
    8. `bad-cbtx-mnmerkleroot`
    9. `bad-cbtx-quorummerkleroot` (CbTx v ≥ 2)
    10. `CheckCbTxBestChainlock` (`specialtxman.cpp:139-215`; `bad-cbtx-null-clsig`, `-older-clsig`, `-cldiff`, `-invalid-clsig`). It short-circuits when the clsig matches our in-memory best chainlock; that is an optimization with equivalent results if the cached clsig was verified.
    11. `mnhfman.ProcessBlock` (`bad-mnhf-*`)
    12. switch BLS scheme on V19.
    Any exception gives `failed-procspectxsinblock`.
13. Per tx:
    - `CheckTxInputs` (`bad-txns-inputs-missingorspent`, `-premature-spend-of-coinbase`, `-in-belowout`, …; asset unlock uses `GetAssetUnlockFee`)
    - V24 `CheckSharedCollateralSpends` (`:2707-2715`)
    - `bad-txns-accumulated-fee-outofrange`
    - BIP68 `bad-txns-nonfinal` (only if `tx.nVersion >= 2`, `tx_verify.cpp:56`)
    - cumulative sigops `bad-blk-sigops`
    - `CheckInputScripts`, with parallel failure reported as `block-validation-failed`
    - `UpdateCoins`
14. **InstantSend conflict** (`:2800-2818`), only `if (!IsInitialBlockDownload())`. For each tx with lock inputs, find a conflicting islock:
    - if the block is chainlocked, remove the islock
    - else `conflict-tx-lock` (BLOCK_CHAINLOCK)
15. **`IsBlockValueValid`** → `bad-cb-amount` (BLOCK_RESULT_UNSET, so not punished) (`:2840-2849`). **`IsBlockPayeeValid`** → `bad-cb-payee` (`:2858-2863`). Superblock checking is `NoCheck` if `IsSuperblockValidationRequired` is false (`evo/chainhelper.cpp:41-52`); otherwise the mode depends on V24.
16. Undo write, `BLOCK_VALID_SCRIPTS`, best block, EvoDB best block, commit BLS scheme.

ConnectTip (`:3393-3511`) wraps this in an EvoDB transaction and flush, plus mempool cleanup. It has no further consensus checks.

### 8.1 Validity that depends on non-chain state (critical for determinism)

| # | Rule | Dependency | Where |
|---|---|---|---|
| 1 | `bad-chainlock` in AcceptBlockHeader, ConnectBlock, TestBlockValidity | Best ChainLock received over P2P (`clsig`), gated by **SPORK_19** (`chainlock/chainlock.cpp:33,49-96`) | `validation.cpp:4691,2517,4935` |
| 2 | `bad-prevblk-chainlock` and conflict marking; `EnforceBlock` and `MarkConflictingBlock` disconnect and mark blocks when a CL arrives; `FindMostWorkChain` skips `BLOCK_CONFLICT_CHAINLOCK` | Same | `validation.cpp:4641-4645,4046-4148,3543-3559`; trigger `chainlock/handler.cpp:296` |
| 3 | `conflict-tx-lock` | islock DB (P2P) + **SPORK_2** + `!fReindex && !fImporting` (`instantsend.cpp:510-513`) + **`!IsInitialBlockDownload()`** (tip age vs local clock, min chain work, `validation.cpp:1971-1994`) + has-chainlock | `validation.cpp:2800-2818` |
| 4 | Superblock payee and value strictness | Skipped if best-CL height ≥ h or `!mn_sync.IsSynced()` (`evo/chainhelper.cpp:41-52`). Uses **governance trigger objects (P2P)** and `m_superblocks.IsValid()` (cache loaded). Uses the **tip** MN list, not the block's (`GetListAtChainTip`, `masternode/payments.cpp:366,423`) | `masternode/payments.cpp:313-442` |
| 5 | `time-too-new` | Local clock | `validation.cpp:4507` |
| 6 | Checkpoint fork rejection | Contents of the local block index | `validation.cpp:4491-4500` |
| 7 | assumevalid / fScriptChecks | Config + best header + MinimumChainWork. **Also disables CbTx merkle-root, CbTx, and special-tx signature checks** (`fCheckCbTxMerkleRoots`) | `validation.cpp:2543-2572,2664` |
| 8 | Unrequested-block processing, AcceptBlock min-work | Local tip and config | `validation.cpp:4803-4813` |
| 9 | Process-global BLS legacy flag, global `Params()`, EHF singleton | Process state (deterministic if replayed in order) | `validation.cpp:2491`, `versionbits.cpp:244`, `evo/mnhftx.cpp:78` |

**Historical spork gates removed in afcf3d148e (2026-04-09):** SPORK_3 (IS block filtering) and SPORK_9 (superblocks enabled) used to gate rows 3 and 4. SPORK_8 (MN payment enforcement) was removed in 7d58d87f43 (2019).

---

## 9. Historical rules that current code relaxes or no longer enforces
1. **Difficulty for mainnet h ≤ 68589** is only checked to ±50% in difficulty space. The original DGW v1/v2 (34140..68589) and the original KGW and BTC rules are not reproduced exactly. They are protected by checkpoints (1500 … 68899) and nMinimumChainWork.
2. **Checkpoints** reject forks below the highest checkpoint already in the index (mainnet 2487500). They are not a hash check.
3. **assumevalid** (mainnet 2487500): below it, script checks and **CbTx/special-tx signature and merkle-root checks are skipped** (`specialtxman.cpp:817,835,872`). A "verify everything" Rust node must still pass all of these.
4. **DIP0020 opcodes enabled from genesis** (61bc3002b0). This is a retroactive relaxation that relies on no pre-activation use.
5. **Buried deployments.** BIP9 signalling for CSV through WITHDRAWALS is ignored and replaced with fixed heights. The heights for CSV, DIP0001, DIP0008 and others were the real activation heights. BRRHeight was hardened in 2022 and DIP0024's re-bury attempt was reverted (2c296de6dd).
6. **BIP34/65/66** were activated by IsSuperMajority. Heights were corrected in 2018 (4d651621ef): BIP34 changed from 1 to 951, BIP65 from 618892 to 619382, BIP66 from 245429 to 245817.
7. **powLimit** was changed in 2018 with no effect (§3.5).
8. **Testnet** was reset in Dec 2018 (793e74cc9f removed work-based min-difficulty switches), so current rules apply to the whole current testnet. Testnet keeps the LLMQ_25_67 hard-coded height 847000.
9. **Old masternode-payment and budget enforcement** (SPORK_8, pre-12.1 budgets) is gone. Blocks below `nSuperblockStartBlock` accept any payee once synced (`masternode/payments.cpp:410-418`). The legacy subsidy bug for mainnet h ≤ 4500 stays (`validation.cpp:1815`).
10. **The superblock start hash** (614820) and **DIP3 enforcement hash** (1047200) are hard-coded block-hash consensus checks.
11. **DGW v3 first exact height on mainnet is 68590.** nPowDGWHeight (34140) is the first DGW-v3-computed height.

---

## 10. rust-dashcore: what is reusable
- **X11**: `hashes/src/hash_x11.rs` wraps the external `rs-x11-hash` 0.1.8 (optional feature `x11`, `hashes/Cargo.toml:19,28`). I could not inspect the crate source locally, so whether it is C (sph via cc) or pure Rust is unverified. `BlockHash` is X11 only with feature `core-block-hash-use-x11`; **otherwise it is SHA256d** (`dash/src/hash_types.rs:76-94`). Always enable that feature.
- **U256 / Target / CompactTarget**: `dash/src/pow.rs`, a rust-bitcoin port. `from_compact` returns ZERO for negative values and reports no overflow flag (`:162-183`). `Mul`/`Add` **debug_assert on overflow** (`:735-760`), which breaks the C++ wrapping semantics needed in DGW and min-difficulty on devnet. Use `overflowing_mul`, or write a dedicated `ArithU256`.
- **`dash/src/consensus/params.rs`** contains **Bitcoin** values (10-minute spacing, BIP34 at 227931, …). It is unusable for consensus and needs a rewrite.
- **Network magic, genesis**: `dash/src/network/constants.rs` (magic values match), `dash/src/blockdata/constants.rs:111-176` (genesis headers match). There is no devnet-genesis (block 1) derivation.
- **Merkle**: `dash/src/merkle_tree/mod.rs` has no CVE-2012-2459 mutation flag. `Block::compute_merkle_root` uses `txid()` = SHA256d of the full tx, so **the V24 stable asset-unlock txid is missing** (a grep finds nothing).
- **BIP34**: `Block::bip34_block_height` (`dash/src/blockdata/block.rs:326-356`) parses a push and fails on OP_1..OP_16. Core does a prefix compare against `CScript() << h`.
- **dash-spv** header validation (`dash-spv/src/validation/header.rs:18-45`) only checks PoW-met and prev-hash continuity. It has no DGW, MTP, version or time checks. There is no versionbits code anywhere; a grep for bip9, versionbits and falloff is empty. `dash-spv/src/chain/checkpoints.rs` holds MN-list bootstrap checkpoints with chainwork, which are different from Core's checkpoint list.

---

## 11. Difficulty summary for the Rust port
| Subsection | Est. |
|---|---|
| Params tables plus regtest/devnet arg overrides plus devnet genesis mining | S |
| Compact / arith U256 with exact wrapping | S |
| DGW v3, BTC retarget, min-difficulty rules, KGW (tolerance only), mainnet ≤ 68589 tolerance | M |
| X11 (reuse or port sph) | S–M |
| TXID (incl. stable asset-unlock), merkle with mutation, CbTx asset-unlock root in CheckMerkleRoot | S (needs CbTx payload parsing) |
| CheckBlock / ContextualCheckBlockHeader / ContextualCheckBlock with exact reject strings and order | M |
| Versionbits with dynamic thresholds, MinBIP9WarningHeight floor, EHF signal height coupling and caching | M (L with MNHF store) |
| ConnectBlock orchestration and ordering of Dash hooks (excluding hook internals) | M |
| Non-chain-state rules (chainlock/IS/governance/sync) as deterministic, injectable inputs | L (design problem, not code size) |

---

## 12. Gotchas (ordered roughly by risk)
1. **DGW is a running pseudo-average with per-step integer truncation, over 23 intervals vs 24×spacing.** Use the exact `(avg*k + T)/(k+1)` and wrapping 256-bit multiplies (`pow.cpp:94-125`).
2. **The mainnet h ≤ 68589 nBits check is a ±50% double comparison, not equality** (`validation.cpp:4474-4483`). KGW and BTC are never exact on mainnet. Testnet, devnet and regtest are exact from block 1.
3. **Min-difficulty rules depend on the candidate header's own timestamp**: 300 s in the BTC era, 600 s gives ×10, 7200 s gives powLimit. The ×10 wraps mod 2^256 on devnet.
4. **The V24 asset-unlock stable txid** (zeroed trailing 132 bytes, payload v ≥ 2, exact 145-byte payload) feeds the merkle root. A CbTx asset-unlock-root mismatch is `BLOCK_MUTATED`, not invalid.
5. **`MinBIP9WarningHeight` is a consensus floor for all BIP9 deployments.** EHF deployments also need the MNHF signal's mined height, evaluated through `GetSignalsStage(query pindexPrev)`. Bit-12 signals already sit on mainnet and testnet.
6. **The dynamic threshold uses int32 `nAttempt²·period` and truncating `/100/falloff`.** LOCKED_IN is checked before timeout. NEVER_ACTIVE returns FAILED and ALWAYS_ACTIVE returns ACTIVE before any caching.
7. **BIP34 height is a prefix compare of `CScript()<<h` (OP_N for 1..16), and only for heights before DIP3.** After DIP3 the height is in the CbTx, coinbase nType must be 5, and the minimum scriptSig is 1 byte.
8. **assumevalid disables CbTx, merkle-root and special-tx signature checks** (`fCheckCbTxMerkleRoots = fScriptChecks`), not just scripts. A from-scratch validator must still implement and pass them.
9. **Validity depends on P2P and local state**: ChainLocks (SPORK_19), IS locks (SPORK_2, IBD, reindex), governance triggers and mn_sync, local clock, and checkpoints-in-index. Model these as explicit inputs. Note that IsBlockPayee and IsBlockValue use the **tip** MN list, and that failures return `BLOCK_RESULT_UNSET`.
10. **`MAX_STANDARD_TX_SIZE` (policy.h) is consensus after DIP0001. P2SH is always on. DIP0020 opcodes are enabled from genesis. OP_CHECKDATASIG is not a sigop. BIP30 is always enforced on devnet and regtest** (null BIP34Hash). The rust-dashcore `Params` holds Bitcoin values and `BlockHash` is SHA256d without the x11 feature.
