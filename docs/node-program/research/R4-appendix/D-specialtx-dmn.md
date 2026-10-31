> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# D — DIP2/3/4 special transactions & deterministic masternode list (Dash Core develop @ 15db478750)

All paths are relative to `/work/home/agent/workspace/dash` unless prefixed `rust-dashcore:`.
Every rule cites file:line in develop; historical variants cite tags (`git show <tag>:<file>`).
LLMQ commitment internals, BLS scheme maths, credit-pool limit math, payments and chainlock quorum
selection are owned by other agents — only the interfaces are recorded here.

## 0. Conventions that matter everywhere

* `DeploymentActiveAfter(pindexPrev, dep)` ⇔ `pindexPrev->nHeight + 1 >= DeploymentHeight(dep)` — i.e. "rule applies
  to the block being built/validated" (src/deploymentstatus.h:13-17). `DeploymentActiveAt(*pindex, dep)` ⇔
  `pindex->nHeight >= height` (src/deploymentstatus.h:34-50). **Watch for `DeploymentActiveAt(*pindexPrev, …)`**: it means
  block height ≥ activation+1 (off-by-one, used for DIP0008 in CheckCbTx and formerly for WITHDRAWALS).
* `DeploymentDIP0003Enforced(nHeight)` = `nHeight >= DIP0003EnforcementHeight` (src/deploymentstatus.h:66-69); it is
  called with `pindexPrev->nHeight`, so it applies from block EnforcementHeight+1.
* Mainnet heights (src/chainparams.cpp:177-197): nMasternodeMinimumConfirmations=15; DIP0003Height=1028160;
  DIP0003EnforcementHeight=1047200 with **checkpoint hash** DIP0003EnforcementHash=0x…2d1734087b4c5afc3133e4e1c3e1a89218f62bcd9bb3d17f81
  (chainparams.cpp:187, enforced in deterministicmns.cpp:773-780 → "bad-dip3-enf-block"); DIP0008Height=1088640;
  DIP0024Height=1737792; V19Height=1899072; V20Height=1987776; MN_RRHeight=2128896; WithdrawalsHeight=2201472.
  The only versionbits deployment is DEPLOYMENT_V24 (bit 13, useEHF, window 4032) which is **NEVER_ACTIVE on mainnet**
  (chainparams.cpp:212-219; also NEVER_ACTIVE in v23.1.8 (bit 12!) and v24.0.0-rc.2). Testnet start 2026-09-24
  (chainparams.cpp:413). Everything tagged "V24" below is therefore *pending* on mainnet.
* LLMQ types used here (mainnet): llmqTypeChainLocks=LLMQ_400_60, llmqTypePlatform=LLMQ_100_67 (dkgInterval 24,
  signingActiveQuorumCount 24), llmqTypeMnhf=LLMQ_400_85 (chainparams.cpp:277-280; llmq/params.h:464-478).
* Mainnet ports: P2P 9999; platform P2P 26656, platform HTTP 443 (chainparams.cpp:233-235). Testnet 19999/22000/22001,
  devnet 19799/22100/22101. RequireRoutableExternalIP = true on main/test/devnet, false on regtest (chainparams.cpp:283/477/660/857).
* Payload (de)serialization stream: `CDataStream(SER_NETWORK, PROTOCOL_VERSION)` (no ADDRV2 flag → CService is the
  legacy 16-byte+BE-port form). Parsing **must consume the whole vExtraPayload** (`ds.empty()`), else nullopt
  (src/evo/specialtx.h:17-28). Exceptions → nullopt. `SerializeHash(x)` = SHA256d over SER_GETHASH serialization.
* `CalcTxInputsHash(tx)` = SHA256d(concat of every `vin[i].prevout` = 32-byte hash || u32 LE n), no length prefix
  (src/evo/specialtx.cpp:10-17). `CheckInputsHash` → "bad-protx-inputs-hash"
  (src/evo/providertx.h:559-566). `CalcTxOutputsHash` = SHA256d(concat of serialized CTxOut) (specialtx.cpp:19-26).

---------------------------------------------------------------------------------------------------------------------

## 1. Framework (difficulty: M)

### 1.1 TxType enum and transaction wire format
* `src/primitives/transaction.h:28-42`: 0 NORMAL, 1 PROVIDER_REGISTER, 2 PROVIDER_UPDATE_SERVICE, 3 PROVIDER_UPDATE_REGISTRAR,
  4 PROVIDER_UPDATE_REVOKE, 5 COINBASE, 6 QUORUM_COMMITMENT, 7 MNHF_SIGNAL, 8 ASSET_LOCK, 9 ASSET_UNLOCK,
  **10 PROVIDER_DISSOLVE, 11 PROVIDER_UPDATE_SHARE, 12 PROVIDER_UPDATE_SHARED_REGISTRAR** (10-12 are V24 shared
  masternodes; not in v23).
* Wire: one 32-bit field (`nVersion` low half, `nType` high half), vin, vout, nLockTime, then
  `if (nVersion >= 3 && nType != 0) vExtraPayload` as compact-size byte vector (transaction.h:269-278, 354-365).
  `nType` is parsed for *every* tx,
  including nVersion<3 ones. No segwit in Dash.
* `IsSpecialTxVersion()` = nVersion ≥ 3; `HasExtraPayloadField()` = special version && nType≠0 (transaction.h:325-338).
  Historical (corrected in the R4 fact-check): before v21 (commit 72d2008901, 2024-02), (de)serialization, the sighash payload
  inclusion, CheckSpecialTx and most evo code used `nVersion == 3`; only ContextualCheckTransaction used `>= 3`. So a
  nVersion≥4 tx with nType≠0 was parsed *without* a payload then, but would be parsed *with* one today.
  **Action: scan mainnet for txs with nVersion>3 && nType≠0** (policy TX_MAX_STANDARD_VERSION=3, src/policy/policy.h:95,
  so only miner-inserted ones could exist).
* V24 only — stable txid for asset-unlock payload v2: if `nVersion>=3 && nType==9 && payload.size()==145 && payload[0]>=2`
  (transaction.h:44-56, 382-403) the txid is SHA256d of the full serialization with the **last 132 bytes zeroed**
  (requestedHeight‖quorumHash‖quorumSig) (src/primitives/transaction.cpp:81-88, 115-119); `GetInstanceHash()` = plain
  full hash (transaction.cpp:121-125). Judged on raw bytes before payload validation.

### 1.2 Context-free and contextual tx checks touching special txs
* `CheckTransaction` (src/consensus/tx_check.cpp:14-75): empty vin+vout allowed when `nType ∈ {6,7}`, empty vin allowed
  when `nType==9` — **uses nType without checking nVersion** (lines 16-24). `vExtraPayload.size() > MAX_TX_EXTRA_PAYLOAD
  (10000, consensus/consensus.h:22)` → "bad-txns-payload-oversize" (34-35). Coinbase scriptSig length 1..100 if
  nType==COINBASE else 2..100 → "bad-cb-length" (60-67).
* `ContextualCheckTransaction` (src/validation.cpp:351-386): only if DIP3 active for the block: special-version txs must
  have nType ∈ {0..12} else "bad-txns-type"; a special-version coinbase must be nType 5 else "bad-txns-cb-type";
  non-special-version txs must have nType 0 else "bad-txns-type". **Pre-DIP3 there is no type check** (but see 1.3).
  Note develop accepts types 10-12 here even pre-V24; they die later with "*-too-early".
* `ContextualCheckBlock` (validation.cpp:4575-4591): BIP34 height-in-coinbase enforced only while DIP3 inactive;
  once DIP3 active the coinbase must have `nType == TRANSACTION_COINBASE` → "bad-cb-type".

