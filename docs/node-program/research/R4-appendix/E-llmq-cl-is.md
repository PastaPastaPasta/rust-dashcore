> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# E — LLMQ, BLS, ChainLocks, InstantSend, EHF (consensus effects + masternode protocols)

Scope: Dash Core `develop` @ `15db478750` (/work/home/agent/workspace/dash), rust-dashcore @ `67b307c6`.
All citations are `path:line` in the Dash Core tree unless prefixed `rd:` (rust-dashcore). Facts were verified by reading the code and `git log`/`git show`.
Difficulty legend for a Rust port: S (days), M (1–3 wk), L (1–2 mo), XL (>2 mo / research-grade bit-exactness work).

Conventions used below:
- `SerializeHash(x)` = SHA256d of Dash serialization (`src/hash.h:262-267`; `CHashWriter::GetHash` is double-SHA256, `src/hash.h:113-120`).
- `uint256 <` is **memcmp of the raw 32 bytes, byte 0 first** (`src/uint256.h:55,59`), i.e. *not* numeric. `arith_uint256` comparisons are numeric (LE interpretation).
- `DeploymentActiveAfter(pindexPrev, dep)` ⇔ `pindexPrev->nHeight + 1 >= H(dep)`; `DeploymentActiveAt(index, dep)` ⇔ `index.nHeight >= H(dep)` (`src/deploymentstatus.h:14-44`).
- `std::string`/`std::string_view` serialize as compactsize-length + bytes (`src/serialize.h:1058-1064`).

---------------------------------------------------------------------------------------------------

## 1. LLMQ parameters and per-network assignments  — difficulty **S**

### 1.1 `LLMQType` enum (`src/llmq/params.h:19-46`)
| value | name | notes |
|---|---|---|
| 0xff | LLMQ_NONE | |
| 1 | LLMQ_50_60 | mainnet/testnet (old IS + testnet CL/MNHF) |
| 2 | LLMQ_400_60 | mainnet ChainLocks |
| 3 | LLMQ_400_85 | mainnet MNHF (EHF) |
| 4 | LLMQ_100_67 | mainnet Platform |
| 5 | LLMQ_60_75 | rotated (DIP0024) — InstantSend |
| 6 | LLMQ_25_67 | testnet Platform |
| 100 | LLMQ_TEST | regtest |
| 101 | LLMQ_DEVNET | |
| 102 | LLMQ_TEST_V17 | regtest, gated on TESTDUMMY BIP9 |
| 103 | LLMQ_TEST_DIP0024 | regtest rotated |
| 104 | LLMQ_TEST_INSTANTSEND | regtest |
| 105 | LLMQ_DEVNET_DIP0024 | devnet rotated |
| 106 | LLMQ_TEST_PLATFORM | regtest |
| 107 | LLMQ_DEVNET_PLATFORM | devnet |
`MAX_LLMQ_SIZE = 400` (`params.h:17`). Serialized as one byte (`is_serializable_enum`, `params.h:517-518`).

### 1.2 Parameters (`src/llmq/params.h:138-511`, `available_llmqs`)
| type | rot | size | minSize | thr | dkgInterval | phaseBlocks | mineStart | mineEnd | badVotes | activeCount | keepOldConn | keepOldKeys | recoveryMembers | line |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| LLMQ_TEST | n | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 3 | 4 | 3 | 145-164 |
| LLMQ_TEST_INSTANTSEND | n | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 3 | 4 | 3 | 171-190 |
| LLMQ_TEST_V17 | n | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 3 | 4 | 3 | 197-216 |
| LLMQ_TEST_DIP0024 | **y** | 4 | 4 | 3 | 24 | 2 | 12 | 20 | 2 | 2 | 4 | 4 | 3 | 223-242 |
| LLMQ_TEST_PLATFORM | n | 3 | 2 | 2 | 24 | 2 | 10 | 18 | 2 | 2 | 4 | 1440 | 3 | 249-268 |
| LLMQ_DEVNET | n | 12 | 7 | 6 | 24 | 2 | 10 | 18 | 7 | 4 | 5 | 8 | 6 | 275-294 |
| LLMQ_DEVNET_DIP0024 | **y** | 8 | 6 | 4 | 48 | 2 | 12 | 20 | 7 | 2 | 4 | 4 | 4 | 301-320 |
| LLMQ_DEVNET_PLATFORM | n | 12 | 9 | 8 | 24 | 2 | 10 | 18 | 7 | 4 | 5 | 1440 | 6 | 327-346 |
| **LLMQ_50_60** | n | 50 | 40 | 30 | 24 | 2 | 10 | 18 | 40 | 24 | 25 | 48 | 25 | 354-372 |
| **LLMQ_60_75** | **y** | 60 | 50 | 45 | 288 | 2 | 42 | 50 | 48 | 32 | 64 | 64 | 25 | 380-398 |
| **LLMQ_400_60** | n | 400 | 300 | 240 | 288 | 4 | 20 | 28 | 300 | 4 | 5 | 8 | 100 | 406-425 |
| **LLMQ_400_85** | n | 400 | 350 | 340 | 576 | 4 | 20 | 48 | 300 | 4 | 5 | 8 | 100 | 434-453 |
| **LLMQ_100_67** | n | 100 | 80 | 67 | 24 | 2 | 10 | 18 | 80 | 24 | 25 | 1440 | 50 | 462-481 |
| **LLMQ_25_67** | n | 25 | 22 | 17 | 24 | 2 | 10 | 18 | 22 | 24 | 25 | 1440 | 12 | 490-509 |

Consensus-critical fields: `type, useRotation, size, minSize, dkgInterval, dkgMiningWindowStart/End, signingActiveQuorumCount` (merkleRootQuorums + quorum selection), `threshold` (DKG/recovery; verified only indirectly). `dkgPhaseBlocks, dkgBadVotesThreshold, keepOld*, recoveryMembers` are masternode-protocol params. Helpers: `max_cycles(n) = useRotation ? n/activeCount : n` (`params.h:122-125`), `max_store_depth()` (`:128`), `is_single_member()` = size==1 (`:129`).
History check: mainnet consensus fields of 50_60/400_60/400_85 are identical since v0.13.0.0 (`git show v0.13.0.0:src/chainparams.cpp`), 100_67 since v0.17, 60_75 since v18.0 (only names/keepOld* changed later, e.g. `41b34186c6`). `signingActiveQuorumCount` 24/4/4 since v0.14.

### 1.3 Per-network assignment (`src/chainparams.cpp`; fields in `src/consensus/params.h:195-199`)
| net | AddLLMQ | ChainLocks | DIP0024 IS | Platform | Mnhf | lines |
|---|---|---|---|---|---|---|
| main | 50_60, 60_75, 400_60, 400_85, 100_67 | 400_60 | 60_75 | 100_67 | 400_85 | 272-280 |
| test | + 25_67 | 50_60 | 60_75 | 25_67 | 50_60 | 465-474 |
| devnet | main set + DEVNET, DEVNET_DIP0024, DEVNET_PLATFORM | DEVNET | DEVNET_DIP0024 | DEVNET_PLATFORM | DEVNET | 639-656 |
| regtest | TEST, TEST_INSTANTSEND, TEST_V17, TEST_DIP0024, TEST_PLATFORM | TEST | TEST_DIP0024 | TEST_PLATFORM | TEST | 913-926 |
- There is **no `llmqTypeInstantSend` any more** (removed by `112564974d` "deprecate non-deterministic IS support (#5553)"); historically mainnet `llmqTypeInstantSend = LLMQ_50_60` (v0.15–v20.x). Not consensus.
- Override args: devnet `-llmqchainlocks` (must be non-rotated, `chainparams.cpp:1222-1245`), `-llmqinstantsenddip0024` (must be rotated, `:1247-1270`), `-llmqplatform` (`:1272-1292`), `-llmqmnhf` (`:1294-1314`), `-llmqdevnetparams size:threshold` (sets size, minSize=threshold, threshold, badVotes=threshold; `:741-751,1335-1356`); regtest `-llmqtestparams`, `-llmqtestinstantsendparams`, `-llmqtestplatformparams` (`:983-991,1153-1187`), `-llmqtestinstantsenddip0024` (`:1189-1209`). All bounded by MAX_LLMQ_SIZE.

### 1.4 Which types are "enabled" (consensus!) — `ChainstateManager::IsQuorumTypeEnabled` (`src/validation.cpp:6654-6705`)
Evaluated on `pindexPrev` (nullptr → false):
- `LLMQ_50_60`: enabled iff `!DIP0024Active || pindexPrev->nHeight < DIP0024QuorumsHeight || net==testnet || net==devnet` (`:6675-6677`) → **disabled on mainnet from block 1738699 on**.
- `LLMQ_TEST_INSTANTSEND`: like 50_60 unless it is the DIP0024 IS type (`:6678-6680`).
- `LLMQ_TEST, TEST_PLATFORM, 400_60, 400_85, DEVNET_PLATFORM, DEVNET`: always.
- `LLMQ_TEST_V17`: TESTDUMMY BIP9 active.
- `LLMQ_100_67`: `DeploymentActiveAfter(pindexPrev, DIP0020)` (`:6691-6692`).
- `LLMQ_60_75, DEVNET_DIP0024, TEST_DIP0024`: DIP0024 active after pindexPrev (`:6694-6697`).
- `LLMQ_25_67`: `pindexPrev->nHeight >= 847000` hard-coded testnet constant (`:6666,6700-6701`).
Rotation-enabled: `IsQuorumRotationEnabled` (`src/llmq/options.cpp:50-62`) = `useRotation && cycleBase>=1 && DIP0024 active after ancestor(cycleBase-1)`.

