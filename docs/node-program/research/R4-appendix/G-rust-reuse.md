> **EMBARGO-1:** an unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per PLAN §7). Scrubbed by A1-rev1 on 2026-10-08.

> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# G: What rust-dashcore already provides for a full validating node and masternode (consensus inventory)

Snapshots: rust-dashcore `dev` @ `67b307c6`. Dash Core `develop` @ `15db478750`, which is past `v24.0.0-rc.2` and already contains the Decentralized Masternode Shares (DMS) DIP (#7437), asset-unlock v2 stable txids and CbTx v4.
Method: I read the code, then ran empirical checks in a scratch crate at `/tmp/r4-g-check`. That crate has path-deps on the repo, uses its own `target/`, and leaves the repo untouched (`git status` clean). All `file:line` references are in rust-dashcore unless they are prefixed `core:`.

---

## 0. TL;DR

- **What rust-dashcore is:** a fork of rust-bitcoin's *data-structure* layer, plus a good Dash SPV stack: headers, BIP157/158 filters, MN-list sync via `mnlistdiff`/`qrinfo`, quorum verification, ChainLock and ISLock verification, and a wallet. It has **no validation engine**:
  - no script interpreter, no sigop counting, no UTXO set, no difficulty adjustment (DGW/KGW), no subsidy, MN-payment or superblock rules;
  - no deterministic-MN (DMN) state machine, no special-tx semantic validation, no credit pool, no MNHF/EHF, no versionbits, no mempool policy, no RPC server, no inbound P2P.
- **Reusable with high confidence, empirically confirmed on mainnet data:**
  - SML entry hashing and the `merkleRootMNList` computation;
  - `merkleRootQuorums` (from an SML-style quorum set);
  - LLMQ member scoring and selection, DIP-24 rotated quarter reconstruction from snapshots;
  - ChainLock and ISDLOCK request-id / sign-id / quorum selection;
  - BLS legacy (Chia) and basic (IETF) verification via `dash-pkc` on blst;
  - X11 via C `sph` bindings.
- **Structural problem for bit-exactness:** the transaction parser is *lossy and stricter than Core*. It re-encodes structs to compute txids, ignores the special-payload length prefix, types `version` differently from Core, keeps Bitcoin segwit parsing, and collapses unknown LLMQ types to 0. I proved three concrete divergences: valid-in-Core txs that Rust cannot parse, and v24 CbTx v4 being mis-parsed (§2.1). A full node must keep `vExtraPayload` raw, the way Dash Core and `dashpay/base-sdk`'s `dash-primitives` do.
- **Behind Dash Core develop (v24):**
  - no tx types 10–12 (ProviderDissolve, UpdateShare, UpdateSharedRegistrar);
  - no ProTx v3 (ExtAddr / NetInfo / payout lists / shares) in tx payloads;
  - no CbTx v4 (`merkleRootAssetUnlocks`);
  - no asset-unlock v2 stable-txid hashing;
  - no special-tx elements in BIP158 filters (in Core since v23);
  - `PROTOCOL_VERSION` is 70237 versus Core's 70242.
- **Parameters are partly wrong:**
  - `dash/src/consensus/params.rs` is still **Bitcoin's** (BIP34 height 227931, 10-minute spacing).
  - `Work::MAINNET_MIN` is Bitcoin's pow limit.
  - The test and devnet LLMQ params differ from Core in several fields.
- **Test-data caveat (new finding):**
  - `dash/tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin` and `dash/artifacts/mn_list_diff_testnet_0_1296600.bin` are *not Core-produced*: their IPv4 services use the IPv4-compatible form (`::a.b.c.d`) instead of Core's IPv4-mapped form (`::ffff:a.b.c.d`), so their CbTx `merkleRootMNList` does not match their own list.
  - The test `entry_hash_matches_core_calc_hash` (`dash/src/sml/masternode_list_entry/hash.rs`) uses this fixture, so its "Core ground truth" is suspect.
  - The genuine Core wire fixtures (`DML_0_2221605.hex`, `QR_INFO_0_2224359.hex`) do match (§3.6).

---

## 1. Workspace crates and consensus-relevant dependencies

Root members, from `Cargo.toml:2`: `dash, crypto, dash-network, hashes, internals, fuzz, rpc-client, rpc-json, rpc-integration-test, key-wallet, key-wallet-manager, key-wallet-ffi, dash-spv, dash-spv-ffi, git-state, dash-network-seeds, masternode-seeds-fetcher, dash-spv-bench`. Edition 2024, MSRV 1.89, version 0.45.0.

| Crate | Purpose | Consensus relevance |
|---|---|---|
| `dash` (`dashcore`) | Blocks, txs, special payloads, script types, network messages, SML/LLMQ, ChainLock/ISLock, BIP158/152/37, sighash, pow | Core of reuse; ~121 files |
| `crypto` (`dashcore-crypto`) | Wrappers over `dash-pkc` for BLS, ECDSA, EdDSA, keys, sighash types | BLS verify paths |
| `dash-network` | `Network` enum, magics, ports, `v20_activation_height()` only | Minimal params |
| `hashes` (`dashcore_hashes`) | rust-bitcoin hashes plus `hash_x11` (feature `x11`) | X11 |
| `dash-spv` | SPV client: sync managers, storage, network, `MasternodeListEngine` (`dash-spv/src/sml_engine.rs`, 1850 lines), validation | Networking and SML engine reuse; dashd test harness |
| `key-wallet`, `key-wallet-manager` | HD wallet (BIP32/39/44, DIP9/13/14/15, BLS and ed25519 derivation), tx building, filter matching | Masternode key management (owner, voting, operator BLS, platform ed25519) |
| `rpc-json`, `rpc-client` | Dash RPC types and a client (incl. `protx`, `quorum`, `masternode`, `getbestchainlock`, `verifychainlock`, `verifyislock`) | Differential-harness asset |
| `rpc-integration-test` | Stale: `run.sh` still launches `bitcoind` | Low |
| `fuzz` | 17 honggfuzz targets, mostly rust-bitcoin heritage (deserialize tx/block/script/addr, net msg, hashes) | Low; no differential targets |

Key dependencies:

- **secp256k1 `0.33.1`** (rust-secp256k1, C libsecp256k1), used via `bitcoin-crypto` at a pinned rust-bitcoin rev and via `dash-pkc` (`Cargo.toml` workspace deps). It has lax DER parsing available, but nothing in-repo uses it for consensus.
- **BLS: `dash-pkc`**, from `dashpay/base-sdk` rev `e6402ce`, built on **`blst`**, with two schemes:
  - `BlsScChia`: the legacy, pre-V19 scheme, compatible with the Chia bls-signatures fork that Dash Core used.
  - `BlsScIetf`: the basic, post-V19 scheme.
  - rust-dashcore's `crypto/src/bls.rs` exposes `BlsScheme::{Legacy, Modern}`, `verify`, `verify_secure_aggregate`, `reencode` (legacy↔basic), `add_tweak` and `canonicalize` (`crypto/src/bls.rs:60-67`, `:211-235`).
  - `dash-pkc` itself has much more: secret-key shares and `derive_share`, threshold `recover_shares`, `secure_aggregate`, `verify_aggregates`, IES encryption (`ies_encrypt_multi`, needed for DKG contributions) and PoP (`pkgs/pkc/src/bls/{share_ops,sig_threshold,sig_aggregate,ies_ops,sig_pop}.rs`). That is a strong base for a masternode's DKG and signing.
  - It is not the old `blsful` and not Dash's `bls-signatures` C++ fork. Older checkouts of both exist in `~/.cargo/git` but are unused here.
- **X11: `rs-x11-hash` 0.1.9** (`dashpay/rs-x11-hash`), which is **C (sph) via `cc`**. `BlockHash` is X11 only when the `core-block-hash-use-x11` feature is on, otherwise sha256d (`dash/src/hash_types.rs:85-93`), which is a footgun. `base-sdk`'s `dash-pow` has a pure-Rust X11 with SIMD, but it is unused here.
- **blake3 1.8** is unused for consensus. **bitcoinconsensus** is an optional feature that wires Bitcoin's libbitcoinconsensus into `Script::verify_with_flags` (`dash/src/blockdata/script/borrowed.rs:399-430`). That is **wrong for Dash**: no DIP-0020 opcodes, Bitcoin tx parsing, and payload-bearing txs fail to deserialize. Treat it as dead or harmful.
- **Related asset not used by rust-dashcore:** `dashpay/base-sdk` (same rev) has `dash-primitives`, `dash-params`, `dash-pow`, `dash-script` (opcodes, sigops) and `dash-p2p-core` (BIP324). Its `Transaction` keeps `extra_payload: Vec<u8>` raw with `version` as i16 (`pkgs/primitives/src/transaction.rs:181-209`) and has `TxInvalid` reasons mirroring Core strings. Architecturally it is closer to bit-exact parsing and should be evaluated alongside.

---

## 2. `dash` crate: primitives, encoding, script, PoW, params

### 2.1 Transaction serialization: works on real chain data, but is not Core-exact

The struct is `Transaction { version: u16, lock_time, input, output, special_transaction_payload: Option<TransactionPayload> }` (`dash/src/blockdata/transaction/mod.rs:156-167`).

**Core reference.** `core:src/primitives/transaction.h:253-277, 346-364`: `nVersion` and `nType`, read as one int32. `vExtraPayload` is a **raw byte vector**, read only if `nVersion >= 3 && nType != 0`. Payload structs are decoded later by `GetTxPayload`, which requires full consumption.

| # | Rust behavior | Core behavior | Consequence |
|---|---|---|---|
| a | Reads payload whenever `version != 0` and type ≠ 0 (`mod.rs:606-619`) | Payload only if `nVersion >= 3` | **Demonstrated:** a tx with `nVersion=1,nType=5` (valid as a normal tx pre-DIP3 in Core; `core:validation.cpp:355-378` only rejects after DIP3) fails in Rust with `UnexpectedEof`. Only `version==0` with nonzero type bytes is special-cased, via `ClassicalWithNonStandardVersionTypeBytes`. A full IBD must check whether such historical txs exist. |
| b | Special-payload length prefix is **read and discarded** (`special_transaction/mod.rs:428-431`). The struct is parsed directly off the stream, and `txid`/encode **recompute** the length from the re-encoded struct (`mod.rs:196-219`, `:584-596`). | Raw `vExtraPayload` with exact length; the hash is over the raw bytes | Any payload Rust cannot model exactly (trailing bytes, newer versions) mis-frames the rest of the block or changes the txid. **Demonstrated with CbTx v4 (v24):** Rust consumes 237 of 269 bytes; the 32-byte `merkleRootAssetUnlocks` is left in the stream, so a block containing it fails to parse. |
| c | Unknown `nType` with v≥3 → parse error `UnknownSpecialTransactionType` (`special_transaction/mod.rs:378`) | Parses; rejected later with `bad-txns-type` | Same verdict, different layer. But types **10, 11, 12** (DMS, in v24) are unknown to Rust. |
| d | rust-bitcoin segwit framing retained: 0 inputs ⇒ expect marker/flag except for types 5, 6, 7, 9 (`mod.rs:550-570`, `:621-660`) | No segwit in Dash | 0-input non-special txs are mis-parsed (they are invalid anyway). A Bitcoin segwit block still parses in Rust: the test fixture `mainnet_block_0000…c835b….raw` is a Bitcoin block. Remove for a node. |
| e | `LLMQType` decode maps unknown u8 → `LlmqtypeUnknown`, which re-encodes as 0 (`sml/llmq_type/mod.rs:573-591`) | Raw byte kept | Re-encoded QcTx txid changes. |
| f | DYNBITSET padding bits are dropped on decode and re-emitted as 0 (`consensus/encode.rs:1077-1100`) | Core parses without checking padding, but hashes raw bytes | A QcTx with nonzero padding is Core-valid while Rust computes a different txid, hence a merkle mismatch. Theoretical split. |
| g | ProTx payloads: unknown version still parsed as v1/v2 layout (ProReg, ProUpReg) | `nVersion==0 \|\| > max` ⇒ "bail out early", then `GetTxPayload` fails | Same verdict. |

**Verdict.** Round-trips are fine on all the real data I tried: 6 mainnet MnListDiff/QRInfo coinbases, and the diff fixtures reserialize byte-identically. Exactness is not guaranteed by construction. **Recommendation:** keep `version` and `nType` as Core types them, store `vExtraPayload: Vec<u8>` raw, hash Core's re-serialization (A1 D7), and decode payloads lazily (as `base-sdk` and Core do).

### 2.2 Special-transaction payload coverage vs `core:src/evo/*.h`

| Type | Rust file | Versions covered | Core develop (v24) | Gap |
|---|---|---|---|---|
| 1 ProRegTx | `special_transaction/provider_registration.rs` | v1 legacy BLS, v2 basic (Evo: `platformNodeID, p2pPort, httpPort` after `inputsHash`, `:228-232`) | v3 ExtAddr: `NetInfoSerWrapper(ext)`, then **payouts list (u8 count; script, u16 reward)**, **shares (u8 count; amount, refund, reward, keyIDOwner)**, join sigs (65-byte compact), `nEarlyPeriodBlocks`, `nEarlyPenalty`, replacing `scriptPayout`; Evo v3 drops ports (`core:evo/providertx.h:160-225`) | **v3 missing entirely**. The service is a fixed 18-byte `SocketAddr`. |
| 2 ProUpServTx | `provider_update_service.rs` | v1, v2 (nType only for v2, Evo platform fields) | v3: ExtNetInfo; Evo v3 has only `platformNodeID` | v3 rejected explicitly (`:182-184`). |
| 3 ProUpRegTx | `provider_update_registrar.rs` | v1/v2 | v3 payouts list | v3 missing |
| 4 ProUpRevTx | `provider_update_revocation.rs` | v1/v2 | same | OK |
| 5 CbTx | `coinbase.rs` | v1 (h, mnroot), v2 (+qroot), v3 (+compactsize `bestCLHeightDiff`, 96-byte sig, i64 `creditPoolBalance`). Field order matches `core:evo/cbtx.h:56-69`. | **v4 `merkleRootAssetUnlocks`** | v4 missing; v4 is mis-parsed (see 2.1b). `CURRENT_VERSION=3`. |
| 6 QcTx | `quorum_commitment.rs` | payload `{u16 ver, u32 height, CFinalCommitment}`; commitment v1–v4 with `quorumIndex` for v2/v4 (`:144-170`), matching `core:llmq/commitment.h:95-114` | same | Legacy vs basic encoding is only a wrapper on raw bytes, which is fine for parsing. |
| 7 MNHF | `mnhf_signal.rs` | `{u8 ver, u8 versionBit, quorumHash, sig}` | same | OK |
| 8 AssetLock | `asset_lock.rs` | `{u8 ver, Vec<TxOut>}` | same | OK |
| 9 AssetUnlock | `asset_unlock/*` | `{u8 ver, u64 index, u32 fee, u32 requestedHeight, quorumHash, sig}` | Payload same, but **v2 txid = hash with the last 132 bytes zeroed** (`core:primitives/transaction.cpp:79-95`, `ComputeStableUnlockHash`), plus `GetInstanceHash` and a CbTx v4 commitment | Stable txid missing. |
| 10/11/12 | none | ProviderDissolve, ProviderUpdateShare, ProviderUpdateSharedRegistrar | — | Missing |

**Spot checks:**

- **CbTx v3 field order:** `version(u16), height(i32 vs u32 in Rust), merkleRootMNList, merkleRootQuorums, COMPACTSIZE(bestCLHeightDiff), bestCLSignature(96), creditPoolBalance(i64)`. Matches Rust `coinbase.rs:93-121`.
- **ProRegTx v2 Evo:** Core writes `platformNodeID, platformP2PPort, platformHTTPPort` after `inputsHash`, before `vchSig` (`core:providertx.h:214-222`). Matches Rust `:228-232`.
- **CSimplifiedMNListEntry:** see §3.2.
- **CService (`dash/src/sml/address.rs:7-49`):** 16-byte IPv6 (IPv4-mapped) + big-endian port. Lossless for 16-byte forms since fix `e9f38164`.

### 2.3 Script, sighash, sigops

- **No interpreter.** There is no `EvalScript`/`VerifyScript` equivalent. The only verify path is the `bitcoinconsensus` feature (wrong for Dash, see §1).
- **Opcodes are Bitcoin's:**
  - `0xba` is named `OP_CHECKSIGADD` (Dash: `OP_CHECKDATASIG`), and `0xbb` is "OP_RETURN_187" (Dash: `OP_CHECKDATASIGVERIFY`) (`dash/src/blockdata/opcodes.rs:264-265`).
  - `OP_CAT/SPLIT(0x7f)/NUM2BIN/BIN2NUM/AND/OR/XOR/DIV/MOD` are documented as "disabled", but they were re-enabled by DIP-0020 (`core:script/script.h:117-123,194-195`; `opcodes.rs:202-227`).
  - Classification logic treats them as returns/disabled (`opcodes.rs:358-395`).
- **Legacy sighash:** `SighashCache::legacy_signature_hash` (`dash/src/crypto/sighash.rs:618-765`) follows Core's `SignatureHash` structure:
  - ANYONECANPAY / NONE / SINGLE handling, the SIGHASH_SINGLE "one" bug, and it includes the special payload by re-encoding the tx.
  - **It does not strip `OP_CODESEPARATOR`** from scriptCode (the doc admits this), and it inherits the re-encode risks from §2.1.
  - Only rust-bitcoin's `legacy_sighash.json` vectors are tested (`sighash.rs:960`). Dash Core's own `src/test/data/sighash.json` differs (503 vs 292 lines) and is not used.
  - Segwit and taproot sighash code is dead weight.
- **Sigop counting:** none. Only `policy.rs` Bitcoin constants (`MAX_BLOCK_SIGOPS_COST` etc.) exist. `base-sdk`'s `dash-script/src/sigops.rs` exists.

### 2.4 Merkle

- `merkle_tree::calculate_root` (`dash/src/merkle_tree/mod.rs:71`) matches Core's `ComputeMerkleRoot` (duplicate-last), but has **no CVE-2012-2459 mutation detection** (Core's `BlockMerkleRoot(&mutated)` leads to `bad-txns-duplicate`, and `mutated-calc-cb-mnmerkleroot` for the SML).
- BIP37 `PartialMerkleTree`/`MerkleBlock` (`merkle_tree/block.rs`) are fine.