### 1.3 CheckSpecialTx dispatch (src/evo/specialtxman.cpp:217-282)
1. `!HasExtraPayloadField()` → OK.
2. DIP3 not active after pindexPrev → "bad-tx-type-dip3-inactive" (TX_BAD_SPECIAL). (Old name "bad-tx-type", v20.)
3. switch nType: 1→CheckProRegTx, 2→CheckProUpServTx, 3→CheckProUpRegTx, 4→CheckProUpRevTx, 10→CheckProDisTx,
   11→CheckProUpShareTx, 12→CheckProUpSharedRegTx, 5→ (must be IsCoinBase else "bad-cbtx-invalid"; payload parse else
   "bad-cbtx-payload"; CheckCbTx), 6→llmq::CheckLLMQCommitment, 7→CheckMNHFTx, 8→CheckAssetLockTx, 9→CheckAssetUnlockTx.
4. Unknown type → "bad-tx-type-check". Any std::exception → "failed-check-special-tx" (TX_CONSENSUS).
* **Activation gates now present**: only DIP3 (all types) + V24 for 10/11/12 (inside the checkers). Historical gates that
  develop removed (more permissive now, safe for replaying history): v20 required V20 for types 7 and 8 and **MN_RR for type 9
  on mainnet** ("mnhf-before-v20", "assetlocks-before-v20", "assetunlocks-before-mn_rr"; `git show v20.1.1:src/evo/specialtxman.cpp`);
  removed in v22 (commits b3e9e5c981, e0d97cf7ac, 1d96fbf091).

### 1.4 Block processing order — ConnectBlock (validation.cpp:2483-2730)
* `ScopedBLSLegacyScheme(!DeploymentActiveAt(*pindex, V19))` sets the process-wide BLS scheme: **legacy for blocks
  < V19Height, basic for ≥ V19Height** (validation.cpp:2491-2492). Every *unwrapped* BLS (de)serialization and every
  `VerifyInsecure(pk, hash)` without explicit scheme uses it (src/bls/bls.cpp:296-312, bls.h:160-240).
* `ProcessSpecialTxsInBlock` is called **before any UpdateCoins / CheckTxInputs of the block** (validation.cpp:2662-2668),
  so `view` = UTXO set as of pindexPrev. `fCheckCbTxMerkleRoots = fScriptChecks` (assumevalid turns off CbTx checks, credit
  pool balance check and ProTx/BLS sig checks — a performance shortcut, not a rule).
* ProcessSpecialTxsInBlock (specialtxman.cpp:798-1001), exact order:
  1. If checking & vtx[0].nType==5: vtx[0] must be IsCoinBase ("bad-cbtx-invalid"), payload parse ("bad-cbtx-payload"),
     `CheckCbTx(cbTx, pindex->pprev, is_v24_active)` (817-834). If DIP3 active at pindex and no CbTx → "bad-cbtx-version" (835-840).
  2. If V20 active at pindex: load credit-pool `indexes` at pprev (848-852).
  3. For every tx i: if V24 → `CheckSharedCollateralTemplateOutputs` (incl. coinbase) (857-863); skip i==0 if CbTx; run
     `CheckSpecialTxInner(&chain, tx, pindex->pprev, v24, view, indexes, check_sigs=fCheckCbTxMerkleRoots)` (865-878).
     *Every special tx is validated against pindexPrev state* — txs in the same block cannot see each other here.
  4. If CbTx parsed: `CheckCreditPoolDiffForBlock` (884-890; body 1038-1069: requires DIP0008 and V20 *at pindex*;
     `GetCreditPoolDiffForBlock` then `cbTx.creditPoolBalance == diff.GetTotalLocked()` else "bad-cbtx-assetlocked-amount").
  5. `m_qblockman.ProcessBlock` (quorum commitments; LLMQ agent) (897-900).
  6. If DIP3 active at pindex: `BuildNewListFromBlock` → set blockHash → `m_dmnman.ProcessBlock` (diff/snapshot write,
     DIP3 enforcement-hash check) (907-919).
  7. If CbTx: merkleRootMNList == CalcCbTxMerkleRootMNList(new list SML) else "bad-cbtx-mnmerkleroot"; if CbTx.nVersion ≥ 2:
     merkleRootQuorums == CalcCbTxMerkleRootQuorums(block, pprev) else "bad-cbtx-quorummerkleroot"; CheckCbTxBestChainlock (926-971).
  8. `m_mnhfman.ProcessBlock` (975-978).
  9. If V19 active *after* this block, flip global scheme to basic (985-990).
  Catch: EvoDbInconsistencyError → state.Error (not invalid); other std::exception → "failed-procspectxsinblock" (BLOCK_CONSENSUS).
  **Consequence:** any `runtime_error` thrown by `AddMN/UpdateMN/RemoveMN` on a duplicate unique property in-block makes
  the block invalid with this reason (deterministicmns.cpp:439-679).
* After ProcessSpecialTxsInBlock: per-tx CheckTxInputs; V24 shared-collateral spend check uses the in-block view
  (validation.cpp:2686-2715). Asset unlocks have no inputs: CheckTxInputs short-circuits to `GetAssetUnlockFee`
  (src/consensus/tx_verify.cpp:165-167; assetlocktx.cpp:255-267: fee==0 or !MoneyRange → "bad-txns-assetunlock-fee-outofrange").
* CbTx v4 merkleRootAssetUnlocks is checked in `CheckMerkleRoot`-time block checks as **BLOCK_MUTATED** (not invalid)
  "bad-cbtx-assetunlockmerkleroot" (validation.cpp:4344-4357).

### 1.5 Undo (specialtxman.cpp:1003-1036)
Order: restore legacy BLS scheme if undoing the V19 activation block → `mnhfman.UndoBlock` (sanity only) →
`dmnman.UndoBlock` (drops cached list/diff; nothing to recompute) → `qblockman.UndoBlock`. Credit pool has no explicit undo
(per-block cache). DisconnectBlock calls it at validation.cpp:2311.

### 1.6 Mempool vs block
* Mempool: `CheckSpecialTxForMempool` (validation.cpp:802-825) → `CheckSpecialTx(tx, tip, v24-after-tip, CoinsTip(),
  check_sigs=true)` with `chain=nullptr` and `indexes=nullopt`; **view is the chainstate UTXO only — mempool outputs can never
  be collateral** (comment validation.cpp:1098-1104). Called after all normal checks (1103-1104), then
  `existsProviderTxConflict` → "protx-dup" (1106; txmempool.cpp ~1576-1660: addresses, owner keys, operator key hash,
  platformNodeID, collateral, proTxHash refs).
* Policy only: `IsStandardSpecialTx` (specialtxman.cpp:1841-1868): asset lock >100 inputs "assetlocktx-too-many-inputs",
  >20480 bytes "assetlocktx-too-big", payload v≥2 "assetlocktx-version-2". Asset unlock whose index is already in the credit
  pool → "txn-already-known" (validation.cpp:928-940). V24 v2 unlocks get an "expired window" admission path (810-824).
* Block: same checkers but against `pindex->pprev`, `view` = pprev UTXO, `indexes` = pool indexes at pprev, plus in-block
  re-checks inside BuildNewListFromBlock (§3) and credit-pool diff (§6).

---------------------------------------------------------------------------------------------------------------------

## 2. Provider transactions (difficulty: L; XL with V24 shared MNs/ExtNetInfo)

### 2.1 Versions and types
* `ProTxVersion` LegacyBLS=1, BasicBLS=2, ExtAddr=3; `GetMax(v19, v24)` = 1 / 2 / 3 (src/evo/types.h:15-35).
  Deserializers bail (leaving bytes → "bad-protx-payload") if `nVersion==0 || nVersion>3` (providertx.h:167-171 etc.).
  `GetValidatedPayload` (specialtxman.cpp:1099-1124): wrong type "bad-protx-type"; parse fail "bad-protx-payload";
  `nVersion > GetMax(V19 after pindexPrev, v24)` → "bad-protx-version"; then IsTriviallyValid.