Other per-type sporks (`src/llmq/options.cpp:26-47`): `EvalSpork(v)`: 0 → all types; 1 → all except 100_67/400_60/400_85. Used for SPORK_21 (all-members-connected) and SPORK_23 (PoSe). **On mainnet all sporks are hard-wired** (`src/spork.cpp:173-181`, commit `9ed292a6e1`): SPORK_21=1, everything else 0 (=active; SPORK_19 value 0 also = "signing enabled", `src/chainlock/chainlock.cpp:33-35`).

---------------------------------------------------------------------------------------------------

## 2. Final commitments, mining rules, storage, "active quorums" — difficulty **M** (+quirks)

### 2.1 `CFinalCommitment` (`src/llmq/commitment.h:45-145`)
Versions (`:48-51`): 1 `LEGACY_BLS_NON_INDEXED`, 2 `LEGACY_BLS_INDEXED`, 3 `BASIC_BLS_NON_INDEXED`, 4 `BASIC_BLS_INDEXED`. `GetVersion(rotation, basic)` (`:85-91`).
Wire format (`:95-115`):
```
u16 nVersion | u8 llmqType | uint256 quorumHash | [i16 quorumIndex  iff nVersion ∈ {2,4}]
| DYNBITSET signers | DYNBITSET validMembers
| G1 quorumPublicKey (48B, legacy iff nVersion∈{1,2}) | uint256 quorumVvecHash
| G2 quorumSig (96B, same scheme) | G2 membersSig (96B, same scheme)
```
DYNBITSET = compactsize(bitcount) + ceil(n/8) bytes, LSB-first; **non-zero padding bits are a deserialization error** (`src/serialize.h:433-440, 450-475, 619-634`).
`IsNull()` (`:118-131`): no bit set in either bitset AND pubkey invalid AND vvecHash null AND both sigs invalid. Note "invalid" BLS objects include all-zero bytes *and* bytes that are canonical in the *other* scheme (see §4.3) — so a commitment carrying opposite-scheme-encoded keys/sigs and empty bitsets is treated as **null**.

`CFinalCommitmentTxPayload` (special tx type 6, `TRANSACTION_QUORUM_COMMITMENT`, `src/primitives/transaction.h:35`): `u16 nVersion (=1) | u32 nHeight | CFinalCommitment` (`commitment.h:147-165`). Tx must be version 3 / type 6, may have empty vin+vout (`src/consensus/tx_check.cpp:18-21`), not accepted in mempool (`qc-not-allowed`, `src/validation.cpp:855-858`). Miners create it with `nVersion=3,nType=6`, no inputs/outputs (`src/llmq/blockprocessor.cpp:938-958`).

### 2.2 Commitment hash (`src/llmq/commitment.cpp:312-323`)
`commitmentHash = SHA256d( u8 llmqType | quorumHash | DYNBITSET(validMembers) | quorumPublicKey | quorumVvecHash )`.
**Gotcha:** `hw << pubKey` uses the *global* `bls_legacy_scheme` (`src/bls/bls.h:177-181`), not the commitment version. During `ConnectBlock(h)` the global is legacy iff `h < V19Height` (§4.4). Same hash is signed in DKG premature commitments (`src/llmq/dkgmessages.h:182-185`).

### 2.3 Verification
- **Tx-level** `CheckLLMQCommitment(pindexPrev, tx)` (`commitment.cpp:240-310`): payload parse (`bad-qc-payload`); known type (`bad-qc-commitment-type`); `1 <= qcTx.nVersion <= 1` (`bad-qc-version`); `qcTx.nHeight == pindexPrev->nHeight+1` (`bad-qc-height`); quorumHash known and an ancestor of pindexPrev (`bad-qc-quorum-hash`); null → `VerifyNull` (`bad-qc-invalid-null`) else `Verify(checkSigs=false)` (`bad-qc-invalid`). Dispatched from `CheckSpecialTxInner` (`src/evo/specialtxman.cpp:258-259`).
- **`Verify(base, checkSigs)`** (`commitment.cpp:113-209`), base = quorum base block index:
  1. base has a parent (`:125-128`).
  2. `nVersion == GetVersion(IsQuorumRotationEnabled(params, base), DeploymentActiveAfter(base, V19))` (`:130-137`) — i.e. version is fixed by the **quorum base block**, not the mining height.
  3. `base.hash == quorumHash`; `base.nHeight % dkgInterval == quorumIndex` (`:139-147`).
  4. `VerifySizes`: both bitsets have exactly `params.size` bits (`:226-238`).
  5. `CountValidMembers >= minSize` and `CountSigners >= minSize` (`:153-160`) — note **signers ≥ minSize, not threshold**.
  6. pubkey valid; `quorumVvecHash` non-null unless single-member; both sigs valid (`:161-176`).
  7. `members = GetAllQuorumMembers(type, base)` (§3); every bit `i >= members.size()` must be 0 in both bitsets (`:177-197`).
  8. If checkSigs: `VerifySignatureAsync` (`:35-110`): single-member → `membersSig.VerifyInsecure(members[0].pubKeyOperator, hash)`; else `membersSig.VerifySecureAggregated({operator pubkeys of signers, in member order}, hash)` (`:76-95`), and `quorumSig.VerifyInsecure(quorumPublicKey, hash)` (`:96-108`). Both use the global scheme.
- `VerifyNull` = IsNull && VerifySizes (`:211-224`).

### 2.4 Block processing (`src/llmq/blockprocessor.cpp`)
`ProcessBlock` (`:212-295`), run from `ProcessSpecialTxsInBlock` after all special-tx checks (`src/evo/specialtxman.cpp:897`):
1. Pre-DIP0003 block: nothing (`:218-221`).
2. `PreComputeQuorumMembers` for rotation-enabled types at `h % dkgInterval == 0` — **this is what creates and stores the quarter snapshots** (`:34-42, 223`).
3. `GetCommitmentsFromBlock` (`:564-604`): every type-6 tx; unknown type → `bad-qc-commitment-type`; for non-rotated types more than one commitment of the same type → `bad-qc-dup`; any commitment pre-DIP0003 → `bad-qc-premature`.
4. For each type **enabled at pindex->pprev** (`GetEnabledQuorumParams`, `src/llmq/options.cpp:125-135`): `required = GetNumCommitmentsRequired(params, h)`; `count > required` → `bad-qc-not-allowed`; `count < required` → `bad-qc-missing` (`:234-253`).
5. Within-block duplicate (type, quorumHash) among **non-null** commitments → `bad-qc-dup` (`:255-265`; re-added by `055e279129`, matches v23 behaviour).
6. If `fBLSChecks` (= `fScriptChecks`, i.e. **skipped below assumevalid**, `src/validation.cpp:2543-2569,2664`; `specialtxman.cpp:897`): batch-verify sigs of all non-null commitments → `bad-qc-invalid` (`:267-284`).
7. `ProcessCommitment` for each (`:326-459`):
   - `quorumHash` must equal `chain[h - h%dkgInterval + quorumIndex]` (`GetQuorumBlockHash`, `:629-642`) → else `bad-qc-block` (`:338-367`).
   - null: `VerifyNull` → `bad-qc-invalid-null`; **returns true here, before the mining-phase check** (`:369-379`).
   - non-null already mined for (type, quorumHash) → `bad-qc-dup` (`:381-393`; v23 semantics = `HasMinedCommitment` on the active chain).
   - `!IsMiningPhase` → `bad-qc-height` (`:395-398`); `Verify(checkSigs=false)` → `bad-qc-invalid` (`:408-415`).
   - Store `q_mc:(type,quorumHash) → (qc, minedBlockHash)` (`:46,432`) and index `q_mcih:(type, BE32(UINT32_MAX - minedHeight)) → baseHeight` for non-rotated or `q_mcihi:(type, quorumIndex, BE32(...)) → baseHeight` for rotated (`:299-309, 437-441`).
`IsMiningPhase(h)` (`:311-324`): `cycleStart = h - h%dkgInterval`; `cycleStart+mineStart <= h <= cycleStart+mineEnd`.
`GetNumCommitmentsRequired` (`:606-626`): 0 outside the window; inside: count of `quorumIndex ∈ [0, rotation? activeCount : 1)` such that `chain[cycleStart+quorumIndex]` exists and no commitment for it is mined **on this chain**. ⇒ in every window block a (null-or-real) commitment is mandatory per still-unmined quorum; once mined, none allowed. Real DKG output can therefore appear only in the window; failures are filled with null commitments every block until window end.
`UndoBlock` (`:519-562`) erases `q_mc` and the height index (if no other chainstate references the block), re-adds to the mineable pool.
Side effect in the MN list: for every non-null commitment, members whose `validMembers[i]==0` and still in the list get `PoSePunish(CalcPenalty(66))` (`src/evo/specialtxman.cpp:284-297, 703-728`). **This makes bit-exact member computation consensus-critical even when BLS checks are skipped.**