### 2.5 PoW, X11, difficulty, params, subsidy

- **Header hash** = X11 over the 80-byte header (`block.rs:59-63`).
- **`Target::from_compact`** (`dash/src/pow.rs:162-183`) is rust-bitcoin's. It returns ZERO for negative values but **does not signal overflow** (Core `SetCompact` sets `fOverflow`, and `CheckProofOfWork` rejects `fNegative || ==0 || fOverflow || > powLimit`). There is no `powLimit` check anywhere.
- **`Work::MAINNET_MIN`/`TESTNET_MIN` = `0x00000000ffff…`** (Bitcoin), while Dash `powLimit = 0x00000fff…` (`pow.rs:86-95`; `core:chainparams.cpp:198`).
- **Work** (`to_work` = `~t/(t+1)+1`) matches Core's `GetBlockProof`. `dash-spv/src/chain/chain_work.rs` accumulates it.
- **No DGW v3, no KGW, no pre-KGW retarget.** `consensus/params.rs:50-125` is verbatim Bitcoin:
  - `bip34_height: 227931`, 10-minute spacing, 2-week timespan, threshold 1916/2016;
  - Dash values are BIP34 at 951, spacing 2.5 minutes, KGW at 15200, DGW at 34140, threshold 1815, and so on (`core:chainparams.cpp:178-206`).
  - Only `pow.rs:16` imports it.