* `MnType` Regular=0 (1000 DASH, weight 1), Evo=1 (4000 DASH, weight 4); serialized as raw uint16, any value parses;
  `IsValidMnType` = <2 (src/evo/dmn_types.h:52-104).
* BLS encoding per version: operator pubkey and BLS sigs inside ProTx payloads use **legacy encoding iff nVersion==1**
  (version wrappers). `CBLSLazyPublicKey` is lazy: unserialize stores 48 bytes; validity only checked in `Get()` (point
  parse + re-serialization equality "CheckMalleable"); all-zero = null (bls.h:392-576). `CBLSSignature` is eager: bytes
  invalid in the requested scheme are retried in the opposite scheme; valid-in-opposite → object reset to *invalid*;
  invalid in both → throw → payload parse failure. All-zero = invalid/null; point-at-infinity bytes → throw
  (bls.h:100-122, 204-226).
* **Signature verification scheme is decoupled from encoding**: `CheckHashSig(ptx, CBLSPublicKey)` →
  `sig.VerifyInsecure(pubKey, SerializeHash(ptx))` → global scheme (specialtxman.cpp:1090-1097; bls.cpp:309-312), i.e.
  LegacySchemeMPL (ep2_map_legacy hash-to-G2, dashbls schemes.cpp:653-668, elements.cpp:334-348) for blocks < V19Height,
  BasicSchemeMPL for ≥ V19Height **even for v1 (legacy-encoded) ProUpServ/ProUpRev**. Same in v19-v21.

### 2.2 Byte layouts (all LE ints unless noted; CService = 16-byte IPv6/IPv4-mapped + **big-endian** u16 port)
* **CProRegTx** (providertx.h:162-227): nVersion u16 | nType u16 | nMode u16 | collateralOutpoint (32+u32) |
  netInfo (v<3: MnNetInfo = one CService; v3: ExtNetInfo) | keyIDOwner 20 | pubKeyOperator 48 | keyIDVoting 20 |
  nOperatorReward u16 | v<3: scriptPayout (varbytes) — v3: u8 payouts_count, payouts[{script varbytes, reward u16}],
  u8 shares_count, shares[{amount i64, scriptRefund, scriptReward, keyIDOwner 20}], joinSigs[65 raw bytes × shares_count],
  nEarlyPeriodBlocks u32, nEarlyPenalty i64 | inputsHash 32 | if nType==Evo: platformNodeID 20, and if v<3:
  platformP2PPort u16 **LE**, platformHTTPPort u16 LE | vchSig (varbytes; omitted under SER_GETHASH).
  Historical: v18 had no version-gating and serialized pubkey with the global (legacy) scheme; v19 serialized platform
  fields only if `nVersion==2 && nType==HPMN` (`git show v19.3.0:src/evo/providertx.h`). Develop serializes them for any
  version with nType==Evo — harmless because v1+Evo is invalid in both ("bad-protx-evo-version" now,
  "bad-protx-platform-nodeid" then).
* **CProUpServTx** (providertx.h:269-304): nVersion u16 | v≥2: nType u16 (v1 ⇒ Regular) | proTxHash 32 | netInfo |
  scriptOperatorPayout varbytes | inputsHash 32 | if nType==Evo: platformNodeID 20, v<3: p2p u16, http u16 | sig 96 (not in GETHASH).
* **CProUpRegTx** (providertx.h:333-368): nVersion | proTxHash | nMode u16 | pubKeyOperator 48 | keyIDVoting 20 |
  v<3 scriptPayout / v3 u8 count + payouts | inputsHash | vchSig varbytes (not in GETHASH).
* **CProUpRevTx** (providertx.h:402-422): nVersion | proTxHash | nReason u16 | inputsHash | sig 96 (not in GETHASH).
  Reasons 0 NOT_SPECIFIED, 1 TERMINATION_OF_SERVICE, 2 COMPROMISED_KEYS, 3 CHANGE_OF_KEYS (informational) (388-394).
* V24 only: **CProDisTx** (type 10) nVersion u16 | proTxHash | actorIndex u16 | u8 sig_count | 65-byte sigs (452-462);
  **CProUpShareTx** (11) nVersion | proTxHash | shareIndex u16 | scriptReward | inputsHash | vchSig (497-503);
  **CProUpSharedRegTx** (12) nVersion | proTxHash | pubKeyOperator (always basic) | keyIDVoting | inputsHash |
  [non-GETHASH: u8 count + 65-byte sigs] (531-545).
* **MnNetInfo** (src/evo/netinfo.h:313-372): wire = one legacy CService. **Unserialize normalizes: if `!CService::IsValid()`
  the entry becomes empty; Serialize writes `CService{}` (16 zero bytes + port 0) for empty.** (since v23; commit 6286c9bd71).
* **ExtNetInfo** (netinfo.h:374-464): u8 version (1; 0 or >1 bails) | `std::map<u8 purpose, vector<NetInfoEntry>>`.
  NetInfoEntry: u8 type (0x01 Service → CService in **ADDRv2** form, 0x02 Domain → LIMITED_STRING(253) + u16 BE port,
  anything else → Invalid, re-serialized as single 0xff) (netinfo.h:194-263). std::map deserialization sorts keys and
  **silently drops duplicate keys**; invalid CService entries become Invalid. Purposes CORE_P2P=0, PLATFORM_P2P=1,
  PLATFORM_HTTPS=2 (70-76). Max 4 entries per purpose (20).

### 2.3 IsTriviallyValid (context-free)
* ProRegTx (providertx.cpp:265-336), in order: version 0/>3 "bad-protx-version"; v<2 && Evo "bad-protx-evo-version";
  invalid type "bad-protx-type"; nMode≠0 "bad-protx-mode"; [shared: Evo "bad-protx-shares-evo", external collateral
  "bad-protx-shares-external", keyIDOwner non-null "bad-protx-shares-owner-key", payouts non-empty "bad-protx-shares-payouts"]
  [non-shared: early fields ≠0 "bad-protx-shares-empty-fields", null owner "bad-protx-key-null"]; invalid operator key or
  null voting "bad-protx-key-null"; `pubKeyOperator.IsLegacy() != (v==1)` "bad-protx-operator-pubkey" (always consistent
  after deserialization); shared → IsShareListTriviallyValid; else IsPayoutListTriviallyValid(GetOwnerPayouts) with the
  owner+voting keys; netInfo kind ≠ (v≥3) "bad-protx-netinfo-version"; non-empty netInfo → IsNetInfoTriviallyValid; any
  entry !IsTriviallyValid "bad-protx-netinfo-bad"; nOperatorReward > 10000 "bad-protx-operator-reward".
* `IsPayoutListTriviallyValid` (providertx.cpp:25-61): legacy scriptPayout is wrapped as one share with reward 10000
  (providertx_util.cpp:19-22). 1..8 entries "bad-protx-payouts-count"; reward 100..10000 "bad-protx-payout-reward";
  script must be exact P2PKH or P2SH template "bad-protx-payee"; duplicate script "bad-protx-payee-dup"; ExtractDestination
  "bad-protx-payee-dest"; dest == PKHash(owner) (if owner non-null) or PKHash(voting) "bad-protx-payee-reuse";
  sum ≠ 10000 "bad-protx-payout-reward-sum". (Equivalent to v18 rules for single legacy payee.)
* ProUpServTx (providertx.cpp:412-440): version; v<2 && Evo "bad-protx-evo-version"; type; netInfo kind; netInfo empty
  "bad-protx-netinfo-empty"; IsNetInfoTriviallyValid; entry trivially valid. (v18 only checked version; address checks
  were contextual "bad-protx-ipaddr*".)
