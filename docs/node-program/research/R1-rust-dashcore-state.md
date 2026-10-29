# R1: State of rust-dashcore relative to a full Dash node

- **Audited:** `~/workspace/rust-dashcore` at `dev` 67b307c6 (synced with dashpay/rust-dashcore `dev`), on 2026-10-08.
- **Cross-checked against:**
  - Dash Core at `~/workspace/dash` 15db478750 (the `develop` / v24 line, `PROTOCOL_VERSION = 70242`).
  - base-sdk at the pinned rev `e6402ced`.
- **Method:** read-only. Four parallel sub-audits:
  1. the `dash` crate,
  2. `dash-spv`,
  3. the wallet and FFI crates,
  4. support crates, RPC, CI and base-sdk.

  I then spot-checked the surprising claims myself against the source (marked ✔ where I re-verified). PRs, issues and branches come from `gh` and `git` on 2026-10-08.

---

## 0. Executive summary

rust-dashcore is a mature **protocol-types, SPV and mobile-wallet** stack: about 200K lines of Rust across 18 crates. It is **not a node**, and none of the stateful consensus machinery exists:

- no script interpreter
- no transaction or block validation
- no DGW
- no UTXO set or chainstate
- no consensus mempool
- no inbound P2P
- no RPC server

What it does well, and what the node can reuse:

- **Wire codecs** for blocks, transactions, all current DIP2 payloads, headers2, BIP157/158, MnListDiff and QRInfo.
- **SML/LLMQ machinery** with real BLS verification of quorum commitments, ChainLocks and InstantSend locks (DIP24 rotation included). BLS is base-sdk's `dash-pkc`, which uses `blst`.
- **A well-tested async P2P client** (tokio).
- **A rich HD wallet:** DIP9/13/15/17 paths, provider keys, asset locks, coin selection.
- **A real dashd-23.1.0 integration-test harness** that runs in CI on four operating systems.

Several Bitcoin leftovers are **actively wrong for Dash** and must be fixed before any consensus work. Details in §7.

base-sdk is barely used. Only the BLS/EdDSA key types from `dash-pkc`, and `make_bytes` from `dash-types`, are consumed, all inside `crypto/`. Meanwhile about 20K LOC of rust-dashcore duplicate base-sdk crates that rust-dashcore doesn't use: `dash-primitives`, `dash-p2p-core` (which includes BIP324), `dash-params`, `dash-pow` (pure-Rust X11), `dash-script` and `dash-num`.

---

## 1. Crate inventory

LOC counts all `.rs` files, tests included.