- **Activation heights:** only V20 is present (`dash-network/src/lib.rs:91-97`). DIP0001/3/8/20/24, BRR, V19, MN_RR, deployments and EHF are all missing.
- **Genesis headers** for mainnet and testnet are Dash-correct (`blockdata/constants.rs:111-180`). Devnet genesis does not model Core's named-devnet second genesis.
- `SUBSIDY_HALVING_INTERVAL = 210_000` and `MAX_BLOCK_WEIGHT` are Bitcoin constants. **No subsidy, MN payment, superblock or treasury logic.**

---

## 3. SML / LLMQ (`dash/src/sml`, `dash-spv/src/sml_engine*`)

### 3.1 Scope

What exists:

- A *simplified* masternode list built from `mnlistdiff` (`from_diff.rs`, `apply_diff.rs`). Masternodes are keyed by reversed proTxHash.
- Quorum maps per type with verification status.
- LLMQ params and network mappings (`llmq_type/mod.rs`, `llmq_type/network.rs`).
- Quorum modifier (`quorum_entry/quorum_modifier_type.rs`), scoring (`masternode_list/scores_for_quorum.rs`), commitment hash and validation (`quorum_entry/{hash,validation}.rs`), request→quorum selection (`masternode_list/quorum_helpers.rs`).
- DIP-24 reconstruction in dash-spv: `sml_engine/rotated_quorum_construction.rs` and `cycle_quarter_work_heights`/`rotated_cycle_base_height` (`sml_engine.rs:112-144`).