* ProUpRegTx (459-476): version; nMode; key null; operator-pubkey-legacy consistency; payout list (owner unknown → CKeyID{}).
* ProUpRevTx (486-498): version; nReason > 3 "bad-protx-reason".
* IsNetInfoTriviallyValid (166-185): CORE_P2P required "bad-protx-netinfo-empty"; Regular must not have platform
  purposes "bad-protx-netinfo-bad"; ExtNetInfo+Evo needs both platform purposes "bad-protx-netinfo-empty".

### 2.4 Network-field rules — CheckProviderNetworkFields (providertx.cpp:212-263)
* netInfo null or kind mismatch → "bad-protx-netinfo-version" (TX_CONSENSUS). Empty: ProRegTx allowed (MN starts banned),
  ProUpServTx "bad-protx-netinfo-empty". Non-empty: IsNetInfoTriviallyValid then `Validate()` mapped by CheckNetInfo
  (187-210): BadAddress→"bad-protx-netinfo-addr", BadPort→"-port", BadType→"-addr-type", NotRoutable→"-addr-unroutable",
  Malformed→"bad-protx-netinfo-bad", BadInput→"bad-protx-netinfo-entry", Duplicate→"bad-protx-dup-netinfo-entry",
  MaxLimit→"bad-protx-netinfo-maxlimit".
* **MnNetInfo::Validate** (netinfo.cpp:286-305, 351-357): entry trivially valid else Malformed; `CService::IsValid`; **IPv4
  only**; routable if RequireRoutableExternalIP; **mainnet: port must equal 9999; other nets: port must NOT be 9999**.
  Same as v18 `CheckService` (`git show v18.2.2:src/evo/deterministicmns.cpp`), but v18 treated any addr ≠ `CService()`
  (even invalid, e.g. 0.0.0.0:9999) as "set" and rejected it; v23+ normalizes such a CService to empty → accepted as
  "no address" (relaxation).
* ExtNetInfo::Validate (V24; netinfo.cpp:452-498, 588-638): version 1, non-empty, no addr:port dups, valid purposes,
  ≤4 per purpose, no addr dups within a purpose; services IPv4/IPv6/Tor/I2P/CJDNS, routable, I2P port must be 0 (SAM3.1),
  others port≠0 and `!IsBadPort` unless PLATFORM_HTTPS:443; domains only for PLATFORM_HTTPS, RFC1035 charset, 3..253
  chars, ≥2 labels, labels 1..63 not starting/ending '-', lowercase, TLD alphabetic, bad TLD list (.mail .arpa .example
  .invalid .localhost .test .local .corp .home .internal .intranet .lan .private) and .i2p/.onion rejected.
  **No mainnet-9999 rule for ExtAddr core P2P.**
* Evo platform fields: platformNodeID null → "bad-protx-platform-nodeid". v≥3: scalar ports must be 0. v<3: on mainnet
  p2p==26656 and http==443 required ("bad-protx-platform-p2p-port"/"-http-port"); on all nets neither may equal
  **mainnet** 9999; p2p≠http≠core port else "bad-protx-platform-dup-ports" (core port = primary CService port, 0 if empty).
  Identical to v19 `CheckPlatformFields`.

### 2.5 CheckProRegTx (specialtxman.cpp:1150-1314)
Order: GetValidatedPayload; V24: v<2 "bad-protx-version-disallowed"; CheckProviderNetworkFields(allow_empty);
expected collateral = 1000/4000 DASH by nType.
* External collateral (hash≠0): `view.GetCoin` (pprev UTXO) must exist, unspent, value == expected else
  "bad-protx-collateral" (1178-1182); ExtractDestination else "bad-protx-collateral-dest"; must be **PKHash** (P2PKH, or
  P2PK → hash160 of the pubkey as encoded) else "bad-protx-collateral-pkh". ⇒ external collateral created in the same
  block or only in the mempool is invalid.
* Internal (hash==0): n < vout.size() else "bad-protx-collateral-index"; vout[n].nValue == expected else
  "bad-protx-collateral"; shared → script must be exactly the template else "bad-protx-shares-collateral-script";
  else ExtractDestination (P2PK/P2PKH/P2SH) else "bad-protx-collateral-dest". Collateral outpoint = (txid, n).
* Non-shared: `IsPayoutListKeySafe` (providertx.cpp:146-164): collateral dest == PKHash(owner) or PKHash(voting)
  "bad-protx-collateral-reuse"; v3 only: any payout dest == collateral dest "bad-protx-payee-reuse".
* List at pprev: every netInfo entry already held by an MN with a *different collateral* → "bad-protx-dup-netinfo-entry"
  (address reuse allowed only when replacing the same collateral); owner key / share keys / operator key held by anyone →
  "bad-protx-dup-key" (even when replacing); V24: operator key held under the other BLS encoding → "bad-protx-dup-key";
  Evo platformNodeID held → "bad-protx-dup-platformnodeid"; pre-enforcement (pprev height < 1047200) owner≠voting →
  "bad-protx-key-not-same".
* inputsHash. Signature: external collateral → if check_sigs, `CMessageSigner::VerifyMessage(collateralKeyID, vchSig,
  MakeSignString())` (message magic "DarkCoin Signed Message:\n", src/util/message.cpp:25; messagesigner.cpp:39-46;
  compact-sig recovery, header byte `(h-27)&3` recid, `&4` compressed, no low-S rule, pubkey.cpp:199-217) else
  "bad-protx-sig". Internal collateral → vchSig must be empty else "bad-protx-sig"; shared → every joinSig verified with
  `VerifyHashCanonical` (65 bytes, header 27..34, low-S; messagesigner.cpp:58-90) over MakeSharedRegConsentHash
  (providertx.cpp:338-373) else "bad-protx-shares-sig".
* **MakeSignString** (providertx.cpp:375-395) = `<payout>|<nOperatorReward decimal>|<base58 owner addr>|<base58 voting
  addr>|<SerializeHash(payload w/o sig).ToString() (reversed hex)>`; payout = v3: `addr:reward,…` list, else base58
  address of scriptPayout or hex if not extractable. **Network-dependent (address prefixes).** Identical in v18/v19.
  SerializeHash is over the *re-serialized parsed object* (so MnNetInfo/ExtNetInfo normalization feeds into it).

### 2.6 CheckProUpServTx (1316-1419)
GetValidatedPayload; CheckProviderNetworkFields(allow_empty=false); MN must exist at pprev "bad-protx-hash";
`nType == dmn->nType` "bad-protx-type-mismatch"; V24: state v≥2 and tx v1 "bad-protx-version-downgrade", Evo with v<3
"bad-protx-version-disallowed", dry-run of version migration (ApplyProUpServTx) and cross-scheme key collision; each netInfo
entry held by another MN "bad-protx-dup-netinfo-entry"; Evo platformNodeID held by another "bad-protx-dup-platformnodeid";
scriptOperatorPayout non-empty requires `dmn->nOperatorReward != 0` and P2PKH/P2SH else "bad-protx-operator-payee";
inputsHash; BLS sig with current operator key (null key ⇒ always fails) "bad-protx-sig".

### 2.7 CheckProUpRegTx (1421-1518)
MN exists "bad-protx-hash"; shared MN "bad-protx-shared-mn"; V24 downgrade/collision/migration checks; payout list valid
with the MN's owner key; collateral coin must exist in pprev UTXO "bad-protx-collateral" (TX_CONSENSUS); ExtractDestination
"bad-protx-collateral-dest"; IsPayoutListKeySafe (collateral==owner/voting "bad-protx-collateral-reuse"; payout==collateral
only if max(state,tx) version ≥ 3); new operator key held by another MN (same encoding) "bad-protx-dup-key";
pre-enforcement owner ≠ new voting "bad-protx-key-not-same"; inputsHash; `CHashSigner::VerifyHash(SerializeHash(ptx),
ownerKeyID, vchSig)` (plain compact sig over the hash, no magic) "bad-protx-sig".