**Quirk (disabled types):** step 4 only polices *enabled* types; a **null** commitment of a known-but-disabled type (e.g. LLMQ_50_60 on mainnet after 1738698, LLMQ_60_75 before DIP0024, LLMQ_100_67 before DIP0020) passes CheckLLMQCommitment/ProcessCommitment at *any* height as long as `quorumHash == chain[h - h%interval + quorumIndex]` (null path skips `IsMiningPhase`). Replicate the acceptance; don't "fix" it.

### 2.5 "Active quorums at height h"
- `GetMinedCommitmentsUntilBlock(type, pindex, maxCount)` (`:687-729`): newest-first by **mined height ≤ pindex.h**, up to maxCount.
- Rotated: `GetLastMinedCommitmentsByQuorumIndexUntilBlock(type, pindex, idx, cycle)` (`:731-777`) = the `cycle`-th most recent mined commitment with that quorumIndex (regardless of age); `GetMinedCommitmentsIndexedUntilBlock` (`:796-815`) concatenates cycle 0 for idx 0..N-1, then cycle 1, … up to maxCount. ⇒ if index i failed this cycle, the older quorum with index i stays "active".
- `ScanQuorums(type, pindexStart, n)` (`src/llmq/quorumsman.cpp:170-286`): returns {} if type not enabled at pindexStart; snaps pindexStart to the mining window: before window → `ancestor(cycleStart+mineEnd-dkgInterval)`; after window → `ancestor(cycleStart+mineEnd)`; then the above lists. (`ScanCommitments` same, `:294-409`.)
- **CbTx `merkleRootQuorums`** (DIP0008; `src/evo/cbtx.cpp:65-156`, called at `specialtxman.cpp:944-953` whenever cbtx.nVersion ≥ 2): `GetMinedAndActiveCommitmentsUntilBlock(pindexPrev)` (`blockprocessor.cpp:818-832`) iterates **all configured LLMQ types, not only enabled ones** — non-rotated: last `signingActiveQuorumCount` mined; rotated: latest per quorumIndex. Leaves = `SerializeHash(CFinalCommitment)`; current block's non-null commitments: rotated → replace slot `quorumIndex`; non-rotated → pop oldest if full, push (`cbtx.cpp:87-119`); `> activeCount` → `excess-quorums-calc-cbtx-quorummerkleroot`; all leaves sorted by memcmp; Bitcoin merkle; `mutated` → invalid (`:121-153`). **Implication: the last 24 LLMQ_50_60 commitments remain leaves forever after 50_60 was disabled.** *Empirically confirmed:* decoding rust-dashcore's mainnet fixture `dash/tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin` (full SML at height 2,227,096; probe crate in `/tmp/r4-e-probe`, repo untouched) gives quorums {100_67 v3: 24, 400_60 v3: 4, 400_85 v3: 4, **50_60 v1: 24**, 60_75 v4: 32}; the sorted-leaf merkle root over all 88 equals the coinbase `merkleRootQuorums` `0xd46ee282…1243`, i.e. the stale **legacy-BLS (v1)** 50_60 commitments from before 1738698 are still committed to in 2024+ blocks. (The SML/`mnlistdiff` quorum set is built from the same function.)
- `CbTx` version gating: v≥2 required when `DeploymentActiveAt(*pindexPrev, DIP0008)` i.e. from block DIP0008Height+1 (`cbtx.cpp:35-38`); v3 (CL+balance) iff V20 active after prev (`:40-43`); v4 iff V24&&V20 (`:47-51`).

### 2.6 Mineable-commitment pool (node policy)
`qfcommit` P2P (`blockprocessor.cpp:98-210`): only accepted if solicited; null → ban 100; quorum base must be in active chain, first block of interval(+index), not older than one interval; verify with sigs; keep the one with most signers (`AddMineableCommitment`, `:840-867`). `GetMineableCommitments` (`:882-936`) emits stored best or a null commitment with correct version/quorumIndex.

---------------------------------------------------------------------------------------------------

## 3. Quorum member selection — difficulty **L→XL** (rotation is the hardest bit-exact code here)

### 3.1 Primitives (`src/llmq/utils.cpp`)
- `confirmedHashWithProRegTxHash = SHA256(proTxHash || confirmedHash)` (single SHA256, `src/evo/dmnstate.h:53-55,166-168`).
- **Score** `calculateQuorumScore = SHA256(confirmedHashWithProRegTxHash || modifier)` single SHA256, compared as `arith_uint256` (numeric, LE) (`utils.cpp:72-85`).
- `CalculateScoresForQuorum(list)` (`:137-155`): iterate **valid (non-PoSe-banned)** MNs, skip `confirmedHash == 0`, skip non-Evo if `onlyEvoNodes`. Vector overload (`:117-135`) additionally skips banned explicitly.
- `CalculateQuorum` (`:160-186`): sort **descending** by score (`std::sort(rbegin, rend, cmp)`), tie → by collateralOutpoint (effectively descending outpoint); truncate to `maxSize` if given.
- `WORK_DIFF_DEPTH = 8` (`src/llmq/snapshot.h:40`).

