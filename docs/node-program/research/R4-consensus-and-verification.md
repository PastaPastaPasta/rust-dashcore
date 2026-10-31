> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

# R4: Dash Core consensus surface and verification strategy

- **Status:** Phase 0 research report (R4). Read-only research; neither repository was modified.
- **Date:** 2026-10-08.
- **Sources:**
  - Dash Core `develop` @ `15db478750` (`v24.0.0-rc.2-109`, version string 24.1.0-dev). `origin/develop` @ `2340e3a265` is 14 commits ahead; the only consensus delta is the new `evo_shares` deployment (§1.12).
  - rust-dashcore `dev` @ `67b307c6`.
  - Live checks against a **dashd v24.0.0-rc.3** release binary on regtest, in `/tmp/r4-verif-*` and `/tmp/r4-negver`.
- **Citations:** `path:line` refers to the Dash Core tree unless prefixed `rd:` (rust-dashcore).
- **Appendices:** `research/R4-appendix/` holds the full per-area catalogs: every rule, reject string and file:line, plus repro scripts. Section numbers like "A§3" point into them:
  - A: tx + script
  - B: block, PoW, params, versionbits
  - C: subsidy, payments, governance, sporks, credit-pool accounting
  - D: special txs + deterministic MN list
  - E: LLMQ, BLS, ChainLocks, InstantSend, EHF
  - F: verification assets + P2P fingerprint surface
  - G: rust-dashcore reuse audit
- **Difficulty scale** (one strong implementer, including tests): S ≤ 1 week, M 1–3 weeks, L 1–2 months, XL more than 2 months or research-grade exactness work.

---

## 0. Executive summary

1. **Scope of the surface.** Dash consensus is Bitcoin's legacy-script UTXO core, plus seven Dash layers:
   - X11 PoW with the DGW difficulty algorithm (and KGW/BTC retargeting in early history).
   - A float-based, era-dependent subsidy.
   - Masternode, treasury and platform reward splits.
   - 12 special-transaction types, 1–12 (DIP2/3/4, LLMQ, MNHF, asset lock/unlock, V24 shared masternodes).
   - The deterministic masternode-list state machine.
   - LLMQ commitment and member selection, including DIP24 rotation, under two BLS schemes.
   - In-coinbase commitments: MN-list and quorum merkle roots, best ChainLock, credit-pool balance, and asset-unlock root.

   There is **no segwit, no BIP143 and no SIGHASH_FORKID**: one legacy sighash only, which also commits to the special-tx payload. The DIP0020 opcodes (OP_CAT, OP_SPLIT, OP_AND/OR/XOR, OP_DIV/MOD, OP_NUM2BIN, OP_BIN2NUM, OP_CHECKDATASIG(VERIFY)) are enabled **from genesis**. Only five script flags are consensus: P2SH (always on), DERSIG, CLTV, CSV and NULLDUMMY.
2. **Hardest parts, in order:**
   - (a) The DMN list state machine (XL).
   - (b) Quorum member selection with DIP24 rotation and snapshots (XL).
   - (c) BLS legacy/basic semantics, driven by a process-global "legacy until V19" flag (L).
   - (d) The script interpreter with Dash's quirks (L).
   - (e) Governance and superblocks (XL, but mostly *not* deterministic consensus).
   - (f) The masternode protocols: DKG (XL), signing sessions (L), CoinJoin server (L).
3. **Validity is not a pure function of the chain.** These checks depend on P2P-received or local state:
   - `bad-chainlock` and ChainLock-conflict marking.
   - `conflict-tx-lock` (InstantSend; only outside IBD).
   - Superblock payee checks: governance objects and votes, mnsync status, and "skipped if chainlocked".
   - `time-too-new` (local clock) and checkpoint-in-index.

   A replaying node enforces only a **deterministic core** (§1.10). The Rust design must model the rest as injectable node-local oracles, the same way dashd does.
4. **assumevalid means more on Dash.** Below assumevalid, dashd skips not only scripts but also **CbTx checks, merkle roots, credit-pool balance, quorum-commitment and ProTx signatures** (`validation.cpp:2543-2572,2664`). A from-genesis Rust validator must pass all of these with assumevalid off.
5. **An unfixed Dash Core security issue (EMBARGO-1).** EMBARGO-1: see ~/private/embargo/EMBARGO-1.md. The Rust node replicates Core's behaviour (PLAN §7 handling); pasta owns private disclosure (DEC-14).
6. **Moving target.** Mainnet runs v23.1.x. V24 is NEVER_ACTIVE on mainnet in every branch. Testnet started V24 signalling on 2026-09-24. `origin/develop` and the rc.3 backport branch add a second deployment, `evo_shares` (bit 14). "Bit-for-bit" must mean bit-for-bit with a **pinned dashd release** per network, tracked continuously.
7. **rust-dashcore today** is an SPV stack with **no validation engine**. Reusable:
   - SML entry hash and MN/quorum merkle roots (verified 15/15 against a Core vector and on 6/6 mainnet diffs).
   - Quorum scoring and modifier.
   - Rotation reconstruction from snapshots.
   - CL/IS request ids and quorum selection.
   - dash-pkc BLS (both schemes, with fixes).
   - X11, the RPC JSON types, and the dashd/masternode test harness.

   Must be replaced:
   - the tx parser (u16 version, discarded payload length, segwit framing, re-encode hashing);
   - Bitcoin-valued `consensus/params.rs`;
   - the merkle root without mutation detection;
   - the Bitcoin opcode table.
8. **Verification plan: seven layers.**
   - **L0** Core's JSON vectors plus extracted C++ vectors, and `test_dash` from the release tarball as the spec oracle.
   - **L1** In-process differential testing and fuzzing against a C++ "oracle shim" over Core's consensus libraries. libdashconsensus still exists but is deprecated and offers only `verify_script` with five flags.
   - **L2** Full mainnet and testnet replay with per-block state digests compared against a dashd oracle:
     - `gettxoutsetinfo muhash <h>` (coinstatsindex)
     - `protx listdiff h-1 h`
     - `getblockstats`
     - `masternode payments`
     - `getcreditpoolinfo`
     - `quorum listextended`
     - `quorum rotationinfo`
     - `getdeploymentinfo`

     The in-block CbTx commitments check the MN list, quorums, CL and credit pool for free. Estimated cost: hours per replay on this box, plus a one-off digest extraction of about 9M RPC calls (≈1–4 h).
   - **L3** Differential block and tx mutation testing via `getblocktemplate` proposal mode (full validation without PoW, returns the reject reason) and `testmempoolaccept`.
   - **L4** Dash's Python functional suite drives the Rust node: 35 tier-1 tests need only ~60 RPCs. Masternode tests run as mixed networks where dashd node 0 holds the wallet.
   - **L5** Mixed dashd/Rust masternode networks: regtest framework, then a devnet soak on agentbox, then testnet.
   - **L6** P2P fingerprint conformance: handshake and order probes, behaviour probes, timing statistics, an adversarial classifier.

---

## 0.1 Findings that change the plan

| # | Finding | Consequence |
|---|---|---|
| F1 | EMBARGO-1 (an unfixed Dash Core security issue) | EMBARGO-1: see ~/private/embargo/EMBARGO-1.md. Replicate Core's behaviour under PLAN §7 handling; pasta owns private disclosure (DEC-14). |
| F2 | `fScriptChecks` (assumevalid) also gates CbTx, merkle-root, credit-pool, commitment-sig and ProTx-sig checks | Rust replay must run with assumevalid=0 to be meaningful. Rust's own assumevalid must skip exactly the same set (perf parity). |
| F3 | Superblock *payees* are never checked during IBD or reindex, only `value ≤ subsidy+fees+limit`. Old-budget windows (mainnet 332320+16616k … 614819) accept **any** coinbase value when not checking (`masternode/payments.cpp:282-287`). Block 332320 overpays ~1176 DASH. | Replay cannot verify governance. Governance parity needs regtest/devnet functional tests (L4/L5). Rust must reproduce the "accept unbounded" rule or it will reject mainnet history. |
| F4 | Mainnet difficulty for h ≤ 68589 is only a ±50% float-tolerance check (`validation.cpp:4474-4483`). On mainnet the BTC and KGW eras are therefore only tolerance-checked, and KGW only ever ran on mainnet. Other networks check exactly from block 1, including their BTC-retarget era (testnet < 4002, devnet < 4001, regtest < 15200). | KGW is low risk. DGW must be bit-exact from 68590. |
| F5 | `merkleRootQuorums` includes *disabled* LLMQ types. 24 stale legacy-BLS LLMQ_50_60 commitments are still leaves today (E§2.5, verified on a mainnet fixture). | The quorum-root implementation must iterate all configured types, not only enabled ones. |
| F6 | MN payments are unvalidated before DIP3 enforcement (h < 1047200). Payee checks are skipped (accept anything) when every MN is PoSe-banned, but the credit pool is still credited (C§3.2). | Implement exactly. Do not "tighten". |
| F7 | `gettxoutsetinfo` default `hash_serialized_2` has an operator-precedence bug: it writes `VARINT(1)` instead of the height/coinbase code (`kernel/coinstats.cpp:77`). Reproduced in Python against dashd. | Rust coinstats must replicate it. Use `muhash` (coinstatsindex) for per-height comparison. |
| F8 | A canonical full Dash-state codec exists: `evo::EvoSnapshot`, `CanonicalMNListHash` (`evo/snapshot.h:501-520`, `evo/snapshot.cpp:150-155`). It is **not wired to RPC yet**. | Use it via a test-only dashd patch for per-block full-state digests (§2.3.2), and as the future snapshot-sync interop format. |
| F9 | `getblocktemplate` `mode=proposal` runs `TestBlockValidity(fCheckPOW=false)` on any block built on the tip and returns the reject reason (`rpc/mining.cpp:731-759`). | A zero-PoW, side-effect-free block-validity oracle usable on regtest, testnet *and* mainnet (L3). |
| F10 | The functional framework already supports per-node binaries (`add_nodes(binary=[...])`). Masternodes force `-disablewallet` (`init.cpp:1094-1103`). | Mixed dashd/Rust masternode tests need no Rust wallet. |
| F11 | dashd's handshake is distinctive. After `verack`: `sendheaders2\|sendheaders`, `sendcmpct(0,1)`, `senddsq(1)`, `ping`, `getheaders2`, then `getsporks` (+0.5–2 s) and `govsync` (+8 s). It never sends `feefilter`, `wtxidrelay` or `reject`. The version message is 137 bytes with `mnauthChallenge` and `fMasternode`. | Fingerprint checklist (§2.7). Captured live from rc.3 (F§5). |
| F12 | rust-dashcore test fixtures `mn_list_diff_0_2227096.bin` and the testnet `.bin` are **not Core-produced**: their IPv4 encoding is wrong and their MN root mismatches. A "Core ground truth" test relies on one of them (G§0). | Regenerate fixtures from a dashd oracle before using them as vectors. |

---

## 0.2 What "bit-for-bit" means operationally

For every input and every pinned reference release `R`:

- Inputs: chain state plus block, chain state plus tx (mempool), P2P message sequence.
- Required outputs, identical to dashd `R`:
  1. **Validity verdict.**
  2. **`BlockValidationResult` / `TxValidationResult` class and reject string.** These drive misbehaviour scoring, RPC errors and functional-test assertions.
  3. **Post-state:**
     - UTXO set;
     - DMN list, all fields including `nLastPaidHeight`, PoSe, `confirmedHash` and `nConsecutivePayments`;
     - mined-commitment index;
     - rotation snapshots (`llmq_S`);
     - credit pool;
     - MNHF signals;
     - versionbits state;
     - process BLS-scheme state;
     - block-index status flags, including `BLOCK_CONFLICT_CHAINLOCK`.
  4. Mined block templates that `R` accepts.

**Pinned references:**
- mainnet history and tip: **v23.1.x**, the current mainnet release;
- testnet and upcoming rules: **v24.0.0-rcN**, then final;
- watch list: `develop`, rebased weekly.

Historical software versions are *not* the reference. Current dashd is a relaxation of historic rules (e.g. DIP0020 opcodes from genesis, removed spork gates, `MnNetInfo` invalid-address normalization). The exceptions are a few stricter-than-history items that the chain scans in §2.3.5 must clear: the `>= 3` special-version parsing (§1.3) and `bestCLHeightDiff < height`. So matching current dashd is necessary and sufficient to validate history.

---

## 1. Consensus surface catalog

### 1.1 Validation pipeline and the order of Dash hooks

`ProcessNewBlock` (`validation.cpp:4865-4905`) runs `CheckBlock`, then `AcceptBlock`, then `ActivateBestChain`. `AcceptBlock` runs `AcceptBlockHeader` → `ContextualCheckBlockHeader`, then `ContextualCheckBlock`. `ActivateBestChain` runs `ConnectTip` → `ConnectBlock`.

**`ConnectBlock` (`validation.cpp:2470-2933`)**, in order:

| # | Step | Line | Deterministic? |
|---|---|---|---|
| 1 | `ScopedBLSLegacyScheme(!V19 active at pindex)`: process-global BLS scheme | 2491 | yes (process state) |
| 2 | `CheckBlock` again | 2506 | yes |
| 3 | `HasConflictingChainLock` → `bad-chainlock` | 2517 | **no** (P2P CLSIG) |
| 4 | EvoDB consistency (abort, not invalid) | 2526-2532 | n/a |
| 5 | Genesis: skip txs | 2537 | yes |
| 6 | assumevalid → `fScriptChecks` | 2543-2572 | config |
| 7 | BIP30 (`IsBIP30Repeat` always false; skipped on the BIP34Hash chain) | 2591-2613 | yes |
| 8 | `bad-sb-start`: hash pin at `nSuperblockStartBlock` (mainnet 614820) | 2617-2623 | yes |
| 9 | Lock-time flags (CSV) and `GetBlockScriptFlags` | 2627-2633, 2420-2449 | yes |
| 10 | `GetBlockSubsidy(pindex)` uses **prev** nBits and height | 2659, 1885-1890 | yes |
| 11 | **`ProcessSpecialTxsInBlock`** (runs *before* any UTXO update of this block; `fCheckCbTxMerkleRoots = fScriptChecks`) | 2664; `evo/specialtxman.cpp:798-1001` | yes |
| 12 | Per tx: `CheckTxInputs`, V24 shared-collateral spends, fee range, BIP68, cumulative sigops, `CheckInputScripts`, `UpdateCoins` | 2687-2767 | yes |
| 13 | Parallel script-check join → `block-validation-failed` | 2777-2780 | yes |
| 14 | InstantSend conflict → `conflict-tx-lock` (**only if `!IsInitialBlockDownload()`**; a CL'd block overrides) | 2800-2818 | **no** |
| 15 | `IsBlockValueValid` → `bad-cb-amount`; `IsBlockPayeeValid` → `bad-cb-payee` (both `BLOCK_RESULT_UNSET`, not punished) | 2840-2863 | partly (superblock part is node-local) |
| 16 | Undo write, raise validity, commit BLS scheme | 2884-2933 | — |

`ProcessSpecialTxsInBlock` runs these steps (D§1.4):
1. CbTx parse and `CheckCbTx`; `bad-cbtx-version` if DIP3 is active and the CbTx is missing.
2. Load credit-pool indexes (V20).
3. Per tx: V24 `CheckSharedCollateralTemplateOutputs` and `CheckSpecialTxInner`. Every special tx is checked against the **pprev** state.
4. `CheckCreditPoolDiffForBlock`.
5. `qblockman.ProcessBlock`: quorum commitments; creates rotation snapshots.
6. `BuildNewListFromBlock` and `dmnman.ProcessBlock`, including the `bad-dip3-enf-block` hash pin at 1047200.
7. `bad-cbtx-mnmerkleroot`.
8. `bad-cbtx-quorummerkleroot`.
9. `CheckCbTxBestChainlock`.
10. `mnhfman.ProcessBlock`.
11. Flip the BLS scheme to basic at the end of block V19Height−1, i.e. once `DeploymentActiveAfter(pindex, V19)` (`specialtxman.cpp:985-990`).

Any exception gives `failed-procspectxsinblock`. That includes duplicate unique properties inside one block, thrown from `AddMN`/`UpdateMN`.

Header and context checks (B§4):
- `CheckBlockHeader`: `high-hash`, `devnet-genesis`.
- `CheckMerkleRoot`: `bad-txnmrklroot` and `bad-txns-duplicate` (CVE-2012-2459), both `BLOCK_MUTATED`. V24 adds `bad-cbtx-assetunlockmerkleroot`, also MUTATED.
- `CheckBlock`: `bad-blk-length` (relaxed 2 MB), `bad-cb-missing`/`-multiple`, `CheckTransaction`, legacy sigops ≤ 40000.
- `ContextualCheckBlockHeader`:
  - `bad-diffbits`
  - checkpoints → `bad-fork-prior-to-checkpoint`
  - `time-too-old` (time ≤ MTP-11)
  - `time-too-new` (time > local clock +7200 s)
  - `bad-version(0x…)` for BIP34/66/65
- `AcceptBlockHeader`, after `ContextualCheckBlockHeader` returns: a conflicting CL → `bad-chainlock`, and the header is stored as conflicting (`validation.cpp:4691-4696`).
- `ContextualCheckBlock`:
  - BIP113 cutoff
  - DIP0001 1/2 MB
  - `IsFinalTx`
  - `ContextualCheckTransaction`
  - sigops 20k/40k
  - BIP34 prefix check **only in [BIP34Height, DIP0003Height)**
  - post-DIP3 coinbase must be `nType=5` (`bad-cb-type`)

**Difficulty:** the orchestration and reject-string parity are M. The hook internals are scored per section.

### 1.2 Parameters and activations (mainnet / testnet)

All of the following are now **buried heights** (`consensus/params.h:22-39`; `chainparams.cpp:178-197` main, `:378-397` test). Activation means "rule applies to blocks at height ≥ H" (`DeploymentActiveAfter(prev)` ⇔ `prev.h+1 ≥ H`), except where noted.

| Rule | Mainnet H | Testnet H | Notes |
|---|---|---|---|
| BTC retarget → KGW → DGW | KGW 15200, DGW 34140; exact nBits from 68590 | DGW 4002 (no KGW) | B§3 |
| BIP34 (height in coinbase) | 951 | 76 | Prefix compare, only until DIP3 |
| BIP66 DERSIG | 245817 | 2075 | |
| BIP65 CLTV | 619382 | 2431 | |
| CSV (BIP68/112/113) | 622944 | 8064 | BIP113 keyed on CSV |
| DIP0001 (2 MB, 40k sigops, 100 kB tx as consensus) | 782208 | 5500 | `validation.cpp:382` uses the *policy* constant `MAX_STANDARD_TX_SIZE` |
| BIP147 NULLDUMMY | 939456 | 4300 | |
| DIP0003 (special txs, CbTx, DML) | 1028160 | 7000 | |
| DIP0003 enforcement (MN payments; hash pin) | 1047200 | 7300 | `bad-dip3-enf-block` |
| DIP0008 (CbTx v2 `merkleRootQuorums`, ChainLocks era) | 1088640 (CbTx v2 required from **1088641**) | 78800 | Off-by-one: `DeploymentActiveAt(*pindexPrev)` |
| BRR (block reward reallocation) | 1374912 (realloc schedule from 1379128) | 387500 | C§2.1 |
| DIP0020 (now only gates LLMQ_100_67) | 1516032 | 414100 | Opcodes enabled from genesis since `61bc3002b0` |
| DIP0024 (rotation) / first rotated quorum | 1737792 / 1738698 | 769700 / 770730 | LLMQ_50_60 disabled on mainnet from 1738699 |
| V19 (basic BLS, EvoNodes, ProTx v2) | 1899072 | 850100 | Global BLS flag flips |
| V20 (CbTx v3, credit pool, asset lock/unlock, MNHF, CL-based quorum modifier, 20% treasury, 75% MN) | 1987776 | 905100 | |
| MN_RR (platform share to credit pool; EvoNode streak ends) | 2128896 | 1066900 | Activated by EHF |
| WITHDRAWALS (unlock limit 2000 DASH/block) | 2201472 | 1148500 | Activated by EHF |
| LLMQ_25_67 | — | **enabled when `pindexPrev.h ≥ 847000`, i.e. blocks ≥ 847001 (hard-coded)** | `validation.cpp:6666` |
| V24 (BIP9 + EHF, bit 13) | NEVER_ACTIVE | start 2026-09-24 | §1.12 |
| evo_shares (bit 14; origin/develop and rc.3 only) | NEVER_ACTIVE | start 2026-09-24 (window 100, EHF), but v24 MNs do not sign its MNHF signal | §1.12 |

Other consensus constants:
- powLimit `00000fff…` (main/test); spacing 150 s; difficulty window 576.
- `nSubsidyHalvingInterval` 210240.
- Budget start 328008; superblock start 614820, cycle 16616 (first SB 631408).
- MN payment increase 158000 + 17280·k.
- `nMasternodeMinimumConfirmations` 15.
- `nCreditPoolPeriodBlocks` 576 (regtest 100); this lives outside `Consensus::Params` and is read via global `Params()`.
- Coinbase maturity 100. `MAX_MONEY` = 21M·COIN.
- Genesis hashes `00000ffd…7ab6` (main), `00000baf…bc2c` (test).
- Devnet block 1 is a mined "devnet genesis" (`chainparams.cpp:43-107`).

Full per-network tables (devnet and regtest too), checkpoints (39 mainnet, up to 2487500), assumevalid and minimum chain work are in B§2.

**Regtest and devnet knobs** must be reproduced exactly for L4/L5, including the names each one rejects: `-testactivationheight`, `-vbparams`, `-dip3params`, `-budgetparams`, `-llmqtest*params`, `-llmqchainlocks`, `-llmqinstantsenddip0024`, `-llmqplatform`, `-llmqmnhf`, `-llmqdevnetparams`, `-highsubsidyblocks`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-devnet=` (B§2.7). Difficulty: S.

### 1.3 Transaction layer: serialization, context-free and contextual checks, sigops, policy

**Where:** `primitives/transaction.{h,cpp}`, `consensus/tx_check.cpp`, `consensus/tx_verify.cpp`, `consensus/consensus.h`, `consensus/amount.h`, `validation.cpp:351-386` (ContextualCheckTransaction), `policy/policy.{h,cpp}`, `validation.cpp:828-1225` (MemPoolAccept). Full detail in A§1–2 and A§5.

**Serialization (DIP2)** (`primitives/transaction.h:269-278, 354-365`):
- Wire format: one 32-bit field (`nVersion` in the low half, `nType` in the high half), vin, vout, nLockTime, then `vExtraPayload` (compactsize bytes) iff `nVersion >= 3 && nType != 0`. There is no witness.
- `IsSpecialTxVersion()` is `nVersion >= 3`.
- Payload structs are decoded lazily by `GetTxPayload`, which must consume the payload exactly (`evo/specialtx.h:17-28`).
- **V24 stable txid:** when `nVersion>=3 && nType==9 && payload.size()==145 && payload[0]>=2`, the txid is SHA256d over the serialization with the last 132 bytes zeroed. `GetInstanceHash()` is the full hash (`primitives/transaction.cpp:74-125`). The rule keys on raw bytes, not on activation.

#### 1.3.1 EMBARGO-1

EMBARGO-1: see ~/private/embargo/EMBARGO-1.md.

**CheckTransaction** (`consensus/tx_check.cpp:14-75`), context-free and identical at every height:
- Empty vin/vout. Exempt: types 6 and 7 may have both empty; type 9 may have empty vin. The exemption keys on `nType` **without checking nVersion**.
- Serialized size > 1,000,000 → `bad-txns-oversize`. Payload > 10,000 → `bad-txns-payload-oversize`.
- Output value range and running-sum `MoneyRange`.
- Duplicate inputs.
- Coinbase scriptSig length: 2..100, or 1..100 when `nType==5`.
- Null prevouts in a non-coinbase.

**ContextualCheckTransaction** (`validation.cpp:351-386`):
- From DIP3: special-version txs must have `nType ∈ {0..12}`, and the coinbase must be type 5. Non-special versions must have `nType==0`, else `bad-txns-type`.
- From DIP0001: serialized size > 100,000 → `bad-txns-oversize`. This is consensus `>`, while policy is `>=`.

**Finality and locks:**
- `IsFinalTx` (`tx_verify.cpp:19-39`). The cutoff is the MTP of prev from CSV height, else block time (`validation.cpp:4535-4543`).
- BIP68 `CalculateSequenceLocks`/`EvaluateSequenceLocks` (`tx_verify.cpp:41-116`). The ConnectBlock flag is set from CSV height (`validation.cpp:2627-2630`). Coins created earlier in the same block have `nHeight = h`.

**CheckTxInputs** (`tx_verify.cpp:163-208`), reject strings in this order:
1. `bad-txns-inputs-missingorspent`
2. `bad-txns-premature-spend-of-coinbase` (maturity 100)
3. `bad-txns-inputvalues-outofrange`
4. `bad-txns-in-belowout`
5. `bad-txns-fee-outofrange`

**Asset unlocks** short-circuit to `fee = payload.fee` with no inputs (`tx_verify.cpp:165-167`; `evo/assetlocktx.cpp:255-267`).

**Sigops** (`tx_verify.cpp:118-161`, `script/script.cpp:154-200`): legacy count plus P2SH count (P2SH is always on). CHECKMULTISIG counts 20, or N when accurate. **OP_CHECKDATASIG(VERIFY) counts 0.** Limits are `MaxBlockSigOps = MaxBlockSize/50` = 20,000 (pre-DIP0001) or 40,000. The count is enforced in three places with different semantics:
- `CheckBlock`: relaxed legacy count, limit 40k.
- `ContextualCheckBlock`: legacy count.
- `ConnectBlock`: cumulative legacy+P2SH, checked *before* that tx's scripts.

**Policy only (not consensus)** (A§5):
- Size and version: `MAX_STANDARD_TX_SIZE` 100,000 (`>=`), `MIN_STANDARD_TX_SIZE` 65, standard versions 1..3.
- Dust 3000 sat/kB (P2PKH dust 546).
- Script limits: scriptSig ≤ 1650 bytes and push-only, `MAX_P2SH_SIGOPS` 15, `MAX_STANDARD_TX_SIGOPS` 4000.
- One OP_RETURN ≤ 83 bytes. Bare multisig ≤ 3 keys.
- Fees: min relay fee 1000 sat/kB.
- **No RBF.**
- Special-tx mempool rules (`CheckSpecialTxForMempool`): ProTx collateral must be confirmed; `protx-dup` conflicts across addresses, keys and collateral; `qc-not-allowed`; asset-unlock index rules; `IsStandardSpecialTx`.
- InstantSend lock conflicts `tx-txlock-conflict`.
- `STANDARD_SCRIPT_VERIFY_FLAGS` = 0x147DF.

**History:**
- DIP2 (2018) split the 32-bit version. Pre-DIP2 txs are reinterpreted under the split; e.g. rust-dashcore notes a mainnet tx with type bytes 0x2a, which Core reads as `nVersion=0`.
- Before Feb 2024 (`72d2008901`, v21), serialization, deserialization, the sighash payload inclusion, `CheckSpecialTx` and most evo code used `nVersion == 3`; only `ContextualCheckTransaction` used `>= 3`. Today everything uses `>= 3`. So a historic tx with `nVersion > 3 && nType != 0` would now be parsed *with* a payload, changing its txid and the block's merkle root. Since current dashd syncs mainnet, none should exist. Action: confirm with a chain scan (§2.3.5).

**Difficulty:** serialization M (it must be Core-faithful); checks S–M; sigops S; policy and mempool L (special-tx and IS interplay).

**Rust reuse:** none for checks. Replace the rust-dashcore tx model: it uses a u16 version, ignores the payload length prefix, keeps segwit framing, and hashes re-encoded structs; three divergences were demonstrated (G§2.1). base-sdk `dash-primitives` keeps a raw `extra_payload` and an i16 version (R3), so evaluate it as the tx model.

### 1.4 Script interpreter

**Where:** `script/interpreter.{h,cpp}` (1717 LOC), `script/script.{h,cpp}`, `pubkey.cpp`, `validation.cpp:2154-2238` (CheckInputScripts), `script/sigcache.cpp`. Full detail in A§3–4.

**Flags and activation** (`interpreter.h:38-114`; `GetBlockScriptFlags` `validation.cpp:2420-2449`):

| Flag | Consensus? | Mainnet height |
|---|---|---|
| P2SH | yes | **always**, every network, no exception block (`5033bdef73`) |
| DERSIG | yes | 245817 |
| CHECKLOCKTIMEVERIFY | yes | 619382 |
| CHECKSEQUENCEVERIFY | yes | 622944 |
| NULLDUMMY | yes | 939456 |
| STRICTENC, LOW_S, MINIMALDATA, DISCOURAGE_UPGRADABLE_NOPS, CLEANSTACK, NULLFAIL, CONST_SCRIPTCODE | policy only (`STANDARD_SCRIPT_VERIFY_FLAGS`) | — |
| SIGPUSHONLY | neither: not in the standard flags; push-only scriptSigs are enforced by `IsStandardTx` ("scriptsig-not-pushonly", `policy/policy.cpp:107`) | — |

There is no MINIMALIF and no witness flag. Bit 15, the retired `SCRIPT_ENABLE_DIP0020_OPCODES`, is ignored.

**Opcodes:**
- Bitcoin's legacy set, plus DIP0020: OP_CAT, OP_SPLIT (0x7f), OP_AND/OR/XOR, OP_DIV, OP_MOD, OP_NUM2BIN, OP_BIN2NUM, **OP_CHECKDATASIG (0xba)/OP_CHECKDATASIGVERIFY (0xbb)**.
- All are enabled unconditionally since `61bc3002b0` (Oct 2024, a pure relaxation; before that they were gated at DIP0020Height 1516032).
- Still disabled even inside unexecuted branches: INVERT, 2MUL, 2DIV, MUL, LSHIFT, RSHIFT. OP_VERIF and OP_VERNOTIF are bad even when unexecuted.

**Exact semantics to replicate** (A§3.4):
- OP_NUM2BIN: the number operand has no 4-byte limit and no minimality check; a negative size becomes a huge `uint64` (PUSH_SIZE).
- OP_BIN2NUM: the minimal form must be ≤ 4 bytes.
- OP_SPLIT: range checks.
- AND/OR/XOR: operands must be equal size.
- DIV/MOD: truncate toward zero; division by zero gives its own error.
- **CHECKDATASIG:**
  - The signature must pass the CHECKSIG encoding check, i.e. under DERSIG strict DER *plus exactly one trailing byte*.
  - It verifies `SHA256(msg)` (single hash) over the whole sig, including the trailing byte, via the lax parser.
  - It bypasses `BaseSignatureChecker` and the sigcache, and costs no sigops.
- C++ exceptions (scriptnum overflow or non-minimal, empty pops) map to **UNKNOWN_ERROR**.
- Error codes must match Core's `ScriptError` exactly (functional tests assert `mandatory-script-verify-flag-failed (…)` strings).

**Sighash** (`interpreter.cpp:1340-1534`): **legacy only.** There is no BIP143 and no FORKID; `SigVersion` has only `BASE`. Rules:
- The SIGHASH_SINGLE bug returns `uint256::ONE`.
- Base type is `nHashType & 0x1f`; the full 32-bit hashtype is appended.
- OP_CODESEPARATORs are removed from the scriptCode, and FindAndDelete runs on the signature push. For an empty sig this removes every OP_0.
- Version and type are serialized exactly as the tx serializer does it.
- **The special-tx payload is serialized into the sighash.**

**ECDSA consensus layer** (`pubkey.cpp:182-197`): lax DER parse, then `secp256k1_ecdsa_signature_normalize` (high-S verifies at consensus). Hybrid 06/07 pubkeys are accepted with a parity check. Sigs with R/S overflow parse to zero and then fail.

**Execution:** script cache keyed by `SHA256(salt‖txid‖flags)`; parallel check queue. Failures retry without the non-mandatory flags, to tell `non-mandatory-script-verify-flag` from `mandatory-script-verify-flag-failed` (`validation.cpp:2197-2225`).

**libdashconsensus:**
- Still built by default (`--with-libs`, `configure.ac:775-778`) and shipped in the release (`lib/libdashconsensus.so`).
- Deprecated; it will disappear when the upstream removal is backported.
- API: `dashconsensus_verify_script(spk, len, tx, len, nIn, flags, err*)` plus `dashconsensus_version()`. Accepted flags are only P2SH, DERSIG, NULLDUMMY, CLTV and CSV. It reports no `ScriptError`; there is no sighash or EvalScript access (A§6).
- Usable now as a cheap FFI oracle for accept/reject; not sufficient for full differential coverage (§2.2).

**Difficulty:** interpreter L, sighash S–M, ECDSA layer M.

**Rust reuse:**
- No interpreter exists.
- The `bitcoinconsensus` feature wraps Bitcoin Core 0.20.2's library and is wrong for Dash.
- The opcode table is Bitcoin's: 0xba is named OP_CHECKSIGADD, and the DIP0020 opcodes are listed as disabled.
- `legacy_signature_hash` includes the payload but does not strip OP_CODESEPARATOR (G§2.3).
- base-sdk `dash-script` (opcodes, sigops) is a candidate starting point (R3).

### 1.5 Block header, PoW and difficulty, block limits

**Where:** `primitives/block.cpp:13-19`, `hash_x11.h:25-90`, `crypto/x11/*`, `pow.cpp:15-256`, `arith_uint256.{h,cpp}`, `validation.cpp:4305-4594`, `consensus/merkle.cpp`. Full detail in B§3–5.

**Hashing:**
- Block hash = **X11** of the 80-byte header: 11 chained sph functions over 64-byte intermediates, first 32 bytes of the echo output.
- Txids and the merkle tree use SHA256d. The exception is the V24 stable asset-unlock txid (§1.3).
- Merkle root follows Bitcoin, with a mutation flag (CVE-2012-2459).

**Difficulty algorithms over history:**

| Algorithm | Mainnet use | Exactness |
|---|---|---|
| BTC retarget | h < 15200 (576-block interval) | ±50% tolerance only |
| KGW | 15200 ≤ h < 34140; uses libm `pow` and floats | ±50% tolerance only |
| DGW v3 | from 34140 | **exact from 68590** |

For **mainnet h ≤ 68589**, `ContextualCheckBlockHeader` checks only `|diff(nBits) − diff(expected)| ≤ 0.5·diff(nBits)` in double precision (`validation.cpp:4474-4483`). All other networks, and mainnet from 68590, require exact `nBits` (`bad-diffbits`).

**DGW v3 exactness points** (`pow.cpp:81-132`):
- The pseudo-average is `(avg*k + T)/(k+1)`, truncating at every step.
- The actual timespan covers **23 intervals** but is compared against 24×150 s; this off-by-one gives ~2.626 min observed spacing.
- Clamp to [1200, 10800].
- `arith_uint256` wraps mod 2^256. `*= int64` resolves to the `uint32` overload.

**Testnet and devnet min-difficulty rules** depend on the new header's own timestamp:
- In the BTC era: +300 s gives powLimit.
- Otherwise: +600 s gives ×10 (which wraps on devnet), and +7200 s gives powLimit.

Regtest never retargets (`fPowNoRetargeting`).

**Time:**
- Reject `time-too-old` if `time ≤ MTP(11)`.
- Reject `time-too-new` if `time > now + 7200`. This uses the **local clock**.

**Version:** `bad-version` if <2 after BIP34, <3 after BIP66, <4 after BIP65.

**Size limits:**
- Block: 1 MB, then 2 MB from DIP0001 (`MaxBlockSize`). `CheckBlock` uses the relaxed 2 MB bound.
- Tx: 1 MB always; 100 kB from DIP0001.
- Payload: 10 kB.

**BIP30:** always enforced on devnet and regtest, because their BIP34Hash is null (B§7).

**Checkpoints:** a fork is rejected only below the highest checkpoint *present in the index*; there is no per-height hash check. mainnet has 39 checkpoints up to 2487500.

**Hard-coded block-hash pins:** `bad-sb-start` (614820) and `bad-dip3-enf-block` (1047200).

**Chain selection:** most work, then lower `nSequenceId`. Chains marked `BLOCK_CONFLICT_CHAINLOCK` are skipped (`validation.cpp:3543-3559`; node-local, §1.9.5).

**Difficulty:** M overall. X11 is S (reuse); compact and U256 arithmetic with exact wrapping is S; DGW, the tolerance rule and the min-difficulty rules are M; header and block checks with reject-string parity are M.

**Rust reuse:**
- X11 via `rs-x11-hash` (C sph). Enable `core-block-hash-use-x11`, or `BlockHash` silently becomes SHA256d.
- `from_compact` has no overflow flag.
- U256 `Mul` debug-asserts on overflow, but Core wraps.
- There is no DGW, MTP or version check.
- `consensus/params.rs` holds Bitcoin values (B§10, G§2.5).

### 1.6 Versionbits (Dash-modified BIP9 with EHF)

**Where:** `versionbits.{h,cpp}`, `deploymentstatus.h`, `deploymentinfo.cpp`, `evo/mnhftx.cpp:61-92` (`GetSignalsStage`). Full detail in B§6.

**State machine:**
- `NEVER_ACTIVE` → FAILED; `ALWAYS_ACTIVE` → ACTIVE.
- The period is `nWindowSize` (mainnet V24: 4032).
- **The walk-back stops at `MinBIP9WarningHeight`**, which makes it a *consensus floor*: no mainnet deployment can leave DEFINED before ~2203488.
- LOCKED_IN is evaluated before timeout.

**Dynamic threshold:** `max(thrMin, thrStart − (int)(attempt²·period)/100/falloff)`. The product is computed in **int32**; it overflows at ~730 attempts, so a Rust implementation must not panic.

**EHF deployments** (`useEHF`): DEFINED → STARTED additionally needs a mined MNHF signal for the bit, with `pindexPrev.h ≥ signalHeight` (`versionbits.cpp:38,69-91,238-252`). `GetSignalsStage` is empty before V20. It drops signals whose block MTP predates the start of the deployment that *currently* uses that bit, so bit reuse is handled.

**Historic signals:** mainnet carries MNHF signals for bits 10 (MN_RR), 11 (WITHDRAWALS) and **12 (the abandoned V23)**. Unknown bits are "valid" (`chainparams.cpp:109-130`), so these signals must be accepted and stored. V24 moved to bit 13 for this reason (`1be3ac299a`).

**History:**
- All mainnet deployments up to WITHDRAWALS are buried at their real activation heights (B§6.5).
- BIP34/65/66 were IsSuperMajority-activated; their heights were corrected in 2018.
- Today historical activation is decided purely by height; signalling bits in old headers are never consulted.

**Difficulty:** M, or L if the MNHF signal store is counted here.

**Rust reuse:** none.

### 1.7 Block reward: subsidy, MN/treasury/platform split, payee selection, superblocks, sporks

**Where:** `validation.cpp:1785-1890` (subsidy), `masternode/payments.cpp` (`CMNPaymentsProcessor`), `evo/deterministicmns.cpp:132-194` (`GetMNPayee`), `governance/superblock.cpp`, `governance/object.cpp`, `governance/signing.cpp`, `evo/chainhelper.cpp:41-52`, `spork.{h,cpp}`. Full detail in C.

#### Subsidy

`GetBlockSubsidyInner(prevBits, prevHeight, V20)` (`validation.cpp:1810-1890`) is a function of the **previous** block's nBits and height (the miner does the same, `node/miner.cpp:255-256`).

1. **dDiff** = `ConvertBitsToDouble(prevBits)`. On mainnet with prevHeight ≤ 4500, a buggy form with no exponent shift is used instead (`:1815-1817`).
2. **Base** (whole DASH), with truncation *before* clamping:

   | Condition | Formula | Clamp |
   |---|---|---|
   | prevHeight < 5465 | `1111/(dDiff+1)²` | [1, 500] |
   | prevHeight < 17000, or (dDiff ≤ 75 and prevHeight < 24000) | `11111/((dDiff+51)/6)²` | [25, 500] |
   | otherwise | `2222222/((dDiff+2600)/9)²` | [5, 25] |
   | V20 (or devnet past `nHighSubsidyBlocks`) | fixed 5 | — |

3. **Decline:** `for i=interval; i<=prevHeight; i+=interval: s -= s/14`. This is an integer loop, not a closed form. The first decline hits block 210241.
4. **Treasury:** if `prevHeight > nBudgetPaymentsStartBlock`, the treasury takes `s/10`, or `s/5` from V20. This is a strict `>`, so mainnet's first treasury block is **328010**.

Use IEEE binary64 with no FMA, computing `x*x` rather than `pow`. The coinbase value check is only `≤`, so a Rust subsidy that is too *high* would go unnoticed by replay below 1047200. After that, the exact MN payment, which is derived from subsidy + fees, must appear in the coinbase, and from MN_RR `creditPoolBalance` commits to the platform share. **Differential-test every height** against `getblockstats h ["subsidy"]` (C§1).

#### MN share

`GetMasternodePayment` (`payments.cpp:62-128`):
- Starts at 20%. Adds `blockValue/20` three times (periods 0–2), then `blockValue/40` six times (periods 3–7 and 9, **period 8 skipped**), reaching 50%. Thresholds are 158000 + 17280·k, strict `>` (`payments.cpp:72-80`).
- From BRR the realloc table applies (51.3% … 60%, steps of 3 superblock cycles), starting at **1379128**.
- From V20, 75% of the net reward; with V20's 20% treasury that is 60% of gross.
- MNs get the same percentage of fees.

**Platform share (MN_RR+):** `PlatformShare(GetMasternodePayment(h, subsidyOnly))` = 37.5% of the subsidy-only MN reward. It is paid as an `OP_RETURN` output (pushed even when 0) and added to the credit pool (`payments.cpp:131-151`).

**Operator split:** `reward·nOperatorReward/10000`, only when the operator script is set. V24 adds multi-payout and shared-collateral splits (C§2.3).

#### Payee

Payee = `list(h-1).GetMNPayee(pindexPrev)`:
- Among non-banned MNs (unconfirmed MNs included), pick the minimum of `(revivedHeight > lastPaid ? revived : lastPaid==0 ? registered : lastPaid, proTxHash)`.
- The tie-break compares proTxHash with **uint256 memcmp of internal LE bytes**.
- Between V19 and MN_RR, an EvoNode is paid 4 consecutive blocks.

`nLastPaidHeight` is updated in `BuildNewListFromBlock` regardless of what the coinbase actually paid.

#### What is checked (`IsTransactionValid` and `IsBlockPayeeValid`)

- **No MN payee check before DIP3 enforcement** (h < 1047200). All legacy-MN-era payments are unvalidated.
- If the list is non-empty but every MN is banned → accept anything.
- Matching is existence-based: every expected `CTxOut` (value **and** script) must appear somewhere; order is free and extra outputs are allowed.
- Before V24 one output can satisfy duplicate expectations. From V24, multiset matching applies.

#### Block value (`IsBlockValueValid`)

`V = coinbase.GetValueOut()`, `R = subsidy + fees` (`payments.cpp:313-390`):
- Below the budget start: `V ≤ R`.
- In 12.0 budget windows (`h % 16616 < 100` while h < 614820): unbounded when `NoCheck`, else `V ≤ R`. The real block 332320 overpays.
- In the superblock era: `V ≤ R + GetPaymentsLimit(h)` at superblock heights, else `V ≤ R`.
  - `GetPaymentsLimit` uses nBits=1 on mainnet (base 5) and powLimit on test networks, times the cycle.
  - First mainnet limit is 6651.85 DASH at 631408; 8528.34 DASH after V20.

#### Superblock payees: node-local, not deterministic

Full checks run only if all of these hold:
- governance is enabled and the cache loaded;
- `mn_sync.IsSynced()`;
- the best ChainLock height is below h (`evo/chainhelper.cpp:41-52`).

The check (C§4) then requires:
- a trigger with ≥ `max(nGovernanceMinQuorum, weightedMNs/10)` absolute-yes FUNDING votes, counted on the **tip** MN list with EvoNodes weighted 4;
- `CSuperblock::IsValid`: payments matched **in order**, interleaving allowed, inclusive index before V24.

Trigger *contents* are not validated against proposals at consensus level; MNs only vote for triggers matching their own candidate.

During IBD, `-reindex`, or with `-disablegovernance`, **only the value bound is enforced**. Rejections are `BLOCK_RESULT_UNSET` (no ban), and a ChainLock later "overrides" them, because `EnforceBlock` resets failure flags and re-connection happens under `NoCheck`.

#### Sporks

The remaining sporks are SPORK_2, 17, 19, 21 and 23. On mainnet they are **hard-coded** since v22 (`spork.cpp:171-181`): all are active; SPORK_21 is 1. SPORK_3 and SPORK_9 were removed in v24 (`afcf3d148e`).

No spork is read by subsidy, payment, credit-pool or superblock-bound code. Sporks only gate CL and IS behaviour (node-local) and DKG networking. Test networks need spork message verification (ECDSA compact signatures, one key per network, overridable with `-sporkaddr`).

**History:** budget windows (2015–2017), superblocks from 614820, BRR 2020, V20 2023 (treasury 20%), MN_RR 2024.

**Difficulty and reuse:**

| Item | Difficulty |
|---|---|
| Subsidy | M (small code; the exactness proof is the work) |
| MN/platform/operator split | S (pre-V24), M (V24) |
| Payee selection | M, on top of the DMN list |
| Deterministic value bounds | S |
| Full governance (objects, votes, triggers, sync, expiry) | **XL**, needed for masternodes and tip policy, not for replay |
| Sporks | S |

rust-dashcore has none of this. Its `SUBSIDY_HALVING_INTERVAL = 210000` is a Bitcoin leftover, and the V20 height for regtest and devnet is wrong (C§9).

### 1.8 Special transactions and the deterministic MN list (DIP2/3/4)

**Where:** `evo/specialtx*.{h,cpp}`, `evo/specialtxman.cpp` (all ProTx checks and `RebuildListFromBlock`), `evo/providertx*.{h,cpp}`, `evo/netinfo.{h,cpp}`, `evo/dmnstate.{h,cpp}`, `evo/deterministicmns.{h,cpp}`, `evo/cbtx.{h,cpp}`, `evo/simplifiedmns.{h,cpp}`, `evo/mnhftx.{h,cpp}`, `evo/assetlocktx.{h,cpp}`, `evo/creditpool.{h,cpp}`, `evo/sharedcollateral.h`. Full detail in D (and C§6 for credit-pool math).

**Types** (`primitives/transaction.h:28-42`):

| # | Type | Notes |
|---|---|---|
| 1 | ProRegTx | |
| 2 | ProUpServTx | |
| 3 | ProUpRegTx | |
| 4 | ProUpRevTx | |
| 5 | CbTx | |
| 6 | QcTx | quorum commitment |
| 7 | MNHF signal | |
| 8 | AssetLock | |
| 9 | AssetUnlock | |
| 10 | ProDisTx | V24 |
| 11 | ProUpShareTx | V24 |
| 12 | ProUpSharedRegTx | V24 |

Dispatch is `CheckSpecialTx` (`specialtxman.cpp:217-282`). The only activation gates left are DIP3 (all types) and V24 (types 10–12). Earlier V20/MN_RR gates on types 7–9 were removed; that is a relaxation, so history replays.

**Scheme vs encoding:** BLS objects in ProTx payloads use legacy *encoding* iff the ProTx `nVersion==1`. Signatures are *verified* with the **global height-based scheme**: legacy for blocks below V19, basic from V19. So a v1 ProUpServTx after V19 is verified with basic hash-to-curve (D§2.1). Wrappers parse opposite-scheme-canonical bytes as an **invalid** object rather than an error; malleable bytes throw.

#### ProTx rules (D§2)

- **Versions:** LegacyBLS=1, BasicBLS=2 (V19+), ExtAddr=3 (V24). MnType: Regular (1000 DASH) or Evo (4000 DASH, V19+, v2+ only, platform fields).
- **Trivial validity:** keys, version, `nOperatorReward ≤ 10000`, payout lists, netInfo kind.
- **Network fields** (`MnNetInfo` v<3):
  - IPv4 only, routable.
  - **Mainnet port must be 9999; other networks must not use 9999.**
  - Since v23, invalid CService bytes normalize to "empty". The MN is then registered banned, where v18–v22 rejected the tx; a relaxation.
- **EvoNode platform ports** (v<3): mainnet requires 26656/443; ports must not collide.
- **Collateral:**
  - External collateral must exist in the **pprev UTXO set**, so same-block or mempool collateral is invalid. Value must match exactly; it must be P2PKH (or P2PK).
  - Internal collateral is an output of the ProRegTx itself.
  - Collateral reuse with the owner or voting key is forbidden.
- **Uniqueness** (`bad-protx-dup-*`): all unique properties live in **one map keyed by `SerializeHash(value)`**, so a platformNodeID collides with an equal owner keyID. Voting keys are not unique.
- **Signatures:**
  - External collateral: `CMessageSigner::VerifyMessage` over `MakeSignString` = `payout|reward|owner|voting|payloadHash`. The string is network-dependent because it uses address prefixes, and the message magic is "DarkCoin Signed Message:\n".
  - ProUpServTx and ProUpRevTx: BLS operator signature over `SerializeHash(payload w/o sig)`.
  - ProUpRegTx: owner compact signature over the hash.
- **Key-not-same:** before enforcement, `owner == voting` is required (`bad-protx-key-not-same`).
- **inputsHash:** SHA256d of the concatenated prevouts.

#### DMN list state machine (`RebuildListFromBlock`, `specialtxman.cpp:310-796`)

The order has been identical since v0.13:
1. Payee = `prevList.GetMNPayee`.
2. Set `confirmedHash = hash(pindexPrev)` for MNs with `pindexPrev.h − registered ≥ 15`.
3. PoSe penalty −1 for every non-banned MN.
4. Special txs in block order:
   - ProReg: replaces an MN with the same collateral; registered with empty netInfo means banned.
   - ProUpServ: revives a banned MN.
   - ProUpReg: an operator key change resets operator fields, sets state version, and bans.
   - ProUpRev: resets and bans.
   - V24 types.
   - **QcTx**: every member with `validMembers[i]==0` gets `PoSePunish(CalcPenalty(66))`, for every LLMQ type.
5. Collateral spends remove MNs. This is a second pass, so an MN registered and spent in the same block is added then removed.
6. Payee `nLastPaidHeight = h`. For EvoNodes, `nConsecutivePayments` is updated.
7. Reset EvoNode streaks.

PoSe math: `max = max(100, total MNs)`. The penalty is still added to already-banned MNs. The ban is set at list height. Only `Revive` lifts it.

Storage is per-block diffs plus snapshots every 576 blocks; this layout is not consensus.

#### CbTx (D§4)

**Versions:**

| Version | Adds | Required from (mainnet block) |
|---|---|---|
| v1 | height, `merkleRootMNList` | DIP3 |
| v2 | `merkleRootQuorums` | 1088641 |
| v3 | `bestCLHeightDiff` (compactsize), `bestCLSignature`, `creditPoolBalance` (int64) | iff V20 |
| v4 | `merkleRootAssetUnlocks` | iff V24 && V20 |

**`merkleRootMNList`:** every MN in list(h), banned included. Leaves are sorted by proRegTxHash memcmp. Each leaf is `SHA256d(SER_GETHASH entry)`, which **never includes the entry's nVersion**. An empty list gives the zero hash. A mutated tree is invalid.

**`merkleRootQuorums`:**
- Covers all *configured* types, enabled or not.
- Non-rotated types: the last `signingActiveQuorumCount` mined commitments.
- Rotated types: the latest commitment per `quorumIndex`.
- Then the block's own commitments are applied.
- Leaves are `SerializeHash(commitment)`, sorted, then merkled.

**`CheckCbTxBestChainlock`:**
- A non-null CL in the previous coinbase forces non-null here and `diff ≤ prevDiff+1`.
- The CL is verified against the `llmqTypeChainLocks` quorum selected at CL height − 8.
- A null sig requires diff 0.

**`creditPoolBalance`** must equal `prev + locks − unlocks + platformReward`.

#### MNHF (type 7) (D§5, E§9)

- Payload: `{u8 1, u8 bit, quorumHash, sig (always basic)}`.
- `requestId = SerializeHash(("mnhf", int64 bit))`; `msgHash` = txid with the sig zeroed.
- Signer: *any* mined `llmqTypeMnhf` quorum (mainnet LLMQ_400_85).
- Unknown bits are valid. No duplicate bit is allowed in a block or in the active signals.

#### AssetLock (8) and AssetUnlock (9) (D§6)

**Lock:**
- Exactly one `OP_RETURN` output, which must be `6a 00`, carrying the locked amount.
- Credit outputs are P2PKH in v1, P2PKH or P2SH in v2 (V24); their sum must equal the OP_RETURN amount.

**Unlock:**
- No inputs, at most 32 outputs, 145-byte payload.
- The index must not be already used; the credit pool keeps a range set since genesis.
- The quorum must be among `ScanQuorums(llmqTypePlatform, pprev, 25)`.
- Block height must be in `[requestedHeight+1, requestedHeight+48]`.
- `requestId = SerializeHash(("plwdtx", u64 index))`, signed by the platform quorum.
- Historic note: only 2 quorums were scanned before WITHDRAWALS; the 25-quorum rule is a superset.

**Credit-pool unlock limit** (C§6.2), by era:
- V20 to WITHDRAWALS: `limit = balance`. If `balance + latelyUnlocked > 100 DASH`, then `limit = max(0, max(100 DASH, balance/10) − latelyUnlocked)`. Then `limit = min(limit, 1000 DASH − latelyUnlocked)`. `latelyUnlocked` is a 576-block sliding sum (`creditpool.cpp:208-221`).
- WITHDRAWALS: `min(balance, 2000 DASH)` per block.
- V24: a new formula, still changing between rc branches.

**V24 shared masternodes** (types 10–12, ExtNetInfo, multi-payouts; D§2.9): pending on mainnet, large. In rc.3 and origin/develop the EvoNode-shares and multi-payout parts are moved behind `evo_shares` (§1.12).

**History:** see the timeline in §1.11. "Stricter in develop than history" items need chain scans: special txs with `nVersion>3`, and `bestCLHeightDiff ≥ height`.

**Difficulty and reuse:**

| Item | Difficulty |
|---|---|
| Payload codecs (all types and versions, raw-bytes-preserving) | M |
| ProTx contextual validation, types 1–4 | L |
| V24 shared MNs | L–XL |
| **DMN state machine plus unique map plus storage** | **XL** |
| CbTx plus roots | L |
| MNHF | M |
| Asset lock/unlock plus credit pool | M |

rust-dashcore provides byte-exact payload structs for v1/v2 only (no ProTx v3, CbTx v4 or types 10–12) and an exact SML entry hash and MN root. It has no DMN state machine (D§9, G§3).

### 1.9 LLMQ, BLS, ChainLocks, InstantSend, EHF

**Where:** `llmq/params.h`, `llmq/commitment.{h,cpp}`, `llmq/blockprocessor.cpp`, `llmq/utils.cpp`, `llmq/snapshot.{h,cpp}`, `llmq/quorumsman.cpp`, `llmq/signhash.cpp`, `llmq/signing*.cpp`, `active/dkgsession*.cpp`, `llmq/dkg*.{h,cpp}`, `bls/*`, `dashbls/*`, `chainlock/*`, `instantsend/*`, `llmq/ehf_signals.cpp`, and `validation.cpp:6654-6705` (`IsQuorumTypeEnabled`). Full detail in E.

#### 1.9.1 Parameters and enabled types

LLMQ types: 50_60 (1), 400_60 (2), 400_85 (3), 100_67 (4), 60_75 rotated (5), 25_67 (6, testnet), plus test and devnet types 100–107 (`llmq/params.h:19-511`).

Consensus fields: `size`, `minSize`, `dkgInterval`, mining window, `signingActiveQuorumCount`, `useRotation`.

Mainnet roles:

| Role | Type |
|---|---|
| ChainLocks | 400_60 |
| DIP24 InstantSend | 60_75 |
| Platform | 100_67 |
| MNHF | 400_85 |

"Enabled" is consensus, evaluated on pprev:
- 50_60 is disabled on mainnet from 1738699.
- 100_67 from DIP0020.
- 60_75 from DIP0024.
- 25_67 on testnet when `pindexPrev.h ≥ 847000`.

Difficulty: S.

#### 1.9.2 Commitments and block rules (`llmq/commitment.cpp`, `llmq/blockprocessor.cpp`)

**Format:** `CFinalCommitment` versions 1–4 (legacy or basic × indexed or not). The version is fixed by the **quorum base block** (rotation and V19 state at base). Wire:

```
u16 ver | u8 type | quorumHash | [i16 quorumIndex] | DYNBITSET signers | DYNBITSET validMembers
| G1 quorumPublicKey | vvecHash | G2 quorumSig | G2 membersSig
```

Non-zero padding bits in the bitsets are a **deserialization error**.

**`Verify`:**
- Exact bitset sizes.
- `signers ≥ minSize` and `validMembers ≥ minSize`. The bound is minSize, not threshold.
- No bits set beyond the member count.
- The commitment hash is `SHA256d(type|quorumHash|validMembers|pubKey|vvecHash)`. The pubkey is serialized with the **global** scheme.
- `membersSig` is a secure-aggregated signature over the signers' operator keys. Its weights depend on the serialization scheme.
- `quorumSig` is verified as an insecure signature.

**Mining:**
- In each window block, exactly `GetNumCommitmentsRequired` commitments are required per *enabled* type (`bad-qc-missing` / `bad-qc-not-allowed`). These are null or real, one per still-unmined `quorumIndex`.
- Real commitments are allowed only inside the mining window.
- Null commitments of known-but-disabled types skip the mining-window and enabled-type checks, so they are accepted at **any height**. They still need `qcTx.nHeight == h`, a `quorumHash` equal to the current DKG-cycle base (else `bad-qc-block`), and `VerifyNull` (`llmq/blockprocessor.cpp:352-379`). Replicate this.
- `bad-qc-dup`: a commitment already mined or duplicated within the block.
- Signature checks are batched and skipped under assumevalid. Member computation (for bounds and PoSe) is never skipped.

**Active quorum sets:**
- `ScanQuorums`, `GetMinedCommitmentsUntilBlock`, `GetLastMinedCommitmentsByQuorumIndexUntilBlock`.
- These feed `merkleRootQuorums`, signer selection, asset-unlock verification and MNHF.

Difficulty: M, plus quirks.

#### 1.9.3 Member selection (`llmq/utils.cpp`)

The hardest bit-exact code.

**Score:**
- `SHA256(SHA256(proTxHash‖confirmedHash) ‖ modifier)`, single SHA256 at both levels.
- Compared numerically as `arith_uint256`, descending.
- Ties go to `collateralOutpoint`.
- Only valid, confirmed MNs are scored. Platform quorums are EvoNode-only from V19.

**Modifier:**
- Pre-V20: `SerializeHash(type, cycleBase.hash)` for non-rotated quorums; the work-block hash for rotated ones.
- Post-V20: `SerializeHash(type, work.h, CL sig from the work block's own coinbase)` when that coinbase carries a CL, else `(type, work.hash)`.
- `work = base − 8`.

**V20 boundary quirk:** from V20, non-rotated quorums take their MN list from base−8, but the modifier switches only when V20 is active after base−8 (E§3.3).

**Rotation** (`ComputeQuorumMembersByQuarterRotation`, `:471-544`). Quorum i = `quarter(H−3C) ++ quarter(H−2C) ++ quarter(H−C) ++ new`:
- Old quarters come from stored snapshots, which are **derived state**: created in `ProcessBlock` at each cycle base and keyed `llmq_S`.
- A missing previous snapshot `break`s. That is why the first three rotated cycles produced only null commitments; DIP0024QuorumsHeight = 1737792 + 3·288 + 42 = 1738698.
- Skip-list modes 0 and 1. Mode 1 has a "first index 0" quirk.
- `AddMN` uniqueness exceptions are **swallowed**.
- Removed MNs are skipped only from V19 (or always on testnet).

Difficulty: **XL**. rust-dashcore implements most of the *SPV* direction, reconstructing from snapshots supplied by peers, and is accurate post-V20. Gaps (G§3.5, E§11.3):
- snapshot building and skip-list generation;
- pre-V19 and pre-V20 rules;
- mode 2;
- `AddMN` uniqueness exceptions;
- the collateral tie-break.

#### 1.9.4 BLS

`dashbls` (relic, Chia-derived) offers `LegacySchemeMPL` and `BasicSchemeMPL`. **While validating block h the process-global flag is legacy ⇔ h < V19Height** (`validation.cpp:2491`; flipped in `ProcessSpecialTxsInBlock`). The flag affects:
- serialization of un-versioned objects (CLSIG, recsig, islock, CbTx CL sig);
- the commitment-hash pubkey bytes;
- secure-aggregation weights;
- hash-to-curve: legacy `ep2_map_legacy` vs IETF SSWU.

Legacy parsing does **no curve or subgroup check**, and relic errors are swallowed. Bytes canonical only in the opposite scheme decode to an *invalid* object, not an error. Store raw bytes plus scheme and decode lazily, as `CBLSLazyWrapper` does.

Always-basic: MNHF signature, MNAUTH, the IES key derivation, and the vvec on disk.

On mainnet and testnet no commitment's base/mining heights straddle V19, so "version scheme" and "global scheme" agree. Implement the global rule literally anyway, for devnet and regtest.

Difficulty: L in pure Rust; M via FFI to dashbls. Reuse: **dash-pkc** (blst) implements both encodings, legacy hash-to-G2, secure aggregation, threshold and IES. To fix (E§11.1):
- the unreduced secure-aggregation weight;
- the IES key derived from the legacy encoding where Core uses basic;
- Dash wrapper semantics;
- the global-flag rule.

dash-spv hard-codes basic, so it fails the 24 stale v1 commitments still in today's SML.

#### 1.9.5 ChainLocks: what is consensus

- **Deterministic:** only the CbTx v3 `bestCL*` fields (V20+). They are validated by `CheckCbTxBestChainlock` (§1.8) and feed the V20 quorum modifier.
- **Node-local:** every reorg prohibition.
  - `Chainlocks` store P2P `clsig`s (verified against the quorum selected at `height−8`; requestId `SHA256d("clsig", height)`).
  - `EnforceBestChainLock` → `EnforceBlock` marks competing branches `BLOCK_CONFLICT_CHAINLOCK` and disconnects them if needed (`validation.cpp:4046-4148`).
  - New headers or blocks conflicting with the best CL get `bad-chainlock` / `bad-prevblk-chainlock` (`:4641-4696`, `:2517`).
  - Chain selection skips conflicting chains (`:3543-3559`).
  - All of this is gated by SPORK_19, which is hard-wired on mainnet.
  - Coinbase CLs are also fed into the store once synced (`chainlock/handler.cpp:104-131`).

So "a ChainLock forbids reorgs" is enforced by fork-choice and AcceptBlockHeader **policy driven by received signatures**, not by any rule a replaying node can check.

Signer rules (masternode): sign the tip once every tx in the last ≤ 5 blocks is IS-locked or older than 10 minutes (`chainlock/handler.cpp:39-40,257-269`).

Difficulty: M for verification and CbTx; M for enforcement.

#### 1.9.6 InstantSend: what is consensus

**Lock format:** `isdlock` = `{v1, inputs, txid, cycleHash, sig}`, signed by the rotated 60_75 quorum. The signer index is `(b >> (64−n−1)) & ((1<<n)−1)` with `b = id.GetUint64(3)`, i.e. bits 58–62, not the top bits (E§6.2).

**Consensus effect:** only `conflict-tx-lock` in `ConnectBlock`, when all of these hold:
- not IBD;
- IS enabled (SPORK_2, not reindexing or importing);
- the block is not chainlocked; a CL'd block deletes the conflicting islock instead.

That effect is node-local. Mempool `tx-txlock-conflict` and the miner's "≥ 10 min or locked" inclusion rule are policy.

Develop adds synthetic lock inputs for asset unlocks (V24 era).

Difficulty: M to verify; L to sign, including input locks.

#### 1.9.7 Signing sessions and DKG (masternode protocols, not block validity)

**Sign hash:** `SHA256d(u8 type | quorumHash | id | msgHash)`.

**Quorum selection for signing:**
- Non-rotated: `argmin_memcmp SHA256d(type|quorumHash|id)` over the active quorums at `signHeight−8`.
- Rotated: by bits 58–62 of the request id (§1.9.6).

**Signing-session messages:** `qsigsesann`, `qsigsinv`, `qgetsigs`, `qbsigs`, `qsigshare` (the latter only with SPORK_21), `qsigrec`, `qsendrecsigs`. Recovery is Lagrange over `CBLSId(proTxHash)` (big-endian read of the LE bytes, mod r). Shares are verified against the vvec, which comes from DKG or from `qgetdata`/`qdata`.

**DKG** (`active/dkgsession.cpp`): phases at `(tip−quorumIndex) % dkgInterval`. Contribute, complain, justify, commit and finalize each take `phaseBlocks`; finalize lands at the mining-window start. Messages: `qcontrib` (BLS-IES encrypted shares, AES-256-CBC without padding), `qcomplaint`, `qjustify`, `qpcommit`, `qfcommit`. Supporting pieces:
- intra-quorum connections: all-to-all under SPORK_21, else a ring;
- `mnauth`;
- `qwatch`;
- data recovery.

The DKG outcome (`validMembers`) drives on-chain PoSe. A Rust MN that participates incorrectly gets **its own operators punished** and degrades quorums, so DKG correctness is consensus-adjacent.

Difficulty: signing sessions L; DKG XL. dash-pkc has the threshold, IES and DH primitives; the protocol, messages and connection management are missing.

#### 1.9.8 EHF

`ehf_signals.cpp`: after V20, MNs sign `mnhf` requests for in-window `useEHF` deployments not yet signalled. On recovery any MN builds the type-7 tx and puts it in its mempool; miners include it like a normal tx.

Consensus validation is in §1.8. Signatures are verified even under assumevalid.

Mainnet history: MN_RR (bit 10) and WITHDRAWALS (bit 11) were EHF-activated and are now buried. Bit 12 signals are orphaned but on chain.

Difficulty: S–M.

### 1.10 Validity inputs that are not chain data

Model each of these as an explicit, injectable input in the Rust validator:

| # | Rule | Depends on | Where | Behaviour during replay |
|---|---|---|---|---|
| 1 | `bad-chainlock`, `bad-prevblk-chainlock`, CL-conflict marking, fork-choice exclusion | Best P2P CLSIG (+ SPORK_19), coinbase CLs once synced | `validation.cpp:2517,4641-4696,3543-3559`; `chainlock/handler.cpp` | Empty CL store: no effect |
| 2 | `conflict-tx-lock` | islock DB, SPORK_2, `!IsInitialBlockDownload()` (tip age vs local clock), not reindexing, local CL | `validation.cpp:2800-2818`; `instantsend.cpp:510-513` | Skipped (IBD) |
| 3 | Superblock payee/trigger checks and old-budget strictness | Governance objects and votes (P2P), `mn_sync.IsSynced()`, best CL height, `-disablegovernance`, wall-clock trigger expiry | `evo/chainhelper.cpp:41-52`; `masternode/payments.cpp:262-442`; `governance/*` | `NoCheck`: bounds only; old-budget windows unbounded |
| 4 | `time-too-new` | Local clock (+2 h) | `validation.cpp:4507` | Never triggers for history |
| 5 | Checkpoint fork rejection | Contents of the local block index | `validation.cpp:4491-4500` | N/A on the main chain |
| 6 | assumevalid / `fScriptChecks` | Config, best header, min chain work | `validation.cpp:2543-2572` | Must be off for verification |
| 7 | Unrequested or low-work block processing | Local tip and config | `validation.cpp:4803-4813` | — |
| 8 | Process-global state: BLS legacy flag, global `Params()`, EHF singleton, versionbits cache | Process history | `validation.cpp:2491`; `versionbits.cpp:244`; `evo/mnhftx.cpp:78` | Deterministic if blocks are connected in order |

**Design rule:** the Rust consensus core is a pure function `(chain view, block) → (verdict, state delta)`. Node-local policies 1–3 wrap it exactly where dashd calls them, with the same skip conditions, so that a synced Rust node and a synced dashd fed the same P2P inputs make the same decisions.

### 1.11 Mainnet historical timeline (rules that old blocks are validated under today)

| Height | What current dashd enforces from here |
|---|---|
| 0 | Genesis outputs never enter the UTXO set. P2SH and DIP0020 opcodes on. BIP30 on (until the BIP34 hash chain). |
| 1–4501 | Buggy dDiff subsidy formula; pre-BIP34 coinbase. |
| 951 | BIP34 prefix check, until DIP3. |
| < 15200 | BTC retarget, ±50% tolerance. |
| 15200–34139 | KGW, ±50% tolerance. |
| 34140–68589 | DGW, ±50% tolerance. |
| 68590+ | DGW exact. |
| 100000+ | Subsidy base effectively 5 (difficulty-dependent until V20). |
| 158000…313520 | MN share steps (skip period 8). |
| 210241 | First 1/14 decline. |
| 245817 / 619382 / 622944 | DERSIG / CLTV / CSV (+BIP68/113). |
| 328010 | Treasury 10%. Budget windows 332320+16616k (unbounded value when `NoCheck`). |
| 614820 | `bad-sb-start` hash pin; superblock era (first SB 631408). |
| 782208 | DIP0001: 2 MB, 40k sigops, 100 kB tx. |
| 939456 | NULLDUMMY. |
| 1028160 | DIP3: special txs, CbTx v1, DML, BIP34 replaced by CbTx height. |
| 1047200 | DIP3 enforcement: MN payee checks; hash pin. Owner==voting is still required in this block and dropped from 1047201 (`DeploymentDIP0003Enforced(pindexPrev->nHeight)`). |
| 1088641 | CbTx v2 (`merkleRootQuorums`); commitments mined from DIP3 on. |
| 1374912 / 1379128 | BRR / realloc schedule. |
| 1516032 | LLMQ_100_67 enabled. |
| 1737792 / 1738698 / 1738699 | DIP24 rotation / first rotated commitment / LLMQ_50_60 disabled. |
| 1899072 | V19: basic BLS (global flag), ProTx v2, EvoNodes (4-block streaks), EvoNode-only platform quorums, skip removed MNs in rotation. |
| 1987776 | V20: subsidy base fixed at 5, treasury 20%, MN 75%; CbTx v3 (bestCL, credit pool); asset lock/unlock, MNHF effective; CL-based quorum modifier; work-block list for non-rotated quorums. |
| 2128896 | MN_RR: platform share 37.5% to the credit pool via OP_RETURN; EvoNode streaks end. |
| 2201472 / 2201473 | WITHDRAWALS: unlock limit min(balance, 2000 DASH) per block. |
| (pending) | V24, evo_shares (§1.12). |

### 1.12 Pending: V24 and evo_shares (mainnet NEVER_ACTIVE, testnet signalling since 2026-09-24)

**V24 (bit 13, EHF, window 4032; testnet 100)** in develop @15db478750:
- ProTx v3 `ExtAddr`: `ExtNetInfo` with IPv4/IPv6/Tor/I2P/CJDNS/domain entries, per-purpose lists, no mainnet port-9999 rule. Also multi-payout lists, shared masternodes (types 10–12) with a template collateral covenant `04 'DSHC' OP_DROP OP_TRUE`, a ban on v1 registrations, no version downgrade, and cross-scheme operator-key uniqueness.
- CbTx v4 `merkleRootAssetUnlocks`. A mismatch is `BLOCK_MUTATED`.
- Asset unlock v2 with the stable txid; asset lock v2 adds P2SH credit outputs.
- V24 credit-pool unlock limit `UnlockLimitV24`, which **differs from v23.1.8's V24 rule**.
- Strict multiset matching for MN and superblock outputs; `DisallowDuplicates`.
- InstantSend locks for asset unlocks.

**evo_shares (bit 14):** `origin/develop` @2340e3a265 (commits `68a9144d50`, `9deae1f8f0`) and `backport/v24.0.0-rc.3` move the EvoNode shares and multiple-payout features behind a separate deployment, "not signed in v24".

**Action:** treat V24 as a separate milestone, pinned to the final v24.0.0 tag. Testnet will activate it first, so the Rust testnet replay must follow it.

### 1.13 Masternode-side behaviour that other nodes depend on (not block validity)

A Rust masternode must also match these, or it is punished or degrades the network:
- **DKG participation** (§1.9.7): failures show up on chain as PoSe penalties and bans.
- **PoSe reachability.** MNs probe each other (`protx info .metaInfo.lastOutbound*`). With SPORK_23 and SPORK_21, DKG members that cannot be connected to are marked bad.
- **Signing:** recovered sigs for ChainLocks, IS input locks and islocks, platform (`quorum platformsign`), EHF, and asset unlocks (platform quorum).
- **Governance signer** (`governance/signing.cpp:38-308`). From v20, MNs *create* superblock triggers deterministically: approved proposals, taken in order of absolute YES votes descending (ties by hash, `governance.cpp:1221-1226`) and budget-limited; the final payment list is sorted by proposal hash descending (`signing.cpp:112-115`), with the epoch estimated as `now + (nextSB−h)·2.62·60`. Only the current payee submits; every MN votes YES on the identical trigger (matched by data hash) and NO on the rest. Byte-identical trigger data is required for a Rust MN to "agree". The epoch estimate is wall-clock dependent.
- **CoinJoin server** (`coinjoin/server.cpp`, ~1300 LOC): `dsa`, `dsi`, `dss`, `dsf`, `dsc`, `dssu`, `dsq` (operator-signed), `dstx` relay; collateral rules and timeouts.
- **MNAUTH** on every connection; masternode-specific eviction protection; refusing inbound connections until synced.
- **Quorum data serving and recovery** (`qgetdata`/`qdata`), `qwatch`, `platformban`.

Difficulty: DKG XL; signing L; governance signer M (plus the governance XL in §1.7); CoinJoin server L; MNAUTH and probes M.

### 1.14 Effort and reuse summary

| Area | Diff. | rust-dashcore / base-sdk reuse |
|---|---|---|
| Core-faithful tx/block codec (raw payload, stable unlock txid, merkle mutation, PLAN §7 handling) | M | Replace; consider base-sdk `dash-primitives` |
| Params, regtest/devnet knobs, genesis/devnet genesis | S | Network magic and genesis OK; `consensus/params.rs` must be rewritten |
| CheckTransaction / contextual / BIP68 / BIP113 / maturity / sigops | M | — |
| Script interpreter + sighash + ECDSA layer | L | Sighash partially; base-sdk `dash-script` candidate |
| Mempool and policy (incl. special-tx and IS interplay, no RBF) | L | — |
| PoW / X11 / DGW / KGW / tolerance / compact | M | X11 |
| Header and block checks with reject parity | M | — |
| Versionbits (dynamic threshold + EHF) | M | — |
| Subsidy / MN / treasury / platform split | M | — |
| Payee selection + deterministic value/payee checks | M | — |
| Governance (objects, votes, triggers, superblock payees) | XL | — |
| Special-tx payload codecs (all types and versions) | M | v1/v2 OK; v3/v4/types 10–12 missing |
| ProTx contextual validation (1–4) | L | — |
| V24 shared MNs / ExtNetInfo / multi-payout | L–XL | SML ExtNetInfo hash only |
| DMN state machine + unique map + storage | **XL** | — |
| CbTx v1–v4 + MN/quorum roots + bestCL | L | SML root exact (H) |
| MNHF / EHF | M | Parse only |
| Asset lock/unlock + credit pool | M | Parse only |
| LLMQ commitments + block processor + active sets | L | Partial |
| Member selection + DIP24 rotation + snapshot building | **XL** | Partial (post-V20 SPV direction) |
| BLS (two schemes, Dash wrapper semantics, global flag) | L | dash-pkc with fixes |
| ChainLock verify + enforcement + signing | L | Verify OK |
| InstantSend verify + conflicts + signing | L | Verify OK |
| Signing sessions | L | Primitives in dash-pkc |
| DKG | XL | Primitives in dash-pkc |
| CoinJoin server | L | Client side in key-wallet |
| UTXO DB + coinstats (muhash, hash_serialized_2 quirk) | M | — |
| P2P full node + Dash messages + fingerprint parity | XL | Codec scaffolding in dash-spv |

### 1.15 Quirk register: replicate exactly (bug-for-bug)

1. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md.
2. `CheckTransaction` empty-vin/vout exemptions and the 1-byte coinbase scriptSig rule key on `nType` without checking the version (`tx_check.cpp:16-24,60-67`).
3. 100,000-byte tx limit: consensus `>` (from DIP0001) vs policy `>=`.
4. OP_CHECKDATASIG: DER plus one trailing byte, single SHA256, lax verify over the full sig, zero sigops, no sigcache.
5. OP_NUM2BIN operand quirks; exceptions map to UNKNOWN_ERROR; disabled opcodes fail inside unexecuted branches, as do OP_VERIF and OP_VERNOTIF.
6. SIGHASH_SINGLE returns ONE; FindAndDelete on BASE scripts, including OP_0 removal for empty sigs; the payload is included in the sighash.
7. ECDSA: lax DER, normalize S, hybrid keys allowed at consensus.
8. DGW 23-vs-24 interval off-by-one, per-step truncation, wrapping U256; devnet ×10 min-difficulty wrap.
9. Mainnet h ≤ 68589 ±50% difficulty tolerance.
10. BIP34 is a byte-prefix compare of `CScript()<<h` (OP_1..OP_16 for heights 1..16), and only before DIP3.
11. Subsidy: prev nBits and prev height; buggy dDiff when mainnet prevHeight ≤ 4500; truncation before clamping; iterative 1/14 decline; treasury uses a strict `>`.
12. MN share: separately truncated increments, period 8 skipped, realloc start rounded to the superblock cycle.
13. Payee tie-break by uint256 memcmp; unconfirmed MNs payable; all-banned list means accept-anything (credit pool still credited).
14. Old-budget windows unbounded under `NoCheck`; superblock payees only checked when synced and not chainlocked.
15. `GetPaymentsLimit` uses nBits=1 on mainnet and powLimit elsewhere.
16. CbTx v2 required from DIP0008Height **+1**.
17. SML leaf hash excludes `nVersion`; empty-list root is zero; mutation is invalid.
18. `merkleRootQuorums` iterates configured types, including disabled ones (stale LLMQ_50_60 v1 leaves remain).
19. Null commitments of disabled types are accepted at any height, subject to height/quorumHash/VerifyNull; `signers ≥ minSize`; padding bits are a parse error.
20. Commitment version is fixed by the quorum base block; commitment-hash pubkey bytes use the global BLS scheme.
21. ProTx signature *verification* scheme follows height (global), not the ProTx version.
22. Opposite-scheme-canonical BLS bytes decode to an invalid object; legacy keys get no curve/subgroup checks.
23. Unique-property map keyed by `SerializeHash(value)`: platformNodeID can collide with an owner keyID; legacy vs basic operator-key encodings are distinct slots pre-V24.
24. PoSe penalty is added to already-banned MNs; `max(100, totalMNs)` is evaluated on the list as rebuilt so far in the block.
25. DMN update order: confirm → decrease PoSe → txs in order with QC punishment interleaved → collateral spends → payee bookkeeping → streak reset. ProRegTx with the same collateral replaces the old MN.
26. External collateral must be in the pprev UTXO set; special txs never see same-block outputs.
27. `MakeSignString` is network-dependent (base58 prefixes) with the "DarkCoin Signed Message" magic.
28. Rotation: `break` on a missing snapshot; swallowed `AddMN` exceptions; skip-list first-index-0 quirk; `skipRemovedMNs` from V19 (or testnet).
29. V20 boundary: base−8 MN list with the old modifier for the first V20 cycle; the modifier reads the work block's coinbase CL from disk.
30. Rotated signer index uses bits 58–62 of `GetUint64(3)`.
31. MNHF: unknown bits valid; any mined 400_85 quorum may sign; int64 bit in the requestId; historic bit 10/11/12 signals.
32. `MinBIP9WarningHeight` is a consensus floor; the dynamic threshold's int32 overflow.
33. `hash_serialized_2` VARINT precedence bug (`kernel/coinstats.cpp:77`); unspendable outputs never in the UTXO set; coinstatsindex `unclaimed_rewards` can go negative because of asset unlocks.
34. Hard-coded pins: `bad-sb-start` (614820), `bad-dip3-enf-block` (1047200); testnet LLMQ_25_67 enabled when `pindexPrev.h ≥ 847000`.

---

## 2. Verification strategy

### 2.0 Principles

1. **Oracle = pinned dashd release.** Reference nodes are the BS2 oracles in `/work/chains/{mainnet,testnet}`, which already run with `coinstatsindex`, `txindex`, `addressindex`, `spentindex`, `timestampindex` and `blockfilterindex` (see ORACLE.md when BS2 lands). Regtest and devnet oracles run from release tarballs (`dashcore-<ver>-x86_64-linux-gnu.tar.gz`; rc.3 is cached in `/tmp/r4-verif-bin`).
2. **Every consensus function gets three independent kinds of evidence:**
   - (a) fixed vectors;
   - (b) a differential comparison against Core on generated inputs;
   - (c) real-chain replay.

   P2P and masternode behaviour additionally get (d) mixed-network operation and (e) fingerprint probes.
3. **Compare more than verdicts:** compare the reject string, validation-result class and post-state too. Divergent reject strings change misbehaviour scoring and break functional tests.
4. **Node-local rules** (§1.10) are tested by scenario (functional and devnet), never by replay.
5. **Performance work never bypasses the gates.** Any optimization in consensus code must pass L1–L3 unchanged (charter performance rule).

**Verification matrix.** ● = primary evidence, ○ = secondary.

| Area | L0 vectors | L1 diff-fuzz | L2 replay | L3 mutation | L4 func tests | L5 mixed nets | L6 fingerprint |
|---|---|---|---|---|---|---|---|
| Tx/block codec, txid (incl. EMBARGO-1) | ● | ● | ● | ● | ○ | | |
| Script, sighash, sigops | ● | ● | ● | ○ | ● | | |
| PoW / DGW / header rules | ● | ● | ● | ● | ○ | | |
| Subsidy / payments / value bounds | ● | ● | ● | ● | ○ | ○ | |
| Governance / superblock payees | ○ | | (bounds only) | ○ | ● | ● | |
| Special-tx validity | ● | ● | ● | ● | ● | ○ | |
| DMN state machine | | ○ | ● | ● | ● | ● | |
| CbTx roots / credit pool / MNHF | ○ | ○ | ● | ● | ● | ● | |
| Member selection / rotation | ○ | ○ | ● | ○ | ● | ● | |
| BLS (both schemes) | ● | ● | ● | ○ | ○ | ● | |
| CL/IS enforcement (node-local) | | | | ○ | ● | ● | |
| DKG / signing / CoinJoin server | | ○ | | | ● | ● | ○ |
| Mempool / policy | ● | ○ | | ● | ● | ○ | |
| P2P behaviour | | ○ | | | ● | ● | ● |

### 2.1 L0: reuse Dash Core's vector suites

**Directly consumable JSON** (`src/test/data`, F§1.1):

| File | Count | Dash-specific content |
|---|---|---|
| `script_tests.json` | 1282 vectors | about 120 DIP0020 rows |
| `tx_valid.json` / `tx_invalid.json` | 83 / 76 | — |
| `sighash.json` | 500 | — |
| `trivially_valid.json` / `trivially_invalid.json` | 25 / 12 | ProTx, with reject strings; "basic"/"legacy" scheme column |
| `proposals_valid.json` / `proposals_invalid.json` | 6 / 19 | governance proposals |
| `key_io_*`, `base58_encode_decode.json`, `bip39_vectors.json` | — | — |
| `blockfilters.json` | 7 | X11 hashes; Dash v23+ filters add special-tx elements, which these vectors do not cover |

Also usable: `test/util/data/bitcoin-util-test.json` (77 `dash-tx` golden cases) and functional-test data (`rpc_getblockstats.json`, `quorum_proof.json`, 257 testnet headers, and the `feature_utxo_set_hash` golden hashes).

**How to run them in Rust:** create one `dash-consensus-vectors` test crate that ports Core's harness semantics exactly:
- `ParseScript` (`core_read.cpp:63-103`);
- `mapFlagNames` / `ParseScriptFlags`;
- the script-error name table (Dash adds CHECKDATASIGVERIFY, SPLIT_RANGE, OPERAND_SIZE, INVALID_NUMBER_RANGE, DIV_BY_ZERO, MOD_BY_ZERO);
- `tx_valid` "excluded flags" semantics;
- the `BADTX` marker.

Do **not** copy two harness bugs: flag perturbation never touches bits 14 and 16, and `ExcludeIndividualFlags` is a no-op (A§7).

**Vectors embedded in C++** (transcribe into JSON; F§1.2):
- DGW over 24 real mainnet blocks, plus devnet slow-block cases (`pow_tests.cpp:14-59`).
- Subsidy points (`subsidy_tests.cpp`).
- BRR and payment amounts (`block_reward_reallocation_tests.cpp`).
- SML entry hashes and root (`evo_simplifiedmns_tests.cpp:19-76`).
- A real ISDLOCK (`evo_islock_tests.cpp:177`).
- A testnet quorum proof (`quorum_proofs_tests.cpp`).
- DIP14 vectors.
- The dynamic versionbits threshold formula.
- DIP0020 and CHECKDATASIG cases (`dip0020opcodes_tests.cpp`, `checkdatasig_tests.cpp`). These include positive CHECKDATASIG signatures, which the JSON lacks.
- `dashbls` "Chia" and "Legacy" vectors (`src/dashbls/src/test.cpp`).

**Vectors from chain data.** Generate these from the BS2 oracle, once per pinned release:
- every header's `(nBits, expected)` (DGW/KGW/tolerance);
- `getblockstats subsidy` for every height;
- every special tx's raw payload plus its `getrawtransaction` JSON;
- every coinbase's CbTx;
- `protx diff` at checkpoints.

Also replace the corrupted rust-dashcore `.bin` fixtures (F12) with genuine dashd `mnlistdiff` and `qrinfo` captures.

**Spec oracle for unclear behaviour:** the release tarball ships `test_dash` with 167 suites. Run a targeted case (e.g. `--run_test=evo_deterministicmns_tests/...`) to settle disputes without a build.

Effort: S–M.

### 2.2 L1: function-level differential testing and fuzzing

**libdashconsensus is not enough.** It still ships, but it is deprecated, exposes only `verify_script` with the five consensus flags, and returns 0/1 without a `ScriptError`. Use it in week 1 as a quick FFI oracle (`/tmp/r4-verif-bin/dashcore-24.0.0-rc.3/lib/libdashconsensus.so`). Do not build on it.

**Build an oracle shim instead.** Add a C++ static library `liboracle` in a Dash Core branch (`node/oracle-shim`, per PROCESS.md). It links `libbitcoin_consensus.a`, `libbitcoin_common.a`, the needed `libbitcoin_node` objects, `libdashbls`, `crypto` and `secp256k1`, built from the pinned commit with `-fsanitize=fuzzer-no-link`. A Rust crate `dashcore-oracle-sys` (dev/fuzz-only, never in release builds) links it. The shim exposes pure `extern "C"` entry points:

| Priority | Entry points |
|---|---|
| P0 | `tx_parse` → (ok \| error, re-serialized bytes, txid, instance hash, payload-type parse result) |
| P0 | `check_transaction`, `contextual_check_transaction(flags: dip3, dip0001, v24)` → reject string |
| P0 | `verify_script(tx, nIn, spk, flags[all 17 bits])` → (ok, ScriptError) |
| P0 | `eval_script(script, flags, sigversion)` → (ok, err, final stack) |
| P0 | `signature_hash(scriptCode, tx, nIn, hashType)`; `sigops_legacy`, `sigops_p2sh(tx, spent outputs)` |
| P0 | `compact_set/get`, `check_pow`, `x11(header)`, `dgw(24 headers)`, `next_work(net, headers, candidate time)` |
| P0 | `block_subsidy(prevBits, prevHeight, net, v20)`, `mn_payment(h, value, era)`, `platform_share`, `payments_limit(h, net)` |
| P0 | `bls_parse(bytes, kind, scheme)` → (valid \| invalid \| throw, canonical bytes); `bls_verify`, `bls_verify_secure_aggregated`, `bls_recover` |
| P1 | `commitment_parse`, `commitment_hash(qc, global_scheme)`; `sml_entry_hash(state bytes)`; `mn_list_root(entries)` |
| P1 | `protx_payload_parse(type, bytes)` → (struct JSON, `IsTriviallyValid` reason, `MakeSignString(net)`) |
| P1 | `netinfo_validate(net, entries)`; `credit_pool_limit(era, balance, window)`; `versionbits_threshold(...)`; MNHF and asset lock/unlock payload checks |
| P2 | `calculate_quorum(list, modifier, size)`; `quarter_members_by_snapshot(...)`. These need synthetic `CDeterministicMNList` construction; the replay covers them better. |

**Fuzzing setup:**
- `cargo-fuzz` (libFuzzer) with both implementations in-process. Each target asserts equality of the full output tuple.
- Run continuously on agentbox: nightly, about 16 cores for 8 h.
- Seed corpora:
  - Core's JSON vectors;
  - bitcoin-core/qa-assets `script_flags` and `script` corpora (Core points there: `test/fuzz/test_runner.py:147`);
  - every unique scriptSig/scriptPubKey pair and every special payload harvested during L2 replay.
- **Third implementation:** the functional framework's Python serializers (`test_framework/messages.py`: CCbTx, CFinalCommitment, CSimplifiedMNListEntry, CompressibleBlockHeader, the asset lock/unlock and MNHF classes) give an independent check for codec disagreements.
- Gaps on the Core side: Core has no fuzz target for special-tx payloads, BLS or LLMQ objects (F§2). The shim fills these, and upstreaming the targets would benefit Dash Core.

Effort: shim M; targets S each; ongoing compute.

### 2.3 L2: full-chain replay with per-height state comparison

#### 2.3.1 Mechanics

- **Inputs:** the BS2 oracle's `blocks/blk*.dat`, read-only, or a fork-free `bootstrap.dat` made with `contrib/linearize`. The Rust node imports them through a `-loadblock`/`-reindex`-equivalent path with **`-assumevalid=0`**, checkpoints on (they are harmless on the canonical chain), `-par` at maximum, and no P2P. This isolates consensus from networking.
- **The chain checks itself:** from DIP3 the Rust node independently recomputes, every block, `merkleRootMNList`; from 1088641, `merkleRootQuorums`; from V20, `bestCL` and `creditPoolBalance`; from V24, `merkleRootAssetUnlocks`. A wrong DMN list, quorum set or credit pool fails the block immediately. This covers the SML-visible state but not all of it: `nLastPaidHeight`, PoSe counters, `nConsecutivePayments` and non-SML fields are only caught indirectly, through later payee or merkle mismatches, possibly much later. Hence the explicit digests below.
- **Rust output:** a per-block **StateDigest v1** record, one JSONL line per block, with the fields below computed natively.
- **Oracle output:** the same digest extracted from dashd via RPC, as golden files versioned by dashd release and network. Extract once per release. A comparator reports the first divergent height and field. Bisection is trivial because the records are per block.

#### 2.3.2 StateDigest v1: fields, oracle RPC, cadence

| Field(s) | dashd RPC (stock binary) | From (mainnet) | Cadence |
|---|---|---|---|
| hash, prev, height, header fields | `getblockheader <hash>` | 0 | every block |
| UTXO `muhash`, `txouts`, `total_amount`, `total_unspendable_amount`, `block_info{prevout_spent, coinbase, new_outputs_ex_coinbase, unspendable, unspendables{genesis_block, scripts, unclaimed_rewards}}` | `gettxoutsetinfo muhash <h>` (needs `-coinstatsindex`) | 0 | every block |
| subsidy, totalfee, txs, ins, outs, utxo_increase, utxo_size_inc | `getblockstats <h> [...]` | 1 | every block |
| Expected MN payee outputs and amounts | `masternode payments <hash>` | 1028160 | every block |
| Full DMN state delta (added MNs with full state; removed; updated fields incl. PoSePenalty, lastPaidHeight, consecutivePayments, banHeight, revivedHeight, confirmedHash) | **`protx listdiff <h-1> <h>`** (`rpc/evo.cpp:1898-1981`, full `CDeterministicMNStateDiff`) | 1028160 | every block |
| Full DMN list (absolute check) | `protx list registered true <h>` | 1028160 | every 576 blocks (the evodb snapshot period) |
| CbTx fields | `getblock <hash> 1` → `cbTx` | 1028160 | every block (also consensus-checked) |
| Credit pool `{balance, currentlimit, window{blocks, height, balance, unlocked}}` | `getcreditpoolinfo <h>` | 1987776 | every block |
| Active quorum sets per type `{creationHeight, quorumIndex, minedBlockHash, numValidMembers}` | `quorum listextended <h>`. It iterates only *enabled* types, so the stale LLMQ_50_60 leaves (F5) do not appear; those are covered by the CbTx root. | 1028160 | every block inside mining windows; else every 24 |
| Quorum members (incl. valid flags) | `quorum info <type> <hash>` | 1028160 | per mined quorum |
| Rotation snapshots and quarter members (qrinfo) | `quorum rotationinfo <hash>` | 1737792 | per 288-block cycle base |
| Deployment states (incl. EHF signal heights) | `getdeploymentinfo <hash>` | 2203488 (`MinBIP9WarningHeight`) for V24/evo_shares; buried ones are static | every 576, plus every window boundary |
| Coinbase CL carried in block h | `getblock <hash> 1` → `cbTx.bestCLHeightDiff/bestCLSignature`. `getchainlockbyheight <h>` is a different thing: it returns the CL *for* height h, found in some later coinbase (`rpc/quorums.cpp:1405-1450`). | 1987776 | every block |
| Asset-unlock index statuses | `getassetunlockstatuses [idx…] <h>` | 2128896 | per block that contains unlocks |
| Raw block bytes (codec round trip) | `getblock <hash> 0` | 0 | every block |

**Oracle version caveat.** `getcreditpoolinfo` (added 2026-09-22, `06bd914591`), `getchainlockbyheight` and `getdeploymentinfo` do **not** exist in v23.1.8, the mainnet reference. Extract golden files with a v24.0.0-rcN or final binary over the same chain. Mainnet consensus is identical, because V24 and evo_shares are NEVER_ACTIVE there. Cross-check a sample of heights against v23.1.8 with the RPCs it has.

At the tip, or at heights chosen with `-stopatheight`, also compare: `gettxoutsetinfo hash_serialized_2` (reproduce the quirk); a full `dumptxoutset` set-diff; `getchaintips`; `verifychain 4 0`.

**Test-only dashd patch, recommended in addition to the RPC path.** Branch `node/state-digest` in `~/workspace/dash`: about 150 LOC behind a hidden `-statedigestlog=<file>` flag. In `ConnectTip` it writes one JSONL record per block with:
- `CanonicalMNListHash(list)` (already in `evo/snapshot.cpp:150-155`);
- the canonical hash of mined-commitment index entries;
- `llmq_S` snapshot hashes;
- the EHF signals map;
- credit-pool state;
- versionbits cache states;
- the BLS legacy flag;
- the coinstats muhash.

The patched oracle then emits complete Dash-state digests during its *own* `-reindex` (state is identical with `-reindex-chainstate`, which is faster), with no RPC cost. The Rust node emits the identical record, and comparison is `cmp`. It also covers derived state that has no RPC, such as rotation snapshot bytes. The same canonical encodings are the natural Rust snapshot format: dashd's `EvoSnapshot` codec is complete but not yet wired to `dumptxoutset`/`loadtxoutset`. If pasta agrees, upstream a hidden `getevostatehash <h>` RPC.

#### 2.3.3 Cost and time on agentbox (32 vCPU, 160 GB RAM)

**Chain size today:** mainnet 2,551,641 blocks, 71.4M txs, ≈ 43.3 GB of blocks. Testnet 1,568,316 blocks. Sources: blockchair and insight on 2026-10-08 (F§7); `chainTxData` shows 69.77M txs at height 2,487,500.

| Job | Estimate | Basis |
|---|---|---|
| BS2 oracle IBD, mainnet, all indexes | ≈ 4–12 h (B1 is measuring dashd IBD now, runs started 2026-10-08 03:19 UTC; replace this estimate with the B1 number) | 43 GB, 71M txs; Dash overhead is dominated by per-block DML/SML and EvoDB work after 1028160, plus BLS checks after DIP8 |
| Golden digest extraction, mainnet | ≈ 9M RPC calls → ≈ 1–4 h with batched JSON-RPC over 32 `rpcthreads`; ≈ 1 GB zstd JSONL | `gettxoutsetinfo` and `getblockstats` × 2.55M; `listdiff`/`payments` × 1.52M; credit pool × 0.56M; the rest sampled |
| Golden digest extraction, testnet | ≈ 0.5–1.5 h | |
| Rust full replay, mainnet, `-assumevalid=0` | Target ≤ dashd `-reindex -assumevalid=0` (charter: never slower; target 2×). Do not compare against `-reindex-chainstate`: it skips `ContextualCheckBlockHeader`/`ContextualCheckBlock` (`validation.cpp:4465,4526`), i.e. no `bad-diffbits`, `bad-txns-type`, BIP34 or DIP0001 size checks. | Same work as dashd |
| Rust full replay, testnet | ≈ 1–3 h | |
| Segment replays (§2.3.4) | 10–60 min each | |

**Resources per concurrent job:**
- Oracle: ≈ 12 GB dbcache, ~100–150 GB disk with indexes.
- Rust replay: ≈ 8–16 vCPU, 16–24 GB RAM, ~20–40 GB disk for chainstate, evo state and digests.

The box can hold both oracles plus one mainnet replay and one testnet replay at once. **Do not overlap replays with B1 benchmark runs.** B1 pins NUMA node 0 (`numactl --cpunodebind=0`), so schedule correctness replays on node 1, or in benchmark-free windows, and coordinate through STATUS.md.

**Cadence:**
- Testnet full replay: nightly.
- Mainnet full replay: weekly, and before every release.
- Segment replays: on every PR touching consensus crates.
- Golden-digest re-extraction: whenever the pinned dashd version changes.

#### 2.3.4 Segment replays and snapshots (fast PR gating)

Snapshot the Rust state (UTXO plus full evo state) once, at heights chosen around the risky transitions:

| Snapshot height | Transition |
|---|---|
| 68589 | DGW exactness |
| 328008 | treasury |
| 614820 | superblocks |
| 1028159 | DIP3 |
| 1047199 | DIP3 enforcement |
| 1088639 | DIP8 |
| 1374911 | BRR |
| 1737791 | DIP24 |
| 1899071 | V19 |
| 1987775 | V20 |
| 2128895 | MN_RR |
| 2201471 | WITHDRAWALS |
| tip − 50k | recent history |

Validate each snapshot against the oracle (muhash plus `protx list` plus `quorum listextended` plus `rotationinfo` plus credit pool at that height). Each PR then replays `[H, H+N]` windows: N = 20k blocks for the boundary windows, and covering ≥ 3 DKG cycles of every LLMQ type.

#### 2.3.5 Chain scans to run once (cheap, via the oracle)

- (a) txs with `nVersion > 3 && nType ≠ 0` (D§1.1);
- (g) the EMBARGO-1 scan (criteria and results private: ~/private/embargo/EMBARGO-1.md);
- (b) pre-DIP3 txs with non-zero type bytes and version < 3;
- (c) blocks whose coinbase pays more than the bound, i.e. the budget-window blocks (§1.7);
- (d) `bestCLHeightDiff ≥ height` or odd CbTx encodings;
- (e) MNHF signals by bit;
- (f) quorum commitments of disabled types after their disable height.

These confirm which quirks history actually exercises, and so must be tested hardest.

### 2.4 L3: differential block and tx mutation testing (rejection coverage)

Replay proves acceptance only. Rejection parity needs generated invalid inputs.

**Oracles:**
- `getblocktemplate {"mode":"proposal","data":<hex>}`: `TestBlockValidity` without PoW, on any block whose prev is the tip (`rpc/mining.cpp:731-759`). It is side-effect free and returns `null` or the BIP22 reject string, so it works against a *live mainnet or testnet* oracle as well as regtest.
- On regtest, where PoW is trivial: `submitblock`, plus P2P submission through the framework's `P2PInterface`, to compare misbehaviour and disconnect effects.
- For txs: `testmempoolaccept` and `sendrawtransaction` reject strings.
- Headless alternative: the experimental `dash-chainstate` binary (`--enable-experimental-util-chainstate`; mainnet hard-coded; reads hex blocks on stdin and prints `BlockValidationState`).

**Mutation engine.** About 40 operators, applied to real templates (`getblocktemplate`, or `generateblock submit=false` on regtest):

| Target | Mutations |
|---|---|
| Coinbase | value ±1 duff; drop, move or alter the MN payee, operator or platform OP_RETURN output |
| CbTx | each field and version; `bestCLHeightDiff ±1`; CL sig bit flips, all-zero, and opposite-scheme encodings |
| QcTx | insert, remove or duplicate; bitset bit flips and padding bits; version vs base; null vs real outside the window |
| ProTx (regtest, re-signed with known keys) | every field; uniqueness collisions inside one block; same-block collateral |
| Asset lock/unlock | fee, index reuse, height window edges, output limits; credit-pool balance |
| MNHF | duplicate bit, unknown bit, wrong quorum |
| Locktime | `nLockTime`/`nSequence` at the MTP and height boundaries |
| Limits | sigops at the limit and limit+1; sizes at exactly 100,000, 1,000,000 and 2,000,000 bytes |
| Merkle | duplicate-tx mutation (CVE-2012-2459) |
| Header | nBits ±1; time = MTP and MTP+1, and now+7200/+7201 (regtest mocktime) |
| Tx version/type | version/type combinations |
| Private | the EMBARGO-1 operator (PLAN §7 handling; ~/private/embargo/EMBARGO-1.md) |

**Workload generation on regtest:**
- Compress the activation heights (`-testactivationheight`, `-dip3params`, `-vbparams`).
- Run MiniWallet and DTF-style ProTx/QC workloads.
- Mine alternately with dashd and Rust (each GBT block submitted to the other).
- Compare StateDigest records per block over 10^5 blocks.

The rust-dashcore `regtest-mn` data (4 MNs, 8 DKG cycles) and `regtest-40000` give ready starting chains (F§6).

**Pass criterion:** identical verdict, reject string and result class for 100% of mutations.

Effort: M.

### 2.5 L4: Dash's functional tests as acceptance tests

**Feasibility: yes, with a thin compatibility layer.** From F§3:
- **Binaries:** the framework selects per-node binaries (`DASHD` env, `add_nodes(binary=[...])`).
- **Startup:** it starts nodes from `dash.conf` plus about 15 standard args; authenticates by the `.cookie` file; waits on `getblockcount` and `getmempoolinfo.loaded`.
- **Connecting:** it connects with `addnode … onetry` and waits for `pong` bytes ≥ 29 in `getpeerinfo.bytesrecv_per_msg` on both sides.
- **Logs:** 63 tests assert on `debug.log` strings.
- **Masternodes:** they force `-disablewallet`, so Rust masternodes need no wallet. `copy_datadir` failures are ignored, so a Rust masternode just syncs from node 0.

**Required Rust surface:**
- CLI and conf: accept or ignore every standard arg; implement the regtest knobs.
- `<datadir>/regtest/{.cookie, debug.log}`.
- JSON-RPC 1.0/2.0 with exact error codes and messages (`assert_raises_rpc_error` matches strings).
- Test RPCs: `setmocktime`, `mockscheduler`, `generatetoaddress`, `generateblock`, `generatetodescriptor`, `submitblock`, `submitheader`, `invalidateblock`, `reconsiderblock`, `syncwithvalidationinterfacequeue`, `waitforblockheight`.
- The P2P handshake from §2.7.

**Census:** 264 scripts use 209 RPC methods. 151 tests need no node wallet (many use MiniWallet), 83 need the wallet, and 34 use DashTestFramework (wallet only on node 0).

**Tiers and milestone gates** (lists in F§3.5):

| Tier | Gate for | Tests | Contents |
|---|---|---|---|
| **1** | "validating node" milestone | 35 tests, ~60 RPCs | `feature_block`, `feature_cltv`, `feature_dersig`, `feature_csv_activation`, `feature_nulldummy`, `feature_bip68_sequence`, `feature_utxo_set_hash` (golden), `feature_coinstatsindex`, `feature_new_quorum_type_activation` (dynamic versionbits), `feature_assumevalid`, `feature_minchainwork`, `feature_reindex`, `feature_loadblock`, `p2p_invalid_block`, `p2p_invalid_tx`, `p2p_unrequested_blocks`, `p2p_sendheaders(_compressed)`, `p2p_compactblocks`, `p2p_dos_header_tree` (real testnet X11/DGW headers), `p2p_mutated_blocks`, `rpc_invalidateblock`, `rpc_getchaintips`, `rpc_blockchain`, `rpc_getblockstats` (golden), `rpc_dumptxoutset`, `mempool_accept`, `mempool_reorg`, `mining_basic` … |
| **2** | "masternode" milestone; run as **mixed networks** (dashd node 0 = wallet, sporkkey, miner; Rust MNs) | 29 DTF tests | `feature_dip3_deterministicmns`, `feature_dip4_coinbasemerkleroots`, `feature_llmq_signing`, `feature_llmq_chainlocks`, `feature_llmq_rotation`, `feature_llmq_is_cl_conflicts`, `feature_llmq_is_retroactive`, `p2p_instantsend`, `feature_asset_locks`, `feature_mnehf`, `feature_governance(_cl)`, `feature_llmq_simplepose`, `feature_llmq_dkgerrors`, `feature_llmq_evo`, `feature_llmq_data_recovery`, `feature_llmq_connections`, `rpc_verifyislock`, `rpc_verifychainlock`, `rpc_quorum`, `feature_sporks`, `feature_protx_version`, v24 share/payout tests |
| **3** | fingerprint and ops conformance | — | `p2p_leak`, `p2p_fingerprint`, `p2p_ping`, `p2p_timeouts`, `p2p_addr_relay`, `p2p_getaddr_caching`, `p2p_v2_*`, `p2p_eviction`, `p2p_invalid_messages`, `p2p_permissions`, `interface_*`, `feature_config_args` |
| **4** | if the Rust wallet ships | 58 `wallet_*` tests | — |

**Porting approach.** Branch `node/func-compat` in `~/workspace/dash`:
- a runner flag `--rust-nodes=<mask>` (or a `RUSTD` env) mapping node indexes to the Rust binary;
- a tracked `rust_expected_failures.txt` listing test, reason and owner;
- a `debug.log` compatibility mode in the Rust node that emits Core's exact lines for the asserted messages (grep the 63 tests to build the list), or per-test patches where that is unreasonable.

Run tier 1 against **pure-Rust** networks and tier 2 as mixed networks. Then run each tier-2 test with *all* MNs Rust and dashd only as wallet/miner.

### 2.6 L5: mixed dashd/Rust masternode networks

**Stage A: regtest, scripted** (DashTestFramework plus the rust-dashcore `masternode_network.rs` harness, which already ports `mine_dkg_cycle`, `wait_for_chainlocked_block` and mocktime orchestration).

Alternate MN binaries: 5 MNs gives 3 Rust + 2 dashd; rotation (8 MNs) gives 4/4; also all-Rust. Pass criteria beyond the tests' own asserts:
1. Every Rust MN that is a member appears in `validMembers` of every mined commitment for every type: `quorum info … members[].valid`.
2. No PoSe penalty ever lands on a Rust MN: `protx info .state.PoSePenalty == 0`.
3. Recovered sigs, ChainLocks and islocks form when the threshold can only be met with Rust shares. Force this by stopping dashd MNs.
4. `quorum dkgsimerror` on Rust MNs gives the same complaint and justification outcome as on dashd.
5. Blocks built by the Rust GBT are accepted by dashd and vice versa, every block.
6. StateDigest equality on all nodes, every block.

**Stage B: devnet soak on agentbox** (docker compose, ≥ 2 weeks before testnet):
- **Topology:**
  - 1 dashd seed and miner;
  - 20 regular MNs (10 dashd, 10 Rust);
  - 8 EvoNodes (4/4);
  - 4 dashd wallet nodes generating traffic;
  - 1 Rust non-MN full node.
- **Devnet LLMQ types:**

  | Role | Type | Size / minSize / threshold | Flag |
  |---|---|---|---|
  | ChainLocks and MNHF | LLMQ_DEVNET | 12/7/6 | `-llmqchainlocks`, `-llmqmnhf` |
  | InstantSend | LLMQ_DEVNET_DIP0024 | 8/6/4 | `-llmqinstantsenddip0024` |
  | Platform | LLMQ_DEVNET_PLATFORM | 12/9/8 | `-llmqplatform` |

- **Timing:** `-powtargetspacing=30..60` speeds up DKG cycles. `-highsubsidyblocks` funds the network (B§2.6 notes a possible devnet block-1 subsidy issue; verify empirically).
- **Workload:**
  - continuous payments and IS txs;
  - CoinJoin mixing by the dashd wallets, with Rust and dashd MNs as coordinators;
  - governance proposals and votes → superblocks, with triggers created by both implementations;
  - asset locks and unlocks signed by platform quorums (`quorum platformsign`);
  - an EHF-activated test deployment (`-vbparams … useEHF`);
  - ProUpServ/ProUpReg/ProUpRev churn;
  - V24 / evo_shares txs when testing V24.
- **Chaos:** process kills and restarts, `tc netem` partitions and latency, clock skew (±60 s), mixed-version rolling upgrades, disk-full on one node.
- **Metrics** (Prometheus or plain logs):
  - DKG success per type, plus the Rust fraction of `validMembers`;
  - CL latency and coverage per block;
  - IS lock latency;
  - PoSe events by implementation;
  - CoinJoin rounds per coordinator implementation;
  - superblock payouts;
  - StateDigest or best-hash divergence (any divergence pages the manager).

**Stage C: public testnet.** Run Rust MNs and an EvoNode next to the BS2 testnet oracle. Collateral is 1000/4000 tDASH via the `dash-faucet` skill (rate-limited; plan ahead). Testnet already runs protocol 70242 with V24 signalling, so this exercises V24 activation live.

**Stage D: mainnet.** Start with a non-MN Rust node with digest comparison against the mainnet oracle. A mainnet MN needs pasta's decision: 1000/4000 DASH collateral and PoSe risk.

### 2.7 L6: P2P fingerprint conformance ("very hard to tell from dashd")

**Static checklist** (live rc.3 capture, F§5):
- `version`: 137 bytes; trailing `mnauthChallenge` (32 bytes) and `fMasternode` (1 byte); services `0x1c05` (+`0x40` `COMPACT_FILTERS` on MNs); UA `/Dash Core:24.0.0/` (no rc suffix; `devnet.<name>` comment on devnets); protocol 70242.
- Before `verack`, only `sendaddrv2` is sent.
- After `verack`, in order:
  1. `mnauth` if this node is an MN;
  2. `sendheaders2` or `sendheaders`;
  3. `sendcmpct(0,1)`;
  4. `senddsq(1)`;
  5. `ping`;
  6. `getheaders2` or `getheaders`.
- Then `getsporks` (once per address) and `govsync` (≈ 8 s later); `mempool` to outbound peers after sync; `getaddr` to outbound peers only.
- Never `feefilter`, `wtxidrelay`, `reject` or `sendtxrcncl` (default).
- BIP324 is on by default, with Dash short IDs 128–168 and slot 5 empty.

**Behavioural probes.** Scripted in Python on the framework's `P2PInterface`/`v2_p2p.py`, run against both binaries, with normalized transcripts diffed:

| Area | What is probed |
|---|---|
| Headers | limits 2000 / **8000 (headers2, ≥70235)** |
| Addr | `getaddr` cache and the 23% cap; `addr` rate limiting |
| Stale blocks | hiding (30-day / 7-day rules); NODE_NETWORK_LIMITED depth 288 |
| Inventory | `getdata`/`notfound` behaviour |
| Errors | unknown-message handling; malformed message → misbehaviour points and disconnect timing (score table in F§5.4) |
| Compact blocks | HB/LB mode selection |
| Timers | ping interval, timeouts (2 min ping, 20 min timeout, 60 s handshake) |
| Masternode-specific | MNAUTH, `qwatch`, DSQ relay, eviction protection of verified MNs, refusing inbound before sync, disconnecting recently-synced peers during mnsync |

**Timing statistics:**
- Inventory trickle: Poisson, mean 5 s inbound, 2 s outbound, 1 s to verified-MN outbound; immediate for NoBan and MN↔MN.
- `INVENTORY_BROADCAST_PER_SECOND` 7; addr broadcast ~30 s.
- Compare inter-message-gap distributions with Kolmogorov–Smirnov tests over ≥ 10^4 samples.

**Passive traces:** run dashd (`-capturemessages`, parsed with `contrib/message-capture/message-capture-parser.py`) and the Rust node (same capture format) side by side on testnet and mainnet with identical configs. Compare per-peer message-type n-grams and timing histograms.

**Adversarial classifier:** a gradient-boosted classifier trained on features from probes plus passive traces (message order, timing, sizes, error responses). Acceptance target: held-out AUC ≤ 0.55. Re-run on every release of either implementation.

**CI:** the probe suite runs against the pinned dashd and the Rust node on every P2P-touching PR; the transcript diff must be empty except for an explicit allow-list (nonces, timestamps, random ordering).

### 2.8 Cadence, gates and first tasks

**Gate schedule:**

| When | Runs |
|---|---|
| Per PR (consensus crates) | L0 suites; L1 fuzz smoke (5 min/target with corpora); L3 regtest mutation subset (≈ 500 mutations); the tier-1 functional subset for the touched area; segment replays around the touched transitions |
| Nightly | L1 fuzz 8 h on 16 cores; testnet full replay plus digest compare; L3 long run; tier 1 and tier 2 (mixed); fingerprint probe suite |
| Weekly | mainnet full replay plus digest compare; devnet soak report; rebase oracle-shim and state-digest branches onto the pinned Core; refresh golden digests if the pin moved |
| Release | mainnet and testnet replay with `-assumevalid=0` and with default assumevalid (perf parity); ≥ 2-week devnet soak; testnet MN run; fingerprint classifier |

**Proposed first briefs:**

| ID | Task | Diff. |
|---|---|---|
| V0 | **Escalate EMBARGO-1 to pasta** (private disclosure) | — |
| V1 | `dash-consensus-vectors` crate: port the JSON suites and embedded vectors; regenerate rust-dashcore SML fixtures from dashd | S–M |
| V2 | Oracle shim (`node/oracle-shim` in Dash Core) + `dashcore-oracle-sys` + P0 fuzz targets (codec, script, sighash, sigops, PoW/DGW, subsidy, BLS parse/verify) | M |
| V3 | StateDigest v1 spec, golden extractor (Rust, on rust-dashcore's `rpc-client`/`rpc-json`) against BS2, and comparator | M |
| V4 | `node/state-digest` dashd patch (`-statedigestlog`, `CanonicalMNListHash` etc.) | S–M |
| V5 | One-off chain scans (§2.3.5) | S |
| V6 | Functional-test compat runner (`node/func-compat`) + Rust test-RPC surface spec (tier 1) | M |
| V7 | L3 mutation engine + GBT-proposal differential harness | M |
| V8 | Mixed-MN regtest harness (DTF `binary=` + rust-dashcore `masternode_network.rs`) + devnet compose | M |
| V9 | Fingerprint probe suite + capture tooling + classifier | M |

### 2.9 Risks and open questions

**Risks:**
1. **Moving target.** V24 and evo_shares rules differ between v23.1.8, develop and rc.3 (e.g. the V24 unlock limit). Mitigation: pin per network; weekly develop watch; V24 as its own milestone.
2. **Governance is unverifiable by replay.** Only L4/L5 cover superblock payees and trigger agreement. Budget a full governance implementation for MN parity, since MNs create and vote triggers.
3. **Node-local rules** (CL/IS enforcement) diverge silently if skip conditions differ. Mitigation: tier-2 conflict tests (`feature_llmq_is_cl_conflicts`) and devnet chaos.
4. **BLS legacy-scheme exactness in pure Rust** (dash-pkc). Mitigation: L1 diff-fuzz against dashbls, plus replay of 1028160..1899071 with assumevalid=0.
5. **Performance pressure** ("never slower than dashd") on consensus code. Mitigation: no optimization merges without L1–L3 parity; replays double as perf benchmarks only through B1's harness.
6. **Box contention** between oracles, replays and B1 benchmarks. Mitigation: scheduling and NUMA separation; document in STATUS.md.
7. **Upstream Core bugs** (EMBARGO-1, and any found by L1). Policy: replicate, report privately, follow upstream's fix in lockstep.

**Open questions for pasta:**
1. Disclosure path and timing for EMBARGO-1 (pasta is a Dash Core maintainer).
2. Confirm the reference pins: v23.1.x for mainnet and v24.0.0 final for testnet, and when to move mainnet to v24.
3. May we carry test-only patches in Dash Core branches (oracle shim, state digest, functional-test compat), and should any be upstreamed? A `getevostatehash`/EvoSnapshot RPC and special-tx/BLS fuzz targets would benefit Core.
4. Testnet MN collateral, and whether a mainnet Rust MN is ever in scope for the "done" criteria.

---

## Appendix index

- `R4-appendix/A-tx-script.md`: tx serialization, CheckTransaction, contextual checks, sigops, interpreter, flags, sighash, policy, libdashconsensus, JSON vectors.
- `R4-appendix/B-block-pow-params.md`: params tables for all networks, PoW/DGW/KGW, block pipeline and reject strings, versionbits, the non-chain-state inventory.
- `R4-appendix/C-subsidy-payments-gov.md`: subsidy eras with exact amounts, MN share, payee selection, superblocks and governance determinism, sporks, credit-pool accounting.
- `R4-appendix/D-specialtx-dmn.md`: every special tx type: layouts, rules, reject strings; the DMN state machine; CbTx; MNHF; asset lock/unlock; V24 shared MNs.
- `R4-appendix/E-llmq-cl-is.md`: LLMQ params, commitments, member selection and rotation, BLS schemes, ChainLocks, InstantSend, signing, DKG, EHF.
- `R4-appendix/F-verification-assets.md`: vectors, fuzz targets, functional framework and RPC census, state RPCs, UTXO hash algorithms, P2P fingerprint surface, tools, chain size.
- `R4-appendix/G-rust-reuse.md`: rust-dashcore inventory, exactness checks, gaps.
- `R4-appendix/repro/`: moved in full to the private directory on 2026-10-08 (EMBARGO-1: see ~/private/embargo/EMBARGO-1.md). Only `README.txt` and `mk.py` are embargoed; `utxohash.py` (hash_serialized_2 and muhash reimplementation), `handshake.py` (P2P probe), `census.txt` and `test_matrix.txt` (functional-test RPC and wallet census) are not, and the manager may restore them to git.