What is missing: **no `CDeterministicMNList` state machine.** There is no processing of ProRegTx/ProUpServ/ProUpReg/ProUpRev/collateral spends, no PoSe penalties or bans, no `confirmedHash` assignment, no payment queue (`nLastPaidHeight`), no `BuildQuorumSnapshot`, no `CQuorumBlockProcessor` (mined commitments, active set), and no DKG.

### 3.2 SML entry encoding and hash: exact

- `MasternodeListEntry::consensus_encode_body` (`masternode_list_entry/mod.rs`) writes: proRegTxHash, confirmedHash, netInfo (legacy `CService`, or `ExtNetInfo` if v≥3), operator key (raw 48 bytes), keyIDVoting, isValid; then, if v≥2, nType(u16); if Evo, `platformHTTPPort` (only v<3) then `platformNodeID`.
- That equals `core:evo/simplifiedmns.h:72-100` under SER_GETHASH (no leading nVersion). `calculate_entry_hash` hashes the body only (`hash.rs:6-10`).
- `ExtNetInfo` (`net_info.rs`) mirrors `core:evo/netinfo.h` (version short-circuit, purpose map, Service/Domain/Invalid entries, BE ports).
- **Empirical check against Core's unit-test vector** (`core:src/test/evo_simplifiedmns_tests.cpp:19-80`, 15 synthetic legacy entries): **15/15 entry hashes and the merkle root `0bae2176…4022` match exactly** (probe3).