### 3.2 Modifier (`utils.cpp:87-115`)
`GetHashModifier(params, cycleBase)`: `work = ancestor(cycleBase.h - 8)`.
- If `DeploymentActiveAfter(work, V20)`: `GetHashModifierFromWorkBlock` (`:87-97`): if the **work block's own coinbase** carries a non-null CL (`GetNonNullCoinbaseChainlock(work)`, `src/evo/cbtx.cpp:181-211`: CbTx v≥3, `nHeight` matches, sig valid, `diff < height`; reads the block from disk): `SerializeHash(tuple(u8 llmqType, i32 work.nHeight, G2 bestCLSignature[global scheme]))`; else `SerializeHash(pair(u8 type, work.hash))`.
- Pre-V20: rotated → `SerializeHash(pair(type, work.hash))`; non-rotated → `SerializeHash(pair(type, cycleBase.hash))` (`:110-114`).
History: V20 path added in `9eee9ee680` (#5366).

### 3.3 Non-rotated quorums (`ComputeQuorumMembers`, `utils.cpp:291-305`; caller `GetAllQuorumMembers` `:715-725`)
- MN list = `GetListForBlock(workBlock)` where workBlock = `base-8` if `DeploymentActiveAfter(base, V20)` else `base` (`:716-722`).
- `EvoOnly = (type == llmqTypePlatform) && DeploymentActiveAfter(base, V19)` (`:294-295`).
- members = top `params.size` by score.
- **Quirk (V20 boundary):** MN-list gate uses `base`, modifier gate uses `base-8`. On mainnet V20Height=1987776 is a multiple of 24/288/576, so the first V20 cycle (base=V20Height) uses the **base-8 MN list with the old base-hash modifier**. Replicate exactly.
- Members are cached per base hash; `GetAllQuorumMembers` returns {} if base has no parent or type not enabled at `base->pprev` (`:644-647`).

### 3.4 Rotated quorums (DIP0024) — `GetAllQuorumMembers` (`:666-714`) → `ComputeQuorumMembersByQuarterRotation` (`:471-544`)
Definitions: `quorumIndex = base.h % dkgInterval` (must be `< activeCount`), `cycleBase = base.h - quorumIndex`, `work = cycleBase - 8`; members of **all** `activeCount` quorums of a cycle are computed at once, from the work block MN list, and the result for the cycle is cached by `(cycleBaseHash, idx)`.
Algorithm:
1. `allMns = GetListForBlock(work)`.
2. For prev cycles k=1..3 (`H-C, H-2C, H-3C`): `cycleIdx_k = ancestor(cycleBase - k*dkgInterval)`; load stored snapshot for `(type, cycleIdx_k)`; **if missing, `break`** (remaining older quarters stay empty) (`:488-510`). Then `GetQuorumQuarterMembersBySnapshot` (`:188-289`):
   - `sortedAll = CalculateQuorum(GetListForBlock(cycleIdx_k - 8), GetHashModifier(cycleIdx_k))` (no size limit);
   - split by `snapshot.activeQuorumMembers[i]`: unused (non-banned) first, then used, preserving sorted order;
   - mode 0 (no skipping): fill each of `activeCount` quarters with `size/4` MNs round-robin through the combined list (wraps);
   - mode 1 (skip list): decode skip list (first entry absolute, others relative to first — **if first==0 every later entry is treated as absolute**, mirrors the encoder), walk the list skipping those indexes, wrapping;
   - modes 2/3: return empty quarters.
3. `BuildNewQuorumQuarterMembers` (`:343-466`):
   - if `allMns.enabled() < size/4` → all quarters empty.
   - `skipRemovedMNs = cycleBase+1 >= V19Height || net==testnet` (`:369-373`; introduced `78593c7d71`, "Rotation members calc v19 (#5142)" — pre-V19 mainnet keeps removed MNs).
   - Build `MnsUsedAtH` and per-index `MnsUsedAtHIndexed[idx]` by inserting, for idx=0..N-1, for prev cycles (H-C, H-2C, H-3C), each member (skipping removed if `skipRemovedMNs`, skipping currently PoSe-banned); **`AddMN` exceptions are swallowed** (`:375-394`). `AddMN` throws on duplicate proTxHash/internalId/collateral/any address/owner key(s)/operator key (`src/evo/deterministicmns.cpp:439-499`). The inserted objects carry their **old** state from the snapshot-era list.
   - `MnsNotUsedAtH` = allMns entries (incl. invalid) not in MnsUsedAtH and not banned; `sortedCombined = CalculateQuorum(NotUsed) ++ CalculateQuorum(UsedAtH)` (`:396-409`).
   - Fill: global cursor `idx` over sortedCombined, for each quorum i: while quarter not full and `usedCount_i + quarter_i.size < sortedCombined.size()`: if MN not in `MnsUsedAtHIndexed[i]` try `AddMN` (may throw → skip) and append; else skip. Skipped positions are recorded: first as absolute idx (`firstSkippedIndex`), later as `idx - firstSkippedIndex` (with the same "first==0" quirk). Wrap at end; a full loop without progress → return all-empty (`:416-456`).
   - If not predicting: `BuildQuorumSnapshot` (`:307-341`) — bitset over `CalculateQuorum(allMns, modifier)` of "used at H" + skip list/mode — and `StoreSnapshotForBlock(type, cycleBase)` (EvoDB key `("llmq_S", SerializeHash(pair(type, cycleBaseHash)))`, `src/llmq/snapshot.cpp:24,301-330`).
4. Quorum i members = `quarter(H-3C)[i] ++ quarter(H-2C)[i] ++ quarter(H-C)[i] ++ new[i]` (`:529-541`).
Snapshot wire format (`snapshot.h:42-90`): `i32 mnSkipListMode | compactsize n | fixed-bitset(n) | compactsize m | i32 × m`.
Consequences: (a) the node must process every cycle base sequentially and persist snapshots (they are derived state, not on chain); (b) the first three rotated cycles after activation produce quorums of 15/30/45 members (prev snapshots missing) → only null commitments; mainnet DIP0024Height=1737792 (=6034·288) ⇒ first full cycle 1738656, first rotated commitment at +42 = **1738698 = DIP0024QuorumsHeight** (`src/chainparams.cpp:191-192`); (c) `ComputeQuorumMembersFromWorkBlock` (`utils.cpp:589-632`) predicts future members (post-V20 only) — node-local.
Other masternode helpers: `DeterministicOutboundConnection` (`:732-752`), `GetQuorumConnections` all-connected vs `GetQuorumRelayMembers` ring `i+2^k` (`:754-831`), `CalcDeterministicWatchConnections` (random seed, `:833-853`).

---------------------------------------------------------------------------------------------------

## 4. BLS — difficulty **L** (pure-Rust legacy scheme) / **M** (FFI to vendored dashbls)

### 4.1 Library and schemes
Dash wraps the vendored Chia-derived `src/dashbls` (relic backend). `src/bls/bls.cpp:21-27`: `Scheme(true)=LegacySchemeMPL`, `Scheme(false)=BasicSchemeMPL`. Global switch `bls::bls_legacy_scheme` (default `true`, `bls.cpp:17-19`). Messages are always 32-byte hashes.
| aspect | Legacy | Basic |
|---|---|---|
| G1 (48B) encoding | relic x, bit7 of byte0 = sign(y) where sign = `y > (p-1)/2`; no compression flag; infinity `0xc0 00…` | IETF/ZCash: bit7=compressed(1), bit6=infinity, bit5=sign | 
| G2 (96B) encoding | relic Fp2 order (c0‖c1?) as written by relic, sign in bit7 of byte0 | halves swapped vs relic (`elements.cpp:266-267, 414-419`), flags 0x80/0x20, byte48 top 3 bits must be 0 |
| validation on parse | **no `CheckValid` (no subgroup/on-curve enforcement); relic errors swallowed** (`elements.cpp:24-30, 85, 245-251, 310`) | full `CheckValid` |
| hash-to-G2 | `ep2_map_legacy(msg, 32)` — old Chia map with "G2_0_c0"… suffixes, custom sqrt(-3) constants (`src/dashbls/src/legacy.cpp:1-40,306+`, `elements.cpp:339-344`, `privatekey.cpp:298-310`) | IETF SSWU RO, DST `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_` (`schemes.cpp:81`) |
| Verify | pairing check with legacy map (`schemes.cpp:658-669`) | standard |
| AggregateVerify | legacy map per msg (`schemes.cpp:683-704`) | Basic (distinct msgs) |
Code pointers: G1 parse/serialize `elements.cpp:24-87,173-198`; G2 `:245-312,397-424`; relic sign logic `depends/relic/src/ep/relic_ep_pck.c:40-135` (both schemes use the "y > (p-1)/2" sign; the difference is only bit placement/order/validation).
**Key observation (from `src/governance/vote.cpp:32-36`):** legacy encoding of P can equal basic encoding of −P (legacy sign bit = 0x80 = basic compression flag). Parsing under the wrong scheme silently negates points.

### 4.2 Secure aggregation (used for `CFinalCommitment.membersSig`) — `CoreMPL::VerifySecure/AggregateSecure` (`schemes.cpp:30-58,194-260`)
`pk_i` serialized **in the active scheme**, sorted by memcmp; `H = SHA256(concat sorted pks)` (single SHA256, `Util::Hash256` = `md_map_sh256`, `include/dashbls/util.hpp:61-64`); `t_i = SHA256(BE32(i) ‖ H) mod r`; `aggPk = Σ t_i·pk_sorted[i]`; verify `e(aggPk, H2(msg)) == e(g1, sig)` (legacy: `ep2_map_legacy`; basic: AggregateVerify with SSWU). Because `t_i` depends on the serialization, **legacy and basic give different results for the same points.**

### 4.3 Dash wrapper semantics (`src/bls/bls.h`)
- `SetBytes` (`:100-123`): wrong size or all-zero → invalid (Reset); parse exception → invalid; parsed point equal to default (identity) → invalid.
- `Unserialize(s, legacy)` (`:183-203`): parse; **`CheckMalleable`** = re-serialize must equal input; if not, parse with the *opposite* scheme; if that is canonical → **object becomes invalid (Reset), no error**; if neither → throw `"malleable BLS object"` (= payload parse failure).
- Canonical infinity (`0xc0 00…`) decodes in dashbls (`elements.cpp:46-55`) but equals the default element, so `SetBytes` resets it to invalid, `CheckMalleable` then fails in both schemes ⇒ **throw** (payload/message parse failure). Practically: identity points are rejected.
- Legacy parse quirk: `FromBytes(fLegacy=true)` skips `CheckValid`, and relic errors are cleared without throwing (`CheckRelicErrors(!fLegacy)`, `src/dashbls/src/bls.cpp:81-92`; relic `RLC_THROW` only sets an error code, `depends/relic/include/relic_err.h:255-266`), so an off-curve legacy x with sign bit 0 can yield a "valid" `CBLSPublicKey` holding a non-curve point whose re-serialization matches (passes CheckMalleable). Any later pairing just fails. Store **raw bytes + scheme** (like `CBLSLazyPublicKey`) and decode lazily; never normalize stored keys.
- `Serialize()` without explicit scheme uses the global flag (`:177-181,205-209`); `CBLSPublicKeyVersionWrapper`/`CBLSSignatureVersionWrapper` force a scheme (`:322-389`).
- `CBLSLazyWrapper` (`:391-575`) keeps raw bytes + a per-object legacy flag; decodes on `Get()`; default flag = global at construction. Used for ProTx/DMN-state operator keys and for P2P sigs (recsig, islock).
- Ops: `AggregateInsecure` (scheme-independent math but wrapper uses global), `SubInsecure`, `Recover` (Lagrange, §4.5), `DHKeyExchange` = `sk·pk` (`bls.cpp:208-220`), `Sign(hash, legacy)`, `VerifyInsecure(pk, hash[, legacy])`, `VerifySecureAggregated` (global), `VerifyInsecureAggregated` (global).

### 4.4 When the scheme flips and who uses which
- ConnectBlock: `ScopedBLSLegacyScheme guard{ !DeploymentActiveAt(*pindex, V19) }` (`src/validation.cpp:137-178, 2485-2492`), committed on success (`:2914`); `ProcessSpecialTxsInBlock` flips to basic after a block once `DeploymentActiveAfter(pindex, V19)` (`src/evo/specialtxman.cpp:985-990`); undo flips back (`:1007-1015`); DisconnectTip guard (`validation.cpp:3263-3271`). Startup sets it from tip (`src/node/chainstate.cpp:431,458-460`). **Rule: while validating block h the global = legacy ⇔ h < V19Height.**
- Version-selected objects: `CFinalCommitment` (v1/2 legacy, v3/4 basic, §2.1); ProTx `pubKeyOperator`/`sig` legacy iff `nVersion == ProTxVersion::LegacyBLS` (`src/evo/providertx.h:180,301,346,419`; ProUpRev-like at `:534` always basic); DMN state (`src/evo/dmnstate.h:107,301`; old-format `:436` legacy); SML entry (`src/evo/simplifiedmns.h:83`).
- Always-basic: MNHF sig (`src/evo/mnhftx.h:49`), MNAUTH (`src/evo/mnauth.cpp:45,98`), IES symmetric key derivation (`src/bls/bls_ies.cpp:47,83,100`), quorum vvec on disk (`src/llmq/quorums.cpp:192-205`).
- Global-flag objects: CLSIG/`ChainLockSig.sig`, CbTx `bestCLSignature` (only exists post-V20 ⇒ basic in practice), `CRecoveredSig`, `InstantSendLock.sig`, sig shares, DKG messages, commitment-hash pubkey, member secure aggregation, `quorumVvecHash = SerializeHash(vector<CBLSPublicKey>)` (`src/llmq/quorums.cpp:117-128`; DKG `src/active/dkgsession.cpp:529`).
- DKG sessions pick the scheme from the quorum base: `m_use_legacy_bls = !DeploymentActiveAfter(base, V19)` (`src/active/dkgsession.cpp:34`).
- Mainnet/testnet boundary check: V19Height mainnet 1899072 (multiple of 576), testnet 850100 — in both cases no commitment's base/mining heights straddle V19 (checked arithmetic for 24/288/576 intervals), so "version scheme" and "global scheme" agree on real chains; devnet/regtest may differ — implement the global rule literally.

### 4.5 Threshold / DKG math (`src/dashbls/src/threshold.cpp`)
- Member id = `CBLSId(proTxHash)` (raw 32 bytes, `src/bls/bls.cpp:29-34`), interpreted by `bn_read_bin` as **big-endian** integer of the uint256's internal (LE) bytes, mod r (`threshold.cpp:170-188,191-246`).
- `Evaluate` Horner, needs ≥2 coeffs (threshold ≥ 2); `LagrangeInterpolate` at 0 with "Zero id"/"Duplicate id" errors; relic floored division ⇒ normal field arithmetic.
- `PrivateKeyShare`, `PublicKeyShare` (vvec at id), `SignatureRecover` (first `threshold` shares in map iteration order — any t shares give the same result).
- Threshold signing itself uses LegacySchemeMPL internally only for `Threshold::Sign/Verify` (`threshold.cpp:12,274-280`) — Dash signs shares via `CBLSSecretKey::Sign(hash, scheme)` instead.

### 4.6 BLS-IES (DKG secret share encryption) (`src/bls/bls_ies.{h,cpp}`)
Ephemeral sk/pk per contribution; `symKey = first 32 bytes of basic-serialized (eph_sk · recipient_pk)`; AES-256-CBC **without padding**; IV for recipient i = first 16 bytes of `SHA256d^i(ivSeed)` (`bls_ies.cpp:31-38,59-73`). Wire: `G1 ephemeralPubKey (global scheme) | uint256 ivSeed | vector<vector<u8>> blobs` (`bls_ies.h:82`). Plaintext = 32-byte serialized secret key.

### 4.7 Test vectors
`src/test/bls_tests.cpp` (`:423-767`: sethex, serialize, sign, key/sig agg, secure agg, DH, threshold, batch verifier, contribution shares, V19 boundary scheme restore), `src/dashbls/src/test.cpp` ("IETF test vectors" `:319`, "Chia test vectors" `:335`, "Legacy HD keys" `:1325`, "Threshold Signatures" `:1439`, "CheckValid for Legacy" `:1574`), `src/test/llmq_hash_tests.cpp` (commitment hash), `llmq_commitment_tests.cpp`, `llmq_snapshot_tests.cpp`, `evo_islock_tests.cpp`, `llmq_chainlock_tests.cpp`, `evo_mnhf_tests.cpp`. Best real-world vectors: mainnet blocks 1088640..V19 (legacy commitments, legacy membersSig) and post-V19 (basic), plus rust-dashcore's mainnet MNListDiff/QRInfo fixtures (see §11).

---------------------------------------------------------------------------------------------------

## 5. ChainLocks — difficulty **M** (verification + CbTx), **M** (enforcement/signing, node-local)

### 5.1 Objects and signing
- `ChainLockSig` = `i32 nHeight | uint256 blockHash | G2 sig (global scheme)` (`src/chainlock/chainlock.h:23-45`); P2P `clsig` (`src/protocol.cpp:85`).
- requestId `= SerializeHash(pair(string_view "clsig", i32 height))` = SHA256d(`05 'clsig' | LE32 height`) (`src/chainlock/clsig.cpp:27,248-251`); msgHash = block hash.
- Quorum: `llmqTypeChainLocks` (mainnet LLMQ_400_60, non-rotated), selected with `SelectQuorumForSigning(…, signHeight=clsig.height, SIGN_HEIGHT_OFFSET=8)` i.e. ScanQuorums at `chain[height-8]` (`src/llmq/quorumsman.h:207-213`, `quorumsman.cpp:711-722,724-775`); non-rotated choice = `argmin_memcmp SHA256d(u8 type | quorumHash | requestId)` over the `signingActiveQuorumCount` active quorums (`:759-774`, sort at `:772`).
- `VerifyRecoveredSig` (`quorumsman.cpp:784-797`): `sig.VerifyInsecure(quorumPublicKey, SignHash(type, quorumHash, id, msgHash))` with global scheme. `SignHash = SHA256d(u8 type | quorumHash | id | msgHash)` (`src/llmq/signhash.cpp:14-22`).
- Signer (`src/chainlock/signing.cpp`, masternode): every 5 s / on new tip, if synced, CL enabled and `SPORK_19 == 0`, tip not already CL'd at ≥height, no conflicting CL; if IS enabled, require every tx in the tip and up to `TX_CONFIRM_THRESHOLD=5` blocks below (stopping at a CL'd block) to be IS-locked or seen ≥ `WAIT_FOR_ISLOCK_TIMEOUT=10 min` (`chainlock/handler.cpp:39-40,257-269`); then `AsyncSignIfMember(llmqTypeChainLocks, requestId, blockHash)`.