| Crate (dir) | Package | Ed. | LOC | Role | Maturity |
|---|---|---|---|---|---|
| `dash/` | `dashcore` | 2024 | 42.4K (126 files) | Protocol types, consensus encoding, P2P messages, script types (no interpreter), sighash, SML/LLMQ, CL/IS types, BIP37/152/158 | Mature for parsing; not a validator |
| `hashes/` | `dashcore_hashes` | 2021 | 2.2K | Re-exports `bitcoin_hashes` 0.14; adds `hash_x11` (C `rs-x11-hash` 0.1.9 via `cc`), SIMD SipHash for BIP158, newtype macros | Stable |
| `crypto/` | `dashcore-crypto` | 2021 | 2.0K | secp256k1 keys/ECDSA (own), sighash and taproot types re-exported from `bitcoin-crypto` (git rev 7ba35c7), BLS/EdDSA wrappers over `dash-pkc` | Stable, recently reworked (#1036 replaced blsful) |
| `internals/` | `dashcore-private` | 2024 | 0.9K | Fork of rust-bitcoin internals (hex display, macros) | Shrinking (#1110, #1115) |
| `dash-network/` | `dash-network` | 2024 | 0.35K | `Network {Mainnet, Testnet, Devnet, Regtest}`, magic, P2P ports, DNS seeds, `v20_activation_height()`, optional `FFINetwork` | Minimal |
| `dash-network-seeds/` | same | 2024 | 0.65K + data | Embedded masternode IP seeds: mainnet 2,094, testnet 94, with probe columns. No `chainparamsseeds`-style fixed seeds | Weekly auto-refresh |
| `masternode-seeds-fetcher/` | bin | 2024 | 0.9K | P2P-only tool that regenerates the seed files (uses `dash_spv::network::Peer`) | Tool |
| `key-wallet/` | `key-wallet` | 2021 | 51.3K (about 10K tests) | HD wallet: BIP32/39/44, DIP9/13/14/15/17, provider keys (BLS operator, EdDSA platform), accounts, address pools, tx checking and routing, builder, coin selection, asset locks | Heavily used in production (mobile, Platform SDK) |
| `key-wallet-manager/` | same | 2021 | 7.8K | `WalletManager<T>` multi-wallet, `WalletInterface` trait consumed by dash-spv, events, compact-filter matching | Active |
| `key-wallet-ffi/` | same | 2021 | 24.1K | 259 `#[no_mangle]` C exports, cbindgen headers, iOS build script | Active; PR #776 proposes deleting it |
| `dash-spv/` | `dash-spv` | 2021 | 53.4K (154 files) | Async SPV client: P2P, segmented flat-file storage, header/filter/block/masternode/CL/IS/mempool sync managers, `dash-spv` CLI binary, dashd test harness | Most active crate |
| `dash-spv-ffi/` | same | 2021 | 8.6K | 43 C exports wrapping `DashSpvClient`, callbacks, `ffi_cli` | Active; PR #776 proposes deleting it |
| `dash-spv-bench/` | bin | 2021 | 0.7K (+scripts) | End-to-end sync benchmark (docker dashd 23.1.7 peers, netem, pprof/jemalloc) | Tool, not in CI |
| `rpc-client/` | `dashcore-rpc` | 2024 | 2.3K | **Blocking** JSON-RPC *client* (`jsonrpc` 0.18 simple_http), 95 methods | Partial coverage |
| `rpc-json/` | `dashcore-rpc-json` | 2024 | 5.2K (1 file) | 158 result/arg structs | Partial; a Core v24 update sits on branch `integration/v4.2-core-v24` |
| `rpc-integration-test/` | `integration_test` | 2024 | 1.6K | 100 tests against a dashmate-style 2-node setup | **Dead**: CI job `if: false`, bitcoind `run.sh` |
| `fuzz/` | `dash-fuzz` | 2024 | 0.9K | 17 honggfuzz targets (deserialization and hashes) | Nightly only |
| `git-state/` | proc-macro | 2021 | 38 | `git_dirty!()` for version strings | — |
| `ffi-c-tests/` | — | — | 4 C files | Only checks that generated headers compile | Thin |

**Workspace facts:**
- Workspace version 0.45.0, MSRV 1.89, toolchain file pins 1.98.1, resolver 2.
- No committed `Cargo.lock`.
- `[patch.crates-io]` points `dashcore_hashes` at the local path and `rs-x11-hash` at a git tag.

**Dependency layering** (normal deps only):

```
dashcore_hashes, dashcore-private, dash-network, git-state         (leaves)
dashcore-crypto  -> dash-network, dashcore-private, dashcore_hashes, dash-pkc, dash-types, bitcoin-crypto
dashcore         -> dashcore-crypto, dash-network, dashcore-private, dashcore_hashes  (+ unused dash-types)
key-wallet       -> dashcore
key-wallet-manager -> key-wallet
dash-spv         -> dashcore, key-wallet(-manager), dash-network-seeds, git-state, [test-utils] dashcore-rpc
*-ffi            -> the above
dashcore-rpc-json -> dashcore, key-wallet ; dashcore-rpc -> rpc-json
```

`dash-spv` hard-enables these `dashcore` features: `core-block-hash-use-x11`, `bls`, `quorum_validation`, `message_verification` and `serde`.

---

## 2. Capability matrix

Status meanings:
- **present:** usable as-is for the node (it may still need review).
- **partial:** exists but is incomplete or not wired in.
- **missing:** does not exist.
- **wrong:** exists but its behaviour diverges from Dash Core.

### 2.1 Consensus and validation

| Capability | Status | Where | Notes |
|---|---|---|---|
| Block / tx / header serialization | present (with v24 gaps) | `dash/src/blockdata/`, `consensus/encode.rs` | 16-bit version/type split and Core's version-0 quirk preserved. BIP144 segwit decode path still active for input-less non-whitelisted txs |
| Special-tx payload length bound | **wrong** ✔ | `special_transaction/mod.rs:427-431` | The `vExtraPayload` length prefix is read into `_len` and ignored; payloads are parsed inline. New payload versions misalign the whole block. PR #1136 adds unknown-type passthrough |
| DIP2 payloads: ProRegTx, ProUpServ, ProUpReg, ProUpRev, CbTx v1-3, QcTx v1-4, MnHf, AssetLock, AssetUnlock v1 | present (parse only) | `dash/src/blockdata/transaction/special_transaction/` (3.9K LOC) | v24 formats missing: CbTx v4 (PR #1137), ProUpServ/ProUpReg v3 (PR #1138), ProRegTx v3 (#1132), types 10-12 (#1131), v2 asset-unlock stable txid / `MSG_ASSET_UNLOCK` (#1134). Tracking issue #1047 |
| Special-tx *validation* (ProTx sigs, inputsHash, collateral, AssetUnlock quorum sig, MnHf sig, credit pool limits) | missing | — | Only `QuorumEntry::validate_structure` (`quorum_commitment.rs:68`), which is weaker than Core's `VerifySizes` |
| Script interpreter (EvalScript/VerifyScript) | **missing** ✔ | — | `Script` and `Instruction` only classify, iterate and format |
| Script verification | **wrong** | `script/borrowed.rs:399`, `transaction/mod.rs:455` | Behind the optional `bitcoinconsensus` 0.20.2 feature, which is **Bitcoin's** libbitcoinconsensus. It can't parse special transactions and doesn't know DIP-0020 |
| DIP-0020 opcodes (OP_CAT/SPLIT/NUM2BIN/BIN2NUM/AND/OR/XOR/DIV/MOD, CHECKDATASIG[VERIFY]) | **wrong** ✔ | `blockdata/opcodes.rs:202,264,358,395` | 0xba is `OP_CHECKSIGADD` here but `OP_CHECKDATASIG` in Core. The re-enabled opcodes are classified `IllegalOp` |
| Legacy sighash | present | `dash/src/crypto/sighash.rs:618,751` | ANYONECANPAY/NONE/SINGLE and the SINGLE bug handled; payload included as in Core. **No OP_CODESEPARATOR/FindAndDelete.** The 290-vector `sighash.json` test is `#[ignore]` |
| CheckTransaction / CheckBlock (sizes, sigops, dup inputs, coinbase rules) | missing | — | No sigop counting at all |
| Contextual checks (BIP34 height enforcement, finality, MTP, future time, version/BIP9) | missing | — | `Block::bip34_block_height` extracts the height, nothing enforces it |
| Merkle root check | partial ✔ | `Block::check_merkle_root` `block.rs:216` | **Never called by dash-spv** (issue #852). No CVE-2012-2459 mutation flag in `merkle_tree::calculate_root` |
| X11 PoW hash | partial ✔ | `hashes/src/hash_x11.rs`; `dash/src/hash_types.rs:84-93` | **`BlockHash` is SHA256d unless the non-default feature `core-block-hash-use-x11` is on.** C implementation via FFI |
| PoW target check | partial | `Header::validate_pow`, `pow.rs` (`Target`, `CompactTarget`, `Work`, `U256`) | Only checks the hash against the header's own nBits |
| DarkGravityWave v3 retarget | missing ✔ | — | A port exists in open PR #803 (fork detection). Issue #1116. base-sdk `dash-pow` has params |
| Chain work / best-chain selection | partial (dead code) | `dash-spv/src/chain/chain_work.rs`, `chain_tip.rs` | Not referenced by the sync code. Issue #332 |
| Consensus params | **wrong** | `dash/src/consensus/params.rs`, `blockdata/constants.rs` | Bitcoin values (600 s spacing, 2016 retarget, BIP34 at 227931, halving 210000, 4M weight). `Params::new` has no callers. Genesis blocks are correct. No DIP/V19/V20/V24 deployment heights except `v20_activation_height()` |
| Subsidy / superblock / MN and Platform payments / coinbase payout checks | missing | — | — |
| BIP9 versionbits / EHF (MNHF) deployment state | missing | — | MnHf payload parse only |
| UTXO set / coins view / undo data | missing | — | The wallet tracks only its own UTXOs |
| Chainstate, block connect/disconnect, reorg with undo | missing | — | dash-spv "reorg" truncates header/filter storage only |
| Credit pool accounting | missing | — | — |
| Checkpoints | present (SPV style) | `dash-spv/src/chain/checkpoints.rs` (89 hard-coded) | Used as trust anchors for header segments. PR #375 rewrites this |

### 2.2 Masternode, LLMQ, CL and IS

| Capability | Status | Where | Notes |
|---|---|---|---|
| MnListDiff / QRInfo messages | present | `dash/src/network/message_sml.rs`, `message_qrinfo.rs` | — |
| Simplified MN list and entries (v1-3, ExtNetInfo) | present | `dash/src/sml/masternode_list/`, `masternode_list_entry/` | — |
| `MasternodeListEngine` | present (in dash-spv) | `dash-spv/src/sml_engine.rs` (+ `sml_engine/`, about 3K LOC), private module | Was in `dash` once; now SPV-internal |
| LLMQ type params (1-6, 100-107, devnet overrides) | present | `dash/src/sml/llmq_type/` | — |
| Quorum commitment BLS verification (aggregate members sig + threshold sig) | present | `sml/quorum_entry/validation.rs:26,69` | `verify_secure_aggregate` silently drops undecodable keys, where Core rejects (`crypto/src/bls.rs`) |
| Rotated (DIP24) quorum reconstruction and validation | present | `dash-spv/src/sml_engine/` | Several open correctness issues (#942, #954, #956, #957) |
| MN-list / quorum merkle root vs CbTx; coinbase merkle proof in mnlistdiff | **missing (unwired)** ✔ | `sml/masternode_list/merkle_roots.rs` (no callers); `from_diff.rs:102` stores `merkle_hashes.first()` as the list root (bug) | Open PRs #799 and #1077; issue #1117 |
| ChainLock verification | present | `ephemerealdata/chain_lock.rs`, `dash-spv/src/validation/chainlock.rs` | Quorum selection lacks Core's active-quorum windowing in `dash`. #1126: marked validated before the header hash check |
| InstantSend (isdlock) verification | present | `ephemerealdata/instant_lock.rs`, `dash-spv/src/validation/instantlock.rs` | Rotated quorum selection lives only in dash-spv. Legacy `islock` not supported |
| LLMQ DKG (contrib, complaint, justify, premature/final commit) | missing | — | No message types |
| Signing sessions (qsigshare, qsigrec, qbsigs, qsendrecsigs, recovery) | missing | — | No message types, no BLS threshold signing in the wrapper. `dash-pkc` has threshold/aggregate primitives |
| Producing CL / IS as a masternode | missing | — | — |
| mnauth / masternode connections / quorum connections | missing | — | `version` carries `mn_auth_challenge`, always zero |
| Platform ban, EHF signals as MN | missing | — | — |

### 2.3 P2P networking

| Capability | Status | Where | Notes |
|---|---|---|---|
| Message enum coverage | partial | `dash/src/network/message.rs:180-283` | 42 of Core's 74 commands. Missing: spork/getsporks; all CoinJoin (dsa/dsi/dsf/dss/dsc/dssu/dstx/dsq; only `senddsq` exists); govsync/govobj/govobjvote; all qsig*/DKG/qwatch/qgetdata/qdata; mnauth; platformban; sendtxrcncl. Unknown commands decode as `Unknown` |
| Inventory types | **wrong / partial** ✔ | `message_blockdata.rs:47,80,105` | `CompactBlock` uses type **4**; Dash's `MSG_CMPCT_BLOCK` is **20**. Spork 6, DSTX 16, gov 17/18, quorum 21-28, DSQ 32, PlatformBan 33, AssetUnlock 34 → `Unknown` |
| Protocol version | stale ✔ | `network/constants.rs:51` | 70237 vs Core 70242. `ServiceFlags` still has WITNESS/GETUTXO |
| Size limits | wrong | `consensus/encode.rs` compact size is u32; `MAX_MSG_SIZE = 5,000,000` | Core: 3 MiB message cap. Headers count prealloc DoS (PR #1140) |
| V1 transport | present | `dash-spv/src/network/peer.rs` | — |
| V2 transport (BIP324) | missing on `dev` | Open PR #264 (pasta; `bip324` crate, Dash short IDs 128-167, V2Preferred) | base-sdk `dash-p2p-core` also has bip324 |
| Outbound peer manager, DNS seeds, addr/addrv2, reputation/bans | present (SPV-grade) | `dash-spv/src/network/{manager,discovery,addrv2,reputation}.rs` | Default 3 outbound peers. Scores only for connection-level faults; invalid data isn't penalised. PR #902 rewrites the manager (+7.3K/−10.9K) |
| **Inbound listener** | **missing** ✔ | — | `TcpListener` only appears in a test port probe |
| Serving data (getheaders, getdata, getcfilters, getblocks, mempool) | missing | `getheaders` explicitly dropped | Answers `getaddr` (even on outbound, unlike dashd) |
| Tx relay (inv/getdata, trickle, feefilter) | missing | — | Broadcast pushes an unsolicited `tx` to a subset of peers |
| Compact blocks | types only | `bip152.rs`, `message_compact_blocks.rs` | Never negotiated |
| headers2 (compressed headers) | present | `dash/src/network/message_headers2.rs`, used by dash-spv | — |
| BIP37 bloom | present | `dash/src/bloom/`, used by the mempool manager | — |
| BIP157/158 | present (client) | `dash/src/bip158.rs`; `dash-spv/src/sync/filter*` | Filter-header chaining across batches is not checked (#1118). No filter *index* or serving |
| Fingerprint vs dashd | very distinct | `dash-spv/src/network/handshake.rs` | UA `/rust-dash-spv:x/`, services `NODE_HEADERS_COMPRESSED` only, `start_height=0`, `relay=false`, no getsporks/sendcmpct/feefilter, sends `filterload`+`mempool` |

### 2.4 Storage, mempool, indexes

| Capability | Status | Where | Notes |
|---|---|---|---|
| Storage engine | present (custom) | `dash-spv/src/storage/` (`DiskStorageManager`, segments, atomic temp+fsync+rename, lockfile, migrator v2) | Flat segment files only. No sqlite/rocksdb/redb |
| Header store | present | `block_headers/segment_*.dat`, 10K/segment, header + cached X11 hash; in-memory hash→height index | — |
| Filter header and filter stores | present | `filter_headers/`, `filters/` | — |
| Block store | partial | `blocks/` | **Only wallet-matched blocks** |
| Masternode state | present | `masternodes/diff_*.dat`, `qrinfo_*.dat` | Raw messages replayed at startup (branch `feat/masternode-diff-storage` reworks this) |
| Mempool | partial (wallet-only) | `dash-spv/src/sync/mempool/` (3.9K LOC) | Tracks wallet-relevant unconfirmed txs, cap 1000, 24 h expiry. No acceptance rules, policy, eviction, fee or ancestor tracking |
| Fee estimation | missing | — | Wallet uses static sat/kB |
| txindex / addressindex / spentindex / timestampindex / blockfilterindex (server) | missing | — | — |
| Governance object store / sporks store | missing | — | — |

### 2.5 Wallet

| Capability | Status | Where | Notes |
|---|---|---|---|
| HD BIP32/39/44, DIP9 feature paths, DIP14 256-bit, DIP15 DashPay, DIP17 platform payment | present | `key-wallet/src/{bip32,dip9,mnemonic}.rs`, `account/account_type.rs` | `Wallet::from_mnemonic` hard-codes an empty BIP39 passphrase (`wallet/initialization.rs:225`) |
| CoinJoin accounts | present (paths) | `m/9'/coin'/4'/n'`, gap 100 | Matches Core's **descriptor** wallets (`pkh(xpub/9h/5h/4h/0h/0/*)`, `src/wallet/scriptpubkeyman.cpp:2535`). Core **legacy HD** wallets mix on the BIP44 chain, so imports need care |
| Provider keys (owner, voting, BLS operator legacy/basic, EdDSA platform node) | present | `derivation_bls_bip32.rs`, `derivation_slip10.rs` | — |
| UTXO and balance tracking, IS/CL awareness, conflict sweep, abandon | present | `managed_account/`, `wallet/balance.rs`, `managed_wallet_info/helpers.rs` | Chain-locked records are dropped from memory unless `keep-finalized-transactions` is on |
| Tx building, coin selection (BnB default), BIP69, coin control, UTXO lock | present | `managed_wallet_info/{transaction_builder,coin_selection}.rs` | `Random` strategy is a stub (`coin_selection.rs:323`) |
| Signing | partial | `TransactionSigner::sign_tx` | P2PKH only, SIGHASH_ALL. No P2SH/multisig, no PSBT |
| Asset lock creation (Platform identity funding) | present | `asset_lock_builder.rs` | Beyond what dashd does |
| ProTx building from the wallet | partial | Generic builder plus payload finalizer | No ProRegTx/ProUp* builders, no BLS signing helper |
| Governance voting / proposals | missing | — | — |
| CoinJoin mixing client | missing | — | Only detection and routing of denominations |
| Encryption at rest / passphrase / lock-unlock | **missing** | — | `Wallet::backup()` is plaintext bincode including mnemonic and xpriv |
| Persistence | delegated | `WalletManager::take_persistence_receiver()` event stream | `ManagedWalletInfo` is serde-only. No built-in DB |
| Import: mnemonic, seed (hdseed), xprv/xpub, bincode | present | `key-wallet-manager/src/lib.rs:437,482` | — |
| Import: WIF / dumpwallet / importwallet / wallet.dat (BDB) / SQLite descriptor wallet / descriptors | **missing** | — | BIP38 removed in #1111 |
| Reorg rollback | missing | `WalletInterface` has no disconnect | Issue #854 |
| Fee estimation, RBF (n/a in Dash), subtract-fee, signmessage | missing | `dash/src/sign_message.rs` exists, unused by the wallet | — |

### 2.6 RPC and CLI

| Capability | Status | Where | Notes |
|---|---|---|---|
| RPC client | partial | `rpc-client/src/client.rs` (95 methods, blocking) | Missing: gobject/governance, spork, coinjoin, `protx *_evo`, `quorum platformsign/dkginfo`, address/spent index RPCs, `gettransaction`, `decoderawtransaction`, `sendmany`, `walletpassphrase`, `dumpwallet`, … |
| RPC JSON types | partial | `rpc-json/src/lib.rs` | Core v24 masternode shape changes sit on branch `integration/v4.2-core-v24` (+1.7K) |
| **RPC server** | **missing** ✔ | — | No axum/hyper/jsonrpsee/tiny_http anywhere, and none in the resolved lockfile |
| dash-cli equivalent | missing | — | — |
| Daemon binary / config file / datadir | partial | `dash-spv/src/main.rs` (clap; requires `--mnemonic-file`; default datadir is a fresh `/tmp/dash-spv-<ts>-<pid>`) | No `dash.conf`, no daemonization |

---

## 3. Crate-by-crate notes

### 3.1 `dash` (dashcore), 42K LOC

**Module map (LOC):**

| Module | LOC |
|---|---|
| `blockdata/` (block, tx, script 3.3K, opcodes, locktime, witness, weight, special_transaction 3.9K) | ~12K |
| `network/` | 4.35K |
| `sml/` | 4.45K |
| `address.rs` | 2.2K |
| `amount.rs` | 2.4K |
| `taproot.rs` | 2.3K |
| `consensus/` | 2.46K |
| `crypto/` (sighash) | 1.8K |
| `pow.rs` | 1.77K |
| `merkle_tree/` | 0.93K |
| `bip158.rs` | 0.83K |
| `bloom/` | 0.58K |
| `bip152.rs` | 0.5K |
| `ephemerealdata/` (ChainLock, InstantLock) | 0.31K |
| `hash_types.rs` (about 30 newtypes: ProTxHash, QuorumHash, …) | 0.55K |

**Tests:**
- 449 unit tests plus 13 integration tests; 17 `#[ignore]`.
- About 12 MB of fixtures, including 11 real mainnet MnListDiff/QRInfo captures and Dash `blockfilters.json`.
- **None of Dash Core's `script_tests.json`, `tx_valid.json`, `tx_invalid.json` or `sighash.json` are used.** Only a handful of script cases were copied, and the Bitcoin `legacy_sighash.json` test is ignored.

**Features:** default `secp-recovery,bincode`, plus `core-block-hash-use-x11`, `bls`, `quorum_validation`, `message_verification`, `eddsa`, `signer`, `serde`, `test-utils`, and the implicit `bitcoinconsensus`.

### 3.2 `dash-spv`, 53K LOC

**Architecture:**
- `DashSpvClient<W: WalletInterface, N: NetworkManager, S: StorageManager>`.
- `SyncCoordinator` spawns 8 `SyncManager` tokio tasks: BlockHeaders, FilterHeaders, Filters, Blocks, Masternodes, ChainLock, InstantSend, Mempool.
- Each manager runs a `select!` over topic-filtered network messages, a `broadcast<SyncEvent>` bus (capacity 10K), network events and a tick.
- Progress is merged from per-manager `watch` channels.
- `EventHandler` trait for consumers.
- One reader task per peer, with `Arc<RwLock<Peer>>`.
- Rayon for parallel PoW and filter checks.

**Header pipeline:**
- Checkpoint-segmented parallel download.
- `BlockHeaderValidator` (`validation/header.rs`) checks only linkage and PoW against the header's own bits.
- **`ValidationMode {None, Basic, Full}` is plumbed through config and CLI but never read** ✔.

**Fork handling:** `handle_fork` stops the client, truncates every store above the fork point, and restarts. There is no work comparison and no ChainLock constraint. `CheckpointManager::should_reject_fork`, `ChainWork` and `ChainTipManager` are all unused.

**Tests:**
- 633 unit tests.
- 72 integration tests, including **35 `dashd_sync`** tests (basic, restart, disconnect, fork, mempool, multi_wallet, transaction) and **10 `dashd_masternode`** tests (sync, chainlock, instantsend) against a prebuilt regtest masternode network.

**Docs are stale:**
- `ARCHITECTURE.md` says BLS validation is still to do.
- `docs/utxo_rollback.md` describes a non-existent `UTXORollbackManager`.
- `README.md` shows an old API.

### 3.3 Wallet crates

Covered in §2.5.

**`key-wallet`:**
- Strengths: account and path taxonomy, tx routing and checking (9.5K LOC), builder, BnB selection, asset locks.
- `key-wallet/CLAUDE.md` is partly stale: it says `accounts: BTreeMap<Network, …>`, but the wallet is now single-network.

**`key-wallet-manager`:**
- The `WalletInterface` trait is the only seam between SPV and wallet. Its methods: `process_block_for_wallets`, `process_mempool_transaction`, `process_instant_send_lock`, `apply_chain_lock`, `monitored_script_pubkeys_for`, `watched_outpoints`, `wallets_behind`, …
- Open PR #634 (shumkov) restructures it into a single-map `Arc<RwLock<T>>`-per-wallet design.

### 3.4 FFI crates

- Opaque `Box::into_raw` handles with matching `*_free` functions.
- key-wallet-ffi uses an `FFIError` out-param. dash-spv-ffi uses `i32` codes plus a **process-global** last-error mutex, which is not thread-local.
- No `catch_unwind`, and about 105 unwrap/expect/panic calls in non-test FFI code. `types.rs:281` deliberately panics, and a panic across `extern "C"` aborts the process.
- C callbacks plus `user_data` are invoked from tokio worker threads.
- Swift via `build-ios.sh` and an xcframework built elsewhere. No Kotlin/JNI or uniffi.
- Open PR #776 (xdustinface) proposes deleting both FFI crates, −37.6K LOC. That is relevant to how much maintenance surface the node program inherits.

### 3.5 RPC

- **Client:** synchronous, `jsonrpc::simple_http`, plaintext HTTP only (PR #1141 rejects https).
- **Bugs:** #1128 (submitchainlock byte order), #1122 (secrets in logs).
- **Coverage by category:**
  - blockchain 20 methods
  - network 13
  - mining 5
  - raw/PSBT 8
  - util 6
  - wallet 31
  - CL/IS/evo 7
  - masternode 7
  - bls 2
  - quorum 12
  - protx 11
- **Integration test:** dead. It has a bitcoind `run.sh`, DASHVERSION 22.1.3 in `contrib/test-rpc.sh`, and a disabled CI job.

### 3.6 hashes, crypto, internals, network, seeds, fuzz

- **hashes:** X11 is a C dependency. Pure-Rust X11 exists in base-sdk `dash-pow`, unused.
- **blake3:** used only to sort masternode peer addresses (`sml/masternode_list/peer_addresses.rs:43`).
- **fuzz:** 17 honggfuzz targets, run for 60 s nightly. There are no targets for special transactions, SML application, BLS or X11, and no differential fuzzing against Core.
- **Open fuzz failure:** issue #999.

---

## 4. Architecture notes

- **Async runtime:** tokio (`full` in dash-spv and the FFI crates; narrower features in the wallet crates). The RPC client is blocking. Rayon is used for CPU-parallel work.
- **Storage:**
  - One custom flat-file segment store in dash-spv (`DiskStorageManager`), with a background flush every 5 s and a schema migrator.
  - The wallet has no storage of its own; it emits events for the host to persist.
  - Serialization is `grovedb-bincode` 2.1, a pinned fork of the unmaintained bincode (issue #344), plus serde.
  - A node needs a real KV store (UTXO, block index, indexes) and an undo-data format. No such abstraction exists today.
- **Crypto:**
  - secp256k1 0.33.1, pinned to match `bitcoin-crypto` and `dash-pkc`.
  - BLS is `dash-pkc` on `blst`, with legacy = `BlsScChia` and basic = `BlsScIetf` (`crypto/src/bls.rs:59-66`).
  - EdDSA is `dash-pkc` on `ed25519-dalek` 3.
  - Lockfile resolution shows three versions of `bitcoin_hashes` (0.14.101, 1.2.0 from crates.io, 1.2.0 from git) and two of `bitcoin-consensus-encoding`.
- **Testing infrastructure:**
  - `contrib/setup-dashd.py` downloads dashd **23.1.0** for all four OSes, plus `dashpay/regtest-blockchain` v0.0.4 datasets (`regtest-40000`, `regtest-200`, `regtest-mn`).
  - It caches them in `~/.rust-dashcore-test` and exports `DASHD_PATH`, `DASHD_TEST_DATA` and `DASHD_MN_DATADIR`.
  - `DashdTestContext`, `DashCoreNode` and `MasternodeTestContext` (`dash-spv/src/test_utils/`) drive dashd through `dashcore-rpc`.
  - **This harness is the natural seed for differential "Rust node vs dashd" testing.**
- **CI:**
  - **Groups** (`.github/ci-groups.yml`): core, spv, wallet, ffi, rpc.
  - **`build-and-test.yml`:** runs each crate's tests on ubuntu x64, ubuntu arm, macOS and Windows. The spv and ffi groups run real dashd tests on all four OSes. Coverage goes to Codecov.
  - **`rust.yml`:** cargo-deny, MSRV check, docs `-D warnings`, group verification.
  - **`pre-commit.yml`:** fmt, clippy `-D warnings` in debug and release, typos, actionlint.
  - **`sanitizer.yml`:** ASAN and TSAN. Its path filters are broken: `dashcore/**` and `dashcore_hashes/**` don't exist.
  - **`fuzz.yml`:** nightly only.
  - **`update-masternode-seeds.yml`:** weekly.
  - **Staleness:** `CLAUDE.md` refers to `contrib/test.sh` / `DO_LINT` / `DO_FMT`, which don't exist. The CI dashd version is 23.1.0, but Core `develop` is v24 (protocol 70242).

---

## 5. In-flight work

### 5.1 Open PRs relevant to the node program

There are 38 open PRs as of 2026-10-08. The relevant ones:

| PR | Author | Size | Relevance |
|---|---|---|---|
| **#264** feat(dash-spv): BIP324 v2 transport | PastaPastaPasta | +2.8K/−0.5K | `Transport` trait, V1/V2, Dash short IDs, `consensus_{en,de}code_payload` split. Needed for dashd-indistinguishable networking |
| **#902** (draft) network manager refactor | ZocoLini | +7.4K/−10.9K | Request broker, strict-priority scheduler, bandwidth-sized budgets, peer supervisor. Will conflict with any node P2P layer, so coordinate |
| **#803** (draft) block locator + fork detection | xdustinface | +2.4K/−0.4K | **The only DGW v3 port in the repo**, plus MTP and an exponential locator |
| **#799** (draft) / **#1077** validate MN-list root / check MnListDiff against the coinbase | xdustinface / QuantumExplorer | +0.4K / +1.5K | Closes the SML↔header binding gap (issues #1117, #852-adjacent) |
| **#1081** (draft) restore validated rotation cycles | ZocoLini | small | SML persistence |
| **#1136 / #1137 / #1138** unknown special types, CbTx v4, ProUp* v3 | ZocoLini | +0.1–0.4K each | Core v24 wire formats (tracking issue #1047; #1131, #1132, #1134, #1135 not started) |
| **#1140** reject oversized headers messages | ZocoLini | small | DoS hardening that matches Core limits |
| **#1141** rpc-client rejects https | ZocoLini | small | — |
| **#1142** (draft) / **#1110** adopt `dash_num::Arith256` for U256 / drop `dashcore_hashes::hex` | kwvg | ±1.5K | **First moves toward consuming more of base-sdk** (`dash-num`). #1142's text says base-sdk `dash-p2p-core` is "incubating" |
| **#400** (draft) drop SegWit and Taproot | xdustinface | −10.4K | Removes the Bitcoin leftovers (§7) |
| **#776** (draft) remove FFI crates | xdustinface | −37.6K | Policy question for the program |
| **#375** (draft) checkpoint rewrite | ZocoLini | −1.2K net | — |
| **#856, #849, #910** force-resync, `--birth-height`, re-anchor storage | various | — | SPV UX |
| **#634** single-map WalletManager | shumkov | +3.7K/−1.5K | Wallet-manager architecture |
| **#1078** lock MN collateral out of coin selection | QuantumExplorer | +1.4K | Masternode-owner wallet UX |
| **#819, #1105, #959** CoinJoin sweep/recovery, CoinJoin targeted discovery, adaptive gap probe | various | ~0.6–1K each | CoinJoin is only on the wallet-discovery side |
| **#1104, #1112, #1113, #1106, #979, #901** | various | — | Wallet correctness (truncate, spent claims, IS backfill, rescans) |

Open issues that are effectively node-gap tickets:
- #852 (no merkle-root check on blocks)
- #1116 (no difficulty validation)
- #332 (chain-work validation)
- #1103 (fork recovery vs malicious headers)
- #1118 (filter-header cross-check)
- #854 (reorg/disconnect pipeline for the wallet)
- #986 (stuck after a testnet reorg)
- #344 (bincode unmaintained)
- #398 (validate stored data on startup)
- #357 (bloom for the mempool)

### 5.2 Remote branches

- **`origin/wip-basic-daemon`** (pasta, 2025-03-27, 3 commits on merge base `4f0f9b91`; +313/−54 across 5 files). This was an **extremely thin prototype**:
  - It extended `dash/examples/handshake.rs` into a blocking `std::net::TcpStream` loop with a `HandshakeState` enum, ping, `sendheaders`/"desires", and header download into a `Vec<Header>` with a prev-hash linkage `assert!`.
  - It added `ChainLockHash` and `Inventory::ChainLock` (type 29), which `dev` now has.
  - It has no storage, no validation, no listener and no RPC. **There's nothing to salvage** beyond confirming the intent.
- **`origin/codex/clarify-consensus-scope`** (pasta, 2026-08-31, README-only). It replaces "must not be used for consensus" with an explicit list of what is not implemented: expected-difficulty/contextual header validation, UTXO transitions, all tx and special-tx rules, subsidy/MN/Platform payouts, governance and superblocks, credit-pool limits, and block templates. **This is a good, accurate summary of the gap and matches this audit.**
- **`upstream/refactor/validation`** (2025-12): a one-commit cleanup of dash-spv's `HeaderValidator`/`ValidationManager` (−124 net). Not consensus work.
- **`upstream/feat/masternode-diff-storage`** (+3K): persists masternode *messages* rather than engine snapshots.
- **`upstream/perf/reduce-ram-usage`**: storage segment cache work.
- **`upstream/integration/v4.2-core-v24`**: rpc-json for Core v24.
- **`upstream/feat/quorumValidation`, `feat/quorum-lookup-*`, `feat/validate-mn-list-root`, `fix/masternode-*`**: SML validation hardening, partly superseded by #799/#1077.

Branches containing `node`, `daemon`, `consensus` (other than the README one), `base-sdk` or `script` **do not exist upstream**. Nobody upstream is building a node.

**Velocity:** 189 commits to `dev` since 2026-07-01. Top authors are Borja Castellano (ZocoLini), Ivan Shumkov, QuantumExplorer, Kevin Rombach (xdustinface), romchornyi and kwvg. The work is overwhelmingly SPV, wallet and Platform-SDK driven.

---

## 6. base-sdk usage (the rust-dashcore side)

**Declared:** root `Cargo.toml:16-17`, `dash-pkc` and `dash-types`, git `dashpay/base-sdk` rev `e6402ced`, `default-features = false`.

| Consumer | Dependency and features | Actual use |
|---|---|---|
| `crypto/Cargo.toml` | `dash-pkc` (`std`; `bls`→`dash-pkc/bls`, `eddsa`→`dash-pkc/eddsa`, `serde`), `dash-types` (`codec`, `serde`) | See below |
| `dash/Cargo.toml:47` | `dash-types` (`codec`) | **Nothing in `dash/src` uses it.** Feature unification only, or dead |
| Every other crate | none directly | Transitive via `dashcore` re-exports |

**Import sites (all in `crypto/`):**
- `crypto/src/lib.rs:15`: `pub extern crate dash_pkc`, re-exporting the whole crate.
- `crypto/src/bls.rs:10`: `dash_pkc::__deps::ff::PrimeField`. This reaches into a **private** `__deps` path, which is fragile.
- `crypto/src/bls.rs:12-15`: `dash_pkc::bls::{BlsPublicKey, BlsSecretKey, BlsSignature, BlsScheme, BlsScChia, BlsScIetf, Fr}`, wrapped in local `BlsPublicKey`, `BlsSecretKey`, `BlsSignature`, `BlsScheme {Legacy, Modern}` and `BlsError`.
- `crypto/src/bls.rs:16`: `dash_types::{make_bytes, make_sbytes}`, which generate `BlsPkBytes`, `BlsSkBytes` and `BlsSigBytes`.
- `crypto/src/eddsa.rs:10-12`: `dash_pkc::eddsa::{EddsaError, EddsaPublicKey, EddsaSecretKey, EddsaPkBytes, EddsaSkBytes, EDDSA_PK_LEN, EDDSA_SK_LEN}` (re-exported) and `dash_types::{make_bytes, Hashable}`, which defines `EddsaPkHash`.

**Downstream re-exports:**
- `dash/src/bls_sig_utils.rs:9-11`: `BLSPublicKey = BlsPkBytes`, `BLSSignature = BlsSigBytes`.
- `dash/src/lib.rs:73,109`: `eddsa`, `PlatformNodeId = EddsaPkHash`.
- These are used by key-wallet (`derivation_bls_bip32.rs`, `derivation_slip10.rs`, `account/{bls,eddsa}_account.rs`, …) and by dash-spv (`sml_engine`, CL/IS validation).

**Resolved base-sdk crates:** `dash-pkc`, `dash-types`, `dash-num` (transitive) and `dash-types-marker`. All are `0.1.0-beta`. The BLS backend is `blst` 0.3.17, and `ed25519-dalek` 3.0.0 arrives only through `dash-pkc`. `dash-pkc`'s `ecdsa` feature is **not** enabled. The local base-sdk checkout is 51 commits ahead of the pin.

**Duplication with base-sdk** (rust-dashcore side; another agent is auditing base-sdk itself):

| rust-dashcore | LOC | base-sdk counterpart (unused) |
|---|---|---|
| `dash-network` `Network` + `dash/src/consensus/params.rs` + `blockdata/constants.rs` | ~0.9K | `dash-params` (`ChainParams`, `ConsensusParams`, `Bip9Deployment`) and `dash-p2p-core::magic` |
| `hashes/src/hash_x11.rs` (C FFI) | 0.14K + C | `dash-pow` (pure-Rust X11) |
| `dash/src/pow.rs` (`U256`, compact) | 1.8K | `dash-num::{arith256, compact}`. PR #1142 starts adopting it |
| `crypto/src/{key,ecdsa}.rs` | 1.1K | `dash-pkc::ecdsa` |
| `dash/src/consensus/encode.rs` | 2.5K | `dash-types::codec` |
| `dash/src/blockdata/transaction/special_transaction/` | 3.9K | `dash-primitives::payload::*` |
| `dash/src/blockdata/{block,transaction}` | ~4K | `dash-primitives::{block, transaction}` |
| `dash/src/blockdata/script/`, `opcodes.rs`, `address.rs` | ~5.6K | `dash-script` (with CHECKDATASIG and sigops) |
| `dash/src/network/` messages | 4.35K | `dash-p2p-core::msg` (including governance) and `dash-p2p-core::bip324` |
| `internals/`, `hashes/` newtype macros | ~1.3K | `dash-types::{hex, make_bytes}` |

---

## 7. Bitcoin leftovers that are wrong for Dash (must fix before consensus work)

1. **`BlockHash` is SHA256d by default** (`hash_types.rs:89-93`) ✔. X11 is only used under `core-block-hash-use-x11`, which only dash-spv, the bench and the seeds fetcher enable. Any crate that depends on `dashcore` without that feature gets wrong block hashes. Feature unification makes the result depend on build order.
2. **Opcode table**: Tapscript semantics instead of DIP-0020 (§2.1) ✔.
3. **`bitcoinconsensus` verification**: Bitcoin rules.
4. **`consensus::Params` / `constants.rs`**: Bitcoin values.
5. **Inventory `CompactBlock = 4`** (Dash uses 20; 4 was the legacy TXLOCK_REQUEST) ✔. The `WTx`/`Witness*` inventory types, `ServiceFlags::WITNESS`/`GETUTXO`, and `wtxidrelay`/`feefilter`/`alert` handling.
6. **Segwit:**
   - `TxIn::witness`; BIP144 encoding that can misread input-less transactions.
   - `Weight`/`predict_weight`/`policy.rs`.
   - Witness commitment.
   - Bech32 P2WPKH/P2WSH/P2TR addresses with an invented "ds" HRP.
   - `taproot.rs` (2.3K).
   - Segwit-v0 and taproot sighash.

   PR #400 removes most of this.
7. **Special-transaction payload length ignored** ✔. This is a forward-compatibility hazard: one unknown or new-version payload fails the whole block.
8. **Compact-size u32, `MAX_MSG_SIZE` 5 MB, `PROTOCOL_VERSION` 70237** ✔. These don't match Core's 32 MB/3 MiB caps or protocol 70242.

---

## 8. Gaps for a full node and masternode (entirely missing)

1. **Consensus engine:**
   - Native Dash script interpreter with DIP-0020, CHECKDATASIG and sigop accounting.
   - CheckTransaction/CheckBlock and contextual checks.
   - DGW v3 and all difficulty rules.
   - Subsidy and payment schedule: MN payments, superblocks, Platform share, credit pool.
   - Every special-transaction validation rule (ProTx state machine, collateral, MNHF/EHF, asset lock/unlock limits).
   - BIP9 and EHF deployments.
   - Block template construction.
2. **State:** UTXO set with cache and flush, block index, undo data, block files, chainstate with disconnect/connect reorg, the deterministic MN list (full `CDeterministicMNList`, not the simplified one), quorum snapshots, credit pool, governance and spork state. A real database layer.
3. **Mempool:** acceptance (`AcceptToMemoryPool` equivalent), policy, IS-locked conflict handling, eviction, fee estimation, and relay (inv trickling, `feefilter`, `getdata` serving, DSTX).
4. **P2P server side:**
   - Inbound listener, connection manager with inbound/outbound slots, eviction.
   - Address manager (addrman buckets).
   - Serving headers, blocks, filters and mempool.
   - BIP324 (in PR).
   - Compact-block relay.
   - DoS scoring that matches Core's `Misbehaving`.
   - Sporks.
   - Behaviour indistinguishable from dashd.
5. **Masternode operation:**
   - mnauth and MN/quorum connections.
   - LLMQ DKG (all phases).
   - Signing sessions and recovery (qsigshare/qsigrec/…).
   - Producing ChainLocks and InstantSend locks.
   - EHF signals and Platform ban.
   - CoinJoin *server* (dsa/dsi/dsq queueing, collateral).
   - Governance object relay, validation and voting.
   - The masternode payment and PoSe ban state machine.
6. **Indexes:** txindex, addressindex, spentindex, timestampindex, blockfilterindex (server side).
7. **RPC server and dash-cli** with Core-compatible error codes and JSON shapes, cookie auth and a config file.
8. **Wallet parity:**
   - Encryption and locking.
   - Import of wallet.dat (BDB and SQLite), dumpwallet, WIF and descriptors.
   - Imported non-HD keys.
   - Multisig/P2SH.
   - PSBT.
   - Fee estimation.
   - ProTx builders with BLS signing.
   - Governance votes.
   - CoinJoin mixing client.
   - Reorg rollback.
9. **Import of Dash Core block data** (blk*.dat/rev*.dat or `-loadblock`): missing.
10. **Conformance test assets:** Dash Core `script_tests.json`, `tx_valid/invalid`, `sighash.json` and functional-test parity are unused.

---

## 9. Top risks and surprises

1. **The `BlockHash` hash function is decided by a Cargo feature.** It defaults to SHA256d. In a node, feature unification across crates could silently change block identity. Make X11 unconditional, or move to base-sdk's pure-Rust `dash-pow`, before anything else.
2. **The "validation" that exists is mostly cryptographic, not consensus.** BLS checks on quorums, CLs and ISLocks are real. Nothing ties a mnlistdiff to the header chain, though: the coinbase proof and CbTx merkle roots go unchecked, and `from_diff` stores the wrong root. Block bodies aren't merkle-checked and headers aren't difficulty-checked, and `ValidationMode::Full` is a no-op. Several of these have open PRs (#799, #1077, #803), but none has merged.
3. **The script layer is the wrong shape for Dash.** Opcode semantics follow Bitcoin and Tapscript. The only verifier is Bitcoin's libbitcoinconsensus. No Dash Core script, tx or sighash vectors are used, and the one sighash vector test is ignored. An interpreter must be written from scratch, or taken from base-sdk `dash-script`, and fed Core's JSON vectors from day one.
4. **Forward-compatibility hazard on v24.** Payload length prefixes are ignored, so v24 (CbTx v4, ProTx v3, types 10-12, v2 asset unlocks) will desynchronize block decoding until #1047's sub-issues land. Testnet activation is "next". The test harness also pins dashd 23.1.0.
5. **Two parallel Dash type systems.** base-sdk already has primitives, a P2P codec with BIP324, params, a pure-Rust X11 and script. rust-dashcore uses only its BLS/EdDSA key types and is just starting to take `dash-num` (#1142). Choosing which stack the node is built on is the biggest architectural decision. Doing nothing means maintaining two codecs that must agree byte for byte.
6. **dash-spv is SPV-shaped throughout.** Outbound-only, wallet-filtered blocks, a custom flat-file store, a wallet-only mempool, "reorg = truncate and restart", and a client generic over a wallet. A node can reuse the message codecs, BLS/SML verification, the test harness and possibly the peer code. Its chain, storage and mempool layers will need new designs. The large in-flight network rewrite (#902) and BIP324 (#264) will collide with any node P2P work if they aren't coordinated.
7. **Upstream momentum is elsewhere.** It goes to the mobile wallet and the Platform SDK; nobody upstream is building node work. `wip-basic-daemon` is a 300-line example, not a foundation. PR #776 would delete about 37K LOC of FFI, and maintainers already push back on surface area.
8. **Wallet security gaps matter for a node wallet.** There's no encryption at rest: `backup()` writes the mnemonic and xpriv in plaintext bincode. The FFI can panic across `extern "C"`, and there's a process-global last-error slot. CoinJoin path conventions differ between Core legacy HD wallets (BIP44 chain) and descriptor wallets (DIP9 `4'`), and the default gap limit of 30 is far below Core's keypool of 1000. Both matter for importing Core wallets.
9. **Serialization debt.** The persistence format is `grovedb-bincode` (a pinned fork of the unmaintained bincode). There are three `bitcoin_hashes` versions and two `bitcoin-consensus-encoding` versions in the lockfile, and a private `dash_pkc::__deps` import. A node's on-disk formats should not inherit these.
10. **Doc and CI drift.** `CLAUDE.md` mentions `contrib/test.sh`, which doesn't exist. dash-spv's ARCHITECTURE and README are stale. The sanitizer path filters are broken. The RPC integration test is dead, and fuzzing doesn't run on PRs or cover the Dash-specific decoders beyond generic `deser_net_msg`.