### 3.3 merkleRootMNList and merkleRootQuorums

- `calculate_masternodes_merkle_root` sorts by raw proTxHash bytes (memcmp, equal to Core `uint256::Compare`, `core:uint256.h:55`) and applies the duplicate-last merkle (`merkle_roots.rs:122-162`). The empty list returns `None`; Core returns the zero hash.
- **Empirical check:** on 6 genuine Core wire messages (`DML_0_2221605.hex`, and `mn_list_diff_{tip,h,h-c,h-2c,h-3c}` in `QR_INFO_0_2224359.hex`, mainnet heights 2221605–2224359), **both the MN root and the quorum root match the CbTx in the same message, 6/6.**
- The quorum root here is computed from the *SML's quorum set*. Core computes it from `CQuorumBlockProcessor::GetQcHashes` plus commitments in the current block, with per-type `signingActiveQuorumCount` truncation and indexed rotation slots (`core:evo/cbtx.cpp:~70-130`). A full node needs that logic; only the final sort+merkle is reusable.
- **The SPV client never checks these roots against the CbTx**, and never verifies the `cbTxMerkleTree` proof against a header: `from_diff.rs:102` just stores `merkle_hashes.first()`, and `has_valid_mn_list_root` (`merkle_roots.rs:60-74`) is unused outside the swift SDK history.

### 3.4 Member selection (non-rotated)

- **Score:** `sha256(confirmedHashWithProRegTx ‖ modifier)` with single SHA-256, matching `core:llmq/utils.cpp:72-85`. `ConfirmedHashHashedWithProRegTx` is sha256 (`hash_types.rs:147`).
- **Ordering:** `ScoreHash` has a custom reversed-byte `Ord`, equal to `arith_uint256` comparison (`hash_types.rs:156-175`). The selection is descending top-`size`. The Core tie-break by collateral outpoint is missing (it is irrelevant in practice, but a BTreeMap would *drop* a tied entry).
- **Modifier:**
  - `PreCoreV20(type, blockHash)` = `SerializeHash(pair(type, hash))`; `CoreV20(type, height, clsig)`. Both match `core:utils.cpp:87-112`.
  - **But** the V20 switch uses `work_block_height >= v20_height` (`quorum_modifier_type.rs:110`), while Core uses `DeploymentActiveAfter(pWorkBlockIndex)`, i.e. `work_height + 1 >= V20Height`. That is an off-by-one, harmless on historical mainnet cycles.
  - dash-spv validates non-rotated quorums always with **list at `height-8` and work-block modifier** (`dash-spv/src/sml_engine/validation.rs:29-50`). Core pre-V20 uses the **quorum base block's list and hash** (`core:utils.cpp:714-724`). It is fine for SPV (post-V20 only), but not for genesis replay.
- **EvoOnly:** `llmq_type == platform_type()`, without Core's V19 gate (`scores_for_quorum.rs:24`).
- **Commitment verification** (`quorum_entry/validation.rs`): `verify_secure_aggregate` over signer keys, plus a threshold sig over `BuildCommitmentHash` (`hash.rs:41-69` matches `core:llmq/commitment.cpp:312-323`).
  - The single-member `VerifyInsecure` special case (`core:commitment.cpp:69-74`, `is_single_member()`) is missing; dash-pkc's secure aggregate weights even one key.
  - SPV verification hard-codes `BlsScheme::Modern` (`dash-spv/src/sml_engine/validation.rs:89-97`).
  - Core's `CBLSPublicKey` hashing in `BuildCommitmentHash` uses the *global* legacy flag, which must be modelled for pre-V19 replay.

### 3.5 DIP-24 rotation

- Quarter reconstruction from `QuorumSnapshot` matches `core:llmq/utils.cpp:184-285` for modes 0 (NoSkipping) and 1 (SkipFirst: relative skip-list decode, circular index).
- The new-quarter algorithm ("New" with used-by-index) mirrors `ComputeQuorumMembersByQuarterRotation`.
- **Divergence:** mode 2 `MODE_NO_SKIPPING_ENTRIES` returns the kept entries in Rust (`rotated_quorum_construction.rs:268-279`), while **Core returns empty** (`core:utils.cpp:283-285`). Core never emits mode 2, so this is latent.
- **Missing for a full node:** `BuildQuorumSnapshot` / skip-list construction, the `predicting=true` path, and snapshot persistence (`core:evo/snapshot.cpp`, 387 lines).

### 3.6 LLMQ params vs `core:llmq/params.h`

Mainnet types 50_60, 60_75, 400_60, 400_85, 100_67 and 25_67 match on size, minSize, threshold, dkg interval, phases, window and bad-votes. Discrepancies:

- `LLMQ_TEST.size = 4` (Core 3).
- `LLMQ_TEST_PLATFORM` interval 288 (Core 24).
- `LLMQ_DEVNET_DIP0024` mining window 10/18 (Core 12/20).
- `LLMQ_DEV_PLATFORM` interval 288 (Core 24), recovery 3 (Core 6), keepOldConnections 4 (Core 5).
- `LLMQ_DEVNET` keepOldConnections 4 (Core 5).
- `LLMQ_400_85.quorum_type = Llmqtype400_60` copy-paste bug (`llmq_type/mod.rs:422`; currently unused).
- No `useRotation`/`keepOldKeys` fields.
- `LlmqtypeUnknown.params()` returns devnet params (`:557`).
- `LLMQ_NONE` is 0xff in Core, 0 in Rust.