### 5.2 What is consensus vs. node-local
**Deterministic consensus (from chain data only):**
- CbTx v3 (`CLSIG_AND_BALANCE`, V20+) `bestCLHeightDiff` (compactsize) + `bestCLSignature` (`src/evo/cbtx.h:54-70`), checked by `CheckCbTxBestChainlock` (`src/evo/specialtxman.cpp:139-214`, run only when `fCheckCbTxMerkleRoots`/`fScriptChecks`):
  - if prev block's coinbase has a non-null CL: this one must be non-null (`bad-cbtx-null-clsig`) and `diff <= prevDiff + 1` (`bad-cbtx-older-clsig`) ⇒ CL height non-decreasing (`:169-178`);
  - non-null: `diff < h` (`bad-cbtx-cldiff`), CL height = `h - diff - 1`, verify as a CLSIG for `ancestor(that height)` via quorum at `CLheight-8` (`bad-cbtx-invalid-clsig`) (`:181-205`); shortcut: equal to the node's best P2P CLSIG ⇒ skip verify (pure cache);
  - null sig requires `diff == 0` (`:209-212`).
  - "null" = `!IsValid()` ⇒ all-zero bytes **or bytes canonical only in the opposite scheme** (§4.3).
  - V20+ modifier for quorum selection depends on these coinbase CLs (§3.2).
- Miner fills it from best CL or carries the previous coinbase CL forward (`src/node/miner.cpp:117-168`).

**Node-local (P2P CLSIG + sporks), not reproducible from chain data:**
- `Chainlocks` best-CL store (`src/chainlock/chainlock.cpp:49-173`); `ProcessNewChainLock` verifies and updates (`handler.cpp:133-183`); also seeds from coinbase CLs once synced (`ProcessCoinbaseChainLock`, `:104-131`).
- Enforcement: `EnforceBestChainLock` → `Chainstate::EnforceBlock` marks all sibling branches along the CL'd chain `BLOCK_CONFLICT_CHAINLOCK` (disconnecting if needed) and reconsiders the CL'd chain (`src/validation.cpp:4046-4075, 4077-4148`); `ActivateBestChain` (`handler.cpp:273-322`).
- Validity hooks using the local best CL: `ConnectBlock` `bad-chainlock` (`validation.cpp:2517-2520`), header acceptance `bad-chainlock` + store header as conflicting (`:4691-4696`), `bad-prevblk-chainlock` (`:4641-4644`), TestBlockValidity (`:4935-4937`), chain selection skips `BLOCK_CONFLICT_CHAINLOCK` (`:3543-3558,3878,3926,4027`).
- Gated by SPORK_19 (`IsEnabled` = active; `IsSigningEnabled` = value 0, `chainlock.cpp:33-35`); hard-wired on mainnet (§1.4). First shipped v0.14.0.0 (`29532ba196`, `135829dc49`).
- Note: there is **no** spork-3-style switch; `SPORK_3_INSTANTSEND_BLOCK_FILTERING` was removed (`afcf3d148e`).

### 5.3 Coinbase-CL history reader
`CoinbaseChainLockReader` (`src/chainlock/clsig.cpp:30-246`, `a0bcedba58`): reads raw sig bytes at payload offset 70 (`:128-144`), binary search over carriers; used by quorum proofs (`src/llmq/quorumproofs.cpp`) — RPC/Platform feature, not block validity.

---------------------------------------------------------------------------------------------------

## 6. InstantSend — difficulty **M** (verify + DB), **L** (full signer incl. input locks); consensus effect small