### 2.8 CheckProUpRevTx (1520-1551)
MN exists; V24 downgrade; inputsHash; BLS sig with current operator key "bad-protx-sig".

### 2.9 V24 shared-masternode txs (pending on mainnet; XL)
* Share list rules `IsShareListTriviallyValid` (providertx.cpp:63-130): 2..8 shares; earlyPeriod ≤ 420480; amount
  100 DASH..collateral; non-null, unique owner keys; unique refund scripts; refund/effective reward scripts P2PKH/P2SH,
  not paying voting key or any share owner key; Σamount == collateral; 0 ≤ penalty < min share; penalty==0 if period==0.
* ProDisTx (1666-1803): MN exists at pprev (same-block register+dissolve invalid), must be shared; exactly one input =
  collateral with empty scriptSig; actor < shares; sig count 1 (unilateral) or shares.size() (unanimous); outputs = refund
  of each non-actor share in order (+ optional non-zero actor output); bonus ≥ ProRataFloor(requiredPenalty, …)
  (128-bit floor, providertx_util.cpp:88-95); Σbonus ≥ penalty; fee ≤ 1,000,000 duffs; unilateral Σbonus ≤ earlyPenalty;
  sigs over `MakeSignHash` (providertx.cpp:506-533, prefix "DashSharedMNDissolve"). ProUpShareTx (1553-1607),
  ProUpSharedRegTx (1609-1664) similar. Covenant: template script `04 'DSHC' OP_DROP OP_TRUE` (sharedcollateral.h:116-133)
  may only be created as a shared ProRegTx's collateral output "bad-shared-collateral-create" (1823-1839) and spent only by
  a ProDisTx "bad-shared-collateral-spend" (1805-1821; also in BuildNewList 740-744).

### 2.10 What changed at V19 / V20 / MN_RR for ProTxs
* V19 (block ≥ 1899072): ProTx v2 (BasicBLS encoding) accepted; Evo (HPMN) registrations (v2 only, 4000 DASH, platform
  fields); ProUpServTx v2 carries nType; global BLS scheme flips to basic (affects verification of *all* ProTx BLS sigs);
  SML entries of v2 states gain nType/Evo fields; EvoNodes paid 4 consecutive blocks.
* MN_RR (block ≥ 2128896): EvoNodes paid once per cycle (§3.6).
* V24 (pending): ExtAddr v3, multi-payout, shared MNs, v1 registrations banned, no downgrade, state version can be raised by
  ProUpServ/ProUpReg/ProUpRev, cross-scheme operator-key uniqueness.

---------------------------------------------------------------------------------------------------------------------

## 3. Deterministic MN list (difficulty: XL — highest consensus risk)

### 3.1 Data model
* `CDeterministicMN` (deterministicmns.h:43-94): proTxHash, internalId (local, non-consensus), collateralOutpoint,
  nOperatorReward (immutable after registration), nType, pdmnState.
* `CDeterministicMNState` (dmnstate.h:33-175): nVersion(int), nRegisteredHeight(-1), nLastPaidHeight(0),
  nConsecutivePayments(0), nPoSePenalty(0), nPoSeRevivedHeight(-1), nPoSeBanHeight(-1, private; IsBanned ⇔ ≠ -1),
  nRevocationReason, confirmedHash, confirmedHashWithProRegTxHash = **single SHA256**(proTxHash‖confirmedHash) (162-169),
  keyIDOwner, pubKeyOperator, keyIDVoting, netInfo, scriptPayout / payouts / shares / early fields (v3),
  scriptOperatorPayout, platformNodeID, platformP2PPort, platformHTTPPort. Constructed from ProRegTx (74-89).
* `ResetOperatorFields` (127-135): nVersion=1(!), pubKeyOperator=null, netInfo=empty MnNetInfo, scriptOperatorPayout=∅,
  nRevocationReason=0, platformNodeID=0 (ports NOT reset). Same in v22. `BanIfNotBanned(h)`, `Revive(h)` (penalty 0,
  ban -1, revivedHeight=h) (136-155).
* Unique-property map (deterministicmns.h:440-614): **one map keyed by `SerializeHash(value)`** with refcount
  (proTxHash, count). Inserted: collateralOutpoint, each netInfo CService (addrv1 bytes unless not v1-compatible) /
  DomainPort, keyIDOwner (or every share owner key), pubKeyOperator (**hash of its current encoding** — legacy vs basic bytes
  are different slots), Evo platformNodeID. Default/null values are never inserted. **Voting keys are not unique.**
  Because CKeyID and uint160 both serialize to 20 raw bytes, **an Evo platformNodeID collides with any owner keyID of equal
  value** (cross-namespace "bad-protx-dup-platformnodeid"/"bad-protx-dup-key").
* Add/Update/Remove throw on conflicts (439-679); BLS key re-key compares serialized hashes (597-600).

### 3.2 List at height h
`GetListForBlock(pindex)` = list *after* connecting pindex (blockHash = pindex hash). Empty default list for any block
below DIP0003Height (deterministicmns.cpp:830-832). Consumers: ProTx checks use list(pindexPrev); the payee of block h is
`list(h-1).GetMNPayee(pindexPrev=h-1)` (src/masternode/payments.cpp:152-157 and specialtxman.cpp:326); CbTx
merkleRootMNList of block h commits to list(h); quorum membership uses list(quorum base block) (LLMQ agent).

### 3.3 RebuildListFromBlock — exact order (specialtxman.cpp:310-796)
Let nHeight = pindexPrev.height+1, newList = copy(prevList) with height nHeight.
0. `payee = prevList.GetMNPayee(pindexPrev)` (326).
1. **confirmedHash**: for every MN in *prevList* (banned included) with null confirmedHash and
   `pindexPrev.height − nRegisteredHeight ≥ nMasternodeMinimumConfirmations (15 mainnet)` → confirmedHash =
   hash(pindexPrev) (331-344). ⇒ MN registered at R gets confirmedHash = hash(R+15), visible in list(R+16).
2. **DecreaseScores**: every non-banned MN with penalty > 0: penalty −= 1 (346; deterministicmns.cpp:309-334).
3. **Special txs in block order (skip coinbase)**, single loop (351-730), non-special-version txs skipped:
   * ProRegTx (359-454): parse; internalId = nTotalRegisteredCount; proTxHash = txid; collateral = (txid,n) or external;
     (only when view has a best block) re-check external collateral coin & amount "bad-protx-collateral"; if an MN with the
     same collateral exists → **remove it** (replacement; new MN goes to the back of the payment queue); for each netInfo
     entry already in newList → "bad-protx-dup-netinfo-entry" (non-addr entry → "bad-protx-netinfo-entry"); owner/share
     keys or operator key present → "bad-protx-dup-key"; state = from proTx, nRegisteredHeight = nHeight;
     **empty netInfo → BanIfNotBanned(nHeight)**; V24 cross-scheme operator key → "bad-protx-dup-key"; AddMN
     (throws on platformNodeID dup → block invalid "failed-procspectxsinblock").
   * ProUpServTx (455-522): parse; invalid nType "bad-protx-type"; netInfo dup vs *newList* (other MN) "bad-protx-dup-netinfo-entry";
     MN must exist in newList "bad-protx-hash"; type mismatch; `ApplyProUpServTx` (110-137): netInfo, scriptOperatorPayout,
     Evo: platformNodeID and (v<3) ports — **pre-V24 the state nVersion never changes**; if banned and operator key non-null
     and voting non-null and (owner non-null or shared) → `Revive(nHeight)` (any ban reason). UpdateMN.
   * ProUpRegTx (523-587): MN exists; shared → "bad-protx-shared-mn"; `operator_changed = state.key != tx.key`
     (**point comparison, ignores encoding**); target version: pre-V24 = changed ? tx.nVersion : old; if changed →
     ResetOperatorFields + BanIfNotBanned + set new key; keyIDVoting = tx; `SetStateVersion(target)` (51-106: re-encodes the
     key per version, converts netInfo kind) ; payout = scriptPayout (v<3) or payouts. Equals v22 behaviour
     (`newState->nVersion = proTx.nVersion` only when the key changes).
   * ProUpRevTx (588-619): ResetOperatorFields (→ nVersion 1 pre-V24), BanIfNotBanned, nRevocationReason = tx reason.
   * V24: ProUpShareTx (620-649), ProUpSharedRegTx (650-702).
   * Quorum commitment (703-728): non-null commitment → quorum base = ancestor at `nHeight - nHeight%dkgInterval + quorumIndex`
     must equal quorumHash "bad-qc-quorum-hash"; members = GetAllQuorumMembers(type, base) (LLMQ agent);
     `HandleQuorumCommitment` (284-299): for i, if member still in list and `!validMembers[i]` →
     `PoSePunish(member, CalcPenalty(66))`. **Applies to every LLMQ type.**