### 3.7 Reusable test vectors (all mainnet unless noted)

`dash/tests/data/test_DML_diffs/`:

- `DML_0_2221605.hex`: raw `mnlistdiff` P2P message (genuine, roots verified).
- `QR_INFO_0_2224359.hex` (5.2 MB): raw `qrinfo` (genuine, 5 diffs verified).
- `qrinfo_core231_paloma.hex`: a devnet/testnet "paloma" QRInfo from Core 23.1.
- `mn_list_diff_0_2227096.bin` and `mn_list_diff_2227096_2241332.bin`: **NOT Core-exact.** IPv4 is in IPv4-compatible form, so the MN root mismatches the CbTx. The quorum root still matches.
- `masternode_lists_2243493.bin`, `mnlistdiffs_2240504.dat`, `qrinfo_2240504.dat`, `qrinfo_2518986.dat`, `block_container_{2240504,2518986}.dat`: dash-spv engine fixtures. They are bincode/serde-serialized Rust structs; provenance is unverified.

Elsewhere:

- `dash/artifacts/mn_list_diff_testnet_0_1296600.bin` (testnet): same IPv4-compatible corruption, MN root mismatches.
- `dash/tests/data/blockfilters.json`: **identical to Core's** `src/test/data/blockfilters.json`.
- ChainLock mainnet fixture pair, plus ISLock and rotated-cycle fixtures in `dash/src/test_utils/{chainlock,instantlock,sml}.rs`, used by the `dash-spv/src/validation/*` tests.
- Core's own `src/test/data/{sighash,script_tests,tx_valid,tx_invalid}.json` are **not** imported, and they should be.

---

## 4. ChainLock, InstantLock, BLS utilities

- **`ChainLock {u32 height, blockHash, sig}`** (`ephemerealdata/chain_lock.rs`):
  - request id = `SHA256d(compact("clsig") ‖ height_le)`;
  - sign id = `SHA256d(type_u8 ‖ quorumHash ‖ requestId ‖ blockHash)`.
  - Both match Core's `CChainLockSig` and `BuildSignHash`.
- **`InstantLock {u8 version, Vec<OutPoint>, txid, cyclehash, sig}`**: ISDLOCK only. Request id = `SHA256d("islock" ‖ inputs)`, matching `core:instantsend/lock.cpp:18-24`.
  - Core develop adds index-based locking for v2 asset unlocks (commit `22af9d647e`), which is not modelled.
- **Quorum selection:**
  - Non-rotated: min over `SHA256d(type ‖ quorumHash ‖ requestId)` with memcmp ordering (`quorum_helpers.rs:12-25`, `hash_types.rs:313-319`).
  - Rotated: `(1<<n)-1 & (GetUint64(3) >> (64-n-1))` (`dash-spv/src/validation/instantlock.rs:96-100`).
  - Both match Core's `SelectQuorumForSigning`. The ChainLock validator tries the lists before and after (`dash-spv/src/validation/chainlock.rs:47-63`).
- **`bls_sig_utils.rs`** (37 lines) re-exports `dashcore_crypto::bls` and adds 48/96-byte consensus encode. BLS bytes are kept raw everywhere, which is good for exactness.
- **Missing:** recovered-sig (`qsigrec`), sig-share messages and aggregation sessions, `CSigningManager` and `CSigSharesManager` logic, and ChainLock *creation* (best-CL selection rules, `CheckCbTxBestChainlock`).

---

## 5. dash-spv (~46.8k lines)

- **Header sync and validation** (`dash-spv/src/validation/header.rs:18-36`):
  - It checks only `prev_blockhash` continuity and `hash <= target(nBits)`.
  - **No DGW/expected nBits, no powLimit, no MTP/future-time checks, no version checks.**
  - Security relies on hard-coded checkpoints (`chain/checkpoints.rs`, with fork rejection below the last checkpoint) plus ChainLocks.
  - The pipeline downloads parallel segments between checkpoints. It supports DIP-25 `headers2`.
- **Filters:** BIP157 cfheaders and cfilters, with the filter-header chain checked (`validation/filter.rs`).
  - Rust's `BlockFilter::new_script_filter` (`dash/src/bip158.rs:124-200`) builds Bitcoin BIP158 only. Since v23, **Core adds special-tx elements** (`core:evo/specialtx_filter.cpp`, `blockfilter.cpp:201`).
  - A Rust full node serving BIP157 would therefore produce filters and cfheaders that disagree with Core.