### 6.1 Formats
- `InstantSendLock` (P2P `isdlock`, `src/instantsend/lock.h:21-52`): `u8 nVersion(=1) | vector<COutPoint> inputs | uint256 txid | uint256 cycleHash | G2 sig (lazy, global)`. `TriviallyValid`: txid non-null, 1..MAX_INPUTS(=MaxBlockSize()/41) unique inputs (`src/instantsend/lock.cpp:30-39`).
- islock requestId = `SHA256d("\x06islock" | compactsize n | n×(txid|u32 n))` (`lock.cpp:15,18-24`); msgHash = txid.
- Input lock requestId = `SerializeHash(pair("inlock", outpoint))` (`lock.cpp:14,41-44`); input locks are signed first (one per input) with msgHash = txid; once all recovered, the islock is signed (`src/instantsend/signing.cpp:372-474`).
- `GetLockInputs`: prevouts, except **asset-unlock tx → synthetic outpoint `{SerializeHash(pair(ASSET_UNLOCK_REQUESTID_PREFIX, index)), 0}`** (`lock.cpp:46-57`) — new on develop (v24-era); not on historical chain.
- Deterministic (`isdlock`, cycleHash) since v18 (`b4d001a602`); non-deterministic `islock` removed (`112564974d`).

### 6.2 Quorum selection / verification (node-local)
- Type `llmqTypeDIP0024InstantSend` (mainnet LLMQ_60_75, rotated). Signer: `SelectQuorumForSigning(params, chain, id)` and `cycleHash = hash of ancestor(base.h - base.h % dkgInterval)` (`signing.cpp:425-474`).
- Rotated selection (`quorumsman.cpp:733-757`, formula at `:738-743`; duplicated in `SelectCommitmentForSigning` `:810-815`): `n = log2(activeCount)` (=5), `b = id.GetUint64(3)` (last 8 bytes LE), **`signer = ((1<<n)-1) & (b >> (64-n-1))`** — i.e. bits 58..62, *not* the top n bits (quirk since rotation `83ef1c6c66`); pick the active quorum with `quorumIndex == signer` (the `signer > size` guard is off-by-one but harmless).
- Verifier: resolve cycleHash height (must be `% dkgInterval == 0`, else misbehave) (`net_instantsend.cpp:73-101`); `signHeight = cycleHeight + dkgInterval - 1` if the cycle is older than tip-dkgInterval else tip; try `signOffset=0` then `signOffset=dkgInterval` (with ban) (`net_instantsend.cpp:139-160, 330-355`). Regtest non-rotated: cycleHash *is* the quorum hash (`:148-152`).

### 6.3 Consensus / mempool effects
- `ConnectBlock`: **only when not IBD** and IS enabled (SPORK_2, also off during reindex/import, `src/instantsend/instantsend.cpp:510-513`): for each tx with lock inputs, if an islock on any of its inputs locks a *different* txid → if the block is CL'd (local best CL) remove the conflicting islock (CL overrides IS), else reject `conflict-tx-lock` (`src/validation.cpp:2796-2817`). Node-local, non-deterministic.
- Mempool: `tx-txlock-conflict` (`validation.cpp:891-900`); waiting-for-tx islocks evict conflicts (`:902-906`).
- Miner: include non-locked txs only if seen ≥10 min (`src/node/miner.cpp:418-425`).
- CL ⇒ islocks for confirmed txs are pruned (`instantsend.cpp:515-541`).
- `nInstantSendConfirmationsRequired` (mainnet 6, `chainparams.cpp:166-167`) for signer-side input maturity unless CL'd (`signing.cpp:239-300`).
**Nothing in IS affects block validity during IBD or for an offline validator; a validating-only port can defer IS.**

---------------------------------------------------------------------------------------------------

## 7. Signing sessions (recovered sigs) — difficulty **L**

- Common base `CSigBase{u8 llmqType, quorumHash, id, msgHash}` (`src/llmq/signing.h:44-72`).
- `qsigrec` `CRecoveredSig` = `u8 type | quorumHash | id | msgHash | G2 sig(lazy)` (`signing.h:74-111`); verified in batches against `SelectQuorumForSigning` result? — no: against the quorum named by `quorumHash`, which must be "active" = among `ScanQuorums(type, keepOldConnections)` (`signing.cpp:701-712`); unknown type → ban (`net_signing.cpp:49-62`). `qsendrecsigs` opt-in for relaying.
- DB: recovered sigs by (type,id), by hash, by signHash; "votes" (type,id → msgHash) prevent signing conflicting msgHash (`signing.h:113-151`; `AsyncSignIfMember` `signing_shares.cpp:776-860`). Platform type may sign multiple msgs per id (`HasRecoveredSigForSigning`, `signing.h:245-246`).
- Sig share = `u8 type | quorumHash | u16 quorumMember | id | msgHash | G2 sigShare(lazy)` (`signing_shares.h:49-89`); share = `skShare.Sign(SignHash, globalScheme)` (`signing_shares.cpp:1587-1636`); single-member quorum signs with operator key (`:1550-1585`).
- Messages: `qsigsesann` `CSigSesAnn{VARINT sessionId, type, quorumHash, id, msgHash}` (max 100/msg) (`signing_shares.h:94-117`, `signing_shares.cpp:245-284`); `qsigsinv`/`qgetsigs` `CSigSharesInv{VARINT sessionId, AUTOBITSET inv(size==params.size)}` (max 200) (`:119-145`, `:286-300`); `qbsigs` `CBatchedSigShares{VARINT sessionId, vector<(u16 member, sig)>}` (≤400 total) (`:148-161`, `net_signing.cpp:26-45`); `qsigshare` direct shares (≤32) **only processed if SPORK_21 active** (`net_signing.cpp:66-95`).
- Share verification: against `PubKeyShare = PublicKeyShare(quorumVvec, CBLSId(proTxHash))` for valid members (`src/llmq/quorums.cpp:155-164`) — requires the quorum **verification vector**, obtained from own DKG or via `qgetdata/qdata` (`net_quorum.cpp:273-354`).
- Recovery: when `#shares >= threshold`, `Recover` (Lagrange over member ids) (`signing_shares.cpp:616-748`); non-all-connected types push own share to `recoveryMembers` members ordered by `SerializeHash(pair(proTxHash, id))` (`:750-774,1009-1069`).
- Timeouts: `SESSION_NEW_SHARES_TIMEOUT=60s`, `SIG_SHARE_REQUEST_TIMEOUT=5s` (`signing_shares.h:448-459`).
- `IsQuorumSigningAllowed` refuses signing while an unvalidated UTXO snapshot is active (`signing_shares.cpp:1545-1548`).

---------------------------------------------------------------------------------------------------

## 8. DKG (masternode-only) — difficulty **XL**

- Phases (`src/llmq/dkgsessionhandler.h:33-41`; timing `src/active/dkgsessionhandler.cpp:43-70`): `stage = (tipHeight - quorumIndex) % dkgInterval`; on `stage % phaseBlocks == 0`, `phase = stage/phaseBlocks + 1`: 1 Initialized, 2 Contribute, 3 Complain, 4 Justify, 5 Commit, 6 Finalize, 7 Idle. For phaseBlocks=2: contribute at +2, complain +4, justify +6, commit +8, finalize +10 (= mining window start). Rotated: one handler per quorumIndex, offset by index blocks (window 42..50 = 32+10..). Members sleep `index * (phaseBlocks-1)*spacing/size * factor` before acting (`:166-227`).
- Session init: `members = GetAllQuorumMembers(type, base)`; abort if `< minSize` (`src/llmq/dkgsession.cpp:90-150`).
- Messages (`src/llmq/dkgmessages.h`):
  - `qcontrib` `CDKGContribution{u8 type, quorumHash, proTxHash, vector<G1> vvec (size=threshold), CBLSIESMultiRecipientObjects<sk> contributions (one per member), G2 sig}`; signHash = SHA256d(fields + 96 zero-sig bytes) (`:20-70`). Generation: random degree-(threshold−1) polynomial, vvec = coefficient pubkeys, share_j = poly(id_j) (`src/bls/bls_worker.cpp:78-117`); encrypted to each member's operator key (`src/active/dkgsession.cpp:40-110`); signed with operator key in the base's scheme.
  - `qcomplaint` `{type, quorumHash, proTxHash, DYNBITSET badMembers, DYNBITSET complainForMembers, sig}` (`:72-105`); a member is bad if `≥ dkgBadVotesThreshold` voted (`active/dkgsession.cpp:348`).
  - `qjustify` `{type, quorumHash, proTxHash, vector<(u32 index, sk)>, sig}` (`:107-136`).
  - `qpcommit` `CDKGPrematureCommitment{type, quorumHash, proTxHash, DYNBITSET validMembers, G1 quorumPublicKey, quorumVvecHash, G2 quorumSig (share), G2 sig (operator)}`; signHash = commitment hash (`:142-185`). quorumPubKey = vvec[0] of the summed vvec; `quorumVvecHash = SerializeHash(vvec)`; skShare = Σ verified contributions (`active/dkgsession.cpp:469-579`, vvecHash at `:529`).
  - Finalize: group premature commitments by validMembers; groups with `≥ minSize` → `membersSig = AggregateSecure(sigs, operatorPks, hash)`, `quorumSig = Recover(shares, ids)`, verify, emit `qfcommit` (`active/dkgsession.cpp:581-688`, AggregateSecure at `:661`). Single-member quorum workaround: quorum pubkey = operator key (`:690-746`).
  - Rules: max 2 messages per member per phase relayed, second marks member bad (`dkgsession.cpp:687-755`); PreVerify bounds sizes/sigs (`:158-520`).