4. **Collateral spends** (second pass over all non-coinbase txs, 733-758): any input equal to an MN's collateral removes the
   MN (V24: shared collateral only by ProDisTx). Runs after step 3, so an MN registered *and* spent in the same block is
   added then removed; a ProUpServTx for an MN whose collateral is spent in the same block is applied then removed.
5. **Payee bookkeeping** (762-776): if payee still in newList: nLastPaidHeight = nHeight (even if just banned/revoked);
   if Evo and MN_RR not active after pindexPrev: ++nConsecutivePayments.
6. **Reset streaks** (779-791): every Evo MN with nConsecutivePayments ≠ 0 that is not (the payee while pre-MN_RR) → 0.
Identical order since v0.13 (`git show v0.13.3.0:src/evo/deterministicmns.cpp`). The block-wide in-list re-checks exist
because per-tx checks only saw pindexPrev.

### 3.4 PoSe math (deterministicmns.cpp:267-334)
`maxPenalty = max(100, total MN count (regular+evo, unweighted, banned included))`; `CalcPenalty(p) = maxPenalty*p/100`
(integer); `PoSePunish`: penalty += x, clamp to max (**added even when already banned**); if not banned and penalty ≥ max →
banHeight = list height. Decrease: −1/block for non-banned MNs. Ban only lifted by `Revive` via ProUpServTx (penalty→0).
The max is evaluated on the list *as rebuilt so far* (registrations earlier in the same block count).

### 3.5 Ban/revive/key-reset state transitions
* Registered with empty netInfo → banned at registration height.
* ProUpRegTx changing the operator key → reset operator fields (address cleared, platformNodeID cleared), banned
  ("need ProUpServTx"), revived by the next ProUpServTx signed by the new key.
* ProUpRevTx → key null + banned; no ProUpServTx can ever validate (sig against null key fails), needs ProUpRegTx first.
* PoSe ban → any valid ProUpServTx revives.

### 3.6 Payee selection (deterministicmns.cpp:132-194)
Sort key `CompareByLastPaid_GetHeight` = revivedHeight if (≠-1 and > lastPaid), else lastPaid, else (if lastPaid==0)
registeredHeight; ties by proTxHash (uint256 memcmp = internal little-endian byte order). Payee = minimum among
**non-banned** MNs. Between V19 and MN_RR: if a non-banned Evo MN has `nLastPaidHeight == list height` and
`nConsecutivePayments < 4`, it is paid again. Projections (GetProjectedMNPayees, 196-249) are RPC only.

### 3.7 Storage (non-consensus)
Diffs per block (`dmn_D4`), full snapshots every 576 blocks (`dmn_S3`), legacy diff format migration
(deterministicmns.cpp:31-34, 716-732; dmnstate.h:177-483). Internal IDs only used by storage/RPC/governance. A Rust node may
use any storage.

---------------------------------------------------------------------------------------------------------------------

## 4. CbTx (difficulty: M for v1-v3 parsing/height; L with SML; quorum root & CL depend on LLMQ/CL agents)

### 4.1 Payload (src/evo/cbtx.h:32-70)
nVersion u16 (enum; INVALID 0, MERKLE_ROOT_MNLIST 1, MERKLE_ROOT_QUORUMS 2, CLSIG_AND_BALANCE 3, MERKLE_ROOT_ASSETUNLOCKS 4)
| nHeight i32 | merkleRootMNList | v≥2 merkleRootQuorums | v≥3 bestCLHeightDiff (CompactSize, canonical, ≤ 0x02000000 —
serialize.h:292-322, 715-725) | bestCLSignature 96 (unwrapped → global scheme) | creditPoolBalance i64 | v≥4 merkleRootAssetUnlocks.

### 4.2 CheckCbTx (cbtx.cpp:24-55)
version 0 or ≥5 "bad-cbtx-version"; `nHeight == pindexPrev.height+1` "bad-cbtx-height"; **v≥2 required once
`pindexPrev.height ≥ DIP0008Height`** (i.e. block ≥ 1088641; identical in v18) "bad-cbtx-version"; v≥3 required iff V20
active for this block (block ≥ 1987776), and v≥3 forbidden before; v≥4 required iff V24 && V20 (forbidden otherwise).

### 4.3 merkleRootMNList (simplifiedmns.h:27-108, simplifiedmns.cpp:45-146)
* SML = every MN in list(h) (banned included) → `CSimplifiedMNListEntry` (deterministicmns.cpp:43-50), sorted by proRegTxHash
  memcmp order (simplifiedmns.cpp:73-79), leaf = `CalcHash()` = SHA256d over **SER_GETHASH** serialization:
  `proRegTxHash 32 | confirmedHash 32 | netInfo (v<3: 18-byte legacy CService, empty ⇒ zeros; v3: ExtNetInfo w/ ADDRv2) |
  pubKeyOperator 48 (legacy encoding iff state nVersion==1; null ⇒ 48 zeros) | keyIDVoting 20 | isValid u8 (=!banned) |
  if nVersion≥2: nType u16 | if Evo: (v<3: platformHTTPPort u16 LE) platformNodeID 20`. The leading `nVersion` field is
  written only for SER_NETWORK ≥ 70228 (wire), never in the hash. A v1 entry is 151 bytes (same as pre-V19 entries).
* Root = Bitcoin `ComputeMerkleRoot` (duplicate-last; empty list ⇒ uint256 zero; consensus/merkle.cpp); mutation detected
  → "mutated-calc-cb-mnmerkleroot" (block invalid). Mismatch "bad-cbtx-mnmerkleroot".
* v19 had identical field order (`git show v19.3.0:src/evo/simplifiedmns.h`); v18 = v1 layout.

### 4.4 merkleRootQuorums (cbtx.cpp:65-156; llmq/blockprocessor.cpp:470-517, 687-831)
* Only when CbTx.nVersion ≥ 2. For every LLMQ type configured for the network (`Params().GetConsensus().llmqs`):
  non-rotated → the `signingActiveQuorumCount` most recently *mined* commitments as of pindexPrev; rotated (DIP24, if
  `IsQuorumRotationEnabled(params, pindexPrev)` = DIP0024 active at block `cycleBase−1`, llmq/options.cpp:50-62) → the
  last mined commitment per quorumIndex. Leaf = `SerializeHash(CFinalCommitment)`.
* Then add the block's own non-null commitments: rotated → replace by quorumIndex; non-rotated → if already at
  signingActiveQuorumCount pop the oldest, push new. More than signingActiveQuorumCount for a type →
  "excess-quorums-calc-cbtx-quorummerkleroot". Payload/type errors → "bad-qc-payload-calc-cbtx-quorummerkleroot",
  "bad-qc-commitment-type-calc-cbtx-quorummerkleroot"; missing stored commitment → "commitment-not-found".