- **Masternodes:** sync via `getmnlistd`/`qrinfo` (`sync/masternodes/*`) into a `MasternodeListEngine` (`sml_engine.rs`), with retention windows, rotated-cycle caches and quorum verification. MN-root and CbTx-proof checking is missing (§3.3).
- **ChainLocks and IS:** `sync/chainlock`, `sync/instantsend`, `validation/{chainlock,instantlock}.rs`.
- **Storage:** custom segmented flat files plus an index (`storage/segments.rs`, `block_headers.rs`, `filters.rs`, `masternode.rs`, `peers.rs`, `metadata.rs`, migrator v1/v2, lockfile). There is no KV database, and nothing like a chainstate or UTXO set.
- **Network** (`dash-spv/src/network`, 6.3k lines):
  - tokio, **outbound only** (no `TcpListener`); version/verack handshake (`handshake.rs`); peer pool, reputation, DNS seeds, masternode IP seeds; addrv2 (it answers `getaddr` but does not relay).
  - It **ignores** `getheaders`/`getheaders2`/`getdata` from peers (`manager.rs:823-838`) and is V1 transport only.
  - BIP324 lives on branch `upstream/feat/bip324-v2-transport` (commits `6f418ddd`, `02093760`, `4bb26d55`) and is not merged.
  - Message set (`dash/src/network/message.rs:182-275`): version, verack, addr, addrv2, inv, getdata, notfound, getblocks, getheaders, getheaders2, headers, headers2, sendheaders(2), mempool, tx, block, ping/pong, merkleblock, filterload/add/clear, BIP157, BIP152 cmpct, alert, reject, feefilter, wtxidrelay, getmnlistd/mnlistdiff, getqrinfo/qrinfo, clsig, isdlock, senddsq.
  - **Missing for a node or masternode:** `mnauth`, `qsigshare`/`qbsigs`/`qsigsinv`/`qgetsigs`/`qsigrec`/`qsendrecsigs`, `qcontrib`/`qcomplaint`/`qjustify`/`qpcommit`/`qfcommit`/`qwatch`, `qgetdata`/`qdata`, `spork`/`getsporks`, `govobj`/`govobjvote`/`govsync`, CoinJoin (`dsa`/`dsi`/`dsf`/`dss`/`dssu`/`dsq`/`dstx`), `platformban`, asset-unlock inv (70242).
  - `PROTOCOL_VERSION` is 70237 (`dash/src/network/constants.rs:51`), versus Core 70242 (`core:version.h:14`). `MnListDiff` only supports the ≥70229 field order.
  - Reusability: framing, codec and peer management are decent scaffolding, but a full node needs inbound accept, inventory relay, serving, DoS scoring per Core, and BIP324.
- **dashd test harness (high value):**
  - `dash-spv/src/test_utils/node.rs`: `DashCoreNode` and `DashCoreConfig` spawn dashd. RPC helpers cover generate, send, invalidate, mocktime, `disconnect`/`setnetworkactive` and raw RPC.
  - `context.rs`: `DashdTestContext` over pre-generated regtest chains.
  - `masternode_network.rs`: `MasternodeTestContext` brings up a controller plus N masternodes from `DASHD_MN_DATADIR` (`network.json` with proTx/BLS keys), with `mine_dkg_cycle`, `wait_for_chainlocked_block` and mocktime orchestration.
  - Setup is `contrib/setup-dashd.py` (dashd 23.1.0 plus `dashpay/regtest-blockchain` v0.0.4).
  - This is directly reusable to drive a reference dashd in differential tests.

---

## 6. key-wallet, RPC, fuzz

- **key-wallet** (~50k lines) and **key-wallet-manager** (~6.5k): BIP32/39, DIP9/DIP13 account paths, BLS BIP32-style derivation (`derivation_bls_bip32.rs`), SLIP10 ed25519 (platform node keys), CoinJoin, provider-key accounts, UTXO tracking, a tx builder that constructs special payloads, and a signer. These are useful for masternode key and ProTx tooling, not for consensus.
- **rpc-json** (5.2k lines) and **rpc-client** (1.8k): typed results for `getblock`/`getblockheader`/`getrawtransaction`/`getblockchaininfo`/`getbestchainlock`/`getchaintips`/`gettxoutsetinfo`/`masternodelist`/`protx {info,list,diff,listdiff,register*,update*,revoke}`/`quorum {list,listextended,info,dkgstatus,sign,getrecsig,hasrecsig,isconflicting,memberof,rotationinfo,selectquorum,verify}`/`verifychainlock`/`verifyislock`/`mnsync`, plus `DMNState` and `DMNStateDiff` (`rpc-json/src/lib.rs:2007-2290`).
  - Branch `upstream/integration/v4.2-core-v24` adds about 1.8k lines of v24 shapes (shared and multi-payout MNs, nested platform addresses).
  - This is a ready asset for a "same RPC in, same JSON out" differential harness between a Rust node and dashd. The RPC *server* side does not exist.
- **fuzz:** 17 targets (`fuzz/Cargo.toml`) covering deserialize round-trips. None target special payloads specifically or compare against Core. Core's qa-assets corpus could seed differential parse fuzzing.

---

## 7. Coverage table

Confidence key: **H** = verified empirically or line-by-line against Core; **M** = reviewed and plausible but not exhaustively tested; **L** = known divergences; **—** = absent.