- Connectivity: intra-quorum connections (`EnsureQuorumConnections`, `src/llmq/net_quorum.cpp:763-830`) — all-to-all (deterministic initiator) for SPORK_21 types, else relay ring; `qwatch` for `-watchquorums` observers (`net_quorum.cpp:200-210`); `mnauth` authenticates MN peers: `sig_basic(SerializeHash(tuple(operatorPk, receivedChallenge, isInbound, nodeVersion)))` (`src/evo/mnauth.cpp:26-46,95-98`). Quorum data recovery `qgetdata/qdata` (vvec + encrypted contributions) (`net_quorum.cpp:273-354,534-706`).
- Chain data needed: DMN lists at work blocks with full state (operator keys, confirmedHash, PoSe), snapshots, mined commitments, block hashes; operator secret key.

---------------------------------------------------------------------------------------------------

## 9. EHF (DIP0023 MNHF) — difficulty **S–M**

- Special tx type 7 `TRANSACTION_MNHF_SIGNAL` payload `u8 nVersion(=1) | MNHFTx{u8 versionBit, uint256 quorumHash, G2 sig (always basic)}` (`src/evo/mnhftx.h:36-93`). Empty vin/vout allowed, fee-exempt in mempool (`tx_check.cpp:18-21`, `validation.cpp:1057`).
- requestId = `SerializeHash(pair(std::string "mnhf", i64 versionBit))` (**int64**) (`src/evo/mnhftx.cpp:28,32-35`).
- msgHash = txid of the same tx with `sig` set to 96 zero bytes (`mnhftx.cpp:139-143`).
- `CheckMNHFTx` (`mnhftx.cpp:107-172`, always verifies sig, independent of assumevalid): payload version 1, `versionBit < VERSIONBITS_NUM_BITS` (`bad-mnhf-version`, `bad-mnhf-nbit-out-of-bounds`, `:94-103`); `IsValidMNActivation(bit, pindexPrev MTP)` (`bad-mnhf-non-ehf`) — **unknown bits return true** (`src/chainparams.cpp:109-129`); quorumHash in chain (`bad-mnhf-quorum-hash`); **any mined quorum of `llmqTypeMnhf`** (no "active" check) (`bad-mnhf-missing-quorum`); `VerifyInsecure(quorumPk, SignHash(type, quorumHash, requestId, msgHash))` (`bad-mnhf-invalid`).
- Block: no duplicate bit in block (`bad-mnhf-duplicates-in-block`, `mnhftx.cpp:190-206`), bit not already in signals stage (`bad-mnhf-duplicate`, `:233`), `IsValidMNActivation(bit, block MTP)` (`bad-mnhf-non-mn-fork`, `:238`) (`ProcessBlock` `:210-262`). Signals map `bit → minedHeight` stored per block under `mnhf_s2` (`:28-30,323-378`). `GetSignalsStage(pindexPrev)` is empty before V20 and drops a signal whose MTP is before its (current) deployment's nStartTime (`:61-92`). Note: no V20 gate on the tx type itself.
- Signer (masternode, `src/llmq/ehf_signals.cpp:42-139`): after V20, for every `useEHF` deployment within its window and not yet signalled, sign via `SelectQuorumForSigning(llmqTypeMnhf, requestId)` (`allowDiffMsgHashSigning=true`), then on recovery build the tx and submit to own mempool; miners include it like any tx.
- Mainnet history: MN_RR (bit 10, start 2024-01-01) → buried `MN_RRHeight=2128896` (`0de3923b06`); WITHDRAWALS (bit 11, start 2024-10-14) → buried `WithdrawalsHeight=2201472` (`77dd259004`); bit 12 (V23, later renamed V24) has **mined MNHF signals on mainnet and testnet** from before V24 had a start time, so V24 moved to bit 13 (`1be3ac299a`). A port must accept these historical bit-10/11/12 signal txs (they pass because unknown bits are "valid").

---------------------------------------------------------------------------------------------------

## 10. Historical variation & bug-for-bug list — difficulty **M**

Mainnet heights (`src/chainparams.cpp:178-197`): DIP0003 1028160 (commitment rules start; DIP6 commitments shipped in v0.13.0.0, `22b5952c5a`), DIP0003Enforcement 1047200, **DIP0008 1088640** (CbTx v2 merkleRootQuorums from 1088641), BRR 1374912, **DIP0020 1516032** (LLMQ_100_67 enabled), **DIP0024 1737792**, **DIP0024Quorums 1738698**, **V19 1899072** (basic BLS, evonodes, evo-only platform quorums, skipRemovedMNs), **V20 1987776** (CbTx v3 CL+credit pool, CL-based modifier, work-block MN list for non-rotated), MN_RR 2128896, Withdrawals 2201472. Testnet equivalents `:378-397` (DIP0024 769700/770730, V19 850100, V20 905100, LLMQ_25_67 at 847000).
LLMQ type timeline (mainnet): v0.13: 50_60, 400_60, 400_85; v0.14: CL=400_60, IS=50_60; v0.17: +100_67 (Platform); v18: +60_75 (rotated IS), Mnhf=400_85; v21: llmqTypeInstantSend removed (non-deterministic IS gone).
ChainLocks/IS enablement was by spork (SPORK_19/SPORK_2), runtime state; not derivable from code — now hard-wired active on mainnet (`spork.cpp:173-181`).

---------------------------------------------------------------------------------------------------

## 11. rust-dashcore: reusable pieces and gaps

Prefixes: `rd:` = /work/home/agent/workspace/rust-dashcore (67b307c6); `pkc:` = ~/.cargo/git/checkouts/base-sdk-bc5b311133250857/e6402ce/pkgs/pkc/src/bls/ (the `dash-pkc` dependency, rev `e6402ced…`, `rd:Cargo.toml:16`).

### 11.1 BLS
- Stack: `rd:crypto` feature `bls` → `dash-pkc/bls` (`rd:crypto/Cargo.toml:17`; `rd:dash/Cargo.toml:30-33`), which builds both schemes on **blst 0.3** (no relic, no blsful). Schemes `BlsScChia` (legacy) / `BlsScIetf` (basic) (`pkc:schemes.rs:83-95`).
- Legacy implemented in pure Rust: G1 legacy layout (`pkc:scheme_chia.rs:74-110`), G2 legacy layout `[c0,c1]`, sign of `y.c1` (`:133-201`), legacy hash-to-G2 (SHA-256 with `G2_0_c0…G2_1_c1` tags, two SvdW maps, Budroni–Pintore cofactor clearing, `pkc:chia_h2c.rs:23-70,138`), manual sk·H(m) signing (`scheme_chia.rs:214-218`). Basic DST matches Core (`pkc:scheme_ietf.rs:19`).
- Available in dash-pkc: sign/verify (`pkc:scheme_ops.rs:131,138`), insecure agg (`:226,234,320`), multi-msg verify (`:329`), **secure aggregation** with Core's weight formula (`:348-386,596-614`), threshold split/recover/pk-share/sk-share (`:437-567`), 32-byte ids read big-endian like relic (`pkc:share_id.rs:12-33`), DH (`:149`), IES single/multi (`:174-218`, `pkc:ies_ops.rs:37-62,323-421`); DKG test corpus for both schemes (`pkc:share_ops.rs:509-559`, `pkgs/pkc/corpus/bls_llmq_100.json5`, provenance undocumented).
- Exposed by rust-dashcore: raw-bytes types `BlsPkBytes/BlsSigBytes` + `BlsScheme{Legacy,Modern}` (`rd:crypto/src/bls.rs:62`), single verify (`:211`), secure-aggregate verify (`:396`), scheme re-encode (`:184`); aliases `BLSPublicKey/BLSSignature` (`rd:dash/src/bls_sig_utils.rs:9-10,36-37`). Sign/threshold/DH/IES not wrapped.
- Divergences to fix before consensus use: (a) secure-agg weight multiplied **unreduced** (`pkc:scheme_ops.rs:29`, `WEIGHT_BITS=256`) vs Core `mod r` (`src/dashbls/src/schemes.cpp:56-57`) — equal only for prime-order-subgroup keys; legacy keys are not subgroup-checked on either side; (b) dash-pkc's Chia IES derives the symmetric key from the *legacy* serialization (`pkc:scheme_ops.rs:169-178`) while Core always uses basic (`src/bls/bls_ies.cpp:47,83,100`) — use the IETF variant; (c) infinity rejected (consistent with Core's wrapper outcome, §4.3, but via a different path); (d) threshold < 2 refused (single-member quorums need special-casing as Core does); (e) the Dash wrapper's "opposite-scheme ⇒ invalid object" and "global-flag-at-height" semantics (§4.3/4.4) are not modelled; (f) off-curve legacy key acceptance (§4.3) not modelled.