* All hashes sorted (memcmp) → ComputeMerkleRoot; mutation → "mutated-calc-cbtx-quorummerkleroot".

### 4.5 bestCL fields — CheckCbTxBestChainlock (specialtxman.cpp:139-215)
Only v≥3. Let prev = previous block's CbTx CL (non-null, i.e. V20-active, v≥3, valid sig, height match, diff < height;
cbtx.cpp:181-211). If prev non-null: current sig must be valid "bad-cbtx-null-clsig" and `diff ≤ prevDiff+1` (CL height
must not go backwards) "bad-cbtx-older-clsig". If current sig valid: `diff < h` "bad-cbtx-cldiff"; CL height = h−diff−1;
verify `ChainLockSig(height, ancestor hash, sig)` with `VerifyChainLock` (llmqTypeChainLocks, requestId
GenSigRequestId(height); src/chainlock/clsig.cpp:253-261) "bad-cbtx-invalid-clsig". Null sig ⇒ diff must be 0
"bad-cbtx-cldiff". Fast paths against the node's best known CL are caches only. The `diff >= height` guard is new
(c8923498ae, v24-rc); older code dereferenced null (crash) — no valid chain is affected.

### 4.6 creditPoolBalance — checked in ProcessSpecialTxsInBlock step 4 (specialtxman.cpp:1038-1069), see §6.

### 4.7 SPV-facing P2P (difficulty: M)
* `getmnlistd` = {baseBlockHash, blockHash} → `mnlistdiff` (smldiff.h:35-79; net_processing.cpp:5607-5618):
  [nVersion u16 if peer ≥ 70229] base, block, CPartialMerkleTree(coinbase only), coinbase tx, [nVersion if 70225 ≤ peer
  < 70229], deletedMNs (proTxHashes), mnList (SML entries, wire form), deletedQuorums (u8 type, hash), newQuorums
  (CFinalCommitment), [peer ≥ 70230: map<CBLSSignature, set<u16>> quorumsCLSigs]. Built in smldiff.cpp (BuildSimplifiedDiff,
  BuildQuorumsDiff, BuildQuorumChainlockInfo: CL of the coinbase at quorum base − quorumIndex − 8).
* `getqrinfo`/`qrinfo` live in src/llmq/snapshot.h:102-113 (LLMQ agent).
* BIP158 basic filters in Dash include special-tx elements (src/blockfilter.cpp:201; evo/specialtx_filter.cpp) — affects
  cfilter/cfheaders bytes; P2P-compat, not consensus.

---------------------------------------------------------------------------------------------------------------------

## 5. MNHF signal (type 7) (difficulty: M, mostly glue to LLMQ + versionbits)

* Payload (mnhftx.h:37-93): nVersion u8 (=1) | versionBit u8 | quorumHash 32 | sig 96 (**basic encoding forced**).
* `IsTriviallyValid` (mnhftx.cpp:94-104): version 1 else "bad-mnhf-version"; bit < 29 else "bad-mnhf-nbit-out-of-bounds".
* `CheckMNHFTxImpl` (106-157): type "bad-mnhf-type"; payload "bad-mnhf-payload"; `IsValidMNActivation(bit,
  pindexPrev MTP)` else "bad-mnhf-non-ehf" — **returns true for unknown bits and for deployments outside their time window**,
  false only if a matching in-window deployment has useEHF=false (chainparams.cpp:109-130); quorumHash block must be on
  this chain "bad-mnhf-quorum-hash"; msgHash = txid of the tx with sig zeroed (`tx_copy.GetHash()`); quorum =
  GetQuorum(llmqTypeMnhf, quorumHash) — **no recency requirement** — else "bad-mnhf-missing-quorum"; requestId =
  SerializeHash(("mnhf" as compact-string, int64 versionBit)); verify `SignHash(llmqType, quorumHash, requestId, msgHash)`
  with global-scheme VerifyInsecure "bad-mnhf-invalid".
* Block processing `CMNHFManager::ProcessBlock` (190-262): two signals for the same bit in a block
  "bad-mnhf-duplicates-in-block"; bit already in active signals of pprev "bad-mnhf-duplicate"; `IsValidMNActivation(bit,
  pindex MTP)` (note: *current* block MTP) "bad-mnhf-non-mn-fork"; exceptions "failed-proc-mnhf-inblock". Signals map
  {bit → mined height} carried forward per block.
* `GetSignalsStage(pindexPrev)` (61-92): empty unless V20 active; drops a signal whose mined block's MTP < the start time
  of a deployment using that bit (bit reuse expiry). versionbits: for useEHF deployments `SignalHeight` = signal height (or
  INT_MAX) and DEFINED→STARTED requires `pindexPrev.height ≥ signalHeight` in addition to MTP ≥ start (versionbits.cpp:38,
  69-91, 238-251); threshold decays `max(min, start − attempt²·period/100/falloff)` (258-266).
* Undo: no state change (264-283). History: v20 required V20 for the tx type; v22 dropped it. Mainnet carried real EHF
  signals for MN_RR and WITHDRAWALS (both now buried), whose bits are no longer in vDeployments → `IsValidMNActivation`
  returns true ("unknown bit"), so replay works.

---------------------------------------------------------------------------------------------------------------------

## 6. AssetLock (8) / AssetUnlock (9) (difficulty: M for validity; credit pool math elsewhere)

* AssetLock payload (assetlocktx.h:32-73): nVersion u8 | vector<CTxOut> creditOutputs. Rules `CheckAssetLockTx`
  (assetlocktx.cpp:29-89): every output whose script starts with OP_RETURN must be exactly `6a 00` ("bad-assetlocktx-non-empty-return"),
  value in (0, MAX_MONEY] ("…-opreturn-outofrange"), only one ("…-multiple-return"); none ⇒ "…-no-return"; payload parse
  "…-payload"; version 1..2 ("…-version"), v2 only with V24 ("…-version-2"); creditOutputs non-empty ("…-emptycreditoutputs");
  each credit output value in (0, MAX_MONEY] and cumulative in range ("…-credit-outofrange"); v1: P2PKH only
  ("…-pubKeyHash"), v2: P2PKH or P2SH ("…-script-pubkey"); Σcredit == OP_RETURN amount ("…-creditamount"). The tx still
  pays normal fees (inputs ≥ outputs incl. the OP_RETURN value). No minimum amount.
* AssetUnlock payload (assetlocktx.h:75-168): nVersion u8 | index u64 | fee u32 | requestedHeight u32 | quorumHash 32 |
  quorumSig 96 (global scheme) = 145 bytes. `CheckAssetUnlockTxImpl` (176-232): no inputs ("bad-assetunlocktx-have-input");
  ≤ 32 outputs ("…-too-many-outs"); payload ("…-payload"); version 1..2 / v2 needs V24; index not in pool indexes at pprev
  ("bad-assetunlock-duplicated-index"); quorumHash must be a known block ("bad-assetunlock-quorum-hash", note: *any*
  known block index, chain-membership implied by the scan below); msgHash = SerializeHash(full tx with quorumSig zeroed)
  (v1: equals old `tx_copy.GetHash()`).