| Consensus area | Exists? | Where | Exactness confidence | Gap |
|---|---|---|---|---|
| Block header serialization and hash | Yes | `blockdata/block.rs:40-63`, X11 `hashes/src/hash_x11.rs` | H | `BlockHash` is sha256d without the feature flag |
| Tx serialization | Yes | `transaction/mod.rs:156-670` | **L** | Lossy and over-strict: u16 version, ignored payload len, segwit, unknown types, re-encode hashing (§2.1, 3 demonstrated divergences) |
| Block serialization | Yes | `impl_consensus_encoding!(Block…)` | M | Inherits tx issues; no size limits |
| Special-tx parsing | Partial | `special_transaction/*` | H for v1/v2, L for v24 | ProTx v3, CbTx v4, types 10–12, asset-unlock stable txid |
| Special-tx validation | — | — | — | All of `CheckProRegTx`/`CheckProUpServTx`/…, `CheckCbTx`, `CheckLLMQCommitment`, `CheckMNHFTx`, `CheckAssetLock/UnlockTx`, inputsHash and payload sigs |
| Script interpreter | — | (`bitcoinconsensus` feature is wrong) | — | Full Dash interpreter incl. DIP-0020 ops, CHECKDATASIG, flags by height |
| Sighash | Yes (legacy) | `crypto/sighash.rs:618-765` | M | No OP_CODESEPARATOR/FindAndDelete; tested only on Bitcoin vectors |
| Sigops | — | — | — | Legacy and P2SH sigop counting, block limits (DIP0001 changes) |
| PoW check / compact | Partial | `pow.rs:162-215` | M | No overflow flag; Bitcoin powLimit constants |
| DGW / KGW / retarget | — | — | — | All; devnet/regtest special cases; min-difficulty rules |
| Block checks (CheckBlock / Contextual) | — | (SPV continuity+PoW only) | — | Merkle mutation, size, BIP34 (h=951), time rules, version rules, coinbase rules |
| Subsidy | — | — | — | Dash subsidy formula (pre/post fork, 7.14% decline, BRR, MN_RR) |
| MN payments | — | — | — | Payee selection from DMN list, operator reward split, v24 payout lists and shares |
| Superblocks / governance | — | — | — | Governance objects, votes, trigger validation |
| DMN list state machine | — | — | — | Entire `CDeterministicMNManager`, EvoDB |
| SML entry / merkleRootMNList | Yes | `sml/masternode_list_entry/*`, `merkle_roots.rs` | **H** (Core unit vector 15/15 plus 6/6 mainnet diffs) | Empty list returns None, not 0; no mutation flag; SPV does not enforce |
| merkleRootQuorums | Partial | `merkle_roots.rs:140-162` | H for the final step (6/6) | Mined/active commitment tracking (`CQuorumBlockProcessor`) |
| LLMQ commitment verification | Yes | `quorum_entry/{hash,validation}.rs` | M-H | single-member path, pre-V19 legacy key/hash handling, `VerifySizes`/`VerifyNull`, height and type rules |
| Quorum member selection (non-rotated) | Yes | `scores_for_quorum.rs`, `quorum_modifier_type.rs` | M-H (post-V20) | Pre-V20 base-block list; V20 off-by-one; V19 EvoOnly gate; tie-break |
| Quorum member selection (rotated) | Yes (from snapshots) | `dash-spv/src/sml_engine/rotated_quorum_construction.rs` | M-H | Snapshot *building*; mode-2 semantics; predicting path |
| BLS legacy / basic | Yes | `crypto/src/bls.rs` over `dash-pkc` (blst) | H for verify | Threshold, shares and IES exist in dash-pkc but are not wired |
| ChainLock verify | Yes | `ephemerealdata/chain_lock.rs`, `dash-spv/src/validation/chainlock.rs` | H | Signing and creation, CbTx best-CL rules |
| IS lock verify | Yes (ISDLOCK) | `ephemerealdata/instant_lock.rs`, `validation/instantlock.rs` | H | IS signing, conflict handling, v2 asset-unlock locks |
| Credit pool | — | — | — | `creditpool.cpp` (limits, unlock index, balance in CbTx) |
| MNHF / EHF | Parse only | `mnhf_signal.rs` | H (parse) | Signal validation, EHF state, deployments |
| Versionbits | — | — | — | BIP9 plus Dash dynamic thresholds (`nThresholdStart/Min/FalloffCoeff`), `useEHF` |
| UTXO / coins DB | — | — | — | All |
| Mempool | — | (wallet mempool tracking only) | — | Acceptance, policy, special-tx conflicts, IS locking |
| P2P full-node behavior | Partial | `dash-spv/src/network`, `dash/src/network` | M (codec) | Inbound, relay, serving, LLMQ/MN/gov/spork/CoinJoin messages, BIP324 unmerged, proto 70237 |
| BIP157/158 | Yes (Bitcoin rules) | `bip158.rs` | L vs Core v23+ | Special-tx filter elements |
| RPC server | — | (client and types only) | — | All; rpc-json types reusable for differential harness |
| Network params | Partial | `dash-network`, `consensus/params.rs` | **L** | params.rs is Bitcoin; only V20 height; test/devnet LLMQ param errors |

---

## 8. Implications for the program

1. **Reuse as-is, behind a differential test wall:**
   - SML entry, root and ordering;
   - quorum scoring and modifier;
   - DIP-24 snapshot reconstruction;
   - ChainLock and ISLock hashing and selection;
   - `dash-pkc` BLS;
   - X11;
   - the BIP37, BIP152 and headers2 codecs;
   - `rpc-json` types;
   - the dashd and masternode test harness.
2. **Replace or redo for bit-exactness:**
   - tx/payload parsing (raw `vExtraPayload`, Core's version/type types, no segwit; consider adopting `base-sdk` `dash-primitives`);
   - all chain params (move to a `dash-params`-like table copied from `core:chainparams.cpp`);
   - `from_compact` overflow;
   - merkle mutation detection;
   - BIP158 special elements.
3. **Build new:** script interpreter with Dash flags, sigops, DGW/KGW, block and tx contextual checks, subsidy and payments, governance and superblocks, DMN state machine plus EvoDB, quorum block processor and DKG, signing sessions, credit pool, EHF/versionbits, coins DB, mempool, a full P2P node, and an RPC server.
4. **Cheap, high-yield verification assets to add first:**
   - import Core's `sighash.json`, `script_tests.json`, `tx_valid.json` and `tx_invalid.json`;
   - add a test computing MN and quorum roots on `DML_0_2221605.hex` and `QR_INFO_0_2224359.hex` (my probes show they pass);
   - regenerate or delete the two corrupted `.bin` SML fixtures;
   - run a full mainnet block-parse and re-serialize round-trip from genesis against dashd's `getblock <hash> 0`, to find historical v1/v2-with-type txs and payload edge cases.

Scratch evidence (not part of either repo): `/tmp/r4-g-check/src/main.rs` and `src/bin/probe{,2..8}.rs`.
- probe3: Core unit vector, 15/15 entry hashes and root.
- probe6: 6/6 mainnet Core wire diffs match both roots.
- probe7: corrupted `.bin` fixture IPv4 encoding.
- probe8: tx-parse divergences (v1/type5, CbTx v4).