### 11.2 LLMQ params/types (`rd:dash/src/sml/llmq_type/`)
- Enum 1..6 + 100..107 (`mod.rs:492-509`); rotation hard-coded set (`:669`); no `useRotation`/`keepOldKeys` fields (`:40-50`). Six mainnet/testnet types match Core numerically (`:84-127,399-465`).
- **Bugs:** `LLMQ_400_85.quorum_type = Llmqtype400_60` (`mod.rs:421-422`, verified); test/devnet param mismatches (LLMQ_TEST size 4 vs 3 `:153-162`; DEVNET keepOldConnections; DEVNET_DIP0024 window 10–18 vs 12–20 `:77-83`; TEST/DEV_PLATFORM interval 288 vs 24 `:137,145`); regtest rotation params wrong (`:650`); unknown type round-trips as 0 (`:590,644`).
- Network assignment (`network.rs:29-60`) matches CL/IS/Platform; **no Mnhf type**; enabled-type lists incomplete (`:63-93`); activation gates wrong: mainnet 100_67 at 1,888,888 vs DIP0020 1,516,032; testnet 25_67 at 1,289,520 vs 847,000 (`:144-153`); no V19/DIP0024 heights in `rd:dash-network/src/lib.rs` (V20 only, `:91`).

### 11.3 Member selection (SPV-oriented, from peer data)
- Modifier (`rd:dash/src/sml/quorum_entry/quorum_modifier_type.rs:16-115`): forms match Core; V20 test is `work >= V20` vs Core `work+1 >= V20` (harmless on mainnet/testnet).
- Score/order: `SHA256(SHA256(proTx‖confirmed)‖modifier)` (`rd:…/qualified_masternode_list_entry.rs:55-64`, `rd:dash/src/hash_types.rs:279-291`), numeric ordering; but sorted via `BTreeMap<ScoreHash,…>` ⇒ no collateral tiebreak, equal scores collapse (`rd:dash/src/sml/masternode_list/scores_for_quorum.rs:50-73`); Evo-only filter **without V19 gate** (`:24,37`).
- Non-rotated (dash-spv): always MN list at h−8 and the work hash for pre-V20 modifier (`rd:dash-spv/src/sml_engine/validation.rs:29-48`) — wrong for pre-V20 quorums (Core: list at base, base hash).
- Rotation (`rd:dash-spv/src/sml_engine/rotated_quorum_construction.rs`): snapshot quarters modes 0/1 match Core (`:231-267`); **mode 2 diverges** (fills vs Core empty, `:268-280`); new quarter lacks `enabled() < quarterSize` exit, skip-list generation, snapshot building/storage, and always applies skipRemovedMNs (pre-V19 mainnet cycles 1,737,792–1,899,072 would differ) (`:284-321`, `rotated_quorums_info.rs:144`); does not model `AddMN` uniqueness exceptions. Rotation CL sigs come from `mnlistdiff.quorumsCLSigs` rather than CbTx (`rd:dash-spv/src/sml_engine.rs:377-400`).
- Snapshot sizing quirk to copy: Core sizes `activeQuorumMembers` to `allMns.GetCounts().total()` (incl. banned) but indexes only valid+confirmed MNs (`src/llmq/utils.cpp:317-331`).

### 11.4 Commitments, CL, IS, CbTx, MNHF
- `QuorumEntry`/type-6 payload wire format correct (`rd:dash/src/blockdata/transaction/special_transaction/quorum_commitment.rs:41-52,120-218`). Commitment hash uses the received 48 key bytes (`rd:dash/src/sml/quorum_entry/hash.rs:40-70`) — equals Core whenever version scheme == era scheme.
- Validation gaps: dash-spv hard-codes `BlsScheme::Modern` (`rd:dash-spv/src/sml_engine/validation.rs:90-96`, verified) ⇒ v1/v2 legacy commitments (incl. the 24 stale 50_60 ones still in today's SML!) fail; undecodable operator keys are silently dropped from the aggregate (`rd:crypto/src/bls.rs:423-426`); counts checked vs threshold instead of minSize; missing size==params.size, vvecHash non-null, version-vs-era, zero-bits-beyond-members checks, null commitments (`quorum_commitment.rs:68-113`); **bitset decoder accepts non-zero padding bits** (`rd:dash/src/consensus/encode.rs:1077-1101`, verified) — a consensus divergence (Core throws).
- ChainLock/ISDLock structs, request ids, sign hashes, non-rotated selection (memcmp min) and the rotated `b >> (64-n-1)` quirk are correct (`rd:dash/src/ephemerealdata/chain_lock.rs:28-75`, `instant_lock.rs:22-79`, `rd:dash/src/sml/masternode_list/quorum_helpers.rs:10-24`, `rd:dash-spv/src/validation/instantlock.rs:96-104`); verification Modern-only.
- CbTx struct OK (field `best_cl_height` is really `bestCLHeightDiff`; balance typed u64 vs Core i64) (`coinbase.rs:16-24`). `merkleRootQuorums` computed from SML entry hashes only (`merkle_roots.rs:140-185`) — correct for SML-derived sets (confirmed above) but no mined-commitment logic; `has_valid_*_root` uncalled.
- MNHF payload parse only (`mnhf_signal.rs:12-17`); no request id/sig/EHF state.
- dash-spv trusts MN lists (doesn't verify mnlistdiff merkle proof/CbTx roots, `rd:dash-spv/src/sync/masternodes/pipeline.rs:167-210`), keeps best CL without fork-choice enforcement (`sync/chainlock/manager.rs:154-315`), IS without conflict handling (`sync/instantsend/manager.rs:136-358`).

### 11.5 Summary table
| Reusable | Gap |
|---|---|
| dash-pkc: both schemes' encodings, legacy hash-to-G2, sign/verify, secure agg, threshold, DH, IES | wrap & fix weight reduction, IES key scheme; model Dash-wrapper parse semantics + global-flag rule |
| wire structs: QC/type-6, CbTx, MNHF, QRInfo/snapshot, CLSIG, ISDLOCK | DKG/sig-share/recsig/qwatch/mnauth messages absent (`rd:dash/src/network/message.rs:332-337`) |
| request ids, SignHash, quorum selection (both kinds) | full DMN list engine, mined-commitment DB, block QC rules, merkle-root checks |
| score/modifier math, snapshot quarter decode (modes 0/1) | snapshot build/store, skip-list gen, AddMN-uniqueness, pre-V19/pre-V20 rules, mode 2 |
| 6 mainnet LLMQ param sets | 400_85 type bug, test/devnet params, Mnhf type, activation heights |

---------------------------------------------------------------------------------------------------

## 12. Port plan notes / what assumevalid buys
- Under assumevalid (`fScriptChecks=false`, `src/validation.cpp:2543-2569`) Dash skips: CbTx payload/merkle-root/bestCL checks, commitment BLS signatures, ProTx signatures (`src/evo/specialtxman.cpp:810-836,872,897`). **Still required:** commitment structure + `GetAllQuorumMembers` (bitset bounds and PoSe punishment), snapshots, EHF signature verification (always on, basic scheme), legacy/basic *parsing* semantics. ⇒ A first milestone can sync mainnet with assumevalid using basic-scheme BLS only, provided member selection (incl. rotation) is bit-exact; legacy BLS verification is only needed for full validation of 1028160..1899071.
- Rotation snapshots are derived node state: must be produced in order from DIP0024 activation; a node bootstrapping from a UTXO/evo snapshot must import them.

## 13. Top gotchas (summary)
1. Global BLS scheme flag = legacy iff validating block h < V19Height; it affects commitment hash pubkey bytes, secure-aggregation weights, hash-to-curve, and unversioned sigs (§2.2, §4.4).
2. Wire BLS bytes canonical in the *other* scheme decode to an *invalid* (null) object, not an error; infinity/garbage throws; legacy keys aren't curve/subgroup-checked (§4.3).
3. `merkleRootQuorums` includes all configured types incl. disabled ones: 24 stale v1 LLMQ_50_60 commitments are still leaves on mainnet (§2.5, verified).
4. Null commitments of known-but-disabled types are accepted at any height (null path returns before mining-phase check; counts only enforced for enabled types) (§2.4).
5. Rotation: missing prev-cycle snapshot ⇒ `break` (smaller quorums for first 3 cycles; DIP0024QuorumsHeight = DIP0024Height+3·288+42); `AddMN` uniqueness exceptions silently drop MNs; skip-list "first index 0" encoding quirk; skipRemovedMNs only from V19 (or testnet) (§3.4).
6. V20 boundary mixes rules: non-rotated MN list from base-8 when V20 active after base, but modifier uses old base-hash form until V20 active after base-8 (§3.3); V20 modifier uses the work block's *coinbase* CL sig, read from disk.
7. `uint256` comparisons are memcmp (quorum-for-signing score, merkle leaf sort, outbound selection) while MN scores are numeric arith_uint256 (§conventions, §3.1, §5.1).
8. Rotated signer index = bits 58..62 of the last 8 LE bytes of the request id (`b >> (64-n-1)`), not the top bits (§6.2).
9. Commitment validity uses `signers >= minSize` (not threshold), exact bitset sizes, zero padding bits, zero bits beyond member count; version fixed by quorum base block V19/rotation state (§2.3).
10. ChainLock/IS enforcement in ConnectBlock/header acceptance is node-local (P2P CLSIG/islocks, sporks; IS check skipped in IBD) and must not be treated as deterministic consensus; only CbTx bestCL (V20+) is deterministic (§5.2, §6.3). EHF: any mined LLMQ_400_85 quorum may sign; unknown bits are valid; historical bit-10/11/12 signals exist on mainnet (§9).