* `VerifyAssetUnlockSig` (106-153), with pindexTip = **pindexPrev**: quorum must be among `ScanQuorums(llmqTypePlatform,
  pindexPrev, signingActiveQuorumCount+1 = 25)` ("bad-assetunlock-too-old-quorum"); window `requestedHeight ≤ pindexPrev.height
  < requestedHeight+48` ⇒ block height ∈ [requestedHeight+1, requestedHeight+48] ("bad-assetunlock-too-late"); requestId =
  SerializeHash(("plwdtx", u64 index)); `SignHash(llmqTypePlatform, quorumHash, requestId, msgHash)` verify
  "bad-assetunlock-not-verified". Both window failures are TX_BAD_SPECIAL now (were TX_CONSENSUS ≤ v23).
  **Historical:** before WITHDRAWALS (v22: `DeploymentActiveAt(*pindexTip, WITHDRAWALS)` ⇒ blocks ≥ 2201473) only **2**
  quorums were scanned (`git show v22.1.3:src/evo/assetlocktx.cpp`); develop removed that branch (75de5766cb) — the new rule is
  a superset, so history still validates.
* Credit pool interaction (creditpool.cpp:279-405, owned by credit-pool agent): per block, in tx order, Lock adds the
  OP_RETURN value; Unlock amount = fee + Σvout (each MoneyRange, "failed-creditpool-unlock-txout-outofrange"), must not
  exceed `pool.currentLimit` cumulatively ("failed-creditpool-unlock-too-much"), index unique vs pool and earlier txs of the
  block ("failed-creditpool-unlock-duplicated-index"). Enforced only through `CheckCreditPoolDiffForBlock`
  (specialtxman.cpp:1038-1069) when a CbTx exists, DIP0008 and V20 active at pindex; result must equal
  `cbTx.creditPoolBalance` ("bad-cbtx-assetlocked-amount"). Mempool: index already mined → "txn-already-known".

---------------------------------------------------------------------------------------------------------------------

## 7. Other evo code
* `src/evo/snapshot.*` — assumeutxo-style "evo snapshot v3" codec (marker "DASHEVO\0", bounds 100k MNs etc.,
  snapshot.h:15-40, 501-741). Needed only for snapshot sync interop; not consensus. (M if interop desired.)
* `src/evo/mnauth.*` — P2P MNAUTH handshake (masternode proves operator key on connection). **P2P only.**
* `src/evo/providertx_service.*` — wallet/RPC transaction builders. Non-consensus.
* `src/evo/evodb.*` — storage; `core_write.cpp` — JSON. Non-consensus.
* `src/evo/specialtx_filter.cpp` — BIP37/BIP158 element extraction (see 4.7).

---------------------------------------------------------------------------------------------------------------------

## 8. Historical timeline (mainnet) and replay notes

| Height | Rule change |
|---|---|
| < 1028160 | No special-tx validation beyond: nVersion≥3 && nType≠0 txs are *invalid* ("bad-tx-type-dip3-inactive"); nVersion<3 txs with nType≠0 bytes are valid and treated as normal (but CheckTransaction still lets nType 6/7 have empty vin/vout, nType 9 empty vin). |
| 1028160 (DIP3) | Special txs 1-6 (and today 7-9 too); CbTx v1 mandatory; coinbase must be CbTx; BIP34 replaced by CbTx.nHeight; MN list starts empty; merkleRootMNList checked. |
| ≤ 1047200 | ProRegTx owner==voting, ProUpRegTx new voting==owner ("bad-protx-key-not-same"). |
| 1047200 | Block hash checkpoint "bad-dip3-enf-block". MN payments enforced (payments agent). |
| 1088641 | CbTx v2 mandatory (DIP0008 off-by-one) → merkleRootQuorums. |
| 1737792/1738698 | DIP24 rotated quorums → quorum merkle root uses per-index commitments (when cycle base−1 ≥ DIP0024Height). |
| 1899072 (V19) | ProTx v2 / BasicBLS, Evo nodes, global BLS scheme basic, SML v2 fields, Evo 4-block streaks. |
| 1987776 (V20) | CbTx v3 (bestCL + creditPoolBalance), credit pool, MNHF signals effective; (≤v21: types 7/8 only from here). |
| 2128896 (MN_RR) | Evo streaks end; (≤v21: asset unlocks only from here). |
| 2201473 | (historical) asset unlock quorum scan 2 → 25. |
| V24 (pending) | §2.9, ExtAddr, CbTx v4, unlock v2 stable txid, asset lock v2. |

Replay-relevant relaxations in develop vs. the software that produced history (all supersets, so history replays):
removed V20/MN_RR gates for types 7/8/9; unlock quorum scan; invalid-CService-as-empty; reason strings and
TX_BAD_SPECIAL vs TX_CONSENSUS classifications changed many times (only peer-punishment, not validity).
Stricter in develop than history: nVersion≥4 special txs validated (verify none exist); bestCLHeightDiff range.

---------------------------------------------------------------------------------------------------------------------

## 9. rust-dashcore reuse assessment (HEAD 67b307c6)

* `dash/src/blockdata/transaction/mod.rs:603-673` **tx decoding is not Core-compatible for a full node**: treats
  `version != 0` (u16) with nType≠0 as special (Core: nVersion ≥ 3); unknown types ≥10 are a decode error
  (special_transaction/mod.rs:365-382); payload length prefix is read and ignored (`_len`, mod.rs:417-420) and the payload is
  parsed straight from the stream — no "consume exactly len bytes" rule, no raw-bytes preservation; txid is recomputed from
  re-encoded structs (transaction/mod.rs:196-220) so any non-round-tripping payload breaks the txid; BIP144 segwit
  detection on empty vin (Dash has no segwit) except for types 5/6/7/9; no V24 stable unlock txid. Needed: raw `vExtraPayload`
  bytes + lazy typed parsing exactly like `GetTxPayload`.
* Payload structs: ProRegTx v1/v2 only (no ExtAddr, payouts, shares), mn type decode errors on values >1, uses
  `SocketAddr` (round-trips 16 bytes losslessly, sml/address.rs:7-49) (provider_registration.rs); ProUpServTx v1/v2 with
  correct field order (provider_update_service.rs:133-225), rejects v3; ProUpRegTx/ProUpRevTx v1/v2 raw BLS bytes;
  CbTx v1-v3 (no v4 merkleRootAssetUnlocks) (coinbase.rs); AssetLock any version; AssetUnlock v1 layout; MNHF ok;
  no types 10-12. Layouts look byte-exact for the versions covered; usable as reference/test vectors.
* SML: `sml/masternode_list_entry/mod.rs:157-201` encodes the hash body exactly as Core (incl. v3 ExtNetInfo, v<3 HTTP port)
  and a test checks against Core CalcHash (hash.rs:20-60). ExtNetInfo codec is strict (Vec, errors on bad purpose/type) —
  differs from Core's std::map/normalizing decode (net_info.rs:191-250); fine for states produced by validated txs.
  Merkle root (`merkle_roots.rs:9-30`) returns None for an empty list (Core: zero hash) and has no mutation detection.
  Quorum root sorts entry hashes (ok) but only over what the SPV list holds. **There is no DMN state machine** (no PoSe,
  confirmedHash, payee, unique-property map, BuildNewList) anywhere in the workspace.
* BLS (`crypto/src/bls.rs:57-235`): scheme enum ties hash-to-curve to the encoding scheme (`BlsPublicKey::verify` uses the
  key's own scheme). Core verifies post-V19 v1-encoded ProTx sigs with the *basic* hash-to-curve — needs decoupling
  (decode with encoding scheme, `reencode`, verify with height scheme).
* Reusable: mnlistdiff/qrinfo messages (network/message_sml.rs, message_qrinfo.rs), SML entry hashing, quorum entry
  hashing, BLS primitives.

---------------------------------------------------------------------------------------------------------------------

## 10. Difficulty summary
§1 framework M · §2 ProTx validation L (XL incl. V24 shared MNs + ExtNetInfo/domain rules) · §3 DMN list XL ·
§4 CbTx M-L (+ dependency on LLMQ commitment storage and chainlock verification) · §5 MNHF M · §6 asset lock/unlock M ·
§7 snapshot interop M (optional) · rust-dashcore deltas: tx codec rewrite M, DMN engine from scratch XL.
